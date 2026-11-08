//! The view half of the boundary: what a confined process can see, measured not asserted. An
//! absent file beats a denied one. [`Bwrap`] and [`super::seatbelt::Seatbelt`] answer through
//! [`Confinement`]; [`Boundary`] is what a probe measured. [`super::scope`] owns lifetime.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::ExecError;

/// The namespaces a boundary is made of. Declaration order is the order
/// [`Boundary::describe`] lists them in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Namespace {
    /// The one that makes the rest possible unprivileged.
    User,
    /// The project-rooted view: the enforcement point.
    Mount,
    /// The daemon, the model server and a sibling job become invisible.
    Pid,
    /// Egress becomes a decision rather than a default.
    Net,
    /// Shared memory and message queues, which are a channel between jobs.
    Ipc,
    /// Hostname. Cheap, and it stops a command renaming the operator's box.
    Uts,
    /// So `/proc/self/cgroup` does not hand out the path to its own reaper.
    Cgroup,
}

impl Namespace {
    /// The `/proc/self/ns/` entry, which is also the `readlink` target's prefix.
    pub fn proc_name(&self) -> &'static str {
        match self {
            Namespace::User => "user",
            Namespace::Mount => "mnt",
            Namespace::Pid => "pid",
            Namespace::Net => "net",
            Namespace::Ipc => "ipc",
            Namespace::Uts => "uts",
            Namespace::Cgroup => "cgroup",
        }
    }

    /// What entering this namespace buys, in the words a disclosure uses: the sentence
    /// rather than a category, because `describe()` is read by somebody deciding whether
    /// to seat `bash`.
    pub fn buys(&self) -> &'static str {
        match self {
            Namespace::User => "the rest of this is possible without root",
            Namespace::Mount => {
                "a project-rooted filesystem view: a secret outside it is ABSENT, not denied"
            }
            Namespace::Pid => {
                "the command cannot see or signal the daemon, the model server, or a sibling job"
            }
            Namespace::Net => "egress is a decision rather than a default",
            Namespace::Ipc => "no shared-memory channel to another job",
            Namespace::Uts => "the command cannot rename the host",
            Namespace::Cgroup => "`/proc/self/cgroup` does not hand out the path to its own reaper",
        }
    }

    /// The three without which there is no boundary worth the name. [`Namespace::Net`] is not
    /// among them: [`Egress::Host`] is a legitimate declared configuration, and requiring net
    /// would make the declaration unspellable.
    pub const REQUIRED: [Namespace; 3] = [Namespace::User, Namespace::Mount, Namespace::Pid];

    /// Every namespace this module asks for, in declaration order.
    pub const ALL: [Namespace; 7] = [
        Namespace::User,
        Namespace::Mount,
        Namespace::Pid,
        Namespace::Net,
        Namespace::Ipc,
        Namespace::Uts,
        Namespace::Cgroup,
    ];
}

impl std::fmt::Display for Namespace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.proc_name())
    }
}

/// A property of the boundary that is not a namespace but decides whether the
/// namespaces are worth anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SealKind {
    /// Can a process inside create a user namespace and step back out? Mount, PID and net
    /// namespaces are not a boundary if the child can leave them, so the helper is asked to
    /// seal it and the seal is measured anyway.
    NestedNamespaces,
    /// `PR_SET_NO_NEW_PRIVS`: without it a setuid binary reachable inside the view is an
    /// escalation, and an unprivileged boundary has nothing to escalate to. bubblewrap sets
    /// it; this reads the result rather than trusting that.
    NoNewPrivs,
}

impl SealKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            SealKind::NestedNamespaces => "nested-namespaces",
            SealKind::NoNewPrivs => "no-new-privs",
        }
    }

    pub fn buys(&self) -> &'static str {
        match self {
            SealKind::NestedNamespaces => {
                "a process inside cannot `unshare` itself back out of the boundary"
            }
            SealKind::NoNewPrivs => "a setuid binary inside cannot raise privilege",
        }
    }

    pub const ALL: [SealKind; 2] = [SealKind::NestedNamespaces, SealKind::NoNewPrivs];
}

impl std::fmt::Display for SealKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether a seal holds, tested rather than requested.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seal {
    /// A process inside tried the thing and could not do it.
    Held { how: String },
    /// It could. A fence with a gate in it is not a fence, and this refuses.
    Open { why: String },
    /// The test could not be run from inside: the property is unknown, and an unknown
    /// property is reported as unknown, never as held.
    Unverified { why: String },
    /// This mechanism has no such property to test: not `Held` (a claim about a mechanism
    /// that is not there), not `Unverified` (the test was not missed), and not `Open` (there
    /// is nothing to step through).
    NotApplicable { why: String },
}

impl Seal {
    pub fn held(&self) -> bool {
        matches!(self, Seal::Held { .. })
    }
    pub fn line(&self) -> String {
        match self {
            Seal::Held { how } => format!("held ({how})"),
            Seal::Open { why } => format!("**OPEN** — {why}"),
            Seal::Unverified { why } => format!("UNVERIFIED — {why}"),
            Seal::NotApplicable { why } => format!("NOT APPLICABLE on this platform — {why}"),
        }
    }
}

/// Which family of machinery a measured boundary is made of. A namespace boundary makes
/// what is outside it absent and a policy boundary makes it denied; the difference is
/// printed rather than averaged away.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Mechanism {
    /// Linux namespaces, entered through bubblewrap: `/proc/self/ns/*` read from
    /// inside, compared with the harness's own inodes, and the seals functionally
    /// tested. [`super::Bwrap`].
    Namespaces,
    /// A mandatory-access policy the kernel applies to the process: macOS's Seatbelt,
    /// through `sandbox-exec -f <profile>`. [`super::seatbelt::Seatbelt`].
    Policy,
}

impl Mechanism {
    pub fn as_str(&self) -> &'static str {
        match self {
            Mechanism::Namespaces => "namespaces",
            Mechanism::Policy => "policy",
        }
    }
}

impl std::fmt::Display for Mechanism {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A property a boundary can have that is not a namespace. A policy boundary has no
/// namespace table to describe itself with, so both families answer these instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Prop {
    /// A file outside the view cannot be reached: absent under namespaces, denied under a
    /// policy, and [`Boundary::describe`] says which.
    FileView,
    /// A process outside the boundary cannot be reached: a signal, or a lookup that would
    /// find it.
    ProcessIsolation,
    /// A process outside the boundary is not there at all. A PID namespace buys this;
    /// nothing on macOS does.
    ProcessAbsence,
    /// Egress is a decision rather than a default.
    Egress,
    /// A child cannot step out of the boundary its parent is in.
    Nested,
}

impl Prop {
    pub fn as_str(&self) -> &'static str {
        match self {
            Prop::FileView => "file-view",
            Prop::ProcessIsolation => "process-isolation",
            Prop::ProcessAbsence => "process-absence",
            Prop::Egress => "egress",
            Prop::Nested => "nested",
        }
    }

    /// What holding this buys, in the words a disclosure uses: the same question
    /// [`Namespace::buys`] answers, so the two tables read alike.
    pub fn buys(&self) -> &'static str {
        match self {
            Prop::FileView => {
                "the operator's credentials and profiles cannot be read, and a write outside \
                 the checkout cannot happen"
            }
            Prop::ProcessIsolation => {
                "the command cannot signal the daemon, the model server, or a sibling job"
            }
            Prop::ProcessAbsence => {
                "a process outside the boundary does not appear in a listing at all"
            }
            Prop::Egress => "egress is a decision rather than a default",
            Prop::Nested => {
                "a child cannot strip the boundary its parent is in, so wrapping a command is \
                 enough"
            }
        }
    }

    /// The three a policy boundary is not a boundary without. [`Prop::ProcessAbsence`] is
    /// not among them: refusing an OS for a property it has no kernel for would make the
    /// honest answer (disclosed, absent) unspellable.
    pub const REQUIRED_POLICY: [Prop; 3] = [Prop::FileView, Prop::ProcessIsolation, Prop::Nested];

    /// Every property a policy boundary reports, in the order a disclosure lists them.
    pub const ALL: [Prop; 5] = [
        Prop::FileView,
        Prop::ProcessIsolation,
        Prop::ProcessAbsence,
        Prop::Egress,
        Prop::Nested,
    ];
}

impl std::fmt::Display for Prop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What one [`Prop`] measured, and the third variant is the important one:
/// [`PropState::Absent`] is not a failure but an OS that cannot give the property, kept
/// apart from [`PropState::Unverified`] because nobody could is not somebody did not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropState {
    /// Measured holding, from inside, with how it was measured.
    Held { how: String },
    /// This operating system cannot give it, with what the kernel is missing.
    Absent { why: String },
    /// It could have been measured and was not. Reported as unknown, never as held.
    Unverified { why: String },
}

impl PropState {
    pub fn held(&self) -> bool {
        matches!(self, PropState::Held { .. })
    }

    pub fn line(&self) -> String {
        match self {
            PropState::Held { how } => format!("held ({how})"),
            PropState::Absent { why } => format!("**ABSENT — this OS cannot give it**: {why}"),
            PropState::Unverified { why } => format!("UNVERIFIED — {why}"),
        }
    }
}

/// What happened to one namespace, as read back. The middle variant is why there are three:
/// a namespace shared on purpose and one that failed to be entered look identical from
/// inside and are completely different facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NsState {
    /// Entered. The inode is the evidence: what `readlink /proc/self/ns/<n>` returned from
    /// inside, and it differs from the harness's own.
    Entered { inode: String },
    /// Shared with the harness because somebody declared it, with the declaration.
    SharedByDecision { why: String },
    /// Asked for and not entered. This is the state that refuses.
    NotEntered { why: String },
}

impl NsState {
    pub fn entered(&self) -> bool {
        matches!(self, NsState::Entered { .. })
    }

    /// One line for a disclosure.
    pub fn line(&self) -> String {
        match self {
            NsState::Entered { inode } => format!("entered ({inode})"),
            NsState::SharedByDecision { why } => {
                format!("SHARED WITH THE HOST by decision — {why}")
            }
            NsState::NotEntered { why } => format!("**NOT ENTERED** — {why}"),
        }
    }
}

/// Whether a confined command may reach the network, and it is a decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Egress {
    /// The default. An empty network namespace: no interfaces, no routes, no DNS, so a
    /// `curl` inside fails at the socket.
    Denied,
    /// The host's network namespace, shared because somebody said so, with the sentence
    /// they said it in. Recorded as [`NsState::SharedByDecision`] so no disclosure mistakes
    /// it for a boundary that failed.
    Host { why: String },
}

impl Egress {
    pub fn allowed(&self) -> bool {
        matches!(self, Egress::Host { .. })
    }
}

/// What `$HOME` is inside the view. It cannot be the operator's home, which the whole
/// boundary exists to leave outside, and it cannot be nothing: a shell with no writable
/// `$HOME` breaks tools that have nothing to do with credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HomeView {
    /// A fresh tmpfs each run: the safe default, and it costs a build cache under `$HOME`,
    /// so [`Boundary::describe`] says so rather than letting a re-download read as a
    /// mysterious slowdown.
    Tmpfs,
    /// A letibot-owned directory, bound read-write at its own path. Persistent across runs
    /// and not the operator's home.
    Dir(PathBuf),
}

/// What `/tmp` is inside the view. An agent's scratch lives there, and a filesystem that
/// forgets it between two calls makes every use of it look like a broken shell. The host's
/// `/tmp` is never bound either way: it holds other programs' sockets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TmpView {
    /// A fresh tmpfs per command: private to it, empty at its start, gone at its end. The
    /// default, because it claims nothing, and the one that breaks a two-call scratch idiom,
    /// so a caller with a session directory should say so.
    PerCommand,
    /// One letibot-owned directory, bound read-write at `/tmp`, so a file one call of the
    /// session writes is there for the next call of that session and for no other. Created by
    /// [`Bwrap::probe`] if it is not there, like [`HomeView::Dir`].
    Session(PathBuf),
}

/// The sentence a [`Presence::Replaced`] for `/tmp` carries when a session's directory is
/// bound there: an `ENOENT` under it is neither "this command started empty" nor the host's.
const SESSION_TMP_WHAT: &str = "this session's own directory, bound here: it survives between the calls of one session \
     and holds none of the host's files";

/// A path put inside the view on purpose, with the sentence that put it there. Every variant
/// carries a `why`, because a grant nobody can explain is a grant nobody can revoke.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Grant {
    /// Readable inside the view, and therefore readable into the transcript: a grant is a
    /// decision with that consequence, and [`Boundary::describe`] prints it next to the grant.
    ReadOnly { path: PathBuf, why: String },
    /// Writable inside the view, with the same consequence and one more.
    ReadWrite { path: PathBuf, why: String },
    /// The credential mechanism: a unix socket to an agent that holds a key, bound at
    /// [`AGENT_SOCKET`] with `SSH_AUTH_SOCK` pointing at it. The key never enters the mount
    /// namespace, so what crosses is a challenge and a signature.
    AgentSocket { path: PathBuf, why: String },
}

