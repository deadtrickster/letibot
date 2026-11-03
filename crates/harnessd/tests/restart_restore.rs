//! A daemon restart brings back what a session was running - or, past the threshold reminds
//! the model of it. Under the threshold a restart restores the children, monitors and jobs

use std::io::Write;
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
use letibot_tokencore::store::{
    JobContext, JobRecord, Store, SubagentRun, TodoBy, TodoItem, TodoStatus,
};
use letibot_tools::NullToolSink;
use letibot_tools::builtins::task::{TaskSpec, WorktreePlacement};
use letibot_transcript::{ToolCall, TranscriptItem, UserPart};
use letibot_turn::Endpoint;

mod support;

use support::canned;
use support::canned::Frame;
// The descriptors a binary's parallel tests need: the soft open-file limit here is 256 and
// eighteen daemons and their children at once want more.
use support::qwen::{IM_END, THINK_CLOSE};
use support::room;
use support::stub::{read_request, write_props};

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
    cfg.vocab_gguf = support::vocab::gguf();
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
            vec!["commit", "-q", "--allow-empty", "-m", "base"],
        ] {
            let ok = letibot_tools::testing::git(&repo)
                .args(&args)
                .status()
                .expect("git runs")
                .success();
            assert!(ok, "git {args:?}");
        }
        letibot_tools::testing::repo_config(&repo);
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
        let _ = letibot_tools::testing::git(&self.repo())
            .args(["worktree", "prune"])
            .status();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// One daemon: a registry with the parent session adopted, and the parent's harness.
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
        later_answer: None,
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

/// A child mid-turn when the daemon stops is resumed when it starts again - same session
/// same worktree, the restart notice in its own transcript - and its answer reaches the parent
/// through the parent's watcher, exactly as a fresh child's does.
#[test]
fn a_child_running_when_the_daemon_stopped_resumes_and_its_answer_reaches_the_parent() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    // Eighteen daemons and their children in parallel want more than a shell's 256 open files.
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
    // The child is named after its worktree, and the name is recorded and listed - the slug
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
    // And the time, with the list - the return time and the stop are in every restart notice
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
    // The resumed child is checked as a fresh one is: its turn was about to end with the
    // tree dirty, so it was told once, and the parent's answer carries the salvage line.
    assert!(
        user_rows(&child)
            .iter()
            .any(|r| r.starts_with("[the deliverable]") && r.contains("half-done.txt")),
        "the resumed child was not held to its branch: {:?}",
        user_rows(&child)
    );
    // And after the restart the child is still known by its name: the new daemon's list
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

/// A child that had answered and was mid-way through a LATER turn when the daemon stopped
/// is resumed too - its row read `done` from the first answer, so the next daemon left it
/// and every door then disagreed about what had become of it.
#[test]
fn a_child_stopped_in_a_later_turn_is_resumed_not_left_as_finished() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    // Eighteen daemons and their children in parallel want more than a shell's 256 open files.
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

/// A later turn that ends on its own puts the row back - the child is between turns again
/// its first answer delivered, and a restart now has nothing to resume or re-deliver.
#[test]
fn a_later_turn_that_ends_on_its_own_leaves_the_row_settled() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    // Eighteen daemons and their children in parallel want more than a shell's 256 open files.
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

/// A tool's helpers are never the model's news, and a recorded child reads one way through
/// every door - twelve part readers, settled and never marked delivered (their tool read
/// them), all derived the one name
#[test]
fn helpers_are_not_restored_and_a_recorded_child_reads_one_way() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    // Eighteen daemons and their children in parallel want more than a shell's 256 open files.
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
    // By handle and by name, its recorded answer - the sentence `task_resume` points at.
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

