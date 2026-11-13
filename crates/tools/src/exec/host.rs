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
    DEAD_KILL, DEADLINE_KILL, Due, Job, JobId, JobState, Lifetime, MemoryCap, OutputFile,
    OutputSlice,
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
    /// Append the environment trailer to this call, so it reports what it exported on the
    /// way out ([`super::shell::oneshot_trailer`]). The caller asks for it only where the
    /// session's environment is observed, and never for a run with a terminal.
    pub env_trailer: bool,
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
        /// Not one status query about the thing was answered during the wait, so the wait learnt
        /// nothing about it: a loaded host, not a stuck job. [`crate::builtins::jobs::UNANSWERED_STATUS`]
        /// is how a result says so, and the progress check reads it.
        silent: bool,
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
    /// The environment report this job's trailer took, once it did. `None` for every run that
    /// was not given one and for one whose trailer has not arrived; the harness observes the
    /// `Ok` pairs and treats an `Err` as a report it cannot believe.
    pub env_report: Option<Result<String, String>>,
    /// What its cgroup holds at this instant: the processes in it and what they are, what it has
    /// burned, and how recently the kernel killed one of them at its cap. `None` for a job that is
    /// not running (its cgroup is gone), for a job of jobs (its work is on other machines) and for
    /// a backend that cannot read a cgroup at all.
    pub holding: Option<super::scope::Holding>,
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
    /// One clause about this job's memory, or `None` unless there is something to say. A job that
    /// is RUNNING always says what it holds: a cap it is living at is the fact a reader needs, and
    /// `peak` alone cannot tell that job from one the cap ended. An ended job says nothing unless
    /// the cap is what ended it - the peak on every row was a number to size work by.
    pub fn memory_clause(&self) -> Option<String> {
        match (&self.memory_cap, &self.memory) {
            (MemoryCap::Uncapped { asked, why }, _) => Some(format!(
                "UNCAPPED ({} was asked for: {why})",
                super::scope::human_bytes(*asked)
            )),
            (_, Some(u)) if self.state.is_running() => Some(self.live_memory_clause(u)),
            (_, Some(u)) if u.killed_at_cap() => {
                Some(format!("{} — KILLED at the cap", u.clause()))
            }
            // A job that ended well inside its cap says nothing about memory: the peak on
            // every row was a number to size work by, which the model is not given.
            (_, Some(u)) if u.near_its_cap() => Some(format!("{} — near its cap", u.clause())),
            (_, Some(_)) | (MemoryCap::Capped(_), None) | (MemoryCap::NotAsked, None) => None,
        }
    }

    /// `17 GiB now, peak 32 GiB of a 32 GiB cap — 591 processes killed at the cap, last 12.0s
    /// ago`. The word KILLED belongs to an ending: a running job at its cap is one the kernel is
    /// killing processes out of, and calling that KILLED is the sentence that lost a fuzz job.
    fn live_memory_clause(&self, u: &super::scope::MemoryUse) -> String {
        let mut s = u.live_clause();
        if u.oom_kills > 0 {
            let n = u.oom_kills;
            s.push_str(&format!(
                " — {n} {} killed at the cap",
                if n == 1 { "process" } else { "processes" }
            ));
            if let Some(since) = self.holding.as_ref().and_then(|h| h.oom_since) {
                s.push_str(&format!(", last {} ago", super::scope::span(since)));
            }
        }
        if u.at_its_cap() {
            s.push_str("; it is at its cap and still running");
        }
        s
    }

    /// The state as a row says it. A running job whose leader is gone is still running: the
    /// cgroup is what a job is here, so the row says both facts rather than the word alone - and
    /// the processes in it are the ones a `ps` from inside the sandbox could never show.
    pub fn state_phrase(&self) -> String {
        match &self.holding {
            Some(h) if self.state.is_running() && !h.leader_alive && h.processes > 0 => {
                format!(
                    "running (leader gone, {n} {} still in it)",
                    if h.processes == 1 {
                        "process"
                    } else {
                        "processes"
                    },
                    n = h.processes
                )
            }
            _ => self.state.word(),
        }
    }

    /// The warning a row carries for a running job that has written nothing for a long time.
    /// `since_last_output` is `None` for a job that never wrote, so the time it has been running
    /// is the one that counts: a fuzz campaign that prints nothing is what this exists for.
    pub fn silence_warning(&self) -> Option<String> {
        if !self.state.is_running() {
            return None;
        }
        let quiet = self.since_last_output.unwrap_or(self.elapsed);
        if quiet < SILENT_WARNING {
            return None;
        }
        Some(match self.since_last_output {
            Some(_) => format!(
                "nothing written for {} — it is still running, so read the `cgroup` and `cpu` \
                 lines before deciding it is stuck",
                super::scope::span(quiet)
            ),
            None => format!(
                "nothing written in the {} it has been running — that is not proof it is stuck, \
                 but read the `cgroup` and `cpu` lines",
                super::scope::span(quiet)
            ),
        })
    }

    /// What this job is called: [`job_label`], which is this method's body. Four
    /// readers need the same string, and a settlement has the name and the id but no
    /// view to hang a method on.
    pub fn label(&self) -> String {
        job_label(self.slug.as_deref(), &self.id.0)
    }
}

