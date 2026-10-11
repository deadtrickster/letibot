//! The board: the operator's rows beside the model's, the union, and upserts.

use super::*;

/// **REPLACING THE HALF WIPES STATE THE OTHER AUTHOR SET ON IT — the hazard the file-migration
/// design has to answer before anything else.**
///
/// The proposed shape is *"you literally migrate the whole todo, starting from the top level"*:
/// `TODO.md` becomes the operator's half, re-read when a watcher notices the file change, and
/// sent as `SetOperatorTodos`. That frame calls `set_operator`, which **replaces** the half.
///
/// But the model's only way to dispose of the operator's rows is `set_operator_states`, which
/// moves STATE and deliberately cannot change membership. So the two writes disagree about what
/// the half IS: the model changes one row's status, and the next whole-file migration sends the
/// FILE's statuses and takes that mark away.
///
/// MEASURED here rather than argued. The order is the one a session would actually see:
/// migrate, work, mark done, file touched, migrate again.
#[test]
fn a_whole_file_migration_would_wipe_the_state_the_model_set() {
    let b = TodoBoard::new(Vec::new());

    // 1. The file migrates whole — the operator's half, as `TODO.md` has it.
    let from_file = || {
        vec![
            TodoItem {
                content: "T1 vendor the deps".into(),
                status: TodoStatus::Pending,
                by: TodoBy::Operator,
                when: None,
                needs: Vec::new(),
            },
            TodoItem {
                content: "T2 wire the pane".into(),
                status: TodoStatus::Pending,
                by: TodoBy::Operator,
                when: None,
                needs: Vec::new(),
            },
        ]
    };
    b.set_operator(from_file());

    // 2. The model does T1 and marks it — the mechanism that already exists for exactly this.
    let changed = b
        .set_operator_states(&[("T1 vendor the deps".into(), TodoStatus::Completed)])
        .expect("the model marks the operator's row done");
    assert_eq!(changed, 1);
    assert_eq!(
        b.operator_snapshot()[0].status,
        TodoStatus::Completed,
        "the premise: the model's mark is on the board"
    );

    // 3. **The operator touches a line** — any line, even T2's, even a comment — and the watcher
    //    re-migrates the whole file, because a whole-file migration is what removes the
    //    line-level problem. The file still says `[ ]` for T1, because the model's work was
    //    never written back to it.
    b.set_operator(from_file());

    // **The assertion is that the wipe HAPPENED** — this test holds the hazard still while the
    // design answers it, so it fails the day `set_operator` learns to preserve state, and that
    // failure is the signal to rewrite it as the positive.
    assert_eq!(
        b.operator_snapshot()[0].status,
        TodoStatus::Pending,
        "the model's mark survived a whole-file migration, so the hazard this records is gone \
             — rewrite this test as the positive"
    );
}

/// And the tag question, measured: **a tag in the tail survives both paths to the model**, so
/// `TodoEntry` needs nothing for it — provided the model is told the whole tree.
///
/// `content` is printed VERBATIM by `render` (that is what makes the words quotable back, which
/// `set_operator_states` depends on), and the nag's only edit is `trim`, which removes
/// whitespace and not a trailing word. So a line's tail rides the wire as part of the row.
#[test]
fn a_tag_in_the_lines_tail_reaches_the_model_with_no_field_for_it() {
    let b = TodoBoard::new(vec![TodoItem {
        content: "T1 vendor the deps  #model".into(),
        status: TodoStatus::Pending,
        by: TodoBy::Operator,
        when: None,
        needs: Vec::new(),
    }]);
    let all = b.snapshot();
    // The renderer the model reads.
    let rendered = render(&all);
    assert!(
        rendered.contains("#model"),
        "the tag did not survive into what the model is told: {rendered}"
    );
    // And the nag, whose only edit is a trim.
    let nag = nag_for(&all).expect("one row is open");
    assert!(
        nag.contains("#model"),
        "the tag did not survive the nag: {nag}"
    );
}

