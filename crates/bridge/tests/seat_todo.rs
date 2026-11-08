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

/// Something outside letibot - a review on a forge - as a wait: nothing here can see it end, so the
/// check is silent on the row until the seat takes the wait off, which is the row moving.
#[test]
fn a_row_waiting_on_something_outside_is_quiet_until_the_seat_takes_the_wait_off() {
    seat_handle();
    let (_dir, store, folder) = scratch("external");
    let bridge = bridge(&store, &folder);
    let (tx, rx) = mpsc::channel();
    bridge.push_seat(tx);
    call(
        &bridge,
        "todo_write",
        json!({"todos": [{"content": "push once the review lands", "status": "in_progress"}]}),
    )
    .expect("the row is written");
    let said = call(
        &bridge,
        "todo_write",
        json!({"wait_on": [{"content": "push once the review lands",
                "external": "R156's review at arangodb"}]}),
    )
    .expect("the wait is taken");
    assert!(
        said["said"]
            .as_str()
            .unwrap_or_default()
            .contains("the row moves"),
        "the reply says what ends it: {said}"
    );
    assert_eq!(
        rows(&bridge).remove(0).needs,
        vec![TodoNeed::External {
            what: "R156's review at arangodb".into()
        }],
        "the wait is on the row, in the seat's own words"
    );
    assert!(
        nags(&rx, WINDOW * 3).is_empty(),
        "a wait nothing here can see end is not nagged"
    );

    // The review lands and the seat says so: the row is work again, and it is asked about once.
    call(
        &bridge,
        "todo_write",
        json!({"wait_on": [{"content": "push once the review lands", "detach": true}]}),
    )
    .expect("the wait comes off");
    let after = nags(&rx, WINDOW + Duration::from_millis(600));
    assert_eq!(after.len(), 1, "{after:?}");
    assert!(nag_text(&after[0]).contains("push once the review lands"));
}

/// A job the SEAT started, as opposed to one of its packages': its end settles a row that waits on
/// it, and it reaches the seat as ONE `job_ended` - the frame a package's jobs already use.
#[test]
fn a_seat_jobs_end_settles_its_row_and_pushes_one_job_ended() {
    seat_handle();
    let (_dir, store, folder) = scratch("seat-job");
    let bridge = bridge(&store, &folder);
    let (tx, rx) = mpsc::channel();
    bridge.push_seat(tx);
    let started = call(
        &bridge,
        "seat_job_start",
        json!({"argv": ["sleep", "300"], "name": "the gate"}),
    )
    .expect("a job of the seat's own");
    let handle = started["job"].as_str().expect("a handle").to_string();
    assert!(started["log"].as_str().is_some_and(|l| !l.is_empty()));
    call(
        &bridge,
        "todo_write",
        json!({"todos": [{"content": "wait for the gate", "status": "in_progress"}]}),
    )
    .expect("the row is written");
    call(
        &bridge,
        "todo_write",
        json!({"wait_on": [{"content": "wait for the gate", "job": handle.clone()}]}),
    )
    .expect("the seat's own job is a wait it can make");
    assert_eq!(
        rows(&bridge).remove(0).needs,
        vec![TodoNeed::Job {
            handle: handle.clone()
        }],
        "the handle is on the row"
    );
    assert!(
        nags(&rx, WINDOW * 3).is_empty(),
        "a row waiting on the seat's own job is quiet while it runs"
    );

    call(&bridge, "seat_job_kill", json!({"job": handle.clone()})).expect("killed");
    let notes = nags(&rx, WINDOW + Duration::from_millis(900));
    let ended: Vec<&Value> = notes
        .iter()
        .filter(|n| n["params"]["meta"]["event"] == "job_ended")
        .collect();
    assert_eq!(ended.len(), 1, "one settlement, one event: {notes:?}");
    assert_eq!(ended[0]["params"]["meta"]["job"], json!(handle));
    assert_eq!(ended[0]["params"]["meta"]["source"], "seat");
    let nags_after: Vec<&Value> = notes
        .iter()
        .filter(|n| n["params"]["meta"]["event"] == "todo")
        .collect();
    assert_eq!(nags_after.len(), 1, "the row is work again: {notes:?}");
    assert!(nag_text(nags_after[0]).contains("wait for the gate"));

    // And the listing says how it ended, from the store.
    let listed = call(&bridge, "seat_jobs", json!({})).expect("the listing");
    let job = listed["jobs"].as_array().and_then(|j| j.first()).cloned();
    let job = job.expect("the job is listed");
    assert_ne!(job["state"], json!("running"), "{job}");
}

/// A process this host is already running, waited on by its pid: quiet while it is there, settled
/// when it exits - and a pid the number was reused for is a different process.
#[test]
fn a_pid_wait_settles_when_the_process_exits_and_not_when_a_pid_is_reused() {
    seat_handle();
    let (_dir, store, folder) = scratch("pid");
    let bridge = bridge(&store, &folder);
    let (tx, rx) = mpsc::channel();
    bridge.push_seat(tx);
    let mut sleeper = std::process::Command::new("sleep")
        .arg("300")
        .spawn()
        .expect("a process of the test's own");
    let pid = sleeper.id();
    call(
        &bridge,
        "todo_write",
        json!({"todos": [{"content": "the gate somebody ran by hand", "status": "in_progress"}]}),
    )
    .expect("the row is written");
    call(
        &bridge,
        "todo_write",
        json!({"wait_on": [{"content": "the gate somebody ran by hand", "pid": pid}]}),
    )
    .expect("the process is here, so the wait is a wait");
    let start = match rows(&bridge).remove(0).needs.as_slice() {
        [TodoNeed::Pid { pid: p, start }] if *p == pid => *start,
        other => panic!("the edge is a pid wait with its start time: {other:?}"),
    };
    assert!(
        start > 0,
        "the start time is read at the moment it is written"
    );
    assert!(
        nags(&rx, WINDOW * 3).is_empty(),
        "a row waiting on a live process is quiet"
    );

    sleeper.kill().expect("killed");
    sleeper.wait().expect("reaped");
    let after = nags(&rx, WINDOW + Duration::from_millis(900));
    assert_eq!(after.len(), 1, "the exit settles the row: {after:?}");
    assert!(nag_text(&after[0]).contains("the gate somebody ran by hand"));

    // The same NUMBER with another start time is another process, and the row is work at once.
    call(
        &bridge,
        "todo_write",
        json!({"todos": [{"content": "the gate somebody ran by hand", "status": "in_progress",
                "needs": [{"kind": "pid", "pid": pid, "start": 1}]}]}),
    )
    .expect("a pid edge with a start time is taken as given");
    let again = nags(&rx, WINDOW + Duration::from_millis(900));
    assert_eq!(
        again.len(),
        1,
        "a reused pid is not the process the row waited on: {again:?}"
    );
}
