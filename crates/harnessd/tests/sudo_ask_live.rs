//! A live `harnessd` on this box, its stderr to a file, and an operator `!` run whose
//! waiting process it may not look at.

// `PR_SET_DUMPABLE` is how these tests make a process this uid may not inspect, and it is
// `<linux/prctl.h>`: there is no macOS spelling of the same shape.
#![cfg(target_os = "linux")]

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use letibot_sessionlog::client::{HeadClient, Inbound, pump};
use letibot_sessionlog::event::SessionEvent;
use letibot_sessionlog::protocol::{Caps, ServerFrame};

/// How long any wait here may take before it is a failure rather than a hang. A live daemon has
/// to start, open a session and reach its worker.
const PATIENCE: Duration = Duration::from_secs(90);

/// `PR_SET_DUMPABLE`, from `<linux/prctl.h>`. The one libc call in this file, declared rather
/// than pulled in with a crate - and the whole of what stands in for a root `apt`.
const PR_SET_DUMPABLE: i32 = 4;

unsafe extern "C" {
    fn prctl(option: i32, ...) -> i32;
}

/// The run's own process, when this test binary is re-executed as it. The `!` line names this
/// test with `--exact`, so the daemon's exec host spawns this function with the daemon's terminal
/// on fd 0; it makes itself unreadable and then waits on that terminal.
#[test]
fn the_process_the_daemon_may_not_read() {
    if std::env::var("LETIBOT_SUDO_ASK_CHILD").as_deref() != Ok("1") {
        return;
    }
    let rc = unsafe { prctl(PR_SET_DUMPABLE, 0, 0, 0, 0) };
    println!("child: PR_SET_DUMPABLE 0 -> {rc}; waiting on the daemon's terminal");
    let mut line = String::new();
    let n = std::io::stdin().read_line(&mut line).unwrap_or(0);
    println!("child: read {n} byte(s): {:?}", line.trim_end());
}

