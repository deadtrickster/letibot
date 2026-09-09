//! The execution-backend interface — the seam between W9 and W10.
//!
//! `docs/workstreams.md` §3 names this as one of the six interfaces worth fixing
//! early, and says exactly why:
//!
//! > **The execution-backend interface** for tools (`run(cmd, cwd, env) ->
//! > (stdout, stderr, exit)` plus read/write/list). Names the seam between W9 and
//! > W10, so tools ship host-local in M1 and firecode becomes a swap in M2 rather
//! > than a rewrite.
//!
//! And its §5 verdict on the W9 → W10 edge: *"tools are algorithms … firecode is a
//! **backend swap**, not a prerequisite, provided the execution interface is
//! named."* This module is that naming. Every built-in reaches the world through
//! this trait and through nothing else — no `std::fs` calls in the tools — which
//! is what makes the M2 swap one `Box<dyn ExecBackend>`.
//!
//! # What is implemented, and what is still refused
//!
//! [`HostBackend`] implements read, list, stat and — since W10 — **write**, but
//! only when it was opened with [`HostBackend::writable`]. [`HostBackend::new`]
//! still refuses, and that asymmetry is the second of the two gates a write must
//! pass; see the type's own docs.
//!
//! `run` is still refused outright. Exec needs §11.4's boundary — *the guest sees
//! a copy of one project and nothing else of the host* — and a boundary is a
//! substrate, not a policy: no adjudicator on this side of it can make an
//! unsandboxed `bash` on the operator's box into something the plan describes. It
//! arrives with the firecode backend, which implements all four methods with
//! nothing above this line changing.

use std::path::{Component, Path, PathBuf};
use std::sync::atomic::AtomicU64;

/// Distinguishes the temp files two concurrent writes create in one directory.
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// A command to run. `run(cmd, cwd, env)` in the shape the plan names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    /// argv, not a shell string. A shell string is a parser somebody else owns.
    pub argv: Vec<String>,
    /// Relative to the backend's root.
    pub cwd: String,
    /// Additions to the environment, in a fixed order so a run is reproducible.
    pub env: Vec<(String, String)>,
}

/// What a run produced. Bytes, not `String`: a command that emits invalid UTF-8 is
/// a command, not an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit: i32,
}

/// One entry from `list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    /// Path relative to the backend root, using `/`.
    pub path: String,
    pub name: String,
    pub is_dir: bool,
    pub bytes: u64,
}

#[derive(Debug)]
pub enum BackendError {
    /// The path does not exist. Distinguished from every other failure because
    /// clause 1's whole behaviour hangs off it: a miss is where the tool has to
    /// help, and "does not exist" is the miss.
    NotFound(String),
    /// The path resolves outside the backend's root.
    Outside(String),
    NotADirectory(String),
    IsADirectory(String),
    /// Named, so the M2 swap is visible rather than mysterious.
    Unsupported(&'static str),
    Io(String),
}

impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BackendError::NotFound(p) => write!(f, "no such path: {p}"),
            BackendError::Outside(p) => write!(f, "path resolves outside the session root: {p}"),
            BackendError::NotADirectory(p) => write!(f, "not a directory: {p}"),
            BackendError::IsADirectory(p) => write!(f, "is a directory: {p}"),
            BackendError::Unsupported(w) => write!(f, "this backend cannot do that: {w}"),
            BackendError::Io(e) => write!(f, "io: {e}"),
        }
    }
}

impl std::error::Error for BackendError {}

/// The seam. Host today, firecode in M2, with nothing above it changing.
pub trait ExecBackend: Send + Sync {
    /// `run(cmd, cwd, env) -> (stdout, stderr, exit)`.
    fn run(&self, cmd: &Command) -> Result<Output, BackendError>;

    fn read(&self, path: &str) -> Result<Vec<u8>, BackendError>;

    /// Replace a file's contents.
    ///
    /// **Required to be atomic**, and that is part of the interface rather than an
    /// implementation note: a half-written file is worse than a refused edit,
    /// because it is a file whose contents nobody — not the model, not the
    /// operator, not the next tool call — can predict. An implementation that
    /// cannot write atomically must fail rather than write partially.
    ///
    /// Missing parent directories are created. A tool that had to call `mkdir`
    /// first would either need a `mkdir` in this trait or a shell, and both are
    /// larger holes than this is a convenience.
    fn write(&self, path: &str, bytes: &[u8]) -> Result<(), BackendError>;

