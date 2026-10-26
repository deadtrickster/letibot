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
//! | [`Waiting::Delivery`] | the queue that owes the model something: the watcher set's un-drained settlements and held results, a monitor that has fired since [`Harness::monitor_cursor`] moved, and the hub's own pending command or wake ([`Hub::work_pending`]) |
//! | [`Waiting::Job`], [`Waiting::Child`] | the watcher set's live threads — [`JobWatchers::watching_now`] — filtered by whether the settlement would reach the model as a WAKE ([`awaited`], `crate::delivery`'s own rule) |
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
use letibot_tools::builtins::todo::TodoBoard;
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

/// **Would this running thing's settlement reach the model as a wake?** — `crate::delivery`'s own
/// rule, asked of a handle instead of a completion.
///
/// R79 decides a settlement against the plan: `Delivery::Immediate` when an open row needs it or
/// has a `when:` condition on it, when the session asked for it (`job_wait`, `task_result`), or
/// when it was started with `wake: "now"`; `Delivery::Held` otherwise, which is a result the model
/// is *shown* at its next break rather than one it is waiting for. **This is the same reading**
/// and deliberately so, because a second rule would drift: a session with a dev server left
/// running in the background is not busy for the life of the daemon, and a caller told to wait for
/// it would wait for ever (the tree already says this where `Waiting::Job` is defined).
///
/// A handle with no row and no want is therefore not counted — and the window that follows its
/// settlement is not a silence either: the settlement is queued, which is [`Waiting::Delivery`],
/// until the turn it wakes has started.
pub fn awaited(
    handle: &str,
    slug: Option<&str>,
    kind: crate::jobwatch::BackgroundKind,
    rows: &[letibot_tokencore::store::TodoItem],
    wanted: Option<letibot_tools::runtime::WantedNow>,
) -> bool {
    if matches!(
        wanted,
        Some(
            letibot_tools::runtime::WantedNow::Awaited | letibot_tools::runtime::WantedNow::WakeNow
        )
    ) {
        return true;
    }
    // **`delivery`'s own reading of the board, not a copy of it** — see [`crate::delivery::plan_says`]
    // for the two ways the copy drifted, one of which made `wait` say `quiet` about a job whose
    // settlement was about to be handed over at once.
    let (when_row, needed) = crate::delivery::plan_says(rows, handle, slug, kind);
    when_row || needed
}

