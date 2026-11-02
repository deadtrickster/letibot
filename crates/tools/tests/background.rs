//! Acceptance tests for the two ways into the background, the kill deadline, and
//! monitors.
//!
//! Every test here asserts on both branches: the substrate needs a delegated cgroup v2
//! subtree, and a box without one checks that the refusal names what was missing
//! instead of going green having measured nothing.

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
                // Announced through `apparatus`, so a run that requires the apparatus refuses
                // rather than silently skipping what this box cannot measure.
                return letibot_tokencore::apparatus::absent(
                    "a cgroup v2 subtree this user may write",
                );
            }
        }
    };
}

/// The job id out of a result body, for the results whose outcome carries none.
fn job_id_anywhere(text: &str) -> Option<String> {
    let at = text.find("`j")?;
    let rest = &text[at + 1..];
    let end = rest.find('`')?;
    Some(rest[..end].to_string())
}

// ------------------------------------------------------------------- the deadline

/// A foreground command that outlives its deadline must be killed, not described as
/// killed: the model did not ask to run it longer, so it must not run forever.
#[test]
fn a_command_that_outlives_its_deadline_is_killed() {
    let mut h = runner!("kill_on_deadline");
    // Foreground: no `background`, and a deadline short enough to fire inside the test.
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

/// The deadline is its own outcome, not a failure and not a backgrounding, and it names
/// one way to run the command longer: `background: true`. A larger `timeout_ms` only buys
/// the wait the model should not be making.
#[test]
fn a_deadline_is_its_own_outcome_and_says_how_to_run_longer() {
    let mut h = runner!("timeout_outcome");
    let r = h.call(
        "bash",
        &serde_json::json!({"command": "sleep 30", "timeout_ms": 300}).to_string(),
    );
    assert_eq!(r.outcome, ToolOutcome::Timeout, "{}", r.render());
    let rendered = r.render();
    assert!(rendered.contains("Killed"), "{rendered}");
    assert!(
        rendered.contains("background: true") && !rendered.contains("larger"),
        "the timeout names background as the way out: {rendered}"
    );
}

// --------------------------------------- two ways in, and a deadline that is not one

/// Two ways into the background (the model asks, a person moves it) and the deadline as a
/// third path that is not a backgrounding: three facts the model must be able to tell
/// apart.
#[test]
fn the_ways_into_the_background_and_the_deadline_are_distinguishable() {
    let mut h = runner!("ways");

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

    // A person moves one mid-flight: the daemon-side verb behind the head's Ctrl+B, which
    // is why this drives `promote` rather than a `bash` argument.
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

    // The deadline is the third path, and it must not read as either backgrounding.
    let killed = h.call(
        "bash",
        &serde_json::json!({"command": "sleep 30", "timeout_ms": 300}).to_string(),
    );
    assert_eq!(killed.outcome, ToolOutcome::Timeout, "{}", killed.render());

    // Each way in reads differently, or the model cannot tell which one happened.
    assert_ne!(Backgrounding::Asked.phrasing(), p.how.phrasing());

    for id in [asked_id, third_id] {
        let _ = host.kill_job(&JobId(id));
    }
}

/// The promotion record is a measurement, not a boolean: a move that took nothing and one
/// that took three must not read alike.
#[test]
fn the_promotion_record_carries_what_moved_and_reaches_the_model_through_job_list() {
    let mut h = runner!("promotion_record");
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

    // A second promotion of the same job moves nothing and must say so, rather than
    // produce a record that reads as another move.
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

/// The head's Ctrl+B, which arrives as a promote request on the channel the daemon wires
/// from the hub while `bash` is blocked in its wait loop.
#[test]
fn a_promote_request_from_the_head_moves_the_running_command() {
    let mut h = runner!("promote_from_head");
    // The request is in place before the call, so the wait loop finds it.
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

/// A run moved to the background mid-flight keeps what it wrote on both sides of the move: the promotion
/// seeds the file from the window and the drains keep filling it. The command prints on a clock, so
/// before and after are facts about time and not about a race.
#[test]
fn a_promoted_run_keeps_the_output_from_before_and_after_the_move() {
    let mut h = runner!("promote_keeps_output");
    // The Ctrl+O on another thread: the runner fixture has no head, so the request is set
    // on the channel the daemon wires from the hub while `bash` blocks in its wait loop.
    let channel = h.promote.clone().expect("a promote channel");
    let promoted_at = std::time::Instant::now();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(1200));
        *channel.lock().unwrap_or_else(|e| e.into_inner()) = Some("deadtrickster".to_string());
    });
    let r = h.call(
        "bash",
        &serde_json::json!({
            "command": "for i in 1 2 3 4 5 6 7 8; do echo line-$i; sleep 0.4; done"
        })
        .to_string(),
    );
    let handle = match &r.outcome {
        ToolOutcome::Backgrounded { handle, how, .. } => {
            assert_eq!(
                *how,
                Backgrounding::Operator {
                    identity: "deadtrickster".into()
                }
            );
            handle.clone()
        }
        other => panic!("the Ctrl+O did not move the run: {other:?}\n{}", r.render()),
    };
    let host = h.processes.clone().unwrap();
    let jid = JobId(handle.clone());

    // The file the promotion made, and the path the result must name: the call that lands
    // is the last moment anything tells this caller where its run went.
    let file = host
        .job(&jid)
        .and_then(|v| v.output_file)
        .expect("a promoted run's output keeps going into a file");
    assert!(
        file.path.ends_with(&format!("job-{handle}.out")),
        "the file is named after the job: {}",
        file.path
    );
    assert_eq!(
        file.from, 0,
        "nothing was dropped before the move, so it starts at 0"
    );
    let rendered = r.render();
    assert!(
        rendered.contains(&format!("job=\"{handle}\"")),
        "the result must name the handle: {rendered}"
    );
    assert!(
        rendered.contains(&file.path),
        "the result must name the file its output is going into: {rendered}"
    );

    // `job_wait` returns at once for a job this session started, so the wait is a poll on
    // the state, not a clock the harness invents.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while host
        .job(&jid)
        .map(|v| v.state.is_running())
        .unwrap_or(false)
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(
        !host.job(&jid).map(|v| v.state.is_running()).unwrap_or(true),
        "the command never finished"
    );
    assert!(
        promoted_at.elapsed() > std::time::Duration::from_secs(1),
        "the promote has to have landed mid-flight for this to be about anything"
    );

    // `job_output` must return both sides of the move: the pre-move lines are the ones the
    // foreground result carried, and the post-move ones only exist because the window kept
    // filling after the promotion.
    let read = h.call(
        "job_output",
        &serde_json::json!({"job": handle}).to_string(),
    );
    let body = read.render();
    for n in 1..=8 {
        assert!(
            body.contains(&format!("line-{n}\n")),
            "line-{n} is missing from `job_output`: {body}"
        );
    }
    assert!(
        body.contains(&file.path),
        "`job_output` must name the file that keeps the whole of it: {body}"
    );

    // Read from the disk, not through the harness: the file must hold every line, and
    // asking the tool whether it wrote is asking its opinion of itself.
    let on_disk = std::fs::read_to_string(&file.path).expect("the scratch file exists");
    for n in 1..=8 {
        assert!(
            on_disk.contains(&format!("line-{n}\n")),
            "line-{n} is missing from `{}`: {on_disk}",
            file.path
        );
    }
    assert!(
        file.path.starts_with(
            h.scratch_dir()
                .expect("the runner fixture has a scratch")
                .to_str()
                .unwrap()
        ),
        "the file belongs in the session's scratch directory: {}",
        file.path
    );
}