impl Grant {
    pub fn path(&self) -> &Path {
        match self {
            Grant::ReadOnly { path, .. }
            | Grant::ReadWrite { path, .. }
            | Grant::AgentSocket { path, .. } => path,
        }
    }

    pub fn why(&self) -> &str {
        match self {
            Grant::ReadOnly { why, .. }
            | Grant::ReadWrite { why, .. }
            | Grant::AgentSocket { why, .. } => why,
        }
    }

    /// One line for a disclosure, and it names the consequence, not only the path.
    pub fn line(&self) -> String {
        match self {
            Grant::ReadOnly { path, why } => format!(
                "{} — read-only, READABLE INTO CONTEXT: {why}",
                path.display()
            ),
            Grant::ReadWrite { path, why } => format!(
                "{} — read-write, READABLE INTO CONTEXT and writable: {why}",
                path.display()
            ),
            Grant::AgentSocket { path, why } => format!(
                "{} — agent socket at {AGENT_SOCKET}, usable but NOT readable (the key never enters the view): {why}",
                path.display()
            ),
        }
    }

    /// The authorised-`ssh` grant, from `$SSH_AUTH_SOCK`, or the refusal that names the
    /// mechanism nobody built. With no agent there is no way to use a key from inside, and
    /// binding the key instead is exactly what the rule forbids.
    pub fn agent_from_env(why: impl Into<String>) -> Result<Grant, ExecError> {
        let Some(sock) = std::env::var_os("SSH_AUTH_SOCK") else {
            return Err(ExecError::NoCredentialMechanism(no_agent_said()));
        };
        let path = PathBuf::from(sock);
        // Guard the fact, not the proxy: it has to be a socket. A regular file called
        // `agent.sock` would be a key by another name.
        let meta = std::fs::symlink_metadata(&path).map_err(|e| {
            ExecError::NoCredentialMechanism(format!(
                "`SSH_AUTH_SOCK` points at `{}`, which could not be read ({e}), so \
                 there is nothing to forward. Nothing was bound.",
                path.display()
            ))
        })?;
        use std::os::unix::fs::FileTypeExt;
        if !meta.file_type().is_socket() {
            return Err(ExecError::NoCredentialMechanism(format!(
                "`SSH_AUTH_SOCK` points at `{}`, which is not a socket ({:?}). Only a \
                 socket is forwardable: a socket is usable without being readable, \
                 and a file is not. Nothing was bound.",
                path.display(),
                meta.file_type()
            )));
        }
        Ok(Grant::AgentSocket {
            path,
            why: why.into(),
        })
    }
}

/// Why an `ssh` with no agent cannot run, for the model: what to do instead, and no argument
/// about the rule's source, which the reader can neither look up nor act on.
pub(crate) fn no_agent_said() -> String {
    "`SSH_AUTH_SOCK` is not set, so there is no agent to forward. A key file is NOT an \
     alternative: binding a private key into the view would make it readable, and a \
     readable key is one `cat` away from the transcript. Start an agent and load the key \
     (`eval $(ssh-agent); ssh-add <key>`); letibot forwards the agent, never the key. An \
     authorised `ssh` also needs a second decision, to reach a host, because the default \
     network namespace has no route out."
        .to_string()
}

/// Where a forwarded agent socket appears inside the view. Fixed, so that
/// `SSH_AUTH_SOCK` inside does not leak the operator's `/run/user/1000/...` path.
pub const AGENT_SOCKET: &str = "/run/letibot/agent.sock";

/// Where a [`HomeView::Tmpfs`] `$HOME` appears inside the view.
pub const TMPFS_HOME: &str = "/run/letibot/home";

/// What a host path is when looked at from inside the view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    /// Bound in: the host's file is the sandbox's file.
    Inside,
    /// The path exists inside the view, but as a fresh filesystem, so nothing the host keeps
    /// there is visible and a write does not reach the host.
    Replaced { what: &'static str },
    /// Not in the view at all. Absent, and no rule is what is stopping it.
    Outside,
}

/// The paths that exist inside the view but are not the host's, with what they are instead.
/// See [`Presence::Replaced`].
const REPLACED_ROOTS: &[(&str, &str)] = &[
    (
        "/tmp",
        "a fresh tmpfs, private to this command and empty at its start",
    ),
    (
        "/var/tmp",
        "a fresh tmpfs, private to this command and empty at its start",
    ),
    (
        "/run",
        "a fresh tmpfs, private to this command and empty at its start",
    ),
    (
        "/proc",
        "this command's own procfs, showing only the processes in its PID namespace",
    ),
    ("/dev", "a minimal device set, not the host's `/dev`"),
];

/// Which paths are in the view, and therefore which are absent from it. The project is bound
/// read-write at its own path so a compiler's diagnostic names the same file the `read` tool
/// names; rewriting the root would make every path a translation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewSpec {
    /// The workspace root. Read-write, at its own path.
    pub project: PathBuf,
    /// The read-only system roots a command needs to be a command at all.
    pub system: Vec<PathBuf>,
    pub home: HomeView,
    /// What `/tmp` is (see [`TmpView`]). `/var/tmp` and `/run` stay fresh tmpfs filesystems
    /// either way.
    pub tmp: TmpView,
    pub grants: Vec<Grant>,
    /// Files that must not be visible or connectable inside, whatever else the view binds:
    /// the host coordinator's socket and its lease and marker files are added at spawn time
    /// by [`room_files`], and a caller may add more.
    pub hidden: Vec<PathBuf>,
    /// Paths inside a read-write grant that stay read-only: a file the operator's own tools run
    /// later outside the boundary (git hooks and config) must not be writable from inside.
    pub protected: Vec<Protection>,
    /// The whole host filesystem is the view, at its own paths and writable as the user, with
    /// `home` as `$HOME`: a top-level session's seat, whose only fence is [`ViewSpec::masked`].
    pub wide: bool,
    /// Read and write, whatever else the view binds: the bwrap rendering puts an empty filesystem
    /// where the path is a directory and `/dev/null` where it is a file. A credential store has to
    /// be gone even from a view of the whole machine, and it is empty rather than absent.
    pub masked: Vec<PathBuf>,
}

/// A path a confined command must not write, and what to bind there when it does not exist. bwrap
/// can only mount over a path that is there, so an absent `hooks/` was one the command could make and
/// fill; the kind is carried, because the wrong placeholder is a different hole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Protection {
    pub path: PathBuf,
    pub kind: ProtectedKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtectedKind {
    Dir,
    File,
}

impl Protection {
    pub fn dir(path: impl Into<PathBuf>) -> Protection {
        Protection {
            path: path.into(),
            kind: ProtectedKind::Dir,
        }
    }

    pub fn file(path: impl Into<PathBuf>) -> Protection {
        Protection {
            path: path.into(),
            kind: ProtectedKind::File,
        }
    }
}

/// The host room coordinator's files (`room-<host key>.sock`, `.lease`, `.restart`, `.token`) in
/// letibot's data directory, plus `$LETIBOT_ROOM_SOCKET` and its lease and token. Found by scanning,
/// so no host key is needed: a call that read the token could post as the operator.
pub fn room_files() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let data = std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .map(|d| d.join("letibot"));
    if let Some(dir) = data
        && let Ok(rd) = std::fs::read_dir(&dir)
    {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with("room-")
                && (name.ends_with(".sock")
                    || name.ends_with(".lease")
                    || name.ends_with(".restart")
                    || name.ends_with(".token"))
            {
                out.push(e.path());
            }
        }
    }
    if let Some(p) = std::env::var_os("LETIBOT_ROOM_SOCKET").filter(|v| !v.is_empty()) {
        let p = PathBuf::from(p);
        out.push(p.with_extension("lease"));
        out.push(p.with_extension("token"));
        out.push(p);
    }
    // Both spellings of each path: a symlinked prefix is two strings for one file.
    let canon: Vec<PathBuf> = out.iter().filter_map(|p| p.canonicalize().ok()).collect();
    out.extend(canon);
    out.sort();
    out.dedup();
    out
}

/// The daemon's control sockets a tight seat must not reach: every socket in the per-user `letibot`
/// runtime directories (the daemon's own, the ssh control masters) and `$LETIBOT_SOCKET`. The wide
/// seat keeps them, because `sudo -A` reaches the session through that socket.
pub fn daemon_sockets() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(d) = std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
        dirs.push(PathBuf::from(d).join("letibot"));
    }
    dirs.push(std::env::temp_dir().join("letibot"));
    // SAFETY: `getuid` cannot fail.
    dirs.push(PathBuf::from(format!("/run/user/{}/letibot", unsafe {
        libc::getuid()
    })));
    daemon_sockets_in(
        &dirs,
        &std::env::temp_dir(),
        std::env::var_os("LETIBOT_SOCKET")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from),
    )
}

/// [`daemon_sockets`] over given directories, so a test does not race the process environment.
fn daemon_sockets_in(dirs: &[PathBuf], tmp: &Path, named: Option<PathBuf>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for dir in dirs {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            if e.file_name().to_string_lossy().ends_with(".sock") {
                out.push(e.path());
            }
        }
    }
    // The ssh masters fall back to the temp dir when the runtime dir is too long for a socket path.
    for e in std::fs::read_dir(tmp).into_iter().flatten().flatten() {
        let n = e.file_name().to_string_lossy().to_string();
        if n.starts_with("letibot-ssh-") && n.ends_with(".sock") {
            out.push(e.path());
        }
    }
    out.extend(named);
    let canon: Vec<PathBuf> = out.iter().filter_map(|p| p.canonicalize().ok()).collect();
    out.extend(canon);
    out.sort();
    out.dedup();
    out
}

/// The read-only system paths bound by default, with `--ro-bind-try` so a host that does
/// not have one is not a failure. This is what `/bin/sh` needs to exist, not a security
/// decision: the security decision is everything not on it, where `$HOME` and `/root` live.
pub const SYSTEM_ROOTS: &[&str] = &[
    "/usr",
    "/bin",
    "/sbin",
    "/lib",
    "/lib32",
    "/lib64",
    "/libx32",
    "/etc/alternatives",
    "/etc/ld.so.cache",
    "/etc/ld.so.conf",
    "/etc/ld.so.conf.d",
    "/etc/localtime",
    "/etc/ssl",
    "/etc/ca-certificates",
    "/etc/ca-certificates.conf",
    "/etc/nsswitch.conf",
    "/etc/passwd",
    "/etc/group",
    "/etc/terminfo",
];

/// The resolver's files, bound only for a view that shares the host's network
/// ([`Egress::Host`]): with a network and without these, every name fails to resolve.
/// `nsswitch.conf` is already in [`SYSTEM_ROOTS`], because `getpwuid` reads it too.
pub const NETWORK_ROOTS: &[&str] = &[
    "/etc/resolv.conf",
    "/etc/hosts",
    "/etc/host.conf",
    "/etc/gai.conf",
];

/// The environment variables a confined command keeps. The environment is cleared and
/// these put back, which is absence beats denial applied to the environment: an unset list
/// would be wrong in both directions, a keep list only in the visible one.
pub const KEEP_ENV: &[&str] = &["PATH", "TERM", "LANG", "LC_ALL", "TZ"];

impl ViewSpec {
    /// The default view: one project, the system roots, a tmpfs home, no grants.
    pub fn project_only(project: impl Into<PathBuf>) -> ViewSpec {
        ViewSpec {
            project: project.into(),
            system: SYSTEM_ROOTS.iter().map(PathBuf::from).collect(),
            home: HomeView::Tmpfs,
            tmp: TmpView::PerCommand,
            grants: Vec::new(),
            hidden: Vec::new(),
            protected: Vec::new(),
            wide: false,
            masked: Vec::new(),
        }
    }

    /// The whole machine at its own paths, read-write, with the operator's home as `$HOME`. The
    /// caller sets `home` too: a wide view whose home is a throwaway would be the tight one.
    pub fn wide(mut self) -> ViewSpec {
        self.wide = true;
        self
    }

    /// Put `paths` behind an empty filesystem inside the view. Only a wide view renders them:
    /// anywhere else the operator's home is already outside the view.
    pub fn masking(mut self, paths: impl IntoIterator<Item = PathBuf>) -> ViewSpec {
        self.masked.extend(paths);
        self.masked.sort();
        self.masked.dedup();
        self
    }

    /// Keep this path read-only even inside a read-write grant.
    pub fn protecting(mut self, protection: Protection) -> ViewSpec {
        self.protected.push(protection);
        self
    }

    /// Keep this file out of the view and unconnectable even if a wider bind covers it.
    pub fn hiding(mut self, path: impl Into<PathBuf>) -> ViewSpec {
        self.hidden.push(path.into());
        self
    }

    pub fn with_home(mut self, home: HomeView) -> ViewSpec {
        self.home = home;
        self
    }

    /// Give this session a `/tmp` that outlives a command: see [`TmpView`].
    pub fn with_tmp(mut self, tmp: TmpView) -> ViewSpec {
        self.tmp = tmp;
        self
    }

    pub fn granting(mut self, grant: Grant) -> ViewSpec {
        self.grants.push(grant);
        self
    }