/// **The closure a hub re-derives the value with** — see [`Hub::set_activity_source`].
///
/// Every argument is an `Arc` the harness already holds, so the closure keeps no session alive and
/// borrows nothing: it is read from whatever thread publishes, which for a settlement is the
/// watcher's own.
pub fn source(
    watch: Option<Arc<JobWatchers>>,
    monitors: Option<Arc<Monitors>>,
    monitor_cursor: Arc<AtomicUsize>,
    todos: Arc<TodoBoard>,
    prompt_in_flight: Arc<AtomicBool>,
) -> ActivitySource {
    Arc::new(move |hub: &Hub| {
        let facts = facts_of(
            hub,
            watch.as_ref(),
            monitors.as_ref(),
            &monitor_cursor,
            &todos,
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
    todos: &TodoBoard,
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
        // A settlement queued for this session, or a result held for its next break, is a turn it
        // will take by itself. This is the window QC's `wait` exited in (`478f415a`), and it is
        // the one fact no event on the hub can say.
        if w.pending() || w.held_count() > 0 {
            f.delivery = true;
        }
        let rows = todos.snapshot();
        for (handle, kind, slug) in w.watching_now() {
            if !awaited(&handle, slug.as_deref(), kind, &rows, w.wanted(&handle)) {
                continue;
            }
            f.at_work.push(match kind {
                crate::jobwatch::BackgroundKind::Subagent => Waiting::Child(handle),
                crate::jobwatch::BackgroundKind::Job => Waiting::Job(handle),
            });
        }
        // **A settled or failed child keeps nothing open here**: the watcher removes the handle when
        // its thread ends, and by then the settlement is a queued completion — which is `delivery`,
        // and the turn it wakes is the session doing something rather than waiting for it.
    }
    f
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// **A job counts only when the plan waits on it** — the rule the tree already stated where
    /// `waiting_on_jobs` lived (*"a server left running, or the test's `sleep 30`, must not hold a
    /// later turn open for ever"*), asked through `delivery`'s own facts so the two cannot drift.
    #[test]
    fn a_job_counts_only_when_the_work_waits_on_it() {
        use letibot_tokencore::store::{TodoItem, TodoNeed, TodoStatus};
        let row = |needs: Vec<TodoNeed>| TodoItem {
            content: "the criterion".into(),
            status: TodoStatus::InProgress,
            by: letibot_tokencore::store::TodoBy::Model,
            when: None,
            needs,
        };
        let nothing: Vec<TodoItem> = Vec::new();
        let job = crate::jobwatch::BackgroundKind::Job;
        let child = crate::jobwatch::BackgroundKind::Subagent;
        assert!(
            !awaited("j12", None, job, &nothing, None),
            "nobody is waiting on it"
        );
        assert!(
            !awaited("j12", None, job, &[row(Vec::new())], None),
            "a row that does not need it"
        );
        assert!(
            awaited(
                "j12",
                None,
                job,
                &[row(vec![TodoNeed::Job {
                    handle: "j12".into()
                }])],
                None
            ),
            "the board needs it"
        );
        assert!(
            awaited(
                "s-sub-1",
                None,
                child,
                &[row(vec![TodoNeed::Child {
                    id: "s-sub-1".into()
                }])],
                None
            ),
            "a child is a job the board needs"
        );
        assert!(
            awaited(
                "j12",
                None,
                job,
                &nothing,
                Some(letibot_tools::runtime::WantedNow::Awaited)
            ),
            "the session waited on it"
        );
        // A finished row waits on nothing: the work it described is over.
        let done = TodoItem {
            status: TodoStatus::Completed,
            ..row(vec![TodoNeed::Job {
                handle: "j12".into(),
            }])
        };
        assert!(!awaited("j12", None, job, &[done], None));
    }

    /// **The false wake this rule was fixed for**: a model that NAMED its job and wrote the name
    /// into the plan. `bash(slug: "release-build")` hands back `j65`, `job_output` takes either, and
    /// a row that says `needs: job release-build` is a row the plan is blocked on.
    ///
    /// The delivery has always read the slug (`delivery::named`), so the settlement was handed over
    /// at once — while the activity, whose own copy of the predicate compared the handle and nothing
    /// else, said `quiet`. `wait` reads the activity: it returned *the turn ended* about a session
    /// that was about to be woken by exactly that job. The two now read one predicate, and this test
    /// asserts both sides of it, because a fix on either side alone would leave them drifting.
    #[test]
    fn a_job_named_by_its_slug_counts_and_the_delivery_agrees() {
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
        let child = crate::jobwatch::BackgroundKind::Subagent;
        assert!(
            awaited("j65", Some("release-build"), job, &board, None),
            "the plan names the job by the name the model gave it"
        );
        // ...and the delivery decision reaches the same reading, which is what makes the activity a
        // prediction rather than a second opinion.
        let completion = crate::jobwatch::JobCompletion {
            memory: None,
            kind: job,
            owner: "s-1".into(),
            job: "j65".into(),
            command: "cargo test --offline".into(),
            slug: Some("release-build".into()),
            // A job that exited 0 — the routine success the rule is about. A failed one is
            // `DependencyFailed`, which is a different arm and a truer one.
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

        // **The other half, and it must not become busy**: a `needs: child` names a SESSION. A job
        // whose handle was written there is not a job this plan waits on, and counting it was the
        // activity's own second divergence — a session that read `waiting on: job j9` for a result
        // no delivery would ever hand over.
        let wrong = vec![row(vec![TodoNeed::Child { id: "j9".into() }])];
        assert!(
            !awaited("j9", None, job, &wrong, None),
            "a job is not a child, however the row is spelled"
        );
        assert!(
            awaited("j9", None, child, &wrong, None),
            "the child it names is — the row is about a session, not about a job"
        );
    }
}
