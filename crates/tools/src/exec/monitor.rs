//! Monitors: a condition watched **across turns**, per `TODO.md` T24.
//!
//! # This is not `job_wait` with a longer deadline
//!
//! `job_wait` blocks **inside** one turn. A monitor watches while the model is
//! not running and wakes the loop when it fires — *"the encoder running while the
//! model is not"*, in `docs/closed-loop.md`'s terms. T24 says both are wanted and
//! that they are different primitives, and folding them into one verb with a flag
//! would make the model's worst mistake here — believing it waited when it did
//! not — spellable.
//!
//! The seat brief pays for the distinction in its own words: *a Stop hook fires
//! when a session goes idle, and a seat that is rate limited, has crashed, or
//! never started is not running a session, so no stop event ever fires and the
//! silence looks exactly like a quiet room.*
//!
//! # A monitor keys on a HANDLE or a CGROUP, never a pattern
//!
//! This is the rule the whole module is shaped around, and it is not a guard.
//! [`Watch`] has four built-in variants and **none of them can hold a string
//! that is matched against a process**. `until pgrep -f X` is not refused
//! here; it is unspellable, because there is no argument to put the `X` in.
//!
//! The `process` argument on the tool (2026-09-14) does not change that: the
//! string is consumed **once, at declaration**, by [`super::procs::find`] — an
//! in-process `/proc` scan that removes this daemon, its ancestors and its
//! protected pids before matching, so it cannot find itself — and what the
//! monitor holds from then on is [`super::procs::ProcessCondition`]: (pid,
//! start time) handles. The poller never sees the string.
//!
//! The evidence is not an argument about taste. On this box a process check
//! self-matched its own shell **seven times in one session** with the lesson in
//! memory and T21 open in `TODO.md`, and the seventh — `pkill -f
//! 'harnessd-rice.sock'` — killed a running command mid-flight, because the
//! string was in the shell's own command line. Prompting has now failed eight
//! times against this hazard. `docs/tool-design-brief.md` §2.4: *make the mistake
//! blocked, do not warn about it.*
//!
//! [`crate::builtins::monitor`]'s test asserts no monitor tool has grown a
//! `pattern`, `match` or `cmdline` argument, the way the job tools already do.
//!
//! # What a monitor may watch, and what it may not
//!
//! | may | keyed on |
//! |---|---|
//! | a job finishing | the job's handle |
//! | a scope emptying | the cgroup |
//! | a path appearing, vanishing or changing | the path |
//! | a loopback TCP port becoming listenable, or stopping | the port number |
//! | a process leaving | (pid, start time) — from a `pid`, or from a `process` string resolved once |
//!
//! **Refused: an arbitrary shell predicate.** That is a polling loop with extra
//! steps, and `flowy wait`'s own help records what happens: three seats each
//! hand-wrote one, and somebody wrote it in seventy-six spellings. A predicate
//! the model composes is a predicate the model composes *differently every time*,
//! and one of those spellings matches the shell evaluating it.
//!
//! **Refused: a host.** [`Watch::Port`] is loopback only and has no host
//! argument, which is what keeps the whole tool out of [`crate::schema::Access::Network`]
//! — a monitor that could reach an arbitrary address would have to declare
//! network access on *every* call, including the ones watching a cgroup.
//!
//! # T24's five requirements, and where each one is
//!
//! | | |
//! |---|---|
//! | 1. scoped at creation, default session | [`Monitor::owner`] is a [`ScopeId`] and there is no constructor without one |
//! | 2. listable and attributable | [`Monitors::list`], and [`Monitor::declared_by`] |
//! | 3. reports **why** it fired | [`Fired::Fired::why`] — a sentence naming the change, not a bit |
//! | 4. one waiter per name, enforced | [`Monitors::declare`] refuses [`MonitorError::NameTaken`] |
//! | 5. bounded | [`Monitor::ttl`], capped at [`MAX_TTL`], renewed only by an explicit call |
//!
//! # The mechanism does not leak the thing it exists to stop
//!
//! A monitor is structurally a long-lived process, which is why T24 would not let
//! them land before the lifetime work. So: **one poller thread for all monitors,
//! started lazily on the first declaration and exiting when nothing is left
//! watching.** No monitors, no thread. A thread per monitor would have made this
//! module the leak.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime};

use super::jobs::{Job, JobId};
use super::scope::ScopeId;

/// The longest a monitor may live without being renewed. **One hour.**
///
/// T24 requirement 5: *"a TTL or an explicit renewal, so a monitor whose reason
/// has passed dies without anybody remembering it."* The cap is what makes the
/// requirement true of a monitor whose declarer never came back — a TTL the
/// caller chooses is only a bound if there is a bound on the choice.
pub const MAX_TTL: Duration = Duration::from_secs(3600);
/// What a monitor gets when nobody said. Five minutes: long enough for a build,
/// short enough that a forgotten one is gone before anybody has to decide about
/// it.
pub const DEFAULT_TTL: Duration = Duration::from_secs(300);
/// How often the poller looks. Deliberately unhurried: a monitor exists because
/// nobody is waiting on it this instant.
const TICK: Duration = Duration::from_millis(250);

/// What a monitor watches. The built-in conditions are the first-class, listable
/// ones; [`Watch::Custom`] carries a caller-supplied [`Condition`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Watch {
    /// A job leaving `Running`. Keyed on the handle the model already holds.
    Job(JobId),
    /// A scope's cgroup emptying. Keyed on the cgroup — *"is this cgroup
    /// populated"*, which is the replacement `TODO.md` T24 names for `until pgrep
    /// -f X`, and which has no predicate that can match its own waiter.
    Scope(ScopeId),
    /// A path appearing, vanishing, or changing size or mtime.
    Path(PathBuf),
    /// A **loopback** TCP port reaching the wanted state. No host argument: see
    /// this module's docs for why that omission is load-bearing.
    Port { port: u16, want: PortState },
    /// A caller-supplied condition: a timer, a command, a log tail, a flowy
    /// watcher. The open seam — built-ins are here, everything else is a
    /// [`Condition`].
    Custom(CustomWatch),
}

/// A [`Watch::Custom`], behind an `Arc` so the enum stays `Clone`. Compared by
/// pointer identity: two `Custom` watches are equal iff they are the same
/// condition object.
#[derive(Clone)]
pub struct CustomWatch(pub Arc<dyn Condition>);

impl std::fmt::Debug for CustomWatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Custom({})", self.0.describe())
    }
}

impl PartialEq for CustomWatch {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for CustomWatch {}

/// A condition a monitor watches. The open seam: a flowy watcher, a log tail, a
/// health check — anything that can answer "is it met" — is a `Condition`, not a
/// new arm of a closed enum.
///
/// Evaluation is **poll-based**: the monitor's runner calls [`Condition::met`]
/// each pass. A condition that can be signalled instead (a job's `finished`
/// condvar) overrides [`Condition::wait`] so the runner blocks rather than polls;
/// until then it polls on [`TICK`].
pub trait Condition: Send + Sync {
    /// Evaluate now. `Some(why)` when the condition is met, `None` otherwise.
    fn met(&self) -> Option<String>;

