//! **One channel between sessions, with three callers** — the operator's ruling, verbatim:
//! *"well gatekeeper is not a child so we are not doing up"*.
//!
//! So nothing "up" is built. The channel is **lateral — one session sends another session a
//! message** — and `CommandKind::Message { from, text }` was already exactly that. What was
//! missing was two things, and only one of them was behaviour:
//!
//! * **the sender's SENTENCE described the door as a parent's list of its children.**
//!   `HarnessTaskRunner::send` looks its recipient up with `Registry::get`, which is the
//!   daemon's registry keyed by session id — so the *lookup* was never child-scoped and a peer
//!   was never refused for being a peer. The wording was child-shaped, and the tool's
//!   description with it, so a session could not tell it was allowed to name one.
//! * **the delivery arm knew only how to hand a message to a session with its OWN reader.**
//!   The **main session is `open`** — the worker owns it — so a message addressed to it skipped
//!   the hand-back, fell through to `message_idle` (*"that session is gone, so nothing will
//!   deliver it"*) and was dropped, in precisely the case the design needs. That one is
//!   behaviour, and it is what most of this file measures.
//!
//! # What is proved here, and through what
//!
//! The real chain, not a unit on the arm: two REAL sessions in one daemon's registry, the
//! sender's own `task_message` through that session's own tool runtime, and the delivery
//! through `Sessions::dispatch` — the daemon worker's own door — with the model replaced by a
//! stub. What is asserted is on the RECIPIENT: its log holds the sender's words as
//! `Speaker::Agent` (never the operator's, which is the whole reason a message is not a
//! prompt), its turn RAN, and the prompt that turn was handed carries the message.
//!
//! # The three callers this channel has, and where each is proved
//!
//! 1. **the tutor raising to the father** — a peer session sending the session that hosts the
//!    work a message. Wired, and pinned by `a_session_may_send_a_peer_it_names`: the door takes
//!    any live session id. That test PINS a claim rather than fixing one — the lookup was
//!    already the daemon's registry, so it passes on the old code too; what the change fixes
//!    there is the sentence and the tool's description. What is NOT yet reachable is the
//!    *address*: a gatekeeper child is told nothing about the session it reviews for, so no
//!    model can name it yet.
//! 2. **the gatekeeper sending a kid back to work** — a refusing verdict, delivered to
//!    `entry.id` by `Harness::serve_reviews`. Proved end to end by
//!    `a_refusing_verdict_goes_back_to_the_child_that_did_the_work`.
//! 3. **a notes keeper reporting what it could not settle** — not wired. It is the same two
//!    acts (a session names a peer, the peer's turn starts), and nothing new is needed for it.
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
use letibot_sessionlog::hub::{Hub, QueuedCommand};
use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_tokencore::Vocab;
use letibot_tokencore::store::{MergeEntry, MergePriority, MergeState, ReviewRecord, Store};
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

/// What the sender says. A marker, so it can be found in a row and in a prompt.
const MESSAGE: &str = "the branch is not what the brief asked for";

/// The child's own answers, and the reviewer's verdict, so the test can say WHICH turn a row
/// came from.
const TO_THE_TASK: &str = "the work is done, waiting";
const TO_THE_PEER: &str = "understood, I am on it";
const REFUSAL: &str = "the change never touches the file the brief names";

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
    // `task_result`, `task_message`. The default seat is `orchestrator`, which seats none of
    // them, and a test that invoked a tool the session does not seat would be measuring the
    // seat table rather than this door.
    cfg.seat = Seat::Coder;
    cfg.endpoint = endpoint;
    cfg.http_retries = 0;
    cfg.context_window = Some(32_768);
    cfg.store = Some(store.to_path_buf());
    cfg.session_id = session_id.to_string();
    // The operator's own `providers.toml` is not this test's business (`wired.rs`'s rule).
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

/// **One session of this daemon, as a peer of any other** — its own hub in the shared
/// registry, its own harness, and no tree edge to the session it will talk to. This is the
/// shape the operator's correction names: a session in its own right, not a child.
struct Session {
    harness: Harness,
}

