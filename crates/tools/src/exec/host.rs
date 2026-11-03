//! [`ProcessHost`]: the seam the tools reach the world through, and the host
//! implementation of it. Waiting takes a handle and never a pattern, so a loop
//! that could match its own waiter cannot be spelled ([`Waited`], [`super::predicate`]).

use std::io::Read;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime};

use super::ExecError;
use super::confine::{ConfinePlan, Confinement, Unconfined};
use super::jobs::{
    DEADLINE_KILL, Due, Job, JobId, JobState, Lifetime, MemoryCap, OutputFile, OutputSlice,
};
use super::monitor::Monitors;
use super::scope::{
    Cgroup2, EXIT_NOT_SCOPED, Migration, Reaped, Reaping, ScopeId, ScopeKind, ScopeTree,
    join_script,
};
use letibot_transcript::Backgrounding;

/// A process the harness manages and the model must not be allowed to reason
/// about by pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Protected {
    pub pid: u32,
    pub comm: String,
    pub cmdline: String,
    /// Why this pid is here, in the words the refusal quotes. A sentence rather than a category,
    /// because the refusal has to be self-correcting and "protected" is not a reason to act on.
    pub why: String,
    /// Whether waiting for this process to exit can never finish while the session
    /// runs. The model server serving the turn is the case that matters: a waiter
    /// on it is a deadlock the harness can see and the model cannot.
    pub outlives_the_turn: bool,
}

/// What a job was asked to be.
#[derive(Debug, Clone)]
pub struct SpawnRequest {
    /// The command text, exactly as the model wrote it.
    pub command: String,
    /// The name the caller gave this work, or `None` when nobody said one: the
    /// agent's own stated intent, never a parse of the command line, because the
    /// command is already on the row and a guessed name earns nothing ([`Job::slug`]).
    pub slug: Option<String>,
    /// Relative to the backend root.
    pub cwd: String,
    /// Which scope owns the process's lifetime.
    pub scope: ScopeKind,
    /// The name of an [`ScopeKind::Explicit`] scope. Ignored otherwise.
    pub scope_name: Option<String>,
    pub background: bool,
    /// Additions to the environment, in a fixed order so a run is reproducible.
    pub env: Vec<(String, String)>,
    /// Does this run meet the world the operator's own console gives a command?
    /// A property of who reads the bytes: a pty on all three descriptors, their
    /// interactive shell, their variables, and an answerable stdin ([`super::console`]).
    pub tty: bool,
    /// The memory cap the caller decided, in bytes, or `None` for no cap: the
    /// operator's own run. The host does not choose a number; it only makes the kernel
    /// hold the one the tool chose.
    pub memory: Option<u64>,
}

/// How a wait ended. Three outcomes and they are never conflated: a deadline reported as a
/// completion would make every wait a lie.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Waited {
    /// It happened: the job left `Running`, or the scope emptied after having been
    /// seen populated.
    Happened {
        state: Option<JobState>,
        /// How long the wait actually took.
        took: Duration,
    },
    /// The deadline passed quietly and the thing is still running. Carries the
    /// progress facts, not a liveness bit.
    Deadline {
        took: Duration,
        produced: u64,
        since_last_output: Option<Duration>,
    },
    /// Presence was never observed, so the absence proves nothing. A boot window,
    /// not a finish.
    NeverStarted { waited_for_presence: Duration },
}

/// A job as a listing shows it. A snapshot, not a handle: a `job_list` that
/// handed out live references would let a rendering block on a mutex a reader
/// thread holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobView {
    /// The memory cap the job runs under and what the kernel counted: the peak so
    /// far while it runs, and as recorded at the settle once it has ended.
    pub memory_cap: MemoryCap,
    pub memory: Option<super::scope::MemoryUse>,
    pub id: JobId,
    pub command: String,
    /// The name the caller gave this job, or `None` when nobody said one: the stated
    /// intent, never a derivation from the command. On the view because a listing and a
    /// tool resolving a name back to a handle must read the same string.
    pub slug: Option<String>,
    /// The job's own cgroup: what `job_kill` ends.
    pub scope: ScopeId,
    /// The scope that reaps it if nobody does: the turn, the session, or a named
    /// explicit scope.
    pub owner: ScopeId,
    pub cwd: String,
    pub pid: u32,
    /// How this job came to be in the background, or `None` if it never was.
    /// Not a bool: "the model asked" and "the runtime moved it" are the
    /// difference between a listing the model recognises and one it does not.
    pub background: Option<Backgrounding>,
    pub state: JobState,
    pub elapsed: Duration,
    /// How long the job ran, from its own stamps, or `None` while it is running. This is the
    /// number a settlement reports; `elapsed` is a live clock and keeps counting.
    pub ran_for: Option<Duration>,
    pub produced: u64,
    pub since_last_output: Option<Duration>,
    /// Where the whole of this job's output is also going, for every run nothing bounds,
    /// and `None` for a run that still has one. The window above is bounded, this is not, and
    /// a reader told only about the window loses a long build's first half ([`OutputFile`]).
    pub output_file: Option<OutputFile>,
}

/// What a job is called: the caller's name and the id, or just the id when nobody
/// named one. A free function as well as a method because a settlement outlives the view
/// it was read from, and the name is flattened to one line however it was typed.
pub fn job_label(slug: Option<&str>, id: &str) -> String {
    match slug {
        Some(s) if !s.trim().is_empty() => {
            let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
            format!("{} · {}", flat, id)
        }
        _ => id.to_string(),
    }
}

impl JobView {
    /// One clause about this job's memory, or `None` unless there is something to say:
    /// it was killed at its cap, came within a tenth of it, or ran uncapped against an ask.
    pub fn memory_clause(&self) -> Option<String> {
        match (&self.memory_cap, &self.memory) {
            (MemoryCap::Uncapped { asked, why }, _) => Some(format!(
                "UNCAPPED ({} was asked for: {why})",
                super::scope::human_bytes(*asked)
            )),
            (_, Some(u)) if u.killed_at_cap() => {
                Some(format!("{} — KILLED at the cap", u.clause()))
            }
            // A job that ended well inside its cap says nothing about memory: the peak on
            // every row was a number to size work by, which the model is not given.
            (_, Some(u)) if u.near_its_cap() => Some(format!("{} — near its cap", u.clause())),
            (_, Some(_)) | (MemoryCap::Capped(_), None) | (MemoryCap::NotAsked, None) => None,
        }
    }

    /// What this job is called: [`job_label`], which is this method's body. Four
    /// readers need the same string, and a settlement has the name and the id but no
    /// view to hang a method on.
    pub fn label(&self) -> String {
        job_label(self.slug.as_deref(), &self.id.0)
    }
}

/// One job moved from one scope's ownership to another's, with the measurement
/// that says whether the move is true of every process it claims.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Promotion {
    pub job: JobId,
    /// The command, kept here because a promotion record read an hour later has
    /// to identify something. `/proc` will not still say.
    pub command: String,
    /// The name the caller gave this job, or `None` when nobody said one, for the same
    /// reason the command is here: a promotion read back an hour later is a sentence about a
    /// job, and the name is certain only at the promotion.
    pub slug: Option<String>,
    /// The scope that owned its lifetime before.
    pub from: ScopeId,
    /// The scope that owns it now.
    pub to: ScopeId,
    /// How long it ran in the foreground first. The number the model is shown.
    pub ran_for: Duration,
    pub how: Backgrounding,
    /// `None` when there was nothing to move: a job that had already finished. Distinguished from
    /// a migration that moved zero of three.
    pub migration: Option<Migration>,
    pub at: SystemTime,
    pub note: Option<String>,
    /// The file the run's output keeps going into from here on, when this session has a
    /// scratch directory to put it in. `None` where there is nowhere to spill, and then the
    /// run's output is exactly what it was before: the window.
    pub output_file: Option<OutputFile>,
}

impl Promotion {
    /// Is every process this job still has now owned by the new scope?
    ///
    /// A promotion with nothing to move did nothing, and [`Promotion::note`] says so.
    pub fn complete(&self) -> bool {
        self.migration.as_ref().is_some_and(|m| m.complete())
    }

    /// The one line a listing draws, in the spelling every other sentence uses
    /// ([`job_label`]). The record's identity is still the handle.
    pub fn summary(&self) -> String {
        let mut s = format!(
            "`{}` promoted from {} to {} after {:.1}s — {}",
            job_label(self.slug.as_deref(), &self.job.0),
            self.from,
            self.to,
            self.ran_for.as_secs_f32(),
            self.how.phrasing()
        );
        match &self.migration {
            Some(m) => s.push_str(&format!("; {}", m.summary())),
            None => s.push_str("; nothing was moved"),
        }
        if let Some(f) = &self.output_file {
            s.push_str(&format!("; its output keeps going to {}", f.path));
        }
        if let Some(n) = &self.note {
            s.push_str(&format!(" — {n}"));
        }
        s
    }
}

/// The seam. Host today; a firecode guest owns its processes by owning the VM,
/// and nothing above this line changes when that is what is underneath.
pub trait ProcessHost: Send + Sync {
    /// One line for `EXPLAIN`. Must say what it does not confine, because a
    /// disclosure that only lists the guarantees reads as a claim about the rest.
    fn describe(&self) -> String;

    fn spawn(&self, req: &SpawnRequest) -> Result<JobId, ExecError>;

    /// Arm a run's deadline, so that something other than the run enforces it. Required
    /// rather than defaulted: a default that did nothing would leave the timeout silently the
    /// caller's problem, which is the shape this method exists to end.
    fn arm_deadline(&self, job: &JobId, after: Duration);

    /// Block on a handle, never a pattern. `job` waits for one job to leave
    /// `Running`; `scope` waits for a cgroup to empty, and applies the
    /// presence-before-absence rule itself.
    fn wait_job(&self, job: &JobId, deadline: Duration) -> Result<Waited, ExecError>;
    fn wait_scope(&self, scope: &ScopeId, deadline: Duration) -> Result<Waited, ExecError>;

    fn job(&self, job: &JobId) -> Option<JobView>;
    fn jobs(&self) -> Vec<JobView>;
    fn output(&self, job: &JobId, from: u64, limit: usize) -> Result<OutputSlice, ExecError>;

    /// Kill one job by killing its cgroup, and record it.
    fn kill_job(&self, job: &JobId) -> Result<Reaping, ExecError> {
        self.kill_job_as(job, "job_kill")
    }

    /// The same, with who ended it as an argument: what [`JobState::Killed`] carries and
    /// every reader is shown. A sentence about the cause, not a label, because *the model
    /// asked* and *its deadline passed* are different things to have happened.
    fn kill_job_as(&self, job: &JobId, by: &str) -> Result<Reaping, ExecError>;

    /// The jobs this host holds that will NOT go with it: an `explicit` scope outlives the session
    /// that started it, so a session that ends hands those on instead of ending them.
    fn survivors(&self) -> Vec<Adopted> {
        Vec::new()
    }

    /// Stop listing a job, on the side that gave it up: two tables claiming one process is two
    /// owners, and two owners is the leak.
    fn forget(&self, job: &JobId) {
        let _ = job;
    }

    /// Take a job this backend did not start, and make it one of its own. A host that cannot
    /// identify a pid must answer *I cannot* rather than take back something it cannot end.
    fn adopt(&self, a: &Adopted) -> Result<JobId, ExecError> {
        Err(ExecError::Spawn(format!(
            "this backend cannot adopt `{}`: the record outlived the process that started the \
             job, and only a host that can identify a pid can take one back",
            a.handle
        )))
    }

    /// End every run this host still has, and say why. The daemon's shutdown is the caller: a
    /// stop that arrives while a run holds the worker must end that run rather than wait out its
    /// deadline. One reap record per run ended, so an empty answer says nothing was running.
    fn end_running(&self, by: &str) -> Vec<Reaping> {
        let running: Vec<JobId> = self
            .jobs()
            .into_iter()
            .filter(|v| v.state.is_running())
            .map(|v| v.id)
            .collect();
        running
            .iter()
            .filter_map(|id| self.kill_job_as(id, by).ok())
            .collect()
    }
    /// End a scope: kill everything under it, and record it.
    fn end_scope(&self, scope: &ScopeId) -> Result<Reaping, ExecError>;

    /// Move a running job into a scope that outlives the turn, and record what moved: the
    /// one verb behind the runtime's threshold and a person promoting from a head. A
    /// [`Promotion`] and never a bool, because a process that did not move makes that false.
    ///
    /// The frame that would let a head ask for this is not on the wire: a new `ClientFrame`
    /// variant is a `PROTOCOL_VERSION` bump that costs the running daemon its live session,
    /// and nothing in production can send it today.
    ///
    /// ```text
    /// ClientFrame::PromoteJob {
    ///     client_request_id: String,   // as every other acting frame carries
    ///     expected_seq: u64,           // as Prompt and Interrupt do
    ///     job: String,                 // the job id, from ToolEvent::Progress
    ///     identity: String,            // WHO — this becomes Backgrounding::Operator
    /// }
    /// ```
    fn promote(
        &self,
        job: &JobId,
        to: ScopeKind,
        name: Option<&str>,
        how: Backgrounding,
    ) -> Result<Promotion, ExecError>;

    /// Every promotion this session made. The same falsifier as
    /// [`ProcessHost::reap_log`]: a promotion that moved nothing and a job that was already
    /// where it needed to be are different facts.
    fn promotions(&self) -> Vec<Promotion>;

    /// Every scope open right now, so a listing can show what is running and for whom.
    /// An invisible watcher is an unreapable one.
    fn scopes(&self) -> Vec<ScopeId>;

    /// What every ended scope killed. The falsifier: a reaper whose zero is
    /// unfalsifiable is the empty-haystack bug one layer up.
    fn reap_log(&self) -> Vec<Reaping>;

    /// This session's monitors, or `None` on a host with no monitor mechanism. `None` is an
    /// honest absence and not an empty list: *there are no monitors* and *this host cannot
    /// watch anything* are different answers, and a tool must not render the second as the first.
    fn monitors(&self) -> Option<&Arc<Monitors>> {
        None
    }

    /// The workspace root, for a tool that must turn a relative path into a real one.
    /// `None` on a host with no opinion about where it is, which is not a root of `/`.
    fn workspace(&self) -> Option<&std::path::Path> {
        None
    }

    /// Where the whole of a job's output lives, as the HOST spells it, or `None` when it
    /// lives nowhere but this process's window ([`JobView::output_file`] is the other half: a
    /// path a reader here can open). One caller: [`crate::fleet`], for a fanout branch's record.
    fn job_log(&self, job: &JobId) -> Option<String> {
        let _ = job;
        None
    }

    /// The live handle for a job, so a monitor can be declared against the job itself rather
    /// than against an id it would have to look up every tick. An id can be re-used by a
    /// different host; a handle cannot.
    fn job_handle(&self, job: &JobId) -> Option<Arc<Job>> {
        let _ = job;
        None
    }

    /// The processes this job has right now, by pid, when the host can say. One reader:
    /// [`super::ask`], whose question is asked of the run's processes and which therefore has
    /// to name them. An empty list means *this host cannot say*, not *nothing is running*.
    fn job_pids(&self, job: &JobId) -> Vec<u32> {
        let _ = job;
        Vec::new()
    }

    /// The pids the harness manages, which the model cannot see.
    fn protected(&self) -> Vec<Protected>;

    /// The scope a foreground command belongs to, opened on demand.
    fn scope_for(&self, kind: ScopeKind, name: Option<&str>) -> Result<ScopeId, ExecError>;

    /// What bounds the view, for a tool that has to explain its own result. `None` means
    /// this host has no opinion, which is neither [`super::confine::Unconfined`] (nobody asked)
    /// nor [`super::confine::NoConfinement`] (asked for and missing).
    fn confinement_summary(&self) -> Option<String> {
        None
    }

    /// What a command could not see, read off the boundary: the sentences the model is
    /// given, the same paths for a caller that can act on them, and whether the launcher itself
    /// failed. Empty for a host with no boundary, which is not the same as nothing being absent.
    fn absence_notes(&self, _output: &str) -> Vec<String> {
        Vec::new()
    }

    fn outside_paths(&self, _output: &str) -> Vec<std::path::PathBuf> {
        Vec::new()
    }

    fn launcher_failure(&self, _output: &str) -> Option<String> {
        None
    }

