//! Needs: edges between rows, children and jobs waited on, and what is ready.

use super::*;

/// A plan's field is only a graph if the tool can write an edge, and an edge the model cannot SEE
/// is one it cannot maintain: the next `todo_write` that re-words a row breaks every edge into it.
#[test]
fn a_plan_writes_its_edges_and_the_reply_shows_them() {
    let (rt, board) = runtime();
    let mut sink = RecordingToolSink::new();
    let r = rt.invoke(
        "t1",
        &call(
            r#"{"todos": [
                    {"content": "run the tests", "status": "pending"},
                    {"content": "deploy", "status": "pending", "needs": ["run the tests"]}
                ]}"#,
        ),
        &mut sink,
    );
    assert_eq!(r.outcome, ToolOutcome::Ok, "{}", r.payload);
    let snap = board.snapshot();
    assert_eq!(
        snap[1].needs,
        vec![TodoNeed::Row {
            content: "run the tests".into()
        }],
        "the edge is on the board"
    );
    assert!(
        r.payload.contains("(waiting on `run the tests`)"),
        "and the reply shows it, so the model can maintain it: {}",
        r.payload
    );
    // And the check then asks about the row that can start - the whole point of the edge.
    let msg = nag_for(&snap).expect("the plan is unfinished");
    assert!(msg.contains("  - run the tests"), "{msg}");
    assert!(
        msg.contains("`deploy` waits on `run the tests` (still open)"),
        "{msg}"
    );
}

/// The tagged form is accepted as well as the plain name, so the shape the store and the
/// wire carry is one a model can write too - which is what keeps the tool's vocabulary and the
/// store's from being two spellings a reader has to know.
#[test]
fn a_need_may_be_written_in_the_tagged_form() {
    let (rt, board) = runtime();
    let mut sink = RecordingToolSink::new();
    let r = rt.invoke(
        "t1",
        &call(
            r#"{"todos": [{"content": "run the tests", "status": "completed"},
                    {"content": "deploy", "status": "pending",
                     "needs": [{"kind": "row", "content": "run the tests"}]}]}"#,
        ),
        &mut sink,
    );
    assert_eq!(r.outcome, ToolOutcome::Ok, "{}", r.payload);
    assert_eq!(
        board.snapshot()[1].needs,
        vec![TodoNeed::Row {
            content: "run the tests".into()
        }]
    );
    // And it is satisfied, so the row is READY and nothing is blocked.
    let msg = nag_for(&board.snapshot()).expect("open work");
    assert!(msg.contains("  - deploy"), "{msg}");
    assert!(!msg.contains("cannot start yet"), "{msg}");
}

/// A kind this build cannot evaluate is refused by name, never dropped: dropping it leaves a row
/// that looks like it waits for something and waits for nothing.
#[test]
fn a_need_of_a_kind_this_build_does_not_know_is_refused_by_name() {
    let (rt, board) = runtime();
    let mut sink = RecordingToolSink::new();
    let r = rt.invoke(
        "t1",
        &call(
            r#"{"todos": [{"content": "deploy", "status": "pending",
                     "needs": [{"kind": "time", "at": "midnight"}]}]}"#,
        ),
        &mut sink,
    );
    assert!(
        matches!(r.outcome, ToolOutcome::Failed { .. }),
        "a kind nobody can evaluate is not a write: {:?}",
        r.outcome
    );
    let said = format!("{} {:?}", r.payload, r.outcome);
    assert!(
        said.contains("kind `time`") && said.contains("cannot evaluate"),
        "the refusal names the kind it does not know: {said}"
    );
    assert!(
        said.contains("the kinds of need are `row`")
            && said.contains("`child`")
            && said.contains("`job`"),
        "and says what it does take: {said}"
    );
    assert!(
        board.snapshot().is_empty(),
        "and nothing was written: {:?}",
        board.snapshot()
    );
}

/// The write half of the child edge, without which the nag's silence is unreachable. The tagged
/// form is the only shape a child can take, because a plain string names a row.
#[test]
fn a_plan_can_wait_on_a_child_by_its_session_id() {
    let (rt, board) = runtime();
    let mut sink = RecordingToolSink::new();
    let r = rt.invoke(
        "t1",
        &call(
            r#"{"todos": [{"content": "collect the child's answer", "status": "pending",
                     "needs": [{"kind": "child", "id": "s-1-sub-2"}]}]}"#,
        ),
        &mut sink,
    );
    assert_eq!(r.outcome, ToolOutcome::Ok, "{}", r.payload);
    assert_eq!(
        board.snapshot()[0].needs,
        vec![TodoNeed::Child {
            id: "s-1-sub-2".into()
        }],
        "the edge is on the board"
    );
    assert!(
        r.payload.contains("(waiting on the child `s-1-sub-2`)"),
        "and the reply shows it, so the model can maintain it: {}",
        r.payload
    );
    // And with no daemon behind this board the child is not running, which is the honest
    // reading rather than a guess - `ChildSessions::none()` is what a caller with no children
    // hands in, and the row is then ordinary open work.
    assert!(
        nag_for(&board.snapshot()).is_some(),
        "a row whose child is not running is work, not a stuck plan"
    );

    // An empty id is refused by name, the same rule an empty row name keeps.
    let r = rt.invoke(
        "t2",
        &call(
            r#"{"todos": [{"content": "deploy", "status": "pending",
                     "needs": [{"kind": "child", "id": "  "}]}]}"#,
        ),
        &mut sink,
    );
    assert!(
        matches!(r.outcome, ToolOutcome::Failed { .. }),
        "{:?}",
        r.outcome
    );
    assert!(
        format!("{:?} {}", r.outcome, r.payload).contains("empty session id"),
        "the refusal says which kind of name was empty: {:?} {}",
        r.outcome,
        r.payload
    );
}

