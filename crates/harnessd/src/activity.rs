//! **What this session is waiting on, computed from the daemon's own state** — the other half of
//! [`letibot_sessionlog::activity`], which carries the vocabulary and the argument.
//!
//! # Why this crate is the one that can answer
//!
//! The bridge had to guess *will this session act again by itself* from outside, and every guess
//! was a timer: a ten-second settle window, a `wake_due` flag set by an edge off the todo board,
//! a `jobs_open` set folded from `Backgrounded` handles. Each fixed one false wake and left the
//! next — because the facts that decide it live in processes and tables an outside reader cannot
//! see:
//!
//! | the entry | where it comes from, here |
//! |---|---|
//! | [`Waiting::Turn`] | **the PROMPT**, from the moment the harness takes it until its loop returns — `Harness::prompt_in_flight`, which [`Harness::begin_turn_clock`] sets and [`Harness::end_turn_clock`] clears (["called once per prompt, never per round"](Harness::begin_turn_clock)), falling back to the hub's round-level [`Hub::turn_shape`] for a hub with no harness (a test, a replay) |
//! | [`Waiting::Calls`] | the finer detail inside that: the round's generation ended and the calls it proposed are still running — again [`Hub::turn_shape`] |
//! | [`Waiting::Delivery`] | the queue that owes the model something: the watcher set's un-drained settlements, held results and settlements taken for a turn that has not begun, a monitor that has fired since [`Harness::monitor_cursor`] moved, and the hub's own pending command or wake ([`Hub::work_pending`]) |
//! | [`Waiting::Job`], [`Waiting::Child`] | the watcher set's live threads ([`JobWatchers::activity_facts`], whose snapshot is what a publish may read) for every one whose settlement is still to come: the daemon says it, so it is a turn. The exception is the job the model PARKED (`wake: "later"`), whose settlement it said it does not need waking for; seen [`awaited`] |
//! | [`Waiting::Monitor`] | the monitors the session declared, live ([`Monitors::live_names`]) |
//! | [`Waiting::Ask`] | the cards the hub's view still has open |
//!
//! # The two rules that are not obvious, and why
//!
//! **`Turn` is the PROMPT and not a round.** The engine publishes `TurnStarted`/`TurnFinished` once
//! per round, and a round that ended with tool calls is *always* followed by another round of the
//! same prompt — the results go back and the model reads them. A `Turn` derived from that pair would
//! therefore read *quiet* at every round boundary (measured three times: the bridge's `wait` exited
//! there, QC's `wait` exited there, and R55's row stood down there), and no amount of `Calls`
//! patching closes the last window: the gap between a round's last call finishing and the next
//! round's `TurnStarted`. The daemon does know the span — it is the one
//! [`Harness::begin_turn_clock`] brackets, *"called once per prompt, never per round"* — so `Turn`
//! is read from that flag and the round's own state is only the fallback for a hub with no harness.
//! `Calls` stays as the finer detail inside it (a call of this round is still running), which is
//! what a head wants to *draw*; nothing about the answer depends on it.
//!
//! **A card open on this session is shown instead of the prompt it stopped.** The turn is still in
//! flight behind the gate — the call it is inside is blocked on the card — but *will this session
//! act again by itself* is then **no**: a person has to move first. Listing both would hand a reader
//! a contradiction the daemon could have resolved, and `Activity::moves_by_itself` (what `wait`'s
//! exit 0 rests on) would be true for a session that is stopped.
//!
//! # Where it is published, and why so few places
//!
//! [`Hub::publish`] re-derives the value after any event whose fold can have moved it
//! ([`SessionEvent::touches_activity`]) and [`Hub::set_activity`] drops a value equal to the last
//! one, so *published only when it changes* is a property of the hub and not a discipline every
//! call site has to keep. What is left for this module is the changes **no event announces**: a
//! settlement leaving a watcher's hands, a monitor fire, a prompt sitting in a queue. Those are
//! said by the harness itself, at the few points it knows them to be true
//! ([`Harness::publish_activity`]).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use letibot_sessionlog::Hub;
use letibot_sessionlog::activity::{Activity, Waiting};
use letibot_sessionlog::hub::ActivitySource;
use letibot_tools::exec::monitor::Monitors;

