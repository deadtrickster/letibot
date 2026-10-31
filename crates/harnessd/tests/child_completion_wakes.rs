//! **R57's last hop: a child's answer wakes its parent, with nobody asking.**
//!
//! The operator's measurement (2026-10-03) was a session that spawned `task` children: the
//! child ran and answered, the head's agent counter flashed 1 → 0, and **no notice row
//! appeared** — in the head or in the model's own conversation. `job_list` in the spawning
//! session said `0 monitors, 0 fired, 0 jobs`, so nothing was armed and nothing could fire,
//! and the model learned the child had finished only by polling `task_result`.
//!
//! # What the tree says, and what this file measures
//!
//! Read rather than assumed, the chain is one queue and one bell: `task` returns the same
//! `ToolOutcome::Backgrounded` a `bash --background` call does, `JobWatchSink` arms a
//! watcher for it as the result passes through the round's sink, `watch_task` waits on the
//! runner's own condvar, `SettlementQueue::push` queues the settlement on **the session that
//! started it** and rings that session's bell, and `Harness::wake` drains the queue and
//! submits one `User { speaker: Agent }` item. Every link has a unit test except the last
//! one — and a unit test of a link is not a measurement of a chain, which is what this row
//! has been waiting for since 2026-10-03.
//!
//! So this file drives the whole of it, through the doors a daemon drives:
//!
//! 1. a **real parent `Harness`** with the real `HarnessTaskRunner`, opened over a real store
//!    and a registry, with a stub model at the endpoint;
//! 2. a **real turn** (`Harness::submit`) in which the model calls `task` — so the call goes
//!    through `Harness::tool_sink`, the round's own arm. It is not the only arm in the tree: a
//!    `ToolEventSink` is a parameter of the invocation, so "is this handle watched?" is a
//!    property of the CALL SITE, and the operator's doors arm the same watcher through
//!    `Harness::unprompted_sink` (with `crate::bangrun`'s thread carrying it). Those are
//!    `operator_task_wakes.rs`'s to measure; this file drives the round's;
//! 3. the child's own thread opening its own harness and running its own turn, and settling;
//! 4. **the parent's bell** — `Registry::next_work` answering `Work::Woken(parent)` with
//!    nobody having called `task_result`, `job_list` or `task_wait`;
//! 5. **the parent's wake** — `Harness::wake`, whose notice is the `Speaker::Agent` row the
//!    operator's ruling asks for, naming the child and carrying the first line of its answer.
//!
//! The second half is the state the first one cannot reach: **a settlement for a session the
//! daemon is not holding**. See `a_settlement_for_a_session_between_opens_is_delivered_at_its_next_open`.
//!
//! # What this needs
//!
//! The vocabulary GGUF (`Harness::open` renders its stable prefix) and **not** a model: the
//! turns are answered by the stub below. The seat is `leticode`, the unconfined seat that
//! names the delegation trio — the default seat names none of them, and a test that invoked a
//! tool its session does not seat would be measuring the seat table.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime};

use letibot_harnessd::config::{Config, Seat};
use letibot_harnessd::{Dialect, Harness, Parts};
use letibot_sessionlog::registry::{Registry, SessionWiring, Work, WorkOrIdle};
use letibot_tokencore::store::{SessionRecord, Store};
use letibot_transcript::{Speaker, ToolOutcome, TranscriptItem, UserPart};
use letibot_turn::Endpoint;

/// The canned server the turn crate's tests replay, shared by path rather than copied — a
/// second copy of the wire shape would drift from the first. `dead_code` allowed because this
/// file writes its own server (it has to hold one request and answer the others): what it takes
/// from here is [`canned::Frame`] and [`canned::sse_body`].
#[allow(dead_code)]
#[path = "../../turn/tests/support/canned.rs"]
mod canned;

use canned::Frame;

