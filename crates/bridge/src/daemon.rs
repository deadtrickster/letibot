//! Where a folder's daemon lives, and how to start one, from a head. A session must live in the
//! folder's existing daemon, or two daemons interleave one transcript. A port of the launcher's
//! paths: socket `<rundir>/<sha256(workspace)[..12]>.sock`, store `$LETIBOT_STORE` or the config dir.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use letibot_tools::builtins::task::Placement;

/// The name a head attaches under - a fact of the daemon's log, so it is the bridge's own and
/// not a guess: `HeadAttached`/`HeadDetached` name it. It is also the mark the session list
/// groups by, so a conversation a package started is drawn under `packages`.
pub const IDENTITY: &str = letibot_sessionlog::MCP_IDENTITY;

/// The handshake kind this head sends. The protocol's vocabulary is `tui`, `remote`,
/// `flowy`, `acp`; a program holding a session is `remote`, which is what its own doc
/// calls a head that is not a terminal.
pub const HEAD_KIND: &str = "remote";

/// The directory the runtime files live in: `$XDG_RUNTIME_DIR/letibot`, else
/// `/run/user/<uid>/letibot` on Linux and the per-user temp dir on macOS - `launcher::rundir`'s
/// rule, asked of the system rather than read from `$TMPDIR`.
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
/// the path with nothing added - the shell writes `printf '%s' "$WORKSPACE" | sha12`, and a
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
    /// The workspace the key was taken from - the git toplevel, as the launcher resolves
    /// it, because `letibot` in a subdirectory joins the repository's daemon.
    pub workspace: PathBuf,
    pub key: String,
    pub socket: PathBuf,
    pub log: PathBuf,
    /// Where the daemon keeps transcripts. `$LETIBOT_STORE` wins, as it does in the shell.
    pub store: PathBuf,
    /// Where this daemon's tools run - the record it wrote beside its socket
    /// ([`letibot_sessionlog::placement`]), so this is the daemon's own answer. `None` is
    /// *nothing is known*, never *local*: see [`ensure_here`], which refuses rather than guesses.
    pub placement: Option<Placement>,
    /// True when THIS call started the daemon, so the answer can say so - a bridge that
    /// claimed to have started a daemon somebody else's process was already serving would
    /// be describing the wrong process.
    pub started: bool,
}

/// `$HOME`, or a refusal rather than a path spelled `~`.
/// The store a daemon is handed: `$LETIBOT_STORE`, else the data directory. A DATA DIRECTORY,
/// not `sessions.db`: the daemon opens `shared.db` and `projects/<key>.db` under it.
pub(crate) fn default_store() -> Result<PathBuf, String> {
    match std::env::var_os("LETIBOT_STORE") {
        Some(s) if !s.is_empty() => Ok(PathBuf::from(s)),
        _ => Ok(store_in(&data_dir()?)),
    }
}

/// The daemon's data directory, as `letibot_tools::lifecycle::data_dir` reads it:
/// `$XDG_DATA_HOME/letibot` first, so a run with its own data home never reaches the operator's.
fn data_dir() -> Result<PathBuf, String> {
    data_dir_of(std::env::var_os("XDG_DATA_HOME"), std::env::var_os("HOME"))
}

fn data_dir_of(
    xdg: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Result<PathBuf, String> {
    if let Some(d) = xdg.map(PathBuf::from).filter(|p| p.is_absolute()) {
        return Ok(d.join("letibot"));
    }
    home.map(|h| PathBuf::from(h).join(".local/share/letibot"))
        .ok_or_else(|| "no $HOME: the store has no default without one".to_string())
}

fn home() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "no $HOME: the store and the log have no default without one".to_string())
}

/// The workspace a path belongs to, as the launcher resolves it: the git toplevel when the
/// path is inside a checkout, else the path itself. A path that does not exist is refused by
/// name rather than hashed: an empty workspace is a socket key everything shares.
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

/// The socket a key names under a rundir - one spelling, so `resolve` and the test that
/// pins the rule cannot drift.
pub(crate) fn socket_in(rundir: &Path, key: &str) -> PathBuf {
    rundir.join(format!("{key}.sock"))
}

