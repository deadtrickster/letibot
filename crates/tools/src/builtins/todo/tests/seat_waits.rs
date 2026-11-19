//! A `todo` wait on ANOTHER board: a seat's row or another session's, refused by name when the
//! board is not there, quiet while that row is open, and said out loud when the board finishes it.

use super::*;

use crate::ToolResult;
use letibot_tokencore::store::{Archived, SessionRecord, Store};

/// The fake store: two boards with rows, and one name nothing answers to. Everything the tool asks
/// of another board, and none of the store's own work - which file a row is in is the store's, and
/// is tested where the store is.
struct FakeBoards;

impl super::Boards for FakeBoards {
    fn board_of(&self, seat: &str) -> Result<Board, String> {
        match seat {
            "cc@etcd-a36" => Ok(Board::Rows(vec![
                item(
                    "collect the package — its turn end is the wait",
                    TodoStatus::InProgress,
                ),
                item("read the design", TodoStatus::Completed),
            ])),
            "db-fuzz" => Ok(Board::Rows(vec![item(
                "ship the pane",
                TodoStatus::InProgress,
            )])),
            "cc@elsewhere" => Err(
                "`cc@elsewhere` is a board of another project, and this one is `etcd-a36`".into(),
            ),
            other => Ok(Board::Unknown(format!(
                "no seat and no session is called `{other}`: its seats are `cc@etcd-a36`"
            ))),
        }
    }
}

fn boards_runtime(board: Arc<TodoBoard>) -> (ToolRuntime, Arc<TodoBoard>) {
    let mut reg = Registry::new();
    reg.register(Box::new(
        super::TodoWriteTool::new(board.clone()).with_boards(Arc::new(FakeBoards)),
    ))
    .unwrap();
    let d = TempDir::new();
    let backend = HostBackend::new(d.path()).unwrap();
    std::mem::forget(d);
    (ToolRuntime::new(reg, Box::new(backend)), board)
}

const PACKAGE: &str = "collect the package — its turn end is the wait";
const ROW: &str = "wait for the package";

/// A refusal as the model reads it: the reason AND the payload, because the name of the thing that
/// was refused is in the one and the shape that would have been right in the other.
fn said_out(inv: &ToolResult) -> String {
    format!("{:?} {}", inv.outcome, inv.payload)
}

/// The wait is written with the shape the lead asks for, and it keeps the row quiet: no nag while
/// another board's row is open, and the check speaks again once the sweep has taken the edge off.
#[test]
fn a_row_waiting_on_another_board_is_quiet_until_that_row_is_finished() {
    let (rt, board) = boards_runtime(Arc::new(TodoBoard::new(Vec::new())));
    let mut sink = RecordingToolSink::new();
    let wrote = rt.invoke(
        "t1",
        &call(&format!(
            r#"{{"todos": [{{"content": "{ROW}", "status": "in_progress",
                "needs": [{{"kind": "todo", "seat": "cc@etcd-a36", "content": "{PACKAGE}"}}]}}]}}"#
        )),
        &mut sink,
    );
    assert!(
        matches!(wrote.outcome, ToolOutcome::Ok),
        "{:?} {}",
        wrote.outcome,
        wrote.payload
    );
    assert!(
        wrote.payload.contains("cc@etcd-a36") && wrote.payload.contains(PACKAGE),
        "the reply names the board and its row: {}",
        wrote.payload
    );
    let rows = board.snapshot();
    assert_eq!(
        rows[0].needs,
        vec![TodoNeed::Todo {
            seat: "cc@etcd-a36".into(),
            content: PACKAGE.into(),
        }]
    );

    // Quiet while the other board's row is open - the edge itself is the fact, as a room's is.
    let none = ChildSessions::none();
    let jobs = no_jobs();
    assert!(!the_check_may_ask_about(&rows[0], &none, &jobs));
    assert!(the_plan_as_checked(&rows, &none, &jobs).is_empty());
    assert_eq!(
        nag_with(&the_plan_as_checked(&rows, &none, &jobs), &none, &jobs),
        None,
        "the check has nothing to say while that board's row is open"
    );

    // And the moment the edge is off - what the sweep does when that row is completed - the row is
    // work the check may ask about again.
    let mut freed = rows.clone();
    freed[0].needs.clear();
    assert!(the_check_may_ask_about(&freed[0], &none, &jobs));
    let text = nag_with(&freed, &none, &jobs).expect("the row is asked about again");
    assert!(text.contains(ROW), "{text}");
}

