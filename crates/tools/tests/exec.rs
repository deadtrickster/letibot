//! Acceptance tests for the exec substrate: the gates a command has to pass and the
//! reaper's record. A test that needs real processes asserts the behaviour, or else
//! that the refusal names what was missing and that nothing was started.

use letibot_tools::exec::{ProcessHost, ScopeKind};
use letibot_tools::testing::{runner_harness, runner_harness_with_gate};
use letibot_transcript::ToolOutcome;

/// The branch a test took, printed so a green run says which half of the world it
/// was measured against.
macro_rules! runner {
    ($name:literal) => {
        match runner_harness() {
            Ok(h) => h,
            Err(e) => {
                no_cgroups(&e, $name);
                return;
            }
        }
    };
    ($name:literal, $gate:expr) => {
        match runner_harness_with_gate($gate) {
            Ok(h) => h,
            Err(e) => {
                no_cgroups(&e, $name);
                return;
            }
        }
    };
}

fn no_cgroups(e: &letibot_tools::exec::ExecError, test: &str) {
    eprintln!("{test}: no cgroup v2 subtree here, exercising the refusal path instead: {e}");
    let msg = format!("{e}");
    assert!(
        msg.contains("nothing would reap it") || msg.contains("cgroup"),
        "a refusal must name what was missing: {msg}"
    );
    // Announced through `apparatus` so `LETIBOT_REQUIRE_APPARATUS=1` can refuse the skip; an
    // `eprintln!` and a `return` it could not see.
    letibot_tokencore::apparatus::absent("a cgroup v2 subtree this user may write");
}

// A pkill that matches the harness itself, and the pid a daemon protects.

#[test]
fn t21_1_a_pkill_matching_the_harness_is_refused_with_what_it_matches_and_the_handle_form() {
    let mut h = runner!("t21_1");
    // The harness knows its own pid and its parent chain without being told. This
    // is the pattern a model writes when it wants to stop a server it started.
    let me = std::process::id();
    let my_cmdline = std::fs::read_to_string(format!("/proc/{me}/cmdline")).unwrap_or_default();
    // Pick a token that really is in this process's command line, so the refusal
    // is about a genuine match rather than a fixture.
    let token = my_cmdline
        .split('\0')
        .flat_map(|s| s.rsplit('/'))
        .find(|s| {
            s.len() > 4
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        })
        .map(|s| s.to_string())
        .unwrap_or_else(|| "letibot".to_string());

    let r = h.call(
        "bash",
        &serde_json::json!({ "command": format!("pkill -f {token}") }).to_string(),
    );
    let body = r.render();
    assert!(
        matches!(r.outcome, ToolOutcome::Failed { .. }),
        "a self-matching pkill must not run: {body}"
    );
    assert!(body.contains("was NOT run"), "{body}");
    // What it matches is the shell that will evaluate the line, by construction.
    assert!(
        body.contains("the shell that will evaluate this command"),
        "the refusal must say WHAT it matches: {body}"
    );
    // The handle-shaped alternative, which has no pattern.
    assert!(body.contains("job_kill"), "{body}");
    assert!(body.contains("job_list"), "{body}");
    assert_eq!(h.processes.as_ref().unwrap().jobs().len(), 0);
}

#[test]
fn t21_1_the_managed_pid_is_named_with_its_reason_not_a_category() {
    let mut h = runner!("t21_1_managed");
    // What a daemon does at startup: declare the server it manages. The model has
    // no way to know this pid; the harness does.
    let host = h.processes.clone().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    let found = host.protect_listener(port, "this is the model server serving this session", true);
    if found.is_none() {
        eprintln!("t21_1_managed: /proc/<pid>/fd unreadable here; asserting on a declared pid");
        host.protect_outliving(
            std::process::id(),
            "this is the model server serving this session",
        );
    }

    let r = h.call(
        "bash",
        &serde_json::json!({ "command": "pkill -f llama-server" }).to_string(),
    );
    // The point of this case is the reason string, not the refusal: a refusal that
    // says "protected" gives the model nothing to act on.
    let _ = r;
    let protected = host.protected();
    let server = protected
        .iter()
        .find(|p| p.why.contains("model server"))
        .expect("the declared server is in the protected set");
    assert!(server.outlives_the_turn);
    assert!(protected.iter().any(|p| p.pid == std::process::id()));
}

// A waiter on the model server is a deadlock the harness can see.

#[test]
fn t21_2_a_waiter_on_the_model_server_is_refused_as_a_deadlock_the_harness_can_see() {
    let mut h = runner!("t21_2");
    let host = h.processes.clone().unwrap();
    // This process stands in for the model server: it is the one that cannot exit
    // while the session runs, which is the fact the model has no access to.
    host.protect_outliving(
        std::process::id(),
        "this is the model server serving this session — it does not exit while your \
         turn is running",
    );
    // `Reaped::observe` is `/proc/<pid>/comm` on Linux and libproc on macOS.
    let comm = letibot_tools::Reaped::observe(std::process::id()).comm;

    let cmd = format!("until pgrep -f {comm}; do sleep 2; done; echo up");
    let r = h.call("bash", &serde_json::json!({ "command": cmd }).to_string());
    let body = r.render();
    assert!(
        matches!(r.outcome, ToolOutcome::Failed { .. }),
        "a waiter on the model server must not run: {body}"
    );
    assert!(body.contains("CANNOT TERMINATE"), "{body}");
    assert!(body.contains("model server serving this session"), "{body}");
    // The remedy for a WAIT is the wait verb, not a kill.
    assert!(body.contains("job_wait"), "{body}");
    assert_eq!(h.processes.as_ref().unwrap().jobs().len(), 0);
}

#[test]
fn t21_2_a_waiter_whose_predicate_matches_its_own_shell_is_refused() {
    let mut h = runner!("t21_2_self");
    // The canonical form. `zzz-nothing-like-this` matches no process on the box,
    // except the shell that will hold this very command line, which is exactly why
    // the loop either never fires or fires at once.
    let r = h.call(
        "bash",
        &serde_json::json!({
            "command": "until pgrep -f zzz-nothing-like-this; do sleep 1; done"
        })
        .to_string(),
    );
    let body = r.render();
    assert!(matches!(r.outcome, ToolOutcome::Failed { .. }), "{body}");
    assert!(
        body.contains("the shell that will evaluate this command"),
        "{body}"
    );
    assert!(body.contains("job_wait"), "{body}");
}

