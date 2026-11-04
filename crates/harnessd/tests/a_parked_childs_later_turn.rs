//! A parked child's later turn reaches its parent: the notice, and `task_result`.
//! A parked child has no thread: `task_message` reopens it and the daemon's worker runs the turn, so the worker side must settle the parent's slot.
//! The tree is the daemon's: a `Sessions` with the parent open, a real `task` call, a child parked by a short window.

use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use letibot_harnessd::config::{Config, Seat};
use letibot_harnessd::sessions::{Outcome, Sessions, SubagentExit};
use letibot_harnessd::{Dialect, Parts};
use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::hub::{CommandKind, Hub};
use letibot_sessionlog::registry::{Registry, SessionWiring, Work, WorkOrIdle};
use letibot_transcript::{Speaker, TranscriptItem, UserPart};
use letibot_turn::Endpoint;

mod support;

use support::canned::Frame;
use support::qwen::{THINK_CLOSE, a_plain_answer_turn, ids, spoken};
use support::stub::{read_request, write_frames, write_props};

/// Qwen's own ids, the same values the turn crate's canned tests pin.
const THINK_OPEN: u32 = 248068;
const TOOL_CALL_OPEN: u32 = 248058;
const TOOL_CALL_CLOSE: u32 = 248059;

/// What the operator says to start the parent's first turn.
const START: &str = "start the child and carry on";

/// The subtask the parent hands the child. A marker, so a request is told to be the child's.
const SUBTASK: &str = "count the files and report\nmarker: the subtask";

/// What the parent tells the child in its later turn. A marker too, so the child's later turn is
/// told from its first.
const MESSAGE: &str = "marker: and the other half";

/// The child's answer to its task, and the first line of it is what a notice carries.
const TO_THE_TASK: &str = "the count is forty-two";

/// The child's answer to the parent's message - a later turn's answer, and the one `task_result`
/// must answer with.
const TO_THE_PARENT: &str = "the other half is forty-four";

/// How long a driver waits for something a correct build produces in milliseconds.
const PATIENCE: Duration = Duration::from_secs(120);

// ---------------------------------------------------------------------------
// The fixtures
// ---------------------------------------------------------------------------

