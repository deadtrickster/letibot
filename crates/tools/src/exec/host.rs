//! [`ProcessHost`] — the seam the tools reach the world through, and the host
//! implementation of it.
//!
//! # What the harness knows that the model cannot
//!
//! `docs/tool-design-brief.md` §2.4, corollary: *the harness owns the substrate,
//! so it knows things the model cannot — its own pid, the parent chain, the pids
//! of the servers it manages … A check the model could have done itself is the
//! weak version.*
//!
//! [`HostProcesses::protected`] is that knowledge as a list. It is populated
//! without being asked with the harness's own pid and its whole parent chain, and
//! a daemon adds what it manages — [`HostProcesses::protect_listener`] resolves a
//! listening port to the pid holding it, which is how "the model server serving
//! this session" becomes a fact rather than a guess. [`super::predicate`] is what
//! reads it.
//!
//! # The wait verb, and why there is one
//!
//! The fleet measured a polling loop written by hand in **seventy-six spellings**
//! before it became one verb. A model that wants to wait for something will
//! compose a loop, and it will compose a different one every time — which is how
//! `until pgrep -f X` gets reinvented, self-match and all.
//!
//! So waiting is a capability here and not a thing to build: [`ProcessHost::wait`]
//! takes a **handle** — a job or a scope — and never a pattern. That is not a
//! guard against T21.2; it makes T21.2 unspellable, because a wait keyed on a
//! cgroup has no predicate that could match its own waiter.
//!
//! Its outcomes are three and they never collapse ([`Waited`]): it happened, the
//! deadline passed quietly and the thing is *still running*, or the thing was
//! never there to begin with. The third exists because of the seat brief's rule —
//! **only count absence after presence** — an empty result before startup is a
//! boot window, not a finish.

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
    /// Why this pid is here, in the words the refusal quotes. Not a category — a
    /// sentence, because the refusal has to be self-correcting and "protected" is
    /// not a reason anyone can act on.
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
    /// **The name the caller gave this work**, or `None` when nobody said one.
    ///
    /// The agent's own stated intent for the run — `bash(…, slug: "release-build")` —
    /// and **never a parse of the command line**. A slug derived from
    /// `W=…; cd /tmp && cargo test …` would be the machine inventing an intent, which is
    /// the defect this tree refuses everywhere else; the command is already on the row,
    /// so a guessed name earns nothing. `None` and not an empty string: *nobody named
    /// this* is a fact, and the tool that reads the argument renders a blank one the
    /// same way ([`crate::builtins::bash`]).
    ///
    /// The host makes it unique within the session at the spawn and never moves it
    /// afterwards; see [`Job::slug`].
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
    /// **Does this run meet the world the operator's own console gives a command?**
    ///
    /// `true` for the OPERATOR's own run — the `!` line and the door's calls, which
    /// are the same ungated path ([`crate::runtime::ToolRuntime::invoke_operator`]) —
    /// and `false` for a model's call. It is a property of **who reads the bytes** and
    /// not of the host or the session: the operator is looking at a screen, where
    /// `ls --color=auto` colouring a directory is the whole point, and the model is
    /// reading tokens, where `ESC[01;34m` around every name is a cost it cannot see.
    ///
    /// **Four things follow from it, and they follow from this one flag.**
    ///
    /// | what | where | why |
    /// |---|---|---|
    /// | a pty on **all three** of stdin, stdout and stderr instead of pipes and `/dev/null` | the spawn below, and [`super::pty`] | a pipe is not a terminal, so `ls --color=auto` prints plain — and the child's terminal has to be its *controlling* one, and its stdin, or bash has no job control |
    /// | their shell, interactive, instead of the host's | [`HostProcesses::with_console_shell`] | the alias that colours `ls` is shell state in a rc file, and no rc is read by `/bin/sh -c` |
    /// | their terminal's variables, and pagers that cannot page | [`super::console::env`], put on the request by `bash` | `TERM` and `LS_COLORS` describe the screen; `less` waits for a keystroke that cannot arrive |
    /// | **the daemon holds a handle on the run's own input, instead of `/dev/null`** | the spawn below, and [`super::jobs::Stdin`] | the fourth thing *a person is there* means: a program that asks a question can be **answered**, and `/dev/null` is an EOF that is not a `Y` |
    ///
    /// It is one flag and not four because it is one fact — *a person typed this and
    /// is looking at a screen* — and four flags with the same value are four things
    /// a later edit can make disagree. [`super::console`] carries the decision, what
    /// it changes, and why a model's call gets none of it.
    ///
    /// **The fourth is the newest and it is the one with a cost.** A model's run reads
    /// `/dev/null` and gets EOF; the operator's reads a pipe and **waits**, which is the
    /// whole point and is also a behaviour change for a command that never wanted an
    /// answer (`! cat`, `! grep x`). It is stated at the spawn and in
    /// [`super::jobs::Stdin`] rather than left to be discovered.
    pub tty: bool,
    /// **The memory cap the caller decided, in bytes** (TODO.md R65), or `None` for no cap —
    /// the operator's own run. The host does not choose a number: what is free, the default
    /// and the ceiling are the tool's business, and the host only makes the kernel hold it.
    pub memory: Option<u64>,
}

/// How a wait ended. **Three outcomes and they are never conflated** — a deadline
/// reported as a completion is F5 exactly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Waited {
    /// It happened: the job left `Running`, or the scope emptied after having been
    /// seen populated.
    Happened {
        state: Option<JobState>,
        /// How long the wait actually took.
        took: Duration,
    },
    /// The deadline passed quietly and the thing is **still running**. Carries the
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
    /// **The memory cap the job runs under** and **what the kernel counted** — live while it
    /// runs (the peak so far), and as recorded at the settle once it has ended. TODO.md R65.
    pub memory_cap: MemoryCap,
    pub memory: Option<super::scope::MemoryUse>,
    pub id: JobId,
    pub command: String,
    /// **The name the caller gave this job**, or `None` when nobody said one — the
    /// stated intent, never a derivation from the command (see [`Job::slug`]).
    ///
    /// On the view because this is what a listing draws and what a tool resolves a
    /// name back to a handle with: a person reading the pane and a model writing
    /// `job_output` both need the same string, and neither may have to rebuild it.
    pub slug: Option<String>,
    /// The job's own cgroup: what `job_kill` ends.
    pub scope: ScopeId,
    /// The scope that reaps it if nobody does: the turn, the session, or a named
    /// explicit scope.
    pub owner: ScopeId,
    pub cwd: String,
    pub pid: u32,
    /// How this job came to be in the background, or `None` if it never was.
    /// **Not a bool**: "the model asked" and "the runtime moved it" are the
    /// difference between a listing the model recognises and one it does not.
    pub background: Option<Backgrounding>,
    pub state: JobState,
    pub elapsed: Duration,
    /// How long the job ran, from its own stamps, or `None` while it is running.
    /// **This** is the number a settlement reports — `elapsed` is a live clock and
    /// keeps counting after the job does not.
    pub ran_for: Option<Duration>,
    pub produced: u64,
    pub since_last_output: Option<Duration>,
    /// **Where the whole of this job's output is also going**, once it was promoted out of the
    /// foreground — or `None` for a job nobody moved (and for one that was already over when
    /// they asked). See [`OutputFile`]: the window above is bounded, this is not, and a reader
    /// told only about the window is a reader who loses a long build's first half.
    pub output_file: Option<OutputFile>,
}

/// **What a job is called**: `release-build · j65`, or just `j65` when nobody named it.
///
/// The one rule, and it is a free function as well as a method for one reason: **a
/// settlement outlives the view it was read from.** `Harness::completion_notice` is handed a
/// [`crate::exec::JobView`]'s two facts and no view — by the time the notice is built the job
/// has usually been reaped, which is exactly why the settlement carries the command — so the
/// sentence that names the job in the transcript has to reach the rule without a table to
/// read it off. [`JobView::label`] is this function on a view, so the daemon's `/job`
/// listing, the `JobEntry` a head draws, a tool's own sentence and the settlement notice
/// cannot say it four ways.
///
/// The name is flattened here as it is everywhere else text becomes a row: a name is one
/// line however it was typed. It is **not** re-cut to a width — the tool that takes a name
/// caps it at its source ([`crate::builtins::bash`]), so a name is short by construction and
/// a second cut would only be a second opinion.
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
    /// **One clause about this job's memory**, or `None` unless there is something to say: it was
    /// killed at its cap, came within a tenth of it, or ran uncapped against an ask — the clause
    /// `job_list` and a completion notice both print.
    pub fn memory_clause(&self) -> Option<String> {
        match (&self.memory_cap, &self.memory) {
            (MemoryCap::Uncapped { asked, why }, _) => Some(format!(
                "UNCAPPED ({} was asked for: {why})",
                super::scope::human_bytes(*asked)
            )),
            (_, Some(u)) if u.killed_at_cap() => {
                Some(format!("{} — KILLED at the cap", u.clause()))
            }
            // R93.F7/C4: a job that ended well inside its cap says nothing about memory — the
            // peak on every row was a number to size work by, which the model is not given.
            (_, Some(u)) if u.near_its_cap() => Some(format!("{} — near its cap", u.clause())),
            (_, Some(_)) | (MemoryCap::Capped(_), None) | (MemoryCap::NotAsked, None) => None,
        }
    }

    /// **What this job is called**: `release-build · j65`, or just `j65` when nobody named
    /// it.
    ///
    /// One rule, because four readers need the same string — the daemon's `/job` listing,
    /// the `JobEntry` a head draws, a tool's own refusal when a name matches nothing, and
    /// the settlement notice the model reads in the transcript. Four spellings of it would
    /// be four answers to *which job is this*, which is the question a name exists to
    /// answer. The rule itself is [`job_label`], which is this method's body: a settlement
    /// has the name and the id but no view to hang a method on.
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
    /// **The name the caller gave this job**, or `None` when nobody said one — for the
    /// same reason [`Promotion::command`] is here: a promotion read back an hour later is
    /// a sentence about a job, and a sentence about a job says what it is called. Read off
    /// the job at the promotion, which is the last moment the name is certain.
    pub slug: Option<String>,
    /// The scope that owned its lifetime before.
    pub from: ScopeId,
    /// The scope that owns it now.
    pub to: ScopeId,
    /// How long it ran in the foreground first. The number the model is shown.
    pub ran_for: Duration,
    pub how: Backgrounding,
    /// `None` when there was nothing to move — a job that had already finished.
    /// Distinguished from a migration that moved zero of three.
    pub migration: Option<Migration>,
    pub at: SystemTime,
    pub note: Option<String>,
    /// **The file the run's output keeps going into from here on**, when this session has a
    /// scratch directory to put it in. `None` where there is nowhere to spill — and then the
    /// run's output is exactly what it was before, in the window.
    ///
    /// On the record rather than said in prose at the call site because it is the promotion's
    /// own answer to *where does the rest of this go*, and every reader of a promotion — the
    /// `bash` result, `job_output`, a `job_list` an hour later — needs the same string.
    pub output_file: Option<OutputFile>,
}

