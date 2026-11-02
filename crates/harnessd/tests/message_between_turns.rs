//! A message to a child between turns starts a turn for it: the message goes on the child's
//! queue, the child's own serving thread is woken, and the loop that drains that queue
//! answers a `Message` by RUNNING THE TURN that reads it (`serve_child`'s
//! `ChildCommand::Hear` → `Harness::submit_a_parents_message`). Without that, a submission
//! against a parked child answered `Accepted` and sat there, so a parent believed it had
//! corrected a child that never heard a word. The chain proved here is the real one: a
//! parent [`Harness`] with the REAL runner, `task` and `task_message` invoked through that
//! session's own tool runtime.

use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use letibot_harnessd::config::{Config, Seat};
use letibot_harnessd::sessions::{Outcome, Sessions, SubagentExit};
use letibot_harnessd::{Dialect, Harness, Parts};
use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::hub::{CommandKind, DAEMON_SUBMITTER, Hub, QueuedCommand};
use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_tokencore::Vocab;
use letibot_tokencore::store::Store;
use letibot_tools::NullToolSink;
use letibot_transcript::{Speaker, ToolCall, TranscriptItem, UserPart};
use letibot_turn::Endpoint;

mod support;

use support::canned;
use support::canned::Frame;
use support::qwen::{THINK_CLOSE, a_plain_answer_turn, ids, spoken};
use support::stub::{read_request, write_frames, write_props};

/// Qwen's own ids, the same values the turn crate's canned tests pin.
const THINK_OPEN: u32 = 248068;
const TOOL_CALL_OPEN: u32 = 248058;
const TOOL_CALL_CLOSE: u32 = 248059;

/// What the parent says. A marker, so it can be found in a row and in a prompt.
const MESSAGE: &str = "stop and report what you have";

/// The child's answer to it, and to the task before it - two sentences so a row can say WHICH
/// turn it came from.
const TO_THE_TASK: &str = "the task is done, waiting";
const TO_THE_PARENT: &str = "understood, reporting now";

/// How long the driver waits for something a correct build produces in milliseconds - a
/// ceiling on *the far side never ended*, never a claim about speed. A child's OPEN (a
/// vocabulary, a composed prompt, a store) is the slowest thing any wait here covers.
const PATIENCE: Duration = Duration::from_secs(120);

/// The bound a test GIVES a drain when the question is whether the drain spends it. The claim
/// below is *it returned without waiting*, so the reference is the bound and not a clock: only a
/// drain that waits can reach 60 s.
const GRACE: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------------------
// The fixtures
// ---------------------------------------------------------------------------

fn config(session_id: &str, endpoint: Endpoint, store: &Path) -> Config {
    let mut cfg = Config::for_this_box("/tmp");
    cfg.dialect = Dialect::Qwen;
    // `leticode` because it is the UNCONFINED seat that has the delegation trio -
    // `task`, `task_result`, `task_message`. The default seat seats none of them, and the
    // confined seats cannot open on a box with no boundary; nothing here asserts one.
    cfg.seat = Seat::Leticode;
    cfg.endpoint = endpoint;
    // No retry ladder: nothing here should fail, and a failure that retried would spend
    // the suite's time hiding behind doubling waits.
    cfg.http_retries = 0;
    // A window, because a child is planned against one. A spawn with no window is
    // refused by name (`nothing was spawned on `local`: nobody can name a window for it`)
    // and the refusal is right: a child with no window has nothing to compact it.
    cfg.context_window = Some(32_768);
    cfg.store = Some(store.to_path_buf());
    cfg.session_id = session_id.to_string();
    // The operator's own `providers.toml` is not this test's business (`wired.rs`'s rule)
    // without these, a search key on the developer's laptop decides what is seated.
    cfg.web_search = None;
    cfg.permission = Vec::new();
    cfg.vocab_gguf = support::vocab::gguf();
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

/// A parent session with the REAL runner - the whole production path, with the model
/// replaced by the stub, and its own hub adopted so the tree looks the way a daemon's does.
struct Parent {
    harness: Harness,
    registry: Arc<Registry>,
    gate: Arc<Stub>,
    children: Mutex<Vec<String>>,
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
        children: Mutex::new(Vec::new()),
    }
}

impl Parent {
    /// One tool call, as the engine would make it - through the session's own runtime, so
    /// this is the production door rather than a call on the runner.
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
            letibot_transcript::ToolOutcome::Backgrounded { handle, .. } => {
                // Recorded, so the teardown can wait for this child's thread - nothing
                // else in the process can, because the thread is spawned detached.
                self.children.lock().expect("children").push(handle.clone());
                handle
            }
            other => panic!("`task` did not start a child: {other:?} — {}", r.payload),
        }
    }

    /// The child's hub, once its own thread has adopted it - a spawn returns as soon as the
    /// thread is started, and the session enters the registry only after the child's harness
    /// opens, so this waits for the fact rather than assuming it.
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

    /// Tell it something. The reply is the runner's own words - a delivery that is not said is a
    /// delivery the parent cannot trust.
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

    /// Wait for the child to be between turns - its first turn's answer has landed and
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

