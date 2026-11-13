//! `session/cancel` stops a streaming turn, and the answer is `cancelled` - the one stop reason ACP
//! says a client can rely on confirming.
//!
//! The provider writes its reply a piece at a time, so the cancel lands mid-stream: a body held
//! until the last piece would make this test a race between two clocks.

mod support;

use serde_json::json;
use support::{Answer, Client, World};

const PIECES: [&str; 8] = [
    "one ", "two ", "three ", "four ", "five ", "six ", "seven ", "eight ",
];

#[test]
fn a_client_that_cancels_mid_stream_is_answered_cancelled() {
    let world = World::start(
        "acp-cancel",
        letibot_tools::mode::Mode::ALWAYS_ASK,
        vec![Answer::Slow(&PIECES)],
    );
    let (mut client, session) = Client::opened(&world.worktree);

    let id = client.send(
        "session/prompt",
        json!({
            "sessionId": session,
            "prompt": [{"type": "text", "text": "count, slowly"}],
        }),
    );

    // Two pieces arrive, then the stop button: the turn is running and something has been drawn.
    let (answer, updates) = client.until_cancelled(id, &session, 2);

    assert_eq!(
        answer["result"]["stopReason"], "cancelled",
        "ACP says a cancelled turn is answered `cancelled` and not with an error: {answer}"
    );
    let chunks = updates
        .iter()
        .filter(|u| u["params"]["update"]["sessionUpdate"] == "agent_message_chunk")
        .count();
    assert!(
        chunks >= 2 && chunks < PIECES.len(),
        "the turn was stopped part way through its reply, not after it: {chunks} of {}",
        PIECES.len()
    );
    assert!(
        world.fake.requests().len() <= 1,
        "stopping a turn asks the model for nothing more"
    );
}
