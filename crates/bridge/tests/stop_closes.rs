//! **`leticode_stop` ends the session here**, which is what lets its placement settle. Detaching
//! alone left the daemon holding the harness - and the branch on the host - for as long as the
//! daemon lived, so a package's commits came back only if the operator stopped the whole daemon.
//!
//! The daemon's half is the registry: a stop that closed the session leaves no entry behind and
//! rings the worker to let the harness go. That ring is the act every placement hangs from.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use letibot_bridge::daemon;
use letibot_bridge::head::Head;
use letibot_sessionlog::registry::{Registry, SessionWiring, Work, WorkOrIdle};
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

    fn path(&self) -> &PathBuf {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_stop_closes_the_session_here_so_its_placement_can_settle() {
    let dir = TempDir::new("stop-closes");
    let root = dir.path().clone();
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
        .create(SESSION, "", SessionWiring::default())
        .expect("a fresh registry takes the session");
    let _server = serve_registry(registry.clone(), &socket).expect("the socket binds");
    let head = Head::attach(
        &daemon::resolve(&worktree).expect("the daemon resolves"),
        SESSION,
    )
    .expect("the head attaches");

    head.stop().expect("the stop is sent");

    // The daemon's half, off one ring: a close removes the entry and rings the worker, and the
    // worker's `Sessions::close` is what closes the backend. A bare detach rings nothing.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(
            Instant::now() < deadline,
            "a stop closed the session here: the registry still holds it, so its placement has \
             no door to come home through"
        );
        match registry.next_work_until(Some(Instant::now() + Duration::from_millis(100))) {
            WorkOrIdle::Work(Work::Close(id)) => {
                assert_eq!(
                    id, SESSION,
                    "the close names the session the bridge stopped"
                );
                break;
            }
            // The attach's own rings first, and an idle tick is nothing.
            WorkOrIdle::Work(Work::Open(_))
            | WorkOrIdle::Work(Work::Woken(_))
            | WorkOrIdle::Work(Work::Command(..))
            | WorkOrIdle::Idle => continue,
            WorkOrIdle::Closed => panic!("the daemon went away under the stop"),
        }
    }
    assert!(
        registry.get(SESSION).is_none(),
        "the session is closed here and stays on disk for whoever opens it next"
    );
}