/// The tree is DRAINED at the end of a test, and that is a crash fix rather than tidiness.
impl Drop for Parent {
    fn drop(&mut self) {
        self.gate.release(usize::MAX);
        self.registry.close();
        if !self.harness.wait_for_children(PATIENCE) && !std::thread::panicking() {
            panic!(
                "the tree still had a subagent thread after {PATIENCE:?} of waiting, with its \
                 hubs closed. The process must not exit with one: `exit()` finalizes \
                 libllama/libggml-cuda underneath it, and the last time this binary did that \
                 it died of SIGSEGV after every test had passed."
            );
        }
    }
}

// ---------------------------------------------------------------------------
// The child's own log
// ---------------------------------------------------------------------------

/// Every user row on a session's log, as (speaker, text) - the child's OWN log, which its parent
/// and the operator both read.
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

/// The assistant's own answers, which is how a turn that RAN is told from one that was merely
/// queued.
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

/// Every warning on a session's log, as (code, detail) - where a lost message, a failed
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

/// Every user row in a session's STORE, as (speaker, text) - the half a later reader (a
/// resume, another daemon, a `transcript_item` query) sees, where [`user_rows`] is this
/// process's view.
fn stored_user_rows(path: &Path, session: &str) -> Vec<(Speaker, String)> {
    let store = Store::open(path).expect("the store opens");
    let Some(transcript) = store
        .current_transcript_id(session)
        .expect("the transcript id")
    else {
        return Vec::new();
    };
    store
        .load_transcript(&transcript)
        .expect("the transcript loads")
        .items
        .iter()
        .filter_map(|(item, _, _)| match item {
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
        })
        .collect()
}

/// Wait until `what` answers, or panic with the session's whole log - the log IS the
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

