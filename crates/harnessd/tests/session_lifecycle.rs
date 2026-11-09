//! One owner per session, and one lifecycle with an explicit close.

use std::net::TcpListener;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use letibot_harnessd::config::{Config, Seat};
use letibot_harnessd::sessions::{Outcome, Sessions};
use letibot_harnessd::{Dialect, Parts};
use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::hub::{CommandKind, Hub};
use letibot_sessionlog::protocol::Caps;
use letibot_sessionlog::registry::{Registry, SessionWiring, Work, WorkOrIdle};
use letibot_tools::NullToolSink;
use letibot_transcript::{Speaker, ToolCall, TranscriptItem, UserPart};
use letibot_turn::Endpoint;

mod support;

use support::canned::Frame;
// The descriptors a binary's parallel tests need - see `room`'s own note: the soft open-file
// limit on this box is small and fifty concurrent subagents want more than it.
use support::qwen::a_plain_answer_turn;
use support::room;
use support::stub::{read_request, write_frames, write_props};

const PATIENCE: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// Fixtures - `message_between_turns.rs`'s shapes, kept to what this file needs
// ---------------------------------------------------------------------------

fn config(session_id: &str, endpoint: Endpoint, store: &Path) -> Config {
    let mut cfg = Config::for_this_box("/tmp");
    cfg.dialect = Dialect::Qwen;
    // `leticode`, the unconfined seat with the delegation trio (`task`, `task_result`
    // `task_message`) - and not `coder`, which has the trio but is a CONFINED seat
    cfg.seat = Seat::Leticode;
    cfg.endpoint = endpoint;
    cfg.http_retries = 0;
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

/// A model that answers every request at once with the next script (cycling), and answers a spawn's
/// `GET /props` with a window without consuming a script.
struct Stub {
    endpoint: Endpoint,
    turns: Arc<std::sync::atomic::AtomicUsize>,
}

impl Stub {
    fn answering(scripts: Vec<Vec<Frame>>) -> Stub {
        let listener = TcpListener::bind("127.0.0.1:0").expect("an ephemeral port");
        let addr = listener.local_addr().unwrap();
        let turns = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = turns.clone();
        std::thread::spawn(move || {
            let mut i = 0usize;
            while let Ok((stream, _)) = listener.accept() {
                let (line, _body) = read_request(stream.try_clone().expect("clone the socket"));
                if line.starts_with("GET ") {
                    let _ = write_props(stream);
                    continue;
                }
                counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let _ = write_frames(stream, &scripts[i % scripts.len()]);
                i += 1;
            }
        });
        Stub {
            endpoint: Endpoint::new(addr.ip().to_string(), addr.port()),
            turns,
        }
    }

    fn turns(&self) -> usize {
        self.turns.load(std::sync::atomic::Ordering::SeqCst)
    }
}

struct TempDir {
    path: std::path::PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
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
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// The root's own runtime, invoking one tool as a model's call would.
fn call(
    sessions: &Sessions<'_>,
    root: &str,
    name: &str,
    args: serde_json::Value,
) -> letibot_tools::ToolResult {
    let call = ToolCall {
        id: format!("call_{name}"),
        name: name.to_string(),
        arguments: args.to_string(),
    };
    sessions
        .harness_of(root)
        .expect("the root is open")
        .runtime_handle()
        .invoke("t1", &call, &mut NullToolSink)
}

fn spawn(sessions: &Sessions<'_>, root: &str, prompt: &str) -> String {
    let r = call(
        sessions,
        root,
        "task",
        serde_json::json!({"prompt": prompt}),
    );
    match r.outcome {
        letibot_transcript::ToolOutcome::Backgrounded { handle, .. } => handle,
        other => panic!("`task` did not start a child: {other:?} — {}", r.payload),
    }
}

fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
    let began = Instant::now();
    while !f() {
        if began.elapsed() > PATIENCE {
            panic!("waited {PATIENCE:?} for {what}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Every operator row on a session's log.
fn operator_rows(hub: &Hub) -> Vec<String> {
    hub.retained()
        .iter()
        .filter_map(|e| match &e.event {
            SessionEvent::TranscriptContent { item, .. }
            | SessionEvent::TranscriptAppended {
                item: Some(item), ..
            } => match &**item {
                TranscriptItem::User { speaker, parts } if *speaker == Speaker::Operator => Some(
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

/// Threads in this process whose name starts with `prefix` - `memwatch`'s own census (the line the
/// daemon's watchdog prints), so the test counts what an operator would read.
fn threads_named(prefix: &str) -> usize {
    letibot_harnessd::memwatch::thread_names()
        .into_iter()
        .filter(|(name, _)| name.starts_with(prefix))
        .map(|(_, n)| n)
        .sum()
}

/// The process's resident set, in KiB - for the record the test prints, not for an assertion.
fn rss_kib() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("VmRSS:"))
                .and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok())
        })
        .unwrap_or(0)
}

/// One test at a time in this binary: the thread census is the PROCESS's, and a sibling test
/// spawning children in parallel would be counted as this one's leak.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// The worker's own loop, for one slice of time - `Daemon::run`'s arms that matter here. Answers
/// the commands it took, by session, so a test can say which sessions the worker served.
fn drive_worker(
    sessions: &mut Sessions<'_>,
    registry: &Registry,
    for_: Duration,
) -> Vec<(String, Outcome)> {
    let mut took = Vec::new();
    let until = Instant::now() + for_;
    while Instant::now() < until {
        let at = Instant::now() + Duration::from_millis(20);
        let deadline = sessions.next_idle_at().map_or(at, |d| d.min(at));
        match registry.next_work_until(Some(deadline)) {
            WorkOrIdle::Idle => {
                sessions.sweep_abandoned_calls();
                sessions.deliver_due_nags();
                sessions.park_idle_children();
            }
            WorkOrIdle::Closed => break,
            WorkOrIdle::Work(Work::Open(id)) => {
                let _ = sessions.open(&id);
            }
            WorkOrIdle::Work(Work::Command(id, cmd)) => {
                let out = sessions.dispatch(&id, &cmd);
                took.push((id, out));
            }
            WorkOrIdle::Work(Work::Woken(id)) => {
                let out = sessions.wake(&id);
                took.push((id, out));
            }
            // A session something closed: the worker's half is letting the harness go. This driver
            // is about turns, and a close is the daemon's own teardown - served here so the fixture
            // drains the bell the way the daemon does.
            WorkOrIdle::Work(Work::Close(id)) => {
                let _ = sessions.close(&id);
            }
        }
    }
    took
}

// ---------------------------------------------------------------------------
// One owner per session
// ---------------------------------------------------------------------------

/// A prompt to an attached, live subagent is served by its own thread only - the worker
/// races it and never wins.
#[test]
fn a_prompt_to_a_live_subagent_is_served_by_its_own_thread_only() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let _one = serial();
    let dir = TempDir::new("harnessd-one-owner");
    let path = dir.path.join("sessions.db");
    let root = "s-root-one-owner";
    let parts = parts_for(&config(root, Endpoint::new("127.0.0.1", 1), &path));
    let answer = a_plain_answer_turn(&parts.vocab, "thinking", "answered", 30);
    let stub = Stub::answering(vec![answer]);
    let cfg = config(root, stub.endpoint.clone(), &path);
    let registry = Registry::new();
    registry
        .create(root, "the root", wiring(&cfg))
        .expect("the root is created");
    let mut sessions =
        Sessions::open_first(&parts, cfg.clone(), registry.clone()).expect("the daemon opens");
    // The root's own `Work::Open` (already open, so a no-op), drained before the race.
    drive_worker(&mut sessions, &registry, Duration::from_millis(50));

    let handle = spawn(&sessions, root, "do the task, then wait");
    wait_until("the child to answer its task", || {
        registry.get(&handle).is_some()
            && call(
                &sessions,
                root,
                "task_result",
                serde_json::json!({"task": handle}),
            )
            .outcome
                == letibot_transcript::ToolOutcome::Ok
    });
    let child = registry.get(&handle).expect("the child is live");
    let head = child.attach("tui", "dead", Caps::default(), 0);
    for n in 0..5 {
        child.submit(
            &head.head_id,
            format!("c{n}"),
            0,
            CommandKind::Prompt {
                text: format!("question {n}"),
                attachments: Vec::new(),
            },
        );
    }

    let mut took = Vec::new();
    let began = Instant::now();
    while began.elapsed() < PATIENCE {
        took.extend(drive_worker(
            &mut sessions,
            &registry,
            Duration::from_millis(100),
        ));
        let asked = operator_rows(&child);
        if (0..5).all(|n| asked.iter().any(|r| r.contains(&format!("question {n}"))))
            && !child.turn_in_flight()
            && child.census().commands == 0
        {
            break;
        }
    }
    let theirs: Vec<_> = took
        .iter()
        .filter(|(id, _)| id == &handle)
        .map(|(_, o)| match o {
            Outcome::Replied(_) => "replied".to_string(),
            Outcome::Compacted(_) => "compacted".to_string(),
            Outcome::Failed(e) => format!("failed: {e}"),
            Outcome::Ignored => "ignored".to_string(),
            Outcome::HandedOn => "handed on".to_string(),
        })
        .collect();
    assert!(
        theirs.is_empty(),
        "the worker served the child's own commands — a second writer: {theirs:#?}"
    );
    assert_eq!(
        sessions.opened(),
        1,
        "the worker built a harness for a session its own thread holds"
    );
    let asked = operator_rows(&child);
    for n in 0..5 {
        assert!(
            asked.iter().any(|r| r.contains(&format!("question {n}"))),
            "question {n} never reached the child's own turn: {asked:#?}"
        );
    }
    assert!(stub.turns() >= 2, "the child ran its turns");
    registry.close();
    assert!(
        matches!(
            sessions.drain_subagents(PATIENCE),
            letibot_harnessd::sessions::SubagentExit::Waited { .. }
        ),
        "the child left with its hub closed"
    );
}

// ---------------------------------------------------------------------------
// One lifecycle, with an explicit close
// ---------------------------------------------------------------------------

/// A root session opened by the daemon's own `Sessions`, its children parking after `park`.
struct Daemon {
    _dir: TempDir,
    stub: Stub,
    parts: Parts,
    cfg: Config,
    registry: Arc<Registry>,
}

const ROOT: &str = "s-root-lifecycle";

fn a_daemon(tag: &str, park: Option<Duration>) -> Daemon {
    let dir = TempDir::new(tag);
    let path = dir.path.join("sessions.db");
    let parts = parts_for(&config(ROOT, Endpoint::new("127.0.0.1", 1), &path));
    let answer = a_plain_answer_turn(&parts.vocab, "thinking", "done and waiting", 30);
    let stub = Stub::answering(vec![answer]);
    let mut cfg = config(ROOT, stub.endpoint.clone(), &path);
    cfg.child_park_after = park;
    let registry = Registry::new();
    // The daemon's own store source (`cli.rs` installs the same one): it is where a parked
    // child's row is read back from when something reopens it.
    registry.set_source(
        letibot_harnessd::sessions::StoreSessions::open(&cfg).expect("the store opens"),
    );
    registry
        .create(ROOT, "the root", wiring(&cfg))
        .expect("the root is created");
    Daemon {
        _dir: dir,
        stub,
        parts,
        cfg,
        registry,
    }
}

fn answered(sessions: &Sessions<'_>, handle: &str) -> bool {
    call(
        sessions,
        ROOT,
        "task_result",
        serde_json::json!({"task": handle}),
    )
    .outcome
        == letibot_transcript::ToolOutcome::Ok
}

/// Fifty subagents started and finished leave no thread and no registry entry behind -
/// memwatch's thread census flat across 50 subagents.
#[test]
fn fifty_finished_subagents_leave_no_thread_and_no_registry_entry() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    // Fifty children at once: ~6 descriptors each for the store and its corpus, plus a socket per
    // turn - `room`'s note has the measurement, and the limit this box starts a run with is small.
    let Some(_) = room::room_for("fifty concurrent subagents", 512) else {
        return;
    };
    let _one = serial();
    let d = a_daemon("harnessd-fifty", Some(Duration::from_millis(300)));
    let mut sessions = Sessions::open_first(&d.parts, d.cfg.clone(), d.registry.clone())
        .expect("the daemon opens");
    drive_worker(&mut sessions, &d.registry, Duration::from_millis(50));
    let threads_before = (threads_named("subagent"), threads_named("letibot-deadlin"));
    let rss_before = rss_kib();

    let handles: Vec<String> = (0..50)
        .map(|n| spawn(&sessions, ROOT, &format!("task number {n}")))
        .collect();
    wait_until("all fifty children to answer", || {
        handles.iter().all(|h| answered(&sessions, h))
    });
    let threads_peak = (threads_named("subagent"), threads_named("letibot-deadlin"));
    let rss_peak = rss_kib();

    let began = Instant::now();
    while began.elapsed() < PATIENCE
        && (d.registry.len() > 1
            || threads_named("subagent") > threads_before.0
            || threads_named("letibot-deadlin") > threads_before.1)
    {
        drive_worker(&mut sessions, &d.registry, Duration::from_millis(50));
    }
    let threads_after = (threads_named("subagent"), threads_named("letibot-deadlin"));
    eprintln!(
        "threads (subagent, deadline): before {threads_before:?}, all answered {threads_peak:?}, \
         after parking {threads_after:?}; rss KiB: before {rss_before}, peak {rss_peak}, after {}; \
         registry entries after: {}; stub turns: {}",
        rss_kib(),
        d.registry.len(),
        d.stub.turns()
    );
    assert_eq!(
        d.registry.len(),
        1,
        "a finished subagent's entry stayed in the registry: {:?}",
        d.registry
            .list()
            .iter()
            .map(|b| b.session_id.clone())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        threads_after, threads_before,
        "threads were left behind by finished subagents (peak {threads_peak:?})"
    );
    assert_eq!(
        sessions.opened(),
        1,
        "the worker holds the root and nothing else"
    );
    // And each child still answers for what it did: the slot outlives the thread.
    assert!(handles.iter().all(|h| answered(&sessions, h)));
    d.registry.close();
}

/// A stopped child is gone, and a wake or a message to it is answered as gone - not
/// `HandedOn` into a hub nobody reads. Before the guard, a child that left `serve_child` on
/// a stop kept its hub registered
#[test]
fn a_wake_or_a_message_to_a_closed_child_is_answered_gone() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let _one = serial();
    // Parking off: this is the STOP exit, not the idle one.
    let d = a_daemon("harnessd-gone", None);
    let mut sessions = Sessions::open_first(&d.parts, d.cfg.clone(), d.registry.clone())
        .expect("the daemon opens");
    drive_worker(&mut sessions, &d.registry, Duration::from_millis(50));
    let handle = spawn(&sessions, ROOT, "answer, then wait");
    wait_until("the child to answer", || answered(&sessions, &handle));
    let hub = d.registry.get(&handle).expect("a parked child is live");
    hub.submit(
        letibot_sessionlog::hub::DAEMON_SUBMITTER,
        "stop-it",
        0,
        CommandKind::Interrupt {
            reason: "the parent is done with it".into(),
        },
    );
    wait_until("the stopped child to leave the registry", || {
        d.registry.get(&handle).is_none()
    });
    assert!(hub.is_closed(), "the stopped child's hub was left open");

    let woke = sessions.wake(&handle);
    assert!(
        matches!(woke, Outcome::Ignored),
        "a wake for a child that is gone was not answered as gone"
    );
    let said = call(
        &sessions,
        ROOT,
        "task_message",
        serde_json::json!({"task": handle, "text": "are you there?"}),
    );
    assert_ne!(
        said.outcome,
        letibot_transcript::ToolOutcome::Ok,
        "a message to a stopped child was reported delivered: {}",
        said.payload
    );
    assert!(
        said.payload.contains("Nothing was sent"),
        "the refusal says nothing was sent: {}",
        said.payload
    );
    d.registry.close();
}

/// A parked child comes back for the next message, and parks again - the idle-child policy's
/// other half: parking is store-only, not an end (`Config::child_park_after`).
#[test]
fn a_message_to_a_parked_child_reopens_it_from_the_store() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let _one = serial();
    let d = a_daemon("harnessd-reopen", Some(Duration::from_millis(300)));
    let mut sessions = Sessions::open_first(&d.parts, d.cfg.clone(), d.registry.clone())
        .expect("the daemon opens");
    drive_worker(&mut sessions, &d.registry, Duration::from_millis(50));
    let handle = spawn(&sessions, ROOT, "answer, then wait");
    wait_until("the child to answer", || answered(&sessions, &handle));
    wait_until("the idle child to park", || {
        d.registry.get(&handle).is_none()
    });

    let said = call(
        &sessions,
        ROOT,
        "task_message",
        serde_json::json!({"task": handle, "text": "one more thing"}),
    );
    assert_eq!(
        said.outcome,
        letibot_transcript::ToolOutcome::Ok,
        "a message to a parked child was refused: {}",
        said.payload
    );
    assert!(said.payload.contains("reopened"), "{}", said.payload);
    let turns_before = d.stub.turns();
    let began = Instant::now();
    while began.elapsed() < PATIENCE && d.stub.turns() == turns_before {
        drive_worker(&mut sessions, &d.registry, Duration::from_millis(50));
    }
    assert!(
        d.stub.turns() > turns_before,
        "the reopened child never ran the turn the message started"
    );
    let reopened = d.registry.get(&handle).expect("reopened into the registry");
    assert_eq!(
        reopened.owner(),
        letibot_sessionlog::hub::Owner::Worker,
        "a reopened child is the worker's, like every resumed session"
    );
    assert!(
        reopened.retained().iter().any(|e| matches!(
            &e.event,
            SessionEvent::TranscriptContent { item, .. } | SessionEvent::TranscriptAppended { item: Some(item), .. }
                if matches!(&**item, TranscriptItem::User { speaker: Speaker::Agent, parts }
                    if parts.iter().any(|p| matches!(p, UserPart::Text { text } if text.contains("one more thing"))))
        )),
        "the parent's words are on the reopened child's log as an agent's"
    );
    // And it parks again under the worker's half of the policy.
    let began = Instant::now();
    while began.elapsed() < PATIENCE && d.registry.get(&handle).is_some() {
        drive_worker(&mut sessions, &d.registry, Duration::from_millis(50));
    }
    assert!(
        d.registry.get(&handle).is_none(),
        "the reopened child never parked again"
    );
    assert_eq!(
        sessions.opened(),
        1,
        "the worker let go of the reopened child"
    );
    d.registry.close();
}

// ---------------------------------------------------------------------------
// Opening a session reads no list of the store
// ---------------------------------------------------------------------------

/// A store source that counts how often the WHOLE store is listed.
struct CountingSource {
    inner: Arc<letibot_harnessd::sessions::StoreSessions>,
    lists: std::sync::atomic::AtomicUsize,
}

impl letibot_sessionlog::registry::SessionSource for CountingSource {
    fn list(&self) -> Vec<letibot_sessionlog::registry::StoredBrief> {
        self.lists.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        letibot_sessionlog::registry::SessionSource::list(&*self.inner)
    }
}

/// Opening a session costs no full scan of the store on the worker.
#[test]
fn opening_a_session_lists_nothing_from_the_store() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let _one = serial();
    let dir = TempDir::new("harnessd-no-scan");
    let path = dir.path.join("sessions.db");
    let parts = parts_for(&config(ROOT, Endpoint::new("127.0.0.1", 1), &path));
    let cfg = config(ROOT, Endpoint::new("127.0.0.1", 1), &path);
    let registry = Registry::new();
    let source = Arc::new(CountingSource {
        inner: letibot_harnessd::sessions::StoreSessions::open(&cfg).expect("the store opens"),
        lists: std::sync::atomic::AtomicUsize::new(0),
    });
    registry.set_source(source.clone());
    registry
        .create(ROOT, "the root", wiring(&cfg))
        .expect("the root is created");
    let mut sessions =
        Sessions::open_first(&parts, cfg.clone(), registry.clone()).expect("the daemon opens");
    registry
        .create("s-a-second-session", "another", wiring(&cfg))
        .expect("created");
    let before = source.lists.load(std::sync::atomic::Ordering::SeqCst);
    sessions.open("s-a-second-session").expect("it opens");
    let scans = source.lists.load(std::sync::atomic::Ordering::SeqCst) - before;
    assert_eq!(
        scans, 0,
        "opening one session listed the whole store {scans} time(s) on the worker"
    );
    registry.close();
}

