//! Acceptance tests for the two ways into the background, the kill deadline, and
//! monitors.
//!
//! # Why these assert on both branches
//!
//! Same reason `exec.rs` does: the substrate needs a delegated cgroup v2 subtree,
//! and a host without one is a real thing. A `#[ignore]` there would be a green
//! suite that measured nothing — the reaper-whose-zero-is-unfalsifiable defect,
//! one layer up. So a box with no cgroups exercises the refusal path and asserts
//! that the refusal names what was missing.

use letibot_tools::exec::{Fired, JobId, ProcessHost, ScopeKind};
use letibot_tools::testing::runner_harness;
use letibot_transcript::{Backgrounding, ToolOutcome};

macro_rules! runner {
    ($name:literal) => {
        match runner_harness() {
            Ok(h) => h,
            Err(e) => {
                eprintln!(
                    "{}: no cgroup v2 subtree here, checking the refusal: {e}",
                    $name
                );
                let msg = format!("{e}");
                assert!(
                    msg.contains("nothing would reap it") || msg.contains("cgroup"),
                    "a refusal must name what was missing: {msg}"
                );
                return;
            }
        }
    };
}

/// The job id out of a result body, for the paths that do not carry one in the
/// outcome.
fn job_id_anywhere(text: &str) -> Option<String> {
    let at = text.find("`j")?;
    let rest = &text[at + 1..];
    let end = rest.find('`')?;
    Some(rest[..end].to_string())
}

// --------------------------------------------------- the deadline, opencode parity

/// **The load-bearing one.** A foreground command that outlives its deadline must
/// be *killed*, not described as killed. opencode's `timeout` semantics: the model
/// did not ask to run it longer, so it must not run forever.
#[test]
fn a_command_that_outlives_its_deadline_is_killed() {
    let mut h = runner!("kill_on_deadline");
    // Foreground — no `background` — with a deadline short enough to fire here.
    let r = h.call(
        "bash",
        &serde_json::json!({"command": "sleep 30", "timeout_ms": 300}).to_string(),
    );
    assert_eq!(r.outcome, ToolOutcome::Timeout, "{}", r.render());

    // The kill is a fact about the process, not a sentence next to it.
    let host = h.processes.clone().unwrap();
    let id = job_id_anywhere(&r.payload).expect("a job id in the body");
    let view = host.job(&JobId(id)).expect("the job is still known");
    assert!(
        !view.state.is_running(),
        "a timed-out command must be stopped, not left running: {}",
        view.state.word()
    );
}

/// The deadline is its own outcome, not a failure and not a backgrounding, and it
/// hands over both ways to run the command longer.
#[test]
fn a_deadline_is_its_own_outcome_and_says_how_to_run_longer() {
    let mut h = runner!("timeout_outcome");
    let r = h.call(
        "bash",
        &serde_json::json!({"command": "sleep 30", "timeout_ms": 300}).to_string(),
    );
    assert_eq!(r.outcome, ToolOutcome::Timeout, "{}", r.render());
    let rendered = r.render();
    assert!(rendered.contains("killed"), "{rendered}");
    assert!(
        rendered.contains("timeout_ms") && rendered.contains("background"),
        "the timeout must name both ways out: {rendered}"
    );
}

// --------------------------------------- two ways in, and a deadline that is not one

/// Two ways into the background — the model asks, or a person moves it — and the
/// deadline is the third path, which is *not* a backgrounding. Each is a distinct
/// result, which is the requirement itself.
#[test]
fn the_ways_into_the_background_and_the_deadline_are_distinguishable() {
    let mut h = runner!("ways");

    // 1. The model asks.
    let asked = h.call(
        "bash",
        &serde_json::json!({"command": "sleep 30", "background": true}).to_string(),
    );
    let asked_id = match &asked.outcome {
        ToolOutcome::Backgrounded { handle, how, .. } => {
            assert_eq!(*how, Backgrounding::Asked);
            handle.clone()
        }
        other => panic!("{other:?}: {}", asked.render()),
    };
    assert!(asked.render().contains("you asked"), "{}", asked.render());

    // 2. A person moves one mid-flight. This is the daemon-side verb for the
    //    head's Ctrl+B; the frame that drives it is written in `ProcessHost::promote`.
    let host = h.processes.clone().unwrap();
    let third = h.call(
        "bash",
        &serde_json::json!({"command": "sleep 30", "background": true, "scope": "turn"})
            .to_string(),
    );
    let third_id = job_id_anywhere(&third.payload).expect("a job id");
    let jid = JobId(third_id.clone());
    let p = host
        .promote(
            &jid,
            ScopeKind::Session,
            None,
            Backgrounding::Operator {
                identity: "deadtrickster".into(),
            },
        )
        .expect("the operator promotion");
    assert!(p.summary().contains("deadtrickster"), "{}", p.summary());
    assert_eq!(host.job(&jid).unwrap().owner.kind, ScopeKind::Session);

    // 3. The deadline kills — a distinct outcome from either backgrounding.
    let killed = h.call(
        "bash",
        &serde_json::json!({"command": "sleep 30", "timeout_ms": 300}).to_string(),
    );
    assert_eq!(killed.outcome, ToolOutcome::Timeout, "{}", killed.render());

    // Two distinct phrasings for the two backgroundings, and a third for the kill.
    assert_ne!(Backgrounding::Asked.phrasing(), p.how.phrasing());

    for id in [asked_id, third_id] {
        let _ = host.kill_job(&JobId(id));
    }
}

