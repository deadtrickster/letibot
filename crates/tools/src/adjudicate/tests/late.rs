//! A card nobody answered, answered afterwards: `/gate grant` on the row it was recorded as.
//! The call in front of the person is let through ONCE, and the ladder's own refusals stand.

use super::*;

/// An adjudicator whose card nobody answers: the state `/gate grant` exists for.
struct Nobody {
    asked: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl Adjudicator for Nobody {
    fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision {
        self.asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        AdjudicationDecision {
            request_id: req.id.clone(),
            outcome: DecisionOutcome::Timeout,
            by: "gate:timeout".into(),
            basis: "nobody answered within 300s".into(),
            latency_ms: 1,
            grant_at: None,
        }
    }

    fn describe(&self) -> String {
        "asked at the head: nobody attached".into()
    }
}

fn timed_out_gate(
    session: &'static str,
) -> (
    AdjudicatedGate,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
) {
    use std::sync::atomic::AtomicUsize;
    crate::late_grant::forget(session);
    let asked = std::sync::Arc::new(AtomicUsize::new(0));
    let gate = AdjudicatedGate::new(Box::new(Nobody {
        asked: asked.clone(),
    }))
    .with_identity(session, "agent")
    .with_surroundings(pinned());
    (gate, asked)
}

/// The work the operator was watching: a message kept in `/var/tmp` and then committed. Nobody
/// answered the card, so the call did not run - and the model's own retry of it is what the
/// grant has to let through.
fn kept_outside<'a>(args: &'a Value) -> GateCall<'a> {
    GateCall {
        name: "bash",
        access: Access::Exec,
        args,
        turn_id: "t1",
        call_id: "c1",
        workspace: "/w",
        target_exists: None,
        scripts: &[],
        preview: &[],
    }
}

#[test]
fn a_call_that_timed_out_is_let_through_once_when_the_operator_grants_it_late() {
    use std::sync::atomic::Ordering;
    let (mut g, asked) = timed_out_gate("s-late-gate-1");
    let args = json!({"command": "cat > /var/tmp/msg-tmuxsock.txt <<'EOF'\nhi\nEOF"});

    // The premise: a card, and nobody to answer it, so nothing ran.
    match g.admit(&kept_outside(&args)) {
        GateDecision::Refuse {
            outcome: ToolOutcome::NotRun { why },
            ..
        } => assert!(why.contains("timed out") || why.contains("nobody"), "{why}"),
        other => panic!("a timed-out card must refuse as not-run: {other:?}"),
    }
    assert_eq!(
        asked.load(Ordering::SeqCst),
        1,
        "the card was put to somebody"
    );

    // The operator rules on that row afterwards - what `/gate grant` records.
    let row = g.log.last().expect("the decision is a row").request.clone();
    let kept = crate::late_grant::grant(
        &row.session_id,
        &row.tool,
        &row.arguments,
        &row.id,
        crate::late_grant::now_ms(),
    );
    assert_eq!(kept.key, crate::late_grant::key("bash", &args));

    // The same call, now: it runs, and nobody is asked a second time.
    assert_eq!(g.admit(&kept_outside(&args)), GateDecision::Admit);
    assert_eq!(
        asked.load(Ordering::SeqCst),
        1,
        "a person answered it already"
    );

    // Once. The next identical call is a new card, because the grant was spent by that one.
    assert!(matches!(
        g.admit(&kept_outside(&args)),
        GateDecision::Refuse { .. }
    ));
    assert_eq!(asked.load(Ordering::SeqCst), 2);

    // And the row a person allowed late says so, rather than reading as a mode's admission.
    let admitted = g.log.iter().find(|r| r.effect == "admit").expect("a row");
    assert_eq!(admitted.decision.by, "human:operator");
    assert!(admitted.asked, "a person decided this call");
}

#[test]
fn a_late_grant_is_one_call_and_never_a_standing_answer() {
    use std::sync::atomic::Ordering;
    let (mut g, asked) = timed_out_gate("s-late-gate-2");
    let granted = json!({"command": "sh scripts/one.sh"});
    let other = json!({"command": "sh scripts/two.sh"});
    assert!(matches!(
        g.admit(&kept_outside(&granted)),
        GateDecision::Refuse { .. }
    ));
    let row = g.log.last().expect("a row").request.clone();
    crate::late_grant::grant(
        &row.session_id,
        &row.tool,
        &row.arguments,
        &row.id,
        crate::late_grant::now_ms(),
    );

    // Another command in the same session is another question, and it is still asked.
    assert!(matches!(
        g.admit(&kept_outside(&other)),
        GateDecision::Refuse { .. }
    ));
    assert_eq!(asked.load(Ordering::SeqCst), 2);
    // And the grant it did not use is still there for the call it named.
    assert_eq!(g.admit(&kept_outside(&granted)), GateDecision::Admit);
}

/// A blocked tier is the one thing no answer promotes, and a grant recorded against a row of
/// some other call must not become one: it is keyed, not a flag on the session.
#[test]
fn a_late_grant_cannot_reach_a_call_the_rules_refuse() {
    let (mut g, asked) = timed_out_gate("s-late-gate-3");
    // A WRITE into the repository's own git directory: the call that can only be judged by its
    // spelling, which the never-write list refuses before the person half of the ladder.
    let secret = json!({"command": "cp notes.txt /w/.git/objects/pack/x.pack"});
    let granted = json!({"command": "cat > /w/notes.txt <<'EOF'\nhi\nEOF"});
    crate::late_grant::grant(
        "s-late-gate-3",
        "bash",
        &granted,
        "adj-somewhere-else",
        crate::late_grant::now_ms(),
    );
    // The never-write list refuses this call before the person half of the ladder is reached.
    match g.admit(&kept_outside(&secret)) {
        GateDecision::Refuse {
            outcome: ToolOutcome::Denied { .. },
            ..
        } => {}
        other => panic!("`.git` is refused by the boundary, not granted late: {other:?}"),
    }
    assert_eq!(
        asked.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "nobody was asked"
    );
}
