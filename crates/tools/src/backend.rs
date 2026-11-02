//! The execution-backend interface: every built-in reaches the world through
//! [`ExecBackend`] and through nothing else, which is what makes a substrate swap one
//! `Box<dyn ExecBackend>` rather than a rewrite of the tools.
//!
//! [`HostBackend`] implements read, list, stat and write (write only when it was opened
//! with [`HostBackend::writable`]). `run` is refused: exec needs a boundary, and a
//! boundary is a substrate rather than a policy, so it arrives with the firecode backend.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

/// Distinguishes the temp files two concurrent writes create in one directory.
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// A command to run: `run(cmd, cwd, env)`.
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
    /// The path does not exist. Distinguished from every other failure because the
    /// tools' help-on-a-miss behaviour hangs off it: this is the one error a tool can
    /// answer with a suggestion instead of a refusal.
    NotFound(String),
    /// The path resolves outside the backend's root.
    Outside(String),
    NotADirectory(String),
    IsADirectory(String),
    /// Named, so a missing capability says which backend lacks it rather than failing
    /// silently.
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

/// The seam. A substrate implements it and the tools above it do not change.
pub trait ExecBackend: Send + Sync {
    /// `run(cmd, cwd, env) -> (stdout, stderr, exit)`.
    fn run(&self, cmd: &Command) -> Result<Output, BackendError>;

    /// The boundary this backend's commands run inside, when it has one: the view and the
    /// egress that were measured, not the ones that were asked for. `harnessd` reads it as
    /// `backend_confined`, and the gate reads it for the facts a card carries.
    ///
    /// By value because a local backend's confinement can be replaced by a grant, so it
    /// lives behind a lock and cannot be borrowed out of it.
    fn boundary(&self) -> Option<crate::exec::Boundary> {
        None
    }

    /// Why a command here is not confined, when confinement was wanted and is not there: the
    /// probe's own reason, for the card and the banner. `None` means no question was asked,
    /// which is a different fact from asked and the host could not answer.
    fn boundary_refusal(&self) -> Option<String> {
        None
    }

    /// Bind a path into this session's view, after somebody decided it may be.
    ///
    /// Refusing is the right answer for a backend with no view: an unconfined session can
    /// already reach the path and a VM's view is the VM's. A refusal rather than a silent
    /// success, so a caller never reports a grant nobody made.
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

    /// The same answers as [`Self::read`], for a whole page of paths, in one call: the seam
    /// a backend whose `read` is a round trip overrides. The default is the per-file loop, so
    /// every other backend keeps its behaviour byte for byte.
    ///
    /// One entry per path, in the order asked, each exactly what [`Self::read`] would have
    /// answered. `Err` from the call means the channel failed and says nothing about any
    /// path: fall back to [`Self::read`] rather than reading it as N unreadable files.
    fn read_many(
        &self,
        paths: &[&str],
    ) -> Result<Vec<Result<Vec<u8>, BackendError>>, BackendError> {
        Ok(paths.iter().map(|p| self.read(p)).collect())
    }

    /// Replace a file's contents.
    ///
    /// Required to be atomic, and that is part of the interface rather than an
    /// implementation note: a half-written file is worse than a refused edit, because
    /// nobody - not the model, not the operator, not the next call - can predict what it
    /// holds. An implementation that cannot write atomically must fail.
    ///
    /// Missing parent directories are created: a tool that had to call `mkdir` first would
    /// need a `mkdir` in this trait or a shell, and both are larger holes than this.
    fn write(&self, path: &str, bytes: &[u8]) -> Result<(), BackendError>;

    /// The process host, when this backend can start a process whose lifetime is owned by
    /// something. `None` by default, and that default is the safety property: no backend
    /// gains an exec path by this method existing.
    ///
    /// A cgroup bounds a process's lifetime, not what it can read, so this is not a guest
    /// boundary and must never be described as one.
    fn processes(&self) -> Option<&dyn crate::exec::ProcessHost> {
        None
    }

