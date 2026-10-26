//! Remote hosts — a session whose **tools** run on another machine, over ssh.
//!
//! `docs/remote-hosts.md` is the contract. The shape it names is a placement, exactly
//! as a firecode VM is one ([`crate::firecode`]): the session's harness, its model
//! connection, its store rows and its gate stay in the local daemon, and its tool
//! calls — `bash`, `read`, `write`, `edit`, `grep`, `glob`, jobs — execute there
//! through an [`ExecBackend`] that speaks ssh where firecode's speaks `firecode in`.
//!
//! Three consequences, and each is the reason the choice was made:
//!
//! * **No letibot on the remote.** It needs `sshd`, `bash`, `git` and coreutils
//!   (`base64`, `mv`, `mkdir`, `wc`, `kill`). Nothing is installed or built there.
//! * **No keys leave this box.** Provider keys, the store and the corpus stay local;
//!   the remote sees commands and files, never a model call.
//! * **Every existing piece works unchanged** — `task_result`, activity, the
//!   bridge's `wait`, the todo board, compaction, resume — because they are above
//!   the backend.
//!
//! # What is *not* true of it
//!
//! The remote host is not a boundary the way a VM is. It is the operator's own
//! machine with the operator's own files, so a remote session gets the **host
//! gate** — the modes, the always-ask list, the corpus — with the host named in
//! every card and in the session's placement line. [`SshBackend::describe`] says
//! `NOT CONFINED` for the same reason [`crate::backend::HostBackend::executable`]
//! does: a disclosure that lists only the guarantees reads as a claim about the rest.
//!
//! # The tree, by git, both ways
//!
//! The remote works on a **checkout of the repository**, never on a copy of the
//! local folder. On placement the local `HEAD` goes over as a `git bundle`, lands in
//! a bare mirror at `<root>/<repo>.git` (created on first use) and is checked out as
//! a worktree at `<root>/<repo>/<short id>` on the branch `remote/<short id>`. When
//! the session ends its branch comes back with `git fetch` over ssh into the local
//! repository as `remote/<host>/<short id>`, and that branch is the result.
//!
//! Uncommitted local changes are **not** carried: [`SshBackend::placement_notes`]
//! lists them and says so, because a tree that silently differs from the one the
//! operator is looking at is worse than a refusal. A folder that is not a git
//! repository is refused, and names the fix.
//!
//! # Processes
//!
//! A foreground command is `ssh host 'cd <worktree> && bash -lc …'` and its lifetime
//! is the ssh call. A background job is different in kind, and [`RemoteProcesses`]
//! is why: it is started under `setsid` with its output in
//! `<worktree>/.letibot-remote/jobs/<id>.out`, its pid in `<id>.pid` and its exit
//! status in `<id>.exit`, so it **outlives the ssh connection and this Mac
//! sleeping**; `job_output` reads the file, `job_kill` kills the remote process
//! group, and a daemon that restarts finds the job again from those files.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use letibot_transcript::Backgrounding;

use crate::backend::{BackendError, Command, DirEntry, ExecBackend, Output};
use crate::exec::{
    ExecError, JobId, JobState, JobView, MemoryCap, OutputSlice, ProcessHost, Promotion, Protected,
    Reaped, Reaping, ScopeId, ScopeKind, SpawnRequest, Waited,
};

// ---------------------------------------------------------------------------
// hosts.toml
// ---------------------------------------------------------------------------

/// One machine the operator lets a session run on.
///
/// Read from **the operator's own file and never a project's** — the contract's
/// words, and the reason is a permission one: a repository must not be able to name
/// a machine to run on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Host {
    /// The name used everywhere else — `--host lab2x1`, `where: "host:lab2x1"`.
    pub name: String,
    /// What `ssh` connects to. Defaults to the name, which is what an alias in the
    /// ssh client's own config needs and is the common case.
    pub hostname: String,
    pub user: Option<String>,
    pub key: Option<PathBuf>,
    /// Where remote worktrees live, as the *remote* shell writes it. `~` is expanded
    /// there, once, at placement ([`SshBackend::place`]) — so the paths this backend
    /// hands around are absolute and mean the same thing to `git fetch` a minute later.
    pub root: String,
}

impl Host {
    /// `user@hostname`, or the bare name — the argument ssh takes.
    pub fn destination(&self) -> String {
        match &self.user {
            Some(u) => format!("{u}@{}", self.hostname),
            None => self.hostname.clone(),
        }
    }
}

/// **The operator's hosts**, from `~/.config/letibot/hosts.toml`.
///
/// ```toml
/// [host.lab2x1]
/// hostname = "lab2x1"
/// user     = "dead"
/// key      = "~/.config/letibot/keys/lab2x1"
/// root     = "~/letibot-remote"
/// ```
///
/// A missing file means no hosts, and that is not a fault: a box that places
/// nothing remotely is every box until somebody writes the file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hosts {
    hosts: BTreeMap<String, Host>,
}

/// Where the operator's hosts are written. `$LETIBOT_HOSTS` is a **test's** door and
/// nothing else's: it is an environment variable, and a project that could set one
/// could name a machine, which is the whole thing this file exists to prevent.
pub fn hosts_file() -> PathBuf {
    if let Some(p) = std::env::var_os("LETIBOT_HOSTS") {
        return PathBuf::from(p);
    }
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_default()
        .join("letibot")
        .join("hosts.toml")
}

impl Hosts {
    /// The operator's hosts. A missing file is no hosts; an unreadable one is no hosts
    /// **and a line on stderr**, because a `--host lab2x1` that fails because of a typo
    /// in a file the operator forgot they wrote is a refusal that has to name the file.
    pub fn load() -> Hosts {
        Self::load_from(hosts_file())
    }

    pub fn load_from(file: impl AsRef<Path>) -> Hosts {
        let file = file.as_ref();
        match std::fs::read_to_string(file) {
            Ok(text) => match Hosts::parse(&text) {
                Ok(h) => h,
                Err(why) => {
                    eprintln!("letibot: {}: {why}", file.display());
                    Hosts::default()
                }
            },
            Err(_) => Hosts::default(),
        }
    }

    /// The same reader `providers.toml` has: `[section]` lines and `key = value`, one
    /// section per host under `host.`. Deliberately the same shape and the same
    /// tolerance — two config files on one box read two different ways is a rule
    /// nobody can hold in their head.
    pub fn parse(text: &str) -> Result<Hosts, String> {
        let mut sections: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        let mut current: Option<String> = None;
        for (n, raw) in text.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            if let Some(inner) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                current = Some(inner.trim().trim_matches('"').to_string());
                continue;
            }
            let Some((k, v)) = line.split_once('=') else {
                return Err(format!(
                    "line {}: expected `key = value` or `[section]`",
                    n + 1
                ));
            };
            let Some(sec) = &current else {
                return Err(format!(
                    "line {}: `{}` before any [section]",
                    n + 1,
                    k.trim()
                ));
            };
            sections
                .entry(sec.clone())
                .or_default()
                .insert(k.trim().to_string(), v.trim().trim_matches('"').to_string());
        }
        let mut hosts = BTreeMap::new();
        for (sec, kv) in sections {
            let Some(name) = sec.strip_prefix("host.") else {
                continue;
            };
            let name = name.trim_matches('"').to_string();
            let get = |k: &str| kv.get(k).map(|v| v.trim()).filter(|v| !v.is_empty());
            let key = get("key").map(|k| expand_tilde(Path::new(k)));
            hosts.insert(
                name.clone(),
                Host {
                    hostname: get("hostname").unwrap_or(&name).to_string(),
                    user: get("user").map(str::to_string),
                    key,
                    root: get("root").unwrap_or("~/letibot-remote").to_string(),
                    name,
                },
            );
        }
        Ok(Hosts { hosts })
    }

    pub fn get(&self, name: &str) -> Option<&Host> {
        self.hosts.get(name).or_else(|| {
            self.hosts
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, h)| h)
        })
    }

    pub fn names(&self) -> Vec<String> {
        self.hosts.keys().cloned().collect()
    }

    pub fn is_empty(&self) -> bool {
        self.hosts.is_empty()
    }

    /// **What a wrong host name is told.** The known names, because a refusal a caller
    /// cannot act on is the refusal that costs an hour — and "no host named `lab2`"
    /// when the file says `lab2x1` is exactly that refusal.
    pub fn refusal(&self, name: &str) -> String {
        let known = self.names();
        let list = if known.is_empty() {
            "the file has no `[host.…]` blocks at all".to_string()
        } else {
            format!("it knows: {}", known.join(", "))
        };
        format!(
            "there is no host `{name}` in {} — {list}. Nothing was placed. A host is \
             declared by the operator, in that file and never in a project: \
             `[host.{name}]` with `hostname` (what ssh connects to), `user`, `key` and \
             `root` (where remote worktrees live, default `~/letibot-remote`).",
            hosts_file().display()
        )
    }
}

/// `~/x` in a key path, expanded **here**, because it names a file on this machine.
fn expand_tilde(p: &Path) -> PathBuf {
    let s = p.display().to_string();
    match (s.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => p.to_path_buf(),
    }
}

