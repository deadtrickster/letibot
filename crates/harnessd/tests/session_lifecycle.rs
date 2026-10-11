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

/// Threads in this process whose name starts with `prefix` — `memwatch`'s census, read the same
/// way (`/proc/self/task/*/comm`).
fn threads_named(prefix: &str) -> usize {
    let Ok(dir) = std::fs::read_dir("/proc/self/task") else {
        return 0;
    };
    dir.filter_map(|e| e.ok())
        .filter_map(|e| std::fs::read_to_string(e.path().join("comm")).ok())
        .filter(|n| n.trim_end().starts_with(prefix))
        .count()
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