    /// The same host as an owned handle, for a watcher that has to outlive the call that
    /// noticed the job: a background job settles between turns, when no invocation is in
    /// scope to borrow through.
    ///
    /// No default, on purpose: a backend that can start processes must say so here too, or
    /// every job it starts runs unwatched, and the compiler is what makes it say so.
    fn processes_arc(&self) -> Option<std::sync::Arc<dyn crate::exec::ProcessHost>>;

    /// A tool's `cwd` argument, relative to where the session sits, in the form
    /// [`crate::exec::ProcessHost::spawn`] wants it: relative to the root.
    fn workdir(&self, cwd: &str) -> Result<String, BackendError> {
        Ok(cwd.to_string())
    }

    /// Whether [`ExecBackend::write`] can do anything.
    ///
    /// Defaults to `false`: a backend that has not said it is writable is not writable. A
    /// tool asks so its refusal can name which of the two gates stopped it.
    fn is_writable(&self) -> bool {
        false
    }

    /// The backend's root as a path a caller may compare an argument against.
    ///
    /// `None` by default: a tar channel or a guest seen from the host has no host path
    /// that means anything here, and inventing one would make a containment check answer
    /// confidently about a filesystem it is not looking at.
    fn root_path(&self) -> Option<String> {
        None
    }

    /// Where a relative path starts, as this backend is actually configured. `None` when
    /// the substrate has no such thing.
    ///
    /// Not [`Self::root_path`]: an unconfined session's root is `/`, which answers *is this
    /// path inside the boundary* and not *which directory is this session working in*.
    /// Filling a fact named `workspace` from the root puts `/` on every card.
    ///
    /// A trait method rather than a downcast, because the code that fills a card's facts
    /// holds a `dyn ExecBackend`.
    fn workspace_path(&self) -> Option<String> {
        None
    }

    /// Which machine the tools run on, when it is not this one. `None` for every backend
    /// whose files are here.
    ///
    /// A directory alone is the least useful half of the answer over ssh: the path names a
    /// machine the card never mentions, and a person deciding a `bash` call needs to know
    /// it lands on another one.
    fn host_label(&self) -> Option<String> {
        None
    }

    /// The session's scratch directory, where tools put working artifacts too big for the
    /// transcript. Per-session and set by whoever opened the session, so every tool in it
    /// sees the same place and no tool has to know how it was chosen.
    ///
    /// `None` by default: a tool that wants one gets a refusal rather than a guess.
    fn scratch_dir(&self) -> Option<String> {
        None
    }

    /// The operator's home, as this backend expands a leading `~`. `None` by default, for
    /// the same reason [`Self::root_path`] is: a substrate that reads no host path has no
    /// home to name.
    ///
    /// The reader that pulls the script an interpreter was handed asks this rather than
    /// reading `$HOME` again, so `~` means the same thing to it and to
    /// [`HostBackend::resolve`].
    fn home_path(&self) -> Option<String> {
        None
    }

    /// One directory, not recursive. Recursion belongs to the tools, which then work
    /// identically over a tar channel that has no `walkdir`.
    ///
    /// That rule is a trade, and it is the condition to weigh before overriding
    /// [`Self::list_tree`]: one call per directory is right when one call is cheap and
    /// wrong when it is a network round trip. `list` stays the primitive either way.
    fn list(&self, path: &str) -> Result<Vec<DirEntry>, BackendError>;

    /// The tree under `root`, in the order a walk of [`Self::list`] produces: the seam a
    /// backend whose `list` is expensive overrides.
    ///
    /// The default is that walk - breadth-first, `skip` tested before descent so a skipped
    /// directory's contents are never fetched, `limit` stopping it where it stands with the
    /// flag set.
    ///
    /// An override must return the same entries in the same order, `skip` and `limit`
    /// included, or a tool's answer depends on the substrate it ran on.
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
                    // Bounded, and the caller is told it was bounded: a silent cut is the
                    // failure the tool layer above refuses.
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

