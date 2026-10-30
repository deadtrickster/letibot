//! **A daemon restart brings back what a session was running — or, past the threshold, reminds
//! the model of it.**
//!
//! **And a restart that had nothing to restore is said anyway**, because the one fact only the
//! daemon has is the time: the operator, 2026-10-30, after one restart had told the model
//! something and the next had said nothing — *"mb always tell agent about restart? with the
//! current time lol"*. That sentence is a NOTICE and not a turn (`restart::nothing_running`),
//! while a restore keeps the turn it has always had: `an_empty_restart_is_said_once_and_starts_no_turn`
//! pins the first, and every other test here pins the second.
//!
//! The operator, 2026-10-12, verbatim: *"and here is one more thing about restart - we should
//! bring subagents back - i cant afford to wait till they finish. we should record them (session
//! persist anyway) and restart automatically"*; then *"on restoring agents - in fact to make
//! restarts as much painless as possible - by default we restore agents, monitors, and remind the
//! model about jobs. Now, if some threshold passed - say more than 1 hours after the stop, we start
//! to simply remind the model stating that time passed and this is what you wre running and how
//! to restart that."*; and *"this is the case for when harnessd stopped of course"*.
//!
//! # How a restart is staged here
//!
//! A daemon is a [`Registry`] (its instance id is what every record is stamped with) and a store
//! file. "Stopping the daemon" is what `cli.rs` does on the orderly path: `Registry::close` (which
//! interrupts every turn in flight), the subagent threads waited out, `Lifeline::stopped`.
//! "Starting it again" is a NEW registry on the SAME store, and the parent session opened in it —
//! `restore_after_restart` is what `Sessions` calls at that open, called here directly where the
//! test drives a bare [`Harness`], and through `Sessions::open_first` where the ring that starts
//! the model's turn is the thing asserted. What a crash would leave (a `running` row from a daemon
//! with no stop recorded) is staged by writing the rows a crashed daemon leaves, which is the
//! only way to have a daemon die in a test process without taking the test with it.
//!
//! The model is a stub that answers every turn with a plain answer — except the first child turn
//! of the main test, which it HOLDS so the daemon can be stopped while the child is mid-turn.
//! Needs the vocabulary GGUF (a harness renders its stable prefix), no model.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use letibot_harnessd::config::{Config, Seat};
use letibot_harnessd::restart::Lifeline;
use letibot_harnessd::sessions::{Outcome, Sessions};
use letibot_harnessd::subagent_record::RecordedSpec;
use letibot_harnessd::{Dialect, Harness, Parts};
use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::registry::{Registry, SessionWiring, Work, WorkOrIdle};
use letibot_tokencore::Vocab;
use letibot_tokencore::store::{JobContext, JobRecord, Store, SubagentRun, TodoItem};
use letibot_tools::NullToolSink;
use letibot_tools::builtins::task::{TaskSpec, WorktreePlacement};
use letibot_transcript::{ToolCall, TranscriptItem, UserPart};
use letibot_turn::Endpoint;

#[path = "../../turn/tests/support/canned.rs"]
mod canned;
// **The descriptors a binary's parallel tests need** — see [`room`]'s own note: on this box the
// soft open-file limit is 256 and eighteen daemons and their children at once want more.
#[path = "support/room.rs"]
mod room;

use canned::Frame;

const IM_END: u32 = 248046;
const THINK_CLOSE: u32 = 248069;
const PATIENCE: Duration = Duration::from_secs(30);
const PARENT: &str = "s-restart-parent";
const OLD: &str = "d-old-daemon";
const ANSWER: &str = "finished the restored work";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn config(repo: &Path, store: &Path, endpoint: Endpoint) -> Config {
    let mut cfg = Config::for_this_box(repo);
    cfg.dialect = Dialect::Qwen;
    // `leticode` seats the delegation verbs (with `task_resume`), `monitor` and `bash`.
    cfg.seat = Seat::Leticode;
    cfg.allow_bash = true;
    cfg.endpoint = endpoint;
    cfg.http_retries = 0;
    cfg.context_window = Some(32_768);
    cfg.store = Some(store.to_path_buf());
    cfg.session_id = PARENT.to_string();
    cfg.web_search = None;
    cfg.permission = Vec::new();
    cfg.vocab_gguf = std::env::var("LETIBOT_VOCAB_GGUF")
        .ok()
        .map(PathBuf::from)
        .or_else(letibot_tokencore::apparatus::present_gguf);
    cfg
}

fn parts_for(cfg: &Config) -> Parts {
    let p = Parts::load(cfg).expect("the vocabulary must load");
    *p.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    p
}

fn wiring(cfg: &Config) -> SessionWiring {
    SessionWiring {
        model: cfg.model.clone(),
        dialect: "qwen".into(),
        endpoint: "http://127.0.0.1:1".into(),
        workspace: cfg.workspace.display().to_string(),
    }
}

