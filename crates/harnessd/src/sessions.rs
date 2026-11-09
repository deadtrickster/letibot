//! One daemon, several sessions: the `Harness` per `Hub`.
//!
//! ```text
//!   Registry ── "s-1" → Hub ──┐            ┌── Harness(s-1)  engine · ledger · region · store
//!            ── "s-2" → Hub ──┼─ Bell ─> Sessions ── Harness(s-2)  … its own of each
//!            ── "s-3" → Hub ──┘            └── (s-3: not opened yet)
//! ```
//!
//! [`Parts`] is shared because it is read-only and expensive. Everything the prefix invariant names -
//! the engine, the `Session`, the ledger and its region, the tools, the transcript row - is per
//! session, and falls out of opening one [`Harness`] per hub.
//!
//! Sessions are opened lazily except the daemon's first: a dialect that does not fit, a missing GGUF
//! and an unwritable store are silent at run time, so they are raised at open time - and a later
//! open cannot newly meet them.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use letibot_sessionlog::hub::{CommandKind, Owner, QueuedCommand};
use letibot_sessionlog::origin::SessionOrigin;
use letibot_sessionlog::registry::{Registry, SessionSource, SessionWiring, StoredBrief};
use letibot_tokencore::store::{Store, TodoBy, TodoItem, TodoStatus};
// The plan's own text, and the queue it is served from - one renderer, so the idle check
// and the tool's reply cannot describe two different plans.
use letibot_tools::builtins::todo::{ChildSessions, RunningChild, RunningJobs};
use std::sync::{Arc, Mutex};

use crate::config::Config;
use crate::harness::{CompactReport, Harness, HarnessError, Parts, Reply};
use crate::turn_tail::{Door, TurnOwner, publish_failure, run_turn};

/// The review queue's own session - one, hardcoded, whose todo board is the queue. See the
/// module's own docs for the design and for where the rows live versus where the STATES live.
pub mod gatekeeper;

/// How long a monitor waiter blocks before re-checking whether the daemon is shutting down.
/// Not a poll interval - delivery is the condvar in `Monitors::wait_for_any` - and five seconds
/// is how long a thread may outlive `Bell::close`, which notifies a *different* condvar.
const SHUTDOWN_RECHECK: std::time::Duration = std::time::Duration::from_secs(5);

/// How many times one prompt may continue itself past the context wall. Each continuation
/// is a whole turn plus the compaction behind it, so three is the runaway guard for the
/// cycle the room check cannot see: every cycle does real work and no flag ever says stop.
pub(crate) const WALL_CONTINUES: usize = 3;

// The plan check's schedule is the board's own and lives beside it, because two runtimes serve it:
// a session from its turns, and the bridge's Claude Code seat from its tool calls (`docs/seat-todo.md`).
// The three types stay `pub` under their old names: an integration test drives a clock by hand.
pub use letibot_tools::builtins::todo::{NagHold, NagTimings, QueueNews, TodoNagClock};
pub(crate) use letibot_tools::builtins::todo::{the_check_may_ask_about, the_plan_as_checked};

/// The interval between this check and the next, when a `nag_` layer chose it: clamped to
/// [`NagTimings`]'s own two bounds - never shorter than the idle window, never longer than the repeat
/// cap. The layer names a number and cannot shout louder or go quieter than `leticode.toml` says.
pub fn the_next_check(next: Duration, timings: NagTimings) -> Duration {
    // `clamp` panics on a crossed pair, and a file may set a cap under its window.
    let lo = timings.window.min(timings.cap);
    let hi = timings.cap.max(lo);
    next.clamp(lo, hi)
}
// What only the idle-nag suite below reads: it composes the sentence and drives the ladder
// itself, so it takes these under the names they had before the schedule moved.
#[cfg(test)]
pub(crate) use letibot_tools::builtins::todo::{
    NAG_REPEAT_CAP, OPERATOR_ADD_NAG_AFTER, STUCK_REPEAT, nag_should_arm, unfinished_plan,
};

/// How long after a turn the idle arm looks for a tool call nothing answered. A grace and
/// not zero, because the turn has returned and its last append may not be published yet.
pub const SWEEP_AFTER: Duration = Duration::from_secs(2);

/// How long the orderly stop waits for one session's thread to settle its placement. A session
/// mid-turn answers when the interrupt lands; one that never answers is said rather than waited
/// on, because the process is already on its way out.
const SETTLE_PATIENCE: Duration = Duration::from_secs(30);

/// How long a `Sessions` being dropped waits for the threads that served its sessions.
/// A court still freeing the vocabulary when
/// the process runs its ELF finalizers is a SIGSEGV with no `panicked at` anywhere.
const COURT_TEARDOWN: Duration = Duration::from_secs(2);

/// How often this daemon reads the store for the merge-queue reviews its sessions are owed.
/// The queue's ring is per-daemon, so it cannot reach a review asked for in another daemon
/// or one asked before a restart.
pub(crate) const REVIEW_PASS_EVERY: Duration = Duration::from_secs(REVIEW_PASS_SECS);

/// How often a daemon that is still busy is looked at again for the idle exit - a head that
/// detaches is noticed within this, so the hour it counts is an hour and at most five minutes.
pub(crate) const IDLE_EXIT_RECHECK: Duration = Duration::from_secs(300);

/// The same number in seconds, for the reader that has no `Duration` - the merge queue's own
/// grace is counted in PASSES rather than seconds, so a grace shorter than one pass cannot declare
/// a gatekeeper missing before the clock that opens it has run.
pub(crate) const REVIEW_PASS_SECS: u64 = 30;

/// A status, as the STORE spells it - the wire→store half of the one conversion the two copies
/// of `TodoStatus` need. A `match` rather than a pass-through because the two crates own the type
/// separately and on purpose, so a variant added to one fails to compile here.
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

/// The children this daemon is running, as the plan-check needs them - the fact a board
/// cannot hold.
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
        // The journal's `running` IS the daemon's record that a turn is in flight - written just
        // before the child's first prompt is submitted and replaced at the turn's end. It fills
        // `working`, NOT `SessionStatus::running`
        let turn_started = entry.state == "running";
        out = out.running(
            entry.name.clone(),
            child_reading(registry, &entry.name, turn_started, now_ms),
        );
    }
    out
}

/// One live child, as the daemon can see it - the ONE place a [`RunningChild`] is filled
/// so the idle check's snapshot and `task_result` cannot come to different answers about
/// one child.
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
    // A child in `opening` has no hub yet - it is a session this daemon has not adopted - and that
    // is said as *no evidence* rather than as *no progress*: `None` and no turn.
    let Some(hub) = registry.get(name) else {
        return RunningChild {
            age,
            quiet_for: None,
            working: turn_started,
            unknown: None,
        };
    };
    let status = hub.status();
    RunningChild {
        age,
        quiet_for: (status.last_ms > 0)
            .then(|| Duration::from_millis(now_ms.saturating_sub(status.last_ms))),
        working: turn_started || hub.turn_in_flight(),
        unknown: None,
    }
}

/// Did this push put a row on the operator's half that was not there before? - the question that
/// tells an operator ADD (or re-wording) from the head re-pushing the rows it already sent, which it
/// does on every add, delete and HELLO. Open rows only: a row pushed as done is not a request.
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

/// What the worker did with one command.
pub enum Outcome {
    Replied(Box<Reply>),
    /// The session was compacted: one summary turn, then a transcript fork.
    /// The report is the evidence, not the word.
    Compacted(Box<CompactReport>),
    Failed(String),
    Ignored,
    /// The daemon ran no turn because the session has its OWN reader, and that reader was handed
    /// the wake. A subagent's harness lives on the thread its parent spawned it on, so the wake goes
    /// there: not `Ignored` (the condition was acted on) and not `Replied` (no turn ran here).
    HandedOn,
}

/// What a session's own thread says to the daemon's log. The wording belongs to the daemon and
/// the thread is not the daemon, so the thread's half travels through a sink ([`ReplySink`]) rather
/// than being said twice.
pub enum OutcomeSaid<'a> {
    Command(&'a QueuedCommand, &'a Outcome),
    Wake(&'a Outcome),
}

/// The daemon's log, as the sessions served on their own threads reach it. `Arc` because one sink
/// is shared by every session's thread and by the worker loop that installed it; `Fn` for the same
/// reason.
pub type ReplySink = Arc<dyn Fn(&str, OutcomeSaid<'_>) + Send + Sync>;

/// Something the daemon asks of a session whose command queue did not carry it. It carries its
/// own reply rather than the court carrying a typed one, so one channel serves every door.
type Job = Box<dyn FnOnce(&mut Sessions<'static>) -> Follow + Send>;

/// What a session's thread does after a job - carry on serving, or leave. `Finish` is
/// [`Sessions::drain_subagents`]'s door: the daemon is going, and this session's children have to be
/// told to stop and waited for by the thread that holds the harness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Follow {
    Serve,
    Finish,
}

/// One session, on its own thread - the reader that serves it, the shape a subagent already
/// had (`harness::serve_child`). Sessions must not share ONE worker running one command at a
/// time, or a prompt to one waits behind an unrelated session's turn.
struct Court {
    /// The nudge. A job sent here does not wake the thread by itself: it is blocked in the hub's
    /// condvar, so [`Court::ask`] rings that too.
    jobs: std::sync::mpsc::Sender<Job>,
    /// `None` once it has been joined, so a second shutdown waits on nothing.
    thread: Option<std::thread::JoinHandle<()>>,
    /// What this session is holding the daemon up for, as its thread last read it - the idle
    /// exit's question ([`Sessions::idle_exit_refusal`]), answered without standing still for a
    /// turn that may be minutes long.
    status: Arc<Mutex<Option<String>>>,
    /// The hub, because a job is delivered by waking the thread that is waiting on it.
    hub: Arc<letibot_sessionlog::hub::Hub>,
}

impl Court {
    /// Ask this session something, and wait for the answer. For the doors a caller outside the
    /// daemon reaches synchronously - `cli.rs`'s banner, and every test that drives `Sessions` by
    /// hand. `None` is a thread that has already gone: the session is closed, its answer nobody's.
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

    /// [`Court::ask`] with a deadline, for the daemon's last acts: a session that never answers is
    /// the caller's line to say, not a reason to hold the exit open.
    fn ask_within<R: Send + 'static>(
        &self,
        within: std::time::Duration,
        f: impl FnOnce(&mut Sessions<'static>) -> R + Send + 'static,
    ) -> Option<R> {
        let (tx, rx) = std::sync::mpsc::channel();
        let sent = self.jobs.send(Box::new(move |s: &mut Sessions<'static>| {
            let _ = tx.send(f(s));
            Follow::Serve
        }));
        sent.ok()?;
        self.hub.wake_its_own_reader();
        rx.recv_timeout(within).ok()
    }

    /// Ask without waiting - for the daemon's own pass, which must not stand still while a
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

    /// Tell this session's thread to leave, and answer what its children did: `(told, left)` -
    /// whether anything was told to stop, and whether anything could not be waited for.
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
    /// The parts, borrowed or owned. Borrowed by the daemon - the vocabulary and the dialect
    /// renderer are one load for the process, and `Sessions<'a>` has always borrowed them - and
    /// OWNED by each session's thread, which outlives the `&Parts` a constructor was handed.
    parts: std::borrow::Cow<'a, Parts>,
    /// The command line, minus the session id. Cloned per session with its own id
    /// substituted, so a second session is the same daemon and not a second
    /// configuration nobody typed.
    base: Config,
    /// The review-queue session THIS daemon owns - `s-gatekeeper-<workspace key>`, one per
    /// folder, and the same id across this daemon's restarts (see [`gatekeeper::session`]).
    gatekeeper: String,
    registry: Arc<Registry>,
    open: HashMap<String, Harness>,
    /// Sessions whose monitor waiter is already running. One waiter per name which is
    /// requirement 4 - the fleet has already paid for what two processes under one reader
    /// costs: the roster shows a seat attached while the real one hears nothing.
    armed: std::collections::HashSet<String>,
    /// The flowy seat this daemon holds, when it holds one. Daemon-level, on purpose: a seat is
    /// a persistent identity with one inbox reader, and a session is a temporary consumer of it - see `letibot_flowy`'s crate docs.
    seat: Option<letibot_flowy::Seat>,
    /// Each root session's `flowy` tool slot: the door is always seated, and the
    /// slot is filled when a seat is attached - at open, or later.
    slots: HashMap<String, letibot_flowy::tool::SeatSlot>,
    /// Each root session's condition on the seat, so a title change can rename
    /// the session's address (`@seat/title`).
    seated: HashMap<String, Arc<letibot_flowy::InboxCondition>>,
    /// The fabric block each root session last saw, and the doors its seat has - so a
    /// refresh after a compaction is a system update only when something changed, and
    /// is rendered for the same tools the session's prompt was.
    fabric_seen: HashMap<String, (letibot_flowy::FabricReach, String)>,
    /// Each session's idle plan-check clock - absent for a session with nothing to check. See
    /// [`TodoNagClock`], the one clock shared with the thread that serves a subagent.
    nags: HashMap<String, TodoNagClock>,
    /// The numbers every session's clock is made with: `[nag]`'s three over the clock's own, and
    /// the census's repeat from `[queue]` - [`NagTimings::default`] is the daemon that shipped, and
    /// a field so a test can drive a delivery without sleeping through a minute.
    nag: NagTimings,
    /// When to next look for a tool call nothing will answer. See `arm_sweep`.
    sweep_at: Option<Instant>,
    /// When to next look for a subagent this worker holds that has been idle long enough to park - 's idle-child policy, for the children served here rather than on their own thread.
    park_at: Option<Instant>,
    /// When to next read the store for the merge-queue reviews this daemon's sessions are owed.
    /// Armed at open to *now*, so a daemon that comes up between the ask and the ring serves it on
    /// its first idle pass.
    reviews_at: Option<Instant>,
    /// When to next ask the room what this session holds, armed at open to *now*. Every
    /// `Sessions` keeps its own, the daemon's and a court's alike, because the board it writes is
    /// the one its own `open` holds: one clock per owner, one writer per board.
    room_at: Option<Instant>,
    /// When to next ask whether this daemon has anything left to do - the idle exit
    /// (`Config::idle_exit_after`). Armed by the FIRST look rather than by the open, and `None` for
    /// ever when the daemon never exits on its own. See [`Sessions::idle_exit_due`].
    idle_exit_at: Option<Instant>,
    /// The last instant this worker saw the daemon busy - a head attached, a turn, a job. The
    /// idle window counts from the later of this and the newest event, because a head that
    /// detaches may say nothing
    busy_seen: Instant,
    /// What last held the idle exit off, said once each time it changes, so a daemon that never
    /// exits names why in its log.
    idle_held: Option<String>,
    /// The operator's run, per session: whether one is in flight on its own thread and the
    /// lines queued behind it. See [`crate::bangrun::State`] - this map only holds the handle
    bangs: HashMap<String, Arc<crate::bangrun::State>>,
    /// The last reason the review queue's session did not open, as said on a hub - so
    /// [`Sessions::ensure_gatekeeper`], which runs every review pass, says a standing failure
    /// once and a new one again. `None` once it opens.
    gatekeeper_said: Option<String>,
    /// Each session's plugin layers - the stack, the recorded inputs and the panes a head has
    /// opened (W4 of `docs/plugins-slice1.md`).
    plugins: HashMap<String, Arc<crate::plugins::PluginHost>>,
    /// The sessions served on a thread of their own - see [`Court`]. Empty on a daemon that
    /// was never told to serve them that way (`Daemon::run` is the one caller that says so), and
    /// empty on a court's own instance, which is what keeps a forwarder from forwarding to itself.
    courts: HashMap<String, Court>,
    /// Whether a session gets a thread of its own. Armed by `Daemon::run`, which IS the worker
    /// this replaces; a test that drives `Sessions` by hand leaves it off and drives the same
    /// single-threaded path every test has always driven. See [`Court`].
    threaded: bool,
    /// Where a turn's outcome is said - the daemon's log, once - so a session's thread can
    /// report an outcome it ran without owning the wording. `None` for a `Sessions` under a test.
    reply: Option<ReplySink>,
}

/// The three facts a session the DAEMON ITSELF starts has, which the daemon's command line cannot
/// supply - one caller today, [`Sessions::ensure_gatekeeper`]. The seat decides what it may do
/// the title is what `--sessions` prints, and the prompt is the paragraph only this session gets.
struct OwnSession {
    seat: crate::config::Seat,
    /// The name the session is opened under - the hardcoded one when it is being made, and the name
    /// its own row already holds when it is being RESUMED, so a session the operator renamed does
    /// not come back with the hardcoded name on the config and the renamed one in the store.
    title: String,
    /// What this session is told that no other session is - appended to the composed prompt at
    /// open, and `None` for a session the ordinary prompt is right for.
    prompt: Option<String>,
}

/// Which sessions attach to the seat, and how the attachment is wired. Root sessions only.
/// A subagent spawned by `task` hears the room through its parent and speaks through it
/// one name, one mind under it at a time.
fn is_root(registry: &Registry, session_id: &str) -> bool {
    registry.parent_of(session_id).is_none()
}

/// What the daemon can do with a wake for a session - and which of the three it is. Pure
/// so the routing decision is assertable without a daemon, a socket or a model: *"think
/// about it like it is an erlang supervision tree.
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
        // A hub the WORKER owns and does not hold is not somebody else's reader : it
        // is a session whose open failed, and handing a wake to its condvar was handing it into a
        // void while the outcome said `HandedOn`. A prompt reopens it lazily.
        (false, Some(Owner::Worker)) | (false, None) => WakeRoute::Gone,
    }
}

