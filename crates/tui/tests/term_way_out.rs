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
//!
//! # The two waits, which are conditions
//!
//! A positive claim waits for the frame it is about ([`frames_until`]) and a draw waits for the
//! state it is about ([`settle_until`]); neither is a pause. MEASURED 2026-11-01 on the gate's own
//! shape — three hundred busy processes and `TMPDIR` on a real disk, `/var/tmp/letibot-gate` —
//! the 200 ms drain these tests settled with saw **nothing at all** after a frame the head had
//! written, and two of them failed with `the pane opened: []` and `the Enter is the program's …:
//! []`. An absence cannot be waited for, so every claim that nothing was sent uses a window
//! ([`ABSENCE_WINDOW`]), and the comment at each says why that one has to be a window.
//!
//! # A barrier frame is not a window, and two of these claims used to think it was
//!
//! A positive frame makes an absence exact only for what was written **before** it — a unix socket
//! is read in order, so a `TermClose` in front of the `TermOpen` would be in the same collection.
//! The defect is the other way round: one tick builds one `actions` vector, the Enter's `TermOpen`
//! first and the way-out's actions appended after it, so the frame a way-out would emit is always
//! the NEXT one and [`frames_until`] — which returns the moment its predicate holds — stops in
//! front of it. MEASURED 2026-11-01 with the defect mutated back into `App::pane_keys`: both of
//! those assertions passed. They now collect through the barrier and over the window after it
//! ([`frames_until_and_after`]).

use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use letibot_sessionlog::ScrubReport;
use letibot_sessionlog::protocol::{ClientFrame, PROTOCOL_VERSION, ServerFrame};
use letibot_sessionlog::registry::SessionWiring;
use letibot_sessionlog::wire::{FrameReader, FrameWriter};

use letibot_tui::app::App;
use letibot_tui::app::{Key, key_of};
use letibot_tui::driver::Link;
use letibot_tui::ui::render::RenderConfig;

const SESSION: &str = "s-wayout";

