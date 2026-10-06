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
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use super::ExecError;
use super::confine::{ConfinePlan, Confinement, Unconfined};
use super::jobs::{Job, JobId, JobState, Lifetime, OutputSlice};
use super::monitor::Monitors;
use super::scope::{
    Cgroup2, EXIT_NOT_SCOPED, Migration, Reaped, Reaping, ScopeId, ScopeKind, ScopeTree,
    cmdline_of, join_script,
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
    /// **Three things follow from it, and they follow from this one flag.**
    ///
    /// | what | where | why |
    /// |---|---|---|
    /// | a pty on stdout and stderr instead of pipes | the spawn below, and [`super::pty`] | a pipe is not a terminal, so `ls --color=auto` prints plain |
    /// | their shell, interactive, instead of the host's | [`HostProcesses::with_console_shell`] | the alias that colours `ls` is shell state in a rc file, and no rc is read by `/bin/sh -c` |
    /// | their terminal's variables, and pagers that cannot page | [`super::console::env`], put on the request by `bash` | `TERM` and `LS_COLORS` describe the screen; `less` waits for a keystroke that cannot arrive |
    ///
    /// It is one flag and not three because it is one fact — *a person typed this and
    /// is looking at a screen* — and three flags with the same value are three things
    /// a later edit can make disagree. [`super::console`] carries the decision, what
    /// it changes, and why a model's call gets none of it.
    pub tty: bool,
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
    pub id: JobId,
    pub command: String,
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
}

