//! **The room's rows on a session's board** — `TodoBoard::set_room_rows`, and the one predicate
//! that decides whether the idle check speaks about them.
//!
//! `crates/harnessd/tests/room_reminders.rs` is the end-to-end proof (a real socket, a real
//! harness, the real nag). This file is the board's own contract, which that test would only reach
//! through a daemon: what a *second* pass does, what an item that is gone does, and what happens to
//! a row the model already wrote with the same words.

use super::*;

/// One room row, as the daemon's pass writes it: the session's own, in progress, and quiet or loud
/// by whether it carries the room edge.
fn room_row(item: u64, waiting: bool) -> TodoItem {
    TodoItem {
        content: format!("you hold `ram` #{item} — 8 GB"),
        status: TodoStatus::InProgress,
        by: TodoBy::Model,
        when: None,
        needs: vec![TodoNeed::Room {
            item,
            working: waiting,
        }],
    }
}

/// **One row per item, updated not appended** — the whole of why the row's words carry no clock.
/// A second pass over one claim is not a second row, and a pass that finds the wait ended moves
/// the SAME row's edge rather than writing beside it.
#[test]
fn a_second_pass_updates_the_row_and_never_adds_one() {
    let b = TodoBoard::new(Vec::new());
    assert_eq!(b.set_room_rows(vec![(1, room_row(1, false))]), 1);
    assert_eq!(b.snapshot().len(), 1, "{:?}", b.snapshot());

    // Three more passes: two identical, one that moves the edge.
    b.set_room_rows(vec![(1, room_row(1, false))]);
    b.set_room_rows(vec![(1, room_row(1, false))]);
    assert_eq!(b.snapshot().len(), 1, "{:?}", b.snapshot());

    b.set_room_rows(vec![(1, room_row(1, true))]);
    let rows = b.snapshot();
    assert_eq!(rows.len(), 1, "an edge moving is not a row: {rows:?}");
    assert_eq!(
        rows[0].needs,
        vec![TodoNeed::Room {
            item: 1,
            working: true
        }]
    );
}

/// **A pass that changes nothing is not an event** — no version bump, so no store write and no
/// publish on an idle daemon that is holding one item and doing nothing with it.
#[test]
fn a_pass_that_changes_nothing_bumps_nothing() {
    let b = TodoBoard::new(Vec::new());
    b.set_room_rows(vec![(1, room_row(1, true))]);
    let v = b.version();
    b.set_room_rows(vec![(1, room_row(1, true))]);
    assert_eq!(b.version(), v, "the same answer is not a second write");

    // …and a change IS one.
    b.set_room_rows(vec![(1, room_row(1, false))]);
    assert!(b.version() > v, "an edge coming off is news");
}

/// **Retired when the item is** — the pass that no longer names an item takes that item's row off
/// the board, in the same pass. This is the one door on the model's half that can remove a row,
/// and the argument is the queue's: a room row is the ROOM seen from a session, and the record of
/// it is the room.
#[test]
fn an_item_that_is_gone_takes_its_row_with_it() {
    let b = TodoBoard::new(Vec::new());
    b.set_room_rows(vec![(1, room_row(1, false)), (2, room_row(2, false))]);
    assert_eq!(b.snapshot().len(), 2);

    // Item 1 is done; item 2 is still held.
    assert_eq!(b.set_room_rows(vec![(2, room_row(2, false))]), 1);
    let rows = b.snapshot();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(rows[0].content.contains("#2"), "{rows:?}");

    // …and nothing held is an empty board.
    b.set_room_rows(Vec::new());
    assert!(b.snapshot().is_empty(), "{:?}", b.snapshot());
}

/// **It must not fight a row the model wrote** — the board has one key and it is the trimmed
/// `content`, so a row that already says these words IS this row: the pass moves it into place
/// and writes no second copy of the same sentence.
#[test]
fn a_row_the_model_already_wrote_is_reused_and_a_reworded_one_is_left_alone() {
    // The model wrote the sentence itself, in its own plan, at Pending.
    let model_wrote = TodoItem {
        content: "you hold `ram` #1 — 8 GB".into(),
        status: TodoStatus::Pending,
        by: TodoBy::Model,
        when: None,
        needs: Vec::new(),
    };
    // …and a row of its own about something else.
    let unrelated = item("wire the pane", TodoStatus::Pending);
    let b = TodoBoard::new(vec![model_wrote, unrelated]);

    assert_eq!(b.set_room_rows(vec![(1, room_row(1, true))]), 1);
    let rows = b.snapshot();
    assert_eq!(
        rows.len(),
        2,
        "the pass must not put a second copy of one sentence on the board: {rows:?}"
    );
    let room = rows
        .iter()
        .find(|r| r.content.starts_with("you hold `ram` #1"))
        .expect("the row");
    assert_eq!(
        room.needs,
        vec![TodoNeed::Room {
            item: 1,
            working: true
        }],
        "it is the same row, so the pass's edge goes on it"
    );
    assert_eq!(
        room.status,
        TodoStatus::InProgress,
        "and the pass says what the row now is"
    );
    assert!(
        rows.iter().any(|r| r.content == "wire the pane"),
        "and nothing else on the board moved: {rows:?}"
    );
}