/// A child whose record was never written is still this session's child - listed, readable
/// and refused by name when nothing can bring it back.
///
/// A `subagent_run` row can be lost (the store was locked when the spawn wrote it) and the child goes
/// on working, so the daemon's own log is the only place that says so
///
/// ```text
///   s-…-sub-1793488007308855461 · not recorded (sqlite: database is locked); it runs, and a
///   daemon restart will not bring it back
/// ```
///
/// Its own session row is the second witness, and this is the test that reads it: the store is
/// staged as a lost spawn leaves it (a child session, no `subagent_run` row, a turn that never
/// ended), the parent is opened, and every door a model has is asked about the child.
///
/// And one child from before the store recorded any, which is the other class and not a loss at
/// all: a child older than the store's first record must be ABSENT from the list and must be told
/// *this child is from before this store recorded any child* rather than the refused-write sentence.
#[test]
fn a_child_whose_record_was_never_written_is_still_listed_and_readable() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let Some(_) = room::room_for("a daemon and its children", 1024) else {
        return;
    };
    let dir = Scratch::new("restart-lost-child");
    let parts = parts_for(&config(
        &dir.repo(),
        &dir.store(),
        Endpoint::new("127.0.0.1", 1),
    ));
    let stub = Stub::new(plain(&parts.vocab, "noted"), 0);
    let cfg = config(&dir.repo(), &dir.store(), stub.endpoint.clone());
    // One recorded child, so the store has a window - it began recording children a minute ago
    // and a reader that ignored the window would list every child this store ever had. The
    // daemon that loses it, and then a restart
    let one = a_daemon(&parts, &cfg);
    one.stop(None);
    let store = Store::open(&dir.store()).unwrap();
    let mut kept = run(
        "s-restart-parent-sub-kept",
        PARENT,
        "done",
        &TaskSpec {
            slug: Some("kept".into()),
            ..TaskSpec::default()
        },
    );
    kept.outcome = Some("kept is in".into());
    kept.delivered = true;
    // The window is a real instant in this store's life. A child created inside the recorded
    // minute had a row due and one created before it never did - the only staging in which a
    // windowed reader and an unwindowed one answer differently.
    let began = letibot_sessionlog::registry::now_ms() - 60_000;
    kept.started_ms = began;
    store.put_subagent_run(&kept).unwrap();
    // And the child the store refused the row for: its session, and nothing else.
    let lost = "s-restart-parent-sub-lost";
    store
        .put_session(&letibot_tokencore::store::SessionRecord {
            id: lost.into(),
            title: Some("the-notices-stay-collapsed".into()),
            model_id: "qwen".into(),
            dialect_sha: "d".into(),
            workspace_root: "/wt/agent-the-notices-stay-collapsed".into(),
            owner: "deadtrickster".into(),
            role: Some("coder".into()),
            approvers: vec![],
            parent_session_id: Some(PARENT.into()),
        })
        .unwrap();
    assert!(
        store.subagent_run(lost).unwrap().is_none(),
        "the state this test is about: a child with no record"
    );
    // And a child from BEFORE this store recorded any child - the other class, and not a loss
    // it ran, it settled, and no row of it was ever due.
    let old = "s-restart-parent-sub-old";
    store
        .put_session(&letibot_tokencore::store::SessionRecord {
            id: old.into(),
            title: Some("the old sweep".into()),
            model_id: "qwen".into(),
            dialect_sha: "d".into(),
            workspace_root: "/wt/agent-old".into(),
            owner: "deadtrickster".into(),
            role: Some("coder".into()),
            approvers: vec![],
            parent_session_id: Some(PARENT.into()),
        })
        .unwrap();
    // `put_session` stamps `created_at` with the clock, so the one row this test has to move is
    // the child's own: a minute before the store began recording children.
    store
        .connection()
        .execute(
            "UPDATE session SET created_at = ?1 WHERE id = ?2",
            (began as i64 - 60_000, old),
        )
        .unwrap();
    // Two more children with no record, and each stopped somewhere different. The LIST printed
    // one sentence for every child that did not end on an answer, so a parent could not tell a
    // child that died mid-`bash` from one that never opened a session.
    let prefix_id = store
        .put_stable_prefix(&letibot_tokencore::store::StablePrefixRecord {
            dialect_sha: "dd".repeat(32),
            system: "you are a harness".into(),
            tools_json: vec![],
            h_init: letibot_tokencore::hash_tokens(&[1, 2, 3, 4]),
            tokens: vec![1, 2, 3, 4],
            vocab_source: "/models/x.gguf".into(),
        })
        .unwrap();
    let a_child_that_stopped_at = |id: &str, title: &str, last: TranscriptItem| {
        store
            .put_session(&letibot_tokencore::store::SessionRecord {
                id: id.into(),
                title: Some(title.into()),
                model_id: "qwen".into(),
                dialect_sha: "dd".repeat(32),
                workspace_root: format!("/wt/{id}"),
                owner: "deadtrickster".into(),
                role: Some("coder".into()),
                approvers: vec![],
                parent_session_id: Some(PARENT.into()),
            })
            .unwrap();
        let transcript = format!("t-{id}");
        store.put_transcript(&transcript, id, &prefix_id).unwrap();
        store
            .append_item(
                &transcript,
                0,
                &last,
                &letibot_tokencore::LedgerRow {
                    item_id: "a".into(),
                    tok_offset: 4,
                    tok_len: 1,
                    h_k: [1; 32],
                },
                &[9],
            )
            .unwrap();
    };
    // The one that died with a call's result in hand - the turn went on from there and never came
    // back.
    let mid = "s-restart-parent-sub-mid";
    a_child_that_stopped_at(
        mid,
        "the mid-bash child",
        TranscriptItem::ToolResult {
            call_id: "c1".into(),
            name: "bash".into(),
            outcome: letibot_transcript::ToolOutcome::Ok,
            payload: "ok".into(),
            edit: None,
            origin: None,
            media: None,
        },
    );
    // And the one whose last row IS an answer - the case the review was about: its text is what it
    // had written when it stopped, and nothing says its turn ended.
    let got_an_answer = "s-restart-parent-sub-answer";
    a_child_that_stopped_at(
        got_an_answer,
        "the child that answered",
        TranscriptItem::Assistant {
            text: "the sweep is in".into(),
            tool_calls: vec![],
            truncated: false,
        },
    );
    // And one from BEFORE the store recorded any child, whose last row is an answer. It is not in
    // the list (a row was never due for it) and a model may still name it, so the caveat must come
    // from the keyed read rather than the list - or that whole class is handed over bare.
    let old_answer = "s-restart-parent-sub-old-answer";
    a_child_that_stopped_at(
        old_answer,
        "the old child that answered",
        TranscriptItem::Assistant {
            text: "the old sweep is in".into(),
            tool_calls: vec![],
            truncated: false,
        },
    );
    store
        .connection()
        .execute(
            "UPDATE session SET created_at = ?1 WHERE id = ?2",
            (began as i64 - 60_000, old_answer),
        )
        .unwrap();

    let d = a_daemon(&parts, &cfg);
    // Listed - by its handle, marked as a child with no record rather than silently missing.
    let listed = d.call("task_result", serde_json::json!({})).payload;
    eprintln!("--- what task_result lists ---\n{listed}");
    assert!(
        listed.contains(lost),
        "a child the daemon lost is not absent from the list: {listed}"
    );
    assert!(
        listed.contains(&format!("{lost} — gone with no answer")),
        "and it says what it is: {listed}"
    );
    assert!(
        listed.contains("s-restart-parent-sub-kept") && listed.contains("answered"),
        "the recorded child reads as it always did: {listed}"
    );
    // HOW FAR EACH GOT - three children with no record, three different classes, and each row
    // says which: a parent reading the list can tell a child that died after `bash`'s result from
    // one that never opened a session, which one fixed sentence for both could not.
    assert!(
        listed.contains(&format!(
            "{mid} — gone with no answer — it stopped after `bash`'s result"
        )),
        "the child that died mid-turn says where it was: {listed}"
    );
    assert!(
        listed.contains(&format!(
            "{lost} — gone with no answer — it stopped before it wrote a single row"
        )),
        "and the one that never opened says that: {listed}"
    );
    assert!(
        listed.contains(&format!(
            "{got_an_answer} — answered, read from its own session — whatever it had written when \
             it stopped, not a finished answer"
        )),
        "and the one whose last row is an answer is named for what that is: {listed}"
    );
    // And no row names a cause the list does not know. Why a record is missing is the store's
    // own class (`no_record_cause`), and the list is handed only the fact that it is missing.
    assert!(
        !listed.contains("refused"),
        "a cause nobody measured reached the list: {listed}"
    );
    // And the answer to a child whose last row is an answer is NOT an answer - nothing
    // recorded its settlement, and during a shutdown an interrupted turn comes back as
    // whatever the child had written when it stopped.
    let not_an_answer = task_result(&d, got_an_answer);
    eprintln!(
        "--- what task_result says about the child that answered ---\n{}",
        not_an_answer.payload
    );
    assert!(
        not_an_answer
            .payload
            .starts_with("whatever it had written when it stopped, not a finished answer"),
        "the caveat leads, so nothing cuts it off: {}",
        not_an_answer.payload
    );
    assert!(
        not_an_answer
            .payload
            .contains("nothing says its turn ended"),
        "{}",
        not_an_answer.payload
    );
    assert!(
        not_an_answer.payload.contains("the sweep is in"),
        "the text is still there, under the caveat: {}",
        not_an_answer.payload
    );
    assert!(
        !not_an_answer.payload.contains("refused"),
        "its record was due and went missing — but this door is not the one that says why: {}",
        not_an_answer.payload
    );

    // The window, and this is the assertion an unwindowed reader fails - a child older than
    // the store's first record was never due one, so the list does not carry it. A reader that
    // dropped the window lists it here, which is the wall of rows this class used to be.
    assert!(
        !listed.contains(old),
        "a child from before the store recorded any was never due a row: {listed}"
    );
    assert!(
        !listed.contains(old_answer),
        "and the same window holds for the one that answered: {listed}"
    );
    // The caveat does not depend on the list. A child the list does not carry, named by
    // handle, still gets it - which is the only way the 517-of-573 class is covered.
    let old_not_an_answer = task_result(&d, old_answer);
    eprintln!(
        "--- what task_result says about the old child that answered ---\n{}",
        old_not_an_answer.payload
    );
    assert!(
        old_not_an_answer
            .payload
            .starts_with("whatever it had written when it stopped, not a finished answer"),
        "a child the list does not carry is not handed over bare: {}",
        old_not_an_answer.payload
    );
    assert!(
        old_not_an_answer.payload.contains("the old sweep is in"),
        "{}",
        old_not_an_answer.payload
    );

    // Readable - the single-handle answer says it is gone, where its work is, and how to read
    // it, and does not pretend to resume what has no description to resume from.
    let r = task_result(&d, lost);
    eprintln!("--- what task_result says about it ---\n{}", r.payload);
    assert!(
        r.payload.contains("no record of it was ever written"),
        "{}",
        r.payload
    );
    assert!(
        r.payload.contains("/wt/agent-the-notices-stay-collapsed"),
        "{}",
        r.payload
    );
    assert!(
        r.payload
            .contains(&format!("`transcript` with session=\"{lost}\"")),
        "{}",
        r.payload
    );
    // And what it was asked, which is the only thing left that says WHICH child this was
    // a lost child was never given a name anything wrote down, so its brief's first line is how
    // a parent recognises it.
    assert!(
        r.payload
            .contains("it was asked: the-notices-stay-collapsed"),
        "{}",
        r.payload
    );
    // Its handle is the only name it answers to: a name it does not have is refused rather than
    // resolving to a child nobody named.
    let by_name = task_result(&d, "a-name-nobody-gave-it");
    assert!(
        matches!(
            by_name.outcome,
            letibot_transcript::ToolOutcome::Failed { .. }
        ),
        "{}",
        by_name.payload
    );
    // And `job_kill` does not call it a stranger either - it is in the list above, so *no
    // subagent in this session* would contradict the list.
    let stopped = d.call("job_kill", serde_json::json!({"job": lost}));
    assert!(
        stopped.payload.contains("nothing is running"),
        "{}",
        stopped.payload
    );
    // And `task_resume` says why it cannot come back rather than calling it a stranger.
    let resumed = d.call("task_resume", serde_json::json!({"task": lost}));
    assert!(
        resumed
            .payload
            .contains("there is no record of it to resume from"),
        "{}",
        resumed.payload
    );
    assert!(
        resumed
            .payload
            .contains("the store refused the row its spawn writes"),
        "its record was due and went missing, so the cause is the write: {}",
        resumed.payload
    );

    // The child older than the record, through the two doors that answer by id - it is not in
    // the list, and `task_result` and `task_resume` still answer for it because the model
    // named it, saying which class it is in and never a cause nobody measured.
    let before = task_result(&d, old);
    eprintln!(
        "--- what task_result says about the older child ---\n{}",
        before.payload
    );
    assert!(
        before
            .payload
            .contains("this child is from before this store recorded any child"),
        "{}",
        before.payload
    );
    assert!(
        !before.payload.contains("refused the row"),
        "no write of it was refused, so no sentence may say one was: {}",
        before.payload
    );
    assert!(
        before.payload.contains("/wt/agent-old"),
        "it still says where the child worked: {}",
        before.payload
    );
    let before_resume = d.call("task_resume", serde_json::json!({"task": old}));
    assert!(
        before_resume
            .payload
            .contains("this child is from before this store recorded any child"),
        "{}",
        before_resume.payload
    );
    assert!(
        !before_resume.payload.contains("refused the row"),
        "{}",
        before_resume.payload
    );
    d.stop(None);
}