    /// Hold a handle that stands for work running somewhere else ([`Aggregate`]). The
    /// default is a refusal and not a no-op: a backend that answered `Ok` and then settled
    /// nothing would leave a handle a model waits on for ever.
    fn hold(&self, place: &JobId, agg: Arc<dyn Aggregate>) -> Result<(), ExecError> {
        let _ = (place, agg);
        Err(ExecError::Spawn(
            "this backend cannot hold a job of jobs: it has no table of its own for one handle \
             to live in, so a job whose settlement waits on N others would settle nowhere"
                .to_string(),
        ))
    }

    /// What a settling job wants to say for itself, beyond its state and its numbers.
    /// `None` for every ordinary job, because a settlement carries its ending and not its
    /// output; an [`Aggregate`]'s result IS its output, so it answers with that text.
    fn settlement_note(&self, _job: &JobId) -> Option<String> {
        None
    }

    /// A name a job of jobs may answer to, made unique against every name this host has
    /// handed out ([`JobView::slug`]), so two fanouts in one session cannot be reached by one
    /// word. The default hands the name back unchanged, for a backend with no such record.
    fn claim_name(&self, asked: &str) -> String {
        asked.to_string()
    }
}

/// A job of jobs: one handle in the session's own table that stands for work running on
/// other machines and settles when that work does. `state` is the truth once it is not
/// `Running`, and `output` is an append-only journal, so an offset stays meaningful.
pub trait Aggregate: Send + Sync {
    /// The row's command column: what this job of jobs *is*, in one line. There is no single
    /// command line, and inventing one from the first branch would be a claim about all of them.
    fn command(&self) -> String;
    /// The name the caller would use for it, if it has one: the same slug a `bash` job may
    /// carry, so the tools take a word as well as a handle.
    fn slug(&self) -> Option<String>;
    fn state(&self) -> JobState;
    /// How long it has been going, live, or how long it ran.
    fn elapsed(&self) -> Duration;
    /// The same, settled: the number a settlement reports.
    fn ran_for(&self) -> Option<Duration>;
    fn produced(&self) -> u64;
    /// The last time a branch said something, so a wait can report progress and not a bit.
    fn since_last_output(&self) -> Option<Duration>;
    fn output(&self, from: u64, limit: usize) -> OutputSlice;
    /// Block until it settles, at most `within`. `None` is *still running*, never a guess.
    fn wait(&self, within: Duration) -> Option<JobState>;
    /// End every branch, on its own host, through its own process group, and answer what
    /// was there before the kill, so the record is a measurement and not a claim.
    fn kill(&self, by: &str) -> Vec<Reaped>;
    /// The table, for the settlement that wakes the model: the same text the blocking form
    /// answers with. Empty while it is still running.
    fn report(&self) -> String;
}

/// The run's deadline, and the thread that enforces it. It gets a thread of its own and
/// ends the run's cgroup, so it reaches a root `sudo` and everything the run forked even when
/// the run's own thread is stuck; the loop's own check stays as the fast path.
pub struct Deadlines {
    /// Armed deadlines, earliest first by construction of the wait below.
    inner: Mutex<Vec<Due>>,
    /// Rung when a deadline is armed or the list changes, so the watchdog re-reads it
    /// rather than polling.
    armed: Condvar,
    /// What the watchdog did, so a caller can read back *the deadline ended this run*, the same
    /// reason `reaps` exists beside it.
    fired: Mutex<Vec<DeadlineFired>>,
    /// The host is gone, so the watchdog is too: set by [`Deadlines::close`] from
    /// `HostProcesses`' `Drop`, under `inner`'s lock so the watchdog cannot miss it between its
    /// check and its wait. Without it the thread held its own `Arc` and never ended.
    closed: std::sync::atomic::AtomicBool,
}

/// One firing, as a record rather than a side effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeadlineFired {
    pub job: JobId,
    /// How long the run had been alive when its deadline ended it.
    pub after: Duration,
    /// The cgroup reap, which is the same record `job_kill` leaves.
    pub reaping: Reaping,
}

impl std::fmt::Debug for Deadlines {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let n = self.inner.lock().map(|g| g.len()).unwrap_or(0);
        let fired = self.fired.lock().map(|g| g.len()).unwrap_or(0);
        f.debug_struct("Deadlines")
            .field("armed", &n)
            .field("fired", &fired)
            .finish()
    }
}

impl Deadlines {
    /// Keep the record of what this watchdog did, bounded the way the host's other records are.
    fn record(&self, f: DeadlineFired) {
        let mut fired = self.fired.lock().unwrap_or_else(|e| e.into_inner());
        fired.push(f);
        bound_records(&mut fired, RECORD_LIMIT);
    }

    /// An empty set, with the watchdog already running.
    fn new(tree: Arc<dyn ScopeTree>) -> Arc<Deadlines> {
        let d = Arc::new(Deadlines {
            inner: Mutex::new(Vec::new()),
            armed: Condvar::new(),
            fired: Mutex::new(Vec::new()),
            closed: std::sync::atomic::AtomicBool::new(false),
        });
        let watch = Arc::clone(&d);
        // A failure to spawn is not swallowed. A host whose watchdog could not start is a
        // host with no deadline at all, so it is said on stderr where a daemon's operator
        // already reads, and the loop's own check remains the only enforcement.
        if let Err(e) = std::thread::Builder::new()
            .name("letibot-deadline".into())
            .spawn(move || watch.run(tree))
        {
            eprintln!(
                "letibot: the deadline watchdog did not start ({e}); a run that outlives its \
                 timeout is ended by the tool's own wait loop and by nothing else"
            );
        }
        d
    }

    /// Arm one run's deadline.
    fn arm(&self, job: Arc<Job>, after: Duration) {
        let at = Instant::now() + after;
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        // A second arm for one job replaces the first: a run has one deadline, and two
        // entries for it would be two firings for one fact.
        g.retain(|d| d.job.id != job.id);
        g.push(Due { at, job });
        drop(g);
        self.armed.notify_all();
    }

    /// End the watchdog: its host is being dropped, so nothing it could fire for has an
    /// owner left. The flag is set under the list's lock and the condvar rung after, so a
    /// watchdog asleep in either wait wakes, reads it and returns, dropping its `Arc`.
    fn close(&self) {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        self.closed.store(true, std::sync::atomic::Ordering::SeqCst);
        drop(g);
        self.armed.notify_all();
    }

    /// What has fired, oldest first. The read-back half of the record.
    pub fn fired(&self) -> Vec<DeadlineFired> {
        self.fired.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The watchdog. Sleeps until the earliest armed deadline, then ends that run's cgroup
    /// on this thread, which is not the run's. Returns when [`Deadlines::close`] says its host
    /// is gone.
    fn run(&self, tree: Arc<dyn ScopeTree>) {
        loop {
            let due: Vec<Due> = {
                let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                loop {
                    if self.closed.load(std::sync::atomic::Ordering::SeqCst) {
                        return;
                    }
                    let now = Instant::now();
                    match g.iter().map(|d| d.at).min() {
                        // Nothing armed: sleep until something is.
                        None => {
                            g = self.armed.wait(g).unwrap_or_else(|e| e.into_inner());
                        }
                        // The earliest is still ahead: sleep exactly that long and no longer, as
                        // a shorter deadline armed meanwhile rings the condvar.
                        Some(t) if t > now => {
                            let (g2, _) = self
                                .armed
                                .wait_timeout(g, t - now)
                                .unwrap_or_else(|e| e.into_inner());
                            g = g2;
                        }
                        // At least one is due.
                        Some(_) => break,
                    }
                }
                let now = Instant::now();
                let (due, keep): (Vec<Due>, Vec<Due>) = g.drain(..).partition(|d| d.at <= now);
                *g = keep;
                due
            };
            for d in due {
                // The run is ended the same way `job_kill` ends one, and only while it is
                // still running: a job that exited a moment before its deadline finished, and
                // reaping its cgroup would report an empty scope as the deadline's doing.
                if !d.job.state().is_running() {
                    continue;
                }
                let after = d.job.elapsed();
                d.job.settle(JobState::Killed {
                    by: DEADLINE_KILL.to_string(),
                });
                let reaping = tree.end(&d.job.lifetime().scope);
                self.record(DeadlineFired {
                    job: d.job.id.clone(),
                    after,
                    reaping,
                });
            }
        }
    }
}

/// The host implementation: real processes, real cgroups.
pub struct HostProcesses {
    tree: Arc<dyn ScopeTree>,
    /// What bounds the process's view, as against its lifetime. Not an `Option`: the three
    /// answers ([`super::confine::Bwrap`], `NoConfinement`, [`Unconfined`]) are genuinely
    /// different. Behind a lock because a grant replaces it, by re-probing.
    confine: std::sync::RwLock<Box<dyn Confinement>>,
    root: PathBuf,
    /// The turn and session scopes, opened lazily and reused.
    session: Mutex<Option<ScopeId>>,
    turn: Mutex<Option<ScopeId>>,
    /// Every job this host ever ran, in spawn order, which is the order `jobs()` lists.
    jobs: Mutex<Vec<Arc<Job>>>,
    /// The same jobs, by id. `find` (every wait, output read, kill, deadline arm and pane
    /// read) would otherwise scan `jobs`, which only grows. Written beside `jobs` at the one
    /// place a job is added ([`HostProcesses::spawn`]).
    by_id: Mutex<std::collections::HashMap<JobId, Arc<Job>>>,
    /// Every slug this host has handed out, so the next one is unique. A record of its own
    /// rather than a scan of `jobs`, because a spawn decides its slug before it pushes it and
    /// two spawns can be in flight at once. A name is claimed once and never released.
    claimed_slugs: Mutex<Vec<String>>,
    /// The deadlines, and the thread that enforces them. See [`Deadlines`].
    deadlines: Arc<Deadlines>,
    reaps: Mutex<Vec<Reaping>>,
    /// Every promotion, for the same reason `reaps` exists: a lifetime that
    /// changed under the model is a fact somebody has to be able to read back.
    promotions: Mutex<Vec<Promotion>>,
    /// The `PATH` this host was constructed with, set explicitly on every spawn after
    /// `env_clear()`, so a bare name resolves through a `PATH` fixed at seat time rather than
    /// whatever the environment carries when the command runs.
    pinned_path: String,
    /// What every command of this session carries besides `PATH`: the daemon's socket and
    /// this session's id (so a helper the command runs can find its way back), `SUDO_ASKPASS`
    /// and `HOME`. The request's own pairs come after and win; not the process environment.
    standing_env: Mutex<Vec<(String, String)>>,
    /// A directory put in FRONT of the pinned `PATH`: the daemon's shims, so a
    /// `sudo` the session runs is the one that asks the head instead of a tty.
    path_prefix: Mutex<Option<String>>,
    /// The monitors this session declared. On the host, not beside it, because a monitor's
    /// owner is one of this host's scopes and the reaping path has to take its watchers with it.
    monitors: Arc<Monitors>,
    protected: Mutex<Vec<Protected>>,
    /// Bytes retained per job. Beyond this the ring drops from the front and says so
    /// ([`super::jobs::OutputSlice::denominator`]).
    capture_bytes: usize,
    /// Where a run with no deadline's output keeps going: the session's scratch directory.
    /// Behind a lock because the daemon sets it after the host is built and the host is shared
    /// with every tool. `None` on a host nobody wired one to, which spills nothing.
    scratch: Mutex<Option<PathBuf>>,
    /// The shell a command is handed to.
    shell: Vec<String>,
    /// The shell the OPERATOR's own command is handed to, which is theirs and interactive
    /// ([`super::console::shell`]). Only a run whose [`SpawnRequest::tty`] is set reaches it. A
    /// field for the same reason `shell` is: a host with no shell has no console shell either.
    console_shell: Vec<String>,
    /// The handles that stand for work running elsewhere ([`Aggregate`]), by id. A table of
    /// its own rather than a field on [`Job`], because a job of jobs has no process, no cgroup
    /// and no waiter thread; the two tables are read together, which is why `jobs()` lists both.
    aggregates: Mutex<std::collections::HashMap<JobId, Arc<dyn Aggregate>>>,
    /// The job scopes a settled job left a process in, so the next spawn can look at them again:
    /// the waiter thread returns after its one look, and nothing else revisits a job's scope.
    kept_scopes: Arc<Mutex<Vec<ScopeId>>>,
    /// What this host calls its session scope. Unique per host, not the constant `s`: two
    /// sessions in one daemon share a cgroup root, so a fixed name would put both sessions'
    /// processes in one cgroup and ending either would reap the other's jobs.
    session_name: String,
}

/// Distinguishes the scopes two sessions in one process open.
static SCOPE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn unique(prefix: &str) -> String {
    format!(
        "{prefix}{}",
        SCOPE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

impl std::fmt::Debug for HostProcesses {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostProcesses")
            .field("scopes", &self.tree.describe())
            .field("confinement", &self.confinement_describe())
            .field("jobs", &self.jobs.lock().map(|j| j.len()).unwrap_or(0))
            .finish()
    }
}

/// 8 MiB per job. Large enough that an ordinary build never drops a byte, small
/// enough that a runaway `yes` cannot take the box.
pub const DEFAULT_CAPTURE_BYTES: usize = 8 * 1024 * 1024;

/// How much SETTLED output one host keeps, across all its jobs: 64 MiB, eight full captures.
/// The capture of a settled job has done its work, so past this budget the oldest are released
/// ([`super::jobs::Capture::release`]) and the row and its counts stay. A running job keeps its.
pub const SETTLED_CAPTURE_BUDGET: usize = 64 * 1024 * 1024;

/// **How many records of what a host did one host keeps**: every reaping, every promotion and every
/// deadline it fired are a small record a caller reads back, not a history, and a daemon that has
/// been up for a month must not carry every kill it ever made.
pub const RECORD_LIMIT: usize = 512;

/// Keep `records` to `limit`, dropping the OLDEST first: the newest is the one a caller asking *what
/// just happened* wants, and the front is where the oldest are.
fn bound_records<T>(records: &mut Vec<T>, limit: usize) {
    if records.len() > limit {
        let excess = records.len() - limit;
        records.drain(..excess);
    }
}

/// How many FINISHED jobs one host keeps: a row is about 1.1 KB, nothing else ever removed one, and a
/// session's table grew with every `bash` it ran for as long as it lived. The newest are the ones a
/// model pages back through, and a running job is never dropped however many there are.
pub const KEPT_FINISHED_JOBS: usize = 32;

/// How long a finished job is kept whatever the window says. A watcher polls its settlement every
/// five seconds, and a row that went before the poll is a completion nobody was told about.
pub const FINISHED_JOB_GRACE: Duration = Duration::from_secs(120);

/// A job this host did not start: a restarted daemon's record of one, or a peer's, as this layer
/// needs it. See [`HostProcesses::adopt`] and [`hand_over`].
#[derive(Debug, Clone)]
pub struct Adopted {
    pub handle: String,
    pub command: String,
    pub slug: Option<String>,
    pub cwd: String,
    pub pid: u32,
    /// What makes a reused pid not this process: `start:<n>` on this box, which is what the
    /// process record holds and [`super::procs`] answers.
    pub identity: String,
    /// The owner scope as the record spells it: `session.<name>`, `turn.<name>`, `explicit.<name>`.
    pub scope: String,
    /// The directory the job's own scope really is, when the caller still holds it: `None` from a
    /// daemon's record, whose cgroup died with its process. A name alone re-derives a directory on
    /// the HEIR's root, where its `job_kill` would end nothing.
    pub scope_dir: Option<String>,
    /// The reaping scope's directory, for the same reason: one opened by the giver is not under the
    /// heir's root, and opening it there would give the job a second owner.
    pub owner_dir: Option<String>,
}

/// One job moved from the host that held it to another. A caller records these rather than a
/// count, because a hand-over that moved nothing and one that moved two are different facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handover {
    pub job: JobId,
    pub label: String,
    /// `None` when it moved; the refusal otherwise.
    pub refused: Option<String>,
}

/// Give `to` every job `from` would otherwise leave running when it goes: the ones in an
/// `explicit` scope, which outlives the session by design and has no owner left once that session
/// is a subagent's. It travels as [`Adopted`], so `job_list` and `job_kill` already work there.
pub fn hand_over(from: &dyn ProcessHost, to: &dyn ProcessHost) -> Vec<Handover> {
    // One host is not two: `adopt` answers the id it already holds, and the caller then forgets
    // the only record there is - so a hand-over to itself loses a process that is still running.
    let itself = std::ptr::eq(from, to);
    from.survivors()
        .into_iter()
        .map(|a| {
            let label = job_label(a.slug.as_deref(), &a.handle);
            let job = JobId(a.handle.clone());
            if itself {
                return Handover {
                    job,
                    label,
                    refused: Some(
                        "this host is both the giver and the heir: adopting here would move \
                         nothing and forget the only record of it, so it was left where it is"
                            .to_string(),
                    ),
                };
            }
            match to.adopt(&a) {
                Ok(_) => {
                    from.forget(&job);
                    Handover {
                        job,
                        label,
                        refused: None,
                    }
                }
                Err(why) => Handover {
                    job,
                    label,
                    refused: Some(why.to_string()),
                },
            }
        })
        .collect()
}

/// A record's scope spelling back into the two things a cgroup needs, with the conservative
/// answer for a word this build does not know: the session, which is where a background job lives.
/// A guess that widened a job's lifetime would keep it alive past its reaper.
fn scope_kind_of(scope: &str) -> (super::scope::ScopeKind, String) {
    use super::scope::ScopeKind;
    match scope.split_once('.') {
        Some(("turn", n)) => (ScopeKind::Turn, n.to_string()),
        Some(("explicit", n)) => (ScopeKind::Explicit, n.to_string()),
        Some(("session", n)) => (ScopeKind::Session, n.to_string()),
        _ => (ScopeKind::Session, scope.to_string()),
    }
}

/// Release a job's own scope once its leader has gone, and say whether it went. Only an EMPTY one
/// goes: a scope still holding a process is one the command left behind and somebody's live work,
/// and `end` would kill it. Left alone, each of these is a cgroup directory per command a session
/// ever ran - which is why the caller remembers the ones that stayed.
fn release_settled_scope(tree: &dyn ScopeTree, scope: &ScopeId) -> bool {
    if !tree.members(scope).is_ok_and(|m| m.is_empty()) {
        return false;
    }
    let _ = tree.end(scope);
    true
}

/// The same look, remembering a scope that could not be released: the last process in it can leave
/// long after its leader, and the only other moment this host touches the tree is a spawn.
fn settle_scope(tree: &dyn ScopeTree, kept: &Mutex<Vec<ScopeId>>, scope: &ScopeId) {
    if release_settled_scope(tree, scope) {
        return;
    }
    kept.lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(scope.clone());
}

impl HostProcesses {
    /// Hold settled output to `budget` bytes, releasing the oldest first ([`SETTLED_CAPTURE_BUDGET`]).
    /// Called at each spawn, the only moment the set can have grown past a bound. Newest first, so
    /// the output a model is most likely to page through again is the output still held.
    fn release_settled_captures(&self, budget: usize) {
        let jobs: Vec<Arc<Job>> = self.jobs.lock().expect("jobs").clone();
        let mut held = 0usize;
        for job in jobs.iter().rev() {
            if job.state().is_running() {
                continue;
            }
            let mut cap = job.capture.lock().expect("capture");
            let here = cap.retained();
            if held + here > budget {
                cap.release();
            } else {
                held += here;
            }
        }
    }

