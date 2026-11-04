//! The board's resource bounds: what a row's length may cost, and what is never refused for it.

use super::*;

/// The bounds in the model-facing description are literals, because a `format!` would mean
/// escaping every `{"kind": …}` example in it; this is what catches the constants and the
/// sentences drifting.
#[test]
fn the_tools_description_names_the_bounds_it_enforces() {
    let board = Arc::new(TodoBoard::new(Vec::new()));
    let schema = super::TodoWriteTool::new(board).schema();
    assert!(
        schema.description.contains(&format!(
            "a row over {TODO_ROW_MAX_BYTES} bytes, or a write past {TODO_ROWS_MAX} rows"
        )),
        "the description does not name the bounds: {}",
        schema.description
    );
    assert!(
        schema.description.contains("Length is never refused"),
        "nor that length is not the rule: {}",
        schema.description
    );
    let content =
        schema.parameters["properties"]["todos"]["items"]["properties"]["content"]["description"]
            .as_str()
            .expect("the `todos` entry's `content` says what a content is");
    assert!(
        content.contains("Stored exactly as written"),
        "the field the model writes says the row is stored as written: {content}"
    );
    assert!(
        content.contains(&TODO_ROW_MAX_BYTES.to_string()),
        "the field the model writes does not name the byte bound either: {content}"
    );
}

// -- a row is stored as written ------------------------------------------

/// The case that made the bound: a 168-character title is stored verbatim, with no refusal and
/// nothing moved to a detail. The pane cuts at draw time; the writer does not.
#[test]
fn a_168_character_title_is_written_verbatim() {
    let (rt, board) = runtime();
    let mut sink = RecordingToolSink::new();
    let words = "write the migration that moves the board's rows into the new store and backfills \
                 the old ones from the on-disk json, then checks a resumed session sees them";
    let long = format!("{words} — and some more words to push the title past any pane");
    assert!(long.chars().count() > 160, "{}", long.chars().count());
    let r = rt.invoke(
        "t1",
        &call(&serde_json::json!({"todos": [{"content": long, "status": "pending"}]}).to_string()),
        &mut sink,
    );
    assert_eq!(r.outcome, ToolOutcome::Ok, "{} {:?}", r.payload, r.outcome);
    let all = board.snapshot();
    assert_eq!(all.len(), 1);
    assert_eq!(
        all[0].content, long,
        "the words are stored exactly as written"
    );
    assert!(
        r.payload.contains(&long),
        "and rendered whole, to quote back: {}",
        r.payload
    );
}

/// A long detail after the separator rides with the row: the row is one `content`, and nothing
/// in the writer splits or trims it.
#[test]
fn a_long_detail_after_the_separator_rides_whole() {
    let (rt, board) = runtime();
    let mut sink = RecordingToolSink::new();
    let content = format!(
        "fix the flaky test — {}",
        "d".repeat(TODO_ROW_MAX_BYTES / 2)
    );
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
    assert_eq!(all[0].content, content, "the detail is kept with the title");
}

// -- the resource bounds -------------------------------------------------

/// The refusal names the resource and the number, and nothing is written. Bytes, not chars: the
/// store holds bytes, and a row of multibyte characters reaches the bound first.
#[test]
fn a_row_over_the_byte_bound_is_refused_and_nothing_is_written() {
    let (rt, board) = runtime();
    let mut sink = RecordingToolSink::new();
    let long = "ä".repeat(TODO_ROW_MAX_BYTES / 2 + 1);
    assert!(long.len() > TODO_ROW_MAX_BYTES, "{} bytes", long.len());
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
    let n = long.len();
    assert!(
        said.contains(&format!("that row is {n}")),
        "the size it measured is named: {said}"
    );
    assert!(
        said.contains(&format!("at most {TODO_ROW_MAX_BYTES} bytes")),
        "and the bound: {said}"
    );
    assert!(board.snapshot().is_empty(), "nothing was written: {said}");
}

/// The other half of the pair: a write that would take the board past the row bound is refused
/// naming the count, and nothing is written - not even the rows under it.
#[test]
fn a_write_past_the_row_bound_is_refused_whole() {
    let (rt, board) = runtime();
    let mut sink = RecordingToolSink::new();
    let rows: Vec<serde_json::Value> = (0..TODO_ROWS_MAX + 1)
        .map(|i| serde_json::json!({"content": format!("step {i}"), "status": "pending"}))
        .collect();
    let r = rt.invoke(
        "t1",
        &call(&serde_json::json!({"todos": rows}).to_string()),
        &mut sink,
    );
    assert!(
        matches!(r.outcome, ToolOutcome::Failed { .. }),
        "{:?}",
        r.outcome
    );
    let said = format!("{} {:?}", r.payload, r.outcome);
    assert!(
        said.contains(&format!("at most {TODO_ROWS_MAX} rows")),
        "the refusal names the bound: {said}"
    );
    assert!(
        said.contains(&format!("would make it {}", TODO_ROWS_MAX + 1)),
        "and the count it measured: {said}"
    );
    assert!(board.snapshot().is_empty(), "nothing was written: {said}");
}

/// The bounds bind the rows a call WRITES, not the rows already on the board: an old row over
/// the byte bound can still be restated and moved, so a board that predates it is not wedged
/// shut.
#[test]
fn a_row_written_before_the_bound_can_still_be_restated_and_moved() {
    let legacy = "L".repeat(TODO_ROW_MAX_BYTES + 64);
    let (rt, board) = runtime_with(vec![TodoItem {
        content: legacy.clone(),
        status: TodoStatus::Pending,
        by: TodoBy::Model,
        when: None,
        needs: Vec::new(),
    }]);
    let mut sink = RecordingToolSink::new();

    // The whole-list form, restating it - the case that would be a wedge if the bound were
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

    // And a quote, which is not a new row at all.
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

/// A parent's `target` write follows the same bounds, and the refusal is the CHILD's board's:
/// the half a `target` writes is not the caller's, so the check is `upsert_parent`'s and its
/// `Err` reaches the model through the tool's target arm.
#[test]
fn a_parents_target_write_follows_the_same_bounds() {
    let child = Arc::new(TodoBoard::new(Vec::new()));
    let (rt, own, _fake) = children_runtime(child.clone());
    let mut sink = RecordingToolSink::new();

    let long = "z".repeat(TODO_ROW_MAX_BYTES + 1);
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
        said.contains(&format!("at most {TODO_ROW_MAX_BYTES} bytes")),
        "the refusal names the bound on the child's board too: {said}"
    );
    assert!(
        child.snapshot().is_empty(),
        "nothing reached the child's board: {said}"
    );

    // And a long title under the bound lands there verbatim - a bound, not a bar.
    let exact = "w".repeat(TODO_ROW_MAX_BYTES);
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