    /// How the runner waits before the next evaluation. `Block` means the
    /// condition can signal (push); `Sleep` means poll.
    ///
    /// Defaults to polling on [`TICK`]; a blockable condition overrides it.
    fn wait(&self) -> Wait {
        Wait::Sleep(TICK)
    }

    /// Reset after a firing, so a **continuous** monitor watches for the next
    /// occurrence. Default: stateless — the condition is re-evaluated as-is.
    fn rearm(&self) {}

    /// One line for a listing.
    fn describe(&self) -> String;

    /// **Push**: called once at declaration with a signal the condition can ping
    /// when it changes, so the poller wakes immediately rather than on [`TICK`].
    /// The default does nothing — a poll condition. A blockable one (a channel
    /// fed by a subprocess, a job handle) overrides this and pings `signal`.
    ///
    /// `self: Arc<Self>` so an implementation can move itself into a notifier
    /// thread.
    fn install(self: Arc<Self>, _signal: Arc<dyn Fn() + Send + Sync>) {}
}

/// How a runner waits before the next evaluation.
#[derive(Debug, Clone, Copy)]
pub enum Wait {
    /// Block up to the deadline; the condition wakes the runner (push).
    Block(Duration),
    /// Sleep this long and evaluate again (poll).
    Sleep(Duration),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortState {
    Listening,
    Closed,
}

impl PortState {
    pub fn as_str(&self) -> &'static str {
        match self {
            PortState::Listening => "listening",
            PortState::Closed => "closed",
        }
    }

    pub fn parse(s: &str) -> Option<PortState> {
        match s {
            "listening" | "listen" | "up" => Some(PortState::Listening),
            "closed" | "gone" | "down" => Some(PortState::Closed),
            _ => None,
        }
    }

    /// The other state, for a continuous port monitor that alternates.
    pub fn flip(self) -> PortState {
        match self {
            PortState::Listening => PortState::Closed,
            PortState::Closed => PortState::Listening,
        }
    }
}

impl Watch {
    /// What a listing calls this, in words that name the handle rather than the
    /// condition — because the handle is what a reader needs to go look.
    pub fn describe(&self) -> String {
        match self {
            Watch::Job(id) => format!("job `{id}` leaving the running state"),
            Watch::Scope(s) => format!("scope `{s}` emptying"),
            Watch::Path(p) => format!("path `{}` changing", p.display()),
            Watch::Port { port, want } => {
                format!("loopback port {port} becoming {}", want.as_str())
            }
            Watch::Custom(c) => c.0.describe(),
        }
    }
}

/// How a monitor ended. **Four endings and they never collapse.**
///
/// The one that matters is that [`Fired::Expired`] is not a firing. A monitor
/// whose TTL passed learned nothing about the world, and reporting it as though
/// the condition happened is the same defect as reporting a deadline as a
/// completion — F5, one primitive over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fired {
    /// The condition happened, and `why` says **what changed**, not that
    /// something did. T24 requirement 3.
    Fired {
        why: String,
        at: SystemTime,
        after: Duration,
    },
    /// The TTL passed and the condition never happened. Carries what the last
    /// look actually saw, so the absence is a measurement rather than a silence.
    Expired { after: Duration, last_seen: String },
    /// Somebody retired it by name.
    Cancelled { by: String, after: Duration },
    /// **Its owning scope ended, so it went with it.** This is the reaping half
    /// of T24 applied to watchers: a monitor whose owner is gone is not something
    /// anybody is coming back for.
    OwnerEnded { scope: ScopeId, after: Duration },
}

impl Fired {
    /// Did the thing being watched actually happen?
    ///
    /// Deliberately narrow. Three of the four endings are ways a monitor stopped
    /// existing, and only one of them is an answer about the world.
    pub fn happened(&self) -> bool {
        matches!(self, Fired::Fired { .. })
    }

    pub fn word(&self) -> String {
        match self {
            Fired::Fired { why, after, .. } => {
                format!("FIRED after {:.1}s — {why}", after.as_secs_f32())
            }
            Fired::Expired { after, last_seen } => format!(
                "expired after {:.0}s WITHOUT firing; at the last look, {last_seen}",
                after.as_secs_f32()
            ),
            Fired::Cancelled { by, after } => {
                format!("retired by {by} after {:.0}s", after.as_secs_f32())
            }
            Fired::OwnerEnded { scope, after } => format!(
                "its owner `{scope}` ended after {:.0}s, so it was retired with it",
                after.as_secs_f32()
            ),
        }
    }
}

/// One declared watch.
#[derive(Debug)]
pub struct Monitor {
    /// **The name is the caller's**, and it is the identity T24 requirement 4
    /// enforces one waiter per.
    pub name: String,
    /// Who owns its lifetime. There is no constructor without one: T24
    /// requirement 1, *"no monitor without an owner"*, as a type rather than as a
    /// convention.
    pub owner: ScopeId,
    pub watch: Watch,
    /// Attribution. A watcher nobody can trace to a declarer is the invisible
    /// one, and *"an invisible watcher is an unreapable one."*
    pub declared_by: String,
    pub created: SystemTime,
    started: Instant,
    /// Bounded, and renewed only by somebody asking. Behind the lock because a
    /// renewal moves it.
    deadline: Mutex<Instant>,
    ttl: Mutex<Duration>,
    probe: Probe,
    /// **Presence, before absence.** A [`Watch::Scope`] must see the cgroup hold
    /// something before an empty one means anything: an empty scope at t=0 is a
    /// boot window and not a finish, and this flag is the only thing that keeps
    /// the two apart. Unused by the other three probes, which have no such
    /// ambiguity — a path either changed or did not.
    seen_populated: AtomicBool,
    /// **Continuous** rather than one-shot: after a firing the monitor re-arms and
    /// keeps watching instead of settling. One-shot is the default; a continuous
    /// monitor lives until its TTL or an explicit retire. It is a **persistent**
    /// watcher — it does not die between firings; each firing is an event in the
    /// stream the harness wakes on.
    repeat: bool,
}

/// One firing of a monitor — a message in the stream the harness wakes on. A
/// continuous monitor produces a stream of these without dying; a one-shot monitor
/// produces exactly one. Recorded separately from the live [`Monitor`], so a
/// continuous monitor's stream of firings is a list of events rather than a shared
/// mutable cell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Firing {
    pub name: String,
    /// The condition, described.
    pub watch: String,
    pub declared_by: String,
    pub fired: Fired,
    pub at: SystemTime,
}

/// The resolved thing a monitor looks at each tick.
///
/// Separate from [`Watch`] because a `Watch` is a *description* a listing can
/// clone and compare, and this holds live handles. A job monitor keeps the
/// `Arc<Job>` it was declared against rather than looking the id up every tick:
/// the id could be re-used by a different host, and a monitor that silently
/// re-targeted would be watching something nobody asked about.
enum Probe {
    Job(Arc<Job>),
    Scope(ScopeId),
    Path {
        path: PathBuf,
        /// Behind a lock so a **continuous** monitor can re-baseline at the new
        /// state after each firing.
        baseline: Mutex<PathFacts>,
    },
    Port {
        port: u16,
        /// Behind a lock so a continuous monitor can flip its target after firing.
        want: Mutex<PortState>,
    },
    Custom(Arc<dyn Condition>),
}

impl std::fmt::Debug for Probe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Probe")
    }
}

