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
//! **Since R79 a drain does not mean a delivery.** Both doors take the queue through
//! [`crate::delivery::sort`], which hands over at once only what the session's work waits on
//! and HOLDS the rest ([`JobWatchers::hold`]) for one batch at the next turn boundary.
//!
//! The bell is rung for the second one, because a settlement lands **between turns**,
//! when the worker is blocked in `Registry::next_work` and nothing else would wake it.
//! It is the same bell monitors ring for a firing, and the ordering rule is the same
//! one `Bell::next_any` keeps: a wake has nobody waiting on it, a head that pressed
//! enter does — so a queued prompt is **always** served before a completion.
//!
//! # The ring names the session that OWNS the settlement, and that is the correction
//!
//! A settlement belongs to the session that started the work, so the ring names **that**
//! session ([`JobWatchers::me`]) — the parent, at every depth, never the tree's root. What
//! the ring has to reach is a session that can be *served*, and the daemon serves a session
//! it does not hold by handing the wake to the thread that does (see
//! [`letibot_sessionlog::hub::Hub::wake_its_own_reader`], and `Sessions::wake`). Ringing the
//! root instead put the ring and the drain on two different sessions: a grandchild's
//! settlement was queued for its own parent and woke the *main* session, which drained its
//! own queue, found nothing, and ran no turn while the notice sat unread below it. That is
//! R58's own `Ignored`, arrived at from the other side.
//!
//! The exception is deliberate and it is one line: a settlement whose owning session has
//! **stopped** is handed up a level ([`JobWatchers::stop`]), and then the ring names the
//! level above, because that is the session the settlement is now waiting for.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, SystemTime};

use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::registry::Bell;
use letibot_tokencore::store::{JobRecord, Store};
use letibot_tools::builtins::task::{TaskRunner, TaskStatus};
use letibot_tools::exec::{JobId, JobState, ProcessHost, Waited};
use letibot_tools::runtime::WantedNow;
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
    /// **What the job's memory did** — [`letibot_tools::exec::JobView::memory_clause`], read
    /// off the view at the settle with the rest of the numbers. `None` for a subagent and
    /// for a job nobody capped.
    pub memory: Option<String>,
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
    /// **The name the caller gave this job**, or `None` when nobody said one — the same
    /// string `bash(slug: …)` stated, read off the view at the last moment it exists.
    ///
    /// Here rather than looked up when the notice is built, for the reason [`Self::command`]
    /// is: a settlement is delivered between turns, by which time the job is usually reaped
    /// and there is no table left to read a name off. It is the field that lets the notice
    /// say `release-build · j65` — [`letibot_tools::exec::job_label`]'s rule, which is what
    /// the `bash` result, `/job` and the tools all use — instead of a bare counter.
    ///
    /// **For a subagent, the child's name** — `task_start`'s worktree slug or the one `task`
    /// derived, read off the runner at the settlement — which
    /// [`crate::harness::subagent_notice`] labels the child with. `None` for a runner that keeps
    /// no names.
    pub slug: Option<String>,
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
    /// **When the settlement was queued** — the moment the watcher saw it end (R79).
    ///
    /// What a held result's wait is measured from, by the starvation cap and by the delivery
    /// record ([`crate::delivery`]). Wall-clock rather than an `Instant` because it is written
    /// down: a record's `arrived_at` is read by somebody who was not running this process.
    pub arrived: SystemTime,
}

/// **What one set is waiting on** — handle → (which kind, the name the caller gave it).
///
/// A named alias because it is spelled in three signatures: the field, and the two watcher threads
/// that carry a clone of it. Three spellings of a `HashMap<String, (BackgroundKind, Option<String>)>`
/// is a reader re-parsing a type to see it is the same one.
type Mine = Arc<Mutex<HashMap<String, (BackgroundKind, Option<String>)>>>;

/// **One thing this session is watching, as the activity reads it** — a row of
/// [`JobWatchers::activity_facts`]'s snapshot.
///
/// A copy, and the copying is the point: the maps it comes from (`mine`, `wanted`) are written by
/// watcher threads and by the harness, and the activity line is re-derived from inside
/// [`Hub::publish`] — see [`WatchedNow`] for the measurement and the rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watched {
    /// The handle a call was given back (`j8032`, `s-kid`).
    pub handle: String,
    /// Which of the two it is — the word a sentence about it uses.
    pub kind: BackgroundKind,
    /// The name the caller gave it, when it has one (`bash(slug: …)`).
    pub slug: Option<String>,
    /// **What the session said it wants of it** (R79) — a wait, a promoted foreground call, or
    /// `wake: "now"` — as of the last rebuild. Carried in the snapshot rather than looked up per
    /// handle, because the lookup would take `wanted`'s lock on a publish.
    pub wanted: Option<WantedNow>,
}

/// **The three facts the activity line is made of, taken without waiting on a lock a watcher
/// holds.**
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActivityFacts {
    /// A settlement is queued for this session and nobody has drained it — see
    /// [`JobWatchers::pending`].
    pub pending: bool,
    /// How many results are held for the model's next break — see [`JobWatchers::held_count`].
    pub held: usize,
    /// The jobs and children it is watching, with what it asked of each.
    pub at_work: Arc<Vec<Watched>>,
}

/// **The two maps the activity's snapshot is built from, and the snapshot itself.**
///
/// # Why this type exists
///
/// The activity line is re-derived from inside [`Hub::publish`] (`Hub::refresh_activity`), and a
/// publish is made from places that hold a session's own state — the merge queue's state machine
/// publishes a warning from inside `pass`/`apply_gate`. So **nothing reachable from a publish may
/// block on a lock a non-queue thread can hold**, and the maps below are exactly such locks:
/// `mine` is written by [`JobWatchers::watch`] and by every watcher thread as it ends, `wanted` by
/// the harness and by the delivery sort.
///
/// Measured 2026-10-29 (`harnessd 0.5.1`, pid 53775, `main` at `706f4147`): the queue's thread sat
/// in `apply_gate → judged → raise → publish_for_repository → Hub::publish → refresh_activity →
/// activity::facts_of → JobWatchers::pending → Mutex::lock` for 8½ hours, 795 of 795 samples.
/// [`JobWatchers::stop`] held the settlement queue while it published, and the publish asked for
/// that same queue — one thread, one non-reentrant mutex, and every publisher on the box behind
/// it. The lease's heartbeat stopped, a target stayed `gating` with no gate (its verdict had been
/// taken out of the slot before the application began), and an approved branch sat `queued`.
///
/// # The rule this type keeps
///
/// `snapshot` is a **leaf** lock: it is held only across a clone or a store of an `Arc`, never
/// across a call that can block, so it cannot be part of a cycle. A publisher takes nothing else
/// from this set — `pending` and `held` are atomics ([`Settlements::any`], `JobWatchers::held_now`)
/// — so a publish does not wait here at all.
///
/// The order the writers keep is `mine` → `wanted` → `snapshot`; the reader takes only `snapshot`.
struct WatchedNow {
    mine: Mine,
    wanted: Arc<Mutex<HashMap<String, WantedNow>>>,
    snapshot: Mutex<Arc<Vec<Watched>>>,
}

impl WatchedNow {
    fn new(wanted: Arc<Mutex<HashMap<String, WantedNow>>>) -> Arc<WatchedNow> {
        Arc::new(WatchedNow {
            mine: Arc::new(Mutex::new(HashMap::new())),
            wanted,
            snapshot: Mutex::new(Arc::new(Vec::new())),
        })
    }

    /// **Rebuild the snapshot from the two maps** — called by whatever moved them, once it has let
    /// go of them.
    ///
    /// Sorted by handle, because the maps are `HashMap`s and the activity is compared for equality
    /// to decide whether to publish it: an order that wandered would republish an unchanged line.
    fn rebuild(&self) {
        let mine = self.mine.lock().expect("job watchers");
        let wanted = self.wanted.lock().expect("wanted results");
        let mut out: Vec<Watched> = mine
            .iter()
            .map(|(handle, (kind, slug))| Watched {
                handle: handle.clone(),
                kind: *kind,
                slug: slug.clone(),
                wanted: wanted.get(handle).copied(),
            })
            .collect();
        drop(wanted);
        drop(mine);
        out.sort_by(|a, b| a.handle.cmp(&b.handle));
        *self.snapshot.lock().expect("watched now") = Arc::new(out);
    }

    /// **The snapshot, taken without waiting** — a leaf lock, cloned, released.
    fn now(&self) -> Arc<Vec<Watched>> {
        Arc::clone(&self.snapshot.lock().expect("watched now"))
    }
}

