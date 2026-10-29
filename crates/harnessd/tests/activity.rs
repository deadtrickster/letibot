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
//! **The real-turn half lives in `activity_turn.rs`**, and it has to be a binary of its own: it
//! points a provider session at a loopback fake by writing a `providers.toml` under a scratch
//! `XDG_CONFIG_HOME`, which means `set_var`, which is only sound in a process whose other tests
//! are not reading the environment. Two tests in one binary sharing that is the race `std::env`
//! documents; a file per test is how this suite already keeps promises it cannot keep in a `cfg`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize};
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
    // **The prompt's own span**, which the harness sets and clears around a turn. This test drives it
    // by hand because there is no harness here; the real-turn test below is where it is set by
    // `begin_turn_clock`/`end_turn_clock` on a live session.
    let prompt = Arc::new(AtomicBool::new(false));
    hub.set_activity_source(activity::source(
        Some(watch.clone()),
        None,
        cursor.clone(),
        board.clone(),
        prompt.clone(),
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

    // **A card open on a session whose PROMPT is still in flight shows the card and not the turn**
    // — the prompt span is up (`prompt_in_flight`), a call of it is running, and nobody has
    // answered: *will this session act again by itself* is NO, and the list must not claim both.
    prompt.store(true, std::sync::atomic::Ordering::SeqCst);
    hub.publish(SessionEvent::TurnStarted {
        turn_id: "t2".into(),
        model: "m".into(),
        ledger_head: "h".into(),
        began_ms: None,
    });
    hub.publish(SessionEvent::ToolCallProposed {
        turn_id: "t2".into(),
        call_id: "c2".into(),
        name: "bash".into(),
        args_digest: "fnv1a:2".into(),
        target: "git push".into(),
    });
    hub.publish(SessionEvent::TurnFinished {
        turn_id: "t2".into(),
        finish_reason: letibot_sessionlog::FinishReason::Eos,
        usage: letibot_sessionlog::Usage::default(),
        timings: letibot_sessionlog::Timings::default(),
    });
    hub.publish(letibot_sessionlog::testing::requested("adj-2", "git push"));
    let busy = hub.activity();
    assert_eq!(
        busy.waiting_on(),
        &[Waiting::Ask("adj-2".into())],
        "a card is shown instead of the prompt it stopped: {}",
        said(&busy)
    );
    assert!(
        !busy.moves_by_itself(),
        "and a stopped session is stopped, whatever its turn is doing: {}",
        said(&busy)
    );
    // Answered: the prompt's span is what is left, and it is a turn again.
    hub.publish(letibot_sessionlog::testing::answered("adj-2", "allow_once"));
    assert_eq!(
        hub.activity().waiting_on(),
        &[Waiting::Turn, Waiting::Calls],
        "{}",
        said(&hub.activity())
    );
    // **The prompt's span alone is a turn**, with no event at all — which is the whole point of
    // `Waiting::Turn` being the prompt and not a round: nothing is published between a round's last
    // call finishing and the next round starting, and the value must not move there.
    prompt.store(false, std::sync::atomic::Ordering::SeqCst);
    hub.publish(SessionEvent::ToolFinished {
        turn_id: "t2".into(),
        call_id: "c2".into(),
        outcome: letibot_transcript::ToolOutcome::Ok,
        payload_digest: "fnv1a:2".into(),
        inline_bytes: 1,
        full_bytes: 1,
        spill: None,
        repairs: 0,
        edit: None,
    });
    hub.publish(SessionEvent::TurnFinished {
        turn_id: "t2".into(),
        finish_reason: letibot_sessionlog::FinishReason::Eos,
        usage: letibot_sessionlog::Usage::default(),
        timings: letibot_sessionlog::Timings::default(),
    });
    assert!(
        hub.activity().is_quiet(),
        "with the span cleared the round boundary is quiet — which is why the span, and not the \
         round, is what `Turn` is read from: {}",
        said(&hub.activity())
    );

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
        Arc::new(AtomicBool::new(false)),
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
        vec![("s-kid".to_string(), BackgroundKind::Subagent, None)],
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

/// **A runner whose child has already answered** — so the watcher thread settles on its first pass
/// and the handle really leaves the set. `collect` answers `Done` to the `watch` probe as well,
/// which is what makes the handle a subagent rather than a job.
struct Answers {
    handle: String,
}

impl TaskRunner for Answers {
    fn start(&self, _prompt: &str, _spec: &TaskSpec) -> Result<String, String> {
        Err("this runner starts nothing".into())
    }
    fn collect(&self, handle: &str, _timeout: Duration) -> TaskStatus {
        if handle == self.handle {
            TaskStatus::Done {
                answer: "the fix".into(),
                child: None,
            }
        } else {
            TaskStatus::Unknown
        }
    }
}

/// A warning, which is the shape the merge queue's `raise` publishes — one of the events whose fold
/// asks the activity to be re-derived.
fn a_warning(code: &str) -> SessionEvent {
    SessionEvent::Warning {
        code: code.into(),
        detail: "the queue's own raise".into(),
        compaction: None,
    }
}

/// **A publish returns while a session's settlement queue is held** — the deadlock the merge queue
/// spent 8½ hours in on 2026-10-29, as a property rather than as a story.
///
/// The queue's state machine publishes from inside itself
/// (`pass` → `apply_gate` → `judged` → `raise` → `publish_for_repository` → `Hub::publish`), and
/// `Hub::publish` re-derives the activity, so **anything the activity reads is a lock the queue's
/// thread holds while it holds the queue**. One thread holding the settlement queue was therefore
/// enough: `sample 53775` found the queue's worker in `JobWatchers::pending → Mutex::lock` in 795 of
/// 795 samples.
///
/// The wait is bounded on purpose: a test for a deadlock that hangs is a test that says nothing.
#[test]
fn a_publish_returns_while_the_settlement_queue_is_held() {
    let hub = Hub::new("s-held-queue");
    let watch = watchers(&hub);
    hub.set_activity_source(activity::source(
        Some(watch.clone()),
        None,
        Arc::new(AtomicUsize::new(0)),
        Arc::new(TodoBoard::new(Vec::new())),
        Arc::new(AtomicBool::new(false)),
    ));
    // A settlement in the queue, so the value the publisher re-derives is not the empty one.
    watch.queue_for_test(child_settlement("s-held-queue", "j8032"));

    let (holding_tx, holding_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let holder = {
        let watch = watch.clone();
        std::thread::spawn(move || {
            watch.hold_settlements_for_test(|| {
                holding_tx.send(()).expect("the test is waiting");
                // Hold it until the publisher is done, or until the test has given up — a holder
                // that never let go would turn one failure into a hung binary.
                let _ = release_rx.recv_timeout(Duration::from_secs(10));
            })
        })
    };
    holding_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the holder took the queue");

    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let publisher = {
        let hub = hub.clone();
        std::thread::spawn(move || {
            hub.publish(a_warning("queue_blocked"));
            let _ = done_tx.send(());
        })
    };
    let returned = done_rx.recv_timeout(Duration::from_secs(5));
    let _ = release_tx.send(());
    let _ = holder.join();
    let _ = publisher.join();
    returned.expect(
        "a publish must not wait on a session's settlement queue: the merge queue publishes from \
         inside its own state machine, so a publish that waits is a lock the queue holds while a \
         session's thread holds the queue",
    );

    // **And the facts are still right**: the settlement is still queued, and the value the publish
    // left behind says so.
    assert_eq!(
        hub.activity(),
        Activity::of(vec![Waiting::Delivery]),
        "the line a head draws: {:?}",
        hub.activity()
    );
}

/// **A session closing itself does not wedge a publisher** — the exact site of the 8½ hours.
///
/// `JobWatchers::stop` publishes the held counts as it hands them up, and it used to do that while
/// holding the settlement queue — which the activity then asked for, from the same thread. One
/// non-reentrant mutex taken twice: the closing session never returned, and every publisher on the
/// box (the merge queue's worker among them) queued behind it for the rest of the night.
///
/// A held result is the state that makes `stop` speak, so the test holds one first: that is what the
/// operator's own sessions do — a result held for the next break is the whole point of `held`.
#[test]
fn a_session_closing_itself_does_not_wedge_a_publisher() {
    let hub = Hub::new("s-stopping");
    let watch = watchers(&hub);
    hub.set_activity_source(activity::source(
        Some(watch.clone()),
        None,
        Arc::new(AtomicUsize::new(0)),
        Arc::new(TodoBoard::new(Vec::new())),
        Arc::new(AtomicBool::new(false)),
    ));
    watch.hold(vec![child_settlement("s-stopping", "j9001")]);
    assert_eq!(watch.held_count(), 1, "the held set is what makes `stop` speak");

    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let stopper = {
        let watch = watch.clone();
        std::thread::spawn(move || {
            watch.stop();
            let _ = done_tx.send(());
        })
    };
    let stopped = done_rx.recv_timeout(Duration::from_secs(5));
    let _ = stopper.join();
    stopped.expect(
        "`stop` published while holding its own settlement queue, and the publish asked for that \
         queue: one thread, one non-reentrant mutex, and every publisher behind it",
    );

    // The held result was handed up rather than dropped, and nothing is held any more.
    assert_eq!(watch.held_count(), 0, "a stopped session holds nothing");
    assert!(
        !watch.pending(),
        "its own queue was drained and handed up, so nothing is queued for it"
    );
}

/// **A watcher that has gone is not work any more** — the snapshot follows the map it is built from.
///
/// The settlement the watcher queued is `Delivery` (the model is owed the turn it wakes), and the
/// child is no longer an entry: the handle leaves the set as the thread ends, and the line says so
/// before anything drains the queue.
#[test]
fn the_line_drops_a_child_whose_watcher_has_gone() {
    let hub = Hub::new("s-child-gone");
    let runner: Arc<dyn TaskRunner> = Arc::new(Answers {
        handle: "s-kid-done".into(),
    });
    let watch = watchers(&hub).with_tasks(&runner);
    let board = Arc::new(TodoBoard::new(Vec::new()));
    hub.set_activity_source(activity::source(
        Some(watch.clone()),
        None,
        Arc::new(AtomicUsize::new(0)),
        board.clone(),
        Arc::new(AtomicBool::new(false)),
    ));
    board.upsert_model(&[(
        "land the fix".into(),
        TodoStatus::InProgress,
        vec![TodoNeed::Child {
            id: "s-kid-done".into(),
        }],
    )]);

    watch.watch("s-kid-done".into());
    // The child answers at once, so the watcher ends; the handle leaves the set and the settlement
    // is queued. What is left is `Delivery` and no child.
    let ended = std::time::Instant::now() + Duration::from_secs(10);
    while !watch.watching_now().is_empty() && std::time::Instant::now() < ended {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        watch.watching_now().is_empty(),
        "the watcher settled and took its handle out: {:?}",
        watch.watching_now()
    );
    assert!(watch.pending(), "the settlement it queued is still owed");

    hub.refresh_activity();
    assert_eq!(
        hub.activity(),
        Activity::of(vec![Waiting::Delivery]),
        "a child that has gone is not work; the result it left behind is: {:?}",
        hub.activity()
    );

    // Drained, the session is quiet — and that is the value a `wait` reads as *done*.
    watch.take_completions();
    hub.refresh_activity();
    assert!(
        hub.activity().is_quiet(),
        "nothing left (got {})",
        said(&hub.activity())
    );
}