/// What a path looked like when the monitor was declared. The **baseline** — a
/// path monitor with no baseline could only report existence, and "it changed"
/// is the question people actually have.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PathFacts {
    exists: bool,
    len: u64,
    mtime: Option<SystemTime>,
}

impl PathFacts {
    fn read(p: &Path) -> PathFacts {
        match std::fs::metadata(p) {
            Ok(m) => PathFacts {
                exists: true,
                len: m.len(),
                mtime: m.modified().ok(),
            },
            Err(_) => PathFacts {
                exists: false,
                len: 0,
                mtime: None,
            },
        }
    }

    /// What changed, in a sentence, or `None` if nothing did.
    fn changed_from(&self, base: &PathFacts) -> Option<String> {
        if self.exists != base.exists {
            return Some(if self.exists {
                "it now exists; it did not when the monitor was declared".into()
            } else {
                "it is gone; it existed when the monitor was declared".into()
            });
        }
        if !self.exists {
            return None;
        }
        if self.len != base.len {
            return Some(format!(
                "its size went from {} to {} bytes",
                base.len, self.len
            ));
        }
        if self.mtime != base.mtime {
            return Some("its modification time moved".into());
        }
        None
    }

    fn describe(&self) -> String {
        if !self.exists {
            return "it does not exist".into();
        }
        format!("it exists and is {} bytes", self.len)
    }
}

impl Monitor {
    pub fn ttl(&self) -> Duration {
        *self.ttl.lock().expect("monitor ttl")
    }

    /// How long before the TTL takes it. A monitor is live until it expires,
    /// retires or its owner ends; `remaining` is the TTL countdown.
    pub fn remaining(&self) -> Option<Duration> {
        let d = *self.deadline.lock().expect("monitor deadline");
        Some(d.saturating_duration_since(Instant::now()))
    }

    pub fn age(&self) -> Duration {
        self.started.elapsed()
    }

    /// T24 requirement 5's other half: an **explicit** renewal. Nothing renews a
    /// monitor by looking at it, because a monitor that lives as long as somebody
    /// keeps listing it is a monitor with no bound.
    fn renew(&self, ttl: Duration) {
        *self.ttl.lock().expect("monitor ttl") = ttl;
        *self.deadline.lock().expect("monitor deadline") = Instant::now() + ttl;
    }

    /// One firing event for this monitor, carrying the ending and the attribution.
    fn firing(&self, fired: Fired) -> Firing {
        Firing {
            name: self.name.clone(),
            watch: self.watch.describe(),
            declared_by: self.declared_by.clone(),
            fired,
            at: SystemTime::now(),
        }
    }

    /// One look. `Some(why)` when the condition has happened.
    fn look(&self) -> Option<String> {
        match &self.probe {
            Probe::Job(job) => {
                let st = job.state();
                if st.is_running() {
                    return None;
                }
                // The state, not "it is gone": `exited 0`, `killed by job_kill`
                // and `not run (could not join its scope)` are three different
                // things to have happened and a monitor that flattened them would
                // be reporting the fact it was created to distinguish.
                Some(format!(
                    "`{}` {} after {:.1}s",
                    job.id,
                    st.word(),
                    job.elapsed().as_secs_f32()
                ))
            }
            Probe::Scope(s) => {
                // **Presence, then absence.** A scope directory that is gone is a
                // scope that ended; one that exists and holds nothing has to have
                // been seen holding something first, or an empty cgroup at t=0
                // reads as a finish when it is a boot window.
                if std::fs::metadata(&s.path).is_err() {
                    return Some(format!("`{s}` no longer exists — the scope ended"));
                }
                if !super::scope::populated_at(&s.path) {
                    if !self.seen_populated.load(Ordering::Relaxed) {
                        return None;
                    }
                    return Some(format!(
                        "`{s}` held at least one process while this monitor was \
                         watching, and now holds none"
                    ));
                }
                self.seen_populated.store(true, Ordering::Relaxed);
                None
            }
            Probe::Path { path, baseline } => PathFacts::read(path)
                .changed_from(&baseline.lock().expect("path baseline"))
                .map(|w| format!("`{}`: {w}", path.display())),
            Probe::Port { port, want } => {
                let want = *want.lock().expect("port want");
                let now = if super::host::port_is_listening(*port) {
                    PortState::Listening
                } else {
                    PortState::Closed
                };
                (now == want).then(|| format!("loopback port {port} is {}", want.as_str()))
            }
            Probe::Custom(c) => c.met(),
        }
    }

    /// What the last look saw, for an expiry that has to say something.
    fn last_seen(&self) -> String {
        match &self.probe {
            Probe::Job(job) => format!("`{}` was {}", job.id, job.state().word()),
            Probe::Scope(s) => {
                if std::fs::metadata(&s.path).is_err() {
                    format!("`{s}` did not exist")
                } else if super::scope::populated_at(&s.path) {
                    format!("`{s}` still held at least one process")
                } else {
                    format!("`{s}` was empty and had never been seen populated")
                }
            }
            Probe::Path { path, .. } => {
                format!("`{}`: {}", path.display(), PathFacts::read(path).describe())
            }
            Probe::Port { port, .. } => format!(
                "loopback port {port} was {}",
                if super::host::port_is_listening(*port) {
                    "listening"
                } else {
                    "not listening"
                }
            ),
            Probe::Custom(c) => format!("the condition `{}` was not met", c.describe()),
        }
    }
}

/// A scope watch's presence flag, kept out of the struct literal above so the
/// field order reads.
impl Monitor {
    fn new(
        name: String,
        owner: ScopeId,
        watch: Watch,
        probe: Probe,
        declared_by: String,
        ttl: Duration,
        repeat: bool,
    ) -> Monitor {
        Monitor {
            name,
            owner,
            watch,
            declared_by,
            created: SystemTime::now(),
            started: Instant::now(),
            deadline: Mutex::new(Instant::now() + ttl),
            ttl: Mutex::new(ttl),
            probe,
            seen_populated: AtomicBool::new(false),
            repeat,
        }
    }

    /// Re-arm after a firing, in place: a **continuous** monitor is persistent, so
    /// it re-baselines its probe for the *next* occurrence rather than dying. The
    /// path is re-read at its new state, the port's target flips, and a custom
    /// condition re-arms itself. A job or scope is terminal and has no next
    /// occurrence.
    fn rearm(&self) {
        match &self.probe {
            Probe::Path { path, baseline } => {
                *baseline.lock().expect("path baseline") = PathFacts::read(path);
            }
            Probe::Port { want, .. } => {
                let flip = want.lock().expect("port want").flip();
                *want.lock().expect("port want") = flip;
            }
            Probe::Custom(c) => c.rearm(),
            Probe::Job(_) | Probe::Scope(_) => {}
        }
    }
}

/// Why a monitor was not declared. Every variant hands over what the retry needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MonitorError {
    /// **One waiter per name.** T24 requirement 4, and the fleet's own words:
    /// *two processes under one reader means the roster shows a seat attached
    /// while the real one hears nothing.*
    NameTaken {
        name: String,
        watching: String,
        owner: ScopeId,
        declared_by: String,
        age: Duration,
    },
    /// The TTL asked for is past the cap.
    Unbounded { asked: Duration, cap: Duration },
    /// The job named is not one this session started. The tool layer resolves a
    /// job before it gets here and supplies the listing, so this is the
    /// defensive path rather than the one a model sees.
    NoSuchJob { name: String },
    /// Renew or retire named something that is not watching.
    NoSuchMonitor { name: String, live: Vec<String> },
}