use crate::jobwatch::JobWatchers;

/// **Everything the value is made of**, gathered before it is spelled — so the spelling is a pure
/// function and the tests assert it without a session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Facts {
    /// **A PROMPT is in flight** — not a round. See the module header: the engine's round pair is
    /// the defect, and this is the span that covers every round of one prompt.
    pub turn: bool,
    /// That round ended and its calls are still running.
    pub calls: bool,
    /// The model is owed a turn it has not taken: a settlement queued or held, a monitor that has
    /// fired, a command or a wake waiting for the worker.
    pub delivery: bool,
    /// The jobs and children this session watches, each only if its settlement is one the work
    /// waits on — see [`awaited`].
    pub at_work: Vec<Waiting>,
    /// The monitors it armed.
    pub monitors: Vec<String>,
    /// The cards nobody has answered, oldest first.
    pub asks: Vec<String>,
}

/// **The list, in the order a reader reads it** — and the one place the entries are spelled, so a
/// head, a caller and a test cannot disagree about what *busy* means.
///
/// The order is fixed by [`Activity::of`]; what this decides is membership: an empty `Facts` is
/// [`Activity::Quiet`] and nothing else is.
pub fn compose(f: &Facts) -> Activity {
    let mut waiting_on = Vec::new();
    // **A card open on this session is shown INSTEAD of the prompt it stopped.** The turn is still
    // in flight behind the gate — the call it is inside is blocked on the card — but the answer to
    // *will this session act again by itself* is no: a person has to move first. Two entries would
    // say both, and a reader that had to reconcile them would be reading a contradiction the
    // daemon could have resolved (`Activity::moves_by_itself` is what `wait`'s exit 0 rests on).
    let carded = !f.asks.is_empty();
    if f.turn && !carded {
        waiting_on.push(Waiting::Turn);
    }
    if f.calls && !carded {
        waiting_on.push(Waiting::Calls);
    }
    if f.delivery {
        waiting_on.push(Waiting::Delivery);
    }
    waiting_on.extend(f.at_work.iter().cloned());
    waiting_on.extend(f.monitors.iter().map(|m| Waiting::Monitor(m.clone())));
    waiting_on.extend(f.asks.iter().map(|a| Waiting::Ask(a.clone())));
    Activity::of(waiting_on)
}

/// **Will this running thing hold a caller's `wait`?** No for the one thing the model parked.
///
/// **Every watched job and child counts**, because its settlement reaches the model: the daemon
/// queues it, rings the session's bell, and the turn that says it is the session acting again on
/// its own (`crate::delivery`'s `Rule::Idle` at an idle session, `Rule::TurnEnd` for a batch).
/// The activity used to ask only about the jobs the PLAN named, and the session it read quiet for
/// was one a settlement was about to wake: a false *done* that cost a package its orchestrator.
///
/// **Except `wake: "later"`**, which is the model saying it does not need waking for this one.
/// A dev server left running settles only when the session ends, so a caller told to wait for it
/// waits out its whole deadline. The tool's own sentence says that is what the word means.
pub fn awaited(wanted: Option<letibot_tools::runtime::WantedNow>) -> bool {
    !matches!(wanted, Some(letibot_tools::runtime::WantedNow::WakeLater))
}

