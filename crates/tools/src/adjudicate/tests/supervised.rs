//! Supervised points: the person and the model both consulted, and the corpus row.

use super::*;

fn supervised_gate(model_allows: bool, human_allows: bool) -> AdjudicatedGate {
    let advisor: std::sync::Arc<dyn Adjudicator> =
        std::sync::Arc::new(Fixed::new("model:test", model_allows));
    AdjudicatedGate::new(Box::new(Fixed::new("human:op", human_allows)))
        .with_mode(crate::mode::Mode::ALWAYS_ASK)
        .with_advisor(advisor)
        .start_supervised(true)
}

/// Without this the point's name is a lie: "ask me if I agree" requires something in front of me
/// to agree with.
#[test]
fn the_human_is_shown_what_the_model_said() {
    let seen: std::sync::Arc<std::sync::Mutex<Option<Option<ModelAdvice>>>> =
        std::sync::Arc::new(std::sync::Mutex::new(None));
    let captured = seen.clone();
    let human = AskAdjudicator::new("human:op", move |req: &AdjudicationRequest| {
        *captured.lock().unwrap() = Some(req.advice.clone());
        req.options
            .first()
            .map(|o| AdjudicationDecision::selected(req, &o.id, "human:op", "ok"))
    });
    let advisor: std::sync::Arc<dyn Adjudicator> =
        std::sync::Arc::new(Fixed::new("model:test", true));
    let mut g = AdjudicatedGate::new(Box::new(human))
        .with_mode(crate::mode::Mode::ALWAYS_ASK)
        .with_advisor(advisor)
        .start_supervised(true);
    let _ = g.admit(&call("edit", &json!({"path": "src/lib.rs"})));

    let advice = seen.lock().unwrap().clone().expect("the human was asked");
    let advice = advice.expect("carrying the model's verdict");
    assert_eq!(advice.would, "admit");
    assert_eq!(advice.by, "model:test");
}

/// The model advises and never decides: the human's answer is the outcome, whichever way the model
/// went.
#[test]
fn the_person_overrules_the_model_in_both_directions() {
    let mut g = supervised_gate(true, false);
    assert!(matches!(
        g.admit(&call("edit", &json!({"path": "src/lib.rs"}))),
        GateDecision::Refuse { .. }
    ));
    assert_eq!(g.log[0].effect, "refuse");

    let mut g = supervised_gate(false, true);
    assert!(matches!(
        g.admit(&call("edit", &json!({"path": "src/lib.rs"}))),
        GateDecision::Admit
    ));
    assert_eq!(g.log[0].effect, "admit");
}

/// The model's verdict and the operator's ruling are both on the row and are different values, so
/// nothing has to be reconstructed afterwards from a list.
#[test]
fn a_supervised_call_produces_a_labelled_corpus_row_by_itself() {
    let mut g = supervised_gate(true, false);
    let _ = g.admit(&call("edit", &json!({"path": "src/lib.rs"})));
    let row = g.corpus().remove(0);

    assert!(row.asked, "a person answered this one");
    // The model's, not the person's wearing its name.
    let verdict = row.model_verdict.as_deref().expect("the model was asked");
    assert!(verdict.starts_with("admit by model:test"), "{verdict}");
    // The person's, separately.
    assert_eq!(row.verdict_by.as_deref(), Some("human:op"));
    assert_eq!(row.effect, "refuse");
    // And the label, computed from the two rather than typed later.
    assert_eq!(
        row.operator
            .as_ref()
            .map(crate::authorise::OperatorOverride::as_str),
        Some("revoked"),
        "the model would have admitted and the person stopped it"
    );
    assert!(row.is_disagreement());

    // The other direction is the over-refusal, and it is `granted`.
    let mut g = supervised_gate(false, true);
    let _ = g.admit(&call("edit", &json!({"path": "src/lib.rs"})));
    let row = g.corpus().remove(0);
    assert_eq!(
        row.operator
            .as_ref()
            .map(crate::authorise::OperatorOverride::as_str),
        Some("granted")
    );

    // Agreement is a label too: a corpus of corrections alone teaches that every decision was
    // wrong.
    let mut g = supervised_gate(true, true);
    let _ = g.admit(&call("edit", &json!({"path": "src/lib.rs"})));
    let row = g.corpus().remove(0);
    assert_eq!(
        row.operator
            .as_ref()
            .map(crate::authorise::OperatorOverride::as_str),
        Some("upheld")
    );
    assert!(!row.is_disagreement());
}

