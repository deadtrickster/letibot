//! `FetchRows`: whole rows by ordinal, for a head that holds a window and scrolls above it.
//! `FetchRow` answers a window inside one row's body; this answers the rows themselves, which is
//! what a reader going up the transcript needs.

use std::os::unix::net::UnixStream;
use std::sync::Arc;

use letibot_sessionlog::ViewBounds;
use letibot_sessionlog::event::SessionEvent;
use letibot_sessionlog::protocol::{Caps, ClientFrame, PROTOCOL_VERSION, ServerFrame};
use letibot_sessionlog::registry::{Registry, RowSource, SessionWiring};
use letibot_sessionlog::server::serve_conn;
use letibot_sessionlog::view::SnapshotItem;
use letibot_sessionlog::wire::{FrameReader, FrameWriter};
use letibot_transcript::{ToolOutcome, TranscriptItem};

/// A row whose body is `body`, announced and then filled the way the daemon does it.
fn a_row_with_a_body(
    registry: &Arc<Registry>,
    session: &str,
    item_id: &str,
    body: &str,
) -> SnapshotItem {
    let hub = registry
        .get(session)
        .expect("the session this fixture made");
    hub.publish(SessionEvent::TranscriptAppended {
        item_id: item_id.into(),
        kind: "tool_result".into(),
        ledger_head: "0000".into(),
        item: None,
        body_bytes: 0,
    });
    let item = TranscriptItem::ToolResult {
        call_id: format!("c-{item_id}"),
        name: "read".into(),
        outcome: ToolOutcome::Ok,
        payload: body.into(),
        edit: None,
        origin: None,
        media: None,
    };
    hub.record_item(item_id, item.clone());
    SnapshotItem {
        item_id: item_id.into(),
        kind: "tool_result".into(),
        ledger_head: String::new(),
        ts: 0,
        item: Some(item),
        citations: Vec::new(),
    }
}

/// A store, as far as a page of rows is concerned: the rows the daemon's view no longer holds.
struct Stored(Vec<SnapshotItem>);

impl RowSource for Stored {
    fn row_body(&self, _session_id: &str, row: usize) -> Option<String> {
        self.0
            .get(row)
            .and_then(|r| r.item.as_ref())
            .map(letibot_sessionlog::body_of)
    }

    fn row_items(
        &self,
        _session_id: &str,
        from: usize,
        count: usize,
    ) -> Vec<(usize, SnapshotItem)> {
        self.0
            .iter()
            .enumerate()
            .skip(from)
            .take(count)
            .map(|(at, r)| (at, r.clone()))
            .collect()
    }
}

