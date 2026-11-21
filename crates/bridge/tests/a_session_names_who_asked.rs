//! A session the bridge makes for a Claude Code conversation says whose work it is: the frame
//! carries the session that asked, and the daemon files the new one under it. A session nobody asked
//! for by id - the operator's own - stays a root, which `group_of` draws by its origin.

use std::path::{Path, PathBuf};

use letibot_bridge::daemon;
use letibot_bridge::head;
use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_sessionlog::server::serve_registry;
use letibot_sessionlog::{GroupRow, SessionGroup, group_of};

const OPERATOR: &str = "s-01abcdef";
/// The id shape a bridge's own seat carries: `cc-` and the Claude Code session id.
const ASKER: &str = "cc-b7b1c0de-4c2a-4c5e-9b8a-1b2c3d4e5f60";

/// A directory that removes itself.
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

/// Which group the daemon's own list draws `id` in - the question the operator asks when a worker
/// shows up as one of their conversations.
fn group(registry: &Registry, id: &str) -> SessionGroup {
    let list = registry.list();
    let rows: Vec<GroupRow<'_>> = list
        .iter()
        .map(|s| GroupRow {
            id: &s.session_id,
            parent: s.parent_session_id.as_deref(),
            origin: s.origin,
            role: (!s.role.is_empty()).then_some(s.role.as_str()),
            title: &s.title,
        })
        .collect();
    group_of(&rows, id)
}

/// A head may not be attached from a tool call's process group (`peer::in_daemon_tree`), and a
/// fixture run from inside a session is in one. A session of its own is the guard's own escape, so
/// this measures the frame rather than the group it was started in.
fn leave_the_tools_process_group() {
    // SAFETY: `setsid` takes no arguments and returns -1 where it cannot; a failure leaves this
    // process where it was, and the attach then says whatever the daemon thinks of it.
    unsafe {
        libc::setsid();
    }
}

#[test]
fn a_session_made_for_a_caller_is_filed_under_it() {
    leave_the_tools_process_group();
    let dir = TempDir::new("who-asked");
    let root = dir.path();
    std::fs::create_dir_all(root.join("config")).expect("config dir");
    std::fs::create_dir_all(root.join("data")).expect("data dir");
    let worktree = root.join("worktree");
    std::fs::create_dir_all(&worktree).expect("worktree");
    let socket = root.join("daemon.sock");
    // The one lock a test binary holds to write the environment (AGENTS.md), held to the end of
    // this test; it puts every variable back when it drops.
    let mut env = letibot_testenv::EnvGuard::lock();
    env.set("XDG_CONFIG_HOME", root.join("config"))
        .set("XDG_DATA_HOME", root.join("data"))
        .set("LETIBOT_SOCKET", &socket)
        .remove("LETIBOT_STORE");

    let registry = Registry::new();
    registry
        .create(OPERATOR, "the operator's own", SessionWiring::default())
        .expect("a fresh registry takes the session");
    let _server = serve_registry(registry.clone(), &socket).expect("the socket binds");
    let d = daemon::resolve(&worktree).expect("the daemon resolves");

    // What `leticode_start` does for a Claude Code caller: one session, under the conversation that
    // asked for it.
    let worker = head::create_session_under(
        &d,
        "Read your brief from /tmp/claude-501/brief.md",
        &worktree.display().to_string(),
        None,
        "",
        Some(ASKER),
    )
    .expect("the daemon takes a session that names its caller");
    let row = registry
        .brief(&worker)
        .expect("the daemon holds what it minted");
    assert_eq!(row.parent_session_id.as_deref(), Some(ASKER), "{row:?}");
    assert_eq!(
        group(&registry, &worker),
        SessionGroup::Subagents,
        "a session made for somebody's work is that session's child, not one of the operator's"
    );

    // And the other half of the rule: a session nobody asked for by id is a root - whatever its
    // ORIGIN says, which is the other axis (a bridge's identity files it as a package).
    let own = head::create_session(&d, "a question of my own").expect("a second session");
    let row = registry.brief(&own).expect("the daemon holds it");
    assert_eq!(row.parent_session_id, None, "{row:?}");
    assert_ne!(group(&registry, &own), SessionGroup::Subagents);
}