/// A job the old daemon's orderly stop ended is reminded - a job killed by
/// `end_running("the daemon stopping")`: after a clean restart no job row reads `running`, so the
/// reminder was never given.
#[test]
fn a_job_the_orderly_stop_ended_is_reminded() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    // Eighteen daemons and their children in parallel want more than a shell's 256 open files.
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

/// A relic draws no REMINDER - a job row left by a build before `job_context`, naming no
/// daemon so `Stopped::latest` over no daemon is unknown. Such a row is closed quietly and
/// named to nobody. The restart itself is still said
#[test]
fn relics_alone_draw_no_reminder_and_are_closed_quietly() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    // Eighteen daemons and their children in parallel want more than a shell's 256 open files.
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
        // The daemon that really stopped did so a moment ago, in order - and left nothing here.
        old_daemon_stopped(&store, Duration::from_secs(2));
        for h in ["j900", "j610", "j624", "j2437"] {
            a_relic(&store, h);
        }
    }
    let mut d = a_daemon(&parts, &cfg);
    let notice = d.parent.restore_after_restart();
    // The restart is said, and nothing is listed: no relic is named, no reminder is drawn, and
    // the sentence is the empty one - the relics name no daemon, so the stop is unknown.
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
    assert!(
        warnings(&d.hub(), "restart_nothing").is_empty(),
        "one line, and it is the row"
    );
    assert!(
        user_rows(&d.hub())
            .iter()
            .any(|r| r.contains("nothing was running")),
        "the row says it: {:?}",
        user_rows(&d.hub())
    );
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

