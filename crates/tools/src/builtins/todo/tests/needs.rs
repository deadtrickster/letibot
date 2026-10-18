//! Needs: edges between rows, children and jobs waited on, and what is ready.

use super::*;

/// **A PLAN CAN WRITE ITS OWN EDGES, and the reply SHOWS them.** Both halves matter: the field
/// is only a graph if the tool that writes the plan can express an edge, and an edge the model
/// cannot SEE is an edge it cannot maintain — the next `todo_write` that re-words a row would
/// break every edge into it with nothing said.
#[test]
fn a_plan_writes_its_edges_and_the_reply_shows_them() {
    let (mut rt, board) = runtime();
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
    // **And the check then asks about the row that can start** — the whole point of the edge.
    let msg = nag_for(&snap).expect("the plan is unfinished");
    assert!(msg.contains("  - run the tests"), "{msg}");
    assert!(
        msg.contains("`deploy` waits on `run the tests` (still open)"),
        "{msg}"
    );
}

/// **The tagged form is accepted as well as the plain name**, so the shape the store and the
/// wire carry is one a model can write too — which is what keeps the tool's vocabulary and the
/// store's from being two spellings a reader has to know.
#[test]
fn a_need_may_be_written_in_the_tagged_form() {
    let (mut rt, board) = runtime();
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

/// **A kind this build cannot evaluate is REFUSED by name** — never dropped. Dropping it would
/// leave a row that looks like it waits for something and waits for nothing, which is the one
/// reading a dependency must never have. (`session` here is a fiction, and deliberately one
/// that is not a kind this build has: `child` IS one, and the whole point of the tag is that a
/// model writing a kind nobody can answer is told so rather than silently dropped.)
#[test]
fn a_need_of_a_kind_this_build_does_not_know_is_refused_by_name() {
    let (mut rt, board) = runtime();
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

/// **A row can be told to WAIT ON A CHILD, and the reply shows it** — the write half of the
/// dependency, and the half that makes the nag's silence reachable at all: a need nothing can
/// write is a rule nothing can exercise.
///
/// Both spellings the parser takes are asserted, because the model has to be able to say this
/// in the tagged form the store and the wire carry — a plain string is a row's name and can
/// never be a child's, so there is exactly one shape here and it has to be the tagged one.
#[test]
fn a_plan_can_wait_on_a_child_by_its_session_id() {
    let (mut rt, board) = runtime();
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
    // **And with no daemon behind this board the child is not running**, which is the honest
    // reading rather than a guess — `ChildSessions::none()` is what a caller with no children
    // hands in, and the row is then ordinary open work.
    assert!(
        nag_for(&board.snapshot()).is_some(),
        "a row whose child is not running is work, not a stuck plan"
    );

    // **An empty id is refused by name**, the same rule an empty row name keeps.
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

/// **A row can be told to WAIT ON A JOB, and the reply shows it** — the write half of the edge
/// the operator asked for (*"todo can be declared to be dependent on a subsession and on a
/// job"*), and the half that makes the idle check's silence reachable at all: a need nothing can
/// write is a rule nothing can exercise.
///
/// The handle is the one `bash` with `background: true` and `job_list` print, quoted verbatim
/// for the same reason a child's session id is: it is what `job_output` and `job_kill` are given.
#[test]
fn a_plan_can_wait_on_a_job_by_its_handle() {
    let (mut rt, board) = runtime();
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
    // **And with no daemon behind this board the job is not running**, which is the honest
    // reading rather than a guess — `RunningJobs::none()` is what a caller with no process host
    // hands in, and the row is then ordinary open work.
    assert!(
        nag_for(&board.snapshot()).is_some(),
        "a row whose job is not running is work, not a stuck plan"
    );

    // **An empty handle is refused by name**, the same rule an empty row name and an empty
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

/// **An empty name and a `needs` that is not a list are refused by name too** — the same rule,
/// one level down: an edge that names nothing is not an edge.
#[test]
fn an_empty_need_and_a_malformed_needs_are_refused_by_name() {
    let (mut rt, board) = runtime();
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

/// **A BOARD WITH NO EDGES IS READ EXACTLY AS IT WAS BEFORE EDGES EXISTED** — the compatibility
/// claim, as evidence rather than as a promise.
///
/// Two halves, and the second is what makes the first more than a golden string. The TEXT is
/// asserted byte for byte against what `1d24fab`'s implementation produced — every `state` arm
/// and the count — so a change to any of them fails here. And the STRUCTURE is asserted too:
/// with no `needs` anywhere, `as_a_graph` is the IDENTITY on `open_priority` — nothing can be
/// blocked and the ready set is the whole queue — which is *why* the text cannot have moved,
/// rather than a coincidence that it has not.
#[test]
fn a_board_with_no_dependencies_is_read_exactly_as_before() {
    let plan = vec![
        item("write it up", TodoStatus::Completed),
        item("wire the check", TodoStatus::InProgress),
        item("test it", TodoStatus::Pending),
    ];
    // R93.B1–B3: the sentence changed deliberately — no count, no restating suffix, one line of
    // advice — and it is pinned byte for byte here as the old one was.
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

    // **AND THE STRUCTURE**: the graph split of a board with no edges is the identity.
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

/// **An edge that is MET is inert.** The same board with a satisfied need is read exactly as
/// the same board with no need at all — which is the general form of the compatibility claim,
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

/// **A BLOCKED ROW IS NOT THE ASK, and the row behind it is** — the whole point of the DAG.
///
/// The defect this replaces is head-of-line blocking: `open_priority(todos).first()` named the
/// first open row whatever it was waiting for, so a row that could not start was asked about
/// for ever and the row that could start was never reached.
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
    // **And it is SAID rather than skipped**, because a blocked row is invisible to the ask and
    // a plan whose edges went stale would otherwise look like a shorter plan.
    assert!(
        msg.contains("1 row cannot start yet: `deploy` waits on `run the tests` (still open)."),
        "what cannot start is said, with what it waits for: {msg}"
    );
}

/// **AN EDGE INTO A ROW THE OPERATOR STRUCK OFF IS NOT *STILL OPEN* — it can never be met.**
///
/// The interaction between the two halves of this change, and the reason it has its own
/// sentence: `/todo rm N` cancels a row rather than removing it, so a plan's edge can now name a
/// row that is on the board and will never be done. Saying *"still open"* about it sends the
/// model to finish work the operator has decided against — the nag this check exists not to
/// produce, and the same defect the *fourteen turns* measurement names. What the reader needs is
/// that the edge is dead and that the remedy is its own: `todo_write` can drop or re-point a
/// need.
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
    // narrowed one — exactly what `harnessd`'s `the_plan_as_checked` builds.
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
        msg.contains("re-word it"),
        "and the remedy, which is the model's own: {msg}"
    );
    assert!(
        !msg.contains("(still open)"),
        "**and never says *still open* about a row nobody is going to finish**: {msg}"
    );
}

/// **THE DEGENERATE CASE, designed deliberately: an unfinished plan with an EMPTY ready set.**
///
/// Everything left waits on something, so there is nothing to work on and something to UNBLOCK.
/// Going quiet here would read as *nothing to do*, which is the one reading that is wrong; and
/// asking about the head of the list anyway is the head-of-line blocking this replaced. A cycle
/// lands in exactly this state, which is why it is the example.
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

/// **An unresolvable need is NEVER met** — the rule `TodoCondition` learned first, *a condition
/// nobody can evaluate must never read as met*.
///
/// A name that matches no row is the common case and the one the message has to make
/// repairable: a row was re-worded, or deleted, or the name was never right.
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

/// **A name that matches two rows is unresolvable** — the house rule for an ambiguous name,
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

/// **A need of a kind this build cannot evaluate blocks the row and SAYS SO** — the promise
/// `TodoCondition` makes in its own words, kept where breaking it would be silent. This is the
/// value a NEWER build's store leaves behind; see `TodoNeed::Unknown` for why it does not fail
/// the whole plan instead.
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

/// **A row that waits on a CHILD is silent while the child is YOUNG, and spoken about once it
/// has been running long enough to be worth a look.**
///
/// The operator's directive, and the failure it was written against: *"so you are not nagged
/// till childs are running for the first 30 minutes of child life"*, because a row marked in
/// progress with a child working on it was nagged four times in twenty minutes.
///
/// The two halves are asserted on the SAME plan and the SAME child, forty minutes apart in age:
/// the only fact that changes is the age, and what changes with it is whether the check may
/// speak about the row at all. The narrowing is the caller's — `waits_on_a_child_with_room_to_
/// breathe`, which is what `harnessd`'s `the_check_may_ask_about` reads — so this asserts the
/// rule where it lives rather than through a daemon.
#[test]
fn a_child_that_is_still_young_keeps_the_row_silent_and_an_old_one_is_spoken_about() {
    let plan = vec![waiting_on_a_child(
        "collect the child's answer",
        "s-1-sub-2",
    )];

    // **Five minutes old: the row is untalkable.** The predicate the caller narrows with says
    // so — and the child is at its most alive, generating four seconds ago.
    let young = ChildSessions::none().running("s-1-sub-2", child(5 * 60, Some(4), true));
    assert!(
        waits_on_a_child_with_room_to_breathe(&plan[0], &young),
        "a row waiting on a five-minute-old child is one the check may not speak about"
    );

    // **Forty-five minutes old: it is talkable, and what it says is *check the child*.** The
    // child has gone quiet — a turn is in flight and nothing has come out of it for twelve
    // minutes — which is the state the operator wants the model steered to. **A turn in flight
    // is what makes this the *stuck* case**: the same silence with NO turn in flight is a child
    // between turns, and `a_child_between_turns_is_told_to_be_asked_and_not_killed` below is
    // that state's own sentence.
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

/// **A WORKING child and a QUIET one are told apart by the sentence** — the second half of the
/// directive, and the whole of the report that prompted it: *"I have just asked the registry
/// three times about another row's child and got the identical sentence every time — 'still
/// working' — which tells me it has not died and nothing at all about whether it is stuck."*
///
/// Both children are the same age here — forty-five minutes — so the age decides nothing and
/// the difference between the two notices is only what the child last showed. That is the whole
/// point: a notice that can report liveness alone cannot steer anybody.
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

    // **The same age, nothing moving.** The two notices differ, and that difference is the
    // whole of what the operator asked for.
    let quiet = ChildSessions::none().running("s-1-sub-2", child(45 * 60, Some(12 * 60), true));
    let quiet_msg = nag_with(&plan, &quiet, &no_jobs()).expect("a stuck plan speaks");
    assert_ne!(msg, quiet_msg, "working and quiet must not read the same");
    assert!(
        quiet_msg.contains("check whether it is stuck"),
        "{quiet_msg}"
    );
}

/// **An IDLE child is told to be ASKED, and not to be killed** — the third state, and the one
/// that had no sentence at all.
///
/// The operator's own measurement: *"it was between turns … a turn has been started for it"*
/// was learned only by sending a message nothing said to send. The reading is one fact — **no
/// turn is in flight and the child is still there** — and the move it asks for is
/// `task_message`, which is the only door that reaches a parked child.
///
/// **What it must NOT say is `job_kill`**, and that is the assertion with teeth: the old
/// sentence sent its reader to stop a child that was doing nothing wrong, waiting to be asked.
#[test]
fn a_child_between_turns_is_told_to_be_asked_and_not_killed() {
    let plan = vec![waiting_on_a_child(
        "collect the child's answer",
        "s-1-sub-2",
    )];
    // Forty-five minutes old, nothing for twelve minutes, and **no turn in flight**: the child
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

    // **And the state is the reading's own**, so the same child with a turn in flight keeps the
    // check sentence — the two states cannot be told apart by anything but the fact.
    let working = ChildSessions::none().running("s-1-sub-2", child(45 * 60, Some(12 * 60), true));
    let other = nag_with(&plan, &working, &no_jobs()).expect("a stuck plan speaks");
    assert_ne!(msg, other, "idle and wedged must not read the same");
}

/// **A child that has FINISHED resolves the need, and the row stops being about the child.**
///
/// A finished child is simply not in the snapshot — the daemon's journal says `done` — so the
/// need is met, the row is READY, and the check asks about it as ordinary work. This is the
/// half that makes the need a dependency rather than a note about one, and it is the same
/// reading `TodoCondition::Job` gives a handle it does not know.
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

/// **A row that waits on a JOB is silent while the job runs, and asked about once it ends.**
///
/// The operator's ask, 2026-10-11: *"todo can be declared to be dependent on a subsession and on
/// a job ... and that should drastically decrease nags count"* — measured the same night, when
/// each of their long jobs had a row nagging while it ran.
///
/// **The same plan and the same handle throughout**, and the only fact that changes is whether
/// the job is running. While it is, `waits_on_a_running_job` is what the caller narrows with —
/// `harnessd`'s `the_check_may_ask_about` reads it exactly as it reads the child one — so the row
/// is not asked about AND not listed as blocked. When the job ends the edge is MET and what the
/// check says is the row itself.
#[test]
fn a_running_job_keeps_the_row_silent_and_the_end_of_it_asks_again() {
    let plan = vec![waiting_on_a_job("push the branch", "j2070")];

    // **Running: the row is one the check may not speak about at all.**
    let running = RunningJobs::none().running("j2070");
    assert!(
        waits_on_a_running_job(&plan[0], &running),
        "a row waiting on a running job is one the check may not speak about"
    );
    // **And a caller that did NOT narrow still gets the truth rather than a silence**: the edge
    // is unmet, and the handle is what it is named by — it is what `job_output` is given.
    assert_eq!(
        unmet_needs(&plan, &plan[0], &ChildSessions::none(), &running),
        vec![("j2070".to_string(), NeedFate::JobRunning)],
        "the edge is unmet while the job runs"
    );
    // R93.B5: and a plan blocked ONLY on a running job is silent even unnarrowed — the
    // *"NOTHING can start … a plan to fix"* notice fired on exactly this, which `todo_write`
    // promises is silent.
    assert_eq!(
        nag_with(&plan, &ChildSessions::none(), &running),
        None,
        "a plan waiting only on a running job is not a plan to fix"
    );

    // **And the lead a reader gets when there IS other work to do.** A blocked row is SAID
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

    // **Ended: the edge is met, and the row is the work the check asks about.**
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

/// **A handle NOBODY knows is a job that ended** — `TodoCondition::Job`'s own ruling, read as an
/// edge: *"a job that is not running — including one this session has never heard of, which is
/// what a handle looks like after a restart — is a job that ended"*.
///
/// This is the half that keeps the feature from inventing a way to wait for ever: a plan resumed
/// after a restart names handles the new daemon has never seen, and every one of them is a job
/// that is over. The row is READY, which is what the model is then asked about.
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

/// **A job edge and a child edge are read by their OWN snapshot, and a daemon full of one says
/// nothing about the other** — the compatibility half, said the other way round from the
/// child-side test above.
///
/// A board that waits on rows is read exactly as it was before either kind existed, whether the
/// caller hands in running jobs, running children, or both; and a row waiting on a job is
/// unmoved by a snapshot full of children (and the reverse). One handle is one question.
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
    // **The one thing a running job DOES add is the offer of `wait_on`** — a sentence appended to
    // the advice, naming a handle this session holds — and it is taken off before comparing: the
    // claim here is about the PLAN's reading, which the offer leaves alone.
    // The offer runs from its first sentence ("It reads like …", or "If this row is only
    // waiting …") to the end of the advice line, "Also running: …" included — the line it rides
    // so the head's fold still folds it.
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

/// **The silence a young child buys has an END, and the clock is told when it is.**
///
/// A clock is armed by a turn, a board write or a prompt, and a child's AGE is none of those —
/// so a row that went quiet at five minutes would stay quiet until somebody happened to do
/// something, and the steering the operator asked for would arrive only by accident. This is the
/// one number that closes that: the moment the FIRST of the children a plan waits on leaves its
/// room to breathe.
///
/// Three shapes, and the middle one is why it is a `min`: a plan waiting on two young children
/// is due when the EARLIER one grows up, not when both have — the other row's silence is not
/// this row's.
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

    // **A child that is already past it is not a deadline** — that row is talkable now, and
    // `nag_should_arm` is what says so; this function answers only about the silence.
    let grown = ChildSessions::none().running("s-1-sub-2", child(45 * 60, Some(60), false));
    assert_eq!(when_a_young_child_grows_up(&two[..1], &grown), None);
    // **And a plan that waits on nothing has no such deadline**, whatever is running.
    assert_eq!(
        when_a_young_child_grows_up(&[item("open", TodoStatus::Pending)], &children),
        None
    );
}

/// **A daemon full of children changes nothing about a board that waits on rows** — the
/// compatibility half, said the other way round. `ChildSessions` is read by a `Child` need and
/// by nothing else, so a flat plan and a row-edged plan are both read exactly as they were
/// before any of this existed, whether the caller knows about children or not.
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

/// **The graph MOVES**: finishing a row is what makes the rows that waited on it ready, so the
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

/// **The sticky choice is held only while the row is READY** — the composition of the two
/// halves. A row that has left the ready set — done, set aside, or BLOCKED — is no longer a
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

/// **A child named in `needs` by its slug is stored as its session id**, and a name that is no
/// child of this session is refused with nothing written — the resolution `wait_on`'s `child`
/// already goes through. A `job` need that names no job but is one of this session's children
/// becomes a `child` need. A session id is stored as written.
#[test]
fn a_child_named_in_needs_by_its_slug_is_stored_as_its_id() {
    let child = Arc::new(TodoBoard::new(vec![]));
    let (mut rt, own, _fake) = children_runtime(child);
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

/// **A row can wait on a MONITOR** — a timer, a file, a pull request: whatever a monitor watches.
/// While it watches the row is quiet (its firing wakes the session by itself); once it has ended,
/// or for a name this session does not hold (a restart), the row is ready. The operator's *"come
/// with it later"* is a row waiting on a one-shot timer.
#[test]
fn a_row_waits_on_a_monitor_until_it_ends() {
    let (mut rt, board) = runtime();
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
