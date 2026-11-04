//! The execution-backend interface: the one seam every built-in reaches the world
//! through, and through nothing else. [`HostBackend`] implements read, list, stat and
//! write; `run` only once a boundary exists, because a boundary is a substrate.

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
    /// When the file was last written, in whole seconds since the Unix epoch, and `None` for a
    /// substrate that cannot answer. A reader that needs an age reads it here rather than
    /// stat'ing the file a second time, which over ssh is a round trip per note.
    pub modified: Option<u64>,
}

/// A file's mtime in whole seconds, for [`DirEntry::modified`]. A filesystem that answers no time,
/// or one stamped before 1970, has no answer rather than a zero.
fn modified_secs(meta: &std::fs::Metadata) -> Option<u64> {
    meta.modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
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

    /// The boundary this backend's commands run inside, when it has one: the view and the
    /// egress that were measured, not the ones that were asked for. By value, because a grant
    /// can replace it, so it lives behind a lock and cannot be borrowed out of it.
    fn boundary(&self) -> Option<crate::exec::Boundary> {
        None
    }

    /// Why a command here is not confined, when confinement was wanted and is not there. `None`
    /// means no question was asked, which is a different fact from asked and unanswerable.
    fn boundary_refusal(&self) -> Option<String> {
        None
    }

    /// Bind a path into this session's view, after somebody decided it may be. Refusing is the
    /// right answer for a backend with no view, and a refusal rather than a silent success so
    /// that a caller never reports a grant nobody made. Returns the view as it now stands.
    fn grant_into_view(
        &self,
        _path: &Path,
        _writable: bool,
        _why: &str,
    ) -> Result<String, BackendError> {
        Err(BackendError::Io(
            "this backend has no project-scoped view to widen".into(),
        ))
    }

    fn read(&self, path: &str) -> Result<Vec<u8>, BackendError>;

    /// The same answers as [`Self::read`], for a whole page of paths, in one call: the seam a
    /// backend whose `read` is a round trip overrides, and the default is the per-file loop.
    /// `Err` means the channel failed and says nothing about any path, so fall back to `read`.
    fn read_many(
        &self,
        paths: &[&str],
    ) -> Result<Vec<Result<Vec<u8>, BackendError>>, BackendError> {
        Ok(paths.iter().map(|p| self.read(p)).collect())
    }

    /// Replace a file's contents. Required to be atomic, as part of the interface: a
    /// half-written file is one whose contents nobody can predict. Missing parent directories
    /// are created.
    fn write(&self, path: &str, bytes: &[u8]) -> Result<(), BackendError>;

    /// The process host, when this backend can start a process whose lifetime is owned by
    /// something. `None` by default, and that default is the whole safety property: no backend
    /// gains an exec path by this method existing. A cgroup bounds a lifetime, not a view.
    fn processes(&self) -> Option<&dyn crate::exec::ProcessHost> {
        None
    }

    /// The same host as an owned handle, for a watcher that outlives the call that noticed the
    /// job. No default, so a backend that can start processes must say so here too, or its jobs
    /// run unwatched.
    fn processes_arc(&self) -> Option<std::sync::Arc<dyn crate::exec::ProcessHost>>;

    /// A tool's `cwd` argument - relative to where the session sits - in the form
    /// [`crate::exec::ProcessHost::spawn`] wants it: relative to the root. The
    /// identity by default; a backend whose root and workspace differ maps it.
    fn workdir(&self, cwd: &str) -> Result<String, BackendError> {
        Ok(cwd.to_string())
    }

    /// Whether [`ExecBackend::write`] can do anything. Defaults to `false`, the same fail-closed
    /// default the gate keeps one layer up, so that a tool's refusal can name which of the two
    /// gates stopped it.
    fn is_writable(&self) -> bool {
        false
    }

    /// The backend's root as a path a caller may compare an argument against. `None` by
    /// default, because a substrate with no host path would make
    /// [`crate::runtime::GateCall::path_is_inside`] answer about a filesystem it is not looking at.
    fn root_path(&self) -> Option<String> {
        None
    }

    /// Where a relative path starts, as this backend is actually configured. Not
    /// [`Self::root_path`]: an unconfined session's root is `/`, which answers whether a path is
    /// inside the boundary and not which directory the session is working in.
    fn workspace_path(&self) -> Option<String> {
        None
    }

    /// Which machine the tools run on, when it is not this one. `None` for every backend whose
    /// files are here. [`Self::workspace_path`] answers which directory; over ssh a directory
    /// alone names a path the operator cannot open and a machine the card never mentions.
    fn host_label(&self) -> Option<String> {
        None
    }

    /// The session's scratch directory, where tools put working artifacts too big for the
    /// transcript. Per-session, set by whoever opened the session. `None` by default, so a tool
    /// that wants one gets a refusal rather than a guess.
    fn scratch_dir(&self) -> Option<String> {
        None
    }

    /// The operator's home, as this backend expands a leading `~`. `None` by default, for the
    /// same reason [`Self::root_path`]'s is. The reader that pulls the script an interpreter was
    /// handed asks here rather than reading `$HOME` a second time.
    fn home_path(&self) -> Option<String> {
        None
    }

    /// One directory, not recursive: recursion belongs to the tools, which then works the same
    /// over a tar channel with no `walkdir`. A backend whose `list` is a round trip overrides
    /// [`Self::list_tree`] instead of paying one call per directory.
    fn list(&self, path: &str) -> Result<Vec<DirEntry>, BackendError>;

    /// The tree under `root`, in the order a walk of [`Self::list`] produces: the seam a backend
    /// whose `list` is expensive overrides. An override must return the same entries in the same
    /// order, or a tool's answer would depend on the substrate it ran on.
    fn list_tree(
        &self,
        root: &str,
        limit: usize,
        skip: &dyn Fn(&DirEntry) -> bool,
    ) -> Result<(Vec<DirEntry>, bool), BackendError> {
        let mut out = Vec::new();
        let mut queue = std::collections::VecDeque::from([root.to_string()]);
        let mut truncated = false;
        while let Some(dir) = queue.pop_front() {
            let Ok(entries) = self.list(&dir) else {
                continue;
            };
            for e in entries {
                if skip(&e) {
                    continue;
                }
                if out.len() >= limit {
                    // Bounded, and the caller is told it was bounded. A silent cut is the
                    // failure the truncation rule one layer up refuses.
                    truncated = true;
                    return Ok((out, truncated));
                }
                if e.is_dir {
                    queue.push_back(e.path.clone());
                }
                out.push(e);
            }
        }
        Ok((out, truncated))
    }

    /// Does this path exist, and what is it? `None` for absent.
    fn stat(&self, path: &str) -> Option<DirEntry>;

    /// For `EXPLAIN` and for the head: which substrate the tools are talking to.
    fn describe(&self) -> String;

    /// A head asked to move the running command to the background, and this is the identity of
    /// whoever asked. Taking it clears it, so the `bash` tool acts once. `None` by default: a
    /// backend with no exec path has nothing to move.
    fn promote_requested(&self) -> Option<String> {
        None
    }

    /// The session is over: release what the backend holds and say where its work went, if
    /// anywhere a caller should know about. `None` by default. A firecode backend brings its VM
    /// down and names the sibling directory the guest's tree landed in.
    fn close(&self) -> Option<String> {
        None
    }
}

