//! A late head attaching mid-turn, over a real socket, against a live producer.
//!
//! This is the case where §13.2b's mechanics either work or do not. Every one of
//! them is reachable from here:
//!
//! - snapshot and register under one lock → **not one byte missing, not one twice**
//! - non-blocking fan-out → the producer's rate does not depend on the head's
//! - bounded queue → a head that stops reading is demoted, and told
//! - idle is quiet → detaching every head mid-turn changes nothing
//! - scrub as projection → a settled decision is never re-asked
//! - ack after render → a head that dies unacked gets duplicates, never a gap
//!
//! The reconstruction assertion is the sharp one: `snapshot.turn.text` plus every
//! `Delta` that follows must equal, **exactly**, what the producer emitted. A gap
//! at the attach boundary loses a byte; an overlap repeats one. *"The missing byte
//! is usually the prompt."*

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use letibot_sessionlog::client::{HeadClient, Inbound, pump};
use letibot_sessionlog::event::{DeltaTarget, SessionEvent};
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::protocol::{Caps, ClientFrame, PROTOCOL_VERSION, PeekShape, ServerFrame};
use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_sessionlog::server::{ServerHandle, serve, serve_conn};
use letibot_sessionlog::testing::*;
use letibot_sessionlog::wire::{FrameReader, FrameWriter};
use std::os::unix::net::UnixStream;

const DELTAS: usize = 2_000;
const END: &str = "END-OF-PRODUCTION";

fn socket_path(tag: &str) -> std::path::PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "letibot-head-{tag}-{}-{n}.sock",
        std::process::id()
    ))
}

fn start(tag: &str) -> (Arc<Hub>, ServerHandle) {
    let hub = Hub::new("s");
    let h = serve(hub.clone(), socket_path(tag)).expect("bind");
    (hub, h)
}

/// The text the producer will have emitted in full.
fn full_text() -> String {
    (0..DELTAS).map(piece).collect()
}

fn piece(i: usize) -> String {
    format!("{i} ")
}

/// Emit `DELTAS` deltas as fast as possible, then a sentinel.
fn produce(hub: Arc<Hub>, go: Arc<Barrier>) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        hub.publish(turn_started("t1"));
        go.wait();
        for i in 0..DELTAS {
            hub.publish(SessionEvent::Delta {
                turn_id: "t1".into(),
                target: DeltaTarget::Text,
                text: piece(i),
            });
        }
        hub.publish(warn(END));
    })
}

/// Attach over the socket and read to the sentinel, reconstructing the text.
///
/// Returns `(text, dropped, resyncs)`.
fn read_to_end(path: &std::path::Path, since: u64) -> (String, u64, usize) {
    let (mut client, hello, reader) = HeadClient::attach(
        path,
        "s",
        since,
        "tui",
        "test",
        Caps {
            queue: 1 << 16,
            ..Caps::default()
        },
    )
    .expect("attach");

    let (tx, rx) = std::sync::mpsc::channel();
    let t = std::thread::spawn(move || pump(reader, tx));

    let mut text = String::new();
    let mut dropped = 0;
    let mut resyncs = 0;
    if let ServerFrame::Hello {
        snapshot,
        dropped: d,
        ..
    } = hello
    {
        dropped = d;
        if let Some(s) = snapshot
            && let Some(turn) = s.turn
        {
            text.push_str(&turn.text);
        }
    }

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut last_seq;
    let mut rendered = 0u64;
    let mut filtered = 0u64;
    while Instant::now() < deadline {
        let Ok(frame) = rx.recv_timeout(Duration::from_secs(5)).map(Inbound::frame) else {
            break;
        };
        match frame {
            ServerFrame::Event(env) => {
                last_seq = env.seq;
                match &env.event {
                    SessionEvent::Delta { text: t, .. } => {
                        text.push_str(t);
                        rendered += 1;
                    }
                    SessionEvent::Warning { detail, .. } if detail == END => {
                        rendered += 1;
                        // Ack after the batch is accounted for, never on receipt.
                        let _ = client.ack(letibot_sessionlog::protocol::Ack {
                            seq: last_seq,
                            rendered,
                            filtered,
                        });
                        break;
                    }
                    _ => filtered += 1,
                }
            }
            ServerFrame::Resync { snapshot, .. } => {
                resyncs += 1;
                text.clear();
                if let Some(turn) = snapshot.turn {
                    text.push_str(&turn.text);
                }
            }
            ServerFrame::Bye { .. } => break,
            _ => {}
        }
    }
    let _ = client.detach();
    drop(client);
    let _ = t.join();
    (text, dropped, resyncs)
}

