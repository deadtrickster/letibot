//! A child's answer wakes its parent, with nobody asking - and so does each later turn of it.

use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime};

use letibot_harnessd::config::{Config, Seat};
use letibot_harnessd::{Dialect, Harness, Parts};
use letibot_sessionlog::ServerFrame;
use letibot_sessionlog::hub::{CommandKind, DAEMON_SUBMITTER};
use letibot_sessionlog::registry::{Registry, SessionWiring, Work, WorkOrIdle};
use letibot_tokencore::store::{SessionRecord, Store};
use letibot_transcript::{Speaker, ToolOutcome, TranscriptItem, UserPart};
use letibot_turn::Endpoint;

/// The shared fixtures. This file writes its own server (it has to hold one request and answer
/// the others), so what it takes from there is the frames, the stub's HTTP and the vocabulary.
mod support;

use support::canned::Frame;
use support::qwen::{THINK_CLOSE, a_plain_answer_turn, ids, spoken};
use support::stub::{read_request, write_frames, write_props};

/// Qwen's own ids, the same values the turn crate's canned tests pin.
const THINK_OPEN: u32 = 248068;
const TOOL_CALL_OPEN: u32 = 248058;
const TOOL_CALL_CLOSE: u32 = 248059;

/// What the parent says to start the turn. A marker, so a failure can name the turn it was in.
const PROMPT: &str = "start the child and carry on";

/// The subtask the model hands the child - the string the `task` call carries. Two lines
/// and the second one is a MARKER: only the child is handed the subtask, so the marker is
/// in the child's prompt and nowhere in the parent's.
const SUBTASK: &str = "count the files and report\nmarker: this child is the one";

/// The line that makes a request the CHILD's - see [`SUBTASK`].
const CHILD_MARKER: &str = "marker: this child is the one";

/// The child's answer, and the first line of it is what the notice must carry. Two lines
/// so the notice's `first_line` rule is exercised rather than assumed.
const CHILD_SAYS: &str = "the count is forty-two.\nthe rest is in task_result";

/// What the child says when its parent asks it something AFTER its task - a second answer, so
/// a notice carrying this line can only have come from the later turn.
const CHILD_AGAIN: &str = "second time round: the total is forty-four";

/// What a parent tells a child that has already answered, to make it turn again.
const MORE: &str = "one more thing before you go";

/// What the parent says to the child AFTER it has answered, on the door a session between turns
/// hears a message through, so the answer below can only belong to that turn.
const LATER: &str = "marker: now the other half of it";

/// The child's answer to that later turn, distinct from [`CHILD_SAYS`] so a notice proves WHICH
/// turn it carries.
const CHILD_SAYS_AGAIN: &str = "the other half is done too.\nthe branch holds both halves";

/// A later turn that answers NOTHING - the model says it is still working and stops. The parent is
/// not woken for it.
const LATER_SILENT: &str = "marker: keep going quietly";

/// How long the driver waits for something a correct build produces in milliseconds -
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

