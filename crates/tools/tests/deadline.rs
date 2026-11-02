//! The deadline is enforced by the host, not by the thread that runs the command.
//!
//! A deadline enforced at the top of `bash`'s wait loop is enforced inside the thing it guards: if
//! that thread does not come back round the loop, nothing ends the run and the daemon's one worker is
//! held for as long as the command lives. So each test spawns a run and never waits on it.

use std::time::{Duration, Instant};

use letibot_tools::exec::{
    DEADLINE_KILL, HostProcesses, JobState, ProcessHost, ScopeKind, SpawnRequest,
};

/// A foreground run of `command`, in this host's turn scope. `tty` decides whether the run gets its own
/// terminal, which matters: a run with `/dev/null` on stdin gets EOF at once and exits, a different
/// shape entirely.
fn spawn(host: &HostProcesses, command: &str, tty: bool) -> letibot_tools::exec::JobId {
    host.spawn(&SpawnRequest {
        command: command.to_string(),
        // Nobody named this run: the substrate's own test.
        slug: None,
        cwd: ".".into(),
        scope: ScopeKind::Turn,
        scope_name: None,
        background: false,
        env: vec![],
        tty,
        memory: None,
    })
    .expect("the run starts")
}

/// Wait for the state to stop being `Running`, bounded, without polling the run's output: the
/// deadline is the only thing that can end it, and this is only how long the test waits to see
/// that it did.
fn settled(host: &HostProcesses, id: &letibot_tools::exec::JobId) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if host.job(id).map(|v| !v.state.is_running()).unwrap_or(true) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The firing, once the watchdog has written it: a second read rather than the same one, because the
/// watchdog settles the job first and records the reap second.
fn firing(
    host: &HostProcesses,
    id: &letibot_tools::exec::JobId,
) -> Option<letibot_tools::exec::DeadlineFired> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Some(f) = host.deadlines_fired().into_iter().find(|f| &f.job == id) {
            return Some(f);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    None
}

/// The cgroup's members, read off the world rather than off the record the kill left: a record
/// that said `killed` while the processes ran would be an unfalsifiable zero.
fn members(host: &HostProcesses, job: &letibot_tools::exec::JobId) -> Vec<u32> {
    let Some(view) = host.job(job) else {
        // A job the host no longer lists has no cgroup left to read. That is an answer, not an
        // error to report as one.
        return Vec::new();
    };
    // Live members, as the tree reads them: on macOS `cgroup.procs` holds group ids that
    // outlive their last member until the reaper removes the directory.
    letibot_tools::exec::live_members(&view.scope)
}

/// What is left of the run once the reap has had its moment: [`members`] read until empty or
/// two seconds pass, and the last reading returned. One reading, so an assertion and its
/// message cannot disagree while the kill is recorded and the reap follows it.
fn left_after_reap(host: &HostProcesses, job: &letibot_tools::exec::JobId) -> Vec<u32> {
    let until = std::time::Instant::now() + Duration::from_secs(2);
    let mut left = members(host, job);
    while !left.is_empty() && std::time::Instant::now() < until {
        std::thread::sleep(Duration::from_millis(20));
        left = members(host, job);
    }
    left
}

/// A host with a real cgroup tree, or `None` with the reason said out loud.
fn host() -> Option<HostProcesses> {
    match HostProcesses::new(std::env::temp_dir()) {
        Ok(h) => Some(h),
        Err(e) => {
            // A host without a delegated cgroup v2 subtree is a real thing, and an `#[ignore]`
            // here would be a green suite that measured nothing.
            eprintln!("deadline: no cgroup v2 subtree here: {e}");
            assert!(
                format!("{e}").contains("cgroup"),
                "a refusal must name what was missing: {e}"
            );
            // Through `apparatus`, so `LETIBOT_REQUIRE_APPARATUS=1` can refuse the skip.
            letibot_tokencore::apparatus::absent("a cgroup v2 subtree this user may write");
            None
        }
    }
}