    /// A head asked to move the running command to the background, and this is who asked.
    /// `None` when no such request is pending. Taking it clears it, so the `bash` tool acts
    /// once.
    ///
    /// `None` by default: a backend with no exec path has nothing to move. The host backend
    /// carries the channel the daemon wires from the hub, so the `bash` wait loop honours a
    /// request without the worker - which is blocked in that wait - delivering it.
    fn promote_requested(&self) -> Option<String> {
        None
    }

    /// The session is over: release what the backend holds and say where its work went, if
    /// anywhere a caller should know about. `None` by default, since a host backend holds
    /// nothing; a firecode backend brings its VM down and names the directory the guest's
    /// tree landed in.
    fn close(&self) -> Option<String> {
        None
    }
}

/// The host filesystem, confined to a root.
///
/// Not a sandbox and does not pretend to be one: it is the smallest thing that makes "a
/// read-only tool never prompts" safe to ship before a real boundary exists, since a read
/// tool that can reach `~/.ssh` is a tool that should have prompted.
///
/// [`HostBackend::new`] gives a read-only backend and [`HostBackend::writable`] one that can
/// write. That asymmetry is the point: a write tool registered into an old session refuses
/// at the backend even if something went wrong at the gate, and two independent mechanisms
/// is what keeps a safety property from shipping broken the first time one is refactored.
#[derive(Debug, Clone)]
pub struct HostBackend {
    root: PathBuf,
    writable: bool,
    /// The operator's home, for expanding a leading `~`. Read once at construction, so
    /// `resolve` does not reach into the environment on every call and the value is a
    /// property of the backend rather than of the process calling it.
    home: PathBuf,
    /// Where a RELATIVE path starts. The root for a confined backend; the session's
    /// workspace for one rooted at `/`. A relative path is relative to where the session
    /// sits: being able to reach the whole host is not the same as starting there.
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
    /// This session's `/tmp`, when its commands run inside a boundary: the one directory
    /// [`HostBackend::with_session_tmp`] bound there. Kept so that closing the session can
    /// remove what its commands left behind, which nothing else knows the name of.
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

    /// Set the session's scratch directory. A backend built without it has no scratch, and
    /// a tool that wants one gets a refusal rather than a guess.
    ///
    /// Both halves from one call: the tools read it through [`ExecBackend::scratch_dir`] and
    /// the process host writes a promoted run's output into it, so a host pointed somewhere
    /// else than the tools would be the same defect one layer down.
    pub fn with_scratch_dir(mut self, dir: impl AsRef<Path>) -> Self {
        self.scratch = Some(dir.as_ref().to_path_buf());
        if let Some(host) = &self.processes {
            host.set_scratch_dir(dir.as_ref());
        }
        self
    }

    /// Give this session a `/tmp` that outlives one command, by binding `dir` there.
    ///
    /// A fresh tmpfs per command loses a file between two calls, which reads as a broken
    /// shell rather than as a boundary, and agents write scratch there constantly: a heredoc
    /// patch in one call, `git apply` in the next.
    ///
    /// A new view and a new probe, because the argv is built once and what was measured has
    /// to be what runs. A backend with no view is returned unchanged: its `/tmp` is the
    /// machine's own already. The directory is created by the probe and removed by
    /// [`ExecBackend::close`].
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

    /// A backend that can change the operator's tree.
    ///
    /// Its own constructor so that `grep -rn 'HostBackend::writable'` finds every place where
    /// that became possible.
    pub fn writable(root: impl AsRef<Path>) -> Result<Self, BackendError> {
        Ok(HostBackend {
            writable: true,
            ..Self::new(root)?
        })
    }

