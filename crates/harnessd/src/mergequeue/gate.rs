//! **The gate: the target's own `## Merge gate`, run with a deadline, in a process group, and
//! killable** (R61 requirement 4, decision 9). Every step is `sh -c <step>` in a new process group
//! writing to a file; the runner polls it, says the owner is alive between polls, and kills the
//! whole group — `SIGTERM`, then `SIGKILL` — on the deadline, on the daemon's stop, or when the
//! lease is gone.
//!
//! **Each step runs in a cgroup of its own** (R76 item 6), a sibling of the daemon's leaf under the
//! delegated scope ([`letibot_tools::exec::SiblingScope`]), capped by `[memory] gate` — half of what
//! is free unless it says otherwise — so a heavy build cannot throttle the sessions the daemon
//! serves. The step joins it before `exec`, and `cgroup.kill` reaches what the process group does
//! not; the row's evidence ends with the step's peak, saying so when the kernel killed it at the
//! cap. With no delegated scope the step runs *uncapped* and the row says so.
//!
//! **And the step is told what it may use**: `TMPDIR` is a directory of the gate's own on the box's
//! real disk ([`GATE_TMP_ROOT`], one per run) and `CARGO_BUILD_JOBS` comes from the step's own cap
//! rather than from the core count — see [`GATE_TMPDIR`], [`GATE_TMP_ROOT`] and [`build_jobs`].
//!
//! **The runner is an interface** ([`GateRunner`]) and there are two implementations of it:
//! [`LocalGate`], which runs the steps here, and [`HostGate`] (`mergequeue::gate_host`), which
//! runs them on a machine the operator declared in `hosts.toml` — R62's *"the gate runs on the
//! fleet"*, and the operator's 2026-10-29 ruling that a box whose daemon, VMs and dozen
//! worktrees are already competing should not also be the box the gate's `cargo test` runs on.
//!
//! **`[queue] gate_host` (or `--gate-host`) chooses between them, once, at daemon start** —
//! [`set_gate_host`], the same shape as `[memory] gate` above, because the queue builds one
//! runner for the whole daemon (`QueueEnv::production`) and a setting that had to be threaded
//! through it would be a second thing the queue's own tests all had to grow a field for. Unset
//! is today's behaviour, byte for byte: [`LocalGate::run`] with nothing set is
//! [`run_with_memory`] and nothing else.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use letibot_tokencore::store::{MergeGateOutcome, MergeGateStep};
use letibot_tools::exec::memory::{Amount, Ask, Meminfo};
use letibot_tools::exec::{MemoryUse, SiblingScope, join_script};

use super::git;

/// **Thirty minutes** — the operator's decision 9. A gate that is still running then is killed and
/// its row is `timed_out`, an outcome of its own.
pub const GATE_DEADLINE: Duration = Duration::from_secs(30 * 60);

/// The environment variable the gate's builds are pointed with — `<worktree>/target`, so a branch
/// is never judged with another tree's artifacts.
pub const GATE_TARGET_DIR: &str = "CARGO_TARGET_DIR";

/// **The environment variable a step's scratch space is pointed with** — a directory of the gate's
/// own under [`GATE_TMP_ROOT`], one per run, never the `TMPDIR` the daemon's environment would give
/// the step and never a path inside the landing tree.
///
/// **Why not the box's temp dir.** A tmpfs write is shmem charged to the step's own cgroup, and not
/// reclaimable the way a dirty page on a real disk is: a step that copies a store into `TMPDIR`
/// pays for its own execution out of the memory it is measured against.
///
/// **A repository whose gate is not cargo is unaffected**: a variable no program reads is inert,
/// and a step that does not use `TMPDIR` is unaffected by it being set.
pub const GATE_TMPDIR: &str = "TMPDIR";

/// **The box's directory the gate's per-run scratch directories live in** — a real filesystem, and
/// a short path, both for a reason.
///
/// **Why not inside the landing tree.** This workspace's own tests derive AF_UNIX socket paths from
/// `std::env::temp_dir()`, and a `sun_path` is 108 bytes **including its NUL**; a root inside a
/// landing tree's path leaves no room for them. The longest socket name the workspace binds is 60
/// bytes, so a root may spend 46 and no more.
///
/// **Why `/var/tmp`.** It is on the root filesystem, so a step's scratch is page cache on a disk
/// rather than shmem charged to the step's own cgroup ([`GATE_TMPDIR`]). A box that made `/var/tmp`
/// a tmpfs would need another disk instead.
pub const GATE_TMP_ROOT: &str = "/var/tmp/letibot-gate";

/// **How much of the landing tree's name a run's directory carries** — enough that a leftover is
/// diagnosable (`letibot-a156c9c3` is the landing tree `letibot-a156c9c34c8e`); the rest of the
/// byte budget goes to the run's number, which keeps the whole path inside the 46 a root may spend.
const GATE_TMP_NAME_CHARS: usize = 16;

/// **The environment variable that bounds a step's build** — how many jobs the gate's cap pays
/// for. Like [`GATE_TARGET_DIR`], the gate knows what it is allowed to use and says so, rather than
/// letting cargo guess from the core count. See [`build_jobs`].
pub const GATE_BUILD_JOBS: &str = "CARGO_BUILD_JOBS";

/// **One rustc job per this much of the step's cap** — the gate's rule for how many compilers fit
/// in the memory it was given. A rustc job's peak is not a fixed number, but a workspace build runs
/// at roughly 0.7–0.8 GiB per job; 2 GiB is the round number above that, leaving room for the
/// linker and for the test run that follows in the same step.
pub const GATE_JOB_MEMORY: u64 = 2 << 30;

/// **The most a step's row may carry** — the excerpt *and* the note the gate appends after it
/// ([`StepMemory::evidence`]), because a cap two writers spend separately is not a cap. The row
/// is **what failed, then the end**: a failure says which test and why at the TOP of its own
/// block, and a workspace run then prints every other target after it. See [`excerpt`].
pub const EVIDENCE_BYTES: usize = 4_096;

/// **The room the row keeps for the note it appends after the excerpt** — [`StepMemory::evidence`],
/// the line about the step's cgroup and the exit code.
///
/// MEASURED 2026-10-30 on `lab2x1`: that sentence is not a fixed size. When the daemon has no
/// scope of its own it says so and NAMES the cgroup it shares — `/sys/fs/cgroup/user.slice/…/
/// session-80945.scope` — which is ~370 bytes there and ~90 on a Mac, where the same sentence
/// stops at `No such file or directory`. Two rows of one 4 KiB cap therefore came out 4 397 bytes
/// on the host and under 4 096 here, on one commit and one fixture: the excerpt was inside its cap
/// both times and the note was spent outside it. The room is now taken before the tail is cut,
/// and [`judge`] clips the note to it, so the row is inside the cap on every platform.
const NOTE_BYTES: usize = 512;

/// **The most of a step's output the excerpt may take** — [`EVIDENCE_BYTES`] less the note and the
/// newline before it. This is the cap [`read_excerpt`] hands [`excerpt`].
const EXCERPT_BYTES: usize = EVIDENCE_BYTES - NOTE_BYTES - 1;

/// **The marker of an INSTALL step** — run after the push, never before it (decision 7). The
/// gate in `AGENTS.md` lists `sh scripts/install-gate-build.sh` as its last step; a landing that
/// installed before it pushed installed code that was not on the target when the push failed.
pub const INSTALL_MARK: &str = "install-gate-build";

/// **A gate step's memory cap when `[memory] gate` says nothing: half of what is free when the
/// gate starts** — the operator's own line, because a gate is the operator's run, and the other
/// half stays for the daemon and the sessions it serves. A fraction, not a size: the boxes differ by
/// an order of magnitude, and half of what is free shrinks exactly when the box is busiest.
pub const GATE_MEMORY_DEFAULT: Amount = Amount::Fraction(0.5);

/// **The clause a target's evidence (or a branch's row) leads with when its gate's step was
/// killed before it judged the code** — at the gate's memory cap, or by a signal from outside.
/// Read like [`super::setup::GATE_SETUP_FAILED`]: the tip is UNJUDGED, not red.
pub const GATE_KILLED: &str = "the gate's step was killed before it could judge the code";

/// **The clause a target's evidence leads with when the step could not judge the code because the
/// apparatus it DEMANDS is not on the machine it ran on** — the project's own
/// `LETIBOT_REQUIRE_APPARATUS=1` prefix, refused on the far side of a host gate by
/// `letibot_tokencore::apparatus::present`.
///
/// MEASURED 2026-10-30, `main` at `7becd144` with `[queue] gate_host = "lab2x1"`: **twenty-one
/// targets failed with ONE sentence** — *LETIBOT_REQUIRE_APPARATUS is set, and a cgroup v2 subtree
/// this user may write (the process-lifetime tree) is not on this machine* — and both queues were
/// halted by a workspace called RED. Twenty-one targets with one cause is a fact about the MACHINE,
/// not about the diff, and a gate that reds for a fact about the machine is a gate whose red is
/// ignored (the same defect `apparatus::applies` was written for, one layer out).
///
/// So this is a [`RedCause`] of its own: the tip is UNJUDGED, not red. [`super::gate_host`] is what
/// stops it happening at all — it drops the demand on a machine that cannot meet it, and says so in
/// the row — and this is the net under that: a probe that has gone stale, or an apparatus the probe
/// does not know, still cannot turn one machine's absence into a verdict about a branch.
pub const APPARATUS_ABSENT: &str = "the gate could not run here";

/// The two halves of the sentence `letibot_tokencore::apparatus::present_given` refuses with —
/// *"LETIBOT_REQUIRE_APPARATUS is set, and `<what>` is not on this machine."* followed by *"This
/// run does not accept a skip."*.
///
/// Two independent phrases rather than one, because the classifier below must not fire on a test
/// that merely SAYS the words (this tree's own tests quote them): a refusal is both halves, and a
/// rewording in `tokencore` has to break both to go unnoticed.
const APPARATUS_SAID: (&str, &str) = (" is not on this machine", "does not accept a skip");

/// **What a step killed at the gate's cap is said to be** — the cap, the peak, how it was known,
/// and the line that raises it.
pub(crate) fn killed_at_cap_words(step: &str, clause: &str, how: &str, leftover: usize) -> String {
    let left = if leftover > 0 {
        format!(" {leftover} process(es) were still running in it when it ended.")
    } else {
        String::new()
    };
    format!(
        "{GATE_KILLED}: `{step}` was KILLED at the gate's memory cap ({clause}) — {how}.{left} It \
         ran out of the memory the gate gives a step, which says nothing about the code: the tip \
         is UNJUDGED, not red. The cap is `[memory] gate` in providers.toml (unset, it is half of \
         what is free when the gate starts); raise it — a size such as `gate = \"40G\"`, or a \
         larger fraction of what is free — and gate it again"
    )
}

static GATE_MEMORY: std::sync::OnceLock<Ask> = std::sync::OnceLock::new();

/// **`[memory] gate`, set once at daemon start** (`cli.rs`). Process-wide rather than a field
/// of [`LocalGate`] because it is a property of the box, read once, like `memory.high`.
pub fn set_gate_memory(ask: Ask) {
    let _ = GATE_MEMORY.set(ask);
}

/// The gate's cap as configured: [`set_gate_memory`]'s, else [`GATE_MEMORY_DEFAULT`].
pub fn gate_memory() -> Ask {
    GATE_MEMORY.get().copied().unwrap_or(Ask::Default)
}

static GATE_HOST: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();

/// **`[queue] gate_host` / `--gate-host`, set once at daemon start** (`cli.rs`) — the name of the
/// declared host the gate's steps run on, or `None` for this machine.
///
/// Process-wide for [`set_gate_memory`]'s reason: the queue builds ONE runner for the whole
/// daemon (`QueueEnv::production`'s `Arc::new(LocalGate)`) and nothing else in the queue knows
/// about a host. Set once, read by every gate the daemon runs; an empty or blank name is `None`,
/// because *`gate_host = ""`* is how a project says *here* over a value it inherited.
pub fn set_gate_host(host: Option<String>) {
    let _ = GATE_HOST.set(host.map(|h| h.trim().to_string()).filter(|h| !h.is_empty()));
}

/// The host the gate runs on, or `None` for this machine — [`set_gate_host`]'s value.
pub fn gate_host() -> Option<String> {
    GATE_HOST.get().cloned().flatten()
}

// ===== The step running now, and the tail of its log =====

/// **How much of a running step's log a snapshot reads** — 4 KiB from the END of the file, whatever
/// the file's size, so a `cargo test` that has written forty megabytes costs the same as one that has
/// written forty bytes. The tail is the last [`TAIL_LINES`] of those bytes.
pub const TAIL_BYTES: u64 = 4 * 1024;

/// **How many lines of it are kept** — a pane's detail, not a log viewer.
pub const TAIL_LINES: usize = 40;

/// One gate's landing worktree, the repository it gates, and the step in it — if any.
struct Watched {
    worktree: PathBuf,
    repository: String,
    step: Option<Running>,
}

/// A step that is running: what it is, which of how many, when it started, and where its output is
/// going. The log's path is here and nowhere else: [`tempfile_in`] makes it per step and deletes it
/// when the step ends, so *the file the gate is streaming into* is a fact only the gate has.
struct Running {
    command: String,
    /// 1-based, as a person counts steps.
    step: usize,
    of: usize,
    started_ms: u64,
    log: PathBuf,
}

/// **Every landing worktree a gate is (or was last) running in** — one process-wide list, because
/// the reader is a different thread from the writer: the queue's worker starts a gate on a thread of
/// its own, and a head's `Queues` request builds the snapshot on the server's thread.
static WATCHED: std::sync::Mutex<Vec<Watched>> = std::sync::Mutex::new(Vec::new());

fn watched() -> std::sync::MutexGuard<'static, Vec<Watched>> {
    WATCHED.lock().unwrap_or_else(|e| e.into_inner())
}

/// **Say which repository a landing worktree is gating** — one call, from the queue's worker, before
/// the gate starts ([`super::serve::RepoQueue::start_gate`]). The gate itself is handed a worktree
/// and knows nothing about rows; this is the one fact that lets a snapshot answer *which row's step
/// is this?*
pub fn watch(worktree: &Path, repository: &str) {
    let mut all = watched();
    if let Some(w) = all.iter_mut().find(|w| w.worktree == worktree) {
        w.repository = repository.to_string();
        return;
    }
    all.push(Watched {
        worktree: worktree.to_path_buf(),
        repository: repository.to_string(),
        step: None,
    });
}

