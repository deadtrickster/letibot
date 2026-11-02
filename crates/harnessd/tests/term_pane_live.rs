//! `!term` end to end: the daemon's own driver, a real pty, and a real program in it. The
//! pty is asserted without a daemon and the frame path against a driver that runs nothing
//! so nothing joined the two - which is exactly where a defect would live. This installs
//! `letibot_harnessd::term::Terminals` (the driver `cli.rs` installs at startup, not a
//! double) on a registry, attaches a head over a real socket, and drives it with the frames
//! a head sends. What it cannot answer: whether `nano` is *usable* - the right rectangle
//! the right chords. That needs a person on a real terminal.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use letibot_harnessd::term::Terminals;
use letibot_sessionlog::protocol::{Caps, ClientFrame, PROTOCOL_VERSION, ServerFrame};
use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_sessionlog::server::serve_conn;
use letibot_sessionlog::wire::{FrameReader, FrameWriter};
use std::os::unix::net::UnixStream;

/// How long a read may wait before the test calls it a missing frame. Generous - a pty has to
/// fork, `exec` and write - and finite, which is the whole point.
const PATIENCE: Duration = Duration::from_secs(30);

/// One pane at a time in this file. Each test here starts a real program on a real pty
/// inside a real cgroup scope, and running them concurrently measures the box's cgroup
/// rather than the pane.
static ONE_PANE_AT_A_TIME: Mutex<()> = Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    ONE_PANE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner())
}