    /// The process host, when this backend can start a process whose **lifetime
    /// is owned by something**.
    ///
    /// `None` by default, and that default is the whole safety property: an
    /// existing backend gains no exec path by this method existing, and a tool
    /// that wants one gets a refusal naming the backend rather than a process
    /// nothing will reap. See [`crate::exec`] — and note what it is *not*: a
    /// cgroup bounds a process's lifetime, not what it can read, so this is not
    /// §11.4's guest boundary and must never be described as one.
    fn processes(&self) -> Option<&dyn crate::exec::ProcessHost> {
        None
    }

    /// Whether [`ExecBackend::write`] can do anything.
    ///
    /// Defaults to `false` — a backend that has not said it is writable is not
    /// writable, which is the same fail-closed default the gate keeps one layer
    /// up. A tool asks so that its refusal can name *which* of the two gates
    /// stopped it, because "permission denied" that does not say by whom is the
    /// refusal that costs an hour.
    fn is_writable(&self) -> bool {
        false
    }

    /// The backend's root as a path a caller may compare an argument against.
    ///
    /// `None` by default: a tar channel, or a firecode guest seen from the host,
    /// has no host path that means anything here, and inventing one would make
    /// [`crate::runtime::GateCall::path_is_inside`] answer confidently about a
    /// filesystem it is not looking at.
    fn root_path(&self) -> Option<String> {
        None
    }

    /// One directory, not recursive. Recursion belongs to the tools, which then
    /// works identically over a tar channel that has no `walkdir`.
    fn list(&self, path: &str) -> Result<Vec<DirEntry>, BackendError>;

    /// Does this path exist, and what is it? `None` for absent.
    fn stat(&self, path: &str) -> Option<DirEntry>;

    /// For `EXPLAIN` and for the head: which substrate the tools are talking to.
    fn describe(&self) -> String;
}

/// The host filesystem, confined to a root.
///
/// The confinement is not a sandbox and does not pretend to be one — M2's boundary
/// is the sandbox. It is the smallest thing that makes clause 4's "a read-only
/// tool never prompts" safe to ship before that boundary exists: a read tool that
/// can reach `~/.ssh` is a tool that should have prompted.
///
/// # Writable is a constructor, not a flag
///
/// [`HostBackend::new`] gives a **read-only** backend and [`HostBackend::writable`]
/// gives one that can write. That asymmetry is the point: every caller that
/// existed before write tools did keeps a backend that physically cannot write,
/// so a write tool registered into an old session refuses at the backend even if
/// something went wrong at the gate. Two independent mechanisms, because a safety
/// property with one mechanism behind it is a safety property that ships broken
/// the first time somebody refactors the mechanism.
#[derive(Debug, Clone)]
pub struct HostBackend {
    root: PathBuf,
    writable: bool,
    /// `Some` only via [`HostBackend::executable`]. Shared, because the job table
    /// and the scope tree are session state and a `HostBackend` is cloned freely.
    processes: Option<std::sync::Arc<crate::exec::HostProcesses>>,
}

impl HostBackend {
    /// A read-only backend. `write` refuses.
    pub fn new(root: impl AsRef<Path>) -> Result<Self, BackendError> {
        let root = root
            .as_ref()
            .canonicalize()
            .map_err(|e| BackendError::Io(e.to_string()))?;
        Ok(HostBackend {
            root,
            writable: false,
            processes: None,
        })
    }

    /// A backend that can change the operator's tree.
    ///
    /// Spelled as its own constructor so that `grep -rn 'HostBackend::writable'`
    /// finds every place in the tree where that became possible.
    pub fn writable(root: impl AsRef<Path>) -> Result<Self, BackendError> {
        Ok(HostBackend {
            writable: true,
            ..Self::new(root)?
        })
    }

