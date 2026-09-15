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
use std::sync::Arc;
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

    /// A tool's `cwd` argument — relative to where the session sits — in the form
    /// [`crate::exec::ProcessHost::spawn`] wants it: relative to the root. The
    /// identity by default; a backend whose root and workspace differ maps it.
    fn workdir(&self, cwd: &str) -> Result<String, BackendError> {
        Ok(cwd.to_string())
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

    /// A head asked to move the running command to the background, and this is the
    /// identity of whoever asked. `None` when no such request is pending. **Taking
    /// it clears it**, so the `bash` tool acts once.
    ///
    /// `None` by default: a backend with no exec path has nothing to move. The host
    /// backend carries the shared channel the daemon wires from the hub, so the
    /// `bash` tool's wait loop can honour the request without the worker — which is
    /// blocked inside that wait — having to deliver it.
    fn promote_requested(&self) -> Option<String> {
        None
    }

    /// The session is over: release what the backend holds and say where its
    /// work went, if anywhere a caller should know about. `None` by default — a
    /// host backend holds nothing and its writes were where they always were.
    /// A firecode backend brings its VM down and names the sibling directory
    /// the guest's tree landed in.
    fn close(&self) -> Option<String> {
        None
    }
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
    /// The operator's home directory, for expanding a leading `~` the way opencode's
    /// `os.homedir()` does. Read once at construction, so `resolve` does not reach
    /// into the environment on every call and so the value is a property of the
    /// backend rather than of the process that happens to be calling it.
    home: PathBuf,
    /// Where a RELATIVE path starts. The root for a confined backend; the session's
    /// workspace for one rooted at `/`.
    ///
    /// Measured 2026-09-14, twice, on a leticode one-shot: `read
    /// crates/flowy/src/context.rs` answered `no file`, because the path was joined
    /// to the root and the root was `/`. The model then went looking for the file
    /// with `glob` and `grep` from `/home` downwards, read a 27 GB model shard into
    /// memory on the way, and the daemon spent twelve minutes in the kernel. A
    /// relative path is relative to where the session sits, the way opencode's
    /// `read` joins it to the worktree — being able to reach the whole host is not
    /// the same as starting there.
    cwd: PathBuf,
    /// A head asked to move the running command to the background, and this is who.
    /// `None` when no request is pending. Wired by the daemon from the session's
    /// hub; the `bash` tool's wait loop reads it.
    promote: Arc<std::sync::Mutex<Option<String>>>,
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
        let home = std::env::var("HOME").map(PathBuf::from).unwrap_or_default();
        Ok(HostBackend {
            cwd: root.clone(),
            root,
            writable: false,
            home,
            promote: Arc::new(std::sync::Mutex::new(None)),
            processes: None,
        })
    }

    /// Where relative paths start. Must lie under the root; a `cwd` outside it is
    /// refused, because a relative path could then resolve to something the root
    /// was chosen to exclude.
    pub fn with_cwd(mut self, cwd: impl AsRef<Path>) -> Result<Self, BackendError> {
        let cwd = cwd
            .as_ref()
            .canonicalize()
            .map_err(|e| BackendError::Io(e.to_string()))?;
        if !cwd.starts_with(&self.root) {
            return Err(BackendError::Outside(cwd.display().to_string()));
        }
        self.cwd = cwd;
        Ok(self)
    }

    /// Where relative paths start — see [`HostBackend::with_cwd`].
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// Wire the shared "move the running command to the background" channel from the
    /// session's hub. The daemon calls this after construction; a backend a test
    /// built without it simply never sees a request.
    pub fn with_promote_channel(mut self, channel: Arc<std::sync::Mutex<Option<String>>>) -> Self {
        self.promote = channel;
        self
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
    /// lifetime, not a view. [`ExecBackend::describe`] says `NOT CONFINED` for
    /// exactly this reason: a disclosure that lists only the guarantees reads as a
    /// claim about the rest, which is `docs/closed-loop.md`'s banner defect.
    ///
    /// **Since layer 1, this is a choice and not the only option.**
    /// [`HostBackend::confined`] adds the namespaces, and the difference between
    /// the two backends is a [`crate::exec::Confinement`] that names itself:
    /// [`crate::exec::Unconfined`] here (*not asked for*) against a measured
    /// [`crate::exec::Bwrap`] there. What neither ever is:
    /// [`crate::exec::NoConfinement`] silently behaving like this one.
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

    /// A backend that can start processes **inside a project-scoped boundary**.
    ///
    /// The fourth constructor, and the first one that is a *narrowing* rather than
    /// a widening — but it is spelled the same way for the same reason: `grep -rn
    /// 'HostBackend::confined'` finds every session that has a boundary, and the
    /// absence of the string is then evidence about a session rather than a
    /// question about it.
    ///
    /// # What this buys over [`HostBackend::executable`]
    ///
    /// `docs/boundary-and-adjudication.md` §4's layer 1: a mount namespace rooted
    /// at this project (so the operator's `~/.ssh` is **absent**, not denied), a
    /// PID namespace (so the daemon, the model server and a sibling job are
    /// invisible and unsignalable), a network namespace with no route out (so
    /// egress is a decision), and a user namespace, which is what makes the other
    /// three possible unprivileged.
    ///
    /// # What it does not buy
    ///
    /// §3's invariant has two halves and this is the first. Secret bytes stay
    /// outside the view, so they cannot be *read* — but a tool result that already
    /// contains something still travels into the transcript, and the choke point
    /// for that is open (§5). And an authorised credential is usable only through
    /// [`crate::exec::Grant::AgentSocket`]; there is no grant that binds a key.
    ///
    /// # It fails rather than degrading
    ///
    /// Two independent ways to fail and the error says which: no delegated cgroup
    /// v2 subtree (a process with no owner), or no usable boundary (a command that
    /// would read the operator's disk while a disclosure said otherwise). Neither
    /// falls back to [`HostBackend::executable`].
    pub fn confined(root: impl AsRef<Path>) -> Result<Self, BackendError> {
        Self::confined_granting(root, Vec::new())
    }

    /// A confined backend whose view also holds `grants`.
    ///
    /// **A grant is a decision with a consequence**, and the consequence is the reason
    /// this takes them rather than hardcoding any: a `ReadOnly` path is readable inside
    /// the view and therefore readable *into the transcript*, since §3's second half is
    /// not enforced by the confinement module. `Boundary::describe` prints that next to
    /// each grant, and every `Grant` carries a `why`, because *"a grant nobody can
    /// explain is a grant nobody can revoke"*.
    ///
    /// Nothing here decides what to grant. The caller does, from a flag the operator
    /// typed — which is what keeps a capability from arriving as a side effect.
    pub fn confined_granting(
        root: impl AsRef<Path>,
        grants: Vec<crate::exec::confine::Grant>,
    ) -> Result<Self, BackendError> {
        let base = Self::writable(root)?;
        let mut view = crate::exec::confine::ViewSpec::project_only(base.root.clone());
        for g in grants {
            view = view.granting(g);
        }
        let confine = crate::exec::Bwrap::probe(view, crate::exec::confine::Egress::Denied)
            .map_err(|e| BackendError::Io(e.to_string()))?;
        let host = crate::exec::HostProcesses::confined(base.root.clone(), Box::new(confine))
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

    /// Override the home directory used to expand a leading `~`.
    ///
    /// The harness sets this from the session's [`crate::intent::Surroundings`]
    /// rather than leaving the backend to read `$HOME` itself, so a test can pin the
    /// home and a session's `~` is a property of the session, not of the process.
    pub fn with_home(mut self, home: impl Into<PathBuf>) -> Self {
        self.home = home.into();
        self
    }

    /// Resolve a tool-supplied path inside the root, **following symlinks as it
    /// goes** rather than checking one at the end.
    ///
    /// # The escape this used to have
    ///
    /// It ended in `if let Ok(real) = out.canonicalize() && !real.starts_with(root)`.
    /// `canonicalize` fails on a path that does not exist, and `if let Ok` turns
    /// that failure into *no check at all* — so for a **new** file the containment
    /// test was skipped, invisibly. With `root/escape -> /elsewhere`, resolving
    /// `escape/new.txt` returned a path inside `root` in name only, and `write`
    /// then created `/elsewhere/new.txt`. `write`'s own comment asserted the
    /// opposite: *"`resolve` already refused one that points out of the root"*. A
    /// guard whose failure mode is silence is the shape `docs/closed-loop.md` calls
    /// an open-loop stepper.
    ///
    /// A dangling link was the same hole with a different spelling:
    /// `root/escape -> /elsewhere/gone` canonicalises to nothing, so nothing was
    /// checked, and a write created the target.
    ///
    /// # Why this is not made moot by [`crate::exec::confine`]
    ///
    /// The mount namespace confines the **exec** path. `read`, `write`, `edit`,
    /// `glob` and `grep` are in-process calls in the daemon's own mount namespace
    /// and go through this function instead. Layer 1 covers one of the two ways in;
    /// this is the other one, and it is why the fix is here rather than deferred to
    /// a boundary.
    ///
    /// # What it does now
    ///
    /// Lexical normalisation first (so `a/../../etc` is refused without touching
    /// the disk), then [`resolve_under`] walks the components from the root
    /// downwards, resolving each link against its own parent and requiring
    /// containment at every step — which is answerable about a path whose tail does
    /// not exist yet, and is the only form that is.
    pub fn resolve(&self, path: &str) -> Result<PathBuf, BackendError> {
        // A leading `~` is the operator's home, read the way a shell reads it.
        // opencode expands it, and a path the model writes as `~/x` must not resolve
        // to a literal directory named `~` under the root.
        let expanded = expand_tilde(path, &self.home);
        let given = Path::new(&expanded);
        // A relative path starts where the session sits, not at the root: the two
        // differ only for a backend rooted at `/`, and there the difference is the
        // whole workspace.
        let given = if given.is_absolute() {
            given.to_path_buf()
        } else {
            self.cwd.join(given)
        };
        let rel = match given.strip_prefix(&self.root) {
            Ok(r) => r.to_path_buf(),
            Err(_) if given.is_absolute() => {
                return Err(BackendError::Outside(path.to_string()));
            }
            Err(_) => given.to_path_buf(),
        };

        resolve_under(&self.root, &rel).ok_or_else(|| BackendError::Outside(path.to_string()))
    }

    /// The path a tool should show back to the model: relative to the root, `/`
    /// separated. Absolute host paths in a tool result are the same staleness
    /// clause 6 refuses in a description.
    pub fn display(&self, p: &Path) -> String {
        // Under the workspace, relative to it; elsewhere under a `/` root, the
        // absolute path — `home/dead/x` with the slash stripped is a path that
        // resolves to nothing when the model hands it back.
        let shown = match p.strip_prefix(&self.cwd) {
            Ok(r) => r,
            Err(_) if self.root == Path::new("/") => p,
            Err(_) => p.strip_prefix(&self.root).unwrap_or(p),
        };
        shown.to_string_lossy().replace('\\', "/")
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
        let cwd = self.workdir(&cmd.cwd)?;
        let id = host
            .spawn(&SpawnRequest {
                command: joined,
                cwd,
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
        // (5) `resolve` has already followed every symlink on the way down and
        // refused any that left the root — **including on a path whose tail does
        // not exist yet**, which is the case this comment used to be wrong about.
        // The `canonicalize` is belt and braces for a component that changed
        // between the resolve and now, and it falls back to the resolved path
        // because a file that is about to be created has nothing to canonicalise.
        let resolved = self.resolve(path)?;
        let target = match resolved.canonicalize() {
            Ok(c) if c.starts_with(&self.root) => c,
            Ok(_) => return Err(BackendError::Outside(path.to_string())),
            Err(_) => resolved,
        };
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
    /// ... and the *same fix, a second time*, in the half this constructor added.
    ///
    /// `(_, true) => "writable + UNSANDBOXED EXEC"` was correct for every backend
    /// that existed when it was written and became a hard-coded claim the moment
    /// [`HostBackend::confined`] existed. So the exec half is read from the process
    /// host too, through [`crate::exec::Confinement::describe`], which reads it from
    /// a boundary that was measured from inside itself. There is no constant in
    /// this function that asserts a confinement property.
    fn describe(&self) -> String {
        let mode = match (self.writable, self.processes.as_ref()) {
            (_, Some(p)) => {
                let view = match crate::exec::ProcessHost::confinement(p.as_ref()) {
                    Some(c) => c.describe(),
                    None => "no confinement seam on this process host, so what a \
                             command can see is unknown — and an unknown boundary is \
                             not a boundary"
                        .to_string(),
                };
                format!("writable + EXEC ({view})")
            }
            (true, None) => "writable".to_string(),
            (false, None) => "read-only".to_string(),
        };
        if self.cwd == self.root {
            format!("host filesystem, {mode}, rooted at {}", self.root.display())
        } else {
            format!(
                "host filesystem, {mode}, rooted at {}; relative paths start at {}",
                self.root.display(),
                self.cwd.display()
            )
        }
    }

    fn processes(&self) -> Option<&dyn crate::exec::ProcessHost> {
        self.processes
            .as_ref()
            .map(|p| p.as_ref() as &dyn crate::exec::ProcessHost)
    }

    /// The request's `cwd` is relative to where the session sits, the same as
    /// every other path a tool hands this backend; the process host wants it
    /// relative to the root. They differ only for a backend rooted at `/`, and
    /// there `.` used to mean the root of the filesystem.
    fn workdir(&self, cwd: &str) -> Result<String, BackendError> {
        let dir = self.resolve(cwd)?;
        Ok(dir
            .strip_prefix(&self.root)
            .unwrap_or(&dir)
            .to_string_lossy()
            .into_owned())
    }

    fn promote_requested(&self) -> Option<String> {
        // Take, not read: whoever acts on the request (the `bash` wait loop) clears
        // it, so a second pass does not promote the same command twice.
        self.promote
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }
}

/// The most symlinks one path may traverse. Linux's own limit is 40 (`ELOOP`
/// above it); matching it means a path this refuses is a path the kernel would
/// have refused too, so no legitimate tree becomes unusable.
const MAX_SYMLINK_HOPS: usize = 40;

/// A `realpath` for a path whose **tail may not exist yet**.
///
/// This is the containment primitive, and its shape is forced by the requirement:
/// `std::fs::canonicalize` cannot answer for a file that is about to be created,
/// and a check that is skipped when it cannot answer is not a check. So the walk
/// goes *down* from the root, one component at a time:
///
/// - a component that does not exist ends the resolving — nothing under a
///   non-existent directory can exist either, so the rest is appended literally
///   and the containment already established stands;
/// - a component that is a **symlink** is replaced by its target, pushed back onto
///   the front of the queue so the target's own components are resolved in turn.
///   An absolute target restarts from the root and must land inside it;
/// - `..` pops, and popping past the root is a refusal rather than a clamp: a
///   clamp silently answers a different question from the one asked.
///
/// `None` means *outside the root, or unresolvable* — the caller turns it into
/// [`BackendError::Outside`], which already carries the path. Returning an
/// `Option` rather than a `bool` plus an out-parameter is deliberate: there is no
/// way to spell "refused" and still have a path to use.
/// A leading `~` is the operator's home directory, read the way a shell reads it.
/// `~` alone is the home itself; `~/x` is `x` under it. A path that is not
/// `~`-prefixed, or a home that is empty, is returned unchanged — a literal `~`
/// directory is the honest failure rather than a guessed home.
fn expand_tilde(path: &str, home: &Path) -> String {
    if home.as_os_str().is_empty() {
        return path.to_string();
    }
    if path == "~" {
        return home.to_string_lossy().to_string();
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return format!("{}/{rest}", home.display());
    }
    path.to_string()
}

fn resolve_under(root: &Path, rel: &Path) -> Option<PathBuf> {
    use std::collections::VecDeque;
    use std::ffi::OsString;

    // The work queue, in order. A symlink target is prepended, which is what makes
    // a chain of links terminate in one loop rather than in recursion.
    let mut queue: VecDeque<OsString> = VecDeque::new();
    for c in rel.components() {
        match c {
            Component::Normal(p) => queue.push_back(p.to_os_string()),
            Component::CurDir => {}
            Component::ParentDir => queue.push_back(OsString::from("..")),
            // An absolute or prefixed path inside what should be a relative
            // remainder is not a path this root can contain.
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }

    let mut out = root.to_path_buf();
    let mut hops = 0usize;
    while let Some(name) = queue.pop_front() {
        if name == ".." {
            if !out.pop() || !out.starts_with(root) {
                return None;
            }
            continue;
        }
        if name == "." {
            continue;
        }
        let cand = out.join(&name);
        // `symlink_metadata` and not `metadata`: the question is whether THIS
        // component is a link, not whether its target exists. A dangling link
        // answers `Ok` here and `Err` to `metadata`, and it is the dangling one
        // that used to get through.
        let Ok(meta) = std::fs::symlink_metadata(&cand) else {
            // Does not exist. Everything left is new; append it literally.
            out = cand;
            for rest in queue {
                if rest == ".." {
                    // A `..` after a non-existent component still has to be
                    // honoured, because the caller may create the parent first.
                    if !out.pop() || !out.starts_with(root) {
                        return None;
                    }
                } else if rest != "." {
                    out.push(rest);
                }
            }
            return out.starts_with(root).then_some(out);
        };
        if !meta.file_type().is_symlink() {
            out = cand;
            continue;
        }
        hops += 1;
        if hops > MAX_SYMLINK_HOPS {
            // A cycle, or a chain long enough that the kernel would refuse it too.
            // A refusal, never a hang and never a partial resolution.
            return None;
        }
        let target = std::fs::read_link(&cand).ok()?;
        // Prepend the target's components so they are resolved in turn. An
        // absolute target restarts the walk at the root and must lie inside it —
        // this is the check the old code did once, at the end, and therefore not
        // at all for a path that did not exist.
        let mut front: VecDeque<OsString> = VecDeque::new();
        if target.is_absolute() {
            let inside = target.strip_prefix(root).ok()?;
            out = root.to_path_buf();
            for c in inside.components() {
                match c {
                    Component::Normal(p) => front.push_back(p.to_os_string()),
                    Component::CurDir => {}
                    Component::ParentDir => front.push_back(OsString::from("..")),
                    Component::RootDir | Component::Prefix(_) => return None,
                }
            }
        } else {
            for c in target.components() {
                match c {
                    Component::Normal(p) => front.push_back(p.to_os_string()),
                    Component::CurDir => {}
                    Component::ParentDir => front.push_back(OsString::from("..")),
                    Component::RootDir | Component::Prefix(_) => return None,
                }
            }
        }
        while let Some(c) = front.pop_back() {
            queue.push_front(c);
        }
    }
    out.starts_with(root).then_some(out)
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
    ) ||
    // On a backend rooted at `/` a walk can reach the kernel's own trees. `/proc`
    // holds files whose size lies and whose reads block; `/sys` and `/dev` are not
    // files; `/run` is sockets and pid files. None is a place a search for source
    // would find it. Matched on the absolute path, so a project directory that
    // happens to be called `dev` is still walked.
    matches!(e.path.as_str(), "/proc" | "/sys" | "/dev" | "/run")
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

    /// A backend rooted at `/` with the workspace as its cwd: a relative path is
    /// the workspace's, an absolute one is the host's, and what is shown back is
    /// relative to the workspace when under it and absolute otherwise. The
    /// measured failure was `read crates/x.rs` -> `/crates/x.rs` -> "no file".
    #[test]
    fn relative_paths_start_at_the_cwd_not_the_root() {
        let d = tempdir::TempDir::new();
        std::fs::create_dir_all(d.path().join("ws/src")).unwrap();
        std::fs::write(d.path().join("ws/src/lib.rs"), "fn main() {}\n").unwrap();
        std::fs::write(d.path().join("outside.txt"), "x\n").unwrap();
        let ws = d.path().join("ws").canonicalize().unwrap();
        let b = HostBackend::new("/").unwrap().with_cwd(&ws).unwrap();
        assert_eq!(b.cwd(), ws.as_path());
        assert_eq!(b.read("src/lib.rs").unwrap(), b"fn main() {}\n");
        assert_eq!(b.read("./src/lib.rs").unwrap(), b"fn main() {}\n");
        assert_eq!(
            b.read(&ws.join("src/lib.rs").to_string_lossy()).unwrap(),
            b"fn main() {}\n"
        );
        // Absolute paths outside the workspace are still the host's, this is a `/` root.
        assert_eq!(
            b.read(&d.path().join("outside.txt").to_string_lossy())
                .unwrap(),
            b"x\n"
        );
        // Shown back: relative under the workspace, absolute elsewhere.
        assert_eq!(b.display(&ws.join("src/lib.rs")), "src/lib.rs");
        let outside = d.path().canonicalize().unwrap().join("outside.txt");
        assert_eq!(b.display(&outside), outside.to_string_lossy());
        // A command's `.` is the workspace, root-relative for the process host.
        let wd = b.workdir(".").unwrap();
        assert_eq!(Path::new("/").join(&wd), ws);
        assert!(!wd.starts_with('/'), "root-relative, got {wd}");
        // The pseudo-filesystems are skipped by the default walk on such a root.
        for p in ["/proc", "/sys", "/dev", "/run"] {
            let e = DirEntry {
                path: p.into(),
                name: p[1..].into(),
                is_dir: true,
                bytes: 0,
            };
            assert!(default_skip(&e), "{p} must be skipped");
        }
        let e = DirEntry {
            path: "src/dev".into(),
            name: "dev".into(),
            is_dir: true,
            bytes: 0,
        };
        assert!(
            !default_skip(&e),
            "a project directory called dev is walked"
        );
        // A cwd outside the root is refused.
        let confined = HostBackend::new(&ws).unwrap();
        assert!(confined.with_cwd(d.path()).is_err());
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

    /// A leading `~` expands to the backend's home, the way opencode's
    /// `os.homedir()` does — not to a literal directory named `~` under the root.
    #[test]
    fn a_leading_tilde_expands_to_the_backends_home() {
        let d = tempdir::TempDir::new();
        std::fs::create_dir_all(d.path().join("sub")).unwrap();
        std::fs::write(d.path().join("sub/f.txt"), "tilta\n").unwrap();
        // Root at `/` so the home (under the temp dir) is reachable, and pin the
        // home to prove `~` means *it* and not a literal `~` directory.
        let b = HostBackend::new("/")
            .unwrap()
            .with_home(d.path().to_path_buf());

        let p = b.resolve("~/sub/f.txt").unwrap();
        assert_eq!(p, d.path().join("sub/f.txt"));
        assert_eq!(b.read("~/sub/f.txt").unwrap(), b"tilta\n");
        // `~` alone is the home itself.
        assert_eq!(b.resolve("~").unwrap(), d.path().to_path_buf());
    }

    /// The promote channel the daemon wires from the hub is read **and cleared** by
    /// the `bash` wait loop, so a head's Ctrl+B is honoured once.
    #[test]
    fn a_promote_request_is_taken_not_merely_read() {
        let (_d, b) = fixture();
        let channel = Arc::new(std::sync::Mutex::new(None));
        let b = b.with_promote_channel(channel.clone());
        assert_eq!(b.promote_requested(), None);
        *channel.lock().unwrap() = Some("dead".to_string());
        assert_eq!(b.promote_requested().as_deref(), Some("dead"));
        // Taken: the second read sees nothing, so the command is not promoted twice.
        assert_eq!(b.promote_requested(), None);
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

    /// **A first-party containment escape, found by reading, and it is the one the
    /// namespaces do NOT make moot.**
    ///
    /// `resolve` ended in `if let Ok(real) = out.canonicalize()`. `canonicalize`
    /// fails on a path that does not exist, so for a **new** file the containment
    /// check was not merely weak — it was *skipped*, and the `let Ok(..)` made the
    /// skip invisible. A symlinked-out parent plus a filename that is not there yet
    /// wrote outside the root, and `write`'s own comment asserted the opposite:
    /// *"`resolve` already refused one that points out of the root"*.
    ///
    /// Why this matters after layer 1 exists: `read`, `write`, `edit`, `glob` and
    /// `grep` do **not** go through a namespace. They are in-process calls on the
    /// daemon's own mount namespace, so the mount view confines the exec path and
    /// nothing else. A boundary that covers one of two paths in is not a boundary,
    /// and this is the second path.
    #[test]
    fn a_new_file_under_a_symlinked_out_parent_is_refused() {
        let d = tempdir::TempDir::new();
        let outside = d.path().join("outside");
        let root = d.path().join("root");
        std::fs::create_dir_all(&outside).expect("outside");
        std::fs::create_dir_all(&root).expect("root");
        // The escape: a link inside the root pointing at a directory outside it.
        std::os::unix::fs::symlink(&outside, root.join("escape")).expect("symlink");
        let b = HostBackend::writable(&root).expect("writable");

        // The path that DOES exist was always refused, and still is.
        assert!(matches!(b.resolve("escape"), Err(BackendError::Outside(_))));
        // The path that does not exist yet is the hole.
        let e = b.resolve("escape/new.txt");
        assert!(
            matches!(e, Err(BackendError::Outside(_))),
            "a new file under a symlinked-out parent must be refused, got {e:?}"
        );
        // And through `write`, which is where the damage would land. Guard the fact,
        // not the proxy: the assertion is that nothing appeared on the operator's
        // disk outside the root, not merely that the call returned an error.
        let _ = b.write("escape/new.txt", b"escaped");
        assert!(
            !outside.join("new.txt").exists(),
            "a write escaped the root to {}",
            outside.join("new.txt").display()
        );
        // A deeper tail, so the fix cannot be "check one level".
        let _ = b.write("escape/a/b/c.txt", b"escaped");
        assert!(!outside.join("a").exists(), "a deep write escaped the root");
    }

    #[test]
    fn a_symlink_that_stays_inside_the_root_still_works() {
        // The other direction, because a containment fix that refuses everything
        // looks exactly like one that works. A project with an internal symlink is
        // ordinary, and `resolve` must follow it.
        let d = tempdir::TempDir::new();
        let root = d.path().join("root");
        std::fs::create_dir_all(root.join("real")).expect("real");
        std::os::unix::fs::symlink(root.join("real"), root.join("link")).expect("symlink");
        let b = HostBackend::writable(&root).expect("writable");
        let p = b
            .resolve("link/new.txt")
            .expect("an internal link resolves");
        assert!(p.starts_with(&root), "{}", p.display());
        b.write("link/new.txt", b"inside")
            .expect("write through an internal link");
        assert_eq!(
            std::fs::read_to_string(root.join("real/new.txt")).unwrap_or_default(),
            "inside"
        );
        // A relative link, and a link whose target is itself a link.
        std::os::unix::fs::symlink("real", root.join("rel")).expect("relative symlink");
        std::os::unix::fs::symlink("link", root.join("link2")).expect("link to a link");
        assert!(b.resolve("rel/x.txt").expect("relative").starts_with(&root));
        assert!(
            b.resolve("link2/x.txt")
                .expect("chained")
                .starts_with(&root)
        );
    }

    #[test]
    fn a_symlink_loop_is_a_refusal_and_not_a_hang() {
        // The cost of resolving component by component is that a cycle is now this
        // function's problem. It is bounded, and the bound is a refusal.
        let d = tempdir::TempDir::new();
        let root = d.path().join("root");
        std::fs::create_dir_all(&root).expect("root");
        std::os::unix::fs::symlink("b", root.join("a")).expect("a->b");
        std::os::unix::fs::symlink("a", root.join("b")).expect("b->a");
        let b = HostBackend::writable(&root).expect("writable");
        let e = b.resolve("a/x.txt");
        assert!(matches!(e, Err(BackendError::Outside(_))), "{e:?}");
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
