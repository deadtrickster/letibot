//! `!term` — the pane's byte stream, through the frames, with no daemon and no pty.
//!
//! The operator's ask, in their words: *"i mean i want it broooo"* — `! mc`, `! nano` running
//! **in the pane**, the conversation's rectangle given to the program with the composer keeping
//! its rows. `crates/tools/src/exec/terminal.rs` refuses those by name today and its own
//! message calls the fix `!term`.
//!
//! # What this file is for, and what it deliberately is not
//!
//! The pty itself has its own tests in `letibot-tools` (a real child on a real pty: the
//! controlling terminal, keys in and bytes back, `TIOCSWINSZ`, the ending), and the screen has
//! its own in `letibot-ui` (a byte stream turned into exactly `room` rows). **What cannot be
//! seen from either half is the wire between them**, and that is this file:
//!
//! 1. **The six frames survive the wire**, byte for byte — including the one thing a JSON
//!    round trip could quietly change, which is a `Vec<u8>` that comes back as a string or as
//!    a list of the wrong numbers.
//! 2. **The daemon hands the driver the command and the rectangle**, and the driver's bytes
//!    reach the head as [`ServerFrame::TermOutput`] — the whole path from `ClientFrame` to
//!    `ServerFrame` through a canned driver, so the framing is asserted and not the pty.
//! 3. **Keys go down verbatim.** `ESC O A` is the application-cursor spelling of *up*, and a
//!    head that decoded and re-encoded it would send `ESC [ A` — a different byte string to a
//!    program that asked for the first. The assertion is on the bytes.
//! 4. **A pane is not a command.** No row, no seq, nothing on the queue. This is the frame
//!    class `Screen` and `Secret` belong to, and the reason is sharper here than anywhere: a
//!    keystroke that queued behind a running turn is a key that arrives after the thing it was
//!    answering.
//! 5. **Ending a pane is the deliberate act, and a detach is the absence of a frame.**
//!    `ctrl-\` used to send `TermClose`, so leaving `nano` killed it; it now sends **nothing**,
//!    and the only act that reaches the driver's `close` is the head's `!term close` — after the
//!    operator confirmed it. A pane that never started is the same frame as one that ended.
//! 6. **A bare `!term` attaches**: it reaches the driver's `attach` and not its `open`, the
//!    rectangle it carries is the *attaching* head's, and what comes back is the name of what is
//!    running followed by the screen the daemon kept. The screen itself is the daemon's — see
//!    `letibot_harnessd`'s `term` module for why, and for what a capped log costs.
//! 7. **The status read answers what is running, or nothing** — the fact a head draws when it is
//!    not drawing the pane, which is a read and not a row because a detach is not an event.
//!
//! **What is not here, and cannot be:** the *feel*. Whether `nano` is usable, whether the
//! rectangle is the right rectangle, whether ctrl-\ is where a person's fingers go — none of
//! that is answerable without a live head on a real terminal, and the operator is the one who
//! drives it.

use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};

use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::protocol::{Caps, ClientFrame, PROTOCOL_VERSION, ServerFrame};
use letibot_sessionlog::registry::{Registry, SessionWiring, TerminalDriver};
use letibot_sessionlog::server::serve_conn;
use letibot_sessionlog::term_command;
use letibot_sessionlog::wire::{FrameReader, FrameWriter};

/// **A driver that runs nothing and remembers everything.** It is a `TerminalDriver` and not
/// `harnessd`'s `Terminals` on purpose: this file is about the seam between the daemon and
/// whoever owns a pty, so the pty — which has its own tests over a real child — is replaced by
/// the case we care about, which is *the daemon asked, with this line and this rectangle*, and
/// *these bytes went up*.
#[derive(Default)]
struct Canned {
    opened: Mutex<Vec<(String, String, usize, usize)>>,
    /// **What `attach` was told**, in order: (session, cols, rows) — the sibling of `opened`,
    /// and separate from it so a test can assert which of the two a line reached.
    attached: Mutex<Vec<(String, usize, usize)>>,
    /// **What the pane this session has is running.** `None` is *this session has no pane*,
    /// which a real driver reports as an `Err` — the same sentence a pane that could not start
    /// uses, because the head's act is the same either way.
    running: Mutex<Option<String>>,
    input: Mutex<Vec<Vec<u8>>>,
    resized: Mutex<Vec<(usize, usize)>>,
    closed: Mutex<Vec<String>>,
    /// The hub `open` was handed, so a test can push bytes the way a pty's reader thread
    /// would. `None` until a pane has been opened.
    hub: Mutex<Option<Arc<Hub>>>,
    /// What `open` answers. `None` is a pane that started.
    refuse: Mutex<Option<String>>,
}