impl Promotion {
    /// Is every process this job still has now owned by the new scope?
    ///
    /// A promotion with nothing to move is **not** complete in the sense that
    /// matters and is not reported as one: the honest reading of "the job had
    /// already exited" is that the promotion did nothing, which
    /// [`Promotion::note`] says.
    pub fn complete(&self) -> bool {
        self.migration.as_ref().is_some_and(|m| m.complete())
    }

    /// The one line a listing draws, in the spelling every other sentence uses:
    /// `release-build · j14` when the caller named it, the bare id when nobody did
    /// ([`job_label`]). The record's identity is still the handle ([`Promotion::job`]); this
    /// is what a reader is shown, and `job_list` draws the rows above it with the same rule.
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
    /// One line for `EXPLAIN`. Must say what it does **not** confine, because a
    /// disclosure that only lists the guarantees reads as a claim about the rest.
    fn describe(&self) -> String;

    fn spawn(&self, req: &SpawnRequest) -> Result<JobId, ExecError>;

    /// **Arm a run's deadline, so that something other than the run enforces it.**
    ///
    /// Required rather than defaulted, and that is deliberate: a default that did nothing
    /// would be a backend where the timeout is silently the caller's problem again, which
    /// is precisely the shape this method exists to end. A backend that cannot arm one
    /// must say so by answering, not by staying quiet.
    ///
    /// The tool that started the run calls this; the host decides who fires it. See
    /// [`Deadlines`] for why the answer cannot be *the thread that is running the command*.
    fn arm_deadline(&self, job: &JobId, after: Duration);

    /// Block on a **handle**, never a pattern. `job` waits for one job to leave
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

    /// The same, with **who ended it** as an argument.
    ///
    /// `by` is what [`JobState::Killed`] carries and what every reader of the row is shown,
    /// so it is a sentence about the cause and not a label: *the model asked*, *its deadline
    /// passed* and *the daemon was stopping* are three different things to have happened to
    /// one command, and a single word for all three would be F5 with the sign flipped — a
    /// caller's decision reported as the command's own.
    fn kill_job_as(&self, job: &JobId, by: &str) -> Result<Reaping, ExecError>;

    /// **Take a job this backend did not start, and make it one of its own.**
    ///
    /// The caller is a restarted daemon: the record is what outlived the process that started the
    /// job (`store::ProcessRecord`), and everything above this layer — `jobs`, `job_output`,
    /// `job_kill`, `job_wait`, the settlement that wakes the model — is written against the
    /// backend's own job table. [`HostProcesses::adopt`] is the local one; a remote host's is the
    /// next slice's, whose answer must be *I cannot* rather than a job object that settles nothing.
    fn adopt(&self, a: &Adopted) -> Result<JobId, ExecError> {
        Err(ExecError::Spawn(format!(
            "this backend cannot adopt `{}`: the record outlived the process that started the \
             job, and only a host that can identify a pid can take one back",
            a.handle
        )))
    }

    /// **End every run this host still has, and say why.**
    ///
    /// The daemon's shutdown is the caller: a stop that arrives while a run holds the
    /// worker must end that run rather than wait out its deadline, because the alternative
    /// is a stop that takes two minutes and a head that has to be told so. The run's own
    /// row is where it is said — it settles as [`JobState::Killed`] carrying `by`, and
    /// `bash` renders that on the row the session keeps.
    ///
    /// Returns one reap record per run ended, so *it ended nothing* and *it ended nothing
    /// because nothing was running* are different answers to the caller that asked.
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

    /// **Move a running job into a scope that outlives the turn**, and record
    /// what actually moved.
    ///
    /// This is the one verb behind all three ways into the background. The model
    /// asking for `background: true` never reaches it — that job is started in
    /// the session scope and has nothing to move — but the runtime's threshold
    /// and a person promoting from a head are the same operation with a
    /// different [`Backgrounding`] on it, which is why `how` is an argument
    /// rather than something the caller writes into prose afterwards.
    ///
    /// Returns a [`Promotion`], never a bool, for the reason
    /// [`super::scope::Migration`] gives: a promotion is a claim about which
    /// scope reaps this work, and a process that did not move makes that claim
    /// false for that process.
    ///
    /// # What the head must send, and why it is not sent yet
    ///
    /// This is the daemon-side verb. The operator-facing third of the
    /// requirement needs one frame, and it is specified here rather than added,
    /// because adding it costs more than it can currently buy:
    ///
    /// ```text
    /// ClientFrame::PromoteJob {
    ///     client_request_id: String,   // as every other acting frame carries
    ///     expected_seq: u64,           // as Prompt and Interrupt do
    ///     job: String,                 // the job id, from ToolEvent::Progress
    ///     identity: String,            // WHO — this becomes Backgrounding::Operator
    /// }
    /// ```
    ///
    /// It goes through the command queue (it acts on a session), it is
    /// **idempotent** — promoting an already-promoted job is answered with the
    /// record saying nothing moved, never refused — and the daemon answers it by
    /// calling this method with [`Backgrounding::Operator`]. The head learns the
    /// job id the same way it learns everything else about a running call: from
    /// the `ToolEvent::Progress` notes `bash` already emits, which name the job.
    ///
    /// **Why it is not in `protocol.rs` on this branch.** A new `ClientFrame`
    /// variant is a wire change, so `PROTOCOL_VERSION` has to go 5 → 6, and both
    /// sides refuse a mismatch by name. Two things follow. First, landing a bump
    /// costs the running daemon its live session (`TODO.md` D11 priced exactly
    /// this for 3 → 4). Second, the constant is a single line that every branch
    /// touching the wire edits, and it was already coordinated once — D10's note
    /// says 5 was taken deliberately *"to coordinate rather than race"*. Racing
    /// it for a verb that **nothing in production can call** would be a
    /// guaranteed conflict buying nothing: `harnessd` seats
    /// [`crate::runtime::roles::m1_orchestrator`], which has no `bash`, no jobs
    /// and therefore no job to promote.
    ///
    /// So the seam is here and tested, and the wire change is one variant and one
    /// integer whenever a head is ready to send it.
    fn promote(
        &self,
        job: &JobId,
        to: ScopeKind,
        name: Option<&str>,
        how: Backgrounding,
    ) -> Result<Promotion, ExecError>;

    /// **Every promotion this session made.** The same falsifier as
    /// [`ProcessHost::reap_log`], one verb over: a promotion that silently moved
    /// nothing and a job that was already where it needed to be are different
    /// facts, and a listing that shows neither cannot tell them apart.
    fn promotions(&self) -> Vec<Promotion>;

    /// Every scope open right now, so a listing can show what is running and for
    /// whom. *"An invisible watcher is an unreapable one."*
    fn scopes(&self) -> Vec<ScopeId>;

    /// **What every ended scope killed.** The falsifier: a reaper whose zero is
    /// unfalsifiable is the empty-haystack bug one layer up.
    fn reap_log(&self) -> Vec<Reaping>;

    /// This session's monitors, or `None` on a host with no monitor mechanism.
    ///
    /// `None` is an honest absence and not an empty list, the same distinction
    /// [`ProcessHost::confinement`] keeps: *"there are no monitors"* and *"this
    /// host cannot watch anything"* are different answers, and a tool that
    /// rendered the second as the first would be reporting a capability the
    /// session does not have.
    fn monitors(&self) -> Option<&Arc<Monitors>> {
        None
    }

    /// The workspace root, for a tool that must turn a relative path into a real
    /// one — [`crate::builtins::monitor`] is the caller.
    ///
    /// `None` on a host with no opinion about where it is, which is a different
    /// fact from a root of `/` and is not rendered as one.
    fn workspace(&self) -> Option<&std::path::Path> {
        None
    }

    /// **Where the whole of a job's output lives, as the HOST spells it** — or `None` when it
    /// lives nowhere but this process's own window ([`JobView::output_file`] is the other half:
    /// a path a reader *here* can open).
    ///
    /// The two are one fact with two readers and they cannot be one field. A remote job's output
    /// is a file on that machine (`<worktree>/.letibot-remote/jobs/<n>.out`), and the supervisor
    /// needs that path because it is what a restarted daemon reads — while `read` and `tail` here
    /// could not open it, which is exactly why it is not [`JobView::output_file`].
    ///
    /// One caller: [`crate::fleet`], which writes a fanout branch's `process` record. A host with
    /// no such file says `None` rather than inventing a path, and the `harnessd` supervisor's
    /// exit-file convention is then simply not there — which is the truth for a job whose status
    /// this box can ask the kernel for.
    fn job_log(&self, job: &JobId) -> Option<String> {
        let _ = job;
        None
    }

    /// The live handle for a job, so a monitor can be declared against the job
    /// itself rather than against an id it would have to look up every tick.
    ///
    /// An id can be re-used by a different host; a handle cannot. A monitor that
    /// silently re-targeted would be watching something nobody asked about.
    fn job_handle(&self, job: &JobId) -> Option<Arc<Job>> {
        let _ = job;
        None
    }

    /// **The processes this job has right now**, by pid, when the host can say.
    ///
    /// One reader: [`super::ask`], whose whole question — *"is the program blocked reading
    /// the answer we hold"* — is asked **of the run's processes**, and which therefore has
    /// to be able to name them. The cgroup is the list: a job's own cgroup holds the shell
    /// that became the command and everything it forked, because membership is inherited.
    ///
    /// **An empty list means *this host cannot say*, and not *nothing is running*.** That
    /// is the distinction [`super::ask::Waiting::Unreadable`] exists for: a host that
    /// cannot name the processes must not be read as having looked and found nobody asking,
    /// or the miss would look exactly like a program that is not asking.
    fn job_pids(&self, job: &JobId) -> Vec<u32> {
        let _ = job;
        Vec::new()
    }

    /// The pids the harness manages, which the model cannot see.
    fn protected(&self) -> Vec<Protected>;

    /// The scope a foreground command belongs to, opened on demand.
    fn scope_for(&self, kind: ScopeKind, name: Option<&str>) -> Result<ScopeId, ExecError>;

    /// **What bounds the view**, for a tool that has to explain its own result.
    ///
    /// `None` means this host has no opinion about views — which is a different
    /// fact from [`super::confine::Unconfined`], where somebody decided not to ask
    /// for one, and different again from
    /// [`super::confine::NoConfinement`], where one was asked for and is missing.
    /// A caller that renders `None` as "not confined" would be asserting a
    /// property of a host it did not read.
    fn confinement_summary(&self) -> Option<String> {
        None
    }