/// The record beside the socket: what this folder's daemon is. A socket names a hash
/// and nothing else - not the folder, not the role - so the record is the one place those facts
/// exist, and the shell reads it with `sed`: a flat object, `pid` as a NUMBER, named `<key>.json`.
fn write_record(d: &Daemon, pid: u32, model: Option<&str>) -> std::io::Result<PathBuf> {
    let answers = match model {
        Some(m) => match m.split_once('/') {
            Some((provider, name)) => format!("{provider}/{name}"),
            None if is_provider(m) => m.to_string(),
            None => String::new(),
        },
        None => String::new(),
    };
    // `role` is the seat `daemon_args` starts, and `dialect` is empty because this bridge
    // does not decide one - the daemon reads it from its model. An empty field is what the shell
    // writes for a fact it does not have, and its readers treat it as absent.
    let record = serde_json::json!({
        "workspace": d.workspace.display().to_string(),
        "socket": d.socket.display().to_string(),
        "pid": pid,
        "role": "leticode",
        "model": model.unwrap_or_default(),
        "answers": answers,
        "dialect": "",
        "bash": true,
        "started": rfc3339_utc_now(),
    });
    let path = d
        .socket
        .parent()
        .unwrap_or(Path::new("."))
        .join(format!("{}.json", d.key));
    // Written whole, through a temp name and a rename, so a launcher reading the glob at
    // the same moment sees the old file or the new one and never half of either.
    let tmp = path.with_extension(format!("json.{}", std::process::id()));
    std::fs::write(&tmp, serde_json::to_string(&record).unwrap_or_default())?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

/// RFC3339 UTC to the second, with no `chrono`: this crate pulls nothing it does not need.
fn rfc3339_utc_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // Civil from days (Howard Hinnant's algorithm), the same one `flowy::seat` uses.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// The daemon for a workspace, without asking whether it is up.
pub fn resolve(workspace: &Path) -> Result<Daemon, String> {
    let workspace = workspace_of(workspace)?;
    let key = key(&workspace);
    // `$LETIBOT_SOCKET` is authoritative when set, exactly as in the shell: it is how a
    // test - or a second head on the same folder - names a daemon the hash does not.
    let socket = match std::env::var_os("LETIBOT_SOCKET") {
        Some(s) if !s.is_empty() => PathBuf::from(s),
        _ => socket_in(&rundir(), &key),
    };
    let log = match std::env::var_os("LETIBOT_LOG") {
        Some(s) if !s.is_empty() => PathBuf::from(s),
        _ => home()?.join("logs").join(format!("harnessd-{key}.log")),
    };
    let store = default_store()?;
    // The daemon's own record, with its socket's identity in it: a record left by a daemon that
    // is gone answers nothing, which the caller must treat as *unknown*.
    let placement =
        letibot_sessionlog::placement::read(&socket).and_then(|s| Placement::parse(&s).ok());
    Ok(Daemon {
        workspace,
        key,
        socket,
        log,
        store,
        placement,
        started: false,
    })
}

/// The store to hand the daemon: the data directory, or its `sessions.db` while the directory has
/// not been split yet. The daemon refuses an unsplit directory but still opens the old file.
pub(crate) fn store_in(dir: &Path) -> PathBuf {
    let old = dir.join("sessions.db");
    if old.is_file() && !dir.join("shared.db").exists() {
        old
    } else {
        dir.to_path_buf()
    }
}

/// Is something accepting connections on this socket right now? A connect, not a `stat`: the
/// daemon's own shutdown unlinks the socket file while the process goes on listening on the
/// nameless inode, so "is there a file" answers *no* for a folder that has a daemon.
pub fn is_up(socket: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(socket).is_ok()
}

/// The daemon's binary: `$LETIBOT_BIN`, then beside this executable, then `harnessd` on `PATH`.
/// The one beside the bridge is its own build, so it speaks the same store layout; an older
/// install earlier on `PATH` would be handed flags it does not know.
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
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let p = dir.join("harnessd");
        if p.is_file() {
            return Ok(p);
        }
    }
    if let Some(path) = std::env::var_os("PATH")
        && let Some(found) = std::env::split_paths(&path)
            .map(|d| d.join("harnessd"))
            .find(|p| p.is_file())
    {
        return Ok(found);
    }
    Err("no harnessd: set $LETIBOT_BIN to the directory holding it, or put it on PATH".into())
}