/// **A POSTPONED row is still the model's to SEE, and the reply says what the mark means.**
///
/// The operator's ask has two halves and this is the second one: the row *"persists"* — so it
/// is in the list the model is handed, with its own words and its author, which is what keeps
/// `todo_write`'s `operator` field able to aim at it — and it is *"without nag"*, which from the
/// model's side means it can stop proposing the row **without being told twice**. That is what
/// the legend below the list is for: `[p]` with no explanation is a mark the next turn reads as
/// *mine to pick up*, and the model proposing it is the nagging arriving through its own good
/// manners instead of through the clock.
///
/// And the state is not one the model may set: `todo_write` still takes three words, so a model
/// cannot silence the check that exists to stop it abandoning a plan. Asserted on the refusal
/// itself, because that is where a fourth word would arrive.
#[test]
fn a_postponed_row_is_marked_in_what_the_model_is_shown() {
    let set_aside = TodoItem {
        content: "push once CI lands".into(),
        status: TodoStatus::Postponed,
        by: TodoBy::Operator,
        when: Some(TodoCondition::Job {
            handle: "j121".into(),
        }),
        needs: Vec::new(),
    };
    let all = vec![item("the model's own row", TodoStatus::Pending), set_aside];

    let shown = render(&all);
    assert!(
        shown.contains("[p] push once CI lands"),
        "the row keeps its place in the list and is marked: {shown}"
    );
    assert!(
        shown.contains("— the operator's"),
        "and says whose it is, so `operator` can still aim at it: {shown}"
    );
    assert!(
        shown.contains("do not propose it again"),
        "the mark's meaning has to be in the reply, not only in the mark: {shown}"
    );
    // The state is the OPERATOR's, so the word is not one this tool takes.
    let (mut rt, _board) = runtime();
    let mut sink = RecordingToolSink::new();
    let r = rt.invoke(
        "t1",
        &call(r#"{"todos": [{"content": "mine", "status": "postponed"}]}"#),
        &mut sink,
    );
    assert!(
        matches!(r.outcome, ToolOutcome::Failed { .. }),
        "a model must not be able to set a row aside: {:?}",
        r.outcome
    );
    let said = format!("{} {:?}", r.payload, r.outcome);
    assert!(
        said.contains("pending, in_progress, completed"),
        "and the refusal names the words it does take: {said}"
    );
}

/// **A CANCELLED ROW IS MARKED IN WHAT THE MODEL IS SHOWN — AND THE WORD IS NOT THE MODEL'S.**
///
/// The operator's two rulings are what the mark is for: *"only i should be able to delete todo
/// items. as a rule everything that ever created stays in history"* and *"so done items or
/// canceled items should be kept"*. So a cancelled row keeps its place in the list the model is
/// shown, marked, with the mark's meaning in the legend — a model that read `[c]` as *still mine
/// to pick up* would propose work the operator has already decided against, which is the nagging
/// the board exists to stop, arriving through the model's own good manners.
///
/// And the state is not one the model may set: `todo_write` still takes three words, asserted on
/// the refusal itself, because that is where a fifth word would arrive.
#[test]
fn a_cancelled_row_is_marked_and_the_word_is_not_the_models() {
    let struck_off = TodoItem {
        content: "push leticl to github".into(),
        status: TodoStatus::Cancelled,
        by: TodoBy::Operator,
        when: None,
        needs: Vec::new(),
    };
    let all = vec![item("the model's own row", TodoStatus::Pending), struck_off];

    let shown = render(&all);
    assert!(
        shown.contains("[c] push leticl to github"),
        "**the row keeps its place in the list and is marked**: {shown}"
    );
    assert!(
        shown.contains("— the operator's"),
        "and says whose it is: {shown}"
    );
    assert!(
        shown.contains("STRUCK OFF"),
        "the mark's meaning has to be in the reply, not only in the mark: {shown}"
    );
    // The state is the OPERATOR's, so the word is not one this tool takes.
    let (mut rt, _board) = runtime();
    let mut sink = RecordingToolSink::new();
    let r = rt.invoke(
        "t1",
        &call(r#"{"todos": [{"content": "mine", "status": "cancelled"}]}"#),
        &mut sink,
    );
    assert!(
        matches!(r.outcome, ToolOutcome::Failed { .. }),
        "**a model must not be able to strike a row off**: {:?}",
        r.outcome
    );
    let said = format!("{} {:?}", r.payload, r.outcome);
    assert!(
        said.contains("pending, in_progress, completed"),
        "and the refusal names the words it does take: {said}"
    );
}

/// **THE SET-ASIDE SURVIVES THE MODEL'S NEXT WRITE, AND AN ANSWER DOES NOT.**
///
/// This is the half that makes the operator's reach over a MODEL's row real rather than a
/// gesture. The model's half is replaced wholesale on every `todo_write`, and the row comes back
/// with one of the three words it can spell — `pending`, because the work is not done — so
/// without this rule the operator's silence would die on the model's very next revision of the
/// plan. **A status the model cannot spell is not the model's to clear.**
///
/// And a row the model ANSWERS is answered: `completed` is not a way of clearing the operator's
/// word, it is the work being done, and there is nothing left to be silent about.
///
/// The refusals are asserted too, because they are the other half of content-keying: a name
/// that matches nothing, or two rows at once, is refused rather than guessed at.
#[test]
fn a_set_aside_on_a_model_row_survives_the_models_rewrite() {
    let b = TodoBoard::new(vec![item("operator: paste the token", TodoStatus::Pending)]);
    assert_eq!(
        b.set_states(&[("operator: paste the token".into(), TodoStatus::Postponed)])
            .expect("the row is on the model's half and is named exactly"),
        1
    );
    assert_eq!(b.snapshot()[0].status, TodoStatus::Postponed);

    // The model rewrites its whole list and restates the row as open work.
    b.upsert_model(&[(
        "operator: paste the token".into(),
        TodoStatus::Pending,
        Vec::new(),
    )]);
    assert_eq!(
        b.snapshot()[0].status,
        TodoStatus::Postponed,
        "**the operator's silence outlives the plan's next revision**"
    );

    // A row the model ANSWERS is answered — the set-aside has nothing left to do.
    b.upsert_model(&[(
        "operator: paste the token".into(),
        TodoStatus::Completed,
        Vec::new(),
    )]);
    assert_eq!(b.snapshot()[0].status, TodoStatus::Completed);

    // A name that resolves to nothing is refused, not guessed at.
    assert!(
        b.set_states(&[("nobody says this".into(), TodoStatus::Postponed)])
            .is_err(),
        "a name that matches no row of the model's is not a row to move"
    );
    // And so is one that matches two — the board's one key can collide.
    let two = TodoBoard::new(vec![
        item("the same words", TodoStatus::Pending),
        item("the same words", TodoStatus::InProgress),
    ]);
    assert!(
        two.set_states(&[("the same words".into(), TodoStatus::Postponed)])
            .is_err(),
        "two rows with one name is not a row to move"
    );
}

/// **A cancelled row is not in the queue of open work.** The queue is what the check asks about
/// and what `(N more open)` counts, so a struck-off row must not be in it — the operator has
/// decided against the work, and a plan that still served it would be nagging about something
/// nobody owes. `Postponed` is deliberately NOT excluded there (it is work still owed, and the
/// daemon's own predicate is what keeps it out of the check).
#[test]
fn a_cancelled_row_is_not_in_the_queue_of_open_work() {
    let plan = vec![
        item("open", TodoStatus::Pending),
        item("struck off", TodoStatus::Cancelled),
        item("done", TodoStatus::Completed),
    ];
    let queued: Vec<&str> = open_priority(&plan)
        .iter()
        .map(|t| t.content.as_str())
        .collect();
    assert_eq!(
        queued,
        vec!["open"],
        "only work nobody has answered: {queued:?}"
    );
}

/// **THE OPERATOR'S ROWS ARE ON THE SAME BOARD, so the nag and the pane see them.** The
/// operator's ruling: *"the existing getter should return mine and yours, and the rest is also
/// the same. the only difference is who created and that is it."*
///
/// Two claims, and the second is why the halves are separate: the union is what `snapshot`
/// answers — which is what `unfinished_plan`, and therefore the idle nag, reads — and the `todo`
/// tool's own write, scoped to the model's rows, does not take the operator's rows with it.
#[test]
fn the_board_returns_the_operators_rows_alongside_the_models() {
    let b = TodoBoard::new(vec![TodoItem {
        content: "the model's".into(),
        status: TodoStatus::Pending,
        by: TodoBy::Model,
        when: None,
        needs: Vec::new(),
    }]);
    assert_eq!(b.snapshot().len(), 1, "the model's list as given");

    b.set_operator(vec![TodoItem {
        content: "the operator's".into(),
        status: TodoStatus::Pending,
        by: TodoBy::Operator,
        when: None,
        needs: Vec::new(),
    }]);

    let all = b.snapshot();
    assert_eq!(all.len(), 2, "**the getter returns BOTH**: {all:?}");
    assert!(
        all.iter().any(|t| t.by == TodoBy::Operator),
        "and says who wrote each"
    );
    // **`unfinished_plan` — what the nag asks — sees the operator's row with no change at all**
    assert!(
        nag_for(&all).is_some(),
        "so the reminder can fire for work the OPERATOR queued"
    );

    // **AND THE MODEL'S OWN WRITE DOES NOT DELETE THEM.** `upsert_model` is the `todo` tool's
    // own write, and it is why the two halves are kept apart rather than concatenated.
    b.upsert_model(&[("the model's".into(), TodoStatus::Completed, Vec::new())]);
    let after = b.snapshot();
    assert_eq!(after.len(), 2, "the operator's row survived: {after:?}");
    assert_eq!(
        after[0].status,
        TodoStatus::Completed,
        "and the model's own row moved"
    );
    assert!(after.iter().any(|t| t.content == "the operator's"));
    // **A RE-WORDED ROW IS A NEW ROW** — the text is the name — so the old row stays as well:
    // there is no write on this board that removes one.
    b.upsert_model(&[(
        "the model's, revised".into(),
        TodoStatus::Pending,
        Vec::new(),
    )]);
    let after = b.snapshot();
    assert_eq!(after.len(), 3, "nothing was removed: {after:?}");
    assert!(after.iter().any(|t| t.content == "the model's"));
}

/// **The union is TWO BLOCKS, not an interleaving** — and with hierarchy coming, that is the
/// fact the whole shape turns on.
///
/// `snapshot` is a clone-then-`extend`: every model row precedes every operator row, always,
/// because nothing merges the two vectors. So the halves cannot interleave by depth, which means
/// **"my own write" stays a well-defined edit**: the model's write cannot move, delete or
/// re-parent a row of the operator's, and the operator's half is a suffix of the union rather
/// than a scatter through it.
///
/// # And the hazard that comes with it, which is why this is asserted and not assumed
///
/// Depth is POSITIONAL — that is what makes it markdown's model and rano's, and it is the right
/// choice. But positional depth over a concatenation means a row's apparent parent is *the row
/// before it in the union*, and at the seam that row belongs to somebody else. An operator row at
/// depth 1 sitting after a model row at depth 0 renders as a child of it, and **the model's next
/// write can change that parent without touching a single operator row**: a write that re-words
/// or adds a row changes the last model row, and the operator's subtree is silently re-hung
/// under whatever took its place. That is the same failure as a flattened subtree — data that reads as a statement
/// nobody made — one level up, and it is silent.
///
/// **The rule that removes it is one line and it is asserted here**: each half is its own tree,
/// so the first row of each BLOCK is depth 0. The seam is a boundary, not a parent. When depth
/// lands, a writer that indents the first row of a half must be refused where the board is built
/// rather than coped with where it is drawn — see the operator's own instruction on the shape:
/// *"the depth invariant enforced where the board is built, not where it is drawn. A renderer
/// that copes with a gap hides a writer that made one."*
///
/// Today there is no depth, so the assertion is the shape it will have to satisfy: the blocks are
/// contiguous and their order is fixed.
#[test]
fn the_union_is_two_contiguous_blocks_so_a_half_is_a_suffix_not_a_scatter() {
    let b = TodoBoard::new(vec![
        TodoItem {
            content: "m1".into(),
            status: TodoStatus::Pending,
            by: TodoBy::Model,
            when: None,
            needs: Vec::new(),
        },
        TodoItem {
            content: "m2".into(),
            status: TodoStatus::Pending,
            by: TodoBy::Model,
            when: None,
            needs: Vec::new(),
        },
    ]);
    b.set_operator(vec![
        TodoItem {
            content: "o1".into(),
            status: TodoStatus::Pending,
            by: TodoBy::Operator,
            when: None,
            needs: Vec::new(),
        },
        TodoItem {
            content: "o2".into(),
            status: TodoStatus::Pending,
            by: TodoBy::Operator,
            when: None,
            needs: Vec::new(),
        },
    ]);

    let all = b.snapshot();
    let authors: Vec<TodoBy> = all.iter().map(|t| t.by.clone()).collect();
    assert_eq!(
        authors,
        vec![
            TodoBy::Model,
            TodoBy::Model,
            TodoBy::Operator,
            TodoBy::Operator
        ],
        "the union interleaved the halves, so the model's write is no longer a half-edit: {all:?}"
    );

    // **And the model's own write cannot reorder what it does not own.** It restates one of its
    // rows and adds another; the operator's block is still the tail, in the same order, with the
    // same contents — and the row it did not mention is still there, because a row left out is
    // not a row removed.
    b.upsert_model(&[
        ("m1".into(), TodoStatus::InProgress, Vec::new()),
        ("m3".into(), TodoStatus::Pending, Vec::new()),
    ]);
    let after = b.snapshot();
    assert_eq!(
        after.iter().map(|t| t.content.as_str()).collect::<Vec<_>>(),
        vec!["m1", "m2", "m3", "o1", "o2"],
        "the model's write moved the operator's rows: {after:?}"
    );
    assert_eq!(
        after.iter().filter(|t| t.by == TodoBy::Operator).count(),
        2,
        "and none of them went with it"
    );
}

/// **THE MODEL CAN DISPOSE OF THE OPERATOR'S ROW, which is what `TodoBy::Operator`'s doc
/// claimed and the code did not do.** *"the model can mark the operator's item done, and the
/// nag in `harness.rs` picks it up like any other"* — and it could not: `todo_write` was scoped
/// to the model's half, `set_operator` is the head's frame, so a row the operator wrote was
/// nagged about every idle turn and could never be answered. R48's wedge, from the other side.
///
/// The words are the name — there is no id on the wire, and the operator ruled out a bump — so
/// this also pins what happens when the name does not fit: **refused, with the candidates
/// named**, and NOTHING written.
/// **THE CALL THAT MADE A DUPLICATE ROW IN THE OPERATOR'S PANE, byte for byte from the
/// transcript of a real session.**
///
/// The model put `operator` one level too deep — inside a `todos` entry instead of beside it.
/// Every field this function read was valid, so the call SUCCEEDED: the entry landed in the
/// model's own half and the operator's row was never touched. The model then spent two more calls
/// and a paragraph of its reply on why the row would not close — *"the mark didn't take, and I
/// can't make it"* — while the operator watched a second row appear where they expected their own
/// to change. **Ignoring a field is what made a wrong write silent**, which is the defect this
/// refuses.
#[test]
fn an_operator_block_inside_a_todos_entry_is_refused_by_name() {
    let (mut rt, board) = runtime();
    board.set_operator(vec![TodoItem {
        content: "plain quoting".into(),
        status: TodoStatus::Pending,
        by: TodoBy::Operator,
        when: None,
        needs: Vec::new(),
    }]);
    let mut sink = RecordingToolSink::new();

    let r = rt.invoke(
        "t1",
        &call(
            r#"{"todos": [{"content": "plain quoting — marked complete; origin unrecoverable",
                                "operator": "mark that todo item as complete. no idea where it came from",
                                "status": "completed"}]}"#,
        ),
        &mut sink,
    );

    assert!(
        matches!(r.outcome, ToolOutcome::Failed { .. }),
        "**the call is refused, not half-obeyed**: {:?}",
        r.outcome
    );
    // **THE REFUSAL NAMES THE FIELD AND SAYS WHERE IT BELONGS** — a message that only said
    // "unknown field" would leave the model to guess, and it already guessed once.
    let said = format!("{} {:?}", r.payload, r.outcome);
    assert!(
        said.contains("operator"),
        "the refusal names the field it did not understand: {said}"
    );
    assert!(
        said.contains("TOP-LEVEL"),
        "**and says where it belongs**, because one level too deep is exactly the mistake: {said}"
    );
    // **AND NOTHING MOVED** — not their row, and not the model's list either, which is the
    // half-applied call the operator-rows-before-todos ordering exists to prevent.
    assert!(
        board
            .operator_snapshot()
            .iter()
            .all(|t| t.status == TodoStatus::Pending),
        "the operator's row is untouched: {:?}",
        board.operator_snapshot()
    );
    assert!(
        !board
            .snapshot()
            .iter()
            .any(|t| t.content.starts_with("plain quoting —")),
        "**and no duplicate row of the model's either** — which is what the operator saw: {:?}",
        board.snapshot()
    );
}

