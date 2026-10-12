//! **A run's marker opens by its parts** — the operator: *"when I see this "[7 tool calls, 11
//! thinking lines]" and then click on tool calls section or thinking section i wanted them to
//! expand. Esc should fold them back"*.

use super::*;

/// The shape of `a_run_of_hidden_rows_draws_one_line_that_continues_the_sentence`: prose ending in
/// a colon, one reasoning row, four tool results — at the conversation rung, one marker.
fn a_run() -> App {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    // **The operator's own shape**: an answer that ENDS IN A COLON, then the work. An
    // assistant row is the conversation, so it is kept — and the marker has to read as
    // the continuation of the sentence it points at.
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::TranscriptAppended {
            item_id: "s.0".into(),
            kind: "assistant".into(),
            ledger_head: String::new(),
        },
    )));
    // **The row carries the calls as well as the prose**, which is the shape the daemon
    // writes and the only place a row's *target* survives: `targets_before` reads the
    // nearest assistant row's `tool_calls` arguments, so a fixture that published only
    // proposals would leave every target empty — and this test would then be asserting
    // that a marker with no target is a marker with the right target.
    let calls: Vec<(&str, &str, &str)> = vec![
        ("c0", "read", "crates/tui/src/app.rs"),
        ("c1", "grep", "ctrl-t"),
        ("c2", "edit", "crates/tui/src/chrome.lisp"),
        ("c3", "read", "crates/tui/src/term.rs"),
    ];
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::TranscriptContent {
            item_id: "s.0".into(),
            item: Box::new(TranscriptItem::Assistant {
                text: "here is what I am about to do:".into(),
                tool_calls: calls
                    .iter()
                    .map(|(id, name, arg)| letibot_transcript::ToolCall {
                        id: (*id).into(),
                        name: (*name).into(),
                        arguments: format!("{{\"path\": \"{arg}\"}}"),
                    })
                    .collect(),
                truncated: false,
            }),
        },
    )));
    // One reasoning block, then four tool results — all hidden, all contiguous.
    a.apply(ServerFrame::Event(env(
        3,
        SessionEvent::TranscriptAppended {
            item_id: "s.1".into(),
            kind: "reasoning".into(),
            ledger_head: String::new(),
        },
    )));
    a.apply(ServerFrame::Event(env(
        4,
        SessionEvent::TranscriptContent {
            item_id: "s.1".into(),
            item: Box::new(TranscriptItem::Reasoning {
                text: "let me check the file\nand then the other one\nand then the tests".into(),
                field: letibot_transcript::ReasoningField::ReasoningContent,
                truncated: false,
            }),
        },
    )));
    for (i, (call, name, _arg)) in calls.iter().enumerate() {
        let id = format!("s.{}", i + 2);
        a.apply(ServerFrame::Event(env(
            (i as u64) * 2 + 5,
            testing::appended(&id, "tool_result"),
        )));
        a.apply(ServerFrame::Event(env(
            (i as u64) * 2 + 6,
            SessionEvent::TranscriptContent {
                item_id: id.clone(),
                item: Box::new(TranscriptItem::ToolResult {
                    call_id: (*call).into(),
                    name: (*name).into(),
                    outcome: letibot_transcript::ToolOutcome::Ok,
                    payload: format!("CONTENTS-{i}"),
                    edit: None,
                    origin: None,
                    media: None,
                }),
            },
        )));
    }
    a.visibility = Visibility::of(Profile::CONVERSATION);
    a.invalidate_history();
    a
}

/// Where `needle` starts on the screen, as (x, y).
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
fn a_click_on_the_tool_calls_opens_them_and_esc_folds_them_back() {
    let mut a = a_run();
    let screen = a.screen(200, 40);
    let (x, y) = find(&screen, "4 tool calls");
    assert_eq!(a.key(Key::Click { x: x + 1, y }), None);
    let open = a.screen(200, 40).join("\n");
    assert!(
        open.contains("CONTENTS-0"),
        "the tool calls are open:\n{open}"
    );
    assert!(
        !open.contains("let me check the file"),
        "and only the tool calls:\n{open}"
    );
    assert_eq!(a.key(Key::Esc), None);
    let folded = a.screen(200, 40).join("\n");
    assert!(
        !folded.contains("CONTENTS-0"),
        "esc folds them back:\n{folded}"
    );
    assert!(
        folded.contains("[4 tool calls, 3 thinking lines]"),
        "{folded}"
    );
}

#[test]
fn a_click_on_the_thinking_lines_opens_them() {
    let mut a = a_run();
    let screen = a.screen(200, 40);
    let (x, y) = find(&screen, "3 thinking lines");
    a.key(Key::Click { x: x + 1, y });
    let open = a.screen(200, 40).join("\n");
    assert!(
        open.contains("let me check the file"),
        "the thinking is open:\n{open}"
    );
    assert!(
        !open.contains("CONTENTS-0"),
        "and only the thinking:\n{open}"
    );
}

/// The pointer over a part lights that part, as over any other clickable thing.
#[test]
fn hovering_a_part_of_the_marker_lights_it() {
    let mut a = a_run();
    let screen = a.screen(200, 40);
    let (x, y) = find(&screen, "4 tool calls");
    a.key(Key::Hover { x: x + 1, y });
    let lit = a.screen(200, 40);
    assert!(
        lit[y as usize].contains("\x1b[22;1m"),
        "{:?}",
        lit[y as usize]
    );
}

/// **No light is left on what a click opened** — the operator: *"stuck highlight still hapens on
/// expanded reasoning and tools - when i click"*. The light is worked out from the frame that is
/// drawn, so the rows the click put under the pointer are lit only if they are clickable
/// themselves.
#[test]
fn a_click_that_opens_a_run_leaves_no_light_on_what_it_opened() {
    let mut a = a_run();
    let screen = a.screen(200, 40);
    let (x, y) = find(&screen, "3 thinking lines");
    a.key(Key::Hover { x: x + 1, y });
    assert!(
        a.screen(200, 40)[y as usize].contains("\x1b[22;1m"),
        "the premise"
    );
    a.key(Key::Click { x: x + 1, y });
    let open = a.screen(200, 40);
    for (i, l) in open.iter().enumerate() {
        assert!(
            !l.contains("\x1b[22;1m"),
            "row {i} is lit after the click: {l:?}"
        );
    }
}
