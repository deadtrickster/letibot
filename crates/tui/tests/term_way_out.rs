//! **The pane's way out, driven through the driver's own loop.**
//!
//! `app.rs`'s unit test hands `App::pane_keys` the bytes directly, which proves the
//! interception in isolation and *nothing about whether the byte ever arrives there*.
//! What sits between the operator's terminal and `pane_keys` is the driver's tick: it
//! takes the bytes the reader consumed and the keys this head decoded from them, and it
//! decides which of the two the pane gets. This file drives that seam.
//!
//! # The defect this file was written for
//!
//! `Link::tick` asked `app.pane_open()` **once, at the top of the tick**, and routed the
//! whole read to one side or the other. So a read that carried the keystroke which opens
//! the pane *and* the way-out byte after it — `!term nano` + Enter + `ctrl-\` arriving
//! together, which is one keystroke of a fast hand or one coalesced tty read — began with
//! no pane: the raw stream went down `App::key`, `term.rs`'s decoder ate `0x1c` (it has no
//! arm for it, `_ => i += 1`), and the pane opened a moment later **with no way out at
//! all**. The way out is looked for on the byte stream, so the byte stream is where it has
//! to be looked for, and not only when the pane was already open when the tick began.

use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use letibot_sessionlog::ScrubReport;
use letibot_sessionlog::protocol::{ClientFrame, PROTOCOL_VERSION, ServerFrame};
use letibot_sessionlog::registry::SessionWiring;
use letibot_sessionlog::wire::{FrameReader, FrameWriter};

use letibot_tui::app::{App, Key};
use letibot_tui::driver::Link;
use letibot_tui::render::RenderConfig;
use letibot_tui::term::decode;

const SESSION: &str = "s-wayout";

fn socket_path(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("letibot-wayout-{tag}-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

fn hello() -> ServerFrame {
    ServerFrame::Hello {
        protocol_version: PROTOCOL_VERSION,
        session_id: SESSION.into(),
        head_id: "h1".into(),
        dropped: 0,
        snapshot: None,
        resumed_from: None,
        scrubbed: ScrubReport::default(),
        wiring: SessionWiring::default(),
        sessions: Vec::new(),
    }
}

/// **A daemon drawn on the wire.** It answers a `TermOpen` with a screen and a `TermClose`
/// with the ending, and every frame the head sent is handed back to the test — because
/// *did the close leave the head* is a fact about the socket and not about a screen.
fn scripted_daemon(path: PathBuf) -> Receiver<ClientFrame> {
    let (tx, rx) = std::sync::mpsc::channel();
    let listener = UnixListener::bind(&path).expect("the socket binds");
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().expect("the attach");
        let mut r = FrameReader::new(stream.try_clone().unwrap());
        let mut w = FrameWriter::new(stream);
        w.write(&hello()).unwrap();
        while let Ok(f) = r.read::<ClientFrame>() {
            match &f {
                ClientFrame::TermOpen { .. } => {
                    w.write(&ServerFrame::TermOutput {
                        bytes: b"\x1b[2J\x1b[Hnano's screen\r\n".to_vec(),
                    })
                    .unwrap();
                }
                // The ending the daemon composes for the operator's own act — the same
                // sentence `TermSession::close` uses. `Pane::left` is read off it.
                ClientFrame::TermClose => {
                    w.write(&ServerFrame::TermEnded {
                        reason: "you left the terminal".into(),
                    })
                    .unwrap();
                }
                _ => {}
            }
            let _ = tx.send(f);
        }
    });
    rx
}

fn app() -> App {
    App::new(RenderConfig {
        width: 100,
        color: false,
        ..RenderConfig::default()
    })
}

/// One pass of the driver's loop, with the bytes and the keys the reader would have
/// produced from them. A test has no terminal, so the drawing goes to a sink.
fn step(link: &mut Link, a: &mut App, raw: &[u8]) {
    let mut sink = |_lines: &[String], _cursor| {};
    link.tick(a, (100, 30), &decode(raw), raw, &mut sink);
}