/// Beside a real record, a relic neither dates the stop nor is reminded - the down time is
/// the real record's daemon's, under the threshold, and only the real job is in the notice.
#[test]
fn a_relic_beside_a_real_record_takes_the_down_time_from_the_real_one() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    // Eighteen daemons and their children in parallel want more than a shell's 256 open files.
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

/// Past the threshold nothing comes back on its own - the model is told how long the daemon
/// was down and given the one call per child - and `task_resume` brings the child back in that
/// one call.
#[test]
fn past_the_threshold_a_child_is_only_reminded_and_task_resume_brings_it_back() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    // Eighteen daemons and their children in parallel want more than a shell's 256 open files.
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

/// The restore threshold is measured to the OPEN, not to the daemon's return. The harm the
/// threshold prevents is a child resumed on a tree that moved while nobody was looking.
#[test]
fn a_session_opened_late_is_measured_from_the_open_and_only_reminded() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let Some(_) = room::room_for("eighteen daemons and their children in parallel", 1024) else {
        return;
    };
    let dir = Scratch::new("restart-opened-late");
    let parts = parts_for(&config(
        &dir.repo(),
        &dir.store(),
        Endpoint::new("127.0.0.1", 1),
    ));
    let stub = Stub::new(plain(&parts.vocab, ANSWER), 0);
    let mut cfg = config(&dir.repo(), &dir.store(), stub.endpoint.clone());
    cfg.restore_within = Duration::from_millis(1);
    let store = Store::open(&dir.store()).unwrap();

    // The daemon first, because the stop is written AT its start: the fixture's whole point
    // is that the daemon's own downtime says nothing about how long the SESSION was away.
    let mut d = a_daemon(&parts, &cfg);
    let started = d.registry.started_ms();
    store
        .life_started(OLD, started.saturating_sub(60_000))
        .unwrap();
    store.life_stopped(OLD, started).unwrap();
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
    // The open is a moment after the stop, and past the one-millisecond threshold.
    std::thread::sleep(Duration::from_millis(20));

    let notice = d.parent.restore_after_restart().expect("a reminder");
    eprintln!("--- what the parent's model is told ---\n{notice}");
    assert!(
        notice.contains("NOTHING was restored automatically"),
        "a session opened past the threshold is reminded, not resumed: {notice}"
    );
    assert!(
        notice.contains(r#"call `task_resume {"task": "s-restart-parent-sub-a"}`"#),
        "{notice}"
    );
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        d.registry.get("s-restart-parent-sub-a").is_none(),
        "the child is not resumed: it is a tree nobody has looked at since the stop"
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
    d.stop(None);
}

