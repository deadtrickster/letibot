//! A background job has no wall-clock deadline. It is ended when it is DEAD — no output and no CPU
//! for the configured window — or when somebody stops it, because a fixed age cannot tell a fuzz
//! campaign from a wedged `sleep`. Each test here spawns a real run in a real cgroup: the check reads
//! the kernel's own clock (`cpu.stat`), and a fake would prove a rule about a fake.

use std::time::{Duration, Instant};

use letibot_tools::exec::{
    DEAD_KILL, DEADLINE_KILL, HostProcesses, JobId, JobState, ProcessHost, ScopeKind, SpawnRequest,
};
use letibot_tools::testing::runner_harness;
use letibot_transcript::ToolOutcome;

const WINDOW: Duration = Duration::from_millis(500);

/// One run in this host's session scope, as a backgrounded `bash` call is.
fn spawn(host: &HostProcesses, command: &str) -> JobId {
    spawn_run(host, command, false)
}

/// The same, with the operator's own `!` run's shape when `tty` is set: a person's shell.
fn spawn_run(host: &HostProcesses, command: &str, tty: bool) -> JobId {
    host.spawn(&SpawnRequest {
        command: command.to_string(),
        slug: None,
        cwd: ".".into(),
        scope: ScopeKind::Session,
        scope_name: None,
        background: true,
        env: vec![],
        tty,
        memory: None,
        env_trailer: false,
    })
    .expect("the run starts")
}

/// A host with a real cgroup tree and one short window, or `None` with the reason said out loud.
fn host(window: Duration) -> Option<HostProcesses> {
    match HostProcesses::new(std::env::temp_dir()) {
        Ok(h) => {
            h.set_dead_after(Some(window));
            Some(h)
        }
        Err(e) => {
            eprintln!("liveness: no cgroup v2 subtree here: {e}");
            assert!(
                format!("{e}").contains("cgroup"),
                "a refusal must name what was missing: {e}"
            );
            letibot_tokencore::apparatus::absent("a cgroup v2 subtree this user may write");
            None
        }
    }
}

/// Wait until `f` says so, bounded. Only ever used to wait for a decision the check takes on its
/// own clock; nothing here sleeps a window out and calls that a measurement.
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

/// A job that writes nothing and burns nothing is dead, and it is ended for that reason — not for
/// its age, and not by anything a caller asked for.
#[test]
fn a_job_stuck_in_sleep_with_no_output_is_ended_as_dead_after_the_window() {
    let Some(host) = host(WINDOW) else { return };
    let id = spawn(&host, "sleep 300");
    let ended = until(Duration::from_secs(15), || {
        host.job(&id).map(|v| !v.state.is_running()).unwrap_or(true)
    });
    let view = host.job(&id).expect("the row stays");
    assert!(
        ended,
        "a silent job outlived its own dead check: {}",
        view.state.word()
    );
    match &view.state {
        JobState::Killed { by } => assert_eq!(by, DEAD_KILL, "ended for the wrong reason"),
        other => panic!("a dead job must say so, not {}", other.word()),
    }
    // The record follows the settle and the cgroup's end, so it is waited for, not read once.
    until(Duration::from_secs(5), || {
        host.dead_ended().iter().any(|e| e.job == id)
    });
    let record = host
        .dead_ended()
        .into_iter()
        .find(|e| e.job == id)
        .expect("the check records what it ended");
    assert!(
        record.silent_for >= WINDOW,
        "ended after {:?}, before the window it was measured against",
        record.silent_for
    );
}

/// The case the whole rule exists for: a build or a campaign prints nothing between two steps and
/// burns CPU throughout, and no wall clock may end it.
#[test]
fn a_job_that_prints_nothing_but_burns_cpu_is_never_ended() {
    let Some(host) = host(WINDOW) else { return };
    let id = spawn(&host, "while :; do :; done");
    // Four windows of it, which is four times the stretch a silent job gets.
    std::thread::sleep(WINDOW * 4);
    let view = host.job(&id).expect("the row stays");
    assert!(
        view.state.is_running(),
        "a job burning CPU was ended as dead: {}",
        view.state.word()
    );
    assert!(
        host.dead_ended().iter().all(|e| e.job != id),
        "the check recorded an end for a job that never stopped working"
    );
    let _ = host.kill_job(&id);
}

/// And the same for output: a job that keeps talking is alive however long it runs.
#[test]
fn a_job_that_keeps_talking_outlives_the_window() {
    let Some(host) = host(WINDOW) else { return };
    let id = spawn(&host, "while :; do echo tick; sleep 0.2; done");
    std::thread::sleep(WINDOW * 4);
    let view = host.job(&id).expect("the row stays");
    assert!(
        view.state.is_running(),
        "a job that is printing was ended as dead: {}",
        view.state.word()
    );
    let _ = host.kill_job(&id);
}