/// The promotion record is a measurement and not a boolean — the same falsifier
/// the reap log is. A promotion that moved nothing and one that moved three must
/// not look alike.
#[test]
fn the_promotion_record_carries_what_moved_and_reaches_the_model_through_job_list() {
    let mut h = runner!("promotion_record");
    // Start a job in the turn scope, then promote it as the operator would.
    let r = h.call(
        "bash",
        &serde_json::json!({"command": "sleep 30", "background": true, "scope": "turn"})
            .to_string(),
    );
    let id = job_id_anywhere(&r.payload).expect("a job id");
    let host = h.processes.clone().unwrap();
    let jid = JobId(id);
    host.promote(
        &jid,
        ScopeKind::Session,
        None,
        Backgrounding::Operator {
            identity: "deadtrickster".into(),
        },
    )
    .expect("the operator promotion");

    let log = host.promotions();
    assert_eq!(log.len(), 1, "the promotion must be recorded");
    let m = log[0]
        .migration
        .as_ref()
        .expect("a live job has a migration");
    assert!(
        !m.observed.is_empty(),
        "presence before absence: the record must say what was there. {}",
        m.summary()
    );
    assert!(m.complete(), "left behind: {}", m.summary());
    assert!(m.summary().contains("moved"), "{}", m.summary());

    let listing = h.call("job_list", "{}");
    assert!(
        listing.payload.contains("moved to a different scope"),
        "the model must be able to read back a lifetime that changed under it: {}",
        listing.payload
    );

    // Promoting the same job twice moves nothing, and says so rather than
    // producing a record that reads as a second move.
    let again = host
        .promote(
            &jid,
            ScopeKind::Session,
            None,
            Backgrounding::Operator {
                identity: "deadtrickster".into(),
            },
        )
        .expect("second promotion");
    assert!(again.migration.is_none());
    assert!(
        again.note.as_deref().unwrap_or("").contains("already"),
        "{:?}",
        again.note
    );
    let _ = host.kill_job(&jid);
}

/// The head's Ctrl+B: the promote channel the daemon wires from the hub, read by
/// the `bash` wait loop. Setting the channel is the head's half; the `bash` result
/// carrying `Operator` is the daemon's.
#[test]
fn a_promote_request_from_the_head_moves_the_running_command() {
    let mut h = runner!("promote_from_head");
    // The head asks, before the command would finish on its own.
    h.promote
        .as_ref()
        .expect("a promote channel")
        .lock()
        .unwrap()
        .replace("deadtrickster".to_string());
    let r = h.call(
        "bash",
        &serde_json::json!({"command": "sleep 30"}).to_string(),
    );
    match &r.outcome {
        ToolOutcome::Backgrounded { handle, how, .. } => {
            assert_eq!(
                *how,
                Backgrounding::Operator {
                    identity: "deadtrickster".into()
                }
            );
            let _ = h
                .processes
                .clone()
                .unwrap()
                .kill_job(&JobId(handle.clone()));
        }
        other => panic!(
            "a Ctrl+B must background the command, got {other:?}: {}",
            r.render()
        ),
    }
    assert!(
        r.render().contains("moved to the background"),
        "{}",
        r.render()
    );
}

// ------------------------------------------------------- monitors, T24