impl Canned {
    fn new() -> Arc<Canned> {
        Arc::new(Canned::default())
    }

    /// What `open` was told, in order: (session, command, cols, rows).
    fn opened(&self) -> Vec<(String, String, usize, usize)> {
        self.opened.lock().unwrap().clone()
    }

    /// What `attach` was told, in order: (session, cols, rows).
    fn attached(&self) -> Vec<(String, usize, usize)> {
        self.attached.lock().unwrap().clone()
    }

    fn input(&self) -> Vec<Vec<u8>> {
        self.input.lock().unwrap().clone()
    }

    fn resized(&self) -> Vec<(usize, usize)> {
        self.resized.lock().unwrap().clone()
    }

    fn closed(&self) -> Vec<String> {
        self.closed.lock().unwrap().clone()
    }

    /// **What the pty's reader thread would do**: a frame that is not the record, straight to
    /// the session's heads.
    fn say(&self, bytes: &[u8]) {
        let hub = self.hub.lock().unwrap().clone().expect("a pane was opened");
        hub.push_frame(ServerFrame::TermOutput {
            bytes: bytes.to_vec(),
        });
    }

    fn end(&self, reason: &str) {
        let hub = self.hub.lock().unwrap().clone().expect("a pane was opened");
        hub.push_frame(ServerFrame::TermEnded {
            reason: reason.to_string(),
        });
    }
}

impl TerminalDriver for Canned {
    fn open(
        &self,
        session_id: &str,
        hub: &Arc<Hub>,
        command: &str,
        cols: usize,
        rows: usize,
    ) -> Result<(), String> {
        self.opened
            .lock()
            .unwrap()
            .push((session_id.to_string(), command.to_string(), cols, rows));
        if let Some(why) = self.refuse.lock().unwrap().clone() {
            return Err(why);
        }
        *self.hub.lock().unwrap() = Some(hub.clone());
        Ok(())
    }

    fn attach(
        &self,
        session_id: &str,
        hub: &Arc<Hub>,
        cols: usize,
        rows: usize,
    ) -> Result<(), String> {
        self.attached
            .lock()
            .unwrap()
            .push((session_id.to_string(), cols, rows));
        let Some(command) = self.running.lock().unwrap().clone() else {
            return Err(
                "this session has no pane to attach to — `!term COMMAND` starts one. Nothing \
                 was attached."
                    .to_string(),
            );
        };
        *self.hub.lock().unwrap() = Some(hub.clone());
        // **What a real driver does, in the order it does it**: what is running, and then the
        // screen it has kept. Both are `push_frame`s, so this is the same door the pty's reader
        // thread uses and the ordering is the one a head sees.
        hub.push_frame(ServerFrame::TermAttached { command });
        hub.push_frame(ServerFrame::TermOutput {
            bytes: b"\x1b[2J\x1b[Hmc's screen".to_vec(),
        });
        Ok(())
    }

    /// **What a real driver answers**: the command it is holding, or nothing. The canned half
    /// keeps the same field `attach` reads, so a test cannot make the two disagree.
    fn status(&self, _session_id: &str) -> Option<String> {
        self.running.lock().unwrap().clone()
    }

    fn input(&self, _session_id: &str, bytes: &[u8]) -> Result<(), String> {
        self.input.lock().unwrap().push(bytes.to_vec());
        Ok(())
    }

