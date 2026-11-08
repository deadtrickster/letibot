//! The gate: the target's own `## Merge gate`, run with a deadline, in a process group, and
//! killable - each step `sh -c <step>` in a new process group writing to a file, killed on
//! the deadline, the daemon's stop, or a lost lease.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use letibot_tokencore::store::{MergeGateOutcome, MergeGateStep};
use std::sync::Arc;

use letibot_tools::exec::memory::{Amount, Ask, Meminfo};
use letibot_tools::exec::{MemoryUse, SiblingScope, join_script};
use letibot_transcript::text::bytes_head;

use super::git;

/// Thirty minutes - the operator's decision 9. A gate that is still running then is killed and
/// its row is `timed_out`, an outcome of its own.
pub const GATE_DEADLINE: Duration = Duration::from_secs(30 * 60);

/// The environment variable the gate's builds are pointed with - `<worktree>/target`, so a branch
/// is never judged with another tree's artifacts.
pub const GATE_TARGET_DIR: &str = "CARGO_TARGET_DIR";

/// The variable a step's scratch space is pointed with: a directory of the gate's own under
/// [`GATE_TMP_ROOT`], one per run, never the daemon's `TMPDIR` - a tmpfs write is shmem charged to
/// the step's own cgroup, so a step would pay for itself out of the memory it is measured against.
pub const GATE_TMPDIR: &str = "TMPDIR";

/// The box's directory the gate's per-run scratch directories live in: on the root filesystem (page
/// cache, not shmem - see [`GATE_TMPDIR`]) and short, because this workspace's tests derive AF_UNIX
/// socket paths from `std::env::temp_dir` and a `sun_path` is 108 bytes including its NUL.
pub const GATE_TMP_ROOT: &str = "/var/tmp/letibot-gate";

/// How much of the landing tree's name a run's directory carries - enough that a leftover is
/// diagnosable; the rest of the byte budget goes to the run's number, keeping the path inside the
/// 46 bytes a root may spend.
const GATE_TMP_NAME_CHARS: usize = 16;

/// The variable that bounds a step's build: how many jobs the gate's cap pays for, rather than
/// letting cargo guess from the core count. See [`build_jobs`].
pub const GATE_BUILD_JOBS: &str = "CARGO_BUILD_JOBS";

/// One rustc job per this much of the step's cap: a workspace build runs at roughly 0.7–0.8 GiB per
/// job, and 2 GiB leaves room for the linker and the test run that follow in the same step.
pub const GATE_JOB_MEMORY: u64 = 2 << 30;

/// The most a step's row may carry - the excerpt *and* the note appended after it
/// ([`StepMemory::evidence`]), because a cap two writers spend separately is not a cap. The row is
/// what failed, then the end. See [`excerpt`].
pub const EVIDENCE_BYTES: usize = 4_096;

/// The room the row keeps for the note [`StepMemory::evidence`] appends after the excerpt - that
/// sentence is not a fixed size (with no scope of its own the daemon NAMES the cgroup it shares), so
/// the room is taken before the tail is cut and [`judge`] clips the note to it.
const NOTE_BYTES: usize = 512;

/// The most of a step's output the excerpt may take - [`EVIDENCE_BYTES`] less the note and the
/// newline before it; the cap [`read_excerpt`] hands [`excerpt`].
const EXCERPT_BYTES: usize = EVIDENCE_BYTES - NOTE_BYTES - 1;

/// The marker of an INSTALL step - run after the push, never before it (decision 7): a landing that
/// installed first installed code that was not on the target when the push failed.
pub const INSTALL_MARK: &str = "install-gate-build";

/// A gate step's cap when `[memory] gate` says nothing: half of what is free when the gate starts
/// the other half staying for the daemon. A fraction, not a size - half of what is free shrinks
/// exactly when the box is busiest.
pub const GATE_MEMORY_DEFAULT: Amount = Amount::Fraction(0.5);

/// What a target's evidence (or a branch's row) leads with when its step was killed before it
/// judged the code - at the gate's cap or by a signal from outside. Read like
/// [`super::setup::GATE_SETUP_FAILED`]: the tip is UNJUDGED, not red.
pub const GATE_KILLED: &str = "the gate's step was killed before it could judge the code";

/// What the evidence leads with when the step could not judge the code because the
/// apparatus it DEMANDS is not on the machine it ran on - twenty-one targets failing with
/// one sentence is a fact about the MACHINE, and a gate that reds for that is a gate whose
pub const APPARATUS_ABSENT: &str = "the gate could not run here";

/// The two halves of the sentence `letibot_tokencore::apparatus::present_given` refuses with. Two
/// independent phrases, because the classifier must not fire on a test that merely QUOTES the words
/// (this tree's own tests do), and a rewording has to break both to go unnoticed.
const APPARATUS_SAID: (&str, &str) = (" is not on this machine", "does not accept a skip");

/// A count and its noun, spelled: `1 process`, `3 processes`. The number is in hand when the row
/// is written, and a row is read by a person and by a model, so `process(es)` is a placeholder
/// standing where a word belongs.
pub(super) fn many(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("{n} {one}")
    } else {
        format!("{n} {many}")
    }
}

/// What a step killed at the gate's cap is said to be - the cap, the peak, how it was known
/// and the line that raises it.
pub(crate) fn killed_at_cap_words(step: &str, clause: &str, how: &str, leftover: usize) -> String {
    let left = if leftover > 0 {
        format!(
            " {} were still running in it when it ended.",
            many(leftover, "process", "processes")
        )
    } else {
        String::new()
    };
    format!(
        "{GATE_KILLED}: `{step}` was KILLED at the gate's memory cap ({clause}) — {how}.{left} It \
         ran out of the memory the gate gives a step, which says nothing about the code: the tip \
         is UNJUDGED, not red. Raising the cap is the operator's call — tell them; gate the tip \
         again once they have"
    )
}

/// Where the gate's cap comes from. A lock, not a `OnceLock`: the number must be readable when the gate starts, and a later [`set_gate_memory`] must not be dropped.
enum CapSource {
    /// A value somebody decided: a fixture, or a caller with no file to read.
    Fixed(Ask),
    /// The operator's `providers.toml`, re-read at every ask ([`gate_memory`]). `last` is the newest cleanly read value, kept when the file goes bad. `said` is the refusal already printed, so it is not repeated each pass.
    File {
        path: PathBuf,
        last: Ask,
        said: Option<String>,
    },
}