/// A scratch directory holding a git repository (`repo/`, one commit) and the store.
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let root = letibot_sessionlog::testing::scratch_root().join(format!(
            "{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec![
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "base",
            ],
        ] {
            let ok = std::process::Command::new("git")
                .args(&args)
                .current_dir(&repo)
                .status()
                .expect("git runs")
                .success();
            assert!(ok, "git {args:?}");
        }
        Scratch { root }
    }
    fn repo(&self) -> PathBuf {
        self.root.join("repo")
    }
    fn store(&self) -> PathBuf {
        self.root.join("sessions.db")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::process::Command::new("git")
            .args(["worktree", "prune"])
            .current_dir(self.repo())
            .status();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// **One daemon**: a registry with the parent session adopted, and the parent's harness.
struct Daemon {
    registry: Arc<Registry>,
    parent: Harness,
}

fn a_daemon(parts: &Parts, cfg: &Config) -> Daemon {
    let registry = Registry::new();
    let hub = registry.new_hub(PARENT.to_string());
    registry
        .adopt(hub.clone(), PARENT.to_string(), wiring(cfg), None)
        .expect("the parent is in the registry");
    let parent = Harness::open_with_registry(parts, cfg.clone(), hub, None, None, registry.clone())
        .expect("the parent opens");
    Daemon { registry, parent }
}

impl Daemon {
    fn call(&self, name: &str, args: serde_json::Value) -> letibot_tools::ToolResult {
        let rt = self.parent.runtime_handle();
        let call = ToolCall {
            id: format!("call_{name}"),
            name: name.to_string(),
            arguments: args.to_string(),
        };
        rt.invoke("t1", &call, &mut NullToolSink)
    }

    fn hub(&self) -> Arc<Hub> {
        self.registry.get(PARENT).expect("the parent's hub")
    }

    /// The daemon's orderly stop, in `cli.rs`'s order.
    fn stop(self, lifeline: Option<&Lifeline>) {
        self.registry.close();
        let _ = self.parent.stop_children();
        assert!(
            self.parent.wait_for_children(PATIENCE),
            "a child thread outlived its daemon's stop"
        );
        if let Some(l) = lifeline {
            l.stopped();
        }
    }
}

fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
    let began = Instant::now();
    while !f() {
        assert!(began.elapsed() < PATIENCE, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn user_rows(hub: &Hub) -> Vec<String> {
    hub.retained()
        .iter()
        .filter_map(|e| match &e.event {
            SessionEvent::TranscriptContent { item, .. } => match &**item {
                TranscriptItem::User { parts, .. } => Some(
                    parts
                        .iter()
                        .filter_map(|p| match p {
                            UserPart::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn warnings(hub: &Hub, code: &str) -> Vec<String> {
    hub.retained()
        .iter()
        .filter_map(|e| match &e.event {
            SessionEvent::Warning {
                code: c, detail, ..
            } if c == code => Some(detail.clone()),
            _ => None,
        })
        .collect()
}

fn run(child: &str, parent: &str, state: &str, spec: &TaskSpec) -> SubagentRun {
    SubagentRun {
        child_id: child.into(),
        parent_id: parent.into(),
        state: state.into(),
        prompt: format!("the subtask of {child}"),
        spec_json: RecordedSpec::of(spec).to_json(),
        instance: OLD.into(),
        resumes: 0,
        outcome: None,
        delivered: false,
        started_ms: 1,
        updated_ms: 1,
    }
}

/// The previous daemon's life: started a minute before it stopped, stopped `ago` before now.
fn old_daemon_stopped(store: &Store, ago: Duration) {
    let now = letibot_sessionlog::registry::now_ms();
    let at = now.saturating_sub(ago.as_millis() as u64);
    store.life_started(OLD, at.saturating_sub(60_000)).unwrap();
    store.life_stopped(OLD, at).unwrap();
}

// --- the stub model -------------------------------------------------------------------------

struct Stub {
    endpoint: Endpoint,
    gate: Arc<(Mutex<(usize, usize)>, Condvar)>,
}

impl Stub {
    /// Every turn answered with `frames`; the first `hold` turns wait for [`Stub::release`].
    fn new(frames: Vec<Frame>, hold: usize) -> Stub {
        Stub::holding(frames, 0..hold)
    }

    /// Every turn answered with `frames`; the turns whose arrival index is in `held` wait for
    /// [`Stub::release`].
    fn holding(frames: Vec<Frame>, held: std::ops::Range<usize>) -> Stub {
        let body = Arc::new(canned::sse_body(&frames));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        // (arrived, released)
        let gate = Arc::new((Mutex::new((0usize, 0usize)), Condvar::new()));
        let g = gate.clone();
        std::thread::spawn(move || {
            while let Ok((stream, _)) = listener.accept() {
                let (line, _) = read_request(stream.try_clone().unwrap());
                if line.starts_with("GET ") {
                    let _ = write_props(stream);
                    continue;
                }
                let body = body.clone();
                let g = g.clone();
                let held = held.clone();
                std::thread::spawn(move || {
                    let (lock, cv) = &*g;
                    let mut st = lock.lock().unwrap();
                    let n = st.0;
                    st.0 += 1;
                    cv.notify_all();
                    while held.contains(&n) && st.1 <= n {
                        st = cv.wait(st).unwrap();
                    }
                    drop(st);
                    let _ = write_body(stream, &body);
                });
            }
        });
        Stub {
            endpoint: Endpoint::new(addr.ip().to_string(), addr.port()),
            gate,
        }
    }

    fn arrived(&self) -> usize {
        self.gate.0.lock().unwrap().0
    }

    fn release(&self) {
        let (lock, cv) = &*self.gate;
        lock.lock().unwrap().1 = usize::MAX;
        cv.notify_all();
    }
}

impl Drop for Stub {
    fn drop(&mut self) {
        self.release();
    }
}

fn read_request(mut s: TcpStream) -> (String, String) {
    let mut acc: Vec<u8> = Vec::new();
    let mut buf = [0u8; 4096];
    let mut head_end = None;
    let mut want = 0usize;
    loop {
        match s.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => acc.extend_from_slice(&buf[..n]),
        }
        if head_end.is_none()
            && let Some(at) = acc.windows(4).position(|w| w == b"\r\n\r\n")
        {
            head_end = Some(at);
            want = acc[..at]
                .split(|b| *b == b'\n')
                .find_map(|line| {
                    let line = String::from_utf8_lossy(line);
                    let (name, value) = line.split_once(':')?;
                    name.trim()
                        .eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())?
                })
                .unwrap_or(0);
        }
        if let Some(at) = head_end
            && acc.len() - (at + 4) >= want
        {
            break;
        }
    }
    let text = String::from_utf8_lossy(&acc).into_owned();
    match head_end {
        Some(at) => (
            text[..at].lines().next().unwrap_or("").trim().to_string(),
            text[at + 4..].to_string(),
        ),
        None => (String::new(), text),
    }
}

fn write_props(mut stream: TcpStream) -> std::io::Result<()> {
    const BODY: &str = "{\"default_generation_settings\":{\"n_ctx\":32768}}";
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{BODY}",
        BODY.len()
    )?;
    stream.flush()
}

fn write_body(mut stream: TcpStream, body: &[u8]) -> std::io::Result<()> {
    stream.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
          Transfer-Encoding: chunked\r\n\r\n",
    )?;
    stream.write_all(body)?;
    stream.flush()
}

fn plain(vocab: &Vocab, answer: &str) -> Vec<Frame> {
    let mut ids = vocab.tokenize_text("thinking").unwrap();
    ids.push(THINK_CLOSE);
    ids.extend(vocab.tokenize_text(answer).unwrap());
    ids.push(IM_END);
    let mut frames = vec![Frame::Progress {
        total: 30,
        processed: 30,
    }];
    frames.extend(ids.iter().map(|id| Frame::Token {
        id: *id,
        text: vocab.detokenize(&[*id], true).unwrap().leak(),
    }));
    frames.push(Frame::Final {
        stop_type: "eos",
        n_decoded: ids.len() as u64,
        n_prompt: 30,
        cache_n: 0,
    });
    frames
}

fn task_result(d: &Daemon, handle: &str) -> letibot_tools::ToolResult {
    d.call("task_result", serde_json::json!({"task": handle}))
}

fn answered(d: &Daemon, handle: &str) -> bool {
    let r = task_result(d, handle);
    r.outcome == letibot_transcript::ToolOutcome::Ok && r.payload.contains(ANSWER)
}

// ---------------------------------------------------------------------------
// The one this change exists for
// ---------------------------------------------------------------------------

/// **A child mid-turn when the daemon stops is resumed when it starts again** — same session,
/// same worktree, the restart notice in its own transcript — and its answer reaches the parent
/// through the parent's watcher, exactly as a fresh child's does.
#[test]
fn a_child_running_when_the_daemon_stopped_resumes_and_its_answer_reaches_the_parent() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    // Eighteen daemons and their children in parallel need more open files than a shell's 256 —
    // `room`'s note has the measurement.
    let Some(_) = room::room_for("eighteen daemons and their children in parallel", 1024) else {
        return;
    };
    let dir = Scratch::new("restart-resume");
    let parts = parts_for(&config(
        &dir.repo(),
        &dir.store(),
        Endpoint::new("127.0.0.1", 1),
    ));
    // --- daemon one: start a child in a worktree, stop the daemon while its turn is held ---
    let stub = Stub::new(plain(&parts.vocab, ANSWER), 1);
    let cfg = config(&dir.repo(), &dir.store(), stub.endpoint.clone());
    let one = a_daemon(&parts, &cfg);
    let life = Lifeline::start(cfg.store.clone(), one.registry.clone());
    let started = one.call(
        "task_start",
        serde_json::json!({"prompt": "fix the flaky test", "slug": "fix-flaky"}),
    );
    let handle = match &started.outcome {
        letibot_transcript::ToolOutcome::Backgrounded { handle, .. } => handle.clone(),
        other => panic!(
            "task_start did not start a child: {other:?} {}",
            started.payload
        ),
    };
    wait_until("the child's first turn to reach the model", || {
        stub.arrived() >= 1
    });
    let store = Store::open(&dir.store()).unwrap();
    let recorded = store
        .subagent_run(&handle)
        .unwrap()
        .expect("the child is recorded");
    assert_eq!(recorded.state, "running");
    assert_eq!(recorded.parent_id, PARENT);
    assert_eq!(recorded.prompt, "fix the flaky test");
    // **The child is named after its worktree, and the name is recorded and listed** — the slug
    // its branch `agent/fix-flaky` was cut from.
    assert_eq!(
        RecordedSpec::from_json(&recorded.spec_json)
            .unwrap()
            .slug
            .as_deref(),
        Some("fix-flaky")
    );
    let brief = |d: &Daemon| {
        d.registry
            .list()
            .into_iter()
            .find(|b| b.session_id == handle)
            .map(|b| b.slug)
    };
    assert_eq!(brief(&one).as_deref(), Some("fix-flaky"));
    let worktree = RecordedSpec::from_json(&recorded.spec_json)
        .unwrap()
        .worktree
        .expect("the worktree is recorded")
        .path;
    assert!(Path::new(&worktree).is_dir(), "{worktree}");

    // The orderly stop interrupts the child's turn: its daemon's ending, not its own.
    let old_instance = one.registry.instance().to_string();
    std::thread::spawn({
        let stub_release = stub.gate.clone();
        move || {
            std::thread::sleep(Duration::from_millis(200));
            let (lock, cv) = &*stub_release;
            lock.lock().unwrap().1 = usize::MAX;
            cv.notify_all();
        }
    });
    one.stop(Some(&life));
    let after_stop = store.subagent_run(&handle).unwrap().unwrap();
    assert_eq!(
        after_stop.state, "running",
        "an orderly stop must leave the child to be resumed: {after_stop:?}"
    );
    assert_eq!(after_stop.resumes, 0);
    assert!(
        store
            .life(&old_instance)
            .unwrap()
            .unwrap()
            .stopped_ms
            .is_some()
    );

    // Work the child left uncommitted when its daemon stopped: the resumed child is a worktree
    // child like any other, and is held to its branch at the end of its turn (`set_deliverable`).
    std::fs::write(Path::new(&worktree).join("half-done.txt"), "uncommitted\n").unwrap();

    // --- daemon two: the same store, a new registry ---
    let stub2 = Stub::new(plain(&parts.vocab, ANSWER), 0);
    let cfg2 = config(&dir.repo(), &dir.store(), stub2.endpoint.clone());
    let mut two = a_daemon(&parts, &cfg2);
    let notice = two
        .parent
        .restore_after_restart()
        .expect("the parent had a child to bring back");
    eprintln!("--- what the parent's model is told ---\n{notice}");
    assert!(
        notice.contains("under the 1h00m restore threshold"),
        "{notice}"
    );
    assert!(
        notice.contains(&format!(
            "[restart] subagent `fix-flaky · {handle}` (fix the flaky test) was running when \
             the daemon restarted and has been resumed."
        )),
        "{notice}"
    );
    // **And the time, with the list** — the return time and the stop are in every restart notice,
    // and a restore is not the case that is allowed to drop them.
    assert!(notice.contains("; back at "), "{notice}");
    assert!(notice.contains("stopped at "), "{notice}");
    let child = {
        let mut hub = None;
        wait_until("the resumed child to be in the new registry", || {
            hub = two.registry.get(&handle);
            hub.is_some()
        });
        hub.unwrap()
    };
    // Same session, same worktree, and the restart notice in its own transcript.
    assert_eq!(
        store
            .session(&handle)
            .unwrap()
            .expect("the child's session row")
            .workspace_root,
        worktree,
        "the resumed child works in the worktree it was started in"
    );
    wait_until("the restart notice in the child's transcript", || {
        user_rows(&child)
            .iter()
            .any(|r| r.starts_with("[restart] the daemon restarted while you were working"))
    });
    assert!(
        user_rows(&child)
            .iter()
            .any(|r| r.contains("fix the flaky test")),
        "the resumed child still holds its original task: {:?}",
        user_rows(&child)
    );
    wait_until("the resumed child to answer", || answered(&two, &handle));
    // **The resumed child is checked as a fresh one is**: its turn was about to end with the
    // tree dirty, so it was told once, and the parent's answer carries the salvage line.
    assert!(
        user_rows(&child)
            .iter()
            .any(|r| r.starts_with("[the deliverable]") && r.contains("half-done.txt")),
        "the resumed child was not held to its branch: {:?}",
        user_rows(&child)
    );
    // **And after the restart the child is still known by its name**: the new daemon's list
    // carries it, and `task_result` takes it in place of the handle.
    assert_eq!(brief(&two).as_deref(), Some("fix-flaky"));
    let by_name = task_result(&two, "fix-flaky");
    assert!(by_name.payload.contains(ANSWER), "{}", by_name.payload);
    let collected = task_result(&two, &handle).payload;
    assert!(
        collected.contains("has uncommitted changes") && collected.contains("half-done.txt"),
        "{collected}"
    );
    let row = store.subagent_run(&handle).unwrap().unwrap();
    assert_eq!(row.state, "done");
    assert_eq!(row.resumes, 1);
    assert_eq!(row.instance, two.registry.instance());

    // The operator sees it; the parent's model is told, and the completion arrives through the
    // parent's own watcher.
    assert!(
        warnings(&two.hub(), "restart_restored")
            .iter()
            .any(|w| w.contains(&handle) && w.contains("resumed")),
        "{:?}",
        warnings(&two.hub(), "restart_restored")
    );
    let mut said = Vec::new();
    wait_until("the parent to be told the child finished", || {
        let _ = two.parent.wake();
        said = user_rows(&two.hub());
        said.iter()
            .any(|r| r.contains("a subagent you started has finished") && r.contains(&handle))
    });
    // The completion names the child as `fix-flaky · <handle>`.
    assert!(
        said.iter()
            .any(|r| r.contains(&format!("`fix-flaky · {handle}` done"))),
        "{said:?}"
    );
    assert!(
        said.iter().any(|r| r.contains("has been resumed")),
        "the parent's model got the restart line: {said:?}"
    );
    wait_until("the delivery to be recorded", || {
        store.subagent_run(&handle).unwrap().unwrap().delivered
    });
    two.stop(None);
}

/// **A child that had answered and was mid-way through a LATER turn when the daemon stopped is
/// resumed too** — pg-noop, 2026-10-14: `slt-current-number` answered at 01:57, its parent's
/// `task_message` woke it at 02:01:41 to collect its sweep, and the restart at 02:06:05 found its
/// row reading `done` from the first answer. The next daemon left it, `task_result` said it was
/// never started, `task_resume` said it had finished, and the parent read the worktree to find
/// out. A later turn moves the row back to `running` for its length, so the stop leaves a child
/// to resume — and a later turn that ends on its own puts it back.
#[test]
fn a_child_stopped_in_a_later_turn_is_resumed_not_left_as_finished() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    // Eighteen daemons and their children in parallel need more open files than a shell's 256 —
    // `room`'s note has the measurement.
    let Some(_) = room::room_for("eighteen daemons and their children in parallel", 1024) else {
        return;
    };
    let dir = Scratch::new("restart-later-turn");
    let parts = parts_for(&config(
        &dir.repo(),
        &dir.store(),
        Endpoint::new("127.0.0.1", 1),
    ));
    // The first answer goes through; every request after it waits.
    let stub = Stub::holding(plain(&parts.vocab, ANSWER), 1..usize::MAX);
    let cfg = config(&dir.repo(), &dir.store(), stub.endpoint.clone());
    let one = a_daemon(&parts, &cfg);
    let life = Lifeline::start(cfg.store.clone(), one.registry.clone());
    let started = one.call(
        "task_start",
        serde_json::json!({"prompt": "take the sweep's number", "slug": "slt-sweep"}),
    );
    let handle = match &started.outcome {
        letibot_transcript::ToolOutcome::Backgrounded { handle, .. } => handle.clone(),
        other => panic!("task_start did not start: {other:?} {}", started.payload),
    };
    wait_until("the child's first answer", || answered(&one, &handle));
    let store = Store::open(&dir.store()).unwrap();
    wait_until("the first answer to be recorded", || {
        store.subagent_run(&handle).unwrap().unwrap().state == "done"
    });

    // The parent wakes it between turns: a later turn starts, and the row says it runs.
    let told = one.call(
        "task_message",
        serde_json::json!({"task": handle, "text": "collect the sweep and finish"}),
    );
    assert_eq!(
        told.outcome,
        letibot_transcript::ToolOutcome::Ok,
        "{}",
        told.payload
    );
    let arrived = stub.arrived();
    wait_until("the later turn to reach the model", || {
        stub.arrived() > arrived
    });
    assert_eq!(
        store.subagent_run(&handle).unwrap().unwrap().state,
        "running",
        "a child mid-way through a later turn is running, whatever its first turn said"
    );

    // The orderly stop interrupts that turn: the daemon's ending, not the child's.
    std::thread::spawn({
        let gate = stub.gate.clone();
        move || {
            std::thread::sleep(Duration::from_millis(200));
            let (lock, cv) = &*gate;
            lock.lock().unwrap().1 = usize::MAX;
            cv.notify_all();
        }
    });
    one.stop(Some(&life));
    let after = store.subagent_run(&handle).unwrap().unwrap();
    assert_eq!(after.state, "running", "{after:?}");

    // The next daemon resumes it, and every door reads it the same way.
    let stub2 = Stub::new(plain(&parts.vocab, ANSWER), 0);
    let cfg2 = config(&dir.repo(), &dir.store(), stub2.endpoint.clone());
    let mut two = a_daemon(&parts, &cfg2);
    let notice = two
        .parent
        .restore_after_restart()
        .expect("the child mid-turn is brought back");
    eprintln!("--- what the parent's model is told ---\n{notice}");
    assert!(
        notice.contains(&format!("`slt-sweep · {handle}`")) && notice.contains("has been resumed"),
        "{notice}"
    );
    wait_until("the resumed child to answer", || answered(&two, &handle));
    let listed = two.call("task_result", serde_json::json!({})).payload;
    assert!(
        listed.contains(&format!("slt-sweep · {handle} — ")) && !listed.contains("unknown"),
        "{listed}"
    );
    two.stop(None);
}

/// **A later turn that ends on its own puts the row back** — the child is between turns again,
/// its first answer delivered, and a restart now has nothing to resume or re-deliver.
#[test]
fn a_later_turn_that_ends_on_its_own_leaves_the_row_settled() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    // Eighteen daemons and their children in parallel need more open files than a shell's 256 —
    // `room`'s note has the measurement.
    let Some(_) = room::room_for("eighteen daemons and their children in parallel", 1024) else {
        return;
    };
    let dir = Scratch::new("restart-later-turn-ends");
    let parts = parts_for(&config(
        &dir.repo(),
        &dir.store(),
        Endpoint::new("127.0.0.1", 1),
    ));
    let stub = Stub::new(plain(&parts.vocab, ANSWER), 0);
    let cfg = config(&dir.repo(), &dir.store(), stub.endpoint.clone());
    let one = a_daemon(&parts, &cfg);
    let started = one.call(
        "task_start",
        serde_json::json!({"prompt": "answer twice", "slug": "twice"}),
    );
    let handle = match &started.outcome {
        letibot_transcript::ToolOutcome::Backgrounded { handle, .. } => handle.clone(),
        other => panic!("task_start did not start: {other:?} {}", started.payload),
    };
    wait_until("the child's first answer", || answered(&one, &handle));
    let store = Store::open(&dir.store()).unwrap();
    wait_until("the first answer to be recorded", || {
        store.subagent_run(&handle).unwrap().unwrap().state == "done"
    });
    let arrived = stub.arrived();
    let told = one.call(
        "task_message",
        serde_json::json!({"task": handle, "text": "and once more"}),
    );
    assert_eq!(
        told.outcome,
        letibot_transcript::ToolOutcome::Ok,
        "{}",
        told.payload
    );
    wait_until("the later turn to reach the model", || {
        stub.arrived() > arrived
    });
    wait_until("the later turn to end and the row to rest", || {
        store.subagent_run(&handle).unwrap().unwrap().state == "done"
    });
    let row = store.subagent_run(&handle).unwrap().unwrap();
    assert_eq!(
        row.outcome.as_deref().map(|o| o.contains(ANSWER)),
        Some(true)
    );
    one.stop(None);
}

/// **A tool's helpers are never the model's news, and a recorded child reads one way through
/// every door** — pg-noop, 2026-10-14, staged as its store held it: twelve `digest` part readers
/// from the day before, settled and never marked delivered (their tool read them), all deriving
/// the one name `you-are-reading-part`; and a finished, delivered child left without a slot.
/// The restart announced the twelve as *"12 subagents you started have finished"*, listed every
/// other child as `unknown`, and `task_result` refused the finished one as never started while
/// `task_resume` called it finished.
#[test]
fn helpers_are_not_restored_and_a_recorded_child_reads_one_way() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    // Eighteen daemons and their children in parallel need more open files than a shell's 256 —
    // `room`'s note has the measurement.
    let Some(_) = room::room_for("eighteen daemons and their children in parallel", 1024) else {
        return;
    };
    let dir = Scratch::new("restart-helpers");
    let parts = parts_for(&config(
        &dir.repo(),
        &dir.store(),
        Endpoint::new("127.0.0.1", 1),
    ));
    let stub = Stub::new(plain(&parts.vocab, "noted"), 0);
    let cfg = config(&dir.repo(), &dir.store(), stub.endpoint.clone());
    let store = Store::open(&dir.store()).unwrap();
    old_daemon_stopped(&store, Duration::from_secs(2));
    // The readers, as a build before helpers were marked wrote them.
    let reader_spec = r#"{"role":"researcher","downgrade":"read-only","placement":"host"}"#;
    for n in 1..=3 {
        let mut r = run(
            &format!("s-restart-parent-sub-read{n}"),
            PARENT,
            "done",
            &TaskSpec::default(),
        );
        r.prompt = format!(
            "You are reading part {n} of 3 of a longer body of text on somebody else's behalf. \
             They asked:\n\n  what did it measure"
        );
        r.spec_json = reader_spec.into();
        r.outcome = Some(format!("Part {n} says little"));
        store.put_subagent_run(&r).unwrap();
    }
    // One caught mid-read by the stop, marked as a helper by this build.
    let mut reading = run(
        "s-restart-parent-sub-read4",
        PARENT,
        "running",
        &TaskSpec {
            role: "researcher".into(),
            helper: true,
            slug: Some("read-part-1-of-1".into()),
            ..TaskSpec::default()
        },
    );
    reading.prompt = "You are reading part 1 of 1 of a longer body of text on somebody else's \
                      behalf."
        .into();
    store.put_subagent_run(&reading).unwrap();
    // The model's own child: finished, its answer already told, no slot after the restart.
    let mut done = run(
        "s-restart-parent-sub-slt",
        PARENT,
        "done",
        &TaskSpec {
            slug: Some("slt-current-number".into()),
            ..TaskSpec::default()
        },
    );
    done.outcome = Some("Committed as `dce7cca`; the sweep is still in flight.".into());
    done.delivered = true;
    store.put_subagent_run(&done).unwrap();

    let mut d = a_daemon(&parts, &cfg);
    assert_eq!(
        d.parent.restore_after_restart(),
        None,
        "nothing here is the model's news"
    );
    assert!(warnings(&d.hub(), "restart_restored").is_empty());
    assert!(warnings(&d.hub(), "restart_reminded").is_empty());
    assert_eq!(
        store
            .subagent_run("s-restart-parent-sub-read4")
            .unwrap()
            .unwrap()
            .state,
        "abandoned",
        "a helper caught mid-read is closed"
    );
    for n in 1..=3 {
        let r = store
            .subagent_run(&format!("s-restart-parent-sub-read{n}"))
            .unwrap()
            .unwrap();
        assert_eq!((r.state.as_str(), r.delivered), ("done", false));
    }
    // The listing: no helper, and the finished child reads as answered.
    let listed = d.call("task_result", serde_json::json!({})).payload;
    assert!(!listed.contains("read"), "{listed}");
    assert!(
        listed.contains("slt-current-number · s-restart-parent-sub-slt — answered"),
        "{listed}"
    );
    // By handle and by name, its recorded answer — the sentence `task_resume` points at.
    for asked in ["s-restart-parent-sub-slt", "slt-current-number"] {
        let r = task_result(&d, asked);
        assert_eq!(
            r.outcome,
            letibot_transcript::ToolOutcome::Ok,
            "{}",
            r.payload
        );
        assert!(
            r.payload.contains("Committed as `dce7cca`"),
            "{}",
            r.payload
        );
    }
    let resumed = d.call(
        "task_resume",
        serde_json::json!({"task": "s-restart-parent-sub-slt"}),
    );
    assert!(
        resumed
            .payload
            .contains("read its answer with `task_result`"),
        "{}",
        resumed.payload
    );
    // Nothing was delivered to the model.
    std::thread::sleep(Duration::from_millis(300));
    let _ = d.parent.wake();
    assert!(
        !user_rows(&d.hub()).iter().any(|r| r.contains("[task]")),
        "{:?}",
        user_rows(&d.hub())
    );
    d.stop(None);
}

/// **A job the old daemon's orderly stop ended is reminded** — pg-noop's `slt-sweep` (j17239),
/// killed at 02:05:55 by `end_running("the daemon stopping")`: after a clean restart no job row
/// reads `running`, so the reminder was never given.
#[test]
fn a_job_the_orderly_stop_ended_is_reminded() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    // Eighteen daemons and their children in parallel need more open files than a shell's 256 —
    // `room`'s note has the measurement.
    let Some(_) = room::room_for("eighteen daemons and their children in parallel", 1024) else {
        return;
    };
    let dir = Scratch::new("restart-job-stopped");
    let parts = parts_for(&config(
        &dir.repo(),
        &dir.store(),
        Endpoint::new("127.0.0.1", 1),
    ));
    let stub = Stub::new(plain(&parts.vocab, "noted"), 0);
    let cfg = config(&dir.repo(), &dir.store(), stub.endpoint.clone());
    seed_session(&parts, &cfg);
    {
        let store = Store::open(&dir.store()).unwrap();
        old_daemon_stopped(&store, Duration::from_secs(5));
        let mut j = a_job(
            "j17239",
            letibot_harnessd::restart::STOPPED_WITH_THE_DAEMON,
            "cd /data/slt && bash criterion-slt.sh > run0/out.txt 2>&1",
        );
        j.produced = 0;
        store.put_job(PARENT, &j).unwrap();
        store.deliver_job(PARENT, "j17239").unwrap();
        the_old_daemons(&store, "j17239");
    }
    let mut d = a_daemon(&parts, &cfg);
    let notice = d.parent.restore_after_restart().expect("a reminder");
    eprintln!("--- what the parent's model is told ---\n{notice}");
    assert!(
        notice.contains("- job `j17239`")
            && notice.contains("ENDED by the old daemon's orderly stop")
            && notice.contains("`run0/out.txt`"),
        "{notice}"
    );
    assert!(
        d.parent.restore_after_restart().is_none(),
        "the ended job was reminded a second time"
    );
    d.stop(None);
}

/// A job row as a build before `job_context` left it: a command, ended by that daemon's stop (or
/// left `running` by its crash), and no context naming any daemon.
fn a_relic(store: &Store, handle: &str) {
    store
        .put_job(
            PARENT,
            &a_job(
                handle,
                letibot_harnessd::restart::STOPPED_WITH_THE_DAEMON,
                "cd /data && ./target/release/letidb --port 5509 > /tmp/tpcds-server2.log 2>&1",
            ),
        )
        .unwrap();
    store.deliver_job(PARENT, handle).unwrap();
}

fn job_state(store: &Store, handle: &str) -> String {
    store
        .jobs(PARENT)
        .unwrap()
        .into_iter()
        .find(|j| j.handle == handle)
        .map(|j| j.state)
        .unwrap_or_default()
}

/// **A relic draws no REMINDER** — pg-noop, 2026-10-15: a daemon down a couple of seconds told the
/// model *"when the previous one stopped is unknown … NOTHING was restored automatically"* over
/// four jobs (j900, j610, j624, j2437) that builds long gone had left with no `job_context`. They
/// were the session's only records, they name no daemon, and `Stopped::latest` over no daemon is
/// unknown. Such a row is closed quietly and named to nobody.
///
/// **The restart itself is still said** (2026-10-30): a session with nothing running is told when
/// the daemon came back, because that is the one fact only the daemon has. Here that sentence is
/// the whole notice — no job in it — and, because the relics name no daemon, it is the one that
/// says the stop is unknown rather than the one that dates it.
#[test]
fn relics_alone_draw_no_reminder_and_are_closed_quietly() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    // Eighteen daemons and their children in parallel need more open files than a shell's 256 —
    // `room`'s note has the measurement.
    let Some(_) = room::room_for("eighteen daemons and their children in parallel", 1024) else {
        return;
    };
    let dir = Scratch::new("restart-relics-only");
    let parts = parts_for(&config(
        &dir.repo(),
        &dir.store(),
        Endpoint::new("127.0.0.1", 1),
    ));
    let stub = Stub::new(plain(&parts.vocab, "noted"), 0);
    let cfg = config(&dir.repo(), &dir.store(), stub.endpoint.clone());
    seed_session(&parts, &cfg);
    {
        let store = Store::open(&dir.store()).unwrap();
        // The daemon that really stopped did so a moment ago, in order — and left nothing here.
        old_daemon_stopped(&store, Duration::from_secs(2));
        for h in ["j900", "j610", "j624", "j2437"] {
            a_relic(&store, h);
        }
    }
    let mut d = a_daemon(&parts, &cfg);
    let notice = d.parent.restore_after_restart();
    // **The restart is said, and nothing is listed**: no relic is named, no reminder is drawn, and
    // the sentence is the empty one — the relics name no daemon, so the stop is unknown.
    let notice = notice.expect("a restart with nothing running is still said");
    assert!(
        notice.starts_with("[restart] the daemon came back at ")
            && notice.ends_with(": nothing was running"),
        "{notice}"
    );
    for relic in ["j900", "j610", "j624", "j2437"] {
        assert!(!notice.contains(relic), "a relic drew a notice: {notice}");
    }
    assert!(!notice.contains("Nothing above is running now"), "{notice}");
    assert!(warnings(&d.hub(), "restart_reminded").is_empty());
    assert!(warnings(&d.hub(), "restart_restored").is_empty());
    assert_eq!(warnings(&d.hub(), "restart_nothing").len(), 1);
    let store = Store::open(&dir.store()).unwrap();
    for h in ["j900", "j610", "j624", "j2437"] {
        assert_eq!(job_state(&store, h), "lost at restart", "{h}");
    }
    assert!(
        store.undelivered_jobs(PARENT).unwrap().is_empty(),
        "a closed relic is not an ending the next open announces"
    );
    assert!(d.parent.restore_after_restart().is_none());
    d.stop(None);
}

