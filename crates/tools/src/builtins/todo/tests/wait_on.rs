//! `wait_on`: marking a row already on the board as waiting on a job or a subagent.

use super::*;

use crate::exec::{JobId, JobState, JobView, MemoryCap, ScopeId, ScopeKind};

/// A job, as this session's table holds it - the fields `wait_on` reads (the handle, the name,
/// the state) and the rest filled with something obvious.
fn job(id: &str, slug: Option<&str>, state: JobState) -> JobView {
    let scope = || ScopeId {
        kind: ScopeKind::Session,
        name: "s".into(),
        path: std::path::PathBuf::from("/sys/fs/cgroup/x"),
    };
    JobView {
        id: JobId(id.into()),
        command: "scripts/correctness_gate.sh".into(),
        slug: slug.map(|s| s.to_string()),
        scope: scope(),
        owner: scope(),
        cwd: "/".into(),
        pid: 1,
        background: Some(letibot_transcript::Backgrounding::Asked),
        state,
        elapsed: Duration::from_secs(1),
        ran_for: None,
        produced: 0,
        since_last_output: None,
        memory_cap: MemoryCap::NotAsked,
        memory: None,
        output_file: None,
        env_report: None,
    }
}

/// The pg-noop row, as the operator's case has it: a CRITERION that stays open until a gate a job
/// is running goes green.
const CRITERION: &str = "CRITERION, NOT A TASK — stays open until the gate is green";

fn operator_row(content: &str, status: TodoStatus) -> TodoItem {
    TodoItem {
        by: TodoBy::Operator,
        ..item(content, status)
    }
}

fn running_gate() -> Vec<JobView> {
    vec![job("j123", Some("gate"), JobState::Running)]
}

fn refusal(inv: &Invocation) -> String {
    format!("{:?} {}", inv.outcome, inv.payload)
}

fn ok(inv: &Invocation) -> bool {
    inv.outcome == ToolOutcome::Ok
}

/// A wait puts the row on the job the way a `needs` entry does, so the reading that keeps the check
/// silent is one the row already has: quiet while the job runs, askable again once it ends.
#[test]
fn a_wait_on_the_models_own_row_quiets_it_while_the_job_runs_and_not_after() {
    let board = Arc::new(TodoBoard::new(vec![item(
        CRITERION,
        TodoStatus::InProgress,
    )]));
    let tool = TodoWriteTool::new(board.clone());
    let before = board.version();
    // By its NAME - the full handle is what goes on the row.
    let r = tool.wait_on(
        &json!([{"content": CRITERION, "job": "gate"}]),
        Some(&running_gate()),
    );
    assert!(ok(&r), "{}", refusal(&r));
    let row = &board.snapshot()[0];
    assert_eq!(
        row.needs,
        vec![TodoNeed::Job {
            handle: "j123".into()
        }]
    );
    assert_eq!(
        row.status,
        TodoStatus::InProgress,
        "the status is untouched"
    );
    assert_eq!(row.content, CRITERION, "and so are the words");
    assert!(
        board.version() > before,
        "a wait is a board move, announced like one"
    );

    let running = RunningJobs::none().running("j123");
    assert!(
        waits_on_a_running_job(row, &running),
        "while the job runs the row is one the check may not speak about"
    );
    // The job ends, and the row is the check's to name again.
    let said = nag_with(
        &board.snapshot(),
        &ChildSessions::none(),
        &RunningJobs::none(),
    )
    .expect("the row is open work once the job is over");
    assert!(said.contains(&format!("  - {CRITERION}")), "{said}");

    // Attaching it again is not a change.
    let v = board.version();
    let r = tool.wait_on(
        &json!([{"content": CRITERION, "job": "j123"}]),
        Some(&running_gate()),
    );
    assert!(ok(&r), "{}", refusal(&r));
    assert_eq!(board.version(), v, "a wait already there is not a move");
}

