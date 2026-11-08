use super::*;
use letibot_tokencore::store::NoRecord;

/// Every record below was written by a daemon that is gone - what a restart reads. The rule about
/// a daemon that is still beating has its own test, which calls `super::decide` to say so.
fn decide(
    run: &SubagentRun,
    instance: &str,
    held: bool,
    dir_exists: impl Fn(&str) -> bool,
    mode: RestoreMode,
) -> Decision {
    super::decide(run, instance, held, false, dir_exists, mode)
}

fn resumable(
    run: &SubagentRun,
    instance: &str,
    held: bool,
    dir_exists: impl Fn(&str) -> bool,
) -> Result<TaskSpec, String> {
    super::resumable(run, instance, held, false, dir_exists)
}

fn run(state: &str) -> SubagentRun {
    SubagentRun {
        child_id: "s-1-sub-1".into(),
        parent_id: "s-1".into(),
        state: state.into(),
        prompt: "fix the thing".into(),
        spec_json: RecordedSpec::of(&TaskSpec {
            role: "coder".into(),
            worktree: Some(WorktreePlacement {
                path: "/wt/agent-fix".into(),
                branch: "agent/fix".into(),
                base_sha: "abc".into(),
                main_tree: false,
            }),
            model: Some("local".into()),
            ..TaskSpec::default()
        })
        .to_json(),
        instance: "old".into(),
        resumes: 0,
        outcome: None,
        later_answer: None,
        delivered: false,
        started_ms: 0,
        updated_ms: 0,
    }
}

const NEW: &str = "new";

/// A spawn whose row the store refused still leaves a record when the child settles. A
/// `database is locked` at the spawn is not dropped after a print, which would leave the child with
/// no record at all
#[test]
fn a_row_the_spawn_could_not_write_is_written_when_the_child_settles() {
    let path = std::env::temp_dir().join(format!(
        "letibot-record-pending-{}-{}.db",
        std::process::id(),
        letibot_sessionlog::registry::now_ms()
    ));
    for s in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{s}", path.display()));
    }
    let store = RunStore {
        path: path.clone(),
        cell: Default::default(),
    };
    let mut row = run("running");
    row.child_id = "s-1-sub-lost".into();
    let record = SubagentRecord {
        store: store.clone(),
        child: "s-1-sub-lost".into(),
        registry: Registry::new(),
        pending: Some(row),
    };
    assert!(
        store
            .with(|s| s.subagent_run("s-1-sub-lost").map_err(|e| e.to_string()))
            .unwrap()
            .is_none(),
        "the state this test is about: a spawn whose row was not written"
    );
    record.settled(
        &TaskStatus::Done {
            answer: "the sweep is in".into(),
            child: None,
        },
        false,
    );
    let written = store
        .with(|s| s.subagent_run("s-1-sub-lost").map_err(|e| e.to_string()))
        .unwrap()
        .expect("the row the spawn could not write is written at the settlement");
    assert_eq!(written.state, "done");
    assert_eq!(written.outcome.as_deref(), Some("the sweep is in"));
    assert_eq!(written.parent_id, "s-1");
    for s in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{s}", path.display()));
    }
}

/// A later turn the orderly stop interrupted is not the child's answer. `run_turn` hands back
/// the text it had when the daemon stopped it, so recording that makes the next daemon deliver a
/// fragment as the child's last word - the reading `settled` already refuses for the first turn.
#[test]
fn a_later_answer_the_shutdown_interrupted_is_not_recorded() {
    let path = std::env::temp_dir().join(format!(
        "letibot-record-stopping-{}-{}.db",
        std::process::id(),
        letibot_sessionlog::registry::now_ms()
    ));
    for s in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{s}", path.display()));
    }
    let store = RunStore {
        path: path.clone(),
        cell: Default::default(),
    };
    let mut row = run("done");
    row.outcome = Some("the answer it gave first".into());
    store
        .with(|s| s.put_subagent_run(&row).map_err(|e| e.to_string()))
        .unwrap();
    let registry = Registry::new();
    registry.close();
    let record = SubagentRecord {
        store: store.clone(),
        child: "s-1-sub-1".into(),
        registry,
        pending: None,
    };
    record.answered_again("half a sen");
    let got = store
        .with(|s| s.subagent_run("s-1-sub-1").map_err(|e| e.to_string()))
        .unwrap()
        .expect("the row");
    assert_eq!(
        got.later_answer, None,
        "the daemon's own interruption was written as the child's newest answer"
    );
    for s in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{s}", path.display()));
    }
}