/// A model that answers on a gate, and keeps every request it was asked. `canned::Canned`
/// answers as fast as it can, which closes the window this file has to reach into.
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
    /// Which request is answered in halves and then dropped - `None` for an ordinary stub.
    cut_at: Option<usize>,
    /// Set by [`Stub::cut`]: the half-answered request may send its second half and go.
    cutting: bool,
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
        Stub::with_a_cut(scripts, requests, None)
    }

    /// One request is answered in halves and then dropped - see [`Inner::cut_at`]. Everything
    /// else is [`Stub::answering`], so the test only says *which* request dies.
    fn cutting(scripts: Vec<Vec<Frame>>, requests: usize, cut_at: usize) -> Stub {
        let stub = Stub::with_a_cut(scripts, requests, Some(cut_at));
        stub.release(requests);
        stub
    }

    /// The body both constructors share: `cut_at` decides whether one request is dropped.
    fn with_a_cut(scripts: Vec<Vec<Frame>>, requests: usize, cut_at: Option<usize>) -> Stub {
        let listener = TcpListener::bind("127.0.0.1:0").expect("an ephemeral port");
        let addr = listener.local_addr().unwrap();
        let shared = Arc::new(Gate {
            inner: Mutex::new(Inner {
                cut_at,
                ..Inner::default()
            }),
            cv: Condvar::new(),
        });
        let mine = shared.clone();
        let handle = std::thread::spawn(move || {
            let mut i = 0usize;
            while i < requests {
                // A listener that cannot accept is a listener nobody will be answered by
                // there is nothing left to say, and the client's own error is the report.
                let Ok((stream, _)) = listener.accept() else {
                    return;
                };
                let (line, body) = read_request(stream.try_clone().expect("clone the socket"));
                // `/props` is not a turn and does not consume a script. A spawn asks the
                // parent's endpoint for its window (`served_ctx`, one `GET` per child), and
                // counting that as a turn would shift every script by one.
                if line.starts_with("GET ") {
                    let _ = write_props(stream);
                    continue;
                }
                let cut = {
                    let mut g = mine.inner.lock().unwrap();
                    g.bodies.push(body);
                    mine.cv.notify_all();
                    while g.released <= i {
                        g = mine.cv.wait(g).unwrap();
                    }
                    g.cut_at == Some(i)
                };
                let frames = &scripts[i % scripts.len()];
                if cut {
                    let _ = write_frames_then_drop(stream, frames, &mine);
                } else {
                    let _ = write_frames(stream, frames);
                }
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

    /// Let the half-answered request send its second half and drop the connection. Called
    /// after the message has been put on the recipient's queue, so the token that carries the
    /// second half is the one whose steering poll absorbs it.
    fn cut(&self) {
        let mut g = self.shared.inner.lock().unwrap();
        g.cutting = true;
        self.shared.cv.notify_all();
    }

    /// What the model was handed for request `n` - the token ids, detokenized with the
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
        // Release anything still held, then detach: a test that failed early must not leave a
        // thread parked on the gate for the rest of the suite - the cut gate is opened for the
        // same reason.
        self.cut();
        self.release(usize::MAX);
        if let Some(h) = self.handle.take() {
            drop(h);
        }
    }
}

/// Answer with the first frame, wait, then send the rest and drop the socket - the shape of
/// a generation that dies mid-stream. Two details make it the state the test needs rather
/// than merely a failure
fn write_frames_then_drop(
    mut stream: TcpStream,
    frames: &[Frame],
    gate: &Arc<Gate>,
) -> std::io::Result<()> {
    stream.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
          Transfer-Encoding: chunked\r\n\r\n",
    )?;
    // The first frame alone: enough for the client to be inside the stream, not enough to be a
    // generation. `sse_body` appends the `0\r\n\r\n` terminator, which is the one thing this
    // must not send.
    let first = canned::sse_body(&frames[..1]);
    stream.write_all(&first[..first.len() - 5])?;
    stream.flush()?;
    // The test speaks now, and then says go.
    {
        let mut g = gate.inner.lock().unwrap();
        while !g.cutting {
            g = gate.cv.wait(g).unwrap();
        }
    }
    // Everything but the last two frames - the end-of-turn token and the `Final` - so the client
    // absorbs what is here, sees a token boundary to poll its steering queue at, and then loses
    // the connection.
    let rest = &frames[1..frames.len().saturating_sub(2).max(1)];
    let body = canned::sse_body(rest);
    stream.write_all(&body[..body.len() - 5])?;
    stream.flush()?;
    // Dropping the socket is the failure: a chunked body that never ends.
    drop(stream);
    Ok(())
}

/// A turn that calls ONE tool and ends - so the turn has a round boundary, which is the
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

/// A message to a child that is BETWEEN TURNS is heard: the child's own turn runs with it.
/// The old refusal's reason was that nothing drains a child's queue between turns, so the
/// state this test stands in is *the child is parked, nothing is running*.
#[test]
fn a_message_to_a_child_between_turns_starts_its_turn() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-message-between");
    let path = dir.path().join("sessions.db");
    let parent_id = "s-parent-who-corrects";

    // The vocabulary first - the canned frames must speak the ids the harness will send.
    let parts = parts_for(&config(parent_id, Endpoint::new("127.0.0.1", 1), &path));
    let first = a_plain_answer_turn(&parts.vocab, "planning", TO_THE_TASK, 30);
    let second = a_plain_answer_turn(&parts.vocab, "hearing", TO_THE_PARENT, 30);
    // Two requests: the task's own turn, and the turn the message starts. A third would be
    // a turn nothing asked for, and it would arrive as a socket error rather than silence.
    let stub = Arc::new(Stub::answering(vec![first, second], 2));

    let cfg = config(parent_id, stub.endpoint.clone(), &path);
    let parent = a_parent(parent_id, &parts, &cfg, stub.clone());
    let handle = parent.spawn("do the task, then stop and wait");
    let child = parent.hub_of(&handle);

    // A plain `task` child is named as `task_start` would name it - the slug derived from
    // its prompt - on the daemon's list and on every `Subagent` row the parent's hub carries.
    let slug = letibot_tools::builtins::task::slug_from_prompt("do the task, then stop and wait");
    let listed = parent
        .registry
        .list()
        .into_iter()
        .find(|b| b.session_id == handle)
        .map(|b| b.slug);
    assert_eq!(listed.as_deref(), Some(slug.as_str()));
    let named_rows: Vec<String> = parent
        .registry
        .get(parent_id)
        .expect("the parent's hub")
        .retained()
        .iter()
        .filter_map(|e| match &e.event {
            SessionEvent::Subagent {
                subagent_id, slug, ..
            } if *subagent_id == handle => Some(slug.clone()),
            _ => None,
        })
        .collect();
    assert!(
        !named_rows.is_empty() && named_rows.iter().all(|s| *s == slug),
        "{named_rows:?}"
    );

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

    // The child's turn RAN, with the message in it. The row is the parent's words - not
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
    // The same bytes the mid-turn door writes, so the two doors are one message - and the
    // store holds them once, which is what a resume replays.
    let said =
        letibot_harnessd::harness::message_from_a_session(parent_id, MESSAGE, Some(parent_id));
    assert_eq!(
        rows[0], said,
        "the parent is NAMED in the words the child hears, in `message_from_a_session`'s own bytes"
    );
    let stored = stored_user_rows(&path, &handle);
    assert_eq!(
        stored
            .iter()
            .filter(|(s, t)| *s == Speaker::Agent && t.contains(MESSAGE))
            .count(),
        1,
        "and the child's store holds it once: {stored:#?}"
    );
    assert!(
        !user_rows(&child)
            .iter()
            .any(|(s, t)| *s == Speaker::Operator && t.contains(MESSAGE)),
        "the message must not be recorded as something the operator typed"
    );

    // And the prompt the model was handed for that turn says so too - the row is the
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

/// A message to a child whose turn is ALREADY RUNNING still lands in that turn. The child
/// hears it at its next round boundary, as its parent's words, and keeps working - not a
/// second prompt, and no second turn.
#[test]
fn a_message_to_a_running_child_lands_in_that_turn() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-message-running");
    let path = dir.path().join("sessions.db");
    let parent_id = "s-parent-who-steers";

    // The file the child's first round reads - a relative path, because the session root is
    // `/tmp` and `read` is confined to it.
    std::fs::write(dir.path().join("note.txt"), "the plan, in one line\n")
        .expect("the file to read is written");
    let target = format!("{}/note.txt", dir.relative());

    let parts = parts_for(&config(parent_id, Endpoint::new("127.0.0.1", 1), &path));
    // Round zero calls a tool (so the turn has a round boundary at all); round one answers.
    let round0 = a_tool_call_turn(&parts.vocab, &read_call(&target), 30);
    let round1 = a_plain_answer_turn(&parts.vocab, "hearing", TO_THE_PARENT, 30);
    // Held: nothing is answered until this test says so, which is what makes the child
    // reliably mid-turn when the message arrives.
    let stub = Arc::new(Stub::held(vec![round0, round1], 2));

    let cfg = config(parent_id, stub.endpoint.clone(), &path);
    let parent = a_parent(parent_id, &parts, &cfg, stub.clone());
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
    // One turn, and the message in it - a message that had started a turn of its own
    // would have needed a third request, which this stub cannot serve.
    let rows = heard(&child, MESSAGE);
    assert_eq!(rows.len(), 1, "heard once, in the turn: {rows:#?}");
    // The row is the shared sentence's own bytes, and the store holds it once - a head draws
    // the row, a resume replays what the store has; a paraphrase or a second stored row would be
    // a duplicate of one act.
    let said =
        letibot_harnessd::harness::message_from_a_session(parent_id, MESSAGE, Some(parent_id));
    assert_eq!(
        rows[0], said,
        "the row is `message_from_a_session`'s own bytes, not a paraphrase of them"
    );
    let stored = stored_user_rows(&path, &handle);
    assert_eq!(
        stored
            .iter()
            .filter(|(s, t)| *s == Speaker::Agent && t.contains(MESSAGE))
            .count(),
        1,
        "the child's STORE holds the parent's words once: {stored:#?}"
    );
    assert!(
        warnings(&child).is_empty(),
        "no `message_idle` and no failed turn: {:#?}",
        warnings(&child)
    );
}