/// **The step `worktree`'s gate has started** — called by both machines' step loops, so a local gate
/// and a host gate answer the pane's question the same way (a host gate streams its step back into
/// the same local log as it polls, so the tail is the same read either way).
pub(super) fn watch_step(worktree: &Path, command: &str, step: usize, of: usize, log: &Path) {
    let mut all = watched();
    if let Some(w) = all.iter_mut().find(|w| w.worktree == worktree) {
        w.step = Some(Running {
            command: command.to_string(),
            step,
            of,
            started_ms: now_ms(),
            log: log.to_path_buf(),
        });
    }
}

/// The step ended (however it ended) — the worktree has no step running any more.
fn ended(worktree: &Path) {
    let mut all = watched();
    if let Some(w) = all.iter_mut().find(|w| w.worktree == worktree) {
        w.step = None;
    }
}

/// Clears the step on every return path out of a step, including a panic: a stale entry would tell
/// a pane that a step has been running for ever.
pub(super) struct RunningFor<'a>(&'a Path);

impl Drop for RunningFor<'_> {
    fn drop(&mut self) {
        ended(self.0);
    }
}

/// The guard a step holds for as long as it runs — see [`RunningFor`].
pub(super) fn step_guard(worktree: &Path) -> RunningFor<'_> {
    RunningFor(worktree)
}

/// **What a `gating` row's detail reads**: the step, of how many, since when, and the end of its
/// output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningGate {
    pub command: String,
    pub step: usize,
    pub of: usize,
    pub started_ms: u64,
    pub tail: String,
    /// Every byte the step has written, so *the tail has not moved* can be told apart from *the
    /// step is quiet between lines* — a number that only ever grows while it works.
    pub log_bytes: u64,
}

/// **The step `repository`'s gate is running, and the tail of its log** — `None` when no gate is
/// running for it, which is the honest answer for a row that is not `gating` and for the moment
/// between two steps.
pub fn running(repository: &str) -> Option<RunningGate> {
    let (command, step, of, started_ms, log) = {
        let all = watched();
        let w = all.iter().find(|w| w.repository == repository)?;
        let running = w.step.as_ref()?;
        (
            running.command.clone(),
            running.step,
            running.of,
            running.started_ms,
            running.log.clone(),
        )
    };
    let (tail, log_bytes) = tail_of(&log);
    Some(RunningGate {
        command,
        step,
        of,
        started_ms,
        tail,
        log_bytes,
    })
}

/// **The end of a log, bounded twice** — at most [`TAIL_BYTES`] read (a seek, so the file's size
/// costs nothing to skip) and at most [`TAIL_LINES`] lines kept, plus the one line that says what
/// was dropped. A step's output is the step's own bytes and may be any length; this is the only
/// place the pane's side reads them, and it never reads more than that.
fn tail_of(log: &Path) -> (String, u64) {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(log) else {
        return (String::new(), 0);
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let from = len.saturating_sub(TAIL_BYTES);
    let mut buf = Vec::with_capacity(TAIL_BYTES as usize);
    if f.seek(SeekFrom::Start(from)).is_err() || f.take(TAIL_BYTES).read_to_end(&mut buf).is_err() {
        return (String::new(), len);
    }
    let text = String::from_utf8_lossy(&buf);
    // A read that began mid-line begins mid-word: the first line is dropped rather than drawn as
    // the step's first words, which it is not.
    let body = if from > 0 {
        text.split_once('\n').map(|(_, rest)| rest).unwrap_or("")
    } else {
        text.as_ref()
    };
    let lines: Vec<&str> = body.lines().collect();
    let from_line = lines.len().saturating_sub(TAIL_LINES);
    let mut tail = lines[from_line..].join("\n");
    if from > 0 || from_line > 0 {
        tail.insert_str(
            0,
            &format!(
                "… [the step has written {len} byte(s); what follows is its last {} line(s)]\n",
                lines.len() - from_line
            ),
        );
    }
    (tail, len)
}

/// **Where a run's steps go, decided once per run**: a cgroup of their own (with the cap, or
/// `None` for `uncapped`), or the daemon's own cgroup as before R76 and why.
#[derive(Debug, Clone, PartialEq)]
enum Placement {
    Scoped(Option<u64>),
    Unscoped(String),
}

/// `cap` is [`intended_cap`] of the same `ask`, resolved once by the caller: the cgroup and the
/// step's build are bounded by one number.
fn placement(ask: Ask, cap: Option<u64>) -> Placement {
    if let Err(why) = letibot_tools::exec::delegate_memory() {
        return Placement::Unscoped(why.clone());
    }
    if ask == Ask::Uncapped {
        return Placement::Scoped(None);
    }
    match cap {
        Some(bytes) => Placement::Scoped(Some(bytes)),
        None => Placement::Unscoped(
            "`/proc/meminfo` could not be read, so a fraction of what is free means nothing".into(),
        ),
    }
}

/// **The memory the gate means to give a step, in bytes** — the `Ask` resolved without needing a
/// cgroup, so that the same number caps the step and bounds its build ([`StepEnv`]). `uncapped` is
/// `None`; so is a fraction of what is free when `/proc/meminfo` cannot be read — no number, no
/// bound (and [`placement`] then says why the step runs in the daemon's own cgroup).
fn intended_cap(ask: Ask) -> Option<u64> {
    let amount = match ask {
        Ask::Uncapped => return None,
        Ask::Default => GATE_MEMORY_DEFAULT,
        Ask::Amount(a) => a,
    };
    match amount {
        Amount::Bytes(b) => Some(b),
        a => Meminfo::read().map(|m| a.resolve(m.available)),
    }
}

/// **How many rustc jobs the cap pays for**: one per [`GATE_JOB_MEMORY`], never fewer than one and
/// never more than the cores this process may use. `None` — the gate is `uncapped` — is `None`:
/// there is no number to bound with, and cargo's own default stands.
///
/// A `cargo test --workspace` at the core count does not fit the gate's cap, and nothing else tells
/// cargo so: the step would take its parallelism from the cores and its memory from the gate, two
/// numbers that know nothing about each other.
///
/// **A repository whose gate is not cargo is unaffected**: the variable is read by cargo and by
/// nothing else.
pub fn build_jobs(cap: Option<u64>) -> Option<usize> {
    let cap = cap?;
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let by_memory = (cap / GATE_JOB_MEMORY).max(1);
    Some(by_memory.min(cores as u64) as usize)
}

/// **What the gate tells a step about itself** — the environment a run decides rather than
/// inherits: the tree's `target/`, the run's scratch directory ([`GateScratch`]), and the build's
/// parallelism from the cap. One struct because the three are one decision about what memory and
/// what disk a step may use, and a step must be given them together.
struct StepEnv {
    target: PathBuf,
    tmp: PathBuf,
    /// `None` when the gate is `uncapped`: no number to give, so cargo's own default stands.
    jobs: Option<usize>,
}

impl StepEnv {
    /// One step's environment, for a run whose scratch is already made ([`GateScratch`]).
    fn for_run(worktree: &Path, scratch: &Path, cap: Option<u64>) -> StepEnv {
        StepEnv {
            target: gate_target_dir(worktree),
            tmp: scratch.to_path_buf(),
            jobs: build_jobs(cap),
        }
    }

    /// Point a step's command at the tree's `target/`, at its own scratch, and at the parallelism
    /// its cap pays for.
    fn apply(&self, cmd: &mut Command) {
        cmd.env(GATE_TARGET_DIR, &self.target);
        cmd.env(GATE_TMPDIR, &self.tmp);
        if let Some(jobs) = self.jobs {
            cmd.env(GATE_BUILD_JOBS, jobs.to_string());
        }
    }
}

/// **What a step's memory did**, for its row.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum StepMemory {
    /// Its own cgroup's counters.
    Used(MemoryUse),
    /// It ran in the daemon's cgroup, and why.
    Uncapped(String),
    /// **It ran on a declared host** (`mergequeue::gate_host`), where this daemon makes no cgroup
    /// and so has no peak, no cap and no count to report. A variant rather than `Uncapped`
    /// because the sentence must not claim a cgroup it never looked at: the row says where the
    /// step ran, and the model's copy of the evidence drops the line like any other quiet one.
    Elsewhere(String),
    /// It never started, or the counters could not be read.
    Unknown,
}

/// **How close to its cap a step's peak has to be for a red step that showed no failing code to
/// be read as killed at the cap** — 95%.
///
/// The kernel's own `oom_kill` count is the proof, and it is read first. This is the second
/// reading, for the death the counter does not see: letibot's own `main` at 79c32c1, 2026-10-12,
/// went RED with `peak 24.1 GiB of a 24.9 GiB cap` (96.8%), 128 processes still running in the
/// step's cgroup, its output ending in compile warnings — no test result, no `error:` — and an
/// `oom_kill` of 0. A cargo that died with its rustc's still compiling was not judging the code,
/// and a step whose peak sat within 5% of its cap was out of the memory the gate gives it.
pub const NEAR_CAP_PERCENT: u64 = 95;

/// **How the step's leader ended** — its own exit code, or the signal that ended it. Kept on the
/// row because a cargo killed by `SIGKILL` says nothing on its way out, and a red step whose exit
/// is not recorded cannot be told from a red step that judged the code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepExit {
    Code(i32),
    Signal(i32),
}

impl StepExit {
    fn of(status: std::process::ExitStatus) -> StepExit {
        use std::os::unix::process::ExitStatusExt;
        match (status.code(), status.signal()) {
            (Some(c), _) => StepExit::Code(c),
            (None, Some(s)) => StepExit::Signal(s),
            (None, None) => StepExit::Code(-1),
        }
    }

    fn clause(&self) -> String {
        match self {
            StepExit::Code(c) => format!("exit code {c}"),
            StepExit::Signal(s) => format!("killed by signal {s}{}", signal_name(*s)),
        }
    }
}

fn signal_name(s: i32) -> &'static str {
    match s {
        libc::SIGKILL => " (SIGKILL)",
        libc::SIGTERM => " (SIGTERM)",
        libc::SIGINT => " (SIGINT)",
        libc::SIGSEGV => " (SIGSEGV)",
        libc::SIGABRT => " (SIGABRT)",
        libc::SIGBUS => " (SIGBUS)",
        _ => "",
    }
}

/// **Why a step that did not pass did not pass** — the classifier between *the code is red* and
/// *the gate could not judge it*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedCause {
    /// The step judged the code and it failed.
    Code,
    /// The step ran out of the gate's memory: the kernel killed in it at the cap, or it died
    /// within [`NEAR_CAP_PERCENT`] of the cap without the code ever failing. The words say which.
    KilledAtCap(String),
    /// The step's leader was killed by a signal from outside the gate, far from its cap, before
    /// any code failed.
    KilledBySignal(i32),
    /// **The step demanded apparatus this machine does not have** — `LETIBOT_REQUIRE_APPARATUS=1`
    /// refused a skip, and the refusal is the whole of what the step says went wrong. The string is
    /// the apparatus in the tree's own words (*"a cgroup v2 subtree this user may write (the
    /// process-lifetime tree)"*). Twenty-one targets with one cause is a fact about the machine:
    /// nothing was judged about the code (see [`APPARATUS_ABSENT`]).
    ApparatusAbsent(String),
}

/// **The apparatus a `LETIBOT_REQUIRE_APPARATUS` refusal names**, out of the step's own words — or
/// `None` when this output is not that refusal.
///
/// MEASURED 2026-10-30 on `lab2x1`, `LETIBOT_REQUIRE_APPARATUS=1 cargo test --no-fail-fast -p
/// letibot-harnessd --lib`: **sixteen of seventeen failures are one refusal**, printed as
///
/// ```text
/// thread 'jobwatch::tests::a_subagent_settles_through_the_jobs_own_channel' (805) panicked at
/// crates/tokencore/src/apparatus.rs:181:9:
/// LETIBOT_REQUIRE_APPARATUS is set, and a process-lifetime tree (cgroup v2; process groups on
/// macOS) is not on this machine.
/// ```
///
/// — and the refusal is on the line AFTER the `panicked at` one, which a step run with
/// `-- --nocapture` does not promise to keep adjacent. So the rule is a COUNT and not a pairing:
/// every panic in the output must have a refusal to go with it. A genuine failure anywhere (an
/// assertion, a compile error, a format diff) leaves a panic with no refusal behind it, and then
/// the step judged the code whatever else it said.
fn apparatus_absent(out: &str) -> Option<String> {
    let (refused, skip) = APPARATUS_SAID;
    if !out.contains(refused) || !out.contains(skip) {
        return None;
    }
    let panics = out.matches("panicked at").count();
    let refusals = out.matches(refused).count();
    if refusals < panics {
        return None;
    }
    let elsewhere = super::setup::step_words(out).any(|l| {
        let t = l.trim_start();
        t.starts_with("error[")
            || t.starts_with("error: could not compile")
            || t.starts_with("Diff in ")
    });
    if elsewhere {
        return None;
    }
    // The apparatus, in the refusal's own words: `LETIBOT_REQUIRE_APPARATUS is set, and <what> is
    // not on this machine.` A workspace run names several, so all of them are collected — the row
    // says what the machine has not got, and one sentence per thing is what a reader acts on.
    let mut said: Vec<String> = Vec::new();
    for line in out.lines().filter(|l| l.contains(refused)) {
        let Some(from) = line.find("is set, and ").map(|i| i + "is set, and ".len()) else {
            continue;
        };
        let rest = &line[from..];
        let Some(to) = rest.find(refused) else {
            continue;
        };
        let what = rest[..to].trim().to_string();
        if !what.is_empty() && !said.contains(&what) {
            said.push(what);
        }
    }
    if said.is_empty() {
        said.push("apparatus it demands".to_string());
    }
    // One per line, because an apparatus NAME has commas and semicolons of its own —
    // `a usable confinement boundary (bwrap, an unprivileged user namespace, a cgroup v2
    // subtree)` — so no punctuation can tell *one name* from *two*.
    Some(said.join("\n"))
}