/// What a ring nobody can serve is called - the sentence [`Sessions::wake`] prints and publishes
/// when [`wake_route`] answers [`WakeRoute::Gone`]. A pure function so the wording is a thing a test
/// can hold, and it says what is NOT lost as well as what is: the settlement behind it is still a row.
fn dropped_wake(session: &str) -> String {
    format!(
        "a wake for `{session}` was dropped: this daemon is not holding that session (it was \
         closed, its open failed, or it has not been opened yet), so nothing could run the turn \
         it was for. A job or child settlement behind it is not lost — its row is in the store \
         with `delivered = 0`, and the session's next open says it."
    )
}

/// What a shutdown's wait for the subagent threads came to - the two outcomes
/// [`Sessions::drain_subagents`] distinguishes, so a caller's log can say which it was rather
/// than only speaking up when something went wrong.
#[derive(Debug)]
pub enum SubagentExit {
    /// Every thread left inside the bound. `told` is how many sessions had a child that was
    /// TOLD to stop, which is the count of sessions that actually had something running - `0` is
    /// a daemon that had nothing to wait for.
    Waited { told: usize },
    /// The bound was spent and these sessions still had a thread. See
    /// [`SubagentExit::sentence`].
    GaveUp {
        sessions: Vec<String>,
        waited: std::time::Duration,
    },
}

impl SubagentExit {
    /// The one line a shutdown prints, or `None` when there is nothing to say. Silence is the
    /// ordinary case, while a quit that HAD to wait and one that could not are exactly the two
    /// things a reader must be able to tell apart.
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

/// One session's children, told to stop and waited for - [`Sessions::drain_subagents`]' body for
/// one harness, so the daemon's loop and a session's own thread run the same two lines. The answer is
/// `(told, left)`.
fn stop_children_of(harness: &Harness, patience: std::time::Duration) -> (bool, bool) {
    let told = harness.stop_children().is_some();
    let left = !harness.wait_for_children(patience);
    (told, left)
}

/// The loop of a session served on its own thread - the shape a subagent already has. Three
/// things and no more
fn court_main(
    mut s: Sessions<'static>,
    hub: Arc<letibot_sessionlog::hub::Hub>,
    id: String,
    jobs: std::sync::mpsc::Receiver<Job>,
    status: Arc<Mutex<Option<String>>>,
) {
    use std::sync::mpsc::TryRecvError;
    let closed = loop {
        // The daemon's requests first. Each one has an asker behind it and a `Finish` is the
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
            // A wake is this session's own settlement being ready - the same door
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
                // The room's rows first, so the checks below read a board that is not one
                // pass stale: a row for an item this session has just released would otherwise be
                // asked about for a whole window.
                let pass = Instant::now();
                let held = s.fill_room_boards();
                crate::instruments::worker_pass("room", pass, held);
                let pass = Instant::now();
                let ran = s.deliver_due_nags();
                crate::instruments::worker_pass("todo-check", pass, ran);
                // And the queue's census, on the same idle arm and as a TURN - one model call per
                // interval while a row is stuck, because a notice reaches nobody.
                let pass = Instant::now();
                let rang = s.deliver_due_censuses();
                crate::instruments::worker_pass("queue-census", pass, rang);
                // And the provider's own clock: a usage limit's park resumes the turn it stopped, and a
                // switch that limit forced is undone once its model answers again.
                let pass = Instant::now();
                let back = s.resume_due_limits();
                crate::instruments::worker_pass("limit", pass, back);
                let pass = Instant::now();
                let parked = s.park_idle_children();
                crate::instruments::worker_pass("park", pass, parked);
                s.plugin_tick();
            }
        }
    };
    // Said once, whether this thread served or was closed out: the status is the daemon's
    // answer to *what is holding you up*, and a thread that has left holds nothing.
    *status.lock().unwrap_or_else(|e| e.into_inner()) = None;
    if closed {
        // A session CLOSED out from under it is over, and a DAEMON going away is not. A close
        // takes the registry entry away, while a stop closes every hub and leaves the tree standing
        // only the second has children still running, so only the second waits.
        if s.registry.get(&id).is_none() {
            if let Some(h) = s.open.get(&id) {
                h.stop_children();
                h.close_backend();
            }
            return;
        }
        // The hub closed under it - the daemon is stopping. The harness is NOT dropped here
        // its children may still be running, and telling them to stop is `drain_subagents`'s job
        // which arrives on this thread as its last job.
        while let Ok(job) = jobs.recv() {
            if job(&mut s) == Follow::Finish {
                break;
            }
        }
    }
}

/// Wait for the given threads to leave, bounded and shared - one deadline for all of them
/// so a second session cannot double the wait.
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
    /// No session's thread outlives the `Sessions` that armed it. A court owns its session's
    /// harness and its own clone of `Parts`, so a thread still inside either when the process
    /// exits is a SIGSEGV in `_dl_fini` - measured, see [`COURT_TEARDOWN`].
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