    fn resize(&self, _session_id: &str, cols: usize, rows: usize) -> Result<(), String> {
        self.resized.lock().unwrap().push((cols, rows));
        Ok(())
    }

    fn close(&self, session_id: &str) -> Result<(), String> {
        self.closed.lock().unwrap().push(session_id.to_string());
        Ok(())
    }
}

fn start(driver: Option<Arc<Canned>>) -> Arc<Registry> {
    let registry = Registry::new();
    if let Some(d) = driver {
        registry.set_terminal(d);
    }
    registry
        .create(
            "s-1",
            "one",
            SessionWiring {
                model: "qwen3-4b".into(),
                dialect: "qwen".into(),
                endpoint: "127.0.0.1:8080".into(),
                workspace: "/tmp/ws".into(),
            },
        )
        .expect("create");
    registry
}

/// Attach a head to `s-1` and hand back the wire.
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

/// Read until a pane frame, skipping the session's own traffic.
///
/// **Every pane frame counts**: the bytes, the ending, the name of what is running
/// (`TermAttached`, which a bare `!term` is answered with before the bytes) and the answer to
/// the status read. A helper that skipped one of them would make the tests that read a
/// *sequence* of pane frames flaky in the direction that reads as a wrong assertion.
fn until_term(r: &mut FrameReader<UnixStream>) -> ServerFrame {
    loop {
        match r.read::<ServerFrame>().expect("a frame") {
            f @ (ServerFrame::TermAttached { .. }
            | ServerFrame::TermOutput { .. }
            | ServerFrame::TermEnded { .. }
            | ServerFrame::TermStatus { .. }) => return f,
            ServerFrame::Event(_) => continue,
            other => panic!("expected a pane frame, got {other:?}"),
        }
    }
}

/// **The pane's frames survive the wire, byte for byte.**
///
/// The one thing a JSON round trip can quietly change is a byte vector: `serde_json` writes it
/// as an array of integers, and a reader that got back a string, a list of characters or a list
/// of the wrong numbers would be a pane that draws something other than what the program wrote.
/// So the assertion is on the **bytes**, including the ones a text-shaped codec would eat —
/// `ESC`, a NUL, and half of a UTF-8 character, which is exactly what a read that ends
/// mid-character delivers.
#[test]
fn the_pane_frames_survive_the_wire_byte_for_byte() {
    let raw: Vec<u8> = vec![0x1b, b'[', b'2', b'J', 0x00, 0xe2, 0x94]; // ED, NUL, half a '─'
    let down = [
        ClientFrame::TermOpen {
            line: "!term mc /etc".into(),
            cols: 97,
            rows: 23,
        },
        ClientFrame::TermInput { bytes: raw.clone() },
        ClientFrame::TermResize { cols: 97, rows: 23 },
        ClientFrame::TermStatus,
        ClientFrame::TermClose,
    ];
    for f in down {
        let json = serde_json::to_string(&f).expect("encode");
        let back: ClientFrame = serde_json::from_str(&json).expect("decode");
        assert_eq!(back, f, "a client frame did not survive the wire: {json}");
    }
    let up = [
        ServerFrame::TermAttached {
            command: "mc /etc".into(),
        },
        ServerFrame::TermOutput { bytes: raw.clone() },
        ServerFrame::TermEnded {
            reason: "the program exited with 0".into(),
        },
        // **The read's answer in both of its states**, because `None` is the one a codec is
        // most likely to eat: an `Option` that came back as `""` would make *no pane* look
        // like *a pane running the empty command*.
        ServerFrame::TermStatus {
            command: Some("mc /etc".into()),
        },
        ServerFrame::TermStatus { command: None },
    ];
    for f in up {
        let json = serde_json::to_string(&f).expect("encode");
        let back: ServerFrame = serde_json::from_str(&json).expect("decode");
        assert_eq!(back, f, "a server frame did not survive the wire: {json}");
    }
    // And the bytes really are bytes, not a lossy string: the NUL and the half-character are
    // both still there.
    let back: ServerFrame = serde_json::from_str(
        &serde_json::to_string(&ServerFrame::TermOutput { bytes: raw }).unwrap(),
    )
    .unwrap();
    match back {
        ServerFrame::TermOutput { bytes } => {
            assert_eq!(bytes, vec![0x1b, 0x5b, 0x32, 0x4a, 0, 0xe2, 0x94])
        }
        other => panic!("not TermOutput: {other:?}"),
    }
}