#[test]
fn a_late_head_attaching_mid_turn_loses_no_byte_and_repeats_none() {
    // Twenty attaches at twenty different moments in the stream. A window between
    // "cut the snapshot" and "register the subscriber" would show up here as a
    // short reconstruction; a window the other way as a doubled fragment.
    for round in 0..20 {
        let (hub, server) = start(&format!("mid-{round}"));
        let path = server.path().to_path_buf();
        let go = Arc::new(Barrier::new(2));
        let producer = produce(hub.clone(), go.clone());
        go.wait();
        // Attach at a varying point in the stream.
        std::thread::sleep(Duration::from_micros(round as u64 * 120));

        let (text, dropped, resyncs) = read_to_end(&path, 0);
        producer.join().unwrap();

        assert_eq!(
            dropped, 0,
            "round {round}: nothing should have been dropped"
        );
        assert_eq!(resyncs, 0, "round {round}: the queue was large enough");
        assert_eq!(
            text,
            full_text(),
            "round {round}: the snapshot and the stream did not meet cleanly"
        );
        server.shutdown();
    }
}

#[test]
fn the_producer_is_never_slowed_by_a_head_that_stops_reading() {
    let (hub, server) = start("slow");
    let path = server.path().to_path_buf();

    // A head that attaches with a tiny queue and then never reads a frame.
    let stop = Arc::new(AtomicBool::new(false));
    let s2 = stop.clone();
    let p2 = path.clone();
    let stuck = std::thread::spawn(move || {
        let stream = std::os::unix::net::UnixStream::connect(&p2).unwrap();
        let mut w = FrameWriter::new(stream.try_clone().unwrap());
        w.write(&ClientFrame::Attach {
            protocol_version: PROTOCOL_VERSION,
            session_id: "s".into(),
            since_seq: 0,
            kind: "tui".into(),
            identity: "stuck".into(),
            caps: Caps {
                queue: 4,
                ..Caps::default()
            },
        })
        .unwrap();
        // Read exactly the Hello, then stop reading entirely.
        let mut r = FrameReader::new(stream);
        let _: ServerFrame = r.read().unwrap();
        while !s2.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(5));
        }
        drop(r);
    });

    // Give it time to attach.
    let t0 = Instant::now();
    while hub.attached_heads() == 0 && t0.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(hub.attached_heads(), 1);

    let started = Instant::now();
    hub.publish(turn_started("t1"));
    for i in 0..DELTAS {
        hub.publish(SessionEvent::Delta {
            turn_id: "t1".into(),
            target: DeltaTarget::Text,
            text: piece(i),
        });
    }
    let elapsed = started.elapsed();

    // HeadAttached, TurnStarted, then every delta.
    assert_eq!(hub.head_seq() as usize, DELTAS + 2);
    assert!(
        elapsed < Duration::from_secs(5),
        "publishing took {elapsed:?} with a stalled head attached; the fan-out blocked"
    );

    stop.store(true, Ordering::Relaxed);
    let _ = stuck.join();
    server.shutdown();
}

