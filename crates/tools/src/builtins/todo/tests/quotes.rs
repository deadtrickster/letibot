//! Removing rows by quote.

use super::*;

/// Nothing this tool sends removes a row, so an empty list cannot mean "clear the board": it says
/// nothing at all, and is refused rather than read as a call that did something.
#[test]
fn an_empty_list_is_refused_and_removes_nothing() {
    let (rt, board) = runtime();
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

/// `update` takes a row's exact content plus its new status, so one row can be marked without
/// resending the list; the assertion is a line diff, which a row that moved or vanished would fail.
#[test]
fn a_quote_marks_exactly_one_row() {
    let (rt, board) = runtime();
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
    // NO `todos` AT ALL. One row, named by its own words - which is why `todos` is no longer
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

/// A quote that fits no row is refused naming the rows that half DOES hold, so the model can quote
/// one instead of guessing; and nothing is written, neither the quote nor the `todos` beside it.
#[test]
fn a_quote_that_fits_no_row_is_refused_by_name() {
    let (rt, board) = runtime();
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

/// Two rows answer to one quote when their text differs only in whitespace, because the match
/// trims: refusing beats a row changing state under a model that meant the other one.
#[test]
fn a_quote_that_fits_two_rows_is_refused_naming_both() {
    let (rt, board) = runtime_with(vec![
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