#[test]
fn the_wait_verb_takes_a_handle_and_the_three_endings_are_three_outcomes() {
    let mut h = runner!("wait_verb");
    let started = h.call(
        "bash",
        &serde_json::json!({"command": "sleep 0.4; echo done", "background": true}).to_string(),
    );
    let id = job_id(&started.payload);
    // The wait ends when the run does, so this bound is a hang detector and not a claim
    // about the box.
    let ok = h.call(
        "job_wait",
        &serde_json::json!({"job": id, "timeout_ms": 30_000}).to_string(),
    );
    assert_eq!(ok.outcome, ToolOutcome::Ok, "{}", ok.render());
    assert!(ok.payload.contains("exited 0"), "{}", ok.payload);

    // The deadline passed quietly, and it is not reported as completion.
    let slow = h.call(
        "bash",
        &serde_json::json!({"command": "sleep 30", "background": true}).to_string(),
    );
    let slow_id = job_id(&slow.payload);
    let late = h.call(
        "job_wait",
        &serde_json::json!({"job": slow_id, "timeout_ms": 300}).to_string(),
    );
    assert_eq!(late.outcome, ToolOutcome::Timeout, "{}", late.render());
    assert!(late.payload.contains("STILL RUNNING"), "{}", late.payload);
    assert!(
        late.payload.contains("not a completion"),
        "{}",
        late.payload
    );

    // Nothing was ever there: absence without presence is a boot window.
    let never = h.call(
        "job_wait",
        &serde_json::json!({"scope": "explicit", "timeout_ms": 200}).to_string(),
    );
    // No explicit scope is open, so this is the unknown-scope miss, which must list
    // what there is.
    assert!(never.payload.contains("scope"), "{}", never.render());
}

#[test]
fn waiting_on_a_scope_that_never_held_anything_is_a_boot_window_and_says_so() {
    let h = runner!("boot_window");
    let host = h.processes.clone().unwrap();
    let empty = host
        .scope_for(ScopeKind::Explicit, Some("nothing-here"))
        .expect("open an explicit scope");
    let w = host
        .wait_scope(&empty, std::time::Duration::from_millis(400))
        .expect("wait");
    // The whole point: an empty cgroup at t=0 is NOT a finish.
    assert!(
        matches!(w, letibot_tools::exec::Waited::NeverStarted { .. }),
        "{w:?}"
    );
    let _ = host.end_scope(&empty);
}

// The reaper's record: what it killed, and that the processes are gone.

#[test]
fn a_scope_that_ends_records_what_it_killed_and_the_processes_are_actually_gone() {
    let h = runner!("reaper_record");
    let host = h.processes.clone().unwrap();
    let scope = host
        .scope_for(ScopeKind::Explicit, Some("reaper-test"))
        .expect("scope");

    // Start three processes that will outlive the test unless something reaps
    // them. One of them forks a child, so the record also proves the cgroup
    // caught a descendant the harness never had a handle on.
    for cmd in ["sleep 300", "sleep 300", "sh -c 'sleep 300 & sleep 300'"] {
        host.spawn(&letibot_tools::exec::SpawnRequest {
            command: cmd.into(),
            // Nobody named this run: the substrate's own test.
            slug: None,
            cwd: ".".into(),
            scope: ScopeKind::Explicit,
            scope_name: Some("reaper-test".into()),
            background: true,
            env: vec![],
            // No terminal: these are the substrate's own tests, not an operator's run.
            tty: false,
            memory: None,
        })
        .expect("spawn");
    }

    // Presence first: without this loop a later empty read would be a boot window and the test would
    // pass on a substrate that never started anything. The window detects hangs, not box speed.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut seen = 0;
    while std::time::Instant::now() < deadline {
        seen = tree_members(&host, &scope);
        if seen >= 4 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        seen >= 4,
        "the scope must be seen POPULATED before its emptiness means anything; saw {seen}"
    );

    // THEN the reap.
    let r = host.end_scope(&scope).expect("end");

    // The record is what makes the zero falsifiable.
    assert!(
        r.observed.len() >= 4,
        "the record must name every process it found: {}",
        r.summary()
    );
    assert!(
        r.observed.iter().any(|p| p.cmdline.contains("sleep")),
        "the record must be usable — pids alone identify nothing: {}",
        r.summary()
    );
    assert_eq!(
        r.mechanism,
        letibot_tools::exec::HOST_KILL,
        "{}",
        r.summary()
    );
    assert!(r.survivors.is_empty(), "survivors: {:?}", r.survivors);
    assert!(
        r.removed,
        "the cgroup directory must be gone: {}",
        r.summary()
    );
    assert!(r.clean());

    // ABSENCE, measured on the world and not on the record: the pids are gone
    // from /proc. A record that said "killed" while the processes ran would be
    // exactly the unfalsifiable zero this exists to prevent.
    for p in &r.observed {
        assert!(
            std::fs::metadata(format!("/proc/{}", p.pid)).is_err()
                || std::fs::read_to_string(format!("/proc/{}/stat", p.pid))
                    .map(|s| s.contains(") Z "))
                    .unwrap_or(true),
            "pid {} is still alive after its scope ended",
            p.pid
        );
    }

    // And the log holds it, so `job_list` can show it later.
    let log = host.reap_log();
    assert_eq!(log.len(), 1);
    assert!(
        log[0].summary().contains("observed"),
        "{}",
        log[0].summary()
    );
}

#[test]
fn an_empty_scope_and_a_reaper_that_did_nothing_are_different_records() {
    let h = runner!("two_zeroes");
    let host = h.processes.clone().unwrap();
    let empty = host
        .scope_for(ScopeKind::Explicit, Some("never-used"))
        .expect("scope");
    let r = host.end_scope(&empty).expect("end");
    assert_eq!(r.mechanism, "nothing to kill");
    assert!(r.summary().contains("observed 0"), "{}", r.summary());
    // It is `clean`, and that is honest - but the summary says WHY it is clean,
    // which is the difference between a zero from vigilance and a zero from a
    // mechanism.
    assert!(r.clean());
}

