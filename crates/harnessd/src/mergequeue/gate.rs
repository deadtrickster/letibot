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
//! **The runner is an interface** ([`GateRunner`]) with one implementation here
//! ([`LocalGate`]). R62 runs the gate on the fleet (lubuntu1/2 over `ssh -i`); that is a second
//! implementation of the same trait, and nothing else in the queue has to change for it.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use letibot_tokencore::store::{MergeGateOutcome, MergeGateStep};

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
        let target = gate_target_dir(worktree);
        let mut rows: Vec<MergeGateStep> = Vec::with_capacity(steps.len());
        for (i, step) in steps.iter().enumerate() {
            let started = now_ms();
            let outcome = run_step(worktree, step, &target, ctl);
            let elapsed = now_ms().saturating_sub(started);
            let (word, output, verdict) = match outcome {
                StepEnd::Ok(out) => (MergeGateOutcome::Passed, out, None),
                StepEnd::Red(out) => (
                    MergeGateOutcome::Failed,
                    out.clone(),
                    Some(GateVerdict::Failed(format!("`{step}` failed:\n{out}"))),
                ),
                StepEnd::Deadline(out) => (
                    MergeGateOutcome::TimedOut,
                    out,
                    Some(GateVerdict::TimedOut(format!(
                        "`{step}` was still running at the gate's deadline and was killed with \
                         its whole process group"
                    ))),
                ),
                StepEnd::Stopped(out, why) => (
                    MergeGateOutcome::Killed,
                    out,
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
}

enum StepEnd {
    Ok(String),
    Red(String),
    Deadline(String),
    Stopped(String, String),
    Infra(String),
}

fn run_step(worktree: &Path, step: &str, target: &Path, ctl: &GateControl) -> StepEnd {
    use std::os::unix::process::CommandExt;
    let log = match tempfile_in(worktree) {
        Ok(l) => l,
        Err(e) => return StepEnd::Infra(e),
    };
    let (out, err) = match (
        std::fs::File::create(&log),
        std::fs::OpenOptions::new().append(true).open(&log),
    ) {
        (Ok(o), Ok(e)) => (o, e),
        (Err(e), _) | (_, Err(e)) => return StepEnd::Infra(format!("{}: {e}", log.display())),
    };
    let child = Command::new("sh")
        .arg("-c")
        .arg(step)
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
            return StepEnd::Infra(format!("sh -c {step:?}: {e}"));
        }
    };
    let pgid = child.id() as i32;
    let mut last_beat = Instant::now();
    let end = loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let text = read_tail(&log);
                break if status.success() {
                    StepEnd::Ok(text)
                } else {
                    StepEnd::Red(text)
                };
            }
            Ok(None) => {}
            Err(e) => {
                kill_group(pgid, &mut child);
                break StepEnd::Infra(format!("waiting on `{step}`: {e}"));
            }
        }
        if Instant::now() >= ctl.deadline {
            kill_group(pgid, &mut child);
            break StepEnd::Deadline(read_tail(&log));
        }
        if ctl.stop.load(Ordering::Relaxed) {
            kill_group(pgid, &mut child);
            break StepEnd::Stopped(read_tail(&log), "the daemon is stopping".into());
        }
        if last_beat.elapsed() >= Duration::from_secs(5) {
            last_beat = Instant::now();
            if !(ctl.alive)() {
                kill_group(pgid, &mut child);
                break StepEnd::Stopped(
                    read_tail(&log),
                    "this daemon lost the repository's lease, so it may not land".into(),
                );
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let _ = std::fs::remove_file(&log);
    end
}

/// `SIGTERM` to the whole group, five seconds, then `SIGKILL`, and the child reaped.
fn kill_group(pgid: i32, child: &mut std::process::Child) {
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
    let _ = child.wait();
}

fn tempfile_in(worktree: &Path) -> Result<PathBuf, String> {
    let dir = std::env::temp_dir().join("letibot-gate");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let name = format!(
        "{}-{}-{}.log",
        std::process::id(),
        now_ms(),
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
