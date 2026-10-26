//! **Where a folder's daemon lives, and how to start one** — the launcher's job, done
//! from a head.
//!
//! `scripts/letibot` owns this knowledge for a person: one daemon per workspace, a socket
//! named by the workspace's own hash under `$XDG_RUNTIME_DIR/letibot`, and a start
//! sequence that refuses to begin a second daemon over a live one. The bridge needs the
//! same answers, because a session it starts must live in the daemon that folder already
//! has — a same-shaped second one would mean two daemons interleaving rows into one
//! transcript until the store refuses every write (the defect `scripts/letibot`'s `live`
//! and `live_detached` read `ss` to avoid).
//!
//! **This is a port of the paths, not of the launcher.** The launcher's crate
//! (`crates/letibot`) is a binary-only crate — `src/launcher.rs` has no library target —
//! so there is nothing to call; what is here is the same three facts spelled again:
//!
//! * the socket is `<rundir>/<sha256(workspace)[..12]>.sock`,
//! * the store is `$LETIBOT_STORE` or `~/.local/share/letibot/sessions.db`,
//! * the daemon is started detached, with the seat and mode the `leticode` wrapper
//!   starts it with, and its own log beside the socket's key.
//!
//! A difference between this and the shell would name a socket no daemon listened on, so
//! the two facts a person can check — the key, and the path it makes — are asserted
//! against the values `launcher.rs` measured on this box.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The name a head attaches under — a fact of the daemon's log, so it is the bridge's own
/// and not a guess: `HeadAttached`/`HeadDetached` name it, and the decision corpus records
/// the answers given from it.
pub const IDENTITY: &str = "letibot-mcp";

/// The handshake kind this head sends. The protocol's vocabulary is `tui`, `remote`,
/// `flowy`, `acp`; a program holding a session is `remote`, which is what its own doc
/// calls a head that is not a terminal.
pub const HEAD_KIND: &str = "remote";

/// The directory the runtime files live in: `$XDG_RUNTIME_DIR/letibot`, else
/// `/run/user/<uid>/letibot` on Linux and the per-user temp dir on macOS —
/// `launcher::rundir`'s rule, asked of the system rather than read from `$TMPDIR` so a
/// daemon started without that variable lands where the head looks.
pub fn rundir() -> PathBuf {
    let base = match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => fallback_runtime_base(),
    };
    base.join("letibot")
}

#[cfg(not(target_os = "macos"))]
fn fallback_runtime_base() -> PathBuf {
    // SAFETY: getuid cannot fail.
    PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() }))
}

#[cfg(target_os = "macos")]
fn fallback_runtime_base() -> PathBuf {
    let mut buf = vec![0u8; 1024];
    // SAFETY: `confstr` writes at most `buf.len()` bytes and returns how many.
    let n = unsafe {
        libc::confstr(
            libc::_CS_DARWIN_USER_TEMP_DIR,
            buf.as_mut_ptr() as *mut libc::c_char,
            buf.len(),
        )
    };
    if n == 0 || n > buf.len() {
        return std::env::temp_dir();
    }
    buf.truncate(n - 1);
    PathBuf::from(String::from_utf8_lossy(&buf).into_owned())
}

/// The key a workspace hashes to: the first twelve hex characters of its SHA-256, over
/// the path with nothing added — the shell writes `printf '%s' "$WORKSPACE" | sha12`, and a
/// trailing newline here would name a different socket for every folder.
pub fn key(workspace: &Path) -> String {
    use sha2::Digest;
    let mut h = sha2::Sha256::new();
    h.update(workspace.as_os_str().as_encoded_bytes());
    let digest = h.finalize();
    digest.iter().take(6).map(|b| format!("{b:02x}")).collect()
}

/// A folder's daemon: what it is called, where it listens, and where it writes its log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Daemon {
    /// The workspace the key was taken from — the git toplevel, as the launcher resolves
    /// it, because `letibot` in a subdirectory joins the repository's daemon.
    pub workspace: PathBuf,
    pub key: String,
    pub socket: PathBuf,
    pub log: PathBuf,
    /// Where the daemon keeps transcripts. `$LETIBOT_STORE` wins, as it does in the shell.
    pub store: PathBuf,
    /// True when THIS call started the daemon, so the answer can say so — a bridge that
    /// claimed to have started a daemon somebody else's process was already serving would
    /// be describing the wrong process.
    pub started: bool,
}