/// A close's answer with the panes it ended - the sentence the daemon's log carries beside "the
/// store keeps its rows", and the only report a PARK has, because nothing is told there. `None`
/// only when the session was not open here and took no pane with it either.
pub(crate) fn close_report(said: Option<String>, panes: usize) -> Option<String> {
    let panes = match panes {
        0 => return said,
        n => format!("{n} pane(s) ended with it"),
    };
    Some(match said {
        Some(said) if said.is_empty() => panes,
        Some(said) => format!("{said} · {panes}"),
        None => panes,
    })
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
            nag: cfg.leticode.nag_timings(),
            sweep_at: None,
            park_at: None,
            // Due at once: the first idle pass looks. A daemon that started between a review's
            // ask and its wake has the row and nothing else, and this is what finds it.
            reviews_at: Some(Instant::now()),
            // Due at once, for the same reason one fact over: a session that comes up holding a
            // room item has it in the room's memory and nowhere else, so the first pass is what
            // puts the reminder on its board.
            room_at: Some(Instant::now()),
            // Nothing yet: `idle_exit_due` arms it on the daemon's first idle look, because the open
            // below - the store, the harness, the layers - is work, and a window counting it is a
            // window a loaded box has spent before that look ever comes.
            idle_exit_at: None,
            busy_seen: Instant::now(),
            idle_held: None,
            bangs: HashMap::new(),
            gatekeeper_said: None,
            plugins: HashMap::new(),
            courts: HashMap::new(),
            threaded: false,
            reply: None,
        };
        let (tool, cond) = sessions.seat_tool(&id);
        // The system prompt is composed at open, for the tools the seat actually holds
        // from `prompts.toml`, the standing notes and the fabric. Message 0 is written
        // from it and never rewritten after - see `Config::compose_for_seat`.
        let mut cfg = cfg;
        cfg.seat_prompt = Some(crate::config::SeatPrompt::default());
        let cfg = sessions.with_fabric(cfg, cond.is_some());
        let mut harness =
            Harness::open_with_registry(parts, cfg, hub, None, tool, registry.clone())?;
        // And who this daemon's own first session is for, written now that its row exists
        // (`Harness::open` writes the row and this is the first moment there is one to write on).
        // Two answers and no others
        registry.record_origin(
            &id,
            if sessions.base.one_shot {
                SessionOrigin::OneShot
            } else if sessions.base.parent_seat.is_some() {
                SessionOrigin::Package
            } else {
                registry.origin_of(&id)
            },
        );
        sessions.fabric_opened(&id, &harness);
        // A `--continue` resumes the session on the model it was switched to. The first session is
        // the common resume
        harness.restore_provider_choice();
        // And on the compaction the no-progress guard stood down, for the same reason - a resumed
        // session coming back with the pre-emptive door re-armed is the guard's finding silently
        // unrecorded, and the loop it existed to stop comes back with it.
        if let Some(said) = harness.restore_auto_compact() {
            eprintln!("  {id}: {said}");
        }
        // The first session's operator-run state, same rule as the lazy path below
        // one `Arc` in the map, its clone in the harness.
        let bang = sessions
            .bangs
            .entry(id.clone())
            .or_insert_with(crate::bangrun::State::new)
            .clone();
        harness.set_bang_state(bang);
        // And what the last daemon left running comes back with it, or is reminded - the operator
        // *"we should bring subagents back - i cant afford to wait till they finish"*. Before the
        // ring below, so the wake it causes carries the `[restart]` notice. See `crate::restart`.
        harness.restore_after_restart();
        sessions.open.insert(id.clone(), harness);
        sessions.declare_flowy_monitor(&id, cond);
        // AND THE DAEMON'S OWN FIRST SESSION ARMS ITS CLOCK TOO.
        sessions.rearm_todo_nag(&id);
        // And the same rule one line down, because THIS is the session a restart restores. A
        // `--continue` comes back to a board, a queue and a set of job handles that the process
        // before it left behind
        sessions.put_a_restored_session_to_work(&id);
        // And the plugin layers this session has, found last so nothing above it waits for a
        // wasm compile: a session with no layers costs three `read_dir`s on directories that do
        // not exist.
        sessions.open_plugins(&id, crate::plugins::Inherited::none());
        Ok(sessions)
    }

    /// Serve every session on a thread of its own from now on, and say a turn's outcome where
    /// `reply` sends it. One caller: `Daemon::run`, which IS the single worker this replaces.
    pub fn serve_each_on_its_own_thread(&mut self, reply: ReplySink) {
        self.threaded = true;
        self.reply = Some(reply);
        // The first session was opened by `open_first`, before there was a thread to hand it to
        // its creation rang a `Work::Open` that the loop has not drained yet, and that is where
        // this is reached for it too. This catches the case where the ring was spent already.
        let ids: Vec<String> = self.open.keys().cloned().collect();
        for id in ids {
            self.spawn_court(&id);
        }
    }

    /// Whether each session is served on a thread of its own. See [`Court`].
    pub fn each_on_its_own_thread(&self) -> bool {
        self.threaded
    }

    /// Say a turn's outcome where the daemon's log reads it - once, from whichever thread ran
    /// the turn. Silence under a test, which supplies no sink.
    fn say(&self, session_id: &str, said: OutcomeSaid<'_>) {
        if let Some(reply) = &self.reply {
            reply(session_id, said);
        }
    }

    /// Every parked session whose reset has come, and every switch back a limit owes - the
    /// provider's own clock, on the worker's idle arm beside the plan-checks and the census. A park
    /// resumes the SAME turn: the prompt that was refused is still the last thing said, so the door
    /// opens a turn carrying the notice the park left behind.
    pub fn resume_due_limits(&mut self) -> usize {
        let mut resumed = self.ask_courts(|s| s.resume_due_limits());
        let due: Vec<String> = self
            .open
            .iter()
            .filter(|(_, h)| h.limit_park_due(h.limit_now_ms()))
            .map(|(id, _)| id.clone())
            .collect();
        for session_id in due {
            let out = self.turn(&session_id, Door::Limit, |h| match h.take_a_due_park() {
                Some(notice) => h.resume_after_limit(notice).map(Some),
                None => Ok(None),
            });
            match out {
                Some(Ok(_)) => resumed += 1,
                Some(Err(e)) => eprintln!("  {session_id} · the turn a limit parked -> {e}"),
                None => {}
            }
        }
        // And the way back: a switch a limit forced is undone once that model answers again, which
        // is not a turn at all - `/models` between turns is the same act.
        let back: Vec<String> = self
            .open
            .iter()
            .filter(|(_, h)| h.limit_return_due(h.limit_now_ms()))
            .map(|(id, _)| id.clone())
            .collect();
        for session_id in back {
            if let Some(h) = self.open.get_mut(&session_id)
                && let Some(said) = h.return_from_a_limit()
            {
                if let Some(hub) = self.registry.get(&session_id) {
                    hub.warn("limit_returned", said);
                }
                resumed += 1;
            }
        }
        resumed
    }

    /// What this session is holding the daemon up for - the per-harness half of
    /// [`Sessions::idle_exit_refusal`]. Its own function so the two readers answer with one
    /// body
    fn court_refusal(&self, session_id: &str) -> Option<String> {
        let h = self.open.get(session_id)?;
        if let Some(park) = h.limit_park() {
            return Some(format!(
                "{session_id} is parked until its provider's limit lifts ({})",
                park.model
            ));
        }
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

    /// Give this session a thread of its own, if it has one to serve and none yet. Idempotent
    /// because three doors reach it: the daemon's `Work::Open`, the first pass after
    /// `serve_each_on_its_own_thread`, and any door that finds an open session with no court.
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
        // The session is handed over on its own channel, not captured, so a thread that could
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
                // Said, and the session keeps the only reader it had: its turns run on the
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
        // Marked before the session is handed over, so there is no instant in which the worker
        // could take this session's command as well as the thread. A command that lands between this
        // line and the thread's first pass waits in the hub's queue, and the thread takes it.
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

    /// This session's own `Sessions`, for the thread that will serve it. Everything per-session
    /// MOVES: the harness, its clocks, the seat slot, the plugin host, the `!` state. Two
    /// instances holding one session would be two answers to *who runs this turn*.
    fn court_of(&mut self, session_id: &str) -> Option<Sessions<'static>> {
        let id = session_id.to_string();
        let harness = self.open.remove(&id)?;
        let mut court = Sessions {
            // OWNED, not borrowed: this thread outlives the `&Parts` the daemon was handed.
            // A clone is three `Arc`s - the vocabulary and the dialect renderer are not copied.
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
            nag: self.nag,
            sweep_at: None,
            park_at: None,
            // The daemon's own clocks are the daemon's. A court with a review clock or an
            // idle exit would be a second daemon deciding to leave - and the review pass reads
            // the queue, which is the daemon's.
            reviews_at: None,
            // A court keeps this clock, unlike the review one: the board the room's rows go on
            // is the harness's, and a court's harness is here. Due at once, like the daemon's.
            room_at: Some(Instant::now()),
            idle_exit_at: None,
            busy_seen: Instant::now(),
            idle_held: None,
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

    /// One session's harness, asked rather than borrowed. `harness_of` hands out a reference
    /// which a session served on its own thread cannot: the harness is on that thread. So a caller
    /// says what it wants with a closure and gets the same answer either way.
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

    /// The `flowy` tool for a root session - always, as a door - and, when the
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
        // The title is the session's short address on the fabric - `@seat/title` beside `@seat/id`
        // - so agents on one project can name each other. From the entry
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
        // A verb a session's own thread must answer is asked of it - the harness it acts on is
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
            // A verb this daemon has not got: its `command_<verb>` layer is asked first, and the
            // sentence an unknown verb has always answered stands when no layer takes the line (X23).
            Slash::Unknown { verb, line } => {
                let host = self.plugins.get(session_id).cloned();
                let hub = self.registry.get(session_id);
                crate::slash::unknown_verb(host.as_deref(), hub.as_ref(), session_id, &verb, &line)
            }
            Slash::Gate(verb) => crate::slash::gate(self.base.store.as_deref(), &verb),
            // A fresh conversation for this session - the operator's *"clear context - fresh reseat
            // without loosing session todos and jobs"*.
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
            // §6's standalone prune: no summary turn, so the verb runs with no model behind it -
            // the one reduction that works at a wall with no room to write into.
            Slash::Prune => {
                let h = match self.harness(session_id) {
                    Ok(h) => h,
                    Err(e) => {
                        return SlashReply {
                            lines: vec![e.to_string()],
                            ok: false,
                        };
                    }
                };
                match h.prune() {
                    Ok(r) => SlashReply {
                        lines: vec![crate::harness::prune_reply(&r)],
                        ok: true,
                    },
                    Err(e) => SlashReply {
                        lines: vec![e.to_string()],
                        ok: false,
                    },
                }
            }
            // The merge queue's person-verbs.
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
                // Scoped to this session's repository unless the line names another. `regate` resolves
                // its own repository (it was already the session's by default); a named scope stands in
                // for its `/REPO`. A session of a one-org project folder acts in each of its repositories
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
                // `session_id` is passed to `approve` and not to the others, because it is the one act
                // that writes a decider onto a row: the review's `session_id` becomes the session the
                // person decided in, and the pane draws that field as *attach to it to read the argument*.
                let said = match verb {
                    QueueVerb::Restart { entry } => h.restart_review(&entry, repo),
                    QueueVerb::Approve { entry } => h.approve_review(&entry, session_id, repo),
                    QueueVerb::Veto { entry, why } => h.veto_review(&entry, &why, repo),
                    QueueVerb::Rm { entry } => h.remove_merge_entry(&entry, repo),
                    // The two sweeps take an id or none, and none is the whole queue - the
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
            // The wall controller, by hand - see [`Slash::AutoCompact`].
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
            // The pause is the harness's (its config, its row, its hub)
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
            Slash::Listen(words) => match self.open.get_mut(session_id) {
                Some(h) => h.listen_verb(&words),
                None => SlashReply {
                    lines: vec![format!("session {session_id} is not open")],
                    ok: false,
                },
            },
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
            Slash::Boundary => match self.open.get(session_id) {
                Some(h) => SlashReply {
                    lines: h.boundary_lines(),
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
            // Switches this session and nothing else. The standing choice is
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
                // The act itself is the harness's - `Harness::apply_model_choice`, the ONE
                // implementation of this verb. The harness's own slash door is its other caller.
                h.apply_model_choice(choice, lines)
            }
        }
    }

    /// A seat arrives while sessions are open - `/flowy login` from a head. Every open root
    /// session is attached: its slot filled, its `flowy` monitor declared, its wake armed, the
    /// shelf installed, the fabric block appended as a system update.
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

    /// The fabric for a root session's system prompt, and its provenance into the disclosure. A
    /// session that is not seated gets neither, and the disclosure says `fabric: OFF` when a
    /// seat exists.
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

    /// What the session was given of the fabric, recorded once it is open - the block
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
                     tool's `artifact` action or its CLI, when its seat has either",
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
    /// it as a system update. Called after a compaction. Returns whether
    /// an update went in.
    pub fn refresh_fabric(&mut self, session_id: &str) -> Result<bool, HarnessError> {
        // The fabric block is a system update, which is the harness's - so it is asked of the
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

    /// Declare the session's `flowy` monitor - continuous, owned by the session scope, renewed
    /// by the seat's loop - and arm the wake. The operator's words: *"I don't want to fiddle
    /// with monitors."* The model never declares this one; it finds it in `job_list`.
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
                hub.warn(
                    "flowy_not_seated",
                    format!(
                        "seat `{}` is attached to this session but its backend cannot hold \
                         a monitor, so nothing said on the fabric can wake it. The `flowy` \
                         tool still speaks; `flowy status` shows what is pending.",
                        seat.name()
                    ),
                );
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
                    hub.warn(
                        "flowy_not_seated",
                        format!("the `flowy` monitor could not be declared: {e}"),
                    );
                }
            }
        }
    }

    /// What a session is attached to, from the daemon's own command line - and the `model` here
    /// is WHO ANSWERS, not what the vocabulary binds. `cfg.model` is the local BINDING (the
    /// alias whose tokenizer renders the prompt)
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

    /// Open a session now, rather than on its first prompt. This is what a `ResumeSession`
    /// turns into
    pub fn open(&mut self, session_id: &str) -> Result<bool, HarnessError> {
        self.open_as(session_id, None)
    }

    /// One session this daemon holds, by id - for a test that has to ask a session something no
    /// door of the daemon's exposes. `#[cfg(test)]` and `pub(crate)` for the reason
    /// [`Harness::set_subagents_for_test`] is
    #[cfg(test)]
    pub(crate) fn held(&mut self, session_id: &str) -> Option<&mut Harness> {
        self.open.get_mut(session_id)
    }

    /// [`Sessions::open`] with the two facts a session the DAEMON ITSELF starts needs - the door
    /// [`Sessions::ensure_gatekeeper`] uses, because the daemon's own `--role` and `--title` are the
    /// OPERATOR's conversation's and the review queue's session is not that conversation.
    fn open_as(&mut self, session_id: &str, own: Option<OwnSession>) -> Result<bool, HarnessError> {
        // A session served by its own thread IS open - its harness is there, not here - so an
        // open for it is the `Work::Open` its creation rang, arriving after the thread took it.
        if self.open.contains_key(session_id) || self.courts.contains_key(session_id) {
            // An open session with no thread yet gets one here. The daemon's own first session
            // is the case: `open_first` opened it before there was a worker to hand it to, and its
            // creation rang the `Work::Open` that lands on this line.
            self.spawn_court(session_id);
            return Ok(false);
        }
        let hub = self.registry.get(session_id);
        // The daemon's own session, held as a fact and not as a value: `own` is the seat, the
        // title and the prompt for a session the daemon starts for itself, and it is consumed by
        // the open below while the origin it implies is still needed afterwards.
        let own_session = own.is_some();
        match self.harness_as(session_id, own) {
            Ok(h) => {
                if let Some(r) = h.resumed() {
                    let r = r.clone();
                    if let Some(hub) = &hub {
                        for note in &r.notes {
                            hub.warn("resume_note", note.clone());
                        }
                    }
                }
                if let Some(hub) = &hub {
                    for note in h.open_notes() {
                        hub.warn("open_note", note.clone());
                    }
                }
                self.publish_title(session_id);
                // And who this session is for - written now that its row exists. Three answers
                self.registry.record_origin(
                    session_id,
                    if own_session {
                        SessionOrigin::Queue
                    } else if self.base.one_shot {
                        SessionOrigin::OneShot
                    } else if self.base.parent_seat.is_some() {
                        // A seat's child: grouped with the packages, never in the operator's list.
                        SessionOrigin::Package
                    } else {
                        self.registry.origin_of(session_id)
                    },
                );
                // A RESUMED SESSION ARMS ITS OWN CLOCK, because nothing else will
                self.rearm_todo_nag(session_id);
                if self.registry.parent_of(session_id).is_some() {
                    self.arm_park();
                }
                // And the thread that will serve it from now on, handed everything above.
                self.spawn_court(session_id);
                Ok(true)
            }
            Err(e) => {
                if let Some(hub) = &hub {
                    hub.warn("resume_failed", e.to_string());
                }
                Err(e)
            }
        }
    }

    /// What a session was rebuilt from, for the daemon's own banner.
    pub fn resume_report(&self, session_id: &str) -> Option<crate::harness::ResumeReport> {
        self.open.get(session_id).and_then(|h| h.resumed().cloned())
    }

    /// What opening a NEW session decided that the operator should hear - the mode
    /// the project store chose over the flag, for one. Empty for a resumed session.
    pub fn open_notes(&self, session_id: &str) -> Vec<String> {
        self.open
            .get(session_id)
            .map(|h| h.open_notes().to_vec())
            .unwrap_or_default()
    }

    /// Move a title the harness derived into the registry, where a picker reads it. The harness holds
    /// a `Hub` and not a `Registry` - deliberately, so a `Harness` can be built in a test with nothing
    /// else - so the handover happens at the one layer that holds both.
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

    /// The harness for `session_id`, opening it if a head made the session and nothing has run in it
    /// yet. `Err` is a session that cannot be served at all; the caller announces it on that
    /// session's log rather than taking the daemon down, because every other session is still fine.
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
            // A session live under another owner is not the worker's to open. A subagent's harness
            // lives on the thread its parent spawned it on
            if hub.owner() == Owner::OwnThread {
                return Err(HarnessError::Setup(format!(
                    "session {session_id} is a subagent served by its own thread, so the \
                     daemon's worker will not open a second harness on it; prompt it from a head \
                     attached to it, or message it with `task_message`"
                )));
            }
            // The session's own tree, when the registry has one - a head that made this session named
            // the directory it was standing in, and a resumed one carries the root from the store.
            // `self.base.workspace` is the daemon's command line, which is a fact about the daemon.
            let ws = self.registry.wiring(session_id).workspace;
            // Whether this is somebody's subagent is the registry's fact, not the
            // command line's: a resumed subagent must not be seated as a root.
            // From the entry, not a scan of the store - see `is_root`.
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
            // And the two facts of a session the DAEMON owns, when the caller named one. Set on the
            // SESSION's config and never on `self.base`
            let mut prompt = None;
            if let Some(own) = own {
                cfg.seat = own.seat;
                // The title is on the config and not only in the registry, which is what makes
                // the session's own STORE row carry it: a name that lived only in memory would come
                // back `(unnamed)` in `--sessions` after every restart.
                cfg.title = own.title;
                prompt = own.prompt;
            }
            let (tool, cond) = self.seat_tool(session_id);
            // The system prompt is composed at open, for the tools the seat actually holds - the
            // same rule as the first session - and the paragraph only THIS session is given goes
            // after the notes, part of the same frozen prefix.
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
                    // The seat's bookkeeping for a session that did not open goes with it
                    // : `seat_tool` inserted the slot and the `seated` condition before
                    // the open could fail, and a retry inserts them again.
                    self.slots.remove(session_id);
                    self.seated.remove(session_id);
                    return Err(e);
                }
            };
            self.fabric_opened(session_id, &h);
            // The lazy open is the resume a head reaches - `/resume`, a switch from the picker
            // a subagent's parent coming back - so a session this daemon did not start running still
            // comes back on its own model rather than the CLI default.
            h.restore_provider_choice();
            // And the stood-down compaction with it - the pair on the row is the guard's
            // finding, and a session this daemon adopts must not come back with it unrecorded.
            h.restore_auto_compact();
            // The operator's run state is born with the harness - one `Arc`, held by this map
            // and cloned into the harness, so the dispatch arm and the round boundary's pickup
            // answer "is a run going?" from one place. See `crate::bangrun`.
            let bang = self
                .bangs
                .entry(session_id.to_string())
                .or_insert_with(crate::bangrun::State::new)
                .clone();
            h.set_bang_state(bang);
            // What a daemon that is gone left running is brought back, or reminded, as the
            // session is opened - before the ring below, so the wake carries the `[restart]`
            // notice. See `crate::restart`.
            h.restore_after_restart();
            self.open.insert(session_id.to_string(), h);
            self.declare_flowy_monitor(session_id, cond);
            // A SESSION THAT HAS JUST BEEN BUILT IS PUT TO WORK, WHICH ARMING IS NOT.
            self.put_a_restored_session_to_work(session_id);
            self.open_plugins(session_id, crate::plugins::Inherited::none());
        }
        Ok(self.open.get_mut(session_id).expect("just inserted"))
    }

    /// Sessions with a harness behind them. The rest exist and have never run.
    pub fn opened(&self) -> usize {
        // A session on its own thread is open - its harness is there, not here - so counting only
        // `self.open` would answer *nothing runs* about a daemon that is running everything.
        self.open.len() + self.courts.len()
    }

    /// The plugin layers `session_id` has, discovered once, when it opens. At open rather than
    /// on the first pane because of C4's own shape. `inherited` is what a spawned child takes from
    /// its parent and a session the daemon started itself takes from nothing
    /// (`PluginHost::for_child`).
    fn open_plugins(&mut self, session_id: &str, inherited: crate::plugins::Inherited) {
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
        // The daemon's session list, handed to the host - a pane on disk that declares the
        // `sessions` feed is drawn from it (`docs/plugins-plan.md`). The registry is the one
        // holder of *every* session, live and on disk, so it is the whole source.
        let host = crate::plugins::PluginHost::for_child(session_id, &workspace, inherited)
            .with_sessions(Arc::new(crate::plugins::feed::RegistryFeed::new(
                self.registry.clone(),
            )));
        // And the session's own connection to the box's room, so a room pane is drawn from it and
        // its four controls have somewhere to go.
        let host = match self.open.get(session_id).and_then(|h| h.room_link()) {
            Some(link) => host.with_room(link),
            None => host,
        };
        // And the daemon's GitHub reviewer seat (R118), so `/pane github/github` draws the scopes
        // `letibot attach github <scope>` asked the daemon to hold.
        let host = match self.registry.github() {
            Some(seat) => host.with_github(Arc::new(crate::plugins::feed::SeatFeed(seat))),
            None => host,
        };
        let host = Arc::new(host);
        host.announce(&hub);
        let driver: Arc<dyn letibot_sessionlog::registry::PaneDriver> = host.clone();
        self.registry.set_pane_driver(session_id, driver);
        // And the `plugin` tool asks the same host. It was seated when the harness opened before
        // this - a hub and a pane driver exist only here - so it holds a cell and this is the only
        // place that fills one.
        if let Some(h) = self.open.get_mut(session_id) {
            let slot = h.plugin_slot();
            *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(host.clone());
            // X21: `lifecycle_open`, or `lifecycle_reseat` for a resume - asked HERE, where the
            // session's layers are in force for the first time. `Harness::open` ran before this
            // session had a stack, so nothing at open could have answered.
            h.announce_lifecycle();
            // And the GitHub panel's `review_verdict` floor (K5): the panel is the daemon's and the
            // stack is a session's, so the first one to open hands it over - a review hook is not a
            // session's, so every session of this daemon answers the project's policy alike.
            if self.registry.review_floor().is_none() {
                self.registry
                    .set_review_floor(Arc::new(crate::plugins::ReviewFloorHooks::new(
                        slot,
                        hub.clone(),
                    )));
            }
        }
        self.plugins.insert(session_id.to_string(), host);
    }

    /// The plugin tick, run from the daemon's idle pass and never blocking it: the hot-reload
    /// poll (which hands a changed layer to a thread), the panes whose `every_s` has come round
    /// and the notices a layer owes this session's model.
    pub fn plugin_tick(&mut self) {
        // A session on its own thread ticks its own layers, on the clock that thread keeps
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
                if let Some(h) = self.open.get_mut(&id)
                    && let Err(e) = h.note_to_model(&notice)
                {
                    eprintln!("  {id} · a plugin notice could not be written: {e}");
                }
            }
            if let Some(hub) = self.registry.get(&id) {
                host.tick(hub);
            }
        }
    }

    /// When the plugin tick next has something to do: the earliest hot-reload poll or pane refresh
    /// over every session this daemon holds. `None` for a daemon whose sessions have no layers
    /// which is every daemon without plugins - so this adds no wake to a box that has none.
    fn plugins_due_at(&self) -> Option<Instant> {
        self.plugins.values().filter_map(|h| h.next_at()).min()
    }

    /// Close every held session's backend and say what each delivered - the daemon's last act.
    /// A placement hands work back only when its backend closes, and sessions on their own threads keep theirs ([`Court`]), so both are asked. The only door `--stop` and the idle exit have.
    pub fn close_backends(&self) -> Vec<String> {
        // The daemon is going, so everything it supervises goes too - the same rule a session's
        // own close keeps, applied to every session this daemon holds. `--stop` reaches here; a
        // crash does not, which is why the supervisor's restart path is what cleans up after one.
        for id in self.open.keys().chain(self.courts.keys()) {
            crate::supervise::teardown_session(id, "the daemon stopping");
        }
        // Every block of ports this daemon's sessions held goes back (RH9) - before the
        // backends close, because a session's placement is what a remote teardown's ssh runs over.
        if let Some(at) = self.base.store_at() {
            for id in self.open.keys().chain(self.courts.keys()) {
                for line in crate::leases::release_session(&at, id, "the daemon stopping") {
                    eprintln!("  {id} · lease: {line}");
                }
            }
        }
        let mut said: Vec<String> = self
            .open
            .values()
            .filter_map(|h| h.close_backend())
            .collect();
        for (id, court) in &self.courts {
            let seen = id.clone();
            // The sentence comes before the thread leaves: its harness is dropped the moment this
            // session's own `finish` job is served, and then there is nothing left to settle.
            match court.ask_within(SETTLE_PATIENCE, move |s: &mut Sessions<'static>| {
                s.open.get(&seen).and_then(|h| h.close_backend())
            }) {
                Some(Some(line)) => said.push(line),
                // A session with no placement says nothing; one that could not be asked at all is
                // said, because a settlement nobody waited for is otherwise a silence.
                Some(None) => {}
                None => {
                    let why = match &court.thread {
                        Some(t) if !t.is_finished() => {
                            format!("did not answer within {} s", SETTLE_PATIENCE.as_secs())
                        }
                        _ => "had already gone".to_string(),
                    };
                    said.push(format!(
                        "{id} · its placement was NOT settled here: the session's own thread \
                         {why}, so what it committed is still wherever its tools ran"
                    ));
                }
            }
        }
        said
    }

    /// Stop every session's subagents and wait for their threads to leave - the daemon's exit
    /// path, and the door the process's own exit needs before it runs `_dl_fini`. A child's
    /// thread is spawned detached, so nothing can be joined to it
    pub fn drain_subagents(&self, timeout: std::time::Duration) -> SubagentExit {
        let began = std::time::Instant::now();
        let mut told = 0;
        let mut left = Vec::new();
        // The sessions on their own threads first. Each is asked to stop its children and wait
        // for them, and answers from the only thread that may drive its harness - so a session
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
                    // `told` counts sessions that had something to stop - a session with nothing
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
        // And the threads themselves, which the process's exit needs gone. `exit` runs every
        // ELF finalizer, and `_dl_fini` tears the libraries down underneath a thread still inside
        // them. `join` cannot be used from `&self`, so this is the same bound read as a poll.
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
        // The sessions this struct holds itself - every session of a daemon that never armed
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

    /// Submit one prompt to a session and wait for the answer - the scripted path (`--prompt …`)
    /// which has no head and no socket traffic. It goes through the same `harness` as the worker, so
    /// a scripted run and a driven one open a session the same way.
    pub fn submit(&mut self, session_id: &str, text: &str) -> Result<Reply, HarnessError> {
        // A session on its own thread answers there - `cli.rs`'s scripted steps are the caller
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

    /// One turn, and everything that has to happen around one. Both ways a prompt reaches a
    /// session go through here, so there is one copy of this sequence, with the automatic
    /// compaction - the path every interactive session takes.
    fn run_prompt(
        &mut self,
        session_id: &str,
        text: &str,
        attachments: &[letibot_sessionlog::protocol::Attachment],
    ) -> Result<Reply, HarnessError> {
        // Opened first, lazily - the resume a head reaches (`/resume`, a switch from the
        // picker). A session that cannot be opened ran nothing, and says so on its own log.
        if let Err(e) = self.harness(session_id) {
            self.warn(
                session_id,
                "session_unavailable",
                format!("this session could not be opened, so nothing was run: {e}"),
            );
            return Err(e);
        }
        // Everything around the turn is the shared tail's : the plan-check earned back the wall
        // checked BEFORE the send, the clock, the failure, the compaction, the continuation past
        // the wall, the title, and the wake.
        self.turn(session_id, Door::Prompt, |h| {
            h.submit_attached(text, attachments).map(Some)
        })
        .expect("a prompt always runs a turn")
    }

    /// A message to a session the daemon holds, between that session's turns - the row it is
    /// and the turn it makes: *"my commands should start a turn and should be printed to me"*.
    /// `Ignored` here would be the message sitting in the transcript until something else ran.
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

    /// One turn of a session the worker holds, through the shared tail - every door of
    /// `Sessions` comes here. `None` is a door that found nothing to say.
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
        // A child this worker holds - a parked child a parent's message reopened - runs its
        // later turns here, and its record moves for the turn's length exactly as it does on a
        // thread (`harness::later_turn`).
        let record = h
            .config()
            .parent_session_id
            .as_ref()
            .and(h.config().store_at())
            .map(|at| crate::subagent_record::SubagentRecord {
                store: crate::subagent_record::RunStore {
                    at,
                    cell: Default::default(),
                },
                child: session_id.to_string(),
                registry: self.registry.clone(),
                pending: None,
            });
        let prior = record.as_ref().and_then(|r| r.later_turn_began());
        // The parent's slot, settled from here: this turn is not on the child's thread, so the two acts it makes (`harness::later_turn`) are made from the parent's side.
        // Whether it had settled is read before the turn, since a turn of the hold is part of the answer, not news.
        let parent = h.config().parent_session_id.clone();
        let a_child_turn = parent.as_ref().map(|parent| {
            let settled_before = self
                .parts
                .trees
                .of(parent)
                .and_then(|tree| tree.slot(session_id))
                .is_some_and(|slot| slot.has_settled());
            (parent.clone(), settled_before)
        });
        let somebody_spoke = matches!(
            door,
            crate::turn_tail::Door::Prompt
                | crate::turn_tail::Door::Message
                | crate::turn_tail::Door::Wake
        );
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
            // The answer of THIS turn is recorded where a later one is on a child's own thread: the
            // row already says what the first turn said, and a restart reads the row.
            if let Some(Ok(reply)) = &out
                && !reply.text.trim().is_empty()
            {
                r.answered_again(reply.text.trim());
            }
            r.later_turn_ended(prior);
        }
        if let Some((parent, settled_before)) = &a_child_turn {
            crate::harness::a_child_turn_ended(
                &self.parts.trees,
                parent,
                session_id,
                somebody_spoke,
                *settled_before,
                &out,
            );
        }
        self.open.insert(session_id.to_string(), h);
        out
    }

    /// Start (or stand down) the idle plan-check for SESSION, as of NOW. An unfinished plan is
    /// always armed: a notice not yet sent comes due one window after the turn that armed it
    /// and one already sent at the doubled interval [`TodoNagClock::rearm`] keeps.
    fn rearm_todo_nag(&mut self, session_id: &str) {
        let (rows, jobs, hold, news) = match self.open.get(session_id) {
            Some(h) => {
                let rows = h.todo_list();
                let jobs = h.running_jobs(&rows);
                (rows, jobs, h.nag_hold(), h.queue_news())
            }
            None => (
                Vec::new(),
                RunningJobs::none(),
                NagHold::Running,
                QueueNews::none(),
            ),
        };
        self.rearm_todo_nag_from(session_id, rows, jobs, hold, news);
    }

    /// [`Sessions::rearm_todo_nag`] from a harness in hand - the shared tail's door, which holds
    /// the session's harness out of `open` while it runs.
    fn rearm_todo_nag_on(&mut self, session_id: &str, h: &Harness) {
        let rows = h.todo_list();
        let jobs = h.running_jobs(&rows);
        self.rearm_todo_nag_from(session_id, rows, jobs, h.nag_hold(), h.queue_news());
    }

    /// [`Sessions::rearm_todo_nag`] for a push that ADDED an operator row - the clock's
    /// [`TodoNagClock::operator_added`] in place of its plain re-arm, the inputs read the same way.
    fn rearm_todo_nag_after_an_operator_add(&mut self, session_id: &str) {
        let Some(h) = self.open.get(session_id) else {
            return;
        };
        let rows = h.todo_list();
        let jobs = h.running_jobs(&rows);
        let hold = h.nag_hold();
        let news = h.queue_news();
        let children = child_sessions(
            &self.registry,
            &self.parts.tasks,
            letibot_sessionlog::registry::now_ms(),
        );
        let timings = self.nag;
        let clock = self
            .nags
            .entry(session_id.to_string())
            .or_insert_with(|| TodoNagClock::with_timings(timings));
        clock.hold(hold);
        clock.queue(news);
        clock.operator_added(&rows, Instant::now(), &children, &jobs);
    }

    fn rearm_todo_nag_from(
        &mut self,
        session_id: &str,
        rows: Vec<TodoItem>,
        jobs: RunningJobs,
        hold: NagHold,
        news: QueueNews,
    ) {
        let children = child_sessions(
            &self.registry,
            &self.parts.tasks,
            letibot_sessionlog::registry::now_ms(),
        );
        let timings = self.nag;
        let clock = self
            .nags
            .entry(session_id.to_string())
            .or_insert_with(|| TodoNagClock::with_timings(timings));
        clock.hold(hold);
        clock.queue(news);
        clock.rearm(&rows, Instant::now(), &children, &jobs);
    }

    /// The operator has spoken to SESSION: a new idle period, and the check may be made again - the
    /// half of the rule they stated as a negative, *"certainly not after my message"*. Without it a
    /// plan rewritten by their own instruction would never be checked at all.
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

    /// A session this daemon has just RESTORED is put to work, not left to be woken.
    fn put_a_restored_session_to_work(&self, session_id: &str) {
        self.registry.bell().ring_wake(session_id);
    }

    /// When the next check comes due, for the worker's blocking wait - `None` when no session
    /// has one armed, which is what lets the worker sleep until there is real work. The sweep
    /// needs its own deadline and cannot rely on a nag
    pub fn next_nag_at(&self) -> Option<Instant> {
        let nags = self
            .nags
            .values()
            .filter_map(|c| c.due_at())
            .chain(self.nags.values().filter_map(|c| c.census_due_at()))
            .min();
        match (nags, self.sweep_at) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) => Some(a),
            (None, b) => b,
        }
    }

    /// When the worker must next wake with nothing to do - the earliest of the plan-check, the
    /// abandoned-call sweep and the store read for the reviews this daemon's sessions are owed.
    /// The review clock is armed from open and never stands down, by design
    pub fn next_idle_at(&self) -> Option<Instant> {
        [
            self.next_nag_at(),
            self.reviews_at,
            // Only when some session here has a room or is waiting for one to appear: the pass
            // is what links a session to a coordinator that started after it.
            self.has_a_room().then_some(self.room_at).flatten(),
            self.park_at,
            self.next_limit_at(),
            self.idle_exit_at,
            self.plugins_due_at(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// The daemon loop's deadline: [`Sessions::next_idle_at`], and due at once while the idle exit
    /// is unarmed. Only an idle pass arms it, so a daemon with no other clock would otherwise
    /// wait with no deadline and never leave. A court never arms it, so this is the daemon's alone.
    pub fn daemon_idle_at(&self) -> Option<Instant> {
        if self.base.idle_exit_after.is_some() && self.idle_exit_at.is_none() {
            return Some(Instant::now());
        }
        let exe = self
            .base
            .idle_exit_after
            .map(|_| Instant::now() + crate::exe_stamp::CHECK);
        [self.next_idle_at(), exe].into_iter().flatten().min()
    }

    /// Whether this daemon should leave now because its executable was replaced and nothing holds
    /// it. Only a daemon with an idle window: one that never idles out, the room's, keeps running.
    pub fn exe_replaced_exit_due(&self) -> Option<std::path::PathBuf> {
        self.base.idle_exit_after?;
        let path = crate::exe_stamp::replaced()?;
        self.idle_exit_refusal().is_none().then_some(path)
    }

    /// When the earliest limit clock falls due, as an `Instant` from the harnesses' own clocks: the
    /// park that resumes a session, and the switch back a limit owes. `None` when nothing is
    /// limited, which is the common case and the one that arms no deadline at all.
    fn next_limit_at(&self) -> Option<Instant> {
        let now = Instant::now();
        self.open
            .values()
            .filter_map(|h| {
                let clock = h.limit_now_ms();
                let due = [h.limit_park_until(), h.limit_return_at()]
                    .into_iter()
                    .flatten()
                    .min()?;
                Some(now + Duration::from_millis(due.saturating_sub(clock)))
            })
            .min()
    }

    /// Whether any session this `Sessions` holds speaks in the box's room - the one question
    /// [`Sessions::next_idle_at`] asks before it makes the room clock a reason to wake. A court holds
    /// one harness, so this is exact there; it is `false` on a box with no coordinator.
    pub fn has_a_room(&self) -> bool {
        self.open.values().any(|h| h.has_room() || h.wants_room())
    }

    /// Whether the room pass is due, spending the deadline when it is -
    /// [`Sessions::reviews_due`]'s shape and for its reason: the disarm is before the work, so a
    /// pass that took longer than the interval does not fire again at once.
    pub fn room_due(&mut self) -> bool {
        let now = Instant::now();
        if self.room_at.is_some_and(|at| at > now) {
            return false;
        }
        self.room_at = Some(now + crate::room::remind::ROOM_PASS_EVERY);
        true
    }

    /// The room's reminders, onto every session this `Sessions` holds - one row per item a
    /// session holds, so the idle check carries the reminder and no model call is needed.
    pub fn fill_room_boards(&mut self) -> usize {
        if !self.room_due() {
            return 0;
        }
        let mut rows = 0;
        let mut linked = Vec::new();
        for (id, harness) in self.open.iter_mut() {
            let wanted = harness.wants_room();
            rows += harness.fill_room_board();
            if wanted && let Some(link) = harness.room_link() {
                linked.push((id.clone(), link));
            }
        }
        // A session that linked just now has a plugin host built without a room.
        for (id, link) in linked {
            if let (Some(host), Some(hub)) = (self.plugins.get(&id), self.registry.get(&id)) {
                host.link_room(link, &hub);
            }
        }
        rows
    }

    /// Whether the store read for owed reviews is due, spending the deadline when it is -
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

    /// Every review owed to a session this daemon holds, served from the store. The queue's
    /// ring is per-daemon, so a mover in another daemon rings one nobody is standing at and a
    /// daemon restarted between the ask and the wake has nothing left to ring.
    pub fn serve_reviews(&mut self) -> usize {
        // The pass runs where the queue's session is. That session has a thread of its own, so the
        // daemon POSTS the pass rather than running it: a gatekeeper mid-turn would otherwise hold
        // the daemon's loop for the length of that turn.
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

    /// The id of the review-queue session this daemon owns - `s-gatekeeper-<workspace key>`, for
    /// the log line that tells the operator what to attach to. Computed once at construction; the
    /// field carries why the id is per workspace and not one string for the box.
    pub fn review_session(&self) -> &str {
        &self.gatekeeper
    }

    /// The review queue's own session, started once - the daemon's hardcoded gatekeeper session
    /// whose board is the queue. `Ok(true)` is the pass that started it. Once, by two checks
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
        // Where its reviewers will run, decided HERE because the session has to be TOLD.
        //
        // And a `review_brief` layer's paragraph for the queue itself (X18), asked of the daemon's
        // own session: review hooks are no session-scope layer's, so what comes back is the project's.
        let shaped = self
            .open
            .get(&self.base.session_id)
            .and_then(|h| h.plugin_host().map(|host| (host, h.hub().clone())))
            .and_then(|(host, hub)| host.review_queue_brief(&hub));
        let prompt = gatekeeper::prompt_with(
            &gatekeeper::reviewer_placement(
                self.base.reviewers == crate::config::Reviewers::Vm,
                letibot_tools::firecode::firecode_available(),
            ),
            &gatekeeper::room_handle(&self.base.workspace),
            shaped.as_ref(),
        );
        if self.registry.get(&queue).is_none() {
            // Resumed, or made, and the store is what decides which. The stored row carries the
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
        // Already in the registry - a head opened it, or a previous pass did - so the name is the
        // one it already carries. From the registry's entry, not `list`
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

    /// The queue, onto the gatekeeper session's board - one row per entry waiting for a verdict
    /// read from the store and SET (not merged) on the session's own half.
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
            // The session did not open - said on the daemon's log by the caller of
            // `ensure_gatekeeper`. Nothing is lost: the next pass fills the board.
            None => return 0,
        };
        self.rearm_todo_nag(&queue);
        rows
    }

    /// Ask for a sweep a moment from now, so the idle arm gets a chance to look. A short grace
    /// rather than none: `dispatch` returns when the turn is over, and a sweep in the same
    /// instant would be looking at a transcript whose last append may not be published yet.
    pub fn arm_sweep(&mut self, after: std::time::Duration) {
        let at = Instant::now() + after;
        self.sweep_at = Some(match self.sweep_at {
            Some(prev) => prev.min(at),
            None => at,
        });
    }

    /// When the abandoned-call sweep is due, or `None` when nothing has armed it - the one
    /// clock in [`Sessions::next_nag_at`] that a turn's door owes, read on its own so a test
    /// can ask whether a door armed IT rather than whether some clock is armed (the plan-check
    pub fn sweep_due_at(&self) -> Option<Instant> {
        self.sweep_at
    }

    /// The window every session's idle plan-check is made with, from now on - the tests' door: a
    /// delivery has to be drivable without sleeping through [`TODO_NAG_AFTER`]. Clocks that already
    /// exist take the new window, and every session is re-armed with it.
    pub fn set_nag_window(&mut self, window: Duration) {
        self.nag.window = window;
        // And every session's own thread, which keeps its own clock. A court made after this
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

    /// The census's repeat, from now on - the tests' door, as [`Sessions::set_nag_window`] is
    /// for the plan's window and [`TodoNagClock::set_repeat`] is one level down. A test that
    /// waited two hours for the second delivery would not be a test.
    pub fn set_stuck_repeat(&mut self, repeat: Duration) {
        self.nag.census_repeat = repeat;
        let courted: Vec<String> = self.courts.keys().cloned().collect();
        for id in courted {
            if let Some(court) = self.courts.get(&id) {
                court.post(move |s: &mut Sessions<'static>| s.set_stuck_repeat(repeat));
            }
        }
        for clock in self.nags.values_mut() {
            clock.set_repeat(repeat);
        }
        let ids: Vec<String> = self.open.keys().cloned().collect();
        for id in ids {
            self.rearm_todo_nag(&id);
        }
    }

    /// When this session's census is next due - the tests' door to ONE clock, for
    /// [`Sessions::sweep_due_at`]'s reason: since the census became a TURN every delivery arms the
    /// sweep, so `next_nag_at` answers `Some` for ever and cannot say whether the census is armed.
    pub fn census_due_at(&self, session_id: &str) -> Option<Instant> {
        self.nags.get(session_id).and_then(|c| c.census_due_at())
    }

    /// Sweep every session for a tool call nothing will ever answer. Called from the daemon's
    /// own idle arm, and that placement is the whole liveness test
    pub fn sweep_abandoned_calls(&mut self) -> usize {
        // The sweep is the thread that owns the harness's to run, and that placement is the
        // liveness test: a round in flight holds the thread, so a sweep that runs at all is a
        // sweep at a moment when no round owns a call.
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

    /// Close one session this worker holds: its harness, every per-session table and its registry entry.
    /// `None` for a session this worker does not hold, else where the work went - see [`Sessions::close_backends`].
    pub fn close(&mut self, session_id: &str) -> Option<String> {
        // A closed session takes no turn, so the room notes queued for one are for nobody, and a
        // queue nothing removes is the memory of every session this daemon ever held.
        let untaken = crate::room::wake::forget(session_id);
        if untaken > 0 {
            eprintln!("  {session_id} · closed; {untaken} untaken room note(s) dropped with it");
        }
        // The panes this session held go first, and they are the one thing here a head can see:
        // the operator typed `!term` into them, and a program still drawing into a pane no session
        // can open again is one nobody can end.
        let panes = self
            .registry
            .terminal()
            .map(|driver| driver.end_session(session_id))
            .unwrap_or(0);
        // What this session started goes with it (RH7's supervisor): every process it owns, on
        // every host, ended and recorded. A TURN ending does not come near this: a job started to
        // outlive its turn is what `background: true` means.
        let ended = crate::supervise::teardown_session(session_id, "the session closing");
        if ended > 0 {
            eprintln!("  {session_id} · closed; {ended} supervised process(es) ended with it");
        }
        // And the blocks of ports it held go back (RH9), with what it made under its prefixes
        // removed and nothing else. After the processes, because a role's containers are what its
        // processes started.
        if let Some(at) = self.base.store_at() {
            for line in crate::leases::release_session(&at, session_id, "the session closing") {
                eprintln!("  {session_id} · lease: {line}");
            }
        }
        // A session on its own thread is closed THERE: the harness, its backend and its
        // children are on that thread, and closing them from here would be the second writer this
        // whole change exists to remove.
        if self.courts.contains_key(session_id) {
            let id = session_id.to_string();
            let closed = self
                .courts
                .get(session_id)
                .and_then(|court| court.ask(move |s: &mut Sessions<'static>| s.close(&id)));
            // The court's own close took the registry entry with it, which is what an answer
            // means; a thread that is already gone answers nothing, and the entry goes the same way
            // here so a hub is never left open behind a session nobody serves.
            self.registry.remove(session_id);
            // And its thread leaves before this returns.
            if let Some(Court { jobs, thread, .. }) = self.courts.remove(session_id) {
                drop(jobs);
                if let Some(thread) = thread {
                    let _ = wait_for_threads(&[thread], COURT_TEARDOWN);
                }
            }
            // A court that answered is the close; one whose thread had already gone still closed
            // the session here, and says so rather than claiming where the work went.
            return close_report(
                match closed {
                    Some(said) => Some(said.unwrap_or_default()),
                    None => Some(
                        "its own thread had already gone, so where its work went was not said"
                            .into(),
                    ),
                },
                panes,
            );
        }
        let mut held = self.open.remove(session_id);
        // The tree table keeps a session's watcher set and slots for as long as it is open, and a
        // session nobody serves has neither: a settlement naming it has nowhere to go.
        self.parts.trees.forget(session_id);
        // X21's `close`, before the harness and its layer stack go: the last event a layer hears
        // about this session, and the one that can still write a row or a sentence for whoever
        // resumes it. Nothing a layer answers can keep the session open.
        if let Some(h) = held.as_mut() {
            h.lifecycle("close", Default::default());
        }
        let said = match &held {
            Some(h) => {
                h.stop_children();
                h.close_backend()
            }
            None => None,
        };
        self.slots.remove(session_id);
        self.seated.remove(session_id);
        self.fabric_seen.remove(session_id);
        self.nags.remove(session_id);
        self.bangs.remove(session_id);
        self.armed.remove(session_id);
        // And the plugin host, which nothing else removed. `open_plugins` installs one per session
        // and `close` removes it, or the layer stack, its wasm
        // instances and the panes it drew would be held for every session ever opened.
        self.plugins.remove(session_id);
        self.registry.remove(session_id);
        close_report(held.as_ref().map(|_| said.unwrap_or_default()), panes)
    }

    /// Arm the park clock for the subagents this worker holds, a window from now - called at
    /// every turn's end, so the clock measures idleness. Nothing to arm when the policy is off.
    fn arm_park(&mut self) {
        let Some(window) = self.base.child_park_after else {
            return;
        };
        let at = Instant::now() + window;
        self.park_at = Some(self.park_at.map_or(at, |prev| prev.min(at)));
    }

    /// Park every subagent this worker holds that has been idle for the window - 's idle-child
    /// policy (`Config::child_park_after`), on the worker's side. The thread-side half is
    /// `harness::serve_child`
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

    /// What keeps this daemon up, or `None` when nothing does - the idle exit's test, asked of
    /// each fact's owner rather than remembered: no head attached and no turn in flight in any
    /// session
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
        // And the sessions on their own threads, as they last reported themselves. Read rather
        // than asked, because asking would mean waiting for a turn that may be minutes long - and
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

    /// The idle exit's deadline as the clock is armed for it, `None` until the first look arms one.
    /// A fixture's door: it measures a window from a moment it took itself.
    pub fn idle_exit_deadline(&self) -> Option<Instant> {
        self.idle_exit_at
    }

    /// Whether the daemon has been idle for its whole window - and so should exit - spending
    /// the deadline as [`Sessions::reviews_due`] does. `false` before the deadline, and `false`
    /// with the clock re-armed whenever something still holds it
    pub fn idle_exit_due(&mut self) -> bool {
        let Some(window) = self.base.idle_exit_after else {
            self.idle_exit_at = None;
            return false;
        };
        let now = Instant::now();
        match self.idle_exit_at {
            // The first look ARMS the window and counts as finding the daemon busy, which is the
            // truth of it: this call is the first moment anyone has said there is nothing to do.
            None => {
                self.busy_seen = now;
                self.idle_exit_at = Some(now + window);
                return false;
            }
            Some(at) if at > now => return false,
            Some(_) => {}
        }
        let held = self.idle_exit_refusal();
        if held != self.idle_held {
            match &held {
                Some(why) => eprintln!("  idle exit held: {why}"),
                None => eprintln!("  idle exit no longer held"),
            }
            self.idle_held = held.clone();
        }
        if held.is_some() {
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

    /// Send every check whose deadline has passed, and answer how many ran. A check is a TURN
    /// so it goes through the same tail every other turn does. And it arms what the other two
    /// doors arm
    pub fn deliver_due_nags(&mut self) -> usize {
        let mut ran = self.ask_courts(|s| s.deliver_due_nags());
        let now = Instant::now();
        let due: Vec<String> = self
            .nags
            .iter_mut()
            .filter_map(|(id, clock)| clock.due_now(now).then(|| id.clone()))
            .collect();
        for session_id in due {
            // A paused session's deadline is the pause's END, not a check (`/nag pause 2h`): the
            // pause is lifted here, said on the hub, and the clock re-armed as if the operator had
            // typed `/nag resume` - no turn.
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
                // : the wording moves from one delivery to the next.
                Some(h) => h.nag_notice().map(|_| h.nag_said_as()),
                None => continue,
            };
            // The clock's own numbers, written before the turn: a `nag_` layer decides on the same
            // facts the clock does, rather than on a copy the harness invented.
            let readings = self.the_nag_readings(&session_id);
            if let Some(h) = self.open.get_mut(&session_id) {
                h.nag_readings(readings);
            }
            // Nothing to say after all - the plan moved between the arming and the clock - is no
            // turn, and a door that runs no turn owes no sweep.
            let Some(notice) = notice else {
                continue;
            };
            // The shared tail arms the sweep and the wake, records the notice as said and
            // re-arms the clock.
            match self.turn(&session_id, Door::Nag(notice), |h| h.nag_turn()) {
                Some(Ok(_)) => ran += 1,
                Some(Err(e)) => eprintln!("  {session_id} · todo check -> {e}"),
                // The layer held the check back, or said nothing after all. The deadline was spent
                // by `due_now`, so this door owes the clock an arming - at the interval the layer
                // asked for, clamped, or at the clock's own from the plan as it stands.
                None => self.arm_a_quiet_nag(&session_id),
            }
        }
        ran
    }

    /// The plan-check's clock as the daemon reads it for SESSION: the numbers the clock decides on,
    /// plus the one idle reading it cannot know - how long the session has been quiet.
    fn the_nag_readings(&self, session_id: &str) -> letibot_plugin::nag::Clock {
        let now = Instant::now();
        let mut clock = match self.nags.get(session_id) {
            Some(clock) => clock.readings(now),
            None => letibot_plugin::nag::Clock {
                floor_ms: self.nag.window.as_millis() as u64,
                cap_ms: self.nag.cap.as_millis() as u64,
                ..letibot_plugin::nag::Clock::default()
            },
        };
        clock.idle_ms = self.registry.get(session_id).map_or(0, |h| {
            let now_ms = letibot_sessionlog::registry::now_ms();
            now_ms.saturating_sub(h.status().last_ms)
        });
        clock
    }

    /// Re-arm SESSION's check after a delivery that ran no turn, honouring the interval a `nag_`
    /// layer asked for - clamped here, where the bounds live.
    fn arm_a_quiet_nag(&mut self, session_id: &str) {
        let next = self.open.get(session_id).and_then(|h| h.nag_next());
        self.rearm_todo_nag(session_id);
        if let (Some(next), Some(clock)) = (next, self.nags.get_mut(session_id)) {
            clock.arm_in(the_next_check(next, self.nag), Instant::now());
        }
    }

    /// Read-only access to a session's harness, for a caller that wants the ledger
    /// or the prefix. `None` for a session nothing has run in yet.
    pub fn harness_of(&self, session_id: &str) -> Option<&Harness> {
        self.open.get(session_id)
    }

    /// Say the queue's census to every project session whose clock has come due - as a TURN
    /// which is what makes it a bell rather than a note. It was a transcript row and cost no
    /// model call
    pub fn deliver_due_censuses(&mut self) -> usize {
        let mut ran = self.ask_courts(|s| s.deliver_due_censuses());
        let now = Instant::now();
        let due: Vec<String> = self
            .nags
            .iter_mut()
            .filter_map(|(id, clock)| clock.census_due_now(now).then(|| id.clone()))
            .collect();
        for session_id in due {
            // A clock with no harness behind it is skipped, not reported: `court_of` MOVES a
            // court-served session's clock onto its own thread, so this is the leftover case - and a
            // turn on a session nothing holds answers `Err`, a line at every pass.
            if !self.open.contains_key(&session_id) {
                continue;
            }
            // The sentence is composed here, from the store, at the moment it is said - so a
            // row that recovered between the arming and the clock is not announced as stuck, and
            // `Ok(None)` is that answer: no row, no turn, no model call.
            let out = self.turn(&session_id, Door::Census, |h| match h.stuck_census_text() {
                Some(text) => h.census_turn(&text).map(Some),
                None => Ok(None),
            });
            match out {
                Some(Ok(_)) => ran += 1,
                Some(Err(e)) => eprintln!("  {session_id} · the queue census -> {e}"),
                // A door that ran no turn owes the clock a re-arm, and nothing else will make one: the
                // deadline was spent by `census_due_now`, and a turn's end is what otherwise re-arms it.
                None => self.rearm_todo_nag(&session_id),
            }
        }
        ran
    }

    /// Compact a session: the summary turn, then the transcript fork. The same shape as
    /// [`Sessions::submit`], because a compaction is a turn with a fork behind it. No title derivation
    /// and no wake-arming: a summary proposes nothing and monitors do not fire on it.
    pub fn compact(&mut self, session_id: &str) -> Result<CompactReport, HarnessError> {
        // A compaction is a turn, so it runs where the harness is.
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
                // The fabric may have moved since the session opened; after a compaction is the
                // moment to say so, as an update and never as a rewrite of message 0.
                if let Err(e) = self.refresh_fabric(session_id)
                    && let Some(hub) = &hub
                {
                    hub.warn("fabric_refresh_failed", e.to_string());
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

    /// Arm the monitor wake for a session that has declared one - the *"wakes the loop when
    /// it fires"*, which had no caller. A thread and not a poll.
    pub fn arm_wake(&mut self, session_id: &str) {
        let Some(mut h) = self.open.remove(session_id) else {
            return;
        };
        self.arm_wake_on(session_id, &mut h);
        self.open.insert(session_id.to_string(), h);
    }

    /// [`Sessions::arm_wake`] with the harness in hand - the shared tail's door. The thread itself
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

    /// Something fired while nothing was running. The worker's half of the wake.
    /// `Outcome::Ignored` is the honest answer to a wake that raced a mid-turn pickup
    pub fn wake(&mut self, session_id: &str) -> Outcome {
        // A session on its own thread is woken THERE: `open` does not hold it, so `wake_route`
        // already answers `ItsOwnReader` - this is only the spawn, for a session that is open here and
        // has no thread yet.
        self.spawn_court(session_id);
        let hub = self.registry.get(session_id);
        // The decision is `wake_route`'s, so it can be asserted without a daemon: `open` is this
        // struct's fact and the registry is the daemon's.
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
            WakeRoute::Gone => {
                self.warn(session_id, "wake_undelivered", dropped_wake(session_id));
                eprintln!("  {} · {}", session_id, dropped_wake(session_id));
                return Outcome::Ignored;
            }
        }
        // A wake's turn is a turn - it appends rows and can reach the wall - so it takes the
        // shared tail like every other door, which arms the sweep and the monitor wake too.
        // `None` is a wake with nothing to say: nothing ran and nothing grew.
        match self.turn(session_id, Door::Wake, |h| h.wake()) {
            None => Outcome::Ignored,
            Some(Ok(reply)) => Outcome::Replied(Box::new(reply)),
            Some(Err(e)) => Outcome::Failed(e.to_string()),
        }
    }

    // --------------------------------------------------------------------------- The
    // operator's own run, off the worker. See `crate::bangrun` for the shape these are the
    // worker's hands on the state it owns.

    /// A failure on the session's own log - the hub every attached head draws - for the arms
    /// whose only other witness is the daemon's stderr. It goes through [`Hub::warn`], the one door
    /// every other warning site uses, so the daemon's half and the harness's cannot drift.
    fn warn(&self, session_id: &str, code: &str, detail: String) {
        if let Some(hub) = self.registry.get(session_id) {
            hub.warn(code, detail);
        }
    }

    /// The operator's run could not have a thread of its own, so it runs on the worker -
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

    /// The session was not open, said where the asker reads - the sentence `Outcome::Failed`
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

    /// The serialization state for `session_id`'s operator runs - created with the harness, so a
    /// session this map holds always has one.
    fn bang_state(&self, session_id: &str) -> Option<Arc<crate::bangrun::State>> {
        self.bangs.get(session_id).cloned()
    }

    /// Whether an operator run for this session is in flight on its own thread.
    fn bang_run_in_flight(&self, session_id: &str) -> bool {
        self.bang_state(session_id).is_some_and(|s| s.in_flight())
    }

    /// Park a line typed while a run was in flight; it starts when that run settles. Only called
    /// after [`Self::bang_run_in_flight`] answered true, and both run on the worker's thread.
    fn queue_bang_line(&self, session_id: &str, line: String, who: String) {
        if let Some(s) = self.bang_state(session_id) {
            s.queue(line, who);
        }
    }

    /// The run settled - clear the flag first, then take what waited with
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

    /// Hand the operator's run to a thread of its own.
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
        let (call_id, call, runtime, watch) = {
            let h = self
                .open
                .get_mut(session_id)
                .ok_or_else(|| format!("session {session_id} is not open"))?;
            let (call_id, call) = h.prepare_operator_shell(line)?;
            // The watcher travels with the run, because the run is what mints the handle: a line
            // moved to the background mid-flight settles as `Backgrounded`, and a thread holding a
            // bare null sink arms nothing - the row promises a completion nobody would queue.
            (call_id, call, h.runtime_handle(), h.job_watchers())
        };
        state.start();
        if let Err(why) = crate::bangrun::spawn_run(crate::bangrun::OperatorRun {
            session: session_id.to_string(),
            call_id,
            call,
            line: line.to_string(),
            who: who.to_string(),
            runtime,
            hub,
            watch,
        }) {
            state.end();
            return Err(why);
        }
        Ok(())
    }

    /// The old arm, kept for the case that cannot hand off: the thread could not be started, so
    /// the run happens here, on the worker, exactly as it always did - the rows, then the turn
    /// its rows are for. This is the behaviour whose cost the whole change exists to end
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
        // And the turn its rows are for - the same sending half the settle arm runs
        // because the operator asked for their command's result to reach the model. Through the
        // shared tail, so the wake and the sweep run.
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

    /// The court that serves this session, if one does - giving it one first when the session is
    /// open and has none. A session opened by a door older than the threads has no court; without
    /// this it would go on running its turns on the daemon's worker for ever.
    fn court_serving(&mut self, session_id: &str) -> Option<&Court> {
        if !self.courts.contains_key(session_id) {
            self.spawn_court(session_id);
        }
        self.courts.get(session_id)
    }

    /// Ask every session on its own thread the same question, and add up the answers. A court's
    /// `open` holds exactly its one session, so the per-session body answers for it. `Copy` because
    /// every court is asked the same question.
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

    /// Put a command back on the queue of the thread that owns the session - unless this IS
    /// that thread. Both readers take from ONE queue, so the worker can lose a race it should
    /// not answer the interrupt a parent aimed at its child, and the message a peer sent it.
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
        // A session served on its own thread runs its own commands.
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
                // One line, because everything a turn needs around it - the title, the wake, the failure
                // notice, the compaction at the wall - is in `run_prompt`, which the scripted path also
                // calls. One copy, so the compaction is never missed.
                match self.run_prompt(session_id, text, attachments) {
                    Ok(reply) => Outcome::Replied(Box::new(reply)),
                    Err(e) => Outcome::Failed(e.to_string()),
                }
            }
            // A compaction is a whole-session act and runs through [`Sessions::compact`], as a
            // prompt runs through [`Sessions::submit`] - which also opens its harness.
            CommandKind::Compact => {
                // Measured before and after, as the automatic door is this arm read the session's pair
                // AFTER the fork and converted both sides through it, naming no unit, so a manual
                // `/compact` told the head a byte count where the automatic one said deepseek tokens.
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
                        // A compaction that re-seated says so: it forks onto the prompt this
                        // daemon seats now, and a capability that appears silently is one nobody
                        // uses.
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
                            hub.warn("reseated", said);
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
                            hub.warn_with(
                                "compacted",
                                compaction_said(&r, scales.clone()),
                                Some(Box::new(compaction_wire(
                                    &r,
                                    "compacted",
                                    scales,
                                    resident,
                                    window,
                                    headroom,
                                ))),
                            );
                        }
                        Outcome::Compacted(Box::new(r))
                    }
                    Err(e) => Outcome::Failed(e.to_string()),
                }
            }
            // A re-seat is a compaction that lands on a different prompt, so it runs through the
            // same door and reports through the same one. What it adds is the sentence naming which
            // tools the model can call now that it could not before - the reason anybody types this.
            CommandKind::Reseat { summarise } => {
                // Two ways to change message zero: `reseat` pays a summary turn, `reingest`
                // carries the conversation across and pays a cold prefill instead.
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
                            // A summarising re-seat is a compaction, so it carries the structure
                            // too: the record it produced is the base every later turn reads. A bare
                            // re-seat summarised nothing and carries none.
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
                                    record_wall(&c).0,
                                    record_wall(&c).1,
                                ))
                            });
                            hub.warn_with("reseated", said, compaction);
                        }
                        // And the model is told, in a turn of its own: the person's act reached only the head, and
                        // the operator asked for the symmetric half - *"that reseat happened shall reach the model
                        // too. and it should start the turn"*.
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
                            hub.warn("reseat_refused", e.to_string());
                        }
                        Outcome::Failed(e.to_string())
                    }
                }
            }
            CommandKind::Interrupt { reason } => {
                // A stop delivered to a session the daemon does not hold belongs to the thread that owns
                // it.
                if self.hand_back_to_the_owner(session_id, &hub, cmd) {
                    return Outcome::HandedOn;
                }
                // AND THE SUBTREE GOES FIRST, whichever session this is. A stop is a stop
                // whether or not a turn is running: the children are stopped before it, so nothing
                // is left computing for nobody - *"we talk to parents and they own lifecycle"*.
                if let Some(h) = self.open.get_mut(session_id) {
                    h.stop_children();
                }
                if let Some(hub) = &hub {
                    hub.warn(
                        "interrupt_idle",
                        format!(
                            "interrupt ({reason}) arrived between turns; nothing was generating"
                        ),
                    );
                }
                Outcome::Ignored
            }
            // A take-back reaching the worker means the turn ended before the steering poll saw it
            // whatever prompts it named have run or are about to. Quietly nothing - a warning here
            // would be noise about a no-op.
            CommandKind::SetOperatorTodos { items, moved } => {
                let converted: Vec<letibot_tokencore::store::TodoItem> = items
                    .iter()
                    .map(|e| letibot_tokencore::store::TodoItem {
                        content: e.content.clone(),
                        // The wire's condition, as the STORE spells it - the other half of
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
                        // The edges now CROSS this door rather than dying at it: `set_operator` REPLACES this
                        // half, so a row the operator owns would lose every edge it had on the head's next push.
                        // The `match` makes a kind added on one side a compile error on the other.
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
                                // The job handle crosses verbatim, the same rule the child's id
                                // keeps: it is what `job_output` and `job_kill` are given.
                                letibot_sessionlog::event::TodoNeed::Job { handle } => {
                                    letibot_tokencore::store::TodoNeed::Job {
                                        handle: handle.clone(),
                                    }
                                }
                                letibot_sessionlog::event::TodoNeed::Pid { pid, start } => {
                                    letibot_tokencore::store::TodoNeed::Pid {
                                        pid: *pid,
                                        start: *start,
                                    }
                                }
                                letibot_sessionlog::event::TodoNeed::Monitor { name } => {
                                    letibot_tokencore::store::TodoNeed::Monitor {
                                        name: name.clone(),
                                    }
                                }
                                // The room's item number crosses verbatim - it is the name
                                // `room`'s own verbs take, and a head that dropped it would hand
                                // the daemon back a board with the reminder's edge missing.
                                letibot_sessionlog::event::TodoNeed::Room { item, working } => {
                                    letibot_tokencore::store::TodoNeed::Room {
                                        item: *item,
                                        working: *working,
                                    }
                                }
                                // The operator's question, and their answer, cross whole - the
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
                                    ask,
                                } => letibot_tokencore::store::TodoNeed::Answered {
                                    question: question.clone(),
                                    reply: reply.clone(),
                                    ask: *ask,
                                },
                                // An ask crosses whole, id and words alike: the head echoes its
                                // rows back, and an edge it dropped would release a row the peer is
                                // still being nagged about.
                                letibot_sessionlog::event::TodoNeed::Ask {
                                    ask,
                                    of,
                                    room,
                                    question,
                                } => letibot_tokencore::store::TodoNeed::Ask {
                                    ask: *ask,
                                    of: of.clone(),
                                    room: room.clone(),
                                    question: question.clone(),
                                },
                                letibot_sessionlog::event::TodoNeed::Owed { ask } => {
                                    letibot_tokencore::store::TodoNeed::Owed { ask: *ask }
                                }
                                // The words cross verbatim: they are all the edge has, and a head
                                // that dropped them would hand back a row that waits on nothing.
                                letibot_sessionlog::event::TodoNeed::External { what } => {
                                    letibot_tokencore::store::TodoNeed::External {
                                        what: what.clone(),
                                    }
                                }
                                letibot_sessionlog::event::TodoNeed::Unknown => {
                                    letibot_tokencore::store::TodoNeed::Unknown
                                }
                            })
                            .collect(),
                    })
                    .collect();
                // The head is the source of truth for its own rows, so its list REPLACES that half.
                let n = converted.len();
                // And the state moves it asked for on rows that are not its own - `/todo
                // postpone|resume <the row's words>` on a row the MODEL wrote, which is the one
                // thing the operator can change about a row they did not write.
                let states: Vec<(String, letibot_tokencore::store::TodoStatus)> = moved
                    .iter()
                    .map(|s| (s.content.clone(), status_from_the_wire(s.status)))
                    .collect();
                // The board before the write, so an ADD can be told from a re-push : the
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
                // ARMING HERE IS THE WHOLE POINT OF A ROW THE OPERATOR ADDS. The clock must not start in
                // one place only - the end of a turn - or a row added to an IDLE session is never
                // checked. It is the same arm the turn end makes, and an ADD is more than a moved board.
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
            // The operator answered a row's question, and the session is between turns - so the
            // answer is applied and the model is told AT ONCE, as a turn of its own
            // . A refusal runs nothing and is said to the heads.
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
            // The request is honoured mid-turn by the `bash` wait loop, which reads the hub's promote
            // channel. Reaching here means nothing was running, so the request is stale - clear it and
            // say so rather than leaving it to promote itself unprompted.
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
                        // Pending until the result arrives, keyed by the HEAD that asked - so a
                        // head that dies between the two frames leaves a sentence rather than a silent
                        // admission. A daemon-run call has one for the length of this arm.
                        if let Some(hub) = &hub {
                            hub.note_operator_call(&call_id, &cmd.head_id, &name, &who);
                        }
                        // : the daemon runs it, when the head asked it to. The head has no
                        // tool runtime and no HTTP client; the daemon has both, and the payload then
                        // comes from *this* program, bounded by this session's caps and spill policy.
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
            // The operator's shell line, run by this daemon and recorded as two rows (`! ls.` → their
            // `User` row + a `bash` `ToolResult` with `origin: Operator`). Between turns it runs on a
            // thread of its own - a run that held THIS worker held every other session's work with it.
            CommandKind::LoadSkill { name, then } => {
                let (name, then) = (name.clone(), then.clone());
                match self.turn(session_id, Door::Prompt, |h| {
                    h.load_skill(&name, &then).map(Some)
                }) {
                    None => Outcome::Ignored,
                    Some(Ok(reply)) => Outcome::Replied(Box::new(reply)),
                    Some(Err(why)) => {
                        let said = why.to_string();
                        self.warn(session_id, "slash_refused", said.clone());
                        Outcome::Failed(said)
                    }
                }
            }
            CommandKind::OperatorShell { line, who } => {
                let (line, who) = (line.clone(), who.clone());
                if !self.open.contains_key(session_id) {
                    return Outcome::Failed(self.not_open(session_id, &format!("`{line}`")));
                }
                // One operator run per session at a time - `Prompts` holds ONE input handle, so a second
                // concurrent run would steal the first's card and `!send`. A line typed while one is in
                // flight waits HERE and starts when it settles
                if self.bang_run_in_flight(session_id) {
                    self.queue_bang_line(session_id, line, who);
                    return Outcome::HandedOn;
                }
                match self.start_operator_run(session_id, &line, &who) {
                    Ok(()) => Outcome::HandedOn,
                    // The thread could not be started. The command is not refused for that: it runs HERE
                    // synchronously, exactly as this arm always did - the old behaviour, with the reason on
                    // the log rather than a silent loss of the operator's line.
                    Err(why) => {
                        self.said_on_the_worker(session_id, &line, &why);
                        self.run_operator_shell_here(session_id, &line, &who)
                    }
                }
            }
            // What the operator's run produced, back from its own thread. The rows and the turn are
            // appended HERE, by the worker - one writer of the transcript - exactly as they always
            // were; only the wait moved off this thread. See `crate::bangrun` for the handoff's shape.
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
                // The run is over, whether or not its rows could be written - so the flag comes down
                // FIRST, before anything here can return.
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
                // The next queued line starts now - not when some later command happens to
                // come through, and not never because this one's rows failed. One per settle
                // the one started here settles in its own turn, and drains the next.
                let next = self.pop_queued_bang_line(session_id);
                if let Some((next_line, next_who)) = next
                    && let Err(why) = self.start_operator_run(session_id, &next_line, &next_who)
                {
                    self.said_on_the_worker(session_id, &next_line, &why);
                    // Same fallback as the dispatch arm above, and for the same reason: a queued line is not
                    // lost because a thread could not be started. It runs here, between turns, where it would
                    // have waited anyway.
                    let ran = self.run_operator_shell_here(session_id, &next_line, &next_who);
                    if let Outcome::Failed(e) = ran {
                        return Outcome::Failed(e);
                    }
                }
                if let Err(e) = settled {
                    return Outcome::Failed(e);
                }
                // And the turn its rows are for - the operator's correction, in their words: *"my commands
                // should start a turn and should be printed to me"*. The rows above are the printing; this
                // is the turn. `Ignored` here was the whole defect
                if !self.open.contains_key(session_id) {
                    return Outcome::Failed(
                        self.not_open(session_id, "the turn after your `!` line"),
                    );
                }
                // A `!` turn is a turn: it appends rows, it can reach the wall, and a
                // session that only ever ran shell lines would have compacted never - the
                // shared tail every door takes.
                match self.turn(session_id, Door::OperatorShell, |h| {
                    h.run_after_operator_shell().map(Some)
                }) {
                    None => Outcome::Ignored,
                    Some(Ok(reply)) => Outcome::Replied(Box::new(reply)),
                    Some(Err(e)) => Outcome::Failed(e.to_string()),
                }
            }
            // What it produced. The row goes in with its `origin` set, so every head
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
            // A message for a session the daemon does not hold belongs to the thread that owns it -
            // the same rule, and the same door, as the interrupt arm above. Since the between-turns
            // fix a message to a child is no longer *"a turn in flight or nothing"*
            CommandKind::Message { from, text } => {
                if self.hand_back_to_the_owner(session_id, &hub, cmd) {
                    return Outcome::HandedOn;
                }
                // The session the WORKER owns: a row the agent reads, and the turn it makes. The
                // operator's ruling for their own lines is the rule here too - *"my commands should start
                // a turn and should be printed to me"* - and a message is that pair of acts
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
                // and has no reader - its hub is closed, or there is none - where the sentence
                // below is true.
                if let Some(hub) = &hub {
                    let said = match text.chars().count() > 200 {
                        true => format!("{}…", text.chars().take(200).collect::<String>()),
                        false => text.clone(),
                    };
                    hub.warn(
                        "message_idle",
                        format!(
                            "a message from `{from}` could not be delivered: `{session_id}` is \
                             gone, so nothing will read it — the daemon holds no session of that \
                             id and the hub it had is closed: {said}. The sender's `task_message` \
                             was accepted and did not land."
                        ),
                    );
                }
                Outcome::Ignored
            }
            CommandKind::Promote => {
                if let Some(hub) = &hub {
                    // A run of the operator's is a call in flight that this worker is NOT holding. Its own
                    // thread's `bash` wait loop is the designed consumer of the promote flag - it knows the
                    // job id and can move the cgroup.
                    if self.bang_run_in_flight(session_id) {
                        hub.warn(
                            "promote_in_flight",
                            "a background request arrived while the operator's run is in \
                             flight on its own thread; its wait loop takes it and moves the run",
                        );
                        return Outcome::Ignored;
                    }
                    // `None` means a wait loop already honoured it. A promote sent during a turn is taken by
                    // that turn's own `bash` wait loop, which promotes the call and settles it as
                    // `Backgrounded`; this command then reaches the worker only after the turn has ended.
                    if hub.take_promote_request().is_some() {
                        hub.warn(
                            "promote_idle",
                            "a background request arrived between turns; nothing was running to move",
                        );
                    }
                }
                Outcome::Ignored
            }
            // An answer that got this far had nowhere better to go. A session with an adjudicator
            // installs an `AnswerSink`, and `Hub::submit` then delivers straight to the thread waiting
            // for it rather than queueing - see `crate::answers` for why the queue is a deadlock.
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
                    hub.warn("answer_unclaimed", detail);
                }
                Outcome::Ignored
            }
            CommandKind::Slash { line } => {
                // A `/clear` aimed at a session its own thread owns is that thread's - the interrupt arm's
                // rule and door.
                if matches!(
                    crate::slash::Slash::parse(line),
                    crate::slash::Slash::Clear { .. }
                        | crate::slash::Slash::Prune
                        | crate::slash::Slash::Nag(_)
                ) && let Some(hub) = &hub
                    && hub.owner() == Owner::OwnThread
                    && hub.give_back_to_its_own_reader(cmd.clone())
                {
                    return Outcome::HandedOn;
                }
                let line = line.clone();
                let reply = self.slash(session_id, &line);
                if let Some(hub) = &hub {
                    hub.warn(
                        if reply.ok { "slash" } else { "slash_refused" },
                        format!("/{line}\n{}", reply.lines.join("\n")),
                    );
                }
                if reply.ok {
                    Outcome::Ignored
                } else {
                    Outcome::Failed(format!("/{line} was refused"))
                }
            }
            // A read of a job's output, for the pane that asked. The same read `/job ID` does
            // answered with the offsets beside the text instead of folded into a footer sentence - the
            // jobs pane draws its own header and its own paging.
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
                        // A refused read is a `Warning`, the same as a refused slash
                        // and the code says which read it was: the pane renders it as
                        // a line rather than a window.
                        Err(e) => hub.warn("job_output_refused", e),
                    };
                }
                Outcome::Ignored
            }
            // A head ASKS for the point that needs a person (`PROTOCOL_VERSION` 54, B8), and a person
            // ANSWERS it. Two arms rather than one because they come from different heads and mean
            // different things
            CommandKind::ModeConfirm { name } => {
                let said = match self.harness(session_id) {
                    Ok(h) => h.ask_mode_confirm(
                        name,
                        &cmd.head_id,
                        &cmd.identity,
                        &cmd.client_request_id,
                    ),
                    // A session that cannot be opened is a session that cannot be asked
                    // about, and the words the open failed with are the answer.
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
                            hub.warn("mode_unknown", e);
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
                // A consented `allow-all` IS written - with the answer that opens it. It is
                // otherwise left out of the row, for a good reason.
                let consented_here =
                    *consented && mode.name == letibot_tools::mode::Mode::ALLOW_ALL.name;
                // The row, and then the answer - in that order, through one lock, so a reader of the file
                // never sees a consent for a row that is not there.
                if let Err(e) = (|| -> std::io::Result<()> {
                    let mut store = self.parts.mode_store.write().unwrap();
                    store.set(&workspace, mode)?;
                    if consented_here {
                        store.set_consent(&workspace, &letibot_flowy::seat::rfc3339_now())?;
                    }
                    Ok(())
                })() {
                    if let Some(hub) = &hub {
                        hub.warn(
                            "mode_unpersisted",
                            format!(
                                "could not record `{}` for {}: {e}",
                                mode.name,
                                workspace.display()
                            ),
                        );
                    }
                    return Outcome::Failed(format!("persisting mode: {e}"));
                }
                // And this session moves too. The row above is the project's point for every session that
                // opens later
                let moved = match self.open.get_mut(session_id) {
                    // The operator's answer travels with the command, and only this call reads it: the project
                    // row above is written from the NAME, which stays `allow-all` and stays refused for the
                    // next daemon. Consent is for the session in front of them.
                    Some(h) => h
                        .set_mode_consented(mode, *consented)
                        .map(|said| (said, h.config().mode)),
                    None => Err("this session has no harness open".into()),
                };
                // And the session remembers where it was put - its own row, so a restart or a reseat
                // brings it back here whatever the project's row says by then. The requested point's NAME
                // with the answer beside it, as the project row is written
                if moved.is_ok() {
                    let when = consented_here.then(letibot_flowy::seat::rfc3339_now);
                    let store = self.parts.mode_store.read().unwrap();
                    if let Err(e) = store.set_session(session_id, mode, when.as_deref())
                        && let Some(hub) = &hub
                    {
                        hub.warn(
                            "mode_unpersisted",
                            format!("could not record `{}` for this session: {e}", mode.name),
                        );
                    }
                }
                if let Some(hub) = &hub {
                    let _ = match moved {
                        Ok((said, applied)) => hub.warn(
                            "mode_set",
                            // Say what is true of the project separately from what is true of this session, because
                            // they can differ: a point the row carries may still be one this session cannot move to.
                            {
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
                        ),
                        Err(why) => hub.warn(
                            "mode_set_next_session_only",
                            format!(
                                "{} is set to `{}` for every LATER session in this \
                                 project; this one stays where it opened, because it \
                                 cannot carry that point: {why}",
                                workspace.display(),
                                mode.name
                            ),
                        ),
                    };
                }
                Outcome::Ignored
            }
        }
    }
}

