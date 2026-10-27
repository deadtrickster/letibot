//! The supervisor's own tests: a state machine over real records and a probe that is handed the
//! world's answers, plus one test that runs [`SshProbe`]'s script for real against a job directory
//! made the way a remote host makes one.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use letibot_tokencore::store::{ProcState, ProcessRecord, Store};
use letibot_tools::exec::{ProcessHost as _, Waited};

use super::*;

/// **What a host answers**, in the order the test says.
enum Say {
    /// Every process is there.
    Alive,
    /// One handle's answer, with the rest alive.
    One(String, Answer),
    /// The host could not be reached.
    Fail(String),
}

/// A probe whose answers the test writes down — and which records every call and every kill, so
/// *one check per host* and *the kill went to the group* are assertions rather than hopes.
struct Fake {
    says: Mutex<Vec<Say>>,
    calls: Mutex<Vec<Option<String>>>,
    kills: Mutex<Vec<(Option<String>, u32)>>,
    checks: AtomicUsize,
}

impl Fake {
    fn new(says: Vec<Say>) -> Arc<Fake> {
        Arc::new(Fake {
            says: Mutex::new(says),
            calls: Mutex::new(Vec::new()),
            kills: Mutex::new(Vec::new()),
            checks: AtomicUsize::new(0),
        })
    }

    fn calls(&self) -> Vec<Option<String>> {
        self.calls.lock().unwrap().clone()
    }

    fn kills(&self) -> Vec<(Option<String>, u32)> {
        self.kills.lock().unwrap().clone()
    }
}

impl Probe for Fake {
    fn check(&self, host: Option<&str>, wants: &[Want]) -> Checked {
        self.checks.fetch_add(1, Ordering::SeqCst);
        self.calls.lock().unwrap().push(host.map(str::to_string));
        let say = {
            let mut g = self.says.lock().unwrap();
            if g.is_empty() {
                Say::Alive
            } else {
                g.remove(0)
            }
        };
        match say {
            Say::Fail(why) => Checked::failed("ssh", Duration::ZERO, why),
            Say::Alive => Checked {
                how: "proc",
                took: Duration::ZERO,
                answers: wants
                    .iter()
                    .map(|w| (w.handle.clone(), Answer::Alive))
                    .collect(),
                error: None,
            },
            Say::One(handle, answer) => Checked {
                how: "proc",
                took: Duration::ZERO,
                answers: wants
                    .iter()
                    .map(|w| {
                        let a = if w.handle == handle {
                            answer
                        } else {
                            Answer::Alive
                        };
                        (w.handle.clone(), a)
                    })
                    .collect(),
                error: None,
            },
        }
    }

    fn kill(&self, host: Option<&str>, rec: &ProcessRecord, _by: &str) -> Result<(), String> {
        self.kills
            .lock()
            .unwrap()
            .push((host.map(str::to_string), rec.pgid.unwrap_or(rec.pid)));
        Ok(())
    }
}

/// A record in the state a spawn leaves it, with everything a check reads filled in.
fn rec(session: &str, handle: &str, host: Option<&str>, pid: u32) -> ProcessRecord {
    let mut r = ProcessRecord::starting(session, handle, host.map(str::to_string), pid);
    r.state = ProcState::Starting;
    r.pgid = Some(pid);
    r.identity = format!("starttime:{pid}00");
    r.command = "cargo test --offline".into();
    r.cwd = "/w".into();
    r.scope = format!("session.{session}");
    r
}

fn store_with(records: &[ProcessRecord]) -> Arc<Mutex<Store>> {
    let s = Arc::new(Mutex::new(Store::open_in_memory().unwrap()));
    for r in records {
        s.lock().unwrap().put_process(r).unwrap();
    }
    s
}

fn clock(ms: i64) -> Box<dyn Fn() -> i64 + Send + Sync> {
    Box::new(move || ms)
}

