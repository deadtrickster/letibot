//! **The pane's way out, driven through the driver's own loop.**
//!
//! `app.rs`'s unit tests hand `App::pane_keys` the bytes directly, which proves the
//! interception in isolation and *nothing about whether the byte ever arrives there*.
//! What sits between the operator's terminal and `pane_keys` is the driver's tick: it
//! takes the bytes the reader consumed and the keys this head decoded from them, and it
//! decides which of the two the pane gets. This file drives that seam.
//!
//! # The two acts, and this file is where they are told apart on the socket
//!
//! * **`ctrl-\` is a DETACH.** Nothing leaves the head — not a frame, not a keystroke —
//!   and the pane is still held, so the program keeps running and a later `!term` attaches
//!   back to the same run. The operator's words for the defect: *"but i dont want it to
//!   exit"*.
//! * **`!term close` is the ending**, and it **asks first**. The frame leaves on `y` and on
//!   nothing else, which is the operator's other rule: *"yeah it is pretty much a terminal
//!   emulator - if a process runs then `ending` must ask"*.
//!
//! The assertions are on **what the socket carried**, because *did anything leave the head*
//! is a fact about the socket and not about a screen — and the two are exactly the two
//! things that were the same act before protocol 34.
//!
//! # The defect the second test was written for
//!
//! `Link::tick` asked `app.pane_open()` **once, at the top of the tick**, and routed the
//! whole read to one side or the other. So a read that carried the keystroke which opens
//! the pane *and* the way-out byte after it — `!term nano` + Enter + `ctrl-\` arriving
//! together, which is one keystroke of a fast hand or one coalesced tty read — began with
//! no pane: the raw stream went down `App::key`, `term.rs`'s decoder ate `0x1c` (it has no
//! arm for it, `_ => i += 1`), and the pane opened a moment later **with no way out at
//! all**. The way out is looked for on the byte stream, so the byte stream is where it has
//! to be looked for, and not only when the pane was already open when the tick began.

use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use letibot_sessionlog::ScrubReport;
use letibot_sessionlog::protocol::{ClientFrame, PROTOCOL_VERSION, ServerFrame};
use letibot_sessionlog::registry::SessionWiring;
use letibot_sessionlog::wire::{FrameReader, FrameWriter};

use letibot_tui::app::App;
use letibot_tui::backend::decode::decode;
use letibot_tui::driver::Link;
use letibot_tui::render::RenderConfig;

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