impl std::fmt::Display for MonitorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MonitorError::NameTaken {
                name,
                watching,
                owner,
                declared_by,
                age,
            } => write!(
                f,
                "a monitor called `{name}` is already watching {watching}. It was \
                 declared by {declared_by} {:.0}s ago and is owned by `{owner}`. \
                 NOTHING was declared and the existing one is untouched — one waiter \
                 per name is the rule, because two watchers under one name means a \
                 listing shows the name attached while one of them hears nothing. \
                 Either use a different `name`, or retire this one with \
                 action=\"retire\" first.",
                age.as_secs_f32()
            ),
            MonitorError::Unbounded { asked, cap } => write!(
                f,
                "a ttl of {:.0}s is past the cap of {:.0}s, so nothing was declared. \
                 A monitor is bounded so that one whose reason has passed dies \
                 without anybody remembering it; a ttl nobody caps is not a bound. \
                 Ask for {:.0}s or less, and renew it with action=\"renew\" if the \
                 reason is still live.",
                asked.as_secs_f32(),
                cap.as_secs_f32(),
                cap.as_secs_f32()
            ),
            MonitorError::NoSuchJob { name } => write!(
                f,
                "no job called `{name}`, so nothing was declared. `job_list` shows the \
                 jobs this session has started; monitor one of those ids."
            ),
            MonitorError::NoSuchMonitor { name, live } => {
                if live.is_empty() {
                    write!(
                        f,
                        "no monitor called `{name}`, and nothing is watching right now \
                         — so there is nothing to look in. `job_list` shows what has \
                         already settled and why."
                    )
                } else {
                    write!(
                        f,
                        "no monitor called `{name}`. Watching right now: {}.",
                        live.join(", ")
                    )
                }
            }
        }
    }
}

impl std::error::Error for MonitorError {}

/// Every monitor this session has, and the one thread that looks at them.
///
/// **Listable by construction** (T24 requirement 2): there is no way to declare a
/// monitor that this does not hold, so there is no monitor a listing cannot show.
#[derive(Debug, Default)]
pub struct Monitors {
    inner: Mutex<Registry>,
    /// Woken when a monitor settles. **This is the wake seam** — see
    /// [`Monitors::wait_for_any`].
    settled: Condvar,
    /// Woken by a blockable condition (push) so the poller re-polls immediately
    /// instead of waiting out [`TICK`]. Guarded by [`Monitors::wake_guard`].
    wake: Condvar,
    wake_guard: Mutex<()>,
}

#[derive(Debug, Default)]
struct Registry {
    /// Ordered by name so a listing is stable across calls; an unstable listing
    /// re-renders differently every turn and costs the prefix cache.
    live: BTreeMap<String, Arc<Monitor>>,
    /// The stream of firings, kept so that "why did it fire" survives the firing. A
    /// record that is deleted at the moment it becomes interesting is not a record.
    /// A continuous monitor appends one entry per firing without leaving `live`.
    events: Vec<Firing>,
    /// Whether the poller thread is running. Not a handle: the thread exits on
    /// its own when nothing is watching, and holding a `JoinHandle` for a thread
    /// nobody joins is bookkeeping that can only go stale.
    polling: bool,
}

impl Monitors {
    pub fn new() -> Monitors {
        Monitors {
            inner: Mutex::new(Registry::default()),
            settled: Condvar::new(),
            wake: Condvar::new(),
            wake_guard: Mutex::new(()),
        }
    }

    /// **Push**: a blockable condition calls this when it changes, so the poller
    /// wakes now rather than on the next [`TICK`]. Cheap and idempotent — a wake
    /// with nothing new to look at costs one re-poll.
    pub fn signal(&self) {
        self.wake.notify_all();
    }

    /// The signal a [`Condition::install`] gets: a `'static` closure that pings this
    /// registry's poller.
    fn signal_fn(self: &Arc<Self>) -> Arc<dyn Fn() + Send + Sync> {
        let weak = Arc::downgrade(self);
        Arc::new(move || {
            if let Some(me) = weak.upgrade() {
                me.signal();
            }
        })
    }

    /// Declare a watch. **Refuses rather than replacing** when the name is taken.
    ///
    /// `job` must be the live handle rather than an id, so that a monitor cannot
    /// be declared against a job that does not exist — see [`Probe`]. `repeat`
    /// makes it **continuous**: it re-arms after each firing instead of settling.
    pub fn declare(
        self: &Arc<Self>,
        name: &str,
        owner: ScopeId,
        watch: Watch,
        job: Option<Arc<Job>>,
        declared_by: &str,
        ttl: Duration,
        repeat: bool,
    ) -> Result<Arc<Monitor>, Box<MonitorError>> {
        if ttl > MAX_TTL {
            return Err(Box::new(MonitorError::Unbounded {
                asked: ttl,
                cap: MAX_TTL,
            }));
        }
        let probe = match (&watch, job) {
            (Watch::Job(_), Some(j)) => Probe::Job(j),
            (Watch::Job(id), None) => {
                return Err(Box::new(MonitorError::NoSuchJob { name: id.0.clone() }));
            }
            (Watch::Scope(s), _) => Probe::Scope(s.clone()),
            (Watch::Path(p), _) => Probe::Path {
                path: p.clone(),
                // The baseline is read HERE, at declaration, and not at the first
                // tick: a baseline read a quarter-second late has already missed
                // the change it exists to detect.
                baseline: Mutex::new(PathFacts::read(p)),
            },
            (Watch::Port { port, want }, _) => Probe::Port {
                port: *port,
                want: Mutex::new(*want),
            },
            (Watch::Custom(c), _) => {
                let cond = c.0.clone();
                // Install the push signal, so a blockable condition wakes the
                // poller instead of waiting out TICK. Clone into `install` — it
                // takes `self` by value — and keep the original for the probe.
                cond.clone().install(self.signal_fn());
                Probe::Custom(cond)
            }
        };

        let mut reg = self.inner.lock().expect("monitors");
        if let Some(existing) = reg.live.get(name) {
            return Err(Box::new(MonitorError::NameTaken {
                name: name.to_string(),
                watching: existing.watch.describe(),
                owner: existing.owner.clone(),
                declared_by: existing.declared_by.clone(),
                age: existing.age(),
            }));
        }
        let m = Arc::new(Monitor::new(
            name.to_string(),
            owner,
            watch,
            probe,
            declared_by.to_string(),
            ttl,
            repeat,
        ));
        reg.live.insert(name.to_string(), Arc::clone(&m));
        let start = !reg.polling;
        reg.polling = true;
        drop(reg);
        if start {
            self.spawn_poller();
        }
        Ok(m)
    }

    /// Extend a monitor's TTL. T24 requirement 5's explicit renewal.
    pub fn renew(&self, name: &str, ttl: Duration) -> Result<Arc<Monitor>, Box<MonitorError>> {
        if ttl > MAX_TTL {
            return Err(Box::new(MonitorError::Unbounded {
                asked: ttl,
                cap: MAX_TTL,
            }));
        }
        let reg = self.inner.lock().expect("monitors");
        let Some(m) = reg.live.get(name).cloned() else {
            return Err(Box::new(MonitorError::NoSuchMonitor {
                name: name.to_string(),
                live: reg.live.keys().cloned().collect(),
            }));
        };
        m.renew(ttl);
        Ok(m)
    }