/// And the SAME argument, sent where it belongs, still works — the refusal is about the shape,
/// not about the model touching the operator's rows at all.
#[test]
fn an_unknown_field_other_than_operator_is_refused_too() {
    let (mut rt, board) = runtime();
    board.set_operator(vec![TodoItem {
        content: "a row".into(),
        status: TodoStatus::Pending,
        by: TodoBy::Operator,
        when: None,
        needs: Vec::new(),
    }]);
    let mut sink = RecordingToolSink::new();
    let r = rt.invoke(
        "t1",
        &call(r#"{"todos": [{"content": "a step", "status": "pending", "depth": 2}]}"#),
        &mut sink,
    );
    assert!(
        matches!(r.outcome, ToolOutcome::Failed { .. }),
        "a field the schema does not name is refused rather than ignored: {:?}",
        r.outcome
    );
    assert!(
        format!("{} {:?}", r.payload, r.outcome).contains("depth"),
        "and it is named"
    );
    // `snapshot()` is the UNION of both halves, so the operator's own row is legitimately in
    // it; what must not be there is anything of the MODEL's.
    assert!(
        board.snapshot().iter().all(|t| t.by == TodoBy::Operator),
        "nothing of the model's was written: {:?}",
        board.snapshot()
    );
}

#[test]
fn a_row_the_operator_wrote_is_moved_by_its_own_words() {
    let (mut rt, board) = runtime();
    board.set_operator(vec![
        TodoItem {
            content: "restart the daemon".into(),
            status: TodoStatus::Pending,
            by: TodoBy::Operator,
            when: None,
            needs: Vec::new(),
        },
        TodoItem {
            content: "push leticl".into(),
            status: TodoStatus::Pending,
            by: TodoBy::Operator,
            when: None,
            needs: Vec::new(),
        },
    ]);
    let mut sink = RecordingToolSink::new();

    // **the model names it by its exact words**, alongside its own list
    let r = rt.invoke(
        "t1",
        &call(
            r#"{"todos": [{"content": "my own step", "status": "in_progress"}],
                    "operator": [{"content": "restart the daemon", "status": "completed"}]}"#,
        ),
        &mut sink,
    );
    assert!(
        !matches!(r.outcome, ToolOutcome::Failed { .. }),
        "the move is not a failure: {:?}",
        r.outcome
    );
    let done = board.operator_snapshot();
    assert_eq!(
        done[0].status,
        TodoStatus::Completed,
        "**the operator's row moved**"
    );
    assert_eq!(
        done[1].status,
        TodoStatus::Pending,
        "and only the one that was named"
    );
    assert!(
        board.snapshot().iter().any(|t| t.content == "my own step"),
        "and the model's own list was written in the same call"
    );
    // and the model is told what happened to their row, and can see WHO wrote what
    let told = r.payload.clone();
    assert!(
        told.contains("— the operator's"),
        "the reply says whose each row is: {told}"
    );
    assert!(told.contains("— yours"), "{told}");

    // **A NAME THAT FITS NOTHING IS REFUSED AND WRITES NOTHING** — not their row, and not the
    // model's own list either, which is the half-applied call this ordering exists to prevent.
    let before = board.snapshot();
    let r = rt.invoke(
        "t2",
        &call(
            r#"{"todos": [{"content": "a plan that must not land", "status": "pending"}],
                    "operator": [{"content": "restart the daemon please", "status": "completed"}]}"#,
        ),
        &mut sink,
    );
    assert!(
        matches!(r.outcome, ToolOutcome::Failed { .. }),
        "a quote that fits nothing is not a write: {:?}",
        r.outcome
    );
    // BOTH halves, because both are shown to the model: `transcript_source.rs` renders a failed
    // result as `failed: {reason}` and then the payload under it.
    let told = format!("{:?}\n{}", r.outcome, r.payload);
    assert!(
        told.contains("restart the daemon") && told.contains("push leticl"),
        "**the refusal names the rows it DOES have**, so the model can quote one of them: {told}"
    );
    assert_eq!(
        board.snapshot(),
        before,
        "**AND NOTHING WAS WRITTEN** — the model's plan is untouched too"
    );

    // **TWO ROWS WITH THE SAME WORDS IS REFUSED RATHER THAN GUESSED.** The row that changes
    // state must not be the one the tool happened to find first.
    board.set_operator(vec![
        TodoItem {
            content: "same words".into(),
            status: TodoStatus::Pending,
            by: TodoBy::Operator,
            when: None,
            needs: Vec::new(),
        },
        TodoItem {
            content: "same words".into(),
            status: TodoStatus::Pending,
            by: TodoBy::Operator,
            when: None,
            needs: Vec::new(),
        },
    ]);
    let r = rt.invoke(
        "t3",
        &call(r#"{"todos": [], "operator": [{"content": "same words", "status": "completed"}]}"#),
        &mut sink,
    );
    assert!(
        matches!(r.outcome, ToolOutcome::Failed { .. }),
        "an ambiguous name is refused: {:?}",
        r.outcome
    );
    assert!(
        board
            .operator_snapshot()
            .iter()
            .all(|t| t.status == TodoStatus::Pending),
        "and neither of the two moved: {:?}",
        board.operator_snapshot()
    );
}

