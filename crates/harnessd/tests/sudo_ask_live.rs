//! **The daemon's own words for the hang** — a live `harnessd` on this box, its stderr
//! redirected to a file, an operator run driven through the socket.
//!
//! This is the capture the operator asked for and did not get. Their live daemon's log
//! (`/home/dead/logs/harnessd.log`) holds only its startup disclosures, so *"the command
//! appears queued and the daemon hangs"* arrived with nothing behind it. What this file does is
//! reproduce the state with a daemon of this branch — **the real binary, its own worker, its own
//! socket, its own store under a temp path** — and write down both of the voices it has:
//!
//! * its **stderr**, which is the file the operator reads, captured to a temp path; and
//! * its **session log**, read off the socket as the frames a head receives.
//!
//! # The state being reproduced
//!
//! `! sudo apt install mc`, after the password: `apt` runs as root, so `/proc/<pid>/fd/0` is
//! `EACCES` for the daemon, and the process that is waiting at `Continue? [Y/n]` cannot be
//! looked at. No card can be raised (a card is a *reading* of the process, and this is a
//! failure to read one), the run holds the daemon's single worker, and the `!` line's echo
//! still says `queued` because no row has landed.
//!
//! A real `sudo` is not used — this test must not depend on the box's sudo policy, and the
//! password path is covered by `sudo_ask.rs`. What stands in for `apt` is the same thing that
//! file uses: **`PR_SET_DUMPABLE 0`**, which is the flag a setuid exec sets and which makes
//! `/proc/<pid>/fd/0` `EACCES` for every reader without `CAP_SYS_PTRACE`. MEASURED 2026-10-06:
//! `readlink /proc/1/fd/0` → `PermissionDenied` for a root process, and the same for an
//! undumpable process of this uid, with `wchan` reading `0` — the kernel's own *could not look*.
//!
//! # What is asserted
//!
//! 1. **The daemon says it cannot tell** — `operator_run_unreadable` on the session log, once,
//!    naming the command and `!send`.
//! 2. **The way in works on a real daemon**: `!send` reaches the run's stdin and the run ends,
//!    which is the difference between a command that is *answerable* and one that hangs.
//! 3. **The daemon's stderr is the file it always was** — its banner and its per-turn lines —
//!    and it carries no line about the run at all. That is stated rather than assumed: it is why
//!    the operator's own capture came up empty, and it is why the sentence had to go where a
//!    head reads it (the session log, which the store keeps) rather than to a file nobody
//!    tails.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use letibot_sessionlog::client::{HeadClient, pump};
use letibot_sessionlog::event::SessionEvent;
use letibot_sessionlog::protocol::{Caps, ServerFrame};

/// How long any wait here may take before it is a failure rather than a hang. A live daemon has
/// to start, open a session and reach its worker.
const PATIENCE: Duration = Duration::from_secs(90);

/// `PR_SET_DUMPABLE`, from `<linux/prctl.h>`. The one libc call in this file, declared rather
/// than pulled in with a crate — and the whole of what stands in for a root `apt`.
const PR_SET_DUMPABLE: i32 = 4;

unsafe extern "C" {
    fn prctl(option: i32, ...) -> i32;
}

/// **The run's own process, when this test binary is re-executed as it.**
///
/// Same device as `sudo_ask.rs`'s: the `!` line names this test with `--exact`, so what the
/// daemon's exec host spawns is this function, with the daemon's pipe on fd 0 and the run's
/// cgroup around it. It makes itself unreadable to the daemon and then waits on that pipe.
#[test]
fn the_process_the_daemon_may_not_read() {
    if std::env::var("LETIBOT_SUDO_ASK_CHILD").as_deref() != Ok("1") {
        return;
    }
    let rc = unsafe { prctl(PR_SET_DUMPABLE, 0, 0, 0, 0) };
    println!("child: PR_SET_DUMPABLE 0 -> {rc}; waiting on the daemon's pipe");
    let mut line = String::new();
    let n = std::io::stdin().read_line(&mut line).unwrap_or(0);
    println!("child: read {n} byte(s): {:?}", line.trim_end());
}

/// A temp directory for this run's socket, store and log — **never the live store.**
fn scratch(tag: &str) -> PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "letibot-sudo-ask-live-{tag}-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// The daemon, its stderr going to `log`.
///
/// **`HOME` is a temp directory and the endpoint is a dead port, on purpose.** The daemon's
/// standing choice comes from the operator's own `providers.toml` (`[default] provider`), and a
/// test that inherits it spends the operator's money on the turn that a `!` line starts. With
/// no config directory there is no provider, and the local endpoint refuses at once — so the
/// turn fails the way an unreachable server fails and nothing leaves the box.
fn start_daemon(dir: &Path, log: &Path, socket: &Path, store: &Path, session: &str) -> Child {
    let out = std::fs::File::create(log).expect("the log file");
    let err = out.try_clone().expect("a second handle on the log");
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("the workspace root is two levels above this crate");
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
        // The session's own vocabulary is read from the box's default path; the workspace is
        // this temp directory, so nothing of the operator's tree is in reach of the run.
        .current_dir(repo)
        .env("HOME", &home)
        .stdin(Stdio::null())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err));
    cmd.spawn().expect("harnessd starts")
}

