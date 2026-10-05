//! A background job's end, published the moment it is true.
//!
//! The **start** of a background job needs no watcher: the `bash` call that began
//! it finishes as `ToolOutcome::Backgrounded`, whose `handle` is the job id, and
//! the event stream already carries that to every head. The **end** needs one, and
//! the reason is in the job's own lifetime: a session-scoped job outlives the turn
//! that started it — that is what background is *for* — so it settles when no turn
//! is running, when the only events a hub publishes are the ones the daemon
//! publishes itself. The runtime reaps a job when somebody asks (`job_wait`, or
//! turn-end scope reaping); between turns, nobody asks. Without this, every head's
//! picture of a background job was frozen at "running" forever.
//!
//! # Blocked, not polling
//!
//! §18.1-I12: "no timer, no poll loop" is a property this daemon states about
//! itself, and a watcher that woke every second to ask `jobs()` would break it
//! while adding nothing — the host can block. So this is the monitor-wake shape:
//! **one thread per watched job**, blocked in
//! [`ProcessHost::wait_job`], woken by the job's own settlement, publishing
//! [`SessionEvent::JobSettled`], and done. No background jobs, no threads — the
//! laziness rule `Sessions::arm_wake` keeps, and for the same reason: watchers
//! that accumulate are T24's shape.
//!
//! # Why the handles are weak
//!
//! The thread holds `Weak` references to the host and the hub and upgrades them
//! once per wait chunk. A session that closes takes its backend and its hub with
//! it; a watcher holding strong handles would keep the host — and with it the
//! session-scope cgroup its jobs live in — alive for as long as the thread stayed
//! blocked. The chunk is the shutdown re-check, the same role the bell's deadline
//! plays for the monitor waiter: at most [`WATCH_CHUNK`] after close, the thread
//! wakes, fails to upgrade, and is gone.
//!
//! # Why a thread per job and not one per session
//!
//! One thread per session would have to pick a job to block on and poll the rest —
//! a poll loop again. One thread per job blocks on exactly its own job and ends
//! with it. A session with three long builds has three blocked threads, which is
//! the shape the monitor waiter already puts a session in, and each is woken by
//! the thing it reports.
//!
//! # The head is told and the model is not — which is the whole of R7
//!
//! [`SessionEvent::JobSettled`] has been on the wire since protocol 14 and the jobs
//! pane folds it, so the **head** knows. Nothing in `crates/turn/` or the tools layer
//! reads it, so the **model** — the one process that could act on the job ending —
//! learns nothing, and its only route to a result is `job_wait`. That is the
//! affordance the operator hit: *"as soon as i background a job model does `job_wait`
//! and things block again"*, and it is not discipline, it is the absence of any other
//! way to find out.
//!
//! So a settlement is published twice, to the two readers that need it:
//!
//! 1. [`SessionEvent::JobSettled`] to the hub, as it always was — the durable fact,
//!    the pane's row, every head's picture.
//! 2. A [`JobCompletion`] onto the queue of **the session that started it** —
//!    [`JobWatchers::completions`], one per session — which that session's own drain
//!    turns into a notice: [`crate::harness::Harness::wake`] between turns for a
//!    session the daemon drives, and
//!    [`JobWatchers::take_mid_turn_completions`] at a round boundary for a child,
//!    which nothing wakens. That is the hop that was missing.
//!
//! The bell is rung for the second one, because a settlement lands **between turns**,
//! when the worker is blocked in `Registry::next_work` and nothing else would wake it.
//! It is the same bell monitors ring for a firing, and the ordering rule is the same
//! one `Bell::next_any` keeps: a wake has nobody waiting on it, a head that pressed
//! enter does — so a queued prompt is **always** served before a completion.

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::registry::Bell;
use letibot_tools::builtins::task::{TaskRunner, TaskStatus};
use letibot_tools::exec::{JobId, JobState, ProcessHost, Waited};
use letibot_tools::{ToolEvent, ToolEventSink};
use letibot_transcript::ToolOutcome;

/// How long one blocking wait lasts before the thread re-checks its handles and
/// the stop flag. A job's settlement wakes the wait **immediately** — the condvar
/// under the job's state is what `wait_job` sleeps in, and a subagent's is the one
/// `TaskRunner::collect` waits in — so this bounds only how long a closing session's
/// watcher can linger, never how late a settlement is.
const WATCH_CHUNK: Duration = Duration::from_secs(5);

/// **Which kind of backgrounded thing this is.**
///
/// A subagent settles through this channel because it *is* a background job — the
/// operator's ruling: *"i think it should arrive the same way backgrounded jobs
/// complete. in a way agent is a background job."* It is work handed off, it runs
/// while the turn does not, it settles once, and it has a result to collect and an id
/// to kill by. The two differ only in **how** you wait for them and in what a
/// settlement is called, so they share one queue, one bell and one wake, and this is
/// the field that lets the sentence be true for each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundKind {
    /// A host process from `bash --background`, waited on by `wait_job`.
    Job,
    /// A subagent from `task`, waited on by its own slot's condvar.
    Subagent,
}

/// A backgrounded thing's end, as the model is told it.
///
/// Built by the watcher, queued on its own session's [`JobWatchers::completions`], and
/// drained by that session — [`crate::harness::Harness::wake`] for a root,
/// [`JobWatchers::take_mid_turn_completions`] for a child.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobCompletion {
    /// Which of the two this was — see [`BackgroundKind`].
    pub kind: BackgroundKind,
    /// **Which session started it** — the id of the session whose own action
    /// backgrounded this, and the field the routing is decided by.
    ///
    /// A settlement belongs to the session that started it, and the sentence it
    /// becomes says so: *a job you backgrounded has ended*, *a subagent you started
    /// has finished*. Both are claims about **ownership**, so a notice that reaches
    /// the wrong session is not merely misplaced, it is false — measured by the
    /// operator, 2026-10-05: a `sleep 25; echo done` a SUBAGENT backgrounded was
    /// read by the main session, which had never started it and whose own `job_list`
    /// held no such handle.
    ///
    /// It is carried rather than inferred from which queue the completion is on,
    /// because a settlement whose own session can no longer be told is handed up a
    /// level ([`JobWatchers::stop`]) and lands in an ancestor's queue — where it must
    /// still be readable as *not this session's*.
    pub owner: String,
    /// The job's handle, as the backgrounded result printed it.
    pub job: String,
    /// The command it ran. Empty when the job was already reaped and the view is
    /// gone, which the notice says rather than inventing one.
    ///
    /// **Always empty for a subagent**, which is not a command and has no command
    /// line. What a subagent has instead is [`JobCompletion::detail`], and the two
    /// are separate fields rather than one reused one because a command that turns
    /// out to hold an answer is the kind of field a reader stops trusting.
    pub command: String,
    /// What happened to the process: `exited 0`, `signalled 15`, `killed by job_kill`.
    ///
    /// For a subagent: `done` or `failed` — the same two words
    /// [`letibot_sessionlog::SessionEvent::Subagent`] publishes, so the pane's row
    /// and the model's notice name the same state.
    pub state: String,
    /// Bytes the job wrote, all streams together — the `job_output` denominator.
    /// Zero for a subagent, which writes no bytes to a stream this end can count.
    pub produced: u64,
    /// Wall time from spawn to settlement.
    pub elapsed_ms: u64,
    /// **A subagent's own first word about how it ended**: the first line of its
    /// answer, or the reason it failed.
    ///
    /// The jobs path needs no such field — the model's route to what a job said is
    /// `job_output`, and the command it ran is what a settlement is *about*. A
    /// subagent's answer is the thing the model is waiting for, and putting it in
    /// the notice is what makes the wake actionable rather than a nudge; the whole
    /// of it is still in `task_result`, which the notice names.
    pub detail: String,
}