#[test]
fn the_reap_log_reaches_the_model_through_job_list() {
    let mut h = runner!("reap_log_visible");
    let before = h.call("job_list", "{}");
    assert!(
        before.payload.contains("reaped nothing yet"),
        "a session that has reaped nothing must say so rather than showing an \
         empty section: {}",
        before.payload
    );

    let started = h.call(
        "bash",
        // `; :` keeps the shell beside `sleep` on every `sh`: macOS's is bash, which
        // would otherwise `exec` it, and the record would be one process caught mid-exec.
        &serde_json::json!({"command": "sleep 60; :", "background": true}).to_string(),
    );
    let id = job_id(&started.payload);
    // Kill once `sleep` exists, not before: a kill that lands while the shell is still
    // forking records one process, and the count below is about the record, not the race.
    let host = h.processes.clone().unwrap();
    let job = letibot_tools::JobId(id.clone());
    // Thirty seconds is a hang detector rather than a bet on the box: the count
    // assertion below is about the record, so it must not fail on a race with a slow
    // fork.
    let until = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while host.job_pids(&job).len() < 2 && std::time::Instant::now() < until {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let killed = h.call("job_kill", &serde_json::json!({"job": id}).to_string());
    assert_eq!(killed.outcome, ToolOutcome::Ok, "{}", killed.render());
    assert!(killed.payload.contains("sleep 60"), "{}", killed.payload);
    assert!(
        killed
            .payload
            .contains(&format!("mechanism: {}", letibot_tools::exec::HOST_KILL)),
        "{}",
        killed.payload
    );
    assert!(
        killed.payload.contains("survivors after: none"),
        "{}",
        killed.payload
    );

    let after = h.call("job_list", "{}");
    // The kill ends the job's own cgroup, which is one reaping in the log: the listing has to
    // count it rather than say the session has reaped nothing.
    assert!(
        after.payload.contains("1 scope has ended this session"),
        "{}",
        after.payload
    );
    assert!(after.payload.contains("observed 2"), "{}", after.payload);
    // The record has to be USABLE later: a pid list from an hour ago identifies
    // nothing, so the command line travels with it.
    assert!(after.payload.contains("sleep"), "{}", after.payload);
}

// The two gates: an adjudicator, and a backend that cannot exec.

#[test]
fn with_no_adjudicator_bash_is_not_run_and_it_is_not_denied() {
    let mut h = runner!("gate", None);
    let r = h.call(
        "bash",
        &serde_json::json!({"command": "echo hi"}).to_string(),
    );
    match &r.outcome {
        // `NotRun`, because nobody decided. `Denied` would claim a decision was made.
        ToolOutcome::NotRun { why } => {
            assert!(why.contains("exec"), "{why}");
            assert!(why.contains("not a denial"), "{why}");
        }
        other => panic!("bash must not run unadjudicated: {other:?}"),
    }
    assert_eq!(h.processes.as_ref().unwrap().jobs().len(), 0);
}

#[test]
fn a_session_whose_backend_cannot_exec_refuses_at_the_backend_naming_which() {
    // The second mechanism, independent of the gate: the coder harness has an
    // allowing adjudicator and a writable-but-not-executable backend.
    let mut h = letibot_tools::testing::writable_harness();
    // `bash` is not even registered there, which is the third mechanism: the role.
    // That refusal is the unknown-tool path and must list what is.
    let r = h.call(
        "bash",
        &serde_json::json!({"command": "echo hi"}).to_string(),
    );
    assert!(
        r.payload.contains("this session has these tools"),
        "{}",
        r.render()
    );
    // The nearest-tool hint names `bash` (it is the name the model asked for), so
    // "bash is absent" is asserted on the refusal reason, not the whole payload.
    match &r.outcome {
        ToolOutcome::Failed { reason } => {
            assert!(reason.contains("no tool called `bash`"), "{reason}");
        }
        other => panic!("bash must be an unknown tool: {other:?}"),
    }
}

#[test]
fn a_coder_asking_for_bash_is_told_how_its_test_gets_run() {
    // The coder seat has no shell, so the miss for `bash` is the moment a model learns its test
    // has no runner here. The refusal has to say how the test does get run, and must not name a
    // cause it cannot see.
    let mut h = letibot_tools::testing::writable_harness();
    let r = h.call(
        "bash",
        &serde_json::json!({"command": "cargo test --offline"}).to_string(),
    );
    let payload = r.payload.clone();
    assert!(
        payload.contains("does not seat it"),
        "the miss must say the seat lacks it: {payload}"
    );
    assert!(
        payload.contains("nothing was run"),
        "the miss must say nothing ran: {payload}"
    );
    assert!(
        payload.contains("No shell command can be run"),
        "the miss must say what it cannot do, which is run a shell command: {payload}"
    );
    // A seat with no `bash` still starts processes through its own tools - `task` is a child
    // session, `plugin build` runs cargo - so the seat-wide claim was false and is gone.
    assert!(
        !payload.contains("starts no process"),
        "the miss must not claim the seat starts no process: {payload}"
    );
    assert!(
        payload.contains("on your board with `todo`"),
        "the miss must name the tool that carries the command: {payload}"
    );
    assert!(
        payload.contains("cargo test --offline"),
        "the miss must show the command's shape: {payload}"
    );
    // No cause: an unknown name, `--bash` off and plan mode all land in this arm, so the
    // text must not claim which one this is.
    assert!(
        !payload.contains("--bash"),
        "the miss must not claim a cause it cannot see: {payload}"
    );

    // A name nothing knows gets the list and no runner advice: there is no test to run.
    let unknown = h.call("not_a_tool", "{}");
    let other = unknown.payload.clone();
    assert!(other.contains("this session has these tools"), "{other}");
    for phrase in [
        "does not seat it",
        "No shell command can be run",
        "nothing was run",
    ] {
        assert!(
            !other.contains(phrase),
            "`not_a_tool` must get no exec guidance, but it says {phrase:?}: {other}"
        );
    }
}

#[test]
fn the_exec_surface_of_each_role_is_exactly_what_was_decided() {
    use letibot_tools::runtime::roles;
    let reg = letibot_tools::runner_tools(std::sync::Arc::new(
        letibot_tools::builtins::retrieval::Unavailable,
    ))
    .unwrap();
    // The runner seats twelve; the ceiling is 17 for leticode's tool union and `recall`,
    // so this role is under it rather than over.
    let role = roles::m2_runner();
    assert_eq!(role.max_tools, 12, "the runner's own number is 12");
    assert_eq!(
        letibot_tools::runtime::DEFAULT_MAX_TOOLS,
        17,
        "the ceiling holds leticode's tool union and recall"
    );
    for other in [
        roles::m1_orchestrator(),
        roles::m2_coder(),
        roles::planner(),
    ] {
        assert_eq!(
            other.max_tools,
            letibot_tools::runtime::DEFAULT_MAX_TOOLS,
            "`{}` must still be at the default ceiling",
            other.name
        );
    }
    let seated = reg.resolve_role(&role).unwrap();
    assert_eq!(seated.len(), 12);
    assert!(seated.names().contains(&"bash".to_string()));
    assert!(seated.names().contains(&"monitor".to_string()));
    assert!(seated.names().contains(&"fanout".to_string()));
    assert!(seated.names().contains(&"scenario".to_string()));

    // The orchestrator names no exec tool at all, and that has not moved.
    for name in EXEC_TOOLS {
        assert!(
            !roles::m1_orchestrator().tools.contains(&name.to_string()),
            "`m1_orchestrator` gained `{name}` — an exec path must arrive by decision, \
             not by a sibling workstream"
        );
    }

    // `coder` names `bash` by decision, and this is an exact set rather than a
    // contains-check: a role that quietly grew `job_kill` alongside it would pass the
    // latter, which is precisely what this file exists to refuse.
    let coder = roles::m2_coder();
    let coder_exec: Vec<&str> = EXEC_TOOLS
        .iter()
        .copied()
        .filter(|n| coder.tools.contains(&n.to_string()))
        .collect();
    assert_eq!(
        coder_exec,
        vec!["bash"],
        "`m2_coder`'s exec surface is one decision wide. Anything else here arrived \
         without one"
    );

    // The seat is only the NAME of a permission: whether a session gets the tool is
    // `--bash` on the daemon, which strips it back off when absent. The two are
    // deliberately separate, and `crates/harnessd` tests its half.
}

/// Named once, because a list that is retyped per assertion is a list that drifts.
const EXEC_TOOLS: &[&str] = &[
    "bash",
    "job_kill",
    "job_wait",
    "job_list",
    "job_output",
    "monitor",
    // `fanout` and `scenario` are exec surface too: a seat that cannot run a command
    // here has no business running one on three machines.
    "fanout",
    "scenario",
];

// Output caps, denominators, and what an empty answer means.

#[test]
fn a_capped_result_says_it_capped_and_job_output_has_the_rest() {
    let mut h = runner!("cap");
    let r = h.call(
        "bash",
        &serde_json::json!({
            // Respelled with `seq`: `$i` and `$((i+1))` are values that do not exist
            // until the shell runs, so the normaliser refuses that command before it
            // runs. Same volume, and this test is about the output cap.
            "command": "seq 0 2999 | sed 's/^/line /; s/$/ padding padding padding/'"
        })
        .to_string(),
    );
    let rendered = r.render();
    assert_eq!(r.outcome, ToolOutcome::Ok, "{rendered}");
    // The cap is stated, WITH its denominator, and with the call that gets the rest.
    assert!(rendered.contains("capped inline"), "{rendered}");
    assert!(rendered.contains("of"), "{rendered}");
    assert!(rendered.contains("job_output"), "{rendered}");
    // The tail survived, not the head.
    assert!(
        r.payload.contains("line 2999"),
        "shell output keeps the tail"
    );

    // And the rest really is reachable.
    let id = job_id_from_note(&rendered).expect("the cap note names the job");
    let head = h.call(
        "job_output",
        &serde_json::json!({"job": id, "offset": 0, "limit": 2000}).to_string(),
    );
    assert_eq!(head.outcome, ToolOutcome::Ok, "{}", head.render());
    assert!(head.payload.contains("line 0 "), "{}", head.payload);
    assert!(
        head.payload.contains("of"),
        "the denominator travels: {}",
        head.payload
    );
}

#[test]
fn a_job_that_wrote_nothing_abstains_rather_than_returning_an_empty_answer() {
    let mut h = runner!("empty_output");
    let r = h.call("bash", &serde_json::json!({"command": "true"}).to_string());
    let id = job_id_anywhere(&r.render())
        .unwrap_or_else(|| h.processes.as_ref().unwrap().jobs()[0].id.0.clone());
    let out = h.call("job_output", &serde_json::json!({"job": id}).to_string());
    // Not `ok` with an empty body: `0 bytes` is a claim, and this one is about a
    // process that finished having written nothing.
    assert!(
        matches!(out.outcome, ToolOutcome::Abstained { .. }),
        "{}",
        out.render()
    );
    assert!(
        out.render().contains("wrote nothing at all"),
        "{}",
        out.render()
    );
}

#[test]
fn a_non_zero_exit_is_the_commands_answer_and_not_a_harness_failure() {
    let mut h = runner!("exit_code");
    let r = h.call(
        "bash",
        &serde_json::json!({"command": "echo nope >&2; exit 3"}).to_string(),
    );
    assert_eq!(r.outcome, ToolOutcome::Ok, "{}", r.render());
    assert!(r.payload.contains("[exit 3]"), "{}", r.payload);
    assert!(
        r.payload.contains("nope"),
        "stderr is captured: {}",
        r.payload
    );
}

#[test]
fn an_unknown_job_comes_back_with_the_jobs_there_are() {
    let mut h = runner!("unknown_job");
    // Zero jobs: the miss must say the table is empty, not merely that the id is
    // absent. A denominator of zero is a failed scope, never an answer.
    let cold = h.call(
        "job_output",
        &serde_json::json!({"job": "j999"}).to_string(),
    );
    assert!(cold.payload.contains("started 0 jobs"), "{}", cold.render());

    h.call("bash", &serde_json::json!({"command": "true"}).to_string());
    let warm = h.call(
        "job_output",
        &serde_json::json!({"job": "j999"}).to_string(),
    );
    // One job in the table is said as `1 job`, not `1 jobs`: the count is in words.
    assert!(
        warm.render().contains("no job called `j999`"),
        "{}",
        warm.render()
    );
    assert!(
        warm.payload.contains("this session has 1 job:"),
        "{}",
        warm.render()
    );
}

#[test]
fn a_foreground_command_dies_with_its_turn_and_a_background_one_does_not() {
    let mut h = runner!("scopes");
    let fg = h.call("bash", &serde_json::json!({"command": "true"}).to_string());
    assert_eq!(fg.outcome, ToolOutcome::Ok, "{}", fg.render());
    let bg = h.call(
        "bash",
        &serde_json::json!({"command": "sleep 30", "background": true}).to_string(),
    );
    assert!(bg.payload.contains("session"), "{}", bg.payload);
    assert!(
        bg.payload.contains("reaped when this session ends"),
        "a background job must be told what will reap it: {}",
        bg.payload
    );

    let host = h.processes.clone().unwrap();
    let turn = host.scope_for(ScopeKind::Turn, None).expect("turn scope");
    let r = host.end_scope(&turn).expect("end turn");
    // The turn scope is a CHILD of the session scope, so ending it must not touch
    // the background job.
    let bg_id = job_id(&bg.payload);
    let still = host
        .job(&letibot_tools::exec::JobId(bg_id.clone()))
        .expect("the background job is still known");
    assert!(
        still.state.is_running(),
        "ending a turn reaped a session-scoped job: {} / {}",
        still.state.word(),
        r.summary()
    );
}

#[test]
fn an_explicit_scope_must_be_named_and_says_that_it_outlives_the_session() {
    let mut h = runner!("explicit");
    let unnamed = h.call(
        "bash",
        &serde_json::json!({"command": "sleep 5", "scope": "explicit"}).to_string(),
    );
    assert!(
        matches!(unnamed.outcome, ToolOutcome::Failed { .. }),
        "{}",
        unnamed.render()
    );
    assert!(
        unnamed.payload.contains("scope_name"),
        "{}",
        unnamed.payload
    );

    let named = h.call(
        "bash",
        &serde_json::json!({
            "command": "sleep 30", "background": true,
            "scope": "explicit", "scope_name": "the-comparison"
        })
        .to_string(),
    );
    assert!(named.payload.contains("SURVIVES"), "{}", named.payload);

    let host = h.processes.clone().unwrap();
    let sid = host
        .scope_for(ScopeKind::Explicit, Some("the-comparison"))
        .unwrap();
    // The placement is the rule: an explicit scope is a SIBLING of the session's,
    // so ending the session cannot reap it.
    let session = host.scope_for(ScopeKind::Session, None).unwrap();
    assert!(
        !sid.path.starts_with(&session.path),
        "an explicit scope under the session would be reaped by it: {:?} in {:?}",
        sid.path,
        session.path
    );
    let _ = host.end_scope(&sid);
}

#[test]
fn a_bad_cwd_is_a_miss_with_the_listing_and_nothing_runs() {
    let mut h = runner!("cwd");
    let r = h.call(
        "bash",
        &serde_json::json!({"command": "ls", "cwd": "src/parser"}).to_string(),
    );
    assert!(matches!(r.outcome, ToolOutcome::Failed { .. }));
    assert!(r.render().contains("src/"), "{}", r.render());
    assert_eq!(h.processes.as_ref().unwrap().jobs().len(), 0);
}

// Housekeeping: a session that ends leaves nothing behind.

/// A session that ends leaves no cgroup directory behind, not merely no process:
/// an empty cgroup holds nothing and is still one more row somebody has to decide
/// about.
#[test]
fn a_session_that_ends_leaves_no_empty_cgroup_directory_either() {
    let h = runner!("housekeeping");
    let host = h.processes.clone().unwrap();
    let session = host.scope_for(ScopeKind::Session, None).expect("session");
    let root = session.path.parent().unwrap().to_path_buf();
    assert!(
        root.exists(),
        "the harness root cgroup must exist while a session does"
    );

    host.spawn(&letibot_tools::exec::SpawnRequest {
        command: "true".into(),
        // Nobody named this run: the substrate's own test.
        slug: None,
        cwd: ".".into(),
        scope: ScopeKind::Turn,
        scope_name: None,
        background: false,
        env: vec![],
        // No terminal: these are the substrate's own tests, not an operator's run.
        tty: false,
        memory: None,
    })
    .expect("spawn");

    drop(h);
    drop(host);

    assert!(
        !root.exists(),
        "the harness root cgroup `{}` outlived its session — an empty cgroup holds \
         no process and is still one more row somebody has to decide about",
        root.display()
    );
}

// Helpers: scope membership, and a job id out of a result body.

fn tree_members(
    host: &letibot_tools::exec::HostProcesses,
    scope: &letibot_tools::exec::ScopeId,
) -> usize {
    // The tree's own reading of membership: the file the reaper reads on Linux, the
    // live members of the recorded groups on macOS. See `scope::live_members`.
    let _ = host;
    letibot_tools::exec::live_members(scope).len()
}

/// The job id out of a `bash` result body.
fn job_id(payload: &str) -> String {
    job_id_anywhere(payload).expect("a bash result always names its job")
}

fn job_id_anywhere(text: &str) -> Option<String> {
    let at = text.find("`j")?;
    let rest = &text[at + 1..];
    let end = rest.find('`')?;
    Some(rest[..end].to_string())
}

fn job_id_from_note(text: &str) -> Option<String> {
    let at = text.find("job=\"")?;
    let rest = &text[at + 5..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

// The operator's own `!` shell line, which the daemon runs through the ungated
// half of the runtime (`invoke_operator`): no gate call appears, and everything
// else is identical to a model's call.

/// A gate that refuses everything and counts every way it was consulted: the
/// assertion is not that the call succeeded (a permissive gate gives that too) but
/// that the gate has no calls on its books at all.
struct Counting {
    admits: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    grants: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl letibot_tools::runtime::Gate for Counting {
    fn admit(
        &mut self,
        _call: &letibot_tools::runtime::GateCall<'_>,
    ) -> letibot_tools::runtime::GateDecision {
        self.admits
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        // Refuse, so the test cannot pass because a permissive gate let the call
        // through before anybody counted: if this answer ever reaches a `!` line,
        // the row says NotRun and the assertion on the outcome fails first.
        letibot_tools::runtime::GateDecision::refuse(letibot_transcript::ToolOutcome::NotRun {
            why: "the counting gate refuses".into(),
        })
    }

    fn grant_view(
        &mut self,
        _path: &std::path::Path,
        _tool: &str,
    ) -> letibot_tools::runtime::ViewGrant {
        self.grants
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        letibot_tools::runtime::ViewGrant::NotAsked
    }

    fn describe(&self) -> String {
        "counting gate (refuses, and counts)".into()
    }
}

/// The contrast case first, because it is what makes the other test mean anything: a
/// MODEL's `bash` call goes through `invoke`, so the gate is consulted exactly once
/// and its refusal stops the call.
#[test]
fn a_model_bash_call_consults_the_gate_and_is_stopped_by_it() {
    let admits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let grants = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let gate = Box::new(Counting {
        admits: admits.clone(),
        grants: grants.clone(),
    });
    let mut h = runner!("model bash gate", Some(gate));
    let r = h.call(
        "bash",
        &serde_json::json!({"command": "echo hi"}).to_string(),
    );
    assert!(
        matches!(r.outcome, ToolOutcome::NotRun { .. }),
        "the counting gate must stop a model call: {}",
        r.render()
    );
    assert_eq!(
        admits.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "a model's bash call consults the gate exactly once"
    );
}

/// An operator's own `!` line runs the command and consults nothing: the gate counts
/// both ways it can be reached, and both counters must stay at zero while the
/// command's own output lands in the row.
#[test]
fn an_operators_own_bash_line_runs_and_no_gate_call_appears() {
    let admits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let grants = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let gate = Box::new(Counting {
        admits: admits.clone(),
        grants: grants.clone(),
    });
    let mut h = runner!("operator bash gate", Some(gate));
    let call = letibot_transcript::ToolCall {
        id: "bang-1".into(),
        name: "bash".into(),
        arguments: serde_json::json!({"command": "printf 'operator-ran-this'"}).to_string(),
    };
    let r = h.rt.invoke_operator("", &call, &mut h.sink);
    assert!(
        matches!(r.outcome, ToolOutcome::Ok),
        "the operator's own command must run: {}",
        r.render()
    );
    assert!(
        r.render().contains("operator-ran-this"),
        "the row carries the command's own output: {}",
        r.render()
    );
    assert_eq!(
        admits.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "an operator's own command consulted the gate's admit"
    );
    assert_eq!(
        grants.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "an operator's own command raised a view-grant ask"
    );
}

/// The operator's own run gets a pty and a model's call does not, asserted on what the
/// command itself can see (`[ -t 1 ]` is `ls`'s own question) rather than on the
/// request, because that is the property that has to be true.
#[test]
fn the_operators_own_run_gets_a_terminal_and_a_models_call_does_not() {
    let mut h = runner!("operator tty");
    let question = "if [ -t 1 ]; then echo ON-A-TERMINAL; else echo ON-A-PIPE; fi";

    let call = letibot_transcript::ToolCall {
        id: "bang-tty".into(),
        name: "bash".into(),
        arguments: serde_json::json!({ "command": question }).to_string(),
    };
    let theirs = h.rt.invoke_operator("", &call, &mut h.sink);
    assert!(
        matches!(theirs.outcome, ToolOutcome::Ok),
        "the operator's own command must run: {}",
        theirs.render()
    );
    assert!(
        theirs.render().contains("ON-A-TERMINAL"),
        "the operator's own run must see a terminal, or `ls --color=auto` stays plain: {}",
        theirs.render()
    );

    // The contrast: the same command, the same runtime, the model's entry.
    let mine = h.call(
        "bash",
        &serde_json::json!({ "command": question }).to_string(),
    );
    assert!(
        mine.render().contains("ON-A-PIPE"),
        "a model's call must keep its pipe: {}",
        mine.render()
    );
}

/// The operator's own run is a job-control shell: `$-` carries `m` and `/dev/tty`
/// opens, so this pty is its controlling terminal. `(exec 9</dev/tty)` and not
/// `[ -r /dev/tty ]`, which answers yes without a controlling terminal.
#[test]
fn the_operators_own_run_is_a_job_control_shell_and_its_two_bash_lines_are_gone() {
    let mut h = runner!("operator job control");
    // An exact line and not `contains`: `NO-MONITOR` contains `MONITOR`, so the obvious
    // spelling of the assertion below would pass on a run with no job control.
    fn said(body: &str, word: &str) -> bool {
        body.lines().any(|l| l.trim() == word)
    }
    let question = concat!(
        "case \"$-\" in *m*) echo MONITOR;; *) echo NO-MONITOR;; esac; ",
        "if (exec 9</dev/tty) 2>/dev/null; then echo HAS-CTTY; else echo NO-CTTY; fi"
    );
    let call = letibot_transcript::ToolCall {
        id: "bang-jobctl".into(),
        name: "bash".into(),
        arguments: serde_json::json!({ "command": question }).to_string(),
    };

    // The operator's own run.
    let theirs = h.rt.invoke_operator("", &call, &mut h.sink);
    assert!(
        matches!(theirs.outcome, ToolOutcome::Ok),
        "the operator's own command must run: {}",
        theirs.render()
    );
    let body = theirs.render();
    assert!(
        said(&body, "MONITOR"),
        "job control must be ON for the operator's own run, or `jobs`, `^Z`, `fg` and `bg` are \
         all still absent: {body}"
    );
    assert!(
        said(&body, "HAS-CTTY"),
        "the operator's run must have THIS pty as its controlling terminal — that is the cause \
         `MONITOR` is the symptom of: {body}"
    );
    // The two sentences, which follow from the two facts above rather than being filtered out.
    assert!(
        !body.contains("job control in this shell"),
        "bash's own `no job control` line reached the row: {body}"
    );
    assert!(
        !body.contains("cannot set terminal process group"),
        "bash's own `cannot set terminal process group` line reached the row: {body}"
    );

    // The control: a model's call has no terminal at all.
    let pty_probe = concat!(
        "if [ -t 0 ]; then echo STDIN-TTY; else echo STDIN-NOT-TTY; fi; ",
        "if [ -t 1 ]; then echo STDOUT-TTY; else echo STDOUT-NOT-TTY; fi"
    );
    let mine = h.call(
        "bash",
        &serde_json::json!({ "command": pty_probe }).to_string(),
    );
    let mine = mine.render();
    assert!(
        said(&mine, "STDIN-NOT-TTY") && said(&mine, "STDOUT-NOT-TTY"),
        "a model's call must have no terminal on either descriptor, so `MONITOR` above is about \
         the operator's path: {mine}"
    );
}

/// The operator's own case, against the program they reported it about: `ls` itself
/// writes the SGR on their run, and a model's payload must carry no escape at all.
/// The head's half, whether the row draws it, is `letibot-tui`'s.
#[test]
fn the_operators_own_ls_colours_because_its_output_is_a_terminal() {
    let mut h = runner!("operator ls colour");
    let ask = |id: &str| letibot_transcript::ToolCall {
        id: id.into(),
        name: "bash".into(),
        // GNU `ls` colours on `--color=auto`; BSD `ls` (macOS) ignores it and takes `-G`.
        arguments: serde_json::json!({
            "command": if cfg!(target_os = "macos") {
                // It colours only on a terminal it knows, so it is given a `TERM`; the
                // terminal, not the variable, is what turns the colour on.
                "TERM=xterm-256color ls -G"
            } else {
                // GNU `ls` also reads `TERM` before it colours, and a run inherits whatever
                // the environment says: a systemd user service can carry a `TERM` that `ls`
                // does not count as a colour terminal.
                "TERM=xterm-256color ls --color=auto"
            }
        })
        .to_string(),
    };
    let theirs = h.rt.invoke_operator("", &ask("bang-ls"), &mut h.sink);
    assert!(
        matches!(theirs.outcome, ToolOutcome::Ok),
        "the operator's own command must run: {}",
        theirs.render()
    );
    assert!(
        theirs.render().contains('\u{1b}'),
        "`ls --color=auto` must colour on the operator's own run, or part A bought nothing: {}",
        theirs.render()
    );
    // The same command on the model's entry, and it is plain: the control that makes the
    // assertion above mean the pty rather than this `ls` always colours.
    let mine = h.call(
        "bash",
        &serde_json::json!({ "command": "ls --color=auto" }).to_string(),
    );
    assert!(
        !mine.render().contains('\u{1b}'),
        "a model's payload must stay plain: {}",
        mine.render()
    );
}

// A command that wants the terminal: the pty that makes `ls` colour is a capture, so a
// program that takes the screen draws into one row and waits for a keystroke that cannot
// arrive. `exec::terminal` is the rule; these are the wiring.

/// The operator's own `!` line is refused, by name, with the sentence: the rule is the
/// first thing `bash` decides, so a session whose backend cannot start a process still
/// gets it.
#[test]
fn an_interactive_program_on_the_operators_own_line_is_refused_by_name_with_the_sentence() {
    let mut h = letibot_tools::testing::text_only_runner_harness();
    let bang = |id: &str, command: &str| letibot_transcript::ToolCall {
        id: id.into(),
        name: "bash".into(),
        arguments: serde_json::json!({ "command": command }).to_string(),
    };

    // Refused, and said: the assertions are on the text - the program's name, that
    // nothing ran, what the run actually is, and what to do instead.
    let r =
        h.rt.invoke_operator("", &bang("bang-nano", "nano /etc/fstab"), &mut h.sink);
    assert!(
        matches!(r.outcome, ToolOutcome::NotRun { .. }),
        "`nano` on the operator's own line must be refused and must not run: {:?}",
        r.outcome
    );
    let body = r.render();
    assert!(
        body.contains("nano"),
        "the refusal must name the program: {body}"
    );
    assert!(
        body.contains("needs the terminal and letibot cannot hand you one"),
        "the refusal must be the operator's sentence: {body}"
    );
    // The remedy names the verb that works: `!term` runs the program in a pane, rather
    // than sending the operator out of the harness.
    assert!(
        body.contains("`!term <command>`"),
        "the refusal must say what to do instead: {body}"
    );
    assert!(
        body.contains("The command was NOT run"),
        "the row must say that nothing ran: {body}"
    );
    assert!(
        body.contains("ONE transcript row") && body.contains("/dev/null"),
        "the row must say what the operator's own run actually is: {body}"
    );

    // `sudo ls /root` must reach the backend: this harness has no process host, so the
    // reason it is refused is the backend's and not this rule's.
    let ordinary =
        h.rt.invoke_operator("", &bang("bang-ls", "sudo ls /root"), &mut h.sink);
    let text = ordinary.render();
    assert!(
        !text.contains("cannot hand you one"),
        "`sudo ls /root` is the operator's own example and must not be refused by the \
         terminal rule: {text}"
    );

    // A pipeline is judged member by member.
    let piped =
        h.rt.invoke_operator("", &bang("bang-less", "ls | less"), &mut h.sink);
    assert!(
        matches!(piped.outcome, ToolOutcome::NotRun { .. }),
        "`ls | less` must be refused for the `less`: {:?}",
        piped.outcome
    );
    assert!(
        piped.render().contains("`less` needs the terminal"),
        "and the refusal must name the pipeline member that wants it: {}",
        piped.render()
    );
}

/// The rule is not consulted on a model's call, and that is a decision: a model has no
/// pty, so `nano` reads EOF and exits, and a rule that fired on both would tell the
/// model to run it in a pane - advice a model cannot take.
#[test]
fn the_terminal_rule_does_not_fire_on_a_models_call() {
    let mut h = letibot_tools::testing::text_only_runner_harness();
    let mine = h.call(
        "bash",
        &serde_json::json!({ "command": "nano /etc/fstab" }).to_string(),
    );
    let text = mine.render();
    assert!(
        !text.contains("cannot hand you one"),
        "a model's call keeps its pipe and must not be refused by the terminal rule: {text}"
    );
    // It still does not run, and the reason is the one that applies here: this harness
    // has no process host. The contrast is the point - a different reason, from a
    // different guard.
    assert!(
        matches!(mine.outcome, ToolOutcome::Failed { .. }),
        "the backend's own refusal is what a model's call gets here: {text}"
    );
}

/// The end-to-end version, against the real substrate: on a session that can run
/// commands `! nano` is refused with the sentence and nothing is started, while `! ls`
/// on the same entry runs.
#[test]
fn on_a_real_substrate_the_operators_nano_is_refused_and_their_ls_still_runs() {
    let mut h = runner!("operator terminal rule");
    let bang = |id: &str, command: &str| letibot_transcript::ToolCall {
        id: id.into(),
        name: "bash".into(),
        arguments: serde_json::json!({ "command": command }).to_string(),
    };

    let refused =
        h.rt.invoke_operator("", &bang("bang-nano", "nano /etc/fstab"), &mut h.sink);
    assert!(
        matches!(refused.outcome, ToolOutcome::NotRun { .. }),
        "`nano` must not run on the operator's own line: {}",
        refused.render()
    );
    assert!(
        refused.render().contains("`!term <command>`"),
        "and it must say what to do instead: {}",
        refused.render()
    );

    // The control that makes the refusal about the command rather than about `bash`
    // being broken on this entry: the ordinary command still runs, and its own bytes
    // are in the row.
    let ran =
        h.rt.invoke_operator("", &bang("bang-echo", "printf 'ordinary-ran'"), &mut h.sink);
    assert!(
        matches!(ran.outcome, ToolOutcome::Ok),
        "an ordinary command must still run: {}",
        ran.render()
    );
    assert!(
        ran.render().contains("ordinary-ran"),
        "and its output must be in the row: {}",
        ran.render()
    );
}

/// The view-grant ask specifically: `grant_view` fires when a command's output names a
/// path the confinement hid, so this needs a confined harness. The ungated path takes
/// none and the row keeps the absence note `bash` already wrote.
#[test]
fn an_operators_own_command_does_not_ask_to_grant_a_path_into_the_view() {
    let admits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let grants = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let gate = Box::new(Counting {
        admits: admits.clone(),
        grants: grants.clone(),
    });
    let mut h = match letibot_tools::testing::confined_harness_with_gate(Some(gate), |root| {
        letibot_tools::exec::Bwrap::project(root).map(|b| Box::new(b) as _)
    }) {
        Ok(h) => h,
        Err(e) => {
            // The confined substrate needs a usable unprivileged namespace boundary, and a
            // host without one is a real host. The refusal is asserted rather than skipped,
            // for the same reason `runner!`'s is.
            eprintln!(
                "an_operators_own_command_does_not_ask: no boundary here, so there is \
                 no view to grant into: {e}"
            );
            assert!(
                format!("{e}").contains("namespace") || format!("{e}").contains("bwrap"),
                "a refusal must name what was missing: {e}"
            );
            return;
        }
    };
    let outside =
        std::path::PathBuf::from(format!("/opt/letibot-bang-outside-{}", std::process::id()));
    // A command whose OUTPUT names a path the boundary hid; `outside_paths` classifies
    // lexically, so the path need not exist. `/opt` is past every bound and replaced
    // root; `/tmp` is a replaced root, whose absence raises no ask.
    let command = format!("ls {d}/gone.txt 2>&1; true", d = outside.display());
    let call = letibot_transcript::ToolCall {
        id: "bang-2".into(),
        name: "bash".into(),
        arguments: serde_json::json!({ "command": command }).to_string(),
    };
    let r = h.rt.invoke_operator("", &call, &mut h.sink);
    assert!(
        matches!(r.outcome, ToolOutcome::Ok),
        "the operator's own command must run: {}",
        r.render()
    );
    let said = r.render();
    assert!(
        said.contains("No such file or directory"),
        "the command's own output is the finding: {said}"
    );
    // The absence is still NAMED on the row - bash's own note, not a gate question.
    assert!(
        said.contains("not in this session's filesystem view")
            || said.contains("outside this session's view")
            || said.contains("filesystem view"),
        "the boundary's finding must stay on the row now the ask is gone: {said}"
    );
    assert_eq!(
        grants.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "an operator's own command raised a view-grant ask"
    );
    assert_eq!(
        admits.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "an operator's own command consulted the gate's admit"
    );
    let _ = outside;
}

// The world their console gives it: their `~/.bashrc` aliases `ls` to `--color=auto`,
// and an alias is shell state that no `/bin/sh -c` reads. `exec::console` carries the
// decision; these are the wiring, against a real process.

/// The operator's own line is read by their shell, so their alias is their command.
/// The console's rc is a file, so `HOME` on the standing environment is the whole
/// wiring, and the model's entry is the control.
#[test]
fn the_operators_own_run_reads_their_rc_so_their_alias_is_their_command() {
    let mut h = runner!("operator rc");
    let home = h.root().to_path_buf();
    std::fs::write(
        home.join(".bashrc"),
        "alias letibot-rc-probe='printf ALIAS-APPLIED'\n",
    )
    .expect("write the console's rc");
    h.processes
        .as_ref()
        .unwrap()
        .set_standing_env(vec![("HOME".to_string(), home.display().to_string())]);

    let call = letibot_transcript::ToolCall {
        id: "bang-rc".into(),
        name: "bash".into(),
        arguments: serde_json::json!({ "command": "letibot-rc-probe" }).to_string(),
    };
    let theirs = h.rt.invoke_operator("", &call, &mut h.sink);
    assert!(
        matches!(theirs.outcome, ToolOutcome::Ok),
        "the operator's own command must run: {}",
        theirs.render()
    );
    assert!(
        theirs.render().contains("ALIAS-APPLIED"),
        "their line must meet their shell, or `alias ls='ls --color=auto'` is a file \
         nothing reads and `! ls -la` stays plain: {}",
        theirs.render()
    );

    // The control: the same command on the model's entry. `sh` has no alias table
    // here, so the name is not a command.
    let mine = h.call(
        "bash",
        &serde_json::json!({ "command": "letibot-rc-probe" }).to_string(),
    );
    assert!(
        !mine.render().contains("ALIAS-APPLIED"),
        "a model's call must not be read through the operator's rc: {}",
        mine.render()
    );
}

/// The operator's own run gets the console's environment; a model's does not. `PAGER`
/// is the assertion because it is forced rather than inherited, so it does not depend
/// on what this test process happens to carry.
#[test]
fn the_operators_own_run_gets_the_consoles_pagers_and_a_models_call_does_not() {
    let mut h = runner!("operator console env");
    let home = h.root().to_path_buf();
    h.processes
        .as_ref()
        .unwrap()
        .set_standing_env(vec![("HOME".to_string(), home.display().to_string())]);

    let call = letibot_transcript::ToolCall {
        id: "bang-env".into(),
        name: "bash".into(),
        arguments: serde_json::json!({ "command": "printenv PAGER GIT_PAGER SYSTEMD_PAGER" })
            .to_string(),
    };
    let theirs = h.rt.invoke_operator("", &call, &mut h.sink);
    let said = theirs.render();
    assert!(
        matches!(theirs.outcome, ToolOutcome::Ok),
        "the operator's own command must run: {said}"
    );
    assert!(
        said.contains("cat"),
        "a pager on the operator's own run is the sibling defect — `git log` execs \
         `less`, and `less` waits for a keystroke that cannot arrive: {said}"
    );

    let mine = h.call(
        "bash",
        &serde_json::json!({ "command": "printenv PAGER" }).to_string(),
    );
    assert!(
        !mine.render().contains("cat"),
        "a model's call has no terminal, so no pager engages and it must be handed \
         no pager variable: {}",
        mine.render()
    );
}