/// The daemon's flags, as the `leticode` wrapper passes them. A seat, an approval point and
/// a shell are what make a session answerable at all. `--no-queue` because a package worker is
/// not the repository's queue holder, and without it the daemon runs the queue itself.
fn daemon_args(
    d: &Daemon,
    model: Option<&str>,
    mode: &str,
    placement: &Placement,
    session: Option<&str>,
) -> Vec<String> {
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
        // the daemon's own stdin, and a detached daemon has none - every gated call would
        // refuse with "nobody answered".
        "--adjudicator".into(),
        "head".into(),
        "--mode".into(),
        mode.into(),
        "--bash".into(),
        // See this function's doc: a package's daemon serves no queue.
        "--no-queue".into(),
        // A package is this seat's child, not a top-level service, so it stays out of the room.
        "--parent-seat".into(),
        crate::rooms::handle(),
    ];
    // The id the daemon's own first session must have. A package's owner row, its worktree and the
    // branch `pk/<sid8>/<name>` are all named by it, and all three exist before the session does, so
    // the daemon is told which id to open rather than minting one of its own.
    if let Some(id) = session.filter(|s| !s.trim().is_empty()) {
        args.push("--session".into());
        args.push(id.into());
    }
    if let Some(host) = placement.host() {
        args.push("--host".into());
        args.push(host.into());
    }
    // A VM ON that machine, which is the pair `--host NAME --where firecode` and not another
    // placement: the host's own `firecode` boots the guest over this daemon's ssh link. Spelled
    // the way harnessd parses it - `--vm` is the launcher's alias, expanded before it gets here.
    if placement.is_vm() {
        args.push("--where".into());
        args.push("firecode".into());
    }
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
/// child. `setsid` rather than a plain spawn: a daemon that died with the MCP process would take
/// every session it holds with it, which is what the two-process split exists to deny.
fn start(
    binary: &Path,
    d: &Daemon,
    model: Option<&str>,
    mode: &str,
    placement: &Placement,
    session: Option<&str>,
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
    cmd.args(daemon_args(d, model, mode, placement, session))
        // The daemon links libllama for its TOKENIZER, and ggml registers every backend at
        // load - 1.1 GB of a card for ops it never runs. `scripts/letibot` blindfolds it
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

/// Every child this process starts is waited for - the tree's rule: *any process letibot
/// starts is supervised*. A `Child` is not waited for on drop, so a bridge that dropped it at
/// the pid left every exited daemon `<defunct>`. Waiting is not owning: the child is `setsid`'d.
fn reap(mut child: std::process::Child) -> Exited {
    let pid = child.id();
    let exited = Exited::default();
    let flag = exited.clone();
    let spawned = std::thread::Builder::new()
        .name(format!("reap-{pid}"))
        .spawn(move || {
            // The status is not this thread's business: the log's tail says why. That it ended
            // is, because the start wait gives up on an exited daemon at once.
            let _ = child.wait();
            flag.0.store(true, std::sync::atomic::Ordering::Release);
        });
    if let Err(e) = spawned {
        // Said rather than silent, and the daemon is not harmed by it: the only thing lost
        // is this process's tidiness, which is exactly the defect this function exists for - so
        // a bridge that cannot afford a thread says so on its stderr instead of leaking quietly.
        eprintln!(
            "  harnessd pid {pid} will not be waited for: the reaping thread could not be \
             started ({e}). It becomes a zombie of this process when it exits; the daemon itself \
             is unaffected and outlives this bridge."
        );
    }
    exited
}

/// This folder's daemon started, detached, and reaped when it exits - the pid the answer
/// names and nothing else about the child. The binary is the caller's so that the start path a
/// test drives is the one a daemon start really takes; resolving it is `ensure`'s business.
#[allow(clippy::too_many_arguments)]
fn spawn(
    binary: &Path,
    d: &Daemon,
    model: Option<&str>,
    mode: &str,
    placement: &Placement,
    session: Option<&str>,
) -> Result<(u32, Exited), String> {
    let child = start(binary, d, model, mode, placement, session)?;
    let pid = child.id();
    let exited = reap(child);
    Ok((pid, exited))
}

/// Set once a started daemon has exited and been reaped.
#[derive(Clone, Default)]
struct Exited(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl Exited {
    fn yes(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Acquire)
    }
}

/// How long a started daemon that is still running gets to bind its socket. Long, because on a
/// loaded box its store opens queue behind other daemons' writes; an exited daemon fails at once.
pub const START_PATIENCE: std::time::Duration = std::time::Duration::from_secs(300);

/// What the start wait saw: the socket, the daemon gone, or the patience spent.
#[derive(Debug, PartialEq, Eq)]
enum Came {
    Up,
    Exited,
    TimedOut,
}

/// Poll `up` until it answers, the daemon exits, or `patience` runs out.
fn wait_up(
    up: impl Fn() -> bool,
    exited: &Exited,
    patience: std::time::Duration,
    step: std::time::Duration,
) -> Came {
    let deadline = std::time::Instant::now() + patience;
    loop {
        if up() {
            return Came::Up;
        }
        // Read after `up`, so a daemon that bound and then exited is not called gone.
        if exited.yes() {
            return if up() { Came::Up } else { Came::Exited };
        }
        if std::time::Instant::now() >= deadline {
            return Came::TimedOut;
        }
        std::thread::sleep(step);
    }
}

/// The launcher that starts a coordinator: `$LETIBOT_ROOM_SUPERVISOR`, else `letibot` on `PATH`.
fn room_launcher() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("LETIBOT_ROOM_SUPERVISOR").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(p)).filter(|p| p.is_file());
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join("letibot"))
        .find(|c| c.is_file())
}