/// **Beside a real record, a relic neither dates the stop nor is reminded** — the down time is
/// the real record's daemon's, under the threshold, and only the real job is in the notice.
#[test]
fn a_relic_beside_a_real_record_takes_the_down_time_from_the_real_one() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    // Eighteen daemons and their children in parallel need more open files than a shell's 256 —
    // `room`'s note has the measurement.
    let Some(_) = room::room_for("eighteen daemons and their children in parallel", 1024) else {
        return;
    };
    let dir = Scratch::new("restart-relic-and-real");
    let parts = parts_for(&config(
        &dir.repo(),
        &dir.store(),
        Endpoint::new("127.0.0.1", 1),
    ));
    let stub = Stub::new(plain(&parts.vocab, "noted"), 0);
    let cfg = config(&dir.repo(), &dir.store(), stub.endpoint.clone());
    seed_session(&parts, &cfg);
    {
        let store = Store::open(&dir.store()).unwrap();
        old_daemon_stopped(&store, Duration::from_secs(4));
        a_relic(&store, "j900");
        store
            .put_job(
                PARENT,
                &a_job("j9", "running", "cargo build > build.log 2>&1"),
            )
            .unwrap();
        store.deliver_job(PARENT, "j9").unwrap();
        the_old_daemons(&store, "j9");
    }
    let mut d = a_daemon(&parts, &cfg);
    let notice = d.parent.restore_after_restart().expect("a reminder");
    eprintln!("--- what the parent's model is told ---\n{notice}");
    assert!(
        notice.starts_with("[restart] the daemon was down for "),
        "{notice}"
    );
    assert!(
        notice.contains("under the 1h00m restore threshold"),
        "{notice}"
    );
    assert!(!notice.contains("unknown"), "{notice}");
    assert!(notice.contains("- job `j9`"), "{notice}");
    assert!(!notice.contains("j900"), "the relic was reminded: {notice}");
    let reminded = warnings(&d.hub(), "restart_reminded");
    assert_eq!(reminded.len(), 1, "{reminded:?}");
    assert!(!reminded[0].contains("j900"), "{reminded:?}");
    let store = Store::open(&dir.store()).unwrap();
    assert_eq!(job_state(&store, "j900"), "lost at restart");
    assert_eq!(job_state(&store, "j9"), "lost at restart");
    d.stop(None);
}

