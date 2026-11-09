//! `leticode_stop` is the bridge's own act: the daemon's goodbye behind it is the close arriving,
//! and a channel that reported it would call a stopped session a broken one.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self};
use std::time::{Duration, Instant};

use letibot_bridge::daemon;
use letibot_bridge::head::Head;
use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_sessionlog::server::serve_registry;
use serde_json::Value;

/// Generous: every wait here is for something a correct build produces in a second or two.
const PATIENCE: Duration = Duration::from_secs(30);

const SESSION: &str = "s-push-stop";

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn kind(note: &Value) -> &str {
    note["params"]["meta"]["event"].as_str().unwrap_or("")
}

/// Wait for something the head's own state says has happened, without guessing at a sleep.
fn until(what: &str, cond: impl Fn() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !cond() {
        assert!(
            Instant::now() < deadline,
            "waited {PATIENCE:?} for {what}, and it did not happen"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_stop_the_bridge_asked_for_is_not_a_failure_on_the_channel() {
    let root = letibot_sessionlog::testing::scratch_root()
        .join(format!("letibot-mcp-push-stop-{}", std::process::id()));
    let _dir = TempDir(root.clone());
    for d in ["config", "data", "worktree", "run"] {
        std::fs::create_dir_all(root.join(d)).expect("dir");
    }
    let socket = root.join("daemon.sock");
    // SAFETY: this binary runs one test, and these are set before the socket or the head exists.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
        std::env::set_var("XDG_DATA_HOME", root.join("data"));
        // The per-session channel is kept under the rundir; a test keeps it under its own root.
        std::env::set_var("XDG_RUNTIME_DIR", root.join("run"));
        std::env::set_var("LETIBOT_SOCKET", &socket);
        std::env::remove_var("LETIBOT_STORE");
    }
    let registry = Registry::new();
    registry
        .create(SESSION, "", SessionWiring::default())
        .expect("a fresh registry takes the session");
    let _server = serve_registry(registry.clone(), &socket).expect("the socket binds");
    let head = Arc::new(
        Head::attach(
            &daemon::resolve(&root.join("worktree")).expect("the daemon resolves"),
            SESSION,
        )
        .expect("the head attaches"),
    );
    let (tx, rx) = mpsc::channel();
    letibot_bridge::push::watch(
        &head,
        std::sync::Arc::new(letibot_bridge::push::Rules::none()),
    )
    .attach(tx);

    // The verb `leticode_stop` calls, and then the proof that the daemon's goodbye did arrive:
    // without the marker this is a `failed: the daemon closed this connection` on the channel.
    head.stop().expect("the stop is sent");
    until(
        "the daemon closed the connection the stop asked it to",
        || head.snapshot().closed.is_some(),
    );

    let mut said: Vec<Value> = Vec::new();
    while let Ok(note) = rx.recv_timeout(Duration::from_secs(2)) {
        said.push(note);
    }
    let failed: Vec<&Value> = said.iter().filter(|n| kind(n) == "failed").collect();
    assert!(
        failed.is_empty(),
        "the bridge's own stop was pushed as a failure: {failed:?}"
    );
}
