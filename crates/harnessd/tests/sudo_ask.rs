//! The password ask and the operator's own run: who serves whom, and what happens when the
//! daemon cannot see.
//!
//! The operator's report: `! sudo apt install mc` - *"after entering the sudo password, the
//! command appears queued and the daemon hangs."* Reproduced several times, and the live
//! daemon's log carried only its startup disclosures, which is why this file exists.
//!
//! Two mechanisms were proposed, and the rule both are measured against is the one this tree
//! already writes down in `letibot_sessionlog`'s `PROTOCOL_VERSION` 33 section
//!
//! > A run must not block the door that serves it.
//!
//! The daemon served every session from one worker when this was measured
//! (`harnessd/src/daemon.rs`; since R63 each root session has a thread of its own), and the
//! operator's `!` line ran *on* it - `run_operator_shell` calls `invoke_operator`, which waited
//! on the job - so for as long as the command ran, everything queued behind it waited.
//!
//! # Mechanism 1, refuted here
//!
//! *"`sudo`'s `SUDO_ASKPASS` asks the daemon for the password through the same queue, so the
//! ask sits queued behind the run that is waiting for it."* It does not: `Askpass` is answered
//! on the helper's own connection thread (`sessionlog/src/server.rs`), the head's `Secret`
//! is answered on the head's, and neither goes near the command queue. The first test below is
//! the measurement - the run is blocked inside its command on this thread while the password
//! ask is raised and answered over the socket - and it is the half that decides the question.
//!
//! # Mechanism 2, which is the one, in its real shape
//!
//! The run's held input (`agent/bang-prompt`, protocol 33 - a pipe then, and the run's own
//! terminal since `agent/quiet-shell`) made the operator's run wait where `/dev/null` used
//! to give it EOF. The wait is the feature - it is what makes
//! `Continue? [Y/n]` answerable - and the defect is that in the case the feature was built for
//! the daemon cannot see the wait at all
//!
//! ```text
//!   ! sudo apt install mc
//!     bash -ic …        the operator's uid, /proc readable, blocked in wait4
//!       sudo            ROOT — /proc/<pid>/fd/0 is EACCES for the daemon
//!         letibot-askpass   the operator's uid again, blocked on the socket
//!         apt           ROOT — waiting at `Continue? [Y/n]` on the terminal the daemon holds
//! ```
//!
//! `letibot_tools::exec::ask` reads `/proc/<pid>/fd/0` and `/proc/<pid>/wchan`, so the two
//! processes it *can* read (the shell in `wait4`, the helper on a socket) say *not asking*
//! and the one that is asking cannot be opened. MEASURED on this box, 2026-10-06
//! `readlink /proc/1/fd/0` is `PermissionDenied`; `wchan` for such a process reads `0`, the
//! kernel's own *could not look*; and a process of this uid that has called
//! `PR_SET_DUMPABLE 0` - the flag a setuid exec sets - is refused exactly the same way.
//!
//! So no card is raised (correctly: a card is a reading of the process, and this is not one)
//! the run sits on its terminal, the `!` line's echo still says `queued` because no row has landed
//! and the daemon's one worker is inside it. That is the report.
//!
//! # What these tests therefore assert
//!
//! 1. `the_password_ask_is_served_while_the_operators_run_is_blocked` - the ask is answered
//!    while the run waits, and the run's *own* wait raises no card it did not earn. `sudo -A`'s
//!    process shape (`pw=$(helper)`: a pipe on the child's stdout, blocked reading it, fd 0
//!    left alone) must not read as a question. 2. `a_run_this_daemon_may_not_read_is_said_out_loud_and_can_still_be_answered` - the
//!    operator's case, end to end, without a real `sudo`: a run whose process makes itself as
//!    unreadable as a root `apt` is, waiting on the terminal the daemon holds. The daemon says so, names
//!    the way in, and `!send` reaches it - which is the whole of what a person was owed.

