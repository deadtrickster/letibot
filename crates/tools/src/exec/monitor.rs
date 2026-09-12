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
//! [`Watch`] has four variants and **none of them can hold a string that is
//! matched against a process**. `until pgrep -f X` is not refused here; it is
//! unspellable, because there is no argument to put the `X` in.
//!
//! The evidence is not an argument about taste. On this box a process check
//! self-matched its own shell **seven times in one session** with the lesson in
//! memory and T21 open in `TODO.md`, and the seventh — `pkill -f
//! 'harnessd-rice.sock'` — killed a running command mid-flight, because the
//! string was in the shell's own command line. Prompting has now failed eight
//! times against this hazard. `docs/tool-design-brief.md` §2.4: *make the mistake
//! inexpressible, do not warn about it.*
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

use std::collections::BTreeMap;
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

/// What a monitor watches. **Four, and not one of them holds a pattern.**
///
/// Adding a fifth is a deliberate act and the bar is the one in this module's
/// docs: it keys on a handle, a path or a port, and a model cannot write a string
/// into it that gets matched against a process.
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
    state: Mutex<Option<Fired>>,
    probe: Probe,
    /// **Presence, before absence.** A [`Watch::Scope`] must see the cgroup hold
    /// something before an empty one means anything: an empty scope at t=0 is a
    /// boot window and not a finish, and this flag is the only thing that keeps
    /// the two apart. Unused by the other three probes, which have no such
    /// ambiguity — a path either changed or did not.
    seen_populated: AtomicBool,
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
    Path { path: PathBuf, baseline: PathFacts },
    Port { port: u16, want: PortState },
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
    /// Whether this monitor is still watching. `None` is watching; `Some` is one
    /// of the four endings and is final.
    pub fn settled(&self) -> Option<Fired> {
        self.state.lock().expect("monitor state").clone()
    }

    pub fn is_watching(&self) -> bool {
        self.state.lock().expect("monitor state").is_none()
    }

    pub fn ttl(&self) -> Duration {
        *self.ttl.lock().expect("monitor ttl")
    }

    /// How long before the TTL takes it, or `None` once it has settled.
    pub fn remaining(&self) -> Option<Duration> {
        if !self.is_watching() {
            return None;
        }
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

    fn settle(&self, f: Fired) -> bool {
        let mut s = self.state.lock().expect("monitor state");
        if s.is_some() {
            return false;
        }
        *s = Some(f);
        true
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
                .changed_from(baseline)
                .map(|w| format!("`{}`: {w}", path.display())),
            Probe::Port { port, want } => {
                let now = if super::host::port_is_listening(*port) {
                    PortState::Listening
                } else {
                    PortState::Closed
                };
                (now == *want).then(|| format!("loopback port {port} is {}", want.as_str()))
            }
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
            state: Mutex::new(None),
            probe,
            seen_populated: AtomicBool::new(false),
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
}

#[derive(Debug, Default)]
struct Registry {
    /// Ordered by name so a listing is stable across calls; an unstable listing
    /// re-renders differently every turn and costs the prefix cache.
    live: BTreeMap<String, Arc<Monitor>>,
    /// Settled monitors, kept so that "why did it fire" survives the firing. A
    /// record that is deleted at the moment it becomes interesting is not a
    /// record.
    history: Vec<Arc<Monitor>>,
    /// Whether the poller thread is running. Not a handle: the thread exits on
    /// its own when nothing is watching, and holding a `JoinHandle` for a thread
    /// nobody joins is bookkeeping that can only go stale.
    polling: bool,
}

impl Monitors {
    pub fn new() -> Monitors {
        Monitors::default()
    }

    /// Declare a watch. **Refuses rather than replacing** when the name is taken.
    ///
    /// `job` must be the live handle rather than an id, so that a monitor cannot
    /// be declared against a job that does not exist — see [`Probe`].
    pub fn declare(
        self: &Arc<Self>,
        name: &str,
        owner: ScopeId,
        watch: Watch,
        job: Option<Arc<Job>>,
        declared_by: &str,
        ttl: Duration,
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
                baseline: PathFacts::read(p),
            },
            (Watch::Port { port, want }, _) => Probe::Port {
                port: *port,
                want: *want,
            },
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

    /// Retire one by name.
    pub fn retire(&self, name: &str, by: &str) -> Option<Arc<Monitor>> {
        let mut reg = self.inner.lock().expect("monitors");
        let m = reg.live.remove(name)?;
        m.settle(Fired::Cancelled {
            by: by.to_string(),
            after: m.age(),
        });
        reg.history.push(Arc::clone(&m));
        drop(reg);
        self.settled.notify_all();
        Some(m)
    }

    /// **Retire every monitor a scope owned, because the scope ended.**
    ///
    /// Called from the reaping path rather than left to the poller, so the record
    /// lands at the moment the scope goes and says which scope took it. A monitor
    /// that merely stopped being polled would be a watcher that went quiet, and a
    /// watcher that goes quiet looks exactly like one whose condition never fired.
    pub fn retire_under(&self, scope: &ScopeId) -> Vec<Arc<Monitor>> {
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
                m.settle(Fired::OwnerEnded {
                    scope: scope.clone(),
                    after: m.age(),
                });
                reg.history.push(Arc::clone(&m));
                out.push(m);
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

    /// Everything that has settled, in the order it settled. The falsifier: a
    /// session with no monitors and a session whose monitors all expired without
    /// firing are different facts.
    pub fn history(&self) -> Vec<Arc<Monitor>> {
        self.inner.lock().expect("monitors").history.clone()
    }

    /// One monitor by name, live or settled. **Settled ones are still findable**:
    /// the moment a monitor becomes interesting is the moment it fires, and a
    /// lookup that only saw live ones would answer "no such monitor" about the
    /// one that just did its job.
    pub fn get(&self, name: &str) -> Option<Arc<Monitor>> {
        let reg = self.inner.lock().expect("monitors");
        reg.live
            .get(name)
            .cloned()
            .or_else(|| reg.history.iter().rev().find(|m| m.name == name).cloned())
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

    /// **The wake seam.** Block until a monitor settles, or until the deadline.
    ///
    /// This is what a loop calls between turns: it is the "wakes the loop when it
    /// fires" half of T24's sentence, and it is a `Condvar` rather than a poll so
    /// that a caller waiting on it costs nothing while nothing is happening.
    ///
    /// Returns the monitors that settled **during this call**, which is why it
    /// takes a `since`: a caller that asked twice would otherwise be handed the
    /// same firing twice and act on it twice.
    pub fn wait_for_any(&self, since: usize, deadline: Duration) -> Vec<Arc<Monitor>> {
        let reg = self.inner.lock().expect("monitors");
        let (reg, _) = self
            .settled
            .wait_timeout_while(reg, deadline, |r| r.history.len() <= since)
            .expect("monitors");
        reg.history.iter().skip(since).cloned().collect()
    }

    /// How many monitors have settled. The cursor [`Monitors::wait_for_any`] takes.
    pub fn settled_count(&self) -> usize {
        self.inner.lock().expect("monitors").history.len()
    }

    /// One pass over every live monitor. Exposed so a test does not have to sleep
    /// for the poller — a test that waits on a thread it cannot see is a test that
    /// is flaky on a loaded box.
    pub fn tick(&self) -> Vec<Arc<Monitor>> {
        let mut settled = Vec::new();
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
            let Some(f) = ending else { continue };
            if m.settle(f) {
                let mut reg = self.inner.lock().expect("monitors");
                reg.live.remove(&name);
                reg.history.push(Arc::clone(&m));
                settled.push(m);
            }
        }
        if !settled.is_empty() {
            self.settled.notify_all();
        }
        settled
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
                    std::thread::sleep(TICK);
                    let Some(me) = weak.upgrade() else { return };
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
            "letibot-mon-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
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
        )
        .unwrap();
        assert!(ms.tick().is_empty(), "nothing has changed yet");
        std::fs::write(&p, b"hello").unwrap();
        let fired = ms.tick();
        assert_eq!(fired.len(), 1);
        match fired[0].settled().unwrap() {
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
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(5));
        let settled = ms.tick();
        assert_eq!(settled.len(), 1);
        let f = settled[0].settled().unwrap();
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
        )
        .unwrap();
        let retired = ms.retire_under(&owner);
        assert_eq!(retired.len(), 1);
        match retired[0].settled().unwrap() {
            Fired::OwnerEnded { scope, .. } => assert_eq!(scope, owner),
            other => panic!("{other:?}"),
        }
        assert!(
            ms.list().is_empty(),
            "a retired monitor is not still listed"
        );
        assert_eq!(ms.history().len(), 1, "and it is still in the record");
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
}