#[test]
fn every_head_detaching_mid_turn_changes_nothing() {
    let (hub, server) = start("detach");
    let path = server.path().to_path_buf();
    let (mut client, _hello, reader) =
        HeadClient::attach(&path, "s", 0, "tui", "test", Caps::default()).unwrap();
    let (tx, _rx) = std::sync::mpsc::channel();
    let t = std::thread::spawn(move || pump(reader, tx));

    hub.publish(turn_started("t1"));
    client.detach().unwrap();
    drop(client);
    let _ = t.join();

    let t0 = Instant::now();
    while hub.attached_heads() > 0 && t0.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(hub.attached_heads(), 0);

    // Twenty minutes of build, or twenty deltas. Either way it carries on.
    for i in 0..20 {
        hub.publish(SessionEvent::Delta {
            turn_id: "t1".into(),
            target: DeltaTarget::Text,
            text: piece(i),
        });
    }
    let snap = hub.snapshot();
    assert_eq!(
        snap.turn.unwrap().text,
        (0..20).map(piece).collect::<String>(),
        "reaping on detach is how a twenty-minute build dies to a closed tab"
    );
    server.shutdown();
}

#[test]
fn a_reattaching_head_is_never_re_asked_a_settled_question() {
    let (hub, server) = start("scrub");
    let path = server.path().to_path_buf();

    let (mut client, hello, reader) =
        HeadClient::attach(&path, "s", 0, "tui", "alice", Caps::default()).unwrap();
    let ServerFrame::Hello { snapshot, .. } = hello else {
        panic!()
    };
    let since = snapshot.unwrap().seq;
    let (tx, _rx) = std::sync::mpsc::channel();
    let t = std::thread::spawn(move || pump(reader, tx));

    // While it is away: a question is asked and answered.
    client.detach().unwrap();
    drop(client);
    let _ = t.join();
    hub.publish(turn_started("t1"));
    hub.publish(requested("d1", "rm -rf /"));
    hub.publish(progress("t1"));
    hub.publish(answered("d1", "deny"));

    let (mut client, hello, reader) =
        HeadClient::attach(&path, "s", since, "tui", "alice", Caps::default()).unwrap();
    let ServerFrame::Hello {
        resumed_from,
        scrubbed,
        ..
    } = hello
    else {
        panic!()
    };
    assert_eq!(resumed_from, Some(since), "served from the scrollback");
    assert_eq!(
        scrubbed.settled_decisions, 1,
        "and it says what it stripped"
    );
    assert_eq!(scrubbed.prompt_progress, 1);

    let (tx, rx) = std::sync::mpsc::channel();
    let t = std::thread::spawn(move || pump(reader, tx));
    let mut saw_prompt = false;
    let mut saw_outcome = false;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let Ok(ServerFrame::Event(env)) = rx
            .recv_timeout(Duration::from_millis(300))
            .map(Inbound::frame)
        else {
            break;
        };
        match env.event {
            SessionEvent::DecisionRequested { .. } => saw_prompt = true,
            SessionEvent::DecisionAnswered { .. } => saw_outcome = true,
            _ => {}
        }
    }
    assert!(
        !saw_prompt,
        "a settled decision was replayed as an open prompt"
    );
    assert!(
        saw_outcome,
        "the outcome is what a reattaching head should see"
    );

    client.detach().unwrap();
    drop(client);
    let _ = t.join();
    server.shutdown();
}