/// **`!term` reaches the driver with the command and the conversation's rectangle**, and the
/// program's bytes come back as `TermOutput` — the whole path, through a canned driver.
///
/// The rectangle is the head's fact and the daemon has no screen, so this is the only place the
/// two numbers can be checked against each other: `97×23` in, `97×23` at the driver, which is
/// what becomes the pty's `winsize` before the program's first byte.
#[test]
fn the_verb_the_command_and_the_rectangle_reach_the_driver() {
    let driver = Canned::new();
    let registry = start(Some(driver.clone()));
    let (mut w, mut r) = attach(&registry);

    w.write(&ClientFrame::TermOpen {
        line: "!term mc /etc".into(),
        cols: 97,
        rows: 23,
    })
    .expect("open");

    // The driver's `open` runs on the connection's reader thread, so give it the moment it
    // needs before pushing what the pty's own thread would have pushed after it.
    for _ in 0..200 {
        if !driver.opened().is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }

    // What the pty's reader thread would push a moment later.
    driver.say(b"\x1b[2J\x1b[Hmc's screen\r\n");
    assert_eq!(
        until_term(&mut r),
        ServerFrame::TermOutput {
            bytes: b"\x1b[2J\x1b[Hmc's screen\r\n".to_vec()
        }
    );
    assert_eq!(
        driver.opened(),
        vec![("s-1".to_string(), "mc /etc".to_string(), 97usize, 23usize)],
        "the driver must get the command with the verb stripped and the pane's own rectangle"
    );
}