fn a_session(session_id: &str, parts: &Parts, cfg: &Config, registry: Arc<Registry>) -> Session {
    let mut mine = cfg.clone();
    mine.session_id = session_id.to_string();
    // **A peer, not a child**: `parent_session_id` is what makes a session a subagent, and the
    // whole point of this channel is that neither end of it has to be one.
    mine.parent_session_id = None;
    let hub = registry.new_hub(session_id.to_string());
    registry
        .adopt(hub.clone(), session_id.to_string(), wiring(cfg), None)
        .expect("the peer is in the registry");
    let harness = Harness::open_with_registry(parts, mine, hub, None, None, registry)
        .expect("the peer opens");
    Session { harness }
}

impl Session {
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

    /// Say something to a session, by id. The reply is the door's own words, which is half of
    /// what this file is about — a delivery that is not said is a delivery nobody can trust.
    fn message(&self, to: &str, text: &str) -> Result<String, String> {
        let r = self.call(
            "task_message",
            serde_json::json!({"task": to, "text": text}),
        );
        match r.outcome {
            letibot_transcript::ToolOutcome::Ok => Ok(r.payload),
            _ => Err(r.payload),
        }
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
    fn hub_of(&self, registry: &Registry, handle: &str) -> Arc<Hub> {
        let began = Instant::now();
        loop {
            if let Some(hub) = registry.get(handle) {
                return hub;
            }
            if began.elapsed() >= PATIENCE {
                panic!("`{handle}` was spawned and never entered the registry");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// **Wait for the child to be between turns** — its first turn's answer has landed and it
    /// is parked.
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

/// **A directory that removes itself**, because a test that leaks a store per run eventually
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
// The recipient's own log
// ---------------------------------------------------------------------------

/// Every user row on a session's log, as (speaker, text). This is the RECIPIENT's own log —
/// the thing its own agent reads and every head draws.
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

/// The rows on which the session heard `text` as another session's words.
fn heard(hub: &Hub, text: &str) -> Vec<String> {
    user_rows(hub)
        .into_iter()
        .filter(|(speaker, said)| *speaker == Speaker::Agent && said.contains(text))
        .map(|(_, said)| said)
        .collect()
}

/// The assistant's own answers, which is how a turn that RAN is told from one merely queued.
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

/// Every warning on a session's log, as (code, detail) — where a lost message, a failed turn
/// and the worker's `message_idle` all land.
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
fn wait_for(hub: &Hub, mut what: impl FnMut(&Hub) -> Option<String>) -> String {
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
/// `canned::Canned` answers as fast as it can, which closes the window one test here has to
/// reach into: a turn is mid-flight only for as long as its model takes to answer. So each
/// request is read, RECORDED, and held until the test releases it — *"the recipient is in a
/// turn"* becomes a state the test stands in rather than races. The recorded body is what
/// makes the other half provable: a request carries token ids, and detokenized with the same
/// vocabulary the session tokenized with, they are the prompt the model was handed.
///
/// **`/props` is answered and does not consume a script.** A spawn asks the parent's endpoint
/// for its window (`HarnessTaskRunner::start`'s `served_ctx`), and counting that as a turn
/// would shift every script by one. (`message_between_turns.rs`'s `Stub` is the same fixture
/// with the recording this file's third caller needs; the reader is copied, as
/// `model_change_mid_retry.rs`'s is.)
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
    bodies: Vec<String>,
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
                let Ok((stream, _)) = listener.accept() else {
                    return;
                };
                let (line, body) = read_request(stream.try_clone().expect("clone the socket"));
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

    /// Wait until at least `n` requests have arrived. `false` is the timeout, so a caller can
    /// say what it was waiting for rather than hang.
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
    /// vocabulary the session tokenized with.
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
        // Release anything still held, then detach: a test that failed early must not leave a
        // thread parked on the gate for the rest of the suite.
        self.release(usize::MAX);
        if let Some(h) = self.handle.take() {
            drop(h);
        }
    }
}

/// Drain one request — headers plus the body `Content-Length` names — so the client's write
/// side is finished before the answer arrives. It hands back the request line as well so the
/// caller can tell a `/props` probe from a turn.
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

/// **The endpoint's own props**, as `served_ctx` reads them: the one field a spawn asks for is
/// the window, and a server that answers nothing gets `None`, which is no wall at all.
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

/// A turn that calls ONE tool and ends — so the turn has a round boundary, which is the only
/// place a turn in flight can be told anything.
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
        n_decoded: (call_ids.len() + 4) as u64,
        n_prompt,
        cache_n: 0,
    });
    frames
}

/// The `read` call the first round makes, in the Qwen dialect's own shape.
fn read_call(path: &str) -> String {
    format!("\n<function=read>\n<parameter=path>\n{path}\n</parameter>\n</function>\n")
}

/// **The reviewer's own reply**, in the closing block the gatekeeper protocol asks for: a
/// refusal with one reason, which is the complaint this channel has to carry.
fn a_refusing_verdict() -> String {
    format!(
        "I read the diff against the brief.\n\nverdict: reject\nreasons:\n- {REFUSAL}\nfiles: none\ncommands: none\n"
    )
}

// ---------------------------------------------------------------------------
// 1. The arm: an IDLE session the worker owns
// ---------------------------------------------------------------------------

/// **A peer's message to the session the WORKER owns is a row the agent reads, and the turn
/// it makes.**
///
/// The operator's precedent is their own `!` lines, in their words: *"my commands should start
/// a turn and should be printed to me"*. A message is that pair of acts for the same reason —
/// the deposit is a row on the session's own log, and the turn is what makes it an utterance
/// somebody answered. Before this, a message to the main session skipped the hand-back (the
/// main session is `open`, so it has no reader of its own to hand anything to), fell through
/// to `message_idle`, and was dropped — in exactly the case the design needs.
///
/// The sender is a **peer**: a session of its own, seated in the same daemon, with no tree
/// edge to the recipient. That is the operator's correction, in their words: *"well gatekeeper
/// is not a child so we are not doing up"*.
#[test]
fn a_peers_message_to_an_idle_worker_owned_session_is_a_row_and_a_turn() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-session-channel-idle");
    let path = dir.path().join("sessions.db");
    let main_id = "s-father";
    let peer_id = "s-tutor";