/// **Past the threshold nothing comes back on its own** — the model is told how long the daemon
/// was down and given the one call per child — and `task_resume` brings the child back in that
/// one call.
#[test]
fn past_the_threshold_a_child_is_only_reminded_and_task_resume_brings_it_back() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    // Eighteen daemons and their children in parallel need more open files than a shell's 256 —
    // `room`'s note has the measurement.
    let Some(_) = room::room_for("eighteen daemons and their children in parallel", 1024) else {
        return;
    };
    let dir = Scratch::new("restart-remind");
    let parts = parts_for(&config(
        &dir.repo(),
        &dir.store(),
        Endpoint::new("127.0.0.1", 1),
    ));
    let stub = Stub::new(plain(&parts.vocab, ANSWER), 0);
    let mut cfg = config(&dir.repo(), &dir.store(), stub.endpoint.clone());
    cfg.restore_within = Duration::from_secs(2);
    let store = Store::open(&dir.store()).unwrap();
    old_daemon_stopped(&store, Duration::from_secs(10));
    let wt = dir.repo();
    let spec = TaskSpec {
        role: "coder".into(),
        worktree: Some(WorktreePlacement {
            path: wt.display().to_string(),
            branch: "agent/paused".into(),
            base_sha: "x".into(),
            main_tree: false,
        }),
        ..TaskSpec::default()
    };
    store
        .put_subagent_run(&run("s-restart-parent-sub-a", PARENT, "running", &spec))
        .unwrap();

    let mut d = a_daemon(&parts, &cfg);
    let notice = d.parent.restore_after_restart().expect("a reminder");
    eprintln!("--- what the parent's model is told ---\n{notice}");
    assert!(
        notice.contains("longer than the 2s restore threshold"),
        "{notice}"
    );
    assert!(
        notice.contains("NOTHING was restored automatically"),
        "{notice}"
    );
    assert!(
        notice.contains(r#"call `task_resume {"task": "s-restart-parent-sub-a"}`"#),
        "{notice}"
    );
    assert!(notice.contains("on branch `agent/paused`"), "{notice}");
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        d.registry.get("s-restart-parent-sub-a").is_none(),
        "nothing is resumed past the threshold"
    );
    assert_eq!(
        store
            .subagent_run("s-restart-parent-sub-a")
            .unwrap()
            .unwrap()
            .state,
        "paused"
    );
    assert!(!warnings(&d.hub(), "restart_reminded").is_empty());
    // A second open in the same daemon says nothing more.
    assert!(d.parent.restore_after_restart().is_none());

    // ONE call brings it back.
    let r = d.call(
        "task_resume",
        serde_json::json!({"task": "s-restart-parent-sub-a"}),
    );
    assert!(
        matches!(
            r.outcome,
            letibot_transcript::ToolOutcome::Backgrounded { .. }
        ),
        "{:?} {}",
        r.outcome,
        r.payload
    );
    wait_until("the resumed child to answer", || {
        answered(&d, "s-restart-parent-sub-a")
    });
    // And a second call on a finished child is refused by name.
    let again = d.call(
        "task_resume",
        serde_json::json!({"task": "s-restart-parent-sub-a"}),
    );
    assert!(
        again.payload.contains("finished its task"),
        "{}",
        again.payload
    );
    d.stop(None);
}