/// **A row the model re-worded is the MODEL's** — this board's key is the words, so a re-worded
/// row is a new row, and the pass's own row is still there to retire. What must not happen is the
/// pass deleting a row it did not write.
///
/// **And retirement needs no memory of what was written**, which is what makes it survive a
/// daemon restart: `Harness::open` rebuilds the board from the store, so the first pass of a new
/// process has written nothing — and the row says whose it is, because the edge is on it.
#[test]
fn retiring_by_the_edge_survives_a_board_rebuilt_from_a_store() {
    let b = TodoBoard::new(Vec::new());
    b.set_room_rows(vec![(1, room_row(1, false))]);
    // The model restates the row in its own words: a new row, by this board's one rule.
    let mut model = b.snapshot();
    model.push(item(
        "hold `ram` #1 while the suite runs",
        TodoStatus::Pending,
    ));
    // **A NEW board** — the restart. It remembers nothing about having written anything.
    let b = TodoBoard::new(model);
    assert_eq!(b.snapshot().len(), 2);

    // The item is gone. The pass's row goes; the model's own words stay.
    b.set_room_rows(Vec::new());
    let rows = b.snapshot();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].content, "hold `ram` #1 while the suite runs");
}

/// **A row whose room edge names an item still held is left alone**, even if the pass writes no
/// row for it — the guard on the other side of the retirement above, so a rebuilt board is not
/// emptied by its first pass.
#[test]
fn a_rebuilt_board_keeps_the_rows_the_pass_is_about_to_write_again() {
    let b = TodoBoard::new(Vec::new());
    b.set_room_rows(vec![(1, room_row(1, true))]);
    let b = TodoBoard::new(b.snapshot());
    b.set_room_rows(vec![(1, room_row(1, false))]);
    let rows = b.snapshot();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(
        !waits_on_the_room(&rows[0]),
        "and the edge the pass brought with it is the loud one"
    );
}

/// **The edge is the whole of quiet-or-loud** — `waits_on_the_room` is what the idle check reads,
/// and it needs no snapshot from the caller because the daemon that wrote the row already asked
/// the room.
#[test]
fn the_room_edge_is_what_makes_the_row_quiet() {
    assert!(waits_on_the_room(&room_row(1, true)));
    assert!(!waits_on_the_room(&room_row(1, false)));
    assert!(!waits_on_the_room(&item(
        "wire the pane",
        TodoStatus::Pending
    )));
}

/// **A quiet row is not asked about and is not listed as blocked either** — the operator's own
/// silence rule, one kind over: a row whose blocker is the room working on what the holder waits
/// for is not a plan to fix, so the check says nothing at all.
#[test]
fn a_quiet_row_is_not_nagged_about() {
    let quiet = room_row(1, true);
    assert!(
        nag_for(std::slice::from_ref(&quiet)).is_none(),
        "the check must say nothing about a row the room is working on"
    );
    // …and the same row with the edge off is named, with what the holder has to do about it.
    let loud = room_row(1, false);
    let said =
        nag_for(std::slice::from_ref(&loud)).expect("a row with nothing coming is asked about");
    assert!(said.contains("you hold `ram` #1 — 8 GB"), "{said}");
    assert!(
        said.contains("yours"),
        "the row is the session's own: {said}"
    );
}

/// **A plan blocked only by the room is not a plan to fix.** The ready set is empty and every
/// unmet need is a room edge — the roots [`all_on_the_operator`] already treats as not-the-plan's
/// — so the degenerate *nothing can start* sentence must not fire either.
#[test]
fn a_plan_that_only_waits_on_the_room_is_silent() {
    let quiet = room_row(1, true);
    assert!(
        nag_for(std::slice::from_ref(&quiet)).is_none(),
        "{:?}",
        nag_for(std::slice::from_ref(&quiet))
    );
    // The fate is still answerable for a caller that did not narrow: the room item, by the number
    // `room`'s own verbs take.
    let said = unfinished_plan_for(
        std::slice::from_ref(&quiet),
        std::slice::from_ref(&quiet),
        None,
        &ChildSessions::none(),
        &no_jobs(),
    );
    assert!(
        said.is_none(),
        "and the narrowed reading is silent too: {said:?}"
    );
}
