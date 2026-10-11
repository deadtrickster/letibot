//! R11 — the audit rows, read back at decision time.

use super::*;

/// §4h's loop, closed: a second call of the same shape is shown the first
/// answer, with its count and its age. The prior rows are keyed on the task
/// direction — `(tool, intents, scope, regions)` — so a re-spelled command in
/// the same direction matches and a different direction does not.
#[test]
fn a_second_call_of_the_same_shape_shows_the_first_answer_with_its_count_and_age() {
    // **A PERSON's answer, and only a person's.** This used to run against a
    // closed gate, whose fail-closed refusal is nobody's judgement, and assert
    // that it came back as history. It does not any more, and the reason is the
    // operator's: *"self recursion can do drift — 'that command was approved
    // already by someone and this is ever so slightly different'. we dont
    // include its own decisions to the brief."* A gate shown its own answers
    // ratchets; only what a human decided is evidence about what anyone wanted.
    let mut human = AdjudicatedGate::new(Box::new(AskAdjudicator::new(
        "human:test",
        |req: &AdjudicationRequest| {
            Some(AdjudicationDecision::selected(
                req,
                "allow_once",
                "human:test",
                "the person said yes",
            ))
        },
    )))
    .with_surroundings(pinned());
    // **Exec, and still asking** (R21). `ls` is a look and nothing else, so a shell
    // carrying it is clause 4 and no human answers — and this test is about what a
    // HUMAN's answer leaves behind in the brief. `sed -n` is the exec that still
    // reaches one; see `Baseline::reads_only`.
    let args = json!({"command": "/bin/sed -n 1p /w"});

    let req1 = human.request_for(&bash_at(&args, "s#3"));
    assert!(
        req1.prior.is_empty(),
        "nothing has been decided yet: {:?}",
        req1.prior
    );
    let _ = human.admit(&bash_at(&args, "s#3"));

    // A re-spelling of the same direction, three turns later.
    let req2 = human.request_for(&bash_at(&json!({"command": "/bin/sed -n 2p /w"}), "s#6"));
    assert_eq!(req2.prior.len(), 1, "{:?}", req2.prior);
    let p = &req2.prior[0];
    assert_eq!(p.effect, "admit");
    assert_eq!(p.count, 1);
    assert_eq!(p.latest_turns_ago, Some(3));
    assert_eq!(p.first_turns_ago, Some(3));

    // And the gate's OWN decisions stay out: a closed gate refuses every call,
    // and none of those refusals is shown back to it as history.
    let mut closed = AdjudicatedGate::closed().with_surroundings(pinned());
    let _ = closed.admit(&bash_at(&args, "s#3"));
    let after = closed.request_for(&bash_at(&args, "s#6"));
    assert!(
        after.prior.is_empty(),
        "the gate was shown its own answer: {:?}",
        after.prior
    );

    // A different direction does not inherit the answer: regions differ, and
    // a history for one shape is not a history for the next.
    let other = human.request_for(&bash_at(&json!({"command": "/bin/ls /elsewhere"}), "s#6"));
    assert!(
        other.prior.is_empty(),
        "a different region set is a different direction: {:?}",
        other.prior
    );
}

/// *"A denial is history too."* Both effects appear, denials beside approvals,
/// each with its own count and age — and the ages differ, because *"once,
/// three weeks ago"* and *"nine times this week"* are different facts.
#[test]
fn a_denial_is_history_too_and_sits_beside_the_approvals() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let n = AtomicUsize::new(0);
    let adj = AskAdjudicator::new("test", move |req: &AdjudicationRequest| {
        let i = n.fetch_add(1, Ordering::Relaxed);
        if i == 0 {
            // First: refused.
            Some(AdjudicationDecision::selected(
                req,
                "deny_and_tell",
                "human:test",
                "not today",
            ))
        } else {
            // Then: allowed.
            Some(AdjudicationDecision::selected(
                req,
                "allow_once",
                "human:test",
                "fine",
            ))
        }
    });
    let mut g = AdjudicatedGate::new(Box::new(adj)).with_surroundings(pinned());
    let args = json!({"command": "/bin/sed -n 1p /w"});
    let _ = g.admit(&bash_at(&args, "s#2"));
    let _ = g.admit(&bash_at(&args, "s#4"));

    let req3 = g.request_for(&bash_at(&args, "s#9"));
    let refuse = req3
        .prior
        .iter()
        .find(|p| p.effect == "refuse")
        .expect("the denial is history too");
    assert_eq!(refuse.count, 1);
    assert_eq!(refuse.latest_turns_ago, Some(7));
    let admit = req3
        .prior
        .iter()
        .find(|p| p.effect == "admit")
        .expect("so is the approval");
    assert_eq!(admit.count, 1);
    assert_eq!(admit.latest_turns_ago, Some(5));
}

/// *"History is evidence, never precedent"* — and it never lifts a tier. Two
/// approvals of the same shape do not turn an always-ask action into one the
/// gate admits unasked: the tier is layer A's, the option list still offers
/// only what the gate would honour, and a third ask still has to be answered
/// by somebody.
#[test]
fn a_history_of_approvals_cannot_move_an_always_ask_action() {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let n = AtomicUsize::new(0);
    let seen: std::sync::Arc<Mutex<Vec<(String, Vec<String>)>>> =
        std::sync::Arc::new(Mutex::new(Vec::new()));
    let s = seen.clone();
    let adj = AskAdjudicator::new("test", move |req: &AdjudicationRequest| {
        let i = n.fetch_add(1, Ordering::Relaxed);
        s.lock().unwrap().push((
            req.tier.as_str().to_string(),
            req.options.iter().map(|o| o.id.to_string()).collect(),
        ));
        if i < 2 {
            Some(AdjudicationDecision::selected(
                req,
                "allow_once",
                "human:test",
                "allowed once, by a human",
            ))
        } else {
            None
        }
    });
    let mut g = AdjudicatedGate::new(Box::new(adj)).with_surroundings(pinned());
    let args = json!({"command": "sudo systemctl restart foo"});
    // Two approvals in the direction — `sudo`, the privilege-escalation ask.
    assert_eq!(g.admit(&bash_at(&args, "s#1")), GateDecision::Admit);
    assert_eq!(g.admit(&bash_at(&args, "s#2")), GateDecision::Admit);

    // The third call: the history says admitted twice, and the gate asks
    // anyway — and with nobody to answer, nothing ran.
    match g.admit(&bash_at(&args, "s#3")) {
        GateDecision::Refuse {
            outcome: ToolOutcome::NotRun { .. },
            ..
        } => {}
        other => panic!("history must not admit an always-ask action: {other:?}"),
    }
    let tiers = seen.lock().unwrap();
    for (tier, options) in tiers.iter() {
        assert_eq!(tier, "always_ask", "the tier never moved: {tier}");
        assert!(
            !options.iter().any(|o| o == "allow_session"),
            "an always-ask action is never offered a standing grant: {options:?}"
        );
    }
}