/// The per-session set of watched background jobs and subagents.
///
/// Held by the harness, fed by [`JobWatchSink`] as backgrounded results pass
/// through it, and stopped when the session's backend closes.
pub struct JobWatchers {
    /// `None` when this session cannot start processes **but can still spawn
    /// subagents** — `task` needs no process host, and a read-only seat that hands work
    /// to a child is exactly the case where a completion nobody is told about is hardest
    /// to notice. A set with no host watches no jobs because there are none to watch;
    /// it is not an error state.
    host: Option<Weak<dyn ProcessHost>>,
    hub: Weak<Hub>,
    /// **The other thing that backgrounds, and the reason this file is task-aware.**
    ///
    /// `task` finishes as [`ToolOutcome::Backgrounded`] — the same outcome
    /// `bash --background` gives, deliberately, so the sink above arms a watcher for
    /// it without knowing which tool it came from. But a subagent is not a host
    /// process: `wait_job` has never heard of its handle, answers
    /// [`Waited::NeverStarted`], and the wait below would spin on that name until the
    /// session closed — the completion never queued, the bell never rung, and a
    /// thread leaked per `task` call for the session's life. So the handle is asked
    /// about here first, and a subagent is waited on the way a subagent settles.
    ///
    /// `Weak` for the same reason the host is: a watcher must not keep a session's
    /// runner alive.
    tasks: Option<Weak<dyn TaskRunner>>,
    /// **THIS session's settlements**, in the order they happened.
    ///
    /// Per session and not per tree, which is the correction of 2026-10-05: the
    /// queue used to be the ROOT's for the whole tree (R58), so a job a subagent
    /// started was drained by the main session's [`crate::harness::Harness::wake`]
    /// and never by the session that started it. What a session's own drain returns
    /// is its own settlements, plus whatever a session BELOW it handed up because it
    /// can no longer be told ([`JobWatchers::stop`]). Drained by
    /// [`JobWatchers::take_completions`] between turns, or by
    /// [`JobWatchers::take_mid_turn_completions`] at a round boundary for a session
    /// the daemon cannot wake.
    completions: Arc<Mutex<VecDeque<JobCompletion>>>,
    /// **The queue of the set this one was built from** — the parent's, when this is
    /// a child's ([`JobWatchers::shares_tree`]).
    ///
    /// It has exactly one reader: [`JobWatchers::stop`], which hands up what this
    /// session will never drain. A session that has finished its turn cannot be told
    /// anything more, and a settlement of its own left in its queue would be a
    /// settlement nobody reads — the shape R58 was built to prevent, one level
    /// further down.
    parent_completions: Option<Arc<Mutex<VecDeque<JobCompletion>>>>,
    /// **This set's own session id** — what a completion it queues is stamped with.
    ///
    /// Read from the hub at construction and never from the tree: a child's set
    /// belongs to the child even though it shares the tree's bookkeeping, and this is
    /// the field that says so.
    me: String,
    /// The worker's bell, rung when a completion is queued so a settlement between
    /// turns is not a fact nobody acts on. `None` when there is no daemon — a
    /// harness driven directly by a test has no worker to wake and honestly says so.
    bell: Option<Arc<Bell>>,
    stop: Arc<AtomicBool>,
    /// Jobs with a thread blocked on them, so a re-finished call id cannot start
    /// a second watcher for one job.
    watching: Arc<Mutex<HashSet<String>>>,
    /// Jobs already published, so a late duplicate finish cannot publish a
    /// settlement twice.
    settled: Arc<Mutex<HashSet<String>>>,
    /// **Which session the bell is rung for** — the tree's ROOT, not this session (R58).
    ///
    /// A settlement must reach a session the daemon can *drive*, and the only such
    /// session in a subagent tree is its root: `Sessions::wake` needs
    /// `self.open.get_mut(id)`, a child is adopted into the registry and not into
    /// `open`, so a ring for a child is a condition that fires and is discarded. A
    /// root's set rings itself; a child's set is built from the root's
    /// ([`JobWatchers::shares_tree`]) and inherits this, so the whole tree rings one
    /// bell — **and only one bell**, because a handed-up settlement is the root's
    /// business and that is who the ring names.
    ///
    /// **This is the whole of what a tree still shares about a settlement.** The
    /// queue is per session now (see `completions`): sharing it (R58) is what put a
    /// child's job in the parent's conversation. A ring for a settlement the root
    /// will not be told about is not silent work — `Sessions::wake` drains nothing,
    /// `Harness::wake` returns `Ok(None)`, and the daemon runs no turn — and it is
    /// the price of not having to know, at the moment a settlement lands, whether the
    /// session that owns it will still be there to drain it.
    wake_target: String,
}

impl JobWatchers {
    /// A watcher set for one session's host and hub. Weak by design — see the
    /// module header.
    ///
    /// `bell` is the daemon's worker bell, so a settlement between turns wakes the
    /// model. `None` for a harness with no daemon behind it: the completion is still
    /// queued and the next `wake` would still drain it, but nothing rings to cause
    /// one, which is the truth for a session no worker is serving.
    pub fn new(host: &Arc<dyn ProcessHost>, hub: &Arc<Hub>, bell: Option<Arc<Bell>>) -> Arc<Self> {
        Arc::new(JobWatchers {
            host: Some(Arc::downgrade(host)),
            hub: Arc::downgrade(hub),
            tasks: None,
            completions: Arc::new(Mutex::new(VecDeque::new())),
            // A root has no level above it: nothing it fails to drain can be handed up.
            parent_completions: None,
            me: hub.session_id(),
            bell,
            stop: Arc::new(AtomicBool::new(false)),
            watching: Arc::new(Mutex::new(HashSet::new())),
            settled: Arc::new(Mutex::new(HashSet::new())),
            // A root session rings for itself — the tree's root and its own id are one.
            wake_target: hub.session_id(),
        })
    }

    /// **A set for a session that cannot start processes but can still spawn a
    /// subagent.**
    ///
    /// `task` is `Access::Session` and needs no host — a read-only seat can hand work to
    /// a child — so keying the whole watcher set on the *process* host would have made
    /// the wake work only where a command could also be backgrounded. It is the same
    /// channel either way; this is the arm where the job half has nothing to watch.
    pub fn watching_tasks(hub: &Arc<Hub>, bell: Option<Arc<Bell>>) -> Arc<Self> {
        Arc::new(JobWatchers {
            host: None,
            hub: Arc::downgrade(hub),
            tasks: None,
            completions: Arc::new(Mutex::new(VecDeque::new())),
            parent_completions: None,
            me: hub.session_id(),
            bell,
            stop: Arc::new(AtomicBool::new(false)),
            watching: Arc::new(Mutex::new(HashSet::new())),
            settled: Arc::new(Mutex::new(HashSet::new())),
            wake_target: hub.session_id(),
        })
    }

    /// **And the session's subagents settle here too.**
    ///
    /// A separate call rather than a fourth parameter, because a watcher set is built
    /// in two places that do not both have a runner — a test's bare host, and the
    /// harness — and a `None` there is the honest description of a set that can watch
    /// no subagents at all.
    pub fn with_tasks(self: Arc<Self>, tasks: &Arc<dyn TaskRunner>) -> Arc<Self> {
        Arc::new(JobWatchers {
            host: self.host.clone(),
            hub: Weak::clone(&self.hub),
            tasks: Some(Arc::downgrade(tasks)),
            completions: Arc::clone(&self.completions),
            parent_completions: self.parent_completions.clone(),
            me: self.me.clone(),
            bell: self.bell.clone(),
            stop: Arc::clone(&self.stop),
            watching: Arc::clone(&self.watching),
            settled: Arc::clone(&self.settled),
            wake_target: self.wake_target.clone(),
        })
    }

    /// **Join this session's watcher to its tree's** (R58).
    ///
    /// A child keeps everything that is genuinely its own — the hub its rows are
    /// published to, the host and the runner that know its handles, and **now the
    /// completions queue**: a settlement belongs to the session that started it
    /// (`JobCompletion::owner`), and a shared queue is what delivered a child's job to
    /// the main session as *"a job you backgrounded"*. What it takes from the tree is
    /// the **watching/settled sets** that make `delivering` tree-wide and stop one
    /// settlement being queued twice, and the **ring target**, so a grandchild's
    /// settlement rings the root rather than the child — which is what turns
    /// `Sessions::wake`'s `Ignored` into a turn the daemon actually runs.
    ///
    /// **The `stop` flag is deliberately NOT shared, and it was, and that was a defect.**
    /// The reasoning was *"a tree closes together"*, and it is wrong about when `stop` is
    /// set: `Harness::close_backend` runs at the end of a **child's** turn — *"release its
    /// substrate now, not when the harness is dropped"* — so the first subagent to finish
    /// set the ROOT's stop flag and every watcher in the tree broke out of its wait at the
    /// next chunk. The root then stopped being told about its own background jobs. Measured
    /// by the operator, 2026-10-04: *"it suddenly stopped getting job completion events"* —
    /// and the shape is in the same measurement, because a job that settles inside one
    /// [`WATCH_CHUNK`] still raced through the `Happened` arm and reported, which is why
    /// three- and four-second demo jobs kept arriving and a forty-five-second one did not.
    ///
    /// `stop` means *this session's backend has closed*, which is a fact about one session;
    /// a child's turn ending is not that fact for its parent.
    ///
    /// `tree` is the root's set (its own `wake_target` is its own id), so this needs no
    /// parent-chain walk: the set it is built from already knows its root.
    pub fn shares_tree(self: Arc<Self>, tree: &Arc<JobWatchers>) -> Arc<Self> {
        Arc::new(JobWatchers {
            host: self.host.clone(),
            hub: Weak::clone(&self.hub),
            tasks: self.tasks.clone(),
            // **Its own**, and the one field this constructor no longer takes from the
            // tree. The child's queue is where its own settlements are drained from.
            completions: Arc::clone(&self.completions),
            // And the tree's is where what it can no longer drain is handed up.
            parent_completions: Some(Arc::clone(&tree.completions)),
            me: self.me.clone(),
            bell: self.bell.clone(),
            stop: Arc::clone(&self.stop),
            watching: Arc::clone(&tree.watching),
            settled: Arc::clone(&tree.settled),
            wake_target: tree.wake_target.clone(),
        })
    }