// `PR_SET_DUMPABLE` is how these tests make a process this uid may not inspect, and it is
// `<linux/prctl.h>`: there is no macOS spelling of the same shape.
#![cfg(target_os = "linux")]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use letibot_harnessd::config::{Config, Seat};
use letibot_harnessd::{Dialect, Harness, Parts};
use letibot_sessionlog::Registry;
use letibot_sessionlog::client::{HeadClient, pump};
use letibot_sessionlog::event::SessionEvent;
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::protocol::{Caps, ServerFrame};
use letibot_sessionlog::server::serve_registry;
use letibot_transcript::TranscriptItem;

mod support;

/// How long any wait in this file may take before it is a failure rather than a hang.
const PATIENCE: Duration = Duration::from_secs(60);

/// How long the head takes to type the password. Longer than `bash`'s wait loop needs to look
/// at the run once (500 ms a beat, 250 ms of quiet), which is the window the operator's own
/// report lives in - nobody types in under half a second.
const TYPING: Duration = Duration::from_secs(3);

/// `PR_SET_DUMPABLE`, from `<linux/prctl.h>`. Declared here rather than pulled in with a crate
/// this is the one libc call this file makes, and it is the whole of what stands in for `sudo`.
const PR_SET_DUMPABLE: i32 = 4;

unsafe extern "C" {
    /// The one syscall this file needs, declared rather than depended on.
    fn prctl(option: i32, ...) -> i32;
}

fn config(session: &str, socket: &Path) -> Config {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("the workspace root is two levels above this crate")
        .to_path_buf();
    let mut cfg = Config::for_this_box(repo);
    cfg.web_search = None;
    cfg.permission = Vec::new();
    cfg.dialect = Dialect::Qwen;
    cfg.session_id = session.into();
    cfg.socket = socket.to_path_buf();
    // A seat that can run commands: unconfined (`leticode`), `bash` seated, and a mode that
    // needs no oracle - the point here is the exec path, not the mode.
    cfg.seat = Seat::Leticode;
    cfg.allow_bash = true;
    // `Config::for_this_box` carries no vocabulary default, so a harness built here is handed
    // one: the operator's `LETIBOT_VOCAB_GGUF` if set, else the box's own GGUF, the same path
    // this file's `vocab_gguf` gate consults.
    cfg.vocab_gguf = support::vocab::gguf();
    cfg
}

fn parts(cfg: &Config) -> Parts {
    let p = Parts::load(cfg).expect(
        "the vocabulary must load; set LETIBOT_VOCAB_GGUF if this box is not the one this \
         repository is developed on",
    );
    *p.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    p
}

fn socket_path(tag: &str) -> PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    letibot_sessionlog::testing::scratch_root().join(format!(
        "letibot-sudo-ask-{tag}-{}-{n}.sock",
        std::process::id()
    ))
}

/// The shape of `sudo -A`, as a script: the helper's stdout is a pipe this process blocks
/// reading, with fd 0 inherited from the run. `pw=$(helper)` is that shape, and it raised a card
/// for a question nobody asked before `ask`'s descriptor check existed.
fn write_sudo_double(dir: &Path, helper: &Path) -> PathBuf {
    let path = dir.join("sudodouble");
    letibot_tools::testing::write_stub(
        &path,
        &format!(
            "#!/bin/sh\n\
             # The operator's `!` line runs this with NO arguments, so the probe's own run is\n\
             # told apart by `write_stub`'s LETIBOT_STUB_PROBE, not by `$#`.\n\
             [ -n \"$LETIBOT_STUB_PROBE\" ] && exit 0\n\
             # letibot test double for `sudo -A`: the helper is a child whose stdout is a\n\
             # pipe, and this process -- whose own fd 0 is the terminal the daemon holds --\n\
             # blocks reading it. That is the whole of what sudo is here.\n\
             pw=$({helper} '[sudo] password for dead: ')\n\
             printf 'sudodouble: the helper said [%s]\\n' \"$pw\"\n",
            helper = helper.display()
        ),
    );
    path
}