/// Make sure the host coordinator is serving before a daemon opens its sessions, which join the
/// rooms at open. `letibot --room` is the launcher's idempotent verb. A failure is a sentence,
/// never fatal.
pub fn ensure_room(launcher: &Path) -> Result<(), String> {
    let ended = letibot_tools::rpc::run_bounded(
        launcher,
        &["--room".to_string()],
        std::time::Duration::from_secs(60),
    );
    match ended {
        letibot_tools::rpc::Ended::Answered(out) if out.status.success() => Ok(()),
        letibot_tools::rpc::Ended::Answered(out) => Err(format!(
            "`{} --room` answered {}",
            launcher.display(),
            out.status
        )),
        letibot_tools::rpc::Ended::TimedOut => Err(format!(
            "`{} --room` did not finish within 60s",
            launcher.display()
        )),
        letibot_tools::rpc::Ended::Unstartable(why) => Err(format!(
            "`{} --room` could not run: {why}",
            launcher.display()
        )),
    }
}

/// [`ensure_room`] for this host, said once on stderr when it cannot. The daemon still starts: a
/// session with no room works, and its `room` tool refuses by name.
fn ensure_host_room() {
    let Some(launcher) = room_launcher() else {
        return;
    };
    if let Err(why) = ensure_room(&launcher) {
        eprintln!("  the host room did not start ({why}); sessions will not be in any room");
    }
}

/// This folder's daemon, started if nothing is listening, and never one whose placement is
/// not the one asked for. A daemon already up with another placement is refused, not reused
/// and not stopped: stopping it would end sessions somebody else is running.
pub fn ensure(
    worktree: &Path,
    model: Option<&str>,
    mode: &str,
    placement: &Placement,
) -> Result<Daemon, String> {
    ensure_with(worktree, model, mode, placement, None)
}

/// [`ensure`], with the id the daemon's own first session must carry: a package's owner row and the
/// worktree and branch it names exist before the session does (`docs/lifecycle.md` G3), so the id is
/// the caller's to name and the daemon opens the session it is told to.
pub fn ensure_with(
    worktree: &Path,
    model: Option<&str>,
    mode: &str,
    placement: &Placement,
    session: Option<&str>,
) -> Result<Daemon, String> {
    let d = resolve(worktree)?;
    // Not in `ensure_here`: that is the shape a test drives with a stub binary, and it must not
    // start a real coordinator.
    if !is_up(&d.socket) {
        ensure_host_room();
    }
    ensure_here_with(d, model, mode, placement, session)
}

/// [`ensure`] for a daemon already resolved - the shape a test drives, with a socket it bound
/// itself and no folder to hash.
pub fn ensure_here(
    d: Daemon,
    model: Option<&str>,
    mode: &str,
    placement: &Placement,
) -> Result<Daemon, String> {
    ensure_here_with(d, model, mode, placement, None)
}