    /// The scopes [`settle_scope`] had to leave alone, looked at again: a process can leave one long
    /// after the leader that started it, and a spawn is when this host already does this housekeeping.
    fn release_kept_scopes(&self) {
        let mut kept = self.kept_scopes.lock().unwrap_or_else(|e| e.into_inner());
        kept.retain(|scope| !release_settled_scope(self.tree.as_ref(), scope));
    }

    /// Drop the finished jobs past a window of `keep`, oldest first, and any finished one settled
    /// less than `grace` before `now`. Both tables lose the row together, or `find` answers about a
    /// row `jobs()` has not got. The clock is a parameter so a test can age a finish without sleeping.
    fn prune_finished(&self, keep: usize, grace: Duration, now: SystemTime) -> usize {
        let aged = |job: &Arc<Job>| {
            !job.state().is_running()
                && job
                    .settled_at()
                    .is_some_and(|at| now.duration_since(at).is_ok_and(|d| d >= grace))
        };
        let mut jobs = self.jobs.lock().expect("jobs");
        // Newest first, because what stays is the window the model is most likely to read again.
        let mut kept = 0usize;
        let mut doomed: Vec<JobId> = Vec::new();
        for job in jobs.iter().rev() {
            if !aged(job) {
                continue;
            }
            kept += 1;
            if kept > keep {
                doomed.push(job.id.clone());
            }
        }
        if doomed.is_empty() {
            return 0;
        }
        jobs.retain(|job| !doomed.contains(&job.id));
        let mut by_id = self.by_id.lock().expect("jobs by id");
        for id in &doomed {
            by_id.remove(id);
        }
        doomed.len()
    }

    /// A host with a real cgroup tree, rooted under `root` on the filesystem. Returns the
    /// probe's own error when there is no cgroup v2 to delegate, rather than falling back to an
    /// unowned spawn: a process with no owner is the leak, so the substrate refuses to exist.
    pub fn new(root: impl Into<PathBuf>) -> Result<HostProcesses, ExecError> {
        Ok(Self::with_tree(root, super::scope::host_tree()?))
    }

    /// The confined constructor: a real cgroup tree and a measured boundary. Both halves
    /// fail closed and for different reasons, so the error names which; nothing turns a failure
    /// here into an unconfined run.
    pub fn confined(
        root: impl Into<PathBuf>,
        confine: Box<dyn Confinement>,
    ) -> Result<HostProcesses, ExecError> {
        Ok(Self::with_tree(root, super::scope::host_tree()?).with_confinement(confine))
    }

    pub fn with_tree(root: impl Into<PathBuf>, tree: Box<dyn ScopeTree>) -> HostProcesses {
        let tree: Arc<dyn ScopeTree> = Arc::from(tree);
        let h = HostProcesses {
            deadlines: Deadlines::new(Arc::clone(&tree)),
            tree,
            // Not asked for, and named as such rather than left absent, so a
            // reader of `describe()` cannot mistake silence for a boundary.
            confine: std::sync::RwLock::new(Box::new(Unconfined::because(
                "this host was built by `HostProcesses::with_tree` / \
                 `HostBackend::executable`, which ask for a lifetime mechanism and \
                 no boundary. `HostBackend::confined` is the one that adds the \
                 namespaces.",
            ))),
            root: root.into(),
            session: Mutex::new(None),
            turn: Mutex::new(None),
            jobs: Mutex::new(Vec::new()),
            by_id: Mutex::new(std::collections::HashMap::new()),
            claimed_slugs: Mutex::new(Vec::new()),
            reaps: Mutex::new(Vec::new()),
            promotions: Mutex::new(Vec::new()),
            pinned_path: std::env::var("PATH").unwrap_or_else(|_| {
                "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".into()
            }),
            standing_env: Mutex::new(Vec::new()),
            path_prefix: Mutex::new(None),
            monitors: Arc::new(Monitors::new()),
            protected: Mutex::new(Vec::new()),
            capture_bytes: DEFAULT_CAPTURE_BYTES,
            scratch: Mutex::new(None),
            shell: vec!["/bin/sh".into(), "-c".into()],
            console_shell: super::console::shell(),
            aggregates: Mutex::new(std::collections::HashMap::new()),
            kept_scopes: Arc::new(Mutex::new(Vec::new())),
            session_name: unique("s"),
        };
        h.protect_self_and_ancestors();
        h
    }

    pub fn with_confinement(mut self, confine: Box<dyn Confinement>) -> Self {
        self.confine = std::sync::RwLock::new(confine);
        self
    }

    /// Widen this session's view, by re-probing. The confinement handed in was measured the
    /// same way the first one was, and only then does it take the slot. The caller builds it:
    /// this layer is a mechanism and must not learn what to bind, which is a decision.
    pub fn replace_confinement(&self, confine: Box<dyn Confinement>) {
        *self.confine.write().expect("confinement") = confine;
    }

    /// The view as it stands, for a caller building a wider one from it. `None`
    /// where there is no view to widen.
    pub fn current_view(&self) -> Option<crate::exec::confine::ViewSpec> {
        self.confine
            .read()
            .expect("confinement")
            .boundary()
            .map(|b| b.view.clone())
    }

    /// The measured boundary, cloned out from under the lock. By value because a grant replaces
    /// the confinement, so a borrow of the one inside it cannot outlive the guard.
    pub fn boundary(&self) -> Option<crate::exec::confine::Boundary> {
        self.confine
            .read()
            .expect("confinement")
            .boundary()
            .cloned()
    }

    /// The argv a `!term` pane is started inside, so it gets the boundary a `bash` call gets,
    /// read off the confinement as it stands now so a grant made since the session opened is in
    /// it. `wrap_terminal` and not `wrap`: the pane's pty is its controlling terminal.
    pub fn terminal_wrap(
        &self,
        cwd: &std::path::Path,
        env: &[(String, String)],
    ) -> Result<Vec<String>, ExecError> {
        self.confine
            .read()
            .expect("confinement")
            .wrap_terminal(&ConfinePlan { cwd, env })
    }

    /// What the boundary is, in its own words.
    pub fn confinement_describe(&self) -> String {
        self.confine.read().expect("confinement").describe()
    }

    pub fn with_capture_bytes(mut self, bytes: usize) -> Self {
        self.capture_bytes = bytes;
        self
    }

    /// Where a run with no deadline's output goes on living. The daemon calls this once with
    /// the session's scratch directory, and only the session's own opener knows it.
    pub fn set_scratch_dir(&self, dir: impl Into<PathBuf>) {
        *self.scratch.lock().unwrap_or_else(|e| e.into_inner()) = Some(dir.into());
    }

    /// Give a run that nothing bounds a file keeping everything it prints, in the session's
    /// scratch directory as `job-<id>.out`; a run with a deadline keeps the 8 MiB window instead,
    /// and `None` means an ended run, no scratch directory, or an open that failed on stderr.
    fn spill_output(&self, job: &Job) -> Option<OutputFile> {
        if let Some(existing) = job.output_file() {
            return Some(existing);
        }
        // Nothing more will be written by a run that has ended, and its window still holds what it
        // wrote: a file made now would be a copy of a finished thing.
        if !job.state().is_running() {
            return None;
        }
        let dir = self
            .scratch
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()?;
        if let Err(e) = std::fs::create_dir_all(&dir) {
            eprintln!("letibot: scratch dir {}: {e}", dir.display());
            return None;
        }
        let path = dir.join(format!("job-{}.out", job.id.0));
        match job.capture.lock().expect("capture").spill_to(&path) {
            Ok(f) => Some(f),
            Err(why) => {
                // Said where the daemon's own banner is, because the alternative is a
                // record naming a file that is not there.
                eprintln!(
                    "letibot: `{}`'s output could not be spilled to a file, so it stays only \
                     in its window: {why}",
                    job_label(job.slug.as_deref(), &job.id.0)
                );
                None
            }
        }
    }

    /// The shell a command is handed to, `/bin/sh -c` unless this says otherwise.
    /// Non-interactive and non-login, so no rc file is read and a bare name resolves through
    /// the pinned `PATH` and nothing else. This is what a model's command is read by.
    pub fn with_shell(mut self, shell: Vec<String>) -> Self {
        self.shell = shell;
        self
    }

    /// The shell the operator's own command is handed to. See the field and
    /// [`super::console::shell`].
    pub fn with_console_shell(mut self, shell: Vec<String>) -> Self {
        self.console_shell = shell;
        self
    }

    /// The harness's own process and every process above it, to the root of the tree. A `pkill`
    /// matching any of them kills the thing evaluating the command, and the parent chain is the
    /// half a model has no way to see.
    fn protect_self_and_ancestors(&self) {
        let me = std::process::id();
        let mut pid = me;
        let mut depth = 0;
        while pid > 1 && depth < 32 {
            let why = if pid == me {
                "this is the process evaluating your command".to_string()
            } else {
                format!(
                    "this is an ancestor of the process evaluating your command (depth {depth})"
                )
            };
            self.protect_with(pid, why, false);
            match parent_of(pid) {
                Some(p) if p != pid => pid = p,
                _ => break,
            }
            depth += 1;
        }
    }

    /// Declare a pid the model must not reach by pattern.
    pub fn protect(&self, pid: u32, why: impl Into<String>) {
        self.protect_with(pid, why.into(), false);
    }

    /// Declare a pid that cannot exit while this session runs: the model server serving the
    /// turn is the case this exists for. A waiter on one of these is a deadlock, not a slow wait.
    pub fn protect_outliving(&self, pid: u32, why: impl Into<String>) {
        self.protect_with(pid, why.into(), true);
    }

    fn protect_with(&self, pid: u32, why: String, outlives: bool) {
        // The same two reads `Reaped::observe` makes, on either kernel.
        let Reaped { comm, cmdline, .. } = Reaped::observe(pid);
        if comm.is_empty() && cmdline.is_empty() {
            return;
        }
        let mut p = self.protected.lock().expect("protected");
        if let Some(e) = p.iter_mut().find(|e| e.pid == pid) {
            e.outlives_the_turn |= outlives;
            // Reasons accumulate. One pid can be protected for two reasons, and dropping the
            // second because the first got there first leaves a refusal naming the less useful
            // one, which the model reasonably reads as a broken guard.
            if !e.why.contains(why.trim_end_matches('.')) {
                e.why = format!("{}; and {}", e.why, why);
            }
            return;
        }
        p.push(Protected {
            pid,
            comm,
            cmdline,
            why,
            outlives_the_turn: outlives,
        });
    }

    /// The pairs every command of this session carries (see the field). Later calls replace
    /// earlier ones wholesale.
    pub fn set_standing_env(&self, pairs: Vec<(String, String)>) {
        *self.standing_env.lock().unwrap_or_else(|e| e.into_inner()) = pairs;
    }

    /// A directory searched before the pinned `PATH` (see the field).
    pub fn set_path_prefix(&self, dir: Option<String>) {
        *self.path_prefix.lock().unwrap_or_else(|e| e.into_inner()) = dir;
    }

    /// Resolve a listening TCP port to the pid holding it, and protect it: the model can see
    /// `:8080` in a config and has no way to know which pid that is. `None` is a real answer,
    /// since another user's `/proc/<pid>/fd` may not be readable from here.
    pub fn protect_listener(
        &self,
        port: u16,
        why: impl Into<String>,
        outlives: bool,
    ) -> Option<u32> {
        let pid = listener_pid(port)?;
        self.protect_with(pid, why.into(), outlives);
        Some(pid)
    }

    fn find(&self, id: &JobId) -> Option<Arc<Job>> {
        self.by_id.lock().expect("jobs by id").get(id).cloned()
    }

    /// The job of jobs held under this id, if one is: the lookup every read path in
    /// [`ProcessHost`] makes before it goes looking for a process.
    fn aggregate(&self, id: &JobId) -> Option<Arc<dyn Aggregate>> {
        self.aggregates.lock().expect("aggregates").get(id).cloned()
    }

    /// The row a job of jobs answers with. Two fields are said rather than faked: `pid` is
    /// 0, because there is no process to name; and `scope` has no cgroup path, because the
    /// work is in other machines' groups and a path here would be a cgroup a kill could empty.
    fn aggregate_view(&self, id: &JobId, agg: &dyn Aggregate) -> JobView {
        JobView {
            memory_cap: MemoryCap::NotAsked,
            memory: None,
            id: id.clone(),
            command: agg.command(),
            slug: agg.slug(),
            scope: self.session_scope_id(),
            owner: self.session_scope_id(),
            cwd: String::new(),
            pid: 0,
            // `Asked` and not `Promoted`. A fanout the model backgrounded is the model's own
            // request; it was never in the foreground for anybody to move.
            background: Some(Backgrounding::Asked),
            state: agg.state(),
            elapsed: agg.elapsed(),
            ran_for: agg.ran_for(),
            produced: agg.produced(),
            since_last_output: agg.since_last_output(),
            output_file: None,
        }
    }

    /// The session scope's id, as a [`ScopeId`], with no cgroup path for a caller that only needs
    /// the name (see [`HostProcesses::aggregate_view`]).
    fn session_scope_id(&self) -> ScopeId {
        ScopeId {
            kind: ScopeKind::Session,
            name: self.session_name.clone(),
            path: PathBuf::new(),
        }
    }

