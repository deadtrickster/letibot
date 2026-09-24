//! `FetchRow`: a window of one row's body, read without leaving the session.
//!
//! The operator's framing, which is what this exists for: a tool result is a **logical**
//! string that wraps to thousands of **display** lines, of which a screen shows a few
//! dozen. The head holds a window of the conversation — `ViewBounds` bounds the snapshot
//! by count and by bytes — so an old row is not on it at all, and a row that is can be
//! longer than anything the snapshot would carry.
//!
//! `Peek` is not this. It names a session and returns **all** of it, with no position; that
//! is right for the picker's "what was that session about" and wrong for "the display line
//! 3,000 of this 400 KB payload". What the two share is the property worth keeping: a read
//! that does not move your seat.

use std::os::unix::net::UnixStream;

use letibot_sessionlog::ViewBounds;
use letibot_sessionlog::event::SessionEvent;
use letibot_sessionlog::protocol::{Caps, ClientFrame, PROTOCOL_VERSION, ServerFrame};
use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_sessionlog::server::{MAX_FETCH_ROW, serve_conn};
use letibot_sessionlog::wire::{FrameReader, FrameWriter};
use letibot_transcript::{ToolOutcome, TranscriptItem};

/// A row whose body is `body`, announced and then filled the way the daemon does it.
fn a_row_with_a_body(
    registry: &std::sync::Arc<Registry>,
    session: &str,
    item_id: &str,
    body: &str,
) {
    let hub = registry
        .get(session)
        .expect("the session this fixture made");
    hub.publish(SessionEvent::TranscriptAppended {
        item_id: item_id.into(),
        kind: "tool_result".into(),
        ledger_head: "0000".into(),
    });
    hub.record_item(
        item_id,
        TranscriptItem::ToolResult {
            call_id: "c1".into(),
            name: "read".into(),
            outcome: ToolOutcome::Ok,
            payload: body.into(),
            edit: None,
            origin: None,
        },
    );
}

/// Attach a head to `s-1` and hand back the wire plus the `Hello` it was given.
fn attach(
    registry: &std::sync::Arc<Registry>,
) -> (FrameWriter<UnixStream>, FrameReader<UnixStream>) {
    let (a, b) = UnixStream::pair().expect("pair");
    let reg = registry.clone();
    let _server = std::thread::spawn(move || {
        let _ = serve_conn(reg, a);
    });
    let mut w = FrameWriter::new(b.try_clone().expect("clone"));
    let mut r = FrameReader::new(b);
    w.write(&ClientFrame::Attach {
        protocol_version: PROTOCOL_VERSION,
        session_id: "s-1".into(),
        since_seq: 0,
        kind: "tui".into(),
        identity: "test".into(),
        caps: Caps::default(),
    })
    .expect("attach");
    assert!(matches!(
        r.read::<ServerFrame>().expect("hello"),
        ServerFrame::Hello { .. }
    ));
    (w, r)
}

fn start() -> std::sync::Arc<Registry> {
    let registry = Registry::new();
    registry
        .create("s-1", "one", SessionWiring::default())
        .expect("create");
    registry
}

/// Ask for a window and get it.
///
/// **Skips live events on the way**, which is what a head's pump does: the answer arrives on
/// the same stream as the session's own traffic, so a frame that is not the answer is
/// something else that happened, not a reply. The failing version of this read one frame and
/// assumed it was the answer, which is why a test that published a row *after* attaching
/// failed with `Event(TranscriptAppended …)`.
fn fetch(
    w: &mut FrameWriter<UnixStream>,
    r: &mut FrameReader<UnixStream>,
    row: usize,
    at: usize,
    len: usize,
) -> (Option<String>, usize, usize) {
    w.write(&ClientFrame::FetchRow {
        session_id: "s-1".into(),
        row,
        at,
        len,
    })
    .expect("fetch");
    loop {
        match r.read::<ServerFrame>().expect("row") {
            ServerFrame::RowFetched {
                at, body, total, ..
            } => return (body, total, at),
            // The session's own events, delivered while we were asking.
            ServerFrame::Event(_) => continue,
            other => panic!("expected RowFetched, got {other:?}"),
        }
    }
}

