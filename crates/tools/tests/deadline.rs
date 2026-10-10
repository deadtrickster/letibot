//! **The deadline is enforced by the host, and not by the thread that runs the command.**
//!
//! # The defect these measure
//!
//! A foreground run's timeout used to be enforced in exactly one place: the top of
//! `bash`'s wait loop, which runs **on the thread the run occupies**. That is the deadline
//! being enforced inside the thing it guards, and it has one consequence no care inside the
//! loop can fix — *if that thread does not come back round the loop, nothing ends the run*.
//! The worker that ran the command is held for as long as the command lives; the daemon has
//! one worker; so a single wedged run is the whole session, and the operator's report —
//! *"harnessd hangs"* — is that state.
//!
//! # What the loop is and is not
//!
//! Measured rather than assumed, 2026-10-06, on a live daemon: the loop's tick is a
//! *bounded* condvar wait (`Job::wait_until`) and the run's own processes are waited on by
//! **other** threads (the drain and the waiter), so the loop does not itself block in a
//! syscall on the run's behalf. A root `su` blocked on the device the daemon held for it —
//! a pipe then, its own terminal since `agent/quiet-shell` — with its whole
//! tree unreadable to the daemon, was killed by its deadline at 120.02 s, with the worker
//! in `futex_do_wait` for the whole of it. So *a run stuck in a syscall* is not what wedges
//! the check.
//!
//! What is true is the structural half, and it is the half that matters: **the check is
//! only there.** It is the only thing in the system that would end the run, so anything
//! that stops that thread reaching it removes the timeout entirely.
//!
//! # So these tests take the run's thread out of the picture
//!
//! [`HostProcesses::arm_deadline`] is the seam: the tool says *this run may live this
//! long*, and the **host** says who fires it. Each test below spawns a run and then
//! **never waits on it from the spawning thread** — no loop, no poll, no tick — which is
//! exactly the state the wait loop is in when it is not moving. The run must still be gone
//! when its deadline passes, and the record must say the deadline is what ended it.
//!
//! Before the change there was nothing to call: the deadline lived in the caller's loop,
//! so a test in this shape could not be written at all — which is the defect stated as
//! plainly as it can be.

use std::time::{Duration, Instant};

use letibot_tools::exec::{
    DEADLINE_KILL, HostProcesses, JobState, ProcessHost, ScopeKind, SpawnRequest,
};

/// A foreground run of `command`, in this host's turn scope.
///
/// `tty` is [`SpawnRequest::tty`], which is what decides whether the run gets **its own
/// terminal, held by the daemon**. It matters here: `sudo apt install mc` is the operator's
/// report and their run is the one with the terminal, so the test that reproduces it has to
/// have one too. With `/dev/null` on stdin, `su` gets EOF at once and exits — which is a
/// different shape and not the one being measured.
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
    })
    .expect("the run starts")
}

/// **Wait for the state to stop being `Running`**, bounded, without any polling of the
/// run's own output — the deadline is the only thing that can end it and this is only how
/// long the test is willing to wait to see that it did.
fn settled(host: &HostProcesses, id: &letibot_tools::exec::JobId) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if host.job(id).map(|v| !v.state.is_running()).unwrap_or(true) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The firing, once the watchdog has written it.
///
/// **A second read rather than the same one.** The watchdog settles the job *first* and
/// records the reap second, and that order is deliberate — `kill_job` settles first for the
/// same reason, so the job's own waiter thread cannot overwrite `Killed` with the SIGKILL it
/// is about to see. So a reader that sees the state can be a moment ahead of the record,
/// and this is that moment and not a missing fact.
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

/// **The cgroup's members, read off the world** — not off the record the kill left.
///
/// The distinction is the whole of the falsifier: a record that said *killed* while the
/// processes ran would be the unfalsifiable zero this substrate refuses everywhere else.
fn members(host: &HostProcesses, job: &letibot_tools::exec::JobId) -> Vec<u32> {
    let Some(view) = host.job(job) else {
        // A job the host no longer lists is a job that is gone; there is no cgroup left to
        // read. That is an answer, and not an error to report as one.
        return Vec::new();
    };
    // Live members, as the tree reads them — on macOS `cgroup.procs` holds group ids
    // that outlive their last member until the reaper removes the directory.
    letibot_tools::exec::live_members(&view.scope)
}

