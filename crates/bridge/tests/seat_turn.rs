//! A wait on a package's turn end is met when that turn ends - the seat's `child` edge, over a real
//! socket and `Registry`, with the idle check speaking through the channel push. One test, one
//! process: `LETIBOT_SOCKET` and the seat's handle are both process-wide.

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use letibot_bridge::tools::{self, Bridge};
use letibot_sessionlog::hub::Hub;
use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_sessionlog::server::serve_registry;
use letibot_sessionlog::{SessionEvent, activity::Activity, activity::Waiting, testing};
use letibot_tokencore::store::Store;
use serde_json::json;

const SESSION: &str = "s-seat-package";
const WINDOW: Duration = Duration::from_millis(200);

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// One turn as the daemon publishes it: started, the model saying one thing, ended, and quiet.
fn turn(hub: &Hub, start: bool) {
    if start {
        hub.publish(SessionEvent::ActivityChanged {
            activity: Activity::of(vec![Waiting::Turn]),
        });
        hub.publish(testing::turn_started("t1"));
        return;
    }
    hub.publish(testing::turn_finished("t1"));
    hub.publish(SessionEvent::ActivityChanged {
        activity: Activity::of(Vec::new()),
    });
}

fn nag_text(note: &serde_json::Value) -> String {
    note["params"]["content"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

#[test]
fn a_wait_on_a_packages_turn_is_met_when_that_turn_ends() {
    let root = letibot_sessionlog::testing::scratch_root()
        .join(format!("letibot-seat-turn-{}", std::process::id()));
    let _dir = TempDir(root.clone());
    for d in ["config", "data", "worktree"] {
        std::fs::create_dir_all(root.join(d)).expect("dir");
    }
    let socket = root.join("daemon.sock");
    let store = root.join("sessions.db");
    Store::open(&store).expect("the daemon's store");
    // SAFETY: first thing in the only test of this file, before any of it is read.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
        std::env::set_var("XDG_DATA_HOME", root.join("data"));
        std::env::set_var("LETIBOT_SOCKET", &socket);
        std::env::set_var(letibot_bridge::rooms::NAME_ENV, "cc@seat-turn");
    }
    let registry = Registry::new();
    registry
        .create(SESSION, "", SessionWiring::default())
        .expect("a fresh registry takes the session");
    let _server = serve_registry(registry.clone(), &socket).expect("the socket binds");
    let hub = registry.get(SESSION).expect("the session's hub");

    let bridge = Bridge::new()
        .with_names(store.clone(), root.join("worktree"))
        .with_nag_window(WINDOW);
    // Watched, not driven: a package is somebody else's session and the seat only reads it.
    bridge
        .attach(&root.join("worktree").display().to_string(), SESSION, true)
        .expect("the bridge holds the package");
    let (tx, rx) = mpsc::channel();
    bridge.push_seat(tx);

    // The turn is in flight BEFORE the row is written, so the wait is a real one: a row written
    // against an idle package would be waiting on nothing and the check would speak at once.
    turn(&hub, true);
    std::thread::sleep(Duration::from_millis(300));
    let wrote = tools::call(
        &bridge,
        "todo_write",
        &json!({"todos": [{"content": "collect the package — its turn end is the wait",
                           "status": "in_progress",
                           "needs": [{"kind": "child", "id": SESSION}]}]}),
    )
    .expect("the wait names a session this bridge holds");
    assert_eq!(wrote["stored"], json!(true), "{wrote}");

    // While the package's turn is in flight the row is out of the plan's hands, and the check says
    // nothing about it - not one sentence, not a shorter one.
    let quiet = Instant::now();
    while quiet.elapsed() < WINDOW * 3 {
        if let Ok(note) = rx.recv_timeout(Duration::from_millis(50)) {
            panic!("a row waiting on a working package was nagged: {note}");
        }
    }

    // The turn ends. That is the wait: the row is askable, and the check names it.
    turn(&hub, false);
    let deadline = Instant::now() + WINDOW + Duration::from_secs(5);
    let mut said = None;
    while Instant::now() < deadline && said.is_none() {
        if let Ok(note) = rx.recv_timeout(Duration::from_millis(50)) {
            said = Some(nag_text(&note));
        }
    }
    let said = said.expect("a met wait is work, and the idle check says so");
    assert!(
        said.contains("collect the package"),
        "the notice names the row: {said}"
    );
}
