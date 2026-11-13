//! The adapter against a real daemon, driven the way a client drives it: the binary as a child
//! process, JSON-RPC on its stdio, and a session on a socket this test bound.
//!
//! What is asserted is the row's own bar: an ACP client opens a session, prompts with a ranged
//! resource, and gets a streamed reply - and the model was handed the ranged block, because a
//! resource that never reached the turn would still produce a reply here.

mod support;

use serde_json::{Value, json};
use support::{Client, World};

/// The reply, in the pieces the provider streams: more than one, because "streamed" is the claim
/// and a single delta would be indistinguishable from one answer.
const CHUNKS: [&str; 3] = [
    "The first transatlantic cable ",
    "was laid in 1866, and the record ",
    "of it is in the article.",
];
const BRIEF: &str = "when was the cable laid?";
const PASSAGE: &str = "The first transatlantic cable was laid in 1866";

#[test]
fn an_acp_client_opens_a_session_prompts_with_a_ranged_resource_and_is_answered_in_a_stream() {
    let world = World::start(
        "acp-client",
        letibot_tools::mode::Mode::ALWAYS_ASK,
        vec![support::Answer::Say(&CHUNKS)],
    );
    let (mut client, session) = Client::opened(&world.worktree);
    let article = world.worktree.join("article.md").display().to_string();

    // --- session/prompt: the words, and a resource with lines -------------------
    let id = client.send(
        "session/prompt",
        json!({
            "sessionId": session,
            "prompt": [
                {"type": "text", "text": BRIEF},
                {
                    "type": "resource",
                    "resource": {
                        "uri": format!("file://{article}#L41-L47"),
                        "mimeType": "text/markdown",
                        "text": world.passage(),
                    },
                    "_meta": {"letibot": {"unsaved": true}},
                },
            ],
        }),
    );

    let (answer, updates) = client.until(id, |_| None);
    assert_eq!(
        answer["result"]["stopReason"], "end_turn",
        "a turn that ended is an end_turn, and nothing else: {answer}"
    );
    let chunks: Vec<&Value> = updates
        .iter()
        .filter(|u| u["params"]["update"]["sessionUpdate"] == "agent_message_chunk")
        .collect();
    assert!(
        chunks.len() >= 2,
        "the reply is a stream and not one blob: {updates:?}"
    );
    for chunk in &chunks {
        assert_eq!(
            chunk["params"]["sessionId"].as_str(),
            Some(session.as_str()),
            "every update names the session it belongs to"
        );
        assert_eq!(
            chunk["params"]["update"]["content"]["type"], "text",
            "a chunk of the reply is text"
        );
    }
    let said: String = chunks
        .iter()
        .map(|c| {
            c["params"]["update"]["content"]["text"]
                .as_str()
                .unwrap_or("")
        })
        .collect();
    assert_eq!(said, CHUNKS.concat(), "the whole reply, in order");

    // --- and the turn the model actually ran carried the ranged block -----------
    let asked = world.fake.requests().join("\n");
    assert!(
        asked.contains(BRIEF),
        "the operator's words reached the model: {asked}"
    );
    assert!(
        asked.contains("article.md:41-47"),
        "the resource's range is the header the daemon prints: {asked}"
    );
    assert!(
        asked.contains(PASSAGE),
        "the selection's own text is what the model read: {asked}"
    );
    assert!(
        asked.contains("unsaved edits"),
        "the client said the buffer was the source, and the daemon says so to the model: {asked}"
    );
}