/// A run whose own thread never comes back round any loop is still ended: `sleep 300` is spawned, its
/// deadline armed, and then nothing waits on it, which is precisely the state the wait loop is in when
/// it is not moving.
#[test]
fn a_run_nobody_is_waiting_on_is_still_ended_by_its_deadline() {
    let Some(host) = host() else { return };
    let id = spawn(&host, "sleep 300", false);

    // Presence first, so the absence below is a measurement and not a boot window.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut seen = false;
    while Instant::now() < deadline && !seen {
        seen = !members(&host, &id).is_empty();
        if !seen {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    assert!(
        seen,
        "the run never appeared in its cgroup, so nothing can be concluded"
    );
    let pids = members(&host, &id);

    // Armed, and then not waited on: this is the whole test.
    host.arm_deadline(&id, Duration::from_millis(400));

    // Long enough that a run which was going to be ended has been, and that an unended
    // `sleep 300` is unmistakably still there.
    settled(&host, &id);

    let view = host.job(&id).expect("the job is listed");
    assert!(
        !view.state.is_running(),
        "a run nobody is waiting on outlived its deadline: {} — this is the state where \
         the daemon's one worker is held by the run and nothing ends it",
        view.state.word()
    );
    assert_eq!(
        view.state,
        JobState::Killed {
            by: DEADLINE_KILL.to_string()
        },
        "the run must end BY ITS DEADLINE and say so — *killed* for any other reason \
         proves nothing about the timeout: {}",
        view.state.word()
    );

    // And the world agrees with the record: measured on `/proc`, not on the state, once the
    // reap has had its moment.
    let left = left_after_reap(&host, &id);
    assert!(
        left.is_empty(),
        "the cgroup still holds {left:?} two seconds after the deadline"
    );
    for p in &pids {
        assert!(
            std::fs::metadata(format!("/proc/{p}")).is_err()
                || std::fs::read_to_string(format!("/proc/{p}/stat"))
                    .map(|s| s.contains(") Z "))
                    .unwrap_or(true),
            "pid {p} is still alive after its deadline"
        );
    }

    // The firing is a record, so the claim can be checked afterwards, and it carries the same
    // reap a `job_kill` leaves: that makes it a fact about a cgroup, not a claim about one.
    let fired = firing(&host, &id).expect("the deadline left no record of having fired");
    assert!(
        fired.reaping.mechanism == letibot_tools::exec::HOST_KILL,
        "the deadline must end the run by its cgroup, which is the mechanism that does not \
         care what uid the process is: {}",
        fired.reaping.summary()
    );
    assert!(
        fired.reaping.survivors.is_empty(),
        "the deadline left survivors: {}",
        fired.reaping.summary()
    );
    assert!(
        fired.after >= Duration::from_millis(400),
        "it cannot have been ended before its deadline: {:?}",
        fired.after
    );
}

/// The same, for a tree this uid may not signal: a root process blocked on a read whose `/proc` entry is
/// `EACCES` cannot be signalled by pid, so the deadline goes through `cgroup.kill`, a file this daemon
/// owns, which reaches every process the run forked.
#[test]
fn a_root_process_the_daemon_may_not_signal_is_ended_by_the_same_deadline() {
    let Some(host) = host() else { return };
    let Some(_) = letibot_tokencore::apparatus::present(
        "/usr/bin/su (a root process this daemon may not signal)",
        std::path::Path::new("/usr/bin/su").exists(),
    ) else {
        return;
    };
    // `tty: true` is the run with its own terminal, whose master the daemon holds: that is what
    // `su` blocks on for a password it will never be given, and the same deadline ends it.
    // BSD `su` (macOS) takes the login before `-c`; util-linux's takes either order.
    let su = if cfg!(target_os = "macos") {
        "/usr/bin/su root -c true"
    } else {
        "/usr/bin/su -c true"
    };
    let id = spawn(&host, su, true);

    // Presence, and the right presence: the wrapper joins its cgroup first and `su` `exec`s into
    // it a moment later, so the wait is for a process this uid may not look at, which is the
    // fact the test is about.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut pids = Vec::new();
    let mut unlookable = false;
    while Instant::now() < deadline && !unlookable {
        pids = members(&host, &id);
        unlookable = pids.iter().any(|p| !may_look_at(*p));
        if !unlookable {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    assert!(
        unlookable,
        "this test is about a process the daemon may not look at and the tree never had \
         one: {pids:?} — without it this is measuring a different shape"
    );

    host.arm_deadline(&id, Duration::from_millis(400));
    settled(&host, &id);
    let view = host.job(&id).expect("the job is listed");
    assert_eq!(
        view.state,
        JobState::Killed {
            by: DEADLINE_KILL.to_string()
        },
        "a root process on the run's own device must end by the deadline too: {}",
        view.state.word()
    );
    let left = left_after_reap(&host, &id);
    assert!(
        left.is_empty(),
        "the cgroup still holds {left:?} — `cgroup.kill` did not reach the root process"
    );
}

/// A run ended by something other than its own deadline says WHO ended it. Four different things
/// happened to one command, and one word for all four would report a caller's own decision as the
/// command's answer.
#[test]
fn a_run_ended_by_the_daemon_stopping_says_so() {
    let Some(host) = host() else { return };
    let id = spawn(&host, "sleep 300", false);

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut seen = false;
    while Instant::now() < deadline && !seen {
        seen = !members(&host, &id).is_empty();
        if !seen {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    assert!(seen, "the run never appeared in its cgroup");

    let ended = host.end_running("the daemon stopping");
    assert_eq!(ended.len(), 1, "one running job, one reap: {ended:?}");
    assert_eq!(
        host.job(&id).expect("listed").state,
        JobState::Killed {
            by: "the daemon stopping".into()
        },
        "the row has to name what ended it, not the signal that was its shape"
    );
    let left = left_after_reap(&host, &id);
    assert!(left.is_empty(), "and the cgroup has to be empty: {left:?}");
    // Nothing running: the second call is the honest zero rather than a second kill.
    assert!(
        host.end_running("the daemon stopping").is_empty(),
        "a second stop must find nothing to end rather than reporting the first one again"
    );
}

/// Can this uid look at the process, asked the way the daemon asks it: not whose uid it is, but whether
/// the daemon may open `/proc/<pid>/fd/0`, which is the question that changes its behaviour.
fn may_look_at(pid: u32) -> bool {
    std::fs::read_link(format!("/proc/{pid}/fd/0")).is_ok()
}