/// A directory of this test's own, so `pwd` inside the pane has a known answer and the pane's
/// workspace is not the daemon's.
fn workspace(tag: &str) -> PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let p = letibot_sessionlog::testing::scratch_root()
        .join(format!("letibot-pane-{tag}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&p).expect("a workspace to run in");
    // The canonical path, because `/tmp` is a symlink on some boxes and `pwd` prints where the
    // process really is, so a comparison against the un-canonicalised name would fail for nothing.
    p.canonicalize().expect("the workspace resolves")
}

/// The daemon's registry, with the daemon's own driver on it. The line
/// `crates/harnessd/src/cli.rs` runs at startup, with the same `Weak`
fn daemon(session: &str, workspace: &std::path::Path) -> Arc<Registry> {
    let registry = Registry::new();
    registry
        .create(
            session,
            "one",
            SessionWiring {
                model: "qwen3-4b".into(),
                dialect: "qwen".into(),
                endpoint: "127.0.0.1:8080".into(),
                workspace: workspace.display().to_string(),
            },
        )
        .expect("create");
    registry.set_terminal(Arc::new(Terminals::new(Arc::downgrade(&registry))));
    registry
}

/// Attach a head and hand back the wire the way `term_pane.rs` does - with a read timeout, which is
/// this file's addition: a pane that never answers must fail the test rather than park it.
fn attach(
    registry: &Arc<Registry>,
    session: &str,
) -> (FrameWriter<UnixStream>, FrameReader<UnixStream>) {
    let (server_side, head_side) = UnixStream::pair().expect("a socket pair");
    let reg = registry.clone();
    std::thread::spawn(move || {
        let _ = serve_conn(reg, server_side);
    });
    head_side
        .set_read_timeout(Some(PATIENCE))
        .expect("a bounded read");
    let mut w = FrameWriter::new(head_side.try_clone().expect("clone"));
    let mut r = FrameReader::new(head_side);
    w.write(&ClientFrame::Attach {
        protocol_version: PROTOCOL_VERSION,
        session_id: session.into(),
        since_seq: 0,
        kind: "tui".into(),
        identity: "test".into(),
        caps: Caps::default(),
    })
    .expect("attach");
    assert!(
        matches!(
            r.read::<ServerFrame>().expect("hello"),
            ServerFrame::Hello { .. }
        ),
        "the daemon says hello before anything else"
    );
    (w, r)
}

fn open(w: &mut FrameWriter<UnixStream>, line: &str, cols: usize, rows: usize) {
    w.write(&ClientFrame::TermOpen {
        line: line.into(),
        cols,
        rows,
    })
    .expect("term open");
}

/// Every byte the program wrote, up to the pane's one ending. A `TermStatus` answer is skipped
/// rather than refused: it is a pane frame, it carries no program bytes and it is not the ending.
fn read_to_end(r: &mut FrameReader<UnixStream>) -> (Vec<u8>, String) {
    let mut said = Vec::new();
    loop {
        match r.read::<ServerFrame>().expect("a pane frame") {
            ServerFrame::TermOutput { bytes, .. } => said.extend_from_slice(&bytes),
            ServerFrame::TermEnded { reason, .. } => return (said, reason),
            // The session's own traffic - a pane moves no seq, but the seat still sends the
            // hello's events, and skipping them is what makes this a pane assertion.
            ServerFrame::Event(_)
            | ServerFrame::TermStatus { .. }
            | ServerFrame::TermAttached { .. } => continue,
            other => panic!("expected a pane frame, got {other:?}"),
        }
    }
}

/// Read until the program has written `needle`, and refuse to wait for ever for it.
fn read_until(r: &mut FrameReader<UnixStream>, needle: &str) -> Vec<u8> {
    let mut said = Vec::new();
    loop {
        match r.read::<ServerFrame>().expect("a pane frame") {
            ServerFrame::TermOutput { bytes, .. } => {
                said.extend_from_slice(&bytes);
                if String::from_utf8_lossy(&said).contains(needle) {
                    return said;
                }
            }
            ServerFrame::TermEnded { reason, .. } => {
                panic!(
                    "the pane ended ({reason}) before it wrote {needle:?}; it wrote: {:?}",
                    String::from_utf8_lossy(&said)
                )
            }
            ServerFrame::Event(_)
            | ServerFrame::TermStatus { .. }
            | ServerFrame::TermAttached { .. } => continue,
            other => panic!("expected a pane frame, got {other:?}"),
        }
    }
}

/// The pane's program gets the console's environment, and not the daemon's own. The control
/// is the first assertion, and it is what makes the absence mean anything
#[test]
fn the_panes_program_gets_the_consoles_environment_and_not_the_daemons_own() {
    let _serial = serial();
    assert!(
        std::env::var("CARGO_MANIFEST_DIR").is_ok(),
        "this test is only evidence if the daemon's own environment carries the variable \
         the pane must not see"
    );
    let ws = workspace("env");
    let registry = daemon("s-env", &ws);
    let (mut w, mut r) = attach(&registry, "s-env");

    open(
        &mut w,
        "!term printf 'TERM=[%s] HOME=[%s] PATH=[%s] MANIFEST=[%s]\\n' \"$TERM\" \"$HOME\" \
         \"$PATH\" \"$CARGO_MANIFEST_DIR\"",
        80,
        24,
    );
    let (said, _) = read_to_end(&mut r);
    let text = String::from_utf8_lossy(&said);
    // The pty translates the newline to `\r\n` and the program's own format has one field per
    // pair, so the answer is read by name rather than by position.
    let value = |name: &str| -> String {
        text.split_whitespace()
            .find_map(|word| word.strip_prefix(name))
            .unwrap_or("")
            .trim_matches(|c| c == '[' || c == ']')
            .to_string()
    };
    assert!(
        !value("TERM=").is_empty(),
        "`mc` needs TERM for terminfo, and a pane whose program has none cannot address the \
         cursor either: {text:?}"
    );
    assert!(
        value("HOME=").starts_with('/'),
        "`mc` writes its config under HOME and exits with a sentence when it has none, which \
         is the flash: {text:?}"
    );
    assert!(
        !value("PATH=").is_empty(),
        "a pane runs a program by name, so it needs the console's PATH: {text:?}"
    );
    assert!(
        value("MANIFEST=").is_empty(),
        "the daemon's own environment must not reach a program the operator runs, and this is \
         the variable that says it did: {text:?}"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

/// A pane whose program exited frees its slot, and the next `!term` is not refused. The
/// daemon kept the slot while the head had lost the rectangle, because the question it
/// asked was `closed` (has anybody ended this pane) and nobody had
#[test]
fn a_pane_whose_program_exited_frees_its_slot() {
    let _serial = serial();
    let ws = workspace("ghost");
    let registry = daemon("s-ghost", &ws);
    let (mut w, mut r) = attach(&registry, "s-ghost");

    // A program that prints one line and exits at once. `sh -c` rather than a name, so this does
    // not depend on what is installed on the box - the shape `mc` on a box without it produced.
    open(&mut w, "!term sh -c 'echo one-line; exit 7'", 80, 24);
    let (said, reason) = read_to_end(&mut r);
    let text = String::from_utf8_lossy(&said);
    assert!(
        text.contains("one-line"),
        "the program's last bytes must reach the head: {text:?}"
    );
    assert_eq!(
        reason, "the program exited with 7",
        "and its exit status is the ending, which is half of the row the head files"
    );

    // The slot is free. This is the assertion the defect is about: the next pane starts
    // rather than being refused by a ghost, and it runs.
    open(&mut w, "!term echo second-pane", 80, 24);
    let (said, reason) = read_to_end(&mut r);
    let text = String::from_utf8_lossy(&said);
    assert!(
        text.contains("second-pane"),
        "the second pane must have RUN rather than been refused: {text:?} (reason: {reason})"
    );
    assert!(
        !reason.contains("already open"),
        "and it must not have been refused by the pane that was already over: {reason:?}"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

/// Read until a pane frame, skipping the session's own traffic - the `Hello` a fresh attach is
/// answered with, and the events behind it. `TermStatus` is one of the pane's frames: it is the
/// daemon's answer to *what is this session's pane running* and it carries no program bytes.
fn until_pane(r: &mut FrameReader<UnixStream>) -> ServerFrame {
    loop {
        match r.read::<ServerFrame>().expect("a pane frame") {
            f @ (ServerFrame::TermAttached { .. }
            | ServerFrame::TermOutput { .. }
            | ServerFrame::TermEnded { .. }
            | ServerFrame::TermStatus { .. }) => return f,
            ServerFrame::Event(_) | ServerFrame::Hello { .. } => continue,
            other => panic!("expected a pane frame, got {other:?}"),
        }
    }
}

/// The daemon's answer to a status read, with the pane's own drawing skipped: a running pane
/// writes bytes while this is asked, so an assertion against "the next frame" would pass or fail
/// on how fast the program drew. `None` is a real answer - *this session has no live pane*.
fn until_status(r: &mut FrameReader<UnixStream>) -> Option<String> {
    loop {
        match until_pane(r) {
            ServerFrame::TermStatus { panes } => return panes.first().map(|b| b.command.clone()),
            _ => continue,
        }
    }
}

/// A bare `!term` gives a head back the screen the daemon holds - and a live pane. Who
/// holds the screen is the decision underneath, and it is the daemon. It is deliberately a
/// second head, and the proof is the replay
#[test]
fn a_bare_term_line_attaches_to_the_screen_the_daemon_holds() {
    let _serial = serial();
    let ws = workspace("attach");
    let registry = daemon("s-attach", &ws);
    let (mut w, mut r) = attach(&registry, "s-attach");

    // A program that draws, then waits for a keystroke - so the pane is alive across the
    // attach, which is the case the operator was in.
    open(
        &mut w,
        // `read y` after the answer keeps the program waiting, so the deliberate close below is
        // the only way this pane ends; without it the close raced the program's own exit.
        "!term sh -c 'printf drawn-by-the-program; read x; printf \"GOT:%s\" \"$x\"; read y'",
        80,
        24,
    );
    read_until(&mut r, "drawn-by-the-program");

    // A second head, which has never seen this pane. `attach` is the session switch, and it
    // is also the head that closed its rectangle and came back.
    let (mut w2, mut r2) = attach(&registry, "s-attach");
    open(&mut w2, "!term", 100, 30);

    // What is running, first.
    match until_pane(&mut r2) {
        ServerFrame::TermAttached { command, .. } => assert!(
            command.contains("drawn-by-the-program"),
            "the daemon must say what is running in the pane this session has: {command:?}"
        ),
        other => panic!("expected TermAttached first, got {other:?}"),
    }
    // And then the screen it held, replayed.
    match until_pane(&mut r2) {
        ServerFrame::TermOutput { bytes, .. } => {
            let said = String::from_utf8_lossy(&bytes);
            assert!(
                said.contains("drawn-by-the-program"),
                "the daemon must replay what the program drew, and this head never saw it \
                 draw: {said:?}"
            );
        }
        other => panic!("expected the replayed screen, got {other:?}"),
    }

    // And it is a live pane, not a picture: the attaching head types at it, and the program
    // answers this head.
    w2.write(&ClientFrame::TermInput {
        pane: 1,
        bytes: b"hi\n".to_vec(),
    })
    .expect("term input");
    let mut said = Vec::new();
    loop {
        match until_pane(&mut r2) {
            ServerFrame::TermOutput { bytes, .. } => {
                said.extend_from_slice(&bytes);
                if String::from_utf8_lossy(&said).contains("GOT:hi") {
                    break;
                }
            }
            ServerFrame::TermEnded { reason, .. } => panic!(
                "the pane ended ({reason}) before the program answered; it said: {:?}",
                String::from_utf8_lossy(&said)
            ),
            other => panic!("expected a pane frame, got {other:?}"),
        }
    }
    // The deliberate close is the daemon's act, and this head's - `!term close`, confirmed by
    // the operator. `ctrl-\` is not this: it detaches and sends nothing at all, so the daemon never
    // hears about it and the pane goes on running.
    w2.write(&ClientFrame::TermClose { pane: 1 })
        .expect("term close");
    let (_, reason) = read_to_end(&mut r2);
    assert_eq!(reason, "you closed the terminal");
    let _ = std::fs::remove_dir_all(&ws);
}

/// A session with no pane refuses the attach by name - a sentence, not an empty rectangle.
#[test]
fn attaching_to_a_session_with_no_pane_says_so() {
    let _serial = serial();
    let ws = workspace("attach-none");
    let registry = daemon("s-attach-none", &ws);
    let (mut w, mut r) = attach(&registry, "s-attach-none");

    open(&mut w, "!term", 80, 24);
    match until_pane(&mut r) {
        ServerFrame::TermEnded { reason, .. } => assert!(
            reason.contains("no pane to attach to"),
            "a session with no pane must say so: {reason}"
        ),
        other => panic!("expected TermEnded, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&ws);
}

/// A real program, on a real pty, its bytes on the wire, and its exit status in the ending.
/// The `\r\n` is not asserted and must not be - it is the pty's own translation, and
/// pinning it would be pinning the line discipline rather than the pane.
#[test]
fn a_real_program_runs_in_the_pane_and_its_bytes_reach_the_head() {
    let _serial = serial();
    let ws = workspace("bytes");
    let registry = daemon("s-bytes", &ws);
    let (mut w, mut r) = attach(&registry, "s-bytes");

    open(&mut w, "!term echo hello-pane", 80, 24);
    let (said, reason) = read_to_end(&mut r);

    let text = String::from_utf8_lossy(&said);
    assert!(
        text.contains("hello-pane"),
        "the program's own bytes did not reach the head: {text:?}"
    );
    assert_eq!(
        reason, "the program exited",
        "a program that returned 0 says so without a number"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

/// The pane runs where the conversation runs, not where the daemon does. `Terminals::workspace`
/// reads the session's wiring out of the registry; the daemon's own cwd is this test's cwd, so a
/// pane that ignored the wiring would print the wrong path.
#[test]
fn the_pane_runs_in_the_sessions_workspace_and_not_the_daemons() {
    let _serial = serial();
    let ws = workspace("cwd");
    assert_ne!(
        ws,
        std::env::current_dir()
            .expect("a cwd")
            .canonicalize()
            .unwrap(),
        "the fixture must differ from the daemon's own directory or this proves nothing"
    );
    let registry = daemon("s-cwd", &ws);
    let (mut w, mut r) = attach(&registry, "s-cwd");

    open(&mut w, "!term pwd", 80, 24);
    let (said, _) = read_to_end(&mut r);

    let text = String::from_utf8_lossy(&said);
    assert!(
        text.contains(&ws.display().to_string()),
        "the pane did not start in the session's workspace: {text:?}"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

/// The operator's bytes reach the program verbatim, and it is the program that answers. `tr
/// a-z A-Z` and not `cat`, for a reason that is the whole point
#[test]
fn the_operators_bytes_reach_the_program() {
    let _serial = serial();
    let ws = workspace("keys");
    let registry = daemon("s-keys", &ws);
    let (mut w, mut r) = attach(&registry, "s-keys");

    open(&mut w, "!term tr a-z A-Z", 80, 24);
    w.write(&ClientFrame::TermInput {
        pane: 1,
        bytes: b"hello-pane\n".to_vec(),
    })
    .expect("term input");

    let said = read_until(&mut r, "HELLO-PANE");
    assert!(
        String::from_utf8_lossy(&said).contains("HELLO-PANE"),
        "the program never saw the keystrokes"
    );
    // The deliberate act ends it, so the test does not walk away from a running `tr`.
    w.write(&ClientFrame::TermClose { pane: 1 })
        .expect("term close");
    let (_, reason) = read_to_end(&mut r);
    assert_eq!(reason, "you closed the terminal");
    let _ = std::fs::remove_dir_all(&ws);
}

/// A second `!term` runs beside the first, and the first keeps running. What the one-pane
/// refusal protected - a running program is not killed to make room - is kept the refusal
/// is not.
#[test]
fn a_second_pane_runs_beside_the_first_and_each_is_named_by_its_number() {
    let _serial = serial();
    let ws = workspace("two");
    let registry = daemon("s-two", &ws);
    let (mut w, mut r) = attach(&registry, "s-two");

    open(&mut w, "!term sh -c 'printf first-pane; sleep 30'", 80, 24);
    read_until(&mut r, "first-pane");
    open(&mut w, "!term echo second-pane", 80, 24);

    // Pane 2 is numbered, runs and ends - under its own number.
    let mut second = Vec::new();
    let ended = loop {
        match until_pane(&mut r) {
            ServerFrame::TermAttached { pane, opened, .. } => {
                assert_eq!((pane, opened), (2, true), "the second pane is pane 2");
            }
            ServerFrame::TermOutput { pane: 2, bytes } => second.extend_from_slice(&bytes),
            ServerFrame::TermOutput { .. } => {}
            ServerFrame::TermEnded { pane, reason } => break (pane, reason),
            other => panic!("expected a pane frame, got {other:?}"),
        }
    };
    assert_eq!(
        ended,
        (Some(2), "the program exited".to_string()),
        "the second program ran to its own end, and was not refused"
    );
    assert!(
        String::from_utf8_lossy(&second).contains("second-pane"),
        "pane 2's bytes are its program's: {:?}",
        String::from_utf8_lossy(&second)
    );

    // The first is still running, and the daemon says so by number.
    w.write(&ClientFrame::TermStatus).expect("status");
    match until_pane(&mut r) {
        ServerFrame::TermStatus { panes } => {
            assert_eq!(panes.len(), 1, "{panes:?}");
            assert_eq!(panes[0].pane, 1);
            assert!(panes[0].command.contains("first-pane"), "{panes:?}");
        }
        other => panic!("expected TermStatus, got {other:?}"),
    }

    // `!term 1` comes back to it, with the screen the daemon kept.
    open(&mut w, "!term 1", 80, 24);
    match until_pane(&mut r) {
        ServerFrame::TermAttached { pane, opened, .. } => assert_eq!((pane, opened), (1, false)),
        other => panic!("expected TermAttached, got {other:?}"),
    }
    match until_pane(&mut r) {
        ServerFrame::TermOutput { pane: 1, bytes } => {
            assert!(String::from_utf8_lossy(&bytes).contains("first-pane"))
        }
        other => panic!("expected pane 1's screen, got {other:?}"),
    }

    // And it is the operator's to end, by number.
    w.write(&ClientFrame::TermClose { pane: 1 })
        .expect("term close");
    let (_, reason) = read_to_end(&mut r);
    assert_eq!(reason, "you closed the terminal");
    let _ = std::fs::remove_dir_all(&ws);
}

/// A pane runs inside the boundary its session installed. The harness installs its process
/// host's boundary on the registry at open (`Registry::bound_panes`), and `Terminals::open`
/// puts that argv in front of the pane's shell.
#[test]
fn a_pane_runs_inside_the_boundary_its_session_installed() {
    use letibot_tools::exec::confine::{Bwrap, ConfinePlan, Confinement};
    let _serial = serial();
    let root = workspace("confined");
    let ws = root.join("project");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::write(root.join("beside"), "secret").unwrap();
    let bwrap = match Bwrap::project(&ws) {
        Ok(b) => Arc::new(b),
        Err(e) => {
            eprintln!("SKIP: no usable boundary on this box: {e}");
            return;
        }
    };
    let registry = daemon("s-confined", &ws);
    registry.bound_panes(
        "s-confined",
        Arc::new(move |cwd: &std::path::Path, env: &[(String, String)]| {
            bwrap
                .wrap_terminal(&ConfinePlan { cwd, env })
                .map_err(|e| e.to_string())
        }),
    );
    let (mut w, mut r) = attach(&registry, "s-confined");
    let line = format!(
        "!term if [ -e {} ]; then echo SEEN; else echo ABSENT; fi; pwd; \
         ( : </dev/tty ) 2>/dev/null && echo TTY-OK",
        root.join("beside").display()
    );
    open(&mut w, &line, 80, 24);
    let (said, reason) = read_to_end(&mut r);
    let text = String::from_utf8_lossy(&said);
    assert!(
        text.contains("ABSENT"),
        "the boundary hid the file: {text:?} ({reason})"
    );
    assert!(text.contains(&ws.display().to_string()), "{text:?}");
    assert!(
        text.contains("TTY-OK"),
        "the pane kept its terminal: {text:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn closing_is_the_daemons_act_and_names_the_operator() {
    let _serial = serial();
    let ws = workspace("left");
    let registry = daemon("s-left", &ws);
    let (mut w, mut r) = attach(&registry, "s-left");

    open(&mut w, "!term sleep 30", 80, 24);
    w.write(&ClientFrame::TermClose { pane: 1 })
        .expect("term close");

    let (_, reason) = read_to_end(&mut r);
    assert_eq!(
        reason, "you closed the terminal",
        "the operator's own act is the fact worth reporting"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

/// The daemon answers *what is this session's pane running*, and a detach is invisible to
/// it. `None` is an answer and not a failure
#[test]
fn the_status_read_answers_what_is_running_and_a_detach_is_invisible_to_it() {
    let _serial = serial();
    let ws = workspace("status");
    let registry = daemon("s-status", &ws);
    let (mut w, mut r) = attach(&registry, "s-status");

    // Nothing has ever been started in this session.
    w.write(&ClientFrame::TermStatus).expect("status");
    assert_eq!(
        until_status(&mut r),
        None,
        "a session with no pane must answer with nothing, not with a sentence or a silence"
    );

    // A pane, and the answer is the command the daemon was handed.
    open(&mut w, "!term sleep 30", 80, 24);
    w.write(&ClientFrame::TermStatus).expect("status");
    let running = until_status(&mut r);
    assert!(
        running.as_deref().is_some_and(|c| c.contains("sleep 30")),
        "the daemon must say what is running in the pane this session has: {running:?}"
    );

    // The detach, which is to send nothing. The same read, with no frame in between, still
    // answers - the pane is the session's and the daemon was never told anything happened.
    w.write(&ClientFrame::TermStatus).expect("status");
    let still = until_status(&mut r);
    assert_eq!(
        still, running,
        "a detach sends nothing, so the daemon's answer must not change: {still:?}"
    );

    // And the deliberate close is the one act that changes it.
    w.write(&ClientFrame::TermClose { pane: 1 })
        .expect("term close");
    let (_, reason) = read_to_end(&mut r);
    assert_eq!(reason, "you closed the terminal");
    w.write(&ClientFrame::TermStatus).expect("status");
    assert_eq!(
        until_status(&mut r),
        None,
        "a pane that was closed is not a pane that is running"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

/// The deliberate close frees the slot, and the next `!term` starts rather than being
/// refused. The other half of the ghost: `a_pane_whose_program_exited_frees_its_slot`
/// covers a pane the PROGRAM ended, and this is the pane the OPERATOR ended.
#[test]
fn a_deliberate_close_frees_the_slot_and_the_next_pane_starts() {
    let _serial = serial();
    let ws = workspace("close-slot");
    let registry = daemon("s-close-slot", &ws);
    let (mut w, mut r) = attach(&registry, "s-close-slot");

    open(&mut w, "!term sleep 30", 80, 24);
    w.write(&ClientFrame::TermClose { pane: 1 })
        .expect("term close");
    let (_, reason) = read_to_end(&mut r);
    assert_eq!(
        reason, "you closed the terminal",
        "the frame the head sends for a confirmed close is the one that ends the program"
    );

    // The slot is free. A `sleep 30` still running here would refuse this by name, so the
    // second pane starting at all is the assertion that the close ended the program.
    open(&mut w, "!term echo after-close", 80, 24);
    let (said, reason) = read_to_end(&mut r);
    let text = String::from_utf8_lossy(&said);
    assert!(
        text.contains("after-close"),
        "the next pane must have RUN rather than been refused: {text:?} (reason: {reason})"
    );
    assert!(
        !reason.contains("already open"),
        "and not refused by the pane the close ended: {reason:?}"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

/// `!term close` is refused by name, and a program merely STARTING with it is not.
#[test]
fn a_term_close_line_is_refused_by_name_and_a_name_that_begins_with_it_is_not() {
    let _serial = serial();
    let ws = workspace("close-word");
    let registry = daemon("s-close-word", &ws);
    let (mut w, mut r) = attach(&registry, "s-close-word");

    open(&mut w, "!term close", 80, 24);
    match until_pane(&mut r) {
        ServerFrame::TermEnded { reason, .. } => {
            assert!(
                reason.contains("`!term close` is not a command to run"),
                "the line must be refused as the ending it is: {reason:?}"
            );
            assert!(
                reason.contains("!term command close"),
                "and the refusal must name the way to run a program called `close`: {reason:?}"
            );
        }
        other => panic!("expected the refusal, got {other:?}"),
    }

    // A name that merely begins with the word is a program, and it runs: the shell cannot find it
    // which is its own ending rather than this daemon's refusal.
    open(&mut w, "!term close-it", 80, 24);
    let (_, reason) = read_to_end(&mut r);
    assert!(
        reason.contains("the program exited"),
        "`!term close-it` is a program name and must have been run as one: {reason:?}"
    );
    assert!(
        !reason.contains("is not a command to run"),
        "the refusal was matched by prefix: {reason:?}"
    );
    let _ = std::fs::remove_dir_all(&ws);
}
