//! The nag: which open row the check asks about, and when it says nothing.

use super::*;

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
