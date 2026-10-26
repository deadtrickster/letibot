//! **What the daemon publishes as a session moves** — `PROTOCOL_VERSION` 52, end to end through
//! the machinery that publishes it.
//!
//! `crates/sessionlog/src/activity.rs` carries the argument and `crates/harnessd/src/activity.rs`
//! the composition; what is asserted here is that the two are wired to the things that move: a
//! round, the calls a round leaves, a card, a settlement queued for the model, a child the plan
//! waits on — and that a value that has not moved is not published again.
//!
//! The first test drives the hub directly, the way the daemon's own threads do: `TurnStarted`,
//! `ToolCallProposed`, `TurnFinished` and the rest are published by the engine and the gate, and
//! this publishes the same shapes at the same points. Nothing is faked past that — the watcher set
//! is a real [`JobWatchers`], with a real watcher thread blocked on a real `TaskRunner`, and the
//! board is the real [`TodoBoard`] the tool writes.
//!
//! The second is a real session with a canned server: a prompt goes in, a round calls a tool, the
//! answer comes back — and a sampler thread reads the hub the whole time, so what it asserts is
//! that the session is never reported QUIET between the first round and the last. That is the
//! property a caller's `wait` depends on, and the one round-level `TurnStarted`/`TurnFinished`
//! cannot give (see `Hub::turn_in_flight`).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use letibot_harnessd::activity;
use letibot_harnessd::jobwatch::{BackgroundKind, JobCompletion, JobWatchers};
use letibot_sessionlog::activity::{Activity, Waiting};
use letibot_sessionlog::protocol::Caps;
use letibot_sessionlog::{Hub, SessionEvent};
use letibot_tokencore::store::{TodoNeed, TodoStatus};
use letibot_tools::builtins::task::{TaskRunner, TaskSpec, TaskStatus};
use letibot_tools::builtins::todo::TodoBoard;

/// A watcher set with nothing to watch: no process host, no runner. Enough for a settlement to be
/// **queued** on — which is the state that matters here (`delivery`), and the one a caller between
/// a job ending and its wake reads.
fn watchers(hub: &Arc<Hub>) -> Arc<JobWatchers> {
    JobWatchers::watching_tasks(hub, None)
}

fn child_settlement(owner: &str, child: &str) -> JobCompletion {
    JobCompletion {
        memory: None,
        kind: BackgroundKind::Subagent,
        owner: owner.into(),
        job: child.into(),
        command: String::new(),
        slug: Some("fix-login".into()),
        state: "done".into(),
        produced: 0,
        elapsed_ms: 0,
        detail: "the answer".into(),
        arrived: SystemTime::now(),
    }
}

/// **A runner whose one child never finishes**, so a watcher thread really is blocked on it and
/// `JobWatchers::watching_now` really does see the handle. Everything else is `Unknown`, which is
/// what a runner says about a handle it never minted.
struct NeverFinishes {
    handle: String,
}

impl TaskRunner for NeverFinishes {
    fn start(&self, _prompt: &str, _spec: &TaskSpec) -> Result<String, String> {
        Err("this runner starts nothing".into())
    }
    fn collect(&self, handle: &str, _timeout: Duration) -> TaskStatus {
        if handle == self.handle {
            TaskStatus::Running {
                note: None,
                child: None,
            }
        } else {
            TaskStatus::Unknown
        }
    }
}

/// The value as one line, for a readable failure.
fn said(a: &Activity) -> String {
    if a.is_quiet() {
        "quiet".into()
    } else {
        a.line()
    }
}