/// **A child of a child comes back with its parent** — the root resumes the child, and the
/// child's own open resumes the grandchild. The child died while it was still opening, so it has
/// no transcript: it is asked its task again, rather than told to carry on from nothing.
#[test]
fn a_grandchild_comes_back_when_its_parent_does() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    // Eighteen daemons and their children in parallel need more open files than a shell's 256 —
    // `room`'s note has the measurement.
    let Some(_) = room::room_for("eighteen daemons and their children in parallel", 1024) else {
        return;
    };
    let dir = Scratch::new("restart-nested");
    let parts = parts_for(&config(
        &dir.repo(),
        &dir.store(),
        Endpoint::new("127.0.0.1", 1),
    ));
    let stub = Stub::new(plain(&parts.vocab, ANSWER), 0);
    let cfg = config(&dir.repo(), &dir.store(), stub.endpoint.clone());
    let store = Store::open(&dir.store()).unwrap();
    old_daemon_stopped(&store, Duration::from_secs(5));
    let spec = TaskSpec {
        role: "coder".into(),
        ..TaskSpec::default()
    };
    let child = "s-restart-parent-sub-c";
    let grandchild = "s-restart-parent-sub-c-sub-g";
    store
        .put_subagent_run(&run(child, PARENT, "running", &spec))
        .unwrap();
    store
        .put_subagent_run(&run(grandchild, child, "running", &spec))
        .unwrap();

    let mut d = a_daemon(&parts, &cfg);
    d.parent
        .restore_after_restart()
        .expect("the child comes back");
    wait_until("the grandchild to be back, under its own parent", || {
        d.registry.get(grandchild).is_some()
    });
    assert_eq!(d.registry.parent_of(grandchild).as_deref(), Some(child));
    let c = d.registry.get(child).unwrap();
    wait_until("the child to be asked its task", || {
        user_rows(&c)
            .iter()
            .any(|r| r.contains(&format!("the subtask of {child}")))
    });
    assert!(
        !user_rows(&c)
            .iter()
            .any(|r| r.starts_with("[restart] the daemon restarted while you were working")),
        "a child with no transcript is not told to carry on"
    );
    wait_until("the grandchild to finish", || {
        store.subagent_run(grandchild).unwrap().unwrap().state == "done"
    });
    d.stop(None);
}

/// **The edge cases a restore must not get wrong**, each named in the notice: a worktree that is
/// gone, a child that crashed the daemon three times in a row, a child a person stopped (never
/// resumed, never mentioned), and a finished child whose answer never reached its parent (handed
/// over, not respawned).
#[test]
fn what_must_not_be_respawned_is_reported_or_delivered() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    // Eighteen daemons and their children in parallel need more open files than a shell's 256 —
    // `room`'s note has the measurement.
    let Some(_) = room::room_for("eighteen daemons and their children in parallel", 1024) else {
        return;
    };
    let dir = Scratch::new("restart-edges");
    let parts = parts_for(&config(
        &dir.repo(),
        &dir.store(),
        Endpoint::new("127.0.0.1", 1),
    ));
    let stub = Stub::new(plain(&parts.vocab, "noted"), 0);
    let cfg = config(&dir.repo(), &dir.store(), stub.endpoint.clone());
    let store = Store::open(&dir.store()).unwrap();
    old_daemon_stopped(&store, Duration::from_secs(5));
    let gone = TaskSpec {
        worktree: Some(WorktreePlacement {
            path: dir.root.join("no-such-worktree").display().to_string(),
            branch: "agent/gone".into(),
            base_sha: "x".into(),
            main_tree: false,
        }),
        ..TaskSpec::default()
    };
    let plain_spec = TaskSpec::default();
    store
        .put_subagent_run(&run("s-restart-parent-sub-gone", PARENT, "running", &gone))
        .unwrap();
    let mut looping = run("s-restart-parent-sub-loop", PARENT, "running", &plain_spec);
    looping.resumes = 3;
    store.put_subagent_run(&looping).unwrap();
    store
        .put_subagent_run(&run(
            "s-restart-parent-sub-killed",
            PARENT,
            "killed",
            &plain_spec,
        ))
        .unwrap();
    let mut done = run("s-restart-parent-sub-done", PARENT, "done", &plain_spec);
    done.outcome = Some("the answer nobody heard".into());
    store.put_subagent_run(&done).unwrap();

    let mut d = a_daemon(&parts, &cfg);
    let notice = d.parent.restore_after_restart().expect("a notice");
    eprintln!("--- what the parent's model is told ---\n{notice}");
    assert!(
        // Named by its record: the branch its worktree was cut on, `agent/gone`.
        notice.contains("`gone · s-restart-parent-sub-gone`")
            && notice.contains("could NOT be resumed")
            && notice.contains("no-such-worktree")
            && notice.contains("is gone"),
        "{notice}"
    );
    assert!(notice.contains("3 times in a row"), "{notice}");
    assert!(!notice.contains("sub-killed"), "{notice}");
    std::thread::sleep(Duration::from_millis(300));
    for h in [
        "s-restart-parent-sub-gone",
        "s-restart-parent-sub-loop",
        "s-restart-parent-sub-killed",
        "s-restart-parent-sub-done",
    ] {
        assert!(d.registry.get(h).is_none(), "{h} was respawned");
    }
    assert_eq!(
        store
            .subagent_run("s-restart-parent-sub-gone")
            .unwrap()
            .unwrap()
            .state,
        "abandoned"
    );
    assert_eq!(
        store
            .subagent_run("s-restart-parent-sub-killed")
            .unwrap()
            .unwrap()
            .state,
        "killed"
    );
    // The finished one is collectable, and its completion is delivered.
    let r = task_result(&d, "s-restart-parent-sub-done");
    assert!(
        r.payload.contains("the answer nobody heard"),
        "{}",
        r.payload
    );
    wait_until("the undelivered answer to be delivered", || {
        let _ = d.parent.wake();
        store
            .subagent_run("s-restart-parent-sub-done")
            .unwrap()
            .unwrap()
            .delivered
    });
    assert!(
        user_rows(&d.hub())
            .iter()
            .any(|r| r.contains("s-restart-parent-sub-done") && r.contains("has finished")),
        "{:?}",
        user_rows(&d.hub())
    );
    d.stop(None);
}