    /// **What a command could not see, read off the boundary.** Three views of one
    /// finding: the sentences the model is given, the same paths for a caller that
    /// can act on them, and whether the launcher itself failed.
    ///
    /// On the trait rather than reached through a borrowed `&dyn Confinement`,
    /// because the confinement is behind a lock — a grant replaces it — and a
    /// borrow cannot outlive the guard. Defaults are empty: a host with no
    /// boundary has nothing to say about absence, which is a different fact from
    /// saying nothing was absent, and `confinement_summary` is where that
    /// distinction is carried.
    fn absence_notes(&self, _output: &str) -> Vec<String> {
        Vec::new()
    }

    fn outside_paths(&self, _output: &str) -> Vec<std::path::PathBuf> {
        Vec::new()
    }

    fn launcher_failure(&self, _output: &str) -> Option<String> {
        None
    }

    /// **Hold a handle that stands for work running somewhere else** — see [`Aggregate`].
    ///
    /// The default is a refusal and not a no-op, because the two are different facts: a
    /// backend that answered `Ok` and then settled nothing would leave a handle a model waits
    /// on for ever. `Err` is *this host cannot do that*, and the caller turns it into a
    /// sentence before it starts the work.
    fn hold(&self, place: &JobId, agg: Arc<dyn Aggregate>) -> Result<(), ExecError> {
        let _ = (place, agg);
        Err(ExecError::Spawn(
            "this backend cannot hold a job of jobs: it has no table of its own for one handle \
             to live in, so a job whose settlement waits on N others would settle nowhere"
                .to_string(),
        ))
    }

    /// **What a settling job wants to say for itself, beyond its state and its numbers.**
    ///
    /// `None` for every ordinary job, and that is the rule rather than an accident: R93.C3
    /// settled that a job's settlement carries its ending and not its output, because the model
    /// can read the output when it wants it. An [`Aggregate`] is the one kind whose *result* is
    /// its output — the table of cells is the answer a fanout was asked for — so it answers here
    /// with that text, and the settlement carries it. A host with no aggregates says `None`.
    fn settlement_note(&self, _job: &JobId) -> Option<String> {
        None
    }

    /// **A name a job of jobs may answer to, made unique against every name this host has
    /// handed out** — the record a `bash` job's `slug` goes through ([`JobView::slug`]), so two
    /// fanouts in one session cannot be reached by one word.
    ///
    /// The default hands the name back unchanged, which is honest for a backend with no such
    /// record: the caller then refuses to hold the handle rather than giving two jobs one name.
    fn claim_name(&self, asked: &str) -> String {
        asked.to_string()
    }
}

/// **A job of jobs** — one handle in the session's own table that stands for work running on
/// other machines, and settles when that work does (FB).
///
/// # Why this exists rather than a composite `ProcessHost`
///
/// A `fanout` branch is a job of the *host's* table, not this session's: it is spawned through a
/// per-host `RemoteProcesses`, so `wait_job` here has never heard of it and `jobwatch` — whose
/// whole job is `wait_job` on this session's host — could not settle it. Making the session's
/// process host a composite over every host it can reach (`SpawnRequest.host`, `JobView.host`,
/// `jobs()` merged, a recorder per host) would fix that and change every caller of the seam.
///
/// This is the smallest honest version of the same fact: **one handle in this host's table, whose
/// `wait_job` waits on N branches**, each on its own host through that host's own process group.
/// `jobs`, `job_output`, `job_kill`, `job_wait`, the activity's `waiting on: job …` and the
/// settlement that wakes the model are then all the code that was already there.
///
/// What it does *not* do is say so after a restart: this table is in memory, so a daemon that
/// restarts loses the parent. The branches are `process` records and are adopted (RH7); the
/// parent's settlement is not. `docs/remote-hosts.md` § RH8 says that plainly.
///
/// # The two halves a caller must get right
///
/// * **`state` is the truth once it is not `Running`**, and `wait` must not report an ending it
///   has not got: a caller that answered `Exited { code: 0 }` for work still in flight would make
///   every wait a lie.
/// * **`output` is a JOURNAL, not a table** — append-only, so an offset stays meaningful while the
///   thing is running (see [`super::jobs::Capture`] for why the ring's offsets are absolute).
pub trait Aggregate: Send + Sync {
    /// The row's command column — what this job of jobs *is*, in one line. There is no single
    /// command line, and inventing one from the first branch would be a claim about all of them.
    fn command(&self) -> String;
    /// **The name the caller would use for it**, if it has one — the same slug a `bash` job may
    /// carry, so `job_output`/`job_kill`/`job_wait` take a word as well as a handle.
    fn slug(&self) -> Option<String>;
    fn state(&self) -> JobState;
    /// How long it has been going, live, or how long it ran.
    fn elapsed(&self) -> Duration;
    /// The same, settled — the number a settlement reports.
    fn ran_for(&self) -> Option<Duration>;
    fn produced(&self) -> u64;
    /// The last time a branch said something, so a wait can report progress and not a bit.
    fn since_last_output(&self) -> Option<Duration>;
    fn output(&self, from: u64, limit: usize) -> OutputSlice;
    /// **Block until it settles**, at most `within`. `None` is *still running*, never a guess.
    fn wait(&self, within: Duration) -> Option<JobState>;
    /// **End every branch, on its own host, through its own process group** — and answer what
    /// was there before the kill, so `job_kill`'s record is a measurement and not a claim.
    fn kill(&self, by: &str) -> Vec<Reaped>;
    /// **The table**, for the settlement that wakes the model — the same text the blocking form
    /// answers with. Empty while it is still running.
    fn report(&self) -> String;
}