// ---------------------------------------------------------------------------
// The ssh channel
// ---------------------------------------------------------------------------

/// **One host's ssh connection**, with the flags the contract names.
///
/// `-i key -o IdentitiesOnly=yes -o BatchMode=yes`: a host that asks for a password
/// or a host-key confirmation is **refused by name**, never prompted for — a daemon
/// has no one at a terminal, and a prompt it cannot answer is a call that hangs until
/// its deadline. `ControlMaster`/`ControlPath` keeps one TCP connection per host, so a
/// command costs a round trip rather than a handshake; the master is dropped at
/// [`Ssh::exit_master`].
///
/// The program is a field (`$LETIBOT_SSH` by default) so a test can substitute a fake
/// that runs the command locally; nothing else about this file changes for it.
pub struct Ssh {
    host: Host,
    program: PathBuf,
    control: PathBuf,
}

impl Ssh {
    pub fn new(host: Host) -> Ssh {
        let program = ssh_program_default();
        // **`/tmp`, not `$TMPDIR`.** A unix socket path is capped at ~104 bytes, and
        // macOS's per-user temp directory is already most of that.
        let control = PathBuf::from("/tmp").join(format!(
            "letibot-ssh-{}-{}.sock",
            std::process::id(),
            host.name
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                .collect::<String>()
        ));
        Ssh {
            host,
            program,
            control,
        }
    }

    pub fn with_program(mut self, program: impl Into<PathBuf>) -> Ssh {
        self.program = program.into();
        self
    }

    pub fn host(&self) -> &Host {
        &self.host
    }

    pub fn program(&self) -> &Path {
        &self.program
    }

    pub fn destination(&self) -> String {
        self.host.destination()
    }

    /// The flags, and nothing else — what `git` needs for `core.sshCommand`, where the
    /// destination and the command are git's to append.
    pub fn flags(&self) -> Vec<String> {
        let mut v = Vec::new();
        if let Some(key) = &self.host.key {
            v.push("-i".into());
            v.push(key.display().to_string());
        }
        let control = self.control.display().to_string();
        for (k, val) in [
            ("IdentitiesOnly", "yes".to_string()),
            ("BatchMode", "yes".to_string()),
            (
                "StrictHostKeyChecking",
                std::env::var("LETIBOT_SSH_HOSTKEY").unwrap_or_else(|_| "accept-new".into()),
            ),
            ("ControlMaster", "auto".to_string()),
            ("ControlPersist", "120".to_string()),
            ("ControlPath", control),
            ("ConnectTimeout", "20".to_string()),
        ] {
            v.push("-o".into());
            v.push(format!("{k}={val}"));
        }
        v
    }

    /// The argv that runs `command` on the remote: flags, destination, command — the
    /// whole of the ssh policy in one place, and the one thing a test's fake has to
    /// tolerate.
    pub fn argv(&self, command: &str) -> Vec<String> {
        let mut v = self.flags();
        v.push(self.destination());
        v.push(command.to_string());
        v
    }

    /// One ssh call, with the remote command wrapped in a login shell — the contract's
    /// `ssh host 'cd <worktree> && bash -lc …'`, spelled once.
    ///
    /// The returned stderr is the ssh client's own (a refused key, a name that does not
    /// resolve) and never the remote command's: ssh does not carry two channels apart
    /// into two pipes. That is [`crate::firecode`]'s trade as well, and it is stated in
    /// [`SshBackend::describe`] rather than left to be discovered.
    pub fn sh(&self, script: &str) -> Result<(String, Vec<u8>, i32), BackendError> {
        let command = format!("bash -lc {}", quote(script));
        let out = std::process::Command::new(&self.program)
            .args(self.argv(&command))
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| {
                BackendError::Io(format!(
                    "running `{}` for {}: {e}",
                    self.program.display(),
                    self.host.name
                ))
            })?;
        let exit = match out.status.code() {
            Some(c) => c,
            None => {
                use std::os::unix::process::ExitStatusExt;
                128 + out.status.signal().unwrap_or(0)
            }
        };
        Ok((
            String::from_utf8_lossy(&out.stdout).into_owned(),
            out.stderr,
            exit,
        ))
    }

    /// The same call, but a non-zero exit is an error carrying what the host said. Used
    /// by everything that is arranging rather than running the model's work.
    fn must(&self, what: &str, script: &str) -> Result<String, BackendError> {
        let (out, stderr, exit) = self.sh(script)?;
        if exit != 0 {
            let said = if stderr.is_empty() {
                out.trim().to_string()
            } else {
                String::from_utf8_lossy(&stderr).trim().to_string()
            };
            return Err(BackendError::Io(format!(
                "{what} on {}: exit {exit}: {}",
                self.host.name,
                if said.is_empty() {
                    "it said nothing"
                } else {
                    &said
                }
            )));
        }
        Ok(out)
    }

    /// A file's bytes, base64 over the wire — **binary-safe**, which a `cat` is not
    /// once the connection is a pipe with a locale on it.
    fn read_abs(&self, path: &str) -> Result<Vec<u8>, BackendError> {
        let q = quote(path);
        let (out, _, exit) = self.sh(&format!(
            "if [ -d {q} ]; then exit 21; fi; if [ ! -e {q} ]; then exit 22; fi; \
             base64 < {q} | tr -d '\\n'"
        ))?;
        match exit {
            0 => crate::firecode::decode_base64(out.as_bytes())
                .map_err(|e| BackendError::Io(format!("{path}: {e}"))),
            21 => Err(BackendError::IsADirectory(path.to_string())),
            _ => Err(BackendError::NotFound(path.to_string())),
        }
    }

    /// **Written atomically**: every chunk goes into a temporary name beside the target
    /// and one `mv -f` puts it in place. A half-written file is worse than a refused
    /// edit — [`ExecBackend::write`] says why — and over ssh the window is wider, not
    /// narrower.
    fn write_abs(&self, path: &str, bytes: &[u8]) -> Result<(), BackendError> {
        let dir = match path.rfind('/') {
            Some(0) => "/".to_string(),
            Some(i) => path[..i].to_string(),
            None => ".".to_string(),
        };
        let tmp = format!("{path}.letibot-{}", std::process::id());
        self.must(
            &format!("making {dir}"),
            &format!("mkdir -p {}", quote(&dir)),
        )?;
        if bytes.is_empty() {
            self.must("making an empty file", &format!(": > {}", quote(&tmp)))?;
        }
        // One ssh call per 48 KiB chunk: a command line is capped (ARG_MAX) and a base64
        // of the whole file would meet that cap on the first large write.
        for (i, chunk) in bytes.chunks(INLINE_WRITE_BYTES).enumerate() {
            let redir = if i == 0 { ">" } else { ">>" };
            self.must(
                &format!("writing {path}"),
                &format!(
                    "printf %s {} | base64 -d {redir} {}",
                    quote(&crate::firecode::encode_base64(chunk)),
                    quote(&tmp)
                ),
            )?;
        }
        self.must(
            &format!("putting {path} in place"),
            &format!("mv -f {} {}", quote(&tmp), quote(path)),
        )
        .map(|_| ())
    }

    /// Drop the ControlMaster, if one was made. Best effort: an ssh that says it has no
    /// master is not a failure of anything.
    pub fn exit_master(&self) {
        let control = self.control.display().to_string();
        let _ = std::process::Command::new(&self.program)
            .args([
                "-o",
                &format!("ControlPath={control}"),
                "-o",
                "ControlMaster=no",
                "-O",
                "exit",
                &self.destination(),
            ])
            .stdin(std::process::Stdio::null())
            .output();
    }
}

impl std::fmt::Debug for Ssh {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ssh")
            .field("dest", &self.destination())
            .field("program", &self.program)
            .finish()
    }
}

/// `'` quoting for a POSIX shell: one argument, no expansion, nothing to escape.
pub(crate) fn quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | ':' | '=' | ',' | '@')
        })
    {
        return s.to_string();
    }
    // A single quote leaves and re-enters the quoting: `'it'\''s'`, which is one
    // argument to the far side's shell and nothing else.
    format!("'{}'", s.replace('\'', "'\\''"))
}

// ---------------------------------------------------------------------------
// The backend
// ---------------------------------------------------------------------------

/// Inline writes up to this many bytes ride in one ssh command.
const INLINE_WRITE_BYTES: usize = 48 * 1024;

/// What to place.
#[derive(Debug, Clone)]
pub struct SshSpec {
    /// The host, from [`Hosts`] — resolved by the caller so that a wrong name is refused
    /// **before** anything is arranged.
    pub host: Host,
    /// The session id: the name of the remote worktree and of the branch.
    pub name: String,
    /// **The repository to send.** The local tree's `HEAD` goes over as a bundle, so this
    /// is the repository the work is in and not necessarily the directory the session was
    /// started in; the caller resolves it (`Config::vm_copy_source` is that resolution).
    pub source: PathBuf,
    /// `write` refused for a downgraded child, exactly as it is for a VM.
    pub writable: bool,
    /// `exec` refused for a downgraded child: no process host and no `bash`.
    pub exec: bool,
    /// The ssh program, when the caller names one (a test's fake). `None` is
    /// `$LETIBOT_SSH`, else `ssh`.
    pub ssh: Option<PathBuf>,
}

