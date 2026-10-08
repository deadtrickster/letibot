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
//!
//! # The other three, which are the operator's second report
//!
//! The three above are one half of the case. The other half is the report they made **after**
//! they had seen the sentence once — *"so i start it completely fresh and do apt install and
//! harnessd hangs without printing anything to me"* — and each of the three tests below is one
//! of the facts that made that run silent:
//!
//! 4. `a_real_root_process_waiting_on_the_terminal_is_the_operators_own_shape` — the same case
//!    with a **real root process** in the tree (`/usr/bin/su`, setuid, reading the terminal)
//!    rather than a stand-in of this uid. The detection must not depend on the stand-in being
//!    faithful.
//! 5. `the_operators_own_sudo_line_says_something_while_it_is_stuck` — `! sudo apt install mc`,
//!    character for character, through the daemon's own shim and `SUDO_ASKPASS`, with the
//!    password card answered by a head.
//! 6. `a_run_still_writing_is_still_a_run_this_daemon_cannot_look_at` — **the one that was
//!    silent**. The unreadable report used to be gated behind the 250 ms quiet beat the *card*
//!    needs, and `apt` streams its progress, so a run nobody could see the output of never had
//!    the beat and was never reported on. It is reported either way now, and the sentence says
//!    which of the two facts it is.

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
/// than pulled in with a crate — and the whole of what stands in for a root `apt`.
const PR_SET_DUMPABLE: i32 = 4;

unsafe extern "C" {
    fn prctl(option: i32, ...) -> i32;
}

/// **The run's own process, when this test binary is re-executed as it.**
///
/// Same device as `sudo_ask.rs`'s: the `!` line names this test with `--exact`, so what the
/// daemon's exec host spawns is this function, with the daemon's **terminal** on fd 0 and the
/// run's cgroup around it. It makes itself unreadable to the daemon and then waits on that
/// terminal.
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
        // **`XDG_RUNTIME_DIR` is this temp directory, and that is not tidiness.**
        // `sudo::install()` writes its shim to `$XDG_RUNTIME_DIR/letibot/shims/sudo`, which is
        // the directory the operator's LIVE daemon and its sessions use. A test that inherited
        // it would be writing into a running box's shim path; pointing it here makes the test
        // hermetic, and the shim it writes is the same file either way.
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
    // operator asked for: the run is at this moment waiting on the device the daemon holds for
    // it, with nothing having answered it, and the file they read says nothing about it. Read here,
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

/// **The operator's own shape, with a REAL root process in it** — not a stand-in.
///
/// The test above makes a process of *this* uid undumpable, which `sudo_ask.rs` measured to be
/// the same `/proc` refusal a root `apt` gives. That is a stand-in, and a stand-in is exactly
/// where a reproduction stops reproducing: the operator's report is about a process that is
/// **root**, and the difference between *same uid, undumpable* and *another user* is the one
/// `ask`'s own module header names as the two ways `EACCES` happens.
///
/// So this test uses neither a stand-in nor a fake: `/usr/bin/su` is setuid root on this box,
/// and it reads the password **from the terminal** — which is the run's own pty, the one this
/// daemon holds the master of. That is `apt` at `Continue? [Y/n]` in every fact that matters: a
/// root process, undumpable, blocked in a read on the device it was given, with the operator's
/// own shell above it in `wait4`. `! /usr/bin/su -c true` is `! sudo apt install mc` with the
/// password step taken out, and the assertion is the same one: the daemon says it cannot tell.
///
/// **And `su` is the trade in the wild, which is worth saying here rather than in the abstract.**
/// It used to read fd 0 because there was no controlling terminal for it to open; with the pty
/// as the run's controlling terminal it calls `setsid` and takes it — measured, 2026-10-07, the
/// process appears as `Ss+` in the run's tree, a *session leader in the foreground process
/// group*, which is the state that only exists on a controlling terminal. It reads the password
/// from there instead of from fd 0, and **`!send` still reaches it**, because `!send` writes to
/// the master of that same terminal. That is the whole of the trade `exec::pty`'s header states:
/// a program reached **indirectly** that opens `/dev/tty` now *waits* instead of failing at
/// once — and this test is the evidence that it is answerable rather than lost.
///
/// # Why this is a second test and not the first one written differently
///
/// The stand-in proves the *detection*. This proves the *report* for the tree the operator
/// actually had, and it is the only version of the case that can be checked against the
/// operator's own daemon, which is running as this uid against a root child.
#[test]
fn a_real_root_process_waiting_on_the_terminal_is_the_operators_own_shape() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let su = Path::new("/usr/bin/su");
    if !su.exists() {
        eprintln!("no {} on this box — nothing to reproduce", su.display());
        return;
    }
    let dir = scratch("root");
    let log = dir.join("harnessd.stderr");
    let socket = dir.join("harnessd.sock");
    let store = dir.join("sessions.db");
    let session = "sudo-ask-root";
    let mut daemon = start_daemon(&dir, &log, &socket, &store, session);
    let (mut head, rx) = attach(&socket, session, &log, &mut daemon);

    // ---- The `!` line. See this test's docs for why it is this program.
    head.operator_shell(0, "! /usr/bin/su -c true")
        .expect("the line is submitted");

    // ---- The tree, read from `/proc` while the run is stuck. This is the evidence the
    // reproduction is faithful, and it is printed rather than asserted: what the kernel says
    // about these processes is the fact under test, and a test that asserted the shape would
    // fail on a box whose `su` behaves differently for reasons that are not this daemon's.
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

    // ---- And the way in, on a real root process: the line goes down the terminal, `su` reads
    // it as a password, refuses it, and the run ends. The point is not that the password is
    // wrong — it is that a line the operator typed **reached the program that was waiting**,
    // which is the difference between a command that is stuck and one that is answerable.
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