/// A child of a child comes back with its parent - the root resumes the child, and the
/// child's own open resumes the grandchild. The child died while it was still opening, so it has
/// no transcript: it is asked its task again, rather than told to carry on from nothing.
#[test]
fn a_grandchild_comes_back_when_its_parent_does() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    // Eighteen daemons and their children in parallel want more than a shell's 256 open files.
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

/// The edge cases a restore must not get wrong, each named in the notice
#[test]
fn what_must_not_be_respawned_is_reported_or_delivered() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    // Eighteen daemons and their children in parallel want more than a shell's 256 open files.
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

/// The parent's session row, as the old daemon left it - written by opening the parent once in a
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
    // A reminder the model hung on a scope of its own - the soak's `lubuntu2-soak`.
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

/// A restart with nothing running is SAID - once, through the daemon's own open - and it
/// starts no turn. A restore is work to continue and keeps its turn; a restart with nothing
/// to restore is a NOTICE a transcript row the model reads when it next reads.
#[test]
fn an_empty_restart_is_said_once_and_starts_no_turn() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    // Eighteen daemons and their children in parallel want more than a shell's 256 open files.
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
    // Nothing was running, and the session still names the daemon that held it: a monitor that
    // had already ended. That is where the down time is read from - a session whose rows name no
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
    // The whole sentence, not a fragment: the return time from the daemon's own clock, the down
    // time and the stop it is counted from, and that nothing was running.
    let sentence = format!(
        "[restart] the daemon came back at {} (down {}, stopped at {}): nothing was running",
        letibot_harnessd::subagent_record::utc(back),
        letibot_harnessd::restart::span(Duration::from_millis(back.saturating_sub(stopped))),
        letibot_harnessd::subagent_record::utc(stopped)
    );
    // One line, and it is the row. The sentence used to be published as a card *and* written as a
    // row, which drew one restart twice; the row is the copy both readers can use, so the card is
    // gone.
    assert!(
        warnings(&hub, "restart_nothing").is_empty(),
        "no card: the row is the line"
    );
    assert!(
        user_rows(&hub).iter().any(|r| r == &sentence),
        "the whole sentence, as a row: {:?}",
        user_rows(&hub)
    );
    assert!(warnings(&hub, "restart_restored").is_empty());
    assert!(warnings(&hub, "restart_reminded").is_empty());

    // The open rang the session - that ring is the daemon's own, and it carries everything a
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
    // And the model was told anyway, as a row it reads at its next turn.
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
    // Once. A second wake finds nothing, and the sentence is not said again.
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