/// The daemon's half of the shared turn tail - the [`TurnOwner`] for a session the
/// worker holds. Each method is one step of [`crate::turn_tail::run_turn`]'s sequence, landing
/// where the daemon keeps that step's state; the ORDER is the tail's.
struct DaemonTurn<'s, 'a, 'id> {
    sessions: &'s mut Sessions<'a>,
    session_id: &'id str,
}

impl TurnOwner for DaemonTurn<'_, '_, '_> {
    /// Before the turn, for every door: a turn can strand a call, and only an
    /// idle pass closes it out.
    fn arm_sweep(&mut self) {
        self.sessions.arm_sweep(SWEEP_AFTER);
    }

    /// The operator (or a peer) has spoken: the plan-check is earned back.
    fn spoken_to(&mut self, _h: &mut Harness) {
        self.sessions.note_operator_prompt(self.session_id);
    }

    /// A compaction forked the transcript, so a seated root is told the fabric again when it
    /// changed - what `Sessions::compact` does after a `/compact`, for the compactions the tail
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

    /// Every turn ends here, so this is where the idle clocks start - the plan-check (with a
    /// spent nag recorded as said first: the metronome rule), the queue's census (recorded the
    /// same way, against its own anchor) and, for a subagent the worker holds, its park window.
    fn turn_ended(&mut self, h: &mut Harness, door: &Door) {
        // The interval a `nag_` layer asked for, taken whatever the door is so a stale one cannot
        // arrive with the next turn. Clamped HERE, where the bounds live.
        let next = h
            .nag_next()
            .map(|next| the_next_check(next, self.sessions.nag));
        if let Some(clock) = self.sessions.nags.get_mut(self.session_id) {
            match door {
                Door::Nag(notice) => clock.said(notice.clone(), Instant::now()),
                // The census is recorded by its DELIVERY, not by its words - the sentence names the age of
                // the oldest row, so a text identity would read every delivery as new. Before the re-arm
                // which reads the anchor
                Door::Census => clock.census_said(Instant::now()),
                _ => {}
            }
        }
        self.sessions.rearm_todo_nag_on(self.session_id, h);
        // AFTER the re-arm, which arms the ladder's own deadline: a layer's interval is the one
        // that stands for a delivered check.
        if matches!(door, Door::Nag(_))
            && let (Some(next), Some(clock)) = (next, self.sessions.nags.get_mut(self.session_id))
        {
            clock.arm_in(next, Instant::now());
        }
        if self.sessions.registry.parent_of(self.session_id).is_some() {
            self.sessions.arm_park();
        }
    }
}