/// `$HOME`, or a refusal rather than a path spelled `~`.
fn home() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "no $HOME: the store and the log have no default without one".to_string())
}

/// **The workspace a path belongs to**, as the launcher resolves it: the git toplevel when
/// the path is inside a checkout, else the path itself, normalised with `cd`'s own answer.
///
/// A path that does not exist is refused by name rather than hashed: an empty workspace is
/// a socket key everything shares, and the shell's own guard exists because a failed `cd`
/// there once reopened another project's conversation.
pub fn workspace_of(worktree: &Path) -> Result<PathBuf, String> {
    let dir =
        std::fs::canonicalize(worktree).map_err(|e| format!("{}: {e}", worktree.display()))?;
    let out = Command::new("git")
        .arg("-C")
        .arg(&dir)
        .args(["rev-parse", "--show-toplevel"])
        .stdin(Stdio::null())
        .output();
    if let Ok(out) = out
        && out.status.success()
    {
        let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !text.is_empty()
            && let Ok(top) = std::fs::canonicalize(&text)
        {
            return Ok(top);
        }
    }
    Ok(dir)
}

/// The socket a key names under a rundir — one spelling, so `resolve` and the test that
/// pins the rule cannot drift.
fn socket_in(rundir: &Path, key: &str) -> PathBuf {
    rundir.join(format!("{key}.sock"))
}

/// The daemon for a workspace, without asking whether it is up.
pub fn resolve(workspace: &Path) -> Result<Daemon, String> {
    let workspace = workspace_of(workspace)?;
    let key = key(&workspace);
    // `$LETIBOT_SOCKET` is authoritative when set, exactly as in the shell: it is how a
    // test — or a second head on the same folder — names a daemon the hash does not.
    let socket = match std::env::var_os("LETIBOT_SOCKET") {
        Some(s) if !s.is_empty() => PathBuf::from(s),
        _ => socket_in(&rundir(), &key),
    };
    let log = match std::env::var_os("LETIBOT_LOG") {
        Some(s) if !s.is_empty() => PathBuf::from(s),
        _ => home()?.join("logs").join(format!("harnessd-{key}.log")),
    };
    let store = match std::env::var_os("LETIBOT_STORE") {
        Some(s) if !s.is_empty() => PathBuf::from(s),
        _ => home()?.join(".local/share/letibot/sessions.db"),
    };
    Ok(Daemon {
        workspace,
        key,
        socket,
        log,
        store,
        started: false,
    })
}

/// Is something accepting connections on this socket right now?
///
/// A connect, not a `stat`: the daemon's own shutdown unlinks the socket file while the
/// process goes on listening on the nameless inode, so "is there a file" answers *no* for
/// a folder that has a daemon — measured by the shell, and the reason it reads a listener
/// rather than a path.
pub fn is_up(socket: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(socket).is_ok()
}

/// The daemon's binary: `$LETIBOT_BIN`, then `harnessd` on `PATH`, then beside this
/// executable.
///
/// The launcher resolves it against its own location and `../../target/release`; a bridge
/// started by Claude Code has no launcher beside it, so the order is inverted — an
/// explicit `LETIBOT_BIN` first (the same variable the shell treats as authoritative) and
/// then whatever `harnessd` the box has.
fn daemon_binary() -> Result<PathBuf, String> {
    if let Some(dir) = std::env::var_os("LETIBOT_BIN")
        && !dir.is_empty()
    {
        let p = PathBuf::from(dir).join("harnessd");
        return if p.is_file() {
            Ok(p)
        } else {
            Err(format!(
                "$LETIBOT_BIN is set but there is no harnessd at {}",
                p.display()
            ))
        };
    }
    if let Some(path) = std::env::var_os("PATH")
        && let Some(found) = std::env::split_paths(&path)
            .map(|d| d.join("harnessd"))
            .find(|p| p.is_file())
    {
        return Ok(found);
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let p = dir.join("harnessd");
        if p.is_file() {
            return Ok(p);
        }
    }
    Err("no harnessd: set $LETIBOT_BIN to the directory holding it, or put it on PATH".into())
}

