//! `leticode_start` on a session whose tools run somewhere else queues the model, the mode and the
//! brief and answers at once: a placed session's worker opens a remote tree or boots a guest first,
//! which on a loaded box is minutes, and a start that waited for it gave up with all three unsent.
//!
//! No worker is started here at all, which is that wait taken to its limit: the frames are queued on
//! the session and nothing applies them, and the point is that the start does not hang.

use std::path::{Path, PathBuf};

use letibot_bridge::tools::{self, Bridge};
use letibot_sessionlog::hub::CommandKind;
use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_sessionlog::server::serve_registry;
use serde_json::json;

const BRIEF: &str = "brief-marker: say one line";
const MODEL: &str = "deepseek/deepseek-chat";

/// A scratch directory that removes itself.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = letibot_sessionlog::testing::scratch_root()
            .join(format!("letibot-mcp-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&path).expect("temp dir");
        TempDir(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_start_on_a_placed_session_queues_the_model_the_mode_and_the_brief() {
    let dir = TempDir::new("vm-start-queues");
    let root = dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("config")).expect("config dir");
    std::fs::create_dir_all(root.join("data")).expect("data dir");
    let worktree = root.join("worktree");
    std::fs::create_dir_all(&worktree).expect("worktree");
    let socket = root.join("daemon.sock");
    // SAFETY: this file runs one test, and these are set before the socket or a head exists.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
        std::env::set_var("XDG_DATA_HOME", root.join("data"));
        std::env::set_var("LETIBOT_SOCKET", &socket);
        std::env::remove_var("LETIBOT_STORE");
        std::env::remove_var("LETIBOT_HOSTS");
    }
    // A daemon holding nothing and serving frames, with no worker behind it: every command a head
    // sends is queued on its session and stays queued, which is *the VM is still booting* seen from
    // the bridge's side.
    let registry = Registry::new();
    registry
        .create("s-daemons-own", "", SessionWiring::default())
        .expect("a fresh registry takes a session");
    // The record a joining bridge reads: without it the daemon is placed nowhere it can see, and
    // `leticode_start` refuses to join rather than guess where its tools run.
    let _server = serve_registry(registry.clone(), &socket).expect("the socket binds");
    letibot_sessionlog::placement::write(&socket, "firecode").expect("the placement record");
    let bridge = Bridge::new();

    let began = std::time::Instant::now();
    let started = tools::call(
        &bridge,
        "leticode_start",
        &json!({
            "worktree": worktree.display().to_string(),
            "brief": BRIEF,
            "model": MODEL,
            "mode": "allow-all",
            "vm": true,
        }),
    )
    .expect("a start on a session whose VM is still booting");
    // The bar: it answers rather than waiting out a boot. A worker that never opens is the worst
    // case of one that opens in minutes, and the wait this replaces was two minutes.
    assert!(
        began.elapsed() < std::time::Duration::from_secs(30),
        "the start waited {:?} for a session nothing is opening",
        began.elapsed()
    );

    let session = started["session"]
        .as_str()
        .expect("a session id")
        .to_string();
    assert_eq!(started["placement"], json!("firecode"), "{started}");
    assert_eq!(started["model"], json!(MODEL), "{started}");
    assert_eq!(started["model_queued"], json!(true), "{started}");
    let queued: Vec<&str> = started["queued"]
        .as_array()
        .expect("the steps that are queued")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(
        queued,
        vec!["model", "mode", "brief"],
        "the order the daemon applies them in, and the three the row names: {started}"
    );
    let state = started["state"].as_str().expect("a sentence");
    assert!(state.contains("queued until the VM is up"), "{started}");

    // And the frames really are on the session, in that order: the daemon applies one session's
    // commands in the order they arrived, which is what makes a queued model still the model the
    // brief runs on. They arrive after the answer, because the answer does not wait for the daemon.
    let hub = registry
        .get(&session)
        .expect("the session the start created");
    let began = std::time::Instant::now();
    while hub.census().commands < 3 {
        assert!(
            began.elapsed() < std::time::Duration::from_secs(10),
            "the daemon took {} of the three frames",
            hub.census().commands
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let mut order: Vec<String> = Vec::new();
    while let Some(cmd) = hub.try_command() {
        order.push(match cmd.kind {
            CommandKind::Slash { line } => format!("slash:{line}"),
            CommandKind::Mode { name, .. } => format!("mode:{name}"),
            CommandKind::Prompt { .. } => "prompt".to_string(),
            other => format!("{other:?}"),
        });
    }
    assert_eq!(
        order,
        vec![
            format!("slash:models {MODEL}"),
            "mode:allow-all".to_string(),
            "prompt".to_string()
        ],
        "the session's queue is not the model, the mode and the brief in that order"
    );
}