/// Read until nothing new arrives, so a frame already on the socket has landed.
fn settle(link: &mut Link, a: &mut App) {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut quiet = 0;
    while Instant::now() < deadline {
        let before = a.seq;
        step(link, a, &[]);
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
}

fn sent(frames: &Receiver<ClientFrame>) -> Vec<ClientFrame> {
    let mut out = Vec::new();
    while let Ok(f) = frames.recv_timeout(Duration::from_millis(200)) {
        out.push(f);
    }
    out
}

/// **The way out leaves the head, and the pane is over.**
///
/// The ordinary case, and the one `app.rs` cannot see: the pane is open when the tick
/// begins, so the bytes go to the program and the interception is on the same stream.
#[test]
fn ctrl_backslash_closes_the_pane_through_the_drivers_loop() {
    let path = socket_path("plain");
    let frames = scripted_daemon(path.clone());
    let mut a = app();
    let mut link = Link::open(&path, SESSION, 0, "tui", "wayout-plain").expect("attach");
    settle(&mut link, &mut a);

    step(&mut link, &mut a, b"!term nano\r");
    settle(&mut link, &mut a);
    assert!(
        a.pane_open(),
        "the pane is up before the way out is pressed"
    );
    assert!(
        sent(&frames)
            .iter()
            .any(|f| matches!(f, ClientFrame::TermOpen { .. })),
        "the pane's open reached the daemon"
    );

    step(&mut link, &mut a, b"\x1c");
    settle(&mut link, &mut a);

    assert!(
        sent(&frames)
            .iter()
            .any(|f| matches!(f, ClientFrame::TermClose)),
        "the way out never left the head"
    );
    assert!(!a.pane_open(), "the conversation's rectangle is back");
    assert!(
        a.screen(100, 30)
            .iter()
            .any(|r| r.contains("you left the terminal")),
        "the ending is the row the last branch added: {:?}",
        a.screen(100, 30)
    );
}

/// **The byte that opens the pane and the byte that leaves it can arrive in one read.**
///
/// This is the defect, as a test. `!term nano` + Enter + `ctrl-\` in one coalesced tty
/// read used to begin with no pane: the whole stream went down `App::key`, the decoder
/// dropped `0x1c` on its `_ => i += 1` arm, and the pane opened with no way out in it —
/// the operator stuck in a full-screen program with no key that would end it.
///
/// So the assertion is that the way out is looked for **on the byte stream**, and not only
/// when the pane happened to be open when the tick began: the program gets the bytes
/// before the way-out byte, the head sends the close, and the pane is over.
#[test]
fn the_way_out_is_not_eaten_by_the_read_that_opens_the_pane() {
    let path = socket_path("same-read");
    let frames = scripted_daemon(path.clone());
    let mut a = app();
    let mut link = Link::open(&path, SESSION, 0, "tui", "wayout-same-read").expect("attach");
    settle(&mut link, &mut a);

    // One read, exactly as `Terminal::keys` would have consumed it.
    step(&mut link, &mut a, b"!term nano\r\x1c");
    settle(&mut link, &mut a);

    let frames = sent(&frames);
    assert!(
        frames
            .iter()
            .any(|f| matches!(f, ClientFrame::TermOpen { .. })),
        "the pane opened: {frames:?}"
    );
    assert!(
        frames.iter().any(|f| matches!(f, ClientFrame::TermClose)),
        "the way-out byte was eaten by the read that opened the pane — there is no way out \
         of a pane the operator cannot leave: {frames:?}"
    );
    assert!(
        !a.pane_open(),
        "the rectangle is back, not left standing over the conversation"
    );
}

/// **And the way-out byte is still not a key.** The control for the two above: with no pane
/// and no pane opening, the same byte is nothing at all — it is not typed, it does not
/// submit, and it does not leave the composer.
#[test]
fn with_no_pane_the_way_out_byte_is_still_nothing() {
    let path = socket_path("no-pane");
    let frames = scripted_daemon(path.clone());
    let mut a = app();
    let mut link = Link::open(&path, SESSION, 0, "tui", "wayout-no-pane").expect("attach");
    settle(&mut link, &mut a);

    step(&mut link, &mut a, b"hello\x1c");
    settle(&mut link, &mut a);

    assert_eq!(a.input(), "hello", "the byte is not a character");
    assert!(
        !sent(&frames)
            .iter()
            .any(|f| matches!(f, ClientFrame::TermClose)),
        "a way out of nothing is not a close"
    );
    let _ = Key::Enter;
    let _ = UnixStream::pair();
}
