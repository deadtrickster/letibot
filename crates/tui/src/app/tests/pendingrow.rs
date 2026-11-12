//! A result row announced without its body: the row draws one line and never zero, and the call
//! stops being the live pane's at that line - not when the body lands. A row the window let go is
//! still this turn's, so the page that brings it back takes the card with it.

use super::*;

/// The daemon's announcement of a tool result too large to ride it: the id, the kind, and how many
/// bytes are on their way in a `TranscriptContent` of their own.
fn announced_result(id: &str, bytes: u64) -> SessionEvent {
    SessionEvent::TranscriptAppended {
        item_id: id.into(),
        kind: "tool_result".into(),
        ledger_head: "beef".into(),
        item: None,
        body_bytes: bytes,
    }
}

/// A turn one call deep: the call finished, its result row announced, and its body still in the
/// air. That is every payload over `ANNOUNCE_BYTES`, which is most of them.
fn a_result_without_its_body(bytes: u64) -> App {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(env_at(
        1,
        1_000,
        testing::turn_started("t1"),
    )));
    a.apply(ServerFrame::Event(env_at(
        2,
        1_100,
        testing::proposed("t1", "c0", "bash"),
    )));
    a.apply(ServerFrame::Event(env_at(
        3,
        1_200,
        SessionEvent::ToolStarted {
            turn_id: "t1".into(),
            call_id: "c0".into(),
            name: "bash".into(),
            access: Default::default(),
        },
    )));
    a.apply(ServerFrame::Event(env_at(
        4,
        1_900,
        SessionEvent::ToolFinished {
            turn_id: "t1".into(),
            call_id: "c0".into(),
            outcome: letibot_transcript::ToolOutcome::Ok,
            payload_digest: "fnv1a:9".into(),
            inline_bytes: bytes,
            full_bytes: bytes,
            spill: None,
            repairs: 0,
            edits: Vec::new(),
        },
    )));
    a.apply(ServerFrame::Event(env(5, announced_result("s.0", bytes))));
    a.invalidate_history();
    a
}

/// A 9 KiB result arrives bodyless, and the row draws its own header while it waits: one line
/// naming the call and what is still coming, never zero lines - and once, not twice.
#[test]
fn a_result_row_without_its_body_draws_its_header_and_what_is_coming() {
    let mut a = a_result_without_its_body(9 * 1024);
    let frame = a.screen(100, 30);
    let drawn = frame.join("\n");
    assert!(
        drawn.contains("9.0 KB, loading"),
        "the row did not say what it was waiting for:\n{drawn}"
    );
    assert!(
        drawn.contains("Ran"),
        "the row did not draw its own header:\n{drawn}"
    );
    // One line for the row and none for a card: the call is the transcript's from the announcement.
    let said: Vec<&String> = frame.iter().filter(|l| l.contains("loading")).collect();
    assert_eq!(
        said.len(),
        1,
        "the row drew {} placeholder lines:\n{drawn}",
        said.len()
    );
    assert_eq!(
        a.turn.as_ref().expect("the turn").settled_calls,
        1,
        "the live pane still holds a call its row is drawing\n{drawn}"
    );

    // And the body landing replaces that line with the row itself, once.
    a.apply(ServerFrame::Event(env(
        6,
        SessionEvent::TranscriptContent {
            item_id: "s.0".into(),
            item: Box::new(TranscriptItem::ToolResult {
                call_id: "c0".into(),
                name: "bash".into(),
                outcome: letibot_transcript::ToolOutcome::Ok,
                payload: "ran the whole suite\n".into(),
                edits: Vec::new(),
                origin: None,
                media: None,
            }),
        },
    )));
    let after = a.screen(100, 30).join("\n");
    assert!(
        after.contains("ran the whole suite"),
        "the body did not replace the placeholder:\n{after}"
    );
    assert!(
        !after.contains("loading"),
        "the placeholder outlived its body:\n{after}"
    );

    // And a placeholder the window lets go of does not outlive its row.
    assert!(
        a.pending_rows.is_empty(),
        "a row with a body kept its placeholder: {:?}",
        a.pending_rows.keys().collect::<Vec<_>>()
    );
}

/// A row that arrives while this head holds a window and the session went on below it: counted,
/// fetched back a page later, and drawn there - with its card stood down, since the row is the
/// call's now.
#[test]
fn a_row_below_the_window_is_fetched_back_and_drawn_in_the_same_turn() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(env_at(
        1,
        1_000,
        testing::turn_started("t1"),
    )));
    a.apply(ServerFrame::Event(env_at(
        2,
        1_100,
        testing::proposed("t1", "c0", "bash"),
    )));
    a.apply(ServerFrame::Event(env_at(
        3,
        1_200,
        SessionEvent::ToolStarted {
            turn_id: "t1".into(),
            call_id: "c0".into(),
            name: "bash".into(),
            access: Default::default(),
        },
    )));
    a.apply(ServerFrame::Event(env_at(
        4,
        1_900,
        SessionEvent::ToolFinished {
            turn_id: "t1".into(),
            call_id: "c0".into(),
            outcome: letibot_transcript::ToolOutcome::Ok,
            payload_digest: "fnv1a:9".into(),
            inline_bytes: 9 * 1024,
            full_bytes: 9 * 1024,
            spill: None,
            repairs: 0,
            edits: Vec::new(),
        },
    )));
    // The reader is up in the history and the session went on below them.
    a.items_below = 2;
    a.apply(ServerFrame::Event(env(
        5,
        announced_result("s.1", 9 * 1024),
    )));
    assert_eq!(a.items_below, 3, "the row below the window was not counted");
    assert!(
        !a.items.iter().any(|r| r.item_id == "s.1"),
        "a row below the window was spliced into a window that has a hole under it"
    );
    // The turn knows the row anyway: the page that brings it back must hand the call over.
    assert_eq!(
        a.turn
            .as_ref()
            .expect("the turn")
            .appended
            .last()
            .map(String::as_str),
        Some("s.1"),
        "the row below the window was forgotten by the turn that produced it"
    );
    let _ = a.screen(100, 30);
    // Down to the tail: the head asks for the rows it counted, and the daemon answers with them.
    a.hold(100_000);
    let _ = a.screen(100, 30);
    let at = a
        .queued
        .iter()
        .position(|x| matches!(x, Action::FetchRows { .. }))
        .expect("no page below the window was asked for");
    let Action::FetchRows { from, .. } = a.queued.remove(at) else {
        unreachable!("the position was of a FetchRows")
    };
    a.apply(ServerFrame::RowsFetched {
        session_id: "s".into(),
        from,
        rows: vec![SnapshotItem {
            citations: Vec::new(),
            item_id: "s.1".into(),
            kind: "tool_result".into(),
            ledger_head: "beef".into(),
            ts: 2_000,
            item: Some(TranscriptItem::ToolResult {
                call_id: "c0".into(),
                name: "bash".into(),
                outcome: letibot_transcript::ToolOutcome::Ok,
                payload: "the whole suite, green\n".into(),
                edits: Vec::new(),
                origin: None,
                media: None,
            }),
        }],
    });
    let drawn = a.screen(100, 30).join("\n");
    assert!(
        drawn.contains("the whole suite, green"),
        "the row fetched back below the window is not on the screen:\n{drawn}"
    );
    assert_eq!(
        a.turn.as_ref().expect("the turn").settled_calls,
        1,
        "the paged-in row left its card standing:\n{drawn}"
    );
}