/// The host filesystem, confined to a root. Not a sandbox and does not pretend to be one: the
/// smallest thing that makes "a read-only tool never prompts" safe before a boundary exists.
/// [`HostBackend::new`] cannot write and [`HostBackend::writable`] can.
#[derive(Debug, Clone)]
pub struct HostBackend {
    root: PathBuf,
    writable: bool,
    /// The operator's home directory, for expanding a leading `~`. Read once at construction,
    /// so the value is a property of the backend rather than of the calling process.
    home: PathBuf,
    /// Where a RELATIVE path starts: the root for a confined backend, the session's workspace
    /// for one rooted at `/`. Being able to reach the whole host is not the same as starting
    /// there, and a path joined to `/` is how a read came to look from `/home` downwards.
    cwd: PathBuf,
    /// A head asked to move the running command to the background, and this is who.
    /// `None` when no request is pending. Wired by the daemon from the session's
    /// hub; the `bash` tool's wait loop reads it.
    promote: Arc<std::sync::Mutex<Option<String>>>,
    /// `Some` only via [`HostBackend::executable`]. Shared, because the job table
    /// and the scope tree are session state and a `HostBackend` is cloned freely.
    processes: Option<std::sync::Arc<crate::exec::HostProcesses>>,
    /// The session's scratch directory, set by the daemon. `None` when the session
    /// has no scratch, and a tool that wants one gets a refusal rather than a guess.
    scratch: Option<PathBuf>,
    /// This session's `/tmp`, when its commands run inside a boundary, so that closing the
    /// session can remove what its commands left behind.
    session_tmp: Option<PathBuf>,
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
            scratch: None,
            session_tmp: None,
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

    /// Where relative paths start - see [`HostBackend::with_cwd`].
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

    /// Set the session's scratch directory. Both halves from one call: the tools read it through
    /// [`ExecBackend::scratch_dir`] and the process host writes a promoted run's output there, so
    /// a host pointed elsewhere would be the same defect one layer down.
    pub fn with_scratch_dir(mut self, dir: impl AsRef<Path>) -> Self {
        self.scratch = Some(dir.as_ref().to_path_buf());
        if let Some(host) = &self.processes {
            host.set_scratch_dir(dir.as_ref());
        }
        self
    }

    /// Give this session a `/tmp` that outlives one command, by binding `dir` there: a confined
    /// session's `/tmp` was a fresh tmpfs per command, so a file one call wrote was gone in the
    /// next. A new view and a new probe, because the argv is built once.
    pub fn with_session_tmp(mut self, dir: impl AsRef<Path>) -> Result<Self, BackendError> {
        let dir = dir.as_ref().to_path_buf();
        let Some(host) = self.processes.as_ref() else {
            return Ok(self);
        };
        // The boundary that was measured is the basis of the one that replaces it, the view
        // and the egress decision both, so a re-probe cannot quietly change a decision nobody
        // asked it to revisit.
        let Some(boundary) = host.boundary() else {
            return Ok(self);
        };
        let widened = boundary
            .view
            .with_tmp(crate::exec::TmpView::Session(dir.clone()));
        let confine = crate::exec::confine::probe(widened, boundary.egress)
            .map_err(|e| BackendError::Io(e.to_string()))?;
        host.replace_confinement(confine);
        self.session_tmp = Some(dir);
        Ok(self)
    }