    /// What a host path is, from inside the view: three answers, not two, because a fresh
    /// tmpfs inside the view is not the host's `/tmp`. Lexical on purpose: a `stat` on the
    /// host would disclose what the operator has, and cannot answer about an absent path.
    pub fn classify(&self, path: &Path) -> Presence {
        if !path.is_absolute() {
            return Presence::Inside;
        }
        // A wide view is the machine itself: nothing is absent from it, and a masked path is
        // there and empty rather than outside.
        if self.wide {
            return Presence::Inside;
        }
        // The bound roots first, so a project under `/tmp` is Inside, not Replaced.
        let inside = std::iter::once(self.project.as_path())
            .chain(self.system.iter().map(|p| p.as_path()))
            .chain(self.grants.iter().map(|g| g.path()));
        for root in inside {
            if path == root || path.starts_with(root) {
                return Presence::Inside;
            }
            // The parent chain to a bound path exists inside as empty directories, so
            // `/home/dead` can be listed and calling it outside would be wrong.
            if root.starts_with(path) {
                return Presence::Inside;
            }
        }
        if let HomeView::Dir(d) = &self.home
            && (path == d || path.starts_with(d) || d.starts_with(path))
        {
            return Presence::Inside;
        }
        for (p, what) in REPLACED_ROOTS {
            let p = Path::new(p);
            if path == p || path.starts_with(p) || p.starts_with(path) {
                // A session's `/tmp` is still not the host's, and it is no longer emptied
                // between calls, so the sentence follows the view rather than the constant.
                let what = match &self.tmp {
                    TmpView::Session(_) if p == Path::new("/tmp") => SESSION_TMP_WHAT,
                    _ => what,
                };
                return Presence::Replaced { what };
            }
        }
        Presence::Outside
    }

    /// Is this absolute path outside the view entirely? A [`Presence::Replaced`] path is
    /// not outside: it exists and can be used, it is simply empty.
    pub fn outside(&self, path: &Path) -> bool {
        matches!(self.classify(path), Presence::Outside)
    }

    /// The roots a disclosure lists, in the order it lists them.
    pub fn summary(&self) -> String {
        if self.wide {
            return self.wide_summary();
        }
        let mut s = format!("project {} (rw)", self.project.display());
        s.push_str(&format!("; {} read-only system paths", self.system.len()));
        match &self.home {
            HomeView::Tmpfs => s.push_str(&format!(
                "; $HOME is a FRESH TMPFS at {TMPFS_HOME} (so a build cache under \
                 $HOME is empty every run)"
            )),
            HomeView::Dir(d) => s.push_str(&format!("; $HOME is {} (rw)", d.display())),
        }
        match &self.tmp {
            TmpView::PerCommand => {
                // "a new tmpfs", not "FRESH TMPFS": that phrase is the `$HOME` arm's, and a
                // disclosure is read to tell a throwaway home from a kept one.
                s.push_str(
                    "; /tmp is a new tmpfs for each command (so a file one call writes is gone \
                     in the next)",
                )
            }
            TmpView::Session(d) => s.push_str(&format!(
                "; /tmp is {} (rw), this session's own, so a file one call writes is there for \
                 the next",
                d.display()
            )),
        }
        if self.grants.is_empty() {
            s.push_str("; no grants");
        } else {
            for g in &self.grants {
                s.push_str(&format!("\n    grant: {}", g.line()));
            }
        }
        // A grant of a directory can carry a credential store with it, and this view does not: the
        // list is the one a wide view masks, and only what this view reaches is named.
        let masked: Vec<String> = self
            .masked
            .iter()
            .filter(|p| self.classify(p) == Presence::Inside)
            .map(|p| p.display().to_string())
            .collect();
        if !masked.is_empty() {
            s.push_str(&format!(
                "\n    masked inside the grants: {}",
                masked.join(", ")
            ));
        }
        s
    }

    /// [`ViewSpec::summary`] for a wide view, where the project is not a bound root because the
    /// whole machine is: the fence is the only thing worth listing.
    fn wide_summary(&self) -> String {
        let home = match &self.home {
            HomeView::Dir(d) => d.display().to_string(),
            // A wide view with a throwaway home is a caller's mistake, and a disclosure that
            // said nothing about it would be the place it went unnoticed.
            HomeView::Tmpfs => TMPFS_HOME.to_string(),
        };
        let mut s = format!(
            "the WHOLE host filesystem at its own paths, read-write as this user, so nothing is \
             absent from it; $HOME is {home} (rw); {} paths masked for read and write (an empty \
             filesystem where a store is a directory, an unreadable `/dev/null` where it is a file)",
            self.masked.len()
        );
        match &self.tmp {
            TmpView::PerCommand => s.push_str("; /tmp is this machine's own"),
            TmpView::Session(d) => s.push_str(&format!(
                "; the session's scratch is $TMPDIR, {} (rw)",
                d.display()
            )),
        }
        if !self.masked.is_empty() {
            let list = self
                .masked
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            s.push_str(&format!("\n    masked: {list}"));
        }
        if self.grants.is_empty() {
            s.push_str("; no grants");
        } else {
            for g in &self.grants {
                s.push_str(&format!("\n    grant: {}", g.line()));
            }
        }
        s
    }
}

/// The boundary that was measured, not the one that was asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Boundary {
    /// What did it, with its version. `bubblewrap 0.11.1`, not `namespaces`.
    pub mechanism: String,
    /// Which family of machinery the rest of this record is written in, so that a
    /// reader knows whether the namespace table or the property table is the answer.
    pub platform: Mechanism,
    pub view: ViewSpec,
    pub egress: Egress,
    /// One entry per [`Namespace::ALL`], read back from inside. Empty under
    /// [`Mechanism::Policy`], which has no namespaces to enter.
    pub ns: BTreeMap<Namespace, NsState>,
    /// One entry per [`SealKind::ALL`], functionally tested from inside.
    pub seals: BTreeMap<SealKind, Seal>,
    /// One entry per [`Prop::ALL`], measured from inside. Empty under
    /// [`Mechanism::Namespaces`], whose namespace table says the same things by name.
    pub props: BTreeMap<Prop, PropState>,
    pub measured_at: SystemTime,
}

impl Boundary {
    /// An all-namespace boundary with no property table, the shape bubblewrap's probe fills
    /// in. A constructor, so a new field does not mean touching every fixture that builds one.
    pub fn namespaces(
        mechanism: impl Into<String>,
        view: ViewSpec,
        egress: Egress,
        ns: BTreeMap<Namespace, NsState>,
        seals: BTreeMap<SealKind, Seal>,
    ) -> Boundary {
        Boundary {
            mechanism: mechanism.into(),
            platform: Mechanism::Namespaces,
            view,
            egress,
            ns,
            seals,
            props: BTreeMap::new(),
            measured_at: SystemTime::now(),
        }
    }

    /// Every required property of this boundary's own mechanism held, and no seal measured
    /// open: required means three namespaces on Linux, three properties under a policy. An
    /// untested seal makes the boundary partial; one measured open fails it.
    pub fn complete(&self) -> bool {
        let sealed = !self.seals.values().any(|s| matches!(s, Seal::Open { .. }));
        match self.platform {
            Mechanism::Namespaces => {
                Namespace::REQUIRED
                    .iter()
                    .all(|n| self.ns.get(n).map(|s| s.entered()).unwrap_or(false))
                    && sealed
            }
            Mechanism::Policy => {
                Prop::REQUIRED_POLICY
                    .iter()
                    .all(|p| self.props.get(p).map(|s| s.held()).unwrap_or(false))
                    && sealed
            }
        }
    }

    /// Is this a boundary a per-command ruling would buy nothing inside? Required namespaces
    /// or properties held, every seal held rather than merely not open (one this platform
    /// cannot have does not count against it), and egress denied.
    pub fn encloses(&self) -> bool {
        let inside = match self.platform {
            Mechanism::Namespaces => {
                Namespace::REQUIRED
                    .iter()
                    .all(|n| self.ns.get(n).map(|s| s.entered()).unwrap_or(false))
                    && SealKind::ALL
                        .iter()
                        .all(|k| self.seals.get(k).map(|s| s.held()).unwrap_or(false))
            }
            Mechanism::Policy => {
                Prop::REQUIRED_POLICY
                    .iter()
                    .all(|p| self.props.get(p).map(|s| s.held()).unwrap_or(false))
                    && SealKind::ALL.iter().all(|k| {
                        self.seals
                            .get(k)
                            .map(|s| s.held() || matches!(s, Seal::NotApplicable { .. }))
                            .unwrap_or(false)
                    })
            }
        };
        inside && self.egress == Egress::Denied
    }

    /// Something was asked for and did not happen, or could not be confirmed. Not the same as
    /// [`Boundary::complete`] being false: a partial boundary in a namespace that is not
    /// required still confines, and [`PropState::Absent`] or `Egress::Host` is a decision.
    pub fn partial(&self) -> bool {
        self.ns
            .values()
            .any(|s| matches!(s, NsState::NotEntered { .. }))
            || self
                .props
                .values()
                .any(|s| matches!(s, PropState::Unverified { .. }))
            || self
                .seals
                .values()
                .any(|s| matches!(s, Seal::Unverified { .. }))
    }

    /// The namespaces that were asked for and are not there, with why.
    pub fn missing(&self) -> Vec<(Namespace, String)> {
        self.ns
            .iter()
            .filter_map(|(n, s)| match s {
                NsState::NotEntered { why } => Some((*n, why.clone())),
                _ => None,
            })
            .collect()
    }

    /// The disclosure, read off the map: every line comes from `self.ns` (or `self.props`),
    /// so a boundary that lost its PID namespace says so and a hard-coded `confined` cannot
    /// outlive the confinement.
    pub fn describe(&self) -> String {
        let mut s = String::new();
        let head = if self.complete() && !self.partial() {
            "confined"
        } else if self.complete() {
            "confined, PARTIALLY — something that was asked for is missing or could not be confirmed"
        } else if self.platform == Mechanism::Policy {
            "NOT CONFINED — a required property is not held or a seal is open; a command here is refused"
        } else {
            "NOT CONFINED — a required namespace is missing or a seal is open; a command here is refused"
        };
        s.push_str(&format!("{head}; by {}", self.mechanism));
        for k in SealKind::ALL {
            let seal = self.seals.get(&k).cloned().unwrap_or(Seal::Unverified {
                why: "never tested".into(),
            });
            s.push_str(&format!("\n  seal {k}: {} — {}", seal.line(), k.buys()));
        }
        match self.platform {
            // The namespace table, by name: the whole answer on Linux.
            Mechanism::Namespaces => {
                for n in Namespace::ALL {
                    let state = self.ns.get(&n).cloned().unwrap_or(NsState::NotEntered {
                        why: "never measured".into(),
                    });
                    let req = if Namespace::REQUIRED.contains(&n) {
                        " [required]"
                    } else {
                        ""
                    };
                    s.push_str(&format!("\n  {n}{req}: {} — {}", state.line(), n.buys()));
                }
            }
            // The property table, which is what a mechanism without namespaces says
            // instead, including the properties it cannot have.
            Mechanism::Policy => {
                for p in Prop::ALL {
                    let state = self
                        .props
                        .get(&p)
                        .cloned()
                        .unwrap_or(PropState::Unverified {
                            why: "never measured".into(),
                        });
                    let req = if Prop::REQUIRED_POLICY.contains(&p) {
                        " [required]"
                    } else {
                        ""
                    };
                    s.push_str(&format!("\n  {p}{req}: {} — {}", state.line(), p.buys()));
                }
            }
        }
        s.push_str(&format!("\n  view: {}", self.view.summary()));
        s.push_str(&format!(
            "\n  egress: {}",
            match &self.egress {
                Egress::Denied => "DENIED — no interfaces, no routes, no DNS".to_string(),
                Egress::Host { why } => format!("the host's network, by decision — {why}"),
            }
        ));
        s
    }