static GATE_MEMORY: std::sync::Mutex<Option<CapSource>> = std::sync::Mutex::new(None);

fn cap_source() -> std::sync::MutexGuard<'static, Option<CapSource>> {
    GATE_MEMORY.lock().unwrap_or_else(|e| e.into_inner())
}

/// The gate's cap as a VALUE - and a second call takes effect, which is the whole point of the lock.
/// A caller holding the operator's file wants [`set_gate_memory_from_file`] instead.
pub fn set_gate_memory(ask: Ask) {
    *cap_source() = Some(CapSource::Fixed(ask));
}

/// The gate's cap from `providers.toml`: parsed now so a malformed value is refused at start with a sentence naming the file, then re-read at every gate.
/// The `Err` is why a `set` cannot take effect, reported by the caller (`cli.rs`).
pub fn set_gate_memory_from_file(path: &Path) -> Result<Ask, String> {
    let ask = gate_memory_in(path)?;
    *cap_source() = Some(CapSource::File {
        path: path.to_path_buf(),
        last: ask,
        said: None,
    });
    Ok(ask)
}

/// `[memory] gate` as `path` writes it - [`Ask::Default`] (half of what is free when the gate starts)
/// when the file says nothing about it, and an `Err` naming the file when what it says is not an
/// amount of memory.
pub fn gate_memory_in(path: &Path) -> Result<Ask, String> {
    let section = letibot_provider::memory(Some(path));
    Ask::parse(section.gate.as_deref())
        .map_err(|e| format!("[memory] gate in {}: {e}", path.display()))
}

/// The gate's cap in force now: the file's `[memory] gate` if armed ([`set_gate_memory_from_file`]), else [`set_gate_memory`]'s value, else [`GATE_MEMORY_DEFAULT`].
/// [`LocalGate::run`] asks once per gate.
pub fn gate_memory() -> Ask {
    let mut slot = cap_source();
    match slot.as_mut() {
        None => Ask::Default,
        Some(CapSource::Fixed(ask)) => *ask,
        Some(CapSource::File { path, last, said }) => match gate_memory_in(path) {
            Ok(ask) => {
                *last = ask;
                *said = None;
                ask
            }
            Err(why) => {
                if said.as_deref() != Some(why.as_str()) {
                    eprintln!(
                        "harnessd: {why} — the gate keeps {} until that line parses, and reads it \
                         again at the next gate",
                        ask_words(*last)
                    );
                    *said = Some(why.clone());
                }
                *last
            }
        },
    }
}

/// An [`Ask`] as a person reads it - the number a refusal about the file says still stands.
fn ask_words(ask: Ask) -> String {
    match ask {
        Ask::Default => "half of what is free when the gate starts".to_string(),
        Ask::Uncapped => "no cap at all".to_string(),
        Ask::Amount(a) => a.to_string(),
    }
}

static GATE_HOST: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

fn gate_host_slot() -> std::sync::MutexGuard<'static, Option<String>> {
    GATE_HOST.lock().unwrap_or_else(|e| e.into_inner())
}

/// `[queue] gate_host` / `--gate-host`: the declared host the gate's steps run on, `None` for this machine. Read at every gate for [`set_gate_memory`]'s reason. An empty name is `None`.
/// No file is re-read: the flag wins over `leticode.toml` and a re-read would undo it.
pub fn set_gate_host(host: Option<String>) {
    *gate_host_slot() = host.map(|h| h.trim().to_string()).filter(|h| !h.is_empty());
}

/// The host the gate runs on, or `None` for this machine - [`set_gate_host`]'s value.
pub fn gate_host() -> Option<String> {
    gate_host_slot().clone()
}

// ===== The step running now, and the tail of its log =====

/// How much of a running step's log a snapshot reads - 4 KiB from the END, so a `cargo test` that
/// has written forty megabytes costs the same as one that has written forty bytes.
pub const TAIL_BYTES: u64 = 4 * 1024;

/// How many lines of it are kept - a pane's detail, not a log viewer.
pub const TAIL_LINES: usize = 40;

/// One gate's landing worktree, the repository it gates, and the step in it - if any.
struct Watched {
    worktree: PathBuf,
    repository: String,
    step: Option<Running>,
}

/// A step that is running: what it is, which of how many, when it started, and where its output is
/// going. The log's path is here and nowhere else - [`tempfile_in`] makes it per step and deletes it
/// when the step ends.
struct Running {
    command: String,
    /// 1-based, as a person counts steps.
    step: usize,
    of: usize,
    started_ms: u64,
    log: PathBuf,
}

/// Every landing worktree a gate is (or was last) running in - one process-wide list, because the
/// reader is a different thread from the writer: the queue's worker starts a gate on a thread of
/// its own, and a head's `Queues` request builds the snapshot on the server's thread.
static WATCHED: std::sync::Mutex<Vec<Watched>> = std::sync::Mutex::new(Vec::new());

fn watched() -> std::sync::MutexGuard<'static, Vec<Watched>> {
    WATCHED.lock().unwrap_or_else(|e| e.into_inner())
}

/// Which repository a landing worktree is gating - one call from the queue's worker before the gate
/// starts ([`super::serve::RepoQueue::start_gate`]). The gate knows nothing about rows; this is the
/// one fact that lets a snapshot answer *which row's step is this?*
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

/// The step `worktree`'s gate has started - called by both machines' step loops, so a local gate
/// and a host gate answer the pane's question the same way (a host gate streams its step back into
/// the same local log, so the tail is the same read either way).
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

/// The step ended (however it ended) - the worktree has no step running any more.
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

/// The guard a step holds for as long as it runs - see [`RunningFor`].
pub(super) fn step_guard(worktree: &Path) -> RunningFor<'_> {
    RunningFor(worktree)
}

/// What a `gating` row's detail reads: the step, of how many, since when, and the end of its
/// output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningGate {
    pub command: String,
    pub step: usize,
    pub of: usize,
    pub started_ms: u64,
    pub tail: String,
    /// Every byte the step has written, so *the tail has not moved* can be told apart from *the
    /// step is quiet between lines*.
    pub log_bytes: u64,
    /// When the log last grew, in Unix ms - its own mtime, which is the instant the step's last
    /// byte landed, so one read says *this is still going* where the bytes alone cannot.
    pub wrote_ms: Option<u64>,
}