/// **One session's settlement queue, and the flag the activity reads instead of its lock.**
///
/// `any` is `true` exactly while `queue` is non-empty: every push sets it **inside** the lock and
/// every drain clears it inside the lock, so a reader that loads `false` cannot have missed a
/// settlement that was already there. A reader that loads `true` may find the queue empty by the
/// time the drain's turn starts — two reads, two instants — and that is a value the turn
/// republishes, not a fact gone stale.
///
/// The queue itself is never taken on a publish path; see [`WatchedNow`].
struct Settlements {
    queue: Mutex<VecDeque<JobCompletion>>,
    any: AtomicBool,
}

impl Settlements {
    fn new() -> Arc<Settlements> {
        Arc::new(Settlements {
            queue: Mutex::new(VecDeque::new()),
            any: AtomicBool::new(false),
        })
    }

    /// Push under the lock, and say so — one of the two places `any` moves.
    fn push(&self, c: JobCompletion) {
        let mut g = self.queue.lock().expect("job completions");
        g.push_back(c);
        self.any.store(true, Ordering::SeqCst);
    }

    /// Everything queued, taken — the other place `any` moves.
    fn drain(&self) -> Vec<JobCompletion> {
        let mut g = self.queue.lock().expect("job completions");
        let out: Vec<JobCompletion> = g.drain(..).collect();
        self.any.store(false, Ordering::SeqCst);
        out
    }
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
    /// **Where settlements are written down**, when this session has a store — see
    /// [`JobRecorder`]. `None` until the harness says otherwise, which is every test and every
    /// storeless run.
    recorder: Arc<Mutex<Option<JobRecorder>>>,
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
    completions: Arc<Settlements>,
    /// **The queue of the set this one was built from, and whose session owns it** —
    /// the parent's, when this is a child's ([`JobWatchers::shares_tree`]).
    ///
    /// The queue has two writers, and both are this set handing something up that its own
    /// session can no longer be told: [`JobWatchers::stop`], carrying what is left in this
    /// queue when the session closes, and [`SettlementQueue::push`], for a settlement that
    /// arrives after the owner has stopped. A session that has finished its turn cannot be told
    /// anything more, and a settlement of its own left in its queue would be a
    /// settlement nobody reads — the shape R58 was built to prevent, one level
    /// further down. The id beside it is the level above's **session**, because that is
    /// who the ring for a handed-up settlement has to name: the settlement is waiting
    /// for the parent now, and the parent is where it must be looked for.
    parent: Option<(Arc<Settlements>, String)>,
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
    /// **Settlements taken off the queue and HELD** (R79): nothing in this session's plan waits
    /// on them, so they are handed over in one batch at the next break rather than one at a time
    /// into whatever the model is doing. Filled and emptied by [`crate::delivery`]; per session,
    /// like `completions`, and handed up with it by [`JobWatchers::stop`].
    held: Arc<Mutex<Vec<JobCompletion>>>,
    /// **How many results are held, as an atomic** — the value [`JobWatchers::held_count`] answers,
    /// so the activity does not have to take `held`'s lock on a publish (see [`WatchedNow`]).
    /// Written inside `held`'s lock by [`JobWatchers::hold`] and by both takes.
    held_now: Arc<AtomicUsize>,
    /// **The held counts last published to the hub** — `(jobs, subagents, failed)` — so
    /// [`SessionEvent::HeldResults`] goes out when the set CHANGES and not on every touch (a
    /// re-sort that takes and puts back the same results changes nothing a head draws). Shared
    /// with `held`, since it describes it.
    held_said: Arc<Mutex<(u64, u64, u64)>>,
    /// **Handles this session has said it wants the moment they end** (R79) — a wait on one, a
    /// foreground call promoted out from under it, or `wake: "now"` at its start. See
    /// [`WantedNow`]. Per session: a want is the asking session's, never its tree's.
    wanted: Arc<Mutex<HashMap<String, WantedNow>>>,
    /// **What THIS session is watching, and which kind each handle is** — the jobs and children
    /// whose settlement will reach this session's model, and the fact a sentence about one has to
    /// get right (`job j8032` or `child fix-login`).
    ///
    /// **Not [`JobWatchers::watching`]** (R58): that set is the TREE's, shared so a settlement is
    /// watched once wherever it is handed up, and a parent's jobs sitting in it are not work the
    /// CHILD is waiting on. This one is per set, like the completion queue and the wants, because
    /// the question it answers — *what does the model of THIS session owe a turn to* — is per
    /// session.
    ///
    /// The kind is recorded here, once, at [`JobWatchers::watch`], which already asks the runner
    /// which path a handle takes; a reader that called `TaskRunner::collect` instead would be taking
    /// a slot's lock from inside a hub publish, and a lock taken on the publish path is how a
    /// session wedges on a random event.
    ///
    /// **And the NAME the caller gave it**, when it has one. A model may refer to a job by its id or
    /// by its slug (`bash(slug: …)`; `job_output` takes either), and a row that says
    /// `needs: job release-build` is a row the plan is blocked on just as much as one that says
    /// `j65`. The delivery decision has always read the slug; the activity could not, because the
    /// handle was all this map carried — so a session waiting on a named job read `quiet`, and
    /// `wait` was told nothing more would happen. Recorded at `watch` from the live view, which is
    /// the only moment it can be read.
    ///
    /// **Read through [`WatchedNow`]'s snapshot, never directly, by the activity** — this map is
    /// written by watcher threads, and the activity is re-derived from inside a hub publish.
    watched: Arc<WatchedNow>,
    /// **The tree's ROOT** — the one session in a subagent tree that a head can be attached
    /// to, and therefore the one a permission card can be drawn by.
    ///
    /// **Not the ring target**, and the two were the same field until the tree was read as
    /// what it is (a supervision tree): a card needs a session with a *head*, an exit needs the
    /// session that *started* the work. Read only by [`JobWatchers::tree_root`], which
    /// `subagent_ask_target` addresses a child's ask with.
    root: String,
    /// **What this session's model has been handed, and how many of its turns have ended since** —
    /// the record the end-of-turn plan check reads to give a row that waited on a job or child a
    /// grace after the ending. See [`JobWatchers::grace_left`]. Per session, like `held`: what the
    /// model read is this model's.
    read: Arc<Mutex<ReadRecord>>,
}

