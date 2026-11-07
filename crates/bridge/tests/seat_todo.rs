//! A Claude Code seat's todo board (`docs/seat-todo.md`): the session's rows and waits kept under
//! the seat's handle, and the session's idle check speaking through the channel push. One process,
//! one seat: the handle is process-wide, as the room's is.

use std::path::PathBuf;
use std::sync::{Once, mpsc};
use std::time::{Duration, Instant};

use letibot_bridge::tools::{self, Bridge};
use letibot_tokencore::store::{Store, TodoItem, TodoNeed};
use serde_json::{Value, json};

/// The window these tests drive the check with: a minute is the production one, and no test can
/// sleep through it.
const WINDOW: Duration = Duration::from_millis(200);

/// The handle every test in this file writes under. Process-wide, so it is set once and never
/// again: `set_var` from two test threads at once is the race `handle` would read.
fn seat_handle() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // SAFETY: called before any test reads the handle, and only once in this process.
        unsafe {
            std::env::set_var(letibot_bridge::rooms::NAME_ENV, "cc@seat-todo");
            std::env::remove_var("LETIBOT_STORE");
            std::env::remove_var("LETIBOT_SOCKET");
        }
    });
}

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A scratch folder with a store in it, and the env a bridge reads its folder and store from.
fn scratch(name: &str) -> (TempDir, PathBuf, PathBuf) {
    let root = letibot_sessionlog::testing::scratch_root()
        .join(format!("letibot-seat-todo-{name}-{}", std::process::id()));
    let dir = TempDir(root.clone());
    for d in ["config", "data", "worktree"] {
        std::fs::create_dir_all(root.join(d)).expect("dir");
    }
    let store = root.join("sessions.db");
    // The store is the daemon's file and the bridge never makes one: the fixture makes it, as a
    // daemon's first start does.
    Store::open(&store).expect("a store for the bridge to write into");
    (dir, store, root.join("worktree"))
}

/// One bridge over `store`, its seat's check due `WINDOW` after the seat goes quiet.
fn bridge(store: &std::path::Path, folder: &std::path::Path) -> Bridge {
    Bridge::new()
        .with_names(store.to_path_buf(), folder.to_path_buf())
        .with_nag_window(WINDOW)
}

/// A call through the MCP tool table - the same door a client's `tools/call` takes.
fn call(bridge: &Bridge, name: &str, args: Value) -> Result<Value, String> {
    tools::call(bridge, name, &args)
}

fn rows(bridge: &Bridge) -> Vec<TodoItem> {
    let answer = call(bridge, "todos", json!({})).expect("the board reads");
    serde_json::from_value(answer["rows"].clone()).expect("rows are the store's own type")
}

/// Every nag the watcher pushed, waiting up to `patience` for the first of them.
fn nags(rx: &mpsc::Receiver<Value>, patience: Duration) -> Vec<Value> {
    let mut out = Vec::new();
    let deadline = Instant::now() + patience;
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(note) => out.push(note),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    out
}