/// **A session whose tools run on another machine.**
pub struct SshBackend {
    ssh: Ssh,
    /// The host's name, for sentences and cards.
    host: String,
    /// The resolved remote root, absolute.
    root: String,
    /// `<root>/<repo>.git` — the bare mirror, created on first use.
    mirror: String,
    /// `<root>/<repo>/<short id>` — the checkout the session works in.
    worktree: String,
    /// `remote/<short id>`, the branch the remote commits to.
    branch: String,
    /// `remote/<host>/<short id>`, what that branch becomes in the local repository.
    back_branch: String,
    /// The local repository the tree came from and comes back to.
    local_repo: PathBuf,
    /// The local branch (or commit) the work started from, for the report.
    base: String,
    processes: Option<Arc<RemoteProcesses>>,
    writable: bool,
    /// What the placement had to say: the worktree, and the local edits that stayed here.
    notes: Vec<String>,
    /// What [`ExecBackend::close`] found when it brought the branch back, said once.
    brought_back: Mutex<Option<String>>,
}

impl std::fmt::Debug for SshBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SshBackend")
            .field("host", &self.host)
            .field("worktree", &self.worktree)
            .field("branch", &self.branch)
            .finish()
    }
}

impl SshBackend {
    /// **Arrange the checkout on the remote and open the session on it.**
    ///
    /// Every failure names the step: the repository, the bundle, the mirror, the
    /// worktree. Five things have to be true for this to work and a message that says
    /// only "ssh failed" would leave the operator to guess which.
    pub fn place(spec: &SshSpec) -> Result<SshBackend, BackendError> {
        let mut ssh = Ssh::new(spec.host.clone());
        if let Some(p) = &spec.ssh {
            ssh = ssh.with_program(p.clone());
        }
        let host = spec.host.name.clone();
        let place =
            |e: BackendError| BackendError::Io(format!("placing this session on {host}: {e}"));

        // **The repository, or the refusal that names the fix.** A remote session works
        // on a checkout of a repository, so a folder that is not one cannot carry its
        // work back — and the contract refuses it rather than copying a directory the
        // local machine could never fetch from.
        let local_repo = match git(&spec.source, &["rev-parse", "--show-toplevel"]) {
            Ok(top) => PathBuf::from(top.trim()),
            Err(e) => {
                return Err(BackendError::Io(format!(
                    "placing this session on {host}: {} is not a git repository, and a remote \
                     session works on a checkout — that is what brings its commits back. \
                     Nothing was placed. `git init` the folder (or place the session in a \
                     repository) and ask again. ({e})",
                    spec.source.display()
                )));
            }
        };
        let repo_name = local_repo
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or_else(|| {
                BackendError::Io(format!(
                    "placing this session on {host}: the repository {} has no name to give the \
                     remote worktree",
                    local_repo.display()
                ))
            })?;
        // The branch the work starts from, so the report can say what came back on top of
        // what. A detached HEAD is not a branch name and is recorded as the commit.
        let base = match git(&local_repo, &["symbolic-ref", "--short", "-q", "HEAD"]) {
            Ok(b) => b.trim().to_string(),
            Err(_) => git(&local_repo, &["rev-parse", "--short", "HEAD"])
                .map_err(place)?
                .trim()
                .to_string(),
        };

        let short = short_branch_id(&spec.name);
        let branch = format!("remote/{short}");
        let back_branch = format!("remote/{host}/{short}");

        // **The root, resolved ONCE on the remote.** `root = "~/letibot-remote"` is a
        // fact about the remote's home, not this machine's, so it is expanded there and
        // every path below is absolute — which is also what `git fetch` needs, since it
        // hands the path to `git-upload-pack` in quotes and no shell expands a `~` in
        // them.
        //
        // The `~` is expanded by the remote SHELL and by nothing else: quoting it (`cd
        // '~/x'`) makes a directory literally named `~`, which is the one mistake here
        // that fails silently rather than loudly — so the answer is also required to be
        // absolute, and a relative one is refused rather than used.
        let root = ssh
            .must(
                "making the remote root",
                &format!(
                    // `${r:2}`, not `${r#~/}`: bash tilde-expands the pattern of a `${v#…}`,
                    // so `#~/` quietly matches nothing and the root becomes `$HOME/~/…`.
                    "r={}; case \"$r\" in '~/'*) r=\"$HOME/${{r:2}}\" ;; esac; \
                     mkdir -p \"$r\" && cd \"$r\" && pwd -P",
                    quote(&spec.host.root)
                ),
            )
            .map_err(place)?
            .trim()
            .to_string();
        if !root.starts_with('/') {
            return Err(place(BackendError::Io(format!(
                "the remote said its root is `{root}`, which is not an absolute path — a \
                 `root = \"~/letibot-remote\"` must be expanded by ssh's own shell, and a \
                 remote shell that answers a relative path would put every worktree somewhere \
                 neither this box nor `git fetch` can name"
            ))));
        }
        let mirror = format!("{root}/{repo_name}.git");
        let worktree = format!("{root}/{repo_name}/{short}");

        // **A bare mirror, once.** Reused by every later session onto the same repository,
        // so a second child on the same host sends only its own commits.
        ssh.must(
            "making the mirror",
            &format!(
                "if [ ! -d {m} ]; then git init --bare {m} >/dev/null; fi",
                m = quote(&mirror)
            ),
        )
        .map_err(place)?;

        // **The tree itself, as a bundle**, written into the mirror where the remote `git`
        // can read it. `git bundle` fails on a repository with no commits, and that
        // failure is worth its own sentence: an empty repository is a real answer.
        //
        // The staging file is unique per call — a nanosecond and the pid — because two
        // sessions placing at once in one daemon would otherwise write one bundle and meet
        // each other's `.lock`.
        let staging = std::env::temp_dir().join(format!(
            "letibot-remote-{}-{}-{}.bundle",
            std::process::id(),
            short,
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let bundled = git(
            &local_repo,
            &["bundle", "create", &staging.display().to_string(), "HEAD"],
        );
        if let Err(e) = bundled {
            let _ = std::fs::remove_file(&staging);
            return Err(place(BackendError::Io(format!(
                "bundling {}: {e}",
                local_repo.display()
            ))));
        }
        let bundle = std::fs::read(&staging).map_err(|e| {
            place(BackendError::Io(format!(
                "reading the bundle {}: {e}",
                staging.display()
            )))
        });
        let _ = std::fs::remove_file(&staging);
        let bundle = bundle?;
        let incoming = format!("{mirror}/incoming.bundle");
        ssh.write_abs(&incoming, &bundle).map_err(place)?;
        let staged = format!("refs/heads/letibot/{short}");
        ssh.must(
            "handing the bundle to the remote repository",
            &format!(
                "git -C {m} fetch -f {b} +HEAD:{st} >/dev/null 2>&1 && \
                 git -C {m} rev-parse --verify {st} >/dev/null",
                m = quote(&mirror),
                b = quote(&incoming),
                st = quote(&staged)
            ),
        )
        .map_err(place)?;

        // **The checkout.** A second session on the same id (a resume) finds its worktree
        // already there and keeps it — the branch and everything committed on it.
        let existing = ssh
            .sh(&format!("[ -d {} ] && echo yes", quote(&worktree)))
            .map_err(place)?
            .0;
        if !existing.trim().eq("yes") {
            ssh.must(
                "checking the branch out",
                &format!(
                    "git -C {m} worktree add -b {br} {wt} {st} >/dev/null",
                    m = quote(&mirror),
                    br = quote(&branch),
                    wt = quote(&worktree),
                    st = quote(&staged)
                ),
            )
            .map_err(place)?;
        }

        // **What stayed here, and it is not carried.** Said with the list, because a
        // session that works on a tree the operator is not looking at must say so.
        let mut notes = vec![
            format!("the tree is the branch `{branch}` at {worktree} on {host}"),
            format!("what the session commits there comes back as `{back_branch}`"),
        ];
        let dirty = git(&local_repo, &["status", "--porcelain"])
            .unwrap_or_default()
            .lines()
            .map(str::trim_end)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>();
        if !dirty.is_empty() {
            let shown: Vec<String> = dirty.iter().take(20).cloned().collect();
            notes.push(format!(
                "your uncommitted changes were NOT carried: {}{} — commit them here and place \
                 the session again if you want them there",
                shown.join("; "),
                if dirty.len() > shown.len() {
                    format!(" ({} more)", dirty.len() - shown.len())
                } else {
                    String::new()
                }
            ));
        }

        let processes = if spec.exec {
            let mut pssh = Ssh::new(spec.host.clone());
            if let Some(p) = &spec.ssh {
                pssh = pssh.with_program(p.clone());
            }
            Some(Arc::new(RemoteProcesses::new(
                pssh,
                host.clone(),
                worktree.clone(),
            )))
        } else {
            None
        };

        Ok(SshBackend {
            ssh,
            host,
            root,
            mirror,
            worktree,
            branch,
            back_branch,
            local_repo,
            base,
            processes,
            writable: spec.writable,
            notes,
            brought_back: Mutex::new(None),
        })
    }

    /// The host's name, for a card, a progress line or a test.
    pub fn host_name(&self) -> &str {
        &self.host
    }

    /// The remote root the worktrees live under.
    pub fn root(&self) -> &str {
        &self.root
    }

    /// The remote checkout the session works in.
    pub fn worktree_path(&self) -> &str {
        &self.worktree
    }

    /// The branch the remote commits to.
    pub fn branch(&self) -> &str {
        &self.branch
    }

    /// The local branch that branch becomes.
    pub fn back_branch(&self) -> &str {
        &self.back_branch
    }

    /// **What the placement had to say** — the worktree, the branch that comes back, and
    /// the local edits that were left behind. Printed by the opener, not by the session.
    pub fn placement_notes(&self) -> &[String] {
        &self.notes
    }

    /// A path relative to the checkout, as the remote shell must spell it.
    fn abs(&self, path: &str) -> String {
        let p = path.trim_end_matches('/');
        if p.is_empty() || p == "." {
            return self.worktree.clone();
        }
        if p.starts_with('/') {
            // Absolute, and it means the remote's filesystem: an absolute path in a
            // remote session is a path on that machine, which is the whole point.
            return p.to_string();
        }
        format!("{}/{}", self.worktree, p)
    }

    /// **Bring the branch back**, and say what came with it. Idempotent and safe to call
    /// twice: the second call reports what the first found.
    fn bring_back(&self) -> String {
        if let Some(said) = self
            .brought_back
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            return said;
        }
        let url = format!("{}:{}", self.ssh.destination(), self.mirror);
        let refspec = format!("+{}:refs/heads/{}", self.branch, self.back_branch);
        // `git fetch` over ssh, with **this backend's ssh**: the same flags and the same
        // ControlMaster, so the fetch reuses the connection the session's commands used.
        let ssh_command = std::iter::once(self.ssh.program().display().to_string())
            .chain(self.ssh.flags())
            .map(|a| quote(&a))
            .collect::<Vec<_>>()
            .join(" ");
        let fetched = std::process::Command::new("git")
            .arg("-C")
            .arg(&self.local_repo)
            .arg("-c")
            .arg(format!("core.sshCommand={ssh_command}"))
            .arg("fetch")
            .arg("--no-tags")
            .arg(&url)
            .arg(&refspec)
            .stdin(std::process::Stdio::null())
            .output();
        let mut said = match fetched {
            Ok(out) if out.status.success() => {
                let commit = git(
                    &self.local_repo,
                    &["rev-parse", "--short", &self.back_branch],
                )
                .unwrap_or_default()
                .trim()
                .to_string();
                let ahead = git(
                    &self.local_repo,
                    &[
                        "rev-list",
                        "--count",
                        &format!("{}..{}", self.base, self.back_branch),
                    ],
                )
                .unwrap_or_default()
                .trim()
                .to_string();
                format!(
                    "the session on {} ended. Its branch came back here as `{}` — {} commit(s) \
                     on top of `{}`, now at {}: diff it and take what you want",
                    self.host, self.back_branch, ahead, self.base, commit
                )
            }
            Ok(out) => format!(
                "the session on {} ended and its branch did NOT come back: `git fetch` said \
                 \"{}\". The work is still there, in {} on that machine (branch `{}`) — reach \
                 it with `git fetch {} {refspec}` once the cause is fixed",
                self.host,
                String::from_utf8_lossy(&out.stderr).trim(),
                self.worktree,
                self.branch,
                url
            ),
            Err(e) => format!(
                "the session on {} ended and its branch did NOT come back: git would not run \
                 ({e}). The work is still there, in {} on that machine (branch `{}`)",
                self.host, self.worktree, self.branch
            ),
        };
        // **What was left uncommitted is not in that branch.** A session that ends with a
        // dirty tree has work the branch does not hold, and a report that named only the
        // branch would be a false "it is all here".
        if let Ok((dirty, _, 0)) = self.ssh.sh(&format!(
            "cd {} && git status --porcelain",
            quote(&self.worktree)
        )) {
            let lines: Vec<&str> = dirty.lines().filter(|l| !l.trim().is_empty()).collect();
            if !lines.is_empty() {
                let shown = lines
                    .iter()
                    .take(20)
                    .copied()
                    .collect::<Vec<_>>()
                    .join("; ");
                said.push_str(&format!(
                    ". **{} uncommitted change(s) are still only on that machine**: {}{}",
                    lines.len(),
                    shown,
                    if lines.len() > 20 {
                        format!(" ({} more)", lines.len() - 20)
                    } else {
                        String::new()
                    }
                ));
            }
        }
        said.push_str(&format!(". The checkout is still at {}", self.worktree));
        *self.brought_back.lock().unwrap_or_else(|e| e.into_inner()) = Some(said.clone());
        self.ssh.exit_master();
        said
    }
}

/// The ssh program this process would run with no override.
fn ssh_program_default() -> PathBuf {
    std::env::var_os("LETIBOT_SSH")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("ssh"))
}