/// **A daemon drawn on the wire.** It answers a `TermOpen` with a screen, a `TermClose` with
/// the ending, and a `TermStatus` with **what it is holding** — which is the read the head
/// asks for on `Hello` and before a `!term close` it cannot answer itself. Every frame the
/// head sent is handed back to the test, because *did anything leave the head* is a fact
/// about the socket and not about a screen.
///
/// The pane's state is the daemon's, as it is in the real one: `TermOpen` sets what is
/// running, `TermClose` clears it, and `TermStatus` reports whichever it is.
fn scripted_daemon(path: PathBuf) -> Receiver<ClientFrame> {
    let (tx, rx) = std::sync::mpsc::channel();
    let listener = UnixListener::bind(&path).expect("the socket binds");
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().expect("the attach");
        let mut r = FrameReader::new(stream.try_clone().unwrap());
        let mut w = FrameWriter::new(stream);
        w.write(&hello()).unwrap();
        let mut running: Option<String> = None;
        while let Ok(f) = r.read::<ClientFrame>() {
            match &f {
                ClientFrame::TermOpen { line, .. } => {
                    // The verb's argument, as the daemon names it in its own status answer —
                    // `!term nano` runs `nano`, and the pane is the session's from then on.
                    running = Some(
                        line.strip_prefix("!term ")
                            .unwrap_or(line)
                            .trim()
                            .to_string(),
                    );
                    w.write(&ServerFrame::TermOutput {
                        bytes: b"\x1b[2J\x1b[Hnano's screen\r\n".to_vec(),
                    })
                    .unwrap();
                }
                ClientFrame::TermClose => {
                    running = None;
                    // The ending the daemon composes for the operator's own act — the same
                    // sentence `Terminals::CLOSED` uses since protocol 34.
                    w.write(&ServerFrame::TermEnded {
                        reason: "you closed the terminal".into(),
                    })
                    .unwrap();
                }
                ClientFrame::TermStatus => {
                    w.write(&ServerFrame::TermStatus {
                        command: running.clone(),
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

/// **`ctrl-\` detaches, and NOTHING leaves the head.**
///
/// The ordinary case, and the one `app.rs` cannot see: the pane is open when the tick
/// begins, so the bytes go to the program and the interception is on the same stream.
///
/// The assertion that matters is the negative — **no `TermClose` on the socket** — because
/// that is the whole of the change: the key used to end the program, and now it does not
/// even tell the daemon about it. What replaces it is stated twice on the screen, and both
/// are asserted: the pane is still held (so the program is alive and `!term` has something
/// to come back to) and the chrome says so.
#[test]
fn ctrl_backslash_detaches_through_the_drivers_loop() {
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
        !sent(&frames)
            .iter()
            .any(|f| matches!(f, ClientFrame::TermClose)),
        "a detach sent a frame that ends the program — that is the defect the split exists \
         to remove"
    );
    assert!(!a.pane_open(), "the conversation's rectangle is back");
    assert!(
        a.holds_pane(),
        "and the pane is still held: the program is running on the daemon's pty"
    );
    let frame = a.screen(100, 30);
    assert!(
        frame.iter().any(|r| r.contains("a pane is running")
            && r.contains("!term nano")
            && r.contains("`!term close` ends it")),
        "the screen says the program is still running and names both ways on: {frame:?}"
    );
    assert!(
        !frame.iter().any(|r| r.contains("you closed the terminal")),
        "a detach files no ending: {frame:?}"
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
/// before the way-out byte, the pane comes up, and **the way out is a detach** — the
/// program is not signalled, not killed, and not even told, which is what the absence of a
/// frame here says.
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
        !frames.iter().any(|f| matches!(f, ClientFrame::TermClose)),
        "the way-out byte in the read that opened the pane must not end anything: {frames:?}"
    );
    assert!(
        !a.pane_open(),
        "the rectangle is back, not left standing over the conversation"
    );
    assert!(
        a.holds_pane(),
        "and the pane is still the head's — the program is running"
    );
}

/// **`!term` attaches back to the SAME run, through the driver's loop.**
///
/// The way back, and the whole of it is that the detach ended nothing: the head sends the
/// bare verb, the daemon answers with what it is holding and replays the screen, and the
/// rectangle is drawn again. A head that had *ended* the pane on `ctrl-\` would be opening
/// a new one here — or being refused.
#[test]
fn a_bare_term_line_after_a_detach_attaches_back_through_the_drivers_loop() {
    let path = socket_path("attach-back");
    let frames = scripted_daemon(path.clone());
    let mut a = app();
    let mut link = Link::open(&path, SESSION, 0, "tui", "wayout-attach-back").expect("attach");
    settle(&mut link, &mut a);

    step(&mut link, &mut a, b"!term nano\r");
    settle(&mut link, &mut a);
    step(&mut link, &mut a, b"\x1c");
    settle(&mut link, &mut a);
    assert!(!a.pane_open(), "detached");
    assert!(
        !sent(&frames)
            .iter()
            .any(|f| matches!(f, ClientFrame::TermClose)),
        "the detach ended nothing"
    );

    step(&mut link, &mut a, b"!term\r");
    settle(&mut link, &mut a);

    assert!(
        sent(&frames)
            .iter()
            .any(|f| matches!(f, ClientFrame::TermOpen { line, .. } if line == "!term")),
        "a bare `!term` is an attach, and the line goes over as typed"
    );
    assert!(a.pane_open(), "the rectangle is drawn again");
    assert!(
        a.screen(100, 30)
            .iter()
            .any(|r| r.contains("nano's screen")),
        "with the screen the daemon kept for the run it never ended: {:?}",
        a.screen(100, 30)
    );
}

/// **The deliberate close asks on the socket too, and only a `y` sends the frame.**
///
/// The operator's rule for the destructive act, driven through the loop where it matters:
/// `!term close` typed and submitted sends **nothing at all** — the frame leaves the head
/// when, and only when, the operator confirms with the one key that means yes. The daemon
/// is never asked to end a program because somebody typed a verb.
///
/// The cancel half is asserted here as well, because it is the default: Esc (and every
/// other key) leaves the program running, and the frame that would have killed it is not on
/// the socket.
#[test]
fn the_deliberate_close_asks_first_and_only_the_yes_leaves_the_head() {
    let path = socket_path("close-asks");
    let frames = scripted_daemon(path.clone());
    let mut a = app();
    let mut link = Link::open(&path, SESSION, 0, "tui", "wayout-close-asks").expect("attach");
    settle(&mut link, &mut a);

    step(&mut link, &mut a, b"!term nano\r");
    settle(&mut link, &mut a);
    step(&mut link, &mut a, b"\x1c");
    settle(&mut link, &mut a);
    let _ = sent(&frames);

    // The verb, submitted. Nothing is sent, and the card is up naming what would end.
    step(&mut link, &mut a, b"!term close\r");
    settle(&mut link, &mut a);
    assert!(
        !sent(&frames)
            .iter()
            .any(|f| matches!(f, ClientFrame::TermClose)),
        "the verb ended the program on its own — that is what the confirmation exists to stop"
    );
    let card = a.screen(100, 30);
    assert!(
        card.iter()
            .any(|r| r.contains("end the pane") && r.contains("!term nano")),
        "the card names what it is about to end: {card:?}"
    );
    assert!(
        card.iter().any(|r| r.contains("y ends it")),
        "and says both keys, and which is the default: {card:?}"
    );

    // Esc is the cancel — the safe default — and it sends nothing.
    step(&mut link, &mut a, b"\x1b");
    settle(&mut link, &mut a);
    assert!(
        !sent(&frames)
            .iter()
            .any(|f| matches!(f, ClientFrame::TermClose)),
        "the cancel sent the frame anyway"
    );
    assert!(
        a.holds_pane(),
        "and the program is still running, which is what the cancel is for"
    );

    // Ask again, and this time say yes.
    step(&mut link, &mut a, b"!term close\r");
    settle(&mut link, &mut a);
    step(&mut link, &mut a, b"y");
    settle(&mut link, &mut a);

    assert!(
        sent(&frames)
            .iter()
            .any(|f| matches!(f, ClientFrame::TermClose)),
        "the confirmed close never left the head"
    );
    assert!(!a.pane_open(), "the pane is over");
    assert!(!a.holds_pane(), "and the head has let it go");
    let frame = a.screen(100, 30);
    assert!(
        frame
            .iter()
            .any(|r| r.contains("!term nano") && r.contains("you closed the terminal")),
        "the ending is a row, with the daemon's own sentence: {frame:?}"
    );
    // **The dim register**: the operator chose this ending, so it is housekeeping rather
    // than the answer to something they just typed.
    assert!(
        frame
            .iter()
            .any(|r| r.trim_start().starts_with('·') && r.contains("!term nano")),
        "an ending the operator chose is the housekeeping register: {frame:?}"
    );
}

/// **And the way-out byte is still not a key.** The control for the three above: with no
/// pane and no pane opening, the same byte is nothing at all — it is not typed, it does not
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
    let frames = sent(&frames);
    assert!(
        !frames.iter().any(|f| matches!(f, ClientFrame::TermClose)),
        "a way out of nothing is not a close: {frames:?}"
    );
    assert!(
        !frames
            .iter()
            .any(|f| matches!(f, ClientFrame::TermOpen { .. })),
        "and it opens nothing: {frames:?}"
    );
    assert!(
        !a.pane_open(),
        "and there is no pane to be in: {:?}",
        a.screen(100, 30)
    );
}