    /// **Take every settlement this session has not been told yet.** One drain per
    /// turn: what comes back is what the harness submits, and taking it is what
    /// stops a settlement being delivered twice.
    ///
    /// What is in here is this session's own settlements and nothing else, which is
    /// the routing rule in one sentence: a job started by a subagent is queued on the
    /// subagent's set and drained by the subagent. The one exception is a settlement
    /// whose own session has stopped ([`JobWatchers::stop`]) — it is handed up a level
    /// rather than left where nobody will read it, and it still names its owner in
    /// [`JobCompletion::owner`] so the sentence can say it is not this session's.
    ///
    /// # Which drain a session has is a fact about the session, not a choice
    ///
    /// A **root** is driven: `Sessions::wake` names it, and this is what
    /// [`crate::harness::Harness::wake`] drains to run the settlement's own turn (R7).
    /// A **child** is not — it is adopted into the registry and never into
    /// `Sessions::open`, so `Sessions::wake` answers `Ignored` for it and nothing will
    /// ever wake it between turns. Its own turn is therefore the only place it can be
    /// told, and it is told there: [`JobWatchers::take_mid_turn_completions`], whose
    /// guard is exactly this root/child difference.
    pub fn take_completions(&self) -> Vec<JobCompletion> {
        let mut g = self.completions.lock().expect("job completions");
        g.drain(..).collect()
    }

    /// **Take them at a round boundary, for the session the daemon cannot wake.**
    ///
    /// Empty for the session at the top of a tree, and that is the whole of the
    /// guard: a root has a between-turn wake, which is where R7 delivers a settlement
    /// (as a turn of its own, after any queued operator line), so taking one into a
    /// running turn here would be a second delivery path for the one R7 built. A child
    /// has no wake at all, so this is not a second path for it — it is the only one.
    ///
    /// Is this session its own tree's root is a question this set can answer from two
    /// fields it already carries: `shares_tree` gives a child its root's `wake_target`
    /// and leaves `me` alone, so they are equal at the root and unequal below it.
    pub fn take_mid_turn_completions(&self) -> Vec<JobCompletion> {
        if self.wake_target == self.me {
            return Vec::new();
        }
        self.take_completions()
    }

    /// **Is this job's completion already on its way to the model?**
    ///
    /// True while a watcher thread is blocked on the job. That thread publishes the
    /// settlement to the hub and queues a [`JobCompletion`], which
    /// [`crate::harness::Harness::wake`] submits as a turn of its own — so from the
    /// moment the watcher exists, the model is going to be told whether it asks or
    /// not. Nothing `job_wait` does can change that, which is exactly why R23 makes
    /// the wait return instead of blocking: the answer is in flight.
    ///
    /// **A settled job is deliberately `false`.** Once the watcher has published, it
    /// is moved from `watching` to `settled` and the thread ends. A `job_wait` on a
    /// job in that state is answered by the caller's own "had already finished"
    /// branch, which is the honest sentence for it — *nothing was waited for*, rather
    /// than *you are about to be told*. Reporting `true` here for a job whose
    /// delivery has already happened would make the two readings one.
    ///
    /// False for a job this session was never watching: a job from another session, a
    /// scope's contents, anything a runtime with no daemon behind it sees. Those
    /// waits keep blocking for exactly as long as they were asked to.
    pub fn delivering(&self, job: &str) -> bool {
        self.watching.lock().expect("job watchers").contains(job)
    }

    /// Stop every watcher this set has spawned, **and hand up what this session will
    /// never drain**. Called when the session's backend closes: the threads wake within
    /// [`WATCH_CHUNK`], fail to upgrade their handles, and exit without publishing.
    ///
    /// **Per set, and the set is per session** — see [`JobWatchers::shares_tree`] for the
    /// defect that came of sharing this across a tree.
    ///
    /// # Why this is not only a flag
    ///
    /// Setting the flag says *this session will not drain again* — `close_backend` runs
    /// at the end of a child's turn, and a child is driven by nobody between turns (see
    /// [`JobWatchers::take_completions`]). Anything still in the queue at that moment is
    /// a settlement no reader will ever reach: a job that settled between the child's
    /// last round boundary and its turn ending, or a grandchild still working when its
    /// parent finished (`watch_task` deliberately does not stop with the turn, so the
    /// parent can still be reawakened — 2026-10-04).
    ///
    /// So the queue is emptied into the level above, which is a session that is still
    /// running — the child's parent, or the tree's root when the parent is the one that
    /// stopped. The completion keeps its `owner`, so the notice at the other end says
    /// whose it was rather than claiming *you backgrounded*.
    ///
    /// **The hand-over and the queue are ordered by one lock.** A watcher pushes
    /// holding this queue's lock and reads the flag there
    /// ([`SettlementQueue::push`]); this drains holding the same lock, after setting the
    /// flag. Whichever of the two runs first, the settlement is either in the queue this
    /// drain takes or pushed past it by the flag — it cannot fall between them.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        let pending: Vec<JobCompletion> = {
            let mut own = self.completions.lock().expect("job completions");
            own.drain(..).collect()
        };
        // A root has nowhere to hand anything up to, and a session that drained
        // everything it had leaves nothing to carry.
        let Some(parent) = &self.parent_completions else {
            return;
        };
        if pending.is_empty() {
            return;
        }
        parent.lock().expect("job completions").extend(pending);
        // **And the ring, because the level above has not been told yet.** The push
        // that queued this rang the tree's root before the hand-over was even possible;
        // without this ring a settlement that arrives here a moment after that ring
        // would sit in the parent's queue until the parent had some other reason to wake.
        if let Some(bell) = &self.bell {
            bell.ring_wake(&self.wake_target);
        }
    }

    /// **Has this set been stopped?** The property [`JobWatchers::shares_tree`] is asserted
    /// against: a child joining a tree must not take the tree's stop, or a child's turn
    /// ending silences every watcher the root has.
    pub fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// Watch one backgrounded thing. Idempotent: one already watched, or already
    /// settled, is left alone.
    ///
    /// **Which kind it is, is asked rather than inferred.** The sink that calls this
    /// sees a `Backgrounded` outcome and a handle; it does not know whether the tool
    /// that produced it was `bash` or `task`, and it should not have to — the whole
    /// point of `task` returning the same outcome is that the same channel carries
    /// both. So the runner is asked once, with a zero timeout, and `Unknown` — the
    /// answer for a handle it never minted — is what sends the handle down the job
    /// path. No string sniffing on the handle's shape, which would be a naming
    /// convention mistaken for a fact.
    pub fn watch(self: &Arc<Self>, job: String) {
        {
            let mut watching = self.watching.lock().expect("job watchers");
            if watching.contains(&job) || self.settled.lock().expect("job watchers").contains(&job)
            {
                return;
            }
            watching.insert(job.clone());
        }
        let host = self.host.as_ref().map(Weak::clone);
        let hub = Weak::clone(&self.hub);
        // **Where this settlement goes is decided by these fields and nothing else** —
        // see `SettlementQueue`, which is what the watcher threads carry.
        let queue = Arc::new(SettlementQueue {
            own: Arc::clone(&self.completions),
            parent: self.parent_completions.clone(),
            stopped: Arc::clone(&self.stop),
            owner: self.me.clone(),
            bell: self.bell.clone(),
            wake_target: self.wake_target.clone(),
        });
        let watching = Arc::clone(&self.watching);
        let settled = Arc::clone(&self.settled);
        let runner = self.tasks.as_ref().and_then(Weak::upgrade);
        if let Some(runner) =
            runner.filter(|r| !matches!(r.collect(&job, Duration::ZERO), TaskStatus::Unknown))
        {
            let queue = Arc::clone(&queue);
            let _ = std::thread::Builder::new()
                .name(format!("subagent-watch-{}", &job[..job.len().min(20)]))
                .spawn(move || watch_task(runner, hub, queue, watching, settled, job));
            return;
        }
        // Not a subagent and no host to ask: nothing here can ever settle this handle, so
        // it is dropped rather than left in `watching` — a name that outlives its watcher
        // would make `delivering` true for ever and leak the entry.
        let Some(host) = host else {
            watching.lock().expect("job watchers").remove(&job);
            return;
        };
        let _ = std::thread::Builder::new()
            .name(format!("job-watch-{}", &job[..job.len().min(20)]))
            .spawn(move || watch_one(host, hub, queue, watching, settled, job));
    }
}