/// **What a step that demanded absent apparatus is said to be** — the machine, what it has not got,
/// and the two ways out. Never *red*: the code was not read.
pub(crate) fn apparatus_absent_words(step: &str, what: &str, machine: Option<&str>) -> String {
    let here = match machine {
        Some(h) => format!("`{h}`"),
        None => "this machine".to_string(),
    };
    // One thing missing reads as the brief's own sentence — *"the gate could not run here: a
    // cgroup v2 subtree … is not on `lab2x1`"*; several read as a list, because "A; B is not" is
    // not English. The names are newline-separated by [`apparatus_absent`] for this reason.
    let names: Vec<&str> = what.split('\n').collect();
    let absent = match names.as_slice() {
        [one] => format!("{one} is not on {here}"),
        many => format!("these are not on {here}: {}", many.join("; ")),
    };
    format!(
        "{APPARATUS_ABSENT}: {absent}, and `{step}` demands it \
         (`LETIBOT_REQUIRE_APPARATUS=1`), so the step refused to skip and nothing it would have \
         judged was judged. This is a fact about the MACHINE and not about the code: the tip is \
         UNJUDGED, not red, and the queue is not halted by it. Either provision the machine — \
         `letibot_tokencore::apparatus` names each thing and what supplies it — or take the demand \
         off the step for this machine, which the host gate does by itself and says in the row's \
         evidence"
    )
}

/// Output that shows a kill rather than a verdict: cargo's own sentence for a child (a rustc, a
/// test binary) that a signal ended.
fn shows_a_kill(out: &str) -> bool {
    out.contains("(signal: 9, SIGKILL") || out.contains("(signal: 15, SIGTERM")
}

/// **Read a red step** ([`RedCause`]). Pure, so the measured case is a unit test: `exit` is how
/// the leader ended, `memory` what its cgroup's counters said, `out` the end of its output.
fn red_cause(out: &str, exit: StepExit, memory: &StepMemory) -> RedCause {
    let code_failed = super::setup::shows_code_failing(out) && !shows_a_kill(out);
    match memory {
        StepMemory::Used(u) if u.cap.is_some() && u.killed_at_cap() => {
            return RedCause::KilledAtCap(format!(
                "the kernel killed {} process(es) inside it at the cap",
                u.oom_kills
            ));
        }
        StepMemory::Used(u) if !code_failed && u.near_cap() => {
            // R93.F10: no percent and no gaps — the words are what reaches a reviewer.
            return RedCause::KilledAtCap(format!(
                "it died near its memory cap ({}) with no failing code in its output — out of \
                 memory, not a verdict",
                exit.clause()
            ));
        }
        _ => {}
    }
    // **The machine's absence, before the code's** — a `LETIBOT_REQUIRE_APPARATUS` refusal is a
    // PANIC, and [`super::setup::shows_code_failing`] reads `panicked at` as the code failing, so
    // the order here is the whole of the fix: twenty-one targets that all refused to skip are one
    // fact about the machine (MEASURED 2026-10-30, see [`APPARATUS_ABSENT`]).
    if let Some(what) = apparatus_absent(out) {
        return RedCause::ApparatusAbsent(what);
    }
    match exit {
        StepExit::Signal(s) if !code_failed => RedCause::KilledBySignal(s),
        _ => RedCause::Code,
    }
}

/// **The gate's evidence as a model reads it** (R93.F8): the `[gate memory: …]` line is dropped
/// unless the step was killed at its cap or came near it. The row keeps it for the operator; the
/// model is not handed peaks to size work by.
pub fn for_the_model(evidence: &str) -> String {
    evidence
        .lines()
        .filter(|l| {
            !l.starts_with("[gate memory: ") || l.contains("KILLED") || l.contains("near its cap")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

trait NearCap {
    fn near_cap(&self) -> bool;
}

impl NearCap for MemoryUse {
    fn near_cap(&self) -> bool {
        self.cap.is_some_and(|cap| {
            cap > 0 && self.peak.saturating_mul(100) >= cap.saturating_mul(NEAR_CAP_PERCENT)
        })
    }
}

impl StepMemory {
    fn killed_at_cap(&self) -> bool {
        matches!(self, StepMemory::Used(u) if u.cap.is_some() && u.killed_at_cap())
    }

    /// The line the row's evidence ends with.
    fn evidence(&self, leftover: usize, exit: Option<StepExit>) -> Option<String> {
        let mut line = match self {
            StepMemory::Used(u) if self.killed_at_cap() => format!(
                "[gate memory: KILLED at the gate's cap — {}; the kernel killed {} process(es) \
                 inside it. This is `[memory] gate` in providers.toml, not the code]",
                u.clause(),
                u.oom_kills
            ),
            StepMemory::Used(u) if u.near_its_cap() => {
                format!("[gate memory: {} — near its cap]", u.clause())
            }
            StepMemory::Used(u) => format!("[gate memory: {}]", u.clause()),
            StepMemory::Uncapped(why) => {
                format!("[gate memory: uncapped — the step ran in the daemon's own cgroup: {why}]")
            }
            StepMemory::Elsewhere(host) => format!(
                "[gate memory: not measured — the step ran on `{host}`, where this daemon has no \
                 cgroup of its own]"
            ),
            StepMemory::Unknown => match exit {
                Some(e) => return Some(format!("[step: {}]", e.clause())),
                None => return None,
            },
        };
        if let Some(e) = exit {
            line.push_str(&format!("\n[step: {}]", e.clause()));
        }
        if leftover > 0 {
            line.push_str(&format!(
                "\n[{leftover} process(es) the step left running were killed with its cgroup]"
            ));
        }
        Some(line)
    }
}

/// **What the gate came to** — four outcomes and not one, because *the branch is bad*, *nobody
/// could tell in time*, *the daemon stopped* and *the gate could not run* send a person to four
/// different places.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateVerdict {
    /// Every step was green.
    Passed,
    /// A step was red: the branch (or the target) is bad. The step and the tail of its output.
    Failed(String),
    /// **The deadline passed and the gate was killed** — not `failed`.
    TimedOut(String),
    /// The daemon is stopping (or lost its lease): the gate was killed and judged nothing.
    Stopped(String),
    /// The gate could not run at all — a step that could not be spawned, an output file that
    /// could not be made. Infrastructure, not the branch.
    Infra(String),
}

/// One run: the rows (one per step, in order) and the verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateRun {
    pub steps: Vec<MergeGateStep>,
    pub verdict: GateVerdict,
}

/// **The controls a run is under** — its deadline, the daemon's stop flag, and the heartbeat,
/// which answers `false` when the lease is gone (the gate is then killed: a daemon that does not
/// own the repository must not land into it).
pub struct GateControl<'a> {
    pub deadline: Instant,
    /// **How much time the deadline is** — `deadline` is the instant it falls on, and a sentence
    /// about a timeout has to name the *budget* it was given (`[queue]`'s `gate_deadline`, thirty
    /// minutes unless set) rather than leave a reader guessing which clock ran out. Two clocks run
    /// on a gate and they are not the same number: a poll on a declared host is bounded by
    /// seconds, and the step is bounded by this.
    pub budget: Duration,
    pub stop: &'a AtomicBool,
    pub alive: &'a dyn Fn() -> bool,
}

/// **Who runs a gate** — R62's seam, and the fleet is now one of its two implementations.
pub trait GateRunner: Send + Sync {
    /// Run `steps` in `worktree`, in order, stopping at the first red one.
    fn run(&self, worktree: &Path, steps: &[String], ctl: &GateControl) -> GateRun;
    /// What this runner is, for the row: `local`, or the host's name.
    fn name(&self) -> String {
        "local".to_string()
    }
}

/// **The daemon's gate** — decision 8: *"the gate runs here for now"*, and, since the operator's
/// 2026-10-29 ruling, on a declared host when `[queue] gate_host` / `--gate-host` names one.
///
/// The queue builds this runner in exactly one place (`QueueEnv::production`), so the choice
/// between the two machines is made *here*, at the seam the queue already has, rather than by a
/// second constructor every fixture would have to grow. Nothing set is [`run_with_memory`] and
/// nothing else: today's behaviour, byte for byte.
#[derive(Debug, Default, Clone)]
pub struct LocalGate;

impl GateRunner for LocalGate {
    fn run(&self, worktree: &Path, steps: &[String], ctl: &GateControl) -> GateRun {
        run_gate(gate_host().as_deref(), worktree, steps, ctl, gate_memory())
    }

    /// The name the row carries: the host's, or `local`.
    fn name(&self) -> String {
        gate_host().unwrap_or_else(|| "local".to_string())
    }
}

/// **The gate on one declared host** — `hosts.toml`'s machine, reached over its own key and root
/// (`mergequeue::gate_host`).
///
/// A type rather than only the setting, because it is the second implementation of
/// [`GateRunner`] that [`LocalGate`]'s own doc has promised since R61, and because a test (or an
/// operator with a `--gate-host` on the command line) can name a host without going through the
/// process-wide value.
#[derive(Debug, Clone)]
pub struct HostGate {
    pub host: String,
}

impl GateRunner for HostGate {
    fn run(&self, worktree: &Path, steps: &[String], ctl: &GateControl) -> GateRun {
        super::gate_host::run_on_host(&self.host, worktree, steps, ctl)
    }

    fn name(&self) -> String {
        self.host.clone()
    }
}

/// **[`LocalGate`]'s run, under a memory cap named by the caller** — `[memory] gate` in the
/// daemon, a small one in the test that proves a step is killed at it alone.
pub fn run_with_memory(
    worktree: &Path,
    steps: &[String],
    ctl: &GateControl,
    memory: Ask,
) -> GateRun {
    // **The cap is resolved once and used twice**: the cgroup the step joins is capped with it and
    // the build's parallelism is bounded by it ([`StepEnv`]) — a step judged by two different
    // numbers is the bug this closes.
    let cap = intended_cap(memory);
    let place = placement(memory, cap);
    // **One scratch for the whole run, and the gate owns it**: made here, handed to every step
    // ([`StepEnv`]), and removed however this function returns — which is why it is a `Drop`
    // ([`GateScratch`]) and not a call at the end of the loop below.
    let scratch = match GateScratch::for_run(worktree) {
        Ok(s) => s,
        Err(why) => return could_not_run(steps, why),
    };
    let mut done: Vec<Done> = Vec::with_capacity(steps.len());
    for (i, step) in steps.iter().enumerate() {
        let started = now_ms();
        let (end, mem, leftover) = run_step(
            worktree,
            step,
            (i + 1, steps.len()),
            scratch.path(),
            cap,
            ctl,
            &place,
        );
        let elapsed = now_ms().saturating_sub(started);
        let judged = judge(step, end, mem, leftover, ctl.budget);
        let last = judged.verdict.is_some();
        done.push(Done {
            command: step.clone(),
            started_ms: started,
            elapsed_ms: elapsed,
            judged,
        });
        if last {
            break;
        }
    }
    assemble(steps, done)
}

/// **The one place a step's end becomes a row and a verdict** — shared by both machines, so a
/// host gate cannot read a red step differently from a local one.
pub(crate) fn judge(
    step: &str,
    outcome: StepEnd,
    mem: StepMemory,
    leftover: usize,
    budget: Duration,
) -> Judged {
    let exit = match &outcome {
        StepEnd::Red(_, e) => Some(*e),
        _ => None,
    };
    let evidence = mem.evidence(leftover, exit);
    // **The note is inside the row's cap too** — [`read_excerpt`] leaves [`NOTE_BYTES`] for it,
    // and this is what makes that a budget rather than an expectation.
    let note = evidence.map(|line| cut_to(&line, NOTE_BYTES, " …"));
    let with_memory = |out: String| match &note {
        Some(line) if out.is_empty() => line.clone(),
        Some(line) => format!("{out}\n{line}"),
        None => out,
    };
    let (word, output, verdict) = match outcome {
        StepEnd::Red(out, exit) => {
            // **A step that did not pass is read once more before it is called red.**
            // Killed at the gate's cap — or near it with no failing code — is the gate's
            // limit, and killed by a signal before any code failed is somebody else's hand:
            // neither judged the branch. So the row says `killed` and the verdict is the
            // gate's own (`Infra`: the target UNJUDGED and blocked, the branch not failed),
            // naming the cap, the peak, and the line that raises it.
            let cause = red_cause(&out, exit, &mem);
            let clause = match &mem {
                StepMemory::Used(u) => u.clause(),
                StepMemory::Uncapped(_) => "uncapped".to_string(),
                StepMemory::Elsewhere(host) => format!("on `{host}`"),
                StepMemory::Unknown => "its memory could not be read".to_string(),
            };
            // **A host gate's step is not in a cgroup this daemon made**, so the sentence must
            // not claim a count it never took. Same words everywhere else.
            let not_the_cap = match &mem {
                StepMemory::Elsewhere(_) => {
                    format!("{clause}; the gate's memory cap is not in force on that machine")
                }
                _ => format!("{clause}; no OOM kill in its cgroup, so not by the gate's cap"),
            };
            let output = with_memory(out);
            match cause {
                RedCause::Code => (
                    MergeGateOutcome::Failed,
                    output.clone(),
                    Some(GateVerdict::Failed(format!("`{step}` failed:\n{output}"))),
                ),
                RedCause::KilledAtCap(how) => (
                    MergeGateOutcome::Killed,
                    output,
                    Some(GateVerdict::Infra(killed_at_cap_words(
                        step, &clause, &how, leftover,
                    ))),
                ),
                RedCause::KilledBySignal(sig) => (
                    MergeGateOutcome::Killed,
                    output,
                    Some(GateVerdict::Infra(format!(
                        "{GATE_KILLED}: `{step}` was {} before any code failed ({not_the_cap}). It \
                         judged nothing about the code — the tip is UNJUDGED, not red. Gate it \
                         again once whatever killed it is gone",
                        StepExit::Signal(sig).clause()
                    ))),
                ),
                // **The machine, not the branch.** The step demanded apparatus this machine has
                // not got, so it refused to skip and nothing it would have judged was judged. The
                // row is `not_run` — a step that judged nothing is not a step that was green, and
                // it is not a step that was red either — and the verdict is the gate's own.
                RedCause::ApparatusAbsent(what) => (
                    MergeGateOutcome::NotRun,
                    output,
                    Some(GateVerdict::Infra(apparatus_absent_words(
                        step,
                        &what,
                        match &mem {
                            StepMemory::Elsewhere(host) => Some(host.as_str()),
                            _ => None,
                        },
                    ))),
                ),
            }
        }
        StepEnd::Ok(out) => (MergeGateOutcome::Passed, with_memory(out), None),
        // **The step's own bound, named.** This is the gate's deadline (`[queue]`'s
        // `gate_deadline`) and not any poll's: a step is minutes, a question about the host is
        // seconds, and a verdict has to say which clock ran out or a reader cannot tell a slow
        // build from a slow machine.
        StepEnd::Deadline(out) => (
            MergeGateOutcome::TimedOut,
            with_memory(out),
            Some(GateVerdict::TimedOut(format!(
                "`{step}` was still running when the gate's own deadline passed ({}) and was killed \
                 with its whole process group",
                clause_of(budget)
            ))),
        ),
        StepEnd::Stopped(out, why) => (
            MergeGateOutcome::Killed,
            with_memory(out),
            Some(GateVerdict::Stopped(format!("`{step}` was killed: {why}"))),
        ),
        StepEnd::Infra(why) => (
            MergeGateOutcome::NotRun,
            why.clone(),
            Some(GateVerdict::Infra(format!("`{step}` could not run: {why}"))),
        ),
    };
    Judged {
        outcome: word,
        output,
        verdict,
    }
}

