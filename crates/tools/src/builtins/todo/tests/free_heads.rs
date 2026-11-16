//! The check names every free head of the plan, so independent rows can run in parallel.
//! A row held by another row, or by a wait, is not a free head.

use super::*;

const T0: u64 = 1_700_000_000_000;

fn naming(rows: &[TodoItem], jobs: &RunningJobs) -> Option<(String, Vec<String>)> {
    let none = ChildSessions::none();
    let askable = the_plan_as_checked(rows, &none, jobs);
    unfinished_plan_naming(&askable, rows, None, &none, jobs)
}

fn with(content: &str, need: TodoNeed) -> TodoItem {
    TodoItem {
        needs: vec![need],
        ..item(content, TodoStatus::Pending)
    }
}

#[test]
fn two_independent_free_rows_are_both_named() {
    let plan = vec![
        item("port the parser", TodoStatus::Pending),
        item("write the docs", TodoStatus::Pending),
    ];
    let (text, named) = naming(&plan, &no_jobs()).expect("open work");
    assert_eq!(named, ["port the parser", "write the docs"]);
    assert!(text.contains("\n  - port the parser — yours\n"), "{text}");
    assert!(text.contains("\n  - write the docs — yours\n"), "{text}");
    assert!(
        text.contains(
            "\nThese do not depend on each other: start each as a subagent with `task` (or a \
             background job for a long command) and collect them."
        ),
        "the recommendation, naming `task`: {text}"
    );
    // One advice line per author, not one per row.
    assert_eq!(text.matches("`update` field").count(), 1, "{text}");
}

#[test]
fn a_row_blocked_by_another_is_not_named() {
    let plan = vec![
        item("port the parser", TodoStatus::Pending),
        item("write the docs", TodoStatus::Pending),
        needs("release", TodoStatus::Pending, &["port the parser"]),
    ];
    let (text, named) = naming(&plan, &no_jobs()).expect("open work");
    assert_eq!(named, ["port the parser", "write the docs"]);
    assert!(!text.contains("  - release"), "not a free head: {text}");
    assert!(text.contains("release"), "said on the waiting line: {text}");
}

#[test]
fn one_free_row_keeps_the_single_row_check() {
    let plan = vec![
        item("port the parser", TodoStatus::Pending),
        needs("release", TodoStatus::Pending, &["port the parser"]),
    ];
    let (text, named) = naming(&plan, &no_jobs()).expect("open work");
    assert_eq!(named, ["port the parser"]);
    // Byte for byte the single-row check.
    assert_eq!(
        text,
        "[todo check] Not done:\n  - port the parser — yours\nDo it, or mark it done with \
         `todo_write`'s `update` field (quote it exactly).\n\
         1 row cannot start yet: `release` waits on `port the parser` (still open).",
    );
    assert!(!text.contains("subagent"), "{text}");
}

#[test]
fn an_external_wait_silences_its_row_only_for_its_window() {
    let plan = vec![
        item("port the parser", TodoStatus::Pending),
        with(
            "push the archive",
            TodoNeed::External {
                what: "a vendor's upload slot".into(),
                since_ms: Some(T0),
            },
        ),
    ];
    let inside = RunningJobs::none().at(T0 + 3_600_000);
    let (text, named) = naming(&plan, &inside).expect("open work");
    assert_eq!(named, ["port the parser"]);
    assert!(!text.contains("push the archive"), "quiet inside: {text}");

    let after = RunningJobs::none().at(T0 + 2 * 3_600_000 + 1);
    let (text, named) = naming(&plan, &after).expect("open work");
    assert_eq!(
        named,
        ["port the parser"],
        "a lapsed wait is not a free head"
    );
    assert!(
        text.contains("push the archive"),
        "asked about again: {text}"
    );
}

#[test]
fn an_operator_or_seat_wait_keeps_its_row_out_of_the_heads() {
    let plan = vec![
        item("port the parser", TodoStatus::Pending),
        item("write the docs", TodoStatus::Pending),
        with(
            "pick the database",
            TodoNeed::Operator {
                question: "which database?".into(),
                options: vec!["postgres".into(), "sqlite".into()],
            },
        ),
        with(
            "merge the peer's fix",
            TodoNeed::Ask {
                ask: 7,
                of: "cc@etcd-752".into(),
                room: "proj".into(),
                question: "is the fix pushed?".into(),
            },
        ),
    ];
    let (text, named) = naming(&plan, &no_jobs()).expect("open work");
    assert_eq!(named, ["port the parser", "write the docs"]);
    assert!(!text.contains("  - pick the database"), "{text}");
    assert!(!text.contains("  - merge the peer's fix"), "{text}");
    assert!(text.contains("waiting on the operator"), "tracked: {text}");
    assert!(
        text.contains("waiting on another session"),
        "tracked: {text}"
    );

    // Every row on a tracked wait: nothing is the model's to move, so the check is quiet.
    assert!(naming(&plan[2..], &no_jobs()).is_none());
}