/// A session's own config, with the operator's own `providers.toml` and `permission.json`
/// out of the way - a key on the developer's laptop must not decide what a test seats. The
/// workspace is the test's own directory and the seeded session row names the same one
fn config(
    session_id: &str,
    endpoint: Endpoint,
    store: &std::path::Path,
    workspace: &std::path::Path,
) -> Config {
    let mut cfg = Config::for_this_box(workspace);
    cfg.dialect = Dialect::Qwen;
    // `leticode`: unconfined, and it names `task`/`task_result`/`task_message`. `coder` names them
    // too and refuses to open on a Mac, which would make this file measure an absent boundary.
    cfg.seat = Seat::Leticode;
    cfg.endpoint = endpoint;
    cfg.http_retries = 0;
    // A window, because a child is planned against one - a spawn with none is refused by
    // name, and the refusal is right.
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

/// A parent session with the REAL runner, opened the way a daemon opens one - its own hub
/// adopted into a shared registry, so the bell the watcher rings is one this test can ask.
struct Parent {
    /// `None` once [`Parent::close`] has dropped it - which is what a daemon's close does, and
    /// what gives the store's one-writer claim back so the session can be opened again.
    harness: Option<Harness>,
    registry: Arc<Registry>,
    /// Whether this handle is the DAEMON - the one that closes the registry when it goes. A
    /// test that opens one session three times has three `Parent`s and one registry, so the first
    /// one carries the close and the others must not.
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

    /// This handle is the daemon: it closes the registry when it is dropped, which is what
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

    /// Close the session the way the daemon does - the backend (which stops the watcher set)
    /// then the registry entry (which closes the hub, so a ring for this session has nothing to
    /// wake). What that leaves is exactly the hole
    fn close(&mut self) {
        if let Some(h) = &self.harness {
            h.close_backend();
        }
        self.registry.remove(&self.session_id);
    }

    /// Let go of the harness, which gives the store's one-writer claim back - the step that
    /// makes a second open of the same session legal.
    fn forget(&mut self) {
        self.harness = None;
    }

    /// The child's handle, read off the parent's own transcript - the `task` call's result row
    /// is the production record, not a fixture's note. `None` until that round has landed.
    fn spawned(&self) -> Option<String> {
        self.harness().items().iter().find_map(|i| match i {
            TranscriptItem::ToolResult {
                name,
                outcome: ToolOutcome::Backgrounded { handle, .. },
                ..
            } if name == "task" => Some(handle.clone()),
            _ => None,
        })
    }

    /// Tell the child something, through the tool a parent actually uses: the message is a
    /// command on the child's own hub, so this is the production door and not a second prompt.
    fn message(&self, handle: &str, text: &str) -> String {
        let rt = self.harness().runtime_handle();
        let call = letibot_transcript::ToolCall {
            id: "call_task_message".into(),
            name: "task_message".into(),
            arguments: serde_json::json!({"task": handle, "text": text}).to_string(),
        };
        let mut sink = letibot_tools::NullToolSink;
        rt.invoke("t1", &call, &mut sink).payload
    }

    /// Wait for the daemon's own wake queue to name `who` - `Work::Woken` is the ring, and a
    /// ring is the only thing that makes `Harness::wake` run at all. Everything else the queue
    /// holds is dropped on the way past: the assertion is *this session was woken*.
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
        // The session goes with the handle: a hub left behind makes the next open of that id
        // `adopt` answering `Exists`, which is a fixture artefact rather than a daemon's state.
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

/// A model that answers its first request with a `task` call, and answers the child according to
/// which turn of it the request holds - the answer to the task, the answer to the parent's later
/// message, or nothing at all.
struct Stub {
    endpoint: Endpoint,
    requests: Arc<AtomicUsize>,
    /// The gate: `true` once every turn may be answered. Until then, a request whose prompt
    /// carries [`CHILD_MARKER`] waits here - which is what makes *the child has not answered yet*
    /// a state a test stands in rather than races.
    gate: Arc<(Mutex<bool>, Condvar)>,
    /// How many requests are parked at the gate - the barrier [`Stub::wait_until_held`] waits on
    /// so a test can say *the child is waiting* rather than *it has reached the endpoint*.
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
        let again = Arc::new(a_plain_answer_turn(vocab, "thinking", CHILD_AGAIN, 30));
        let later = Arc::new(a_plain_answer_turn(vocab, "thinking", CHILD_SAYS_AGAIN, 30));
        // An answer with no answer in it: the turn ran and produced nothing to tell a parent.
        let silent = Arc::new(a_plain_answer_turn(vocab, "thinking", "", 30));
        let requests = Arc::new(AtomicUsize::new(0));
        // How many of this endpoint's requests were the CHILD's, so a second child turn gets
        // words of its own without the parent's turns moving the count.
        let child_turns = Arc::new(AtomicUsize::new(0));
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
                // One thread per connection, and that is not tidiness. The child's request is
                // HELD here, so a server that answered its connections one at a time would hold
                // the socket the parent's next round needs.
                let call = Arc::clone(&call);
                let answer = Arc::clone(&answer);
                let again = Arc::clone(&again);
                let later = Arc::clone(&later);
                let silent = Arc::clone(&silent);
                let vocab = Arc::clone(&vocab);
                let seen = Arc::clone(&mine);
                let child_seen = Arc::clone(&child_turns);
                let gate = Arc::clone(&theirs);
                let held = Arc::clone(&parked);
                std::thread::spawn(move || {
                    let (line, body) = read_request(stream.try_clone().expect("clone the socket"));
                    // `/props` is not a turn and does not consume the call: a spawn asks the
                    // parent's own endpoint for its window, which is a fact about the spawn path
                    // rather than about the model.
                    if line.starts_with("GET ") {
                        let _ = write_props(stream);
                        return;
                    }
                    let n = seen.fetch_add(1, Ordering::SeqCst);
                    // The child's request, told from the parent's by whose words it holds. The marker alone is
                    // not enough
                    let asked = prompt_of(&vocab, &body);
                    let is_child = asked.contains(CHILD_MARKER) && !asked.contains(PROMPT);
                    if is_child {
                        let (lock, cv) = &*gate;
                        let mut open = lock.lock().expect("the stub's gate");
                        while !*open {
                            held.fetch_add(1, Ordering::SeqCst);
                            open = cv.wait(open).expect("the stub's gate");
                        }
                    }
                    let child_n = if is_child {
                        child_seen.fetch_add(1, Ordering::SeqCst)
                    } else {
                        0
                    };
                    // Which turn of the child's this is, told by the words of the turn that asked
                    // for it: the parent's message is in the prompt of the turn that answers it.
                    let frames: &[Frame] = if n == 0 {
                        &call
                    } else if asked.contains(LATER_SILENT) {
                        &silent
                    } else if asked.contains(LATER) {
                        &later
                    } else if child_n >= 1 {
                        &again
                    } else {
                        &answer
                    };
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

    /// Let the held turn be answered.
    fn release(&self) {
        let (lock, cv) = &*self.gate;
        *lock.lock().expect("the stub's gate") = true;
        cv.notify_all();
    }

    /// How many turns the endpoint has answered - the child's is among them, which is how the
    /// test can tell *the child ran* from *the child was spawned*.
    fn turns(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }

    /// Wait until a turn is parked at the gate - the child has asked, and has not been
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

/// What the model was handed for one request - the token ids, detokenized with the vocabulary
/// the session tokenized with. A prompt is the one thing that says who a request is for.
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

/// A turn that calls `task` and ends - the call is what arms the watcher, and this is the shape a
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

/// A parent spawns a child, the child answers, and the parent is RUNG - with nobody asking.
/// The ring (`Work::Woken`), the wake (`Harness::wake` returns a turn, not `Ok(None)`), and
/// the sentence: one `User { speaker
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
    // was opened with - the two must be one vocabulary, or the ids mean nothing.
    let stub = Stub::new(&parts.vocab);
    cfg.endpoint = stub.endpoint.clone();
    let registry = Registry::new();
    let mut parent = Parent::open(&parts, &cfg, registry).owning();
    // One real turn, whose first round calls `task`. This is the round's path: the result
    // passes through `Harness::tool_sink`, which arms the watcher. The operator's doors arm the
    // same watcher through `Harness::unprompted_sink`, and `operator_task_wakes.rs` drives those.
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

    // The ring, and it is the daemon's own queue. Nobody has asked the child anything.
    assert!(
        parent.wait_for_a_ring(parent_id),
        "the child answered and its parent was never woken — nothing was armed for it, or the \
         settlement never reached this session's queue"
    );

    // The wake. `Ok(None)` here is *the settlement did not reach the model*.
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
    // The child RAN, which is what makes the settlement above a settlement. Two requests
    // were answered: the parent's first round (the call) and the child's own turn.
    assert!(
        stub.turns() >= 2,
        "the endpoint answered {} turn(s): the child never ran",
        stub.turns()
    );
}

/// **A child's LATER turn reaches its parent**, and reads as one: the slot settled once, so a
/// second answer used to produce no settlement, no ring and no notice; now it settles again and is
/// not told as the child finishing, which it is not.
#[test]
fn a_childs_second_answer_reaches_its_parent() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = TempDir::new("child-again");
    let store = dir.path().join("sessions.db");
    let parent_id = "s-parent-again";
    seed_session(&store, parent_id, dir.path());
    let mut cfg = config(parent_id, Endpoint::new("127.0.0.1", 1), &store, dir.path());
    let parts = parts_for(&cfg);
    let stub = Stub::new(&parts.vocab);
    cfg.endpoint = stub.endpoint.clone();
    let registry = Registry::new();
    let mut parent = Parent::open(&parts, &cfg, registry).owning();
    parent
        .harness_mut()
        .submit(PROMPT)
        .expect("the turn that starts the child");
    let child = parent
        .spawned()
        .expect("the `task` call's result row names the child it started");

    // The child's task answer, taken as any parent takes it: the ring, then the wake that
    // puts the notice in the conversation.
    assert!(
        parent.wait_for_a_ring(parent_id),
        "the child's first answer never rang its parent"
    );
    parent
        .harness_mut()
        .wake()
        .expect("the first settlement's turn")
        .expect("the first settlement is a turn");

    // And now the parent asks it something else, which is a turn of its own.
    let said = parent.message(&child, MORE);
    assert!(
        said.contains(&child),
        "`task_message` did not reach the child ({said})"
    );
    assert!(
        parent.wait_for_a_ring(parent_id),
        "the child's later turn ended and its parent was never woken: nothing tells a parent \
         about a child's second answer ({child})"
    );
    let woke = parent
        .harness_mut()
        .wake()
        .expect("the second settlement's turn")
        .expect("a second settlement is a turn too");
    assert!(!woke.text.is_empty(), "the wake carries its notice");

    let heard = notices(parent.harness().items());
    assert_eq!(
        heard.len(),
        2,
        "a parent hears once per answer, so there are two notices: {heard:#?}"
    );
    let again = &heard[1];
    assert!(
        again.contains("answered again on a later turn"),
        "it says the child answered again, not that it finished a second time: {again}"
    );
    assert!(
        again.contains(CHILD_AGAIN),
        "the second notice carries the second answer's first line: {again}"
    );
    assert!(
        !again.contains("the count is forty-two."),
        "and not the answer the first notice already carried: {again}"
    );
}

// ---------------------------------------------------------------------------
// And the same settlement when its session is NOT open
// ---------------------------------------------------------------------------

/// A child that answers into a session this daemon is not holding is still delivered at
/// that session's next open. `JobWatchers` is built per open, so such a settlement has
/// nowhere to go
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
    // The child's turn is HELD at the endpoint, so *it has not answered yet* is a state this
    // test stands in rather than races: the close below has to happen first.
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
    // Close the session while the child is still working: the backend (which stops the watcher
    // set), then the registry entry (which closes the hub, so the ring has nothing to wake).
    parent.close();

    // Now the child answers, into a session nobody is holding.
    stub.release();

    // The row is the only survivor, and it is written with `delivered = 0`.
    let run = wait_for_a_settled_child(&store_path, &child);
    assert_eq!(run.state, "done", "the child answered: {run:?}");
    assert!(
        !run.delivered,
        "the settlement was never delivered — nothing was holding the session: {run:?}"
    );

    // The tree is let go, and it MUST be - a session is claimed for writing at open, and a
    // second open of one a live harness holds is refused by name. `wait_for_children` is
    // deliberately NOT the barrier
    parent.forget();

    // The next open.
    let mut second = Parent::open(&parts, &cfg, parent.registry.clone());
    // What every open does - both `Sessions::harness_as` and `Sessions::open_first_with_seat` call
    // it, and this is the line inside it that reads what the daemon dropped.
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

    // And said once. The row's `delivered` column is the record, so a third open is silent.
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

/// A parent spawns a child, the child answers, and then the child runs a turn of its own - the turn a
/// settlement wakes it for. That answer reaches the parent as a SECOND settlement, said to be a later
/// turn of the same child; and a later turn that answers nothing reaches nobody.
#[test]
fn a_childs_later_turn_reaches_its_parent_too() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = TempDir::new("child-later-turn");
    let store = dir.path().join("sessions.db");
    let parent_id = "s-parent-later";
    seed_session(&store, parent_id, dir.path());
    let mut cfg = config(parent_id, Endpoint::new("127.0.0.1", 1), &store, dir.path());
    let parts = parts_for(&cfg);
    let stub = Stub::new(&parts.vocab);
    cfg.endpoint = stub.endpoint.clone();
    let registry = Registry::new();
    let mut parent = Parent::open(&parts, &cfg, registry).owning();
    parent
        .harness_mut()
        .submit(PROMPT)
        .expect("the turn that starts the child");
    let child = parent
        .spawned()
        .expect("the `task` call's result row names the child it started");

    // The first answer, and the settlement it makes.
    assert!(
        parent.wait_for_a_ring(parent_id),
        "the child's first answer never woke its parent"
    );
    parent
        .harness_mut()
        .wake()
        .expect("the wake")
        .expect("the first settlement is a turn");
    let first = notices(parent.harness().items());
    assert_eq!(first.len(), 1, "one settlement, one notice: {first:#?}");

    // The later turn: the door a message reaches a session between turns by, which is the same
    // door a settlement wakes it through.
    let hub = parent.registry.get(&child).expect("the child's hub");
    let sent = hub.submit(
        DAEMON_SUBMITTER,
        "later-turn-1",
        0,
        CommandKind::Message {
            from: parent_id.into(),
            text: LATER.into(),
        },
    );
    assert!(
        matches!(sent, ServerFrame::Accepted { .. }),
        "the message to the child was refused: {sent:?}"
    );
    // The model has answered that turn, so anything that was going to reach the parent has.
    let answered = stub.turns();
    let began = Instant::now();
    while stub.turns() == answered {
        assert!(
            began.elapsed() < PATIENCE,
            "the child never ran the later turn"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let began = Instant::now();
    let later = loop {
        if parent.harness_mut().wake().expect("the wake").is_some() {
            let said = notices(parent.harness().items());
            if said.len() > 1 {
                break said;
            }
        }
        assert!(
            began.elapsed() < PATIENCE,
            "the child answered again and its parent was never told: {:?}",
            notices(parent.harness().items())
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(later.len(), 2, "{later:#?}");
    let again = &later[1];
    assert!(
        again.contains(&child),
        "the later settlement names the child it is about ({child}): {again}"
    );
    assert!(
        again.contains("a later turn"),
        "it says it is a later turn of that child, not a second child: {again}"
    );
    assert!(
        again.contains("the other half is done too."),
        "it carries the later turn's answer: {again}"
    );
    assert!(
        !again.contains("the count is forty-two."),
        "and not the answer the first settlement already carried: {again}"
    );
    assert!(
        first[0].contains("the count is forty-two."),
        "the first settlement is unchanged: {}",
        first[0]
    );

    // A later turn that answers NOTHING is not a settlement: the model said nothing, so there is
    // nothing for the parent to be woken for.
    let quiet = notices(parent.harness().items()).len();
    let hub = parent.registry.get(&child).expect("the child's hub");
    let sent = hub.submit(
        DAEMON_SUBMITTER,
        "later-turn-2",
        0,
        CommandKind::Message {
            from: parent_id.into(),
            text: LATER_SILENT.into(),
        },
    );
    assert!(
        matches!(sent, ServerFrame::Accepted { .. }),
        "the second message to the child was refused: {sent:?}"
    );
    let answered = stub.turns();
    let began = Instant::now();
    while stub.turns() == answered {
        assert!(
            began.elapsed() < PATIENCE,
            "the child never ran the silent later turn"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    // Long enough for a settlement that should not exist to have been queued and delivered.
    let served = Instant::now();
    while served.elapsed() < Duration::from_secs(2) {
        let _ = parent.harness_mut().wake().expect("the wake");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        notices(parent.harness().items()).len(),
        quiet,
        "a later turn that produced nothing woke its parent: {:?}",
        notices(parent.harness().items())
    );
}

/// The `[task]` notices in a session's log, in order - the rows only the daemon writes.
fn notices(items: &[TranscriptItem]) -> Vec<String> {
    user_rows(items)
        .into_iter()
        .filter(|(speaker, text)| *speaker == Speaker::Agent && text.starts_with("[task] "))
        .map(|(_, text)| text)
        .collect()
}

/// Wait for the child's row to say it settled, and hand the row back - the durable fact this
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
