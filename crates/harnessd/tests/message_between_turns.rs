//! **A message to a child BETWEEN turns starts a turn for it** — the operator's ruling,
//! verbatim: *"fix task_message - it should enqueue"*.
//!
//! What it rules on is the sentence in the middle of `HarnessTaskRunner::send`'s old
//! refusal: a message rides the child's queue like every other command, so a submission
//! against a child that was not in a turn answered `Accepted` and then sat there —
//! *"nothing drains a child's queue between turns"* — and the parent would have been told
//! it had corrected a child that never heard a word of it. The refusal was honest. The fix
//! is to make its reason false: the message goes on the child's queue FIRST, the child's
//! own serving thread is woken SECOND, and the loop that drains that queue answers a
//! `Message` by RUNNING THE TURN that reads it (`serve_child`'s `ChildCommand::Hear` →
//! `Harness::submit_a_parents_message`).
//!
//! # What is proved here, and through what
//!
//! The real chain, not a unit on the submitter: a parent [`Harness`] with the REAL runner,
//! the `task` tool invoked through that session's own tool runtime (so the child is spawned
//! exactly as a model's call spawns one — its own thread, its own hub, adopted into the
//! registry), and then `task_message` invoked the same way. What is asserted is on the
//! CHILD: its own log holds the parent's words as `Speaker::Agent` (not the operator's), and
//! — the one that cannot be faked — its turn RAN, with that text in the prompt the model was
//! handed.
//!
//! # The two states a child can be in, and how the second one is reached
//!
//! A child between turns is the easy state to stand in: wait for its first answer and it is
//! parked. A child whose turn is RUNNING is a window, and a window a test races is a flaky
//! test — so the model is a stub that **holds each request open until the test lets it go**
//! (`Stub`, which records every request body). The turn is therefore mid-flight for as long
//! as this file needs it to be, and the proof that the message landed IN that turn is the
//! next round's prompt: the request's token ids, detokenized with the same vocabulary the
//! child tokenized with.
//!
//! # What this needs, and what it does not
//!
//! The vocabulary GGUF (`Harness::open` renders its stable prefix) and **not** a model: the
//! turns are answered by a stub built on the shared `canned` frames, included by path rather
//! than copied. No process tree, so no apparatus gate beyond the GGUF.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use letibot_harnessd::config::{Config, Seat};
use letibot_harnessd::sessions::{Outcome, Sessions};
use letibot_harnessd::{Dialect, Harness, Parts};
use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::hub::{CommandKind, DAEMON_SUBMITTER, Hub, QueuedCommand};
use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_tokencore::Vocab;
use letibot_tools::NullToolSink;
use letibot_transcript::{Speaker, ToolCall, TranscriptItem, UserPart};
use letibot_turn::Endpoint;

/// The canned server the turn crate's tests replay, shared by path rather than copied — a
/// second copy of the wire shape would drift from the first.
#[path = "../../turn/tests/support/canned.rs"]
mod canned;

use canned::Frame;

/// Qwen's own ids, the same values the turn crate's canned tests pin.
const IM_END: u32 = 248046;
const THINK_OPEN: u32 = 248068;
const THINK_CLOSE: u32 = 248069;
const TOOL_CALL_OPEN: u32 = 248058;
const TOOL_CALL_CLOSE: u32 = 248059;

/// What the parent says. A marker, so it can be found in a row and in a prompt.
const MESSAGE: &str = "stop and report what you have";

/// The child's answer to it, and to the task before it — two different sentences so the
/// test can say WHICH turn a row came from.
const TO_THE_TASK: &str = "the task is done, waiting";
const TO_THE_PARENT: &str = "understood, reporting now";

/// How long the driver waits for something a correct build produces in milliseconds —
/// generous on purpose, because the assertion is what happened, not how fast.
const PATIENCE: Duration = Duration::from_secs(20);

// ---------------------------------------------------------------------------
// The fixtures
// ---------------------------------------------------------------------------