/// A wait says "not now", never "not this", so it is the one thing the model may set on the
/// operator's row: their words and status stay put.
#[test]
fn a_wait_on_the_operators_row_leaves_their_words_and_status_alone() {
    let board = Arc::new(TodoBoard::new(vec![operator_row(
        CRITERION,
        TodoStatus::InProgress,
    )]));
    let tool = TodoWriteTool::new(board.clone());
    let r = tool.wait_on(
        &json!([{"content": CRITERION, "job": "j123"}]),
        Some(&running_gate()),
    );
    assert!(ok(&r), "{}", refusal(&r));
    let theirs = board.operator_snapshot();
    assert_eq!(theirs.len(), 1);
    assert_eq!(theirs[0].content, CRITERION);
    assert_eq!(theirs[0].status, TodoStatus::InProgress);
    assert_eq!(theirs[0].by, TodoBy::Operator, "still theirs");
    assert!(
        board.model_snapshot().is_empty(),
        "and not copied into the model's half"
    );
    assert!(waits_on_a_running_job(
        &theirs[0],
        &RunningJobs::none().running("j123")
    ));
    let said = nag_with(
        &board.snapshot(),
        &ChildSessions::none(),
        &RunningJobs::none(),
    )
    .expect("named again once the job is over");
    assert!(said.contains(CRITERION), "{said}");

    // A child of this session is a wait too, by the id `task` handed back.
    let (board, fake) = {
        let board = Arc::new(TodoBoard::new(vec![operator_row(
            "collect the answer",
            TodoStatus::Pending,
        )]));
        let fake = Arc::new(FakeChildren {
            board: Arc::new(TodoBoard::new(vec![])),
        });
        (board, fake)
    };
    let tool = TodoWriteTool::with_children(board.clone(), fake);
    let r = tool.wait_on(
        &json!([{"content": "collect the answer", "child": "s-1-sub-2"}]),
        None,
    );
    assert!(ok(&r), "{}", refusal(&r));
    assert_eq!(
        board.operator_snapshot()[0].needs,
        vec![TodoNeed::Child {
            id: "s-1-sub-2".into()
        }]
    );
    // And a session that is not this one's child is refused by name.
    let r = tool.wait_on(
        &json!([{"content": "collect the answer", "child": "s-9-sub-9"}]),
        None,
    );
    assert!(!ok(&r));
    assert!(refusal(&r).contains("s-9-sub-9"), "{}", refusal(&r));
}

/// A handle this session does not hold is refused by name, and nothing is written - and so is
/// one that has already ended, which nothing would quiet anyway.
#[test]
fn an_unknown_or_ended_job_is_refused_by_name() {
    let board = Arc::new(TodoBoard::new(vec![item(
        CRITERION,
        TodoStatus::InProgress,
    )]));
    let tool = TodoWriteTool::new(board.clone());
    let v = board.version();

    let r = tool.wait_on(
        &json!([{"content": CRITERION, "job": "j999"}]),
        Some(&running_gate()),
    );
    assert!(!ok(&r));
    assert!(
        refusal(&r).contains("no job called `j999`"),
        "{}",
        refusal(&r)
    );

    let ended = vec![job("j7", None, JobState::Exited { code: 0 })];
    let r = tool.wait_on(&json!([{"content": CRITERION, "job": "j7"}]), Some(&ended));
    assert!(!ok(&r));
    assert!(refusal(&r).contains("already ended"), "{}", refusal(&r));

    // No process host at all: no job here could ever end the wait.
    let r = tool.wait_on(&json!([{"content": CRITERION, "job": "j123"}]), None);
    assert!(!ok(&r));
    assert!(refusal(&r).contains("`j123`"), "{}", refusal(&r));

    // A row nobody wrote is refused with the rows that are there.
    let r = tool.wait_on(
        &json!([{"content": "no such row", "job": "j123"}]),
        Some(&running_gate()),
    );
    assert!(!ok(&r));
    assert!(refusal(&r).contains(CRITERION), "{}", refusal(&r));

    // And a child with no resolver seated is refused rather than waved through.
    let r = tool.wait_on(&json!([{"content": CRITERION, "child": "s-1-sub-2"}]), None);
    assert!(!ok(&r));

    assert_eq!(board.version(), v, "no refusal moved the board");
    assert!(board.snapshot()[0].needs.is_empty());
}

/// Detach takes the job and subagent waits off and leaves the plan's own order.
#[test]
fn detach_restores_the_row() {
    let row = TodoItem {
        needs: vec![TodoNeed::Row {
            content: "run the tests".into(),
        }],
        ..operator_row("deploy", TodoStatus::Pending)
    };
    let original = row.clone();
    let board = Arc::new(TodoBoard::new(vec![
        item("run the tests", TodoStatus::Completed),
        row,
    ]));
    let tool = TodoWriteTool::new(board.clone());
    let r = tool.wait_on(
        &json!([{"content": "deploy", "job": "gate"}]),
        Some(&running_gate()),
    );
    assert!(ok(&r), "{}", refusal(&r));
    assert_eq!(board.operator_snapshot()[0].needs.len(), 2);

    let r = tool.wait_on(&json!([{"content": "deploy", "detach": true}]), None);
    assert!(ok(&r), "{}", refusal(&r));
    assert_eq!(
        board.operator_snapshot()[0],
        original,
        "the row is exactly what it was before the wait went on — its row edge included"
    );
}

