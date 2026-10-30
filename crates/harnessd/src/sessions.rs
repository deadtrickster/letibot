//! One daemon, several sessions: the `Harness` per `Hub`.
//!
//! ```text
//!   Registry ── "s-1" → Hub ──┐            ┌── Harness(s-1)  engine · ledger · region · store
//!            ── "s-2" → Hub ──┼─ Bell ─> Sessions ── Harness(s-2)  … its own of each
//!            ── "s-3" → Hub ──┘            └── (s-3: not opened yet)
//! ```
//!
//! # What is per session and what is shared, exactly
//!
//! Shared, because it is read-only and expensive: [`Parts`] — the vocabulary GGUF
//! and the dialect's renderer and parser. One 0.6 s `vocab_only` load for the
//! process, borrowed by every session's engine.
//!
//! Per session, because the prefix invariant is: **the `TurnEngine`, the `Session`,
//! the `TokenLedger` and its `TokenRegion`, the `ToolRuntime` and the store's
//! transcript row.** None of that is arranged here — it falls out of opening one
//! [`Harness`] per hub. `TokenLedger::new` creates its own memfd, `TokenRegion` is
//! not `Clone` and appending takes `&mut`, so *two sessions sharing a token region
//! is not an expression this crate could write.* That is the guarantee, and it is
//! structural rather than upheld.
//!
//! # Sessions are opened lazily, and that is a decision
//!
//! The daemon's first session is opened eagerly at startup, because that is where a
//! dialect that does not fit the vocabulary, a missing GGUF or an unwritable store
//! is caught — all three are silent at run time, which is why `Harness::open` raises
//! them at open time.
//!
//! A session created later by a head is opened on its **first command**. It uses
//! the same `Parts` and the same `Config`, so the three failures above cannot
//! newly appear: if they were going to, the daemon would not have started. What can
//! still fail is the store, and that failure is published as a `Warning` **on that
//! session's own log**, which is where the head that just prompted into it is
//! looking. A creation that reserved nothing and cost nothing is also what makes
//! `/new` instant rather than a two-second pause on a busy box.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::hub::{CommandKind, Owner, QueuedCommand};
use letibot_sessionlog::origin::SessionOrigin;
use letibot_sessionlog::registry::{Registry, SessionSource, SessionWiring, StoredBrief};
use letibot_tokencore::store::{Store, TodoBy, TodoItem, TodoStatus};
// **The plan's own text, and the queue it is served from** — the one renderer, so the idle check
// and the tool's reply cannot come to describe two different plans. And the children the plan may
// wait on: `ChildSessions` is the snapshot the daemon builds for them (`child_sessions` below),
// `RunningChild` is what it holds about one, and `waits_on_a_child_with_room_to_breathe` is the
// silence the operator asked for, kept beside the sentence it suppresses.
use letibot_tools::builtins::todo::{
    ChildSessions, RunningChild, RunningJobs, queue_review_in_flight, unfinished_plan,
    waits_on_a_child_with_room_to_breathe, waits_on_a_running_job, waits_on_an_undigested_ending,
    when_a_queue_reviewer_goes_quiet, when_a_young_child_grows_up, when_an_ending_is_digested,
};
use std::sync::{Arc, Mutex};

use crate::config::Config;
use crate::harness::{CompactReport, Harness, HarnessError, Parts, Reply};
use crate::turn_tail::{Door, TurnOwner, publish_failure, run_turn};

/// **The review queue's own session** — one, hardcoded, whose todo board is the queue. See the
/// module's own docs for the design and for where the rows live versus where the STATES live.
pub mod gatekeeper;

/// How long a monitor waiter blocks before re-checking whether the daemon is
/// shutting down.
///
/// **Not a poll interval.** Delivery is the condvar inside
/// [`letibot_tools::exec::monitor::Monitors::wait_for_any`], which returns the
/// instant something settles. This is the only thing a thread blocked in that
/// condvar can do about `Bell::close`, which notifies a *different* condvar. Five
/// seconds because the cost of the delay is a thread that outlives a shutdown by
/// up to five seconds, and the cost of making it shorter is a wake-up that does
/// nothing, several times a minute, forever.
const SHUTDOWN_RECHECK: std::time::Duration = std::time::Duration::from_secs(5);

/// How many times one prompt may continue itself past the context wall.
///
/// Each continuation is a whole turn plus the compaction behind it, and the room
/// check in [`crate::turn_tail::run_turn`] is what normally ends the loop. The cap is
/// the runaway guard for the cycle the room check cannot see: a turn that fills
/// the window in one round, a summary that frees it, repeat — every cycle does
/// real work, so no flag ever says stop. Three wall-cycles inside one prompt is
/// not a conversation recovering from the wall any more, it is a task that does
/// not fit the window, and the fourth wall is the operator's to answer — by
/// `/compact`, a fresh session, or a bigger `--context-window`.
///
/// **Read by the one loop that continues past a wall** — [`crate::turn_tail::run_turn`],
/// which every door of every session runs, the daemon's own and a subagent's alike
/// (R68.4). Before, the daemon's sessions and a child's harness each looped over it in
/// their own copy of the tail.
pub(crate) const WALL_CONTINUES: usize = 3;

/// How long a session sits IDLE before the unfinished-plan check is sent.
///
/// **The operator's scheduling, in their words:** *"maybe wait for a timeout actually. so send it
/// when model is idling"* — and, on the failure the turn-boundary check had, *"but certainly not
/// after my message."*
///
/// The check used to run at the END OF A TURN, on a budget of one per user turn. That has the
/// problem backwards in both directions: a session in constant conversation was checked after
/// every single exchange (the overkill the operator reported from the `rano` window), while a
/// session that went quiet with an unfinished plan was checked once and then never again — the
/// model would have to be spoken to before it was reminded. A minute of silence is the signal
/// that nobody is waiting, which is the only state in which a nudge can be anything but an
/// interruption.
///
/// A minute and not less: a turn of real work is often longer than that, and a check that
/// arrives while a person is still reading the answer they just got is the interruption this
/// exists to avoid. The window is measured from the end of the last turn, and ANY turn re-arms
/// it — a monitor's wake or a background job's completion is work too, and the model being busy
/// with something else is not idle.
///
/// **The first check of a plan, and the base of every later one**: a notice the plan has already
/// been sent comes due again at [`TodoNagClock::interval`], which doubles from here and stops at
/// [`NAG_REPEAT_CAP`].
const TODO_NAG_AFTER: Duration = Duration::from_secs(60);

/// **How long after the operator ADDS a row the check comes due** — shorter than
/// [`TODO_NAG_AFTER`], and the reason is the one that window was written for, read the other way
/// (R77).
///
/// The minute exists so a check does not land while a person is still reading the answer they
/// were just given. An operator who has just added a row to the board is not reading anything: they
/// have finished saying what they want done, and the session is idle (the worker takes the push
/// only between turns). The operator's own report — *"i added a todo to pg-noop session … and
/// didnt arrive the next nag"* — is what waiting the full minute and then not naming the row looks
/// like from their side. Ten seconds and not zero: a person adding three rows in a row, or adding
/// one and then typing the prompt that goes with it, has finished inside that, and a check that
/// started on the first of them would be the interruption the minute is for. Applied only where
/// the window it replaces is longer, so a clock with a shorter window (a test's) keeps its own.
const OPERATOR_ADD_NAG_AFTER: Duration = Duration::from_secs(10);

/// **How long after a turn the idle arm looks for a tool call nothing answered.**
///
/// A grace and not zero, for the reason [`Sessions::arm_sweep`] gives: the turn has returned, and a
/// sweep in the same instant would be looking at a transcript whose last append may not be
/// published yet.
///
/// Named because THREE doors pass it, and they are the three that run a turn on the worker's
/// thread: the command arm and the wake arm in `Daemon::run`, and [`Sessions::deliver_due_nags`]
/// here. It used to be a literal at the command's call site alone, which is how a wake-driven turn
/// came to leave no deadline behind at all — see the comment on the wake arm in `Daemon::run` —
/// and then a `daemon.rs` constant the nag door could not see, which is how the same defect came
/// back on the third door (R67 item 10). It lives beside the clock it arms so every door can.
pub const SWEEP_AFTER: Duration = Duration::from_secs(2);

/// **How long a `Sessions` being dropped waits for the threads that served its sessions.**
///
/// The same job [`Sessions::drain_subagents`] does for subagents, for the threads this daemon armed
/// itself: a `Court` owns its session's `Harness` AND its own clone of [`Parts`], so a court still
/// tearing either down when the process runs its ELF finalizers is the SIGSEGV that drain's own
/// sentence names — "a thread still inside libllama when `exit()` runs the ELF finalizers".
///
/// MEASURED 2026-10-26, or this would not exist: `--test promote_midflight` printed
/// `test result: ok. 1 passed` and then died of `Segmentation fault: 11` — **2 runs in 10** under
/// load, and never with a `panicked at` anywhere, because the crash is the exit and not the test.
/// The court was parked on its channel and woken by the disconnect, and the process was gone before
/// it finished freeing the vocabulary.
///
/// Short on purpose: a court parked between items leaves in microseconds, and a court inside a model
/// call cannot be interrupted at all — waiting minutes for one is a hang, which is the case
/// `drain_subagents` names rather than waits for.
const COURT_TEARDOWN: Duration = Duration::from_secs(2);

/// **How often this daemon reads the store for the merge-queue reviews its sessions are owed.**
///
/// The queue's ring ([`crate::mergequeue::GatekeeperDoor`]) is the fast path, and it only reaches
/// a host *this* daemon holds: a ring is a bell, and a bell is per-daemon. So the two cases the
/// ring cannot cover are exactly the ones the store has to, and both were measured on 2026-10-10:
///
/// * **A review asked for a session in another daemon.** The mover rings its own bell, the host
///   hears nothing, and ten rows sat `Due` and unspawned.
/// * **A daemon restarted between the ask and the wake.** The ring is in memory and the row is
///   not: *"restarted pg-noop and nothing moves"*.
///
/// The store is the only shared medium, and the only way to notice a row somebody else wrote is to
/// read it — so this is a clock, and it is the honest name for one. Thirty seconds is under the
/// review's own first retry rung ([`crate::mergequeue::REVIEW_RETRY_BASE_MS`]), so the host never
/// lags the bound the queue already holds the entry to, and the cost of a pass is one indexed
/// `SELECT` over a table that is one row per queue entry, against a daemon that is otherwise
/// parked on a condvar.
pub(crate) const REVIEW_PASS_EVERY: Duration = Duration::from_secs(REVIEW_PASS_SECS);

/// **How often a daemon that is still busy is looked at again for the idle exit** — the error in
/// the hour it counts (`Sessions::idle_exit_due`): a head that detaches is noticed within this, so
/// the daemon leaves an hour and at most five minutes after it was last busy. A window shorter
/// than four of these is looked at four times in it instead.
pub(crate) const IDLE_EXIT_RECHECK: Duration = Duration::from_secs(300);

/// **The same number in seconds, for the reader that has no `Duration`** — the merge queue's own
/// grace, [`crate::mergequeue::GATEKEEPER_GRACE_MS`], which is counted in milliseconds and is
/// expressed in PASSES rather than in seconds so that the two cannot drift apart: a grace shorter
/// than one pass would declare a gatekeeper missing before the clock that opens it had run.
pub(crate) const REVIEW_PASS_SECS: u64 = 30;

/// **The ceiling on the repeat's interval** — how long a stalled plan may go unmentioned.
///
/// The operator's ruling on the once-per-plan rule, verbatim: *"hmm, yes obviously it should be a
/// repeated nag."* What stays right about the old rule is the RATE: every check is a real turn and
/// therefore a real model call, so a plan stalled for nine hours must not cost 540 of them. So the
/// interval between repeats doubles per delivery of the same notice and stops here.
///
/// Thirty minutes is the working-night number: with the one-minute window, the deliveries of an
/// unchanged plan land at roughly 1, 2, 4, 8, 16, 30, 30, … minutes after it was last touched —
/// about twenty in nine hours, which is a session that keeps being reminded rather than one that is
/// shouted at or one that is left alone. Lower is a metronome again and higher is a plan that can
/// stall unnoticed for an afternoon, so this is a number to tune rather than a property of the
/// mechanism.
pub(crate) const NAG_REPEAT_CAP: Duration = Duration::from_secs(30 * 60);

/// **Whether a row is work the idle plan-check may speak about.**
///
/// The operator's ask, in their own words: *"can we handle postponed todo item properly? i.e. they
/// persist but without nag and with some counter visible to me"*. A POSTPONED row is one they set
/// aside: it stays on the board, the model still sees it (marked), and the whole of what the state
/// means is that it stops asking. So it is not work this check may speak about — while a `Pending`
/// or an `InProgress` row is, whoever wrote it.
///
/// **And a row that waits on a CHILD is not, while that child is still young.** The operator's
/// directive, 2026-10-10: *"so you are not nagged till childs are running for the first 30 minutes
/// of child life"* — a row marked in progress with a child working on it was nagged four times in
/// twenty minutes. So the fact lands here as well as in the sentence: a row whose need is a live
/// child inside [`CHILD_ROOM_TO_BREATHE`] is filtered out of the plan the check reads, which makes
/// the nag SILENT rather than a blocked line about a child that is doing what it was told.
///
/// The rule itself is the tools crate's (`waits_on_a_child_with_room_to_breathe`), for the reason
/// `the_plan_as_checked` gives about `unfinished_plan`: the predicate and the sentence it suppresses
/// have to be one reading, and the constant lives beside the sentence it decides.
///
/// **Beside `nag_should_arm` because it is the same decision**, and a free function for the same
/// reason that one is: the rule is the whole of the behaviour, and a rule that can only be
/// exercised through a daemon is a rule tested by accident. Three readers, one definition — the
/// arming decision below, `Harness::nag_notice` (the `[todo check]` text) and `due_rows` (the
/// `[todo] … is due` firing).
///
/// **And a row that waits on a RUNNING JOB is not, for the same reason one kind over.** The
/// operator's ask, 2026-10-11: *"todo can be declared to be dependent on a subsession and on a
/// job ... and that should drastically decrease nags count"* — and the failure it answers was
/// measured the same night: each of the operator's long jobs had a row nagging while it ran, and
/// there was no way to say *this waits on that job*. The job half of the rule is
/// `waits_on_a_running_job` and needs no clock to end it: a job's settlement is delivered to the
/// model as a turn of its own, and a turn's end is what re-arms this very clock, so the silence
/// ends by itself where the child's has to be given a deadline.
///
/// **And a row whose job or child has ENDED is not either, until the model has digested the
/// completion** — the operator, 2026-10-11: *"when wait on condition happens - say job finishes, we
/// must delay the nag for a bit to let the model process the result"*. Measured on pg-noop the
/// same day: the silence above ended the instant the job did, so the check named the row while
/// the completion that answered it was still on its way in. `waits_on_an_undigested_ending` reads
/// `Harness::running_jobs`' record of what was delivered when (see `JobWatchers::grace_left`): quiet
/// while the completion is queued or held, quiet at the end of the turn it was delivered in, and
/// askable from the next turn end — or once the backstop runs out.
///
/// **All the facts land in one predicate because they are one question** — *is this row work the
/// check may speak about now* — and a second filter is the two-answers failure `the_plan_as_checked`
/// exists to prevent.
pub(crate) fn the_check_may_ask_about(
    row: &TodoItem,
    children: &ChildSessions,
    jobs: &RunningJobs,
) -> bool {
    !matches!(
        row.status,
        letibot_tokencore::store::TodoStatus::Completed
            | letibot_tokencore::store::TodoStatus::Postponed
            // **And the row the operator STRUCK OFF**, for the same reason one word over: there is
            // nothing owed, so there is nothing to ask about. `Cancelled` is not `Completed` — the
            // row is a record of something that was wanted and decided against — but the check's
            // question is *what do you owe*, and the answer is nothing either way.
            | letibot_tokencore::store::TodoStatus::Cancelled
    ) && !waits_on_a_child_with_room_to_breathe(row, children)
        && !waits_on_a_running_job(row, jobs)
        && !waits_on_an_undigested_ending(row, jobs)
        // R93.B3: a merge queue's row is not nagged while its review is in flight.
        && !queue_review_in_flight(row, children)
}

/// **A status, as the STORE spells it** — the wire→store half of the one conversion the two copies
/// of `TodoStatus` need, written once because both callers are on this side of the door.
///
/// It is a `match` rather than a pass-through for the reason `Harness::todo_entry`'s is: the two
/// crates own the type separately and on purpose, so a variant added to one fails to compile here
/// rather than arriving as a row in a state nothing can name.
pub(crate) fn status_from_the_wire(
    s: letibot_sessionlog::event::TodoStatus,
) -> letibot_tokencore::store::TodoStatus {
    use letibot_sessionlog::event::TodoStatus as Wire;
    use letibot_tokencore::store::TodoStatus as Stored;
    match s {
        Wire::Pending => Stored::Pending,
        Wire::InProgress => Stored::InProgress,
        Wire::Completed => Stored::Completed,
        Wire::Postponed => Stored::Postponed,
        Wire::Cancelled => Stored::Cancelled,
    }
}

/// **The children this daemon is running, as the plan-check needs them** — the fact a board cannot
/// hold, read from the two places that do hold it.
///
/// **The task JOURNAL says who is running**, and it is the only thing that does: `TaskEntry` is
/// written at the spawn (`opening`), at the open (`running`), and at the settle (`done`/`failed`),
/// so a child in one of the first two states is one this daemon is still working on. The registry's
/// own `live` is NOT that fact — a session entry is never removed from it, so a child that finished
/// an hour ago is still `live: true` — which is why this reads both sources rather than one.
///
/// **The REGISTRY says how long it has been running**, from the session's `created_ms`, which is
/// stamped when the child's hub is adopted. A child in its `opening` phase has no session row yet
/// and reads as age zero, which is what the journal itself says about it (`elapsed: 0.0` on that
/// record) — and it is the safe direction: a spawn that has not finished opening has not been
/// running long, so it is inside the room to breathe and the check stays quiet about it. The same
/// reading, stated honestly: **the age is time since the child was OPENED**, so a child that spent
/// a while opening (a VM copy, a harness load) reads younger than it is, and the room it gets is
/// that much longer than thirty minutes of wall clock.
///
/// **And the child's own HUB says what it last showed** — `SessionStatus::last_ms` is the timestamp
/// of the last event on that session's log — which is the half that moves while a child generates
/// and while a tool writes. It is read under the hub's own lock, which is one uncontended mutex,
/// and this whole function runs once per idle window per session.
///
/// **What the daemon does NOT record, and this cannot invent**: nothing timestamps the child's last
/// TOOL CALL or its round, and `TaskSlot`'s note is written only while the child is opening (the
/// progress callback `run_to_completion` takes is called three times, all before the child's turn)
/// — which is why `task_result` no longer prints that note as the child's last word (see
/// [`child_reading`]). And a tool call *in flight* is not in `Registry::jobs` either, because
/// `publish_jobs` runs at the END of a round: the smallest honest change there is to publish a
/// child's jobs when a job STARTS, which needs a hook from the process host into the session's
/// registry mailbox and is the gap this change names rather than closes. See
/// `CHILD_QUIET_ENOUGH_TO_LOOK` for what that leaves a reader to judge.
pub(crate) fn child_sessions(
    registry: &Registry,
    tasks: &crate::tasks::TaskJournal,
    now_ms: u64,
) -> ChildSessions {
    let mut out = ChildSessions::none();
    for entry in tasks.snapshot() {
        if !matches!(entry.state.as_str(), "opening" | "running") {
            continue;
        }
        // **The journal's `running` IS the daemon's record that a turn is in flight** — the record
        // is written just before the child's first prompt is submitted and replaced at the turn's
        // end — so it is what fills `working` here, and NOT `SessionStatus::running`. That flag is
        // the ROUND's, and a child executing a ten-minute tool call would read as *no turn in
        // flight* for ten minutes. See [`child_reading`].
        let turn_started = entry.state == "running";
        out = out.running(
            entry.name.clone(),
            child_reading(registry, &entry.name, turn_started, now_ms),
        );
    }
    out
}

/// **One live child, as the daemon can see it** — the ONE place a [`RunningChild`] is filled, so
/// the idle check's snapshot and `task_result` cannot come to different answers about one child.
///
/// **Why one place.** The operator's complaint is that a parent holding a row open on a child could
/// not tell *working* from *idle* from *wedged*, and the two doors that could tell it — the idle
/// plan-check's notice and `task_result` — each had a piece of the reading. A second fill would be
/// the two-answers failure this tree refuses, one layer below the sentences that describe it.
///
/// **`turn_started` is the caller's own record of the turn, and the caller is the one that has
/// it.** For the check that is the journal's `running` state; for `task_result` it is
/// `TaskSlot::Running`, which is the same fact one layer down (the slot settles when the child's
/// turn ends). It is a parameter rather than a read because the two callers hold different records
/// of one fact and neither is a second copy of the other.
///
/// **`working` is `turn_started` OR [`Hub::turn_in_flight`]** — the union of the daemon's record
/// and the hub's view, and each covers what the other cannot: the record spans the whole of the
/// child's FIRST turn (including the round-boundary window between a round's last call finishing
/// and the next round's `TurnStarted`), while the hub's view is the only thing that sees a turn the
/// child runs AFTER its first — the second turn a `task_message` starts, when the slot has already
/// settled and the journal says `done`.
///
/// **`quiet_for` is the age of the last event on the child's own log**, or `None` for one that has
/// published nothing at all — `None` is *no evidence*, which is a different fact from *no progress*
/// and is why `ChildDoing` has a state of its own for it.
pub(crate) fn child_reading(
    registry: &Registry,
    name: &str,
    turn_started: bool,
    now_ms: u64,
) -> RunningChild {
    let age = registry
        .created_ms(name)
        .map(|born| Duration::from_millis(now_ms.saturating_sub(born)))
        .unwrap_or_default();
    // A child in `opening` has no hub yet — it is a session this daemon has not adopted — and that
    // is said as *no evidence* rather than as *no progress*: `None` and no turn.
    let Some(hub) = registry.get(name) else {
        return RunningChild {
            age,
            quiet_for: None,
            working: turn_started,
        };
    };
    let status = hub.status();
    RunningChild {
        age,
        quiet_for: (status.last_ms > 0)
            .then(|| Duration::from_millis(now_ms.saturating_sub(status.last_ms))),
        working: turn_started || hub.turn_in_flight(),
    }
}

/// **The plan as the idle check reads it** — every row, minus the ones it may not speak about.
///
/// **This is where a plan is narrowed, and deliberately the only place.** `unfinished_plan` renders
/// whatever it is handed, so the narrowing has to happen before the message and not inside it; two
/// functions each deciding what a plan *is* is the two-answers failure this tree refuses, and a
/// postponed row reaching the queue would be named as the next thing to do.
///
/// **And the WHOLE board goes beside it**, because a row's edges resolve against it: a need is met by
/// a row that is DONE, and a done row is exactly one this removes. The two are one reading taken
/// twice, never two rules — the caller passes this list and the board it came from, and
/// `unfinished_plan_for` says why they are separate arguments.
///
/// **`children` is the third half of the same reading**, and it is a parameter rather than a read
/// because a board cannot hold it: whether a child session is still running is the daemon's fact
/// ([`child_sessions`]), and a row that waits on a young one is one this check may not speak about
/// either — the operator's *"not nagged till childs are running for the first 30 minutes of child
/// life"*. **`jobs` is the fourth**, the same argument one layer down: a row that waits on a job
/// that is still running is out of the plan's hands too, and it is read from the daemon's own
/// process table rather than from the board (see `Harness::running_jobs`).
pub(crate) fn the_plan_as_checked(
    rows: &[TodoItem],
    children: &ChildSessions,
    jobs: &RunningJobs,
) -> Vec<TodoItem> {
    rows.iter()
        .filter(|row| the_check_may_ask_about(row, children, jobs))
        .cloned()
        .collect()
}

/// Should the idle check be armed for a plan of these ROWS, given what it was last NAGGED with?
///
/// **The `moved` half of the schedule, as one predicate, and it exists so the rule can be tested
/// without a daemon.** Both halves are load-bearing: a finished (or absent) plan has nothing to
/// check, and an UNCHANGED one has already been said — sending it again at the window would be the
/// nagging the operator called overkill, at a slower rate instead of a faster one. What an
/// unchanged plan earns instead is the REPEAT, at the doubled-and-capped interval
/// [`TodoNagClock::rearm`] keeps — the operator's ruling, *"hmm, yes obviously it should be a
/// repeated nag"*, against the same objection the old rule was written for: a check is a real
/// turn, so it is the RATE that has to be bounded rather than the number of times a plan may be
/// mentioned at all.
///
/// **It takes the ROWS and not a rendered notice**, and that is what makes the third half of the
/// rule — *a postponed row does not arm this* — assertable as a predicate instead of through a
/// session. The reading it takes (`the_plan_as_checked`) is the same one `Harness::nag_notice`
/// takes, so a plan whose only unfinished row is postponed is silent on both counts: no clock, and
/// no `[todo check]` text to send if one were armed by an earlier state of the plan.
fn nag_should_arm(
    todos: &[TodoItem],
    nagged: Option<&str>,
    children: &ChildSessions,
    jobs: &RunningJobs,
) -> bool {
    // **The two arguments are the two halves of one reading**: `the_plan_as_checked` is what the
    // check may SPEAK about, and the board itself is what its edges RESOLVE against — a done row is
    // exactly one the narrowing removes, and it is what satisfies a need. Both come from the ONE
    // predicate, so nothing here is a second answer to what a plan is — and `children` and `jobs` go
    // to both, because a row that waits on a young child or on a running job is removed by that same
    // predicate and a need of either kind is answered by nothing on the board at all.
    let askable = the_plan_as_checked(todos, children, jobs);
    match unfinished_plan(&askable, todos, children, jobs) {
        Some(text) => nagged != Some(text.as_str()),
        None => false,
    }
}

/// **One session's idle plan-check clock: the deadline, the notice that disarmed it, and the
/// backoff that decides when that same notice may be sent again.**
///
/// The operator's ruling that put a child on this clock too, verbatim: *"childs being just a
/// session should get nags"*. A session the daemon holds runs its clock here in `Sessions`
/// (`nags`, armed by [`Sessions::rearm_todo_nag`], delivered by `Sessions::deliver_due_nags`);
/// a subagent's harness lives on the thread its parent spawned it on, so ITS clock is the same
/// type held by `harness::serve_child`, armed and delivered on that thread. **The decision is
/// not duplicated with it**: what may be asked about is `nag_should_arm`'s one answer, the
/// window is [`TODO_NAG_AFTER`], and the text is `Harness::nag_notice`'s — the clock holds
/// only when and what-was-said, which is schedule, not policy.
///
/// `window` is a field and not the constant read directly, for the same reason `deadline.rs`
/// injects its due times: a test that waited a real minute would not be a test, so the clock
/// takes the window at construction and the production constructors — both of them — pass
/// [`TODO_NAG_AFTER`].
///
/// **And the schedule is not "once per plan".** A check is a TURN — a real model call — so a plan
/// nobody acts on must not be re-asked every window; that is the metronome the operator called
/// overkill, and it is why the interval between two deliveries of the same notice doubles. Silence
/// for ever is the other failure and the one the operator ruled on — *"hmm, yes obviously it
/// should be a repeated nag"* — so the interval is capped at [`NAG_REPEAT_CAP`] and measured from
/// the last DELIVERY rather than from the last turn: a session that keeps waking for reasons of its
/// own must not push its own reminder back for ever.
pub struct TodoNagClock {
    /// How long after the last turn or board move the check comes due.
    window: Duration,
    /// When the check comes due, or `None` for a session with nothing to check — which is
    /// the common case, and the one that keeps the waiter's sleep unbounded (the worker's
    /// `next_nag_at` returns NIL; a child's `take_own_work_until` gets `None`).
    due: Option<Instant>,
    /// **The plan notice this session was last nagged with** — the identity the backoff counts
    /// repeats of.
    ///
    /// A re-arm compares the plan against this: a different notice is a new thing to say, at the
    /// base window, and the same one is a repeat, at [`Self::interval`]. It is not the whole of
    /// the rate any more — the interval is — because the model has been told and telling it the
    /// same unchanged list again a minute later is still the nagging the operator called overkill.
    nagged: Option<String>,
    /// **When `nagged` was sent** — the anchor the repeat's interval is measured from. Written and
    /// cleared with `nagged`, so the two cannot disagree about whether a notice is outstanding.
    nagged_at: Option<Instant>,
    /// **How many times in a row that same notice has been delivered** — the backoff's exponent,
    /// one-based: the first delivery is what makes the next gap twice the window. A different
    /// notice, and the operator speaking (which clears the notice), start the ladder over.
    repeats: u32,
    /// **Whether the operator has paused this session's checks** (`/nag pause`), handed in by
    /// whoever owns the clock from the harness it reads the board from ([`Self::hold`]) — the
    /// pause is the session's fact, on its config and its row, and the clock holds only a copy
    /// refreshed at every arming. See [`crate::nag_pause`].
    hold: NagHold,
}

/// **Is the operator holding this session's plan-check, and until when** — the clock's copy of
/// the session's `/nag pause`, as an `Instant` it can arm for.
///
/// `Paused { until: None }` is a pause the operator ends by hand: nothing is armed at all, which
/// is the waiter's unbounded sleep. `Paused { until: Some(at) }` arms exactly one deadline — the
/// pause's end — so the owner wakes to END the pause and re-arm, rather than the pause quietly
/// outliving its hour because nothing was waiting for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NagHold {
    #[default]
    Running,
    Paused {
        until: Option<Instant>,
    },
}

impl TodoNagClock {
    /// The production clock: the window every session gets, whoever serves it.
    /// `pub` for `serve_child_under`'s signature, which a test drives — the constructor
    /// a caller should reach for is this one, `with_window` is the tests' door.
    pub fn new() -> Self {
        Self::with_window(TODO_NAG_AFTER)
    }

    /// A clock with a different window — the tests' door, and only theirs: driving the
    /// schedule has to be possible without sleeping through the window it pins.
    pub fn with_window(window: Duration) -> Self {
        TodoNagClock {
            window,
            due: None,
            nagged: None,
            nagged_at: None,
            repeats: 0,
            hold: NagHold::Running,
        }
    }

    /// **The session's pause, as of this arming** — set by the owner before every
    /// [`Self::rearm`] and [`Self::operator_added`], from the harness (`Harness::nag_hold`), so
    /// the clock cannot hold a pause the session no longer has. It arms nothing by itself; the
    /// next re-arm reads it.
    pub fn hold(&mut self, hold: NagHold) {
        self.hold = hold;
    }

    /// **Start (or stand down) the check, as of `now`** — the body of
    /// [`Sessions::rearm_todo_nag`], and of `serve_child`'s re-arm, so the two owners
    /// cannot drift.
    ///
    /// Four answers, and the last two are the operator's rulings:
    ///
    /// * **nothing to ask about** — a finished plan, or one whose only open row is postponed: no
    ///   deadline at all;
    /// * **the plan moved** — a new thing to say, one base [`Self::window`] from `now`;
    /// * **the same plan, already said** — the repeat, one [`Self::interval`] after the last
    ///   DELIVERY, so a turn that changes nothing does not postpone the check it owes;
    /// * **silent only because a CHILD is young** — armed for the moment that child leaves its
    ///   room to breathe. A clock is otherwise armed by a turn, a board write or a prompt, and a
    ///   child's AGE is none of those, so without this arm the silence the operator asked for would
    ///   have no end: the row would stay quiet until somebody happened to do something, and the
    ///   steering (*"be steared to check the child is not stuck"*) would arrive only by accident.
    /// * **silent only because an ended job's or child's completion is not digested yet** — armed
    ///   for the backstop's end, for the young child's reason: the grace ends at the next turn
    ///   end, and a model that read the completion and went idle takes no further turn.
    /// * **silent only because a JOB is running** — armed for NOTHING, and that is the one place
    ///   this clock's answer differs from the child's above. A running job needs no deadline of its
    ///   own: its settlement is delivered to the model as a turn, and a turn's end is what re-arms
    ///   this clock (the turn tail's end, `serve_child_under`), so the row is asked about the moment the
    ///   job is over without anything here having to know when that will be.
    pub fn rearm(
        &mut self,
        todos: &[TodoItem],
        now: Instant,
        children: &ChildSessions,
        jobs: &RunningJobs,
    ) {
        // **A paused session arms nothing but the pause's end** — the operator's `/nag pause`,
        // ahead of every other answer: what the plan says is not asked about while they hold it.
        if let NagHold::Paused { until } = self.hold {
            self.due = until;
            return;
        }
        self.due = if nag_should_arm(todos, self.nagged.as_deref(), children, jobs) {
            Some(now + self.window)
        } else if unfinished_plan(
            &the_plan_as_checked(todos, children, jobs),
            todos,
            children,
            jobs,
        )
        .is_some()
        {
            // `nagged_at` is written with `nagged` and cleared with it, and this arm is reached only
            // with a notice outstanding — so the anchor is there, and a missing one would be a
            // clock that armed nothing rather than one that fired at the wrong moment.
            self.nagged_at.map(|at| at + self.interval())
        } else {
            // **The young child's deadline, or the end of an ending's grace, whichever is first.**
            // A grace normally ends at a turn end, which re-arms this clock by itself; the deadline
            // is for the model that read the completion and went idle (see
            // `when_an_ending_is_digested`).
            when_a_young_child_grows_up(todos, children)
                .into_iter()
                .chain(when_an_ending_is_digested(todos, jobs))
                .chain(when_a_queue_reviewer_goes_quiet(todos, children))
                .min()
                .map(|left| now + left)
        };
    }

    /// **How long after the last delivery the same notice may be sent again** — the base window
    /// doubled once per delivery of it, and capped at [`NAG_REPEAT_CAP`].
    ///
    /// Doubled from the gap just used and not from the first one, because what the ladder is for is
    /// a rate that decays; capped because a reminder that has retreated into an hour is a plan that
    /// can stall unnoticed for an afternoon.
    fn interval(&self) -> Duration {
        let mut interval = self.window;
        for _ in 0..self.repeats {
            interval = (interval * 2).min(NAG_REPEAT_CAP);
        }
        interval
    }

    /// **The operator has spoken: a new idle period, and the check may be made again.**
    /// [`Sessions::note_operator_prompt`]'s half — *"but certainly not after my message"*
    /// has this as its complement: whatever they said may have changed what the plan
    /// should be, so the once-per-saying is earned back rather than spent for ever.
    ///
    /// **And the backoff goes with the notice**, which is a decision rather than tidying: what the
    /// ladder counts is REPEATS of one notice, and a message clears the notice, so there is nothing
    /// left being repeated. The next check is due one base window after the message rather than at
    /// whatever gap the old notice had grown to — which is the same rule read the other way: the
    /// person's turn earns the check back at the base rate, and *"certainly not after my message"*
    /// is about the moment it must not arrive, not about how long they must wait for it.
    pub fn prompt_arrived(&mut self) {
        self.nagged = None;
        self.nagged_at = None;
        self.repeats = 0;
    }

    /// **The operator ADDED a row to the board: the check is earned back, and comes due sooner**
    /// (R77) — [`Sessions::rearm_todo_nag`]'s operator-add door, called only when the push put an
    /// open row on their half that was not there before ([`the_operator_added`]).
    ///
    /// **Earned back** for the reason [`Self::prompt_arrived`] gives a prompt: the operator has said
    /// something about what the plan should be, and a row on the board is them saying it. Without
    /// this a session whose notice had already climbed the ladder would hear about their row at
    /// whatever gap the old notice had grown to. **And sooner**: at [`OPERATOR_ADD_NAG_AFTER`]
    /// rather than the full window, for the reason that constant gives. A plan that is silent for
    /// another reason (a young child, a running job) is armed exactly as [`Self::rearm`] arms it —
    /// the shorter window replaces the base window and nothing else.
    pub fn operator_added(
        &mut self,
        todos: &[TodoItem],
        now: Instant,
        children: &ChildSessions,
        jobs: &RunningJobs,
    ) {
        self.prompt_arrived();
        self.rearm(todos, now, children, jobs);
        // **And not while the operator has paused the checks** — the sooner check their row
        // earns is still a check, and `/nag pause` says so in its reply.
        if matches!(self.hold, NagHold::Paused { .. }) {
            return;
        }
        if nag_should_arm(todos, None, children, jobs) {
            self.due = Some(now + self.window.min(OPERATOR_ADD_NAG_AFTER));
        }
    }

