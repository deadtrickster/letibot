//! A Claude Code seat's board on the head's todo pane (R144, `docs/seat-todo.md`): a seat is not a
//! session, so its boards arrive on the `Todos` read beside the session's, and the pane only draws
//! them - the seat in another process writes them, and no key here moves one of its rows.

use super::*;

use letibot_sessionlog::event::{SeatBoard, TodoBy, TodoEntry, TodoNeed, TodoStatus};

/// One row, built the way the daemon's store hands it over.
fn row(content: &str, status: TodoStatus, needs: Vec<TodoNeed>) -> TodoEntry {
    TodoEntry {
        content: content.into(),
        status,
        by: TodoBy::Model,
        when: None,
        needs,
    }
}

/// One seat's board, as the daemon's `Todos` answer carries it.
fn seat(handle: &str, todos: Vec<TodoEntry>) -> SeatBoard {
    SeatBoard {
        seat: handle.into(),
        todos,
    }
}

/// The daemon's answer: the session's board and the seats' boards on one frame, which is the one
/// read the pane makes.
fn board(a: &mut App, todos: Vec<TodoEntry>, seats: Vec<SeatBoard>) {
    a.apply(ServerFrame::Todos {
        seats,
        session_id: a.session_id.clone(),
        todos,
    });
}

/// The package a seat orchestrates: one of this daemon's sessions, named so the pane can draw it.
const PACKAGE: &str = "s-1789462738453908838";

/// **The head draws a seat's board.** Its handle and its rows, in the pane the session's board is
/// drawn in, under the session's and above the repo's file: a row carries its mark and its author,
/// which is the whole of what a session's row carries.
#[test]
fn a_seats_board_is_drawn_in_the_todo_pane_with_its_rows() {
    let mut a = app();
    a.session_id = "s1".into();
    board(
        &mut a,
        vec![row(
            "the session's own row",
            TodoStatus::Pending,
            Vec::new(),
        )],
        vec![seat(
            "cc@proj",
            vec![
                row(
                    "collect the package",
                    TodoStatus::InProgress,
                    vec![TodoNeed::Child { id: PACKAGE.into() }],
                ),
                row("land the port", TodoStatus::Completed, Vec::new()),
            ],
        )],
    );
    a.key(Key::CtrlT);
    let lines = a.todos_lines(120);
    // The handle, the count, and whose board it is: a seat's rows are not this session's plan and
    // the pane has to say so where they are drawn.
    assert!(
        lines
            .iter()
            .any(|l| l.contains("cc@proj — a Claude Code seat's board, 1 open")),
        "{lines:#?}"
    );
    // Every row, with the mark a session's row gets and the author in the pane's own word.
    assert!(
        lines
            .iter()
            .any(|l| l.contains("[~] collect the package  — model")),
        "{lines:#?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("[x] land the port  — model")),
        "a done row of a seat's board is drawn done: {lines:#?}"
    );
    // And the session's own half is untouched.
    assert!(
        lines
            .iter()
            .any(|l| l.contains("[ ] the session's own row  — model")),
        "{lines:#?}"
    );
}

/// **A seat's row shows the wait it was written with**, and that wait is a package of THIS daemon:
/// the edge is `wait_on`'s `Child`, drawn with the short name the subagents pane gives it, exactly as
/// a session's row waiting on its own child is.
#[test]
fn a_seat_row_waiting_on_a_package_shows_the_package() {
    let mut a = app();
    a.session_id = "s1".into();
    let mut package = brief(PACKAGE, "fix-login", false);
    package.slug = "fix-login".into();
    a.sessions = vec![package];
    board(
        &mut a,
        Vec::new(),
        vec![seat(
            "cc@proj",
            vec![row(
                "collect the package",
                TodoStatus::InProgress,
                vec![TodoNeed::Child { id: PACKAGE.into() }],
            )],
        )],
    );
    a.key(Key::CtrlT);
    let lines = a.todos_lines(140);
    let drawn = lines
        .iter()
        .find(|l| l.contains("collect the package"))
        .unwrap_or_else(|| panic!("the seat's row is not drawn: {lines:#?}"));
    let short = letibot_sessionlog::registry::short_id(PACKAGE);
    assert!(
        drawn.ends_with(&format!("waits on {short} · fix-login")),
        "the wait names the package and its own name: {drawn:?}"
    );
    assert!(
        !drawn.contains(PACKAGE),
        "and the whole id is the overlay's, not the pane's: {drawn:?}"
    );
}

