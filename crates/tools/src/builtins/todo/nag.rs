//! The idle plan check: when a board is asked about, and what it is told. Two runtimes serve it -
//! a harnessd session from its turns and its child journals, and the bridge's Claude Code seat from
//! its tool calls and its packages (`docs/seat-todo.md`) - so the schedule lives beside the board it
//! reads, and neither owner keeps a second copy of the rule.
use std::time::{Duration, Instant};

use letibot_tokencore::store::TodoItem;

use super::{
    ChildSessions, RunningJobs, queue_review_in_flight, unfinished_plan,
    waits_on_a_child_with_room_to_breathe, waits_on_a_running_job, waits_on_an_undigested_ending,
    waits_on_the_room, when_a_queue_reviewer_goes_quiet, when_a_young_child_grows_up,
    when_an_ending_is_digested,
};

/// How long a session sits IDLE before the unfinished-plan check is sent. The operator's scheduling
/// *"send it when model is idling"*, and *"certainly not after my message."* A minute of silence is
/// the signal that nobody is waiting, any turn re-arms it, and it is the base every repeat doubles.
pub const TODO_NAG_AFTER: Duration = Duration::from_secs(60);

/// How long after the operator ADDS a row the check comes due - shorter than [`TODO_NAG_AFTER`]
/// because a person who has just added a row is not reading anything. Ten and not zero: three
/// rows added in a row finish inside it. Applied only where the window it replaces is longer.
pub const OPERATOR_ADD_NAG_AFTER: Duration = Duration::from_secs(10);

/// The ceiling on the repeat's interval - how long a stalled plan may go unmentioned. Every
/// check is a real model call, so the interval doubles per delivery and stops here: about twenty in
/// nine hours, which is a session reminded rather than shouted at or left alone.
pub const NAG_REPEAT_CAP: Duration = Duration::from_secs(30 * 60);

/// How often the queue's census is said again - two hours. It is NOT the plan's ladder: a stuck
/// queue is not a stalled plan, and the rate is chosen against the queue's own units.
pub const STUCK_REPEAT: Duration = Duration::from_secs(2 * 60 * 60);

/// Whether a row is work the idle plan-check may speak about. A `Completed`, `Postponed` or
/// `Cancelled` row owes nothing
pub fn the_check_may_ask_about(
    row: &TodoItem,
    children: &ChildSessions,
    jobs: &RunningJobs,
) -> bool {
    !matches!(
        row.status,
        letibot_tokencore::store::TodoStatus::Completed
            | letibot_tokencore::store::TodoStatus::Postponed
            // And the row the operator STRUCK OFF, for the same reason one word over: there is
            // nothing owed. `Cancelled` is not `Completed` - the row is the record of something
            // decided against - but the check's question is *what do you owe*, and the answer is nothing.
            | letibot_tokencore::store::TodoStatus::Cancelled
    ) && !waits_on_a_child_with_room_to_breathe(row, children)
        && !waits_on_a_running_job(row, jobs)
        && !waits_on_an_undigested_ending(row, jobs)
        // .B3: a merge queue's row is not nagged while its review is in flight.
        && !queue_review_in_flight(row, children)
        // And a room row is not nagged while the ROOM is working on the item - the daemon's
        // room pass puts a `TodoNeed::Room` edge on the row while the holder's beat is fresh and
        // takes it off in the pass that sees either end, so *does this row carry the edge* IS the fact.
        && !waits_on_the_room(row)
}

/// The plan as the idle check reads it - every row, minus the ones it may not speak about.
/// This is where a plan is narrowed, and deliberately the only place
pub fn the_plan_as_checked(
    rows: &[TodoItem],
    children: &ChildSessions,
    jobs: &RunningJobs,
) -> Vec<TodoItem> {
    rows.iter()
        .filter(|row| the_check_may_ask_about(row, children, jobs))
        .cloned()
        .collect()
}

/// Should the idle check be armed for a plan of these ROWS, given what it was last NAGGED
/// with? The `moved` half of the schedule, as one predicate, so the rule can be tested
/// without a daemon
pub fn nag_should_arm(
    todos: &[TodoItem],
    nagged: Option<&str>,
    children: &ChildSessions,
    jobs: &RunningJobs,
) -> bool {
    // The two arguments are the two halves of one reading: `the_plan_as_checked` is what the
    // check may SPEAK about, and the board is what its edges RESOLVE against. Both come from the one
    // predicate, so nothing here is a second answer to what a plan is.
    let askable = the_plan_as_checked(todos, children, jobs);
    match unfinished_plan(&askable, todos, children, jobs) {
        Some(text) => nagged != Some(text.as_str()),
        None => false,
    }
}