    /// The slug this spawn takes, made unique against every name this session has handed out:
    /// `release-build`, then `release-build-2` and so on until one is free. Nothing here derives
    /// anything from the command, and a request that named nothing gets nothing.
    fn claim_slug(&self, asked: &str) -> String {
        let mut claimed = self.claimed_slugs.lock().unwrap_or_else(|e| e.into_inner());
        let mut name = asked.to_string();
        let mut n = 2u32;
        while claimed.iter().any(|c| c == &name) {
            name = format!("{asked}-{n}");
            n += 1;
        }
        claimed.push(name.clone());
        name
    }

    fn view(job: &Job) -> JobView {
        let cap = job.capture.lock().expect("capture");
        let life = job.lifetime();
        let memory = job
            .memory_used()
            .or_else(|| super::scope::memory_use_of(&life.scope));
        JobView {
            memory_cap: job.memory_cap().clone(),
            memory,
            id: job.id.clone(),
            command: job.command.clone(),
            slug: job.slug.clone(),
            scope: life.scope,
            owner: life.owner,
            cwd: job.cwd.clone(),
            pid: job.pid,
            background: life.background,
            state: job.state(),
            elapsed: job.elapsed(),
            ran_for: job.ran_for(),
            produced: cap.produced(),
            since_last_output: cap.since_last(),
            output_file: cap.output_file(),
        }
    }

    /// What the deadline watchdog has ended, oldest first. The read-back half of the
    /// mechanism: *the deadline ended this run* is a claim, and a claim nothing can check is the
    /// unfalsifiable zero this substrate refuses everywhere else.
    pub fn deadlines_fired(&self) -> Vec<DeadlineFired> {
        self.deadlines.fired()
    }

    /// Every job id this host knows, for clause 1's benefit when one is unknown.
    pub fn job_ids(&self) -> Vec<JobId> {
        let mut out: Vec<JobId> = self
            .jobs
            .lock()
            .expect("jobs")
            .iter()
            .map(|j| j.id.clone())
            .collect();
        out.extend(self.aggregates.lock().expect("aggregates").keys().cloned());
        out
    }

    fn record(&self, r: Reaping) -> Reaping {
        let mut reaps = self.reaps.lock().expect("reaps");
        reaps.push(r.clone());
        bound_records(&mut reaps, RECORD_LIMIT);
        r
    }

    fn record_promotion(&self, p: Promotion) -> Promotion {
        let mut promotions = self.promotions.lock().expect("promotions");
        promotions.push(p.clone());
        bound_records(&mut promotions, RECORD_LIMIT);
        p
    }
}

impl ProcessHost for HostProcesses {
    /// Read off the state, both halves: the sentence comes from [`Confinement::describe`],
    /// which comes from a boundary measured from inside itself, rather than from a banner that
    /// asserts a property of the session.
    fn describe(&self) -> String {
        format!(
            "host processes; lifetime: {}; view: {}",
            self.tree.describe(),
            self.confinement_describe()
        )
    }

    fn scope_for(&self, kind: ScopeKind, name: Option<&str>) -> Result<ScopeId, ExecError> {
        match kind {
            ScopeKind::Explicit => {
                // An explicit scope must be NAMED: an unnamed one would outlive the session and
                // nobody could refer to it in order to end it, which is the leak with an extra step.
                let n = name.unwrap_or("unnamed");
                self.tree.open(ScopeKind::Explicit, n, None)
            }
            ScopeKind::Session => {
                let mut s = self.session.lock().expect("session scope");
                if let Some(id) = s.as_ref() {
                    return Ok(id.clone());
                }
                let id = self
                    .tree
                    .open(ScopeKind::Session, &self.session_name, None)?;
                *s = Some(id.clone());
                Ok(id)
            }
            ScopeKind::Turn => {
                // A turn scope is a CHILD of the session scope, which is what makes the
                // session's end reap it without the session having to know it is there.
                let parent = self.scope_for(ScopeKind::Session, None)?;
                let mut t = self.turn.lock().expect("turn scope");
                if let Some(id) = t.as_ref() {
                    return Ok(id.clone());
                }
                let id = self
                    .tree
                    .open(ScopeKind::Turn, &unique("t"), Some(&parent))?;
                *t = Some(id.clone());
                Ok(id)
            }
        }
    }