/// **The whole point**: a window of a body, with the length so the head knows what is on
/// either side of it.
#[test]
fn a_window_of_a_row_comes_back_with_the_total() {
    let registry = start();
    let body: String = (0..500).map(|i| format!("line {i}\n")).collect();
    a_row_with_a_body(&registry, "s-1", "i1", &body);
    let (mut w, mut r) = attach(&registry);

    // The head of it.
    let (win, total, at) = fetch(&mut w, &mut r, 0, 0, 40);
    let win = win.expect("a body");
    assert_eq!(at, 0);
    assert_eq!(total, body.len(), "the whole length, not the window's");
    assert_eq!(win, &body[..40]);

    // And a window from the middle, which is what paging asks for. `total` is unchanged,
    // so a head can say "N above, M below" without ever holding the body.
    let (win, total2, at) = fetch(&mut w, &mut r, 0, 400, 40);
    assert_eq!(at, 400);
    assert_eq!(total2, body.len());
    assert_eq!(win.expect("a body"), &body[400..440]);
}

/// **The cap is the daemon's, not the head's request.** A head that asks for a megabyte
/// gets a window, because one request must not be able to make the daemon serialise one.
#[test]
fn a_head_cannot_ask_for_more_than_the_cap() {
    let registry = start();
    let body = "x".repeat(MAX_FETCH_ROW * 3);
    a_row_with_a_body(&registry, "s-1", "big", &body);
    let (mut w, mut r) = attach(&registry);

    let (win, total, _) = fetch(&mut w, &mut r, 0, 0, usize::MAX);
    let win = win.expect("a body");
    assert_eq!(win.len(), MAX_FETCH_ROW, "the daemon's cap did not bind");
    assert_eq!(total, body.len());
}

/// **Asking past the end is how a head finds the end.** It is clamped, not refused, and it
/// comes back empty rather than as an error — with the total, which is the answer.
#[test]
fn asking_past_the_end_is_clamped_and_says_so() {
    let registry = start();
    a_row_with_a_body(&registry, "s-1", "i1", "short body");
    let (mut w, mut r) = attach(&registry);

    let (win, total, at) = fetch(&mut w, &mut r, 0, 9_999, 40);
    assert_eq!(total, "short body".len());
    assert_eq!(at, total, "clamped to the end, not refused");
    assert_eq!(win.expect("a body"), "", "nothing after the end");
}

/// **A window starts on a character boundary.** A head cannot render half a glyph, and the
/// daemon is the side that knows the encoding — so an offset landing inside a multi-byte
/// character is rounded down, and the answer says where it really starts.
#[test]
fn a_window_never_splits_a_character() {
    let registry = start();
    // `日` is three bytes, so offset 1 and 2 are inside it.
    let body = "日本語のテキスト";
    a_row_with_a_body(&registry, "s-1", "i1", body);
    let (mut w, mut r) = attach(&registry);

    for at in [1usize, 2, 4, 5] {
        let (win, total, got) = fetch(&mut w, &mut r, 0, at, 6);
        assert_eq!(total, body.len());
        assert!(
            body.is_char_boundary(got),
            "at {at}: the window starts mid-character ({got})"
        );
        let win = win.expect("a body");
        assert!(
            body[got..].starts_with(&win),
            "at {at}: the window is not a slice of the body"
        );
        // And it is valid UTF-8 by construction — a split character would not deserialise
        // at all, so reaching here is half the proof; this is the other half.
        assert!(std::str::from_utf8(win.as_bytes()).is_ok());
    }
}