/// **What is left of the run once the reap has had its moment**: [`members`] read until it
/// is empty or two seconds have passed, and the last reading returned — ONE reading, so an
/// assertion and its message cannot disagree. The kill is recorded when it is sent and the
/// reap follows it; read twice, an assertion saw a process its message no longer did
/// (*"the cgroup still holds []"*, CI's macOS job, 2026-10-08).
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
            // The same branch every other substrate test takes: a host without a
            // delegated cgroup v2 subtree is a real thing, and a `#[ignore]` here would
            // be a green suite that measured nothing.
            eprintln!("deadline: no cgroup v2 subtree here: {e}");
            assert!(
                format!("{e}").contains("cgroup"),
                "a refusal must name what was missing: {e}"
            );
            None
        }
    }
}

/// **A run whose own thread never comes back round any loop is still ended.**
///
/// The shape: `sleep 300` is spawned, its deadline is armed, and then **nothing waits on
/// it**. There is no loop, no tick and no thread of this test's looking at it — which is
/// precisely the state the wait loop is in when it is not moving. The host's own watchdog
/// must end the run's cgroup anyway.
#[test]
fn a_run_nobody_is_waiting_on_is_still_ended_by_its_deadline() {
    let Some(host) = host() else { return };
    let id = spawn(&host, "sleep 300", false);

    // Presence first, so that the absence below is a measurement and not a boot window —
    // the seat brief's rule, and the same one `wait_scope` applies.
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

    // **Armed, and then not waited on.** This is the whole test.
    host.arm_deadline(&id, Duration::from_millis(400));

    // Long enough that a run which was going to be ended has been, and long enough that
    // an unended `sleep 300` is unmistakably still there.
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

    // **And the world agrees with the record.** Measured on `/proc`, not on the state, once
    // the reap has had its moment (see `left_after_reap`).
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

    // The firing is a record, so the claim can be checked afterwards — and it carries the
    // same reap a `job_kill` leaves, which is what makes it a fact about a cgroup rather
    // than a claim about one.
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

/// **The same, for a tree this uid may not signal.**
///
/// `sudo apt install mc` is the operator's report and `su` is its shape with the password
/// step taken out: a **root** process, blocked on a read, whose `/proc` entry is `EACCES`
/// for this daemon. Signalling it by pid is `EPERM` and is not available; `cgroup.kill` is
/// a write to a file this daemon owns, and it reaches every process the run forked
/// whatever uid they are — which is why the deadline must be enforced that way and not by
/// `kill(2)`.
#[test]
fn a_root_process_the_daemon_may_not_signal_is_ended_by_the_same_deadline() {
    let Some(host) = host() else { return };
    if !std::path::Path::new("/usr/bin/su").exists() {
        eprintln!("deadline: no /usr/bin/su on this box — nothing to measure");
        return;
    }
    // `tty: true` is the operator's own run: **its own terminal**, whose master the daemon
    // holds and writes to. That is what `su` blocks on for a password it will never be given —
    // and since the terminal is `su`'s *controlling* terminal as well, it may be `/dev/tty` it
    // opens rather than fd 0. The same block either way, and the same deadline ends it.
    // BSD `su` (macOS) takes the login before `-c`; util-linux's takes either order.
    let su = if cfg!(target_os = "macos") {
        "/usr/bin/su root -c true"
    } else {
        "/usr/bin/su -c true"
    };
    let id = spawn(&host, su, true);

    // **Presence, and the right presence.** The wrapper joins its cgroup first and `su`
    // `exec`s into it a moment later, so a check that ran on the first non-empty read would
    // be measuring the wrapper — a process of this very uid — and would be asserting about
    // a shape that was not there yet. So the wait is for *a process this uid may not look
    // at*, which is the fact the test is about.
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

/// **A run ended by something other than its own deadline says WHO ended it.**
///
/// This is the row half of the daemon's stop. `ProcessHost::end_running` is what the daemon
/// calls when it is asked to stop while a run holds its one worker — see [`RunEnder`]'s
/// counterpart in `letibot_sessionlog::registry` — and what it passes as the reason is what
/// [`JobState::Killed`] carries and what `bash` renders onto the row the session keeps.
///
/// *The command failed*, *the model killed it*, *its deadline passed* and *the daemon was
/// stopping* are four different things to have happened to one command, and a single word
/// for all four would be F5 with the sign flipped: a caller's own decision reported as the
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

/// **Can this uid look at the process?** — asked the way the daemon asks it.
///
/// `super::exec::ask`'s whole detection is `/proc/<pid>/fd/0`, and the operator's report is
/// exactly the case where that read is `EACCES`: `apt` runs as root, and the daemon is not.
/// So the fact this test needs is not *whose uid is it* — a setuid binary's **real** uid is
/// the caller's, and its effective one is root — it is *may the daemon open it*, which is
/// the same question the daemon asks and the only one that changes its behaviour.
fn may_look_at(pid: u32) -> bool {
    std::fs::read_link(format!("/proc/{pid}/fd/0")).is_ok()
}