/// The warning comes once, at four fifths of the window, and it is TAKEN — a watcher that polls says
/// it one time.
#[test]
fn the_warning_arrives_once_before_the_end() {
    let window = Duration::from_millis(1500);
    let Some(host) = host(window) else { return };
    let id = spawn(&host, "sleep 300");
    assert!(
        until(Duration::from_secs(10), || !host.dead_warnings().is_empty()),
        "a job whose window is running out is owed a warning"
    );
    let warnings = host.dead_warnings();
    assert_eq!(warnings.len(), 1, "one warning per job: {warnings:?}");
    assert_eq!(warnings[0].job, id);
    assert_eq!(warnings[0].window, window);
    assert!(
        warnings[0].silent_for * 5 >= window * 4,
        "warned at {:?}, before four fifths of {window:?}",
        warnings[0].silent_for
    );
    assert!(
        warnings[0].silent_for < window,
        "the warning arrived after the end it warns about"
    );
    assert!(host.take_dead_warning(&id).is_some(), "it is said once");
    assert!(
        host.take_dead_warning(&id).is_none(),
        "and never said twice for one job"
    );
    let _ = host.kill_job(&id);
}

/// An explicit deadline is a hard limit exactly as it was: it ends the job on its own clock, and the
/// dead check does not get a say.
#[test]
fn an_explicit_deadline_still_kills_on_time() {
    let Some(host) = host(Duration::from_secs(60)) else {
        return;
    };
    let id = spawn(&host, "sleep 300");
    host.arm_deadline(&id, Duration::from_millis(300));
    assert!(
        until(Duration::from_secs(10), || host
            .job(&id)
            .map(|v| !v.state.is_running())
            .unwrap_or(true)),
        "a job with a deadline of 300ms was still running ten seconds later"
    );
    let view = host.job(&id).expect("the row stays");
    match &view.state {
        JobState::Killed { by } => assert_eq!(by, DEADLINE_KILL, "{}", view.state.word()),
        other => panic!("a deadline ends a job as killed: {}", other.word()),
    }
    assert!(
        host.dead_ended().iter().all(|e| e.job != id),
        "the dead check ended a job somebody had already bounded"
    );
}

/// A job that has been given a deadline is that deadline's business: the dead check stops watching
/// it, which is what makes `job_extend` a way to keep a silent-but-working job alive.
#[test]
fn a_job_given_a_deadline_is_not_ended_by_the_dead_check() {
    let Some(host) = host(WINDOW) else { return };
    let id = spawn(&host, "sleep 300");
    assert_eq!(
        host.extend_deadline(&id, Duration::from_secs(30)),
        Some(Duration::from_secs(30)),
        "a running job can be bounded from now"
    );
    std::thread::sleep(WINDOW * 4);
    assert!(
        host.job(&id).map(|v| v.state.is_running()).unwrap_or(true),
        "the dead check ended a job that was told to expect longer"
    );
    assert!(host.dead_ended().iter().all(|e| e.job != id));
    let _ = host.kill_job(&id);
}

/// The operator's own run is a person's shell. A prompt nobody has typed at for half an hour looks
/// exactly like a wedged job, and ending it would be the harness closing somebody's terminal.
#[test]
fn the_operators_own_run_is_never_ended_by_the_dead_check() {
    let Some(host) = host(WINDOW) else { return };
    let id = spawn_run(&host, "sleep 300", true);
    std::thread::sleep(WINDOW * 4);
    assert!(
        host.job(&id).map(|v| v.state.is_running()).unwrap_or(true),
        "a person's own shell was ended as dead"
    );
    assert!(host.dead_ended().iter().all(|e| e.job != id));
    let _ = host.kill_job(&id);
}

/// `job_extend` end to end: it bounds a running job, the job ends at that deadline, and its result
/// says the output is partial — the whole point of the tool being reachable at all.
#[test]
fn job_extend_bounds_a_running_job_and_its_result_says_partial() {
    let mut h = match runner_harness() {
        Ok(h) => h,
        Err(e) => {
            eprintln!("liveness: no cgroup v2 subtree here: {e}");
            letibot_tokencore::apparatus::absent("a cgroup v2 subtree this user may write");
            return;
        }
    };
    let started = h.call(
        "bash",
        &serde_json::json!({"command": "sleep 300", "background": true}).to_string(),
    );
    let id = job_id_anywhere(&started.payload).expect("a job id in the body");

    let extended = h.call(
        "job_extend",
        &serde_json::json!({"job": id, "deadline": 1}).to_string(),
    );
    assert_eq!(extended.outcome, ToolOutcome::Ok, "{}", extended.render());
    assert!(
        extended.payload.contains("hard deadline 1.0s"),
        "the result names the limit it now carries: {}",
        extended.payload
    );

    let host = h.processes.clone().expect("a process host");
    let died = until(Duration::from_secs(15), || {
        host.job(&JobId(id.clone()))
            .map(|v| !v.state.is_running())
            .unwrap_or(true)
    });
    assert!(died, "the deadline `job_extend` set never came due");
    let view = host.job(&JobId(id.clone())).expect("the row stays");
    assert_eq!(
        view.state.word(),
        "killed by its deadline",
        "{}",
        view.state.word()
    );

    // And the reader is told what it has, not that the work finished.
    let out = h.call("job_output", &serde_json::json!({"job": id}).to_string());
    assert!(
        out.payload.contains("its output is partial"),
        "a killed job's window is not a finish: {}",
        out.payload
    );
    assert!(
        out.payload.contains("job_extend"),
        "and it names the way to give a running job longer: {}",
        out.payload
    );
}

/// The job id out of a backgrounded call's body, for the results whose outcome carries none.
fn job_id_anywhere(text: &str) -> Option<String> {
    let at = text.find("`j")?;
    let rest = &text[at + 1..];
    let end = rest.find('`')?;
    Some(rest[..end].to_string())
}
