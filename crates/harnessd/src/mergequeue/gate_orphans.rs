//! A gate step's record on disk, so a daemon that dies without stopping does not leave its gate
//! running. The stop, the deadline and a lost lease kill a step from inside the daemon; a SIGKILL
//! or a crash cannot, so the next daemon to take a queue kills what a dead owner left.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use letibot_tools::exec::procs;

/// One running step: its process group, and the daemon that started it. Both are (pid, start
/// time) pairs, because a pid alone may be reused by the time anybody reads the record.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StepRecord {
    pub owner_pid: u32,
    pub owner_start: u64,
    pub pgid: u32,
    pub leader_start: u64,
    pub worktree: PathBuf,
    pub step: String,
    /// The step's own cgroup, when it ran in one: `cgroup.kill` reaches what left the group.
    pub cgroup: Option<PathBuf>,
}

/// What [`reap`] did with one record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reaped {
    /// Its owner is alive: the step is that daemon's, and left alone.
    Owned,
    /// Its owner is gone and so is every process of the step: only the record went.
    Gone,
    /// Its owner is gone and the step was still running: killed.
    Killed(StepRecord),
    /// The leader's pid now belongs to another process: nothing was signalled.
    Reused,
}

/// The box-wide directory the records live in, beside the gate's logs.
pub fn dir() -> PathBuf {
    std::env::temp_dir().join("letibot-gate").join("steps")
}

/// Removes the record when the step ends, on every path out of `run_step`.
pub struct Written(Option<PathBuf>);

impl Drop for Written {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// Write the record of a step this daemon just spawned as group `pgid`. A record that cannot be
/// written is not a reason to refuse the gate; that step is then only killed from inside.
pub fn write(dir: &Path, pgid: u32, worktree: &Path, step: &str, cgroup: Option<&Path>) -> Written {
    let me = std::process::id();
    let (Some(owner), Some(leader)) = (procs::read(me), procs::read(pgid)) else {
        return Written(None);
    };
    let rec = StepRecord {
        owner_pid: me,
        owner_start: owner.start,
        pgid,
        leader_start: leader.start,
        worktree: worktree.to_path_buf(),
        step: step.to_string(),
        cgroup: cgroup.map(Path::to_path_buf),
    };
    let path = dir.join(format!("{pgid}-{}.json", leader.start));
    let written = std::fs::create_dir_all(dir).is_ok()
        && serde_json::to_vec(&rec)
            .ok()
            .is_some_and(|b| std::fs::write(&path, b).is_ok());
    Written(written.then_some(path))
}

/// Kill every recorded step under `within` whose owner is dead, and drop its record. `patience`
/// is how long a group gets after SIGTERM before SIGKILL.
pub fn reap(dir: &Path, within: &Path, patience: Duration) -> Vec<Reaped> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let path = e.path();
        let Some(rec) = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<StepRecord>(&b).ok())
        else {
            continue;
        };
        if !rec.worktree.starts_with(within) {
            continue;
        }
        let r = reap_one(&rec, patience);
        if r != Reaped::Owned {
            let _ = std::fs::remove_file(&path);
        }
        out.push(r);
    }
    out
}

/// [`reap`] over [`dir`] for one landing worktree, as the lines the daemon's log says: one per
/// step it killed.
pub fn reap_said(landing: &Path) -> Vec<String> {
    reap(&dir(), landing, Duration::from_secs(5))
        .into_iter()
        .filter_map(|r| match r {
            Reaped::Killed(rec) => Some(format!(
                "killed gate step `{}` in {} (group {}), left running by daemon pid {}, which is gone",
                rec.step,
                rec.worktree.display(),
                rec.pgid,
                rec.owner_pid
            )),
            _ => None,
        })
        .collect()
}

fn reap_one(rec: &StepRecord, patience: Duration) -> Reaped {
    if procs::alive(rec.owner_pid, rec.owner_start) {
        return Reaped::Owned;
    }
    // The kernel does not hand out a pid still in use as a process group id, so with the leader
    // gone, any process left in group `pgid` is the step's.
    let leader = procs::read(rec.pgid);
    match &leader {
        Some(p) if p.start != rec.leader_start => return Reaped::Reused,
        _ => {}
    }
    let group_left = || procs::alive(rec.pgid, rec.leader_start) || !in_group(rec.pgid).is_empty();
    let cgroup_left = rec.cgroup.as_ref().is_some_and(|c| c.exists());
    if !group_left() && !cgroup_left {
        return Reaped::Gone;
    }
    signal_group(rec.pgid, libc::SIGTERM);
    let until = Instant::now() + patience;
    while group_left() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(50));
    }
    signal_group(rec.pgid, libc::SIGKILL);
    if let Some(c) = &rec.cgroup {
        let _ = std::fs::write(c.join("cgroup.kill"), b"1");
        let _ = std::fs::remove_dir(c);
    }
    Reaped::Killed(rec.clone())
}

fn signal_group(pgid: u32, sig: i32) {
    // SAFETY: a group id read from a record whose leader is gone or verified by start time; a
    // group that no longer exists answers ESRCH.
    unsafe {
        libc::killpg(pgid as i32, sig);
    }
}

/// The pids whose process group is `pgid`, read from `/proc`.
fn in_group(pgid: u32) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| pgrp_of(*pid) == Some(pgid))
        .collect()
}

fn pgrp_of(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = &stat[stat.rfind(')')? + 1..];
    // After `)`: state ppid pgrp.
    let mut f = rest.split_whitespace();
    let state = f.next()?;
    if state == "Z" {
        return None;
    }
    f.next()?;
    f.next()?.parse().ok()
}

#[cfg(test)]
mod tests;