/// The write half of the job edge. The handle is quoted verbatim because it is the one `bash` with
/// `background: true` prints, and the one `job_output` and `job_kill` are given.
#[test]
fn a_plan_can_wait_on_a_job_by_its_handle() {
    let (rt, board) = runtime();
    let mut sink = RecordingToolSink::new();
    let r = rt.invoke(
        "t1",
        &call(
            r#"{"todos": [{"content": "push the branch", "status": "pending",
                     "needs": [{"kind": "job", "handle": "j2070"}]}]}"#,
        ),
        &mut sink,
    );
    assert_eq!(r.outcome, ToolOutcome::Ok, "{}", r.payload);
    assert_eq!(
        board.snapshot()[0].needs,
        vec![TodoNeed::Job {
            handle: "j2070".into()
        }],
        "the edge is on the board"
    );
    assert!(
        r.payload.contains("(waiting on the job `j2070`)"),
        "and the reply shows it, so the model can maintain it: {}",
        r.payload
    );
    // And with no daemon behind this board the job is not running, which is the honest
    // reading rather than a guess - `RunningJobs::none()` is what a caller with no process host
    // hands in, and the row is then ordinary open work.
    assert!(
        nag_for(&board.snapshot()).is_some(),
        "a row whose job is not running is work, not a stuck plan"
    );

    // An empty handle is refused by name, the same rule an empty row name and an empty
    // session id keep: a need that names nothing must not read as one that names something.
    let r = rt.invoke(
        "t2",
        &call(
            r#"{"todos": [{"content": "push the branch", "status": "pending",
                     "needs": [{"kind": "job", "handle": "  "}]}]}"#,
        ),
        &mut sink,
    );
    assert!(
        matches!(r.outcome, ToolOutcome::Failed { .. }),
        "{:?}",
        r.outcome
    );
    assert!(
        format!("{:?} {}", r.outcome, r.payload).contains("empty job handle"),
        "the refusal says which kind of name was empty: {:?} {}",
        r.outcome,
        r.payload
    );
    // And a `job` need with no `handle` at all is refused too, rather than read as an empty one.
    let r = rt.invoke(
        "t3",
        &call(
            r#"{"todos": [{"content": "push the branch", "status": "pending",
                     "needs": [{"kind": "job"}]}]}"#,
        ),
        &mut sink,
    );
    assert!(
        matches!(r.outcome, ToolOutcome::Failed { .. }),
        "{:?}",
        r.outcome
    );
    assert!(
        format!("{:?} {}", r.outcome, r.payload).contains("no `handle`"),
        "{:?} {}",
        r.outcome,
        r.payload
    );
}