    /// **Record the notice a delivery just sent, as of `now`** — before the outcome is known, which
    /// is deliberate: a check whose turn failed still counts as said, or the same notice would come
    /// back every window, which is the metronome at a slower rate.
    ///
    /// **The same text again is a REPEAT and the ladder counts it**; a different text is a new thing
    /// to say, and the interval starts over — one delivery of THIS notice is what makes the next gap
    /// twice the window, and the gap before the first one is the window itself.
    pub fn said(&mut self, notice: Option<String>, now: Instant) {
        self.repeats = match (&self.nagged, &notice) {
            (Some(was), Some(is)) if was == is => self.repeats.saturating_add(1),
            _ => 1,
        };
        self.nagged = notice;
        self.nagged_at = Some(now);
    }

    /// **The deadline for whoever waits on this clock** — the worker's blocking wait for
    /// a session it holds, a child's serving thread for its own. `None` when nothing is
    /// armed, so nothing spins on behalf of a session with no check pending.
    pub fn due_at(&self) -> Option<Instant> {
        self.due
    }

    /// **Whether the check is due as of `now`, spending the deadline when it is.** The disarm is
    /// before the turn that answers it, not after: a delivery that left the old deadline in
    /// place would fire again immediately.
    ///
    /// **And a session that is NOT due keeps its deadline.** `deliver_due_nags` walks the whole
    /// map whenever ANY session (or the sweep) comes due, so a `take` here would spend the clock
    /// of every session that was not yet due — armed at `t+60`, asked at `t+1`, and then never
    /// asked again, because `next_nag_at` no longer has a deadline to hand the worker. That is
    /// the one shape this must not have: a nag that was armed and then quietly went away. The
    /// old `HashMap` got this right by filtering before it removed (`at <= now`, and only then
    /// `nag_due.remove`), and this is that order, kept where the state now lives.
    pub fn due_now(&mut self, now: Instant) -> bool {
        match self.due {
            Some(at) if at <= now => {
                self.due = None;
                true
            }
            _ => false,
        }
    }

    /// **Change the window of a clock that already exists** — the door [`Sessions::set_nag_window`]
    /// goes through, for a test that must drive a delivery on a session `open_first` already
    /// armed at the production window. The deadline already armed is left alone; the next
    /// `rearm` reads the new window.
    pub fn set_window(&mut self, window: Duration) {
        self.window = window;
    }

    /// **The deadline arrived and is being answered** — `serve_child`'s half of the
    /// disarm `due_now` performs for the worker, which learns the same fact from
    /// `OwnWork::TimedOut` instead.
    pub fn fired(&mut self) {
        self.due = None;
    }
}

/// **Did this push put a row on the operator's half that was not there before?** — the one
/// question that tells an operator ADD (or re-wording, which is a new row by its words) from the
/// head re-pushing the rows it already sent, which it does on every add, delete and HELLO (R77).
///
/// Open rows only: a row pushed as already done, or set aside, is not work the operator is asking
/// for. The words are the key, as they are everywhere on this board.
pub(crate) fn the_operator_added(before: &[TodoItem], after: &[TodoItem]) -> bool {
    let open_and_theirs = |row: &&TodoItem| {
        row.by == TodoBy::Operator
            && matches!(row.status, TodoStatus::Pending | TodoStatus::InProgress)
    };
    after.iter().filter(open_and_theirs).any(|row| {
        !before
            .iter()
            .filter(open_and_theirs)
            .any(|was| was.content == row.content)
    })
}

impl Default for TodoNagClock {
    fn default() -> Self {
        Self::new()
    }
}

/// What the worker did with one command.
pub enum Outcome {
    Replied(Box<Reply>),
    /// The session was compacted: one summary turn, then a transcript fork.
    /// The report is the evidence, not the word.
    Compacted(Box<CompactReport>),
    Failed(String),
    Ignored,
    /// **The daemon ran no turn because the session has its OWN reader**, and that reader was
    /// handed the wake — see [`Sessions::wake`]. A subagent's harness lives on the thread its
    /// parent spawned it on, so the wake goes there rather than being discarded, and this is the
    /// word for that: not `Ignored` (the condition was acted on) and not `Replied` (no turn ran
    /// here). Nothing is printed for it — a settlement inside a working tree is ordinary.
    HandedOn,
}

/// **What a session's own thread says to the daemon's log.**
///
/// A command it ran, or a wake — which has no command behind it and which the daemon's worker
/// used to print itself. The wording belongs to the daemon and the thread is not the daemon, so the
/// thread's half of it travels through a sink ([`ReplySink`]) rather than being said twice.
pub enum OutcomeSaid<'a> {
    Command(&'a QueuedCommand, &'a Outcome),
    Wake(&'a Outcome),
}

/// **The daemon's log, as the sessions served on their own threads reach it.**
///
/// `Arc` rather than a reference because one sink is shared by every session's thread and by the
/// worker loop that installed it; `Fn` rather than `FnMut` for the same reason.
pub type ReplySink = Arc<dyn Fn(&str, OutcomeSaid<'_>) + Send + Sync>;

/// **Something the daemon asks of a session whose command queue did not carry it.**
///
/// It carries its own reply (`std::sync::mpsc`) rather than the court carrying a typed one, so one
/// channel serves every door: the answer type is the closure's return.
type Job = Box<dyn FnOnce(&mut Sessions<'static>) -> Follow + Send>;

/// **What a session's thread does after a job** — carry on serving, or leave.
///
/// `Finish` is [`Sessions::drain_subagents`]'s door: the daemon is going, and this session's
/// children have to be told to stop and waited for by the thread that holds the harness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Follow {
    Serve,
    Finish,
}

/// **One session, on its own thread** — the reader that serves it.
///
/// The operator's requirement, in their words: *"one daemon, several sessions … give a thread to
/// each session and that is it"*. Before this every session the daemon held was served by ONE
/// worker, one command at a time across all of them — measured 2026-10-26 in the operator's own
/// daemon, where a prompt to session B sat queued from 14:08 to 14:10:52, exactly one second after
/// session A's three-minute turn ended.
///
/// It is the shape a subagent already had (`harness::serve_child`): the thread takes the session's
/// commands off its own hub queue, runs the turn each one makes, and keeps the session's clocks
/// (the plan-check, the abandoned-call sweep, the child park) because it is the only thread that
/// may drive the harness. What the daemon keeps is the door — [`Court::ask`] and [`Court::post`] —
/// for the requests a session's queue does not carry.
struct Court {
    /// The nudge. A job sent here does not wake the thread by itself: it is blocked in the hub's
    /// condvar, so [`Court::ask`] rings that too.
    jobs: std::sync::mpsc::Sender<Job>,
    /// `None` once it has been joined, so a second shutdown waits on nothing.
    thread: Option<std::thread::JoinHandle<()>>,
    /// What this session is holding the daemon up for, as its thread last read it — the idle
    /// exit's question ([`Sessions::idle_exit_refusal`]), answered without standing still for a
    /// turn that may be minutes long.
    status: Arc<Mutex<Option<String>>>,
    /// The hub, because a job is delivered by waking the thread that is waiting on it.
    hub: Arc<letibot_sessionlog::hub::Hub>,
}

impl Court {
    /// **Ask this session something, and wait for the answer.**
    ///
    /// For the doors a caller outside the daemon reaches synchronously — `cli.rs`'s banner, and
    /// every test that drives `Sessions` by hand. `None` is a thread that has already gone: the
    /// session is closed, and its answer is nobody's.
    fn ask<R: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Sessions<'static>) -> R + Send + 'static,
    ) -> Option<R> {
        let (tx, rx) = std::sync::mpsc::channel();
        let sent = self.jobs.send(Box::new(move |s: &mut Sessions<'static>| {
            // A caller that has given up is not an error: the work is done either way.
            let _ = tx.send(f(s));
            Follow::Serve
        }));
        sent.ok()?;
        self.hub.wake_its_own_reader();
        rx.recv().ok()
    }

    /// **Ask without waiting** — for the daemon's own pass, which must not stand still while a
    /// session is mid-turn. The daemon's review pass is the one caller that matters.
    fn post(&self, f: impl FnOnce(&mut Sessions<'static>) + Send + 'static) {
        let sent = self.jobs.send(Box::new(move |s: &mut Sessions<'static>| {
            f(s);
            Follow::Serve
        }));
        if sent.is_ok() {
            self.hub.wake_its_own_reader();
        }
    }

    /// **Tell this session's thread to leave**, and answer what its children did.
    ///
    /// The answer is `(told, left)`: whether anything was told to stop, and whether anything could
    /// not be waited for inside `patience` — [`SubagentExit`]'s two facts, per session.
    fn finish(
        &self,
        id: &str,
        patience: std::time::Duration,
        reply: std::sync::mpsc::Sender<(bool, bool)>,
    ) {
        let id = id.to_string();
        let job: Job = Box::new(move |s: &mut Sessions<'static>| {
            let held = s.open.remove(&id);
            let answer = match &held {
                Some(h) => stop_children_of(h, patience),
                None => (false, false),
            };
            if let Some(h) = held {
                s.open.insert(id.clone(), h);
            }
            let _ = reply.send(answer);
            Follow::Finish
        });
        if self.jobs.send(job).is_ok() {
            self.hub.wake_its_own_reader();
        }
    }
}

/// Every session this daemon is serving, and the harness behind each one.
pub struct Sessions<'a> {
    /// **The parts, borrowed or owned.** Borrowed by the daemon — the vocabulary and the dialect
    /// renderer are one load for the process, and `Sessions<'a>` has always borrowed them — and
    /// OWNED by each session's thread, which outlives the `&Parts` a constructor was handed.
    parts: std::borrow::Cow<'a, Parts>,
    /// The command line, minus the session id. Cloned per session with its own id
    /// substituted, so a second session is the same daemon and not a second
    /// configuration nobody typed.
    base: Config,
    /// **The review-queue session THIS daemon owns** — `s-gatekeeper-<workspace key>`, one per
    /// folder, and the same id across this daemon's restarts (see
    /// [`gatekeeper::session`]).
    ///
    /// A field rather than a call at every use, and it is one value and not a rule read
    /// repeatedly: `serve_reviews`, `fill_gatekeeper_board` and `ensure_gatekeeper` all have to
    /// agree about WHICH session is the queue, and the id is derived from the workspace, which
    /// never changes for the life of a daemon. It used to be the constant `s-gatekeeper` — one
    /// id for every daemon on the box, sharing one store — which two daemons appended to at once
    /// (MEASURED 2026-10-26: `s-gatekeeper#t0`, `transcript_item seq must be the next one`).
    gatekeeper: String,
    registry: Arc<Registry>,
    open: HashMap<String, Harness>,
    /// Sessions whose monitor waiter is already running. **One waiter per name**,
    /// which is T24 requirement 4 — the fleet has already paid for what two
    /// processes under one reader costs: the roster shows a seat attached while the
    /// real one hears nothing.
    armed: std::collections::HashSet<String>,
    /// The flowy seat this daemon holds, when it holds one. Daemon-level, on
    /// purpose: a seat is a persistent identity with one inbox reader, and a
    /// session is a temporary consumer of it — see `letibot_flowy`'s crate docs.
    /// Set at start by `--flowy`, or later by `/flowy login` from a head.
    seat: Option<letibot_flowy::Seat>,
    /// Each root session's `flowy` tool slot: the door is always seated, and the
    /// slot is filled when a seat is attached — at open, or later.
    slots: HashMap<String, letibot_flowy::tool::SeatSlot>,
    /// Each root session's condition on the seat, so a title change can rename
    /// the session's address (`@seat/title`).
    seated: HashMap<String, Arc<letibot_flowy::InboxCondition>>,
    /// The fabric block each root session last saw, and the doors its seat has — so a
    /// refresh after a compaction is a system update only when something changed, and
    /// is rendered for the same tools the session's prompt was.
    fabric_seen: HashMap<String, (letibot_flowy::FabricReach, String)>,
    /// **Each session's idle plan-check clock** — the deadline when one is armed,
    /// absent for a session with nothing to check. See [`Sessions::rearm_todo_nag`]
    /// and [`TodoNagClock`] (the one clock, shared with the thread that serves a
    /// subagent).
    nags: HashMap<String, TodoNagClock>,
    /// **The window a session's clock is made with** — [`TODO_NAG_AFTER`] in every daemon, and
    /// a field only so a test can drive a delivery without sleeping through a minute. See
    /// [`Sessions::set_nag_window`], and [`TodoNagClock::with_window`] for the same seam one
    /// level down.
    nag_window: Duration,
    /// When to next look for a tool call nothing will answer. See `arm_sweep`.
    sweep_at: Option<Instant>,
    /// **When to next look for a subagent this worker holds that has been idle long enough to
    /// park** — R68's idle-child policy (`Config::child_park_after`) for the children the daemon
    /// holds: a child REOPENED from the store (a parent's message to a parked child, a head's
    /// resume of one) is served here and not on a thread of its own, so its park clock is here
    /// too. Armed at every turn's end; `None` when no child is held. See
    /// [`Sessions::park_idle_children`].
    park_at: Option<Instant>,
    /// **When to next read the store for the merge-queue reviews this daemon's sessions are
    /// owed.** Armed at open to *now*, so a daemon that comes up between the ask and the ring
    /// serves it on its first idle pass rather than after a window, and spent by
    /// [`Sessions::reviews_due`]. See [`REVIEW_PASS_EVERY`] for why this is a clock at all.
    reviews_at: Option<Instant>,
    /// **When to next ask whether this daemon has anything left to do** — the idle exit
    /// (`Config::idle_exit_after`). `None` when the daemon never exits on its own. See
    /// [`Sessions::idle_exit_due`].
    idle_exit_at: Option<Instant>,
    /// **The last instant this worker saw the daemon busy** — a head attached, a turn, a job.
    /// The idle window counts from the later of this and the newest event, because a head that
    /// detaches may say nothing: a tab closed after two quiet hours would otherwise read as two
    /// hours idle, and the daemon would leave the moment its terminal did.
    busy_seen: Instant,
    /// **The operator's run, per session**: whether one is in flight on its own thread,
    /// and the lines queued behind it. See [`crate::bangrun::State`] — this map only holds
    /// the handle; the harness gets a clone of the same `Arc`, because the round boundary's
    /// pickup reads the same fact mid-turn.
    bangs: HashMap<String, Arc<crate::bangrun::State>>,
    /// **The last reason the review queue's session did not open, as said on a hub** — so
    /// [`Sessions::ensure_gatekeeper`], which runs every review pass, says a standing failure
    /// once and a new one again. `None` once it opens.
    gatekeeper_said: Option<String>,
    /// **Each session's plugin layers** — the stack, the recorded inputs and the panes a head has
    /// opened (W4 of `docs/plugins-slice1.md`). One per session, because the layers are discovered
    /// per session, and installed on the registry at `open` so a `PaneOpen` reaches it from the
    /// connection's thread rather than through this worker.
    plugins: HashMap<String, Arc<crate::plugins::PluginHost>>,
    /// **The sessions served on a thread of their own** — see [`Court`]. Empty on a daemon that
    /// was never told to serve them that way (`Daemon::run` is the one caller that says so), and
    /// empty on a court's own instance, which is what keeps a forwarder from forwarding to itself.
    courts: HashMap<String, Court>,
    /// **Whether a session gets a thread of its own.** Armed by `Daemon::run`, which IS the worker
    /// this replaces; a test that drives `Sessions` by hand leaves it off and drives the same
    /// single-threaded path every test has always driven. See [`Court`].
    threaded: bool,
    /// **Where a turn's outcome is said** — the daemon's log, once — so a session's thread can
    /// report an outcome it ran without owning the wording. `None` for a `Sessions` under a test.
    reply: Option<ReplySink>,
}

/// **The three facts a session the DAEMON ITSELF starts has, which the daemon's command line
/// cannot supply.** One caller today — [`Sessions::ensure_gatekeeper`] — and a struct rather than
/// three parameters because they are one decision: *this session is not the conversation the
/// operator described on the command line*. The seat decides what it may do; the title is the name
/// its own store row carries, which is what `letibot --sessions` prints; the prompt is the
/// paragraph only this session is given.
struct OwnSession {
    seat: crate::config::Seat,
    /// The name the session is opened under — the hardcoded one when it is being made, and the name
    /// its own row already holds when it is being RESUMED, so a session the operator renamed does
    /// not come back with the hardcoded name on the config and the renamed one in the store.
    title: String,
    /// **What this session is told that no other session is** — appended to the composed prompt at
    /// open, and `None` for a session the ordinary prompt is right for. See
    /// [`crate::sessions::gatekeeper::PROMPT`] for the constant paragraph, and
    /// [`crate::sessions::gatekeeper::prompt`] for why this is a `String` and not a `&'static str`:
    /// half of that paragraph is the placement its reviewers have, which is this BOX's answer
    /// (`firecode` or not) and cannot be a constant. `Config::compose_for_seat` is what appends it:
    /// the sections from `prompts.toml`, the `system_extra`, then the standing notes, then this.
    ///
    /// **It is frozen with the rest of the prefix**, which is the rule every session's prompt
    /// keeps: a session already open keeps the prompt it opened with, and `ReseatSession` is the
    /// tree's remedy for a prompt that changed under a live conversation.
    prompt: Option<String>,
}

/// Which sessions attach to the seat, and how the attachment is wired.
///
/// **Root sessions only.** A subagent spawned by `task` hears the room through
/// its parent and speaks through it: one name, one mind under it at a time. So
/// the tool and the monitor are given to a session with no `parent_session_id`
/// and to nothing else.
///
/// **Read from the registry's own entry** (R68.6), not from `Registry::list`, which read every
/// stored session (`Store::list_sessions`, under the store mutex every head's picker frame also
/// takes) on every open. A child is adopted or created WITH its parent, and a resumed one is
/// created from its stored row (`ResumeSession`, the daemon's first session in `cli.rs`), so the
/// entry has the answer.
fn is_root(registry: &Registry, session_id: &str) -> bool {
    registry.parent_of(session_id).is_none()
}

/// **What the daemon can do with a wake for a session** — and which of the three it is.
///
/// Pure, so the routing decision is assertable without a daemon, a socket or a model: it is two
/// booleans the caller already has to look up, and the whole of the operator's requirement is
/// which arms exist. The operator's design, in their words: *"think about it like it is an
/// erlang supervision tree. we talk to parents and they own lifecycle."* — and a supervisor whose
/// exits nobody can deliver is not a supervisor, so there is no arm here that throws one away
/// while anything can still act on it.
///
/// * **`Drive`** — the daemon holds this session's harness (`Sessions::open`), so the worker runs
///   the turn: `Harness::wake` for a root, and for every session the daemon opened.
/// * **`ItsOwnReader`** — the session is live in the registry, owned by its own thread
///   ([`Owner::OwnThread`]) and so not held by the daemon:
///   a subagent, whose harness lives on the thread its parent spawned it on. The daemon hands the
///   wake to that thread (`Hub::wake_its_own_reader`), which is what makes a ring naming a child
///   a promise rather than the `Ignored` R58 measured.
/// * **`Gone`** — no hub at all, or a hub the worker owns and does not hold (an open that
///   failed). Nothing to hand anything to, and nothing lost: a session that
///   closed handed its undrained settlements up a level first (`jobwatch::JobWatchers::stop`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WakeRoute {
    Drive,
    ItsOwnReader,
    Gone,
}

fn wake_route(held_by_the_daemon: bool, live_owner: Option<Owner>) -> WakeRoute {
    match (held_by_the_daemon, live_owner) {
        (true, _) => WakeRoute::Drive,
        (false, Some(Owner::OwnThread)) => WakeRoute::ItsOwnReader,
        // **A hub the WORKER owns and does not hold is not somebody else's reader** (R68.2, .9):
        // it is a session whose open failed (or has not run), and handing a wake to its own
        // condvar was handing it into a void — nothing waits there — while the outcome said
        // `HandedOn`. It is gone as far as a wake is concerned; a prompt reopens it lazily.
        (false, Some(Owner::Worker)) | (false, None) => WakeRoute::Gone,
    }
}

/// **What a shutdown's wait for the subagent threads came to** — the two outcomes
/// [`Sessions::drain_subagents`] distinguishes, so a caller's log can say which it was rather
/// than only speaking up when something went wrong.
#[derive(Debug)]
pub enum SubagentExit {
    /// **Every thread left inside the bound.** `told` is how many sessions had a child that was
    /// TOLD to stop, which is the count of sessions that actually had something running — `0` is
    /// a daemon that had nothing to wait for.
    Waited { told: usize },
    /// **The bound was spent and these sessions still had a thread.** See
    /// [`SubagentExit::sentence`].
    GaveUp {
        sessions: Vec<String>,
        waited: std::time::Duration,
    },
}

impl SubagentExit {
    /// **The one line a shutdown prints, or `None` when there is nothing to say.**
    ///
    /// Silence is the ordinary case — a daemon with nothing running, or whose children had
    /// already left, has no fact here worth a line — while a quit that HAD to wait, and one that
    /// could not, are exactly the two things a reader of the log needs to be able to tell apart.
    /// Which sessions could not be waited for is named rather than counted: the session that is
    /// still computing is the one somebody can go and look at, and a count would be the
    /// claim-versus-fact defect this tree keeps paying for.
    pub fn sentence(&self) -> Option<String> {
        match self {
            SubagentExit::Waited { told: 0 } => None,
            SubagentExit::Waited { told } => Some(format!(
                "waited for the subagent thread(s) of {told} session(s), and every one left \
                 before the exit"
            )),
            SubagentExit::GaveUp { sessions, waited } => Some(format!(
                "waited {waited:?} for the subagent thread(s) of {} and gave up; exiting anyway. \
                 A thread still inside libllama when `exit()` runs the ELF finalizers can die of \
                 SIGSEGV after this line — the crash this wait exists to prevent — so this \
                 sentence is the record that the exit was not clean. The session(s) named are the \
                 ones whose child could not be waited for.",
                sessions.join(", ")
            )),
        }
    }
}

/// **One session's children, told to stop and waited for** — [`Sessions::drain_subagents`]' body
/// for one harness, here so the daemon's loop and a session's own thread run the same two lines.
///
/// The answer is `(told, left)`: whether anything was told to stop, and whether anything was still
/// running when the patience ran out.
fn stop_children_of(harness: &Harness, patience: std::time::Duration) -> (bool, bool) {
    let told = harness.stop_children().is_some();
    let left = !harness.wait_for_children(patience);
    (told, left)
}

/// **The loop of a session served on its own thread** — the shape a subagent already has
/// (`harness::serve_child`), for a session the daemon holds.
///
/// Three things and no more: the daemon's requests (its own channel, because a door like
/// `dispatch` or `with_harness` is not something a head put on the command queue), the session's
/// own queue, and the clocks this thread alone may keep — because keeping the clocks anywhere else
/// is what forced every session onto one worker.
fn court_main(
    mut s: Sessions<'static>,
    hub: Arc<letibot_sessionlog::hub::Hub>,
    id: String,
    jobs: std::sync::mpsc::Receiver<Job>,
    status: Arc<Mutex<Option<String>>>,
) {
    use std::sync::mpsc::TryRecvError;
    let closed = loop {
        // **The daemon's requests first.** Each one has an asker behind it and a `Finish` is the
        // daemon leaving, which ends this loop rather than waiting for a queue to close first.
        let mut finish = false;
        loop {
            match jobs.try_recv() {
                Ok(job) => finish |= job(&mut s) == Follow::Finish,
                Err(TryRecvError::Empty) => break,
                // The daemon's `Sessions` is gone: nothing can ask this session anything again.
                Err(TryRecvError::Disconnected) => {
                    finish = true;
                    break;
                }
            }
        }
        if finish {
            break false;
        }
        // Published rather than answered on demand: the idle exit asks every second, and a session
        // mid-turn cannot answer at all. See `Sessions::court_refusal`.
        *status.lock().unwrap_or_else(|e| e.into_inner()) = s.court_refusal(&id);
        match hub.take_own_work_until(s.next_idle_at()) {
            letibot_sessionlog::hub::OwnWork::Closed => break true,
            letibot_sessionlog::hub::OwnWork::Command(cmd) => {
                let at = crate::instruments::Clock::now();
                let outcome = s.dispatch(&id, &cmd);
                crate::instruments::worker_cmd(&id, &cmd, at, &outcome);
                s.say(&id, OutcomeSaid::Command(&cmd, &outcome));
            }
            // **A wake is this session's own settlement being ready** — the same door
            // `harness::serve_child` answers, and the reason a ring naming a root still means
            // something now that the daemon's worker cannot run its turn.
            letibot_sessionlog::hub::OwnWork::Wake => {
                let outcome = s.wake(&id);
                s.say(&id, OutcomeSaid::Wake(&outcome));
            }
            // Nothing was asked of this session: this is where its own clocks are read, on the one
            // thread that may drive the harness. `daemon.rs`'s idle pass, one session wide.
            letibot_sessionlog::hub::OwnWork::TimedOut => {
                let pass = Instant::now();
                let swept = s.sweep_abandoned_calls();
                crate::instruments::worker_pass("sweep", pass, swept);
                let pass = Instant::now();
                let ran = s.deliver_due_nags();
                crate::instruments::worker_pass("todo-check", pass, ran);
                let pass = Instant::now();
                let parked = s.park_idle_children();
                crate::instruments::worker_pass("park", pass, parked);
                s.plugin_tick();
            }
        }
    };
    // **Said once, whether this thread served or was closed out**: the status is the daemon's
    // answer to *what is holding you up*, and a thread that has left holds nothing.
    *status.lock().unwrap_or_else(|e| e.into_inner()) = None;
    if closed {
        // **A session CLOSED out from under it is over, and a DAEMON going away is not.** The
        // difference is the registry: a close takes the entry away (`Registry::remove`), while a
        // stop closes every hub and leaves the tree standing. Only the second has children still
        // running whose stop is `Sessions::drain_subagents`'s to deliver, so only the second waits.
        if s.registry.get(&id).is_none() {
            if let Some(h) = s.open.get(&id) {
                h.stop_children();
                h.close_backend();
            }
            return;
        }
        // **The hub closed under it** — the daemon is stopping. The harness is NOT dropped here:
        // its children may still be running, and telling them to stop is
        // `Sessions::drain_subagents`'s job, which arrives on this thread as its last job. A daemon
        // that never asks (a test that ends) drops the sender and this channel, which is the same
        // ending one step earlier.
        while let Ok(job) = jobs.recv() {
            if job(&mut s) == Follow::Finish {
                break;
            }
        }
    }
}

/// **Wait for the given threads to leave, bounded and shared** — one deadline for all of them, so
/// a second session cannot double the wait.
///
/// A poll rather than `join`, because the caller holds a `&self` or is inside `Drop` and
/// `JoinHandle::join` takes the handle by value; the tree already reads its shutdowns this way
/// ([`Sessions::drain_subagents`] does the same thing for the same reason).
fn wait_for_threads(threads: &[std::thread::JoinHandle<()>], patience: Duration) -> usize {
    let began = Instant::now();
    let mut left = 0;
    for thread in threads {
        while !thread.is_finished() && began.elapsed() < patience {
            std::thread::sleep(Duration::from_millis(2));
        }
        if !thread.is_finished() {
            left += 1;
        }
    }
    left
}

impl Drop for Sessions<'_> {
    /// **No session's thread outlives the `Sessions` that armed it.**
    ///
    /// A court owns its session's harness and its own clone of `Parts`, so a thread still inside
    /// either when the process exits is a SIGSEGV in `_dl_fini` — measured, see [`COURT_TEARDOWN`].
    /// Dropping the channel is what wakes a parked court (`recv` answers `Disconnected`); the wait
    /// is for the teardown that follows it.
    ///
    /// **Only when the registry is closed**, because that is when this really is the end: a
    /// `Sessions` dropped while its daemon is still serving is a caller giving up its handle, and a
    /// session whose hub is still open and whose queue may still be written has to keep its reader.
    /// In that case the threads are detached, exactly as they were before this drop existed.
    fn drop(&mut self) {
        if self.courts.is_empty() {
            return;
        }
        let mut threads = Vec::new();
        for (_, court) in std::mem::take(&mut self.courts) {
            // The sender goes FIRST: a court parked on its channel leaves on the disconnect, and a
            // drop that waited before dropping them would wait for a thread nothing had woken.
            let Court { jobs, thread, .. } = court;
            drop(jobs);
            if let Some(thread) = thread {
                threads.push(thread);
            }
        }
        if !self.registry.is_closed() {
            return;
        }
        let left = wait_for_threads(&threads, COURT_TEARDOWN);
        if left > 0 {
            eprintln!(
                "  {left} of {} session thread(s) did not leave inside {COURT_TEARDOWN:?}; exiting \
                 anyway — a thread still inside libllama when the process runs its ELF finalizers \
                 can die of SIGSEGV after this line",
                threads.len()
            );
        }
    }
}

