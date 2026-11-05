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
/// letibot's data directory, plus `$LETIBOT_ROOM_SOCKET` and its lease and token. Found by
/// scanning, so no host key is needed. A tool call that could open the socket could post as anyone
/// in the rooms, and one that could read the token could post as the operator.
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

/// The daemon's control sockets a tight seat must not reach: every socket in the per-user
/// `letibot` runtime directories (the daemon's own, the ssh control masters) and `$LETIBOT_SOCKET`.
/// The wide seat keeps them: `sudo -A` reaches the session through that socket, and the daemon
/// answers a command's tree with that one door only.
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
            for tok in absolute_paths(line) {
                if matches!(self.view.classify(&tok), Presence::Outside) && !outside.contains(&tok)
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
                 {} as absent. That failure is the boundary, NOT an answer about \
                 whether anything is there: this session's view is {}. Nothing \
                 outside it can be reached by another spelling of the path, and {}. \
                 If the work genuinely needs a path outside the view, the operator is \
                 being asked to grant it in — re-running before they answer will not \
                 change it.",
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

/// The paths a remote view holds read-only by default: the toolchain, and the one config file
/// a commit cannot happen without. Each is under that machine's `$HOME` and none is the home
/// itself, whose mail, browser profile and `.ssh` are what this boundary exists to leave out.
pub const GRANT_CANDIDATES: &[&str] = &[
    "$HOME/.cargo",
    "$HOME/.rustup",
    "$HOME/.local/bin",
    "$HOME/.local/share/mise",
    "$HOME/.local/state/mise",
    "$HOME/bin",
    "$HOME/.gitconfig",
    "$HOME/Projects/llama.cpp",
    "$HOME/models",
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
        s.push_str(&format!(" \"{c}\""));
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
fn build_argv_with(
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
        // The machine itself, at its own paths and writable as this user: that is the wide seat, and
        // the masks below (after the grants) are the only thing it hides. The system roots and the
        // project are inside this bind, so binding them again would only be two mounts of one tree.
        // `--dev-bind`: a plain bind leaves every mount under `/` nodev, and `/dev/null` is then
        // unopenable, which also breaks the file masks bound from it.
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

    // The seat's fence, after the mounts it has to win over: a wide view reaches every store, and a
    // tight one reaches the few a grant covers. A store the view does not reach, or that is not on
    // the disk, gets no mount: bwrap would make the mount point in the operator's own home.
    // A directory masked as an empty read-only tmpfs holds nothing to hide and nothing to mount
    // over: bwrap cannot make a mount point in it, so later mounts under it are skipped.
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
    // The caller's own environment last, so an explicit request wins over the keep-list.
    for (k, v) in plan.env {
        arg(&mut a, "--setenv");
        arg(&mut a, k);
        arg(&mut a, v);
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
mod tests {
    use super::*;

    fn boundary_with(states: Vec<(Namespace, NsState)>) -> Boundary {
        let mut ns = BTreeMap::new();
        for n in Namespace::ALL {
            ns.insert(
                n,
                NsState::Entered {
                    inode: format!("{n}:[1000{}]", n as u8),
                },
            );
        }
        for (n, s) in states {
            ns.insert(n, s);
        }
        let mut seals = BTreeMap::new();
        for k in SealKind::ALL {
            seals.insert(
                k,
                Seal::Held {
                    how: format!("{k} was tested from inside the fixture"),
                },
            );
        }
        Boundary {
            mechanism: "bubblewrap 0.0.0 (fixture)".into(),
            platform: Mechanism::Namespaces,
            view: ViewSpec::project_only("/proj"),
            egress: Egress::Denied,
            ns,
            seals,
            props: BTreeMap::new(),
            measured_at: SystemTime::now(),
        }
    }

    #[test]
    fn a_partial_boundary_describes_itself_as_partial() {
        // The sentence is computed from the state, so changing one entry in the map must change
        // the disclosure, and a hard-coded `confined` cannot pass.
        let full = boundary_with(vec![]);
        assert!(full.complete());
        assert!(!full.partial());
        assert!(
            full.describe().starts_with("confined;"),
            "{}",
            full.describe()
        );

        let partial = boundary_with(vec![(
            Namespace::Ipc,
            NsState::NotEntered {
                why: "the inode inside is the harness's own".into(),
            },
        )]);
        // Required namespaces are all there, so it still confines, and it must not describe
        // itself as if nothing were missing.
        assert!(partial.complete());
        assert!(partial.partial());
        let d = partial.describe();
        assert!(d.contains("PARTIALLY"), "{d}");
        assert!(d.contains("NOT ENTERED"), "{d}");
        assert!(d.contains("ipc"), "{d}");
        assert_eq!(partial.missing().len(), 1);
        assert_eq!(partial.missing()[0].0, Namespace::Ipc);
    }

    #[test]
    fn a_boundary_missing_a_required_namespace_says_a_command_is_refused() {
        let broken = boundary_with(vec![(
            Namespace::Pid,
            NsState::NotEntered {
                why: "`--unshare-pid` did not take effect".into(),
            },
        )]);
        assert!(!broken.complete());
        let d = broken.describe();
        assert!(d.contains("NOT CONFINED"), "{d}");
        assert!(d.contains("refused"), "{d}");
        // The missing namespace is named with what it would have bought, so the reader knows
        // what they do not have.
        assert!(d.contains("cannot see or signal the daemon"), "{d}");
    }

    #[test]
    fn a_shared_namespace_is_a_decision_and_not_a_failure() {
        // The two look identical from inside (the same inode as the harness) and are
        // different facts: collapsing them would report a declared egress as a broken
        // boundary, and the reverse.
        let b = Boundary {
            egress: Egress::Host {
                why: "the operator authorised `git push` to origin".into(),
            },
            ..boundary_with(vec![(
                Namespace::Net,
                NsState::SharedByDecision {
                    why: "the operator authorised `git push` to origin".into(),
                },
            )])
        };
        assert!(b.complete(), "net is not required");
        assert!(!b.partial(), "a decision is not a missing namespace");
        let d = b.describe();
        assert!(d.contains("SHARED WITH THE HOST by decision"), "{d}");
        assert!(d.contains("git push"), "{d}");
        assert!(!d.contains("NOT ENTERED"), "{d}");
    }

    #[test]
    fn the_default_disclosure_states_the_tmpfs_home_cost() {
        // A surprise slowdown nobody can explain is a worse disclosure than a sentence here.
        let d = boundary_with(vec![]).describe();
        assert!(d.contains("FRESH TMPFS"), "{d}");
        assert!(d.contains("build cache"), "{d}");
        assert!(d.contains("DENIED"), "{d}");
    }

    #[test]
    fn no_confinement_refuses_and_names_what_is_missing() {
        let c = NoConfinement::new("`bwrap` is not on `PATH`");
        let e = c
            .wrap(&ConfinePlan {
                cwd: Path::new("/proj"),
                env: &[],
            })
            .unwrap_err();
        let m = format!("{e}");
        assert!(m.contains("bwrap"), "{m}");
        assert!(m.contains("was NOT run"), "{m}");
        assert!(c.boundary().is_none());
        assert!(c.describe().contains("refuses to exec"));
    }

    /// A protected path that is not there is a name a command can claim, and bwrap can only mount
    /// over what exists: each kind gets an empty read-only placeholder of its own kind, and a path
    /// whose parent is absent gets none, because bwrap would make the parents in the real tree.
    #[test]
    fn a_protected_path_that_is_not_there_is_still_bound_read_only() {
        let dir = std::env::temp_dir().join(format!("letibot-protect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("common/present")).expect("a fixture tree");
        let view = ViewSpec::project_only(&dir)
            .protecting(Protection::dir(dir.join("common/present")))
            .protecting(Protection::dir(dir.join("common/hooks")))
            .protecting(Protection::file(dir.join("common/packed-refs")))
            .protecting(Protection::dir(dir.join("common/gone/child")));
        let argv = build_argv_with(
            Path::new("/usr/bin/bwrap"),
            &view,
            &Egress::Denied,
            &ConfinePlan {
                cwd: &dir,
                env: &[],
            },
            Session::Detached,
        );
        let joined = argv.join(" ");
        let present = dir.join("common/present").display().to_string();
        assert!(
            joined.contains(&format!("--ro-bind {present} {present}")),
            "a path that is there is bound over itself: {joined}"
        );
        let hooks = dir.join("common/hooks").display().to_string();
        assert!(
            joined.contains(&format!("--tmpfs {hooks} --remount-ro {hooks}")),
            "an absent directory gets an empty one it cannot write: {joined}"
        );
        let packed = dir.join("common/packed-refs").display().to_string();
        assert!(
            joined.contains(&format!("--ro-bind /dev/null {packed}")),
            "an absent file is the empty read-only one every box has: {joined}"
        );
        assert!(
            !joined.contains(&dir.join("common/gone").display().to_string()),
            "a parent that is absent is not made in the operator's tree: {joined}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The wide seat's argv: the machine at its own paths and writable, with the stores masked out
    /// of it. Nothing is bound twice, and the mask is the whole of what the view hides.
    #[test]
    fn a_wide_view_binds_the_machine_and_masks_the_stores_it_is_given() {
        let dir = std::env::temp_dir().join(format!("letibot-wide-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let home = dir.join("home");
        std::fs::create_dir_all(home.join(".ssh")).expect("a fake store");
        std::fs::write(home.join(".npmrc"), "//registry/:_authToken=fake\n").expect("a fake file");
        std::fs::create_dir_all(dir.join("project")).expect("a project");
        let tmp = dir.join("tmp");
        let view = ViewSpec::project_only(dir.join("project"))
            .with_home(HomeView::Dir(home.clone()))
            .with_tmp(TmpView::Session(tmp.clone()))
            .wide()
            .masking([home.join(".ssh"), home.join(".npmrc"), home.join(".aws")]);
        let argv = build_argv_with(
            Path::new("/usr/bin/bwrap"),
            &view,
            &Egress::Denied,
            &ConfinePlan {
                cwd: &dir,
                env: &[],
            },
            Session::Detached,
        );
        let joined = argv.join(" ");
        assert!(joined.contains("--dev-bind / /"), "{joined}");
        // Still this command's own `/proc`, because the PID namespace above it is.
        assert!(joined.contains("--proc /proc"), "{joined}");
        let ssh = home.join(".ssh").display().to_string();
        assert!(
            joined.contains(&format!("--tmpfs {ssh} --remount-ro {ssh}")),
            "a store that is a directory is an empty one: {joined}"
        );
        let npmrc = home.join(".npmrc").display().to_string();
        assert!(
            joined.contains(&format!("--dev-bind /dev/null {npmrc}")),
            "a store that is a file is the unreadable empty one: {joined}"
        );
        assert!(
            !joined.contains(&home.join(".aws").display().to_string()),
            "a store that is not there has no mount point, and making one writes the operator's \
             home: {joined}"
        );
        // The machine IS the view, so nothing is bound a second time: no system roots, no project,
        // and the machine keeps its own `/tmp`.
        assert!(!joined.contains("--ro-bind-try"), "{joined}");
        assert!(!joined.contains("--tmpfs /tmp "), "{joined}");
        let project = dir.join("project").display().to_string();
        assert!(
            !joined.contains(&format!("--bind {project} {project}")),
            "{joined}"
        );
        assert!(
            joined.contains(&format!("--setenv HOME {}", home.display())),
            "{joined}"
        );
        assert!(
            joined.contains(&format!("--setenv TMPDIR {}", tmp.display())),
            "the session's scratch reaches a wide command as $TMPDIR: {joined}"
        );
        assert!(
            joined.contains("--unshare-net"),
            "a read-only seat has no network: {joined}"
        );
        // And the same view with the network seated does not take it away.
        let open = build_argv_with(
            Path::new("/usr/bin/bwrap"),
            &view,
            &Egress::Host {
                why: "a test".into(),
            },
            &ConfinePlan {
                cwd: &dir,
                env: &[],
            },
            Session::Detached,
        )
        .join(" ");
        assert!(!open.contains("--unshare-net"), "{open}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A file inside a directory that is masked as an empty tmpfs is already hidden, and bwrap
    /// cannot make a mount point in that read-only tmpfs: no mount is emitted for it.
    #[test]
    fn nothing_is_mounted_inside_a_directory_masked_as_empty() {
        let dir = std::env::temp_dir().join(format!("letibot-wide-in-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let home = dir.join("home");
        let store = home.join(".local/share/letibot");
        std::fs::create_dir_all(&store).expect("a fake store");
        let lease = store.join("room-x.lease");
        std::fs::write(&lease, "").expect("a fake lease");
        std::fs::create_dir_all(dir.join("project")).expect("a project");
        let view = ViewSpec::project_only(dir.join("project"))
            .with_home(HomeView::Dir(home.clone()))
            .wide()
            .masking([store.clone()])
            .hiding(lease.clone());
        let joined = build_argv_with(
            Path::new("/usr/bin/bwrap"),
            &view,
            &Egress::Denied,
            &ConfinePlan {
                cwd: &dir,
                env: &[],
            },
            Session::Detached,
        )
        .join(" ");
        let s = store.display().to_string();
        assert!(joined.contains(&format!("--tmpfs {s} ")), "{joined}");
        assert!(
            !joined.contains(&format!("/dev/null {}", lease.display())),
            "a mount under the emptied store fails every spawn: {joined}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// One list, two renderings: the Linux seat's masks are the paths the macOS profile denies,
    /// over the same home, so a store added to one cannot be missing from the other.
    #[test]
    fn the_wide_seats_masks_are_the_list_the_macos_seat_denies() {
        use crate::exec::seat;
        let dir = std::env::temp_dir().join(format!("letibot-parity-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let home = dir.join("operator");
        let mut want: Vec<PathBuf> = crate::exec::seatbelt::deny_under_home(true)
            .into_iter()
            .map(|rel| home.join(rel))
            .collect();
        want.sort();
        want.dedup();
        // Every store made, so the argv carries one mount per entry and the comparison is about
        // the list rather than about what this box happens to have.
        for p in &want {
            std::fs::create_dir_all(p).expect("a fake store");
        }
        std::fs::create_dir_all(dir.join("project")).expect("a project");
        let wide = seat::seat_for_level(
            seat::Level::Wide,
            &crate::mode::Mode::WRITES_ALLOWED,
            "this machine",
            &[] as &[&str],
            &home,
            None,
        );
        assert_eq!(
            wide.masked, want,
            "the seat holds the list both renderings read"
        );
        assert!(
            !want.iter().any(|p| p.ends_with(".config/gh")),
            "a top-level seat keeps gh's config, which is what the token it injects is for: {want:?}"
        );
        let view = ViewSpec::project_only(dir.join("project"))
            .with_home(HomeView::Dir(home.clone()))
            .wide()
            .masking(wide.masked.iter().cloned());
        let argv = build_argv_with(
            Path::new("/usr/bin/bwrap"),
            &view,
            &Egress::Denied,
            &ConfinePlan {
                cwd: &dir,
                env: &[],
            },
            Session::Detached,
        );
        let joined = argv.join(" ");
        for p in &want {
            let p = p.display().to_string();
            assert!(
                joined.contains(&format!("--tmpfs {p} --remount-ro {p}")),
                "{p} is on the macOS deny list and not masked here: {joined}"
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The daemon's own socket and the ssh masters are found by scanning; a plain file and a
    /// socket outside the letibot directories are not.
    #[test]
    fn the_daemons_sockets_are_found_in_the_runtime_dirs() {
        let dir = std::env::temp_dir().join(format!("letibot-dsock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let run = dir.join("run/letibot");
        std::fs::create_dir_all(&run).unwrap();
        std::fs::write(run.join("abc.sock"), "").unwrap();
        std::fs::write(run.join("abc.json"), "").unwrap();
        std::fs::write(dir.join("letibot-ssh-1-x-call.sock"), "").unwrap();
        std::fs::write(dir.join("other.sock"), "").unwrap();
        let got = daemon_sockets_in(
            std::slice::from_ref(&run),
            &dir,
            Some(dir.join("named.sock")),
        );
        let has = |p: PathBuf| got.contains(&p) || got.contains(&p.canonicalize().unwrap_or(p));
        assert!(has(run.join("abc.sock")), "{got:?}");
        assert!(has(dir.join("letibot-ssh-1-x-call.sock")), "{got:?}");
        assert!(has(dir.join("named.sock")), "{got:?}");
        assert!(!got.iter().any(|p| p.ends_with("abc.json")), "{got:?}");
        assert!(!got.iter().any(|p| p.ends_with("other.sock")), "{got:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The wide seat's persistence, history and git-config rules, read from the same lists by both
    /// renderings: denied outright on the macOS profile and masked here, and the read-only files
    /// written nowhere in either.
    #[test]
    fn the_wide_seats_persistence_and_history_rules_hold_in_both_renderings() {
        use crate::exec::{seat, seatbelt};
        let dir = std::env::temp_dir().join(format!("letibot-persist-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let home = dir.join("operator");
        std::fs::create_dir_all(home.join(".config/git")).expect("a fake home");
        std::fs::create_dir_all(dir.join("project")).expect("a project");
        std::fs::write(home.join(".gitconfig"), "[user]\n").unwrap();
        std::fs::write(home.join(".config/git/config"), "").unwrap();
        let denied = [
            "Library/LaunchAgents",
            ".zlogin",
            ".zprofile",
            ".bash_profile",
            ".profile",
            ".oh-my-zsh/custom",
            ".zsh_history",
            ".bash_history",
        ];
        let deny = seatbelt::deny_under_home(true);
        for d in denied {
            assert!(deny.contains(&d), "{d} is not on the seat's deny list");
        }
        let mut view = ViewSpec::project_only(dir.join("project"))
            .with_home(HomeView::Dir(home.clone()))
            .wide();
        let wide = seat::seat_for_level(
            seat::Level::Wide,
            &crate::mode::Mode::WRITES_ALLOWED,
            "this machine",
            &[] as &[&str],
            &home,
            None,
        );
        assert!(
            !deny.contains(&".config/gh") && !wide.masked.iter().any(|p| p.ends_with(".config/gh")),
            "gh stays readable in the top-level seat"
        );
        view = view.masking(wide.masked.iter().cloned());
        for p in seat::wide_protected(&home) {
            view = view.protecting(p);
        }
        let argv = build_argv_with(
            Path::new("/usr/bin/bwrap"),
            &view,
            &Egress::Denied,
            &ConfinePlan {
                cwd: &dir,
                env: &[],
            },
            Session::Detached,
        )
        .join(" ");
        for rel in crate::secrets::SEAT_READ_ONLY {
            let p = home.join(rel);
            if p.exists() {
                let p = p.display().to_string();
                assert!(argv.contains(&format!("--ro-bind {p} {p}")), "{p}: {argv}");
            }
        }
        let layout = seatbelt::Layout::of(&view, &home, &dir);
        let profile = seatbelt::render_profile(&view, &Egress::Denied, &layout);
        for rel in crate::secrets::SEAT_READ_ONLY {
            let p = home.join(rel);
            assert!(
                profile.contains(&format!("(subpath \"{}\")", p.display())),
                "{rel} is writable on macOS: {profile}"
            );
        }
        for d in denied {
            assert!(
                layout.deny.contains(&home.join(d)),
                "{d} is not denied on macOS"
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A tight view reaches the operator's home only through a grant, and a grant of a toolchain
    /// directory carries a credential store with it: the store is masked, the toolchain is not, and a
    /// store the view does not reach gets no mount at all.
    #[test]
    fn a_grant_reaches_a_store_and_the_store_is_masked_and_the_toolchain_is_not() {
        use crate::exec::seat;
        let dir = std::env::temp_dir().join(format!("letibot-grantmask-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let home = dir.join("operator");
        let cargo = home.join(".cargo");
        std::fs::create_dir_all(&cargo).expect("a fake toolchain");
        std::fs::create_dir_all(home.join(".ssh")).expect("a fake store");
        std::fs::write(cargo.join("credentials.toml"), "token\n").expect("a fake store");
        std::fs::create_dir_all(dir.join("project")).expect("a project");
        let seat = seat::seat_view(
            &crate::mode::Mode::WRITES_ALLOWED,
            "this machine",
            &[cargo.display().to_string()],
            &home,
        );
        let view = seat.grants.iter().fold(
            ViewSpec::project_only(dir.join("project")).masking(seat.masked.iter().cloned()),
            |v, g| v.granting(g.clone()),
        );
        let argv = build_argv_with(
            Path::new("/usr/bin/bwrap"),
            &view,
            &Egress::Denied,
            &ConfinePlan {
                cwd: &dir,
                env: &[],
            },
            Session::Detached,
        );
        let joined = argv.join(" ");
        let granted = cargo.display().to_string();
        assert!(
            joined.contains(&format!("--ro-bind {granted} {granted}")),
            "the toolchain is bound read-only: {joined}"
        );
        let store = cargo.join("credentials.toml").display().to_string();
        assert!(
            joined.contains(&format!("--dev-bind /dev/null {store}")),
            "the store inside the grant is masked: {joined}"
        );
        assert!(
            !joined.contains(&cargo.join("config.toml").display().to_string()),
            "the toolchain's own config is not the fence's business: {joined}"
        );
        assert!(
            !joined.contains(&home.join(".ssh").display().to_string()),
            "a store the view does not reach gets no mount point: {joined}"
        );
        // And the disclosure says so, in the view's own summary and in the model's brief.
        assert!(
            view.summary().contains("masked inside the grants"),
            "{}",
            view.summary()
        );
        assert!(
            view.summary().contains(&store),
            "the disclosure names the path: {}",
            view.summary()
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unconfined_says_so_rather_than_saying_nothing() {
        let c = Unconfined::because("HostBackend::executable asks for no boundary");
        assert!(
            c.wrap(&ConfinePlan {
                cwd: Path::new("/"),
                env: &[]
            })
            .unwrap()
            .is_empty()
        );
        let d = c.describe();
        assert!(d.contains("NOT CONFINED"), "{d}");
        assert!(d.contains("whole filesystem"), "{d}");
        // It claims no view, so nothing produces an absence note blaming a boundary that is
        // not there.
        assert!(!c.outside_view(Path::new("/home/dead/.ssh/id_rsa")));
        assert!(
            c.absence_notes("cat: /etc/shadow: No such file or directory")
                .is_empty()
        );
    }

    #[test]
    fn the_view_is_lexical_and_the_parent_chain_is_inside_it() {
        let v = ViewSpec::project_only("/home/dead/Projects/letibot");
        assert!(!v.outside(Path::new("/home/dead/Projects/letibot/crates/tools")));
        assert!(v.outside(Path::new("/home/dead/.ssh/id_rsa")));
        assert!(v.outside(Path::new("/home/dead/Projects/other")));
        assert!(v.outside(Path::new("/root/.ssh/id_rsa")));
        assert!(v.outside(Path::new("/etc/shadow")));
        // `/usr` is a system root, so it is in the view and must not produce a note.
        assert!(!v.outside(Path::new("/usr/bin/cc")));
        // The parent chain to the project exists inside as empty directories, so a listing of
        // it succeeds and calling it absent would be a note about something the command sees.
        assert!(!v.outside(Path::new("/home/dead")));
        assert!(!v.outside(Path::new("/home")));
        // A relative path is not this function's question.
        assert!(!v.outside(Path::new("crates/tools")));
    }

    #[test]
    fn a_granted_path_is_in_the_view_and_its_consequence_is_printed() {
        let v = ViewSpec::project_only("/proj").granting(Grant::ReadOnly {
            path: PathBuf::from("/home/dead/.cargo"),
            why: "the crate registry cache, so a build does not re-download".into(),
        });
        assert!(!v.outside(Path::new("/home/dead/.cargo/registry")));
        assert!(v.outside(Path::new("/home/dead/.ssh")));
        let s = v.summary();
        assert!(s.contains("READABLE INTO CONTEXT"), "{s}");
        assert!(s.contains("re-download"), "{s}");
    }

    #[test]
    fn an_absent_path_is_reported_as_absent_from_the_view_and_not_as_missing() {
        // A bare `ENOENT` is read as "the file does not exist", an empty-haystack answer.
        let b = boundary_with(vec![]);
        let notes = b.absence_notes("cat: /home/dead/.ssh/id_rsa: No such file or directory");
        assert_eq!(notes.len(), 1, "{notes:?}");
        let n = &notes[0];
        assert!(n.contains("not in this session's filesystem view"), "{n}");
        assert!(n.contains("/home/dead/.ssh/id_rsa"), "{n}");
        assert!(n.contains("another spelling"), "{n}");
        // The note must not claim the path exists on the host: nothing was stat'ed, and the
        // sentence does not assert it either way.
        assert!(!n.contains("exists on"), "{n}");
    }

    #[test]
    fn a_hidden_file_inside_the_view_is_replaced_by_dev_null_and_one_outside_is_left_alone() {
        let dir = std::env::temp_dir().join(format!("letibot-hidden-argv-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("room-x.sock");
        std::fs::write(&sock, "").unwrap();
        let outside = std::env::temp_dir().join("elsewhere-room.sock");
        let _ = std::fs::write(&outside, "");
        let view = ViewSpec::project_only(&dir)
            .hiding(&sock)
            .hiding("/nope/room.sock");
        let a = build_argv(
            Path::new("/usr/bin/bwrap"),
            &view,
            &Egress::Denied,
            &ConfinePlan {
                cwd: &dir,
                env: &[],
            },
        )
        .join(" ");
        assert!(
            a.contains(&format!("--ro-bind /dev/null {}", sock.display())),
            "{a}"
        );
        assert!(
            !a.contains("/nope/room.sock"),
            "an absent path is not made to appear: {a}"
        );
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&outside);
    }

    #[test]
    fn the_failure_note_names_both_grants_that_would_let_the_path_through() {
        // The card offers a read-only and a read-write answer, so a note naming only the first
        // sends a model that needs to WRITE to ask for a grant that cannot let it through.
        let b = boundary_with(vec![]);
        let notes = b.absence_notes("cat: /home/dead/.cargo/bin/cargo: No such file or directory");
        assert_eq!(notes.len(), 1, "{notes:?}");
        let n = &notes[0];
        assert!(n.contains("`/home/dead/.cargo/bin/cargo`"), "{n}");
        assert!(
            n.contains("`ReadOnly`"),
            "the read-only grant is named: {n}"
        );
        assert!(
            n.contains("`ReadWrite`"),
            "the write spelling the card offers is named too: {n}"
        );
        assert!(n.contains("the transcript"), "{n}");
        assert!(n.contains("the operator's to decide"), "{n}");
    }

    #[test]
    fn the_model_is_told_what_its_view_holds_before_anything_fails() {
        // The failure note alone would leave a hermetic view reading as a broken
        // toolchain. The brief is what the prompt carries at open: the paths, and what `~` is.
        let b = boundary_with(vec![]);
        let t = b.model_brief();
        assert!(t.contains("`/proj`"), "the project is named: {t}");
        assert!(t.contains("/usr"), "the system roots are named: {t}");
        assert!(
            t.contains(TMPFS_HOME),
            "the home inside the view is named: {t}"
        );
        assert!(
            t.contains("not this machine's"),
            "and it says the home is the view's own: {t}"
        );
        assert!(
            t.contains("absent"),
            "a path outside is absent, not forbidden: {t}"
        );
        assert!(
            t.contains("no network") && t.contains("no routes"),
            "egress is stated: {t}"
        );
        assert!(t.contains("`ReadOnly`") && t.contains("`ReadWrite`"), "{t}");
        assert!(t.contains("the operator's to decide"), "{t}");
        // Nothing is granted in a plain view, and a brief that listed one would be a claim.
        assert!(!t.contains("Bound in"), "{t}");

        // A path bound in on purpose is in the brief, with the consequence the grant carries.
        let granted = Boundary {
            view: ViewSpec::project_only("/proj").granting(Grant::ReadOnly {
                path: PathBuf::from("/home/dead/.cargo"),
                why: "the registry, so a build does not re-download".into(),
            }),
            ..boundary_with(vec![])
        };
        let t = granted.model_brief();
        assert!(t.contains("/home/dead/.cargo"), "{t}");
        assert!(t.contains("READABLE INTO CONTEXT"), "{t}");
    }

    /// The wide seat's `$HOME` is the operator's own, and the brief said letibot owned it: a model
    /// reading that treats `~` as scratch and somebody else's files as its own to tidy.
    #[test]
    fn the_brief_says_when_the_home_is_the_operators_own() {
        let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
            return;
        };
        let wide = Boundary {
            view: ViewSpec::project_only("/proj").with_home(HomeView::Dir(home.clone())),
            ..boundary_with(vec![])
        };
        let t = wide.model_brief();
        assert!(
            t.contains("THIS MACHINE'S OWN HOME"),
            "the wide home must not read as this view's own: {t}"
        );
        assert!(!t.contains("a directory letibot owns"), "{t}");
        assert!(t.contains("writable"), "{t}");

        // And a home the daemon made for the session still says so.
        let narrow = Boundary {
            view: ViewSpec::project_only("/proj")
                .with_home(HomeView::Dir(home.join("letibot-home"))),
            ..boundary_with(vec![])
        };
        let t = narrow.model_brief();
        assert!(t.contains("a directory letibot owns"), "{t}");
        assert!(!t.contains("THIS MACHINE'S OWN HOME"), "{t}");
    }

    #[test]
    fn a_session_tmp_is_bound_at_tmp_and_nothing_replaces_it() {
        // The two-call scratch idiom: a file one call wrote was gone in the next, and nothing
        // announces it. This is the argv half, `--tmpfs /tmp` gone and `--bind <dir> /tmp` there.
        let dir = PathBuf::from("/var/lib/letibot/scratch-1/tmp/s-a");
        let a = build_argv(
            Path::new("/usr/bin/bwrap"),
            &ViewSpec::project_only("/proj").with_tmp(TmpView::Session(dir.clone())),
            &Egress::Denied,
            &ConfinePlan {
                cwd: Path::new("/proj"),
                env: &[],
            },
        );
        let joined = a.join(" ");
        assert!(!joined.contains("--tmpfs /tmp"), "{joined}");
        assert!(
            joined.contains(&format!("--bind {} /tmp", dir.display())),
            "the session's own directory, bound at /tmp: {joined}"
        );
        // `/var/tmp` and `/run` are still fresh filesystems: the one thing `/var/tmp` means on
        // a host is that it survives a reboot, and `/run` holds the agent socket and the home.
        assert!(joined.contains("--tmpfs /var/tmp"), "{joined}");
        assert!(joined.contains("--tmpfs /run"), "{joined}");

        // And the default view is unchanged: one fresh tmpfs at `/tmp`, bound to nothing.
        let d = build_argv(
            Path::new("/usr/bin/bwrap"),
            &ViewSpec::project_only("/proj"),
            &Egress::Denied,
            &ConfinePlan {
                cwd: Path::new("/proj"),
                env: &[],
            },
        )
        .join(" ");
        assert!(d.contains("--tmpfs /tmp"), "{d}");
    }

    #[test]
    fn a_replaced_tmp_says_the_session_owns_it_and_not_that_it_starts_empty() {
        // The disclosure and the absence note both read off the view, so a `/tmp` that outlives
        // a command must not describe itself as one that does not.
        let dir = PathBuf::from("/var/lib/letibot/scratch-1/tmp/s-a");
        let v = ViewSpec::project_only("/proj").with_tmp(TmpView::Session(dir.clone()));
        let Presence::Replaced { what } = v.classify(Path::new("/tmp/host-file")) else {
            panic!("a session /tmp is still not the host's /tmp");
        };
        assert!(what.contains("this session's own"), "{what}");
        assert!(what.contains("survives between the calls"), "{what}");
        assert!(
            v.summary().contains(&dir.display().to_string()),
            "{}",
            v.summary()
        );
        assert!(
            v.summary().contains("there for the next"),
            "{}",
            v.summary()
        );
        // The note an `ENOENT` under it produces follows the same fact.
        let b = Bwrap {
            helper: PathBuf::from("/usr/bin/bwrap"),
            boundary: Boundary {
                view: v,
                ..boundary_with(vec![])
            },
        };
        let notes = b.absence_notes("cat: /tmp/gone.txt: No such file or directory");
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(
            notes[0].contains("survives between the calls"),
            "{}",
            notes[0]
        );
        assert!(!notes[0].contains("starts empty"), "{}", notes[0]);
    }

    #[test]
    fn a_replaced_root_is_its_own_answer_and_not_either_of_the_other_two() {
        // The middle answer, and the note that would have been wrong: `/tmp` exists inside, so
        // calling it outside claims it cannot be reached; it is a fresh tmpfs, so calling it
        // inside leaves a bare `ENOENT` about a host file that really is invisible.
        let v = ViewSpec::project_only("/proj");
        assert_eq!(v.classify(Path::new("/proj/src")), Presence::Inside);
        assert_eq!(v.classify(Path::new("/opt/thing")), Presence::Outside);
        assert!(matches!(
            v.classify(Path::new("/tmp/host-file")),
            Presence::Replaced { .. }
        ));
        assert!(matches!(
            v.classify(Path::new("/proc/1234/cmdline")),
            Presence::Replaced { .. }
        ));
        // A project under `/tmp` is Inside, not Replaced: the bound roots are checked first,
        // and getting that order wrong would tell every fixture its workspace was a tmpfs.
        let under_tmp = ViewSpec::project_only("/tmp/letibot-fixture");
        assert_eq!(
            under_tmp.classify(Path::new("/tmp/letibot-fixture/src")),
            Presence::Inside
        );

        let b = boundary_with(vec![]);
        let notes = b.absence_notes("cat: /tmp/host-file: No such file or directory");
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("fresh tmpfs"), "{}", notes[0]);
        assert!(notes[0].contains("starts empty"), "{}", notes[0]);
        // And it must NOT say the path is outside the view, because it is not.
        assert!(
            !notes[0].contains("not in this session's filesystem view"),
            "{}",
            notes[0]
        );
    }

    #[test]
    fn a_miss_inside_the_view_gets_no_boundary_note() {
        // The false positive that would matter: a genuinely missing project file blamed on the
        // boundary sends the model looking for a grant it does not need.
        let b = boundary_with(vec![]);
        assert!(
            b.absence_notes("cat: /proj/src/nope.rs: No such file or directory")
                .is_empty()
        );
        assert!(b.absence_notes("hello, nothing is missing here").is_empty());
        // And a relative path, which is what most tools print.
        assert!(
            b.absence_notes("cat: src/nope.rs: No such file or directory")
                .is_empty()
        );
    }

    #[test]
    fn the_absence_note_is_bounded_like_a_success_body() {
        // A miss must not produce an unbounded reply: see [`MAX_ABSENCE_PATHS`].
        let b = boundary_with(vec![]);
        let mut text = String::new();
        for i in 0..50 {
            text.push_str(&format!("cat: /opt/secret{i}: No such file or directory\n"));
        }
        let notes = b.absence_notes(&text);
        assert_eq!(notes.len(), 1);
        // Each path is named in the list and once more by its own grant sentence.
        let named: std::collections::BTreeSet<&str> = notes[0]
            .split('`')
            .filter(|t| t.starts_with("/opt/secret"))
            .collect();
        assert!(named.len() <= MAX_ABSENCE_PATHS, "named {}", named.len());
        assert!(
            notes[0].matches("/opt/secret").count() <= 2 * MAX_ABSENCE_PATHS,
            "{}",
            notes[0]
        );
    }

    /// A boundary over a real project directory, so the view's roots are paths a `stat` agrees
    /// with rather than `/proj`. Its SYSTEM roots are empty on purpose: nothing of this box's own
    /// is in the view, so a program that really exists here really is outside it.
    fn a_boundary_over(project: &Path) -> Boundary {
        Boundary {
            view: ViewSpec {
                system: Vec::new(),
                ..ViewSpec::project_only(project)
            },
            ..boundary_with(vec![])
        }
    }

    /// A real directory this view does not contain, proved rather than hoped for: the note is about
    /// a path that exists outside, and a fixture that cannot find one must fail here rather than
    /// assert nothing and be counted as a pass.
    fn a_dir_outside(view: &ViewSpec) -> PathBuf {
        let found = ["sh", "ls", "cat"]
            .into_iter()
            .filter_map(on_path)
            .filter_map(|p| p.parent().map(Path::to_path_buf))
            .find(|d| view.outside(d));
        match found {
            Some(d) => d,
            None => panic!(
                "no directory on this PATH is outside the fixture's view, so the case cannot be \
                 built here: PATH is {:?}",
                std::env::var_os("PATH")
            ),
        }
    }

    /// A program this box really has and the fixture's view really does not contain, with the word a
    /// shell prints for it. Proved for [`a_dir_outside`]'s reason.
    fn a_program_outside(view: &ViewSpec) -> (String, PathBuf) {
        let found = ["cargo", "sh", "ls"]
            .into_iter()
            .find_map(|n| on_path(n).map(|p| (n.to_string(), p)))
            .filter(|(_, p)| view.outside(p));
        match found {
            Some(pair) => pair,
            None => panic!(
                "no program on this PATH is outside the fixture's view, so the case cannot be \
                 built here: PATH is {:?}",
                std::env::var_os("PATH")
            ),
        }
    }

    /// The first hit for `name` on this process's `PATH`, resolved the way the note resolves it.
    fn on_path(name: &str) -> Option<PathBuf> {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .map(|d| d.join(name))
            .find(|p| p.is_file())
    }

    /// And the guard that makes the two above real: a view that contains every program on this
    /// PATH leaves nothing to assert, and that is a failure rather than a pass.
    #[test]
    #[should_panic(expected = "no program on this PATH is outside the fixture's view")]
    fn a_fixture_that_finds_no_program_outside_fails_instead_of_passing() {
        let mut view = ViewSpec::project_only(std::env::temp_dir());
        for dir in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
            view = view.granting(Grant::ReadOnly {
                path: dir,
                why: "the fixture puts everything this process can run in the view".into(),
            });
        }
        a_program_outside(&view);
    }

    #[test]
    fn an_absence_note_names_the_grant_that_would_allow_the_path_in() {
        // The grant is the only thing that changes the answer, so a note that stops at "absent"
        // leaves the model with no move but to ask again.
        let project = std::env::temp_dir().join(format!("letibot-view-{}", std::process::id()));
        std::fs::create_dir_all(&project).expect("fixture project");
        let b = a_boundary_over(&project);
        let real = a_dir_outside(&b.view);
        let file = real.join("config.toml");
        let said = format!("cat: {}: No such file or directory", file.display());

        let notes = b.absence_notes(&said);
        assert_eq!(notes.len(), 1, "{notes:?}");
        let n = &notes[0];
        assert!(n.contains(&file.display().to_string()), "{n}");
        assert!(n.contains("`ReadOnly` binds"), "{n}");
        assert!(n.contains("reach the transcript"), "{n}");

        // A granted path is in the view, so there is nothing absent to report about it.
        let granted = Boundary {
            view: ViewSpec::project_only(&project).granting(Grant::ReadOnly {
                path: real.clone(),
                why: "the fixture's own directory".into(),
            }),
            ..boundary_with(vec![])
        };
        assert!(granted.absence_notes(&said).is_empty());
    }

    #[test]
    fn a_missing_program_is_named_as_the_path_the_view_does_not_contain() {
        // `cargo: command not found` names no path at all, so the scan found nothing and the
        // model was left reading the boundary as a broken toolchain.
        let project = std::env::temp_dir().join(format!("letibot-view-cmd-{}", std::process::id()));
        std::fs::create_dir_all(&project).expect("fixture project");
        let b = a_boundary_over(&project);
        let (name, program) = a_program_outside(&b.view);
        for said in [
            format!("{name}: command not found"),
            // What this box's `/bin/sh` really prints, measured rather than assumed.
            format!("/bin/sh: 1: {name}: not found"),
        ] {
            let notes = b.absence_notes(&said);
            assert_eq!(notes.len(), 1, "{said}: {notes:?}");
            let n = &notes[0];
            assert!(n.contains(&program.display().to_string()), "{said}: {n}");
            assert!(n.contains("`ReadOnly` binds"), "{said}: {n}");
            assert!(n.contains("reach the transcript"), "{said}: {n}");
        }
        // A name this host does not have is a typo, and a typo must not become a claim about
        // the boundary.
        assert!(
            b.absence_notes("nosuchtool_xyz: command not found")
                .is_empty()
        );
    }

    /// The note promises a grant, so the door that raises grants must name the same path. A
    /// missing program folded into the note and not into the ask leaves the model waiting for a
    /// decision nobody was asked for.
    #[test]
    fn a_missing_program_is_a_path_the_grant_card_is_raised_for() {
        let project =
            std::env::temp_dir().join(format!("letibot-view-grant-{}", std::process::id()));
        std::fs::create_dir_all(&project).expect("fixture project");
        let b = a_boundary_over(&project);
        let (name, program) = a_program_outside(&b.view);
        for said in [
            format!("{name}: command not found"),
            // What this box's `/bin/sh` really prints, measured rather than assumed. It names the
            // shell too, so the claim is that the program is one of the paths asked about.
            format!("/bin/sh: 1: {name}: not found"),
        ] {
            let asked = b.outside_paths(&said);
            assert!(
                asked.contains(&program),
                "{said}: the note names {}, so the grant door has to ask about it: it asked \
                 about {asked:?}",
                program.display()
            );
        }
        // A typo is not a path: nothing is asked about it, and nothing is said.
        assert!(
            b.outside_paths("nosuchtool_xyz: command not found")
                .is_empty()
        );
    }

    #[test]
    fn an_agent_socket_is_forwardable_and_a_key_file_is_not() {
        // The subtle credential case as a test: the mechanism accepts a socket and refuses
        // anything else, because usable-without-readable is a property of the socket.
        let dir = std::env::temp_dir().join(format!("letibot-agent-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let key = dir.join("id_rsa");
        std::fs::write(&key, "-----BEGIN OPENSSH PRIVATE KEY-----\n").expect("fixture key");

        // Not a socket: refused, with the mechanism named.
        // SAFETY: single-threaded test scope; the variable is restored below.
        unsafe { std::env::set_var("SSH_AUTH_SOCK", &key) };
        let e = Grant::agent_from_env("authorised ssh").unwrap_err();
        let m = format!("{e}");
        assert!(m.contains("not a socket"), "{m}");
        assert!(m.contains("Nothing was bound"), "{m}");

        // Absent: refused, and the refusal names ssh-agent rather than a bind.
        unsafe { std::env::remove_var("SSH_AUTH_SOCK") };
        let e = Grant::agent_from_env("authorised ssh").unwrap_err();
        let m = format!("{e}");
        assert!(m.contains("ssh-add"), "{m}");
        assert!(m.contains("never the key"), "{m}");
        // And it names the SECOND decision the authorised case needs, in words a model can act
        // on rather than the enum it is built from.
        assert!(m.contains("second decision"), "{m}");
        for internal in ["Egress::Host", "§"] {
            assert!(!m.contains(internal), "{m}");
        }

        // A real socket is accepted and bound at a fixed path, so the operator's
        // `/run/user/<uid>` path does not travel.
        let sock = dir.join("agent.sock");
        let _ = std::fs::remove_file(&sock);
        let l = std::os::unix::net::UnixListener::bind(&sock).expect("bind agent socket");
        unsafe { std::env::set_var("SSH_AUTH_SOCK", &sock) };
        let g = Grant::agent_from_env("the operator said `ssh build-box`").expect("socket");
        assert!(matches!(g, Grant::AgentSocket { .. }));
        assert!(g.line().contains("NOT readable"), "{}", g.line());
        assert!(g.line().contains(AGENT_SOCKET), "{}", g.line());
        drop(l);
        unsafe { std::env::remove_var("SSH_AUTH_SOCK") };
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_argv_puts_the_project_in_and_leaves_the_home_directory_out() {
        let view = ViewSpec::project_only("/home/dead/Projects/letibot");
        let a = build_argv(
            Path::new("/usr/bin/bwrap"),
            &view,
            &Egress::Denied,
            &ConfinePlan {
                cwd: Path::new("/home/dead/Projects/letibot/crates"),
                env: &[("CARGO_TERM_COLOR".into(), "never".into())],
            },
        );
        let joined = a.join(" ");
        for f in [
            "--unshare-user",
            "--unshare-pid",
            "--unshare-net",
            "--unshare-ipc",
            "--unshare-uts",
            "--unshare-cgroup",
            "--die-with-parent",
            "--new-session",
            "--clearenv",
        ] {
            assert!(joined.contains(f), "missing {f} in {joined}");
        }
        // The project is bound read-write at its own path.
        let i = a.iter().position(|x| x == "--bind").expect("a --bind");
        assert_eq!(a[i + 1], "/home/dead/Projects/letibot");
        assert_eq!(a[i + 2], "/home/dead/Projects/letibot");
        // `$HOME` is NOT the operator's home, and the operator's home is not bound.
        let home = a.iter().position(|x| x == "HOME").expect("HOME is set");
        assert_eq!(a[home + 1], TMPFS_HOME);
        assert!(
            !a.iter().any(|x| x == "/home/dead"),
            "the operator's home must never be an argument: {joined}"
        );
        // The caller's env survives `--clearenv`, because it is re-declared.
        assert!(joined.contains("CARGO_TERM_COLOR never"), "{joined}");
        // The command is separated from the options by `--`, so a command beginning with a
        // dash is a command.
        assert_eq!(a.last().map(String::as_str), Some("--"));
    }

    #[test]
    fn declared_egress_drops_the_network_unshare_and_binds_the_resolver() {
        let view = ViewSpec::project_only("/proj");
        let denied = build_argv(
            Path::new("/usr/bin/bwrap"),
            &view,
            &Egress::Denied,
            &ConfinePlan {
                cwd: Path::new("/proj"),
                env: &[],
            },
        );
        let host = build_argv(
            Path::new("/usr/bin/bwrap"),
            &view,
            &Egress::Host {
                why: "authorised".into(),
            },
            &ConfinePlan {
                cwd: Path::new("/proj"),
                env: &[],
            },
        );
        assert!(denied.iter().any(|a| a == "--unshare-net"));
        assert!(!host.iter().any(|a| a == "--unshare-net"));
        // The resolver's files come with the network and only with it.
        for p in NETWORK_ROOTS {
            assert!(host.iter().any(|a| a == p), "{p} is bound with the network");
            assert!(
                !denied.iter().any(|a| a == p),
                "{p} is not bound without it"
            );
        }
        // Those two differences, and only those: everything else is identical.
        let d: Vec<&String> = denied.iter().filter(|a| *a != "--unshare-net").collect();
        let mut h: Vec<&String> = Vec::new();
        let mut i = 0;
        while i < host.len() {
            if host[i] == "--ro-bind-try" && NETWORK_ROOTS.contains(&host[i + 1].as_str()) {
                i += 3;
                continue;
            }
            h.push(&host[i]);
            i += 1;
        }
        assert_eq!(d, h);
    }

    #[test]
    fn a_boundary_a_child_can_unshare_out_of_is_not_confined() {
        // An open seal fails `complete()` rather than merely making the boundary partial:
        // "partial" implies the rest still holds, and this is what makes the rest hold.
        let mut b = boundary_with(vec![]);
        assert!(b.complete());
        b.seals.insert(
            SealKind::NestedNamespaces,
            Seal::Open {
                why: "`unshare -U true` inside the boundary SUCCEEDED".into(),
            },
        );
        assert!(!b.complete(), "an open seal is not a boundary");
        let d = b.describe();
        assert!(d.contains("NOT CONFINED"), "{d}");
        assert!(d.contains("seal is open"), "{d}");
        assert!(d.contains("**OPEN**"), "{d}");
        assert!(d.contains("cannot `unshare` itself back out"), "{d}");
    }

    #[test]
    fn a_seal_that_could_not_be_tested_is_partial_and_never_held() {
        // An absent measurement is not a passing one: it still confines, because the
        // namespaces were measured, but the disclosure must not read as fully checked.
        let mut b = boundary_with(vec![]);
        b.seals.insert(
            SealKind::NestedNamespaces,
            Seal::Unverified {
                why: "`unshare` is not in this view".into(),
            },
        );
        assert!(b.complete(), "unknown is not open");
        assert!(b.partial(), "and unknown is not held either");
        let d = b.describe();
        assert!(d.contains("PARTIALLY"), "{d}");
        assert!(d.contains("UNVERIFIED"), "{d}");
    }

    #[test]
    fn the_seals_are_tested_and_a_missing_verdict_is_unknown_not_ok() {
        // The parse, which is where "an absent measurement counts as a pass" would creep in.
        let held = parse_seals("ns user user:[1]\nseal nested held\nseal nnp 1\n");
        assert!(held[&SealKind::NestedNamespaces].held());
        assert!(held[&SealKind::NoNewPrivs].held());

        let open = parse_seals("seal nested open\nseal nnp 0\n");
        assert!(matches!(
            open[&SealKind::NestedNamespaces],
            Seal::Open { .. }
        ));
        assert!(matches!(open[&SealKind::NoNewPrivs], Seal::Open { .. }));

        // Nothing at all: unknown, both of them.
        let silent = parse_seals("ns user user:[1]\n");
        for k in SealKind::ALL {
            assert!(
                matches!(silent[&k], Seal::Unverified { .. }),
                "{k} must be unknown, not held"
            );
        }
        // `unshare` absent from the view is its own reason, and it names the independent
        // mechanism without claiming that mechanism as a measurement.
        let no_tool = parse_seals("seal nested unverified\nseal nnp 1\n");
        let Seal::Unverified { why } = &no_tool[&SealKind::NestedNamespaces] else {
            panic!("expected unverified");
        };
        assert!(why.contains("assert-userns-disabled"), "{why}");
        assert!(why.contains("not a measurement"), "{why}");
    }

    #[test]
    fn the_argv_seals_the_boundary_and_asserts_the_seal_took() {
        let a = build_argv(
            Path::new("/usr/bin/bwrap"),
            &ViewSpec::project_only("/proj"),
            &Egress::Denied,
            &ConfinePlan {
                cwd: Path::new("/proj"),
                env: &[],
            },
        );
        assert!(a.iter().any(|x| x == "--disable-userns"), "{a:?}");
        assert!(
            a.iter().any(|x| x == "--assert-userns-disabled"),
            "the request must be checked by the helper too: {a:?}"
        );
    }

    #[test]
    fn a_helper_setup_failure_is_not_reported_as_the_commands_answer() {
        // The shape that is easy to miss: the wrong answer here is not `ok`, it is "the
        // command ran and exited non-zero".
        let dir = std::env::temp_dir();
        let b = Bwrap {
            helper: PathBuf::from("/usr/bin/bwrap"),
            boundary: Boundary {
                view: ViewSpec::project_only(&dir),
                ..boundary_with(vec![])
            },
        };
        let n = b
            .launcher_failure("bwrap: Can't bind mount /nope on /nope: No such file\n")
            .expect("a bwrap diagnostic is a launcher failure");
        assert!(n.contains("NOT the command's answer"), "{n}");
        assert!(n.contains("may never have run"), "{n}");
        // And ordinary output is not misread as one.
        assert!(
            b.launcher_failure("error: could not compile `x`\n")
                .is_none()
        );
        assert!(b.launcher_failure("").is_none());
    }

    #[test]
    fn a_readback_that_matches_the_harness_is_not_a_boundary() {
        // The parse and the comparison, without a helper: this is what decides `Entered` from
        // `NotEntered`, and it is what would let a hard-coded `confined` through if wrong.
        let inside = parse_readback("ns user user:[42]\nns mnt mnt:[43]\nnoise\nns pid\n");
        assert_eq!(inside.get("user"), Some(&"user:[42]".to_string()));
        assert_eq!(inside.get("mnt"), Some(&"mnt:[43]".to_string()));
        assert_eq!(
            inside.get("pid"),
            None,
            "a line with no inode is not a reading"
        );
        assert!(parse_readback("total nonsense").is_empty());
    }

    #[test]
    fn a_missing_helper_is_a_refusal_that_names_the_kernel_policy_too() {
        // The refusal has to be actionable: the interesting half is not "install bubblewrap"
        // but why a direct unshare is not the answer.
        // SAFETY: single-threaded test scope; restored below.
        unsafe { std::env::set_var(BWRAP_ENV, "/nonexistent/bwrap") };
        let e = find_helper().unwrap_err();
        unsafe { std::env::remove_var(BWRAP_ENV) };
        let m = format!("{e}");
        assert!(m.contains("/nonexistent/bwrap"), "{m}");
        assert!(m.contains("was NOT run"), "{m}");
    }
}