/// **The run's deadline, and the thread that enforces it.**
///
/// # Why this is not the run's own loop
///
/// The deadline used to be enforced in exactly one place: the top of `bash`'s wait loop,
/// which runs **on the thread the run occupies**. That is the deadline being enforced
/// *inside the thing it guards*, and it has one consequence that no amount of care in the
/// loop can fix — if that thread does not come back round the loop, nothing ends the run.
/// The worker that ran the command is then held for as long as the command lives, and
/// since the daemon has one worker, so is every session it serves.
///
/// The loop's tick is a *bounded* condvar wait and the run's own processes are waited on by
/// other threads, so the loop is not itself the thing that blocks — measured, 2026-10-06: a
/// root `su` blocked on the device the daemon held for it (a pipe then, its own terminal now)
/// with its whole tree unreadable was killed by its
/// deadline at 120.02 s, with the worker in `futex_do_wait` throughout. What is true, and
/// is the whole of the defect, is that the check is **only** there: it is the only thing
/// that would end the run, so anything that stops that thread reaching it removes the
/// timeout from the system entirely.
///
/// So the deadline gets a thread of its own, it belongs to the **host** rather than to the
/// tool, and what it does is what [`HostProcesses::kill_job`] does: end the run's cgroup.
/// `cgroup.kill` is a write to a file this daemon owns — it is not this uid signalling a
/// stranger's process, so it reaches a root `sudo`, a confined child in another pid
/// namespace and everything a run forked, whether or not the run's own thread is moving.
///
/// # What it does not replace
///
/// The loop keeps its own check. That is the fast path and the one with the precise
/// sentence — it can say *how long* the run outlived its deadline, and it can do it up to
/// half a second earlier. This is the **backstop**, and the two agree because they do the
/// same thing at the same moment; a run ended by either settles as
/// [`JobState::Killed`] with [`DEADLINE_KILL`] as the reason, so one row says what happened
/// however the run was ended.
///
/// # No timer for message delivery
///
/// `daemon.rs`'s §18.1-I12 rule is that the daemon spawns no timer or poll loop **for
/// message delivery**: a hub blocks on a condvar and a signal arrives on a self-pipe. This
/// is neither. It is the one thing a deadline is, and it is armed per run and disarmed by
/// firing, so an idle session has this thread asleep on a condvar and nothing else.
pub struct Deadlines {
    /// Armed deadlines, earliest first by construction of the wait below.
    inner: Mutex<Vec<Due>>,
    /// Rung when a deadline is armed or the list changes, so the watchdog re-reads it
    /// rather than polling.
    armed: Condvar,
    /// What the watchdog did, so a caller can read back *the deadline ended this run* —
    /// the same reason `reaps` exists beside it.
    fired: Mutex<Vec<DeadlineFired>>,
    /// **The host is gone, so the watchdog is too** — set by [`Deadlines::close`] from
    /// `HostProcesses`' `Drop`, under `inner`'s lock so the watchdog cannot miss it between
    /// its check and its wait.
    ///
    /// R67 item 13: `run` was an endless `loop` and the thread held its own `Arc` to this, so
    /// nothing ever ended it — one `letibot-deadline` thread per `HostProcesses` for the life of
    /// the process, which is one of the two threads R66 counted per finished subagent (7 → 19).
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
    /// An empty set, with the watchdog already running.
    fn new(tree: Arc<dyn ScopeTree>) -> Arc<Deadlines> {
        let d = Arc::new(Deadlines {
            inner: Mutex::new(Vec::new()),
            armed: Condvar::new(),
            fired: Mutex::new(Vec::new()),
            closed: std::sync::atomic::AtomicBool::new(false),
        });
        let watch = Arc::clone(&d);
        // **A failure to spawn is not swallowed.** A host whose watchdog could not start is
        // a host with no deadline at all, which is the state this whole type exists to end —
        // so it is said on stderr, where a daemon's operator already reads, and the loop's
        // own check remains the only enforcement exactly as it was before.
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

    /// **End the watchdog** — its host is being dropped, so nothing it could fire for has an
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

    /// **The watchdog.** Sleeps until the earliest armed deadline, then ends that run's
    /// cgroup — on this thread, which is not the run's. Returns when [`Deadlines::close`]
    /// says its host is gone.
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
                        // The earliest is still ahead: sleep exactly that long, and no
                        // longer — a shorter deadline armed meanwhile rings the condvar.
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
                // **The run is ended the same way `job_kill` ends one**, and only while it
                // is still running: a job that exited on its own a moment before its
                // deadline is a job that finished, and reaping its cgroup would report a
                // kill of an empty scope as if it were the deadline's doing.
                if !d.job.state().is_running() {
                    continue;
                }
                let after = d.job.elapsed();
                d.job.settle(JobState::Killed {
                    by: DEADLINE_KILL.to_string(),
                });
                let reaping = tree.end(&d.job.lifetime().scope);
                self.fired
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(DeadlineFired {
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
    /// What bounds the process's **view**, as against its lifetime.
    ///
    /// Not an `Option`. An absent field is a question nobody answered, and the
    /// three answers here are genuinely different —
    /// [`super::confine::Bwrap`] (asked for and measured),
    /// [`super::confine::NoConfinement`] (asked for and missing, so every spawn
    /// refuses) and [`Unconfined`] (not asked for, and loud about it). Making it a
    /// `Box<dyn Confinement>` means the second cannot be spelled as the third by
    /// leaving something out.
    ///
    /// **Behind a lock because a grant replaces it.** A boundary's argv is built
    /// once, at probe time, so that what was measured is what runs — so widening
    /// the view is not an edit to a live namespace, it is a re-probe producing a
    /// new `Confinement` that takes this slot.
    confine: std::sync::RwLock<Box<dyn Confinement>>,
    root: PathBuf,
    /// The turn and session scopes, opened lazily and reused.
    session: Mutex<Option<ScopeId>>,
    turn: Mutex<Option<ScopeId>>,
    /// Every job this host ever ran, in spawn order — the order `jobs()` lists.
    jobs: Mutex<Vec<Arc<Job>>>,
    /// **The same jobs, by id** — R67 item 27. `find` (every `job_wait`, `job_output`,
    /// `job_kill`, deadline arm and pane read) used to scan `jobs`, which only grows: a
    /// session's thousandth `bash` call paid for the nine hundred and ninety-nine before it.
    /// Written beside `jobs` at the one place a job is added ([`HostProcesses::spawn`]).
    by_id: Mutex<std::collections::HashMap<JobId, Arc<Job>>>,
    /// **Every slug this host has handed out**, so the next one is unique.
    ///
    /// A record of its own rather than a scan of `jobs`, because a spawn builds its
    /// `Arc<Job>` — and therefore decides its slug — some way before it pushes it, and
    /// two spawns can be in flight at once (the operator's own `!` run has a thread of
    /// its own). A scan would let both choose the same free name. A name is claimed
    /// once and never released: a slug that two jobs answer to, one of them settled,
    /// would make resolving it a guess, and a name that moved when a job settled would
    /// not identify anything.
    claimed_slugs: Mutex<Vec<String>>,
    /// **The deadlines, and the thread that enforces them.** See [`Deadlines`].
    deadlines: Arc<Deadlines>,
    reaps: Mutex<Vec<Reaping>>,
    /// Every promotion, for the same reason `reaps` exists: a lifetime that
    /// changed under the model is a fact somebody has to be able to read back.
    promotions: Mutex<Vec<Promotion>>,
    /// The `PATH` this host was constructed with, set explicitly on every spawn
    /// after `env_clear()`, so a bare name resolves through a `PATH` fixed at
    /// seat time rather than whatever the environment carries when the command
    /// runs (R10 / `docs/boundary-and-adjudication.md` §5, layer 1).
    pinned_path: String,
    /// What every command of this session carries besides `PATH`: the daemon's
    /// socket and this session's id (so a helper the command runs can find its
    /// way back — `letibot-askpass` does), `SUDO_ASKPASS`, and `HOME`. Set once
    /// by the daemon after construction; the request's own pairs come after and
    /// win. Not the process environment: layer 1 cleared that on purpose.
    standing_env: Mutex<Vec<(String, String)>>,
    /// A directory put in FRONT of the pinned `PATH`: the daemon's shims, so a
    /// `sudo` the session runs is the one that asks the head instead of a tty.
    path_prefix: Mutex<Option<String>>,
    /// The monitors this session declared. **On the host, not beside it**,
    /// because a monitor's owner is one of this host's scopes and the reaping
    /// path has to be able to take its watchers with it.
    monitors: Arc<Monitors>,
    protected: Mutex<Vec<Protected>>,
    /// Bytes retained per job. Beyond this the ring drops from the front and says
    /// so — see [`super::jobs::OutputSlice::denominator`].
    capture_bytes: usize,
    /// **Where a promoted run's output keeps going** — the session's scratch directory, set by
    /// the daemon ([`crate::backend::Backend::with_scratch_dir`]'s host half).
    ///
    /// Behind a lock because the daemon sets it after the host is built and the host is shared
    /// by `Arc` with every tool in the session. `None` on a host nobody wired one to: a
    /// promotion then leaves the run's output exactly where it was — in the window — rather than
    /// inventing a directory.
    scratch: Mutex<Option<PathBuf>>,
    /// The shell a command is handed to.
    shell: Vec<String>,
    /// **The shell the OPERATOR's own command is handed to**, which is theirs and
    /// interactive — see [`super::console::shell`] for the decision and what it
    /// changes. Only a run whose [`SpawnRequest::tty`] is set is handed to it.
    ///
    /// A field rather than a constant at the call site for the same reason [`shell`]
    /// is one: a host that has no shell of its own has no console shell either. The
    /// firecode VM's host is the case — it hands the command to the guest's own
    /// `bash -lc`, so there is nothing here to make interactive, and its constructor
    /// says so with [`HostProcesses::with_console_shell`].
    ///
    /// [`shell`]: HostProcesses::with_shell
    console_shell: Vec<String>,
    /// **The handles that stand for work running elsewhere** — [`Aggregate`], by id.
    ///
    /// A table of its own rather than a field on [`Job`], because a job of jobs has no process: no
    /// cgroup, no waiter thread, no `Child`. Everything above this layer asks the same four
    /// questions of it that it asks of a job (`state`, `output`, `wait`, `kill`), and the two
    /// tables are read together, which is why `jobs()` lists both.
    aggregates: Mutex<std::collections::HashMap<JobId, Arc<dyn Aggregate>>>,
    /// What this host calls its session scope.
    ///
    /// **Unique per host, not the constant `s`.** Two sessions in one daemon share
    /// a cgroup root, so a fixed name would put both sessions' processes in one
    /// cgroup — and then ending either session would reap the other's jobs. The
    /// bug would show up as "somebody else's build died", which is the hardest kind
    /// to trace back to a name.
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

/// **How much SETTLED output one host keeps, across all its jobs** (R68.3) — 64 MiB, eight
/// full captures.
///
/// `jobs` keeps every job the host ever ran (a job's row is what `job_list`, a settlement and a
/// restart's `due_todo_rows` read), and each held its whole capture for the host's life: up to
/// [`DEFAULT_CAPTURE_BYTES`] a job, for a session that ran fifteen thousand of them. The
/// capture of a job that has SETTLED has done its work — its result was handed back when it
/// ended — so past this budget the oldest settled jobs' captures are released
/// ([`super::jobs::Capture::release`]), keeping the row and its counts. A running job's
/// capture is never touched: it is the only copy of output nobody has read yet.
pub const SETTLED_CAPTURE_BUDGET: usize = 64 * 1024 * 1024;

/// **A job this host did not start** — a restarted daemon's record of one, as this layer needs it.
/// See [`HostProcesses::adopt`].
pub struct Adopted {
    pub handle: String,
    pub command: String,
    pub slug: Option<String>,
    pub cwd: String,
    pub pid: u32,
    /// **What makes a reused pid not this process** — `start:<n>` on this box, which is what
    /// `letibot_tokencore::store::ProcessRecord::identity` holds and [`super::procs`] answers.
    pub identity: String,
    /// The owner scope as the record spells it — `session.<name>`, `turn.<name>`, `explicit.<name>`.
    pub scope: String,
}

/// **A record's scope spelling back into the two things a cgroup needs** — with the conservative
/// answer for a word this build does not know: the session, which is where a background job lives
/// (`SpawnRequest::scope`'s own default). A guess that widened a job's lifetime would keep it alive
/// past the scope that should have reaped it.
fn scope_kind_of(scope: &str) -> (super::scope::ScopeKind, String) {
    use super::scope::ScopeKind;
    match scope.split_once('.') {
        Some(("turn", n)) => (ScopeKind::Turn, n.to_string()),
        Some(("explicit", n)) => (ScopeKind::Explicit, n.to_string()),
        Some(("session", n)) => (ScopeKind::Session, n.to_string()),
        _ => (ScopeKind::Session, scope.to_string()),
    }
}

impl HostProcesses {
    /// **Hold settled output to `budget` bytes, releasing the oldest first** — see
    /// [`SETTLED_CAPTURE_BUDGET`]. Called at each spawn, which is the only moment the set of
    /// settled captures can have grown past a bound since the last call (a capture grows while
    /// its job runs, and a job settles before the next one in its session starts or while it
    /// runs beside it). Newest first, so the output a model is most likely to page through
    /// again is the output still held.
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

    /// A host with a real cgroup tree, rooted under `root` on the filesystem.
    ///
    /// Returns the probe's own error when there is no cgroup v2 to delegate,
    /// rather than falling back to an unowned spawn: **a process with no owner is
    /// the leak T24 exists to stop**, so the substrate refuses to exist rather
    /// than exist without it.
    pub fn new(root: impl Into<PathBuf>) -> Result<HostProcesses, ExecError> {
        Ok(Self::with_tree(root, super::scope::host_tree()?))
    }

    /// The confined constructor: a real cgroup tree **and** a measured boundary.
    ///
    /// Both halves fail closed and they fail for different reasons, so the error
    /// names which. There is no argument, flag or environment variable that turns
    /// a failure here into an unconfined run.
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
            // **Not asked for**, and named as such rather than left absent, so a
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
            session_name: unique("s"),
        };
        h.protect_self_and_ancestors();
        h
    }

    pub fn with_confinement(mut self, confine: Box<dyn Confinement>) -> Self {
        self.confine = std::sync::RwLock::new(confine);
        self
    }

    /// **Widen this session's view, by re-probing.** The confinement handed in was
    /// measured the same way the first one was, and only then does it take the
    /// slot: a boundary that was described but not measured is the banner defect
    /// this module exists to refuse.
    ///
    /// The caller builds it. This layer is a mechanism and must not learn to choose
    /// what to bind, which is a decision.
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

    /// The measured boundary, cloned out from under the lock.
    ///
    /// By value rather than by reference for the reason the whole field moved
    /// behind a lock: a grant replaces the confinement, so a borrow of the one
    /// inside it cannot outlive the guard.
    pub fn boundary(&self) -> Option<crate::exec::confine::Boundary> {
        self.confine
            .read()
            .expect("confinement")
            .boundary()
            .cloned()
    }

    /// **The argv a `!term` pane is started inside, so it gets the boundary a `bash` call
    /// gets** (R60 item 3) — read off the confinement as it stands *now*, so a grant made since
    /// the session opened is in it, exactly as it is for the next command.
    ///
    /// [`Confinement::wrap_terminal`] and not `wrap`: the pane's pty is its controlling terminal
    /// and has to stay so inside the boundary — see that method. Empty for an unconfined host,
    /// which is [`Unconfined`]'s own answer; a refusal for a host whose boundary is missing,
    /// which is [`super::confine::NoConfinement`]'s — and a pane in such a session does not run,
    /// for the same reason its commands do not.
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

    /// **Where a promoted run's output goes on living.** The daemon calls this once, with the
    /// session's scratch directory, and only the session's own opener knows it — see
    /// [`HostBackend::with_scratch_dir`](crate::backend::HostBackend::with_scratch_dir), which
    /// is the one call that sets both halves.
    pub fn set_scratch_dir(&self, dir: impl Into<PathBuf>) {
        *self.scratch.lock().unwrap_or_else(|e| e.into_inner()) = Some(dir.into());
    }

    /// **Give a promoted run a file that keeps everything it prints from here on** — the
    /// window's unbounded companion, in the session's scratch directory as `job-<id>.out`.
    ///
    /// Three answers, and each is a different fact: the file it already has (a second Ctrl+O
    /// must not truncate a file a run is writing); `None` because this host has no scratch
    /// directory to put one in; `None` because opening it failed, which is printed on the
    /// daemon's stderr rather than smuggled into a promotion record that would then claim a
    /// file nobody can open.
    fn spill_output(&self, job: &Job) -> Option<OutputFile> {
        if let Some(existing) = job.output_file() {
            return Some(existing);
        }
        // Nothing more will be written by a run that has ended, and its window still holds
        // what it wrote — a file made now would be a copy of a finished thing.
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
                // promotion record naming a file that is not there.
                eprintln!(
                    "letibot: `{}` was moved to the background but its output could not be \
                     spilled, so it stays only in its window: {why}",
                    job_label(job.slug.as_deref(), &job.id.0)
                );
                None
            }
        }
    }