/// The shape is refused by name: two waits in one entry, an unknown field, and `wait_on` beside
/// `todos` - through the real runtime, so the top-level door is the one under test.
#[test]
fn a_malformed_wait_on_is_refused_and_writes_nothing() {
    let (rt, board) = runtime_with(vec![item(CRITERION, TodoStatus::InProgress)]);
    let mut sink = RecordingToolSink::new();
    for (args, says) in [
        (
            r#"{"wait_on": [{"content": "x", "job": "j1", "child": "s-1"}]}"#,
            "names 2 of",
        ),
        (
            r#"{"wait_on": [{"content": "x", "until": "j1"}]}"#,
            "`until`",
        ),
        (
            r#"{"wait_on": [{"content": "x", "job": "j1"}], "todos": []}"#,
            "do not mix",
        ),
        // A backend with no process host has no job to wait on.
        (
            r#"{"wait_on": [{"content": "x", "job": "j1"}]}"#,
            "runs no jobs",
        ),
    ] {
        let r = rt.invoke("t", &call(args), &mut sink);
        let said = format!("{:?} {}", r.outcome, r.payload);
        assert!(
            matches!(r.outcome, ToolOutcome::Failed { .. }),
            "{args}: {said}"
        );
        assert!(said.contains(says), "{args}: {said}");
    }
    assert!(board.snapshot()[0].needs.is_empty());
}