    /// A backend that can start processes: writable, **and unsandboxed**.
    ///
    /// The third constructor, for the same reason there is a second one — so that
    /// `grep -rn 'HostBackend::executable'` finds every place in the tree where an
    /// exec path became reachable. It is not a flag on the other two and it is not
    /// reachable from them.
    ///
    /// # What this does and does not buy
    ///
    /// What it buys is [`crate::exec`]: every process lands in a cgroup owned by a
    /// scope, a scope that ends kills its cgroup and records what it killed, and
    /// `pkill`/`pgrep` become unnecessary rather than merely discouraged.
    ///
    /// What it does **not** buy is §11.4's boundary — *the guest sees a copy of
    /// one project and nothing else of the host*. A command started here runs with
    /// this user's rights over this user's whole filesystem. A cgroup bounds a
    /// lifetime, not a view. [`ExecBackend::describe`] says `unsandboxed` for
    /// exactly this reason: a disclosure that lists only the guarantees reads as a
    /// claim about the rest, which is `docs/closed-loop.md`'s banner defect.
    ///
    /// Fails, rather than degrading, when there is no cgroup v2 subtree to
    /// delegate: a process with no owner is the leak `TODO.md` T24 exists to stop.
    pub fn executable(root: impl AsRef<Path>) -> Result<Self, BackendError> {
        let base = Self::writable(root)?;
        let host = crate::exec::HostProcesses::new(base.root.clone())
            .map_err(|e| BackendError::Io(e.to_string()))?;
        Ok(HostBackend {
            processes: Some(std::sync::Arc::new(host)),
            ..base
        })
    }

    /// An executable backend over a chosen process host. The seam tests reach
    /// through, and the one a daemon uses when it wants to declare what it manages
    /// — [`crate::exec::HostProcesses::protect_listener`] — before any tool runs.
    pub fn executable_with(
        root: impl AsRef<Path>,
        host: std::sync::Arc<crate::exec::HostProcesses>,
    ) -> Result<Self, BackendError> {
        Ok(HostBackend {
            processes: Some(host),
            ..Self::writable(root)?
        })
    }