fn nag_text(note: &Value) -> String {
    note["params"]["content"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

#[test]
fn a_seats_rows_are_written_by_the_session_tool_and_read_back_across_a_restart() {
    seat_handle();
    let (_dir, store, folder) = scratch("restart");
    let first = bridge(&store, &folder);
    let wrote = call(
        &first,
        "todo_write",
        json!({"todos": [
            {"content": "read the design", "status": "completed"},
            {"content": "collect the package — its turn end is the wait",
             "status": "in_progress",
             "needs": [{"kind": "child", "id": "s-package"}]}
        ]}),
    )
    .expect("the write is taken");
    assert_eq!(wrote["stored"], json!(true), "{wrote}");
    assert_eq!(rows(&first).len(), 2);
    drop(first);

    // A second connection to the file - what the next bridge process opens - reads the rows with
    // their edge, so the board outlived the process rather than the store's shared connection.
    let next_process = Store::open_private(&store).expect("a connection of its own");
    let stored = next_process.seat_todos("cc@seat-todo").expect("the rows");
    assert_eq!(stored.len(), 2, "{stored:?}");
    assert_eq!(
        stored[1].needs,
        vec![TodoNeed::Child {
            id: "s-package".into()
        }],
        "a wait is half of what a row is, and it is on disk with it"
    );

    let second = bridge(&store, &folder);
    let back = rows(&second);
    assert_eq!(back.len(), 2, "a restarted bridge reads its own board back");
    assert_eq!(back[0].content, "read the design");
    assert!(
        back[0].status == letibot_tokencore::store::TodoStatus::Completed,
        "and its statuses"
    );
}

#[test]
fn an_in_progress_row_with_no_wait_is_nagged_once_and_not_again_without_a_change() {
    seat_handle();
    let (_dir, store, folder) = scratch("nag");
    let bridge = bridge(&store, &folder);
    let (tx, rx) = mpsc::channel();
    bridge.push_seat(tx);
    call(
        &bridge,
        "todo_write",
        json!({"todos": [{"content": "ship the pane", "status": "in_progress"}]}),
    )
    .expect("the write is taken");

    // Nothing before the window: the seat has just spoken.
    assert!(
        nags(&rx, WINDOW / 2).is_empty(),
        "the check may not speak before the seat has been quiet for the window"
    );
    let first = nags(&rx, WINDOW + Duration::from_millis(600));
    assert_eq!(first.len(), 1, "one nag, once: {first:?}");
    assert_eq!(first[0]["method"], "notifications/claude/channel");
    assert_eq!(first[0]["params"]["meta"]["event"], "todo");
    assert!(
        nag_text(&first[0]).contains("ship the pane"),
        "the notice names the row: {:?}",
        nag_text(&first[0])
    );

    // A change of nothing is not a change: the same board says nothing again.
    assert!(
        nags(&rx, WINDOW * 3).is_empty(),
        "an unchanged board is not nagged a second time"
    );

    // The board moves - the row is done - and the check has nothing to say any more.
    call(
        &bridge,
        "todo_write",
        json!({"update": [{"content": "ship the pane", "status": "completed"}]}),
    )
    .expect("the update is taken");
    assert!(
        nags(&rx, WINDOW * 2).is_empty(),
        "a finished plan is silent"
    );
}

#[test]
fn a_row_waiting_on_the_operator_is_quiet_until_the_answer_lands() {
    seat_handle();
    let (_dir, store, folder) = scratch("operator");
    let bridge = bridge(&store, &folder);
    let (tx, rx) = mpsc::channel();
    bridge.push_seat(tx);
    call(
        &bridge,
        "todo_write",
        json!({"todos": [{"content": "pick the store", "status": "in_progress", "needs": [
            {"kind": "operator", "question": "sqlite or one file per project?",
             "options": ["sqlite", "per project"]}]}]}),
    )
    .expect("the write is taken");
    assert!(
        nags(&rx, WINDOW * 3).is_empty(),
        "a row waiting on a person is not nagged while they have not answered"
    );

    let answered = call(
        &bridge,
        "todo_answer",
        json!({"content": "pick the store",
               "question": "sqlite or one file per project?",
               "reply": "sqlite, one file"}),
    )
    .expect("the answer is taken");
    assert_eq!(answered["stored"], json!(true), "{answered}");
    let row = rows(&bridge).remove(0);
    assert!(
        row.needs.iter().any(|n| matches!(
            n,
            TodoNeed::Answered { reply, .. } if reply == "sqlite, one file"
        )),
        "the answer is on the row: {:?}",
        row.needs
    );

    // And the row is work again, so the check speaks about it once.
    let after = nags(&rx, WINDOW + Duration::from_millis(600));
    assert_eq!(after.len(), 1, "{after:?}");
    assert!(nag_text(&after[0]).contains("pick the store"));
}

#[test]
fn a_wait_on_a_session_this_bridge_does_not_hold_is_refused_by_name() {
    seat_handle();
    let (_dir, store, folder) = scratch("refuse");
    let bridge = bridge(&store, &folder);
    call(
        &bridge,
        "todo_write",
        json!({"todos": [{"content": "collect", "status": "pending"}]}),
    )
    .expect("the row is written");
    // `wait_on` and not a `needs` entry: a full session id in `needs` is taken as it stands (the
    // daemon's rule, because a child may be one from before a restart), so the door that checks
    // whether this runtime can ever end the wait is `wait_on`'s `child`.
    let refused = call(
        &bridge,
        "todo_write",
        json!({"wait_on": [{"content": "collect", "child": "s-nobody"}]}),
    )
    .expect_err("a wait nothing here could end");
    assert!(
        refused.contains("not a package of this seat"),
        "the refusal says why: {refused}"
    );
    assert!(
        rows(&bridge)[0].needs.is_empty(),
        "the row still waits on nothing"
    );
}