/// What the queue has to say to the idle check, as of one arming - the second subject this
/// clock speaks about besides the plan, from the operator's report of eight merge-queue
/// rows stuck for hours with no sentence anywhere giving the TOTAL or the AGE.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QueueNews {
    /// There are rows stuck long enough that the count and the age are news.
    pub says: bool,
    /// How long until there are - the deadline a clock arms for when rows are stuck but too
    /// young. The crossing is not an event anything rings, so without this the census would wait for
    /// a turn a quiet session may never take.
    pub matures_in: Option<Duration>,
}

impl QueueNews {
    /// Nothing stuck, or nothing this session is the project of.
    pub fn none() -> Self {
        Self::default()
    }
}

/// One session's idle clock: the plan's deadline, the queue's deadline, and the ladder that
/// paces the plan's repeats. A session the daemon holds runs its clock in its own sessions table;
/// the bridge's seat runs one over its board and its packages.
pub struct TodoNagClock {
    /// How long after the last turn or board move the check comes due.
    window: Duration,
    /// When the check comes due, or `None` for a session with nothing to check - which is
    /// the common case, and the one that keeps the waiter's sleep unbounded (the worker's
    /// `next_nag_at` returns NIL; a child's `take_own_work_until` gets `None`).
    due: Option<Instant>,
    /// The plan notice `due` was armed for. A re-arm for the same notice keeps the deadline it
    /// has: a session woken every few minutes by a monitor re-armed it from each turn's end,
    /// so a thirty-minute floor never arrived and the check went silent for hours.
    armed_for: Option<String>,
    /// The plan notice this session was last nagged with - the identity the backoff counts
    /// repeats of. A different notice is a new thing to say at the base window; the same one is a
    /// repeat at [`Self::interval`].
    nagged: Option<String>,
    /// When `nagged` was sent - the anchor the repeat's interval is measured from. Written and
    /// cleared with `nagged`, so the two cannot disagree about whether a notice is outstanding.
    nagged_at: Option<Instant>,
    /// How many times in a row that same notice has been delivered - the backoff's exponent
    /// one-based: the first delivery is what makes the next gap twice the window. A different
    /// notice, and the operator speaking (which clears the notice), start the ladder over.
    repeats: u32,
    /// Whether the operator has paused this session's checks (`/nag pause`), handed in by
    /// whoever owns the clock from the harness it reads the board from ([`Self::hold`]) - the
    /// pause is the session's fact, on its config and its row, and the clock holds only a copy
    hold: NagHold,
    /// The queue's census, as of this arming - [`QueueNews`], handed in by [`Self::queue`] by
    /// the same owner that hands in `hold`, and for the same reason.
    queue: QueueNews,
    /// How long between two deliveries of the census - [`STUCK_REPEAT`] in every daemon, and a
    /// field for `window`'s reason exactly: a test that waited two hours would not be a test. See
    /// [`Self::set_repeat`].
    repeat: Duration,
    /// When the census is next due, or `None` when nothing is stuck - a deadline of its own
    /// beside the plan's. A notice whose text carries a GROWING NUMBER cannot be paced by
    /// comparing texts (it would be new at every delivery) nor by the plan's ladder
    census_due: Option<Instant>,
    /// When the census was last delivered - the anchor its repeat is measured from.
    census_said_at: Option<Instant>,
}