/// [`ensure_here`] with the id the daemon's first session must carry - see [`ensure_with`].
pub fn ensure_here_with(
    mut d: Daemon,
    model: Option<&str>,
    mode: &str,
    placement: &Placement,
    session: Option<&str>,
) -> Result<Daemon, String> {
    if is_up(&d.socket) {
        if d.placement.as_ref() == Some(placement) {
            return Ok(d);
        }
        return Err(placed_elsewhere(&d, placement));
    }
    // A second daemon for one folder is what corrupts a transcript, and the refusal that
    // catches it is harnessd's own folder claim. So the start is attempted, and a refusal
    // reaches the caller as the log tail rather than being pre-empted by a guess.
    let (pid, exited) = spawn(&daemon_binary()?, &d, model, mode, placement, session)?;
    let came = wait_up(
        || is_up(&d.socket),
        &exited,
        START_PATIENCE,
        std::time::Duration::from_millis(500),
    );
    if came == Came::Up {
        d.started = true;
        // The record the launcher's own verbs read: `--ls`,
        // `--stop N` and the attach banner all read `<key>.json` beside the socket, so a
        // daemon started here must leave one. A record that cannot be written is said.
        if let Err(e) = write_record(&d, pid, model) {
            eprintln!(
                "  harnessd pid {pid} came up and its record could not be written: {e}. \
                 `letibot --ls` will not list it until a launcher starts this folder's \
                 daemon itself."
            );
        }
        // The daemon's own record, read back rather than echoed: what it wrote is what a
        // later head will read. The flags this call passed are the fallback for a daemon
        // whose build predates the record.
        d.placement = letibot_sessionlog::placement::read(&d.socket)
            .and_then(|s| Placement::parse(&s).ok())
            .or_else(|| Some(placement.clone()));
        return Ok(d);
    }
    let how = match came {
        Came::Exited => "it exited before anything was accepting on".to_string(),
        _ => format!(
            "it is still running, and nothing was accepting within {}s on",
            START_PATIENCE.as_secs()
        ),
    };
    Err(format!(
        "the daemon for {} (pid {pid}) did not come up: {how} {}.\n\
         Its log is {} and its last lines are:\n{}",
        d.workspace.display(),
        d.socket.display(),
        d.log.display(),
        tail(&d.log, 15)
    ))
}