/// T24 requirements 1, 2, 3 and 4, through the tool the model actually calls.
#[test]
fn a_monitor_is_owned_listed_says_why_it_fired_and_refuses_a_second_under_one_name() {
    let mut h = runner!("monitor_tool");
    let declared = h.call(
        "monitor",
        &serde_json::json!({"name": "built", "path": "marker-a"}).to_string(),
    );
    assert_eq!(declared.outcome, ToolOutcome::Ok, "{}", declared.render());
    // 1: an owner, and the default is the session.
    assert!(declared.payload.contains("owner:"), "{}", declared.payload);
    assert!(declared.payload.contains("session"), "{}", declared.payload);

    // 2: listable and attributable, in the listing people already open.
    let listing = h.call("job_list", "{}");
    assert!(listing.payload.contains("built"), "{}", listing.payload);
    assert!(
        listing.payload.contains("declared by"),
        "an invisible watcher is an unreapable one: {}",
        listing.payload
    );

    // 4: one waiter per name, enforced, and the refusal hands over what holds it.
    let second = h.call(
        "monitor",
        &serde_json::json!({"name": "built", "path": "marker-b"}).to_string(),
    );
    assert!(
        matches!(second.outcome, ToolOutcome::Failed { .. }),
        "{}",
        second.render()
    );
    assert!(
        second.payload.contains("already watching"),
        "{}",
        second.payload
    );
    assert!(
        second.payload.contains("NOTHING was declared"),
        "{}",
        second.payload
    );

    // 3: why it fired, not that it did.
    let host = h.processes.clone().unwrap();
    let monitors = host.monitors().expect("a host with monitors").clone();
    let root = host.workspace().expect("a workspace").to_path_buf();
    std::fs::write(root.join("marker-a"), b"x").expect("write the marker");
    let fired = monitors.tick();
    assert_eq!(fired.len(), 1, "the monitor must fire on the change");
    let why = match &fired[0].fired {
        Fired::Fired { why, .. } => why.clone(),
        other => panic!("{other:?}"),
    };
    assert!(why.contains("now exists"), "{why}");

    let after = h.call("job_list", "{}");
    assert!(after.payload.contains("FIRED"), "{}", after.payload);
    assert!(after.payload.contains("now exists"), "{}", after.payload);
}

/// T24 requirement 1 from the other end, and the reason monitors could not land
/// before the lifetime work: an owner is only an owner if its end takes what it
/// owns, and the record has to say which scope took it.
#[test]
fn a_monitor_dies_with_the_scope_that_owns_it() {
    let mut h = runner!("monitor_reaped");
    // Something has to be in the turn scope for it to exist to be named.
    h.call("bash", &serde_json::json!({"command": "true"}).to_string());
    let declared = h.call(
        "monitor",
        &serde_json::json!({"name": "turnwatch", "path": "marker", "owner": "turn"}).to_string(),
    );
    assert_eq!(declared.outcome, ToolOutcome::Ok, "{}", declared.render());

    let host = h.processes.clone().unwrap();
    let turn = host.scope_for(ScopeKind::Turn, None).expect("turn scope");
    host.end_scope(&turn).expect("end turn");

    let listing = h.call("job_list", "{}");
    assert!(
        listing.payload.contains("its owner"),
        "a monitor reaped with its scope must say which scope took it: {}",
        listing.payload
    );
    assert!(
        listing.payload.contains("0 monitor(s) watching"),
        "the turn ended and its watcher did not: {}",
        listing.payload
    );
}

