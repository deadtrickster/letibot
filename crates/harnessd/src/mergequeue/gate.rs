//! **The gate: the target's own `## Merge gate`, run with a deadline, in a process group, and
//! killable** (R61 requirement 4, decision 9).
//!
//! The v23 gate was `Command::output()`: no timeout, no process group, the whole output in
//! memory, and a thread detached at exit — a hung step held its row `taken` for ever and, after
//! a restart, the orphaned `cargo test` still held the worktree's `target` lock (defect 6). Here
//! every step is `sh -c <step>` in a NEW process group whose output goes to a file; the runner
//! polls it, says the owner is alive between polls (the lease heartbeat), and kills the whole
//! group — `SIGTERM`, then `SIGKILL` — when the deadline passes, when the daemon stops, or when
//! the heartbeat says the lease is gone.
//!
//! **Each step runs in a cgroup of its own** (R76 item 6), a sibling of the daemon's leaf in the
//! delegated scope ([`letibot_tools::exec::SiblingScope`]), under `[memory] gate` from
//! `providers.toml` — half of what is free when the gate starts unless it says otherwise. Before
//! this, the step's `sh` inherited the daemon's own leaf and its 16 GiB `memory.high` (R66), so a
//! heavy `cargo test --release` throttled every session the daemon served. The step joins its
//! cgroup before `exec` (`join_script`), the kill reaches it through `cgroup.kill` as well as the
//! process group, and the row's evidence ends with the step's peak — and, when the kernel killed
//! it at the cap, says that, so a gate that ran out of ITS memory reads as the gate's limit and
//! not as red code. A daemon with no delegated scope (an old-style start) runs the step as
//! before and the evidence says *uncapped*.
//!
//! **The runner is an interface** ([`GateRunner`]) with one implementation here
//! ([`LocalGate`]). R62 runs the gate on the fleet (lubuntu1/2 over `ssh -i`); that is a second
//! implementation of the same trait, and nothing else in the queue has to change for it.

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

/// How much of a step's output the row keeps — the end, which is where a failure says why.
pub const EVIDENCE_BYTES: usize = 4_096;

/// **The marker of an INSTALL step** — run after the push, never before it (decision 7). The
/// gate in `AGENTS.md` lists `sh scripts/install-gate-build.sh` as its last step; a landing that
/// installed before it pushed installed code that was not on the target when the push failed.
pub const INSTALL_MARK: &str = "install-gate-build";

/// **A gate step's memory cap when `[memory] gate` says nothing: half of what is free when the
/// gate starts.** The same line a model's call may take without asking the operator (R65's
/// `ceiling`), because a gate is the operator's own run and is owed the operator's line — and
/// the other half is what stays for the daemon and the sessions it serves, which is the whole
/// point of giving the gate a cgroup of its own. A fraction rather than a size because the
/// fleet's boxes differ by an order of magnitude (lab2x1's 185 GB, a laptop's 16): a fixed
/// 16 GiB would starve a release build on one and leave the other with nothing.
pub const GATE_MEMORY_DEFAULT: Amount = Amount::Fraction(0.5);

/// **The clause a target's evidence (or a branch's row) leads with when its gate's step was
/// killed before it judged the code** — at the gate's memory cap, or by a signal from outside.
/// Read like [`super::setup::GATE_SETUP_FAILED`]: the tip is UNJUDGED, not red.
pub const GATE_KILLED: &str = "the gate's step was killed before it could judge the code";

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

/// **Where a run's steps go, decided once per run**: a cgroup of their own (with the cap, or
/// `None` for `uncapped`), or the daemon's own cgroup as before R76 and why.
#[derive(Debug, Clone, PartialEq)]
enum Placement {
    Scoped(Option<u64>),
    Unscoped(String),
}

fn placement(ask: Ask) -> Placement {
    if let Err(why) = letibot_tools::exec::delegate_memory() {
        return Placement::Unscoped(why.clone());
    }
    let amount = match ask {
        Ask::Uncapped => return Placement::Scoped(None),
        Ask::Default => GATE_MEMORY_DEFAULT,
        Ask::Amount(a) => a,
    };
    match (amount, Meminfo::read()) {
        (Amount::Bytes(b), _) => Placement::Scoped(Some(b)),
        (a, Some(m)) => Placement::Scoped(Some(a.resolve(m.available))),
        (_, None) => Placement::Unscoped(
            "`/proc/meminfo` could not be read, so a fraction of what is free means nothing".into(),
        ),
    }
}

/// **What a step's memory did**, for its row.
#[derive(Debug, Clone, PartialEq)]
enum StepMemory {
    /// Its own cgroup's counters.
    Used(MemoryUse),
    /// It ran in the daemon's cgroup, and why.
    Uncapped(String),
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
    match exit {
        StepExit::Signal(s) if !code_failed => RedCause::KilledBySignal(s),
        _ => RedCause::Code,
    }
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
            StepMemory::Used(u) => format!("[gate memory: {}]", u.clause()),
            StepMemory::Uncapped(why) => {
                format!("[gate memory: uncapped — the step ran in the daemon's own cgroup: {why}]")
            }
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
    pub stop: &'a AtomicBool,
    pub alive: &'a dyn Fn() -> bool,
}

/// **Who runs a gate** — the seam R62 fills with the fleet.
pub trait GateRunner: Send + Sync {
    /// Run `steps` in `worktree`, in order, stopping at the first red one.
    fn run(&self, worktree: &Path, steps: &[String], ctl: &GateControl) -> GateRun;
    /// What this runner is, for the row: `local`, or a runner's name.
    fn name(&self) -> String {
        "local".to_string()
    }
}

/// **The gate on this machine** — decision 8: *"the gate runs here for now"*.
#[derive(Debug, Default, Clone)]
pub struct LocalGate;

