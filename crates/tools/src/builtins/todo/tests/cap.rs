//! The title cap.

use super::*;

/// The cap in the model-facing description is a literal, because a `format!` would mean escaping
/// every `{"kind": …}` example in it; this is what catches the constant and the sentence drifting.
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

/// The refusal names the row's words, the length it measured, the cap and where the rest of the row
/// goes; nothing is written. The cap's own reason lives at `TODO_TITLE_MAX`.
#[test]
fn a_title_over_the_cap_is_refused_by_name_and_nothing_is_written() {
    let (rt, board) = runtime();
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

/// **A title AT the cap is written whole** - the bound is a bound and not a shave, and the row
/// that passes is the row the model gets back.
#[test]
fn a_title_at_the_cap_is_written() {
    let (rt, board) = runtime();
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

/// **The cap is on the TITLE, and the detail after ` - ` is not capped.** A row whose title fits
/// is written whole, detail and all - that is the sentence the tool's description gives the
/// model for anything long, so it has to be the thing that actually passes.
#[test]
fn a_long_detail_after_the_separator_is_not_the_title() {
    let (rt, board) = runtime();
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

/// The cap binds the words a call WRITES, not the rows already on the board: an old long row can
/// still be restated and moved, so a board that predates the cap is not wedged shut.
#[test]
fn a_row_written_before_the_cap_can_still_be_restated_and_moved() {
    let legacy = "L".repeat(TODO_TITLE_MAX * 2);
    let (rt, board) = runtime_with(vec![TodoItem {
        content: legacy.clone(),
        status: TodoStatus::Pending,
        by: TodoBy::Model,
        when: None,
        needs: Vec::new(),
    }]);
    let mut sink = RecordingToolSink::new();

    // The whole-list form, restating it - the case that would be a wedge if the cap were
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
    let (rt, own, _fake) = children_runtime(child.clone());
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

    // **And a title at the cap lands there** - the rule is a bound, not a bar.
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