    fn spawn(&self, req: &SpawnRequest) -> Result<JobId, ExecError> {
        let parent = self.scope_for(req.scope, req.scope_name.as_deref())?;
        let id = JobId::next();
        // One cgroup per job, inside its scope's. `job_kill` is then the same operation as
        // ending a scope, on a smaller directory: one kill mechanism, not two.
        let cgroup = self
            .tree
            .open(req.scope, &format!("job-{id}"), Some(&parent))?;

        let cwd = self.root.join(req.cwd.trim_start_matches('/'));

        // The boundary is decided before the cgroup is committed to. A session that asks for
        // confinement and has none refuses here, and refusing after the `mkdir` would leave an
        // empty scope directory that a later listing reads as somebody's work.
        let confined = self.confine.read().expect("confinement");
        let planned = if req.tty && confined.spares_operator() {
            Ok(Vec::new())
        } else {
            confined.wrap(&ConfinePlan {
                cwd: &cwd,
                env: &req.env,
            })
        };
        let wrap = match planned {
            Ok(w) => w,
            Err(e) => {
                let _ = self.tree.end(&cgroup);
                return Err(e);
            }
        };

        // The cap goes on before any process joins, so there is no instant in which the
        // command runs uncapped. A cap the tree cannot set does not refuse the command: the job
        // runs, and its result says UNCAPPED and why ([`MemoryCap::Uncapped`]).
        let memory_cap = match req.memory {
            None => MemoryCap::NotAsked,
            Some(bytes) => match self.tree.cap_memory(&cgroup, bytes) {
                Ok(()) => MemoryCap::Capped(bytes),
                Err(why) => MemoryCap::Uncapped { asked: bytes, why },
            },
        };

        let procs = Cgroup2::procs_path(&cgroup);
        // The join token, which is the evidence and belongs to the daemon. A path this
        // process owns, passed to the wrapper as `$2` and created only on the branch that joined
        // the cgroup: a file the command's output cannot reach, so `joined` is OBSERVED, not read.
        let token = std::env::temp_dir().join(format!("letibot-joined-{}", id.0));
        let _ = std::fs::remove_file(&token);
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.arg("-c")
            .arg(join_script())
            .arg("letibot-scope")
            .arg(&procs)
            .arg(&token);
        // The order is load-bearing. `join_script` writes `$$` into `cgroup.procs` and only
        // then `exec`s `"$@"`, so the join happens on the HOST before any namespace exists:
        // `/sys/fs/cgroup` is not in the mount view, so joining from inside would fail.
        for a in &wrap {
            cmd.arg(a);
        }
        // Their shell for their own run, the host's for everything else. `bash -ic` reads
        // `~/.bashrc`, which is where `alias ls='ls --color=auto'` lives, and without it the
        // colour does not exist ([`super::console`]).
        let shell = if req.tty {
            &self.console_shell
        } else {
            &self.shell
        };
        for s in shell {
            cmd.arg(s);
        }
        cmd.arg(&req.command);
        cmd.current_dir(&cwd);
        // stdin, which the pty branch below replaces whenever there is a pty to give the run.
        // What is set here is the fallback: the pipe that keeps the operator's own run answerable,
        // and `/dev/null` for everything else, so a reader gets EOF rather than waiting.
        cmd.stdin(if req.tty {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        // A terminal on the output side, for the operator's own run ([`super::pty`]). A box
        // where the pty cannot be opened falls back to pipes rather than refusing: the terminal
        // is what the operator's colour needs, not what the command needs.
        let mut pty: Option<super::pty::Pty> = None;
        if req.tty {
            match super::pty::Pty::open() {
                // One pty on all three descriptors, so the capture ring is fed by the master
                // and the command's input is the same terminal its output is on.
                Ok(p) => match (p.stdio(), p.stdio(), p.stdio()) {
                    (Ok(inp), Ok(out), Ok(err)) => {
                        // `ECHO` off, before anything runs. The daemon is the writer on this
                        // terminal, and an echoing terminal would put every `!send` line into the
                        // run's output, which is a transcript row. Not fatal if it fails.
                        let _ = p.no_echo();
                        // And the terminal becomes the child's controlling terminal, which is
                        // what makes bash a job-control shell and silences the two lines it printed
                        // on every row ([`super::pty::controlling_terminal`]).
                        super::pty::controlling_terminal(&mut cmd, p.slave_fd());
                        cmd.stdin(inp).stdout(out).stderr(err);
                        pty = Some(p);
                    }
                    _ => {
                        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
                    }
                },
                Err(_) => {
                    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
                }
            }
        } else {
            cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        }
        // On macOS the scope IS the process group (`scope::ProcessGroups`), so a run without
        // a pty (the pty branch's `setsid` already made it a leader) must lead a group of its own,
        // or the `$$` the wrapper records names a group it is not in.
        #[cfg(target_os = "macos")]
        if pty.is_none() {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        // Env hygiene: the environment is cleared before anything is set, so `PATH` is the
        // pinned one and a bare name resolves through nothing else. The four names below are then
        // dropped from the session's pairs too, because bash sources `$BASH_ENV` unasked.
        cmd.env_clear();
        let prefix = self
            .path_prefix
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        cmd.env(
            "PATH",
            match prefix {
                Some(p) => format!("{p}:{}", self.pinned_path),
                None => self.pinned_path.clone(),
            },
        );
        let standing = self
            .standing_env
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        for (k, v) in standing.iter().chain(req.env.iter()) {
            if !matches!(
                k.as_str(),
                "BASH_ENV" | "ENV" | "SHELLOPTS" | "BASHOPTS" | "PATH"
            ) {
                cmd.env(k, v);
            }
        }

        let mut child = cmd.spawn().map_err(|e| {
            // The cgroup was made and nothing joined it; take it back rather than
            // leaving an empty directory that a later listing would call a scope.
            let _ = self.tree.end(&cgroup);
            ExecError::Spawn(e.to_string())
        })?;
        let pid = child.id();
        // The parent's own copies of the slave close HERE, and this is load-bearing. A pty
        // whose slave is open anywhere does not report the child's exit, so the drains below would
        // block for ever. `Stdio::null` here is a drop, not a redirection.
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::null());

        // The run's input: a second handle on the master of its own terminal, taken here because
        // it stops being available once `into_master` consumes the pty. A `dup` that failed is
        // swallowed: the run keeps its terminal and has no handle, so `Stdin::is_open` is false.
        let input = pty
            .as_ref()
            .and_then(|p| p.input().ok().map(|m| (m, p.slave_path().to_string())));
        // `child.stdin` is the fallback's pipe and `None` whenever the pty was used: a run
        // with a terminal is answered through the terminal, and a run with neither is one nobody
        // can answer ([`super::jobs::Stdin::none`]).
        let stdin = match (input, child.stdin.take()) {
            (Some((master, slave_path)), _) => super::jobs::Stdin::terminal(master, slave_path),
            (None, Some(w)) => super::jobs::Stdin::pipe(w),
            (None, None) => super::jobs::Stdin::none(),
        };

        let job = Arc::new(
            Job::new(
                id.clone(),
                req.command.clone(),
                // The name, decided here and never again. The claim is made where this spawn
                // can no longer be refused for a reason that leaves no job behind, and fixed on
                // the record, so a listing an hour later reads the same string.
                req.slug.as_deref().map(|s| self.claim_slug(s)),
                Lifetime {
                    scope: cgroup,
                    owner: parent.clone(),
                    // `Asked` only when the model asked. A foreground command starts with
                    // `None`, and a promotion fills this in, so a job the runtime moved can never
                    // be read back as one the model requested.
                    background: req.background.then_some(Backgrounding::Asked),
                },
                req.cwd.clone(),
                pid,
                self.capture_bytes,
                stdin,
            )
            .with_memory_cap(memory_cap),
        );

        // A run nothing bounds gets its file here, before a single byte is drained, and one
        // that already has it keeps it. Not for a run with a deadline: an 8 MiB window is what
        // bounds a foreground `yes` ([`HostProcesses::spill_output`]).
        if req.background || req.tty {
            self.spill_output(&job);
        }

        let out = child.stdout.take();
        let err = child.stderr.take();
        // The slave handle goes now, once the child holds its own copy. A slave this process
        // keeps open never closes, so the master would never report the exit. macOS holds one
        // across the exit so the tail is not discarded, released by the waiter below.
        #[cfg(target_os = "macos")]
        let tail_guard = pty.as_ref().and_then(|p| p.tail_guard().ok());
        let master = pty.map(|p| p.into_master());
        let a = drain(out, Arc::clone(&job));
        let b = match master {
            Some(m) => drain(Some(m), Arc::clone(&job)),
            None => drain(err, Arc::clone(&job)),
        };

        let waiter = Arc::clone(&job);
        let tree = Arc::clone(&self.tree);
        let kept = Arc::clone(&self.kept_scopes);
        std::thread::Builder::new()
            .name(format!("letibot-job-{id}"))
            .spawn(move || {
                // Drain BEFORE reaping, so every byte is in the capture by the time the state stops
                // saying `Running`. macOS reaps first, because its guard holds a slave that the
                // master would otherwise wait on.
                #[cfg(target_os = "macos")]
                let status = child.wait();
                #[cfg(target_os = "macos")]
                if let Some(g) = tail_guard {
                    g.release();
                }
                if let Some(h) = a {
                    let _ = h.join();
                }
                if let Some(h) = b {
                    let _ = h.join();
                }
                // Both drains have joined, so the file's last byte is in. The descriptor goes
                // now rather than at the host's death: the file is still on disk and still named,
                // and fifty backgrounded builds must not hold fifty descriptors.
                waiter.close_spill();
                #[cfg(not(target_os = "macos"))]
                let status = child.wait();
                // A job already marked `Killed` keeps that: a SIGKILL from a cgroup reap arrives
                // here as a signal, and reporting the signal would lose the fact that we did it.
                if !waiter.state().is_running() {
                    waiter.finished.notify_all();
                    return;
                }
                let state = match status {
                    Ok(s) => match s.code() {
                        // The wrapper's marker, and not the number alone: 125 is a legitimate
                        // exit code, so a command that chose it would read as never having run. A
                        // command that printed the marker itself ran, and 125 is its own answer.
                        Some(EXIT_NOT_SCOPED) if launcher_failed(&waiter) && !token.exists() => {
                            JobState::NotScoped
                        }
                        Some(c) => JobState::Exited { code: c },
                        None => JobState::Signalled {
                            signal: signal_of(&s),
                        },
                    },
                    Err(e) => JobState::Killed {
                        by: format!("the harness lost track of it: {e}"),
                    },
                };
                // The token is consumed here, once, by the only reader: a settled job must
                // not leave it behind, and removing it after the read keeps `present` and
                // `joined` the same fact for the whole of one job.
                let _ = std::fs::remove_file(&token);
                // Read the kernel's counters before the state settles: a reader woken by
                // the settle builds the result, and the result is where the peak is said.
                if let Some(used) = super::scope::memory_use_of(&waiter.lifetime().scope) {
                    waiter.record_memory(used);
                }
                waiter.settle(state);
                // The leader is gone, so its own scope is debris unless the command left a
                // process in it: one directory per command ever run otherwise, and a scope still
                // holding work is somebody's live process and stays - for the next spawn to look at.
                settle_scope(tree.as_ref(), &kept, &waiter.lifetime().scope);
            })
            .map_err(|e| ExecError::Spawn(e.to_string()))?;

        // Do not return until the process is IN its cgroup, or is finished. Without it, a
        // background run followed by `job_kill` reads an empty cgroup and kills nothing. The loop
        // ends on either half because a fast command can finish before it is ever a member.
        let deadline = Instant::now() + Duration::from_secs(5);
        let joined = loop {
            if self
                .tree
                .members(&job.lifetime().scope)
                .map(|m| m.contains(&pid))
                .unwrap_or(false)
            {
                break true;
            }
            if !job.state().is_running() {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            std::thread::sleep(Duration::from_millis(1));
        };
        if !joined {
            // A live process that is not in its scope is the orphan this substrate exists to
            // make impossible, so it is killed rather than handed back as a job nothing owns.
            let _ = std::process::Command::new("kill")
                .arg("-KILL")
                .arg(pid.to_string())
                .status();
            let _ = self.tree.end(&job.lifetime().scope);
            return Err(ExecError::Spawn(format!(
                "pid {pid} started but was still not a member of its cgroup after 5s; \
                 it was killed rather than left running outside any scope"
            )));
        }

        self.by_id
            .lock()
            .expect("jobs by id")
            .insert(job.id.clone(), Arc::clone(&job));
        self.jobs.lock().expect("jobs").push(job);
        self.release_settled_captures(SETTLED_CAPTURE_BUDGET);
        self.release_kept_scopes();
        self.prune_finished(KEPT_FINISHED_JOBS, FINISHED_JOB_GRACE, SystemTime::now());
        Ok(id)
    }

    /// Take a job the daemon did not start, and make it one of this host's. Everything above
    /// reads the host's own job table, so the record becomes a [`Job`]. When the record carries
    /// the directories the job really occupies they are used verbatim - the alternative is a
    /// fresh cgroup on this host's root, which would be an empty directory named after a
    /// process that is somewhere else. Failing that the cgroup name is derivable and the
    /// re-open is exact; a vanished process settles as [`JobState::Dead`], never `Exited`.
    fn adopt(&self, a: &Adopted) -> Result<JobId, ExecError> {
        let id = JobId(a.handle.clone());
        if self.find(&id).is_some() {
            return Ok(id);
        }
        // A record with no identity is one nothing can tell from a later process at that number,
        // and the watcher below would settle a LIVE process `Dead` on its first look.
        if a.identity.is_empty() {
            return Err(ExecError::Spawn(format!(
                "`{}` (pid {}) carries no readable start time, so nothing here could tell it from \
                 a later process with that number. It was NOT adopted: it is still running \
                 wherever it was started, and no table here claims to watch it.",
                a.handle, a.pid
            )));
        }
        let (kind, name) = scope_kind_of(&a.scope);
        let (owner, scope) = match (&a.owner_dir, &a.scope_dir) {
            (Some(owner_dir), Some(scope_dir)) => {
                // The real directories, listed so `scopes()` can show them: a scope nobody lists is
                // an unreapable one.
                let owner = super::scope::ScopeId {
                    kind,
                    name,
                    path: std::path::PathBuf::from(owner_dir),
                };
                let scope = super::scope::ScopeId {
                    kind,
                    name: format!("job-{}", a.handle),
                    path: std::path::PathBuf::from(scope_dir),
                };
                self.tree.remember(&owner);
                self.tree.remember(&scope);
                (owner, scope)
            }
            _ => {
                let owner = self.scope_for(kind, Some(&name))?;
                let scope = self
                    .tree
                    .open(kind, &format!("job-{}", a.handle), Some(&owner))?;
                (owner, scope)
            }
        };
        let job = Arc::new(Job::new(
            id.clone(),
            a.command.clone(),
            a.slug.clone(),
            Lifetime {
                scope,
                owner,
                background: Some(letibot_transcript::Backgrounding::Asked),
            },
            a.cwd.clone(),
            a.pid,
            // The ring every job of this host gets: a zero-length one dropped every byte.
            DEFAULT_CAPTURE_BYTES,
            super::jobs::Stdin::none(),
        ));
        self.by_id
            .lock()
            .expect("jobs by id")
            .insert(id.clone(), Arc::clone(&job));
        self.jobs.lock().expect("jobs").push(Arc::clone(&job));
        // The record's identity is the whole of the evidence that this is the same process. A
        // pid alone would settle a stranger's job, or keep one running for ever, on a box that had
        // reused the number.
        let identity = a.identity.clone();
        let pid = a.pid;
        let watched = Arc::clone(&job);
        let tree = Arc::clone(&self.tree);
        let kept = Arc::clone(&self.kept_scopes);
        let _ = std::thread::Builder::new()
            .name(format!("adopted-{}", &a.handle[..a.handle.len().min(20)]))
            .spawn(move || {
                while watched.state.lock().expect("job state").is_running() {
                    match super::procs::read(pid) {
                        Some(p) if identity == format!("start:{}", p.start) && p.state != 'Z' => {}
                        // Gone, or the number belongs to somebody else now: either way this job's
                        // process is not there.
                        _ => {
                            watched.settle(JobState::Dead);
                            settle_scope(tree.as_ref(), &kept, &watched.lifetime().scope);
                            break;
                        }
                    }
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
            });
        Ok(id)
    }

    /// Give this run a deadline, and hand its enforcement to the host. The tool that starts a
    /// run knows how long it may live, so it arms this; what it must not do is enforce it, which is
    /// the whole of [`Deadlines`]: the run's own thread reports, this thread ends the run.
    fn arm_deadline(&self, job: &JobId, after: Duration) {
        // A job of jobs arms nothing: every branch carries its own deadline in its own process
        // record, where the supervisor enforces it on the machine that runs it, and a second clock
        // here would be a second answer to *when is it out of time*.
        if self.aggregate(job).is_some() {
            return;
        }
        if let Some(j) = self.find(job) {
            self.deadlines.arm(j, after);
        }
    }

    /// Take a handle that stands for work running elsewhere ([`Aggregate`]). The name it answers
    /// to is the aggregate's own ([`Aggregate::slug`]), claimed through this host's slug record, so
    /// nothing here has to know what a fanout is.
    fn hold(&self, place: &JobId, agg: Arc<dyn Aggregate>) -> Result<(), ExecError> {
        if self.find(place).is_some() || self.aggregate(place).is_some() {
            return Err(ExecError::Spawn(format!(
                "`{place}` is already a job of this host, so a second one under that handle would \
                 be two things with one name"
            )));
        }
        self.aggregates
            .lock()
            .expect("aggregates")
            .insert(place.clone(), agg);
        Ok(())
    }

    fn settlement_note(&self, job: &JobId) -> Option<String> {
        self.aggregate(job).map(|a| a.report())
    }

    fn claim_name(&self, asked: &str) -> String {
        self.claim_slug(asked)
    }

    fn wait_job(&self, id: &JobId, deadline: Duration) -> Result<Waited, ExecError> {
        // A job of jobs settles here, not in a cgroup: one wait, N branches, each watched by
        // the thread that started it, so the answer is *it happened* only when every one has ended.
        if let Some(agg) = self.aggregate(id) {
            let started = Instant::now();
            return match agg.wait(deadline) {
                Some(state) => Ok(Waited::Happened {
                    state: Some(state),
                    took: started.elapsed(),
                }),
                None => Ok(Waited::Deadline {
                    took: started.elapsed(),
                    produced: agg.produced(),
                    since_last_output: agg.since_last_output(),
                }),
            };
        }
        let job = self.find(id).ok_or(ExecError::NoSuchJob(id.0.clone()))?;
        let started = Instant::now();
        let state = job.wait_until(deadline);
        if state.is_running() {
            let cap = job.capture.lock().expect("capture");
            return Ok(Waited::Deadline {
                took: started.elapsed(),
                produced: cap.produced(),
                since_last_output: cap.since_last(),
            });
        }
        Ok(Waited::Happened {
            state: Some(state),
            took: started.elapsed(),
        })
    }

    /// Presence, then absence, and never absence alone. An empty cgroup at t=0 is a boot
    /// window, not a finish, so this waits up to a quarter of the deadline (at most two seconds)
    /// to see the scope populated, and otherwise returns [`Waited::NeverStarted`].
    fn wait_scope(&self, scope: &ScopeId, deadline: Duration) -> Result<Waited, ExecError> {
        let started = Instant::now();
        let members = self.tree.members(scope)?;
        let presence_budget = (deadline / 4).min(Duration::from_secs(2));
        let mut seen = !members.is_empty();
        while !seen && started.elapsed() < presence_budget {
            std::thread::sleep(Duration::from_millis(20));
            seen = !self.tree.members(scope).unwrap_or_default().is_empty();
        }
        if !seen {
            return Ok(Waited::NeverStarted {
                waited_for_presence: started.elapsed(),
            });
        }
        while started.elapsed() < deadline {
            if self.tree.members(scope).unwrap_or_default().is_empty() {
                return Ok(Waited::Happened {
                    state: None,
                    took: started.elapsed(),
                });
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        Ok(Waited::Deadline {
            took: started.elapsed(),
            produced: 0,
            since_last_output: None,
        })
    }

    fn job(&self, id: &JobId) -> Option<JobView> {
        match self.aggregate(id) {
            Some(agg) => Some(self.aggregate_view(id, &*agg)),
            None => self.find(id).map(|j| Self::view(&j)),
        }
    }

    fn jobs(&self) -> Vec<JobView> {
        let mut out: Vec<JobView> = self
            .jobs
            .lock()
            .expect("jobs")
            .iter()
            .map(|j| Self::view(j))
            .collect();
        // A job of jobs is a row like any other, after the runs, because it stands for work those
        // runs are not: a listing that interleaved the two would suggest the parent was one more
        // process on this box.
        let aggs: Vec<(JobId, Arc<dyn Aggregate>)> = self
            .aggregates
            .lock()
            .expect("aggregates")
            .iter()
            .map(|(id, a)| (id.clone(), Arc::clone(a)))
            .collect();
        for (id, agg) in aggs {
            out.push(self.aggregate_view(&id, &*agg));
        }
        out
    }

    fn output(&self, id: &JobId, from: u64, limit: usize) -> Result<OutputSlice, ExecError> {
        // The journal, not a table ([`Aggregate::output`]).
        if let Some(agg) = self.aggregate(id) {
            return Ok(agg.output(from, limit));
        }
        let job = self.find(id).ok_or(ExecError::NoSuchJob(id.0.clone()))?;
        let cap = job.capture.lock().expect("capture");
        Ok(cap.slice(from, limit))
    }

    fn kill_job_as(&self, id: &JobId, by: &str) -> Result<Reaping, ExecError> {
        // A job of jobs is ended branch by branch, on each branch's own host through that
        // host's own process group: there is no cgroup of ours holding them, and a `kill -TERM`
        // here would reach processes this box cannot see.
        if let Some(agg) = self.aggregate(id) {
            let observed = agg.kill(by);
            let scope = self.session_scope_id();
            return Ok(self.record(Reaping {
                scope,
                at: std::time::SystemTime::now(),
                mechanism: "each branch's own process group, on its own host",
                observed,
                survivors: Vec::new(),
                waited: Duration::ZERO,
                // Nothing of this host's was removed: the parent has no cgroup of its own.
                removed: false,
                note: Some(format!(
                    "`{id}` is a job of jobs: every branch was ended on the machine it ran on, \
                     and the parent's own settlement follows when the last of them has stopped"
                )),
            }));
        }
        let job = self.find(id).ok_or(ExecError::NoSuchJob(id.0.clone()))?;
        // Mark first, so the waiter thread does not overwrite `Killed` with the
        // SIGKILL it is about to see. F5: the reason it stopped is a fact and the
        // signal is only its shape.
        let running = job.state().is_running();
        if running {
            job.settle(JobState::Killed { by: by.to_string() });
        }
        let mut r = self.tree.end(&job.lifetime().scope);
        if !running && r.observed.is_empty() {
            r.note = Some(format!(
                "`{id}` had already {} when the kill arrived; nothing was running to kill",
                job.state().word()
            ));
        }
        Ok(self.record(r))
    }

    fn end_scope(&self, scope: &ScopeId) -> Result<Reaping, ExecError> {
        let r = self.tree.end(scope);
        // Every job whose cgroup was under this scope stopped because we stopped
        // it, and its state must say that rather than `signalled 9`.
        for j in self.jobs.lock().expect("jobs").iter() {
            if j.lifetime().scope.path.starts_with(&scope.path) && j.state().is_running() {
                j.settle(JobState::Killed {
                    by: format!("the {} scope ending", scope.kind.as_str()),
                });
            }
        }
        // The watchers go with the scope, because a monitor that merely stopped being polled
        // would look exactly like one whose condition never fired.
        self.monitors.retire_under(scope);
        if scope.kind == ScopeKind::Turn {
            *self.turn.lock().expect("turn scope") = None;
        }
        if scope.kind == ScopeKind::Session {
            *self.session.lock().expect("session scope") = None;
            *self.turn.lock().expect("turn scope") = None;
        }
        Ok(self.record(r))
    }

    /// The order is load-bearing, and it is not the obvious one: open the new cgroup, migrate,
    /// measure, and only then change the job's recorded lifetime, or `job_list` says `session` about
    /// processes the turn scope is about to kill. The output file comes after the migration.
    fn promote(
        &self,
        id: &JobId,
        to: ScopeKind,
        name: Option<&str>,
        how: Backgrounding,
    ) -> Result<Promotion, ExecError> {
        let job = self.find(id).ok_or(ExecError::NoSuchJob(id.0.clone()))?;
        let life = job.lifetime();
        let ran_for = job.elapsed();
        let target = self.scope_for(to, name)?;

        // Already there. A caller that promoted the same job twice is told the second call
        // moved nothing. The file is still the answer to *where does its output go*, and this may
        // be the first promotion that could make one.
        if life.owner == target {
            return Ok(self.record_promotion(Promotion {
                job: id.clone(),
                command: job.command.clone(),
                slug: job.slug.clone(),
                from: life.owner.clone(),
                to: target,
                ran_for,
                how,
                migration: None,
                at: SystemTime::now(),
                note: Some(format!(
                    "`{}` was already owned by that scope, so nothing moved",
                    job_label(job.slug.as_deref(), &id.0)
                )),
                output_file: self.spill_output(&job),
            }));
        }

        if !job.state().is_running() {
            return Ok(self.record_promotion(Promotion {
                job: id.clone(),
                command: job.command.clone(),
                slug: job.slug.clone(),
                from: life.owner.clone(),
                to: target,
                ran_for,
                how,
                migration: None,
                at: SystemTime::now(),
                note: Some(format!(
                    "`{}` had already {} when the promotion arrived; there was no \
                     process left to move, and its output is still readable",
                    job_label(job.slug.as_deref(), &id.0),
                    job.state().word()
                )),
                output_file: None,
            }));
        }

        let fresh = self.tree.open(to, &format!("job-{id}"), Some(&target))?;
        // A promoted job keeps the cap it was started under: the new scope is a new cgroup,
        // and an uncapped one would be a cap lifted by a move nobody asked to lift it.
        if let MemoryCap::Capped(bytes) = job.memory_cap()
            && let Err(why) = self.tree.cap_memory(&fresh, *bytes)
        {
            let _ = self.tree.end(&fresh);
            return Err(ExecError::Cgroup {
                op: "carry the memory cap to",
                path: fresh.path.display().to_string(),
                why,
            });
        }
        let migration = match self.tree.migrate(&life.scope, &fresh) {
            Ok(m) => m,
            Err(e) => {
                // The new cgroup was made and nothing joined it. Take it back
                // rather than leave a directory a later listing reads as work.
                let _ = self.tree.end(&fresh);
                return Err(e);
            }
        };

        // Only now. The bookkeeping follows the measurement.
        job.relocate(Lifetime {
            scope: fresh.clone(),
            owner: target.clone(),
            background: Some(how.clone()),
        });

        // And the bytes get somewhere unbounded to go, now that the run really has outlived the
        // turn (see this method's header).
        let output_file = self.spill_output(&job);

        let note = (!migration.complete()).then(|| {
            format!(
                "PARTIAL: {} process(es) would not leave `{}` and are still reaped by \
                 it. The promotion is true of the rest.",
                migration.left_behind.len(),
                life.owner
            )
        });
        Ok(self.record_promotion(Promotion {
            job: id.clone(),
            command: job.command.clone(),
            slug: job.slug.clone(),
            from: life.owner,
            to: target,
            ran_for,
            how,
            migration: Some(migration),
            at: SystemTime::now(),
            note,
            output_file,
        }))
    }

    fn promotions(&self) -> Vec<Promotion> {
        self.promotions.lock().expect("promotions").clone()
    }

    fn scopes(&self) -> Vec<ScopeId> {
        self.tree.list()
    }

    /// The running jobs in an `explicit` scope: the ones no reaper is left for once this host goes,
    /// and the only ones an heir can be handed.
    fn survivors(&self) -> Vec<Adopted> {
        self.jobs
            .lock()
            .expect("jobs")
            .iter()
            .filter(|j| j.state().is_running())
            .filter_map(|j| {
                let life = j.lifetime();
                if life.owner.kind != ScopeKind::Explicit {
                    return None;
                }
                Some(Adopted {
                    handle: j.id.0.clone(),
                    command: j.command.clone(),
                    slug: j.slug.clone(),
                    cwd: j.cwd.clone(),
                    pid: j.pid,
                    // Read, never invented: an identity that cannot match settles the handed-over
                    // job as `Dead` on its first look, and the process runs on unwatched.
                    identity: super::procs::read(j.pid)
                        .map(|p| format!("start:{}", p.start))
                        .unwrap_or_default(),
                    scope: format!("explicit.{}", life.owner.name),
                    scope_dir: Some(life.scope.path.display().to_string()),
                    owner_dir: Some(life.owner.path.display().to_string()),
                })
            })
            .collect()
    }

    fn forget(&self, job: &JobId) {
        self.jobs.lock().expect("jobs").retain(|j| &j.id != job);
        self.by_id.lock().expect("jobs by id").remove(job);
    }

    fn reap_log(&self) -> Vec<Reaping> {
        self.reaps.lock().expect("reaps").clone()
    }

    fn monitors(&self) -> Option<&Arc<Monitors>> {
        Some(&self.monitors)
    }

    fn job_handle(&self, job: &JobId) -> Option<Arc<Job>> {
        self.find(job)
    }

    /// The job's own cgroup's members. See [`ProcessHost::job_pids`] for the reader and for
    /// why an empty list is *cannot say* rather than *nothing there*.
    fn job_pids(&self, job: &JobId) -> Vec<u32> {
        let Some(j) = self.find(job) else {
            return Vec::new();
        };
        self.tree.members(&j.lifetime().scope).unwrap_or_default()
    }

    fn workspace(&self) -> Option<&std::path::Path> {
        Some(&self.root)
    }

    fn protected(&self) -> Vec<Protected> {
        self.protected.lock().expect("protected").clone()
    }

    fn confinement_summary(&self) -> Option<String> {
        Some(self.confinement_describe())
    }

    fn absence_notes(&self, output: &str) -> Vec<String> {
        self.confine
            .read()
            .expect("confinement")
            .absence_notes(output)
    }

    fn outside_paths(&self, output: &str) -> Vec<std::path::PathBuf> {
        self.confine
            .read()
            .expect("confinement")
            .outside_paths(output)
    }

    fn launcher_failure(&self, output: &str) -> Option<String> {
        self.confine
            .read()
            .expect("confinement")
            .launcher_failure(output)
    }
}

/// The reaper lives at the creation site, which is here: a sweeper cannot tell debris from a
/// deliberate long-lived resource, and the thing that starts processes is the thing that ends them.
/// The session scope goes, and every turn scope and job under it; an explicit scope does not.
impl Drop for HostProcesses {
    fn drop(&mut self) {
        // The deadline watchdog goes with its host (see `Deadlines::closed`).
        self.deadlines.close();
        let session = self.session.lock().expect("session scope").clone();
        if let Some(s) = session {
            let _ = self.tree.end(&s);
        }
        // Then the empty directories: a cgroup holding nothing is not a leak of anything that
        // runs, but it is one more row in a listing somebody will one day have to decide about.
        self.tree.prune();
    }
}

/// Did the WRAPPER fail to put the process in its scope? The evidence is the sentence
/// [`super::scope::join_script`] writes to stderr, so the script and the check cannot drift, and
/// not the exit code: 125 is legitimate, so `exit 125` would read as never having run.
fn launcher_failed(job: &Job) -> bool {
    job.capture
        .lock()
        .expect("capture")
        .contains(super::scope::NOT_SCOPED_MARKER)
}

/// A reader thread that appends one stream into the job's capture. Stdout and stderr go into
/// one buffer, in arrival order, which is what a terminal shows; the ordering *between* the two
/// is approximate, because two pipes have no shared clock.
fn drain<R: Read + Send + 'static>(
    stream: Option<R>,
    job: Arc<Job>,
) -> Option<std::thread::JoinHandle<()>> {
    let mut stream = stream?;
    std::thread::Builder::new()
        .name(format!("letibot-drain-{}", job.id))
        .spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match stream.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => job.capture.lock().expect("capture").push(&buf[..n]),
                }
            }
        })
        .ok()
}

/// The signal that ended a process, without a `libc` dependency: `signal()` is behind the unix
/// extension trait, which this crate may use.
fn signal_of(status: &std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status.signal().unwrap_or(0)
}

/// A pid's parent, from `/proc/<pid>/stat`. Field 4 is `ppid`, read by splitting on the last
/// `)` rather than on whitespace: field 2 is the executable name in parentheses and may hold spaces.
#[cfg(target_os = "macos")]
pub(crate) fn parent_of(pid: u32) -> Option<u32> {
    super::darwin::info(pid).map(|i| i.ppid)
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn parent_of(pid: u32) -> Option<u32> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = &s[s.rfind(')')? + 1..];
    rest.split_whitespace().nth(1)?.parse().ok()
}

/// The socket inodes LISTENing on `port`, from `/proc/net/tcp{,6}`. Split out of [`listener_pid`]
/// because the two questions fail differently: *is anything listening* is answerable from
/// `/proc/net/tcp`, while *which pid* needs `/proc/<pid>/fd`, refused for another user.
#[cfg(not(target_os = "macos"))]
fn listen_inodes(port: u16) -> Vec<String> {
    let mut inodes = Vec::new();
    for f in ["/proc/net/tcp", "/proc/net/tcp6"] {
        let Ok(text) = std::fs::read_to_string(f) else {
            continue;
        };
        for line in text.lines().skip(1) {
            let cols: Vec<&str> = line.split_whitespace().collect();
            // local_address is column 1 as `HEX:HEX`, st is column 3, inode is 9.
            if cols.len() < 10 {
                continue;
            }
            if cols[3] != "0A" {
                continue; // not LISTEN
            }
            let Some((_, p)) = cols[1].rsplit_once(':') else {
                continue;
            };
            if u16::from_str_radix(p, 16).ok() != Some(port) {
                continue;
            }
            inodes.push(cols[9].to_string());
        }
    }
    inodes
}

/// Is anything listening on this TCP port? What [`super::monitor::Watch::Port`] asks each
/// tick. It reads the kernel's own socket table and never a process name, so no string here can
/// match the shell asking.
#[cfg(not(target_os = "macos"))]
pub fn port_is_listening(port: u16) -> bool {
    !listen_inodes(port).is_empty()
}

/// macOS: there is no `/proc/net/tcp`. `netstat -anv -p tcp` is the kernel's socket
/// table for every user, without root, and its `-v` columns carry `process:pid`.
#[cfg(target_os = "macos")]
pub fn port_is_listening(port: u16) -> bool {
    !netstat_listeners(port).is_empty()
}

/// The `LISTEN` rows on `port`, as the pid each names (`None` where the column
/// could not be read).
#[cfg(target_os = "macos")]
fn netstat_listeners(port: u16) -> Vec<Option<u32>> {
    let Ok(out) = std::process::Command::new("/usr/sbin/netstat")
        .args(["-anv", "-p", "tcp"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    parse_netstat_listeners(&String::from_utf8_lossy(&out.stdout), port)
}

#[cfg(any(target_os = "macos", test))]
fn parse_netstat_listeners(text: &str, port: u16) -> Vec<Option<u32>> {
    let mut found = Vec::new();
    for line in text.lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 6 || !cols[0].starts_with("tcp") {
            continue;
        }
        let Some(state) = cols.iter().position(|c| *c == "LISTEN") else {
            continue;
        };
        // `127.0.0.1.8080`, `*.8080`, `::1.8080`: the port is after the LAST dot.
        let Some((_, p)) = cols[3].rsplit_once('.') else {
            continue;
        };
        if p.parse::<u16>().ok() != Some(port) {
            continue;
        }
        let pid = cols[state + 1..]
            .iter()
            .find_map(|c| c.rsplit_once(':').and_then(|(_, n)| n.parse::<u32>().ok()));
        found.push(pid);
    }
    found
}

/// The pid holding a listening TCP socket on `port`, by inode. The second step can legitimately
/// fail: reading another user's `fd` directory is refused by the kernel, so `None` means *not
/// resolvable from here* and never *nothing is listening*.
#[cfg(target_os = "macos")]
pub fn listener_pid(port: u16) -> Option<u32> {
    netstat_listeners(port).into_iter().flatten().next()
}

#[cfg(not(target_os = "macos"))]
pub fn listener_pid(port: u16) -> Option<u32> {
    let inodes = listen_inodes(port);
    if inodes.is_empty() {
        return None;
    }
    let wanted: Vec<String> = inodes.iter().map(|i| format!("socket:[{i}]")).collect();
    let rd = std::fs::read_dir("/proc").ok()?;
    for e in rd.flatten() {
        let name = e.file_name();
        let Some(pid) = name.to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let Ok(fds) = std::fs::read_dir(e.path().join("fd")) else {
            continue; // another user's process: not resolvable from here
        };
        for fd in fds.flatten() {
            if let Ok(target) = std::fs::read_link(fd.path())
                && wanted.iter().any(|w| target.to_string_lossy() == *w)
            {
                return Some(pid);
            }
        }
    }
    None
}

/// The processes in a scope, as [`Reaped`] rows, for a listing that wants to show
/// what is in a cgroup without killing it.
pub fn peek(tree: &dyn ScopeTree, scope: &ScopeId) -> Vec<Reaped> {
    tree.members(scope)
        .unwrap_or_default()
        .into_iter()
        .map(Reaped::observe)
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_netstat_listen_row_names_its_port_and_pid() {
        let t = "Proto Recv-Q Send-Q  Local Address  Foreign Address  (state) rxbytes txbytes rhiwat shiwat process:pid state\n\
tcp6 0 0 2003:c8:a714:850.50213 2600:1901:0:9e23.443 ESTABLISHED 1 2 3 4 claude:3068 00102\n\
tcp46 0 0 *.18765 *.* LISTEN 0 0 131072 131072 Python:34982 00000\n\
tcp4 0 0 127.0.0.1.8080 *.* LISTEN 0 0 131072 131072 llama-server:77 00000\n";
        assert_eq!(super::parse_netstat_listeners(t, 18765), vec![Some(34982)]);
        assert_eq!(super::parse_netstat_listeners(t, 8080), vec![Some(77)]);
        assert!(super::parse_netstat_listeners(t, 443).is_empty());
    }

    use super::*;

    #[test]
    fn the_parent_chain_is_read_past_a_comm_containing_spaces() {
        // The parse everybody gets wrong once: field 2 is `(name)` and the name may
        // hold spaces and parentheses.
        let me = std::process::id();
        let p = parent_of(me);
        assert!(p.is_some(), "this process has a parent");
        assert_ne!(p, Some(me));
    }

    /// The `letibot-deadline` threads alive in this process, by their kernel name (truncated
    /// to 15 bytes, so `letibot-deadlin`).
    fn deadline_threads() -> std::collections::BTreeSet<String> {
        std::fs::read_dir("/proc/self/task")
            .map(|d| {
                d.flatten()
                    .filter(|t| {
                        std::fs::read_to_string(t.path().join("comm"))
                            .is_ok_and(|c| c.trim_end().starts_with("letibot-deadl"))
                    })
                    .map(|t| t.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// A dropped host takes its deadline watchdog with it. The watchdog was an endless loop
    /// holding its own `Arc<Deadlines>`, so every host ever built left a thread behind for the life
    /// of the process. The `Weak` frees exactly when that thread leaves `run`; the tid is checked too.
    #[test]
    fn a_dropped_host_takes_its_deadline_thread_with_it() {
        let before = deadline_threads();
        let h =
            HostProcesses::with_tree("/tmp", Box::new(super::super::scope::NoScopes::new("test")));
        let weak = Arc::downgrade(&h.deadlines);
        // Ours is among the threads that appeared; other tests may have added their own. A thread
        // names itself once it runs, so a loaded box can show it a moment after the spawn.
        let mut started: Vec<String> = Vec::new();
        let named_by = Instant::now() + Duration::from_secs(5);
        while started.is_empty() && Instant::now() < named_by {
            started = deadline_threads().difference(&before).cloned().collect();
            if started.is_empty() {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        assert!(
            !started.is_empty() || cfg!(not(target_os = "linux")),
            "a host starts a watchdog"
        );
        drop(h);
        let until = Instant::now() + Duration::from_secs(10);
        while weak.upgrade().is_some() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            weak.upgrade().is_none(),
            "**the watchdog outlived its host** — the thread still holds the deadlines, so it is \
             still running, and one is left behind per HostProcesses for the process's life"
        );
        // And by tid: the thread is gone from the process. (Threads other tests started in the
        // same window may still be alive, so this asserts that not every new one survived.)
        let until = Instant::now() + Duration::from_secs(10);
        let alive = |started: &[String]| {
            let now = deadline_threads();
            started.iter().filter(|t| now.contains(*t)).count()
        };
        while !started.is_empty() && alive(&started) == started.len() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            started.is_empty() || alive(&started) < started.len(),
            "no deadline thread ended after its host was dropped: {started:?}"
        );
    }

    #[test]
    fn the_harness_protects_itself_without_being_asked() {
        let h =
            HostProcesses::with_tree("/tmp", Box::new(super::super::scope::NoScopes::new("test")));
        let p = h.protected();
        assert!(
            p.iter().any(|e| e.pid == std::process::id()),
            "the process evaluating the command must be in the protected set"
        );
        // And the reason is a sentence a refusal can quote, not a category.
        let me = p.iter().find(|e| e.pid == std::process::id()).unwrap();
        assert!(me.why.contains("evaluating"), "{}", me.why);
        assert!(
            p.len() > 1,
            "the parent chain is protected too, not just self"
        );
    }

    #[test]
    fn a_listener_resolves_to_the_pid_holding_it() {
        // Bind a real socket so the test depends on nothing running on this box. Guard the fact,
        // not the proxy: the assertion is that the pid found is OURS.
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = l.local_addr().unwrap().port();
        match listener_pid(port) {
            Some(pid) => assert_eq!(pid, std::process::id()),
            // /proc may be unreadable in some sandboxes; that is a different fact from "nothing is
            // listening", and the function says so by returning None rather than a wrong pid.
            None => eprintln!("listener_pid: /proc/<pid>/fd not readable here"),
        }
    }

    #[test]
    fn a_host_with_no_scope_tree_refuses_to_spawn() {
        // Fail closed: a process with no owner is the leak, so there is no path from "no cgroups"
        // to "run it anyway".
        let h = HostProcesses::with_tree(
            "/tmp",
            Box::new(super::super::scope::NoScopes::new("no cgroup v2 here")),
        );
        let e = h
            .spawn(&SpawnRequest {
                command: "true".into(),
                // Nobody named this run: the substrate's own test, not an agent's work.
                slug: None,
                cwd: ".".into(),
                scope: ScopeKind::Turn,
                scope_name: None,
                background: false,
                env: vec![],
                // No terminal: these are the substrate's own tests, not an operator's run.
                tty: false,
                memory: None,
            })
            .unwrap_err();
        assert!(format!("{e}").contains("no cgroup v2 here"), "{e}");
        assert!(format!("{e}").contains("nothing would reap it"), "{e}");
    }

    /// **A scope a settled job left a process in is looked at again.** The waiter thread returns
    /// after its one look, and nothing else in the tree revisits a job's scope - so without this a
    /// session that started a background server per checkout keeps one cgroup directory each.
    #[test]
    fn a_scope_a_job_left_a_process_in_is_released_by_the_next_spawn() {
        let root = std::env::temp_dir().join(format!("letibot-kept-scope-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("a root");
        let h = HostProcesses::with_tree(&root, PlainTree::over(root.join("tree")));
        let tree = Arc::clone(&h.tree);
        let scope = tree
            .open(ScopeKind::Session, "job-kept", None)
            .expect("a scope");
        // A command that left something behind: this test's own pid stands in for the child, and
        // the process has to be a live one, or the scope is empty and goes at once.
        std::fs::write(
            scope.path.join("cgroup.procs"),
            std::process::id().to_string(),
        )
        .expect("a member");
        settle_scope(tree.as_ref(), &h.kept_scopes, &scope);
        assert!(
            scope.path.exists(),
            "a scope still holding a process is somebody's live work and must not be ended"
        );
        assert_eq!(
            h.kept_scopes.lock().expect("kept").len(),
            1,
            "the scope that stayed is kept for the next look"
        );

        // The process goes, and the next spawn is what notices.
        std::fs::write(scope.path.join("cgroup.procs"), "").expect("an empty scope");
        h.spawn(&SpawnRequest {
            command: "true".into(),
            slug: None,
            cwd: "/".into(),
            scope: ScopeKind::Session,
            scope_name: None,
            background: true,
            env: vec![],
            tty: false,
            memory: None,
        })
        .expect("a job runs");
        assert!(
            !scope.path.exists(),
            "a kept scope nothing revisits is one cgroup directory per command a session ever ran"
        );
        assert!(
            h.kept_scopes.lock().expect("kept").is_empty(),
            "the kept list does not grow for ever"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A directory per scope and a `cgroup.procs` a test writes into: what a kernel would answer,
    /// on a box with no cgroup v2 to delegate. The join script writes a real pid, so the spawn path
    /// under test is the host's own rather than a fake of it.
    struct PlainTree {
        root: PathBuf,
    }

    impl PlainTree {
        fn over(root: PathBuf) -> Box<dyn ScopeTree> {
            let _ = std::fs::create_dir_all(&root);
            Box::new(PlainTree { root })
        }
    }

    impl ScopeTree for PlainTree {
        fn describe(&self) -> String {
            format!("plain directories under {}", self.root.display())
        }

        fn root(&self) -> Option<PathBuf> {
            Some(self.root.clone())
        }

        fn open(
            &self,
            kind: ScopeKind,
            name: &str,
            parent: Option<&ScopeId>,
        ) -> Result<ScopeId, ExecError> {
            let base = parent
                .map(|p| p.path.clone())
                .unwrap_or_else(|| self.root.clone());
            let path = base.join(format!("{}.{name}", kind.as_str()));
            std::fs::create_dir_all(&path)
                .map_err(|e| ExecError::Spawn(format!("{}: {e}", path.display())))?;
            std::fs::write(path.join("cgroup.procs"), b"")
                .map_err(|e| ExecError::Spawn(format!("{}: {e}", path.display())))?;
            Ok(ScopeId {
                kind,
                name: name.to_string(),
                path,
            })
        }

        /// What the kernel would answer: the pids the join script wrote, less the ones that have
        /// exited - the kernel drops a pid from the file when its process goes.
        fn members(&self, scope: &ScopeId) -> Result<Vec<u32>, ExecError> {
            let body = std::fs::read_to_string(scope.path.join("cgroup.procs"))
                .map_err(|e| ExecError::NoSuchScope(format!("{}: {e}", scope.path.display())))?;
            Ok(body
                .lines()
                .filter_map(|l| l.trim().parse().ok())
                .filter(|pid| super::super::procs::read(*pid).is_some())
                .collect())
        }

        /// Nothing is signalled here, on purpose: a test stands its own pid in for the process a
        /// command left behind, and a fake that killed it would take the test with it.
        fn end(&self, scope: &ScopeId) -> Reaping {
            let observed = self
                .members(scope)
                .unwrap_or_default()
                .into_iter()
                .map(Reaped::observe)
                .collect();
            let removed = std::fs::remove_dir_all(&scope.path).is_ok();
            Reaping {
                scope: scope.clone(),
                at: SystemTime::now(),
                mechanism: "nothing to kill",
                observed,
                survivors: Vec::new(),
                waited: Duration::ZERO,
                removed,
                note: None,
            }
        }

        fn migrate(&self, _from: &ScopeId, _to: &ScopeId) -> Result<Migration, ExecError> {
            Err(ExecError::Spawn("this tree cannot move a process".into()))
        }

        fn list(&self) -> Vec<ScopeId> {
            Vec::new()
        }
    }

    /// **The record of what a host did is bounded, and the oldest go first**: a reaping, a promotion
    /// and a deadline fired are read back by a caller, so they cannot keep the pids of every kill the
    /// daemon ever made.
    #[test]
    fn the_records_of_what_a_host_did_are_bounded_and_the_oldest_go_first() {
        // The rule, on its own: past the limit the FRONT goes, because the newest record is the one a
        // caller asking *what just happened* wants.
        let mut v: Vec<u32> = (0..10).collect();
        bound_records(&mut v, 4);
        assert_eq!(v, vec![6, 7, 8, 9]);
        let mut short: Vec<u32> = vec![1, 2];
        bound_records(&mut short, 4);
        assert_eq!(short, vec![1, 2], "under the limit nothing is dropped");

        // And through the door a caller uses: a real host, one more promotion than it keeps.
        let Some(_) = letibot_tokencore::apparatus::present(
            "a process-lifetime tree (cgroup v2; process groups on macOS)",
            letibot_tools::host_tree().is_ok(),
        ) else {
            return;
        };
        let root = std::env::temp_dir().join(format!("letibot-records-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let h = HostProcesses::new(&root).expect("this box has a cgroup v2 tree");
        let scope = |name: &str| super::ScopeId {
            kind: ScopeKind::Session,
            name: name.into(),
            path: root.join(name),
        };
        for n in 0..RECORD_LIMIT + 3 {
            h.record_promotion(Promotion {
                job: JobId(format!("job-{n}")),
                command: "sleep 1".into(),
                slug: None,
                from: scope("before"),
                to: scope("after"),
                ran_for: Duration::ZERO,
                how: Backgrounding::Asked,
                migration: None,
                at: std::time::SystemTime::now(),
                note: None,
                output_file: None,
            });
        }
        let kept = h.promotions();
        assert_eq!(kept.len(), RECORD_LIMIT, "the record is held to its bound");
        assert_eq!(
            kept.first().map(|p| p.job.0.as_str()),
            Some("job-3"),
            "the three oldest went: {}",
            kept.first().map(|p| p.job.0.clone()).unwrap_or_default()
        );
        assert_eq!(kept.last().map(|p| p.job.0.as_str()), Some("job-514"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Settled output is held to a budget, oldest released first, and a running job's never.
    /// Without it a host kept every job's whole capture for its life, so a session's job table grew
    /// with every `bash` it ever ran.
    #[test]
    fn settled_captures_are_held_to_a_budget_and_the_oldest_go_first() {
        let Some(_) = letibot_tokencore::apparatus::present(
            "a process-lifetime tree (cgroup v2; process groups on macOS)",
            letibot_tools::host_tree().is_ok(),
        ) else {
            return;
        };
        let root = std::env::temp_dir().join(format!("letibot-budget-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let h = HostProcesses::new(&root).expect("this box has a cgroup v2 tree");
        let spawn = |command: &str| {
            h.spawn(&SpawnRequest {
                command: command.into(),
                slug: None,
                cwd: "/".into(),
                scope: ScopeKind::Session,
                scope_name: None,
                background: true,
                env: vec![],
                tty: false,
                memory: None,
            })
            .expect("a cgroup v2 tree was measured above")
        };
        // Three settled jobs of 1000 bytes each, and one still running.
        let settled: Vec<JobId> = (0..3)
            .map(|_| {
                let id = spawn("head -c 1000 /dev/zero");
                let _ = h.wait_job(&id, Duration::from_secs(30));
                id
            })
            .collect();
        let running = spawn("head -c 1000 /dev/zero; sleep 30");
        let until = Instant::now() + Duration::from_secs(10);
        while h.output(&running, 0, 4096).map(|o| o.retained).unwrap_or(0) < 1000
            && Instant::now() < until
        {
            std::thread::sleep(Duration::from_millis(10));
        }

        h.release_settled_captures(2500);
        let held = |id: &JobId| h.output(id, 0, 4096).expect("the row stays").retained;
        assert_eq!(
            held(&settled[0]),
            0,
            "the oldest settled capture is released"
        );
        assert_eq!(held(&settled[1]), 1000);
        assert_eq!(held(&settled[2]), 1000);
        assert_eq!(
            held(&running),
            1000,
            "a running job's output is never released"
        );
        let gone = h.output(&settled[0], 0, 4096).unwrap();
        assert_eq!(
            (gone.produced, gone.dropped),
            (1000, 1000),
            "a released capture still says what was written, and that it was dropped"
        );
        let _ = h.kill_job_as(&running, "test");
        std::fs::remove_dir_all(&root).ok();
    }

    /// **A row is about 1.1 KB and nothing ever removed one.** What is kept is the running jobs and
    /// the newest window of finished ones, and a finish younger than the grace whatever the window
    /// says: a watcher polls every five seconds and would lose the news of a row that went.
    #[test]
    fn finished_jobs_are_kept_to_a_window_and_a_running_one_is_never_dropped() {
        let h = HostProcesses::with_tree(
            "/tmp",
            Box::new(crate::exec::scope::NoScopes::new(
                "this test starts no process: the rows are made by hand",
            )),
        );
        let add = |id: &str, settle: Option<JobState>| {
            let scope = ScopeId {
                kind: ScopeKind::Session,
                name: format!("s-{id}"),
                path: PathBuf::from(format!("/nonexistent/{id}")),
            };
            let job = Arc::new(Job::new(
                JobId(id.into()),
                "true".into(),
                None,
                Lifetime {
                    scope: scope.clone(),
                    owner: scope,
                    background: None,
                },
                "/".into(),
                0,
                1024,
                crate::exec::jobs::Stdin::none(),
            ));
            if let Some(state) = settle {
                job.settle(state);
            }
            h.jobs.lock().expect("jobs").push(Arc::clone(&job));
            h.by_id
                .lock()
                .expect("jobs by id")
                .insert(job.id.clone(), job);
        };
        // Three hundred commands a session ran, and five still going.
        for i in 0..300 {
            add(&format!("done-{i}"), Some(JobState::Exited { code: 0 }));
        }
        for i in 0..5 {
            add(&format!("running-{i}"), None);
        }

        // A job that has only just finished is kept whatever the window says.
        assert_eq!(
            h.prune_finished(KEPT_FINISHED_JOBS, FINISHED_JOB_GRACE, SystemTime::now()),
            0,
            "a fresh finish is kept: a watcher has not read its settlement yet"
        );
        assert_eq!(h.jobs().len(), 305);

        // An hour later the window is the rule, and it is counted from the newest.
        let later = SystemTime::now() + Duration::from_secs(3600);
        let dropped = h.prune_finished(KEPT_FINISHED_JOBS, FINISHED_JOB_GRACE, later);
        assert_eq!(
            dropped,
            300 - KEPT_FINISHED_JOBS,
            "the finished rows past the window"
        );
        assert_eq!(
            h.jobs().len(),
            KEPT_FINISHED_JOBS + 5,
            "the window and the running five"
        );
        assert_eq!(
            h.by_id.lock().expect("jobs by id").len(),
            h.jobs().len(),
            "the two tables name the same jobs, or `find` answers about rows `jobs()` has not got"
        );
        for i in 0..5 {
            assert!(
                h.find(&JobId(format!("running-{i}"))).is_some(),
                "a running job is never dropped"
            );
        }
        for i in 299 - KEPT_FINISHED_JOBS + 1..300 {
            assert!(
                h.find(&JobId(format!("done-{i}"))).is_some(),
                "the newest finished rows are the window"
            );
        }
        assert!(
            h.find(&JobId("done-0".into())).is_none(),
            "the oldest finished row went"
        );
        // And a second pass at the same clock drops nothing: the bound is the state, not a count.
        assert_eq!(
            h.prune_finished(KEPT_FINISHED_JOBS, FINISHED_JOB_GRACE, later),
            0
        );
    }

    /// A run with no deadline has its file from the spawn, and `from` is 0 because nothing had
    /// been printed yet, not because a constant said so. A run that still has a deadline gets none:
    /// the 8 MiB window is what bounds a runaway `yes`. The ring here is sixteen bytes.
    #[test]
    fn a_background_job_has_a_file_from_its_spawn_and_a_foreground_one_does_not() {
        // The cgroup is apparatus, the same skip the tests around this one make.
        let Some(_) = letibot_tokencore::apparatus::present(
            "a process-lifetime tree (cgroup v2; process groups on macOS)",
            letibot_tools::host_tree().is_ok(),
        ) else {
            return;
        };
        let root = std::env::temp_dir().join(format!("letibot-spawn-spill-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        // A fixture scratch directory, never the box's own: the same rule the promotion tests
        // follow, and the reason `set_scratch_dir` takes one at all.
        let scratch =
            std::env::temp_dir().join(format!("letibot-spill-dir-{}", std::process::id()));
        std::fs::create_dir_all(&scratch).unwrap();
        let h = HostProcesses::new(&root)
            .expect("this box has a cgroup v2 tree")
            .with_capture_bytes(16);
        h.set_scratch_dir(&scratch);

        let spawn = |command: &str, background: bool| {
            h.spawn(&SpawnRequest {
                command: command.into(),
                slug: None,
                cwd: "/".into(),
                scope: ScopeKind::Session,
                scope_name: None,
                background,
                env: vec![],
                tty: false,
                memory: None,
            })
            .expect("a cgroup v2 tree was measured above")
        };

        // Four bytes, then twenty: twenty-four into a sixteen-byte ring, so the ring drops its
        // first eight and the file must not.
        let id = spawn("printf aaaa; printf bbbbbbbbbbbbbbbbbbbb", true);
        let _ = h.wait_job(&id, Duration::from_secs(30));

        let view = h.job(&id).expect("the row stays");
        let f = view
            .output_file
            .expect("a job with no deadline has a file from its spawn");
        assert!(
            f.path.ends_with(&format!("job-{id}.out")),
            "the file is named after the job: {}",
            f.path
        );
        assert!(
            f.path.starts_with(&scratch.display().to_string()),
            "the file belongs in the session's scratch directory: {}",
            f.path
        );
        assert_eq!(
            f.from, 0,
            "the file's first byte is the job's first byte — nothing had been dropped when it \
             was opened"
        );

        // Read from the disk, not through the harness: a test that asked the tool whether it
        // wrote is a test of the tool's opinion of itself.
        let on_disk = std::fs::read(&f.path).expect("the scratch file exists");
        assert_eq!(
            on_disk,
            b"aaaabbbbbbbbbbbbbbbbbbbb".to_vec(),
            "the file holds every byte from the job's first, and the ring did not bound it"
        );
        assert_eq!(
            f.bytes, 24,
            "the count is the file's own, and it is final once the drains have joined"
        );

        // The window still dropped the front, and says so: the two are different facts, and the
        // file is the answer to the first one.
        let slice = h.output(&id, 0, 1024).expect("the row stays");
        assert_eq!((slice.dropped, slice.retained), (8, 16));
        assert_eq!(
            slice.text(),
            "bbbbbbbbbbbbbbbb",
            "the window is the last sixteen bytes and nothing else"
        );

        // A run that still has a deadline keeps the window as its only copy: an unbounded file
        // behind every `bash` call would remove the bound the 8 MiB window exists to be.
        let foreground = spawn("true", false);
        let _ = h.wait_job(&foreground, Duration::from_secs(30));
        assert!(
            h.job(&foreground)
                .expect("the row stays")
                .output_file
                .is_none(),
            "a deadline-bounded run gets its file from the promotion, not from the spawn"
        );

        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&scratch).ok();
    }

    /// A promotion of a run that already has a file keeps it, byte for byte. `spill_to`
    /// truncates, so a second promotion that opened the file again would cut a run's output at the
    /// moment somebody asked where the rest of it goes. This job is in the "already there" arm.
    #[test]
    fn a_promotion_does_not_truncate_the_file_a_background_job_already_has() {
        let Some(_) = letibot_tokencore::apparatus::present(
            "a process-lifetime tree (cgroup v2; process groups on macOS)",
            letibot_tools::host_tree().is_ok(),
        ) else {
            return;
        };
        let root = std::env::temp_dir().join(format!("letibot-repromote-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let scratch =
            std::env::temp_dir().join(format!("letibot-repromote-out-{}", std::process::id()));
        std::fs::create_dir_all(&scratch).unwrap();
        let h = HostProcesses::new(&root).expect("this box has a cgroup v2 tree");
        h.set_scratch_dir(&scratch);

        // It prints, then waits: the file has bytes in it and a descriptor still open, which is
        // the state a truncation would be invisible in until somebody read the file.
        let id = h
            .spawn(&SpawnRequest {
                command: "printf 'kept'; sleep 30".into(),
                slug: None,
                cwd: "/".into(),
                scope: ScopeKind::Session,
                scope_name: None,
                background: true,
                env: vec![],
                tty: false,
                memory: None,
            })
            .expect("a cgroup v2 tree was measured above");
        let until = Instant::now() + Duration::from_secs(10);
        while h.output(&id, 0, 64).map(|o| o.retained).unwrap_or(0) == 0 && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
        }
        let before = h
            .job(&id)
            .and_then(|v| v.output_file)
            .expect("a run with no deadline has its file from the spawn");
        assert_eq!(before.bytes, 4, "the four bytes it has printed so far");

        let p = h
            .promote(
                &id,
                ScopeKind::Session,
                None,
                Backgrounding::Operator {
                    identity: "test".into(),
                },
            )
            .expect("a promotion of a job this host owns");
        let after = p
            .output_file
            .expect("the promotion still answers with the file");
        assert_eq!(
            after.path, before.path,
            "the same file, not a second one beside it"
        );
        assert_eq!(
            after.bytes, 4,
            "the promotion did not truncate a file the run is writing"
        );
        assert_eq!(
            std::fs::read(&after.path).expect("the file is still there"),
            b"kept".to_vec()
        );

        let _ = h.kill_job_as(&id, "test");
        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&scratch).ok();
    }

    /// A name is unique within the session, and it never moves: two jobs the caller named
    /// `tests` are `tests` and `tests-2`. And a request that named nothing gets nothing: no
    /// derivation, no fallback word.
    #[test]
    fn a_slug_is_made_unique_within_the_session_and_never_invented() {
        // The cgroup is apparatus: a container with no delegatable cgroup v2 subtree is a fact
        // about the machine and not a failure of this claim.
        let Some(_) = letibot_tokencore::apparatus::present(
            "a process-lifetime tree (cgroup v2; process groups on macOS)",
            letibot_tools::host_tree().is_ok(),
        ) else {
            return;
        };
        let root = std::env::temp_dir().join(format!("letibot-slug-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let h = HostProcesses::new(&root).expect("this box has a cgroup v2 tree");
        let spawn = |slug: Option<&str>| {
            h.spawn(&SpawnRequest {
                command: "true".into(),
                slug: slug.map(|s| s.to_string()),
                cwd: "/".into(),
                scope: ScopeKind::Session,
                scope_name: None,
                background: true,
                env: vec![],
                tty: false,
                memory: None,
            })
            .expect("a cgroup v2 tree was measured above")
        };
        let slug_of = |id: &JobId| h.job(id).expect("a job this host just made").slug;

        let first = spawn(Some("tests"));
        // Settled before the next one is named: a name that came free when its job ended
        // would point at two jobs the moment a reader looked the first one up again.
        let _ = h.wait_job(&first, Duration::from_secs(30));
        let second = spawn(Some("tests"));
        let third = spawn(Some("tests"));
        let unnamed = spawn(None);

        assert_eq!(slug_of(&first).as_deref(), Some("tests"));
        assert_eq!(slug_of(&second).as_deref(), Some("tests-2"));
        assert_eq!(slug_of(&third).as_deref(), Some("tests-3"));
        assert_ne!(first, second, "two jobs, two handles");
        assert_eq!(
            slug_of(&unnamed),
            None,
            "nobody named this run, so it has no name — not `tests-4` and not a guess"
        );
        // Stable for the job's life: nothing above moved a name that was already taken.
        assert_eq!(slug_of(&first).as_deref(), Some("tests"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_bash_env_planted_in_the_parent_never_reaches_the_child() {
        // The spawn clears the environment before setting its own pairs, so nothing planted in
        // this process, `BASH_ENV` included, can reach the child, and `PATH` is the pinned one.
        // The cgroup is apparatus: a container without a delegatable subtree has none to give.
        let Some(_) = letibot_tokencore::apparatus::present(
            "a process-lifetime tree (cgroup v2; process groups on macOS)",
            letibot_tools::host_tree().is_ok(),
        ) else {
            return;
        };
        let root = std::env::temp_dir().join(format!("letibot-r10-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        // Edition 2024: touching the process environment is unsafe, and this is the one test that
        // needs to. `env_clear()` in the spawn path undoes it for every child this host starts.
        unsafe {
            std::env::set_var("BASH_ENV", "/tmp/letibot-absent.rc");
            std::env::set_var("SHELLOPTS", "xtrace");
        }
        let h = HostProcesses::new(&root).expect("this box has a cgroup v2 tree");
        let id = h
            .spawn(&SpawnRequest {
                // Both printf and the `${var-}` expansion are builtins, so the assertion does not
                // depend on anything outside the pinned PATH.
                command: "printf 'be=%s\\n' \"${BASH_ENV-}\"; printf 'path=%s\\n' \"$PATH\"".into(),
                // Nobody named this run: the substrate's own test.
                slug: None,
                cwd: "/".into(),
                scope: ScopeKind::Turn,
                scope_name: None,
                background: false,
                // A session's env configuration must not re-arm what the clear removed: the four
                // names are filtered even when asked for by name.
                env: vec![("BASH_ENV".into(), "/tmp/letibot-absent.rc".into())],
                // No terminal: the substrate's own test, not an operator's run.
                tty: false,
                memory: None,
            })
            .unwrap();
        let waited = h.wait_job(&id, std::time::Duration::from_secs(30)).unwrap();
        let Waited::Happened { state, .. } = waited else {
            panic!("the job did not finish inside 30s: {waited:?}");
        };
        assert_eq!(
            state,
            Some(JobState::Exited { code: 0 }),
            "the child exited nonzero"
        );
        let out = h.output(&id, 0, 4096).unwrap();
        let text = String::from_utf8(out.bytes).unwrap();
        // Restored before anything can fail, because the variables are the whole process's:
        // every other test that starts a shell without clearing its environment inherited
        // `SHELLOPTS=xtrace` from here, where `/bin/sh` is bash and honours it.
        unsafe {
            std::env::remove_var("BASH_ENV");
            std::env::remove_var("SHELLOPTS");
        }
        assert!(text.starts_with("be=\npath=/"), "{text}");
        std::fs::remove_dir_all(&root).ok();
    }

    /// The operator's own run can be ANSWERED: the command reads its stdin, the handle comes out
    /// of the job the spawn returned, and a line goes in. The assertion is the exact capture, so
    /// the typed `Y` must not be echoed, and with `tty: false` the send must be refused instead.
    #[test]
    fn the_operators_own_run_has_a_stdin_a_line_can_be_sent_to() {
        // The cgroup is apparatus: a container without a delegatable subtree has none to give,
        // which is a fact about the machine and not a failure of this claim.
        let Some(_) = letibot_tokencore::apparatus::present(
            "a process-lifetime tree (cgroup v2; process groups on macOS)",
            letibot_tools::host_tree().is_ok(),
        ) else {
            return;
        };
        let Some(_) = letibot_tokencore::apparatus::present(
            "/bin/bash, the shell the operator's own run is handed to",
            std::path::Path::new("/bin/bash").exists(),
        ) else {
            return;
        };
        let root = std::env::temp_dir().join(format!("letibot-stdin-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let h = HostProcesses::new(&root)
            .expect("this box has a cgroup v2 tree")
            .with_console_shell(vec![
                "/bin/bash".to_string(),
                "--norc".to_string(),
                "--noprofile".to_string(),
                "-ic".to_string(),
            ]);
        // `read` is a builtin in every shell this host can be configured with, so the assertion does
        // not depend on anything outside the pinned PATH, the same rule the test above keeps.
        let command = "read answer; printf 'answered=%s\\n' \"$answer\"";
        let spawn = |tty: bool| {
            h.spawn(&SpawnRequest {
                command: command.into(),
                // Nobody named this run: the substrate's own test.
                slug: None,
                cwd: "/".into(),
                scope: ScopeKind::Turn,
                scope_name: None,
                background: false,
                env: vec![],
                tty,
                memory: None,
            })
            .unwrap()
        };

        // ---- The operator's own run: its own terminal, and the line reaches the command.
        let id = spawn(true);
        // And the host can NAME the run's processes, the other half of the detection: `ask`
        // reads each one's `/proc/<pid>/fd/0` and its `wchan`, so a host that cannot list them would
        // make every operator run look like one that is not asking (`ask::Waiting::Unreadable`).
        let pids = h.job_pids(&id);
        assert!(
            !pids.is_empty(),
            "a spawned job's own cgroup must name its processes"
        );
        let handle = h.job_handle(&id).expect("the job the spawn returned");
        let stdin = handle.stdin();
        assert!(
            stdin.is_open(),
            "the operator's own run must have a stdin the daemon holds"
        );
        stdin.send_line("Y").expect("the line reaches the command");
        let waited = h.wait_job(&id, std::time::Duration::from_secs(30)).unwrap();
        let Waited::Happened { state, .. } = waited else {
            panic!("the answered command did not finish inside 30s: {waited:?}");
        };
        assert_eq!(
            state,
            Some(JobState::Exited { code: 0 }),
            "the command read the line and exited"
        );
        let text = h.output(&id, 0, 4096).unwrap().text();
        assert_eq!(
            text, "answered=Y\n",
            "the answer must reach the command's own stdin and must NOT be echoed back into \
             the run's output: {text:?}"
        );

        // ---- The control: a model's run is unchanged, and the send says so.
        let id = spawn(false);
        let handle = h.job_handle(&id).expect("the job the spawn returned");
        let stdin = handle.stdin();
        assert!(
            !stdin.is_open(),
            "a run that is not the operator's must keep /dev/null on stdin"
        );
        let refused = stdin.send_line("Y").unwrap_err();
        assert!(
            refused.contains("/dev/null"),
            "the refusal must name what is missing: {refused}"
        );
        // And it got EOF rather than waiting, which is the older rule intact: the command
        // ran `read` against `/dev/null` and exited at once.
        let waited = h.wait_job(&id, std::time::Duration::from_secs(30)).unwrap();
        assert!(
            matches!(waited, Waited::Happened { .. }),
            "a model's run must not wait for a person who is not there: {waited:?}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// A tree that opens a scope as a plain directory and kills nothing: the by-id map is what
    /// this test is about, and a real cgroup is apparatus it does not need.
    struct DirScopes(std::path::PathBuf);

    impl super::super::scope::ScopeTree for DirScopes {
        fn describe(&self) -> String {
            "a directory per scope".into()
        }
        fn open(
            &self,
            kind: ScopeKind,
            name: &str,
            _parent: Option<&ScopeId>,
        ) -> Result<ScopeId, ExecError> {
            let path = self.0.join(format!("{}.{}", kind.as_str(), name));
            std::fs::create_dir_all(&path)
                .map_err(|e| ExecError::Spawn(format!("{}: {e}", path.display())))?;
            Ok(ScopeId {
                kind,
                name: name.to_string(),
                path,
            })
        }
        fn members(&self, _scope: &ScopeId) -> Result<Vec<u32>, ExecError> {
            Ok(Vec::new())
        }
        fn end(&self, scope: &ScopeId) -> Reaping {
            Reaping {
                scope: scope.clone(),
                at: SystemTime::now(),
                mechanism: "nothing (a directory per scope)",
                observed: Vec::new(),
                survivors: Vec::new(),
                waited: Duration::ZERO,
                removed: false,
                note: None,
            }
        }
        fn migrate(&self, from: &ScopeId, _to: &ScopeId) -> Result<Migration, ExecError> {
            Err(ExecError::Spawn(format!(
                "a directory per scope cannot migrate out of {from}"
            )))
        }
        fn list(&self) -> Vec<ScopeId> {
            Vec::new()
        }
    }

    /// **A job is found by its id, at every door it comes in by.** The host adopted a record
    /// another daemon left, and its row has to be where a read looks: a handle the table does not
    /// hold is one every wait and kill fails to find.
    #[test]
    fn an_adopted_job_is_found_by_its_id() {
        let dir = std::env::temp_dir().join(format!("letibot-by-id-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the scope root");
        let h = HostProcesses::with_tree("/tmp", Box::new(DirScopes(dir.clone())));
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .stdout(Stdio::null())
            .spawn()
            .expect("a child to adopt");
        let pid = child.id();
        let start = super::super::procs::read(pid)
            .expect("the child is in /proc")
            .start;
        let adopted = h
            .adopt(&Adopted {
                handle: "h-adopted".into(),
                command: "sleep 30".into(),
                slug: Some("adopted".into()),
                cwd: "/tmp".into(),
                pid,
                identity: format!("start:{start}"),
                scope: "session.test".into(),
                // What a daemon's record can say: the cgroup died with that process, so the
                // directory is re-derived from the scope's name, and the job is found by id.
                scope_dir: None,
                owner_dir: None,
            })
            .expect("a recorded job is adopted");
        let row = h
            .job(&adopted)
            .expect("the job this host adopted is found by its id");
        assert_eq!(row.command, "sleep 30");
        assert_eq!(row.pid, pid);
        assert!(
            h.job_handle(&adopted).is_some(),
            "and the job itself, not only a row built from it"
        );
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A root for one test, and a host over it: a directory per scope, because a real cgroup is
    /// apparatus these three facts do not need.
    fn host_and_root(tag: &str) -> (HostProcesses, PathBuf) {
        let dir = std::env::temp_dir().join(format!("letibot-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the scope root");
        (
            HostProcesses::with_tree("/tmp", Box::new(DirScopes(dir.clone()))),
            dir,
        )
    }

    /// A live process to hand over, and the identity a record would carry for it.
    fn a_live_child() -> (std::process::Child, u32, String) {
        let child = std::process::Command::new("sleep")
            .arg("30")
            .stdout(Stdio::null())
            .spawn()
            .expect("a child to adopt");
        let pid = child.id();
        let start = super::super::procs::read(pid)
            .expect("the child is in /proc")
            .start;
        (child, pid, format!("start:{start}"))
    }

    /// A hand-over must not take back a job nothing can identify. `survivors` invents an empty
    /// identity where `/proc` could not be read, and `""` never matches `start:<n>` - so the
    /// heir's watcher settled a LIVE process `Dead` on its first look, 500 ms later.
    #[test]
    fn a_job_with_no_readable_identity_is_refused_rather_than_settled_dead() {
        let (h, dir) = host_and_root("by-no-identity");
        let (mut child, pid, _) = a_live_child();
        let refused = h.adopt(&Adopted {
            handle: "h-blind".into(),
            command: "sleep 30".into(),
            slug: Some("blind".into()),
            cwd: "/tmp".into(),
            pid,
            identity: String::new(),
            scope: "explicit.blind".into(),
            scope_dir: None,
            owner_dir: None,
        });
        assert!(
            refused.is_err(),
            "a job nothing can identify was taken back anyway, so its watcher will settle a live \
             process Dead: {refused:?}"
        );
        // And nothing is left behind wearing its handle, for a reader to believe.
        std::thread::sleep(Duration::from_millis(700));
        assert!(
            h.job(&JobId("h-blind".into())).is_none(),
            "a refused adoption still put a row in the host's table"
        );
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An adopted job is a job of this host like any other, so it gets the same output ring.
    /// `adopt` built it with a zero-length one, and every byte that arrived was dropped.
    #[test]
    fn an_adopted_job_keeps_the_output_it_is_given() {
        let (h, dir) = host_and_root("adopted-ring");
        let (mut child, pid, identity) = a_live_child();
        let id = h
            .adopt(&Adopted {
                handle: "h-ring".into(),
                command: "make".into(),
                slug: None,
                cwd: "/tmp".into(),
                pid,
                identity,
                scope: "session.test".into(),
                scope_dir: None,
                owner_dir: None,
            })
            .expect("a recorded job is adopted");
        let job = h.job_handle(&id).expect("the job this host adopted");
        job.capture
            .lock()
            .expect("capture")
            .push(b"the build says what it did\n");
        let window = h.output(&id, 0, 100).expect("the window onto it");
        assert_eq!(
            window.text(),
            "the build says what it did\n",
            "an adopted job dropped its own output: {window:?}"
        );
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// One host is not two: `adopt` answers the id it already holds, and `hand_over` then forgets
    /// the only record there is, leaving a running process in no table at all.
    #[test]
    fn a_hand_over_to_the_same_host_leaves_the_job_where_it_is() {
        let (h, dir) = host_and_root("hand-over-to-itself");
        let (mut child, pid, identity) = a_live_child();
        let id = h
            .adopt(&Adopted {
                handle: "h-explicit".into(),
                command: "sleep 30".into(),
                slug: Some("cache-warm".into()),
                cwd: "/tmp".into(),
                pid,
                identity,
                scope: "explicit.cache-warm".into(),
                scope_dir: Some(dir.join("job-h-explicit").display().to_string()),
                owner_dir: Some(dir.join("explicit.cache-warm").display().to_string()),
            })
            .expect("a recorded job is adopted");
        assert_eq!(
            h.survivors().len(),
            1,
            "an explicit-scope job is a survivor"
        );
        let moved = hand_over(&h, &h);
        assert_eq!(moved.len(), 1, "{moved:?}");
        assert!(
            moved[0].refused.is_some(),
            "a hand-over with one host as both ends is not a hand-over: {moved:?}"
        );
        assert!(
            h.job(&id).is_some(),
            "the only record of a running process was forgotten, and the process runs on"
        );
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