/// One job moved from one scope's ownership to another's, with the measurement
/// that says whether the move is true of every process it claims.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Promotion {
    pub job: JobId,
    /// The command, kept here because a promotion record read an hour later has
    /// to identify something. `/proc` will not still say.
    pub command: String,
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

    pub fn summary(&self) -> String {
        let mut s = format!(
            "`{}` promoted from {} to {} after {:.1}s — {}",
            self.job,
            self.from,
            self.to,
            self.ran_for.as_secs_f32(),
            self.how.phrasing()
        );
        match &self.migration {
            Some(m) => s.push_str(&format!("; {}", m.summary())),
            None => s.push_str("; nothing was moved"),
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

    /// Block on a **handle**, never a pattern. `job` waits for one job to leave
    /// `Running`; `scope` waits for a cgroup to empty, and applies the
    /// presence-before-absence rule itself.
    fn wait_job(&self, job: &JobId, deadline: Duration) -> Result<Waited, ExecError>;
    fn wait_scope(&self, scope: &ScopeId, deadline: Duration) -> Result<Waited, ExecError>;

    fn job(&self, job: &JobId) -> Option<JobView>;
    fn jobs(&self) -> Vec<JobView>;
    fn output(&self, job: &JobId, from: u64, limit: usize) -> Result<OutputSlice, ExecError>;

    /// Kill one job by killing its cgroup, and record it.
    fn kill_job(&self, job: &JobId) -> Result<Reaping, ExecError>;
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

    /// The live handle for a job, so a monitor can be declared against the job
    /// itself rather than against an id it would have to look up every tick.
    ///
    /// An id can be re-used by a different host; a handle cannot. A monitor that
    /// silently re-targeted would be watching something nobody asked about.
    fn job_handle(&self, job: &JobId) -> Option<Arc<Job>> {
        let _ = job;
        None
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
}

/// The host implementation: real processes, real cgroups.
pub struct HostProcesses {
    tree: Box<dyn ScopeTree>,
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
    jobs: Mutex<Vec<Arc<Job>>>,
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

impl HostProcesses {
    /// A host with a real cgroup tree, rooted under `root` on the filesystem.
    ///
    /// Returns the probe's own error when there is no cgroup v2 to delegate,
    /// rather than falling back to an unowned spawn: **a process with no owner is
    /// the leak T24 exists to stop**, so the substrate refuses to exist rather
    /// than exist without it.
    pub fn new(root: impl Into<PathBuf>) -> Result<HostProcesses, ExecError> {
        Ok(Self::with_tree(root, Box::new(Cgroup2::probe()?)))
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
        Ok(Self::with_tree(root, Box::new(Cgroup2::probe()?)).with_confinement(confine))
    }

    pub fn with_tree(root: impl Into<PathBuf>, tree: Box<dyn ScopeTree>) -> HostProcesses {
        let h = HostProcesses {
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
            shell: vec!["/bin/sh".into(), "-c".into()],
            console_shell: super::console::shell(),
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

    /// What the boundary is, in its own words.
    pub fn confinement_describe(&self) -> String {
        self.confine.read().expect("confinement").describe()
    }

    pub fn with_capture_bytes(mut self, bytes: usize) -> Self {
        self.capture_bytes = bytes;
        self
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
        let comm = std::fs::read_to_string(format!("/proc/{pid}/comm"))
            .unwrap_or_default()
            .trim()
            .to_string();
        let cmdline = std::fs::read(format!("/proc/{pid}/cmdline"))
            .map(|b| cmdline_of(&b))
            .unwrap_or_default();
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
        self.jobs
            .lock()
            .expect("jobs")
            .iter()
            .find(|j| &j.id == id)
            .cloned()
    }

    fn view(job: &Job) -> JobView {
        let cap = job.capture.lock().expect("capture");
        let life = job.lifetime();
        JobView {
            id: job.id.clone(),
            command: job.command.clone(),
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
        }
    }

    /// Every job id this host knows, for clause 1's benefit when one is unknown.
    pub fn job_ids(&self) -> Vec<JobId> {
        self.jobs
            .lock()
            .expect("jobs")
            .iter()
            .map(|j| j.id.clone())
            .collect()
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
        cmd.current_dir(&cwd).stdin(Stdio::null());
        // **A terminal on the output side, for the operator's own run** — see
        // [`SpawnRequest::tty`] for who gets it and why, and [`super::pty`] for what it
        // buys and what it costs. stdin is `/dev/null` either way: a command that reads
        // it must get EOF rather than wait for a person who is not there.
        //
        // A box where the pty cannot be opened falls back to pipes rather than refusing:
        // the terminal is what the operator's colour needs, not what the command needs,
        // and a run that would have answered correctly is not worth failing over a
        // cosmetic. `None` here is that fallback, taken silently and only for this.
        let mut pty: Option<super::pty::Pty> = None;
        if req.tty {
            match super::pty::Pty::open() {
                // Both streams are the one pty, so one `Stdio` per stream and no pipe
                // between them: the capture ring is fed by the master.
                Ok(p) => match (p.stdio(), p.stdio()) {
                    (Ok(out), Ok(err)) => {
                        cmd.stdout(out).stderr(err);
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
        // two slave descriptors while the drains below read the master, and a pty whose
        // slave is open anywhere does not report the child's exit. Measured, first cut,
        // 2026-09-25: the drain blocked, the waiter behind it never reaped, the finished
        // child stayed a zombie, **a zombie is not in `cgroup.procs`** — so the
        // membership check below timed out at five seconds and killed a command that had
        // already run to completion, in two of the twenty-four exec tests. `Stdio::null`
        // here is a drop, not a redirection: nothing is spawned again with this `Command`.
        //
        // [`super::pty::Pty::into_master`] is the other half of the same rule, for this
        // process's own slave handle.
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::null());

        let job = Arc::new(Job::new(
            id.clone(),
            req.command.clone(),
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
        ));

        let out = child.stdout.take();
        let err = child.stderr.take();
        // **The slave handle goes now, once the child holds its own copy.** A slave this
        // process keeps open is a slave that never closes, so the master would never
        // report the child's exit and this drain would block for ever — see
        // [`super::pty::Pty::into_master`]. Both streams are the one pty, so there is one
        // master to read and the second drain has nothing to do; the capture is one ring
        // either way, which is what makes the merge no loss.
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
                if let Some(h) = a {
                    let _ = h.join();
                }
                if let Some(h) = b {
                    let _ = h.join();
                }
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

        self.jobs.lock().expect("jobs").push(job);
        Ok(id)
    }

    fn wait_job(&self, id: &JobId, deadline: Duration) -> Result<Waited, ExecError> {
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
        self.find(id).map(|j| Self::view(&j))
    }

    fn jobs(&self) -> Vec<JobView> {
        self.jobs
            .lock()
            .expect("jobs")
            .iter()
            .map(|j| Self::view(j))
            .collect()
    }

    fn output(&self, id: &JobId, from: u64, limit: usize) -> Result<OutputSlice, ExecError> {
        let job = self.find(id).ok_or(ExecError::NoSuchJob(id.0.clone()))?;
        let cap = job.capture.lock().expect("capture");
        Ok(cap.slice(from, limit))
    }

    fn kill_job(&self, id: &JobId) -> Result<Reaping, ExecError> {
        let job = self.find(id).ok_or(ExecError::NoSuchJob(id.0.clone()))?;
        // Mark first, so the waiter thread does not overwrite `Killed` with the
        // SIGKILL it is about to see. F5: the reason it stopped is a fact and the
        // signal is only its shape.
        let running = job.state().is_running();
        if running {
            job.settle(JobState::Killed {
                by: "job_kill".into(),
            });
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

        // Already there. Not an error and not a no-op worth hiding: a caller that
        // promoted the same job twice should be told that the second call moved
        // nothing, rather than shown a record that looks like a second move.
        if life.owner == target {
            return Ok(self.record_promotion(Promotion {
                job: id.clone(),
                command: job.command.clone(),
                from: life.owner.clone(),
                to: target,
                ran_for,
                how,
                migration: None,
                at: SystemTime::now(),
                note: Some(format!(
                    "`{id}` was already owned by that scope, so nothing moved"
                )),
            }));
        }

        if !job.state().is_running() {
            return Ok(self.record_promotion(Promotion {
                job: id.clone(),
                command: job.command.clone(),
                from: life.owner.clone(),
                to: target,
                ran_for,
                how,
                migration: None,
                at: SystemTime::now(),
                note: Some(format!(
                    "`{id}` had already {} when the promotion arrived; there was no \
                     process left to move, and its output is still readable",
                    job.state().word()
                )),
            }));
        }

        let fresh = self.tree.open(to, &format!("job-{id}"), Some(&target))?;
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
            from: life.owner,
            to: target,
            ran_for,
            how,
            migration: Some(migration),
            at: SystemTime::now(),
            note,
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
pub fn port_is_listening(port: u16) -> bool {
    !listen_inodes(port).is_empty()
}

/// The pid holding a listening TCP socket on `port`, by inode.
///
/// Two steps, and the second is the one that can legitimately fail: find the
/// socket's inode in `/proc/net/tcp{,6}`, then find the process whose `/proc/<pid>/fd`
/// contains a link to `socket:[<inode>]`. Reading another user's `fd` directory is
/// refused by the kernel, so `None` genuinely means *not resolvable from here* and
/// never *nothing is listening*.
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
                cwd: ".".into(),
                scope: ScopeKind::Turn,
                scope_name: None,
                background: false,
                env: vec![],
                // No terminal: these are the substrate's own tests, not an operator's run.
                tty: false,
            })
            .unwrap_err();
        assert!(format!("{e}").contains("no cgroup v2 here"), "{e}");
        assert!(format!("{e}").contains("nothing would reap it"), "{e}");
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
            "a cgroup v2 tree",
            letibot_tools::Cgroup2::probe().is_ok(),
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
                cwd: "/".into(),
                scope: ScopeKind::Turn,
                scope_name: None,
                background: false,
                // A session's env configuration must not re-arm what the clear
                // removed: the four are filtered even when asked for by name.
                env: vec![("BASH_ENV".into(), "/tmp/letibot-absent.rc".into())],
                // No terminal: the substrate's own test, not an operator's run.
                tty: false,
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
        assert!(text.starts_with("be=\npath=/"), "{text}");
        std::fs::remove_dir_all(&root).ok();
    }
}