#[test]
fn a_running_child_of_a_gone_daemon_is_resumed_with_its_own_spec() {
    match decide(&run("running"), NEW, false, |_| true, RestoreMode::Resume) {
        Decision::Resume { spec } => {
            assert_eq!(spec.role, "coder");
            assert_eq!(spec.model.as_deref(), Some("local"));
            assert_eq!(spec.worktree.unwrap().path, "/wt/agent-fix");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn past_the_threshold_a_running_child_is_only_reminded() {
    assert!(matches!(
        decide(&run("running"), NEW, false, |_| true, RestoreMode::Remind),
        Decision::Remind { .. }
    ));
    // A settled result is news either way, and is still delivered.
    let mut r = run("done");
    r.outcome = Some("x".into());
    assert!(matches!(
        decide(&r, NEW, false, |_| true, RestoreMode::Remind),
        Decision::Deliver(_)
    ));
}

#[test]
fn task_resume_refuses_what_it_must_not_bring_back() {
    assert!(resumable(&run("paused"), NEW, false, |_| true).is_ok());
    assert!(resumable(&run("abandoned"), NEW, false, |_| true).is_ok());
    assert!(resumable(&run("running"), NEW, false, |_| true).is_ok());
    for (state, says) in [("done", "finished"), ("killed", "stopped by a person")] {
        let why = resumable(&run(state), NEW, false, |_| true).unwrap_err();
        assert!(why.contains(says), "{why}");
    }
    assert!(
        resumable(&run("running"), "old", false, |_| true)
            .unwrap_err()
            .contains("running in this daemon")
    );
    assert!(
        resumable(&run("paused"), NEW, false, |_| false)
            .unwrap_err()
            .contains("is gone")
    );
}

#[test]
fn a_child_this_daemon_owns_is_left_alone() {
    assert_eq!(
        decide(&run("running"), "old", false, |_| true, RestoreMode::Resume),
        Decision::Leave
    );
    assert_eq!(
        decide(&run("running"), NEW, true, |_| true, RestoreMode::Resume),
        Decision::Leave
    );
}

/// Another daemon still beating is not a gone one: the store is shared by every
/// daemon on the box, so a running child of a live instance is left where it runs - resumed, it
/// would be a second copy in the same worktree - and `task_resume` is refused by name.
#[test]
fn a_record_whose_daemon_is_still_beating_is_left_alone() {
    for mode in [RestoreMode::Resume, RestoreMode::Remind] {
        assert_eq!(
            super::decide(&run("running"), NEW, false, true, |_| true, mode),
            Decision::Leave,
            "{mode:?}"
        );
    }
    // A tool's helper is the one a restart must not RETIRE - which would kill a live daemon's read.
    let mut helper = run("running");
    helper.prompt = "You are reading part 3 of 12 of a longer body of text on somebody else's \
                         behalf. They asked:\n\n  why"
        .into();
    helper.spec_json = r#"{"role":"researcher","placement":"host"}"#.into();
    assert!(is_helper(&helper));
    assert_eq!(
        super::decide(&helper, NEW, false, true, |_| true, RestoreMode::Resume),
        Decision::Leave
    );
    let why = super::resumable(&run("running"), NEW, false, true, |_| true)
        .expect_err("a live daemon's child is not brought back");
    assert!(
        why.contains("still up") && why.contains("Nothing was spawned"),
        "{why}"
    );
    // A settled child of a live daemon is still not news here: that daemon delivers it.
    let mut done = run("done");
    done.outcome = Some("x".into());
    assert_eq!(
        super::decide(&done, NEW, false, true, |_| true, RestoreMode::Resume),
        Decision::Leave
    );
}

#[test]
fn a_child_whose_worktree_is_gone_is_not_respawned() {
    match decide(&run("running"), NEW, false, |_| false, RestoreMode::Resume) {
        Decision::Abandon { why } => assert!(why.contains("/wt/agent-fix"), "{why}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_settled_undelivered_child_is_delivered_not_respawned() {
    let mut r = run("done");
    r.outcome = Some("all fixed".into());
    assert_eq!(
        decide(&r, NEW, false, |_| true, RestoreMode::Resume),
        Decision::Deliver(TaskStatus::Done {
            answer: "all fixed".into(),
            child: None
        })
    );
    r.delivered = true;
    assert_eq!(
        decide(&r, NEW, false, |_| true, RestoreMode::Resume),
        Decision::Leave
    );
    let mut f = run("failed");
    f.outcome = Some("it broke".into());
    assert_eq!(
        decide(&f, NEW, false, |_| true, RestoreMode::Resume),
        Decision::Deliver(TaskStatus::Failed {
            why: "it broke".into()
        })
    );
}

/// pg-noop's twelve part readers : settled and never marked delivered, because
/// `digest` read their answers itself - a restart handed all twelve to the model as just
/// finished. A helper, old record or new, is left when settled and retired silently mid-task.
#[test]
fn a_tools_helper_is_never_delivered_or_resumed() {
    let mut old = run("done");
    old.prompt = "You are reading part 3 of 12 of a longer body of text on somebody else's \
                      behalf. They asked:\n\n  why"
        .into();
    old.spec_json = r#"{"role":"researcher","placement":"host"}"#.into();
    old.outcome = Some("part 3 says".into());
    assert!(is_helper(&old));
    for mode in [RestoreMode::Resume, RestoreMode::Remind] {
        assert_eq!(decide(&old, NEW, false, |_| true, mode), Decision::Leave);
    }
    let mut marked = run("running");
    marked.spec_json = RecordedSpec::of(&TaskSpec {
        role: "researcher".into(),
        helper: true,
        ..TaskSpec::default()
    })
    .to_json();
    assert!(is_helper(&marked));
    assert!(matches!(
        decide(&marked, NEW, false, |_| true, RestoreMode::Resume),
        Decision::Retire { .. }
    ));
    // The model's own child with the same words in its brief, further in, is not one.
    let mut mine = run("done");
    mine.prompt = "Summarise it. You are reading part 1 of 2 of a longer body of text on \
                       somebody else's behalf."
        .into();
    mine.outcome = Some("x".into());
    assert!(!is_helper(&mine));
    assert!(matches!(
        decide(&mine, NEW, false, |_| true, RestoreMode::Resume),
        Decision::Deliver(_)
    ));
}

/// A child that failed and then answered again reads as its last word, at every door. The row
/// keeps the failure in `outcome`, but the later turn is what the parent is handed as news - so a
/// reader that reported the old failure would contradict the notice it just received.
#[test]
fn a_failed_child_that_answered_again_reads_as_that_answer() {
    let mut again = run("failed");
    again.outcome = Some("it blew up on the second read".into());
    again.later_answer = Some("the answer it gave after".into());
    match recorded_status(&again, NEW) {
        TaskStatus::Done { answer, .. } => {
            assert!(answer.contains("the answer it gave after"), "{answer}");
        }
        other => panic!("a child that spoke again read as {other:?}"),
    }
    assert_eq!(
        decide(&again, NEW, false, |_| true, RestoreMode::Resume),
        Decision::Deliver(TaskStatus::Done {
            answer: answer_said(&again),
            child: None,
        }),
        "the delivery after a restart says what `task_result` says"
    );
    // A failure with no later turn is still a failure.
    let mut once = run("failed");
    once.outcome = Some("it blew up on the second read".into());
    assert_eq!(
        recorded_status(&once, NEW),
        TaskStatus::Failed {
            why: "it blew up on the second read".into()
        }
    );
}

/// Derived names are unique among a parent's children; carried names are kept - the
/// twelve readers that all derived `you-are-reading-part`.
#[test]
fn derived_names_are_made_unique_and_carried_ones_kept() {
    let reader = |n: u32| {
        let mut r = run("done");
        r.child_id = format!("s-1-sub-{n}");
        r.prompt = format!(
            "You are reading part {n} of 3 of a longer body of text on somebody else's \
                 behalf."
        );
        r.spec_json = r#"{"role":"researcher","placement":"host"}"#.into();
        r
    };
    let mut named = run("done");
    named.spec_json = RecordedSpec::of(&TaskSpec {
        slug: Some("you-are-reading-part-2".into()),
        ..TaskSpec::default()
    })
    .to_json();
    let runs = vec![reader(1), named, reader(2), reader(3)];
    assert_eq!(
        recorded_slugs(&runs),
        vec![
            "you-are-reading-part",
            "you-are-reading-part-2",
            "you-are-reading-part-3",
            "you-are-reading-part-4",
        ]
    );
}

/// One state for one child, whichever door asks: `task_result` listed
/// `slt-current-number` as `unknown`, refused it as *"no subagent called … was started here"*
/// and `task_resume` refused it as *"finished its task"*. The record answers both now.
#[test]
fn a_recorded_child_reads_the_same_to_task_result_and_task_resume() {
    let mut done = run("done");
    done.outcome = Some("Committed as `dce7cca`.".into());
    assert_eq!(
        recorded_status(&done, NEW),
        TaskStatus::Done {
            answer: "Committed as `dce7cca`.".into(),
            child: None
        }
    );
    assert!(
        resumable(&done, NEW, false, |_| true)
            .unwrap_err()
            .contains("read its answer with `task_result`")
    );
    // Not running anywhere: the record says why, and both doors name `task_resume`.
    for state in ["running", "paused"] {
        let r = run(state);
        match recorded_status(&r, NEW) {
            TaskStatus::Failed { why } => {
                assert!(why.contains("task_resume"), "{state}: {why}");
                assert!(why.contains("/wt/agent-fix"), "{state}: {why}");
            }
            other => panic!("{state}: {other:?}"),
        }
        assert!(resumable(&r, NEW, false, |_| true).is_ok(), "{state}");
    }
    match recorded_status(&run("killed"), NEW) {
        TaskStatus::Failed { why } => assert!(why.contains("stopped by a person"), "{why}"),
        other => panic!("{other:?}"),
    }
}

/// A child that answered again is read as its later turn, because after a restart the row is
/// all `task_result` has: the answer it hands over is the last word, it says which turn that came
/// from, and the first answer stays in the row for whoever reads `outcome`.
#[test]
fn a_settled_child_with_a_later_answer_reads_as_that_later_turn() {
    let mut again = run("done");
    again.outcome = Some("the first answer".into());
    again.later_answer = Some("the second answer".into());
    match recorded_status(&again, NEW) {
        TaskStatus::Done { answer, .. } => {
            assert!(answer.contains("the second answer"), "{answer}");
            assert!(!answer.contains("the first answer"), "{answer}");
            assert!(
                answer.contains("a later turn of this child"),
                "a parent has to be able to tell this from the answer the child gave first: \
                     {answer}"
            );
        }
        other => panic!("a settled child with a later answer read as {other:?}"),
    }
    // A restart's delivery at open says the same thing, or the notice would carry the first
    // answer while `task_result` hands over the second.
    match decide(&again, NEW, false, |_| true, RestoreMode::Resume) {
        Decision::Deliver(TaskStatus::Done { answer, .. }) => {
            assert!(answer.contains("the second answer"), "{answer}");
        }
        other => panic!("a settled, undelivered child is {other:?}"),
    }
    // And a child that answered once is unchanged: no sentence, no later answer.
    let mut once = run("done");
    once.outcome = Some("the first answer".into());
    assert_eq!(
        recorded_status(&once, NEW),
        TaskStatus::Done {
            answer: "the first answer".into(),
            child: None,
        }
    );
}

#[test]
fn a_killed_child_is_never_resumed() {
    assert_eq!(
        decide(&run("killed"), NEW, false, |_| true, RestoreMode::Resume),
        Decision::Leave
    );
    assert_eq!(
        decide(&run("abandoned"), NEW, false, |_| true, RestoreMode::Resume),
        Decision::Leave
    );
}

#[test]
fn a_child_that_keeps_crashing_the_daemon_is_given_up_on() {
    let mut r = run("running");
    r.resumes = RESUME_LIMIT - 1;
    assert!(matches!(
        decide(&r, NEW, false, |_| true, RestoreMode::Resume),
        Decision::Resume { .. }
    ));
    r.resumes = RESUME_LIMIT;
    match decide(&r, NEW, false, |_| true, RestoreMode::Resume) {
        Decision::Abandon { why } => assert!(why.contains("3 times in a row"), "{why}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_vm_child_is_not_resumed_on_a_fresh_copy() {
    let mut r = run("running");
    r.spec_json = RecordedSpec::of(&TaskSpec {
        placement: Placement::Firecode,
        ..TaskSpec::default()
    })
    .to_json();
    assert!(matches!(
        decide(&r, NEW, false, |_| true, RestoreMode::Resume),
        Decision::Abandon { .. }
    ));
}

/// A VM child that worked in its worktree MOUNTED comes back; one that worked on a copy
/// does not - a Mac VM child given a worktree works in it directly (`firecode::mounts`), so
/// its edits survive the restart. Through both doors.
#[test]
fn a_vm_child_in_its_mounted_worktree_is_resumed_and_one_on_a_copy_is_not() {
    let base = std::env::temp_dir().join(format!("letibot-vm-resume-{}", std::process::id()));
    let wt = base.join("agent-fix");
    std::fs::create_dir_all(&wt).unwrap();
    std::fs::write(
        wt.join(".git"),
        format!("gitdir: {}/repo/.git/worktrees/agent-fix\n", base.display()),
    )
    .unwrap();
    let record = |vm_source: Option<PathBuf>| {
        let mut r = run("running");
        r.spec_json = RecordedSpec::of(&TaskSpec {
            role: "coder".into(),
            placement: Placement::Firecode,
            worktree: Some(WorktreePlacement {
                path: wt.display().to_string(),
                branch: "agent/fix".into(),
                base_sha: "abc".into(),
                main_tree: false,
            }),
            vm_source,
            ..TaskSpec::default()
        })
        .to_json();
        r
    };
    let mounted = record(None);
    let resumed = matches!(
        decide(&mounted, NEW, false, |_| true, RestoreMode::Resume),
        Decision::Resume { .. }
    );
    assert_eq!(resumed, cfg!(target_os = "macos"), "the mounted worktree");
    assert_eq!(
        resumable(&mounted, NEW, false, |_| true).is_ok(),
        cfg!(target_os = "macos"),
        "task_resume on the mounted worktree"
    );
    let copied = record(Some(base.join("repo")));
    match decide(&copied, NEW, false, |_| true, RestoreMode::Resume) {
        Decision::Abandon { why } => assert!(why.contains("on a copy"), "{why}"),
        other => panic!("a caller-named tree is a copy: {other:?}"),
    }
    let why = resumable(&copied, NEW, false, |_| true).unwrap_err();
    assert!(why.contains("on a copy"), "{why}");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn the_spec_round_trips_through_its_record() {
    let spec = TaskSpec {
        role: "researcher".into(),
        downgrade: Downgrade::parse("read-only").unwrap(),
        placement: Placement::Host,
        model: Some("zai/glm".into()),
        worktree: None,
        vm_source: Some("/src".into()),
        vm_verify: None,
        slug: Some("survey-the-store".into()),
        helper: true,
        skills: vec!["release".into()],
        perimeter: Some("reviewer".into()),
        widen: vec![
            letibot_tools::exec::perimeter::WidenItem::ReadOnly("/h/.local/state".into()),
            letibot_tools::exec::perimeter::WidenItem::Secret(
                letibot_tools::exec::perimeter::Secret::Github,
            ),
        ],
        widen_because: Some("it removes placements".into()),
    };
    let back = RecordedSpec::from_json(&RecordedSpec::of(&spec).to_json())
        .unwrap()
        .spec()
        .unwrap();
    assert_eq!(back, spec);
}

/// A recorded child is named by its record, and an old record still gets a name - the
/// slug the record carries, else the branch its worktree was cut on, else its prompt's.
#[test]
fn a_recorded_child_is_named_by_its_record_or_as_its_spawn_would_have_been() {
    // A record from before names: the worktree branch `agent/fix` names it.
    assert_eq!(recorded_slug(&run("running")), "fix");
    // A record that carries its name: that name, whatever the branch says.
    let mut named = run("running");
    named.spec_json = RecordedSpec::of(&TaskSpec {
        slug: Some("release-notes".into()),
        ..TaskSpec::default()
    })
    .to_json();
    assert_eq!(recorded_slug(&named), "release-notes");
    // A plain `task` child from before names, with no worktree: its prompt's derivation.
    let mut plain = run("done");
    plain.spec_json = RecordedSpec::of(&TaskSpec::default()).to_json();
    assert_eq!(recorded_slug(&plain), "fix-the-thing");
    // And a record written before the field existed decodes with no name rather than failing.
    let old = r#"{"role":"coder","placement":"host"}"#;
    assert_eq!(RecordedSpec::from_json(old).unwrap().slug, None);
}

/// A child with no record reads two ways, and the difference is its last row - an answer is
/// returned as an answer, and anything else is said to be gone rather than dressed up as
/// one. The state is not hypothetical
#[test]
fn a_child_with_no_record_reads_its_own_last_row() {
    let child = |why, last: Option<TranscriptItem>| letibot_tokencore::store::UnrecordedChild {
        id: "s-1-sub-lost".into(),
        title: Some("the-notices-stay-collapsed".into()),
        role: Some("coder".into()),
        workspace_root: "/wt/agent-lost".into(),
        created_ms: 1,
        why,
        last_item: last,
    };
    // An answer with no call after it is a finished turn: its text IS the answer.
    let answered = a_child_with_no_record(&child(
        NoRecord::Due,
        Some(TranscriptItem::Assistant {
            text: "the sweep is in".into(),
            tool_calls: vec![],
            truncated: false,
        }),
    ));
    assert_eq!(
        answered,
        TaskStatus::Done {
            answer: "the sweep is in".into(),
            child: None,
        }
    );
    // A turn that was still going when the rows stopped has NO answer, and the sentence says
    // where the child was and where its work still is.
    let gone = a_child_with_no_record(&child(
        NoRecord::Due,
        Some(TranscriptItem::ToolResult {
            call_id: "c1".into(),
            name: "bash".into(),
            outcome: letibot_transcript::ToolOutcome::Ok,
            payload: "ok".into(),
            edits: Vec::new(),
            origin: None,
            media: None,
        }),
    ));
    let TaskStatus::Failed { why } = gone else {
        panic!("a child mid-turn has no answer: {gone:?}");
    };
    assert!(why.contains("no record of it was ever written"), "{why}");
    assert!(why.contains("the result of `bash`"), "{why}");
    assert!(
        why.contains("`transcript` with session=\"s-1-sub-lost\""),
        "{why}"
    );
    assert!(why.contains("/wt/agent-lost"), "{why}");
    assert!(
        !why.contains("task_resume` with task="),
        "nothing can resume it: {why}"
    );
    // And a child that wrote no row of its own at all.
    let TaskStatus::Failed { why } = a_child_with_no_record(&child(NoRecord::Due, None)) else {
        panic!("a child that wrote nothing has no answer");
    };
    assert!(why.contains("it wrote no row of its own at all"), "{why}");
}

/// How far a child with no record got, in the two sizes the two doors need - both from ONE
/// judgement, because a child whose text [`a_child_with_no_record`] returns as an ANSWER
/// must be the child [`how_far`] calls `Answered`. The defect this closes
#[test]
fn how_far_says_where_a_child_with_no_record_stopped() {
    use letibot_tools::builtins::task::{LostChild, TaskStatus};

    let child = |last: Option<TranscriptItem>| letibot_tokencore::store::UnrecordedChild {
        id: "s-1-sub-lost".into(),
        title: Some("the sweep".into()),
        role: Some("coder".into()),
        workspace_root: "/wt/agent-lost".into(),
        created_ms: 1,
        why: NoRecord::Due,
        last_item: last,
    };
    let answer = TranscriptItem::Assistant {
        text: "the sweep is in".into(),
        tool_calls: vec![],
        truncated: false,
    };
    let asked = TranscriptItem::Assistant {
        text: String::new(),
        tool_calls: vec![letibot_transcript::ToolCall {
            id: "c1".into(),
            name: "bash".into(),
            arguments: "{}".into(),
        }],
        truncated: false,
    };
    let result = TranscriptItem::ToolResult {
        call_id: "c1".into(),
        name: "bash".into(),
        outcome: letibot_transcript::ToolOutcome::Ok,
        payload: "ok".into(),
        edits: Vec::new(),
        origin: None,
        media: None,
    };

    // Two children that stopped somewhere different read differently - this is the whole
    // point, and the sentence the list printed for both was one.
    let mid_call = how_far(&child(Some(asked)));
    let mid_turn = how_far(&child(Some(result.clone())));
    let never = how_far(&child(None));
    assert_eq!(
        mid_call,
        LostChild::Stopped("mid-`bash`, on a call it never got an answer to".into())
    );
    assert_eq!(mid_turn, LostChild::Stopped("after `bash`'s result".into()));
    assert_eq!(
        never,
        LostChild::Stopped("before it wrote a single row".into())
    );
    assert_ne!(mid_call, mid_turn, "two classes, one clause");

    // A last row that IS an answer is the other class, and the two doors agree about it.
    assert_eq!(how_far(&child(Some(answer.clone()))), LostChild::Answered);
    assert!(
        matches!(
            a_child_with_no_record(&child(Some(answer))),
            TaskStatus::Done { .. }
        ),
        "the class the list calls `Answered` is the one the answer door returns as an answer"
    );

    // Every shape of last row, in both sizes - the clause reads on from *"it stopped "*
    // and the paragraph never loses the long form a model has been reading.
    for (item, said, brief) in [
        (
            None,
            "nothing: it wrote no row of its own at all",
            "before it wrote a single row",
        ),
        (
            Some(result.clone()),
            "the result of `bash`, so the turn went on from there",
            "after `bash`'s result",
        ),
        (
            Some(TranscriptItem::User {
                parts: vec![],
                speaker: Default::default(),
            }),
            "the prompt it was given",
            "on the prompt it was given, before it answered",
        ),
        (
            Some(TranscriptItem::Reasoning {
                text: "hm".into(),
                field: letibot_transcript::ReasoningField::Inline,
                truncated: false,
            }),
            "a thought it never finished",
            "mid-thought",
        ),
    ] {
        let ended = EndedIn::of(item.as_ref());
        assert_eq!(ended.said(), said);
        assert_eq!(ended.brief(), brief);
        assert_eq!(
            how_far(&child(item)),
            LostChild::Stopped(brief.into()),
            "{brief}"
        );
    }
    // And nothing in either rendering guesses WHY the row is missing - that is
    // `no_record_cause`'s, from the store's own class, and the list is not told it at all.
    for why in [NoRecord::Due, NoRecord::BeforeTheStore] {
        let c = letibot_tokencore::store::UnrecordedChild { why, ..child(None) };
        let said = match how_far(&c) {
            LostChild::Stopped(words) => words,
            other => panic!("{other:?}"),
        };
        assert!(
            !said.contains("refused") && !said.contains("store"),
            "{said}"
        );
    }
}

/// A child older than the store's first record is not told a write was refused - the class
/// the window exists for, and the sentence a model reads about it.
#[test]
fn a_child_from_before_the_record_is_told_that_and_not_a_refused_write() {
    let old = letibot_tokencore::store::UnrecordedChild {
        id: "s-1-sub-old".into(),
        title: Some("the sweep".into()),
        role: Some("coder".into()),
        workspace_root: "/wt/agent-old".into(),
        created_ms: 1,
        why: NoRecord::BeforeTheStore,
        last_item: None,
    };
    let TaskStatus::Failed { why } = a_child_with_no_record(&old) else {
        panic!("a child with no answer is not an answer");
    };
    assert!(
        why.contains("this child is from before this store recorded any child"),
        "{why}"
    );
    assert!(
        !why.contains("refused the row"),
        "no write of it was refused, so the sentence may not say one was: {why}"
    );
    // The rest of what a parent needs is still there: where it worked, how to read it, and
    // what it was asked.
    assert!(why.contains("/wt/agent-old"), "{why}");
    assert!(
        why.contains("`transcript` with session=\"s-1-sub-old\""),
        "{why}"
    );
    assert!(why.contains("it was asked: the sweep"), "{why}");
    // And the class that IS a lost write still says so.
    assert_eq!(
        no_record_cause(NoRecord::Due),
        "the store refused the row its spawn writes"
    );
}

#[test]
fn the_notices_say_what_happened() {
    assert_eq!(utc(0), "1970-01-01 00:00:00 UTC");
    assert_eq!(utc(1_791_806_400_000), "2026-10-12 12:00:00 UTC");
    // what happened and what survived, with no clock and no build sha.
    let n = child_notice(1_791_806_400_000);
    assert_eq!(
        n,
        "[restart] You were interrupted and resumed; your conversation, worktree and plan \
             are as you left them, and background jobs you started are gone. Carry on."
    );
    assert!(!n.contains("2026-10-12") && !n.contains("build "), "{n}");
    // a bullet in the parent's notice, and no second `[restart]` tag inside it.
    assert_eq!(
        parent_resumed_line("s-1-sub-1"),
        "- subagent s-1-sub-1: resumed."
    );
}