/// `shown` and `reply` come from the advisor and not from the decider: at a supervised point the
/// decider is the person, whose brief is the card, so the oracle's brief and the bytes its verdict
/// was read out of would both be lost.
#[test]
fn the_corpus_keeps_the_oracles_brief_and_reply_not_the_persons_card() {
    let mut g = supervised_gate(true, false);
    let _ = g.admit(&call("edit", &json!({"path": "src/lib.rs"})));
    assert!(g.corpus()[0].asked, "a person answered this one");
    let row = g.corpus().remove(0);

    let shown = row
        .shown
        .as_deref()
        .expect("an oracle was consulted, so the brief is on the row");
    assert!(
        shown.contains("model:test"),
        "not the oracle's brief: {shown}"
    );
    assert!(
        !shown.starts_with("decision —"),
        "the person's card is on a row an oracle decided: {shown}"
    );
    let reply = row
        .reply
        .as_deref()
        .expect("the oracle answered, so the reply is on the row");
    assert!(reply.contains("ALLOW 0"), "{reply}");

    // Nothing is filed where nobody spoke: with no advisor both columns stay empty rather than
    // carrying the card or a previous call's reply.
    let mut g = AdjudicatedGate::new(Box::new(Fixed::new("human:op", true)))
        .with_mode(crate::mode::Mode::ALWAYS_ASK);
    let _ = g.admit(&call("edit", &json!({"path": "src/lib.rs"})));
    let row = g.corpus().remove(0);
    assert!(row.asked, "the person answered");
    assert_eq!(
        row.shown, None,
        "a person's answer is not an oracle's brief"
    );
    assert_eq!(row.reply, None, "and it is not a reply either");

    // The model's own verdict admits, and that path returns before the gate reaches its decider,
    // so the brief and the reply that admitted the call have to be filed by the path itself.
    let mut g = supervised_gate(true, true).with_mode(crate::mode::Mode::AUTO);
    let _ = g.admit(&call("edit", &json!({"path": "src/lib.rs"})));
    let row = g.corpus().remove(0);
    assert!(
        row.verdict_by.as_deref().unwrap_or("").contains("model"),
        "the model decided this one: {row:?}"
    );
    assert!(
        row.shown
            .as_deref()
            .is_some_and(|s| s.contains("model:test")),
        "the model decided and its brief was not filed: {:?}",
        row.shown
    );
    assert!(
        row.reply.as_deref().is_some_and(|r| r.contains("ALLOW 0")),
        "nor its reply: {:?}",
        row.reply
    );
}

/// The adviser is not load-bearing: an oracle that cannot answer leaves the person answering
/// exactly as they would at `always-ask`.
#[test]
fn a_silent_oracle_does_not_stop_the_person_being_asked() {
    struct Mute;
    impl Adjudicator for Mute {
        fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision {
            AdjudicationDecision {
                request_id: req.id.clone(),
                outcome: DecisionOutcome::Timeout,
                by: "model:test".into(),
                basis: "budget".into(),
                latency_ms: 400,
                grant_at: None,
            }
        }
        fn describe(&self) -> String {
            "model:test".into()
        }
    }
    let mut g = AdjudicatedGate::new(Box::new(Fixed::new("human:op", true)))
        .with_mode(crate::mode::Mode::ALWAYS_ASK)
        .with_advisor(std::sync::Arc::new(Mute))
        .start_supervised(true);
    assert!(matches!(
        g.admit(&call("edit", &json!({"path": "src/lib.rs"}))),
        GateDecision::Admit
    ));
    let row = g.corpus().remove(0);
    // No advice, so no label is invented: there is nothing for the person to
    // have agreed or disagreed with.
    assert!(row.operator.is_none());
    assert!(row.asked);
}