/// **The operator's command, verbatim, against the box's real `sudo`.**
///
/// Everything above is a stand-in for one half of `! sudo apt install mc`. This is the report
/// itself: the same line, the daemon's own shim and `SUDO_ASKPASS`, the password card answered
/// by a head, and then the run's tree — `bash` in `wait4`, over `sudo` as **root**, over
/// `letibot-askpass` — which is the tree the operator had. A wrong password is typed, on
/// purpose: what is being measured is not whether the install succeeds but whether the daemon
/// says *anything at all* while the run is stuck, and a run that is stuck waiting for a
/// password is the same silence with a shorter fuse.
///
/// It is here rather than left to a live session because this is the one test whose `!` line is
/// the operator's, character for character.
#[test]
fn the_operators_own_sudo_line_says_something_while_it_is_stuck() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    if !Path::new("/usr/bin/sudo").exists() {
        eprintln!("no sudo on this box — nothing to reproduce");
        return;
    }
    let dir = scratch("sudo");
    let log = dir.join("harnessd.stderr");
    let socket = dir.join("harnessd.sock");
    let store = dir.join("sessions.db");
    let session = "sudo-ask-own";
    let mut daemon = start_daemon(&dir, &log, &socket, &store, session);
    let (mut head, rx) = attach(&socket, session, &log, &mut daemon);

    head.operator_shell(0, "! sudo apt install mc")
        .expect("the line is submitted");

    // ---- The password card, answered with something that is not the password. `sudo` will
    // ask again and then give up; the point is that the card was served while the run was
    // blocked, which is the operator's own sequence.
    let mut sentence: Option<String> = None;
    let mut card_answered = false;
    let deadline = Instant::now() + PATIENCE;
    while Instant::now() < deadline && sentence.is_none() {
        let Ok(inbound) = rx.recv_timeout(Duration::from_millis(250)) else {
            continue;
        };
        match inbound.frame() {
            ServerFrame::Event(env) => match &env.event {
                SessionEvent::Warning { code, detail, .. } if code == "operator_run_unreadable" => {
                    sentence = Some(detail.clone());
                }
                // The password card, answered with something that is not the password. `sudo`
                // asks again and then gives up; the point is that the card was served while
                // the run was blocked, which is the operator's own sequence.
                SessionEvent::SecretRequested { req_id, .. } if !card_answered => {
                    card_answered = true;
                    eprintln!("---- the password card is up for {req_id} ----");
                    head.secret(req_id, Some("definitely-not-the-password".to_string()))
                        .expect("the card is answered");
                }
                _ => {}
            },
            _ => {}
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

/// **A run that is still WRITING while one of its processes is blocked on the terminal.**
///
/// # The operator's own report, and the one fact the daemon was not looking at
///
/// Their second report, on a fresh daemon, is not the one the sentence above answers:
///
/// > *"so i start it completely fresh and do apt install and harnessd hangs without printing
/// > anything to me. I suppose it waits for y or n but doesnt show me anything"*
///
/// And their own transcript records what `apt` does in that run, in as many words:
///
/// > `! sudo apt install mc` **streamed its progress** and then aborted at `Continue? [Y/n]`
///
/// So the run they were looking at was **producing output** — apt's own progress — while the
/// process that would eventually ask was one the daemon may not look at. And the daemon said
/// nothing, because the unreadable report is gated behind the **same 250 ms quiet beat the
/// card needs**: `ask_if_waiting` returns before it ever asks when `since_last_output` is
/// under a beat, and a run writing every 100 ms never has one.
///
/// # Why the two facts are not the same fact
///
/// The beat is right for the **card**. A card claims *some process of this run is blocked
/// reading the answer we hold*, and a run that is still drawing is genuinely not blocked —
/// that is the operator's own correction (`"i think Continue? is an overfit"`) turned into a
/// rule about the process.
///
/// `Unreadable` is the opposite kind of thing: it is the **absence** of a reading — *one of
/// this run's processes is not mine to look at* — and that is a fact about **permissions**, not
/// about the clock. It is exactly as true at a 100 ms write interval as at a 2 s one. Gating
/// it behind the beat is what made the daemon silent for a person who could see no output at
/// all: the run's bytes land only when it ends, so *working* and *blocked* look the same to
/// them, and the one sentence that would have told them the difference was never said.
///
/// # What is asserted
///
/// 1. **The daemon says it cannot tell**, on this path too — the sentence is published while
///    the run is still writing.
/// 2. **And it says which of the two facts it is**: the run is still writing, so the sentence
///    must not claim the quiet beat it did not earn. That is the sibling of the askpass
///    deadline's sentence (`no head answered before the deadline, or the person refused`),
///    which is one sentence for two facts and is wrong for one of them.
#[test]
fn a_run_still_writing_is_still_a_run_this_daemon_cannot_look_at() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let dir = scratch("ticker");
    let log = dir.join("harnessd.stderr");
    let socket = dir.join("harnessd.sock");
    let store = dir.join("sessions.db");
    let session = "sudo-ask-ticker";
    let mut daemon = start_daemon(&dir, &log, &socket, &store, session);
    let (mut head, rx) = attach(&socket, session, &log, &mut daemon);

    // ---- The run. A writer every 100 ms, and beside it a process that is not this daemon's to
    // look at, blocked on the terminal the daemon holds. `apt`'s shape while it works, with the
    // question one keystroke away — which is exactly the state the operator was in.
    let exe = std::env::current_exe().expect("this test binary's own path");
    let script = dir.join("streaming-run.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\n\
             # letibot test double for `apt install mc` while it streams: a writer that never\n\
             # lets the run be quiet for a beat, over a process of this run that cannot be\n\
             # looked at and is waiting on the daemon's terminal.\n\
             while true; do printf 'tick\\n'; sleep 0.1; done &\n\
             LETIBOT_SUDO_ASK_CHILD=1 {exe} --exact the_process_the_daemon_may_not_read \\\n\
               --nocapture\n",
            exe = exe.display()
        ),
    )
    .expect("write the run");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
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

/// **The deadline ends it, for this shape too.**
///
/// The operator's report is *"harnessd hangs"*, and the todo their own session left behind
/// says why that word is the right one: *"while a run is in flight **nothing else of the user's
/// runs**, and if the deadline cannot fire the daemon is **hung, not busy**."* So the last
/// thing to check is the one thing they could not see from their seat: that the deadline ends a
/// run whose waiting process belongs to **root** — a process this daemon may not signal.
///
/// # Why the door is not the route, which was tried first
///
/// The head-run door looks like the cheap way in: it takes the same path into the same tool
/// (`invoke_operator` → `bash` → `wait_with_progress` → the deadline branch) and a head may
/// name a `timeout_ms`, so three seconds instead of two minutes. It is refused —
/// `HEAD_RUN_TOOLS` does not contain `bash`, deliberately, because *"`bash` and `write` behind
/// a composer's chord would put a shell one keystroke from where the operator is typing"*. So
/// the operator's own `!` line is the only way to run a `bash` call as the operator, and the
/// 120 s default is what it gets: `run_operator_shell` builds the call with the command and
/// nothing else.
///
/// # Why this is opt-in
///
/// Its cost is fixed and it is the largest in the file: two minutes of wall clock for a run
/// that is *supposed* to sit still. `sudo_live.rs` gates its real-`sudo` test for the same
/// reason, and a suite that blocks a child for two minutes on every run is a suite people stop
/// running. `LETIBOT_DEADLINE_LIVE=1` turns it on.
///
/// # What is asserted
///
/// 1. The run ends at all — *"if the deadline cannot fire the daemon is **hung, not busy**"*,
///    in the operator's own session's words.
/// 2. It ends **because of the deadline**: the row it leaves says `was killed after 120s` and
///    `outlived its deadline`. Ending for some other reason would prove nothing about this.
#[test]
fn the_deadline_ends_a_run_this_daemon_may_not_look_at() {
    if std::env::var("LETIBOT_DEADLINE_LIVE").as_deref() != Ok("1") {
        eprintln!("LETIBOT_DEADLINE_LIVE=1 to spend two minutes on the 120 s deadline");
        return;
    }
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    if !Path::new("/usr/bin/su").exists() {
        eprintln!("no /usr/bin/su on this box — nothing to measure");
        return;
    }
    let dir = scratch("deadline");
    let log = dir.join("harnessd.stderr");
    let socket = dir.join("harnessd.sock");
    let store = dir.join("sessions.db");
    let session = "sudo-ask-deadline";
    let mut daemon = start_daemon(&dir, &log, &socket, &store, session);
    let (mut head, rx) = attach(&socket, session, &log, &mut daemon);

    // ---- The run: `su` as root, reading the terminal the daemon holds, so it waits forever
    // and cannot be signalled by this uid. The operator's `!` line, and the 120 s default it
    // gets.
    let started = Instant::now();
    head.operator_shell(0, "! /usr/bin/su -c true")
        .expect("the line is submitted");

    let mut ended = false;
    let mut rows = String::new();
    let deadline = Instant::now() + Duration::from_secs(180);
    // **And it keeps reading after the run has ended**, because the two halves arrive in this
    // order: `run_operator_shell` publishes `operator_shell_ran` first and appends the rows
    // second, so a loop that stopped at the warning would read the note and miss the row the
    // note is about. That is what the first cut of this test did, and it measured a run that
    // had ended by its deadline as having no row at all.
    while Instant::now() < deadline && !(ended && rows.contains("outlived its deadline")) {
        let Ok(inbound) = rx.recv_timeout(Duration::from_millis(250)) else {
            continue;
        };
        if let ServerFrame::Event(env) = inbound.frame() {
            match &env.event {
                SessionEvent::Warning { code, .. } if code == "operator_shell_ran" => ended = true,
                // The row the run produced, which is where the deadline says what it did.
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
    eprintln!("---- the deadline took {took:?}; the run's own row said ----");
    for line in rows.lines().filter(|l| !l.trim().is_empty()).take(12) {
        eprintln!("  {line}");
    }
    assert!(
        ended,
        "a run this daemon may not look at never ended — the deadline did not fire, which is \
         the second, worse defect: the daemon is hung, not busy"
    );
    assert!(
        rows.contains("was killed after 120s") && rows.contains("outlived its deadline"),
        "the run ended, but not by its deadline — and *the deadline cannot fire* is the state \
         the operator's own todo calls a hung daemon: {rows}"
    );
    assert!(
        took < Duration::from_secs(180),
        "the run ended after {took:?}, which is past the budget this test gives the deadline"
    );
}

/// **The `!` line's tree, as the box's own `ps` reports it**, for the evidence.
///
/// Read from `ps` rather than from `/proc` by hand: the shape under test is *whose uid each
/// process is and where each one sits*, and `ps` is the one program that already renders
/// exactly that.
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
