//! **The tool-call counter's panel, while the calls are still running** — the operator, from a
//! fresh session: *"model started toolcalling (yellow counter), clicked on the tools counter —
//! empty panel, Esc brings me back"*.
//!
//! The counter's numbers are the LIVE pane's (`LiveWork`): a round's result rows are appended only
//! after every call in it has run, so at the moment the marker appears the transcript holds nothing
//! about the calls it counts. What a click on it opens has to show the calls themselves.

use super::*;

/// **The tool-call counter's panel, mid-turn** — the operator's *"model started toolcalling (yellow
/// counter), clicked on the tools counter — empty panel"*.
///
/// A round's result rows are appended only after every call in it has run, so while the calls are
/// running the transcript holds nothing about them: the counter's numbers are the live pane's
/// (`LiveWork`), and the panel that opens has to show the calls themselves. This is the state the
/// bug lived in — a turn one round deep, a marker carrying two running calls, and an open run whose
/// rows are a reasoning row the "tool calls" clause deliberately hides.
fn a_turn_with_two_calls_running() -> App {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    // The operator asked, the model narrated, the calls are out and none has returned.
    a.apply(ServerFrame::Event(env_at(
        1,
        1_000,
        testing::turn_started("t1"),
    )));
    a.apply(ServerFrame::Event(env(2, testing::appended("s.0", "user"))));
    a.apply(ServerFrame::Event(env(
        3,
        SessionEvent::TranscriptContent {
            item_id: "s.0".into(),
            item: Box::new(TranscriptItem::User {
                speaker: letibot_transcript::Speaker::Operator,
                parts: vec![UserPart::Text {
                    text: "check the two files".into(),
                }],
            }),
        },
    )));
    a.apply(ServerFrame::Event(env(
        4,
        testing::appended("s.1", "assistant"),
    )));
    a.apply(ServerFrame::Event(env(
        5,
        SessionEvent::TranscriptContent {
            item_id: "s.1".into(),
            item: Box::new(TranscriptItem::Assistant {
                text: "let me look at both:".into(),
                tool_calls: vec![
                    letibot_transcript::ToolCall {
                        id: "c0".into(),
                        name: "read".into(),
                        arguments: "{\"path\": \"a.rs\"}".into(),
                    },
                    letibot_transcript::ToolCall {
                        id: "c1".into(),
                        name: "read".into(),
                        arguments: "{\"path\": \"b.rs\"}".into(),
                    },
                ],
                truncated: false,
            }),
        },
    )));
    // The model's working-out, landed as a row the conversation rung hides — which is what makes
    // the run the marker is on a run of REASONING while the calls it counts have no rows at all.
    a.apply(ServerFrame::Event(env(
        6,
        testing::appended("s.2", "reasoning"),
    )));
    a.apply(ServerFrame::Event(env(
        7,
        SessionEvent::TranscriptContent {
            item_id: "s.2".into(),
            item: Box::new(TranscriptItem::Reasoning {
                text: "two reads, then I will compare".into(),
                field: letibot_transcript::ReasoningField::ReasoningContent,
                truncated: false,
            }),
        },
    )));
    for (i, (id, name)) in [("c0", "read"), ("c1", "read")].iter().enumerate() {
        a.apply(ServerFrame::Event(env_at(
            8 + 2 * i as u64,
            1_100,
            testing::proposed("t1", id, name),
        )));
        a.apply(ServerFrame::Event(env_at(
            9 + 2 * i as u64,
            1_200,
            SessionEvent::ToolStarted {
                turn_id: "t1".into(),
                call_id: (*id).into(),
                name: (*name).into(),
                access: Default::default(),
            },
        )));
    }
    a.visibility = Visibility::of(Profile::CONVERSATION);
    a.invalidate_history();
    a
}

fn find(screen: &[String], needle: &str) -> (u16, u16) {
    for (y, l) in screen.iter().enumerate() {
        let cells: String = crate::app::keyhints::cells(l).into_iter().collect();
        if let Some(x) = cells.find(needle) {
            let x = cells[..x].chars().count();
            return (x as u16, y as u16);
        }
    }
    panic!("{needle:?} not on screen:\n{}", screen.join("\n"));
}

#[test]
fn the_tool_call_counter_lists_the_calls_that_are_still_running() {
    let mut a = a_turn_with_two_calls_running();
    let screen = a.screen(120, 30);
    let (x, y) = find(&screen, "2 tool calls");
    assert_eq!(a.key(Key::Click { x: x + 1, y }), None);
    let open = a.screen(120, 30).join("\n");
    // The card's verb for a running read is `Reading`; two calls, two cards.
    assert_eq!(
        open.matches("Reading").count(),
        2,
        "the panel is empty while two calls run:\n{open}"
    );
    assert!(
        open.contains("esc folds the run back"),
        "the panel names no way out:\n{open}"
    );
    // **And Esc folds it**, rather than leaving a lift nobody can undo.
    a.key(Key::Esc);
    let folded = a.screen(120, 30).join("\n");
    assert!(
        !folded.contains("Reading"),
        "esc left the calls open:\n{folded}"
    );
    assert!(folded.contains("2 tool calls"), "{folded}");
}

/// **The counter's panel keeps its place when the round's rows land** — the calls stop being live
/// and become transcript rows, and the reader who opened the panel is looking at the same calls,
/// not at an empty one that lost its lift.
#[test]
fn the_panel_holds_the_calls_when_their_rows_land() {
    let mut a = a_turn_with_two_calls_running();
    let screen = a.screen(120, 30);
    let (x, y) = find(&screen, "2 tool calls");
    a.key(Key::Click { x: x + 1, y });
    assert!(a.screen(120, 30).join("\n").contains("Reading"));
    // The round returns: two result rows land, one append each, and the bodies follow.
    for (i, (id, path)) in [("c0", "a.rs"), ("c1", "b.rs")].iter().enumerate() {
        let item = format!("s.{}", 3 + i);
        a.apply(ServerFrame::Event(env(
            12 + 2 * i as u64,
            testing::appended(&item, "tool_result"),
        )));
        a.apply(ServerFrame::Event(env(
            13 + 2 * i as u64,
            SessionEvent::TranscriptContent {
                item_id: item,
                item: Box::new(TranscriptItem::ToolResult {
                    call_id: (*id).into(),
                    name: "read".into(),
                    outcome: letibot_transcript::ToolOutcome::Ok,
                    payload: format!("CONTENTS-{path}"),
                    edit: None,
                    origin: None,
                    media: None,
                }),
            },
        )));
    }
    let after = a.screen(120, 30).join("\n");
    assert!(
        after.contains("CONTENTS-a.rs") && after.contains("CONTENTS-b.rs"),
        "the panel lost the calls the round landed:\n{after}"
    );
}
