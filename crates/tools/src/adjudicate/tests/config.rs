//! The config card is answered by the OPERATOR and by nobody else. Every other answerer this
//! tree has — a mode's own admission, a guard model at an automode point, a bridge seat, a
//! session's own answer — reaches the same door with a name, and the name is the whole rule.

use super::*;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::builtins::config::{ConfigAnswer, ConfigAsk};
use crate::runtime::Gate;

/// An adjudicator that answers with one option and one name, counting what it was shown.
struct Named {
    id: &'static str,
    option: &'static str,
    seen: Arc<AtomicUsize>,
}

impl Adjudicator for Named {
    fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision {
        self.seen.fetch_add(1, Ordering::SeqCst);
        AdjudicationDecision::selected(req, self.option, self.id, "says so")
    }

    fn describe(&self) -> String {
        format!("answered by {}", self.id)
    }
}

fn ask() -> ConfigAsk {
    ConfigAsk::Propose {
        file: "providers.toml".into(),
        diff: "--- a/providers.toml\n+++ b/providers.toml\n@@ -1,2 +1,2 @@\n-x\n+y\n".into(),
        why: "the guard answers too much".into(),
        preview: Vec::new(),
    }
}

fn gate(id: &'static str, option: &'static str) -> (AdjudicatedGate, Arc<AtomicUsize>) {
    let seen = Arc::new(AtomicUsize::new(0));
    let g = AdjudicatedGate::new(Box::new(Named {
        id,
        option,
        seen: seen.clone(),
    }));
    (g, seen)
}

/// The operator's own `allow once` is the one yes. It carries their name, so a result can say
/// whose word it was.
#[test]
fn the_operators_allow_once_is_the_only_yes() {
    let (mut g, seen) = gate("human:dead", "allow_once");
    assert_eq!(
        g.ask_config(&ask()),
        ConfigAnswer::Granted {
            by: "human:dead".into()
        }
    );
    assert_eq!(seen.load(Ordering::SeqCst), 1);
}

/// The operator saying no is a refusal with their words, not a yes and not an error.
#[test]
fn the_operators_no_is_a_refusal() {
    let (mut g, _) = gate("human:dead", "deny_and_tell");
    match g.ask_config(&ask()) {
        ConfigAnswer::Refused { by, .. } => assert_eq!(by, "human:dead"),
        other => panic!("{other:?}"),
    }
}

/// Every other answerer: refused by name, with the reason in the words the model reads.
#[test]
fn an_answer_that_is_not_the_operators_is_refused_by_name() {
    for who in [
        // A guard model at an automode point, and the shadow judge beside the operator.
        "model:gatekeeper",
        "shadow:qwen",
        // A bridge seat answering a package's card, and a session or parent agent answering
        // its own.
        "agent:cc@letibot-a4f",
        "agent:s-1",
        // The gate's own arms, which never reach this door but must not be taken for a person
        // by a future one.
        "gate:mode",
        "gate:boundary",
        "gate:session-grant",
        "gatekeeper:local",
    ] {
        let (mut g, _) = gate(who, "allow_once");
        match g.ask_config(&ask()) {
            ConfigAnswer::Refused { by, why } => {
                assert_eq!(by, who);
                assert!(
                    why.contains("is not the operator"),
                    "the refusal says what was wrong: {why}"
                );
                assert!(
                    why.contains("not a mode") && why.contains("not a bridge seat"),
                    "and what is left out: {why}"
                );
            }
            other => panic!("{who} answered and was taken for the operator: {other:?}"),
        }
    }
}

/// The card offers no standing rung at all: a rule or a session grant would be the permission
/// to edit letibot's own configuration, taken once and spent for ever.
#[test]
fn the_ladder_has_no_standing_answer() {
    let seen: Arc<std::sync::Mutex<Option<AdjudicationRequest>>> = Arc::default();
    struct Keep(Arc<std::sync::Mutex<Option<AdjudicationRequest>>>);
    impl Adjudicator for Keep {
        fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision {
            *self.0.lock().unwrap() = Some(req.clone());
            AdjudicationDecision::selected(req, "deny", "human:dead", "no")
        }
        fn describe(&self) -> String {
            "human".into()
        }
    }
    let mut g = AdjudicatedGate::new(Box::new(Keep(seen.clone())));
    let _ = g.ask_config(&ask());
    let req = seen.lock().unwrap().clone().expect("the card was put");
    assert!(matches!(req.tier, Tier::AlwaysAsk { rule, .. } if rule == "config_change"));
    assert!(
        req.options.iter().all(|o| matches!(
            o.kind,
            OptionKind::AllowOnce | OptionKind::Deny | OptionKind::DenyAndTell
        )),
        "the ladder is {:?}",
        req.options
    );
    assert!(req.options_note.is_some(), "the card says why a rung is missing");
    assert!(req.on_timeout == OnTimeout::Deny);
    assert_eq!(req.preview.len(), 0, "show's card draws no diff");
    // A proposal's card carries the diff itself, so whoever answers reads the change rather
    // than a description of it.
    match &req.arguments {
        Value::Object(o) => {
            let diff = o.get("diff").and_then(Value::as_str).unwrap_or_default();
            assert!(diff.contains("+++ b/providers.toml"), "{diff}");
            assert_eq!(o.get("why").and_then(Value::as_str), Some("the guard answers too much"));
        }
        other => panic!("{other}"),
    }
}

/// No adjudicator, no question: `NotAsked` rather than a refusal, because nobody decided.
#[test]
fn with_nobody_to_ask_nothing_is_applied() {
    let mut g = AdjudicatedGate::new(Box::new(crate::adjudicate::NoAdjudicator));
    match g.ask_config(&ask()) {
        ConfigAnswer::NotAsked(why) => assert!(!why.is_empty()),
        other => panic!("{other:?}"),
    }
}

/// The door is on `Gate` so a runtime with no gate behind it answers `NotAsked` rather than
/// admitting what nothing asked about.
#[test]
fn a_gate_that_does_not_carry_the_door_answers_not_asked() {
    struct Plain;
    impl Gate for Plain {
        fn admit(&mut self, _call: &GateCall<'_>) -> GateDecision {
            GateDecision::Admit
        }
    }
    let mut plain = Plain;
    assert!(matches!(
        plain.ask_config(&ask()),
        ConfigAnswer::NotAsked(_)
    ));
    // And the decision is a row like any other: `show`'s and `propose`'s both.
    let (mut g, _) = gate("human:dead", "allow_once");
    let _ = g.ask_config(&ConfigAsk::Show {
        file: "leticode.toml".into(),
    });
    assert_eq!(g.log.len(), 1, "a config answer is filed");
}