/// **A differing protocol version is SERVED, not refused** — the reversal of the gate this
/// file used to pin.
///
/// The operator lost a 17-day-old daemon to a bare `Bye` while a *newer* head stood there
/// unable to read the conversation its warm KV cost minutes to rebuild, and ruled: *"so
/// ideally it would be like - connect, look around and make informed decision"*. Both
/// directions are asserted, because the ruling is not about one of them: a head from the
/// future (`+99`) and a head from the past (the operator's own case — a head speaking 22
/// against this daemon's 36) are both seated, and the `Hello` each gets states **this
/// daemon's** version, which is the fact the head's own informed decision is made from.
/// The daemon's loudness for a skew is its stderr record at the attach, and the louder
/// guard — the `Bye` naming both builds when a frame genuinely cannot be read — is the
/// read loop's, unchanged and further down this file.
///
/// "Serves" is asserted, not implied: a read the oldest head and the newest daemon share
/// (`ListSessions`, protocol 2) is asked on the skewed connection and answered on it.
#[test]
fn a_version_mismatch_is_served_not_refused() {
    let (_hub, server) = start("version");
    for (label, version) in [
        ("newer", PROTOCOL_VERSION + 99),
        ("older", PROTOCOL_VERSION.saturating_sub(14)),
    ] {
        let stream = std::os::unix::net::UnixStream::connect(server.path()).unwrap();
        let mut w = FrameWriter::new(stream.try_clone().unwrap());
        w.write(&ClientFrame::Attach {
            protocol_version: version,
            session_id: "s".into(),
            since_seq: 0,
            kind: "tui".into(),
            identity: "late".into(),
            caps: Caps::default(),
        })
        .unwrap();
        let mut r = FrameReader::new(stream);
        let f: ServerFrame = r.read().unwrap();
        let ServerFrame::Hello {
            protocol_version: said,
            ..
        } = &f
        else {
            panic!("{label}: expected to be seated, got {f:?}");
        };
        assert_eq!(
            *said, PROTOCOL_VERSION,
            "{label}: the Hello states this daemon's own version, whatever the head claimed"
        );
        w.write(&ClientFrame::ListSessions).unwrap();
        let mut saw_list = false;
        for _ in 0..4 {
            if let ServerFrame::Sessions { .. } = r.read().unwrap() {
                saw_list = true;
                break;
            }
        }
        assert!(saw_list, "{label}: the skewed attach must answer a read");
        w.write(&ClientFrame::Detach).unwrap();
        drop(w);
        drop(r);
    }
    server.shutdown();
}

#[test]
fn a_head_that_falls_behind_is_demoted_and_told_where_it_now_is() {
    let hub = Hub::new("s");
    let a = hub.attach(
        "tui",
        "slow",
        Caps {
            queue: 8,
            ..Caps::default()
        },
        0,
    );
    hub.publish(turn_started("t1"));
    for i in 0..200 {
        hub.publish(SessionEvent::Delta {
            turn_id: "t1".into(),
            target: DeltaTarget::Text,
            text: piece(i),
        });
    }
    match hub.next_batch(&a.head_id, 64) {
        letibot_sessionlog::hub::Delivery::Resync { snapshot, .. } => {
            // Not a hole: the accumulated text, complete, as of the resync seq.
            assert_eq!(
                snapshot.turn.unwrap().text,
                (0..200).map(piece).collect::<String>()
            );
            assert_eq!(snapshot.seq, hub.head_seq());
        }
        other => panic!("expected a resync, got {other:?}"),
    }
}