/// The last eight usable characters of a session id, as a branch and directory name.
///
/// Alphanumerics only: a session id is a time and a random tail (`s-1793abcd-ef01-…`), the
/// tail is what tells two of them apart, and a `-` in the middle of eight characters buys
/// nothing but a branch name with a seam in it.
fn short_branch_id(id: &str) -> String {
    let cleaned: String = id.chars().filter(char::is_ascii_alphanumeric).collect();
    let tail: String = cleaned
        .chars()
        .rev()
        .take(8)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let tail = tail.trim_matches('-').to_string();
    if tail.is_empty() {
        "session".to_string()
    } else {
        tail
    }
}

/// One local `git` call, with its output, or the reason it failed.
fn git(repo: &Path, args: &[&str]) -> Result<String, BackendError> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| BackendError::Io(format!("git {}: {e}", args.join(" "))))?;
    if !out.status.success() {
        return Err(BackendError::Io(format!(
            "git {} in {}: {}",
            args.join(" "),
            repo.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

impl ExecBackend for SshBackend {
    fn run(&self, cmd: &Command) -> Result<Output, BackendError> {
        if self.processes.is_none() {
            return Err(BackendError::Unsupported(
                "this subagent was downgraded to no exec, so nothing runs on its host",
            ));
        }
        let joined = cmd
            .argv
            .iter()
            .map(|a| quote(a))
            .collect::<Vec<_>>()
            .join(" ");
        let env_prefix = cmd
            .env
            .iter()
            .map(|(k, v)| format!("{}={} ", quote(k), quote(v)))
            .collect::<String>();
        let cwd = self.abs(&cmd.cwd);
        let (out, stderr, exit) = self
            .ssh
            .sh(&format!("cd {} && {env_prefix}{joined}", quote(&cwd)))?;
        if exit == 255 && out.is_empty() {
            // ssh's own exit code, with nothing from the command: the connection is the
            // failure, and saying so is the difference between a broken command and an
            // unreachable machine.
            return Err(BackendError::Io(format!(
                "ssh to {} failed: {}",
                self.host,
                String::from_utf8_lossy(&stderr).trim()
            )));
        }
        Ok(Output {
            stdout: out.into_bytes(),
            // Merged by ssh itself — see `Ssh::sh`.
            stderr: Vec::new(),
            exit,
        })
    }

    fn read(&self, path: &str) -> Result<Vec<u8>, BackendError> {
        self.ssh.read_abs(&self.abs(path))
    }

    fn write(&self, path: &str, bytes: &[u8]) -> Result<(), BackendError> {
        if !self.writable {
            return Err(BackendError::Unsupported(
                "this subagent was downgraded to no write; nothing on its host changed",
            ));
        }
        self.ssh.write_abs(&self.abs(path), bytes)
    }

    fn processes(&self) -> Option<&dyn ProcessHost> {
        self.processes.as_deref().map(|p| p as &dyn ProcessHost)
    }

    fn processes_arc(&self) -> Option<Arc<dyn ProcessHost>> {
        self.processes.clone().map(|p| p as Arc<dyn ProcessHost>)
    }

    fn is_writable(&self) -> bool {
        self.writable
    }

    /// **A remote path means nothing on this host.** `root_path` is the answer to *is
    /// this path inside the boundary*, and inventing `/home/dead/letibot-remote` here
    /// would make the gate answer confidently about a filesystem it is not looking at.
    /// [`Self::workspace_path`] is the other question — *which directory is this session
    /// working in* — and that one has an answer.
    fn root_path(&self) -> Option<String> {
        None
    }

    fn workspace_path(&self) -> Option<String> {
        Some(self.worktree.clone())
    }

    /// **The machine the tools run on**, for the gate's own sentence. A card that said
    /// only `/home/dead/letibot-remote/letibot/1a2b3c4d` would name a path nobody could
    /// tell apart from one on this box — and an operator deciding a `bash` call here is
    /// deciding about another machine.
    fn host_label(&self) -> Option<String> {
        Some(format!("{} over ssh", self.host))
    }

    /// Where a tool's `cwd` starts: the checkout, not this machine's root.
    fn workdir(&self, cwd: &str) -> Result<String, BackendError> {
        Ok(self.abs(cwd))
    }

    fn list(&self, path: &str) -> Result<Vec<DirEntry>, BackendError> {
        let q = format!("'{}'", self.abs(path).replace('\'', r#"'\\''"#));
        let (out, _, exit) = self.ssh.sh(&format!(
            "if [ ! -e {q} ]; then exit 22; fi\n\
             if [ ! -d {q} ]; then exit 23; fi\n\
             for f in {q}/* {q}/.[!.]*; do\n\
               [ -e \"$f\" ] || [ -L \"$f\" ] || continue\n\
               if [ -d \"$f\" ]; then printf 'd\\t0\\t%s\\n' \"$f\"; \
               else printf 'f\\t%s\\t%s\\n' \"$(wc -c < \"$f\")\" \"$f\"; fi\n\
             done"
        ))?;
        match exit {
            0 => {}
            22 => return Err(BackendError::NotFound(path.to_string())),
            23 => return Err(BackendError::NotADirectory(path.to_string())),
            _ => return Err(BackendError::Io(out)),
        }
        let mut entries = Vec::new();
        for line in out.lines() {
            let mut parts = line.splitn(3, '\t');
            let (Some(kind), Some(size), Some(p)) = (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            entries.push(DirEntry {
                path: p.to_string(),
                name: p.rsplit('/').next().unwrap_or(p).to_string(),
                is_dir: kind == "d",
                bytes: size.trim().parse().unwrap_or(0),
            });
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }

    /// One walk for the whole tree, because `list` here is an ssh round trip: the
    /// trait's default loop over a 122-directory tree would be 122 of them.
    fn list_tree(
        &self,
        root: &str,
        limit: usize,
        skip: &dyn Fn(&DirEntry) -> bool,
    ) -> Result<(Vec<DirEntry>, bool), BackendError> {
        let abs = self.abs(root);
        let q = quote(&abs);
        let (_, _, gate) = self.ssh.sh(&format!(
            "if [ ! -e {q} ]; then exit 22; fi; if [ ! -d {q} ]; then exit 23; fi; exit 0"
        ))?;
        match gate {
            0 => {}
            22 => return Err(BackendError::NotFound(root.to_string())),
            23 => return Err(BackendError::NotADirectory(root.to_string())),
            _ => {}
        }
        let (listing, _, _) = self.ssh.sh(&format!("{} walk {q}", TREE_WALK))?;
        Ok(crate::firecode::tree_in_walk_order(
            listing.as_bytes(),
            &abs,
            limit,
            skip,
        ))
    }

    fn stat(&self, path: &str) -> Option<DirEntry> {
        let abs = self.abs(path);
        let q = quote(&abs);
        let (out, _, exit) = self
            .ssh
            .sh(&format!(
                "if [ -e {q} ] || [ -L {q} ]; then \
                   if [ -d {q} ]; then printf 'd\\t0\\n'; \
                   else printf 'f\\t%s\\n' \"$(wc -c < {q})\"; fi; \
                 else exit 1; fi"
            ))
            .ok()?;
        if exit != 0 {
            return None;
        }
        let (kind, size) = out.trim().split_once('\t')?;
        Some(DirEntry {
            path: abs.clone(),
            name: abs.rsplit('/').next().unwrap_or(&abs).to_string(),
            is_dir: kind == "d",
            bytes: size.trim().parse().unwrap_or(0),
        })
    }

    fn describe(&self) -> String {
        format!(
            "ssh host `{}` ({}): every tool call runs there — its files, its commands, its \
             processes — while this session's model, store and gate stay on this machine. \
             NOT CONFINED: it is your machine with your files, so the gate is the host's gate \
             and every call is decided by it. The tree is a checkout of {} at {}, on branch \
             `{}`; what it commits comes back as `{}` when the session ends, and uncommitted \
             edits stay there. A command's stderr arrives merged into its output (ssh carries \
             one channel). {}",
            self.host,
            self.ssh.destination(),
            self.local_repo.display(),
            self.worktree,
            self.branch,
            self.back_branch,
            if self.processes.is_some() {
                "exec through ssh; a background job runs under setsid with its output in \
                 `.letibot-remote/jobs/` and outlives the connection"
            } else {
                "NO exec (downgraded)"
            }
        )
    }

    fn close(&self) -> Option<String> {
        Some(self.bring_back())
    }
}

/// A depth-first walk, printed as `<d|f>\t<size>\t<path>` — the same shape firecode's
/// `find -printf` produces, so [`crate::firecode::tree_in_walk_order`] rebuilds the
/// breadth-first order the trait's default loop would have produced.
///
/// A shell function and not `find -printf`: `-printf` is GNU's, and the machine on the
/// other end of an ssh connection is not always Linux.
///
/// **The definition only — the call is the caller's**, with its directory as the argument.
/// This ended in a bare `walk` line once, and a caller that appended a path got `walk` on
/// one line and `/path` on the next: `walk` then ran with no argument and the walk covered
/// the whole filesystem from `/`, which is a hang rather than an error and was measured as
/// one (two tests sat in it). A constant a caller has to *finish* is the safe shape.
const TREE_WALK: &str = r#"
walk() {
  [ -n "$1" ] && [ -d "$1" ] || return 0
  for f in "$1"/* "$1"/.[!.]*; do
    [ -e "$f" ] || [ -L "$f" ] || continue
    if [ -d "$f" ]; then
      printf 'd\t0\t%s\n' "$f"
      walk "$f"
    else
      printf 'f\t%s\t%s\n' "$(wc -c < "$f" 2>/dev/null | tr -d ' ')" "$f"
    fi
  done
}
"#;

// ---------------------------------------------------------------------------
// Processes on the remote
// ---------------------------------------------------------------------------

/// **The process host of a remote session**: its jobs live in files on that machine.
///
/// A foreground `bash` call does not have to be a local process at all — it is started
/// the same way a background one is, under `setsid`, and waited on by polling its exit
/// file. That is what makes *"a job outlives the ssh connection"* true of every run
/// rather than of the ones that asked: there is no ssh call left to outlive once the
/// launch returns, and the daemon's own death does not reach it either.
///
/// The record of a job is four files under `<worktree>/.letibot-remote/jobs/`:
///
/// | file | is |
/// |---|---|
/// | `<id>.sh` | the launcher: `cd`, the environment, the command, the exit status |
/// | `<id>.pid` | the shell's pid, written before the command runs |
/// | `<id>.out` | its stdout and stderr, merged, which is what `job_output` reads |
/// | `<id>.exit` | its status, written last — **its existence is the settlement** |
///
/// so a daemon that restarted can find a job it lost track of, and `job_kill` has a pid
/// it can signal remotely.
pub struct RemoteProcesses {
    ssh: Ssh,
    host: String,
    /// `<worktree>/.letibot-remote/jobs`, absolute on the remote.
    jobs_dir: String,
    worktree: String,
    jobs: Mutex<Vec<Arc<RemoteJob>>>,
    reaps: Mutex<Vec<Reaping>>,
    session_scope: Mutex<Option<ScopeId>>,
    turn_scope: Mutex<Option<ScopeId>>,
}

/// One job, as this daemon remembers it. The files are the truth; this is the index.
struct RemoteJob {
    id: JobId,
    command: String,
    slug: Option<String>,
    cwd: String,
    started: SystemTime,
    state: Mutex<JobState>,
    settled_at: Mutex<Option<SystemTime>>,
    pid: Mutex<u32>,
    produced: Mutex<u64>,
    scope: ScopeId,
    owner: ScopeId,
    background: Option<Backgrounding>,
    out: String,
    pid_path: String,
    exit: String,
    /// **Who is expected to end it**, when somebody besides the command itself is: the
    /// deadline. Read when the pid goes away with no status file, so the settlement says
    /// *its deadline passed* rather than *something on that machine ended it*.
    armed_kill: Mutex<Option<String>>,
}

impl RemoteJob {
    fn state(&self) -> JobState {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn is_running(&self) -> bool {
        self.state().is_running()
    }

    fn settle(&self, state: JobState) {
        let mut g = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if g.is_running() {
            *g = state;
            *self.settled_at.lock().unwrap_or_else(|e| e.into_inner()) = Some(SystemTime::now());
        }
    }

    fn produced(&self) -> u64 {
        *self.produced.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn pid(&self) -> u32 {
        *self.pid.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl std::fmt::Debug for RemoteProcesses {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteProcesses")
            .field("host", &self.host)
            .field("jobs", &self.jobs_dir)
            .finish()
    }
}

impl RemoteProcesses {
    pub fn new(ssh: Ssh, host: String, worktree: String) -> RemoteProcesses {
        let jobs_dir = format!("{worktree}/.letibot-remote/jobs");
        RemoteProcesses {
            ssh,
            host,
            jobs_dir,
            worktree,
            jobs: Mutex::new(Vec::new()),
            reaps: Mutex::new(Vec::new()),
            session_scope: Mutex::new(None),
            turn_scope: Mutex::new(None),
        }
    }

    fn find(&self, id: &JobId) -> Option<Arc<RemoteJob>> {
        self.jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|j| j.id.0 == id.0)
            .cloned()
    }

    /// **Ask the files what happened**, and record it. One round trip per job: the exit
    /// status if there is one, whether the pid is still alive, and how much output there
    /// is.
    fn refresh(&self, job: &RemoteJob) {
        if !job.is_running() {
            return;
        }
        let script = format!(
            "e=; if [ -f {exit} ]; then e=$(cat {exit}); fi\n\
             a=0; if [ -f {pid} ]; then p=$(cat {pid}); if kill -0 \"$p\" 2>/dev/null; then a=$p; fi; fi\n\
             n=0; if [ -f {out} ]; then n=$(wc -c < {out} | tr -d ' '); fi\n\
             printf '%s\\t%s\\t%s\\n' \"$e\" \"$a\" \"$n\"",
            exit = quote(&job.exit),
            pid = quote(&job.pid_path),
            out = quote(&job.out),
        );
        let Ok((line, _, 0)) = self.ssh.sh(&script) else {
            return;
        };
        // **`trim_end`, not `trim`.** The fields are tab-separated and the first one is
        // EMPTY whenever the job has not written its exit status yet — `trim()` ate that
        // leading tab, the three fields became two, and every running job read as "no pid,
        // no output, no exit" for its whole life. Measured: a live `sleep 300` reported
        // pid 0 fifty times while its pid file said otherwise.
        let mut parts = line.trim_end_matches(['\n', '\r']).split('\t');
        let (Some(code), Some(alive), Some(produced)) = (parts.next(), parts.next(), parts.next())
        else {
            return;
        };
        if let Ok(n) = produced.trim().parse::<u64>() {
            *job.produced.lock().unwrap_or_else(|e| e.into_inner()) = n;
        }
        if let Ok(p) = alive.trim().parse::<u32>()
            && p != 0
        {
            *job.pid.lock().unwrap_or_else(|e| e.into_inner()) = p;
        }
        let code = code.trim();
        if !code.is_empty() {
            job.settle(JobState::Exited {
                code: code.parse().unwrap_or(-1),
            });
        } else if alive.trim() == "0" && job.pid() != 0 {
            // No exit file, and the pid we saw is gone: something on that machine ended
            // it and it never wrote a status. Said as that, not as a graceful exit it
            // did not make.
            let by = job
                .armed_kill
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
                .unwrap_or_else(|| {
                    "something on the remote ended it and it wrote no exit status".to_string()
                });
            job.settle(JobState::Killed { by });
        }
    }

    /// Everything the files say, for a view.
    fn view(&self, job: &RemoteJob) -> JobView {
        self.refresh(job);
        let state = job.state();
        let settled = *job.settled_at.lock().unwrap_or_else(|e| e.into_inner());
        JobView {
            memory_cap: MemoryCap::NotAsked,
            memory: None,
            id: job.id.clone(),
            command: job.command.clone(),
            slug: job.slug.clone(),
            scope: job.scope.clone(),
            owner: job.owner.clone(),
            cwd: job.cwd.clone(),
            pid: job.pid(),
            background: job.background.clone(),
            elapsed: job.started.elapsed().unwrap_or_default(),
            ran_for: settled.map(|at| at.duration_since(job.started).unwrap_or_default()),
            state,
            produced: job.produced(),
            since_last_output: None,
        }
    }

    fn scope_id(&self, kind: ScopeKind, name: &str) -> ScopeId {
        ScopeId {
            kind,
            name: name.to_string(),
            // **Not a cgroup, and spelled so it cannot be mistaken for one.** Nothing on
            // this machine owns these processes; the remote's own session does.
            path: PathBuf::from(format!("ssh://{}/{}", self.host, self.jobs_dir)),
        }
    }

    fn scope_of(&self, kind: ScopeKind, name: Option<&str>) -> ScopeId {
        let slot = match kind {
            ScopeKind::Session => &self.session_scope,
            _ => &self.turn_scope,
        };
        let mut g = slot.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(s) = g.as_ref() {
            return s.clone();
        }
        let s = self.scope_id(kind, name.unwrap_or("remote"));
        *g = Some(s.clone());
        s
    }

    /// **Start a job under `setsid`**, so it survives this ssh call and this Mac.
    ///
    /// The launcher is written first and run in the background with all three streams on
    /// `/dev/null`: ssh stays connected until every stream the command inherited is
    /// closed, so a background job that kept stdout would hold the launch call open for
    /// as long as it ran — the failure mode that makes "background" mean "foreground
    /// with an id".
    fn launch(&self, req: &SpawnRequest) -> Result<JobId, ExecError> {
        let id = JobId::next();
        let n = id.0.trim_start_matches('j').to_string();
        let scope = self.scope_id(ScopeKind::Turn, &format!("job-{id}"));
        let owner = self.scope_of(req.scope, req.scope_name.as_deref());
        let jobs = self.jobs_dir.clone();
        let out = format!("{jobs}/{n}.out");
        let pid_path = format!("{jobs}/{n}.pid");
        let exit = format!("{jobs}/{n}.exit");
        let script_path = format!("{jobs}/{n}.sh");
        let cwd = if req.cwd.trim().is_empty() {
            self.worktree.clone()
        } else if req.cwd.starts_with('/') {
            req.cwd.clone()
        } else {
            format!("{}/{}", self.worktree, req.cwd.trim_end_matches('/'))
        };
        let mut launcher = String::from("#!/bin/bash\n");
        launcher.push_str(&format!("echo $$ > {}\n", quote(&pid_path)));
        launcher.push_str(&format!("cd {} || exit 97\n", quote(&cwd)));
        for (k, v) in &req.env {
            launcher.push_str(&format!("export {}={}\n", quote(k), quote(v)));
        }
        // **A SUBSHELL, not a brace group.** `{ …; }` runs in the launcher's own shell, so a
        // command containing `exit` — `bash -c 'exit 7'`, or a plain `exit 1` — ended the
        // launcher before it could write the status, and the job was then read as "died
        // without writing one". Measured with `exit 7`: pid written, output written, no exit
        // file. `( … )` is a child shell, so its `exit` is the status and not the end.
        launcher.push_str(&format!(
            "( {} ) > {} 2>&1\necho $? > {}\n",
            req.command,
            quote(&out),
            quote(&exit)
        ));
        self.ssh
            .write_abs(&script_path, launcher.as_bytes())
            .map_err(|e| {
                ExecError::Spawn(format!(
                    "writing the launcher {script_path} on {}: {e}",
                    self.host
                ))
            })?;

        // `setsid` where the host has it: the job leads its own process group, so
        // `job_kill` reaches everything it forked. Where it does not, the job still
        // outlives the connection — it is only the group kill that is weaker, and
        // [`RemoteProcesses::kill`] checks which of the two it has.
        let launch = format!(
            "cd {jobs} && rm -f {exit} && \
             if command -v setsid >/dev/null 2>&1; then setsid bash {s} </dev/null >/dev/null 2>&1 & \
             else nohup bash {s} </dev/null >/dev/null 2>&1 & fi; echo started",
            jobs = quote(&jobs),
            exit = quote(&exit),
            s = quote(&script_path),
        );
        self.ssh
            .sh(&format!("mkdir -p {}", quote(&jobs)))
            .map_err(|e| ExecError::Spawn(format!("making {} on {}: {e}", jobs, self.host)))?;
        let (said, stderr, code) = self
            .ssh
            .sh(&launch)
            .map_err(|e| ExecError::Spawn(format!("starting a job on {}: {e}", self.host)))?;
        if code != 0 {
            return Err(ExecError::Spawn(format!(
                "the launcher on {} exited {code}: {}",
                self.host,
                if stderr.is_empty() {
                    said.trim().to_string()
                } else {
                    String::from_utf8_lossy(&stderr).trim().to_string()
                }
            )));
        }
        let job = Arc::new(RemoteJob {
            id: id.clone(),
            command: req.command.clone(),
            slug: req.slug.clone(),
            cwd,
            started: SystemTime::now(),
            state: Mutex::new(JobState::Running),
            settled_at: Mutex::new(None),
            pid: Mutex::new(0),
            produced: Mutex::new(0),
            scope,
            owner,
            background: req.background.then_some(Backgrounding::Asked),
            out: out.clone(),
            pid_path,
            exit,
            armed_kill: Mutex::new(None),
        });
        // **The pid is the evidence it started.** Polled rather than assumed: a launcher
        // that could not `cd` writes its pid and then exits 97, so no pid after two
        // seconds is a job that never began — and saying so here is better than a
        // `job_output` that reads an empty file for ever.
        let mut seen = 0;
        for _ in 0..50 {
            self.refresh(&job);
            seen = job.pid();
            if seen != 0 || !job.is_running() {
                break;
            }
            std::thread::sleep(Duration::from_millis(40));
        }
        if seen == 0 && job.is_running() {
            job.settle(JobState::Killed {
                by: "the remote wrote no pid, so the command never started".into(),
            });
            // **What the launcher said**, when it said anything: a job that never wrote its
            // pid is either a launcher that could not run or one that died between the two
            // writes, and the difference is in this file.
            let said = self
                .ssh
                .sh(&format!(
                    "echo '--- the launcher:'; cat {s} 2>/dev/null | head -c 400; \
                     echo '--- what it wrote:'; cat {o} 2>/dev/null | tail -c 400",
                    s = quote(&script_path),
                    o = quote(&out)
                ))
                .map(|(o, _, _)| o)
                .unwrap_or_default();
            return Err(ExecError::Spawn(format!(
                "the command did not start on {}: {} was not written within 2 s. {}",
                self.host,
                pid_path_message(&self.jobs_dir, &n),
                said.trim()
            )));
        }
        self.jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(job);
        Ok(id)
    }

    /// End one job by killing **its process group on the remote**, and record what was
    /// there before and after — the same falsifier the local reaper keeps.
    fn kill(&self, job: &RemoteJob, by: &str) -> Result<Reaping, ExecError> {
        let pid = job.pid();
        let mut reaping = Reaping {
            scope: job.scope.clone(),
            at: SystemTime::now(),
            mechanism: "setsid process group on the remote",
            observed: Vec::new(),
            survivors: Vec::new(),
            waited: Duration::ZERO,
            removed: false,
            note: None,
        };
        if !job.is_running() {
            reaping.mechanism = "nothing to kill";
            reaping.note = Some(format!("{} had already ended", job.id.0));
            return Ok(reaping);
        }
        if pid == 0 {
            reaping.mechanism = "nothing to kill";
            reaping.note = Some(format!("{} never started", job.id.0));
            job.settle(JobState::Killed { by: by.into() });
            return Ok(reaping);
        }
        // **What is there before the kill**, so a zero is falsifiable.
        if let Ok((said, _, 0)) = self
            .ssh
            .sh(&format!("ps -p {pid} -o pid=,comm= 2>/dev/null || true"))
            && let Some(line) = said.lines().find(|l| !l.trim().is_empty())
        {
            let mut it = line.split_whitespace();
            let p = it.next().and_then(|p| p.parse().ok()).unwrap_or(pid);
            reaping.observed.push(Reaped {
                pid: p,
                comm: it.next().unwrap_or("(unnamed)").to_string(),
                cmdline: job.command.clone(),
            });
        }
        // **A group kill only when the pid really leads a group.** `kill -TERM -<pid>`
        // on a pid that is not a process-group leader signals *this* shell's group, and
        // that is the one mistake here that could take out something the operator cares
        // about. `ps -o pgid=` answers it, on the remote, before the signal.
        let pgid = self
            .ssh
            .sh(&format!("ps -p {pid} -o pgid= 2>/dev/null | tr -d ' '"))
            .ok()
            .map(|(said, _, _)| said.trim().to_string())
            .unwrap_or_default();
        let target = if pgid == pid.to_string() {
            format!("-{pid}")
        } else {
            reaping.note = Some(format!(
                "`setsid` is not on {} — killed the launcher only, not its group",
                self.host
            ));
            pid.to_string()
        };
        let started = Instant::now();
        let _ = self
            .ssh
            .sh(&format!("kill -TERM {target} 2>/dev/null || true"));
        for _ in 0..40 {
            self.refresh(job);
            if !job.is_running() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if job.is_running() {
            let _ = self
                .ssh
                .sh(&format!("kill -KILL {target} 2>/dev/null || true"));
            reaping.note = Some(format!("{} ignored SIGTERM and was killed hard", job.id.0));
        }
        reaping.waited = started.elapsed();
        job.settle(JobState::Killed { by: by.into() });
        // **Who is left**, read back the way it was read before.
        if let Ok((said, _, 0)) = self
            .ssh
            .sh(&format!("kill -0 {pid} 2>/dev/null && echo alive"))
            && said.contains("alive")
        {
            reaping.survivors.push(pid);
        }
        reaping.removed = reaping.survivors.is_empty();
        self.reaps
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(reaping.clone());
        Ok(reaping)
    }
}

fn pid_path_message(jobs: &str, n: &str) -> String {
    format!("{jobs}/{n}.pid")
}

impl ProcessHost for RemoteProcesses {
    fn describe(&self) -> String {
        format!(
            "processes on ssh host `{}`: a job is started there under setsid, with its output \
             in `{}`, so it outlives the ssh call that started it and this machine going to \
             sleep. NOT CONFINED: no boundary and no cgroup on this machine — the remote's own \
             process tree owns it, and job_kill signals that group over ssh.",
            self.host, self.jobs_dir
        )
    }

    fn spawn(&self, req: &SpawnRequest) -> Result<JobId, ExecError> {
        self.launch(req)
    }

    /// **The deadline is armed on the machine that runs the command**, not on this one.
    ///
    /// The local host arms a thread for a reason worth keeping: the loop enforcing a
    /// deadline must not be the loop being guarded. Here there is a second reason and it
    /// is stronger — a thread here dies with this daemon, and the process it would have
    /// ended does not: a job on the remote outlives this Mac sleeping, so its deadline
    /// has to as well. So a detached `sleep … kill` is started over the same connection,
    /// and [`RemoteProcesses::refresh`] settles the job as a deadline kill when its pid
    /// goes away without writing a status.
    fn arm_deadline(&self, job: &JobId, after: Duration) {
        let Some(job) = self.find(job) else {
            return;
        };
        let pid = job.pid();
        if pid == 0 {
            return;
        }
        *job.armed_kill.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(crate::exec::jobs::DEADLINE_KILL.to_string());
        let secs = after.as_secs_f64().max(1.0);
        // The group only when the pid really leads one — `kill -TERM -<pid>` on a pid that
        // does not signals *this shell's* group instead. Same test `kill` makes.
        let script = format!(
            "sleep {secs}\n\
             g=$(ps -p {pid} -o pgid= 2>/dev/null | tr -d ' ')\n\
             if [ \"$g\" = \"{pid}\" ]; then kill -TERM -{pid}; else kill -TERM {pid}; fi\n\
             sleep 2\n\
             if [ \"$g\" = \"{pid}\" ]; then kill -KILL -{pid} 2>/dev/null; \
             else kill -KILL {pid} 2>/dev/null; fi\n"
        );
        let _ = self.ssh.sh(&format!(
            "nohup bash -c {} </dev/null >/dev/null 2>&1 & echo armed",
            quote(&script)
        ));
    }

    fn wait_job(&self, job: &JobId, deadline: Duration) -> Result<Waited, ExecError> {
        let Some(job) = self.find(job) else {
            return Err(ExecError::NoSuchJob(job.0.clone()));
        };
        let started = Instant::now();
        // Polled, and the interval is the trade: every check is a round trip to another
        // machine, so 120 ms is the fastest that does not cost more than the work.
        let tick = Duration::from_millis(120);
        loop {
            self.refresh(&job);
            if !job.is_running() {
                return Ok(Waited::Happened {
                    state: Some(job.state()),
                    took: started.elapsed(),
                });
            }
            if started.elapsed() >= deadline {
                return Ok(Waited::Deadline {
                    took: started.elapsed(),
                    produced: job.produced(),
                    since_last_output: None,
                });
            }
            std::thread::sleep(tick);
        }
    }

    fn wait_scope(&self, scope: &ScopeId, deadline: Duration) -> Result<Waited, ExecError> {
        let started = Instant::now();
        let mine: Vec<Arc<RemoteJob>> = self
            .jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|j| j.owner.name == scope.name || j.scope.name == scope.name)
            .cloned()
            .collect();
        if mine.is_empty() {
            return Ok(Waited::NeverStarted {
                waited_for_presence: started.elapsed(),
            });
        }
        loop {
            for j in &mine {
                self.refresh(j);
            }
            if mine.iter().all(|j| !j.is_running()) {
                return Ok(Waited::Happened {
                    state: None,
                    took: started.elapsed(),
                });
            }
            if started.elapsed() >= deadline {
                return Ok(Waited::Deadline {
                    took: started.elapsed(),
                    produced: mine.iter().map(|j| j.produced()).sum(),
                    since_last_output: None,
                });
            }
            std::thread::sleep(Duration::from_millis(120));
        }
    }

    fn job(&self, job: &JobId) -> Option<JobView> {
        self.find(job).map(|j| self.view(&j))
    }

    fn jobs(&self) -> Vec<JobView> {
        let all: Vec<Arc<RemoteJob>> = self.jobs.lock().unwrap_or_else(|e| e.into_inner()).clone();
        all.iter().map(|j| self.view(j)).collect()
    }

    /// **The output is a file on the remote**, so a read is `tail -c` from where the
    /// caller stopped — one round trip, and the bytes it asks for rather than the whole
    /// file every time.
    fn output(&self, job: &JobId, from: u64, limit: usize) -> Result<OutputSlice, ExecError> {
        let job = self
            .find(job)
            .ok_or_else(|| ExecError::NoSuchJob(job.0.clone()))?;
        self.refresh(&job);
        let (said, _, code) = self
            .ssh
            .sh(&format!(
                "tail -c +{} {} 2>/dev/null | base64 | tr -d '\\n'",
                from + 1,
                quote(&job.out)
            ))
            .map_err(|e| ExecError::Spawn(format!("reading {}: {e}", job.out)))?;
        if code != 0 {
            return Err(ExecError::Spawn(format!(
                "reading {} on {}: exit {code}",
                job.out, self.host
            )));
        }
        let mut bytes = crate::firecode::decode_base64(said.trim().as_bytes())
            .map_err(|e| ExecError::Spawn(format!("reading {}: {e}", job.out)))?;
        if limit != usize::MAX && bytes.len() > limit {
            bytes.truncate(limit);
        }
        // **Nothing is dropped**: the file on that machine keeps everything, so a reader
        // that stops early can ask again from where it stopped and get the bytes it missed.
        // `produced` is the file's size, and `to` is where this slice ends in it.
        let to = from + bytes.len() as u64;
        let produced = job.produced().max(to);
        Ok(OutputSlice {
            bytes,
            from,
            to,
            produced,
            dropped: 0,
            retained: produced,
        })
    }

    fn kill_job_as(&self, job: &JobId, by: &str) -> Result<Reaping, ExecError> {
        let job = self
            .find(job)
            .ok_or_else(|| ExecError::NoSuchJob(job.0.clone()))?;
        self.kill(&job, by)
    }

    fn end_scope(&self, scope: &ScopeId) -> Result<Reaping, ExecError> {
        let mine: Vec<Arc<RemoteJob>> = self
            .jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|j| j.owner.name == scope.name || j.scope.name == scope.name)
            .cloned()
            .collect();
        let mut observed = Vec::new();
        for j in &mine {
            if let Ok(r) = self.kill(j, "its scope ended") {
                observed.extend(r.observed);
            }
        }
        let reaping = Reaping {
            scope: scope.clone(),
            at: SystemTime::now(),
            mechanism: "setsid process group on the remote",
            observed,
            survivors: Vec::new(),
            waited: Duration::ZERO,
            removed: true,
            note: None,
        };
        self.reaps
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(reaping.clone());
        Ok(reaping)
    }

    /// **Nothing is moved, because nothing needed moving.** A job on this host is
    /// started detached from the ssh call that made it, so it already outlives the turn;
    /// the record says that rather than claiming a migration that did not happen.
    fn promote(
        &self,
        job: &JobId,
        to: ScopeKind,
        name: Option<&str>,
        how: Backgrounding,
    ) -> Result<Promotion, ExecError> {
        let j = self
            .find(job)
            .ok_or_else(|| ExecError::NoSuchJob(job.0.clone()))?;
        let target = self.scope_of(to, name);
        Ok(Promotion {
            job: j.id.clone(),
            command: j.command.clone(),
            slug: j.slug.clone(),
            from: j.owner.clone(),
            to: target,
            ran_for: j.started.elapsed().unwrap_or_default(),
            how,
            migration: None,
            at: SystemTime::now(),
            note: Some(format!(
                "it was started detached on {}, so it already outlives this turn: nothing moved",
                self.host
            )),
        })
    }

    fn promotions(&self) -> Vec<Promotion> {
        Vec::new()
    }

    fn scopes(&self) -> Vec<ScopeId> {
        let mut out: Vec<ScopeId> = Vec::new();
        for j in self.jobs.lock().unwrap_or_else(|e| e.into_inner()).iter() {
            if !out.iter().any(|s| s.name == j.owner.name) {
                out.push(j.owner.clone());
            }
        }
        out
    }

    fn reap_log(&self) -> Vec<Reaping> {
        self.reaps.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// **Empty, and it is a fact rather than a gap.** Every pid this machine's harness
    /// manages is *here*, and a command on the remote cannot signal it — there is no
    /// process namespace shared with that machine. A pattern that matched one of them is
    /// a pattern about a different box.
    fn protected(&self) -> Vec<Protected> {
        Vec::new()
    }

    fn scope_for(&self, kind: ScopeKind, name: Option<&str>) -> Result<ScopeId, ExecError> {
        Ok(self.scope_of(kind, name))
    }

    fn confinement_summary(&self) -> Option<String> {
        Some(format!(
            "the whole of {}: this is the operator's machine with the operator's files, \
             reached over ssh. No boundary — the gate decides, and it is the host's gate",
            self.host
        ))
    }

    /// The checkout, on the **remote's** filesystem. Nothing local joins a path to it:
    /// `monitors` is `None` here, and it is the one caller that would.
    fn workspace(&self) -> Option<&Path> {
        Some(Path::new(&self.worktree))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = r#"
# the operator's machines
[host.lab2x1]
hostname = "lab2x1"
user     = "dead"
key      = "~/.config/letibot/keys/lab2x1"
root     = "~/letibot-remote"

[host."bench-2"]
hostname = "10.0.0.7"
user     = "root"
"#;

    #[test]
    fn a_missing_file_is_no_hosts() {
        let h = Hosts::load_from("/nonexistent/hosts.toml");
        assert!(h.is_empty());
        assert!(h.names().is_empty());
    }

    #[test]
    fn the_file_the_contract_documents_parses() {
        let h = Hosts::parse(FILE).expect("it parses");
        assert_eq!(h.names(), vec!["bench-2".to_string(), "lab2x1".to_string()]);
        let lab = h.get("lab2x1").expect("lab2x1");
        assert_eq!(lab.hostname, "lab2x1");
        assert_eq!(lab.user.as_deref(), Some("dead"));
        assert_eq!(lab.root, "~/letibot-remote");
        assert_eq!(lab.destination(), "dead@lab2x1");
        assert!(
            lab.key.as_ref().is_some_and(|k| k.ends_with("keys/lab2x1")),
            "a `~` in a key is this machine's home: {:?}",
            lab.key
        );
        // The two defaults: hostname falls back to the name, root to the contract's.
        let bench = h.get("bench-2").expect("a quoted section name");
        assert_eq!(bench.hostname, "10.0.0.7");
        assert_eq!(bench.root, "~/letibot-remote");
        assert_eq!(bench.destination(), "root@10.0.0.7");
    }

    #[test]
    fn a_wrong_host_is_told_which_ones_are_there() {
        let h = Hosts::parse(FILE).expect("it parses");
        let said = h.refusal("lab2");
        assert!(said.contains("lab2x1"), "{said}");
        assert!(said.contains("bench-2"), "{said}");
        assert!(said.contains("Nothing was placed"), "{said}");
        let none = Hosts::default().refusal("lab2x1");
        assert!(none.contains("no `[host."), "{none}");
    }

    #[test]
    fn a_branch_name_is_eight_safe_characters() {
        assert_eq!(short_branch_id("s-17ab93cd-ef01-2345"), "ef012345");
        assert_eq!(short_branch_id("x"), "x");
        assert_eq!(short_branch_id("---"), "session");
    }

    #[test]
    fn quoting_survives_a_quote() {
        assert_eq!(quote("plain/path.ext"), "plain/path.ext");
        assert_eq!(quote("it's"), r#"'it'\''s'"#);
        assert_eq!(quote(""), "''");
    }

    #[test]
    fn the_ssh_argv_is_the_contract_flags_then_dest_then_command() {
        let h = Hosts::parse(FILE).expect("it parses");
        let ssh = Ssh::new(h.get("lab2x1").expect("lab2x1").clone());
        let argv = ssh.argv("bash -lc 'true'");
        let flags = argv.join(" ");
        assert!(flags.contains("-o IdentitiesOnly=yes"), "{flags}");
        assert!(flags.contains("-o BatchMode=yes"), "{flags}");
        assert!(flags.contains("ControlMaster=auto"), "{flags}");
        assert!(flags.contains("lab2x1"), "{flags}");
        assert_eq!(argv[argv.len() - 2], "dead@lab2x1");
        assert_eq!(argv[argv.len() - 1], "bash -lc 'true'");
    }

    #[test]
    fn the_walk_prints_what_tree_in_walk_order_parses() {
        // The format is firecode's `find -printf '%y\t%s\t%p'` by another hand, so the
        // one parser serves both substrates — and the walk runs on the far side of an
        // ssh, where GNU's `find` may not be.
        let dir = std::env::temp_dir().join(format!(
            "letibot-walk-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(dir.join("sub")).expect("tree");
        std::fs::write(dir.join("sub/a.txt"), b"hello").expect("file");
        std::fs::write(dir.join("b.txt"), b"hi").expect("file");
        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(format!(
                "{} walk {}",
                TREE_WALK,
                quote(&dir.display().to_string())
            ))
            .output()
            .expect("bash");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let (entries, _) = crate::firecode::tree_in_walk_order(
            &out.stdout,
            &dir.display().to_string(),
            100,
            &|_| false,
        );
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["b.txt", "sub", "a.txt"], "{names:?}");
        assert_eq!(entries[0].bytes, 2, "the file's size, not a directory's");
        std::fs::remove_dir_all(&dir).ok();
    }
}