/// **Where a watcher's settlement goes, and the one place that decides it.**
///
/// Cloned into the watcher thread rather than passed as six arguments, because the
/// decision is not the watcher's: a thread carries this and calls [`SettlementQueue::push`],
/// which queues the settlement on the session that started the thing — the whole of the
/// routing rule — or one level up when that session has already stopped.
struct SettlementQueue {
    /// The queue of the session that started it.
    own: Arc<Mutex<VecDeque<JobCompletion>>>,
    /// The queue of the level above it, when there is one.
    parent: Option<Arc<Mutex<VecDeque<JobCompletion>>>>,
    /// The owner's stop flag — see [`JobWatchers::stop`]. Read **while holding `own`**,
    /// which is what makes the two orderings exhaustive.
    stopped: Arc<AtomicBool>,
    /// The owner's session id, stamped on every settlement this queue produces.
    owner: String,
    bell: Option<Arc<Bell>>,
    wake_target: String,
}

impl SettlementQueue {
    /// **Queue one settlement where its session can still be told, and ring.**
    ///
    /// The `stopped` read is inside the `own` lock on purpose: the alternative is a
    /// settlement pushed into an owner's queue in the instant between the owner's last
    /// drain and the flag being set — which is the shape of settlement-nobody-reads this
    /// whole mechanism exists to avoid.
    fn push(&self, c: JobCompletion) {
        let mut g = self.own.lock().expect("job completions");
        if self.stopped.load(Ordering::Relaxed)
            && let Some(parent) = &self.parent
        {
            drop(g);
            parent.lock().expect("job completions").push_back(c);
            self.ring();
            return;
        }
        g.push_back(c);
        drop(g);
        self.ring();
    }

    /// The bell, rung for the session that can be driven — never for a child.
    fn ring(&self) {
        if let Some(bell) = &self.bell {
            bell.ring_wake(&self.wake_target);
        }
    }
}

fn watch_one(
    host: Weak<dyn ProcessHost>,
    hub: Weak<Hub>,
    queue: Arc<SettlementQueue>,
    watching: Arc<Mutex<HashSet<String>>>,
    settled: Arc<Mutex<HashSet<String>>>,
    job: String,
) {
    loop {
        let (Some(host), Some(hub)) = (host.upgrade(), hub.upgrade()) else {
            // The session is gone — backend, hub, or both. Nothing left that a
            // settlement would be true of.
            break;
        };
        match host.wait_job(&JobId(job.clone()), WATCH_CHUNK) {
            Ok(Waited::Happened { state, .. }) => {
                // The wake's own reading of the state is the truth; the view, if
                // it still exists, adds the numbers a settlement reports. It can
                // be gone already — a scope reap at session end removes jobs —
                // and then the settlement carries the state and no numbers,
                // rather than numbers invented for a process nobody can measure.
                let view = host.job(&JobId(job.clone()));
                let word = state
                    .map(|s: JobState| s.word())
                    .or_else(|| view.as_ref().map(|v| v.state.word()))
                    .unwrap_or_else(|| "gone".to_string());
                let produced = view.as_ref().map(|v| v.produced).unwrap_or(0);
                let elapsed_ms = view
                    .as_ref()
                    .and_then(|v| v.ran_for)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                // The command is read here, from the live view, because this is the
                // last moment it exists: a settlement is delivered between turns, by
                // which time the job is usually reaped and `/proc` no longer says.
                let command = view.as_ref().map(|v| v.command.clone()).unwrap_or_default();
                hub.publish(SessionEvent::JobSettled {
                    job: job.clone(),
                    state: word.clone(),
                    produced,
                    elapsed_ms,
                });
                // **And the model, which the event does not reach.** Queued on THIS
                // session's own queue — the session that backgrounded it — and the bell
                // rung because a settlement lands between turns, when nothing else is
                // asking the worker for anything.
                queue.push(JobCompletion {
                    kind: BackgroundKind::Job,
                    owner: queue.owner.clone(),
                    job: job.clone(),
                    command,
                    state: word,
                    produced,
                    elapsed_ms,
                    detail: String::new(),
                });
                settled.lock().expect("job watchers").insert(job.clone());
                break;
            }
            // Still running — or never seen, which is a boot window and not a
            // finish. The chunk is the shutdown re-check and nothing else: a
            // settlement wakes the wait at once, so a long-lived job spends its
            // whole life in this arm.
            Ok(Waited::Deadline { .. } | Waited::NeverStarted { .. }) => {
                if queue.stopped.load(Ordering::Relaxed) {
                    break;
                }
            }
            // The job is gone from the host: its scope was reaped with the
            // session, or the host dropped the record. There is no hub left that
            // a settlement would be true of.
            Err(_) => break,
        }
    }
    watching.lock().expect("job watchers").remove(&job);
}

/// **The same settlement, waited on the way a subagent settles.**
///
/// `watch_one`'s shape is kept exactly — a chunked wait whose chunk is the shutdown
/// re-check and nothing else, a queue push, a bell — and the only thing that changes
/// is what is being waited on. `TaskRunner::collect` is a condvar wait inside the
/// subagent's own slot, so a child that answers unblocks this at once and the chunk
/// bounds only how long a closing session's watcher lingers.
///
/// **No `JobSettled` is published here, and that is deliberate rather than an
/// omission.** The subagent's run already published [`SessionEvent::Subagent`] with
/// the same state word, from the thread that ran it (harness.rs) — that is the
/// head-facing fact and the pane's row. Publishing a second event for one settlement
/// would be the duplication this tree keeps finding under other names, and the two
/// would drift the moment either changed.
/// **No `stop` here as a BREAK, and that is still the point** (2026-10-04): a subagent is not a
/// process, so there is no cgroup to release and nothing this thread holds that a closing
/// backend must free. Its liveness test is the `hub` upgrade in the loop below, so a
/// grandchild that outlives its parent's turn is still waited on. The owner's `stop` flag does
/// reach the queue this thread pushes to — but there it decides **where** the settlement
/// goes, never whether this thread keeps waiting.
fn watch_task(
    runner: Arc<dyn TaskRunner>,
    hub: Weak<Hub>,
    queue: Arc<SettlementQueue>,
    watching: Arc<Mutex<HashSet<String>>>,
    settled: Arc<Mutex<HashSet<String>>>,
    job: String,
) {
    loop {
        // The hub is the same liveness test the job path uses: a session that has
        // closed takes its hub with it, and there is nothing left a settlement would
        // be true of. The queue is the OWNER's, so a child's settlement still needs the
        // child's hub alive to be worth queueing — nothing else here reads it.
        if hub.upgrade().is_none() {
            break;
        }
        match runner.collect(&job, WATCH_CHUNK) {
            // Settled, one way or the other. Both are the model's business: a subagent
            // that failed is the thing the parent is blocked on just as much as one
            // that answered.
            TaskStatus::Done { answer } => {
                settled_here("done", first_line(&answer), &job, &queue, &settled);
                break;
            }
            TaskStatus::Failed { why } => {
                settled_here("failed", first_line(&why), &job, &queue, &settled);
                break;
            }
            // Still working, or a handle this runner never minted.
            //
            // **`Unknown` ends the wait and `Running` does not end on `stop`** (2026-10-04).
            // The old arm broke on `stop` for BOTH, and that was `close_backend`'s shutdown
            // re-check borrowed for a job that is not a process: a subagent needs no host,
            // nothing here holds a cgroup, and the thread's liveness test is the `hub`
            // upgrade at the top of this loop. What the `stop` check actually did was kill a
            // CHILD's watcher for its own grandchild — `Harness::close_backend` runs at the
            // end of the child's turn, while the grandchild is still working — so R58's
            // promise (*a settlement rings a bell that has a worker*) lasted exactly one
            // turn.
            //
            // `Unknown` is the handle this runner does not know at all, which the paragraph
            // above has always claimed ends the wait: a name nobody owns is not something to
            // keep asking about, and nothing will ever settle it.
            TaskStatus::Running { .. } => {}
            TaskStatus::Unknown => break,
        }
    }
    watching.lock().expect("job watchers").remove(&job);
}

/// Queue one subagent's settlement and ring — the push and the bell, which are the
/// same two things the job path does at the same moment in its own wait.
fn settled_here(
    state: &str,
    detail: String,
    job: &str,
    queue: &Arc<SettlementQueue>,
    settled: &Arc<Mutex<HashSet<String>>>,
) {
    queue.push(JobCompletion {
        kind: BackgroundKind::Subagent,
        owner: queue.owner.clone(),
        job: job.to_string(),
        // A subagent runs no command; the notice for one is built from `state`
        // and `detail` and never reads this.
        command: String::new(),
        state: state.to_string(),
        // It writes no bytes to a stream this end can count. The tokens it
        // generated are in the subagent's own metrics, and `produced` means
        // `job_output`'s denominator everywhere else.
        produced: 0,
        elapsed_ms: 0,
        detail,
    });
    settled
        .lock()
        .expect("job watchers")
        .insert(job.to_string());
}