    /// The process host, concretely, for a caller that needs to end a scope or
    /// read the reap log — neither of which is a tool call.
    pub fn host_processes(&self) -> Option<&std::sync::Arc<crate::exec::HostProcesses>> {
        self.processes.as_ref()
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolve a tool-supplied path inside the root.
    ///
    /// Lexical normalisation first (so `a/../../etc` is refused without touching
    /// the disk), then, for a path that exists, a canonicalisation check — which is
    /// what catches a symlink pointing out of the tree.
    pub fn resolve(&self, path: &str) -> Result<PathBuf, BackendError> {
        let given = Path::new(path);
        let rel = match given.strip_prefix(&self.root) {
            Ok(r) => r.to_path_buf(),
            Err(_) if given.is_absolute() => {
                return Err(BackendError::Outside(path.to_string()));
            }
            Err(_) => given.to_path_buf(),
        };

        let mut out = self.root.clone();
        for c in rel.components() {
            match c {
                Component::Normal(p) => out.push(p),
                Component::CurDir => {}
                Component::ParentDir => {
                    if !out.pop() || !out.starts_with(&self.root) {
                        return Err(BackendError::Outside(path.to_string()));
                    }
                }
                Component::RootDir | Component::Prefix(_) => {
                    return Err(BackendError::Outside(path.to_string()));
                }
            }
        }
        if let Ok(real) = out.canonicalize()
            && !real.starts_with(&self.root)
        {
            return Err(BackendError::Outside(path.to_string()));
        }
        Ok(out)
    }

    /// The path a tool should show back to the model: relative to the root, `/`
    /// separated. Absolute host paths in a tool result are the same staleness
    /// clause 6 refuses in a description.
    pub fn display(&self, p: &Path) -> String {
        p.strip_prefix(&self.root)
            .unwrap_or(p)
            .to_string_lossy()
            .replace('\\', "/")
    }
}

impl ExecBackend for HostBackend {
    /// Runs only on a backend built by [`HostBackend::executable`], and even there
    /// it runs **through the job machinery** rather than beside it.
    ///
    /// There is deliberately no second exec path. A `Command` that bypassed
    /// [`crate::exec`] would be a process with no scope to reap it, no capture to
    /// read afterwards and no denominator on its output — three properties this
    /// substrate exists to give — so this spawns a job in the turn scope and waits
    /// for it. The synchronous shape is kept because `docs/workstreams.md` names
    /// the seam that way and a firecode backend will implement it directly.
    fn run(&self, cmd: &Command) -> Result<Output, BackendError> {
        let Some(host) = &self.processes else {
            // Unchanged for every backend that did not ask to be executable, and
            // unchanged in what it says: the boundary is still not here.
            return Err(BackendError::Unsupported(
                "this session's backend cannot start processes (HostBackend::new / \
                 ::writable); a session that may run commands opens it with \
                 HostBackend::executable, and §11.4's guest boundary is still not \
                 what that gives",
            ));
        };
        use crate::exec::{ProcessHost, ScopeKind, SpawnRequest, Waited};
        // argv, not a shell string: `Command` is argv by contract and turning it
        // back into one here would be inventing a quoting policy.
        let joined = cmd
            .argv
            .iter()
            .map(|a| shell_quote(a))
            .collect::<Vec<_>>()
            .join(" ");
        let id = host
            .spawn(&SpawnRequest {
                command: joined,
                cwd: cmd.cwd.clone(),
                scope: ScopeKind::Turn,
                scope_name: None,
                background: false,
                env: cmd.env.clone(),
            })
            .map_err(|e| BackendError::Io(e.to_string()))?;
        let waited = host
            .wait_job(&id, std::time::Duration::from_secs(600))
            .map_err(|e| BackendError::Io(e.to_string()))?;
        let out = host
            .output(&id, 0, usize::MAX)
            .map_err(|e| BackendError::Io(e.to_string()))?;
        let exit = match waited {
            Waited::Happened {
                state: Some(crate::exec::JobState::Exited { code }),
                ..
            } => code,
            // A deadline is NOT an exit code and is not dressed as one (F5).
            Waited::Deadline { .. } => {
                return Err(BackendError::Io(format!(
                    "`{id}` did not finish inside 600s and is still running; \
                     it is in the turn scope and will be reaped with it"
                )));
            }
            _ => -1,
        };
        Ok(Output {
            stdout: out.bytes,
            stderr: Vec::new(),
            exit,
        })
    }