// ---------------------------------------------------------------------------
// One queue, one drainer
// ---------------------------------------------------------------------------

/// Two messages while the first queued turn is starting do not make two turns for one of
/// them, and neither is lost. One queue and one drainer: the child's own thread.
#[test]
fn a_second_message_does_not_start_a_second_turn_for_the_first() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
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
    let stub = Arc::new(Stub::answering(vec![task, one, two], 3));

    let cfg = config(parent_id, stub.endpoint.clone(), &path);
    let parent = a_parent(parent_id, &parts, &cfg, stub.clone());
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

    // And nothing was handed to the worker instead. `message_idle` is the sentence the
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

/// A genuine failure is still refused by name, and never claims to have queued. Three
/// different facts: a handle no session in this daemon answers to
#[test]
fn a_message_to_a_child_that_is_gone_is_refused_by_name() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-message-gone");
    let path = dir.path().join("sessions.db");
    let parent_id = "s-parent-with-lost-children";

    let parts = parts_for(&config(parent_id, Endpoint::new("127.0.0.1", 1), &path));
    // Two children are spawned below, one turn each - and both are HELD, because the second
    // child's turn must still be running when its hub closes.
    let stub = Arc::new(Stub::held(
        vec![
            a_plain_answer_turn(&parts.vocab, "planning", TO_THE_TASK, 30),
            a_plain_answer_turn(&parts.vocab, "planning", TO_THE_TASK, 30),
        ],
        2,
    ));

    let cfg = config(parent_id, stub.endpoint.clone(), &path);
    let parent = a_parent(parent_id, &parts, &cfg, stub.clone());

    // No such handle. Nothing was sent and the refusal says which sessions there are - a peer
    // this daemon does hold is deliverable to (`session_channel.rs`), so the sentence names the
    // session rather than the caller's children.
    let said = parent
        .message("sub-never-existed", MESSAGE)
        .expect_err("a handle this session never minted is refused");
    assert!(
        said.contains("no session `sub-never-existed`") && said.contains("Nothing was sent"),
        "the refusal names the session that is not there and does not claim to have sent \
         anything: {said}"
    );

    // A session that is over. A stop reaching a child BETWEEN turns ends its thread
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

    // And a session that goes away with the message already queued - the one window the
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

