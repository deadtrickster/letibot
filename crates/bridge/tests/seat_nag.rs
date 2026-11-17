//! The seat's plan check as a MONITOR: the bridge's clock decides "due" and `letibot-mcp todo-wait`
//! carries the notice, so a session started without `--dangerously-load-development-channels` is
//! nagged instead of having every channel frame dropped (`crates/bridge/src/seat_nag.rs`).
//!
//! The waiter is the real binary, as the plugin's monitor runs it; the bridge is in this process.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Once, mpsc};
use std::time::{Duration, Instant};

use letibot_bridge::seat_nag;
use letibot_bridge::tools::{self, Bridge};
use letibot_tokencore::store::Store;
use serde_json::{Value, json};

/// The window these tests drive the check with: a minute is the production one, and no test can
/// sleep through it.
const WINDOW: Duration = Duration::from_millis(1000);

/// How long a waiter may take beyond the moment a test expects its answer.
const PATIENCE: Duration = Duration::from_secs(20);

/// The seat every test in this file waits on, and the two directories the waiter and the bridge
/// must name their files from. Process-wide - `rooms::handle` is a `OnceLock` - so this is set once.
fn seat_env() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let root = letibot_sessionlog::testing::scratch_root().join("letibot-seat-nag-env");
        for d in ["run/letibot", "share/letibot"] {
            std::fs::create_dir_all(root.join(d)).expect("dir");
        }
        // SAFETY: called once, before any test in this binary reads any of them - a test binary is
        // one process, so this is the only writer.
        unsafe {
            std::env::set_var(letibot_bridge::rooms::NAME_ENV, "cc@seat-nag");
            std::env::set_var("XDG_RUNTIME_DIR", root.join("run"));
            std::env::set_var("XDG_DATA_HOME", root.join("share"));
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

/// A scratch folder with a store in it, and the folder the seat's bridge serves.
fn scratch(name: &str) -> (TempDir, PathBuf, PathBuf) {
    let root = letibot_sessionlog::testing::scratch_root()
        .join(format!("letibot-seat-nag-{name}-{}", std::process::id()));
    // A killed run's leftover dir is this run's fixture inside a sandbox, where every process has
    // the same pid.
    let _ = std::fs::remove_dir_all(&root);
    let dir = TempDir(root.clone());
    for d in ["config", "data", "worktree"] {
        std::fs::create_dir_all(root.join(d)).expect("dir");
    }
    let store = root.join("sessions.db");
    Store::open(&store).expect("a store for the bridge to read and write");
    (dir, store, root.join("worktree"))
}

/// One bridge over `store`, its seat's check due `WINDOW` after the seat goes quiet, with its
/// channel attached as the MCP server attaches it at `initialize` - until then no watcher runs at
/// all, and a test without it would be asserting nothing.
fn watched(store: &Path, folder: &Path) -> (Bridge, mpsc::Receiver<Value>) {
    let bridge = Bridge::new()
        .with_names(store.to_path_buf(), folder.to_path_buf())
        .with_nag_window(WINDOW);
    let (tx, rx) = mpsc::channel();
    bridge.push_seat(tx);
    (bridge, rx)
}

fn call(bridge: &Bridge, name: &str, args: Value) -> Result<Value, String> {
    tools::call(bridge, name, &args)
}

/// The seat this process is, as the bridge derives it: the store names the folder's files and
/// `rooms::handle` names the seat.
fn seat(store: &Path) -> seat_nag::Nag {
    seat_nag::for_seat(store, &letibot_bridge::rooms::handle())
}

/// The waiter the plugin's monitor starts: the real binary, in the folder the seat's bridge serves.
/// Every other fact - the handle, the runtime directory - is inherited, because it is the one this
/// process's bridge is using.
fn waiter(store: &Path, folder: &Path, timeout: Duration) -> Child {
    Command::new(env!("CARGO_BIN_EXE_letibot-mcp"))
        .arg("todo-wait")
        .arg("--timeout")
        .arg(format!("{}s", timeout.as_secs()))
        .current_dir(folder)
        .env("LETIBOT_STORE", store)
        .env_remove("LETIBOT_SOCKET")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the waiter is the binary the plugin's monitor runs")
}

/// What a waiter said, and how it ended.
struct Said {
    code: Option<i32>,
    out: String,
    err: String,
}

impl Said {
    fn why(&self) -> String {
        format!(
            "exit {:?}, stdout {:?}, stderr {:?}",
            self.code, self.out, self.err
        )
    }
}

/// Wait for the waiter to answer, and kill it rather than hang the suite if it does not.
fn finish(mut child: Child, patience: Duration) -> Said {
    let deadline = Instant::now() + patience;
    let code = loop {
        match child.try_wait().expect("the waiter is a process") {
            Some(status) => break status.code(),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("the waiter was still running after {patience:?}");
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    };
    let mut out = String::new();
    let mut err = String::new();
    if let Some(mut pipe) = child.stdout.take() {
        use std::io::Read;
        let _ = pipe.read_to_string(&mut out);
    }
    if let Some(mut pipe) = child.stderr.take() {
        use std::io::Read;
        let _ = pipe.read_to_string(&mut err);
    }
    Said { code, out, err }
}

/// Block until the waiter holds the seat, so the check cannot come due before anybody waits on it:
/// the fixture decides that order, not the scheduler.
fn awaiting(nag: &seat_nag::Nag) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !nag.claimed() {
        assert!(
            Instant::now() < deadline,
            "the waiter never claimed the seat"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Every note the channel pushed, waiting up to `patience` for the first of them.
fn notes(rx: &mpsc::Receiver<Value>, patience: Duration) -> Vec<Value> {
    let mut out = Vec::new();
    let deadline = Instant::now() + patience;
    while Instant::now() < deadline {
        match rx.recv_timeout(patience.min(Duration::from_millis(50))) {
            Ok(note) => out.push(note),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    out
}

/// A row the seat is on, with no wait on it - the case the check is for - reaches the waiter as the
/// notice the channel frame carries, and the waiter ends 0.
#[test]
fn a_due_nag_reaches_a_waiting_seat_with_the_notices_own_text() {
    seat_env();
    let (_dir, store, folder) = scratch("reported");
    let (bridge, _rx) = watched(&store, &folder);
    let child = waiter(&store, &folder, Duration::from_secs(20));
    awaiting(&seat(&store));

    call(
        &bridge,
        "todo_write",
        json!({"todos": [{"content": "ship the pane", "status": "in_progress"}]}),
    )
    .expect("the write is taken");

    let said = finish(child, PATIENCE);
    assert_eq!(
        said.code,
        Some(0),
        "a notice, not a timeout: {}",
        said.why()
    );
    assert!(
        said.out.starts_with("[todo check]"),
        "the channel frame's own words: {:?}",
        said.out
    );
    assert!(
        said.out.contains("ship the pane"),
        "the notice names the row: {:?}",
        said.out
    );
}

/// The seat either working or waiting on someone who has not answered: nothing for the waiter, so
/// it keeps waiting and ends at its own timeout with nothing on stdout. The seat's check is proved
/// alive afterwards, when the answer lands and the channel - the waiter now gone - says it once.
#[test]
fn a_seat_that_is_working_or_waiting_on_a_person_is_left_alone() {
    seat_env();
    let (_dir, store, folder) = scratch("quiet");
    let (bridge, rx) = watched(&store, &folder);
    let child = waiter(&store, &folder, Duration::from_secs(4));
    awaiting(&seat(&store));
    call(
        &bridge,
        "todo_write",
        json!({"todos": [{"content": "ship the pane", "status": "in_progress"}]}),
    )
    .expect("the write is taken");

    // Working: a tool call every half window is the seat answering, and it postpones the check.
    for _ in 0..4 {
        bridge.touch_seat();
        std::thread::sleep(WINDOW / 2);
    }
    // And now it stops - on a row a person owes an answer on, which is not asked about.
    call(
        &bridge,
        "todo_write",
        json!({"todos": [{"content": "ship the pane", "status": "in_progress", "needs": [
            {"kind": "operator", "question": "which store?", "options": ["one file", "per project"]}]}]}),
    )
    .expect("the write is taken");

    let said = finish(child, PATIENCE);
    assert_eq!(
        said.code,
        Some(30),
        "the waiter ended at its timeout: {}",
        said.why()
    );
    assert!(
        said.out.trim().is_empty(),
        "nothing was due, so nothing is on stdout: {:?}",
        said.out
    );

    // The answer lands: the row is work again, and a check that was alive all along now speaks -
    // through the channel, because the waiter has exited and released the seat.
    call(
        &bridge,
        "todo_answer",
        json!({"content": "ship the pane", "question": "which store?", "reply": "one file"}),
    )
    .expect("the answer is taken");
    let pushed = notes(&rx, WINDOW * 3);
    assert!(
        pushed
            .iter()
            .any(|n| n["params"]["meta"]["event"] == "todo"),
        "the check runs, and with nobody waiting the channel carries it: {pushed:?}"
    );
}

/// Both paths live at once - the channel attached and a waiter holding the seat - and the nag is
/// delivered once, by the monitor. A frame dropped for want of the channels flag is what this
/// design removes; a second copy of the same notice is what it must not add.
#[test]
fn one_nag_is_one_delivery_when_the_channel_and_the_monitor_are_both_live() {
    seat_env();
    let (_dir, store, folder) = scratch("once");
    let (bridge, rx) = watched(&store, &folder);
    let child = waiter(&store, &folder, Duration::from_secs(20));
    awaiting(&seat(&store));

    call(
        &bridge,
        "todo_write",
        json!({"todos": [{"content": "ship the pane", "status": "in_progress"}]}),
    )
    .expect("the write is taken");

    let said = finish(child, PATIENCE);
    assert_eq!(said.code, Some(0), "{}", said.why());
    assert!(
        said.out.starts_with("[todo check]") && said.out.contains("ship the pane"),
        "the monitor carries the notice itself: {:?}",
        said.out
    );

    let pushed = notes(&rx, WINDOW * 2);
    assert!(
        pushed.is_empty(),
        "the waiter held the seat, so the channel said nothing: {pushed:?}"
    );
}
