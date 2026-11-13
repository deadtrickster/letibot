//! A restart keeps a host's background jobs and the next host takes them back on proof alone:
//! the cgroup, the stamp and the wrapper's status file. Each test runs real processes in a real
//! cgroup; a fake tree would prove a rule about a fake.
#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use letibot_tools::exec::detach::{self, Detached, Proof, Reattached};
use letibot_tools::exec::{HostProcesses, JobId, JobState, ProcessHost, ScopeKind, SpawnRequest};

/// A scratch directory of the test's own, removed at the end.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let p = std::env::temp_dir().join(format!(
            "letibot-keep-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        Scratch(p)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A host with a real cgroup tree writing its jobs' files into `scratch`, or `None` said out loud.
fn host(scratch: &Path) -> Option<HostProcesses> {
    match HostProcesses::new(std::env::temp_dir()) {
        Ok(h) => {
            h.set_scratch_dir(scratch);
            Some(h)
        }
        Err(e) => {
            assert!(format!("{e}").contains("cgroup"), "{e}");
            letibot_tokencore::apparatus::absent("a cgroup v2 subtree this user may write");
            None
        }
    }
}

fn spawn(host: &HostProcesses, command: &str, scope: ScopeKind, background: bool) -> JobId {
    host.spawn(&SpawnRequest {
        command: command.to_string(),
        slug: None,
        cwd: ".".into(),
        scope,
        scope_name: None,
        background,
        env: vec![],
        tty: false,
        memory: None,
        env_trailer: false,
    })
    .expect("the run starts")
}

/// Bounded wait for a decision taken elsewhere. The bound is generous: the box may be loaded.
fn until(limit: Duration, mut f: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    f()
}

const BOUND: Duration = Duration::from_secs(30);

fn text(host: &HostProcesses, id: &JobId) -> String {
    host.output(id, 0, 1 << 20)
        .map(|s| s.text())
        .unwrap_or_default()
}

/// A job that counts, one line every 50 ms, so a gap or a repeat in its output is visible.
const COUNTER: &str = "i=0; while :; do echo tick $i; i=$((i+1)); sleep 0.05; done";

/// Every `tick N` in `text`, in order.
fn ticks(text: &str) -> Vec<u64> {
    text.lines()
        .filter_map(|l| l.strip_prefix("tick ")?.trim().parse().ok())
        .collect()
}

/// End a kept job nobody holds any more, by its cgroup.
fn kill_cgroup(d: &Detached) {
    let _ = std::fs::write(Path::new(&d.cgroup).join("cgroup.kill"), "1");
}

/// Start the counter, let it print, and detach it the way a restarting daemon does.
fn detached_counter(scratch: &Path) -> Option<Detached> {
    let h = host(scratch)?;
    let id = spawn(&h, COUNTER, ScopeKind::Session, true);
    assert!(
        until(BOUND, || ticks(&text(&h, &id)).len() >= 3),
        "{}",
        text(&h, &id)
    );
    let (kept, ended) = h.detach_for_restart("the daemon stopping");
    assert!(ended.is_empty(), "{ended:?}");
    assert_eq!(kept.len(), 1, "{kept:?}");
    let d = kept.into_iter().next().unwrap();
    assert_eq!(d.handle, id.0);
    drop(h);
    Some(d)
}

/// A kept job outlives its host: the process is alive after the host is gone, and the record
/// carries the cgroup and the stamp that prove it.
#[test]
fn a_kept_job_survives_the_restart_stop_with_its_cgroup_and_stamp() {
    let scratch = Scratch::new("survives");
    let Some(d) = detached_counter(&scratch.0) else {
        return;
    };
    assert!(d.identity.starts_with("start:"), "{d:?}");
    assert!(Path::new(&d.cgroup).join("cgroup.procs").exists(), "{d:?}");
    assert!(d.raw_read > 0, "nothing was read before the detach: {d:?}");
    // Still the same process, still in its cgroup, after the host that started it is gone.
    std::thread::sleep(Duration::from_millis(300));
    assert!(detach::stamp_matches(d.pid, &d.identity), "{d:?}");
    assert_eq!(detach::prove(&d), Proof::Running);
    kill_cgroup(&d);
}