/// A run that began in the background has a file holding all of it, since the harness sends exactly these
/// long runs to `background: true`. Three readers must name it: the `bash` result, `job_output` and
/// `job_list`.
#[test]
fn a_run_started_in_the_background_gets_a_file_holding_all_of_it() {
    let mut h = runner!("background_spill");
    let scratch = h
        .scratch_dir()
        .expect("the runner fixture has a scratch")
        .to_path_buf();
    // Past the window's 8 MiB, so the ring must drop a front the file must not. The
    // markers at each end make "from byte 0" checkable rather than a length somebody could
    // have got right by accident.
    let r = h.call(
        "bash",
        &serde_json::json!({
            "command": "printf 'FIRST\\n'; yes x | head -c 9000000; printf '\\nLAST\\n'",
            "background": true,
        })
        .to_string(),
    );
    let handle = match &r.outcome {
        ToolOutcome::Backgrounded { handle, .. } => handle.clone(),
        other => panic!(
            "the model asked for the background: {other:?}\n{}",
            r.render()
        ),
    };
    let host = h.processes.clone().unwrap();
    let jid = JobId(handle.clone());

    // The file is opened before a byte is drained, so the result that hands the handle back
    // is already able to name it.
    let file = host
        .job(&jid)
        .and_then(|v| v.output_file)
        .expect("a run with no deadline has its file from the spawn");
    assert!(
        file.path.ends_with(&format!("job-{handle}.out")),
        "the file is named after the job: {}",
        file.path
    );
    assert_eq!(
        file.from, 0,
        "nothing had been printed when it was opened, so it starts where the job did"
    );
    assert!(
        file.path.starts_with(scratch.to_str().unwrap()),
        "the file belongs in the session's scratch directory: {}",
        file.path
    );
    let rendered = r.render();
    assert!(
        rendered.contains(&format!("job=\"{handle}\"")),
        "the result must name the handle: {rendered}"
    );
    assert!(
        rendered.contains(&file.path),
        "the result must name the file that keeps the whole of it: {rendered}"
    );

    // `job_wait` returns at once here too, so the wait is a poll on the state.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while host
        .job(&jid)
        .map(|v| v.state.is_running())
        .unwrap_or(false)
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(
        !host.job(&jid).map(|v| v.state.is_running()).unwrap_or(true),
        "the command never finished"
    );

    // The window must have dropped its front, or this is a test about a short run.
    let view = host.job(&jid).expect("the row stays");
    let produced = view.produced;
    assert!(
        produced > 8 * 1024 * 1024,
        "the run has to outgrow the window for this to be about anything: {produced} bytes"
    );
    let window = host.output(&jid, 0, 64).expect("the row stays");
    assert!(
        window.dropped > 0,
        "the window kept everything, so nothing was measured: {produced} produced"
    );

    // Read from the disk, not through the harness: the file must hold every byte from the
    // job's first, and asking the tool would be asking its opinion of itself.
    let on_disk = std::fs::read(&file.path).expect("the scratch file exists");
    assert_eq!(
        on_disk.len() as u64,
        produced,
        "the file holds all {produced} bytes, not the window's {}",
        window.retained
    );
    assert!(
        on_disk.starts_with(b"FIRST\n"),
        "the file starts where the job did: {:?}",
        String::from_utf8_lossy(&on_disk[..on_disk.len().min(40)])
    );
    assert!(
        on_disk.ends_with(b"\nLAST\n"),
        "and it kept up to the end: {:?}",
        String::from_utf8_lossy(&on_disk[on_disk.len().saturating_sub(40)..])
    );

    // `job_output` returns the window and names the file, which is where a dropped front
    // stops being gone.
    let read = h.call(
        "job_output",
        &serde_json::json!({"job": handle, "limit": 64}).to_string(),
    );
    let body = read.render();
    assert!(
        body.contains(&file.path),
        "`job_output` must name the file that keeps the whole of it: {body}"
    );
    assert!(
        body.contains("not lost"),
        "a window that dropped its front must say where those bytes are: {body}"
    );

    // `job_list` too: a row showing only the window's byte count would be the one place that
    // did not say where the rest is.
    let listing = h.call("job_list", &serde_json::json!({"all": true}).to_string());
    assert!(
        listing.render().contains(&file.path),
        "`job_list` must name the file too: {}",
        listing.render()
    );
}

