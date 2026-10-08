//! Acceptance tests for the exec substrate: T21.1, T21.2, T24's reaper record,
//! and the two gates a command has to pass.
//!
//! # Why several of these end with `Err(e) => …` rather than a skip
//!
//! The substrate needs a delegated cgroup v2 subtree, and a host without one is a
//! real thing. A `#[ignore]` there would be a green suite that measured nothing —
//! the same defect as the reaper whose zero is unfalsifiable, one layer up. So
//! every test that needs real processes asserts **something** on both branches:
//! either the behaviour, or that the refusal names what was missing and that
//! nothing was started.

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
}

// ------------------------------------------------------------------ T21.1

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
    // 1. The diagnosis.
    assert!(body.contains("was NOT run"), "{body}");
    // 2. What it currently matches — the shell, by construction.
    assert!(
        body.contains("the shell that will evaluate this command"),
        "the refusal must say WHAT it matches: {body}"
    );
    // 3. The handle-shaped alternative, which has no pattern.
    assert!(body.contains("job_kill"), "{body}");
    assert!(body.contains("job_list"), "{body}");
    // And nothing ran.
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
    // The point of this case is the *reason string*, not the refusal: a refusal
    // that says "protected" gives the model nothing to act on.
    let _ = r;
    let protected = host.protected();
    let server = protected
        .iter()
        .find(|p| p.why.contains("model server"))
        .expect("the declared server is in the protected set");
    assert!(server.outlives_the_turn);
    assert!(protected.iter().any(|p| p.pid == std::process::id()));
}

// ------------------------------------------------------------------ T21.2

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
    // The canonical form. `zzz-nothing-like-this` matches no process on the box —
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
    // 1. It happened.
    let started = h.call(
        "bash",
        &serde_json::json!({"command": "sleep 0.4; echo done", "background": true}).to_string(),
    );
    let id = job_id(&started.payload);
    let ok = h.call(
        "job_wait",
        &serde_json::json!({"job": id, "timeout_ms": 8000}).to_string(),
    );
    assert_eq!(ok.outcome, ToolOutcome::Ok, "{}", ok.render());
    assert!(ok.payload.contains("exited 0"), "{}", ok.payload);

    // 2. The deadline passed quietly, and it is NOT reported as completion.
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

    // 3. Nothing was ever there: absence without presence is a boot window.
    let never = h.call(
        "job_wait",
        &serde_json::json!({"scope": "explicit", "timeout_ms": 200}).to_string(),
    );
    // No explicit scope is open, so this is the unknown-scope miss — which is
    // clause 1's job and must list what there is.
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

// -------------------------------------------------- T24: the reaper record

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
            cwd: ".".into(),
            scope: ScopeKind::Explicit,
            scope_name: Some("reaper-test".into()),
            background: true,
            env: vec![],
            // No terminal: these are the substrate's own tests, not an operator's run.
            tty: false,
        })
        .expect("spawn");
    }

    // PRESENCE FIRST. `only count absence after presence`: without this loop, a
    // later empty read would be a boot window and the test would pass on a
    // substrate that never started anything.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut seen = 0;
    while std::time::Instant::now() < deadline {
        seen = host.scopes().len().max(seen);
        let members = tree_members(&host, &scope);
        if members >= 4 {
            seen = members;
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
    // It is `clean`, and that is honest — but the summary says WHY it is clean,
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
        // `; :` keeps the shell beside `sleep` on every `sh` — macOS's is bash, which would
        // otherwise `exec` it, and the record would be one process caught mid-exec.
        &serde_json::json!({"command": "sleep 60; :", "background": true}).to_string(),
    );
    let id = job_id(&started.payload);
    // Kill once `sleep` exists, not before: a kill that lands while the shell is still
    // forking records one process, and the count below is about the record, not the race.
    let host = h.processes.clone().unwrap();
    let job = letibot_tools::JobId(id.clone());
    let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
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
    assert!(
        after.payload.contains("scope(s) have ended this session"),
        "{}",
        after.payload
    );
    assert!(after.payload.contains("observed 2"), "{}", after.payload);
    // The record has to be USABLE later: a pid list from an hour ago identifies
    // nothing, so the command line travels with it.
    assert!(after.payload.contains("sleep"), "{}", after.payload);
}

// ------------------------------------------------------- the two gates