    /// A backend that can change the operator's tree, spelled as its own constructor so that
    /// `grep -rn 'HostBackend::writable'` finds every place that became possible.
    pub fn writable(root: impl AsRef<Path>) -> Result<Self, BackendError> {
        Ok(HostBackend {
            writable: true,
            ..Self::new(root)?
        })
    }

    /// A backend that can start processes: writable, and unsandboxed. Every process lands in a
    /// cgroup a scope owns and a scope that ends kills it, but a command still runs with this
    /// user's rights over the whole filesystem, so [`ExecBackend::describe`] says NOT CONFINED.
    pub fn executable(root: impl AsRef<Path>) -> Result<Self, BackendError> {
        let base = Self::writable(root)?;
        let host = crate::exec::HostProcesses::new(base.root.clone())
            .map_err(|e| BackendError::Io(e.to_string()))?
            // The operator's own `!` line runs under this one, and `/bin/bash` on a stock Mac
            // is 3.2 while the rc chain `-ic` reads is written for bash 4. Only this
            // constructor: a confined view's `$HOME` is a tmpfs, so the rc there is nobody's.
            .with_console_shell(crate::console_bash::shell());
        Ok(HostBackend {
            processes: Some(std::sync::Arc::new(host)),
            ..base
        })
    }

    /// A backend that can start processes inside a project-scoped boundary: a mount namespace
    /// rooted at this project, a PID namespace, a network namespace with no route out, and a
    /// user namespace. Two independent ways to fail, and the error says which.
    pub fn confined(root: impl AsRef<Path>) -> Result<Self, BackendError> {
        Self::confined_granting(
            root,
            Vec::new(),
            crate::exec::seat::SeatView {
                grants: Vec::new(),
                egress: crate::exec::confine::Egress::Denied,
                env: Vec::new(),
                home: None,
                masked: Vec::new(),
                secret_env: Default::default(),
            },
        )
    }

    /// A confined backend whose view also holds `grants`. A grant is a decision with a
    /// consequence: a `ReadOnly` path is readable into the transcript too, so `Boundary::describe`
    /// prints that next to each grant. Nothing here decides what to grant; the caller does.
    pub fn confined_granting(
        root: impl AsRef<Path>,
        grants: Vec<crate::exec::confine::Grant>,
        seat: crate::exec::seat::SeatView,
    ) -> Result<Self, BackendError> {
        Self::confined_over(root.as_ref(), root.as_ref(), grants, seat)
    }

    /// Commands confined to `project`'s view while the file tools keep their reach from `files_root`.
    /// `seat` is the view every platform is threaded with: its grants join the caller's, its egress is
    /// the network rule, its env points tools at them.
    pub fn confined_over(
        files_root: impl AsRef<Path>,
        project: impl AsRef<Path>,
        grants: Vec<crate::exec::confine::Grant>,
        seat: crate::exec::seat::SeatView,
    ) -> Result<Self, BackendError> {
        let base = Self::writable(files_root)?;
        let project = Self::writable(project)?.root;
        let mut view = crate::exec::confine::ViewSpec::project_only(project.clone());
        let seat_grants = seat
            .grants
            .into_iter()
            .filter(|g| !grants.iter().any(|h| h.path() == g.path()));
        for g in grants
            .iter()
            .cloned()
            .chain(seat_grants.collect::<Vec<_>>())
        {
            view = view.granting(g);
        }
        // Only the tight view: a top-level session edits its own hooks and config, and a child
        // gets every part of the repository a write could land itself with, one by one.
        if seat.home.is_none() {
            for p in crate::exec::seat::git_dir_protected(&project) {
                view = view.protecting(p);
            }
        }
        // The seat's fence, at either level: a wide view holds all of it, and a tight one holds
        // whatever a grant lets in — the renderer masks the stores its own view reaches.
        view = view.masking(seat.masked.iter().cloned());
        if let Some(h) = &seat.home {
            // The operator's own home makes this the wide seat: the whole machine, at its own paths.
            view = view
                .with_home(crate::exec::confine::HomeView::Dir(h.clone()))
                .wide();
        }
        let confine = crate::exec::confine::probe(view, seat.egress)
            .map_err(|e| BackendError::Io(e.to_string()))?;
        let host = crate::exec::HostProcesses::confined(base.root.clone(), confine)
            .map_err(|e| BackendError::Io(e.to_string()))?;
        host.set_confined_env(seat.env);
        host.set_secret_env(seat.secret_env);
        Ok(HostBackend {
            processes: Some(std::sync::Arc::new(host)),
            ..base
        })
    }

    /// An executable backend over a chosen process host. The seam tests reach
    /// through, and the one a daemon uses when it wants to declare what it manages
    /// - [`crate::exec::HostProcesses::protect_listener`] - before any tool runs.
    pub fn executable_with(
        root: impl AsRef<Path>,
        host: std::sync::Arc<crate::exec::HostProcesses>,
    ) -> Result<Self, BackendError> {
        Ok(HostBackend {
            processes: Some(host),
            ..Self::writable(root)?
        })
    }