/// **Keys go down verbatim, as bytes.** `ESC O A` is the application-cursor spelling of *up*,
/// and a head that decoded it into `Key::Up` and re-encoded it would send `ESC [ A` — a
/// different byte string to a program that asked for the first, and the reason
/// [`ClientFrame::TermInput`] carries a vector rather than a keycode.
#[test]
fn keys_are_forwarded_as_bytes_and_are_not_decoded() {
    let driver = Canned::new();
    let registry = start(Some(driver.clone()));
    let (mut w, _r) = attach(&registry);
    w.write(&ClientFrame::TermOpen {
        line: "!term nano notes.txt".into(),
        cols: 80,
        rows: 24,
    })
    .expect("open");

    // An application-cursor arrow, a control byte, a plain letter, and half a UTF-8
    // character — every shape a keystroke read can have.
    let keys: Vec<u8> = vec![0x1b, b'O', b'A', 0x03, b'x', 0xe2];
    w.write(&ClientFrame::TermInput {
        bytes: keys.clone(),
    })
    .expect("keys");
    w.write(&ClientFrame::TermResize {
        cols: 120,
        rows: 40,
    })
    .expect("resize");

    // The reader thread is the connection's, so give it the moment it needs and then assert.
    for _ in 0..200 {
        if !driver.input().is_empty() && !driver.resized().is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(
        driver.input(),
        vec![keys],
        "the keys must arrive exactly as the operator's terminal produced them"
    );
    assert_eq!(driver.resized(), vec![(120, 40)]);
}

/// **A pane is not a command.** No row, no seq, nothing on the queue — the frame class
/// `Screen` and `Secret` belong to, and the reason is sharper here than anywhere else: a
/// keystroke that queued behind a running turn would be a key that arrives after the thing it
/// was answering.
#[test]
fn a_pane_writes_no_row_and_moves_no_seq() {
    let driver = Canned::new();
    let registry = start(Some(driver.clone()));
    let (mut w, _r) = attach(&registry);
    let hub = registry.resolve("s-1").expect("the session");
    let before = hub.head_seq();
    let items_before = hub.snapshot().items.len();

    w.write(&ClientFrame::TermOpen {
        line: "!term top".into(),
        cols: 80,
        rows: 24,
    })
    .expect("open");
    w.write(&ClientFrame::TermInput {
        bytes: b"q".to_vec(),
    })
    .expect("keys");
    w.write(&ClientFrame::TermClose).expect("close");
    for _ in 0..200 {
        if !driver.closed().is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }

    assert_eq!(
        hub.head_seq(),
        before,
        "a pane must move no seq: its repaints are not the record"
    );
    assert_eq!(
        hub.snapshot().items.len(),
        items_before,
        "a pane must write no row: the transcript comes back exactly as it was"
    );
    assert!(
        hub.try_command().is_none(),
        "a pane must queue nothing: a keystroke behind a turn is not a keystroke"
    );
}

/// **Ending the pane is the deliberate act, and it is the daemon's.**
///
/// `TermClose` is the frame a head sends only after its `!term close` was confirmed by the
/// operator — `ctrl-\` detaches and sends nothing (see
/// [`a_detach_sends_nothing_and_leaves_the_pane_live`]). It reaches the driver, the driver ends
/// the pane's scope, and the ending arrives as `TermEnded` with the operator's own act in the
/// sentence. The program never sees a byte of it, which is why there is no `TermInput` here.
#[test]
fn the_deliberate_close_ends_the_pane_and_never_reaches_the_program() {
    let driver = Canned::new();
    let registry = start(Some(driver.clone()));
    let (mut w, mut r) = attach(&registry);
    w.write(&ClientFrame::TermOpen {
        line: "!term mc".into(),
        cols: 80,
        rows: 24,
    })
    .expect("open");
    // The ending, as the head would report it: a close and no key.
    w.write(&ClientFrame::TermClose).expect("close");
    for _ in 0..200 {
        if !driver.closed().is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(driver.closed(), vec!["s-1".to_string()]);
    assert!(
        driver.input().is_empty(),
        "the ending must not be a byte the program could read, or trap"
    );

    // And the ending the operator reads is the one the pane composes, not a second one.
    driver.end("you closed the terminal");
    assert_eq!(
        until_term(&mut r),
        ServerFrame::TermEnded {
            reason: "you closed the terminal".into()
        }
    );
}

/// **A detach is the ABSENCE of a frame, and the pane is still live afterwards.**
///
/// This is the operator's *"but i dont want it to exit"*, as a test. `ctrl-\` used to send
/// `TermClose`, so leaving `nano` killed it and the attach work (protocol 32) bought nothing.
/// Leaving now sends **nothing at all**, which is why there is no frame to assert on — what this
/// test asserts is that a head that goes quiet changes nothing:
///
/// * the driver is **never told to close** (so no scope is ended and no program is killed);
/// * and the read a head makes when it comes back — `TermStatus` — still answers with the same
///   command, which is the daemon's own statement that the slot is occupied and the program is
///   running.
///
/// **The frame that is missing is the subject.** A test that asserted *no `TermClose`* against a
/// stream the head never wrote would be asserting about a socket, so the positive half is the
/// status read: the pane is still there, and it is the same pane.
#[test]
fn a_detach_sends_nothing_and_leaves_the_pane_live() {
    let driver = Canned::new();
    *driver.running.lock().unwrap() = Some("nano notes.txt".into());
    let registry = start(Some(driver.clone()));
    let (mut w, mut r) = attach(&registry);

    w.write(&ClientFrame::TermOpen {
        line: "!term nano notes.txt".into(),
        cols: 80,
        rows: 24,
    })
    .expect("open");
    for _ in 0..200 {
        if !driver.opened().is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }

    // The operator pressed `ctrl-\`: the head hides the rectangle and writes nothing. What the
    // head does next — a moment or an hour later — is ask whether the program is still there.
    w.write(&ClientFrame::TermStatus).expect("status");
    match until_term(&mut r) {
        ServerFrame::TermStatus { command } => assert_eq!(
            command.as_deref(),
            Some("nano notes.txt"),
            "a detach must leave the pane running the same program"
        ),
        other => panic!("expected TermStatus, got {other:?}"),
    }
    assert!(
        driver.closed().is_empty(),
        "a detach must end nothing — the only act that reaches `close` is a deliberate one: {:?}",
        driver.closed()
    );
    assert_eq!(
        driver.opened().len(),
        1,
        "and coming back must not START a second program: {:?}",
        driver.opened()
    );
}

/// **`!term close` is not a command to run** — the daemon refuses it by name, and the sentence
/// names the spelling that does run a program called `close`.
///
/// The bare word is the ending and it is the **head's** act: the head asks the operator to
/// confirm it and then sends `TermClose`. A `TermOpen` carrying the same line is therefore a head
/// that did not do that, and running a program named `close` instead would be the two halves
/// disagreeing about what one line means. The cost of the word — a program called `close` with no
/// arguments cannot be started by the shortest spelling — is paid in the open, in this sentence.
#[test]
fn a_term_close_line_is_refused_by_name_and_starts_nothing() {
    let driver = Canned::new();
    let registry = start(Some(driver.clone()));
    let (mut w, mut r) = attach(&registry);

    w.write(&ClientFrame::TermOpen {
        line: "!term close".into(),
        cols: 80,
        rows: 24,
    })
    .expect("open");
    match until_term(&mut r) {
        ServerFrame::TermEnded { reason } => {
            assert!(
                reason.contains("!term command close"),
                "the refusal must name how to run a program called `close`: {reason}"
            );
            assert!(
                reason.contains("ENDED"),
                "and it must say what the word is for: {reason}"
            );
        }
        other => panic!("expected TermEnded, got {other:?}"),
    }
    assert!(
        driver.opened().is_empty(),
        "nothing may be started for the ending's spelling: {:?}",
        driver.opened()
    );
    assert!(
        driver.closed().is_empty(),
        "and a TermOpen is not an ending either — the head sends `TermClose` for that"
    );
}

/// **The status read answers what is running, or nothing.**
///
/// The read behind a head's own line about a program it is not drawing: `Some(command)` for a
/// live pane and `None` for a session with none. **`None` is not an error** — it is the ordinary
/// state of a session nobody has run `!term` in, and it is what makes a head draw nothing at all
/// rather than a row.
#[test]
fn the_status_read_answers_what_is_running_or_nothing() {
    let driver = Canned::new();
    let registry = start(Some(driver.clone()));
    let (mut w, mut r) = attach(&registry);

    // Nothing running: the honest answer, and not a refusal.
    w.write(&ClientFrame::TermStatus).expect("status");
    assert_eq!(
        until_term(&mut r),
        ServerFrame::TermStatus { command: None },
        "a session with no pane answers with nothing, not with a sentence"
    );

    // A pane, and the daemon's own string for what it is running.
    *driver.running.lock().unwrap() = Some("mc /etc".into());
    w.write(&ClientFrame::TermStatus).expect("status");
    assert_eq!(
        until_term(&mut r),
        ServerFrame::TermStatus {
            command: Some("mc /etc".into())
        },
        "and a live pane is named with the command the daemon was handed at `TermOpen`"
    );
}

/// **A pane that never started is the same frame as one that ended**, and the sentence is the
/// whole of the difference — so a refusal is never silence.
///
/// Three refusals, in the three places one can happen: a line that is not the verb, a session
/// with no pane to attach to, and a daemon with no driver at all.
#[test]
fn a_pane_that_cannot_start_says_so_in_one_sentence() {
    // A daemon with no driver: the daemon says so rather than pretending to run something.
    let registry = start(None);
    let (mut w, mut r) = attach(&registry);
    w.write(&ClientFrame::TermOpen {
        line: "!term mc".into(),
        cols: 80,
        rows: 24,
    })
    .expect("open");
    match until_term(&mut r) {
        ServerFrame::TermEnded { reason } => assert!(
            reason.contains("no terminal driver"),
            "a daemon with no driver must say so: {reason}"
        ),
        other => panic!("expected TermEnded, got {other:?}"),
    }

    // A line that is not the verb at all, and — now — the verb with nothing after it, which is
    // an ATTACH and refuses only because this canned driver has no pane.
    let driver = Canned::new();
    let registry = start(Some(driver.clone()));
    let (mut w, mut r) = attach(&registry);
    for (line, needle) in [
        ("!terminal x", "not a `!term` line"),
        ("!term", "no pane to attach to"),
        ("!term   ", "no pane to attach to"),
    ] {
        w.write(&ClientFrame::TermOpen {
            line: line.into(),
            cols: 80,
            rows: 24,
        })
        .expect("open");
        match until_term(&mut r) {
            ServerFrame::TermEnded { reason } => assert!(
                reason.contains(needle),
                "`{line}` must be refused with a sentence naming why, got: {reason}"
            ),
            other => panic!("expected TermEnded for `{line}`, got {other:?}"),
        }
    }
    assert!(
        driver.opened().is_empty(),
        "nothing may be STARTED for a line that is not the verb, and nothing may be started \
         for a bare `!term` — that one attaches"
    );

    // And a driver that refuses — a pty that would not open, a scope that would not be joined
    // — reaches the head as the driver's own sentence rather than as a silence.
    *driver.refuse.lock().unwrap() = Some("no pty: /dev/ptmx is not there".into());
    w.write(&ClientFrame::TermOpen {
        line: "!term mc".into(),
        cols: 80,
        rows: 24,
    })
    .expect("open");
    match until_term(&mut r) {
        ServerFrame::TermEnded { reason } => {
            assert_eq!(reason, "no pty: /dev/ptmx is not there")
        }
        other => panic!("expected TermEnded, got {other:?}"),
    }
}

/// **A bare `!term` reaches `attach` and not `open`, and what comes back is the pane.**
///
/// The operator's defect: `!term mc` flashed and was gone, and a second `!term` said *"a pane is
/// already open in this session"*. The verb with nothing after it used to be refused
/// (*"`!term` needs a command to run"*), and it is now the way back to a program that is still
/// running — which is only possible because the screen is the **daemon's**, so the driver is
/// what is asked and the driver is what replays.
///
/// Three assertions, and they are the whole of the seam:
///
/// * **`open` is never called** — a bare verb starts nothing, which is what makes this an attach
///   rather than a second pane;
/// * **`attach` gets the session and the rectangle** — the head's own rectangle, so a program is
///   laid out for the screen it is being drawn in rather than for the one it left;
/// * **and the head is told what is running before it is shown the screen.** The order is the
///   assertion: a head that drew the bytes first and learned what they were afterwards would
///   flash a rectangle it could not name.
#[test]
fn a_bare_term_line_attaches_to_the_pane_the_session_has() {
    let driver = Canned::new();
    *driver.running.lock().unwrap() = Some("mc /etc".into());
    let registry = start(Some(driver.clone()));
    let (mut w, mut r) = attach(&registry);

    w.write(&ClientFrame::TermOpen {
        line: "!term".into(),
        cols: 97,
        rows: 23,
    })
    .expect("attach");

    // The name of what is running, first.
    match until_term(&mut r) {
        ServerFrame::TermAttached { command } => assert_eq!(
            command, "mc /etc",
            "the daemon was handed the command with the verb stripped, and that is what it \
             can say back"
        ),
        other => panic!("expected TermAttached first, got {other:?}"),
    }
    // And then the screen it has kept.
    match until_term(&mut r) {
        ServerFrame::TermOutput { bytes } => assert!(
            String::from_utf8_lossy(&bytes).contains("mc's screen"),
            "the driver's replay is what comes back: {bytes:?}"
        ),
        other => panic!("expected the replayed screen, got {other:?}"),
    }
    assert!(
        driver.opened().is_empty(),
        "a bare `!term` must start nothing: {:?}",
        driver.opened()
    );
    assert_eq!(
        driver.attached(),
        vec![("s-1".to_string(), 97usize, 23usize)],
        "the driver is told the session and the ATTACHING head's rectangle"
    );
}

/// **The verb is a whole word, and the parse is the one both halves share.**
///
/// The head recognises `!term` at the composer and the daemon re-checks it at the socket —
/// because a frame is a socket and not a keyboard — and the two cannot disagree, because there
/// is one function. The contrast cases are here because a recogniser is only honest next to
/// what it must NOT take: `!terminal` and `!terms` are ordinary `!` lines, and they always were.
///
/// **And the three readings are one parse.** `!term` is an attach, `!term close` is the ending,
/// and anything else is a command — the word `close` is the ending only as a whole word, so
/// `!term closed` and `!term close-it` are programs like any other.
#[test]
fn the_verb_is_a_whole_word_and_the_parse_is_shared() {
    use letibot_sessionlog::TermLine::{Attach, Close, Run};
    use letibot_sessionlog::term_line;

    assert_eq!(term_command("!term mc"), Some("mc"));
    assert_eq!(
        term_command("!term   nano notes.txt"),
        Some("nano notes.txt")
    );
    assert_eq!(term_command("!term\tmc"), Some("mc"));
    // The verb with nothing after it: a `!term` line whose command is empty, which is a
    // different answer from *not this verb*, because the head says a different sentence.
    assert_eq!(term_command("!term"), Some(""));
    assert_eq!(term_command("!term   "), Some(""));
    // Not the verb.
    assert_eq!(term_command("!terminal x"), None);
    assert_eq!(term_command("!terms x"), None);
    assert_eq!(term_command("!term-x"), None);
    assert_eq!(term_command("term mc"), None);
    assert_eq!(term_command(" !term mc"), None);
    assert_eq!(term_command(""), None);
    // And the command is whole: it is a shell line and the shell is what reads it, so
    // `FOO=1`, `&&` and a pipe all survive the parse untouched.
    assert_eq!(
        term_command("!term FOO=1 mc /etc && echo done | less"),
        Some("FOO=1 mc /etc && echo done | less")
    );
    // The control that makes the word boundary mean something: an ordinary `!` line is still
    // an ordinary `!` line.
    assert_eq!(
        letibot_sessionlog::operator_shell_command("!term mc"),
        Some("term mc"),
        "the `!` line's own parse is untouched — `!term` is a `!` line with a command, which \
         is why the head has to check the verb first"
    );

    // **The three readings.** `term_line` is what the head dispatches on and what the daemon
    // re-checks, so the endings and the programs are asserted together — a `close` that fell
    // through to `Run` would be a program started where a pane was meant to end, and the
    // reverse is a pane ended where a program was meant to run.
    assert_eq!(term_line("!term"), Some(Attach));
    assert_eq!(term_line("!term   "), Some(Attach));
    assert_eq!(term_line("!term close"), Some(Close));
    assert_eq!(term_line("!term   close"), Some(Close));
    assert_eq!(
        term_line("!term nano notes.txt"),
        Some(Run("nano notes.txt"))
    );
    // A whole word, exactly as the verb is: these are programs called `closed`, `close-it`
    // and `closing`, and none of them is the ending.
    assert_eq!(term_line("!term closed"), Some(Run("closed")));
    assert_eq!(term_line("!term close-it"), Some(Run("close-it")));
    assert_eq!(term_line("!term closing"), Some(Run("closing")));
    // **A program called `close` is still reachable**, which is the cost of the bare word paid
    // in the open: the command is a shell line, so anything that makes it a line and not the
    // bare word runs it.
    assert_eq!(term_line("!term command close"), Some(Run("command close")));
    assert_eq!(term_line("!term ./close"), Some(Run("./close")));
    assert_eq!(term_line("!term close foo"), Some(Run("close foo")));
    // Not the verb at all, in either spelling of the parse.
    for line in [
        "!terminal x",
        "!terms x",
        "!term-x",
        "term close",
        " !term close",
        "",
    ] {
        assert_eq!(term_line(line), None, "`{line}` is not a `!term` line");
        assert_eq!(
            term_command(line),
            None,
            "and not one by the primitive either"
        );
    }
}
