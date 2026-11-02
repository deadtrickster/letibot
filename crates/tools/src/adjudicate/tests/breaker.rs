//! Surfacing denials to the operator, and the breaker that stops the asking.

use super::*;

#[test]
fn every_denial_reaches_the_operator_with_the_grant_path_attached() {
    // A refusal taken on the operator's behalf is news while the work is happening, and it has
    // to say what they can do about it.
    let sink = std::sync::Arc::new(crate::authorise::RecordingDenialSink::default());
    struct Shared(std::sync::Arc<crate::authorise::RecordingDenialSink>);
    impl crate::authorise::DenialSink for Shared {
        fn denied(&self, n: &crate::authorise::DenialNotice) {
            self.0.denied(n);
        }
    }
    let mut g = AdjudicatedGate::closed()
        .with_surroundings(pinned())
        .with_denial_sink(Box::new(Shared(sink.clone())));
    let args = json!({"command": "/bin/rm /w/x.rs"});
    assert!(matches!(g.admit(&bash(&args)), GateDecision::Refuse { .. }));
    assert_eq!(sink.len(), 1);
    let n = sink.last().unwrap();
    assert_eq!(n.outcome, "not_run");
    assert!(n.grant.contains("grant"), "{}", n.grant);
    assert!(n.grant.contains("Nothing was executed"), "{}", n.grant);
    assert!(n.headline().starts_with("REFUSED bash"));
}

#[test]
fn three_refusals_in_one_direction_open_the_breaker_and_stop_asking() {
    // What stops a classifier being argued down one call at a time: after three, the gate does
    // not produce another verdict at all.
    let consulted = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let c = consulted.clone();
    let mut g = AdjudicatedGate::new(Box::new(AskAdjudicator::new(
        "no",
        move |req: &AdjudicationRequest| {
            c.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Some(AdjudicationDecision::selected(
                req,
                "deny",
                "human:test",
                "no",
            ))
        },
    )))
    .with_surroundings(pinned())
    .with_trail_source(|_| crate::authorise::AuthorisationTrail::from_messages(vec![], 1));

    // Three re-spellings of one intention, and the counter must not be reset by the
    // re-spelling. They have to be exec: `sed -n` reads and still asks, where `cat` would be
    // clause 4 and no breaker could ever reach it (see `Baseline::reads_only`).
    for cmd in [
        "/bin/sed -n 1p /w/secret-ish.txt",
        "/usr/bin/sed -n 2p /w/other.txt",
        "/usr/bin/sed -n 3p /w/third.txt",
    ] {
        let args = json!({"command": cmd});
        assert!(
            matches!(g.admit(&bash(&args)), GateDecision::Refuse { .. }),
            "{cmd}"
        );
    }
    assert_eq!(consulted.load(std::sync::atomic::Ordering::Relaxed), 3);

    // The fourth is not adjudicated at all, and it is exec for the same reason the three above
    // are.
    let args = json!({"command": "/usr/bin/sed -n 4p /w/fourth.txt"});
    match g.admit(&bash(&args)) {
        GateDecision::Refuse {
            outcome: ToolOutcome::NotRun { why },
            tell,
        } => {
            assert!(why.contains("circuit breaker is open"), "{why}");
            assert!(tell.contains("breaker is OPEN"), "{tell}");
            assert!(tell.contains("only the operator can lift it"), "{tell}");
        }
        other => panic!("the fourth must not reach the adjudicator: {other:?}"),
    }
    assert_eq!(
        consulted.load(std::sync::atomic::Ordering::Relaxed),
        3,
        "an open breaker consults nobody"
    );
}

#[test]
fn an_admission_clears_the_direction_so_the_breaker_is_not_a_permanent_ban() {
    let answers = std::sync::Arc::new(Mutex::new(vec!["deny", "deny", "allow_once", "deny"]));
    let a = answers.clone();
    let mut g = AdjudicatedGate::new(Box::new(AskAdjudicator::new(
        "mixed",
        move |req: &AdjudicationRequest| {
            let mut q = a.lock().unwrap();
            let pick = if q.is_empty() { "deny" } else { q.remove(0) };
            Some(AdjudicationDecision::selected(req, pick, "human:test", "…"))
        },
    )))
    .with_surroundings(pinned())
    .with_trail_source(|_| crate::authorise::AuthorisationTrail::from_messages(vec![], 1));
    // The fourth call is a different program and the same direction: an identical call would be
    // settled by the approved shape an `allow_once` records, and never reach the breaker.
    let args = json!({"command": "/bin/sed -n 1p /w/a.txt"});
    assert!(matches!(g.admit(&bash(&args)), GateDecision::Refuse { .. }));
    assert!(matches!(g.admit(&bash(&args)), GateDecision::Refuse { .. }));
    assert_eq!(g.admit(&bash(&args)), GateDecision::Admit);
    // The loop closed, so the count is gone and the next refusal starts over.
    let args = json!({"command": "/usr/bin/awk {print} /w/a.txt"});
    assert!(matches!(g.admit(&bash(&args)), GateDecision::Refuse { .. }));
    assert!(g.breaker.open_directions().is_empty());
}

#[test]
fn a_second_attempt_at_the_same_thing_is_named_as_one_rather_than_read_as_fresh() {
    let mut g = AdjudicatedGate::closed().with_surroundings(pinned());
    let args = json!({"command": "/bin/rm /w/x.rs"});
    let _ = g.admit(&bash(&args));
    match g.admit(&bash(&args)) {
        GateDecision::Refuse { tell, .. } => {
            assert!(tell.contains("attempt 2"), "{tell}");
            assert!(tell.contains("go to a human"), "{tell}");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_trail_source_is_what_puts_the_operators_words_in_front_of_an_adjudicator() {
    // Without a trail source the trail says NOT COLLECTED, which is a different fact from the
    // operator having said nothing.
    let mut bare = AdjudicatedGate::closed().with_surroundings(pinned());
    let args = json!({"command": "/bin/ls /w"});
    let req = bare.request_for(&bash(&args));
    assert!(!req.trail.was_collected());
    assert!(req.trail.render().contains("NOT COLLECTED"));

    let mut wired = AdjudicatedGate::closed()
        .with_surroundings(pinned())
        .with_trail_source(|_| {
            crate::authorise::AuthorisationTrail::from_messages(
                vec![crate::authorise::Utterance::operator("yeah restart it", 1).at_seconds(30)],
                18,
            )
        });
    let req = wired.request_for(&bash(&args));
    assert!(req.trail.was_collected());
    assert!(req.trail.render().contains("1 operator message(s) of 18"));
    assert!(req.trail.render().contains("yeah restart it"));
}

#[test]
fn escalation_is_refused_with_the_reason_and_not_treated_as_an_allow() {
    let adj = AskAdjudicator::new("test", |req: &AdjudicationRequest| {
        Some(AdjudicationDecision {
            request_id: req.id.clone(),
            outcome: DecisionOutcome::Escalate {
                to: "human".into(),
                why: "I am not sure".into(),
            },
            by: "model:small".into(),
            basis: "unsure".into(),
            latency_ms: 1,
        })
    });
    let mut g = AdjudicatedGate::new(Box::new(adj));
    let args = json!({"path": "src/lib.rs"});
    match g.admit(&call("edit", &args)) {
        GateDecision::Refuse {
            outcome: ToolOutcome::NotRun { why },
            ..
        } => {
            assert!(why.contains("escalated"), "{why}")
        }
        other => panic!("{other:?}"),
    }
}