#[test]
fn with_no_adjudicator_bash_is_not_run_and_it_is_not_denied() {
    let mut h = runner!("gate", None);
    let r = h.call(
        "bash",
        &serde_json::json!({"command": "echo hi"}).to_string(),
    );
    match &r.outcome {
        // `NotRun` — nobody decided. `Denied` would claim a decision was made.
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
    // `bash` is not even registered there, which is the third mechanism — the
    // role. That refusal is clause 1's unknown-tool path and must list what is.
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
fn the_exec_surface_of_each_role_is_exactly_what_was_decided() {
    use letibot_tools::runtime::roles;
    let reg = letibot_tools::runner_tools(std::sync::Arc::new(
        letibot_tools::builtins::retrieval::Unavailable,
    ))
    .unwrap();
    // The runner role seats nine; with the ceiling raised to 16 for leticode it
    // is under the ceiling rather than over it, but its own number still stands.
    let role = roles::m2_runner();
    assert_eq!(role.max_tools, 9, "the runner's own number is 9");
    assert_eq!(
        letibot_tools::runtime::DEFAULT_MAX_TOOLS,
        16,
        "the ceiling was raised for leticode's tool union"
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
    assert_eq!(seated.len(), 9);
    assert!(seated.names().contains(&"bash".to_string()));
    assert!(seated.names().contains(&"monitor".to_string()));

    // The orchestrator names no exec tool at all, and that has not moved.
    for name in EXEC_TOOLS {
        assert!(
            !roles::m1_orchestrator().tools.contains(&name.to_string()),
            "`m1_orchestrator` gained `{name}` — an exec path must arrive by decision, \
             not by a sibling workstream"
        );
    }

    // **`coder` names `bash`, and that arrived by decision on 2026-09-11.** The
    // operator asked for a seat that can run the project's own tests, and a shaped
    // `test` verb was the alternative that was put to them and not taken.
    //
    // This assertion is written as an exact set rather than a "does not contain"
    // list, because the guard it replaces was the thing that caught the seat moving
    // at all: `bash` is one decision and the job/monitor tools are a different one
    // that nobody has made. A role that quietly grew `job_kill` alongside it would
    // pass a contains-check and is precisely what this file exists to refuse.
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

    // And the seat is still only the NAME of a permission. Whether a session gets
    // the tool is `--bash` on the daemon, which strips it back off when absent —
    // `crates/harnessd` owns that half and tests it there. The two are deliberately
    // separate: a role that names a tool no session is given is a role that can be
    // read, and a daemon flag that adds a tool to a role that never named it would
    // be an exec path with no declaration anywhere.
}

/// Named once, because a list that is retyped per assertion is a list that drifts.
const EXEC_TOOLS: &[&str] = &[
    "bash",
    "job_kill",
    "job_wait",
    "job_list",
    "job_output",
    "monitor",
];

// ----------------------------------------------------- output and denominators

#[test]
fn a_capped_result_says_it_capped_and_job_output_has_the_rest() {
    let mut h = runner!("cap");
    let r = h.call(
        "bash",
        &serde_json::json!({
            // Was `i=0; while [ $i -lt 3000 ]; do echo "line $i ..."; i=$((i+1)); done`,
            // and layer 2's normaliser now refuses that with `NotRun`: `$i` and
            // `$((i+1))` are values that do not exist until the shell runs, so nothing
            // could decide about the command. That refusal is correct and this test is
            // not about it — it is about the output cap — so the command is respelled
            // with the same volume and no unresolvable construct. `seq` does the
            // counting the shell variable was doing.
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
    assert!(warm.payload.contains("job(s):"), "{}", warm.render());
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

// ------------------------------------------------------------- housekeeping

/// A session that ends leaves no cgroup directory behind, not merely no process.
///
/// Written **after looking**, not after a failure: the suite was green, no
/// process had leaked, and six empty `letibot.<pid>/` directories had accumulated
/// — one per test-binary run. Nothing was holding anything, so no reap record was
/// wrong, and the count still went up by one per session. That is the shape of
/// the thirteen abandoned worktrees T24 was opened about, and the only reason it
/// was noticed is that somebody went and looked — which is exactly the vigilance
/// the fleet said a mechanism has to replace.
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
        cwd: ".".into(),
        scope: ScopeKind::Turn,
        scope_name: None,
        background: false,
        env: vec![],
        // No terminal: these are the substrate's own tests, not an operator's run.
        tty: false,
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

// ------------------------------------------------------------------ helpers

fn tree_members(
    host: &letibot_tools::exec::HostProcesses,
    scope: &letibot_tools::exec::ScopeId,
) -> usize {
    // The tree's own reading of membership — the file the reaper reads on Linux, the
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

// ------------------------------------------- the operator's own `!` shell line
//
// The ungated half of the runtime (`invoke_operator`) is what the daemon runs an
// operator's `!` line through. Its contract is the door's own ruling — *nobody left
// to ask* — and these pin the two properties that ruling has:
//
// 1. **No gate call appears.** Not `admit`, and not the view-grant ask `bash` can
//    raise when its output names a path the boundary hid. A gate the operator must
//    answer for their own command is the feature not working.
// 2. **Everything else is identical.** Same tool, same execution path, same caps —
//    which is the reason the daemon runs it rather than a head.

/// A gate that refuses everything and counts every way it was consulted.
///
/// The counting is the point: the assertion is not "the call succeeded" (a permissive
/// gate would give that too) but "the gate has no calls on its books at all".
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

/// **The contrast case first, because it is what makes the other test mean anything.**
///
/// A MODEL's `bash` call goes through `invoke`, and `bash` is `Access::Exec` — so the
/// gate is consulted exactly once and its refusal stops the call. If this test were
/// green while the operator's ran, the difference would be the gate and not the test.
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

/// **An operator's own `!` line runs the command and consults nothing.**
///
/// This is the test that would fail if a gate call appeared on the `!` path: the gate
/// counts both ways it can be reached, and both counters must stay at zero while the
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

/// **The operator's own run gets a terminal and a model's does not** — part A of the
/// ANSI requirement, asserted on what the command itself can see.
///
/// The operator reported it more than once: *"i run `! ls -la` and the output is plain,
/// while in a proper terminal directory names are highlighted"*. `ls --color=auto`
/// colourises only when `isatty(1)` is true, and a pipe is not a terminal, so the fix
/// is not a colour of our choosing but **a pty for their run** (`exec::pty` carries the
/// measurement, the cost and why the environment cannot do it).
///
/// The assertion is the command's own answer rather than an inspection of the request,
/// because that is the property that has to be true: `[ -t 1 ]` is `ls`'s question, put
/// to the same fd, and it is asked twice through the two entries of one runtime — the
/// ungated one a `!` line takes and the gated one a model's call takes. A model's call
/// must keep its pipe: the payload is tokens it reads, and an escape sequence around
/// every directory name is a cost it pays and cannot see.
///
/// **What this cannot cover is the operator's screen**: whether the colour that now
/// reaches the row is *drawn* is part B and is asserted in `letibot-tui`'s frame tests.
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

/// **The operator's own run is a job-control shell, and bash's two lines about not having
/// one are gone** — gone because their cause is gone and not because anything filtered them.
///
/// The operator's answer to the first cut of this was *"nah, i think that bash should feel
/// comfortable actually"*: a filter that drops `bash: no job control in this shell` leaves a
/// shell that still has none — no `jobs`, no `^Z`/`fg`/`bg`, no signal behaviour like their
/// own console. So this asserts the **cause** and the **want**, and the two absent sentences
/// are the consequence rather than the thing bought:
///
/// * `$-` contains `m` — bash's own answer that **monitor mode**, which is job control, is on.
///   Measured on this box, 2026-10-07: `bash -ic` with this pty as its controlling terminal
///   answers `MONITOR`; the same shell with the same pty **not** its controlling terminal
///   answers `NO-MONITOR` and prints the two lines.
/// * `/dev/tty` **opens** — so the terminal it opened is this pty, which is what makes the
///   first true. It is the same question [`letibot_tools::exec::term`]'s pane test asks.
///
/// **`(exec 9</dev/tty)` and not `[ -r /dev/tty ]`**, and the difference is not style: measured
/// in the same pair, `[ -r /dev/tty ]` answers *yes* **without a controlling terminal too**,
/// because it is a mode check on a device node rather than an open. The obvious probe lies, and
/// a test written on it would be green on a box where this whole change did nothing.
///
/// **The control is the model's entry in the same test.** Its run has no pty at all —
/// `/dev/null` on fd 0 and pipes for output — so it must answer *not a terminal* on both
/// descriptors, which is what keeps `MONITOR` above from being a fact about `bash` on this box
/// rather than about this path. (`MONITOR` itself is only asserted on the operator's entry: the
/// model's run is `/bin/sh`, a different shell, so *it does not say MONITOR* would prove
/// nothing about the terminal.)
#[test]
fn the_operators_own_run_is_a_job_control_shell_and_its_two_bash_lines_are_gone() {
    let mut h = runner!("operator job control");
    // **An exact line and not `contains`**: `NO-MONITOR` contains `MONITOR`, so the obvious
    // spelling of the assertion below would be green on the run this change did not fix.
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

    // ---- The operator's own run.
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

    // ---- The control: a model's call has no terminal at all.
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

/// **The operator's own case, against the program they reported it about.**
///
/// `[ -t 1 ]` is the mechanism; this is the fact. The operator's report was *"i run
/// `! ls -la` and the output is plain, while in a proper terminal directory names are
/// highlighted"*, so what has to be true is that **`ls` itself writes the SGR** on their run
/// — and `--color=auto` is the spelling that decides by asking the same question every
/// well-behaved program asks. The fixture tree has three directories in it, so a run that
/// colours has something to colour.
///
/// The assertion is on the payload the runtime produced, not on a screen: the head's half is
/// `letibot-tui`'s
/// (`on_a_head_that_emits_colour_a_foreign_escape_arrives_as_a_role_and_nothing_else`), and
/// the two together are the whole requirement — the run writes it, the row draws it.
/// **A model's call has the same assertion backwards**: its payload must carry no escape at
/// all, which is what keeps the model's context free of bytes it cannot see.
#[test]
fn the_operators_own_ls_colours_because_its_output_is_a_terminal() {
    let mut h = runner!("operator ls colour");
    let ask = |id: &str| letibot_transcript::ToolCall {
        id: id.into(),
        name: "bash".into(),
        // GNU `ls` colours on `--color=auto`; BSD `ls` (macOS) ignores it and takes `-G`.
        arguments: serde_json::json!({
            "command": if cfg!(target_os = "macos") {
                // …and colours only on a terminal it knows, so it is given a `TERM`; it is
                // still the terminal, not the variable, that turns the colour on.
                "TERM=xterm-256color ls -G"
            } else {
                "ls --color=auto"
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
    // **The same command on the model's entry, and it is plain.** This is the control that
    // makes the assertion above mean *the pty* rather than *this `ls` always colours*.
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

// ------------------------------------------- a command that wants the terminal
//
// The operator's second report, in the same breath as the colour one: *"what if I do
// `! sudo ls /root` … and we also have to think about things like nano. what happens
// when I run something long and running and that wants to own everything"*.
//
// The pty that makes `ls --color=auto` colourise is a **capture**: one transcript row,
// `/dev/null` on stdin. So a program that takes the screen draws cursor-addressing
// escapes into that row and then waits for a keystroke that cannot arrive — worse than
// plain, and the state to make honest.
//
// [`letibot_tools::exec::terminal`] is the rule, and its own tests are the pure
// predicate: refused or not, by which program, for which class, with no process ever
// started. These two are the **wiring**: that the rule is consulted on the operator's
// entry and NOT on the model's, and that the refusal is said rather than silent.

/// **The operator's own `!` line is refused, by name, with the sentence.**
///
/// `text_only_runner_harness` rather than `runner!`, and deliberately: the rule is the
/// FIRST thing `bash` decides — ahead of the backend, the scope and the predicate —
/// because it is a fact about the command text alone. So a session whose backend cannot
/// start a process still gets the right sentence, and this test then measures something
/// on a host with no delegated cgroup instead of taking `runner!`'s refusal branch.
///
/// Three assertions, and the last two are what make the first mean *this rule* rather
/// than *this harness refuses everything*:
///
/// 1. `nano` on the operator's entry is `NotRun` and the row carries the sentence.
/// 2. **The operator's own example from the same sentence** — `! sudo ls /root` — is not
///    refused by this rule: it reaches the backend and is refused for the backend's
///    reason, which says nothing about a terminal.
/// 3. A **pipeline** is refused for its interactive member, which is what makes this a
///    rule over the line's stages rather than over its first word.
#[test]
fn an_interactive_program_on_the_operators_own_line_is_refused_by_name_with_the_sentence() {
    let mut h = letibot_tools::testing::text_only_runner_harness();
    let bang = |id: &str, command: &str| letibot_transcript::ToolCall {
        id: id.into(),
        name: "bash".into(),
        arguments: serde_json::json!({ "command": command }).to_string(),
    };

    // 1. Refused, and SAID. The hard requirement is that the refusal is never silent,
    //    so the assertions are on the text: the program's name, that nothing ran, what
    //    the run actually is, and what to do instead.
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
    // **The remedy names the verb that works.** *"run it in another window"* was the
    // answer while there was no pane, and it sent the operator out of the harness for a
    // program this harness now runs itself — see `exec::term`.
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

    // 2. The operator's own example, from the same sentence as `nano`, and it must run
    //    — which here means *reach the backend*: this harness has no process host, so
    //    the reason is the backend's. The assertion is that it is NOT this rule's.
    let ordinary =
        h.rt.invoke_operator("", &bang("bang-ls", "sudo ls /root"), &mut h.sink);
    let text = ordinary.render();
    assert!(
        !text.contains("cannot hand you one"),
        "`sudo ls /root` is the operator's own example and must not be refused by the \
         terminal rule: {text}"
    );

    // 3. A pipeline is judged member by member.
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

/// **The rule is not consulted on a model's call**, and that is a decision rather than
/// an oversight.
///
/// A model's `bash` call has no pty — `InvokeCtx::tty` is `!gated`, and this is the
/// gated entry — so `nano` there reads EOF on `/dev/null` and exits instead of hanging
/// in a screen nobody is watching. That is a different defect and `exec::terminal` lists
/// it among its own misses; what this test pins is that the rule stays on the side of
/// the line it was written for, because a rule that fired on both would tell the model
/// to *run it in the pane*, which is advice a model cannot take.
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
    // It still does not RUN, and the reason is the one that applies here: this harness
    // has no process host. The contrast is the point — a different reason, from a
    // different guard.
    assert!(
        matches!(mine.outcome, ToolOutcome::Failed { .. }),
        "the backend's own refusal is what a model's call gets here: {text}"
    );
}

/// **The end-to-end version, against the real substrate**, for a host that has one.
///
/// The two tests above prove the wiring without needing a process. This one proves the
/// thing the operator will actually see: on a session that CAN run commands, `! nano`
/// is refused with the sentence and **nothing is started**, while `! ls` on the same
/// entry runs and its output lands in the row. `runner!` needs a delegated cgroup v2
/// subtree, so on a host without one this takes the refusal branch and says so — the
/// house pattern for every test in this file that needs a real process.
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

/// **The view-grant ask specifically, on a boundary that produces one.**
///
/// `grant_view` fires when a command's output names a path the confinement hid —
/// reachable only on a confined session, which is why this one is built with
/// `confined_harness_with_gate` rather than the plain runner. On a MODEL's call that
/// is a card the operator answers; on their OWN command it would be a question about
/// a line they just typed, so the ungated path takes none and the row keeps the
/// absence note `bash` already wrote (`absence_notes` and `outside_paths` scan the
/// same output for the same paths — nothing is lost but the ask).
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
            // The confined substrate needs a usable unprivileged namespace boundary,
            // and a host without one is a real host (`kernel.apparmor_restrict_
            // unprivileged_userns=1`). The refusal is asserted rather than skipped,
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
    // A command whose OUTPUT names a path the boundary hid. The path does not have
    // to exist on the host — `outside_paths` classifies LEXICALLY, on purpose (a
    // stat would turn the note into a disclosure about the operator's disk), and
    // `/opt` is past every bound and replaced root, so the token is `Outside` and
    // nothing under `/tmp` would be: `/tmp` is one of the REPLACED roots, a fresh
    // tmpfs, and a path there is absent for a different reason that raises no ask.
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
    // The absence is still NAMED on the row — bash's own note, not a gate question.
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

// ------------------------------------------- the world their console gives it
//
// The operator's first report was *"i run `! ls -la` and the output is plain, while
// in a proper terminal directory names are highlighted"*, and the pty above is only
// half of it: on this box `ls` colourises because their `~/.bashrc` says
// `alias ls='ls --color=auto'`, and an alias is shell state that no `/bin/sh -c`
// reads. `letibot_tools::exec::console` carries the decision, what it changes, and
// why a model's call gets none of it; these are the wiring, against a real process.

/// **The operator's own line is read by their shell, so their alias is their
/// command.**
///
/// The console's rc is a file, so this is testable without a daemon: the daemon's
/// only part is `HOME`, which it puts on the standing environment
/// (`harness.rs` `set_standing_env`), and a test can put the same pair there. The
/// assertion is the alias's own output rather than an inspection of the argv,
/// because the property is *the line met their shell* and not *the argv had `-i`
/// in it* — and the model's entry is the control that makes it mean that: the same
/// command, the same host, the gated path, where `sh` has never heard of the name.
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
    // here, so the name is not a command — which is R10 layer 1 working, not a bug.
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

/// **The operator's own run gets the console's environment; a model's does not.**
///
/// `PAGER` is the assertion because it is the one pair that is **forced** rather
/// than inherited, so it does not depend on what this test process happens to carry
/// — the console's own `PAGER` cannot survive into a run nobody can type at, and a
/// model's run has no console at all. `printenv` prints nothing and exits non-zero
/// for a variable that is not there, which is the second half of the contrast.
///
/// The pure version of the whole six-pair environment is
/// `exec::console::tests::the_operators_run_is_given_the_consoles_terminal_and_pagers_that_cannot_page`;
/// this is the wiring from that function to a process.
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