/// **Nothing on a seat's board is the head's to act on.** The stops are the session's rows and the
/// file's items, so the cursor cannot land on a seat's row and Enter there is nothing: the seat
/// writes its own board and this pane only reads it.
#[test]
fn a_seats_rows_are_not_stops_and_add_no_cursor() {
    let mut a = app();
    a.session_id = "s1".into();
    board(
        &mut a,
        vec![row(
            "the session's own row",
            TodoStatus::Pending,
            Vec::new(),
        )],
        vec![seat(
            "cc@proj",
            vec![
                row("collect the package", TodoStatus::Pending, Vec::new()),
                row("land the port", TodoStatus::Pending, Vec::new()),
            ],
        )],
    );
    a.key(Key::CtrlT);
    a.todos_lines(120);
    // The control and the session's one row - and not the seat's two.
    assert_eq!(
        a.todos_stops(),
        vec![
            TodoStop::Add,
            TodoStop::Theirs("the session's own row".into()),
        ],
        "a seat's row became a stop"
    );
    // One Down is the session's row, one more wraps: a row the pane draws and the cursor skips is
    // the arrangement, and a walk that stopped on it would offer Enter an act with no owner.
    a.key(Key::Down);
    assert_eq!(a.todos_sel, 1);
    a.key(Key::Down);
    assert_eq!(a.todos_sel, 0, "the walk wrapped past the seat's rows");
}

/// **A head told about no seats draws the pane it always drew.** Two heads, one board, and the one
/// that was never told the field exists: a daemon from before this frame, or a folder with no seat
/// in it, is not a pane with a section on it.
#[test]
fn a_pane_with_no_seat_boards_is_drawn_exactly_as_before() {
    // The older daemon's frame, spelled out: it has no `seats` at all, and this head reads it.
    let old: ServerFrame = serde_json::from_str(
        r#"{"frame":"todos","session_id":"s1","todos":[
             {"content":"a row of the session's","status":"pending"}]}"#,
    )
    .expect("a version-66 daemon's frame still parses");
    let mut a = app();
    a.session_id = "s1".into();
    a.apply(old);
    a.key(Key::CtrlT);
    let never_told = a.todos_lines(120);

    let mut b = app();
    b.session_id = "s1".into();
    board(
        &mut b,
        vec![row(
            "a row of the session's",
            TodoStatus::Pending,
            Vec::new(),
        )],
        Vec::new(),
    );
    b.key(Key::CtrlT);
    assert_eq!(
        never_told,
        b.todos_lines(120),
        "a frame with no seats and one with none drawn are the same pane"
    );
    assert!(
        !never_told.iter().any(|l| l.contains("seat's board")),
        "nothing about seats is drawn: {never_told:#?}"
    );
}

/// Two seats are two boards, drawn one after the other: the store keeps a seat per handle, and a
/// pane that drew only the first would hide a plan the operator was told was on the screen.
#[test]
fn every_seat_in_the_store_is_drawn() {
    let mut a = app();
    a.session_id = "s1".into();
    board(
        &mut a,
        Vec::new(),
        vec![
            seat(
                "cc@proj",
                vec![row("collect", TodoStatus::Pending, Vec::new())],
            ),
            seat(
                "cc@other",
                vec![row("wait for CI", TodoStatus::Pending, Vec::new())],
            ),
        ],
    );
    a.key(Key::CtrlT);
    let lines = a.todos_lines(120);
    for want in [
        "cc@proj — a Claude Code seat's board, 1 open",
        "cc@other — a Claude Code seat's board, 1 open",
        "[ ] collect  — model",
        "[ ] wait for CI  — model",
    ] {
        assert!(
            lines.iter().any(|l| l.contains(want)),
            "{want:?} is not drawn: {lines:#?}"
        );
    }
}