    /// The view told to the MODEL, before anything fails: what a command can reach, that `~` is
    /// the view's own home, and what a path outside it is. A model with only the failure note
    /// reads a hermetic view as a broken box.
    pub fn model_brief(&self) -> String {
        if self.view.wide {
            return self.wide_brief();
        }
        let v = &self.view;
        // Top-level roots only: `/etc/ssl` and `/etc/ld.so.cache` are one answer to "what is
        // under /etc", and the full table is 19 lines of prompt bytes for one fact.
        let mut roots: Vec<String> = Vec::new();
        for p in &v.system {
            let Some(top) = p.components().nth(1) else {
                continue;
            };
            let name = format!("/{}", top.as_os_str().to_string_lossy());
            if !roots.contains(&name) {
                roots.push(name);
            }
        }
        let home = match &v.home {
            HomeView::Tmpfs => format!(
                "`{TMPFS_HOME}`, a home this view makes for itself and not this machine's, so a \
                 build cache kept under `~` is empty when a command starts"
            ),
            // The wide seat's home IS the operator's: a brief calling it letibot's own would tell
            // a model that `~` is scratch, which is a claim about somebody else's files.
            HomeView::Dir(d) if is_the_operator_home(d) => format!(
                "`{}`, THIS MACHINE'S OWN HOME: the operator's real home, writable, so its files \
                 are theirs and not this session's to tidy, and the deny list below is the whole \
                 of what is fenced off in it",
                d.display()
            ),
            HomeView::Dir(d) => format!(
                "`{}`, a directory letibot owns rather than this machine's home",
                d.display()
            ),
        };
        let tmp = match &v.tmp {
            TmpView::PerCommand => {
                "a fresh tmpfs for each command, so a file one call writes is gone in the next"
                    .to_string()
            }
            TmpView::Session(_) => "this session's own directory, so a file one call writes is \
                                    there for the next"
                .to_string(),
        };
        let mut s = format!(
            "Commands here run inside a filesystem view, not on this machine's disk: `{}` is \
             writable at its own path; the {} read-only system paths a shell needs are bound \
             ({}); `$HOME` is {home}; `/tmp` is {tmp}.",
            v.project.display(),
            v.system.len(),
            roots.join(", "),
        );
        s.push_str(&match &self.egress {
            Egress::Denied => {
                " There is no network either: no interfaces, no routes, no DNS.".to_string()
            }
            Egress::Host { why } => {
                format!(" The network is this machine's, by a decision — {why}.")
            }
        });
        s.push_str(match self.platform {
            Mechanism::Namespaces => {
                " Anything else is absent from the view rather than forbidden: a command that \
                 fails on a path outside it has hit the boundary, and no other spelling of the \
                 path reaches further."
            }
            Mechanism::Policy => {
                " Anything else is denied by the policy: a command that fails on a path outside \
                 the view has hit the boundary, and no other spelling of the path reaches \
                 further."
            }
        });
        s.push_str(
            " `ReadOnly` on such a path is the grant that binds it in, and its contents can then \
             reach the transcript; `ReadWrite` is the same and writable. Whether one is given is \
             the operator's to decide, and the failing call raises the question.",
        );
        if !v.grants.is_empty() {
            s.push_str(" Bound in beyond that already:");
            for g in &v.grants {
                s.push_str(&format!(" {}.", g.line()));
            }
        }
        // A granted directory can carry a credential store, and a readable store is the thing this
        // boundary exists to keep out of the transcript, so a model is told which names are empty.
        let masked: Vec<String> = v
            .masked
            .iter()
            .filter(|p| v.classify(p) == Presence::Inside)
            .map(|p| format!("`{}`", p.display()))
            .collect();
        if !masked.is_empty() {
            s.push_str(&format!(
                " Inside those, {} {} masked and holds nothing: a grant covers the directory, and \
                 these names are not readable through it.",
                masked.join(", "),
                if masked.len() == 1 { "is" } else { "are" }
            ));
        }
        s
    }

    /// [`Boundary::model_brief`] for a wide view: the machine is not a view of some of its files,
    /// so what a model has to know is that everything is here and what the mask is.
    fn wide_brief(&self) -> String {
        let v = &self.view;
        let home = match &v.home {
            HomeView::Dir(d) => format!("`{}`, this machine's own home, writable", d.display()),
            HomeView::Tmpfs => format!("`{TMPFS_HOME}`, a home this view makes for itself"),
        };
        let tmp = match &v.tmp {
            TmpView::PerCommand => "this machine's own".to_string(),
            TmpView::Session(d) => {
                format!(
                    "`{}`, this session's scratch, through `$TMPDIR`",
                    d.display()
                )
            }
        };
        let mut s = format!(
            "Commands here run inside a boundary AROUND this machine, not in a view of part of \
             it: every path is at its own place, writable as this user, and nothing is absent. \
             `$HOME` is {home}; `/tmp` is {tmp}. The fence is a mask: {} of the operator's \
             credential stores are an empty filesystem inside the boundary, or an unreadable \
             `/dev/null` where a store is a file, so a name like `~/.ssh` is there and holds \
             nothing.",
            v.masked.len()
        );
        s.push_str(&match &self.egress {
            Egress::Denied => {
                " There is no network either: no interfaces, no routes, no DNS.".to_string()
            }
            Egress::Host { why } => {
                format!(" The network is this machine's, by a decision — {why}.")
            }
        });
        s.push_str(
            " Nothing is outside this boundary: a path that fails here failed on its own \
             permissions or because it is not there, so a missing file really is missing. What \
             that costs is the same as anywhere: what a command reads it can copy into the \
             transcript, and the masked stores are the one thing the boundary hides.",
        );
        s
    }

    /// The paths this command could not see, as paths rather than as prose: the same finding
    /// [`Boundary::absence_notes`] gives in a sentence, for a caller that raises a grant
    /// decision from it instead of only reporting it.
    pub fn outside_paths(&self, output: &str) -> Vec<PathBuf> {
        let mut outside: Vec<PathBuf> = Vec::new();
        for line in output.lines() {
            if !self.boundary_refusal(line) {
                continue;
            }
            // A "not there" line may come from a container (`-v /tmp:/hosttmp`), so its path counts
            // only if this host has it; a denial already proves the path exists. A glob never counts.
            let absent = line_says_absent(line);
            for tok in absolute_paths(line) {
                if matches!(self.view.classify(&tok), Presence::Outside)
                    && !has_glob(&tok)
                    && (!absent || std::fs::symlink_metadata(&tok).is_ok())
                    && !outside.contains(&tok)
                {
                    outside.push(tok);
                }
            }
            // The same fold [`Boundary::absence_notes`] makes: a missing program names no path,
            // and the note it produces promises a grant, so the grant has to be asked about.
            if let Some(p) = self.missing_program(line)
                && !outside.contains(&p)
            {
                outside.push(p);
            }
            if outside.len() >= MAX_ABSENCE_PATHS {
                break;
            }
        }
        outside.truncate(MAX_ABSENCE_PATHS);
        outside
    }

    /// The paths a command could not WRITE although they are inside the view: a path bound in
    /// read-only is absent from nowhere, so the only sign of it is the refusal, and the operator
    /// is still owed the question (`/build` granted read-only, then a write under it).
    pub fn refused_write_paths(&self, output: &str) -> Vec<PathBuf> {
        let mut refused: Vec<PathBuf> = Vec::new();
        let read_only_grant = |p: &Path| {
            self.view.grants.iter().any(|g| match g {
                Grant::ReadOnly { path, .. } => p.starts_with(path),
                _ => false,
            })
        };
        let wide_open = |p: &Path| {
            p.starts_with(&self.view.project)
                || self.view.grants.iter().any(|g| match g {
                    Grant::ReadWrite { path, .. } => p.starts_with(path),
                    _ => false,
                })
        };
        for line in output.lines() {
            // `EROFS` under a namespace boundary, `EPERM` under a policy one: a path bound
            // read-only refuses a write differently on each platform.
            if !line.contains("Read-only file system")
                && !line.contains("EROFS")
                && !line.contains("Operation not permitted")
            {
                continue;
            }
            for tok in absolute_paths(line) {
                if read_only_grant(&tok)
                    && !wide_open(&tok)
                    && !has_glob(&tok)
                    && std::fs::symlink_metadata(&tok).is_ok()
                    && !refused.contains(&tok)
                {
                    refused.push(tok);
                }
            }
            if refused.len() >= MAX_ABSENCE_PATHS {
                break;
            }
        }
        refused.truncate(MAX_ABSENCE_PATHS);
        refused
    }

    /// The host path a missing-program line is about, when this host has that program and the
    /// view does not. A name that resolves nowhere is a typo, and a typo must not become a
    /// claim about the boundary.
    fn missing_program(&self, line: &str) -> Option<PathBuf> {
        let name = missing_program_name(line)?;
        let dirs = std::env::var_os("PATH")?;
        let hit = std::env::split_paths(&dirs)
            .map(|d| d.join(name))
            .find(|p| p.is_file())?;
        self.view.outside(&hit).then_some(hit)
    }

    /// The words in a command's output that mean the boundary refused this: `ENOENT` for an
    /// absent path, `EPERM` under a policy, and a shell's missing-binary words on both,
    /// because a note written for one mechanism says nothing on the other.
    fn boundary_refusal(&self, line: &str) -> bool {
        let absent = line.contains("No such file or directory")
            || line.contains("ENOENT")
            || line.contains("not found")
            || line.contains("cannot find");
        match self.platform {
            Mechanism::Namespaces => absent,
            Mechanism::Policy => {
                absent
                    || line.contains("Operation not permitted")
                    || line.contains("Permission denied")
            }
        }
    }

    /// What the boundary does to a path outside the view, in the words the note uses. One
    /// sentence per mechanism: a mount namespace does not contain the path, a policy denies
    /// it, and the policy's version says how to tell that from a file permission.
    fn refusal_clause(&self) -> &'static str {
        match self.platform {
            Mechanism::Namespaces => {
                "a path list is not what is stopping it — the mount namespace does not \
                 contain it"
            }
            Mechanism::Policy => {
                "the policy denies the path rather than hiding it, so this `Operation not \
                 permitted` is the boundary and not a file permission in the checkout"
            }
        }
    }

    /// Turn a bare `ENOENT` into "not in this session's filesystem view": a model that reads
    /// one concludes the file does not exist and reasons from an empty haystack. Bounded,
    /// because a miss path must not produce more output than a hit.
    pub fn absence_notes(&self, output: &str) -> Vec<String> {
        let mut outside: Vec<PathBuf> = Vec::new();
        let mut replaced: Vec<(PathBuf, &'static str)> = Vec::new();
        for line in output.lines() {
            if !self.boundary_refusal(line) {
                continue;
            }
            for tok in absolute_paths(line) {
                match self.view.classify(&tok) {
                    Presence::Outside if !outside.contains(&tok) => outside.push(tok),
                    Presence::Replaced { what } if !replaced.iter().any(|(p, _)| *p == tok) => {
                        replaced.push((tok, what))
                    }
                    _ => {}
                }
            }
            // A missing program names no path, so the scan above finds nothing in it, and a
            // toolchain the view has not got is the one absence read as a broken box.
            if let Some(p) = self.missing_program(line)
                && !outside.contains(&p)
            {
                outside.push(p);
            }
            if outside.len() >= MAX_ABSENCE_PATHS && replaced.len() >= MAX_ABSENCE_PATHS {
                break;
            }
        }
        outside.truncate(MAX_ABSENCE_PATHS);
        replaced.truncate(MAX_ABSENCE_PATHS);

        let mut notes = Vec::new();
        if !outside.is_empty() {
            let list = outside
                .iter()
                .map(|p| format!("`{}`", p.display()))
                .collect::<Vec<_>>()
                .join(", ");
            let mut note = format!(
                "{list} {} not in this session's filesystem view, so the command saw \
                 {} as absent — an absence the boundary makes, not an answer about whether \
                 anything is there. This session's view is {}; no other spelling of the path \
                 reaches past it, and {}. The operator is being asked to grant it in, and \
                 re-running before they answer will not change it.",
                if outside.len() == 1 { "is" } else { "are" },
                if outside.len() == 1 { "it" } else { "them" },
                self.view.summary().lines().next().unwrap_or("the project"),
                self.refusal_clause(),
            );
            // The grant is the only thing that changes the answer, so the note names both
            // spellings the card offers and the consequence the operator weighs: a path readable
            // in the view is readable into the transcript.
            for p in &outside {
                note.push_str(&format!(
                    " A grant would change that for `{}`: `ReadOnly` binds the path into the \
                     view, and its contents can then reach the transcript; `ReadWrite` is the \
                     same and writable too. Which one it is, and whether, is the operator's to \
                     decide.",
                    p.display()
                ));
            }
            // Once for the list, so the reply stays as short as a hit: the first path that can
            // be granted stands for the shape of the key.
            let home = std::env::var_os("HOME").map(PathBuf::from);
            let mut keyed = false;
            for p in &outside {
                let hint = key_hint_under(p, home.as_deref());
                if hint.contains("never granted") || !keyed {
                    note.push_str(&hint);
                    keyed |= !hint.contains("never granted");
                }
            }
            notes.push(note);
        }
        if !replaced.is_empty() {
            let list = replaced
                .iter()
                .map(|(p, what)| format!("`{}` ({what})", p.display()))
                .collect::<Vec<_>>()
                .join("; ");
            notes.push(format!(
                "{list} — {} inside this session's filesystem view, but {} not the \
                 host's, so anything the host keeps there is absent and anything \
                 written there does not reach it. This `ENOENT` is the boundary too, \
                 in a different way from a path that is simply outside: {}",
                if replaced.len() == 1 {
                    "this is"
                } else {
                    "these are"
                },
                if replaced.len() == 1 {
                    "it is"
                } else {
                    "they are"
                },
                // The session's `/tmp` is the one replaced root that does not start empty, so
                // saying it does would be a note about a file the model itself wrote.
                if replaced.iter().any(|(_, what)| *what == SESSION_TMP_WHAT) {
                    "the directory exists and can be used, and what it holds is this \
                     session's own, written by an earlier call and not by the host."
                } else {
                    "the directory exists and can be used, it just starts empty."
                },
            ));
        }
        notes
    }
}

/// At most this many paths in one absence note.
const MAX_ABSENCE_PATHS: usize = 5;