impl<'a> Sessions<'a> {
    /// Start with one session, opened eagerly. See the module header for why the
    /// first one is not lazy.
    pub fn open_first(
        parts: &'a Parts,
        cfg: Config,
        registry: Arc<Registry>,
    ) -> Result<Sessions<'a>, HarnessError> {
        Self::open_first_with_seat(parts, cfg, registry, None)
    }

    /// [`Sessions::open_first`] with the daemon's flowy seat, which the first
    /// session attaches to like every root session after it.
    pub fn open_first_with_seat(
        parts: &'a Parts,
        cfg: Config,
        registry: Arc<Registry>,
        seat: Option<letibot_flowy::Seat>,
    ) -> Result<Sessions<'a>, HarnessError> {
        let id = cfg.session_id.clone();
        let hub = registry
            .get(&id)
            .ok_or_else(|| HarnessError::Setup(format!("session {id} is not in the registry")))?;
        let mut sessions = Sessions {
            parts: std::borrow::Cow::Borrowed(parts),
            base: cfg.clone(),
            gatekeeper: gatekeeper::session(&cfg.workspace),
            registry: registry.clone(),
            open: HashMap::new(),
            armed: std::collections::HashSet::new(),
            seat,
            slots: HashMap::new(),
            seated: HashMap::new(),
            fabric_seen: HashMap::new(),
            nags: HashMap::new(),
            nag_window: TODO_NAG_AFTER,
            sweep_at: None,
            park_at: None,
            // Due at once: the first idle pass looks. A daemon that started between a review's
            // ask and its wake has the row and nothing else, and this is what finds it.
            reviews_at: Some(Instant::now()),
            idle_exit_at: cfg.idle_exit_after.map(|w| Instant::now() + w),
            busy_seen: Instant::now(),
            bangs: HashMap::new(),
            gatekeeper_said: None,
            plugins: HashMap::new(),
            courts: HashMap::new(),
            threaded: false,
            reply: None,
        };
        let (tool, cond) = sessions.seat_tool(&id);
        // The system prompt is composed at open, for the tools the seat actually holds,
        // from `prompts.toml`, the standing notes and the fabric. Message 0 is written
        // from it and never rewritten after — see `Config::compose_for_seat`.
        let mut cfg = cfg;
        cfg.seat_prompt = Some(crate::config::SeatPrompt::default());
        let cfg = sessions.with_fabric(cfg, cond.is_some());
        let mut harness =
            Harness::open_with_registry(parts, cfg, hub, None, tool, registry.clone())?;
        // **And who this daemon's own first session is for**, written now that its row exists
        // (`Harness::open` writes the row and this is the first moment there is one to write
        // on). Two answers and no others: a plain start is the OPERATOR's conversation —
        // whether it is new or a `--continue` — and `--prompt`/`--slash` is a one-shot's
        // (`cfg.one_shot`, set from the steps themselves). The row is the only place a picker
        // tomorrow can read it. See `letibot_sessionlog::SessionOrigin`.
        registry.record_origin(
            &id,
            if sessions.base.one_shot {
                SessionOrigin::OneShot
            } else {
                registry.origin_of(&id)
            },
        );
        sessions.fabric_opened(&id, &harness);
        // **A `--continue` resumes the session on the model it was switched to.** The
        // first session is the common resume: without this call the row this change
        // writes would only be read on the lazy path a head's switch reaches, and the
        // one resume everybody types would go on ignoring it.
        harness.restore_provider_choice();
        // **And on the compaction the no-progress guard stood down**, for the same
        // reason — a resumed session coming back with the pre-emptive door re-armed
        // is the guard's finding silently unrecorded, and the loop it existed to
        // stop comes back with it. The pair the guard wrote names what it saw.
        if let Some(said) = harness.restore_auto_compact() {
            eprintln!("  {id}: {said}");
        }
        // The first session's operator-run state, same rule as the lazy path below:
        // one `Arc` in the map, its clone in the harness.
        let bang = sessions
            .bangs
            .entry(id.clone())
            .or_insert_with(crate::bangrun::State::new)
            .clone();
        harness.set_bang_state(bang);
        // **And what the last daemon left running comes back with it, or is reminded** — the
        // operator: *"we should bring subagents back - i cant afford to wait till they finish"*.
        // Before the ring below, so the wake it causes carries the `[restart]` notice. See
        // `crate::restart`.
        harness.restore_after_restart();
        sessions.open.insert(id.clone(), harness);
        sessions.declare_flowy_monitor(&id, cond);
        // **AND THE DAEMON'S OWN FIRST SESSION ARMS ITS CLOCK TOO.** This constructor inserts into
        // `open` directly and never goes through `Sessions::open`, so the arming added there does not
        // reach it — and this is the session a plain `letibot --continue` resumes, which makes it the
        // COMMON case rather than a corner. Same rule as there: the board was just rebuilt from the
        // store, so an unfinished plan is the model's own outstanding work and is worth one check.
        sessions.rearm_todo_nag(&id);
        // **And the same rule one line down, because THIS is the session a restart restores.** A
        // `--continue` comes back to a board, a queue and a set of job handles that the process
        // before it left behind; the clock above speaks about the PLAN a minute later, while what
        // is due NOW — a row whose job is gone, an entry the queue is holding — has no clock of its
        // own. The ring is drained by the worker's first pass. See
        // [`Sessions::put_a_restored_session_to_work`].
        sessions.put_a_restored_session_to_work(&id);
        // **And the plugin layers this session has**, found last so nothing above it waits for a
        // wasm compile: a session with no layers costs three `read_dir`s on directories that do
        // not exist.
        sessions.open_plugins(&id);
        Ok(sessions)
    }

    /// **Serve every session on a thread of its own from now on**, and say a turn's outcome where
    /// `reply` sends it.
    ///
    /// One caller: `Daemon::run`, which IS the single worker this replaces. A test that drives
    /// `Sessions` by hand (`drive_worker` and its kin) leaves it off and drives exactly the path it
    /// has always driven — one thread, one command at a time — which is what keeps those suites
    /// about the thing they were written about rather than about this.
    pub fn serve_each_on_its_own_thread(&mut self, reply: ReplySink) {
        self.threaded = true;
        self.reply = Some(reply);
        // The first session was opened by `open_first`, before there was a thread to hand it to;
        // its creation rang a `Work::Open` that the loop has not drained yet, and that is where
        // this is reached for it too. This catches the case where the ring was spent already.
        let ids: Vec<String> = self.open.keys().cloned().collect();
        for id in ids {
            self.spawn_court(&id);
        }
    }

    /// **Whether each session is served on a thread of its own.** See [`Court`].
    pub fn each_on_its_own_thread(&self) -> bool {
        self.threaded
    }

    /// **Say a turn's outcome where the daemon's log reads it** — once, from whichever thread ran
    /// the turn. Silence under a test, which supplies no sink.
    fn say(&self, session_id: &str, said: OutcomeSaid<'_>) {
        if let Some(reply) = &self.reply {
            reply(session_id, said);
        }
    }

    /// **What this session is holding the daemon up for** — the per-harness half of
    /// [`Sessions::idle_exit_refusal`], asked where the harness is.
    ///
    /// It exists as its own function so the two readers answer with one body: the session's own
    /// thread computes it at every boundary and publishes it ([`Court::status`]), and the daemon
    /// reads the published answer rather than waiting for a turn that may be minutes long.
    fn court_refusal(&self, session_id: &str) -> Option<String> {
        let h = self.open.get(session_id)?;
        if let Some(why) = h.park_refusal() {
            return Some(format!("{session_id}: {why}"));
        }
        if h.monitors().is_some_and(|m| !m.list().is_empty()) {
            return Some(format!("{session_id} has a monitor watching"));
        }
        if self.nags.get(session_id).and_then(|c| c.due_at()).is_some() {
            return Some(format!("{session_id} has a plan-check armed"));
        }
        if self.bang_run_in_flight(session_id) {
            return Some(format!("{session_id} has a `!` run in flight"));
        }
        None
    }

    /// **Give this session a thread of its own, if it has one to serve and none yet.**
    ///
    /// Idempotent, because three doors reach it: the daemon's `Work::Open` (a session a head made
    /// or resumed), the first pass after `serve_each_on_its_own_thread`, and any door that finds an
    /// open session with no court (`dispatch`, `wake`) — which is what a session opened lazily by
    /// something other than `Sessions::open` looks like.
    fn spawn_court(&mut self, session_id: &str) -> bool {
        if !self.threaded
            || self.courts.contains_key(session_id)
            || !self.open.contains_key(session_id)
        {
            return false;
        }
        let Some(hub) = self.registry.get(session_id) else {
            return false;
        };
        let (jobs, job_rx) = std::sync::mpsc::channel();
        // **The session is handed over on its own channel, not captured**, so a thread that could
        // not be started costs nothing: nothing has been moved out of this struct until the thread
        // is there to take it. See the `Err` arm below.
        let (ready, ready_rx) = std::sync::mpsc::channel::<Sessions<'static>>();
        let status = Arc::new(Mutex::new(None));
        let for_thread = status.clone();
        let thread_id = session_id.to_string();
        let thread_hub = hub.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("session-{session_id}"))
            .spawn(move || {
                // A daemon that never hands the session over is a daemon that left: nothing to do.
                if let Ok(court) = ready_rx.recv() {
                    court_main(court, thread_hub, thread_id, job_rx, for_thread);
                }
            });
        let thread = match spawned {
            Ok(t) => t,
            Err(e) => {
                // **Said, and the session keeps the only reader it had**: its turns run on the
                // daemon's worker, as they did before this thread existed. Worse, and never silent.
                eprintln!(
                    "  {session_id} · no thread of its own ({e}) — its turns run on the daemon's \
                     one worker, behind every other session's"
                );
                return false;
            }
        };
        let Some(court) = self.court_of(session_id) else {
            return false;
        };
        // **Marked before the session is handed over**, so there is no instant in which the worker
        // could take this session's command and run it as well as the thread — the second writer
        // [`Owner`] exists to make unspellable. A command that lands between this line and the
        // thread's first pass waits in the hub's queue, and the thread takes it.
        hub.owned_by_its_own_thread();
        if ready.send(court).is_err() {
            return false;
        }
        self.courts.insert(
            session_id.to_string(),
            Court {
                jobs,
                thread: Some(thread),
                status,
                hub,
            },
        );
        true
    }

    /// **This session's own `Sessions`, for the thread that will serve it.**
    ///
    /// Everything per-session MOVES: the harness, its clocks, the seat slot, the plugin host, the
    /// `!` state. Two instances holding one session would be two answers to *who runs this turn* —
    /// the defect [`Owner`] exists to make unspellable, one level up. What stays behind is the
    /// daemon's own: its review clock, its idle exit, the queue.
    fn court_of(&mut self, session_id: &str) -> Option<Sessions<'static>> {
        let id = session_id.to_string();
        let harness = self.open.remove(&id)?;
        let mut court = Sessions {
            // **OWNED, not borrowed**: this thread outlives the `&Parts` the daemon was handed.
            // A clone is three `Arc`s — the vocabulary and the dialect renderer are not copied.
            parts: std::borrow::Cow::Owned(Parts::clone(&self.parts)),
            base: self.base.clone(),
            gatekeeper: self.gatekeeper.clone(),
            registry: self.registry.clone(),
            open: HashMap::new(),
            armed: std::collections::HashSet::new(),
            seat: self.seat.clone(),
            slots: HashMap::new(),
            seated: HashMap::new(),
            fabric_seen: HashMap::new(),
            nags: HashMap::new(),
            nag_window: self.nag_window,
            sweep_at: None,
            park_at: None,
            // **The daemon's own clocks are the daemon's.** A court with a review clock or an
            // idle exit would be a second daemon deciding to leave — and the review pass reads
            // the queue, which is the daemon's.
            reviews_at: None,
            idle_exit_at: None,
            busy_seen: Instant::now(),
            bangs: HashMap::new(),
            gatekeeper_said: None,
            plugins: HashMap::new(),
            courts: HashMap::new(),
            threaded: false,
            reply: self.reply.clone(),
        };
        court.open.insert(id.clone(), harness);
        if let Some(slot) = self.slots.remove(&id) {
            court.slots.insert(id.clone(), slot);
        }
        if let Some(cond) = self.seated.remove(&id) {
            court.seated.insert(id.clone(), cond);
        }
        if let Some(seen) = self.fabric_seen.remove(&id) {
            court.fabric_seen.insert(id.clone(), seen);
        }
        if let Some(nag) = self.nags.remove(&id) {
            court.nags.insert(id.clone(), nag);
        }
        if let Some(bang) = self.bangs.remove(&id) {
            court.bangs.insert(id.clone(), bang);
        }
        if let Some(plugin) = self.plugins.remove(&id) {
            court.plugins.insert(id.clone(), plugin);
        }
        if self.armed.remove(&id) {
            court.armed.insert(id);
        }
        Some(court)
    }

    /// **One session's harness, asked rather than borrowed.**
    ///
    /// `harness_of` hands out a reference, which a session served on its own thread cannot: the
    /// harness is on that thread. So a caller that only needs to read something says what it wants
    /// with a closure and gets the same answer either way — run here when this struct holds the
    /// session, asked of its thread when the thread does.
    pub fn with_harness<R: Send + 'static>(
        &self,
        session_id: &str,
        f: impl FnOnce(&Harness) -> R + Send + 'static,
    ) -> Option<R> {
        if let Some(court) = self.courts.get(session_id) {
            let id = session_id.to_string();
            return court.ask(move |s| s.open.get(&id).map(f))?;
        }
        self.open.get(session_id).map(f)
    }

    /// The seat this daemon holds, if any.
    pub fn seat(&self) -> Option<&letibot_flowy::Seat> {
        self.seat.as_ref()
    }

    /// The `flowy` tool for a root session — always, as a door — and, when the
    /// daemon holds a seat, the session's condition on it. A subagent gets
    /// neither: it routes through its parent.
    fn seat_tool(
        &mut self,
        session_id: &str,
    ) -> (
        Option<Box<dyn letibot_tools::runtime::Tool>>,
        Option<Arc<letibot_flowy::InboxCondition>>,
    ) {
        if !is_root(&self.registry, session_id) {
            return (None, None);
        }
        let (tool, slot) = letibot_flowy::Flowy::unattached();
        self.slots.insert(session_id.to_string(), slot.clone());
        let cond = self
            .seat
            .clone()
            .map(|seat| self.attach_session(&seat, session_id, &slot));
        (Some(Box::new(tool)), cond)
    }

    /// Attach one root session to the seat: its condition, its slot filled.
    fn attach_session(
        &mut self,
        seat: &letibot_flowy::Seat,
        session_id: &str,
        slot: &letibot_flowy::tool::SeatSlot,
    ) -> Arc<letibot_flowy::InboxCondition> {
        // The title is the session's short address on the fabric — `@seat/title`
        // beside `@seat/id` — so agents on one project can name each other.
        // From the entry (R68.6): the name it was created or resumed with, or the last one a
        // rename or a derived title put there — not a scan of every stored session.
        let title = self.registry.title_of(session_id).unwrap_or_default();
        let cond = seat.attach_as(session_id, &title, letibot_flowy::Attention::default());
        *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(letibot_flowy::tool::Attachment {
            seat: seat.clone(),
            cond: cond.clone(),
        });
        self.seated.insert(session_id.to_string(), cond.clone());
        cond
    }

    /// One slash verb from a head, against this session.
    pub fn slash(&mut self, session_id: &str, line: &str) -> crate::slash::SlashReply {
        // **A verb a session's own thread must answer is asked of it** — the harness it acts on is
        // there, and so is the clock `/nag` moves. `cli.rs`'s `--slash` steps are the caller here.
        if let Some(court) = self.court_serving(session_id) {
            let id = session_id.to_string();
            let line = line.to_string();
            if let Some(reply) = court.ask(move |s: &mut Sessions<'static>| s.slash(&id, &line)) {
                return reply;
            }
        }
        self.slash_parsed(session_id, crate::slash::Slash::parse(line))
    }

    fn slash_parsed(
        &mut self,
        session_id: &str,
        parsed: crate::slash::Slash,
    ) -> crate::slash::SlashReply {
        use crate::slash::{Slash, SlashReply};
        match parsed {
            Slash::Help(h) => SlashReply {
                lines: vec![h],
                ok: false,
            },
            Slash::Gate(verb) => crate::slash::gate(self.base.store.as_deref(), &verb),
            // **A fresh conversation for this session** — the operator's *"clear context - fresh
            // reseat without loosing session todos and jobs"*. Through the session's own harness,
            // opened if it is not, because everything that carries on (the board, the jobs, the
            // children) lives there; see `harness/clear.rs` for why none of it has to be copied.
            // A child's own `/clear` never reaches here: its serving thread owns its harness and
            // answers it there (`serve_child`).
            Slash::Clear { next } => {
                let h = match self.harness(session_id) {
                    Ok(h) => h,
                    Err(e) => {
                        return SlashReply {
                            lines: vec![e.to_string()],
                            ok: false,
                        };
                    }
                };
                match h.clear_conversation(next.as_deref()) {
                    Ok(r) => {
                        h.publish_cleared(&r);
                        SlashReply {
                            lines: vec![crate::harness::clear_reply(&r)],
                            ok: true,
                        }
                    }
                    Err(e) => SlashReply {
                        lines: vec![e.to_string()],
                        ok: false,
                    },
                }
            }
            // **The merge queue's person-verbs.** They go through the session's own harness
            // because that is the half that can announce the move on every head's log — an entry
            // moving is the pane's business, and the pane may be open in a session that is not the
            // one the entry came from — and, for a veto, because it is the half that owns the
            // `task_message` door the child is reached through.
            //
            // A refusal is a `SlashReply` with `ok: false`, which the worker publishes as
            // `slash_refused` — the same door every other verb's "no" comes through, so a verb
            // that could not happen says so where the operator is already looking.
            parsed @ (Slash::Queue(_) | Slash::QueueIn { .. }) => {
                use crate::slash::{QueueScope, QueueVerb};
                let (scope, verb) = match parsed {
                    Slash::QueueIn { scope, verb } => (scope, verb),
                    Slash::Queue(verb) => (QueueScope::Here, verb),
                    _ => unreachable!("the arm's own pattern"),
                };
                let Some(h) = self.open.get_mut(session_id) else {
                    return SlashReply {
                        lines: vec![format!("session {session_id} is not open")],
                        ok: false,
                    };
                };
                // **Scoped to this session's repository unless the line names another** (R92
                // item 5). `regate` resolves its own repository (it was already the session's
                // by default); a named scope stands in for its `/REPO`.
                // **A session of a one-org project folder acts in each of its repositories**: a
                // row verb in the one holding the row, a sweep once per repository.
                let scopes = match (&verb, &scope) {
                    (QueueVerb::Regate { .. }, _) => vec![None],
                    _ => match h.queue_scopes(&scope) {
                        Ok(r) => r,
                        Err(why) => {
                            return SlashReply {
                                lines: vec![why],
                                ok: false,
                            };
                        }
                    },
                };
                let entry_id = match &verb {
                    QueueVerb::Restart { entry }
                    | QueueVerb::Approve { entry }
                    | QueueVerb::Veto { entry, .. }
                    | QueueVerb::Rm { entry } => Some(entry.clone()),
                    QueueVerb::Reset { entry } | QueueVerb::Clean { entry } => entry.clone(),
                    QueueVerb::Regate { .. } => None,
                };
                // A sweep of the whole queue across several repositories: once per repository.
                if entry_id.is_none() && scopes.len() > 1 {
                    let mut lines = Vec::new();
                    let mut ok = true;
                    for s in &scopes {
                        let said = match &verb {
                            QueueVerb::Reset { .. } => h.reset_reviews(None, s.as_deref()),
                            QueueVerb::Clean { .. } => h.clean_entries(None, s.as_deref()),
                            _ => continue,
                        };
                        match said {
                            Ok(l) => lines.push(l),
                            Err(e) => {
                                ok = false;
                                lines.push(e);
                            }
                        }
                    }
                    return SlashReply { lines, ok };
                }
                let repo = match &entry_id {
                    Some(id) => h.queue_scope_of_row(&scopes, id),
                    None => scopes.first().cloned().flatten(),
                };
                let repo = repo.as_deref();
                // **`session_id` is passed to `approve` and not to the others**, because it is the
                // one act that writes a decider onto a row: the review's `session_id` becomes the
                // session the person decided in, and the pane draws that field as *attach to it to
                // read the argument*.
                let said = match verb {
                    QueueVerb::Restart { entry } => h.restart_review(&entry, repo),
                    QueueVerb::Approve { entry } => h.approve_review(&entry, session_id, repo),
                    QueueVerb::Veto { entry, why } => h.veto_review(&entry, &why, repo),
                    QueueVerb::Rm { entry } => h.remove_merge_entry(&entry, repo),
                    // **The two sweeps take an id or none**, and none is the whole queue — the
                    // operator's ruling is about the queue as much as about a row, so the
                    // `Option` travels as it was parsed rather than being refused here.
                    QueueVerb::Reset { entry } => h.reset_reviews(entry.as_deref(), repo),
                    QueueVerb::Clean { entry } => h.clean_entries(entry.as_deref(), repo),
                    QueueVerb::Regate { repository, target } => match (&scope, repository) {
                        (QueueScope::All, None) => Err(
                            "`/queue all regate` names no one repository to gate; `/queue \
                             regate /REPO [TARGET]` does"
                                .to_string(),
                        ),
                        (QueueScope::Repo(p), None) => h.regate_target(
                            Some(p),
                            target.as_deref(),
                            &format!("operator:{session_id}"),
                        ),
                        (_, repository) => h.regate_target(
                            repository.as_deref(),
                            target.as_deref(),
                            &format!("operator:{session_id}"),
                        ),
                    },
                };
                match said {
                    Ok(line) => SlashReply {
                        lines: vec![line],
                        ok: true,
                    },
                    Err(why) => SlashReply {
                        lines: vec![why],
                        ok: false,
                    },
                }
            }
            // **The wall controller, by hand** — see [`Slash::AutoCompact`].
            Slash::AutoCompact { on } => {
                let Some(h) = self.open.get_mut(session_id) else {
                    return SlashReply {
                        lines: vec![format!("session {session_id} is not open")],
                        ok: false,
                    };
                };
                let line = if on {
                    h.rearm_auto_compact()
                } else {
                    format!(
                        "automatic compaction {}; wall: {}",
                        if h.config().auto_compact { "on" } else { "off" },
                        h.wall().row(&h.token_unit())
                    )
                };
                SlashReply {
                    lines: vec![line],
                    ok: true,
                }
            }
            Slash::Supervise { want, at } => {
                let Some(h) = self.open.get_mut(session_id) else {
                    return SlashReply {
                        lines: vec![format!("session {session_id} is not open")],
                        ok: false,
                    };
                };
                let mut lines = Vec::new();
                if let Some(addr) = at {
                    let ep = match letibot_turn::Endpoint::parse(&addr) {
                        Ok(e) => e,
                        Err(why) => {
                            return SlashReply {
                                lines: vec![why],
                                ok: false,
                            };
                        }
                    };
                    match h.attach_oracle(ep) {
                        Ok(line) => lines.push(line),
                        Err(why) => {
                            return SlashReply {
                                lines: vec![why],
                                ok: false,
                            };
                        }
                    }
                }
                match want {
                    // A report, not a change. `/supervise status` after a long turn
                    // is the cheapest way to answer "is this being measured", and it
                    // must not be the same keystroke as turning it on.
                    None => SlashReply {
                        lines: vec![if h.supervising() {
                            "supervised: the guard model answers every gated call \
                             before you do, and both verdicts land on the corpus row"
                                .into()
                        } else {
                            "not supervised: calls ask whoever the mode says, and no \
                             model verdict is recorded. `/supervise` turns it on."
                                .into()
                        }],
                        ok: true,
                    },
                    Some(on) => match h.set_supervision(on) {
                        Ok(line) => {
                            lines.push(line);
                            SlashReply { lines, ok: true }
                        }
                        Err(why) => {
                            lines.push(why);
                            SlashReply { lines, ok: false }
                        }
                    },
                }
            }
            // **The pause is the harness's (its config, its row, its hub); the clock is ours**, so
            // the verb lands on the one and the re-arm follows on the other — a pause the clock
            // has not been re-armed with would leave the deadline it had, and that deadline is a
            // check the operator just asked not to get.
            Slash::Nag(verb) => {
                let Some(h) = self.open.get_mut(session_id) else {
                    return SlashReply {
                        lines: vec![format!("session {session_id} is not open")],
                        ok: false,
                    };
                };
                let reply = h.nag_verb(verb);
                self.rearm_todo_nag(session_id);
                reply
            }
            Slash::FlowyStatus => crate::slash::flowy_status(self.seat.as_ref()),
            Slash::FlowyLogout => match self.detach_seat() {
                Some(name) => SlashReply {
                    lines: vec![format!(
                        "released seat `{name}`; the room is no longer heard"
                    )],
                    ok: true,
                },
                None => SlashReply {
                    lines: vec!["no seat was attached".into()],
                    ok: false,
                },
            },
            Slash::FlowyLogin {
                seat,
                addr,
                token,
                token_file,
                new_reader,
            } => {
                let (creds, mut lines) = match crate::slash::flowy_login_credentials(
                    seat.as_deref(),
                    addr.as_deref(),
                    token.as_deref(),
                    token_file.as_ref(),
                ) {
                    Ok(x) => x,
                    Err(lines) => return SlashReply { lines, ok: false },
                };
                lines.push(format!("credentials: {}", creds.describe()));
                let seat = match letibot_flowy::Seat::open(creds, None, None) {
                    Ok(s) => s,
                    Err(e) => {
                        lines.push(format!("{e}"));
                        return SlashReply { lines, ok: false };
                    }
                };
                if new_reader {
                    match seat.declare_reader() {
                        Ok(r) => lines.push(format!(
                            "declared reader `{}` at cursor {}",
                            r.reader, r.cursor
                        )),
                        Err(e) => {
                            lines.push(format!("declaring the reader: {e}"));
                            return SlashReply { lines, ok: false };
                        }
                    }
                } else {
                    match seat.reader() {
                        Ok(Some(r)) => {
                            lines.push(format!("reader `{}` at cursor {}", r.reader, r.cursor))
                        }
                        Ok(None) => {
                            lines.push(format!(
                                "reader `{}` is NOT DECLARED on the node. If this seat has never listened, \
                                 `/flowy login {} --new-reader`. If it has, its token was SWITCHED and the old \
                                 identity still holds every message since — read that first.",
                                seat.name(),
                                seat.name()
                            ));
                            return SlashReply { lines, ok: false };
                        }
                        Err(e) => lines.push(format!(
                            "node not answering yet ({e}); the listener will keep trying"
                        )),
                    }
                }
                let _ = session_id;
                lines.extend(self.attach_seat(seat));
                lines.push(
                    "attached. `/flowy status` for the seat; the `flowy` tool speaks as it.".into(),
                );
                SlashReply { lines, ok: true }
            }
            // The standing choice, and only that: no session changes here. The
            // operator asked for it as its own verb after `/models` doing both at
            // once cost them two questions.
            Slash::DefaultModel(want) => crate::slash::default_model(want.as_deref(), None),
            Slash::Tools => match self.open.get(session_id) {
                Some(h) => SlashReply {
                    lines: h.tools_lines(),
                    ok: true,
                },
                None => SlashReply {
                    lines: vec![format!("session {session_id} is not open")],
                    ok: false,
                },
            },
            Slash::MonitorRetire(name) => match self.open.get(session_id) {
                Some(h) => h.retire_monitor_for_operator(&name),
                None => SlashReply {
                    lines: vec![format!("session {session_id} is not open")],
                    ok: false,
                },
            },
            Slash::Job { job, offset } => {
                let Some(h) = self.open.get(session_id) else {
                    return SlashReply {
                        lines: vec![format!("session {session_id} is not open")],
                        ok: false,
                    };
                };
                match job {
                    None => SlashReply {
                        lines: h.job_lines(),
                        ok: true,
                    },
                    // One page: a build log's tail is one read, and the reply names
                    // the next offset when it is not. See [`JOB_OUTPUT_WINDOW`].
                    Some(j) => match h.job_output(&j, offset, crate::harness::JOB_OUTPUT_WINDOW) {
                        Ok(lines) => SlashReply { lines, ok: true },
                        Err(e) => SlashReply {
                            lines: vec![e],
                            ok: false,
                        },
                    },
                }
            }
            Slash::Models => {
                let current = self
                    .open
                    .get(session_id)
                    .map(|h| h.provider_line())
                    .unwrap_or_else(|| "(session not open)".into());
                SlashReply {
                    lines: crate::slash::models_listing(&current),
                    ok: true,
                }
            }
            // **Switches this session and nothing else.** The standing choice is
            // `/default-model`'s; see `Slash::ModelsSet` for the three goes it
            // took to separate them.
            Slash::ModelsSet {
                provider,
                model,
                key,
            } => {
                let (choice, mut lines) = match crate::slash::models_choice(
                    &provider,
                    model.as_deref(),
                    key.as_deref(),
                    None,
                ) {
                    Ok(x) => x,
                    Err(lines) => return SlashReply { lines, ok: false },
                };
                let Some(h) = self.open.get_mut(session_id) else {
                    lines.push(format!("session {session_id} is not open"));
                    return SlashReply { lines, ok: false };
                };
                // **The act itself is the harness's** — `Harness::apply_model_choice`,
                // which is now the ONE implementation of this verb. It has a second
                // caller as of 2026-10-12: the retry loop, which takes a `/models` typed
                // during a turn out of the queue and answers it on the spot rather than after it
                // — see `Harness::apply_queued_model`. Since 2026-10-26 that answer is
                // *"turns go to X when this turn ends"*: the switch is recorded there and
                // applied at the turn's end, because a model's first request has to follow a
                // user message. Two copies of a switch would be two
                // places for the screen, the session row and the thing that actually
                // answers to disagree.
                h.apply_model_choice(choice, lines)
            }
        }
    }

    /// **A seat arrives while sessions are open** — `/flowy login` from a head.
    /// Every open root session is attached: its slot filled, its `flowy` monitor
    /// declared, its wake armed, the shelf installed, the fabric block appended
    /// as a system update. Returns one line per session, for the head.
    pub fn attach_seat(&mut self, seat: letibot_flowy::Seat) -> Vec<String> {
        let mut report = Vec::new();
        if let Some(old) = self.seat.replace(seat.clone()) {
            old.stop();
            report.push(format!("released the previous seat `{}`", old.name()));
        }
        seat.start();
        self.parts
            .skills
            .set_shelf(std::sync::Arc::new(letibot_flowy::FabricShelf::new(
                seat.clone(),
            )));
        let ids: Vec<String> = self
            .open
            .keys()
            .filter(|id| is_root(&self.registry, id))
            .cloned()
            .collect();
        for id in ids {
            let Some(slot) = self.slots.get(&id).cloned() else {
                continue;
            };
            let cond = self.attach_session(&seat, &id, &slot);
            self.declare_flowy_monitor(&id, Some(cond));
            match self.refresh_fabric(&id) {
                Ok(true) => report.push(format!(
                    "{id}: attached; the fabric block went in as a system update"
                )),
                Ok(false) => report.push(format!("{id}: attached")),
                Err(e) => report.push(format!(
                    "{id}: attached; the fabric block could not be read: {e}"
                )),
            }
        }
        report
    }

    /// `/flowy logout`: stop the seat, empty every slot, retire the monitors.
    pub fn detach_seat(&mut self) -> Option<String> {
        let seat = self.seat.take()?;
        seat.stop();
        for slot in self.slots.values() {
            *slot.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }
        self.seated.clear();
        for h in self.open.values() {
            if let Some(m) = h.monitors() {
                m.retire("flowy", "the seat was released");
            }
        }
        Some(seat.name().to_string())
    }

    /// The fabric for a root session's system prompt, and its provenance into the
    /// disclosure. A session that is not seated gets neither, and the disclosure says
    /// `fabric: OFF` when a seat exists. The block itself is rendered at open
    /// (`Config::compose_for_seat`), for the doors the seated tools give it.
    ///
    /// Message 0 is never rewritten, so a resumed session keeps the block it
    /// was opened with; `refresh_fabric` appends a system update when a fresh
    /// reading differs — that is the path after a compaction too.
    fn with_fabric(&mut self, mut cfg: Config, seated: bool) -> Config {
        let Some(seat) = &self.seat else {
            return cfg;
        };
        if !seated {
            return cfg;
        }
        let (part, line) = self.fabric_block(seat);
        cfg.seat_prompt.get_or_insert_with(Default::default).fabric = Some(part);
        cfg.fabric = Some(line);
        cfg
    }

    /// **What the session was given of the fabric, recorded once it is open** — the block
    /// as its prompt rendered it, and the doors its seat has, which every refresh renders
    /// for again.
    fn fabric_opened(&mut self, session_id: &str, h: &Harness) {
        let cfg = h.config();
        let (Some(prompt), Some(facts)) = (&cfg.seat_prompt, &cfg.seat_facts) else {
            return;
        };
        let Some(part) = &prompt.fabric else {
            return;
        };
        let reach = facts.fabric_reach();
        self.fabric_seen
            .insert(session_id.to_string(), (reach, part.render(reach)));
    }

    /// The fabric as read now and its one-line provenance.
    fn fabric_block(&self, seat: &letibot_flowy::Seat) -> (crate::config::FabricPart, String) {
        use crate::config::FabricPart;
        use letibot_flowy::FabricSource;
        let (ctx, source) = seat.fabric();
        match (ctx, source) {
            (Some(ctx), src @ FabricSource::Live) => {
                let line = format!(
                    "{} skill summaries and {} memory titles from the node, read at {}; the \
                     model loads a skill through `skill` and reads a memory with the `flowy` \
                     CLI, when its seat has those",
                    ctx.skills.len(),
                    ctx.memories.len(),
                    ctx.read_at
                );
                (FabricPart::Read { ctx, source: src }, line)
            }
            (Some(ctx), src @ FabricSource::Cached { .. }) => {
                let line = format!(
                    "CACHED — the node is unreachable; {} skills and {} memories from the copy \
                     read at {}, labelled stale in the prompt",
                    ctx.skills.len(),
                    ctx.memories.len(),
                    ctx.read_at
                );
                (FabricPart::Read { ctx, source: src }, line)
            }
            (_, FabricSource::Unreachable { why }) => (
                FabricPart::Unreachable(letibot_flowy::FabricContext::render_unreachable(
                    seat.name(),
                    &why,
                )),
                format!("UNREACHABLE — {why}; no cached copy, and the prompt says so"),
            ),
            (None, src) => (
                FabricPart::Unreachable(letibot_flowy::FabricContext::render_unreachable(
                    seat.name(),
                    &format!("{src:?}"),
                )),
                "UNREACHABLE".into(),
            ),
        }
    }

    /// Re-read the fabric for a root session and, when the block changed, append
    /// it as a system update (§5.3). Called after a compaction. Returns whether
    /// an update went in.
    pub fn refresh_fabric(&mut self, session_id: &str) -> Result<bool, HarnessError> {
        // The fabric block is a system update, which is the harness's — so it is asked of the
        // thread that owns it. `attach_seat` is the caller here.
        if let Some(court) = self.court_serving(session_id) {
            let id = session_id.to_string();
            if let Some(out) = court.ask(move |s: &mut Sessions<'static>| s.refresh_fabric(&id)) {
                return out;
            }
        }
        let Some(block) = self.fabric_due(session_id) else {
            return Ok(false);
        };
        let h = self.harness(session_id)?;
        h.system_update(&block)?;
        self.fabric_saw(session_id, block);
        Ok(true)
    }

    /// The fabric block a seated root should be told after a compaction, or `None` when it is not
    /// seated or the block has not changed since it last saw it. Rendered for the doors the
    /// session's seat has, the same as the block its prompt opened with.
    fn fabric_due(&self, session_id: &str) -> Option<String> {
        let seat = self.seat.as_ref()?;
        if !self.seated.contains_key(session_id) {
            return None;
        }
        let (reach, seen) = self.fabric_seen.get(session_id)?;
        let (part, _line) = self.fabric_block(seat);
        let block = part.render(*reach);
        (seen != &block).then_some(block)
    }

    /// Record that the session was told `block`.
    fn fabric_saw(&mut self, session_id: &str, block: String) {
        if let Some((_, seen)) = self.fabric_seen.get_mut(session_id) {
            *seen = block;
        }
    }

    /// Declare the session's `flowy` monitor — continuous, owned by the session
    /// scope, renewed by the seat's loop — and arm the wake. The operator's
    /// words: *"I don't want to fiddle with monitors."* The model never declares
    /// this one; it finds it in `job_list`.
    ///
    /// A session whose backend cannot hold a monitor gets the tool and no
    /// listener, and the disclosure says `NOT SEATED` rather than pretending.
    fn declare_flowy_monitor(
        &mut self,
        session_id: &str,
        cond: Option<Arc<letibot_flowy::InboxCondition>>,
    ) {
        let (Some(cond), Some(seat)) = (cond, &self.seat) else {
            return;
        };
        let Some(h) = self.open.get(session_id) else {
            return;
        };
        let (Some(monitors), Some(owner)) = (h.monitors().cloned(), h.session_scope()) else {
            if let Some(hub) = self.registry.get(session_id) {
                hub.publish(SessionEvent::Warning {
                    code: "flowy_not_seated".into(),
                    detail: format!(
                        "seat `{}` is attached to this session but its backend cannot hold \
                         a monitor, so nothing said on the fabric can wake it. The `flowy` \
                         tool still speaks; `flowy status` shows what is pending.",
                        seat.name()
                    ),

                    compaction: None,
                });
            }
            return;
        };
        use letibot_tools::exec::monitor::{CustomWatch, MAX_TTL, Watch};
        match monitors.declare(
            "flowy",
            owner,
            Watch::Custom(CustomWatch(cond)),
            None,
            "harnessd",
            MAX_TTL,
            true,
        ) {
            Ok(_) => {
                seat.keep_renewing(&monitors, "flowy");
                self.arm_wake(session_id);
            }
            Err(e) => {
                if let Some(hub) = self.registry.get(session_id) {
                    hub.publish(SessionEvent::Warning {
                        code: "flowy_not_seated".into(),
                        detail: format!("the `flowy` monitor could not be declared: {e}"),

                        compaction: None,
                    });
                }
            }
        }
    }

    /// **What a session is attached to, from the daemon's own command line — and the `model`
    /// here is WHO ANSWERS, not what the vocabulary binds.**
    ///
    /// `cfg.model` is the local BINDING: the alias whose tokenizer renders the prompt, which the
    /// launcher passes on every start. `cfg.prompt_model_name()` is the model that actually
    /// answers — `deepseek/deepseek-flash` on a box whose `providers.toml` says `[default]
    /// provider = "deepseek"`, `qwen-3.8-flash-next` on one answering locally.
    ///
    /// This wrote `cfg.model`, and that is wrong in exactly one direction: a daemon on the
    /// operator's `[default]` provider recorded and DREW `qwen-3.8-flash-next` for every session
    /// it opened — `SessionWiring` is what a head's `Hello` carries, what a session's row keeps
    /// (`Harness::open`'s `SessionRecord.model_id`), and what the ctrl-s picker draws beside each
    /// stored session, so the header's first frame, the picker line and `letibot --sessions` all
    /// said qwen about a session whose turns were going to deepseek. The operator met it as
    /// *"with --new it started with qwen, not default model"*, one minute after starting it on
    /// the default. The `model` settings row got this right (`Config::disclosures`, which reads
    /// the provider), which is why the two disagreeing was the whole defect: one session described
    /// two ways.
    pub fn wiring(cfg: &Config) -> SessionWiring {
        SessionWiring {
            model: cfg.prompt_model_name(),
            dialect: cfg.dialect.name().to_string(),
            endpoint: cfg.endpoint.authority(),
            workspace: cfg.workspace.display().to_string(),
        }
    }

    pub fn registry(&self) -> &Arc<Registry> {
        &self.registry
    }

    /// Open a session now, rather than on its first prompt.
    ///
    /// This is what a `ResumeSession` turns into: a resume's chain verification, its
    /// dialect refusal and its republished transcript all live in `Harness::open`,
    /// and running them lazily would put all three inside the first prompt — so a
    /// head that resumed a session and attached to it would see an empty screen, and
    /// find out it had failed only by typing into it.
    ///
    /// The failure is announced on **that session's own log**, which is where the
    /// head that asked for it is looking, and the daemon keeps serving: one session
    /// that cannot be rebuilt is not a reason to take down the others.
    /// `Ok(false)` means it was already open — the daemon's own first session, which
    /// `open_first` opened eagerly and whose creation also queued a `Work::Open`.
    /// Reported rather than swallowed so the caller does not print the resume banner
    /// twice for one session.
    pub fn open(&mut self, session_id: &str) -> Result<bool, HarnessError> {
        self.open_as(session_id, None)
    }

    /// **One session this daemon holds, by id** — for a test that has to ask a session something no
    /// door of the daemon's exposes.
    ///
    /// `#[cfg(test)]` and `pub(crate)` for the reason [`Harness::set_subagents_for_test`] is: the
    /// field stays private, this is in no binary, and the one caller is `harness::queue_e2e`'s
    /// fallback test, which needs the notice a SESSION builds rather than the queue's own row.
    #[cfg(test)]
    pub(crate) fn held(&mut self, session_id: &str) -> Option<&mut Harness> {
        self.open.get_mut(session_id)
    }

    /// [`Sessions::open`] with the two facts a session the DAEMON ITSELF starts needs — the door
    /// [`Sessions::ensure_gatekeeper`] uses, because the daemon's own `--role` and `--title` are the
    /// OPERATOR's conversation's and the review queue's session is not that conversation.
    fn open_as(&mut self, session_id: &str, own: Option<OwnSession>) -> Result<bool, HarnessError> {
        // **A session served by its own thread IS open** — its harness is there, not here — so an
        // open for it is the `Work::Open` its creation rang, arriving after the thread took it.
        if self.open.contains_key(session_id) || self.courts.contains_key(session_id) {
            // **An open session with no thread yet gets one here.** The daemon's own first session
            // is the case: `open_first` opened it before there was a worker to hand it to, and its
            // creation rang the `Work::Open` that lands on this line.
            self.spawn_court(session_id);
            return Ok(false);
        }
        let hub = self.registry.get(session_id);
        // **The daemon's own session, held as a fact and not as a value**: `own` is the seat, the
        // title and the prompt for a session the daemon starts for itself, and it is consumed by
        // the open below while the origin it implies is still needed afterwards.
        let own_session = own.is_some();
        match self.harness_as(session_id, own) {
            Ok(h) => {
                if let Some(r) = h.resumed() {
                    let r = r.clone();
                    if let Some(hub) = &hub {
                        for note in &r.notes {
                            hub.publish(SessionEvent::Warning {
                                code: "resume_note".into(),
                                detail: note.clone(),

                                compaction: None,
                            });
                        }
                    }
                }
                if let Some(hub) = &hub {
                    for note in h.open_notes() {
                        hub.publish(SessionEvent::Warning {
                            code: "open_note".into(),
                            detail: note.clone(),

                            compaction: None,
                        });
                    }
                }
                self.publish_title(session_id);
                // **And who this session is for** — written now that its row exists. Three
                // answers: the review queue's own session (`own`, which is the only caller of
                // this door), a one-shot's, and otherwise whatever the door that created it
                // recorded — a head's `NewSession` (the operator's, or a package's) or a
                // resumed session's own row.
                //
                // The row is what a picker tomorrow reads, and a session is opened LAZILY, so
                // this is the moment the two facts are both in hand. See
                // `letibot_sessionlog::SessionOrigin`.
                self.registry.record_origin(
                    session_id,
                    if own_session {
                        SessionOrigin::Queue
                    } else if self.base.one_shot {
                        SessionOrigin::OneShot
                    } else {
                        self.registry.origin_of(session_id)
                    },
                );
                // **A RESUMED SESSION ARMS ITS OWN CLOCK, because nothing else will.**
                //
                // `nag_due` is in-memory and the only other arming point used to be the end of a
                // turn — so a daemon that came back with an unfinished plan sat there for ever: no
                // turn had ended yet, so no deadline existed, so the idle worker had nothing to wake
                // for. The operator, watching exactly this on a resumed session: *"so when I bring
                // session back it will not fire right now - this is exactly what i see with rano"*.
                //
                // **It is safe because the plan was RESTORED, not invented.** `Harness::open`
                // rebuilds the todo board from the store (`store.todos(session_id)`), so the rows
                // this arms on are the plan the model was working from — the ones it wrote before
                // the restart. A session with a finished or empty plan arms nothing, which is the
                // ordinary case and still costs no wake.
                //
                // The nag is a real turn, so this is a session that will speak once, unprompted, a
                // minute after it is reopened with work outstanding. That is the intent: it is the
                // replacement for a model's memory that did not survive the restart either.
                self.rearm_todo_nag(session_id);
                if self.registry.parent_of(session_id).is_some() {
                    self.arm_park();
                }
                // **And the thread that will serve it from now on**, handed everything above.
                self.spawn_court(session_id);
                Ok(true)
            }
            Err(e) => {
                if let Some(hub) = &hub {
                    hub.publish(SessionEvent::Warning {
                        code: "resume_failed".into(),
                        detail: e.to_string(),

                        compaction: None,
                    });
                }
                Err(e)
            }
        }
    }

    /// What a session was rebuilt from, for the daemon's own banner.
    pub fn resume_report(&self, session_id: &str) -> Option<crate::harness::ResumeReport> {
        self.open.get(session_id).and_then(|h| h.resumed().cloned())
    }

    /// What opening a NEW session decided that the operator should hear — the mode
    /// the project store chose over the flag, for one. Empty for a resumed session.
    pub fn open_notes(&self, session_id: &str) -> Vec<String> {
        self.open
            .get(session_id)
            .map(|h| h.open_notes().to_vec())
            .unwrap_or_default()
    }

    /// Move a title the harness derived into the registry, where a picker reads it.
    ///
    /// The harness holds a `Hub` and not a `Registry` — deliberately, so a `Harness`
    /// can be built in a test with nothing else — so the handover happens here, at
    /// the one layer that holds both.
    fn publish_title(&mut self, session_id: &str) {
        let Some(title) = self
            .open
            .get_mut(session_id)
            .and_then(|h| h.take_new_title())
        else {
            return;
        };
        self.name_session(session_id, title);
    }

    /// The title's two homes: the registry (a picker's row) and, for a seated root, the session's
    /// address on the fabric (`@seat/title`).
    fn name_session(&mut self, session_id: &str, title: String) {
        if let Some(c) = self.seated.get(session_id) {
            c.set_alias(&title);
        }
        self.registry.set_title(session_id, title);
    }

    /// The harness for `session_id`, opening it if a head made the session and
    /// nothing has run in it yet.
    ///
    /// `Err` is a session that cannot be served at all; the caller announces it on
    /// that session's log rather than taking the daemon down, because every other
    /// session is still fine.
    fn harness(&mut self, session_id: &str) -> Result<&mut Harness, HarnessError> {
        self.harness_as(session_id, None)
    }

    /// [`Sessions::harness`] with the two facts of [`OwnSession`], for the one caller that opens a
    /// session the daemon itself owns rather than one the operator described.
    fn harness_as(
        &mut self,
        session_id: &str,
        own: Option<OwnSession>,
    ) -> Result<&mut Harness, HarnessError> {
        if !self.open.contains_key(session_id) {
            let hub = self.registry.get(session_id).ok_or_else(|| {
                HarnessError::Setup(format!("no session {session_id} in this daemon"))
            })?;
            // **A session live under another owner is not the worker's to open** (R68.1). A
            // subagent's harness lives on the thread its parent spawned it on; a second one built
            // here would be a second writer on its transcript and would replace its `watch_runs`,
            // `set_prompt` and `notes` under it. The hub's pop already refuses the worker
            // (`Hub::try_command`); this is the same fact at the door every worker path opens
            // through, so a scripted `submit` or a future arm cannot spell the old defect either.
            if hub.owner() == Owner::OwnThread {
                return Err(HarnessError::Setup(format!(
                    "session {session_id} is a subagent served by its own thread, so the \
                     daemon's worker will not open a second harness on it; prompt it from a head \
                     attached to it, or message it with `task_message`"
                )));
            }
            // The session's own tree, when the registry has one — a head that made
            // this session named the directory it was standing in, and a resumed one
            // carries the root from the store. `self.base.workspace` is the daemon's
            // command line, which is a fact about the daemon.
            let ws = self.registry.wiring(session_id).workspace;
            // Whether this is somebody's subagent is the registry's fact, not the
            // command line's: a resumed subagent must not be seated as a root.
            // From the entry, not a scan of the store (R68.6) — see `is_root`.
            let parent = self.registry.parent_of(session_id);
            let mut cfg = Config {
                session_id: session_id.to_string(),
                parent_session_id: parent,
                workspace: if ws.is_empty() {
                    self.base.workspace.clone()
                } else {
                    std::path::PathBuf::from(ws)
                },
                ..self.base.clone()
            };
            // **And the two facts of a session the DAEMON owns**, when the caller named one. Set on
            // the SESSION's config and never on `self.base`: one session opening on a seat and a
            // name of its own must not re-seat or rename the daemon's other sessions, and the two
            // are the same struct one clone apart.
            let mut prompt = None;
            if let Some(own) = own {
                cfg.seat = own.seat;
                // **The title is on the config and not only in the registry**, which is what makes
                // the session's own STORE row carry it: `Harness::open` writes that row from `cfg`,
                // and a name that lived only in memory would come back `(unnamed)` in
                // `letibot --sessions` after every restart.
                cfg.title = own.title;
                prompt = own.prompt;
            }
            let (tool, cond) = self.seat_tool(session_id);
            // The system prompt is composed at open, for the tools the seat actually holds —
            // the same rule as the first session: message 0 is written from it and never
            // rewritten after. **And the paragraph only THIS session is given** goes after the
            // notes, part of the same frozen prefix; a session with none of its own (`None`,
            // which is every session the operator started) is the composed prompt byte for byte.
            cfg.seat_prompt = Some(crate::config::SeatPrompt {
                own: prompt,
                fabric: None,
            });
            let cfg = self.with_fabric(cfg, cond.is_some());
            let mut h = match Harness::open_with_registry(
                &self.parts,
                cfg,
                hub,
                None,
                tool,
                self.registry.clone(),
            ) {
                Ok(h) => h,
                Err(e) => {
                    // **The seat's bookkeeping for a session that did not open goes with it**
                    // (R67 item 21). `seat_tool` inserted the slot — and, with a seat, the
                    // condition in `seated` — before the open could fail, so a session that never
                    // opened kept both: a slot nothing fills and an address on the fabric for a
                    // session nobody holds. A retry inserts them again.
                    self.slots.remove(session_id);
                    self.seated.remove(session_id);
                    return Err(e);
                }
            };
            self.fabric_opened(session_id, &h);
            // **The lazy open is the resume a head reaches** — `/resume`, a switch
            // from the picker, a subagent's parent coming back. The row the switch
            // wrote is read here, so a session this daemon did not start running
            // still comes back on its own model rather than the CLI default.
            h.restore_provider_choice();
            // **And the stood-down compaction with it** — same reason as the first
            // session's open: the pair on the row is the guard's finding, and a
            // session this daemon adopts must not come back with it unrecorded.
            h.restore_auto_compact();
            // **The operator's run state is born with the harness** — one `Arc`, held by
            // this map and cloned into the harness, so the dispatch arm (here) and the
            // round boundary's pickup (in the harness) answer "is a run going?" from one
            // place. See `crate::bangrun`.
            let bang = self
                .bangs
                .entry(session_id.to_string())
                .or_insert_with(crate::bangrun::State::new)
                .clone();
            h.set_bang_state(bang);
            // **What a daemon that is gone left running is brought back, or reminded,** as the
            // session is opened — before the ring below, so the wake carries the `[restart]`
            // notice. See `crate::restart`.
            h.restore_after_restart();
            self.open.insert(session_id.to_string(), h);
            self.declare_flowy_monitor(session_id, cond);
            // **A SESSION THAT HAS JUST BEEN BUILT IS PUT TO WORK, WHICH ARMING IS NOT.**
            //
            // This is the one place a harness is built for a session the daemon holds — the
            // constructor above builds the first session's inline — so this is the one place
            // every restored session passes through: a head's `ResumeSession`, the merge queue's
            // project session, the review queue's own, and a session whose first act is somebody's
            // prompt. What each of them owes is not only the PLAN (that clock is armed a few lines
            // up, and again at every turn's end) but the facts that have no clock of their own — a
            // row whose condition the world already meets, an entry the queue is holding, a
            // handover — and those are carried by a RING, which a restart throws away. See
            // [`Sessions::put_a_restored_session_to_work`].
            self.put_a_restored_session_to_work(session_id);
            self.open_plugins(session_id);
        }
        Ok(self.open.get_mut(session_id).expect("just inserted"))
    }

    /// Sessions with a harness behind them. The rest exist and have never run.
    pub fn opened(&self) -> usize {
        // A session on its own thread is open — its harness is there, not here — so counting only
        // `self.open` would answer *nothing runs* about a daemon that is running everything.
        self.open.len() + self.courts.len()
    }

    /// **The plugin layers `session_id` has**, discovered once, when it opens.
    ///
    /// At open rather than on the first pane because of C4's own shape: a head learns a pane
    /// EXISTS from a `SessionEvent::PluginPane`, and the view a call answers is also its title —
    /// so a session that discovered its layers lazily would have a panes list that filled in the
    /// first time somebody guessed a name. [`crate::plugins::PluginHost::announce`] is the call
    /// that publishes them, and it runs no look: nothing reads a project's files until somebody
    /// opens the dashboard.
    ///
    /// A session with no hub (removed under us) or no harness gets no host, which a `PaneOpen`
    /// reads as *silence* — the same answer a session with no driver gives.
    fn open_plugins(&mut self, session_id: &str) {
        if self.plugins.contains_key(session_id) {
            return;
        }
        let Some(hub) = self.registry.get(session_id) else {
            return;
        };
        let Some(workspace) = self
            .open
            .get(session_id)
            .map(|h| h.workspace().to_path_buf())
        else {
            return;
        };
        // **The daemon's session list, handed to the host** — a pane on disk that declares the
        // `sessions` feed is drawn from it (`docs/plugins-plan.md` §4.9). The registry is the one
        // holder of *every* session, live and on disk, so it is the whole source; a host built by
        // a test has none and a pane that declares the feed says so on its screen.
        let host = Arc::new(
            crate::plugins::PluginHost::production(session_id, &workspace).with_sessions(Arc::new(
                crate::plugins::feed::RegistryFeed::new(self.registry.clone()),
            )),
        );
        host.announce(&hub);
        let driver: Arc<dyn letibot_sessionlog::registry::PaneDriver> = host.clone();
        self.registry.set_pane_driver(session_id, driver);
        // **And the `plugin` tool asks the same host.** It was seated when the harness opened,
        // before this — a hub and a pane driver exist only here — so it holds a cell and this is
        // the only place that fills one. Without it, `plugin load` would build a SECOND stack and
        // a pane loaded through the tool would not be the pane the head draws.
        if let Some(h) = self.open.get(session_id) {
            *h.plugin_slot().lock().unwrap_or_else(|e| e.into_inner()) = Some(host.clone());
        }
        self.plugins.insert(session_id.to_string(), host);
    }

    /// **The plugin tick**, run from the daemon's idle pass and never blocking it: the hot-reload
    /// poll (which hands a changed layer to a thread), the panes whose `every_s` has come round,
    /// and the notices a layer owes this session's model.
    ///
    /// The notices are the half that needs this worker: a notice is a transcript row, and a row is
    /// the harness's to write — which is why the plugin host queues them and does not publish
    /// them itself. Everything else in the tick would work without the worker, and that is
    /// deliberate: a pane must keep answering while a turn is running.
    pub fn plugin_tick(&mut self) {
        // **A session on its own thread ticks its own layers**, on the clock that thread keeps:
        // the notices are transcript rows and the rows are the harness's to write. Asking here is
        // for the callers that drive `Sessions` by hand; the daemon's own pass finds nothing.
        let courted: Vec<String> = self.courts.keys().cloned().collect();
        for id in courted {
            if let Some(court) = self.courts.get(&id) {
                court.post(|s: &mut Sessions<'static>| s.plugin_tick());
            }
        }
        if self.plugins.is_empty() {
            return;
        }
        let ids: Vec<String> = self.plugins.keys().cloned().collect();
        for id in ids {
            let Some(host) = self.plugins.get(&id).cloned() else {
                continue;
            };
            for notice in host.take_notices() {
                if let Some(h) = self.open.get_mut(&id) {
                    if let Err(e) = h.note_to_model(&notice) {
                        eprintln!("  {id} · a plugin notice could not be written: {e}");
                    }
                }
            }
            if let Some(hub) = self.registry.get(&id) {
                host.tick(hub);
            }
        }
    }

    /// When the plugin tick next has something to do: the earliest hot-reload poll or pane refresh
    /// over every session this daemon holds. `None` for a daemon whose sessions have no layers,
    /// which is every daemon without plugins — so this adds no wake to a box that has none.
    fn plugins_due_at(&self) -> Option<Instant> {
        self.plugins.values().filter_map(|h| h.next_at()).min()
    }

    /// **Close every held session's backend, and say what each one delivered** — the
    /// one-shot's last act. A session's placement hands its work back only when its backend
    /// closes: a remote host's branch is fetched home, a firecode VM is brought down and its copy
    /// delivered. The one-shot exited without this, so the first live run on lab2x1 committed
    /// there and nothing came back (2026-10-26). Only the sessions this daemon holds directly;
    /// a court's harness closes on its own thread when that session closes.
    pub fn close_backends(&self) -> Vec<String> {
        // **The daemon is going, so everything it supervises goes too** — the same rule a session's
        // own close keeps, applied to every session this daemon holds. `--stop` reaches here; a
        // crash does not, which is why the supervisor's restart path is what cleans up after one.
        for id in self.open.keys() {
            crate::supervise::teardown_session(id, "the daemon stopping");
        }
        // **Every block of ports this daemon's sessions held goes back** (RH9) — before the
        // backends close, because a session's placement is what a remote teardown's ssh runs over.
        if let Some(path) = &self.base.store {
            for id in self.open.keys() {
                for line in crate::leases::release_session(path, id, "the daemon stopping") {
                    eprintln!("  {id} · lease: {line}");
                }
            }
        }
        self.open
            .values()
            .filter_map(|h| h.close_backend())
            .collect()
    }

    /// **Stop every session's subagents and wait for their threads to leave** — the daemon's
    /// exit path, and the door the process's own exit needs before it runs `_dl_fini`.
    ///
    /// # Why the process's exit needs this at all
    ///
    /// A child's thread is spawned **detached** (`HarnessTaskRunner::start` drops the
    /// `JoinHandle`), so nothing in the process can be joined to it. `exit()` runs every ELF
    /// finalizer, and `_dl_fini` tears `libggml-cuda`/`libcudart` down underneath a thread
    /// still inside libllama's tokenizer — measured on `message_between_turns`, `si_addr: 0x3`,
    /// `SEGV_MAPERR`, **after every test had printed `ok`**. The daemon has the same shape: it
    /// calls `daemon.shutdown()` (which closes every hub) and then returns `Ok(0)`, which
    /// `bin/harnessd.rs` turns into `std::process::exit(0)` with the child threads still
    /// inside the libraries those finalizers unload. See [`Harness::wait_for_children`].
    ///
    /// # Stop first, then wait
    ///
    /// Closing a hub is what a child PARKED between turns leaves on; it does not end a turn
    /// already running. So each session's children are told to stop first
    /// ([`Harness::stop_children`]), which is the interrupt that reaches a running turn at its
    /// next round boundary — and only then are the threads waited for. The order is the whole
    /// of this: a wait that ran first would spend its bound on a child nobody had told to
    /// stop, and a child between turns would never be told at all.
    ///
    /// # The bound, and what it costs
    ///
    /// `timeout` is the caller's — `cli::SUBAGENT_EXIT_GRACE` is the daemon's, and the number
    /// is argued there. A child inside a single model call or a long tool cannot be
    /// interrupted at all, and a local turn on this box has been measured in minutes, so an
    /// unbounded wait would make Ctrl-C a hang. When the bound is spent this returns a
    /// sentence naming the sessions it could not wait for rather than blocking: a crash nobody
    /// can explain is worse than a sentence naming what could not be waited for.
    ///
    /// The wait covers each session's **whole tree** and not one level of it — that is
    /// [`Harness::wait_for_children`]'s own rule, and the reason it is safe to wait per session
    /// here: a child's slots are its root's (`Parts::tree_slots`).
    ///
    /// Returns [`SubagentExit`], whose [`SubagentExit::sentence`] is the line to print: it is
    /// `None` when nothing was running, so a caller prints it unconditionally without putting a
    /// line on every ordinary quit.
    pub fn drain_subagents(&self, timeout: std::time::Duration) -> SubagentExit {
        let began = std::time::Instant::now();
        let mut told = 0;
        let mut left = Vec::new();
        // **The sessions on their own threads first.** Each is asked to stop its children and wait
        // for them, and answers from the only thread that may drive its harness — so a session
        // that is mid-turn answers when the turn ends, which is what the bound is spent on.
        let mut answers = Vec::new();
        for (id, court) in &self.courts {
            let (tx, rx) = std::sync::mpsc::channel();
            court.finish(id, timeout.saturating_sub(began.elapsed()), tx);
            answers.push((id.clone(), rx));
        }
        for (id, rx) in answers {
            let remaining = timeout.saturating_sub(began.elapsed());
            match rx.recv_timeout(remaining.max(Duration::from_millis(1))) {
                Ok((was_told, was_left)) => {
                    // `told` counts sessions that had something to stop — a session with nothing
                    // live says nothing, exactly as the loop below has always counted.
                    if was_told {
                        told += 1;
                    }
                    if was_left {
                        left.push(id);
                    }
                }
                // No answer inside the bound: the session is still inside a turn, so its children
                // have not even been told. Named rather than counted, and by the session a person
                // can go and look at.
                Err(_) => left.push(id),
            }
        }
        // **And the threads themselves, which the process's exit needs gone.** `exit()` runs every
        // ELF finalizer, and `_dl_fini` tears the libraries down underneath a thread still inside
        // them — the crash this wait exists to prevent. `JoinHandle::join` would be the plain door
        // and cannot be used from `&self`, so this is the same bound read as a poll.
        for (id, court) in &self.courts {
            let Some(thread) = &court.thread else {
                continue;
            };
            while !thread.is_finished() && began.elapsed() < timeout {
                std::thread::sleep(Duration::from_millis(2));
            }
            if !thread.is_finished() && !left.iter().any(|named| named == id) {
                left.push(id.clone());
            }
        }
        // **The sessions this struct holds itself** — every session of a daemon that never armed
        // the threads (a test), and any session whose thread could not be started.
        let ids: Vec<String> = self.open.keys().cloned().collect();
        for id in ids {
            let Some(harness) = self.open.get(&id) else {
                continue;
            };
            let (was_told, was_left) =
                stop_children_of(harness, timeout.saturating_sub(began.elapsed()));
            if was_told {
                told += 1;
            }
            if was_left {
                left.push(id);
            }
        }
        if left.is_empty() {
            SubagentExit::Waited { told }
        } else {
            SubagentExit::GaveUp {
                sessions: left,
                waited: timeout,
            }
        }
    }

    /// Submit one prompt to a session and wait for the answer.
    ///
    /// The scripted path (`harnessd --prompt …`), which has no head and no socket
    /// traffic. It goes through the same `harness()` as the worker, so a scripted
    /// run and a driven one open a session the same way.
    pub fn submit(&mut self, session_id: &str, text: &str) -> Result<Reply, HarnessError> {
        // **A session on its own thread answers there** — `cli.rs`'s scripted steps are the caller,
        // and they run before the worker loop starts, so this waits on a thread that is idle.
        if let Some(court) = self.court_serving(session_id) {
            let id = session_id.to_string();
            let text = text.to_string();
            if let Some(out) = court.ask(move |s: &mut Sessions<'static>| s.submit(&id, &text)) {
                return out;
            }
        }
        self.run_prompt(session_id, text, &[])
    }

    /// **One turn, and everything that has to happen around one.**
    ///
    /// Both ways a prompt reaches a session go through here — the scripted
    /// `--prompt` path and the command queue a head's prompt lands on — because
    /// they used to be two copies of this sequence and one of them was missing a
    /// step. The missing step was the automatic compaction, and the path missing it
    /// was the one every interactive session takes: a banner that said *"compacts
    /// automatically with 16384 left"* to sessions where nothing called it, and an
    /// operator who met the wall and was told a compaction had been attempted.
    /// *"it failed to compact lol"* — it had not failed, it had not run.
    ///
    /// A second copy of a sequence is a second place to forget one of its steps, so
    /// there is one.
    fn run_prompt(
        &mut self,
        session_id: &str,
        text: &str,
        attachments: &[letibot_sessionlog::protocol::Attachment],
    ) -> Result<Reply, HarnessError> {
        // **Opened first, lazily** — the resume a head reaches (`/resume`, a switch from the
        // picker). A session that cannot be opened ran nothing, and says so on its own log.
        if let Err(e) = self.harness(session_id) {
            self.warn(
                session_id,
                "session_unavailable",
                format!("this session could not be opened, so nothing was run: {e}"),
            );
            return Err(e);
        }
        // **Everything around the turn is the shared tail's** (R68.4): the plan-check earned
        // back (*"but certainly not after my message"*), the wall checked BEFORE the send
        // (measured 2026-10-02: a resumed session sent 1,463,497 tokens against a 1,048,576
        // window, because every check ran after a turn), the clock, the failure, the
        // compaction, the continuation past the wall, the title, the wake and the clocks after.
        self.turn(session_id, Door::Prompt, |h| {
            h.submit_attached(text, attachments).map(Some)
        })
        .expect("a prompt always runs a turn")
    }

    /// **A message to a session the daemon holds, between that session's turns** — the row it
    /// is, and the turn it makes.
    ///
    /// The operator's ruling, in their words: *"my commands should start a turn and should be
    /// printed to me"*. A `!` line and a message are the same two acts, for the same reason: the
    /// deposit is a row on the session's own log — the thing the agent reads and the head draws —
    /// and the turn is what makes it an utterance somebody answered rather than a note nobody saw.
    /// `Ignored` here would be the whole defect `run_after_operator_shell` was fixed for, one door
    /// over: the message sits in the transcript until something else starts a turn.
    ///
    /// **The words are [`Harness::submit_a_parents_message`]'s, and that is not a detail.** A
    /// message to a running session arrives through `HubSteering`'s arm as a steering message; a
    /// message to an idle one arrives through this door as a turn. Same speaker
    /// (`Speaker::Agent`, never the operator: a peer's words may not authorise the act they race),
    /// same text, same trail line — a recipient that could tell which door its message came
    /// through would be reading the machinery instead of the message.
    ///
    /// **The tail is every door's, deliberately** ([`crate::turn_tail::run_turn`], R68.4). A
    /// message's turn is a turn: it appends rows, it can reach the wall, and a session that was
    /// only ever spoken to would have compacted never.
    fn run_a_message_from_a_session(
        &mut self,
        session_id: &str,
        from: &str,
        text: &str,
    ) -> Result<Reply, HarnessError> {
        self.turn(session_id, Door::Message, |h| {
            h.parents_message_turn(from, text).map(Some)
        })
        .expect("a message always runs a turn")
    }

    /// **One turn of a session the worker holds, through the shared tail** — every door of
    /// `Sessions` comes here (R68.4), and `harness::serve_child` runs the same
    /// [`crate::turn_tail::run_turn`] for a child. `None` is a door that found nothing to say.
    ///
    /// **The harness is taken out of `open` for the length of the turn** and put back after,
    /// so the owner's half ([`DaemonTurn`]) can reach every other field of `Sessions` while the
    /// tail holds the harness — the borrow the two need at once. Nothing reads `open` for this
    /// session during its own turn: the worker is the only thread with a `Sessions`, and it is
    /// inside the turn.
    fn turn(
        &mut self,
        session_id: &str,
        door: Door,
        f: impl FnOnce(&mut Harness) -> Result<Option<Reply>, HarnessError>,
    ) -> Option<Result<Reply, HarnessError>> {
        let Some(mut h) = self.open.remove(session_id) else {
            return Some(Err(HarnessError::Setup(format!(
                "session {session_id} is not open, so nothing could be run in it"
            ))));
        };
        // **A child this worker holds** — a parked child a parent's message reopened — runs its
        // later turns here rather than on its own thread, and its record moves for the turn's
        // length exactly as there (`harness::later_turn`): `running` while it runs, its settled
        // state again after, `running` still if the daemon stopped under it.
        let record = h
            .config()
            .parent_session_id
            .as_ref()
            .and(h.config().store.clone())
            .map(|path| crate::subagent_record::SubagentRecord {
                store: crate::subagent_record::RunStore {
                    path,
                    cell: Default::default(),
                },
                child: session_id.to_string(),
                registry: self.registry.clone(),
            });
        let prior = record.as_ref().and_then(|r| r.later_turn_began());
        let out = run_turn(
            &mut h,
            &mut DaemonTurn {
                sessions: self,
                session_id,
            },
            door,
            f,
        );
        if let (Some(r), Some(prior)) = (&record, &prior) {
            r.later_turn_ended(prior);
        }
        self.open.insert(session_id.to_string(), h);
        out
    }

    /// Start (or stand down) the idle plan-check for SESSION, as of NOW.
    ///
    /// **An unfinished plan is always armed.** A notice the plan has not been sent comes due one
    /// window after the turn that armed it; a notice it HAS been sent comes due again at the
    /// doubled-and-capped interval [`TodoNagClock::rearm`] keeps — the operator's ruling,
    /// *"hmm, yes obviously it should be a repeated nag"*. A finished plan, and a plan whose only
    /// unfinished row is postponed, arm nothing: that is `nag_should_arm`'s reading and the
    /// operator's `[p]` staying theirs.
    ///
    /// **The rows are handed over whole and `nag_should_arm` does the reading**, so the decision
    /// about what the check may ask about — a postponed row is not it, and neither is a row that
    /// waits on a child still inside its room to breathe — lives in one place rather than being
    /// pre-applied here and asserted somewhere else.
    ///
    /// **And the children are read HERE, at the moment the clock is armed**, because they are the
    /// one input that moves without the board moving: a child that has gone quiet changes what the
    /// check would say about a row nobody has touched, so the snapshot is taken per arming rather
    /// than held (see `child_sessions`).
    ///
    /// **And the jobs with them, read from the SESSION's own process table** — the one fact here the
    /// daemon does not own outright. A handle is answered by the session that holds it
    /// (`Harness::running_jobs`, which asks the same `job_fate` the `[todo] … is due` firing reads
    /// through), so a session the daemon does not hold contributes nothing rather than a guess: its
    /// rows are not this daemon's to arm.
    ///
    /// **And the operator's pause with them** (`/nag pause`), read off the same harness: a paused
    /// session's clock arms only the pause's end. See [`NagHold`].
    fn rearm_todo_nag(&mut self, session_id: &str) {
        let (rows, jobs, hold) = match self.open.get(session_id) {
            Some(h) => {
                let rows = h.todo_list();
                let jobs = h.running_jobs(&rows);
                (rows, jobs, h.nag_hold())
            }
            None => (Vec::new(), RunningJobs::none(), NagHold::Running),
        };
        self.rearm_todo_nag_from(session_id, rows, jobs, hold);
    }

    /// [`Sessions::rearm_todo_nag`] from a harness in hand — the shared tail's door, which holds
    /// the session's harness out of `open` while it runs.
    fn rearm_todo_nag_on(&mut self, session_id: &str, h: &Harness) {
        let rows = h.todo_list();
        let jobs = h.running_jobs(&rows);
        self.rearm_todo_nag_from(session_id, rows, jobs, h.nag_hold());
    }

    /// [`Sessions::rearm_todo_nag`] for a push that ADDED an operator row — the clock's
    /// [`TodoNagClock::operator_added`] in place of its plain re-arm, the inputs read the same way.
    fn rearm_todo_nag_after_an_operator_add(&mut self, session_id: &str) {
        let Some(h) = self.open.get(session_id) else {
            return;
        };
        let rows = h.todo_list();
        let jobs = h.running_jobs(&rows);
        let hold = h.nag_hold();
        let children = child_sessions(
            &self.registry,
            &self.parts.tasks,
            letibot_sessionlog::registry::now_ms(),
        );
        let window = self.nag_window;
        let clock = self
            .nags
            .entry(session_id.to_string())
            .or_insert_with(|| TodoNagClock::with_window(window));
        clock.hold(hold);
        clock.operator_added(&rows, Instant::now(), &children, &jobs);
    }

    fn rearm_todo_nag_from(
        &mut self,
        session_id: &str,
        rows: Vec<TodoItem>,
        jobs: RunningJobs,
        hold: NagHold,
    ) {
        let children = child_sessions(
            &self.registry,
            &self.parts.tasks,
            letibot_sessionlog::registry::now_ms(),
        );
        let window = self.nag_window;
        let clock = self
            .nags
            .entry(session_id.to_string())
            .or_insert_with(|| TodoNagClock::with_window(window));
        clock.hold(hold);
        clock.rearm(&rows, Instant::now(), &children, &jobs);
    }

    /// The operator has spoken to SESSION: a new idle period, and the check may be made again.
    ///
    /// **This is the half of the rule the operator stated as a negative** — *"but certainly not
    /// after my message"*. Their prompt ends a turn, the turn tail arms the clock, and the check
    /// cannot arrive until a minute of silence has passed since it; when they speak again, the
    /// plan check is *earned back* because whatever they said may have changed what the plan
    /// should be. Without this the session would be checked once ever, and a plan re-written by
    /// the operator's own instruction would never be checked at all.
    pub fn note_operator_prompt(&mut self, session_id: &str) {
        // The clock this earns back is the session's, and that clock is on its own thread.
        if let Some(court) = self.courts.get(session_id) {
            let id = session_id.to_string();
            court.post(move |s: &mut Sessions<'static>| s.note_operator_prompt(&id));
            return;
        }
        if let Some(clock) = self.nags.get_mut(session_id) {
            clock.prompt_arrived();
        }
    }

    /// **A session this daemon has just RESTORED is put to work, not left to be woken.**
    ///
    /// # The hole this closes, in one sentence
    ///
    /// Every door that turns a fact that is already true in the STORE into a turn is a bell —
    /// `Harness::wake` is the only reader of `due_todo_rows`, of `queue_notices` and of
    /// `handover_notices`, and `Sessions::wake` is its only servant on the worker — and **a bell
    /// is rung by something that was running before the restart**. So a daemon that came back to
    /// a board whose row is due, to an entry the queue is holding, or to a review it owes opened
    /// its session, armed its clock and parked: nothing ever rang it.
    ///
    /// # Why arming is not enough, in the operator's own case
    ///
    /// Their ruling, and the acceptance case: *"a todo conditioned on job end, and then i
    /// restart head and harnessd. once server is back it should fire - job is gone"*. The
    /// DECISION is already right (`JobFate::Unknown` meets the condition, and the job's own row
    /// answers what became of it), and what the row waited for was a DELIVERY. The watcher that
    /// would have rung for that very settlement is a thread that died with the daemon it belonged
    /// to — `jobwatch`'s watcher is one thread per watched job, and the job it was watching is
    /// not in this process's table at all. So the firing waited for whatever armed the session
    /// next, and the only thing on an idle resumed session's clock is the idle plan-check a
    /// minute later, whose text is the plan's unfinished work and **not** the fate.
    ///
    /// # Why the ring and not a turn here
    ///
    /// `Sessions::wake` could be called on the spot, and it must not be: this runs on the
    /// daemon's own startup path, before `Daemon::run` has taken the worker, and a turn is a
    /// model call — the constructor would then block the banner and the socket on a generation.
    /// The ring is the same door every other between-turns fact arrives through, and the worker
    /// drains it on its first pass. It is cheap when there is nothing to say: a wake with no
    /// notice behind it runs no turn at all (`Harness::wake` answers `Ok(None)`, which is
    /// `Outcome::Ignored`), so a session whose plan is ordinary and whose queue is empty pays
    /// one lock and one comparison.
    ///
    /// # What it does not do
    ///
    /// It does not make the plan check earlier or different: an unfinished plan still speaks a
    /// minute after the last turn, by the operator's own scheduling (*"but certainly not after
    /// my message"*). What it fixes is the class of fact that has NO clock of its own — the ones
    /// that are true in the store and were only ever carried by an event.
    fn put_a_restored_session_to_work(&self, session_id: &str) {
        self.registry.bell().ring_wake(session_id);
    }

    /// When the next check comes due, for the worker's blocking wait.
    ///
    /// NIL when no session has one armed — which is the common case, and the one that lets the
    /// worker sleep until there is real work rather than waking every minute on behalf of nothing.
    /// The next moment this daemon has something to do that is not work arriving: a check to
    /// deliver, or a **sweep for a tool call nothing will answer**.
    ///
    /// The sweep needs its own deadline and cannot rely on a nag, and that is not a detail: the
    /// worker sleeps until this returns, so a session with no check armed and no commands would
    /// never be swept at all — and the stall this exists for happens while a command is RUNNING,
    /// which is exactly when nothing else is due. Armed after every turn (see `arm_sweep`), so a
    /// turn that ends with a call nobody answered is looked at a moment later.
    ///
    /// **This is the plan-check and the sweep; the worker's wait is [`Sessions::next_idle_at`]**,
    /// which is this and the review clock together — see `reviews_at`.
    pub fn next_nag_at(&self) -> Option<Instant> {
        let nags = self.nags.values().filter_map(|c| c.due_at()).min();
        match (nags, self.sweep_at) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) => Some(a),
            (None, b) => b,
        }
    }

    /// **When the worker must next wake with nothing to do** — the earliest of the idle
    /// plan-check, the abandoned-call sweep ([`Sessions::next_nag_at`]) and the store read for the
    /// reviews this daemon's sessions are owed ([`Sessions::serve_reviews`], armed by
    /// [`Sessions::reviews_due`]).
    ///
    /// The review clock is the one that is armed from open and never stands down, and that is the
    /// design rather than an oversight: a review is asked for by the QUEUE's thread and served by a
    /// SESSION's — the review queue's own — and the store is the only thing they share, so the ring
    /// between them is a bell and a bell is per-daemon. See [`REVIEW_PASS_EVERY`].
    pub fn next_idle_at(&self) -> Option<Instant> {
        [
            self.next_nag_at(),
            self.reviews_at,
            self.park_at,
            self.idle_exit_at,
            self.plugins_due_at(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// **Whether the store read for owed reviews is due, spending the deadline when it is** —
    /// `TodoNagClock::due_now`'s shape and for its reason: the disarm is before the work, so a pass
    /// that took longer than the interval does not fire again at once.
    pub fn reviews_due(&mut self) -> bool {
        let now = Instant::now();
        if self.reviews_at.is_some_and(|at| at > now) {
            return false;
        }
        self.reviews_at = Some(now + REVIEW_PASS_EVERY);
        true
    }

    /// **Every review owed to a session this daemon holds, served from the store.**
    ///
    /// The queue's door ([`crate::mergequeue::GatekeeperDoor`]) writes the request row naming the
    /// review queue's session and rings it — and a bell is per-daemon, so a mover in another daemon
    /// rings one nobody is standing at, and a daemon that restarts between the ask and the wake has
    /// nothing left to ring (the ring is in memory; the row is in the store). This is the durable
    /// half: **each daemon serves the reviews its own sessions host**, on its own clock
    /// ([`Sessions::reviews_due`]), from the one medium both daemons can see.
    ///
    /// **One session hosts them, and it is the queue's own** —
    /// [`Sessions::review_session`], whose board is the queue. A review is a `task` child of THAT
    /// session (`Harness::serve_reviews` starts it through the session's own runner), which is the
    /// operator's *"gatekeepers are subagents"* one level down: the reviewer is a subagent of the
    /// tutor, and its asks come up to the tutor's head. The loop below is over every session this
    /// daemon holds rather than over that one id, because which session a row names is the STORE's
    /// fact and not this function's: a row naming a session this daemon does not hold is left
    /// alone, which is the whole arrangement (two daemons each serving what they hold needs
    /// nothing to reach anything).
    ///
    /// **"Holds" means a harness** — `self.open`, which is every session this daemon has built and
    /// can run a turn in. A session that is merely in the registry is not held yet: the open is on
    /// the worker's own queue and `Sessions::open` builds the harness there, after which the next
    /// pass serves it.
    ///
    /// The body is [`Harness::serve_reviews`] — the same function the wake and the round boundary
    /// call — so all three doors make one decision from one body of evidence: the store's
    /// `attempts`/`failed_ms` through `mergequeue::review_retry`, and the session's own `reviewing`
    /// list. Answers how many gatekeepers were started, for the log line.
    pub fn serve_reviews(&mut self) -> usize {
        // **The pass runs where the queue's session is.** That session has a thread of its own now,
        // so the daemon POSTS the pass rather than running it: a gatekeeper mid-turn would
        // otherwise hold the daemon's loop for the length of that turn, which is the defect these
        // threads exist to remove. `0` is what this answers because nobody here is waiting for the
        // count — the log line that read it is the session's own now.
        let queue = self.gatekeeper.clone();
        if let Some(court) = self.courts.get(&queue) {
            court.post(|s: &mut Sessions<'static>| {
                s.serve_reviews();
            });
            return 0;
        }
        let ids: Vec<String> = self.open.keys().cloned().collect();
        let mut started = 0;
        for id in ids {
            if let Some(h) = self.open.get_mut(&id) {
                started += h.serve_reviews();
            }
        }
        started
    }

    /// **The id of the review-queue session this daemon owns** — `s-gatekeeper-<workspace key>`.
    ///
    /// For the log line that tells the operator what to attach to, and for anything that has a
    /// `Sessions` in hand and needs to name the queue. It is [`gatekeeper::session`] of the
    /// workspace this daemon was started on, computed once at construction — see the field, which
    /// also carries why the id is per workspace and not one string for the box.
    pub fn review_session(&self) -> &str {
        &self.gatekeeper
    }

    /// **The review queue's own session, started once** — the daemon's hardcoded gatekeeper
    /// session, whose board is the queue. `Ok(true)` is the pass that started it.
    ///
    /// # Once, and what makes it once
    ///
    /// Two checks, and they answer two different questions:
    ///
    /// * **this daemon** — `self.open` holds it, so the pass does nothing. Every idle pass calls
    ///   this, and a pass is not an event: without this check a second pass would build a second
    ///   harness for one session id.
    /// * **this box** — the REGISTRY, and behind it the STORE. A session this daemon does not hold
    ///   but the store knows is **resumed** ([`Registry::resumable`]) rather than created
    ///   fresh — so a daemon restarted, or a second daemon on the same store, comes back to the
    ///   SAME conversation under the same id instead of making a twin. [`Sessions::review_session`]'s
    ///   own doc says why that id is fixed rather than minted — and per workspace rather than per
    ///   box, which is the defect the fixed-per-box version caused.
    ///
    /// # The prompt, which is this session's own
    ///
    /// [`gatekeeper::prompt`] is appended to the composed prompt at open — the constant paragraph
    /// that says what the rows are and what the session does about them, plus the paragraph that says
    /// WHERE ITS REVIEWERS RUN. The second half is why the prompt is composed here rather than read
    /// off a constant: the placement is this box's answer, and the session has to know which one it
    /// got. See [`OwnSession::prompt`] for where it lands, and the two functions for what they
    /// deliberately leave out.
    ///
    /// # The seat, and why it is not the daemon's
    ///
    /// [`gatekeeper::SEAT`] — see it for why the session that will spawn the reviews cannot be
    /// seated on a seat with a gated tool: the daemon starts it at startup, with nobody attached,
    /// and `Harness::open` refuses an exec seat whose adjudicator reaches nobody.
    ///
    /// # No store, no session
    ///
    /// The queue lives in the store and so does the board filled from it, so a daemon with no
    /// `--store` has no queue to be a session for. That is the line the reviewer door draws
    /// (`NoReviewer`), drawn here rather than opening a session that could never hold a row.
    ///
    /// # A failure is said on a hub, once per reason
    ///
    /// R67 item 15: the error went to the daemon's stderr and nowhere else, so a queue whose
    /// reviewing session could not open looked, from every head, like a queue with nothing to
    /// review. It is published as `review_queue_not_open` on the daemon's own session and on the
    /// queue's session when it is registered — and only when the reason CHANGES, because this is
    /// called every review pass and a failure that stands would otherwise be a line every pass.
    pub fn ensure_gatekeeper(&mut self) -> Result<bool, HarnessError> {
        let out = self.open_the_gatekeeper();
        match &out {
            Ok(_) => self.gatekeeper_said = None,
            Err(e) => {
                let said = e.to_string();
                if self.gatekeeper_said.as_deref() != Some(said.as_str()) {
                    let detail = format!(
                        "the review queue's session `{}` did not open, so nothing in the merge \
                         queue is being reviewed by this daemon: {said}. It is tried again on \
                         every review pass; this is said again only if the reason changes.",
                        self.gatekeeper
                    );
                    let mut ids = vec![self.base.session_id.clone()];
                    if self.registry.get(&self.gatekeeper).is_some() {
                        ids.push(self.gatekeeper.clone());
                    }
                    for id in ids {
                        self.warn(&id, "review_queue_not_open", detail.clone());
                    }
                    self.gatekeeper_said = Some(said);
                }
            }
        }
        out
    }

    /// The body of [`Sessions::ensure_gatekeeper`], whose doc is this function's.
    fn open_the_gatekeeper(&mut self) -> Result<bool, HarnessError> {
        let queue = self.gatekeeper.clone();
        if self.base.store.is_none()
            || self.open.contains_key(&queue)
            || self.courts.contains_key(&queue)
        {
            return Ok(false);
        }
        // **Where its reviewers will run, decided HERE because the session has to be TOLD.**
        // `Harness::serve_reviews` asks the same function at the spawn, so the paragraph this session
        // opens with and the placement its children get are one answer from one reading of the same
        // two facts — and a box that cannot boot a VM is a STATE this session can read
        // (`ReviewerPlacement::NoVm`, with the box's own reason in it), not a fallback nobody can see.
        let prompt = gatekeeper::prompt(&gatekeeper::reviewer_placement(
            self.base.reviewers == crate::config::Reviewers::Vm,
            letibot_tools::firecode::firecode_available(),
        ));
        if self.registry.get(&queue).is_none() {
            // **Resumed, or made, and the store is what decides which.** The stored row carries the
            // title a person may have renamed; a session nobody has ever opened gets the hardcoded
            // one.
            let (title, wiring) = match self.registry.resumable(&queue) {
                Some(brief) => (brief.title, brief.wiring),
                None => (gatekeeper::TITLE.to_string(), Sessions::wiring(&self.base)),
            };
            self.registry.create(&queue, &title, wiring).map_err(|e| {
                HarnessError::Setup(format!(
                    "the review queue's session `{queue}` could not be created: {e}"
                ))
            })?;
            return self.open_as(
                &queue,
                Some(OwnSession {
                    seat: gatekeeper::SEAT,
                    title,
                    prompt: Some(prompt),
                }),
            );
        }
        // **Already in the registry** — a head opened it, or a previous pass did — so the name is the
        // one it already carries.
        // From the registry's entry, not `list()` (R68.6): the entry was created with the stored
        // row's title above (or by a head's resume, which carries it too), and a rename updates
        // both — so this is the same name without a read of every stored session, every pass.
        let title = self
            .registry
            .title_of(&queue)
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| gatekeeper::TITLE.to_string());
        self.open_as(
            &queue,
            Some(OwnSession {
                seat: gatekeeper::SEAT,
                title,
                prompt: Some(prompt),
            }),
        )
    }

    /// **The queue, onto the gatekeeper session's board** — one row per entry waiting for a verdict,
    /// read from the store and SET (not merged) on the session's own half.
    ///
    /// **The board is a VIEW and the states stay in the store**: [`gatekeeper`]'s module docs carry
    /// the boundary, and `TodoBoard::set_queue` carries why the half is replaced rather than upserted.
    /// Nothing here decides what a row is — `gatekeeper::waiting` does, out of the queue's own
    /// `is_ready`/`review_gate`/`served_by` — and nothing here arms a clock by itself: the re-arm
    /// below is the ordinary one, because a board that MOVED is a board worth checking. That is the
    /// rule `CommandKind::SetOperatorTodos` keeps, and for its reason: the clock reads the rows when
    /// it is armed, so a row written onto a board nobody re-armed is a row nothing will ever ask
    /// about.
    ///
    /// **Which workspace the queue is read from is the harness's own** ([`Harness::fill_queue_board`]
    /// reads `self.cfg.workspace`, canonical), so the board and the review pass ask one question
    /// with one spelling rather than two that could drift.
    ///
    /// Returns how many rows the board holds, which is what a caller's log line counts.
    pub fn fill_gatekeeper_board(&mut self) -> usize {
        // Posted for `serve_reviews`' reason: the board is the queue's session's, and that session
        // is on its own thread.
        let queue = self.gatekeeper.clone();
        if let Some(court) = self.courts.get(&queue) {
            court.post(|s: &mut Sessions<'static>| {
                s.fill_gatekeeper_board();
            });
            return 0;
        }
        let rows = match self.open.get_mut(&queue) {
            Some(h) => h.fill_queue_board(),
            // The session did not open — said on the daemon's log by the caller of
            // `ensure_gatekeeper`. Nothing is lost: the next pass fills the board.
            None => return 0,
        };
        self.rearm_todo_nag(&queue);
        rows
    }

    /// Ask for a sweep a moment from now, so the idle arm gets a chance to look.
    ///
    /// A short grace rather than none: `dispatch` returns when the turn is over, and a sweep in the
    /// same instant would be looking at a transcript whose last append may not be published yet.
    pub fn arm_sweep(&mut self, after: std::time::Duration) {
        let at = Instant::now() + after;
        self.sweep_at = Some(match self.sweep_at {
            Some(prev) => prev.min(at),
            None => at,
        });
    }

    /// **When the abandoned-call sweep is due**, or `None` when nothing has armed it — the one
    /// clock in [`Sessions::next_nag_at`] that a turn's door owes, read on its own so a test can
    /// ask whether a door armed IT rather than whether some clock is armed (the plan-check is
    /// re-armed by the turn tail on every door, and would answer for a door that forgot this one).
    pub fn sweep_due_at(&self) -> Option<Instant> {
        self.sweep_at
    }

    /// **The window every session's idle plan-check is made with, from now on** — the tests'
    /// door, as [`TodoNagClock::with_window`] is one level down: a delivery has to be drivable
    /// without sleeping through [`TODO_NAG_AFTER`]. Clocks that already exist take the new
    /// window, and every session this daemon holds is re-armed with it, so a session
    /// `open_first` armed at the production minute comes due at the new one.
    pub fn set_nag_window(&mut self, window: Duration) {
        self.nag_window = window;
        // **And every session's own thread, which keeps its own clock.** A court made after this
        // is given the new window by `court_of`. A session that moved its clock is disarmed and
        // re-armed on the session's own thread, on the rows it holds there.
        let courted: Vec<String> = self.courts.keys().cloned().collect();
        for id in courted {
            if let Some(court) = self.courts.get(&id) {
                court.post(move |s: &mut Sessions<'static>| s.set_nag_window(window));
            }
        }
        for clock in self.nags.values_mut() {
            clock.set_window(window);
        }
        let ids: Vec<String> = self.open.keys().cloned().collect();
        for id in ids {
            self.rearm_todo_nag(&id);
        }
    }

    /// **Sweep every session for a tool call nothing will ever answer.**
    ///
    /// Called from the daemon's own idle arm, which is the same place the todo check is delivered —
    /// and here that placement is not tidiness, it is the whole liveness test. The round loop is
    /// SYNCHRONOUS: a turn blocks inside `runtime.invoke` until the tool answers, so while a round
    /// is in flight this cannot run at all, because the worker is the thread the round is holding.
    /// **The fact that we are here is the fact that no round owns a call**, which is exactly the
    /// condition under which a call with no result is a call nobody will answer. No clock, no
    /// thread introspection, no new bookkeeping.
    ///
    /// MEASURED, or this would not exist: a `grep` whose executor thread and process had both
    /// vanished left the operator's head showing `running` for eight minutes, with `esc esc` dead
    /// because the interrupt stops GENERATION and generation had already ended.
    pub fn sweep_abandoned_calls(&mut self) -> usize {
        // **The sweep is the thread that owns the harness's to run**, and that placement is the
        // liveness test: a round in flight holds the thread, so a sweep that runs at all is a
        // sweep at a moment when no round owns a call. Asking here is for the callers that drive
        // `Sessions` by hand.
        let mut settled = self.ask_courts(|s| s.sweep_abandoned_calls());
        self.sweep_at = None;
        let ids: Vec<String> = self.open.keys().cloned().collect();
        for id in ids {
            if let Some(h) = self.open.get_mut(&id) {
                settled += h.sweep_abandoned_calls();
            }
        }
        settled
    }

    /// **Close one session this worker holds: its harness, every per-session table, and its
    /// registry entry** (R68.2, R68.3).
    ///
    /// Nothing closed a session before this. `open` kept every harness the daemon ever built —
    /// transcript, ledger, `HostProcesses` and its job captures, a SQLite connection — and nine
    /// maps beside it kept an entry each (`slots`, `seated`, `fabric_seen`, `nags`, `bangs`,
    /// `armed`; the monitor-wake thread `armed` names, which now ends with the
    /// hub). The order is the harness's own shutdown order: its children are told to stop, its
    /// backend is closed (watchers first, then the host), the registry forgets the session and
    /// closes its hub — which every attached head and the monitor-wake thread see — and only then
    /// is the harness dropped.
    ///
    /// `false` when the worker did not hold the session; the registry entry is removed either way,
    /// so a hub whose open failed is closed through the same door.
    pub fn close(&mut self, session_id: &str) -> bool {
        // **What this session started goes with it** (RH7's supervisor): every process it owns,
        // on every host, ended and recorded — the session's cgroup is what would reap a local job
        // anyway, and a job on another machine has nothing else. A TURN ending does not come near
        // this: a job started to outlive its turn is what `background: true` means.
        let ended = crate::supervise::teardown_session(session_id, "the session closing");
        if ended > 0 {
            eprintln!("  {session_id} · closed; {ended} supervised process(es) ended with it");
        }
        // **And the blocks of ports it held go back** (RH9), with what it made under its prefixes
        // removed and nothing else. After the processes, because a role's containers are what its
        // processes started.
        if let Some(path) = &self.base.store {
            for line in crate::leases::release_session(path, session_id, "the session closing") {
                eprintln!("  {session_id} · lease: {line}");
            }
        }
        // **A session on its own thread is closed THERE**: the harness, its backend and its
        // children are on that thread, and closing them from here would be the second writer this
        // whole change exists to remove.
        if self.courts.contains_key(session_id) {
            let id = session_id.to_string();
            let closed = self
                .courts
                .get(session_id)
                .and_then(|court| court.ask(move |s: &mut Sessions<'static>| s.close(&id)));
            // The court's own close took the registry entry with it, which is what the caller's
            // `bool` means; a thread that is already gone answers nothing, and the entry goes the
            // same way here so a hub is never left open behind a session nobody serves.
            self.registry.remove(session_id);
            // **And its thread leaves before this returns.** Not for the answer — a close is not an
            // exit, and a thread that could not leave is not a crash waiting here — but because the
            // harness it is tearing down is the thing a later exit must not catch mid-flight
            // ([`COURT_TEARDOWN`]).
            if let Some(Court { jobs, thread, .. }) = self.courts.remove(session_id) {
                drop(jobs);
                if let Some(thread) = thread {
                    let _ = wait_for_threads(&[thread], COURT_TEARDOWN);
                }
            }
            return closed.unwrap_or(false);
        }
        let held = self.open.remove(session_id);
        if let Some(h) = &held {
            h.stop_children();
            h.close_backend();
        }
        self.slots.remove(session_id);
        self.seated.remove(session_id);
        self.fabric_seen.remove(session_id);
        self.nags.remove(session_id);
        self.bangs.remove(session_id);
        self.armed.remove(session_id);
        self.registry.remove(session_id);
        held.is_some()
    }

    /// **Arm the park clock for the subagents this worker holds**, a window from now — called at
    /// every turn's end, so the clock measures idleness. Nothing to arm when the policy is off.
    fn arm_park(&mut self) {
        let Some(window) = self.base.child_park_after else {
            return;
        };
        let at = Instant::now() + window;
        self.park_at = Some(self.park_at.map_or(at, |prev| prev.min(at)));
    }

    /// **Park every subagent this worker holds that has been idle for the window** — R68's
    /// idle-child policy (`Config::child_park_after`), on the worker's side. The thread-side half
    /// is `harness::serve_child`; this half exists because a child REOPENED from the store is
    /// held here, and without it the reopen would be a one-way door back into the leak.
    ///
    /// A child is parked when the hub has been quiet for the window (its last event, so a head's
    /// attach or a settlement counts), no head is attached, no turn is in flight, no plan-check
    /// is armed, no `!` run is in flight, and its own work is done (`Harness::park_refusal`).
    /// Root sessions are never parked here: they are the operator's conversations, and their
    /// lifetime is the operator's to end. Answers how many were parked.
    pub fn park_idle_children(&mut self) -> usize {
        let mut parked = self.ask_courts(|s| s.park_idle_children());
        let Some(window) = self.base.child_park_after else {
            self.park_at = None;
            return parked;
        };
        let now = Instant::now();
        if self.park_at.is_some_and(|at| at > now) {
            return parked;
        }
        self.park_at = None;
        let now_ms = letibot_sessionlog::registry::now_ms();
        let window_ms = window.as_millis() as u64;
        let children: Vec<String> = self
            .open
            .keys()
            .filter(|id| self.registry.parent_of(id).is_some())
            .cloned()
            .collect();
        let mut again: Option<Instant> = None;
        for id in children {
            let quiet_for = self.registry.get(&id).map(|h| {
                let st = h.status();
                let busy = st.heads > 0 || h.turn_in_flight();
                (busy, now_ms.saturating_sub(st.last_ms))
            });
            let eligible = match quiet_for {
                Some((false, quiet)) if quiet >= window_ms => {
                    self.nags.get(&id).and_then(|c| c.due_at()).is_none()
                        && !self.bang_run_in_flight(&id)
                        && self
                            .open
                            .get(&id)
                            .is_some_and(|h| h.park_refusal().is_none())
                }
                _ => false,
            };
            if eligible {
                self.close(&id);
                parked += 1;
                continue;
            }
            // Not yet: look again when its own window would end, or a window from now when it is
            // held by something other than time.
            let next = match quiet_for {
                Some((false, quiet)) if quiet < window_ms => {
                    now + Duration::from_millis(window_ms - quiet)
                }
                _ => now + window,
            };
            again = Some(again.map_or(next, |a| a.min(next)));
        }
        self.park_at = again;
        parked
    }

    /// **What keeps this daemon up**, or `None` when nothing does — the idle exit's test
    /// (`Config::idle_exit_after`), asked of each fact's owner rather than remembered.
    ///
    /// Every session in the registry: no head attached, no turn in flight. Every session with a
    /// harness: no subagent alive, no job running (`Harness::park_refusal`, the child-park rule's
    /// own test, so the two policies cannot disagree about what "its work is done" means), no
    /// monitor watching, no plan-check armed, no `!` run in flight. And the daemon's own: no flowy
    /// seat held (a seat is an inbox someone may write to), and no queue work it serves
    /// ([`crate::mergequeue::busy_here`]).
    pub fn idle_exit_refusal(&self) -> Option<String> {
        if self.seat.is_some() {
            return Some("it holds a flowy seat".into());
        }
        for id in self.registry.ids() {
            let Some(hub) = self.registry.get(&id) else {
                continue;
            };
            if hub.attached_heads() > 0 {
                return Some(format!("{id} has a head attached"));
            }
            if hub.turn_in_flight() {
                return Some(format!("{id} has a turn in flight"));
            }
        }
        for id in self.open.keys() {
            if let Some(why) = self.court_refusal(id) {
                return Some(why);
            }
        }
        // **And the sessions on their own threads, as they last reported themselves.** Read rather
        // than asked, because asking would mean waiting for a turn that may be minutes long — and
        // a turn in flight is already answered above, from the hub.
        for court in self.courts.values() {
            if let Some(why) = court
                .status
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
            {
                return Some(why);
            }
        }
        self.base
            .store
            .as_deref()
            .and_then(crate::mergequeue::busy_here)
    }

    /// The idle exit's window, `None` when this daemon never exits on its own.
    pub fn idle_window(&self) -> Option<Duration> {
        self.base.idle_exit_after
    }

    /// **Whether the daemon has been idle for its whole window** — and so should exit — spending
    /// the deadline as [`Sessions::reviews_due`] does. `false` before the deadline, and `false`
    /// with the clock re-armed whenever something still holds it: a look every
    /// [`IDLE_EXIT_RECHECK`] while it is busy, so the hour is counted from about when it last was
    /// rather than from when this pass happened to run.
    ///
    /// Idle means [`Sessions::idle_exit_refusal`] has nothing to say AND the window has passed
    /// since the later of the newest event in any session and the last pass that saw it busy.
    pub fn idle_exit_due(&mut self) -> bool {
        let Some(window) = self.base.idle_exit_after else {
            self.idle_exit_at = None;
            return false;
        };
        let now = Instant::now();
        if self.idle_exit_at.is_none_or(|at| at > now) {
            return false;
        }
        if self.idle_exit_refusal().is_some() {
            self.busy_seen = now;
            self.idle_exit_at = Some(now + IDLE_EXIT_RECHECK.min(window / 4));
            return false;
        }
        let now_ms = letibot_sessionlog::registry::now_ms();
        let newest_ms = self
            .registry
            .ids()
            .iter()
            .filter_map(|id| self.registry.get(id))
            .map(|h| h.status().last_ms)
            .max()
            .unwrap_or(0);
        let since_event = Duration::from_millis(now_ms.saturating_sub(newest_ms));
        match idle_left(window, since_event, now.duration_since(self.busy_seen)) {
            None => true,
            Some(left) => {
                self.idle_exit_at = Some(now + left);
                false
            }
        }
    }

    /// Send every check whose deadline has passed, and answer how many ran.
    ///
    /// A check is a TURN — the model reads the plan and can act on it — so it goes through the
    /// same tail every other turn does (`turn_tail::run_turn`), which is what re-arms or stands the clock
    /// down afterwards.
    ///
    /// **And it arms what the other two doors arm**, which it did not (R67 item 10). A nag turn
    /// dispatches tool calls exactly as a prompt's does, so it can strand one exactly as a
    /// prompt's does — and it runs on the idle arm right AFTER [`Sessions::sweep_abandoned_calls`]
    /// spent `sweep_at`, so a call it stranded was looked at by nothing: the worker went back to
    /// a wait whose only deadline was the nag's own repeat, minutes away, and that only for a plan
    /// still open. That is the wake arm's defect of 2026-10-10 (`daemon.rs`, the comment on
    /// [`SWEEP_AFTER`]) on the third door. So the sweep is armed before the turn, as the command
    /// and wake arms arm it, and the monitor wake after it, as `run_prompt` arms it: a monitor
    /// the nag turn declared is a monitor whose firing must wake the session.
    pub fn deliver_due_nags(&mut self) -> usize {
        let mut ran = self.ask_courts(|s| s.deliver_due_nags());
        let now = Instant::now();
        let due: Vec<String> = self
            .nags
            .iter_mut()
            .filter_map(|(id, clock)| clock.due_now(now).then(|| id.clone()))
            .collect();
        for session_id in due {
            // **A paused session's deadline is the pause's END, not a check** (`/nag pause 2h`):
            // the pause is lifted here, said on the hub, and the clock re-armed as if the operator
            // had typed `/nag resume` — no turn. A deadline that reaches a session still paused
            // (armed before the pause, or a wall clock a hair behind the monotonic one) runs
            // nothing either, and the re-arm puts the clock back on the pause's end.
            if let Some(h) = self.open.get_mut(&session_id)
                && let Some(p) = h.nag_pause()
            {
                if p.over_at(letibot_sessionlog::registry::now_ms()) {
                    // A row the store refused is said on the hub by `resume_nags` itself.
                    let _ = h.resume_nags(crate::nag_pause::Resumed::AtItsEnd);
                }
                self.rearm_todo_nag(&session_id);
                continue;
            }
            let notice = match self.open.get(&session_id) {
                // Recorded as the clock reads the plan, not as the wording sent
                // (`Harness::nag_said_as`, R77): the wording moves from one delivery to the next.
                Some(h) => h.nag_notice().map(|_| h.nag_said_as()),
                None => continue,
            };
            // Nothing to say after all — the plan moved between the arming and the clock — is no
            // turn, and a door that runs no turn owes no sweep.
            let Some(notice) = notice else {
                continue;
            };
            // **The shared tail arms the sweep and the wake, records the notice as said and
            // re-arms the clock** — what this door used to do by hand, and once did not (R67
            // item 10).
            match self.turn(&session_id, Door::Nag(notice), |h| h.nag_turn()) {
                None => {}
                Some(Ok(_)) => ran += 1,
                Some(Err(e)) => eprintln!("  {session_id} · todo check -> {e}"),
            }
        }
        ran
    }

    /// Read-only access to a session's harness, for a caller that wants the ledger
    /// or the prefix. `None` for a session nothing has run in yet.
    pub fn harness_of(&self, session_id: &str) -> Option<&Harness> {
        self.open.get(session_id)
    }

    /// Compact a session: the summary turn, then the transcript fork.
    ///
    /// The same shape as [`Sessions::submit`] — open-or-reuse the harness, run,
    /// publish a failure on the session's own log — because a compaction is a turn
    /// with a fork behind it, not a different kind of thing. No title derivation
    /// and no wake-arming: a summary proposes nothing and monitors do not fire on
    /// it.
    pub fn compact(&mut self, session_id: &str) -> Result<CompactReport, HarnessError> {
        // **A compaction is a turn**, so it runs where the harness is.
        if let Some(court) = self.court_serving(session_id) {
            let id = session_id.to_string();
            if let Some(out) = court.ask(move |s: &mut Sessions<'static>| s.compact(&id)) {
                return out;
            }
        }
        let hub = self.registry.get(session_id);
        let harness = self.harness(session_id)?;
        let (started, before) = (std::time::Instant::now(), harness.ledger_len() as u64);
        let out = harness.compact();
        crate::instruments::operator_compaction(harness, before, started, out.is_ok());
        match out {
            Ok(r) => {
                // The fabric may have moved since the session opened; after a
                // compaction is the moment to say so, as an update, never as a
                // rewrite of message 0.
                if let Err(e) = self.refresh_fabric(session_id)
                    && let Some(hub) = &hub
                {
                    hub.publish(SessionEvent::Warning {
                        code: "fabric_refresh_failed".into(),
                        detail: e.to_string(),

                        compaction: None,
                    });
                }
                Ok(r)
            }
            Err(e) => {
                let turn_id = self
                    .open
                    .get(session_id)
                    .map(|h| h.last_turn_id().to_string())
                    .unwrap_or_default();
                if let Some(hub) = &hub {
                    publish_failure(hub, &turn_id, &e);
                }
                Err(e)
            }
        }
    }

    /// **Arm the monitor wake for a session that has declared one.** T24's
    /// *"wakes the loop when it fires"*, which had no caller.
    ///
    /// # Why a thread, and why it is not a poll
    ///
    /// `Monitors::wait_for_any` is a `Condvar`: a waiter costs nothing while
    /// nothing is happening, and it returns the monitors that settled **during the
    /// call**, which is why it takes a cursor — a caller that asked twice would
    /// otherwise be handed the same firing twice and act on it twice. The thread
    /// blocks in it and rings the bell. `Registry::next_work` is already blocked on
    /// that bell, so the worker wakes on the firing itself rather than on a clock.
    ///
    /// The deadline the waiter passes is **not** message delivery — the condvar is.
    /// It is how a blocked thread notices the registry closing, because
    /// `Bell::close` notifies the bell's condvar and not the monitors'. That is a
    /// shutdown re-check, and it is worth naming because "no timer, no poll loop"
    /// is a property this daemon states about itself (§18.1-I12) and a re-check
    /// that went undescribed would read as a violation of it.
    ///
    /// # Why it is lazy
    ///
    /// **No monitors, no thread** — the same rule `Monitors`' own poller keeps, and
    /// the same rule T24 exists to enforce: this entry is about watchers that
    /// accumulate. A session with an exec backend and nothing watching gets no
    /// waiter; the first `monitor` call gets it one, and it lives until the daemon
    /// stops.
    ///
    /// **One waiter per name**, tracked in `armed`. Two waiters on one registry
    /// would each ring for the same firing, and the second wake would find the
    /// cursor already advanced and run nothing — a wasted `next_work` round rather
    /// than a duplicated turn, but it is the shape of defect this fleet has paid
    /// for and it is cheap to make unspellable.
    pub fn arm_wake(&mut self, session_id: &str) {
        let Some(mut h) = self.open.remove(session_id) else {
            return;
        };
        self.arm_wake_on(session_id, &mut h);
        self.open.insert(session_id.to_string(), h);
    }

    /// [`Sessions::arm_wake`] with the harness in hand — the shared tail's door. The thread itself
    /// is [`crate::turn_tail::spawn_monitor_wake`], the one a subagent's own thread starts too.
    fn arm_wake_on(&mut self, session_id: &str, h: &mut Harness) {
        if self.armed.contains(session_id) {
            return;
        }
        match crate::turn_tail::spawn_monitor_wake(&self.registry, h, session_id, SHUTDOWN_RECHECK)
        {
            Ok(true) => {
                self.armed.insert(session_id.to_string());
            }
            Ok(false) => {}
            // Not fatal and not silent. A session whose wake could not be armed still has
            // monitors and `job_list` still shows them; what it does not have is the wake, and
            // saying so is the difference between a poll and a poll nobody was told about.
            Err(e) => self.warn(
                session_id,
                "monitor_wake_not_armed",
                format!(
                    "this session's monitors will be POLLED, not woken: the waiter thread could \
                     not be started ({e}). A condition that fires between turns reaches the \
                     model only when something calls `job_list`."
                ),
            ),
        }
    }

    /// **Something fired while nothing was running.** The worker's half of the wake.
    ///
    /// `Outcome::Ignored` is the honest answer to a wake that raced a mid-turn
    /// pickup: the steering source and the harness share a cursor, so the firing
    /// had already reached the model and running a turn about it again would be
    /// telling the model the same thing twice.
    ///
    /// # A session the daemon does not HOLD is not a session nobody can serve
    ///
    /// This is R58's finding, and the correction is the operator's design in their own words:
    /// *"think about it like it is an erlang supervision tree. we talk to parents and they own
    /// lifecycle."* A subagent's harness is built inside the runner's thread and adopted into the
    /// `registry` — which is exactly what lets a head peek at it and attach to it — and it is
    /// **not** in `open`, so `Sessions::wake` cannot run its turn. What that used to mean was
    /// that a ring naming a child was a condition that fires and is discarded, which made depth
    /// 2 a hole: a grandchild's settlement reached the bell and stopped there.
    ///
    /// So the daemon HANDS IT ON. [`Hub::wake_its_own_reader`] wakes the hub's own condvar — the
    /// one the thread that runs that session is blocked in — and the wake is answered there by
    /// `harness::serve_child`, which calls [`crate::harness::Harness::wake`] exactly as this
    /// function does for a root. **Every session that can start a child is now a session the
    /// daemon can serve**; it is served by the thread that owns it, and the ring names the
    /// session that owns the settlement (its parent) rather than the tree's root.
    ///
    /// `Ignored` is left for what it honestly means: the session is gone from the registry
    /// altogether. Nothing is lost when that happens — a session that has closed handed its
    /// undrained settlements up a level first (`jobwatch::JobWatchers::stop`).
    pub fn wake(&mut self, session_id: &str) -> Outcome {
        // A session on its own thread is woken THERE. On this struct `open` does not hold it, so
        // `wake_route` already answers `ItsOwnReader` — this is only the spawn, for a session that
        // is open here and has no thread yet.
        self.spawn_court(session_id);
        let hub = self.registry.get(session_id);
        // **The decision is `wake_route`'s, so it can be asserted without a daemon.** The two
        // facts it reads are asked of the layers that own them: `open` is this struct's, the
        // registry is the daemon's.
        match wake_route(
            self.open.contains_key(session_id),
            hub.as_ref().map(|h| h.owner()),
        ) {
            WakeRoute::Drive => {}
            WakeRoute::ItsOwnReader => {
                let handed = hub.map(|h| h.wake_its_own_reader()).unwrap_or(false);
                return if handed {
                    Outcome::HandedOn
                } else {
                    Outcome::Ignored
                };
            }
            WakeRoute::Gone => return Outcome::Ignored,
        }
        // **A wake's turn is a turn** — it appends rows, it can reach the wall, and a session that
        // only ever woke would have compacted never — so it takes the shared tail like every
        // other door: the sweep and the monitor wake are armed by it too, which this door used
        // to leave to its caller or to nobody (R68.4). `None` is a wake with nothing to say:
        // nothing ran, nothing grew, and there is nothing to tidy after it.
        match self.turn(session_id, Door::Wake, |h| h.wake()) {
            None => Outcome::Ignored,
            Some(Ok(reply)) => Outcome::Replied(Box::new(reply)),
            Some(Err(e)) => Outcome::Failed(e.to_string()),
        }
    }

    // ---------------------------------------------------------------------------
    // The operator's own run, off the worker. See `crate::bangrun` for the shape;
    // these are the worker's hands on the state it owns.
    // ---------------------------------------------------------------------------

    /// **A failure on the session's own log** — the hub every attached head draws — for the
    /// arms whose only other witness is the daemon's stderr.
    ///
    /// R67 item 15: an `Outcome::Failed` from `dispatch` reaches `on_reply`, and `on_reply` is an
    /// `eprintln!` (`cli.rs`). So an operator's `!` line whose rows could not be recorded, a door
    /// call that failed, a session that was not open — each was a sentence in a log nobody at the
    /// head reads, and the head that asked saw nothing at all. The stderr line stays (the
    /// caller's, or `on_reply`'s); this is the half that reaches the person.
    fn warn(&self, session_id: &str, code: &str, detail: String) {
        if let Some(hub) = self.registry.get(session_id) {
            hub.publish(SessionEvent::Warning {
                code: code.to_string(),
                detail,
                compaction: None,
            });
        }
    }

    /// **The operator's run could not have a thread of its own, so it runs on the worker** —
    /// the daemon's log line, and now the session's: while it runs, every other session's work
    /// waits behind it, which is the cost the person at the head is paying and should be told.
    fn said_on_the_worker(&self, session_id: &str, line: &str, why: &str) {
        eprintln!("  {session_id} · ! run on the worker instead of its own thread: {why}");
        self.warn(
            session_id,
            "operator_shell_on_worker",
            format!(
                "`{line}` runs on the daemon's one worker instead of a thread of its own ({why}), \
                 so every other session's work waits until it ends."
            ),
        );
    }

    /// **The session was not open, said where the asker reads** — the sentence `Outcome::Failed`
    /// carries, published as `session_unavailable` (the store-family code for *this session could
    /// not be used*) and handed back for the outcome.
    fn not_open(&self, session_id: &str, what: &str) -> String {
        let said = format!("session {session_id} is not open");
        self.warn(
            session_id,
            "session_unavailable",
            format!("{what} was not run: {said} in this daemon."),
        );
        said
    }

    /// The serialization state for `session_id`'s operator runs — created with the
    /// harness (both open paths attach the same `Arc` to it), so a session this map
    /// holds always has one.
    fn bang_state(&self, session_id: &str) -> Option<Arc<crate::bangrun::State>> {
        self.bangs.get(session_id).cloned()
    }

    /// Whether an operator run for this session is in flight on its own thread.
    fn bang_run_in_flight(&self, session_id: &str) -> bool {
        self.bang_state(session_id).is_some_and(|s| s.in_flight())
    }

    /// Park a line typed while a run was in flight; it starts when that run settles.
    /// Only called after [`Self::bang_run_in_flight`] answered true, so the state is
    /// there to park into — and both run on the worker's thread, so nothing can move
    /// between the two reads.
    fn queue_bang_line(&self, session_id: &str, line: String, who: String) {
        if let Some(s) = self.bang_state(session_id) {
            s.queue(line, who);
        }
    }

    /// **The run settled** — clear the flag first, then take what waited with
    /// [`Self::pop_queued_bang_line`], one line per settle.
    fn clear_bang_in_flight(&self, session_id: &str) {
        if let Some(s) = self.bang_state(session_id) {
            s.end();
        }
    }

    /// The next line that waited for the run in flight, in submission order.
    fn pop_queued_bang_line(&self, session_id: &str) -> Option<(String, String)> {
        self.bang_state(session_id).and_then(|s| s.pop_pending())
    }

    /// **Hand the operator's run to a thread of its own.**
    ///
    /// Mints the call (the `bang-<n>` id is this session's counter, incremented HERE,
    /// in submission order — a queued line started later must not take an earlier
    /// number), marks the run in flight BEFORE the thread exists — so two lines cannot
    /// race the spawn — and spawns it. `Err` only when the thread could not be started
    /// at all; the flag is undone first, because a run that never started must not
    /// block every line behind it.
    fn start_operator_run(
        &mut self,
        session_id: &str,
        line: &str,
        who: &str,
    ) -> Result<(), String> {
        let state = self
            .bang_state(session_id)
            .ok_or_else(|| "no operator-run state for this session".to_string())?;
        let hub = self
            .registry
            .get(session_id)
            .ok_or_else(|| "no hub for this session".to_string())?;
        let (call_id, call, runtime) = {
            let h = self
                .open
                .get_mut(session_id)
                .ok_or_else(|| format!("session {session_id} is not open"))?;
            let (call_id, call) = h.prepare_operator_shell(line)?;
            (call_id, call, h.runtime_handle())
        };
        state.start();
        if let Err(why) = crate::bangrun::spawn_run(
            session_id.to_string(),
            call_id,
            call,
            line.to_string(),
            who.to_string(),
            runtime,
            hub,
        ) {
            state.end();
            return Err(why);
        }
        Ok(())
    }

    /// **The old arm, kept for the case that cannot hand off**: the thread could not be
    /// started, so the run happens here, on the worker, exactly as it always did — the
    /// rows, then the turn its rows are for. This is the behaviour whose cost the whole
    /// change exists to end; it survives only as the fallback that keeps the operator's
    /// line when a spawn fails.
    fn run_operator_shell_here(&mut self, session_id: &str, line: &str, who: &str) -> Outcome {
        let ran = match self.open.get_mut(session_id) {
            Some(h) => h.run_operator_shell(line, who),
            None => Err(self.not_open(session_id, &format!("`{line}`"))),
        };
        if let Err(e) = ran {
            self.warn(
                session_id,
                "operator_shell_failed",
                format!(
                    "`{who}` ran `{line}` from their own console and it could not be recorded: \
                     {e}. The command may have run; what is missing is the row the next turn \
                     would have read."
                ),
            );
            return Outcome::Failed(e);
        }
        // **And the turn its rows are for** — the same sending half the settle arm runs,
        // because the operator asked for their command's result to reach the model. Through the
        // shared tail, which this door used to run without the wake or the sweep (R68.4).
        if !self.open.contains_key(session_id) {
            return Outcome::Failed(self.not_open(session_id, "the turn after your `!` line"));
        }
        match self.turn(session_id, Door::OperatorShell, |h| {
            h.run_after_operator_shell().map(Some)
        }) {
            None => Outcome::Ignored,
            Some(Ok(reply)) => Outcome::Replied(Box::new(reply)),
            Some(Err(e)) => Outcome::Failed(e.to_string()),
        }
    }

    /// **The court that serves this session, if one does** — giving it one first when the session
    /// is open and has none.
    ///
    /// A session opened by a door older than the threads (`harness_as` reached from somewhere
    /// other than `open`) has no court; without this it would go on running its turns on the
    /// daemon's worker for ever, which is the defect these threads exist to remove.
    fn court_serving(&mut self, session_id: &str) -> Option<&Court> {
        if !self.courts.contains_key(session_id) {
            self.spawn_court(session_id);
        }
        self.courts.get(session_id)
    }

    /// **Ask every session on its own thread the same question, and add up the answers.**
    ///
    /// The shape a per-session count has once the sessions are no longer in THIS struct: the
    /// sweep, the plan check and the child park each walk `self.open` in one session's body — and
    /// on a court that `open` holds exactly that one session, so the same body answers for it.
    ///
    /// `Copy` because every court is asked the same question, and a closure a loop may hand out
    /// more than once is the only way to say that.
    fn ask_courts(
        &self,
        f: impl Fn(&mut Sessions<'static>) -> usize + Send + Sync + Copy + 'static,
    ) -> usize {
        let mut total = 0;
        for id in self.courts.keys() {
            if let Some(court) = self.courts.get(id) {
                total += court.ask(f).unwrap_or(0);
            }
        }
        total
    }

    /// **Put a command back on the queue of the thread that owns the session — unless this IS
    /// that thread.** The two arms that use it are the two the worker may not answer itself: an
    /// `Interrupt` and a `Message` for a session served on its own thread.
    ///
    /// Both readers take from ONE queue (`Hub::take_own_work` for the thread that runs the
    /// session, `Hub::try_command` for the worker), so the worker of a daemon that has armed
    /// [`Sessions::serve_each_on_its_own_thread`] can lose a race it should not answer: the
    /// interrupt a parent aimed at its child, and the message a peer sent it. So it is handed
    /// back, and the thread that holds the harness takes it.
    ///
    /// **And that thread must not hand anything back to itself.** It is the same hub, the same
    /// queue and the same `Owner::OwnThread`, so a court that reached this would put the command
    /// it just took back where it took it from, ring its own condvar, and take it again — a
    /// resume that never runs anything, one core per session, for ever. MEASURED 2026-10-26 on
    /// the operator's box: seven daemons at a full core each after the bridge's `leticode_stop`
    /// queued an `Interrupt` on a session that had a thread of its own, with the CPU equal to
    /// their age. `crates/bridge/tests/no_spin.rs` is the measurement.
    ///
    /// The test for *is this reader the one that owns the session* is `open`, which is the same
    /// fact [`wake_route`] decides a wake with: a court holds its session's harness exactly as
    /// the worker holds the ones it serves, so *held here* IS *served here*.
    fn hand_back_to_the_owner(
        &self,
        session_id: &str,
        hub: &Option<Arc<letibot_sessionlog::hub::Hub>>,
        cmd: &QueuedCommand,
    ) -> bool {
        let Some(hub) = hub else {
            return false;
        };
        if hub.owner() != Owner::OwnThread || self.open.contains_key(session_id) {
            return false;
        }
        hub.give_back_to_its_own_reader(cmd.clone())
    }

    /// Run one command against its session.
    pub fn dispatch(&mut self, session_id: &str, cmd: &QueuedCommand) -> Outcome {
        // **A session served on its own thread runs its own commands.** The daemon's worker never
        // takes one off the hub any more (`Hub::try_command` answers `None` for a hub owned by its
        // own thread), so this arm is for the callers that reach `dispatch` directly — a test, and
        // a daemon that never armed the threads. Asking and waiting is right for both: neither is
        // the loop that a turn must not stand still in front of.
        if let Some(court) = self.court_serving(session_id) {
            let id = session_id.to_string();
            let cmd = cmd.clone();
            if let Some(outcome) = court.ask(move |s: &mut Sessions<'static>| s.dispatch(&id, &cmd))
            {
                return outcome;
            }
        }
        let hub = self.registry.get(session_id);
        match &cmd.kind {
            CommandKind::Prompt { text, attachments } => {
                // One line, because everything a turn needs around it — the title,
                // the wake, the failure notice, the compaction at the wall — is in
                // `run_prompt`, which the scripted path also calls. This arm used
                // to be the second copy of that sequence, and the copy was missing
                // the compaction.
                match self.run_prompt(session_id, text, attachments) {
                    Ok(reply) => Outcome::Replied(Box::new(reply)),
                    Err(e) => Outcome::Failed(e.to_string()),
                }
            }
            // A compaction is a whole-session act and runs through
            // [`Sessions::compact`], as a prompt runs through
            // [`Sessions::submit`] — which also opens its harness, so nothing here
            // holds one.
            CommandKind::Compact => {
                // **Measured before and after, as the automatic door is** (R72 follow-up,
                // defect B). This arm read the session's pair AFTER the fork and converted
                // both sides through it — the history's ratio, unrolled since the last
                // round, applied to the base too — and named no unit, so a manual `/compact`
                // on a scratch deepseek session told the head *"compacted: 20999 → 20755
                // tokens"* while the automatic one, one door over, said the same fact in
                // deepseek tokens with each side converted through its own pair. Now the
                // count it compacted FROM is rolled before the fork, the base's after it
                // (`Scales`), and the closing line is `compacted_line`, the automatic one's.
                let before = self.harness(session_id).ok().map(|h| {
                    let now = h.measure_resident();
                    let c = h.config();
                    // `Config::scale`, not the measurement alone: on a session whose switch
                    // carried a ratio, `now.tokens` is the converted count and a line that
                    // converted the same history with nothing would print bytes beside it.
                    (now.tokens, c.scale(), h.token_unit(), c.shown_wall())
                });
                match self.compact(session_id) {
                    Ok(r) => {
                        // **A compaction that re-seated says so.** It forks onto the
                        // prompt this daemon seats now, so a conversation older than a
                        // tool picks that tool up by compacting — and a capability
                        // that appears silently is one nobody uses.
                        if let Some(hub) = &hub
                            && !(r.gained.is_empty() && r.lost.is_empty())
                        {
                            let mut said =
                                "this compaction also re-seated: the new prompt announces the \
                             tools this daemon seats now"
                                    .to_string();
                            if !r.gained.is_empty() {
                                said.push_str(&format!(
                                    ". The model can now call: {}",
                                    r.gained.join(", ")
                                ));
                            }
                            if !r.lost.is_empty() {
                                said.push_str(&format!(". It has lost: {}", r.lost.join(", ")));
                            }
                            said.push_str(
                                ". `/reseat` would have cost a second summary turn and a second \
                             cold prefill; this one is already paid for.",
                            );
                            hub.publish(SessionEvent::Warning {
                                code: "reseated".into(),
                                detail: said,

                                compaction: None,
                            });
                        }
                        let base = self
                            .open
                            .get_mut(session_id)
                            .map(|h| (h.measure_resident(), h.config().scale(), h.is_metered()));
                        let (resident, scale, unit, (window, headroom)) =
                            before.unwrap_or((0, None, "tokens".into(), (0, 0)));
                        let said = match &base {
                            Some((b, _, metered)) => {
                                crate::harness::compacted_line(resident, *b, &unit, *metered)
                            }
                            None => format!("compacted: {resident} {unit} resident before."),
                        };
                        eprintln!("  {session_id}: {said}");
                        if let Some(hub) = &hub {
                            let scales = Scales {
                                before: scale,
                                after: base.as_ref().and_then(|b| b.1).or(scale),
                                unit: Some(unit),
                                estimated: base.as_ref().is_some_and(|(b, _, metered)| {
                                    *metered && b.measured.is_none()
                                }),
                            };
                            hub.publish(SessionEvent::Warning {
                                code: "compacted".into(),
                                detail: compaction_said(&r, scales.clone()),
                                compaction: Some(Box::new(compaction_wire(
                                    &r,
                                    "compacted",
                                    scales,
                                    resident,
                                    window,
                                    headroom,
                                ))),
                            });
                        }
                        Outcome::Compacted(Box::new(r))
                    }
                    Err(e) => Outcome::Failed(e.to_string()),
                }
            }
            // A re-seat is a compaction that lands on a different prompt, so it
            // runs through the same door and reports through the same one. What it
            // adds is the sentence naming which tools the model can call now that
            // it could not before — the reason anybody types this.
            CommandKind::Reseat { summarise } => {
                // Two ways to change message zero, and the difference is what
                // happens to everything under it: `reseat` pays a summary turn,
                // `reingest` carries the conversation across and pays a cold
                // prefill instead.
                let summarise = *summarise;
                let out = match self.harness(session_id) {
                    Ok(h) => {
                        if summarise {
                            h.reseat()
                        } else {
                            h.reingest()
                        }
                    }
                    Err(e) => Err(e),
                };
                match out {
                    Ok(r) => {
                        // Composed while the report is whole; said after the head's row.
                        let to_the_model = crate::harness::reseated_for_the_model(summarise, &r);
                        if let Some(hub) = &hub {
                            let mut said = if summarise {
                                format!(
                                    "this conversation now speaks the prompt this daemon \
                                     seats: {} tokens of history replaced by a {}-token \
                                     summary, and the tool list rebuilt",
                                    r.fork.was_tokens, r.fork.base_tokens
                                )
                            } else {
                                format!(
                                    "this conversation now speaks the prompt this daemon \
                                     seats: the tool list was rebuilt and all {} tokens of \
                                     history carried across as they are — nothing \
                                     summarised, nothing dropped",
                                    r.fork.base_tokens
                                )
                            };
                            let gained = r.gained.clone();
                            let lost = r.lost.clone();
                            if !gained.is_empty() {
                                said.push_str(&format!(
                                    ". The model can now call: {}",
                                    gained.join(", ")
                                ));
                            }
                            if !lost.is_empty() {
                                said.push_str(&format!(". It has lost: {}", lost.join(", ")));
                            }
                            if !r.grew.is_empty() {
                                said.push_str(&format!(
                                    ". Seated on both sides with a schema that changed, which \
                                     the model now reads: {}",
                                    r.grew.join(", ")
                                ));
                            }
                            said.push_str(if summarise {
                                ". The server's cache for the new prompt is cold, so the \
                                 next turn prefills from scratch — that is what changing \
                                 the announced tools costs."
                            } else {
                                ". The server's cache for the new prompt is cold, so the \
                                 next turn prefills the WHOLE conversation from scratch — \
                                 that is what keeping it costs. `/reseat summarise` is the \
                                 cheaper, lossy way."
                            });
                            // **A summarising re-seat is a compaction**, so it carries
                            // the structure too: the record it produced is the base
                            // every later turn reads, and a head drawing a row for the
                            // tool-list change should be able to draw the same row it
                            // draws for `/compact`. A bare re-seat is not a compaction —
                            // nothing was summarised — and carries none, which is why
                            // ``because`` is empty rather than ``local_model``.
                            let compaction = summarise.then(|| {
                                let c = self
                                    .harness_of(session_id)
                                    .map(|h| h.config().clone())
                                    .unwrap_or_else(|| self.base.clone());
                                let resident = c.shown_tokens(r.fork.was_tokens as u64);
                                Box::new(compaction_wire(
                                    &crate::harness::CompactReport {
                                        fork: r.fork.clone(),
                                        summary_turn: r.summary_turn.clone(),
                                        gained: Vec::new(),
                                        lost: Vec::new(),
                                        summary_was_streamed: true,
                                    },
                                    "reseated",
                                    // `Config::scale` and not the measurement alone: the
                                    // pair the numbers above were converted with is the
                                    // pair this line has to convert with, carried or not.
                                    c.scale(),
                                    resident,
                                    c.shown_tokens(c.planning_window().unwrap_or(0)),
                                    c.shown_tokens(c.headroom()),
                                ))
                            });
                            hub.publish(SessionEvent::Warning {
                                code: "reseated".into(),
                                detail: said,
                                compaction,
                            });
                        }
                        // **And the model is told, in a turn of its own.** The operator:
                        // *"since reseated nag reaches the model and it starts nagging me - the
                        // result - that reseat happened shall reach the model too. and it should
                        // start the turn"*. The stale-prompt notice reaches the model through the
                        // plan-check's turn (`nag_notice_naming`) — then telling it to keep
                        // asking until the person acted, now once and as information, since the
                        // head's reseat card holds the decision — and the person's act reached
                        // only the head. This
                        // is the symmetric half, through the same shared turn tail every door
                        // uses. A re-seat runs on the worker's queue, never inside a turn, so
                        // this IS the next step boundary.
                        //
                        // The re-seat stands whatever the turn does: its report is returned
                        // either way, and a turn that fails says so through the tail's own
                        // `turn_failed` path.
                        if let Some(Err(e)) = self.turn(session_id, Door::Reseat, |h| {
                            h.run_after_reseat(&to_the_model).map(Some)
                        }) {
                            eprintln!(
                                "  {session_id}: the turn telling the model about its re-seat \
                                 failed: {e}"
                            );
                        }
                        Outcome::Compacted(Box::new(crate::harness::CompactReport {
                            fork: r.fork,
                            summary_turn: r.summary_turn,
                            gained: r.gained,
                            lost: r.lost,
                            // A re-seat runs the ordinary compaction, which streams.
                            summary_was_streamed: true,
                        }))
                    }
                    Err(e) => {
                        if let Some(hub) = &hub {
                            hub.publish(SessionEvent::Warning {
                                code: "reseat_refused".into(),
                                detail: e.to_string(),

                                compaction: None,
                            });
                        }
                        Outcome::Failed(e.to_string())
                    }
                }
            }
            CommandKind::Interrupt { reason } => {
                // **A stop delivered to a session the daemon does not hold belongs to the thread
                // that owns it.** Both readers take from ONE queue (`Hub::take_own_work` for the
                // thread that runs the session, `Hub::try_command` for the worker), so the worker
                // can win an interrupt a parent aimed at its child — and its answer would be
                // *"nothing was generating"*, which for a subagent that is mid-turn in its own
                // thread is false. This is not about `Prompt`, which the daemon CAN serve by
                // opening the session lazily: an interrupt has nothing to act on unless the turn
                // already running is reached, and the daemon cannot reach one it does not hold.
                // So it is put back for that thread, which is the same door a relayed wake takes —
                // and NOT when this reader is that thread, which is the spin
                // [`Sessions::hand_back_to_the_owner`] exists for.
                if self.hand_back_to_the_owner(session_id, &hub, cmd) {
                    return Outcome::HandedOn;
                }
                // **AND THE SUBTREE GOES FIRST, whichever session this is.** A stop is a stop
                // whether or not a turn is running: the children this session owns are stopped
                // before it, so nothing is left computing for nobody — the operator's design in
                // one line, *"we talk to parents and they own lifecycle"*.
                if let Some(h) = self.open.get_mut(session_id) {
                    h.stop_children();
                }
                if let Some(hub) = &hub {
                    hub.publish(SessionEvent::Warning {
                        code: "interrupt_idle".into(),
                        detail: format!(
                            "interrupt ({reason}) arrived between turns; nothing was generating"
                        ),

                        compaction: None,
                    });
                }
                Outcome::Ignored
            }
            // A take-back reaching the worker means the turn ended before the
            // steering poll saw it: whatever prompts it named have already run
            // as their own turns or are about to, and are no longer the
            // operator's to take back. Quietly nothing — the head cleared its
            // own echo when it recalled the line, and a warning here would be
            // noise about a no-op.
            // **The operator's todos onto the board — ONE LIST, TWO AUTHORS.**
            //
            // The operator's ruling: *"the existing getter should return mine and yours, and the
            // rest is also the same. the only difference is who created and that is it."* So this
            // is not a second list on the daemon side: it is the other half of the SAME board, and
            // `TodoBoard::snapshot` is their union. Everything that reads the board then sees the
            // operator's rows for free — the pane's `Todos` reply, the prompt the model is sent, and
            // the idle NAG (`Harness::nag_notice` → `unfinished_plan`), which is the whole reason
            // this exists: until now the nag could only ever fire for work the MODEL had written.
            CommandKind::SetOperatorTodos { items, moved } => {
                let converted: Vec<letibot_tokencore::store::TodoItem> = items
                    .iter()
                    .map(|e| letibot_tokencore::store::TodoItem {
                        content: e.content.clone(),
                        // **The wire's condition, as the STORE spells it** — the other half of
                        // `todo_entry`'s conversion, and a `match` for the same reason: the two
                        // crates own the type separately, so a new variant must be named here.
                        when: e.when.as_ref().map(|c| match c {
                            letibot_sessionlog::event::TodoCondition::Job { handle } => {
                                letibot_tokencore::store::TodoCondition::Job {
                                    handle: handle.clone(),
                                }
                            }
                        }),
                        status: status_from_the_wire(e.status),
                        by: letibot_tokencore::store::TodoBy::Operator,
                        // **The edges now CROSS this door rather than dying at it.** It used to
                        // say *"the wire carries no edges yet, so an operator's row arrives with
                        // none"* — true while `TodoEntry` had no `needs`, and a hole the moment a
                        // row could have one: `set_operator` REPLACES this half, so a row the
                        // operator owns would lose every edge it had on the head's next push.
                        // `TodoEntry::needs` is that field, added and defaulted, and this is the
                        // `match` that makes a kind added on one side a compile error on the
                        // other.
                        needs: e
                            .needs
                            .iter()
                            .map(|need| match need {
                                letibot_sessionlog::event::TodoNeed::Row { content } => {
                                    letibot_tokencore::store::TodoNeed::Row {
                                        content: content.clone(),
                                    }
                                }
                                letibot_sessionlog::event::TodoNeed::Child { id } => {
                                    letibot_tokencore::store::TodoNeed::Child { id: id.clone() }
                                }
                                // **The job handle crosses verbatim**, the same rule the child's
                                // id and a row's words keep: it is what `job_output` and
                                // `job_kill` are given, and a shortened one is a name nothing
                                // answers to.
                                letibot_sessionlog::event::TodoNeed::Job { handle } => {
                                    letibot_tokencore::store::TodoNeed::Job {
                                        handle: handle.clone(),
                                    }
                                }
                                letibot_sessionlog::event::TodoNeed::Monitor { name } => {
                                    letibot_tokencore::store::TodoNeed::Monitor {
                                        name: name.clone(),
                                    }
                                }
                                // **The operator's question, and their answer, cross whole** — the
                                // head echoes its own rows back on every push, and a question it
                                // dropped here would be a person no longer waited on for nothing.
                                letibot_sessionlog::event::TodoNeed::Operator {
                                    question,
                                    options,
                                } => letibot_tokencore::store::TodoNeed::Operator {
                                    question: question.clone(),
                                    options: options.clone(),
                                },
                                letibot_sessionlog::event::TodoNeed::Answered {
                                    question,
                                    reply,
                                } => letibot_tokencore::store::TodoNeed::Answered {
                                    question: question.clone(),
                                    reply: reply.clone(),
                                },
                                letibot_sessionlog::event::TodoNeed::Unknown => {
                                    letibot_tokencore::store::TodoNeed::Unknown
                                }
                            })
                            .collect(),
                    })
                    .collect();
                // The head is the source of truth for its own rows, so its list REPLACES that half.
                let n = converted.len();
                // **And the state moves it asked for on rows that are not its own** — `/todo
                // postpone|resume <the row's words>` on a row the MODEL wrote. The words are the
                // key: the board resolves them against the model's half and sets the status, which
                // is the one thing the operator can change about a row they did not write.
                let states: Vec<(String, letibot_tokencore::store::TodoStatus)> = moved
                    .iter()
                    .map(|s| (s.content.clone(), status_from_the_wire(s.status)))
                    .collect();
                // **The board before the write**, so an ADD can be told from a re-push (R77): the
                // head sends its whole half on every add, delete and HELLO.
                let before = self
                    .open
                    .get(session_id)
                    .map(|h| h.todo_list())
                    .unwrap_or_default();
                if let Some(h) = self.open.get_mut(session_id) {
                    h.set_operator_todos(converted, &states);
                }
                let _ = n;
                // **ARMING HERE IS THE WHOLE POINT OF A ROW THE OPERATOR ADDS.** The idle clock used
                // to start in exactly one place — the end of a turn (the turn tail) — so a row added
                // to an IDLE session had no clock at all and was never checked: the model is quiet,
                // the operator has just said what they want done, and nothing ever reminds it. The
                // operator found it the direct way: *"my todos will be reminded to a model?"*, and
                // on a resumed session, *"this is exactly what i see with rano"*.
                //
                // It is the same arm the turn end makes, deliberately: a board that MOVED is a board
                // worth checking, and `rearm_todo_nag` already decides — an unchanged plan is not
                // re-armed, so a head that re-pushes the same rows (any add, any delete, every
                // HELLO) does not become a metronome.
                //
                // **And an ADD is more than a moved board** (R77): the operator has said what they
                // want done, so the check is earned back and comes due at the shorter window —
                // `TodoNagClock::operator_added`. A re-push is the ordinary arm above.
                let after = self
                    .open
                    .get(session_id)
                    .map(|h| h.todo_list())
                    .unwrap_or_default();
                if the_operator_added(&before, &after) {
                    self.rearm_todo_nag_after_an_operator_add(session_id);
                } else {
                    self.rearm_todo_nag(session_id);
                }
                Outcome::Ignored
            }
            CommandKind::WithdrawPrompts => Outcome::Ignored,
            // **The operator answered a row's question, and the session is between turns** — so
            // the answer is applied and the model is told AT ONCE, as a turn of its own (R79: an
            // operator's words are never held). The turn is the operator's door (`Door::Prompt`):
            // it is them speaking, and it earns the plan-check back like any word of theirs. A
            // refusal — the row moved on, the model detached the question, an option the row never
            // offered — runs nothing and is said to the heads.
            CommandKind::AnswerTodo {
                content,
                question,
                answer,
            } => {
                let settled = match self.harness(session_id) {
                    Ok(h) => h.answer_operator_ask(content, question, answer),
                    Err(e) => Err(e.to_string()),
                };
                match settled {
                    Ok(line) => {
                        match self.turn(session_id, Door::Prompt, |h| h.submit(&line).map(Some)) {
                            None => Outcome::Ignored,
                            Some(Ok(reply)) => Outcome::Replied(Box::new(reply)),
                            Some(Err(e)) => Outcome::Failed(e.to_string()),
                        }
                    }
                    Err(why) => {
                        self.warn(
                            session_id,
                            "todo_answer_refused",
                            format!("your answer on `{}` was not taken: {why}", content.trim()),
                        );
                        Outcome::Ignored
                    }
                }
            }
            // The request is honoured mid-turn by the `bash` wait loop, which reads
            // the hub's promote channel. Reaching here means nothing was running, so
            // the request is stale — clear it and say so, rather than leaving it for
            // the next command to promote itself unprompted.
            // **The operator's own call, admitted before it runs** — R24 part two.
            //
            // The admission is written here, by the worker, through the same corpus sink the
            // gate writes every other decision through. The allowlist was already enforced on
            // the connection's thread (a name outside it never reaches this queue), so what is
            // left is the record and the permission to go.
            CommandKind::OperatorCall {
                call_id,
                name,
                arguments,
                who,
                execute,
            } => {
                let call_id = call_id.clone();
                let name = name.clone();
                let arguments = arguments.clone();
                let who = who.clone();
                let admitted = match self.open.get_mut(session_id) {
                    Some(h) => h.admit_operator_call(&call_id, &name, &arguments, &who),
                    None => Err(self.not_open(session_id, &format!("`{name}`"))),
                };
                match admitted {
                    Ok(()) => {
                        // **Pending until the result arrives**, keyed by the HEAD that asked —
                        // so a head that dies between the two frames leaves a sentence rather
                        // than a silent admission. See `Hub::detach`.
                        //
                        // A daemon-run call is noted too and then cleared by the run below,
                        // rather than being a special case: the pending set means *an
                        // admission this daemon is holding*, and a call the daemon runs and
                        // finishes in the same arm has one for the length of that arm.
                        if let Some(hub) = &hub {
                            hub.note_operator_call(&call_id, &cmd.head_id, &name, &who);
                        }
                        // **R31: the daemon runs it, when the head asked it to.** The head
                        // that asked has no tool runtime and no HTTP client; the daemon has
                        // both, and — the reason that is not merely convenient — the payload
                        // then comes from *this* program, bounded by this session's byte
                        // caps and spill policy. A head fetching a page itself would write a
                        // corpus row saying the operator ran `web_fetch` about another
                        // program's answer.
                        if *execute {
                            let said = match self.open.get_mut(session_id) {
                                Some(h) => h.run_operator_call(&call_id, &name, &arguments, &who),
                                None => Err(self.not_open(session_id, &format!("`{name}`"))),
                            };
                            if let Some(hub) = &hub {
                                hub.take_operator_call(&call_id);
                            }
                            if let Err(e) = said {
                                self.warn(
                                    session_id,
                                    "operator_call_failed",
                                    format!(
                                        "`{who}`'s `{name}` (call `{call_id}`) was admitted and the \
                                         daemon could not run it: {e}"
                                    ),
                                );
                                return Outcome::Failed(e);
                            }
                        }
                        Outcome::Ignored
                    }
                    Err(e) => {
                        self.warn(
                            session_id,
                            "operator_call_failed",
                            format!(
                                "`{who}`'s `{name}` (call `{call_id}`) was not admitted, so \
                                 nothing ran: {e}"
                            ),
                        );
                        Outcome::Failed(e)
                    }
                }
            }
            // **The operator's shell line, run by this daemon and recorded as two rows**
            // (`! ls .` → the operator's own `User` row + a `bash` `ToolResult` with
            // `origin: Operator`). Between turns it runs on a thread of its own (see
            // `crate::bangrun`) — a run that held THIS worker held every other session's
            // work with it, which is the defect this arm exists to end. Mid-turn the round
            // boundary takes it (`apply_queued_head_run`), which is the door's own timing
            // and for its own reason: a turn already running reads the rows at its next
            // round, and that pickup stays synchronous.
            CommandKind::OperatorShell { line, who } => {
                let (line, who) = (line.clone(), who.clone());
                if !self.open.contains_key(session_id) {
                    return Outcome::Failed(self.not_open(session_id, &format!("`{line}`")));
                }
                // **One operator run per session at a time** — `Prompts` holds ONE input
                // handle, so a second concurrent run would steal the first's card and
                // `!send`. A line typed while one is in flight waits HERE, on worker-owned
                // state (nothing else touches this map), and starts when the in-flight one
                // settles. The worker does not wait with it: that is the whole point.
                if self.bang_run_in_flight(session_id) {
                    self.queue_bang_line(session_id, line, who);
                    return Outcome::HandedOn;
                }
                match self.start_operator_run(session_id, &line, &who) {
                    Ok(()) => Outcome::HandedOn,
                    // **The thread could not be started.** The command is not refused for
                    // that: it runs HERE, synchronously, exactly as this arm always did —
                    // the old behaviour, with the reason on the log rather than a silent
                    // loss of the operator's line.
                    Err(why) => {
                        self.said_on_the_worker(session_id, &line, &why);
                        self.run_operator_shell_here(session_id, &line, &who)
                    }
                }
            }
            // **What the operator's run produced, back from its own thread.** The rows and
            // the turn are appended HERE, by the worker — one writer of the transcript —
            // exactly as they always were; only the wait moved off this thread. See
            // `crate::bangrun` for the handoff's shape.
            CommandKind::OperatorShellResult {
                line,
                who,
                call_id,
                outcome,
                payload,
                spill,
            } => {
                let (line, who, call_id) = (line.clone(), who.clone(), call_id.clone());
                let settled = match self.open.get_mut(session_id) {
                    Some(h) => h.settle_operator_shell(
                        &line,
                        &who,
                        &call_id,
                        outcome.clone(),
                        payload.clone(),
                        spill.as_ref().map(|s| (s.full_bytes, s.hash.clone())),
                    ),
                    None => Err(self.not_open(session_id, &format!("the rows of `{line}`"))),
                };
                // **The run is over, whether or not its rows could be written** — so the flag
                // comes down FIRST, before anything here can return. R67 item 11: this used to
                // sit after an early `return Outcome::Failed` for a settle that failed, so one
                // failed settle left the session's run in flight for ever and every later `!`
                // line was parked behind a run that had already ended — `!` dead for the
                // session's life, with the reason on stderr only.
                self.clear_bang_in_flight(session_id);
                if let Err(e) = &settled {
                    self.warn(
                        session_id,
                        "operator_shell_failed",
                        format!(
                            "`{who}` ran `{line}` from their own console and it could not be \
                             recorded: {e}. The command ran; what is missing is the row the next \
                             turn would have read, so no turn was started for it. Your next `!` \
                             line runs as usual."
                        ),
                    );
                }
                // **The next queued line starts now** — not when some later command happens to
                // come through, and not never because this one's rows failed. One per settle:
                // the one started here settles in its own turn, and drains the next.
                let next = self.pop_queued_bang_line(session_id);
                if let Some((next_line, next_who)) = next {
                    if let Err(why) = self.start_operator_run(session_id, &next_line, &next_who) {
                        self.said_on_the_worker(session_id, &next_line, &why);
                        // Same fallback as the dispatch arm above, and for the same
                        // reason: a queued line is not lost because a thread could not
                        // be started. It runs here, between turns, where it would have
                        // waited anyway.
                        let ran = self.run_operator_shell_here(session_id, &next_line, &next_who);
                        if let Outcome::Failed(e) = ran {
                            return Outcome::Failed(e);
                        }
                    }
                }
                if let Err(e) = settled {
                    return Outcome::Failed(e);
                }
                // **And the turn its rows are for** — the operator's correction, in their
                // words: *"my commands should start a turn and should be printed to me"*.
                // The rows above are the printing; this is the turn. `Ignored` here was
                // the whole defect: the deposit sat in the transcript until something else
                // started a turn, which is a command nobody answered.
                if !self.open.contains_key(session_id) {
                    return Outcome::Failed(
                        self.not_open(session_id, "the turn after your `!` line"),
                    );
                }
                // A `!` turn is a turn: it appends rows, it can reach the wall, and a
                // session that only ever ran shell lines would have compacted never — the
                // shared tail every door takes (R68.4).
                match self.turn(session_id, Door::OperatorShell, |h| {
                    h.run_after_operator_shell().map(Some)
                }) {
                    None => Outcome::Ignored,
                    Some(Ok(reply)) => Outcome::Replied(Box::new(reply)),
                    Some(Err(e)) => Outcome::Failed(e.to_string()),
                }
            }
            // **What it produced.** The row goes in with its `origin` set, so every head
            // draws it as the person's act and the model sees the result.
            CommandKind::OperatorResult {
                call_id,
                outcome,
                payload,
            } => {
                let call_id = call_id.clone();
                let outcome = outcome.clone();
                let payload = payload.clone();
                // The pending entry is the admission's other half: a `call_id` this daemon
                // never admitted is refused by name rather than appended, because a row with
                // no admission behind it is a row nothing can be checked against.
                let pending = match &hub {
                    Some(h) => h.take_operator_call(&call_id),
                    None => None,
                };
                let Some((name, who)) = pending else {
                    return Outcome::Failed(format!(
                        "no admitted operator call `{call_id}` is pending in this session, so \
                         its result was not appended. A row whose admission is missing is a \
                         row nothing stands behind."
                    ));
                };
                let appended = match self.open.get_mut(session_id) {
                    Some(h) => h
                        .finish_operator_call(&call_id, &name, &who, outcome, &payload)
                        .map_err(|e| e.to_string()),
                    None => Err(format!("session {session_id} is not open")),
                };
                match appended {
                    Ok(()) => Outcome::Ignored,
                    Err(e) => Outcome::Failed(e),
                }
            }
            // **A message for a session the daemon does not hold belongs to the thread that
            // owns it** — the same rule, and the same door, as the interrupt arm above. Since
            // the between-turns fix a message to a child is no longer *"a turn in flight or
            // nothing"*: the child's own reader is woken by the sender's `task_message` and runs
            // a turn for it (`harness::serve_child`'s `ChildCommand::Hear`). So a worker that ran
            // this arm's sentence instead would be both the second writer this tree keeps closing
            // and a false report — the reader IS there, and it will read it.
            //
            // **And a message for a session the daemon DOES hold is this arm's own work.** That
            // is the lateral channel's whole delivery problem: the main session is `open`, so it
            // has no reader of its own to hand anything back to, and until this branch existed a
            // message addressed to it skipped the hand-back, fell through to `message_idle`
            // (*"that session is gone, so nothing will deliver it"*) and was dropped — in exactly
            // the case the design needs, a session sending another session something.
            CommandKind::Message { from, text } => {
                if self.hand_back_to_the_owner(session_id, &hub, cmd) {
                    return Outcome::HandedOn;
                }
                // **The session the WORKER owns: a row the agent reads, and the turn it makes.**
                // The operator's ruling for their own lines is the rule here too — *"my commands
                // should start a turn and should be printed to me"* — and a message is that pair
                // of acts: the deposit is a row on the session's own log, and the turn is what
                // makes it an utterance somebody answered. A session whose turn is already
                // RUNNING never reaches this arm: its own steering poll takes the message at the
                // next round boundary (`HubSteering`'s `Message` arm), so there is one writer and
                // one text on both doors.
                //
                // **And a session the worker OWNS but has not opened is opened for it** (R68.9) —
                // the lazy open a prompt already gets. A hub whose `Work::Open` failed has no
                // other reader, so the message is the worker's to deliver or to refuse by name:
                // the open is retried here, and if it fails again the sentence below is true.
                if !self.open.contains_key(session_id)
                    && hub
                        .as_ref()
                        .is_some_and(|h| h.owner() == Owner::Worker && !h.is_closed())
                    && let Err(e) = self.harness(session_id)
                {
                    self.warn(
                        session_id,
                        "session_unavailable",
                        format!(
                            "a message from `{from}` arrived and this session could not be \
                             opened to hear it: {e}"
                        ),
                    );
                }
                if self.open.contains_key(session_id) {
                    return match self.run_a_message_from_a_session(session_id, from, text) {
                        Ok(reply) => Outcome::Replied(Box::new(reply)),
                        Err(e) => Outcome::Failed(e.to_string()),
                    };
                }
                // What is left is the case neither door can reach: a session that is not held
                // and has no reader — its hub is closed, or there is none — where the sentence
                // below is true.
                if let Some(hub) = &hub {
                    let said = match text.chars().count() > 200 {
                        true => format!("{}…", text.chars().take(200).collect::<String>()),
                        false => text.clone(),
                    };
                    hub.publish(SessionEvent::Warning {
                        code: "message_idle".into(),
                        detail: format!(
                            "a message from `{from}` could not be delivered: `{session_id}` is \
                             gone, so nothing will read it — the daemon holds no session of that \
                             id and the hub it had is closed: {said}. The sender's `task_message` \
                             was accepted and did not land."
                        ),
                        compaction: None,
                    });
                }
                Outcome::Ignored
            }
            CommandKind::Promote => {
                if let Some(hub) = &hub {
                    // **A run of the operator's is a call in flight that this worker is
                    // NOT holding.** It runs on a thread of its own (`crate::bangrun`),
                    // and that thread's `bash` wait loop — not this arm — is the
                    // designed consumer of the promote flag: it is the thing that
                    // knows the job id and can move the cgroup. Taking the flag here
                    // was the operator's report, sentence for sentence — *"ctrl-o
                    // printed \"background requested\" and didnt background it"*: the
                    // request arrived, this arm ate it between turns because the
                    // WORKER was between turns, and the run it was aimed at kept
                    // sleeping in the foreground with nothing left to read. So the
                    // flag stays for the run's own loop (it polls every half second),
                    // and the sentence says where the request went rather than the
                    // false "nothing was running to move".
                    //
                    // The one race — the run ending in the window after this check but
                    // before its loop's last poll — is closed at the run's end:
                    // `run_and_hand_back` takes whatever flag survives it, so a
                    // request can never sit stale for the next unrelated foreground
                    // call to eat.
                    if self.bang_run_in_flight(session_id) {
                        hub.publish(SessionEvent::Warning {
                            code: "promote_in_flight".into(),
                            detail: "a background request arrived while the operator's \
                                     run is in flight on its own thread; its wait loop \
                                     takes it and moves the run"
                                .into(),

                            compaction: None,
                        });
                        return Outcome::Ignored;
                    }
                    // **`None` means a wait loop already honoured it.** A promote sent
                    // during a turn is taken by that turn's own `bash` wait loop, which
                    // promotes the call and settles it as `Backgrounded`; this command
                    // then reaches the worker only after the turn has ended. Warning
                    // "nothing was running to move" there is the same lie with the
                    // sign flipped — a promotion that happened, reported as one that
                    // could not.
                    if hub.take_promote_request().is_some() {
                        hub.publish(SessionEvent::Warning {
                            code: "promote_idle".into(),
                            detail: "a background request arrived between turns; nothing \
                                     was running to move"
                                .into(),

                            compaction: None,
                        });
                    }
                }
                Outcome::Ignored
            }
            // **An answer that got this far had nowhere better to go.** A session
            // with an adjudicator installs an `AnswerSink`, and `Hub::submit` then
            // delivers straight to the thread waiting for it rather than queueing —
            // see `crate::answers` for why the queue is a deadlock. So reaching this
            // arm means one of two things, and the message says which rather than
            // repeating a sentence about M1 that stopped being true when roles became
            // reachable.
            CommandKind::Answer { req_id, .. } => {
                if let Some(hub) = &hub {
                    let detail = match hub.answer_sink_describes() {
                        Some(who) => format!(
                            "an answer to {req_id} reached the command queue even though \
                             this session can be answered ({who}). That is a defect: the \
                             answer was not delivered to whatever asked, and the decision \
                             is still open or has already timed out. Nothing was run."
                        ),
                        None => format!(
                            "an answer to {req_id} arrived and this session has nothing \
                             that asks: its tools are read-only, and a read never prompts \
                             (clause 4). Nothing was waiting for it and nothing was run."
                        ),
                    };
                    hub.publish(SessionEvent::Warning {
                        code: "answer_unclaimed".into(),
                        detail,

                        compaction: None,
                    });
                }
                Outcome::Ignored
            }
            CommandKind::Slash { line } => {
                // **A `/clear` aimed at a session its own thread owns is that thread's** — the
                // interrupt arm's rule and door. Both readers take from one queue, and a worker that
                // answered it would open a SECOND harness for a subagent whose real one is parked in
                // `serve_child`, then fork the transcript under it. The child answers its own clear
                // (`ChildCommand::Clear`), with the harness that holds its board, jobs and children.
                //
                // **And `/nag`, for the same reason**: a subagent's clock lives on the thread that
                // serves it, and this worker holds neither that clock nor that harness
                // (`ChildCommand::Nag`).
                if matches!(
                    crate::slash::Slash::parse(line),
                    crate::slash::Slash::Clear { .. } | crate::slash::Slash::Nag(_)
                ) && let Some(hub) = &hub
                    && hub.owner() == Owner::OwnThread
                    && hub.give_back_to_its_own_reader(cmd.clone())
                {
                    return Outcome::HandedOn;
                }
                let line = line.clone();
                let reply = self.slash(session_id, &line);
                if let Some(hub) = &hub {
                    hub.publish(SessionEvent::Warning {
                        code: if reply.ok {
                            "slash".into()
                        } else {
                            "slash_refused".into()
                        },
                        detail: format!("/{line}\n{}", reply.lines.join("\n")),

                        compaction: None,
                    });
                }
                if reply.ok {
                    Outcome::Ignored
                } else {
                    Outcome::Failed(format!("/{line} was refused"))
                }
            }
            // **A read of a job's output, for the pane that asked.** The same read
            // `/job ID` does, answered with the offsets **beside** the text instead
            // of folded into a footer sentence — the jobs pane draws its own header
            // and its own paging. Published on the log like every other verb's
            // reply, so both heads see the window the operator opened and neither
            // has to be the one that asked.
            CommandKind::ReadJobOutput { job, offset } => {
                let job = job.clone();
                let offset = *offset;
                let window = match self.harness_of(session_id) {
                    Some(h) => h.job_output_window(&job, offset, crate::harness::JOB_OUTPUT_WINDOW),
                    None => Err(format!("session {session_id} is not open")),
                };
                if let Some(hub) = &hub {
                    match window {
                        Ok(w) => hub.publish(w.into_event(&job)),
                        // A refused read is a `Warning`, the same as a refused slash,
                        // and the code says which read it was: the pane renders it as
                        // a line rather than a window.
                        Err(e) => hub.publish(SessionEvent::Warning {
                            code: "job_output_refused".into(),
                            detail: e,

                            compaction: None,
                        }),
                    };
                }
                Outcome::Ignored
            }
            // **A head ASKS for the point that needs a person** (`PROTOCOL_VERSION` 54, B8), and
            // **a person ANSWERS it**. Two arms rather than one because they come from different
            // heads and mean different things: the ask is a program asking, and the consent is
            // somebody saying yes about their own box. The policy — does this point need anybody,
            // is there anybody to ask, was this head among the ones asked — is the harness's, in
            // `Harness::ask_mode_confirm` and `Harness::answer_mode_confirm`, because the open
            // request lives there and the supplies that decide *needs a person* are its facts.
            //
            // **A refusal goes back to the head that asked, and to no other.** `Rejected` carries
            // no addressee, so it is written on that head's own stream (`Hub::push_frame_to`); the
            // sentence also lands on the session's log, where the operator can see that a package
            // was asking for something and could not be answered.
            CommandKind::ModeConfirm { name } => {
                let said = match self.harness(session_id) {
                    Ok(h) => h.ask_mode_confirm(
                        name,
                        &cmd.head_id,
                        &cmd.identity,
                        &cmd.client_request_id,
                    ),
                    // **A session that cannot be opened is a session that cannot be asked
                    // about**, and the words the open failed with are the answer.
                    Err(e) => Err(e.to_string()),
                };
                match said {
                    Ok(()) => Outcome::Ignored,
                    Err(why) => {
                        if let Some(hub) = &hub {
                            hub.push_frame_to(
                                &cmd.head_id,
                                letibot_sessionlog::protocol::ServerFrame::Rejected {
                                    client_request_id: cmd.client_request_id.clone(),
                                    reason: why.clone(),
                                    expected_seq: 0,
                                    actual_seq: hub.head_seq(),
                                },
                            );
                        }
                        self.warn(session_id, "mode_confirm_refused", why.clone());
                        Outcome::Failed(why)
                    }
                }
            }
            CommandKind::ModeConsent { req_id, yes } => {
                let said = match self.harness(session_id) {
                    Ok(h) => h.answer_mode_confirm(req_id, *yes, &cmd.head_id, &cmd.identity),
                    Err(e) => Err(e.to_string()),
                };
                match said {
                    Ok(()) => Outcome::Ignored,
                    Err(why) => {
                        if let Some(hub) = &hub {
                            hub.push_frame_to(
                                &cmd.head_id,
                                letibot_sessionlog::protocol::ServerFrame::Rejected {
                                    client_request_id: cmd.client_request_id.clone(),
                                    reason: why.clone(),
                                    expected_seq: 0,
                                    actual_seq: hub.head_seq(),
                                },
                            );
                        }
                        self.warn(session_id, "mode_consent_refused", why.clone());
                        Outcome::Failed(why)
                    }
                }
            }
            CommandKind::Mode { name, consented } => {
                let mode = match letibot_tools::mode::Mode::parse(name) {
                    Ok(m) => m,
                    Err(e) => {
                        if let Some(hub) = &hub {
                            hub.publish(SessionEvent::Warning {
                                code: "mode_unknown".into(),
                                detail: e,

                                compaction: None,
                            });
                        }
                        return Outcome::Failed("unknown mode".into());
                    }
                };
                // The project root this session is confined to, read off the harness
                // (the session's own, not the daemon's start directory), then cloned so
                // the harness borrow ends before the store write below.
                let workspace = {
                    let Some(harness) = self.harness_of(session_id) else {
                        return Outcome::Failed(format!("session {session_id} is not open"));
                    };
                    harness.workspace().to_path_buf()
                };
                // **A consented `allow-all` IS written now — with the answer that opens it.**
                //
                // It used to be left out of the row, and for a good reason: a row naming
                // `allow-all` refuses at every later open on a bare host, and leticl died
                // exactly that way on 2026-09-20 — the row was written, every subagent then
                // died at open with *"a confinement for exec"*, and the operator saw
                // *"subagents dont work"*. What changed is that the operator's ANSWER now
                // travels with the row as its third field: a later open reading `allow-all`
                // AND a stamp resolves to `ALLOW_ALL_HERE` — the same coordinate with the
                // boundary told truthfully — instead of refusing, and the startup says whose
                // answer it was and when. A row naming `allow-all` with NO stamp still
                // refuses exactly as it did, and that is what keeps the leticl failure from
                // coming back: the stamp is only ever written by a person answering.
                let consented_here =
                    *consented && mode.name == letibot_tools::mode::Mode::ALLOW_ALL.name;
                // **The row, and then the answer** — in that order, through one lock, so a
                // reader of the file never sees a consent for a row that is not there. `set`
                // itself forgets a stamp when a row moves off `allow-all`, so putting a project
                // back to another point is a real revocation, and `/mode allow-all` again
                // cannot resurrect an older `yes`.
                if let Err(e) = (|| -> std::io::Result<()> {
                    let mut store = self.parts.mode_store.write().unwrap();
                    store.set(&workspace, mode)?;
                    if consented_here {
                        store.set_consent(&workspace, &letibot_flowy::seat::rfc3339_now())?;
                    }
                    Ok(())
                })() {
                    if let Some(hub) = &hub {
                        hub.publish(SessionEvent::Warning {
                            code: "mode_unpersisted".into(),
                            detail: format!(
                                "could not record `{}` for {}: {e}",
                                mode.name,
                                workspace.display()
                            ),

                            compaction: None,
                        });
                    }
                    return Outcome::Failed(format!("persisting mode: {e}"));
                }
                // **And this session moves too.** The row above is the project's
                // point for every session that opens later; this is the one in
                // front of the operator, which used to be told it would keep the
                // point it opened at. True, and asked about three times, because
                // nobody typing `/mode automode` means "next time". The gate reads
                // its mode at decision time, so the move is cheap; what it has to
                // get right is in `Harness::set_mode` — the prerequisite check the
                // open would have run, the grants taken under the old point, and
                // the guard model for a point whose decider it is.
                //
                // A refusal keeps the SESSION where it was and says why; the project
                // row stays written, because the next session may well be able to
                // carry the point this one cannot (a `coder` with no shell cannot go
                // to `allow-all`; a daemon started with `--bash` can).
                let moved = match self.open.get_mut(session_id) {
                    // The operator's answer travels with the command, and only
                    // this call reads it: the project row above is written from
                    // the NAME, which stays `allow-all` and stays refused for the
                    // next daemon. Consent is for the session in front of them.
                    // **Which point it actually landed on**, not which one was
                    // asked for. A consented `allow-all` becomes
                    // `allow-all (this box, consented)`, and the sentence below
                    // used to name the requested one and print ITS summary — so a
                    // bare host was told "the VM is the boundary and nothing inside
                    // it reaches this box", two lines above the same card saying
                    // the confinement prerequisite refuses that point here.
                    Some(h) => h
                        .set_mode_consented(mode, *consented)
                        .map(|said| (said, h.config().mode)),
                    None => Err("this session has no harness open".into()),
                };
                // **And the session remembers where it was put** — its own row, so a restart or
                // a reseat brings it back here whatever the project's row says by then. The
                // requested point's NAME with the answer beside it, as the project row is
                // written: a consented `allow-all` comes back as `allow-all (this box, consented)`.
                if moved.is_ok() {
                    let when = consented_here.then(letibot_flowy::seat::rfc3339_now);
                    let store = self.parts.mode_store.read().unwrap();
                    if let Err(e) = store.set_session(session_id, mode, when.as_deref())
                        && let Some(hub) = &hub
                    {
                        hub.publish(SessionEvent::Warning {
                            code: "mode_unpersisted".into(),
                            detail: format!(
                                "could not record `{}` for this session: {e}",
                                mode.name
                            ),
                            compaction: None,
                        });
                    }
                }
                if let Some(hub) = &hub {
                    let _ = match moved {
                        Ok((said, applied)) => hub.publish(SessionEvent::Warning {
                            code: "mode_set".into(),
                            // **Say what is true of the project separately from what
                            // is true of this session**, because they can differ: a point
                            // the row carries may still be one this session cannot move
                            // to. **Ask the store what the row says rather than assuming
                            // it** — `set` has already run above, so this is the file's own
                            // answer and the one a person reading the row would get. The
                            // consented `allow-all` needs no branch of its own any more:
                            // the row IS written, with the stamp beside it, and the applied
                            // point's own summary says what the confirmation covers.
                            detail: {
                                let row = self
                                    .parts
                                    .mode_store
                                    .read()
                                    .ok()
                                    .map(|st| st.for_project(&workspace).name)
                                    .unwrap_or(mode.name);
                                format!(
                                    "{said}. {} is at `{row}` from every later session too. {}",
                                    workspace.display(),
                                    applied.summary
                                )
                            },

                            compaction: None,
                        }),
                        Err(why) => hub.publish(SessionEvent::Warning {
                            code: "mode_set_next_session_only".into(),
                            detail: format!(
                                "{} is set to `{}` for every LATER session in this \
                                 project; this one stays where it opened, because it \
                                 cannot carry that point: {why}",
                                workspace.display(),
                                mode.name
                            ),

                            compaction: None,
                        }),
                    };
                }
                Outcome::Ignored
            }
        }
    }
}

/// **The daemon's half of the shared turn tail** (R68.4) — the [`TurnOwner`] for a session the
/// worker holds. Each method is one step of [`crate::turn_tail::run_turn`]'s sequence, landing
/// where the daemon keeps that step's state; the ORDER is the tail's.
struct DaemonTurn<'s, 'a, 'id> {
    sessions: &'s mut Sessions<'a>,
    session_id: &'id str,
}

impl TurnOwner for DaemonTurn<'_, '_, '_> {
    /// Before the turn, as the command and wake arms of `Daemon::run` used to do at their call
    /// sites and the nag door did by hand (R67 item 10): a turn can strand a call, and only an
    /// idle pass closes it out.
    fn arm_sweep(&mut self) {
        self.sessions.arm_sweep(SWEEP_AFTER);
    }

    /// The operator (or a peer) has spoken: the plan-check is earned back.
    fn spoken_to(&mut self, _h: &mut Harness) {
        self.sessions.note_operator_prompt(self.session_id);
    }

    /// **A compaction forked the transcript, so a seated root is told the fabric again** when it
    /// changed — what `Sessions::compact` does after a `/compact`, for the compactions the tail
    /// runs at the wall.
    fn compacted(&mut self, h: &mut Harness) {
        let Some(block) = self.sessions.fabric_due(self.session_id) else {
            return;
        };
        match h.system_update(&block) {
            Ok(_) => self.sessions.fabric_saw(self.session_id, block),
            Err(e) => self
                .sessions
                .warn(self.session_id, "fabric_refresh_failed", e.to_string()),
        }
    }

    fn title(&mut self, _h: &mut Harness, title: String) {
        self.sessions.name_session(self.session_id, title);
    }

    fn arm_wake(&mut self, h: &mut Harness) {
        self.sessions.arm_wake_on(self.session_id, h);
    }

    /// **Every turn ends here, so this is where the idle clocks start** — the plan-check (with a
    /// spent nag recorded as said first: the metronome rule) and, for a subagent the worker
    /// holds, its park window.
    fn turn_ended(&mut self, h: &mut Harness, door: &Door) {
        if let Door::Nag(notice) = door
            && let Some(clock) = self.sessions.nags.get_mut(self.session_id)
        {
            clock.said(notice.clone(), Instant::now());
        }
        self.sessions.rearm_todo_nag_on(self.session_id, h);
        if self.sessions.registry.parent_of(self.session_id).is_some() {
            self.sessions.arm_park();
        }
    }
}

/// The store, as somewhere a registry can find sessions it is not holding.
///
/// Its **own** connection, behind a `Mutex`. Not the `Harness`'s: `rusqlite`'s
/// `Connection` is `Send` and not `Sync`, the registry is shared across every
/// connection thread, and a head listing sessions must not be able to block a turn
/// that is writing rows. SQLite in WAL mode is built for exactly this — one writer,
/// many readers — so a second connection is the cheap answer rather than a
/// compromise.
pub struct StoreSessions {
    store: Mutex<Store>,
    wiring: SessionWiring,
}

impl StoreSessions {
    /// `None` when the daemon has no store: there is then nothing on disk to list,
    /// and a source that answered "no sessions" would be indistinguishable from a
    /// store that is empty.
    pub fn open(cfg: &Config) -> Option<Arc<StoreSessions>> {
        let path = cfg.store.as_ref()?;
        let store = Store::open(path).ok()?;
        Some(Arc::new(StoreSessions {
            store: Mutex::new(store),
            wiring: Sessions::wiring(cfg),
        }))
    }
}

/// **The same second connection, answering a row the view has trimmed** (R19.2b).
///
/// One instance does both jobs because they are one store asked two questions, and
/// because a third connection would be a third thing to keep in WAL's reader set. The
/// daemon passes this same `Arc` to `Registry::set_source` and `Registry::set_row_source`.
/// **R11's locator, against the store** — leticl's ask, second half.
///
/// The same shape as [`RowSource`]: one implementation over the daemon's second connection,
/// set once at startup, and a registry with none answers `None` exactly as it did before.
impl letibot_sessionlog::registry::DiagnosticSource for StoreSessions {
    fn diagnostic(
        &self,
        request_id: &str,
        kind: letibot_sessionlog::protocol::DiagnosticKind,
    ) -> Option<String> {
        // The wire name and the column name are two vocabularies on purpose: the first is what
        // a head asks for, the second is what the store has always called it. The map lives
        // here, in the one place that knows both, rather than being flattened into either.
        let column = match kind {
            letibot_sessionlog::protocol::DiagnosticKind::Brief => "brief",
            letibot_sessionlog::protocol::DiagnosticKind::Reply => "reply",
        };
        let store = self.store.lock().ok()?;
        store.diagnostic(request_id, column).ok().flatten()
    }
}

impl letibot_sessionlog::registry::RowSource for StoreSessions {
    fn row_body(&self, session_id: &str, row: usize) -> Option<String> {
        // A session is not going to have four billion rows; a `usize` that does not fit is
        // a request for something that cannot exist, and `None` is the honest answer.
        let seq = u32::try_from(row).ok()?;
        let store = self.store.lock().ok()?;
        // **The ordinal is relative to the CURRENT transcript.** After a fork — a
        // compaction, a reseat — ordinal 3 is a row of the new base, so reading the old
        // transcript's row 3 would answer with a row nobody asked about.
        let transcript_id = store.current_transcript_id(session_id).ok()??;
        let json = store.row_json_at(&transcript_id, seq).ok()??;
        // The row's own type decides what its body is, and that match lives once — in
        // `letibot_sessionlog::body_of`, which the view's own reader calls too. Two copies
        // is how the two tiers would come to disagree about what a row's `body` is.
        let item: letibot_transcript::TranscriptItem = serde_json::from_str(&json).ok()?;
        Some(letibot_sessionlog::body_of(&item))
    }
}

/// **Ledger tokens as the operator's screen counts them.**
///
/// One function for the two callers that render a compaction — the sentence and the
/// structure beside it — because two copies of this arithmetic is how a head comes to
/// print two different numbers for one quantity. `None` scale is the ledger's own figure,
/// which is the right answer on a local endpoint and the only one available before a
/// metered turn has measured the ratio.
fn shown_tokens(scale: Option<(u64, u64)>) -> impl Fn(usize) -> u64 {
    move |ledger: usize| -> u64 {
        let n = ledger as u64;
        match scale {
            Some((l, p)) if l > 0 && p > 0 => ((n as u128 * p as u128) / l as u128) as u64,
            _ => n,
        }
    }
}

/// **The two pairs a compaction's numbers convert through** — the history's, and the
/// new base's (R72).
///
/// One pair used to convert both, and it was the HISTORY's: the ratio of a
/// conversation three quarters reasoning, applied to a summary and a verbatim tail
/// whose share was nothing like it. `Harness::measure_resident` re-bases on the fork,
/// so the caller that has run it passes the base's own pair as `after`; a caller that
/// has one pair passes it and both sides read it, which is the old behaviour exactly.
///
/// `unit` is what the converted numbers are in, named in the sentence (R72: every
/// printed count names its unit) — `Harness::token_unit`; `None` says `tokens`, which
/// is what a caller with no session to ask has always printed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Scales {
    pub before: Option<(u64, u64)>,
    pub after: Option<(u64, u64)>,
    pub unit: Option<String>,
    /// **The after-count is an estimate** — a metered base the provider has not counted
    /// yet. Said as `≈ N, estimated until the next round measures it`; see
    /// `crate::wall` for what judging it as a measurement cost (pg-noop, 2026-10-11).
    pub estimated: bool,
}

impl From<Option<(u64, u64)>> for Scales {
    fn from(scale: Option<(u64, u64)>) -> Self {
        Scales {
            before: scale,
            after: scale,
            unit: None,
            estimated: false,
        }
    }
}

/// **How the summary turn was paid for, said only with what was measured** (R72
/// follow-up, defect B).
///
/// `reusable` is the floor the prefix invariant proved the server could reuse, and it is
/// ZERO whenever the invariant was not checked — a metered provider (it is sent messages,
/// not token arrays, so `prefix::check` skips) or the first turn on a transcript. The log
/// line printed the two side by side regardless: MEASURED on a scratch deepseek session,
/// *"summary turn reused 19584 of 0 carryable tokens"* — DeepSeek's own cache count over a
/// bound nobody computed, which reads as a numerator larger than its denominator. With
/// no bound, the cache count is said as the server's own figure and the missing bound as
/// missing.
pub(crate) fn summary_turn_reuse(o: &letibot_turn::CompactionOutcome) -> String {
    match (o.cached_tokens, o.reusable) {
        (cached, reusable) if reusable > 0 => {
            format!("summary turn reused {cached} of {reusable} carryable tokens")
        }
        (0, _) => "summary turn reused nothing the server reported".into(),
        (cached, _) => format!(
            "summary turn: the server reported {cached} tokens from its cache; the prefix \
             invariant was not checked on this turn, so there is no carryable bound to set \
             it against"
        ),
    }
}

/// **What a compaction tells the head.**
///
/// The result used to go to the daemon's stderr and nowhere else. On the
/// ordinary path that was invisible, because the summary turn runs through the
/// session's own sink and every head watches the text being written. The overrun
/// paths cannot — they summarise a SCRATCH transcript through a `NullSink` on
/// purpose — so they streamed nothing, and a `/compact` that folded 1.5M tokens
/// correctly looked from the head like it had stopped partway. The operator,
/// 2026-09-20: *"started summarization of the first 1500+ and then scrolled some
/// s-tasks and that is it, not summary output, nothing"*.
///
/// So: always the numbers, and the summary itself exactly when nobody saw it
/// written. Printing it on the streamed path too would be the same text twice.
/// `scale` is the session's `ledger_scale` — **the session's, not the daemon's**.
/// `Sessions::base` is the command line, where it is always `None`, so reading it
/// there would convert nothing and quietly print the ledger's figures again.
///
/// The sentence is shared by the two doors that publish it —
/// [`Sessions::compact_if_at_the_wall`] for the daemon's own sessions and
/// [`Harness::submit_as_a_normal_session`] for a subagent's — because a second
/// copy of the wording would drift from the first exactly the way the seam
/// itself did.
pub(crate) fn compaction_said(
    r: &crate::harness::CompactReport,
    scales: impl Into<Scales>,
) -> String {
    let scales = scales.into();
    let shown = shown_tokens(scales.before);
    let shown_after = shown_tokens(scales.after);
    let unit = scales.unit.as_deref().unwrap_or("tokens");
    // **An empty summary is a re-ingest**, which is the same signal
    // `fork_to_summary` reads to decide what note to write. Nothing was
    // summarised, so "compacted" would be a lie and `was → base` would be one
    // number printed twice.
    if r.summary_turn.summary.is_empty() {
        return format!(
            "re-seated: {} {unit} of conversation carried onto the new prompt as they are, \
             on transcript {}. Nothing was summarised and nothing was dropped.",
            shown_after(r.fork.base_tokens),
            r.fork.transcript_id
        );
    }
    // In the units the operator's header shows; see `Config::shown_tokens`. This
    // line said `1352917 → 11353` beside a header reading 900k, and the operator
    // read the first number as a lie rather than as the other unit.
    let mut said = if scales.estimated {
        format!(
            "compacted: {} → ≈ {} {unit}, estimated until the next round measures it, on \
             transcript {}.",
            shown(r.fork.was_tokens),
            shown_after(r.fork.base_tokens),
            r.fork.transcript_id
        )
    } else {
        format!(
            "compacted: {} → {} {unit}, on transcript {}.",
            shown(r.fork.was_tokens),
            shown_after(r.fork.base_tokens),
            r.fork.transcript_id
        )
    };
    // **What was carried rather than described.** Zero on a local model, and a
    // non-zero count on a remote one, so the sentence is evidence of which path
    // ran rather than a restatement of the policy (R27).
    if r.fork.tail_items > 0 {
        said.push_str(&format!(
            " {} item(s) of the most recent exchange(s) were carried over verbatim \
             rather than summarised.",
            r.fork.tail_items
        ));
    }
    if let Some(dropped) = r.fork.tail_dropped {
        said.push_str(&format!(
            " The verbatim part begins inside an exchange — {dropped} item(s) of it are \
             gone — so its first message answers something that is no longer there."
        ));
    }
    // **And WHICH zero, when there is no tail.** This comment two paragraphs up claims the
    // non-zero count is *"evidence of which path ran rather than a restatement of the policy
    // (R27)"* — and that was true in one direction only: zero printed nothing, so policy and
    // accident were one appearance, on the screen and in the record. See
    // `CompactionTail::why_line`.
    if r.fork.tail_items == 0
        && let Some(line) = r.fork.tail_why.as_ref().and_then(|t| t.why_line())
    {
        said.push_str(&format!(" {line}"));
    }
    if !r.summary_was_streamed {
        said.push_str(&format!(
            " Nothing of the summary turn reached this screen — it ran over a scratch \
             transcript — so here is what the model now reads in place of everything \
             before it:\n\n{}",
            r.summary_turn.summary
        ));
    }
    said
}

/// **The structure behind the sentence** — R27's `warning.compaction`.
///
/// The `detail` sentence is written for a person and stays exactly as it was, because it
/// is the fallback every reader that cannot use this lands on. This is the same
/// compaction as fields: the numbers the sentence spells out, the record split into the
/// template's sections, and the verbatim tail. A head that draws a *row* needs these and
/// cannot get them by parsing English out of the sentence — which is R24 frame 2's defect
/// one layer up.
///
/// `kind` is the warning code the caller is publishing under, so the two cannot disagree
/// about which compaction this is.
///
/// **The units are the shown ones**, the same conversion the sentence was rendered
/// through. A head that printed a ledger figure beside this sentence would be printing
/// two different numbers for one quantity — the defect `Config::shown_tokens`' own
/// comment records.
pub(crate) fn compaction_wire(
    r: &crate::harness::CompactReport,
    kind: &str,
    scales: impl Into<Scales>,
    resident: u64,
    window: u64,
    headroom: u64,
) -> letibot_sessionlog::event::CompactionReport {
    let scales = scales.into();
    let shown = shown_tokens(scales.before);
    let shown_after = shown_tokens(scales.after);
    use letibot_sessionlog::event::{CompactionReport, CompactionSection, CompactionTail};
    CompactionReport {
        kind: kind.to_string(),
        tokens_before: shown(r.fork.was_tokens),
        tokens_after: shown_after(r.fork.base_tokens),
        transcript: r.fork.transcript_id.clone(),
        resident,
        window,
        headroom,
        cut_off: r.fork.truncated,
        template: letibot_sessionlog::event::COMPACTION_TEMPLATE.to_string(),
        // **The record, split by the daemon, not by the head.** A missing section here
        // means the model wrote no such heading; an empty body means it wrote the
        // heading and nothing under it. Two facts, and the list is the only shape that
        // keeps them apart.
        sections: letibot_turn::parse_summary_sections(&r.summary_turn.summary)
            .into_iter()
            .map(|(name, body)| CompactionSection { name, body })
            .collect(),
        // **Present on every compaction, including a local one**, where it is an object
        // with zero turns. The local artefact is the remote one with an empty tail.
        tail: CompactionTail {
            turns: r.fork.tail_turns.clone(),
            carried: r.fork.tail_items as u64,
            because: r.fork.tail_because.clone(),
            dropped: r.fork.tail_dropped.unwrap_or(0) as u64,
        },
    }
}

impl SessionSource for StoreSessions {
    fn set_title(&self, session_id: &str, title: &str) -> Result<(), String> {
        let g = self.store.lock().unwrap_or_else(|e| e.into_inner());
        g.set_title(session_id, title).map_err(|e| e.to_string())
    }

    /// **Make it durable who this session is for** — see
    /// `letibot_sessionlog::SessionOrigin`.
    ///
    /// Called twice for a session a head created: once when the daemon knows who asked (there
    /// is no row yet, so the store writes nothing) and once from `Sessions::open`, when the row
    /// exists. `Store::set_session_origin` refuses to overwrite a value it already has, so the
    /// second call is the one that lands and a third would change nothing.
    ///
    /// A failure is swallowed, deliberately: this is a picker's grouping and nothing else reads
    /// it, so a busy store must not turn *make me a session* into a refusal over a word the
    /// person never typed. The `origin` column stays `NULL`, which is the honest state — *nobody
    /// has said* — and the grouping rule falls back to the marks the row does carry.
    fn set_origin(&self, session_id: &str, origin: letibot_sessionlog::SessionOrigin) {
        let g = self.store.lock().unwrap_or_else(|e| e.into_inner());
        let _ = g.set_session_origin(session_id, origin.as_str());
    }

    fn todos(&self, session_id: &str) -> Vec<letibot_sessionlog::event::TodoEntry> {
        let g = self.store.lock().unwrap_or_else(|e| e.into_inner());
        g.todos(session_id)
            .unwrap_or_default()
            .into_iter()
            .map(crate::harness::Harness::todo_entry)
            .collect()
    }

    /// **Both queues, whole, for a head's pane** (R61) — every row, the leases with their
    /// heartbeats, the targets' health and the last moves, through this source's own connection
    /// to the file the queues' threads write. A table that could not be read is SAID in the
    /// snapshot (`unreadable`) and never answered as an empty queue (R61 defect 12, R67 item 15).
    fn queues(&self) -> letibot_sessionlog::event::QueueSnapshot {
        let g = self.store.lock().unwrap_or_else(|e| e.into_inner());
        crate::mergequeue::snapshot(&g)
    }

    /// **The asking session's repository, by the queues' own rule** (R92 item 5) — the
    /// workspace the store holds for it, read as `mergequeue::repository_of_workspace` reads it.
    /// The store lock is released before git is asked.
    fn queue_here(&self, session_id: &str) -> Option<letibot_sessionlog::event::QueueHere> {
        let workspace = {
            let g = self.store.lock().unwrap_or_else(|e| e.into_inner());
            g.session_workspace(session_id).ok().flatten()?
        };
        let repositories =
            crate::mergequeue::repositories_of_workspace_cached(std::path::Path::new(&workspace));
        Some(letibot_sessionlog::event::QueueHere {
            repository: match repositories.as_slice() {
                [one] => Some(one.clone()),
                _ => None,
            },
            repositories,
            workspace,
        })
    }

    fn list(&self) -> Vec<StoredBrief> {
        let g = self.store.lock().unwrap_or_else(|e| e.into_inner());
        // **The picker's `waiting on you` mark, read for every session in one pass** — the board
        // is the question's one home, so a session nobody has opened since a restart is marked
        // as surely as a live one.
        let waiting = g.sessions_waiting_on_operator().unwrap_or_default();
        // **Every child's name, from its record** (`subagent_record::recorded_slugs`), so a child
        // listed after a restart is named as it was when it was spawned — and a name derived for
        // a record from before names is unique among its parent's children, as the runner's own
        // listing makes it.
        let mut by_parent: std::collections::BTreeMap<String, Vec<_>> = Default::default();
        for r in g.subagent_runs().unwrap_or_default() {
            by_parent.entry(r.parent_id.clone()).or_default().push(r);
        }
        let slugs: std::collections::HashMap<String, String> = by_parent
            .values()
            .flat_map(|runs| {
                runs.iter()
                    .map(|r| r.child_id.clone())
                    .zip(crate::subagent_record::recorded_slugs(runs))
                    .collect::<Vec<_>>()
            })
            .collect();
        g.list_sessions()
            .unwrap_or_default()
            .into_iter()
            .map(|s| StoredBrief {
                waits_on_operator: waiting.get(&s.id).copied().unwrap_or(0),
                slug: slugs.get(&s.id).cloned().unwrap_or_default(),
                // **Who it is for, off its own row** — `None` is a row written before the
                // column existed, and every row was the operator's then.
                origin: s
                    .origin
                    .as_deref()
                    .and_then(letibot_sessionlog::SessionOrigin::parse)
                    .unwrap_or_default(),
                session_id: s.id,
                title: s.title.unwrap_or_default(),
                items: s.items,
                last_activity_ms: s.last_activity_ms.max(0) as u64,
                parent_session_id: s.parent_session_id,
                context_tokens: s.context_tokens,
                context_cached: s.context_cached,
                stored_end: s.last_item.as_ref().map(stored_end),
                // **A pause already over is no pause**, whoever has yet to say so: a session this
                // daemon is not holding has no clock to end it, and a picker row that went on
                // saying *paused* past its hour would be the stale state this field exists to
                // replace.
                nags_paused: s
                    .nags_paused_until
                    .map(letibot_sessionlog::NagPause::from_stored)
                    .filter(|p| !p.over_at(letibot_sessionlog::registry::now_ms())),
                wiring: SessionWiring {
                    // The session's own model and workspace, from its row. The
                    // dialect and endpoint are this daemon's — they are not stored
                    // per session, and a plausible guess on a picker row is a guess
                    // somebody quotes.
                    model: s.model_id,
                    workspace: s.workspace_root,
                    ..self.wiring.clone()
                },
            })
            .collect()
    }
}

/// **How a stored conversation's last turn ended**, from its last row.
///
/// An answer with no calls is a finished turn, and its first non-empty line is what the
/// subagents pane shows under the row. Everything else — calls the answer made, a result
/// waiting for the next round, a prompt nobody answered, an answer cut short — is a turn
/// that was still going when the rows stopped.
fn stored_end(last: &letibot_transcript::TranscriptItem) -> letibot_sessionlog::StoredEnd {
    use letibot_sessionlog::StoredEnd;
    match last {
        letibot_transcript::TranscriptItem::Assistant {
            text,
            tool_calls,
            truncated: false,
        } if tool_calls.is_empty() => match text.lines().map(str::trim).find(|l| !l.is_empty()) {
            Some(line) => StoredEnd::Answered {
                first_line: line.to_string(),
            },
            None => StoredEnd::MidTurn,
        },
        _ => StoredEnd::MidTurn,
    }
}

/// **How much of the idle window is left** — `None` when it has all passed. The daemon has been
/// quiet for the shorter of the time since the newest event and the time since a look last found
/// it busy: a head's leaving is an event (`HeadDetached`), but a job that finishes or a monitor
/// that expires says nothing to any session's log, so the event clock alone would read a daemon
/// that was busy a minute ago as idle for as long as its last event is old.
fn idle_left(window: Duration, since_event: Duration, since_busy: Duration) -> Option<Duration> {
    let quiet = since_event.min(since_busy);
    (quiet < window).then(|| window - quiet)
}

#[cfg(test)]
mod idle_exit_clock {
    use super::idle_left;
    use std::time::Duration;

    const HOUR: Duration = Duration::from_secs(3600);
    const MIN: Duration = Duration::from_secs(60);

    /// A job that ran for two silent hours and finished a minute ago: the last event is two hours
    /// old, the daemon was busy a minute ago, and 59 minutes of the window are left.
    #[test]
    fn a_holder_that_ends_silently_restarts_the_window() {
        assert_eq!(idle_left(HOUR, 2 * HOUR, MIN), Some(59 * MIN));
    }

    /// The other way about: a look found it busy long ago, but an event five minutes old (a head
    /// that left) is the newer fact.
    #[test]
    fn a_newer_event_restarts_it_too() {
        assert_eq!(idle_left(HOUR, 5 * MIN, 3 * HOUR), Some(55 * MIN));
    }

    #[test]
    fn a_whole_window_of_both_is_idle() {
        assert_eq!(idle_left(HOUR, HOUR, 2 * HOUR), None);
        assert_eq!(idle_left(HOUR, 3 * HOUR, HOUR + MIN), None);
    }
}

#[cfg(test)]
mod review_pass;

/// **Two daemons, one store, one session** — the writer claim (`crate::writer`) driven through
/// `Sessions`, which is the door a daemon comes through. Here rather than beside `crate::writer`
/// because a test of an OPEN has to hold a `Sessions`' own harness, and that field is this
/// module's.
#[cfg(test)]
mod writer_tests;

#[cfg(test)]
mod stored_end_rule {
    use super::stored_end;
    use letibot_sessionlog::StoredEnd;
    use letibot_transcript::TranscriptItem;

    fn answer(text: &str, calls: usize, truncated: bool) -> TranscriptItem {
        TranscriptItem::Assistant {
            text: text.into(),
            tool_calls: (0..calls)
                .map(|i| letibot_transcript::ToolCall {
                    id: format!("c{i}"),
                    name: "bash".into(),
                    arguments: "{}".into(),
                })
                .collect(),
            truncated,
        }
    }

    #[test]
    fn only_a_whole_answer_with_no_calls_is_a_finished_turn() {
        assert_eq!(
            stored_end(&answer("\n  Done. Report follows.\nmore", 0, false)),
            StoredEnd::Answered {
                first_line: "Done. Report follows.".into()
            }
        );
        assert_eq!(stored_end(&answer("calling", 1, false)), StoredEnd::MidTurn);
        assert_eq!(stored_end(&answer("cut", 0, true)), StoredEnd::MidTurn);
        assert_eq!(stored_end(&answer("  \n", 0, false)), StoredEnd::MidTurn);
    }
}

#[cfg(test)]
mod idle_nag;

#[cfg(test)]
mod the_wake_route {
    //! **Which of the three things the daemon does with a wake for a session** — the operator's
    //! design in their own words: *"think about it like it is an erlang supervision tree. we talk
    //! to parents and they own lifecycle."*
    //!
    //! Tested here rather than through a daemon because the whole of it is the decision, and the
    //! decision is two facts the caller already looks up. The defect it replaces was one arm
    //! short: a ring naming a session the daemon does not hold was DISCARDED (R58), which is what
    //! made depth 2 a hole — a grandchild's settlement reached the bell and stopped there.
    use super::{WakeRoute, wake_route};
    use letibot_sessionlog::hub::Owner;

    /// The daemon's own session: the worker runs the turn, as it always did.
    #[test]
    fn a_session_the_daemon_holds_is_driven() {
        assert_eq!(wake_route(true, Some(Owner::Worker)), WakeRoute::Drive);
        // And `open` is the fact that decides it — a session in `open` whose hub has gone is
        // still the daemon's to run, and the harness inside is what runs it.
        assert_eq!(wake_route(true, None), WakeRoute::Drive);
    }

    /// **A live session served by its own thread is handed to that thread.** This is the arm
    /// R58 added, and it is a subagent: its harness lives on the thread its parent spawned it
    /// on, so the wake goes to that thread's own condvar rather than being dropped.
    #[test]
    fn a_session_owned_by_its_own_thread_goes_to_its_own_reader() {
        assert_eq!(
            wake_route(false, Some(Owner::OwnThread)),
            WakeRoute::ItsOwnReader
        );
    }

    /// **A session the worker owns and does not hold has no other reader** (R68.2, R68.9) — its
    /// open failed. Before the owner tag this was routed `ItsOwnReader` and reported `HandedOn`,
    /// a wake handed to a condvar nobody waits on.
    #[test]
    fn a_worker_session_that_did_not_open_is_not_handed_into_a_void() {
        assert_eq!(wake_route(false, Some(Owner::Worker)), WakeRoute::Gone);
    }

    /// A session with no hub at all has nobody to hand anything to — and that is not a loss:
    /// a session that closed handed its undrained settlements up a level first
    /// (`jobwatch::JobWatchers::stop`), so the wake it would have been told by is no longer the
    /// one holding the notice.
    #[test]
    fn a_session_that_is_gone_is_nothing() {
        assert_eq!(wake_route(false, None), WakeRoute::Gone);
    }
}

#[cfg(test)]
mod the_wire_report {
    //! **R27's `warning.compaction`, built and read back.**
    //!
    //! The builder is private to this module and takes a `CompactReport`, so this is
    //! where it can be exercised without a store, a model or a turn: what a head
    //! draws a row from is exactly this function's output, and every field of it is a
    //! fact the daemon already owned and was spelling out in English.

    use super::*;
    use crate::harness::{CompactReport, ForkReport};
    use letibot_sessionlog::event::CompactionTurn;

    /// R72 follow-up, defect B: *"summary turn reused 19584 of 0 carryable tokens"* —
    /// DeepSeek's own cache count over a bound the prefix check never computed.
    #[test]
    fn the_summary_turns_reuse_is_said_only_against_a_bound_that_was_measured() {
        let o = |cached_tokens, reusable| letibot_turn::CompactionOutcome {
            turn_id: "t".into(),
            summary: "s".into(),
            tool_calls: 0,
            truncated: false,
            cached_tokens,
            reusable,
            generated_tokens: 0,
        };
        let metered = summary_turn_reuse(&o(19_584, 0));
        assert!(!metered.contains("of 0"), "{metered}");
        assert!(
            metered.contains("19584 tokens from its cache") && metered.contains("no carryable"),
            "{metered}"
        );
        assert_eq!(
            summary_turn_reuse(&o(20_000, 20_480)),
            "summary turn reused 20000 of 20480 carryable tokens"
        );
        assert_eq!(
            summary_turn_reuse(&o(0, 0)),
            "summary turn reused nothing the server reported"
        );
    }

    /// **Zero has three causes, and the sentence says which** — R41's shape one document
    /// over, and the operator's own ask after an automatic compaction of leticl's wrote no
    /// tail:
    ///
    /// *"the head should be able to say which treatment a compaction got. A reader who cannot
    /// tell whether the tail was omitted by policy or by accident is in the position R41's job
    /// pane was in: an absence with two causes and one appearance."*
    ///
    /// The comment above `compaction_said` claimed the non-zero count was *"evidence of which
    /// path ran rather than a restatement of the policy (R27)"* — true in one direction only.
    /// All three of these are `carried == 0` and they were one screen.
    #[test]
    fn a_compaction_with_no_tail_says_which_zero_it_is() {
        let none = |because: &str| report("a summary", Vec::new(), 0, because);
        let local = compaction_said(&none("local_model"), None);
        let nothing = compaction_said(&none("nothing_fits"), None);
        let empty = compaction_said(&none("no_turns"), None);
        assert!(local.contains("LOCAL model"), "{local}");
        assert!(local.contains("R27"), "the ruling must be named: {local}");
        assert!(
            nothing.contains("larger than the whole tail budget"),
            "{nothing}"
        );
        assert!(empty.contains("nothing to carry"), "{empty}");
        // **And they are three different sentences**, which is the whole requirement: an
        // absence with one appearance was the defect.
        assert_ne!(local, nothing);
        assert_ne!(local, empty);
        assert_ne!(nothing, empty);
        // **`budget` stays silent**: the tail exists and the count beside it says how much, so
        // a sentence explaining that what fitted was what fitted is furniture.
        let with_tail = compaction_said(
            &report(
                "a summary",
                vec![CompactionTurn {
                    role: "operator".into(),
                    text: "hi".into(),
                }],
                1,
                "budget",
            ),
            None,
        );
        assert!(!with_tail.contains("No verbatim tail"), "{with_tail}");
        assert!(with_tail.contains("carried over verbatim"), "{with_tail}");
        // **A reason this build does not know is shown, not swallowed** — the wire's own rule
        // for `because`, and the shape that would catch a daemon one version ahead.
        let unknown = compaction_said(&none("something_new"), None);
        assert!(unknown.contains("something_new"), "{unknown}");
    }

    fn report(
        summary: &str,
        tail: Vec<CompactionTurn>,
        carried: usize,
        because: &str,
    ) -> CompactReport {
        CompactReport {
            fork: ForkReport {
                transcript_id: "s-1#t2".into(),
                parent_id: "s-1#t1".into(),
                forked_at: 42,
                was_tokens: 240_000,
                base_tokens: 9_000,
                truncated: true,
                tail_items: carried,
                // A fork with no tail cannot have dropped any of it: the daemon
                // derives this from the plan, and a plan that carries nothing has no
                // split. Set here the same way, so the test cannot assert one.
                tail_dropped: (carried > 0).then_some(2),
                // The same reason in the wire's shape, built here the way the daemon builds
                // it so a test of the sentence is a test of what a reader meets.
                tail_why: (!because.is_empty()).then(|| {
                    letibot_sessionlog::event::CompactionTail {
                        turns: tail.clone(),
                        carried: carried as u64,
                        because: because.into(),
                        dropped: u64::from(carried > 0) * 2,
                    }
                }),
                tail_turns: tail,
                tail_because: because.into(),
            },
            summary_turn: letibot_turn::CompactionOutcome {
                turn_id: "s-1#t1#9".into(),
                summary: summary.into(),
                tool_calls: 0,
                truncated: true,
                cached_tokens: 0,
                reusable: 0,
                generated_tokens: 0,
            },
            gained: Vec::new(),
            lost: Vec::new(),
            summary_was_streamed: false,
        }
    }

    /// **A local compaction carries the template and an empty tail, and that is not
    /// an absent field.** The ruled split (R27) says a local model gets no verbatim
    /// turns; the *shape* still has to be the remote one, because a session compacted
    /// locally and resumed against a remote model must not meet a record its reader
    /// cannot read. So the tail is an object with zero turns and a reason saying why.
    #[test]
    fn a_local_compaction_carries_an_empty_tail_and_still_says_why() {
        let r = report("## Objective\n\n- ship it", Vec::new(), 0, "local_model");
        let wire = compaction_wire(&r, "compacted", None, 240_000, 262_144, 16_384);
        assert_eq!(wire.kind, "compacted");
        assert_eq!(wire.tail.turns, Vec::new());
        assert_eq!(wire.tail.carried, 0);
        assert_eq!(wire.tail.because, "local_model");
        assert_eq!(wire.tail.dropped, 0);
        assert_eq!(
            wire.template,
            letibot_sessionlog::event::COMPACTION_TEMPLATE
        );
        assert!(wire.cut_off, "the report's cut-off is the wire's");
        assert_eq!(wire.transcript, "s-1#t2");
        assert_eq!(wire.resident, 240_000);
        assert_eq!(wire.window, 262_144);
        assert_eq!(wire.headroom, 16_384);
    }

    /// **A remote compaction carries the turns themselves**, in order, with a role
    /// each — and the two facts about a mid-exchange start are both here rather than
    /// left to be parsed out of the sentence.
    #[test]
    fn a_remote_compaction_carries_the_turns_and_the_split() {
        let r = report(
            "## Objective\n\n- ship it\n\n## Blocked\n\n",
            vec![
                CompactionTurn {
                    role: "operator".into(),
                    text: "carry the recent past".into(),
                },
                CompactionTurn {
                    role: "agent".into(),
                    text: "working on it".into(),
                },
            ],
            7,
            "budget",
        );
        let wire = compaction_wire(&r, "compacted", None, 1, 2, 3);
        assert_eq!(wire.tail.carried, 7, "items, not turns");
        assert_eq!(wire.tail.turns.len(), 2, "turns");
        assert_eq!(wire.tail.turns[0].role, "operator");
        assert_eq!(wire.tail.because, "budget");
        assert_eq!(wire.tail.dropped, 2);
    }

    /// **The record is split section by section, and the two absences stay apart.**
    /// `Blocked` is present with an empty body — *nothing is blocked* — while
    /// `Relevant Files` is absent altogether — *nobody said*. A reader that collapsed
    /// them would report silence as a clean bill of health.
    #[test]
    fn the_sections_keep_written_and_empty_apart_from_never_written() {
        let r = report(
            "## Objective\n\n- ship it\n\n## Work State\n\n### Blocked\n\n### Active\n\n- going",
            Vec::new(),
            0,
            "local_model",
        );
        let wire = compaction_wire(&r, "compacted", None, 1, 2, 3);
        let names: Vec<&str> = wire.sections.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["Objective", "Active", "Blocked"]);
        let blocked = wire.sections.iter().find(|s| s.name == "Blocked").unwrap();
        assert_eq!(blocked.body, "", "written and empty");
        assert!(
            !names.contains(&"Relevant Files"),
            "never written at all, which is a different fact"
        );
        // And a record that ignored the template yields an empty list rather than
        // invented sections — the raw text is still in `detail`.
        let prose = report(
            "I could not follow the format.",
            Vec::new(),
            0,
            "local_model",
        );
        assert!(
            compaction_wire(&prose, "compacted", None, 1, 2, 3)
                .sections
                .is_empty()
        );
    }

    /// **The units are the operator's, the same conversion the sentence uses**, so a
    /// head cannot print one number beside a sentence carrying another: this is the
    /// defect `Config::shown_tokens` was written for, one layer along.
    #[test]
    fn the_numbers_are_the_same_units_the_sentence_is_in() {
        // 3 ledger tokens to 2 provider tokens: 240,000 ledger is 160,000 shown.
        let scale = Some((3u64, 2u64));
        let r = report("## Objective\n\n- x", Vec::new(), 0, "local_model");
        let wire = compaction_wire(&r, "compacted", scale, 300, 300_000, 18_000);
        assert_eq!(wire.tokens_before, 160_000);
        assert_eq!(wire.tokens_after, 6_000);
        assert_eq!(
            wire.resident, 300,
            "the caller's own conversion, passed through"
        );
        // And the sentence beside it agrees, which is the property that matters.
        let said = compaction_said(&r, scale);
        assert!(said.contains("160000 → 6000"), "{said}");
    }
}