    let parts = parts_for(&config(main_id, Endpoint::new("127.0.0.1", 1), &path));
    // One request: the turn the message starts. A second would be a turn nothing asked for,
    // and it would arrive as a socket error rather than silence.
    let turn = a_plain_answer_turn(&parts.vocab, "hearing", TO_THE_PEER, 30);
    let stub = Stub::answering(vec![turn], 1);

    let cfg = config(main_id, stub.endpoint.clone(), &path);
    let registry = Registry::new();
    registry
        .create(main_id, "the father", wiring(&cfg))
        .expect("the father is created");
    let mut sessions =
        Sessions::open_first(&parts, cfg.clone(), registry.clone()).expect("the daemon opens");
    let main_hub = registry.get(main_id).expect("the father's hub");

    let peer = a_session(peer_id, &parts, &cfg, registry.clone());

    // **The sender's own door**: the peer names the session it wants to talk to. This is
    // `task_message` — the one sender there is — and nothing about it is parent-shaped.
    let said = peer
        .message(main_id, MESSAGE)
        .expect("a session may send a peer a message");
    assert!(
        said.contains(main_id),
        "the answer names the session it was told as: {said}"
    );
    assert!(
        !said.contains("no subagent"),
        "the child-only refusal must not fire for a peer: {said}"
    );