/// **A run that could not start at all** — a scratch directory the gate cannot make. Every step's
/// row is `NotRun` and the verdict is `Infra`: never `failed`, which would say the branch is red
/// when nothing about it was judged.
fn could_not_run(steps: &[String], why: String) -> GateRun {
    let mut rows: Vec<MergeGateStep> = steps
        .iter()
        .map(|s| MergeGateStep {
            command: s.clone(),
            outcome: MergeGateOutcome::NotRun,
            output: String::new(),
            started_ms: 0,
            elapsed_ms: 0,
        })
        .collect();
    if let Some(first) = rows.first_mut() {
        first.output = why.clone();
    }
    GateRun {
        steps: rows,
        verdict: GateVerdict::Infra(format!("the gate could not run: {why}")),
    }
}

/// A duration as a sentence says it — `30 min`, `90 s`, `1500 ms`.
pub(crate) fn clause_of(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0 => format!("{} ms", d.as_millis()),
        s if s % 60 == 0 => format!("{} min", s / 60),
        s => format!("{s} s"),
    }
}

/// One step, decided: the row's two words and the verdict it forces, if any.
pub(crate) struct Judged {
    pub outcome: MergeGateOutcome,
    pub output: String,
    pub verdict: Option<GateVerdict>,
}

/// One step that RAN, with when it did and what came of it.
pub(crate) struct Done {
    pub command: String,
    pub started_ms: u64,
    pub elapsed_ms: u64,
    pub judged: Judged,
}

/// **The rows and the verdict, from the steps that ran** — `done` is a prefix of `steps`, and the
/// step that ended the run is the last of it. Every step after that one is `not_run` with no
/// output, because a step the gate never reached is not a step that was green.
pub(crate) fn assemble(steps: &[String], done: Vec<Done>) -> GateRun {
    let mut rows: Vec<MergeGateStep> = Vec::with_capacity(steps.len());
    let mut verdict = GateVerdict::Passed;
    let mut stopped: Option<usize> = None;
    for d in done {
        rows.push(MergeGateStep {
            command: d.command,
            outcome: d.judged.outcome,
            output: d.judged.output,
            started_ms: d.started_ms,
            elapsed_ms: d.elapsed_ms,
        });
        if let Some(v) = d.judged.verdict {
            verdict = v;
            stopped = Some(rows.len());
            break;
        }
    }
    if let Some(n) = stopped {
        rows.extend(steps[n..].iter().map(|s| MergeGateStep {
            command: s.clone(),
            outcome: MergeGateOutcome::NotRun,
            output: String::new(),
            started_ms: 0,
            elapsed_ms: 0,
        }));
    }
    GateRun {
        steps: rows,
        verdict,
    }
}

/// **The gate, on the machine the daemon was told to use** — `host` `None` is this machine.
pub fn run_gate(
    host: Option<&str>,
    worktree: &Path,
    steps: &[String],
    ctl: &GateControl,
    memory: Ask,
) -> GateRun {
    match host {
        Some(name) => super::gate_host::run_on_host(name, worktree, steps, ctl),
        None => run_with_memory(worktree, steps, ctl, memory),
    }
}

/// How one step ended, as the two machines report it.
pub(crate) enum StepEnd {
    Ok(String),
    Red(String, StepExit),
    Deadline(String),
    Stopped(String, String),
    Infra(String),
}

