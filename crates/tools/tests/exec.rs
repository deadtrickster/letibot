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
    let comm = std::fs::read_to_string(format!("/proc/{}/comm", std::process::id()))
        .unwrap_or_default()
        .trim()
        .to_string();

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
    assert_eq!(r.mechanism, "cgroup.kill", "{}", r.summary());
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
        &serde_json::json!({"command": "sleep 60", "background": true}).to_string(),
    );
    let id = job_id(&started.payload);
    let killed = h.call("job_kill", &serde_json::json!({"job": id}).to_string());
    assert_eq!(killed.outcome, ToolOutcome::Ok, "{}", killed.render());
    assert!(killed.payload.contains("sleep 60"), "{}", killed.payload);
    assert!(
        killed.payload.contains("mechanism: cgroup.kill"),
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
    // Read through the same file the reaper reads, so the presence check and the
    // kill are looking at one fact and not two.
    let mut n = 0;
    fn walk(dir: &std::path::Path, n: &mut usize) {
        if let Ok(s) = std::fs::read_to_string(dir.join("cgroup.procs")) {
            *n += s.lines().filter(|l| !l.trim().is_empty()).count();
        }
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    walk(&e.path(), n);
                }
            }
        }
    }
    let _ = host;
    walk(&scope.path, &mut n);
    n
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
