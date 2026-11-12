//! A session closed while its worker is inside a tool call still holds its VM, and a VM left up is
//! a machine the next session cannot boot on: the worker is stuck in `firecode in`, so the
//! `Work::Close` that would bring the guest down is never served.
//!
//! So the close is settled off the worker (`letibot_harnessd::close_settle`). This file is that
//! taken at its limit: there is no worker at all, and the VM is the one this process booted for the
//! session. A stop through the socket must leave no VM behind.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use letibot_bridge::tools::Bridge;
use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_sessionlog::server::serve_registry;
use serde_json::json;

/// The session whose VM this test boots. The VM is recorded against it, which is how the close
/// finds it with no harness to ask.
const SESSION: &str = "s-vm-close";

/// A fake firecode: it boots, `in` runs the command here, and `down` leaves the marker whose
/// absence is *the VM is still up*.
const FIRECODE: &str = r#"#!/bin/bash
# A firecode that answers without a VM, logging each verb so a test can see what was asked.
set -u
root=$(cd "$(dirname "$0")/.." && pwd)
project=
verb=$1
shift
while [ $# -gt 0 ]; do
  case "$1" in
    --project) project=$2; shift 2 ;;
    *) shift ;;
  esac
done
[ -n "$project" ] && printf 'ARGV %s\n' "$verb" >> "$root/argv.log"
case "$verb" in
  up) exit 0 ;;
  info) printf '{"run": "r1"}\n' ;;
  in) bash -c "${1:-true}" 2>/dev/null; exit 0 ;;
  down)
    touch "$root/downed"
    mkdir -p "$root/runs/r1"
    printf 'none\n' > "$root/runs/r1/copy_out"
    ;;
esac
exit 0
"#;

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

/// The stub, executable, under `root`.
fn fake_firecode(root: &Path) -> PathBuf {
    let bin = root.join("fc").join("bin").join("firecode");
    std::fs::create_dir_all(bin.parent().expect("a bin dir")).expect("the bin directory");
    std::fs::create_dir_all(root.join("fc").join("runs")).expect("the runs directory");
    letibot_tools::testing::write_stub(&bin, FIRECODE);
    bin
}

/// The verbs the fake was asked, in order.
fn asked(root: &Path) -> Vec<String> {
    std::fs::read_to_string(root.join("fc").join("argv.log"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.strip_prefix("ARGV ").map(str::to_string))
        .collect()
}

#[test]
fn a_stop_brings_down_the_vm_of_a_session_no_worker_can_close() {
    let dir = TempDir::new("vm-close");
    let root = dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("config")).expect("config dir");
    std::fs::create_dir_all(root.join("data")).expect("data dir");
    let worktree = root.join("worktree");
    std::fs::create_dir_all(&worktree).expect("worktree");
    let tree = root.join("tree");
    std::fs::create_dir_all(&tree).expect("the tree the VM is booted on");
    std::fs::write(tree.join("a.txt"), b"hello\n").expect("a file");
    let socket = root.join("daemon.sock");
    // SAFETY: this file runs one test, and these are set before the socket or a head exists.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
        std::env::set_var("XDG_DATA_HOME", root.join("data"));
        std::env::set_var("LETIBOT_SOCKET", &socket);
        std::env::remove_var("LETIBOT_STORE");
    }

    // The daemon's registry, with the close's settler installed exactly where the daemon installs
    // it, and no worker: the session is never opened, so nothing but the settler can close anything.
    let registry = Registry::new();
    letibot_harnessd::close_settle::install(&registry);
    registry
        .create(SESSION, "", SessionWiring::default())
        .expect("a fresh registry takes the session");
    let _server = serve_registry(registry.clone(), &socket).expect("the socket binds");
    letibot_sessionlog::placement::write(&socket, "firecode").expect("the placement record");

    // The VM this session's worker booted, recorded in this process against the session. Mounted
    // rather than copied: `copy_tree` wants `rsync`, and the copy is not what this test is about.
    let bin = fake_firecode(&root);
    let mut spec = letibot_tools::firecode::FirecodeSpec::new(&tree, SESSION);
    spec.bin = Some(bin);
    spec.mount_rw = true;
    spec.exec = false;
    let vm = letibot_tools::firecode::FirecodeBackend::up(&spec).expect("the VM is placed");
    assert!(
        letibot_tools::firecode::held_for(SESSION),
        "the boot was not recorded: {}",
        letibot_tools::firecode::held_for(SESSION)
    );

    let bridge = Bridge::new();
    // Joined, not started: a stop is the bridge's verb for a session it holds.
    letibot_bridge::tools::call(
        &bridge,
        "leticode_attach",
        &json!({
            "worktree": worktree.display().to_string(),
            "session": SESSION,
            "watch_only": false,
        }),
    )
    .expect("the bridge joins the session");
    let stopped =
        letibot_bridge::tools::call(&bridge, "leticode_stop", &json!({ "session": SESSION }))
            .expect("the session closes");
    assert_eq!(stopped["closed"], json!(true), "{stopped}");

    // The settler waits a grace for the worker that is not there, then brings the VM down itself.
    // The marker is what is waited for: the record is cleared before the client runs, so a poll on
    // the record would pass a moment early.
    let downed = root.join("fc").join("downed");
    let began = Instant::now();
    while !downed.exists() {
        assert!(
            began.elapsed() < Duration::from_secs(30),
            "the VM outlived the session: {:?}",
            asked(&root)
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        asked(&root).iter().any(|v| v == "down"),
        "the client was never asked to bring the VM down: {:?}",
        asked(&root)
    );
    assert!(
        !letibot_tools::firecode::held_for(SESSION),
        "the VM was brought down but the session's record still holds it"
    );
    drop(vm);
}