/// **Every entry, in the order the session moves through them** — and the value a head that
/// attaches in the middle of it is handed.
#[test]
fn the_activity_follows_a_session_through_a_turn_a_card_and_a_settlement() {
    let hub = Hub::new("s-activity");
    let watch = watchers(&hub);
    let board = Arc::new(TodoBoard::new(Vec::new()));
    let cursor = Arc::new(AtomicUsize::new(0));
    hub.set_activity_source(activity::source(
        Some(watch.clone()),
        None,
        cursor.clone(),
        board.clone(),
    ));

    // A session nobody has touched: quiet, and saying so changes nothing.
    assert!(hub.activity().is_quiet(), "{}", said(&hub.activity()));
    let seq = hub.status().seq;
    hub.refresh_activity();
    assert_eq!(
        hub.status().seq,
        seq,
        "an unchanged value is not published again"
    );

    // **A round is generating.**
    hub.publish(SessionEvent::TurnStarted {
        turn_id: "t1".into(),
        model: "m".into(),
        ledger_head: "h".into(),
        began_ms: None,
    });
    assert_eq!(
        hub.activity(),
        Activity::of(vec![Waiting::Turn]),
        "{:?}",
        hub.activity()
    );

    // **The round ended and its call is running.** `TurnFinished` alone would read as *idle* to
    // anybody folding round-level events, which is the whole defect this event exists for.
    hub.publish(SessionEvent::ToolCallProposed {
        turn_id: "t1".into(),
        call_id: "c1".into(),
        name: "bash".into(),
        args_digest: "fnv1a:1".into(),
        target: "cargo test".into(),
    });
    hub.publish(SessionEvent::TurnFinished {
        turn_id: "t1".into(),
        finish_reason: letibot_sessionlog::FinishReason::Eos,
        usage: letibot_sessionlog::Usage::default(),
        timings: letibot_sessionlog::Timings::default(),
    });
    assert_eq!(
        hub.activity(),
        Activity::of(vec![Waiting::Calls]),
        "the calls a round proposed are work in flight: {:?}",
        hub.activity()
    );

    // A second round over the same call — the state must not flap back to quiet in between.
    hub.publish(SessionEvent::TurnStarted {
        turn_id: "t1".into(),
        model: "m".into(),
        ledger_head: "h".into(),
        began_ms: None,
    });
    assert_eq!(hub.activity(), Activity::of(vec![Waiting::Turn]));
    hub.publish(SessionEvent::ToolFinished {
        turn_id: "t1".into(),
        call_id: "c1".into(),
        outcome: letibot_transcript::ToolOutcome::Ok,
        payload_digest: "fnv1a:1".into(),
        inline_bytes: 1,
        full_bytes: 1,
        spill: None,
        repairs: 0,
        edit: None,
    });
    hub.publish(SessionEvent::TurnFinished {
        turn_id: "t1".into(),
        finish_reason: letibot_sessionlog::FinishReason::Eos,
        usage: letibot_sessionlog::Usage::default(),
        timings: letibot_sessionlog::Timings::default(),
    });
    assert!(
        hub.activity().is_quiet(),
        "nothing left: the turn is over (got {})",
        said(&hub.activity())
    );

    // **A card nobody has answered**, and the ONE entry that waits for a person.
    hub.publish(letibot_sessionlog::testing::requested("adj-1", "git push"));
    let busy = hub.activity();
    assert_eq!(busy.ask(), Some("adj-1"), "{}", said(&busy));
    assert!(
        !busy.moves_by_itself(),
        "stopped on a card is stopped: {}",
        said(&busy)
    );
    hub.publish(letibot_sessionlog::testing::answered("adj-1", "allow_once"));
    assert!(hub.activity().is_quiet(), "{}", said(&hub.activity()));

    // **A settlement the model owes a turn for** — the window QC's `wait` exited in, and the one
    // no event says: the job is over and the turn it wakes has not started. The queue is the
    // daemon's own, put there the way a watcher thread puts it.
    watch.queue_for_test(child_settlement("s-activity", "j8032"));
    hub.refresh_activity();
    assert_eq!(
        hub.activity(),
        Activity::of(vec![Waiting::Delivery]),
        "{:?}",
        hub.activity()
    );

    // **A head that attaches here is not blind**: the value travels in the snapshot, and the
    // hub says the current one again after it — the event is ephemeral, so a reconnect across a
    // change would otherwise keep the value from before the gap.
    let attached = hub.attach("tui", "tester", Caps::default(), 0);
    let snapshot = attached.snapshot.expect("a fresh attach takes a snapshot");
    assert_eq!(
        snapshot.activity,
        Activity::of(vec![Waiting::Delivery]),
        "the snapshot carries what the session is waiting on"
    );
}