    /// Retire one by name. Returns the firing record (`Cancelled`).
    pub fn retire(&self, name: &str, by: &str) -> Option<Firing> {
        let mut reg = self.inner.lock().expect("monitors");
        let m = reg.live.remove(name)?;
        let f = m.firing(Fired::Cancelled {
            by: by.to_string(),
            after: m.age(),
        });
        reg.events.push(f.clone());
        drop(reg);
        self.settled.notify_all();
        Some(f)
    }

    /// **Retire every monitor a scope owned, because the scope ended.**
    ///
    /// Called from the reaping path rather than left to the poller, so the record
    /// lands at the moment the scope goes and says which scope took it. A monitor
    /// that merely stopped being polled would be a watcher that went quiet, and a
    /// watcher that goes quiet looks exactly like one whose condition never fired.
    pub fn retire_under(&self, scope: &ScopeId) -> Vec<Firing> {
        let mut reg = self.inner.lock().expect("monitors");
        let doomed: Vec<String> = reg
            .live
            .iter()
            .filter(|(_, m)| m.owner.path.starts_with(&scope.path))
            .map(|(n, _)| n.clone())
            .collect();
        let mut out = Vec::new();
        for n in doomed {
            if let Some(m) = reg.live.remove(&n) {
                let f = m.firing(Fired::OwnerEnded {
                    scope: scope.clone(),
                    after: m.age(),
                });
                reg.events.push(f.clone());
                out.push(f);
            }
        }
        drop(reg);
        if !out.is_empty() {
            self.settled.notify_all();
        }
        out
    }

    /// Everything watching now, oldest name first.
    pub fn list(&self) -> Vec<Arc<Monitor>> {
        self.inner
            .lock()
            .expect("monitors")
            .live
            .values()
            .cloned()
            .collect()
    }

    /// The stream of firings, in the order they fired. The falsifier: a session
    /// with no monitors and a session whose monitors all expired without firing
    /// are different facts — and a **continuous** monitor contributes one entry per
    /// firing while it keeps watching.
    pub fn firings(&self) -> Vec<Firing> {
        self.inner.lock().expect("monitors").events.clone()
    }

    /// One live monitor by name. A settled one is no longer watching, so it is not
    /// here — its firing is in [`Monitors::firings`].
    pub fn get(&self, name: &str) -> Option<Arc<Monitor>> {
        self.inner.lock().expect("monitors").live.get(name).cloned()
    }

    /// The names watching right now, for a refusal that has to say what there is.
    pub fn live_names(&self) -> Vec<String> {
        self.inner
            .lock()
            .expect("monitors")
            .live
            .keys()
            .cloned()
            .collect()
    }

    /// **The wake seam.** Block until a monitor fires, or until the deadline.
    ///
    /// This is what a loop calls between turns: it is the "wakes the loop when it
    /// fires" half of T24's sentence, and it is a `Condvar` rather than a poll so
    /// that a caller waiting on it costs nothing while nothing is happening.
    ///
    /// Returns the firings **during this call**, which is why it takes a `since`: a
    /// caller that asked twice would otherwise be handed the same firing twice and
    /// act on it twice.
    pub fn wait_for_any(&self, since: usize, deadline: Duration) -> Vec<Firing> {
        let reg = self.inner.lock().expect("monitors");
        let (reg, _) = self
            .settled
            .wait_timeout_while(reg, deadline, |r| r.events.len() <= since)
            .expect("monitors");
        reg.events.iter().skip(since).cloned().collect()
    }

    /// How many firings have happened. The cursor [`Monitors::wait_for_any`] takes.
    pub fn settled_count(&self) -> usize {
        self.inner.lock().expect("monitors").events.len()
    }

    /// One pass over every live monitor. Exposed so a test does not have to sleep
    /// for the poller — a test that waits on a thread it cannot see is a test that
    /// is flaky on a loaded box.
    pub fn tick(&self) -> Vec<Firing> {
        let mut fired = Vec::new();
        let live: Vec<(String, Arc<Monitor>)> = {
            let reg = self.inner.lock().expect("monitors");
            reg.live
                .iter()
                .map(|(n, m)| (n.clone(), Arc::clone(m)))
                .collect()
        };
        for (name, m) in live {
            // The owner first. A monitor whose scope is gone must not fire on a
            // condition it noticed after nobody was left to care.
            let owner_gone = std::fs::metadata(&m.owner.path).is_err();
            let ending = if owner_gone {
                Some(Fired::OwnerEnded {
                    scope: m.owner.clone(),
                    after: m.age(),
                })
            } else if let Some(why) = m.look() {
                Some(Fired::Fired {
                    why,
                    at: SystemTime::now(),
                    after: m.age(),
                })
            } else if m.remaining().is_some_and(|r| r.is_zero()) {
                // Expiry is checked LAST, so a condition that happened inside the
                // final tick is reported as a firing rather than as a timeout.
                Some(Fired::Expired {
                    after: m.age(),
                    last_seen: m.last_seen(),
                })
            } else {
                None
            };
            let Some(ending) = ending else { continue };
            let is_fire = matches!(ending, Fired::Fired { .. });
            let event = m.firing(ending);
            let mut reg = self.inner.lock().expect("monitors");
            if m.repeat && is_fire {
                // **Continuous**: the monitor persists — it does not die and get
                // re-declared. It re-baselines its probe for the next occurrence and
                // keeps watching; the firing is one message in the stream.
                m.rearm();
            } else {
                reg.live.remove(&name);
            }
            reg.events.push(event.clone());
            fired.push(event);
        }
        if !fired.is_empty() {
            self.settled.notify_all();
        }
        fired
    }

    /// One thread for **all** monitors, and it exits when there is nothing left
    /// to watch.
    ///
    /// A thread per monitor would make this module the accumulation T24 exists to
    /// stop — *"you guys like to accumulate monitors and shells"*. The `Arc` is
    /// downgraded so that a session dropping its `Monitors` lets the thread
    /// finish rather than keeping the registry alive to be polled forever.
    fn spawn_poller(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        let _ = std::thread::Builder::new()
            .name("letibot-monitors".into())
            .spawn(move || {
                loop {
                    // Wait for a push signal or the poll interval, whichever comes
                    // first. A blockable condition (a job, a channel) wakes this
                    // immediately; everything else is re-polled every TICK.
                    let Some(me) = weak.upgrade() else { return };
                    let g = me.wake_guard.lock().expect("monitor wake");
                    let _ = me.wake.wait_timeout(g, TICK).expect("monitor wake");
                    me.tick();
                    let mut reg = me.inner.lock().expect("monitors");
                    if reg.live.is_empty() {
                        // Nothing is watching, so nothing needs a thread. The next
                        // `declare` starts a new one.
                        reg.polling = false;
                        return;
                    }
                }
            });
    }
}

// ---------------------------------------------------------------------------
// Shipped conditions. A flowy watcher, a log tail, a health check — these are the
// examples of the [`Condition`] seam, and the things a caller builds its own on
// top of.
// ---------------------------------------------------------------------------