/// The parent's session row, as the old daemon left it — written by opening the parent once in a
/// daemon that is then dropped. The `job` and `todo` rows reference it.
fn seed_session(parts: &Parts, cfg: &Config) {
    let d = a_daemon(parts, cfg);
    d.stop(None);
}

/// Rows a crashed daemon leaves for a session's monitors and background jobs, and a todo row that
/// waits on the job.
fn stage_monitors_and_jobs(store: &Store) {
    store
        .put_monitor(
            PARENT,
            "built",
            r#"{"name":"built","path":"out.txt","ttl_ms":600000}"#,
            OLD,
            600_000,
        )
        .unwrap();
    store
        .put_monitor(
            PARENT,
            "on-build",
            r#"{"name":"on-build","job":"j9"}"#,
            OLD,
            300_000,
        )
        .unwrap();
    // A reminder the model hung on a scope of its own — the soak's `lubuntu2-soak`.
    store
        .put_monitor(
            PARENT,
            "soak-reminder",
            r#"{"name":"soak-reminder","owner":"lubuntu2-soak","repeat":true,"timer_ms":1800000,"ttl_ms":3600000}"#,
            OLD,
            3_600_000,
        )
        .unwrap();
    store
        .put_job(
            PARENT,
            &JobRecord {
                handle: "j9".into(),
                command: "cargo build > build.log".into(),
                how: "you asked for this to run in the background".into(),
                state: "running".into(),
                produced: 0,
                elapsed_ms: 0,
                redirect: Some("build.log".into()),
            },
        )
        .unwrap();
    store
        .put_job_context(
            PARENT,
            &JobContext {
                handle: "j9".into(),
                slug: Some("build".into()),
                cwd: "/somewhere".into(),
                instance: OLD.into(),
                started_ms: letibot_sessionlog::registry::now_ms() - 120_000,
            },
        )
        .unwrap();
    store
        .put_todos(
            PARENT,
            &[serde_json::from_value::<TodoItem>(serde_json::json!({
                "content": "ship once the build is done",
                "status": "pending",
                "when": {"kind": "job", "handle": "j9"}
            }))
            .unwrap()],
        )
        .unwrap();
}

/// **A restart with nothing running is SAID — once, through the daemon's own open — and it starts
/// no turn.** The operator, 2026-10-30, after one restart had told the model something and the next
/// had said nothing: *"mb always tell agent about restart? with the current time lol"*.
///
/// The two doors are pinned here and in `harness::tests::restart_notice`: a restore is work to
/// continue and keeps its turn, and a restart with nothing to restore is a NOTICE — a transcript
/// row the model reads when it next reads. The daemon's open rings the session either way (that
/// ring is what carries a todo row, a queue handover and a restored child), so the assertion that
/// matters is the one after it: the wake finds nothing to say and no model call is made.
#[test]
fn an_empty_restart_is_said_once_and_starts_no_turn() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    // Eighteen daemons and their children in parallel need more open files than a shell's 256 —
    // `room`'s note has the measurement.
    let Some(_) = room::room_for("eighteen daemons and their children in parallel", 1024) else {
        return;
    };
    let dir = Scratch::new("restart-nothing");
    let parts = parts_for(&config(
        &dir.repo(),
        &dir.store(),
        Endpoint::new("127.0.0.1", 1),
    ));
    let stub = Stub::new(plain(&parts.vocab, "noted"), 0);
    let cfg = config(&dir.repo(), &dir.store(), stub.endpoint.clone());
    seed_session(&parts, &cfg);
    let store = Store::open(&dir.store()).unwrap();
    old_daemon_stopped(&store, Duration::from_secs(1));
    // **Nothing was running**, and the session still names the daemon that held it: a monitor that
    // had already ended. That is where the down time is read from — a session whose rows name no
    // daemon at all says `unknown` (`harness::tests::restart_notice`).
    store
        .put_monitor(
            PARENT,
            "ended",
            r#"{"name":"ended","path":"x"}"#,
            OLD,
            60_000,
        )
        .unwrap();
    store.end_monitor(PARENT, "ended").unwrap();
    let stopped = store.life(OLD).unwrap().unwrap().stopped_ms.unwrap();

    let registry = Registry::new();
    registry
        .create(PARENT, "the parent", wiring(&cfg))
        .expect("created");
    let mut sessions =
        Sessions::open_first(&parts, cfg.clone(), registry.clone()).expect("the daemon opens");
    let hub = registry.get(PARENT).unwrap();
    let back = registry.started_ms();
    // **The whole sentence**, not a fragment: the return time from the daemon's own clock, the down
    // time and the stop it is counted from, and that nothing was running.
    let sentence = format!(
        "[restart] the daemon came back at {} (down {}, stopped at {}): nothing was running",
        letibot_harnessd::subagent_record::utc(back),
        letibot_harnessd::restart::span(Duration::from_millis(back.saturating_sub(stopped))),
        letibot_harnessd::subagent_record::utc(stopped)
    );
    assert_eq!(warnings(&hub, "restart_nothing"), vec![sentence.clone()]);
    assert!(warnings(&hub, "restart_restored").is_empty());
    assert!(warnings(&hub, "restart_reminded").is_empty());

    // **The open rang the session** — that ring is the daemon's own, and it carries everything a
    // restored session owes. This session owes nothing, so the wake says so.
    let woke = loop {
        match registry.next_work_until(Some(Instant::now() + Duration::from_secs(5))) {
            WorkOrIdle::Work(Work::Woken(id)) => break id,
            WorkOrIdle::Work(_) => continue,
            _ => panic!("the opened session was not rung"),
        }
    };
    assert_eq!(woke, PARENT);
    assert!(
        matches!(sessions.wake(PARENT), Outcome::Ignored),
        "a restart with nothing running opened a turn"
    );
    // **And the model was told anyway**, as a row it reads at its next turn.
    let rows = user_rows(&hub);
    assert!(
        rows.contains(&sentence),
        "the notice never reached the transcript: {rows:?}"
    );
    assert_eq!(
        stub.arrived(),
        0,
        "the empty restart cost a model call — it is a notice, not a turn"
    );
    // **Once.** A second wake finds nothing, and the sentence is not said again.
    assert!(matches!(sessions.wake(PARENT), Outcome::Ignored));
    assert_eq!(
        user_rows(&hub)
            .iter()
            .filter(|r| r.starts_with("[restart]"))
            .count(),
        1,
        "{:?}",
        user_rows(&hub)
    );
    assert_eq!(stub.arrived(), 0);
    registry.close();
}

/// **Under the threshold: monitors come back, a monitor that cannot is reported, and a job is
/// reminded with the call that runs it again** — and the reminder starts the model's turn, through
/// the daemon's own open (`Sessions`), its ring, and its wake.
#[test]
fn monitors_are_rearmed_and_jobs_reminded_and_the_reminder_is_a_turn() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    // Eighteen daemons and their children in parallel need more open files than a shell's 256 —
    // `room`'s note has the measurement.
    let Some(_) = room::room_for("eighteen daemons and their children in parallel", 1024) else {
        return;
    };
    let dir = Scratch::new("restart-monitors");
    let parts = parts_for(&config(
        &dir.repo(),
        &dir.store(),
        Endpoint::new("127.0.0.1", 1),
    ));
    let stub = Stub::new(plain(&parts.vocab, "noted"), 0);
    let cfg = config(&dir.repo(), &dir.store(), stub.endpoint.clone());
    seed_session(&parts, &cfg);
    {
        let store = Store::open(&dir.store()).unwrap();
        old_daemon_stopped(&store, Duration::from_secs(5));
        stage_monitors_and_jobs(&store);
    }
    let registry = Registry::new();
    registry
        .create(PARENT, "the parent", wiring(&cfg))
        .expect("created");
    let mut sessions =
        Sessions::open_first(&parts, cfg.clone(), registry.clone()).expect("the daemon opens");
    let hub = registry.get(PARENT).unwrap();
    let restored = warnings(&hub, "restart_restored");
    assert_eq!(restored.len(), 1, "{restored:?}");
    assert!(
        restored[0].contains("monitor `built`: re-armed"),
        "{restored:?}"
    );
    assert!(
        restored[0].contains("monitor `on-build`: NOT re-armed"),
        "{restored:?}"
    );
    assert!(restored[0].contains("job `j9`: reminded (not re-run), 1 waiting row(s) released"));
    assert!(
        restored[0]
            .contains("monitor `soak-reminder`: not re-armed — owned by the scope `lubuntu2-soak`"),
        "a scoped monitor is the model's to bring back: {restored:?}"
    );

    // The open rang the session: the reminder is a turn.
    let woke = loop {
        match registry.next_work_until(Some(Instant::now() + Duration::from_secs(5))) {
            WorkOrIdle::Work(Work::Woken(id)) => break id,
            WorkOrIdle::Work(_) => continue,
            _ => panic!("the restored session was not rung"),
        }
    };
    assert_eq!(woke, PARENT);
    assert!(matches!(sessions.wake(PARENT), Outcome::Replied(_)));
    let rows = user_rows(&hub);
    let notice = rows
        .iter()
        .find(|r| r.starts_with("[restart] the daemon was down for"))
        .unwrap_or_else(|| panic!("the reminder reached the model: {rows:?}"));
    eprintln!("--- what the parent's model is told ---\n{notice}");
    assert!(
        notice.contains("- monitor `built` was re-armed"),
        "{notice}"
    );
    assert!(
        notice.contains("- monitor `on-build` could NOT be re-armed"),
        "{notice}"
    );
    assert!(
        notice.contains(
            "- monitor `soak-reminder` was owned by the scope `lubuntu2-soak`, which the stopped \
             daemon held — it was NOT re-armed"
        ),
        "{notice}"
    );
    assert!(
        notice.contains(
            r#"To run it again: `bash {"command":"cargo build > build.log","background":true,"cwd":"/somewhere","slug":"build"}`"#
        ),
        "{notice}"
    );
    assert!(notice.contains("`build.log`, which survives"), "{notice}");
    assert!(
        notice.contains("Rows that waited on it: `ship once the build is done`")
            && notice.contains("Their wait is RELEASED"),
        "{notice}"
    );
    let store = Store::open(&dir.store()).unwrap();
    let jobs = store.jobs(PARENT).unwrap();
    assert_eq!(jobs[0].state, "lost at restart");
    let monitors = store.monitors_of(PARENT).unwrap();
    let built = monitors.iter().find(|m| m.name == "built").unwrap();
    assert!(
        built.live && built.instance == registry.instance(),
        "{built:?}"
    );
    assert!(!monitors.iter().find(|m| m.name == "on-build").unwrap().live);
    assert!(
        !monitors
            .iter()
            .find(|m| m.name == "soak-reminder")
            .unwrap()
            .live,
        "nothing armed it again"
    );
    registry.close();
}

