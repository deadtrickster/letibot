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
//! | [`Waiting::Turn`], [`Waiting::Calls`] | the hub's own view — [`Hub::turn_shape`], which is the round-level `TurnStarted`/`TurnFinished` read the way `Hub::turn_in_flight` reads it |
//! | [`Waiting::Delivery`] | the queue that owes the model something: the watcher set's un-drained settlements and held results, a monitor that has fired since [`Harness::monitor_cursor`] moved, and the hub's own pending command or wake ([`Hub::work_pending`]) |
//! | [`Waiting::Job`], [`Waiting::Child`] | the watcher set's live threads — [`JobWatchers::watching_now`] — filtered by whether the settlement would reach the model as a WAKE ([`awaited`], `crate::delivery`'s own rule) |
//! | [`Waiting::Monitor`] | the monitors the session declared, live ([`Monitors::live_names`]) |
//! | [`Waiting::Ask`] | the cards the hub's view still has open |
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
use std::sync::atomic::{AtomicUsize, Ordering};

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
    /// A round is generating.
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
    if f.turn {
        waiting_on.push(Waiting::Turn);
    }
    if f.calls {
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
    rows: &[letibot_tokencore::store::TodoItem],
    wanted: Option<letibot_tools::runtime::WantedNow>,
) -> bool {
    use letibot_tokencore::store::{TodoCondition, TodoNeed, TodoStatus};
    if matches!(
        wanted,
        Some(
            letibot_tools::runtime::WantedNow::Awaited | letibot_tools::runtime::WantedNow::WakeNow
        )
    ) {
        return true;
    }
    rows.iter()
        .filter(|r| matches!(r.status, TodoStatus::Pending | TodoStatus::InProgress))
        .any(|row| {
            matches!(&row.when, Some(TodoCondition::Job { handle: h }) if h == handle)
                || row.needs.iter().any(|n| match n {
                    TodoNeed::Job { handle: h } => h == handle,
                    TodoNeed::Child { id } => id == handle,
                    _ => false,
                })
        })
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
) -> ActivitySource {
    Arc::new(move |hub: &Hub| {
        let facts = facts_of(
            hub,
            watch.as_ref(),
            monitors.as_ref(),
            &monitor_cursor,
            &todos,
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
) -> Facts {
    let (turn, calls) = hub.turn_shape();
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
        for (handle, kind) in w.watching_now() {
            if !awaited(&handle, &rows, w.wanted(&handle)) {
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
        assert!(!awaited("j12", &nothing, None), "nobody is waiting on it");
        assert!(
            !awaited("j12", &[row(Vec::new())], None),
            "a row that does not need it"
        );
        assert!(
            awaited(
                "j12",
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
        assert!(!awaited("j12", &[done], None));
    }
}