    /// **The shell a command is handed to**, `/bin/sh -c` unless this says otherwise.
    ///
    /// Non-interactive and non-login, so **no rc file is read** — which is R10's
    /// layer 1 and the reason a bare name resolves through the pinned `PATH` and
    /// nothing else. It is what a *model's* command is read by, and the gate that
    /// judged the text is entitled to it.
    ///
    /// The operator's own run is handed to [`HostProcesses::with_console_shell`]
    /// instead, because their command has no gate to be entitled to it and their
    /// aliases are theirs.
    pub fn with_shell(mut self, shell: Vec<String>) -> Self {
        self.shell = shell;
        self
    }

    /// **The shell the operator's own command is handed to.** See the field, and
    /// [`super::console::shell`] for the default and the decision behind it.
    pub fn with_console_shell(mut self, shell: Vec<String>) -> Self {
        self.console_shell = shell;
        self
    }

    /// The harness's own process and every process above it, to the root of the
    /// tree. A `pkill` matching any of them kills the thing evaluating the
    /// command, and the parent chain is the half a model has no way to see.
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

    /// Declare a pid that **cannot exit while this session runs** — the model
    /// server serving the turn is the case this exists for. A waiter on one of
    /// these is a deadlock, not a slow wait.
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
            // **Reasons accumulate.** One pid can be protected for two reasons —
            // it is the process evaluating the command AND it is the server this
            // session talks to — and dropping the second because the first got
            // there first is how a refusal ends up naming the less useful of the
            // two. Clause 1 wants the diagnosis, and half the diagnosis is worse
            // than it looks: the model reads a reason it cannot act on and
            // reasonably concludes the guard is wrong.
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

    /// The pairs every command of this session carries — see the field. Later
    /// calls replace earlier ones wholesale.
    pub fn set_standing_env(&self, pairs: Vec<(String, String)>) {
        *self.standing_env.lock().unwrap_or_else(|e| e.into_inner()) = pairs;
    }

    /// A directory searched before the pinned `PATH` — see the field.
    pub fn set_path_prefix(&self, dir: Option<String>) {
        *self.path_prefix.lock().unwrap_or_else(|e| e.into_inner()) = dir;
    }

    /// Resolve a **listening TCP port** to the pid holding it, and protect it.
    ///
    /// This is the "the harness knows what the model cannot" case at its
    /// strongest: the model can see `:8080` in a config and has no way to know
    /// which pid that is, so it writes `pkill -f llama-server` instead. Returns
    /// the pid it found, or `None` — and `None` is a real answer here, since the
    /// socket may be held by another user's process whose `/proc/<pid>/fd` this
    /// process may not read.
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

    /// **The job of jobs held under this id**, if one is — the lookup every read path in
    /// [`ProcessHost`] makes before it goes looking for a process.
    fn aggregate(&self, id: &JobId) -> Option<Arc<dyn Aggregate>> {
        self.aggregates.lock().expect("aggregates").get(id).cloned()
    }

    /// **The row a job of jobs answers with.**
    ///
    /// Two fields are not what they are on a job, and both are said rather than faked:
    ///
    /// * **`pid` is 0** — there is no process, and a made-up number would name somebody's. It is
    ///   the same 0 a remote job reports when it exits inside the launch's pid poll, which is
    ///   already the vocabulary for *this host cannot point at a process*.
    /// * **`scope` has no cgroup path** — the work is in other machines' groups, and inventing a
    ///   path here would be a cgroup a `job_kill` could try to empty. [`Aggregate::kill`] is where
    ///   it is ended instead, and `kill_job_as` routes there first.
    ///
    /// The scope's *name* is this host's session, which is the true half: the session owns the
    /// handle, and that is what a listing should say.
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
            // **`Asked` and not `Promoted`.** A fanout the model backgrounded is the model's own
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

    /// The session scope's id, as a [`ScopeId`] — with no cgroup path, for a caller that only
    /// needs the name (see [`HostProcesses::aggregate_view`]).
    fn session_scope_id(&self) -> ScopeId {
        ScopeId {
            kind: ScopeKind::Session,
            name: self.session_name.clone(),
            path: PathBuf::new(),
        }
    }

    /// **The slug this spawn takes, made unique against every name this session has
    /// handed out.**
    ///
    /// `release-build`, then `release-build-2`, `release-build-3` … until one is free.
    /// See the field for why this is a record of its own rather than a scan of `jobs`,
    /// and why a name is never released. The caller's spelling is otherwise untouched:
    /// nothing here derives anything from the command, and a request that named nothing
    /// gets nothing.
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

    /// **What the deadline watchdog has ended, oldest first.**
    ///
    /// The read-back half of the mechanism, for the same reason [`HostProcesses::reap_log`]
    /// exists: *the deadline ended this run* is a claim, and a claim that nothing can be
    /// checked against is the unfalsifiable zero this substrate refuses everywhere else.
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
        self.reaps.lock().expect("reaps").push(r.clone());
        r
    }

    fn record_promotion(&self, p: Promotion) -> Promotion {
        self.promotions.lock().expect("promotions").push(p.clone());
        p
    }
}

impl ProcessHost for HostProcesses {
    /// **Read off the state, both halves.**
    ///
    /// This used to end in a hard-coded `NOT confined`, which was true of every
    /// host that existed when it was written and is the exact shape of the defect
    /// `docs/tool-design-brief.md` §1 names — a banner asserting a property of a
    /// session rather than reading it. Layer 1 makes a confined host possible, so
    /// the sentence now comes from [`Confinement::describe`], which in turn comes
    /// from a boundary that was measured from inside itself.
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
                // An explicit scope must be NAMED. An unnamed one would be a scope
                // that outlives the session and that nobody can refer to in order
                // to end it, which is the leak with an extra step.
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
                // A turn scope is a CHILD of the session scope, which is what makes
                // the session's end reap it without the session having to know it
                // is there.
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
        // One cgroup per job, inside its scope's. `job_kill` is then exactly the
        // same operation as ending a scope, on a smaller directory — there is one
        // kill mechanism, not two.
        let cgroup = self
            .tree
            .open(req.scope, &format!("job-{id}"), Some(&parent))?;

        let cwd = self.root.join(req.cwd.trim_start_matches('/'));

        // **The boundary is decided before the cgroup is committed to.** A session
        // that asks for confinement and has none refuses here, and refusing after
        // the `mkdir` would leave an empty scope directory that a later listing
        // reads as somebody's work.
        let confined = self.confine.read().expect("confinement");
        let wrap = match confined.wrap(&ConfinePlan {
            cwd: &cwd,
            env: &req.env,
        }) {
            Ok(w) => w,
            Err(e) => {
                let _ = self.tree.end(&cgroup);
                return Err(e);
            }
        };

        // **The cap goes on before any process joins**, so there is no instant in which the
        // command runs uncapped. A cap the tree cannot set does not refuse the command — the
        // job runs, and its result says UNCAPPED and why (`MemoryCap::Uncapped`), which is the
        // operator's rule: never silently.
        let memory_cap = match req.memory {
            None => MemoryCap::NotAsked,
            Some(bytes) => match self.tree.cap_memory(&cgroup, bytes) {
                Ok(()) => MemoryCap::Capped(bytes),
                Err(why) => MemoryCap::Uncapped { asked: bytes, why },
            },
        };