#[cfg(test)]
mod the_subagent_exit_wait {
    //! **What the daemon's exit says about the subagent threads it waited for** — the pure half
    //! of `Sessions::drain_subagents`, tested where it lives rather than through a daemon.
    //!
    //! The wait itself is `Harness::wait_for_children` (tested in `harness.rs`), and the call
    //! the daemon makes is exercised end to end by `message_between_turns`'s
    //! `the_daemons_exit_waits_for_a_child_and_gives_up_on_a_bound`. What is left here is the
    //! telling: which of the two outcomes speaks, what it names, and that the ordinary quit is
    //! silent.
    use super::SubagentExit;
    use std::time::Duration;

    /// **A quit with nothing running says nothing.** This is the ordinary case — no session had
    /// a child to stop — and a line here would be a warning about nothing on every quit.
    #[test]
    fn a_quit_with_nothing_running_says_nothing() {
        assert_eq!(SubagentExit::Waited { told: 0 }.sentence(), None);
    }

    /// **A wait that succeeded says so**, which is the other half of *the log says which of the
    /// two it did*: a reader must be able to tell a clean exit from one that never looked.
    #[test]
    fn a_wait_that_succeeded_says_how_many_sessions_it_waited_for() {
        let said = SubagentExit::Waited { told: 2 }
            .sentence()
            .expect("two sessions had children, so there is something to say");
        assert!(said.contains('2'), "{said}");
        assert!(said.contains("left"), "it claims the threads went: {said}");
    }

