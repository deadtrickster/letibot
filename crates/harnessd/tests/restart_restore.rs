//! **A daemon restart brings back what a session was running — or, past the threshold, reminds
//! the model of it.**
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
                std::thread::spawn(move || {
                    let (lock, cv) = &*g;
                    let mut st = lock.lock().unwrap();
                    let n = st.0;
                    st.0 += 1;
                    cv.notify_all();
                    while n < hold && st.1 <= n {
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
            "[restart] subagent `{handle}` (fix the flaky test) was running when the daemon \
             restarted and has been resumed."
        )),
        "{notice}"
    );
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
    assert!(
        said.iter().any(|r| r.contains("has been resumed")),
        "the parent's model got the restart line: {said:?}"
    );
    wait_until("the delivery to be recorded", || {
        store.subagent_run(&handle).unwrap().unwrap().delivered
    });
    two.stop(None);
}

/// **Past the threshold nothing comes back on its own** — the model is told how long the daemon
/// was down and given the one call per child — and `task_resume` brings the child back in that
/// one call.
#[test]
fn past_the_threshold_a_child_is_only_reminded_and_task_resume_brings_it_back() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
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
        notice.contains("`s-restart-parent-sub-gone`")
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

/// **Under the threshold: monitors come back, a monitor that cannot is reported, and a job is
/// reminded with the call that runs it again** — and the reminder starts the model's turn, through
/// the daemon's own open (`Sessions`), its ring, and its wake.
#[test]
fn monitors_are_rearmed_and_jobs_reminded_and_the_reminder_is_a_turn() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
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
    registry.close();
}

/// **Over the threshold the same session is only reminded** — no monitor is armed, and each is
/// given back as the call that declares it.
#[test]
fn past_the_threshold_monitors_and_jobs_are_only_reminded() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
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
    // And a second harness on the same session in the same daemon — the reopen a head's attach
    // or `/resume` reaches — finds the same.
    let hub2 = Hub::new(PARENT);
    let mut again = Harness::open_with_registry(
        &parts,
        cfg.clone(),
        hub2.clone(),
        None,
        None,
        d.registry.clone(),
    )
    .expect("reopens");
    assert!(again.restore_after_restart().is_none());
    assert!(warnings(&d.hub(), "restart_restored").is_empty());
    assert!(warnings(&hub2, "restart_reminded").is_empty());
    std::thread::sleep(Duration::from_millis(200));
    assert!(d.registry.get("s-restart-parent-sub-mine").is_none());
    d.stop(None);
}