        let procs = Cgroup2::procs_path(&cgroup);
        // **The join token, which is the evidence and belongs to the daemon.**
        //
        // A path this process owns, passed to the wrapper as `$2`, created on the branch that
        // joined the cgroup and on no other. The exit code is the process's own answer; the
        // marker is a string the process can print; this is a file the command's output cannot
        // reach, so `joined` is something the daemon OBSERVED rather than something it read.
        let token = std::env::temp_dir().join(format!("letibot-joined-{}", id.0));
        let _ = std::fs::remove_file(&token);
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.arg("-c")
            .arg(join_script())
            .arg("letibot-scope")
            .arg(&procs)
            .arg(&token);
        // **The order is load-bearing.** `join_script` writes `$$` into
        // `cgroup.procs` and only then `exec`s `"$@"`, so the join happens on the
        // HOST, before any namespace exists — which is the only order that works:
        // `/sys/fs/cgroup` is deliberately not in the mount view, so a process that
        // tried to join from inside would fail the write and `exit 125`.
        //
        // Membership survives the crossing, because a cgroup is not a namespace: the
        // helper stays in the cgroup, its children inherit it, `cgroup.kill` reaches
        // every one of them regardless of which pid namespace they are in, and the
        // pid the harness tracks is the helper's, which is a real member.
        for a in &wrap {
            cmd.arg(a);
        }
        // **Their shell for their own run, the host's for everything else.**
        //
        // This is where the operator's `! ls -la` stops being the same command as the
        // daemon's `/bin/sh -c 'ls -la'`: `bash -ic` reads `~/.bashrc`, which is where
        // `alias ls='ls --color=auto'` lives, and without it the alias — and so the
        // colour — does not exist. [`super::console`] carries the decision and what it
        // changes; the flag is the one [`SpawnRequest::tty`] already documented.
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
        // **stdin, which the pty branch below replaces whenever there is a pty to give the
        // run.** What is set here is therefore the *fallback*: the pipe that keeps the
        // operator's own run answerable on a box where no pty can be opened, and `/dev/null`
        // for everything else. [`super::pty`]'s header carries why the normal case is the
        // run's own terminal instead, and [`super::jobs::Stdin`] what either one buys.
        //
        // The `/dev/null` half is the older rule and it is unchanged: a command that reads
        // stdin must get EOF rather than wait for a person who is not there. The other half is
        // what makes the operator's own run *answerable* — see [`super::jobs::Stdin`] for the
        // defect (`! sudo apt install mc` aborting at `Continue? [Y/n]` because EOF is not a
        // `Y`) and for what the handle is.
        //
        // **The cost, said rather than discovered: a `!` command that reads stdin waits.**
        // `! cat` with no argument used to print nothing and return, because EOF is an EOF;
        // it now waits for a line, and its deadline (120 s, the `bash` default) is what ends
        // it if the operator never sends one. That is the price of the feature and not a
        // defect in it: the operator asked for a run that can be *answered*, and a run that
        // can be answered is a run that waits.
        cmd.stdin(if req.tty {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        // **A terminal on the output side, for the operator's own run** — see
        // [`SpawnRequest::tty`] for who gets it and why, and [`super::pty`] for what it
        // buys and what it costs.
        //
        // A box where the pty cannot be opened falls back to pipes rather than refusing:
        // the terminal is what the operator's colour needs, not what the command needs,
        // and a run that would have answered correctly is not worth failing over a
        // cosmetic. `None` here is that fallback, taken silently and only for this — and
        // it is also why [`super::jobs::Stdin`] still has a pipe in it.
        let mut pty: Option<super::pty::Pty> = None;
        if req.tty {
            match super::pty::Pty::open() {
                // **One pty on all three descriptors**, so the capture ring is fed by the
                // master and the command's input is the same terminal its output is on.
                Ok(p) => match (p.stdio(), p.stdio(), p.stdio()) {
                    (Ok(inp), Ok(out), Ok(err)) => {
                        // **`ECHO` off, before anything runs.** The daemon is the writer on
                        // this terminal — `!send`, and the prompt card's answer — and an
                        // echoing terminal would put both into the run's output, which is a
                        // transcript row and a model's prompt. `super::pty`'s header says
                        // which reader wants which, and why the pane keeps its echo. (The
                        // *secret* card is the `askpass` path and never reaches this
                        // terminal; `harnessd`'s `prompt` module owns that half.)
                        //
                        // Not fatal if it fails, for the reason the whole pty is not: a
                        // terminal that echoes is noisy and not broken.
                        let _ = p.no_echo();
                        // **And the terminal becomes the child's controlling terminal**, which
                        // is what makes bash a job-control shell and silences the two lines it
                        // printed on every row. `super::pty::controlling_terminal` is the one
                        // copy of those two calls, shared with the pane.
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
        // **On macOS the scope IS the process group** (`scope::ProcessGroups`), so a run
        // without a pty — the pty branch's `setsid` already made it a leader — must lead a
        // group of its own, or the `$$` the wrapper records names a group it is not in.
        #[cfg(target_os = "macos")]
        if pty.is_none() {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        // **Env hygiene is layer 1 (R10 / `docs/boundary-and-adjudication.md` §5).**
        // The environment is cleared before anything is set, so `PATH` is the
        // pinned one captured at construction and a bare name resolves through
        // nothing else. `BASH_ENV`, `ENV`, `SHELLOPTS` and `BASHOPTS` are then
        // dropped from the session's own pairs as well, because bash sources
        // `$BASH_ENV` for *non-interactive* shells — a real injection point on a
        // distribution where `/bin/sh` is bash, even though this box's dash reads
        // `$ENV` only when interactive. The clear is what kills a variable
        // planted in this process's own parent; the filter is what keeps a
        // session's env configuration from re-arming it.
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
        // **The parent's own copies of the slave close HERE, and this is load-bearing.**
        //
        // A `Command` keeps every `Stdio` it was given for as long as it lives, and this
        // one lives to the end of this function — so without this the parent still holds
        // **three** slave descriptors while the drains below read the master, and a pty whose
        // slave is open anywhere does not report the child's exit. Measured, first cut,
        // 2026-09-25: the drain blocked, the waiter behind it never reaped, the finished
        // child stayed a zombie, **a zombie is not in `cgroup.procs`** — so the
        // membership check below timed out at five seconds and killed a command that had
        // already run to completion, in two of the twenty-four exec tests. `Stdio::null`
        // here is a drop, not a redirection: nothing is spawned again with this `Command`.
        //
        // **Three of them since the terminal landed, and the third is the one a reader will
        // not expect.** `cmd.stdin(Stdio::null())` is not "give the child /dev/null" — the
        // child has its own copy already and `/dev/null` here never reaches it — it is the
        // drop of the `Stdio` holding the last slave the parent owns. The pipe case is
        // unaffected: `Stdio::piped` is a marker, and the descriptor the parent keeps is
        // `child.stdin`, which is taken below.
        //
        // [`super::pty::Pty::into_master`] is the other half of the same rule, for this
        // process's own slave handle.
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::null());

        // **The run's input: a second handle on the master of its own terminal.**
        //
        // Taken here, with the three drops above, because all three are facts about the pty
        // that stop being available once `into_master` consumes it. `None` on a run with no
        // pty — a model's call, or the fallback — and [`super::jobs::Stdin`] says what the
        // two handles are and why the terminal is the normal one.
        //
        // **The third way it is `None` is a `dup` that failed, and it is swallowed.** A run
        // whose `Pty::input` could not be cloned keeps its terminal on all three descriptors
        // and simply has no handle the daemon can write to — so it is not answerable, and the
        // honest consequence is that [`super::jobs::Stdin::is_open`] is false for it: no card
        // is raised and `!send` refuses with its own sentence rather than claiming a way in
        // that does not exist. `dup` fails on `EMFILE`/`ENFILE`, at which point the spawn
        // below is about to fail for the same reason anyway.
        let input = pty
            .as_ref()
            .and_then(|p| p.input().ok().map(|m| (m, p.slave_path().to_string())));
        // **`child.stdin` is the fallback's pipe and `None` whenever the pty was used**, and
        // the order of these arms is the whole of the choice: a run with a terminal is
        // answered through the terminal, and the pipe is only for the box that had none.
        //
        // A run with neither is a run nobody can answer — a model's `bash` call, and a
        // `!` line on a box where no pty opened *and* whose pipe was never made. `None`
        // travels as [`super::jobs::Stdin::none`] rather than as an `Option` every reader
        // would have to unwrap.
        let stdin = match (input, child.stdin.take()) {
            (Some((master, slave_path)), _) => super::jobs::Stdin::terminal(master, slave_path),
            (None, Some(w)) => super::jobs::Stdin::pipe(w),
            (None, None) => super::jobs::Stdin::none(),
        };

        let job = Arc::new(
            Job::new(
                id.clone(),
                req.command.clone(),
                // **The name, decided here and never again.** The claim is made at the point
                // where this spawn can no longer be refused for a reason that leaves no job
                // behind, so a failed `mkdir` does not consume a name — and the name is fixed
                // on the record, so a listing an hour later reads the same string it read at
                // the spawn.
                req.slug.as_deref().map(|s| self.claim_slug(s)),
                Lifetime {
                    scope: cgroup,
                    owner: parent.clone(),
                    // **`Asked` only when the model asked.** A foreground command
                    // starts with `None`, and a promotion is what fills this in —
                    // so a job that was moved by the runtime can never be read back
                    // as one the model requested.
                    background: req.background.then_some(Backgrounding::Asked),
                },
                req.cwd.clone(),
                pid,
                self.capture_bytes,
                stdin,
            )
            .with_memory_cap(memory_cap),
        );

        let out = child.stdout.take();
        let err = child.stderr.take();
        // **The slave handle goes now, once the child holds its own copy.** A slave this
        // process keeps open is a slave that never closes, so the master would never
        // report the child's exit and this drain would block for ever — see
        // [`super::pty::Pty::into_master`]. Both streams are the one pty, so there is one
        // master to read and the second drain has nothing to do; the capture is one ring
        // either way, which is what makes the merge no loss.
        // macOS: hold a slave across the exit so the tail is not discarded — see
        // [`super::pty::Pty::tail_guard`]. Released by the waiter below.
        #[cfg(target_os = "macos")]
        let tail_guard = pty.as_ref().and_then(|p| p.tail_guard().ok());
        let master = pty.map(|p| p.into_master());
        let a = drain(out, Arc::clone(&job));
        let b = match master {
            Some(m) => drain(Some(m), Arc::clone(&job)),
            None => drain(err, Arc::clone(&job)),
        };

        let waiter = Arc::clone(&job);
        std::thread::Builder::new()
            .name(format!("letibot-job-{id}"))
            .spawn(move || {
                // Drain BEFORE reaping the child, so every byte it wrote is in the
                // capture by the time the state stops saying `Running`. A state
                // that settles first would let a caller read the output of a
                // finished job and get half of it.
                //
                // **macOS reaps first**, because the guard holds a slave and the master would
                // not end while it does: reap, let the guard go once the drain has emptied
                // the buffer, and only then join — the state still settles after the drain.
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
                #[cfg(not(target_os = "macos"))]
                let status = child.wait();
                // A job already marked `Killed` keeps that: SIGKILL from a cgroup
                // reap arrives here as a signal, and reporting it as a signal would
                // lose the fact that WE did it.
                if !waiter.state().is_running() {
                    waiter.finished.notify_all();
                    return;
                }
                let state = match status {
                    Ok(s) => match s.code() {
                        // **The wrapper's marker, and not the number alone.** R21's
                        // sibling, found by the other head on 2026-09-22:
                        // `EXIT_NOT_SCOPED` is 125, and **125 is a legitimate exit code**,
                        // so `bash -c "exit 125"` was listed as
                        // `not run (could not join its scope)` — a command described as
                        // never having run when it ran and chose 125. The model read the
                        // state word and concluded *"the 125 exit code was never
                        // produced"*, which is worse than misleading a person: a person
                        // can disbelieve a card.
                        //
                        // The two layers here now agree, which they did not: the
                        // CONFINEMENT layer refuses to classify a launcher failure by
                        // exit code at all (*"we do not control bubblewrap's exit codes,
                        // so this reads its stderr instead"*, `confinement_failure`), and
                        // the scope wrapper — which does control its own code — took the
                        // shortcut. **A launcher failure is recognised by what the
                        // launcher SAID.**
                        //
                        // The capture is complete here (both drains have joined, above),
                        // and in the case this tests the marker is all there is: the
                        // command never ran, so there is nothing else to have pushed the
                        // marker out of the ring.
                        // **Both halves, and the second is the daemon's own.** The marker
                        // is what the wrapper says; the token is whether it got that far.
                        // A command that printed the marker itself has the first and not
                        // the second — it ran, and 125 is its own answer.
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
                // **The token is consumed here, once, by the only reader.** It is the
                // daemon's own file in the temp directory, so a job that settles must not
                // leave it behind — and removing it after the read is what keeps
                // `present` and `joined` the same fact for the whole of one job.
                let _ = std::fs::remove_file(&token);
                // **Read the kernel's counters before the state settles**: a reader woken by
                // the settle builds the result, and the result is where the peak is said.
                if let Some(used) = super::scope::memory_use_of(&waiter.lifetime().scope) {
                    waiter.record_memory(used);
                }
                waiter.settle(state);
            })
            .map_err(|e| ExecError::Spawn(e.to_string()))?;

        // **Do not return until the process is IN its cgroup, or is finished.**
        //
        // Measured, not anticipated: without this, `bash --background` followed
        // immediately by `job_kill` read an empty cgroup, killed nothing, reported
        // `observed 0` — and the process joined a millisecond later and ran on. An
        // orphan produced by the kill path, and a reap record that said so
        // truthfully while the leak happened anyway.
        //
        // The loop ends on either half of the disjunction, because a fast command
        // (`true`) can be finished before it is ever observable as a member, and
        // waiting for membership alone would hang on every quick call.
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
            // A live process that is not in its scope is precisely the orphan this
            // substrate exists to make impossible, so it is killed rather than
            // handed back as a job whose lifetime nothing owns.
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
        Ok(id)
    }

    /// **Take a job the daemon did not start, and make it one of this host's.**
    ///
    /// This is what a RESTART is made of: the old daemon wrote the job's record down
    /// ([`letibot_tokencore::store::ProcessRecord`]), a new one finds it still running, and
    /// everything above this layer — `jobs`, `job_output`, `job_kill`, `job_wait`, the pane, the
    /// settlement that wakes the model — is written against the host's own job table. So the
    /// record is turned back into a [`Job`] here, rather than a second read path being taught to
    /// every one of those readers.
    ///
    /// **The cgroup name is derivable, and that is what makes this exact rather than
    /// approximate.** A spawn opens its job's cgroup as `<owner>/job-<handle>`
    /// ([`HostProcesses::spawn`]), so re-opening that same name re-opens the SAME directory — and
    /// the process the old daemon put there is still a member of it. `job_kill` on an adopted job
    /// is therefore the same operation it always was, not a signal to a pid nobody checked.
    ///
    /// **Nothing here invents an exit.** The thread this spawns settles the job the only way a
    /// process that vanished can be settled: [`JobState::Dead`], which is *gone, and no exit was
    /// recorded* — never `Exited { 0 }`, which would be a success nobody reported.
    fn adopt(&self, a: &Adopted) -> Result<JobId, ExecError> {
        let id = JobId(a.handle.clone());
        if self.find(&id).is_some() {
            return Ok(id);
        }
        let (kind, name) = scope_kind_of(&a.scope);
        let owner = self.scope_for(kind, Some(&name))?;
        let scope = self
            .tree
            .open(kind, &format!("job-{}", a.handle), Some(&owner))?;
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
            0,
            super::jobs::Stdin::none(),
        ));
        self.by_id
            .lock()
            .expect("jobs by id")
            .insert(id.clone(), Arc::clone(&job));
        self.jobs.lock().expect("jobs").push(Arc::clone(&job));
        // **The record's identity is the whole of the evidence that this is the same process.**
        // A pid alone would settle a stranger's job — or keep one running for ever — on a box that
        // had reused the number.
        let identity = a.identity.clone();
        let pid = a.pid;
        let watched = Arc::clone(&job);
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
                            break;
                        }
                    }
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
            });
        Ok(id)
    }

    /// **Give this run a deadline, and hand its enforcement to the host.**
    ///
    /// The tool that starts a run is the one that knows how long it may live —
    /// `timeout_ms` is its argument — so it is the one that arms this. What it must not be
    /// is the one that *enforces* it, and that is the whole of [`Deadlines`]: the run's own
    /// thread reports the deadline, and this thread ends the run.
    fn arm_deadline(&self, job: &JobId, after: Duration) {
        // **A job of jobs arms nothing** — every branch carries its own deadline in its own
        // `process` record (RH7/RH8), where the supervisor enforces it on the machine that runs
        // it. A second clock here would be a second answer to *when is it out of time*.
        if self.aggregate(job).is_some() {
            return;
        }
        if let Some(j) = self.find(job) {
            self.deadlines.arm(j, after);
        }
    }

    /// **Take a handle that stands for work running elsewhere** — see [`Aggregate`].
    ///
    /// The name it answers to is the aggregate's own ([`Aggregate::slug`]), claimed by the tool
    /// that made it through this host's own slug record — so two fanouts in one session cannot be
    /// given the same one, and nothing here has to know what a fanout is.
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
        // **A job of jobs settles here, not in a cgroup.** One wait, N branches, each watched
        // by the thread that started it — so the answer to `jobwatch` is *it happened* only when
        // every one of them has ended.
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

    /// **Presence, then absence, and never absence alone.**
    ///
    /// The seat brief's rule, mechanised: an empty cgroup at t=0 is a boot window,
    /// not a finish. So this waits up to a quarter of the deadline (at most two
    /// seconds) to *see* the scope populated, and if it never is, it returns
    /// [`Waited::NeverStarted`] rather than a completion nobody earned.
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
        // **A job of jobs is a row like any other** — after the runs, because it stands for work
        // those runs are not: it is a table of its own, and a listing that interleaved the two
        // would suggest the parent was one more process on this box.
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
        // **The journal, not a table** — see [`Aggregate::output`].
        if let Some(agg) = self.aggregate(id) {
            return Ok(agg.output(from, limit));
        }
        let job = self.find(id).ok_or(ExecError::NoSuchJob(id.0.clone()))?;
        let cap = job.capture.lock().expect("capture");
        Ok(cap.slice(from, limit))
    }

