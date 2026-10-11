//! Removing rows by quote.

use super::*;

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