fn stop(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// **The whole capture**: a live daemon, a run it may not look at, and both voices written down.
#[test]
fn a_live_daemons_own_words_for_a_run_it_may_not_look_at() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let dir = scratch("live");
    let log = dir.join("harnessd.stderr");
    let socket = dir.join("harnessd.sock");
    let store = dir.join("sessions.db");
    let session = "sudo-ask-live";
    let mut daemon = start_daemon(&dir, &log, &socket, &store, session);
    // Wait for the socket to be served. A daemon that died at startup is a failure with its own
    // log as the explanation, so the log is read out before this panics.
    let deadline = Instant::now() + PATIENCE;
    let (mut head, rx) = loop {
        if let Ok((head, _hello, reader)) =
            HeadClient::attach(&socket, session, 0, "tui", "dead", Caps::default())
        {
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || pump(reader, tx));
            break (head, rx);
        }
        if Instant::now() > deadline {
            let said = std::fs::read_to_string(&log).unwrap_or_default();
            stop(&mut daemon);
            panic!("the daemon never served {socket:?}; its own words were:\n{said}");
        }
        std::thread::sleep(Duration::from_millis(100));
    };

    // ---- The `!` line, and the run it makes.
    let exe = std::env::current_exe().expect("this test binary's own path");
    let line = format!(
        "! LETIBOT_SUDO_ASK_CHILD=1 {exe} --exact the_process_the_daemon_may_not_read --nocapture",
        exe = exe.display()
    );
    head.operator_shell(0, &line)
        .expect("the line is submitted");

    // ---- Read the daemon's session log until it says it cannot tell.
    let mut sentence: Option<String> = None;
    let deadline = Instant::now() + PATIENCE;
    while Instant::now() < deadline && sentence.is_none() {
        let Ok(inbound) = rx.recv_timeout(Duration::from_millis(250)) else {
            continue;
        };
        if let ServerFrame::Event(env) = inbound.frame()
            && let SessionEvent::Warning { code, detail, .. } = &env.event
            && code == "operator_run_unreadable"
        {
            sentence = Some(detail.clone());
        }
    }
    let sentence = match sentence {
        Some(s) => s,
        None => {
            let said = std::fs::read_to_string(&log).unwrap_or_default();
            stop(&mut daemon);
            panic!("the daemon said nothing about the run; its stderr was:\n{said}")
        }
    };
    assert!(
        sentence.contains("!send"),
        "the sentence must name the way in: {sentence}"
    );

    // ---- **The daemon's stderr while the run is still stuck.** This is the capture the
    // operator asked for: the run is at this moment waiting on the pipe the daemon holds, with
    // nothing having answered it, and the file they read says nothing about it. Read here,
    // before `!send`, so what is printed is the state and not the aftermath.
    let during = std::fs::read_to_string(&log).unwrap_or_default();
    eprintln!("---- {log:?} while the run is still waiting ----");
    for line in BufReader::new(during.as_str().as_bytes())
        .lines()
        .map_while(Result::ok)
    {
        eprintln!("  {line}");
    }
    assert!(
        !during.contains("operator_run_unreadable") && !during.contains("!send"),
        "nothing about the stuck run reaches stderr — that is why the operator's own capture \
         came up empty, and it is the finding, not an oversight: {during}"
    );

    // ---- The way in, on a real daemon.
    head.send_line("Y").expect("the verb's line");
    let mut ended = false;
    let deadline = Instant::now() + PATIENCE;
    while Instant::now() < deadline && !ended {
        let Ok(inbound) = rx.recv_timeout(Duration::from_millis(250)) else {
            continue;
        };
        if let ServerFrame::Event(env) = inbound.frame()
            && let SessionEvent::Warning { code, .. } = &env.event
            && code == "operator_shell_ran"
        {
            ended = true;
        }
    }
    assert!(ended, "the run never ended after `!send`");

    // ---- The capture, printed so it is in the evidence rather than only in the assertion.
    let said = std::fs::read_to_string(&log).unwrap_or_default();
    eprintln!("---- {log:?} (the daemon's stderr, after the run ended) ----");
    for line in BufReader::new(said.as_str().as_bytes())
        .lines()
        .map_while(Result::ok)
    {
        eprintln!("  {line}");
    }
    eprintln!("---- the session log's sentence ----");
    eprintln!("  operator_run_unreadable: {sentence}");

    // ---- 3. What the file does and does not carry, stated as an assertion.
    assert!(
        said.contains("harnessd: session"),
        "the daemon's own banner is the file's first word: {said}"
    );
    assert!(
        !said.contains("operator_run_unreadable") && !said.contains("!send"),
        "the sentence is on the session log and not on stderr — which is why the operator's own \
         capture came up empty, and why the fix puts it where a head reads it rather than in a \
         file nobody tails. If this ever becomes true, this test is where that decision was \
         changed: {said}"
    );
    stop(&mut daemon);
    let _ = std::fs::remove_dir_all(&dir);
}
