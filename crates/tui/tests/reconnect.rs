//! **The daemon goes away, and the head does not.**
//!
//! §4.2, end to end and against a real socket: a head attaches, the socket is closed
//! under it, and what is measured is that the head is still *running* — drawing the
//! conversation it had, saying the link is down, holding the composer — and that when a
//! daemon comes back it **resumes from the seq it had**, so the transcript is carried
//! over rather than started again.
//!
//! The unit tests in `app.rs` own the head's half (the line, its clock, the composer
//! refusal, and that a `Bye` is not a drop). This file owns the socket half, which is
//! where the defect was: `client.ack(...)?` propagated out of the driver's loop and out
//! of `main`, so there was nothing left running to assert on.
//!
//! # Why the daemon here is scripted on the wire and not a `ServerHandle`
//!
//! **A real daemon cannot produce this case in-process.** `ServerHandle::shutdown`
//! calls `registry.close()`, the seat's pump answers `Delivery::Closed` with
//! `ServerFrame::Bye { reason: "daemon shutting down" }` — and a `Bye` is by design
//! *not* a drop: it is the daemon saying the conversation is over, and the head leaves
//! with the reason on its screen. That is a different requirement with its own test
//! (`a_bye_leaves_and_never_looks_like_a_link_to_reconnect`).
//!
//! What this file needs is a socket that **closes without a word**, which is what a
//! daemon that is killed, restarted or crashed does. So the peer below speaks protocol
//! frames directly, on two connections, and the assertion that matters is made **on the
//! wire**: the retry's `ATTACH` carries `since_seq` = the seq the head had read, which
//! is the requirement as a number rather than inferred from a screen.
//!
//! # Resumed, not restarted, and how this tells them apart
//!
//! The two are easy to confuse and the difference is the point. A reconnect that took a
//! **fresh snapshot** would show a conversation that *looked* right while having thrown
//! away everything the daemon's log no longer holds, and would rewind the head's read
//! mark — a snapshot is a replacement.
//!
//! So the first connection puts a transcript row on the screen and the second
//! connection's `Hello` carries **no snapshot** and its stream no such row, and the test
//! asserts the row is still there afterwards. A resume keeps `items` and fills the gap;
//! a resync replaces `items` wholesale, and that row would be gone.

use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use letibot_sessionlog::ScrubReport;
use letibot_sessionlog::event::{Envelope, SessionEvent};
use letibot_sessionlog::protocol::{ClientFrame, PROTOCOL_VERSION, ServerFrame};
use letibot_sessionlog::registry::SessionWiring;
use letibot_sessionlog::wire::{FrameReader, FrameWriter};
use letibot_transcript::{TranscriptItem, UserPart};

use letibot_tui::app::{App, Key, RECONNECT_BACKOFF_MS};
use letibot_tui::driver::Link;
use letibot_tui::render::RenderConfig;

const SESSION: &str = "s-reconnect";

fn socket_path(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "letibot-reconnect-{tag}-{}.sock",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&p);
    p
}

fn app() -> App {
    App::new(RenderConfig {
        width: 100,
        color: false,
        ..RenderConfig::default()
    })
}

/// Everything the last frame drew, as one string.
fn frame(a: &mut App) -> String {
    a.screen(100, 30).join("\n")
}

/// One pass, drawing into a throwaway sink. A test has no terminal.
fn step(link: &mut Link, a: &mut App) {
    let mut sink = |_lines: &[String], _cursor| {};
    link.tick(a, (100, 30), &[], &[], &mut sink);
}

/// One pass **plus the caller's half of the recovery**, exactly as the binary does it:
/// ask the head whether the backoff has passed, and if it has, open the socket here.
///
/// Written out rather than hidden behind a helper that took a path, because the split
/// *is* the design — the timing belongs to the head, which has the clock, and the socket
/// belongs to this layer, which has the path.
fn pump_once(link: &mut Link, a: &mut App, path: &std::path::Path) {
    step(link, a);
    if a.should_reconnect() {
        match link.reconnect(path, a.seq) {
            Ok(()) => a.reconnect_sent(),
            Err(e) => a.reconnect_failed(&e.to_string()),
        }
    }
}