    fn read(&self, path: &str) -> Result<Vec<u8>, BackendError> {
        let p = self.resolve(path)?;
        if p.is_dir() {
            return Err(BackendError::IsADirectory(path.to_string()));
        }
        std::fs::read(&p).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => BackendError::NotFound(path.to_string()),
            _ => BackendError::Io(e.to_string()),
        })
    }

    /// Atomic by construction: a temp file beside the target, then `rename`.
    ///
    /// The order matters and each step is a failure somebody has met:
    ///
    /// 1. the temp file is created in the **same directory**, because `rename` is
    ///    only atomic within a filesystem and `/tmp` is routinely a different one;
    /// 2. the original's mode is copied onto the temp file before the rename, or
    ///    an edit to an executable script silently disarms it;
    /// 3. the data is `sync_all`'d before the rename, so a crash between the two
    ///    leaves either the old file or the new one and never a rename that
    ///    published an empty inode;
    /// 4. the *directory* is synced after, so the rename itself survives;
    /// 5. a symlink is followed to its target rather than replaced, because
    ///    replacing it is a different edit from the one that was asked for. The
    ///    target has already been checked to be inside the root by
    ///    [`HostBackend::resolve`].
    ///
    /// The temp file is removed on every failure path, so a refused write leaves
    /// no litter to be mistaken for a partial one.
    fn write(&self, path: &str, bytes: &[u8]) -> Result<(), BackendError> {
        use std::io::Write;

        if !self.writable {
            return Err(BackendError::Unsupported(
                "this session's backend was opened read-only (HostBackend::new); \
                 a session that may change files opens it with HostBackend::writable",
            ));
        }
        let resolved = self.resolve(path)?;
        // (5) follow a symlink to its target; `resolve` already refused one that
        // points out of the root.
        let target = resolved.canonicalize().unwrap_or(resolved);
        if target.is_dir() {
            return Err(BackendError::IsADirectory(path.to_string()));
        }
        let Some(dir) = target.parent() else {
            return Err(BackendError::Io(format!("{path} has no parent directory")));
        };
        std::fs::create_dir_all(dir).map_err(|e| BackendError::Io(e.to_string()))?;

        let name = target
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "out".into());
        // (1) same directory, and a name nothing else will pick.
        let tmp = dir.join(format!(
            ".{name}.letibot-{}-{}.tmp",
            std::process::id(),
            TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));

        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let write_it = || -> std::io::Result<()> {
            let mut f = opts.open(&tmp)?;
            f.write_all(bytes)?;
            // (3) the data reaches the disk before the rename publishes it.
            f.sync_all()?;
            drop(f);
            // (2) inherit the original's mode. A **new** file gets 0644 rather
            // than keeping the 0600 it was created with: a source file the
            // operator's editor, build or web server cannot read is a surprise
            // that shows up somewhere else entirely, and the confidentiality it
            // would buy inside a project tree is nil. Fixed rather than
            // umask-derived because `std` does not expose the umask, and a
            // predictable mode beats one that depends on how the daemon was
            // started.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(&target)
                    .map(|m| m.permissions().mode())
                    .unwrap_or(0o644);
                std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))?;
            }
            std::fs::rename(&tmp, &target)?;
            // (4) best effort: a filesystem that will not let us open a directory
            // is not a reason to report a write that succeeded as failed.
            if let Ok(d) = std::fs::File::open(dir) {
                let _ = d.sync_all();
            }
            Ok(())
        };
        match write_it() {
            Ok(()) => Ok(()),
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                Err(BackendError::Io(e.to_string()))
            }
        }
    }

    fn is_writable(&self) -> bool {
        self.writable
    }

    fn root_path(&self) -> Option<String> {
        Some(self.root.to_string_lossy().to_string())
    }

    fn list(&self, path: &str) -> Result<Vec<DirEntry>, BackendError> {
        let p = self.resolve(path)?;
        let rd = std::fs::read_dir(&p).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => BackendError::NotFound(path.to_string()),
            std::io::ErrorKind::NotADirectory => BackendError::NotADirectory(path.to_string()),
            _ => BackendError::Io(e.to_string()),
        })?;
        let mut out = Vec::new();
        for e in rd.flatten() {
            let meta = e.metadata().ok();
            out.push(DirEntry {
                path: self.display(&e.path()),
                name: e.file_name().to_string_lossy().to_string(),
                is_dir: meta.as_ref().map(|m| m.is_dir()).unwrap_or(false),
                bytes: meta.as_ref().map(|m| m.len()).unwrap_or(0),
            });
        }
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }

    fn stat(&self, path: &str) -> Option<DirEntry> {
        let p = self.resolve(path).ok()?;
        let meta = std::fs::metadata(&p).ok()?;
        Some(DirEntry {
            path: self.display(&p),
            name: p
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default(),
            is_dir: meta.is_dir(),
            bytes: meta.len(),
        })
    }

    /// **Found while adding exec:** this used to say `read-only` for every
    /// `HostBackend`, including one built by [`HostBackend::writable`]. A hard-coded
    /// banner asserting a property the session does not have is the exact defect
    /// `docs/tool-design-brief.md` §1 names — *"a banner asserted 'read-only tools'
    /// about a session that had write tools"* — and it was in the disclosure the
    /// daemon prints at startup. It is now read off the fields.
    fn describe(&self) -> String {
        let mode = match (self.writable, self.processes.is_some()) {
            (_, true) => "writable + UNSANDBOXED EXEC",
            (true, false) => "writable",
            (false, false) => "read-only",
        };
        format!("host filesystem, {mode}, rooted at {}", self.root.display())
    }

    fn processes(&self) -> Option<&dyn crate::exec::ProcessHost> {
        self.processes.as_ref().map(|p| p.as_ref() as &dyn crate::exec::ProcessHost)
    }
}