/// A grant stops asking the person and does not stop asking the model. Three claims in one test
/// because they are one mechanism: the person is asked once, the oracle on every covered call, and
/// every covered call is a labelled row.
#[test]
fn a_standing_grant_settles_the_person_and_still_asks_the_model() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Counts how often the model was consulted, and how often a person was.
    struct Counted {
        id: &'static str,
        allow: bool,
        n: std::sync::Arc<AtomicUsize>,
    }
    impl Adjudicator for Counted {
        fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision {
            self.n.fetch_add(1, Ordering::SeqCst);
            // The person takes the SESSION grant: an `allow_once` would settle one call and prove
            // nothing about what a remembered answer does.
            let want = |o: &&DecisionOption| {
                if self.allow {
                    o.kind == OptionKind::AllowSession
                } else {
                    !o.kind.admits()
                }
            };
            match req.options.iter().find(want) {
                Some(o) => AdjudicationDecision::selected(req, &o.id, self.id, "yes"),
                None => AdjudicationDecision::unavailable(req, self.id, "none offered"),
            }
        }
        fn last_advice(&self) -> Option<ModelAdvice> {
            self.id.starts_with("model").then(|| ModelAdvice {
                consulted: true,
                would: if self.allow { "admit" } else { "ask" },
                by: self.id.to_string(),
                basis: "yes".into(),
                cites: Vec::new(),
                latency_ms: 1,
                unsure: None,
            })
        }
        fn describe(&self) -> String {
            self.id.into()
        }
    }

    let asks_model = std::sync::Arc::new(AtomicUsize::new(0));
    let asks_human = std::sync::Arc::new(AtomicUsize::new(0));
    let advisor: std::sync::Arc<dyn Adjudicator> = std::sync::Arc::new(Counted {
        id: "model:test",
        allow: true,
        n: asks_model.clone(),
    });
    // A point that asks about writes and lets one answer stand for the session, which is the
    // shape a grant is for. Written out, because this test is about the grant and not about which
    // point happens to have that scope today.
    let asks_and_grants = crate::mode::Mode {
        name: "test:asks-and-grants",
        grants: crate::mode::GrantScope::Session,
        ..crate::mode::Mode::ALWAYS_ASK
    };
    let mut g = AdjudicatedGate::new(Box::new(Counted {
        id: "human:op",
        allow: true,
        n: asks_human.clone(),
    }))
    .with_mode(asks_and_grants)
    .with_advisor(advisor)
    .start_supervised(true);

    let args = json!({"path": "src/lib.rs"});
    for _ in 0..3 {
        assert!(matches!(g.admit(&call("edit", &args)), GateDecision::Admit));
    }

    assert_eq!(g.grants().len(), 1, "the first answer took a session grant");
    assert_eq!(
        asks_human.load(Ordering::SeqCst),
        1,
        "the person is asked once and the grant covers the rest"
    );
    assert_eq!(
        asks_model.load(Ordering::SeqCst),
        3,
        "the oracle is asked on EVERY call, grant or no grant — that is the label"
    );

    // Every one is a row, and every one is labelled.
    let rows = g.corpus();
    assert_eq!(rows.len(), 3);
    for r in &rows {
        assert!(r.model_verdict.is_some(), "the model answered this one");
        assert!(r.operator.is_some(), "and the operator's ruling is on it");
    }

    // The first is a judgement made with the verdict on screen, the other two a standing ruling
    // applied: `asked` separates them and the note says which grant, so a trainer can weight them
    // differently.
    assert!(rows[0].asked);
    assert!(!rows[1].asked && !rows[2].asked);
    let note = match rows[1].operator.as_ref().unwrap() {
        crate::authorise::OperatorOverride::Upheld { note }
        | crate::authorise::OperatorOverride::Granted { note }
        | crate::authorise::OperatorOverride::Revoked { note } => note.clone(),
    };
    assert!(note.contains("standing decision"), "{note}");
    assert!(note.contains("granted for this session"), "{note}");
}