    /// **A give-up NAMES the session and the time it waited** — a count would leave the reader
    /// unable to tell which session is still computing, and the number is the only way they can
    /// tell a child that ignored the interrupt from one that was never told.
    #[test]
    fn a_give_up_names_the_session_and_the_time_it_waited() {
        let said = SubagentExit::GaveUp {
            sessions: vec!["s-root".to_string(), "s-other".to_string()],
            waited: Duration::from_secs(5),
        }
        .sentence()
        .expect("two sessions still running is a sentence");
        assert!(
            said.contains("s-root") && said.contains("s-other"),
            "{said}"
        );
        assert!(
            said.contains("5s"),
            "the bound is part of the record: {said}"
        );
        assert!(
            said.contains("SIGSEGV") && said.contains("not clean"),
            "the sentence says what giving up costs, rather than claiming a clean exit: {said}"
        );
    }
}

#[cfg(test)]
mod a_childs_state_is_read_from_the_right_facts {
    //! **The two facts `working` is filled from, and the one it is NOT.**
    //!
    //! The defect these hold shut, measured: `working` used to be
    //! `SessionStatus::running`, which the engine publishes **once per ROUND** — so it is false for
    //! the whole of a round's tool calls and for the boundary between two rounds, which is where a
    //! child spends most of its wall clock. A child executing a ten-minute build therefore read as
    //! *no turn in flight* for ten minutes, and the idle plan-check told its reader to go and look
    //! at a child that was working. `child_reading` fills it from the daemon's own record of the
    //! turn, OR the hub's corrected view, and the cases below are those two halves.
    //!
    //! **Everything here is constructed**: a registry, one hub, a handful of events and a journal
    //! entry. No child, no model, no clock — the reading is a value, which is what makes the three
    //! states assertable at all.
    use super::{child_reading, child_sessions};
    use crate::tasks::{TaskEntry, TaskJournal};
    use letibot_sessionlog::SessionEvent;
    use letibot_sessionlog::event::{FinishReason, Timings, Usage};
    use letibot_sessionlog::hub::Hub;
    use letibot_sessionlog::registry::{Registry, SessionWiring};
    use letibot_tools::builtins::todo::{ChildDoing, child_doing};
    use std::sync::Arc;