/// **The read record** behind [`JobWatchers::grace_left`]: each delivered completion's handle (and
/// slug), with when it ended and how many turns had ended when it was delivered.
#[derive(Debug, Default)]
struct ReadRecord {
    /// Turns this session has ENDED — counted by the harness after every turn's rounds
    /// ([`JobWatchers::turn_ended`]).
    turns_ended: u64,
    /// **When the last of those turns ended** — the line between a result that arrived while the
    /// model was WORKING and one that arrived while it was idle (R79, the operator 2026-10-11:
    /// *"jobs were ran for a reason and held for a reason. once turn ends no reason to hold"*).
    /// `None` until a turn has ended in this process.
    ended_at: Option<SystemTime>,
    /// Handle or slug → (when it ended, `turns_ended` at the moment it was delivered).
    seen: HashMap<String, (SystemTime, u64)>,
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
        let wanted: Arc<Mutex<HashMap<String, WantedNow>>> = Arc::new(Mutex::new(HashMap::new()));
        Arc::new(JobWatchers {
            host: Some(Arc::downgrade(host)),
            recorder: Arc::new(Mutex::new(None)),
            hub: Arc::downgrade(hub),
            tasks: None,
            completions: Settlements::new(),
            // A root has no level above it: nothing it fails to drain can be handed up.
            parent: None,
            me: hub.session_id(),
            bell,
            stop: Arc::new(AtomicBool::new(false)),
            watching: Arc::new(Mutex::new(HashSet::new())),
            settled: Arc::new(Mutex::new(HashSet::new())),
            held: Arc::new(Mutex::new(Vec::new())),
            held_now: Arc::new(AtomicUsize::new(0)),
            held_said: Arc::new(Mutex::new((0, 0, 0))),
            wanted: Arc::clone(&wanted),
            watched: WatchedNow::new(wanted),
            // A root session is its own tree: the root and its own id are one.
            root: hub.session_id(),
            read: Arc::new(Mutex::new(ReadRecord::default())),
        })
    }

    /// **Tell this set where to leave a record of what it settles.** See [`JobRecorder`].
    ///
    /// A setter rather than a constructor parameter: `JobWatchers::new` is called in twelve
    /// places and all but a couple of them are tests, so a parameter would be twelve edits to
    /// carry one value that every one of them wants to leave `None`.
    ///
    /// **Safe to call after construction, which is the only order the harness can offer.** The
    /// value is read when a queue is BUILT — per watch, well after this — so a recorder set a
    /// line after `new` reaches every queue that follows it. Nothing can be watched in between:
    /// a watch is armed by a tool result, and no tool has run at that point in a session's life.
    pub fn record_jobs_into(
        &self,
        path: std::path::PathBuf,
        session_id: String,
        instance: &str,
        placement: Option<String>,
    ) {
        *self.recorder.lock().expect("job recorder") = Some(
            JobRecorder::new(path, session_id, self.hub.clone())
                .owned_by(instance)
                .on(placement),
        );
    }

    /// **A job is a row from the moment it exists, not from the moment it ends.**
    ///
    /// The reason is the daemon's death: a job that is running when the process goes is never
    /// settled by anything, so without this write it left no trace at all — and a handle the
    /// operator read in the pane an hour ago would be a handle the store had never heard of.
    ///
    /// Called where the daemon first learns a job exists ([`JobWatchSink::emit`]), which is also
    /// where the watcher is armed; the two belong together because both are *this job is now a
    /// thing in the world*.
    ///
    /// **What this side cannot know is the command.** The event that announces a backgrounded job
    /// carries the outcome and no arguments, so the command is read at the settlement, from the
    /// live view, which its own comment calls *"the last moment it exists"*. The row is therefore
    /// written with `how` and a `running` state, and the settlement fills in the rest — an empty
    /// field never erases a recorded one.
    ///
    /// `ran_for_ms` is how long it ran BEFORE it was handed off, so for a running job it is a
    /// floor rather than a total; the settlement replaces it with the whole span.
    pub fn record_start(&self, handle: &str, how: &str, ran_for_ms: u64) {
        let Ok(g) = self.recorder.lock() else {
            return;
        };
        if let Some(rec) = g.as_ref() {
            rec.start(handle, how, ran_for_ms);
            // **And what a restart's reminder needs to say how to run it again** — the command,
            // its name and its directory, read from the live view while the job exists (the
            // operator: *"remind the model about jobs"*). See `crate::subagent_record`.
            let view = self
                .host
                .as_ref()
                .and_then(Weak::upgrade)
                .and_then(|h| h.jobs().into_iter().find(|j| j.id.0 == handle));
            if let Some(view) = view {
                rec.context(handle, &view.command, view.slug.clone(), &view.cwd);
                // **And the process itself**, which is what a restart has to find. See
                // [`JobRecorder::process`].
                rec.process(&view);
            }
        }
    }

    /// **A set for a session that cannot start processes but can still spawn a
    /// subagent.**
    ///
    /// `task` is `Access::Session` and needs no host — a read-only seat can hand work to
    /// a child — so keying the whole watcher set on the *process* host would have made
    /// the wake work only where a command could also be backgrounded. It is the same
    /// channel either way; this is the arm where the job half has nothing to watch.
    pub fn watching_tasks(hub: &Arc<Hub>, bell: Option<Arc<Bell>>) -> Arc<Self> {
        let wanted: Arc<Mutex<HashMap<String, WantedNow>>> = Arc::new(Mutex::new(HashMap::new()));
        Arc::new(JobWatchers {
            host: None,
            recorder: Arc::new(Mutex::new(None)),
            hub: Arc::downgrade(hub),
            tasks: None,
            completions: Settlements::new(),
            parent: None,
            me: hub.session_id(),
            bell,
            stop: Arc::new(AtomicBool::new(false)),
            watching: Arc::new(Mutex::new(HashSet::new())),
            settled: Arc::new(Mutex::new(HashSet::new())),
            held: Arc::new(Mutex::new(Vec::new())),
            held_now: Arc::new(AtomicUsize::new(0)),
            held_said: Arc::new(Mutex::new((0, 0, 0))),
            wanted: Arc::clone(&wanted),
            watched: WatchedNow::new(wanted),
            root: hub.session_id(),
            read: Arc::new(Mutex::new(ReadRecord::default())),
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
            // **Not inherited, and that is the point.** A recorder carries the SESSION its rows
            // belong to, and this set is being built for a different session — so a copy of the
            // parent's would file a child's job in the parent's history. It is set, if at all,
            // by the harness that owns this session, a line after this returns.
            recorder: Arc::new(Mutex::new(None)),
            completions: Arc::clone(&self.completions),
            parent: self.parent.clone(),
            me: self.me.clone(),
            bell: self.bell.clone(),
            stop: Arc::clone(&self.stop),
            watching: Arc::clone(&self.watching),
            settled: Arc::clone(&self.settled),
            held: Arc::clone(&self.held),
            held_now: Arc::clone(&self.held_now),
            held_said: Arc::clone(&self.held_said),
            wanted: Arc::clone(&self.wanted),
            // Its own snapshot, over its own `mine`: what this set watches is this set's, and the
            // map it is built from starts empty for a set built for another session.
            watched: WatchedNow::new(Arc::clone(&self.wanted)),
            root: self.root.clone(),
            read: Arc::clone(&self.read),
        })
    }

    /// **Join this session's watcher to its tree's** (R58).
    ///
    /// A child keeps everything that is genuinely its own — the hub its rows are
    /// published to, the host and the runner that know its handles, and **the
    /// completions queue**: a settlement belongs to the session that started it
    /// (`JobCompletion::owner`), and a shared queue is what delivered a child's job to
    /// the main session as *"a job you backgrounded"*. What it takes from the tree is
    /// the **watching/settled sets** that make `delivering` tree-wide and stop one
    /// settlement being queued twice, the **level above** — whose queue a stopped
    /// session's settlements go to, and whose id the ring for them names — and the
    /// **root**, which is the tree's head and belongs to the ask route rather than to
    /// this one.
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
    /// `tree` is **the parent's own set** — the one its harness filled into the slot its
    /// runner reads, so the level above is `tree.me` and this needs no parent-chain walk:
    /// the set it is built from already knows whose child this is.
    pub fn shares_tree(self: Arc<Self>, tree: &Arc<JobWatchers>) -> Arc<Self> {
        Arc::new(JobWatchers {
            host: self.host.clone(),
            hub: Weak::clone(&self.hub),
            tasks: self.tasks.clone(),
            // Not inherited, for the reason `with_tasks` gives above: the recorder names the
            // session, and this set is another session's.
            recorder: Arc::new(Mutex::new(None)),
            // **Its own**, and the one field this constructor no longer takes from the
            // tree. The child's queue is where its own settlements are drained from.
            completions: Arc::clone(&self.completions),
            // And the tree's is where what it can no longer drain is handed up — carrying the
            // parent's own id, which is who the ring for a handed-up settlement names.
            parent: Some((Arc::clone(&tree.completions), tree.me.clone())),
            me: self.me.clone(),
            bell: self.bell.clone(),
            stop: Arc::clone(&self.stop),
            watching: Arc::clone(&tree.watching),
            settled: Arc::clone(&tree.settled),
            // **Its own**, for the queue's reason: what this session holds and what it asked
            // for are this session's.
            held: Arc::clone(&self.held),
            held_now: Arc::clone(&self.held_now),
            held_said: Arc::clone(&self.held_said),
            wanted: Arc::clone(&self.wanted),
            // **Its own**, for `held`'s reason: what this session is waiting on is its own, and a
            // child that inherited its parent's would report the parent's jobs as its work.
            watched: WatchedNow::new(Arc::clone(&self.wanted)),
            root: tree.root.clone(),
            // Its own, for `held`'s reason: what this session's model has read is its own.
            read: Arc::clone(&self.read),
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
    /// A **child** is not in `Sessions::open` — its harness lives on the thread its parent
    /// spawned it on — so `Sessions::wake` cannot run its turn. Since 2026-10-06 that is not
    /// silence: the wake is handed to that thread ([`letibot_sessionlog::hub::Hub::wake_its_own_reader`],
    /// answered by `harness::serve_child`), so a child's between-turns door exists too. What the
    /// hand-over cannot reach is a turn already RUNNING — the child's serving thread is inside
    /// it — and that is the door [`JobWatchers::take_mid_turn_completions`] opens, whose
    /// guard is exactly this root/child difference.
    pub fn take_completions(&self) -> Vec<JobCompletion> {
        self.completions.drain()
    }

    /// **Take them at a round boundary, for the session the daemon does not hold.**
    ///
    /// Empty for the session at the top of a tree, and that is the whole of the
    /// guard: a root's between-turn turn IS the daemon's, so taking a settlement into
    /// a running turn here would be a second delivery path for the one R7 built. A
    /// child below the top is not held by the daemon — its own thread runs its turns,
    /// and its between-turns door is a wake RELAYED to that thread
    /// ([`crate::harness::Harness::wake`], answered by `harness::serve_child`) — and
    /// the round boundary is the only place a turn already RUNNING can be told.
    ///
    /// The two doors read ONE queue, so taking here is what stops a settlement being
    /// delivered twice: the relayed wake that follows finds nothing and runs no turn,
    /// which is the `Ok(None)` [`crate::harness::Harness::wake`] already documents.
    ///
    /// Is this session the top of its tree is a question this set can answer from a
    /// field it already carries: only [`JobWatchers::shares_tree`] gives a set a level
    /// above, and the set at the top is the one the daemon built for itself.
    ///
    /// **R79: the daemon's round boundary no longer drains through this** — it takes the
    /// queue through [`crate::delivery::sort`], for a root as for a child, and only what the
    /// work waits on comes in mid-turn. This stays as the raw routing door the tests below
    /// assert *whose* queue a settlement lands on.
    pub fn take_mid_turn_completions(&self) -> Vec<JobCompletion> {
        if self.parent.is_none() {
            return Vec::new();
        }
        self.take_completions()
    }

    /// **Queue a settlement as a watcher would, without the ring** — the door a settlement that
    /// outlived its session comes back through, and the one the delivery tests use to stand in a
    /// watcher's place.
    ///
    /// **The ring is deliberately not rung here**, because this is not a settlement arriving: it
    /// is one being put back into a set that is about to be rung for its own reasons (the open
    /// that found it, or a test about to call `wake`). A ring from here would be a wake with
    /// nothing behind it for every caller that queues before it is ready.
    pub fn queue(&self, c: JobCompletion) {
        self.completions.push(c);
    }

    /// **Queue a settlement as a watcher would, without the ring** — for a test of the delivery
    /// rules (R79), which needs a completion in the queue without a process ending. Nothing in
    /// the daemon calls it: a settlement is queued by its watcher thread.
    #[doc(hidden)]
    pub fn queue_for_test(&self, c: JobCompletion) {
        self.queue(c);
    }

    /// **Hold this session's settlement queue while `during` runs** — the position a publisher must
    /// not be able to be parked in, stood in on purpose.
    ///
    /// The measurement this file was rewritten for is exactly this state: one thread inside this
    /// queue's lock and publishing, every other publisher on the box queued behind it. Nothing in
    /// the daemon holds the queue across a publish any more (see [`JobWatchers::stop`]) and the
    /// activity does not ask for it at all (see [`WatchedNow`]) — so a test can hold it and still
    /// require a publish to return, and that is what
    /// `tests/activity.rs::a_publish_returns_while_the_settlement_queue_is_held` asserts.
    #[doc(hidden)]
    pub fn hold_settlements_for_test<R>(&self, during: impl FnOnce() -> R) -> R {
        let _held = self.completions.queue.lock().expect("job completions");
        during()
    }

    /// **Say this handle's result is wanted the moment it ends** (R79) — the daemon's half of
    /// [`letibot_tools::runtime::CompletionWanted`]. A want already recorded is not replaced:
    /// `wake: "now"` and a later wait are both *now*, and the first is the one recorded.
    ///
    /// **A want for a result already delivered is not recorded** (R91.17): nothing would ever
    /// deliver it again to take it off, so it would sit in the map for the session's life.
    pub fn want_now(&self, handle: &str, why: WantedNow) {
        if self
            .read
            .lock()
            .expect("read record")
            .seen
            .contains_key(handle)
        {
            return;
        }
        self.wanted
            .lock()
            .expect("wanted results")
            .entry(handle.to_string())
            .or_insert(why);
        // **And the snapshot, because `awaited` reads the want from it.** A want is what makes a
        // running job or child count as work the session is blocked on, so a snapshot left without
        // it would read `quiet` about a session that asked to be told the moment it ended.
        self.watched.rebuild();
    }

    /// Whether this handle was asked for, and how — read by [`crate::delivery`]'s sort.
    pub fn wanted(&self, handle: &str) -> Option<WantedNow> {
        self.wanted
            .lock()
            .expect("wanted results")
            .get(handle)
            .copied()
    }

    /// **Forget a want once its result has been delivered**, so the map holds only what is
    /// still outstanding.
    pub fn delivered(&self, handle: &str) {
        self.wanted.lock().expect("wanted results").remove(handle);
        self.watched.rebuild();
    }

    /// **These completions have just been put into the model's transcript** — recorded with the
    /// number of turns ended so far, so [`JobWatchers::grace_left`] can tell *delivered in the turn
    /// that is ending now* from *delivered a turn ago*. Called by the two places a completion is
    /// worded for the model (`Deliveries::immediates` and `Deliveries::batch`), which is where
    /// *delivered* is true rather than inferred. Keyed by the handle and by the slug, because a
    /// row's `TodoNeed::Job` may name either.
    pub fn read_now(&self, done: &[JobCompletion]) {
        {
            let mut r = self.read.lock().expect("read record");
            let at = r.turns_ended;
            for c in done {
                r.seen.insert(c.job.clone(), (c.arrived, at));
                if let Some(slug) = &c.slug {
                    r.seen.insert(slug.clone(), (c.arrived, at));
                }
            }
        }
        // **And its want is spent, whichever door delivered it** (R91.17). Only the immediate
        // door used to forget it (`delivery::sort`); a result handed over in a batch, or between
        // turns, left its want in the map for the session's life.
        {
            let mut wanted = self.wanted.lock().expect("wanted results");
            for c in done {
                wanted.remove(&c.job);
            }
        }
        // And the snapshot, which carries each handle's want (`WatchedNow`).
        self.watched.rebuild();
        // **And a subagent's settlement is marked delivered in the store**, because this is the
        // one place *the model has been told* is true. A daemon that goes down between a child
        // settling and its parent being told brings that settlement back on the parent's next
        // open (`crate::subagent_record`) — and a delivered one must not come back a second time.
        // **A job's too** (TODO.md R91.5): `job.delivered`, so a result held when the daemon
        // stopped is said by the next one (`Harness::restore_after_restart`) and one already told
        // is not said twice.
        let recorder = self.recorder.lock().expect("job recorder").clone();
        if let Some(rec) = recorder {
            for c in done {
                match c.kind {
                    BackgroundKind::Subagent => rec.subagent_delivered(&c.job),
                    BackgroundKind::Job => rec.job_delivered(&c.job),
                }
            }
        }
    }

    /// **A turn of this session has ended** — counted after its rounds have run, whatever they
    /// came to, by the harness's two doors into `run_rounds`.
    ///
    /// **And a turn that ends with results held rings this session's own wake** (R79, the
    /// operator 2026-10-11: *"jobs were ran for a reason and held for a reason. once turn ends no
    /// reason to hold"*). Holding exists only so a RUNNING turn is not interrupted; once the turn is
    /// over nothing is being interrupted, so the held batch is delivered at once as its own turn —
    /// through the same door an immediate completion between turns takes (`Harness::wake`). The
    /// fact guarded is *held is non-empty at the moment the turn ended*; a completion still in the
    /// queue rang when it was queued, and the bell keeps one ring per session, so a second ring is
    /// harmless. A set with no bell (a test's harness, no daemon) rings nothing.
    pub fn turn_ended(&self) {
        {
            let mut r = self.read.lock().expect("read record");
            r.turns_ended += 1;
            r.ended_at = Some(SystemTime::now());
        }
        if self.held_count() > 0
            && let Some(bell) = &self.bell
        {
            bell.ring_wake(&self.me);
        }
    }

    /// **When this session's last turn ended**, or `None` before its first — what tells a result
    /// that arrived while the model was working from one that arrived while it was idle.
    pub fn last_turn_ended(&self) -> Option<SystemTime> {
        self.read.lock().expect("read record").ended_at
    }

    /// **How much of an ended job's or child's grace is left, or `None` when it has none** — the
    /// fact the end-of-turn plan check stays quiet on (`waits_on_an_undigested_ending`).
    ///
    /// The operator, 2026-10-11: *"when wait on condition happens - say job finishes, we must
    /// delay the nag for a bit to let the model process the result"*. The fact guarded is
    /// *the completion was delivered into the model's transcript and a turn ended after the one it
    /// arrived in*, read off this set's own state rather than off a clock:
    ///
    /// * **ended, not delivered yet** — the completion is still in the queue or held under R79:
    ///   graced;
    /// * **delivered during the turn whose end is being checked now** — the model has just been
    ///   handed it and may not have acted yet: graced;
    /// * **delivered before that** — the model has had a whole turn with it: no grace;
    /// * **never seen here** — a handle from before a restart, or one that ended in another
    ///   session: no grace, the reading `TodoCondition::Job` gives a handle it does not know.
    ///
    /// **And a backstop**, `cap` after the ending (the harness passes R79's own starvation cap,
    /// `Config::held_results_cap`), so a completion that never arrives cannot silence a row for
    /// ever. What is returned is how long is left of it, for the idle clock's deadline.
    ///
    /// Not covered, and said: the instant between a job's process ending and its watcher queuing
    /// the settlement (the watcher is woken by the ending itself, so it is a scheduling gap, not a
    /// poll interval) reads as *never seen here*.
    pub fn grace_left(&self, handle: &str, cap: Duration, now: SystemTime) -> Option<Duration> {
        let left = |ended: SystemTime| {
            cap.checked_sub(now.duration_since(ended).unwrap_or(Duration::ZERO))
                .filter(|l| !l.is_zero())
        };
        let names = |c: &JobCompletion| c.job == handle || c.slug.as_deref() == Some(handle);
        let pending = {
            let q = self.completions.queue.lock().expect("job completions");
            q.iter().find(|c| names(c)).map(|c| c.arrived)
        }
        .or_else(|| {
            let h = self.held.lock().expect("held results");
            h.iter().find(|c| names(c)).map(|c| c.arrived)
        });
        if let Some(ended) = pending {
            return left(ended);
        }
        let mut r = self.read.lock().expect("read record");
        let turns = r.turns_ended;
        // What the backstop has released, and what a full turn has digested, is forgotten — the
        // record holds only what may still be graced.
        r.seen
            .retain(|_, (ended, at)| turns <= *at + 1 && left(*ended).is_some());
        r.seen.get(handle).and_then(|(ended, _)| left(*ended))
    }

    /// **Hold these until the next break** (R79). See [`JobWatchers::held`].
    ///
    /// Says the held set on the hub afterwards ([`JobWatchers::say_held`]) — also when `more` is
    /// empty, because the re-sort that calls this may have taken results out
    /// ([`JobWatchers::take_held_to_resort`]) and promoted every one of them.
    pub fn hold(&self, more: Vec<JobCompletion>) {
        if !more.is_empty() {
            let mut h = self.held.lock().expect("held results");
            h.extend(more);
            self.held_now.store(h.len(), Ordering::SeqCst);
        }
        self.say_held();
    }

    /// **Everything held, taken** — the batch delivered, or the set handed up — and said on the
    /// hub, which then reads nothing held.
    pub fn take_held(&self) -> Vec<JobCompletion> {
        let taken = self.take_held_to_resort();
        self.say_held();
        taken
    }

    /// **Everything held, taken to be decided again** (`delivery::sort`), WITHOUT saying so on
    /// the hub: the sort puts back what is still held with [`JobWatchers::hold`], which says the
    /// result, so a head never sees the set drop to zero and come back within one sort.
    pub fn take_held_to_resort(&self) -> Vec<JobCompletion> {
        let mut h = self.held.lock().expect("held results");
        let taken = std::mem::take(&mut *h);
        self.held_now.store(0, Ordering::SeqCst);
        taken
    }

    /// **The held set's counts — jobs, subagents, and how many of them failed.**
    pub fn held_counts(&self) -> (u64, u64, u64) {
        let h = self.held.lock().expect("held results");
        let jobs = h.iter().filter(|c| c.kind == BackgroundKind::Job).count() as u64;
        let subagents = h.len() as u64 - jobs;
        let failed = h.iter().filter(|c| crate::delivery::failed(c)).count() as u64;
        (jobs, subagents, failed)
    }

    /// **Publish [`SessionEvent::HeldResults`] when the held set's counts changed since the last
    /// one** — the head's counter beside *Responding* (the operator, 2026-10-11: *"what would be
    /// useful is a counter of how much and what kind is held - somewhere close to Responding
    /// ...."*). A set whose hub has gone publishes nothing.
    fn say_held(&self) {
        let now = self.held_counts();
        {
            let mut said = self.held_said.lock().expect("held counts said");
            if *said == now {
                return;
            }
            *said = now;
        }
        if let Some(hub) = self.hub.upgrade() {
            hub.publish(SessionEvent::HeldResults {
                jobs: now.0,
                subagents: now.1,
                failed: now.2,
            });
        }
    }

    /// When the longest-held result arrived, or `None` when nothing is held — what the
    /// starvation cap is read against.
    pub fn oldest_held(&self) -> Option<SystemTime> {
        self.held
            .lock()
            .expect("held results")
            .iter()
            .map(|c| c.arrived)
            .min()
    }

    /// How many results are held right now.
    pub fn held_count(&self) -> usize {
        // The flag, for `pending`'s reason: `held`'s lock is one a session's own thread holds.
        self.held_now.load(Ordering::SeqCst)
    }

    /// **Has a settlement arrived that this session has not been told yet?** — the queue behind
    /// [`JobWatchers::wake`]'s next drain, read by the activity (`crate::activity`) because it is the
    /// window between a job ending and the turn its ending starts.
    ///
    /// Distinct from [`JobWatchers::held_count`], which is a result taken out of this queue and set
    /// aside so a RUNNING turn is not interrupted: both are *the model is owed a turn*, and neither
    /// is a job still running.
    pub fn pending(&self) -> bool {
        // **The flag, not the queue** — see [`Settlements::any`]. A publish reaches this, and a
        // publish that waited on the queue is the 8½-hour deadlock in [`WatchedNow`]'s header.
        self.completions.any.load(Ordering::SeqCst)
    }

    /// **The jobs and children THIS session is waiting on, by handle and kind** — the things whose
    /// settlement will reach its model.
    ///
    /// Read from [`WatchedNow`]'s `mine` rather than from the tree-wide `watching` set, and the
    /// difference is the tree (R58): `watching` is shared so a settlement is watched once wherever it
    /// is handed up, so a parent's job sits in a child's too — and work the child is not waiting on
    /// is not work the child's model is owed a turn for.
    ///
    /// A snapshot of one instant: a watcher that ends between this call and the reader's next breath
    /// leaves the entry here, and what corrects that is the settlement's own
    /// [`crate::activity`] refresh — `Delivery`, and then the turn it wakes.
    pub fn watching_now(&self) -> Vec<(String, BackgroundKind, Option<String>)> {
        let mut out: Vec<(String, BackgroundKind, Option<String>)> = self
            .watched
            .mine
            .lock()
            .expect("job watchers")
            .iter()
            .map(|(h, (k, slug))| (h.clone(), *k, slug.clone()))
            .collect();
        // A stable order, so two readings of one set are one value: the activity is published only
        // when it CHANGES, and a `HashMap`'s iteration order is not a change.
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// **The three facts the activity line is made of, taken without waiting on a lock a watcher
    /// holds** — the invariant [`letibot_sessionlog::hub::Hub::publish`] rests on, and the answer
    /// to the 8½-hour deadlock [`WatchedNow`] records.
    ///
    /// `pending` and `held` are atomics, and the watchers are one **leaf** lock over an `Arc` that
    /// is cloned and released: nothing here can wait on a thread that is itself waiting for
    /// something else, so a publish that reaches this cannot be part of a cycle.
    ///
    /// **What a reader sees, and what it can miss.** Each of the three is one instant and they are
    /// not one instant together — a watcher that ends between the load of `pending` and the clone of
    /// the snapshot is in neither — so the value is a moment, not a guarantee, exactly as
    /// `Hub::work_pending` says of itself. What makes that safe is that every move of these facts
    /// publishes afterwards: a settlement rings the worker (whose turn republishes), `hold`/`take_held`
    /// say the held counts on the hub, and both watcher threads refresh the activity after they take
    /// their handle out. So a publish that reads here always reads the truth as of the read, and a
    /// change that lands in the same breath is announced by the thread that made it.
    pub fn activity_facts(&self) -> ActivityFacts {
        ActivityFacts {
            pending: self.pending(),
            held: self.held_count(),
            at_work: self.watched.now(),
        }
    }

    /// **The session at the top of this tree** — the ROOT, which is where a subagent's ask
    /// belongs.
    ///
    /// The operator's ruling is that a subagent's permission ask *"should surface to the
    /// parent head all the way to the root obviously"*, and a child has no head. The one
    /// session in a subagent tree that has one is the session the daemon holds — which is
    /// exactly this name, inherited through [`JobWatchers::shares_tree`].
    ///
    /// **An exit is not a card, and this is not the ring.** A settlement goes to the session
    /// that started the work ([`JobWatchers::me`]; the module header says why), an ask goes to
    /// the session that can answer it. One tree, two questions.
    pub fn tree_root(&self) -> &str {
        &self.root
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
    /// Setting the flag says *this session will not drain again* — `close_backend` runs when
    /// the session's backend closes, and nothing drains this queue after it (see
    /// [`JobWatchers::take_completions`]). Anything still in the queue at that moment is
    /// a settlement no reader will ever reach: a job that settled between the child's
    /// last round boundary and its turn ending, or a grandchild still working when its
    /// parent finished (`watch_task` deliberately does not stop with the turn, so the
    /// parent can still be reawakened — 2026-10-04).
    ///
    /// So the queue is emptied into the level above, which is a session that is still
    /// running — the child's parent, one level at a time and never the tree's root (the ring
    /// names the parent for the same reason: see the module header). The completion keeps its
    /// `owner`, so the notice at the other end says whose it was rather than claiming *you
    /// backgrounded*.
    ///
    /// **The hand-over and the queue are ordered by one lock.** A watcher pushes
    /// holding this queue's lock and reads the flag there
    /// ([`SettlementQueue::push`]); this drains holding the same lock, after setting the
    /// flag. Whichever of the two runs first, the settlement is either in the queue this
    /// drain takes or pushed past it by the flag — it cannot fall between them.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        let pending: Vec<JobCompletion> = {
            let mut own = self.completions.queue.lock().expect("job completions");
            // **What was HELD goes up with the queue** (R79): a held result is one this session
            // was going to read at its next break, and a stopped session has none. Taken with
            // `take_held_to_resort` — the door that does NOT say so on the hub — because saying it
            // publishes, and a publish from inside this lock is the deadlock this method was the
            // site of: the activity asked for this queue (`JobWatchers::pending`), so one thread
            // took it twice and every publisher in the daemon queued behind that thread. Measured
            // 2026-10-29: the merge queue's worker sat there for 8½ hours. See [`WatchedNow`].
            let mut all = self.take_held_to_resort();
            all.extend(own.drain(..));
            self.completions.any.store(false, Ordering::SeqCst);
            all
        };
        // **Said out here**, where it cannot wait on the queue this drain has just let go of.
        self.say_held();
        // A session at the top of a tree has nowhere to hand anything up to, and a
        // session that drained everything it had leaves nothing to carry.
        let Some((above, who)) = &self.parent else {
            return;
        };
        if pending.is_empty() {
            return;
        }
        {
            let mut g = above.queue.lock().expect("job completions");
            g.extend(pending);
            above.any.store(true, Ordering::SeqCst);
        }
        // **And the ring, naming the session it was handed TO.** The push that queued
        // this rang the owner before the hand-over was even possible; without this ring a
        // settlement that arrives here a moment after that ring would sit in the parent's
        // queue until the parent had some other reason to wake. Naming the parent rather
        // than the owner is the whole of the correction: the settlement is the parent's to
        // read now, and a ring for the session that can no longer be told is a condition
        // that fires and is discarded.
        if let Some(bell) = &self.bell {
            bell.ring_wake(who);
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
            parent: self.parent.clone(),
            stopped: Arc::clone(&self.stop),
            owner: self.me.clone(),
            bell: self.bell.clone(),
            recorder: self.recorder.lock().expect("job recorder").clone(),
        });
        let watching = Arc::clone(&self.watching);
        let settled = Arc::clone(&self.settled);
        let watched = Arc::clone(&self.watched);
        let runner = self.tasks.as_ref().and_then(Weak::upgrade);
        // **Which kind this handle is, decided once and recorded** — see [`WatchedNow`] for
        // why a reader must not ask the runner again later. The same question `watch` has always
        // asked, in the same place, so the two answers cannot disagree.
        let child = runner
            .as_ref()
            .is_some_and(|r| !matches!(r.collect(&job, Duration::ZERO), TaskStatus::Unknown));
        let kind = if child {
            BackgroundKind::Subagent
        } else {
            BackgroundKind::Job
        };
        // **The name, read while the job exists** — see [`WatchedNow`]. A subagent's name is
        // the runner's (`slug_of`), and a job's is the one `bash` was given.
        let slug = if child {
            self.tasks
                .as_ref()
                .and_then(Weak::upgrade)
                .and_then(|r| r.slug_of(&job))
        } else {
            host.as_ref()
                .and_then(Weak::upgrade)
                .and_then(|h| h.job(&JobId(job.clone())))
                .and_then(|v| v.slug)
        };
        if let Some(runner) = runner.filter(|_| child) {
            watched
                .mine
                .lock()
                .expect("job watchers")
                .insert(job.clone(), (kind, slug));
            // **And the activity's snapshot** — a handle in `mine` that the snapshot does not have
            // is a session waiting on a child and reading `quiet`.
            watched.rebuild();
            let watched = Arc::clone(&watched);
            let queue = Arc::clone(&queue);
            let _ = std::thread::Builder::new()
                .name(format!("subagent-watch-{}", &job[..job.len().min(20)]))
                .spawn(move || watch_task(runner, hub, queue, watching, settled, watched, job));
            return;
        }
        // Not a subagent and no host to ask: nothing here can ever settle this handle, so
        // it is dropped rather than left in `watching` — a name that outlives its watcher
        // would make `delivering` true for ever and leak the entry.
        let Some(host) = host else {
            watching.lock().expect("job watchers").remove(&job);
            return;
        };
        watched
            .mine
            .lock()
            .expect("job watchers")
            .insert(job.clone(), (kind, slug));
        watched.rebuild();
        let _ = std::thread::Builder::new()
            .name(format!("job-watch-{}", &job[..job.len().min(20)]))
            .spawn(move || watch_one(host, hub, queue, watching, settled, watched, job));
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
    own: Arc<Settlements>,
    /// **The queue of the level above it and that session's own id**, when there is one.
    /// The id travels with the queue because the hand-up needs both and they must not be
    /// able to disagree about where the settlement went.
    parent: Option<(Arc<Settlements>, String)>,
    /// The owner's stop flag — see [`JobWatchers::stop`]. Read **while holding `own`**,
    /// which is what makes the two orderings exhaustive.
    stopped: Arc<AtomicBool>,
    /// The owner's session id, stamped on every settlement this queue produces — and the
    /// session whose bell is rung for one that goes to `own`, because that is the session
    /// that has to act on it.
    owner: String,
    bell: Option<Arc<Bell>>,
    /// **Where a settlement is written down**, when this session has a store — `None` on a
    /// storeless run and in every test that does not ask for one. Copied in from
    /// [`JobWatchers::recorder`] when the queue is built, which is per watch and therefore
    /// always after the harness has had its say.
    recorder: Option<JobRecorder>,
}

impl SettlementQueue {
    /// **Queue one settlement where its session can still be told, and ring.**
    ///
    /// The `stopped` read is inside the `own` lock on purpose: the alternative is a
    /// settlement pushed into an owner's queue in the instant between the owner's last
    /// drain and the flag being set — which is the shape of settlement-nobody-reads this
    /// whole mechanism exists to avoid.
    fn push(&self, c: JobCompletion) {
        // **The record goes down where the settlement is first known, not where it is read.**
        // `own`/`parent` decide who HEARS about a settlement, and a job's fate does not depend
        // on which session's queue it lands in — so writing after the routing would lose the row
        // for a settlement handed up to an ancestor.
        if let Some(rec) = &self.recorder {
            rec.record(&c);
        }
        let mut g = self.own.queue.lock().expect("job completions");
        if self.stopped.load(Ordering::Relaxed)
            && let Some((above, who)) = &self.parent
        {
            drop(g);
            above.push(c);
            self.ring(who);
            return;
        }
        g.push_back(c);
        self.own.any.store(true, Ordering::SeqCst);
        drop(g);
        self.ring(&self.owner);
    }

    /// The bell, rung for the session that has to act on the settlement — its owner, or
    /// the level above when the owner has stopped. **Never for a session that is neither**:
    /// a ring is a promise that somebody can be woken by it.
    fn ring(&self, session: &str) {
        if let Some(bell) = &self.bell {
            bell.ring_wake(session);
        }
    }
}

/// **The one thing a settlement needs to leave a record of itself** — a path and a session id.
///
/// Not a `Store`: the harness's connection lives on another thread, and a
/// `rusqlite::Connection` is `Send` but not `Sync`. So the watcher opens a connection of its own
/// the first time it has something to write and keeps it — see [`Store::path`] for why a second
/// connection is ordinary here rather than a workaround (WAL plus the five-second
/// `busy_timeout` are what make it harmless).
///
/// **A failure here kills nothing, and it is SAID.** The notice is what a run depends on; the
/// row is the history. So a write that fails leaves a missing row — the honest state — where a
/// panic would take the watcher thread down and lose the settlement as well. It used to be
/// reported nowhere (`let _ = store.put_job(…)`), and R67 item 15 is why it is not: this row is
/// what a restart's `due_todo_rows` reads to decide whether a `when: job` row is due, so a row
/// that silently did not land is a condition that silently never fires. The failure goes to the
/// session's hub as `job_not_recorded`, through a `Weak` for the module's reason (a watcher must
/// not keep a session alive).
#[derive(Clone)]
pub struct JobRecorder {
    path: std::path::PathBuf,
    session_id: String,
    /// Where a write that failed is said. `Weak::new()` for a recorder nobody is watching.
    hub: Weak<Hub>,
    /// Opened on the first write, because the queue that holds this is built per watch and a
    /// store opened at construction would be opened for every job that is never backgrounded.
    store: Arc<Mutex<Option<Store>>>,
    /// The daemon instance that owns these jobs — see [`JobRecorder::owned_by`].
    instance: String,
    /// **Where these processes run** — `None` for this box, a `hosts.toml` name otherwise. See
    /// [`JobRecorder::on`].
    placement: Option<String>,
}

impl JobRecorder {
    pub fn new(path: std::path::PathBuf, session_id: String, hub: Weak<Hub>) -> Self {
        JobRecorder {
            path,
            session_id,
            hub,
            store: Arc::new(Mutex::new(None)),
            instance: String::new(),
            placement: None,
        }
    }

    /// **Where this session's processes run** — `None` for this box, a `hosts.toml` name otherwise.
    /// It is what the `process` row's `host` column says, and what decides how the job's identity
    /// is spelled.
    pub fn on(mut self, placement: Option<String>) -> Self {
        self.placement = placement;
        self
    }

    /// The daemon instance (`Registry::instance`) its job contexts are stamped with.
    pub fn owned_by(mut self, instance: &str) -> Self {
        self.instance = instance.to_string();
        self
    }

    /// **A background job's command, name and directory**, written while it runs — the `job`
    /// row's command (which the start alone could not fill) and the `job_context` row.
    fn context(&self, handle: &str, command: &str, slug: Option<String>, cwd: &str) {
        let redirect = letibot_tools::builtins::output_redirect_path(command);
        self.with_store(handle, "its context", |store| {
            store
                .put_job(
                    &self.session_id,
                    &JobRecord {
                        handle: handle.to_string(),
                        command: command.to_string(),
                        how: String::new(),
                        state: "running".to_string(),
                        produced: 0,
                        elapsed_ms: 0,
                        redirect,
                    },
                )
                .and_then(|_| {
                    store.put_job_context(
                        &self.session_id,
                        &letibot_tokencore::store::JobContext {
                            handle: handle.to_string(),
                            slug,
                            cwd: cwd.to_string(),
                            instance: self.instance.clone(),
                            started_ms: letibot_sessionlog::registry::now_ms(),
                        },
                    )
                })
                .map_err(|e| e.to_string())
        });
    }

    /// **A row that did not land, said on the session's log** — see the type's doc.
    fn not_recorded(&self, handle: &str, what: &str, why: &str) {
        eprintln!(
            "  {} · job {handle}: {what} was not recorded: {why}",
            self.session_id
        );
        if let Some(hub) = self.hub.upgrade() {
            hub.publish(SessionEvent::Warning {
                code: "job_not_recorded".into(),
                detail: format!(
                    "background job `{handle}`: {what} could not be written to the store ({why}). \
                     The job itself is unaffected; what is missing is its row in the jobs \
                     history, and a todo row waiting on it may not be released after a restart."
                ),
                compaction: None,
            });
        }
    }

    /// **The store, opened on first use and handed to the closure.** One place, because two
    /// writers share it, and the open is lazy because a queue is built per watch: a store opened
    /// at construction would be opened for every job that is never backgrounded.
    fn with_store(&self, handle: &str, what: &str, f: impl FnOnce(&Store) -> Result<(), String>) {
        let Ok(mut g) = self.store.lock() else {
            return;
        };
        if g.is_none() {
            match Store::open(&self.path) {
                Ok(s) => *g = Some(s),
                Err(e) => {
                    drop(g);
                    self.not_recorded(handle, what, &format!("the store did not open: {e}"));
                    return;
                }
            }
        }
        let out = match g.as_ref() {
            Some(store) => f(store),
            None => return,
        };
        drop(g);
        if let Err(why) = out {
            self.not_recorded(handle, what, &why);
        }
    }

    /// **A subagent's settlement has reached its parent's model** — see
    /// [`JobWatchers::read_now`]. A handle the store has no subagent row for writes nothing.
    fn subagent_delivered(&self, handle: &str) {
        self.with_store(handle, "its delivery", |store| {
            store
                .deliver_subagent_run(handle)
                .map_err(|e| e.to_string())
        });
    }

    /// **A job's ending has reached the model** — `Store::deliver_job`. See
    /// [`JobWatchers::read_now`].
    fn job_delivered(&self, handle: &str) {
        self.with_store(handle, "its delivery", |store| {
            store
                .deliver_job(&self.session_id, handle)
                .map_err(|e| e.to_string())
        });
    }

    /// **And this job's PROCESS, which outlives the record above** — the supervisor's row
    /// (`store::ProcessRecord`, schema 34). Written here because this is where the live view still
    /// exists: the command, the name, the directory, the pid, the scope it lives in. The identity
    /// is what a restarted daemon will compare against — `start:<n>` for a job on this box,
    /// `cmd:<the command>` for one on a host, which is the marker [`crate::supervise::SshProbe`]
    /// greps for on the far side.
    pub(crate) fn process(&self, view: &letibot_tools::exec::JobView) {
        // **A job of jobs has no process to write down.** Its `pid` is 0 and its branches are
        // records of their own (RH8's book), so a `process` row for it would name a pid nothing on
        // this box can be — the supervisor's identity check would refuse it every tick. The `job`
        // row is written for it all the same ([`JobRecorder::start`]), which is the durable half a
        // restart reads.
        if view.pid == 0 && self.placement.is_none() {
            return;
        }
        let identity = match &self.placement {
            Some(_) => format!("cmd:{}", view.command),
            None => match crate::supervise::identity_of(view.pid) {
                Some(id) => id,
                // **A process that cannot be identified is not written down at all**, rather than
                // written without an identity: a row whose identity is empty cannot be told from
                // a stranger's pid, and the supervisor would signal the wrong process with it.
                //
                // **And it is SAID**, which is the half this used to be silent about: a job with
                // no record is a job no restart can adopt, and a silence here would make that a
                // mystery rather than a sentence.
                None => {
                    self.not_recorded(
                        &view.id.0,
                        "its process record",
                        &format!(
                            "pid {} cannot be identified on this box, so the record would name a \
                             number rather than a process",
                            view.pid
                        ),
                    );
                    return;
                }
            },
        };
        let mut rec = letibot_tokencore::store::ProcessRecord::starting(
            &self.session_id,
            &view.id.0,
            self.placement.clone(),
            view.pid,
        );
        rec.state = letibot_tokencore::store::ProcState::Running;
        rec.identity = identity;
        rec.command = view.command.clone();
        rec.cwd = view.cwd.clone();
        rec.scope = view.owner.to_string();
        rec.pgid = Some(view.pid);
        self.with_store(&view.id.0, "its process record", |store| {
            store.put_process(&rec).map_err(|e| e.to_string())
        });
    }

    /// **The start: a `running` row, with what this side knows.** See
    /// [`JobWatchers::record_start`] for why a job is a row before it ends.
    fn start(&self, handle: &str, how: &str, ran_for_ms: u64) {
        self.with_store(handle, "its start", |store| {
            store
                .put_job(
                    &self.session_id,
                    &JobRecord {
                        handle: handle.to_string(),
                        // **Not knowable here.** `ToolEvent::Finished` carries no arguments, so the
                        // command is read at the SETTLEMENT — the last moment it exists — and an
                        // empty field never erases a recorded one.
                        command: String::new(),
                        how: how.to_string(),
                        state: "running".to_string(),
                        produced: 0,
                        elapsed_ms: ran_for_ms,
                        redirect: None,
                    },
                )
                .map_err(|e| e.to_string())
        });
    }

    /// **Write one settlement's row.**
    ///
    /// `running` is never written here — this is the ending, and the row it updates is the one
    /// [`JobRecorder::start`] wrote. **`how` is empty on purpose**: it is a property of the start
    /// (*who backgrounded this*), and the row keeps what the start recorded, because an empty
    /// field is *not recorded here* (see `Store::put_job`).
    ///
    /// **`redirect` IS computed here**, out of the command this side is the last to see — R41's
    /// path, which a reader needs precisely when the window is empty by construction. Until this
    /// line both writers wrote `None` and the field could never be anything else.
    pub(crate) fn record(&self, c: &JobCompletion) {
        // **A subagent settles through this same queue and is not a job.** `task`'s handle is a
        // session id, `job_list` has never heard of it, and a row here would put a child in the
        // jobs pane — the same confusion R58 removed from the notice.
        if c.kind != BackgroundKind::Job {
            return;
        }
        let redirect = letibot_tools::builtins::output_redirect_path(&c.command);
        // **And the process's own ending**, so the record the supervisor reads says the same
        // thing the row does — and stops being one of the records it still has to look after.
        let (state, exit) = fate_of(&c.state);
        let mut check = letibot_tokencore::store::Check::new(
            state,
            letibot_sessionlog::registry::now_ms() as i64,
            0,
            "settle",
        );
        if let Some(code) = exit {
            check = check.with_exit(code);
        }
        self.with_store(&c.job, "its process record", |store| {
            store
                .check_process(&self.session_id, &c.job, &check)
                .map_err(|e| e.to_string())
        });
        self.with_store(&c.job, "its settlement", |store| {
            store
                .put_job(
                    &self.session_id,
                    &JobRecord {
                        handle: c.job.clone(),
                        command: c.command.clone(),
                        how: String::new(),
                        state: c.state.clone(),
                        produced: c.produced,
                        elapsed_ms: c.elapsed_ms,
                        redirect,
                    },
                )
                .map_err(|e| e.to_string())
        });
    }
}

/// **A settlement's word back into the process states** — one place, because the word is the
/// `JobState` vocabulary and the record's is another, and two spellings of that translation would
/// make a `done` in one reader and a `stopped` in the other.
///
/// `exited N` is `done` with N. A signal, a kill and its deadline are all `stopped`: something
/// ended it on purpose. **`dead` is for a word that says nothing ended it** — a job that was gone
/// when it was looked for — which is `JobState::Dead` and `NotScoped`'s *never ran*.
fn fate_of(word: &str) -> (letibot_tokencore::store::ProcState, Option<i32>) {
    use letibot_tokencore::store::ProcState;
    if let Some(code) = word.strip_prefix("exited ") {
        return (ProcState::Done, code.trim().parse().ok());
    }
    if word.starts_with("gone") || word.starts_with("not run") {
        return (ProcState::Dead, None);
    }
    (ProcState::Stopped, None)
}

fn watch_one(
    host: Weak<dyn ProcessHost>,
    hub: Weak<Hub>,
    queue: Arc<SettlementQueue>,
    watching: Arc<Mutex<HashSet<String>>>,
    settled: Arc<Mutex<HashSet<String>>>,
    watched: Arc<WatchedNow>,
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
                // **And the name, for the same reason and at the same moment.** A job's
                // slug is fixed for its life, so this is not a race — it is the only
                // place a settlement can pick it up, because the notice is built later
                // with no table to read. Nobody having named it is `None`, which is the
                // honest answer and draws exactly the id the notice drew before.
                let slug = view.as_ref().and_then(|v| v.slug.clone());
                let memory = view.as_ref().and_then(|v| v.memory_clause());
                // **And what the job says for itself, when it is a job of jobs** — a fanout's
                // answer IS its per-host table, so the settlement carries it and the model does not
                // have to know to go and read it. `None` for every ordinary job, which is why this
                // changes nothing about what a `bash` settlement says (R93.C3).
                let detail = host
                    .settlement_note(&JobId(job.clone()))
                    .unwrap_or_default();
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
                    memory,
                    kind: BackgroundKind::Job,
                    owner: queue.owner.clone(),
                    job: job.clone(),
                    command,
                    slug,
                    state: word,
                    produced,
                    elapsed_ms,
                    detail,
                    arrived: SystemTime::now(),
                });
                settled.lock().expect("job watchers").insert(job.clone());
                // **And what the session is waiting on has moved** (protocol 52): the job is still
                // in `watching` (this thread removes it a line below) but it is no longer a thing
                // whose ending is coming — the ending is now on the queue, which is `Delivery`. The
                // refresh is here rather than left to the `JobSettled` above, because that was
                // published BEFORE the completion was queued: a reader between the two would have
                // seen neither and drawn a session with work left in it as done.
                hub.refresh_activity();
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
    // **And this session is not waiting on it any more.** Two maps, one moment: a handle left in
    // `mine` would keep the session reading as busy on a job whose watcher has gone.
    watched.mine.lock().expect("job watchers").remove(&job);
    // **And the snapshot, and the value on the hub.** The `refresh_activity` the settlement made
    // ran while this handle was still in `mine` (it is removed here, after the push), so without
    // this the line reads `waiting on: delivery, job j8032` for a job whose watcher has gone.
    watched.rebuild();
    if let Some(h) = hub.upgrade() {
        h.refresh_activity();
    }
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
    watched: Arc<WatchedNow>,
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
            TaskStatus::Done { answer, .. } => {
                let slug = runner.slug_of(&job);
                settled_here("done", first_line(&answer), &job, slug, &queue, &settled);
                // The queue is what the activity reads now — see `watch_one`'s comment.
                if let Some(h) = hub.upgrade() {
                    h.refresh_activity();
                }
                break;
            }
            TaskStatus::Failed { why } => {
                let slug = runner.slug_of(&job);
                settled_here("failed", first_line(&why), &job, slug, &queue, &settled);
                if let Some(h) = hub.upgrade() {
                    h.refresh_activity();
                }
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
    // **And this session is not waiting on it any more.** Two maps, one moment: a handle left in
    // `mine` would keep the session reading as busy on a job whose watcher has gone.
    watched.mine.lock().expect("job watchers").remove(&job);
    // **And the snapshot, and the value on the hub.** The `refresh_activity` the settlement made
    // ran while this handle was still in `mine` (it is removed here, after the push), so without
    // this the line reads `waiting on: delivery, job j8032` for a job whose watcher has gone.
    watched.rebuild();
    if let Some(h) = hub.upgrade() {
        h.refresh_activity();
    }
}

/// Queue one subagent's settlement and ring — the push and the bell, which are the
/// same two things the job path does at the same moment in its own wait.
fn settled_here(
    state: &str,
    detail: String,
    job: &str,
    slug: Option<String>,
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
        // **Its name, as the runner gave it** — `task_start`'s worktree slug or the one
        // `task` derived — so `subagent_notice` labels the child as `fix-login · s-…`, the way a
        // job's notice labels a job. Read off the runner at the settlement, beside the handle,
        // for the reason a job's slug is: the notice is built after the fact. `None` for a
        // runner that keeps no names, which draws the bare handle as before.
        slug,
        state: state.to_string(),
        // It writes no bytes to a stream this end can count. The tokens it
        // generated are in the subagent's own metrics, and `produced` means
        // `job_output`'s denominator everywhere else.
        produced: 0,
        elapsed_ms: 0,
        detail,
        memory: None,
        arrived: SystemTime::now(),
    });
    settled
        .lock()
        .expect("job watchers")
        .insert(job.to_string());
}

/// The first line of what a subagent said, for a one-line notice. The whole of it is
/// in `task_result`, which the notice names.
///
/// `pub(crate)` because the one other writer of a child's notice is
/// [`crate::harness::Harness::deliver_settlements_left_undelivered`], which puts a settlement
/// that outlived its session back into the queue: two spellings of "the first line" would be
/// two notices for one fact.
pub(crate) fn first_line(text: &str) -> String {
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
                outcome:
                    ToolOutcome::Backgrounded {
                        handle,
                        how,
                        ran_for_ms,
                        ..
                    },
                ..
            } = &event
        {
            // **The row first, then the watch.** A job is a row from the moment it exists: a
            // daemon that dies before the watcher settles it would otherwise leave nothing, and
            // the settlement that would have written the row is the one that never happens.
            w.record_start(handle, &how.phrasing(), *ran_for_ms);
            w.watch(handle.clone());
        }
        self.inner.emit(event);
    }
}

#[cfg(test)]
mod tests;
