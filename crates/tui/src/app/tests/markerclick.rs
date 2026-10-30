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

/// Where the LAST `needle` starts on the screen — for a frame holding two markers of the same
/// shape, where the test means the lower one.
fn find_last(screen: &[String], needle: &str) -> (u16, u16) {
    for (y, l) in screen.iter().enumerate().rev() {
        let cells: String = crate::app::keyhints::cells(l).into_iter().collect();
        if let Some(x) = cells.rfind(needle) {
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
///
/// **And the marker is drawn again over them** — an open run's marker is its header and its
/// clauses are the toggle (`a_second_click_on_the_same_clause_puts_it_back`), so the line under
/// the pointer IS a button and lights. What must not light is the content it opened.
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
    let (_, marker) = find(&open, "thinking lines");
    for (i, l) in open.iter().enumerate() {
        if i == marker as usize {
            continue;
        }
        assert!(
            !l.contains("\x1b[22;1m"),
            "row {i} is lit after the click: {l:?}"
        );
    }
    assert!(
        open[marker as usize].contains("\x1b[22;1m"),
        "the clause under the pointer is a button and does not say so: {:?}",
        open[marker as usize]
    );
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

// ───────────────── one switch: the click, the chord and esc ─────────────────

/// **A head at `read-edits` with a turn in flight that has thought and committed no row** — the
/// state the operator's click is made in. The rung hides the working, so the live pane draws its
/// own marker and the thinking is behind it; that is the marker whose thinking clause they click.
fn a_live_thought(thought: &str) -> App {
    let mut a = app();
    a.visibility = Visibility::of(Profile::READ_EDITS);
    a.apply(hello(
        "s",
        vec![brief("s", "one", true)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(env_at(
        1,
        1_000,
        SessionEvent::TurnStarted {
            turn_id: "r1".into(),
            model: "qwen3-next-80b".into(),
            ledger_head: "0000".into(),
            began_ms: Some(1_000),
        },
    )));
    a.apply(ServerFrame::Event(env_at(
        2,
        1_100,
        testing::reasoning("r1", thought),
    )));
    a
}

/// The frame, as a person reads it.
fn frame(a: &mut App) -> String {
    a.screen(100, 30).join("\n")
}

/// **A click on the thinking clause and `ctrl-r` flip one state** — the operator's *"sometimes when
/// i click on thinking counter it gets stuck — the openness — and then all subsequent turns go with
/// think open. ctrl-r doesnt help, esc too"*.
///
/// The click lifts a run (`App::lift`), `ctrl-r` moved a `Fold` — two fields, so neither key could
/// reach the other's, and the lift had no key that closed it at all. They are one state now, asked
/// of what is drawn (`App::thinking_open`), and every door goes through `App::set_thinking`.
#[test]
fn a_click_on_thinking_and_ctrl_r_are_one_verb() {
    const THOUGHT: &str = "THINK-LIVE the round's thinking, no row yet";
    let mut a = a_live_thought(THOUGHT);
    assert!(
        !frame(&mut a).contains(THOUGHT),
        "the premise: it starts closed"
    );

    // The click opens it.
    let (x, y) = find(&a.screen(100, 30), "thinking line");
    a.key(Key::Click { x: x + 1, y });
    assert!(
        frame(&mut a).contains(THOUGHT),
        "the click opened the thinking:\n{}",
        frame(&mut a)
    );
    assert!(a.thinking_open());

    // `ctrl-r` closes what a click opened.
    a.key(Key::CtrlR);
    assert!(!a.thinking_open(), "ctrl-r left the state open");
    assert!(
        !frame(&mut a).contains(THOUGHT),
        "ctrl-r did not close what the click opened:\n{}",
        frame(&mut a)
    );

    // And it opens what the click closed — the newest run there is, at a rung that hides the
    // thinking and so cannot express it any other way.
    a.key(Key::CtrlR);
    assert!(a.thinking_open(), "ctrl-r did not open it again");
    assert!(
        frame(&mut a).contains(THOUGHT),
        "ctrl-r opened nothing:\n{}",
        frame(&mut a)
    );

    // `esc` returns to the default from either spelling of open.
    a.key(Key::Esc);
    assert!(!a.thinking_open(), "esc left the state open");
    assert!(
        !frame(&mut a).contains(THOUGHT),
        "esc did not return it to the default:\n{}",
        frame(&mut a)
    );
    // And the exit is on the screen: the marker that names the state is drawn again.
    assert!(
        frame(&mut a).contains("thinking line"),
        "the marker the reader clicks is not on the screen:\n{}",
        frame(&mut a)
    );
}

/// **A second click on the same clause puts it back** — the operator's *"click once opens, click
/// again closes"*, pressed with a real `Key::Click` on a **landed** run: the case in their report,
/// and the case that needs the marker to still be drawn while the run is open (an open run's
/// marker is its header). The old click only ever set the lift, so a second press was a third way
/// to open it, and nothing on the frame produced the toggle's hit at all.
#[test]
fn a_second_click_on_the_same_clause_puts_it_back() {
    let mut a = a_run();
    let (x, y) = find(&a.screen(200, 40), "3 thinking lines");
    a.key(Key::Click { x: x + 1, y });
    let open = a.screen(200, 40).join("\n");
    assert!(
        open.contains("let me check the file"),
        "the first click opens the thinking:\n{open}"
    );
    // **The marker is still on the screen**, and its clause is still a target — that is what makes
    // the second press a click at all.
    let (x, y) = find(&a.screen(200, 40), "3 thinking lines");
    a.key(Key::Click { x: x + 1, y });
    assert!(!a.thinking_open(), "the second click did not close it");
    let closed = a.screen(200, 40).join("\n");
    assert!(
        !closed.contains("let me check the file"),
        "and it is off the screen:\n{closed}"
    );
    assert_eq!(a.lift, None);
}

/// **The marker is drawn while the run it opened is open** — requirement 4, and the exit R29 asks
/// a disclosure to carry.
///
/// Both walks used to ask `unseen_run_at` only when the run was CLOSED (`let unseen = if open_run
/// { None } else { … }`), so the moment a click lifted a run its marker was gone: the counts line
/// that says what is open, and the only thing on the frame that toggles it back. A state whose
/// marker is not drawn is the defect one key removed.
#[test]
fn an_open_runs_marker_is_drawn_over_the_rows_it_lifted() {
    let mut a = a_run();
    let before = a.screen(200, 40).join("\n");
    assert!(
        before.contains("[4 tool calls, 3 thinking lines]"),
        "the premise: one marker, closed:\n{before}"
    );
    let (x, y) = find(&a.screen(200, 40), "3 thinking lines");
    a.key(Key::Click { x: x + 1, y });
    let open = a.screen(200, 40).join("\n");
    assert!(
        open.contains("[4 tool calls, 3 thinking lines]"),
        "the marker went away when the run opened:\n{open}"
    );
    assert!(
        open.contains("let me check the file"),
        "and the rows it stands over are drawn under it:\n{open}"
    );
    // **The header comes first** — it is the run's first line, not a line under its content.
    let header = open
        .lines()
        .position(|l| l.contains("4 tool calls"))
        .expect("the marker is on the screen");
    let body = open
        .lines()
        .position(|l| l.contains("let me check the file"))
        .expect("the thinking is on the screen");
    assert!(
        header < body,
        "the marker is under the rows it lifted:\n{open}"
    );
}

/// **`ctrl-r` will not open a run that holds no thinking** — the reviewer's find, and the mirror of
/// *"clicked on the tools counter — empty panel"*.
///
/// At a rung that hides the working, `ctrl-r` opens the newest run's thinking. `run_skips` hides
/// every `ToolResult` row and every empty-text `Assistant` row when the kind asked for is the
/// thinking — so lifting a CALLS-ONLY run would hide the calls it does hold and draw nothing in
/// their place: `[3 tool calls]` vanished, the run blank. The base refused this press with a
/// sentence; so does this, and the marker is still on the screen.
#[test]
fn ctrl_r_will_not_open_a_run_that_holds_no_thinking() {
    let mut a = app();
    a.visibility = Visibility::of(Profile::READ_EDITS);
    a.apply(hello(
        "s",
        vec![brief("s", "one", true)],
        Hub::new("s").snapshot(),
    ));
    // A run of calls and nothing else: three results, no reasoning row at all.
    for i in 0..3u64 {
        let id = format!("s.{i}");
        a.apply(ServerFrame::Event(env(
            1 + i * 2,
            testing::appended(&id, "tool_result"),
        )));
        a.apply(ServerFrame::Event(env(
            2 + i * 2,
            SessionEvent::TranscriptContent {
                item_id: id,
                item: Box::new(TranscriptItem::ToolResult {
                    call_id: format!("c{i}"),
                    name: "bash".into(),
                    outcome: letibot_transcript::ToolOutcome::Ok,
                    payload: format!("CONTENTS-{i}"),
                    edit: None,
                    origin: None,
                    media: None,
                }),
            },
        )));
    }
    let before = a.screen(100, 30).join("\n");
    assert!(
        before.contains("[3 tool calls]"),
        "the premise: a calls-only run, folded:\n{before}"
    );
    a.key(Key::CtrlR);
    assert_eq!(
        a.lift, None,
        "the press lifted a run with no thinking in it"
    );
    let after = a.screen(100, 30).join("\n");
    assert!(
        after.contains("[3 tool calls]"),
        "the run's marker is gone and nothing is in its place:\n{after}"
    );
    assert!(
        after.contains("nothing to open"),
        "the press said nothing, so it reads as a key that does not work:\n{after}"
    );
}

/// **A new round drops the pane's lift even when the boundary carries no clock** — the reviewer's
/// minor find, and the reason the reset does not rest on `same_turn`.
///
/// `TurnStarted` fires once per ROUND. `same_turn` compares `began_ms`, and its `None` arm asks
/// whether the previous pane still reads `Running` — so a wake turn (a job settling, a monitor
/// firing: `Sessions::wake` stamps no clock) arriving over a running turn is indistinguishable
/// from another round of it. A lift addressed at the live tail belongs to the round that is over,
/// so it goes on every boundary whatever the clock says.
#[test]
fn a_new_round_drops_the_panes_lift_without_a_clock() {
    const THOUGHT: &str = "THINK-WAKE the round that is over";
    let mut a = a_live_thought(THOUGHT);
    let (x, y) = find(&a.screen(100, 30), "thinking line");
    a.key(Key::Click { x: x + 1, y });
    assert_eq!(
        a.lift,
        Some(Show::Thinking),
        "the premise: the click lifted it"
    );
    a.apply(ServerFrame::Event(env_at(
        3,
        1_200,
        SessionEvent::TurnStarted {
            turn_id: "r2".into(),
            model: "qwen3-next-80b".into(),
            ledger_head: "0000".into(),
            // **No clock**: a turn the daemon ran without stamping it.
            began_ms: None,
        },
    )));
    assert_eq!(a.lift, None, "the pane's lift followed into the next round");
}

/// **The pane's own marker opens a PANEL, not a ctrl-v window** — `payload_sel` names both, and
/// `run_open_at` answers *is this row of the open run*, which for the live tail's sentinel is no row
/// at all. Answering it with the row-independent `live.work() > 0 && !live_tail_covered(..)` made
/// every row of the transcript read as inside the open run; answering it with `false` (the right
/// answer for a row) made `run_is_open()` false for the pane's own panel, so the bar offered
/// `page the result` for a window nobody draws. `App::run_is_open` answers the sentinel itself.
#[test]
fn the_live_markers_panel_is_a_run_and_not_a_window() {
    const THOUGHT: &str = "THINK-PANEL the round's thinking";
    let mut a = a_live_thought(THOUGHT);
    let (x, y) = find(&a.screen(100, 30), "thinking line");
    a.key(Key::Click { x: x + 1, y });
    assert!(
        a.run_is_open(),
        "the panel on the pane's own marker does not read as a run"
    );
    // **One sentinel for one panel**: what the click stored is what the chord addresses.
    assert_eq!(
        a.payload_sel.as_deref(),
        a.newest_openable().as_deref(),
        "a click and `ctrl-t` address the live tail by different strings"
    );
    assert!(
        !a.payload_window_open(),
        "and it reads as a ctrl-v window, which it is not"
    );
    let open = frame(&mut a);
    assert!(
        open.contains("esc folds the run back"),
        "the exit is not named:\n{open}"
    );
    assert!(
        !open.contains("page the result"),
        "a window's keys for a window that is not there:\n{open}"
    );
}

/// **A click on a different run's counter does not leave the first one latched** — one panel, one
/// kind, one run: the lift names the run it was thrown on, and the run before it is drawn as the
/// rung draws it.
#[test]
fn a_click_on_another_runs_counter_does_not_leave_the_first_open() {
    const FIRST: &str = "THINK-FIRST the earlier run's thinking";
    const SECOND: &str = "THINK-SECOND the later run's thinking";
    let mut a = app();
    a.visibility = Visibility::of(Profile::READ_EDITS);
    a.apply(hello(
        "s",
        vec![brief("s", "one", true)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(env(
        1,
        testing::appended("s.u1", "user"),
    )));
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::TranscriptContent {
            item_id: "s.u1".into(),
            item: Box::new(operator_row("first question")),
        },
    )));
    shell_row(
        &mut a,
        3,
        "s.r1",
        "reasoning",
        TranscriptItem::Reasoning {
            text: FIRST.into(),
            field: letibot_transcript::ReasoningField::ReasoningContent,
            truncated: false,
        },
    );
    // A drawn row between them ends the first run, so the second is a run of its own.
    shell_row(
        &mut a,
        5,
        "s.a1",
        "assistant",
        TranscriptItem::Assistant {
            text: "the first answer".into(),
            tool_calls: Vec::new(),
            truncated: false,
        },
    );
    shell_row(
        &mut a,
        7,
        "s.r2",
        "reasoning",
        TranscriptItem::Reasoning {
            text: SECOND.into(),
            field: letibot_transcript::ReasoningField::ReasoningContent,
            truncated: false,
        },
    );
    // Both clauses are on the screen as markers; click the FIRST one.
    let screen = a.screen(100, 30);
    let (x, y) = find(&screen, "thinking line");
    a.key(Key::Click { x: x + 1, y });
    assert!(frame(&mut a).contains(FIRST), "the first run opened");
    assert_eq!(a.lift, Some(Show::Thinking));
    // And now a click on the SECOND run's thinking clause — the lower of the two markers, which
    // is why the locator is `find_last`: both say `1 thinking line`, and the first run's header
    // stays on the screen now that it is open.
    let screen = a.screen(100, 30);
    let (x, y) = find_last(&screen, "thinking line");
    a.key(Key::Click { x: x + 1, y });
    let now = frame(&mut a);
    assert!(
        !now.contains(FIRST),
        "the first run is still latched open:\n{now}"
    );
    assert!(now.contains(SECOND), "the second run is open:\n{now}");
}

/// **A click does not follow the operator into the next turn** — the latch, as the operator
/// reported it: *"then all subsequent turns go with think open"*.
///
/// The click opens the pane's thinking; the round's rows land and the panel follows them
/// (`carry_live_open`, which moves the ADDRESS and never the value); and a new TURN — a prompt of
/// the operator's — starts at the default. Before this, `live_open` was re-applied at every round's
/// end and nothing but a click could clear it, so every later turn's pane was drawn open.
#[test]
fn a_click_does_not_follow_into_the_next_turn() {
    const FIRST: &str = "THINK-FIRST the turn the reader clicked";
    const SECOND: &str = "THINK-SECOND the turn after it";
    let mut a = a_live_thought(FIRST);
    let (x, y) = find(&a.screen(100, 30), "thinking line");
    a.key(Key::Click { x: x + 1, y });
    assert!(
        frame(&mut a).contains(FIRST),
        "the premise: the click opened it"
    );
    // The round's thinking lands as a row and the turn ends: the panel follows the rows it became.
    shell_row(
        &mut a,
        3,
        "s.r1",
        "reasoning",
        TranscriptItem::Reasoning {
            text: FIRST.into(),
            field: letibot_transcript::ReasoningField::ReasoningContent,
            truncated: false,
        },
    );
    a.apply(ServerFrame::Event(env_at(
        5,
        1_400,
        finished_costing("r1", None),
    )));
    assert_eq!(
        a.lift,
        Some(Show::Thinking),
        "the panel is still open on the rows the work became"
    );
    // **The next turn.** No key, no click: the operator typed a prompt.
    a.apply(ServerFrame::Event(env_at(
        6,
        2_000,
        SessionEvent::TurnStarted {
            turn_id: "r2".into(),
            model: "qwen3-next-80b".into(),
            ledger_head: "0000".into(),
            began_ms: Some(2_000),
        },
    )));
    assert_eq!(a.lift, None, "the click followed into the next turn");
    a.apply(ServerFrame::Event(env_at(
        7,
        2_100,
        testing::reasoning("r2", SECOND),
    )));
    let now = frame(&mut a);
    assert!(
        !now.contains(SECOND),
        "the next turn's thinking is drawn open:\n{now}"
    );
    assert!(
        now.contains("thinking line"),
        "and its marker is drawn:\n{now}"
    );
}

/// **A round boundary does not touch the thinking** — the operator's *"it is just auto opens now and
/// then closes. then next turn and it opens and then closes"*.
///
/// Two rounds of one turn, no input between them: the thinking is drawn the same way in both —
/// folded, behind the marker. The oscillation was the lift's pane half, re-applied at each round's
/// end; the fold itself is never written by a boundary.
#[test]
fn a_round_boundary_does_not_touch_the_thinking() {
    const ONE: &str = "THINK-ONE the first round";
    const TWO: &str = "THINK-TWO the second round of the same turn";
    let mut a = a_live_thought(ONE);
    let first = frame(&mut a);
    assert!(!first.contains(ONE), "round one draws it folded:\n{first}");
    assert_eq!(
        a.reasoning,
        Fold::Folded,
        "and the fold is where it started"
    );
    // The round's thinking lands, the turn continues: a second round of the SAME turn.
    shell_row(
        &mut a,
        3,
        "s.r1",
        "reasoning",
        TranscriptItem::Reasoning {
            text: ONE.into(),
            field: letibot_transcript::ReasoningField::ReasoningContent,
            truncated: false,
        },
    );
    a.apply(ServerFrame::Event(env_at(
        5,
        1_400,
        finished_costing("r1", None),
    )));
    a.apply(ServerFrame::Event(env_at(
        6,
        1_500,
        SessionEvent::TurnStarted {
            turn_id: "r2".into(),
            model: "qwen3-next-80b".into(),
            ledger_head: "0000".into(),
            began_ms: Some(1_000),
        },
    )));
    a.apply(ServerFrame::Event(env_at(
        7,
        1_600,
        testing::reasoning("r2", TWO),
    )));
    let second = frame(&mut a);
    assert!(
        !second.contains(TWO),
        "round two draws it open, having drawn round one folded:\n{second}"
    );
    assert_eq!(a.reasoning, Fold::Folded, "and no boundary moved the fold");
    assert_eq!(a.lift, None, "and no boundary threw the switch");
}

/// **A frame changes nothing** — three draws with no input are the same lines, and a click that has
/// been made is the same state on every frame after it. A state re-derived or re-applied at DRAW
/// time is the shape that made one session latch and another oscillate.
#[test]
fn frames_with_no_input_do_not_move_the_switch() {
    const THOUGHT: &str = "THINK-STEADY the round's thinking";
    let mut a = a_live_thought(THOUGHT);
    let closed = [frame(&mut a), frame(&mut a), frame(&mut a)];
    assert_eq!(closed[0], closed[1], "two frames of one state differ");
    assert_eq!(closed[1], closed[2], "two frames of one state differ");

    // One click, and the state it sets is the same on every frame drawn after it.
    let (x, y) = find(&a.screen(100, 30), "thinking line");
    a.key(Key::Click { x: x + 1, y });
    let open = [frame(&mut a), frame(&mut a), frame(&mut a)];
    assert_eq!(open[0], open[1], "the click is re-applied per frame");
    assert_eq!(open[1], open[2], "the click is re-applied per frame");
    assert!(open[0].contains(THOUGHT), "the click opened nothing");
    assert_ne!(closed[0], open[0], "the click changed nothing at all");
}

/// **`ctrl-r` above the bottom rung is still the fold** — `terse` draws the reasoning rows (folded)
/// and the ladder can draw them open, so the chord must move the fold there and not look for a run
/// to lift. The two spellings are chosen by what the rung DRAWS, and `terse` draws the thinking.
#[test]
fn ctrl_r_at_terse_still_moves_the_fold() {
    const THOUGHT: &str = "THINK-TERSE a thought with a row of its own";
    let mut a = app();
    a.visibility = Visibility::of(Profile::TERSE);
    a.apply(hello(
        "s",
        vec![brief("s", "one", true)],
        Hub::new("s").snapshot(),
    ));
    shell_row(
        &mut a,
        1,
        "s.r1",
        "reasoning",
        TranscriptItem::Reasoning {
            text: THOUGHT.into(),
            field: letibot_transcript::ReasoningField::ReasoningContent,
            truncated: false,
        },
    );
    assert!(!frame(&mut a).contains(THOUGHT), "terse draws it folded");
    a.key(Key::CtrlR);
    assert_eq!(a.reasoning, Fold::Open, "ctrl-r did not move the fold");
    assert!(a.thinking_open());
    assert!(frame(&mut a).contains(THOUGHT), "and it is drawn open");
    a.key(Key::CtrlR);
    assert_eq!(a.reasoning, Fold::Folded, "ctrl-r did not move it back");
    assert!(!frame(&mut a).contains(THOUGHT));
}

/// **`esc` returns the thinking to the default from the chord's spelling too** — a landed thought,
/// the fold opened with `ctrl-r`, and one Esc.
#[test]
fn esc_returns_the_fold_to_its_default() {
    const THOUGHT: &str = "THINK-LANDED a thought with a row of its own";
    let mut a = app();
    a.visibility = Visibility::of(Profile::NORMAL);
    a.apply(hello(
        "s",
        vec![brief("s", "one", true)],
        Hub::new("s").snapshot(),
    ));
    shell_row(
        &mut a,
        1,
        "s.r1",
        "reasoning",
        TranscriptItem::Reasoning {
            text: THOUGHT.into(),
            field: letibot_transcript::ReasoningField::ReasoningContent,
            truncated: false,
        },
    );
    assert!(!frame(&mut a).contains(THOUGHT), "it starts folded");
    a.key(Key::CtrlR);
    assert!(a.thinking_open(), "ctrl-r opened it");
    assert!(frame(&mut a).contains(THOUGHT), "and it is drawn");
    a.key(Key::Esc);
    assert!(!a.thinking_open(), "esc left it open");
    assert!(
        !frame(&mut a).contains(THOUGHT),
        "esc did not return it to the default:\n{}",
        frame(&mut a)
    );
    assert_eq!(a.reasoning, Fold::Folded, "and the fold itself is closed");
}
