//! A second `session/prompt` while one runs, on the lane the client asks for. ACP has no field for
//! it, so the client says `_meta.letibot.delivery: "queue"`: the prompt is taken, the daemon runs it
//! after the turn that is running, and the two answers arrive one per turn end - which is the whole
//! reason the second prompt is refused on the steer lane instead.

mod support;

use letibot_sessionlog::SessionEvent;
use serde_json::json;
use support::{Answer, Client, World};

const FIRST: &str = "first-marker: say the one line";
const QUEUED: &str = "queued-marker: and then land it";

/// The turns this session has started, from the daemon's own published events: what tells "a turn
/// of its own" from "a steer that extended the running one".
fn turns_started(world: &World, session: &str) -> usize {
    let hub = world.registry.get(session).expect("the session is open");
    hub.retained()
        .iter()
        .filter(|e| matches!(e.event, SessionEvent::TurnStarted { .. }))
        .count()
}

#[test]
fn a_prompt_sent_with_the_queue_lane_runs_after_the_turn_and_is_not_a_steer() {
    let world = World::start(
        "acp-queue-lane",
        letibot_tools::mode::Mode::ALWAYS_ASK,
        vec![
            Answer::Held(&["first-answer-marker"]),
            Answer::Say(&["queued-answer-marker"]),
        ],
    );
    let (mut client, session) = Client::opened(&world.worktree);

    let first = client.send(
        "session/prompt",
        json!({"sessionId": session, "prompt": [{"type": "text", "text": FIRST}]}),
    );
    // Mid-turn: the first round is in the fake's hands and answers nothing until the gate opens.
    letibot_sessionlog::testing::wait_until("the first turn to reach the model", || {
        !world.fake.requests().is_empty()
    });

    // The lane is the client's to name, and it is ACP's own `_meta` rather than a new field.
    let second = client.send(
        "session/prompt",
        json!({
            "sessionId": session,
            "prompt": [{"type": "text", "text": QUEUED}],
            "_meta": {"letibot": {"delivery": "queue"}},
        }),
    );

    world.hold.release();
    let (one, _) = client.until(first, |_| None);
    assert!(
        one.get("error").is_none(),
        "the first prompt is a turn of its own: {one}"
    );
    assert_eq!(one["result"]["stopReason"], json!("end_turn"), "{one}");
    let (two, _) = client.until(second, |_| None);
    assert!(
        two.get("error").is_none(),
        "a queued prompt is taken rather than refused: {two}"
    );
    assert_eq!(two["result"]["stopReason"], json!("end_turn"), "{two}");

    // One turn per prompt, and one round each: a steer would have been a second round of the first
    // turn, so the session would show one turn start and a second body carrying both lines.
    assert_eq!(
        turns_started(&world, &session),
        2,
        "the queued prompt waited for the turn to end and opened its own"
    );
    let bodies = world.fake.requests();
    assert_eq!(bodies.len(), 2, "one round each: {bodies:?}");
    assert!(
        !bodies[0].contains(QUEUED),
        "the running turn did not take it: {bodies:?}"
    );
    assert!(
        bodies[1].contains(QUEUED),
        "the queued line is the second turn's: {bodies:?}"
    );
}