/// The refusal for a daemon that is already up and placed somewhere else. It says three
/// things, because a refusal a caller cannot act on costs an hour: where the running daemon's
/// tools run, that this bridge will not change it, and the one command that ends it.
fn placed_elsewhere(d: &Daemon, asked: &Placement) -> String {
    let running = match d.placement.as_ref().and_then(Placement::host) {
        Some(host) => format!("its tools run on `{host}`"),
        None => match d.placement {
            Some(_) => "its tools run on this box".to_string(),
            None => format!(
                "it recorded no placement beside its socket ({}), so this bridge cannot tell \
                 whether its tools run here or on a host — a daemon from a build before the \
                 record existed, or one whose record belongs to a socket that is gone",
                letibot_sessionlog::placement::path(&d.socket).display()
            ),
        },
    };
    format!(
        "a daemon is already up for {} ({}) and {running}, but this call asked for `{}`. \
         Nothing was started and nothing was joined: a package that joined it would run its \
         tools somewhere other than it was told, and no answer would say so. Stop it — \
         `letibot --stop {}` (or `letibot --stop` in that folder) — and call again; the next \
         start is this call's own and carries the placement it asked for",
        d.workspace.display(),
        d.socket.display(),
        asked.as_str(),
        d.workspace.display()
    )
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

    /// A daemon still starting is waited for past the old fixed 30 s; a slow store at boot held
    /// one for 40 s and the bridge gave up on a daemon that came up moments later.
    #[test]
    fn a_daemon_still_running_is_waited_for_past_thirty_seconds() {
        assert!(START_PATIENCE >= std::time::Duration::from_secs(120));
        let polls = std::sync::atomic::AtomicU32::new(0);
        let came = wait_up(
            || polls.fetch_add(1, std::sync::atomic::Ordering::Relaxed) >= 80,
            &Exited::default(),
            START_PATIENCE,
            std::time::Duration::ZERO,
        );
        assert_eq!(
            came,
            Came::Up,
            "60 polls was the old limit; this one bound on the 81st"
        );
    }

    /// A daemon that exited is not waited for at all: the caller gets the log tail now.
    #[test]
    fn an_exited_daemon_fails_the_wait_at_once() {
        let exited = Exited::default();
        exited.0.store(true, std::sync::atomic::Ordering::Release);
        let t = std::time::Instant::now();
        let came = wait_up(
            || false,
            &exited,
            START_PATIENCE,
            std::time::Duration::from_millis(500),
        );
        assert_eq!(came, Came::Exited);
        assert!(
            t.elapsed() < std::time::Duration::from_secs(5),
            "{:?}",
            t.elapsed()
        );
        assert_eq!(
            wait_up(
                || false,
                &Exited::default(),
                std::time::Duration::ZERO,
                std::time::Duration::ZERO
            ),
            Came::TimedOut,
            "and a running daemon still has a ceiling"
        );
    }

    #[test]
    fn an_unsplit_data_dir_hands_the_daemon_its_sessions_db() {
        let dir = std::env::temp_dir().join(format!("letibot-store-in-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        assert_eq!(store_in(&dir), dir, "an empty dir is a new data directory");
        std::fs::write(dir.join("sessions.db"), b"").expect("old store");
        assert_eq!(
            store_in(&dir),
            dir.join("sessions.db"),
            "unsplit: the old file"
        );
        std::fs::write(dir.join("shared.db"), b"").expect("split store");
        assert_eq!(store_in(&dir), dir, "split: the directory");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The store follows `$XDG_DATA_HOME` before `$HOME`, as the daemon's lifecycle does: a run
    /// with a data home of its own must never resolve the operator's store.
    #[test]
    fn the_data_dir_is_xdg_data_home_first() {
        let some = |s: &str| Some(std::ffi::OsString::from(s));
        assert_eq!(
            data_dir_of(some("/tmp/x/data"), some("/home/op")).unwrap(),
            Path::new("/tmp/x/data/letibot")
        );
        assert_eq!(
            data_dir_of(None, some("/home/op")).unwrap(),
            Path::new("/home/op/.local/share/letibot")
        );
        // A relative value is ignored, as the XDG spec says.
        assert_eq!(
            data_dir_of(some("rel"), some("/home/op")).unwrap(),
            Path::new("/home/op/.local/share/letibot")
        );
        assert!(data_dir_of(None, None).is_err());
    }

    /// The key is the shell's key. These four values are the same ones
    /// `crates/letibot/src/launcher.rs` asserts against; if they ever disagree, the bridge and
    /// the launcher are talking about two different sockets for one folder.
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

    /// The socket is the key plus one suffix, under the rundir - the whole contract of "which
    /// daemon belongs to this folder". Built from a literal key rather than from `resolve`,
    /// because a test that needed the operator's own checkouts would be reporting its box.
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
            placement: Some(Placement::Host),
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

    /// A daemon the bridge opens needs the host coordinator, so the bridge asks the launcher
    /// for it with `--room`, and a launcher that fails is a sentence.
    #[test]
    fn the_bridge_asks_the_launcher_for_a_coordinator() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = letibot_sessionlog::testing::scratch_root().join(format!(
            "letibot-ensure-room-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let log = dir.join("asked");
        let launcher = dir.join("letibot");
        letibot_tools::testing::write_stub(
            &launcher,
            &format!("#!/bin/sh\necho \"$@\" >> {}\n", log.display()),
        );
        assert_eq!(ensure_room(&launcher), Ok(()));
        // `write_stub` probes the script with no arguments, which logs an empty line first.
        let asked = std::fs::read_to_string(&log)
            .unwrap()
            .lines()
            .filter(|l| *l == "--room")
            .count();
        assert_eq!(asked, 1);

        let failing = dir.join("letibot-bad");
        letibot_tools::testing::write_stub(&failing, "#!/bin/sh\nexit 3\n");
        assert!(ensure_room(&failing).is_err());
    }

    /// A daemon the bridge started is waited for, and is still nobody's child to kill. The
    /// child is started in a session of its own, so the bridge going away leaves it running; and
    /// when it exits it is REAPED - `kill(pid, 0)` answers `ESRCH`, not `0`.
    #[test]
    fn a_daemon_the_bridge_started_is_reaped_when_it_exits() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = letibot_sessionlog::testing::scratch_root()
            .join(format!("letibot-reap-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let script = dir.join("harnessd");
        // Two seconds: long enough to be seen alive and in its own session, short enough not to
        // hold the suite - and nothing at all with no arguments, which is the probe `write_stub`
        // runs. Written through that helper because write-then-exec races every fork in this binary.
        letibot_tools::testing::write_stub(&script, "#!/bin/sh\n[ $# -eq 0 ] && exit 0\nsleep 2\n");

        let d = Daemon {
            workspace: dir.clone(),
            key: "reap00000000".into(),
            socket: dir.join("daemon.sock"),
            log: dir.join("daemon.log"),
            store: dir.join("sessions.db"),
            placement: None,
            started: false,
        };
        let (pid, _) = spawn(&script, &d, None, "writes-allowed", &Placement::Host, None)
            .expect("the script starts");
        let pid = pid as libc::pid_t;

        // Its own session, so nothing the bridge's process group is sent reaches it: a bridge
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

        // And it is reaped when it goes. A zombie answers `kill(pid, 0)` with 0 - it is still
        // in the process table - so this is the assertion the defect fails, and it fails by
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

    /// A package's daemon serves no queue. Without the flag the daemon in a package's
    /// worktree is one of the repository's session daemons, takes its lease and tells the
    /// package's model to fix `main`. Asserted on the argument list, not on a started daemon.
    #[test]
    fn every_daemon_the_bridge_starts_serves_no_queue() {
        let d = a_daemon();
        for model in [None, Some("deepseek"), Some("qwen-3.8-27b")] {
            let args = daemon_args(&d, model, "writes-allowed", &Placement::Host, None);
            assert!(
                args.iter().any(|a| a == "--no-queue"),
                "a daemon started without --no-queue claims the repository's lease: {args:?}"
            );
        }
    }

    /// A package is the seat's child: every daemon the bridge starts names the seat, so its
    /// sessions are not top-level services and never join the room.
    #[test]
    fn every_daemon_the_bridge_starts_names_its_parent_seat() {
        let d = a_daemon();
        let args = daemon_args(&d, None, "writes-allowed", &Placement::Host, None);
        let at = args
            .iter()
            .position(|a| a == "--parent-seat")
            .unwrap_or_else(|| panic!("no --parent-seat: {args:?}"));
        assert!(
            args.get(at + 1).is_some_and(|h| h.starts_with("cc@")),
            "{args:?}"
        );
    }

    /// A provider is `--provider NAME`; anything else is the local binding `--model`. A bridge
    /// that invented `--model deepseek` would bind a local alias no server serves.
    #[test]
    fn a_provider_is_a_provider_and_a_model_is_a_binding() {
        let d = a_daemon();
        let args = daemon_args(
            &d,
            Some("deepseek"),
            "writes-allowed",
            &Placement::Host,
            None,
        );
        assert!(
            args.windows(2).any(|w| w == ["--provider", "deepseek"]),
            "{args:?}"
        );
        assert!(!args.iter().any(|a| a == "--model"), "{args:?}");
        let args = daemon_args(
            &d,
            Some("qwen-3.8-27b"),
            "writes-allowed",
            &Placement::Host,
            None,
        );
        assert!(
            args.windows(2).any(|w| w == ["--model", "qwen-3.8-27b"]),
            "{args:?}"
        );
        assert!(!args.iter().any(|a| a == "--provider"), "{args:?}");
        let args = daemon_args(
            &d,
            Some("deepseek/deepseek-chat"),
            "writes-allowed",
            &Placement::Host,
            None,
        );
        assert!(
            args.windows(2).any(|w| w == ["--provider", "deepseek"]),
            "{args:?}"
        );
        assert!(
            args.windows(2).any(|w| w == ["--model", "deepseek-chat"]),
            "{args:?}"
        );
        // No model named at all: neither flag, and the daemon's own choice stands.
        let args = daemon_args(&d, None, "writes-allowed", &Placement::Host, None);
        assert!(
            !args.iter().any(|a| a == "--provider" || a == "--model"),
            "{args:?}"
        );
    }

    /// A host is `--host NAME` on the daemon this call starts, and nothing when no host was
    /// asked for. The flag rather than a bridge-side backend: the placement is one fact of the
    /// daemon, and a bridge that ran ssh itself would be a second implementation of it.
    #[test]
    fn a_host_is_the_flag_the_daemon_is_started_with() {
        let d = a_daemon();
        let args = daemon_args(
            &d,
            None,
            "writes-allowed",
            &Placement::Remote("lab2x1".into()),
            None,
        );
        assert!(
            args.windows(2).any(|w| w == ["--host", "lab2x1"]),
            "a placement on a host must reach the daemon as --host: {args:?}"
        );
        // And once: two names would be one flag too many for harnessd's own parse.
        assert_eq!(
            args.iter().filter(|a| *a == "--host").count(),
            1,
            "{args:?}"
        );
        let args = daemon_args(&d, None, "writes-allowed", &Placement::Host, None);
        assert!(
            !args.iter().any(|a| a == "--host"),
            "a session on this box must not be started with a host: {args:?}"
        );
    }

    /// A VM on a host is BOTH asks, which is the one thing this module has to spell exactly:
    /// `--host NAME` alone is a session on that machine and `--where firecode` alone is a VM on
    /// this box, and harnessd reads the pair as a VM on the machine.
    #[test]
    fn a_vm_on_a_host_is_both_flags() {
        let d = a_daemon();
        let args = daemon_args(
            &d,
            None,
            "writes-allowed",
            &Placement::RemoteVm("lab2x1".into()),
            None,
        );
        assert!(
            args.windows(2).any(|w| w == ["--host", "lab2x1"]),
            "{args:?}"
        );
        assert!(
            args.windows(2).any(|w| w == ["--where", "firecode"]),
            "{args:?}"
        );
        // And a VM here is `--where firecode` alone: a host nobody named must not be invented.
        let args = daemon_args(&d, None, "writes-allowed", &Placement::Firecode, None);
        assert!(
            args.windows(2).any(|w| w == ["--where", "firecode"]),
            "{args:?}"
        );
        assert!(!args.iter().any(|a| a == "--host"), "{args:?}");
    }

    /// A daemon already up for the folder is joined only when it says it is placed where this
    /// call asked. A package must not end up running on the wrong machine because a daemon
    /// somebody else started happened to be listening, and it is not this call's to stop it.
    #[test]
    fn a_daemon_already_up_is_joined_only_where_this_call_asked() {
        let (d, _listener, dir) = a_listening_daemon("joined-only", "host");
        // Asked for this box, and it says it is on this box: joined, and not started.
        let joined = ensure_here(d.clone(), None, "writes-allowed", &Placement::Host)
            .expect("a daemon on this box is the daemon this call asked for");
        assert!(!joined.started, "nothing was started: it was already up");

        // Asked for a host: refused, naming where it actually is and how to stop it.
        let e = ensure_here(
            d.clone(),
            None,
            "writes-allowed",
            &Placement::Remote("lab2x1".into()),
        )
        .expect_err("a daemon running the tools here is not the one a host call asked for");
        assert!(e.contains("its tools run on this box"), "{e}");
        assert!(
            e.contains("letibot --stop"),
            "a refusal must say how to get what it wanted: {e}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A daemon placed on a host is joined by a call that asked for that host, and by nothing
    /// else - the other direction of the test above: a call asking for this box joining a
    /// daemon whose tools are on `lab2x1` would run *nothing* here.
    #[test]
    fn a_daemon_placed_on_a_host_is_not_joined_as_this_box() {
        let (d, _listener, dir) = a_listening_daemon("host-only", "host:lab2x1");
        let asked = Placement::Remote("lab2x1".into());
        assert!(
            ensure_here(d.clone(), None, "writes-allowed", &asked).is_ok(),
            "the same host is the same placement, and is joined"
        );
        let e = ensure_here(d.clone(), None, "writes-allowed", &Placement::Host)
            .expect_err("this box is not where that daemon runs the tools");
        assert!(e.contains("its tools run on `lab2x1`"), "{e}");
        let other = Placement::Remote("lab2x2".into());
        let e = ensure_here(d, None, "writes-allowed", &other).expect_err("another machine");
        assert!(e.contains("lab2x1"), "{e}");
        assert!(e.contains("lab2x2"), "the refusal says what was asked: {e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A daemon that recorded nothing is refused in both directions - `None` is *unknown*,
    /// never *local*: an older build, or a `.placement` left by a daemon that is gone. The
    /// refusal says which file it looked at, so the person who must stop it can see why.
    #[test]
    fn a_daemon_that_recorded_nothing_is_not_taken_for_this_box() {
        let (d, _listener, dir) = a_listening_daemon("no-record", "");
        let e = ensure_here(d.clone(), None, "writes-allowed", &Placement::Host)
            .expect_err("nothing recorded is not an answer");
        assert!(e.contains("recorded no placement"), "{e}");
        assert!(e.contains("letibot --stop"), "{e}");
        assert!(
            e.contains(".placement"),
            "the refusal names the file it read: {e}"
        );
        let e = ensure_here(
            d,
            None,
            "writes-allowed",
            &Placement::Remote("lab2x1".into()),
        )
        .expect_err("nothing recorded is not an answer for a host either");
        assert!(e.contains("recorded no placement"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A daemon value for a folder with a real socket bound at it and, when `record` is not
    /// empty, a placement record beside it. The listener is returned so it outlives the calls:
    /// dropping it would leave a socket nobody accepts on, which is a different test.
    fn a_listening_daemon(
        tag: &str,
        record: &str,
    ) -> (Daemon, std::os::unix::net::UnixListener, PathBuf) {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = letibot_sessionlog::testing::scratch_root().join(format!(
            "letibot-place-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let socket = dir.join("daemon.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).expect("a socket binds");
        if !record.is_empty() {
            letibot_sessionlog::placement::write(&socket, record).expect("the record is written");
        }
        let d = Daemon {
            workspace: dir.clone(),
            key: "place0000000".into(),
            socket,
            log: dir.join("daemon.log"),
            store: dir.join("sessions.db"),
            placement: letibot_sessionlog::placement::read(&dir.join("daemon.sock"))
                .and_then(|s| Placement::parse(&s).ok()),
            started: false,
        };
        (d, listener, dir)
    }
}