/// A command asks every time whatever is granted, and supervision has no relaxed version of that
/// rule.
#[test]
fn an_exec_call_is_not_covered_by_a_grant_at_a_supervised_point() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let asks = std::sync::Arc::new(AtomicUsize::new(0));
    let seen = asks.clone();
    let human = AskAdjudicator::new("human:op", move |req: &AdjudicationRequest| {
        seen.fetch_add(1, Ordering::SeqCst);
        req.options
            .iter()
            .find(|o| o.kind.admits())
            .map(|o| AdjudicationDecision::selected(req, &o.id, "human:op", "ok"))
    });
    let advisor: std::sync::Arc<dyn Adjudicator> =
        std::sync::Arc::new(Fixed::new("model:test", true));
    let mut g = AdjudicatedGate::new(Box::new(human))
        .with_mode(crate::mode::Mode::ALWAYS_ASK)
        .with_advisor(advisor)
        .start_supervised(true)
        .with_exec_follows_mode(false)
        // An absolute path and a pinned shell, so layer A resolves the action and it reaches an
        // adjudicator at all: a bare `cargo` under `ShellTrust::Unknown` is `not_run`, and this
        // test would pass for the wrong reason.
        .with_surroundings(pinned())
        .with_trail_source(|_| crate::authorise::AuthorisationTrail::from_messages(vec![], 1));

    // Exec, and two different shapes of it. `sed -n` reads and still asks, where `cat` would be
    // clause 4; the shapes differ because the cache settles an identical second call at a
    // `MayApprove` tier.
    for cmd in [
        "/bin/sed -n 1p /w/src/lib.rs",
        "/usr/bin/awk {print} /w/src/lib.rs",
    ] {
        assert_eq!(
            g.admit(&bash(&json!({"command": cmd}))),
            GateDecision::Admit
        );
    }
    assert_eq!(
        asks.load(Ordering::SeqCst),
        2,
        "exec asks every time; a standing permission never covers it"
    );
    assert!(
        g.grants().is_empty(),
        "and no grant is taken over an exec call"
    );
}

/// A rule the operator wrote is the answer, and only a REFUSAL is measured against the model: the
/// calibration of a No (a rule refusing what the model would admit) is the signal, and an admitted
/// call is not a question anybody asked.
#[test]
fn a_permission_rule_admits_unmeasured_and_a_deny_is_still_measured() {
    let rules = |json: serde_json::Value| {
        crate::permission::config_to_ruleset(json.as_object().unwrap()).unwrap()
    };
    let gate = |cfg: serde_json::Value, model_allows: bool| {
        let advisor: std::sync::Arc<dyn Adjudicator> =
            std::sync::Arc::new(Fixed::new("model:test", model_allows));
        AdjudicatedGate::new(Box::new(NoAdjudicator))
            .with_mode(crate::mode::Mode::ALWAYS_ASK)
            .with_advisor(advisor)
            .start_supervised(true)
            .with_permission(rules(cfg))
    };

    // An `allow` rule admits, and nothing judged it: the row's verdict is the rule's own.
    let mut g = gate(json!({ "edit": "allow" }), true);
    assert_eq!(
        g.admit(&call("edit", &json!({"path": "a.rs"}))),
        GateDecision::Admit
    );
    let row = g.corpus().remove(0);
    assert!(
        row.model_verdict
            .as_deref()
            .is_some_and(|v| v.contains("gate:permission")),
        "the rule decided: {:?}",
        row.model_verdict
    );
    assert!(!row.asked, "nobody was asked: a rule decided");
    assert!(
        row.operator.is_none(),
        "no model spoke, so there is nothing to uphold or revoke"
    );

    // A `deny` rule refuses and an oracle that would have admitted is recorded disagreeing: the
    // over-refusal signal exists only because the deny path asks too.
    let mut g = gate(json!({ "edit": "deny" }), true);
    assert!(matches!(
        g.admit(&call("edit", &json!({"path": "a.rs"}))),
        GateDecision::Refuse { .. }
    ));
    let row = g.corpus().remove(0);
    assert_eq!(
        row.operator
            .as_ref()
            .map(crate::authorise::OperatorOverride::as_str),
        Some("revoked"),
        "the model would have admitted and the operator's rule refused"
    );
    assert!(row.is_disagreement());

    // The advice changed no outcome: the same rule with a model that would refuse still admits.
    let mut g = gate(json!({ "edit": "allow" }), false);
    assert_eq!(
        g.admit(&call("edit", &json!({"path": "a.rs"}))),
        GateDecision::Admit
    );
}

/// A settled call at a point that is not supervised spends no oracle round trip: paying latency
/// for a row nobody asked for is what this guards.
#[test]
fn a_rule_at_a_non_supervised_point_consults_nothing() {
    let cfg = json!({ "edit": "allow" });
    let rules = crate::permission::config_to_ruleset(cfg.as_object().unwrap()).unwrap();
    let mut g = AdjudicatedGate::new(Box::new(Fixed::new("human:op", true)))
        .with_mode(crate::mode::Mode::WRITES_ALLOWED)
        .with_permission(rules);
    assert_eq!(
        g.admit(&call("edit", &json!({"path": "a.rs"}))),
        GateDecision::Admit
    );
    let row = g.corpus().remove(0);
    // The verdict column holds what decided, the rule, and no label is invented.
    assert!(
        row.model_verdict
            .as_deref()
            .unwrap()
            .contains("gate:permission")
    );
    assert!(row.operator.is_none());
}