/// **Over the threshold the same session is only reminded** — no monitor is armed, and each is
/// given back as the call that declares it.
#[test]
fn past_the_threshold_monitors_and_jobs_are_only_reminded() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    // Eighteen daemons and their children in parallel need more open files than a shell's 256 —
    // `room`'s note has the measurement.
    let Some(_) = room::room_for("eighteen daemons and their children in parallel", 1024) else {
        return;
    };
    let dir = Scratch::new("restart-monitors-late");
    let parts = parts_for(&config(
        &dir.repo(),
        &dir.store(),
        Endpoint::new("127.0.0.1", 1),
    ));
    let stub = Stub::new(plain(&parts.vocab, "noted"), 0);
    let mut cfg = config(&dir.repo(), &dir.store(), stub.endpoint.clone());
    cfg.restore_within = Duration::from_secs(2);
    seed_session(&parts, &cfg);
    {
        let store = Store::open(&dir.store()).unwrap();
        old_daemon_stopped(&store, Duration::from_secs(10));
        stage_monitors_and_jobs(&store);
    }
    let mut d = a_daemon(&parts, &cfg);
    let notice = d.parent.restore_after_restart().expect("a reminder");
    eprintln!("--- what the parent's model is told ---\n{notice}");
    assert!(
        notice.contains("NOTHING was restored automatically"),
        "{notice}"
    );
    assert!(
        notice.contains(
            r#"- monitor `built` was watching; re-arm it with `monitor {"name":"built","path":"out.txt","ttl_ms":600000}`"#
        ),
        "{notice}"
    );
    assert!(notice.contains("To run it again: `bash {"), "{notice}");
    assert!(
        d.parent
            .monitors()
            .map(|m| m.live_names().is_empty())
            .unwrap_or(true),
        "nothing is armed past the threshold"
    );
    d.stop(None);
}

/// **Only a DAEMON stop triggers any of it** — the operator: *"this is the case for when harnessd
/// stopped of course"*. A session whose records were written by the daemon that is running —
/// however long it has sat idle, and however stale its heartbeat looks — is opened again (a head
/// attaching, a `/resume`) and nothing is restored, reminded or rung.
#[test]
fn a_session_reopened_by_the_daemon_that_wrote_its_records_restores_nothing() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    // Eighteen daemons and their children in parallel need more open files than a shell's 256 —
    // `room`'s note has the measurement.
    let Some(_) = room::room_for("eighteen daemons and their children in parallel", 1024) else {
        return;
    };
    let dir = Scratch::new("restart-idle");
    let parts = parts_for(&config(
        &dir.repo(),
        &dir.store(),
        Endpoint::new("127.0.0.1", 1),
    ));
    let stub = Stub::new(plain(&parts.vocab, "noted"), 0);
    let cfg = config(&dir.repo(), &dir.store(), stub.endpoint.clone());
    let mut d = a_daemon(&parts, &cfg);
    let me = d.registry.instance().to_string();
    let store = Store::open(&dir.store()).unwrap();
    // This daemon's own life, with a heartbeat that looks hours old — a long idle.
    store.life_started(&me, 1).unwrap();
    store.life_beat(&me, 2).unwrap();
    let mut mine = run(
        "s-restart-parent-sub-mine",
        PARENT,
        "running",
        &TaskSpec::default(),
    );
    mine.instance = me.clone();
    store.put_subagent_run(&mine).unwrap();
    store
        .put_monitor(
            PARENT,
            "mine",
            r#"{"name":"mine","path":"x"}"#,
            &me,
            300_000,
        )
        .unwrap();

    assert!(d.parent.restore_after_restart().is_none());
    // **A SECOND harness for one session is REFUSED, by name** — the writer claim
    // (`letibot_tokencore::store::writers`, `harnessd::writer`). The daemon's own door never
    // builds one: `Sessions::open_as` answers a `/resume` or a head's attach for a session it
    // holds with the harness it already has. This is what a hand-built second one gets, and it
    // is the reason the guard exists — two harnesses on one transcript interleave rows and the
    // store's append-only trigger refuses the second, leaving a conversation that does not
    // restore (MEASURED 2026-10-26, `s-gatekeeper#t0`).
    let hub2 = Hub::new(PARENT);
    let second = Harness::open_with_registry(
        &parts,
        cfg.clone(),
        hub2.clone(),
        None,
        None,
        d.registry.clone(),
    );
    let refusal = second
        .err()
        .expect("a second harness for one session is refused")
        .to_string();
    assert!(
        refusal.contains("is being written by another process"),
        "the refusal names the reason: {refusal}"
    );
    // **And the reopen that really happens** — the daemon's harness gone, its claim given back
    // with it, the session opened again: nothing restored and nothing reminded, because the
    // records name the daemon that wrote them.
    let registry = d.registry.clone();
    let hub = d.hub();
    let parent = d.parent;
    drop(parent);
    let hub3 = Hub::new(PARENT);
    let mut again = Harness::open_with_registry(
        &parts,
        cfg.clone(),
        hub3.clone(),
        None,
        None,
        registry.clone(),
    )
    .expect("reopens");
    assert!(again.restore_after_restart().is_none());
    assert!(warnings(&hub, "restart_restored").is_empty());
    assert!(warnings(&hub3, "restart_reminded").is_empty());
    std::thread::sleep(Duration::from_millis(200));
    assert!(registry.get("s-restart-parent-sub-mine").is_none());
    // The orderly stop, for the reason `Daemon::stop` is one: the registry closes and every
    // session's services end with it.
    registry.close();
}

/// **A stop no record dates is UNKNOWN, and an unknown down time restores nothing** — the
/// operator, pasting the first restart onto a new build: *"the daemon was down for 0s (stopped
/// at …, estimated from nothing (no record said when); back at …)"*, *"these gray walls are
/// hilarious"*. The old daemon here kept no `daemon_life` row (a build before the lifeline); its
/// monitor and its job name it, so the stop is truly unknown — not merely undated by a relic.
#[test]
fn a_stop_no_record_dates_is_said_unknown_and_restores_nothing() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    // Eighteen daemons and their children in parallel need more open files than a shell's 256 —
    // `room`'s note has the measurement.
    let Some(_) = room::room_for("eighteen daemons and their children in parallel", 1024) else {
        return;
    };
    let dir = Scratch::new("restart-unknown");
    let parts = parts_for(&config(
        &dir.repo(),
        &dir.store(),
        Endpoint::new("127.0.0.1", 1),
    ));
    let stub = Stub::new(plain(&parts.vocab, "noted"), 0);
    let cfg = config(&dir.repo(), &dir.store(), stub.endpoint.clone());
    seed_session(&parts, &cfg);
    {
        let store = Store::open(&dir.store()).unwrap();
        // No `old_daemon_stopped`: the old daemon kept no life row.
        store
            .put_monitor(
                PARENT,
                "built",
                r#"{"name":"built","path":"out.txt","ttl_ms":600000}"#,
                OLD,
                600_000,
            )
            .unwrap();
        store
            .put_job(
                PARENT,
                &JobRecord {
                    handle: "j46".into(),
                    command: "cd /tmp/h && ./run_acceptance.sh > /tmp/acceptance-child.log 2>&1; \
                              echo \"exit=$?\""
                        .into(),
                    how: String::new(),
                    state: "running".into(),
                    produced: 0,
                    elapsed_ms: 0,
                    // What a running row looked like before this build: never filled.
                    redirect: None,
                },
            )
            .unwrap();
        the_old_daemons(&store, "j46");
    }
    let mut d = a_daemon(&parts, &cfg);
    let notice = d.parent.restore_after_restart().expect("a reminder");
    eprintln!("--- what the parent's model is told ---\n{notice}");
    assert!(
        notice.starts_with("[restart] the daemon restarted at "),
        "{notice}"
    );
    assert!(
        notice.contains("when the previous one stopped is unknown"),
        "{notice}"
    );
    assert!(
        notice.contains("An unknown down time counts as longer than the 1h00m restore threshold"),
        "{notice}"
    );
    for wrong in [
        "down for",
        "estimated from nothing",
        "restored where it could be",
    ] {
        assert!(!notice.contains(wrong), "`{wrong}`: {notice}");
    }
    assert!(
        notice.contains("- monitor `built` was watching; re-arm it with"),
        "{notice}"
    );
    assert!(
        notice.contains("Its output went to `/tmp/acceptance-child.log`, which survives"),
        "{notice}"
    );
    assert!(
        d.parent
            .monitors()
            .map(|m| m.live_names().is_empty())
            .unwrap_or(true),
        "nothing is armed when the down time is unknown"
    );
    assert!(warnings(&d.hub(), "restart_restored").is_empty());
    let reminded = warnings(&d.hub(), "restart_reminded");
    assert_eq!(reminded.len(), 1, "{reminded:?}");
    assert!(
        reminded[0].contains("monitor `built`: reminded, not re-armed")
            && reminded[0].contains("job `j46`: reminded (not re-run)"),
        "{reminded:?}"
    );
    d.stop(None);
}

