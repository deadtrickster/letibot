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

use super::*;
use crate::backend::HostBackend;
use crate::backend::tempdir::TempDir;
use crate::events::RecordingToolSink;
use crate::runtime::{Registry, ToolRuntime};
use letibot_transcript::{ToolCall, ToolOutcome};

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

/// The tool behind a real runtime, because the call goes through the gate on
/// its way past — and `Access::Session` must pass it unattended, which this
/// doubles as a check of.
fn runtime() -> (ToolRuntime, Arc<TodoBoard>) {
    runtime_with(Vec::new())
}

/// The same runtime, on a board that already holds rows — the shape a RESUMED session gets from
/// its store, and the only way two rows can share a name (this tool never writes a second row
/// under a name the half already holds).
fn runtime_with(initial: Vec<TodoItem>) -> (ToolRuntime, Arc<TodoBoard>) {
    let board = Arc::new(TodoBoard::new(initial));
    let mut reg = Registry::new();
    reg.register(Box::new(TodoWriteTool::new(board.clone())))
        .unwrap();
    let d = TempDir::new();
    let backend = HostBackend::new(d.path()).unwrap();
    // The temp dir outlives the backend only within one test; leak it there
    // rather than complicate every caller.
    std::mem::forget(d);
    (ToolRuntime::new(reg, Box::new(backend)), board)
}