/// **A pid that is alive is not the job** — the identity is, and this is the whole reason the record
/// carries one. Measured against this very process: the same pid with a different start time is not
/// the process, and a probe that answered `alive` on the pid alone would report a job running for
/// ever on a box that had reused its number.
#[test]
fn a_live_pid_is_not_the_job_when_the_identity_differs() {
    let me = std::process::id();
    let mine = identity_of(me).expect("this process has an identity");
    assert_eq!(
        alive_with_identity(me, &mine),
        Some(true),
        "the process that is here, is here"
    );
    assert_eq!(
        alive_with_identity(me, "start:0"),
        Some(false),
        "the same pid with another start time is another process"
    );

    // And the probe's own reading of the same two facts.
    let probe = LocalProbe::new();
    let live = Want {
        handle: "j1".into(),
        pid: me,
        identity: mine,
        exit_path: None,
    };
    let reused = Want {
        handle: "j2".into(),
        pid: me,
        identity: "start:0".into(),
        exit_path: None,
    };
    let checked = probe.check(None, &[live, reused]);
    assert_eq!(checked.answer("j1"), Some(Answer::Alive));
    assert_eq!(
        checked.answer("j2"),
        Some(Answer::Ended {
            state: ProcState::Dead,
            exit: None
        }),
        "a pid that is somebody else's is a job that is gone, not one that runs"
    );

    // A pid nothing is behind is `dead`, and never `done 0`: nothing said it succeeded.
    let gone = Want {
        handle: "j3".into(),
        pid: 0x7FFF_FFF0,
        identity: "starttime:1".into(),
        exit_path: None,
    };
    assert_eq!(
        probe.check(None, &[gone]).answer("j3"),
        Some(Answer::Ended {
            state: ProcState::Dead,
            exit: None
        })
    );
}

/// **One check per host, whatever the number of jobs** — two local processes and three on one host
/// cost two checks, not five, because a check is a round trip and a per-job check is how eight
/// builds spend eight connections a second to learn one fact.
#[test]
fn one_check_covers_every_job_of_its_host() {
    let recs = vec![
        rec("s-a", "j1", None, 101),
        rec("s-a", "j2", None, 102),
        rec("s-b", "j3", Some("lab2x1"), 201),
        rec("s-b", "j4", Some("lab2x1"), 202),
        rec("s-b", "j5", Some("lab2x1"), 203),
    ];
    let s = store_with(&recs);
    let probe = Fake::new(vec![Say::Alive, Say::Alive]);
    let sup = Supervisor::new(s.clone(), probe.clone());
    let tick = sup.tick();

    assert_eq!(tick.hosts, 2);
    assert_eq!(
        probe.calls(),
        vec![None, Some("lab2x1".to_string())],
        "one call for the local job, one for the host — never one per job"
    );
    assert_eq!(tick.running, 5);
    let open = s.lock().unwrap().open_processes().unwrap();
    assert!(
        open.iter().all(|r| r.state == ProcState::Running),
        "every record reads running: {:?}",
        open.iter()
            .map(|r| (&r.handle, r.state))
            .collect::<Vec<_>>()
    );
    assert!(open.iter().all(|r| r.check_how.as_deref() == Some("proc")));
}