    // **The worker's own door**, exactly as the daemon takes it: the message is on the
    // recipient's queue, and dispatching it is what delivers it.
    let cmd: QueuedCommand = main_hub
        .try_command()
        .expect("the message is on the recipient's queue");
    let outcome = sessions.dispatch(main_id, &cmd);
    match outcome {
        Outcome::Replied(_) => {}
        Outcome::Failed(why) => panic!("the turn the message makes failed: {why}"),
        Outcome::HandedOn => panic!(
            "the worker handed a message for a session IT HOLDS to somebody else's reader — \
             which is the arm this test exists for"
        ),
        // `Ignored` is the defect in one word: the deposit would sit in the transcript until
        // something else started a turn, which is a command nobody answered.
        _ => panic!("the worker did not run the turn a message to a session it owns makes"),
    }

    // **The row**, as the recipient's own agent reads it: the peer's words, as an agent's —
    // never the operator's, which is the whole reason a message is not a prompt.
    let rows = heard(&main_hub, MESSAGE);
    assert_eq!(
        rows.len(),
        1,
        "the recipient heard the message exactly once: {rows:#?}"
    );
    assert!(
        rows[0].contains(peer_id),
        "the sender is NAMED in the words the recipient reads, which is what lets it judge \
         provenance: {}",
        rows[0]
    );
    assert!(
        !user_rows(&main_hub)
            .iter()
            .any(|(s, t)| *s == Speaker::Operator && t.contains(MESSAGE)),
        "a message must not be recorded as something the operator typed"
    );

    // **And the turn ran.** A row with no turn behind it is a note nobody answered.
    let answered = wait_for(&main_hub, |hub| {
        answers(hub).into_iter().find(|a| a.contains(TO_THE_PEER))
    });
    assert!(answered.contains(TO_THE_PEER), "{answered}");

    // **And the prompt that turn was handed says so too** — the row is the session's record;
    // the request is what the model actually read.
    let prompt = stub.prompt_of(0, &parts.vocab);
    assert!(
        prompt.contains(MESSAGE),
        "the message never reached the prompt of the turn it started: {prompt}"
    );
    assert!(
        !warnings(&main_hub)
            .iter()
            .any(|(code, _)| code == "message_idle"),
        "the sentence a delivered message makes false was published: {:#?}",
        warnings(&main_hub)
    );

    registry.close();
}

// ---------------------------------------------------------------------------
// 2. The arm: a session that is BUSY
// ---------------------------------------------------------------------------