/// A timer: fires when the deadline passes. One-shot by default; on a continuous
/// monitor it re-arms and fires every `interval`.
#[derive(Debug)]
pub struct TimerCondition {
    interval: Duration,
    next: Mutex<Instant>,
}

impl TimerCondition {
    /// Fire after `interval`, and — when the monitor is continuous — every
    /// `interval` after each firing.
    pub fn after(interval: Duration) -> Arc<Self> {
        Arc::new(TimerCondition {
            interval,
            next: Mutex::new(Instant::now() + interval),
        })
    }
}

impl Condition for TimerCondition {
    fn met(&self) -> Option<String> {
        let next = *self.next.lock().expect("timer");
        (Instant::now() >= next)
            .then(|| format!("the {:.1}s timer elapsed", self.interval.as_secs_f32()))
    }

    fn rearm(&self) {
        let mut next = self.next.lock().expect("timer");
        *next = Instant::now() + self.interval;
    }

    fn describe(&self) -> String {
        format!("a timer every {:.1}s", self.interval.as_secs_f32())
    }
}

/// What a command condition fires on. `AnyExit` when the command exits — whatever
/// the code; `OutputContains` when its stdout contains the string; `NumberAbove`
/// and `NumberBelow` when its stdout is a number that crosses the threshold (a
/// temperature, a token count, a latency).
#[derive(Debug, Clone, PartialEq)]
pub enum CommandExpect {
    AnyExit,
    OutputContains(String),
    NumberAbove(f64),
    NumberBelow(f64),
}

/// A command returning: the condition is met by the command exiting, its output
/// matching, or its numeric output crossing a threshold. The command is **spawned
/// once and polled** (`try_wait`), not run to completion on every look — so a
/// command that blocks until some external event (a bash script waiting on a
/// marker) works as a condition without stalling the poller. Run on the host, not
/// through the session's exec boundary — a monitor condition is a check, not a job.
#[derive(Debug)]
pub struct CommandCondition {
    argv: Vec<String>,
    expect: CommandExpect,
    /// The running child, spawned lazily on the first look and re-checked each look.
    /// Behind the `Mutex` so a continuous monitor's `rearm` can restart it.
    child: Mutex<Option<std::process::Child>>,
}

impl CommandCondition {
    /// Fire when the command exits, whatever the code.
    pub fn any(argv: Vec<String>) -> Arc<Self> {
        Arc::new(CommandCondition {
            argv,
            expect: CommandExpect::AnyExit,
            child: Mutex::new(None),
        })
    }

    /// Fire when the command's stdout contains `s`.
    pub fn containing(argv: Vec<String>, s: impl Into<String>) -> Arc<Self> {
        Arc::new(CommandCondition {
            argv,
            expect: CommandExpect::OutputContains(s.into()),
            child: Mutex::new(None),
        })
    }

    /// Fire when the command's stdout is a number strictly above `n`.
    pub fn above(argv: Vec<String>, n: f64) -> Arc<Self> {
        Self::new(argv, CommandExpect::NumberAbove(n))
    }

    /// Fire when the command's stdout is a number strictly below `n`.
    pub fn below(argv: Vec<String>, n: f64) -> Arc<Self> {
        Self::new(argv, CommandExpect::NumberBelow(n))
    }

    pub fn new(argv: Vec<String>, expect: CommandExpect) -> Arc<Self> {
        Arc::new(CommandCondition {
            argv,
            expect,
            child: Mutex::new(None),
        })
    }

    /// Spawn the child on the first look, if it is not already running.
    fn ensure_spawned(&self) -> Result<(), String> {
        let mut child = self.child.lock().expect("command child");
        if child.is_some() {
            return Ok(());
        }
        let Some(prog) = self.argv.first() else {
            return Err("the command was empty, so it cannot be run".into());
        };
        let c = std::process::Command::new(prog)
            .args(&self.argv[1..])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("the command could not be run: {e}"))?;
        *child = Some(c);
        Ok(())
    }
}

impl Condition for CommandCondition {
    fn met(&self) -> Option<String> {
        if let Err(e) = self.ensure_spawned() {
            return Some(e);
        }
        let mut child = self.child.lock().expect("command child");
        let c = child.as_mut()?;
        match c.try_wait() {
            Ok(Some(status)) => {
                // The child has exited, so draining its stdout cannot block.
                let mut out = Vec::new();
                if let Some(mut so) = c.stdout.take() {
                    let _ = std::io::Read::read_to_end(&mut so, &mut out);
                }
                let text = String::from_utf8_lossy(&out);
                match &self.expect {
                    CommandExpect::AnyExit => Some(match status.code() {
                        Some(0) => "the command exited 0".into(),
                        Some(n) => format!("the command exited {n}"),
                        None => "the command exited (signalled)".into(),
                    }),
                    CommandExpect::OutputContains(s) if text.contains(s.as_str()) => {
                        Some(format!("the command's output contained `{s}`"))
                    }
                    CommandExpect::NumberAbove(n) => parse_number(&text)
                        .and_then(|v| (v > *n).then(|| format!("{v} is above {n}"))),
                    CommandExpect::NumberBelow(n) => parse_number(&text)
                        .and_then(|v| (v < *n).then(|| format!("{v} is below {n}"))),
                    _ => None,
                }
            }
            Ok(None) => None,
            Err(e) => Some(format!("the command could not be waited: {e}")),
        }
    }

    fn rearm(&self) {
        // A continuous command monitor restarts the command after each firing.
        *self.child.lock().expect("command child") = None;
    }

    fn describe(&self) -> String {
        match &self.expect {
            CommandExpect::AnyExit => format!("command `{}` exiting", self.argv.join(" ")),
            CommandExpect::OutputContains(s) => {
                format!("command `{}` printing `{s}`", self.argv.join(" "))
            }
            CommandExpect::NumberAbove(n) => {
                format!(
                    "command `{}` printing a number above {n}",
                    self.argv.join(" ")
                )
            }
            CommandExpect::NumberBelow(n) => {
                format!(
                    "command `{}` printing a number below {n}",
                    self.argv.join(" ")
                )
            }
        }
    }
}

/// The first number in a command's stdout, for a numeric threshold. Whitespace is
/// trimmed; a unit suffix like `°C` or `ms` is ignored.
fn parse_number(text: &str) -> Option<f64> {
    text.split_whitespace()
        .find_map(|w| w.trim_end_matches(['C', 'c', 's', 'm']).parse::<f64>().ok())
}

/// A log tail: fires on each new line that matches a filter. A **continuous**
/// monitor's example — the condition is stateless between firings except for how
/// far it has read, which `rearm` advances.
#[derive(Debug)]
pub struct LogTailCondition {
    path: PathBuf,
    contains: String,
    read: Mutex<u64>,
}

impl LogTailCondition {
    pub fn new(path: impl Into<PathBuf>, contains: impl Into<String>) -> Arc<Self> {
        Arc::new(LogTailCondition {
            path: path.into(),
            contains: contains.into(),
            read: Mutex::new(0),
        })
    }
}