    fn kill_job_as(&self, id: &JobId, by: &str) -> Result<Reaping, ExecError> {
        // **A job of jobs is ended branch by branch**, on each branch's own host through that
        // host's own process group (RH1) — there is no cgroup of ours holding them, and a
        // `kill -TERM -<pgid>` here would reach processes this box cannot see.
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
        // **The watchers go with the scope, and the record says which scope took
        // them.** A monitor that merely stopped being polled would be a watcher
        // that went quiet, and a watcher that goes quiet looks exactly like one
        // whose condition never fired.
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

    /// **The order is load-bearing, and it is not the obvious one.**
    ///
    /// Open the new cgroup, migrate into it, *measure*, and only then change the
    /// job's recorded lifetime. Updating the bookkeeping first and migrating
    /// afterwards would produce the announced-but-not-done failure with a job id
    /// on it: `job_list` saying `session` about processes the turn scope is
    /// about to kill.
    ///
    /// A job whose processes could not all be moved is **still recorded as
    /// promoted**, because the ones that did move really are owned by the new
    /// scope now — and the residue is in the record and in the note rather than
    /// hidden behind a rollback that would have to kill work to be honest.
    ///
    /// # And the output gets a file, because the move is a promise about time
    ///
    /// A foreground command is bounded by its deadline, so a window that holds 8 MiB is a
    /// window that holds all of it. A move to the background lifts that bound — the run may
    /// now print for an hour — so the promotion is the one moment where *the output is only in
    /// memory* stops being a fact nobody could notice and becomes an agony. The operator,
    /// 2026-10-26: *"when I background a lengthy bash run with Ctrl-o its output goes to
    /// /dev/null, causes agony, should go to a tmp file in a scratch dir"*. So the same step
    /// opens `job-<id>.out` in the session's scratch directory ([`OutputFile`]), seeds it with
    /// what the run has already printed, and every later byte goes to both the window and the
    /// file.
    ///
    /// **After the migration, not before it**: a promotion that failed moved nothing, and a
    /// file created for a move that did not happen is a promise nobody kept. Nothing is lost by
    /// the ordering — the file is seeded from the window as it stands when the move is measured.
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

        // **Already there.** Not an error and not a no-op worth hiding: a caller that
        // promoted the same job twice is told the second call moved nothing, rather than shown
        // a record that looks like a second move. The file is still the answer to *where does
        // its output go* — this may be the first promotion that could make one, either because
        // the earlier one found no scratch directory or because the job was backgrounded by
        // somebody else and never moved at all.
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

        // **And the bytes get somewhere unbounded to go**, now that the run really has
        // outlived the turn — see this method's header.
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

/// **The reaper lives at the creation site**, which is here.
///
/// lubuntu1's finding, and it is the one that ruled out a design T24 had left
/// open: *"the leak scales with children spawned, not with seat uptime … reaping
/// belongs wherever subagents are created, not in a periodic sweep on each box."*
/// A sweeper cannot tell debris from a deliberate long-lived resource, and it runs
/// on boxes with nothing to sweep while the box doing fan-out is the only one that
/// needs it. So the thing that starts processes is the thing that ends them, and
/// it does so without anybody remembering to.
///
/// The session scope goes, and every turn scope and job under it with it. An
/// **explicit** scope does not, because that is the entire content of the word —
/// `.78`'s three live llama.cpp builds are not debris, and a drop handler that
/// reaped them would be the sweeper this design rejected, wearing a destructor.
impl Drop for HostProcesses {
    fn drop(&mut self) {
        // **The deadline watchdog goes with its host** (R67 item 13) — see `Deadlines::closed`.
        self.deadlines.close();
        let session = self.session.lock().expect("session scope").clone();
        if let Some(s) = session {
            let _ = self.tree.end(&s);
        }
        // Then the empty directories. A cgroup holding nothing is not a leak of
        // anything that runs, but it is one more row in a listing that somebody
        // will one day have to decide about — which is what thirteen abandoned
        // worktrees were.
        self.tree.prune();
    }
}

/// **Did the WRAPPER fail to put the process in its scope?**
///
/// The evidence is the sentence [`super::scope::join_script`] writes to stderr when the
/// write to `cgroup.procs` did not work — the same string this check is built from, so the
/// script and the check cannot drift (asserted in `scope.rs`'s own tests).
///
/// **Why not the exit code alone**, which is what this replaced: the wrapper exits
/// `EXIT_NOT_SCOPED`, and 125 is a legitimate code. A command that chose 125 was reported
/// as one that never ran, and the model reasoned from the state word to a false statement
/// about the world. `bash -c "exit 125"` reads `exited 125` now, and this function is the
/// difference.
///
/// The capture is bounded (8 MiB, the tail), and that is enough here for a reason worth
/// stating: a wrapper that could not join the cgroup **ran no command**, so the marker is
/// the entire output and nothing exists that could have pushed it out of the ring.
///
/// **The two layers agree again.** The confinement layer never classified a launcher
/// failure by exit code — *"we do not control bubblewrap's exit codes, so this reads its
/// stderr instead"* — and the scope wrapper, which owns its own code and so could have,
/// took the shortcut. A launcher failure is recognised by what the launcher SAID.
fn launcher_failed(job: &Job) -> bool {
    job.capture
        .lock()
        .expect("capture")
        .contains(super::scope::NOT_SCOPED_MARKER)
}

/// A reader thread that appends one stream into the job's capture.
///
/// Stdout and stderr go into **one** buffer, in arrival order, which is what a
/// terminal shows and what an error message needs — a compiler's diagnostic is
/// useless separated from the line of progress it followed. The ordering *between*
/// the two streams is approximate, because two pipes have no shared clock; that is
/// a real limit and it is stated rather than papered over.
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

/// The signal that ended a process, without a `libc` dependency.
///
/// `ExitStatus`'s `Debug` is stable enough for a fallback and `signal()` is behind
/// the unix extension trait, which this crate may use — it is std, not `libc`.
fn signal_of(status: &std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status.signal().unwrap_or(0)
}

/// A pid's parent, from `/proc/<pid>/stat`.
///
/// Field 4 is `ppid`, and it is read by splitting on the **last** `)` rather than
/// on whitespace: field 2 is the executable name in parentheses and may contain
/// spaces, which is the parse everybody gets wrong once.
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

/// The socket inodes LISTENing on `port`, from `/proc/net/tcp{,6}`.
///
/// Split out of [`listener_pid`] because the two questions have different
/// answers when they fail. *Is anything listening* is answerable from
/// `/proc/net/tcp` alone, which every process may read; *which pid holds it*
/// needs `/proc/<pid>/fd`, which the kernel refuses for another user. A monitor
/// wants the first, and folding it into the second would have made a port watch
/// report "not listening" about somebody else's server.
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

/// **Is anything listening on this TCP port?**
///
/// What [`super::monitor::Watch::Port`] asks each tick. Reads the kernel's own
/// socket table and never a process name, so there is no string here that could
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

/// The pid holding a listening TCP socket on `port`, by inode.
///
/// Two steps, and the second is the one that can legitimately fail: find the
/// socket's inode in `/proc/net/tcp{,6}`, then find the process whose `/proc/<pid>/fd`
/// contains a link to `socket:[<inode>]`. Reading another user's `fd` directory is
/// refused by the kernel, so `None` genuinely means *not resolvable from here* and
/// never *nothing is listening*.
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

    /// **A dropped host takes its deadline watchdog with it** — R67 item 13. The watchdog was an
    /// endless loop holding its own `Arc<Deadlines>`, so every `HostProcesses` ever built left a
    /// `letibot-deadline` thread behind for the life of the process: one of the two threads R66
    /// counted per finished subagent.
    ///
    /// Two measurements. The proof is the `Weak`: the watchdog's closure owns the only other
    /// `Arc`, so the `Deadlines` is freed exactly when that thread has left `run` — which is the
    /// same fact as the thread ending, and immune to other tests building hosts in parallel. The
    /// thread itself is then looked for by its tid in `/proc/self/task`.
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
        // Bind a real socket so the test depends on nothing that is running on
        // this box. Guard the fact, not the proxy: the assertion is that the pid
        // found is OURS, not merely that something was found.
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = l.local_addr().unwrap().port();
        match listener_pid(port) {
            Some(pid) => assert_eq!(pid, std::process::id()),
            // /proc may be unreadable in some sandboxes; that is a different fact
            // from "nothing is listening" and the function says so by returning
            // None rather than a wrong pid.
            None => eprintln!("listener_pid: /proc/<pid>/fd not readable here"),
        }
    }

