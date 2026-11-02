//! The daemon goes away and the head does not: it keeps drawing, says the link is down, and a
//! daemon coming back is RESUMED from the seq the head had read. A real daemon cannot produce this
//! in process (a `Bye` is by design not a drop), so the peer below speaks frames and closes silently.

use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use letibot_sessionlog::ScrubReport;
use letibot_sessionlog::client::{HeadClient, Inbound, pump};
use letibot_sessionlog::event::{Envelope, SessionEvent};
use letibot_sessionlog::protocol::{Caps, ClientFrame, PROTOCOL_VERSION, ServerFrame};
use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_sessionlog::server::serve_registry;
use letibot_sessionlog::wire::{FrameReader, FrameWriter};
use letibot_transcript::{TranscriptItem, UserPart};

use letibot_tui::app::{App, Key, RECONNECT_BACKOFF_MS};
use letibot_tui::driver::Link;
use letibot_tui::ui::render::RenderConfig;

const SESSION: &str = "s-reconnect";

fn socket_path(tag: &str) -> PathBuf {
    let mut p = letibot_sessionlog::testing::scratch_root();
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

/// One pass plus the caller's half of the recovery, exactly as the binary does it: ask the head
/// whether the backoff has passed, and if it has, open the socket here. The split IS the design -
/// timing belongs to the head, which has the clock; the socket to this layer, which has the path.
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

/// A daemon, drawn on the wire: two connections, scripted, each `ATTACH` handed back for the test
/// to judge. Connection one answers a fresh attach, puts two rows on the screen and then closes
/// without a word; connection two answers the retry as a RESUME and stays open.
fn scripted_daemon(path: PathBuf) -> std::sync::mpsc::Receiver<Attach> {
    let (tx, rx) = std::sync::mpsc::channel();
    let listener = UnixListener::bind(&path).expect("the socket binds");
    std::thread::spawn(move || {
        // ---- connection one: a fresh attach, two rows, then silence.
        let (stream, _) = listener.accept().expect("the first attach");
        // The reading fd is scoped, and that is the whole of "the daemon goes away": `try_clone` is
        // a second descriptor for one socket, so the connection is gone only when every one of them
        // is closed, and a peer holding its reading half open looks like a daemon merely quiet.
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
        // Stay attached: read and discard whatever the head sends - a `Settings` and an
        // `Ack`, both ordinary - until it goes away. Holding the stream open is the
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

    // What the first connection put on the screen. Both rows are here already and the daemon is
    // already gone - the peer closes the socket the moment it has written them - so there is no
    // `!a.detached()` to assert: the whole design is that the head keeps this screen afterwards.
    let before = frame(&mut a);
    assert!(before.contains("remember this row"), "{before}");

    // **The daemon goes away.** Nothing is sent, the socket is simply closed, and the
    // head has to notice from its reader thread ending - which is the case that used to
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

    // A line typed here does not leave - and does not look sent. The trap is the echo: `submit`
    // pushes it into the pending list and the conversation draws it `queued`, while there is no
    // queue behind it, so when the daemon comes back the sentence is gone.
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
    // seq this head had read - not 0, which would take a fresh snapshot and start the
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
    // A resume fills a gap; it does not drill one: every row here came with its body, and a resume
    // replays every event past the read mark. The substring is the CURRENT wording - a test that
    // looks for text the code no longer has passes against any string, including a wrong one.
    assert!(
        !recovered.contains("never filled"),
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

    // And it says how long it has been trying, how many times, and what to do - a head
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

/// A socket with nobody on it is not a reconnect case at all - there is nothing on the screen to
/// keep, so the attach fails and the caller reports it. The requirement is about a head whose
/// daemon goes away; a head that could not attach has no session to hold.
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

/// A head that reconnects names the session it is IN, and lands there. An empty session id on
/// `ATTACH` means whatever the daemon calls current, and `Switch` moves that default, so a
/// reconnect must send the session the last `Hello` named - `Link` keeps it for this reason.
#[test]
fn a_reconnecting_head_lands_on_the_session_it_is_in_not_the_daemons_current_one() {
    const HABR: &str = "s-habr";
    const RUST: &str = "s-rust";

    let registry = Registry::new();
    registry
        .create(HABR, "the habr article", SessionWiring::default())
        .expect("a conversation");
    registry
        .create(RUST, "the rust rewrite", SessionWiring::default())
        .expect("another one");
    let path = socket_path("names-its-session");
    let server = serve_registry(registry.clone(), &path).expect("the socket binds");

    // **The head the operator is looking at**, started in a folder whose daemon was already
    // up: `leticode` sends no session id, so it is seated on the daemon's current session.
    let mut a = app();
    let mut link = Link::open(&path, "", 0, "tui", "reconnect-test").expect("attach");
    settle(&mut link, &mut a);
    assert_eq!(
        a.session_id(),
        HABR,
        "a bare attach lands on the current session"
    );

    // **And the other terminal**: `leticode --session s-rust`, which is a bare attach, a
    // resume, and a switch to what the daemon named.
    let (mut other, hello, reader) =
        HeadClient::attach(&path, "", 0, "tui", "dead", Caps::default()).expect("the second head");
    let ServerFrame::Hello { session_id, .. } = hello else {
        panic!("the first frame must be Hello")
    };
    assert_eq!(session_id, HABR);
    let (tx, rx) = std::sync::mpsc::channel();
    let pumping = std::thread::spawn(move || pump(reader, tx));
    other.resume_session(RUST).expect("the resume");
    until(
        &rx,
        |f| matches!(f, ServerFrame::Sessions { .. }),
        "the resume's answer",
    );
    other.switch(RUST, 0).expect("the switch");
    until(
        &rx,
        |f| matches!(f, ServerFrame::Hello { .. }),
        "the switch's Hello",
    );
    assert_eq!(
        registry.default_id(),
        RUST,
        "a switch moves the session a bare attach lands on — which is what the defect rode"
    );

    // **The first head's socket comes back.** `reconnect` is what the binary calls when the
    // link has dropped; the old connection is hung up here, which is the same thing from the
    // daemon's side.
    link.reconnect(&path, a.seq).expect("the retry");
    a.reconnect_sent();
    settle(&mut link, &mut a);

    assert_eq!(
        a.session_id(),
        HABR,
        "a reconnect is a resume of the conversation the head was in — not an attach to \
         whatever session the daemon calls current, which another head's switch had moved"
    );
    assert_eq!(
        registry.get(RUST).unwrap().attached_heads(),
        1,
        "the only head in the rust session is the one that asked for it"
    );
    assert_eq!(registry.get(HABR).unwrap().attached_heads(), 1);

    let _ = other.detach();
    let _ = pumping.join();
    link.detach();
    server.shutdown();
    let _ = std::fs::remove_file(&path);
}

/// Read frames until `f` says stop - the second head's own socket, which nothing in this
/// file's scripted peer needs.
fn until(
    rx: &std::sync::mpsc::Receiver<Inbound>,
    mut f: impl FnMut(&ServerFrame) -> bool,
    what: &str,
) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(frame) => {
                if f(&frame.frame()) {
                    return;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    panic!("{what} never arrived");
}