/// A session restored with a plan it still owes is WOKEN, and its first turn carries the
/// restart sentence - a restart notice that arrives with the person's own `continue` is not
/// a restart. The whole chain, through production doors only
#[test]
fn a_restored_session_with_an_open_todo_row_is_woken_and_its_turn_carries_the_restart() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    // Eighteen daemons and their children in parallel want more than a shell's 256 open files.
    let Some(_) = room::room_for("eighteen daemons and their children in parallel", 1024) else {
        return;
    };
    let dir = Scratch::new("restart-owed-plan");
    let parts = parts_for(&config(
        &dir.repo(),
        &dir.store(),
        Endpoint::new("127.0.0.1", 1),
    ));
    let stub = Stub::new(plain(&parts.vocab, "carried on"), 0);
    let cfg = config(&dir.repo(), &dir.store(), stub.endpoint.clone());
    seed_session(&parts, &cfg);
    let store = Store::open(&dir.store()).unwrap();
    old_daemon_stopped(&store, Duration::from_secs(1));
    // The plan the model left on the board - work it wrote down and never finished, which is
    // the durable half of *"after restart i owe you"*.
    store
        .put_todos(
            PARENT,
            &[TodoItem {
                content: "finish the migration and report".into(),
                status: TodoStatus::Pending,
                by: TodoBy::Model,
                when: None,
                needs: Vec::new(),
            }],
        )
        .unwrap();
    // And nothing was running, with the session still naming the daemon that held it - a
    // monitor that had already ended, which is where the down time is read from.
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

    let registry = Registry::new();
    registry
        .create(PARENT, "the parent", wiring(&cfg))
        .expect("created");
    let mut sessions =
        Sessions::open_first(&parts, cfg.clone(), registry.clone()).expect("the daemon opens");
    let hub = registry.get(PARENT).unwrap();
    // The open rang the session, which is the only door a between-turns fact has after a
    // restart - a watcher or a head's command would both be something that ran before it.
    let woke = loop {
        match registry.next_work_until(Some(Instant::now() + Duration::from_secs(5))) {
            WorkOrIdle::Work(Work::Woken(id)) => break id,
            WorkOrIdle::Work(_) => continue,
            _ => panic!("the opened session was not rung"),
        }
    };
    assert_eq!(woke, PARENT);
    // The wake is a turn, and the turn's opening line is the restart's own sentence.
    assert!(
        !matches!(sessions.wake(PARENT), Outcome::Ignored),
        "a restored session that owes work was not woken"
    );
    assert_eq!(stub.arrived(), 1, "one model call, for the work it owes");
    let rows = user_rows(&hub);
    let restart = rows
        .iter()
        .find(|r| r.starts_with("[restart] "))
        .unwrap_or_else(|| panic!("the restart sentence never reached the turn: {rows:?}"));
    assert!(restart.contains("nothing was running"), "{restart}");
    assert!(
        restart.contains("a todo row still owed"),
        "the sentence says why it is a turn: {restart}"
    );
    // Once per restart, not once per pass. A second wake is the worker's next pass over the
    // same restart: it finds nothing, and neither the sentence nor the row is said again.
    assert!(matches!(sessions.wake(PARENT), Outcome::Ignored));
    assert_eq!(stub.arrived(), 1, "the restart was said twice");
    assert_eq!(
        user_rows(&hub)
            .iter()
            .filter(|r| r.starts_with("[restart]"))
            .count(),
        1,
        "{:?}",
        user_rows(&hub)
    );
    registry.close();
}