/// A temp directory for this run's socket, store and log - never the live store.
fn scratch(tag: &str) -> PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = letibot_sessionlog::testing::scratch_root().join(format!(
        "letibot-sudo-ask-live-{tag}-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// The daemon, its stderr going to `log`. `HOME` is a temp directory and the endpoint is a
/// dead port, on purpose
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
        // The landing root, never the box's real `~/.local/share/letibot` (an inherited
        // `XDG_DATA_HOME` would reach it past the `HOME` above).
        .env("LETIBOT_LANDING_ROOT", home.join("landing"))
        .env("LETIBOT_LOGS_ROOT", home.join("logs"));
    // A real daemon must be handed a vocabulary, and `--vocab` is how. Without it the daemon
    // exits with `no vocabulary` and the test fails at "the daemon never served", which reads like
    // a broken test rather than a missing argument.
    if let Some(gguf) = std::env::var("LETIBOT_VOCAB_GGUF")
        .ok()
        .map(std::path::PathBuf::from)
        .or_else(letibot_harnessd::vocab_gguf)
    {
        cmd.arg("--vocab").arg(gguf);
    }
    cmd
        // The workspace is this temp directory, so nothing of the operator's tree is in reach
        // of the run.
        .current_dir(repo)
        .env("HOME", &home)
        // `XDG_RUNTIME_DIR` is this temp directory, and that is not tidiness: `sudo::install`
        // writes its shim under the runtime dir the operator's LIVE daemon uses, so inheriting it
        // would write into a running box's shim path. The shim is the same file either way.
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

/// The whole capture: a live daemon, a run it may not look at, and both voices written down.
#[test]
fn a_live_daemons_own_words_for_a_run_it_may_not_look_at() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = scratch("live");
    let log = dir.join("harnessd.stderr");
    let socket = letibot_sessionlog::testing::socket_path(&dir, "harnessd.sock");
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

    // ---- The daemon's stderr while the run is still stuck: it waits on the device the daemon
    // holds for it, nothing has answered it, and the file the operator reads says nothing about it.
    // Read before `!send`, so this is the state and not the aftermath.
    let during = std::fs::read_to_string(&log).unwrap_or_default();
    eprintln!("---- {log:?} while the run is still waiting ----");
    for line in BufReader::new(during.as_bytes())
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
    for line in BufReader::new(said.as_bytes())
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

/// The operator's own shape, with a REAL root process in it - `/usr/bin/su`, setuid, reading
/// the password from the run's own pty. The stand-in above proves the *detection*; this proves the
/// *report* for a tree that is root and undumpable, and that `!send` still reaches it.
#[test]
fn a_real_root_process_waiting_on_the_terminal_is_the_operators_own_shape() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let su = Path::new("/usr/bin/su");
    if !su.exists() {
        eprintln!("no {} on this box — nothing to reproduce", su.display());
        return;
    }
    let dir = scratch("root");
    let log = dir.join("harnessd.stderr");
    let socket = letibot_sessionlog::testing::socket_path(&dir, "harnessd.sock");
    let store = dir.join("sessions.db");
    let session = "sudo-ask-root";
    let mut daemon = start_daemon(&dir, &log, &socket, &store, session);
    let (mut head, rx) = attach(&socket, session, &log, &mut daemon);

    // ---- The `!` line. See this test's docs for why it is this program.
    head.operator_shell(0, "! /usr/bin/su -c true")
        .expect("the line is submitted");

    // ---- The tree, read while the run is stuck: printed rather than asserted, because what the
    // kernel says about these processes is the fact under test and a box whose `su` differs would
    // fail the assertion for reasons that are not this daemon's.
    let mut sentence: Option<String> = None;
    let deadline = Instant::now() + PATIENCE;
    let mut tree_printed = false;
    while Instant::now() < deadline && sentence.is_none() {
        let Ok(inbound) = rx.recv_timeout(Duration::from_millis(250)) else {
            continue;
        };
        if let ServerFrame::Event(env) = inbound.frame()
            && let SessionEvent::Warning { code, detail, .. } = &env.event
            && code == "operator_run_unreadable"
        {
            sentence = Some(detail.clone());
            if !tree_printed {
                tree_printed = true;
                eprintln!("---- the run's own tree, at the moment the daemon spoke ----");
                for line in run_tree() {
                    eprintln!("  {line}");
                }
            }
        }
    }
    let sentence = match sentence {
        Some(s) => s,
        None => {
            let said = std::fs::read_to_string(&log).unwrap_or_default();
            stop(&mut daemon);
            panic!(
                "the daemon never said it could not tell about a root process waiting on its \
                 own terminal; the tree was:\n{}\nits stderr was:\n{said}",
                run_tree().join("\n")
            )
        }
    };
    eprintln!("---- the sentence, for a real root child ----");
    eprintln!("  operator_run_unreadable: {sentence}");
    assert!(
        sentence.contains("/usr/bin/su -c true"),
        "the sentence must name the command the person typed: {sentence}"
    );
    assert!(
        sentence.contains("!send"),
        "the sentence must name the way in: {sentence}"
    );

    // ---- And the way in, on a real root process: the line goes down the terminal, `su` reads it
    // as a password and refuses it. The point is not that the password is wrong - it is that a
    // line the operator typed reached the program that was waiting.
    head.send_line("not-the-password").expect("the verb's line");
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
    stop(&mut daemon);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        ended,
        "the run never ended after `!send`: the line did not reach the root process's terminal"
    );
}