fn call(args: &str) -> ToolCall {
    ToolCall {
        id: "c0".into(),
        name: "todo_write".into(),
        arguments: args.into(),
    }
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

// -- the turn boundary ------------------------------------------------

/// **The check's sentence for a plan that IS the whole board** — the shorthand every test below
/// uses, and the honest one: these plans carry no POSTPONED row, so the caller's narrowing
/// (`the_plan_as_checked`) would hand back exactly the same rows, and no row of theirs waits on
/// a child or on a job, so `ChildSessions::none()` and `RunningJobs::none()` are the reading the
/// daemon would hand in for them. A test that needs the two to differ says so by calling
/// `unfinished_plan` with both — or by passing the children or the jobs it means, as the child
/// and job tests below do.
fn nag_for(todos: &[TodoItem]) -> Option<String> {
    nag_with(todos, &ChildSessions::none(), &no_jobs())
}

/// [`nag_for`] with the daemon's own reading of the children a plan waits on.
fn nag_with(todos: &[TodoItem], children: &ChildSessions, jobs: &RunningJobs) -> Option<String> {
    unfinished_plan(todos, todos, children, jobs)
}

/// **Nothing running outside the board** — no job this session holds, which is the reading every
/// test here that is not about a job passes, and the one a daemon with no process host hands in.
fn no_jobs() -> RunningJobs {
    RunningJobs::none()
}

/// **A child this daemon is running**, as a test states one: an age, and what it last showed.
fn child(age_secs: u64, quiet_secs: Option<u64>, working: bool) -> RunningChild {
    RunningChild {
        age: Duration::from_secs(age_secs),
        quiet_for: quiet_secs.map(Duration::from_secs),
        working,
    }
}

/// **A row that waits on a CHILD** — the dispatch case, as these tests write one.
fn waiting_on_a_child(content: &str, id: &str) -> TodoItem {
    TodoItem {
        needs: vec![TodoNeed::Child { id: id.to_string() }],
        ..item(content, TodoStatus::Pending)
    }
}

/// **A row that waits on a JOB** — the case this feature was asked for: work that cannot start
/// until something the daemon is running has ended.
fn waiting_on_a_job(content: &str, handle: &str) -> TodoItem {
    TodoItem {
        needs: vec![TodoNeed::Job {
            handle: handle.to_string(),
        }],
        ..item(content, TodoStatus::Pending)
    }
}

fn item(content: &str, status: TodoStatus) -> TodoItem {
    TodoItem {
        content: content.into(),
        status,
        by: TodoBy::Model,
        when: None,
        needs: Vec::new(),
    }
}

/// **The trigger is pending work and nothing else.** The obvious way to get this
/// wrong is a message that fires whenever a turn ends, which teaches the model to
/// clear its todos to make it stop — worse than no check at all. So the silence cases
/// are asserted first and with the same weight as the firing one.
#[test]
fn a_plan_with_nothing_open_has_nothing_to_say() {
    assert!(nag_for(&[]).is_none(), "no plan at all");
    assert!(
        nag_for(&[
            item("one", TodoStatus::Completed),
            item("two", TodoStatus::Completed),
        ])
        .is_none(),
        "a finished plan is not a finding, it is the answer"
    );
}

/// **THE CHOICE IS STICKY, and this is the test for it.** The check names a row once; a row
/// added elsewhere on the plan must not change which row the agent is being held to. Without
/// the choice, `open_priority`'s head would move to the new row and the question would wander
/// with it.
#[test]
fn the_check_keeps_asking_about_the_row_it_named() {
    let plan = vec![
        item("first", TodoStatus::Pending),
        item("second", TodoStatus::Pending),
    ];
    let (text, named) = unfinished_plan_for(&plan, &plan, None, &ChildSessions::none(), &no_jobs())
        .expect("open work");
    assert_eq!(named, "first", "the queue's head is named first");
    assert!(text.contains("first"), "{text}");

    // **A row arrives AHEAD of it**, which is the ordinary case: the operator's half of the
    // board is theirs to rewrite, and the model rewrites its own list at any time.
    let grown = vec![
        item("something else", TodoStatus::Pending),
        item("first", TodoStatus::Pending),
        item("second", TodoStatus::Pending),
    ];
    let (text, named) = unfinished_plan_for(
        &grown,
        &grown,
        Some(&named),
        &ChildSessions::none(),
        &no_jobs(),
    )
    .expect("open work");
    assert_eq!(
        named, "first",
        "the row already named is the row asked about"
    );
    assert!(
        text.contains("first") && !text.contains("something else"),
        "the question did not wander: {text}"
    );
    // And the count is still the plan's, so the model learns the plan is bigger than the row.
    assert!(text.contains("(2 more open)"), "{text}");
}

/// **The choice moves on when the named row leaves the queue** — done, or set aside by the
/// operator — and then the head of the queue is the new choice. That is the difference between
/// a sticky choice and a sticky QUESTION: the row is held only while it is still askable.
#[test]
fn the_choice_moves_on_when_the_named_row_leaves_the_queue() {
    let plan = vec![
        item("first", TodoStatus::Pending),
        item("second", TodoStatus::Pending),
    ];
    let (_, named) = unfinished_plan_for(&plan, &plan, None, &ChildSessions::none(), &no_jobs())
        .expect("open work");
    assert_eq!(named, "first");

    let first_done = vec![
        item("first", TodoStatus::Completed),
        item("second", TodoStatus::Pending),
    ];
    let (text, named) = unfinished_plan_for(
        &first_done,
        &first_done,
        Some("first"),
        &ChildSessions::none(),
        &no_jobs(),
    )
    .expect("open work");
    assert_eq!(named, "second", "the answered row is not a choice any more");
    assert!(text.contains("second"), "{text}");

    // The same for a row the operator set aside, seen from the one place the narrowing
    // happens: `the_plan_as_checked` is the CALLER's filter (a postponed row is not work this
    // check may speak about), so by the time this function is handed the plan the row the last
    // check named is simply not in it and the head of the queue is the new choice.
    let after_a_postponement = vec![item("second", TodoStatus::Pending)];
    let (_, named) = unfinished_plan_for(
        &after_a_postponement,
        &after_a_postponement,
        Some("first"),
        &ChildSessions::none(),
        &no_jobs(),
    )
    .expect("open work");
    assert_eq!(named, "second");

    // And with nothing askable left there is no sentence at all.
    assert!(
        unfinished_plan_for(
            &[item("first", TodoStatus::Completed)],
            &[item("first", TodoStatus::Completed)],
            Some("first"),
            &ChildSessions::none(),
            &no_jobs(),
        )
        .is_none(),
        "a finished plan is silence, whatever the last check named"
    );
}

/// **A row the model marked in progress is asked about in its own words** — the operator:
/// *"deliberate in progress so nag can be specific … and then it keeps naggin while it is in
/// progress."* The two sentences are different questions: one is *you were asked and have not
/// started*, the other is *you claimed this and it is still open*.
#[test]
fn a_row_marked_in_progress_is_asked_about_as_its_own_claim() {
    let plan = vec![item("the migration", TodoStatus::InProgress)];
    let (fresh, _) = unfinished_plan_for(&plan, &plan, None, &ChildSessions::none(), &no_jobs())
        .expect("open work");
    assert!(
        fresh.contains("you had it in progress"),
        "a row already in progress at the first check: {fresh}"
    );

    let (sticky, _) = unfinished_plan_for(
        &plan,
        &plan,
        Some("the migration"),
        &ChildSessions::none(),
        &no_jobs(),
    )
    .expect("open work");
    assert!(
        sticky.contains("marked in progress and still open"),
        "the model's own claim, said back to it: {sticky}"
    );
    assert_ne!(fresh, sticky, "the question changed with the mark");

    // And a PENDING row that was already asked about says so, rather than repeating the first
    // check's sentence verbatim — which is what makes the repeat legible as a repeat.
    let pending = vec![item("the migration", TodoStatus::Pending)];
    let (first, _) =
        unfinished_plan_for(&pending, &pending, None, &ChildSessions::none(), &no_jobs())
            .expect("open work");
    let (repeat, _) = unfinished_plan_for(
        &pending,
        &pending,
        Some("the migration"),
        &ChildSessions::none(),
        &no_jobs(),
    )
    .expect("open work");
    assert!(
        repeat.contains("asked about this one already"),
        "the repeat says it is a repeat: {repeat}"
    );
    assert_ne!(first, repeat);
}

/// **The queue's ORDER, asserted as an order.** The operator: *"priority queues - in progress
/// items than not done, one-by-one."*
///
/// Five items in, five out, and the two things that matter are both visible here: `in_progress`
/// before `pending` regardless of where each sits in the list, and list order kept WITHIN a band
/// (a stable sort — otherwise the operator's pending row could leapfrog the model's).
#[test]
fn the_open_work_is_queued_in_progress_first_then_not_done() {
    let todos = vec![
        item("model pending A", TodoStatus::Pending),
        item("model done", TodoStatus::Completed),
        item("model in progress", TodoStatus::InProgress),
        item("operator pending", TodoStatus::Pending),
        item("operator in progress", TodoStatus::InProgress),
    ];
    let queued: Vec<&str> = open_priority(&todos)
        .iter()
        .map(|t| t.content.as_str())
        .collect();
    assert_eq!(
        queued,
        vec![
            "model in progress",
            "operator in progress",
            "model pending A",
            "operator pending",
        ],
        "in_progress band first, then pending, list order kept inside each"
    );
    assert!(
        !queued.contains(&"model done"),
        "**a completed item is not in the queue at all**: {queued:?}"
    );
    // and the head of that queue is what the message names
    let msg = nag_for(&todos).expect("four open items is a finding");
    assert!(msg.contains("model in progress"), "{msg}");
    assert!(msg.contains("(3 more open)"), "{msg}");
}

/// **Every serve takes the next one**, which is what *one-by-one* means: deal with the head and
/// the queue's head moves on. This is the whole interaction the operator is describing, walked
/// one step at a time rather than asserted as a formula.
#[test]
fn serving_the_queue_one_by_one_walks_the_work() {
    let mut todos = vec![
        item("first", TodoStatus::Pending),
        item("second", TodoStatus::Pending),
        item("third", TodoStatus::Pending),
    ];
    for expected in ["first", "second", "third"] {
        let msg = nag_for(&todos).expect("open work");
        assert!(
            msg.contains(expected),
            "the queue serves {expected} next: {msg}"
        );
        // the model deals with it — the only way the queue advances, and the reason a model that
        // ignores the check is not nagged about the same row for ever
        let at = todos.iter().position(|t| t.content == expected).unwrap();
        todos[at].status = TodoStatus::Completed;
    }
    assert!(
        nag_for(&todos).is_none(),
        "and when the work runs out, the check is silent"
    );
}

/// **It names ONE item, and it names the RIGHT one.** The operator: *"i think the nagger
/// should mention only one todo at a time, so a model will not be defocused."*
///
/// A list invites a model to touch all of it — read five open rows, do a little of each, end the
/// next turn with five still open — which is the failure this check exists to prevent, arriving
/// on the check's own message. And WHICH one is not the first by accident: an item the model
/// marked `in_progress` is the one it told the board it was doing, so that is what gets named.
#[test]
fn an_open_plan_names_one_item_and_what_to_do_about_it() {
    let msg = nag_for(&[
        item("write it up", TodoStatus::Completed),
        item("wire the check", TodoStatus::InProgress),
        item("test it", TodoStatus::Pending),
    ])
    .expect("one in progress and one pending is work left open");
    // **the item it was WORKING ON**, not the first open row in the list
    assert!(
        msg.contains("wire the check"),
        "the in-progress item is the one named: {msg}"
    );
    assert!(
        msg.contains("yours, and you had it in progress"),
        "and the message says why this one, AND whose it is: {msg}"
    );
    // **and NOT the other open row** — that is the defocusing the operator asked me to stop
    assert!(
        !msg.contains("test it"),
        "the second open item is not named, because one is a directive and five is homework: {msg}"
    );
    assert!(
        !msg.contains("write it up"),
        "a completed item is not part of what is open: {msg}"
    );
    // the rest is a COUNT, which is the fact that must not become a list
    assert!(
        msg.contains("(1 more open)"),
        "the remainder is counted, not listed: {msg}"
    );
    // **all three honest answers are offered, and the third is the one that keeps this from
    // being a trap.** Without *say in your reply that you are dropping it* the model's only exit
    // is to lie about its own statuses, which is the failure this exists to stop — and now that
    // a row left out of `todo_write` STAYS on the board, saying so is the only way to drop one.
    assert!(msg.contains("do this one"), "{msg}");
    assert!(msg.contains("mark it done"), "{msg}");
    assert!(
        msg.contains("you are dropping is something you say in your reply"),
        "{msg}"
    );
    assert!(
        msg.contains("deliberately"),
        "and stopping on purpose is answerable: {msg}"
    );
    assert!(msg.starts_with("[todo check]"), "the house prefix: {msg}");
}

/// **With nothing started, it names the first open row** — the model has told the board nothing,
/// so the list's own order is the only thing to go on, and singling one out is still the rule.
#[test]
fn with_nothing_in_progress_it_names_the_first_open_row() {
    let msg = nag_for(&[
        item("the first", TodoStatus::Pending),
        item("the second", TodoStatus::Pending),
        item("the third", TodoStatus::Pending),
    ])
    .expect("three open items is a finding");
    assert!(msg.contains("the first"), "{msg}");
    assert!(!msg.contains("the second"), "one at a time: {msg}");
    assert!(!msg.contains("the third"), "one at a time: {msg}");
    assert!(msg.contains("(2 more open)"), "{msg}");
    assert!(
        !msg.contains("in progress"),
        "and it does not claim to know why this one: {msg}"
    );
}

/// **A long plan is the same message as a short one.** Ten open items produce ONE named row and a
/// count — the rule `intent`'s steering follows, and the reason `MAX_PLAN_LINES` is gone: there is
/// no longer a length of list to cap.
#[test]
fn a_long_plan_is_one_item_and_a_count() {
    let todos: Vec<TodoItem> = (0..10)
        .map(|i| item(&format!("item {i}"), TodoStatus::Pending))
        .collect();
    let msg = nag_for(&todos).expect("ten open items is a finding");
    assert!(msg.contains("item 0"), "the first is named: {msg}");
    assert!(!msg.contains("item 1"), "and no other: {msg}");
    assert!(msg.contains("(9 more open)"), "the rest is counted: {msg}");
}

/// **A list written and finished inside the turn says nothing**, which is what taking a
/// SNAPSHOT at the boundary buys: the check asks the state of the plan at the moment the
/// turn stopped, not what the model did with it on the way.
#[test]
fn a_plan_written_and_completed_says_nothing() {
    let board = TodoBoard::new(vec![]);
    board.upsert_model(&[("do the thing".into(), TodoStatus::InProgress, Vec::new())]);
    board.upsert_model(&[("do the thing".into(), TodoStatus::Completed, Vec::new())]);
    assert!(
        nag_for(&board.snapshot()).is_none(),
        "the plan as it STANDS is what is asked about"
    );
}

/// **A row that waits on the rows it names** — the DAG's edge, as these tests write one.
fn needs(content: &str, status: TodoStatus, on: &[&str]) -> TodoItem {
    TodoItem {
        needs: on
            .iter()
            .map(|c| TodoNeed::Row {
                content: (*c).to_string(),
            })
            .collect(),
        ..item(content, status)
    }
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
    assert_eq!(
        nag_for(&plan).expect("open work"),
        "[todo check] this turn is finished and one item is not done (1 more open):\n  - wire \
             the check — yours, and you had it in progress\ndo this one, or mark it done with \
             `todo_write`'s `update` field, quoting the text above exactly — a row left out of \
             `todo_write` STAYS on the board, so a line of work you are dropping is something you \
             say in your reply, not something a call does. If you are stopping here deliberately, \
             say why in your reply.",
        "the sentence a dependency-free board gets is the one `1d24fab` produced, byte for byte"
    );

    // The other three arms, one at a time, because the arm is what the field could have moved.
    assert_eq!(
        nag_for(&[item("first", TodoStatus::Pending)]).expect("open work"),
        "[todo check] this turn is finished and one item is not done:\n  - first — yours\ndo \
             this one, or mark it done with `todo_write`'s `update` field, quoting the text above \
             exactly — a row left out of `todo_write` STAYS on the board, so a line of work you \
             are dropping is something you say in your reply, not something a call does. If you \
             are stopping here deliberately, say why in your reply."
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
        "[todo check] this turn is finished and one item is not done (1 more open):"
    );
    assert!(
        nag_for(&[
            item("first", TodoStatus::Pending),
            item("second", TodoStatus::Pending)
        ])
        .expect("open work")
        .contains("  - first — yours\n"),
        "the first open row is named, and the repeat arm says so"
    );
    assert_eq!(
        nag_for(&[item("first", TodoStatus::InProgress)])
            .expect("open work")
            .lines()
            .nth(1),
        Some("  - first — yours, and you had it in progress")
    );
    let sticky = unfinished_plan_for(
        &[item("first", TodoStatus::Pending)],
        &[item("first", TodoStatus::Pending)],
        Some("first"),
        &ChildSessions::none(),
        &no_jobs(),
    )
    .expect("open work");
    assert_eq!(
        sticky.0.lines().nth(1),
        Some(
            "  - first — yours, and you were asked about this one already and it has not \
                 been started"
        )
    );

    // **AND THE STRUCTURE**: the graph split of a board with no edges is the identity.
    let graph = as_a_graph(&plan, &plan, &ChildSessions::none(), &no_jobs());
    assert_eq!(
        graph.open, 2,
        "the plan's size, minus the row that is done — a completed row is not work this check \
             may speak about"
    );
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
    assert!(
        msg.contains("(1 more open)"),
        "and a blocked row is still open work, so it is counted: {msg}"
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
    let msg = nag_with(&plan, &ChildSessions::none(), &running).expect("a stuck plan speaks");
    assert!(
        msg.contains("the job `j2070`, still running"),
        "and the line names the job: {msg}"
    );
    assert!(
        !msg.contains("check whether it is stuck"),
        "a job needs no steering — its settlement is already on its way: {msg}"
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
    assert_eq!(
        nag_for(&flat),
        nag_with(&flat, &ChildSessions::none(), &busy),
        "no needs at all"
    );
    assert_eq!(
        nag_for(&edged),
        nag_with(&edged, &ChildSessions::none(), &busy),
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

/// **A CALL THAT NAMES NO ROW IS REFUSED, and an empty list is one of those.**
///
/// `{"todos": []}` used to mean *clear the board*, and the operator has ruled that out: *"only i
/// should be able to delete todo items."* With no delete on this board an empty list says
/// nothing at all, so it is refused rather than read as one that did something — and the board
/// keeps every row it had.
#[test]
fn an_empty_list_is_refused_and_removes_nothing() {
    let (mut rt, board) = runtime();
    let mut sink = RecordingToolSink::new();
    rt.invoke(
        "t1",
        &call(r#"{"todos": [{"content": "one", "status": "pending"}]}"#),
        &mut sink,
    );
    assert_eq!(board.snapshot().len(), 1);
    let v = board.version();
    let r = rt.invoke("t1", &call(r#"{"todos": []}"#), &mut sink);
    assert!(
        matches!(r.outcome, ToolOutcome::Failed { .. }),
        "an empty list is not a write: {:?}",
        r.outcome
    );
    let told = format!("{:?}\n{}", r.outcome, r.payload);
    assert!(
        told.contains("names no rows"),
        "the refusal says what is wrong with it: {told}"
    );
    assert_eq!(
        board.snapshot().len(),
        1,
        "**and the row is still there** — nothing this tool sends removes one"
    );
    assert_eq!(board.version(), v, "and nothing was announced");
}

// -- ONE ROW, BY ITS OWN WORDS -----------------------------------------

/// **A QUOTE MARKS EXACTLY ONE ROW, and `update` does it without re-sending the list.**
///
/// The operator's ask: an `update` field that takes the row's exact content plus the new status,
/// because there was no way to say *this row, and only this one, is now done* without sending the
/// whole list again.
///
/// The assertion is a DIFF of the rendered board: one line moved, only the mark inside it, and
/// the line count is the same. A status change that also moved a row, added one or dropped one
/// would pass a count of `snapshot().len()` and fail here.
#[test]
fn a_quote_marks_exactly_one_row() {
    let (mut rt, board) = runtime();
    let mut sink = RecordingToolSink::new();
    rt.invoke(
        "t1",
        &call(
            r#"{"todos": [
                    {"content": "read the harness", "status": "pending"},
                    {"content": "seat the tool", "status": "pending"},
                    {"content": "render the pane", "status": "pending"}
                ]}"#,
        ),
        &mut sink,
    );
    let before = render(&board.snapshot());
    // **NO `todos` AT ALL.** One row, named by its own words — which is why `todos` is no longer
    // a required argument of this tool.
    let r = rt.invoke(
        "t1",
        &call(r#"{"update": [{"content": "seat the tool", "status": "completed"}]}"#),
        &mut sink,
    );
    assert_eq!(r.outcome, ToolOutcome::Ok, "{}", r.payload);
    let after = render(&board.snapshot());
    let moved: Vec<(&str, &str)> = before
        .lines()
        .zip(after.lines())
        .filter(|(b, a)| b != a)
        .collect();
    assert_eq!(
        moved.len(),
        1,
        "exactly one line moved:\n{before}\n---\n{after}"
    );
    assert!(moved[0].1.contains("[x] seat the tool"), "{:?}", moved[0]);
    assert_eq!(
        before.lines().count(),
        after.lines().count(),
        "and no line was added or removed:\n{after}"
    );
    // The status is the ONLY thing that moved, and it moved on the named row.
    let snap = board.snapshot();
    assert_eq!(snap[0].status, TodoStatus::Pending);
    assert_eq!(snap[1].status, TodoStatus::Completed);
    assert_eq!(snap[2].status, TodoStatus::Pending);
    assert!(
        r.payload
            .contains("of your own rows you named, 1 changed status"),
        "{}",
        r.payload
    );
    assert!(
        !r.payload.contains("left out of this call"),
        "no `todos` was sent, so nothing was left out of one: {}",
        r.payload
    );
    assert!(
        snap.iter().all(|t| t.needs.is_empty()),
        "and `update` wrote no edges: {snap:?}"
    );
}

/// **A QUOTE THAT FITS NO ROW IS REFUSED, and the refusal names the rows that half DOES hold.**
///
/// The house rule for a name — `set_states`' own doc — and what makes content-keying safe: the
/// model can quote one of the rows it was just shown instead of guessing again. **AND NOTHING IS
/// WRITTEN** — not the row, and not the `todos` beside it.
#[test]
fn a_quote_that_fits_no_row_is_refused_by_name() {
    let (mut rt, board) = runtime();
    let mut sink = RecordingToolSink::new();
    rt.invoke(
        "t1",
        &call(r#"{"todos": [{"content": "seat the tool", "status": "pending"}]}"#),
        &mut sink,
    );
    let before = board.snapshot();
    let v = board.version();
    let r = rt.invoke(
        "t1",
        &call(
            r#"{"todos": [{"content": "a plan that must not land", "status": "pending"}],
                    "update": [{"content": "seat the tool please", "status": "completed"}]}"#,
        ),
        &mut sink,
    );
    assert!(
        matches!(r.outcome, ToolOutcome::Failed { .. }),
        "a quote that fits nothing is not a write: {:?}",
        r.outcome
    );
    let told = format!("{:?}\n{}", r.outcome, r.payload);
    assert!(
        told.contains("no row of your own says `seat the tool please`"),
        "the refusal names the fact: {told}"
    );
    assert!(
        told.contains("`seat the tool`"),
        "**and the row it does have**, so the model can quote that instead: {told}"
    );
    assert_eq!(
        board.snapshot(),
        before,
        "**AND NOTHING WAS WRITTEN** — not the row, and not the list beside it"
    );
    assert_eq!(board.version(), v, "and nothing was announced");
}

/// **A QUOTE THAT FITS TWO ROWS IS REFUSED, and BOTH are named.**
///
/// Two rows answer to one quote when their text differs only in whitespace — the match trims —
/// which `todo_write` cannot produce (it upserts by trimmed text) and a store written by hand
/// can. So the board is built directly, through the same door a RESUMED session's store comes in
/// by. The guess this refuses to make is a row silently changing state under a model that quoted
/// something else.
#[test]
fn a_quote_that_fits_two_rows_is_refused_naming_both() {
    let (mut rt, board) = runtime_with(vec![
        TodoItem {
            content: "same words".into(),
            status: TodoStatus::Pending,
            by: TodoBy::Model,
            when: None,
            needs: Vec::new(),
        },
        TodoItem {
            content: "same words ".into(),
            status: TodoStatus::Pending,
            by: TodoBy::Model,
            when: None,
            needs: Vec::new(),
        },
    ]);
    let mut sink = RecordingToolSink::new();
    assert_eq!(board.snapshot().len(), 2, "both rows are on the board");
    let before = board.snapshot();
    let r = rt.invoke(
        "t1",
        &call(r#"{"update": [{"content": "same words", "status": "completed"}]}"#),
        &mut sink,
    );
    assert!(
        matches!(r.outcome, ToolOutcome::Failed { .. }),
        "an ambiguous name is refused: {:?}",
        r.outcome
    );
    let told = format!("{:?}\n{}", r.outcome, r.payload);
    assert!(told.contains("is 2 of your own rows"), "{told}");
    assert!(
        told.contains("row 1 `same words`") && told.contains("row 2 `same words `"),
        "**both candidates are named**, by their place and their exact words: {told}"
    );
    assert_eq!(board.snapshot(), before, "and neither of the two moved");
}

// -- a parent writing a child's board ---------------------------------

/// The fake daemon half: one child id, one board, a refusal it hands back verbatim —
/// everything the tool needs to be tested against, and nothing the real resolver does
/// that the tool relies on (the parentage check and the author stamp are the daemon's,
/// and are tested where the daemon is).
struct FakeChildren {
    board: Arc<TodoBoard>,
    refused: Mutex<Vec<String>>,
}

impl super::ChildTodos for FakeChildren {
    fn upsert_child(
        &self,
        _target: &str,
        rows: &[(String, TodoStatus, Vec<TodoNeed>)],
    ) -> Result<Vec<TodoItem>, String> {
        // The real resolver stamps the author itself; the fake does the same, with an id
        // shaped like a real one so the rendered reply can be asserted on it.
        let by = TodoBy::parent_of("s-1789462738453908838");
        // The board's own writer, so its title-cap refusal reaches the tool as the real
        // resolver's does: `?` and nothing else.
        self.board.upsert_parent(rows, &by)?;
        Ok(self.board.snapshot())
    }
}

fn children_runtime(board: Arc<TodoBoard>) -> (ToolRuntime, Arc<TodoBoard>, Arc<FakeChildren>) {
    let own = Arc::new(TodoBoard::new(vec![]));
    let fake = Arc::new(FakeChildren {
        board,
        refused: Mutex::new(Vec::new()),
    });
    let mut reg = Registry::new();
    reg.register(Box::new(super::TodoWriteTool::with_children(
        own.clone(),
        fake.clone(),
    )))
    .unwrap();
    let d = TempDir::new();
    let backend = HostBackend::new(d.path()).unwrap();
    std::mem::forget(d);
    (ToolRuntime::new(reg, Box::new(backend)), own, fake)
}

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
        nag.contains("your parent asked for this one"),
        "and the advice is the parent's, not the operator's and not the model's own: {nag}"
    );
    assert!(
        nag.contains("they retire the row themselves"),
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

/// **The description names the cap the code enforces.**
///
/// The number in the model-facing prose is a literal — a `format!` over the whole description
/// would mean escaping every `{"kind": …}` example in it — so this is the test that catches the
/// one drift a reader cannot see: a constant moved and the sentence left behind, which would
/// teach the model a cap the tool refuses at a different length.
#[test]
fn the_tools_description_names_the_cap_it_enforces() {
    let board = Arc::new(TodoBoard::new(Vec::new()));
    let schema = super::TodoWriteTool::new(board).schema();
    let cap = TODO_TITLE_MAX.to_string();
    assert!(
        schema
            .description
            .contains(&format!("a title is at most {cap}")),
        "the description does not name the cap: {}",
        schema.description
    );
    assert!(
        schema
            .description
            .contains("The rest of a row goes after ` — `"),
        "nor where the rest of a row goes: {}",
        schema.description
    );
    let content =
        schema.parameters["properties"]["todos"]["items"]["properties"]["content"]["description"]
            .as_str()
            .expect("the `todos` entry's `content` says what a content is");
    assert!(
        content.contains(&cap),
        "the field the model writes does not name the cap either: {content}"
    );
}

// -- the title cap -------------------------------------------------------

/// **A title over the cap is refused BY NAME, and nothing is written** — the refusal names the
/// row's own words, the number it measured and the cap, and the sentence says where the rest of
/// the row goes.
///
/// The cap itself is `TODO_TITLE_MAX`'s and its reason is written there: the operator's 160-cell
/// terminal, the pane's 156-column row, and the 21 columns of furniture a row spends before its
/// own words.
#[test]
fn a_title_over_the_cap_is_refused_by_name_and_nothing_is_written() {
    let (mut rt, board) = runtime();
    let mut sink = RecordingToolSink::new();
    let long = "x".repeat(TODO_TITLE_MAX + 1);
    let r = rt.invoke(
        "t1",
        &call(&serde_json::json!({"todos": [{"content": long, "status": "pending"}]}).to_string()),
        &mut sink,
    );
    assert!(
        matches!(r.outcome, ToolOutcome::Failed { .. }),
        "{:?}",
        r.outcome
    );
    let said = format!("{} {:?}", r.payload, r.outcome);
    assert!(
        said.contains(&format!("is {} characters", TODO_TITLE_MAX + 1)),
        "the length it measured is named: {said}"
    );
    assert!(
        said.contains(&format!("at most {TODO_TITLE_MAX}")),
        "and the cap: {said}"
    );
    assert!(
        said.contains("put it after ` — `"),
        "and where the rest of the row goes: {said}"
    );
    assert!(board.snapshot().is_empty(), "nothing was written: {said}");
}

/// **A title AT the cap is written whole** — the bound is a bound and not a shave, and the row
/// that passes is the row the model gets back.
#[test]
fn a_title_at_the_cap_is_written() {
    let (mut rt, board) = runtime();
    let mut sink = RecordingToolSink::new();
    let exact = "y".repeat(TODO_TITLE_MAX);
    let r = rt.invoke(
        "t1",
        &call(&serde_json::json!({"todos": [{"content": exact, "status": "pending"}]}).to_string()),
        &mut sink,
    );
    assert_eq!(r.outcome, ToolOutcome::Ok, "{} {:?}", r.payload, r.outcome);
    let all = board.snapshot();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].content, exact, "the words are kept verbatim");
}

/// **The cap is on the TITLE, and the detail after ` — ` is not capped.** A row whose title fits
/// is written whole, detail and all — that is the sentence the tool's description gives the
/// model for anything long, so it has to be the thing that actually passes.
#[test]
fn a_long_detail_after_the_separator_is_not_the_title() {
    let (mut rt, board) = runtime();
    let mut sink = RecordingToolSink::new();
    let content = format!("fix the flaky test — {}", "d".repeat(TODO_TITLE_MAX * 3));
    let r = rt.invoke(
        "t1",
        &call(
            &serde_json::json!({"todos": [{"content": content, "status": "pending"}]}).to_string(),
        ),
        &mut sink,
    );
    assert_eq!(r.outcome, ToolOutcome::Ok, "{} {:?}", r.payload, r.outcome);
    let all = board.snapshot();
    assert_eq!(all.len(), 1);
    assert_eq!(
        all[0].content, content,
        "the detail is kept, and the title is what was measured"
    );
}

/// **A row written BEFORE the cap is not made immortal by it.** The cap binds the words a call
/// WRITES, so an old long row can still be restated by the whole-list form and still be moved by
/// quoting it in `update` — which is what keeps a board that predates the cap disposable rather
/// than stuck with a row nobody can close.
#[test]
fn a_row_written_before_the_cap_can_still_be_restated_and_moved() {
    let legacy = "L".repeat(TODO_TITLE_MAX * 2);
    let (mut rt, board) = runtime_with(vec![TodoItem {
        content: legacy.clone(),
        status: TodoStatus::Pending,
        by: TodoBy::Model,
        when: None,
        needs: Vec::new(),
    }]);
    let mut sink = RecordingToolSink::new();

    // The whole-list form, restating it — the case that would be a wedge if the cap were
    // checked on the row rather than on the write.
    let r = rt.invoke(
        "t1",
        &call(
            &serde_json::json!({"todos": [{"content": legacy, "status": "completed"}]}).to_string(),
        ),
        &mut sink,
    );
    assert_eq!(r.outcome, ToolOutcome::Ok, "{} {:?}", r.payload, r.outcome);
    assert_eq!(board.snapshot()[0].status, TodoStatus::Completed);

    // And a quote, which is not a title at all.
    let r = rt.invoke(
        "t2",
        &call(
            &serde_json::json!({"update": [{"content": legacy, "status": "pending"}]}).to_string(),
        ),
        &mut sink,
    );
    assert_eq!(r.outcome, ToolOutcome::Ok, "{} {:?}", r.payload, r.outcome);
    assert_eq!(board.snapshot()[0].status, TodoStatus::Pending);
}

/// **A parent's `target` write follows the same cap**, and the refusal is the CHILD's board's:
/// the half a `target` writes is not the caller's, so the check is `upsert_parent`'s and its
/// `Err` reaches the model through the tool's target arm.
#[test]
fn a_parents_target_write_follows_the_same_cap() {
    let child = Arc::new(TodoBoard::new(Vec::new()));
    let (mut rt, own, _fake) = children_runtime(child.clone());
    let mut sink = RecordingToolSink::new();

    let long = "z".repeat(TODO_TITLE_MAX + 1);
    let r = rt.invoke(
        "t1",
        &call(
            &serde_json::json!({"target": "s-…908838",
                "todos": [{"content": long, "status": "pending"}]})
            .to_string(),
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
        said.contains(&format!("at most {TODO_TITLE_MAX}")),
        "the refusal names the cap on the child's board too: {said}"
    );
    assert!(
        child.snapshot().is_empty(),
        "nothing reached the child's board: {said}"
    );

    // **And a title at the cap lands there** — the rule is a bound, not a bar.
    let exact = "w".repeat(TODO_TITLE_MAX);
    let r = rt.invoke(
        "t2",
        &call(
            &serde_json::json!({"target": "s-…908838",
                "todos": [{"content": exact, "status": "pending"}]})
            .to_string(),
        ),
        &mut sink,
    );
    assert_eq!(r.outcome, ToolOutcome::Ok, "{} {:?}", r.payload, r.outcome);
    assert_eq!(child.snapshot().len(), 1);
    assert_eq!(child.snapshot()[0].content, exact);
    assert!(
        own.snapshot().is_empty(),
        "and this session's own board stayed empty"
    );
}
