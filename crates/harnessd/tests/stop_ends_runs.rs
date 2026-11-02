//! **A stop ends the runs a session has in flight — and says so.**
//!
//! # The state this measures
//!
//! `Registry::close` wakes the daemon's worker out of `next_command`. A worker **inside a
//! run** has not reached `next_command` and will not until the run ends, and the daemon has
//! **one** worker — so a stop that arrives mid-command waits for that command, and the
//! command's own deadline is what ends it. Two minutes at the `bash` default.
//!
//! MEASURED on a live daemon before this change, 2026-10-06, with the operator's own shape
//! (`! /usr/bin/su -c true`: a root process blocked on the device the daemon holds for it,
//! its whole tree `EACCES` for this uid):
//!
//! ```text
//! Stop acked
//! Bye  after 519 µs      ("daemon shutting down")
//! the daemon's process is still in /proc
//! and it is still there 150 s later
//! ```
//!
//! That is the operator's report, sentence for sentence: *"it reports the server exited
//! within a second — while `harnessd` is in fact hung and has to be killed with `--force`."*
//!
//! # What is asserted
//!
//! 1. **The stop ends the run and the daemon goes.** The bound is deliberately far below the
//!    run's own 120 s deadline, because *it eventually stopped when the command's deadline
//!    expired* is the behaviour being removed.
//! 2. **It says so**, on the session log, while the heads are still watching — the sentence
//!    the daemon publishes before it closes anything.
//! 3. And the head is told the truth either way: it does not leave on the `Bye`, because
//!    `Bye` is published by the connection thread before the worker has ended (that half is
//!    `crates/tui/tests/stop_daemon.rs`'s).
//!
//! # The run, and why it is `/bin/cat`
//!
//! The fixture has to be a run that **cannot end on its own**, or the stop arrives at nothing
//! and the test proves nothing: `su -c true` blocks only where it has to ask for a password,
//! so on a box whose session is already root inside its own namespace it ran `true` and was
//! gone milliseconds later. `/bin/cat` blocked on the terminal the daemon holds is the same
//! state everywhere, and it is also the state the first measurement above was about.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use letibot_sessionlog::client::{HeadClient, Inbound, pump};
use letibot_sessionlog::event::SessionEvent;
use letibot_sessionlog::protocol::{Caps, ServerFrame};

/// How long a live daemon may take to start, open a session and reach its worker.
const PATIENCE: Duration = Duration::from_secs(90);

/// **How long a stop may take while the operator's run is in flight.**
///
/// Far below the run's own deadline — which is the whole point, since *it stopped when the
/// command's two minutes expired* is the state this exists to remove — and far above the
/// milliseconds an orderly stop takes.
const STOP_BUDGET: Duration = Duration::from_secs(20);

fn scratch(tag: &str) -> PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = letibot_sessionlog::testing::scratch_root().join(format!(
        "letibot-stop-runs-{tag}-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn start_daemon(dir: &Path, log: &Path, socket: &Path, store: &Path, session: &str) -> Child {
    let out = std::fs::File::create(log).expect("the log file");
    let err = out.try_clone().expect("a second handle on the log");
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("the workspace root");
    let home = dir.join("home");
    std::fs::create_dir_all(&home).expect("a home with no config in it");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_harnessd"));
    cmd.arg("--socket")
        .arg(socket)
        .arg("--store")
        .arg(store)
        .arg("--workspace")
        .arg(dir)
        .arg("--session")
        .arg(session)
        .arg("--role")
        .arg("leticode")
        .arg("--bash")
        .arg("--adjudicator")
        .arg("console")
        .arg("--endpoint")
        .arg("127.0.0.1:1")
        .current_dir(repo)
        .env("HOME", &home)
        // The landing root, never the box's real `~/.local/share/letibot` (an inherited
        // `XDG_DATA_HOME` would reach it past the `HOME` above).
        .env("LETIBOT_LANDING_ROOT", home.join("landing"))
        .env("LETIBOT_LOGS_ROOT", home.join("logs"))
        .env("XDG_RUNTIME_DIR", dir)
        .stdin(Stdio::null())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err));
    // **A real daemon needs a vocabulary, and `--vocab` is how it is handed one.** Main made
    // that optional — a daemon on the byte vocabulary needs none — and this test's daemon is
    // a local-model one, so it exited with `no vocabulary` until this line existed. The
    // question is the same one the test's own `vocab_gguf` gate asks, so it is asked the
    // same way: the operator's `LETIBOT_VOCAB_GGUF` if it is set, else the box's own GGUF.
    if let Some(gguf) = std::env::var("LETIBOT_VOCAB_GGUF")
        .ok()
        .map(std::path::PathBuf::from)
        .or_else(letibot_harnessd::vocab_gguf)
    {
        cmd.arg("--vocab").arg(gguf);
    }
    cmd.spawn().expect("harnessd starts")
}

