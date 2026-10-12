//! **A run's marker opens by its parts** — the operator: *"when I see this "[7 tool calls, 11
//! thinking lines]" and then click on tool calls section or thinking section i wanted them to
//! expand. Esc should fold them back"*.

use super::*;

/// The shape of `a_run_of_hidden_rows_draws_one_line_that_continues_the_sentence`: prose ending in
/// a colon, one reasoning row, four tool results — at the conversation rung, one marker.
fn a_run() -> App {
    a_run_of(
        "let me check the file\nand then the other one\nand then the tests",
        0,
    )
}

/// The same run, its reasoning `think` and every result `extra` lines longer.
fn a_run_of(think: &str, extra: usize) -> App {
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
                text: think.into(),
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
                    payload: std::iter::once(format!("CONTENTS-{i}"))
                        .chain((0..extra).map(|j| format!("LINE-{i}-{j}")))
                        .collect::<Vec<_>>()
                        .join("\n"),
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

/// **A run opened by a click still scrolls, both ways and to both ends** — the operator: *"when i
/// click on thinking in the [tools, thinking lines] block, the scroll is stuck and broken"*.
///
/// The click opens the run by naming it in `payload_sel`, which is also what an open ctrl-v
/// window is named by — and every scroll key asked only `payload_sel.is_some()`, so the run's
/// wheel and page keys went to a window that is not there: down moved a page offset nobody draws
/// (and threw the history away each notch), and the view stood still.
fn opened_run_scrolls(clause: &str, shown: &str) {
    let think: Vec<String> = (0..132).map(|i| format!("THINK-{i:03}")).collect();
    let mut a = a_run_of(&think.join("\n\n"), 3);
    let (w, h) = (100usize, 16usize);
    let screen = a.screen(w, h);
    let (x, y) = find(&screen, clause);
    a.key(Key::Click { x: x + 1, y });
    let open = a.screen(w, h).join("\n");
    assert!(open.contains(shown), "the run is open:\n{open}");
    // And the bar does not offer a window's keys for a run that has no window.
    assert!(
        !open.contains("page the result") && open.contains("esc folds the run back"),
        "{open}"
    );
    let (wx, wy) = find(&a.screen(w, h), shown);
    let top = "here is what I am about to do";
    let bottom = if shown.starts_with("THINK") {
        "THINK-131"
    } else {
        "CONTENTS-3"
    };
    let mut keys: Vec<(&str, Box<dyn Fn() -> Key>, Box<dyn Fn() -> Key>)> = vec![
        (
            "the wheel",
            Box::new(move || Key::Wheel {
                up: true,
                x: wx,
                y: wy,
            }),
            Box::new(move || Key::Wheel {
                up: false,
                x: wx,
                y: wy,
            }),
        ),
        (
            "the wheel keys",
            Box::new(|| Key::WheelUp),
            Box::new(|| Key::WheelDown),
        ),
        (
            "the page keys",
            Box::new(|| Key::PageUp),
            Box::new(|| Key::PageDown),
        ),
        ("the arrows", Box::new(|| Key::Up), Box::new(|| Key::Down)),
    ];
    for (name, up, down) in keys.drain(..) {
        let start = a.screen(w, h).join("\n");
        assert!(
            !start.contains(top),
            "{name} starts at the bottom:\n{start}"
        );
        for _ in 0..200 {
            a.key(up());
            if a.screen(w, h).join("\n").contains(top) {
                break;
            }
        }
        let at_top = a.screen(w, h).join("\n");
        assert!(at_top.contains(top), "{name} reaches the top:\n{at_top}");
        let mut moved = false;
        for _ in 0..200 {
            let before = a.screen(w, h).join("\n");
            a.key(down());
            let s = a.screen(w, h).join("\n");
            moved |= s != before;
            if s.contains(bottom) && s == before {
                break;
            }
        }
        let at_bottom = a.screen(w, h).join("\n");
        assert!(moved, "{name} moved the view down");
        assert!(
            at_bottom.contains(bottom) && !at_bottom.contains(top),
            "{name} reaches the bottom:\n{at_bottom}"
        );
        assert!(a.following(), "{name} at the bottom follows again");
        assert!(a.run_is_open(), "{name} leaves the run open");
    }
}

#[test]
fn a_run_opened_on_its_thinking_scrolls_to_both_ends() {
    opened_run_scrolls("thinking lines", "THINK-131");
}

#[test]
fn a_run_opened_on_its_tool_calls_scrolls_to_both_ends() {
    opened_run_scrolls("tool calls", "CONTENTS-3");
}
