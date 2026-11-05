#![cfg(target_os = "linux")]

use super::*;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "letibot-gate-orphans-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A step stand-in in a group of its own, with a grandchild in the same group.
fn step(script: &str) -> Child {
    Command::new("sh")
        .args(["-c", script])
        .stdin(Stdio::null())
        .process_group(0)
        .spawn()
        .unwrap()
}

/// A (pid, start) pair for a process that has exited.
fn dead_owner() -> (u32, u64) {
    let mut c = Command::new("sleep").arg("60").spawn().unwrap();
    let start = procs::read(c.id()).unwrap().start;
    c.kill().unwrap();
    c.wait().unwrap();
    (c.id(), start)
}

fn record(dir: &Path, owner: (u32, u64), pgid: u32, leader_start: u64) {
    let rec = StepRecord {
        owner_pid: owner.0,
        owner_start: owner.1,
        pgid,
        leader_start,
        worktree: dir.to_path_buf(),
        step: "cargo test".into(),
        cgroup: None,
    };
    std::fs::write(
        dir.join(format!("{pgid}-{leader_start}.json")),
        serde_json::to_vec(&rec).unwrap(),
    )
    .unwrap();
}

fn records(dir: &Path) -> usize {
    std::fs::read_dir(dir).unwrap().count()
}

fn wait_gone(pgid: u32) -> bool {
    let until = Instant::now() + Duration::from_secs(5);
    while Instant::now() < until {
        if in_group(pgid).is_empty() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

#[test]
fn a_live_daemons_step_is_left_alone() {
    let d = tmp("owned");
    let mut c = step("sleep 60");
    let w = write(&d, c.id(), &d, "cargo test", None);
    assert_eq!(
        reap(&d, &d, Duration::from_millis(200)),
        vec![Reaped::Owned]
    );
    assert!(procs::alive(c.id(), procs::read(c.id()).unwrap().start));
    assert_eq!(records(&d), 1);
    drop(w);
    assert_eq!(records(&d), 0, "the record goes with the step");
    c.kill().unwrap();
    c.wait().unwrap();
}

#[test]
fn a_dead_daemons_step_is_killed_with_its_whole_group() {
    let d = tmp("killed");
    let mut c = step("sleep 60 & sleep 60 & wait");
    let pgid = c.id();
    let start = procs::read(pgid).unwrap().start;
    while in_group(pgid).len() < 3 {
        std::thread::sleep(Duration::from_millis(20));
    }
    record(&d, dead_owner(), pgid, start);
    let r = reap(&d, &d, Duration::from_secs(2));
    assert!(
        matches!(r.as_slice(), [Reaped::Killed(rec)] if rec.pgid == pgid),
        "{r:?}"
    );
    c.wait().unwrap();
    assert!(wait_gone(pgid), "left in the group: {:?}", in_group(pgid));
    assert_eq!(records(&d), 0);
}

#[test]
fn a_step_that_ignores_sigterm_is_killed_after_the_patience() {
    let d = tmp("stubborn");
    let mut c = step("trap '' TERM; while :; do sleep 0.1; done");
    let pgid = c.id();
    let start = procs::read(pgid).unwrap().start;
    std::thread::sleep(Duration::from_millis(100));
    record(&d, dead_owner(), pgid, start);
    let r = reap(&d, &d, Duration::from_millis(300));
    assert!(matches!(r.as_slice(), [Reaped::Killed(_)]), "{r:?}");
    c.wait().unwrap();
    assert!(wait_gone(pgid));
}

#[test]
fn a_reused_leader_pid_is_not_signalled() {
    let d = tmp("reused");
    let mut c = step("sleep 60");
    let pgid = c.id();
    let start = procs::read(pgid).unwrap().start;
    record(&d, dead_owner(), pgid, start + 1);
    assert_eq!(
        reap(&d, &d, Duration::from_millis(200)),
        vec![Reaped::Reused]
    );
    assert!(
        procs::alive(pgid, start),
        "another process's group was signalled"
    );
    assert_eq!(records(&d), 0);
    c.kill().unwrap();
    c.wait().unwrap();
}

#[test]
fn a_step_that_already_ended_only_loses_its_record() {
    let d = tmp("gone");
    let mut c = step("true");
    let pgid = c.id();
    let start = procs::read(pgid).map(|p| p.start).unwrap_or(0);
    c.wait().unwrap();
    record(&d, dead_owner(), pgid, start);
    assert_eq!(reap(&d, &d, Duration::from_millis(200)), vec![Reaped::Gone]);
    assert_eq!(records(&d), 0);
}

#[test]
fn a_step_outside_the_landing_is_not_touched() {
    let d = tmp("elsewhere");
    let mut c = step("sleep 60");
    let pgid = c.id();
    let start = procs::read(pgid).unwrap().start;
    record(&d, dead_owner(), pgid, start);
    assert!(
        reap(
            &d,
            Path::new("/nonexistent-landing"),
            Duration::from_millis(200)
        )
        .is_empty()
    );
    assert!(procs::alive(pgid, start));
    assert_eq!(records(&d), 1);
    c.kill().unwrap();
    c.wait().unwrap();
}