/// **Restored is said only when something was** — under the threshold, a session whose only
/// record is a job (which is never re-run) was raised as `restart_restored`, *"restored where
/// it could be"*, over a list of reminders.
#[test]
fn under_the_threshold_a_session_with_only_a_job_is_reminded_not_restored() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    // Eighteen daemons and their children in parallel need more open files than a shell's 256 —
    // `room`'s note has the measurement.
    let Some(_) = room::room_for("eighteen daemons and their children in parallel", 1024) else {
        return;
    };
    let dir = Scratch::new("restart-jobs-only");
    let parts = parts_for(&config(
        &dir.repo(),
        &dir.store(),
        Endpoint::new("127.0.0.1", 1),
    ));
    let stub = Stub::new(plain(&parts.vocab, "noted"), 0);
    let cfg = config(&dir.repo(), &dir.store(), stub.endpoint.clone());
    seed_session(&parts, &cfg);
    {
        let store = Store::open(&dir.store()).unwrap();
        old_daemon_stopped(&store, Duration::from_secs(5));
        store
            .put_job(
                PARENT,
                &JobRecord {
                    handle: "j9".into(),
                    command: "cargo build".into(),
                    how: String::new(),
                    state: "running".into(),
                    produced: 0,
                    elapsed_ms: 0,
                    redirect: None,
                },
            )
            .unwrap();
        store
            .put_job_context(
                PARENT,
                &JobContext {
                    handle: "j9".into(),
                    slug: None,
                    cwd: "/somewhere".into(),
                    instance: OLD.into(),
                    started_ms: letibot_sessionlog::registry::now_ms() - 60_000,
                },
            )
            .unwrap();
    }
    let mut d = a_daemon(&parts, &cfg);
    let notice = d.parent.restore_after_restart().expect("a reminder");
    eprintln!("--- what the parent's model is told ---\n{notice}");
    assert!(
        notice.contains("under the 1h00m restore threshold, but nothing could be restored"),
        "{notice}"
    );
    // **And the time**, which is what a restart is for: the return time and the stop it is counted
    // from are in every notice, whatever is listed under it.
    assert!(notice.contains("; back at "), "{notice}");
    assert!(notice.contains("stopped at "), "{notice}");
    assert!(!notice.contains("restored where it could be"), "{notice}");
    assert!(
        notice.contains("Its output was in the old daemon's memory and is gone."),
        "{notice}"
    );
    assert!(notice.contains("Nothing above is running now."), "{notice}");
    assert!(warnings(&d.hub(), "restart_restored").is_empty());
    assert_eq!(warnings(&d.hub(), "restart_reminded").len(), 1);
    // **Said once**: the row now reads `lost at restart`, an ending, and it was said — so the next
    // open does not announce it again as an ending nobody was told about (R91.5).
    assert!(
        d.parent.restore_after_restart().is_none(),
        "the lost job was announced a second time"
    );
    d.stop(None);
}

fn a_job(handle: &str, state: &str, command: &str) -> JobRecord {
    JobRecord {
        handle: handle.into(),
        command: command.into(),
        how: String::new(),
        state: state.into(),
        produced: 512,
        elapsed_ms: 90_000,
        redirect: None,
    }
}

fn the_old_daemons(store: &Store, handle: &str) {
    store
        .put_job_context(
            PARENT,
            &JobContext {
                handle: handle.into(),
                slug: None,
                cwd: "/somewhere".into(),
                instance: OLD.into(),
                started_ms: letibot_sessionlog::registry::now_ms() - 120_000,
            },
        )
        .unwrap();
}

/// **A job whose ending was held when the daemon stopped is said by the next one, once** (TODO.md
/// R91.5). Restore used to collect only `running` rows: a job that settled during a long turn and
/// was HELD (R79) had a row reading `exited 0`, the daemon stopped, and the model never heard its
/// result. One that WAS delivered is not said again, and neither is the held one on a second open.
#[test]
fn a_job_that_ended_untold_before_the_stop_is_said_after_it_once() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    // Eighteen daemons and their children in parallel need more open files than a shell's 256 —
    // `room`'s note has the measurement.
    let Some(_) = room::room_for("eighteen daemons and their children in parallel", 1024) else {
        return;
    };
    let dir = Scratch::new("restart-held-job");
    let parts = parts_for(&config(
        &dir.repo(),
        &dir.store(),
        Endpoint::new("127.0.0.1", 1),
    ));
    let stub = Stub::new(plain(&parts.vocab, "noted"), 0);
    let cfg = config(&dir.repo(), &dir.store(), stub.endpoint.clone());
    seed_session(&parts, &cfg);
    {
        let store = Store::open(&dir.store()).unwrap();
        old_daemon_stopped(&store, Duration::from_secs(5));
        for (h, cmd) in [
            ("j21", "cargo test --release > /tmp/held-tests.log 2>&1"),
            ("j22", "cargo build"),
        ] {
            store.put_job(PARENT, &a_job(h, "running", "")).unwrap();
            the_old_daemons(&store, h);
            store.put_job(PARENT, &a_job(h, "exited 0", cmd)).unwrap();
        }
        // j22's ending reached the model before the stop; j21's was held.
        store.deliver_job(PARENT, "j22").unwrap();
    }
    let mut d = a_daemon(&parts, &cfg);
    let notice = d
        .parent
        .restore_after_restart()
        .expect("the held ending is said");
    eprintln!("--- what the parent's model is told ---\n{notice}");
    assert!(
        notice.contains(
            "- job `j21` had ENDED before the stop — exited 0, 512 byte(s) of output, after 1m30s \
             — and its result never reached you."
        ),
        "{notice}"
    );
    assert!(
        notice.contains("Its output went to `/tmp/held-tests.log` — read that."),
        "{notice}"
    );
    assert!(
        !notice.contains("j22"),
        "a delivered ending was said again: {notice}"
    );
    assert!(
        d.parent.restore_after_restart().is_none(),
        "the held ending was said twice"
    );
    d.stop(None);
}

/// **A job handle minted after a restart is above every one the store holds** (TODO.md R91.3).
/// The counter started at `j1` in every daemon life, so a session's first job after a restart
/// could be its own pre-restart `j7` to every reader that matches by handle, and its row
/// inherited the old job's command.
#[test]
fn a_job_handle_minted_after_a_restart_is_above_every_one_the_store_holds() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    // Eighteen daemons and their children in parallel need more open files than a shell's 256 —
    // `room`'s note has the measurement.
    let Some(_) = room::room_for("eighteen daemons and their children in parallel", 1024) else {
        return;
    };
    let dir = Scratch::new("restart-job-ids");
    let parts = parts_for(&config(
        &dir.repo(),
        &dir.store(),
        Endpoint::new("127.0.0.1", 1),
    ));
    let stub = Stub::new(plain(&parts.vocab, "noted"), 0);
    let cfg = config(&dir.repo(), &dir.store(), stub.endpoint.clone());
    seed_session(&parts, &cfg);
    {
        let store = Store::open(&dir.store()).unwrap();
        store
            .put_job(PARENT, &a_job("j9000", "exited 0", "cargo build"))
            .unwrap();
    }
    let d = a_daemon(&parts, &cfg);
    let next = letibot_tools::exec::JobId::next();
    let n: u64 = next.0[1..].parse().unwrap();
    assert!(n > 9000, "{next} was minted over a store that holds j9000");
    d.stop(None);
}

/// **A job still running when a VM child's backend is closed is named, and its row settled**
/// (TODO.md R92.1, second half). A VM child's backend is closed after its first turn and its
/// background jobs die with the guest; the watcher is stopped first, so nothing ever said so and
/// the row read `running` for ever. Driven on a host backend, which is the same `processes()` read.
#[test]
fn the_jobs_a_closing_backend_takes_with_it_are_named_and_their_rows_settled() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    // Eighteen daemons and their children in parallel need more open files than a shell's 256 —
    // `room`'s note has the measurement.
    let Some(_) = room::room_for("eighteen daemons and their children in parallel", 1024) else {
        return;
    };
    let dir = Scratch::new("vm-lost-jobs");
    let parts = parts_for(&config(
        &dir.repo(),
        &dir.store(),
        Endpoint::new("127.0.0.1", 1),
    ));
    let stub = Stub::new(plain(&parts.vocab, "noted"), 0);
    let cfg = config(&dir.repo(), &dir.store(), stub.endpoint.clone());
    let d = a_daemon(&parts, &cfg);
    let r = d.call(
        "bash",
        serde_json::json!({"command": "sleep 60", "background": true, "slug": "long-sleep"}),
    );
    assert!(
        r.payload.contains("long-sleep"),
        "the job did not start: {}",
        r.payload
    );
    let lost = d.parent.jobs_lost_with_the_vm();
    assert_eq!(lost.len(), 1, "{lost:?}");
    let handle = lost[0].id.0.clone();
    let _ = d.parent.close_backend();
    let store = Store::open(&dir.store()).unwrap();
    let row = store
        .jobs(PARENT)
        .unwrap()
        .into_iter()
        .find(|j| j.handle == handle)
        .expect("the job's row");
    assert_eq!(row.state, "lost with its VM");
    assert!(
        store.undelivered_jobs(PARENT).unwrap().is_empty(),
        "the loss is said in the answer, not again at a restart"
    );
    d.stop(None);
}