fn attach(registry: &Arc<Registry>) -> (FrameWriter<UnixStream>, FrameReader<UnixStream>) {
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

fn start() -> Arc<Registry> {
    let registry = Registry::new();
    registry
        .create("s-1", "one", SessionWiring::default())
        .expect("create");
    registry
}

/// Ask for a page, skipping the session's own live events on the way: they share the stream.
fn page(
    w: &mut FrameWriter<UnixStream>,
    r: &mut FrameReader<UnixStream>,
    from: usize,
    count: usize,
) -> (usize, Vec<SnapshotItem>) {
    w.write(&ClientFrame::FetchRows {
        session_id: "s-1".into(),
        from,
        count,
    })
    .expect("fetch rows");
    loop {
        match r.read::<ServerFrame>().expect("page") {
            ServerFrame::RowsFetched { from, rows, .. } => return (from, rows),
            ServerFrame::Event(_) => continue,
            other => panic!("expected RowsFetched, got {other:?}"),
        }
    }
}

/// The page a reader scrolling up needs: the rows themselves, in ordinal order, with the ordinal
/// echoed so the head places them without counting.
#[test]
fn a_page_of_rows_comes_back_by_ordinal() {
    let registry = start();
    for i in 0..4 {
        a_row_with_a_body(&registry, "s-1", &format!("i{i}"), &format!("body {i}"));
    }
    let (mut w, mut r) = attach(&registry);

    let (from, rows) = page(&mut w, &mut r, 1, 2);
    assert_eq!(from, 1, "the ordinal is echoed");
    let ids: Vec<&str> = rows.iter().map(|r| r.item_id.as_str()).collect();
    assert_eq!(ids, ["i1", "i2"], "the page is the ordinals asked for");
    assert!(
        rows.iter().all(|r| r.item.is_some()),
        "a row comes back with its body: the head draws it, it does not fetch it again"
    );
}

/// A page that reaches the beginning stops there rather than padding, and a page past the end is
/// empty. A head reads a short page as *nothing above*, so a padded one would put rows out of
/// place.
#[test]
fn a_page_that_reaches_the_beginning_is_short_and_not_padded() {
    let registry = start();
    for i in 0..3 {
        a_row_with_a_body(&registry, "s-1", &format!("i{i}"), "body");
    }
    let (mut w, mut r) = attach(&registry);

    let (_, rows) = page(&mut w, &mut r, 2, 10);
    assert_eq!(rows.len(), 1, "one row is left above nothing");

    let (_, rows) = page(&mut w, &mut r, 9, 10);
    assert!(rows.is_empty(), "past the end there is no page");
}

/// The case the frame exists for on a long session: the view trims its oldest rows, and the page
/// a head asks for starts below them. The store answers, so the ordinal does not slide.
#[test]
fn a_row_the_view_trimmed_comes_from_the_store() {
    let registry = Registry::with_bounds(
        letibot_sessionlog::LogBounds::default(),
        ViewBounds {
            items: 2,
            ..ViewBounds::default()
        },
    );
    registry
        .create("s-1", "one", SessionWiring::default())
        .expect("create");
    let all: Vec<SnapshotItem> = (0..4)
        .map(|i| a_row_with_a_body(&registry, "s-1", &format!("i{i}"), &format!("body {i}")))
        .collect();
    let hub = registry.get("s-1").expect("the session");
    assert_eq!(hub.snapshot().items_dropped, 2, "the fixture did not trim");
    registry.set_row_source(Arc::new(Stored(all)));

    let (mut w, mut r) = attach(&registry);
    let (from, rows) = page(&mut w, &mut r, 0, 2);
    assert_eq!(from, 0);
    let ids: Vec<&str> = rows.iter().map(|r| r.item_id.as_str()).collect();
    assert_eq!(
        ids,
        ["i0", "i1"],
        "the trimmed ordinals resolved to the rows they name"
    );
    // And a page that starts inside the view is answered from the view, so the two do not
    // disagree about where a row is.
    let (_, rows) = page(&mut w, &mut r, 2, 2);
    let ids: Vec<&str> = rows.iter().map(|r| r.item_id.as_str()).collect();
    assert_eq!(ids, ["i2", "i3"]);
}

/// **A page is bounded in bytes as well as in rows.** `MAX_FETCH_ROWS` alone let four rows of a
/// megabyte each into one frame, and the memory a frame is built in is the daemon's.
#[test]
fn a_page_is_bounded_in_bytes_and_keeps_the_rows_nearest_the_window() {
    let registry = start();
    let big = "x".repeat(1024 * 1024);
    for i in 0..4 {
        a_row_with_a_body(&registry, "s-1", &format!("i{i}"), &big);
    }
    let (mut w, mut r) = attach(&registry);

    let (from, rows) = page(&mut w, &mut r, 0, 4);
    let bytes: usize = rows
        .iter()
        .map(|row| row.item.as_ref().map_or(0, |i| i.bytes()))
        .sum();
    assert!(
        bytes <= letibot_sessionlog::server::MAX_FETCH_BYTES,
        "{} bytes of row text in one page, over the {} byte cap",
        bytes,
        letibot_sessionlog::server::MAX_FETCH_BYTES
    );
    // And what it gave up is its OLDEST rows: a page that kept its head would no longer join the
    // window the reader asked above.
    assert_eq!(
        rows.len(),
        1,
        "the fixture's page is not one row: {}",
        rows.len()
    );
    assert_eq!(from, 3, "the page does not say which ordinal it starts at");
    assert_eq!(rows[0].item_id, "i3");
}

/// **A page says the ordinal it really starts at.** A source that cannot hand over the first row it
/// was asked for answers from a later one, and echoing the request labels every row of the page one
/// place to the left.
#[test]
fn a_page_that_starts_late_names_the_row_it_starts_at() {
    struct Late(Vec<SnapshotItem>);

    impl RowSource for Late {
        fn row_body(&self, _session_id: &str, row: usize) -> Option<String> {
            self.0
                .get(row)
                .and_then(|r| r.item.as_ref())
                .map(letibot_sessionlog::body_of)
        }

        /// The first row is one this source cannot read, so it is left out and the rest stand.
        fn row_items(
            &self,
            _session_id: &str,
            from: usize,
            count: usize,
        ) -> Vec<(usize, SnapshotItem)> {
            self.0
                .iter()
                .enumerate()
                .skip(from)
                .take(count)
                .filter(|(at, _)| *at != 0)
                .map(|(at, r)| (at, r.clone()))
                .collect()
        }
    }

    let registry = Registry::with_bounds(
        letibot_sessionlog::LogBounds::default(),
        ViewBounds {
            items: 0,
            ..ViewBounds::default()
        },
    );
    registry
        .create("s-1", "one", SessionWiring::default())
        .expect("create");
    let all: Vec<SnapshotItem> = (0..3)
        .map(|i| SnapshotItem {
            item_id: format!("i{i}"),
            kind: "user".into(),
            ledger_head: String::new(),
            ts: 0,
            item: Some(TranscriptItem::User {
                speaker: Default::default(),
                parts: vec![letibot_transcript::UserPart::Text {
                    text: format!("body {i}"),
                }],
            }),
            citations: Vec::new(),
        })
        .collect();
    registry.set_row_source(Arc::new(Late(all)));

    let (mut w, mut r) = attach(&registry);
    let (from, rows) = page(&mut w, &mut r, 0, 3);
    let ids: Vec<&str> = rows.iter().map(|row| row.item_id.as_str()).collect();
    assert_eq!(ids, ["i1", "i2"], "the page is the rows the source had");
    assert_eq!(
        from, 1,
        "the page was labelled from the request, not from the row it starts at"
    );
}

/// A session the daemon does not hold is refused by name, the rule `FetchRow` follows: an empty
/// page and a missing session must not look alike.
#[test]
fn a_session_that_is_not_held_is_refused_by_name() {
    let registry = start();
    a_row_with_a_body(&registry, "s-1", "i0", "body");
    let (mut w, mut r) = attach(&registry);

    w.write(&ClientFrame::FetchRows {
        session_id: "no-such-session".into(),
        from: 0,
        count: 4,
    })
    .expect("fetch rows");
    match r.read::<ServerFrame>().expect("answer") {
        ServerFrame::Rejected { reason, .. } => {
            assert!(reason.contains("no-such-session"), "{reason}");
        }
        other => panic!("expected Rejected, got {other:?}"),
    }
}