/// **A peer's message to a session whose turn is ALREADY RUNNING lands in that turn.**
///
/// The other half, and the half that must not regress into a second turn: a running turn
/// takes a message at its next round boundary through its own steering poll, as the sender's
/// words, and keeps working. The turn is held open by the stub, so "already running" is a fact
/// the test stands in rather than races, and the proof that the message landed IN that turn is
/// the next round's prompt — a turn that had ended would have answered it in a turn of its
/// own, and this request would not carry it.
#[test]
fn a_peers_message_to_a_busy_worker_owned_session_lands_at_the_round_boundary() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-session-channel-busy");
    let path = dir.path().join("sessions.db");
    let main_id = "s-father-at-work";
    let peer_id = "s-tutor-nagging";

    // The file the first round reads — a relative path, because the session root is `/tmp`
    // and `read` is confined to it.
    std::fs::write(dir.path().join("note.txt"), "the plan, in one line\n")
        .expect("the file to read is written");
    let target = format!("{}/note.txt", dir.relative());

    let parts = parts_for(&config(main_id, Endpoint::new("127.0.0.1", 1), &path));
    // Round zero calls a tool, so the turn HAS a round boundary; round one answers. Held:
    // nothing is answered until this test says so, which is what makes the recipient reliably
    // mid-turn when the message arrives.
    let round0 = a_tool_call_turn(&parts.vocab, &read_call(&target), 30);
    let round1 = a_plain_answer_turn(&parts.vocab, "hearing", TO_THE_PEER, 30);
    let stub = Stub::held(vec![round0, round1], 2);

    let cfg = config(main_id, stub.endpoint.clone(), &path);
    let registry = Registry::new();
    registry
        .create(main_id, "the father at work", wiring(&cfg))
        .expect("the father is created");
    let mut sessions =
        Sessions::open_first(&parts, cfg.clone(), registry.clone()).expect("the daemon opens");
    let main_hub = registry.get(main_id).expect("the father's hub");

    let peer = a_session(peer_id, &parts, &cfg, registry.clone());

    // **The turn is driven on a thread of its own**, because the worker is INSIDE a turn when
    // this test wants to speak to it — which is the whole state being measured. `Sessions` is
    // borrowed for the length of the turn and nothing else touches it.
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let _ = sessions.submit(main_id, "read the note, then answer");
        });
        assert!(
            stub.wait_for_requests(1),
            "the recipient's first round was never sent"
        );
        assert!(
            main_hub.status().running,
            "the fixture must stand in the state this test is about: the turn is running"
        );

        let said = peer
            .message(main_id, MESSAGE)
            .expect("a message to a running session is accepted");
        assert!(
            said.contains("turn is running") && said.contains("round boundary"),
            "the answer must say this was the RUNNING delivery: {said}"
        );

        // Let round zero's tool call through: the recipient reads the file, and the boundary
        // after it is where the message can be heard. **Then let round ONE through too**, and
        // that is not tidiness: the scope below JOINS the thread running the turn, so a round
        // nothing answers holds the join for the client's read timeout — and that timeout is
        // `crates/http/src/lib.rs`'s `read_timeout: Duration::from_secs(180)`. MEASURED: this
        // file's 183.71s was this one test waiting out those three minutes, with every
        // assertion below already passed, while the other four measured 0.65-0.80s each.
        // Releasing it changes nothing about what is proved: the second round's request has
        // already ARRIVED, which is what carries the message.
        stub.release(2);
        assert!(
            stub.wait_for_requests(2),
            "the session never reached a second round — the message had no boundary to arrive at"
        );
    });

    let second = stub.prompt_of(1, &parts.vocab);
    assert!(
        second.contains(MESSAGE),
        "the message did not land in the running turn: {second}"
    );
    assert!(
        second.contains(peer_id),
        "and the sender is named in the words the turn was handed: {second}"
    );
    // **And no third turn.** The message was taken by the turn it was aimed at, so nothing is
    // left for the worker to dispatch, and the stub served exactly the two rounds.
    assert_eq!(
        stub.bodies().len(),
        2,
        "the message started a turn of its own instead of landing in the running one"
    );
    assert!(
        main_hub.try_command().is_none(),
        "the message is still queued: the running turn did not take it"
    );
    assert!(
        !warnings(&main_hub)
            .iter()
            .any(|(code, _)| code == "message_idle"),
        "the worker's sentence was published for a message that was delivered: {:#?}",
        warnings(&main_hub)
    );

    registry.close();
}

// ---------------------------------------------------------------------------
// 3. The sender: a session that does not exist
// ---------------------------------------------------------------------------

/// **A message to a session that does not exist is refused by name, and nothing is sent.**
///
/// The refusal narrowed to the one case that is a refusal. A live session — a subagent this
/// session started, or a peer — is delivered to; a handle no session answers to is not, and
/// the sentence says so rather than naming the caller's children as the only addressees.
#[test]
fn a_message_to_a_session_that_does_not_exist_is_refused_by_name() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-session-channel-nobody");
    let path = dir.path().join("sessions.db");
    let me_id = "s-somebody";

    let cfg = config(me_id, Endpoint::new("127.0.0.1", 1), &path);
    let parts = parts_for(&cfg);
    let registry = Registry::new();
    let me = a_session(me_id, &parts, &cfg, registry.clone());

    let said = me
        .message("s-nobody-at-all", MESSAGE)
        .expect_err("a session nobody holds is refused");
    assert!(
        said.contains("s-nobody-at-all"),
        "the refusal names the session that is not there: {said}"
    );
    assert!(
        said.contains("Nothing was sent"),
        "a refusal may not claim to have sent anything: {said}"
    );
    assert!(
        said.contains("live session"),
        "and it says what a handle MAY name, or the door is undiscoverable: {said}"
    );
    assert!(
        !said.contains("no subagent"),
        "the refusal is about sessions, not about this session's children: {said}"
    );

    registry.close();
}

