//! The two lanes `leticode_send` can put a line on. A line sent mid-turn on the steer lane joins the
//! turn that is running; one sent with `queue` waits for it to end and gets a turn of its own -
//! which is what `leticode_send`'s `queue` field is for, and the only thing that tells them apart
//! from outside (two turns, not one turn with two rounds).

#[path = "support/world.rs"]
mod world;

use letibot_bridge::tools::{self, Bridge};
use letibot_sessionlog::testing::wait_until;
use serde_json::json;
use world::World;

/// The brief's turn, and the line sent while it runs. Markers, because the fake's bodies are the
/// only place a prompt's words and a turn's round can be read back.
const BRIEF: &str = "brief-marker: say the one line";
const QUEUED: &str = "queued-marker: and then land it";

/// The `turn` entries of the bridge's own progress ring, as `started`/`ended` words in order.
fn turns(bridge: &Bridge, session: &str) -> Vec<String> {
    let events = bridge
        .events(session, None, None)
        .expect("the ring answers");
    let entries = events["entries"].as_array().expect("entries").clone();
    entries
        .iter()
        .filter(|e| e["kind"] == "turn")
        .map(|e| e["state"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[test]
fn a_line_sent_with_queue_waits_for_the_turn_and_is_not_a_steer() {
    let world = World::start_held("prompt-lane");
    let bridge = Bridge::new();
    let started = tools::call(
        &bridge,
        "leticode_start",
        &json!({
            "worktree": world.worktree(),
            "brief": BRIEF,
            "vm": false,
        }),
    )
    .expect("the session starts");
    let session = started["session"].as_str().expect("an id").to_string();

    // Mid-turn: the brief's first model round is in the fake's hands and goes no further until the
    // gate opens, so the send below lands on a turn that is provably still running.
    wait_until("the brief's round to reach the model", || {
        !world.fake.requests().is_empty()
    });
    let sent = tools::call(
        &bridge,
        "leticode_send",
        &json!({ "session": session, "text": QUEUED, "queue": true }),
    )
    .expect("the line is taken");
    assert_eq!(sent["lane"], json!("queue"), "{sent}");

    world.hold().release();
    wait_until("the queued line's own turn to end", || {
        turns(&bridge, &session)
            .iter()
            .filter(|w| w.as_str() == "ended")
            .count()
            >= 2
    });

    // One turn per prompt: a steer would have extended the first turn with a second round instead,
    // and the ring would hold one start and one end however many rounds it took.
    assert_eq!(
        turns(&bridge, &session),
        vec![
            "started".to_string(),
            "ended".to_string(),
            "started".to_string(),
            "ended".to_string()
        ],
        "the queued line waited for the turn to end and opened its own"
    );
    let bodies: Vec<String> = world.fake.requests();
    assert_eq!(bodies.len(), 2, "one round each: {bodies:?}");
    assert!(
        !bodies[0].contains(QUEUED),
        "the turn did not take it: {bodies:?}"
    );
    assert!(
        bodies[1].contains(QUEUED),
        "the queued line is the second turn's: {bodies:?}"
    );
    // And the answer says which lane it took, so a caller reads the same fact the daemon acted on.
    assert_eq!(sent["queued"], json!(true), "{sent}");
}