/// The first line of what a subagent said, for a one-line notice. The whole of it is
/// in `task_result`, which the notice names.
fn first_line(text: &str) -> String {
    let l = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    if l.chars().count() <= 160 {
        return l.trim().to_string();
    }
    format!("{}…", l.trim().chars().take(160).collect::<String>())
}

/// A [`ToolEventSink`] that starts a watcher for every job a call leaves behind.
///
/// A decorator, for the same reason the intent encoder's `IntentSink` is one:
/// wiring it is one line at the call site, and forgetting it is visible rather
/// than silent. It adds nothing to what heads see — the start of a background job
/// is already on the wire, the `Finished` event's outcome *is* `Backgrounded`,
/// handle and all — it only tells the daemon's watcher that the job now exists
/// and will settle, which is the half the event stream cannot say.
pub struct JobWatchSink<S> {
    inner: S,
    watch: Option<Arc<JobWatchers>>,
}

impl<S> JobWatchSink<S> {
    /// `None` for `watch` when the session has no process host: nothing can be
    /// backgrounded, so there is nothing to watch.
    pub fn new(inner: S, watch: Option<Arc<JobWatchers>>) -> Self {
        JobWatchSink { inner, watch }
    }
}

impl<S: ToolEventSink> ToolEventSink for JobWatchSink<S> {
    fn emit(&mut self, event: ToolEvent) {
        if let Some(w) = &self.watch
            && let ToolEvent::Finished {
                outcome: ToolOutcome::Backgrounded { handle, .. },
                ..
            } = &event
        {
            w.watch(handle.clone());
        }
        self.inner.emit(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use letibot_tools::RecordingToolSink;
    use letibot_tools::exec::{HostProcesses, ScopeKind, SpawnRequest};
    use std::time::Instant;

    /// A host over a temp root, the way `exec::host`'s own tests build one. The
    /// box needs a cgroup v2 subtree to delegate; every host-spawning test in
    /// tools already assumes the same.
    fn a_host() -> Arc<HostProcesses> {
        let root = std::env::temp_dir().join(format!("letibot-jobwatch-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        Arc::new(HostProcesses::new(&root).expect("this box has a cgroup v2 tree"))
    }

    fn spawn_a_short_job(host: &HostProcesses, secs: &str) -> JobId {
        host.spawn(&SpawnRequest {
            command: format!("sleep {secs}"),
            cwd: "/".into(),
            scope: ScopeKind::Session,
            scope_name: None,
            background: true,
            env: vec![],
        })
        .expect("spawn")
    }

    /// The one `JobSettled` the hub holds for `job`, if it has arrived yet.
    fn settlement_for(hub: &Hub, job: &str) -> Option<(String, u64, u64)> {
        hub.retained().into_iter().find_map(|e| match e.event {
            SessionEvent::JobSettled {
                job: ref j,
                ref state,
                produced,
                elapsed_ms,
            } if j == job => Some((state.clone(), produced, elapsed_ms)),
            _ => None,
        })
    }

    /// Poll the hub's log for the settlement rather than sleeping a fixed time:
    /// the watcher publishes the moment the job exits, and the test ends at
    /// whichever comes first.
    fn wait_for_settlement(hub: &Hub, job: &str) -> (String, u64, u64) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(hit) = settlement_for(hub, job) {
                return hit;
            }
            assert!(
                Instant::now() < deadline,
                "no JobSettled for {job} within 10s; log: {:?}",
                hub.retained()
                    .iter()
                    .map(|e| e.event.kind())
                    .collect::<Vec<_>>()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    fn a_backgrounded_result_through_the_sink_publishes_the_jobs_settlement() {
        let Some(_) = letibot_tokencore::apparatus::present(
            "a cgroup v2 tree",
            letibot_tools::Cgroup2::probe().is_ok(),
        ) else {
            return;
        };
        let hub = Hub::new("s-jobs");
        let host = a_host();
        let watchers = JobWatchers::new(&(host.clone() as Arc<dyn ProcessHost>), &hub, None);
        let mut sink = JobWatchSink::new(RecordingToolSink::default(), Some(watchers));

        let id = spawn_a_short_job(&host, "0.3");
        sink.emit(ToolEvent::Finished {
            turn_id: "t1".into(),
            call_id: "c1".into(),
            outcome: ToolOutcome::Backgrounded {
                handle: id.0.clone(),
                ran_for_ms: 0,
                how: letibot_transcript::Backgrounding::Asked,
                next: "job_wait".into(),
            },
            payload_digest: letibot_tools::payload_digest(""),
            inline_bytes: 0,
            full_bytes: 0,
            spill: None,
            repairs: 0,
            edit: None,
        });

        let (state, _produced, elapsed_ms) = wait_for_settlement(&hub, &id.0);
        assert_eq!(state, "exited 0", "the word a listing shows, not ok/error");
        // The job slept ~300ms; the number comes off the job's own stamps, so it
        // is the real runtime and not the watcher's wait time.
        assert!(
            (250..5_000).contains(&elapsed_ms),
            "elapsed_ms {elapsed_ms} is not the job's runtime"
        );
    }

    #[test]
    fn a_job_watched_twice_and_again_after_settlement_publishes_once() {
        let Some(_) = letibot_tokencore::apparatus::present(
            "a cgroup v2 tree",
            letibot_tools::Cgroup2::probe().is_ok(),
        ) else {
            return;
        };
        let hub = Hub::new("s-jobs");
        let host = a_host();
        let watchers = JobWatchers::new(&(host.clone() as Arc<dyn ProcessHost>), &hub, None);

        let id = spawn_a_short_job(&host, "0.2");
        watchers.watch(id.0.clone());
        // A re-finished call id, or a second head reporting the same job, must
        // not start a second watcher: one job, one settlement.
        watchers.watch(id.0.clone());

        wait_for_settlement(&hub, &id.0);
        // And a watch that arrives after the settlement is a no-op too — the
        // settlement is already on the wire and a second one would be a job
        // that died twice.
        watchers.watch(id.0.clone());
        std::thread::sleep(Duration::from_millis(300));

        let count = hub
            .retained()
            .iter()
            .filter(|e| matches!(&e.event, SessionEvent::JobSettled { job, .. } if job == &id.0))
            .count();
        assert_eq!(count, 1, "one job, one settlement");
    }

    /// **R23's question, answered by the one thing that can answer it.**
    ///
    /// `delivering` is what `job_wait` asks before it decides whether to block, so
    /// the two readings it must get right are: **true** while a watcher is live for
    /// that job — the promise is real and the wait would be waiting for something
    /// already in flight — and **false** everywhere else. The false cases are the
    /// whole of "R23 removes one case and only that one": a job this session never
    /// watched keeps blocking, and so does a job whose settlement has already been
    /// published.
    ///
    /// The settled case is the subtle one and is asserted separately from the
    /// never-watched one on purpose. Both are `false`, but for different reasons and
    /// with different consequences: for a settled job `job_wait`'s own "had already
    /// finished" branch answers, which is the honest sentence — *nothing was waited
    /// for* — while for an unwatched job the wait really does block, which is what
    /// the operator's "a deliberate block before a dependent step" depends on.
    #[test]
    fn delivering_is_true_only_while_a_watcher_is_live_for_that_job() {
        let Some(_) = letibot_tokencore::apparatus::present(
            "a cgroup v2 tree",
            letibot_tools::Cgroup2::probe().is_ok(),
        ) else {
            return;
        };
        let hub = Hub::new("s-jobs");
        let host = a_host();
        let watchers = JobWatchers::new(&(host.clone() as Arc<dyn ProcessHost>), &hub, None);

        let id = spawn_a_short_job(&host, "30");

        // Before anything watched it: not delivering, so a wait on it blocks.
        assert!(
            !watchers.delivering(&id.0),
            "a job nobody watches must not claim its answer is on the way"
        );

        watchers.watch(id.0.clone());
        assert!(
            watchers.delivering(&id.0),
            "a live watcher IS the promise: the settlement will be submitted to the \
             model whether it asks or not"
        );
        // Per job, not per session. The closure `job_wait` is given takes an id for
        // exactly this reason, and a second id must not ride on the first's watcher.
        assert!(
            !watchers.delivering("j-not-this-one"),
            "the promise is per job; another id must not inherit it"
        );

        // Let it settle: the watcher publishes and ends, so it is no longer
        // delivering — and the wait on it is answered by the caller's own
        // already-finished branch instead.
        host.kill_job(&id).expect("killing the job settles it");
        wait_for_settlement(&hub, &id.0);
        let deadline = Instant::now() + Duration::from_secs(5);
        while watchers.delivering(&id.0) {
            assert!(
                Instant::now() < deadline,
                "the watcher must stop claiming delivery once it has delivered"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            !watchers.delivering(&id.0),
            "a settled job is not being delivered any more; it HAS been"
        );
    }

    /// **The arming path, end to end: the sink is what makes `delivering` true.**
    ///
    /// The test above arms the watcher with a direct `watch()` call, which is not how
    /// a session arms it. A real session routes every tool result through
    /// [`JobWatchSink`], and *that* is the composition `harness.rs` depends on when it
    /// wires `job_wait`'s question to the same watcher set: the sink sees the
    /// `Backgrounded` outcome, arms a watcher, and the moment it does, the promise is
    /// real and `job_wait` must stop blocking.
    ///
    /// Asserted in both directions because the `false` half is the one that keeps R23
    /// from being larger than it is: a job no `Backgrounded` result has passed through
    /// is a job with no promise, and a wait on it blocks.
    ///
    /// **No race.** `watch()` inserts into `watching` synchronously, before it spawns
    /// the thread, so `delivering` is true by the time the sink's `emit` returns — which
    /// is before the turn that emitted it has ended, and therefore necessarily before
    /// the model's next call. A test on a clock would be testing the thread scheduler.
    #[test]
    fn the_sink_arms_the_watcher_and_that_is_what_makes_a_wait_decline() {
        let Some(_) = letibot_tokencore::apparatus::present(
            "a cgroup v2 tree",
            letibot_tools::Cgroup2::probe().is_ok(),
        ) else {
            return;
        };
        let hub = Hub::new("s-jobs");
        let host = a_host();
        let watchers = JobWatchers::new(&(host.clone() as Arc<dyn ProcessHost>), &hub, None);
        let mut sink = JobWatchSink::new(RecordingToolSink::default(), Some(watchers.clone()));

        let id = spawn_a_short_job(&host, "30");
        // Before any result has passed through the sink: no promise, so a wait blocks.
        assert!(
            !watchers.delivering(&id.0),
            "a job the sink has not seen must not claim its answer is on the way"
        );

        sink.emit(ToolEvent::Finished {
            turn_id: "t1".into(),
            call_id: "c1".into(),
            outcome: ToolOutcome::Backgrounded {
                handle: id.0.clone(),
                ran_for_ms: 0,
                how: letibot_transcript::Backgrounding::Asked,
                next: "job_wait".into(),
            },
            payload_digest: letibot_tools::payload_digest(""),
            inline_bytes: 0,
            full_bytes: 0,
            spill: None,
            repairs: 0,
            edit: None,
        });

        // Synchronously true, with no sleep: the promise exists from the emit.
        assert!(
            watchers.delivering(&id.0),
            "the sink is what arms the watcher, and arming it is what makes \
             `job_wait` decline to block"
        );

        let _ = host.kill_job(&id);
    }

    /// **R7: the settlement reaches the MODEL, not only the heads.**
    ///
    /// `JobSettled` has gone to the hub since protocol 14 and the jobs pane folds it,
    /// so every head knows. Nothing read it back into a turn, so the model's only route
    /// to a result was `job_wait` — which is the operator's complaint, and this is the
    /// hop that answers it. What is asserted is the two things the event cannot carry:
    /// that a completion is queued for the harness at all, and that it carries the
    /// **command** (read off the live view, which the durable event does not have and
    /// which is gone by the time a settlement is delivered).
    #[test]
    fn a_settled_job_is_queued_for_the_model_with_the_command_the_event_omits() {
        let Some(_) = letibot_tokencore::apparatus::present(
            "a cgroup v2 tree",
            letibot_tools::Cgroup2::probe().is_ok(),
        ) else {
            return;
        };
        let hub = Hub::new("s-jobs");
        let host = a_host();
        let watchers = JobWatchers::new(&(host.clone() as Arc<dyn ProcessHost>), &hub, None);

        let id = spawn_a_short_job(&host, "0.2");
        watchers.watch(id.0.clone());
        wait_for_settlement(&hub, &id.0);

        let done = watchers.take_completions();
        assert_eq!(done.len(), 1, "one job, one completion for the model");
        assert_eq!(done[0].job, id.0);
        assert_eq!(done[0].state, "exited 0");
        assert!(
            done[0].command.contains("sleep 0.2"),
            "the command must reach the model: {:?}",
            done[0].command
        );
        // **Taken, not left to arrive again.** The harness drains once per wake; a
        // second drain is empty, which is what stops a settlement being delivered as
        // two turns.
        assert!(
            watchers.take_completions().is_empty(),
            "a completion is taken once, not left for the next wake"
        );
    }

    /// **R7's floor rule, structurally: a completion never jumps a queued human line.**
    ///
    /// A job ending is not urgent in the operator's sense — nothing is waiting on it but
    /// the model — so it must not displace a person who pressed enter. That is not a
    /// check anywhere in this file; it is `Bell::next_any`'s order, where commands drain
    /// before wakes. Asserted here for the wake a job rings, in the shape the registry's
    /// own `a_wake_is_served_after_every_queued_command` asserts for a monitor's.
    #[test]
    fn a_jobs_completion_wakes_the_worker_with_no_command_behind_it_and_after_one_that_is() {
        let Some(_) = letibot_tokencore::apparatus::present(
            "a cgroup v2 tree",
            letibot_tools::Cgroup2::probe().is_ok(),
        ) else {
            return;
        };
        use letibot_sessionlog::registry::{Registry, SessionWiring, Work};

        let r = Registry::new();
        let hub = r
            .create("s-jobs", "", SessionWiring::default())
            .expect("the session registers");
        // `create` rings an open, and an open is drained first by design.
        assert!(matches!(r.next_work(), Some(Work::Open(_))));

        let host = a_host();
        // The watcher is given the daemon's own bell, which is what makes a settlement
        // between turns wake the worker rather than sit until something else asks.
        let watchers = JobWatchers::new(
            &(host.clone() as Arc<dyn ProcessHost>),
            &hub,
            Some(Arc::clone(r.bell())),
        );
        let id = spawn_a_short_job(&host, "0.2");
        watchers.watch(id.0.clone());
        wait_for_settlement(&hub, &id.0);

        // **The operator types while the job is ending.** Their line is in the hub's
        // queue before the wake is drained; it must come out of `next_work` first.
        let head = hub.attach("tui", "dead", Default::default(), 0);
        hub.submit(
            &head.head_id,
            "c1",
            0,
            letibot_sessionlog::CommandKind::Prompt {
                text: "answer me first".into(),
            },
        );
        match r.next_work() {
            Some(Work::Command(id, cmd)) => {
                assert_eq!(id, "s-jobs");
                assert!(matches!(
                    cmd.kind,
                    letibot_sessionlog::CommandKind::Prompt { .. }
                ));
            }
            // `Work` is not `Debug` — it carries a `QueuedCommand` — so the failure
            // names the expectation rather than the value.
            _ => panic!("the operator's line must be served before the job's completion"),
        }
        match r.next_work() {
            Some(Work::Woken(id)) => assert_eq!(id, "s-jobs", "the job's completion wakes it"),
            _ => panic!("the completion must arrive as a wake, after the operator's line"),
        }
    }

    /// **A subagent settles through the job channel, because it IS one.**
    ///
    /// The operator's ruling: *"i think it should arrive the same way backgrounded jobs
    /// complete. in a way agent is a background job."* `task` already returns the same
    /// `Backgrounded` outcome `bash --background` does, so the sink above already arms a
    /// watcher for a subagent's handle. Before this, that watcher called
    /// `ProcessHost::wait_job` on a name the host had never heard: the host answers
    /// `NeverStarted`, which `watch_one` reads as *"a boot window and not a finish"*, and
    /// the thread then re-asked every `WATCH_CHUNK` until the session closed. So **the
    /// completion was never queued, the bell was never rung, and a thread was leaked per
    /// `task` call for the session's life** — the parent turn ended, the subagent
    /// finished, and the one process that could act on it was never told.
    ///
    /// This asserts the two things that were missing and the wake that carries them. On
    /// the code before the dispatch it does not merely fail: it times out with an empty
    /// queue, because nothing in the old path could ever put anything in it.
    #[test]
    fn a_subagent_settles_through_the_jobs_own_channel() {
        use letibot_sessionlog::registry::{Registry, SessionWiring, Work};

        let r = Registry::new();
        let hub = r
            .create("s-tasks", "", SessionWiring::default())
            .expect("the session registers");
        assert!(matches!(r.next_work(), Some(Work::Open(_))));

        // A host is still required: a session has one channel and it is built with both
        // waits. A subagent simply never reaches this one.
        let Some(_) = letibot_tokencore::apparatus::present(
            "a cgroup v2 tree",
            letibot_tools::Cgroup2::probe().is_ok(),
        ) else {
            return;
        };
        let host = a_host();

        let runner = Arc::new(FakeTask::new("s-1-sub-1"));
        let watchers = JobWatchers::new(
            &(host as Arc<dyn ProcessHost>),
            &hub,
            Some(Arc::clone(r.bell())),
        )
        .with_tasks(&(runner.clone() as Arc<dyn TaskRunner>));

        watchers.watch("s-1-sub-1".into());
        // The child answers. Nothing else happens: there is no job by that name.
        runner.finish("the child answered");

        let done = wait_for_completion(&watchers);
        assert_eq!(
            done.kind,
            BackgroundKind::Subagent,
            "one channel, and the field that lets the sentence be true for each"
        );
        assert_eq!(done.state, "done", "the same word the pane's row carries");
        assert!(
            done.detail.contains("the child answered"),
            "the model is handed what the child said, not only that it stopped: {:?}",
            done.detail
        );
        assert!(
            done.command.is_empty(),
            "a subagent runs no command, and that field must not quietly hold an answer"
        );

        assert!(
            matches!(r.next_work(), Some(Work::Woken(id)) if id == "s-tasks"),
            "and it WAKES the worker — a settlement that lands between turns is a \
             condition nobody acts on unless this rings"
        );
        assert!(
            watchers.take_completions().is_empty(),
            "a completion is taken once, not left for the next wake"
        );
    }

    /// **A GRANDCHILD's settlement reaches its PARENT and rings the tree's ROOT** — R58,
    /// corrected 2026-10-05.
    ///
    /// Two facts, and they are not one fact. The **ring** is the root's, and that is R58's
    /// finding, unchanged: `Sessions::wake` serves only a session in `open`, a child is
    /// adopted into the registry and not into `open`, so `bell.ring_wake(&child)` is a
    /// condition that fires and is discarded — which is why a child's set is built from the
    /// root's ([`JobWatchers::shares_tree`]) and inherits the target.
    ///
    /// The **drain** is the child's, and that is the half R58 got wrong by sharing the
    /// queue with the ring. The session that started the work is the one told, because that
    /// is the session the sentence *a subagent you started has finished* is true of. With
    /// the queue shared, a grandchild's settlement was read by the main session — measured,
    /// 2026-10-05: a subagent's `sleep 25; echo done` arrived in the main session's
    /// conversation, where the handle was not in `job_list` and `job_output` did not know it.
    ///
    /// No process host anywhere: the whole point of the tree is that `task` needs none, so
    /// a test that required a cgroup would be testing the wrong channel.
    #[test]
    fn a_grandchilds_settlement_reaches_its_parent_and_rings_the_tree_root() {
        use letibot_sessionlog::registry::{Registry, SessionWiring, Work};

        let r = Registry::new();
        // The root is the session a daemon drives; the child is the one it does not.
        let root = r
            .create("s-root", "", SessionWiring::default())
            .expect("the root registers");
        let child = r
            .create_under(
                "s-child",
                "",
                SessionWiring::default(),
                Some("s-root".into()),
            )
            .expect("the child registers");
        // Each create rings an Open; drain both so the next ring is the settlement's.
        assert!(matches!(r.next_work(), Some(Work::Open(id)) if id == "s-root"));
        assert!(matches!(r.next_work(), Some(Work::Open(id)) if id == "s-child"));

        // The root's own set — its wake target is its own id.
        let root_watch = JobWatchers::watching_tasks(&root, Some(Arc::clone(r.bell())));

        // The child's runner knows the grandchild's handle, and the child's set is built
        // FROM the root's, which is what gives it the root's ring target.
        let child_runner = Arc::new(FakeTask::new("s-child-sub-1"));
        let child_watch = JobWatchers::watching_tasks(&child, Some(Arc::clone(r.bell())))
            .with_tasks(&(child_runner.clone() as Arc<dyn TaskRunner>))
            .shares_tree(&root_watch);

        // The grandchild settles; the watcher runs in the CHILD's set.
        child_watch.watch("s-child-sub-1".into());
        child_runner.finish("the grandchild answered");

        // **The CHILD drains it, through the door a child actually has.** Not
        // `take_completions` — the between-turns drain belongs to a session a worker
        // drives, and nothing drives a child; `take_mid_turn_completions` is its own round
        // boundary, which is the drain that exists.
        let done = wait_for_mid_turn_completion(&child_watch);
        assert_eq!(done.job, "s-child-sub-1");
        assert_eq!(done.state, "done");
        assert_eq!(
            done.owner, "s-child",
            "and it is stamped with the session that started it, which is the one reading it"
        );
        assert!(
            !done.detail.is_empty(),
            "the row carries what the grandchild said"
        );

        // **And the PARENT's drain is empty** — that is the defect, one assert away.
        assert!(
            root_watch.take_completions().is_empty(),
            "a grandchild's settlement is its parent's to be told, not the main session's"
        );
        assert!(
            root_watch.take_mid_turn_completions().is_empty(),
            "and a root has no mid-turn door at all: R7's wake is its only one"
        );

        // But the bell was rung for the ROOT. A ring for the child would be silence, and
        // `next_work` returning the child here is what that looks like.
        assert!(
            matches!(r.next_work(), Some(Work::Woken(id)) if id == "s-root"),
            "a grandchild's settlement must ring the tree's root, or Sessions::wake \
             answers `Ignored` and the condition is discarded"
        );
    }

    /// **A job a subagent started is the SUBAGENT's to be told, and the parent never sees
    /// it.** The acceptance test for 2026-10-05, in the three sentences this was asked for.
    ///
    /// Measured twice from the parent's own conversation: a `sleep 25; echo done` a subagent
    /// backgrounded arrived in the MAIN session's transcript as *"a job you backgrounded has
    /// ended"*, and the main session's own `job_output` answered *no job called `j17`*. So
    /// the session that was woken was the one session that could not read the job — and
    /// unless the child also got it, the settlement was lost at both ends.
    ///
    /// The three sentences, each one assert below:
    ///
    ///   1. **the child sees it** — drained through the child's own door
    ///      ([`JobWatchers::take_mid_turn_completions`]);
    ///   2. **the parent never does** — the root's between-turns drain is empty while the
    ///      child lives;
    ///   3. **the id is the child's** — `owner` names the child, so the sentence that reaches
    ///      anybody cannot claim *you backgrounded* nor send them to `job_output` for a handle
    ///      their own `job_list` does not hold.
    ///
    /// What the parent's `job_list` holds is the third sentence's other half and is true by
    /// construction rather than by this code: `Harness::job_entries` reads the session's OWN
    /// process host, and a child's host is not its parent's — which is precisely why the
    /// notice must not reach the parent.
    #[test]
    fn a_childs_job_completion_is_the_childs_and_never_the_parents() {
        use letibot_sessionlog::registry::{Registry, SessionWiring, Work};
        let Some(_) = letibot_tokencore::apparatus::present(
            "a cgroup v2 tree",
            letibot_tools::Cgroup2::probe().is_ok(),
        ) else {
            return;
        };

        let r = Registry::new();
        let root = r
            .create("s-root", "", SessionWiring::default())
            .expect("the root registers");
        let child = r
            .create_under(
                "s-child",
                "",
                SessionWiring::default(),
                Some("s-root".into()),
            )
            .expect("the child registers");
        assert!(matches!(r.next_work(), Some(Work::Open(_))));
        assert!(matches!(r.next_work(), Some(Work::Open(_))));

        let host = a_host();
        let root_watch = JobWatchers::watching_tasks(&root, Some(Arc::clone(r.bell())));
        // The child's set keeps its own queue and shares only the tree's bookkeeping and
        // ring target — see `JobWatchers::shares_tree`.
        let child_watch = JobWatchers::new(
            &(host.clone() as Arc<dyn ProcessHost>),
            &child,
            Some(Arc::clone(r.bell())),
        )
        .shares_tree(&root_watch);

        // **1. The child that backgrounded it is the one told.**
        let id = spawn_a_short_job(&host, "0.2");
        child_watch.watch(id.0.clone());
        let done = wait_for_mid_turn_completion(&child_watch);
        assert_eq!(done.job, id.0);
        assert_eq!(
            done.owner, "s-child",
            "stamped with the session whose own action backgrounded it"
        );
        assert!(
            done.command.contains("sleep 0.2"),
            "and with the command, which is what makes the notice actionable: {:?}",
            done.command
        );

        // **2. The parent is not told while the child is there.**
        assert!(
            root_watch.take_completions().is_empty(),
            "the main session must not be handed a job its own host has never heard of"
        );

        // And the ring is still the ROOT's: a settlement must waken a session the daemon
        // can drive, and the child is not one.
        assert!(
            matches!(r.next_work(), Some(Work::Woken(id)) if id == "s-root"),
            "the ring goes up, even though the notice does not"
        );

        // **3. And what a stopped child could not drain goes UP, saying whose it was.**
        // `Harness::close_backend` runs at the end of the child's turn, so from here on
        // nothing will read the child's queue: a job that settles now is the parent's to be
        // told, as somebody else's, rather than a settlement nobody reads.
        child_watch.stop();
        let after = spawn_a_short_job(&host, "0.2");
        child_watch.watch(after.0.clone());
        let handed = wait_for_completion(&root_watch);
        assert_eq!(handed.job, after.0);
        assert_eq!(
            handed.owner, "s-child",
            "handed up, and still naming its owner — `completion_notices` is what turns this \
             into *a session below this one started*, not *you backgrounded*"
        );
    }

    /// **A settlement reaches the session that started it and not its sibling.**
    ///
    /// The *"and to nobody else"* half: two children of one root, each watching its own
    /// subagent, and neither can be told the other's news — each drains its own queue. The
    /// one direction a settlement does travel is UP, when the session that owns it has
    /// stopped ((`JobWatchers::stop`)), which is the only direction that keeps it inside a
    /// line that can still act on it. No host anywhere: `task` needs none.
    #[test]
    fn a_settlement_reaches_the_session_that_started_it_and_not_its_sibling() {
        use letibot_sessionlog::registry::{Registry, SessionWiring, Work};

        let r = Registry::new();
        let root = r
            .create("s-root", "", SessionWiring::default())
            .expect("the root registers");
        let a = r
            .create_under("s-a", "", SessionWiring::default(), Some("s-root".into()))
            .expect("the first child registers");
        let b = r
            .create_under("s-b", "", SessionWiring::default(), Some("s-root".into()))
            .expect("the second child registers");
        for _ in 0..3 {
            assert!(matches!(r.next_work(), Some(Work::Open(_))));
        }

        let root_watch = JobWatchers::watching_tasks(&root, Some(Arc::clone(r.bell())));
        let a_runner = Arc::new(FakeTask::new("a-sub-1"));
        let a_watch = JobWatchers::watching_tasks(&a, Some(Arc::clone(r.bell())))
            .with_tasks(&(a_runner.clone() as Arc<dyn TaskRunner>))
            .shares_tree(&root_watch);
        let b_runner = Arc::new(FakeTask::new("b-sub-1"));
        let b_watch = JobWatchers::watching_tasks(&b, Some(Arc::clone(r.bell())))
            .with_tasks(&(b_runner.clone() as Arc<dyn TaskRunner>))
            .shares_tree(&root_watch);

        a_watch.watch("a-sub-1".into());
        a_runner.finish("from a");
        let a_done = wait_for_mid_turn_completion(&a_watch);
        assert_eq!(a_done.job, "a-sub-1");
        assert_eq!(a_done.owner, "s-a");
        assert!(
            b_watch.take_completions().is_empty(),
            "a sibling's settlement must not surface in this session"
        );
        assert!(
            root_watch.take_completions().is_empty(),
            "nor in the parent's, while the sibling that started it is still there"
        );

        // The other child's own, the same two facts the other way round.
        b_watch.watch("b-sub-1".into());
        b_runner.finish("from b");
        let b_done = wait_for_mid_turn_completion(&b_watch);
        assert_eq!(b_done.job, "b-sub-1");
        assert_eq!(b_done.owner, "s-b");
        assert!(a_watch.take_completions().is_empty(), "still not across");
        assert!(
            root_watch.take_completions().is_empty(),
            "and still not up, while its owner is running"
        );

        // **The child's turn ends**, and one more of its children settles afterwards: up,
        // with its owner's name on it, and still not across to the sibling.
        b_watch.stop();
        let late_runner = Arc::new(FakeTask::new("b-sub-2"));
        let b_after = b_watch
            .clone()
            .with_tasks(&(late_runner.clone() as Arc<dyn TaskRunner>));
        b_after.watch("b-sub-2".into());
        late_runner.finish("after b stopped");
        let handed = wait_for_completion(&root_watch);
        assert_eq!(handed.job, "b-sub-2");
        assert_eq!(handed.owner, "s-b");
        assert!(
            a_watch.take_completions().is_empty(),
            "a stopped sibling's settlement goes UP, never across: a sibling cannot act on \
             a handle it does not hold"
        );
    }

    /// **A child's turn ending does not silence its tree's watchers** — measured 2026-10-04.
    ///
    /// [`JobWatchers::shares_tree`] shared the `stop` flag along with the queue, on the
    /// reasoning that *"a tree closes together"*. It does not: `Harness::close_backend` runs at
    /// the end of a **child's** turn — *"release its substrate now, not when the harness is
    /// dropped"* — so the first subagent to finish set the ROOT's stop flag and every watcher in
    /// the tree broke out of its wait at its next chunk. The operator, on their own head: *"it
    /// suddenly stopped getting job completion events"* — and the shape is in the same
    /// measurement, because a job that settles inside one [`WATCH_CHUNK`] still raced through
    /// the `Happened` arm, so three- and four-second demo jobs kept reporting and a
    /// forty-five-second one did not.
    #[test]
    fn a_childs_close_does_not_stop_its_trees_watchers() {
        use letibot_sessionlog::registry::{Registry, SessionWiring, Work};

        let r = Registry::new();
        let root = r
            .create("s-root", "", SessionWiring::default())
            .expect("the root registers");
        let child = r
            .create_under(
                "s-child",
                "",
                SessionWiring::default(),
                Some("s-root".into()),
            )
            .expect("the child registers");
        assert!(matches!(r.next_work(), Some(Work::Open(_))));
        assert!(matches!(r.next_work(), Some(Work::Open(_))));

        let root_watch = JobWatchers::watching_tasks(&root, Some(Arc::clone(r.bell())));
        let child_watch = JobWatchers::watching_tasks(&child, Some(Arc::clone(r.bell())))
            .shares_tree(&root_watch);
        assert!(
            !root_watch.stopped() && !child_watch.stopped(),
            "neither set is stopped to begin with"
        );

        // The child's turn ends and it releases its substrate — which is the moment the tree's
        // job notices used to die.
        child_watch.stop();
        assert!(child_watch.stopped(), "the child's own set is stopped");
        assert!(
            !root_watch.stopped(),
            "a child's turn ending must not stop the ROOT's watchers: that is every job \
             notice the root was going to get, and it is what the operator saw stop"
        );
    }

    /// A runner that knows exactly one handle and answers when the test says so.
    struct FakeTask {
        handle: String,
        answer: Mutex<Option<String>>,
        settled: std::sync::Condvar,
    }

    impl FakeTask {
        fn new(handle: &str) -> Self {
            FakeTask {
                handle: handle.to_string(),
                answer: Mutex::new(None),
                settled: std::sync::Condvar::new(),
            }
        }

        fn finish(&self, said: &str) {
            *self.answer.lock().expect("fake task") = Some(said.to_string());
            self.settled.notify_all();
        }
    }

    impl TaskRunner for FakeTask {
        fn start(
            &self,
            _prompt: &str,
            _spec: &letibot_tools::builtins::task::TaskSpec,
        ) -> Result<String, String> {
            Err("this runner starts nothing".into())
        }

        /// **`Unknown` for everything but its one handle**, which is the answer that
        /// tells `JobWatchers::watch` a handle is not a subagent — and which a mirror of
        /// this test on the jobs side relies on to keep its own path.
        fn collect(&self, handle: &str, timeout: Duration) -> TaskStatus {
            if handle != self.handle {
                return TaskStatus::Unknown;
            }
            let g = self.answer.lock().expect("fake task");
            let (g, _) = self
                .settled
                .wait_timeout_while(g, timeout, |a| a.is_none())
                .expect("fake task");
            match &*g {
                Some(a) => TaskStatus::Done { answer: a.clone() },
                None => TaskStatus::Running { note: None },
            }
        }
    }

    /// The one completion, or a deadline. A poll rather than a `job_wait`: what is being
    /// waited for is another thread's queue push, and a test that blocked on a condvar
    /// this file does not own would be testing its own sleep.
    fn wait_for_completion(w: &JobWatchers) -> JobCompletion {
        wait_for(w, |w| w.take_completions())
    }

    /// **The child's own door** — the same wait, drained the way a CHILD's round boundary
    /// drains. Which door a settlement comes through is the whole of the routing question, so
    /// the two are asserted through the two functions rather than through one of them twice.
    fn wait_for_mid_turn_completion(w: &JobWatchers) -> JobCompletion {
        wait_for(w, |w| w.take_mid_turn_completions())
    }

    fn wait_for(w: &JobWatchers, drain: fn(&JobWatchers) -> Vec<JobCompletion>) -> JobCompletion {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(c) = drain(w).into_iter().next() {
                return c;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "no completion within 10s — nothing queued it, or it is on a queue this \
                 session does not drain"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}