/// The operator's command, verbatim, against the box's real `sudo` - the daemon's own shim and
/// `SUDO_ASKPASS`, the password card answered by a head. A wrong password is typed on purpose: the
/// question is not whether the install succeeds but whether the daemon says anything at all.
#[test]
fn the_operators_own_sudo_line_says_something_while_it_is_stuck() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    if !Path::new("/usr/bin/sudo").exists() {
        eprintln!("no sudo on this box — nothing to reproduce");
        return;
    }
    let dir = scratch("sudo");
    let log = dir.join("harnessd.stderr");
    let socket = letibot_sessionlog::testing::socket_path(&dir, "harnessd.sock");
    let store = dir.join("sessions.db");
    let session = "sudo-ask-own";
    let mut daemon = start_daemon(&dir, &log, &socket, &store, session);
    let (mut head, rx) = attach(&socket, session, &log, &mut daemon);

    head.operator_shell(0, "! sudo apt install mc")
        .expect("the line is submitted");

    // ---- The password card. `sudo` asks again and then gives up; the point is that the card was
    // served while the run was blocked.
    let mut sentence: Option<String> = None;
    let mut card_answered = false;
    let deadline = Instant::now() + PATIENCE;
    while Instant::now() < deadline && sentence.is_none() {
        let Ok(inbound) = rx.recv_timeout(Duration::from_millis(250)) else {
            continue;
        };
        if let ServerFrame::Event(env) = inbound.frame() {
            match &env.event {
                SessionEvent::Warning { code, detail, .. } if code == "operator_run_unreadable" => {
                    sentence = Some(detail.clone());
                }
                // The password card. `sudo` asks again and then gives up; the point is that the
                // card was served while the run was blocked.
                SessionEvent::SecretRequested { req_id, .. } if !card_answered => {
                    card_answered = true;
                    eprintln!("---- the password card is up for {req_id} ----");
                    head.secret(req_id, Some("definitely-not-the-password".to_string()))
                        .expect("the card is answered");
                }
                _ => {}
            }
        }
    }
    let said = std::fs::read_to_string(&log).unwrap_or_default();
    let tail: Vec<&str> = said.lines().rev().take(12).collect();
    eprintln!("---- the daemon's own words while the run was stuck (last 12) ----");
    for line in tail.iter().rev() {
        eprintln!("  {line}");
    }
    eprintln!("---- the sentence ----");
    eprintln!("  {sentence:?}");
    stop(&mut daemon);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        card_answered,
        "the password card never came up, so this did not reproduce the operator's sequence"
    );
    assert!(
        sentence.is_some(),
        "the daemon said nothing at all about `! sudo apt install mc` while it was stuck. Its \
         own words were:\n{said}"
    );
}

/// A run that is still WRITING while one of its processes is blocked on the terminal.
#[test]
fn a_run_still_writing_is_still_a_run_this_daemon_cannot_look_at() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = scratch("ticker");
    let log = dir.join("harnessd.stderr");
    let socket = letibot_sessionlog::testing::socket_path(&dir, "harnessd.sock");
    let store = dir.join("sessions.db");
    let session = "sudo-ask-ticker";
    let mut daemon = start_daemon(&dir, &log, &socket, &store, session);
    let (mut head, rx) = attach(&socket, session, &log, &mut daemon);

    // ---- The run: a writer every 100 ms, and beside it a process that is not this daemon's to
    // look at, blocked on the terminal the daemon holds - `apt`'s shape while it works, with the
    // question one keystroke away.
    let exe = std::env::current_exe().expect("this test binary's own path");
    let script = dir.join("streaming-run.sh");
    letibot_tools::testing::write_stub(
        &script,
        &format!(
            "#!/bin/sh\n\
             # letibot test double for `apt install mc` while it streams: a writer that never\n\
             # lets the run be quiet for a beat, over a process of this run that cannot be\n\
             # looked at and is waiting on the daemon's terminal.\n\
             # The `!` line runs this with NO arguments, so `$#` cannot tell the probe from the\n\
             # real thing: `letibot_tools::testing::write_stub` sets LETIBOT_STUB_PROBE in the\n\
             # probe's environment instead, and this is where it is read.\n\
             [ -n \"$LETIBOT_STUB_PROBE\" ] && exit 0\n\
             while true; do printf 'tick\\n'; sleep 0.1; done &\n\
             LETIBOT_SUDO_ASK_CHILD=1 {exe} --exact the_process_the_daemon_may_not_read \\\n\
               --nocapture\n",
            exe = exe.display()
        ),
    );
    head.operator_shell(0, &format!("! {}", script.display()))
        .expect("the line is submitted");

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
    let said = std::fs::read_to_string(&log).unwrap_or_default();
    let tail: Vec<&str> = said.lines().rev().take(12).collect();
    eprintln!("---- the daemon's own words, streaming run (last 12) ----");
    for line in tail.iter().rev() {
        eprintln!("  {line}");
    }
    stop(&mut daemon);
    let _ = std::fs::remove_dir_all(&dir);
    let sentence = sentence.unwrap_or_else(|| {
        panic!(
            "a run that is still writing, with a process this daemon may not look at blocked \
             on its own terminal, said NOTHING. That is the operator's second report word for word: \
             `harnessd hangs without printing anything to me`. The daemon's own words were:\n{said}"
        )
    });
    eprintln!("---- the sentence, for a run that is still writing ----");
    eprintln!("  operator_run_unreadable: {sentence}");
    assert!(
        !sentence.contains("has been quiet for a beat"),
        "the sentence claims a quiet beat this run never earned — it was writing throughout, \
         which is the one thing the person cannot see and the one thing the sentence has to \
         tell them: {sentence}"
    );
    assert!(
        sentence.contains("!send"),
        "the sentence must name the way in: {sentence}"
    );
}