/// Is this path the operator's own home? Compared both ways, because a view's home is
/// canonicalised by the probe and `$HOME` need not be: a symlinked home is one directory.
fn is_the_operator_home(d: &Path) -> bool {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return false;
    };
    if home == d {
        return true;
    }
    match (home.canonicalize(), d.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Absolute-path-shaped tokens in a line of command output. Crude and bounded on purpose: it
/// feeds a note, not a decision, so a false positive costs a sentence. Trailing punctuation a
/// diagnostic adds is stripped, which is what would otherwise make every path miss.
fn absolute_paths(line: &str) -> Vec<PathBuf> {
    line.split(|c: char| c.is_whitespace())
        .filter_map(|t| {
            let t = t.trim_matches(|c: char| "\"'`(),;:".contains(c));
            if t.len() > 1 && t.starts_with('/') {
                Some(PathBuf::from(t))
            } else {
                None
            }
        })
        .collect()
}

/// Whether `p` still holds a glob the shell did not expand.
fn has_glob(p: &std::path::Path) -> bool {
    p.to_string_lossy().contains(['*', '?', '['])
}

/// Whether a line says a path is not there, as opposed to denied.
fn line_says_absent(line: &str) -> bool {
    line.contains("No such file or directory")
        || line.contains("ENOENT")
        || line.contains("not found")
        || line.contains("cannot find")
}

/// The program name in a shell's missing-binary line: `cargo: command not found`, and the same
/// line behind the shell's own prefix (`/bin/sh: 1: cargo: not found`). A name carrying a `/`
/// is a path, and [`absolute_paths`] has those.
fn missing_program_name(line: &str) -> Option<&str> {
    let line = line.trim_end();
    let before = line
        .strip_suffix("command not found")
        .or_else(|| line.strip_suffix("not found"))?;
    let name = before.trim_end().strip_suffix(':')?.trim_end();
    let name = name.rsplit(": ").next().unwrap_or(name);
    (!name.is_empty() && !name.contains('/') && !name.contains(char::is_whitespace)).then_some(name)
}

/// What a spawn needs from the confinement, so that the confinement decides how
/// to express it rather than the caller guessing.
#[derive(Debug, Clone)]
pub struct ConfinePlan<'a> {
    /// Where the command runs. An absolute host path inside the view.
    pub cwd: &'a Path,
    /// The environment additions the caller asked for. Under `--clearenv` they have to be
    /// re-declared inside, so they come through here rather than through `Command::env`.
    pub env: &'a [(String, String)],
}

/// Whether the wrapped command gets a session of its own: a job or a local command is
/// detached so `TIOCSTI` cannot push characters into the operator's shell, while a pane's pty
/// is its own and a remote command's process group is what an interrupt kills.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Session {
    Detached,
    Kept,
}

/// One element of a command line, kept typed until it is joined: a local spawn is an argv and
/// a remote one is a shell script, and a second spelling of the binds would drift. A literal
/// `$PATH` would put five characters in the environment where an expansion belongs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    /// An ordinary argument: quoted for whichever shell joins it.
    Literal(String),
    /// A fragment the far shell must expand, such as the keep-list's `"$PATH"`. Already
    /// spelled for the shell it is going into, so a joiner passes it through.
    Shell(String),
}

impl Piece {
    fn literal(s: impl Into<String>) -> Piece {
        Piece::Literal(s.into())
    }
}

/// Where the keep-list's values come from. [`KEEP_ENV`] keeps names, not values, and the
/// values differ across an ssh connection: this box's `PATH` names directories that do not
/// exist in a Linux sandbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvSource {
    /// This process's own environment: a command this box spawns.
    Process,
    /// The far shell's, expanded there just before the helper runs.
    Remote,
}

/// The descriptor a confined command's secret `--setenv` pairs are read from (bwrap's `--args`).
/// Fixed, so the argv builder stays pure; [`hand_secrets`] puts the pipe there in the child.
pub const SECRET_FD: i32 = 200;

/// The secret pairs `env` holds, as the NUL-separated `--setenv` arguments bwrap reads from
/// [`SECRET_FD`]; `None` when there are none, and then the argv asks for no descriptor. The bytes
/// are what a local spawn writes into a pipe and what a remote one feeds the ssh session's stdin.
pub fn secret_args(env: &[(String, String)]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    for (k, v) in env {
        if super::boundary::is_secret_name(k) {
            for part in ["--setenv", k.as_str(), v.as_str()] {
                out.extend_from_slice(part.as_bytes());
                out.push(0);
            }
        }
    }
    (!out.is_empty()).then_some(out)
}