    /// A backend that can start processes: writable, and unsandboxed.
    ///
    /// Its own constructor because the string is how grep finds every session with an exec
    /// path, and it is not reachable from the other two.
    ///
    /// It buys [`crate::exec`]: every process lands in a cgroup owned by a scope, and a scope
    /// that ends kills its cgroup and records what it killed. It does not buy a boundary - a
    /// command here has this user's rights over this user's whole filesystem, since a cgroup
    /// bounds a lifetime and not a view - which is why [`ExecBackend::describe`] says
    /// `NOT CONFINED`. [`HostBackend::confined`] is the same thing with the namespaces added.
    ///
    /// Fails rather than degrading when there is no cgroup v2 subtree to delegate: a process
    /// with no owner is a leak nobody reaps.
    pub fn executable(root: impl AsRef<Path>) -> Result<Self, BackendError> {
        let base = Self::writable(root)?;
        let host = crate::exec::HostProcesses::new(base.root.clone())
            .map_err(|e| BackendError::Io(e.to_string()))?
            // The operator's own `!` line runs under this one, and `/bin/bash` on a stock
            // Mac is 3.2 while the rc chain it reads with `-ic` wants bash 4 or later, so the
            // row came back as a shell error before the operator's command.
            // `crate::console_bash` picks the program and leaves the flags alone. Only this
            // constructor: a confined view's `$HOME` is a tmpfs, so the rc there is nobody's.
            .with_console_shell(crate::console_bash::shell());
        Ok(HostBackend {
            processes: Some(std::sync::Arc::new(host)),
            ..base
        })
    }

    /// A backend that can start processes inside a project-scoped boundary.
    ///
    /// The one constructor that narrows rather than widens, spelled the same way: grep for it
    /// and a session's absence of the string is evidence rather than a question.
    ///
    /// It buys a mount namespace rooted at the project (so `~/.ssh` is absent, not denied), a
    /// PID namespace (so a sibling job is invisible and unsignalable), a network namespace
    /// with no route out, and the user namespace that makes those possible unprivileged. It
    /// does not buy the other half of the secret invariant: bytes stay outside the view, but
    /// a tool result that already holds one still travels into the transcript.
    ///
    /// Two independent ways to fail and the error says which: no delegated cgroup v2 subtree,
    /// or no usable boundary. Neither falls back to [`HostBackend::executable`]. Which
    /// mechanism a platform uses is [`crate::exec::confine::probe`]'s decision, and what a
    /// caller gets either way is a measured [`crate::exec::Boundary`] that says what it could
    /// not give.
    pub fn confined(root: impl AsRef<Path>) -> Result<Self, BackendError> {
        Self::confined_granting(root, Vec::new())
    }

    /// A confined backend whose view also holds `grants`.
    ///
    /// A grant is a decision with a consequence: a `ReadOnly` path is readable inside the
    /// view and therefore into the transcript, so each carries a `why` and is printed beside
    /// the boundary. Nothing here decides what to grant - the caller does, from a flag the
    /// operator typed, which keeps a capability from arriving as a side effect.
    pub fn confined_granting(
        root: impl AsRef<Path>,
        grants: Vec<crate::exec::confine::Grant>,
    ) -> Result<Self, BackendError> {
        let base = Self::writable(root)?;
        let mut view = crate::exec::confine::ViewSpec::project_only(base.root.clone());
        for g in grants {
            view = view.granting(g);
        }
        let confine = crate::exec::confine::probe(view, crate::exec::confine::Egress::Denied)
            .map_err(|e| BackendError::Io(e.to_string()))?;
        let host = crate::exec::HostProcesses::confined(base.root.clone(), confine)
            .map_err(|e| BackendError::Io(e.to_string()))?;
        Ok(HostBackend {
            processes: Some(std::sync::Arc::new(host)),
            ..base
        })
    }

    /// An executable backend over a chosen process host: the seam tests reach through, and
    /// the one a daemon uses when it wants to declare what it manages
    /// ([`crate::exec::HostProcesses::protect_listener`]) before any tool runs.
    pub fn executable_with(
        root: impl AsRef<Path>,
        host: std::sync::Arc<crate::exec::HostProcesses>,
    ) -> Result<Self, BackendError> {
        Ok(HostBackend {
            processes: Some(host),
            ..Self::writable(root)?
        })
    }

