//! A background job's end, published the moment it is true. The start needs no watcher
//! `bash --background` finishes as `ToolOutcome::Backgrounded` and every head sees it.

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

/// How long one blocking wait lasts before the thread re-checks its handles and the
/// stop flag. A settlement wakes the wait itself, so this bounds only how long a
/// closing session's watcher lingers, never how late a settlement is.
const WATCH_CHUNK: Duration = Duration::from_secs(5);

/// The warning code a job whose dead-check window is running out is filed under: the one sentence
/// that arrives before the harness ends a silent job, and the only chance to say `job_extend`.
pub const JOB_DEAD_SOON: &str = "job_dead_soon";

/// What that warning says. Pure, so the words are asserted without a daemon: it names the job, both
/// numbers, what is about to happen, and the two ways out.
pub fn dead_soon_note(w: &letibot_tools::exec::DeadWarning) -> String {
    format!(
        "`{}` has produced no output and used no CPU for {} of its {} dead-after window. When the \
         whole window is gone it is ended as dead and its output is partial. `job_extend` gives it \
         a longer deadline, which also stops the dead check; `job_kill` ends it now.",
        w.job.0,
        crate::restart::span(w.silent_for),
        crate::restart::span(w.window),
    )
}

/// Which kind of backgrounded thing this is. A subagent *is* a background job (the
/// operator's ruling), so the two share one queue one bell and one wake; they differ only
/// in how you wait and what a settlement is called.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundKind {
    /// A host process from `bash --background`, waited on by `wait_job`.
    Job,
    /// A subagent from `task`, waited on by its own slot's condvar.
    Subagent,
}

/// How the turn that has just ended came to a stop. Only one of the three rings this session's
/// wake for a result it left held: an operator who stopped the turn asked for quiet, and a failed
/// turn has nobody waiting on what its wake would say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnEnd {
    /// The turn answered, or stopped of its own accord.
    Done,
    /// The operator stopped it - `esc esc`, a `/stop`, or a `job_kill` that reached this session.
    Interrupted,
    /// It failed: an outage, a refusal, a broken stream, a guard.
    Failed,
}

/// A backgrounded thing's end, as the model is told it.
///
/// Built by the watcher and drained by the session that owns it - see [`JobWatchers`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobCompletion {
    /// What the job's memory did, read off the view at the settle. `None` for a
    /// subagent and for a job nobody capped.
    pub memory: Option<String>,
    /// Which of the two this was - see [`BackgroundKind`].
    pub kind: BackgroundKind,
    /// Which session started it - the routing is decided by it, and a notice that
    /// reaches another session is false, not merely misplaced. Carried rather than
    /// inferred, because a handed-up settlement sits in an ancestor's queue.
    pub owner: String,
    /// The job's handle, as the backgrounded result printed it.
    pub job: String,
    /// The command it ran, empty when the job was already reaped. Always empty for a
    /// subagent, whose [`JobCompletion::detail`] is a separate field rather than a reused
    /// one - a command field that sometimes holds an answer is one a reader stops trusting.
    pub command: String,
    /// The name the caller gave this job, read off the view at the last moment it exists - a
    /// settlement is delivered between turns, by which time the job is usually reaped. It is
    /// what lets the notice say `release-build · j65` instead of a bare counter.
    pub slug: Option<String>,
    /// What happened to the process: `exited 0`, `signalled 15`, `killed by job_kill`.
    /// For a subagent: `done` or `failed`, the words [`SessionEvent::Subagent`] publishes too.
    pub state: String,
    /// Bytes the job wrote, all streams together - the `job_output` denominator.
    /// Zero for a subagent, which writes no bytes to a stream this end can count.
    pub produced: u64,
    /// Wall time from spawn to settlement.
    pub elapsed_ms: u64,
    /// A subagent's own first word about how it ended: the first line of its answer
    /// or why it failed. It is what makes the wake actionable rather than a nudge; the
    /// whole of it is still in `task_result`.
    pub detail: String,
    /// When the settlement was queued - what a held result's wait is measured from
    /// . Wall-clock rather than an `Instant` because it is written down and read by
    /// somebody who was not running this process.
    pub arrived: SystemTime,
    /// A turn of a child the parent has already been told about, and not its first answer: the
    /// same child, settling again. Never set for a job, which ends once.
    pub later: bool,
}

/// What one set is waiting on - handle → (which kind, the name the caller gave it).
/// An alias because it is spelled in three signatures, and three spellings of one type is a
/// reader re-parsing it to see that it is the same one.
type Mine = Arc<Mutex<HashMap<String, (BackgroundKind, Option<String>)>>>;

/// One thing this session is watching, as the activity reads it - a row of
/// [`JobWatchers::activity_facts`]'s snapshot. A copy, because the maps behind it are
/// written by watcher threads and the activity is re-derived inside [`Hub::publish`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watched {
    /// The handle a call was given back (`j8032`, `s-kid`).
    pub handle: String,
    /// Which of the two it is - the word a sentence about it uses.
    pub kind: BackgroundKind,
    /// The name the caller gave it, when it has one (`bash(slug: …)`).
    pub slug: Option<String>,
    /// What the session said it wants of it - a wait, a promoted foreground call, or
    /// `wake: "now"` - as of the last rebuild. Carried rather than looked up, because the
    /// lookup would take `wanted`'s lock on a publish.
    pub wanted: Option<WantedNow>,
}

/// The three facts the activity line is made of, taken without waiting on a lock a watcher
/// holds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActivityFacts {
    /// A settlement is queued for this session and nobody has drained it - see
    /// [`JobWatchers::pending`].
    pub pending: bool,
    /// How many results are held for a turn that is coming - see [`JobWatchers::held_count`], which
    /// is the count whatever the last turn's ending was.
    pub held: usize,
    /// A settlement has been taken for a turn that has not begun - see
    /// [`JobWatchers::settlement_in_flight`]. The queue is empty here and the turn is still owed.
    pub in_flight: bool,
    /// The jobs and children it is watching, with what it asked of each.
    pub at_work: Arc<Vec<Watched>>,
}

/// The two maps the activity's snapshot is built from, and the snapshot itself.
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

    /// Rebuild the snapshot from the two maps, once the caller has let go of them. Sorted by
    /// handle: the activity is compared for equality to decide whether to publish it, and an
    /// order that wandered would republish an unchanged line.
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

    /// The snapshot, taken without waiting - a leaf lock, cloned, released.
    fn now(&self) -> Arc<Vec<Watched>> {
        Arc::clone(&self.snapshot.lock().expect("watched now"))
    }
}

/// One session's settlement queue, and the flag the activity reads instead of its lock.
/// `any` moves inside the lock on every push and drain, so a reader that loads `false` has
/// missed nothing. The queue itself is never taken on a publish path; see [`WatchedNow`].
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

    /// Push under the lock, and say so - one of the two places `any` moves.
    fn push(&self, c: JobCompletion) {
        let mut g = self.queue.lock().expect("job completions");
        g.push_back(c);
        self.any.store(true, Ordering::SeqCst);
    }

    /// Everything queued, taken - the other place `any` moves.
    fn drain(&self) -> Vec<JobCompletion> {
        let mut g = self.queue.lock().expect("job completions");
        let out: Vec<JobCompletion> = g.drain(..).collect();
        self.any.store(false, Ordering::SeqCst);
        out
    }
}

/// How many settled handles the duplicate guard keeps. A second `watch` for a handle only arrives while its first call is still finishing, so recent history suffices and a long session need not keep every name.
const SETTLED_KEPT: usize = 1024;

/// The handles already settled, so a late duplicate finish cannot publish one settlement twice.
/// Bounded by [`SETTLED_KEPT`], oldest name evicted first, because the set is a guard against a
/// repeat and not a history.
#[derive(Debug, Default)]
struct Settled {
    names: HashSet<String>,
    order: VecDeque<String>,
}

impl Settled {
    fn insert(&mut self, job: &str) {
        if !self.names.insert(job.to_string()) {
            return;
        }
        self.order.push_back(job.to_string());
        while self.order.len() > SETTLED_KEPT {
            if let Some(oldest) = self.order.pop_front() {
                self.names.remove(&oldest);
            }
        }
    }

    fn contains(&self, job: &str) -> bool {
        self.names.contains(job)
    }

    fn len(&self) -> usize {
        self.names.len()
    }
}