/// Give `cmd` the secret pairs of `env` on [`SECRET_FD`], for a bwrap argv that asks for them.
/// The pipe is close-on-exec and moved to the fixed number only in the child, so a command
/// another thread spawns meanwhile does not inherit it. Keep the answer until `spawn` returns.
#[cfg(target_os = "linux")]
pub fn hand_secrets(
    cmd: &mut std::process::Command,
    env: &[(String, String)],
) -> std::io::Result<Option<std::os::fd::OwnedFd>> {
    use std::io::Write;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::process::CommandExt;
    let Some(bytes) = secret_args(env) else {
        return Ok(None);
    };
    let mut fds = [0i32; 2];
    // SAFETY: `pipe2` writes two descriptors into `fds` on success, and both are owned below.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: both descriptors were just created and nothing else owns them.
    let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    // Secrets are a few hundred bytes, under any pipe's buffer, so this cannot block.
    std::fs::File::from(write).write_all(&bytes)?;
    let raw = read.as_raw_fd();
    // SAFETY: `dup2` is async-signal-safe, and the new descriptor drops close-on-exec.
    unsafe {
        cmd.pre_exec(move || {
            if libc::dup2(raw, SECRET_FD) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(Some(read))
}

/// Join pieces into a POSIX shell command line: literals quoted, shell fragments as written.
pub fn join(pieces: &[Piece]) -> String {
    pieces
        .iter()
        .map(|p| match p {
            Piece::Literal(s) => crate::remote::quote(s),
            Piece::Shell(s) => s.clone(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The seam, and the mirror of [`super::ScopeTree`]: lifetime is one mechanism,
/// view is another, and a backend can have either without the other.
pub trait Confinement: Send + Sync {
    /// One line or several for `EXPLAIN` and for the head. It must say what it does not
    /// confine, because a disclosure that lists only the guarantees reads as a claim about
    /// the rest.
    fn describe(&self) -> String;

    /// What was measured, if anything was.
    fn boundary(&self) -> Option<&Boundary>;

    /// The argv that goes in front of the shell, or the refusal. An empty vector means nothing
    /// wraps this; only [`Unconfined`] returns it, and [`Unconfined::describe`] says so.
    fn wrap(&self, plan: &ConfinePlan<'_>) -> Result<Vec<String>, ExecError>;

    /// [`Confinement::wrap`] as typed pieces, for a caller whose command is a shell script
    /// rather than an argv. The default is `wrap` with every element a [`Piece::Literal`].
    fn wrap_pieces(
        &self,
        plan: &ConfinePlan<'_>,
        _env: EnvSource,
    ) -> Result<Vec<Piece>, ExecError> {
        Ok(self.wrap(plan)?.into_iter().map(Piece::Literal).collect())
    }

    /// Whether the operator's own `!` line runs outside this boundary. It is their console, with
    /// their rights and the daemon socket `sudo -A` needs; the boundary is for the model's commands.
    fn spares_operator(&self) -> bool {
        false
    }

    /// Is this path outside the view? `false` when there is no view, because
    /// "outside" is meaningless then and answering `true` would produce a note
    /// claiming a boundary that is not there.
    fn outside_view(&self, _path: &Path) -> bool {
        false
    }

    /// The absence notes for one command's output: a bare `ENOENT` is read as "the file does
    /// not exist", and that is a wrong answer about the world.
    fn absence_notes(&self, _output: &str) -> Vec<String> {
        Vec::new()
    }

    /// The same finding as paths, for a caller that can act on it rather than only report it:
    /// the runtime raises a grant decision from these. Empty where there is no view, because
    /// outside is meaningless then.
    fn outside_paths(&self, _output: &str) -> Vec<PathBuf> {
        Vec::new()
    }

    /// The paths a command could not WRITE although they are inside the view: a read-only grant,
    /// whose refusal is the only sign there is one. Empty where there is no view.
    fn refused_write_paths(&self, _output: &str) -> Vec<PathBuf> {
        Vec::new()
    }

    /// Did the LAUNCHER fail, rather than the command? A boundary that failed to set up
    /// produces an exit code that looks like a command's, and reporting that as the answer
    /// tells the model something false. bubblewrap's codes are not ours, so this reads stderr.
    fn launcher_failure(&self, _output: &str) -> Option<String> {
        None
    }

    /// [`Confinement::wrap`] for a program that keeps a terminal of its own, a `!term` pane:
    /// without its controlling terminal a screen program loses `/dev/tty`, job control and
    /// `SIGWINCH`, and what `TIOCSTI` could reach is the program's own input.
    fn wrap_terminal(&self, plan: &ConfinePlan<'_>) -> Result<Vec<String>, ExecError> {
        self.wrap(plan)
    }
}

/// Confinement was asked for and is not available: every spawn refuses. The default refuses
/// and says why. What this must never do is degrade into [`Unconfined`], and it cannot,
/// because no constructor, method or flag on it yields a runnable command.
#[derive(Debug, Clone)]
pub struct NoConfinement {
    pub why: String,
}

impl NoConfinement {
    pub fn new(why: impl Into<String>) -> Self {
        NoConfinement { why: why.into() }
    }
}

impl Confinement for NoConfinement {
    fn describe(&self) -> String {
        format!(
            "NONE, and this session refuses to exec because of it — {}",
            self.why
        )
    }
    fn boundary(&self) -> Option<&Boundary> {
        None
    }
    fn wrap(&self, _p: &ConfinePlan<'_>) -> Result<Vec<String>, ExecError> {
        Err(ExecError::NoConfinement(self.why.clone()))
    }
}

/// Confinement was not asked for. Not a fallback, because nothing constructs it in response
/// to a failure: it is what an unconfined backend has always had, named, so that not asked
/// for and asked for and missing are two types rather than a comment.
#[derive(Debug, Clone)]
pub struct Unconfined {
    pub why: String,
}

impl Unconfined {
    pub fn because(why: impl Into<String>) -> Self {
        Unconfined { why: why.into() }
    }
}

impl Confinement for Unconfined {
    fn describe(&self) -> String {
        format!(
            "NOT CONFINED — no namespaces: a command here reads this user's whole \
             filesystem, sees every process, and reaches the network. {}",
            self.why
        )
    }
    fn boundary(&self) -> Option<&Boundary> {
        None
    }
    fn wrap(&self, _p: &ConfinePlan<'_>) -> Result<Vec<String>, ExecError> {
        Ok(Vec::new())
    }
}

/// The bubblewrap boundary, measured at construction.
#[derive(Debug, Clone)]
pub struct Bwrap {
    helper: PathBuf,
    boundary: Boundary,
}

/// The environment variable that overrides where `bwrap` is looked for. Point it at a path
/// that is not there to exercise the refusal.
pub const BWRAP_ENV: &str = "LETIBOT_BWRAP";

impl Bwrap {
    /// Find the helper, build the real invocation, run it, and read back which namespaces it
    /// actually entered. Refuses at four points, each naming what was missing: no helper, a
    /// helper that would not run, an unparseable readback, a required namespace not entered.
    pub fn probe(view: ViewSpec, egress: Egress) -> Result<Bwrap, ExecError> {
        // No bubblewrap on macOS, and no installing one: it is a front end to Linux
        // namespaces, which Darwin does not have. Said by name, with what a Mac does have,
        // rather than through the Linux sentence and its `apt install`.
        #[cfg(target_os = "macos")]
        {
            let _ = (&view, egress);
            return Err(ExecError::NoConfinement(
                "macOS has no namespace boundary: bubblewrap is a front end to Linux \
                 namespaces and cannot be installed here. A Mac's boundary is a policy one \
                 (`super::seatbelt::Seatbelt`, `sandbox-exec -f <profile>`), which \
                 `confine::probe` chooses on this platform; ask for that rather than for \
                 this. Nothing was run, and nothing was run unconfined."
                    .to_string(),
            ));
        }
        #[allow(unreachable_code)]
        let helper = find_helper()?;
        let mechanism = version_of(&helper);
        let view = prepared(view)?;

        // The harness's own namespace inodes first: they are the denominator, and without
        // them an inode read from inside proves nothing.
        let mine = own_namespaces();

        let argv = build_argv(
            &helper,
            &view,
            &egress,
            &ConfinePlan {
                cwd: &view.project,
                env: &[],
            },
        );
        let probe = READBACK;
        let out = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .arg("/bin/sh")
            .arg("-c")
            .arg(probe)
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| {
                ExecError::NoConfinement(format!(
                    "`{}` could not be run ({e}), so nothing was measured and no \
                     command will be run confined",
                    helper.display()
                ))
            })?;
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        if !out.status.success() && text.is_empty() {
            return Err(ExecError::NoConfinement(format!(
                "`{}` exited {} without entering anything. Its own words: {}",
                helper.display(),
                out.status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "on a signal".into()),
                if stderr.trim().is_empty() {
                    "(it said nothing)".to_string()
                } else {
                    stderr.trim().to_string()
                }
            )));
        }

        let inside = parse_readback(&text);
        if inside.is_empty() {
            return Err(ExecError::NoConfinement(format!(
                "`{}` ran but the namespace readback could not be parsed, so what it \
                 entered is unknown — and an unknown boundary is treated as no \
                 boundary. It printed: {}",
                helper.display(),
                snippet(&text)
            )));
        }

        let mut ns = BTreeMap::new();
        for n in Namespace::ALL {
            ns.insert(
                n,
                state_of(
                    n,
                    inside.get(n.proc_name()),
                    mine.get(n.proc_name()),
                    &egress,
                ),
            );
        }

        let boundary = Boundary::namespaces(mechanism, view, egress, ns, parse_seals(&text));
        judged(helper, boundary)
    }

    /// The default confinement for a workspace root: project-only view, tmpfs
    /// `$HOME`, egress denied.
    pub fn project(root: impl Into<PathBuf>) -> Result<Bwrap, ExecError> {
        Bwrap::probe(ViewSpec::project_only(root), Egress::Denied)
    }

    /// A boundary measured on ANOTHER machine, out of what [`probe_script`] printed there: the
    /// helper and the namespaces are that kernel's and the denominator is that box's own
    /// inodes, so the script prints both. The view's paths are checked at spawn, not here.
    pub fn measured_on_a_host(helper: PathBuf, boundary: Boundary) -> Result<Bwrap, ExecError> {
        judged(helper, boundary)
    }
}

/// One namespace's state, from the inode read inside, the denominator read outside, and what
/// the egress decision says. Shared by the local probe and the remote one so the two cannot
/// disagree about what entered means.
fn state_of(
    n: Namespace,
    inside: Option<&String>,
    mine: Option<&String>,
    egress: &Egress,
) -> NsState {
    let want_shared = n == Namespace::Net && egress.allowed();
    match inside {
        None => NsState::NotEntered {
            why: format!(
                "`readlink /proc/self/ns/{}` printed nothing from inside, so \
                 this namespace could not be confirmed and is treated as absent",
                n.proc_name()
            ),
        },
        Some(inode) if Some(inode) == mine => {
            if want_shared {
                NsState::SharedByDecision {
                    why: match egress {
                        Egress::Host { why } => why.clone(),
                        Egress::Denied => "unreachable".into(),
                    },
                }
            } else {
                NsState::NotEntered {
                    why: format!(
                        "the inode inside ({inode}) is the harness's own, so \
                         `--unshare-{}` did not take effect however the \
                         arguments read",
                        unshare_flag(n)
                    ),
                }
            }
        }
        Some(inode) => NsState::Entered {
            inode: inode.clone(),
        },
    }
}

/// The three refusals a measurement has to survive, on both sides of an ssh connection: a
/// seal measured open, a required namespace missing, and the boundary that is left.
fn judged(helper: PathBuf, boundary: Boundary) -> Result<Bwrap, ExecError> {
    // A seal measured open refuses rather than describing itself as partial: "partial"
    // implies the part that is there still holds, and this is the part that makes the others
    // hold.
    if let Some((k, why)) = boundary.seals.iter().find_map(|(k, s)| match s {
        Seal::Open { why } => Some((*k, why.clone())),
        _ => None,
    }) {
        return Err(ExecError::NoConfinement(format!(
            "the namespaces were entered but the boundary is not sealed: {k} is \
             OPEN — {why}. A process inside could step back out of the view its \
             parent built, which makes every namespace above it decoration. \
             `--disable-userns`/`--assert-userns-disabled` were passed to \
             `{}` and did not take. What was measured: {}",
            helper.display(),
            boundary.describe()
        )));
    }

    // Fail closed on the three that are the boundary. A partial boundary in a non-required
    // namespace is kept and described as partial; a missing required one is not a boundary at
    // all, and calling it confined is the defect this module is written against.
    if !boundary.complete() {
        let missing = Namespace::REQUIRED
            .iter()
            .filter(|n| !boundary.ns.get(n).map(|s| s.entered()).unwrap_or(false))
            .map(|n| format!("{n} ({})", n.buys()))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(ExecError::NoConfinement(format!(
            "the helper ran but did not enter every required namespace: {missing}. \
             Measured, not assumed — the inode read from inside matched the \
             shell's own. On a host with \
             `kernel.apparmor_restrict_unprivileged_userns=1` this is what a \
             direct `unshare` looks like; `/usr/bin/bwrap` needs its \
             `bwrap-userns-restrict` AppArmor profile loaded. What was measured: \
             {}",
            boundary.describe()
        )));
    }
    Ok(Bwrap { helper, boundary })
}

/// The config key that would put `p` in a view, or the sentence saying none can: the fixed
/// never-grant list is the credential stores and the letibot store, under any home.
fn key_hint_under(p: &Path, home: Option<&Path>) -> String {
    let fixed = home.and_then(|home| {
        super::boundary::never_grant()
            .into_iter()
            .find(|rel| p.starts_with(home.join(rel)))
    });
    match fixed {
        Some(rel) => format!(
            " `~/{rel}` is never granted: no `[boundary]` key in leticode.toml changes that."
        ),
        None => {
            " The operator's keys: `[boundary] grants_ro = [\"PATH\"]`, or `grants_rw` for write, \
             in `~/.config/letibot/leticode.toml`."
                .to_string()
        }
    }
}

/// What a probe of another machine found, before any view is applied to it. The parts are
/// kept apart because they answer different questions: can this box confine at all, and what
/// is there to put in the view.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostProbe {
    /// Where the helper is there, as that shell spelled it. Empty means it is not there.
    pub helper: String,
    /// `bwrap --version`, first line.
    pub version: String,
    /// That machine's `$HOME`, from its own shell.
    pub home: String,
    /// The toolchain paths that exist there, absolute: [`GRANT_CANDIDATES`] reduced to the
    /// ones that are really there.
    pub toolchains: Vec<String>,
    /// `/proc/self/ns/*` of the shell that ran the helper: the denominator.
    pub outer: BTreeMap<String, String>,
    /// The same, read from inside the boundary.
    pub inside: BTreeMap<String, String>,
    pub seals: BTreeMap<SealKind, Seal>,
    /// Everything the helper and the readback said, for a refusal that has to quote it.
    pub said: String,
}

/// The paths a view holds read-only by default, relative to the home they sit under: the toolchain
/// and the config a commit cannot happen without. None is the home itself, whose mail and `.ssh` are
/// what a boundary exists to leave out. One list for every view, the macOS profile and a host's probe.
pub const GRANT_CANDIDATES: &[&str] = &[
    ".cargo",
    ".rustup",
    ".local/bin",
    ".local/share/mise",
    ".local/state/mise",
    "bin",
    ".gitconfig",
    "Projects/llama.cpp",
    "models",
    ".config/git",
];

/// The globbed candidate, kept separate because it is one: a fixed prefix and a `*`, never a
/// walk.
pub const GRANT_GLOBS: &[&str] = &[
    "$HOME/Projects/llama.cpp/build",
    "$HOME/Projects/llama.cpp/build-*",
];

/// The one script a remote probe runs: the same readback the local probe runs, plus the two
/// facts a view needs (where that home is, which toolchain directories exist) and the
/// denominator read outside the boundary. Nothing here writes: a probe is a question.
pub fn probe_script(view: &ViewSpec, egress: &Egress) -> String {
    let mut s = String::new();
    s.push_str("b=$(command -v bwrap 2>/dev/null || true)\n");
    s.push_str("echo \"helper=$b\"\n");
    s.push_str("echo \"home=$HOME\"\n");
    s.push_str("for d in");
    for c in GRANT_CANDIDATES {
        s.push_str(&format!(" \"$HOME/{c}\""));
    }
    s.push_str("; do [ -e \"$d\" ] && echo \"have=$d\"; done\n");
    s.push_str("for d in");
    for c in GRANT_GLOBS {
        s.push_str(&format!(" {c}"));
    }
    s.push_str("; do [ -d \"$d\" ] && echo \"have=$d\"; done\n");
    s.push_str("if [ -n \"$b\" ]; then echo \"version=$(\"$b\" --version 2>&1 | head -1)\"; fi\n");
    s.push_str(
        "for n in user mnt pid net ipc uts cgroup; do \
         printf 'outer %s %s\\n' \"$n\" \"$(readlink /proc/self/ns/$n)\"; done\n",
    );
    // The readback inside the same argv a command is wrapped in: the probe and the spawn
    // share [`pieces_of`], so what was measured is what runs.
    s.push_str("if [ -n \"$b\" ]; then\n");
    let pieces = pieces_of(
        Piece::Shell("\"$b\"".to_string()),
        view,
        egress,
        &ConfinePlan {
            cwd: &view.project,
            env: &[],
        },
        Session::Detached,
        EnvSource::Remote,
    );
    s.push_str(&join(&pieces));
    s.push_str(&format!(" /bin/sh -c {}\n", crate::remote::quote(READBACK)));
    s.push_str("else echo 'bwrap: no helper on this host' >&2; fi\n");
    s
}

/// Read [`probe_script`]'s output, or the sentence that says why it is not an answer. A
/// helper that is not there is a fact (an empty [`HostProbe::helper`]); a helper that ran and
/// entered nothing is [`judged`]'s refusal, so that judgement does not exist twice.
pub fn read_probe(said: &str) -> HostProbe {
    let mut p = HostProbe {
        said: said.to_string(),
        seals: parse_seals(said),
        ..HostProbe::default()
    };
    let mut inside_text = String::new();
    for line in said.lines() {
        let line = line.trim_end_matches('\r');
        if line.starts_with("ns ") {
            // The readback's own lines, collected for `parse_readback` so the remote readback
            // is read by the same parser as the local one.
            inside_text.push_str(line);
            inside_text.push('\n');
            continue;
        }
        if let Some(rest) = line.strip_prefix("outer ") {
            let mut it = rest.split_whitespace();
            if let (Some(n), Some(inode)) = (it.next(), it.next()) {
                p.outer.insert(n.to_string(), inode.to_string());
            }
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "helper" => p.helper = value.to_string(),
            "version" => p.version = value.to_string(),
            "home" => p.home = value.to_string(),
            "have" if !value.is_empty() => p.toolchains.push(value.to_string()),
            _ => {}
        }
    }
    p.inside = parse_readback(&inside_text);
    p
}

/// The boundary a [`HostProbe`] measured, applied to the view this session needs. The
/// refusals are [`judged`]'s, so a host that cannot enter the namespaces is refused with the
/// same sentence a local one is; the fallback to unconfined is the caller's.
pub fn measured_from_probe(
    probe: &HostProbe,
    view: ViewSpec,
    egress: Egress,
) -> Result<Bwrap, ExecError> {
    if probe.helper.trim().is_empty() {
        return Err(ExecError::NoConfinement(
            "`bwrap` is not on `PATH` there. Layer 1's namespaces are entered through \
             bubblewrap rather than a direct `unshare` because this crate has no `libc`, and \
             because a host with `kernel.apparmor_restrict_unprivileged_userns=1` denies \
             `CAP_SYS_ADMIN` in a freshly created user namespace — `/usr/bin/bwrap` has the \
             `bwrap-userns-restrict` profile that permits it. Install it there (`apt install \
             bubblewrap`) and ask again. Nothing was run confined, and nothing was run in a \
             way that claimed to be."
                .to_string(),
        ));
    }
    let mut ns = BTreeMap::new();
    for n in Namespace::ALL {
        ns.insert(
            n,
            state_of(
                n,
                probe.inside.get(n.proc_name()),
                probe.outer.get(n.proc_name()),
                &egress,
            ),
        );
    }
    // The egress as asked for and the network namespace as measured, kept apart: if a run
    // asked for the host's network and came back with its own, the [`NsState`] says entered
    // and a reader sees the decision did not take.
    let boundary = Boundary::namespaces(
        if probe.version.is_empty() {
            "bubblewrap (version unknown)".to_string()
        } else {
            probe.version.clone()
        },
        view,
        egress,
        ns,
        probe.seals.clone(),
    );
    Bwrap::measured_on_a_host(PathBuf::from(probe.helper.trim()), boundary)
}

/// Prepare a view for any mechanism: the project canonicalised, and the directories a view
/// may have asked for abstractly made real. Shared by both renderings, because whether a
/// directory exists is the same question on both.
pub(crate) fn prepared(mut view: ViewSpec) -> Result<ViewSpec, ExecError> {
    // Canonicalise before anything compares against it: [`ViewSpec::outside`] is lexical, and
    // a lexical comparison against a path holding a symlink would put the project outside its
    // own view and refuse every command.
    if let Ok(c) = view.project.canonicalize() {
        view.project = c;
    }

    if !view.project.is_absolute() {
        return Err(ExecError::NoConfinement(format!(
            "the project root `{}` is not absolute; a view is built from absolute host \
             paths and a relative root would bind whatever the daemon's cwd happens to be",
            view.project.display()
        )));
    }
    if !view.project.is_dir() {
        return Err(ExecError::NoConfinement(format!(
            "the project root `{}` is not a directory, so there is nothing to root the \
             view at",
            view.project.display()
        )));
    }
    if let HomeView::Dir(d) = &view.home
        && let Err(e) = std::fs::create_dir_all(d)
    {
        return Err(ExecError::NoConfinement(format!(
            "`$HOME` was declared as `{}` and it could not be created ({e}). A confined \
             command needs a writable `$HOME` that is NOT the operator's home.",
            d.display()
        )));
    }
    // The session's `/tmp` is made here, once: a `--bind` of a path that is not there is a
    // helper failure at every spawn rather than a setup error at the one place that could
    // explain it.
    if let TmpView::Session(d) = &view.tmp
        && let Err(e) = std::fs::create_dir_all(d)
    {
        return Err(ExecError::NoConfinement(format!(
            "this session's `/tmp` was declared as `{}` and it could not be created ({e}). \
             Without it every command would fail at the helper's own bind, which is a \
             boundary failure and not the command's answer.",
            d.display()
        )));
    }
    // Seatbelt matches the resolved path, so `/var/folders/..` as spelled would allow nothing.
    // Done after the create, which is what makes the path resolvable.
    if let HomeView::Dir(d) = &mut view.home
        && let Ok(c) = d.canonicalize()
    {
        *d = c;
    }
    if let TmpView::Session(d) = &mut view.tmp
        && let Ok(c) = d.canonicalize()
    {
        *d = c;
    }
    Ok(view)
}

/// The boundary this platform can measure for `view`, or the refusal that says what is
/// missing: Linux answers with [`Bwrap`], macOS with [`super::seatbelt::Seatbelt`]. Boxed
/// deliberately, because a caller that widens a view replaces one probe's answer with the next.
pub fn probe(view: ViewSpec, egress: Egress) -> Result<Box<dyn Confinement>, ExecError> {
    #[cfg(target_os = "macos")]
    {
        super::seatbelt::Seatbelt::probe(view, egress).map(|s| Box::new(s) as Box<dyn Confinement>)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Bwrap::probe(view, egress).map(|b| Box::new(b) as Box<dyn Confinement>)
    }
}

/// Can this platform build a boundary at all, without asking what the view is? A compile-time
/// fact rather than a probe, so a caller that behaves differently when there is none can ask
/// first; whether the helper is there is [`probe`]'s question.
pub fn available() -> bool {
    cfg!(target_os = "macos") || cfg!(target_os = "linux")
}

impl Confinement for Bwrap {
    fn describe(&self) -> String {
        self.boundary.describe()
    }

    fn boundary(&self) -> Option<&Boundary> {
        Some(&self.boundary)
    }

    // The wide seat is the operator's own session: their `!` line runs outside it, as on a Mac.
    fn spares_operator(&self) -> bool {
        self.boundary.view.wide
    }

    fn wrap(&self, plan: &ConfinePlan<'_>) -> Result<Vec<String>, ExecError> {
        self.wrap_with(plan, Session::Detached)
    }

    fn wrap_terminal(&self, plan: &ConfinePlan<'_>) -> Result<Vec<String>, ExecError> {
        self.wrap_with(plan, Session::Kept)
    }

    fn wrap_pieces(&self, plan: &ConfinePlan<'_>, env: EnvSource) -> Result<Vec<Piece>, ExecError> {
        match env {
            // The far side's own shell supplies the keep-list, and the session is the remote
            // one so a group kill still reaches the sandbox: see [`Session`].
            EnvSource::Remote => self.remote_pieces(plan, Session::Kept),
            EnvSource::Process => Ok(self
                .wrap_with(plan, Session::Detached)?
                .into_iter()
                .map(Piece::Literal)
                .collect()),
        }
    }

    fn outside_view(&self, path: &Path) -> bool {
        self.boundary.view.outside(path)
    }

    fn absence_notes(&self, output: &str) -> Vec<String> {
        self.boundary.absence_notes(output)
    }

    fn outside_paths(&self, output: &str) -> Vec<PathBuf> {
        self.boundary.outside_paths(output)
    }

    fn refused_write_paths(&self, output: &str) -> Vec<PathBuf> {
        self.boundary.refused_write_paths(output)
    }

    /// bubblewrap prefixes its own diagnostics `bwrap: `, which is the only signal it gives.
    /// A heuristic, and the alternative (reporting a boundary failure as the command's answer)
    /// is worse, because it is a false statement about what ran.
    fn launcher_failure(&self, output: &str) -> Option<String> {
        let line = output
            .lines()
            .find(|l| l.trim_start().starts_with("bwrap: "))?
            .trim()
            .to_string();
        Some(format!(
            "the confinement helper failed to set up the boundary, so this is NOT the \
             command's answer — the command may never have run. Its own words: `{line}`. \
             The boundary asked for was: {}",
            self.boundary
                .view
                .summary()
                .lines()
                .next()
                .unwrap_or("the project")
        ))
    }
}

impl Bwrap {
    /// [`Confinement::wrap`] and its two siblings, which differ by one flag: [`Session`]. The
    /// probe measured the argv with `--new-session`, and neither the namespaces nor the seals
    /// depend on it, so one boundary describes every spawn.
    fn wrap_with(
        &self,
        plan: &ConfinePlan<'_>,
        session: Session,
    ) -> Result<Vec<String>, ExecError> {
        self.check(plan)?;
        Ok(build_argv_with(
            &self.helper,
            &self.boundary.view,
            &self.boundary.egress,
            plan,
            session,
        ))
    }

    /// [`Confinement::wrap_pieces`] as the far side renders it: the same view, egress and
    /// session flag, with the keep-list taken from the shell that runs the helper there. This
    /// is `Bwrap`'s and not the backend's so there is exactly one spelling of the binds.
    pub fn remote_pieces(
        &self,
        plan: &ConfinePlan<'_>,
        session: Session,
    ) -> Result<Vec<Piece>, ExecError> {
        self.check(plan)?;
        Ok(pieces_of(
            Piece::literal(self.helper.display().to_string()),
            &self.boundary.view,
            &self.boundary.egress,
            plan,
            session,
            EnvSource::Remote,
        ))
    }

    /// The two refusals every wrap makes, for both renderings.
    fn check(&self, plan: &ConfinePlan<'_>) -> Result<(), ExecError> {
        // Belt and braces: a `Bwrap` whose boundary lost a required namespace cannot produce
        // an argv. No path mutates it, which is what keeps the check cheap.
        if !self.boundary.complete() {
            return Err(ExecError::NoConfinement(format!(
                "this boundary is missing a required namespace, so nothing runs \
                 inside it: {}",
                self.boundary.describe()
            )));
        }
        if self.boundary.view.outside(plan.cwd) {
            return Err(ExecError::NotInView {
                path: plan.cwd.display().to_string(),
                view: self.boundary.view.summary(),
            });
        }
        Ok(())
    }

    /// The helper this boundary was measured with, as the far side spells it.
    pub fn helper(&self) -> &Path {
        &self.helper
    }
}

/// The `--unshare-` flag for a namespace, for the refusal to name.
fn unshare_flag(n: Namespace) -> &'static str {
    match n {
        Namespace::User => "user",
        Namespace::Mount => "mount (implied: bwrap always makes a mount namespace)",
        Namespace::Pid => "pid",
        Namespace::Net => "net",
        Namespace::Ipc => "ipc",
        Namespace::Uts => "uts",
        Namespace::Cgroup => "cgroup",
    }
}

/// The shell fragment the probe runs inside the boundary. No version numbers or feature flags:
/// namespaces are read from `/proc/self/ns/*` inside, the escape seal is tested by trying
/// `unshare -U true`, and `NoNewPrivs` is the kernel's answer in `/proc/self/status`.
const READBACK: &str = "\
for n in user mnt pid net ipc uts cgroup; do printf 'ns %s %s\\n' \"$n\" \"$(readlink /proc/self/ns/$n)\"; done; \
if command -v unshare >/dev/null 2>&1; then \
  if unshare -U true >/dev/null 2>&1; then printf 'seal nested open\\n'; \
  else printf 'seal nested held\\n'; fi; \
else printf 'seal nested unverified\\n'; fi; \
printf 'seal nnp %s\\n' \"$(sed -n 's/^NoNewPrivs:[[:space:]]*//p' /proc/self/status 2>/dev/null)\"";

/// The invocation, built once and used by the probe and by every spawn, so that what was
/// measured is what runs: a probe against a different argv is the banner defect again.
fn build_argv(
    helper: &Path,
    view: &ViewSpec,
    egress: &Egress,
    plan: &ConfinePlan<'_>,
) -> Vec<String> {
    pieces_of(
        Piece::literal(helper.display().to_string()),
        view,
        egress,
        plan,
        Session::Detached,
        EnvSource::Process,
    )
    .into_iter()
    .map(|p| match p {
        Piece::Literal(s) => s,
        // Unreachable by construction: [`EnvSource::Process`] never emits a shell fragment,
        // and a fragment in an argv would be a literal `$PATH` rather than a panic.
        Piece::Shell(s) => s,
    })
    .collect()
}

/// [`build_argv`], and for a pane the same argv without `--new-session`: see
/// [`Confinement::wrap_terminal`].
pub(crate) fn build_argv_with(
    helper: &Path,
    view: &ViewSpec,
    egress: &Egress,
    plan: &ConfinePlan<'_>,
    session: Session,
) -> Vec<String> {
    pieces_of(
        Piece::literal(helper.display().to_string()),
        view,
        egress,
        plan,
        session,
        EnvSource::Process,
    )
    .into_iter()
    .map(|p| match p {
        Piece::Literal(s) => s,
        Piece::Shell(s) => s,
    })
    .collect()
}

/// The one builder, in pieces: the local argv, the pane's argv and a command on another
/// machine all come out of here. `helper` is a [`Piece`] because a remote probe has to spell
/// it `"$(command -v bwrap)"`, which is not known here until that script has run.
fn pieces_of(
    helper: Piece,
    view: &ViewSpec,
    egress: &Egress,
    plan: &ConfinePlan<'_>,
    session: Session,
    env: EnvSource,
) -> Vec<Piece> {
    let mut a: Vec<Piece> = vec![helper];
    fn arg(a: &mut Vec<Piece>, s: impl Into<String>) {
        a.push(Piece::literal(s));
    }

    arg(&mut a, "--unshare-user");
    arg(&mut a, "--unshare-pid");
    arg(&mut a, "--unshare-ipc");
    arg(&mut a, "--unshare-uts");
    arg(&mut a, "--unshare-cgroup");
    if !egress.allowed() {
        arg(&mut a, "--unshare-net");
    }
    // Seal the boundary against a child stepping out of it: without this, an unprivileged
    // process inside can make a user namespace and unshare a mount namespace past this view.
    // `--assert-userns-disabled` makes the helper refuse, and the probe measures it anyway.
    arg(&mut a, "--disable-userns");
    arg(&mut a, "--assert-userns-disabled");
    // If the harness dies, this dies: a third mechanism on top of the cgroup and the join
    // script, and it is free.
    arg(&mut a, "--die-with-parent");
    // No controlling terminal, so `TIOCSTI` cannot push characters back into the operator's
    // shell. Kept off for a pane, whose pty is its own, and for a remote command, whose
    // process group an interrupt has to be able to kill: see [`Session`].
    if session == Session::Detached {
        arg(&mut a, "--new-session");
    }

    if view.wide {
        // The machine itself, at its own paths and writable as this user: the wide seat, whose only
        // hidden things are the masks below. `--dev-bind`, because a plain bind leaves every mount
        // under `/` nodev, and `/dev/null` is then unopenable, masks included.
        arg(&mut a, "--dev-bind");
        arg(&mut a, "/");
        arg(&mut a, "/");
        // Still this command's own `/proc`, because the PID namespace above is: the host's would
        // show pids the namespace says are not there.
        arg(&mut a, "--proc");
        arg(&mut a, "/proc");
    } else {
        // The system roots, read-only and `-try`: a host without `/lib64` is not a failure, and a
        // merged-`/usr` host has symlinks where a split one has directories.
        for p in &view.system {
            arg(&mut a, "--ro-bind-try");
            arg(&mut a, p.display().to_string());
            arg(&mut a, p.display().to_string());
        }
        // A shared network has to be able to name things: with the host's routes and no resolver
        // files every name fails to resolve, so the decision to share the network reads as no
        // network at all. Only with egress, and a symlink binds its target.
        if egress.allowed() {
            for p in NETWORK_ROOTS {
                arg(&mut a, "--ro-bind-try");
                arg(&mut a, *p);
                arg(&mut a, *p);
            }
        }
        // The kernel interfaces a process needs to be a process. `--proc` is what makes the PID
        // namespace visible as isolation rather than as a lie: without it the sandbox reads the
        // host's `/proc` and sees every pid.
        arg(&mut a, "--proc");
        arg(&mut a, "/proc");
        arg(&mut a, "--dev");
        arg(&mut a, "/dev");
        // `/tmp` is the session's, not the command's, when the caller has a directory for it: a
        // fresh tmpfs silently eats a two-call scratch idiom. `/var/tmp` and `/run` stay fresh
        // either way, and `/run` holds the agent socket and the tmpfs home bound below.
        match &view.tmp {
            TmpView::PerCommand => {
                arg(&mut a, "--tmpfs");
                arg(&mut a, "/tmp");
            }
            TmpView::Session(dir) => {
                arg(&mut a, "--bind");
                arg(&mut a, dir.display().to_string());
                arg(&mut a, "/tmp");
            }
        }
        arg(&mut a, "--tmpfs");
        arg(&mut a, "/var/tmp");
        arg(&mut a, "--tmpfs");
        arg(&mut a, "/run");

        // The project, read-write, at its own path.
        arg(&mut a, "--bind");
        arg(&mut a, view.project.display().to_string());
        arg(&mut a, view.project.display().to_string());
    }

    for g in &view.grants {
        match g {
            Grant::ReadOnly { path, .. } => {
                arg(&mut a, "--ro-bind");
                arg(&mut a, path.display().to_string());
                arg(&mut a, path.display().to_string());
            }
            Grant::ReadWrite { path, .. } => {
                arg(&mut a, "--bind");
                arg(&mut a, path.display().to_string());
                arg(&mut a, path.display().to_string());
            }
            Grant::AgentSocket { path, .. } => {
                // Bound at a fixed path so `SSH_AUTH_SOCK` inside does not carry the
                // operator's `/run/user/<uid>/...` out into the transcript.
                arg(&mut a, "--bind");
                arg(&mut a, path.display().to_string());
                arg(&mut a, AGENT_SOCKET);
            }
        }
    }

    // The seat's fence, after the mounts it has to win over. A store the view does not reach, or that
    // is not on the disk, gets no mount: bwrap would make the mount point in the operator's own home,
    // and a directory already masked as an empty tmpfs cannot hold one either.
    let mut emptied: Vec<&Path> = Vec::new();
    for p in &view.masked {
        if view.classify(p) != Presence::Inside || emptied.iter().any(|d| p.starts_with(d)) {
            continue;
        }
        match (p.is_dir(), p.is_file()) {
            (true, _) => {
                emptied.push(p);
                arg(&mut a, "--tmpfs");
                arg(&mut a, p.display().to_string());
                // The letibot store keeps its data readable: bound read-only into the empty
                // tmpfs before it is remounted, with the secrets inside the data covered.
                if p.ends_with(super::store::STORE_REL) {
                    let data = super::store::readable_entries(p);
                    for e in &data {
                        arg(&mut a, "--ro-bind");
                        arg(&mut a, e.display().to_string());
                        arg(&mut a, e.display().to_string());
                    }
                    for (secret, is_dir) in super::store::secrets_within(&data) {
                        if is_dir {
                            arg(&mut a, "--tmpfs");
                        } else {
                            arg(&mut a, "--ro-bind");
                            arg(&mut a, "/dev/null");
                        }
                        arg(&mut a, secret.display().to_string());
                    }
                }
                // Read-only too, so a command cannot fill it and have the operator's own tools read
                // what it left there.
                arg(&mut a, "--remount-ro");
                arg(&mut a, p.display().to_string());
            }
            (false, true) => {
                // `--dev-bind`: a `--ro-bind` mount is nodev, so reading the stand-in is a
                // permission error instead of an empty file.
                arg(&mut a, "--dev-bind");
                arg(&mut a, "/dev/null");
                arg(&mut a, p.display().to_string());
            }
            (false, false) => {}
        }
    }

    for p in &view.protected {
        if view.classify(&p.path) != Presence::Inside
            || emptied.iter().any(|d| p.path.starts_with(d))
        {
            continue;
        }
        // A path that is there is bound over itself; one that is not gets an empty read-only
        // placeholder of its own kind. The parent must be there, or bwrap makes the parents in the
        // operator's real tree and a path nobody can reach needs no placeholder.
        let parent_there = p.path.parent().is_some_and(|d| d.exists());
        match (p.path.exists(), p.kind) {
            (true, _) => {
                arg(&mut a, "--ro-bind");
                arg(&mut a, p.path.display().to_string());
                arg(&mut a, p.path.display().to_string());
            }
            (false, ProtectedKind::Dir) if parent_there => {
                // A tmpfs is created empty and then remounted read-only: a directory that is
                // there and cannot be written, which is what an absent `hooks/` was not.
                arg(&mut a, "--tmpfs");
                arg(&mut a, p.path.display().to_string());
                arg(&mut a, "--remount-ro");
                arg(&mut a, p.path.display().to_string());
            }
            (false, ProtectedKind::File) if parent_there => {
                // `/dev/null` is the one empty file every box has: bound in its place, a name a
                // `mkdir` or `touch` would otherwise claim exists and refuses both.
                arg(&mut a, "--ro-bind");
                arg(&mut a, "/dev/null");
                arg(&mut a, p.path.display().to_string());
            }
            (false, _) => {}
        }
    }

    // Hidden files last, over whatever bound them: a socket replaced by `/dev/null` cannot be
    // connected to or read. Only where the view reaches the path; elsewhere it is already absent
    // and a bind would make its parents appear.
    let mut hidden = view.hidden.clone();
    hidden.extend(room_files());
    hidden.sort();
    hidden.dedup();
    for p in &hidden {
        if view.classify(p) == Presence::Inside
            && p.exists()
            && !emptied.iter().any(|d| p.starts_with(d))
        {
            arg(&mut a, "--ro-bind");
            arg(&mut a, "/dev/null");
            arg(&mut a, p.display().to_string());
        }
    }

    let home = match &view.home {
        HomeView::Tmpfs => TMPFS_HOME.to_string(),
        HomeView::Dir(d) => {
            // A wide view already has the operator's home at its own path. Anywhere else it is a
            // directory of letibot's, which nothing has bound.
            if !view.wide {
                arg(&mut a, "--bind");
                arg(&mut a, d.display().to_string());
                arg(&mut a, d.display().to_string());
            }
            d.display().to_string()
        }
    };
    if matches!(view.home, HomeView::Tmpfs) {
        // `/run` is already a tmpfs above; this is the directory inside it.
        arg(&mut a, "--dir");
        arg(&mut a, TMPFS_HOME);
    }

    // Clear, then put back a keep-list: see [`KEEP_ENV`], absence beats denial applied to
    // the environment.
    arg(&mut a, "--clearenv");
    for k in KEEP_ENV {
        match env {
            EnvSource::Process => {
                if let Ok(v) = std::env::var(k) {
                    arg(&mut a, "--setenv");
                    arg(&mut a, *k);
                    arg(&mut a, v);
                }
            }
            // Expanded by the shell that runs the helper, the far one, because the value has
            // to be that machine's.
            EnvSource::Remote => {
                arg(&mut a, "--setenv");
                arg(&mut a, *k);
                a.push(Piece::Shell(format!("\"${k}\"")));
            }
        }
    }
    arg(&mut a, "--setenv");
    arg(&mut a, "HOME");
    arg(&mut a, home);
    // A wide view keeps this machine's `/tmp`, so the session's scratch reaches the command as
    // `$TMPDIR` instead: the `--bind` over `/tmp` above is the tight view's arm.
    if view.wide
        && let TmpView::Session(d) = &view.tmp
    {
        arg(&mut a, "--setenv");
        arg(&mut a, "TMPDIR");
        arg(&mut a, d.display().to_string());
    }
    if view
        .grants
        .iter()
        .any(|g| matches!(g, Grant::AgentSocket { .. }))
    {
        arg(&mut a, "--setenv");
        arg(&mut a, "SSH_AUTH_SOCK");
        arg(&mut a, AGENT_SOCKET);
    }
    // The caller's own environment last, so an explicit request wins over the keep-list. A secret
    // comes in on [`SECRET_FD`] instead, from a local pipe or from the ssh session's own stdin, so
    // no argv on either machine ever carries its value: `ps` reads argv, not a descriptor.
    let mut sealed = false;
    for (k, v) in plan.env {
        if super::boundary::is_secret_name(k) {
            sealed = true;
            continue;
        }
        arg(&mut a, "--setenv");
        arg(&mut a, k);
        arg(&mut a, v);
    }
    if sealed {
        arg(&mut a, "--args");
        arg(&mut a, SECRET_FD.to_string());
    }

    arg(&mut a, "--chdir");
    arg(&mut a, plan.cwd.display().to_string());
    // Everything after this is the command, never an option, whatever it is spelled like.
    arg(&mut a, "--");
    a
}

/// Where the helper is. `LETIBOT_BWRAP` first, so a test can point it at nothing.
fn find_helper() -> Result<PathBuf, ExecError> {
    if let Ok(p) = std::env::var(BWRAP_ENV) {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Ok(p);
        }
        return Err(ExecError::NoConfinement(format!(
            "`{BWRAP_ENV}` names `{}`, which is not a file, so there is no \
             confinement helper and nothing will be run",
            p.display()
        )));
    }
    for dir in std::env::var("PATH").unwrap_or_default().split(':') {
        if dir.is_empty() {
            continue;
        }
        let c = Path::new(dir).join("bwrap");
        if c.is_file() {
            return Ok(c);
        }
    }
    Err(ExecError::NoConfinement(
        "`bwrap` is not on `PATH`. Layer 1's namespaces are entered through \
         bubblewrap rather than a direct `unshare` because this crate has no `libc` \
         and because a host with \
         `kernel.apparmor_restrict_unprivileged_userns=1` denies `CAP_SYS_ADMIN` in \
         a freshly created user namespace, which makes the `uid_map` write fail and \
         the namespace inert — `/usr/bin/bwrap` has the `bwrap-userns-restrict` \
         profile that permits it. Install it (`apt install bubblewrap`) or point \
         `LETIBOT_BWRAP` at it. Nothing was run, and nothing was run unconfined."
            .to_string(),
    ))
}

/// The helper's version, for the record: a mechanism named `namespaces` is not one anybody
/// can reproduce.
fn version_of(helper: &Path) -> String {
    std::process::Command::new(helper)
        .arg("--version")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("{} (version unknown)", helper.display()))
}

/// The harness's own namespace inodes: the denominator for every readback.
fn own_namespaces() -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    for n in Namespace::ALL {
        if let Ok(l) = std::fs::read_link(format!("/proc/self/ns/{}", n.proc_name())) {
            m.insert(n.proc_name().to_string(), l.display().to_string());
        }
    }
    m
}

/// `ns <name> <inode>` lines from [`READBACK`].
fn parse_readback(text: &str) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    for line in text.lines() {
        let mut it = line.split_whitespace();
        if it.next() != Some("ns") {
            continue;
        }
        let (Some(name), Some(inode)) = (it.next(), it.next()) else {
            continue;
        };
        m.insert(name.to_string(), inode.to_string());
    }
    m
}