/// The store, as somewhere a registry can find sessions it is not holding. A handle onto
/// the process's one connection to the store file (`Store::open`), behind a `Mutex` - not a
/// connection of its own.
pub struct StoreSessions {
    store: Mutex<Store>,
    wiring: SessionWiring,
}

impl StoreSessions {
    /// `None` when the daemon has no store: there is then nothing on disk to list
    /// and a source that answered "no sessions" would be indistinguishable from a
    /// store that is empty.
    pub fn open(cfg: &Config) -> Option<Arc<StoreSessions>> {
        // The daemon's own first open of the store, so a fresh data directory's two files are
        // made here; a head or a listing goes through `run_query`, which creates nothing.
        let store = cfg.store_at()?.open().ok()?;
        Some(Arc::new(StoreSessions {
            store: Mutex::new(store),
            wiring: Sessions::wiring(cfg),
        }))
    }
}

/// The same handle, answering a row the view has trimmed. One instance does both jobs
/// because they are one store asked two questions, and because another handle would be
/// another reader of the same file.
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
        // The ordinal is relative to the CURRENT transcript. After a fork - a
        // compaction, a reseat - ordinal 3 is a row of the new base, so reading the old
        // transcript's row 3 would answer with a row nobody asked about.
        let transcript_id = store.current_transcript_id(session_id).ok()??;
        let json = store.row_json_at(&transcript_id, seq).ok()??;
        // The row's own type decides what its body is, and that match lives once - in
        // `letibot_sessionlog::body_of`, which the view's own reader calls too. Two copies
        // is how the two tiers would come to disagree about what a row's `body` is.
        let item: letibot_transcript::TranscriptItem = serde_json::from_str(&json).ok()?;
        Some(letibot_sessionlog::body_of(&item))
    }

    fn row_items(
        &self,
        session_id: &str,
        from: usize,
        count: usize,
    ) -> Vec<(usize, letibot_sessionlog::view::SnapshotItem)> {
        let (Ok(from), Ok(count)) = (u32::try_from(from), u32::try_from(count)) else {
            return Vec::new();
        };
        let Ok(store) = self.store.lock() else {
            return Vec::new();
        };
        // The current transcript, for `row_body`'s reason: an ordinal names a row of the base
        // the conversation is on now, not of the one a fork replaced.
        let Some(transcript_id) = store.current_transcript_id(session_id).ok().flatten() else {
            return Vec::new();
        };
        let rows = store
            .rows_json_from(&transcript_id, from, count)
            .unwrap_or_default();
        rows.into_iter()
            .filter_map(|(seq, item_id, kind, json, created_at)| {
                // A row whose JSON this build cannot read is left out rather than answered as
                // empty: the page is short, and the row's own `seq` travels with the ones that
                // follow it so a skipped row does not slide the page.
                let item: letibot_transcript::TranscriptItem = serde_json::from_str(&json).ok()?;
                Some((
                    seq as usize,
                    letibot_sessionlog::view::SnapshotItem {
                        item_id,
                        kind,
                        // A paging head draws these rows and asks nothing about their ledger.
                        ledger_head: String::new(),
                        ts: created_at,
                        item: Some(item),
                        citations: Vec::new(),
                    },
                ))
            })
            .collect()
    }
}