/// **The closure a hub re-derives the value with** — see [`Hub::set_activity_source`].
///
/// Every argument is an `Arc` the harness already holds, so the closure keeps no session alive and
/// borrows nothing: it is read from whatever thread publishes, which for a settlement is the
/// watcher's own.
///
/// # The one rule this closure keeps, and what it cost to learn
///
/// **It may not wait on a lock a non-publishing thread can hold.** The daemon's merge queue calls
/// [`Hub::publish`] from inside its own state machine (`RepoQueue::pass` → `apply_gate` → `judged`
/// → `raise` → `publish_for_repository`), so anything this closure blocks on becomes a lock the
/// queue's thread holds while it holds the queue — and a thread that is holding a session's state
/// and waiting for the queue is a cycle.
///
/// Measured 2026-10-29 (`harnessd 0.5.1`, pid 53775): the queue's worker sat 795 of 795 samples in
/// `raise → publish_for_repository → Hub::publish → refresh_activity → facts_of →
/// JobWatchers::pending → Mutex::lock` for 8½ hours. The holder was a session closing itself
/// (`JobWatchers::stop` published while holding its own settlement queue), so the queue's lease
/// heartbeat stopped, a target stayed `gating` with no gate, and an approved branch sat `queued`.
///
/// So every fact here is now taken without waiting: the watcher set answers
/// [`JobWatchers::activity_facts`] from two atomics and one **leaf** lock (an `Arc` cloned and
/// released, never held across a call), and the monitors are leaf locks of the same kind: a lock
/// that is only ever held across a clone cannot be part of a cycle.
pub fn source(
    watch: Option<Arc<JobWatchers>>,
    monitors: Option<Arc<Monitors>>,
    monitor_cursor: Arc<AtomicUsize>,
    prompt_in_flight: Arc<AtomicBool>,
) -> ActivitySource {
    Arc::new(move |hub: &Hub| {
        let facts = facts_of(
            hub,
            watch.as_ref(),
            monitors.as_ref(),
            &monitor_cursor,
            &prompt_in_flight,
        );
        compose(&facts)
    })
}