/// A session that was mid-turn when the daemon stopped is woken too - the arm a model that
/// restarts the daemon from inside a tool call is in, since it leaves its call and no
/// result.
#[test]
fn a_restored_session_that_was_mid_turn_is_woken() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let Some(_) = room::room_for("eighteen daemons and their children in parallel", 1024) else {
        return;
    };
    let dir = Scratch::new("restart-owed-midturn");
    let parts = parts_for(&config(
        &dir.repo(),
        &dir.store(),
        Endpoint::new("127.0.0.1", 1),
    ));
    let stub = Stub::new(plain(&parts.vocab, "carried on"), 0);
    let cfg = config(&dir.repo(), &dir.store(), stub.endpoint.clone());
    seed_session(&parts, &cfg);
    let store = Store::open(&dir.store()).unwrap();
    old_daemon_stopped(&store, Duration::from_secs(1));
    // The last row the dying daemon left: a call was issued and its result never written. The
    // transcript is the one the seeded daemon's own open made, so the prefix and the anchor are
    // its own and only the row is the fixture's.
    let transcript = store
        .current_transcript_id(PARENT)
        .unwrap()
        .expect("the seeded daemon wrote this session's transcript");
    let prefix_id = store
        .transcript_prefix_id(&transcript)
        .unwrap()
        .expect("a transcript has a prefix");
    let (prefix_tokens, h_init) = store.get_stable_prefix(&prefix_id).unwrap();
    let tokens: Vec<u32> = vec![7, 8, 9];
    store
        .append_item(
            &transcript,
            0,
            &TranscriptItem::Assistant {
                text: String::new(),
                tool_calls: vec![ToolCall {
                    id: "call_restart".into(),
                    name: "bash".into(),
                    arguments: r#"{"command":"letibot --stop"}"#.into(),
                }],
                truncated: false,
            },
            &letibot_tokencore::LedgerRow {
                item_id: format!("{transcript}.0"),
                tok_offset: prefix_tokens.len() as u32,
                tok_len: tokens.len() as u32,
                h_k: letibot_tokencore::ledger::chain(&h_init, &tokens),
            },
            &tokens,
        )
        .expect("the row is appended");

    let registry = Registry::new();
    registry
        .create(PARENT, "the parent", wiring(&cfg))
        .expect("created");
    let mut sessions =
        Sessions::open_first(&parts, cfg.clone(), registry.clone()).expect("the daemon opens");
    let hub = registry.get(PARENT).unwrap();
    let _ = loop {
        match registry.next_work_until(Some(Instant::now() + Duration::from_secs(5))) {
            WorkOrIdle::Work(Work::Woken(id)) => break id,
            WorkOrIdle::Work(_) => continue,
            _ => panic!("the opened session was not rung"),
        }
    };
    assert!(
        !matches!(sessions.wake(PARENT), Outcome::Ignored),
        "a session cut off mid-turn was not woken"
    );
    assert_eq!(stub.arrived(), 1);
    let rows = user_rows(&hub);
    let restart = rows
        .iter()
        .find(|r| r.starts_with("[restart] "))
        .unwrap_or_else(|| panic!("the restart sentence never reached the turn: {rows:?}"));
    assert!(
        restart.contains("a turn was in flight when the rows stopped"),
        "{restart}"
    );
    registry.close();
}