// ---------------------------------------------------------------------------
// The idle exit - a daemon nothing needs leaves (`Config::idle_exit_after`)
// ---------------------------------------------------------------------------

/// A daemon with nothing to do exits after its window, and a head attached holds it up for
/// as long as it is attached - through the daemon's own `Sessions`, a real hub and a real
/// attach.
#[test]
fn a_daemon_nothing_needs_exits_after_its_window_and_a_head_holds_it_up() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let _one = serial();
    let window = Duration::from_millis(800);
    let mut d = a_daemon("harnessd-idle-exit", None);
    d.cfg.idle_exit_after = Some(window);
    let mut sessions = Sessions::open_first(&d.parts, d.cfg.clone(), d.registry.clone())
        .expect("the daemon opens");
    // The window is armed by this, the first look, and not by the open - so what is asserted is
    // arithmetic against a clock the test took, not how fast this box opened a session.
    assert!(
        sessions
            .daemon_idle_at()
            .is_some_and(|at| at <= Instant::now()),
        "an unarmed idle exit makes the daemon's first look due at once"
    );
    let armed_at = Instant::now();
    assert!(
        !sessions.idle_exit_due(),
        "a daemon whose first look finds nothing to do is not due at that look"
    );
    let deadline = sessions
        .idle_exit_deadline()
        .expect("the first look armed a window");
    assert!(
        deadline >= armed_at + window,
        "the first look arms a whole window: {deadline:?} came before {armed_at:?} + {window:?}"
    );
    assert!(
        sessions.daemon_idle_at().is_some_and(|at| at <= deadline),
        "and the loop wakes for it"
    );
    let hub = d.registry.get(ROOT).expect("the root's hub");
    let head = hub.attach("tui", "test", Caps::default(), 0);
    std::thread::sleep(window + Duration::from_millis(100));
    assert_eq!(
        sessions.idle_exit_refusal(),
        Some(format!("{ROOT} has a head attached"))
    );
    assert!(!sessions.idle_exit_due(), "a head attached holds it up");
    let registry = d.registry.clone();
    let row = || {
        registry
            .settings(ROOT)
            .into_iter()
            .find(|r| r.key == letibot_sessionlog::registry::IDLE_EXIT_KEY)
            .map(|r| r.value)
    };
    assert_eq!(
        row(),
        Some(format!("held: {ROOT} has a head attached")),
        "a head's settings say what holds the daemon"
    );
    hub.detach(&head.head_id);
    let _ = sessions.idle_exit_due();
    assert_eq!(row().as_deref(), Some("nothing holds it"));
    assert_eq!(sessions.idle_exit_refusal(), None, "nothing holds it now");
    // The look that found the head re-armed the window from itself, so the head going does not make
    // the daemon due at once: the window is counted again from that look.
    assert!(
        !sessions.idle_exit_due(),
        "a look that found it busy re-armed the window from itself"
    );
    // And a whole window with nothing to do is due. Waited for rather than slept: a sleep that
    // overshoots on a loaded box is the box's lateness, not the daemon's.
    let began = Instant::now();
    while !sessions.idle_exit_due() {
        assert!(
            began.elapsed() < PATIENCE,
            "a daemon with nothing to do never became due"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    d.registry.close();
}

/// `--idle-exit 0` keeps the daemon up - never due, however long it sits with nothing to do.
#[test]
fn a_daemon_with_no_idle_window_never_exits_on_its_own() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let _one = serial();
    let mut d = a_daemon("harnessd-no-idle-exit", None);
    d.cfg.idle_exit_after = None;
    let mut sessions = Sessions::open_first(&d.parts, d.cfg.clone(), d.registry.clone())
        .expect("the daemon opens");
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(sessions.idle_exit_refusal(), None, "nothing holds it");
    assert!(!sessions.idle_exit_due(), "and still it stays");
    d.registry.close();
}