/// A new host takes it back: running again, and its output goes on from where the record left
/// off, in the same file, with no tick lost or repeated.
#[test]
fn a_new_host_reattaches_a_kept_job_and_its_output_continues() {
    let scratch = Scratch::new("reattach");
    let Some(d) = detached_counter(&scratch.0) else {
        return;
    };
    let out = detach::out_path(Path::new(&d.raw));
    let before = ticks(&std::fs::read_to_string(&out).unwrap()).len();
    let Some(h) = host(&scratch.0) else { return };
    let (id, how) = h.reattach(&d).expect("a running job with a matching stamp");
    assert_eq!(how, Reattached::Running);
    assert!(h.job(&id).unwrap().state.is_running());
    assert!(
        until(BOUND, || ticks(&text(&h, &id)).len() >= 3),
        "the window did not move: {}",
        text(&h, &id)
    );
    h.kill_job(&id).expect("the kill");
    let all = ticks(&std::fs::read_to_string(&out).unwrap());
    assert!(all.len() > before + 2, "{all:?}");
    let expect: Vec<u64> = (all[0]..all[0] + all.len() as u64).collect();
    assert_eq!(
        all, expect,
        "a tick was lost or repeated across the restart"
    );
}

/// A job that ends while nothing watches leaves its code in the status file, and the next host
/// settles it with that code.
#[test]
fn a_kept_job_that_ended_while_detached_settles_with_its_exit_code() {
    let scratch = Scratch::new("ended");
    let Some(h) = host(&scratch.0) else { return };
    let id = spawn(
        &h,
        "sleep 0.5; echo finished; exit 3",
        ScopeKind::Session,
        true,
    );
    let (kept, _) = h.detach_for_restart("the daemon stopping");
    drop(h);
    let d = kept.into_iter().next().expect("kept");
    // The status is renamed in a moment before the wrapper exits: until then it is running.
    assert!(
        until(BOUND, || detach::prove(&d) == Proof::Ended(3)),
        "{:?}",
        detach::prove(&d)
    );
    let Some(h) = host(&scratch.0) else { return };
    let (got, how) = h.reattach(&d).expect("an ending the status file proves");
    assert_eq!(got, id);
    assert_eq!(how, Reattached::Ended(JobState::Exited { code: 3 }));
    assert_eq!(h.job(&id).unwrap().state, JobState::Exited { code: 3 });
    assert!(text(&h, &id).contains("finished"), "{}", text(&h, &id));
}

/// A plain stop ends a background job as it always did, and a restart ends what it does not
/// keep: a job owned by its turn.
#[test]
fn a_job_a_stop_does_not_keep_is_killed() {
    let scratch = Scratch::new("killed");
    let Some(h) = host(&scratch.0) else { return };
    let plain = spawn(&h, "sleep 300", ScopeKind::Session, true);
    let pid = h.job(&plain).unwrap().pid;
    let ended = h.end_running("the daemon stopping");
    assert_eq!(ended.len(), 1);
    assert!(
        matches!(h.job(&plain).unwrap().state, JobState::Killed { .. }),
        "{:?}",
        h.job(&plain).unwrap().state
    );
    assert!(until(BOUND, || !Path::new(&format!("/proc/{pid}")).exists()));

    let turn = spawn(&h, "sleep 300", ScopeKind::Turn, true);
    let (kept, ended) = h.detach_for_restart("the daemon stopping");
    assert!(kept.is_empty(), "{kept:?}");
    assert_eq!(ended.len(), 1);
    assert!(matches!(
        h.job(&turn).unwrap().state,
        JobState::Killed { .. }
    ));
}

/// A stamp that no longer matches the pid is not adopted, however the cgroup looks.
#[test]
fn a_stale_stamp_is_not_adopted() {
    let scratch = Scratch::new("stale");
    let Some(mut d) = detached_counter(&scratch.0) else {
        return;
    };
    let real = d.clone();
    d.identity = "start:1".into();
    assert!(
        matches!(detach::prove(&d), Proof::Nothing(ref why) if why.contains("not the process stamped")),
        "{:?}",
        detach::prove(&d)
    );
    let Some(h) = host(&scratch.0) else { return };
    let why = h.reattach(&d).expect_err("a stale stamp proves nothing");
    assert!(why.contains("not the process stamped"), "{why}");
    assert!(h.job(&JobId(d.handle.clone())).is_none(), "it was adopted");
    kill_cgroup(&real);
}