// ---------------------------------------------------------------------------
// 4. The sender: a peer, which is nobody's child here
// ---------------------------------------------------------------------------

/// **A session may send a peer it names** — the child-only refusal no longer fires for a
/// session that is not the caller's child.
///
/// The narrow claim, on the door rather than on the arm: the recipient is a ROOT session, in
/// the same daemon, that this session did not start and has no tree edge to. The message is on
/// its queue, carrying the sender's own id as `from`, which is what the recipient judges
/// provenance by.
///
/// **This pins a claim rather than fixing one, and saying so is the point.** `send` resolves
/// through `Registry::get`, which was never child-scoped, so this test passes against the old
/// sentence too — the *lookup* was right and the *words* were wrong. What the change fixes at
/// this door is the wording (`no session \`x\` is live in this daemon` in place of *no subagent
/// in this session*) and the tool's own description, so that a session can tell it may name a
/// peer. The behaviour this file exists for is the arm, above.
#[test]
fn a_session_may_send_a_peer_it_names() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-session-channel-peer");
    let path = dir.path().join("sessions.db");
    let me_id = "s-the-tutor";
    let peer_id = "s-the-father";

    let cfg = config(me_id, Endpoint::new("127.0.0.1", 1), &path);
    let parts = parts_for(&cfg);
    let registry = Registry::new();
    let me = a_session(me_id, &parts, &cfg, registry.clone());

    // **The peer, and nothing else**: a hub of its own in the same registry, no harness, no
    // parent, nobody's child. A message to it has nowhere to be delivered except its queue,
    // which is exactly what makes the queue the thing to assert on.
    let peer_hub = registry.new_hub(peer_id.to_string());
    registry
        .adopt(peer_hub.clone(), peer_id.to_string(), wiring(&cfg), None)
        .expect("the peer is in the registry");

    let said = me
        .message(peer_id, MESSAGE)
        .expect("a session may send a peer a message");
    assert!(
        said.contains(peer_id) && said.contains(me_id),
        "the answer says who was told, as whom: {said}"
    );

    let queued: QueuedCommand = peer_hub
        .try_command()
        .expect("the message is on the peer's queue");
    match queued.kind {
        letibot_sessionlog::CommandKind::Message { from, text } => {
            assert_eq!(from, me_id, "the sender is recorded, not the daemon");
            assert_eq!(text, MESSAGE);
        }
        other => panic!("the peer's queue holds something else: {other:?}"),
    }

    registry.close();
}

// ---------------------------------------------------------------------------
// 5. The second caller: a refusing verdict, back to the child
// ---------------------------------------------------------------------------

