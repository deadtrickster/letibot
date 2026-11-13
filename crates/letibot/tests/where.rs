//! **`letibot --where` is the bridge's own answer, and `--ensure` starts the daemon it names.**
//!
//! A program that drives letibot from outside — rano's editor panel — asks this instead of
//! re-deriving `<rundir>/<sha256(folder)[..12]>.sock`. Two spellings of that rule would name a
//! socket nothing listens on, so this drives `scripts/letibot` against a fake `harnessd` and
//! compares what it printed with `bridge::daemon::resolve`, the resolver it must agree with.

use std::path::{Path, PathBuf};

use letibot_bridge::daemon;

/// A temp directory of this test's own.
fn scratch(tag: &str) -> PathBuf {
    let dir = letibot_sessionlog::testing::scratch_root().join(format!(
        "letibot-where-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("a temp dir");
    dir
}

/// A `harnessd` that binds the socket it is given and then sleeps: the whole of what
/// `daemon::ensure` asks of a daemon (*is anything accepting?*). A real one wants a GGUF, a
/// store and a model, none of which this test is about.
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

/// The launcher's own answer for one invocation, run the way a person's shell runs it: from the
/// worktree, with nothing of the operator's environment, and with `$LETIBOT_BIN` naming the
/// fixture's binaries. `home` and `rundir` are handed over rather than derived from `bin`, so the
/// fixture a case needs is the fixture it gets.
fn run(
    worktree: &Path,
    bin: &Path,
    home: &Path,
    rundir: &Path,
    args: &[&str],
) -> (bool, String, String) {
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/letibot")
        .canonicalize()
        .expect("the launcher script is in this repository");
    let out = std::process::Command::new("bash")
        .arg(&script)
        .args(args)
        .current_dir(worktree)
        .env_clear()
        // `git` (the workspace's own resolution) and `python3` (the fake daemon) live in /usr/bin.
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("HOME", home)
        .env("XDG_RUNTIME_DIR", rundir)
        .env("LETIBOT_BIN", bin)
        .env("LETIBOT_ROOM_SUPERVISOR", bin.join("room"))
        .output()
        .expect("the launcher runs");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn where_prints_the_bridges_socket_and_ensure_starts_that_daemon() {
    let Some(_) = letibot_tokencore::apparatus::present(
        "python3, which the fake daemon binds its socket with",
        on_path("python3"),
    ) else {
        return;
    };
    let dir = scratch("socket");
    let home = dir.join("home");
    let rundir = dir.join("rundir");
    let bin = dir.join("bin");
    let worktree = dir.join("worktree");
    for d in [&home, &rundir, &bin, &worktree] {
        std::fs::create_dir_all(d).expect("a fixture directory");
    }
    fake_daemon(&bin.join("harnessd"));
    // `ensure` runs `letibot --room` first. Unstubbed, that is a real coordinator nothing here
    // stops, so the fixture answers it and does nothing.
    letibot_tools::testing::write_stub(&bin.join("room"), "#!/bin/sh\nexit 0\n");
    // The launcher runs `$BIN/letibot where`, so the binary under test is placed where the
    // launcher looks for it. COPIED, not linked: `current_exe()` resolves the link, and the
    // daemon binary is looked for beside it — which would be another `harnessd` than the stub.
    let me = bin.join("letibot");
    std::fs::copy(env!("CARGO_BIN_EXE_letibot"), &me).expect("the binary is copied");
    use std::os::unix::fs::PermissionsExt as _;
    let mode = std::fs::Permissions::from_mode(0o755);
    std::fs::set_permissions(&me, mode).expect("the copy is executable");

    // SAFETY (Rust 2024's `unsafe` on `set_var`): this binary holds one test and no thread that
    // reads the environment has started. Every value is a path in the fixture — `$HOME`
    // included — so nothing here can reach the operator's own store, log or runtime directory.
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("XDG_RUNTIME_DIR", &rundir);
        std::env::remove_var("LETIBOT_SOCKET");
        std::env::remove_var("LETIBOT_LOG");
        std::env::remove_var("LETIBOT_STORE");
    }
    let d = daemon::resolve(&worktree).expect("the folder's daemon resolves");

    // **1. Nothing is listening: `--where --json` says where, and that it is down.**
    let (ok, out, err) = run(&worktree, &bin, &home, &rundir, &["--where", "--json"]);
    assert!(
        ok,
        "`letibot --where --json` exited non-zero. stderr:\n{err}"
    );
    let v: serde_json::Value = serde_json::from_str(out.trim())
        .unwrap_or_else(|e| panic!("stdout is one JSON object ({e}):\n{out}"));
    assert_eq!(
        v["socket"].as_str(),
        Some(d.socket.display().to_string().as_str()),
        "the socket must be what `bridge::daemon::resolve` computes — anything else is a second \
         spelling of which folder a session belongs to, and it names a socket no daemon binds. \
         It said:\n{out}"
    );
    assert_eq!(v["live"].as_bool(), Some(false), "{out}");
    assert_eq!(v["project"].as_str(), Some(d.key.as_str()), "{out}");
    assert_eq!(
        v["folder"].as_str(),
        Some(d.workspace.display().to_string().as_str()),
        "{out}"
    );

    // **2. The plain form is the path a script can take, and the state on its own line.**
    let (ok, out, err) = run(&worktree, &bin, &home, &rundir, &["--where"]);
    assert!(ok, "`letibot --where` exited non-zero. stderr:\n{err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines.first().copied(),
        Some(d.socket.display().to_string().as_str()),
        "the first line is the path and nothing else:\n{out}"
    );
    assert_eq!(lines.get(1).copied(), Some("down"), "{out}");

    // **3. `--ensure` starts one, and the answer says so.**
    let (ok, out, err) = run(
        &worktree,
        &bin,
        &home,
        &rundir,
        &["--where", "--ensure", "--json"],
    );
    assert!(
        ok,
        "`letibot --where --ensure --json` exited non-zero. stderr:\n{err}"
    );
    let v: serde_json::Value =
        serde_json::from_str(out.trim()).unwrap_or_else(|e| panic!("one object ({e}):\n{out}"));
    assert_eq!(
        v["socket"].as_str(),
        Some(d.socket.display().to_string().as_str()),
        "--ensure answers about the same socket, or the panel attaches somewhere else:\n{out}"
    );
    assert_eq!(
        v["live"].as_bool(),
        Some(true),
        "the daemon was started:\n{out}\nstderr:\n{err}"
    );
    assert!(
        daemon::is_up(&d.socket),
        "nothing is accepting on {} after --ensure",
        d.socket.display()
    );

    // **3b. `--ensure` against a live daemon is a READ.** A panel asks every time it opens, and
    // starting a second daemon over a conversation somebody is in would end it.
    let (ok, out, err) = run(
        &worktree,
        &bin,
        &home,
        &rundir,
        &["--where", "--ensure", "--json"],
    );
    assert!(ok, "the second `--ensure` exited non-zero. stderr:\n{err}");
    let again: serde_json::Value =
        serde_json::from_str(out.trim()).unwrap_or_else(|e| panic!("one object ({e}):\n{out}"));
    assert_eq!(again["live"].as_bool(), Some(true), "{out}");
    assert!(
        !err.contains("started a daemon"),
        "a daemon that is already live is left alone, and this call started one:\n{err}"
    );

    // **4. And the record a live daemon leaves is the launcher's own**, because
    // `--ls` and `--stop` read it: a daemon started here must not be invisible to them.
    let record = d.socket.with_extension("json");
    let text = std::fs::read_to_string(&record).unwrap_or_else(|e| {
        panic!(
            "no record at {} after --ensure ({e}): `letibot --ls` would not list it",
            record.display()
        )
    });
    let rec: serde_json::Value = serde_json::from_str(&text).expect("the record is JSON");
    let pid = rec["pid"].as_u64().expect("pid is a number") as i32;
    assert!(pid > 0, "{text}");

    // The fixture's daemon goes with the fixture.
    // SAFETY: signal 0 and SIGTERM to a pid this test started.
    unsafe {
        libc::kill(pid, libc::SIGTERM);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