fn config(session_id: &str, endpoint: Endpoint, store: &Path) -> Config {
    let mut cfg = Config::for_this_box("/tmp");
    cfg.dialect = Dialect::Qwen;
    // **`coder` because that is the seat the delegation trio lives on** — `task`,
    // `task_result`, `task_message`. The default seat is `orchestrator`, which seats none
    // of them, and a test that invoked a tool the session does not seat would be measuring
    // the seat table rather than this door.
    cfg.seat = Seat::Coder;
    cfg.endpoint = endpoint;
    // No retry ladder: nothing here should fail, and a failure that retried would spend
    // the suite's time hiding behind doubling waits.
    cfg.http_retries = 0;
    // **A window, because a child is planned against one.** A spawn with no window is
    // refused by name (`nothing was spawned on `local`: nobody can name a window for it`),
    // and the refusal is right: a child with no window has nothing to compact it.
    cfg.context_window = Some(32_768);
    cfg.store = Some(store.to_path_buf());
    cfg.session_id = session_id.to_string();
    // The operator's own `providers.toml` is not this test's business (`wired.rs`'s rule):
    // without these, a search key on the developer's laptop decides what is seated.
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
    // And the operator's per-project mode is not this test's business either.
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

/// **A parent session with the REAL runner** — the whole production path, with the model
/// replaced by the stub. Its own hub is adopted so the tree looks the way a daemon's does.
struct Parent {
    harness: Harness,
    registry: Arc<Registry>,
}

fn a_parent(session_id: &str, parts: &Parts, cfg: &Config) -> Parent {
    let registry = Registry::new();
    let hub = registry.new_hub(session_id.to_string());
    registry
        .adopt(hub.clone(), session_id.to_string(), wiring(cfg), None)
        .expect("the parent is in the registry");
    let harness =
        Harness::open_with_registry(parts, cfg.clone(), hub, None, None, registry.clone())
            .expect("the parent opens");
    Parent { harness, registry }
}

impl Parent {
    /// **One tool call, as the engine would make it** — through the session's own runtime,
    /// which is what makes this the production door rather than a call on the runner.
    fn call(&self, name: &str, args: serde_json::Value) -> letibot_tools::ToolResult {
        let rt = self.harness.runtime_handle();
        let call = ToolCall {
            id: format!("call_{name}"),
            name: name.to_string(),
            arguments: args.to_string(),
        };
        rt.invoke("t1", &call, &mut NullToolSink)
    }

    /// Start a child and hand back its handle.
    fn spawn(&self, prompt: &str) -> String {
        let r = self.call("task", serde_json::json!({"prompt": prompt}));
        match r.outcome {
            letibot_transcript::ToolOutcome::Backgrounded { handle, .. } => handle,
            other => panic!("`task` did not start a child: {other:?} — {}", r.payload),
        }
    }

    /// **The child's hub, once its own thread has adopted it.** A spawn returns as soon as
    /// the thread is started; the session enters the registry after the child's harness
    /// opens, on that thread — so this waits for the fact rather than assuming it.
    fn hub_of(&self, handle: &str) -> Arc<Hub> {
        let began = Instant::now();
        loop {
            if let Some(hub) = self.registry.get(handle) {
                return hub;
            }
            if began.elapsed() >= PATIENCE {
                // The child's own settlement is where a spawn that died says why.
                let state = self.call("task_result", serde_json::json!({"task": handle}));
                panic!(
                    "`{handle}` was spawned and never entered the registry — the child says: {}",
                    state.payload
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Tell it something. The reply is the runner's own words, which is half of what this
    /// file is about — a delivery that is not said is a delivery the parent cannot trust.
    fn message(&self, handle: &str, text: &str) -> Result<String, String> {
        let r = self.call(
            "task_message",
            serde_json::json!({"task": handle, "text": text}),
        );
        match r.outcome {
            letibot_transcript::ToolOutcome::Ok => Ok(r.payload),
            _ => Err(r.payload),
        }
    }

    /// **Wait for the child to be between turns** — its first turn's answer has landed and
    /// it is parked. `task_result` is the door the parent has for that, and it does not
    /// block without a `timeout_ms`.
    fn wait_until_parked(&self, handle: &str) {
        let began = Instant::now();
        loop {
            let r = self.call("task_result", serde_json::json!({"task": handle}));
            if r.outcome == letibot_transcript::ToolOutcome::Ok {
                return;
            }
            if began.elapsed() > PATIENCE {
                panic!("`{handle}` never answered its first turn: {}", r.payload);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// **The tree is closed at the end of a test**, the way the daemon closes it: every child's
/// hub goes, so its serving thread takes `Closed`, leaves `run_to_completion` and ends. A
/// parked child left behind is a thread still holding a session — harmless in principle, and
/// measured here as a process that exited with a signal now and then, which is exactly the
/// kind of flake that gets blamed on the next person's change.
impl Drop for Parent {
    fn drop(&mut self) {
        self.registry.close();
    }
}

// ---------------------------------------------------------------------------
// The child's own log
// ---------------------------------------------------------------------------

/// Every user row on a session's log, as (speaker, text). This is the child's OWN log —
/// the thing its parent and the operator both read.
fn user_rows(hub: &Hub) -> Vec<(Speaker, String)> {
    hub.retained()
        .iter()
        .filter_map(|e| match &e.event {
            SessionEvent::TranscriptContent { item, .. } => match &**item {
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
            },
            _ => None,
        })
        .collect()
}

/// The rows on which the child heard `text` as its parent's words.
fn heard(hub: &Hub, text: &str) -> Vec<String> {
    user_rows(hub)
        .into_iter()
        .filter(|(speaker, said)| *speaker == Speaker::Agent && said.contains(text))
        .map(|(_, said)| said)
        .collect()
}

/// The assistant's own answers, which is how a turn that RAN is told from one that was
/// merely queued.
fn answers(hub: &Hub) -> Vec<String> {
    hub.retained()
        .iter()
        .filter_map(|e| match &e.event {
            SessionEvent::TranscriptContent { item, .. } => match &**item {
                TranscriptItem::Assistant { text, .. } => Some(text.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// Every warning on a session's log, as (code, detail) — where a lost message, a failed
/// turn and the worker's `message_idle` all land.
fn warnings(hub: &Hub) -> Vec<(String, String)> {
    hub.retained()
        .iter()
        .filter_map(|e| match &e.event {
            SessionEvent::Warning { code, detail, .. } => Some((code.clone(), detail.clone())),
            _ => None,
        })
        .collect()
}

/// Wait until `what` answers, or panic with the session's whole log — the log IS the
/// diagnosis for a timing failure, and a bare timeout would hide it.
fn wait_for(hub: &Hub, what: impl Fn(&Hub) -> Option<String>) -> String {
    let began = Instant::now();
    while began.elapsed() < PATIENCE {
        if let Some(found) = what(hub) {
            return found;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!(
        "nothing the test waited for arrived within {PATIENCE:?} — the session's log:\n{:#?}",
        hub.retained().iter().map(|e| &e.event).collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------------
// The stub
// ---------------------------------------------------------------------------

/// **A model that answers on a gate, and keeps every request it was asked.**
///
/// `canned::Canned` answers as fast as it can, which closes the window this file has to
/// reach into: a turn is mid-flight only for as long as its model takes to answer. So each
/// request is read, RECORDED, and held until the test releases it — *"the child is in a
/// turn"* becomes a state the test stands in rather than races. The recorded body is what
/// makes the other half provable: a request carries token ids, and detokenized with the
/// same vocabulary the child tokenized with, they are the prompt the model was handed.
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
                // A listener that cannot accept is a listener nobody will be answered by:
                // there is nothing left to say, and the client's own error is the report.
                let Ok((stream, _)) = listener.accept() else {
                    return;
                };
                let (line, body) = read_request(stream.try_clone().expect("clone the socket"));
                // **`/props` is not a turn and does not consume a script.** A spawn asks the
                // parent's own endpoint for its window (`HarnessTaskRunner::start`'s
                // `served_ctx`, one `GET` per child while this session has never left
                // local) — a fact about the spawn path rather than about the model, and
                // counting it as a turn would shift every script by one.
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

    fn bodies(&self) -> Vec<String> {
        self.shared.inner.lock().unwrap().bodies.clone()
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

    /// **What the model was handed for request `n`** — the token ids, detokenized with the
    /// vocabulary the child tokenized with.
    fn prompt_of(&self, n: usize, vocab: &Vocab) -> String {
        let body = self.bodies().get(n).cloned().unwrap_or_default();
        let json: serde_json::Value =
            serde_json::from_str(&body).unwrap_or_else(|e| panic!("request {n} is JSON: {e}"));
        let ids: Vec<u32> = json["prompt"]
            .as_array()
            .unwrap_or_else(|| panic!("request {n} carries a prompt: {body}"))
            .iter()
            .map(|v| v.as_u64().expect("a token id") as u32)
            .collect();
        vocab
            .detokenize(&ids, true)
            .unwrap_or_else(|e| panic!("request {n}'s ids decode: {e}"))
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

/// Drain one request — headers plus the body `Content-Length` names — so the client's write
/// side is finished before the answer arrives. `model_change_mid_retry.rs`'s reader, and it
/// hands back the request line as well so the caller can tell a `/props` probe from a turn.
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
        if acc.len() > (1 << 22) {
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

/// **The endpoint's own props**, as `served_ctx` reads them: the one field a spawn asks for
/// is the window, and a server that answers nothing gets `None`, which is no wall at all.
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

// ---------------------------------------------------------------------------
// The turns the stub replays
// ---------------------------------------------------------------------------

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

/// A complete turn that answers in plain text: thinks, closes the block, answers, ends.
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

/// A turn that calls ONE tool and ends — so the turn has a round boundary, which is the
/// only place a turn in flight can be told anything. `compact.rs`'s shape, kept identical
/// so the two files' fixtures cannot drift apart on the wire bytes that matter.
fn a_tool_call_turn(vocab: &Vocab, call: &str, n_prompt: u64) -> Vec<Frame> {
    let call_ids = ids(vocab, call);
    let mut frames = vec![Frame::Progress {
        total: n_prompt,
        processed: n_prompt,
    }];
    frames.push(Frame::Token {
        id: THINK_OPEN,
        text: "",
    });
    frames.push(Frame::Token {
        id: THINK_CLOSE,
        text: "",
    });
    frames.push(Frame::Token {
        id: TOOL_CALL_OPEN,
        text: "<tool_call>",
    });
    frames.extend(spoken(vocab, &call_ids));
    frames.push(Frame::Token {
        id: TOOL_CALL_CLOSE,
        text: "</tool_call>",
    });
    frames.push(Frame::Final {
        stop_type: "eos",
        // The think fences and the two tool-call fences: the accumulator counts what was
        // streamed, so this has to agree with it exactly.
        n_decoded: (call_ids.len() + 4) as u64,
        n_prompt,
        cache_n: 0,
    });
    frames
}

/// The `read` call the child's first round makes, in the Qwen dialect's own shape.
fn read_call(path: &str) -> String {
    format!("\n<function=read>\n<parameter=path>\n{path}\n</parameter>\n</function>\n")
}

/// A directory that removes itself, because a test that leaks a store per run eventually
/// fills the disk and the run that finds out is not this one.
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

    fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// The path as `read` takes it: relative to the session root, which is `/tmp`.
    fn relative(&self) -> String {
        self.path
            .file_name()
            .expect("a temp dir has a name")
            .to_string_lossy()
            .into_owned()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

// ---------------------------------------------------------------------------
// THE ONE THIS CHANGE EXISTS FOR
// ---------------------------------------------------------------------------

/// **A message to a child that is BETWEEN TURNS is heard: the child's own turn runs with
/// it.**
///
/// The operator's ruling, verbatim: *"fix task_message - it should enqueue"*. The old
/// refusal's reason was that a child's queue is drained by nobody between turns, and the
/// measured cost was theirs: *"message was queued when you stopped and it didnt restart
/// you"*. So what this asserts is exactly the sentence that was false — the child is parked,
/// nothing is running, and the message is not merely queued but READ: the child's log holds
/// it as its parent's words, and the child's turn ran with it.
#[test]
fn a_message_to_a_child_between_turns_starts_its_turn() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-message-between");
    let path = dir.path().join("sessions.db");
    let parent_id = "s-parent-who-corrects";

    // The vocabulary first — the canned frames must speak the ids the harness will send.
    let parts = parts_for(&config(parent_id, Endpoint::new("127.0.0.1", 1), &path));
    let first = a_plain_answer_turn(&parts.vocab, "planning", TO_THE_TASK, 30);
    let second = a_plain_answer_turn(&parts.vocab, "hearing", TO_THE_PARENT, 30);
    // Two requests: the task's own turn, and the turn the message starts. A third would be
    // a turn nothing asked for, and it would arrive as a socket error rather than silence.
    let stub = Stub::answering(vec![first, second], 2);

    let cfg = config(parent_id, stub.endpoint.clone(), &path);
    let parent = a_parent(parent_id, &parts, &cfg);
    let handle = parent.spawn("do the task, then stop and wait");
    let child = parent.hub_of(&handle);

    // Between turns: its first answer has landed and its serving thread is parked.
    parent.wait_until_parked(&handle);
    assert!(
        !child.status().running,
        "the fixture must stand in the state this test is about"
    );

    let said = parent
        .message(&handle, MESSAGE)
        .expect("a message to a between-turns child is queued and a turn started for it");
    assert!(
        said.contains("between turns") && said.contains("started"),
        "the answer must say WHICH delivery this was, and that a turn was started: {said}"
    );
    assert!(
        said.contains(parent_id),
        "and name the parent it was told as: {said}"
    );

    // **The child's turn RAN, with the message in it.** The row is the parent's words — not
    // the operator's, which is the whole reason a message is not a prompt.
    let found = wait_for(&child, |hub| {
        answers(hub).into_iter().find(|a| a.contains(TO_THE_PARENT))
    });
    assert!(found.contains(TO_THE_PARENT), "{found}");

    let rows = heard(&child, MESSAGE);
    assert_eq!(
        rows.len(),
        1,
        "the child heard the parent's message exactly once: {rows:#?}"
    );
    assert!(
        rows[0].contains(&format!("message from your parent session `{parent_id}`")),
        "the parent is NAMED in the words the child hears: {}",
        rows[0]
    );
    assert!(
        !user_rows(&child)
            .iter()
            .any(|(s, t)| *s == Speaker::Operator && t.contains(MESSAGE)),
        "the message must not be recorded as something the operator typed"
    );

    // **And the prompt the model was handed for that turn says so too** — the row is the
    // child's record; the request is what the model actually read.
    let second_prompt = stub.prompt_of(1, &parts.vocab);
    assert!(
        second_prompt.contains(MESSAGE),
        "the message never reached the prompt of the turn it started: {second_prompt}"
    );
    assert!(
        answers(&child).iter().any(|a| a.contains(TO_THE_PARENT)),
        "the turn that heard it answered"
    );
    assert!(
        warnings(&child).is_empty(),
        "nothing went wrong on the child's log: {:#?}",
        warnings(&child)
    );
}

// ---------------------------------------------------------------------------
// The state that already worked, which must not regress
// ---------------------------------------------------------------------------

/// **A message to a child whose turn is ALREADY RUNNING still lands in that turn.**
///
/// The half of the old behaviour that was right, kept: the child hears it at its next round
/// boundary, as its parent's words, and keeps working — it is not a second prompt and it
/// does not start a second turn. The turn is held open by the stub, so "already running" is
/// a fact the test stands in rather than races, and the proof it landed IN that turn is the
/// next round's prompt: a turn that had ended would have answered the message in a turn of
/// its own, and this request would not carry it.
#[test]
fn a_message_to_a_running_child_lands_in_that_turn() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-message-running");
    let path = dir.path().join("sessions.db");
    let parent_id = "s-parent-who-steers";

    // The file the child's first round reads — a relative path, because the session root is
    // `/tmp` and `read` is confined to it.
    std::fs::write(dir.path().join("note.txt"), "the plan, in one line\n")
        .expect("the file to read is written");
    let target = format!("{}/note.txt", dir.relative());

    let parts = parts_for(&config(parent_id, Endpoint::new("127.0.0.1", 1), &path));
    // Round zero calls a tool (so the turn has a round boundary at all); round one answers.
    let round0 = a_tool_call_turn(&parts.vocab, &read_call(&target), 30);
    let round1 = a_plain_answer_turn(&parts.vocab, "hearing", TO_THE_PARENT, 30);
    // **Held**: nothing is answered until this test says so, which is what makes the child
    // reliably mid-turn when the message arrives.
    let stub = Stub::held(vec![round0, round1], 2);

    let cfg = config(parent_id, stub.endpoint.clone(), &path);
    let parent = a_parent(parent_id, &parts, &cfg);
    let handle = parent.spawn("read the note, then answer");
    let child = parent.hub_of(&handle);

    assert!(
        stub.wait_for_requests(1),
        "the child's first round was never sent"
    );
    assert!(
        child.status().running,
        "the fixture must stand in the state this test is about: the child's turn is running"
    );

    let said = parent
        .message(&handle, MESSAGE)
        .expect("a message to a running child is accepted");
    assert!(
        said.contains("turn is running") && said.contains("round boundary"),
        "the answer must say this was the RUNNING delivery: {said}"
    );

    // Let round zero's tool call through: the child reads the file, and the boundary after
    // it is where the message can be heard.
    stub.release(1);
    assert!(
        stub.wait_for_requests(2),
        "the child never reached a second round — the message had no boundary to arrive at"
    );
    let second = stub.prompt_of(1, &parts.vocab);
    assert!(
        second.contains(MESSAGE),
        "the message did not land in the running turn: {second}"
    );
    assert!(
        second.contains("message from your parent session"),
        "and it is the parent's words the child was handed: {second}"
    );

    stub.release(2);
    wait_for(&child, |hub| {
        answers(hub).into_iter().find(|a| a.contains(TO_THE_PARENT))
    });
    // **One turn, and the message in it** — a message that had started a turn of its own
    // would have needed a third request, which this stub cannot serve.
    let rows = heard(&child, MESSAGE);
    assert_eq!(rows.len(), 1, "heard once, in the turn: {rows:#?}");
    assert!(
        warnings(&child).is_empty(),
        "no `message_idle` and no failed turn: {:#?}",
        warnings(&child)
    );
}

// ---------------------------------------------------------------------------
// One queue, one drainer
// ---------------------------------------------------------------------------

/// **Two messages while the first queued turn is starting do not make two turns for one of
/// them, and neither is lost.**
///
/// One queue and one drainer: the child's own thread. The queue takes both, the loop runs
/// what the queue holds — one turn per message it has to carry, never two turns racing for
/// one, and never a message handed to the daemon's worker instead (which would publish
/// `message_idle` and drop it). The stub serves exactly TWO requests, so a third turn —
/// the shape a doubled start would take — cannot be answered at all and would say so on the
/// child's log.
#[test]
fn a_second_message_does_not_start_a_second_turn_for_the_first() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-message-twice");
    let path = dir.path().join("sessions.db");
    let parent_id = "s-parent-who-says-twice";

    const FIRST: &str = "first: stop and report";
    const SECOND: &str = "second: and say what you tried";

    let parts = parts_for(&config(parent_id, Endpoint::new("127.0.0.1", 1), &path));
    let task = a_plain_answer_turn(&parts.vocab, "planning", TO_THE_TASK, 30);
    let one = a_plain_answer_turn(&parts.vocab, "hearing", "reporting", 30);
    let two = a_plain_answer_turn(&parts.vocab, "hearing", "and what I tried", 30);
    let stub = Stub::answering(vec![task, one, two], 3);

    let cfg = config(parent_id, stub.endpoint.clone(), &path);
    let parent = a_parent(parent_id, &parts, &cfg);
    let handle = parent.spawn("do the task, then stop and wait");
    let child = parent.hub_of(&handle);
    parent.wait_until_parked(&handle);

    // Back to back, with no wait between them: the second arrives while the first queued
    // turn is starting.
    parent.message(&handle, FIRST).expect("the first is taken");
    parent
        .message(&handle, SECOND)
        .expect("the second is taken");

    // Both heard, in order, exactly once each.
    let found = wait_for(&child, |hub| {
        let f = heard(hub, FIRST);
        let s = heard(hub, SECOND);
        (f.len() == 1 && s.len() == 1).then(|| format!("{} then {}", f[0], s[0]))
    });
    assert!(found.contains(FIRST) && found.contains(SECOND), "{found}");

    // **And nothing was handed to the worker instead.** `message_idle` is the sentence the
    // daemon's own arm publishes for a message it cannot deliver; a child's message must
    // never reach it, because the child's reader is right there.
    assert!(
        !warnings(&child)
            .iter()
            .any(|(code, _)| code == "message_idle"),
        "the daemon's worker took a child's message instead of handing it back: {:#?}",
        warnings(&child)
    );
    assert!(
        !warnings(&child)
            .iter()
            .any(|(code, _)| code == "turn_failed"),
        "a turn was started that nothing could answer: {:#?}",
        warnings(&child)
    );
}

// ---------------------------------------------------------------------------
// The failures stay failures
// ---------------------------------------------------------------------------

/// **A genuine failure is still refused by name, and never claims to have queued.**
///
/// Three of them, and they are three different facts. A handle this session never minted is
/// the refusal it always was. A child whose own THREAD has ended — a session that is over —
/// takes no wake, so a message queued for it would be read by nobody; that is the one
/// refusal this change adds, and it is checked BEFORE the submit so that *"nothing was
/// sent"* is true when it is said. And a session that goes away with the message already on
/// its queue is the one window the new order cannot close: the hub is closed, no reader will
/// take the entry, and the answer says exactly that rather than claiming a delivery.
///
/// The third case is reached with the child's turn HELD OPEN by the stub, because a child
/// whose hub closes while it is between turns ends its thread on the way out — which is the
/// second refusal, not the third. Both are refusals by name; this pins which is which.
#[test]
fn a_message_to_a_child_that_is_gone_is_refused_by_name() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-message-gone");
    let path = dir.path().join("sessions.db");
    let parent_id = "s-parent-with-lost-children";

    let parts = parts_for(&config(parent_id, Endpoint::new("127.0.0.1", 1), &path));
    // Two children are spawned below, one turn each — and both are HELD, because the second
    // child's turn must still be running when its hub closes.
    let stub = Stub::held(
        vec![
            a_plain_answer_turn(&parts.vocab, "planning", TO_THE_TASK, 30),
            a_plain_answer_turn(&parts.vocab, "planning", TO_THE_TASK, 30),
        ],
        2,
    );

    let cfg = config(parent_id, stub.endpoint.clone(), &path);
    let parent = a_parent(parent_id, &parts, &cfg);

    // **No such handle.** Nothing was sent and the refusal says which ones there are.
    let said = parent
        .message("sub-never-existed", MESSAGE)
        .expect_err("a handle this session never minted is refused");
    assert!(
        said.contains("no subagent") && said.contains("task_result"),
        "the refusal names the handle and where the list is: {said}"
    );

    // **A session that is over.** A stop reaching a child BETWEEN turns ends its thread
    // (`serve_child`'s stop arm), which is the state a message can no longer reach.
    let handle = parent.spawn("do the task, then stop and wait");
    let child = parent.hub_of(&handle);
    stub.release(1);
    parent.wait_until_parked(&handle);
    let killed = child.submit(
        DAEMON_SUBMITTER,
        "job_kill-1",
        0,
        CommandKind::Interrupt {
            reason: "the test stopped this child".into(),
        },
    );
    assert!(
        matches!(killed, letibot_sessionlog::ServerFrame::Accepted { .. }),
        "the stop was not accepted: {killed:?}"
    );
    // The thread ends a moment after the stop reaches it, so this waits for the fact rather
    // than assuming it.
    let began = Instant::now();
    let said = loop {
        match parent.message(&handle, MESSAGE) {
            Err(why) if why.contains("thread has ended") => break why,
            other => {
                assert!(
                    began.elapsed() < PATIENCE,
                    "a message to a child whose thread has ended was not refused by name: \
                     {other:?}"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    };
    assert!(
        said.contains("Nothing was sent"),
        "a refusal may not claim to have queued: {said}"
    );

    // **And a session that goes away with the message already queued** — the one window the
    // new order cannot close. The child is mid-turn, so its thread is still alive and the
    // refusal is about the SESSION rather than about the thread.
    let handle = parent.spawn("a second child, to be lost");
    let child = parent.hub_of(&handle);
    assert!(
        stub.wait_for_requests(2),
        "the second child never sent its first round"
    );
    assert!(child.status().running, "the child must be mid-turn here");
    child.close();
    let said = parent
        .message(&handle, MESSAGE)
        .expect_err("a closed session has no reader, so this is not a delivery");
    assert!(
        said.contains("no turn will be started for it") && said.contains("Do not read this as"),
        "the window between the submit and the wake is named, not hidden: {said}"
    );
}

// ---------------------------------------------------------------------------
// The daemon's own arm
// ---------------------------------------------------------------------------

/// **The daemon's worker hands a child's message back to the child's own reader.**
///
/// Both readers take from ONE queue (`Hub::take_own_work` for the thread that runs the
/// session, `Hub::try_command` for the worker), so the worker can win a message a parent
/// aimed at its child — and its answer would be `message_idle`, *"the turn it was meant to
/// steer had ended, so nothing will deliver it"*, which is false the moment a queued message
/// starts a turn. So it goes back to that thread, the same door an interrupt takes.
///
/// A unit on the arm, and it is here rather than in the child's file because it needs no
/// child: what it proves is the routing, and the end-to-end proof that the routing is RIGHT
/// is the two tests above.
#[test]
fn the_workers_arm_gives_a_childs_message_back_to_its_reader() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-message-dispatch");
    let path = dir.path().join("sessions.db");
    let parent_id = "s-parent-dispatching";

    let cfg = config(parent_id, Endpoint::new("127.0.0.1", 1), &path);
    let parts = parts_for(&cfg);
    let registry = Registry::new();
    // The daemon's own session, so `Sessions` exists at all — the child is a session it does
    // NOT hold, which is the whole condition this arm turns on.
    registry
        .create(parent_id, "the parent", wiring(&cfg))
        .expect("the parent is created");
    let mut sessions =
        Sessions::open_first(&parts, cfg.clone(), registry.clone()).expect("the daemon opens");

    let child = "s-child-not-held-by-the-daemon";
    let child_hub = registry.new_hub(child);
    registry
        .adopt(
            child_hub.clone(),
            child,
            wiring(&cfg),
            Some(parent_id.to_string()),
        )
        .expect("the child is adopted");

    // Exactly what the parent's `task_message` submits, and exactly how the worker takes it.
    let frame = child_hub.submit(
        DAEMON_SUBMITTER,
        "task_message-1",
        0,
        CommandKind::Message {
            from: parent_id.into(),
            text: MESSAGE.into(),
        },
    );
    assert!(
        matches!(frame, letibot_sessionlog::ServerFrame::Accepted { .. }),
        "the message was not accepted: {frame:?}"
    );
    let cmd: QueuedCommand = child_hub
        .try_command()
        .expect("the worker takes the command off the child's queue");

    let outcome = sessions.dispatch(child, &cmd);
    assert!(
        matches!(outcome, Outcome::HandedOn),
        "the worker ran the message itself instead of handing it to the child's own reader"
    );
    let back = child_hub
        .try_command()
        .expect("the message is back in front of the reader that can run it");
    assert!(
        matches!(back.kind, CommandKind::Message { .. }),
        "and it is the message that went back, not something else: {back:?}"
    );
    assert!(
        !warnings(&child_hub)
            .iter()
            .any(|(code, _)| code == "message_idle"),
        "the worker published the sentence a queued message makes false: {:#?}",
        warnings(&child_hub)
    );
}