fn config(session_id: &str, endpoint: Endpoint, store: &std::path::Path) -> Config {
    let mut cfg = Config::for_this_box("/tmp");
    cfg.dialect = Dialect::Qwen;
    // `leticode` because it is the unconfined seat that names `task`, `task_result` and
    // `task_message`; the confined seats cannot open on a box with no boundary.
    cfg.seat = Seat::Leticode;
    cfg.endpoint = endpoint;
    cfg.http_retries = 0;
    cfg.context_window = Some(32_768);
    cfg.store = Some(store.to_path_buf());
    cfg.session_id = session_id.to_string();
    cfg.web_search = None;
    cfg.permission = Vec::new();
    cfg.vocab_gguf = support::vocab::gguf();
    // The child parks in a few hundred milliseconds, which is the whole state this file is
    // about: a child whose thread has EXITED and whose session is in the store.
    cfg.child_park_after = Some(Duration::from_millis(300));
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

/// A directory that removes itself: a leaked store per run fills the disk, and the run that
/// finds out is not this one.
struct TempDir(std::path::PathBuf);

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

/// The tree is drained before the process can exit, whatever the test asserted: `exit()`
/// finalizes libllama underneath a child thread still inside it, and the last time this binary
/// did that it died of SIGSEGV after every test had passed.
struct DrainOnDrop<'s, 'p> {
    sessions: &'s std::cell::RefCell<Sessions<'p>>,
    registry: &'s Registry,
}

impl Drop for DrainOnDrop<'_, '_> {
    fn drop(&mut self) {
        // The daemon's own order, because a child only leaves on a closed hub.
        self.registry.close();
        let gave_up = matches!(
            self.sessions.borrow().drain_subagents(PATIENCE),
            SubagentExit::GaveUp { .. }
        );
        if gave_up && !std::thread::panicking() {
            panic!("a subagent thread outlived {PATIENCE:?} of waiting with its hub closed");
        }
    }
}

// ---------------------------------------------------------------------------
// The model
// ---------------------------------------------------------------------------

/// A model that answers by what the request HOLDS, so nothing here depends on the order two
/// sessions' requests reach it in. The parent's first request calls `task`, its second turn's
/// first round calls `task_message`, and every other request answers in prose.
struct Stub {
    endpoint: Endpoint,
    /// How many requests were the CHILD's - so the later turn is told from the first without trusting the
    /// marker alone.
    child_turns: Arc<AtomicUsize>,
}

impl Stub {
    fn new(vocab: &Arc<letibot_tokencore::Vocab>) -> Stub {
        let listener = TcpListener::bind("127.0.0.1:0").expect("an ephemeral port");
        let addr = listener.local_addr().unwrap();
        let call = Arc::new(a_task_call_turn(vocab, SUBTASK, 30));
        let answer = Arc::new(a_plain_answer_turn(vocab, "thinking", TO_THE_TASK, 30));
        let again = Arc::new(a_plain_answer_turn(vocab, "thinking", TO_THE_PARENT, 30));
        let plain = Arc::new(a_plain_answer_turn(vocab, "thinking", "carrying on", 30));
        let parent_turns = Arc::new(AtomicUsize::new(0));
        let child_turns = Arc::new(AtomicUsize::new(0));
        let vocab = Arc::clone(vocab);
        let parents = Arc::clone(&parent_turns);
        let children = Arc::clone(&child_turns);
        std::thread::spawn(move || {
            loop {
                let Ok((stream, _)) = listener.accept() else {
                    return;
                };
                let call = Arc::clone(&call);
                let answer = Arc::clone(&answer);
                let again = Arc::clone(&again);
                let plain = Arc::clone(&plain);
                let vocab = Arc::clone(&vocab);
                let parents = Arc::clone(&parents);
                let children = Arc::clone(&children);
                // One thread per connection: the child's turn and the parent's run at once.
                std::thread::spawn(move || {
                    let (line, body) = read_request(stream.try_clone().expect("clone the socket"));
                    // `/props` is not a turn: a spawn asks the endpoint for its window.
                    if line.starts_with("GET ") {
                        let _ = write_props(stream);
                        return;
                    }
                    let asked = prompt_of(&vocab, &body);
                    // The child's own prompt, told from the parent's: the parent's carries the
                    // subtask too, in the `task` call's arguments, so its own words are the half
                    // only the parent has.
                    let is_child = asked.contains(SUBTASK) && !asked.contains(START);
                    let frames: &[Frame] = if is_child {
                        let n = children.fetch_add(1, Ordering::SeqCst);
                        match n >= 1 || asked.contains(MESSAGE) {
                            true => &again,
                            false => &answer,
                        }
                    } else {
                        let n = parents.fetch_add(1, Ordering::SeqCst);
                        match n {
                            0 => &call,
                            _ => &plain,
                        }
                    };
                    let _ = write_frames(stream, frames);
                });
            }
        });
        Stub {
            endpoint: Endpoint::new(addr.ip().to_string(), addr.port()),
            child_turns,
        }
    }
}

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

/// A turn that calls `task` and ends.
fn a_task_call_turn(vocab: &letibot_tokencore::Vocab, prompt: &str, n_prompt: u64) -> Vec<Frame> {
    a_call_turn(vocab, "task", "prompt", prompt, n_prompt)
}

/// One tool call in the Qwen dialect's own shape, and the end of the turn.
fn a_call_turn(
    vocab: &letibot_tokencore::Vocab,
    tool: &str,
    field: &str,
    value: &str,
    n_prompt: u64,
) -> Vec<Frame> {
    let call =
        format!("\n<function={tool}>\n<parameter={field}>\n{value}\n</parameter>\n</function>\n");
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
        n_decoded: (call_ids.len() + 4) as u64,
        n_prompt,
        cache_n: 0,
    });
    frames
}

// ---------------------------------------------------------------------------
// The daemon
// ---------------------------------------------------------------------------

/// The daemon's own door: a command on the session's hub, taken the way the worker takes it.
fn cmd_of(hub: &Hub, text: &str) -> letibot_sessionlog::hub::QueuedCommand {
    assert!(
        hub.submit_daemon(
            "test",
            "test-1",
            CommandKind::Prompt {
                text: text.to_string(),
                attachments: Vec::new(),
            },
        ),
        "the hub refused the prompt"
    );
    hub.try_command().expect("a command is on the queue")
}

/// A session's user rows, as (speaker, text).
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
// THE ONE THIS CHANGE EXISTS FOR
// ---------------------------------------------------------------------------

