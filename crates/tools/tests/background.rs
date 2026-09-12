//! Acceptance tests for the three ways into the background, and for monitors.
//!
//! # Why these assert on both branches
//!
//! Same reason `exec.rs` does: the substrate needs a delegated cgroup v2 subtree,
//! and a host without one is a real thing. A `#[ignore]` there would be a green
//! suite that measured nothing — the reaper-whose-zero-is-unfalsifiable defect,
//! one layer up. So a box with no cgroups exercises the refusal path and asserts
//! that the refusal names what was missing.

use letibot_tools::exec::{Fired, JobId, ProcessHost, ScopeKind};
use letibot_tools::result::Envelope;
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

// ------------------------------------------------- promotion, requirement 1.2

/// **The load-bearing one.** A foreground command that outlives its threshold
/// must be *moved*, not merely described as moved.
///
/// The code this replaced returned `Failed` saying *"It is now in the `session`
/// scope, so it outlives this turn"* while the job's cgroup was still a child of
/// the turn's — so the turn's end reaped exactly the work the sentence promised
/// would survive it. The sentence and the fact were never checked against each
/// other, which is `docs/closed-loop.md`'s missing encoder in one line of prose.
#[test]
fn a_promoted_command_actually_outlives_the_turn_that_started_it() {
    let mut h = runner!("promote_survives_turn");
    // Foreground — no `background` — with a threshold short enough to fire here.
    let r = h.call(
        "bash",
        &serde_json::json!({"command": "sleep 30", "timeout_ms": 300}).to_string(),
    );
    let id = match &r.outcome {
        ToolOutcome::Backgrounded { handle, .. } => handle.clone(),
        other => panic!(
            "a command that outran its deadline must be backgrounded, got {other:?}: {}",
            r.render()
        ),
    };

    let host = h.processes.clone().unwrap();
    let jid = JobId(id.clone());
    // Check the claim against the cgroup tree, not against the sentence.
    let view = host.job(&jid).expect("the job is still known");
    assert_eq!(
        view.owner.kind,
        ScopeKind::Session,
        "a promoted job must be owned by the session, not the turn: {}",
        view.owner
    );

    // Now end the turn, which is what used to kill it.
    let turn = host.scope_for(ScopeKind::Turn, None).expect("turn scope");
    let reap = host.end_scope(&turn).expect("end turn");
    let after = host.job(&jid).expect("still known");
    assert!(
        after.state.is_running(),
        "ending the turn killed a promoted job — the promotion was a sentence and \
         not a move. {} / {}",
        after.state.word(),
        reap.summary()
    );
    let _ = host.kill_job(&jid);
}

// --------------------------------------------- the outcome, requirement 2

/// The result must not be readable as any of the three things it is not, and it
/// must carry enough that the obvious next call is right without a guess.
#[test]
fn a_promotion_reaches_the_model_as_its_own_outcome_with_the_handle_and_the_verb() {
    let mut h = runner!("promote_outcome");
    let r = h.call(
        "bash",
        &serde_json::json!({"command": "sleep 30", "timeout_ms": 300}).to_string(),
    );
    match &r.outcome {
        ToolOutcome::Backgrounded {
            handle,
            ran_for_ms,
            how,
            next,
        } => {
            assert!(!handle.is_empty());
            // How long it ran before promotion: the number that makes a promotion
            // legible rather than mysterious.
            assert!(*ran_for_ms >= 300, "ran_for_ms was {ran_for_ms}");
            // The model did NOT ask for this, and the outcome says which of the
            // three ways in it was.
            assert_eq!(*how, Backgrounding::Promoted);
            assert!(
                next.contains("job_output") || next.contains("job_wait"),
                "{next}"
            );
        }
        other => panic!("{other:?}: {}", r.render()),
    }
    let rendered = r.render();
    // Its own envelope. Not the error one, not the no-result one.
    assert_eq!(
        Envelope::classify(&rendered),
        Some("STILL_RUNNING"),
        "{rendered}"
    );
    assert!(rendered.contains("STILL RUNNING"), "{rendered}");
    assert!(rendered.contains("Do NOT start it again"), "{rendered}");
    assert!(rendered.contains("did not ask"), "{rendered}");
    // Not grounding: there is no answer yet for anything to be grounded in.
    assert!(!r.is_grounded());

    let host = h.processes.clone().unwrap();
    if let ToolOutcome::Backgrounded { handle, .. } = &r.outcome {
        let _ = host.kill_job(&JobId(handle.clone()));
    }
}

// --------------------------------------------- all three ways, requirement 1

/// Three ways in, and each distinguishable from the others in the result. A
/// promotion the model reads as its own request is a promotion it did not notice,
/// which is the whole defect requirement 2 exists against.
#[test]
fn the_three_ways_into_the_background_are_three_distinguishable_results() {
    let mut h = runner!("three_ways");

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

    // 2. The runtime promotes it, on elapsed time.
    let promoted = h.call(
        "bash",
        &serde_json::json!({"command": "sleep 30", "timeout_ms": 300}).to_string(),
    );
    let promoted_id = match &promoted.outcome {
        ToolOutcome::Backgrounded { handle, how, .. } => {
            assert_eq!(*how, Backgrounding::Promoted);
            handle.clone()
        }
        other => panic!("{other:?}: {}", promoted.render()),
    };
    assert!(
        promoted.render().contains("did not ask for it"),
        "{}",
        promoted.render()
    );

    // 3. A person promotes one mid-flight. This is the daemon-side verb; the head
    //    frame that would drive it is written down in `ProcessHost::promote`.
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
    assert!(
        p.complete(),
        "the operator promotion left processes behind: {}",
        p.summary()
    );
    assert_eq!(host.job(&jid).unwrap().owner.kind, ScopeKind::Session);

    // Three different sentences, which is the requirement itself.
    let a = Backgrounding::Asked.phrasing();
    let b = Backgrounding::Promoted.phrasing();
    let c = p.how.phrasing();
    assert_ne!(a, b);
    assert_ne!(b, c);
    assert_ne!(a, c);

    for id in [asked_id, promoted_id, third_id] {
        let _ = host.kill_job(&JobId(id));
    }
}

/// The promotion record is a measurement and not a boolean — the same falsifier
/// the reap log is. A promotion that moved nothing and one that moved three must
/// not look alike.
#[test]
fn the_promotion_record_carries_what_moved_and_reaches_the_model_through_job_list() {
    let mut h = runner!("promotion_record");
    let r = h.call(
        "bash",
        &serde_json::json!({"command": "sleep 30", "timeout_ms": 300}).to_string(),
    );
    let ToolOutcome::Backgrounded { handle, .. } = &r.outcome else {
        panic!("{}", r.render())
    };
    let host = h.processes.clone().unwrap();

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
    let jid = JobId(handle.clone());
    let again = host
        .promote(&jid, ScopeKind::Session, None, Backgrounding::Promoted)
        .expect("second promotion");
    assert!(again.migration.is_none());
    assert!(
        again.note.as_deref().unwrap_or("").contains("already"),
        "{:?}",
        again.note
    );
    let _ = host.kill_job(&jid);
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