/// What one set is holding, in entries - the census a reader needs to tell a set that is waiting on
/// work from one that is keeping history. Every field is something a settlement empties, except
/// [`Retained::settled`], which [`SETTLED_KEPT`] bounds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Retained {
    /// Handles a watcher thread is still blocked on.
    pub watching: usize,
    /// Handles settled, within [`SETTLED_KEPT`].
    pub settled: usize,
    /// Results asked for and not yet delivered.
    pub wanted: usize,
    /// Results held for the next break.
    pub held: usize,
    /// Delivered results still inside their grace - at most the last two turns'.
    pub read: usize,
}

/// The level above this session's watcher set: its queue, whose session it is, its own stop flag,
/// and the level above that. A stopped set has handed its queue up, so what is pushed into it
/// afterwards needs the hop `route` makes - and the level it hands to may have stopped too.
#[derive(Clone)]
struct Up {
    queue: Arc<Settlements>,
    who: String,
    stopped: Arc<AtomicBool>,
    up: Option<Arc<Up>>,
}

impl Up {
    /// Queue settlements at the first level up that has not stopped, or at the top of the tree. The
    /// flag is read under the target queue's lock, so a push that sees it clear lands before `stop`'s
    /// drain and one that sees it set moves on. Returns the session the settlements went to.
    fn deliver(&self, items: impl IntoIterator<Item = JobCompletion>) -> String {
        let mut items: Vec<JobCompletion> = items.into_iter().collect();
        let mut here = self;
        loop {
            let mut g = here.queue.queue.lock().expect("job completions");
            if here.stopped.load(Ordering::Acquire)
                && let Some(above) = &here.up
            {
                drop(g);
                here = above;
                continue;
            }
            g.extend(items.drain(..));
            here.queue.any.store(true, Ordering::SeqCst);
            return here.who.clone();
        }
    }
}

/// The per-session set of watched background jobs and subagents: fed by [`JobWatchSink`] as
/// backgrounded results pass through it, stopped when the session's backend closes.
pub struct JobWatchers {
    /// `None` when this session cannot start processes but can still spawn subagents -
    /// `task` needs no host, and a read-only seat handing work to a child is exactly the case
    /// where a completion nobody is told about is hardest to notice. Not an error state.
    host: Option<Weak<dyn ProcessHost>>,
    /// Where settlements are written down, when this session has a store - see
    /// [`JobRecorder`]. `None` until the harness says otherwise, which is every test and every
    /// storeless run.
    recorder: Arc<Mutex<Option<JobRecorder>>>,
    /// Read by the model but not yet marked delivered in the store: the steering text is built
    /// before it is in a persisted row, so the mark waits for the next boundary or the turn's end.
    undelivered: Mutex<Vec<(BackgroundKind, String)>>,
    hub: Weak<Hub>,
    /// The other thing that backgrounds.
    tasks: Option<Weak<dyn TaskRunner>>,
    /// THIS session's settlements, in the order they happened - its own, plus whatever a
    /// session below handed up because it can no longer be told ([`JobWatchers::stop`]). Per
    /// session, not per tree : a shared queue delivered a child's job as the main session's.
    completions: Arc<Settlements>,
    /// The queue of the set this one was built from, and whose session owns it - the parent's
    /// for a child.
    parent: Option<Arc<Up>>,
    /// A settlement left this queue to be said, and the turn that says it has not begun -
    /// between the two the activity would read quiet and a `wait` would answer the turn that
    /// STARTED the job. [`JobWatchers::turn_began`] ends it.
    in_flight: Arc<AtomicBool>,
    /// This set's own session id - what a completion it queues is stamped with. Read from
    /// the hub, never from the tree: a child's set belongs to the child.
    me: String,
    /// The worker's bell, rung when a completion is queued so a settlement between
    /// turns is not a fact nobody acts on. `None` when there is no daemon - a
    /// harness driven directly by a test has no worker to wake and honestly says so.
    bell: Option<Arc<Bell>>,
    stop: Arc<AtomicBool>,
    /// Jobs with a thread blocked on them, so a re-finished call id cannot start
    /// a second watcher for one job.
    watching: Arc<Mutex<HashSet<String>>>,
    /// Handles whose substrate was taken down under them, read by the watcher when the job ends:
    /// a client's exit after its guest died is not the job's own ending. See
    /// [`JobWatchers::killed_with_the_substrate`].
    killed: Arc<Mutex<HashSet<String>>>,
    /// Jobs already published, so a late duplicate finish cannot publish a
    /// settlement twice. See [`Settled`].
    settled: Arc<Mutex<Settled>>,
    /// Settlements taken off the queue and HELD : nothing in this session's plan waits
    /// on them, so they are handed over in one batch at the next break. Per session, and handed
    /// up with `completions` by [`JobWatchers::stop`].
    held: Arc<Mutex<Vec<JobCompletion>>>,
    /// How many results are held, as an atomic, so the activity does not take `held`'s lock
    /// on a publish (see [`WatchedNow`]). Written inside that lock by [`JobWatchers::hold`] and
    /// by both takes.
    held_now: Arc<AtomicUsize>,
    /// Whether the last turn to end RINGS for what it left held. Only a turn that finished on its
    /// own does, so only then is a held result a turn this session will take by itself - an atomic
    /// beside `held_now` for the same reason, since a publish is what reads it.
    rings_held: Arc<AtomicBool>,
    /// The held counts last published to the hub - `(jobs, subagents, failed)` - so
    /// [`SessionEvent::HeldResults`] goes out when the set changes, not on every touch (a
    /// re-sort that puts the same results back changes nothing a head draws).
    held_said: Arc<Mutex<(u64, u64, u64)>>,
    /// Handles this session has said it wants the moment they end - a wait on one, a
    /// foreground call promoted out from under it, or `wake: "now"` at its start. See
    /// [`WantedNow`]. Per session: a want is the asking session's, never its tree's.
    wanted: Arc<Mutex<HashMap<String, WantedNow>>>,
    /// What THIS session is watching, and which kind each handle is - the work whose settlement
    /// reaches this model, plus the caller's NAME (`bash(slug: …)`), since a plan row may name
    /// a job by its slug and a map holding only the handle read such a session as quiet.
    watched: Arc<WatchedNow>,
    /// The tree's ROOT - the one session a head can be attached to, and therefore the one a
    /// permission card can be drawn by. Not the ring target: a card needs a head, an exit needs
    /// the session that started the work. Read only by [`JobWatchers::tree_root`].
    root: String,
    /// What this session's model has been handed, and how many turns have ended since - what
    /// [`JobWatchers::grace_left`] reads to give a waited-on row a grace after the ending. Per
    /// session, like `held`: what the model read is this model's.
    read: Arc<Mutex<ReadRecord>>,
    /// Snapshots of the tree a background job was spawned into, keyed by handle: a `bash` call hands
    /// one over at the spawn and the watcher reads it at the settlement, because only the watcher sees
    /// both ends of a job (R161).
    sweeps: Arc<Mutex<HashMap<String, letibot_tools::detect::Sweep>>>,
}

/// The [`letibot_tools::detect::JobSweeps`] door, which is this set's own map: a tool says what it
/// spawned into, and the watcher that settles the job reads it. A struct rather than the set itself
/// because the tool layer may not name a `JobWatchers` - and should not be able to.
struct Sweeps(Arc<Mutex<HashMap<String, letibot_tools::detect::Sweep>>>);

impl letibot_tools::detect::JobSweeps for Sweeps {
    fn keep(&self, job: &str, sweep: letibot_tools::detect::Sweep) {
        self.0
            .lock()
            .expect("job sweeps")
            .insert(job.to_string(), sweep);
    }
}

/// The read record behind [`JobWatchers::grace_left`]: each delivered completion's handle (and
/// slug), with when it ended and how many turns had ended when it was delivered.
#[derive(Debug, Default)]
struct ReadRecord {
    /// Turns this session has ENDED - counted by the harness after every turn's rounds
    /// ([`JobWatchers::turn_ended`]).
    turns_ended: u64,
    /// When the last of those turns ended - the line between a result that arrived while the
    /// model was WORKING and one that arrived while it was idle. `None` until a turn has
    /// ended in this process.
    ended_at: Option<SystemTime>,
    /// Handle or slug → (when it ended, `turns_ended` at the moment it was delivered).
    seen: HashMap<String, (SystemTime, u64)>,
}