/// An empty name and a `needs` that is not a list are refused by name too - the same rule,
/// one level down: an edge that names nothing is not an edge.
#[test]
fn an_empty_need_and_a_malformed_needs_are_refused_by_name() {
    let (rt, board) = runtime();
    let mut sink = RecordingToolSink::new();
    let r = rt.invoke(
        "t1",
        &call(r#"{"todos": [{"content": "deploy", "status": "pending", "needs": ["  "]}]}"#),
        &mut sink,
    );
    assert!(
        matches!(r.outcome, ToolOutcome::Failed { .. }),
        "{:?}",
        r.outcome
    );
    assert!(
        format!("{:?} {}", r.outcome, r.payload).contains("is empty"),
        "the empty name is named: {:?} {}",
        r.outcome,
        r.payload
    );

    let r = rt.invoke(
        "t2",
        &call(r#"{"todos": [{"content": "deploy", "status": "pending", "needs": "x"}]}"#),
        &mut sink,
    );
    assert!(
        matches!(r.outcome, ToolOutcome::Failed { .. }),
        "{:?}",
        r.outcome
    );
    assert!(
        format!("{:?} {}", r.outcome, r.payload).contains("not a list"),
        "a `needs` that is not a list is refused rather than ignored: {:?} {}",
        r.outcome,
        r.payload
    );
    assert!(
        board.snapshot().is_empty(),
        "nothing was written by either refusal"
    );
}

/// A board with no edges reads as it did before edges existed. The text is pinned byte for byte,
/// and `as_a_graph` is the identity on `open_priority` with no needs, which is why it cannot move.
#[test]
fn a_board_with_no_dependencies_is_read_exactly_as_before() {
    let plan = vec![
        item("write it up", TodoStatus::Completed),
        item("wire the check", TodoStatus::InProgress),
        item("test it", TodoStatus::Pending),
    ];
    // The sentence changed deliberately - no count, no restating suffix, one line of
    // advice - and it is pinned byte for byte here as the old one was.
    assert_eq!(
        nag_for(&plan).expect("open work"),
        "[todo check] Not done:\n  - wire the check — yours (in progress)\nDo it, or mark it done \
         with `todo_write`'s `update` field (quote it exactly).",
    );

    // The other arms, one at a time, because the arm is what the field could have moved.
    assert_eq!(
        nag_for(&[item("first", TodoStatus::Pending)]).expect("open work"),
        "[todo check] Not done:\n  - first — yours\nDo it, or mark it done with `todo_write`'s \
         `update` field (quote it exactly)."
    );
    assert_eq!(
        nag_for(&[
            item("first", TodoStatus::Pending),
            item("second", TodoStatus::Pending)
        ])
        .expect("open work")
        .lines()
        .next()
        .expect("a first line"),
        "[todo check] Not done:"
    );
    assert!(
        nag_for(&[
            item("first", TodoStatus::Pending),
            item("second", TodoStatus::Pending)
        ])
        .expect("open work")
        .contains("  - first — yours\n"),
        "the first open row is named"
    );
    assert_eq!(
        nag_for(&[item("first", TodoStatus::InProgress)])
            .expect("open work")
            .lines()
            .nth(1),
        Some("  - first — yours (in progress)")
    );
    let sticky = unfinished_plan_for(
        &[item("first", TodoStatus::Pending)],
        &[item("first", TodoStatus::Pending)],
        Some("first"),
        &ChildSessions::none(),
        &no_jobs(),
    )
    .expect("open work");
    assert_eq!(sticky.0.lines().nth(1), Some("  - first — yours"));

    // AND THE STRUCTURE: the graph split of a board with no edges is the identity.
    let graph = as_a_graph(&plan, &plan, &ChildSessions::none(), &no_jobs());
    assert!(
        graph.blocked.is_empty(),
        "nothing can be blocked with no edges: {:?}",
        graph.blocked
    );
    assert_eq!(
        graph
            .ready
            .iter()
            .map(|t| t.content.as_str())
            .collect::<Vec<_>>(),
        open_priority(&plan)
            .iter()
            .map(|t| t.content.as_str())
            .collect::<Vec<_>>(),
        "the ready set IS the queue, in the queue's order"
    );
}

/// An edge that is MET is inert. The same board with a satisfied need is read exactly as
/// the same board with no need at all - which is the general form of the compatibility claim,
/// and the reason a plan that adopts the field gradually cannot change under itself.
#[test]
fn a_satisfied_need_is_inert() {
    let flat = vec![
        item("first", TodoStatus::Completed),
        item("second", TodoStatus::Pending),
    ];
    let edged = vec![
        item("first", TodoStatus::Completed),
        needs("second", TodoStatus::Pending, &["first"]),
    ];
    assert_eq!(nag_for(&flat), nag_for(&edged));
}

/// A blocked row is not the ask, the row behind it is: naming the first open row whatever it waits
/// for asks about a row that cannot start and never reaches the one that can.
#[test]
fn a_row_whose_need_is_open_is_not_asked_about() {
    let plan = vec![
        needs("deploy", TodoStatus::Pending, &["run the tests"]),
        item("run the tests", TodoStatus::Pending),
    ];
    let msg = nag_for(&plan).expect("one row can start");
    assert!(
        msg.contains("  - run the tests"),
        "the ready row is the one named: {msg}"
    );
    assert!(
        !msg.contains("  - deploy"),
        "and the blocked row is not named as the thing to do: {msg}"
    );
    // And it is SAID rather than skipped, because a blocked row is invisible to the ask and
    // a plan whose edges went stale would otherwise look like a shorter plan.
    assert!(
        msg.contains("1 row cannot start yet: `deploy` waits on `run the tests` (still open)."),
        "what cannot start is said, with what it waits for: {msg}"
    );
}

/// An edge into a row the operator struck off can never be met, so it must not say "still open":
/// that sends the model to finish work they decided against. The remedy is its own.
#[test]
fn a_need_on_a_row_the_operator_struck_off_says_the_edge_can_never_be_met() {
    let struck_off = TodoItem {
        content: "run the tests".into(),
        status: TodoStatus::Cancelled,
        by: TodoBy::Operator,
        when: None,
        needs: Vec::new(),
    };
    // The cancelled row is not one the check may ask about, so the plan it is handed is the
    // narrowed one - exactly what `harnessd`'s `the_plan_as_checked` builds.
    let rows = vec![needs("deploy", TodoStatus::Pending, &["run the tests"])];
    let board = vec![
        struck_off,
        needs("deploy", TodoStatus::Pending, &["run the tests"]),
    ];
    let (msg, _chosen) =
        unfinished_plan_for(&rows, &board, None, &ChildSessions::none(), &no_jobs())
            .expect("a row that cannot start is said, not skipped");
    assert!(
        msg.contains("struck off by the operator"),
        "the sentence names the operator's act: {msg}"
    );
    assert!(
        msg.contains("re-send this row in `todos` with that need removed"),
        "and the remedy, which is the model's own: {msg}"
    );
    assert!(
        !msg.contains("re-word it"),
        "**not re-wording, which adds a row and leaves this one blocked**: {msg}"
    );
    assert!(
        !msg.contains("(still open)"),
        "**and never says *still open* about a row nobody is going to finish**: {msg}"
    );
}

/// An unfinished plan with an empty ready set: going quiet would read as "nothing to do", the one
/// wrong reading, and asking about the head is the head-of-line blocking this replaced. A cycle
/// lands here, which is why it is the example.
#[test]
fn a_plan_where_nothing_can_start_says_so() {
    let plan = vec![
        needs("deploy", TodoStatus::Pending, &["run the tests"]),
        needs("run the tests", TodoStatus::Pending, &["deploy"]),
    ];
    let (msg, named) = unfinished_plan_for(&plan, &plan, None, &ChildSessions::none(), &no_jobs())
        .expect("a stuck plan is a finding");
    assert!(
        msg.starts_with("[todo check] this turn is finished and NOTHING can start"),
        "the state is said in the house prefix's own voice: {msg}"
    );
    assert!(
        msg.contains("  - `deploy` waits on `run the tests` (still open)\n")
            && msg.contains("  - `run the tests` waits on `deploy` (still open)\n"),
        "every row that is waiting, and on what: {msg}"
    );
    assert!(
        msg.contains("unblock one of these rather than starting something new"),
        "and what to do about it — unblock, rather than start: {msg}"
    );
    assert_eq!(
        named, "",
        "nothing is being held to, so the next check draws its choice afresh"
    );
}

/// A need nobody can resolve is never met: a condition that cannot be evaluated must not read as
/// satisfied, and a name that matches no row is the common case the message has to repair.
#[test]
fn a_need_that_names_nothing_blocks_the_row_and_says_so() {
    let plan = vec![
        needs("deploy", TodoStatus::Pending, &["publish"]),
        item("run the tests", TodoStatus::Pending),
    ];
    let msg = nag_for(&plan).expect("one row can start");
    assert!(
        msg.contains("  - run the tests"),
        "the resolvable row is the one named: {msg}"
    );
    assert!(
        msg.contains("`deploy` waits on `publish` (no such row on this board)"),
        "and the unresolvable need is said as unresolvable, never quietly satisfied: {msg}"
    );

    // Alone, it is the degenerate case: the ONLY row cannot start.
    let alone = vec![needs("deploy", TodoStatus::Pending, &["publish"])];
    let (msg, named) =
        unfinished_plan_for(&alone, &alone, None, &ChildSessions::none(), &no_jobs())
            .expect("a stuck plan speaks");
    assert!(msg.contains("NOTHING can start"), "{msg}");
    assert!(
        msg.contains("`deploy` waits on `publish` (no such row on this board)"),
        "{msg}"
    );
    assert_eq!(named, "");
}

/// A name that matches two rows is unresolvable - the house rule for an ambiguous name,
/// read one step earlier: the guess this refuses to make is a row silently starting on the
/// strength of a name that meant something else.
#[test]
fn a_need_that_matches_two_rows_is_unresolvable() {
    let plan = vec![
        item("same words", TodoStatus::Pending),
        item("same words", TodoStatus::Pending),
        needs("deploy", TodoStatus::Pending, &["same words"]),
    ];
    let msg = nag_for(&plan).expect("two rows can start");
    assert!(
        msg.contains("`deploy` waits on `same words` (two rows on this board say that)"),
        "{msg}"
    );
}

/// A kind this build cannot evaluate blocks the row and says so rather than failing the plan: this
/// is the value a newer build's store leaves behind.
#[test]
fn a_need_of_a_kind_this_build_cannot_evaluate_blocks_and_says_so() {
    let plan = vec![TodoItem {
        needs: vec![TodoNeed::Unknown],
        ..item("deploy", TodoStatus::Pending)
    }];
    let msg = nag_for(&plan).expect("a stuck plan speaks");
    assert!(msg.contains("NOTHING can start"), "{msg}");
    assert!(
        msg.contains("`deploy` waits on a dependency of a kind this build cannot evaluate"),
        "an edge nobody can answer must never read as met: {msg}"
    );
}

/// A row waiting on a young child is silent, and spoken about once the child is old enough to be
/// worth a look. The narrowing is the caller's, so the rule is asserted where it lives.
#[test]
fn a_child_that_is_still_young_keeps_the_row_silent_and_an_old_one_is_spoken_about() {
    let plan = vec![waiting_on_a_child(
        "collect the child's answer",
        "s-1-sub-2",
    )];

    // Five minutes old: the row is untalkable. The predicate the caller narrows with says
    // so - and the child is at its most alive, generating four seconds ago.
    let young = ChildSessions::none().running("s-1-sub-2", child(5 * 60, Some(4), true));
    assert!(
        waits_on_a_child_with_room_to_breathe(&plan[0], &young),
        "a row waiting on a five-minute-old child is one the check may not speak about"
    );

    // Old enough to be talkable, and quiet with a turn in flight, which is the stuck case. The
    // same silence with no turn in flight is a child between turns, and has its own sentence.
    let old = ChildSessions::none().running("s-1-sub-2", child(45 * 60, Some(12 * 60), true));
    assert!(
        !waits_on_a_child_with_room_to_breathe(&plan[0], &old),
        "past the room to breathe, the check may speak"
    );
    let msg = nag_with(&plan, &old, &no_jobs()).expect("a stuck plan speaks");
    assert!(
        msg.contains("the child `s-1-sub-2`, running 45 minutes"),
        "the sentence names the child and its age: {msg}"
    );
    assert!(
        msg.contains("check whether it is stuck"),
        "and steers to the CHILD rather than to the plan: {msg}"
    );
    assert!(
        !msg.contains("cannot start yet"),
        "the one sentence it must not say is the one that tells the model nothing: {msg}"
    );
    assert!(
        msg.contains("a turn is in flight") && msg.contains("12 minutes"),
        "and it carries the evidence the verdict was made of: {msg}"
    );
    assert!(
        msg.contains("`task_result`") && msg.contains("`job_kill`"),
        "with the mechanisms named, because a model that guesses one guesses wrong: {msg}"
    );
}

/// Both children are the same age, so only what the child last showed differs: a notice that can
/// report liveness alone cannot steer anybody, so the two states must read differently.
#[test]
fn a_child_that_is_moving_is_not_sent_to_be_checked_on() {
    let plan = vec![waiting_on_a_child(
        "collect the child's answer",
        "s-1-sub-2",
    )];
    let moving = ChildSessions::none().running("s-1-sub-2", child(45 * 60, Some(8), true));
    let msg = nag_with(&plan, &moving, &no_jobs()).expect("a stuck plan speaks");
    assert!(
        msg.contains("it is working (a turn is in flight, its last event was 8 seconds ago)"),
        "a child that moved eight seconds ago is called working: {msg}"
    );
    assert!(
        !msg.contains("check whether it is stuck"),
        "and the check is not asked for about it: {msg}"
    );

    // The same age, nothing moving. The two notices differ, and that difference is the
    // whole of what the operator asked for.
    let quiet = ChildSessions::none().running("s-1-sub-2", child(45 * 60, Some(12 * 60), true));
    let quiet_msg = nag_with(&plan, &quiet, &no_jobs()).expect("a stuck plan speaks");
    assert_ne!(msg, quiet_msg, "working and quiet must not read the same");
    assert!(
        quiet_msg.contains("check whether it is stuck"),
        "{quiet_msg}"
    );
}

/// No turn in flight and the child still there means it is parked, and the move is `task_message`,
/// the only door that reaches it. Saying `job_kill` would send the reader to stop a child waiting
/// to be asked.
#[test]
fn a_child_between_turns_is_told_to_be_asked_and_not_killed() {
    let plan = vec![waiting_on_a_child(
        "collect the child's answer",
        "s-1-sub-2",
    )];
    // Forty-five minutes old, nothing for twelve minutes, and no turn in flight: the child
    // finished the turn it was running and is parked, waiting for a word.
    let idle = ChildSessions::none().running("s-1-sub-2", child(45 * 60, Some(12 * 60), false));
    let msg = nag_with(&plan, &idle, &no_jobs()).expect("an old child is spoken about");
    assert!(
        msg.contains("no turn is in flight") && msg.contains("12 minutes"),
        "the evidence is printed, because a reader judges by it: {msg}"
    );
    assert!(
        msg.contains("between turns") && msg.contains("`task_message`"),
        "and the move for a parked child is the one door that reaches it: {msg}"
    );
    assert!(
        !msg.contains("`job_kill`") && !msg.contains("check whether it is stuck"),
        "a child waiting to be asked is not stuck, and stopping it is not the move: {msg}"
    );

    // And the state is the reading's own, so the same child with a turn in flight keeps the
    // check sentence - the two states cannot be told apart by anything but the fact.
    let working = ChildSessions::none().running("s-1-sub-2", child(45 * 60, Some(12 * 60), true));
    let other = nag_with(&plan, &working, &no_jobs()).expect("a stuck plan speaks");
    assert_ne!(msg, other, "idle and wedged must not read the same");
}

/// A finished child is not in the snapshot, so the need is met and the row is ordinary work. This
/// is what makes the need a dependency rather than a note about one.
#[test]
fn a_child_that_has_finished_resolves_the_need() {
    let plan = vec![waiting_on_a_child(
        "collect the child's answer",
        "s-1-sub-2",
    )];
    let finished = ChildSessions::none();
    assert!(
        !waits_on_a_child_with_room_to_breathe(&plan[0], &finished),
        "a child that is not running is not a reason to stay quiet"
    );
    let msg = nag_with(&plan, &finished, &no_jobs()).expect("the row is open work now");
    assert!(
        msg.contains("  - collect the child's answer — yours"),
        "the row itself is what the check asks about: {msg}"
    );
    assert!(
        !msg.contains("s-1-sub-2") && !msg.contains("check whether it is stuck"),
        "and it is not about the child any more — the row's own words say `child`, so what \
             this asserts is that the ID is gone and nothing asks to check on it: {msg}"
    );
}

/// The same plan and the same handle throughout, and only whether the job runs changes: while it
/// does the caller narrows the row away, and when it ends the edge is met and the row is the work.
#[test]
fn a_running_job_keeps_the_row_silent_and_the_end_of_it_asks_again() {
    let plan = vec![waiting_on_a_job("push the branch", "j2070")];

    // Running: the row is one the check may not speak about at all.
    let running = RunningJobs::none().running("j2070");
    assert!(
        waits_on_a_running_job(&plan[0], &running),
        "a row waiting on a running job is one the check may not speak about"
    );
    // And a caller that did NOT narrow still gets the truth rather than a silence: the edge
    // is unmet, and the handle is what it is named by - it is what `job_output` is given.
    assert_eq!(
        unmet_needs(&plan, &plan[0], &ChildSessions::none(), &running),
        vec![("j2070".to_string(), NeedFate::JobRunning)],
        "the edge is unmet while the job runs"
    );
    // And a plan blocked ONLY on a running job is silent even unnarrowed - the
    // *"NOTHING can start … a plan to fix"* notice fired on exactly this, which `todo_write`
    // promises is silent.
    assert_eq!(
        nag_with(&plan, &ChildSessions::none(), &running),
        None,
        "a plan waiting only on a running job is not a plan to fix"
    );

    // And the lead a reader gets when there IS other work to do. A blocked row is SAID
    // under a lead of its own rather than the child's: *check whether it is stuck* about a job
    // would send the reader to look at a process that is doing exactly what it was started for.
    let with_work = vec![
        waiting_on_a_job("push the branch", "j2070"),
        item("write it up", TodoStatus::Pending),
    ];
    let msg = nag_with(&with_work, &ChildSessions::none(), &running).expect("one row can start");
    assert!(
        msg.contains("waiting on a job that is still running"),
        "under a lead of its own, not the child's: {msg}"
    );
    assert!(
        msg.contains("  - write it up"),
        "while the row that CAN start is the one named: {msg}"
    );

    // Ended: the edge is met, and the row is the work the check asks about.
    let ended = RunningJobs::none();
    assert!(
        !waits_on_a_running_job(&plan[0], &ended),
        "a job that is not running is not a reason to stay quiet"
    );
    let msg = nag_with(&plan, &ChildSessions::none(), &ended).expect("the row is open work");
    assert!(
        msg.contains("  - push the branch — yours"),
        "the row itself is what the check asks about: {msg}"
    );
    assert!(
        !msg.contains("j2070") && !msg.contains("waits on"),
        "and it is not about the job any more: {msg}"
    );
}

/// A handle nobody knows is a job that ended, which is what a resumed plan's handles look like
/// after a restart: without it the feature would invent a way to wait for ever.
#[test]
fn a_job_nobody_has_heard_of_counts_as_ended() {
    let plan = vec![waiting_on_a_job("push the branch", "j-before-the-restart")];
    // The snapshot a daemon that has just come back hands in: it holds nothing at all.
    let nothing = RunningJobs::none();
    assert!(
        !waits_on_a_running_job(&plan[0], &nothing),
        "an unknown handle is not a reason to stay quiet"
    );
    assert!(
        unmet_needs(&plan, &plan[0], &ChildSessions::none(), &nothing).is_empty(),
        "and the edge is MET rather than waiting for ever"
    );
    let msg = nag_with(&plan, &ChildSessions::none(), &nothing).expect("the row is open work");
    assert!(
        msg.contains("  - push the branch — yours"),
        "so the row is what the check asks about: {msg}"
    );
    assert!(
        !msg.contains("cannot start yet"),
        "and nothing is reported as blocked on it: {msg}"
    );
}

/// A job edge and a child edge are read by their own snapshot: a board that waits on rows reads
/// the same whichever is handed in, and one handle is one question.
#[test]
fn a_snapshot_of_jobs_changes_nothing_about_a_board_that_waits_on_rows() {
    let flat = vec![
        item("first", TodoStatus::Pending),
        item("second", TodoStatus::Pending),
    ];
    let edged = vec![
        item("run the tests", TodoStatus::Pending),
        needs("deploy", TodoStatus::Pending, &["run the tests"]),
    ];
    let busy = RunningJobs::none().running("j2070").running("j2081");
    // A running job adds only the offer of `wait_on`, appended to the advice and taken off before
    // comparing: the claim here is about the plan's reading, which the offer leaves alone.
    let plan_of = |msg: Option<String>| {
        msg.map(|m| {
            let start = [" It reads like", " If this row is only waiting"]
                .iter()
                .filter_map(|s| m.find(s))
                .min();
            match start {
                Some(at) => {
                    let end = m[at..].find('\n').map_or(m.len(), |n| at + n);
                    format!("{}{}", &m[..at], &m[end..])
                }
                None => m,
            }
        })
    };
    assert_eq!(
        nag_for(&flat),
        plan_of(nag_with(&flat, &ChildSessions::none(), &busy)),
        "no needs at all"
    );
    assert_eq!(
        nag_for(&edged),
        plan_of(nag_with(&edged, &ChildSessions::none(), &busy)),
        "row edges"
    );

    // A row waiting on a JOB is unmoved by a daemon full of CHILDREN, and the two snapshots are
    // read by their own kinds: `j2070` is not a session and `s-1-sub-2` is not a job.
    let job_row = vec![waiting_on_a_job("push the branch", "j2070")];
    let children = ChildSessions::none().running("j2070", child(5 * 60, Some(1), true));
    assert!(
        !waits_on_a_running_job(&job_row[0], &RunningJobs::none()),
        "a session id in the child snapshot is not a job handle"
    );
    assert_eq!(
        nag_with(&job_row, &children, &RunningJobs::none()),
        nag_with(&job_row, &ChildSessions::none(), &RunningJobs::none()),
        "the child snapshot says nothing about a job edge"
    );
}

/// A clock is armed by a turn, a board write or a prompt, and a child's age is none of those, so
/// the silence needs a deadline of its own: the earliest child a plan waits on leaving its room to
/// breathe, because another row's silence is not this row's.
#[test]
fn the_silence_a_young_child_buys_ends_when_that_child_grows_up() {
    let two = vec![
        waiting_on_a_child("collect the first child's answer", "s-1-sub-2"),
        waiting_on_a_child("collect the second child's answer", "s-1-sub-3"),
    ];
    let children = ChildSessions::none()
        .running("s-1-sub-2", child(5 * 60, Some(4), true))
        .running("s-1-sub-3", child(20 * 60, Some(30), true));
    assert_eq!(
        when_a_young_child_grows_up(&two, &children),
        Some(Duration::from_secs(10 * 60)),
        "the EARLIER child's remaining breath, which is when the first row becomes talkable"
    );

    // A child that is already past it is not a deadline - that row is talkable now, and
    // `nag_should_arm` is what says so; this function answers only about the silence.
    let grown = ChildSessions::none().running("s-1-sub-2", child(45 * 60, Some(60), false));
    assert_eq!(when_a_young_child_grows_up(&two[..1], &grown), None);
    // And a plan that waits on nothing has no such deadline, whatever is running.
    assert_eq!(
        when_a_young_child_grows_up(&[item("open", TodoStatus::Pending)], &children),
        None
    );
}

/// `ChildSessions` is read by a `Child` need and by nothing else, so a daemon full of children
/// leaves a flat or row-edged plan reading exactly as it did.
#[test]
fn a_snapshot_of_children_changes_nothing_about_a_board_that_waits_on_rows() {
    let flat = vec![
        item("first", TodoStatus::Pending),
        item("second", TodoStatus::Pending),
    ];
    let edged = vec![
        item("run the tests", TodoStatus::Pending),
        needs("deploy", TodoStatus::Pending, &["run the tests"]),
    ];
    let children = ChildSessions::none().running("s-other", child(60 * 60, Some(1), true));
    assert_eq!(
        nag_for(&flat),
        nag_with(&flat, &children, &no_jobs()),
        "no needs at all"
    );
    assert_eq!(
        nag_for(&edged),
        nag_with(&edged, &children, &no_jobs()),
        "row edges"
    );
}

/// The graph MOVES: finishing a row is what makes the rows that waited on it ready, so the
/// check's question follows the work rather than the list's order.
#[test]
fn finishing_a_row_makes_what_waited_on_it_ready() {
    let mut plan = vec![
        item("first", TodoStatus::Pending),
        needs("second", TodoStatus::Pending, &["first"]),
    ];
    let (_, named) = unfinished_plan_for(&plan, &plan, None, &ChildSessions::none(), &no_jobs())
        .expect("first can start");
    assert_eq!(named, "first", "the only ready row is the one asked about");

    plan[0].status = TodoStatus::Completed;
    let (msg, named) = unfinished_plan_for(
        &plan,
        &plan,
        Some("first"),
        &ChildSessions::none(),
        &no_jobs(),
    )
    .expect("second can");
    assert_eq!(
        named, "second",
        "the answered row is gone and the graph moved on"
    );
    assert!(msg.contains("  - second"), "{msg}");
    assert!(
        !msg.contains("cannot start yet"),
        "nothing is blocked once the row it waited on is done: {msg}"
    );
}

/// The sticky choice is held only while the row is READY - the composition of the two
/// halves. A row that has left the ready set - done, set aside, or BLOCKED - is no longer a
/// choice, and the head of the ready set is.
#[test]
fn the_sticky_choice_moves_on_when_the_named_row_becomes_blocked() {
    let plan = vec![
        needs("deploy", TodoStatus::Pending, &["later"]),
        item("later", TodoStatus::Pending),
    ];
    let (msg, named) = unfinished_plan_for(
        &plan,
        &plan,
        Some("deploy"),
        &ChildSessions::none(),
        &no_jobs(),
    )
    .expect("one row can start");
    assert_eq!(
        named, "later",
        "the row the check was holding to is blocked, so it is not the choice any more"
    );
    assert!(msg.contains("  - later"), "{msg}");
    assert!(
        msg.contains("`deploy` waits on `later` (still open)"),
        "and the row it stopped holding is still SAID, as blocked: {msg}"
    );
}

/// A slug resolves to the session id, and a name that is no child of this session is refused with
/// nothing written; a `job` need naming one of this session's children becomes a `child` need.
#[test]
fn a_child_named_in_needs_by_its_slug_is_stored_as_its_id() {
    let child = Arc::new(TodoBoard::new(vec![]));
    let (rt, own, _fake) = children_runtime(child);
    let mut sink = RecordingToolSink::new();
    let r = rt.invoke(
        "t1",
        &call(
            r#"{"todos": [
                {"content": "collect the login fix", "status": "pending",
                 "needs": [{"kind": "child", "id": "fix-login"}]},
                {"content": "merge it", "status": "pending",
                 "needs": [{"kind": "job", "handle": "fix-login"}]}]}"#,
        ),
        &mut sink,
    );
    assert_eq!(r.outcome, ToolOutcome::Ok, "{}", r.payload);
    let rows = own.snapshot();
    for row in &rows {
        assert_eq!(
            row.needs,
            vec![TodoNeed::Child {
                id: "s-1-sub-2".into()
            }],
            "{}: the name became the child's id",
            row.content
        );
    }

    let r = rt.invoke(
        "t2",
        &call(
            r#"{"todos": [{"content": "wait for nobody", "status": "pending",
                     "needs": [{"kind": "child", "id": "no-such-child"}]}]}"#,
        ),
        &mut sink,
    );
    assert!(
        matches!(r.outcome, ToolOutcome::Failed { .. }),
        "{:?}",
        r.outcome
    );
    assert!(r.payload.contains("no-such-child"), "{}", r.payload);
    assert_eq!(own.snapshot(), rows, "nothing was written");
}