/// The run's own process, when this test binary is re-executed as it: it sets
/// `PR_SET_DUMPABLE 0`, the flag a setuid exec sets, then asks a question and blocks on the
/// terminal, which is `apt` at `Continue? [Y/n]`.
#[test]
fn the_run_this_daemon_may_not_read() {
    if std::env::var("LETIBOT_SUDO_ASK_CHILD").as_deref() != Ok("1") {
        return;
    }
    // SAFETY: `prctl` with `PR_SET_DUMPABLE` and a zero; it cannot fail in a way that matters
    // here, and the code is printed so a box where it did is legible rather than silent.
    let rc = unsafe { prctl(PR_SET_DUMPABLE, 0, 0, 0, 0) };
    println!("invisible-child: PR_SET_DUMPABLE 0 -> {rc}");
    let _ = std::io::stdout().flush();
    // `apt`'s question, and then the wait - on fd 0, which is the terminal the daemon holds.
    println!("Continue? [Y/n] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let n = std::io::stdin().read_line(&mut line).unwrap_or(0);
    println!("invisible-child: read {n} byte(s): {:?}", line.trim_end());
}

/// Every event of the kinds this file is about, as the wire spelled it.
fn said(frames: &[SessionEvent]) -> Vec<String> {
    frames
        .iter()
        .filter_map(|e| match e {
            SessionEvent::SecretRequested {
                prompt, command, ..
            } => Some(format!(
                "SecretRequested prompt={prompt:?} command={command:?}"
            )),
            SessionEvent::SecretSettled { given, by, .. } => {
                Some(format!("SecretSettled given={given} by={by}"))
            }
            SessionEvent::PromptRequested {
                command, question, ..
            } => Some(format!(
                "PromptRequested command={command:?} question={question:?}"
            )),
            SessionEvent::PromptSettled { sent, by, .. } => {
                Some(format!("PromptSettled sent={sent} by={by}"))
            }
            SessionEvent::Warning { code, detail, .. } => Some(format!("Warning {code}: {detail}")),
            _ => None,
        })
        .collect()
}

/// The run's own rows, as a head draws them.
fn payloads(hub: &Hub) -> String {
    hub.snapshot()
        .items
        .iter()
        .filter_map(|i| i.item.as_ref())
        .filter_map(|i| match i {
            TranscriptItem::ToolResult { payload, .. } => Some(payload.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Mechanism 1, refuted: the ask is served while the operator's run is blocked inside the
/// command, and the run's own askpass wait raises no card it did not earn. The run is on this
/// thread's own runner, as the daemon's single worker is, so a shared queue could never deliver.
#[test]
fn the_password_ask_is_served_while_the_operators_run_is_blocked() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let helper = PathBuf::from(env!("CARGO_BIN_EXE_letibot-askpass"));
    assert!(helper.is_file(), "{} is not built", helper.display());

    let socket = socket_path("served");
    let cfg = config("sudo-ask-served", &socket);
    let dir = letibot_sessionlog::testing::scratch_root()
        .join(format!("letibot-sudo-ask-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let double = write_sudo_double(&dir, &helper);

    let p: &'static Parts = Box::leak(Box::new(parts(&cfg)));
    let hub = Hub::new(&cfg.session_id);
    let registry = Registry::of(hub.clone());
    let mut h = match Harness::open_with_registry(
        p,
        cfg.clone(),
        hub.clone(),
        None,
        None,
        registry.clone(),
    ) {
        Ok(h) => h,
        // A refusable skip: this file's whole subject is the exec path, so a box where the harness
        // cannot build one has run nothing, and the green would be a claim it never tested.
        Err(e) => {
            letibot_tokencore::apparatus::absent(&format!(
                "an exec host this daemon can open ({e})"
            ));
            return;
        }
    };
    let server = serve_registry(registry, &socket).expect("bind");

    let (mut head, _hello, reader) =
        HeadClient::attach(&socket, &cfg.session_id, 0, "tui", "dead", Caps::default())
            .expect("head attach");
    let (tx, rx) = std::sync::mpsc::channel();
    let _pump = std::thread::spawn(move || pump(reader, tx));

    let line = format!("! {double}", double = double.display());
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let runner = std::thread::spawn({
        let line = line.clone();
        move || {
            let r = h.run_operator_shell(&line, "dead");
            let _ = done_tx.send(());
            r
        }
    });

    let deadline = Instant::now() + PATIENCE;
    let mut seen: Vec<SessionEvent> = Vec::new();
    let mut answered = false;
    let mut finished = false;
    while Instant::now() < deadline {
        if done_rx.try_recv().is_ok() {
            finished = true;
            break;
        }
        let Ok(inbound) = rx.recv_timeout(Duration::from_millis(200)) else {
            continue;
        };
        let ServerFrame::Event(env) = inbound.frame() else {
            continue;
        };
        if let SessionEvent::SecretRequested { req_id, .. } = &env.event
            && !answered
        {
            answered = true;
            // A person takes seconds, and that is the window the daemon's own reading of the
            // run is taken in - see `TYPING`.
            std::thread::sleep(TYPING);
            head.secret(req_id, Some("hunter2".into()))
                .expect("the password is answered");
        }
        seen.push(env.event.clone());
    }

    let first_told = said(&seen);
    assert!(
        finished,
        "the run never ended; the head was told: {first_told:#?}"
    );
    runner
        .join()
        .expect("the run thread")
        .expect("the run records");
    // The run ending and its last frames are two buffers: a settlement is on the wire before the
    // process can exit, but this loop breaks on the completion, so the queue is read to the end
    // here. A verdict built on whichever frames the loop happened to reach first is a verdict on
    // the box's scheduling, not on what the head was told.
    while let Ok(inbound) = rx.try_recv() {
        if let ServerFrame::Event(env) = inbound.frame() {
            seen.push(env.event.clone());
        }
    }
    let told = said(&seen);
    assert!(
        answered,
        "the password ask never reached the head while the run was blocked — which is what \
         mechanism 1 predicted, and it is false: {told:#?}"
    );
    // The password reached the helper, so the ask travelled the whole path while the
    // worker was inside the command.
    let rows = payloads(&hub);
    assert!(
        rows.contains("sudodouble: the helper said [hunter2]"),
        "the answer must reach the helper's stdout and come back out of the run: {rows}"
    );
    // And the run's askpass wait is not a question. No card may be raised for it: `sudo`'s
    // pipe read is on a pipe it made, not on the device this daemon holds.
    assert!(
        !told.iter().any(|l| l.starts_with("PromptRequested")),
        "a process blocked on a pipe of its own raised a card claiming the run was waiting for \
         a line: {told:#?}"
    );
    // The card the person answers is the password's, and it is settled by them.
    assert!(
        told.iter()
            .any(|l| l.starts_with("SecretSettled given=true by=dead")),
        "the password's own settlement is the record: {told:#?}"
    );
    server.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The operator's own case, end to end, without a real `sudo`: a run the daemon may not look at
/// waiting on the terminal it holds, said out loud and answerable. The run is this binary
/// re-executed, unreadable as a root `apt`; no card is raised, because a card is a reading.
#[test]
fn a_run_this_daemon_may_not_read_is_said_out_loud_and_can_still_be_answered() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let socket = socket_path("unreadable");
    let cfg = config("sudo-ask-unreadable", &socket);
    let p: &'static Parts = Box::leak(Box::new(parts(&cfg)));
    let hub = Hub::new(&cfg.session_id);
    let registry = Registry::of(hub.clone());
    let mut h = match Harness::open_with_registry(
        p,
        cfg.clone(),
        hub.clone(),
        None,
        None,
        registry.clone(),
    ) {
        Ok(h) => h,
        // A refusable skip, for the reason the first test's arm carries.
        Err(e) => {
            letibot_tokencore::apparatus::absent(&format!(
                "an exec host this daemon can open ({e})"
            ));
            return;
        }
    };
    let server = serve_registry(registry, &socket).expect("bind");

    let (mut head, _hello, reader) =
        HeadClient::attach(&socket, &cfg.session_id, 0, "tui", "dead", Caps::default())
            .expect("head attach");
    let (tx, rx) = std::sync::mpsc::channel();
    let _pump = std::thread::spawn(move || pump(reader, tx));

    // The `!` line the operator would type, with the process it runs standing in for
    // `apt`: `LETIBOT_SUDO_ASK_CHILD=1` makes the re-executed test binary take the child path.
    let exe = std::env::current_exe().expect("this test binary's own path");
    let line = format!(
        "! LETIBOT_SUDO_ASK_CHILD=1 {exe} --exact the_run_this_daemon_may_not_read --nocapture",
        exe = exe.display()
    );
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let runner = std::thread::spawn({
        let line = line.clone();
        move || {
            let r = h.run_operator_shell(&line, "dead");
            let _ = done_tx.send(());
            r
        }
    });

    // The run is deliberately NOT waited for: it cannot end until somebody answers it, so this
    // loop ends at the sentence.
    let deadline = Instant::now() + PATIENCE;
    let mut seen: Vec<SessionEvent> = Vec::new();
    let mut the_sentence: Option<String> = None;
    while Instant::now() < deadline {
        if done_rx.try_recv().is_ok() {
            break;
        }
        let Ok(inbound) = rx.recv_timeout(Duration::from_millis(200)) else {
            continue;
        };
        let ServerFrame::Event(env) = inbound.frame() else {
            continue;
        };
        if let SessionEvent::Warning { code, detail, .. } = &env.event
            && code == "operator_run_unreadable"
        {
            the_sentence = Some(detail.clone());
            seen.push(env.event.clone());
            break;
        }
        seen.push(env.event.clone());
    }
    let told = said(&seen);
    let sentence = the_sentence.unwrap_or_else(|| {
        panic!(
            "the daemon said nothing about a run it cannot look at — which is the operator's \
             report exactly, `queued` and a hang: {told:#?}"
        )
    });
    assert!(
        sentence.contains("!send"),
        "the sentence must name the way in, which needs no card: {sentence}"
    );
    assert!(
        sentence.contains("the_run_this_daemon_may_not_read"),
        "it names the command the person typed: {sentence}"
    );
    assert!(
        !told.iter().any(|l| l.starts_with("PromptRequested")),
        "an unreadable run must not raise a card — a card is a reading of the process and this \
         is a failure to read one: {told:#?}"
    );

    // ---- 3. The way in works: `!send` reaches the run's terminal, and the run finishes.
    head.send_line("Y")
        .expect("the verb's line is written into the run's terminal");
    let finished = {
        let deadline = Instant::now() + PATIENCE;
        let mut done = false;
        while Instant::now() < deadline {
            if done_rx.try_recv().is_ok() {
                done = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        done
    };
    assert!(
        finished,
        "`!send` did not end the run — the way in named by the sentence does not work: {told:#?}"
    );
    runner
        .join()
        .expect("the run thread")
        .expect("the run records");
    let rows = payloads(&hub);
    assert!(
        rows.contains("invisible-child: read 2 byte(s): \"Y\""),
        "the answer must reach the run's stdin and come back out of it: {rows}"
    );
    // And the settlement the card would have made is not here, because there was no card: what
    // the person did is on the log as the run's own rows.
    assert!(
        !told.iter().any(|l| l.starts_with("PromptSettled")),
        "`!send` with no card settles nothing: {told:#?}"
    );
    server.shutdown();
}