/// A rule at a settled point is the answer, and no model is asked about it: the operator wrote the
/// rule, the card is not drawn, and a round trip spent on an undisputed call is cost nobody chose.
/// A refusal still gets its model label - the calibration of a No is what that row is for.
#[test]
fn an_admitted_call_consults_no_model_and_a_refusal_still_does() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Counts every consultation, whatever the answer.
    struct Counting(std::sync::Arc<AtomicUsize>);
    impl Adjudicator for Counting {
        fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision {
            self.0.fetch_add(1, Ordering::SeqCst);
            AdjudicationDecision::unavailable(req, "model:test", "counted")
        }
        fn last_advice(&self) -> Option<ModelAdvice> {
            Some(ModelAdvice {
                consulted: true,
                would: "ask",
                by: "model:test".into(),
                basis: "counted".into(),
                cites: Vec::new(),
                latency_ms: 1,
                unsure: None,
            })
        }
        fn describe(&self) -> String {
            "model:test".into()
        }
    }

    let build = |f: &dyn Fn(AdjudicatedGate) -> AdjudicatedGate| {
        let n = std::sync::Arc::new(AtomicUsize::new(0));
        let g = AdjudicatedGate::new(Box::new(Fixed::new("human:op", true)))
            .with_mode(crate::mode::Mode::ALWAYS_ASK)
            .with_advisor(std::sync::Arc::new(Counting(n.clone())))
            .start_supervised(true);
        (f(g), n)
    };
    let rules = |j: serde_json::Value| {
        crate::permission::config_to_ruleset(j.as_object().unwrap()).unwrap()
    };

    // The two calls the cases below trip their path with.
    let path = json!({"path": "src/lib.rs"});
    let secret = json!({"path": "/home/op/.ssh/id_ed25519"});

    // 2. the never-write list: a REFUSAL, and its label is worth the round trip.
    let (mut g, n) = build(&|g| g);
    let _ = g.admit(&call("edit", &secret));
    assert_eq!(n.load(Ordering::SeqCst), 1, "never-write list: model asked");
    assert_eq!(g.log[0].effect, "refuse", "and the list still refuses");

    // 4. a config `deny` asks (the label of a No), and a config `allow` does not: the rule is the
    //    answer, and nothing judged it because nothing disputed it.
    let (mut g, n) = build(&|g| g.with_permission(rules(json!({ "edit": "deny" }))));
    let _ = g.admit(&call("edit", &path));
    assert_eq!(n.load(Ordering::SeqCst), 1, "a config deny is labelled");
    assert_eq!(g.log[0].effect, "refuse");

    let (mut g, n) = build(&|g| g.with_permission(rules(json!({ "edit": "allow" }))));
    let _ = g.admit(&call("edit", &path));
    assert_eq!(
        n.load(Ordering::SeqCst),
        0,
        "a config allow was judged anyway"
    );
    assert_eq!(g.log[0].effect, "admit", "and the rule still decides");
    assert!(
        g.log[0].advice.is_none(),
        "an admitted call carries no model verdict"
    );

    // 6. the mode admits unasked, at a point that does: `writes-allowed` admits a write, and no
    //    model is consulted for it.
    let n = std::sync::Arc::new(AtomicUsize::new(0));
    let mut g = AdjudicatedGate::new(Box::new(Fixed::new("human:op", true)))
        .with_mode(crate::mode::Mode::WRITES_ALLOWED)
        .with_advisor(std::sync::Arc::new(Counting(n.clone())))
        .start_supervised(true);
    let _ = g.admit(&call("edit", &path));
    assert_eq!(n.load(Ordering::SeqCst), 0, "the mode's yes was judged");
    assert_eq!(g.log[0].effect, "admit");

    // 0. the breaker is the exception on purpose: the calls reaching it are near-duplicates of
    //    rows already recorded, and an oracle round trip per iteration of a retry loop is what
    //    the breaker exists to prevent. Only an ANSWER counts toward it - a rule's refusal is the
    //    same refusal every time, so it opens nothing (see `authorise::tests`).
    let n = std::sync::Arc::new(AtomicUsize::new(0));
    let mut g = AdjudicatedGate::new(Box::new(Fixed::new("human:op", false)))
        .with_mode(crate::mode::Mode::ALWAYS_ASK)
        .with_advisor(std::sync::Arc::new(Counting(n.clone())))
        .start_supervised(true);
    for _ in 0..5 {
        let _ = g.admit(&call("edit", &path));
    }
    let asked = n.load(Ordering::SeqCst);
    assert!(
        asked < 5,
        "the breaker must stop consulting, asked {asked} of 5"
    );
    assert!(asked >= 3, "and only after it has opened, asked {asked}");
}