/// **The rule the whole design is shaped around**, asserted where a model would
/// hit it: nothing a monitor is asked to watch can be the process asking.
///
/// It used to read "there is nowhere to ask for a process by name", and the
/// `process` argument (2026-09-14) changes the mechanism, not the rule: the
/// string is consumed ONCE by the in-process finder, which removes this daemon,
/// its ancestors and its protected pids before matching, and the monitor holds
/// (pid, start time) handles from then on. So the assertion is now the stronger
/// one — ask for the test's own name, and it must not find itself.
#[test]
fn a_monitor_cannot_be_asked_to_watch_a_process_by_name() {
    let mut h = runner!("monitor_no_pattern");
    // Nothing to watch: the refusal names the conditions and says the process
    // one is never this daemon.
    let none = h.call("monitor", &serde_json::json!({"name": "x"}).to_string());
    assert!(
        matches!(none.outcome, ToolOutcome::Failed { .. }),
        "{}",
        none.render()
    );
    assert!(
        none.payload.contains("never this daemon"),
        "the refusal must say the process condition cannot be this daemon: {}",
        none.payload
    );

    // The test binary's own path carries the crate's name, and so does the cargo
    // that is its ancestor. A `process` watch for it must not find either.
    let me = letibot_tools::exec::procs::self_and_ancestors();
    let own = h.call(
        "monitor",
        &serde_json::json!({"name": "self", "process": "letibot_tools"}).to_string(),
    );
    if matches!(own.outcome, ToolOutcome::Ok) {
        for pid in &me {
            assert!(
                !own.payload.contains(&format!("{pid}  ")),
                "the monitor found this process or an ancestor ({pid}): {}",
                own.payload
            );
        }
        h.call(
            "monitor",
            &serde_json::json!({"name": "self", "action": "retire"}).to_string(),
        );
    } else {
        assert!(own.payload.contains("never matched"), "{}", own.payload);
    }

    // Two conditions is one monitor answering two questions, which answers
    // neither.
    let two = h.call(
        "monitor",
        &serde_json::json!({"name": "x", "path": "a", "port": 9}).to_string(),
    );
    assert!(
        matches!(two.outcome, ToolOutcome::Failed { .. }),
        "{}",
        two.render()
    );

    // And the seated schema has nowhere to put a pattern. This is the assertion
    // that matters: a process check self-matched its own shell seven times in one
    // session on this box, and the seventh killed a running command mid-flight.
    let names: Vec<String> =
        h.rt.registry
            .schemas()
            .into_iter()
            .filter(|s| s.name == "monitor")
            .flat_map(|s| {
                s.param_names()
                    .into_iter()
                    .map(|n| n.to_string())
                    .collect::<Vec<_>>()
            })
            .collect();
    assert!(!names.is_empty(), "the monitor tool is seated");
    // `command` is a condition (run one, fire on its exit) and `process` is a
    // string the finder consumes once; neither is a pattern the poller matches.
    for banned in ["pattern", "match", "cmdline", "regex", "host"] {
        assert!(
            !names.iter().any(|n| n.contains(banned)),
            "`monitor` grew `{banned}`: {names:?}"
        );
    }
}

/// `pkill` through the seated runtime: the listing, a refusal for a pid the
/// listing did not show, and a kill by pid. (The PROTECTED marking is asserted
/// in the tool's own unit test against a declared list; the daemon declares
/// the model server in harness.rs.)
#[test]
fn pkill_lists_and_kills_by_pid_only() {
    use std::process::{Command, Stdio};
    let mut h = runner!("pkill_tool");
    h.rt.registry
        .register(Box::new(letibot_tools::builtins::pkill::Pkill))
        .unwrap();
    let marker = format!("letibot-bg-pkill-{}", std::process::id());
    let mut go = Command::new("sh")
        .arg("-c")
        .arg("sleep 30")
        .arg("letibot-sh")
        .arg(&marker)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(200));
    let list = h.call("pkill", &serde_json::json!({"pattern": marker}).to_string());
    assert!(
        list.payload.contains("1 process(es) match"),
        "{}",
        list.payload
    );
    let kill = h.call(
        "pkill",
        &serde_json::json!({"pattern": marker, "action": "kill", "pids": [std::process::id(), go.id()]}).to_string(),
    );
    assert!(
        kill.payload
            .contains(&format!("{}: REFUSED — not among", std::process::id())),
        "{}",
        kill.payload
    );
    assert!(
        kill.payload.contains(&format!("{}: TERM sent", go.id())),
        "{}",
        kill.payload
    );
    assert!(kill.payload.contains("all 1 gone"), "{}", kill.payload);
    let _ = go.wait();
}

// ------------------------------------------------------------ R23: the wait declines