/// **The daemon's flags, as the `leticode` wrapper passes them.**
///
/// `scripts/letibot` starts a daemon with a seat, an approval point and a shell, and those
/// three are what make a session answerable at all: the reviewer-in-the-loop design
/// (`docs/claude-code-bridge.md`) is `writes-allowed` plus the gate's asks reaching this
/// head — a daemon started at `always-ask` with no shell would ask about nothing and do
/// nothing.
///
/// **And `--no-queue`, which the launcher never passes.** A daemon the bridge starts on a
/// package's worktree is one of that repository's session daemons by the queue's own rule
/// (`mergequeue::serves`: the checkout, under it, or one of its worktrees), so without the
/// flag it claims the repository's lease and runs the queue itself: on 2026-10-26 the daemon
/// started for package P1 found letibot's `main` red and put an EMERGENCY in front of the
/// package's model before its brief. A package worker is not the repository's queue holder —
/// the operator's own daemon is — and two daemons interleaving landings for one repository is
/// the second half of that defect. A daemon that is already up is not this call's to change;
/// `ensure` returns it as it is.
fn daemon_args(d: &Daemon, model: Option<&str>, mode: &str) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "--workspace".into(),
        d.workspace.display().to_string(),
        "--store".into(),
        d.store.display().to_string(),
        "--socket".into(),
        d.socket.display().to_string(),
        "--role".into(),
        "leticode".into(),
        // The gate's asks go to an attached head, which is this bridge. `console` reads
        // the daemon's own stdin, and a detached daemon has none — every gated call would
        // refuse with "nobody answered".
        "--adjudicator".into(),
        "head".into(),
        "--mode".into(),
        mode.into(),
        "--bash".into(),
        // See this function's doc: a package's daemon serves no queue.
        "--no-queue".into(),
    ];
    if let Some(m) = model.filter(|m| !m.trim().is_empty()) {
        // `deepseek`, `glm`, `grok` are providers (the turns leave the box); anything else
        // is the local binding, which is what the launcher's `--model` means. A
        // `provider/model` pair names both.
        match m.split_once('/') {
            Some((provider, name)) => {
                args.push("--provider".into());
                args.push(provider.into());
                args.push("--model".into());
                args.push(name.into());
            }
            None if is_provider(m) => {
                args.push("--provider".into());
                args.push(m.into());
            }
            None => {
                args.push("--model".into());
                args.push(m.into());
            }
        }
    }
    args
}

/// The provider names this build's launcher offers (`letibot --provider`).
fn is_provider(name: &str) -> bool {
    matches!(name, "deepseek" | "glm" | "grok")
}

/// Start the daemon detached, in its own session, writing to its own log, and hand back the
/// child so that whoever started it can wait for it.
///
/// `setsid` rather than a plain spawn: a daemon that died with the MCP process would take
/// every session it holds with it, which is the one property the two-process split exists
/// to deny. The launcher gets the same effect from `setsid(1)` on Linux and perl's
/// `POSIX::setsid` on a Mac; the syscall is the same thing without a fork of either.
fn start(
    binary: &Path,
    d: &Daemon,
    model: Option<&str>,
    mode: &str,
) -> Result<std::process::Child, String> {
    if let Some(dir) = d.log.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    if let Some(dir) = d.socket.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&d.log)
        .map_err(|e| format!("{}: {e}", d.log.display()))?;
    let log2 = log
        .try_clone()
        .map_err(|e| format!("{}: {e}", d.log.display()))?;
    let mut cmd = Command::new(binary);
    cmd.args(daemon_args(d, model, mode))
        // The daemon links libllama for its TOKENIZER, and ggml registers every backend at
        // load — 1.1 GB of a card for ops it never runs. `scripts/letibot` blindfolds it
        // with the same variable, for the same reason and for harnessd only.
        .env("CUDA_VISIBLE_DEVICES", "")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log2));
    // SAFETY: `setsid` is async-signal-safe and touches nothing the child's own state
    // needs; it runs between fork and exec, which is what `pre_exec` is for.
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd.spawn()
        .map_err(|e| format!("starting {}: {e}", binary.display()))
}