/// The daemon's worker hands a child's message back to the child's own reader.
#[test]
fn the_workers_arm_gives_a_childs_message_back_to_its_reader() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-message-dispatch");
    let path = dir.path().join("sessions.db");
    let parent_id = "s-parent-dispatching";

    let cfg = config(parent_id, Endpoint::new("127.0.0.1", 1), &path);
    let parts = parts_for(&cfg);
    let registry = Registry::new();
    // The daemon's own session, so `Sessions` exists at all - the child is a session it does
    // NOT hold, which is the whole condition this arm turns on.
    registry
        .create(parent_id, "the parent", wiring(&cfg))
        .expect("the parent is created");
    let mut sessions =
        Sessions::open_first(&parts, cfg.clone(), registry.clone()).expect("the daemon opens");

    let child = "s-child-not-held-by-the-daemon";
    let child_hub = registry.new_hub(child);
    // What `run_to_completion` does before it adopts: the child's own thread is its reader.
    child_hub.owned_by_its_own_thread();
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
    // The worker cannot win it any more: the hub knows its owner, and the worker's pop
    // answers `None` for a hub it does not own. The arm below is the second line - a command that
    // reached the worker some other way is still handed back.
    assert!(
        child_hub.try_command().is_none(),
        "the worker's pop took a command off a subagent's queue"
    );
    let cmd: QueuedCommand = match child_hub.take_own_work_until(Some(Instant::now())) {
        letibot_sessionlog::hub::OwnWork::Command(c) => c,
        other => panic!("the message is on the child's own queue: {other:?}"),
    };

    let outcome = sessions.dispatch(child, &cmd);
    assert!(
        matches!(outcome, Outcome::HandedOn),
        "the worker ran the message itself instead of handing it to the child's own reader"
    );
    let back = match child_hub.take_own_work_until(Some(Instant::now())) {
        letibot_sessionlog::hub::OwnWork::Command(c) => c,
        other => panic!("the message is back in front of the reader that can run it: {other:?}"),
    };
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

// ---------------------------------------------------------------------------
// The DAEMON'S exit - the call `cli.rs` makes, and the bound it makes it with
// ---------------------------------------------------------------------------

/// Whatever a test here asserted, the tree is drained before the process can exit - the
/// same rule [`Parent`]'s `Drop` keeps, and for the same crash: an assertion that fired
/// early would leave a child thread inside libllama when `exit` runs the ELF finalizers.
struct DrainOnDrop<'s, 'p> {
    sessions: &'s std::cell::RefCell<Sessions<'p>>,
    registry: &'s Registry,
    stub: &'s Stub,
}

impl Drop for DrainOnDrop<'_, '_> {
    fn drop(&mut self) {
        // The daemon's own order, because the child only leaves on a closed hub.
        self.registry.close();
        self.stub.release(usize::MAX);
        let gave_up = matches!(
            self.sessions.borrow().drain_subagents(PATIENCE),
            SubagentExit::GaveUp { .. }
        );
        if gave_up && !std::thread::panicking() {
            panic!(
                "a subagent thread outlived {PATIENCE:?} of waiting with its hub closed. The \
                 process must not exit with one: `exit()` finalizes libllama/libggml-cuda \
                 underneath it, and the last time this binary did that it died of SIGSEGV \
                 after every test had passed."
            );
        }
    }
}