/// How long a running job may write nothing before its row says so. A build compiles quietly and
/// a corpus takes a while, but half an hour of silence is a fact a reader must be told rather than
/// left to notice.
pub const SILENT_WARNING: Duration = Duration::from_secs(30 * 60);

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

    /// Bound a RUNNING job from now, or move the bound it already has, and answer the limit it now
    /// carries. `None` when this host cannot bound this job — a job of jobs carries its branches'
    /// own deadline — so a caller never announces a deadline nothing enforces.
    fn extend_deadline(&self, job: &JobId, after: Duration) -> Option<Duration> {
        let _ = (job, after);
        None
    }

    /// The warning a running job is owed before the dead check ends it, TAKEN so it is said once.
    /// `None` before four fifths of the window, and on a host with no dead check.
    fn take_dead_warning(&self, job: &JobId) -> Option<DeadWarning> {
        let _ = job;
        None
    }

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
    /// The daemon is restarting: keep every background job that runs on its own output file in
    /// a session or explicit scope, and end the rest as [`ProcessHost::end_running`] does.
    fn detach_for_restart(&self, by: &str) -> (Vec<super::detach::Detached>, Vec<Reaping>) {
        (Vec::new(), self.end_running(by))
    }

    /// Take back a job an earlier daemon detached, on the proof [`super::detach::prove`] finds.
    /// `Err` is the sentence for a job nothing proves, which is never adopted.
    fn reattach(
        &self,
        d: &super::detach::Detached,
    ) -> Result<(JobId, super::detach::Reattached), String> {
        Err(format!(
            "`{}` cannot be taken back by this backend: only a host with a process tree can",
            d.handle
        ))
    }

    /// End a scope: kill everything under it, and record it.
    fn end_scope(&self, scope: &ScopeId) -> Result<Reaping, ExecError>;

    /// The argv a pty program is started inside, the boundary a `bash` call gets. `None` is a
    /// host that cannot run a pty program here, so a caller says so and starts nothing.
    fn terminal_argv(
        &self,
        _cwd: &std::path::Path,
        _env: &[(String, String)],
    ) -> Option<Result<Vec<String>, String>> {
        None
    }

    /// Move a running job into a scope that outlives the turn, and record what moved: the one verb behind
    /// the runtime's threshold and a person promoting from a head. A [`Promotion`], never a bool. The frame
    /// that would let a head ask is not on the wire: a new `ClientFrame` variant bumps `PROTOCOL_VERSION`:
    ///
    /// ```text
    /// ClientFrame::PromoteJob {
    ///     client_request_id: String,   // as every other acting frame carries
    ///     expected_seq: u64,           // as Prompt and Interrupt do
    ///     job: String,                 // the job id, from ToolEvent::Progress
    ///     identity: String,            // WHO - this becomes Backgrounding::Operator
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

    /// The paths a command could not write although they are inside the view: a path bound in
    /// read-only is absent from nowhere, so its refusal is the only sign of the boundary.
    fn refused_write_paths(&self, _output: &str) -> Vec<std::path::PathBuf> {
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

    /// Whether this job carries a deadline somebody armed. `job_extend` reads it back rather than
    /// trusting that arming a job of jobs did something.
    fn armed_for(&self, job: &JobId) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .any(|d| &d.job.id == job)
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

/// The dead check: a running job is ended only when it is DEAD - no output and no CPU progress for
/// the whole window - because a wall clock cannot tell a fuzz campaign from a wedged `sleep`. A job
/// that has been given a deadline is skipped here: the caller asked for that clock, and two answers
/// to *when is it out of time* is one too many.
pub struct Liveness {
    /// The window, or `None` when the operator turned the check off. Behind a lock because the
    /// daemon sets it after the host is built and every tool shares the host.
    window: Mutex<Option<Duration>>,
    /// Every job being checked, in spawn order. A job that has ended, or been given a deadline, is
    /// dropped or reset by the tick that finds it so.
    watching: Mutex<Vec<Progress>>,
    /// Rung when a job is registered or the window changes, so the tick re-reads rather than polls.
    added: Condvar,
    /// The one warning each job is owed before a dead check ends it, taken by whoever delivers it.
    warnings: Mutex<Vec<DeadWarning>>,
    /// What the check ended, for the same reason [`Deadlines::fired`] exists.
    ended: Mutex<Vec<DeadEnded>>,
    closed: std::sync::atomic::AtomicBool,
}

/// One watched job, and the last moment either of the two signals moved.
struct Progress {
    job: Arc<Job>,
    alive_at: Instant,
    bytes: u64,
    /// `cpu.stat`'s microseconds, or `None` on a host that cannot say - which is never a kill.
    cpu: Option<u64>,
    warned: bool,
}

/// The warning a job whose clock is running out is owed, once. Taken by the reader that says it, so
/// a watcher that polls reads it once however often it looks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeadWarning {
    pub job: JobId,
    /// How long neither signal has moved.
    pub silent_for: Duration,
    /// The window it is measured against - the number the sentence quotes.
    pub window: Duration,
}

/// One job the dead check ended, as a record rather than a side effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeadEnded {
    pub job: JobId,
    /// How long the run had been alive.
    pub after: Duration,
    /// How long neither signal had moved - the reason it was called dead.
    pub silent_for: Duration,
    pub reaping: Reaping,
}

impl Liveness {
    /// The default window: thirty minutes of neither output nor CPU. A build or a fuzz campaign
    /// prints nothing between two steps and burns CPU throughout, which is exactly the case this
    /// must not end; a wedged `sleep` burns nothing and prints nothing, which is the one it must.
    pub const DEFAULT_AFTER: Duration = Duration::from_secs(1800);

    /// An empty set, with the tick thread already running.
    fn new(tree: Arc<dyn ScopeTree>) -> Arc<Liveness> {
        let l = Arc::new(Liveness {
            window: Mutex::new(Some(Self::DEFAULT_AFTER)),
            watching: Mutex::new(Vec::new()),
            added: Condvar::new(),
            warnings: Mutex::new(Vec::new()),
            ended: Mutex::new(Vec::new()),
            closed: std::sync::atomic::AtomicBool::new(false),
        });
        let watch = Arc::clone(&l);
        // Said on stderr for the same reason the deadline watchdog is: a check that did not start
        // is a host where only `job_kill` ends anything, and the operator reads that here.
        if let Err(e) = std::thread::Builder::new()
            .name("letibot-liveness".into())
            .spawn(move || watch.run(tree))
        {
            eprintln!(
                "letibot: the dead check did not start ({e}); a background job with no deadline \
                 is ended by `job_kill` and by nothing else"
            );
        }
        l
    }

