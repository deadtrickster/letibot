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
        other => panic!("a Ctrl+B must background the command, got {other:?}: {}", r.render()),
    }
    assert!(r.render().contains("moved to the background"), "{}", r.render());
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
    let why = match fired[0].settled().unwrap() {
        Fired::Fired { why, .. } => why,
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
/// hit it: there is nowhere to ask for a condition that could match the shell
/// asking.
#[test]
fn a_monitor_cannot_be_asked_to_watch_a_process_by_name() {
    let mut h = runner!("monitor_no_pattern");
    // Nothing to watch: the four conditions are the four, and the refusal carries
    // the diagnosis rather than merely the rule.
    let none = h.call("monitor", &serde_json::json!({"name": "x"}).to_string());
    assert!(
        matches!(none.outcome, ToolOutcome::Failed { .. }),
        "{}",
        none.render()
    );
    assert!(
        none.payload.contains("matches the process evaluating it"),
        "the refusal must say WHY there is no such argument: {}",
        none.payload
    );

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
    for banned in ["pattern", "match", "cmdline", "command", "regex", "host"] {
        assert!(
            !names.iter().any(|n| n.contains(banned)),
            "`monitor` grew `{banned}`: {names:?}"
        );
    }
}
