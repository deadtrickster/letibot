//! A parent writing its children's boards.

use super::*;

/// **A parent's write UPSERTS and never replaces — the child's rows, the operator's rows and
/// the version's economy all survive it.**
///
/// This is the hazard the whole feature turns on, measured rather than argued: under the old
/// whole-list-replace contract a parent that wrote a child's board would have deleted the
/// child's plan with its own three rows. Every arm below is one thing that must not move.
#[test]
fn a_parents_write_upserts_its_own_rows_and_nobody_elses_move() {
    let b = TodoBoard::new(vec![
        item("the child's own step", TodoStatus::InProgress),
        TodoItem {
            content: "the operator's row".into(),
            status: TodoStatus::Pending,
            by: TodoBy::Operator,
            when: None,
            needs: Vec::new(),
        },
    ]);
    let author = TodoBy::parent_of("s-1789462738453908838");

    // Two rows arrive from the parent.
    let changed = b
        .upsert_parent(
            &[
                (
                    "land the parity row".into(),
                    TodoStatus::Pending,
                    Vec::new(),
                ),
                ("write the report".into(), TodoStatus::Pending, Vec::new()),
            ],
            &author,
        )
        .unwrap();
    assert_eq!(changed, 2, "both rows were added");
    assert_eq!(b.version(), 1);

    // **The child's rows and the operator's are untouched, and the union orders them:
    // what the model chose, then what it was told, then the operator's.**
    let all = b.snapshot();
    assert_eq!(
        all.iter().map(|t| t.content.as_str()).collect::<Vec<_>>(),
        vec![
            "the child's own step",
            "land the parity row",
            "write the report",
            "the operator's row",
        ],
        "the parent's rows went BETWEEN the model's and the operator's: {all:?}"
    );
    assert_eq!(b.parent_snapshot().len(), 2);
    assert!(
        b.parent_snapshot().iter().all(|t| t.by == author),
        "every row in the parent's half carries the author exactly: {:?}",
        b.parent_snapshot()
    );

    // **A re-send is a state update, not a duplicate** — and the SAME text under a different
    // author's name would be a different row, which is the scoping, but one parent is one
    // author, so the second send moves what the first wrote.
    let changed = b
        .upsert_parent(
            &[(
                "land the parity row".into(),
                TodoStatus::Completed,
                Vec::new(),
            )],
            &author,
        )
        .unwrap();
    assert_eq!(changed, 1, "the existing row moved, nothing was added");
    assert_eq!(b.parent_snapshot().len(), 2, "still two rows: no duplicate");
    assert_eq!(b.version(), 2);

    // **Omission is not deletion.** The parent sends one row and leaves the other out —
    // under the own-board contract that would delete it; here it must not.
    let changed = b
        .upsert_parent(
            &[(
                "write the report".into(),
                TodoStatus::InProgress,
                Vec::new(),
            )],
            &author,
        )
        .unwrap();
    assert_eq!(changed, 1);
    assert_eq!(
        b.parent_snapshot().len(),
        2,
        "omitting `land the parity row` did not remove it: {:?}",
        b.parent_snapshot()
    );

    // **An unchanged re-send is not an event** — the version rule `set_operator_states` keeps.
    let before = b.version();
    let changed = b
        .upsert_parent(
            &[(
                "write the report".into(),
                TodoStatus::InProgress,
                Vec::new(),
            )],
            &author,
        )
        .unwrap();
    assert_eq!(changed, 0, "nothing changed");
    assert_eq!(b.version(), before, "and nothing was announced");

    // **And the CHILD's own write cannot reach them either** — the other edge of the same rule.
    // The child re-plans its own half and the parent's rows stay; and because a row left out is
    // not a row removed, its own old row stays too — what it writes is what it ADDS or RESTATES.
    b.upsert_model(&[
        (
            "the child's own step".into(),
            TodoStatus::Completed,
            Vec::new(),
        ),
        (
            "the child's revised step".into(),
            TodoStatus::Pending,
            Vec::new(),
        ),
    ]);
    let all = b.snapshot();
    assert_eq!(
        all.iter().map(|t| t.content.as_str()).collect::<Vec<_>>(),
        vec![
            "the child's own step",
            "the child's revised step",
            "land the parity row",
            "write the report",
            "the operator's row",
        ],
        "the child's write moved somebody else's rows: {all:?}"
    );
}