/// **A deadline is a wall clock, and it is enforced on the tick that finds it past.** The kill goes
/// to the process group, and the record says `stopped` because something ended it on purpose —
/// which is not `done`, and not an exit status a command ever gave.
#[test]
fn a_passed_deadline_kills_the_group_and_the_record_says_stopped() {
    let mut r = rec("s-a", "j1", None, 4242);
    r.deadline_ms = Some(1_000);
    let s = store_with(&[r]);
    let probe = Fake::new(vec![Say::Alive, Say::Alive]);

    // Before the deadline: it runs.
    let sup = Supervisor::with_clock(s.clone(), probe.clone(), clock(999));
    assert_eq!(
        sup.tick(),
        Tick {
            hosts: 1,
            running: 1,
            ..Default::default()
        }
    );
    assert!(probe.kills().is_empty());

    // Past it: one kill, at the group, and the record says stopped.
    let sup = Supervisor::with_clock(s.clone(), probe.clone(), clock(1_000));
    let tick = sup.tick();
    assert_eq!(tick.expired, vec!["j1".to_string()]);
    assert_eq!(tick.ended, vec![("j1".to_string(), ProcState::Stopped)]);
    assert_eq!(
        probe.kills(),
        vec![(None, 4242)],
        "the GROUP, which for a leader is its own pid"
    );
    let got = s.lock().unwrap().process("s-a", "j1").unwrap().unwrap();
    assert_eq!(got.state, ProcState::Stopped);
    assert_eq!(
        got.exit, None,
        "a kill is not an exit status of the command's"
    );
    assert_eq!(got.check_how.as_deref(), Some("expired"));

    // A job that vanished before its deadline is `dead`, never `stopped by its deadline`: killing a
    // process that is not there is not what happened.
    let mut r2 = rec("s-a", "j2", None, 4343);
    r2.deadline_ms = Some(1_000);
    s.lock().unwrap().put_process(&r2).unwrap();
    let probe2 = Fake::new(vec![Say::One(
        "j2".to_string(),
        Answer::Ended {
            state: ProcState::Dead,
            exit: None,
        },
    )]);
    let sup = Supervisor::with_clock(s.clone(), probe2.clone(), clock(2_000));
    sup.tick();
    assert!(probe2.kills().is_empty(), "nothing was killed");
    assert_eq!(
        s.lock()
            .unwrap()
            .process("s-a", "j2")
            .unwrap()
            .unwrap()
            .state,
        ProcState::Dead
    );
}

/// **A host that did not answer leaves the job `lost`, and the next tick asks again** — a job on a
/// machine that is unreachable may still be running, and a supervisor that wrote `dead` there would
/// be reporting a death nobody witnessed.
#[test]
fn an_unreachable_host_leaves_the_job_lost_and_the_next_tick_retries() {
    let s = store_with(&[rec("s-a", "j1", Some("lab2x1"), 77)]);
    let probe = Fake::new(vec![Say::Fail("connection closed".into()), Say::Alive]);
    let sup = Supervisor::new(s.clone(), probe.clone());

    let first = sup.tick();
    assert_eq!(first.lost, vec!["j1".to_string()]);
    let got = s.lock().unwrap().process("s-a", "j1").unwrap().unwrap();
    assert_eq!(got.state, ProcState::Lost);
    assert_eq!(got.check_how.as_deref(), Some("ssh"));
    assert_eq!(
        s.lock().unwrap().open_processes().unwrap().len(),
        1,
        "lost is still looked after — it may be running"
    );

    let second = sup.tick();
    assert_eq!(second.running, 1);
    assert_eq!(
        s.lock()
            .unwrap()
            .process("s-a", "j1")
            .unwrap()
            .unwrap()
            .state,
        ProcState::Running
    );
    assert_eq!(probe.calls().len(), 2, "retried");
}