fn socket_path(tag: &str) -> PathBuf {
    let mut p = letibot_sessionlog::testing::scratch_root();
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
                    // The pane's number first, as the real daemon sends it (protocol 44).
                    w.write(&ServerFrame::TermAttached {
                        pane: 1,
                        command: running.clone().unwrap_or_default(),
                        opened: true,
                    })
                    .unwrap();
                    w.write(&ServerFrame::TermOutput {
                        pane: 1,
                        bytes: b"\x1b[2J\x1b[Hnano's screen\r\n".to_vec(),
                    })
                    .unwrap();
                }
                ClientFrame::TermClose { .. } => {
                    running = None;
                    // The ending the daemon composes for the operator's own act — the same
                    // sentence `Terminals::CLOSED` uses since protocol 34.
                    w.write(&ServerFrame::TermEnded {
                        pane: Some(1),
                        reason: "you closed the terminal".into(),
                    })
                    .unwrap();
                }
                ClientFrame::TermStatus => {
                    w.write(&ServerFrame::TermStatus {
                        panes: running
                            .clone()
                            .map(|command| letibot_sessionlog::PaneBrief { pane: 1, command })
                            .into_iter()
                            .collect(),
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
/// The keys this head reads from `raw`: rano decodes the bytes, [`key_of`] says what they mean.
fn decode(raw: &[u8]) -> Vec<Key> {
    rano::term::decode(raw)
        .into_iter()
        .filter_map(key_of)
        .collect()
}

fn step(link: &mut Link, a: &mut App, raw: &[u8]) {
    let mut sink = |_lines: &[String], _cursor| {};
    link.tick(a, (100, 30), &decode(raw), raw, &mut sink);
}

// ---------------------------------------------------------------------------
// The two waits, and why neither is a pause
// ---------------------------------------------------------------------------

/// **How long a wait for a fact about two threads in THIS process may take before it is a hang.**
///
/// Nothing in this file waits on a network, a model or a build: the head and the scripted daemon
/// are both in this process, so a frame is one socket write and one wake-up. This is a hang
/// detector, and the number is far above anything a busy box can add to a write and a wake-up.
const PATIENCE: Duration = Duration::from_secs(60);

/// **How long the daemon is given to read a frame the head did NOT write.**
///
/// An absence cannot be waited for — only watched for — so this is the one KIND of wait in the
/// file that is a window rather than a condition, and it is a window on the *witness*: the head's
/// own write is synchronous (`HeadClient::term_close` puts the frame on the socket before it
/// returns), so whether anything left the head is decided by the time `step` returns. What the
/// window buys is the daemon thread's chance to read it, which is what the channel below can see.
///
/// **It is used two ways**: where nothing was written at all there is no frame to wait for, and
/// where something WAS written the frame is a barrier and the window is what follows it — see
/// [`frames_until_and_after`], which is why a barrier does not on its own make an absence exact.
///
/// MEASURED 2026-11-01, and this is the whole of the fix: with the gate's own shape — three
/// hundred busy processes and `TMPDIR` on a real disk, `/var/tmp/letibot-gate` — a **200 ms**
/// drain saw NOTHING after a frame the head had written, and two of these tests failed with
/// `the pane opened: []` and `the Enter is the program's …: []`. A positive claim now waits for
/// its frame; this window is only for the claims that nothing was sent.
const ABSENCE_WINDOW: Duration = Duration::from_secs(2);

/// **Pump the driver's loop until the head has got where the test is going.**
///
/// The condition, not a pause: `settle` used to tick until four 25 ms passes changed nothing,
/// which is a bet that the daemon thread is scheduled within a hundred milliseconds — a bet the
/// gate's box lost. `&mut App` because the predicate is usually the screen, and drawing it is what
/// records the frame's click targets.
fn settle_until(link: &mut Link, a: &mut App, what: &str, mut ready: impl FnMut(&mut App) -> bool) {
    let deadline = Instant::now() + PATIENCE;
    loop {
        step(link, a, &[]);
        if ready(a) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "waited {PATIENCE:?} for {what}; the screen is {:?}",
            a.screen(100, 30)
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// **Everything the daemon read, up to the frame `ready` names** — so *this reached the daemon*
/// waits for the frame instead of for a pause. Returns what it collected, so a failure prints the
/// frames rather than an empty list.
fn frames_until(
    frames: &Receiver<ClientFrame>,
    what: &str,
    ready: impl Fn(&[ClientFrame]) -> bool,
) -> Vec<ClientFrame> {
    let deadline = Instant::now() + PATIENCE;
    let mut out: Vec<ClientFrame> = Vec::new();
    loop {
        match frames.recv_timeout(Duration::from_millis(20)) {
            Ok(f) => out.push(f),
            // The daemon's thread is gone, which is a fact about the fixture and not a wait.
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return out,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
        if ready(&out) {
            return out;
        }
        assert!(
            Instant::now() < deadline,
            "waited {PATIENCE:?} for {what}; the daemon read {out:?}"
        );
    }
}

/// **The frames up to the barrier `ready` names, AND whatever arrives after it** — the instrument
/// for a claim about something that did not happen, made on the back of a frame that did.
///
/// [`frames_until`] returns the moment its predicate holds. That is exactly right for *this
/// reached the daemon*, and wrong for *and nothing else did*: the defect these two claims are
/// written against emits its frame **after** the barrier. `Link::tick` builds one `actions` vector
/// in tick order — the Enter's `Action::TermOpen` is pushed by `App::key`, then `App::pane_keys`
/// appends the way-out's actions to it — so a `TermClose` the way-out produced is always the next
/// frame, and a collection that stopped at the `TermOpen` (or at the `TermInput` before it) never
/// sees it. MEASURED 2026-11-01 with the defect mutated back into `App::pane_keys`: both
/// assertions passed, which is the coverage they had lost.
///
/// So the absence is claimed over the barrier's own collection **plus** the window after it —
/// [`arrived_meanwhile`], the same instrument the claims with no frame at all use. The collection
/// still matters: it is what catches a frame written in front of the barrier.
fn frames_until_and_after(
    frames: &Receiver<ClientFrame>,
    what: &str,
    ready: impl Fn(&[ClientFrame]) -> bool,
) -> Vec<ClientFrame> {
    let mut out = frames_until(frames, what, ready);
    out.extend(arrived_meanwhile(frames));
    out
}

/// **What the daemon read while the head wrote nothing** — see [`ABSENCE_WINDOW`].
fn arrived_meanwhile(frames: &Receiver<ClientFrame>) -> Vec<ClientFrame> {
    let deadline = Instant::now() + ABSENCE_WINDOW;
    let mut out: Vec<ClientFrame> = Vec::new();
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        if left.is_zero() {
            break;
        }
        match frames.recv_timeout(left) {
            Ok(f) => out.push(f),
            Err(_) => break,
        }
    }
    out
}

/// Whatever is already in the channel, without waiting — for the places a test clears the board.
fn clear(frames: &Receiver<ClientFrame>) -> Vec<ClientFrame> {
    let mut out = Vec::new();
    while let Ok(f) = frames.try_recv() {
        out.push(f);
    }
    out
}

/// **Wait until the head and the daemon have met.**
///
/// The daemon writes the `Hello` and the head answers it with a `TermStatus` ("what are you
/// holding?"), so a `TermStatus` in the channel is a full round trip: the `Hello` was on the
/// socket before it and the same tick took it in. `Link::open` returns before any of that, which is
/// why every test here starts with this rather than with a pause.
///
/// **The loop is pumped while it waits**, and that is not decoration: the ask is *queued* by the
/// `Hello` and written by the next `tick`, so waiting on the channel alone waits for a frame
/// nobody has sent — measured, all seven tests in this file timed out at `PATIENCE` on the first
/// cut of this helper.
fn attach(link: &mut Link, a: &mut App, frames: &Receiver<ClientFrame>) {
    let deadline = Instant::now() + PATIENCE;
    loop {
        step(link, a, &[]);
        if clear(frames)
            .iter()
            .any(|f| matches!(f, ClientFrame::TermStatus))
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "waited {PATIENCE:?} for the attach's own TermStatus; the screen is {:?}",
            a.screen(100, 30)
        );
        std::thread::sleep(Duration::from_millis(10));
    }
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
    attach(&mut link, &mut a, &frames);

    step(&mut link, &mut a, b"!term nano\r");
    settle_until(&mut link, &mut a, "the pane's screen", |a| {
        a.screen(100, 30)
            .iter()
            .any(|r| r.contains("nano's screen"))
    });
    assert!(
        a.pane_open(),
        "the pane is up before the way out is pressed"
    );
    let opened = frames_until(&frames, "the pane's open to reach the daemon", |fs| {
        fs.iter().any(|f| matches!(f, ClientFrame::TermOpen { .. }))
    });
    assert!(
        opened
            .iter()
            .any(|f| matches!(f, ClientFrame::TermOpen { .. })),
        "the pane's open reached the daemon: {opened:?}"
    );

    step(&mut link, &mut a, b"\x1c");
    settle_until(&mut link, &mut a, "the rectangle to be back", |a| {
        !a.pane_open()
    });

    // **The absence, and it is the one claim here that needs a window**: a detach writes nothing,
    // so there is no frame to wait for — see [`ABSENCE_WINDOW`].
    let meanwhile = arrived_meanwhile(&frames);
    assert!(
        !meanwhile
            .iter()
            .any(|f| matches!(f, ClientFrame::TermClose { .. })),
        "a detach sent a frame that ends the program — that is the defect the split exists \
         to remove: {meanwhile:?}"
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

/// **Enter and the way out in ONE read: BOTH effects, not one of them.**
///
/// The coalescing question with the pane already open, which is the case every fast hand
/// hits: the operator presses Enter — in `nano`, a newline — and reaches for `ctrl-\`, and
/// the tty hands the head both bytes as one read: `\r\x1c`. The pane's split
/// ([`App::pane_keys`]) is *bytes before the way out are the program's, the way out itself
/// detaches*, and a read that carried both is not two decisions but one read of that rule —
/// **the `\r` must reach the program AND the detach must be taken**. The defect this test
/// is written against is the silent winner: a path where one of the two eats the other —
/// a head that detached without the Enter ever reaching the pty, or a head that kept the
/// pane up with no key left that would leave it. So both are asserted on the socket and on
/// the app, never one without the other.
#[test]
fn enter_and_the_way_out_in_one_read_forward_the_enter_and_detach() {
    let path = socket_path("enter-wayout");
    let frames = scripted_daemon(path.clone());
    let mut a = app();
    let mut link = Link::open(&path, SESSION, 0, "tui", "wayout-enter-wayout").expect("attach");
    attach(&mut link, &mut a, &frames);

    step(&mut link, &mut a, b"!term nano\r");
    settle_until(&mut link, &mut a, "the pane's screen", |a| {
        a.screen(100, 30)
            .iter()
            .any(|r| r.contains("nano's screen"))
    });
    assert!(a.pane_open(), "the pane is up before the coalesced read");
    let _ = clear(&frames);

    // One read, exactly as `Terminal::events` would have consumed it: Enter + ctrl-\.
    step(&mut link, &mut a, b"\r\x1c");
    settle_until(&mut link, &mut a, "the rectangle to be back", |a| {
        !a.pane_open()
    });

    // **Both halves of the claim come out of one collection**, and the positive one is what makes
    // it exact: the Enter is a frame the daemon reads, so waiting for it gives the daemon its
    // chance to read anything else this read wrote — and a unix socket is read in order, so a
    // `TermClose` written before that `TermInput` would be in front of it here. **And a
    // `TermClose` written AFTER it is the shape the defect actually has** — one tick, one
    // `actions` vector, the Enter first and the way-out's actions appended — so the collection is
    // taken through the barrier and over the window after it. See [`frames_until_and_after`].
    let frames = frames_until_and_after(&frames, "the Enter to reach the program", |fs| {
        fs.iter()
            .any(|f| matches!(f, ClientFrame::TermInput { .. }))
    });
    assert!(
        frames
            .iter()
            .any(|f| matches!(f, ClientFrame::TermInput { bytes, .. } if bytes == b"\r")),
        "the Enter is the program's — a coalesced read must not detach it away: {frames:?}"
    );
    assert!(
        !frames
            .iter()
            .any(|f| matches!(f, ClientFrame::TermClose { .. })),
        "and the way out is a DETACH — nothing that ends anything left the head: {frames:?}"
    );
    assert!(
        !a.pane_open(),
        "the detach is taken in the same read: the rectangle is back"
    );
    assert!(
        a.holds_pane(),
        "and the pane is still held — the program keeps running on the daemon's pty"
    );
}

/// **A way-out byte in a read the COMPOSER consumed is not a way out of anything.**
///
/// The other edge of the coalesced question, and the one the head's own scan gets wrong:
/// `0x1c` can be in a read with no pane in it and no pane opening from it — a paste of a
/// file that contains a literal `0x1c` (FS is a real byte in real files), or a burst a
/// terminal coalesced out of a fast hand. The bytes of that read are the composer's: the
/// paste is text, nothing submits, no pane opens. **Nothing should be kept.**
///
/// What the head's own second scan (`raw.contains(&WAY_OUT)`, beside [`App::pane_keys`]'s
/// scan — the same fact recognised in two places) does instead is keep it: the latch arms
/// on a byte no pane will ever see, and it is spent on the NEXT pane that opens. Measured
/// as the test below: the operator comes back with `!term` — the attach protocol 32 exists
/// for exactly this — and the pane opens already detached, with the rectangle gone before
/// a single frame was drawn. From the seat: *I can never get back in.* The one recogniser
/// must be the one that also owns the forwarding ([`App::pane_keys`]), and a read whose
/// keys were the composer's has no way out in it at all.
#[test]
fn a_way_out_byte_in_a_read_the_composer_consumed_is_not_a_way_out_of_anything() {
    let path = socket_path("phantom");
    let frames = scripted_daemon(path.clone());
    let mut a = app();
    let mut link = Link::open(&path, SESSION, 0, "tui", "wayout-phantom").expect("attach");
    attach(&mut link, &mut a, &frames);

    // The operator is at the composer: no pane, none opening. A paste of text that
    // contains a literal `0x1c` — one read, one `Key::Paste`, nothing submitted.
    step(&mut link, &mut a, b"\x1b[200~notes\x1c\x1b[201~");
    settle_until(&mut link, &mut a, "the paste in the composer", |a| {
        a.input().contains("notes")
    });
    assert!(!a.pane_open(), "a paste opens nothing");
    assert!(
        !clear(&frames)
            .iter()
            .any(|f| matches!(f, ClientFrame::TermOpen { .. })),
        "and sends nothing: the paste is composer text"
    );

    // The pasted text is submitted as the ordinary line it is — a prompt, not a pane — and
    // the composer is empty again. **The submit is also the barrier**: it is a frame the daemon
    // reads, so everything this head wrote before it is in the collection below, and a
    // `TermOpen` the paste had produced could not hide behind it.
    step(&mut link, &mut a, b"\r");
    settle_until(&mut link, &mut a, "the composer to empty", |a| {
        a.input().is_empty()
    });
    let submitted = frames_until(&frames, "the paste to be submitted", |fs| {
        fs.iter().any(|f| matches!(f, ClientFrame::Prompt { .. }))
    });
    assert!(
        !submitted
            .iter()
            .any(|f| matches!(f, ClientFrame::TermOpen { .. })),
        "a paste with a `0x1c` in it opened a pane: {submitted:?}"
    );
    assert!(!a.pane_open(), "submitting the paste opened no pane");

    // Only now does the operator open one — and the poisoned latch spends itself on it.
    step(&mut link, &mut a, b"!term nano\r");
    settle_until(&mut link, &mut a, "the pane's screen", |a| {
        a.screen(100, 30)
            .iter()
            .any(|r| r.contains("nano's screen"))
    });
    let opened = frames_until(&frames, "the open to go out", |fs| {
        fs.iter().any(|f| matches!(f, ClientFrame::TermOpen { .. }))
    });
    assert!(
        opened
            .iter()
            .any(|f| matches!(f, ClientFrame::TermOpen { line, .. } if line == "!term nano")),
        "the open went out: {opened:?}"
    );
    assert!(
        a.pane_open(),
        "the rectangle is drawn: a way out nobody pressed must not spend itself on the \
         pane that just opened"
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
    attach(&mut link, &mut a, &frames);

    // One read, exactly as `Terminal::events` would have consumed it.
    step(&mut link, &mut a, b"!term nano\r\x1c");
    settle_until(&mut link, &mut a, "the rectangle to be back", |a| {
        !a.pane_open()
    });

    // **The positive frame is what makes the absence exact — for what came BEFORE it**: the
    // `TermOpen` is a frame the daemon reads, so a `TermClose` written before it would be in front
    // of it in this collection, a unix socket being read in order. The way out's own frame would
    // be written AFTER it (one tick, one `actions` vector, the open first), so the collection is
    // taken through the barrier and over the window after it. See [`frames_until_and_after`].
    let frames = frames_until_and_after(&frames, "the pane to open", |fs| {
        fs.iter().any(|f| matches!(f, ClientFrame::TermOpen { .. }))
    });
    assert!(
        frames
            .iter()
            .any(|f| matches!(f, ClientFrame::TermOpen { .. })),
        "the pane opened: {frames:?}"
    );
    assert!(
        !frames
            .iter()
            .any(|f| matches!(f, ClientFrame::TermClose { .. })),
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
    attach(&mut link, &mut a, &frames);

    step(&mut link, &mut a, b"!term nano\r");
    settle_until(&mut link, &mut a, "the pane's screen", |a| {
        a.screen(100, 30)
            .iter()
            .any(|r| r.contains("nano's screen"))
    });
    step(&mut link, &mut a, b"\x1c");
    settle_until(&mut link, &mut a, "the rectangle to be back", |a| {
        !a.pane_open()
    });
    assert!(!a.pane_open(), "detached");
    assert!(
        !arrived_meanwhile(&frames)
            .iter()
            .any(|f| matches!(f, ClientFrame::TermClose { .. })),
        "the detach ended nothing"
    );

    step(&mut link, &mut a, b"!term\r");
    settle_until(&mut link, &mut a, "the pane's screen, replayed", |a| {
        a.screen(100, 30)
            .iter()
            .any(|r| r.contains("nano's screen"))
    });

    let again = frames_until(&frames, "the attach to go out", |fs| {
        fs.iter().any(|f| matches!(f, ClientFrame::TermOpen { .. }))
    });
    assert!(
        again
            .iter()
            .any(|f| matches!(f, ClientFrame::TermOpen { line, .. } if line == "!term")),
        "a bare `!term` is an attach, and the line goes over as typed: {again:?}"
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
    attach(&mut link, &mut a, &frames);

    step(&mut link, &mut a, b"!term nano\r");
    settle_until(&mut link, &mut a, "the pane's screen", |a| {
        a.screen(100, 30)
            .iter()
            .any(|r| r.contains("nano's screen"))
    });
    step(&mut link, &mut a, b"\x1c");
    settle_until(&mut link, &mut a, "the rectangle to be back", |a| {
        !a.pane_open()
    });
    let _ = clear(&frames);

    // The verb, submitted. Nothing is sent, and the card is up naming what would end.
    step(&mut link, &mut a, b"!term close\r");
    settle_until(&mut link, &mut a, "the card", |a| {
        a.screen(100, 30).iter().any(|r| r.contains("end the pane"))
    });
    // **A verb that asks writes nothing**, so the absence is the only claim here and a window is
    // its only instrument — see [`ABSENCE_WINDOW`].
    let asked = arrived_meanwhile(&frames);
    assert!(
        !asked
            .iter()
            .any(|f| matches!(f, ClientFrame::TermClose { .. })),
        "the verb ended the program on its own — that is what the confirmation exists to stop: \
         {asked:?}"
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
    settle_until(&mut link, &mut a, "the card to be gone", |a| {
        !a.screen(100, 30).iter().any(|r| r.contains("end the pane"))
    });
    let cancelled = arrived_meanwhile(&frames);
    assert!(
        !cancelled
            .iter()
            .any(|f| matches!(f, ClientFrame::TermClose { .. })),
        "the cancel sent the frame anyway: {cancelled:?}"
    );
    assert!(
        a.holds_pane(),
        "and the program is still running, which is what the cancel is for"
    );

    // Ask again, and this time say yes.
    step(&mut link, &mut a, b"!term close\r");
    settle_until(&mut link, &mut a, "the card", |a| {
        a.screen(100, 30).iter().any(|r| r.contains("end the pane"))
    });
    let _ = clear(&frames);
    step(&mut link, &mut a, b"y");
    settle_until(&mut link, &mut a, "the ending to be a row", |a| {
        a.screen(100, 30)
            .iter()
            .any(|r| r.contains("you closed the terminal"))
    });

    let ended = frames_until(&frames, "the confirmed close to leave the head", |fs| {
        fs.iter()
            .any(|f| matches!(f, ClientFrame::TermClose { .. }))
    });
    assert!(
        ended
            .iter()
            .any(|f| matches!(f, ClientFrame::TermClose { .. })),
        "the confirmed close never left the head: {ended:?}"
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
    attach(&mut link, &mut a, &frames);

    step(&mut link, &mut a, b"hello\x1c");
    settle_until(&mut link, &mut a, "the bytes in the composer", |a| {
        a.input() == "hello"
    });

    assert_eq!(a.input(), "hello", "the byte is not a character");
    // Nothing was written, so nothing can be waited for: a window, and [`ABSENCE_WINDOW`] says why.
    let frames = arrived_meanwhile(&frames);
    assert!(
        !frames
            .iter()
            .any(|f| matches!(f, ClientFrame::TermClose { .. })),
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