/// **R23: a `job_wait` on a job the harness is already delivering returns at once.**
///
/// The operator's complaint, filed as R23, recurring *after* R7 landed: *"letibot
/// again did `job_wait` right after i backgrounded."* R7 gave the daemon a promise —
/// a background job's settlement is submitted to the model as a turn of its own,
/// unprompted — and four rewrites of the `Backgrounded` result telling the model
/// *do not wait for it* did not stop this. Advice is not a guarantee, which is this
/// tree's own rule about checks applied to a prompt.
///
/// So the promise moves into the mechanism: `job_wait` asks whether the harness has
/// already promised this job's answer, and if it has, returns instead of blocking.
///
/// # The measurement, and why it is a clock rather than a string
///
/// Same as R7's: **force the case and time it.** The deadline is 30 s, the job is a
/// `sleep 300` that cannot end during the call, and the assertion is on elapsed time.
/// Text alone would pass for a wait that blocked 30 s and then said the right thing —
/// which is precisely the behaviour the operator is reporting, and the behaviour this
/// test exists to make impossible.
#[test]
fn a_wait_on_a_job_the_harness_is_delivering_returns_at_once() {
    let mut h = runner!("r23_at_once");

    // A job that will outlive any deadline this test sets.
    let started = h.call(
        "bash",
        &serde_json::json!({"command": "sleep 300", "background": true}).to_string(),
    );
    let id = match &started.outcome {
        ToolOutcome::Backgrounded { handle, .. } => handle.clone(),
        other => panic!("{other:?}: {}", started.render()),
    };

    // **The daemon's own answer, forced.** In a real session this closure reads the
    // per-session job watchers: the sink arms one the moment a `Backgrounded` result
    // passes it, so by the time the model's next call runs the job is watched.
    let watched = id.clone();
    h.with_completion_delivered(std::sync::Arc::new(move |j: &str| j == watched));

    let t0 = std::time::Instant::now();
    let r = h.call(
        "job_wait",
        &serde_json::json!({"job": id, "timeout_ms": 30_000}).to_string(),
    );
    let took = t0.elapsed();

    assert!(
        took < std::time::Duration::from_secs(5),
        "a wait on a job the harness is already delivering must return at once, \
         not after its deadline; it took {took:?}"
    );
    // Not a deadline and not a failure: the call was reasonable and the answer is
    // already on its way.
    assert_eq!(r.outcome, ToolOutcome::Ok, "{}", r.render());
    let body = r.render();
    assert!(
        body.contains("nothing to wait for"),
        "the sentence must say why nothing was waited for: {body}"
    );
    assert!(
        body.contains("on its own"),
        "and it must name the promise it is relying on: {body}"
    );
    assert!(
        body.contains("sleep 300"),
        "and it must still say what the job was: {body}"
    );

    // The job is untouched — this declined to wait, it did not stop anything.
    let host = h.processes.clone().unwrap();
    let view = host.job(&JobId(id.clone())).expect("the job is still known");
    assert!(view.state.is_running(), "the wait must not stop the job");
    let _ = h.call("job_kill", &serde_json::json!({"job": id}).to_string());
}

/// **The contrast: a `job_wait` on a job nobody is delivering still blocks, exactly
/// as before.**
///
/// R23 removes one case and must remove only that one. A scope, a job from another
/// session, a deliberate block before a dependent step — all of those keep the verb
/// they always had, and this is the assertion that says so rather than the comment
/// saying so. The deadline is short so the test is; what is measured is that the
/// deadline *is* reached, which is the whole of "blocks".
#[test]
fn a_wait_on_a_job_nobody_is_delivering_still_blocks_for_its_deadline() {
    let mut h = runner!("r23_still_blocks");

    let started = h.call(
        "bash",
        &serde_json::json!({"command": "sleep 300", "background": true}).to_string(),
    );
    let id = match &started.outcome {
        ToolOutcome::Backgrounded { handle, .. } => handle.clone(),
        other => panic!("{other:?}: {}", started.render()),
    };

    // The same closure, saying the harness is watching something else entirely —
    // the honest shape of "this job is not one the daemon will hand you".
    h.with_completion_delivered(std::sync::Arc::new(|_j: &str| false));

    let t0 = std::time::Instant::now();
    let r = h.call(
        "job_wait",
        &serde_json::json!({"job": id, "timeout_ms": 700}).to_string(),
    );
    let took = t0.elapsed();

    assert_eq!(
        r.outcome,
        ToolOutcome::Timeout,
        "an undelivered job must still reach its deadline: {}",
        r.render()
    );
    assert!(
        took >= std::time::Duration::from_millis(650),
        "it must actually spend the deadline it was given, not return early for \
         some other reason; it took {took:?}"
    );
    let body = r.render();
    assert!(
        !body.contains("nothing to wait for"),
        "the R23 sentence must not appear for a job nobody is delivering: {body}"
    );
    assert!(body.contains("STILL RUNNING"), "{body}");
    let _ = h.call("job_kill", &serde_json::json!({"job": id}).to_string());
}
