//! **`Gate::admit_unasked`** — the ladder without the card, which is how a look a plugin layer
//! declared meets the gate (`docs/plugins-plan.md` §1 principle 2, §4.4).
//!
//! The three things these tests hold apart are the three a caller must not confuse: a call the
//! ladder settles (it runs, and nobody was consulted), a call that would have been put to a person
//! (`NotRun`, and the reason says so), and a call a rule of the operator's refuses (`Denied`). The
//! adjudicator counts its own consultations in every one of them, because *did a card go up* is the
//! one fact the whole entry exists for — and a look runs on the session's monitor thread, where a
//! card would park the watch until somebody answered.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

/// A gate whose adjudicator counts how often it is consulted, and answers `ask` when it is.
fn counting(mode: crate::mode::Mode) -> (AdjudicatedGate, Arc<AtomicUsize>) {
    let asked = Arc::new(AtomicUsize::new(0));
    let a = asked.clone();
    let adj = AskAdjudicator::new("counting", move |_req: &AdjudicationRequest| {
        a.fetch_add(1, Ordering::Relaxed);
        None
    });
    let g = AdjudicatedGate::new(Box::new(adj))
        .with_mode(mode)
        .with_surroundings(pinned());
    (g, asked)
}

/// **A call the ladder settles runs, and the adjudicator is never reached.** A shell that only
/// reads is clause 4 — admitted at every point, including `always-ask` — and that is the answer
/// the plugin's own `gh pr view` look gets too.
#[test]
fn an_unasked_call_the_ladder_admits_consults_nobody() {
    let (mut g, asked) = counting(crate::mode::Mode::ALWAYS_ASK);
    let args = json!({"command": "git status"});
    assert_eq!(g.admit_unasked(&bash(&args)), GateDecision::Admit);
    assert_eq!(
        asked.load(Ordering::Relaxed),
        0,
        "an unasked call may not reach the adjudicator at all"
    );
    assert_eq!(g.log.len(), 1, "and the decision is still a row");
}

/// **A call that would have been put to a person is `NotRun` — not `Denied` — and the sentence
/// says which.** The distinction is the whole of what a plugin can do with the answer: `NotRun`
/// means *a person could grant this*, and `Denied` means *a rule says no*.
#[test]
fn an_unasked_call_that_would_ask_is_refused_with_the_reason() {
    let (mut g, asked) = counting(crate::mode::Mode::ALWAYS_ASK);
    let args = json!({"command": "cargo publish"});
    match g.admit_unasked(&bash(&args)) {
        GateDecision::Refuse {
            outcome: ToolOutcome::NotRun { why },
            tell,
        } => {
            assert!(
                why.starts_with("would need your approval:"),
                "the reason has to say what would have happened: {why}"
            );
            assert!(why.contains("cargo publish"), "{why}");
            assert!(
                tell.contains("adjudicated without asking"),
                "the model's half says why nothing was put to anybody: {tell}"
            );
        }
        other => panic!("a call that would ask is not run: {other:?}"),
    }
    assert_eq!(
        asked.load(Ordering::Relaxed),
        0,
        "the card the answer is about was never raised"
    );
    assert!(
        g.log.is_empty(),
        "a look that was never decided is not an audit row"
    );
}

/// **A rule of the operator's that refuses is `Denied`**, the other class — and still nobody is
/// asked.
#[test]
fn an_unasked_call_a_rule_denies_is_denied() {
    let config = crate::permission::config_to_ruleset(
        &json!({ "bash": { "*": "deny" } })
            .as_object()
            .unwrap()
            .clone(),
    )
    .unwrap();
    let (g, asked) = counting(crate::mode::Mode::ALLOW_ALL);
    let mut g = g.with_permission(config);
    let args = json!({"command": "gh pr view 24"});
    match g.admit_unasked(&bash(&args)) {
        GateDecision::Refuse {
            outcome: ToolOutcome::Denied { .. },
            tell,
        } => assert!(
            tell.contains("denied") || tell.contains("REFUSED"),
            "{tell}"
        ),
        other => panic!("a denied call does not run: {other:?}"),
    }
    assert_eq!(asked.load(Ordering::Relaxed), 0, "nobody was asked");
}

/// **An unasked refusal does not count towards the circuit breaker.** Three consecutive refusals
/// in one direction open it and *only the operator lifts that* — so a watch polling a program the
/// gate refuses would otherwise close the session's own gate, and a look would be changing what
/// the model may do. This is the test that says the look cannot.
#[test]
fn three_unasked_refusals_do_not_open_the_breaker() {
    let (mut g, asked) = counting(crate::mode::Mode::ALWAYS_ASK);
    let args = json!({"command": "cargo publish"});
    for _ in 0..crate::authorise::BREAKER_THRESHOLD + 1 {
        assert!(matches!(
            g.admit_unasked(&bash(&args)),
            GateDecision::Refuse { .. }
        ));
    }
    assert!(
        g.breaker.open_directions().is_empty(),
        "a polling watch opened the session's breaker: {:?}",
        g.breaker.open_directions()
    );
    // And the session's own call is still decided by the gate rather than short-circuited.
    assert!(matches!(g.admit(&bash(&args)), GateDecision::Refuse { .. }));
    assert_eq!(
        asked.load(Ordering::Relaxed),
        1,
        "the ordinary call reached the adjudicator exactly once"
    );
}