/// Ledger tokens as the operator's screen counts them. One function for the two callers
/// that render a compaction - the sentence and the structure beside it - because two copies
/// of this arithmetic is how a head comes to print two different numbers for one quantity.
fn shown_tokens(scale: Option<(u64, u64)>) -> impl Fn(usize) -> u64 {
    move |ledger: usize| -> u64 {
        let n = ledger as u64;
        match scale {
            Some((l, p)) if l > 0 && p > 0 => ((n as u128 * p as u128) / l as u128) as u64,
            _ => n,
        }
    }
}

/// The two pairs a compaction's numbers convert through - the history's, and the new
/// base's. One pair must not convert both, or the new base would be read in the HISTORY's units.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Scales {
    pub before: Option<(u64, u64)>,
    pub after: Option<(u64, u64)>,
    pub unit: Option<String>,
    /// The after-count is an estimate - a metered base the provider has not counted
    /// yet. Said as `≈ N, estimated until the next round measures it`; see
    /// `crate::wall` for what judging it as a measurement cost.
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

/// How the summary turn was paid for, said only with what was measured.
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

/// What a compaction tells the head.
pub(crate) fn compaction_said(
    r: &crate::harness::CompactReport,
    scales: impl Into<Scales>,
) -> String {
    let scales = scales.into();
    let shown = shown_tokens(scales.before);
    let shown_after = shown_tokens(scales.after);
    let unit = scales.unit.as_deref().unwrap_or("tokens");
    // An empty summary is a re-ingest, which is the same signal `fork_to_summary` reads to
    // decide what note to write. Nothing was summarised, so "compacted" would be a lie and
    // `was → base` would be one number printed twice.
    if r.summary_turn.summary.is_empty() {
        return format!(
            "re-seated: {} {unit} of conversation carried onto the new prompt as they are, \
             on transcript {}. Nothing was summarised and nothing was dropped.",
            shown_after(r.fork.base_tokens),
            r.fork.transcript_id
        );
    }
    // In the units the operator's header shows; see `Config::shown_tokens`.
    // One sentence carries every fact a head draws (size, what stands in for history, what was kept and dropped), so no count is repeated for a reader to reconcile.
    let (kept, dropped) = compaction_account(r);
    let mark = if scales.estimated { "≈ " } else { "" };
    // Between the facts and the transcript, so the facts stay inside the first clause.
    let estimate = if scales.estimated {
        ", estimated until the next round measures it"
    } else {
        ""
    };
    let mut said = format!(
        "compacted: {} → {mark}{} {unit}, {kept}, {dropped}{estimate}, on transcript {}.",
        shown(r.fork.was_tokens),
        shown_after(r.fork.base_tokens),
        r.fork.transcript_id
    );
    // The consequence, not the count again: a verbatim part that opens inside an exchange
    // answers something that is no longer there.
    if r.fork.tail_dropped.is_some() {
        said.push_str(
            " The verbatim part begins inside an exchange, so its first message answers \
             something that is no longer there.",
        );
    }
    // The fold worked and what it produced is partial, and this report is the only row left to
    // say the second half.
    if r.fork.truncated {
        said.push_str(SUMMARY_CUT_OFF);
    }
    // And WHICH zero, when there is no tail: `tail_why` knows all three reasons.
    if r.fork.tail_items == 0
        && let Some(line) = r.fork.tail_why.as_ref().and_then(|t| t.why_line())
    {
        said.push_str(&format!(" {line}"));
    }
    // What the record kept of the conversation and what it was allowed: a summary is sized by a stated fraction of the room, so the line says the fraction.
    // Both numbers come from the measured outcome, so a head drawing this sentence and the wire report agree.
    if let Some(f) = &r.summary_turn.fidelity {
        let mut line = format!(" The record it wrote carries {}", f.line());
        if let Some(b) = r.summary_turn.budget {
            line.push_str(&format!(
                "; it is {} characters, and {}.",
                r.summary_turn.summary.len(),
                b.said()
            ));
        } else {
            line.push('.');
        }
        said.push_str(&line);
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

/// The sentence for a summary that ran out of room, said by the report and by the daemon's own
/// log line. One copy, because a warning's wording is what a head reads.
pub(crate) const SUMMARY_CUT_OFF: &str = " The summary was CUT OFF at the model's length limit \
                                          — it is incomplete, and the base says so too.";

/// What the compaction kept and what it dropped. A fork that carried no tail says the summary is
/// what it kept, because `kept 0 items` reads as a compaction that kept nothing at all.
fn compaction_account(r: &crate::harness::CompactReport) -> (String, String) {
    let chars = r.summary_turn.summary.chars().count();
    let kept = match r.fork.tail_items {
        0 => format!("kept as a {chars}-character summary"),
        n => format!("kept {n} item(s) verbatim plus a {chars}-character summary"),
    };
    let dropped = match r.fork.tail_dropped {
        Some(n) => format!("{n} item(s) dropped at the seam"),
        None => "nothing dropped".to_string(),
    };
    (kept, dropped)
}

/// The structure behind the sentence - 's `warning.compaction`. The `detail` sentence stays
/// exactly as it was, because it is the fallback every reader that cannot use this lands
/// on
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
        // The record, split by the daemon, not by the head. A missing section here means the model
        // wrote no such heading; an empty body means it wrote the heading and nothing under it.
        // Two facts, and the list is the only shape that keeps them apart.
        sections: letibot_turn::parse_summary_sections(&r.summary_turn.summary)
            .into_iter()
            .map(|(name, body)| CompactionSection { name, body })
            .collect(),
        // Present on every compaction, including a local one, where it is an object with zero
        // turns: the local artefact is the remote one with an empty tail.
        tail: CompactionTail {
            turns: r.fork.tail_turns.clone(),
            carried: r.fork.tail_items as u64,
            because: r.fork.tail_because.clone(),
            dropped: r.fork.tail_dropped.unwrap_or(0) as u64,
        },
        // What the record kept, and the room it was given, when this compaction had a size: the
        // same numbers the sentence above was written from, in the shape a head can draw.
        fidelity: r.summary_turn.fidelity.as_ref().map(|f| {
            letibot_sessionlog::event::CompactionFidelity {
                levels: f
                    .levels
                    .iter()
                    .map(|l| letibot_sessionlog::event::CompactionLevel {
                        name: l.name.to_string(),
                        total: l.total as u64,
                        kept: l.kept as u64,
                        dropped: l.missing.clone(),
                    })
                    .collect(),
            }
        }),
        budget: r
            .summary_turn
            .budget
            .map(|b| letibot_sessionlog::event::CompactionBudget {
                room: b.room,
                share: b.share,
                writable: b.writable,
                tokens: b.tokens,
                by: b.by.to_string(),
            }),
    }
}