/// The step `repository`'s gate is running, and the tail of its log - `None` when no gate is running
/// for it, which is the honest answer for a row that is not `gating`.
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
        wrote_ms: written_at(&log),
    })
}

/// When a log last grew, in Unix ms - the one fact that separates a step that is working from a step
/// that is waiting for something.
fn written_at(log: &Path) -> Option<u64> {
    let t = std::fs::metadata(log).ok()?.modified().ok()?;
    t.duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis() as u64)
}

/// The end of a log, bounded twice - at most [`TAIL_BYTES`] read (a seek, so the file's size costs
/// nothing to skip) and at most [`TAIL_LINES`] lines kept. This is the only place the pane's side
/// reads a step's output, and it never reads more than that.
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
                "… [the step has written {}; what follows is its last {}]\n",
                many(len as usize, "byte", "bytes"),
                many(lines.len() - from_line, "line", "lines")
            ),
        );
    }
    (tail, len)
}

/// Where a run's steps go, decided once per run: a cgroup of their own (with the cap, or `None` for
/// `uncapped`), or the daemon's own cgroup as before and why.
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

/// The memory the gate means to give a step, in bytes - the `Ask` resolved without needing a cgroup
/// so the same number caps the step and bounds its build ([`StepEnv`]). `uncapped` is `None`, and so
/// is a fraction of what is free when `/proc/meminfo` cannot be read.
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

/// How many rustc jobs the cap pays for: one per [`GATE_JOB_MEMORY`], never fewer than one
/// and never more than the cores this process may use; `None` (uncapped) leaves cargo's own
/// default standing.
pub fn build_jobs(cap: Option<u64>) -> Option<usize> {
    let cap = cap?;
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let by_memory = (cap / GATE_JOB_MEMORY).max(1);
    Some(by_memory.min(cores as u64) as usize)
}

/// What the gate tells a step about itself - the tree's `target/`, the run's scratch directory
/// ([`GateScratch`]) and the build's parallelism from the cap. One struct because the three are one
/// decision about what memory and disk a step may use, and a step must be given them together.
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

/// What a step's memory did, for its row.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum StepMemory {
    /// Its own cgroup's counters.
    Used(MemoryUse),
    /// It ran in the daemon's cgroup, and why.
    Uncapped(String),
    /// It ran on a declared host (`mergequeue::gate_host`), where this daemon makes no cgroup and so
    /// has no peak, cap or count to report. Not `Uncapped` because the sentence must not claim a
    /// cgroup it never looked at.
    Elsewhere(String),
    /// It never started, or the counters could not be read.
    Unknown,
}

/// How close to its cap a step's peak has to be for a red step that showed no failing code to be read
/// as killed at the cap - 95%. The kernel's `oom_kill` count is the proof and is read first; this is
/// the second reading, for a cargo that died with its rustc's still compiling (measured: 96.8%).
pub const NEAR_CAP_PERCENT: u64 = 95;

/// How the step's leader ended - its own exit code, or the signal that ended it. Kept on the row
/// because a cargo killed by `SIGKILL` says nothing on its way out, and a red step whose exit is not
/// recorded cannot be told from a red step that judged the code.
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

/// Why a step that did not pass did not pass - the classifier between *the code is red* and *the gate
/// could not judge it*.
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
    /// The step demanded apparatus this machine does not have - `LETIBOT_REQUIRE_APPARATUS=1` refused
    /// a skip, and the refusal is the whole of what the step says went wrong. The string is the
    /// apparatus in the tree's own words: nothing was judged about the code.
    ApparatusAbsent(String),
}

/// The apparatus a `LETIBOT_REQUIRE_APPARATUS` refusal names, out of the step's own words - or `None`
/// when this output is not that refusal. A workspace run prints it as
///
/// ```text
/// thread 'jobwatch::tests::a_subagent_settles_through_the_jobs_own_channel' (805) panicked at
/// crates/tokencore/src/apparatus.rs:181:9:
/// LETIBOT_REQUIRE_APPARATUS is set, and a process-lifetime tree (cgroup v2; process groups on
/// macOS) is not on this machine.
/// ```
///
/// The rule is a COUNT and not a pairing: the refusal sits on the line after `panicked at`, which a
/// step run with `-- --nocapture` does not promise to keep adjacent, so every panic must have a
/// refusal to go with it. A genuine failure leaves a panic with no refusal behind it.
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
    // The apparatus, in the refusal's own words. A workspace run names several, so all of them are
    // collected - one sentence per thing is what a reader acts on.
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
    // One per line, because an apparatus NAME has commas and semicolons of its own - `a usable
    // confinement boundary (bwrap, an unprivileged user namespace, a cgroup v2 subtree)` - so no
    // punctuation can tell one name from two.
    Some(said.join("\n"))
}

