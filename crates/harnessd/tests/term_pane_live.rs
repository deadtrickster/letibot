//! **`!term` end to end: the daemon's own driver, a real pty, and a real program in it.**
//!
//! # What this file is for, and the gap it closes
//!
//! `!term` is tested in three places, and each of them stops one step short of the thing the
//! operator asked for:
//!
//! | where | what it covers | what it replaces |
//! |---|---|---|
//! | `letibot-tools`' `exec/term.rs` | the pty: `setsid`/`TIOCSCTTY`, keys in, bytes back, `SIGWINCH`, the exit status | nothing — a real child on a real pty |
//! | `letibot-ui`'s `ansi.rs` | the screen: a byte stream turned into exactly `room` rows | nothing |
//! | `letibot-sessionlog`'s `term_pane.rs` | the six frames, the verb, the refusals | **the driver** — a canned `TerminalDriver` |
//!
//! So the frame path is asserted against a driver that runs nothing, and the pty is asserted
//! without a daemon around it. **Nothing joined the two**, which is exactly where a defect
//! would live: `Terminals::open` reads the session's workspace out of the registry, opens the
//! scope, builds the `TermConfig` and hands the pty's bytes to `Hub::push_frame`; and it is
//! `harnessd/src/term.rs`'s own module that had no test at all.
//!
//! This file is that join. It installs **`letibot_harnessd::term::Terminals`** — the driver the
//! daemon installs at startup in `cli.rs`, not a double — on a registry, attaches a head over a
//! real socket, and drives it with the frames a head sends. What runs is `/bin/sh -c <command>`
//! on a real pty, started by the real code path.
//!
//! # What it therefore proves, and what it still cannot
//!
//! Proved here: a command typed as `!term …` reaches a real program on a real pty, its bytes
//! reach the head as `TermOutput`, the pane runs **in the session's workspace and not the
//! daemon's**, the operator's bytes reach the program, a second pane is refused by name, and
//! `TermClose` ends the program with the operator's own sentence.
//!
//! **Not here, and not answerable by a test:** whether `nano` is *usable* — whether the
//! rectangle is the right rectangle, whether `ctrl-\` is where a person's fingers go. That
//! needs a live head on a real terminal and a person driving it. `term_pane.rs` says the same
//! thing from the other side, and neither file pretends otherwise.
//!
//! # Why every read is bounded
//!
//! A pane is a byte stream from a process, so a test that reads it can wait for a frame that
//! never comes — and a test that hangs parks whoever runs it next rather than failing. Every
//! socket here carries a read timeout, so a missing frame is an `Err` from `FrameReader` and a
//! panic with the frames that did arrive, not a wait with no end.

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

/// How long a read may wait before the test calls it a missing frame. Generous — a pty has to
/// fork, `exec` and write — and finite, which is the whole point.
const PATIENCE: Duration = Duration::from_secs(30);