    /// Bind a path into this session's view, now, and re-probe so the boundary described is the
    /// boundary that runs: the argv is built once, at probe time. Commands already running keep
    /// the namespace they started in. Refuses on a backend with no view.
    fn grant_into_view_impl(
        &self,
        path: &Path,
        writable: bool,
        why: &str,
    ) -> Result<String, BackendError> {
        let Some(host) = self.processes.as_ref() else {
            return Err(BackendError::Io(
                "this backend starts no processes, so it has no view to grant into".into(),
            ));
        };
        let Some(view) = host.current_view() else {
            return Err(BackendError::Io(
                "this session has no project-scoped view, so there is nothing to widen: what a command here can reach is already what this user can reach"
                    .into(),
            ));
        };
        let g = if writable {
            crate::exec::confine::Grant::ReadWrite {
                path: path.to_path_buf(),
                why: why.to_string(),
            }
        } else {
            crate::exec::confine::Grant::ReadOnly {
                path: path.to_path_buf(),
                why: why.to_string(),
            }
        };
        let widened = view.granting(g);
        // A grant widens the files, never the network decision the session was opened with.
        let egress = host
            .boundary()
            .map(|b| b.egress)
            .unwrap_or(crate::exec::confine::Egress::Denied);
        let confine = crate::exec::confine::probe(widened, egress)
            .map_err(|e| BackendError::Io(e.to_string()))?;
        let described = crate::exec::Confinement::describe(&*confine);
        host.replace_confinement(confine);
        Ok(described)
    }

    /// The process host, concretely, for a caller that needs to end a scope or
    /// read the reap log - neither of which is a tool call.
    pub fn host_processes(&self) -> Option<&std::sync::Arc<crate::exec::HostProcesses>> {
        self.processes.as_ref()
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Override the home directory used to expand a leading `~`, so a test can pin the home and
    /// a session's `~` is a property of the session rather than of the process.
    pub fn with_home(mut self, home: impl Into<PathBuf>) -> Self {
        self.home = home.into();
        self
    }

    /// Resolve a tool-supplied path inside the root, following symlinks as it goes: a check
    /// `canonicalize` skips for a path that does not exist is not a check. Lexical normalisation
    /// first, then [`resolve_under`] walks down from the root requiring containment at each step.
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
            Err(_) if given.is_absolute() => match relative_to_root(&given, &self.root) {
                Some(r) => r,
                None => return Err(BackendError::Outside(path.to_string())),
            },
            Err(_) => given.to_path_buf(),
        };