/// A wait on a board this project has not got is refused BY NAME, with what the store does hold -
/// never written as an edge nothing would ever free.
#[test]
fn a_wait_on_a_board_this_project_has_not_got_is_refused_by_name() {
    let (rt, board) = boards_runtime(Arc::new(TodoBoard::new(Vec::new())));
    let mut sink = RecordingToolSink::new();
    let refused = rt.invoke(
        "t1",
        &call(
            r#"{"todos": [{"content": "wait for it", "status": "pending",
                "needs": [{"kind": "todo", "seat": "cc@nowhere", "content": "whatever"}]}]}"#,
        ),
        &mut sink,
    );
    assert!(
        matches!(refused.outcome, ToolOutcome::Failed { .. }),
        "{:?} {}",
        refused.outcome,
        refused.payload
    );
    let said = said_out(&refused);
    assert!(
        said.contains("cc@nowhere") && said.contains("cc@etcd-a36"),
        "the refusal names the seat and what there is: {said}"
    );
    assert!(board.snapshot().is_empty(), "nothing was written");

    // A board of ANOTHER project is refused too, and says which project this is.
    let other = rt.invoke(
        "t2",
        &call(
            r#"{"todos": [{"content": "wait for it", "status": "pending",
                "needs": [{"kind": "todo", "seat": "cc@elsewhere", "content": "whatever"}]}]}"#,
        ),
        &mut sink,
    );
    assert!(
        matches!(other.outcome, ToolOutcome::Failed { .. }),
        "{:?}",
        other.outcome
    );
    let said = said_out(&other);
    assert!(said.contains("etcd-a36"), "{said}");
    assert!(board.snapshot().is_empty(), "nothing was written");
}

/// `wait_on` takes the same wait for a row already on the board, and refuses a seat by name in the
/// call that writes it.
#[test]
fn wait_on_attaches_the_wait_and_refuses_an_unknown_seat() {
    let (rt, board) = boards_runtime(Arc::new(TodoBoard::new(vec![item(
        ROW,
        TodoStatus::InProgress,
    )])));
    let mut sink = RecordingToolSink::new();
    let refused = rt.invoke(
        "t1",
        &call(
            r#"{"wait_on": [{"content": "wait for the package",
                "todo": {"seat": "cc@nowhere", "content": "whatever"}}]}"#,
        ),
        &mut sink,
    );
    assert!(
        matches!(refused.outcome, ToolOutcome::Failed { .. }),
        "{:?} {}",
        refused.outcome,
        refused.payload
    );
    let said = said_out(&refused);
    assert!(said.contains("cc@nowhere"), "{said}");
    assert!(board.snapshot()[0].needs.is_empty(), "nothing was written");

    let wrote = rt.invoke(
        "t2",
        &call(&format!(
            r#"{{"wait_on": [{{"content": "{ROW}",
                "todo": {{"seat": "cc@etcd-a36", "content": "{PACKAGE}"}}}}]}}"#
        )),
        &mut sink,
    );
    assert!(
        matches!(wrote.outcome, ToolOutcome::Ok),
        "{:?} {}",
        wrote.outcome,
        wrote.payload
    );
    assert_eq!(
        board.snapshot()[0].needs,
        vec![TodoNeed::Todo {
            seat: "cc@etcd-a36".into(),
            content: PACKAGE.into(),
        }]
    );
}

/// What a board says about the row a wait names, and what a write is refused for: finished, struck
/// off, gone, or still open. One function, so the sweep and the write cannot disagree.
#[test]
fn a_waits_ending_is_finished_struck_off_or_gone() {
    let open = vec![item(PACKAGE, TodoStatus::InProgress)];
    assert_eq!(wait_ended(&open, "cc@etcd-a36", PACKAGE), None);
    assert!(check_wait(&open, PACKAGE).is_ok());

    let done = vec![item(PACKAGE, TodoStatus::Completed)];
    assert_eq!(
        wait_ended(&done, "cc@etcd-a36", PACKAGE).as_deref(),
        Some("cc@etcd-a36 finished \"collect the package — its turn end is the wait\"")
    );
    assert!(
        check_wait(&done, PACKAGE)
            .unwrap_err()
            .contains("already completed")
    );

    let struck = vec![item(PACKAGE, TodoStatus::Cancelled)];
    assert!(
        wait_ended(&struck, "cc@etcd-a36", PACKAGE)
            .unwrap()
            .contains("struck")
    );
    assert!(check_wait(&struck, PACKAGE).is_err());

    // A board is replaced whole, so a row that is gone is a row somebody dropped.
    assert_eq!(
        wait_ended(&Vec::new(), "cc@etcd-a36", PACKAGE).as_deref(),
        Some(
            "`collect the package — its turn end is the wait` is no longer on cc@etcd-a36's board"
        )
    );
    assert!(
        check_wait(&Vec::new(), PACKAGE)
            .unwrap_err()
            .contains("no such row")
    );
}