/// One step: its end, what its memory did, and how many processes it left behind in its
/// cgroup (killed with it). `at` is which step this is, 1-based, and `of` how many the gate has —
/// the `2 of 4` a pane reads beside the tail.
fn run_step(
    worktree: &Path,
    step: &str,
    at: (usize, usize),
    scratch: &Path,
    cap: Option<u64>,
    ctl: &GateControl,
    place: &Placement,
) -> (StepEnd, StepMemory, usize) {
    use std::os::unix::process::CommandExt;
    let env = StepEnv::for_run(worktree, scratch, cap);
    let log = match tempfile_in(worktree) {
        Ok(l) => l,
        Err(e) => return (StepEnd::Infra(e), StepMemory::Unknown, 0),
    };
    let (out, err) = match (
        std::fs::File::create(&log),
        std::fs::OpenOptions::new().append(true).open(&log),
    ) {
        (Ok(o), Ok(e)) => (o, e),
        (Err(e), _) | (_, Err(e)) => {
            return (
                StepEnd::Infra(format!("{}: {e}", log.display())),
                StepMemory::Unknown,
                0,
            );
        }
    };
    // **The step a pane can see from here on** — the log's path is this function's secret (the
    // file is created here and removed on the way out), so it is said once, here, and cleared by
    // the guard whatever way the step ends.
    watch_step(worktree, step, at.0, at.1, &log);
    let _running = step_guard(worktree);
    // **The step's own cgroup**, or the daemon's as before and why. A scope that cannot be
    // made is not a reason to refuse the gate — the gate ran uncapped for its whole life
    // before R76 — so it runs as before and the row says so.
    let (scope, unscoped) = match place {
        Placement::Scoped(cap) => match SiblingScope::open("gate", *cap) {
            Ok(s) => (Some(s), None),
            Err(why) => (None, Some(why)),
        },
        Placement::Unscoped(why) => (None, Some(why.clone())),
    };
    // The host-owned evidence that the join happened (`join_script`'s `$2`): a file the step's
    // own output cannot write.
    let joined = log.with_extension("joined");
    let mut cmd = Command::new("sh");
    match &scope {
        Some(s) => cmd
            .arg("-c")
            .arg(join_script())
            .arg("letibot-gate")
            .arg(s.procs_path())
            .arg(&joined)
            .args(["sh", "-c", step]),
        None => cmd.arg("-c").arg(step),
    };
    env.apply(&mut cmd);
    let child = cmd
        .current_dir(worktree)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .process_group(0)
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            let _ = std::fs::remove_file(&log);
            return (
                StepEnd::Infra(format!("sh -c {step:?}: {e}")),
                StepMemory::Unknown,
                0,
            );
        }
    };
    let pgid = child.id() as i32;
    let kill = |child: &mut std::process::Child| kill_group(pgid, child, scope.as_ref());
    let mut last_beat = Instant::now();
    let end = loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let text = read_excerpt(&log);
                break if scope.is_some() && !joined.exists() {
                    // The wrapper could not put itself in the cgroup, so the step never ran.
                    StepEnd::Infra(format!(
                        "`{step}` could not join its gate cgroup and was NOT run:\n{text}"
                    ))
                } else if status.success() {
                    StepEnd::Ok(text)
                } else {
                    StepEnd::Red(text, StepExit::of(status))
                };
            }
            Ok(None) => {}
            Err(e) => {
                kill(&mut child);
                break StepEnd::Infra(format!("waiting on `{step}`: {e}"));
            }
        }
        if Instant::now() >= ctl.deadline {
            kill(&mut child);
            break StepEnd::Deadline(read_excerpt(&log));
        }
        if ctl.stop.load(Ordering::Relaxed) {
            kill(&mut child);
            break StepEnd::Stopped(read_excerpt(&log), "the daemon is stopping".into());
        }
        if last_beat.elapsed() >= Duration::from_secs(5) {
            last_beat = Instant::now();
            if !(ctl.alive)() {
                kill(&mut child);
                break StepEnd::Stopped(
                    read_excerpt(&log),
                    "this daemon lost the repository's lease, so it may not land".into(),
                );
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let _ = std::fs::remove_file(&log);
    let _ = std::fs::remove_file(&joined);
    // Read BEFORE the close, which removes the files the kernel counts in.
    let (memory, leftover) = match scope {
        Some(s) => (
            s.memory_use()
                .map(StepMemory::Used)
                .unwrap_or(StepMemory::Unknown),
            s.close(),
        ),
        None => (StepMemory::Uncapped(unscoped.unwrap_or_default()), 0),
    };
    (end, memory, leftover)
}

/// `SIGTERM` to the whole group, five seconds, then `SIGKILL` — and `cgroup.kill` on the step's
/// own cgroup when it has one, which reaches a grandchild that left the group (`setsid`, a
/// daemonising test server) — and the child reaped.
fn kill_group(pgid: i32, child: &mut std::process::Child, scope: Option<&SiblingScope>) {
    // SAFETY: `killpg` with a pid this process spawned into its own group; a group that is
    // already gone answers ESRCH, which is the outcome wanted.
    unsafe {
        libc::killpg(pgid, libc::SIGTERM);
    }
    let until = Instant::now() + Duration::from_secs(5);
    while Instant::now() < until {
        if let Ok(Some(_)) = child.try_wait() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // SAFETY: as above. Sent even when the leader exited: a grandchild may still hold the group.
    unsafe {
        libc::killpg(pgid, libc::SIGKILL);
    }
    if let Some(s) = scope {
        let _ = s.kill();
    }
    let _ = child.wait();
}

pub(crate) fn tempfile_in(worktree: &Path) -> Result<PathBuf, String> {
    let dir = std::env::temp_dir().join("letibot-gate");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    // **The gate's own log, in the box's temp dir and not the step's scratch**: the step's `TMPDIR`
    // points elsewhere, so it cannot write its own evidence. The sequence number keeps two gates
    // starting in one millisecond from sharing one log and reading each other's truncation.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let name = format!(
        "{}-{}-{}-{}.log",
        std::process::id(),
        now_ms(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        worktree
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    );
    Ok(dir.join(name))
}

/// **A step's output as its row keeps it** — [`excerpt`] at [`EXCERPT_BYTES`], the room left of
/// [`EVIDENCE_BYTES`] once the note [`judge`] appends after it has been paid for. Both machines
/// read a step's output through here ([`super::gate_host`] included), so the row's arithmetic is
/// the same on both.
pub(crate) fn read_excerpt(log: &Path) -> String {
    let bytes = std::fs::read(log).unwrap_or_default();
    excerpt(&String::from_utf8_lossy(&bytes), EXCERPT_BYTES)
}

/// The end of `text`, **at most `cap` bytes in all** — the line that says what was left out is
/// inside the cap and not added to it, and it names the byte count it actually dropped. `what`
/// names the bytes the reader is missing.
fn end_of(text: &str, cap: usize, what: &str) -> String {
    if text.len() <= cap {
        return text.to_string();
    }
    // **The marker's own width depends on the number in it**, so the room left for the tail is
    // taken from the WIDEST marker this text can produce — its length's own digits. A narrower
    // one then leaves a few bytes of the cap unused, which is the harmless direction.
    let marker = |dropped: usize| format!("… [{dropped} byte(s) {what}]\n");
    let room = cap.saturating_sub(marker(text.len()).len());
    let mut start = text.len().saturating_sub(room);
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    let out = format!("{}{}", marker(start), &text[start..]);
    if out.len() <= cap {
        return out;
    }
    // Only a cap too small to hold the marker itself reaches here, and the cap is a cap: what is
    // left is cut at a character boundary rather than returned over it.
    let mut end = cap;
    while end > 0 && !out.is_char_boundary(end) {
        end -= 1;
    }
    out[..end].to_string()
}

/// The end of `text`, at most `cap` bytes, saying how much was dropped from the front.
pub fn tail(text: &str, cap: usize) -> String {
    end_of(text, cap, "dropped from the front")
}

/// **The most of a step's evidence the failure section may take**, so a tail survives beside it.
/// The section is clipped to this even when the cap would allow more: what follows a failure is
/// still worth reading when the failure was a build error.
const FAILURE_BYTES: usize = 2_400;

/// How many failing targets the section describes in full; the rest are counted.
const FAILING_TARGETS: usize = 4;

/// How many lines of a panic follow its `panicked at` line — the assertion, the io error.
const PANIC_LINES: usize = 2;

/// The width a quoted line is clipped to, so one long assertion cannot fill the section.
const QUOTED_WIDTH: usize = 240;

/// **What the failure section says when the cap cannot hold even it** — the LAST thing cut, and
/// only when there is nothing else left to cut.
const SECTION_CUT: &str = "\n[gate excerpt: the failure section itself was cut here]";

/// **The least tail worth keeping** — below this the end is left out and the section says so,
/// rather than the reader getting a marker and three lines.
const MIN_TAIL: usize = 320;

/// **The words that say the MACHINE refused the step something** — a `fork` with no descriptor
/// left, an allocation the box could not make, a disk with no room. MEASURED 2026-10-29: the
/// failure the operator suspected hid behind `error: 1 target failed: -p letibot-harnessd --lib`
/// was a `fork` at the 256-descriptor ceiling, which reaches the output as `Too many open files`
/// (or, from `std::io::Error`'s own Display, `Too many open files (os error 24)`).
const EXHAUSTED: [&str; 5] = [
    "Too many open files",
    "os error 24",
    "Cannot allocate memory",
    "No space left on device",
    "Resource temporarily unavailable",
];

/// **What one failing target's output said** — the first test that failed in it and why.
#[derive(Debug, PartialEq)]
struct FailingTarget {
    /// Cargo's own clause for the target: `-p letibot-harnessd --lib`, or the descriptor libtest
    /// printed when cargo never named it (`unittests src/lib.rs`).
    target: String,
    /// The first test that failed in it.
    test: Option<String>,
    /// The first lines of the panic: its `panicked at` line and what it said.
    panic: Vec<String>,
    /// How many tests failed in it, as far as the output names them.
    failed: usize,
}

/// **What a step's WHOLE output says about the code** — the counts, cargo's own list of failing
/// targets, and the first failure of each. Read from all of the stream, which is the point: the
/// end of a `cargo test --workspace --no-fail-fast` run is every OTHER target's `running 0 tests`
/// blocks, so the end says nothing about what failed.
#[derive(Debug, Default, PartialEq)]
struct Failures {
    /// Targets libtest started — one `running N tests` line each.
    targets_ran: usize,
    targets_passed: usize,
    targets_failed: usize,
    tests_passed: u64,
    tests_failed: u64,
    /// Cargo's verdict verbatim: `error: N target(s) failed:` and the names under it.
    summary: Vec<String>,
    /// One per failing target, in the order the targets' output ended.
    targets: Vec<FailingTarget>,
    /// Lines from anywhere in the stream that say the machine refused the step something.
    exhausted: Vec<String>,
}

/// `s` cut to `width` bytes at a character boundary, saying it was cut.
fn clip(s: &str, width: usize) -> String {
    if s.len() <= width {
        return s.to_string();
    }
    let mut end = width;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{} …", &s[..end])
}

/// `s` cut to **at most `room` bytes** and ending with `marker` — the marker is inside the room,
/// not added to it. When even the marker does not fit, the marker is what gives way: the answer is
/// never longer than the room, because a cap that is exceeded by the sentence about exceeding it
/// is how the row came out 338 bytes over on `lab2x1`.
fn cut_to(s: &str, room: usize, marker: &str) -> String {
    let mut end = room.saturating_sub(marker.len()).min(s.len());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = String::with_capacity(room);
    out.push_str(&s[..end]);
    out.push_str(marker);
    if out.len() > room {
        let mut at = room;
        while at > 0 && !out.is_char_boundary(at) {
            at -= 1;
        }
        out.truncate(at);
    }
    out
}

/// The number immediately before `what` in `s` — `782` for `… 782 passed; 1 failed; …`.
fn number_before(s: &str, what: &str) -> u64 {
    s.find(what)
        .and_then(|i| s[..i].split(|c: char| !c.is_ascii_digit()).next_back())
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

/// Cargo's own verdict on failing test targets: `error: 2 targets failed:`.
fn targets_failed_head(s: &str) -> bool {
    let Some(rest) = s.strip_prefix("error: ") else {
        return false;
    };
    let Some((n, rest)) = rest.split_once(' ') else {
        return false;
    };
    n.parse::<usize>().is_ok() && (rest == "target failed:" || rest == "targets failed:")
}

/// `error: test failed, to rerun pass `-p letibot-harnessd --lib`` → the clause in backticks.
/// Cargo writes it once per failing target, AFTER that target's output, so it is the boundary
/// that says whose failures the lines before it are.
fn rerun_clause(s: &str) -> Option<String> {
    let rest = s.strip_prefix("error: test failed, to rerun pass ")?;
    Some(rest.trim().trim_matches('`').to_string())
}

/// `test worker::a_test ... FAILED` → `worker::a_test`.
fn test_failed_name(s: &str) -> Option<String> {
    let rest = s.strip_prefix("test ")?.strip_suffix(" ... FAILED")?;
    Some(rest.to_string())
}

/// `---- worker::a_test stdout ----` → `worker::a_test` (libtest's header for a failing test's
/// own output).
fn block_name(s: &str) -> Option<String> {
    let rest = s.strip_prefix("---- ")?.strip_suffix(" stdout ----")?;
    Some(rest.to_string())
}

/// `running 12 tests` — one per target libtest started, so the count says how much of a run the
/// output is talking about. A workspace run whose every target prints `running 0 tests` is
/// otherwise indistinguishable from a run that did nothing.
fn running_count(s: &str) -> Option<usize> {
    let rest = s.strip_prefix("running ")?;
    let n = rest.split(' ').next()?.parse::<usize>().ok()?;
    rest.contains(" test").then_some(n)
}

/// `     Running unittests src/lib.rs (target/debug/deps/x-1)` → `unittests src/lib.rs`.
fn running_descriptor(s: &str) -> Option<String> {
    let rest = s.strip_prefix("Running ")?;
    Some(match rest.rsplit_once(" (") {
        Some((what, _)) => what.to_string(),
        None => rest.to_string(),
    })
}

/// **One failing target, read out of its own segment of the output** — the first test it named
/// and the first panic it printed. `clause` is cargo's name for it, empty when nothing named it.
fn failing_target(seg: &[&str], clause: &str) -> Option<FailingTarget> {
    let mut names: Vec<String> = Vec::new();
    let mut panic: Vec<String> = Vec::new();
    let mut i = 0;
    while i < seg.len() {
        let s = seg[i].trim();
        if let Some(name) = test_failed_name(s).or_else(|| block_name(s)) {
            if !names.contains(&name) {
                names.push(name);
            }
        } else if panic.is_empty() && s.contains("panicked at") {
            panic.push(clip(s, QUOTED_WIDTH));
            // The panic's own message follows it: the assertion, the io error, the sentence.
            let mut j = i + 1;
            while j < seg.len() && panic.len() <= PANIC_LINES {
                let said = seg[j].trim();
                if said.is_empty() {
                    break;
                }
                panic.push(clip(said, QUOTED_WIDTH));
                j += 1;
            }
        }
        i += 1;
    }
    if names.is_empty() && panic.is_empty() {
        return None;
    }
    let target = if clause.is_empty() {
        seg.iter()
            .rev()
            .find_map(|l| running_descriptor(l.trim()))
            .unwrap_or_else(|| "the step's own output".to_string())
    } else {
        clause.to_string()
    };
    Some(FailingTarget {
        target,
        test: names.first().cloned(),
        panic,
        failed: names.len(),
    })
}

/// **Read a step's whole output once**, for what it says about the code.
fn failures(text: &str) -> Failures {
    let lines: Vec<&str> = text.lines().collect();
    let mut f = Failures::default();
    let mut from = 0usize;
    for (i, raw) in lines.iter().enumerate() {
        let s = raw.trim();
        if let Some(clause) = rerun_clause(s) {
            if let Some(t) = failing_target(&lines[from..i], &clause) {
                f.targets.push(t);
            }
            from = i + 1;
            continue;
        }
        if targets_failed_head(s) {
            f.summary.push(s.to_string());
            // The names under it, indented — `` `-p letibot-harnessd --lib` ``.
            let mut j = i + 1;
            while j < lines.len()
                && !lines[j].trim().is_empty()
                && lines[j].starts_with(char::is_whitespace)
            {
                f.summary.push(lines[j].trim().to_string());
                j += 1;
            }
            continue;
        }
        if s.starts_with("test result: ") {
            if s.starts_with("test result: ok.") {
                f.targets_passed += 1;
            } else if s.starts_with("test result: FAILED.") {
                f.targets_failed += 1;
            }
            f.tests_passed += number_before(s, " passed;");
            f.tests_failed += number_before(s, " failed;");
        } else if running_count(s).is_some() {
            f.targets_ran += 1;
        }
    }
    // A runner that is not cargo (`bin/firecode test`, a script) has no `to rerun pass` boundary:
    // the whole output is the one segment, and the first failure in it is the one to lead with.
    if f.targets.is_empty() && (f.targets_failed > 0 || super::setup::shows_code_failing(text)) {
        if let Some(t) = failing_target(&lines, "") {
            f.targets.push(t);
        }
    }
    for line in text.lines() {
        let l = line.trim();
        if f.exhausted.len() < 3
            && !f.exhausted.iter().any(|e| e == l)
            && EXHAUSTED.iter().any(|w| l.contains(w))
        {
            f.exhausted.push(clip(l, QUOTED_WIDTH));
        }
    }
    f
}

impl Failures {
    /// Whether this output says anything worth leading with — a named failure, or a machine that
    /// refused the step something wherever it said so.
    fn says_something(&self) -> bool {
        !self.targets.is_empty() || !self.summary.is_empty() || !self.exhausted.is_empty()
    }

    /// **The one line the excerpt ENDS with**, naming what failed — because a reader that keeps
    /// only the end of a row is a real reader: `notices::failing_step` hands the model the last
    /// [`super::notices::TAIL_LINES`] lines of exactly this string, and a head's card does the
    /// same. Without it the failure would be at the top of the row and nowhere else.
    fn pointer(&self) -> String {
        if let Some(t) = self.targets.first() {
            return match &t.test {
                Some(name) => format!(
                    "[gate excerpt: what failed is at the TOP of this row: `{name}` in `{}`]",
                    t.target
                ),
                None => format!(
                    "[gate excerpt: what failed is at the TOP of this row: `{}`]",
                    t.target
                ),
            };
        }
        if let Some(first) = self.summary.first() {
            return format!("[gate excerpt: what failed is at the TOP of this row: {first}]");
        }
        "[gate excerpt: the machine refused the step something — see the TOP of this row]".into()
    }

    /// **The failure section**, at most `budget` bytes: the counts, cargo's list of failing
    /// targets, then each target's first failing test and its panic — and, from anywhere in the
    /// stream, the lines that say a resource ran out.
    fn words(&self, budget: usize) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "[gate excerpt: {} target(s) ran — {} passed, {} failed; {} test(s) passed, {} failed. \
             What follows is taken from the whole of the step's output, not only its end]\n",
            self.targets_ran,
            self.targets_passed,
            self.targets_failed,
            self.tests_passed,
            self.tests_failed
        ));
        if !self.summary.is_empty() {
            out.push_str(&self.summary.join("\n"));
            out.push('\n');
        }
        for t in self.targets.iter().take(FAILING_TARGETS) {
            let mut head = format!("[gate excerpt: failing target `{}`", t.target);
            match &t.test {
                Some(name) => head.push_str(&format!(" — first failing test `{name}`")),
                None => head.push_str(" — it named no failing test"),
            }
            if t.failed > 1 {
                head.push_str(&format!(" ({} tests failed in it)", t.failed));
            }
            head.push_str("]\n");
            out.push_str(&head);
            for line in &t.panic {
                out.push_str(&format!("    {line}\n"));
            }
        }
        if self.targets.len() > FAILING_TARGETS {
            out.push_str(&format!(
                "[gate excerpt: and {} more failing target(s), named above]\n",
                self.targets.len() - FAILING_TARGETS
            ));
        }
        for line in &self.exhausted {
            out.push_str(&format!(
                "[gate excerpt: the step's whole output also says: {line}]\n"
            ));
        }
        let budget = budget.min(FAILURE_BYTES);
        if out.len() > budget {
            out = cut_to(&out, budget, SECTION_CUT);
        }
        out
    }
}

/// **What a failed step leaves on its row** — the failure first, then as much of the end as the
/// cap allows.
///
/// MEASURED 2026-10-29, twice: `LETIBOT_REQUIRE_APPARATUS=1 cargo test --workspace --no-fail-fast`
/// went red on `main` at `8e30d902` and on another branch, and both rows said only
/// `error: 1 target failed: -p letibot-harnessd --lib`. The failing test's name and panic were
/// hundreds of lines above the last 4 KiB, behind the workspace's other targets printing
/// `running 0 tests` — so the gate could not say which test, and the same command by hand passed
/// both times. The failure is now extracted from the WHOLE stream ([`failures`]), the tail is what
/// gets cut, and when even the tail has to go the section says so.
///
/// **The cap is a cap, and it is spent in this order**: the failure section and the footer first
/// (they are what the excerpt is FOR), then the end of the step's output, and the section only
/// when the cap cannot hold even it — so the tail of the output is the first thing clipped and the
/// section is never clipped to make room for the tail. The line that says what was left out is
/// inside the cap too, and its byte count is the one it really dropped.
pub fn excerpt(text: &str, cap: usize) -> String {
    if text.len() <= cap {
        return text.to_string();
    }
    let f = failures(text);
    if !f.says_something() {
        return tail(text, cap);
    }
    let words = f.words(cap);
    let pointer = f.pointer();
    let room = cap.saturating_sub(words.len() + pointer.len() + 2);
    let out = if room < MIN_TAIL {
        // No room for the end. The section and the footer stay, and if the cap cannot hold both
        // the section is what gives way — it is still the last thing to be cut.
        let left_out = format!(
            "[gate excerpt: the end of the step's output is left out — it is {} byte(s) in all — \
             because what failed is above]",
            text.len()
        );
        let budget = cap.saturating_sub(left_out.len() + pointer.len() + 2);
        format!(
            "{}\n{left_out}\n{pointer}",
            cut_to(&words, budget, SECTION_CUT)
        )
    } else {
        format!(
            "{words}\n{}\n{pointer}",
            end_of(
                text,
                room,
                "of the step's output were left out here; its end follows"
            )
        )
    };
    // **Enforced, not aimed at.** Every budget above is exact, and this is what makes the cap a
    // fact about the answer rather than about the arithmetic that produced it.
    if out.len() <= cap {
        out
    } else {
        cut_to(&out, cap, "")
    }
}

/// `<worktree>/target`, absolute.
pub fn gate_target_dir(worktree: &Path) -> PathBuf {
    std::path::absolute(worktree)
        .unwrap_or_else(|_| worktree.to_path_buf())
        .join("target")
}

/// **This run's scratch directory, absolute** — [`GATE_TMP_ROOT`]`/<the tree's name, at most
/// [`GATE_TMP_NAME_CHARS`]>-<this process's run number>`.
///
/// **The name is per RUN, not per tree.** The tree's name is what a leftover is matched by, and the
/// number keeps two runs from sharing one directory — a run that cleared another's would take that
/// run's bytes with it. A daemon's gates are sequential, but two test binaries run at once.
///
/// **The entry id would name a run better and is not reachable here**: it lives in the queue, and
/// [`GateRunner::run`] — the seam R62 fills — is handed a worktree and steps and no entry.
pub fn gate_tmp_dir(worktree: &Path) -> PathBuf {
    static RUN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let tree: String = worktree
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
        .chars()
        .take(GATE_TMP_NAME_CHARS)
        .collect();
    let run = RUN.fetch_add(1, Ordering::Relaxed) + 1;
    Path::new(GATE_TMP_ROOT).join(format!("{tree}-{run}"))
}

/// **A run's scratch directory, made when the run starts and removed when it ends** — on every way
/// out of [`run_with_memory`], the early returns included, which is why it is a `Drop`.
struct GateScratch(PathBuf);

