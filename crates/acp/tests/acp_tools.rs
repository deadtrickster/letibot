//! A file-changing call, end to end: the daemon asks the adapter's client for permission, the
//! client grants it, the call runs, and its changed lines come back as an ACP `diff`.
//!
//! This is the row's third clause ("edits to ACP `diff`") over the real wire: the excerpt the
//! daemon publishes after a call changed a file, drawn as the two sides ACP clients show in their
//! editor. It is also the one place the permission ladder is proved to survive the trip - and it
//! uses `write` and not `bash` on purpose, because a command needs a cgroup of its own and a file
//! tool is what a `leticode` seat has on every machine.

mod support;

use serde_json::{Value, json};
use support::{Answer, Client, World};

/// What the model writes: the article, plus one line at its end.
const ADDED: &str = "a line the call wrote\n";
/// What the model says once the call has come back.
const AFTER: &str = "the file now has one more line";

#[test]
fn a_gated_call_asks_the_client_answers_and_the_change_comes_back_as_a_diff() {
    let world = World::start(
        "acp-tools",
        // Writes are asked about at this point, which is what produces one permission card.
        letibot_tools::mode::Mode::ALWAYS_ASK,
        vec![
            Answer::Call {
                name: "write",
                arguments: json!({
                    "path": "article.md",
                    "content": format!("{}{ADDED}", support::article_text()),
                }),
            },
            Answer::Say(&[AFTER]),
        ],
    );
    let (mut client, session) = Client::opened(&world.worktree);

    let id = client.send(
        "session/prompt",
        json!({
            "sessionId": session,
            "prompt": [{"type": "text", "text": "add a line to the article"}],
        }),
    );

    // The client's side of `session/request_permission`: it grants once, and reads the ladder the
    // daemon offered rather than guessing an option id.
    let granted = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let granted_for_answer = granted.clone();
    let (answer, updates) = client.until(id, move |asked: &Value| {
        if asked["method"] != "session/request_permission" {
            return None;
        }
        let options = asked["params"]["options"]
            .as_array()
            .expect("a permission request carries options");
        let allow = options
            .iter()
            .find(|o| o["kind"] == "allow_once")
            .expect("an always-ask ladder offers allow_once");
        let option_id = allow["optionId"]
            .as_str()
            .expect("an option has an id")
            .to_string();
        *granted_for_answer.lock().unwrap() = option_id.clone();
        Some(json!({"outcome": {"outcome": "selected", "optionId": option_id}}))
    });

    assert_eq!(
        answer["result"]["stopReason"], "end_turn",
        "the turn ended once the call had come back and the model was asked again: {answer}"
    );
    assert!(
        !granted.lock().unwrap().is_empty(),
        "the client was asked for permission before the file changed: {updates:?}"
    );

    // --- the call's own rows: a `tool_call`, then an update with a diff ---------
    let calls: Vec<&Value> = updates
        .iter()
        .filter(|u| u["params"]["update"]["sessionUpdate"] == "tool_call")
        .collect();
    let call = calls.first().expect("the call was proposed to the client");
    assert_eq!(
        call["params"]["update"]["kind"], "edit",
        "a `write` call is an `edit` tool: {call}"
    );
    assert_eq!(
        call["params"]["update"]["title"], "write: article.md",
        "the title names the tool and its one readable argument: {call}"
    );

    let finished = updates
        .iter()
        .rev()
        .find(|u| {
            u["params"]["update"]["sessionUpdate"] == "tool_call_update"
                && u["params"]["update"]["toolCallId"] == call["params"]["update"]["toolCallId"]
        })
        .expect("the call's end reached the client");
    assert_eq!(finished["params"]["update"]["status"], "completed");

    let diffs: Vec<&Value> = finished["params"]["update"]["content"]
        .as_array()
        .expect("the changed region is the update's content")
        .iter()
        .filter(|c| c["type"] == "diff")
        .collect();
    let diff = diffs.first().expect("an edit is a diff");
    assert_eq!(
        diff["path"],
        world.worktree.join("article.md").display().to_string(),
        "the diff names the file the client can open"
    );
    assert!(
        diff["newText"]
            .as_str()
            .unwrap_or("")
            .contains(ADDED.trim()),
        "the new side is what the call wrote: {diff}"
    );
    assert_ne!(
        diff["oldText"], diff["newText"],
        "a diff is two sides: {diff}"
    );

    // --- and the file on disk is what the diff said -----------------------------
    let on_disk = std::fs::read_to_string(world.worktree.join("article.md")).expect("the article");
    assert!(
        on_disk.contains(ADDED.trim()),
        "the call really ran: {on_disk}"
    );
    assert_eq!(
        world.fake.requests().len(),
        2,
        "one round proposed the call and one answered past it"
    );
}