/// Single-quote one argv element for a shell, the only way that is total: close
/// the quote, escape the quote, reopen. Used solely by [`ExecBackend::run`], whose
/// contract is argv and whose substrate takes a command string.
fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-/=:,+@".contains(c))
    {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Recursive listing, in terms of `list` alone.
///
/// Written here rather than with `std::fs` so that it works unchanged over a
/// backend whose "filesystem" is a tar channel. `limit` bounds the walk: a tool
/// that walks an unbounded tree is a tool that hangs on a home directory.
pub fn walk(
    backend: &dyn ExecBackend,
    root: &str,
    limit: usize,
    skip: &dyn Fn(&DirEntry) -> bool,
) -> (Vec<DirEntry>, bool) {
    let mut out = Vec::new();
    let mut queue = std::collections::VecDeque::from([root.to_string()]);
    let mut truncated = false;
    while let Some(dir) = queue.pop_front() {
        let Ok(entries) = backend.list(&dir) else {
            continue;
        };
        for e in entries {
            if skip(&e) {
                continue;
            }
            if out.len() >= limit {
                // Bounded, and the caller is told it was bounded. A silent cut is
                // the failure §8.3 refuses one layer up.
                truncated = true;
                return (out, truncated);
            }
            if e.is_dir {
                queue.push_back(e.path.clone());
            }
            out.push(e);
        }
    }
    (out, truncated)
}

/// The default skip set: version-control and build directories, which are never
/// what a question is about and are most of the bytes.
pub fn default_skip(e: &DirEntry) -> bool {
    matches!(
        e.name.as_str(),
        ".git" | "target" | "node_modules" | ".venv" | "__pycache__" | ".cache"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempdir::TempDir, HostBackend) {
        let d = tempdir::TempDir::new();
        std::fs::create_dir_all(d.path().join("src")).unwrap();
        std::fs::write(d.path().join("src/lib.rs"), "fn main() {}\n").unwrap();
        std::fs::write(d.path().join("README.md"), "hello\n").unwrap();
        let b = HostBackend::new(d.path()).unwrap();
        (d, b)
    }

    #[test]
    fn reads_within_the_root_and_refuses_outside_it() {
        let (_d, b) = fixture();
        assert_eq!(b.read("src/lib.rs").unwrap(), b"fn main() {}\n");
        for escape in ["../../etc/passwd", "/etc/passwd", "src/../../../etc/passwd"] {
            assert!(
                matches!(b.read(escape), Err(BackendError::Outside(_))),
                "{escape} must not resolve"
            );
        }
    }

    #[test]
    fn a_missing_path_is_its_own_error_because_clause_one_hangs_off_it() {
        let (_d, b) = fixture();
        assert!(matches!(
            b.read("src/nope.rs"),
            Err(BackendError::NotFound(_))
        ));
    }

    #[test]
    fn write_and_run_are_named_and_refused_rather_than_missing() {
        let (_d, b) = fixture();
        let e = b.write("a", b"x").unwrap_err();
        assert!(format!("{e}").contains("read-only"), "{e}");
        let e = b
            .run(&Command {
                argv: vec!["ls".into()],
                cwd: ".".into(),
                env: vec![],
            })
            .unwrap_err();
        assert!(format!("{e}").contains("11.4"), "{e}");
    }

    #[test]
    fn walk_is_bounded_and_says_so() {
        let (_d, b) = fixture();
        let (all, truncated) = walk(&b, ".", 100, &default_skip);
        assert!(all.iter().any(|e| e.path == "src/lib.rs"));
        assert!(!truncated);
        let (few, truncated) = walk(&b, ".", 1, &default_skip);
        assert_eq!(few.len(), 1);
        assert!(truncated);
    }
}

/// A tiny temp-directory helper for this crate's tests.
///
/// The workspace has no `tempfile` dependency and this is fifteen lines of it. It
/// is `pub` because the built-ins' tests need the same fixture.
#[cfg(any(test, feature = "testing"))]
pub mod tempdir {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static N: AtomicU64 = AtomicU64::new(0);

    pub struct TempDir(PathBuf);

    impl TempDir {
        #[allow(clippy::new_without_default)]
        pub fn new() -> Self {
            let n = N.fetch_add(1, Ordering::Relaxed);
            let p = std::env::temp_dir().join(format!(
                "letibot-tools-{}-{}-{n}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ));
            std::fs::create_dir_all(&p).expect("temp dir");
            TempDir(p)
        }

        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