/// **The ordinal is the session's position, not the window's index.**
///
/// This is the whole reason the key is an ordinal. A head knows the rows it holds and
/// `items_dropped` says how many came before them, so it can name *any* row by where it sits in
/// the conversation — including ones it never received. The previous version addressed rows by
/// `item_id`, which a head can only name if it was sent it, so the rows this exists for were
/// exactly the ones it could not ask about.
///
/// Here the view has trimmed its first three rows; ordinal 3 must still be the fourth row of the
/// session, and the three before it must answer `None` rather than sliding the numbering.
#[test]
fn an_ordinal_names_window_and_trimmed_rows_alike() {
    // A registry whose view holds three rows, so four rows leave one trimmed. Set at
    // construction because `ViewBounds` is the daemon's, decided once — mutating it per
    // session afterwards would be a second place to configure the same thing.
    let registry = Registry::with_bounds(
        letibot_sessionlog::LogBounds::default(),
        ViewBounds {
            items: 3,
            ..ViewBounds::default()
        },
    );
    let hub = registry
        .create("s-1", "one", SessionWiring::default())
        .expect("create");
    for i in 0..4 {
        a_row_with_a_body(&registry, "s-1", &format!("i{i}"), &format!("body {i}"));
    }
    // The window dropped the first, and says so.
    assert_eq!(hub.snapshot().items_dropped, 1, "the fixture did not trim");

    let (mut w, mut r) = attach(&registry);

    // Ordinal 1 is the SECOND row of the session — the oldest this view still holds.
    let (win, _, _) = fetch(&mut w, &mut r, 1, 0, 40);
    assert_eq!(
        win.as_deref(),
        Some("body 1"),
        "the ordinal slid with the window instead of naming a session position"
    );
    // Ordinal 3 is the last one written.
    let (win, _, _) = fetch(&mut w, &mut r, 3, 0, 40);
    assert_eq!(win.as_deref(), Some("body 3"));

    // And ordinal 0 — the row the window trimmed — is honestly `None`. The store read that
    // would answer it is R19.2(b) in `letibot`'s TODO; what matters here is that it does not
    // silently answer with the WRONG row, which is what a window-relative index would do.
    let (win, _, _) = fetch(&mut w, &mut r, 0, 0, 40);
    assert_eq!(
        win, None,
        "a trimmed ordinal must not resolve to a held row"
    );
}

/// **A row nobody has is not an empty row.** The two must not look alike, or a head
/// reports "the output was empty" about output that was trimmed.
#[test]
fn a_row_that_is_not_held_answers_none() {
    let registry = start();
    a_row_with_a_body(&registry, "s-1", "i1", "here");
    let (mut w, mut r) = attach(&registry);

    let (win, total, _) = fetch(&mut w, &mut r, 99, 0, 40);
    assert_eq!(win, None, "a row the view does not hold must answer None");
    assert_eq!(total, 0);

    // And a row that exists but is EMPTY is `Some("")`, which is a different answer.
    a_row_with_a_body(&registry, "s-1", "empty", "");
    let (win, total, _) = fetch(&mut w, &mut r, 1, 0, 40);
    assert_eq!(win, Some(String::new()));
    assert_eq!(total, 0);
}

/// A session the daemon does not hold is refused **by name**, the same rule `Peek` follows:
/// an empty answer and a missing session must not look alike.
#[test]
fn a_session_that_is_not_held_is_refused_by_name() {
    let registry = start();
    a_row_with_a_body(&registry, "s-1", "i1", "here");
    let (mut w, mut r) = attach(&registry);

    w.write(&ClientFrame::FetchRow {
        session_id: "no-such-session".into(),
        row: 0,
        at: 0,
        len: 40,
    })
    .expect("fetch");
    match r.read::<ServerFrame>().expect("answer") {
        ServerFrame::Rejected { reason, .. } => {
            assert!(reason.contains("no-such-session"), "{reason}");
        }
        other => panic!("expected Rejected, got {other:?}"),
    }
}

/// **It is a read, not a move.** The same contract `Peek` keeps and the reason both sit in
/// the server's ask arm: the seat, the acks and the live events are untouched, so a head
/// reading a row is still in the session it was in.
#[test]
fn fetching_does_not_move_the_connection() {
    let registry = start();
    let hub = registry.get("s-1").expect("the session");
    a_row_with_a_body(&registry, "s-1", "i1", "the body");
    let (mut w, mut r) = attach(&registry);

    let before = hub.head_seq();
    let _ = fetch(&mut w, &mut r, 0, 0, 40);

    // The session has the same head, and this connection is still seated in it: a live
    // event published after the fetch still arrives on this stream.
    assert_eq!(
        hub.head_seq(),
        before,
        "fetching advanced the session's head"
    );
    hub.publish(SessionEvent::TurnStarted {
        turn_id: "t2".into(),
        model: "m".into(),
        ledger_head: "0000".into(),
    });
    match r.read::<ServerFrame>().expect("the live event") {
        ServerFrame::Event(e) => {
            assert_eq!(e.event.kind(), "TurnStarted");
        }
        other => panic!("expected the live event, got {other:?}"),
    }
}