/// The store's own half: a seat's board, a session's by id and by the one title it has, and a
/// board of another project refused - which is the rule that keeps a wait inside one project.
#[test]
fn a_board_is_read_by_seat_by_session_and_only_of_this_project() {
    let store = Store::open_in_memory().unwrap();
    store
        .put_seat_todos(
            "cc@etcd-a36",
            &[item("collect the package", TodoStatus::InProgress)],
        )
        .unwrap();
    // A seat that has written no board is not a seat with an empty one.
    assert!(store.seat_board("cc@other").unwrap().is_none());
    assert!(matches!(
        board_of(&store, &[], "cc@etcd-a36").unwrap(),
        Board::Rows(rows) if rows.len() == 1
    ));
    assert!(matches!(
        board_of(&store, &[], "cc@other").unwrap(),
        Board::Unknown(_)
    ));

    for (id, title, workspace) in [
        ("s-1", "db-fuzz", "/work/etcd-a36"),
        ("s-2", "elsewhere", "/work/other-thing"),
    ] {
        store
            .put_session(&SessionRecord {
                id: id.into(),
                title: Some(title.into()),
                model_id: "a model".into(),
                dialect_sha: "a dialect".into(),
                workspace_root: workspace.into(),
                owner: "dead".into(),
                approvers: Vec::new(),
                role: None,
                parent_session_id: None,
            })
            .unwrap();
        store
            .put_todos(id, &[item("ship the pane", TodoStatus::InProgress)])
            .unwrap();
    }
    let mine = folder_words(std::path::Path::new("/work/etcd-a36"));
    // The seat id a wait should carry is `<host>/<local>`, looked up as the session it names, and
    // the bare id is the same board. A TITLE is a name, so it is refused with the id behind it.
    for name in ["s-1", "lab2x1/s-1"] {
        assert!(
            matches!(board_of(&store, &mine, name).unwrap(), Board::Rows(rows) if rows.len() == 1),
            "{name} names a session of this project"
        );
    }
    let titled = board_of(&store, &mine, "db-fuzz").unwrap_err();
    assert!(
        titled.contains("is a name") && titled.contains("`s-1`"),
        "{titled}"
    );
    // And a BRIDGE's seat id finds the board that bridge writes under the id: the words a handle
    // carries are not there to match, so an id is looked up whole rather than refused as another
    // project's.
    store
        .put_seat_todos(
            "lab2x1/cc-7001",
            &[item("ship the pane", TodoStatus::InProgress)],
        )
        .unwrap();
    assert!(
        matches!(board_of(&store, &mine, "lab2x1/cc-7001").unwrap(), Board::Rows(rows) if rows.len() == 1),
        "a bridge's seat id names its own board"
    );
    assert!(
        matches!(board_of(&store, &[], "lab2x1/cc-7009").unwrap(), Board::Unknown(why) if why.contains("lab2x1/cc-7009")),
        "a seat that wrote none is unknown, and the sentence names it"
    );

    // A board of another project is refused, and the refusal names the project.
    let elsewhere = board_of(&store, &mine, "cc@other-thing").unwrap_err();
    assert!(elsewhere.contains("another project"), "{elsewhere}");
    assert!(elsewhere.contains("etcd-a36"), "{elsewhere}");

    // And every session is still in the index: the read above filters the board, not the store.
    assert_eq!(
        store.list_sessions_archived(Archived::Any).unwrap().len(),
        2
    );
    assert_eq!(store.session_named("db-fuzz").unwrap().unwrap().id, "s-1");
}
