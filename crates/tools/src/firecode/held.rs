//! **What holds each firecode VM slot, read off firecode's own records — and the reaper that
//! frees one whose owner is gone.**
//!
//! What this was written against, 2026-10-15: the merge queue's reviewer for
//! `feat/memory-engine` said *"VM slots exhausted · waited 33 min, every slot busy"* for hours,
//! and all four taps (`fccode1`–`fccode4`) were held by firecracker VMs whose parent was pid 1.
//! Three belonged to grandchildren of one session whose daemon (`d-404553`) had died without an
//! orderly stop on 2026-10-14 at 18:21 UTC — so `down_all` never ran — and whose rows in
//! `subagent_run` still said `running` a day later; the fourth was a pg-noop great-grandchild
//! from 2026-10-12. Each VM's launcher (`firecode exec …`, a session leader of its own) was alive
//! and holding it, and nothing on the host asked whose it was. The capacity notice could only say
//! *"every slot is busy"*.
//!
//! # The record is firecode's, and every build wrote it
//!
//! `firecode up` leaves `runs/<run>/` beside the CLI: `project` (the copy it booted — for a
//! letibot child, `~/.cache/letibot/firecode/<child id>`, see [`super::FirecodeBackend::project_of`]),
//! `name`, `jail` (the jailer's root, whose `vm-config.json` names the tap), `cgroup` (the run's
//! delegated cgroup) and `launcher`. A VM is live when a `firecracker` process carries
//! `--id <run>` — the fact itself, not the run directory's age or a pid file. Nothing here starts
//! firecode to ask: a listing is file reads, so the queue may make it on every long wait.
//!
//! # Reaping is firecode's `down`, then SIGKILL, then a look
//!
//! [`reap`] asks firecode to bring the run down by its id (the guest's idle loop ends, the work is
//! copied out beside the project the way any `down` delivers it). A jailed VM is pid 1 of its own
//! pid namespace and ignores SIGTERM — measured on the operator's box the same day: a plain
//! `kill` ended the nine unjailed orphans and none of the four jailed tap holders. So a VM still
//! there after the wait is killed with the run's own `cgroup.kill` (SIGKILL to everything in the
//! run's cgroup), then — if it is STILL there — with SIGKILL to the pid itself, after checking the
//! pid is still the firecracker for that run. And the answer is what `/proc` says afterwards,
//! never what was sent: a reap that sent SIGKILL and found the process alive says so.
//!
//! Whether a VM MAY be reaped is not decided here: that is the daemon's records' question
//! (`harnessd`'s `vm_reap`). This module only says what holds a slot and does the reaping.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

/// **One live firecode VM, as its run's records describe it.**
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldVm {
    /// The run id (`firecode-20261014-191759-812644`) — what `firecode down` takes.
    pub run: String,
    /// The project the VM was booted on (`runs/<run>/project`).
    pub project: PathBuf,
    /// `runs/<run>/name`, when firecode wrote one.
    pub name: Option<String>,
    /// The tap it holds (`fccode1`), from its jail's `vm-config.json`.
    pub tap: Option<String>,
    /// The `firecracker` processes carrying `--id <run>`, host pids.
    pub pids: Vec<u32>,
    /// When the run was started, as its id says it (`2026-10-14 19:17:59`, firecode's local time).
    pub started: Option<String>,
    /// The run's delegated cgroup (`runs/<run>/cgroup`).
    pub cgroup: Option<PathBuf>,
}

