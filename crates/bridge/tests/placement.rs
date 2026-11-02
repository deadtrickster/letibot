//! **`leticode_start { host }` on a box whose host is not there** - the two refusals, end to end.
//!
//! Both ways placing a package's TOOLS on another machine goes wrong are refusals rather than
//! failures, because either one silently running the work somewhere nobody asked for is the
//! defect this file exists against: a host name `hosts.toml` does not declare is refused before
//! anything is started, and a daemon already up for the folder but placed elsewhere is refused
//! before it is joined. **Nothing here reaches a host, and nothing starts a daemon**: `$LETIBOT_BIN`
//! points at an empty directory, so a start would fail. One test, as the environment is process-wide.

use std::path::{Path, PathBuf};

use letibot_bridge::tools::{self, Bridge};
use serde_json::json;

/// A scratch directory this file owns, removed when the test ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Scratch {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = letibot_sessionlog::testing::scratch_root().join(format!(
            "letibot-bridge-place-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        Scratch(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_package_is_placed_on_a_host_only_when_the_host_and_the_daemon_agree() {
    let scratch = Scratch::new();
    let root = scratch.path();
    let worktree = root.join("worktree");
    std::fs::create_dir_all(&worktree).expect("the worktree");
    // **No `harnessd` anywhere**: this file's assertions include *nothing was started*, and a
    // binary the bridge could reach would make that a hope rather than a check.
    let no_bin = root.join("no-bin");
    std::fs::create_dir_all(&no_bin).expect("an empty bin directory");
    let hosts = root.join("hosts.toml");
    std::fs::write(
        &hosts,
        "# Written by crates/bridge/tests/placement.rs — a host no test connects to.\n\
         [host.lab2x1]\nhostname = \"lab2x1.invalid\"\n\
         [host.lab3x1]\nhostname = \"lab3x1.invalid\"\n",
    )
    .expect("hosts.toml");
    let socket = root.join("daemon.sock");

    // SAFETY: this test binary is one test, and these are set before the bridge exists. `HOME`,
    // `XDG_CONFIG_HOME` and `XDG_DATA_HOME` are here so nothing of the operator's can decide an
    // answer this file is asserting.
    unsafe {
        std::env::set_var("LETIBOT_HOSTS", &hosts);
        std::env::set_var("LETIBOT_BIN", &no_bin);
        std::env::set_var("LETIBOT_SOCKET", &socket);
        std::env::set_var("LETIBOT_STORE", root.join("sessions.db"));
        std::env::set_var("LETIBOT_LOG", root.join("daemon.log"));
        std::env::set_var("HOME", root);
        std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
        std::env::set_var("XDG_DATA_HOME", root.join("data"));
    }

    let bridge = Bridge::new();

    // --- 1. a host the operator never declared --------------------------------
    let asked = json!({
        "worktree": worktree.display().to_string(),
        "brief": "a brief that must not run anywhere",
        "host": "lab2",
    });
    let refused = tools::call(&bridge, "leticode_start", &asked)
        .expect_err("`lab2` is not a host this box has");
    assert!(
        refused.contains("there is no host `lab2`"),
        "the refusal is the tree's own, the one `where: host:NAME` gives: {refused}"
    );
    assert!(
        refused.contains("it knows: lab2x1, lab3x1"),
        "a refusal that does not list the names costs the caller a guess: {refused}"
    );
    assert!(
        refused.contains(&hosts.display().to_string()),
        "the file it read is named: {refused}"
    );
    assert!(
        refused.contains("nothing was started"),
        "the caller has to know no daemon was begun on its behalf: {refused}"
    );
    assert!(
        !refused.contains("no harnessd"),
        "the name is refused BEFORE anything is started — `$LETIBOT_BIN` was never reached: \
         {refused}"
    );
    assert!(
        !socket.exists(),
        "a refused name leaves no socket: nothing was started"
    );

    // --- 2. a daemon already up, placed on this box ---------------------------
    // A declared host this call can ask for, and not the one the daemon recorded: `lab3x1`
    // exists in the file above, so the name check passes and the answer is the placement's.
    let elsewhere = json!({
        "worktree": worktree.display().to_string(),
        "brief": "a brief that must not run anywhere",
        "host": "lab3x1",
    });
    // The daemon a folder already has, as far as this file is concerned: a socket something
    // accepts on, and the record a daemon on this box writes beside it. Bound for the life of
    // the test so `is_up` sees a listener.
    let daemon = std::os::unix::net::UnixListener::bind(&socket).expect("a socket binds");
    letibot_sessionlog::placement::write(&socket, "host").expect("the placement record");
    let refused = tools::call(&bridge, "leticode_start", &elsewhere)
        .expect_err("a daemon running the tools here is not the one a host call asked for");
    assert!(
        refused.contains("its tools run on this box"),
        "the refusal says where the running daemon's tools actually are: {refused}"
    );
    assert!(
        refused.contains("host:lab3x1"),
        "and what this call asked for: {refused}"
    );
    assert!(
        refused.contains("letibot --stop"),
        "a refusal the caller cannot act on is the one that costs an hour: {refused}"
    );
    assert!(
        !refused.contains("no harnessd"),
        "an already-running daemon is joined or refused, never restarted: {refused}"
    );

    // --- 3. a daemon that recorded nothing ----------------------------------
    // A daemon from a build before the record existed, or one whose record names a socket that is
    // gone. *Unknown* is refused in both directions: it is not *local*, and guessing is worse.
    std::fs::remove_file(letibot_sessionlog::placement::path(&socket)).expect("the record");
    let refused = tools::call(&bridge, "leticode_start", &elsewhere)
        .expect_err("nothing recorded is not an answer for a host either");
    assert!(refused.contains("recorded no placement"), "{refused}");
    assert!(
        refused.contains(".placement"),
        "the refusal names the file it looked for: {refused}"
    );
    // And the same for a call that asked for this box: the record is what is missing, not the
    // host name.
    let here = json!({
        "worktree": worktree.display().to_string(),
        "brief": "a brief that must not run anywhere",
    });
    let refused = tools::call(&bridge, "leticode_start", &here).expect_err("nothing recorded");
    assert!(refused.contains("recorded no placement"), "{refused}");
    drop(daemon);
}
