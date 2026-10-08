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

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use letibot_sessionlog::client::{HeadClient, Inbound, pump};
use letibot_sessionlog::event::SessionEvent;
use letibot_sessionlog::protocol::{Caps, ServerFrame};

/// How long a live daemon may take to start, open a session and reach its worker.
const PATIENCE: Duration = Duration::from_secs(90);

/// **How long a stop may take when a run holds the worker.**
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
    let dir = std::env::temp_dir().join(format!(
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
        .env("XDG_RUNTIME_DIR", dir)
        .stdin(Stdio::null())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err));
    cmd.spawn().expect("harnessd starts")
}

fn stop(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
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
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    if !Path::new("/usr/bin/su").exists() {
        eprintln!("no /usr/bin/su on this box — nothing to measure");
        return;
    }
    let dir = scratch("stop");
    let log = dir.join("harnessd.stderr");
    let socket = dir.join("harnessd.sock");
    let store = dir.join("sessions.db");
    let session = "stop-ends-runs";
    let mut daemon = start_daemon(&dir, &log, &socket, &store, session);
    let (mut head, rx) = attach(&socket, session, &log, &mut daemon);

    // ---- The run. The operator's own shape: a **root** process blocked on the pipe this
    // daemon holds, with its whole tree `EACCES` for this uid. It will not end on its own.
    head.operator_shell(0, "! /usr/bin/su -c true")
        .expect("the line is submitted");

    // **The run is in flight when the daemon says it cannot look at it** — that sentence is
    // published from the run's own wait loop, so it is a measurement of the run being there
    // and not a guess from a sleep.
    let deadline = Instant::now() + PATIENCE;
    let mut in_flight = false;
    while Instant::now() < deadline && !in_flight {
        let Ok(inbound) = rx.recv_timeout(Duration::from_millis(250)) else {
            continue;
        };
        if let ServerFrame::Event(env) = inbound.frame()
            && let SessionEvent::Warning { code, .. } = &env.event
            && code == "operator_run_unreadable"
        {
            in_flight = true;
        }
    }
    assert!(
        in_flight,
        "the run never reached the state this test is about, so nothing can be concluded"
    );

    // ---- The stop, and the clock on it.
    let started = Instant::now();
    head.stop(0, "dead").expect("the stop goes out");

    // What the daemon says about what it ended. Read while the socket is still up: the
    // sentence is published **before** the hubs close, which is what makes it land on the
    // session log rather than in a file nobody tails.
    let mut sentence: Option<String> = None;
    let mut bye_at: Option<Duration> = None;
    let read_until = Instant::now() + Duration::from_secs(5);
    while Instant::now() < read_until && sentence.is_none() {
        let Ok(inbound) = rx.recv_timeout(Duration::from_millis(250)) else {
            continue;
        };
        match inbound.frame() {
            ServerFrame::Event(env) => {
                if let SessionEvent::Warning { code, detail, .. } = &env.event
                    && code == "daemon_stopping_runs"
                {
                    sentence = Some(detail.clone());
                }
            }
            ServerFrame::Bye { .. } if bye_at.is_none() => {
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
    eprintln!("---- the Bye arrived after {bye_at:?} ----");
    if let Some(s) = &sentence {
        eprintln!("---- what the daemon said it ended ----\n  {s}");
    }
    assert!(
        took < STOP_BUDGET,
        "the stop took {took:?}, which is the run's deadline rather than the daemon's stop"
    );
    assert!(
        sentence.is_some(),
        "the daemon ended a run on the way out and said nothing about it — *it killed the \
         run* and *it killed the run and told you* are different facts, and the second is \
         the one that makes a stop auditable"
    );
    let said = sentence.unwrap();
    assert!(
        said.contains("stop") && said.contains("ended"),
        "the sentence has to say what happened and why: {said}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
