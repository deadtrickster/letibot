//! An expanded `[N tool calls]` lists every call it counts: the call still running is its last
//! row, drawn live, and the same row takes the result when it lands.

use super::*;

fn start(a: &mut App, seq: u64, n: u64) {
    a.apply(ServerFrame::Event(env(
        seq,
        testing::proposed_on("t1", &format!("c{n}"), "bash", &format!("\"step {n}\"")),
    )));
    a.apply(ServerFrame::Event(env(
        seq + 1,
        SessionEvent::ToolStarted {
            turn_id: "t1".into(),
            call_id: format!("c{n}"),
            name: "bash".into(),
            access: Default::default(),
        },
    )));
}

fn finish(a: &mut App, seq: u64, n: u64) {
    a.apply(ServerFrame::Event(env(
        seq,
        SessionEvent::ToolFinished {
            turn_id: "t1".into(),
            call_id: format!("c{n}"),
            outcome: letibot_transcript::ToolOutcome::Ok,
            payload_digest: "d".into(),
            inline_bytes: 1,
            full_bytes: 1,
            spill: None,
            repairs: 0,
            edits: Vec::new(),
        },
    )));
    shell_row(
        a,
        seq + 1,
        &format!("s.{}", n + 1),
        "tool_result",
        TranscriptItem::ToolResult {
            call_id: format!("c{n}"),
            name: "bash".into(),
            outcome: letibot_transcript::ToolOutcome::Ok,
            payload: format!("RESULT-{n}"),
            edits: Vec::new(),
            origin: None,
            media: None,
            ran_ms: None,
            decision: None,
        },
    );
}

/// Two calls finished and one running, at the rung that folds the working. `prose` is the
/// round's narration: empty, the stretch starts at an invisible assistant row and the marker
/// names its run by the first hidden row instead.
fn two_done_one_running(prose: &str) -> App {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.visibility = Visibility::of(Profile::CONVERSATION);
    shell_row(&mut a, 1, "s.p", "user", operator_row("go"));
    a.apply(ServerFrame::Event(env(3, testing::turn_started("t1"))));
    shell_row(
        &mut a,
        4,
        "s.0",
        "assistant",
        TranscriptItem::Assistant {
            text: prose.into(),
            tool_calls: Vec::new(),
            truncated: false,
        },
    );
    start(&mut a, 10, 0);
    finish(&mut a, 12, 0);
    start(&mut a, 20, 1);
    finish(&mut a, 22, 1);
    start(&mut a, 30, 2);
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

/// The call cards on the screen, top to bottom.
fn cards(screen: &[String]) -> Vec<String> {
    screen
        .iter()
        .filter(|l| l.contains("Ran (") || l.contains("Running "))
        .map(|l| l.trim().to_string())
        .collect()
}

fn expanded_lists_every_call(prose: &str) {
    let mut a = two_done_one_running(prose);
    let folded = a.screen(100, 30);
    let drawn = folded.join("\n");
    assert!(
        drawn.contains("[3 tool calls]"),
        "collapsed count:\n{drawn}"
    );
    assert!(
        cards(&folded).is_empty(),
        "collapsed draws no card:\n{drawn}"
    );
    let (x, y) = find(&folded, "3 tool calls");
    a.key(Key::Click { x: x + 1, y });
    let open = a.screen(100, 30);
    let drawn = open.join("\n");
    assert!(drawn.contains("[3 tool calls]"), "expanded count:\n{drawn}");
    let rows = cards(&open);
    assert_eq!(rows.len(), 3, "three counted, three listed:\n{drawn}");
    assert!(
        rows[2].starts_with("◐ Running \"step 2\""),
        "the running call is the last row, marked:\n{drawn}"
    );
    finish(&mut a, 32, 2);
    let open = a.screen(100, 30);
    let drawn = open.join("\n");
    let rows = cards(&open);
    assert_eq!(rows.len(), 3, "still three rows:\n{drawn}");
    assert!(
        rows[2].contains("Ran (c2)") && rows[2].contains("RESULT-2"),
        "the last row took the result:\n{drawn}"
    );
}

#[test]
fn an_expanded_group_after_prose_lists_the_running_call() {
    expanded_lists_every_call("Working on it:");
}

#[test]
fn an_expanded_group_with_no_prose_lists_the_running_call() {
    expanded_lists_every_call("");
}