/// The window and reserve a compaction RECORD carries: the pair the wall fires on, in the units
/// the window was given in. `planning_window` is in ledger tokens, and converting it back through
/// the ratio is a round trip that loses a token on each side.
fn record_wall(c: &Config) -> (u64, u64) {
    c.shown_wall()
}

impl SessionSource for StoreSessions {
    fn set_title(&self, session_id: &str, title: &str) -> Result<(), String> {
        let g = self.store.lock().unwrap_or_else(|e| e.into_inner());
        g.rename_unique(session_id, title)
            .map_err(|e| e.to_string())
    }

    /// Make it durable who this session is for - see `letibot_sessionlog::SessionOrigin`.
    /// Called twice for a session a head created
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

    /// Every Claude Code seat's board in the same store (`docs/seat-todo.md`), through the one
    /// conversion a session's rows take, so a head draws a seat's row and a session's alike.
    fn seat_boards(&self) -> Vec<letibot_sessionlog::event::SeatBoard> {
        let g = self.store.lock().unwrap_or_else(|e| e.into_inner());
        g.seat_boards()
            .unwrap_or_default()
            .into_iter()
            .map(|(seat, rows)| letibot_sessionlog::event::SeatBoard {
                seat,
                todos: rows
                    .into_iter()
                    .map(crate::harness::Harness::todo_entry)
                    .collect(),
            })
            .collect()
    }

    /// Both queues, whole, for a head's pane - every row, the leases with their heartbeats, the
    /// targets' health and the last moves, through this source's handle onto the one connection
    /// the queues' own threads write with.
    fn queues(&self) -> letibot_sessionlog::event::QueueSnapshot {
        let g = self.store.lock().unwrap_or_else(|e| e.into_inner());
        crate::mergequeue::snapshot(&g)
    }

    /// The asking session's repository, by the queues' own rule - the
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
        // The picker's `waiting on you` mark, read for every session in one pass - the board
        // is the question's one home, so a session nobody has opened since a restart is marked
        // as surely as a live one.
        let waiting = g.sessions_waiting_on_operator().unwrap_or_default();
        // Every child's name, from its record (`subagent_record::recorded_slugs`), so a child
        // listed after a restart is named as it was when it was spawned - and a name derived for a
        // record from before names is unique among its parent's children, as the runner's own
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
            .map(|s| {
                let waits = waiting.get(&s.id).copied().unwrap_or(0);
                let slug = slugs.get(&s.id).cloned().unwrap_or_default();
                self.brief_of(s, waits, slug)
            })
            .collect()
    }

    fn lookup(&self, session_id: &str) -> Option<StoredBrief> {
        let g = self.store.lock().unwrap_or_else(|e| e.into_inner());
        let s = g.session(session_id).ok().flatten()?;
        Some(self.keyed_brief(&g, s))
    }

    fn children_of(&self, parent_id: &str) -> Vec<StoredBrief> {
        let g = self.store.lock().unwrap_or_else(|e| e.into_inner());
        let kids = g.children_of(parent_id).unwrap_or_default();
        // One parent, so one run list names every child's slug.
        let runs = g.subagent_runs_of(parent_id).unwrap_or_default();
        let slugs: std::collections::HashMap<String, String> = runs
            .iter()
            .map(|r| r.child_id.clone())
            .zip(crate::subagent_record::recorded_slugs(&runs))
            .collect();
        kids.into_iter()
            .map(|s| {
                let slug = slugs.get(&s.id).cloned().unwrap_or_default();
                let waits = Self::waits_of(&g, &s.id);
                self.brief_of(s, waits, slug)
            })
            .collect()
    }
}

impl StoreSessions {
    fn waits_of(g: &Store, id: &str) -> usize {
        g.todos(id)
            .map(|t| letibot_tokencore::store::operator_asks(&t).len())
            .unwrap_or(0)
    }

    /// A row with its two off-row facts read by key: the board of this session, and the run
    /// list of its parent.
    fn keyed_brief(&self, g: &Store, s: letibot_tokencore::store::StoredSession) -> StoredBrief {
        let slug = s
            .parent_session_id
            .as_deref()
            .and_then(|p| {
                let runs = g.subagent_runs_of(p).ok()?;
                runs.iter()
                    .map(|r| r.child_id.clone())
                    .zip(crate::subagent_record::recorded_slugs(&runs))
                    .find(|(c, _)| *c == s.id)
                    .map(|(_, slug)| slug)
            })
            .unwrap_or_default();
        let waits = Self::waits_of(g, &s.id);
        self.brief_of(s, waits, slug)
    }

    fn brief_of(
        &self,
        s: letibot_tokencore::store::StoredSession,
        waits: usize,
        slug: String,
    ) -> StoredBrief {
        StoredBrief {
            waits_on_operator: waits,
            slug,
            // Who it is for, off its own row - `None` is a row written before the
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
            // A pause already over is no pause, whoever has yet to say so: a session this daemon is
            // not holding has no clock to end it, and a picker row that went on saying *paused* past
            // its hour would be the stale state this field exists to replace.
            nags_paused: s
                .nags_paused_until
                .map(letibot_sessionlog::NagPause::from_stored)
                .filter(|p| !p.over_at(letibot_sessionlog::registry::now_ms())),
            wiring: SessionWiring {
                // The session's own model and workspace, from its row. The dialect and endpoint are this
                // daemon's - they are not stored per session, and a plausible guess on a picker row is a
                // guess somebody quotes.
                model: s.model_id,
                workspace: s.workspace_root,
                ..self.wiring.clone()
            },
        }
    }
}

/// How a stored conversation's last turn ended, from its last row. An answer with no calls
/// is a finished turn, and its first non-empty line is what the subagents pane shows under
/// the row.
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

/// How much of the idle window is left - `None` when it has all passed. The daemon has been
/// quiet for the shorter of the time since the newest event and the time since a look last
/// found it busy
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

/// Two daemons, one store, one session - the writer claim (`crate::writer`) driven through
/// `Sessions`, which is the door a daemon comes through.
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

/// What a `nag_` layer may ask for and what the clock does with it: the interval is clamped to
/// `[nag] after_seconds`..`[nag] repeat_cap_seconds`, because a layer names a number and the file
/// the operator wrote is what bounds it.
#[cfg(test)]
mod next_check {
    use super::{NagTimings, TodoNagClock, the_next_check};
    use letibot_tools::builtins::todo::{NAG_REPEAT_CAP, TODO_NAG_AFTER};
    use std::time::{Duration, Instant};

    #[test]
    fn next_ms_is_clamped_to_the_config_bounds() {
        let timings = NagTimings::default();
        // Inside the bounds: the layer's own number, untouched.
        assert_eq!(
            the_next_check(Duration::from_secs(240), timings),
            Duration::from_secs(240)
        );
        // Under the idle window: the window, not a check every second.
        assert_eq!(
            the_next_check(Duration::from_secs(1), timings),
            TODO_NAG_AFTER
        );
        assert_eq!(the_next_check(Duration::ZERO, timings), TODO_NAG_AFTER);
        // Over the repeat cap: the cap, not silence for a day.
        assert_eq!(
            the_next_check(Duration::from_secs(86_400), timings),
            NAG_REPEAT_CAP
        );
        // A file may set a cap under its window, and `Duration::clamp` would panic on the crossed
        // pair; the shorter of the two is then both bounds.
        let crossed = NagTimings {
            window: Duration::from_secs(600),
            cap: Duration::from_secs(60),
            ..NagTimings::default()
        };
        assert_eq!(
            the_next_check(Duration::from_secs(300), crossed),
            Duration::from_secs(60)
        );
    }