impl Condition for LogTailCondition {
    fn met(&self) -> Option<String> {
        let Ok(bytes) = std::fs::read(&self.path) else {
            return None;
        };
        let mut read = self.read.lock().expect("log tail");
        let start = (*read).min(bytes.len() as u64) as usize;
        let tail = &bytes[start..];
        // Advance only when there is a matching line, so a continuous monitor
        // fires once per new matching line and not once per tick.
        for line in String::from_utf8_lossy(tail).lines() {
            if line.contains(&self.contains) {
                *read = bytes.len() as u64;
                return Some(format!("a line matched `{}`: {line}", self.contains));
            }
        }
        None
    }

    fn describe(&self) -> String {
        format!(
            "path `{}` growing a line containing `{}`",
            self.path.display(),
            self.contains
        )
    }
}

/// A channel-fed condition: a subprocess (or thread) writes messages to the
/// `Sender`, and each message is a firing. This is the **push** case — the
/// condition pings the poller on each message instead of being polled, so a
/// continuous monitor wakes the instant a message lands. One message per firing.
#[derive(Debug)]
pub struct ChannelCondition {
    /// The receiver, taken by [`Condition::install`] and moved into the notifier
    /// thread — `mpsc::Receiver` is `Send` but not `Sync`, so it cannot be shared,
    /// and sharing it behind a `Mutex` would hold a lock for the whole of a
    /// blocking `recv` (which deadlocks against the poller). `None` after install.
    rx: Mutex<Option<std::sync::mpsc::Receiver<String>>>,
    /// Messages the notifier has received and the poller has not yet drained.
    pending: Mutex<VecDeque<String>>,
}

impl ChannelCondition {
    /// A condition and the sender that feeds it. Drop the last `Sender` to end the
    /// monitor's stream (the notifier thread exits; the monitor then idles until
    /// its TTL).
    pub fn channel() -> (Arc<Self>, std::sync::mpsc::Sender<String>) {
        let (tx, rx) = std::sync::mpsc::channel();
        (
            Arc::new(ChannelCondition {
                rx: Mutex::new(Some(rx)),
                pending: Mutex::new(VecDeque::new()),
            }),
            tx,
        )
    }
}

impl Condition for ChannelCondition {
    fn met(&self) -> Option<String> {
        self.pending.lock().expect("channel pending").pop_front()
    }

    fn install(self: Arc<Self>, signal: Arc<dyn Fn() + Send + Sync>) {
        // Take the receiver out and move it into a notifier thread that pings the
        // poller per message, so the monitor fires the instant one lands.
        let Some(rx) = self.rx.lock().expect("channel rx").take() else {
            return;
        };
        let weak = Arc::downgrade(&self);
        let _ = std::thread::Builder::new()
            .name("letibot-condition".into())
            .spawn(move || {
                for m in rx {
                    let Some(me) = weak.upgrade() else { return };
                    me.pending.lock().expect("channel pending").push_back(m);
                    signal();
                }
            });
    }

