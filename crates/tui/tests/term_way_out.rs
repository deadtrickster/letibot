//! The pane's way out, driven through the driver's own loop: `ctrl-\` is a detach and nothing
//! leaves the head, while `!term close` asks first and sends its frame on `y` and nothing else.
//! Every claim is asserted on what the socket carried, not on a screen.

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

/// A daemon drawn on the wire: it answers `TermOpen` with a screen, `TermClose` with the ending,
/// and `TermStatus` with what it is holding - the read the head asks on `Hello` and before a
/// `!term close` it cannot answer itself. Every frame the head sent is handed back to the test.
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
                    // The verb's argument, as the daemon names it in its own status answer  -
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
                    // The ending the daemon composes for the operator's own act - the same
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

/// How long a wait for a fact about two threads in THIS process may take before it is a hang: the
/// head and the scripted daemon are both here, so a frame is one socket write and one wake-up, and
/// this number is far above anything a busy box can add to those.
const PATIENCE: Duration = Duration::from_secs(60);

/// How long the daemon is given to read a frame the head did NOT write. An absence can only be
/// watched for, and the head's write is synchronous, so this window is the daemon thread's chance
/// to read what was written - the one wait in this file that is a window and not a condition.
const ABSENCE_WINDOW: Duration = Duration::from_secs(2);

/// Pump the driver's loop until the head has got where the test is going - the condition, not a
/// pause: ticking until passes change nothing is a bet that the daemon thread is scheduled within
/// a hundred milliseconds, and the gate's box lost it. `&mut App` because drawing records the hits.
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

/// Everything the daemon read, up to the frame `ready` names - so *this reached the daemon*
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

/// The frames up to the barrier `ready` names, AND whatever arrives after it: a positive frame makes
/// an absence exact only for what was written BEFORE it, and `Link::tick` appends the way-out's
/// actions to the Enter's in one vector, so the frame a defect emits is always the next one.
fn frames_until_and_after(
    frames: &Receiver<ClientFrame>,
    what: &str,
    ready: impl Fn(&[ClientFrame]) -> bool,
) -> Vec<ClientFrame> {
    let mut out = frames_until(frames, what, ready);
    out.extend(arrived_meanwhile(frames));
    out
}

/// What the daemon read while the head wrote nothing - see [`ABSENCE_WINDOW`].
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

/// Whatever is already in the channel, without waiting - for the places a test clears the board.
fn clear(frames: &Receiver<ClientFrame>) -> Vec<ClientFrame> {
    let mut out = Vec::new();
    while let Ok(f) = frames.try_recv() {
        out.push(f);
    }
    out
}

/// Wait until the head and the daemon have met: a `TermStatus` in the channel is a full round trip,
/// since the `Hello` was on the socket before it. The loop is pumped while it waits, because the
/// ask is queued by the `Hello` and written by the next `tick`.
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

/// `ctrl-\` detaches, and NOTHING leaves the head - no `TermClose` on the socket, which is the
/// whole of the change. What replaces it is asserted twice on the screen: the pane is still held,
/// so the program is alive and `!term` has something to come back to, and the chrome says so.
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

    // The absence, and it is the one claim here that needs a window: a detach writes nothing,
    // so there is no frame to wait for - see [`ABSENCE_WINDOW`].
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

/// Enter and the way out in ONE read: BOTH effects, not one of them. A coalesced `\r\x1c` is one
/// read of the rule, so the `\r` must reach the program AND the detach must be taken - the defect
/// is the silent winner, where one of the two eats the other. Both are asserted, never one alone.
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

    // Both halves come out of one collection: the Enter is a frame the daemon reads, so a unix
    // socket's ordering speaks for what came before it - and the defect writes after it, so the
    // collection runs over the window too. See [`frames_until_and_after`].
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

/// A way-out byte in a read the COMPOSER consumed is not a way out of anything: `0x1c` can be in a
/// paste, and those bytes are the composer's, so nothing may be kept. The recogniser must be the
/// one that also owns the forwarding, or the latch arms on a byte no pane sees and eats the next one.
#[test]
fn a_way_out_byte_in_a_read_the_composer_consumed_is_not_a_way_out_of_anything() {
    let path = socket_path("phantom");
    let frames = scripted_daemon(path.clone());
    let mut a = app();
    let mut link = Link::open(&path, SESSION, 0, "tui", "wayout-phantom").expect("attach");
    attach(&mut link, &mut a, &frames);

    // The operator is at the composer: no pane, none opening. A paste of text that
    // contains a literal `0x1c` - one read, one `Key::Paste`, nothing submitted.
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

    // The pasted text is submitted as the ordinary line it is, a prompt and not a pane, and the
    // composer is empty again. The submit is also the barrier: it is a frame the daemon reads, so
    // everything written before it is in the collection below.
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

    // Only now does the operator open one - and the poisoned latch spends itself on it.
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

/// The byte that opens the pane and the byte that leaves it can arrive in one read: with no pane
/// when the tick began, the whole stream went down `App::key` and the decoder dropped `0x1c` on its
/// `_ => i += 1` arm, so the pane opened with no way out. The way out is looked for on the stream.
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

    // The positive frame makes the absence exact for what came BEFORE it: the `TermOpen` is a frame
    // the daemon reads, so a `TermClose` written earlier would be in front of it here. The way out's
    // own frame would come after, so the collection runs over the window too.
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

/// `!term` attaches back to the SAME run, through the driver's loop: the head sends the bare verb,
/// the daemon answers with what it is holding and replays the screen, and the rectangle is drawn
/// again - which is the whole of the detach having ended nothing.
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

/// The deliberate close asks on the socket too, and only a `y` sends the frame: `!term close` typed
/// and submitted sends nothing, so the daemon is never asked to end a program because somebody
/// typed a verb. The cancel half is the default - Esc leaves it running and sends no frame.
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
    // A verb that asks writes nothing, so the absence is the only claim here and a window is
    // its only instrument - see [`ABSENCE_WINDOW`].
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

    // Esc is the cancel - the safe default - and it sends nothing.
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
    // The dim register: the operator chose this ending, so it is housekeeping rather
    // than the answer to something they just typed.
    assert!(
        frame
            .iter()
            .any(|r| r.trim_start().starts_with('·') && r.contains("!term nano")),
        "an ending the operator chose is the housekeeping register: {frame:?}"
    );
}

/// And the way-out byte is still not a key. The control for the three above: with no
/// pane and no pane opening, the same byte is nothing at all - it is not typed, it does not
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