    /// A registry holding one session, so a reading has a hub to read.
    fn registry_with(id: &str) -> (Arc<Registry>, Arc<Hub>) {
        let r = Registry::new();
        let hub = r.new_hub(id);
        r.adopt(
            hub.clone(),
            "a child",
            SessionWiring::default(),
            Some("s-parent".into()),
        )
        .expect("the child adopts");
        (r, hub)
    }

    /// A round of a turn opening, as the engine publishes it — once per round, which is the whole
    /// reason `status().running` is the wrong fact.
    fn a_round_opens(hub: &Hub, turn: &str) {
        hub.publish(SessionEvent::TurnStarted {
            turn_id: turn.into(),
            model: "m".into(),
            ledger_head: "ab".into(),
            began_ms: None,
        });
    }

    /// The round's generation ending, with its calls still to run.
    fn the_round_generates_out(hub: &Hub, turn: &str) {
        hub.publish(SessionEvent::TurnFinished {
            turn_id: turn.into(),
            finish_reason: FinishReason::Eos,
            usage: Usage::default(),
            timings: Timings::default(),
        });
    }

    /// A call of that round starting to execute — the window `status().running` cannot see.
    fn a_call_starts(hub: &Hub, turn: &str, call: &str) {
        hub.publish(SessionEvent::ToolStarted {
            turn_id: turn.into(),
            call_id: call.into(),
            name: "bash".into(),
            access: "exec".into(),
        });
    }