    fn describe(&self) -> String {
        "a channel that messages are pushed into".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(dir: &Path) -> ScopeId {
        ScopeId {
            kind: super::super::scope::ScopeKind::Session,
            name: "t".into(),
            path: dir.to_path_buf(),
        }
    }

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "letibot-mon-{}-{:?}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            {
                static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            }
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Tick until something fires or the budget runs out. The command condition
    /// spawns and polls a child, so its first firing is asynchronous; a fixed
    /// single `tick()` races the spawn.
    fn tick_until(ms: &Monitors, budget: Duration) -> Vec<Firing> {
        let deadline = Instant::now() + budget;
        loop {
            let f = ms.tick();
            if !f.is_empty() {
                return f;
            }
            if Instant::now() >= deadline {
                return Vec::new();
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_second_monitor_under_one_name_is_refused_with_what_holds_it() {
        // T24 requirement 4. The fleet's most expensive lesson, as a refusal:
        // two watchers under one name means a listing shows the name attached
        // while one of them hears nothing.
        let dir = tmp();
        let ms = Arc::new(Monitors::new());
        let p = dir.join("f");
        ms.declare(
            "build",
            scope(&dir),
            Watch::Path(p.clone()),
            None,
            "turn-1",
            DEFAULT_TTL,
            false,
        )
        .expect("first");
        let e = ms
            .declare(
                "build",
                scope(&dir),
                Watch::Path(p),
                None,
                "turn-2",
                DEFAULT_TTL,
                false,
            )
            .unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains("already watching"), "{msg}");
        assert!(msg.contains("turn-1"), "{msg}");
        assert!(msg.contains("NOTHING was declared"), "{msg}");
        // And the existing one is untouched, which is the half a "replace"
        // implementation would silently get wrong.
        assert_eq!(ms.list().len(), 1);
        assert_eq!(ms.list()[0].declared_by, "turn-1");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_monitor_says_what_changed_and_not_merely_that_something_did() {
        // T24 requirement 3.
        let dir = tmp();
        let ms = Arc::new(Monitors::new());
        let p = dir.join("out.log");
        ms.declare(
            "log",
            scope(&dir),
            Watch::Path(p.clone()),
            None,
            "turn-1",
            DEFAULT_TTL,
            false,
        )
        .unwrap();
        assert!(ms.tick().is_empty(), "nothing has changed yet");
        std::fs::write(&p, b"hello").unwrap();
        let fired = ms.tick();
        assert_eq!(fired.len(), 1);
        match &fired[0].fired {
            Fired::Fired { why, .. } => {
                assert!(why.contains("now exists"), "{why}");
                assert!(why.contains("out.log"), "{why}");
            }
            other => panic!("{other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_expiry_is_not_a_firing_and_says_what_it_last_saw() {
        let dir = tmp();
        let ms = Arc::new(Monitors::new());
        ms.declare(
            "gone",
            scope(&dir),
            Watch::Path(dir.join("never")),
            None,
            "turn-1",
            Duration::from_millis(1),
            false,
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(5));
        let settled = ms.tick();
        assert_eq!(settled.len(), 1);
        let f = &settled[0].fired;
        assert!(!f.happened(), "an expiry learned nothing about the world");
        match f {
            Fired::Expired { last_seen, .. } => {
                assert!(last_seen.contains("does not exist"), "{last_seen}")
            }
            other => panic!("{other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_ttl_past_the_cap_is_refused_rather_than_clamped() {
        // Clamping would be a monitor that outlives what the caller asked for
        // while reporting the number they gave. The refusal names the cap.
        let dir = tmp();
        let ms = Arc::new(Monitors::new());
        let e = ms
            .declare(
                "forever",
                scope(&dir),
                Watch::Path(dir.join("x")),
                None,
                "turn-1",
                MAX_TTL + Duration::from_secs(1),
                false,
            )
            .unwrap_err();
        assert!(e.to_string().contains("past the cap"), "{e}");
        assert!(ms.list().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_monitor_dies_with_the_scope_that_owns_it_and_the_record_says_which() {
        // T24 requirement 1, from the other end: an owner is only an owner if its
        // end actually takes the thing it owns.
        let dir = tmp();
        let ms = Arc::new(Monitors::new());
        let owner = scope(&dir);
        ms.declare(
            "w",
            owner.clone(),
            Watch::Path(dir.join("x")),
            None,
            "turn-1",
            DEFAULT_TTL,
            false,
        )
        .unwrap();
        let retired = ms.retire_under(&owner);
        assert_eq!(retired.len(), 1);
        match &retired[0].fired {
            Fired::OwnerEnded { scope, .. } => assert_eq!(scope, &owner),
            other => panic!("{other:?}"),
        }
        assert!(
            ms.list().is_empty(),
            "a retired monitor is not still listed"
        );
        assert_eq!(ms.firings().len(), 1, "and it is still in the record");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_renewal_moves_the_deadline_and_a_listing_does_not() {
        let dir = tmp();
        let ms = Arc::new(Monitors::new());
        ms.declare(
            "r",
            scope(&dir),
            Watch::Path(dir.join("x")),
            None,
            "turn-1",
            Duration::from_secs(1),
            false,
        )
        .unwrap();
        let before = ms.list()[0].remaining().unwrap();
        // Listing it must not extend it: a monitor that lives as long as somebody
        // keeps looking at it has no bound at all.
        let _ = ms.list();
        assert!(ms.list()[0].remaining().unwrap() <= before);
        ms.renew("r", Duration::from_secs(600)).unwrap();
        assert!(ms.list()[0].remaining().unwrap() > Duration::from_secs(500));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_scope_watch_counts_absence_only_after_presence() {
        // The seat brief's rule, in the primitive that would otherwise get it
        // wrong: an empty cgroup at t=0 is a boot window, not a finish.
        let dir = tmp();
        let cg = dir.join("scope");
        std::fs::create_dir_all(&cg).unwrap();
        let ms = Arc::new(Monitors::new());
        ms.declare(
            "s",
            scope(&dir),
            Watch::Scope(scope(&cg)),
            None,
            "turn-1",
            DEFAULT_TTL,
            false,
        )
        .unwrap();
        // There is no `cgroup.events` here, so `populated_at` reads false — which
        // is exactly the "empty before startup" case. It must NOT fire.
        assert!(
            ms.tick().is_empty(),
            "an empty scope that was never seen populated is a boot window"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_continuous_path_monitor_fires_on_each_change_without_dying() {
        let dir = tmp();
        let ms = Arc::new(Monitors::new());
        let p = dir.join("f");
        ms.declare(
            "c",
            scope(&dir),
            Watch::Path(p.clone()),
            None,
            "turn-1",
            DEFAULT_TTL,
            true,
        )
        .unwrap();
        assert!(ms.tick().is_empty());
        std::fs::write(&p, b"one").unwrap();
        let f1 = ms.tick();
        assert_eq!(f1.len(), 1);
        assert!(matches!(&f1[0].fired, Fired::Fired { .. }));
        // A continuous monitor is persistent: it does not die between firings.
        assert_eq!(ms.list().len(), 1, "a continuous monitor does not die");
        std::fs::write(&p, b"two").unwrap();
        let f2 = ms.tick();
        assert_eq!(f2.len(), 1);
        assert!(matches!(&f2[0].fired, Fired::Fired { .. }));
        assert_eq!(ms.firings().len(), 2, "two firings, one monitor");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_timer_condition_fires_after_its_interval_and_rearms() {
        let dir = tmp();
        let ms = Arc::new(Monitors::new());
        let timer = TimerCondition::after(Duration::from_millis(10));
        ms.declare(
            "t",
            scope(&dir),
            Watch::Custom(CustomWatch(timer)),
            None,
            "turn-1",
            DEFAULT_TTL,
            true,
        )
        .unwrap();
        assert!(ms.tick().is_empty());
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(ms.tick().len(), 1);
        // Re-armed: no immediate re-fire, then fires again after another interval.
        assert!(ms.tick().is_empty());
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(ms.tick().len(), 1);
        assert_eq!(ms.firings().len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_command_condition_fires_on_any_exit_and_on_output_match() {
        let dir = tmp();
        let ms = Arc::new(Monitors::new());
        // Any exit: a command that exits non-zero still fires.
        let no = CommandCondition::any(vec!["false".into()]);
        ms.declare(
            "no",
            scope(&dir),
            Watch::Custom(CustomWatch(no)),
            None,
            "turn-1",
            DEFAULT_TTL,
            false,
        )
        .unwrap();
        let f = tick_until(&ms, Duration::from_secs(2));
        assert_eq!(f.len(), 1, "any exit fires, even non-zero");
        assert!(
            f[0].fired.word().contains("exited"),
            "the firing says the code: {}",
            f[0].fired.word()
        );

        // Output contains: fires only when the output matches. `true` prints
        // nothing, so a `marker` match never fires.
        let out = CommandCondition::containing(vec!["true".into()], "marker");
        ms.declare(
            "out",
            scope(&dir),
            Watch::Custom(CustomWatch(out)),
            None,
            "turn-1",
            DEFAULT_TTL,
            false,
        )
        .unwrap();
        assert!(ms.tick().is_empty(), "no match, no firing");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_bash_script_waits_for_a_marker_and_fires_when_it_appears() {
        let dir = tmp();
        let script = dir.join("wait.sh");
        let marker = dir.join("marker");
        std::fs::write(
            &script,
            "#!/bin/bash\n# Wait for the marker file to appear, then exit.\n\
             while [ ! -f \"$1\" ]; do sleep 0.01; done\n",
        )
        .unwrap();
        let ms = Arc::new(Monitors::new());
        // Run the script through `bash`; its exit is the condition. It blocks
        // until the marker exists, which is what a second process provides.
        let cond = CommandCondition::any(vec![
            "bash".into(),
            script.display().to_string(),
            marker.display().to_string(),
        ]);
        ms.declare(
            "w",
            scope(&dir),
            Watch::Custom(CustomWatch(cond)),
            None,
            "turn-1",
            DEFAULT_TTL,
            false,
        )
        .unwrap();
        // The script is blocked waiting: nothing has fired.
        assert!(
            tick_until(&ms, Duration::from_millis(60)).is_empty(),
            "nothing fires while the script waits"
        );
        // The second process (the test) makes the marker; the script exits and the
        // monitor fires.
        std::fs::write(&marker, b"x").unwrap();
        let f = tick_until(&ms, Duration::from_secs(2));
        assert_eq!(f.len(), 1, "the marker made the script exit");
        assert!(
            f[0].fired.word().contains("exited"),
            "the firing names the exit: {}",
            f[0].fired.word()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_channel_condition_fires_on_each_pushed_message() {
        let dir = tmp();
        let ms = Arc::new(Monitors::new());
        let (cond, tx) = ChannelCondition::channel();
        ms.declare(
            "ch",
            scope(&dir),
            Watch::Custom(CustomWatch(cond)),
            None,
            "turn-1",
            DEFAULT_TTL,
            true,
        )
        .unwrap();
        // The push path: the notifier pings the poller, which fires and settles the
        // wake seam. Wait on that seam rather than racing the poller thread.
        tx.send("first".into()).unwrap();
        let fired = ms.wait_for_any(0, Duration::from_secs(2));
        assert_eq!(fired.len(), 1, "the pushed message fires");
        assert!(
            matches!(&fired[0].fired, Fired::Fired { why, .. } if why == "first"),
            "{:?}",
            fired[0].fired
        );
        // Continuous: a second message is a second firing, from one monitor.
        tx.send("second".into()).unwrap();
        let again = ms.wait_for_any(1, Duration::from_secs(2));
        assert_eq!(again.len(), 1);
        assert_eq!(ms.firings().len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
