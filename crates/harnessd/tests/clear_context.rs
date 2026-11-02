//! `/clear`: the conversation starts over and the session does not. A cleared session keeps
//! its board, its job and its child; a job or a child that settles after the clear lands in
//! the NEW transcript, and the old one stays whole in the store.

use std::net::TcpListener;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use letibot_harnessd::config::{Config, Seat};
use letibot_harnessd::harness::JobFate;
use letibot_harnessd::jobwatch::JobWatchSink;
use letibot_harnessd::{Dialect, Harness, Parts};
use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_tokencore::store::{Store, TodoBy, TodoItem, TodoStatus};
use letibot_tools::NullToolSink;
use letibot_transcript::{Speaker, ToolCall, TranscriptItem, UserPart};
use letibot_turn::Endpoint;

mod support;

use support::canned::Frame;
use support::qwen::a_plain_answer_turn;
use support::stub::{read_request, write_frames, write_props};

const PATIENCE: Duration = Duration::from_secs(30);

/// The child's brief - what the orientation must say it was asked.
const BRIEF: &str = "audit the parser's error paths";
/// The operator's sentence, given with the clear.
const NEXT: &str = "finish the parser, then run the gate";

fn config(session_id: &str, endpoint: Endpoint, store: &Path) -> Config {
    let mut cfg = Config::for_this_box("/tmp");
    cfg.dialect = Dialect::Qwen;
    // Unconfined with `bash` seated: the job here is a real process.
    cfg.seat = Seat::Leticode;
    cfg.allow_bash = true;
    cfg.endpoint = endpoint;
    cfg.http_retries = 0;
    // A child is planned against a window.
    cfg.context_window = Some(32_768);
    cfg.store = Some(store.to_path_buf());
    cfg.session_id = session_id.to_string();
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

/// A parent session with the REAL runner, its hub adopted into a registry as a daemon's is
/// the teardown drains the tree, or the subagent threads outlive the test.
struct Parent {
    harness: Harness,
    registry: Arc<Registry>,
    gate: Arc<Stub>,
}

fn a_parent(session_id: &str, parts: &Parts, cfg: &Config, gate: Arc<Stub>) -> Parent {
    let registry = Registry::new();
    let hub = registry.new_hub(session_id.to_string());
    registry
        .adopt(hub.clone(), session_id.to_string(), wiring(cfg), None)
        .expect("the parent is in the registry");
    let harness =
        Harness::open_with_registry(parts, cfg.clone(), hub, None, None, registry.clone())
            .expect("the parent opens");
    Parent {
        harness,
        registry,
        gate,
    }
}

impl Parent {
    /// One tool call through the session's own runtime, WATCHED - a turn's own sink, so a
    /// `Backgrounded` outcome is armed for delivery.
    fn call(&self, name: &str, args: serde_json::Value) -> letibot_tools::ToolResult {
        let rt = self.harness.runtime_handle();
        let call = ToolCall {
            id: format!("call_{name}"),
            name: name.to_string(),
            arguments: args.to_string(),
        };
        let mut sink = JobWatchSink::new(NullToolSink, self.harness.job_watchers());
        rt.invoke("t1", &call, &mut sink)
    }

    fn backgrounded(&self, name: &str, args: serde_json::Value) -> String {
        let r = self.call(name, args);
        match r.outcome {
            letibot_transcript::ToolOutcome::Backgrounded { handle, .. } => handle,
            other => panic!("`{name}` did not background: {other:?} — {}", r.payload),
        }
    }

    fn hub_of(&self, handle: &str) -> Arc<Hub> {
        let began = Instant::now();
        loop {
            if let Some(hub) = self.registry.get(handle) {
                return hub;
            }
            assert!(
                began.elapsed() < PATIENCE,
                "`{handle}` never entered the registry"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Parent {
    fn drop(&mut self) {
        self.gate.release(usize::MAX);
        self.registry.close();
        if !self.harness.wait_for_children(PATIENCE) && !std::thread::panicking() {
            panic!("the tree still had a subagent thread after {PATIENCE:?}");
        }
    }
}

/// The text of a user item, or `None` for any other kind.
fn user_text(item: &TranscriptItem) -> Option<(Speaker, String)> {
    match item {
        TranscriptItem::User { speaker, parts } => Some((
            *speaker,
            parts
                .iter()
                .filter_map(|p| match p {
                    UserPart::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        )),
        _ => None,
    }
}

/// Whether the model's answer has come back on this hub - the reply, not the prompt: a
/// settlement is owed when a child has FINISHED.
fn answered(hub: &Hub, needle: &str) -> bool {
    hub.retained().iter().any(|e| match &e.event {
        SessionEvent::TranscriptContent { item, .. } => {
            matches!(&**item, TranscriptItem::Assistant { text, .. } if text.contains(needle))
        }
        _ => false,
    })
}

fn warnings(hub: &Hub) -> Vec<(String, String)> {
    hub.retained()
        .iter()
        .filter_map(|e| match &e.event {
            SessionEvent::Warning { code, detail, .. } => Some((code.clone(), detail.clone())),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// THE ONE THIS FILE EXISTS FOR
// ---------------------------------------------------------------------------

#[test]
fn a_cleared_session_keeps_its_board_its_job_and_its_child() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let Some(_) = letibot_tokencore::apparatus::present(
        "a process-lifetime tree (cgroup v2; process groups on macOS)",
        letibot_tools::host_tree().is_ok(),
    ) else {
        return;
    };
    let dir = TempDir::new("harnessd-clear");
    let path = dir.path().join("sessions.db");
    let go = dir.path().join("go");
    let session_id = "s-clear-parent";

    let parts = parts_for(&config(session_id, Endpoint::new("127.0.0.1", 1), &path));
    let v = &parts.vocab;
    // Requests in the only order they can arrive: the parent's own turn, the child's first turn
    // (HELD, so the child is mid-turn at the clear), then the wakes that deliver the settlements.
    let scripts = vec![
        a_plain_answer_turn(
            v,
            "reading",
            "the old conversation's answer, full of drift",
            30,
        ),
        a_plain_answer_turn(v, "auditing", "the child's audit is done", 30),
        a_plain_answer_turn(v, "noting", "noted the settlement", 30),
        a_plain_answer_turn(v, "noting", "noted the settlement", 30),
        a_plain_answer_turn(v, "noting", "noted the settlement", 30),
    ];
    let stub = Arc::new(Stub::held(scripts, 5));
    stub.release(1);

    let cfg = config(session_id, stub.endpoint.clone(), &path);
    let mut parent = a_parent(session_id, &parts, &cfg, stub.clone());

    // A history worth clearing.
    parent
        .harness
        .submit("start on the parser")
        .expect("the first turn runs");
    let old_id = parent.harness.transcript_id().to_string();
    let old_items = parent.harness.items().len();
    assert!(old_items >= 2, "a prompt and its answer");

    // The board: one row of each author.
    parent.harness.set_operator_todos(
        vec![TodoItem {
            content: "review the parser before it lands".into(),
            status: TodoStatus::Pending,
            by: TodoBy::Operator,
            when: None,
            needs: Vec::new(),
        }],
        &[],
    );
    let r = parent.call(
        "todo_write",
        serde_json::json!({"todos": [
            {"content": "write the parser", "status": "in_progress"},
            {"content": "run the gate", "status": "pending"}
        ]}),
    );
    assert_eq!(
        r.outcome,
        letibot_transcript::ToolOutcome::Ok,
        "{}",
        r.payload
    );
    let board = parent.harness.todo_list();
    assert_eq!(board.len(), 3, "{board:#?}");

    // A background job that cannot finish until the test says so - so its settlement lands
    // after the clear, never before it.
    let job = parent.backgrounded(
        "bash",
        serde_json::json!({
            "command": format!(
                "while [ ! -f {} ]; do sleep 0.05; done; echo JOB-FINISHED",
                go.display()
            ),
            "background": true
        }),
    );
    // A child, mid-turn: its first request is held by the stub.
    let child = parent.backgrounded("task", serde_json::json!({"prompt": BRIEF}));
    let child_hub = parent.hub_of(&child);
    assert!(
        stub.wait_for_requests(2),
        "the child's turn reached the model"
    );

    // ---- THE CLEAR ----
    let r = parent
        .harness
        .clear_conversation(Some(NEXT))
        .expect("the clear lands");

    // (1) New and short: message zero plus the orientation, nothing else.
    assert_eq!(parent.harness.transcript_id(), format!("{session_id}#t1"));
    assert_eq!(r.fork.parent_id, old_id);
    let items = parent.harness.items().to_vec();
    assert_eq!(items.len(), 1, "message zero plus ONE item: {items:#?}");
    let (speaker, said) = user_text(&items[0]).expect("the orientation is a user item");
    assert_eq!(
        speaker,
        Speaker::Agent,
        "the harness speaks the orientation, never the operator"
    );
    assert_eq!(said, r.orientation);
    for must in [
        "[cleared]",
        old_id.as_str(),
        NEXT,
        "write the parser",
        "run the gate",
        "review the parser before it lands",
        job.as_str(),
        child.as_str(),
        BRIEF,
    ] {
        assert!(
            said.contains(must),
            "the orientation lacks `{must}`:\n{said}"
        );
    }
    assert!(
        !said.contains("drift"),
        "nothing of the old conversation's prose is carried:\n{said}"
    );
    eprintln!("--- the orientation ---\n{said}\n---");

    // (2) The board is identical.
    assert_eq!(parent.harness.todo_list(), board);

    // (5) The old transcript is still whole in the store.
    let store = Store::open(&path).expect("the store reopens");
    let old = store
        .load_transcript(&old_id)
        .expect("the old transcript loads");
    assert_eq!(old.items.len(), old_items, "kept whole");
    assert!(
        old.items
            .iter()
            .filter_map(|(item, _, _)| user_text(item))
            .any(|(_, t)| t.contains("start on the parser")),
        "with its history in it"
    );

    // (6) The count is the new transcript's, unmeasured.
    let resident = parent.harness.measure_resident();
    assert_eq!(resident.measured, None, "no measurement survives a clear");
    assert_eq!(
        resident.tokens,
        parent.harness.ledger_len() as u64,
        "the count is the new base, not the old conversation"
    );
    let row = store.session(session_id).unwrap().expect("the session row");
    assert_eq!(
        row.context_tokens, None,
        "the stored count was about the old conversation"
    );

    // ---- AFTER THE CLEAR ----
    // The child answers, the job finishes, and every wake after them is answered.
    stub.release(usize::MAX);
    std::fs::write(&go, b"").expect("the job's go file");

    // (3) and (4): both settlements reach the NEW conversation. A routine result is HELD -
    // nothing on this board waits on the job or the child - so it rides in with the next turn
    // the model takes anyway, as one `[meanwhile]` batch
    let began = Instant::now();
    let mut prompted = false;
    loop {
        let _ = parent.harness.wake().expect("the wake runs");
        if !prompted
            && matches!(parent.harness.job_fate(&job), JobFate::Ended { .. })
            && answered(&child_hub, "the child's audit is done")
        {
            parent
                .harness
                .submit("carry on")
                .expect("the prompt's turn runs");
            prompted = true;
        }
        let texts: Vec<String> = parent
            .harness
            .items()
            .iter()
            .filter_map(user_text)
            .map(|(_, t)| t)
            .collect();
        // Past the orientation, which names both handles itself: what is looked for is the
        // SETTLEMENT - the job's output and the child's answer, which nothing before the clear
        // could have known.
        let later = || texts.iter().skip(1);
        // The completion names the job and how it ended; its own command is not echoed.
        let job_in = later().any(|t| t.contains(&format!("`{job}` exited 0")));
        let child_in =
            later().any(|t| t.contains(&child) && t.contains("the child's audit is done"));
        if job_in && child_in {
            eprintln!(
                "--- delivered ---\n{}\n---",
                later().cloned().collect::<Vec<_>>().join("\n---\n")
            );
            break;
        }
        assert!(
            began.elapsed() < PATIENCE,
            "the settlements never reached the new conversation: {texts:#?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        parent.harness.transcript_id(),
        format!("{session_id}#t1"),
        "delivered into the cleared conversation, not a new one"
    );
    // The child is still this session's.
    assert_eq!(
        parent.registry.parent_of(&child).as_deref(),
        Some(session_id),
        "the child's parent is the session, not the transcript"
    );
    drop(child_hub);
}

/// A subagent's own `/clear` is answered by the thread that owns it - with its board kept
/// its transcript forked, and the boundary row on its own log; and the old one in the store.
#[test]
fn a_childs_clear_is_answered_by_its_own_thread() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-clear-child");
    let path = dir.path().join("sessions.db");
    let session_id = "s-clear-child-parent";
    let parts = parts_for(&config(session_id, Endpoint::new("127.0.0.1", 1), &path));
    let v = &parts.vocab;
    let stub = Arc::new(Stub::answering(
        vec![a_plain_answer_turn(v, "auditing", "audited", 30)],
        1,
    ));
    let cfg = config(session_id, stub.endpoint.clone(), &path);
    let parent = a_parent(session_id, &parts, &cfg, stub.clone());
    let child = parent.backgrounded("task", serde_json::json!({"prompt": BRIEF}));
    let hub = parent.hub_of(&child);
    // Between turns: its answer is in.
    let began = Instant::now();
    while parent
        .call("task_result", serde_json::json!({"task": child}))
        .outcome
        != letibot_transcript::ToolOutcome::Ok
    {
        assert!(began.elapsed() < PATIENCE, "the child never answered");
        std::thread::sleep(Duration::from_millis(20));
    }

    let head = hub.attach(
        "tui",
        "dead",
        letibot_sessionlog::protocol::Caps::default(),
        0,
    );
    hub.submit(
        &head.head_id,
        "r-clear",
        0,
        letibot_sessionlog::hub::CommandKind::Slash {
            line: "clear".into(),
        },
    );
    hub.wake_its_own_reader();

    let began = Instant::now();
    let cleared = loop {
        if let Some((_, d)) = warnings(&hub).into_iter().find(|(c, _)| c == "cleared") {
            break d;
        }
        assert!(
            began.elapsed() < PATIENCE,
            "the child never answered its clear: {:#?}",
            warnings(&hub)
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(cleared.contains(&format!("{child}#t1")), "{cleared}");
    assert!(
        !warnings(&hub)
            .iter()
            .any(|(c, _)| c == "child_door_refused"),
        "a clear is the one slash verb a child answers itself"
    );
    let store = Store::open(&path).expect("the store reopens");
    let fork = store
        .load_transcript(&format!("{child}#t1"))
        .expect("the child's new transcript");
    assert_eq!(fork.items.len(), 1, "message zero plus the orientation");
    let old = store
        .load_transcript(&format!("{child}#t0"))
        .expect("the child's old transcript");
    assert!(old.items.len() >= 2, "the brief and the answer, kept");
}

// ---------------------------------------------------------------------------
// The stub (`message_between_turns.rs`'s): a request held until released
// ---------------------------------------------------------------------------

/// A model that answers on a gate, and keeps every request it was asked. `canned::Canned`
/// answers too fast to stand in a mid-flight turn, so a request is read recorded and held
/// until the test releases it
struct Stub {
    endpoint: Endpoint,
    shared: Arc<Gate>,
    handle: Option<std::thread::JoinHandle<()>>,
}

struct Gate {
    inner: Mutex<Inner>,
    cv: Condvar,
}

#[derive(Default)]
struct Inner {
    /// Every request's body, in arrival order.
    bodies: Vec<String>,
    /// How many requests may be answered. A request waits until this passes its index.
    released: usize,
}

impl Stub {
    /// One frame list per request, answered as soon as the request arrives.
    fn answering(scripts: Vec<Vec<Frame>>, requests: usize) -> Stub {
        let stub = Stub::held(scripts, requests);
        stub.release(requests);
        stub
    }

    /// The same, and nothing is answered until [`Stub::release`] says so.
    fn held(scripts: Vec<Vec<Frame>>, requests: usize) -> Stub {
        let listener = TcpListener::bind("127.0.0.1:0").expect("an ephemeral port");
        let addr = listener.local_addr().unwrap();
        let shared = Arc::new(Gate {
            inner: Mutex::new(Inner::default()),
            cv: Condvar::new(),
        });
        let mine = shared.clone();
        let handle = std::thread::spawn(move || {
            let mut i = 0usize;
            while i < requests {
                // An accept that fails has nothing left to answer: the client's own error is
                // the report.
                let Ok((stream, _)) = listener.accept() else {
                    return;
                };
                let (line, body) = read_request(stream.try_clone().expect("clone the socket"));
                // `/props` is not a turn and does not consume a script. A spawn asks the
                // parent's endpoint for its window, one `GET` per child - counting it as a turn
                // would shift every script by one.
                if line.starts_with("GET ") {
                    let _ = write_props(stream);
                    continue;
                }
                {
                    let mut g = mine.inner.lock().unwrap();
                    g.bodies.push(body);
                    mine.cv.notify_all();
                    while g.released <= i {
                        g = mine.cv.wait(g).unwrap();
                    }
                }
                let _ = write_frames(stream, &scripts[i % scripts.len()]);
                i += 1;
            }
        });
        Stub {
            endpoint: Endpoint::new(addr.ip().to_string(), addr.port()),
            shared,
            handle: Some(handle),
        }
    }

    /// Wait until at least `n` requests have arrived. `false` is the timeout, so a caller
    /// can say what it was waiting for rather than hang.
    fn wait_for_requests(&self, n: usize) -> bool {
        let began = Instant::now();
        let mut g = self.shared.inner.lock().unwrap();
        while g.bodies.len() < n {
            if began.elapsed() > PATIENCE {
                return false;
            }
            let (guard, _) = self
                .shared
                .cv
                .wait_timeout(g, Duration::from_millis(20))
                .unwrap();
            g = guard;
        }
        true
    }

    /// Let the first `n` requests be answered.
    fn release(&self, n: usize) {
        let mut g = self.shared.inner.lock().unwrap();
        g.released = g.released.max(n);
        self.shared.cv.notify_all();
    }
}

impl Drop for Stub {
    fn drop(&mut self) {
        // Release anything still held, then detach: a test that failed early must not leave
        // a thread parked on the gate for the rest of the suite.
        self.release(usize::MAX);
        if let Some(h) = self.handle.take() {
            drop(h);
        }
    }
}

/// A directory that removes itself, because a test that leaks a store per run eventually
/// fills the disk and the run that finds out is not this one.
struct TempDir {
    path: std::path::PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = letibot_sessionlog::testing::scratch_root().join(format!(
            "{tag}-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock before epoch")
                .as_nanos(),
            {
                static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            }
        ));
        std::fs::create_dir_all(&path).expect("creating the temp dir");
        Self { path }
    }

    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