/// The daemon's exit waits for a child that is mid-turn, and gives up on a bound rather
/// than hanging - `Sessions::drain_subagents`, which is what `cli.rs` calls before the
/// process exits. The child is inside its model call, the one state a stop cannot reach
#[test]
fn the_daemons_exit_waits_for_a_child_and_gives_up_on_a_bound() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-exit-drain");
    let path = dir.path().join("sessions.db");
    let id = "s-root-with-a-child";

    // The vocabulary first - the canned frames must speak the ids the child will send.
    let parts = parts_for(&config(id, Endpoint::new("127.0.0.1", 1), &path));
    let held = a_plain_answer_turn(&parts.vocab, "working", "still here", 30);
    // One request, HELD: the child is mid-call for as long as this test says.
    let stub = Arc::new(Stub::held(vec![held], 1));

    let cfg = config(id, stub.endpoint.clone(), &path);
    let registry = Registry::new();
    registry
        .create(id, "the root", wiring(&cfg))
        .expect("the session is created");
    let sessions = std::cell::RefCell::new(
        Sessions::open_first(&parts, cfg.clone(), registry.clone()).expect("the daemon opens"),
    );
    let _drained = DrainOnDrop {
        sessions: &sessions,
        registry: &registry,
        stub: &stub,
    };

    // Spawn through the session's own runtime - the production door, the same one a turn uses.
    let call = ToolCall {
        id: "call_task".into(),
        name: "task".into(),
        arguments: serde_json::json!({"prompt": "hold a turn open"}).to_string(),
    };
    let handle = match sessions
        .borrow()
        .harness_of(id)
        .expect("the root is open")
        .runtime_handle()
        .invoke("t1", &call, &mut NullToolSink)
        .outcome
    {
        letibot_transcript::ToolOutcome::Backgrounded { handle, .. } => handle,
        other => panic!("`task` did not start a child: {other:?}"),
    };
    assert!(
        stub.wait_for_requests(1),
        "`{handle}` never sent its first round"
    );

    // The daemon's quit, in its own order: every hub closed first, then the drain.
    registry.close();

    let bound = Duration::from_millis(300);
    let began = Instant::now();
    let said =
        sessions.borrow().drain_subagents(bound).sentence().expect(
            "a child inside one model call cannot be waited for, and the caller must be told",
        );
    assert!(
        said.contains(id),
        "the session that could not be waited for: {said}"
    );
    assert!(
        began.elapsed() >= bound,
        "the wait must actually wait rather than answer on a live child: {:?}",
        began.elapsed()
    );
    assert!(
        began.elapsed() < PATIENCE,
        "and it must give up on its bound rather than hang: {:?}",
        began.elapsed()
    );

    // And with the child able to move, the same call is a clean exit. The release ends its
    // turn, and with its hub already closed its thread leaves rather than parking - so the
    // second drain's bound is not spent.
    stub.release(usize::MAX);
    let began = Instant::now();
    let done = sessions.borrow().drain_subagents(GRACE);
    assert!(
        matches!(done, SubagentExit::Waited { .. }),
        "a child that can leave is a clean wait, not a give-up: {done:?}"
    );
    if let Some(said) = done.sentence() {
        assert!(
            said.contains("left") && !said.contains("SIGSEGV"),
            "a wait that succeeded may not read as a give-up: {said}"
        );
    }
    // And it did not spend the bound it was given - `Waited` says the child left inside it, and
    // this says the drain NOTICED rather than sleeping the grace out and looking afterwards. A
    // loaded box only makes the wait longer; the claim is that the bound was not what ended it.
    assert!(
        began.elapsed() < GRACE,
        "the wait spent its whole {GRACE:?} bound after the child had already left, which is a \
         deadline and not a wakeup"
    );
}

/// A quit with nothing running is not slowed down by the wait - the other half, and the one
/// that catches a hang. This is the ordinary daemon: a session that never started a child, a
/// closed registry, and an exit that must not spend the bound it is given on an empty tree.
#[test]
fn the_daemons_exit_returns_promptly_when_nothing_is_running() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-exit-quiet");
    let path = dir.path().join("sessions.db");
    let id = "s-root-quiet";

    let cfg = config(id, Endpoint::new("127.0.0.1", 1), &path);
    let parts = parts_for(&cfg);
    let registry = Registry::new();
    registry
        .create(id, "the root", wiring(&cfg))
        .expect("the session is created");
    let sessions = std::cell::RefCell::new(
        Sessions::open_first(&parts, cfg.clone(), registry.clone()).expect("the daemon opens"),
    );
    registry.close();

    // A bound the box cannot reach, so what the assertion below measures is the call and not
    // the machine: an empty tree is a lock and a look, and 60 s of it is a drain that waited.
    let began = Instant::now();
    assert_eq!(
        sessions.borrow().drain_subagents(GRACE).sentence(),
        None,
        "a session with no children is a clean exit, not a give-up"
    );
    assert!(
        began.elapsed() < GRACE,
        "an empty tree spent the whole {GRACE:?} bound it was given, which is a hang wearing a \
         bound"
    );
}

// ---------------------------------------------------------------------------
// Chat: the other direction, and the turn that ended before it could read
// ---------------------------------------------------------------------------

/// What the child says back, and the two messages the parent sends after the task - different
/// sentences so a test can say WHICH message a row came from.
const FROM_THE_CHILD: &str = "the parser half is done; the emitter is next";
const FIRST: &str = "start on the emitter now";
const AND_THEN: &str = "and report when it is done";

/// The child's own `task_message` call, in the Qwen dialect's markup - two parameters, as
/// `question_row.rs` and `session_channel.rs` spell their multi-argument calls.
fn task_message_call(to: &str, text: &str) -> String {
    format!(
        "\n<function=task_message>\n<parameter=task>\n{to}\n</parameter>\n\
         <parameter=text>\n{text}\n</parameter>\n</function>\n"
    )
}