/// Qwen's own ids, the same values the turn crate's canned tests pin.
const IM_END: u32 = 248046;
const THINK_OPEN: u32 = 248068;
const THINK_CLOSE: u32 = 248069;
const TOOL_CALL_OPEN: u32 = 248058;
const TOOL_CALL_CLOSE: u32 = 248059;

/// What the parent says to start the turn. A marker, so a failure can name the turn it was in.
const PROMPT: &str = "start the child and carry on";

/// The subtask the model hands the child — the string the `task` call carries.
///
/// **Two lines, and the second one is a MARKER**, which is how the stub tells the child's own
/// request from the parent's: only the child is handed the subtask, so the marker is in the
/// child's prompt and nowhere in the parent's.
const SUBTASK: &str = "count the files and report\nmarker: this child is the one";

/// The line that makes a request the CHILD's — see [`SUBTASK`].
const CHILD_MARKER: &str = "marker: this child is the one";

/// **The child's answer, and the first line of it is what the notice must carry.** Two lines,
/// so the notice's `first_line` rule is exercised rather than assumed.
const CHILD_SAYS: &str = "the count is forty-two.\nthe rest is in task_result";

/// How long the driver waits for something a correct build produces in milliseconds —
/// generous on purpose, because the assertion is what happened, not how fast.
const PATIENCE: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// The fixtures
// ---------------------------------------------------------------------------

struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = letibot_sessionlog::testing::scratch_root().join(format!(
            "{tag}-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .expect("clock before epoch")
                .as_nanos(),
            {
                static N: AtomicUsize = AtomicUsize::new(0);
                N.fetch_add(1, Ordering::Relaxed)
            }
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("the temp dir");
        Self(path)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// **A session's own config**, with the operator's own `providers.toml` and `permission.json`
/// out of the way (`wired.rs`'s rule: a key on the developer's laptop must not decide what a
/// test seats).
///
/// The workspace is the test's own directory and the seeded session row names the same one: a
/// session reopened from a row whose `workspace_root` disagrees with the open is a note on the
/// log, and this file opens one session twice.
fn config(
    session_id: &str,
    endpoint: Endpoint,
    store: &std::path::Path,
    workspace: &std::path::Path,
) -> Config {
    let mut cfg = Config::for_this_box(workspace);
    cfg.dialect = Dialect::Qwen;
    // `leticode`: unconfined, and it names `task`/`task_result`/`task_message`. `coder` names
    // them too and refuses to open on a Mac ("needs a confined execution backend"), which
    // would make this file measure the absence of a boundary.
    cfg.seat = Seat::Leticode;
    cfg.endpoint = endpoint;
    cfg.http_retries = 0;
    // **A window, because a child is planned against one** — a spawn with none is refused by
    // name, and the refusal is right.
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
    // The operator's per-project mode is not this test's business either.
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

fn seed_session(store: &std::path::Path, id: &str, workspace: &std::path::Path) {
    let s = Store::open(store).expect("seeding the store");
    s.put_session(&SessionRecord {
        id: id.to_string(),
        title: None,
        model_id: "m".into(),
        dialect_sha: "sha".into(),
        workspace_root: workspace.display().to_string(),
        owner: "dead".into(),
        role: None,
        approvers: vec![],
        parent_session_id: None,
    })
    .expect("the session row");
}

/// **A parent session with the REAL runner, opened the way a daemon opens one** — its own hub
/// adopted into a shared registry, so the bell the watcher rings is one this test can ask.
///
/// The registry is passed in rather than made here, because **one daemon is one registry**: its
/// `instance` is minted per registry and it is what a settlement row's `instance` column names,
/// so two registries would be two daemons and the whole question this file asks (*did THIS
/// daemon's dropped settlement come back at the next open?*) would be about two of them.
///
/// It owns the two things its teardown needs: the registry (which it closes if it is the daemon)
/// and its own session id (which it takes out of the registry when it goes). A child's thread is
/// spawned detached, and a thread still inside `libllama` when `exit()` runs the loader's
/// finalizers takes SIGSEGV — after every test has printed `ok`, which is the shape that gets
/// blamed on the next person's change (`message_between_turns`'s header measures it at 2 runs in
/// 60).
struct Parent {
    /// `None` once [`Parent::close`] has dropped it — which is what a daemon's close does, and
    /// what gives the store's one-writer claim back so the session can be opened again.
    harness: Option<Harness>,
    registry: Arc<Registry>,
    /// **Whether this handle is the DAEMON** — the one that closes the registry when it goes.
    /// A test that opens one session three times has three `Parent`s and one registry, so the
    /// first one carries the close and the others must not: closing it under the third open is
    /// `adopt` answering `Closed`.
    owns_registry: bool,
    session_id: String,
}

impl Parent {
    fn open(parts: &Parts, cfg: &Config, registry: Arc<Registry>) -> Parent {
        let hub = registry.new_hub(cfg.session_id.clone());
        registry
            .adopt(hub.clone(), cfg.session_id.clone(), wiring(cfg), None)
            .expect("the parent is in the registry");
        let harness =
            Harness::open_with_registry(parts, cfg.clone(), hub, None, None, Arc::clone(&registry))
                .expect("the parent opens");
        Parent {
            harness: Some(harness),
            registry,
            owns_registry: false,
            session_id: cfg.session_id.clone(),
        }
    }

    /// **This handle is the daemon**: it closes the registry when it is dropped, which is what
    /// every hub, the bell and the worker's queue go with.
    fn owning(mut self) -> Parent {
        self.owns_registry = true;
        self
    }

    fn harness(&self) -> &Harness {
        self.harness.as_ref().expect("the session is open")
    }

    fn harness_mut(&mut self) -> &mut Harness {
        self.harness.as_mut().expect("the session is open")
    }

    /// **Close the session the way the daemon does** — `Sessions::close`'s order, minus the
    /// drop: the backend goes (which stops the watcher set), then the registry entry goes (which
    /// closes the hub, so a ring for this session has nothing to wake). What that leaves is
    /// exactly the state the hole is about: a session this daemon is not holding, and a child
    /// still working whose settlement will arrive into it.
    fn close(&mut self) {
        if let Some(h) = &self.harness {
            h.close_backend();
        }
        self.registry.remove(&self.session_id);
    }

    /// **Let go of the harness**, which gives the store's one-writer claim back — the step that
    /// makes a second open of the same session legal, and the reason a test that reopens a
    /// session has to take it.
    fn forget(&mut self) {
        self.harness = None;
    }

    /// **The child's handle, read off the parent's own transcript** — the `task` call's result
    /// row is what the model was handed, so this is the production record and not a fixture's
    /// note. `None` until the round that made the call has landed.
    fn spawned(&self) -> Option<String> {
        self.harness().items().iter().find_map(|i| match i {
            TranscriptItem::ToolResult { name, outcome, .. } if name == "task" => match outcome {
                ToolOutcome::Backgrounded { handle, .. } => Some(handle.clone()),
                _ => None,
            },
            _ => None,
        })
    }

    /// **Wait for the daemon's own wake queue to name `who`** — `Work::Woken` is the ring, and
    /// a ring is the only thing that makes `Harness::wake` run at all.
    ///
    /// Everything else the queue holds is dropped on the way past (the child's `Open`, a
    /// command a head submitted): the assertion is *this session was woken*, not *this was the
    /// only thing the daemon had to do*.
    fn wait_for_a_ring(&self, who: &str) -> bool {
        let began = Instant::now();
        while began.elapsed() < PATIENCE {
            match self
                .registry
                .next_work_until(Some(Instant::now() + Duration::from_millis(50)))
            {
                WorkOrIdle::Work(Work::Woken(id)) if id == who => return true,
                WorkOrIdle::Work(_) | WorkOrIdle::Idle | WorkOrIdle::Closed => {}
            }
        }
        false
    }
}

impl Drop for Parent {
    fn drop(&mut self) {
        // **The session goes with the handle**, the way `Sessions::close` takes the registry
        // entry with it: a hub left behind makes the next open of that id `adopt` answering
        // `Exists`, which is a fixture artefact rather than a daemon's state.
        self.registry.remove(&self.session_id);
        if self.owns_registry {
            self.registry.close();
        }
        let Some(h) = &self.harness else {
            return;
        };
        if !h.wait_for_children(PATIENCE) && !std::thread::panicking() {
            panic!(
                "the tree still had a subagent thread after {PATIENCE:?} of waiting, with its \
                 hubs closed. The process must not exit with one: `exit()` finalizes \
                 libllama underneath it, and the last time this binary did that it died of \
                 SIGSEGV after every test had passed."
            );
        }
    }
}

// ---------------------------------------------------------------------------
// The stub
// ---------------------------------------------------------------------------

/// **A model that answers its first request with a `task` call and every later one with the
/// child's answer.**
///
/// Written rather than taken from `canned::Canned` for one reason: the parent's and the
/// child's requests arrive on ONE endpoint in an order the test does not control (the spawn
/// returns as soon as the child's thread starts, so the parent's second round and the child's
/// first turn race). `Canned::serve_each` cycles its scripts by request index, which would
/// hand the child a script meant for the parent; here the FIRST request is the tool call and
/// everything after it is a plain answer, so whichever order the two arrive in, the parent's
/// first round is the call and nothing else is.
struct Stub {
    endpoint: Endpoint,
    requests: Arc<AtomicUsize>,
    /// **The gate**: `true` once every turn may be answered. Until then, a request whose prompt
    /// carries [`CHILD_MARKER`] waits here — which is what makes *the child has not answered yet*
    /// a state a test stands in rather than races.
    gate: Arc<(Mutex<bool>, Condvar)>,
    /// How many requests are parked at the gate right now — the barrier
    /// [`Stub::wait_until_held`] waits on, so a test can say *the child is waiting* rather than
    /// *the child has probably reached the endpoint*.
    held: Arc<AtomicUsize>,
}

impl Stub {
    /// A model that answers every request as it arrives.
    fn new(vocab: &Arc<letibot_tokencore::Vocab>) -> Stub {
        Stub::with_gate(vocab, true)
    }

    /// A model that answers everything except the CHILD's own turn, which it holds until
    /// [`Stub::release`].
    fn held(vocab: &Arc<letibot_tokencore::Vocab>) -> Stub {
        Stub::with_gate(vocab, false)
    }

    fn with_gate(vocab: &Arc<letibot_tokencore::Vocab>, open: bool) -> Stub {
        let listener = TcpListener::bind("127.0.0.1:0").expect("an ephemeral port");
        let addr = listener.local_addr().unwrap();
        let call = Arc::new(a_task_call_turn(vocab, SUBTASK, 30));
        let answer = Arc::new(a_plain_answer_turn(vocab, "thinking", CHILD_SAYS, 30));
        let requests = Arc::new(AtomicUsize::new(0));
        let gate: Arc<(Mutex<bool>, Condvar)> = Arc::new((Mutex::new(open), Condvar::new()));
        let held = Arc::new(AtomicUsize::new(0));
        let vocab = Arc::clone(vocab);
        let mine = Arc::clone(&requests);
        let theirs = Arc::clone(&gate);
        let parked = Arc::clone(&held);
        std::thread::spawn(move || {
            loop {
                let Ok((stream, _)) = listener.accept() else {
                    return;
                };
                // **One thread per connection, and that is not tidiness.** The child's request is
                // HELD here, and a server that answered its connections one at a time would be
                // holding the socket the parent's next round needs — a deadlock the first cut of
                // this file measured as `submit` failing on `WouldBlock` three minutes in.
                let call = Arc::clone(&call);
                let answer = Arc::clone(&answer);
                let vocab = Arc::clone(&vocab);
                let seen = Arc::clone(&mine);
                let gate = Arc::clone(&theirs);
                let held = Arc::clone(&parked);
                std::thread::spawn(move || {
                    let (line, body) = read_request(stream.try_clone().expect("clone the socket"));
                    // **`/props` is not a turn and does not consume the call.** A spawn asks the
                    // parent's own endpoint for its window (`HarnessTaskRunner::start`'s
                    // `served_ctx`), which is a fact about the spawn path rather than about the
                    // model.
                    if line.starts_with("GET ") {
                        let _ = write_props(stream);
                        return;
                    }
                    let n = seen.fetch_add(1, Ordering::SeqCst);
                    // **The child's request, told from the parent's by whose words it holds.**
                    //
                    // The marker alone is not enough, and the first cut that thought it was
                    // deadlocked this very test: the PARENT's second round carries the assistant's
                    // `task` call in its own history, arguments and marker included, so a gate on
                    // the marker alone held the parent's turn for ever. The parent's conversation
                    // also holds what IT was asked ([`PROMPT`]) and the child's holds only the
                    // subtask, so the two together name one speaker.
                    let asked = prompt_of(&vocab, &body);
                    if asked.contains(CHILD_MARKER) && !asked.contains(PROMPT) {
                        let (lock, cv) = &*gate;
                        let mut open = lock.lock().expect("the stub's gate");
                        while !*open {
                            held.fetch_add(1, Ordering::SeqCst);
                            open = cv.wait(open).expect("the stub's gate");
                        }
                    }
                    let frames: &[Frame] = if n == 0 { &call } else { &answer };
                    let _ = write_frames(stream, frames);
                });
            }
        });
        Stub {
            endpoint: Endpoint::new(addr.ip().to_string(), addr.port()),
            requests,
            gate,
            held,
        }
    }

    /// **Let the held turn be answered.**
    fn release(&self) {
        let (lock, cv) = &*self.gate;
        *lock.lock().expect("the stub's gate") = true;
        cv.notify_all();
    }

    /// How many turns the endpoint has answered — the child's is among them, which is how the
    /// test can tell *the child ran* from *the child was spawned*.
    fn turns(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }

    /// **Wait until a turn is parked at the gate** — the child has asked, and has not been
    /// answered.
    fn wait_until_held(&self) -> bool {
        let began = Instant::now();
        while began.elapsed() < PATIENCE {
            if self.held.load(Ordering::SeqCst) > 0 {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }
}

impl Drop for Stub {
    fn drop(&mut self) {
        // A test that failed early must not leave the server's thread parked on the gate for
        // the rest of the suite.
        self.release();
    }
}

fn ids(vocab: &letibot_tokencore::Vocab, text: &str) -> Vec<u32> {
    vocab.tokenize_text(text).expect("text tokenizes")
}

/// **What the model was handed for one request** — the token ids, detokenized with the vocabulary
/// the session tokenized with. This is how the stub tells the child's request from the parent's:
/// a prompt is the only thing a request carries that says who it is for.
fn prompt_of(vocab: &letibot_tokencore::Vocab, body: &str) -> String {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(body) else {
        return String::new();
    };
    let Some(ids) = json["prompt"].as_array() else {
        return String::new();
    };
    let ids: Vec<u32> = ids
        .iter()
        .filter_map(|v| v.as_u64().map(|n| n as u32))
        .collect();
    vocab.detokenize(&ids, true).unwrap_or_default()
}

fn spoken(vocab: &letibot_tokencore::Vocab, ids: &[u32]) -> Vec<Frame> {
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
fn a_plain_answer_turn(
    vocab: &letibot_tokencore::Vocab,
    thought: &str,
    answer: &str,
    n_prompt: u64,
) -> Vec<Frame> {
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

/// A turn that calls `task` and ends — the call is what arms the watcher, and this is the shape a
/// ROUND takes (the operator's doors run the same call through `unprompted_sink` instead).
fn a_task_call_turn(vocab: &letibot_tokencore::Vocab, prompt: &str, n_prompt: u64) -> Vec<Frame> {
    let call =
        format!("\n<function=task>\n<parameter=prompt>\n{prompt}\n</parameter>\n</function>\n");
    let call_ids = ids(vocab, &call);
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

/// Drain one request — headers plus the body `Content-Length` names — so the client's write
/// side is finished before the answer arrives. The request line comes back too, so the caller
/// can tell a `/props` probe from a turn.
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

/// The endpoint's own props, as `served_ctx` reads them: the one field a spawn asks for is the
/// window, and a server that answers nothing gets `None`, which is no wall at all.
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
// The session's log
// ---------------------------------------------------------------------------

/// Every user row of a session's log, speaker and text.
fn user_rows(items: &[TranscriptItem]) -> Vec<(Speaker, String)> {
    items
        .iter()
        .filter_map(|i| match i {
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

// ---------------------------------------------------------------------------
// THE ONE THIS ROW EXISTS FOR
// ---------------------------------------------------------------------------

/// **A parent spawns a child, the child answers, and the parent is RUNG — with nobody asking.**
///
/// The three assertions, in the order the defect was measured:
///
/// * the ring: `Registry::next_work` answers `Work::Woken(parent)`. Before this row's chain
///   existed the watcher was armed on the wrong wait — `ProcessHost::wait_job` on a name the
///   host had never heard — so the queue stayed empty, the bell rang for nothing and this
///   fails with a timeout rather than a wrong value;
/// * the wake: `Harness::wake` returns a turn rather than `Ok(None)`, which is the whole of
///   *the settlement reaches the model*;
/// * the sentence: one `User { speaker: Agent }` row saying a subagent the session started has
///   finished, naming the child and carrying the first line of what it said. `Speaker::Agent`
///   and not `Operator`: the session is talking to itself, and a head must not draw it as the
///   person's words (R42).
///
/// Nothing in here calls `task_result`, `job_list` or `job_wait`. That is the measurement's own
/// subject: *the model learned the child had finished only by polling*.
#[test]
fn a_childs_answer_wakes_its_parent_with_nobody_asking() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = TempDir::new("child-wakes");
    let store = dir.path().join("sessions.db");
    let parent_id = "s-parent-wake";
    seed_session(&store, parent_id, dir.path());
    let mut cfg = config(parent_id, Endpoint::new("127.0.0.1", 1), &store, dir.path());
    let parts = parts_for(&cfg);
    // The stub after the parts, because a script is tokenized with the vocabulary this session
    // was opened with — the two must be one vocabulary, or the ids mean nothing.
    let stub = Stub::new(&parts.vocab);
    cfg.endpoint = stub.endpoint.clone();
    let registry = Registry::new();
    let mut parent = Parent::open(&parts, &cfg, registry).owning();
    // **One real turn, whose first round calls `task`.** This is the round's path: the result
    // passes through `Harness::tool_sink` and that sink arms the watcher. The operator's doors
    // arm the same watcher through `Harness::unprompted_sink`; `operator_task_wakes.rs` drives
    // those, and a test that is about the round should not pretend to be about both.
    let reply = parent
        .harness_mut()
        .submit(PROMPT)
        .expect("the turn that starts the child");
    assert!(
        !reply.text.is_empty(),
        "the turn ran to an answer after the call"
    );
    let child = parent
        .spawned()
        .expect("the `task` call's result row names the child it started");
    assert!(
        parent.harness().job_watchers().is_some(),
        "a session that can hand work to a child must have a watcher set"
    );

    // **The ring, and it is the daemon's own queue.** Nobody has asked the child anything.
    assert!(
        parent.wait_for_a_ring(parent_id),
        "the child answered and its parent was never woken — nothing was armed for it, or the \
         settlement never reached this session's queue"
    );

    // **The wake.** `Ok(None)` here is *the settlement did not reach the model*.
    let woke = parent
        .harness_mut()
        .wake()
        .expect("the wake's turn")
        .expect("a settlement is a turn, not a no-op");
    assert!(
        !woke.text.is_empty(),
        "the wake's reply carries the notice it submitted"
    );

    let rows = user_rows(parent.harness().items());
    let notice = rows
        .iter()
        .find(|(speaker, text)| {
            *speaker == Speaker::Agent && text.contains("a subagent you started has finished")
        })
        .map(|(_, text)| text.clone())
        .unwrap_or_else(|| {
            panic!("no `[task]` notice row in the parent's conversation: {rows:#?}")
        });
    assert!(
        notice.contains(&child),
        "the notice must name the child it is about ({child}): {notice}"
    );
    assert!(
        notice.contains("the count is forty-two."),
        "the notice carries the first line of the child's answer — that is what makes the \
         wake actionable rather than a nudge: {notice}"
    );
    assert!(
        !notice.contains("the rest is in task_result"),
        "and only the first line of it, because the whole answer is `task_result`'s: {notice}"
    );
    // **The child RAN, which is what makes the settlement above a settlement.** Two requests
    // were answered: the parent's first round (the call) and the child's own turn.
    assert!(
        stub.turns() >= 2,
        "the endpoint answered {} turn(s): the child never ran",
        stub.turns()
    );
}

// ---------------------------------------------------------------------------
// And the same settlement when its session is NOT open
// ---------------------------------------------------------------------------

/// **A child that answers into a session this daemon is not holding is still delivered at that
/// session's next open** — the second half of R57's row, and the half the board named as
/// *HOLE NAMED, NOT FIXED*.
///
/// `JobWatchers` is built per open, so the settlement a child produces after its parent's session
/// closed has nowhere to go: the queue belongs to a harness nobody will drain, and the ring names
/// a session `Sessions::wake` answers `Gone` for (`wake_route`). Both die with the harness. What
/// survives is the **row** — `subagent_run`, written by the child's own thread with
/// `delivered = 0` — and `Harness::deliver_settlements_left_undelivered` is the door that reads it
/// at the next open.
///
/// The three facts, in the order they matter:
///
/// * **the settlement was not delivered while the session was closed** — `delivered` is still 0
///   in the store when the session comes back, which is what makes the second open the place it
///   is said rather than a place it is said twice;
/// * **the wake says it** — the same `[task] a subagent you started has finished` row the first
///   test asserts, produced by the ordinary `completion_notices` and not by a restart sentence:
///   nothing restarted, and a `[restart]` notice about a session that was merely closed would be
///   a false sentence about the daemon;
/// * **and it is marked delivered**, so a third open says nothing. The row's `delivered` column
///   is the whole of *has the model been told*, and a fix that left it at 0 would deliver the
///   same settlement at every open for the life of the store.
#[test]
fn a_settlement_for_a_session_between_opens_is_delivered_at_its_next_open() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = TempDir::new("child-wakes-closed");
    let store_path = dir.path().join("sessions.db");
    let parent_id = "s-parent-closed";
    seed_session(&store_path, parent_id, dir.path());
    let mut cfg = config(
        parent_id,
        Endpoint::new("127.0.0.1", 1),
        &store_path,
        dir.path(),
    );
    let parts = parts_for(&cfg);
    // **The child's turn is HELD at the endpoint**, so *it has not answered yet* is a state this
    // test stands in rather than races: the close below has to happen first, and a child that
    // answered in the same millisecond would be measuring the other path.
    let stub = Stub::held(&parts.vocab);
    cfg.endpoint = stub.endpoint.clone();
    // One registry for both opens: it IS the daemon, and its `instance` is what a settlement
    // row's `instance` column names.
    let registry = Registry::new();
    let mut parent = Parent::open(&parts, &cfg, registry).owning();

    parent
        .harness_mut()
        .submit(PROMPT)
        .expect("the turn that starts the child");
    let child = parent
        .spawned()
        .expect("the `task` call's result row names the child it started");
    assert!(
        stub.wait_until_held(),
        "the child's own turn never reached the endpoint, so there is nothing to hold"
    );
    // **Close the session while the child is still working.** The daemon's order: the backend
    // (which stops the watcher set), then the registry entry (which closes the hub, so the ring
    // for this session has nothing to wake).
    parent.close();

    // **Now the child answers, into a session nobody is holding.**
    stub.release();

    // The row is the only survivor, and it is written with `delivered = 0`.
    let run = wait_for_a_settled_child(&store_path, &child);
    assert_eq!(run.state, "done", "the child answered: {run:?}");
    assert!(
        !run.delivered,
        "the settlement was never delivered — nothing was holding the session: {run:?}"
    );

    // **The tree is let go, and it MUST be** — a session is claimed for writing at open
    // (`writer::claim`) and a second open of a session a live harness holds is refused by name.
    //
    // `wait_for_children` is deliberately NOT the barrier here: a child that has answered is
    // PARKED, not finished (its thread waits in `serve_child` for a `task_message` that may never
    // come), so waiting for its thread before the registry closes would wait out the whole
    // patience. The barrier is the row above — the child's turn has settled, which is the only
    // thing this half needs and the thing that keeps its thread out of `libllama` at exit.
    parent.forget();

    // **The next open.**
    let mut second = Parent::open(&parts, &cfg, parent.registry.clone());
    // What every open does — `Sessions::harness_as` and `Sessions::open_first_with_seat` both
    // call it, and this is the line inside it that reads what the daemon dropped.
    second.harness_mut().restore_after_restart();
    let woke = second
        .harness_mut()
        .wake()
        .expect("the open's wake")
        .expect("the settlement this daemon dropped is a turn at the next open");

    assert!(!woke.text.is_empty(), "the wake carries its notice");
    let rows = user_rows(second.harness().items());
    let notice = rows
        .iter()
        .find(|(speaker, text)| {
            *speaker == Speaker::Agent && text.contains("a subagent you started has finished")
        })
        .map(|(_, text)| text.clone())
        .unwrap_or_else(|| {
            panic!(
                "the settlement a closed session dropped was never said at the next open: {rows:#?}"
            )
        });
    assert!(
        notice.contains(&child),
        "the notice names the child ({child}): {notice}"
    );
    assert!(
        notice.contains("the count is forty-two."),
        "and carries the first line of the answer the row kept: {notice}"
    );
    assert!(
        !notice.contains("[restart]"),
        "nothing restarted: this is the ordinary completion sentence, not a restart's: {notice}"
    );

    // **And said once.** The row's `delivered` column is the record, so a third open is silent.
    let after = wait_for_a_settled_child(&store_path, &child);
    assert!(
        after.delivered,
        "the delivery was not marked in the store, so every open would say it again: {after:?}"
    );
    drop(second);
    let mut third = Parent::open(&parts, &cfg, parent.registry.clone());
    third.harness_mut().restore_after_restart();
    assert!(
        third
            .harness_mut()
            .wake()
            .expect("the third open's wake")
            .is_none(),
        "a settlement already delivered was delivered again"
    );
}

/// **Wait for the child's row to say it settled**, and hand the row back — the durable fact this
/// half of the row rests on. A run that never settles is a failed test rather than a hang.
fn wait_for_a_settled_child(
    store: &std::path::Path,
    child: &str,
) -> letibot_tokencore::store::SubagentRun {
    let began = Instant::now();
    while began.elapsed() < PATIENCE {
        if let Ok(Some(run)) = Store::open(store).and_then(|s| s.subagent_run(child))
            && matches!(run.state.as_str(), "done" | "failed")
        {
            return run;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!(
        "`{child}` never settled: no `done`/`failed` row in {}",
        store.display()
    );
}