/// **A session's close ends what that session owns, on every host** — and an interrupt or a turn
/// ending does not, which is why the only door to this is here and not on a turn's clock.
#[test]
fn a_teardown_ends_what_the_session_owns_and_leaves_other_sessions_alone() {
    let mut remote = rec("s-a", "j2", Some("lab2x1"), 88);
    remote.pgid = Some(88);
    let ended = {
        let mut r = rec("s-a", "j0", None, 11);
        r.state = ProcState::Done;
        r.exit = Some(0);
        r
    };
    let s = store_with(&[
        rec("s-a", "j1", None, 12),
        remote,
        ended,
        rec("s-b", "j3", None, 13),
    ]);
    let probe = Fake::new(vec![]);
    let sup = Supervisor::new(s.clone(), probe.clone());

    let torn = sup.teardown_session("s-a", "the session closing");
    let handles: Vec<&str> = torn.iter().map(|r| r.handle.as_str()).collect();
    assert_eq!(
        handles,
        vec!["j1", "j2"],
        "the open records of s-a, and not its ended one"
    );
    assert_eq!(
        probe.kills(),
        vec![(None, 12), (Some("lab2x1".to_string()), 88)],
        "every host the session owns work on"
    );
    assert_eq!(
        s.lock()
            .unwrap()
            .process("s-a", "j1")
            .unwrap()
            .unwrap()
            .state,
        ProcState::Stopped
    );
    assert_eq!(
        s.lock()
            .unwrap()
            .process("s-a", "j2")
            .unwrap()
            .unwrap()
            .state,
        ProcState::Stopped
    );
    assert_eq!(
        s.lock()
            .unwrap()
            .process("s-b", "j3")
            .unwrap()
            .unwrap()
            .state,
        ProcState::Starting,
        "another session's job is another session's"
    );
    assert_eq!(
        s.lock()
            .unwrap()
            .process("s-a", "j0")
            .unwrap()
            .unwrap()
            .exit,
        Some(0),
        "a job that had already ended keeps its exit"
    );

    // A kill that could not be delivered does not claim to have ended anything.
    let s2 = store_with(&[rec("s-c", "j4", Some("lab2x1"), 99)]);
    let probe = Arc::new(Silent);
    let sup = Supervisor::new(s2.clone(), probe);
    sup.teardown_session("s-c", "the daemon stopping");
    assert_eq!(
        s2.lock()
            .unwrap()
            .process("s-c", "j4")
            .unwrap()
            .unwrap()
            .state,
        ProcState::Lost,
        "a teardown that never reached the host says so"
    );
}

/// A probe that cannot reach anything — the teardown's failure path.
struct Silent;
impl Probe for Silent {
    fn check(&self, _: Option<&str>, _: &[Want]) -> Checked {
        Checked::failed("ssh", Duration::ZERO, "unreachable")
    }
    fn kill(&self, _: Option<&str>, _: &ProcessRecord, _: &str) -> Result<(), String> {
        Err("unreachable".into())
    }
}

/// **A restart finds what is still running, from the store alone.** The new supervisor is a new
/// value with no memory of the old one — the records are all it has — and that is the whole point:
/// the daemon that wrote them is gone.
#[test]
fn a_restart_adopts_the_live_jobs_and_buries_the_vanished_ones() {
    let s = store_with(&[
        rec("s-a", "j1", None, 101),
        rec("s-a", "j2", None, 102),
        rec("s-a", "j3", Some("lab2x1"), 303),
    ]);
    // The first daemon: one tick, so every record carries a check of its own.
    let first = Fake::new(vec![Say::Alive, Say::Alive]);
    Supervisor::new(s.clone(), first).tick();
    assert_eq!(s.lock().unwrap().open_processes().unwrap().len(), 3);

    // ...and it stops. A crash: nothing was torn down, and the records are as they were.
    let probe = Fake::new(vec![
        // This box: j1 is still running, j2 is gone with no exit to report.
        Say::One(
            "j2".to_string(),
            Answer::Ended {
                state: ProcState::Dead,
                exit: None,
            },
        ),
        // The host: its job directory says j3 exited 3.
        Say::One(
            "j3".to_string(),
            Answer::Ended {
                state: ProcState::Done,
                exit: Some(3),
            },
        ),
    ]);
    let restarted = Supervisor::new(s.clone(), probe.clone());
    let tick = restarted.recover();

    assert_eq!(tick.hosts, 2);
    assert_eq!(
        s.lock()
            .unwrap()
            .process("s-a", "j1")
            .unwrap()
            .unwrap()
            .state,
        ProcState::Running
    );
    let j2 = s.lock().unwrap().process("s-a", "j2").unwrap().unwrap();
    assert_eq!(
        (j2.state, j2.exit),
        (ProcState::Dead, None),
        "gone with no exit recorded is dead, which is not an exit of zero"
    );
    let j3 = s.lock().unwrap().process("s-a", "j3").unwrap().unwrap();
    assert_eq!(
        (j3.state, j3.exit),
        (ProcState::Done, Some(3)),
        "the host's own job directory says how it ended"
    );
    assert_eq!(
        s.lock().unwrap().open_processes().unwrap().len(),
        1,
        "the one the caller has to re-attach a session to"
    );
}