/// **A child the plan waits on keeps the session busy, and a child it does not is not counted** —
/// through the real watcher set, whose thread really is blocked on the runner.
#[test]
fn a_child_the_plan_waits_on_is_an_entry_and_one_it_does_not_is_not() {
    let hub = Hub::new("s-activity-child");
    let runner: Arc<dyn TaskRunner> = Arc::new(NeverFinishes {
        handle: "s-kid".into(),
    });
    let watch = watchers(&hub).with_tasks(&runner);
    let board = Arc::new(TodoBoard::new(Vec::new()));
    hub.set_activity_source(activity::source(
        Some(watch.clone()),
        None,
        Arc::new(AtomicUsize::new(0)),
        board.clone(),
    ));

    watch.watch("s-kid".into());
    // Give the watcher thread its first pass, so the handle is really in `watching`.
    for _ in 0..200 {
        if !watch.watching_now().is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        watch.watching_now(),
        vec![("s-kid".to_string(), BackgroundKind::Subagent)],
        "the watcher is blocked on a child, and its kind was asked once at the watch"
    );

    hub.refresh_activity();
    assert!(
        hub.activity().is_quiet(),
        "a child nothing waits on is not work this session is blocked on (got {})",
        said(&hub.activity())
    );

    // The plan says it waits on the child.
    board.upsert_model(&[(
        "land the fix".into(),
        TodoStatus::InProgress,
        vec![TodoNeed::Child { id: "s-kid".into() }],
    )]);
    hub.refresh_activity();
    assert_eq!(
        hub.activity(),
        Activity::of(vec![Waiting::Child("s-kid".into())]),
        "{:?}",
        hub.activity()
    );
    assert_eq!(
        hub.activity().line(),
        "waiting on: child s-kid",
        "the words a head draws and a caller reads"
    );
}

// ---------------------------------------------------------------------------
// A real session: the sampler
// ---------------------------------------------------------------------------

#[allow(dead_code)]
#[path = "../../turn/tests/support/canned.rs"]
mod canned;

use canned::Reply;

const IM_END: u32 = 248046;
const THINK_OPEN: u32 = 248045;
const THINK_CLOSE: u32 = 248069;
const TOOL_CALL_OPEN: u32 = 248057;
const TOOL_CALL_CLOSE: u32 = 248058;