    /// **A child in its tool calls is WORKING, and the round-level flag says it is not.**
    ///
    /// This is the case that made the check send its reader to `job_kill` about a child that was
    /// building, and the two assertions are the two halves of the fix: the old fact is still false
    /// (so nothing about `SessionStatus::running` has been changed for the heads that read it), and
    /// the reading says working anyway.
    #[test]
    fn a_child_executing_a_tool_call_is_working_though_no_round_is_generating() {
        let (r, hub) = registry_with("s-1-sub-1");
        a_round_opens(&hub, "t1");
        the_round_generates_out(&hub, "t1");
        a_call_starts(&hub, "t1", "c1");

        assert!(
            !hub.status().running,
            "the round's own flag is false here — that is the fact this change stops reading"
        );
        assert!(
            hub.turn_in_flight(),
            "and the turn IS in flight: its call has not finished"
        );

        // The journal's `running` record is the other half, and it is true for the whole of the
        // child's first turn — including this window, which the hub's view only sees because of
        // the unfinished call.
        let now = letibot_sessionlog::registry::now_ms();
        let reading = child_reading(&r, "s-1-sub-1", true, now);
        assert!(
            reading.working,
            "a child inside its tool calls is working: {reading:?}"
        );
        assert_eq!(
            child_doing(&reading),
            ChildDoing::Working,
            "and the state follows from the reading, not from a judgement: {reading:?}"
        );

        // **The record alone is enough** — the same hub, read as a child whose slot is Running but
        // whose first round has not even opened (the opening window, where the journal says
        // `running` and the hub has no turn at all).
        let (r2, _hub2) = registry_with("s-2-sub-1");
        assert!(
            child_reading(&r2, "s-2-sub-1", true, now).working,
            "the daemon's own record of the turn is a fact on its own"
        );
        assert!(
            !child_reading(&r2, "s-2-sub-1", false, now).working,
            "and with neither the record nor a turn in the hub, nothing is in flight"
        );
    }