    /// Bind a path into this session's view, now, and re-probe so the boundary that is
    /// described is the boundary that runs.
    ///
    /// The argv a confined command is wrapped in is built once, at probe time, precisely so
    /// that what was measured is what runs, so widening the view is a new view spec, a new
    /// probe and the result taking the old one's place. Commands already running keep the
    /// namespace they started in; the next one gets this.
    ///
    /// Refuses on a backend with no view, rather than reporting a grant it did not make.
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
        let confine = crate::exec::confine::probe(widened, crate::exec::confine::Egress::Denied)
            .map_err(|e| BackendError::Io(e.to_string()))?;
        let described = crate::exec::Confinement::describe(&*confine);
        host.replace_confinement(confine);
        Ok(described)
    }

    /// The process host, concretely, for a caller that needs to end a scope or read the reap
    /// log - neither of which is a tool call.
    pub fn host_processes(&self) -> Option<&std::sync::Arc<crate::exec::HostProcesses>> {
        self.processes.as_ref()
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Override the home directory used to expand a leading `~`. The harness sets it from the
    /// session's [`crate::intent::Surroundings`], so `~` is a property of the session rather
    /// than of the process that happens to be calling.
    pub fn with_home(mut self, home: impl Into<PathBuf>) -> Self {
        self.home = home.into();
        self
    }

    /// Resolve a tool-supplied path inside the root, following symlinks as it goes rather than
    /// checking one at the end.
    ///
    /// The containment test has to be answerable about a path whose tail does not exist yet:
    /// `canonicalize` fails there, and a check that is skipped when it cannot answer is not a
    /// check. So lexical normalisation first, then [`resolve_under`] walks the components from
    /// the root down, requiring containment at every step.
    ///
    /// Not made moot by [`crate::exec::confine`]: that confines the exec path, while `read`,
    /// `write`, `edit`, `glob` and `grep` are in-process calls in the daemon's own mount
    /// namespace and reach the world through this function.
    pub fn resolve(&self, path: &str) -> Result<PathBuf, BackendError> {
        // A leading `~` is the operator's home, read the way a shell reads it: a path the
        // model writes as `~/x` must not resolve to a literal directory named `~`.
        let expanded = expand_tilde(path, &self.home);
        let given = Path::new(&expanded);
        // A relative path starts where the session sits, not at the root: the two differ
        // only for a backend rooted at `/`, and there the difference is the whole workspace.
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

    /// The path a tool should show back to the model: relative to the root, `/` separated.
    /// An absolute host path in a tool result is staleness the model cannot undo.
    pub fn display(&self, p: &Path) -> String {
        // Under the workspace, relative to it; elsewhere under a `/` root, the absolute path,
        // because `home/dead/x` with the slash stripped resolves to nothing when handed back.
        let shown = match p.strip_prefix(&self.cwd) {
            Ok(r) => r,
            Err(_) if self.root == Path::new("/") => p,
            Err(_) => p.strip_prefix(&self.root).unwrap_or(p),
        };
        shown.to_string_lossy().replace('\\', "/")
    }
}

impl ExecBackend for HostBackend {
    /// The boundary the commands actually run inside, read off the process host rather than
    /// off the constructor that was called: a grant replaces the confinement, so the measured
    /// one is the one that answers.
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

    /// Runs only on a backend built by [`HostBackend::executable`], and even there through the
    /// job machinery rather than beside it.
    ///
    /// There is deliberately no second exec path: a `Command` that bypassed [`crate::exec`]
    /// would be a process with no scope to reap it and no capture to read afterwards, so this
    /// spawns a job in the turn scope and waits for it.
    fn run(&self, cmd: &Command) -> Result<Output, BackendError> {
        let Some(host) = &self.processes else {
            // A backend that did not ask to be executable refuses here and says why: the
            // guest boundary is still not what `executable` gives either.
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
                // The backend's own smoke run, named by nobody.
                slug: None,
                cwd,
                scope: ScopeKind::Turn,
                scope_name: None,
                background: false,
                env: cmd.env.clone(),
                // No terminal: this is the synchronous seam a built-in reaches the world
                // through, and its reader is a tool that wants bytes. The operator's colour is
                // set on the `bash` path, where the caller is known to be the person.
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
            // A deadline is not an exit code and is not dressed as one.
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
    /// Each step is a failure somebody has met - a temp in `/tmp` is a different filesystem,
    /// a lost mode disarms an executable script, an unsynced rename publishes an empty inode.
    ///
    /// A symlink is followed to its target rather than replaced, because replacing it is a
    /// different edit from the one that was asked for, and [`HostBackend::resolve`] has
    /// already checked that target. The temp file is removed on every failure path, so a
    /// refused write leaves no litter to be mistaken for a partial one.
    fn write(&self, path: &str, bytes: &[u8]) -> Result<(), BackendError> {
        use std::io::Write;

        if !self.writable {
            return Err(BackendError::Unsupported(
                "this session's backend was opened read-only (HostBackend::new); \
                 a session that may change files opens it with HostBackend::writable",
            ));
        }
        // `resolve` has already followed every symlink on the way down and refused any that
        // left the root, including on a path whose tail does not exist yet. The
        // `canonicalize` is belt and braces for a component that changed since, and falls
        // back because a file about to be created has nothing to canonicalise.
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
        // In the same directory, and a name nothing else will pick.
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
            // The data reaches the disk before the rename publishes it.
            f.sync_all()?;
            drop(f);
            // Inherit the original's mode. A NEW file gets 0644 rather than the 0600 it was
            // created with, because a source file the build or an editor cannot read is a
            // surprise that shows up elsewhere and the confidentiality inside a project tree
            // is nil. Fixed rather than umask-derived: `std` does not expose the umask.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(&target)
                    .map(|m| m.permissions().mode())
                    .unwrap_or(0o644);
                std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))?;
            }
            std::fs::rename(&tmp, &target)?;
            // Best effort: a filesystem that will not let us open a directory is not a reason
            // to report a write that succeeded as failed.
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

    /// The session's working directory, which for an unconfined session is not the root:
    /// `harnessd` builds one as `HostBackend::executable("/").with_cwd(&cfg.workspace)`, so
    /// `/` is the root and the workspace is the cwd. Both true, different facts.
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

    /// The session is over, so its `/tmp` goes with it: the directory holds what a confined
    /// command left there and nothing outside the session can want it. Best effort and
    /// deliberately silent, since a cleanup that failed is not a write that did not land.
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

    /// Every word here is read off the fields, because a hard-coded banner asserting a property
    /// the session does not have is a disclosure that lies. The exec half comes from the
    /// process host's measured confinement, so no constant here claims one.
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

    /// A request's `cwd` is relative to where the session sits, the same as every other path a
    /// tool hands this backend; the process host wants it relative to the root. They differ
    /// only for a backend rooted at `/`.
    fn workdir(&self, cwd: &str) -> Result<String, BackendError> {
        let dir = self.resolve(cwd)?;
        Ok(dir
            .strip_prefix(&self.root)
            .unwrap_or(&dir)
            .to_string_lossy()
            .into_owned())
    }

    fn promote_requested(&self) -> Option<String> {
        // Take, not read: whoever acts on the request clears it, so a second pass does not
        // promote the same command twice.
        self.promote
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }
}

/// The most symlinks one path may traverse. Linux's own limit is 40 (`ELOOP` above it), so a
/// path this refuses is a path the kernel would have refused too and no legitimate tree
/// becomes unusable.
const MAX_SYMLINK_HOPS: usize = 40;

/// A `realpath` for a path whose tail may not exist yet - the containment primitive.
///
/// Its shape is forced by the requirement: the walk goes down from the root one component at
/// a time. A component that does not exist ends the resolving, because nothing under a
/// non-existent directory can exist either; a symlink is replaced by its target and pushed
/// back onto the front of the queue; `..` pops.
///
/// `None` means outside the root or unresolvable, and popping past the root is a refusal
/// rather than a clamp: a clamp answers a different question from the one asked. An `Option`
/// rather than a `bool` plus an out-parameter, because there is no way to spell "refused" and
/// still have a path to use.
/// A leading `~` is the operator's home, read the way a shell reads it: `~` alone is the home
/// itself and `~/x` is `x` under it. A path that is not `~`-prefixed, or a home that is
/// empty, is returned unchanged - a literal `~` directory is the honest failure.
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

/// An absolute path, as a remainder under `root` - also when it is spelled through a symlinked
/// ancestor, since the root is canonical and the path need not be (on macOS `/var` is
/// `/private/var`). When the plain strip fails, the longest existing ancestor is canonicalised
/// and the strip tried again.
///
/// Containment is not decided here: the remainder still goes through [`resolve_under`]'s walk.
/// A `..` in the tail ends the attempt, which is a refusal and never a guess.
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

    // The work queue, in order. A symlink target is prepended, which is what makes a chain of
    // links terminate in one loop rather than in recursion.
    let mut queue: VecDeque<OsString> = VecDeque::new();
    for c in rel.components() {
        match c {
            Component::Normal(p) => queue.push_back(p.to_os_string()),
            Component::CurDir => {}
            Component::ParentDir => queue.push_back(OsString::from("..")),
            // An absolute or prefixed path inside what should be a relative remainder is not a
            // path this root can contain.
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
        // `metadata`, and it is the dangling one that would otherwise get through.
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
        // restarts the walk at the root and must lie inside it, which is the check that has to
        // happen on every link rather than once at the end.
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
/// escape the quote, reopen. Used where the contract is argv and the substrate takes a command
/// string.
pub(crate) fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-/=:,+@".contains(c))
    {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Recursive listing, over [`ExecBackend::list_tree`], written against the trait so it works
/// unchanged over a backend whose filesystem is a tar channel. `limit` bounds the walk: a tool
/// that walks an unbounded tree is a tool that hangs on a home directory.
///
/// An error - a root that does not exist, most often - is an empty walk, which is what a
/// per-directory loop does with a directory it cannot list.
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
    // On a backend rooted at `/` a walk can reach the kernel's own trees. `/proc` holds files
    // whose size lies and whose reads block, `/sys` and `/dev` are not files, and `/run` is
    // sockets and pid files. Matched on the absolute path, so a project directory that happens
    // to be called `dev` is still walked.
    matches!(e.path.as_str(), "/proc" | "/sys" | "/dev" | "/run")
}

#[cfg(test)]
mod tests {

    /// An unconfined seat is built as `HostBackend::executable("/").with_cwd(&workspace)`, so
    /// the two accessors answer two different questions: filling a fact named `workspace` from
    /// `root_path()` puts `/` on every card. A confined seat has them equal, which is why the
    /// wrong accessor looks right.
    #[test]
    fn a_seat_rooted_at_the_host_still_reports_its_own_workspace() {
        let dir = std::env::temp_dir()
            .canonicalize()
            .expect("a canonical temp dir");
        // `writable` rather than `executable`: the same root and the same `with_cwd`, and it
        // needs no cgroup to build.
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
        // The property the second question exists for: a relative path starts here, not at `/`.
        assert_eq!(b.cwd(), dir.as_path());
        // A confined seat has the two equal, which is what hid the defect.
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

    /// A leading `~` expands to the backend's home, not to a literal directory named `~` under
    /// the root.
    #[test]
    fn a_leading_tilde_expands_to_the_backends_home() {
        let d = tempdir::TempDir::new();
        std::fs::create_dir_all(d.path().join("sub")).unwrap();
        std::fs::write(d.path().join("sub/f.txt"), "tilta\n").unwrap();
        // Root at `/` so the home (under the temp dir) is reachable, and pin the home to prove
        // `~` means it and not a literal `~` directory.
        let b = HostBackend::new("/")
            .unwrap()
            .with_home(d.path().to_path_buf());

        let p = b.resolve("~/sub/f.txt").unwrap();
        // Canonical, because the walk from `/` follows every link on the way: on macOS the
        // temp dir is under `/var`, which is `/private/var`.
        assert_eq!(p, d.path().canonicalize().unwrap().join("sub/f.txt"));
        assert_eq!(b.read("~/sub/f.txt").unwrap(), b"tilta\n");
        // `~` alone is the home itself.
        assert_eq!(b.resolve("~").unwrap(), d.path().canonicalize().unwrap());
    }

    /// The promote channel is read and cleared by the `bash` wait loop, so a head's request is
    /// honoured once.
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

    /// A first-party containment escape: `resolve` ended in `if let Ok(real) =
    /// out.canonicalize()`, which fails on a path that does not exist, so for a new file the
    /// containment check was skipped and the `let Ok` made the skip invisible.
    ///
    /// Not mooted by the namespaces: `read`, `write`, `edit`, `glob` and `grep` are in-process
    /// calls in the daemon's own mount namespace, so they reach the world through `resolve` and
    /// not through a boundary.
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
        // And through `write`, which is where the damage would land. Guard the fact, not the
        // proxy: nothing may appear on the operator's disk outside the root, whatever the call
        // returned.
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
    /// the link's absolute target need not be (on macOS `/var` is `/private/var`). Built with an
    /// explicit symlinked ancestor so it means the same on a box whose temp dir is canonical.
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
        // The other direction, because a containment fix that refuses everything looks exactly
        // like one that works. A project with an internal symlink is ordinary.
        let d = tempdir::TempDir::new();
        let root = d.path().join("root");
        std::fs::create_dir_all(root.join("real")).expect("real");
        // Canonical, so the `starts_with` checks below compare like with like (macOS: `/var` is
        // `/private/var`). The links are still made under it, absolute and relative.
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
        // The cost of resolving component by component is that a cycle is this function's
        // problem. It is bounded, and the bound is a refusal.
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
                });
        }
        // Firecode's `list` sorts by name, and the loop's order is what an override has to
        // reproduce, so the fake sorts the same way.
        for v in kids.values_mut() {
            v.sort_by(|a, b| a.name.cmp(&b.name));
        }
        kids
    }

    fn paths(entries: &[DirEntry]) -> Vec<&str> {
        entries.iter().map(|e| e.path.as_str()).collect()
    }

    /// A backend that is the tree in memory, with no `list_tree` of its own: the trait's default
    /// drives it one directory at a time, and the count of `list` calls is what says so.
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
            // A directory the tree does not mention is empty, not an error: an empty directory
            // is a real directory and the loop must call `list` for it.
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

    /// The same shape with `list_tree` defined, as firecode defines it. An override that quietly
    /// fell back on the loop would still return entries, so the counts are the assertion and the
    /// entries alone are not.
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
                }],
                false,
            ))
        }
    }

    /// A backend whose `list_tree` is defined is asked once for the whole tree and never per
    /// directory, which is the cost argument for the seam.
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

    /// The default is still the loop, one call per directory, in its own order, and none at all
    /// for a directory `skip` removed before descent.
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
    /// order, which is the whole contract an override has to keep. The listing is depth-first,
    /// carries a name with a space and a directory entry, and holds a `target/` the skip set must
    /// drop even though the command already fetched it.
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

    /// A backend whose `read` is counted and whose `read_many` is the default: the shape of every
    /// backend that has not said its `read` is a round trip.
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

    /// The default `read_many` is the per-file loop: one `read` per path, the same answers in the
    /// same order, which is what makes this seam safe to add under `grep`.
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