/// **Every child this process starts is waited for** — the bridge's half of the rule this whole
/// tree is held to: *any process letibot starts is supervised, no exceptions*.
///
/// MEASURED 2026-10-26: a `Child` is not waited for on drop, and the bridge dropped it the
/// moment it had the pid — so every daemon that exited stayed `<defunct>` under `letibot-mcp`
/// for as long as that bridge lived. One bridge held twelve (aged 1.5 to 9.5 hours, package
/// daemons stopped cleanly with `letibot --stop`) and another seven. A zombie holds a process
/// table entry and, worse, counts as a running daemon in `ps`: the person asking *is this
/// folder's daemon still up* cannot tell a dead one from a live one.
///
/// **A thread per child, and not a `SIGCHLD` handler.** A handler is process-wide and
/// `wait(2)`-shaped: it would take the exit of *any* child this program ever has — including
/// ones the bridge did not start and cannot account for — and it would have to be installed
/// once and coexist with anything else that wants that signal. A bridge starts one daemon per
/// folder and lives for hours, so a thread per daemon is one stack each, waiting on exactly the
/// child it was handed.
///
/// **And it does not tie the daemon's lifetime to the bridge's.** The child is `setsid`'d
/// ([`start`]), so it is in a session of its own: no signal aimed at this process's group
/// reaches it, and when the bridge exits the waiting thread simply goes with it and the kernel
/// hands the running daemon to init. Waiting is not owning — what is reaped here is a corpse,
/// never a live child.
fn reap(mut child: std::process::Child) {
    let pid = child.id();
    let spawned = std::thread::Builder::new()
        .name(format!("reap-{pid}"))
        .spawn(move || {
            // The status is not this thread's business: *did the daemon come up* is answered by
            // the socket, with the log's tail behind it (`ensure`), and a daemon that came up
            // and later exits is the operator's act or the idle exit's. What is owed here is the
            // wait itself, and an error from it (a child somebody else already waited for) is
            // the same fact as success.
            let _ = child.wait();
        });
    if let Err(e) = spawned {
        // **Said rather than silent, and the daemon is not harmed by it**: the only thing lost
        // is this process's tidiness, which is exactly the defect this function exists for — so
        // a bridge that cannot afford a thread says so on its stderr instead of leaking quietly.
        eprintln!(
            "  harnessd pid {pid} will not be waited for: the reaping thread could not be \
             started ({e}). It becomes a zombie of this process when it exits; the daemon itself \
             is unaffected and outlives this bridge."
        );
    }
}

/// **This folder's daemon started, detached, and reaped when it exits** — the pid the answer
/// names and nothing else about the child.
///
/// The binary is the caller's so that the start path a test can drive is the one a daemon
/// start really takes; resolving it ([`daemon_binary`]) is `ensure`'s business.
fn spawn(binary: &Path, d: &Daemon, model: Option<&str>, mode: &str) -> Result<u32, String> {
    let child = start(binary, d, model, mode)?;
    let pid = child.id();
    reap(child);
    Ok(pid)
}

