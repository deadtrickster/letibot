//! The channel is silent about a connection closed by a stop the bridge asked for, and about
//! nothing else: a daemon that goes away under a session nobody stopped is still a failure.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use letibot_bridge::daemon;
use letibot_bridge::head::Head;
use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_sessionlog::server::serve_registry;
use serde_json::Value;

/// Generous: every wait here is for something a correct build produces in a second or two.
const PATIENCE: Duration = Duration::from_secs(30);

const SESSION: &str = "s-push-bye";

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_daemon_that_goes_away_without_being_asked_is_still_a_failure() {
    let root = letibot_sessionlog::testing::scratch_root()
        .join(format!("letibot-mcp-push-bye-{}", std::process::id()));
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

    // The daemon shuts down with this session in it, and nobody asked it to: `Bye` on the
    // connection is the whole of what the head is told, and it is news.
    registry.close();

    let deadline = Instant::now() + PATIENCE;
    let mut said: Vec<Value> = Vec::new();
    while !said
        .iter()
        .any(|n| n["params"]["meta"]["event"] == "failed")
    {
        assert!(
            Instant::now() < deadline,
            "a daemon that went away was not reported on the channel: {said:?}"
        );
        if let Ok(note) = rx.recv_timeout(Duration::from_secs(1)) {
            said.push(note);
        }
    }
}