impl JobWatchers {
    /// A watcher set for one session's host and hub. Weak by design - see the module header.
    /// `bell` is the daemon's worker bell; `None` for a harness with no daemon, where the
    /// completion is still queued but nothing rings to cause the wake that drains it.
    pub fn new(host: &Arc<dyn ProcessHost>, hub: &Arc<Hub>, bell: Option<Arc<Bell>>) -> Arc<Self> {
        let wanted: Arc<Mutex<HashMap<String, WantedNow>>> = Arc::new(Mutex::new(HashMap::new()));
        Arc::new(JobWatchers {
            host: Some(Arc::downgrade(host)),
            recorder: Arc::new(Mutex::new(None)),
            undelivered: Mutex::new(Vec::new()),
            hub: Arc::downgrade(hub),
            tasks: None,
            completions: Settlements::new(),
            in_flight: Arc::new(AtomicBool::new(false)),
            // A root has no level above it: nothing it fails to drain can be handed up.
            parent: None,
            me: hub.session_id(),
            bell,
            stop: Arc::new(AtomicBool::new(false)),
            watching: Arc::new(Mutex::new(HashSet::new())),
            killed: Arc::new(Mutex::new(HashSet::new())),
            settled: Arc::new(Mutex::new(Settled::default())),
            held: Arc::new(Mutex::new(Vec::new())),
            held_now: Arc::new(AtomicUsize::new(0)),
            rings_held: Arc::new(AtomicBool::new(false)),
            held_said: Arc::new(Mutex::new((0, 0, 0))),
            wanted: Arc::clone(&wanted),
            watched: WatchedNow::new(wanted),
            // A root session is its own tree: the root and its own id are one.
            root: hub.session_id(),
            read: Arc::new(Mutex::new(ReadRecord::default())),
            sweeps: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// These jobs' substrate was taken down under them, before their watchers could see them end.
    /// The exit a client reports after its VM is gone is the teardown's and not the job's own, so
    /// the watcher that settles one of these says the kill instead (R173).
    pub fn killed_with_the_substrate(&self, handles: impl IntoIterator<Item = String>) {
        let mut g = self.killed.lock().expect("job watchers");
        g.extend(handles);
    }

    /// Tell this set where to leave a record of what it settles. A setter rather than a
    /// constructor parameter: `new` is called in twelve places and all but a couple are tests.
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

    /// A job is a row from the moment it exists, not from the moment it ends - a job running
    /// when the daemon dies is never settled by anything, so without this write it leaves no
    /// trace at all.
    pub fn record_start(&self, handle: &str, how: &str, ran_for_ms: u64) {
        let Ok(g) = self.recorder.lock() else {
            return;
        };
        if let Some(rec) = g.as_ref() {
            rec.start(handle, how, ran_for_ms);
            // What a restart's reminder needs to run it again - command, name and directory
            // read from the live view while the job still exists. See `crate::subagent_record`.
            let view = self
                .host
                .as_ref()
                .and_then(Weak::upgrade)
                .and_then(|h| h.jobs().into_iter().find(|j| j.id.0 == handle));
            if let Some(view) = view {
                rec.context(handle, &view.command, view.slug.clone(), &view.cwd);
                // And the process itself, which is what a restart has to find. See
                // [`JobRecorder::process`].
                rec.process(&view);
            }
        }
    }

    /// A set for a session that cannot start processes but can still spawn a subagent.
    /// `task` is `Access::Session` and needs no host, so keying the set on the *process* host
    /// would have made the wake work only where a command could also be backgrounded.
    pub fn watching_tasks(hub: &Arc<Hub>, bell: Option<Arc<Bell>>) -> Arc<Self> {
        let wanted: Arc<Mutex<HashMap<String, WantedNow>>> = Arc::new(Mutex::new(HashMap::new()));
        Arc::new(JobWatchers {
            host: None,
            recorder: Arc::new(Mutex::new(None)),
            undelivered: Mutex::new(Vec::new()),
            hub: Arc::downgrade(hub),
            tasks: None,
            completions: Settlements::new(),
            in_flight: Arc::new(AtomicBool::new(false)),
            parent: None,
            me: hub.session_id(),
            bell,
            stop: Arc::new(AtomicBool::new(false)),
            watching: Arc::new(Mutex::new(HashSet::new())),
            killed: Arc::new(Mutex::new(HashSet::new())),
            settled: Arc::new(Mutex::new(Settled::default())),
            held: Arc::new(Mutex::new(Vec::new())),
            held_now: Arc::new(AtomicUsize::new(0)),
            rings_held: Arc::new(AtomicBool::new(false)),
            held_said: Arc::new(Mutex::new((0, 0, 0))),
            wanted: Arc::clone(&wanted),
            watched: WatchedNow::new(wanted),
            root: hub.session_id(),
            read: Arc::new(Mutex::new(ReadRecord::default())),
            // A set with no host settles no job, so nothing is ever kept here.
            sweeps: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// The door a tool hands a spawned job's change snapshot through, see
    /// [`letibot_tools::detect::JobSweeps`]. The harness that holds this set is the one that wires
    /// it, and a set with no host settles no job, so nothing is ever kept on one.
    pub fn sweeps(&self) -> Arc<dyn letibot_tools::detect::JobSweeps> {
        Arc::new(Sweeps(Arc::clone(&self.sweeps)))
    }

    /// And the session's subagents settle here too. A separate call because a watcher set
    /// is built in two places that do not both have a runner, and a `None` there is honest.
    pub fn with_tasks(self: Arc<Self>, tasks: &Arc<dyn TaskRunner>) -> Arc<Self> {
        Arc::new(JobWatchers {
            host: self.host.clone(),
            hub: Weak::clone(&self.hub),
            tasks: Some(Arc::downgrade(tasks)),
            // Not inherited: a recorder names the SESSION its rows belong to, and a copy of
            // the parent's would file a child's job in the parent's history.
            recorder: Arc::new(Mutex::new(None)),
            undelivered: Mutex::new(Vec::new()),
            completions: Arc::clone(&self.completions),
            // Copied with the queue it describes, never shared with the tree's.
            in_flight: Arc::clone(&self.in_flight),
            parent: self.parent.clone(),
            me: self.me.clone(),
            bell: self.bell.clone(),
            stop: Arc::clone(&self.stop),
            watching: Arc::clone(&self.watching),
            killed: Arc::clone(&self.killed),
            settled: Arc::clone(&self.settled),
            held: Arc::clone(&self.held),
            held_now: Arc::clone(&self.held_now),
            rings_held: Arc::new(AtomicBool::new(false)),
            held_said: Arc::clone(&self.held_said),
            wanted: Arc::clone(&self.wanted),
            // Its own snapshot, over its own `mine`: what this set watches is this set's.
            watched: WatchedNow::new(Arc::clone(&self.wanted)),
            root: self.root.clone(),
            read: Arc::clone(&self.read),
            // Its own: the sweep a child's tool handed over is keyed by a handle of the child's.
            sweeps: Arc::clone(&self.sweeps),
        })
    }

    /// Join this session's watcher to its tree's. A child keeps what is genuinely its own - its
    /// hub, its host and runner, and its completions queue, since a settlement belongs to the
    /// session that started it.
    pub fn shares_tree(self: Arc<Self>, tree: &Arc<JobWatchers>) -> Arc<Self> {
        Arc::new(JobWatchers {
            host: self.host.clone(),
            hub: Weak::clone(&self.hub),
            tasks: self.tasks.clone(),
            // Not inherited, for the reason `with_tasks` gives above: the recorder names the
            // session, and this set is another session's.
            recorder: Arc::new(Mutex::new(None)),
            undelivered: Mutex::new(Vec::new()),
            // Its own: the child's queue is where its own settlements are drained from.
            completions: Arc::clone(&self.completions),
            // Copied with the queue it describes, never shared with the tree's.
            in_flight: Arc::clone(&self.in_flight),
            // Where what it can no longer drain is handed up, with the parent's id - who the
            // ring for a handed-up settlement names.
            parent: Some(Arc::new(Up {
                queue: Arc::clone(&tree.completions),
                who: tree.me.clone(),
                stopped: Arc::clone(&tree.stop),
                // The ancestor's own level, not a copy of its queue: a hop that stopped short is
                // a settlement in the queue of a session nobody serves.
                up: tree.parent.clone(),
            })),
            me: self.me.clone(),
            bell: self.bell.clone(),
            stop: Arc::clone(&self.stop),
            watching: Arc::clone(&tree.watching),
            killed: Arc::clone(&tree.killed),
            settled: Arc::clone(&tree.settled),
            // Its own: what this session holds and what it asked for are this session's.
            held: Arc::clone(&self.held),
            held_now: Arc::clone(&self.held_now),
            rings_held: Arc::new(AtomicBool::new(false)),
            held_said: Arc::clone(&self.held_said),
            wanted: Arc::clone(&self.wanted),
            // Its own, for `held`'s reason: a child that inherited its parent's would report
            // the parent's jobs as its work.
            watched: WatchedNow::new(Arc::clone(&self.wanted)),
            root: tree.root.clone(),
            // Its own, for `held`'s reason: what this session's model has read is its own.
            read: Arc::clone(&self.read),
            sweeps: Arc::clone(&self.sweeps),
        })
    }

    /// Take every settlement this session has not been told yet - one drain per turn, and
    /// taking it is what stops a settlement being delivered twice. What comes back is this
    /// session's own, plus what a stopped session below handed up ([`JobWatchers::stop`])
    pub fn take_completions(&self) -> Vec<JobCompletion> {
        self.completions.drain()
    }

    /// Take them at a round boundary, for the session the daemon does not hold. Empty at the
    /// top of a tree, and that is the guard: a root's between-turn turn is the daemon's, so
    /// taking here would be a second delivery path for the one built.
    pub fn take_mid_turn_completions(&self) -> Vec<JobCompletion> {
        if self.parent.is_none() {
            return Vec::new();
        }
        self.take_completions()
    }

    /// Queue a settlement as a watcher would, without the ring - the door a settlement that
    /// outlived its session comes back through, and the one the delivery tests stand in a
    /// watcher's place with. No ring
    pub fn queue(&self, c: JobCompletion) {
        self.completions.push(c);
    }

    /// A settlement made on this session's OWN side of the tree: the daemon's worker runs a
    /// parked child's later turn, and the slot and the queue that must move are its parent's. Queued
    /// and rung exactly as a watcher's would be, because this session is the one that has to act.
    pub fn settle_here(&self, c: JobCompletion) {
        let queue = SettlementQueue {
            own: Arc::clone(&self.completions),
            // The level above only for what this set cannot drain itself: `SettlementQueue::push`
            // reads `stopped` under `own`'s lock, so a session that is still here keeps it.
            parent: self.parent.clone(),
            stopped: Arc::clone(&self.stop),
            owner: self.me.clone(),
            bell: self.bell.clone(),
            // The row for a child's ending is the record's own, written wherever it settled.
            recorder: None,
            sweeps: Arc::clone(&self.sweeps),
            killed: Arc::clone(&self.killed),
        };
        queue.push(c);
    }

    /// A settlement found by the session that knows and not by a watcher: a subagent's later
    /// turn ends and nothing is blocked on its slot, so it goes to the level above - queued and
    /// rung the way a stopped session's queue hands up. `false` for a session with no level above.
    pub fn settle_up(&self, c: JobCompletion) -> bool {
        let Some(up) = &self.parent else {
            return false;
        };
        let who = up.deliver([c]);
        if let Some(bell) = &self.bell {
            bell.ring_settlement_wake(&who);
        }
        true
    }

    /// Queue a settlement as a watcher would, without the ring - for a test of the delivery
    /// rules, which needs a completion in the queue without a process ending. Nothing in
    /// the daemon calls it: a settlement is queued by its watcher thread.
    #[doc(hidden)]
    pub fn queue_for_test(&self, c: JobCompletion) {
        self.queue(c);
    }

    /// Queue a settlement found by a reader rather than a watcher, unless already owed: the queue, the held batch and what the last turn was told are the three ways a handle is spoken for.
    /// A door re-arming from the store needs the guard or a queued settlement is said twice.
    pub fn queue_unless_owed(&self, c: JobCompletion) -> bool {
        let queued = self
            .completions
            .queue
            .lock()
            .expect("job completions")
            .iter()
            .any(|x| x.job == c.job);
        let held = self
            .held
            .lock()
            .expect("held results")
            .iter()
            .any(|x| x.job == c.job);
        let told = self
            .read
            .lock()
            .expect("read record")
            .seen
            .contains_key(&c.job);
        if queued || held || told {
            return false;
        }
        self.queue(c);
        true
    }

    /// Hold this session's settlement queue while `during` runs - the position a publisher must
    /// not be parked in, stood in on purpose.
    #[doc(hidden)]
    pub fn hold_settlements_for_test<R>(&self, during: impl FnOnce() -> R) -> R {
        let _held = self.completions.queue.lock().expect("job completions");
        during()
    }

    /// Say this handle's result is wanted the moment it ends - the daemon's half of
    /// [`letibot_tools::runtime::CompletionWanted`]. A want already recorded is not replaced
    /// `wake: "now"` and a later wait are both *now*, and the first is the one recorded.
    pub fn want_now(&self, handle: &str, why: WantedNow) {
        // A handle whose result the model has already been told is nothing to ask about: the want
        // would never be spent, and the read record only remembers the last turn or two.
        let delivered = self
            .read
            .lock()
            .expect("read record")
            .seen
            .contains_key(handle);
        if delivered || self.settled.lock().expect("job watchers").contains(handle) {
            return;
        }
        self.wanted
            .lock()
            .expect("wanted results")
            .entry(handle.to_string())
            .or_insert(why);
        // And the snapshot, because `awaited` reads the want from it - a want is what makes a
        // running job count as work the session is blocked on.
        self.watched.rebuild();
    }

    /// Whether this handle was asked for, and how - read by [`crate::delivery`]'s sort.
    pub fn wanted(&self, handle: &str) -> Option<WantedNow> {
        self.wanted
            .lock()
            .expect("wanted results")
            .get(handle)
            .copied()
    }

    /// Forget a want once its result has been delivered, so the map holds only what is
    /// still outstanding.
    pub fn delivered(&self, handle: &str) {
        self.wanted.lock().expect("wanted results").remove(handle);
        self.watched.rebuild();
    }

    /// These completions have just been put into the model's transcript - recorded with the
    /// turns ended so far, so [`JobWatchers::grace_left`] can tell *delivered in the turn
    /// ending now* from *delivered a turn ago*.
    pub fn read_now(&self, done: &[JobCompletion]) {
        {
            let mut r = self.read.lock().expect("read record");
            let at = r.turns_ended;
            // The record holds only what may still be graced, and `grace_left` is one reader of
            // it - a session that never asks about a handle must not grow this map for its life.
            r.seen
                .retain(|_, (_, delivered)| at <= delivered.saturating_add(1));
            for c in done {
                r.seen.insert(c.job.clone(), (c.arrived, at));
                if let Some(slug) = &c.slug {
                    r.seen.insert(slug.clone(), (c.arrived, at));
                }
            }
        }
        // And its want is spent, whichever door delivered it, or a batched result would
        // leave its want in the map for the session's life.
        {
            let mut wanted = self.wanted.lock().expect("wanted results");
            for c in done {
                wanted.remove(&c.job);
            }
        }
        // And the snapshot, which carries each handle's want (`WatchedNow`).
        self.watched.rebuild();
        // Marked delivered in the store only once the text is in a persisted row: a daemon that
        // goes down before then brings the settlement back, and a repeat is safer than a loss.
        // The previous read's text is in the transcript by the time a later one is built.
        self.mark_delivered();
        self.undelivered
            .lock()
            .expect("undelivered")
            .extend(done.iter().map(|c| (c.kind, c.job.clone())));
    }

    /// Write the store's `delivered` for what [`Self::read_now`] recorded.
    pub fn mark_delivered(&self) {
        let pending = std::mem::take(&mut *self.undelivered.lock().expect("undelivered"));
        if pending.is_empty() {
            return;
        }
        let recorder = self.recorder.lock().expect("job recorder").clone();
        if let Some(rec) = recorder {
            for (kind, job) in pending {
                match kind {
                    BackgroundKind::Subagent => rec.subagent_delivered(&job),
                    BackgroundKind::Job => rec.job_delivered(&job),
                }
            }
        }
    }

    /// A turn has ended, counted after its rounds by the harness's two doors into `run_rounds`. A turn ending with results held rings this session's wake; one stopped or failed takes back the wake a settlement rang during it, since nothing asked for that turn.
    pub fn turn_ended(&self, how: TurnEnd) {
        {
            let mut r = self.read.lock().expect("read record");
            r.turns_ended += 1;
            r.ended_at = Some(SystemTime::now());
        }
        // The turn's rows are written: what it read is delivered.
        self.mark_delivered();
        // Recorded before the ring, because it is what the ring MEANS: the ending is decided once
        // per turn, and the activity reads it as *a turn is coming for what this one held*.
        self.rings_held
            .store(how == TurnEnd::Done, Ordering::SeqCst);
        let Some(bell) = &self.bell else { return };
        match how {
            TurnEnd::Interrupted | TurnEnd::Failed => bell.drop_wakes(&self.me),
            TurnEnd::Done if self.held_count() > 0 => bell.ring_settlement_wake(&self.me),
            TurnEnd::Done => {}
        }
    }

    /// When this session's last turn ended, or `None` before its first - what tells a result
    /// that arrived while the model was working from one that arrived while it was idle.
    pub fn last_turn_ended(&self) -> Option<SystemTime> {
        self.read.lock().expect("read record").ended_at
    }

    /// How much of an ended job's or child's grace is left, or `None` when it has none - the
    /// fact the end-of-turn plan check stays quiet on (`waits_on_an_undigested_ending`).
    /// Graced
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
        // What the backstop has released, and what a full turn has digested, is forgotten - the
        // record holds only what may still be graced.
        r.seen
            .retain(|_, (ended, at)| turns <= *at + 1 && left(*ended).is_some());
        r.seen.get(handle).and_then(|(ended, _)| left(*ended))
    }

    /// Hold these until the next break. See [`JobWatchers::held`].
    pub fn hold(&self, more: Vec<JobCompletion>) {
        if !more.is_empty() {
            let mut h = self.held.lock().expect("held results");
            h.extend(more);
            self.held_now.store(h.len(), Ordering::SeqCst);
        }
        self.say_held();
    }

    /// A batch this session owes its model. A door put back results it could not say, so the ring they came with is spent and the next break must be caused by this one.
    /// Rung as a fact, not a settlement: nothing queues a second copy, and the turn it was put back under must not take it back.
    pub fn a_batch_is_owed(&self) {
        if let Some(bell) = &self.bell {
            bell.ring_wake(&self.me);
        }
    }

    /// Everything held, taken - the batch delivered, or the set handed up - and said on the
    /// hub, which then reads nothing held.
    pub fn take_held(&self) -> Vec<JobCompletion> {
        let taken = self.take_held_to_resort();
        self.say_held();
        taken
    }

    /// Everything held, taken to be decided again (`delivery::sort`), WITHOUT saying so on
    /// the hub: the sort puts back what is still held with [`JobWatchers::hold`], which says the
    /// result, so a head never sees the set drop to zero and come back within one sort.
    pub fn take_held_to_resort(&self) -> Vec<JobCompletion> {
        let mut h = self.held.lock().expect("held results");
        let taken = std::mem::take(&mut *h);
        self.held_now.store(0, Ordering::SeqCst);
        taken
    }

    /// The held set's counts - jobs, subagents, and how many of them failed.
    pub fn held_counts(&self) -> (u64, u64, u64) {
        let h = self.held.lock().expect("held results");
        let jobs = h.iter().filter(|c| c.kind == BackgroundKind::Job).count() as u64;
        let subagents = h.len() as u64 - jobs;
        let failed = h.iter().filter(|c| crate::delivery::failed(c)).count() as u64;
        (jobs, subagents, failed)
    }

    /// Publish [`SessionEvent::HeldResults`] when the held counts changed since the last one - the head's counter beside *Responding*. A set whose hub has gone publishes nothing.
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

    /// When the longest-held result arrived, or `None` when nothing is held - what the
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

    /// Has a settlement arrived that this session has not been told yet? - the queue behind
    /// [`JobWatchers::wake`]'s next drain, read by the activity because it is the window
    /// between a job ending and the turn its ending starts.
    pub fn pending(&self) -> bool {
        // The flag, not the queue: a publish reaches this, and a publish that waited on the
        // queue is the deadlock [`WatchedNow`] describes.
        self.completions.any.load(Ordering::SeqCst)
    }

    /// A settlement is being taken out of this queue to be said - called where the take
    /// happens, BEFORE it, because the take is what clears the two facts a reader waits on.
    /// Until [`JobWatchers::turn_began`] the model is still owed that turn.
    pub fn took_a_settlement(&self) {
        self.in_flight.store(true, Ordering::SeqCst);
    }

    /// The turn that says the taken settlements has begun - or nothing was owed after all.
    /// Both ends of that turn call this: its clock (which starts before the queue's words go in)
    /// and its ending, so a turn that never opened cannot leave a session reading busy.
    pub fn turn_began(&self) {
        self.in_flight.store(false, Ordering::SeqCst);
    }

    /// Has a settlement been taken for a turn that has not begun? - the window between the
    /// queue being drained and the turn that carries it starting, which is where a `wait` read
    /// *done* and answered the turn that STARTED the job.
    pub fn settlement_in_flight(&self) -> bool {
        self.in_flight.load(Ordering::SeqCst)
    }

    /// The jobs and children THIS session is waiting on, by handle and kind - the things whose
    /// settlement will reach its model.
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
        // when it changes, and a `HashMap`'s iteration order is not a change.
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// The facts the activity line is made of, taken without waiting on a lock a watcher holds
    /// and the watchers are one leaf lock over an `Arc` that is cloned and released, so nothing
    /// here can wait on a thread that is itself waiting.
    pub fn activity_facts(&self) -> ActivityFacts {
        ActivityFacts {
            pending: self.pending(),
            // A held result is a turn owed only when a wake is coming for it, and only a turn that
            // finished on its own rings one: a readout promising a turn after a failed one parks a
            // caller's `wait` on a session nothing is coming for.
            held: match self.rings_held.load(Ordering::SeqCst) {
                true => self.held_now.load(Ordering::SeqCst),
                false => 0,
            },
            in_flight: self.settlement_in_flight(),
            at_work: self.watched.now(),
        }
    }

    /// What this set is holding, in entries. Every field but `settled` is emptied by the settlement
    /// or the delivery that owns it, so a reading that climbs with the number of jobs a session has
    /// ever run is the reading of a retention - see [`Settled`].
    pub fn retained(&self) -> Retained {
        Retained {
            watching: self.watching.lock().expect("job watchers").len(),
            settled: self.settled.lock().expect("job watchers").len(),
            wanted: self.wanted.lock().expect("wanted results").len(),
            held: self.held.lock().expect("held results").len(),
            read: self.read.lock().expect("read record").seen.len(),
        }
    }

    /// The session at the top of this tree - the ROOT, which is where a subagent's ask belongs
    /// a child has no head, and the one session in the tree that has one is the daemon's
    /// inherited through [`JobWatchers::shares_tree`].
    pub fn tree_root(&self) -> &str {
        &self.root
    }

    /// Is this job's completion already on its way to the model? True while a watcher thread is
    /// blocked on the job
    pub fn delivering(&self, job: &str) -> bool {
        self.watching.lock().expect("job watchers").contains(job)
    }

    /// Stop every watcher this set has spawned, and hand up what this session will never drain.
    /// Per set, and the set is per session - see [`JobWatchers::shares_tree`] for the defect
    /// that came of sharing this across a tree.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        let pending: Vec<JobCompletion> = {
            let mut own = self.completions.queue.lock().expect("job completions");
            // What was HELD goes up with the queue : a stopped session has no next break
            // to read it at. Taken without saying so on the hub, because saying it publishes -
            // and a publish from inside this lock is the deadlock this method was the site of.
            let mut all = self.take_held_to_resort();
            all.extend(own.drain(..));
            self.completions.any.store(false, Ordering::SeqCst);
            all
        };
        // Said out here, where it cannot wait on the queue this drain has just let go of.
        self.say_held();
        // A session at the top of a tree has nowhere to hand anything up to, and a
        // session that drained everything it had leaves nothing to carry.
        let Some(up) = &self.parent else {
            return;
        };
        if pending.is_empty() {
            return;
        }
        let who = up.deliver(pending);
        // And the ring, naming the session it was handed TO. The push that queued this rang
        // the owner before the hand-over was possible; without this ring a settlement arriving
        // just after would sit in the parent's queue until the parent woke for something else.
        if let Some(bell) = &self.bell {
            bell.ring_settlement_wake(&who);
        }
    }

    /// Has this set been stopped? The property [`JobWatchers::shares_tree`] is asserted
    /// against: a child joining a tree must not take the tree's stop, or a child's turn
    /// ending silences every watcher the root has.
    pub fn stopped(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    /// Watch one backgrounded thing. Idempotent: one already watched, or already settled, is
    /// left alone. Which kind it is, is asked rather than inferred.
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
        // Where this settlement goes is decided by these fields and nothing else -
        // see `SettlementQueue`.
        let queue = Arc::new(SettlementQueue {
            own: Arc::clone(&self.completions),
            parent: self.parent.clone(),
            stopped: Arc::clone(&self.stop),
            owner: self.me.clone(),
            bell: self.bell.clone(),
            recorder: self.recorder.lock().expect("job recorder").clone(),
            sweeps: Arc::clone(&self.sweeps),
            killed: Arc::clone(&self.killed),
        });
        let watching = Arc::clone(&self.watching);
        let settled = Arc::clone(&self.settled);
        let watched = Arc::clone(&self.watched);
        let runner = self.tasks.as_ref().and_then(Weak::upgrade);
        // Which kind this handle is, decided once and recorded - a later reader must not ask
        // the runner again (see [`WatchedNow`]). The same question, in the same place.
        let child = runner
            .as_ref()
            .is_some_and(|r| !matches!(r.collect(&job, Duration::ZERO), TaskStatus::Unknown));
        let kind = if child {
            BackgroundKind::Subagent
        } else {
            BackgroundKind::Job
        };
        // The name, read while the job exists (see [`WatchedNow`]): a subagent's is the
        // runner's, a job's is the one `bash` was given.
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
            // And the activity's snapshot - a handle in `mine` the snapshot does not have is
            // a session waiting on a child and reading `quiet`.
            watched.rebuild();
            // And the value a head reads, at the moment it is true: no event announces an
            // armed watcher, so leaving this to the next publish is a session with a running
            // child that answers `quiet`.
            if let Some(h) = hub.upgrade() {
                h.refresh_activity();
            }
            let watched = Arc::clone(&watched);
            let queue = Arc::clone(&queue);
            let _ = std::thread::Builder::new()
                .name(format!("subagent-watch-{}", &job[..job.len().min(20)]))
                .spawn(move || watch_task(runner, hub, queue, watching, settled, watched, job));
            return;
        }
        // Not a subagent and no host to ask: nothing can ever settle this handle, so it is
        // dropped rather than left in `watching` - a name that outlives its watcher would make
        // `delivering` true for ever.
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
        // For the reason the subagent arm gives: arming is a change nothing announces.
        if let Some(h) = hub.upgrade() {
            h.refresh_activity();
        }
        let _ = std::thread::Builder::new()
            .name(format!("job-watch-{}", &job[..job.len().min(20)]))
            .spawn(move || watch_one(host, hub, queue, watching, settled, watched, job));
    }
}

/// Where a watcher's settlement goes, and the one place that decides it. Carried by the
/// watcher thread rather than passed as six arguments, because the decision is not the
/// watcher's
struct SettlementQueue {
    /// The queue of the session that started it.
    own: Arc<Settlements>,
    /// The queue of the level above it, when there is one. The `Up` travels whole, because a level
    /// that has stopped is not where the settlement goes and its own flag is the only fact that says
    /// so.
    parent: Option<Arc<Up>>,
    /// The owner's stop flag - see [`JobWatchers::stop`]. Read while holding `own`, which is
    /// what makes the two orderings exhaustive.
    stopped: Arc<AtomicBool>,
    /// The owner's session id, stamped on every settlement this queue produces - and the
    /// session whose bell is rung for one that goes to `own`, because that is the session
    /// that has to act on it.
    owner: String,
    bell: Option<Arc<Bell>>,
    /// Where a settlement is written down, when this session has a store - `None` on a
    /// storeless run. Copied in when the queue is built, which is per watch and therefore always
    /// after the harness has had its say.
    recorder: Option<JobRecorder>,
    /// The spawn snapshots this session's tools handed over, keyed by handle: read here because the
    /// settlement is the last moment a job's writes are this job's own (R161).
    sweeps: Arc<Mutex<HashMap<String, letibot_tools::detect::Sweep>>>,
    /// Handles whose substrate was taken down under them - see
    /// [`JobWatchers::killed_with_the_substrate`]. Read at the settle.
    killed: Arc<Mutex<HashSet<String>>>,
}

impl SettlementQueue {
    /// Was this job's substrate taken down under it? Then the exit its client reports is the
    /// teardown's and not the job's own, so the state the settlement carries is the kill (R173).
    fn killed_with_the_substrate(&self, job: &str) -> bool {
        self.killed.lock().map(|g| g.contains(job)).unwrap_or(false)
    }
    /// What a job changed while it ran, taken out of the map because one job settles once. `None`
    /// when nothing was handed over - a job spawned outside a git tree, which is a job nothing here
    /// can say anything true about.
    fn changes_of(&self, job: &str) -> Option<letibot_tools::detect::Found> {
        let sweep = self.sweeps.lock().ok()?.remove(job)?;
        let found = sweep.found();
        (!found.changed.is_empty()).then_some(found)
    }

    /// Queue one settlement where its session can still be told, and ring. The `stopped` read
    /// is inside the `own` lock on purpose
    fn push(&self, c: JobCompletion) {
        // The record goes down where the settlement is first known, not where it is read
        // a job's fate does not depend on which session's queue it lands in, and writing after
        // the routing would lose the row for one handed up to an ancestor.
        if let Some(rec) = &self.recorder {
            rec.record(&c);
        }
        let mut g = self.own.queue.lock().expect("job completions");
        if self.stopped.load(Ordering::Acquire)
            && let Some(above) = &self.parent
        {
            // The first level up that is still here: the one this set hands to may have stopped
            // since, and its queue has already been drained to a session nobody serves.
            drop(g);
            let who = above.deliver([c]);
            self.ring(&who);
            return;
        }
        g.push_back(c);
        self.own.any.store(true, Ordering::SeqCst);
        drop(g);
        self.ring(&self.owner);
    }

    /// The durable row without a notice or a bell.
    fn record_only(&self, c: &JobCompletion) {
        if let Some(rec) = &self.recorder {
            rec.record(c);
        }
    }

    /// The bell, rung for the session that has to act on the settlement - its owner, or
    /// the level above when the owner has stopped. Never for a session that is neither
    /// a ring is a promise that somebody can be woken by it.
    fn ring(&self, session: &str) {
        if let Some(bell) = &self.bell {
            bell.ring_settlement_wake(session);
        }
    }
}

/// The one thing a settlement needs to leave a record of itself - a path and a session id.
/// Not a `Store`
#[derive(Clone)]
pub struct JobRecorder {
    path: std::path::PathBuf,
    session_id: String,
    /// Where a write that failed is said. `Weak::new` for a recorder nobody is watching.
    hub: Weak<Hub>,
    /// Opened on the first write: a queue is built per watch, and a store opened at construction
    /// would be opened for every job that is never backgrounded.
    store: Arc<Mutex<Option<Store>>>,
    /// The daemon instance that owns these jobs - see [`JobRecorder::owned_by`].
    instance: String,
    /// Where these processes run - `None` for this box, a `hosts.toml` name otherwise. See
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

    /// Where this session's processes run - `None` for this box, a `hosts.toml` name otherwise.
    /// It is the `process` row's `host` column, and it decides how the job's identity is spelled.
    pub fn on(mut self, placement: Option<String>) -> Self {
        self.placement = placement;
        self
    }

    /// The daemon instance (`Registry::instance`) its job contexts are stamped with.
    pub fn owned_by(mut self, instance: &str) -> Self {
        self.instance = instance.to_string();
        self
    }

    /// A background job's command, name and directory, written while it runs - the `job`
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

    /// A row that did not land, said on the session's log - see the type's doc.
    fn not_recorded(&self, handle: &str, what: &str, why: &str) {
        eprintln!(
            "  {} · job {handle}: {what} was not recorded: {why}",
            self.session_id
        );
        if let Some(hub) = self.hub.upgrade() {
            hub.warn(
                "job_not_recorded",
                format!(
                    "background job `{handle}`: {what} could not be written to the store ({why}). \
                     The job itself is unaffected; what is missing is its row in the jobs \
                     history, and a todo row waiting on it may not be released after a restart."
                ),
            );
        }
    }

    /// The store, opened on first use and handed to the closure. One place, because two
    /// writers share it.
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

    /// A subagent's settlement has reached its parent's model - see
    /// [`JobWatchers::read_now`]. A handle the store has no subagent row for writes nothing.
    fn subagent_delivered(&self, handle: &str) {
        self.with_store(handle, "its delivery", |store| {
            store
                .deliver_subagent_run(handle)
                .map_err(|e| e.to_string())
        });
    }

    /// A job's ending has reached the model - `Store::deliver_job`. See
    /// [`JobWatchers::read_now`].
    fn job_delivered(&self, handle: &str) {
        self.with_store(handle, "its delivery", |store| {
            store
                .deliver_job(&self.session_id, handle)
                .map_err(|e| e.to_string())
        });
    }

    /// And this job's PROCESS, which outlives the record above - the supervisor's row. Written
    /// here because this is where the live view still exists: command, name, directory, pid
    /// scope.
    pub(crate) fn process(&self, view: &letibot_tools::exec::JobView) {
        // A job of jobs has no process to write down. Its `pid` is 0 and its branches are
        // records of their own, so a `process` row would name a pid nothing can be. The `job` row
        // is written for it all the same, which is the durable half a restart reads.
        if view.pid == 0 && self.placement.is_none() {
            return;
        }
        let identity = match &self.placement {
            Some(_) => format!("cmd:{}", view.command),
            None => match crate::supervise::identity_of(view.pid) {
                Some(id) => id,
                // A process that cannot be identified is not written down at all, rather than
                // written without an identity: the supervisor would signal the wrong process with
                // it. Said rather than silent, because no restart can adopt such a job.
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

    /// The start: a `running` row, with what this side knows. See
    /// [`JobWatchers::record_start`] for why a job is a row before it ends.
    fn start(&self, handle: &str, how: &str, ran_for_ms: u64) {
        self.with_store(handle, "its start", |store| {
            store
                .put_job(
                    &self.session_id,
                    &JobRecord {
                        handle: handle.to_string(),
                        // Not knowable here. `ToolEvent::Finished` carries no arguments, so the
                        // command is read at the SETTLEMENT; an empty field never erases a recorded one.
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

    /// Write one settlement's row. `running` is never written here - the row it updates is
    /// [`JobRecorder::start`]'s. `how` is empty on purpose: it is a property of the start, and
    /// an empty field is *not recorded here*, so the row keeps what the start said.
    pub(crate) fn record(&self, c: &JobCompletion) {
        // A subagent settles through this same queue and is not a job. `task`'s handle is a
        // session id, and a row here would put a child in the jobs pane.
        if c.kind != BackgroundKind::Job {
            return;
        }
        let redirect = letibot_tools::builtins::output_redirect_path(&c.command);
        // And the process's own ending, so the supervisor's record says what the row does.
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

/// A settlement's word back into the process states - one place, because the settlement's
/// vocabulary and the record's are two, and two spellings of the translation would make a
/// `done` in one reader and a `stopped` in the other. `exited N` is `done` with N.
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

/// What a `job_kill` settles a job as: the session's own act, already reported in its result.
const OWN_KILL: &str = "killed by job_kill";

/// How long a kill's own verification (the group is killed, then waited on) may take before the
/// watcher stops waiting for it.
const KILL_VERIFY: Duration = Duration::from_secs(8);

/// `None` when the kill held and every process of the job is gone. Else what is still alive: the
/// kill reopened the job, or its processes are still in the scope when the wait ran out.
fn own_kill_survivors(host: &dyn ProcessHost, job: &JobId) -> Option<String> {
    let until = std::time::Instant::now() + KILL_VERIFY;
    loop {
        if host.job(job).is_some_and(|v| v.state.is_running()) {
            break;
        }
        if host.job_pids(job).is_empty() {
            return None;
        }
        if std::time::Instant::now() >= until {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let pids = host.job_pids(job);
    if pids.is_empty() {
        return None;
    }
    Some(
        pids.iter()
            .map(|p| p.to_string())
            .collect::<Vec<_>>()
            .join(", "),
    )
}

/// The line a job's settlement carries about what its tree looked like when it ended: the files git
/// named, and WHEN - never WHO, because another call may have written the same tree while this job ran
/// and neither the snapshot nor the status can attribute a byte to a process.
fn changed_sentence(changed: &[String]) -> String {
    let names: Vec<String> = changed
        .iter()
        .take(letibot_tools::detect::MAX_DRAWN_DIFFS)
        .map(|p| format!("`{p}`"))
        .collect();
    let more = changed
        .len()
        .saturating_sub(letibot_tools::detect::MAX_DRAWN_DIFFS);
    format!(
        "changed while it ran: {}{} (when, not who)",
        names.join(", "),
        match more {
            0 => String::new(),
            more => format!(", and {more} more files"),
        }
    )
}

fn survivor_notice(
    owner: &str,
    job: &str,
    view: &Option<letibot_tools::exec::JobView>,
    pids: String,
) -> JobCompletion {
    JobCompletion {
        memory: None,
        kind: BackgroundKind::Job,
        owner: owner.to_string(),
        job: job.to_string(),
        command: view.as_ref().map(|v| v.command.clone()).unwrap_or_default(),
        slug: view.as_ref().and_then(|v| v.slug.clone()),
        state: format!("kill did not take: pid {pids} still alive; it is still yours to stop"),
        produced: view.as_ref().map(|v| v.produced).unwrap_or(0),
        elapsed_ms: 0,
        detail: String::new(),
        arrived: SystemTime::now(),
        later: false,
    }
}

fn watch_one(
    host: Weak<dyn ProcessHost>,
    hub: Weak<Hub>,
    queue: Arc<SettlementQueue>,
    watching: Arc<Mutex<HashSet<String>>>,
    settled: Arc<Mutex<Settled>>,
    watched: Arc<WatchedNow>,
    job: String,
) {
    loop {
        let (Some(host), Some(hub)) = (host.upgrade(), hub.upgrade()) else {
            // The session is gone - backend, hub, or both. Nothing left that a
            // settlement would be true of.
            break;
        };
        match host.wait_job(&JobId(job.clone()), WATCH_CHUNK) {
            Ok(Waited::Happened { state, .. }) => {
                // The wake's own reading of the state is the truth; the view, if it still exists
                // adds the numbers. It can be gone already - a scope reap at session end removes
                // jobs - and then the settlement carries the state and no invented numbers.
                let view = host.job(&JobId(job.clone()));
                let word = state
                    .map(|s: JobState| s.word())
                    .or_else(|| view.as_ref().map(|v| v.state.word()))
                    .unwrap_or_else(|| "gone".to_string());
                // A substrate taken down under it: whatever its client exited with is what the
                // teardown left behind, so the settlement carries the kill instead - and never an
                // `exited 0` that reads as a finish (R173).
                let word = if queue.killed_with_the_substrate(&job) {
                    crate::harness::VM_LOST.to_string()
                } else {
                    word
                };
                // The session's own `job_kill` already told the model the outcome in its result.
                // News is only a kill that did not hold.
                let mut quiet = false;
                if word == OWN_KILL {
                    match own_kill_survivors(host.as_ref(), &JobId(job.clone())) {
                        None => quiet = true,
                        Some(left) => {
                            queue.push(survivor_notice(&queue.owner, &job, &view, left));
                            hub.refresh_activity();
                            continue;
                        }
                    }
                }
                let produced = view.as_ref().map(|v| v.produced).unwrap_or(0);
                let elapsed_ms = view
                    .as_ref()
                    .and_then(|v| v.ran_for)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                // The command is read here, from the live view, because this is the last moment it
                // exists: a settlement is delivered between turns, by which time the job is reaped.
                let command = view.as_ref().map(|v| v.command.clone()).unwrap_or_default();
                // And the name, for the same reason. A job's slug is fixed for its life, so
                // this is not a race - it is the only place a settlement can pick it up. `None`
                // when nobody named it, which draws exactly the id the notice drew before.
                let slug = view.as_ref().and_then(|v| v.slug.clone());
                let memory = view.as_ref().and_then(|v| v.memory_clause());
                // And what the job says for itself, when it is a job of jobs - a fanout's
                // answer IS its per-host table, so the settlement carries it. `None` for every
                // ordinary job, so nothing a `bash` settlement says changes.
                let mut detail = host
                    .settlement_note(&JobId(job.clone()))
                    .unwrap_or_default();
                // What it changed while it ran, from the snapshot taken at the spawn. Read here
                // because this is the last moment it is true, and said as WHEN and not WHO: another
                // call may have written the same tree in between (R161).
                let changes = queue.changes_of(&job);
                if let Some(found) = &changes {
                    if !detail.is_empty() {
                        detail.push('\n');
                    }
                    detail.push_str(&changed_sentence(&found.changed));
                }
                hub.publish(SessionEvent::JobSettled {
                    job: job.clone(),
                    state: word.clone(),
                    produced,
                    elapsed_ms,
                    edits: changes
                        .as_ref()
                        .map(|f| f.edits.iter().map(|e| e.excerpt(3, 400)).collect())
                        .unwrap_or_default(),
                });
                // And the model, which the event does not reach. Queued on THIS session's
                // own queue - the one that backgrounded it.
                let completion = JobCompletion {
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
                    later: false,
                };
                if quiet {
                    queue.record_only(&completion);
                } else {
                    queue.push(completion);
                }
                settled.lock().expect("job watchers").insert(&job);
                // And what the session is waiting on has moved: the job is still in `watching` but its
                // ending is on the queue now, which is `Delivery`.
                hub.refresh_activity();
                break;
            }
            // Still running - or never seen, which is a boot window and not a finish. The chunk
            // is the shutdown re-check and nothing else; a settlement wakes the wait at once.
            Ok(Waited::Deadline { .. } | Waited::NeverStarted { .. }) => {
                if queue.stopped.load(Ordering::Acquire) {
                    break;
                }
                // The one warning a job is owed before the dead check ends it, TAKEN from the host
                // so a watcher that polls says it once. It is the last moment `job_extend` helps.
                if let Some(w) = host.take_dead_warning(&JobId(job.clone())) {
                    hub.warn(JOB_DEAD_SOON, dead_soon_note(&w));
                }
            }
            // The job is gone from the host: its scope was reaped with the session, or the host
            // dropped the record. No hub is left that a settlement would be true of.
            Err(_) => break,
        }
    }
    watching.lock().expect("job watchers").remove(&job);
    // And this session is not waiting on it any more. Two maps, one moment: a handle left in
    // `mine` would keep the session reading as busy on a job whose watcher has gone.
    watched.mine.lock().expect("job watchers").remove(&job);
    // And the snapshot, and the value on the hub. The settlement's `refresh_activity` ran
    // while this handle was still in `mine`, so without this the line reads `waiting on: delivery
    // job j8032` for a job whose watcher has gone.
    watched.rebuild();
    if let Some(h) = hub.upgrade() {
        h.refresh_activity();
    }
}

/// The same settlement, waited on the way a subagent settles. `watch_one`'s shape is kept
/// exactly - a chunked wait whose chunk is the shutdown re-check, a queue push, a bell -
/// and only what is being waited on changes
fn watch_task(
    runner: Arc<dyn TaskRunner>,
    hub: Weak<Hub>,
    queue: Arc<SettlementQueue>,
    watching: Arc<Mutex<HashSet<String>>>,
    settled: Arc<Mutex<Settled>>,
    watched: Arc<WatchedNow>,
    job: String,
) {
    loop {
        // The hub is the same liveness test the job path uses: a session that has closed takes its
        // hub with it, and nothing is left a settlement would be true of.
        if hub.upgrade().is_none() {
            break;
        }
        match runner.collect(&job, WATCH_CHUNK) {
            // Settled, one way or the other. Both are the model's business: a child that failed is
            // what the parent is blocked on just as much as one that answered.
            TaskStatus::Done { answer, .. } => {
                let slug = runner.slug_of(&job);
                settled_here("done", first_line(&answer), &job, slug, &queue, &settled);
                // The queue is what the activity reads now - see `watch_one`'s comment.
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
            // Its turn ended and a job of it was killed with the substrate: the parent is told
            // the same news a failure is, and the sentence carries what died (R173).
            TaskStatus::Killed { why, .. } => {
                let slug = runner.slug_of(&job);
                settled_here("killed", first_line(&why), &job, slug, &queue, &settled);
                if let Some(h) = hub.upgrade() {
                    h.refresh_activity();
                }
                break;
            }
            // Still working, or a handle this runner never minted. `Unknown` ends the wait and
            // `Running` does not end on `stop`
            TaskStatus::Running { .. } => {}
            TaskStatus::Unknown => break,
        }
    }
    watching.lock().expect("job watchers").remove(&job);
    // And this session is not waiting on it any more. Two maps, one moment: a handle left in
    // `mine` would keep the session reading as busy on a job whose watcher has gone.
    watched.mine.lock().expect("job watchers").remove(&job);
    // And the snapshot, and the value on the hub. The settlement's `refresh_activity` ran
    // while this handle was still in `mine`, so without this the line reads `waiting on: delivery
    // job j8032` for a job whose watcher has gone.
    watched.rebuild();
    if let Some(h) = hub.upgrade() {
        h.refresh_activity();
    }
}

/// Queue one subagent's settlement and ring - the push and the bell, the same two things the job
/// path does at the same moment in its own wait.
fn settled_here(
    state: &str,
    detail: String,
    job: &str,
    slug: Option<String>,
    queue: &Arc<SettlementQueue>,
    settled: &Arc<Mutex<Settled>>,
) {
    queue.push(JobCompletion {
        kind: BackgroundKind::Subagent,
        owner: queue.owner.clone(),
        job: job.to_string(),
        // A subagent runs no command; the notice for one is built from `state` and `detail`.
        command: String::new(),
        // Its name, as the runner gave it, so the child's notice reads `fix-login · s-…` the
        // way a job's does. Read off the runner at the settlement, because the notice is built
        // after the fact; `None` for a runner that keeps no names.
        slug,
        state: state.to_string(),
        // It writes no bytes to a stream this end can count: `produced` means `job_output`'s
        // denominator everywhere else.
        produced: 0,
        elapsed_ms: 0,
        detail,
        memory: None,
        arrived: SystemTime::now(),
        later: false,
    });
    settled.lock().expect("job watchers").insert(job);
}

/// The first line of what a subagent said, for a one-line notice. The whole of it is in
/// `task_result`, which the notice names.
pub(crate) fn first_line(text: &str) -> String {
    let l = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    if l.chars().count() <= 160 {
        return l.trim().to_string();
    }
    format!("{}…", l.trim().chars().take(160).collect::<String>())
}

/// A [`ToolEventSink`] that starts a watcher for every job a call leaves behind. A
/// decorator for the same reason the intent encoder's sink is one: wiring it is one line at
/// the call site, and forgetting it is visible rather than silent.
pub struct JobWatchSink<S> {
    inner: S,
    watch: Option<Arc<JobWatchers>>,
    /// Where a file this session changed is noted, for the roster's working-on.
    seat: Option<Arc<crate::room::seat::SeatCell>>,
}

impl<S> JobWatchSink<S> {
    /// `None` for `watch` when the session has no process host: nothing can be backgrounded, so
    /// there is nothing to watch.
    pub fn new(inner: S, watch: Option<Arc<JobWatchers>>) -> Self {
        JobWatchSink {
            inner,
            watch,
            seat: None,
        }
    }

    pub fn noting_edits(mut self, seat: Arc<crate::room::seat::SeatCell>) -> Self {
        self.seat = Some(seat);
        self
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
            // The row first, then the watch. A daemon that dies before the watcher settles a
            // job would otherwise leave nothing, and the settlement that would have written the
            // row is the one that never happens.
            w.record_start(handle, &how.phrasing(), *ran_for_ms);
            w.watch(handle.clone());
        }
        if let Some(seat) = &self.seat
            && let ToolEvent::Finished { edits, .. } = &event
            && let Some(edit) = edits.first()
        {
            seat.edited(&edit.path);
        }
        self.inner.emit(event);
    }
}

#[cfg(test)]
mod tests;