/// **The version is what the harness watches**, so it decides whether the wire and the store
/// hear about a move at all: a real change announces, and a no-op does not — otherwise every
/// call that re-states a status already set would publish the whole list again.
#[test]
fn moving_a_row_bumps_the_version_once_and_a_no_op_does_not() {
    let b = TodoBoard::new(vec![]);
    b.set_operator(vec![TodoItem {
        content: "the operator's".into(),
        status: TodoStatus::Pending,
        by: TodoBy::Operator,
        when: None,
        needs: Vec::new(),
    }]);
    let v = b.version();

    let moved = b
        .set_operator_states(&[("the operator's".into(), TodoStatus::Completed)])
        .expect("the words are there");
    assert_eq!(moved, 1, "one row changed");
    assert_eq!(
        b.version(),
        v + 1,
        "and that is ONE version, so one announcement"
    );

    let again = b
        .set_operator_states(&[("the operator's".into(), TodoStatus::Completed)])
        .expect("resolves");
    assert_eq!(again, 0, "nothing changed");
    assert_eq!(
        b.version(),
        v + 1,
        "so nothing is announced: {:?}",
        b.version()
    );
}

/// **AND THE NAG SAYS WHOSE ROW IT IS, because that is what chooses the mechanism.** A model
/// told only *one item is not done* has to guess whether to rewrite its own list or quote the
/// operator's row, and the guess it makes is the one it always makes — its own list.
#[test]
fn the_nag_says_whose_row_it_is_and_how_to_dispose_of_it() {
    let mine = nag_for(&[item("my step", TodoStatus::Pending)]).unwrap();
    assert!(mine.contains("— yours"), "{mine}");
    assert!(
        mine.contains("mark it done with `todo_write`'s `update` field"),
        "**the model's own row is retired by MARKING it**, and the field is named: {mine}"
    );
    assert!(
        !mine.contains("drop it"),
        "a row left out of `todo_write` STAYS on the board, so *drop it* is gone: {mine}"
    );

    let theirs = nag_for(&[TodoItem {
        content: "restart the daemon".into(),
        status: TodoStatus::Pending,
        by: TodoBy::Operator,
        when: None,
        needs: Vec::new(),
    }])
    .unwrap();
    assert!(theirs.contains("— the operator's"), "{theirs}");
    assert!(
        theirs.contains("`todo_write`'s `operator` field"),
        "**and names the mechanism**, because a model that has to guess one guesses wrong: {theirs}"
    );
    assert!(
        !theirs.contains("drop it"),
        "while *drop* is not offered for a row that is not the model's to remove: {theirs}"
    );
    assert!(theirs.contains("say why in your reply"), "{theirs}");
}

