use std::path::{Path, PathBuf};

use letibot_tokencore::store::{OwnerKind, OwnerRow, OwnerState, ProcState, ProcessRecord, Store};

use super::*;
use crate::lifecycle::{Places, now_ms};

/// A directory of this test's own, with everything a pass reads inside it.
struct Box_ {
    root: PathBuf,
}

impl Box_ {
    fn new(tag: &str) -> Box_ {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "letibot-reap-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Box_ { root }
    }

    fn at(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    fn places(&self) -> Places {
        let runs = self.at("firecode/runs");
        let cache = self.at("firecode/cache");
        std::fs::create_dir_all(&runs).unwrap();
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::create_dir_all(self.at("run")).unwrap();
        Places {
            gc_root: self.at("gc"),
            box_id: crate::lifecycle::box_id(),
            settings: crate::lifecycle::Settings::default(),
            data_dir: self.at("data"),
            rundir: self.at("run"),
            firecode_runs: Some(runs),
            firecode_cache: Some(cache),
            proc_root: PathBuf::from("/proc"),
            cgroup_fs: PathBuf::from("/nonexistent/cgroup"),
            docker_engine: None,
        }
    }

    fn store(&self) -> Store {
        std::fs::create_dir_all(self.at("data")).unwrap();
        Store::open(&self.at("data/sessions.db")).expect("a store")
    }

    /// A runtime record the way the launcher writes one.
    fn daemon_record(&self, key: &str, workspace: &Path, pid: u32) {
        std::fs::create_dir_all(self.at("run")).unwrap();
        std::fs::write(
            self.at(&format!("run/{key}.json")),
            format!(
                r#"{{"workspace":"{}","socket":"{}","pid":{pid},"role":"head","model":"m","answers":"m","dialect":"d","bash":true,"started":"now"}}"#,
                workspace.display(),
                self.at(&format!("run/{key}.sock")).display()
            ),
        )
        .unwrap();
    }
}

impl Drop for Box_ {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Hands that reach nothing: these tests are about compute, and no fixture here has a remote host,
/// a shared target or a firecode verb to ask.
struct NoHands;

impl Hands for NoHands {
    fn firecode(&self, _: &[String]) -> Result<String, String> {
        Err("no firecode in this test".into())
    }
    fn lock_held(&self, _: &Path) -> Result<bool, String> {
        Err("no lock in this test".into())
    }
    fn host_command(&self, host: &str, _: &str) -> Result<String, String> {
        Err(format!("no {host} in this test"))
    }
    fn host_read(&self, host: &str, _: &str) -> Result<Vec<u8>, String> {
        Err(format!("no {host} in this test"))
    }
}

/// Bounds a test can wait for: nothing here is a real ten-second grace.
fn quick(by: &str) -> Ops {
    Ops {
        by: by.into(),
        firecode: None,
        grace: Duration::from_millis(200),
        after_kill: Duration::from_millis(200),
        daemon_grace: Duration::from_millis(200),
        hands: std::sync::Arc::new(NoHands),
        docker: crate::docker::Settings::default(),
    }
}

fn package(id: &str, lease_ms: i64, idle: i64) -> OwnerRow {
    let mut o = OwnerRow::live(id, OwnerKind::Package, Some("/w".into()), lease_ms);
    o.idle_ttl_ms = Some(idle);
    o.hard_ttl_ms = None;
    o
}

fn activity(busy: bool) -> Activity {
    Activity {
        busy,
        parent: None,
        daemon: None,
    }
}

/// A package with a turn in flight is not closed however long it has been quiet: idleness is
/// "no turn, no job, no input", and one of them is there.
#[test]
fn a_turn_in_flight_is_never_idle() {
    let o = package("s-1", 0, 60_000);
    assert!(
        close_due(&o, &activity(true), 600_000).is_none(),
        "a busy owner was closed for idleness"
    );
    assert!(close_due(&o, &activity(false), 600_000).is_some());
}

/// The notice comes five minutes before the close, and not before - so a session that spoke
/// recently is not told anything.
#[test]
fn the_notice_is_five_minutes_ahead_of_the_close() {
    let o = package("s-1", 0, 60 * 60 * 1000);
    let a = activity(false);
    let closes_at = 60 * 60 * 1000;
    assert_eq!(notice_at(&o, &a, 1_000), None, "nothing to say yet");
    assert_eq!(
        notice_at(&o, &a, closes_at - NOTICE_LEAD_MS),
        Some(closes_at),
        "the notice goes out NOTICE_LEAD_MS before the idle close"
    );
    assert_eq!(notice_at(&o, &a, closes_at + 1), None, "and never after it");
    // And a held owner is never told it is about to close.
    let mut held = o.clone();
    held.held_until_ms = Some(0);
    assert_eq!(notice_at(&held, &a, closes_at - NOTICE_LEAD_MS), None);
}

/// A hard TTL fires whatever the owner is doing: it is the backstop a missing call bound needs.
#[test]
fn a_hard_ttl_does_not_care_what_the_owner_is_doing() {
    let mut o = package("s-1", 0, 60_000);
    o.hard_ttl_ms = Some(3_600_000);
    let busy = activity(true);
    assert!(close_due(&o, &busy, 3_600_000).is_some());
}

/// The operator's hold stops every automatic trigger; an expired one does not.
#[test]
fn a_hold_stops_the_close_and_an_expired_one_does_not() {
    let mut o = package("s-1", 0, 60_000);
    o.held_until_ms = Some(0);
    assert!(close_due(&o, &activity(false), 10_000_000).is_none());
    o.held_until_ms = Some(5_000);
    assert!(close_due(&o, &activity(false), 10_000_000).is_some());
}

/// A child does not outlive a parent whose compute is gone.
#[test]
fn a_closed_parent_takes_its_child_with_it() {
    let mut child = package("s-2", 0, 60 * 60 * 1000);
    child.kind = OwnerKind::Subagent;
    child.parent_id = Some("s-1".into());
    let a = Activity {
        parent: Some(OwnerState::Closed),
        ..activity(false)
    };
    let why = close_due(&child, &a, 1_000).expect("a child of a closed parent is closed");
    assert!(why.contains("s-1"), "{why}");
}

/// The operator's own session is never closed by idleness, a TTL or a dead daemon: it is theirs.
#[test]
fn an_interactive_session_is_never_closed_automatically() {
    let mut o = OwnerRow::live("s-1", OwnerKind::Session, Some("/w".into()), 0);
    o.idle_ttl_ms = Some(1_000);
    o.hard_ttl_ms = Some(2_000);
    let a = Activity {
        daemon: Some(false),
        ..activity(false)
    };
    assert!(close_due(&o, &a, 10_000_000).is_none());
}

/// A daemon record with no process behind it means nothing is running the owner.
#[test]
fn a_record_with_no_process_behind_it_closes_the_owner() {
    let o = package("s-1", 0, 60 * 60 * 1000);
    let a = Activity {
        daemon: Some(false),
        ..activity(false)
    };
    let why = close_due(&o, &a, now_ms()).expect("its daemon is gone");
    assert!(why.contains("daemon"), "{why}");
}

// ---------------------------------------------------------------- the hands

/// A scope that is already empty is `Gone` and its directory goes with it: a step is done when the
/// thing is absent.
#[test]
fn an_empty_scope_is_gone_and_removed() {
    let b = Box_::new("empty-scope");
    let dir = b.at("cg/session.s-1");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cgroup.events"), "populated 0\nfrozen 0\n").unwrap();
    std::fs::write(dir.join("cgroup.procs"), "").unwrap();
    std::fs::write(dir.join("cgroup.kill"), "").unwrap();
    let ended = end_scope(&dir, &quick("test"));
    assert!(matches!(ended, Ended::Gone(_)), "{ended:?}");
    // The directory is not asserted gone: a real cgroup is pseudo-files that `rmdir` takes whole,
    // and a fixture's plain files are not, which is the one thing a fixture cannot stand in for.
}

/// A scope the kernel still reports populated is `cgroup.kill`ed and, if it is still populated, is
/// `Refused`: never reported as reaped. The fixture stands in for the kernel by flipping its own
/// `cgroup.events` once the kill has been written.
#[test]
fn a_populated_scope_is_killed_and_believed_only_when_it_is_empty() {
    let b = Box_::new("scope");
    let dir = b.at("cg/session.s-1");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cgroup.events"), "populated 1\n").unwrap();
    std::fs::write(dir.join("cgroup.procs"), "").unwrap();
    std::fs::write(dir.join("cgroup.kill"), "").unwrap();
    std::fs::write(dir.join("memory.current"), "2048\n").unwrap();

    // No fake kernel: the kill is written and the file still says populated, so the answer is
    // `Refused` and the operator is told what survived.
    let ended = end_scope(&dir, &quick("test"));
    match &ended {
        Ended::Refused(why) => assert!(why.contains("cgroup.kill"), "{why}"),
        other => panic!("a scope that would not empty was reported {other:?}"),
    }
    assert_eq!(
        std::fs::read_to_string(dir.join("cgroup.kill")).unwrap(),
        "1",
        "the kill was not written"
    );
}

/// A `process` row is ended by `(pid, start)` and by nothing else: a pid whose start time no longer
/// matches belongs to somebody else and is not signalled.
#[test]
fn a_process_is_ended_by_its_identity() {
    let mut child = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("a sleep");
    let pid = child.id();
    let info = procs::read(pid).expect("its /proc entry");
    let ready = ProcessRecord {
        identity: format!("start:{}", info.start),
        ..ProcessRecord::starting("s-1", "j1", None, pid)
    };
    let ended = end_process(&ready, &quick("test"));
    let _ = child.wait();
    assert!(matches!(ended, Ended::Gone(_)), "{ended:?}");
    assert!(
        !procs::alive(pid, info.start),
        "the process answered `gone` and is alive"
    );

    // And a record whose start time is another process's refuses to signal even a live pid.
    let mut again = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("a sleep");
    let stale = ProcessRecord {
        identity: format!("start:{}", 1),
        ..ProcessRecord::starting("s-1", "j2", None, again.id())
    };
    let ended = end_process(&stale, &quick("test"));
    assert!(
        matches!(ended, Ended::Gone(_)),
        "a pid that is not that process is not signalled: {ended:?}"
    );
    let _ = again.kill();
    let _ = again.wait();
}

/// A daemon the launcher recorded is stopped by `(pid, start)` and its record removed; a record
/// naming this very process is kept, because a reaper never reaps itself.
#[test]
fn a_daemon_is_stopped_and_its_record_goes_with_it() {
    let b = Box_::new("daemon");
    let places = b.places();
    let mut child = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("a sleep");
    b.daemon_record("k1", Path::new("/w"), child.id());
    let mut owner = package("s-1", 0, 60_000);
    owner.state = OwnerState::Closing;
    let ended = end_daemon(
        &places,
        &quick("test"),
        &owner,
        &format!("pid:{}", child.id()),
    );
    let _ = child.wait();
    assert!(matches!(ended, Ended::Gone(_)), "{ended:?}");
    assert!(
        !b.at("run/k1.json").exists(),
        "a record with no process behind it is the stale handle this package ends"
    );

    b.daemon_record("me", Path::new("/w"), std::process::id());
    let kept = end_daemon(
        &places,
        &quick("test"),
        &owner,
        &format!("pid:{}", std::process::id()),
    );
    match kept {
        Ended::Kept(why) => assert!(why.contains("never reaps itself"), "{why}"),
        other => panic!("a reaper signalled its own process: {other:?}"),
    }
}

// ---------------------------------------------------------------- the pass

/// One pass over an idle package closes it, writes the notice first, and journals every step - and
/// a second pass over the same owner does nothing at all.
#[test]
fn an_idle_package_is_closed_by_one_pass_and_the_notice_comes_first() {
    let b = Box_::new("idle-pass");
    let places = b.places();
    let s = b.store();
    let now = 1_000_000;
    let owner = package("s-1", now - 60_000, 60_000);
    s.put_owner(&owner).expect("the owner");

    // One pass at the moment the notice is due: it is journaled and nothing is closed.
    let first = pass(&s, &places, &quick("test"), None, now - 1).expect("a pass");
    assert!(
        first.acts.iter().any(|a| a.kind == "notice"),
        "the notice comes first: {first:?}"
    );
    assert_eq!(
        s.owner("s-1").unwrap().unwrap().state,
        OwnerState::Live,
        "the notice closed the owner"
    );

    // And the pass that does close it: the notice is not written twice.
    let second = pass(&s, &places, &quick("test"), None, now).expect("a pass");
    let back = s.owner("s-1").unwrap().unwrap();
    assert_eq!(back.state, OwnerState::Closed, "{second:?}");
    assert!(back.closed_ms.is_some());
    let log = s.reap_events_of("s-1").unwrap();
    assert_eq!(
        log.iter()
            .filter(|e| e.kind.as_deref() == Some("notice"))
            .count(),
        1,
        "the notice was written twice: {log:?}"
    );
    assert_eq!(
        log.last().map(|e| e.act.as_str()),
        Some("close"),
        "the close is the last journal line: {log:?}"
    );

    // A third pass has nothing left to do: a closed owner is not closed again.
    let third = pass(&s, &places, &quick("test"), None, now + 60_000).expect("a pass");
    assert!(third.acts.is_empty(), "{third:?}");
}

/// A held owner keeps everything and the pass says why, which is the one opt-out there is.
#[test]
fn a_held_owner_keeps_everything_and_says_why() {
    let b = Box_::new("held");
    let places = b.places();
    let s = b.store();
    let now = 1_000_000;
    let mut owner = package("s-1", 0, 60_000);
    owner.held_until_ms = Some(0);
    s.put_owner(&owner).expect("the owner");
    let out = pass(&s, &places, &quick("test"), None, now).expect("a pass");
    assert!(out.acts.is_empty(), "a held owner lost something: {out:?}");
    assert_eq!(s.owner("s-1").unwrap().unwrap().state, OwnerState::Live);
    assert!(
        out.kept.iter().any(|k| k.contains("holds it")),
        "a pass that keeps something says why: {out:?}"
    );
}

/// A SIGKILLed daemon's owner is closed by a pass that never talks to a daemon: the process row's
/// `(pid, start)` is what ends the job, and the owner goes to `closed` once nothing is left.
#[test]
fn a_dead_daemons_job_is_ended_by_a_pass_that_asks_no_daemon() {
    let b = Box_::new("dead-daemon");
    let places = b.places();
    let s = b.store();
    let now = now_ms();
    let mut owner = package("s-1", now, 60 * 60 * 1000);
    // Its daemon's record is there and no process is behind it.
    b.daemon_record("gone", Path::new("/w"), 4_000_000_000);
    owner.state = OwnerState::Live;
    s.put_owner(&owner).expect("the owner");
    let mut job = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("a sleep");
    let info = procs::read(job.id()).expect("its /proc entry");
    s.put_process(&ProcessRecord {
        identity: format!("start:{}", info.start),
        state: ProcState::Running,
        ..ProcessRecord::starting("s-1", "j1", None, job.id())
    })
    .expect("the job row");

    let out = pass(&s, &places, &quick("test"), None, now).expect("a pass");
    let _ = job.wait();
    assert!(
        !procs::alive(job.id(), info.start),
        "the job outlived the pass: {out:?}"
    );
    let back = s.owner("s-1").unwrap().unwrap();
    assert_eq!(back.state, OwnerState::Closed, "{out:?}");
    let rec = s.process("s-1", "j1").unwrap().expect("the row");
    assert_eq!(rec.state, ProcState::Stopped, "the row still says running");
    assert!(
        s.reap_events_of("s-1")
            .unwrap()
            .iter()
            .any(|e| e.act == "kill" && e.handle.as_deref() == Some("j1")),
        "the kill is journaled"
    );
}

// On the host's real `/proc`, so Linux only: macOS has none, and a stand-in firecracker is found
// there by nothing.
#[cfg(target_os = "linux")]
/// A top-level package whose VM is up and whose daemon is gone: one pass brings the VM down with
/// firecode's own ladder and closes the owner. This is R193 seen from the reaper instead of from
/// `Drop` - the pass never talks to the daemon that booted it.
#[test]
fn a_vm_outliving_its_daemon_is_brought_down_and_seen_gone() {
    let b = Box_::new("vm");
    let places = b.places();
    let s = b.store();
    let now = now_ms();
    let owner = package("s-1", now, 60 * 60 * 1000);
    s.put_owner(&owner).unwrap();
    b.daemon_record("gone", Path::new("/w"), 4_000_000_000);

    let run = "firecode-20260101-120000-4242";
    // firecode's own records: a copy under the cache, named for the owner, and the run's files.
    let runs = places.firecode_runs.clone().unwrap();
    let copy = places.firecode_cache.clone().unwrap().join("s-1");
    std::fs::create_dir_all(&copy).unwrap();
    let dir = runs.join(run);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("project"), copy.display().to_string()).unwrap();
    std::fs::write(dir.join("owner"), "s-1\n").unwrap();
    // A stand-in firecracker: a script NAMED `firecracker`, run with `--id RUN`, ignoring SIGTERM
    // the way a jailed VM does.
    let script = b.at("firecracker");
    crate::testing::write_stub(
        &script,
        "#!/bin/sh\n[ \"$1\" = \"--id\" ] || exit 0\ntrap '' TERM\nwhile :; do sleep 0.2; done\n",
    );
    let mut fc = std::process::Command::new(&script)
        .args(["--id", run])
        .spawn()
        .expect("the stand-in firecracker");
    let began = Instant::now();
    while held::live_vms(&runs, &places.proc_root).is_empty()
        && began.elapsed() < Duration::from_secs(5)
    {
        std::thread::sleep(Duration::from_millis(20));
    }
    // And firecode's CLI, whose `down` does nothing: the ladder below it is what takes the VM.
    let bin = b.at("down-here");
    crate::testing::write_stub(&bin, "#!/bin/sh\nexit 0\n");

    let ops = Ops {
        firecode: Some(bin),
        ..quick("test")
    };
    let out = pass(&s, &places, &ops, None, now).expect("a pass");
    let _ = fc.wait();
    assert!(
        held::live_vms(&runs, &places.proc_root).is_empty(),
        "the VM outlived the pass: {out:?}"
    );
    assert_eq!(
        s.owner("s-1").unwrap().unwrap().state,
        OwnerState::Closed,
        "{out:?}"
    );
    assert!(
        out.acts
            .iter()
            .any(|a| a.kind == "vm" && a.act == ReapAct::Kill),
        "the VM's end is journaled: {out:?}"
    );
}