fn stop(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// **The daemon this test spawned, dead whatever happens to the test.**
///
/// Every failure in this file is a PANIC, and the path to one of them (the assertion that
/// the run never reached the state being measured) is reached *before* the daemon is any
/// use to anybody — so a plain `Child` left it running, on the box's temp space, with its
/// socket bound, for as long as the operator's session lived. A `Drop` is what makes
/// *whatever happens* true: the panic unwinds through it.
///
/// Killing is all this does, and `gone` still asks `try_wait` through `Deref` — the test
/// has to be able to see the daemon EXIT BY ITSELF (that is what it measures), so nothing
/// here may wait for it except `Drop` and [`stop`].
struct Daemon(Child);

impl std::ops::Deref for Daemon {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.0
    }
}

impl std::ops::DerefMut for Daemon {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn attach(
    socket: &Path,
    session: &str,
    log: &Path,
    daemon: &mut Child,
) -> (HeadClient, std::sync::mpsc::Receiver<Inbound>) {
    let deadline = Instant::now() + PATIENCE;
    loop {
        if let Ok((head, _hello, reader)) =
            HeadClient::attach(socket, session, 0, "tui", "dead", Caps::default())
        {
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || pump(reader, tx));
            return (head, rx);
        }
        if Instant::now() > deadline {
            let said = std::fs::read_to_string(log).unwrap_or_default();
            stop(daemon);
            panic!("the daemon never served {socket:?}; its own words were:\n{said}");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// **Is this pid gone, zombie-aware** — the head's own test, and the one that is a fact about
/// the process. A daemon this test spawned is a zombie the moment it exits, and `/proc` goes
/// on listing it until somebody waits.
fn gone(child: &mut Child) -> bool {
    matches!(child.try_wait(), Ok(Some(_)))
}

/// **The stop ends the run, and the daemon goes — not at the run's deadline.**
#[test]
fn a_stop_ends_the_run_that_holds_the_worker() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = scratch("stop");
    let log = dir.join("harnessd.stderr");
    let socket = dir.join("harnessd.sock");
    let store = dir.join("sessions.db");
    let session = "stop-ends-runs";
    let mut daemon = Daemon(start_daemon(&dir, &log, &socket, &store, session));
    let (mut head, rx) = attach(&socket, session, &log, &mut daemon);

    // ---- The run, and it must be one that CANNOT end on its own: a process blocked reading
    // the terminal the daemon holds for it, which is `/bin/cat` on every box and inside every
    // boundary.
    //
    // **It was `su -c true` on Linux and that fixture was the defect.** `su` blocks only where
    // it has to ask for a password; on a box whose session is already root inside its own
    // namespace it runs `true`, and the run is over a few milliseconds after it started. The
    // old precondition was the `operator_run_unreadable` warning, which is a statement about
    // the moment it was published — so the test sent its stop to a run that had ended and
    // proved nothing. MEASURED on a 24-core host with cgroups: the daemon closed its hubs
    // 11.4 ms after the stop and named no run, because there was none left to name.
    head.operator_shell(0, "! /bin/cat")
        .expect("the line is submitted");

    // **The daemon's own signal that the run still holds the worker**: the card it raises for
    // a run of the operator's that is reading its terminal — `Blocked` where `/proc` can be
    // read, `Unreadable` where it cannot. `Prompts` holds that run until `ended` settles the
    // card (`crates/harnessd/src/prompt.rs`), so a card that is up is a statement about the
    // moment the stop is sent and not about an earlier one.
    let deadline = Instant::now() + PATIENCE;
    let mut in_flight = false;
    let mut settled = false;
    while Instant::now() < deadline && !in_flight && !settled {
        let Ok(inbound) = rx.recv_timeout(Duration::from_millis(250)) else {
            continue;
        };
        if let ServerFrame::Event(env) = inbound.frame() {
            match &env.event {
                SessionEvent::PromptRequested { job, reading, .. } => {
                    eprintln!("---- the run's card: job={job} reading={reading:?} ----");
                    in_flight = true;
                }
                // The card came down without one going up: the run ended under this test, and
                // a stop with nothing running is not the state it is about.
                SessionEvent::PromptSettled { by, .. } => {
                    eprintln!("---- the run ended before the stop: {by} ----");
                    settled = true;
                }
                _ => {}
            }
        }
    }
    assert!(
        in_flight && !settled,
        "the run never reached the state this test is about, so nothing can be concluded"
    );

    // ---- The stop, and the clock on it.
    let started = Instant::now();
    head.stop(0, "dead").expect("the stop goes out");

    // What the daemon says about what it ended. Read while the socket is still up: the
    // sentence is published **before** the hubs close, which is what makes it land on the
    // session log rather than in a file nobody tails.
    //
    // **Read until the `Bye`, and not for a fixed five seconds.** A stop that has to kill a
    // tree waits on the cgroup emptying, and that wait is in seconds: five was enough to be
    // the whole window on a box where the killed tree is slow to leave, so the sentence was
    // published into a loop that had already given up. The bound here is the stop's own,
    // because the claim is *the daemon said it before it closed anything* and a clock of this
    // test's own cannot measure that.
    let mut sentence: Option<String> = None;
    let mut said_at: Option<Duration> = None;
    let mut bye_at: Option<Duration> = None;
    // **Every frame between the stop and the `Bye`, by name.** A red here is about ORDER, and the
    // order is only answerable from what actually arrived: the earlier report of this failure said
    // "the sentence after None" and could not say whether `daemon_stopping` or the `Accepted` came
    // at all, which is the difference between a sentence lost and a stop that never ran.
    let mut frames: Vec<String> = Vec::new();
    let read_until = started + STOP_BUDGET;
    while Instant::now() < read_until && sentence.is_none() && bye_at.is_none() {
        let Ok(inbound) = rx.recv_timeout(Duration::from_millis(250)) else {
            continue;
        };
        match inbound.frame() {
            ServerFrame::Event(env) => {
                if let SessionEvent::Warning { code, detail, .. } = &env.event {
                    frames.push(format!(
                        "{}ms warning {code}",
                        started.elapsed().as_millis()
                    ));
                    if code == "daemon_stopping_runs" && sentence.is_none() {
                        sentence = Some(detail.clone());
                        said_at = Some(started.elapsed());
                    }
                }
            }
            ServerFrame::Accepted { note, .. } => {
                frames.push(format!(
                    "{}ms accepted ({note})",
                    started.elapsed().as_millis()
                ));
            }
            ServerFrame::Bye { reason } if bye_at.is_none() => {
                frames.push(format!(
                    "{}ms bye ({reason})",
                    started.elapsed().as_millis()
                ));
                bye_at = Some(started.elapsed());
            }
            _ => {}
        }
    }

    // ---- And the process, which is the only thing that answers the operator's question.
    let deadline = Instant::now() + STOP_BUDGET;
    while Instant::now() < deadline && !gone(&mut daemon) {
        std::thread::sleep(Duration::from_millis(50));
    }
    let took = started.elapsed();
    let left = gone(&mut daemon);
    if !left {
        let said = std::fs::read_to_string(&log).unwrap_or_default();
        stop(&mut daemon);
        let _ = std::fs::remove_dir_all(&dir);
        panic!(
            "the daemon did not go within {STOP_BUDGET:?} of the stop — it is being held by \
             the run, and its own 120 s deadline is the only thing that would have ended it. \
             Its stderr was:\n{said}"
        );
    }

    eprintln!("---- the stop took {took:?} (the run's own deadline is 120 s) ----");
    eprintln!("---- the Bye arrived after {bye_at:?}, the sentence after {said_at:?} ----");
    eprintln!("---- what arrived between the stop and the Bye: {frames:?} ----");
    if let Some(s) = &sentence {
        eprintln!("---- what the daemon said it ended ----\n  {s}");
    }
    assert!(
        took < STOP_BUDGET,
        "the stop took {took:?}, which is the run's deadline rather than the daemon's stop"
    );
    assert!(
        sentence.is_some(),
        "the daemon ended a run on the way out and said nothing about it before it closed the \
         hubs (Bye after {bye_at:?}). *It killed the run* and *it killed the run and told you* \
         are different facts, and the second is the one that makes a stop auditable. What \
         arrived between the stop and the Bye: {frames:?}"
    );
    let said = sentence.unwrap();
    assert!(
        said.contains("stop") && said.contains("ended"),
        "the sentence has to say what happened and why: {said}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
