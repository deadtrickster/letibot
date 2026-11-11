//! R178: a daemon whose last session closed is still given another one. The live failure was a
//! bridge on exactly that daemon: `leticode_start` attached with no session in mind, the bare
//! attach resolved to the closed session, and every package after the first was refused with
//! "no such session" and a list of thousands of ids from every project.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use letibot_bridge::daemon;
use letibot_bridge::head::{self, Head};
use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_sessionlog::server::serve_registry;

const SESSION: &str = "s-01abcdef";

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

#[test]
fn a_daemon_whose_only_session_closed_takes_another_one() {
    let dir = TempDir::new("create-after-close");
    let root = dir.path();
    std::fs::create_dir_all(root.join("config")).expect("config dir");
    std::fs::create_dir_all(root.join("data")).expect("data dir");
    let worktree = root.join("worktree");
    std::fs::create_dir_all(&worktree).expect("worktree");
    let socket = root.join("daemon.sock");
    // SAFETY: this binary runs one test, and these are set before the socket or the head exists.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
        std::env::set_var("XDG_DATA_HOME", root.join("data"));
        std::env::set_var("LETIBOT_SOCKET", &socket);
        std::env::remove_var("LETIBOT_STORE");
    }

    let registry = Registry::new();
    registry
        .create(SESSION, "the package that ends", SessionWiring::default())
        .expect("a fresh registry takes the session");
    let _server = serve_registry(registry.clone(), &socket).expect("the socket binds");
    let d = daemon::resolve(&worktree).expect("the daemon resolves");

    // The package's session ends - `leticode_stop`'s close, which the daemon serves by removing
    // the entry. It stays up holding nothing, which is the state the failure was found in.
    let head = Head::attach(&d, SESSION).expect("the first head attaches");
    head.stop().expect("the stop is acked");
    let deadline = Instant::now() + Duration::from_secs(10);
    while registry.get(SESSION).is_some() {
        assert!(
            Instant::now() < deadline,
            "the close never reached the registry, so this test says nothing about a daemon \
             holding none"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    // The next `leticode_start` on this folder: what used to fail here is the attach itself.
    let made = head::create_session(&d, "the next package")
        .expect("a daemon holding no live session still takes a new one");
    assert_ne!(made, SESSION, "the new session is its own");
    assert!(
        registry.get(&made).is_some(),
        "the daemon holds the session it minted"
    );
    assert_eq!(
        registry.default_id(),
        made,
        "and a bare attach lands on it from now on"
    );

    // And the head that will drive it, from frame one, as `leticode_start` does next.
    let next = Head::attach(&d, &made).expect("a head attaches to the new session");
    assert_eq!(
        next.snapshot().session,
        made,
        "the socket names the session it holds"
    );
}