/// What a step that demanded absent apparatus is said to be - the machine, what it has not got, and
/// the two ways out. Never *red*: the code was not read.
pub(crate) fn apparatus_absent_words(step: &str, what: &str, machine: Option<&str>) -> String {
    let here = match machine {
        Some(h) => format!("`{h}`"),
        None => "this machine".to_string(),
    };
    // One thing missing reads as the brief's own sentence - *"the gate could not run here: a cgroup
    // v2 subtree … is not on `lab2x1`"*; several read as a list, because "A; B is not" is not English.
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

/// The signal a kill in the OUTPUT names, rather than a verdict: cargo's own sentence for a child
/// (a rustc, a test binary) that a signal ended.
fn shown_kill(out: &str) -> Option<i32> {
    for (marker, sig) in [
        ("(signal: 9, SIGKILL", libc::SIGKILL),
        ("(signal: 15, SIGTERM", libc::SIGTERM),
    ] {
        if out.contains(marker) {
            return Some(sig);
        }
    }
    None
}

/// Whether the output shows a kill rather than a verdict - see [`shown_kill`].
fn shows_a_kill(out: &str) -> bool {
    shown_kill(out).is_some()
}

/// Output that carries a verdict of its own, which a kill elsewhere in the same run cannot take
/// away: a rustc diagnostic or a failing test, as opposed to cargo's report that one child it ran
/// did not succeed - the very lines a killed child makes cargo print.
fn shows_a_verdict(out: &str) -> bool {
    super::setup::step_words(out).any(|l| {
        let t = l.trim_start();
        t.starts_with("error[")
            || t.starts_with("test result: FAILED")
            || t.contains("panicked at")
            || t.starts_with("Diff in ")
            || t.starts_with("failures:")
    })
}

/// Read a red step ([`RedCause`]). Pure, so the measured case is a unit test: `exit` is how the
/// leader ended, `memory` what its cgroup's counters said, `out` the end of its output.
fn red_cause(out: &str, exit: StepExit, memory: &StepMemory) -> RedCause {
    let code_failed = super::setup::shows_code_failing(out) && !shows_a_kill(out);
    match memory {
        StepMemory::Used(u) if u.cap.is_some() && u.killed_at_cap() => {
            return RedCause::KilledAtCap(format!(
                "the kernel killed {} inside it at the cap",
                many(u.oom_kills as usize, "process", "processes")
            ));
        }
        StepMemory::Used(u) if !code_failed && u.near_cap() => {
            // .F10: no percent and no gaps - the words are what reaches a reviewer.
            return RedCause::KilledAtCap(format!(
                "it died near its memory cap ({}) with no failing code in its output — out of \
                 memory, not a verdict",
                exit.clause()
            ));
        }
        _ => {}
    }
    // The machine's absence, before the code's - a `LETIBOT_REQUIRE_APPARATUS` refusal is a PANIC, and
    // [`super::setup::shows_code_failing`] reads `panicked at` as the code failing, so this order is
    // the whole of the fix.
    if let Some(what) = apparatus_absent(out) {
        return RedCause::ApparatusAbsent(what);
    }
    // A child killed far from the cap (the OOM killer took a rustc) leaves cargo with its own code and the kill in the output, so the output's signal is the answer.
    // Unless the output also shows code failing: no kill beside it removes that verdict.
    if !shows_a_verdict(out)
        && let Some(sig) = shown_kill(out)
    {
        return RedCause::KilledBySignal(sig);
    }
    match exit {
        StepExit::Signal(s) if !code_failed => RedCause::KilledBySignal(s),
        _ => RedCause::Code,
    }
}

/// The gate's evidence as a model reads it : the `[gate memory: …]` line is dropped unless
/// the step was killed at its cap or came near it. The row keeps it for the operator; the model is
/// not handed peaks to size work by.
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
                "[gate memory: KILLED at the gate's cap — {}; the kernel killed {} inside it. This \
                 is `[memory] gate` in providers.toml, not the code]",
                u.clause(),
                many(u.oom_kills as usize, "process", "processes")
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
            StepMemory::Unknown => {
                let e = exit?;
                return Some(format!("[step: {}]", e.clause()));
            }
        };
        if let Some(e) = exit {
            line.push_str(&format!("\n[step: {}]", e.clause()));
        }
        if leftover > 0 {
            line.push_str(&format!(
                "\n[{} the step left running were killed with its cgroup]",
                many(leftover, "process", "processes")
            ));
        }
        Some(line)
    }
}

/// What the gate came to - four outcomes and not one, because *the branch is bad*, *nobody could tell
/// in time*, *the daemon stopped* and *the gate could not run* send a person to four different places.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateVerdict {
    /// Every step was green.
    Passed,
    /// A step was red: the branch (or the target) is bad. The step and the tail of its output.
    Failed(String),
    /// The deadline passed and the gate was killed - not `failed`.
    TimedOut(String),
    /// The daemon is stopping (or lost its lease): the gate was killed and judged nothing.
    Stopped(String),
    /// The gate could not run at all - a step that could not be spawned, an output file that
    /// could not be made. Infrastructure, not the branch.
    Infra(String),
}

/// One run: the rows (one per step, in order) and the verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateRun {
    pub steps: Vec<MergeGateStep>,
    pub verdict: GateVerdict,
}

/// The controls a run is under - its deadline, the daemon's stop flag, and the heartbeat, which
/// answers `false` when the lease is gone (the gate is then killed: a daemon that does not own the
/// repository must not land into it).
pub struct GateControl<'a> {
    pub deadline: Instant,
    /// How much time the deadline is - `deadline` is the instant it falls on, and a sentence about a
    /// timeout has to name the BUDGET rather than leave a reader guessing which clock ran out: a poll
    /// on a declared host is bounded by seconds, and the step is bounded by this.
    pub budget: Duration,
    pub stop: &'a AtomicBool,
    pub alive: &'a dyn Fn() -> bool,
}

/// Who runs a gate - 's seam, and the fleet is now one of its two implementations.
pub trait GateRunner: Send + Sync {
    /// Run `steps` in `worktree`, in order, stopping at the first red one.
    fn run(&self, worktree: &Path, steps: &[String], ctl: &GateControl) -> GateRun;
    /// What this runner is, for the row: `local`, or the host's name.
    fn name(&self) -> String {
        "local".to_string()
    }
    /// This runner's form for `[queue] gate_in = "firecode"`, or `None` when it has none.
    fn firecode(&self) -> Option<Arc<dyn GateRunner>> {
        None
    }
}

/// The daemon's gate - decision 8: *"the gate runs here for now"*, and, since the
/// operator's 2026-10-29 ruling, on a declared host when `[queue] gate_host` /
/// `--gate-host` names one.
#[derive(Debug, Default, Clone)]
pub struct LocalGate;

impl GateRunner for LocalGate {
    /// Host and cap are read when the gate starts, not at daemon start, so an edited cap applies to the next gate.
    /// [`run_with_memory`] resolves the cap once, so one gate never judges two steps by different numbers.
    fn run(&self, worktree: &Path, steps: &[String], ctl: &GateControl) -> GateRun {
        run_gate(gate_host().as_deref(), worktree, steps, ctl, gate_memory())
    }

    /// The name the row carries: the host's, or `local`.
    fn name(&self) -> String {
        gate_host().unwrap_or_else(|| "local".to_string())
    }

    fn firecode(&self) -> Option<Arc<dyn GateRunner>> {
        Some(Arc::new(super::gate_firecode::FirecodeGate {
            host: gate_host(),
            ready: Arc::new(super::gate_firecode::Doctor),
            memory: gate_memory(),
        }))
    }
}