/// Read until nothing new arrives, so a frame already on the socket has landed before
/// anything is asserted about it.
fn settle(link: &mut Link, a: &mut App) {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut quiet = 0;
    while Instant::now() < deadline {
        let before = a.seq;
        step(link, a);
        if a.seq == before {
            quiet += 1;
            if quiet >= 4 {
                return;
            }
        } else {
            quiet = 0;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("frames never stopped arriving");
}

fn env(seq: u64, event: SessionEvent) -> Envelope {
    Envelope {
        session_id: SESSION.into(),
        seq,
        ts: 0,
        event,
    }
}

/// A `Hello` for a connection served out of the scrollback: no snapshot, and
/// `resumed_from` saying where the gap starts.
fn hello_resumed(from: u64, head_id: &str) -> ServerFrame {
    ServerFrame::Hello {
        protocol_version: PROTOCOL_VERSION,
        session_id: SESSION.into(),
        head_id: head_id.into(),
        dropped: 0,
        snapshot: None,
        resumed_from: Some(from),
        scrubbed: ScrubReport::default(),
        wiring: SessionWiring::default(),
        sessions: Vec::new(),
    }
}

/// The `ATTACH` a connection opened with, as the peer saw it. This is what the test
/// judges: everything else can be inferred from a screen, and this cannot.
#[derive(Debug, Clone, Copy)]
struct Attach {
    since_seq: u64,
    protocol_version: u32,
}

/// Read one `ATTACH` and hand it back.
fn read_attach(r: &mut FrameReader<UnixStream>) -> Attach {
    let first: ClientFrame = r.read().expect("the first frame is an ATTACH");
    let ClientFrame::Attach {
        protocol_version,
        since_seq,
        ..
    } = first
    else {
        panic!("the first frame must be an ATTACH, got {first:?}");
    };
    Attach {
        since_seq,
        protocol_version,
    }
}

/// **A daemon, drawn on the wire**: two connections, scripted, each `ATTACH` handed back
/// for the test to judge.
///
/// Connection one answers a fresh attach, puts two rows on the screen, and then **closes
/// without a word** — the case a real `ServerHandle` cannot produce, because it says
/// goodbye. Connection two answers the head's retry as a *resume* and stays open, so the
/// head ends the test attached.
fn scripted_daemon(path: PathBuf) -> std::sync::mpsc::Receiver<Attach> {
    let (tx, rx) = std::sync::mpsc::channel();
    let listener = UnixListener::bind(&path).expect("the socket binds");
    std::thread::spawn(move || {
        // ---- connection one: a fresh attach, two rows, then silence.
        let (stream, _) = listener.accept().expect("the first attach");
        // **The reading fd is scoped, and that is the whole of "the daemon goes away".**
        // `try_clone` is a second descriptor for one socket, so the connection is gone
        // only when *every* one of them is closed — and a peer that holds its reading
        // half open looks exactly like a daemon that is merely quiet. Measured: the
        // first version of this left it alive and the head sat attached for ever.
        let at = {
            let mut r = FrameReader::new(stream.try_clone().unwrap());
            read_attach(&mut r)
        };
        let _ = tx.send(at);
        {
            let mut w = FrameWriter::new(stream);
            w.write(&hello_resumed(0, "h1")).unwrap();
            w.write(&ServerFrame::Event(env(
                1,
                SessionEvent::TranscriptAppended {
                    item_id: "s.0".into(),
                    kind: "user".into(),
                    ledger_head: "x".into(),
                },
            )))
            .unwrap();
            w.write(&ServerFrame::Event(env(
                2,
                SessionEvent::TranscriptContent {
                    item_id: "s.0".into(),
                    item: Box::new(TranscriptItem::User {
                        speaker: Default::default(),
                        parts: vec![UserPart::Text {
                            text: "remember this row".into(),
                        }],
                    }),
                },
            )))
            .unwrap();
            // **And the daemon goes away**: the socket closes with no frame of any kind.
        }

        // ---- connection two: the head's retry, answered as a resume.
        let (stream, _) = listener.accept().expect("the retry");
        let mut r = FrameReader::new(stream.try_clone().unwrap());
        let at = read_attach(&mut r);
        let _ = tx.send(at);
        let mut w = FrameWriter::new(stream);
        w.write(&hello_resumed(at.since_seq, "h2")).unwrap();
        // One event the head has not seen, at the seq just past its mark: the gap,
        // arriving as events, which is what a resume is.
        w.write(&ServerFrame::Event(env(
            at.since_seq + 1,
            SessionEvent::Warning {
                code: "gap".into(),
                detail: "this is what you missed".into(),

                compaction: None,
            },
        )))
        .unwrap();
        // Stay attached: read and discard whatever the head sends — a `Settings` and an
        // `Ack`, both ordinary — until it goes away. Holding the stream open is the
        // point; dropping it would look like the daemon leaving again.
        while r.read::<ClientFrame>().is_ok() {}
    });
    rx
}

#[test]
fn a_head_survives_its_daemon_and_resumes_from_the_seq_it_had() {
    let path = socket_path("survive");
    let attaches = scripted_daemon(path.clone());

    let mut a = app();
    let mut link = Link::open(&path, SESSION, 0, "tui", "reconnect-test").expect("attach");
    settle(&mut link, &mut a);

    // A fresh head asks for a snapshot, which is what a head with no state has.
    let first = attaches
        .recv_timeout(Duration::from_secs(5))
        .expect("an ATTACH");
    assert_eq!(first.since_seq, 0, "a fresh head asks for a snapshot");
    assert_eq!(
        first.protocol_version, PROTOCOL_VERSION,
        "and names its build"
    );

    // What the first connection put on the screen. Both rows are here already, and the
    // daemon is already gone — the peer closes the socket the moment it has written
    // them, which is exactly what "the daemon vanishes" looks like and is why there is
    // no `!a.detached()` to assert at this point: the whole design is that the head
    // keeps this screen *afterwards*.
    let before = frame(&mut a);
    assert!(before.contains("remember this row"), "{before}");

    // **The daemon goes away.** Nothing is sent, the socket is simply closed, and the
    // head has to notice from its reader thread ending — which is the case that used to
    // be undetectable at rest, because an idle head writes nothing at all.
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && !a.detached() {
        step(&mut link, &mut a);
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        a.detached(),
        "a head whose daemon left must know it — before this the process was simply gone"
    );
    assert!(!a.should_quit(), "a closed socket is not a reason to leave");

    // **The screen it had is still the screen it has.** The conversation is not cleared,
    // and the one thing added is a line saying why nothing is moving.
    let after = frame(&mut a);
    assert!(
        after.contains("remember this row"),
        "the conversation was taken away: {after}"
    );
    assert!(after.contains("daemon connection is down"), "{after}");
    assert!(after.contains("reconnecting"), "{after}");

    // **A line typed here does not leave — and does not look sent.** The trap is the
    // echo: `submit` pushes it into the pending list, the conversation draws
    // `queued · <their words>`, and there is no queue behind it anywhere, so when the
    // daemon comes back the sentence is gone and it was on the screen as held the whole
    // time.
    for c in "is anybody there?".chars() {
        a.key(Key::Char(c));
    }
    assert_eq!(a.key(Key::Enter), None, "nothing leaves a dead link");
    // The pending list is private, so the assertion is the one the operator would make:
    // the word `queued` is not on the screen, and the words are still in the field they
    // were typed into. A head that echoed them as held would show both.
    let held = frame(&mut a);
    assert!(
        !held.contains("queued"),
        "drawn as held with no queue behind it: {held}"
    );
    assert_eq!(
        a.input(),
        "is anybody there?",
        "the words are still the operator's, in the field they typed them into"
    );

    // It keeps trying, at its own pace, and **it is still here** after several rounds of
    // that. This is the loop that used to be the end of the process.
    for _ in 0..6 {
        assert!(!a.should_quit(), "the head left while its daemon was down");
        pump_once(&mut link, &mut a, &path);
        std::thread::sleep(Duration::from_millis(120));
    }
    assert!(a.detached(), "nobody has come back yet");

    // **The daemon comes back on the same path**, and the head gets it back.
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline && a.detached() {
        pump_once(&mut link, &mut a, &path);
        std::thread::sleep(Duration::from_millis(50));
    }
    settle(&mut link, &mut a);

    // **The requirement, as a number on the wire.** The retry is an `ATTACH` carrying the
    // seq this head had read — not 0, which would take a fresh snapshot and start the
    // transcript over.
    let second = attaches
        .recv_timeout(Duration::from_secs(5))
        .expect("the retry");
    assert_eq!(
        second.since_seq, 2,
        "the head re-attached at the seq it had, not at zero"
    );
    assert_eq!(second.protocol_version, PROTOCOL_VERSION);

    assert!(!a.detached(), "the head did not get its daemon back");
    assert!(!a.should_quit(), "…and did not leave");
    let recovered = frame(&mut a);
    assert!(
        recovered.contains("this is what you missed"),
        "the gap was not filled from the seq the head had: {recovered}"
    );
    // **Carried over, not started again.** The second connection's `Hello` carries no
    // snapshot and its stream has no such row: a resync replaces `items` wholesale, and
    // this is the row it would have thrown away.
    assert!(
        recovered.contains("remember this row"),
        "the transcript was replaced instead of resumed: {recovered}"
    );
    assert_eq!(a.resyncs, 0, "no `Resync` was sent, so none may be counted");
    assert!(recovered.contains("daemon is back"), "{recovered}");
    // **A resume fills a gap; it does not drill one.** Every row on this screen came with
    // its body, so the head has nothing to report as announced-and-never-filled. This is
    // the assertion that separates §4.2 from the R2 defect it can look like: the resume
    // path replays *every* event past the read mark, so a body the daemon wrote is
    // delivered. If a resume ever lost one, this is the string it would say.
    // **The substring is the CURRENT wording, not the one this test was written against.**
    // It said `announced and never filled in`, and the sentence was reworded on 2026-09-23 to
    // name the rows and stop asserting a cause — so this assertion would have passed against
    // ANY string, including one that said every row was missing. A test that looks for text the
    // code no longer has is a green test about nothing.
    assert!(
        !recovered.contains("announced to this head and never filled"),
        "a resume left a row with no body: {recovered}"
    );
    assert!(
        !recovered.contains("daemon connection is down"),
        "the line is still there after the link came up: {recovered}"
    );

    let _ = std::fs::remove_file(&path);
}

/// **Getting back is not the same as staying**, and the head says which it is: a retry
/// that cannot connect keeps the line up, counts the attempt, and does not come back
/// pretending to be attached.
#[test]
fn a_retry_that_cannot_connect_says_so_and_keeps_trying() {
    let path = socket_path("still-absent");
    let mut a = app();
    a.clock(1_000);
    // **No peer at all**, and the drop it is in is the state under test: a head whose
    // daemon is not coming back.
    a.link_down("the daemon closed the connection");
    assert!(a.detached());

    for round in 1..=2u32 {
        a.clock(1_000 + u64::from(round) * RECONNECT_BACKOFF_MS);
        assert!(a.should_reconnect(), "the backoff has passed");
        let Err(e) = Link::open(&path, SESSION, 0, "tui", "reconnect-test") else {
            panic!("there is no daemon on that path");
        };
        a.reconnect_failed(&e.to_string());
        assert!(a.detached(), "a failed retry is still detached");
        assert!(!a.should_quit(), "and is not a reason to leave");
    }

    // And it says how long it has been trying, how many times, and what to do — a head
    // retrying silently for a minute is the failure it is retrying to avoid.
    a.clock(1_000 + 63_000);
    let waiting = frame(&mut a);
    assert!(waiting.contains("trying for 1m"), "{waiting}");
    assert!(waiting.contains("2 attempts"), "{waiting}");
    assert!(
        waiting.contains("letibot --status"),
        "and what the operator can do about it: {waiting}"
    );
    assert!(
        waiting.contains("keeps trying"),
        "it must not read as having given up: {waiting}"
    );
}

/// **A socket with nobody on it is not a reconnect case at all** — there is nothing on
/// the screen to keep, so the attach fails and the caller reports it. The distinction is
/// the requirement's own: this is about a head whose *daemon goes away*, and a head that
/// could not attach has no session to hold.
#[test]
fn an_absent_daemon_fails_the_attach_rather_than_running_detached() {
    let path = socket_path("absent");
    let Err(e) = Link::open(&path, "s", 0, "tui", "reconnect-test") else {
        panic!("a socket with nobody on it must fail the attach");
    };
    let said = format!("{e}");
    assert!(
        said.contains("No such file") || said.contains("connect") || said.contains("refused"),
        "{said}"
    );
}
