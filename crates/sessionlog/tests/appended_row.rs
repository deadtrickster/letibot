//! **The body on the announcement.** `TranscriptAppended` named a row and carried nothing, so a head
//! waited for a second event before it could draw a row it had been told about, and a log of those
//! events could not be read back as a conversation. A row that fits now rides its own announcement;
//! a row too large for one still arrives whole, as `TranscriptContent`.

use std::os::unix::net::UnixStream;
use std::sync::Arc;

use letibot_sessionlog::event::{ANNOUNCE_BYTES, SessionEvent};
use letibot_sessionlog::protocol::{Caps, ClientFrame, PROTOCOL_VERSION, ServerFrame};
use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_sessionlog::server::serve_conn;
use letibot_sessionlog::wire::{FrameReader, FrameWriter};
use letibot_sessionlog::{Envelope, Hub};
use letibot_transcript::TranscriptItem;

fn start() -> Arc<Registry> {
    let registry = Registry::new();
    registry
        .create("s-1", "one", SessionWiring::default())
        .expect("create");
    registry
}

fn hub(registry: &Arc<Registry>) -> Arc<Hub> {
    registry.get("s-1").expect("the session this fixture made")
}

fn assistant(text: &str) -> TranscriptItem {
    TranscriptItem::Assistant {
        text: text.into(),
        tool_calls: Vec::new(),
        truncated: false,
    }
}

/// A row, appended the way the daemon does it: the announcement carries the body, and the pairing
/// that follows it is `Hub::record_item`.
fn append(registry: &Arc<Registry>, item_id: &str, item: &TranscriptItem) {
    hub(registry).append_row(item_id, "assistant", "0000", item);
}

/// Attach a head that says it draws a row from the announcement, with the caps it declared.
fn attach(
    registry: &Arc<Registry>,
    caps: Caps,
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
        caps,
    })
    .expect("attach");
    assert!(matches!(
        r.read::<ServerFrame>().expect("hello"),
        ServerFrame::Hello { .. }
    ));
    (w, r)
}

fn reading_bodies() -> Caps {
    Caps {
        row_bodies: true,
        ..Caps::default()
    }
}

/// **A marker says when the events of interest are over**: the daemon answers in order, so the frame
/// after the marker's is the next thing that happened, and nothing sent after the marker can hide
/// in front of it. An absence assertion that stopped anywhere else would have no teeth.
fn marker(registry: &Arc<Registry>, said: &str) {
    hub(registry).publish(SessionEvent::Warning {
        code: "marker".into(),
        detail: said.into(),
        compaction: None,
    });
}

/// Everything the head was sent up to the marker, as events.
fn until_marker(r: &mut FrameReader<UnixStream>, said: &str) -> Vec<Envelope> {
    let mut out = Vec::new();
    loop {
        match r.read::<ServerFrame>().expect("a frame") {
            ServerFrame::Event(env) => {
                let done =
                    matches!(&env.event, SessionEvent::Warning { detail, .. } if detail == said);
                if done {
                    return out;
                }
                out.push(env);
            }
            other => panic!("expected an event, got {other:?}"),
        }
    }
}

fn appended(events: &[Envelope]) -> Vec<&SessionEvent> {
    events
        .iter()
        .filter(|e| matches!(e.event, SessionEvent::TranscriptAppended { .. }))
        .map(|e| &e.event)
        .collect()
}

fn contents(events: &[Envelope]) -> Vec<&SessionEvent> {
    events
        .iter()
        .filter(|e| matches!(e.event, SessionEvent::TranscriptContent { .. }))
        .map(|e| &e.event)
        .collect()
}

/// A row that fits travels on its own announcement, and a head that said it reads one is not sent
/// the same words again.
#[test]
fn a_row_that_fits_rides_its_own_announcement() {
    let registry = start();
    let (mut _w, mut r) = attach(&registry, reading_bodies());
    append(
        &registry,
        "s-1.0",
        &assistant("the parser is in crates/turn"),
    );
    marker(&registry, "after one row");

    let events = until_marker(&mut r, "after one row");
    let rows = appended(&events);
    assert_eq!(rows.len(), 1, "{events:?}");
    let SessionEvent::TranscriptAppended { item_id, item, .. } = rows[0] else {
        unreachable!()
    };
    assert_eq!(item_id, "s-1.0");
    let Some(item) = item else {
        panic!("the announcement carried no body: {events:?}")
    };
    assert!(
        matches!(&**item, TranscriptItem::Assistant { text, .. } if text == "the parser is in crates/turn"),
        "{item:?}"
    );
    assert!(
        contents(&events).is_empty(),
        "the head was told the same row twice: {events:?}"
    );
}

/// A row larger than an announcement may carry still arrives whole, and the announcement says how
/// large it was so the row is not read as lost.
#[test]
fn a_row_too_large_for_an_announcement_is_still_delivered() {
    let registry = start();
    let (mut _w, mut r) = attach(&registry, reading_bodies());
    let long = "x".repeat(ANNOUNCE_BYTES + 1);
    append(&registry, "s-1.0", &assistant(&long));
    marker(&registry, "after one row");

    let events = until_marker(&mut r, "after one row");
    let rows = appended(&events);
    assert_eq!(rows.len(), 1, "{events:?}");
    let SessionEvent::TranscriptAppended { item, .. } = rows[0] else {
        unreachable!()
    };
    assert!(
        item.is_none(),
        "a body over the bound rode the announcement anyway"
    );
    let contents = contents(&events);
    assert_eq!(
        contents.len(),
        1,
        "the whole row must still arrive: {events:?}"
    );
    let SessionEvent::TranscriptContent { item, .. } = contents[0] else {
        unreachable!()
    };
    assert!(
        matches!(&**item, TranscriptItem::Assistant { text, .. } if text.len() == long.len()),
        "the body was cut somewhere between the two events"
    );
}

/// A head that reads no body off an announcement - every head built before the field existed -
/// is still sent the second event, whatever the log carries for the heads that do read one.
#[test]
fn a_head_that_reads_no_body_off_an_announcement_is_still_told() {
    let registry = start();
    let (mut _w, mut r) = attach(&registry, Caps::default());
    append(&registry, "s-1.0", &assistant("a short answer"));
    marker(&registry, "after one row");

    let events = until_marker(&mut r, "after one row");
    assert_eq!(appended(&events).len(), 1, "{events:?}");
    let contents = contents(&events);
    assert_eq!(
        contents.len(),
        1,
        "an older head must still be handed the body: {events:?}"
    );
    let SessionEvent::TranscriptContent { item, .. } = contents[0] else {
        unreachable!()
    };
    assert!(
        matches!(&**item, TranscriptItem::Assistant { text, .. } if text == "a short answer"),
        "{item:?}"
    );
}

/// An announcement written before the field existed reads as one that carried nothing, and the
/// head that receives it waits for the body the way it always did.
#[test]
fn an_announcement_from_an_older_daemon_still_reads() {
    let line =
        r#"{"event":"transcript_appended","item_id":"s.3","kind":"assistant","ledger_head":"ab"}"#;
    let event: SessionEvent = serde_json::from_str(line).expect("an older daemon's row");
    let SessionEvent::TranscriptAppended {
        item_id,
        item,
        body_bytes,
        ..
    } = event
    else {
        panic!("wrong variant")
    };
    assert_eq!(item_id, "s.3");
    assert!(item.is_none(), "a row that carried no body cannot say one");
    assert_eq!(body_bytes, 0, "nothing said how large it was either");
}