/// Under the threshold: monitors come back, a monitor that cannot is reported, and a job is
/// reminded with the call that runs it again - and the reminder starts the model's turn, through
/// the daemon's own open (`Sessions`), its ring, and its wake.
#[test]
fn monitors_are_rearmed_and_jobs_reminded_and_the_reminder_is_a_turn() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    // Eighteen daemons and their children in parallel want more than a shell's 256 open files.
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

/// Over the threshold the same session is only reminded - no monitor is armed, and each is
/// given back as the call that declares it.
#[test]
fn past_the_threshold_monitors_and_jobs_are_only_reminded() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    // Eighteen daemons and their children in parallel want more than a shell's 256 open files.
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

/// Only a DAEMON stop triggers any of it. A session whose records were written by the daemon that
/// is running - however long it has sat idle, and however stale its heartbeat looks - is opened again
/// (a head attaching, a `/resume`) and nothing is restored, reminded or rung.
#[test]
fn a_session_reopened_by_the_daemon_that_wrote_its_records_restores_nothing() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    // Eighteen daemons and their children in parallel want more than a shell's 256 open files.
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
    // This daemon's own life, with a heartbeat that looks hours old - a long idle.
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
    // A SECOND harness for one session is REFUSED, by name - the writer claim. The daemon's
    // own door never builds one (`Sessions::open_as` answers a `/resume` or a head's attach
    // with the harness it already has)
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
    // And the reopen that really happens - the daemon's harness gone, its claim given back
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

/// A stop no record dates is UNKNOWN, and an unknown down time restores nothing. The old daemon
/// here kept no `daemon_life` row (a build before the lifeline); its monitor and its job name it, so
/// the stop is truly unknown - not merely undated by a relic.
#[test]
fn a_stop_no_record_dates_is_said_unknown_and_restores_nothing() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    // Eighteen daemons and their children in parallel want more than a shell's 256 open files.
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

/// Restored is said only when something was - under the threshold, a session whose only
/// record is a job (which is never re-run) was raised as `restart_restored`, *"restored where
/// it could be"*, over a list of reminders.
#[test]
fn under_the_threshold_a_session_with_only_a_job_is_reminded_not_restored() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    // Eighteen daemons and their children in parallel want more than a shell's 256 open files.
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
    // And the time, which is what a restart is for: the return time and the stop it is counted
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
    // Said once: the row now reads `lost at restart`, an ending, and it was said - so the next
    // open does not announce it again as an ending nobody was told about.
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

/// A job whose ending was held when the daemon stopped is said by the next one, once.
/// Restore used to collect only `running` rows
#[test]
fn a_job_that_ended_untold_before_the_stop_is_said_after_it_once() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    // Eighteen daemons and their children in parallel want more than a shell's 256 open files.
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

/// A job handle minted after a restart is above every one the store holds. The counter started at
/// `j1` in every daemon life, so a session's first job after a restart could be its own pre-restart
/// `j7` to every reader that matches by handle, and its row inherited the old job's command.
#[test]
fn a_job_handle_minted_after_a_restart_is_above_every_one_the_store_holds() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    // Eighteen daemons and their children in parallel want more than a shell's 256 open files.
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

/// A job still running when a VM child's backend is closed is named, and its row settled. A
/// VM child's backend is closed after its first turn and its background jobs die with the
/// guest
#[test]
fn the_jobs_a_closing_backend_takes_with_it_are_named_and_their_rows_settled() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    // Eighteen daemons and their children in parallel want more than a shell's 256 open files.
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