/// `ModelAdjudicator` answers several questions without asking a model (an unresolved action, an
/// always-ask entry, an uncollected trail), and from outside those produce the same decision a real
/// verdict does. Labelling against them would manufacture signal, which looks like data.
#[test]
fn an_unconsulted_oracle_produces_no_label() {
    struct Silent;
    impl Adjudicator for Silent {
        fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision {
            AdjudicationDecision::unavailable(req, "model:test", "always-ask short-circuit")
        }
        fn last_advice(&self) -> Option<ModelAdvice> {
            Some(ModelAdvice {
                consulted: false,
                would: "ask",
                by: "model:test".into(),
                basis: "`sudo` is on the always-ask list; no oracle was consulted".into(),
                cites: Vec::new(),
                latency_ms: 0,
                unsure: None,
            })
        }
        fn describe(&self) -> String {
            "model:test".into()
        }
    }
    let mut g = AdjudicatedGate::new(Box::new(Fixed::new("human:op", true)))
        .with_mode(crate::mode::Mode::ALWAYS_ASK)
        .with_advisor(std::sync::Arc::new(Silent))
        .start_supervised(true);
    let _ = g.admit(&call("edit", &json!({"path": "src/lib.rs"})));

    let row = g.corpus().remove(0);
    assert!(
        row.operator.is_none(),
        "no oracle spoke, so there is nothing the operator agreed or disagreed with"
    );
    // Silence is recorded, not dropped: the row still says what happened.
    assert!(row.model_verdict.is_some());
    assert!(row.asked);
    // And a head renders it as an absence rather than as an opinion.
    let advice = g.log[0].advice.as_ref().unwrap();
    assert!(
        advice.line().starts_with("no model verdict"),
        "{}",
        advice.line()
    );
}

/// Supervision is a flag on the gate rather than a mode, so it can change mid-session; this
/// asserts the property that makes that safe: the same calls ask, the same answers travel, and
/// only the verdict column appears.
#[test]
fn supervision_changes_who_is_consulted_and_nothing_else() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let asks = std::sync::Arc::new(AtomicUsize::new(0));
    let seen = asks.clone();
    let human = AskAdjudicator::new("human:op", move |req: &AdjudicationRequest| {
        seen.fetch_add(1, Ordering::SeqCst);
        req.options
            .iter()
            .find(|o| o.kind.admits())
            .map(|o| AdjudicationDecision::selected(req, &o.id, "human:op", "ok"))
    });
    let mut g = AdjudicatedGate::new(Box::new(human))
        .with_mode(crate::mode::Mode::ALWAYS_ASK)
        .with_advisor(std::sync::Arc::new(Fixed::new("model:test", true)));

    let args = json!({"path": "src/lib.rs"});

    // Off: the person is asked, and the row carries no verdict.
    assert!(!g.supervising());
    assert_eq!(g.admit(&call("edit", &args)), GateDecision::Admit);
    assert!(g.corpus()[0].operator.is_none());

    // On, mid-session, without rebuilding anything.
    let said = g.set_supervision(true).expect("an advisor is installed");
    assert!(said.contains("supervision ON"), "{said}");
    assert!(g.supervising());

    assert_eq!(g.admit(&call("edit", &args)), GateDecision::Admit);
    assert_eq!(
        asks.load(Ordering::SeqCst),
        2,
        "the same calls ask the same person; supervision adds a verdict, not a question"
    );
    let row = g.corpus().remove(1);
    assert!(
        row.model_verdict
            .as_deref()
            .unwrap()
            .starts_with("admit by model:test")
    );
    assert_eq!(row.verdict_by.as_deref(), Some("human:op"));
    assert_eq!(
        row.operator
            .as_ref()
            .map(crate::authorise::OperatorOverride::as_str),
        Some("upheld")
    );

    // And off again.
    assert!(g.set_supervision(false).is_ok());
    assert!(!g.supervising());
}