/// `seal <name> <verdict>` lines from [`READBACK`]. A seal that produced no line at all is
/// [`Seal::Unverified`] and never [`Seal::Held`]: an absent measurement is not a passing one.
fn parse_seals(text: &str) -> BTreeMap<SealKind, Seal> {
    let mut got: BTreeMap<String, String> = BTreeMap::new();
    for line in text.lines() {
        let mut it = line.split_whitespace();
        if it.next() != Some("seal") {
            continue;
        }
        if let (Some(name), Some(v)) = (it.next(), it.next()) {
            got.insert(name.to_string(), v.to_string());
        } else if let Some(name) = it.next() {
            // `seal nnp` with nothing after it: the file was unreadable.
            got.insert(name.to_string(), String::new());
        }
    }
    let mut out = BTreeMap::new();
    out.insert(
        SealKind::NestedNamespaces,
        match got.get("nested").map(String::as_str) {
            Some("held") => Seal::Held {
                how: "`unshare -U true` inside the boundary failed, so a child cannot \
                      obtain the capabilities it would need to leave"
                    .into(),
            },
            Some("open") => Seal::Open {
                why: "`unshare -U true` inside the boundary SUCCEEDED, so a child can \
                      make itself a user namespace, gain full capabilities in it, and \
                      `unshare` a mount namespace out of this view"
                    .into(),
            },
            Some("unverified") => Seal::Unverified {
                why: "`unshare` is not in this view, so the escape could not be tried. \
                      `--assert-userns-disabled` is an independent mechanism and would \
                      have made the helper refuse, but that is the helper's word and \
                      not a measurement"
                    .into(),
            },
            _ => Seal::Unverified {
                why: "the readback printed no verdict for this seal".into(),
            },
        },
    );
    out.insert(
        SealKind::NoNewPrivs,
        match got.get("nnp").map(String::as_str) {
            Some("1") => Seal::Held {
                how: "`/proc/self/status` reports `NoNewPrivs: 1` inside the boundary".into(),
            },
            Some("0") => Seal::Open {
                why: "`/proc/self/status` reports `NoNewPrivs: 0` inside the boundary, \
                      so a setuid binary reachable in the view is an escalation"
                    .into(),
            },
            _ => Seal::Unverified {
                why: "`/proc/self/status` could not be read from inside".into(),
            },
        },
    );
    out
}

fn snippet(s: &str) -> String {
    let t: String = s.chars().take(200).collect();
    if t.trim().is_empty() {
        "(nothing)".to_string()
    } else {
        format!("`{}`", t.trim())
    }
}

#[cfg(test)]
#[cfg(test)]
mod tests;