/// **THE FIRING IS RECORDED ON THE ROW, not in a daemon's memory.** The operator's own choice:
/// the board is the record, so a fired row does not fire again, and a daemon that comes back
/// re-reads it as having no condition rather than as one that is due.
///
/// What moves is the condition and nothing else — which is what makes a fired row ordinary open
/// work rather than a closed one, and what keeps the intent on the screen after the job it was
/// waiting for is gone.
#[test]
fn a_fired_condition_is_cleared_from_the_row_and_nothing_else_moves() {
    let b = TodoBoard::new(vec![]);
    b.set_operator(vec![
        TodoItem {
            content: "push once CI lands".into(),
            status: TodoStatus::Pending,
            by: TodoBy::Operator,
            when: Some(TodoCondition::Job {
                handle: "j121".into(),
            }),
            needs: Vec::new(),
        },
        TodoItem {
            content: "an ordinary row".into(),
            status: TodoStatus::Pending,
            by: TodoBy::Operator,
            when: None,
            needs: Vec::new(),
        },
    ]);
    let v = b.version();
    // **A handle nobody is waiting on consumes nothing, and must not announce anything.** A
    // version bump is a store write and a publish to every head, so a no-op that bumped would
    // republish the board on every wake.
    assert_eq!(b.consume_conditions(&["j999".to_string()]), 0);
    assert_eq!(b.version(), v, "nothing changed, so nothing is announced");
    // The one that was due.
    assert_eq!(b.consume_conditions(&["j121".to_string()]), 1);
    assert_eq!(b.version(), v + 1, "and that is ONE announcement");
    let rows = b.operator_snapshot();
    assert!(
        rows[0].when.is_none(),
        "the condition is consumed: {rows:?}"
    );
    assert_eq!(
        rows[0].content, "push once CI lands",
        "and the intent is left standing, which is what the reader acts on"
    );
    assert_eq!(
        rows[0].status,
        TodoStatus::Pending,
        "the status is not the firing's business — the row is open work now"
    );
    assert!(
        rows[1].when.is_none(),
        "a row that never had a condition is untouched"
    );
    assert_eq!(b.operator_snapshot().len(), 2, "and no row was dropped");
}