/// A parked child's later turn, run by the daemon's worker, is the parent's news: the parent's
/// queue gets the settlement and `task_result` answers the later answer rather than the first.
#[test]
fn a_parked_childs_later_turn_reaches_its_parent() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = TempDir::new("harnessd-parked-later");
    let path = dir.path().join("sessions.db");
    let parent_id = "s-parent-of-a-parked-child";

    let parts = parts_for(&config(parent_id, Endpoint::new("127.0.0.1", 1), &path));
    let stub = Stub::new(&parts.vocab);
    let cfg = config(parent_id, stub.endpoint.clone(), &path);

    let registry = Registry::new();
    // The store source the daemon installs at startup (`cli.rs`): `resumable` is what reopens a
    // parked child, and it answers from this and nothing else.
    registry
        .set_source(letibot_harnessd::sessions::StoreSessions::open(&cfg).expect("a store source"));
    registry
        .create(parent_id, "the parent", wiring(&cfg))
        .expect("the parent is created");
    let sessions = std::cell::RefCell::new(
        Sessions::open_first(&parts, cfg.clone(), registry.clone()).expect("the daemon opens"),
    );
    let _drained = DrainOnDrop {
        sessions: &sessions,
        registry: &registry,
    };
    let mut sessions = sessions.borrow_mut();

    // The parent's first turn starts the child.
    let parent_hub = registry.get(parent_id).expect("the parent's hub");
    let first = cmd_of(&parent_hub, START);
    let outcome = sessions.dispatch(parent_id, &first);
    assert!(
        matches!(outcome, Outcome::Replied(_)),
        "the parent's first turn did not run (`Replied` expected)"
    );
    let child = parent_hub
        .retained()
        .iter()
        .find_map(|e| match &e.event {
            SessionEvent::Subagent { subagent_id, .. } => Some(subagent_id.clone()),
            _ => None,
        })
        .expect("the `task` call's row names the child it started");

    // The child answers, and then PARKS: its own thread exits and its session is in the store,
    // which is the state this file is about. `park_refusal` is the session's own answer for
    // *a child of mine still has a thread*, so its silence is the thread being gone.
    let began = Instant::now();
    while sessions
        .harness_of(parent_id)
        .and_then(|h| h.park_refusal())
        .is_some()
    {
        assert!(
            began.elapsed() < PATIENCE,
            "`{child}` never parked, so the worker never runs a turn for it"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        registry.get(&child).is_none(),
        "a parked child has no live hub, and this one still does"
    );
    assert!(
        stub.child_turns.load(Ordering::SeqCst) >= 1,
        "the child never answered its task"
    );

    // The parent messages it, through the tool a parent actually uses: that is the door that
    // reopens the parked session and puts the turn on a queue the worker owns.
    let said = {
        let rt = sessions
            .harness_of(parent_id)
            .expect("the parent is open")
            .runtime_handle();
        let call = letibot_transcript::ToolCall {
            id: "call_task_message".into(),
            name: "task_message".into(),
            arguments: serde_json::json!({"task": child, "text": MESSAGE}).to_string(),
        };
        let mut sink = letibot_tools::NullToolSink;
        rt.invoke("t1", &call, &mut sink).payload
    };
    assert!(
        said.contains("reopened") && said.contains("a turn started for the message"),
        "the message did not reopen the parked child: {said}"
    );

    // The worker runs the child's later turn - the command the reopen put on its queue.
    let mut ran_the_child = false;
    let began = Instant::now();
    while !ran_the_child {
        assert!(
            began.elapsed() < PATIENCE,
            "nothing reached the worker for `{child}`: the parent never reopened it"
        );
        match registry.next_work_until(Some(Instant::now() + Duration::from_millis(50))) {
            WorkOrIdle::Work(Work::Command(id, cmd)) if id == child => {
                let outcome = sessions.dispatch(&child, &cmd);
                assert!(
                    matches!(outcome, Outcome::Replied(_)),
                    "the child's later turn did not run (`Replied` expected)"
                );
                ran_the_child = true;
            }
            WorkOrIdle::Work(_) | WorkOrIdle::Idle | WorkOrIdle::Closed => {}
        }
    }
    assert!(
        stub.child_turns.load(Ordering::SeqCst) >= 2,
        "the child ran no second turn"
    );

    // And the parent was rung for it. A settlement the worker queued on the parent is a wake
    // on the parent; nothing else here rings the parent's bell.
    let mut rung = false;
    let began = Instant::now();
    while !rung {
        assert!(
            began.elapsed() < PATIENCE,
            "the parent was never rung for its child's later turn: no notice, no wake"
        );
        match registry.next_work_until(Some(Instant::now() + Duration::from_millis(50))) {
            WorkOrIdle::Work(Work::Woken(id)) if id == parent_id => rung = true,
            WorkOrIdle::Work(_) | WorkOrIdle::Idle | WorkOrIdle::Closed => {}
        }
    }

    // The wake's turn says it, in the sentence a later turn gets rather than as the child
    // finishing a second time.
    let outcome = sessions.wake(parent_id);
    assert!(
        matches!(outcome, Outcome::Replied(_)),
        "the parent's wake ran nothing (`Replied` expected)"
    );
    let rows = user_rows(
        sessions
            .harness_of(parent_id)
            .expect("the parent is open")
            .items(),
    );
    let notice = rows
        .iter()
        .find(|(s, text)| *s == Speaker::Agent && text.contains("answered again"))
        .map(|(_, text)| text.clone())
        .unwrap_or_else(|| panic!("the parent was told nothing about the later turn: {rows:#?}"));
    assert!(
        notice.contains(TO_THE_PARENT),
        "the notice carries the later turn's answer: {notice}"
    );

    // And `task_result` answers with it, not with the answer to the task.
    let rt = sessions
        .harness_of(parent_id)
        .expect("the parent is open")
        .runtime_handle();
    let call = letibot_transcript::ToolCall {
        id: "call_task_result".into(),
        name: "task_result".into(),
        arguments: serde_json::json!({"task": child}).to_string(),
    };
    let mut sink = letibot_tools::NullToolSink;
    let result = rt.invoke("t1", &call, &mut sink);
    assert!(
        result.payload.contains(TO_THE_PARENT) && !result.payload.contains(TO_THE_TASK),
        "`task_result` answered with something other than the child's last word: {}",
        result.payload
    );
}