/// The gate on one declared host - `hosts.toml`'s machine, reached over its own key and
/// root (`mergequeue::gate_host`).
#[derive(Debug, Clone)]
pub struct HostGate {
    pub host: String,
}

impl GateRunner for HostGate {
    fn run(&self, worktree: &Path, steps: &[String], ctl: &GateControl) -> GateRun {
        super::gate_host::run_on_host(&self.host, worktree, steps, ctl, false)
    }

    fn name(&self) -> String {
        self.host.clone()
    }
}

/// [`LocalGate`]'s run, under a memory cap named by the caller - `[memory] gate` in the daemon, a
/// small one in the test that proves a step is killed at it alone.
pub fn run_with_memory(
    worktree: &Path,
    steps: &[String],
    ctl: &GateControl,
    memory: Ask,
) -> GateRun {
    run_with_launcher(worktree, steps, ctl, memory, &|s| s.to_string())
}

/// [`run_with_memory`] where `launch` turns a step into the command line the shell runs: the row,
/// the deadline and the verdict stay the step's. How `[queue] gate_in = "firecode"` puts a VM
/// between the step and the host.
pub fn run_with_launcher(
    worktree: &Path,
    steps: &[String],
    ctl: &GateControl,
    memory: Ask,
    launch: &dyn Fn(&str) -> String,
) -> GateRun {
    // The cap is resolved once and used twice: the cgroup the step joins is capped with it and the
    // build's parallelism is bounded by it ([`StepEnv`]) - a step judged by two different numbers is
    // the bug this closes.
    let cap = intended_cap(memory);
    let place = placement(memory, cap);
    // One scratch for the whole run, and the gate owns it: made here, handed to every step
    // ([`StepEnv`]), and removed however this function returns - which is why it is a `Drop`
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
            &launch(step),
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

/// The one place a step's end becomes a row and a verdict - shared by both machines, so a host gate
/// cannot read a red step differently from a local one.
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
    // The note is inside the row's cap too - [`read_excerpt`] leaves [`NOTE_BYTES`] for it, and this
    // is what makes that a budget rather than an expectation.
    let note = evidence.map(|line| cut_to(&line, NOTE_BYTES, " …"));
    let with_memory = |out: String| match &note {
        Some(line) if out.is_empty() => line.clone(),
        Some(line) => format!("{out}\n{line}"),
        None => out,
    };
    let (word, output, verdict) = match outcome {
        StepEnd::Red(out, exit) => {
            // A step that did not pass is read once more before it is called red: killed at the cap
            // or near it with no failing code, is the gate's limit, and killed by a signal before any
            // code failed is somebody else's hand. Neither judged the branch.
            let cause = red_cause(&out, exit, &mem);
            let clause = match &mem {
                StepMemory::Used(u) => u.clause(),
                StepMemory::Uncapped(_) => "uncapped".to_string(),
                StepMemory::Elsewhere(host) => format!("on `{host}`"),
                StepMemory::Unknown => "its memory could not be read".to_string(),
            };
            // A host gate's step is not in a cgroup this daemon made, so the sentence must not claim
            // a count it never took. Same words everywhere else.
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
                // The machine, not the branch: the step refused to skip, so nothing it would have
                // judged was judged. The row is `not_run` - neither green nor red.
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
        // The step's own bound, named: this is the gate's deadline and not any poll's, because a
        // reader has to be able to tell a slow build from a slow machine.
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

/// A run that could not start at all - a scratch directory the gate cannot make. Every step's row is
/// `NotRun` and the verdict is `Infra`: never `failed`, which would say the branch is red when nothing
/// about it was judged.
pub(crate) fn could_not_run(steps: &[String], why: String) -> GateRun {
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

/// A duration as a sentence says it - `30 min`, `90 s`, `1500 ms`.
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

/// The rows and the verdict, from the steps that ran - `done` is a prefix of `steps`, and the step
/// that ended the run is the last of it. Every step after that one is `not_run` with no output
/// because a step the gate never reached is not a step that was green.
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

/// The gate, on the machine the daemon was told to use - `host` `None` is this machine.
pub fn run_gate(
    host: Option<&str>,
    worktree: &Path,
    steps: &[String],
    ctl: &GateControl,
    memory: Ask,
) -> GateRun {
    match host {
        Some(name) => super::gate_host::run_on_host(name, worktree, steps, ctl, false),
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

/// One step: its end, what its memory did, and how many processes it left behind in its cgroup
/// (killed with it). `at` is which step this is, 1-based, and `of` how many the gate has - the
/// `2 of 4` a pane reads beside the tail.
fn run_step(
    worktree: &Path,
    step: &str,
    command: &str,
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
    // The step a pane can see from here on - the log's path is this function's secret, so it is said
    // once here and cleared by the guard whatever way the step ends.
    watch_step(worktree, step, at.0, at.1, &log);
    let _running = step_guard(worktree);
    // The step's own cgroup, or the daemon's as before and why. A scope that cannot be made is not a
    // reason to refuse the gate - the gate ran uncapped for its whole life before - so it runs as
    // before and the row says so.
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
            .args(["sh", "-c", command]),
        None => cmd.arg("-c").arg(command),
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
    let _record = super::gate_orphans::write(
        &super::gate_orphans::dir(),
        child.id(),
        worktree,
        step,
        scope.as_ref().map(|s| s.path()),
    );
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

/// `SIGTERM` to the whole group, five seconds, then `SIGKILL` - and `cgroup.kill` on the step's
/// own cgroup when it has one, which reaches a grandchild that left the group (`setsid`, a
/// daemonising test server) - and the child reaped.
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
    // The gate's own log, in the box's temp dir and not the step's scratch, so the step cannot write
    // its own evidence. The sequence number keeps two gates starting in one millisecond from sharing
    // one log and reading each other's truncation.
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

/// A step's output as its row keeps it - [`excerpt`] at [`EXCERPT_BYTES`], the room left of
/// [`EVIDENCE_BYTES`] once the note [`judge`] appends has been paid for. Both machines read a step's
/// output through here, so the row's arithmetic is the same on both.
pub(crate) fn read_excerpt(log: &Path) -> String {
    let bytes = std::fs::read(log).unwrap_or_default();
    excerpt(&String::from_utf8_lossy(&bytes), EXCERPT_BYTES)
}

/// The end of `text`, at most `cap` bytes in all - the line that says what was left out is inside the
/// cap and not added to it, and it names the byte count it actually dropped. `what` names the bytes
/// the reader is missing.
fn end_of(text: &str, cap: usize, what: &str) -> String {
    if text.len() <= cap {
        return text.to_string();
    }
    // The marker's own width depends on the number in it, so the room left for the tail is taken from
    // the WIDEST marker this text can produce. A narrower one then leaves a few bytes unused, which
    // is the harmless direction.
    let marker = |dropped: usize| format!("… [{} {what}]\n", many(dropped, "byte", "bytes"));
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

/// The most of a step's evidence the failure section may take, so a tail survives beside it - what
/// follows a failure is still worth reading when the failure was a build error.
const FAILURE_BYTES: usize = 2_400;

/// How many failing targets the section describes in full; the rest are counted.
const FAILING_TARGETS: usize = 4;

/// How many lines of a panic follow its `panicked at` line - the assertion, the io error.
const PANIC_LINES: usize = 2;

/// The width a quoted line is clipped to, so one long assertion cannot fill the section.
const QUOTED_WIDTH: usize = 240;

/// What the failure section says when the cap cannot hold even it - the LAST thing cut, and
/// only when there is nothing else left to cut.
const SECTION_CUT: &str = "\n[gate excerpt: the failure section itself was cut here]";

/// The least tail worth keeping - below this the end is left out and the section says so
/// rather than the reader getting a marker and three lines.
const MIN_TAIL: usize = 320;

/// The words that say the MACHINE refused the step something - a `fork` with no descriptor left, an
/// allocation the box could not make, a disk with no room. A `fork` at the 256-descriptor ceiling
/// reaches the output as `Too many open files`, which is why that sentence is looked for at all.
const EXHAUSTED: [&str; 5] = [
    "Too many open files",
    "os error 24",
    "Cannot allocate memory",
    "No space left on device",
    "Resource temporarily unavailable",
];

/// What one failing target's output said - the first test that failed in it and why.
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

/// What a step's WHOLE output says about the code - the counts, cargo's own list of failing targets
/// and the first failure of each. Read from all of the stream because the end of a `cargo test
/// --workspace --no-fail-fast` run is every OTHER target's `running 0 tests` blocks.
#[derive(Debug, Default, PartialEq)]
struct Failures {
    /// Targets libtest started - one `running N tests` line each.
    targets_ran: usize,
    targets_passed: usize,
    targets_failed: usize,
    tests_passed: u64,
    tests_failed: u64,
    /// Cargo's verdict verbatim: `error: N target(s) failed:` and the names under it.
    summary: Vec<String>,
    /// The names under that verdict without their backticks - cargo's own names for the failing
    /// targets, which is what a target's segment is matched against.
    named: Vec<String>,
    /// One per failing target, in the order the targets ran.
    targets: Vec<FailingTarget>,
    /// Lines from anywhere in the stream that say the machine refused the step something.
    exhausted: Vec<String>,
}

/// `s` cut to `width` bytes at a character boundary, saying it was cut.
fn clip(s: &str, width: usize) -> String {
    if s.len() <= width {
        return s.to_string();
    }
    format!("{} …", bytes_head(s, width))
}

/// `s` cut to at most `room` bytes and ending with `marker` - the marker is inside the room, not added
/// to it. When even the marker does not fit the marker gives way: a cap exceeded by the sentence about
/// exceeding it is how a row came out 338 bytes over.
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

/// The number immediately before `what` in `s` - `782` for `… 782 passed; 1 failed; …`.
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

/// `test worker::a_test... FAILED` → `worker::a_test`.
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

/// `running 12 tests` - one per target libtest started, so the count says how much of a run the output
/// is talking about; a workspace run whose every target prints `running 0 tests` is otherwise
/// indistinguishable from a run that did nothing.
fn running_count(s: &str) -> Option<usize> {
    let rest = s.strip_prefix("running ")?;
    let n = rest.split(' ').next()?.parse::<usize>().ok()?;
    rest.contains(" test").then_some(n)
}

/// ` Running unittests src/lib.rs (target/debug/deps/x-1)` → `unittests src/lib.rs`.
fn running_descriptor(s: &str) -> Option<String> {
    let rest = s.strip_prefix("Running ")?;
    Some(match rest.rsplit_once(" (") {
        Some((what, _)) => what.to_string(),
        None => rest.to_string(),
    })
}

/// Where one target's output begins - cargo's own ` Running …` or ` Doc-tests …` line, the
/// only thing in the stream that says where one target's output ends and the next begins.
fn target_start(raw: &str) -> Option<String> {
    if !raw.starts_with(char::is_whitespace) {
        return None;
    }
    let s = raw.trim();
    if s.starts_with("Running ") && s.contains(" (") && s.ends_with(')') {
        return running_descriptor(s);
    }
    let doc = s.strip_prefix("Doc-tests ")?;
    (!doc.is_empty() && !doc.contains(char::is_whitespace)).then(|| s.to_string())
}

/// The clause cargo runs a target again by, derived from the descriptor cargo printed for it -
/// `unittests src/lib.rs` is `--lib`, `tests/httphead.rs` is `--test httphead`. This is what ties a
/// target's segment to its name in cargo's own `error: N target(s) failed:` list.
fn descriptor_clause(descriptor: &str) -> Option<String> {
    if let Some(doc) = descriptor.strip_prefix("Doc-tests ") {
        return (!doc.is_empty()).then(|| "--doc".to_string());
    }
    let path = descriptor.strip_prefix("unittests ").unwrap_or(descriptor);
    if path == "src/lib.rs" {
        return Some("--lib".to_string());
    }
    if let Some(rest) = path.strip_prefix("tests/") {
        let name = rest.strip_suffix(".rs")?;
        let name = name.strip_suffix("/main").unwrap_or(name);
        return (!name.is_empty()).then(|| format!("--test {name}"));
    }
    let bin = path.strip_prefix("src/bin/")?.strip_suffix(".rs")?;
    (!bin.is_empty()).then(|| format!("--bin {bin}"))
}

/// Whether `clause` is cargo's name for a target that `want` would be run as - `-p letibot-harnessd
/// --test httphead` for `--test httphead`, and `--lib` for `--lib`.
fn names_clause(clause: &str, want: &str) -> bool {
    clause == want || clause.ends_with(&format!(" {want}"))
}

/// One target's slice of a step's output, cut at the line cargo printed before running it.
/// `descriptor` is empty for the lines before the first one - the build.
struct Segment<'a> {
    descriptor: String,
    lines: &'a [&'a str],
}

impl<'a> Segment<'a> {
    /// The stream cut into targets, in the order they ran. A stream with no `Running` line at all - an
    /// older cargo, a hand-written step, a runner that is not cargo - comes back as ONE segment, which
    /// is the honest reading: nothing said where a target began, so nothing may claim to know.
    fn all(lines: &'a [&'a str]) -> Vec<Segment<'a>> {
        let mut out: Vec<Segment<'a>> = Vec::new();
        let mut from = 0usize;
        let mut descriptor = String::new();
        for (i, raw) in lines.iter().enumerate() {
            let Some(d) = target_start(raw) else {
                continue;
            };
            if i > from || !descriptor.is_empty() {
                out.push(Segment {
                    descriptor: std::mem::take(&mut descriptor),
                    lines: &lines[from..i],
                });
            }
            from = i;
            descriptor = d;
        }
        if from < lines.len() {
            out.push(Segment {
                descriptor,
                lines: &lines[from..],
            });
        }
        out
    }

    /// The `error: test failed, to rerun pass …` cargo wrote INSIDE this target's own output -
    /// cargo writes it once per failing target, after that target's block and before the next
    /// `Running` line, which is what makes it this target's name and not a neighbour's.
    fn clause(&self) -> Option<String> {
        self.lines.iter().find_map(|raw| rerun_clause(raw.trim()))
    }
}

/// Whether two spellings name one test - libtest adds ` - should panic` to the name it prints in
/// the run and not to the header of the test's own output block, so one test has two names.
fn same_test(a: &str, b: &str) -> bool {
    a == b
        || a.strip_prefix(b).is_some_and(|r| r.starts_with(" - "))
        || b.strip_prefix(a).is_some_and(|r| r.starts_with(" - "))
}

/// `thread 'worker::a_test' (472254183) panicked at crates/…:12:5:` → `worker::a_test`.
/// libtest runs a test on a thread named after it, which is what ties a panic printed in
/// the run to its test. The name ends at the closing quote, not at the end of the head
fn panic_thread(s: &str) -> Option<&str> {
    let at = s.find("panicked at")?;
    let rest = s[..at].trim().strip_prefix("thread '")?;
    rest.split_once('\'').map(|(name, _)| name)
}

/// Every test a segment reported FAILED, in the order it reported them - the `test <name>...
/// FAILED` lines, and the `---- <name> stdout ----` headers (libtest writes one per failed test
/// and for no other kind of test).
fn failed_names(seg: &[&str]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for raw in seg {
        let s = raw.trim();
        if let Some(name) = test_failed_name(s).or_else(|| block_name(s))
            && !names.iter().any(|n| same_test(n, &name))
        {
            names.push(name);
        }
    }
    names
}

/// What the failed test printed - and only if it failed. libtest writes a failing test's own output
/// under `---- <name> stdout ----`, and with `-- --nocapture` it prints it in the run instead. Both are
/// tied to the test by NAME, because quoting another test's panic is worse than saying nothing.
fn evidence(seg: &[&str], test: &str) -> Vec<String> {
    let mut block: Option<String> = None;
    for (i, raw) in seg.iter().enumerate() {
        let s = raw.trim();
        if let Some(name) = block_name(s) {
            block = Some(name);
            continue;
        }
        if s == "failures:" || s.starts_with("test result: ") {
            block = None;
        }
        if !s.contains("panicked at") {
            continue;
        }
        let in_its_block = block.as_deref().is_some_and(|b| same_test(b, test));
        let its_thread = panic_thread(s).is_some_and(|t| same_test(t, test));
        if !in_its_block && !its_thread {
            continue;
        }
        let mut out = vec![clip(s, QUOTED_WIDTH)];
        // The panic's own message follows it: the assertion, the io error, the sentence.
        let mut j = i + 1;
        while j < seg.len() && out.len() <= PANIC_LINES {
            let said = seg[j].trim();
            if said.is_empty() {
                break;
            }
            out.push(clip(said, QUOTED_WIDTH));
            j += 1;
        }
        return out;
    }
    // No panic this test printed - a `#[should_panic]` that did not panic, a test that failed on a
    // status libtest reported only by name. The NAME is what a person needs.
    vec![clip(&format!("test {test} ... FAILED"), QUOTED_WIDTH)]
}

/// One failing target, read out of its OWN segment of the output - the first test that
/// segment reported FAILED and the panic it printed. `clause` is cargo's name for the
/// target, empty when nothing named it
fn failing_target(seg: &[&str], clause: &str, descriptor: &str) -> Option<FailingTarget> {
    let names = failed_names(seg);
    let cargo_said = !clause.is_empty();
    let libtest_said = !names.is_empty()
        || seg
            .iter()
            .any(|raw| raw.trim().starts_with("test result: FAILED."));
    if !cargo_said && !libtest_said {
        return None;
    }
    let target = if !clause.is_empty() {
        clause.to_string()
    } else if !descriptor.is_empty() {
        descriptor.to_string()
    } else {
        seg.iter()
            .find_map(|l| target_start(l))
            .unwrap_or_else(|| "the step's own output".to_string())
    };
    let Some(test) = names.first().cloned() else {
        // A target that failed and named no test: its binary never started, or it was killed
        // before libtest could report. Saying so is the honest answer.
        return Some(FailingTarget {
            target,
            test: None,
            panic: Vec::new(),
            failed: 0,
        });
    };
    let panic = evidence(seg, &test);
    Some(FailingTarget {
        target,
        test: Some(test),
        panic,
        failed: names.len(),
    })
}

/// Read a step's whole output once, for what it says about the code. The stream is cut into
/// ONE SEGMENT PER TARGET at the line cargo prints before running it, because a cut per
/// `error
fn failures(text: &str) -> Failures {
    let lines: Vec<&str> = text.lines().collect();
    let mut f = Failures::default();
    for (i, raw) in lines.iter().enumerate() {
        let s = raw.trim();
        if targets_failed_head(s) {
            f.summary.push(s.to_string());
            // The names under it, indented - `` `-p letibot-harnessd --lib` ``.
            let mut j = i + 1;
            while j < lines.len()
                && !lines[j].trim().is_empty()
                && lines[j].starts_with(char::is_whitespace)
            {
                f.summary.push(lines[j].trim().to_string());
                f.named.push(lines[j].trim().trim_matches('`').to_string());
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
    // Each target on its own. The name is cargo's: the clause inside the target's own output first
    // then the same target's name in cargo's `error: N target(s) failed:` list - which still names a
    // target whose output cargo never reached.
    for seg in Segment::all(&lines) {
        let clause = seg.clause().or_else(|| {
            let want = descriptor_clause(&seg.descriptor)?;
            f.named.iter().find(|c| names_clause(c, &want)).cloned()
        });
        if let Some(t) = failing_target(seg.lines, clause.as_deref().unwrap_or(""), &seg.descriptor)
        {
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
    /// Whether this output says anything worth leading with - a named failure, or a machine that
    /// refused the step something wherever it said so.
    fn says_something(&self) -> bool {
        !self.targets.is_empty() || !self.summary.is_empty() || !self.exhausted.is_empty()
    }

    /// The one line the excerpt ENDS with, naming what failed - because a reader that keeps only the
    /// end of a row is a real reader (`notices::failing_step` hands the model the last
    /// [`super::notices::TAIL_LINES`] lines of exactly this string, and a head's card does the same).
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

    /// The failure section, at most `budget` bytes: the counts, cargo's list of failing targets, then
    /// each target's first failing test and its panic - and, from anywhere in the stream, the lines
    /// that say a resource ran out.
    fn words(&self, budget: usize) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "[gate excerpt: {} ran — {} passed, {} failed; {} passed, {} failed. What follows is \
             taken from the whole of the step's output, not only its end]\n",
            many(self.targets_ran, "target", "targets"),
            self.targets_passed,
            self.targets_failed,
            many(self.tests_passed as usize, "test", "tests"),
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
                "[gate excerpt: and {} named above]\n",
                many(
                    self.targets.len() - FAILING_TARGETS,
                    "more failing target",
                    "more failing targets"
                )
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

/// What a failed step leaves on its row - the failure first, then as much of the end as the
/// cap allows, because a failing test's name and panic sit hundreds of lines above the last
/// 4 KiB: behind the workspace's other targets printing `running 0 tests`.
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
        // No room for the end. The section and the footer stay, and if the cap cannot hold both the
        // section is what gives way - it is still the last thing to be cut.
        let left_out = format!(
            "[gate excerpt: the end of the step's output is left out — it is {} in all — because \
             what failed is above]",
            many(text.len(), "byte", "bytes")
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
    // Enforced, not aimed at: every budget above is exact, and this makes the cap a fact about the
    // answer rather than about the arithmetic that produced it.
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

/// This run's scratch directory, absolute - [`GATE_TMP_ROOT`]`/<the tree's name, at most
/// [`GATE_TMP_NAME_CHARS`]>-<this process's run number>`. The name is per RUN, not per
/// tree
pub fn gate_tmp_dir(worktree: &Path) -> PathBuf {
    gate_tmp_dir_in(Path::new(GATE_TMP_ROOT), worktree)
}

/// [`gate_tmp_dir`] under a scratch root the caller chose: a test's own directory, or a box whose
/// [`GATE_TMP_ROOT`] is not on a disk (the property is the root's, not this string's).
pub fn gate_tmp_dir_in(root: &Path, worktree: &Path) -> PathBuf {
    static RUN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let tree: String = worktree
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
        .chars()
        .take(GATE_TMP_NAME_CHARS)
        .collect();
    let run = RUN.fetch_add(1, Ordering::Relaxed) + 1;
    root.join(format!("{tree}-{run}"))
}

/// A run's scratch directory, made when the run starts and removed when it ends - on every way out of
/// [`run_with_memory`], the early returns included, which is why it is a `Drop`.
struct GateScratch(PathBuf);

impl GateScratch {
    /// Make this run's scratch. Anything already at the name is cleared first: the name is per run
    /// so what is there is a run that was killed before it could remove its own.
    fn for_run(worktree: &Path) -> Result<GateScratch, String> {
        GateScratch::for_run_in(Path::new(GATE_TMP_ROOT), worktree)
    }

    /// The same under a root the caller chose. See [`gate_tmp_dir_in`].
    fn for_run_in(root: &Path, worktree: &Path) -> Result<GateScratch, String> {
        use std::os::unix::fs::DirBuilderExt;
        let dir = gate_tmp_dir_in(root, worktree);
        let _ = std::fs::remove_dir_all(&dir);
        // `0700`: a step's scratch holds what the step wrote - on this tree that includes a copy of the
        // operator's store - and `/var/tmp` is world-writable. The mode is set on [`GATE_TMP_ROOT`]
        // as well, when this makes it.
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
    /// Best effort, and said when it is not enough - the voice the rest of the queue uses for what it
    /// could not do. A scratch left behind is disk on the box, and the next run of that tree clears
    /// its own name before it starts.
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.0)
            && self.0.exists()
        {
            eprintln!(
                "  merge queue: the gate's scratch {} could not be removed: {e}",
                self.0.display()
            );
        }
    }
}

/// The queue's clock, from the tree's one, re-exported because every row this module moves is
/// stamped through `gate::now_ms`.
pub(crate) use letibot_metrics::now_ms;

/// The gate's steps and the install steps split off them ([`INSTALL_MARK`]).
pub type GateAndInstall = (Vec<String>, Vec<String>);

/// The steps, as the TARGET declares them at `at` - read from that commit's `AGENTS.md` (any case), so
/// a branch cannot change its own gate. `Ok(None)` when the target declares no gate; the second list
/// is the install steps ([`INSTALL_MARK`]), split off to run after the push.
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

/// The one row a target with no gate records - not an empty list, which reads *nothing ran*.
pub fn no_gate_row(why: &str) -> MergeGateStep {
    MergeGateStep {
        command: String::new(),
        outcome: MergeGateOutcome::NoGate,
        output: why.to_string(),
        started_ms: now_ms(),
        elapsed_ms: 0,
    }
}

/// The green gate - every test's runner that is not about the gate itself.
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
#[cfg(test)]
mod tests;
