//! A Claude Code seat's board, read out of the daemon's store for the head's pane (R144,
//! `docs/seat-todo.md`). The seat writes `seat_todo` in another process; this is the daemon's half:
//! the same store, the same conversion a session's rows take, on the frame the pane already reads.

use letibot_harnessd::config::Config;
use letibot_harnessd::sessions::StoreSessions;
use letibot_sessionlog::event::{TodoNeed, TodoStatus};
use letibot_sessionlog::registry::SessionSource;
use letibot_tokencore::store::{Store, TodoBy, TodoItem, TodoStatus as StoredStatus};

/// A store of its own, named for the process and the moment so a run left behind cannot be read by
/// the next one.
fn store_path(tag: &str) -> std::path::PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "harnessd-seat-board-{tag}-{}-{n}.db",
        std::process::id()
    ))
}

fn row(content: &str, needs: Vec<letibot_tokencore::store::TodoNeed>) -> TodoItem {
    TodoItem {
        content: content.into(),
        status: StoredStatus::InProgress,
        by: TodoBy::Model,
        when: None,
        needs,
    }
}

/// **The daemon's source hands a seat's rows to the head**, with the wait on a package intact: the
/// `Child` edge is what the pane draws, and a store read that dropped it would leave a seat's row
/// reading as one waiting on nothing.
#[test]
fn the_daemon_serves_a_seats_board_out_of_its_store() {
    let path = store_path("rows");
    let _clean = Clean(path.clone());
    {
        let s = Store::open(&path).unwrap();
        s.put_seat_todos(
            "cc@proj",
            &[row(
                "collect the package",
                vec![letibot_tokencore::store::TodoNeed::Child {
                    id: "s-1789462738453908838".into(),
                }],
            )],
        )
        .unwrap();
    }
    let mut cfg = Config::for_this_box("/tmp");
    cfg.store = Some(path.clone());
    cfg.session_id = "s-1".into();
    let src = StoreSessions::open(&cfg).expect("the store opens");

    let boards = src.seat_boards();
    assert_eq!(boards.len(), 1, "one seat in the store: {boards:?}");
    assert_eq!(boards[0].seat, "cc@proj");
    assert_eq!(boards[0].todos.len(), 1);
    assert_eq!(boards[0].todos[0].content, "collect the package");
    assert_eq!(boards[0].todos[0].status, TodoStatus::InProgress);
    assert_eq!(
        boards[0].todos[0].needs,
        vec![TodoNeed::Child {
            id: "s-1789462738453908838".into()
        }],
        "the wait on the package reached the wire's own shape"
    );
    // A seat is not a session: the session's own read does not answer with its board, which is
    // the store's argument for a table of its own.
    assert!(src.todos("cc@proj").is_empty());
}

/// A store with no seat table filled - and one with no store at all - answers no boards rather
/// than failing: the pane of a daemon nobody has written a plan into is the pane that always was.
#[test]
fn a_daemon_whose_store_holds_no_seat_answers_none() {
    let path = store_path("empty");
    let _clean = Clean(path.clone());
    Store::open(&path).unwrap();
    let mut cfg = Config::for_this_box("/tmp");
    cfg.store = Some(path.clone());
    let src = StoreSessions::open(&cfg).expect("the store opens");
    assert!(src.seat_boards().is_empty());

    // And no store at all is `None`, which the registry reads as no boards.
    let mut bare = Config::for_this_box("/tmp");
    bare.store = None;
    assert!(StoreSessions::open(&bare).is_none());
}

/// The store file goes with the test, so a re-run does not read the last one's board.
struct Clean(std::path::PathBuf);

impl Drop for Clean {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
