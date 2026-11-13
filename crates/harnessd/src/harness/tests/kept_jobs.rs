//! A job a restart kept is taken back by the daemon that opens its session next: on proof, with
//! its rows moved back and one line of the restart notice. The earlier daemon is a host this test
//! drops, and the job is a real process in a real cgroup.

use super::*;
use letibot_tokencore::store::{JobContext, JobRecord, ProcState, SessionRecord, Store};
use letibot_tools::exec::detach::Detached;
use letibot_tools::exec::{HostProcesses, ProcessHost, ScopeKind, SpawnRequest};

struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let p = letibot_sessionlog::testing::scratch_root().join(format!(
            "{tag}-{}-{}",
            std::process::id(),
            letibot_sessionlog::registry::now_ms()
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

const EARLIER: &str = "d-the-earlier-one";

/// The earlier daemon: start `command` in the background, let it print, and stop for a restart.
/// `None` on a box with no cgroup v2 subtree, said through `apparatus`.
fn kept_by_an_earlier_daemon(dir: &Scratch, command: &str, settle: Duration) -> Option<Detached> {
    letibot_tokencore::apparatus::present(
        "a process-lifetime tree (cgroup v2; process groups on macOS)",
        letibot_tools::host_tree().is_ok(),
    )?;
    let host = HostProcesses::new(&dir.0).expect("a host");
    host.set_scratch_dir(dir.0.join("scratch"));
    let id = host
        .spawn(&SpawnRequest {
            command: command.into(),
            slug: Some("fuzz".into()),
            cwd: ".".into(),
            scope: ScopeKind::Session,
            scope_name: None,
            background: true,
            env: vec![],
            tty: false,
            memory: None,
            env_trailer: false,
        })
        .expect("the job starts");
    std::thread::sleep(settle);
    let (kept, _) = host.detach_for_restart("the daemon stopping");
    drop(host);
    let d = kept.into_iter().next().expect("the job was kept");
    assert_eq!(d.handle, id.0);
    Some(d)
}

/// The store that daemon left: its stopped life, the session, and the kept job's rows.
fn left_behind(dir: &Scratch, sid: &str, d: &Detached) {
    let store = Store::open(&dir.0.join("sessions.db")).expect("a store");
    let now = letibot_sessionlog::event::now_ms();
    store.life_started(EARLIER, now - 3_600_000).unwrap();
    store.life_stopped(EARLIER, now - 60_000).unwrap();
    store
        .put_session(&SessionRecord {
            id: sid.into(),
            title: None,
            model_id: "a model".into(),
            dialect_sha: "a dialect".into(),
            workspace_root: dir.0.display().to_string(),
            owner: "dead".into(),
            approvers: Vec::new(),
            role: None,
            parent_session_id: None,
        })
        .unwrap();
    store
        .put_job(
            sid,
            &JobRecord {
                handle: d.handle.clone(),
                command: d.command.clone(),
                how: "you asked".into(),
                state: "running".into(),
                produced: 0,
                elapsed_ms: 0,
                redirect: None,
            },
        )
        .unwrap();
    store
        .put_job_context(
            sid,
            &JobContext {
                handle: d.handle.clone(),
                slug: Some("fuzz".into()),
                cwd: ".".into(),
                instance: EARLIER.into(),
                started_ms: now - 120_000,
            },
        )
        .unwrap();
    let failed = crate::kept_jobs::record_detached(&store, sid, std::slice::from_ref(d));
    assert!(failed.is_empty(), "{failed:?}");
    let rec = store
        .process(sid, &d.handle)
        .unwrap()
        .expect("a process row");
    assert_eq!(rec.state, ProcState::Detached);
    assert_eq!(rec.cgroup.as_deref(), Some(d.cgroup.as_str()));
    assert_eq!(
        (rec.pid, rec.identity.as_str()),
        (d.pid, d.identity.as_str())
    );
    assert_eq!(
        job_state(&store, sid, &d.handle),
        crate::kept_jobs::DETACHED
    );
    assert!(
        store.undelivered_jobs(sid).unwrap().is_empty(),
        "a detached job is not an ending"
    );
}

fn job_state(store: &Store, sid: &str, handle: &str) -> String {
    store
        .jobs(sid)
        .unwrap()
        .into_iter()
        .find(|j| j.handle == handle)
        .map(|j| j.state)
        .unwrap_or_default()
}

/// This daemon, opening the session with a process host on the byte vocabulary.
fn opened(dir: &Scratch, sid: &str) -> Harness {
    let mut cfg = Config::for_this_box(&dir.0);
    cfg.dialect = crate::Dialect::Qwen;
    // The seat that holds a process host, as the operator's own sessions do.
    cfg.seat = crate::config::Seat::Leticode;
    cfg.allow_bash = true;
    cfg.store = Some(dir.0.join("sessions.db"));
    cfg.session_id = sid.into();
    cfg.vocab_gguf = None;
    let parts = Parts::load(&cfg).expect("the byte vocabulary loads");
    *parts.mode_store.write().unwrap() = crate::modes::ModeStore::default();
    Harness::open(&parts, cfg, Hub::new(sid)).expect("the session opens")
}

fn until(limit: Duration, mut f: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + limit;
    while std::time::Instant::now() < deadline {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    f()
}

/// The next daemon adopts a kept job: its rows read `running` and name this daemon, its output
/// file goes on growing, and the restart notice names it on one line.
#[test]
fn a_new_daemon_adopts_a_kept_job_and_says_so_once() {
    let dir = Scratch::new("kept-adopt");
    let sid = "s-kept-adopt";
    let Some(d) = kept_by_an_earlier_daemon(
        &dir,
        "i=0; while :; do echo tick $i; i=$((i+1)); sleep 0.05; done",
        Duration::from_millis(300),
    ) else {
        return;
    };
    left_behind(&dir, sid, &d);
    let out = letibot_tools::exec::detach::out_path(std::path::Path::new(&d.raw));
    let before = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
    let mut h = opened(&dir, sid);
    let store = Store::open(&dir.0.join("sessions.db")).unwrap();
    assert_eq!(job_state(&store, sid, &d.handle), "running");
    let rec = store.process(sid, &d.handle).unwrap().unwrap();
    assert_eq!(rec.state, ProcState::Running);
    let ctx = store.job_context(sid, &d.handle).unwrap().unwrap();
    assert_eq!(ctx.instance, h.session_registry.instance());
    assert!(
        until(Duration::from_secs(30), || std::fs::metadata(&out)
            .is_ok_and(|m| m.len() > before + 20)),
        "the output file stopped at {before} bytes"
    );
    let text = h.restore_after_restart().expect("the restart is said");
    let label = format!(
        "job `{}`",
        letibot_tools::exec::job_label(Some("fuzz"), &d.handle)
    );
    let lines: Vec<&str> = text
        .lines()
        .filter(|l| l.contains(&d.handle) || l.contains("fuzz"))
        .collect();
    assert_eq!(lines.len(), 1, "{text}");
    assert!(
        lines[0].contains("still running, reattached") && lines[0].contains(&label),
        "{text}"
    );
    let _ = std::fs::write(std::path::Path::new(&d.cgroup).join("cgroup.kill"), "1");
}

/// A kept job that ended while no daemon watched is settled from its status file with its own
/// exit code, and its ending is owed to the model, not reminded as lost.
#[test]
fn a_kept_job_that_ended_while_detached_is_delivered_with_its_code() {
    let dir = Scratch::new("kept-ended");
    let sid = "s-kept-ended";
    let Some(d) = kept_by_an_earlier_daemon(
        &dir,
        "sleep 0.3; echo finished; exit 3",
        Duration::from_millis(50),
    ) else {
        return;
    };
    left_behind(&dir, sid, &d);
    let status = letibot_tools::exec::detach::status_path(std::path::Path::new(&d.raw));
    assert!(
        until(Duration::from_secs(30), || {
            letibot_tools::exec::detach::prove(&d) == letibot_tools::exec::detach::Proof::Ended(3)
        }),
        "no status at {}",
        status.display()
    );
    let mut h = opened(&dir, sid);
    let store = Store::open(&dir.0.join("sessions.db")).unwrap();
    assert!(
        until(Duration::from_secs(30), || job_state(
            &store, sid, &d.handle
        ) == "exited 3"),
        "{}",
        job_state(&store, sid, &d.handle)
    );
    let undelivered: Vec<String> = store
        .undelivered_jobs(sid)
        .unwrap()
        .into_iter()
        .map(|j| j.handle)
        .collect();
    assert_eq!(undelivered, vec![d.handle.clone()], "its ending is owed");
    // One line says it ended while detached; the ending itself is the watcher's settlement, so
    // the restart pass neither reminds of it nor says its result a second time.
    let text = h.restore_after_restart().expect("the restart is said");
    let lines: Vec<&str> = text.lines().filter(|l| l.contains(&d.handle)).collect();
    assert_eq!(lines.len(), 1, "{text}");
    assert!(
        lines[0].contains("ended while no daemon watched it (exited 3)"),
        "{text}"
    );
}

/// A row whose job nothing proves is said lost, and never adopted.
#[test]
fn a_kept_job_with_no_proof_is_said_lost() {
    let dir = Scratch::new("kept-lost");
    let sid = "s-kept-lost";
    let Some(mut d) = kept_by_an_earlier_daemon(
        &dir,
        "i=0; while :; do echo tick $i; i=$((i+1)); sleep 0.05; done",
        Duration::from_millis(100),
    ) else {
        return;
    };
    let real = d.clone();
    d.identity = "start:1".into();
    left_behind(&dir, sid, &d);
    let mut h = opened(&dir, sid);
    let store = Store::open(&dir.0.join("sessions.db")).unwrap();
    assert_eq!(
        job_state(&store, sid, &d.handle),
        crate::kept_jobs::LOST_NO_PROOF
    );
    let text = h.restore_after_restart().expect("the restart is said");
    assert!(
        text.contains("lost: no proof") && text.contains("not the process stamped"),
        "{text}"
    );
    let _ = std::fs::write(std::path::Path::new(&real.cgroup).join("cgroup.kill"), "1");
}

/// A restart of a VM session keeps nothing: its running jobs end, and one sentence says why.
#[test]
fn a_restart_of_a_vm_session_ends_its_jobs_in_one_sentence() {
    let Some(_) = letibot_tokencore::apparatus::present(
        "a process-lifetime tree (cgroup v2; process groups on macOS)",
        letibot_tools::host_tree().is_ok(),
    ) else {
        return;
    };
    let dir = Scratch::new("kept-vm");
    let host = HostProcesses::new(&dir.0).expect("a host");
    host.set_scratch_dir(dir.0.join("scratch"));
    let id = host
        .spawn(&SpawnRequest {
            command: "sleep 300".into(),
            slug: None,
            cwd: ".".into(),
            scope: ScopeKind::Session,
            scope_name: None,
            background: true,
            env: vec![],
            tty: false,
            memory: None,
            env_trailer: false,
        })
        .expect("the job starts");
    let said = crate::kept_jobs::end_in_vm(&host).expect("a job was running");
    assert!(
        said.contains("runs in a VM") && said.contains("1 running job(s) cannot be kept"),
        "{said}"
    );
    assert!(
        !host.job(&id).unwrap().state.is_running(),
        "a VM job outlived the restart"
    );
    assert!(crate::kept_jobs::end_in_vm(&host).is_none(), "said twice");
}

/// The session's run ender, as the daemon's stop rings it: a restart keeps the job running and
/// writes it down `detached`; a plain stop kills it, as before.
#[test]
fn a_restart_keeps_the_job_and_a_plain_stop_kills_it() {
    let Some(_) = letibot_tokencore::apparatus::present(
        "a process-lifetime tree (cgroup v2; process groups on macOS)",
        letibot_tools::host_tree().is_ok(),
    ) else {
        return;
    };
    for restart in [true, false] {
        let dir = Scratch::new(if restart { "kept-ender" } else { "plain-ender" });
        let sid = if restart {
            "s-kept-ender"
        } else {
            "s-plain-ender"
        };
        let h = opened(&dir, sid);
        let host = h.runtime.backend.processes_arc().expect("a process host");
        let id = host
            .spawn(&SpawnRequest {
                command: "sleep 300".into(),
                slug: None,
                cwd: ".".into(),
                scope: ScopeKind::Session,
                scope_name: None,
                background: true,
                env: vec![],
                tty: false,
                memory: None,
                env_trailer: false,
            })
            .expect("the job starts");
        let view = host.job(&id).expect("the job");
        let stamp = letibot_tools::exec::procs::read(view.pid)
            .map(|p| format!("start:{}", p.start))
            .expect("a stamp");
        if restart {
            h.session_registry.close_for_restart();
        } else {
            h.session_registry.close();
        }
        let store = Store::open(&dir.0.join("sessions.db")).unwrap();
        let rec = store.process(sid, &id.0).unwrap();
        if restart {
            let rec = rec.expect("the kept job's row");
            assert_eq!(rec.state, ProcState::Detached);
            assert!(rec.cgroup.is_some(), "{rec:?}");
            assert_eq!(rec.identity, stamp);
            drop(h);
            drop(host);
            std::thread::sleep(Duration::from_millis(200));
            assert!(
                letibot_tools::exec::detach::stamp_matches(view.pid, &stamp),
                "the kept job did not survive the stop"
            );
            let _ = std::fs::write(
                std::path::Path::new(rec.cgroup.as_deref().unwrap()).join("cgroup.kill"),
                "1",
            );
        } else {
            assert!(
                rec.is_none_or(|r| r.state != ProcState::Detached),
                "a plain stop kept it"
            );
            assert!(
                until(Duration::from_secs(30), || {
                    !letibot_tools::exec::detach::stamp_matches(view.pid, &stamp)
                }),
                "a plain stop left the job running"
            );
        }
    }
}

/// The marker the launcher leaves is the one the daemon takes: two spellings of one path.
#[test]
fn the_restart_marker_is_where_the_launcher_leaves_it() {
    let at = crate::daemon::restart_marker(std::path::Path::new("/run/user/1/letibot/h-abc.sock"));
    assert_eq!(
        at,
        std::path::Path::new("/run/user/1/letibot/h-abc.sock.restart")
    );
    let launcher = include_str!("../../../../../scripts/letibot");
    assert!(
        launcher.contains("marker=\"$SOCKET.restart\""),
        "the launcher moved it"
    );
}