impl HeldVm {
    /// *"fccode1: firecracker pid 861706, run firecode-20261014-191759-812644 started
    /// 2026-10-14 19:17:59"* — the VM half of a capacity notice's line; the owner half is the
    /// daemon's.
    pub fn line(&self) -> String {
        let pids = match self.pids.as_slice() {
            [] => "no firecracker pid".to_string(),
            [p] => format!("firecracker pid {p}"),
            ps => format!(
                "firecracker pids {}",
                ps.iter()
                    .map(|p| p.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
        format!(
            "{}: {pids}, run {}{}",
            self.tap.as_deref().unwrap_or("no tap"),
            self.run,
            self.started
                .as_deref()
                .map(|s| format!(" started {s}"))
                .unwrap_or_default()
        )
    }
}

/// **firecode's `runs/` directory** for the CLI at `bin` — the checkout's `runs`, beside the
/// `bin/` the CLI is in, the way [`super::FirecodeBackend`] finds a run's record.
pub fn runs_dir(bin: &Path) -> Option<PathBuf> {
    let real = std::fs::canonicalize(bin).ok().or_else(|| {
        std::env::var_os("PATH").and_then(|p| {
            std::env::split_paths(&p)
                .map(|d| d.join(bin))
                .find(|c| c.is_file())
                .and_then(|c| std::fs::canonicalize(c).ok())
        })
    })?;
    let runs = real.parent()?.parent()?.join("runs");
    runs.is_dir().then_some(runs)
}

/// **When a run was started, read off its id** — `firecode-YYYYMMDD-HHMMSS-<pid>`.
pub fn started_of(run: &str) -> Option<String> {
    let mut parts = run.strip_prefix("firecode-")?.split('-');
    let d = parts.next()?;
    let t = parts.next()?;
    let digits = |s: &str, n: usize| s.len() == n && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(d, 8) || !digits(t, 6) {
        return None;
    }
    Some(format!(
        "{}-{}-{} {}:{}:{}",
        &d[..4],
        &d[4..6],
        &d[6..],
        &t[..2],
        &t[2..4],
        &t[4..]
    ))
}

/// The arguments of process `pid`, from `<proc>/<pid>/cmdline`.
fn cmdline(proc_root: &Path, pid: u32) -> Option<Vec<String>> {
    let raw = std::fs::read(proc_root.join(pid.to_string()).join("cmdline")).ok()?;
    if raw.is_empty() {
        return None;
    }
    Some(
        raw.split(|b| *b == 0)
            .filter(|a| !a.is_empty())
            .map(|a| String::from_utf8_lossy(a).into_owned())
            .collect(),
    )
}

/// **The run a process is the firecracker of**, if it is one: its program (or, for an
/// interpreted stand-in, its script) is named `firecracker` and it was given `--id <run>`.
pub fn firecracker_run(args: &[String]) -> Option<String> {
    let named = args
        .iter()
        .take(2)
        .any(|a| Path::new(a).file_name().is_some_and(|n| n == "firecracker"));
    if !named {
        return None;
    }
    let i = args.iter().position(|a| a == "--id")?;
    args.get(i + 1).filter(|r| !r.is_empty()).cloned()
}

/// **Whether `pid` is a process that can still act** — present in `<proc>` and not a zombie. A
/// zombie has exited; its entry stays until its parent reaps it, and that is not a VM holding a
/// tap.
pub fn alive(proc_root: &Path, pid: u32) -> bool {
    let Ok(stat) = std::fs::read_to_string(proc_root.join(pid.to_string()).join("stat")) else {
        return false;
    };
    // `pid (comm) S …` — the state is the first field after the LAST `)`, since comm may hold one.
    stat.rsplit_once(')')
        .and_then(|(_, rest)| rest.split_whitespace().next())
        .is_none_or(|state| state != "Z" && state != "X")
}

/// **Every firecracker process on the host, by the run it serves** — `(run, pid)`.
pub fn firecrackers(proc_root: &Path) -> Vec<(String, u32)> {
    let Ok(dir) = std::fs::read_dir(proc_root) else {
        return Vec::new();
    };
    let mut out: Vec<(String, u32)> = dir
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter_map(|pid| {
            let run = firecracker_run(&cmdline(proc_root, pid)?)?;
            alive(proc_root, pid).then_some((run, pid))
        })
        .collect();
    out.sort();
    out
}

/// The tap a run's VM was configured with: `network-interfaces[].host_dev_name` of its jail's
/// `vm-config.json`.
fn tap_of(run_dir: &Path) -> Option<String> {
    let jail = std::fs::read_to_string(run_dir.join("jail")).ok()?;
    let config = std::fs::read_to_string(Path::new(jail.trim()).join("vm-config.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&config).ok()?;
    v.get("network-interfaces")?
        .as_array()?
        .iter()
        .find_map(|n| n.get("host_dev_name")?.as_str().map(str::to_string))
}

fn read_trimmed(p: &Path) -> Option<String> {
    std::fs::read_to_string(p)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// **Every live VM firecode has a record of** — a run directory under `runs` whose firecracker
/// is alive in `proc_root` (`/proc`), oldest run first.
pub fn live_vms(runs: &Path, proc_root: &Path) -> Vec<HeldVm> {
    let mut by_run: std::collections::BTreeMap<String, Vec<u32>> = Default::default();
    for (run, pid) in firecrackers(proc_root) {
        by_run.entry(run).or_default().push(pid);
    }
    by_run
        .into_iter()
        .filter_map(|(run, pids)| {
            let dir = runs.join(&run);
            let project = read_trimmed(&dir.join("project"))?;
            Some(HeldVm {
                project: PathBuf::from(project),
                name: read_trimmed(&dir.join("name")),
                tap: tap_of(&dir),
                started: started_of(&run),
                cgroup: read_trimmed(&dir.join("cgroup")).map(PathBuf::from),
                pids,
                run,
            })
        })
        .collect()
}

/// **How long [`reap`] gives each step.**
#[derive(Debug, Clone, Copy)]
pub struct Patience {
    /// `firecode down` itself: it waits up to a minute for the guest, then on the copy-out.
    pub down: Duration,
    /// After `down`, for the firecracker to leave on its own.
    pub after_down: Duration,
    /// After each SIGKILL, for the process to be gone.
    pub after_kill: Duration,
}

impl Default for Patience {
    fn default() -> Self {
        Patience {
            down: Duration::from_secs(120),
            after_down: Duration::from_secs(20),
            after_kill: Duration::from_secs(5),
        }
    }
}

/// **How a reap ended — what `/proc` said afterwards, not what was sent.**
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reaped {
    /// `firecode down` brought it down: the guest stopped and its work was copied out.
    Down,
    /// It ignored `down` and was killed (SIGKILL): `how` names which kill took it.
    Killed { how: String },
    /// Still alive after every step — named, so nobody reads a reap that failed as one that held.
    StillAlive { pids: Vec<u32>, tried: String },
}

impl Reaped {
    pub fn gone(&self) -> bool {
        !matches!(self, Reaped::StillAlive { .. })
    }

    pub fn said(&self) -> String {
        match self {
            Reaped::Down => "brought down by `firecode down`".into(),
            Reaped::Killed { how } => format!("killed ({how}) after `firecode down` left it up"),
            Reaped::StillAlive { pids, tried } => format!(
                "STILL ALIVE (pid {}) after {tried}",
                pids.iter()
                    .map(|p| p.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

/// The pids of `vm` still alive and still the firecracker for its run.
fn still_there(vm: &HeldVm, proc_root: &Path) -> Vec<u32> {
    vm.pids
        .iter()
        .copied()
        .filter(|&p| {
            alive(proc_root, p)
                && cmdline(proc_root, p)
                    .and_then(|a| firecracker_run(&a))
                    .is_some_and(|r| r == vm.run)
        })
        .collect()
}

fn wait_gone(vm: &HeldVm, proc_root: &Path, within: Duration) -> Vec<u32> {
    let began = Instant::now();
    loop {
        let left = still_there(vm, proc_root);
        if left.is_empty() || began.elapsed() >= within {
            return left;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// `bin down <run>`, bounded by `within`: a `down` that does not return is not waited on for ever
/// (the client is ours to stop; the VM is what the steps after this are for).
fn down(bin: &Path, run: &str, within: Duration) {
    let Ok(mut child) = std::process::Command::new(bin)
        .arg("down")
        .arg(run)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return;
    };
    let began = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => return,
            Ok(None) if began.elapsed() >= within => {
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(200)),
        }
    }
}

/// **Bring one VM down, and say how it went.** `firecode down <run>`, then the run's
/// `cgroup.kill`, then SIGKILL to each pid that is still the firecracker for this run, with a
/// look at `/proc` after each. See the module docs for why SIGTERM is not among the steps.
pub fn reap(bin: &Path, vm: &HeldVm, proc_root: &Path, patience: Patience) -> Reaped {
    down(bin, &vm.run, patience.down);
    if wait_gone(vm, proc_root, patience.after_down).is_empty() {
        return Reaped::Down;
    }
    let mut tried = vec!["`firecode down`".to_string()];
    if let Some(cg) = &vm.cgroup {
        let kill = cg.join("cgroup.kill");
        if kill.exists() && std::fs::write(&kill, "1").is_ok() {
            tried.push(format!("SIGKILL through {}", kill.display()));
            if wait_gone(vm, proc_root, patience.after_kill).is_empty() {
                return Reaped::Killed {
                    how: format!("the run's cgroup.kill, {}", kill.display()),
                };
            }
        }
    }
    let left = still_there(vm, proc_root);
    for p in &left {
        let _ = std::process::Command::new("kill")
            .args(["-KILL", &p.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    if !left.is_empty() {
        tried.push(format!(
            "SIGKILL to pid {}",
            left.iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let left = wait_gone(vm, proc_root, patience.after_kill);
    if left.is_empty() {
        return Reaped::Killed {
            how: "SIGKILL to its pid".into(),
        };
    }
    Reaped::StillAlive {
        pids: left,
        tried: tried.join(", then "),
    }
}

#[cfg(test)]
mod tests;