/// **Gather the facts about one session** — the pull behind [`source`], public so the harness's own
/// publish points and its tests use the same reading.
pub fn facts_of(
    hub: &Hub,
    watch: Option<&Arc<JobWatchers>>,
    monitors: Option<&Arc<Monitors>>,
    monitor_cursor: &AtomicUsize,
    prompt_in_flight: &AtomicBool,
) -> Facts {
    let (round, calls) = hub.turn_shape();
    // **The prompt's own span wins**, and the hub's round is the fallback for a hub nobody is
    // running turns on: `Hub::turn_shape` says *a round is generating*, which is false for the
    // whole of a round's tool calls and for the boundary before the next round starts — the two
    // windows a caller reads as *done*.
    let turn = prompt_in_flight.load(Ordering::SeqCst) || round;
    let mut f = Facts {
        turn,
        calls,
        // **A command or a wake waiting for the worker is a delivery** — a head's prompt is the
        // model's next turn, and a wake is a settlement some thread has already queued. Without
        // this a `wait` that attached in the same breath as a `send` would read *quiet* for the
        // milliseconds before the turn started, which is the false *done* the operator reported.
        delivery: hub.work_pending(),
        asks: hub.open_ask_ids(),
        ..Facts::default()
    };
    if let Some(m) = monitors {
        // A monitor that fired and has not been delivered yet is a turn owed — the firing is
        // `Delivery::Immediate` (R79's table), so the model is woken by it.
        if m.settled_count() > monitor_cursor.load(Ordering::SeqCst) {
            f.delivery = true;
        }
        f.monitors = m.live_names();
    }
    if let Some(w) = watch {
        // **Queued, held, or taken for a turn that has not begun: each is a turn this session will
        // take by itself.** The last is the window the queue's drain opens, where a `wait` answered
        // the turn that STARTED the job. Flags and snapshot, never its maps: this is a publish.
        let watching = w.activity_facts();
        if watching.pending || watching.held > 0 || watching.in_flight {
            f.delivery = true;
        }
        for one in watching.at_work.iter() {
            if !awaited(one.wanted) {
                continue;
            }
            f.at_work.push(match one.kind {
                crate::jobwatch::BackgroundKind::Subagent => Waiting::Child(one.handle.clone()),
                crate::jobwatch::BackgroundKind::Job => Waiting::Job(one.handle.clone()),
            });
        }
        // **A settled or failed child keeps nothing open here**: the watcher removes the handle when
        // its thread ends (and refreshes this value as it goes), and by then the settlement is a
        // queued completion — which is `delivery`, and the turn it wakes is the session doing
        // something rather than waiting for it.
    }
    f
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    /// **The gap between a settlement leaving the queue and the turn it wakes starting.**
    /// The wake drains the queue (`pending`) and the held set before it has words to put in front
    /// of the model, so both are false while the woken turn has not begun — and a `wait` that read
    /// that instant answered the turn which STARTED the job. Taken for a turn is still owed.
    #[test]
    fn a_settlement_taken_for_a_turn_holds_the_activity_until_that_turn_begins() {
        use crate::delivery::{DeliveryLog, HELD_RESULTS_CAP, Rule};
        use crate::harness::Deliveries;
        use crate::jobwatch::{BackgroundKind, JobCompletion};

        let hub = Hub::new("s-gap");
        let watch = JobWatchers::watching_tasks(&hub, None);
        hub.set_activity_source(source(
            Some(Arc::clone(&watch)),
            None,
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicBool::new(false)),
        ));
        let done = JobCompletion {
            memory: None,
            kind: BackgroundKind::Job,
            owner: "s-gap".into(),
            job: "j91".into(),
            command: "sleep 5".into(),
            slug: None,
            state: "exited 0".into(),
            produced: 0,
            elapsed_ms: 4_900,
            detail: String::new(),
            arrived: SystemTime::now(),
        };
        // The settlement a watcher queues, and the queue is the whole of what a reader sees.
        watch.queue_for_test(done);
        hub.refresh_activity();
        assert_eq!(
            hub.activity(),
            Activity::of(vec![Waiting::Delivery]),
            "a settlement in the queue is a turn owed"
        );

        // **The wake's own step**, and the one that opened the gap: it takes what it is owed, so
        // the queue and the held set are both empty before any turn has a clock.
        let deliveries = Deliveries::new(
            Arc::clone(&watch),
            None,
            DeliveryLog::none(),
            HELD_RESULTS_CAP,
            "s-gap",
        );
        let (out, said) = deliveries.between_turns_unread(|_| false, Rule::TurnEnd);
        assert!(
            !out.now.is_empty() || out.batch.is_some(),
            "there is a sentence to say: the take is not empty"
        );
        assert!(!said.is_empty(), "and it says the settlement");
        assert!(!watch.pending(), "the queue is empty now");
        assert_eq!(watch.held_count(), 0, "and nothing is held");
        hub.refresh_activity();
        assert_eq!(
            hub.activity(),
            Activity::of(vec![Waiting::Delivery]),
            "the turn that says it has not begun, so the session is still about to act: {}",
            hub.activity().line()
        );

        // **And the clock is what ends it**, so a `wait` that arrives after the turn started is not
        // held by a settlement the model has already been handed.
        watch.turn_began();
        hub.refresh_activity();
        assert!(
            hub.activity().is_quiet(),
            "nothing is owed once the turn has begun: {}",
            hub.activity().line()
        );
    }

    /// **Quiet is the empty facts and nothing else**, and the entries come in the reading order a
    /// head draws them in — the property `Activity::of` and this module have to agree on, because
    /// the value is compared for equality to decide whether to publish at all.
    #[test]
    fn the_value_is_composed_in_reading_order() {
        let busy = Facts {
            turn: false,
            calls: false,
            delivery: true,
            at_work: vec![
                Waiting::Job("j8032".into()),
                Waiting::Child("s-sub-1".into()),
            ],
            monitors: vec!["ci".into()],
            asks: vec!["adj-1".into()],
        };
        assert_eq!(
            compose(&busy),
            Activity::Busy {
                waiting_on: vec![
                    Waiting::Delivery,
                    Waiting::Job("j8032".into()),
                    Waiting::Monitor("ci".into()),
                    Waiting::Child("s-sub-1".into()),
                    Waiting::Ask("adj-1".into()),
                ]
            }
        );
        assert_eq!(compose(&Facts::default()), Activity::Quiet);
        // And a delivery flag alone is busy: the model is owed a turn.
        assert!(
            !compose(&Facts {
                delivery: true,
                ..Facts::default()
            })
            .is_quiet()
        );
    }

    /// **Every running job and child counts, and the model's own `wake: "later"` is the one thing
    /// that does not**: its settlement is delivered, so it is a turn of the session's own, unless
    /// the model said it does not need waking for it, which is what a dev server left running is.
    /// `wait` reads this, so the two halves are one function.
    #[test]
    fn a_running_job_counts_unless_the_model_parked_it() {
        use letibot_tools::runtime::WantedNow;
        assert!(
            awaited(None),
            "a job nobody's plan names still ends in a turn: the daemon says every settlement"
        );
        assert!(
            awaited(Some(WantedNow::Awaited)),
            "the session waited on it"
        );
        assert!(awaited(Some(WantedNow::WakeNow)), "it asked to be woken");
        assert!(
            !awaited(Some(WantedNow::WakeLater)),
            "it said it does not need waking for it"
        );
    }

    /// **The delivery is still the plan's to decide** (a settlement an open row `needs` is handed
    /// over at once, one nothing needs is held), and the activity no longer reads the board at all,
    /// because a held settlement still becomes a turn when it lands. The two questions were one and
    /// are two; this pins the halves that must not drift back together.
    #[test]
    fn the_delivery_reads_the_plan_and_the_activity_does_not() {
        use letibot_tokencore::store::{TodoItem, TodoNeed, TodoStatus};
        let row = |needs: Vec<TodoNeed>| TodoItem {
            content: "the gate-fix test batch".into(),
            status: TodoStatus::InProgress,
            by: letibot_tokencore::store::TodoBy::Model,
            when: None,
            needs,
        };
        let board = vec![row(vec![TodoNeed::Job {
            handle: "release-build".into(),
        }])];
        let job = crate::jobwatch::BackgroundKind::Job;
        let completion = crate::jobwatch::JobCompletion {
            memory: None,
            kind: job,
            owner: "s-1".into(),
            job: "j65".into(),
            command: "cargo test --offline".into(),
            slug: Some("release-build".into()),
            state: "exited 0".into(),
            produced: 0,
            elapsed_ms: 0,
            detail: String::new(),
            arrived: std::time::SystemTime::now(),
        };
        assert_eq!(
            crate::delivery::decide(&crate::delivery::facts_of(&completion, &board, None)),
            crate::delivery::Delivery::Immediate(crate::delivery::Why::Needed),
            "a job the plan needs is delivered at once, not held"
        );
        assert_eq!(
            crate::delivery::decide(&crate::delivery::facts_of(&completion, &[], None)),
            crate::delivery::Delivery::Held,
            "and one nothing needs is held for the next break"
        );
        // **And it holds a `wait` either way**, because a held result arriving at a quiet session is
        // said as a completion and the turn that says it is the session acting again.
        assert!(awaited(None));

        // **A `needs: child` names a SESSION.** A job whose handle was written there is not a job the
        // plan waits on, which is `plan_says`'s answer and no longer the activity's question.
        let wrong = vec![row(vec![TodoNeed::Child { id: "j9".into() }])];
        assert!(
            !crate::delivery::plan_says(&wrong, "j9", None, job).1,
            "a job is not a child, however the row is spelled"
        );
        assert!(
            crate::delivery::plan_says(
                &wrong,
                "j9",
                None,
                crate::jobwatch::BackgroundKind::Subagent
            )
            .1,
            "the child it names is"
        );
    }
}