    /// The window in force, or `None` when the check is off.
    pub fn window(&self) -> Option<Duration> {
        *self.window.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set_window(&self, window: Option<Duration>) {
        *self.window.lock().unwrap_or_else(|e| e.into_inner()) = window;
        self.added.notify_all();
    }

    /// Start watching one job. The clock starts now, which is the spawn: a job that has not spoken
    /// yet has not been silent either.
    fn watch(&self, job: Arc<Job>) {
        let mut g = self.watching.lock().unwrap_or_else(|e| e.into_inner());
        if g.iter().any(|p| p.job.id == job.id) {
            return;
        }
        g.push(Progress {
            job,
            alive_at: Instant::now(),
            bytes: 0,
            cpu: None,
            warned: false,
        });
        drop(g);
        self.added.notify_all();
    }

    /// The next warning, once per job: the reader that says it takes it.
    fn take_warning(&self, job: &JobId) -> Option<DeadWarning> {
        let mut g = self.warnings.lock().unwrap_or_else(|e| e.into_inner());
        let at = g.iter().position(|w| &w.job == job)?;
        Some(g.remove(at))
    }

    pub fn warnings(&self) -> Vec<DeadWarning> {
        self.warnings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// What the check ended, oldest first. The read-back half of the mechanism.
    pub fn ended(&self) -> Vec<DeadEnded> {
        self.ended.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn close(&self) {
        let g = self.watching.lock().unwrap_or_else(|e| e.into_inner());
        self.closed.store(true, std::sync::atomic::Ordering::SeqCst);
        drop(g);
        self.added.notify_all();
    }

    /// The tick. Sleeps a tenth of the window (a tenth of a second at the floor, a minute at the
    /// top), then looks at every job once; returns when its host is gone.
    fn run(&self, tree: Arc<dyn ScopeTree>) {
        loop {
            let window = self.window();
            let wait = match window {
                Some(w) => (w / 10).clamp(Duration::from_millis(100), Duration::from_secs(60)),
                // Off: nothing to time, so the thread waits to be told otherwise rather than
                // waking ten times a second for a check that is not there.
                None => Duration::from_secs(30),
            };
            {
                let g = self.watching.lock().unwrap_or_else(|e| e.into_inner());
                if self.closed.load(std::sync::atomic::Ordering::SeqCst) {
                    return;
                }
                let _ = self
                    .added
                    .wait_timeout(g, wait)
                    .unwrap_or_else(|e| e.into_inner());
            }
            if self.closed.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            if let Some(window) = window {
                self.scan(tree.as_ref(), window);
            }
        }
    }

    /// A job that has been given a deadline is that deadline's business: it is not watched here, so
    /// the clock cannot fire on a job whose ending somebody already chose.
    fn spare(&self, job: &JobId) {
        self.watching
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|p| &p.job.id != job);
    }

    /// One look at every watched job: move the clock of each job whose output or CPU advanced, warn
    /// the one at four fifths of the window, and end the one that has spent the whole of it silent.
    fn scan(&self, tree: &dyn ScopeTree, window: Duration) {
        let now = Instant::now();
        let mut due = Vec::new();
        let mut warn = Vec::new();
        {
            let mut g = self.watching.lock().unwrap_or_else(|e| e.into_inner());
            g.retain_mut(|p| {
                if !p.job.state().is_running() {
                    return false;
                }
                let bytes = p.job.capture.lock().expect("capture").produced();
                let cpu = super::scope::cpu_use_of(&p.job.lifetime().scope);
                // A host that cannot read the clock cannot prove a job dead: `None` is *we cannot
                // say*, and ending a job on it is the wall clock this check exists to end.
                let moved = cpu.is_none() || bytes != p.bytes || p.cpu.is_none() || cpu != p.cpu;
                p.bytes = bytes;
                p.cpu = cpu;
                if moved {
                    p.alive_at = now;
                    p.warned = false;
                }
                match step(moved, now.duration_since(p.alive_at), window, p.warned) {
                    Step::Alive => true,
                    Step::Warn => {
                        p.warned = true;
                        warn.push(DeadWarning {
                            job: p.job.id.clone(),
                            silent_for: now.duration_since(p.alive_at),
                            window,
                        });
                        true
                    }
                    Step::Dead => {
                        due.push((Arc::clone(&p.job), now.duration_since(p.alive_at)));
                        false
                    }
                }
            });
        }
        if !warn.is_empty() {
            self.warnings
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .extend(warn);
        }
        for (job, silent_for) in due {
            // Ended the way `job_kill` ends one, and only while it is still running: a job that
            // exited a moment ago finished, and reaping its cgroup must not be called a kill.
            if !job.state().is_running() {
                continue;
            }
            let after = job.elapsed();
            job.settle(JobState::Killed {
                by: DEAD_KILL.to_string(),
            });
            let reaping = tree.end(&job.lifetime().scope);
            let mut g = self.ended.lock().unwrap_or_else(|e| e.into_inner());
            g.push(DeadEnded {
                job: job.id.clone(),
                after,
                silent_for,
                reaping,
            });
            bound_records(&mut g, RECORD_LIMIT);
        }
    }
}

/// What one look at one job decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// One of the two signals moved: the clock starts again.
    Alive,
    /// Nothing moved for four fifths of the window: say so, once.
    Warn,
    /// Nothing moved for the whole window: it is dead.
    Dead,
}

/// The rule, pure on purpose: a test pins both sides of both edges without sleeping through a
/// window, and a job that is printing or burning CPU is never the harness's to end.
fn step(moved: bool, silent: Duration, window: Duration, warned: bool) -> Step {
    if moved {
        return Step::Alive;
    }
    if silent >= window {
        return Step::Dead;
    }
    if !warned && silent * 5 >= window * 4 {
        return Step::Warn;
    }
    Step::Alive
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
    /// The dead check, and the thread that runs it. See [`Liveness`].
    liveness: Arc<Liveness>,
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
    /// The seat's toolchain pointers: also handed to the boundary, which may clear the environment.
    confined_env: Mutex<Vec<(String, String)>>,
    secret_env: Mutex<super::seat::SecretEnv>,
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
    /// How long a spawn waits for its process to show up in its scope before killing it.
    join_deadline: Duration,
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
    /// The share every session scope is throttled at, in bytes; `0` is nobody asked. A bound on
    /// the OWNER and not on a job (that is `memory.max`), so a runaway test is slowed by the kernel
    /// instead of taking the box, and one session's pressure is not another's (`docs/lifecycle.md` G5).
    owner_memory_high: std::sync::atomic::AtomicU64,
    /// Jobs kept across a restart: while any is, dropping this host must not end the session
    /// scope they live in.
    detached: Mutex<Vec<JobId>>,
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

/// How many records of what a host did one host keeps: every reaping, every promotion and every
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

/// What a kill that did not finish says: the pids and what they run, and whose they still are.
fn still_alive_note(pids: &[u32], command: &str) -> String {
    let who: Vec<String> = pids
        .iter()
        .map(|p| {
            let r = super::scope::Reaped::observe(*p);
            format!("{p} {}", r.label())
        })
        .collect();
    format!(
        "NOT killed: still alive after the kill: {} (job `{command}`). It is still the session's to stop.",
        who.join(", ")
    )
}

/// Whether a command line is a local model server. The program name is the evidence, not the
/// port it listens on.
pub fn model_server_command(cmdline: &str) -> bool {
    const NAMES: [&str; 8] = [
        "llama-server",
        "llama.cpp",
        "llama-cpp",
        "mlx_lm",
        "ollama",
        "vllm",
        "koboldcpp",
        "text-generation",
    ];
    let lower = cmdline.to_lowercase();
    NAMES.iter().any(|n| lower.contains(n))
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

/// Release a job's own scope once its leader has gone, and say whether it went. Only an empty one
/// goes: a scope still holding a process is live work and `end` would kill it. The caller remembers
/// the ones that stayed.
fn release_settled_scope(tree: &dyn ScopeTree, scope: &ScopeId) -> bool {
    if !tree.members(scope).is_ok_and(|m| m.is_empty()) {
        return false;
    }
    let _ = tree.end(scope);
    true
}

/// Whether a restart keeps this job: a running background job on its own output file, owned by a
/// scope that outlives a turn.
fn keeps_on_restart(job: &Job) -> bool {
    let life = job.lifetime();
    job.state().is_running()
        && job.follow().is_some()
        && life.background.is_some()
        && matches!(life.owner.kind, ScopeKind::Session | ScopeKind::Explicit)
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
            liveness: Liveness::new(Arc::clone(&tree)),
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
            confined_env: Mutex::new(Vec::new()),
            secret_env: Mutex::new(Default::default()),
            path_prefix: Mutex::new(None),
            monitors: Arc::new(Monitors::new()),
            protected: Mutex::new(Vec::new()),
            capture_bytes: DEFAULT_CAPTURE_BYTES,
            join_deadline: Duration::from_secs(5),
            scratch: Mutex::new(None),
            shell: vec!["/bin/sh".into(), "-c".into()],
            console_shell: super::console::shell(),
            aggregates: Mutex::new(std::collections::HashMap::new()),
            kept_scopes: Arc::new(Mutex::new(Vec::new())),
            session_name: unique("s"),
            owner_memory_high: std::sync::atomic::AtomicU64::new(0),
            detached: Mutex::new(Vec::new()),
        };
        h.protect_self_and_ancestors();
        h
    }

    /// The share each session scope is throttled at (`memory.high`): the owner's bound, applied
    /// where the cgroup is opened because that is the one place it exists. `None` leaves it
    /// unbounded, which is the default; a box that cannot write the file says so once.
    pub fn with_owner_memory_high(self, bytes: Option<u64>) -> Self {
        self.owner_memory_high
            .store(bytes.unwrap_or(0), std::sync::atomic::Ordering::Relaxed);
        self
    }

    /// [`HostProcesses::with_owner_memory_high`] on a host already built and shared: the daemon
    /// wires this after the backend exists, as it does the scratch directory.
    pub fn set_owner_memory_high(&self, bytes: Option<u64>) {
        self.owner_memory_high
            .store(bytes.unwrap_or(0), std::sync::atomic::Ordering::Relaxed);
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

    /// A test on a loaded box raises this so a slow join script is not read as an orphan.
    pub fn with_join_deadline(mut self, deadline: Duration) -> Self {
        self.join_deadline = deadline;
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

    /// A background job's own output file, opened for the child, and its status path beside it.
    /// `None` with no scratch directory, or one that cannot be written: the job keeps the pipes.
    fn follow_for(&self, id: &JobId) -> Option<(Arc<super::detach::Follow>, std::fs::File)> {
        let dir = self
            .scratch
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()?;
        std::fs::create_dir_all(&dir).ok()?;
        let raw = super::detach::raw_path(&dir, &id.0);
        let status = super::detach::status_path(&raw);
        let _ = std::fs::remove_file(&status);
        let file = std::fs::File::options()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&raw)
            .ok()?;
        // Append, so the wrapper and the command's own writes never overwrite each other.
        drop(file);
        let file = std::fs::File::options().append(true).open(&raw).ok()?;
        Some((super::detach::Follow::new(raw, status, 0), file))
    }

    /// Whether a restart would keep this job rather than end it. See [`ProcessHost::detach_for_restart`].
    pub fn keeps_on_restart(&self, id: &JobId) -> bool {
        self.find(id).is_some_and(|j| keeps_on_restart(&j))
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
    pub fn set_confined_env(&self, pairs: Vec<(String, String)>) {
        *self.confined_env.lock().unwrap_or_else(|e| e.into_inner()) = pairs;
    }

    /// Environment every command carries whose values are masked in any output slice.
    pub fn set_secret_env(&self, secrets: super::seat::SecretEnv) {
        *self.secret_env.lock().unwrap_or_else(|e| e.into_inner()) = secrets;
    }

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

    /// Protect the model server on `port`, by what it is: the listener's command must look like
    /// a model server, and a process that is one of this host's jobs is never it. A port alone
    /// names whoever bound it, which may be the session's own `http.server`.
    pub fn protect_model_server(
        &self,
        port: u16,
        why: impl Into<String>,
        outlives: bool,
    ) -> Option<u32> {
        let pid = listener_pid(port)?;
        let is_job = self.jobs.lock().expect("jobs").iter().any(|j| {
            j.pid == pid
                || self
                    .tree
                    .members(&j.lifetime().scope)
                    .is_ok_and(|m| m.contains(&pid))
        });
        if is_job || !model_server_command(&Reaped::observe(pid).cmdline) {
            return None;
        }
        self.protect_with(pid, why.into(), outlives);
        Some(pid)
    }

    /// What a table of jobs does when one of them ends: the settled captures go back and the finished
    /// rows past the window go with them. Called wherever a job ends, so a session that runs a hundred
    /// commands does not keep a hundred rows for the rest of its life.
    fn housekeeping(&self) {
        self.release_settled_captures(SETTLED_CAPTURE_BUDGET);
        self.prune_finished(KEPT_FINISHED_JOBS, FINISHED_JOB_GRACE, SystemTime::now());
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
            // Its bytes were produced on other machines and never passed this host's
            // capture, so there is no trailer to have taken.
            env_report: None,
            // The work is in other machines' cgroups: a count read here would be this host's
            // processes, and there are none of them.
            holding: None,
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
        let state = job.state();
        // Read while it runs and nowhere else: an ended job's cgroup is reaped, and a count of
        // nothing over a job that is over would be a number a reader could act on by mistake.
        let holding = if state.is_running() && !life.scope.path.as_os_str().is_empty() {
            Some(super::scope::holding_of(&life.scope.path, job.pid))
        } else {
            None
        };
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
            state,
            elapsed: job.elapsed(),
            ran_for: job.ran_for(),
            produced: cap.produced(),
            since_last_output: cap.since_last(),
            output_file: cap.output_file(),
            env_report: cap.env_report(),
            holding,
        }
    }

    /// What the deadline watchdog has ended, oldest first. The read-back half of the
    /// mechanism: *the deadline ended this run* is a claim, and a claim nothing can check is the
    /// unfalsifiable zero this substrate refuses everywhere else.
    pub fn deadlines_fired(&self) -> Vec<DeadlineFired> {
        self.deadlines.fired()
    }

    /// How long a job may go with neither output nor CPU before the dead check ends it. `None`
    /// turns the check off; the daemon sets this from `[exec] job_dead_after_seconds`.
    pub fn set_dead_after(&self, window: Option<Duration>) {
        self.liveness.set_window(window);
    }

    pub fn dead_after(&self) -> Option<Duration> {
        self.liveness.window()
    }

    /// The warnings the dead check owes jobs, oldest first. Taken one at a time through
    /// [`ProcessHost::take_dead_warning`], so a watcher that polls says each one once.
    pub fn dead_warnings(&self) -> Vec<DeadWarning> {
        self.liveness.warnings()
    }

    /// What the dead check ended, oldest first - the same read-back [`Deadlines::fired`] is for.
    pub fn dead_ended(&self) -> Vec<DeadEnded> {
        self.liveness.ended()
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
    fn terminal_argv(
        &self,
        cwd: &std::path::Path,
        env: &[(String, String)],
    ) -> Option<Result<Vec<String>, String>> {
        Some(self.terminal_wrap(cwd, env).map_err(|e| e.to_string()))
    }

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
                // The owner's throttle, on the cgroup the moment it exists: a cap on a job kills
                // the job, and this is what keeps one owner from starving the box first.
                let high = self
                    .owner_memory_high
                    .load(std::sync::atomic::Ordering::Relaxed);
                if high > 0
                    && let Err(why) = super::scope::throttle(&id.path, high)
                {
                    super::scope::said_once("owner-memory-high", &why);
                }
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
        let seat_env = self
            .confined_env
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let secret_env = self
            .secret_env
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        // The wide seat is this user's own session, so what every command of it carries (the daemon's
        // socket, the sudo shim, a worktree's build cache) has to cross a boundary that clears the
        // environment. A tight view is another disk and gets only the seat's pairs.
        #[cfg(target_os = "linux")]
        let carried: Vec<(String, String)> = if confined.boundary().is_some_and(|b| b.view.wide) {
            let prefix = self
                .path_prefix
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            let path = match prefix {
                Some(p) => format!("{p}:{}", self.pinned_path),
                None => self.pinned_path.clone(),
            };
            std::iter::once(("PATH".to_string(), path))
                .chain(
                    self.standing_env
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .iter()
                        .filter(|(k, _)| {
                            !matches!(k.as_str(), "BASH_ENV" | "ENV" | "SHELLOPTS" | "BASHOPTS")
                        })
                        .cloned(),
                )
                .collect()
        } else {
            Vec::new()
        };
        #[cfg(not(target_os = "linux"))]
        let carried: Vec<(String, String)> = Vec::new();
        let plan_env: Vec<(String, String)> = carried
            .iter()
            .chain(seat_env.iter())
            .chain(secret_env.0.iter())
            .chain(req.env.iter())
            .cloned()
            .collect();
        let planned = if req.tty && confined.spares_operator() {
            Ok(Vec::new())
        } else {
            confined.wrap(&ConfinePlan {
                cwd: &cwd,
                env: &plan_env,
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
            // The guest's RAM is the bound: capping the host-side client would report a cap
            // on the wrong process, or UNCAPPED for a command that never ran here.
            Some(_) if confined.memory_is_elsewhere() => MemoryCap::NotAsked,
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
        // The waiter owns the token below; this side keeps the path for the refusal that kills a
        // process which never joined its scope.
        let token_path = token.clone();
        // What this spawn hands the command: ONE list, read by the child's environment below and
        // by the trailer, which names what the command started with. Two spellings would let the
        // report compare against an environment the run never had.
        let applied: Vec<(String, String)> = {
            let prefix = self
                .path_prefix
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            let path = match prefix {
                Some(p) => format!("{p}:{}", self.pinned_path),
                None => self.pinned_path.clone(),
            };
            let standing = self
                .standing_env
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            // The four names bash sources unasked are dropped from the session's pairs too, so a
            // command cannot set `BASH_ENV` for itself.
            let keep = |(k, _): &(String, String)| {
                !matches!(
                    k.as_str(),
                    "BASH_ENV" | "ENV" | "SHELLOPTS" | "BASHOPTS" | "PATH"
                )
            };
            // The filter is the session's pairs' alone: run over the pinned `PATH` too, it dropped
            // that as well and every command fell back to the shell's built-in path.
            std::iter::once(("PATH".to_string(), path))
                .chain(
                    standing
                        .into_iter()
                        .chain(plan_env.iter().cloned())
                        .filter(keep),
                )
                .collect()
        };
        // The environment trailer, appended only where the caller armed one, the run has no
        // terminal of its own, and the command parses as a WHOLE program: text after a dangling `|`
        // or an unterminated heredoc would be rewritten by the append, so that command gets no report.
        let whole_program = !letibot_code::shell::normalise(&req.command).partial;
        let trailer = (req.env_trailer && !req.tty && whole_program).then(super::shell::nonce);
        let run_command = match &trailer {
            Some(mark) => {
                // A secret's value stays out: the trailer is text in the child's argv, and an
                // argv is what `ps` shows. Its name then lands in the first pass, which is honest.
                let started_with: Vec<(String, String)> = applied
                    .iter()
                    .filter(|(k, _)| !secret_env.0.iter().any(|(secret, _)| secret == k))
                    .cloned()
                    .collect();
                format!(
                    "{}\n{}",
                    req.command,
                    super::shell::oneshot_trailer(mark, &started_with)
                )
            }
            None => req.command.clone(),
        };
        // A background job writes to a file of its own under the wrapper, so it can outlive this
        // daemon (`super::detach`). Without a scratch directory it keeps the pipes.
        let follow = if req.background && !req.tty {
            self.follow_for(&id)
        } else {
            None
        };
        let mut cmd = std::process::Command::new("/bin/sh");
        // No core dump from this command or anything under it: a run the harness ends for size is
        // an expected ending, and its dump is what filled the unit table (R191, G5).
        super::scope::keep_no_core(&mut cmd);
        cmd.arg("-c")
            .arg(join_script())
            .arg("letibot-scope")
            .arg(&procs)
            .arg(&token);
        // After the join and before the boundary: the status file is the host's, not the view's.
        if let Some((f, _)) = &follow {
            cmd.arg("/bin/sh")
                .arg("-c")
                .arg(super::detach::WRAPPER)
                .arg(super::detach::WRAPPER_NAME)
                .arg(f.status());
        }
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
        cmd.arg(&run_command);
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
        } else if let Some((_, out)) = &follow {
            let err = out.try_clone().map_err(|e| {
                let _ = self.tree.end(&cgroup);
                ExecError::Spawn(format!("the job's output file: {e}"))
            })?;
            let out = out.try_clone().map_err(|e| {
                let _ = self.tree.end(&cgroup);
                ExecError::Spawn(format!("the job's output file: {e}"))
            })?;
            cmd.stdout(out).stderr(err);
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
        // pinned one and a bare name resolves through nothing else. `applied` already dropped the
        // four names bash sources unasked.
        cmd.env_clear();
        for (k, v) in &applied {
            cmd.env(k, v);
        }

        // A confined command's secrets go on a descriptor, not in bwrap's argv (`SECRET_FD`).
        #[cfg(target_os = "linux")]
        let _secrets = if wrap.is_empty() {
            None
        } else {
            super::confine::hand_secrets(&mut cmd, &plan_env).map_err(|e| {
                let _ = self.tree.end(&cgroup);
                ExecError::Spawn(format!(
                    "the secrets could not be handed to the boundary: {e}"
                ))
            })?
        };
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

        let job = Job::new(
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
        .with_memory_cap(memory_cap)
        // Masked as the bytes arrive, so the window and the spill file are one set of bytes.
        .with_secret_mask(&secret_env);
        let job = Arc::new(match &follow {
            Some((f, _)) => job.with_follow(Arc::clone(f)),
            None => job,
        });
        // The parent's copy of the output file goes: the child holds its own.
        drop(follow);
        if let Some(p) = super::procs::read(pid) {
            job.set_identity(format!("start:{}", p.start));
        }
        // Armed after the masker and before the first drain starts, so no byte can reach the
        // ring without first passing the trailer out of the stream.
        if let Some(mark) = &trailer {
            job.arm_env_trailer(mark);
        }

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
        let reader = job
            .follow()
            .and_then(|f| super::detach::spawn_reader(Arc::clone(&job), Arc::clone(f)));

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
                // A job on its own file: the wrapper's exit is its last write, so the reader
                // finishes after it rather than before.
                #[cfg(not(target_os = "macos"))]
                let early = waiter.follow().map(|_| child.wait());
                if let Some(f) = waiter.follow() {
                    f.finish();
                }
                if let Some(h) = reader {
                    let _ = h.join();
                }
                // Kept for the next daemon: its ending is the status file's to tell.
                if waiter.follow().is_some_and(|f| f.is_detached()) {
                    let _ = std::fs::remove_file(&token);
                    return;
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
                let status = match early {
                    Some(s) => s,
                    None => child.wait(),
                };
                // A job already marked `Killed` keeps that: a SIGKILL from a cgroup reap arrives
                // here as a signal, and reporting the signal would lose the fact that we did it.
                if !waiter.state().is_running() {
                    // A job we killed is settled before the waiter sees the signal, so the
                    // launcher's marker is not asked about. Its token goes here too: nothing else
                    // will read it, and a file per killed job is a file per killed job in $TMPDIR.
                    let _ = std::fs::remove_file(&token);
                    if let Some(f) = waiter.follow() {
                        f.discard();
                    }
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
                if let Some(f) = waiter.follow() {
                    f.discard();
                }
                // The leader is gone, so its own scope is debris unless the command left a
                // process in it: one directory per command ever run otherwise, and a scope still
                // holding work is somebody's live process and stays - for the next spawn to look at.
                settle_scope(tree.as_ref(), &kept, &waiter.lifetime().scope);
            })
            .map_err(|e| ExecError::Spawn(e.to_string()))?;

        // Do not return until the process is IN its cgroup, or is finished. Without it, a
        // background run followed by `job_kill` reads an empty cgroup and kills nothing. The loop
        // ends on either half because a fast command can finish before it is ever a member.
        let deadline = Instant::now() + self.join_deadline;
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
            // The wrapper may have written its token before it was killed: a job that never joined
            // is still a job whose evidence file must not outlive it.
            let _ = std::fs::remove_file(&token_path);
            return Err(ExecError::Spawn(format!(
                "pid {pid} started but was still not a member of its cgroup after {:?}; \
                 it was killed rather than left running outside any scope",
                self.join_deadline
            )));
        }

        self.by_id
            .lock()
            .expect("jobs by id")
            .insert(job.id.clone(), Arc::clone(&job));
        // Watched from the spawn: a job with no deadline is ended only when it is dead, and the
        // clock that decides that starts here rather than at its first byte. Not for the OPERATOR's
        // own run - a person's shell waiting for them to type looks exactly like a wedged job, and
        // it is theirs to end.
        if !req.tty {
            self.liveness.watch(Arc::clone(&job));
        }
        self.jobs.lock().expect("jobs").push(job);
        self.release_settled_captures(SETTLED_CAPTURE_BUDGET);
        self.release_kept_scopes();
        self.prune_finished(KEPT_FINISHED_JOBS, FINISHED_JOB_GRACE, SystemTime::now());
        Ok(id)
    }

    /// Take a job the daemon did not start and make it this host's. Recorded directories are used
    /// verbatim, else the cgroup name is derived and the re-open is exact. A vanished process settles
    /// as [`JobState::Dead`], never `Exited`.
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
        job.set_identity(a.identity.clone());
        // The restart lost the tree's memory of this job's processes. Re-find them from the
        // leader's pid and start time, or `job_kill` observes nothing and says the job is gone.
        self.tree.claim(&job.lifetime().scope, a.pid, &a.identity);
        self.by_id
            .lock()
            .expect("jobs by id")
            .insert(id.clone(), Arc::clone(&job));
        self.jobs.lock().expect("jobs").push(Arc::clone(&job));
        // An adopted job is watched on the same rule: it was started by a daemon that is gone, and
        // nothing here knows whether it was given a deadline.
        self.liveness.watch(Arc::clone(&job));
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
                        // A leader that exited while its group lives on is not a dead job.
                        _ if !tree
                            .members(&watched.lifetime().scope)
                            .unwrap_or_default()
                            .is_empty() => {}
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
            // A run with a deadline is that deadline's business: the dead check must not end it
            // first on a clock nobody asked for.
            self.liveness.spare(job);
        }
    }

    /// Bound a running job from now, or move the bound it has. Refused by `None` for a job of jobs,
    /// which carries its branches' own deadlines, and for one that has already ended.
    fn extend_deadline(&self, job: &JobId, after: Duration) -> Option<Duration> {
        self.arm_deadline(job, after);
        // `arm_deadline` is silent about whether it armed anything, so the answer is read back
        // through `Deadlines::armed_for` rather than assumed: a caller must not announce a
        // deadline nothing enforces.
        self.deadlines.armed_for(job).then_some(after)
    }

    fn take_dead_warning(&self, job: &JobId) -> Option<DeadWarning> {
        self.liveness.take_warning(job)
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
                    // An aggregate's own state is this process's: there is no query to time out.
                    silent: false,
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
                // A local job's status is a file this process holds: nothing can go unanswered.
                silent: false,
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
            // A scope of this process's own tree, asked with no network in it.
            silent: false,
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
        // The journal, not a table ([`Aggregate::output`]). Its bytes never pass this host's
        // capture - they were produced on other machines - so this is where they are masked.
        if let Some(agg) = self.aggregate(id) {
            let mut slice = agg.output(from, limit);
            slice.bytes = self
                .secret_env
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .mask(&slice.bytes);
            return Ok(slice);
        }
        // A job's own bytes were masked as they arrived ([`Capture::mask`]), so a slice of them
        // needs nothing more: masking twice would be two copies of one rule.
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
        let scope = job.lifetime().scope;
        // A restart can have emptied the scope's memory of a live process: look again by pid
        // and start time before concluding there is nothing to kill.
        if running && self.tree.members(&scope).unwrap_or_default().is_empty() {
            self.tree.claim(&scope, job.pid, &job.identity());
        }
        let mut r = self.tree.end(&scope);
        if running && !r.survivors.is_empty() {
            job.reopen();
            r.note = Some(still_alive_note(&r.survivors, &job.command));
        }
        if !running && r.observed.is_empty() {
            r.note = Some(format!(
                "`{id}` had already {} when the kill arrived; nothing was running to kill",
                job.state().word()
            ));
        }
        self.housekeeping();
        Ok(self.record(r))
    }

    fn detach_for_restart(&self, by: &str) -> (Vec<super::detach::Detached>, Vec<Reaping>) {
        let running: Vec<JobId> = self
            .jobs()
            .into_iter()
            .filter(|v| v.state.is_running())
            .map(|v| v.id)
            .collect();
        let mut kept = Vec::new();
        let mut ended = Vec::new();
        for id in running {
            let keeps = self
                .find(&id)
                .filter(|j| keeps_on_restart(j))
                .and_then(|j| j.follow().cloned().map(|f| (j, f)));
            let Some((job, f)) = keeps else {
                if let Ok(r) = self.kill_job_as(&id, by) {
                    ended.push(r);
                }
                continue;
            };
            self.liveness.spare(&id);
            let life = job.lifetime();
            let raw_read = f.detach(&job);
            self.detached
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(id.clone());
            kept.push(super::detach::Detached {
                handle: id.0.clone(),
                command: job.command.clone(),
                slug: job.slug.clone(),
                cwd: job.cwd.clone(),
                pid: job.pid,
                identity: job.identity(),
                scope: life.owner.to_string(),
                cgroup: life.scope.path.display().to_string(),
                raw: f.raw().display().to_string(),
                raw_read,
            });
        }
        (kept, ended)
    }

    fn reattach(
        &self,
        d: &super::detach::Detached,
    ) -> Result<(JobId, super::detach::Reattached), String> {
        use super::detach::{Follow, Proof, Reattached};
        let id = JobId(d.handle.clone());
        if self.find(&id).is_some() {
            return Err(format!("`{}` is already this host's job", d.handle));
        }
        let proof = super::detach::prove(d);
        if let Proof::Nothing(why) = proof {
            return Err(why);
        }
        let (kind, name) = scope_kind_of(&d.scope);
        let owner = ScopeId {
            kind,
            name,
            path: PathBuf::from(d.owner_dir()),
        };
        let scope = ScopeId {
            kind,
            name: format!("job-{}", d.handle),
            path: PathBuf::from(&d.cgroup),
        };
        self.tree.remember(&owner);
        self.tree.remember(&scope);
        let raw = PathBuf::from(&d.raw);
        let follow = Follow::new(raw.clone(), super::detach::status_path(&raw), d.raw_read);
        let secret_env = self
            .secret_env
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let job = Job::new(
            id.clone(),
            d.command.clone(),
            d.slug.clone(),
            Lifetime {
                scope: scope.clone(),
                owner,
                background: Some(Backgrounding::Asked),
            },
            d.cwd.clone(),
            d.pid,
            self.capture_bytes,
            super::jobs::Stdin::none(),
        )
        .with_secret_mask(&secret_env)
        .with_follow(Arc::clone(&follow));
        job.set_identity(d.identity.clone());
        // The masked file the old daemon wrote goes on growing from its own end.
        if let Err(why) = job
            .capture
            .lock()
            .expect("capture")
            .resume_spill(super::detach::out_path(&raw))
        {
            eprintln!(
                "letibot: `{}`'s output file could not be reopened: {why}",
                d.handle
            );
        }
        let job = Arc::new(job);
        if let Some(slug) = &d.slug {
            self.claimed_slugs
                .lock()
                .expect("claimed slugs")
                .push(slug.clone());
        }
        self.by_id
            .lock()
            .expect("jobs by id")
            .insert(id.clone(), Arc::clone(&job));
        self.jobs.lock().expect("jobs").push(Arc::clone(&job));
        let reader = super::detach::spawn_reader(Arc::clone(&job), Arc::clone(&follow));
        if let Proof::Ended(code) = proof {
            follow.finish();
            if let Some(h) = reader {
                let _ = h.join();
            }
            job.close_spill();
            let state = JobState::Exited { code };
            job.settle(state.clone());
            follow.discard();
            settle_scope(self.tree.as_ref(), &self.kept_scopes, &scope);
            return Ok((id, Reattached::Ended(state)));
        }
        self.liveness.watch(Arc::clone(&job));
        let tree = Arc::clone(&self.tree);
        let kept = Arc::clone(&self.kept_scopes);
        let watched = Arc::clone(&job);
        // The cgroup is the liveness: the wrapper is not this process's child, so nothing can
        // wait on it, and the job is over when its last process has left.
        std::thread::Builder::new()
            .name(format!("letibot-reattached-{}", id.0))
            .spawn(move || {
                loop {
                    if follow.is_detached() || !watched.state().is_running() {
                        break;
                    }
                    if tree.members(&scope).unwrap_or_default().is_empty() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
                follow.finish();
                if let Some(h) = reader {
                    let _ = h.join();
                }
                if follow.is_detached() {
                    return;
                }
                watched.close_spill();
                if watched.state().is_running() {
                    if let Some(used) = super::scope::memory_use_of(&scope) {
                        watched.record_memory(used);
                    }
                    let state = match super::detach::read_status(follow.status()) {
                        Some(code) => JobState::Exited { code },
                        None => JobState::Dead,
                    };
                    watched.settle(state);
                }
                follow.discard();
                settle_scope(tree.as_ref(), &kept, &scope);
            })
            .map_err(|e| e.to_string())?;
        Ok((id, Reattached::Running))
    }

    fn end_scope(&self, scope: &ScopeId) -> Result<Reaping, ExecError> {
        // Marked before the kill, like `kill_job_as`: the waiter would otherwise settle the
        // job as the signal it saw. A job whose processes survive is reopened below.
        let marked: Vec<Arc<Job>> = self
            .jobs
            .lock()
            .expect("jobs")
            .iter()
            .filter(|j| j.lifetime().scope.path.starts_with(&scope.path) && j.state().is_running())
            .cloned()
            .collect();
        for j in &marked {
            j.settle(JobState::Killed {
                by: format!("the {} scope ending", scope.kind.as_str()),
            });
        }
        let mut r = self.tree.end(scope);
        // Only said once its processes are gone: a survivor is still the session's to stop.
        let mut alive = Vec::new();
        for j in &marked {
            let left = self.tree.members(&j.lifetime().scope).unwrap_or_default();
            if !left.is_empty() {
                j.reopen();
                alive.push(still_alive_note(&left, &j.command));
            }
        }
        if !alive.is_empty() {
            let n = alive.join("; ");
            r.note = Some(match r.note.take() {
                Some(o) => format!("{o}; {n}"),
                None => n,
            });
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
        self.housekeeping();
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

    fn refused_write_paths(&self, output: &str) -> Vec<std::path::PathBuf> {
        self.confine
            .read()
            .expect("confinement")
            .refused_write_paths(output)
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
        // And the dead check, for the same reason: a tick that outlived its jobs has nothing to
        // look at and would keep the host's `Arc` alive for the process's life.
        self.liveness.close();
        let session = self.session.lock().expect("session scope").clone();
        let kept = !self
            .detached
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty();
        if let Some(s) = session
            && !kept
        {
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
#[cfg(test)]
mod tests;