/// **One pane at a time in this file.**
///
/// Each test here starts a real program on a real pty inside a real cgroup scope, and what they
/// are about is the pane — not five panes at once. Running them concurrently is measuring the
/// box's cgroup, and on the machine this was written on it measured badly: **one run in three
/// took exactly `sleep 30`'s lifetime** (30.02 s, twice, against 0.02 s the third time), which is
/// a pane whose program was left to exit on its own instead of being ended by the close. The
/// same tests run one at a time are 0.00 s each, and the whole file is 0.03 s serialised.
///
/// So this is a lock and not a note in a README: `--test-threads` is the harness's business and
/// a test that is only correct under one setting is a test that will fail on somebody else's
/// machine for a reason that is not about the pane. Poisoning is recovered rather than
/// propagated — a test that panicked has already reported its own failure, and the next one
/// should still run.
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
    let p = std::env::temp_dir().join(format!("letibot-pane-{tag}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&p).expect("a workspace to run in");
    // The canonical path, because `/tmp` is a symlink on some boxes and `pwd` prints where the
    // process really is — a comparison against the un-canonicalised name would fail for a
    // reason that has nothing to do with the pane.
    p.canonicalize().expect("the workspace resolves")
}

/// **The daemon's registry, with the daemon's own driver on it.**
///
/// This is the line `crates/harnessd/src/cli.rs` runs at startup, with the same `Weak` — the
/// registry owns the driver and the driver reads the registry's wiring, so an `Arc` here would
/// be the cycle `Terminals`' own field comment names.
fn daemon(session: &str, workspace: &PathBuf) -> Arc<Registry> {
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

/// Attach a head and hand back the wire, the way `term_pane.rs` does — and with a read timeout
/// on the reader, which is this file's addition: a pane that never answers must fail the test
/// rather than park it.
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

/// **Every byte the program wrote, up to the pane's one ending.**
fn read_to_end(r: &mut FrameReader<UnixStream>) -> (Vec<u8>, String) {
    let mut said = Vec::new();
    loop {
        match r.read::<ServerFrame>().expect("a pane frame") {
            ServerFrame::TermOutput { bytes } => said.extend_from_slice(&bytes),
            ServerFrame::TermEnded { reason } => return (said, reason),
            // The session's own traffic — a pane moves no seq, but the seat still sends the
            // hello's events, and skipping them is what makes this a pane assertion.
            ServerFrame::Event(_) => continue,
            other => panic!("expected a pane frame, got {other:?}"),
        }
    }
}

/// Read until the program has written `needle`, and refuse to wait for ever for it.
fn read_until(r: &mut FrameReader<UnixStream>, needle: &str) -> Vec<u8> {
    let mut said = Vec::new();
    loop {
        match r.read::<ServerFrame>().expect("a pane frame") {
            ServerFrame::TermOutput { bytes } => {
                said.extend_from_slice(&bytes);
                if String::from_utf8_lossy(&said).contains(needle) {
                    return said;
                }
            }
            ServerFrame::TermEnded { reason } => {
                panic!(
                    "the pane ended ({reason}) before it wrote {needle:?}; it wrote: {:?}",
                    String::from_utf8_lossy(&said)
                )
            }
            ServerFrame::Event(_) => continue,
            other => panic!("expected a pane frame, got {other:?}"),
        }
    }
}

/// **The pane's program gets the console's environment, and not the daemon's own.**
///
/// This is the third defect of the operator's live report, end to end: the daemon's own
/// driver, a real pty, a real program, and the environment it can read for itself.
///
/// **What it was.** `TermSession::start` set the pairs it was given and cleared nothing, so
/// the pane's program inherited *the daemon's whole environment* — measured on this box,
/// `!term env` printed `CARGO_MANIFEST_DIR`, `RUSTUP_TOOLCHAIN`, `LD_LIBRARY_PATH`,
/// `SUDO_ASKPASS`, `LETIBOT_SOCKET` and `LETIBOT_SESSION`. That is the harness's own
/// variables, including every token the daemon's environment carries, handed to a program
/// the operator runs; and `TERM` and `HOME` were in there only *by accident*, which is a
/// daemon started without them giving a pane neither. `mc` needs both — terminfo for the
/// first, `~/.config/mc` for the second — and prints one line and exits without them, which
/// is the flash.
///
/// **The control is the first assertion**, and it is what makes the absence mean anything:
/// the variable this test asks for *is* in the daemon's own environment, so a pane that
/// does not see it did not inherit — it was told.
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

/// **A real program, on a real pty, its bytes on the wire, and its exit status in the ending.**
///
/// This is the whole requirement in one test: `!term echo …` is the operator's line, the daemon
/// starts a program for it, and what the program wrote arrives at the head as `TermOutput`.
/// The `\r\n` is not asserted and must not be — it is the pty's own translation, and pinning it
/// would be pinning the line discipline rather than the pane.
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

/// **The pane runs where the conversation runs, not where the daemon does.**
///
/// `Terminals::workspace` reads the session's wiring out of the registry, and this is the
/// assertion that it is wired: the daemon's own cwd is this test's cwd (the crate directory),
/// which is a different path, so a pane that ignored the wiring would print the wrong one.
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

/// **The operator's bytes reach the program verbatim, and it is the program that answers.**
///
/// `tr a-z A-Z` is chosen over `cat` for a reason that is the whole point of the test: **a pty
/// echoes what is typed at it**, so "the bytes came back" is a claim `cat` cannot make — the
/// echo would satisfy it with the program never having run at all. (It did, in this test's
/// first draft, on a box whose pane could not get a cgroup.) `tr` answers in *different* bytes:
/// the echo returns `hello-pane` and only the program returns `HELLO-PANE`, so the assertion
/// cannot be satisfied by the line discipline.
#[test]
fn the_operators_bytes_reach_the_program() {
    let _serial = serial();
    let ws = workspace("keys");
    let registry = daemon("s-keys", &ws);
    let (mut w, mut r) = attach(&registry, "s-keys");

    open(&mut w, "!term tr a-z A-Z", 80, 24);
    w.write(&ClientFrame::TermInput {
        bytes: b"hello-pane\n".to_vec(),
    })
    .expect("term input");

    let said = read_until(&mut r, "HELLO-PANE");
    assert!(
        String::from_utf8_lossy(&said).contains("HELLO-PANE"),
        "the program never saw the keystrokes"
    );
    // Leaving ends it, and the ending is the operator's act — asserted here so the test does
    // not walk away from a running `tr`.
    w.write(&ClientFrame::TermClose).expect("term close");
    let (_, reason) = read_to_end(&mut r);
    assert_eq!(reason, "you left the terminal");
    let _ = std::fs::remove_dir_all(&ws);
}

/// **A second pane is refused by name, and the first program keeps running.**
///
/// The refusal is the driver's, not the head's: the daemon owns the pty, so the daemon is the
/// authority on *is a pane open*. `sleep 30` is chosen so that "the first program is still
/// alive" is not a race with its own exit — and the test leaves by the operator's own act,
/// which is also the assertion that the refusal did not end the pane it refused to replace.
#[test]
fn a_second_pane_is_refused_by_name_while_one_is_live() {
    let _serial = serial();
    let ws = workspace("one");
    let registry = daemon("s-one", &ws);
    let (mut w, mut r) = attach(&registry, "s-one");

    open(&mut w, "!term sleep 30", 80, 24);
    open(&mut w, "!term echo second", 80, 24);

    let (said, reason) = read_to_end(&mut r);
    assert!(
        reason.contains("a pane is already open"),
        "the second pane was not refused by name: {reason:?}"
    );
    assert!(
        !String::from_utf8_lossy(&said).contains("second"),
        "the refused command ran anyway: {:?}",
        String::from_utf8_lossy(&said)
    );

    // **The first program is still running, and this is the proof.** The refusal left it alone,
    // so it is ended here by the operator's own act rather than by the test walking away — a
    // `sleep 30` left behind would outlive the process that owned its cgroup.
    w.write(&ClientFrame::TermClose).expect("term close");
    let (_, reason) = read_to_end(&mut r);
    assert_eq!(
        reason, "you left the terminal",
        "the pane that was refused a second one was still the operator's to leave"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

/// **Leaving is the daemon's act, and it says what the operator did.**
///
/// `TermClose` is a frame the program never sees — the head finds `ctrl-\` on its raw byte
/// stream and sends this instead — so the program is ended by the daemon rather than by a key
/// it could have trapped. The sentence is the operator's own act rather than the signal the
/// program died of: *"you left the terminal"*, not *"killed by SIGKILL"*.
#[test]
fn leaving_is_the_daemons_act_and_names_the_operator() {
    let _serial = serial();
    let ws = workspace("left");
    let registry = daemon("s-left", &ws);
    let (mut w, mut r) = attach(&registry, "s-left");

    open(&mut w, "!term sleep 30", 80, 24);
    w.write(&ClientFrame::TermClose).expect("term close");

    let (_, reason) = read_to_end(&mut r);
    assert_eq!(
        reason, "you left the terminal",
        "the operator's own act is the fact worth reporting"
    );
    let _ = std::fs::remove_dir_all(&ws);
}
