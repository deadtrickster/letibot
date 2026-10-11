//! **One owner per session, and one lifecycle with an explicit close** — `TODO.md` R68.1–R68.3,
//! end to end.
//!
//! The 2026-10-11 review of the daemon found that a subagent's hub had two readers and no end:
//!
//! * **two readers** — the child's own thread (`Hub::take_own_work_until`, from
//!   `harness::serve_child`) and the daemon's worker (`Hub::try_command`, which popped anything).
//!   A head prompting the subagent it was attached to could make the worker build a SECOND
//!   `Harness` for a live child: two writers on one transcript (R68.1, CONFIRMED);
//! * **no end** — nothing ever removed a session from the registry, closed a finished child's
//!   hub or dropped its harness. R66 measured what that costs: two threads (`subagent-*` and
//!   `letibot-deadline`) and ~12 MB per finished subagent, never released (7 → 19 threads for six
//!   children). The deadline thread was ended by R67 item 13; this file measures the other.
//!
//! # What is proved here, and through what
//!
//! The real chain: a root session opened by the daemon's own `Sessions`, a child spawned through
//! that session's tool runtime (the door a model's `task` call takes), and the daemon's worker
//! driven exactly as `Daemon::run` drives it — `Registry::next_work_until` then
//! `Sessions::dispatch`. The model is a stub answering canned frames.
//!
//! # What this needs
//!
//! The vocabulary GGUF (`Harness::open` renders its stable prefix) and no model.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
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
use letibot_tokencore::Vocab;
use letibot_tools::NullToolSink;
use letibot_transcript::{Speaker, ToolCall, TranscriptItem, UserPart};
use letibot_turn::Endpoint;

#[path = "../../turn/tests/support/canned.rs"]
mod canned;

use canned::Frame;

const IM_END: u32 = 248046;
const THINK_CLOSE: u32 = 248069;

const PATIENCE: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// Fixtures — `message_between_turns.rs`'s shapes, kept to what this file needs
// ---------------------------------------------------------------------------

fn config(session_id: &str, endpoint: Endpoint, store: &Path) -> Config {
    let mut cfg = Config::for_this_box("/tmp");
    cfg.dialect = Dialect::Qwen;
    // `coder` seats the delegation trio (`task`, `task_result`, `task_message`).
    cfg.seat = Seat::Coder;
    cfg.endpoint = endpoint;
    cfg.http_retries = 0;
    cfg.context_window = Some(32_768);
    cfg.store = Some(store.to_path_buf());
    cfg.session_id = session_id.to_string();
    cfg.web_search = None;
    cfg.permission = Vec::new();
    cfg.vocab_gguf = std::env::var("LETIBOT_VOCAB_GGUF")
        .ok()
        .map(std::path::PathBuf::from)
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

/// A model that answers every request at once with the next script (cycling), and answers a
/// spawn's `GET /props` with a window without consuming a script.
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

fn write_frames(mut stream: TcpStream, frames: &[Frame]) -> std::io::Result<()> {
    stream.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
          Transfer-Encoding: chunked\r\n\r\n",
    )?;
    stream.write_all(&canned::sse_body(frames))?;
    stream.flush()
}

fn ids(vocab: &Vocab, text: &str) -> Vec<u32> {
    vocab.tokenize_text(text).expect("text tokenizes")
}

fn spoken(vocab: &Vocab, ids: &[u32]) -> Vec<Frame> {
    ids.iter()
        .map(|id| Frame::Token {
            id: *id,
            text: vocab
                .detokenize(&[*id], true)
                .expect("one token decodes")
                .leak(),
        })
        .collect()
}

fn a_plain_answer_turn(vocab: &Vocab, thought: &str, answer: &str, n_prompt: u64) -> Vec<Frame> {
    let thought_ids = ids(vocab, thought);
    let answer_ids = ids(vocab, answer);
    let mut frames = vec![Frame::Progress {
        total: n_prompt,
        processed: n_prompt,
    }];
    frames.extend(spoken(vocab, &thought_ids));
    frames.extend(spoken(vocab, &[THINK_CLOSE]));
    frames.extend(spoken(vocab, &answer_ids));
    frames.extend(spoken(vocab, &[IM_END]));
    frames.push(Frame::Final {
        stop_type: "eos",
        n_decoded: (thought_ids.len() + answer_ids.len() + 2) as u64,
        n_prompt,
        cache_n: 0,
    });
    frames
}

struct TempDir {
    path: std::path::PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock before epoch")
                .as_nanos()
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
            SessionEvent::TranscriptContent { item, .. } => match &**item {
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

/// Threads in this process whose name starts with `prefix` — `memwatch`'s own census (the line
/// the daemon's watchdog prints), so the test counts what an operator would read.
fn threads_named(prefix: &str) -> usize {
    letibot_harnessd::memwatch::thread_names()
        .into_iter()
        .filter(|(name, _)| name.starts_with(prefix))
        .map(|(_, n)| n)
        .sum()
}

/// The process's resident set, in KiB — for the record the test prints, not for an assertion.
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

/// **One test at a time in this binary**: the thread census is the PROCESS's, and a sibling test
/// spawning children in parallel would be counted as this one's leak.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// **The worker's own loop, for one slice of time** — `Daemon::run`'s three arms that matter
/// here. Answers the commands it took, by session, so a test can say which sessions the worker
/// served.
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
        }
    }
    took
}

// ---------------------------------------------------------------------------
// R68.1 — one owner per session
// ---------------------------------------------------------------------------

/// **A prompt to an attached, live subagent is served by its own thread only** — the worker
/// races it and never wins.
///
/// Five prompts are submitted at once, so while the child's thread is inside the first turn the
/// rest sit on its queue — exactly the window in which the worker, woken by the same submit's
/// bell, used to pop one and build a second `Harness` for the child (`Sessions::harness_as`).
/// MEASURED before the fix: the worker took prompts off the child's queue and `opened()` grew.
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
            &format!("c{n}"),
            0,
            CommandKind::Prompt {
                text: format!("question {n}"),
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
// R68.2 / R68.3 — one lifecycle, with an explicit close
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

/// **Fifty subagents started and finished leave no thread and no registry entry behind** — the
/// "done when" of R68: memwatch's thread census flat across 50 subagents.
///
/// R66 measured the leak on a scratch daemon: 7 → 19 threads for six finished subagents, ~12 MB
/// each, never released — the `subagent-*` thread parked in `serve_child` until a hub close that
/// never came, and `letibot-deadline` (R67 item 13 ended that one with its host, but the host
/// lived as long as the parked harness did). MEASURED here with the policy off
/// (`child_park_after = None`, the old behaviour): 50 `subagent-*` threads and 50
/// `letibot-deadline` threads still alive, and 51 registry entries, after every child answered.
#[test]
fn fifty_finished_subagents_leave_no_thread_and_no_registry_entry() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
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

/// **A stopped child is gone, and a wake or a message to it is answered as gone** — not
/// `HandedOn` into a hub nobody reads.
///
/// Before the guard, a child that left `serve_child` on a stop kept its hub registered: the
/// worker's `wake_route` said `ItsOwnReader`, `Sessions::wake` answered `HandedOn`, and the
/// wake was handed to a condvar with no thread behind it (R68.2, CONFIRMED). MEASURED before:
/// the hub stayed in the registry and the wake came back `HandedOn`.
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

/// **A parked child comes back for the next message, and parks again** — the idle-child policy's
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
            SessionEvent::TranscriptContent { item, .. }
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