        resolve_under(&self.root, &rel).ok_or_else(|| BackendError::Outside(path.to_string()))
    }

    /// The path a tool should show back to the model: relative to the root, `/`
    /// separated. Absolute host paths in a tool result are the same staleness
    /// clause 6 refuses in a description.
    pub fn display(&self, p: &Path) -> String {
        // Under the workspace, relative to it; elsewhere under a `/` root, the
        // absolute path - `home/dead/x` with the slash stripped is a path that
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
    /// The boundary this backend's commands run inside, read off the process host rather
    /// than off the constructor that was called: a grant replaces the confinement, so the
    /// measured one is the one that answers.
    fn boundary(&self) -> Option<crate::exec::Boundary> {
        self.processes.as_ref().and_then(|p| p.boundary())
    }

    fn grant_into_view(
        &self,
        path: &Path,
        writable: bool,
        why: &str,
    ) -> Result<String, BackendError> {
        self.grant_into_view_impl(path, writable, why)
    }

    /// Runs only on a backend built by [`HostBackend::executable`], and even there it runs
    /// through the job machinery: a `Command` that bypassed [`crate::exec`] would be a process
    /// with no scope to reap it and no capture to read afterwards.
    fn run(&self, cmd: &Command) -> Result<Output, BackendError> {
        let Some(host) = &self.processes else {
            // Unchanged for every backend that did not ask to be executable, and
            // unchanged in what it says: the boundary is still not here.
            return Err(BackendError::Unsupported(
                "this session's backend cannot start processes, so nothing was run. Opening \
                 a session on a backend that does is a decision about the session, and such \
                 a session also gets a confinement boundary the run happens inside",
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
                // The backend's own smoke run, named by nobody.
                slug: None,
                cwd,
                scope: ScopeKind::Turn,
                scope_name: None,
                background: false,
                env: cmd.env.clone(),
                // No terminal: this is the synchronous seam a built-in reaches the world
                // through, and its reader is a tool that wants bytes.
                tty: false,
                memory: None,
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
            // A deadline is NOT an exit code and is not dressed as one.
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

    /// Atomic by construction: a temp file in the same directory (rename is only atomic within
    /// one filesystem), the original's mode copied onto it, the data synced before the rename and
    /// the directory after, and a symlink followed rather than replaced. No litter on failure.
    fn write(&self, path: &str, bytes: &[u8]) -> Result<(), BackendError> {
        use std::io::Write;

        if !self.writable {
            return Err(BackendError::Unsupported(
                "this session's backend was opened read-only (HostBackend::new); \
                 a session that may change files opens it with HostBackend::writable",
            ));
        }
        // `resolve` has already followed every symlink on the way down, including on a path
        // whose tail does not exist. The `canonicalize` is belt and braces for a component that
        // changed since, falling back because a file about to be created has nothing to resolve.
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
            // Inherit the original's mode. A new file gets 0644 rather than the 0600 it was
            // created with: a source file the operator's editor or build cannot read is a
            // surprise elsewhere, and fixed beats umask-derived, which `std` does not expose.
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

    /// The session's working directory, which for an unconfined session is not the root: `/` is
    /// the root and the workspace is the cwd. Both are true and they are different facts.
    fn workspace_path(&self) -> Option<String> {
        Some(self.cwd.to_string_lossy().to_string())
    }

    fn scratch_dir(&self) -> Option<String> {
        self.scratch
            .as_ref()
            .map(|p| p.to_string_lossy().to_string())
    }

    fn home_path(&self) -> Option<String> {
        let h = self.home.to_string_lossy().to_string();
        (!h.is_empty()).then_some(h)
    }

    /// The session is over, so its `/tmp` goes with it. Best effort and deliberately silent: a
    /// session whose cleanup failed is not a session whose work did not land, and the answer
    /// this returns is the one the operator reads.
    fn close(&self) -> Option<String> {
        if let Some(dir) = &self.session_tmp {
            let _ = std::fs::remove_dir_all(dir);
        }
        None
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
                modified: meta.as_ref().and_then(modified_secs),
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
            modified: modified_secs(&meta),
        })
    }

    /// Read off the fields rather than asserted: a banner claiming a property the session does
    /// not have is the defect this function exists to avoid, so the exec half comes from
    /// [`crate::exec::Confinement::describe`], measured from inside the boundary itself.
    fn describe(&self) -> String {
        let mode = match (self.writable, self.processes.as_ref()) {
            (_, Some(p)) => {
                let view = match crate::exec::ProcessHost::confinement_summary(p.as_ref()) {
                    Some(c) => c,
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

    fn processes_arc(&self) -> Option<std::sync::Arc<dyn crate::exec::ProcessHost>> {
        self.processes
            .clone()
            .map(|p| p as std::sync::Arc<dyn crate::exec::ProcessHost>)
    }

    /// The request's `cwd` is relative to where the session sits; the process host wants it
    /// relative to the root. They differ only for a backend rooted at `/`.
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

/// A `realpath` for a path whose tail may not exist yet: a missing component ends the resolving,
/// a symlink is replaced by its target and resolved in turn, and `..` past the root is a refusal.
/// A leading `~` is the operator's home, read the way a shell reads it: `~/x` is `x` under it.
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

/// An absolute path, as a remainder under `root`, also when it is spelled through a symlinked
/// ancestor: the root is canonical and the path need not be, so when the plain strip fails the
/// longest existing ancestor is canonicalised and the strip is tried again. `..` ends the attempt.
fn relative_to_root(p: &Path, root: &Path) -> Option<PathBuf> {
    if let Ok(r) = p.strip_prefix(root) {
        return Some(r.to_path_buf());
    }
    let mut ancestor = p;
    let mut tail = Vec::new();
    loop {
        if let Ok(real) = ancestor.canonicalize() {
            let full = tail.iter().rev().fold(real, |acc, t| acc.join(t));
            return full.strip_prefix(root).ok().map(Path::to_path_buf);
        }
        tail.push(ancestor.file_name()?.to_os_string());
        ancestor = ancestor.parent()?;
    }
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
        // `symlink_metadata` and not `metadata`: the question is whether THIS component is a
        // link, not whether its target exists. A dangling link answers `Ok` here and `Err` to
        // `metadata`, and the dangling one is the one to refuse.
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
        // Prepend the target's components so they are resolved in turn. An absolute target
        // restarts the walk at the root and must lie inside it, which is the check the old code
        // did once, at the end, and therefore not at all for a path that did not exist.
        let mut front: VecDeque<OsString> = VecDeque::new();
        if target.is_absolute() {
            let inside = relative_to_root(&target, root)?;
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

/// Single-quote one argv element for a shell, the only way that is total: close the quote,
/// escape the quote, reopen. Used by [`ExecBackend::run`], whose contract is argv and whose
/// substrate takes a command string, and by [`crate::runtime::command_of_argv`].
pub(crate) fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-/=:,+@".contains(c))
    {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Recursive listing, over [`ExecBackend::list_tree`]. Written against the trait rather than
/// with `std::fs`, so it works unchanged over a backend whose filesystem is a tar channel, and
/// `limit` bounds the walk. An error, a root that does not exist most often, is an empty walk.
pub fn walk(
    backend: &dyn ExecBackend,
    root: &str,
    limit: usize,
    skip: &dyn Fn(&DirEntry) -> bool,
) -> (Vec<DirEntry>, bool) {
    backend.list_tree(root, limit, skip).unwrap_or_default()
}

/// The default skip set: version-control and build directories, which are never
/// what a question is about and are most of the bytes.
pub fn default_skip(e: &DirEntry) -> bool {
    matches!(
        e.name.as_str(),
        ".git" | "target" | "node_modules" | ".venv" | "__pycache__" | ".cache"
    ) ||
    // On a backend rooted at `/` a walk can reach the kernel's own trees: `/proc` holds files
    // whose size lies and whose reads block, and `/sys`, `/dev` and `/run` are no place a search
    // would find source. Matched on the absolute path, so a project called `dev` is still walked.
    matches!(e.path.as_str(), "/proc" | "/sys" | "/dev" | "/run")
}

#[cfg(test)]
mod tests {

    /// The two accessors answer two different questions, and filling a fact named `workspace`
    /// from `root_path()` says `workspace: /` for a seat rooted at the host. The configuration
    /// is right; the accessor answers where the boundary is, not where the session works.
    #[test]
    fn a_seat_rooted_at_the_host_still_reports_its_own_workspace() {
        let dir = std::env::temp_dir()
            .canonicalize()
            .expect("a canonical temp dir");
        // `writable` rather than `executable`: the same root and the same `with_cwd`,
        // and it needs no cgroup to build.
        let b = HostBackend::writable("/")
            .expect("a whole-host backend")
            .with_cwd(&dir)
            .expect("the session's workspace");
        assert_eq!(b.root_path().as_deref(), Some("/"), "the root is the host");
        assert_eq!(
            b.workspace_path().as_deref(),
            Some(dir.to_string_lossy().as_ref()),
            "and the workspace is where the session works"
        );
        // The property the second question exists for: a relative path starts here,
        // not at `/`.
        assert_eq!(b.cwd(), dir.as_path());
        // A confined seat has the two equal, which is why this hid. With
        // `root == workspace` the two accessors agree, so the wrong one looks right.
        let c = HostBackend::new(&dir).expect("a project-rooted backend");
        assert_eq!(c.root_path(), c.workspace_path());
    }
    use super::*;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn fixture() -> (tempdir::TempDir, HostBackend) {
        let d = tempdir::TempDir::new();
        std::fs::create_dir_all(d.path().join("src")).unwrap();
        std::fs::write(d.path().join("src/lib.rs"), "fn main() {}\n").unwrap();
        std::fs::write(d.path().join("README.md"), "hello\n").unwrap();
        let b = HostBackend::new(d.path()).unwrap();
        (d, b)
    }

    /// A backend rooted at `/` with the workspace as its cwd: a relative path is the
    /// workspace's, an absolute one is the host's, and what is shown back is relative to the
    /// workspace when under it and absolute otherwise.
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
                modified: None,
            };
            assert!(default_skip(&e), "{p} must be skipped");
        }
        let e = DirEntry {
            path: "src/dev".into(),
            name: "dev".into(),
            is_dir: true,
            bytes: 0,
            modified: None,
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
    /// `os.homedir()` does - not to a literal directory named `~` under the root.
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
        // Canonical, because the walk from `/` follows every link on the way - on macOS
        // the temp dir is under `/var`, which is `/private/var`.
        assert_eq!(p, d.path().canonicalize().unwrap().join("sub/f.txt"));
        assert_eq!(b.read("~/sub/f.txt").unwrap(), b"tilta\n");
        // `~` alone is the home itself.
        assert_eq!(b.resolve("~").unwrap(), d.path().canonicalize().unwrap());
    }

    /// The promote channel the daemon wires from the hub is read and cleared by
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
        assert!(format!("{e}").contains("cannot start processes"), "{e}");
        // The model reads this: a Rust type name or a document section is not a name it can act on.
        for internal in ["HostBackend", "§", "11.4"] {
            assert!(!format!("{e}").contains(internal), "{e}");
        }
    }

    /// A containment escape the namespaces do NOT make moot: `canonicalize` fails on a path that
    /// does not exist, so a check that skips it is no check. Read, write, edit, glob and grep are
    /// in-process calls on the daemon's own namespace, which confines the exec path only.
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

    /// A link spelled through a symlinked ancestor is still inside: the root is canonical and
    /// the link's target need not be, so the fixture builds the ancestor explicitly to mean the
    /// same on a box whose temp dir is canonical.
    #[test]
    fn an_absolute_link_through_a_symlinked_ancestor_stays_inside() {
        let d = tempdir::TempDir::new();
        let real = d.path().join("real-ws");
        std::fs::create_dir_all(real.join("src")).expect("src");
        std::os::unix::fs::symlink(&real, d.path().join("alias")).expect("alias");
        // The link names its target through the alias, not through the root's spelling.
        std::os::unix::fs::symlink(d.path().join("alias/src"), real.join("via")).expect("via");
        let b = HostBackend::writable(&real).expect("writable");
        let p = b
            .resolve("via/x.txt")
            .expect("a link through an alias of the root resolves");
        assert_eq!(p, real.canonicalize().unwrap().join("src/x.txt"));
        // And an absolute path the model spells through the alias is the same file.
        let q = b
            .resolve(&d.path().join("alias/src/y.txt").display().to_string())
            .expect("an absolute path through the alias resolves");
        assert_eq!(q, real.canonicalize().unwrap().join("src/y.txt"));
        // Still a refusal for a target that really is outside.
        std::os::unix::fs::symlink(d.path(), real.join("up")).expect("up");
        assert!(matches!(b.resolve("up/x"), Err(BackendError::Outside(_))));
    }

    #[test]
    fn a_symlink_that_stays_inside_the_root_still_works() {
        // The other direction, because a containment fix that refuses everything
        // looks exactly like one that works. A project with an internal symlink is
        // ordinary, and `resolve` must follow it.
        let d = tempdir::TempDir::new();
        let root = d.path().join("root");
        std::fs::create_dir_all(root.join("real")).expect("real");
        // Canonical, so the `starts_with` checks below compare like with like (macOS: `/var`
        // is `/private/var`). The links are still made under it, absolute and relative.
        let root = root.canonicalize().expect("root");
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

    /// The tree the two walk paths have to agree on: a directory, a name with a space in it, a
    /// nested file, and a `target/` that [`default_skip`] must drop before descending into it.
    const TREE: &[(&str, &str, bool, u64)] = &[
        ("/w", "a b", true, 0),
        ("/w", "empty", true, 0),
        ("/w", "plain.txt", false, 12),
        ("/w", "target", true, 0),
        ("/w/a b", "inner.txt", false, 7),
        ("/w/target", "junk", false, 999),
    ];

    fn kids_of(entries: &[(&str, &str, bool, u64)]) -> HashMap<String, Vec<DirEntry>> {
        let mut kids: HashMap<String, Vec<DirEntry>> = HashMap::new();
        for (parent, name, is_dir, bytes) in entries {
            kids.entry((*parent).to_string())
                .or_default()
                .push(DirEntry {
                    path: format!("{parent}/{name}"),
                    name: (*name).to_string(),
                    is_dir: *is_dir,
                    bytes: *bytes,
                    modified: None,
                });
        }
        // Firecode's `list` sorts by name, and the loop's order is what an override
        // has to reproduce, so the fake sorts the same way.
        for v in kids.values_mut() {
            v.sort_by(|a, b| a.name.cmp(&b.name));
        }
        kids
    }

    fn paths(entries: &[DirEntry]) -> Vec<&str> {
        entries.iter().map(|e| e.path.as_str()).collect()
    }

    /// A backend that is the tree in memory, with no `list_tree` of its own: the
    /// trait's default drives it one directory at a time, and the count of `list`
    /// calls is what says so.
    struct TreeFake {
        kids: HashMap<String, Vec<DirEntry>>,
        list_calls: AtomicUsize,
    }

    impl ExecBackend for TreeFake {
        fn run(&self, _cmd: &Command) -> Result<Output, BackendError> {
            Err(BackendError::Unsupported("a fake runs nothing"))
        }
        fn read(&self, path: &str) -> Result<Vec<u8>, BackendError> {
            Err(BackendError::NotFound(path.to_string()))
        }
        fn write(&self, path: &str, _bytes: &[u8]) -> Result<(), BackendError> {
            Err(BackendError::NotFound(path.to_string()))
        }
        fn list(&self, path: &str) -> Result<Vec<DirEntry>, BackendError> {
            self.list_calls.fetch_add(1, Ordering::SeqCst);
            // A directory the tree does not mention is empty, not an error: an empty
            // directory is a real directory and the loop must call `list` for it.
            Ok(self.kids.get(path).cloned().unwrap_or_default())
        }
        fn stat(&self, _path: &str) -> Option<DirEntry> {
            None
        }
        fn describe(&self) -> String {
            "a tree in memory".into()
        }
        fn processes_arc(&self) -> Option<std::sync::Arc<dyn crate::exec::ProcessHost>> {
            None
        }
    }

    /// The same shape with `list_tree` defined, as firecode defines it. An
    /// override that quietly fell back on the loop would still return entries, so the
    /// counts are the assertion and the entries alone are not.
    struct OneCallFake {
        tree_calls: AtomicUsize,
        list_calls: AtomicUsize,
    }

    impl ExecBackend for OneCallFake {
        fn run(&self, _cmd: &Command) -> Result<Output, BackendError> {
            Err(BackendError::Unsupported("a fake runs nothing"))
        }
        fn read(&self, path: &str) -> Result<Vec<u8>, BackendError> {
            Err(BackendError::NotFound(path.to_string()))
        }
        fn write(&self, path: &str, _bytes: &[u8]) -> Result<(), BackendError> {
            Err(BackendError::NotFound(path.to_string()))
        }
        fn list(&self, _path: &str) -> Result<Vec<DirEntry>, BackendError> {
            self.list_calls.fetch_add(1, Ordering::SeqCst);
            Ok(Vec::new())
        }
        fn stat(&self, _path: &str) -> Option<DirEntry> {
            None
        }
        fn describe(&self) -> String {
            "a tree in memory, in one command".into()
        }
        fn processes_arc(&self) -> Option<std::sync::Arc<dyn crate::exec::ProcessHost>> {
            None
        }
        fn list_tree(
            &self,
            root: &str,
            _limit: usize,
            _skip: &dyn Fn(&DirEntry) -> bool,
        ) -> Result<(Vec<DirEntry>, bool), BackendError> {
            self.tree_calls.fetch_add(1, Ordering::SeqCst);
            Ok((
                vec![DirEntry {
                    path: format!("{root}/one"),
                    name: "one".into(),
                    is_dir: false,
                    bytes: 1,
                    modified: None,
                }],
                false,
            ))
        }
    }

    /// The seam's cost, counted: a backend whose `list_tree` is defined is asked once for the
    /// whole tree and never per directory.
    #[test]
    fn a_defined_list_tree_is_one_call_and_never_the_loop() {
        let b = OneCallFake {
            tree_calls: AtomicUsize::new(0),
            list_calls: AtomicUsize::new(0),
        };
        let (got, truncated) = walk(&b, "/w", 100, &default_skip);
        assert_eq!(
            paths(&got),
            vec!["/w/one"],
            "the override's answer comes back"
        );
        assert!(!truncated);
        assert_eq!(b.tree_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            b.list_calls.load(Ordering::SeqCst),
            0,
            "the override fell back to the per-directory loop"
        );
    }

    /// The default is still the loop, one call per directory, in its own order - and
    /// none at all for a directory `skip` removed before descent.
    #[test]
    fn the_default_list_tree_is_the_per_directory_loop() {
        let b = TreeFake {
            kids: kids_of(TREE),
            list_calls: AtomicUsize::new(0),
        };
        let (got, truncated) = walk(&b, "/w", 100, &default_skip);
        assert_eq!(
            paths(&got),
            vec!["/w/a b", "/w/empty", "/w/plain.txt", "/w/a b/inner.txt"],
            "breadth-first, siblings by name, skipped directories not descended"
        );
        assert!(!truncated);
        assert_eq!(
            b.list_calls.load(Ordering::SeqCst),
            3,
            "one call per directory: /w, /w/a b, /w/empty — and none for the skipped target"
        );
    }

    /// `find`'s listing and the loop's answer are the same list, entry for entry and in the same
    /// order, which is the whole contract an override has to keep. The listing is written the way
    /// `find` prints it, depth-first, with a name containing a space and a directory entry.
    #[test]
    fn the_find_listing_parses_byte_identically_to_the_loop() {
        let b = TreeFake {
            kids: kids_of(TREE),
            list_calls: AtomicUsize::new(0),
        };
        let listing: &[u8] = b"d\t0\t/w/a b\n\
                              f\t7\t/w/a b/inner.txt\n\
                              d\t0\t/w/empty\n\
                              f\t12\t/w/plain.txt\n\
                              d\t0\t/w/target\n\
                              f\t999\t/w/target/junk\n";
        let (looped, cut_loop) = walk(&b, "/w", 100, &default_skip);
        let (found, cut_find) =
            crate::firecode::tree_in_walk_order(listing, "/w", 100, &default_skip);
        assert_eq!(
            found, looped,
            "the one-command listing and the loop disagree"
        );
        assert_eq!(cut_find, cut_loop);
        // And the bound cuts both in the same place, saying so the same way.
        let (few_loop, cut_loop) = walk(&b, "/w", 2, &default_skip);
        let (few_find, cut_find) =
            crate::firecode::tree_in_walk_order(listing, "/w", 2, &default_skip);
        assert_eq!(few_find, few_loop);
        assert_eq!((cut_find, cut_loop), (true, true));
        assert_eq!(paths(&few_find), vec!["/w/a b", "/w/empty"]);
    }

    /// A backend whose `read` is counted and whose `read_many` is the default: the
    /// shape of every backend that has not said its `read` is a round trip.
    struct PerFileReads {
        read_calls: AtomicUsize,
    }

    impl ExecBackend for PerFileReads {
        fn run(&self, _cmd: &Command) -> Result<Output, BackendError> {
            Err(BackendError::Unsupported("a fake runs nothing"))
        }
        fn read(&self, path: &str) -> Result<Vec<u8>, BackendError> {
            self.read_calls.fetch_add(1, Ordering::SeqCst);
            if path.ends_with("gone") {
                return Err(BackendError::NotFound(path.to_string()));
            }
            Ok(format!("body of {path}").into_bytes())
        }
        fn write(&self, path: &str, _bytes: &[u8]) -> Result<(), BackendError> {
            Err(BackendError::NotFound(path.to_string()))
        }
        fn list(&self, _path: &str) -> Result<Vec<DirEntry>, BackendError> {
            Ok(Vec::new())
        }
        fn stat(&self, _path: &str) -> Option<DirEntry> {
            None
        }
        fn describe(&self) -> String {
            "reads one file at a time".into()
        }
        fn processes_arc(&self) -> Option<std::sync::Arc<dyn crate::exec::ProcessHost>> {
            None
        }
    }

    /// The default `read_many` is the per-file loop, one `read` per path and the same answers in
    /// the same order, which is what makes this seam safe to add under `grep`.
    #[test]
    fn the_default_read_many_is_the_per_file_loop() {
        let b = PerFileReads {
            read_calls: AtomicUsize::new(0),
        };
        let got = b
            .read_many(&["/w/a", "/w/gone", "/w/b"])
            .expect("the default cannot fail as a whole");
        assert_eq!(
            b.read_calls.load(Ordering::SeqCst),
            3,
            "one read per path: {got:?}"
        );
        assert_eq!(got[0].as_ref().unwrap(), b"body of /w/a");
        assert!(
            matches!(got[1].as_ref(), Err(BackendError::NotFound(_))),
            "a path that cannot be read is that SLOT's answer, not the call's"
        );
        assert_eq!(got[2].as_ref().unwrap(), b"body of /w/b");
        // And nothing is asked of the backend for nothing.
        assert_eq!(b.read_many(&[]).expect("an empty page").len(), 0);
        assert_eq!(b.read_calls.load(Ordering::SeqCst), 3);
    }
}

/// A tiny temp-directory helper for this crate's tests: the workspace has no `tempfile`
/// dependency. `pub` because the built-ins' tests need the same fixture.
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
                "letibot-tools-{}-{}-{}-{n}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0),
                {
                    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                    N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                }
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