struct TempDir {
    path: std::path::PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = letibot_sessionlog::testing::scratch_root().join(format!(
            "{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
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

fn spoken(vocab: &letibot_tokencore::Vocab, ids: &[u32]) -> Vec<canned::Frame> {
    ids.iter()
        .map(|id| canned::Frame::Token {
            id: *id,
            text: vocab
                .detokenize(&[*id], true)
                .expect("one token decodes")
                .leak(),
        })
        .collect()
}

/// A round that calls ONE tool and ends — so the turn has a round boundary, and the window
/// between the round's end and the next round's start exists to be observed. `message_between_turns.rs`'s
/// fixture, kept the same so the two files cannot drift on the wire bytes that matter.
fn a_tool_call_turn(
    vocab: &letibot_tokencore::Vocab,
    call: &str,
    n_prompt: u64,
) -> Vec<canned::Frame> {
    let call_ids = vocab.tokenize_text(call).expect("tokenizes");
    let mut frames = vec![canned::Frame::Progress {
        total: n_prompt,
        processed: n_prompt,
    }];
    frames.push(canned::Frame::Token {
        id: THINK_OPEN,
        text: "",
    });
    frames.push(canned::Frame::Token {
        id: THINK_CLOSE,
        text: "",
    });
    frames.push(canned::Frame::Token {
        id: TOOL_CALL_OPEN,
        text: "<tool_call>",
    });
    frames.extend(spoken(vocab, &call_ids));
    frames.push(canned::Frame::Token {
        id: TOOL_CALL_CLOSE,
        text: "</tool_call>",
    });
    frames.push(canned::Frame::Final {
        stop_type: "eos",
        n_decoded: (call_ids.len() + 4) as u64,
        n_prompt,
        cache_n: 0,
    });
    frames
}

/// A complete round that answers in plain text.
fn a_plain_answer(
    vocab: &letibot_tokencore::Vocab,
    answer: &str,
    n_prompt: u64,
) -> Vec<canned::Frame> {
    let answer_ids = vocab.tokenize_text(answer).expect("tokenizes");
    let mut frames = vec![canned::Frame::Progress {
        total: n_prompt,
        processed: n_prompt,
    }];
    frames.extend(spoken(vocab, &answer_ids));
    frames.extend(spoken(vocab, &[IM_END]));
    frames.push(canned::Frame::Final {
        stop_type: "eos",
        n_decoded: (answer_ids.len() + 1) as u64,
        n_prompt,
        cache_n: 0,
    });
    frames
}

/// **The session is never quiet between its first round and its last.** A sampler thread reads the
/// hub while a real turn runs two rounds — the first calling a tool, the second answering — and what
/// it must never see in the middle is `Quiet`: *this session is done* is the answer a caller acts
/// on, and the round-level events cannot say when it is true.
#[test]
fn a_real_turn_is_never_quiet_between_its_rounds() {
    use letibot_harnessd::Parts;
    use letibot_harnessd::config::Config;
    use letibot_harnessd::config::Seat;
    use letibot_harnessd::harness::Harness;
    use letibot_sessionlog::registry::{Registry, SessionWiring};

    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = TempDir::new("activity-turn");
    let target = dir.path.join("plan.md");
    std::fs::write(&target, "# the plan\n").expect("the file the call reads");
    let mut cfg = Config::for_this_box(&dir.path);
    cfg.dialect = letibot_harnessd::Dialect::Qwen;
    cfg.seat = Seat::Leticode;
    cfg.http_retries = 0;
    cfg.workspace = dir.path.clone();
    cfg.session_id = "s-activity-turn".into();
    cfg.vocab_gguf = std::env::var("LETIBOT_VOCAB_GGUF")
        .ok()
        .map(std::path::PathBuf::from)
        .or_else(letibot_tokencore::apparatus::present_gguf);
    // **Absent apparatus is not a failed assertion** (`AGENTS.md`). The turn below needs a real
    // engine, and this box's vocabulary file is a SPLIT model's first shard — libllama refuses it
    // ("could not load a vocabulary"), which is a fact about the file and not about this build.
    // What that costs is the sampler, not the mechanism: the two tests above drive the same hub,
    // the same source and the same watcher set, and they run anywhere.
    let Ok(parts) = Parts::load(&cfg) else {
        eprintln!("skipping: this box's vocabulary GGUF does not load");
        return;
    };
    let call = format!(
        "\n<function=read>\n<parameter=path>\n{}\n</parameter>\n</function>\n",
        target.display()
    );
    let serv = canned::Canned::serve_replies(
        vec![
            Reply::Frames(a_tool_call_turn(&parts.vocab, &call, 30)),
            Reply::Frames(a_plain_answer(&parts.vocab, "read it", 60)),
        ],
        2,
    );
    cfg.endpoint = serv.endpoint.clone();

    let registry = Registry::new();
    let hub = registry.new_hub(cfg.session_id.clone());
    registry
        .adopt(
            hub.clone(),
            cfg.session_id.clone(),
            SessionWiring {
                model: cfg.model.clone(),
                dialect: "qwen".into(),
                endpoint: "http://127.0.0.1:1".into(),
                workspace: dir.path.display().to_string(),
            },
            None,
        )
        .expect("the session is in the registry");
    let mut h = Harness::open_with_registry(
        &parts,
        cfg.clone(),
        hub.clone(),
        None,
        None,
        registry.clone(),
    )
    .expect("the session opens");

    assert!(
        hub.activity().is_quiet(),
        "an idle session says so: {:?}",
        hub.activity()
    );

    // The sampler: every value the hub holds, in the order it held them.
    let seen: Arc<Mutex<Vec<Activity>>> = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let sampler = {
        let hub = hub.clone();
        let seen = Arc::clone(&seen);
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let now = hub.activity();
                let mut g = seen.lock().unwrap_or_else(|e| e.into_inner());
                if g.last() != Some(&now) {
                    g.push(now);
                }
                drop(g);
                std::thread::sleep(Duration::from_millis(2));
            }
        })
    };

    h.submit("read the plan and answer").expect("the turn runs");
    stop.store(true, Ordering::Relaxed);
    sampler.join().expect("the sampler stops");

    let seen = seen.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let loud: Vec<String> = seen.iter().map(said).collect();
    assert!(
        seen.iter().any(|a| a.waiting_on().contains(&Waiting::Turn)),
        "a prompt makes the session busy, and the sampler should have caught the round: {loud:?}"
    );
    // **Nothing in the middle is quiet.** The last value may be (and is, below), the first may be;
    // anything between them is the false *done* this whole mechanism exists to prevent.
    let first = seen
        .iter()
        .position(|a| a.moves_by_itself())
        .unwrap_or_else(|| panic!("no busy value at all: {loud:?}"));
    let last = seen
        .iter()
        .rposition(|a| a.moves_by_itself())
        .unwrap_or(first);
    assert!(
        seen[first..=last].iter().all(|a| !a.is_quiet()),
        "the session went quiet in the middle of its own turn: {loud:?}"
    );
    assert!(
        hub.activity().is_quiet(),
        "and once nothing is left it says so: {:?}",
        hub.activity()
    );
}