    /// **A child that answered and is parked is BETWEEN TURNS, and that is the fact the parent
    /// could not get** — the state whose only door is a message.
    #[test]
    fn a_child_that_answered_and_parked_is_between_turns() {
        let (r, hub) = registry_with("s-1-sub-2");
        a_round_opens(&hub, "t1");
        a_call_starts(&hub, "t1", "c1");
        the_round_generates_out(&hub, "t1");
        // The call finishes, the model answers, the turn ends: the child parks. The slot has
        // settled by then, which is what `false` is here — the same fact the journal's `done` and
        // `TaskSlot::Done` carry.
        hub.publish(SessionEvent::ToolFinished {
            turn_id: "t1".into(),
            call_id: "c1".into(),
            outcome: letibot_transcript::ToolOutcome::Ok,
            payload_digest: "d".into(),
            inline_bytes: 1,
            full_bytes: 1,
            spill: None,
            repairs: 0,
            edit: None,
        });

        let now = letibot_sessionlog::registry::now_ms();
        let reading = child_reading(&r, "s-1-sub-2", false, now);
        assert!(!reading.working, "no turn is in flight: {reading:?}");
        assert!(
            reading.quiet_for.is_some(),
            "and something HAS been published in it, which is what tells a parked child from one that has not started: {reading:?}"
        );
        assert_eq!(
            child_doing(&reading),
            ChildDoing::BetweenTurns,
            "the state a parent can act on: {reading:?}"
        );
    }

    /// **A second turn is seen, and that is what the hub's half is for.** The slot has already
    /// settled and the journal says `done` — so the daemon's own record of *the* turn cannot see
    /// it — while the child is running the turn a `task_message` started.
    #[test]
    fn a_later_turn_is_in_flight_even_though_the_slot_has_settled() {
        let (r, hub) = registry_with("s-1-sub-3");
        a_round_opens(&hub, "t1");
        the_round_generates_out(&hub, "t1");
        a_round_opens(&hub, "t2");

        let now = letibot_sessionlog::registry::now_ms();
        // `false` is the settled slot and the journal's `done`: everything the daemon recorded
        // about the FIRST turn says the child is finished with it.
        let reading = child_reading(&r, "s-1-sub-3", false, now);
        assert!(
            reading.working,
            "the hub is the only thing that can see a turn the child runs after its first:              {reading:?}"
        );
        assert_eq!(child_doing(&reading), ChildDoing::Working, "{reading:?}");
    }

    /// **A child with no hub at all reads as NO EVIDENCE, not as no progress** — and the caller's
    /// own record still travels, because that is a fact about the spawn rather than about the
    /// child's log.
    #[test]
    fn a_child_this_daemon_holds_no_session_for_reads_as_no_evidence() {
        let r = Registry::new();
        let now = letibot_sessionlog::registry::now_ms();
        let reading = child_reading(&r, "s-nobody-sub-1", false, now);
        assert!(reading.quiet_for.is_none(), "{reading:?}");
        assert!(!reading.working, "{reading:?}");
        assert_eq!(
            child_doing(&reading),
            ChildDoing::NoEvidence,
            "nothing has been published in it, which is not the same fact as silence: {reading:?}"
        );
        assert!(
            child_reading(&r, "s-nobody-sub-1", true, now).working,
            "and a spawn the journal has recorded as running is still that: {reading:?}"
        );
    }

    /// **`child_sessions` is the same reading, keyed by the journal's own state** — so the notice
    /// and `task_result` cannot disagree about one child, and a `done` child is not in the snapshot
    /// at all (which is what resolves a row's need the ordinary way).
    #[test]
    fn the_snapshot_is_the_journal_gated_and_the_reading_shared() {
        let (r, hub) = registry_with("s-1-sub-4");
        a_round_opens(&hub, "t1");
        let journal = TaskJournal::new(
            None,
            Arc::new(letibot_tools::builtins::lsp::LspConfig::default()),
            Arc::new(letibot_tools::builtins::skill::SkillRegistry::default()),
        );
        for (name, state) in [
            ("s-1-sub-4", "running"),
            ("s-1-sub-5", "opening"),
            ("s-1-sub-6", "done"),
        ] {
            journal.record(TaskEntry {
                name: name.into(),
                role: "coder".into(),
                state: state.into(),
                tokens: 0,
                elapsed: 0.0,
                prompt: "a child".into(),
                parent: "s-parent".into(),
            });
        }
        let now = letibot_sessionlog::registry::now_ms();
        let children = child_sessions(&r, &journal, now);

        let running = children.child("s-1-sub-4").expect("a running child");
        assert!(
            running.working,
            "the journal's `running` IS the daemon's record that a turn is in flight: {running:?}"
        );
        assert_eq!(
            child_doing(running),
            ChildDoing::Working,
            "and with a fresh log it is working: {running:?}"
        );

        let opening = children.child("s-1-sub-5").expect("an opening child");
        assert!(!opening.working, "{opening:?}");
        assert_eq!(
            child_doing(opening),
            ChildDoing::NoEvidence,
            "a spawn that has not opened has no hub and nothing published: {opening:?}"
        );

        assert!(
            children.child("s-1-sub-6").is_none(),
            "a child the journal says `done` is not in the snapshot, which is how a row that waits on it is released"
        );
    }
}

#[cfg(test)]
mod operator_run_failures {
    //! **A `!` run whose rows could not be written leaves the session able to run the next one** —
    //! R67 item 11 — and the failure reaches the session's hub rather than only the daemon's
    //! stderr (item 15).
    //!
    //! The settle that fails here fails because the harness is gone from `open` between the run's
    //! start and its result — the session closed under a run in flight, which is one of the ways
    //! the arm's `Err` is reached and the only one a test can arrange without breaking a store.
    //! What the test measures is the arm's bookkeeping, which is the same whatever the reason:
    //! the run's flag, and what was said where.
    use super::*;

    #[test]
    fn a_settle_that_fails_still_ends_the_run_and_says_so_on_the_hub() {
        let Some(gguf) = crate::vocab_gguf() else {
            return;
        };
        let dir = std::env::temp_dir().join(format!(
            "letibot-bang-settle-{}-{}",
            std::process::id(),
            letibot_sessionlog::registry::now_ms()
        ));
        std::fs::create_dir_all(&dir).expect("the temp dir");
        let mut cfg = Config::for_this_box(&dir);
        cfg.vocab_gguf = Some(gguf);
        cfg.store = Some(dir.join("sessions.db"));
        cfg.session_id = "s-bang".into();
        // No turn runs here — the settle fails before one could — but a test that could reach
        // this box's model server must not be able to, so the endpoint is a port nothing serves.
        cfg.endpoint = letibot_turn::Endpoint::new("127.0.0.1", 9);
        let parts = Parts::load(&cfg).expect("the vocabulary loads");
        let registry = Registry::new();
        registry
            .create(&cfg.session_id, "", Sessions::wiring(&cfg))
            .expect("the session registers");
        let mut sessions =
            Sessions::open_first(&parts, cfg.clone(), registry.clone()).expect("it opens");

        // A run in flight, as `start_operator_run` leaves it — and then the session's harness
        // goes away before the result comes back.
        sessions
            .bang_state("s-bang")
            .expect("the run state")
            .start();
        assert!(sessions.bang_run_in_flight("s-bang"));
        let harness = sessions.open.remove("s-bang").expect("the harness");

        let settle = QueuedCommand {
            head_id: "test-head".into(),
            identity: "dead".into(),
            client_request_id: "req-1".into(),
            at_seq: 0,
            enqueued_ms: 0,
            kind: CommandKind::OperatorShellResult {
                line: "! ls".into(),
                who: "dead".into(),
                call_id: "bang-1".into(),
                outcome: letibot_transcript::ToolOutcome::Ok,
                payload: "a\nb\n".into(),
                spill: None,
            },
        };
        let out = sessions.dispatch("s-bang", &settle);
        assert!(
            matches!(out, Outcome::Failed(_)),
            "the settle had nowhere to write"
        );
        assert!(
            !sessions.bang_run_in_flight("s-bang"),
            "**the run is over, so the flag is down** — left up, every later `!` line is parked \
             behind a run that already ended, and `!` is dead for the session's life"
        );

        let codes: Vec<String> = registry
            .get("s-bang")
            .expect("the hub")
            .retained()
            .iter()
            .filter_map(|e| match &e.event {
                SessionEvent::Warning { code, .. } => Some(code.clone()),
                _ => None,
            })
            .collect();
        assert!(
            codes.iter().any(|c| c == "operator_shell_failed"),
            "the failure is on the session's own log, not only on stderr: {codes:?}"
        );
        drop(harness);
        registry.close();
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod seat_slots {
    //! **A session that fails to open leaves no seat slot behind** — R67 item 21.
    use super::*;

    #[test]
    fn a_failed_open_takes_its_slot_with_it() {
        let Some(gguf) = crate::vocab_gguf() else {
            return;
        };
        let dir = std::env::temp_dir().join(format!(
            "letibot-seat-slots-{}-{}",
            std::process::id(),
            letibot_sessionlog::registry::now_ms()
        ));
        std::fs::create_dir_all(&dir).expect("the temp dir");
        let mut cfg = Config::for_this_box(&dir);
        cfg.vocab_gguf = Some(gguf);
        cfg.store = Some(dir.join("sessions.db"));
        cfg.session_id = "s-first".into();
        cfg.endpoint = letibot_turn::Endpoint::new("127.0.0.1", 9);
        let parts = Parts::load(&cfg).expect("the vocabulary loads");
        let registry = Registry::new();
        registry
            .create(&cfg.session_id, "", Sessions::wiring(&cfg))
            .expect("the first session registers");
        let mut sessions =
            Sessions::open_first(&parts, cfg.clone(), registry.clone()).expect("it opens");
        assert!(
            sessions.slots.contains_key("s-first"),
            "a root session has a slot"
        );

        // A root session whose workspace does not exist: `Harness::open` refuses it, after
        // `seat_tool` has run.
        let mut wiring = Sessions::wiring(&cfg);
        wiring.workspace = dir.join("not-there").display().to_string();
        registry
            .create("s-broken", "", wiring)
            .expect("the broken session registers");
        assert!(sessions.open("s-broken").is_err(), "the open must fail");
        assert!(
            !sessions.slots.contains_key("s-broken"),
            "a session that never opened kept its seat slot"
        );
        assert!(!sessions.seated.contains_key("s-broken"));
        registry.close();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