/// **A RESUMED BOARD COMES BACK SPLIT, and it is the store's own list that has to be.** The
/// persisted list is the UNION, so a `new` that put all of it in the model's half gave a resumed
/// session the operator's rows as its own: the model's write would have taken them for its own,
/// and once the head re-pushed its own half on hello the union held each of them twice.
#[test]
fn the_stores_union_is_split_back_into_its_two_halves() {
    let stored = vec![
        TodoItem {
            content: "the model's, from the store".into(),
            status: TodoStatus::InProgress,
            by: TodoBy::Model,
            when: None,
            needs: Vec::new(),
        },
        TodoItem {
            content: "the operator's, from the store".into(),
            status: TodoStatus::Pending,
            by: TodoBy::Operator,
            when: None,
            needs: Vec::new(),
        },
    ];
    let b = TodoBoard::new(stored);
    assert_eq!(
        b.snapshot().len(),
        2,
        "the union is the same two rows: {:?}",
        b.snapshot()
    );
    let theirs = b.operator_snapshot();
    assert_eq!(
        theirs.len(),
        1,
        "**and the operator's half is the operator's row**"
    );
    assert_eq!(theirs[0].content, "the operator's, from the store");

    // the model's own write cannot take their row with it — the whole reason the halves are apart
    b.upsert_model(&[("a fresh plan".into(), TodoStatus::Pending, Vec::new())]);
    assert_eq!(
        b.snapshot().len(),
        3,
        "and it survives the model's next write: {:?}",
        b.snapshot()
    );
    assert!(
        b.snapshot()
            .iter()
            .any(|t| t.content == "the operator's, from the store"),
        "the operator's row is still on the board: {:?}",
        b.snapshot()
    );
    // and it came back with a status the MODEL can still move, which is the point of the split
    assert_eq!(
        b.set_operator_states(&[(
            "the operator's, from the store".into(),
            TodoStatus::Completed
        )])
        .expect("resolves"),
        1
    );
}