/// **A tick that cannot read its own work list says so** — a supervisor with no records looks exactly
/// like a supervisor whose every job has ended, and the one thing that must not happen is the second
/// reading being printed for the first.
#[test]
fn a_tick_that_cannot_read_its_records_is_reported_rather_than_looking_quiet() {
    // A store whose `process` table has been removed underneath it: there is nothing to read.
    let s = Arc::new(Mutex::new(Store::open_in_memory().unwrap()));
    s.lock()
        .unwrap()
        .put_process(&rec("s-a", "j1", None, 5))
        .unwrap();
    s.lock()
        .unwrap()
        .connection()
        .execute_batch("DROP TABLE process;")
        .unwrap();
    let probe = Fake::new(vec![Say::Alive]);
    let tick = Supervisor::new(s, probe.clone()).tick();
    assert_eq!(tick.hosts, 0, "no host was checked");
    assert_eq!(tick.errors.len(), 1, "{:?}", tick.errors);
    assert!(
        tick.errors[0].contains("reading the open records"),
        "{:?}",
        tick.errors
    );
    assert_eq!(
        probe.checks.load(Ordering::SeqCst),
        0,
        "nothing was checked"
    );
}

/// **`SshProbe`'s script against a real job directory**, run by a runner that executes it locally —
/// the fake-ssh shape RH1's tests use, and never a real host.
///
/// Three jobs in one pass: one still running, one whose exit file says 7, and one whose pid is gone
/// with no exit file at all. That is the whole remote state machine in one script, which is what the
/// one-check-per-host rule costs and buys.
#[test]
fn the_ssh_probe_reads_a_job_directory_in_one_pass() {
    let root = std::env::temp_dir().join(format!(
        "letibot-supervise-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let jobs = root.join("jobs");
    std::fs::create_dir_all(&jobs).unwrap();

    // A real process whose command carries the marker — started and killed by this test alone.
    let marker = format!("supervise-marker-{}", std::process::id());
    let mut child = std::process::Command::new("sh")
        .args(["-c", &format!("sleep 60; : {marker}")])
        .spawn()
        .expect("a process to watch");
    let running = child.id();

    let wants = vec![
        Want {
            handle: "j1".into(),
            pid: running,
            identity: format!("cmd:{marker}"),
            exit_path: Some(format!("{}/j1.exit", jobs.display())),
        },
        Want {
            handle: "j2".into(),
            pid: 999_999,
            identity: "cmd:the-build-that-finished".into(),
            exit_path: Some(format!("{}/j2.exit", jobs.display())),
        },
        Want {
            handle: "j3".into(),
            pid: 999_998,
            identity: "cmd:the-one-that-vanished".into(),
            exit_path: Some(format!("{}/j3.exit", jobs.display())),
        },
    ];
    std::fs::write(jobs.join("j2.exit"), "7\n").unwrap();

    // **The fake ssh**: run the script through `sh`, against this box, standing in for the host.
    let run = Arc::new(|_host: &str, script: &str| {
        let out = std::process::Command::new("sh")
            .args(["-c", script])
            .output()
            .map_err(|e| e.to_string())?;
        Ok((
            String::from_utf8_lossy(&out.stdout).to_string(),
            out.status.code().unwrap_or(-1),
        ))
    });
    let probe = SshProbe::new(run);
    let checked = probe.check(Some("lab2x1"), &wants);
    assert!(checked.error.is_none(), "{:?}", checked.error);
    assert_eq!(
        checked.answer("j1"),
        Some(Answer::Alive),
        "{:?}",
        checked.answers
    );
    assert_eq!(
        checked.answer("j2"),
        Some(Answer::Ended {
            state: ProcState::Done,
            exit: Some(7)
        }),
        "the exit file IS the settlement"
    );
    assert_eq!(
        checked.answer("j3"),
        Some(Answer::Ended {
            state: ProcState::Dead,
            exit: None
        })
    );

    // A host that cannot be reached is the whole host's failure, not each job's.
    let refused = SshProbe::new(Arc::new(|_h: &str, _s: &str| {
        Err("connection refused".into())
    }));
    let lost = refused.check(Some("lab2x1"), &wants);
    assert!(lost.error.unwrap().contains("connection refused"));

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&root);
}

/// **A host over a temp root — or `None` when this box cannot delegate a cgroup v2 subtree.**
///
/// The same gate the crate's other host-spawning tests take: the absence is SAID and
/// `LETIBOT_REQUIRE_APPARATUS=1` refuses the run, because a `#[ignore]` here would be a green
/// suite that measured nothing.
fn a_host(name: &str) -> Option<(std::path::PathBuf, Arc<letibot_tools::exec::HostProcesses>)> {
    letibot_tokencore::apparatus::present(
        "a process-lifetime tree (cgroup v2; process groups on macOS)",
        letibot_tools::host_tree().is_ok(),
    )?;
    let root = std::env::temp_dir().join(format!(
        "letibot-supervise-host-{name}-{pid}",
        pid = std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("scratch");
    let host = letibot_tools::exec::HostProcesses::new(&root)
        .expect("the tree probed writable above, so this root builds one");
    Some((root, Arc::new(host)))
}

/// **END TO END: a real job, the real recorder, a real store — `running`, then `done(0)`.**
///
/// This is the wiring RH7 exists for, walked the way a daemon walks it: the job is spawned, its
/// live [`letibot_tools::exec::JobView`] is handed to `JobRecorder::process` (which is what
/// `JobWatchSink` does the moment a call is backgrounded), the job ends, and the settlement the
/// watcher builds is handed to `JobRecorder::record`.
///
/// **Nothing here is invented in the record**: the pid, the identity and the scope are read off the
/// live view, and the assertion on the identity is the one that matters after a restart —
/// `start:<n>` names *which* process had that pid.
#[test]
fn a_real_job_reads_running_and_then_done_in_the_record() {
    use letibot_tokencore::store::{ProcState, SessionRecord, Store};
    let Some((root, host)) = a_host("end-to-end") else {
        return;
    };
    let dir = root.join("store");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sessions.db");
    let store = Store::open(&path).unwrap();
    store
        .put_session(&SessionRecord {
            id: "s-e2e".into(),
            title: None,
            model_id: "m".into(),
            dialect_sha: "sha".into(),
            workspace_root: root.display().to_string(),
            owner: "dead".into(),
            role: None,
            approvers: vec![],
            parent_session_id: None,
        })
        .unwrap();

    let id = host
        .spawn(&letibot_tools::exec::SpawnRequest {
            command: "sleep 3; exit 0".into(),
            slug: Some("end-to-end".into()),
            cwd: "/".into(),
            scope: letibot_tools::exec::ScopeKind::Session,
            scope_name: None,
            background: true,
            env: vec![],
            tty: false,
            memory: None,
        })
        .expect("spawn");
    let view = host.job(&id).expect("the job is in the table");

    let recorder =
        crate::jobwatch::JobRecorder::new(path.clone(), "s-e2e".into(), std::sync::Weak::new());
    recorder.process(&view);
    let got = store.process("s-e2e", &view.id.0).unwrap().expect("a row");
    assert_eq!(
        got.state,
        ProcState::Running,
        "written as running at the start"
    );
    assert_eq!(got.pid, view.pid);
    assert_eq!(
        got.identity,
        identity_of(view.pid).expect("this box can identify a pid"),
        "the record names WHICH process has that pid"
    );
    assert!(got.host.is_none(), "a job on this box names no host");
    assert_eq!(got.scope, view.owner.to_string());

    // **The job ends, and the settlement the watcher would build is the record's ending too.**
    let waited = host.wait_job(&id, Duration::from_secs(60));
    let state = match waited {
        Ok(Waited::Happened { state: Some(s), .. }) => s,
        other => panic!("the job did not settle: {other:?}"),
    };
    let view = host.job(&id).expect("the view survives the settle");
    recorder.record(&crate::jobwatch::JobCompletion {
        memory: None,
        kind: crate::jobwatch::BackgroundKind::Job,
        owner: "s-e2e".into(),
        job: view.id.0.clone(),
        command: view.command.clone(),
        slug: view.slug.clone(),
        state: state.word(),
        produced: view.produced,
        elapsed_ms: view.ran_for.map(|d| d.as_millis() as u64).unwrap_or(0),
        detail: String::new(),
        arrived: std::time::SystemTime::now(),
    });
    let got = store.process("s-e2e", &id.0).unwrap().expect("a row");
    assert_eq!(
        (got.state, got.exit),
        (ProcState::Done, Some(0)),
        "the ending the row carries is the exit the command gave"
    );
    assert!(
        store.open_processes().unwrap().is_empty(),
        "nothing left for the supervisor to look after"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// **A RESTART, end to end: a fresh host adopts the record and the job is running again.**
///
/// The daemon that started this job is `host`; the one that finds it is `restarted`, which shares
/// no memory with it — the store is the whole of what it has. What it must get back is a job the
/// layers above can list, wait on and kill, and what it must NOT do is invent an exit: a process
/// that has gone settles as [`letibot_tools::exec::JobState::Dead`].
#[test]
fn a_restarted_host_adopts_a_live_job_from_its_record() {
    use letibot_tokencore::store::Store;
    let Some((root, host)) = a_host("adopt") else {
        return;
    };
    let path = root.join("sessions.db");
    let store = Store::open(&path).unwrap();
    let id = host
        .spawn(&letibot_tools::exec::SpawnRequest {
            command: "sleep 30".into(),
            slug: Some("adopted".into()),
            cwd: "/".into(),
            scope: letibot_tools::exec::ScopeKind::Session,
            scope_name: None,
            background: true,
            env: vec![],
            tty: false,
            memory: None,
        })
        .expect("spawn");
    let view = host.job(&id).expect("the job is in the table");
    let recorder =
        crate::jobwatch::JobRecorder::new(path.clone(), "s-e2e".into(), std::sync::Weak::new());
    recorder.process(&view);

    // **The other daemon.** Its own tree, its own table, and the record — nothing else.
    let restarted = letibot_tools::exec::HostProcesses::new(&root).expect("a second host");
    let rec = store.process("s-e2e", &id.0).unwrap().expect("the record");
    let adopted = restarted
        .adopt(&letibot_tools::exec::Adopted {
            handle: rec.handle.clone(),
            command: rec.command.clone(),
            slug: None,
            cwd: rec.cwd.clone(),
            pid: rec.pid,
            identity: rec.identity.clone(),
            scope: rec.scope.clone(),
        })
        .expect("adopted");
    assert_eq!(adopted, id);
    let seen = restarted.job(&adopted).expect("the adopted job is listed");
    assert!(
        seen.state.is_running(),
        "a live process is a job the new daemon runs: {seen:?}"
    );
    assert_eq!(seen.pid, rec.pid);
    assert_eq!(seen.command, rec.command);

    // **And when it is ended, it ends — killed through its own cgroup, as `job_kill` would.**
    restarted.kill_job(&adopted).expect("the kill reached it");
    let waited = restarted.wait_job(&adopted, Duration::from_secs(30));
    match waited {
        Ok(Waited::Happened { state: Some(s), .. }) => {
            assert!(!s.is_running(), "an ended job is not running: {s:?}");
        }
        other => panic!("the adopted job did not settle: {other:?}"),
    }

    // **A record whose process is gone is `Dead`, never `exited 0`** — the half a supervisor
    // exists to get right, and the one an adoption thread settles.
    let mut vanished = rec.clone();
    vanished.handle = "j-vanished".into();
    vanished.pid = 0x7FFF_FFF0;
    vanished.identity = "start:1".into();
    store.put_process(&vanished).unwrap();
    let ghost = restarted
        .adopt(&letibot_tools::exec::Adopted {
            handle: vanished.handle.clone(),
            command: vanished.command.clone(),
            slug: None,
            cwd: vanished.cwd.clone(),
            pid: vanished.pid,
            identity: vanished.identity.clone(),
            scope: vanished.scope.clone(),
        })
        .expect("adopted");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let state = restarted.job(&ghost).expect("still listed").state;
        if !state.is_running() {
            assert_eq!(
                state,
                letibot_tools::exec::JobState::Dead,
                "gone, with no exit"
            );
            break;
        }
        assert!(Instant::now() < deadline, "the adopted ghost never settled");
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// **The operator's own workloads are not ours to end** (RH9). A `teardown_session` — the door a
/// session's close and the daemon's stop both use — reaches `kill` for one record and not the
/// other, and the refusal names the pattern.
#[test]
fn a_never_touch_process_survives_a_teardown_and_the_pattern_is_named() {
    let dir = std::env::temp_dir().join(format!("letibot-never-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    let file = dir.join("hosts.toml");
    std::fs::write(
        &file,
        "[host.lab2x1]\nhostname = \"lab2x1\"\nuser = \"dead\"\n\
         never_touch = [\"etcd\", \"pgfuzz\"]\n",
    )
    .expect("hosts.toml");

    let theirs = {
        let mut r = rec("s-a", "j-etcd", Some("lab2x1"), 4242);
        r.command = "/usr/local/bin/etcd-fuzz --rounds 1000".into();
        r
    };
    let ours = {
        let mut r = rec("s-a", "j-build", Some("lab2x1"), 4243);
        r.command = "cargo test --offline".into();
        r
    };
    let store = store_with(&[theirs.clone(), ours.clone()]);
    let fake = Fake::new(vec![Say::Alive, Say::Alive]);
    let sup = Supervisor::new(Arc::clone(&store), Arc::clone(&fake) as Arc<dyn Probe>)
        .with_hosts(file.clone());

    // **The refusal, in words** — the sentence the operator reads, naming the pattern.
    let refused = sup
        .may_touch(&theirs)
        .expect_err("the operator's etcd fuzz is on the never-touch list");
    assert!(refused.contains("etcd"), "{refused}");
    assert!(refused.contains("lab2x1"), "{refused}");
    assert!(refused.contains("will not signal"), "{refused}");
    assert!(refused.contains("j-etcd"), "{refused}");
    // And the session's own command is not refused.
    sup.may_touch(&ours).expect("our own build may be ended");

    // **A teardown ends one and not the other.** The probe was told about exactly one process.
    let torn = sup.teardown_session("s-a", "the session closing");
    assert_eq!(torn.len(), 2, "both records were open");
    let killed = fake.kills();
    assert_eq!(
        killed.len(),
        1,
        "only one kill reached the host: {killed:?}"
    );
    assert_eq!(killed[0].1, 4243, "the build's pid, not the fuzz's");

    let held = store.lock().unwrap();
    let left = held.process("s-a", "j-etcd").unwrap().expect("a row");
    assert_eq!(
        left.state,
        ProcState::Lost,
        "the record stays open: nothing was ended, so nothing may say `stopped`"
    );
    assert_eq!(
        left.check_how.as_deref(),
        Some("refused"),
        "the record says which of the two endings it was"
    );
    let ended = held.process("s-a", "j-build").unwrap().expect("a row");
    assert_eq!(ended.state, ProcState::Stopped);
    drop(held);
    let _ = std::fs::remove_dir_all(&dir);
}

/// **A record on this box has no list to check** — `never_touch` is a `[host.NAME]` key, and this
/// box is not in the file. Said here so the hole is a fact with a test rather than a surprise.
#[test]
fn a_local_record_has_no_never_touch_list_and_that_is_said() {
    let mut r = rec("s-a", "j1", None, 4242);
    r.command = "/usr/local/bin/etcd-fuzz".into();
    let sup = Supervisor::new(store_with(&[r.clone()]), Fake::new(vec![]));
    sup.may_touch(&r)
        .expect("a record with no host has no declaration to read");
}
