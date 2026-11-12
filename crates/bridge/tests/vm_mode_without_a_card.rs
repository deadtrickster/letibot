//! `leticode_mode allow-all` on a session whose tools run in a VM: the VM IS the boundary, so the
//! point needs nobody, and asking a person about it put a card in front of the heads while the
//! worker was still opening - a question whose answer the placement already had.

use std::path::{Path, PathBuf};

use letibot_bridge::tools::{self, Bridge};
use letibot_sessionlog::hub::CommandKind;
use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_sessionlog::server::serve_registry;
use serde_json::json;

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
fn a_mode_on_a_vm_session_is_applied_without_a_card() {
    let dir = TempDir::new("vm-mode");
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
    let registry = Registry::new();
    registry
        .create("s-daemons-own", "", SessionWiring::default())
        .expect("a fresh registry takes a session");
    let _server = serve_registry(registry.clone(), &socket).expect("the socket binds");
    letibot_sessionlog::placement::write(&socket, "firecode").expect("the placement record");
    let bridge = Bridge::new();
    let started = tools::call(
        &bridge,
        "leticode_start",
        &json!({
            "worktree": worktree.display().to_string(),
            "brief": "say one line",
            "vm": true,
        }),
    )
    .expect("a start on a placed session");
    let session = started["session"]
        .as_str()
        .expect("a session id")
        .to_string();

    let asked = tools::call(
        &bridge,
        "leticode_mode",
        &json!({ "session": session, "mode": "allow-all" }),
    )
    .expect("allow-all inside a VM needs nobody, so it is queued rather than refused");
    assert_eq!(asked["queued"], json!(true), "{asked}");
    assert_eq!(asked["answer"], json!("nobody"), "{asked}");

    // A card would be a `ModeConfirm`, which is the frame that asks a person. Nothing asked.
    let hub = registry.get(&session).expect("the session");
    // The start's own mode and brief, and this call's: all three arrive after their answers, because
    // neither answer waits for the daemon.
    let began = std::time::Instant::now();
    while hub.census().commands < 3 {
        assert!(
            began.elapsed() < std::time::Duration::from_secs(10),
            "the daemon took {} of the three frames",
            hub.census().commands
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let mut kinds: Vec<&str> = Vec::new();
    while let Some(cmd) = hub.try_command() {
        kinds.push(match cmd.kind {
            CommandKind::ModeConfirm { .. } => "confirm",
            CommandKind::Mode { .. } => "mode",
            CommandKind::Slash { .. } => "slash",
            CommandKind::Prompt { .. } => "prompt",
            _ => "other",
        });
    }
    assert!(
        !kinds.contains(&"confirm"),
        "a person was asked about a point the VM already settles: {kinds:?}"
    );
    assert!(
        kinds.contains(&"mode"),
        "the point never reached the daemon: {kinds:?}"
    );
}