/// A child can answer the session that started it, by the reserved handle `parent`.
/// `task_message` has always taken any live session's id, so the door was there
#[test]
fn a_child_answers_its_parent_by_the_reserved_handle() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-child-answers");
    let path = dir.path().join("sessions.db");
    let parent_id = "s-the-father";

    let parts = parts_for(&config(parent_id, Endpoint::new("127.0.0.1", 1), &path));
    // The child's two rounds (the call, then its answer), and then the parent's turn when the
    // message is dispatched to it. In that order, because the stub serves its scripts in order.
    let child_calls = a_tool_call_turn(
        &parts.vocab,
        &task_message_call(letibot_tools::builtins::task::PARENT_HANDLE, FROM_THE_CHILD),
        30,
    );
    let child_answers = a_plain_answer_turn(&parts.vocab, "sent", "told them", 30);
    let parent_hears = a_plain_answer_turn(&parts.vocab, "hearing", "good", 30);
    let stub = Arc::new(Stub::answering(
        vec![child_calls, child_answers, parent_hears],
        3,
    ));

    let cfg = config(parent_id, stub.endpoint.clone(), &path);
    let registry = Registry::new();
    registry
        .create(parent_id, "the father", wiring(&cfg))
        .expect("the session is created");
    let sessions = std::cell::RefCell::new(
        Sessions::open_first(&parts, cfg.clone(), registry.clone()).expect("the daemon opens"),
    );
    let _drained = DrainOnDrop {
        sessions: &sessions,
        registry: &registry,
        stub: &stub,
    };
    let parent_hub = registry.get(parent_id).expect("the parent's hub");

    // Through the parent's own runtime, which is the production door a turn uses.
    let call = ToolCall {
        id: "call_task".into(),
        name: "task".into(),
        arguments: serde_json::json!({"prompt": "do the parser half"}).to_string(),
    };
    let child = match sessions
        .borrow()
        .harness_of(parent_id)
        .expect("the parent is open")
        .runtime_handle()
        .invoke("t1", &call, &mut NullToolSink)
        .outcome
    {
        letibot_transcript::ToolOutcome::Backgrounded { handle, .. } => handle,
        other => panic!("`task` did not start a child: {other:?}"),
    };

    // The child's words reach the parent's queue - and the sender is the child, which is the
    // half that fails before the reserved handle existed: `task_message {"task": "parent"}`
    // answered *"no session `parent` is live in this daemon"*.
    let began = Instant::now();
    let cmd: QueuedCommand = loop {
        if let Some(c) = parent_hub.try_command() {
            break c;
        }
        if began.elapsed() > PATIENCE {
            panic!(
                "the child's message never reached the parent's queue — the child's own log: \
                 {:#?}",
                registry.get(&child).map(|h| h
                    .retained()
                    .iter()
                    .map(|e| e.event.clone())
                    .collect::<Vec<_>>())
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    match &cmd.kind {
        CommandKind::Message { from, text } => {
            assert_eq!(from, &child, "the sender is the child, by its own id");
            assert_eq!(text, FROM_THE_CHILD, "and the words are the child's own");
        }
        other => panic!("the child sent something other than a message: {other:?}"),
    }

    // The daemon's own arm - the worker's `Message` door, exactly as the loop calls it.
    match sessions.borrow_mut().dispatch(parent_id, &cmd) {
        Outcome::Replied(_) => {}
        Outcome::Failed(why) => panic!("the turn the child's message makes failed: {why}"),
        // `Outcome` has no `Debug` (it carries a `Reply`), so every other arm is named.
        Outcome::HandedOn => panic!("the worker handed the child's message to another reader"),
        Outcome::Ignored => panic!("the worker ignored the child's message"),
        Outcome::Compacted(_) => panic!("the worker compacted instead of running the message"),
    }

    // The parent's transcript holds it once, as an agent's row naming the child.
    let rows = heard(&parent_hub, FROM_THE_CHILD);
    assert_eq!(
        rows.len(),
        1,
        "the parent heard the child exactly once: {rows:#?}"
    );
    let said = letibot_harnessd::harness::message_from_a_session(&child, FROM_THE_CHILD, None);
    assert_eq!(
        rows[0], said,
        "the row is the shared sentence's own bytes — `message from session `<child>`` — and NOT \
         `your parent session`, which would be a false sentence about the tree"
    );
    assert!(
        !user_rows(&parent_hub)
            .iter()
            .any(|(s, t)| *s == Speaker::Operator && t.contains(FROM_THE_CHILD)),
        "the child's words must not be recorded as something the operator typed"
    );
    // And the store, which is what a later reader - a resume, a head tomorrow - sees.
    let stored = stored_user_rows(&path, parent_id);
    assert_eq!(
        stored
            .iter()
            .filter(|(s, t)| *s == Speaker::Agent && t.contains(FROM_THE_CHILD))
            .count(),
        1,
        "the parent's store holds the child's words once: {stored:#?}"
    );
}

/// A message the turn ended before reading is written into the transcript anyway - the
/// window that made a parent's words invisible. The ordinary path needs nothing: the engine
/// appends the steering item at a step boundary.
#[test]
fn a_message_the_turn_never_read_is_a_row_anyway() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-message-carried");
    let path = dir.path().join("sessions.db");
    let parent_id = "s-parent-whose-turn-died";

    std::fs::write(dir.path().join("note.txt"), "the plan, in one line\n")
        .expect("the file to read is written");
    let target = format!("{}/note.txt", dir.relative());

    let parts = parts_for(&config(parent_id, Endpoint::new("127.0.0.1", 1), &path));
    // Request by request, because the middle one is the one that dies 0, 1 the task's own
    // turn: a tool call, then its answer - the child then parks. 2, 3 the turn FIRST starts: a
    // tool call, then the round that is half-answered and dropped.
    let call = a_tool_call_turn(&parts.vocab, &read_call(&target), 30);
    let done = a_plain_answer_turn(&parts.vocab, "planning", TO_THE_TASK, 30);
    let call_again = a_tool_call_turn(&parts.vocab, &read_call(&target), 30);
    let doomed = a_plain_answer_turn(&parts.vocab, "hearing", TO_THE_PARENT, 30);
    let after = a_plain_answer_turn(&parts.vocab, "hearing", TO_THE_PARENT, 30);
    let stub = Arc::new(Stub::cutting(
        vec![call, done, call_again, doomed, after],
        5,
        3,
    ));

    let cfg = config(parent_id, stub.endpoint.clone(), &path);
    let parent = a_parent(parent_id, &parts, &cfg, stub.clone());
    let handle = parent.spawn("read the note, then answer");
    let child = parent.hub_of(&handle);

    // The task's own turn, and the child parked after it.
    parent.wait_until_parked(&handle);
    assert!(
        !child.status().running,
        "the fixture must stand in the state this test is about"
    );

    // The turn that will die. It is started by a message like any other turn, and the stub
    // answers its first round with a tool call so the turn has a boundary at all.
    parent
        .message(&handle, FIRST)
        .expect("a message to a between-turns child starts a turn");
    assert!(
        stub.wait_for_requests(4),
        "the child never reached the round that is meant to die"
    );
    // The child is inside the generation now - the first chunk is on the wire and it is
    // waiting for the rest. This is the window a message can be absorbed in and lost from.
    let said = parent
        .message(&handle, MESSAGE)
        .expect("a message to a running child is accepted");
    assert!(
        said.contains("turn is running") && said.contains("round boundary"),
        "the answer must say this was the RUNNING delivery: {said}"
    );
    // One more chunk, so the child polls its queue and takes the message; then the socket closes.
    stub.cut();

    // The row exists, with the parent's own words, as the agent's - and it is the injection's
    // item, not a copy of it: one `User` row with one `Text` part.
    let found = wait_for(&child, |hub| heard(hub, MESSAGE).first().cloned());
    assert!(
        found.contains(&format!("message from your parent session `{parent_id}`")),
        "the parent is named in the row the child holds: {found}"
    );
    assert_eq!(
        heard(&child, MESSAGE).len(),
        1,
        "one row, not two: {:#?}",
        heard(&child, MESSAGE)
    );
    // Which door wrote it, said on the log - `message_carried` is the sentence
    // `keep_the_words_it_never_read` publishes and nothing else does; without it the assertions
    // above would also pass on a build where a step boundary appended the item.
    assert!(
        warnings(&child)
            .iter()
            .any(|(code, detail)| code == "message_carried" && detail.contains(MESSAGE)),
        "the row was written by the carried-set drain, not by a step boundary: {:#?}",
        warnings(&child)
    );
    // And it is in the store, which is what a resume replays - so a later reader sees the
    // parent spoke, and sees it once.
    let stored = stored_user_rows(&path, &handle);
    assert_eq!(
        stored
            .iter()
            .filter(|(s, t)| *s == Speaker::Agent && t.contains(MESSAGE))
            .count(),
        1,
        "the child's store holds the parent's words once: {stored:#?}"
    );

    // And the next turn does not say it again. The carried set is emptied by the append, so
    // the greedy poll of the turn that follows cannot find it - the one way this row could have
    // been written twice.
    parent
        .message(&handle, AND_THEN)
        .expect("a second message starts a turn of its own");
    let answered = wait_for(&child, |hub| {
        answers(hub).into_iter().find(|a| a.contains(TO_THE_PARENT))
    });
    assert!(answered.contains(TO_THE_PARENT), "{answered}");
    assert_eq!(
        heard(&child, MESSAGE).len(),
        1,
        "the message the failed turn carried is not written a second time: {:#?}",
        heard(&child, MESSAGE)
    );
    assert_eq!(
        heard(&child, AND_THEN).len(),
        1,
        "and the second message is one row: {:#?}",
        heard(&child, AND_THEN)
    );
}
