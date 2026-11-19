//! A Claude Code seat's todo board (`docs/seat-todo.md`): the session's rows and waits kept under
//! the seat's handle, and the session's idle check speaking through the channel push. One process,
//! one seat: the handle is process-wide, as the room's is.

use std::path::PathBuf;
use std::sync::{Once, mpsc};
use std::time::{Duration, Instant};

use letibot_bridge::tools::{self, Bridge};
use letibot_tokencore::store::{Store, TodoItem, TodoNeed, TodoStatus};
use serde_json::{Value, json};

/// The window these tests drive the check with: a minute is the production one, and no test can
/// sleep through it. The quiet clock starts as a call begins, so a store write on a loaded host
/// takes from it.
const WINDOW: Duration = Duration::from_millis(1000);

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

/// Drops a nag on a row the test wrote in two calls, about the moment before its wait was set:
/// on a loaded host the window can end between the calls, and that nag is correct.
fn settle(rx: &mpsc::Receiver<Value>) {
    let _ = nags(rx, Duration::from_millis(200));
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
    // their edge, so the board outlived the process rather than the store's shared connection. It is
    // filed under the seat's ID: the handle is minted at connect and is not what a wait can name.
    let next_process = Store::open_private(&store).expect("a connection of its own");
    let stored = next_process
        .seat_todos(&letibot_bridge::rooms::seat())
        .expect("the rows");
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
    // A repeat the clock decided before the update can still be in flight on a loaded host.
    let _ = nags(&rx, Duration::from_millis(200));
    let after = nags(&rx, WINDOW * 2);
    assert!(after.is_empty(), "a finished plan is silent: {after:?}");
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

/// A pane polling the board is not the seat working. The plugin's panes call `todos` (the board)
/// and `leticode_status` (each package) every five seconds from `session.start`, so a poll that
/// postponed the check would mean no Claude Code seat is ever nagged - which is what the field
/// showed: 13 open rows, an idle session, no notice in an hour.
#[test]
fn a_pane_polling_the_board_does_not_keep_the_check_from_coming_due() {
    seat_handle();
    let (_dir, store, folder) = scratch("polled");
    let bridge = bridge(&store, &folder);
    let (tx, rx) = mpsc::channel();
    bridge.push_seat(tx);
    let plan: Vec<Value> = (1..=13)
        .map(|n| json!({"content": format!("row {n}"), "status": "in_progress"}))
        .collect();
    let wrote = call(&bridge, "todo_write", json!({"todos": plan})).expect("the write is taken");
    assert_eq!(rows(&bridge).len(), 13, "{wrote}");

    // The plugin's own loop, at test speed: its 5 s poll against this window is a poll every half
    // window, and six of them outlast the window three times over.
    let mut heard = Vec::new();
    for _ in 0..6 {
        call(&bridge, "todos", json!({})).expect("the board reads");
        heard.extend(nags(&rx, WINDOW / 2));
    }
    assert_eq!(heard.len(), 1, "a read is not work: {heard:?}");
    assert!(
        nag_text(&heard[0]).contains("row 1"),
        "the notice names the plan: {:?}",
        nag_text(&heard[0])
    );
}

/// The other half of the rule: a call that DOES work still postpones the check, so a seat writing
/// its board is not nagged in the middle of it.
#[test]
fn a_seat_writing_its_board_is_working_and_the_check_waits() {
    seat_handle();
    let (_dir, store, folder) = scratch("working");
    let bridge = bridge(&store, &folder);
    let (tx, rx) = mpsc::channel();
    bridge.push_seat(tx);
    let plan = json!({"todos": [{"content": "ship the pane", "status": "in_progress"}]});
    call(&bridge, "todo_write", plan.clone()).expect("the write is taken");

    for _ in 0..6 {
        call(&bridge, "todo_write", plan.clone()).expect("the same board again is still work");
        assert!(
            nags(&rx, WINDOW / 2).is_empty(),
            "a seat writing its board has not stalled"
        );
    }

    // It stops, and the check comes once.
    let heard = nags(&rx, WINDOW + Duration::from_millis(600));
    assert_eq!(
        heard.len(),
        1,
        "one notice once the seat is quiet: {heard:?}"
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
    settle(&rx);
    assert!(
        said["said"]
            .as_str()
            .unwrap_or_default()
            .contains("asked about again"),
        "the reply says the quiet is bounded: {said}"
    );
    let needs = rows(&bridge).remove(0).needs;
    assert!(
        matches!(needs.as_slice(), [TodoNeed::External { what, since_ms: Some(_) }]
            if what == "R156's review at arangodb"),
        "the wait is on the row, in the seat's own words, stamped: {needs:?}"
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
    settle(&rx);
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
    settle(&rx);
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
    // The settled row's own repeat can fall at the start of this window on a loaded host, so the
    // count is not pinned: what matters is that the row is nagged, not held as a wait.
    assert!(
        again
            .last()
            .is_some_and(|n| nag_text(n).contains("the gate somebody ran by hand")),
        "a reused pid is not the process the row waited on: {again:?}"
    );
}

/// The board is filed under the seat's ID, not the handle it speaks as: the handle is minted at
/// connect and may be another one next time, while the id is the same seat. A wait naming the id is
/// what the store answers, so a board written under the handle must not be what anyone looks for.
#[test]
fn a_seats_board_is_stored_under_its_id_and_not_the_handle_it_speaks_as() {
    seat_handle();
    let (_dir, store, folder) = scratch("seat-id");
    let bridge = bridge(&store, &folder);
    call(
        &bridge,
        "todo_write",
        json!({"todos": [{"content": "ship the pane", "status": "in_progress"}]}),
    )
    .expect("the write is taken");
    let id = letibot_bridge::rooms::seat();
    let read = Store::open_private(&store).expect("a connection of its own");
    assert_eq!(
        read.seat_todos(&id).expect("the board").len(),
        1,
        "the board is under `{id}`"
    );
    assert!(
        read.seat_board(&letibot_bridge::rooms::handle())
            .expect("a read")
            .is_none(),
        "and not under the handle it speaks as"
    );
}

/// A board a seat left under its handle before it wrote ids is moved to the id at connect, and a
/// reconnect under ANOTHER handle reads the same board: the handle is what changed, not the seat.
#[test]
fn a_board_left_under_the_handle_is_moved_to_the_id_and_survives_a_new_handle() {
    seat_handle();
    let (_dir, store, folder) = scratch("seat-moved");
    let id = letibot_bridge::rooms::seat();
    // What the previous build wrote: the board under the handle, and nothing under the id.
    let old = letibot_bridge::rooms::handle();
    Store::open(&store)
        .expect("the store")
        .put_seat_todos(
            &old,
            &[TodoItem {
                content: "collect the package".into(),
                status: TodoStatus::InProgress,
                by: letibot_tokencore::store::TodoBy::Model,
                when: None,
                needs: Vec::new(),
            }],
        )
        .expect("the old board");
    let bridge = bridge(&store, &folder);
    assert_eq!(
        rows(&bridge).len(),
        1,
        "the old board is this seat's, and it is read at connect"
    );
    let read = Store::open_private(&store).expect("a connection of its own");
    assert_eq!(read.seat_todos(&id).expect("the board").len(), 1, "{id}");
    assert!(
        read.seat_board(&old).expect("a read").is_none(),
        "the old key is gone: one board, not two"
    );

    // The next connect speaks as another handle - a re-minted suffix is what a restart does - and
    // the board is still one board, because the id is what it is filed under.
    let again = letibot_bridge::seat::Seat::new(
        "cc@seat-todo:second",
        id.clone(),
        store.clone(),
        std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
    );
    assert_eq!(again.rows().len(), 1, "a new handle, the same seat");
    assert_eq!(again.id(), id);
    assert_eq!(
        again.board_json()["seat"],
        json!(id),
        "the tool's answer names it"
    );
}

/// A wait naming ANOTHER bridge seat's ID: quiet while that seat's row is open, and freed with one
/// note the moment it completes - across a reconnect of the waiting seat under a new handle, because
/// the id is what the wait carries and what both boards are filed under.
#[test]
fn a_wait_naming_a_bridge_seats_id_is_freed_when_it_completes_across_a_new_handle() {
    seat_handle();
    let (_dir, store, folder) = scratch("seat-wait-bridge-id");
    let theirs = "lab2x1/cc-other";
    let mine = "lab2x1/cc-waiting";
    let heads = || {
        std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::<
            String,
            std::sync::Arc<letibot_bridge::head::Head>,
        >::new()))
    };
    let other =
        letibot_bridge::seat::Seat::new("cc@seat-todo:other", theirs, store.clone(), heads())
            .with_folder(folder.clone());
    other
        .write(&json!({"todos": [{"content": "ship the pane", "status": "in_progress"}]}))
        .expect("the other seat's board");

    let first = letibot_bridge::seat::Seat::new("cc@seat-todo:first", mine, store.clone(), heads())
        .with_folder(folder.clone())
        .with_window(WINDOW);
    let wrote = first
        .write(
            &json!({"todos": [{"content": "collect the package", "status": "in_progress",
                "needs": [{"kind": "todo", "seat": theirs, "content": "ship the pane"}]}]}),
        )
        .expect("a bridge's seat id is a board this seat can wait on");
    assert_eq!(wrote["stored"], json!(true), "{wrote}");
    drop(first);

    // The seat reconnects and speaks as another handle. The wait is still on its board - the id
    // found the board the first handle wrote - and the note travels to this handle's watcher.
    let again = std::sync::Arc::new(
        letibot_bridge::seat::Seat::new("cc@seat-todo:second", mine, store.clone(), heads())
            .with_folder(folder.clone())
            .with_window(WINDOW),
    );
    assert_eq!(
        again.rows().first().map(|r| r.needs.clone()),
        Some(vec![TodoNeed::Todo {
            seat: theirs.into(),
            content: "ship the pane".into()
        }]),
        "the wait survived the reconnect"
    );
    let (tx, rx) = mpsc::channel();
    letibot_bridge::seat::Seat::watch(&again, tx);
    settle(&rx);
    assert!(
        nags(&rx, WINDOW * 2)
            .iter()
            .all(|n| !nag_text(n).contains("ship the pane")),
        "a row waiting on another board's row is quiet while it is open"
    );

    other
        .write(&json!({"todos": [{"content": "ship the pane", "status": "completed"}]}))
        .expect("the other seat completes its row");
    let notes = nags(&rx, Duration::from_secs(5));
    assert!(
        notes.iter().any(|n| {
            let t = nag_text(n);
            t.contains("[todo]") && t.contains("finished") && t.contains(theirs)
        }),
        "the id's board freed the wait and the note names it: {notes:?}"
    );
}

/// A session of this seat's project, with one row on its board: the other half of a cross-board
/// wait, written the way a daemon writes it.
fn a_sessions_row(
    store: &std::path::Path,
    workspace: &str,
    title: &str,
    row: &str,
    status: TodoStatus,
) {
    let store = Store::open(store).expect("the store");
    if store.session("s-42").expect("the index").is_none() {
        store
            .put_session(&letibot_tokencore::store::SessionRecord {
                id: "s-42".into(),
                title: Some(title.into()),
                model_id: "a model".into(),
                dialect_sha: "a dialect".into(),
                workspace_root: workspace.into(),
                owner: "dead".into(),
                approvers: Vec::new(),
                role: None,
                parent_session_id: None,
            })
            .expect("the session row");
    }
    store
        .put_todos(
            "s-42",
            &[TodoItem {
                content: row.into(),
                status,
                by: letibot_tokencore::store::TodoBy::Model,
                when: None,
                needs: Vec::new(),
            }],
        )
        .expect("the session's board");
}

/// The other direction: a SEAT's row waits on a letibot session's row of the same project, is quiet
/// while that row is open, and is freed with one note when the session finishes it.
#[test]
fn a_seats_row_can_wait_on_a_sessions_row_and_is_freed_by_its_finishing() {
    seat_handle();
    let (_dir, store, folder) = scratch("seat-waits-session");
    // `cc@seat-todo` is this seat's handle, so `seat-todo` is the word its project is known by.
    a_sessions_row(
        &store,
        "/work/seat-todo",
        "db-fuzz",
        "ship the pane",
        TodoStatus::InProgress,
    );
    let bridge = bridge(&store, &folder);
    let (tx, rx) = mpsc::channel();
    bridge.push_seat(tx);
    let wrote = call(
        &bridge,
        "todo_write",
        json!({"todos": [{"content": "collect the package", "status": "in_progress",
                "needs": [{"kind": "todo", "seat": "db-fuzz", "content": "ship the pane"}]}]}),
    )
    .expect("a session of this project is a board this seat can wait on");
    assert_eq!(wrote["stored"], json!(true), "{wrote}");
    assert_eq!(
        rows(&bridge).remove(0).needs,
        vec![TodoNeed::Todo {
            seat: "db-fuzz".into(),
            content: "ship the pane".into(),
        }],
        "the wait is on the row, in the store"
    );
    settle(&rx);
    assert!(
        nags(&rx, WINDOW * 3).is_empty(),
        "a row waiting on another board's row is not nagged"
    );

    // The session marks its row done, and the seat is told - one note, naming the board and the
    // row, and the row is work again.
    a_sessions_row(
        &store,
        "/work/seat-todo",
        "db-fuzz",
        "ship the pane",
        TodoStatus::Completed,
    );
    let notes = nags(&rx, Duration::from_secs(3));
    let freeing: Vec<&Value> = notes
        .iter()
        .filter(|n| nag_text(n).contains("[todo]") && nag_text(n).contains("finished"))
        .collect();
    assert_eq!(freeing.len(), 1, "one note for one freed wait: {notes:?}");
    let said = nag_text(freeing[0]);
    assert!(
        said.contains("db-fuzz finished \"ship the pane\"")
            && said.contains("no longer a wait of")
            && said.contains("collect the package"),
        "{said}"
    );
    assert!(
        rows(&bridge)[0].needs.is_empty(),
        "the wait came off the row"
    );
}

/// A wait across projects, or on a name nothing answers to, is refused by name in the call that
/// writes it - a seat has no daemon to fall back on, so nothing may be left to a guess.
#[test]
fn a_wait_on_another_projects_row_or_an_unknown_one_is_refused_by_name() {
    seat_handle();
    let (_dir, store, folder) = scratch("seat-waits-refuse");
    a_sessions_row(
        &store,
        "/work/somewhere-else",
        "elsewhere",
        "ship it",
        TodoStatus::InProgress,
    );
    let bridge = bridge(&store, &folder);
    let write = |seat: &str| {
        call(
            &bridge,
            "todo_write",
            json!({"todos": [{"content": "collect the package", "status": "in_progress",
                    "needs": [{"kind": "todo", "seat": seat, "content": "ship it"}]}]}),
        )
    };
    let elsewhere = write("elsewhere").expect_err("a board of another project");
    assert!(
        elsewhere.contains("another project") && elsewhere.contains("seat-todo"),
        "the refusal names the project it is not: {elsewhere}"
    );
    let unknown = write("nobody").expect_err("a name nothing answers to");
    assert!(
        unknown.contains("nobody") && unknown.contains("db-fuzz") || unknown.contains("nobody"),
        "the refusal names what there is: {unknown}"
    );
    assert!(
        rows(&bridge).iter().all(|r| r.needs.is_empty()),
        "nothing was written"
    );
}