/// **THE WHOLE LIST IS AN UPSERT, and a row left out of it STAYS on the board.**
///
/// The operator's rule, verbatim: *"regarding todo - only i should be able to delete todo items.
/// as a rule everything that ever created stays in history"* and *"so done items or canceled
/// items should be kept."* So the second write below — ONE entry, where the first wrote three —
/// leaves all three on the board: the named row moves, the two it did not name are exactly where
/// they were, and the reply SAYS so. That sentence is for the model whose frozen prefix still
/// carries the old contract (*"omitting an entry removes it"*), and it is printed at the one call
/// that would have meant a removal.
#[test]
fn the_whole_list_upserts_and_a_row_left_out_stays_on_the_board() {
    let (mut rt, board) = runtime();
    let mut sink = RecordingToolSink::new();
    let r = rt.invoke(
        "t1",
        &call(
            r#"{"todos": [
                    {"content": "read the harness", "status": "completed"},
                    {"content": "seat the tool", "status": "in_progress"},
                    {"content": "render the pane", "status": "pending"}
                ]}"#,
        ),
        &mut sink,
    );
    assert_eq!(r.outcome, ToolOutcome::Ok, "{}", r.payload);
    assert!(r.payload.contains("3)"), "{}", r.payload);
    assert!(r.payload.contains("[x] read the harness"), "{}", r.payload);
    assert!(r.payload.contains("[~] seat the tool"), "{}", r.payload);
    // The board holds what was written, in order.
    let snap = board.snapshot();
    assert_eq!(snap.len(), 3);
    assert_eq!(snap[0].status, TodoStatus::Completed);
    assert_eq!(snap[2].content, "render the pane");
    assert!(
        !r.payload.contains("left out of this call"),
        "the first write left nothing out: {}",
        r.payload
    );

    // **THREE ENTRIES IN, ONE ENTRY SENT, THREE STILL ON THE BOARD.** The named row moves; the
    // two it does not name are untouched.
    let r2 = rt.invoke(
        "t1",
        &call(r#"{"todos": [{"content": "render the pane", "status": "in_progress"}]}"#),
        &mut sink,
    );
    assert_eq!(r2.outcome, ToolOutcome::Ok, "{}", r2.payload);
    let after = board.snapshot();
    assert_eq!(
        after.len(),
        3,
        "a row left out of the list is NOT a deletion: {after:?}"
    );
    assert_eq!(after[0].content, "read the harness");
    assert_eq!(
        after[0].status,
        TodoStatus::Completed,
        "and its state is its own"
    );
    assert_eq!(after[1].content, "seat the tool");
    assert_eq!(after[1].status, TodoStatus::InProgress);
    assert_eq!(
        after[2].status,
        TodoStatus::InProgress,
        "and the named row moved"
    );
    assert!(
        r2.payload.contains("left out of this call")
            && r2.payload.contains("read the harness")
            && r2.payload.contains("seat the tool"),
        "**the reply names the rows it did NOT remove**: {}",
        r2.payload
    );
    // And the version moved twice, which is what the harness flush reads — once per call that
    // changed something.
    assert_eq!(board.version(), 2);
}

#[test]
fn a_missing_list_a_bad_status_and_an_empty_entry_all_refuse_by_name() {
    let (mut rt, board) = runtime();
    let mut sink = RecordingToolSink::new();
    let r = rt.invoke("t1", &call("{}"), &mut sink);
    assert!(
        matches!(r.outcome, ToolOutcome::Failed { .. }),
        "{:?}",
        r.outcome
    );
    assert!(r.payload.contains("todos"), "{}", r.payload);

    let r = rt.invoke(
        "t1",
        &call(r#"{"todos": [{"content": "x", "status": "done"}]}"#),
        &mut sink,
    );
    assert!(
        r.payload.contains("pending, in_progress, completed"),
        "{}",
        r.payload
    );

    let r = rt.invoke(
        "t1",
        &call(r#"{"todos": [{"content": "  ", "status": "pending"}]}"#),
        &mut sink,
    );
    assert!(r.payload.contains("empty"), "{}", r.payload);
    // Nothing was written by any refusal.
    assert_eq!(board.version(), 0);
    assert!(board.snapshot().is_empty());
}