    /// And the clock arms the deadline the clamped interval names: the one thing a nag layer's
    /// `next_ms` reaches.
    #[test]
    fn a_clamped_interval_is_where_the_clock_arms() {
        let timings = NagTimings::default();
        let mut clock = TodoNagClock::with_timings(timings);
        assert_eq!(clock.due_at(), None, "nothing is armed before a check");
        let now = Instant::now();
        clock.arm_in(the_next_check(Duration::from_secs(3_600), timings), now);
        let armed = clock.due_at().expect("a deadline");
        assert_eq!(armed, now + NAG_REPEAT_CAP, "a day's quiet is the cap's");
    }
}

#[cfg(test)]
mod the_wake_route {
    //! Which of the three things the daemon does with a wake for a session - the operator's
    //! design in their own words: *"think about it like it is an erlang supervision tree.
    use super::{WakeRoute, wake_route};
    use letibot_sessionlog::hub::Owner;

    /// The daemon's own session: the worker runs the turn, as it always did.
    #[test]
    fn a_session_the_daemon_holds_is_driven() {
        assert_eq!(wake_route(true, Some(Owner::Worker)), WakeRoute::Drive);
        // And `open` is the fact that decides it - a session in `open` whose hub has gone is
        // still the daemon's to run, and the harness inside is what runs it.
        assert_eq!(wake_route(true, None), WakeRoute::Drive);
    }

    /// A live session served by its own thread is handed to that thread. This is the arm
    /// added, and it is a subagent: its harness lives on the thread its parent spawned it
    /// on, so the wake goes to that thread's own condvar rather than being dropped.
    #[test]
    fn a_session_owned_by_its_own_thread_goes_to_its_own_reader() {
        assert_eq!(
            wake_route(false, Some(Owner::OwnThread)),
            WakeRoute::ItsOwnReader
        );
    }

    /// A session the worker owns and does not hold has no other reader - its
    /// open failed. Before the owner tag this was routed `ItsOwnReader` and reported `HandedOn`
    /// a wake handed to a condvar nobody waits on.
    #[test]
    fn a_worker_session_that_did_not_open_is_not_handed_into_a_void() {
        assert_eq!(wake_route(false, Some(Owner::Worker)), WakeRoute::Gone);
    }

    /// A session with no hub at all has nobody to hand anything to - and that is not a loss of
    /// the SETTLEMENT: a child's or a job's row carries `delivered = 0` until the model is told
    /// and the session's next open says it (`Harness::deliver_settlements_left_undelivered`).
    #[test]
    fn a_session_that_is_gone_is_nothing() {
        assert_eq!(wake_route(false, None), WakeRoute::Gone);
    }

    /// A dropped wake is a SENTENCE, and it names the session: `Gone` must not be
    /// `return Outcome::Ignored` with nothing said anywhere, or a ring that woke nobody
    /// would be the one fact in this file with no witness.
    #[test]
    fn a_dropped_wake_names_the_session_and_says_what_is_not_lost() {
        let said = super::dropped_wake("s-gone");
        assert!(said.contains("s-gone"), "{said}");
        assert!(
            said.contains("delivered = 0") && said.contains("next open"),
            "the sentence must say the settlement is still a row, or a reader looks for a \
             lost child: {said}"
        );
    }
}

#[cfg(test)]
mod the_wire_report {
    //! 's `warning.compaction`, built and read back. The builder is private to this module and
    //! takes a `CompactReport`, so this is where it can be exercised without a store, a model
    //! or a turn

    use super::*;
    use crate::harness::{CompactReport, ForkReport};
    use letibot_sessionlog::event::CompactionTurn;

    /// follow-up, defect B: *"summary turn reused 19584 of 0 carryable tokens"* -
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
            budget: None,
            fidelity: None,
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

    /// What the record kept, and what it was allowed: the levels a reader needs to judge a
    /// summary rather than take it on trust, in the sentence AND in the wire report - one set of
    /// numbers, two shapes, so a head drawing one cannot contradict the other.
    #[test]
    fn the_compacted_line_says_what_the_record_kept_and_what_it_was_allowed() {
        let mut sized = report("a summary", Vec::new(), 0, "local_model");
        sized.summary_turn.fidelity = Some(letibot_turn::Fidelity {
            levels: vec![
                letibot_turn::Level {
                    name: "files",
                    total: 47,
                    kept: 44,
                    missing: vec!["a.rs".into(), "b.rs".into()],
                },
                letibot_turn::Level {
                    name: "commands",
                    total: 21,
                    kept: 19,
                    missing: Vec::new(),
                },
                letibot_turn::Level {
                    name: "decisions",
                    total: 6,
                    kept: 6,
                    missing: Vec::new(),
                },
            ],
        });
        sized.summary_turn.budget = Some(letibot_turn::summary_budget(
            262_144, 6_880, 16_384, 250_000,
        ));

        let said = compaction_said(&sized, None);
        assert!(
            said.contains("carries files 44 of 47, commands 19 of 21, decisions 6 of 6"),
            "{said}"
        );
        // At the wall the turn's own free room is what bound the record, and the line says so: a
        // reader who saw only "15% of the room" would think the model had been short-changed.
        assert!(
            said.contains("allowed 10144 of 238880 ledger tokens") && said.contains("had left"),
            "{said}"
        );
        assert!(
            said.contains("it is 9 characters"),
            "the record's own size: {said}"
        );

        let wire = compaction_wire(&sized, "compacted", None, 250_000, 262_144, 16_384);
        let fidelity = wire.fidelity.expect("the levels are on the wire");
        assert_eq!(
            fidelity.line(),
            "files 44 of 47, commands 19 of 21, decisions 6 of 6"
        );
        assert_eq!(
            fidelity.levels[0].dropped,
            vec!["a.rs".to_string(), "b.rs".to_string()]
        );
        let budget = wire.budget.expect("the budget is on the wire");
        assert_eq!(
            (budget.tokens, budget.room, budget.by.as_str()),
            (10_144, 238_880, "turn")
        );

        // A compaction that was asked for nothing has neither, and says neither: the overrun path,
        // which was left alone.
        let plain = report("a summary", Vec::new(), 0, "local_model");
        let said = compaction_said(&plain, None);
        assert!(!said.contains("carries files"), "{said}");
        assert!(!said.to_lowercase().contains("ledger tokens"), "{said}");
        let wire = compaction_wire(&plain, "overrun", None, 0, 0, 0);
        assert!(wire.fidelity.is_none());
        assert!(wire.budget.is_none());
    }

    /// Zero has three causes, and the sentence says which - 's shape one document over, and the
    /// operator's own ask after an automatic compaction of leticl's wrote no tail *"the head
    /// should be able to say which treatment a compaction got.
    #[test]
    fn a_compaction_with_no_tail_says_which_zero_it_is() {
        let none = |because: &str| report("a summary", Vec::new(), 0, because);
        let local = compaction_said(&none("local_model"), None);
        let nothing = compaction_said(&none("nothing_fits"), None);
        let empty = compaction_said(&none("no_turns"), None);
        assert!(local.contains("LOCAL model"), "{local}");
        // The one fact behind the zero, and nothing the reader cannot act on: no review number,
        // no mechanism of ours.
        assert!(
            local.contains("remote model only") && !local.contains("R27"),
            "the local case must say the tail is a remote model's and no more: {local}"
        );
        assert!(
            nothing.contains("larger than the whole tail budget"),
            "{nothing}"
        );
        assert!(empty.contains("nothing to carry"), "{empty}");
        // And they are three different sentences, which is the whole requirement: an
        // absence with one appearance was the defect.
        assert_ne!(local, nothing);
        assert_ne!(local, empty);
        assert_ne!(nothing, empty);
        // `budget` stays silent: the tail exists and the count beside it says how much, so
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
        assert!(
            with_tail.contains("kept 1 item(s) verbatim plus a 9-character summary"),
            "the one sentence carries what was kept: {with_tail}"
        );
        // A reason this build does not know is shown, not swallowed - the wire's own rule
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
                budget: None,
                fidelity: None,
            },
            gained: Vec::new(),
            lost: Vec::new(),
            summary_was_streamed: false,
        }
    }

    /// A local compaction carries the template and an empty tail, and that is not an absent
    /// field. The ruled split says a local model gets no verbatim turns
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

    /// A remote compaction carries the turns themselves, in order, with a role
    /// each - and the two facts about a mid-exchange start are both here rather than
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

    /// The record is split section by section, and the two absences stay apart. `Blocked` is
    /// present with an empty body - *nothing is blocked* - while `Relevant Files` is absent
    /// altogether - *nobody said*.
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
        // invented sections - the raw text is still in `detail`.
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

    /// A record's window is the one the wall fires on. The wall stops a round at
    /// `15/16 of context_window` in the provider's own count, so the window a head draws beside a
    /// compaction is that window and not a planning window converted back through the ratio.
    #[test]
    fn a_compaction_record_carries_the_window_the_wall_fires_on() {
        let mut cfg = Config::for_this_box("/w");
        cfg.context_window = Some(1_000_000);
        // A ledger 1.49x the provider's count: a gap wide enough to round a window down.
        cfg.ledger_scale = Some((1_000_699, 671_280));
        assert_eq!(
            record_wall(&cfg),
            (1_000_000, 62_500),
            "the wall's own pair"
        );
        // And the spelling the record used, which answered one token less on each side.
        assert_eq!(
            (
                cfg.shown_tokens(cfg.planning_window().expect("a window")),
                cfg.shown_tokens(cfg.headroom())
            ),
            (999_999, 62_499)
        );
    }

    /// The units are the operator's, the same conversion the sentence uses, so a
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

    /// The size, what the summary stands in for, what was kept and what was dropped, in one
    /// sentence: a head draws one line from it, so a fact told in a second sentence never reaches
    /// the line.
    #[test]
    fn the_compacted_sentence_carries_the_size_and_what_it_kept_and_dropped() {
        let r = report("## Objective\n\n- ship it", Vec::new(), 12, "budget");
        let said = compaction_said(&r, Some((3u64, 2u64)));
        assert!(
            said.starts_with("compacted: 160000 → 6000 tokens, "),
            "the size before and after first, in the one unit: {said}"
        );
        assert!(
            said.contains("kept 12 item(s) verbatim plus a 23-character summary"),
            "{said}"
        );
        assert!(said.contains("2 item(s) dropped at the seam"), "{said}");
        // Said once: a count repeated in a later sentence is the defect this line exists for.
        assert_eq!(said.matches("12 item(s)").count(), 1, "{said}");
        assert_eq!(said.matches("dropped").count(), 1, "{said}");
    }

    /// A compaction that carried nothing verbatim says the summary is what it kept - not
    /// `kept 0 items`, which reads as a compaction that kept nothing at all.
    #[test]
    fn a_compaction_with_no_tail_says_the_summary_is_what_it_kept() {
        let r = report("## Objective\n\n- x", Vec::new(), 0, "local_model");
        let said = compaction_said(&r, None);
        assert!(said.contains("kept as a 17-character summary"), "{said}");
        assert!(!said.contains("kept 0 item(s)"), "{said}");
        assert!(said.contains("nothing dropped"), "{said}");
    }

    /// The report is the one row an automatic compaction leaves, so the fact that changes how far
    /// the record can be trusted belongs in it rather than beside it.
    #[test]
    fn a_cut_off_summary_says_so_in_the_report() {
        let cut = report("a summary", Vec::new(), 0, "local_model");
        assert!(cut.fork.truncated, "the fixture's summary is cut off");
        let said = compaction_said(&cut, None);
        assert!(said.contains("CUT OFF"), "{said}");
        assert!(
            said.contains("incomplete"),
            "a reader is told what a cut-off summary means, not only that it happened: {said}"
        );
        let mut whole = report("a summary", Vec::new(), 0, "local_model");
        whole.fork.truncated = false;
        assert!(
            !compaction_said(&whole, None).contains("CUT OFF"),
            "a summary that finished says nothing about being cut off"
        );
    }

    /// The unit is the daemon's own word for it and it is not dropped on the floor: an estimate
    /// keeps its `≈` in front of the number and its reason after the facts.
    #[test]
    fn the_unit_is_named_and_an_estimate_keeps_its_mark() {
        let r = report("a summary", Vec::new(), 0, "local_model");
        let said = compaction_said(
            &r,
            Scales {
                // The identity pair: this test is about the unit and the mark, not the
                // conversion, which `the_numbers_are_the_same_units_the_sentence_is_in` holds.
                before: Some((1, 1)),
                after: Some((1, 1)),
                unit: Some("deepseek tokens".into()),
                estimated: true,
            },
        );
        assert!(
            said.starts_with("compacted: 240000 → ≈ 9000 deepseek tokens, "),
            "{said}"
        );
        assert!(
            said.contains("estimated until the next round measures it"),
            "{said}"
        );
    }
}

#[cfg(test)]
mod the_subagent_exit_wait {
    //! What the daemon's exit says about the subagent threads it waited for - the pure half of
    //! `Sessions::drain_subagents`, tested where it lives rather than through a daemon.
    use super::SubagentExit;
    use std::time::Duration;

    /// A quit with nothing running says nothing. This is the ordinary case - no session had
    /// a child to stop - and a line here would be a warning about nothing on every quit.
    #[test]
    fn a_quit_with_nothing_running_says_nothing() {
        assert_eq!(SubagentExit::Waited { told: 0 }.sentence(), None);
    }

    /// A wait that succeeded says so, which is the other half of *the log says which of the
    /// two it did*: a reader must be able to tell a clean exit from one that never looked.
    #[test]
    fn a_wait_that_succeeded_says_how_many_sessions_it_waited_for() {
        let said = SubagentExit::Waited { told: 2 }
            .sentence()
            .expect("two sessions had children, so there is something to say");
        assert!(said.contains('2'), "{said}");
        assert!(said.contains("left"), "it claims the threads went: {said}");
    }

    /// A give-up NAMES the session and the time it waited - a count would leave the reader
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
    //! The two facts `working` is filled from, and the one it is NOT. The defect these hold
    //! shut, measured
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

    /// A round of a turn opening, as the engine publishes it - once per round, which is the whole
    /// reason `status.running` is the wrong fact.
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
            ends_turn: false,
            prefix_check: None,
            dialect_template_sha: String::new(),
        });
    }

    /// A call of that round starting to execute - the window `status.running` cannot see.
    fn a_call_starts(hub: &Hub, turn: &str, call: &str) {
        hub.publish(SessionEvent::ToolStarted {
            turn_id: turn.into(),
            call_id: call.into(),
            name: "bash".into(),
            access: "exec".into(),
        });
    }

    /// A child in its tool calls is WORKING, and the round-level flag says it is not. This is
    /// the case that made the check send its reader to `job_kill` about a child that was
    /// building, and the two assertions are the two halves of the fix
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
        // child's first turn - including this window, which the hub's view only sees because of
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

        // The record alone is enough - the same hub, read as a child whose slot is Running but
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

    /// A child that answered and is parked is BETWEEN TURNS, and that is the fact the parent
    /// could not get - the state whose only door is a message.
    #[test]
    fn a_child_that_answered_and_parked_is_between_turns() {
        let (r, hub) = registry_with("s-1-sub-2");
        a_round_opens(&hub, "t1");
        a_call_starts(&hub, "t1", "c1");
        the_round_generates_out(&hub, "t1");
        // The call finishes, the model answers, the turn ends: the child parks. The slot has
        // settled by then, which is what `false` is here - the same fact the journal's `done` and
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
            edits: Vec::new(),
        });
        // The answering round is the one that ends the prompt.
        a_round_opens(&hub, "t2");
        hub.publish(SessionEvent::TurnFinished {
            turn_id: "t2".into(),
            finish_reason: FinishReason::Eos,
            usage: Usage::default(),
            timings: Timings::default(),
            ends_turn: true,
            prefix_check: None,
            dialect_template_sha: String::new(),
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

    /// A second turn is seen, and that is what the hub's half is for. The slot has already
    /// settled and the journal says `done` - so the daemon's own record of *the* turn cannot see
    /// it - while the child is running the turn a `task_message` started.
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

    /// A child with no hub at all reads as NO EVIDENCE, not as no progress - and the caller's
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

    /// `child_sessions` is the same reading, keyed by the journal's own state - so the notice
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
    //! A `!` run whose rows could not be written leaves the session able to run the next one -
    //! stderr (item 15).
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
        // No turn runs here - the settle fails before one could - but a test that could reach
        // this box's model server must not be able to, so the endpoint is a port nothing serves.
        cfg.endpoint = letibot_turn::Endpoint::new("127.0.0.1", 9);
        let parts = Parts::load(&cfg).expect("the vocabulary loads");
        let registry = Registry::new();
        registry
            .create(&cfg.session_id, "", Sessions::wiring(&cfg))
            .expect("the session registers");
        let mut sessions =
            Sessions::open_first(&parts, cfg.clone(), registry.clone()).expect("it opens");

        // A run in flight, as `start_operator_run` leaves it - and then the session's harness
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
                letibot_sessionlog::SessionEvent::Warning { code, .. } => Some(code.clone()),
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
    //! A session that fails to open leaves no seat slot behind -.
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