/// While a monitor watches, the row is quiet, because the firing wakes the session by itself; once
/// it has ended, or for a name this session does not hold, the row is ready.
#[test]
fn a_row_waits_on_a_monitor_until_it_ends() {
    let (rt, board) = runtime();
    let mut sink = RecordingToolSink::new();
    let r = rt.invoke(
        "t1",
        &call(
            r#"{"todos": [{"content": "look at the soak again", "status": "pending",
                     "needs": [{"kind": "monitor", "name": "soak-reminder"}]}]}"#,
        ),
        &mut sink,
    );
    assert_eq!(r.outcome, ToolOutcome::Ok, "{}", r.payload);
    let plan = board.snapshot();
    assert_eq!(
        plan[0].needs,
        vec![TodoNeed::Monitor {
            name: "soak-reminder".into()
        }]
    );
    assert!(
        r.payload
            .contains("(waiting on the monitor `soak-reminder`)"),
        "{}",
        r.payload
    );
    let watching = RunningJobs::none().watching("soak-reminder");
    assert!(
        waits_on_a_running_job(&plan[0], &watching),
        "quiet while it watches"
    );
    assert_eq!(
        unmet_needs(&plan, &plan[0], &ChildSessions::none(), &watching),
        vec![("soak-reminder".to_string(), NeedFate::MonitorWatching)]
    );
    assert_eq!(
        nag_with(&plan, &ChildSessions::none(), &watching),
        None,
        "a plan waiting only on a monitor is not a plan to fix"
    );
    let ended = RunningJobs::none();
    assert!(!waits_on_a_running_job(&plan[0], &ended));
    let msg = nag_with(&plan, &ChildSessions::none(), &ended).expect("ready once it ended");
    assert!(msg.contains("look at the soak again"), "{msg}");

    let r = rt.invoke(
        "t2",
        &call(
            r#"{"todos": [{"content": "x", "status": "pending",
                     "needs": [{"kind": "monitor"}]}]}"#,
        ),
        &mut sink,
    );
    assert!(
        matches!(r.outcome, ToolOutcome::Failed { .. })
            && r.payload.contains("names a monitor by the `name`"),
        "{}",
        r.payload
    );
}