/// Is the operator holding this session's plan-check, and until when - the clock's copy of
/// the session's `/nag pause`. `Paused { until: None }` arms nothing at all; `Paused {
/// until
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
    /// `pub` for `serve_child_under`'s signature, which a test drives - the constructor
    /// a caller should reach for is this one, `with_window` is the tests' door.
    pub fn new() -> Self {
        Self::with_window(TODO_NAG_AFTER)
    }

    /// A clock with a different window - the tests' door, and only theirs: driving the
    /// schedule has to be possible without sleeping through the window it pins.
    pub fn with_window(window: Duration) -> Self {
        TodoNagClock {
            window,
            due: None,
            armed_for: None,
            nagged: None,
            nagged_at: None,
            repeats: 0,
            hold: NagHold::Running,
            queue: QueueNews::none(),
            repeat: STUCK_REPEAT,
            census_due: None,
            census_said_at: None,
        }
    }

    /// The session's pause, as of this arming - set by the owner before every [`Self::rearm`]
    /// and [`Self::operator_added`], from the harness (`Harness::nag_hold`), so the clock
    /// cannot hold a pause the session no longer has. It arms nothing by itself
    pub fn hold(&mut self, hold: NagHold) {
        self.hold = hold;
    }

    /// What the queue has to say, as of this arming - [`QueueNews`], set by the owner before
    /// every [`Self::rearm`], exactly as [`Self::hold`] is. It arms nothing by itself; the next
    /// re-arm reads it and decides `census_due`.
    pub fn queue(&mut self, news: QueueNews) {
        self.queue = news;
    }

    /// Start (or stand down) the check, as of `now` - the one body the daemon's idle arm and a
    /// bridge seat's watcher both drive, so the two owners cannot drift. Six answers: nothing to
    /// ask about (no deadline at all); the plan moved (one base window from `now`)
    pub fn rearm(
        &mut self,
        todos: &[TodoItem],
        now: Instant,
        children: &ChildSessions,
        jobs: &RunningJobs,
    ) {
        // A paused session arms nothing but the pause's end - the operator's `/nag pause`
        // ahead of every other answer: what the plan says is not asked about while they hold it.
        if let NagHold::Paused { until } = self.hold {
            self.due = until;
            self.arm_census(now);
            return;
        }
        // A row the model has already moved to in progress is not asked about again until the
        // session has been idle for the longest interval the ladder reaches: the check that
        // restated *in progress* one window after the model wrote it got a restatement back.
        // One window for every free head, in progress or not: a row whose work is in flight says so
        // with a wait, which takes it off the check until the wait ends, and a row in progress with
        // none at a turn's end is a row the model forgot.
        let floor = self.window;
        let askable = the_plan_as_checked(todos, children, jobs);
        let text = unfinished_plan(&askable, todos, children, jobs);
        if nag_should_arm(todos, self.nagged.as_deref(), children, jobs) {
            let fresh = now + floor;
            self.due = match (self.due, &self.armed_for) {
                (Some(d), Some(was)) if Some(was) == text.as_ref() => Some(d.min(fresh)),
                _ => Some(fresh),
            };
            self.armed_for = text;
            self.arm_census(now);
            return;
        }
        self.armed_for = None;
        self.due = if unfinished_plan(
            &the_plan_as_checked(todos, children, jobs),
            todos,
            children,
            jobs,
        )
        .is_some()
        {
            // `nagged_at` is written with `nagged` and cleared with it, and this arm is reached only
            // with a notice outstanding - so the anchor is there, and a missing one would be a
            // clock that armed nothing rather than one that fired at the wrong moment.
            self.nagged_at.map(|at| at + self.interval().max(floor))
        } else {
            // The young child's deadline, or the end of an ending's grace, whichever is first. A grace
            // normally ends at a turn end, which re-arms this clock by itself; the deadline is for the
            // model that read the completion and went idle (see `when_an_ending_is_digested`).
            when_a_young_child_grows_up(todos, children)
                .into_iter()
                .chain(when_an_ending_is_digested(todos, jobs))
                .chain(when_a_queue_reviewer_goes_quiet(todos, children))
                .min()
                .map(|left| now + left)
        };
        self.arm_census(now);
    }

    /// Re-arm as if the owner had just gone quiet at `now` - a Claude Code seat's rule, where every
    /// tool call is a turn: a deadline for a notice that has not been delivered moves to the new
    /// `now`, because a seat still calling tools is working and has not stalled. What does NOT move
    /// is a notice already delivered: it waits out the ladder, so an unchanged board is said once.
    pub fn rearm_from(
        &mut self,
        todos: &[TodoItem],
        now: Instant,
        children: &ChildSessions,
        jobs: &RunningJobs,
    ) {
        self.armed_for = None;
        self.due = None;
        self.rearm(todos, now, children, jobs);
    }

    /// Arm the queue's census, as of `now` - the clock's second subject, on its own deadline. A
    /// pause arms nothing (a census is a nag)
    fn arm_census(&mut self, now: Instant) {
        // A queue with nothing stuck ENDS that census, so the next set of rows starts its own
        // clock: without this, the anchor of a census whose rows are gone would pace the first
        // notice of the next incident at up to one interval.
        if !self.queue.says && self.queue.matures_in.is_none() {
            self.census_said_at = None;
        }
        self.census_due = if matches!(self.hold, NagHold::Paused { .. }) {
            None
        } else if let Some(left) = self.queue.matures_in {
            Some(now + left)
        } else if let Some(at) = self.census_said_at {
            self.queue.says.then(|| at + self.repeat)
        } else if self.queue.says {
            Some(now)
        } else {
            None
        };
    }

    /// Whether the census is due as of `now`, spending the deadline when it is - `due_now`'s
    /// shape and its reason: the disarm is before the work, so a delivery that took longer than
    /// the interval does not fire again at once.
    pub fn census_due_now(&mut self, now: Instant) -> bool {
        match self.census_due {
            Some(at) if at <= now => {
                self.census_due = None;
                true
            }
            _ => false,
        }
    }

    /// Record that the census was delivered, as of `now` - before the outcome is known, for
    /// `said`'s reason: a notice whose write failed still counts as said, or it would come back at
    /// every pass. The repeat is anchored here and nowhere else.
    pub fn census_said(&mut self, now: Instant) {
        self.census_said_at = Some(now);
        self.census_due = self.queue.says.then(|| now + self.repeat);
    }

    /// When the census is next due, for the waiter's deadline.
    pub fn census_due_at(&self) -> Option<Instant> {
        self.census_due
    }

    /// How long after the last delivery the same notice may be sent again - the base window
    /// doubled once per delivery, capped at [`NAG_REPEAT_CAP`] because a reminder that has retreated
    /// into an hour is a plan that can stall unnoticed for an afternoon.
    fn interval(&self) -> Duration {
        let mut interval = self.window;
        for _ in 0..self.repeats {
            interval = (interval * 2).min(NAG_REPEAT_CAP);
        }
        interval
    }

    /// The operator has spoken: a new idle period, and the check may be made again. Whatever
    /// they said may have changed what the plan should be, so the once-per-saying is earned
    /// back. And the backoff goes with the notice
    pub fn prompt_arrived(&mut self) {
        self.armed_for = None;
        self.due = None;
        self.nagged = None;
        self.nagged_at = None;
        self.repeats = 0;
    }

    /// The operator ADDED a row to the board
    pub fn operator_added(
        &mut self,
        todos: &[TodoItem],
        now: Instant,
        children: &ChildSessions,
        jobs: &RunningJobs,
    ) {
        self.prompt_arrived();
        self.rearm(todos, now, children, jobs);
        // And not while the operator has paused the checks - the sooner check their row
        // earns is still a check, and `/nag pause` says so in its reply.
        if matches!(self.hold, NagHold::Paused { .. }) {
            return;
        }
        if nag_should_arm(todos, None, children, jobs) {
            self.due = Some(now + self.window.min(OPERATOR_ADD_NAG_AFTER));
        }
    }

    /// Record the notice a delivery just sent, as of `now` - before the outcome is known, which
    /// is deliberate: a check whose turn failed still counts as said, or the same notice would
    /// come back every window, which is the metronome at a slower rate.
    pub fn said(&mut self, notice: Option<String>, now: Instant) {
        self.repeats = match (&self.nagged, &notice) {
            (Some(was), Some(is)) if was == is => self.repeats.saturating_add(1),
            _ => 1,
        };
        self.nagged = notice;
        self.nagged_at = Some(now);
        self.armed_for = None;
    }

    /// The deadline for whoever waits on this clock - the worker's blocking wait for
    /// a session it holds, a child's serving thread for its own, a seat's watcher for its next
    /// pass. `None` when nothing is armed, so nothing spins on behalf of a board with no check
    /// pending.
    pub fn due_at(&self) -> Option<Instant> {
        self.due
    }

    /// Whether the check is due as of `now`, spending the deadline when it is. The disarm is
    /// before the turn that answers it: a delivery that left the old deadline in place would
    /// fire again immediately. And a session that is NOT due keeps its deadline.
    pub fn due_now(&mut self, now: Instant) -> bool {
        match self.due {
            Some(at) if at <= now => {
                self.due = None;
                true
            }
            _ => false,
        }
    }

    /// Change the window of a clock that already exists - the door `Sessions::set_nag_window`
    /// goes through, for a test that must drive a delivery on a session `open_first` already
    /// armed at the production window. The deadline already armed is left alone
    pub fn set_window(&mut self, window: Duration) {
        self.window = window;
    }

    /// Change the census's repeat on a clock that already exists - [`Self::set_window`]'s twin
    /// and `Sessions::set_stuck_repeat` is its only caller. The deadline already armed is left
    /// alone; the next arming and the next delivery read the new repeat.
    pub fn set_repeat(&mut self, repeat: Duration) {
        self.repeat = repeat;
    }

    /// The deadline arrived and is being answered - `serve_child`'s half of the
    /// disarm `due_now` performs for the worker, which learns the same fact from
    /// `OwnWork::TimedOut` instead.
    pub fn fired(&mut self) {
        self.due = None;
    }
}

impl Default for TodoNagClock {
    fn default() -> Self {
        Self::new()
    }
}