/// The check offers the door when there is something to wait on, and only then.
#[test]
fn the_check_offers_wait_on_when_a_job_or_a_subagent_is_running() {
    let plan = vec![operator_row(CRITERION, TodoStatus::InProgress)];
    let quiet = nag_with(&plan, &ChildSessions::none(), &RunningJobs::none()).unwrap();
    // The in-progress line still names `wait_on` in words; the offer is the call with a handle.
    assert!(
        !quiet.contains("{\"wait_on\""),
        "nothing running, nothing to offer: {quiet}"
    );

    let busy = nag_with(
        &plan,
        &ChildSessions::none(),
        &RunningJobs::none().running("j123"),
    )
    .unwrap();
    assert!(
        busy.contains(r#"{"wait_on": [{"content": "<the text above>", "job": "j123"}]}"#),
        "the offer is a call that can be sent as it stands: {busy}"
    );
    assert!(
        busy.contains("this check stays quiet until it ends"),
        "{busy}"
    );
    // On the advice's own line, so the head's notice fold still folds it: the notice keeps
    // its three lines - head, bullet, advice.
    assert_eq!(busy.lines().count(), 3, "{busy}");

    let child = nag_with(
        &plan,
        &ChildSessions::none(),
        &RunningJobs::none().subagent("s-1-sub-2"),
    )
    .unwrap();
    assert!(child.contains(r#""child": "s-1-sub-2""#), "{child}");

    // And not for a row `wait_on` cannot reach: a parent's row is its author's.
    let parents = vec![TodoItem {
        by: TodoBy::parent_of("s-1"),
        ..item("the parent's row", TodoStatus::Pending)
    }];
    let said = nag_with(
        &parents,
        &ChildSessions::none(),
        &RunningJobs::none().running("j123"),
    )
    .unwrap();
    assert!(!said.contains("wait_on"), "{said}");
}

/// A row that only reads like a running job is not taken as waiting on it: the wait is stated on
/// the row, so the check asks, and its offer names the job the row reads like.
#[test]
fn a_row_a_running_job_reads_like_is_asked_and_offered_that_job() {
    let plan = vec![operator_row(
        "correctness gate must PASS in RATCHET=0 — rerun scripts/correctness_gate.sh",
        TodoStatus::InProgress,
    )];
    let jobs = RunningJobs::none()
        .about(
            "j12",
            "pg-timing: cargo bench --bench timing > /tmp/pgt.log",
        )
        .running("j12")
        .about("j55", "gate: RATCHET=0 scripts/correctness_gate.sh")
        .running("j55");
    let said =
        nag_with(&plan, &ChildSessions::none(), &jobs).expect("an unstated wait is asked about");
    assert!(
        said.contains(r#""job": "j55""#),
        "the offer names the job the row reads like: {said}"
    );
}

/// The offer is for a row that is only waiting, so where nothing running is alike it still names
/// what it could attach - without claiming a likeness it cannot see.
#[test]
fn the_offer_still_names_a_running_job_the_row_does_not_read_like() {
    let plan = vec![operator_row(
        "correctness gate must PASS in RATCHET=0 — rerun scripts/correctness_gate.sh",
        TodoStatus::InProgress,
    )];
    // Nothing alike: the likeliest is still offered, and nothing claims a likeness.
    let unrelated = RunningJobs::none()
        .about("j12", "cargo bench")
        .running("j12");
    let said = nag_with(&plan, &ChildSessions::none(), &unrelated).unwrap();
    assert!(said.contains(r#""job": "j12""#), "{said}");
    assert!(!said.contains("reads like"), "{said}");
}

/// An ending the model has not digested keeps the row quiet: the job is over but its completion has
/// not reached the model, so `RunningJobs::digesting` holds the check off.
#[test]
fn an_ending_the_model_has_not_digested_keeps_the_row_quiet() {
    let mut row = item(CRITERION, TodoStatus::InProgress);
    row.needs = vec![TodoNeed::Job {
        handle: "j123".into(),
    }];
    let grace = RunningJobs::none().digesting("j123", Duration::from_secs(540));
    assert!(
        waits_on_an_undigested_ending(&row, &grace),
        "ended but not digested: the check may not speak about the row"
    );
    assert!(
        !waits_on_a_running_job(&row, &grace),
        "and it is not called running — it has ended"
    );
    assert_eq!(
        when_an_ending_is_digested(std::slice::from_ref(&row), &grace),
        Some(Duration::from_secs(540)),
        "the backstop is the silence's own deadline"
    );
    // A caller that does not narrow is told the row's answer is on its way, not asked to do it.
    let said = nag_with(std::slice::from_ref(&row), &ChildSessions::none(), &grace)
        .expect("an unnarrowed plan still says something");
    assert!(
        said.contains("`j123`, which has ended — its completion is reaching you"),
        "{said}"
    );

    // Digested - the harness stops reporting it - and the row is the check's to name again.
    assert!(!waits_on_an_undigested_ending(&row, &RunningJobs::none()));
    assert_eq!(
        when_an_ending_is_digested(std::slice::from_ref(&row), &RunningJobs::none()),
        None
    );
    let said = nag_with(
        std::slice::from_ref(&row),
        &ChildSessions::none(),
        &RunningJobs::none(),
    )
    .unwrap();
    assert!(said.contains(&format!("  - {CRITERION}")), "{said}");
}

/// The same grace for a SUBAGENT a row waits on - a child that has finished is absent from
/// `ChildSessions`; its undigested completion keeps
/// the row quiet the way a job's does.
#[test]
fn a_finished_childs_undigested_completion_keeps_the_row_quiet() {
    let mut row = operator_row("make tier 1 green", TodoStatus::InProgress);
    row.needs = vec![TodoNeed::Child {
        id: "s-1-sub-4".into(),
    }];
    let grace = RunningJobs::none().digesting("s-1-sub-4", Duration::from_secs(60));
    assert!(waits_on_an_undigested_ending(&row, &grace));
    assert_eq!(
        when_an_ending_is_digested(std::slice::from_ref(&row), &grace),
        Some(Duration::from_secs(60))
    );
    // A RUNNING job is not graced, even if a stale record names it: running is its own quiet.
    let mut job_row = item(CRITERION, TodoStatus::InProgress);
    job_row.needs = vec![TodoNeed::Job {
        handle: "j9".into(),
    }];
    let both = RunningJobs::none()
        .running("j9")
        .digesting("j9", Duration::from_secs(60));
    assert!(!waits_on_an_undigested_ending(&job_row, &both));
    assert_eq!(
        when_an_ending_is_digested(std::slice::from_ref(&job_row), &both),
        None
    );
}

/// A job's name in `needs` is stored as its handle: the raw name finds no job in the table, and an
/// unknown fate reads as ENDED, which would ready the row while its job still runs.
#[test]
fn a_job_named_in_needs_is_stored_as_its_handle() {
    let board = Arc::new(TodoBoard::new(Vec::new()));
    let tool = TodoWriteTool::new(board.clone());
    let r = tool.write(
        &json!({"todos": [{"content": "push", "status": "pending",
                           "needs": [{"kind": "job", "handle": "gate"}]}]}),
        Some(&running_gate()),
    );
    assert!(ok(&r), "{}", refusal(&r));
    assert_eq!(
        board.snapshot()[0].needs,
        vec![TodoNeed::Job {
            handle: "j123".into()
        }],
        "the name was stored, not the handle every reader asks about"
    );
    let running = RunningJobs::none().running("j123");
    assert!(
        waits_on_a_running_job(&board.snapshot()[0], &running),
        "the row is quiet while its job runs"
    );

    // A handle this table does not hold is kept: it may be one the daemon ran before a restart.
    let r = tool.write(
        &json!({"todos": [{"content": "push", "status": "pending",
                           "needs": [{"kind": "job", "handle": "j7"}]}]}),
        Some(&running_gate()),
    );
    assert!(ok(&r), "{}", refusal(&r));
    assert_eq!(
        board.snapshot()[0].needs,
        vec![TodoNeed::Job {
            handle: "j7".into()
        }]
    );
}

/// A name no job has is refused, and so is one two jobs share - a wait on a name nothing will
/// ever end is a wait that reads as already over.
#[test]
fn a_job_need_naming_no_job_or_two_is_refused_and_nothing_is_written() {
    let board = Arc::new(TodoBoard::new(vec![item("keep me", TodoStatus::Pending)]));
    let tool = TodoWriteTool::new(board.clone());
    let v = board.version();
    let r = tool.write(
        &json!({"todos": [{"content": "push", "status": "pending",
                           "needs": [{"kind": "job", "handle": "release-build"}]}]}),
        Some(&running_gate()),
    );
    assert!(!ok(&r), "a name nothing holds was written");
    assert!(
        refusal(&r).contains("no job of this session has that name"),
        "{}",
        refusal(&r)
    );
    assert_eq!(board.version(), v, "nothing was written");

    let two = vec![
        job("j1", Some("gate"), JobState::Running),
        job("j2", Some("gate"), JobState::Running),
    ];
    let r = tool.write(
        &json!({"todos": [{"content": "push", "status": "pending",
                           "needs": [{"kind": "job", "handle": "gate"}]}]}),
        Some(&two),
    );
    assert!(!ok(&r), "an ambiguous name was written");
    assert!(
        refusal(&r).contains("2 jobs have that name"),
        "{}",
        refusal(&r)
    );
    assert_eq!(board.version(), v, "nothing was written");

    // With no process host there is no table, and the need is written as it was given.
    let r = tool.write(
        &json!({"todos": [{"content": "push", "status": "pending",
                           "needs": [{"kind": "job", "handle": "release-build"}]}]}),
        None,
    );
    assert!(ok(&r), "{}", refusal(&r));
}

/// A row waits on ALL of what it names - the operator: *"i think we can allow waiting on
/// multiple things - so AND"*. Two entries for one row put two edges on it, and the row stays
/// quiet until both jobs have ended; one ending is not enough.
#[test]
fn a_row_waits_on_every_job_it_names_until_all_of_them_end() {
    let board = Arc::new(TodoBoard::new(vec![item(
        CRITERION,
        TodoStatus::InProgress,
    )]));
    let tool = TodoWriteTool::new(board.clone());
    let jobs = vec![
        job("j1", Some("build"), JobState::Running),
        job("j2", Some("test"), JobState::Running),
    ];
    let r = tool.wait_on(
        &json!([
            {"content": CRITERION, "job": "build"},
            {"content": CRITERION, "job": "test"}
        ]),
        Some(&jobs),
    );
    assert!(ok(&r), "{}", refusal(&r));
    let row = &board.snapshot()[0];
    assert_eq!(
        row.needs,
        vec![
            TodoNeed::Job {
                handle: "j1".into()
            },
            TodoNeed::Job {
                handle: "j2".into()
            }
        ],
        "both edges, the first not replaced by the second"
    );
    assert!(waits_on_a_running_job(
        row,
        &RunningJobs::none().running("j1").running("j2")
    ));
    assert!(
        waits_on_a_running_job(row, &RunningJobs::none().running("j2")),
        "one job ending is not the row's wait ending"
    );
    assert!(
        !waits_on_a_running_job(row, &RunningJobs::none()),
        "both ended"
    );
}
