//! **R102: a daemon the BRIDGE started is one `letibot --daemons` can see.**
//!
//! MEASURED 2026-10-27 on the operator's Mac: about forty `harnessd` processes were running —
//! every one of them started by a `letibot-mcp` bridge — and `letibot --daemons` answered *no
//! daemons recorded*. The launcher writes `<rundir>/<key>.json` beside the socket when it starts
//! a daemon (`record_write`), and four of its verbs read it: `--daemons` and `--ls` glob it,
//! `--stop N` and `--stop --all` resolve a handle through the same glob, and the attach banner
//! prints the model out of it. The bridge wrote none.
//!
//! **Why the record and not a listing that reads sockets.** The socket is
//! `<sha256(workspace)[..12]>.sock`, so a socket with no record names a hash and nothing else —
//! not the folder, not the role, not what answers its turns. Forty lines of hex is not a listing
//! an operator can act on, and `--stop N` needs the pid and the folder anyway.
//!
//! # What this drives
//!
//! The bridge's real start path (`daemon::ensure`), against a fake `harnessd` that binds the
//! `--socket` it is handed and then sleeps — no model, no store, no daemon of the operator's. And
//! then the operator's own launcher, `scripts/letibot --daemons`, whose reader is the thing that
//! was blind. One test, because `$HOME`, `$XDG_RUNTIME_DIR` and `$LETIBOT_BIN` are process-wide.

use std::path::{Path, PathBuf};

use letibot_bridge::daemon;
use letibot_tools::builtins::task::Placement;

/// A temp directory of this test's own.
fn scratch(tag: &str) -> PathBuf {
    let dir = letibot_sessionlog::testing::scratch_root().join(format!(
        "letibot-record-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("a temp dir");
    dir
}

/// **A `harnessd` that binds the socket it is given and then sleeps** — the whole of what
/// `daemon::ensure` asks of a daemon (*is anything accepting?*), and nothing else.
///
/// `python3`, because binding a unix socket is three lines of it and this test must not need the
/// real daemon: a real one wants a GGUF, a store and a model. The first branch is the probe
/// [`letibot_tools::testing::write_stub`] runs — it execs the file with NO arguments to prove it
/// is not held open for writing by a fork, so a no-argument run must be harmless and quick.
fn fake_daemon(path: &Path) {
    letibot_tools::testing::write_stub(
        path,
        r#"#!/bin/sh
[ $# -eq 0 ] && exit 0
sock=""
while [ $# -gt 0 ]; do
  case "$1" in
    --socket) sock="$2"; shift 2 ;;
    *) shift ;;
  esac
done
exec python3 -c '
import socket, sys, time
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.bind(sys.argv[1])
s.listen(8)
while True:
    time.sleep(0.5)
' "$sock"
"#,
    );
}

/// Is `program` on this box's `PATH`?
fn on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(program).is_file()))
        .unwrap_or(false)
}

#[test]
fn a_daemon_the_bridge_started_is_listed_by_the_launcher() {
    // **A sentence rather than a panic when the box has no python3**: the fake daemon is a
    // fixture, and a suite that reddens because a fixture's interpreter is absent measures the
    // box and not the tree.
    if !on_path("python3") {
        println!(
            "SKIPPED: no python3 on this box, so the fake daemon cannot bind a socket. What \
             this test measures — the bridge writing the launcher's record — is unmeasured here."
        );
        return;
    }

    let dir = scratch("list");
    let home = dir.join("home");
    let rundir = dir.join("rundir");
    let bin = dir.join("bin");
    let worktree = dir.join("worktree");
    for d in [&home, &rundir, &bin, &worktree] {
        std::fs::create_dir_all(d).expect("a fixture directory");
    }
    let harnessd = bin.join("harnessd");
    fake_daemon(&harnessd);

    // SAFETY (Rust 2024's `unsafe` on `set_var`): this binary holds one test and no thread that
    // reads the environment has started. Every one of these is a path in the fixture — `$HOME`
    // included, so nothing here can reach the operator's own store, log or runtime directory.
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("XDG_RUNTIME_DIR", &rundir);
        std::env::set_var("LETIBOT_BIN", &bin);
        std::env::remove_var("LETIBOT_SOCKET");
        std::env::remove_var("LETIBOT_LOG");
        std::env::remove_var("LETIBOT_STORE");
    }

    let d = daemon::ensure(&worktree, None, "writes-allowed", &Placement::Host)
        .expect("the bridge starts the fake daemon");
    assert!(d.started, "this call started it: {d:?}");

    // **The record, beside the socket, under the launcher's own name.**
    let record = d.socket.with_extension("json");
    assert_eq!(
        record.file_name().and_then(|n| n.to_str()),
        Some(format!("{}.json", d.key).as_str()),
        "the launcher globs $RUNDIR/*.json and takes the workspace out of the file"
    );
    let text = std::fs::read_to_string(&record).unwrap_or_else(|e| {
        panic!(
            "no record at {} after a start the bridge made ({e}) — this is the file whose absence \
             made `letibot --daemons` say *no daemons recorded* over forty running daemons",
            record.display()
        )
    });
    let v: serde_json::Value = serde_json::from_str(&text).expect("the record is JSON");
    assert_eq!(
        v["workspace"].as_str(),
        Some(d.workspace.display().to_string().as_str()),
        "the folder, which a socket's hash cannot say: {text}"
    );
    assert_eq!(
        v["socket"].as_str(),
        Some(d.socket.display().to_string().as_str())
    );
    // **A NUMBER, not a string**: the shell's `json_str` cannot see a number and `json_num`
    // cannot see a string, and `pid` is read through `json_field` for exactly that reason.
    assert!(
        v["pid"].is_u64() && v["pid"].as_u64().unwrap() > 0,
        "pid is a JSON number: {text}"
    );
    assert_eq!(v["role"].as_str(), Some("leticode"));
    assert_eq!(v["bash"].as_bool(), Some(true));
    assert!(
        v["started"]
            .as_str()
            .is_some_and(|s| s.ends_with('Z') && s.len() >= 20),
        "an RFC3339 stamp: {text}"
    );

    // **And the operator's own listing sees it**, which is the defect: the reader is
    // `scripts/letibot --daemons` itself, not a copy of it.
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/letibot")
        .canonicalize()
        .expect("the launcher script is in this repository");
    let out = std::process::Command::new("bash")
        .arg(&script)
        .arg("--daemons")
        .current_dir(&worktree)
        .env("HOME", &home)
        .env("XDG_RUNTIME_DIR", &rundir)
        .output()
        .expect("the launcher runs");
    let said = String::from_utf8_lossy(&out.stdout);
    let pid = v["pid"].as_u64().unwrap();
    assert!(
        said.contains(&d.workspace.display().to_string()),
        "`letibot --daemons` must name the folder the bridge's daemon serves. It said:\n{said}\n\
         (stderr: {})",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        said.contains(&format!("up (pid {pid})")),
        "and that it is up, by the pid the record names — the fake daemon is listening on that \
         socket. It said:\n{said}"
    );

    // The fixture's daemon goes with the fixture.
    // SAFETY: signal 0 and SIGTERM to a pid this test started.
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGTERM);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