/// **A parent's row is NAG-WORTHY BY CONSTRUCTION** — and that is the whole of “the child
/// learns via usual todo nags”.
///
/// The selection rules are read here, not assumed: `unfinished_plan` filters by STATUS
/// (completed is out, postponed is out) and by nothing else — there is no `by` anywhere in
/// `open_priority` — so a pending parent row is asked about exactly like the child's own.
/// The child's own row is completed here so the parent's is the one named, which pins both
/// the row and the author string the nag shows.
#[test]
fn a_parent_row_is_nag_worthy_by_construction() {
    let author = TodoBy::parent_of("s-1789462738453908838");
    let all = vec![
        item("the child's own step", TodoStatus::Completed),
        TodoItem {
            content: "land the parity row".into(),
            status: TodoStatus::Pending,
            by: author.clone(),
            when: None,
            needs: Vec::new(),
        },
    ];
    let nag = nag_for(&all).expect("a pending parent row is open work");
    assert!(
        nag.contains("land the parity row"),
        "the nag names the parent's row: {nag}"
    );
    assert!(
        nag.contains("Parent s-1789462738453908838"),
        "and its author — the operator's string, in full, so the child can tell what it was \
             told from what it decided: {nag}"
    );
    assert!(
        nag.contains("Your parent asked for it"),
        "and the advice is the parent's, not the operator's and not the model's own: {nag}"
    );
    assert!(
        nag.contains("they retire the row"),
        "the child is told who disposes of the row: {nag}"
    );

    // And the queue serves the child's own open work first — the model's rows precede the
    // parent's in the union, and the stable sort keeps that order inside a band.
    let board = TodoBoard::new(vec![
        item("mine, pending", TodoStatus::Pending),
        TodoItem {
            content: "theirs, pending".into(),
            status: TodoStatus::Pending,
            by: author,
            when: None,
            needs: Vec::new(),
        },
    ]);
    let served: Vec<String> = open_priority(&board.snapshot())
        .into_iter()
        .map(|t| t.content.clone())
        .collect();
    assert_eq!(served, vec!["mine, pending", "theirs, pending"]);
}

/// **The reply names all three authors** — the child reading its own board (its own
/// `todo_write` reply renders the union) has to be able to tell what it decided from what it
/// was told, and by whom.
#[test]
fn the_reply_names_all_three_authors() {
    let shown = render(&[
        item("mine", TodoStatus::Pending),
        TodoItem {
            content: "told by the parent".into(),
            status: TodoStatus::Pending,
            by: TodoBy::parent_of("s-1789462738453908838"),
            when: None,
            needs: Vec::new(),
        },
        TodoItem {
            content: "the operator's".into(),
            status: TodoStatus::Pending,
            by: TodoBy::Operator,
            when: None,
            needs: Vec::new(),
        },
    ]);
    assert!(shown.contains("— yours"), "{shown}");
    assert!(
        shown.contains("— Parent s-1789462738453908838"),
        "the author string verbatim, not a display name: {shown}"
    );
    assert!(shown.contains("— the operator's"), "{shown}");
}