/// A peek reads another session's scrollback and leaves the connection where it
/// was. Those two facts are the whole feature: the child's events arrive as the
/// answer, and the parent's next event arrives after them on the same stream —
/// no `Hello`, no rebuild, no window in which this head is attached to the
/// wrong session.
#[test]
fn a_peek_reads_another_session_without_moving_the_head() {
    let registry = Registry::new();
    let wiring = SessionWiring::default();
    let parent = registry
        .create("s-parent", "parent", wiring.clone())
        .expect("create");
    let child = registry.create("s-child", "child", wiring).expect("create");
    parent.publish(turn_started("t1"));
    child.publish(turn_started("t2"));
    child.publish(delta("t2", "hello from the child"));

    let (a, b) = UnixStream::pair().expect("pair");
    let reg = registry.clone();
    let _server = std::thread::spawn(move || {
        let _ = serve_conn(reg, a);
    });
    let mut w = FrameWriter::new(b.try_clone().expect("clone"));
    let mut r = FrameReader::new(b);

    w.write(&ClientFrame::Attach {
        protocol_version: PROTOCOL_VERSION,
        session_id: "s-parent".into(),
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

    w.write(&ClientFrame::Peek {
        session_id: "s-child".into(),
        // **The default, spelled.** This test is about the ring, and it is also the assertion
        // that the default shape really is `Events`: a head that asks for nothing keeps getting
        // exactly what it always got, which is what makes the rows field additive.
        shape: PeekShape::Events,
    })
    .expect("peek");
    match r.read::<ServerFrame>().expect("peeked") {
        ServerFrame::Peeked {
            session_id,
            dropped,
            events,
            snapshot,
        } => {
            assert_eq!(session_id, "s-child");
            assert_eq!(dropped, 0);
            assert!(
                snapshot.is_none(),
                "a snapshot arrived unasked: the default shape is events, and a head that does \
                 not ask for rows must not be sent one"
            );
            assert!(
                events.iter().any(|e| matches!(
                    &e.event,
                    SessionEvent::Delta { text, .. } if text == "hello from the child"
                )),
                "the child's scrollback is the answer: {events:?}"
            );
        }
        other => panic!("expected Peeked, got {other:?}"),
    }

    // The connection never moved: the parent's next event still arrives here.
    parent.publish(warn("still here"));
    assert!(matches!(
        r.read::<ServerFrame>().expect("parent event"),
        ServerFrame::Event(_)
    ));
}

/// **The other shape a peek can answer with: the session's own ROWS** — and it is the operator's
/// correction of 2026-10-03 that made it exist: *"yes subagents are not even scratch session they
/// are session, just sub sessions."*
///
/// # Why the ring was not enough, in one line
///
/// [`ServerFrame::Peeked`]'s own docstring is what forced the second renderer: the events it
/// returns are *"for reading, not for folding into the head's state"* — so a head could do nothing
/// with them but **draw them by hand**, and that hand-written plain-string renderer is
/// `sub_out_lines` in letibot and `subagent-out-lines` in leticl. A child is a session, so it can
/// be asked for what an attach returns: rows, with their bodies. Both heads then draw it with the
/// renderer they already have and **delete** their copy of the other one.
///
/// # The three properties, and each one is a way this could be wrong
///
/// * **The rows are the CHILD's.** A peek that answered with the caller's own session would be the
///   same frame with the wrong contents, which is the one failure a head cannot detect.
/// * **The rows carry the BODY.** That is the whole reason for the shape: the ring carries an
///   announcement with no text and a content event beside it, and a snapshot's row carries the
///   text. This asserts the text, because a row without its body is the defect wearing the fix's
///   frame name.
/// * **The two shapes are alternatives, not a pair.** Sending both would put one session on the
///   wire twice and leave a head free to draw it twice — and it would cost the snapshot's bytes on
///   every peek that only wanted the ring.
#[test]
fn a_peek_can_ask_for_rows_and_gets_the_session_it_names() {
    let registry = Registry::new();
    let wiring = SessionWiring::default();
    let parent = registry
        .create("s-parent", "parent", wiring.clone())
        .expect("create");
    let child = registry.create("s-child", "child", wiring).expect("create");
    child.publish(turn_started("t2"));
    // A row, announced and then given its body — the same two steps the daemon performs.
    child.publish(appended("s-child.0", "user"));
    child.publish(content("s-child.0", "a child's own words"));

    let (a, b) = UnixStream::pair().expect("pair");
    let reg = registry.clone();
    let _server = std::thread::spawn(move || {
        let _ = serve_conn(reg, a);
    });
    let mut w = FrameWriter::new(b.try_clone().expect("clone"));
    let mut r = FrameReader::new(b);

    w.write(&ClientFrame::Attach {
        protocol_version: PROTOCOL_VERSION,
        session_id: "s-parent".into(),
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

    w.write(&ClientFrame::Peek {
        session_id: "s-child".into(),
        shape: PeekShape::Rows,
    })
    .expect("peek");
    match r.read::<ServerFrame>().expect("peeked") {
        ServerFrame::Peeked {
            session_id,
            events,
            snapshot,
            ..
        } => {
            assert_eq!(
                session_id, "s-child",
                "the answer names the session that was asked for, not the one that asked"
            );
            assert!(
                events.is_empty(),
                "the shapes are alternatives rather than a pair: one session must not travel \
                 twice in one frame, or a head is free to draw it twice: {events:?}"
            );
            let snap = snapshot.expect("rows were asked for and no snapshot came");
            assert_eq!(snap.session_id, "s-child");
            let row = snap
                .items
                .iter()
                .find(|it| it.item_id == "s-child.0")
                .expect("the child's own row is in the child's snapshot");
            // **The body**, which is the one thing this shape exists to deliver.
            let text = match row.item.as_ref() {
                Some(letibot_transcript::TranscriptItem::User { parts, .. }) => parts
                    .iter()
                    .find_map(|p| match p {
                        letibot_transcript::UserPart::Text { text } => Some(text.clone()),
                        _ => None,
                    })
                    .unwrap_or_default(),
                other => panic!("expected the child's user row, got {other:?}"),
            };
            assert_eq!(
                text, "a child's own words",
                "the row arrived without its body — which is the announcement the ring already \
                 carried, wearing the fix's frame name"
            );
        }
        other => panic!("expected Peeked, got {other:?}"),
    }

    // And this is still a READ: the parent's own stream carries on untouched.
    parent.publish(warn("still here"));
    assert!(matches!(
        r.read::<ServerFrame>().expect("parent event"),
        ServerFrame::Event(_)
    ));
}

/// A peek at a session this daemon does not hold is a Rejected naming it —
/// never an empty `Peeked`, which would read as "this subagent said nothing",
/// a lie in exactly the voice the operator cannot distinguish from the truth.
#[test]
fn a_peek_at_an_unknown_session_is_rejected_by_name() {
    let registry = Registry::new();
    let parent = registry
        .create("s-parent", "parent", SessionWiring::default())
        .expect("create");

    let (a, b) = UnixStream::pair().expect("pair");
    let reg = registry.clone();
    let _server = std::thread::spawn(move || {
        let _ = serve_conn(reg, a);
    });
    let mut w = FrameWriter::new(b.try_clone().expect("clone"));
    let mut r = FrameReader::new(b);

    w.write(&ClientFrame::Attach {
        protocol_version: PROTOCOL_VERSION,
        session_id: "s-parent".into(),
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

    w.write(&ClientFrame::Peek {
        session_id: "s-absent".into(),
        shape: PeekShape::Events,
    })
    .expect("peek");
    match r.read::<ServerFrame>().expect("rejected") {
        ServerFrame::Rejected { reason, .. } => {
            assert!(reason.contains("s-absent"), "{reason}");
        }
        other => panic!("expected Rejected, got {other:?}"),
    }
}

/// A head asks for the settings its session runs under and gets what the
/// harness last published — and nothing, honestly, for a session whose harness
/// has not published yet. The connection does not move either way.
#[test]
fn settings_are_answered_from_what_the_harness_published() {
    let registry = Registry::new();
    let wiring = SessionWiring::default();
    let hub = registry.create("s-1", "one", wiring).expect("create");
    hub.publish(turn_started("t1"));

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

    // Nothing published yet: an empty list, not a refusal.
    w.write(&ClientFrame::Settings).expect("settings");
    match r.read::<ServerFrame>().expect("settings") {
        ServerFrame::Settings { rows } => assert!(rows.is_empty()),
        other => panic!("expected Settings, got {other:?}"),
    }

    // The harness publishes; the next ask sees it.
    registry.set_settings(
        "s-1",
        vec![letibot_sessionlog::protocol::SettingRow {
            tools: Vec::new(),
            key: "mode".into(),
            value: "automode".into(),
            source: "project store (modes.tsv)".into(),
            editable: "/mode NAME".into(),
            choices: vec!["automode".into(), "automode-edits".into()],
        }],
    );
    w.write(&ClientFrame::Settings).expect("settings");
    match r.read::<ServerFrame>().expect("settings") {
        ServerFrame::Settings { rows } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].key, "mode");
            assert_eq!(rows[0].value, "automode");
        }
        other => panic!("expected Settings, got {other:?}"),
    }

    // Still seated on s-1: a live event arrives on the same stream.
    hub.publish(delta("t1", "still here"));
    assert!(matches!(
        r.read::<ServerFrame>().expect("event"),
        ServerFrame::Event(_)
    ));
}