/// **A refusing verdict goes back to the child that did the work, through the same channel.**
///
/// The operator's model of this queue, in their words: *"gatekeeper reviews and drives
/// subagents to completion by nagging them via messages. so it gets queue item - reviews if ok
/// - puts into merge queue ... and if it is not happy - it sends a message with complaints to
/// original subagent"*. This is the refusing half, end to end and with no socket: a REAL
/// child (spawned through `task`, its own thread and its own hub), a real gatekeeper child
/// answering with a rejecting verdict, `Harness::serve_reviews` writing it, and the complaints
/// arriving on the CHILD's log as another session's words — with the child's turn running on
/// them.
///
/// **The recipient is `entry.id`, and not the review row's `session_id`.** The review's own
/// `session_id` is the session that HOSTED the review — the one whose agent was told to judge
/// — while the entry's `id` is the session that did the work. Delivery uses the second; the
/// first would send the reviewer its own verdict.
#[test]
fn a_refusing_verdict_goes_back_to_the_child_that_did_the_work() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-session-channel-refusal");
    let path = dir.path().join("sessions.db");
    let main_id = "s-father-of-the-branch";

    let parts = parts_for(&config(main_id, Endpoint::new("127.0.0.1", 1), &path));
    // Three turns, in the order they happen: the child's work, the gatekeeper's verdict, and
    // the child's turn on the complaints.
    let work = a_plain_answer_turn(&parts.vocab, "working", TO_THE_TASK, 30);
    let verdict = a_plain_answer_turn(&parts.vocab, "reviewing", &a_refusing_verdict(), 30);
    let hearing = a_plain_answer_turn(&parts.vocab, "hearing", TO_THE_PEER, 30);
    let stub = Stub::answering(vec![work, verdict, hearing], 3);

    let cfg = config(main_id, stub.endpoint.clone(), &path);
    let registry = Registry::new();
    let mut father = a_session(main_id, &parts, &cfg, registry.clone());

    // **The child that did the work** — a real one, through the real `task` tool, so it has a
    // session of its own to be sent back to.
    let child_id = father.spawn("do the work, then stop and wait");
    let child_hub = father.hub_of(&registry, &child_id);
    father.wait_until_parked(&child_id);

    // **The queue's two rows**: an entry whose id IS the child, and the review row the
    // verdict will be written onto.
    let branch = "agent/the-work".to_string();
    let base_sha = "0".repeat(40);
    let store = Store::open(&path).expect("the store");
    store
        .put_merge_entry(&MergeEntry {
            id: child_id.clone(),
            session_id: child_id.clone(),
            branch: branch.clone(),
            base_sha: base_sha.clone(),
            priority: MergePriority::Subagent,
            needs: Vec::new(),
            state: MergeState::Waiting,
            brief: "do the work the brief names".into(),
            evidence: String::new(),
            created_ms: 1,
            updated_ms: 1,
            worktree: Some(dir.relative()),
            landed_sha: None,
        })
        .expect("the entry");
    store
        .put_review(&ReviewRecord {
            entry_id: child_id.clone(),
            // The session that HOSTED the review, which is not the child.
            session_id: main_id.to_string(),
            branch: branch.clone(),
            base_sha: base_sha.clone(),
            asked_ms: 1,
            answered_ms: None,
            decision: None,
            attempts: 0,
            failed_ms: None,
            failure: String::new(),
            reasons: Vec::new(),
            files: Vec::new(),
            commands: Vec::new(),
        })
        .expect("the review row");

    // **The daemon's own pass.** One call starts the gatekeeper; the next writes its verdict
    // and sends the complaints, so this drives it until the child has heard them.
    let heard_it = wait_for(&child_hub, |hub| {
        father.harness.serve_reviews();
        heard(hub, REFUSAL).into_iter().next()
    });
    assert!(
        heard_it.contains(REFUSAL),
        "the child was not told what the review said: {heard_it}"
    );
    assert!(
        heard_it.contains(&format!("message from your parent session `{main_id}`")),
        "and the message is the same one every door hands a session, with the sender named: \
         {heard_it}"
    );
    assert!(
        !user_rows(&child_hub)
            .iter()
            .any(|(s, t)| *s == Speaker::Operator && t.contains(REFUSAL)),
        "the complaints must not be recorded as something the operator typed"
    );

    // **And the child's turn RAN on them** — the ball went back, it was not merely filed.
    let answered = wait_for(&child_hub, |hub| {
        answers(hub).into_iter().find(|a| a.contains(TO_THE_PEER))
    });
    assert!(answered.contains(TO_THE_PEER), "{answered}");

    // **And the verdict is on the row**, where the queue reads it.
    let row = store
        .merge_review(&child_id)
        .expect("the review row reads back")
        .expect("the review row is there");
    assert_eq!(row.decision.as_deref(), Some("reject"), "{row:?}");
    assert!(
        row.reasons.iter().any(|r| r.contains(REFUSAL)),
        "the reviewer's reasons are on the row: {row:?}"
    );
    assert!(
        !warnings(father.harness.hub())
            .iter()
            .any(|(code, _)| code == "refusal_undelivered"),
        "the complaints were delivered, so nothing should say they were not: {:#?}",
        warnings(father.harness.hub())
    );

    registry.close();
}