/// **A `target` write reaches the child and ONLY the child.** The tool's own board is the
/// thing that must not move: the target path returns before any of the own-board code runs,
/// and the reply is the CHILD's board — the parent's confirmation that its rows landed
/// beside the child's own.
#[test]
fn a_target_write_reaches_the_child_and_not_this_board() {
    let child = Arc::new(TodoBoard::new(vec![item(
        "the child's step",
        TodoStatus::InProgress,
    )]));
    let (mut rt, own, _fake) = children_runtime(child);
    let mut sink = RecordingToolSink::new();
    let r = rt.invoke(
        "t1",
        &call(
            r#"{"target": "s-…908838", "todos": [{"content": "land the parity row", "status": "pending"}]}"#,
        ),
        &mut sink,
    );
    assert_eq!(r.outcome, ToolOutcome::Ok, "{} {:?}", r.payload, r.outcome);
    assert!(
        r.payload.contains("the child's step"),
        "the reply shows the CHILD's board: {}",
        r.payload
    );
    assert!(
        r.payload.contains("— Parent s-1789462738453908838"),
        "with the row's author as the child will see it: {}",
        r.payload
    );
    assert!(
        r.payload.contains("untouched"),
        "and the upsert rule said in words: {}",
        r.payload
    );
    // **And this session's own board did not move.**
    assert_eq!(own.version(), 0, "the parent's own board was not written");
    assert!(own.snapshot().is_empty(), "nor replaced, nor cleared");
}

/// **The tool's own refusals on the target path, each BY NAME** — the daemon-side refusal
/// (not-a-child) is the resolver's and is tested with the daemon; these are the ones the
/// tool owes itself.
#[test]
fn the_target_paths_own_refusals_are_by_name() {
    let child = Arc::new(TodoBoard::new(Vec::new()));
    let (mut rt, own, _fake) = children_runtime(child.clone());
    let mut sink = RecordingToolSink::new();

    // `target` and `operator` in one call: two boards' concerns, refused together.
    let r = rt.invoke(
        "t1",
        &call(
            r#"{"target": "s-…908838", "operator": [{"content": "x", "status": "completed"}], "todos": [{"content": "y", "status": "pending"}]}"#,
        ),
        &mut sink,
    );
    assert!(
        matches!(r.outcome, ToolOutcome::Failed { .. }),
        "{:?}",
        r.outcome
    );
    let said = format!("{} {:?}", r.payload, r.outcome);
    assert!(
        said.contains("do not mix"),
        "the refusal names both fields: {said}"
    );
    assert!(said.contains("s-…908838"), "and the child it named: {said}");
    assert!(own.snapshot().is_empty(), "and nothing moved here either");
    assert!(
        child.snapshot().is_empty(),
        "the child's board was not touched by the refusal"
    );

    // An empty `todos` with a `target` is not a clear — there is no delete to mean by it.
    let r = rt.invoke(
        "t1",
        &call(r#"{"target": "s-…908838", "todos": []}"#),
        &mut sink,
    );
    assert!(matches!(r.outcome, ToolOutcome::Failed { .. }));
    assert!(r.payload.contains("there is no delete"), "{}", r.payload);

    // A `by` smuggled into an entry: the author is the daemon's to stamp, never the call's.
    let r = rt.invoke(
        "t1",
        &call(
            r#"{"target": "s-…908838", "todos": [{"content": "x", "status": "pending", "by": "operator"}]}"#,
        ),
        &mut sink,
    );
    assert!(matches!(r.outcome, ToolOutcome::Failed { .. }));
    let said = format!("{} {:?}", r.payload, r.outcome);
    assert!(
        said.contains("`by` is not yours to send"),
        "the refusal names the forged field: {said}"
    );
    assert!(
        child.snapshot().is_empty(),
        "nothing reached the child's board"
    );
}

/// **A `target` with no resolver behind it is refused by name** — the standalone runtime's
/// honest answer, rather than a write quietly aimed at this session's own board.
#[test]
fn a_target_with_no_children_behind_it_is_refused_by_name() {
    let (mut rt, own) = runtime();
    let mut sink = RecordingToolSink::new();
    let r = rt.invoke(
        "t1",
        &call(r#"{"target": "s-…908838", "todos": [{"content": "x", "status": "pending"}]}"#),
        &mut sink,
    );
    assert!(
        matches!(r.outcome, ToolOutcome::Failed { .. }),
        "{:?}",
        r.outcome
    );
    let said = format!("{} {:?}", r.payload, r.outcome);
    assert!(said.contains("no children's boards"), "{}", said);
    assert!(own.snapshot().is_empty(), "and this board stayed empty");
}