impl GateRunner for LocalGate {
    fn run(&self, worktree: &Path, steps: &[String], ctl: &GateControl) -> GateRun {
        run_with_memory(worktree, steps, ctl, gate_memory())
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
    let target = gate_target_dir(worktree);
    let place = placement(memory);
    let mut rows: Vec<MergeGateStep> = Vec::with_capacity(steps.len());
    for (i, step) in steps.iter().enumerate() {
        let started = now_ms();
        let (outcome, mem, leftover) = run_step(worktree, step, &target, ctl, &place);
        let elapsed = now_ms().saturating_sub(started);
        let exit = match &outcome {
            StepEnd::Red(_, e) => Some(*e),
            _ => None,
        };
        let evidence = mem.evidence(leftover, exit);
        let with_memory = |out: String| match &evidence {
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
                    StepMemory::Unknown => "its memory could not be read".to_string(),
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
                            "{GATE_KILLED}: `{step}` was {} before any code failed ({clause}; \
                             no OOM kill in its cgroup, so not by the gate's cap). It judged \
                             nothing about the code — the tip is UNJUDGED, not red. Gate it \
                             again once whatever killed it is gone",
                            StepExit::Signal(sig).clause()
                        ))),
                    ),
                }
            }
            StepEnd::Ok(out) => (MergeGateOutcome::Passed, with_memory(out), None),
            StepEnd::Deadline(out) => (
                MergeGateOutcome::TimedOut,
                with_memory(out),
                Some(GateVerdict::TimedOut(format!(
                    "`{step}` was still running at the gate's deadline and was killed with \
                         its whole process group"
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
        rows.push(MergeGateStep {
            command: step.clone(),
            outcome: word,
            output,
            started_ms: started,
            elapsed_ms: elapsed,
        });
        if let Some(v) = verdict {
            rows.extend(steps[i + 1..].iter().map(|s| MergeGateStep {
                command: s.clone(),
                outcome: MergeGateOutcome::NotRun,
                output: String::new(),
                started_ms: 0,
                elapsed_ms: 0,
            }));
            return GateRun {
                steps: rows,
                verdict: v,
            };
        }
    }
    GateRun {
        steps: rows,
        verdict: GateVerdict::Passed,
    }
}

enum StepEnd {
    Ok(String),
    Red(String, StepExit),
    Deadline(String),
    Stopped(String, String),
    Infra(String),
}

/// One step: its end, what its memory did, and how many processes it left behind in its
/// cgroup (killed with it).
fn run_step(
    worktree: &Path,
    step: &str,
    target: &Path,
    ctl: &GateControl,
    place: &Placement,
) -> (StepEnd, StepMemory, usize) {
    use std::os::unix::process::CommandExt;
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
    let child = cmd
        .current_dir(worktree)
        .env(GATE_TARGET_DIR, target)
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
                let text = read_tail(&log);
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
            break StepEnd::Deadline(read_tail(&log));
        }
        if ctl.stop.load(Ordering::Relaxed) {
            kill(&mut child);
            break StepEnd::Stopped(read_tail(&log), "the daemon is stopping".into());
        }
        if last_beat.elapsed() >= Duration::from_secs(5) {
            last_beat = Instant::now();
            if !(ctl.alive)() {
                kill(&mut child);
                break StepEnd::Stopped(
                    read_tail(&log),
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

fn tempfile_in(worktree: &Path) -> Result<PathBuf, String> {
    let dir = std::env::temp_dir().join("letibot-gate");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    // A sequence number beside the millisecond: every landing worktree was once called
    // `letibot-landing`, so two repositories' gates starting in one millisecond shared a log and
    // one read the other's truncation (measured in the R76 tests: a failed step with no output).
    // The name is `<repo>-<hash>` now, and the sequence still guards one repository's two gates.
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

fn read_tail(log: &Path) -> String {
    let bytes = std::fs::read(log).unwrap_or_default();
    tail(&String::from_utf8_lossy(&bytes), EVIDENCE_BYTES)
}

/// The end of `text`, at most `cap` bytes, saying how much was dropped from the front.
pub fn tail(text: &str, cap: usize) -> String {
    if text.len() <= cap {
        return text.to_string();
    }
    let mut start = text.len() - cap;
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    format!(
        "… [{start} byte(s) dropped from the front]\n{}",
        &text[start..]
    )
}

/// `<worktree>/target`, absolute.
pub fn gate_target_dir(worktree: &Path) -> PathBuf {
    std::path::absolute(worktree)
        .unwrap_or_else(|_| worktree.to_path_buf())
        .join("target")
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

    /// **Every step's row ends with what its memory did** (R76 item 6). A test binary is not
    /// started in a delegated scope of its own, so this one takes the old-style path: the step
    /// runs as before, and the row says *uncapped* and why rather than saying nothing. (Run
    /// inside such a scope, it is the step's own peak and cap — `tests/gate_memory.rs`
    /// measures that half.)
    #[test]
    fn a_steps_row_says_what_its_memory_did_and_uncapped_when_there_is_no_tree() {
        let wt = std::env::temp_dir().join(format!("letibot-gate-unit-{}", std::process::id()));
        std::fs::create_dir_all(&wt).unwrap();
        let stop = AtomicBool::new(false);
        let alive = || true;
        let ctl = GateControl {
            deadline: Instant::now() + Duration::from_secs(60),
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
        assert!(fine.starts_with("[gate memory: peak 64 MiB of a 64 MiB cap]"));
        assert!(fine.contains("2 process(es) the step left running"));
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
            "[gate memory: peak 24.1 GiB of a 24.9 GiB cap]\n[step: killed by signal 9 \
             (SIGKILL)]\n[128 process(es) the step left running were killed with its cgroup]"
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
}