// ------------------------------------------------------------------------- monitors

/// A monitor is owned, listed, says why it fired, and refuses a second under one name, all
/// through the tool the model actually calls.
#[test]
fn a_monitor_is_owned_listed_says_why_it_fired_and_refuses_a_second_under_one_name() {
    let mut h = runner!("monitor_tool");
    let declared = h.call(
        "monitor",
        &serde_json::json!({"name": "built", "path": "marker-a"}).to_string(),
    );
    assert_eq!(declared.outcome, ToolOutcome::Ok, "{}", declared.render());
    assert!(declared.payload.contains("owner:"), "{}", declared.payload);
    assert!(declared.payload.contains("session"), "{}", declared.payload);

    // Listed where people already look for jobs, and attributable there: an invisible
    // watcher is an unreapable one.
    let listing = h.call("job_list", "{}");
    assert!(listing.payload.contains("built"), "{}", listing.payload);
    assert!(
        listing.payload.contains("declared by"),
        "an invisible watcher is an unreapable one: {}",
        listing.payload
    );

    // One waiter per name, and the refusal hands over what holds it.
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

    // Why it fired, not that it did.
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

/// An owner is only an owner if its end takes what it owns, so ending a scope must reap
/// the monitor and the record must say which scope took it.
#[test]
fn a_monitor_dies_with_the_scope_that_owns_it() {
    let mut h = runner!("monitor_reaped");
    // The turn scope has to exist before a monitor can be named into it.
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

/// Nothing a monitor is asked to watch may be the process asking, asserted where a model
/// would hit it: the test's own name and its ancestors must not be found.
#[test]
fn a_monitor_cannot_be_asked_to_watch_a_process_by_name() {
    let mut h = runner!("monitor_no_pattern");
    // Nothing to watch: the refusal must name the conditions it accepts, and rule out the
    // process one being this daemon.
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

    // Two conditions is one monitor answering two questions, which answers neither.
    let two = h.call(
        "monitor",
        &serde_json::json!({"name": "x", "path": "a", "port": 9}).to_string(),
    );
    assert!(
        matches!(two.outcome, ToolOutcome::Failed { .. }),
        "{}",
        two.render()
    );

    // The seated schema must have nowhere to put a pattern: a self-matched process check
    // is a monitor that kills the command it was watching.
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
    // `command` is a condition (run one, fire on its exit) and `process` is a string the
    // finder consumes once; neither is a pattern the poller matches.
    for banned in ["pattern", "match", "cmdline", "regex", "host"] {
        assert!(
            !names.iter().any(|n| n.contains(banned)),
            "`monitor` grew `{banned}`: {names:?}"
        );
    }
}

/// `pkill` through the seated runtime: the listing, a refusal for a pid the listing did
/// not show, and a kill by pid. PROTECTED pids are the tool's own unit test's subject.
#[test]
fn pkill_lists_and_kills_by_pid_only() {
    use std::process::{Command, Stdio};
    let mut h = runner!("pkill_tool");
    h.rt.registry
        .register(Box::new(letibot_tools::builtins::pkill::Pkill))
        .unwrap();
    let marker = format!("letibot-bg-pkill-{}", std::process::id());
    // `; :` keeps the shell (and the marker on its command line): macOS's `sh` is bash,
    // which would otherwise `exec` the last command in place.
    let mut go = Command::new("sh")
        .arg("-c")
        .arg("sleep 30; :")
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

// ------------------------------------------------------------------- the wait declines

/// A `job_wait` on a job the harness is already delivering returns at once, because the daemon submits
/// that settlement as a turn of its own. Elapsed time is the measurement, since text alone passes for a
/// wait that blocked its whole deadline.
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

    // The daemon's own answer, forced: in a real session this closure reads the per-session
    // job watchers, armed when the `Backgrounded` result passed the sink.
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
    // Not a deadline and not a failure: the call was reasonable and the answer is already
    // on its way.
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

    // The job is untouched: the call declined to wait, it did not stop anything.
    let host = h.processes.clone().unwrap();
    let view = host
        .job(&JobId(id.clone()))
        .expect("the job is still known");
    assert!(view.state.is_running(), "the wait must not stop the job");
    let _ = h.call("job_kill", &serde_json::json!({"job": id}).to_string());
}

/// The contrast: a `job_wait` on a job nobody is delivering still blocks for its whole
/// deadline, because only the delivered case may return early. The deadline is short so the
/// test is, and reaching it is the whole of "blocks".
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

    // The same closure, watching something else entirely: the honest shape of "this job is
    // not one the daemon will hand you".
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

// --------------------------------------------------------------- the redirected job

/// A job whose output is redirected says so when it is backgrounded: the redirect is in the
/// text and its captured output is empty by construction. The note carries the rule too,
/// because a silent cost sends the model looking for another spelling of the same effect.
#[test]
fn a_backgrounded_job_with_a_redirect_says_its_output_is_not_captured() {
    let mut h = runner!("redirected_background");
    let r = h.call(
        "bash",
        &serde_json::json!({
            "command": "echo building > out.log 2>&1; sleep 30",
            "background": true
        })
        .to_string(),
    );
    let body = r.render();
    assert!(
        matches!(r.outcome, ToolOutcome::Backgrounded { .. }),
        "it must still be a backgrounding: {body}"
    );
    assert!(
        body.contains("out.log"),
        "the note must name the path the output goes to, so the model can read it: {body}"
    );
    assert!(
        body.contains("EMPTY"),
        "the redirect's cost must be stated, not left to be discovered: {body}"
    );
    // The payload must not go on promising the window: a note that argues with the line under
    // it is worse than no note, because the model believes the line it read first.
    assert!(
        !body.contains("reads what it has written so far"),
        "the payload still sends the model to a window that will stay empty: {body}"
    );
    // The rule, not a list of verbs: naming `sleep` alone closes one spelling of the
    // behaviour and leaves the rest.
    assert!(
        body.contains("do not build your own clock"),
        "the note must state the rule rather than one spelling of it: {body}"
    );
    assert!(
        body.contains(letibot_tools::builtins::DELIVERED),
        "the rule must carry its REASON — that the completion reaches you — rather than only \
         forbidding a verb: {body}"
    );
    let id = job_id_anywhere(&body).expect("a job id");
    let _ = h.call("job_kill", &serde_json::json!({"job": id}).to_string());
}

/// A job that is not redirected is told none of it: the note is a fact about the command,
/// and announcing it over every background job would report a capture that is not broken.
#[test]
fn a_backgrounded_job_without_a_redirect_says_nothing_about_one() {
    let mut h = runner!("plain_background");
    let r = h.call(
        "bash",
        &serde_json::json!({"command": "sleep 30", "background": true}).to_string(),
    );
    let body = r.render();
    assert!(
        !body.contains("not captured"),
        "an ordinary background job was told its output is not captured: {body}"
    );
    let id = job_id_anywhere(&body).expect("a job id");
    let _ = h.call("job_kill", &serde_json::json!({"job": id}).to_string());
}

/// `job_output` on a redirected job is an answer, not an empty window: "wrote nothing at
/// all" would be false about a build writing a log as fast as it can.
#[test]
fn job_output_on_a_redirected_job_names_the_file_instead_of_an_empty_window() {
    let mut h = runner!("redirected_output");
    let r = h.call(
        "bash",
        &serde_json::json!({
            "command": "echo one > out.log; sleep 30",
            "background": true
        })
        .to_string(),
    );
    let id = job_id_anywhere(&r.render()).expect("a job id");
    let o = h.call(
        "job_output",
        &serde_json::json!({"job": id.clone()}).to_string(),
    );
    let body = o.render();
    // `out.log` is not enough: the fall-through prints `command: ...`, which carries the path
    // too. What it must say is where the output goes and that the window is empty because of
    // that, and it must not say the job has written nothing.
    assert!(
        body.contains("redirected to") || body.contains("output goes to"),
        "the answer must say the output goes to the file, not merely print the command: {body}"
    );
    for wrong in [
        "wrote nothing at all",
        "written nothing yet",
        "still running",
    ] {
        assert!(
            !body.contains(wrong),
            "a redirected job answered as `{wrong}`: {body}"
        );
    }
    // An abstention, not an `ok` carrying an empty window: "you asked for something that is
    // not there" is the same shape of fact the never-ran case has.
    assert!(
        matches!(o.outcome, ToolOutcome::Abstained { .. }),
        "a zero-byte window must abstain rather than answer ok: {body}"
    );
    let _ = h.call("job_kill", &serde_json::json!({"job": id}).to_string());
}

// ------------------------------------------------- the name, from end to end

/// A named job is called by its name everywhere a sentence calls it, and the id still works
/// everywhere a call needs a handle: the label is what a reader sees, the id is what a call
/// passes, and a label that only half resolves must not be handed over as a handle.
#[test]
fn a_named_job_is_called_by_its_name_and_its_id_still_resolves() {
    let mut h = runner!("named_job_label");
    let started = h.call(
        "bash",
        &serde_json::json!({
            "command": "sleep 30",
            "background": true,
            "slug": "release-build"
        })
        .to_string(),
    );
    let id = match &started.outcome {
        ToolOutcome::Backgrounded { handle, .. } => handle.clone(),
        other => panic!("{other:?}: {}", started.render()),
    };
    let body = started.render();
    // The label, spelled exactly as the settlement notice will spell it.
    let label = format!("release-build · {id}");
    assert!(
        body.contains(&format!("started `{label}` in the background")),
        "the bash result is where the operator first meets the job: {body}"
    );
    // The handle it hands over must resolve: the name, and the id beside it.
    assert!(
        body.contains(&format!(
            "`job_output`, `job_kill` and `job_wait` take this or `{id}`"
        )),
        "the resolvable handles must be spelled out, because the label is not one: {body}"
    );

    // Read back by the name the model stated.
    let by_name = h.call(
        "job_output",
        &serde_json::json!({"job": "release-build"}).to_string(),
    );
    let named = by_name.render();
    assert!(
        named.contains(&format!("`{label}` has produced no output")),
        "a result names the job by its label, whoever asked by whatever handle: {named}"
    );
    // No argument it suggests may be the label, which does not resolve: a running job's
    // completion is delivered, so no `job_wait` is suggested at all.
    assert!(
        !named.contains(&format!("job=\"{label}\"")),
        "a suggested argument must be a resolvable one: {named}"
    );

    // And by the id, with the same label in the answer.
    let by_id = h.call(
        "job_output",
        &serde_json::json!({"job": id.clone()}).to_string(),
    );
    assert!(
        by_id.render().contains(&format!("`{label}`")),
        "the same job, the same name, whichever handle was written: {}",
        by_id.render()
    );

    let _ = h.call("job_kill", &serde_json::json!({"job": id}).to_string());
}