impl GateScratch {
    /// Make this run's scratch. Anything already at the name is cleared first: the name is per run,
    /// so what is there is a run that was killed before it could remove its own.
    fn for_run(worktree: &Path) -> Result<GateScratch, String> {
        use std::os::unix::fs::DirBuilderExt;
        let dir = gate_tmp_dir(worktree);
        let _ = std::fs::remove_dir_all(&dir);
        // **`0700`**: a step's scratch holds what the step wrote — on this tree that includes a
        // copy of the operator's store (`crates/turn/tests/restore.rs`) — and `/var/tmp` is
        // world-writable. The mode is set on [`GATE_TMP_ROOT`] as well, when this makes it.
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?;
        Ok(GateScratch(dir))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for GateScratch {
    /// **Best effort, and said when it is not enough** — the voice the rest of the queue uses for
    /// what it could not do. A scratch left behind is disk on the box, and the next run of that
    /// tree clears its own name before it starts.
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.0) {
            if self.0.exists() {
                eprintln!(
                    "  merge queue: the gate's scratch {} could not be removed: {e}",
                    self.0.display()
                );
            }
        }
    }
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The gate's steps and the install steps split off them ([`INSTALL_MARK`]).
pub type GateAndInstall = (Vec<String>, Vec<String>);

/// **The steps, as the TARGET declares them at `at`** — read from that commit's `AGENTS.md`
/// (any case), so a branch cannot change its own gate. `Ok(None)` when the target declares no
/// gate. The second list is the install steps ([`INSTALL_MARK`]), split off to run after the
/// push.
pub fn declared_gate(repo: &Path, at: &str) -> Result<Option<GateAndInstall>, String> {
    let names = git::git(repo, &["ls-tree", "--name-only", at])?;
    let Some(file) = names
        .lines()
        .find(|n| n.eq_ignore_ascii_case("agents.md"))
        .map(str::to_string)
    else {
        return Ok(None);
    };
    let text = git::git(repo, &["show", &format!("{at}:{file}")])?;
    Ok(
        letibot_tools::gatekeeper::parse_merge_gate(&text).map(|steps| {
            steps
                .into_iter()
                .partition(|s: &String| !s.contains(INSTALL_MARK))
        }),
    )
}

/// The one row a target with no gate records — not an empty list, which reads *nothing ran*.
pub fn no_gate_row(why: &str) -> MergeGateStep {
    MergeGateStep {
        command: String::new(),
        outcome: MergeGateOutcome::NoGate,
        output: why.to_string(),
        started_ms: now_ms(),
        elapsed_ms: 0,
    }
}

/// **The green gate** — every test's runner that is not about the gate itself.
#[derive(Debug, Default, Clone)]
pub struct GreenGate;

impl GateRunner for GreenGate {
    fn run(&self, _worktree: &Path, steps: &[String], _ctl: &GateControl) -> GateRun {
        GateRun {
            steps: steps
                .iter()
                .map(|s| MergeGateStep {
                    command: s.clone(),
                    outcome: MergeGateOutcome::Passed,
                    output: String::new(),
                    started_ms: now_ms(),
                    elapsed_ms: 0,
                })
                .collect(),
            verdict: GateVerdict::Passed,
        }
    }
    fn name(&self) -> String {
        "green (test)".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The step's own output** — a row's output is the step's output and then the memory evidence
    /// line (`with_memory`, R76 item 6), so the step's answer is the first line of it.
    fn step_said(run: &GateRun) -> String {
        run.steps[0]
            .output
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .to_string()
    }

    /// **The tail of a running step GROWS between two reads** — the measurement behind *is it
    /// stuck?*: a step that is writing is not stuck, and a pane can only say so if the daemon reads
    /// the file the step is writing while it writes it.
    ///
    /// A real `sh` step, a real log, the real registry: `echo one`, a pause, `echo two`, a pause,
    /// `echo three`. The first read is taken while it is between the first two, and the second
    /// after it has written the third.
    #[test]
    fn a_running_steps_tail_grows_between_two_reads() {
        let dir = std::env::temp_dir().join(format!("letibot-gate-tail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("the fixture's directory");
        let wt = dir.join("landing");
        std::fs::create_dir_all(&wt).expect("the landing tree");
        let repo = dir.display().to_string();
        watch(&wt, &repo);
        // **A trailing sleep, so the last line is observable** — `three` is written and the step
        // exits, and a poll that has to land between those two instants is a poll that misses under
        // a loaded suite (measured: it passed alone and failed in the workspace run).
        let steps =
            vec!["echo one; sleep 0.6; echo two; sleep 0.6; echo three; sleep 1".to_string()];
        std::thread::scope(|s| {
            let run = s.spawn(|| {
                let stop = AtomicBool::new(false);
                let alive = || true;
                let ctl = GateControl {
                    deadline: Instant::now() + Duration::from_secs(60),
                    budget: Duration::from_secs(60),
                    stop: &stop,
                    alive: &alive,
                };
                run_with_memory(&wt, &steps, &ctl, Ask::Default)
            });
            let wait_for = |word: &str| -> RunningGate {
                let until = Instant::now() + Duration::from_secs(20);
                loop {
                    if let Some(g) = running(&repo)
                        && g.tail.contains(word)
                    {
                        return g;
                    }
                    assert!(Instant::now() < until, "the step never wrote {word:?}");
                    std::thread::sleep(Duration::from_millis(20));
                }
            };
            let first = wait_for("one");
            assert_eq!(
                (first.step, first.of),
                (1, 1),
                "one step, and it is the first"
            );
            assert_eq!(
                first.command,
                "echo one; sleep 0.6; echo two; sleep 0.6; echo three; sleep 1"
            );
            let second = wait_for("three");
            assert!(
                second.log_bytes > first.log_bytes,
                "the tail grew: {} then {} byte(s)",
                first.log_bytes,
                second.log_bytes
            );
            assert!(
                second.tail.contains("two") && !first.tail.contains("three"),
                "each read is the file as it was: {:?} then {:?}",
                first.tail,
                second.tail
            );
            assert_eq!(
                run.join().expect("the gate's thread").verdict,
                GateVerdict::Passed
            );
        });
        assert!(
            running(&repo).is_none(),
            "a step that has ended leaves nothing to read"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **The tail is bounded, and the bound is the READ** — a step that has written far more than
    /// [`TAIL_BYTES`] costs one 4 KiB read and keeps its last [`TAIL_LINES`] lines, saying how much
    /// it dropped. A pane that read the whole file would be a `cargo test` log in a head's memory.
    #[test]
    fn the_tail_of_a_long_log_is_its_end_and_only_its_end() {
        let dir = std::env::temp_dir().join(format!("letibot-gate-tailcap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("the directory");
        let log = dir.join("step.log");
        let mut text = String::new();
        for i in 0..5_000 {
            text.push_str(&format!("line {i} of a build\n"));
        }
        std::fs::write(&log, &text).expect("the log");
        let (tail, bytes) = tail_of(&log);
        assert_eq!(bytes, text.len() as u64, "the size is the file's own");
        let lines: Vec<&str> = tail.lines().collect();
        assert_eq!(
            lines.len(),
            TAIL_LINES + 1,
            "the marker, then the last {TAIL_LINES} lines"
        );
        assert!(
            lines[0].starts_with("… [the step has written"),
            "{:?}",
            lines[0]
        );
        assert_eq!(lines[TAIL_LINES], "line 4999 of a build");
        assert!(
            !tail.contains("line 0 of a build"),
            "the beginning is not read at all"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **Every step's row ends with what its memory did** (R76 item 6). A test binary is not
    /// started in a delegated scope of its own, so this takes the old-style path: the row says
    /// *uncapped* and why rather than saying nothing (`tests/gate_memory.rs` covers the other half).
    #[test]
    fn a_steps_row_says_what_its_memory_did_and_uncapped_when_there_is_no_tree() {
        let wt = std::env::temp_dir().join(format!("letibot-gate-unit-{}", std::process::id()));
        std::fs::create_dir_all(&wt).unwrap();
        let stop = AtomicBool::new(false);
        let alive = || true;
        let ctl = GateControl {
            deadline: Instant::now() + Duration::from_secs(60),
            budget: Duration::from_secs(60),
            stop: &stop,
            alive: &alive,
        };
        let run = run_with_memory(&wt, &["echo hi".to_string()], &ctl, Ask::Default);
        assert_eq!(run.verdict, GateVerdict::Passed, "{run:#?}");
        let out = &run.steps[0].output;
        assert!(
            out.starts_with("hi\n"),
            "the step's own output first: {out}"
        );
        match letibot_tools::exec::delegate_memory() {
            Err(why) => assert!(
                out.contains("[gate memory: uncapped — the step ran in the daemon's own cgroup")
                    && out.contains(why.as_str()),
                "{out}"
            ),
            Ok(_) => assert!(out.contains("[gate memory: peak"), "{out}"),
        }
        let _ = std::fs::remove_dir_all(&wt);
    }

    /// A step killed at the cap names the cap, the peak and the line that raises it; one that
    /// left a process running says it was killed with the cgroup.
    #[test]
    fn the_evidence_line_tells_a_kill_at_the_cap_from_a_peak() {
        let used = |oom_kills| {
            StepMemory::Used(MemoryUse {
                cap: Some(64 << 20),
                peak: 64 << 20,
                oom_kills,
            })
        };
        let killed = used(1).evidence(0, None).unwrap();
        assert!(killed.contains("KILLED at the gate's cap — peak 64 MiB of a 64 MiB cap"));
        assert!(killed.contains("[memory] gate"));
        assert!(used(1).killed_at_cap());
        let fine = used(0).evidence(2, None).unwrap();
        assert!(fine.starts_with("[gate memory: peak 64 MiB of a 64 MiB cap — near its cap]"));
        assert!(fine.contains("2 process(es) the step left running"));
        // R93.F8: what the model reads keeps a near-cap or killed line and drops a quiet peak.
        assert_eq!(for_the_model(&fine), fine);
        let quiet = StepMemory::Used(MemoryUse {
            cap: Some(64 << 20),
            peak: 8 << 20,
            oom_kills: 0,
        })
        .evidence(0, None)
        .unwrap();
        assert!(quiet.starts_with("[gate memory: peak 8 MiB"), "{quiet}");
        assert_eq!(for_the_model(&format!("ok\n{quiet}")), "ok");
        assert_eq!(StepMemory::Unknown.evidence(0, None), None);
    }

    /// The measured case: letibot's `main` at 79c32c1, 2026-10-12, on lab2x1.
    const GIB: u64 = 1 << 30;
    fn measured(oom_kills: u64) -> StepMemory {
        StepMemory::Used(MemoryUse {
            cap: Some((24.9 * GIB as f64) as u64),
            peak: (24.1 * GIB as f64) as u64,
            oom_kills,
        })
    }
    /// The end of that step's output: compile warnings, no test result, no `error:`.
    const ENDED_IN_WARNINGS: &str = "warning: function `answer_frames` is never used\n\
        warning: `letibot-tui` (lib test) generated 17 warnings (4 duplicates)\n";

    /// **A cargo that died at 96.8% of its cap with its compiles still running is the gate's
    /// limit, not red code** — whether its leader ended by `SIGKILL` or by a code, and although
    /// the kernel counted no OOM kill.
    #[test]
    fn the_measured_death_near_the_cap_is_killed_at_the_cap_not_red() {
        for exit in [StepExit::Signal(libc::SIGKILL), StepExit::Code(101)] {
            let cause = red_cause(ENDED_IN_WARNINGS, exit, &measured(0));
            let RedCause::KilledAtCap(how) = &cause else {
                panic!("{exit:?}: {cause:?}");
            };
            assert!(
                how.contains("near its memory cap")
                    && how.contains("out of memory, not a verdict")
                    && !how.contains('%')
                    && !how.contains("  "),
                "{how}"
            );
        }
        let words = killed_at_cap_words(
            "LETIBOT_REQUIRE_APPARATUS=1 cargo test --workspace --no-fail-fast -- --nocapture",
            &match measured(0) {
                StepMemory::Used(u) => u.clause(),
                _ => unreachable!(),
            },
            "it died near the cap",
            128,
        );
        assert!(words.starts_with(GATE_KILLED), "{words}");
        assert!(
            words.contains("KILLED at the gate's memory cap (peak 24.1 GiB of a 24.9 GiB cap)")
                && words.contains("128 process(es) were still running")
                && words.contains("UNJUDGED, not red")
                && words.contains("`[memory] gate` in providers.toml"),
            "{words}"
        );
        let line = measured(0)
            .evidence(128, Some(StepExit::Signal(libc::SIGKILL)))
            .unwrap();
        assert_eq!(
            line,
            "[gate memory: peak 24.1 GiB of a 24.9 GiB cap — near its cap]\n[step: killed by \
             signal 9 (SIGKILL)]\n[128 process(es) the step left running were killed with its cgroup]"
        );
    }

    /// The kernel's own count is the proof, and outranks the output: a rustc it killed makes
    /// cargo print `could not compile`, which is not the code failing.
    #[test]
    fn an_oom_kill_counted_in_the_cgroup_is_killed_at_the_cap_whatever_the_output_says() {
        let out = "error: could not compile `letibot-tui` (lib test)\n\nCaused by:\n  process \
                   didn't exit successfully: `rustc …` (signal: 9, SIGKILL: kill)\n";
        assert!(matches!(
            red_cause(out, StepExit::Code(101), &measured(3)),
            RedCause::KilledAtCap(how) if how.contains("killed 3 process(es)")
        ));
        // Near the cap with no count, the same output is a kill, not a verdict.
        assert!(matches!(
            red_cause(out, StepExit::Code(101), &measured(0)),
            RedCause::KilledAtCap(_)
        ));
    }

    /// **Code that failed is red, near the cap or not**; a death far from the cap with no
    /// failing code is red only when the leader answered with a code — a signal is somebody
    /// else's hand, and judged nothing.
    #[test]
    fn failing_code_is_red_and_a_signal_far_from_the_cap_is_unjudged() {
        let failed = "test result: FAILED. 1 passed; 1 failed\nerror: 1 target failed\n";
        assert_eq!(
            red_cause(failed, StepExit::Code(101), &measured(0)),
            RedCause::Code
        );
        let far = StepMemory::Used(MemoryUse {
            cap: Some(24 * GIB),
            peak: 6 * GIB,
            oom_kills: 0,
        });
        assert_eq!(
            red_cause("merge gate: FAIL\n", StepExit::Code(1), &far),
            RedCause::Code
        );
        assert_eq!(
            red_cause(ENDED_IN_WARNINGS, StepExit::Signal(libc::SIGKILL), &far),
            RedCause::KilledBySignal(libc::SIGKILL)
        );
        assert_eq!(
            red_cause(failed, StepExit::Signal(libc::SIGTERM), &far),
            RedCause::Code,
            "a failure it printed before the signal is still a failure"
        );
        // Uncapped (no delegated scope): only the signal can tell.
        let uncapped = StepMemory::Uncapped("old-style start".into());
        assert_eq!(
            red_cause(ENDED_IN_WARNINGS, StepExit::Code(101), &uncapped),
            RedCause::Code
        );
        assert_eq!(
            red_cause(ENDED_IN_WARNINGS, StepExit::Signal(9), &uncapped),
            RedCause::KilledBySignal(9)
        );
    }

    /// **A step that refused to skip is the MACHINE's, not the branch's** — the measured case,
    /// verbatim, from `lab2x1` on 2026-10-30: `LETIBOT_REQUIRE_APPARATUS=1 cargo test --no-fail-fast
    /// -p letibot-harnessd --lib`, sixteen refusals and one genuine failure.
    ///
    /// Before this, `shows_code_failing` read the refusal's `panicked at` as the code failing and
    /// the whole workspace went RED on both queues for one sentence about a missing cgroup.
    #[test]
    fn a_step_that_refused_to_skip_is_the_machines_and_not_the_branches() {
        let refusal = |test: &str, what: &str| {
            format!(
                "---- {test} stdout ----\n\nthread '{test}' (805) panicked at \
                 crates/tokencore/src/apparatus.rs:181:9:\n\
                 LETIBOT_REQUIRE_APPARATUS is set, and {what} is not on this machine.\n\n\
                 This run does not accept a skip. Either run it where the apparatus is, or unset \
                 LETIBOT_REQUIRE_APPARATUS to let it skip with a reason.\n\n"
            )
        };
        let what = "a process-lifetime tree (cgroup v2; process groups on macOS)";
        // A host gate's step is `Elsewhere`: no cgroup of this daemon's, so no cap to read — and
        // that is the memory a host gate hands `red_cause`.
        let elsewhere = || StepMemory::Elsewhere("lab2x1".into());
        let out = format!(
            "{}{}failures:\n    jobwatch::tests::a\n    supervise::tests::b\n\n\
             test result: FAILED. 782 passed; 17 failed; 1 ignored; 0 measured; 0 filtered \
             out; finished in 26.28s\n\n\
             error: test failed, to rerun pass `-p letibot-harnessd --lib`\n\
             error: 1 target failed:\n    `-p letibot-harnessd --lib`\n",
            refusal("jobwatch::tests::a", what),
            refusal("supervise::tests::b", what),
        );
        assert_eq!(
            red_cause(&out, StepExit::Code(101), &elsewhere()),
            RedCause::ApparatusAbsent(what.to_string())
        );
        // The verdict is the gate's own: unjudged, and the row is `not_run`.
        let judged = judge(
            "LETIBOT_REQUIRE_APPARATUS=1 cargo test --workspace",
            StepEnd::Red(out.clone(), StepExit::Code(101)),
            StepMemory::Elsewhere("lab2x1".into()),
            0,
            Duration::from_secs(1800),
        );
        assert_eq!(judged.outcome, MergeGateOutcome::NotRun);
        let Some(GateVerdict::Infra(why)) = judged.verdict else {
            panic!("a machine's absence must not be a verdict about the code");
        };
        assert!(
            why.starts_with(
                "the gate could not run here: a process-lifetime tree (cgroup v2; process groups \
                 on macOS) is not on `lab2x1`"
            ),
            "{why}"
        );
        assert!(why.contains("UNJUDGED, not red"), "{why}");
        assert!(!why.starts_with(GATE_KILLED), "nothing was killed: {why}");

        // **One genuine failure among the refusals is still red** — the count is what tells them
        // apart, and a suite that also failed on its own must not be excused by the machine.
        let mixed = format!(
            "{}thread 'a_test' (99) panicked at src/x.rs:1:1:\nassertion failed: 1 == 2\n\n",
            out
        );
        assert_eq!(
            red_cause(&mixed, StepExit::Code(101), &elsewhere()),
            RedCause::Code
        );
        // And so is a compile error, whatever the refusals said.
        let broken = format!("{out}\nerror[E0308]: mismatched types\n");
        assert_eq!(
            red_cause(&broken, StepExit::Code(101), &elsewhere()),
            RedCause::Code
        );
        // A run that only MENTIONS the words (a test of the message itself) is not a refusal.
        assert_eq!(
            red_cause(
                "test result: ok. 1 passed\nassert!(!m.contains(\"is not on this machine\"));\n",
                StepExit::Code(101),
                &elsewhere()
            ),
            RedCause::Code
        );
    }

    /// **Two apparatuses named by one step read as a list** — a workspace run refuses on more than
    /// one, and the row says both rather than the first.
    #[test]
    fn the_row_names_every_apparatus_the_step_demanded() {
        let out = "thread 'a' (1) panicked at crates/tokencore/src/apparatus.rs:181:9:\n\
                   LETIBOT_REQUIRE_APPARATUS is set, and a cgroup v2 subtree this user may write \
                   (the process-lifetime tree) is not on this machine.\n\n\
                   This run does not accept a skip. Either run it where the apparatus is, or \
                   unset LETIBOT_REQUIRE_APPARATUS to let it skip with a reason.\n\n\
                   thread 'b' (2) panicked at crates/tokencore/src/apparatus.rs:181:9:\n\
                   LETIBOT_REQUIRE_APPARATUS is set, and a vocabulary GGUF (/nope.gguf) is not on \
                   this machine.\n\n\
                   This run does not accept a skip.\n";
        let RedCause::ApparatusAbsent(what) = red_cause(
            out,
            StepExit::Code(101),
            &StepMemory::Elsewhere("lab2x1".into()),
        ) else {
            panic!("two refusals are still the machine's");
        };
        assert_eq!(
            what,
            "a cgroup v2 subtree this user may write (the process-lifetime tree)\n\
             a vocabulary GGUF (/nope.gguf)"
        );
        let why = apparatus_absent_words("cargo test --workspace", &what, Some("lab2x1"));
        assert!(
            why.starts_with("the gate could not run here: these are not on `lab2x1`: a cgroup v2"),
            "{why}"
        );
    }

    /// **End to end on a real step**: a leader killed by `SIGKILL` before it printed a failure is
    /// `killed`, the verdict is the gate's own (`Infra`, unjudged), and the row records the
    /// signal — the fact the 79c32c1 row did not keep.
    #[test]
    fn a_step_killed_by_a_signal_is_unjudged_and_its_row_says_the_signal() {
        let wt = std::env::temp_dir().join(format!("letibot-gate-sig-{}", std::process::id()));
        std::fs::create_dir_all(&wt).unwrap();
        let stop = AtomicBool::new(false);
        let alive = || true;
        let ctl = GateControl {
            deadline: Instant::now() + Duration::from_secs(60),
            budget: Duration::from_secs(60),
            stop: &stop,
            alive: &alive,
        };
        let steps = vec![
            "echo compiling; kill -9 $$".to_string(),
            "echo next".to_string(),
        ];
        let run = run_with_memory(&wt, &steps, &ctl, Ask::Uncapped);
        let GateVerdict::Infra(why) = &run.verdict else {
            panic!("{run:#?}");
        };
        assert!(why.starts_with(GATE_KILLED), "{why}");
        assert_eq!(run.steps[0].outcome, MergeGateOutcome::Killed, "{run:#?}");
        assert!(
            run.steps[0]
                .output
                .contains("[step: killed by signal 9 (SIGKILL)]"),
            "{}",
            run.steps[0].output
        );
        assert_eq!(run.steps[1].outcome, MergeGateOutcome::NotRun);
        // And a step that fails by its own code is still red.
        let run = run_with_memory(&wt, &["echo nope; exit 3".to_string()], &ctl, Ask::Uncapped);
        assert!(
            matches!(&run.verdict, GateVerdict::Failed(w) if w.contains("[step: exit code 3]")),
            "{run:#?}"
        );
        let _ = std::fs::remove_dir_all(&wt);
    }

    /// **A step's scratch is a short directory on the box's real disk, outside the tree, and it is
    /// gone when the run ends** ([`GATE_TMPDIR`], [`GATE_TMP_ROOT`]) — a `TMPDIR` inside the landing
    /// tree is too long for the AF_UNIX sockets this workspace's own tests bind.
    #[test]
    fn a_steps_scratch_is_short_on_a_real_disk_and_gone_when_the_run_ends() {
        // **The decision, as a decision** — no process needed: the box's own root, the tree's name
        // in the directory, and a number that makes two runs two directories.
        let a = gate_tmp_dir(Path::new("/landing/letibot-a156c9c34c8e"));
        let b = gate_tmp_dir(Path::new("/landing/letibot-a156c9c34c8e"));
        assert_eq!(
            a.parent(),
            Some(Path::new(GATE_TMP_ROOT)),
            "{}",
            a.display()
        );
        assert_ne!(a, b, "one directory per run, not per step");
        assert!(
            a.to_string_lossy().contains("letibot-a156c9c3"),
            "a leftover names the tree it belongs to: {}",
            a.display()
        );
        // **Short**: the longest socket name this workspace binds is 60 bytes
        // (`crates/sessionlog/tests/resume_frames.rs`), and an AF_UNIX path must be under 108 with
        // its NUL — so a root may spend 46 and no more.
        assert!(
            a.as_os_str().len() + 1 + 60 <= 107,
            "{} bytes leaves no room for the 60-byte socket names this workspace binds: {}",
            a.as_os_str().len(),
            a.display()
        );
        assert!(
            !a.starts_with(std::env::temp_dir()) && !a.starts_with("/tmp"),
            "not the box's temp directory, which is the tmpfs this exists to avoid: {}",
            a.display()
        );

        let wt = std::env::temp_dir().join(format!("letibot-gate-scratch-{}", std::process::id()));
        std::fs::create_dir_all(&wt).unwrap();
        let stop = AtomicBool::new(false);
        let alive = || true;
        let ctl = GateControl {
            deadline: Instant::now() + Duration::from_secs(60),
            budget: Duration::from_secs(60),
            stop: &stop,
            alive: &alive,
        };
        let run = run_with_memory(
            &wt,
            &[
                r#"printf '%s %s' "$TMPDIR" "$( [ -d "$TMPDIR" ] && echo dir || echo MISSING )""#
                    .to_string(),
            ],
            &ctl,
            Ask::Default,
        );
        assert_eq!(run.verdict, GateVerdict::Passed, "{run:#?}");
        // **What the step was GIVEN**, not what this test predicts: the run's number is the gate's
        // (`gate_tmp_dir` counts), so the path the step printed is the path to judge.
        let said = step_said(&run);
        let (tmp, is_dir) = said.split_once(' ').unwrap_or((said.as_str(), ""));
        let printed = Path::new(tmp);
        assert_eq!(printed.parent(), Some(Path::new(GATE_TMP_ROOT)), "{said}");
        assert_eq!(
            is_dir, "dir",
            "the gate makes it before the step runs: {said}"
        );
        assert!(
            !printed.starts_with(&wt),
            "NOT inside the landing tree: {said}"
        );
        assert!(
            !printed.starts_with(std::env::temp_dir()) && !printed.starts_with("/tmp"),
            "the step used to inherit the box's temp directory: {said}"
        );
        assert!(
            printed.as_os_str().len() + 1 + 60 <= 107,
            "the run's own directory is short too: {said}"
        );
        assert!(
            !printed.exists(),
            "the gate removes it when the run ends: {}",
            printed.display()
        );
        // **A real filesystem, not a tmpfs** — the whole point, and Linux has the answer. Elsewhere
        // the assertions above (the box's temp directory, and not the tree) are what this can say.
        #[cfg(target_os = "linux")]
        {
            let mut buf: libc::statfs = unsafe { std::mem::zeroed() };
            let path = std::ffi::CString::new(GATE_TMP_ROOT).unwrap();
            // SAFETY: a NUL-terminated path this test owns and a buffer it owns, and `statfs`
            // writes only into that buffer.
            let rc = unsafe { libc::statfs(path.as_ptr(), &mut buf) };
            assert_eq!(rc, 0, "statfs {GATE_TMP_ROOT}");
            assert_ne!(
                buf.f_type,
                libc::TMPFS_MAGIC,
                "{GATE_TMP_ROOT} is a tmpfs, and a write there is shmem charged to the step's own \
                 cgroup — the failure this root exists to avoid"
            );
        }
        let _ = std::fs::remove_dir_all(&wt);
    }

    /// **The build's parallelism comes from the cap, not from the core count** — one job per
    /// [`GATE_JOB_MEMORY`], at least one, never more than the cores: rustc jobs started at the core
    /// count against a cap that pays for fewer is what the kernel kills.
    #[test]
    fn the_builds_jobs_come_from_the_cap_and_never_exceed_the_cores() {
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        // Both clamps: one job per 2 GiB, never zero, never more than the cores.
        assert_eq!(build_jobs(Some(25 << 30)), Some(12.min(cores)));
        assert_eq!(build_jobs(Some(50 << 30)), Some(25.min(cores)));
        assert_eq!(build_jobs(Some(4 << 30)), Some(2.min(cores)));
        assert_eq!(
            build_jobs(Some(1 << 40)),
            Some(cores),
            "a cap no box has is the cores, not 512"
        );
        assert_eq!(build_jobs(Some(64 << 20)), Some(1), "never zero");
        assert_eq!(build_jobs(Some(1)), Some(1));
        assert_eq!(
            build_jobs(None),
            None,
            "uncapped: no number, cargo's default"
        );
    }

    /// **And the step is actually told** — the value reaches the step's environment, overriding
    /// whatever the box had: one job under a 64 MiB cap, the cores under a cap no box has, and
    /// under `uncapped` nothing at all (so a cargo step falls back to its own default; run inside
    /// the gate's own step, this test inherits the gate's value there).
    ///
    /// **The step wraps its answer in `jobs=<…>`, and that is not decoration**: a row's output is
    /// the step's own output and THEN the memory evidence line, so a step that prints nothing
    /// leaves the evidence as the row's first line — which is what [`step_said`] would answer with.
    #[test]
    fn a_step_is_told_how_many_jobs_its_cap_pays_for() {
        let wt = std::env::temp_dir().join(format!("letibot-gate-jobs-{}", std::process::id()));
        std::fs::create_dir_all(&wt).unwrap();
        let stop = AtomicBool::new(false);
        let alive = || true;
        let ctl = GateControl {
            deadline: Instant::now() + Duration::from_secs(60),
            budget: Duration::from_secs(60),
            stop: &stop,
            alive: &alive,
        };
        let said = |ask: Ask| {
            let run = run_with_memory(
                &wt,
                &["printf 'jobs=<%s>' \"$CARGO_BUILD_JOBS\"".to_string()],
                &ctl,
                ask,
            );
            assert_eq!(run.verdict, GateVerdict::Passed, "{run:#?}");
            step_said(&run)
        };
        assert_eq!(said(Ask::Amount(Amount::Bytes(64 << 20))), "jobs=<1>");
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        assert_eq!(
            said(Ask::Amount(Amount::Bytes(1 << 40))),
            format!("jobs=<{cores}>")
        );
        assert_eq!(
            said(Ask::Uncapped),
            format!(
                "jobs=<{}>",
                std::env::var(GATE_BUILD_JOBS).unwrap_or_default()
            ),
            "uncapped is no number: the gate sets nothing, so the step sees whatever its caller \
             had — an empty answer when the box had nothing"
        );
        let _ = std::fs::remove_dir_all(&wt);
    }

    /// **A failing workspace run, as `cargo test --workspace --no-fail-fast` writes one**: the
    /// failing target's block, then sixty other targets each reporting `running 0 tests`. The
    /// failure is at the top; the end of the stream is the other targets and cargo's one-line
    /// summary — which is all the row kept before this, and why a red gate could not say which
    /// test (MEASURED 2026-10-29, `main` at `8e30d902` and a branch).
    fn a_failing_workspace_run() -> String {
        let mut out = String::new();
        for i in 0..300 {
            out.push_str(&format!("test scene::tests::case_{i:03} ... ok\n"));
        }
        out.push_str(
            "     Running unittests src/lib.rs (target/debug/deps/letibot_harnessd-1234)\n\
             \n\
             running 301 tests\n\
             test worker::a_test ... FAILED\n\
             \n\
             failures:\n\
             \n\
             ---- worker::a_test stdout ----\n\
             thread 'worker::a_test' panicked at crates/harnessd/src/worker.rs:12:5:\n\
             the worker never started\n\
             \n\
             failures:\n    worker::a_test\n\
             \n\
             test result: FAILED. 300 passed; 1 failed; 1 ignored; 0 measured; 0 filtered out; \
             finished in 14.59s\n\
             \n\
             error: test failed, to rerun pass `-p letibot-harnessd --lib`\n",
        );
        for j in 0..60 {
            out.push_str(&format!(
                "     Running unittests src/bin/tool_{j}.rs (target/debug/deps/tool_{j}-abcdef)\n\
                 \n\
                 running 0 tests\n\
                 \n\
                 test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; \
                 finished in 0.00s\n\
                 \n"
            ));
        }
        out.push_str("error: 1 target failed:\n    `-p letibot-harnessd --lib`\n");
        out
    }

    /// **The failing test's name and its panic survive, though they are hundreds of lines above
    /// the end** — and the counts say the run happened, so `running 0 tests` blocks cannot be
    /// mistaken for a step that did nothing.
    #[test]
    fn a_failure_hundreds_of_lines_before_the_end_is_what_the_excerpt_keeps() {
        let text = a_failing_workspace_run();
        let at = text.find("worker::a_test").unwrap();
        assert!(
            text.len() - at > EVIDENCE_BYTES,
            "the failure must sit outside the last {EVIDENCE_BYTES} byte(s), which is what the \
             old excerpt kept — it is {} byte(s) from the end",
            text.len() - at
        );
        assert!(
            !tail(&text, EVIDENCE_BYTES).contains("worker::a_test"),
            "the old excerpt could not name the test, and this test is the regression"
        );

        let ex = excerpt(&text, EVIDENCE_BYTES);
        for want in [
            // The target, by cargo's own name for it.
            "`-p letibot-harnessd --lib`",
            "error: 1 target failed:",
            // The first failing test and why.
            "first failing test `worker::a_test`",
            "panicked at crates/harnessd/src/worker.rs:12:5",
            "the worker never started",
            // The counts: 1 + 60 targets, 60 of them green, and 300 + 1 tests.
            "61 target(s) ran — 60 passed, 1 failed; 300 test(s) passed, 1 failed",
        ] {
            assert!(ex.contains(want), "{want:?} is not in the excerpt:\n{ex}");
        }
        // The tail is present too — it fits beside the failure — and says what it left out.
        assert!(
            ex.contains("byte(s) of the step's output were left out here; its end follows"),
            "the cut has to be admitted:\n{ex}"
        );
        assert!(
            ex.trim_end().ends_with(
                "[gate excerpt: what failed is at the TOP of this row: `worker::a_test` in \
                 `-p letibot-harnessd --lib`]"
            ),
            "the row ends by naming what failed, for a reader that keeps only the end:\n{ex}"
        );
        // The model is handed the last `TAIL_LINES` lines of exactly this string
        // (`notices::failing_step`), so the failure has to survive THAT cut too.
        let keep = super::super::notices::TAIL_LINES;
        let lines: Vec<&str> = ex.lines().collect();
        let from = lines.len().saturating_sub(keep);
        let for_the_model = lines[from..].join("\n");
        assert!(
            for_the_model.contains("worker::a_test"),
            "the notice's own cut still loses the test:\n{for_the_model}"
        );
        assert!(
            ex.len() <= EVIDENCE_BYTES,
            "the excerpt is {} byte(s), over a {EVIDENCE_BYTES}-byte cap:\n{ex}",
            ex.len()
        );
    }

    /// **When the failure and the end cannot both fit, the failure stays and the end is left out
    /// in as many words** — never a silent truncation back to the tail.
    #[test]
    fn a_failure_too_big_for_the_cap_keeps_the_failure_and_says_the_end_is_gone() {
        let text = a_failing_workspace_run();
        let ex = excerpt(&text, 600);
        assert!(ex.contains("first failing test `worker::a_test`"), "{ex}");
        assert!(
            ex.contains("the end of the step's output is left out"),
            "{ex}"
        );
        assert!(
            ex.contains("`worker::a_test` in `-p letibot-harnessd --lib`]"),
            "even with the end gone, the row says what failed:\n{ex}"
        );
        assert!(
            !ex.contains("byte(s) of the step's output were left out here"),
            "{ex}"
        );
    }

    /// **A `fork` that ran out of descriptors is named wherever it was printed.** This is the
    /// failure the operator suspects hid behind tonight's `error: 1 target failed:` row: a daemon
    /// holding 253 of 256 descriptors makes `fork` fail, and the sentence that says so is one
    /// line of a step's output — 9 KiB above the end here.
    #[test]
    fn a_machine_that_ran_out_of_descriptors_is_named_wherever_it_was_printed() {
        // Cargo could not even start the test binary: no `failures:` block, no failing test.
        let mut text = String::from(
            "     Running unittests src/lib.rs (target/debug/deps/letibot_harnessd-1234)\n\
             error: failed to run `target/debug/deps/letibot_harnessd-1234`: Too many open files \
             (os error 24)\n",
        );
        for j in 0..80 {
            text.push_str(&format!(
                "     Running unittests src/bin/tool_{j}.rs (target/debug/deps/tool_{j}-abcdef)\n\
                 \n\
                 running 0 tests\n\
                 \n\
                 test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; \
                 finished in 0.00s\n\
                 \n"
            ));
        }
        text.push_str("error: 1 target failed:\n    `-p letibot-harnessd --lib`\n");
        assert!(
            text.len() - text.find("Too many open files").unwrap() > EVIDENCE_BYTES,
            "the sentence has to be outside the last {EVIDENCE_BYTES} byte(s)"
        );
        let ex = excerpt(&text, EVIDENCE_BYTES);
        assert!(
            ex.contains("the step's whole output also says: error: failed to run"),
            "{ex}"
        );
        assert!(ex.contains("Too many open files (os error 24)"), "{ex}");
        assert!(ex.contains("`-p letibot-harnessd --lib`"), "{ex}");

        // And the same words inside a failing test's panic are kept as the panic.
        let mut text = a_failing_workspace_run().replace(
            "the worker never started",
            "clone the socket: Os { code: 24, kind: TooManyOpenFiles, message: \"Too many open \
             files\" }",
        );
        text.push_str("(a tail)\n");
        let ex = excerpt(&text, EVIDENCE_BYTES);
        assert!(ex.contains("TooManyOpenFiles"), "{ex}");
    }

    /// **End to end on a real step**: the failure is hundreds of lines above the end and the row
    /// still names the test, the target and the counts.
    #[test]
    fn a_failed_step_keeps_the_failure_that_was_hundreds_of_lines_above_its_end() {
        let wt = std::env::temp_dir().join(format!("letibot-gate-excerpt-{}", std::process::id()));
        std::fs::create_dir_all(&wt).unwrap();
        let stop = AtomicBool::new(false);
        let alive = || true;
        let ctl = GateControl {
            deadline: Instant::now() + Duration::from_secs(120),
            budget: Duration::from_secs(120),
            stop: &stop,
            alive: &alive,
        };
        let step = r#"
i=0
while [ $i -lt 300 ]; do echo "test scene::tests::case_$i ... ok"; i=$((i+1)); done
echo "     Running unittests src/lib.rs (target/debug/deps/letibot_harnessd-1234)"
echo
echo "running 301 tests"
echo "test worker::a_test ... FAILED"
echo
echo "failures:"
echo
echo "---- worker::a_test stdout ----"
echo "thread 'worker::a_test' panicked at crates/harnessd/src/worker.rs:12:5:"
echo "the worker never started"
echo
echo "failures:"
echo "    worker::a_test"
echo
echo "test result: FAILED. 300 passed; 1 failed; 1 ignored; 0 measured; 0 filtered out; finished in 14.59s"
echo
echo 'error: test failed, to rerun pass `-p letibot-harnessd --lib`'
j=0
while [ $j -lt 60 ]; do
  echo "     Running unittests src/bin/tool_$j.rs (target/debug/deps/tool_$j-abcdef)"
  echo
  echo "running 0 tests"
  echo
  echo "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s"
  echo
  j=$((j+1))
done
echo "error: 1 target failed:"
echo '    `-p letibot-harnessd --lib`'
exit 101
"#
        .to_string();
        let run = run_with_memory(&wt, &[step], &ctl, Ask::Uncapped);
        let GateVerdict::Failed(why) = &run.verdict else {
            panic!("{run:#?}");
        };
        let out = &run.steps[0].output;
        for want in [
            "first failing test `worker::a_test`",
            "`-p letibot-harnessd --lib`",
            "the worker never started",
            "61 target(s) ran — 60 passed, 1 failed; 300 test(s) passed, 1 failed",
        ] {
            assert!(out.contains(want), "{want:?} is not on the row:\n{out}");
            assert!(why.contains(want), "{want:?} is not in the verdict:\n{why}");
        }
        assert!(
            out.len() <= EVIDENCE_BYTES,
            "the row is {} byte(s) against a {EVIDENCE_BYTES}-byte cap:\n{out}",
            out.len()
        );
        let _ = std::fs::remove_dir_all(&wt);
    }

    /// **The row's cap covers the note the gate writes after the excerpt** — the red `main` of
    /// 2026-10-30 in one fixture, and it is the test that would have caught it.
    ///
    /// `lab2x1`'s gate came out **4 397 bytes** against a 4 096-byte cap and this Mac's came out
    /// under it, on one commit and one fixture, because [`StepMemory::evidence`] appends a
    /// sentence after the excerpt and that sentence NAMES the cgroup the daemon shares: a
    /// `/sys/fs/cgroup/user.slice/…/session-N.scope` path on Linux, and a bare `No such file or
    /// directory` on a Mac. The excerpt was inside its cap both times.
    ///
    /// **Nothing here is a number that happened to work**: the tail is the cap's own length of one
    /// line — the boundary the marker's arithmetic is tightest at — and the note is the longest
    /// one this gate can write.
    #[test]
    fn a_row_whose_end_is_as_big_as_the_cap_allows_still_fits_beside_the_gate_note() {
        let why = format!(
            "this daemon shares its cgroup \
             `/sys/fs/cgroup/user.slice/user-1000.slice/session-{}.scope` with 4 other \
             process(es) — it was not started in a scope of its own (the `letibot` launcher does \
             that with `systemd-run --user --scope`), so tool calls run UNCAPPED",
            9_999_999
        );
        let mut text = a_failing_workspace_run();
        text.push_str(&"x".repeat(EVIDENCE_BYTES));
        text.push('\n');

        let log = std::env::temp_dir().join(format!("letibot-gate-cap-{}.log", std::process::id()));
        std::fs::write(&log, &text).unwrap();
        let row = judge(
            "cargo test --workspace --no-fail-fast",
            StepEnd::Red(read_excerpt(&log), StepExit::Code(101)),
            StepMemory::Uncapped(why),
            0,
            Duration::from_secs(60),
        )
        .output;
        let _ = std::fs::remove_file(&log);

        for want in [
            "first failing test `worker::a_test`",
            "panicked at crates/harnessd/src/worker.rs:12:5",
            "[gate memory: uncapped",
            "[step: exit code 101]",
        ] {
            assert!(row.contains(want), "{want:?} is not on the row:\n{row}");
        }
        assert!(
            row.len() <= EVIDENCE_BYTES,
            "the row is {} byte(s) against a {EVIDENCE_BYTES}-byte cap — the note was spent \
             outside it:\n{row}",
            row.len()
        );
    }

    /// **And the end of a stream that names no failure is inside the cap too** — [`tail`]'s marker
    /// says what it dropped, and the marker is part of what it was given, not added to it.
    #[test]
    fn the_end_of_a_stream_that_names_nothing_is_within_the_cap() {
        let text = "warning: function `f` is never used\n".repeat(EVIDENCE_BYTES / 8);
        let ex = excerpt(&text, EVIDENCE_BYTES);
        assert!(ex.starts_with("… ["), "the cut has to be admitted:\n{ex}");
        assert!(ex.contains("byte(s) dropped from the front]"), "{ex}");
        assert!(
            ex.len() <= EVIDENCE_BYTES,
            "the end is {} byte(s) against a {EVIDENCE_BYTES}-byte cap",
            ex.len()
        );
    }
}