/// **This folder's daemon, started if nothing is listening.**
///
/// The wait is the launcher's: poll the cheap check — is anything accepting — up to sixty
/// times at half a second, then report the log's tail rather than a bare timeout, because
/// a daemon that did not come up has said why on its own log and the person reading the
/// bridge's answer needs those lines.
pub fn ensure(worktree: &Path, model: Option<&str>, mode: &str) -> Result<Daemon, String> {
    let mut d = resolve(worktree)?;
    if is_up(&d.socket) {
        return Ok(d);
    }
    // **A second daemon for one folder is what corrupts a transcript**, and the refusal
    // that catches it is harnessd's own folder claim: it refuses a start over a live
    // daemon by name. So the start is attempted, and a refusal reaches the caller as the
    // log tail rather than being pre-empted by a guess about a lock this box may not let
    // a head read.
    let pid = spawn(&daemon_binary()?, &d, model, mode)?;
    for _ in 0..60 {
        if is_up(&d.socket) {
            d.started = true;
            return Ok(d);
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    Err(format!(
        "the daemon for {} (pid {pid}) did not come up: nothing is accepting on {}.\n\
         Its log is {} and its last lines are:\n{}",
        d.workspace.display(),
        d.socket.display(),
        d.log.display(),
        tail(&d.log, 15)
    ))
}

/// The last `n` lines of a file, or the reason it could not be read.
fn tail(path: &Path, n: usize) -> String {
    let Ok(text) = std::fs::read_to_string(path) else {
        return format!("(could not read {})", path.display());
    };
    let lines: Vec<&str> = text.lines().rev().take(n).collect();
    lines.into_iter().rev().collect::<Vec<&str>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The key is the shell's key.** These four values are the same ones
    /// `crates/letibot/src/launcher.rs` asserts against, and each is the filename of a
    /// record on the operator's box that names the workspace on the left. If they ever
    /// disagree, the bridge and the launcher are talking about two different sockets for
    /// one folder.
    #[test]
    fn the_key_matches_what_the_launcher_derives() {
        for (workspace, want) in [
            ("/home/dead/Projects/letibot", "42ce9f1aae08"),
            ("/home/dead/Projects/dbtest/pg-noop", "5a0a8f3c88f8"),
            ("/home/dead", "7a22aa3b1a21"),
            ("/home/dead/.emacs.d", "9f6194e2446d"),
        ] {
            assert_eq!(
                key(Path::new(workspace)),
                want,
                "a different key from the launcher's names a socket no daemon listened on"
            );
        }
    }

    /// The socket is the key plus one suffix, under the rundir — the whole contract of
    /// "which daemon belongs to this folder", checked on the path rather than on a digest.
    ///
    /// Built from a literal key rather than from `resolve`, because `resolve` asks the
    /// filesystem about a folder and this assertion is about the naming rule — a test that
    /// needed the operator's own checkouts to exist would be reporting its box.
    #[test]
    fn the_socket_is_the_key_under_the_rundir() {
        assert_eq!(
            socket_in(Path::new("/run/user/1000/letibot"), "42ce9f1aae08"),
            Path::new("/run/user/1000/letibot/42ce9f1aae08.sock")
        );
        assert_eq!(
            key(Path::new("/home/dead/Projects/letibot")),
            "42ce9f1aae08"
        );
    }

    /// A daemon value for the flag tests, spelled out rather than resolved: no filesystem,
    /// no environment, no `$HOME`.
    fn a_daemon() -> Daemon {
        Daemon {
            workspace: PathBuf::from("/home/dead/Projects/letibot"),
            key: "42ce9f1aae08".into(),
            socket: PathBuf::from("/run/user/1000/letibot/42ce9f1aae08.sock"),
            log: PathBuf::from("/home/dead/logs/harnessd-42ce9f1aae08.log"),
            store: PathBuf::from("/home/dead/.local/share/letibot/sessions.db"),
            started: false,
        }
    }

    /// A folder that does not exist is refused by name, never hashed into an empty
    /// workspace: the shell's own guard exists because that failure rejoined another
    /// project's daemon.
    #[test]
    fn a_folder_that_is_not_there_is_refused() {
        let e = resolve(Path::new("/definitely/not/a/checkout")).expect_err("refused");
        assert!(e.contains("definitely/not/a/checkout"), "{e}");
    }

    /// **A daemon the bridge started is waited for, and is still nobody's child to kill.**
    ///
    /// The two halves are one property each and both are the reason the reaper is a waiting
    /// thread and not a `SIGCHLD` handler:
    ///
    /// * the child is started in a session of its own, so the bridge going away leaves it
    ///   running — asserted while it is alive, before anything waits for it;
    /// * when it exits it is REAPED — `kill(pid, 0)` answers `ESRCH` rather than `0`, which is
    ///   the difference between *gone from the process table* and *a zombie that `ps` still
    ///   counts as a running daemon*. MEASURED 2026-10-26: without the reaper this assertion
    ///   hangs at its deadline for ever, which is exactly what one bridge's twelve `<defunct>`
    ///   harnessds were.
    ///
    /// The binary is a shell script rather than `harnessd`, so the test needs no GGUF, no
    /// socket and no daemon of the operator's — what it drives is the start path `ensure`
    /// drives, not what the daemon does once it is up.
    #[test]
    fn a_daemon_the_bridge_started_is_reaped_when_it_exits() {
        use std::os::unix::fs::PermissionsExt;

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = letibot_sessionlog::testing::scratch_root()
            .join(format!("letibot-reap-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let script = dir.join("harnessd");
        // Two seconds: long enough to be seen alive and in its own session, short enough not to
        // hold the suite.
        std::fs::write(&script, "#!/bin/sh\nsleep 2\n").expect("write the script");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .expect("make the script executable");

        let d = Daemon {
            workspace: dir.clone(),
            key: "reap00000000".into(),
            socket: dir.join("daemon.sock"),
            log: dir.join("daemon.log"),
            store: dir.join("sessions.db"),
            started: false,
        };
        let pid = spawn(&script, &d, None, "writes-allowed").expect("the script starts");
        let pid = pid as libc::pid_t;

        // **Its own session**, so nothing the bridge's process group is sent reaches it: a bridge
        // that exited would leave this daemon running, which is the property `setsid` is for.
        // SAFETY: `getsid` reads a kernel table and cannot fail for a live pid of ours.
        let sid = unsafe { libc::getsid(pid) };
        assert_eq!(
            sid, pid,
            "a daemon the bridge started must be in a session of its own (got {sid}), or the \
             reaper would be killing what it is only supposed to wait for"
        );
        std::thread::sleep(std::time::Duration::from_millis(300));
        // SAFETY: `kill` with signal 0 only asks whether the process exists.
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            0,
            "the reaper must not shorten the daemon's life: it waits, it does not own"
        );

        // **And it is reaped when it goes.** A zombie answers `kill(pid, 0)` with 0 — it is still
        // in the process table — so this is the assertion the defect fails, and it fails by
        // running out of deadline rather than by passing a zombie off as a process.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            // SAFETY: as above.
            let alive = unsafe { libc::kill(pid, 0) } == 0;
            if !alive {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "pid {pid} is still in this process's table ten seconds after it exited: that \
                 is a `<defunct>` harnessd, which is what every daemon the bridge stopped used to \
                 leave behind. The wait belongs in `reap`."
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **A package's daemon serves no queue.** The flag is not a nicety: without it the daemon
    /// in a package's worktree is one of the repository's session daemons, takes its lease and
    /// tells the package's model to fix `main` (2026-10-26). Asserted on the argument list
    /// rather than on a started daemon, because what has to hold is the flag every start
    /// carries — `ensure` does not touch a daemon that is already up.
    #[test]
    fn every_daemon_the_bridge_starts_serves_no_queue() {
        let d = a_daemon();
        for model in [None, Some("deepseek"), Some("qwen-3.8-27b")] {
            let args = daemon_args(&d, model, "writes-allowed");
            assert!(
                args.iter().any(|a| a == "--no-queue"),
                "a daemon started without --no-queue claims the repository's lease: {args:?}"
            );
        }
    }

    /// A provider is `--provider NAME`; anything else is the local binding `--model`. The
    /// launcher never passes both for a provider (`MODEL_ARGS=()` when `--provider` is
    /// given), and a bridge that invented `--model deepseek` would bind a local alias no
    /// server serves.
    #[test]
    fn a_provider_is_a_provider_and_a_model_is_a_binding() {
        let d = a_daemon();
        let args = daemon_args(&d, Some("deepseek"), "writes-allowed");
        assert!(
            args.windows(2).any(|w| w == ["--provider", "deepseek"]),
            "{args:?}"
        );
        assert!(!args.iter().any(|a| a == "--model"), "{args:?}");
        let args = daemon_args(&d, Some("qwen-3.8-27b"), "writes-allowed");
        assert!(
            args.windows(2).any(|w| w == ["--model", "qwen-3.8-27b"]),
            "{args:?}"
        );
        assert!(!args.iter().any(|a| a == "--provider"), "{args:?}");
        let args = daemon_args(&d, Some("deepseek/deepseek-chat"), "writes-allowed");
        assert!(
            args.windows(2).any(|w| w == ["--provider", "deepseek"]),
            "{args:?}"
        );
        assert!(
            args.windows(2).any(|w| w == ["--model", "deepseek-chat"]),
            "{args:?}"
        );
        // No model named at all: neither flag, and the daemon's own choice stands.
        let args = daemon_args(&d, None, "writes-allowed");
        assert!(
            !args.iter().any(|a| a == "--provider" || a == "--model"),
            "{args:?}"
        );
    }
}