/// The operator's own run has no deadline - not even the 120 s one. The 120 s default is
/// for a command the MODEL did not ask to run longer
#[test]
fn the_operators_own_run_outlives_the_models_deadline() {
    if std::env::var("LETIBOT_DEADLINE_LIVE").as_deref() != Ok("1") {
        eprintln!(
            "LETIBOT_DEADLINE_LIVE=1 to spend over two minutes on the operator's no-deadline run"
        );
        return;
    }
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    if !Path::new("/usr/bin/su").exists() {
        eprintln!("no /usr/bin/su on this box — nothing to measure");
        return;
    }
    let dir = scratch("deadline");
    let log = dir.join("harnessd.stderr");
    let socket = letibot_sessionlog::testing::socket_path(&dir, "harnessd.sock");
    let store = dir.join("sessions.db");
    let session = "sudo-ask-deadline";
    let mut daemon = start_daemon(&dir, &log, &socket, &store, session);
    let (mut head, rx) = attach(&socket, session, &log, &mut daemon);

    // ---- The run: `su` as root, reading the terminal the daemon holds, so it waits forever and
    // this uid cannot signal it. The operator's `!` line, which arms no deadline at all.
    let started = Instant::now();
    head.operator_shell(0, "! /usr/bin/su -c true")
        .expect("the line is submitted");

    // ---- The window: PAST the old kill time, and it never exits early on any row, so the
    // assertions below say what did and did not arrive.
    let mut ended = false;
    let mut rows = String::new();
    let mut unreadable: Option<String> = None;
    let window = Instant::now() + Duration::from_secs(125);
    while Instant::now() < window {
        let Ok(inbound) = rx.recv_timeout(Duration::from_millis(250)) else {
            continue;
        };
        if let ServerFrame::Event(env) = inbound.frame() {
            match &env.event {
                SessionEvent::Warning { code, .. } if code == "operator_shell_ran" => ended = true,
                // The sentence for a run whose waiting process this daemon may not look at
                // the daemon SPEAKING is the not-hung half of what this test asserts.
                SessionEvent::Warning { code, detail, .. } if code == "operator_run_unreadable" => {
                    unreadable = Some(detail.clone());
                }
                // The row the run produced, had it ended - none may arrive claiming a kill.
                SessionEvent::TranscriptContent { item, .. } => {
                    if let letibot_transcript::TranscriptItem::ToolResult { payload, .. } =
                        item.as_ref()
                    {
                        rows.push_str(payload);
                        rows.push('\n');
                    }
                }
                _ => {}
            }
        }
    }
    let took = started.elapsed();
    stop(&mut daemon);
    let _ = std::fs::remove_dir_all(&dir);
    eprintln!("---- {took:?} in, past the old kill time; the rows that arrived said ----");
    for line in rows.lines().filter(|l| !l.trim().is_empty()).take(12) {
        eprintln!("  {line}");
    }
    assert!(
        !ended && !rows.contains("outlived its deadline") && !rows.contains("was killed after"),
        "the operator's own run met a clock {took:?} in: {rows}. `deadline_for` hands the \
         operator's run no deadline, and this was to be the live proof that none is armed"
    );
    assert!(
        unreadable.is_some(),
        "the daemon said nothing while the run sat — silent while waiting is the \
         hung-not-busy state the operator reported, and removing the deadline must not \
         bring it back"
    );
}

/// The `!` line's tree, as the box's own `ps` reports it, for the evidence: the shape under
/// test is whose uid each process is and where each one sits, and `ps` already renders that.
fn run_tree() -> Vec<String> {
    let out = Command::new("ps")
        .args(["-eo", "pid,ppid,user,stat,wchan:22,args", "--forest"])
        .output();
    let Ok(out) = out else {
        return vec!["ps did not run".to_string()];
    };
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    text.lines()
        .filter(|l| l.contains("su ") || l.contains("bash -ic") || l.contains("harnessd"))
        .map(|l| l.trim_end().to_string())
        .collect()
}

/// Attach a head, waiting for the daemon to serve its socket, with the daemon's own log as the
/// explanation when it never does.
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