    #[test]
    fn a_host_with_no_scope_tree_refuses_to_spawn() {
        // Fail closed: a process with no owner is the leak, so there is no path
        // from "no cgroups" to "run it anyway".
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

    /// **Settled output is held to a budget, oldest released first, and a running job's never**
    /// (R68.3). Before the budget a host kept every job's whole capture for its life — up to
    /// 8 MiB a job — so a session's job table grew with every `bash` it ever ran.
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

    /// **A name is unique within the session, and it never moves.**
    ///
    /// Two jobs the caller named `tests` are two jobs: the second is `tests-2`. The
    /// alternative — both answering to `tests` — would make resolving a name a guess, and a
    /// name two jobs answer to is worse than the id it was meant to replace. A settled job
    /// keeps its name too: the name has to identify one job for as long as the session can
    /// still be asked about it.
    ///
    /// **And a request that named nothing gets nothing.** No derivation, no fallback word —
    /// the command is already on the row, so a guessed name earns nothing and costs the
    /// truth.
    #[test]
    fn a_slug_is_made_unique_within_the_session_and_never_invented() {
        // **The cgroup is apparatus** — the same skip the two tests below make, for the same
        // reason: a container with no delegatable cgroup v2 subtree is a fact about the
        // machine and not a failure of this claim.
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
        // **Stable for the job's life**: nothing above moved a name that was already taken.
        assert_eq!(slug_of(&first).as_deref(), Some("tests"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_bash_env_planted_in_the_parent_never_reaches_the_child() {
        // R10 / `docs/boundary-and-adjudication.md` §5, layer 1. The spawn clears
        // the environment before setting its own pairs, so nothing planted in
        // this process — including `BASH_ENV`, which bash sources even for
        // non-interactive shells — can reach the child, and `PATH` is the pinned
        // one rather than whatever the parent carries at spawn time.
        // **The cgroup is apparatus.** `HostProcesses::new` asks the kernel for a
        // delegatable cgroup v2 subtree; a container without one has none to give, and
        // that is a fact about the MACHINE rather than about the environment this test
        // plants. Skipped rather than failed, loudly, per `apparatus`.
        let Some(_) = letibot_tokencore::apparatus::present(
            "a process-lifetime tree (cgroup v2; process groups on macOS)",
            letibot_tools::host_tree().is_ok(),
        ) else {
            return;
        };
        let root = std::env::temp_dir().join(format!("letibot-r10-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        // Edition 2024: touching the process environment is unsafe, and this is
        // the one test that needs to. `env_clear()` in the spawn path undoes it
        // for every child this host starts.
        unsafe {
            std::env::set_var("BASH_ENV", "/tmp/letibot-absent.rc");
            std::env::set_var("SHELLOPTS", "xtrace");
        }
        let h = HostProcesses::new(&root).expect("this box has a cgroup v2 tree");
        let id = h
            .spawn(&SpawnRequest {
                // Both printf and the `${var-}` expansion are builtins, so the
                // assertion does not depend on anything outside the pinned PATH.
                command: "printf 'be=%s\\n' \"${BASH_ENV-}\"; printf 'path=%s\\n' \"$PATH\"".into(),
                // Nobody named this run: the substrate's own test.
                slug: None,
                cwd: "/".into(),
                scope: ScopeKind::Turn,
                scope_name: None,
                background: false,
                // A session's env configuration must not re-arm what the clear
                // removed: the four are filtered even when asked for by name.
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
        // **Restored before anything can fail**, because the variables are the whole
        // process's: every other test that starts a shell without clearing its environment
        // inherited `SHELLOPTS=xtrace` from here — MEASURED 2026-10-07 on macOS, where
        // `/bin/sh` is bash and honours it, as four pty tests capturing `+ echo done` before
        // `done`. (Linux's `/bin/sh` is dash, which ignores it, so it never showed there.)
        unsafe {
            std::env::remove_var("BASH_ENV");
            std::env::remove_var("SHELLOPTS");
        }
        assert!(text.starts_with("be=\npath=/"), "{text}");
        std::fs::remove_dir_all(&root).ok();
    }

    /// **The operator's own run can be ANSWERED** — the whole of the branch, measured on
    /// the real spawn path.
    ///
    /// The defect: `! sudo apt install mc` streamed its progress and then aborted at
    /// `Continue? [Y/n]`, because fd 0 was `/dev/null` and an EOF is not a `Y`. The fix is
    /// one line in `spawn` — a handle on the run's input when [`SpawnRequest::tty`] is set —
    /// and this is the test that would have failed before it: the command **reads** its
    /// stdin, the handle comes out of the job the spawn returned, a line goes in, and the
    /// command finishes with that line in its output.
    ///
    /// **And since the terminal landed, that handle is the run's own pty master and not a
    /// pipe beside it.** The shell is `bash -ic` — [`super::console::shell`]'s shape, which is
    /// what the operator's own run gets — and the pty is on all three of its descriptors, so
    /// what this measures is the production mechanism end to end rather than a pipe that is
    /// about to be deleted. The assertion is **the exact capture** and not `contains`, because
    /// two facts have to hold at once and one of them is an absence: `answered=Y\n` is the
    /// program's own line, and the `Y` the daemon typed is **not** in front of it.
    /// `Pty::no_echo` is what makes the second true — measured on this box, 2026-10-07, the
    /// identical run without it captures `Y\nanswered=Y\n` — and an echoing terminal would put
    /// every `!send` line into a transcript row and a model's prompt.
    ///
    /// **`--norc --noprofile` on the shell, for the reason [`super::shell`]'s tests give**:
    /// the assertion below is on the *exact* capture and the operator's rc is a file that
    /// prints things. Measured here, 2026-10-07: with the default console shell and `cwd` `/`,
    /// this run captures `mkdir: Permission denied` twice before its own line. The rc has its
    /// own test — `the_operators_own_ls_colours_because_its_output_is_a_terminal` — and it
    /// keeps the default shell, because the alias it needs lives there.
    ///
    /// **The control is in the same test, and it is the half that keeps the older rule.**
    /// The identical command with `tty: false` has `/dev/null` for stdin, so its handle is
    /// [`super::super::jobs::Stdin::none`] and the send is refused with a sentence rather
    /// than writing into a terminal nobody holds. Without that half, a `spawn` that gave
    /// *every* job an input would pass — and a model's `bash` call that reads stdin would
    /// block for two minutes instead of getting EOF.
    #[test]
    fn the_operators_own_run_has_a_stdin_a_line_can_be_sent_to() {
        // **The cgroup is apparatus** — the same skip the test above makes, for the same
        // reason: a container with no delegatable cgroup v2 subtree is a fact about the
        // machine and not a failure of this claim.
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
        // `read` is a builtin in every shell this host can be configured with, so the
        // assertion does not depend on anything outside the pinned PATH — the same rule
        // the test above keeps for `printf`.
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
        // **And the host can NAME the run's processes**, which is the other half of the
        // detection: `ask` reads each one's `/proc/<pid>/fd/0` and its `wchan`, so a host
        // that cannot list them would make every operator run look like one that is not
        // asking (`ask::Waiting::Unreadable`). The list comes from the job's own cgroup.
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
}
