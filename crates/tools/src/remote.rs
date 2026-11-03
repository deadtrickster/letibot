//! Remote hosts: a session whose tools run on another machine over ssh, through an
//! [`ExecBackend`] that speaks ssh where firecode's speaks `firecode in`. The model
//! connection, the store and the gate stay in the local daemon.

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
use crate::rpc;

/// One machine the operator lets a session run on. Read from the operator's own file and never a
/// project's: a repository must not be able to name a machine to run on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Host {
    /// The name used everywhere else: `--host lab2x1`, `where: "host:lab2x1"`.
    pub name: String,
    /// What `ssh` connects to. Defaults to the name, which is what an alias in the
    /// ssh client's own config needs and is the common case.
    pub hostname: String,
    pub user: Option<String>,
    /// The identity file, with a leading `~` expanded here because it names a file here.
    /// `None` means the dedicated [`Paths::default_key`]; [`identity_for`] turns either
    /// into the file ssh is given.
    pub key: Option<PathBuf>,
    /// Directories to put in front of the remote `PATH`, as the remote shell must spell them:
    /// `~` is expanded there, once, and the far side's own `PATH` follows. Per host, because a
    /// login shell's `PATH` is a fact about that machine.
    pub path: Vec<String>,
    /// Where remote worktrees live, as the remote shell writes it. `~` is expanded there,
    /// once, at placement ([`SshBackend::place`]), so the paths this backend hands around are
    /// absolute and mean the same thing to `git fetch` a minute later.
    pub root: String,
    /// How the host's toolchains are found. `None` is probe and cache, which is what every
    /// host without a `mise` line gets. See [`Mise`].
    pub mise: Option<Mise>,
    /// Cores this machine's owner keeps for themselves, `reserve` in `hosts.toml`. Placement is
    /// by cores minus load minus reserve; [`Host::spare_cores`] does the subtraction, and it is
    /// reported and never enforced.
    pub reserve: u32,
    /// What this machine is: `flags` in `hosts.toml`, free-form words such as `heavy` or `tmpfs`.
    /// The set is open on purpose (a closed vocabulary would refuse the flag somebody needs next
    /// week) and the spelling is closed, so a typo is a parse error.
    pub flags: Vec<String>,
    /// What letibot must never signal, stop or remove on this machine: `never_touch` in
    /// `hosts.toml`. The operator's own workloads live here beside every agent's and nothing on the
    /// far side knows whose is whose, so it is checked before any signal, stop or remove.
    pub never_touch: Vec<String>,
}

/// Toolchains through `mise`, which is how the operator's machines are set up. `mise` is not on a
/// non-login shell's `PATH`, so a remote command that runs without it sees the wrong `cargo` or
/// none at all; that is why the wrapper is the default and `path` is the escape hatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mise {
    /// `mise = false`: do not wrap. A host whose toolchain is on `PATH`, or one where mise is
    /// broken, says so here instead of paying a probe and a wrapper on every call.
    Off,
    /// `mise = "~/.local/bin/mise"`: this program, expanded on the remote, once.
    At(String),
}

impl Host {
    /// `user@hostname`, or the bare name: the argument ssh takes.
    pub fn destination(&self) -> String {
        match &self.user {
            Some(u) => format!("{u}@{}", self.hostname),
            None => self.hostname.clone(),
        }
    }

    /// The pattern on this host's never-touch list that matches `what`, if one does:
    /// [`never_touch_hit`]'s rule over this host's list.
    pub fn never_touch_hit(&self, what: &str) -> Option<&str> {
        never_touch_hit(&self.never_touch, what)
    }

    /// What is left of this machine for anyone else: cores minus load minus reserve, the load
    /// being the first number of `/proc/loadavg`. `None` when the host answered neither, which is
    /// not `0.0`; negative is reported as it is, because a full box is a fact.
    pub fn spare_cores(&self, cores: u64, load: &str) -> Option<f64> {
        if cores == 0 {
            return None;
        }
        let load1: f64 = load.split_whitespace().next()?.parse().ok()?;
        Some(cores as f64 - load1 - self.reserve as f64)
    }

    /// Is this flag declared? Case-insensitive, because a flag is a word a person types.
    pub fn has_flag(&self, flag: &str) -> bool {
        self.flags.iter().any(|f| f.eq_ignore_ascii_case(flag))
    }
}

/// The operator's hosts, from `~/.config/letibot/hosts.toml`.
///
/// ```toml
/// [host.lab2x1]
/// hostname = "lab2x1"
/// user     = "dead"
/// key      = "~/.config/letibot/keys/lab2x1"
/// root     = "~/letibot-remote"
///
/// [local]
/// never_touch = ["etcd", "pgfuzz"]
/// ```
///
/// A missing file means no hosts, and that is not a fault: a box that places
/// nothing remotely is every box until somebody writes the file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hosts {
    hosts: BTreeMap<String, Host>,
    /// What the operator said about THIS box: the `[local]` table. A section of its own rather
    /// than a `[host.local]`, because this box is not a placement. It holds the declaration a
    /// process record on this box is checked against, the same list and the same match.
    local: LocalBox,
}

/// The `[local]` table: what this box's owner keeps for themselves.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalBox {
    /// What letibot must never signal, stop or remove on this machine. One key, because it is the
    /// only question a process-record check can ask about a box that is not a placement.
    pub never_touch: Vec<String>,
}

impl LocalBox {
    /// See [`never_touch_hit`]: one list, one rule, one spelling of the match.
    pub fn never_touch_hit(&self, what: &str) -> Option<&str> {
        never_touch_hit(&self.never_touch, what)
    }
}

/// The pattern on a never-touch list that matches `what`, if one does. The one copy of the rule,
/// because three lists read it. The match is a case-insensitive substring: missing `etcd-fuzz-0`
/// because the list said `etcd` is the silent failure this list exists to prevent.
pub fn never_touch_hit<'a>(list: &'a [String], what: &str) -> Option<&'a str> {
    let lower = what.to_lowercase();
    list.iter()
        .find(|p| !p.is_empty() && lower.contains(&p.to_lowercase()))
        .map(String::as_str)
}

/// Where the operator's hosts are written. `$LETIBOT_HOSTS` is a test's door and nothing else's:
/// it is an environment variable, and a project that could set one could name a machine, which is
/// the whole thing this file exists to prevent.
pub fn hosts_file() -> PathBuf {
    if let Some(p) = std::env::var_os("LETIBOT_HOSTS").filter(|p| !p.as_os_str().is_empty()) {
        return PathBuf::from(p);
    }
    // The config directory is [`Paths`]'s rule, asked rather than restated: two readers of one
    // file that disagree about where it is would be the defect this package exists to remove.
    Paths::from_env().config().join("hosts.toml")
}

impl Hosts {
    /// The operator's hosts. A missing file is no hosts; an unreadable one is no hosts and a line on
    /// stderr, because a `--host lab2x1` that fails because of a typo in a file the operator forgot
    /// they wrote is a refusal that has to name the file.
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

    /// The file's grammar, and its refusals: `[host.NAME]` tables of nine keys, refused by name when
    /// unknown, stated twice, or not one line of quoted strings. Nothing beyond the grammar is
    /// validated here; whether a host may be used is asked when it is used ([`Ssh::connect`]).
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
            let key = k.trim();
            let put = sections.entry(sec.clone()).or_default();
            if put.contains_key(key) {
                return Err(format!(
                    "line {}: `{key}` is stated twice in `[{sec}]`. Refused rather than taken \
                     last-wins — which of the two was meant is not something this file can say.",
                    n + 1
                ));
            }
            put.insert(key.to_string(), v.trim().to_string());
        }
        let mut hosts = BTreeMap::new();
        let mut local = LocalBox::default();
        for (sec, kv) in sections {
            // `[local]` is THIS box, and it is read before `[host....]` because it is not one: this
            // section names what the machine underneath the daemon keeps for itself. One key, refused
            // by name, because a `never_tuch` typo would leave the operator's workload unprotected.
            if sec == "local" {
                if let Some(key) = kv.keys().find(|k| k.as_str() != "never_touch") {
                    return Err(format!(
                        "`[local].{key}` is not a key this section has: never_touch. `[local]` \
                         declares what letibot must never signal, stop or remove on the machine \
                         the daemon runs on — nothing else about this box is a placement, so \
                         there is nothing else to say here."
                    ));
                }
                local.never_touch = match kv.get("never_touch") {
                    Some(raw) => path_list(raw).ok_or_else(|| {
                        format!(
                            "`[local].never_touch` must be one line of quoted strings — \
                             `never_touch = [\"a\", \"b\"]` — and `{raw}` is not that."
                        )
                    })?,
                    None => Vec::new(),
                };
                continue;
            }
            let Some(raw_name) = sec.strip_prefix("host.") else {
                continue;
            };
            let name = raw_name.trim_matches('"').to_string();
            for key in kv.keys() {
                if !HOST_KEYS.contains(&key.as_str()) {
                    return Err(format!(
                        "`host.{name}.{key}` is not a key this file has: {}. Refused rather than \
                         ignored — a typo here places a session on a machine nobody named, or \
                         points ssh at a key nobody meant.",
                        HOST_KEYS.join(", ")
                    ));
                }
            }
            let get = |k: &str| {
                kv.get(k)
                    .map(|v| v.trim().trim_matches('"'))
                    .filter(|v| !v.is_empty())
            };
            let path = match kv.get("path") {
                Some(raw) => path_list(raw).ok_or_else(|| {
                    format!(
                        "`host.{name}.path` must be one line of quoted strings — \
                         `path = [\"~/.cargo/bin\", \"/opt/bin\"]` — and `{raw}` is not that."
                    )
                })?,
                None => Vec::new(),
            };
            // Two more lists, one grammar: `flags` and `never_touch` are the same shape as `path` and
            // are refused the same way, because a quietly half-read list is a rule kept in name only.
            // A dropped `never_touch` is a teardown that stops the operator's own workload.
            let list = |key: &str| match kv.get(key) {
                Some(raw) => path_list(raw).ok_or_else(|| {
                    format!(
                        "`host.{name}.{key}` must be one line of quoted strings — \
                         `{key} = [\"a\", \"b\"]` — and `{raw}` is not that."
                    )
                }),
                None => Ok(Vec::new()),
            };
            let flags = list("flags")?;
            let never_touch = list("never_touch")?;
            // `reserve` is a count of cores, so a number and nothing else. Refused rather than
            // defaulted: a `reserve = "two"` read as zero would be a scheduler that believes a busy
            // machine is empty.
            let reserve = match kv.get("reserve") {
                None => 0,
                Some(raw) => {
                    let raw = raw.trim().trim_matches('"');
                    match raw.parse::<u32>() {
                        Ok(n) => n,
                        Err(_) => {
                            return Err(format!(
                                "`host.{name}.reserve` is a whole number of cores — \
                                 `reserve = 4` — and `{raw}` is not. Leave the line out for 0."
                            ));
                        }
                    }
                }
            };
            // `mise` is two shapes in one key: `false` turns the wrapper off, and a string is the
            // program to run. A `true` is refused rather than read as "find it yourself", because that
            // is what leaving the line out already means.
            let mise = match kv.get("mise") {
                None => None,
                Some(raw) => {
                    let raw = raw.trim();
                    if raw == "false" {
                        Some(Mise::Off)
                    } else if raw.starts_with('"') || raw.starts_with('\'') {
                        get("mise").map(|v| Mise::At(v.to_string()))
                    } else {
                        return Err(format!(
                            "`host.{name}.mise` is either `false` (do not run commands through \
                             mise) or the program's path in quotes — `mise = \"~/.local/bin/mise\"`. \
                             `{raw}` is neither, and a `true` is what leaving the line out means."
                        ));
                    }
                }
            };
            hosts.insert(
                name.clone(),
                Host {
                    hostname: get("hostname").unwrap_or(&name).to_string(),
                    user: get("user").map(str::to_string),
                    key: get("key").map(|k| expand_tilde(Path::new(k))),
                    path,
                    root: get("root").unwrap_or("~/letibot-remote").to_string(),
                    mise,
                    reserve,
                    flags,
                    never_touch,
                    name,
                },
            );
        }
        Ok(Hosts { hosts, local })
    }

    /// What this box's owner keeps for themselves: the `[local]` table, empty when the file says
    /// nothing about this machine.
    pub fn local(&self) -> &LocalBox {
        &self.local
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

    /// What a wrong host name is told. The known names, because a refusal a caller cannot act on is
    /// the refusal that costs an hour, and "no host named `lab2`" when the file says `lab2x1` is
    /// exactly that refusal.
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
             `[host.{name}]` with `hostname` (what ssh connects to), `user`, `key` (the \
             dedicated identity file, default `~/.config/letibot/keys/{name}`), `path` \
             (directories to put in front of the remote `PATH`), `root` (where remote \
             worktrees live, default `~/letibot-remote`), `mise` (the toolchain wrapper, \
             default: found once and cached), `reserve` (cores held back for the owner), \
             `flags` (words such as `heavy` or `tmpfs`) and `never_touch` (process, compose \
             and container names letibot must never signal, stop or remove there). The machine \
             this daemon runs on has a section of its own — `[local]` with `never_touch` — which \
             is what a process record with no host is checked against.",
            hosts_file().display()
        )
    }
}

/// The keys a `[host.NAME]` table may carry. Anything else is refused by name: this file decides
/// which machine a session runs on, which file ssh reads, and what a teardown may never touch, and
/// a typo in it is not a detail to be ignored.
const HOST_KEYS: &[&str] = &[
    "hostname",
    "user",
    "key",
    "path",
    "root",
    "mise",
    "reserve",
    "flags",
    "never_touch",
];

/// `["a", "b"]` -> the strings, with the quotes and spaces off, and an empty list for `[]`. A tiny
/// reader rather than a TOML one: this file's grammar is a line of `key = value`, and this is the only
/// value that is not a plain string. What it refuses, it refuses by name (see [`Hosts::parse`]).
fn path_list(raw: &str) -> Option<Vec<String>> {
    let inner = raw.trim().strip_prefix('[')?.strip_suffix(']')?;
    let mut out = Vec::new();
    for item in inner.split(',') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        let unquoted = item.trim_matches('"').trim_matches('\'').trim();
        if unquoted.is_empty() {
            return None;
        }
        out.push(unquoted.to_string());
    }
    Some(out)
}

/// `~/x` in a key path, expanded here, because it names a file on this machine.
fn expand_tilde(p: &Path) -> PathBuf {
    expand(p, &Paths::from_env().home)
}

// The ssh policy, in one place for both callers: [`SshBackend`] (a session whose tools run
// elsewhere) and `letibot --migrate`. The migration calls this code rather than a copy, so the two
// cannot drift on `-F`, the host key or the identity file.

/// letibot's own known-hosts file, inside [`Paths::config`], and not the operator's: a host key
/// accepted for letibot is not a host key accepted for their own `git push`.
pub const KNOWN_HOSTS_FILE: &str = "known_hosts";

/// Where a host's dedicated identity file lives, inside [`Paths::config`].
pub const KEYS_DIR: &str = "keys";

/// The two directories an ssh call is decided against: the operator's home, inside which one
/// directory is off limits, and letibot's own config directory, where everything it does read lives.
/// A value rather than an environment read at each use, so a test pins the home it means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    home: PathBuf,
    config: PathBuf,
}

impl Paths {
    /// This box's: `$HOME`, and `$XDG_CONFIG_HOME/letibot` (or `$HOME/.config/letibot`).
    pub fn from_env() -> Paths {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|h| !h.as_os_str().is_empty())
            .unwrap_or_else(|| PathBuf::from("/"));
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute() && !p.as_os_str().is_empty())
            .map(|x| x.join("letibot"))
            .unwrap_or_else(|| home.join(".config").join("letibot"));
        Paths { home, config }
    }

    /// A test's own: `home`, with the config directory where a real operator's would be.
    pub fn in_home(home: impl Into<PathBuf>) -> Paths {
        let home = home.into();
        let config = home.join(".config").join("letibot");
        Paths { home, config }
    }

    /// A test's own, with the config directory somewhere other than under the home, the shape a box
    /// with `XDG_CONFIG_HOME` set has.
    pub fn with_config(mut self, config: impl Into<PathBuf>) -> Paths {
        self.config = config.into();
        self
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn config(&self) -> &Path {
        &self.config
    }

    pub fn known_hosts(&self) -> PathBuf {
        self.config.join(KNOWN_HOSTS_FILE)
    }

    pub fn keys(&self) -> PathBuf {
        self.config.join(KEYS_DIR)
    }

    /// `keys/<host>` under the config directory: the key a host uses when the file says nothing.
    pub fn default_key(&self, host: &str) -> PathBuf {
        self.keys().join(sanitise(host))
    }

    /// The operator's own ssh directory: the one letibot may not read, and the one a key may not
    /// be in. Built by a join rather than spelled, so this file has one place that says it.
    pub fn user_ssh(&self) -> PathBuf {
        self.home.join(".ssh")
    }
}

/// A host name as a file name: a table key may carry a `.` or a `-`, and neither is a problem, but
/// a `/` would be a directory and nothing else belongs in a path letibot writes.
fn sanitise(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// `~/x` -> `<home>/x`. A `~` left unexpanded would name a directory called `~`, and a path that
/// does not exist is not one any rule can be asked about.
pub fn expand(path: &Path, home: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix("~/") {
        return home.join(rest);
    }
    if s == "~" {
        return home.to_path_buf();
    }
    path.to_path_buf()
}

/// The real path of `path`, following symlinks, for a path that need not exist yet: `canonicalize`
/// answers only about a path that is there, and a rule that quietly stops applying to a path that is
/// not is a hole. The deepest existing ancestor is canonicalised and the rest applied lexically.
pub fn real(path: &Path) -> PathBuf {
    let mut probe = path.to_path_buf();
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if let Ok(real) = probe.canonicalize() {
            let mut out = real;
            for name in rest.iter().rev() {
                if name == ".." {
                    out.pop();
                } else if name != "." {
                    out.push(name);
                }
            }
            return out;
        }
        let Some(name) = probe.file_name().map(|n| n.to_os_string()) else {
            return path.to_path_buf();
        };
        rest.push(name);
        let Some(parent) = probe.parent() else {
            return path.to_path_buf();
        };
        if parent == probe {
            return path.to_path_buf();
        }
        probe = parent.to_path_buf();
    }
}

/// Is `path` inside the operator's ssh directory, after symlinks, and about a path that may not
/// exist. The rule is about where the bytes are, so a link in the workspace named `keys` and pointing
/// at that directory is that directory.
pub fn inside_user_ssh(path: &Path, home: &Path) -> bool {
    let path = expand(path, home);
    let store = real(&home.join(".ssh"));
    let real_path = real(&path);
    real_path == store || real_path.starts_with(&store)
}

/// The refusal the operator's rule gives, in their own words.
pub fn dot_ssh_refusal(path: &Path, home: &Path) -> String {
    format!(
        "`{}` resolves inside the operator's own ssh directory (`{}`), and the operator's rule \
         (2026-10-26) is \"letibot is forbidden to touch and even read `~/.ssh`\". Nothing was read \
         and nothing was run — this is refused before any mode, grant or adjudicator is consulted, \
         and none of them can override it. Followed through every symlink, and asked of a path that \
         does not exist yet, because the spelling is not the rule.",
        path.display(),
        home.join(".ssh").display()
    )
}

/// The identity file a host connects with: the one `hosts.toml` names, or the dedicated `keys/<host>`
/// under the config directory, after the two refusals that make it letibot's own - it is absolute, it
/// exists, and it does not resolve into the operator's ssh directory.
pub fn identity_for(host: &str, declared: Option<&Path>, paths: &Paths) -> Result<PathBuf, String> {
    let key = match declared {
        Some(p) => {
            let expanded = expand(p, paths.home());
            if !expanded.is_absolute() {
                return Err(format!(
                    "`[host.{host}] key = \"{}\"` is a relative path, and a key named relative to \
                     the directory a daemon happened to start in is a key that stops being found \
                     the moment something else starts it. Say it absolutely — `{}` is the default.",
                    p.display(),
                    paths.default_key(host).display()
                ));
            }
            expanded
        }
        None => paths.default_key(host),
    };
    if inside_user_ssh(&key, paths.home()) {
        return Err(format!(
            "`[host.{host}]` uses `{}` as its key, which is inside the operator's own ssh \
             directory — and the operator's rule (2026-10-26) is \"letibot is forbidden to touch \
             and even read `~/.ssh`\". Nothing was placed and nothing was connected. A host's key \
             is its own file, so the operator's ssh material is never what letibot runs with:\n    \
             ssh-keygen -t ed25519 -N '' -f {key}\n    ssh-copy-id -i {key}.pub <user>@<host>\n\
             and leave `key` out of the file, since that is its default.",
            key.display(),
            key = paths.default_key(host).display()
        ));
    }
    if !key.is_file() {
        return Err(format!(
            "`[host.{host}]` needs the key `{}` and there is no file there. A host's key is a \
             dedicated one under letibot's own directory — the operator's ssh material is not \
             something letibot may read. Make it:\n    \
             ssh-keygen -t ed25519 -N '' -f {key}\n    ssh-copy-id -i {key}.pub <user>@{host}\n\
             and `key` may be left out of `hosts.toml`, since that path is the default.",
            key.display(),
            key = key.display()
        ));
    }
    Ok(key)
}

/// Is this host's key already in letibot's own known-hosts file? The format is ssh's, one entry per
/// line, `<host>[,<host>...] <type> <base64>`, with a bracketed `[host]:port` for a non-default port.
/// A hashed entry (`|1|...`) reads as absent, and a wildcard is not honoured.
pub fn host_key_known(file: &Path, hostname: &str) -> bool {
    let Ok(text) = std::fs::read_to_string(file) else {
        return false;
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(field) = line.split_whitespace().next() else {
            continue;
        };
        for name in field.split(',') {
            let name = name.trim_start_matches('!');
            if name == hostname {
                return true;
            }
            // `[host]:2222`: the same machine on a port ssh was told about.
            if let Some(inner) = name.strip_prefix('[').and_then(|n| n.split(']').next())
                && inner == hostname
            {
                return true;
            }
        }
    }
    false
}

/// The refusal for a host whose key nobody has accepted here, with the one command that adds it -
/// printed for the operator, never run by letibot. A key it scanned and trusted on its own is a key
/// nobody checked.
pub fn unknown_host_key(hostname: &str, paths: &Paths) -> String {
    let file = paths.known_hosts();
    format!(
        "`{hostname}` is not in letibot's own known-hosts file ({}), so there is no host key here \
         to check the machine against — and ssh runs with `StrictHostKeyChecking=yes` and \
         `BatchMode=yes`, which means it would fail rather than ask. Nothing was connected and \
         nothing was placed. Add it once, yourself:\n    \
         mkdir -p {} && ssh-keyscan -t ed25519 {hostname} >> {}\n\
         and read the key it prints before you keep it (no `-H`: a hashed entry cannot be checked \
         here).",
        file.display(),
        paths.config().display(),
        file.display()
    )
}

/// Every flag every letibot ssh runs with, in one list. `-F /dev/null` comes first, so the operator's
/// ssh client config is not read at all, then the files ssh may read, what it may do, the connection,
/// and the identity. `-i` and `IdentitiesOnly=yes` are together on purpose.
pub fn flags(key: &Path, control: &Path, paths: &Paths) -> Vec<String> {
    let mut v: Vec<String> = vec!["-F".into(), "/dev/null".into()];
    for (k, val) in [
        // letibot's own file, and the system's refused: the host keys letibot trusts are the ones the
        // operator put there for it.
        (
            "UserKnownHostsFile",
            paths.known_hosts().display().to_string(),
        ),
        ("GlobalKnownHostsFile", "/dev/null".to_string()),
        // An unknown host key is refused, never learned. `accept-new` would trust whatever
        // answered first, and BatchMode means there is nobody to ask.
        ("StrictHostKeyChecking", "yes".to_string()),
        // A host that wants a password is refused by name rather than prompted for: a daemon has
        // no terminal, and a prompt it cannot answer is a call that hangs until its deadline.
        ("BatchMode", "yes".to_string()),
        // The agent is the operator's own keys: ssh would offer every one of them, in ssh's own order,
        // before the one file it was given.
        ("IdentityAgent", "none".to_string()),
        ("IdentitiesOnly", "yes".to_string()),
        // One TCP connection per host: a command then costs a round trip, not a handshake. The
        // socket is in letibot's rundir ([`control_path`]).
        ("ControlMaster", "auto".to_string()),
        ("ControlPath", control.display().to_string()),
        ("ControlPersist", "120".to_string()),
        ("ConnectTimeout", "20".to_string()),
        // A TCP connection that is up is not a host that is answering: ConnectTimeout covers only the
        // handshake, and a link that dies leaves a socket that accepts writes and answers nothing for
        // ever. These two ask for a sign of life every 5 s and give up after three misses.
        ("ServerAliveInterval", "5".to_string()),
        ("ServerAliveCountMax", "3".to_string()),
    ] {
        v.push("-o".into());
        v.push(format!("{k}={val}"));
    }
    v.push("-i".into());
    v.push(key.display().to_string());
    v
}

/// A host's `root` as the remote shell must spell it: `~` expanded THERE, once, and the rest quoted.
/// `~` inside single quotes is a literal character, so a quoted root makes a directory called `~`; a
/// root is the one path that WANTS the expansion, so it is spliced in as `"$HOME"/...`.
pub fn expand_root(root: &str) -> String {
    one_entry(root)
}

/// letibot's runtime directory - the launcher's rule, restated here because this crate is below the
/// launcher (`crates/letibot` depends on this one, not the other way round).
pub fn rundir() -> PathBuf {
    let base = match std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.as_os_str().is_empty()) {
        Some(d) => PathBuf::from(d),
        None => std::env::temp_dir(),
    };
    base.join("letibot")
}

/// The `ControlMaster` socket for one host and one daemon, under [`rundir`]. The pid is in the name
/// because one socket per host is one OpenSSH connection with `MaxSessions` (10) slots, and every
/// daemon on the box asking at once is refused by the server or lost by the client.
pub fn control_path(host: &str, pid: u32) -> PathBuf {
    control_path_for(host, pid, "call")
}

/// The socket this daemon's git work multiplexes over: [`control_path`] with a role of its own.
/// `git fetch` starts an ssh this crate does not run, so it passes no [`rpc::enter`] and could be the
/// ninth session on the call master. A second socket makes the two workloads unable to meet.
pub fn git_control_path(host: &str, pid: u32) -> PathBuf {
    control_path_for(host, pid, "git")
}

fn control_path_for(host: &str, pid: u32, role: &str) -> PathBuf {
    let short = sanitise(host);
    let preferred = rundir().join(format!("ssh-{short}-{pid}-{role}.sock"));
    if preferred.as_os_str().len() <= 100 {
        return preferred;
    }
    PathBuf::from("/tmp").join(format!("letibot-ssh-{pid}-{short}-{role}.sock"))
}

/// The host's `path` list as the export a remote command starts with, or `None` when the file
/// declares none, so a command's text is unchanged for every host that does not need it. Expanded by
/// the remote shell and prepended, never replaced: the far side's own `PATH` finds `git` and `bash`.
pub fn path_export(entries: &[String]) -> Option<String> {
    let entries = quoted_entries(entries)?;
    Some(format!("export PATH={}:\"$PATH\"", entries.join(":")))
}

/// The same list as a prefix assignment rather than an export, for a remote command that is not a
/// script and so has nowhere to put an `export`: git's `upload-pack`, which ssh runs as
/// `<this> '<path>'`.
pub fn path_prefix(entries: &[String]) -> Option<String> {
    let entries = quoted_entries(entries)?;
    Some(format!("PATH={}:\"$PATH\"", entries.join(":")))
}

/// The list as shell words, or `None` when there is nothing to say.
fn quoted_entries(entries: &[String]) -> Option<Vec<String>> {
    let entries: Vec<String> = entries
        .iter()
        .map(|e| e.trim())
        .filter(|e| !e.is_empty())
        .map(one_entry)
        .collect();
    if entries.is_empty() {
        None
    } else {
        Some(entries)
    }
}

/// One `path` entry as the remote shell must see it. A `~` handed to `quote()` would be a literal
/// character, so `~/.cargo/bin` would name a directory called `~` on the far side; the tilde is
/// expanded there, once, as `"$HOME/..."`, and the rest of the entry is quoted as any word is.
fn one_entry(e: &str) -> String {
    // The characters `quote()` leaves bare. An entry made only of them needs no quoting at all.
    let plain = |s: &str| {
        !s.is_empty()
            && s.chars().all(|c| {
                c.is_ascii_alphanumeric()
                    || matches!(c, '/' | '.' | '_' | '-' | ':' | '+' | ',' | '@')
            })
    };
    match e.strip_prefix("~/") {
        Some(rest) if plain(rest) => format!("\"$HOME/{rest}\""),
        Some(rest) => format!("\"$HOME\"/{}", quote(rest)),
        None if e == "~" => "\"$HOME\"".to_string(),
        None => quote(e),
    }
}

impl Host {
    /// What this host's `path` list does to a remote script, or the script itself.
    pub fn with_path(&self, script: &str) -> String {
        match path_export(&self.path) {
            Some(export) => format!("{export}; {script}"),
            None => script.to_string(),
        }
    }

    /// The host's `path` list as a command prefix, for the remote command that is not a script.
    pub fn path_prefix(&self) -> Option<String> {
        path_prefix(&self.path)
    }
}

/// How long one ssh call that is not a command may take before this box stops waiting: a read, a
/// write, a listing, a poll, a probe, a kill. None is work the operator asked to run, so a call past
/// this bound has its local ssh killed and the operation fails, naming the host and the step.
pub const STEP_BOUND: Duration = Duration::from_secs(20);

/// The bound on one status poll ([`RemoteProcesses::refresh`]), shorter than [`STEP_BOUND`] because a
/// poll's answer arrives for the next tick: a `job_wait` polling every 120 ms must still return within
/// the deadline it was given.
const POLL_BOUND: Duration = Duration::from_secs(5);

/// The bound on the calls that only ask about the connection - `ssh -O check` and `-O exit`. They are
/// local IPC to a socket with no network in them at all, so anything past this is a master that has
/// stopped answering.
const MASTER_BOUND: Duration = Duration::from_secs(3);

/// The bound on the local `git` work a placement does: `bundle create`, the `fetch` back. Generous
/// because it is real work on a real repository: the number only has to be below "for ever".
pub const GIT_BOUND: Duration = Duration::from_secs(300);

/// One fetch's wall clock: a slot wait, a run and the retry all come out of one bound - the
/// [`GIT_BOUND`] a close is given, because a close that asked for one transfer must not cost four.
struct FetchBudget {
    began: Instant,
    /// The whole fetch's bound, so a test can measure it in seconds where production spends
    /// minutes: [`Ssh::with_bound`]'s reason, one layer in.
    within: Duration,
}

impl FetchBudget {
    fn new(within: Duration) -> Self {
        FetchBudget {
            began: Instant::now(),
            within,
        }
    }

    /// What is left of the fetch's own bound, and never less than nothing.
    fn left(&self) -> Duration {
        self.within.saturating_sub(self.began.elapsed())
    }

    /// What a wait may take: no longer than the caller's own bound, and never past the fetch's.
    fn wait(&self, connection: Duration) -> Duration {
        connection.min(self.left())
    }

    /// How long the fetch has been running: what a kill is reported against, because the production
    /// bound is not the only one a caller can give.
    fn spent(&self) -> Duration {
        self.began.elapsed()
    }
}

/// How a fetch ended, so a kill is reported against the clock it was given rather than [`GIT_BOUND`],
/// which a caller and a test both may override.
#[derive(Debug)]
enum FetchEnded {
    /// git answered, and its exit status and streams are the answer.
    Ran(std::process::Output),
    /// Nothing answered within the bound, which had passed after this long.
    Killed(Duration),
}

/// What a caller may do when a call does not answer at all. Retrying follows idempotency: a `read`
/// whose answer never arrived changed nothing, so it is asked once more; a `bash` command that may
/// still be running is never asked twice, because a second run is a second effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Retry {
    /// Asking again is the same as asking once: nothing changed, or the operation lands
    /// atomically (a temp file and one `mv -f`).
    Once,
    /// Never asked twice: the far side may have acted, partly acted, or be acting still.
    Never,
}

/// What a caller really did about a call that did not answer, as the refusal has to say it: a
/// reader acts on that sentence, so it may claim only the attempt that was made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Asked {
    /// One more attempt on a fresh connection, whatever the failure was.
    Again,
    /// One more attempt only where nothing can have run: a session the multiplexer refused.
    MuxOnly,
    /// Nothing: the caller makes one call and no second one ([`Ssh::raw`]'s callers).
    None,
}

impl Asked {
    /// What [`Ssh::call`] does with this policy, which is what [`Retry`] means.
    fn of(retry: Retry) -> Asked {
        match retry {
            Retry::Once => Asked::Again,
            Retry::Never => Asked::MuxOnly,
        }
    }
}

/// Whether a call owes the record a line: the ring, and how a heartbeat stays out of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reported {
    /// Every attempt goes into the ring: this is a call somebody asked for.
    Always,
    /// Only the attempts that did not answer. A status poll is a heartbeat - once every 120 ms for as
    /// long as a job is waited on - and [`crate::rpc::RECORD_RING`] entries of it would push out every
    /// call that mattered. A poll that fails is news, and is kept.
    Failures,
}

/// How one bounded ssh call ended: the shape the retry rule is written against.
enum Call {
    /// The far side answered: what the command printed, ssh's own stderr, and the exit status.
    Answered(String, Vec<u8>, i32),
    /// The bound passed with nothing to show for it. The local ssh was killed.
    TimedOut(Duration),
    /// ssh failed before the command could say anything: exit 255 with nothing on stdout, the
    /// connection dropped, the name does not resolve.
    Dropped(Vec<u8>),
    /// The far side said no and meant it: `Permission denied (publickey)`, a host key that does not
    /// verify. Definite: not a lost connection, never retried, and its sentence names the fix
    /// ([`Ssh::denied`]).
    Denied(AuthRefusal, Vec<u8>),
    /// The multiplexer refused a session, so the command never started: a full master (`MaxSessions`),
    /// a control socket that went away, an fd that would not pass. It is a transport failure about
    /// this call, and it is retried on a fresh connection whatever the caller's policy says.
    MuxRefused(MuxRefusal, Vec<u8>),
}

/// The four ways OpenSSH's multiplexer loses a session, and what each one proves. They look like four
/// faults and they are one: the connection is up, and this call did not get a channel on it. That is
/// why they are separated from [`Call::Dropped`], which says nobody knows what happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MuxRefusal {
    /// `mux_client_request_session: session request failed`: the master refused to open the session.
    /// With `SendEnv`/`SetEnv` or a full session table, the request never became a channel.
    Session,
    /// `mm_send_fd: sendmsg(1): Message too long`: the client could not hand the session's file
    /// descriptors to the master. It is the one that reads most like a broken pipe.
    Fds,
    /// `channel 0: open failed: administratively prohibited`: the server refused the channel -
    /// `MaxSessions` reached, or the server's own limit on the account.
    Channel,
    /// `Control socket connect ...: Connection refused`: the socket file is there and nothing is
    /// listening on it, so the master died or was killed.
    StaleSocket,
}

impl MuxRefusal {
    /// The short form a sentence and a test can name it by.
    pub fn as_str(self) -> &'static str {
        match self {
            MuxRefusal::Session => "the master refused the session",
            MuxRefusal::Fds => "the session's descriptors could not be handed over",
            MuxRefusal::Channel => "the server refused the channel (MaxSessions)",
            MuxRefusal::StaleSocket => "the control socket is there and nothing is listening",
        }
    }
}

/// The two ways ssh says *not you*. They are separated because the fix differs - a key that is not
/// installed, versus a machine whose key is not the one this box recorded - and a sentence that offered
/// both would send the operator to the wrong file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthRefusal {
    /// `Permission denied (publickey, ...)`.
    Key,
    /// `Host key verification failed.` / `No ... host key is known for ...`.
    HostKey,
}

/// Did the far side say anything at all? A refusal counts - `Permission denied` is a machine that is
/// up - and a lost connection or a passed deadline does not. A refused mux session counts too: a
/// master with no free slot is a busy path, not a host that is down.
fn connected(call: &Call) -> bool {
    matches!(
        call,
        Call::Answered(..) | Call::Denied(..) | Call::MuxRefused(..)
    )
}

/// Read ssh's own words as *refused* or *lost*: OpenSSH prints these and exits 255 with nothing on
/// stdout, which is the shape a dropped connection has. Matched as substrings rather than whole lines,
/// because they arrive prefixed by `user@host:` and a host-key line is preceded by another.
fn auth_refusal(stderr: &str) -> Option<AuthRefusal> {
    let lower = stderr.to_ascii_lowercase();
    if lower.contains("permission denied (") || lower.contains("too many authentication failures") {
        return Some(AuthRefusal::Key);
    }
    if lower.contains("host key verification failed")
        || lower.contains("host key is known for")
        || lower.contains("remote host identification has changed")
    {
        return Some(AuthRefusal::HostKey);
    }
    None
}

/// Read ssh's words for "the connection is fine, this session was not". These four arrive exactly
/// like a dropped connection - exit 255, nothing on stdout - and none of them can have run the
/// command: no channel ever existed, so the retry is safe even for an at-most-once operation.
fn mux_refusal(stderr: &str) -> Option<MuxRefusal> {
    let lower = stderr.to_ascii_lowercase();
    if lower.contains("mux_client_request_session") || lower.contains("session request failed") {
        return Some(MuxRefusal::Session);
    }
    if lower.contains("mm_send_fd")
        || (lower.contains("sendmsg") && lower.contains("message too long"))
    {
        return Some(MuxRefusal::Fds);
    }
    if lower.contains("open failed") || lower.contains("channel") && lower.contains("refused") {
        return Some(MuxRefusal::Channel);
    }
    if lower.contains("control socket connect") || lower.contains("mux_client") {
        return Some(MuxRefusal::StaleSocket);
    }
    None
}

/// A command wrapped so its remote process group is known: `setsid` makes the shell a session leader,
/// so its pid is its process group and `kill -TERM -<pid>` reaches everything it forks. The pid is
/// written to `file` before the command starts and removed when it ends normally.
fn grouped(script: &str, file: &str) -> String {
    let q = quote(file);
    let dir = match file.rfind('/') {
        Some(i) => quote(&file[..i]),
        None => ".".to_string(),
    };
    let inner = format!(
        "mkdir -p {dir} 2>/dev/null; echo $$ > {q}; ( {script} ); rc=$?; rm -f {q}; exit $rc"
    );
    format!(
        "if command -v setsid >/dev/null 2>&1; then setsid bash -c {}; else bash -c {}; fi",
        quote(&inner),
        quote(&inner)
    )
}

/// The bound on ending a remote process group: [`Ssh::cancel_group`]'s call. Ten seconds is long
/// enough to dial a fresh master and signal, short enough that a command which already spent its own
/// bound is not made to spend a second one twice.
const CANCEL_BOUND: Duration = Duration::from_secs(10);

/// The variable every command a placed session runs carries, whose value names the placement.
/// `LETIBOT_SESSION` is the whole of what a host with no cgroup can be asked later: *which of these
/// processes did this session start*. Nothing else tells a build of this session's apart from the
/// operator's own build on the same machine, so a sweep that did not read it would either kill their
/// work or leave this session's running to the end of the box.
pub const SESSION_MARK: &str = "LETIBOT_SESSION";

/// How long the sweep gets: its own TERM-and-wait is four seconds at the longest, and a host that has
/// stopped answering must not hold the daemon's exit open past a fresh master's timeout.
const END_SESSION_BOUND: Duration = Duration::from_secs(25);

/// The sweep [`Ssh::end_session_processes`] runs on the far side. One script and not a `pkill`: the
/// pids it finds are TERMed, waited for and then KILLed, so the answer is *what is left* rather than
/// *what was signalled*. The id arrives as `$1` because a mark spelled inside this text would appear
/// in the sweep's own command line, where it would find and kill itself.
///
/// Two readers, because the exact one is Linux's: `/proc/<pid>/environ` holds each process's
/// environment as it was at exec, whole and in NUL-separated fields, so ONE `grep -l` answers for the
/// whole machine; `ps eww` prints it as text after the command, which is all a Mac has. The two run in
/// SEQUENCE, so the `grep` whose own argv holds the mark has exited before `ps` looks for it.
///
/// Both match the mark as a WHOLE FIELD, never as a prefix. A placement's id ends in a decimal
/// counter, so `-1` is a prefix of `-12`: a prefix match ends another placement's work on the same
/// machine, and a call that was TERMed comes back as `exit 143` with nothing said.
const SESSION_SWEEP: &str = r#"
mark="LETIBOT_SESSION=$1"
self=" "
p=$$
while [ -n "$p" ] && [ "$p" != "0" ] && [ "$p" != "1" ]; do
  self="$self$p "
  p=$(ps -p "$p" -o ppid= 2>/dev/null | tr -d ' ')
done
proc_pids() {
  grep -lF -- "$mark" /proc/[0-9]*/environ 2>/dev/null | while read -r f; do
    tr '\0' '\n' < "$f" 2>/dev/null | grep -qxF -- "$mark" && echo "$f"
  done | sed 's|^/proc/||; s|/environ$||'
}
ps_pids() {
  ps eww -A -o pid=,command= 2>/dev/null | while read -r pid rest; do
    case " $rest " in *" $mark "*) echo "$pid" ;; esac
  done
}
mine() {
  { proc_pids; ps_pids; } | sort -un | while read -r pid; do
    case "$self" in *" $pid "*) continue ;; esac
    case "$(ps -p "$pid" -o stat= 2>/dev/null)" in Z*|"") continue ;; esac
    echo "$pid"
  done
}
pids=$(mine)
told=$(echo "$pids" | wc -w | tr -d ' ')
if [ "$told" = "0" ]; then
  echo "TOLD 0"
  exit 0
fi
kill -TERM $pids 2>/dev/null
i=0
while [ $i -lt 40 ]; do
  left=""
  for p in $pids; do
    kill -0 "$p" 2>/dev/null && left="$left $p"
  done
  [ -z "$left" ] && break
  sleep 0.1
  i=$((i+1))
done
[ -n "$left" ] && kill -KILL $left 2>/dev/null
sleep 0.1
echo "TOLD $told"
echo "LEFT $(mine)"
"#;

/// Which `mise` a host turned out to have, per process, `None` meaning *asked, and it has none*. A
/// cache because the answer costs a round trip and does not change under a running daemon; a negative
/// entry because a host without mise must pay for that fact once.
static MISE: std::sync::LazyLock<Mutex<BTreeMap<String, Option<String>>>> =
    std::sync::LazyLock::new(|| Mutex::new(BTreeMap::new()));

/// What a host said when it was asked whether it can confine a command, per process. The measurement
/// is cached, not a boundary: the view is the session's, so a second session re-applies the same
/// measurement to its own view. A probe that did not answer is not cached.
static CONFINE: std::sync::LazyLock<Mutex<BTreeMap<String, crate::exec::confine::HostProbe>>> =
    std::sync::LazyLock::new(|| Mutex::new(BTreeMap::new()));

/// Where a host's `firecode` is, and whether it may open `/dev/kvm`, per process. Asked once per host
/// for the reason [`MISE`] is, and a probe that did not answer is not cached: a host that was briefly
/// unreachable has told us nothing about what it has installed.
static FIRECODE: std::sync::LazyLock<Mutex<BTreeMap<String, Result<VmHost, String>>>> =
    std::sync::LazyLock::new(|| Mutex::new(BTreeMap::new()));

/// How long one `firecode up` or `down` on the host gets. A cold boot with a layer is minutes, so this
/// is a backstop for a client that lost its reply, not a budget.
const VM_BOUND: Duration = Duration::from_secs(600);

/// How long the copy-out of the guest's tree is waited for after `down`: the copy is asynchronous, so
/// this is a quiet window and not a total. A session that changed nothing pays it once, and one whose
/// copy is still coming is told where to collect it.
const VM_COPY_OUT_QUIET: Duration = Duration::from_secs(30);

/// The largest file that goes into the guest inside one command's argv. A single argument over
/// 128 KiB is refused by the far kernel (`MAX_ARG_STRLEN`), and base64 costs a third again.
const VM_INLINE_WRITE: usize = 48 * 1024;

/// Where a confined remote command's `$HOME` is: under the host's own `root`, never the operator's. It
/// has to be writable (a compiler writes there by habit) and it must not be the operator's, because
/// that directory is what the boundary exists to leave outside.
pub const HOME_DIR: &str = ".letibot-home";

/// What a host answered about its own firecode: the program, absolute on that machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmHost {
    pub firecode: String,
}

/// The guest a session's commands run in: `firecode in` on the far side is the boundary. The project is
/// the session's own checkout on the host, and the guest holds a copy of it at the same path, so a path
/// means the same thing on both sides.
#[derive(Debug, Clone)]
pub struct RemoteVm {
    /// The program the host answered with, absolute on that machine.
    pub firecode: String,
    /// The guest path the VM was booted on.
    pub project: String,
}

impl RemoteVm {
    /// The command string `firecode in` runs in the guest, with the session's script as the one argument
    /// firecode hands to the guest's `bash -lc`.
    fn guest_command(&self, script: &str) -> String {
        format!(
            "{} in --project {} --cwd {} {}",
            quote(&self.firecode),
            quote(&self.project),
            quote(&self.project),
            quote(script)
        )
    }

    /// The same client, a control verb on the HOST - `up`, `down`, `cp`. These are not commands in the
    /// guest, so they are never wrapped in `in`.
    fn host_command(&self, verb: &str, rest: &[&str]) -> String {
        let mut parts = vec![quote(&self.firecode), verb.to_string()];
        parts.extend(rest.iter().map(|a| quote(a)));
        parts.join(" ")
    }
}

/// One host's ssh connection, opened by [`Ssh::connect`] after the two refusals that make it
/// letibot's own: its identity file, and its host key. Everything about the command line is
/// [`flags`]'s, and nothing here can be restated by a caller.
#[derive(Clone)]
pub struct Ssh {
    host: Host,
    program: PathBuf,
    /// The identity file ssh is given, resolved and checked once, at [`Ssh::connect`].
    key: PathBuf,
    /// This daemon's master for its own RPC calls - [`control_path`], and the socket
    /// [`rpc::MAX_SESSIONS_PER_HOST`] is a promise about.
    control: PathBuf,
    /// This daemon's master for git's ssh, which this process does not run and therefore cannot count
    /// against that bound ([`git_control_path`]).
    git_control: PathBuf,
    paths: Paths,
    /// The wall clock on every call that is not a command - [`STEP_BOUND`] unless a caller (a test,
    /// mostly) names another.
    bound: Duration,
    /// Where the remote command's work is, as an absolute path on that machine: the checkout once a
    /// placement has made one, and the host's `root` before that. It is `mise exec -C <dir>`'s argument,
    /// because a toolchain is pinned by a `mise.toml` in the tree the work is in.
    dir: Option<String>,
    /// The guest this session's commands run in, once one is booted. `None` on any host placement, which
    /// is what makes the rest of this file's behaviour unchanged there.
    vm: Option<RemoteVm>,
    /// `LETIBOT_SESSION=<the placement>`, in the environment of every command once the session is
    /// placed. A host has no cgroup per session, so this mark is the only handle on the tree the
    /// session forked; [`Ssh::end_session_processes`] is its one reader.
    mark: Option<String>,
}

impl Ssh {
    /// Open the connection a host is reached through, or say why it cannot be. Two refusals happen here
    /// and both name the operator's rule and the fix: a key that is missing or inside their own ssh
    /// directory ([`identity_for`]), and a host key nobody has accepted ([`unknown_host_key`]).
    pub fn connect(host: Host, program: Option<PathBuf>, paths: Paths) -> Result<Ssh, String> {
        let key = identity_for(&host.name, host.key.as_deref(), &paths)?;
        if !host_key_known(&paths.known_hosts(), &host.hostname) {
            return Err(unknown_host_key(&host.hostname, &paths));
        }
        let control = control_path(&host.name, std::process::id());
        let git_control = git_control_path(&host.name, std::process::id());
        // The socket's directory has to exist or ssh cannot make the master, and a missing master is
        // not an error, it is a handshake per command, which nobody asked for. A failure here is not
        // fatal: ssh says so on its own stderr.
        for socket in [&control, &git_control] {
            if let Some(dir) = socket.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
        }
        Ok(Ssh {
            host,
            program: program.unwrap_or_else(ssh_program_default),
            key,
            control,
            git_control,
            paths,
            bound: STEP_BOUND,
            dir: None,
            vm: None,
            mark: None,
        })
    }

    /// Tell the connection which tree the work is in, once a placement knows: the remote checkout,
    /// absolute. It is `mise exec -C`'s argument, so it decides which `mise.toml` a remote command's
    /// toolchain is read from.
    pub fn with_dir(mut self, dir: impl Into<String>) -> Ssh {
        self.dir = Some(dir.into());
        self
    }

    /// Mark every command this connection runs, so the processes it starts on that machine can be
    /// found again and ended when the session stops - the one thing a host with no cgroup cannot
    /// answer any other way.
    pub fn with_mark(mut self, id: &str) -> Ssh {
        self.mark = Some(format!("{SESSION_MARK}={id}"));
        self
    }

    /// The directory a wrapped command is run in: `-C`'s argument. `one_entry`, not `quote`, because this
    /// is a path on the remote, so a `~` in it must be expanded there; `quote` would hand mise a literal
    /// `~`, which mise resolves against the directory it happens to be in.
    fn dir_arg(&self) -> String {
        match &self.dir {
            Some(d) => one_entry(d),
            None => "\"$HOME\"".to_string(),
        }
    }

    /// The wall clock this connection's own work is given. A test that wants a fake ssh's silence to be
    /// measured in a second rather than in [`STEP_BOUND`] says so here; production never calls it, which
    /// is why the default is the constant and not a field a caller has to fill in.
    pub fn with_bound(mut self, within: Duration) -> Ssh {
        self.bound = within;
        self
    }

    /// The bound this connection's non-command calls carry.
    pub fn bound(&self) -> Duration {
        self.bound
    }

    /// Run every command of this session inside `vm`, the firecode guest booted on this host. Set once,
    /// after the boot: everything before it (`up` itself, the probe) is the host's, and everything after
    /// is the guest's.
    pub fn with_vm(mut self, vm: RemoteVm) -> Ssh {
        self.vm = Some(vm);
        self
    }

    /// The same connection, reaching the HOST rather than the guest - for `down` and for the copy-out that
    /// follows it, which are firecode's own verbs and run outside the VM.
    pub fn without_vm(&self) -> Ssh {
        let mut plain = self.clone();
        plain.vm = None;
        plain
    }

    /// The guest this session's commands run in, when it is one.
    pub fn vm(&self) -> Option<&RemoteVm> {
        self.vm.as_ref()
    }

    /// The bound on one status poll - [`POLL_BOUND`], and never longer than the connection's own bound:
    /// a caller that asked for a second of patience did not ask for five times that on every tick of a
    /// wait loop.
    fn poll_bound(&self) -> Duration {
        POLL_BOUND.min(self.bound)
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

    /// The identity file this connection was opened with: resolved, existing, and outside the
    /// operator's ssh directory.
    pub fn key(&self) -> &Path {
        &self.key
    }

    /// The flags, and nothing else: what `git` needs for `core.sshCommand`, where the destination and
    /// the command are git's to append.
    pub fn flags(&self) -> Vec<String> {
        flags(&self.key, &self.control, &self.paths)
    }

    /// The socket this connection's own calls multiplex over. A test asks this, production does not:
    /// it is the one fact that says *one master per daemon*.
    pub fn control_socket(&self) -> &Path {
        &self.control
    }

    /// The argv that runs `command` on the remote: flags, destination, command. The whole of the ssh
    /// policy in one place, and the one thing a test's fake has to tolerate.
    pub fn argv(&self, command: &str) -> Vec<String> {
        let mut v = self.flags();
        v.push(self.destination());
        v.push(command.to_string());
        v
    }

    /// Every remote command this connection runs, spelled once: `bash -lc` (a login shell reads the
    /// remote's profiles), the host's `path` list in front of `PATH` inside that shell, and
    /// `mise exec -C <dir> --` around the lot when the host has mise. Every door goes through here.
    pub fn remote_command(&self, script: &str) -> String {
        // A VM is the boundary a command runs inside, so the script is not the host's command any more:
        // `firecode in` hands it to the guest's `bash -lc`. The login shell around it puts `~/bin` on the
        // host's `PATH`, which is where the operator's hosts keep `firecode`.
        if let Some(vm) = &self.vm {
            return format!(
                "bash -lc {}",
                quote(&self.host.with_path(&vm.guest_command(script)))
            );
        }
        // The session's mark goes on the OUTERMOST command, never inside the script: an exported
        // variable set by the script is not in the shell's own `environ`, and this one has to be -
        // it is what `end_session_processes` reads to find the tree this command forked. Outside the
        // wrapper, so a `mise exec -C <dir> --` line keeps the shape it always had.
        let body = format!("bash -lc {}", quote(&self.host.with_path(script)));
        let body = match self.mise_program() {
            Some(program) => format!("{program} exec -C {} -- {body}", self.dir_arg()),
            None => body,
        };
        match &self.mark {
            Some(mark) => format!("env {} {body}", quote(mark)),
            None => body,
        }
    }

    /// The `mise` this host runs with, or `None`: [`Mise::Off`], a host with no mise, or a probe that
    /// could not answer. `hosts.toml` wins; otherwise the answer is probed once per host per process and
    /// cached, including no mise, because a host without it must pay one round trip and not one per command.
    fn mise_program(&self) -> Option<String> {
        match &self.host.mise {
            Some(Mise::Off) => None,
            // Expanded THERE, once: `~` is that machine's home.
            Some(Mise::At(program)) => Some(one_entry(program)),
            None => {
                {
                    let cache = MISE.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(hit) = cache.get(&self.host.name) {
                        return hit.clone().map(|p| quote(&p));
                    }
                }
                let found = self.probe_mise()?;
                MISE.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(self.host.name.clone(), found.clone());
                found.map(|p| quote(&p))
            }
        }
    }

    /// Ask the host where its mise is, once, or answer `None` when it has none - or when the question did
    /// not get an answer, which the caller deliberately does not remember.
    fn probe_mise(&self) -> Option<Option<String>> {
        let script = "if [ -x \"$HOME/.local/bin/mise\" ]; then echo \"$HOME/.local/bin/mise\"; \
                      elif command -v mise >/dev/null 2>&1; then command -v mise; fi";
        let command = format!("bash -lc {}", quote(&self.host.with_path(script)));
        let within = self.bound.min(STEP_BOUND);
        match self.raw("finding mise", &command, within) {
            Some(Ok(Call::Answered(said, _, 0))) => Some(
                Some(said.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.lines().next().unwrap_or("").trim().to_string())
                    .filter(|s| !s.is_empty()),
            ),
            _ => None,
        }
    }

    /// Ask the host whether it could boot a firecode guest, once per process. Three answers, kept apart
    /// on purpose: the program and `/dev/kvm` are there; the host answered and one is missing; or the
    /// question got no answer, which is not a fact about the machine and so is not remembered.
    fn probe_firecode(&self) -> Option<Result<VmHost, String>> {
        if let Some(hit) = FIRECODE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&self.host.name)
        {
            return Some(hit.clone());
        }
        let script = "p=$(command -v firecode 2>/dev/null) || p=\n\
                      printf 'firecode=%s\\n' \"$p\"\n\
                      k=${LETIBOT_KVM_DEVICE:-/dev/kvm}\n\
                      if [ -r \"$k\" ] && [ -w \"$k\" ]; then echo kvm=yes; \
                      else echo kvm=no; fi";
        let command = format!("bash -lc {}", quote(&self.host.with_path(script)));
        let within = self.bound.min(STEP_BOUND);
        let said = match self.raw("asking the host for its firecode", &command, within) {
            Some(Ok(Call::Answered(said, _, 0))) => said,
            _ => return None,
        };
        let mut program = String::new();
        let mut kvm = false;
        let mut answered = false;
        for line in said.lines() {
            match line.trim().split_once('=') {
                Some(("firecode", v)) => {
                    program = v.trim().to_string();
                    answered = true;
                }
                Some(("kvm", v)) => kvm = v.trim() == "yes",
                _ => {}
            }
        }
        if !answered {
            return None;
        }
        let found = if program.is_empty() {
            Err(self.no_firecode(
                "there is no `firecode` on that machine's login-shell PATH, and a guest is \
                 booted by the host's own client (`~/bin` and `~/.local/bin` are where the \
                 operator's hosts keep it)",
            ))
        } else if !kvm {
            Err(self.no_firecode(
                "`/dev/kvm` is not readable and writable by this connection's user on that \
                 machine, so no guest can boot there (`firecode ls` says so directly; \
                 $LETIBOT_KVM_DEVICE names another node, and is a test's door)",
            ))
        } else {
            Ok(VmHost { firecode: program })
        };
        FIRECODE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(self.host.name.clone(), found.clone());
        Some(found)
    }

    /// The refusal `--vm` gets on a host that cannot boot one, naming the host and the fix.
    fn no_firecode(&self, why: &str) -> String {
        format!(
            "{} cannot run this session's tools in a firecode VM{}",
            self.host.name,
            if why.is_empty() {
                String::new()
            } else {
                format!(": {why}")
            }
        )
    }

    /// [`Ssh::probe_firecode`] or the refusal, for the one caller that must have an answer.
    fn firecode_on_host(&self) -> Result<VmHost, String> {
        match self.probe_firecode() {
            Some(found) => found,
            None => Err(self.no_firecode(
                "it did not answer the question, so nothing is known about what it has",
            )),
        }
    }

    /// Ask the host whether it can confine a command, once per process. The script is
    /// [`crate::exec::confine::probe_script`]'s, run the way every other remote command is run, so the
    /// argv it measures is the argv a command will be wrapped in. Only an answer is cached.
    fn confinement_probe(
        &self,
        view: &crate::exec::ViewSpec,
        egress: &crate::exec::Egress,
    ) -> Result<crate::exec::confine::HostProbe, String> {
        if let Some(hit) = CONFINE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&self.host.name)
        {
            return Ok(hit.clone());
        }
        let script = crate::exec::confine::probe_script(view, egress);
        let within = self.bound.min(STEP_BOUND);
        let (said, stderr, _exit) = self
            .sh_step_within(
                &format!("asking {} whether it can confine a command", self.host.name),
                &script,
                within,
            )
            .map_err(|e| format!("{e}"))?;
        // stderr is read too, and it is where a helper that refused says so: ssh carries one channel for
        // the command's own output, but this call is not wrapped in one shell, so the two arrive
        // separately here. A `bwrap: ` line is the sentence a reader needs.
        let mut all = said;
        if !stderr.is_empty() {
            if !all.is_empty() && !all.ends_with('\n') {
                all.push('\n');
            }
            all.push_str(&String::from_utf8_lossy(&stderr));
        }
        let probe = crate::exec::confine::read_probe(&all);
        CONFINE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(self.host.name.clone(), probe.clone());
        Ok(probe)
    }

    /// The prefix git's `upload-pack` is invoked with, for the fetch that brings a branch home. sshd
    /// runs the far side as `<upload-pack> '<path>'`, not a login shell, so no profile and no mise, and
    /// the toolchain wrapper has to be spliced here too.
    pub fn upload_pack_prefix(&self) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        if let Some(prefix) = self.host.path_prefix() {
            parts.push(prefix);
        }
        if let Some(program) = self.mise_program() {
            parts.push(format!("{program} exec -C {} --", self.dir_arg()));
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join(" "))
        }
    }

    /// One command of the model's: [`SshBackend::run`]'s door, and the only caller. It keeps the
    /// deadline it is given and nothing else, and it is at most once, so a call that died in flight
    /// is reported by [`Ssh::unknown`] and never re-sent.
    fn sh_run(
        &self,
        step: &str,
        script: &str,
        pgid: Option<&str>,
    ) -> Result<(String, Vec<u8>, i32), BackendError> {
        let wrapped = match pgid {
            Some(file) => grouped(script, file),
            None => script.to_string(),
        };
        let ended = self.call(
            step,
            &wrapped,
            Retry::Never,
            self.bound,
            Reported::Always,
            None,
        )?;
        match ended {
            Call::Answered(out, stderr, exit) => Ok((out, stderr, exit)),
            other => {
                if let Some(file) = pgid {
                    self.cancel_group(file);
                }
                Err(BackendError::Io(self.unknown(step, &other)))
            }
        }
    }

    /// Rule 6: kill the remote process group of a command that stopped answering, in its own bounded
    /// call, because the call that timed out had its local ssh killed and sshd's hang-up usually but
    /// not always takes the group with it. The group is signalled only when the pid leads one.
    fn cancel_group(&self, file: &str) {
        let script = format!(
            "f={f}\n\
             if [ -f \"$f\" ]; then\n\
               g=$(cat \"$f\" 2>/dev/null)\n\
               pg=$(ps -p \"$g\" -o pgid= 2>/dev/null | tr -d ' ')\n\
               if [ -n \"$g\" ] && [ \"$pg\" = \"$g\" ]; then t=\"-$g\"; else t=\"$g\"; fi\n\
               if [ -n \"$g\" ]; then\n\
                 kill -TERM \"$t\" 2>/dev/null\n\
                 i=0; while [ $i -lt 40 ]; do kill -0 \"$g\" 2>/dev/null || break; \
                   sleep 0.05; i=$((i+1)); done\n\
                 kill -KILL \"$t\" 2>/dev/null\n\
               fi\n\
               rm -f \"$f\"\n\
             fi",
            f = quote(file)
        );
        let command = match &self.vm {
            // The group is the GUEST's, so the kill has to be a command in the guest too.
            Some(vm) => format!(
                "bash -lc {}",
                quote(&self.host.with_path(&vm.guest_command(&script)))
            ),
            None => format!("bash -lc {}", quote(&self.host.with_path(&script))),
        };
        let _ = self.raw("ending the remote process group", &command, CANCEL_BOUND);
    }

    /// End every process this session started on that machine, and say what that was.
    ///
    /// A host has no cgroup to empty, so the mark ([`SESSION_MARK`]) is the only handle on the tree
    /// the session forked - a stopped package used to leave its shells running for hours in a
    /// placement directory that had already been removed. A process that cleared its own environment
    /// is out of reach, so the answer says what is LEFT rather than claiming a clean machine.
    pub fn end_session_processes(&self) -> Option<String> {
        self.sweep().said
    }

    /// The sweep itself, and whether it was asked at all: [`placements_down`] needs the second
    /// half, because a sweep that never got a slot has not run and must not go with the list.
    fn sweep(&self) -> Swept {
        // A guest's processes end with the guest, and that is `down`: nothing of the session's runs
        // in the host's own process table, so there is nothing here to sweep for.
        if self.vm.is_some() {
            return Swept::asked(None);
        }
        let Some(id) = self
            .mark
            .as_ref()
            .and_then(|m| m.split_once('='))
            .map(|(_, id)| id)
        else {
            return Swept::asked(None);
        };
        // `bash -lc <script> name <arg>`: the id is an ARGUMENT, never part of this command's text,
        // because a sweep whose own argv carried the mark would find itself.
        let command = format!(
            "bash -lc {} letibot-end {}",
            quote(&self.host.with_path(SESSION_SWEEP)),
            quote(id)
        );
        let step = "ending the processes this session started";
        // Never longer than a step of this connection's own work: a host that has stopped answering
        // must cost the close what every other step costs it, and not a bound of its own choosing.
        let within = self.bound.min(END_SESSION_BOUND);
        let out = match self.raw(step, &command, within) {
            Some(Ok(Call::Answered(out, _, 0))) => out,
            Some(Ok(other)) => {
                return Swept::asked(Some(format!(
                    "the processes this session started on {} could NOT be shown to have ended: {}",
                    self.host.name,
                    self.no_answer(step, &other, within, Asked::None)
                )));
            }
            Some(Err(e)) => {
                return Swept::asked(Some(format!(
                    "the processes this session started on {} could NOT be ended: {e} - anything \
                     the session left running is still running",
                    self.host.name
                )));
            }
            // Not `asked`: no slot means no call, so nothing ran and the list must keep this one.
            None => {
                return Swept {
                    said: Some(format!(
                        "the processes this session started on {} could NOT be ended: no slot in \
                         the connection queue within {} s, so the sweep never ran and anything the \
                         session left there is still running",
                        self.host.name,
                        within.as_secs()
                    )),
                    asked: false,
                };
            }
        };
        let said = |tag: &str| {
            out.lines()
                .find_map(|l| l.strip_prefix(tag))
                .unwrap_or("")
                .trim()
                .to_string()
        };
        let told = said("TOLD ");
        if told.is_empty() || told == "0" {
            return Swept::asked(None);
        }
        let left: Vec<&str> = out
            .lines()
            .find_map(|l| l.strip_prefix("LEFT "))
            .unwrap_or("")
            .split_whitespace()
            .collect();
        Swept::asked(Some(if left.is_empty() {
            format!(
                "{told} process(es) this session started on {} were ended, so that machine is not \
                 still running the work of a package that is over",
                self.host.name
            )
        } else {
            format!(
                "{told} process(es) this session started on {} were ended and {} survived even \
                 SIGKILL ({}): they are still running there. Look for them by their own command \
                 line, `ps -A -o pid=,command=`",
                self.host.name,
                left.len(),
                left.join(" ")
            )
        }))
    }

    /// One step of this backend's own work, bounded by [`Ssh::bound`] and asked again once when the
    /// operation is idempotent and the first attempt did not answer. `step` names the operation for
    /// the refusal, because a sentence that says only "ssh failed" sends the reader to the wrong place.
    fn sh_step(&self, step: &str, script: &str) -> Result<(String, Vec<u8>, i32), BackendError> {
        self.sh_step_within(step, script, self.bound)
    }

    /// [`Ssh::sh_step`] with a bound of its own: a status poll is shorter than a write.
    fn sh_step_within(
        &self,
        step: &str,
        script: &str,
        within: Duration,
    ) -> Result<(String, Vec<u8>, i32), BackendError> {
        match self.call(step, script, Retry::Once, within, Reported::Always, None)? {
            Call::Answered(out, stderr, exit) => Ok((out, stderr, exit)),
            other => Err(BackendError::Io(self.no_answer(
                step,
                &other,
                within,
                Asked::Again,
            ))),
        }
    }

    /// One step, exactly once, and silence is a refusal - for the steps that must not be asked twice
    /// even though they are not commands: a write's chunk (a retry would append it again), and a job's
    /// start (a retry would start a second one).
    pub fn sh_once(
        &self,
        step: &str,
        script: &str,
    ) -> Result<(String, Vec<u8>, i32), BackendError> {
        self.sh_once_within(step, script, self.bound)
    }

    /// [`Ssh::sh_once`] with a bound of its own: a status poll is shorter than a write.
    fn sh_once_within(
        &self,
        step: &str,
        script: &str,
        within: Duration,
    ) -> Result<(String, Vec<u8>, i32), BackendError> {
        match self.call(step, script, Retry::Never, within, Reported::Always, None)? {
            Call::Answered(out, stderr, exit) => Ok((out, stderr, exit)),
            other => Err(BackendError::Io(self.no_answer(
                step,
                &other,
                within,
                Asked::MuxOnly,
            ))),
        }
    }

    /// A status poll: the same call, but only a poll that fails is recorded.
    /// [`RemoteProcesses::refresh`] runs this every 120 ms for as long as a job is waited on, and the
    /// record ring is 256 entries, so successful heartbeats would evict every call a reader could act on.
    fn sh_poll_within(
        &self,
        step: &str,
        script: &str,
        within: Duration,
    ) -> Result<(String, Vec<u8>, i32), BackendError> {
        match self.call(step, script, Retry::Never, within, Reported::Failures, None)? {
            Call::Answered(out, stderr, exit) => Ok((out, stderr, exit)),
            other => Err(BackendError::Io(self.no_answer(
                step,
                &other,
                within,
                Asked::MuxOnly,
            ))),
        }
    }

    /// The same call, but a non-zero exit is an error carrying what the host said. Used by everything
    /// that is arranging rather than running the model's work.
    fn must(&self, what: &str, script: &str) -> Result<String, BackendError> {
        self.must_within(what, script, Retry::Once, self.bound)
    }

    /// [`Ssh::must`] for a step that must not be retried, and with its own bound.
    fn must_within(
        &self,
        what: &str,
        script: &str,
        retry: Retry,
        within: Duration,
    ) -> Result<String, BackendError> {
        let (out, stderr, exit) =
            match self.call(what, script, retry, within, Reported::Always, None)? {
                Call::Answered(out, stderr, exit) => (out, stderr, exit),
                other => {
                    return Err(BackendError::Io(self.no_answer(
                        what,
                        &other,
                        within,
                        Asked::of(retry),
                    )));
                }
            };
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

    /// One step, with the retry its caller allows: the rule lives here and nowhere else. The breaker
    /// runs first, a stale master socket is cleared, each attempt records itself, and the verdict
    /// counts "connected" as the far side saying something; a step's retry is not itself refused.
    fn call(
        &self,
        step: &str,
        script: &str,
        retry: Retry,
        within: Duration,
        reported: Reported,
        fed: Option<&[u8]>,
    ) -> Result<Call, BackendError> {
        let pass = rpc::before(&self.host.name).map_err(BackendError::Io)?;
        self.fresh_master();
        let first = self.attempt(step, script, within, reported, fed);
        let first = match first {
            Ok(call) => call,
            // The local process could not be started at all: nobody knows what happened, and the
            // breaker is told that nothing was reached.
            Err(e) => {
                rpc::after(&self.host.name, pass, false);
                return Err(e);
            }
        };
        rpc::after(&self.host.name, pass, connected(&first));
        // The things worth asking twice, both for one reason: nothing on the far side can have
        // changed. No answer at all (retried only when the operation is idempotent), or no session
        // ever opened (retried for any operation, because nothing could have taken effect).
        let again = match &first {
            Call::Answered(..) | Call::Denied(..) => false,
            Call::MuxRefused(..) => true,
            _ => retry == Retry::Once,
        };
        if again {
            // The master may be what died, and a second attempt through the same dead socket would
            // fail the same way (`attempt` has already dropped a socket it was refused on). This one
            // is not a probe: the cooldown's one call was already spent.
            let second = self.attempt(step, script, within, reported, fed);
            return match second {
                Ok(call) => {
                    rpc::after(
                        &self.host.name,
                        rpc::Pass { probe: false },
                        connected(&call),
                    );
                    Ok(call)
                }
                Err(e) => {
                    rpc::after(&self.host.name, rpc::Pass { probe: false }, false);
                    Err(e)
                }
            };
        }
        Ok(first)
    }

    /// One attempt, with the wall clock on it and a record of how it went. The local ssh is killed at
    /// the deadline; what happens on the far side is [`Ssh::sh_run`]'s business. The call takes its
    /// queue slot after the command is built, because building it may itself ask the host a question.
    fn attempt(
        &self,
        step: &str,
        script: &str,
        within: Duration,
        reported: Reported,
        fed: Option<&[u8]>,
    ) -> Result<Call, BackendError> {
        let command = self.remote_command(script);
        let _slot = rpc::enter(&self.host.name, within).map_err(BackendError::Io)?;
        let began = Instant::now();
        // The one place a call is written down, and [`Reported`] is the whole of the policy: a
        // heartbeat is only news when it fails.
        let note = |outcome: rpc::Outcome| {
            if reported == Reported::Always || outcome != rpc::Outcome::Ok {
                rpc::record(&self.host.name, step, began.elapsed(), outcome);
            }
        };
        let argv = self.argv(&command);
        let ended = match fed {
            // The bytes go on stdin and nowhere near the argv: a command line is capped (`ARG_MAX`),
            // which is why this cannot be a series of calls with a slice spliced into each one.
            Some(bytes) => rpc::run_bounded_fed(&self.program, &argv, within, bytes),
            None => rpc::run_bounded(&self.program, &argv, within),
        };
        match ended {
            rpc::Ended::Unstartable(why) => {
                note(rpc::Outcome::Unknown);
                Err(BackendError::Io(format!(
                    "{why} — the ssh for {} could not be started at all, so nothing was sent \
                     and nothing on that machine changed",
                    self.host.name
                )))
            }
            rpc::Ended::TimedOut => {
                // The master may be the thing that stopped answering, and a socket that is still there
                // would make the next call wait on it too.
                note(rpc::Outcome::TimedOut);
                self.drop_socket();
                Ok(Call::TimedOut(within))
            }
            rpc::Ended::Answered(out) => {
                let exit = match out.status.code() {
                    Some(c) => c,
                    None => {
                        use std::os::unix::process::ExitStatusExt;
                        128 + out.status.signal().unwrap_or(0)
                    }
                };
                let said = String::from_utf8_lossy(&out.stdout).into_owned();
                let stderr = out.stderr;
                // ssh's own failure: 255 with nothing from the command. A command that exits 255
                // and says something did run, and its words are the answer.
                if exit == 255 && said.is_empty() {
                    self.drop_socket();
                    let words = String::from_utf8_lossy(&stderr).into_owned();
                    if let Some(kind) = auth_refusal(&words) {
                        note(rpc::Outcome::Refused);
                        return Ok(Call::Denied(kind, stderr));
                    }
                    // A session the multiplexer refused is not the command's exit. Classified before
                    // the catch-all, because the retry rule differs: `Dropped` is *may have run*, and
                    // this is *never ran* ([`mux_refusal`]).
                    if let Some(kind) = mux_refusal(&words) {
                        note(rpc::Outcome::Dropped);
                        return Ok(Call::MuxRefused(kind, stderr));
                    }
                    note(rpc::Outcome::Dropped);
                    return Ok(Call::Dropped(stderr));
                }
                note(rpc::Outcome::Ok);
                Ok(Call::Answered(said, stderr, exit))
            }
        }
    }

    /// A call nobody asked the breaker about and nobody will be told of: the mise probe, and the kill
    /// that ends a cancelled command's remote group. It takes a slot and records itself like any other
    /// call, but it is outside the retry rule and the breaker's counting: a probe must not open one.
    fn raw(
        &self,
        step: &str,
        command: &str,
        within: Duration,
    ) -> Option<Result<Call, BackendError>> {
        let _slot = rpc::enter(&self.host.name, within).ok()?;
        let began = Instant::now();
        match rpc::run_bounded(&self.program, &self.argv(command), within) {
            rpc::Ended::Unstartable(_) => {
                rpc::record(
                    &self.host.name,
                    step,
                    began.elapsed(),
                    rpc::Outcome::Unknown,
                );
                Some(Err(BackendError::Io(format!(
                    "`{}` on {} could not be started",
                    step, self.host.name
                ))))
            }
            rpc::Ended::TimedOut => {
                rpc::record(
                    &self.host.name,
                    step,
                    began.elapsed(),
                    rpc::Outcome::TimedOut,
                );
                self.drop_socket();
                Some(Ok(Call::TimedOut(within)))
            }
            rpc::Ended::Answered(out) => {
                let exit = out.status.code().unwrap_or(-1);
                let said = String::from_utf8_lossy(&out.stdout).into_owned();
                if exit == 255 && said.is_empty() {
                    self.drop_socket();
                    let stderr = out.stderr;
                    let words = String::from_utf8_lossy(&stderr).into_owned();
                    if let Some(kind) = auth_refusal(&words) {
                        rpc::record(
                            &self.host.name,
                            step,
                            began.elapsed(),
                            rpc::Outcome::Refused,
                        );
                        return Some(Ok(Call::Denied(kind, stderr)));
                    }
                    // A probe that was refused a session has not answered, and the cache rules already
                    // say such an answer is not remembered - which is why this arm exists here too: a
                    // `mise` probe reported as a command that exited 255 would be remembered as no mise.
                    let mux = mux_refusal(&words);
                    rpc::record(
                        &self.host.name,
                        step,
                        began.elapsed(),
                        rpc::Outcome::Dropped,
                    );
                    return Some(Ok(match mux {
                        Some(kind) => Call::MuxRefused(kind, stderr),
                        None => Call::Dropped(stderr),
                    }));
                }
                rpc::record(&self.host.name, step, began.elapsed(), rpc::Outcome::Ok);
                Some(Ok(Call::Answered(said, out.stderr, exit)))
            }
        }
    }

    /// The sentence a step that never answered is refused with: the host, the step, the bound, and
    /// what was done about it. `asked` is what the caller really did, because a sentence that
    /// claims a second attempt nobody made is worse than one that claims nothing.
    fn no_answer(&self, step: &str, ended: &Call, within: Duration, asked: Asked) -> String {
        if let Call::Denied(kind, stderr) = ended {
            return self.denied(step, *kind, stderr);
        }
        // A refused session gets its own sentence, and it is the one a reader needs: ssh's words were
        // not the command's, and nothing ran - which is why every caller but `raw` asks it again.
        if let Call::MuxRefused(kind, stderr) = ended {
            let then = match asked {
                Asked::None => "the socket was dropped and nothing was asked again, because this \
                                call is made once"
                    .to_string(),
                _ => "the socket was dropped and the call was asked again on a fresh connection, \
                      which failed the same way"
                    .to_string(),
            };
            return format!(
                "`{step}` on {} never started: ssh's multiplexer lost the session ({}) — ssh said \
                 `{}`. Nothing ran there, so nothing on that machine changed; {then}. This is a \
                 refused session, not your command's exit status.",
                self.host.name,
                kind.as_str(),
                first_line(&String::from_utf8_lossy(stderr))
            );
        }
        let how = match ended {
            Call::TimedOut(_) => {
                format!("got no answer within {} s", within.as_secs_f32().max(0.0))
            }
            Call::Dropped(stderr) => format!(
                "lost the connection before it answered ({})",
                first_line(&String::from_utf8_lossy(stderr))
            ),
            _ => "did not answer".to_string(),
        };
        let again = match asked {
            Asked::Again => "the call was asked once more with a fresh connection",
            _ => "nothing was asked again, because a second attempt is a second effect",
        };
        format!(
            "`{step}` on {} {how}: the local ssh was killed rather than waited on, and {again}. \
             Nothing is left waiting — but the operation may not have happened, so check before \
             relying on it.",
            self.host.name
        )
    }

    /// A step whose outcome nobody knows. `run` and a job's start are the two operations that are not
    /// idempotent, so a call that died in flight is reported as exactly that, never as a failure,
    /// which would read as *it did not happen* and could be run again on that reading.
    fn unknown(&self, step: &str, ended: &Call) -> String {
        if let Call::Denied(kind, stderr) = ended {
            return self.denied(step, *kind, stderr);
        }
        // A refused session is the one *not answered* case whose outcome is not unknown: no
        // channel existed, so the command cannot have run ([`mux_refusal`]). Said as itself, so a
        // `run` that was never started is not reported as *it may be running there*.
        if let Call::MuxRefused(kind, stderr) = ended {
            return format!(
                "`{step}` on {} did NOT run: ssh's multiplexer lost the session ({}) — ssh said \
                 `{}`. No session reached the remote shell, so nothing ran and nothing there \
                 changed; the socket was dropped and the call was asked again on a fresh \
                 connection, which failed the same way. Ask again once the host is less busy.",
                self.host.name,
                kind.as_str(),
                first_line(&String::from_utf8_lossy(stderr))
            );
        }
        let how = match ended {
            Call::TimedOut(within) => {
                format!("stopped answering for {} s", within.as_secs_f32().max(0.0))
            }
            Call::Dropped(stderr) => {
                format!("dropped ({})", first_line(&String::from_utf8_lossy(stderr)))
            }
            _ => "ended without an answer".to_string(),
        };
        format!(
            "the connection to {} {how} while {step}: it may have run, partly run, or still be \
             running there — check before running it again. It was NOT retried: this is not an \
             operation that can be asked twice.",
            self.host.name
        )
    }

    /// The sentence a caller that is not this file needs for a refusal, `None` when the words are not
    /// one. `letibot --migrate` is the other caller of [`Ssh`], and the classification is here once
    /// so the two callers cannot disagree about what ssh's words mean.
    pub fn refusal(&self, step: &str, stderr: &[u8]) -> Option<String> {
        auth_refusal(&String::from_utf8_lossy(stderr)).map(|kind| self.denied(step, kind, stderr))
    }

    /// A refusal that is an answer, not a lost connection. `Permission denied (publickey)` and a host
    /// key that does not verify are the machine answering, and it will go on answering the same way
    /// until a key is installed, so it is its own outcome and is never retried.
    fn denied(&self, step: &str, kind: AuthRefusal, stderr: &[u8]) -> String {
        let said = first_line(&String::from_utf8_lossy(stderr));
        let host = &self.host.name;
        let fix = match kind {
            AuthRefusal::Key => format!(
                "Install letibot's own key there, once:\n    \
                 ssh-copy-id -i {} -o 'IdentitiesOnly=yes' -o 'IdentityAgent=none' {}",
                self.key.with_extension("pub").display(),
                self.host.destination()
            ),
            AuthRefusal::HostKey => format!(
                "letibot checks the host key against its own file and `StrictHostKeyChecking=yes` \
                 means it neither asks nor learns. If the machine's key really changed, remove the \
                 old line from {} and add the new one yourself:\n    \
                 ssh-keyscan -t ed25519 {} >> {}",
                self.paths.known_hosts().display(),
                self.host.hostname,
                self.paths.known_hosts().display()
            ),
        };
        format!(
            "`{step}` on {host}: **letibot's key is not authorised on {host} (or {host}'s host key \
             changed)** — ssh answered `{said}`. This is definite, not a lost connection: the call \
             was not retried and will keep failing until the key is fixed. {fix}",
        )
    }

    /// `ssh -O <verb>` for this host's `ControlMaster`, bounded. Every one of these goes through
    /// `-F /dev/null` like every other call: one that read the operator's client config would be the
    /// hole in every other flag.
    fn master(&self, verb: &str) -> Option<std::process::Output> {
        let args: Vec<String> = vec![
            "-F".into(),
            "/dev/null".into(),
            "-o".into(),
            format!("ControlPath={}", self.control.display()),
            "-o".into(),
            "ControlMaster=no".into(),
            "-O".into(),
            verb.into(),
            self.destination(),
        ];
        run_bounded(&self.program, &args, MASTER_BOUND)
    }

    /// A stale master must not hang the next call: a `ControlMaster` socket outlives the connection
    /// it belongs to, and ssh will try to multiplex through it and wait instead of dialling the host.
    /// So a socket nobody answers on is removed. It is ours, in letibot's runtime directory.
    fn fresh_master(&self) {
        if !self.control.exists() {
            return;
        }
        if self.master("check").is_some_and(|o| o.status.success()) {
            return;
        }
        let _ = std::fs::remove_file(&self.control);
    }

    /// Drop the socket of a master that has just failed us, without speaking to it. `-O exit` is what
    /// [`Ssh::exit_master`] does at the end of a session; here the connection already stopped
    /// answering, so asking it to shut down cleanly would wait out another `MASTER_BOUND`.
    fn drop_socket(&self) {
        let _ = std::fs::remove_file(&self.control);
    }

    /// The one ssh this process does not start: `git fetch` builds it from `core.sshCommand` and
    /// multiplexes over the git socket, so it takes a slot on that gate by hand. A refused session
    /// never reached the remote, so a full master costs a round trip and not a session's branch.
    fn fetch_back(&self, repo: &Path, url: &str, refspec: &str) -> Result<FetchEnded, String> {
        self.fetch_within(Path::new("git"), repo, url, refspec, GIT_BOUND)
    }

    /// [`Ssh::fetch_back`] with the program and the budget named, the way [`Ssh::with_bound`]
    /// names the connection's own: a test that measures the deadline wants seconds and a git it
    /// can make silent, and production names neither.
    fn fetch_within(
        &self,
        program: &Path,
        repo: &Path,
        url: &str,
        refspec: &str,
        within: Duration,
    ) -> Result<FetchEnded, String> {
        let budget = FetchBudget::new(within);
        let mut last: Option<std::process::Output> = None;
        for attempt in 0..2 {
            // The wait and the run both come out of the one budget, so the second attempt gets what
            // the first left rather than a fresh `GIT_BOUND`.
            if budget.left().is_zero() {
                break;
            }
            // The wait is the connection's own bound and never more than the fetch has left: a
            // caller that asked for a second of patience did not ask for the transfer's five
            // minutes of slot queue. The RUN keeps what is left, because that is real work.
            let _slot = match rpc::enter_git(&self.host.name, budget.wait(self.bound)) {
                Ok(slot) => slot,
                Err(why) => {
                    return Err(match &last {
                        Some(out) => refused_then_no_slot(&out.stderr, &why),
                        None => why,
                    });
                }
            };
            // The wait above is charged to the same budget, so it can be the whole of it. A git
            // forked with nothing left is killed on its first poll, and the caller would report a
            // transfer that never had a chance as one that timed out.
            if budget.left().is_zero() {
                let why = format!(
                    "this fetch was given {within:?} and the slot on the git master took all of \
                     it, so nothing was run"
                );
                return Err(match &last {
                    Some(out) => refused_then_no_slot(&out.stderr, &why),
                    None => why,
                });
            }
            let argv = self.fetch_argv(attempt, repo, url, refspec);
            // Bounded, because this fetch's transport is an ssh connection: a close that waits for
            // ever on a dead host is the same defect as a `read` that does.
            let out = run_bounded(program, &argv, budget.left());
            let refused = out.as_ref().is_some_and(|o| {
                !o.status.success() && mux_refusal(&String::from_utf8_lossy(&o.stderr)).is_some()
            });
            last = out;
            if !refused || attempt == 1 {
                break;
            }
        }
        Ok(match last {
            Some(out) => FetchEnded::Ran(out),
            None => FetchEnded::Killed(budget.spent()),
        })
    }

    /// `git fetch`'s argv for one attempt. The socket is the attempt's own: attempt 0 rides the
    /// daemon's shared git master, and a retry dials a socket beside it, because the shared master may
    /// be the one that refused and that socket belongs to every session of this daemon.
    fn fetch_argv(&self, attempt: u32, repo: &Path, url: &str, refspec: &str) -> Vec<String> {
        let mut argv: Vec<String> = vec![
            "-C".into(),
            repo.display().to_string(),
            "-c".into(),
            format!("core.sshCommand={}", self.git_ssh_command(attempt)),
            "fetch".into(),
            "--no-tags".into(),
        ];
        // The far side's `git-upload-pack` is a remote command, and a login shell reads no profile
        // for it, so a host whose `git` is not on the default `PATH` needs the prefix here too.
        if let Some(prefix) = self.upload_pack_prefix() {
            argv.push("--upload-pack".into());
            argv.push(format!("{prefix} git-upload-pack"));
        }
        argv.push(url.to_string());
        argv.push(refspec.to_string());
        argv
    }

    /// What `core.sshCommand` is for one attempt: this program, the connection's flags, and that
    /// attempt's `ControlPath`. The retry's name comes from the counter so two fetches retrying at
    /// once never share one, and through [`control_path_for`] so a long path still falls back.
    fn git_ssh_command(&self, attempt: u32) -> String {
        let socket = match attempt {
            0 => self.git_control.clone(),
            _ => {
                static RETRIES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                let n = RETRIES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                control_path_for(
                    &self.host.name,
                    std::process::id(),
                    &format!("git-retry-{n}"),
                )
            }
        };
        std::iter::once(self.program.display().to_string())
            .chain(flags(&self.key, &socket, &self.paths))
            .map(|a| quote(&a))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// A file's bytes, base64 over the wire - binary-safe, which a `cat` is not once the connection
    /// is a pipe with a locale on it.
    fn read_abs(&self, path: &str) -> Result<Vec<u8>, BackendError> {
        let q = quote(path);
        let (out, _, exit) = self.sh_step(
            &format!("reading {path}"),
            &format!(
                "if [ -d {q} ]; then exit 21; fi; if [ ! -e {q} ]; then exit 22; fi; \
                 base64 < {q} | tr -d '\\n'"
            ),
        )?;
        match exit {
            0 => crate::firecode::decode_base64(out.as_bytes())
                .map_err(|e| BackendError::Io(format!("{path}: {e}"))),
            21 => Err(BackendError::IsADirectory(path.to_string())),
            _ => Err(BackendError::NotFound(path.to_string())),
        }
    }

    /// Written atomically, in ONE call, and only if the far side agrees about the bytes: the bytes go
    /// on stdin of one ssh call, the far side counts and hashes them, and the `mv` is inside the same
    /// script after both checks. A cut stream is short, and the target is never touched.
    fn write_abs(&self, path: &str, bytes: &[u8]) -> Result<(), BackendError> {
        if let Some(vm) = &self.vm {
            return write_into_guest(self, vm, path, bytes);
        }
        let mut last = None;
        for _ in 0..2 {
            match self.write_once(path, bytes) {
                Ok(()) => return Ok(()),
                Err(e) => last = Some(e),
            }
        }
        Err(last.expect("the loop ran"))
    }

    /// One whole attempt at [`Ssh::write_abs`]: one call, one temp file, one verified `mv`.
    fn write_once(&self, path: &str, bytes: &[u8]) -> Result<(), BackendError> {
        let dir = match path.rfind('/') {
            Some(0) => "/".to_string(),
            Some(i) => path[..i].to_string(),
            None => ".".to_string(),
        };
        // A temp name that belongs to this attempt and no other: one name per path per PROCESS would
        // let two writes at once truncate each other's temp, and an `mv` from either would move
        // whatever was in it. The counter makes the collision impossible rather than unlikely.
        static ATTEMPT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let tmp = format!(
            "{path}.letibot-{}-{}",
            std::process::id(),
            ATTEMPT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let sha = {
            use sha2::Digest;
            let digest = sha2::Sha256::digest(bytes);
            digest
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        };
        // One script, one session: `mkdir`, the stream, the two checks, the `mv`. Every one of
        // them is inside the same call, so there is no window in which two calls of this write can
        // interleave and no stage whose failure can be mistaken for a success.
        let script = format!(
            "set -e\n\
             t={tmp}\n\
             mkdir -p {dir}\n\
             trap 'rm -f \"$t\"' EXIT HUP INT TERM\n\
             base64 -d > \"$t\"\n\
             n=$(wc -c < \"$t\" | tr -d ' \\t')\n\
             [ \"$n\" = {len} ] || {{ echo \"the stream arrived as $n bytes, not {len}\" >&2; exit 41; }}\n\
             if command -v sha256sum >/dev/null 2>&1; then s=$(sha256sum < \"$t\" | cut -d' ' -f1)\n\
             elif command -v shasum >/dev/null 2>&1; then s=$(shasum -a 256 < \"$t\" | cut -d' ' -f1)\n\
             elif command -v openssl >/dev/null 2>&1; then s=$(openssl dgst -sha256 -r < \"$t\" | cut -d' ' -f1)\n\
             else s=; fi\n\
             if [ -n \"$s\" ] && [ \"$s\" != {sha} ]; then\n\
               echo \"the bytes on that machine are not the bytes that were sent\" >&2; exit 42\n\
             fi\n\
             mv -f \"$t\" {target}",
            tmp = quote(&tmp),
            dir = quote(&dir),
            len = bytes.len(),
            sha = quote(&sha),
            target = quote(path)
        );
        let body = crate::firecode::encode_base64(bytes);
        self.must_fed(
            &format!("writing {path}"),
            &script,
            Retry::Never,
            self.write_bound(bytes.len()),
            body.as_bytes(),
        )
        .map(|_| ())
    }

    /// The wall clock one streamed write gets: the connection's bound, plus a second for every
    /// 512 KiB it carries. A write is the one step whose cost is the payload's, and a single 20 s
    /// bound would be a regression for a 19 MB bundle on a slow link.
    fn write_bound(&self, len: usize) -> Duration {
        self.bound + Duration::from_secs((len / (512 * 1024)) as u64)
    }

    /// A step that is fed on stdin, and the only one: [`Ssh::write_abs`], which a guest never
    /// reaches because its bytes travel in the command instead.
    fn must_fed(
        &self,
        what: &str,
        script: &str,
        retry: Retry,
        within: Duration,
        fed: &[u8],
    ) -> Result<String, BackendError> {
        let (out, stderr, exit) =
            match self.call(what, script, retry, within, Reported::Always, Some(fed))? {
                Call::Answered(out, stderr, exit) => (out, stderr, exit),
                other => {
                    return Err(BackendError::Io(self.no_answer(
                        what,
                        &other,
                        within,
                        Asked::of(retry),
                    )));
                }
            };
        if exit != 0 {
            let said = if stderr.is_empty() {
                out.trim().to_string()
            } else {
                String::from_utf8_lossy(&stderr).trim().to_string()
            };
            return Err(BackendError::Io(format!(
                "{what} on {}: the far side refused it after {} byte(s) — exit {exit}: {} — and the \
                 target was not touched, because the bytes are checked and moved into place in the \
                 same call",
                self.host.name,
                fed.len(),
                if said.is_empty() {
                    "it said nothing"
                } else {
                    &said
                }
            )));
        }
        Ok(out)
    }

    /// Drop the ControlMaster, if one was made. Best effort: an ssh that says it has no master is not
    /// a failure of anything, and it is bounded, because a master that has stopped answering is
    /// exactly the case this is called in.
    pub fn exit_master(&self) {
        let _ = self.master("exit");
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

/// The first line of something a machine said, for a sentence that has to stay one sentence.
fn first_line(text: &str) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let line = line.trim();
    if line.is_empty() {
        return "it said nothing".to_string();
    }
    let mut cut = line.len().min(200);
    while !line.is_char_boundary(cut) {
        cut -= 1;
    }
    if cut < line.len() {
        format!("{}…", &line[..cut])
    } else {
        line.to_string()
    }
}

/// What a fetch says when it was refused and the retry then found no slot: the refusal is the only
/// clue that points at the master's `MaxSessions`, and `busy`'s words alone would say nothing was
/// sent about an attempt that did send.
fn refused_then_no_slot(first_stderr: &[u8], busy: &str) -> String {
    format!(
        "git was refused by the connection on an earlier attempt (`{}`), and there was no slot on \
         the retry: {busy}",
        first_line(&String::from_utf8_lossy(first_stderr))
    )
}

/// Run a program to completion, or kill it at the deadline. `None` is *the bound passed and it was
/// killed*, and also a program that cannot be started, because every caller is asking a question it
/// can do without. The runner itself is [`rpc::run_bounded`], one implementation for the tree.
fn run_bounded(program: &Path, args: &[String], within: Duration) -> Option<std::process::Output> {
    match rpc::run_bounded(program, args, within) {
        rpc::Ended::Answered(out) => Some(out),
        rpc::Ended::TimedOut | rpc::Ended::Unstartable(_) => None,
    }
}

/// What one probe of a remote host answered: the live facts a placement decision needs. Every field
/// is what the machine itself said, and each is optional or zero-able on purpose, so a fact this box
/// could not read is reported as absent rather than as a zero that reads like a full disk.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Facts {
    /// `uname -n`: the machine's own name, which a `hostname = "10.0.0.7"` line does not tell you.
    pub uname: String,
    /// `nproc`, or the portable `getconf` fallback. 0 means neither answered.
    pub cores: u64,
    /// The three load averages as the machine printed them - a string because that is the shape the
    /// two platforms agree on, and a model reads it directly.
    pub load: String,
    pub mem_total_kb: Option<u64>,
    pub mem_avail_kb: Option<u64>,
    /// Which path the disk figure is about: the host's `root`, or its home when the root has not
    /// been made yet.
    pub disk_path: String,
    pub disk_free_kb: Option<u64>,
    /// Where `git` is, when the host's own `PATH` - with its `path` list in front - finds it. This is
    /// one of the two facts that decide whether a session can work there at all.
    pub git: Option<String>,
    /// Where `cargo` is on `PATH`. Reported only as a fallback: on a host with mise the toolchain is
    /// whatever mise pins, and `cargo` on a non-login `PATH` is not the question.
    pub cargo: Option<String>,
    /// `mise --version`, first line, when the host has mise at all: the fact that says the toolchains
    /// on this machine are pinned per project rather than installed globally.
    pub mise: Option<String>,
    /// `mise ls --current` in the host's root: one entry per tool, as `name version`.
    pub mise_tools: Vec<String>,
    /// The confinement helper's version, when the host has one at all. A presence fact: whether the
    /// helper works cannot be read off a version, so it is measured by a placement and cached per
    /// host, and [`Facts::confine`] carries that answer once anything has asked.
    pub bwrap: Option<String>,
    /// Whether a command on this host would run inside a boundary, from the cache a placement fills:
    /// `confined (bubblewrap 0.11.1 ...)`, or the measured refusal. `None` means nothing has asked in
    /// this process, which is said rather than guessed either way.
    pub confine: Option<String>,
    /// What this host's ssh calls have been doing, as [`crate::rpc::report`] renders it - the last few
    /// and the slowest. Empty when nothing in this process has called the host yet.
    pub calls: String,
}

/// The one script a probe runs, and it only ever reads. `key=value` lines rather than a formatted
/// report, so the parser here has no opinions about the far side's locale, and every fact is one
/// line whose absence is meaningful.
fn facts_script(root: &str) -> String {
    format!(
        r#"r={root}; case "$r" in '~/'*) r="$HOME/${{r:2}}" ;; esac
[ -d "$r" ] || r="$HOME"
echo "uname=$(uname -n 2>/dev/null)"
echo "cores=$( (nproc 2>/dev/null || getconf _NPROCESSORS_ONLN 2>/dev/null || echo 0) | head -1)"
echo "load=$(cat /proc/loadavg 2>/dev/null | cut -d' ' -f1,2,3)"
echo "mem_total_kb=$(awk '/^MemTotal:/ {{print $2}}' /proc/meminfo 2>/dev/null | head -1)"
echo "mem_avail_kb=$(awk '/^MemAvailable:/ {{print $2}}' /proc/meminfo 2>/dev/null | head -1)"
echo "disk_path=$r"
echo "disk_free_kb=$(df -Pk "$r" 2>/dev/null | awk 'NR==2 {{print $4}}')"
# **The confinement helper, if the machine has one** (RH3). Existence and version only: whether it
# can enter a namespace is measured by a placement, with the argv a command would really run and
# against this box's own inodes — see `crate::exec::confine::probe_script`.
_bw=$(command -v bwrap 2>/dev/null)
if [ -n "$_bw" ]; then echo "bwrap=$("$_bw" --version 2>&1 | head -1)"; fi
echo "git=$(command -v git 2>/dev/null)"
echo "cargo=$(command -v cargo 2>/dev/null)"
# **The toolchain, which on these machines means mise.** Found the way every remote command
# finds it (`~/.local/bin/mise`, then `PATH`), and asked from the host's own root so the answer
# is the project's pinned versions rather than some other directory's.
m=$(command -v mise 2>/dev/null)
if [ -z "$m" ] && [ -x "$HOME/.local/bin/mise" ]; then m="$HOME/.local/bin/mise"; fi
if [ -n "$m" ]; then
  echo "mise=$("$m" --version 2>/dev/null | head -1)"
  echo "mise_tools=$("$m" -C "$r" ls --current 2>/dev/null | awk 'NF {{printf "%s%s %s", sep, $1, $2; sep="; "}}')"
fi
"#,
        root = quote(root)
    )
}

/// Ask one host what it is doing, and never write there. The connection is [`Ssh::connect`]'s, so its
/// two refusals arrive as sentences before anything is asked; then one bounded call gives the facts,
/// and the answer and any refusal both carry [`crate::rpc::report`] for that host.
pub fn probe(
    host: &Host,
    program: Option<PathBuf>,
    paths: Paths,
    within: Duration,
) -> Result<Facts, String> {
    let ssh = Ssh::connect(host.clone(), program, paths)?.with_bound(within);
    let answer = ssh.sh_step_within(
        &format!("a probe of {}", host.name),
        &facts_script(&host.root),
        within,
    );
    let (said, stderr, exit) = match answer {
        Ok(t) => t,
        Err(e) => {
            // The records go out with the refusal: a probe that failed is the one with something to
            // say about why, and a caller that read only the sentence would be back to guessing at a
            // flaky host.
            return Err(format!(
                "{e}\n{}",
                crate::rpc::report(&host.name).trim_end()
            ));
        }
    };
    if exit != 0 {
        let why = if stderr.is_empty() {
            said.trim().to_string()
        } else {
            String::from_utf8_lossy(&stderr).trim().to_string()
        };
        return Err(format!(
            "the probe of {} exited {exit}: {}\n{}",
            host.name,
            if why.is_empty() {
                "it said nothing"
            } else {
                &why
            },
            crate::rpc::report(&host.name).trim_end()
        ));
    }
    let mut facts = facts_of(&said);
    facts.calls = crate::rpc::report(&host.name);
    // What this process has already measured about a boundary on the host, when anything has. It is
    // the cache a placement fills ([`CONFINE`]) and not a second probe, so its absence is reported as
    // *nothing has asked yet* rather than guessed at.
    facts.confine = confined_here(&host.name);
    Ok(facts)
}

/// The boundary a host was measured with, in this process: the cache, or `None`. `Some("confined
/// (...)")` is a boundary that really runs; `Some("nothing was confined ...")` is a host that
/// answered and cannot; `None` is *nobody has asked here yet*.
fn confined_here(host: &str) -> Option<String> {
    let probe = CONFINE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(host)
        .cloned()?;
    if probe.helper.trim().is_empty() {
        return Some("nothing was confined: no `bwrap` on that host's `PATH`".to_string());
    }
    // The version is what the probe reported; whether the namespaces were entered is what the
    // readback said, and the two are compared the way `measured_from_probe` compares them.
    let mut entered = 0;
    for n in crate::exec::confine::Namespace::REQUIRED {
        let inside = probe.inside.get(n.proc_name());
        let outer = probe.outer.get(n.proc_name());
        if inside.is_some() && inside != outer {
            entered += 1;
        }
    }
    Some(
        if entered == crate::exec::confine::Namespace::REQUIRED.len() {
            format!(
                "confined ({}): user, mount and pid namespaces measured from inside",
                probe.version
            )
        } else {
            format!(
                "nothing was confined: the helper did not enter the namespaces ({} of {} measured)",
                entered,
                crate::exec::confine::Namespace::REQUIRED.len()
            )
        },
    )
}

/// The command line a confined remote spawn becomes: the helper's argv, `--`, then the command's own
/// argv. One function for both spawns, because two spellings is how the group kill gets lost. The
/// refusal is returned, not swallowed: running unconfined is the boundary quietly not being there.
fn wrap_confined(
    confine: &crate::exec::Bwrap,
    cwd: &str,
    env: &[(String, String)],
    argv: &[String],
) -> Result<String, crate::exec::ExecError> {
    let plan = crate::exec::ConfinePlan {
        cwd: std::path::Path::new(cwd),
        env,
    };
    let pieces = confine.remote_pieces(&plan, crate::exec::Session::Kept)?;
    let mut line = crate::exec::confine::join(&pieces);
    for a in argv {
        line.push(' ');
        line.push_str(&quote(a));
    }
    Ok(line)
}

/// One session's view on a host, and the boundary measured for it, or the reason there is none. The
/// view is the policy a local confined session gets, spelled for the far machine: the checkout
/// writable at its own path, the system roots and toolchain read-only, `$HOME` letibot's own.
fn confine_a_checkout(
    ssh: &Ssh,
    home: &str,
    worktree: &str,
    mirror: &str,
    egress: &crate::exec::Egress,
) -> (Option<crate::exec::Bwrap>, Option<String>) {
    use crate::exec::confine::{Grant, HomeView, ViewSpec};
    // The mirror is part of the checkout, as git sees it: the checkout is a `git worktree` of the bare
    // mirror beside it, and every object, ref and index lives there. A view that bound only the
    // checkout hands the session a tree with no repository. Writable, because a commit writes there.
    let provisional = ViewSpec::project_only(worktree)
        .with_home(HomeView::Dir(PathBuf::from(home)))
        // `/tmp` is this session's own directory on that machine, created beside the home by `place`
        // and bound at `/tmp`, so a file one call writes is there for the next. A fresh tmpfs per
        // command is what a two-call scratch idiom silently loses.
        .with_tmp(crate::exec::confine::TmpView::Session(
            PathBuf::from(home).join("tmp"),
        ))
        .granting(Grant::ReadWrite {
            path: PathBuf::from(mirror),
            why: "the repository this checkout is a worktree of — its objects, refs and the \
                  worktree's own index live there, so without it `git` inside the view has no \
                  repository and the branch, which is the whole deliverable, cannot be committed"
                .to_string(),
        });
    let probe = match ssh.confinement_probe(&provisional, egress) {
        Ok(p) => p,
        Err(why) => return (None, Some(why)),
    };
    // What the host said is there, in the order [`crate::exec::confine::GRANT_CANDIDATES`] lists it.
    let mut view = provisional;
    for g in crate::exec::seat::grants_for(&probe.toolchains) {
        view = view.granting(g);
    }
    match crate::exec::confine::measured_from_probe(&probe, view, egress.clone()) {
        Ok(b) => (Some(b), None),
        Err(e) => (None, Some(e.to_string())),
    }
}

/// The environment that makes the operator's toolchain the one that runs, when it is in the view:
/// with `$HOME` a fresh directory, `rustup` looks for its toolchain under `$HOME/.rustup` and finds
/// nothing, so the two variables point at the paths granted read-only.
fn toolchain_env(confine: Option<&crate::exec::Bwrap>) -> Vec<(String, String)> {
    let Some(b) = confine else {
        return Vec::new();
    };
    let Some(boundary) = crate::exec::Confinement::boundary(b) else {
        return Vec::new();
    };
    crate::exec::seat::env_for_grants(boundary.view.grants.iter().map(|g| g.path()))
}

/// The `key=value` lines of [`facts_script`] as a [`Facts`]. Unknown keys are ignored and missing
/// ones are absent: a host that answers half a probe is half-known, which is a fact itself.
pub fn facts_of(said: &str) -> Facts {
    let mut f = Facts::default();
    for line in said.lines() {
        let Some((key, value)) = line.trim_end_matches('\r').split_once('=') else {
            continue;
        };
        let value = value.trim();
        let number = || value.parse::<u64>().ok();
        match key.trim() {
            "uname" => f.uname = value.to_string(),
            "cores" => f.cores = number().unwrap_or(0),
            "load" => f.load = value.to_string(),
            "mem_total_kb" => f.mem_total_kb = number(),
            "mem_avail_kb" => f.mem_avail_kb = number(),
            "disk_path" => f.disk_path = value.to_string(),
            "disk_free_kb" => f.disk_free_kb = number(),
            "git" => f.git = (!value.is_empty()).then(|| value.to_string()),
            "cargo" => f.cargo = (!value.is_empty()).then(|| value.to_string()),
            "mise" => f.mise = (!value.is_empty()).then(|| value.to_string()),
            "mise_tools" => {
                f.mise_tools = value
                    .split(';')
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .map(str::to_string)
                    .collect()
            }
            "bwrap" => f.bwrap = (!value.is_empty()).then(|| value.to_string()),
            _ => {}
        }
    }
    f
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

/// How many commands this process has run on a remote host, for the per-command name of the file
/// that records its process group. A counter rather than a timestamp: two runs in the same
/// millisecond would otherwise share a file, and one would then cancel the other's group.
static RUNS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// What to place.
#[derive(Debug, Clone)]
pub struct SshSpec {
    /// The host, from [`Hosts`] - resolved by the caller so that a wrong name is refused before
    /// anything is arranged.
    pub host: Host,
    /// The session id: the name of the remote worktree and of the branch.
    pub name: String,
    /// The repository to send. The local tree's `HEAD` goes over as a bundle, so this is the
    /// repository the work is in and not necessarily the directory the session was started in; the
    /// caller resolves it.
    pub source: PathBuf,
    /// `write` refused for a downgraded child, exactly as it is for a VM.
    pub writable: bool,
    /// `exec` refused for a downgraded child: no process host and no `bash`.
    pub exec: bool,
    /// The ssh program, when the caller names one (a test's fake). `None` is
    /// `$LETIBOT_SSH`, else `ssh`.
    pub ssh: Option<PathBuf>,
    /// Where letibot's ssh material is, when the caller says (a test's own home and config
    /// directory). `None` is [`Paths::from_env`], the operator's. A field rather than an environment
    /// variable, because a test that set `$HOME` would race every other reader of it.
    pub paths: Option<Paths>,
    /// The wall clock every call that is not a command is given, when the caller names one. `None`
    /// is [`STEP_BOUND`]. A test's door: a fake ssh that never answers should be measured in a
    /// second rather than in twenty.
    pub bound: Option<Duration>,
    /// The session's network rule, as the confined view expresses it. A field rather than a
    /// constant, because the answer is a decision made above this file: a mode that admits
    /// `web_fetch` must not have its `curl` fail after the gate said yes, and the reverse holds too.
    pub egress: crate::exec::Egress,
    /// Whether to ask this host whether it can confine. `None` is production and is the only value a
    /// real caller passes; `Some(false)` is a fixture's door, because the probe runs on the machine
    /// running the test and the same test would take two paths on two boxes.
    pub confine: Option<bool>,
    /// Boot a firecode VM on the host and run every command of this session inside it (`--host NAME
    /// --vm`). The guest is the boundary, so no `bwrap` is probed for.
    pub vm: bool,
}

/// What a placement's branch needs to come home: the local repository, the url the fetch reads and
/// the refspec it writes, and the names the sentence says out loud. Held beside the connection,
/// because the daemon's last act holds no backend and must still fetch.
#[derive(Clone)]
struct Home {
    local_repo: PathBuf,
    url: String,
    refspec: String,
    worktree: String,
    branch: String,
    back_branch: String,
    /// The local branch the work started from, for the count the sentence gives.
    base: String,
}

/// A session whose tools run on another machine.
pub struct SshBackend {
    ssh: Ssh,
    /// The host's name, for sentences and cards.
    host: String,
    /// The resolved remote root, absolute.
    root: String,
    /// `<root>/<repo>/<short id>`: the checkout the session works in.
    worktree: String,
    /// `remote/<short id>`, the branch the remote commits to.
    branch: String,
    /// `remote/<host>/<short id>`, what that branch becomes in the local repository.
    back_branch: String,
    /// The local repository the tree came from and comes back to.
    local_repo: PathBuf,
    /// The fetch home, gathered once here: [`bring_back`](SshBackend::bring_back) reads it, and so
    /// does the daemon's last act, which reaches no close.
    home: Home,
    processes: Option<Arc<RemoteProcesses>>,
    writable: bool,
    /// The boundary this session's commands run inside, when there is one: measured on the host by
    /// [`Ssh::confinement_probe`] and applied to this session's own view. `None` with
    /// [`SshBackend::unconfined`] set is a host that cannot confine, which the mode and cards read.
    confine: Option<crate::exec::Bwrap>,
    /// The view, kept beside the confinement so the file tools can ask the same question a command's
    /// view answers - *is this path in here at all* - without cloning a `Boundary` on every call.
    view: Option<crate::exec::ViewSpec>,
    /// The environment a confined command carries so that the operator's toolchain is what runs:
    /// `RUSTUP_HOME` and `CARGO_HOME` at the read-only grants. Without them a confined `cargo` looks
    /// under the session's own empty `$HOME` and finds no toolchain.
    confined_env: Vec<(String, String)>,
    /// Why it is not confined, when it is not: the probe's own reason.
    unconfined: Option<String>,
    /// What the placement had to say: the worktree, and the local edits that stayed here.
    notes: Vec<String>,
    /// The guest this session's commands run in, when it was placed in one: `close` brings it down and
    /// collects its work, and `describe` says so.
    vm: Option<RemoteVm>,
    /// What [`ExecBackend::close`] found when it brought the branch back, said once.
    brought_back: Mutex<Option<String>>,
    /// What the sweep ended on the host, said once: `None` before it has run, `Some("")` when it
    /// found nothing. A second close reports what the first found rather than a machine it has
    /// already emptied.
    swept: Mutex<Option<String>>,
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
    /// Arrange the checkout on the remote and open the session on it. Every failure names the step -
    /// the repository, the bundle, the mirror, the worktree - because a message that says only "ssh
    /// failed" would leave the operator to guess which.
    pub fn place(spec: &SshSpec) -> Result<SshBackend, BackendError> {
        let host = spec.host.name.clone();
        // The connection is opened first, and the two refusals it can give have to happen before
        // anything is arranged: a key that is missing or inside the operator's own ssh directory, and
        // a host key nobody has accepted for letibot.
        let mut ssh = Ssh::connect(
            spec.host.clone(),
            spec.ssh.clone(),
            spec.paths.clone().unwrap_or_else(Paths::from_env),
        )
        .map_err(|why| BackendError::Io(format!("placing this session on {host}: {why}")))?;
        if let Some(within) = spec.bound {
            ssh = ssh.with_bound(within);
        }
        // From here on, every command this session runs on that machine carries a mark - including
        // the arranging steps below, which fork `git` there like anything else. It names the PLACEMENT
        // and not only the session: two daemons can place the same session id on one host, and a sweep
        // must end only what it started.
        static MARKS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        ssh = ssh.with_mark(&format!(
            "{}-{}-{}",
            spec.name,
            std::process::id(),
            MARKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let place =
            |e: BackendError| BackendError::Io(format!("placing this session on {host}: {e}"));

        // Can this machine boot a guest, asked before anything is arranged there: a host with no
        // `firecode` or no `/dev/kvm` is a refusal the caller can act on, and it comes before a bundle
        // is sent and a worktree is made.
        let firecode = if spec.vm {
            Some(ssh.firecode_on_host().map_err(|why| {
                place(BackendError::Io(format!(
                    "{why}. Leave `--vm` out to run this session's tools on {host} itself, in a \
                     checkout of the same repository."
                )))
            })?)
        } else {
            None
        };

        // The repository, or the refusal that names the fix: a remote session works on a checkout of a
        // repository, so a folder that is not one cannot carry its work back, and the contract refuses
        // it rather than copying a directory the local machine could never fetch from.
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
        let back_branch = back_branch_for(&host, &spec.name);

        // The root, resolved ONCE on the remote: `root = "~/letibot-remote"` is a fact about the
        // remote's home, not this machine's, so it is expanded there and every path below is absolute,
        // which is what `git fetch` needs. A relative answer is refused rather than used.
        let root = resolved_root(&ssh, &spec.host.root, true).map_err(place)?;
        let mirror = format!("{root}/{repo_name}.git");
        let worktree = format!("{root}/{repo_name}/{short}");

        // `mise exec -C` needs a directory that EXISTS, and the checkout below does not exist until
        // `git worktree add` has run, so the wrapper is pointed at the root until then. mise refuses a
        // missing `-C`, which would fail the placement at `making the mirror`.
        ssh = ssh.with_dir(root.clone());

        // A bare mirror, made COMPLETE by whichever placement gets there first, and reused by every
        // later session onto the same repository. The answer is the directory's `objects/`, not `git
        // init`'s exit status: three `git init --bare` at once on one path leave two of them failing.
        let objects = format!("{mirror}/objects");
        ssh.must(
            "making the mirror",
            &format!(
                "i=0\n\
                 while [ ! -d {o} ]; do\n\
                 \x20 i=$((i+1))\n\
                 \x20 [ $i -le 100 ] || {{ echo {m} could not be made a repository >&2; exit 1; }}\n\
                 \x20 git init --bare -q {m} >/dev/null 2>&1 || sleep 0.05\n\
                 done",
                m = quote(&mirror),
                o = quote(&objects)
            ),
        )
        .map_err(place)?;

        // The tree itself, as a bundle, written into the mirror where the remote `git` can read it.
        // The staging file is unique per call - pid, counter, timestamp - because two sessions placing
        // at once would otherwise meet each other's `.lock`, and the counter is what guarantees it.
        static PLACED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let run = PLACED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let staging = std::env::temp_dir().join(format!(
            "letibot-remote-{}-{}-{}-{}.bundle",
            std::process::id(),
            run,
            short,
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let bundled = git_within(
            &local_repo,
            &["bundle", "create", &staging.display().to_string(), "HEAD"],
            GIT_BOUND,
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
        let incoming = format!(
            "{mirror}/incoming-{}-{}-{}.bundle",
            std::process::id(),
            run,
            short
        );
        ssh.write_abs(&incoming, &bundle).map_err(place)?;
        let staged = format!("refs/heads/letibot/{short}");
        ssh.must(
            "handing the bundle to the remote repository",
            &format!(
                // The fetch's own words, and the bundle removed only after it: `2>&1` into /dev/null
                // was why a failure said only *handing the bundle to the remote repository*. A step
                // asked again must not fail because the first attempt consumed the file.
                "rc=0; said=; b={b}\n\
                 if [ -f \"$b\" ]; then\n\
                 \x20 said=$(git -C {m} fetch -f \"$b\" +HEAD:{st} 2>&1) || rc=$?\n\
                 \x20 rm -f \"$b\"\n\
                 fi\n\
                 if [ \"$rc\" != 0 ]; then echo \"git fetch refused the bundle: $said\" >&2; \
                 exit $rc; fi\n\
                 git -C {m} rev-parse --verify {st} >/dev/null",
                m = quote(&mirror),
                b = quote(&incoming),
                st = quote(&staged)
            ),
        )
        .map_err(place)?;

        // The checkout. A second session on the same id (a resume) finds its worktree already there and
        // keeps it - the branch and everything committed on it.
        let existing = ssh
            .sh_step(
                "checking whether the checkout is already there",
                &format!("[ -d {} ] && echo yes", quote(&worktree)),
            )
            .map_err(place)?
            .0;
        if !existing.trim().eq("yes") {
            // Not retried: `git worktree add -b` is not idempotent, so a second attempt after a call
            // that may have worked would report *already exists* over a placement that is fine. A VM
            // placement gets a CLONE instead, because the guest's copy has no mirror behind it.
            let checkout = if spec.vm {
                // A clone keeps other branches as `refs/remotes/origin/*`, so the branch the
                // bundle went over as is named there.
                let cloned = format!("refs/remotes/origin/letibot/{short}");
                format!(
                    "git -C {m} clone -q --no-checkout {m} {wt} >/dev/null && \
                     git -C {wt} checkout -q -b {br} {st}",
                    m = quote(&mirror),
                    br = quote(&branch),
                    wt = quote(&worktree),
                    st = quote(&cloned)
                )
            } else {
                format!(
                    "git -C {m} worktree add -b {br} {wt} {st} >/dev/null",
                    m = quote(&mirror),
                    br = quote(&branch),
                    wt = quote(&worktree),
                    st = quote(&staged)
                )
            };
            ssh.must_within(
                "checking the branch out",
                &checkout,
                Retry::Never,
                ssh.bound(),
            )
            .map_err(place)?;
        }

        // Now the checkout exists, so the wrapper can be pointed at it: a project pins its versions in
        // a `mise.toml` in ITS tree, so `-C` has to be the tree the work is in and not the root, and it
        // can only be named once the checkout has been made.
        ssh = ssh.with_dir(worktree.clone());

        // The session's own job directory is not part of the tree: `.letibot-remote/` holds a
        // background job's launcher, output, pid and exit file, so it appears as soon as the session
        // runs anything in the background. The checkout's exclude file says so.
        ssh.must(
            "excluding the session's own job directory",
            &format!(
                "p=$(git -C {wt} rev-parse --git-path info/exclude) || exit 1
\
                 case \"$p\" in /*) ;; *) p={wt}/\"$p\" ;; esac
\
                 mkdir -p \"$(dirname \"$p\")\" || exit 1
\
                 grep -qxF '.letibot-remote/' \"$p\" 2>/dev/null || echo '.letibot-remote/' >> \"$p\"",
                wt = quote(&worktree)
            ),
        )
        .map_err(place)?;

        // The combined placement, booted once the checkout exists: the guest is a copy of that checkout
        // at the same path, so this is after every arranging step whose result the guest starts with
        // (the exclude file above) and before any command.
        let mut vm = None;
        if let Some(found) = firecode {
            let plain = ssh.without_vm();
            let booted = vm_up(&plain, &host, &worktree, found).map_err(place)?;
            ssh = ssh.with_vm(booted.clone());
            vm = Some(booted);
        }

        // What stayed here, and it is not carried. Said with the list, because a session that works on
        // a tree the operator is not looking at must say so.
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

        // Can this machine bound a command, and in what view. Asked once per host per process
        // ([`CONFINE`]), after the checkout exists because the checkout is the writable root of the
        // view. A machine that cannot confine runs unconfined and says so, and must never claim it.
        let mut session_home = None;
        let (confine, unconfined) = if let Some(vm) = &vm {
            // The VM is the boundary, and there is nothing for `bwrap` to bound: the guest holds a
            // copy of the checkout and none of that machine's filesystem, so a probe would only
            // measure a confinement nothing runs in.
            notes.push(format!(
                "commands run inside a firecode VM on {host}, booted on a copy of {} at that \
                 same path: the guest is the boundary, and what the session commits is collected \
                 when the VM stops",
                vm.project
            ));
            (None, None)
        } else if !spec.exec {
            // Nothing runs here, so there is nothing to bound: a downgraded child seats no exec
            // tools, and probing a host for a boundary no command will use is a round trip spent on
            // a question nobody asked.
            (
                None,
                Some(
                    "this session seats no exec, so no command of it runs and nothing was \
                     probed"
                        .to_string(),
                ),
            )
        } else {
            match spec.confine {
                Some(false) => (
                    None,
                    Some(
                        "this caller asked for no boundary, so nothing was probed and nothing was \
                         measured"
                            .to_string(),
                    ),
                ),
                _ => {
                    let dir = format!("{root}/{HOME_DIR}/{short}");
                    // The session's home, and its `/tmp` beside it: a confined command's `/tmp` is
                    // `dir/tmp`, the same lifetime as `dir`, so a file one call wrote is there in
                    // the next. Neither is removed when the session ends.
                    ssh.must(
                        "making the session's own home and its /tmp on that machine",
                        &format!("mkdir -p {} {}", quote(&dir), quote(&format!("{dir}/tmp"))),
                    )
                    .map_err(place)?;
                    let measured = confine_a_checkout(&ssh, &dir, &worktree, &mirror, &spec.egress);
                    session_home = Some(dir);
                    measured
                }
            }
        };
        if let Some(why) = &unconfined {
            notes.push(format!(
                "commands on {host} run UNCONFINED — {why} — so every one of them is a decision \
                 and the tree there is your own home with your own files in it"
            ));
        } else if let Some(b) = &confine {
            notes.push(format!(
                "commands on {host} run inside a measured boundary ({}): the checkout is \
                 writable, the toolchain is read-only, everything else of that machine is absent \
                 from the view, and `$HOME` there is {}",
                crate::exec::Confinement::describe(b)
                    .lines()
                    .next()
                    .unwrap_or("bubblewrap"),
                session_home
                    .as_deref()
                    .unwrap_or("the session's own directory")
            ));
        }
        let confined_env = toolchain_env(confine.as_ref());

        let processes = if spec.exec {
            // The same connection, cloned: one ControlPath and one identity file, so a job `ssh`
            // pays the round trip the session's own commands already paid for.
            Some(Arc::new(
                RemoteProcesses::new(ssh.clone(), host.clone(), worktree.clone())
                    .confined(confine.clone(), confined_env.clone()),
            ))
        } else {
            None
        };

        // The fetch home, gathered here so everything that brings a branch back - the close, the
        // daemon's last act, a collect asked for later - reads one set of names. A guest's checkout
        // is a clone and holds its own branch; every other placement's is a worktree of the mirror.
        let source = match &vm {
            Some(_) => &worktree,
            None => &mirror,
        };
        let fetch = Home {
            local_repo: local_repo.clone(),
            url: format!("{}:{source}", ssh.destination()),
            refspec: format!("+{branch}:refs/heads/{back_branch}"),
            worktree: worktree.clone(),
            branch: branch.clone(),
            back_branch: back_branch.clone(),
            base: base.clone(),
        };

        let backend = SshBackend {
            ssh,
            host,
            root,
            worktree,
            branch,
            back_branch,
            local_repo,
            home: fetch,
            processes,
            writable: spec.writable,
            notes,
            view: confine
                .as_ref()
                .and_then(crate::exec::Confinement::boundary)
                .map(|b| b.view.clone()),
            confine,
            confined_env,
            unconfined,
            vm,
            brought_back: Mutex::new(None),
            swept: Mutex::new(None),
        };
        // What is running here is a session's work until somebody ends it, and the daemon's exit
        // closes no backend: the last act sweeps this list, and fetches what it holds, for exactly
        // that reason.
        remember_placement(&backend.ssh, backend.home_for_the_last_act());
        Ok(backend)
    }

    /// The host's name, for a card, a progress line or a test.
    pub fn host_name(&self) -> &str {
        &self.host
    }

    /// The guest this session's commands run in, when it was placed in one.
    pub fn vm(&self) -> Option<&RemoteVm> {
        self.vm.as_ref()
    }

    /// The session's end, when its tools ran in a guest: the VM comes down, the guest's commits are
    /// taken onto the host checkout, and the branch comes home. Every step is idempotent, so a
    /// second call reports what the first found.
    fn settle(&self) -> String {
        let mut said: Vec<String> = Vec::new();
        // First, before anything is fetched home: what the session STARTED on that machine goes with
        // it. A host has no cgroup, so this is the mark sweep and not a scope - and it runs before the
        // branch comes back, because a process still writing in the checkout when the fetch reads it
        // is the one state nobody can describe. Said once, so a second close reports the first's
        // finding rather than an empty machine it has already emptied.
        let swept = {
            let already = self.swept.lock().unwrap_or_else(|e| e.into_inner()).clone();
            match already {
                Some(had) => had,
                None => {
                    let had = self.ssh.end_session_processes().unwrap_or_default();
                    *self.swept.lock().unwrap_or_else(|e| e.into_inner()) = Some(had.clone());
                    had
                }
            }
        };
        said.extend(Some(swept).filter(|s| !s.is_empty()));
        if let Some(vm) = &self.vm {
            let plain = self.ssh.without_vm();
            // The session's own close is the authoritative act, so the last act has nothing left
            // to do here: one `down` per guest, whichever door ends the session.
            forget_guest(&self.host, vm);
            said.push(
                match plain.must_within(
                    "bringing the firecode VM down",
                    &vm.host_command("down", &["--project", &vm.project]),
                    Retry::Never,
                    VM_BOUND,
                ) {
                    Ok(_) => format!("the VM on {} was brought down", self.host),
                    Err(e) => format!("the VM on {} was NOT brought down: {e}", self.host),
                },
            );
            said.push(self.collect_from_guest(&plain, vm));
        }
        said.push(self.bring_back());
        // The sweep has run and the branch is home: nothing of this session is left to find. A second
        // close of the same session (two doors, one `settle`) reports what the first did.
        forget_placement(&self.ssh);
        said.retain(|s| !s.is_empty());
        said.join("; ")
    }

    /// The guest's commits, taken onto the host's checkout. `firecode down` leaves the guest's tree
    /// in a sibling directory of the project, and without this step a session that committed inside
    /// the VM would come back empty.
    fn collect_from_guest(&self, plain: &Ssh, vm: &RemoteVm) -> String {
        let br = format!("refs/heads/{}", self.branch);
        // A fetch, then a fast-forward, never a merge of a bare sha: the guest's commit lives in the
        // guest's own copy, so the object has to come over first. `--ff-only` refuses a copy that
        // has diverged from the checkout.
        let script = format!(
            "i=0\n\
             while [ $i -lt {n} ]; do\n\
             \x20 for d in {wt}-*; do\n\
             \x20   [ -e \"$d/.git\" ] || continue\n\
             \x20   s=$(git -C \"$d\" rev-parse -q --verify {br}) || continue\n\
             \x20   [ \"$s\" = \"$(git -C {wt} rev-parse HEAD)\" ] && continue\n\
             \x20   git -C {wt} fetch -q \"$d\" {br} 2>/dev/null || continue\n\
             \x20   if git -C {wt} merge --ff-only FETCH_HEAD >/dev/null 2>&1; then\n\
             \x20     echo \"the guest's commits are on {branch} now\"; exit 0\n\
             \x20   fi\n\
             \x20 done\n\
             \x20 i=$((i+1)); sleep 2\n\
             done\n\
             echo \"no copy of the guest landed beside {wt} in {secs} s, so the branch is where it \
             was. If firecode's copy-out is still running it appears as {wt}-<stamp>, and \
             `git -C {wt} fetch <that> {br}` takes it\"",
            n = VM_COPY_OUT_QUIET.as_secs() / 2,
            wt = quote(&vm.project),
            br = quote(&br),
            branch = self.branch,
            secs = VM_COPY_OUT_QUIET.as_secs()
        );
        match plain.must_within(
            "collecting the guest's commits",
            &script,
            Retry::Never,
            VM_COPY_OUT_QUIET + self.ssh.bound(),
        ) {
            Ok(said) => said.trim().to_string(),
            Err(e) => format!("the guest's commits could NOT be collected: {e}"),
        }
    }

    /// The boundary this session's commands run inside, when there is one: the measured view, so a
    /// caller answers *is this path inside it* and *what is the disclosure* from the same fact that
    /// produced the argv.
    pub fn confinement(&self) -> Option<crate::exec::Boundary> {
        self.confine
            .as_ref()
            .and_then(crate::exec::Confinement::boundary)
            .cloned()
    }

    /// Why a command on this host is not confined, when it is not: the probe's own reason, with the
    /// host in it.
    pub fn unconfined_reason(&self) -> Option<&str> {
        self.unconfined.as_deref()
    }

    /// Is a command run by this backend inside a measured boundary? `false` for a host whose
    /// probe failed, which is the fact `harnessd` reads as `backend_confined` and the gate reads
    /// for its boundary facts.
    pub fn is_confined(&self) -> bool {
        self.confine.is_some()
    }

    /// The script a command becomes, confined where there is a boundary: the helper's argv, then
    /// `--`, then the command's own `argv`. The argv is spliced rather than wrapped in another
    /// shell, since `Command::argv` is already the shell the caller chose.
    fn confined_script(
        &self,
        cwd: &str,
        env: &[(String, String)],
        argv: &[String],
    ) -> Result<String, crate::exec::ExecError> {
        let mut pairs: Vec<(String, String)> = self.confined_env.clone();
        pairs.extend(env.iter().cloned());
        wrap_confined(
            self.confine
                .as_ref()
                .expect("only called where a boundary was measured"),
            cwd,
            &pairs,
            argv,
        )
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

    /// What the placement had to say: the worktree, the branch that comes back, and the local edits
    /// that were left behind. Printed by the opener, not by the session.
    pub fn placement_notes(&self) -> &[String] {
        &self.notes
    }

    /// Is this path in the view at all: the question a command's own sandbox answers, asked by the
    /// file tools. `Ok(abs)` when there is no view or the path is inside it, [`BackendError::Outside`]
    /// when not - which is how the never-write list holds on a path this box cannot resolve.
    fn in_view(&self, path: &str) -> Result<String, BackendError> {
        let abs = self.abs(path);
        match &self.view {
            Some(v) if v.outside(std::path::Path::new(&abs)) => Err(BackendError::Outside(abs)),
            _ => Ok(abs),
        }
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

    /// Bring the branch back, and say what came with it. Idempotent and safe to call twice: the
    /// second call reports what the first found.
    fn bring_back(&self) -> String {
        if let Some(said) = self
            .brought_back
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            return said;
        }
        let said = self
            .home
            .fetch(&self.ssh, &format!("the session on {} ended", self.host));
        *self.brought_back.lock().unwrap_or_else(|e| e.into_inner()) = Some(said.clone());
        said
    }

    /// The fetch home the daemon's last act needs, or `None` for a guest placement: its commits are
    /// in the guest's copy until a close collects them, and a fetch made before that copy is merged
    /// would count no commits and read as work that had been lost.
    fn home_for_the_last_act(&self) -> Option<Home> {
        self.vm.is_none().then(|| self.home.clone())
    }
}

impl Home {
    /// One placement's branch, fetched into the local repository, and the sentence that says what
    /// arrived. `opening` is the caller's own account of why the fetch ran: a close, the daemon's
    /// last act, or a collect asked for after the daemon that made the placement was gone.
    fn fetch(&self, ssh: &Ssh, opening: &str) -> String {
        let fetched = ssh.fetch_back(&self.local_repo, &self.url, &self.refspec);
        let mut said = match fetched {
            Ok(FetchEnded::Ran(out)) if out.status.success() => {
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
                    "{opening}. Its branch came back here as `{}` — {} commit(s) \
                     on top of `{}`, now at {}: diff it and take what you want",
                    self.back_branch, ahead, self.base, commit
                )
            }
            Ok(FetchEnded::Ran(out)) => format!(
                "{opening} and its branch did NOT come back: `git fetch` said \
                 \"{}\". The work is still there, in {} on that machine (branch `{}`) — reach \
                 it with `git fetch {} {}` once the cause is fixed",
                String::from_utf8_lossy(&out.stderr).trim(),
                self.worktree,
                self.branch,
                self.url,
                self.refspec
            ),
            Ok(FetchEnded::Killed(spent)) => format!(
                "{opening} and its branch did NOT come back: `git fetch` got no \
                 answer within {} s and was killed, so the connection is the suspect. The work is \
                 still there, in {} on that machine (branch `{}`) — fetch it yourself once the \
                 host answers: `git fetch {} {}`",
                spent.as_secs(),
                self.worktree,
                self.branch,
                self.url,
                self.refspec
            ),
            // No slot on the git master: either the queue never cleared, or an earlier attempt was
            // refused and the refusal is part of what `why` says.
            Err(why) => format!(
                "{opening} and its branch did NOT come back: {why}. The work is still \
                 there, in {} on that machine (branch `{}`) — fetch it yourself: `git fetch {} \
                 {}`",
                self.worktree, self.branch, self.url, self.refspec
            ),
        };
        // What was left uncommitted is not in that branch, and a report that named only the branch
        // would be a false "it is all here". `.letibot-remote/` is this backend's own job directory,
        // so it is filtered out rather than reported as the session's work.
        if let Ok((dirty, _, 0)) = ssh.sh_step(
            "listing what is still uncommitted there",
            &format!("cd {} && git status --porcelain", quote(&self.worktree)),
        ) {
            let lines: Vec<&str> = dirty
                .lines()
                .filter(|l| !l.trim().is_empty())
                .filter(|l| !l.contains(".letibot-remote/"))
                .collect();
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
        ssh.exit_master();
        said
    }
}

/// The ssh program this process would run with no override.
fn ssh_program_default() -> PathBuf {
    std::env::var_os("LETIBOT_SSH")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("ssh"))
}

/// The remote root, resolved ONCE on the host: `root = "~/letibot-remote"` is a fact about that
/// machine's home, not this one's, so it is expanded there and every path below is absolute, which
/// is what `git fetch` needs. A relative answer is refused rather than used, and `create` is a
/// placement making the root against a collect that must not make one on a host that has neither.
fn resolved_root(ssh: &Ssh, asked: &str, create: bool) -> Result<String, BackendError> {
    let root = ssh
        .must(
            if create {
                "making the remote root"
            } else {
                "reading the placement's root"
            },
            &format!(
                // `${r:2}`, not `${r#~/}`: bash tilde-expands the pattern of a `${v#...}`, so
                // `#~/` quietly matches nothing and the root becomes `$HOME/~/...`.
                "r={}; case \"$r\" in '~/'*) r=\"$HOME/${{r:2}}\" ;; esac; \
                 {make}cd \"$r\" && pwd -P",
                quote(asked),
                make = if create { "mkdir -p \"$r\" && " } else { "" }
            ),
        )?
        .trim()
        .to_string();
    if !root.starts_with('/') {
        return Err(BackendError::Io(format!(
            "the remote said its root is `{root}`, which is not an absolute path — a \
             `root = \"~/letibot-remote\"` must be expanded by ssh's own shell, and a \
             remote shell that answers a relative path would put every worktree somewhere \
             neither this box nor `git fetch` can name"
        )));
    }
    Ok(root)
}

/// The local branch a placement's work comes home on: `remote/<host>/<short id>`. The one spelling of
/// the rule, because a caller that must NAME that branch without holding the backend - the bridge's
/// stop answer - is telling the operator where to look.
pub fn back_branch_for(host: &str, session: &str) -> String {
    format!("remote/{host}/{}", short_branch_id(session))
}

/// What a collect needs, and nothing a daemon holds: the host, the local repository the branch comes
/// home to, and the session id whose checkout and branch the placement was named by. A placement is
/// the daemon's and dies with it, so these are what a stopped session can still be collected from.
pub struct CollectSpec {
    pub host: Host,
    pub session: String,
    /// The local repository, or a folder inside one: the branch comes home here.
    pub repo: PathBuf,
    pub ssh: Option<PathBuf>,
    pub paths: Paths,
    /// The placement ran in a firecode guest, so its branch is in the checkout and not the mirror.
    pub vm: bool,
}

/// Fetch a stopped session's branch home as `remote/<host>/<short id>`, or say why not. Every name
/// is derived the way [`SshBackend::place`] derived it - the root, the repository's name, the short
/// id - because the daemon that derived them is gone by the time this is asked.
pub fn collect(spec: &CollectSpec) -> Result<String, BackendError> {
    let host = spec.host.name.clone();
    let refuse = |what: String| BackendError::Io(format!("collecting {host}'s branch: {what}"));
    let top = git(&spec.repo, &["rev-parse", "--show-toplevel"]).map_err(|e| {
        refuse(format!(
            "{} is not a git repository, and the branch comes home to one ({e})",
            spec.repo.display()
        ))
    })?;
    let local_repo = PathBuf::from(top.trim());
    let repo_name = local_repo
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| refuse(format!("{} has no name", local_repo.display())))?;
    let ssh = Ssh::connect(spec.host.clone(), spec.ssh.clone(), spec.paths.clone())
        .map_err(|why| refuse(why))?;
    let root = resolved_root(&ssh, &spec.host.root, false).map_err(|e| refuse(e.to_string()))?;
    let short = short_branch_id(&spec.session);
    let branch = format!("remote/{short}");
    let back_branch = back_branch_for(&host, &spec.session);
    let worktree = format!("{root}/{repo_name}/{short}");
    let mirror = format!("{root}/{repo_name}.git");
    // A host with nothing there is a placement made somewhere else, and `git fetch` would refuse
    // with words about a path this box cannot see; this names the path somebody can go and look at.
    // The checkout is the witness and not the mirror: one mirror is shared by every session on the
    // same repository, so its presence says nothing about whether THIS session was placed here.
    if ssh
        .must(
            "looking for the placement's checkout",
            &format!("[ -d {} ]", quote(&worktree)),
        )
        .is_err()
    {
        return Err(refuse(format!(
            "there is nothing at {worktree}, so this session was placed on another host or its \
             checkout is gone. The branch `{back_branch}` in {} is what came home, if anything did",
            local_repo.display()
        )));
    }
    let source = if spec.vm { &worktree } else { &mirror };
    // The count the sentence gives is against the local branch the collect runs from, which is a
    // measurement of that branch and not a guess at the one the session was placed from.
    let base = match git(&local_repo, &["symbolic-ref", "--short", "-q", "HEAD"]) {
        Ok(b) => b.trim().to_string(),
        Err(_) => git(&local_repo, &["rev-parse", "--short", "HEAD"])
            .unwrap_or_default()
            .trim()
            .to_string(),
    };
    let home = Home {
        local_repo,
        url: format!("{}:{source}", ssh.destination()),
        refspec: format!("+{branch}:refs/heads/{back_branch}"),
        worktree,
        branch,
        back_branch,
        base,
    };
    Ok(home.fetch(&ssh, &format!("the session on {host} was collected")))
}

/// The last eight usable characters of a session id, as a branch and directory name. Alphanumerics
/// only: a session id is a time and a random tail, the tail is what tells two apart, and a `-`
/// among eight characters buys a branch name with a seam in it.
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
    git_within(repo, args, GIT_BOUND)
}

/// One local `git` call with a wall clock on it, for the steps that can reach another machine:
/// `bundle create` on a large repository, and the `fetch` whose transport is ssh. A `git` left
/// waiting on a dead connection holds the op that asked for it, so it is killed at the deadline.
fn git_within(repo: &Path, args: &[&str], within: Duration) -> Result<String, BackendError> {
    let mut argv: Vec<String> = vec!["-C".into(), repo.display().to_string()];
    argv.extend(args.iter().map(|a| a.to_string()));
    match run_bounded(Path::new("git"), &argv, within) {
        Some(out) if out.status.success() => Ok(String::from_utf8_lossy(&out.stdout).into_owned()),
        Some(out) => Err(BackendError::Io(format!(
            "git {} in {}: {}",
            args.join(" "),
            repo.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ))),
        None => Err(BackendError::Io(format!(
            "git {} in {} got no answer within {} s: it was killed rather than waited on. The \
             step that needed it did not finish.",
            args.join(" "),
            repo.display(),
            within.as_secs()
        ))),
    }
}

/// Boot a firecode guest on the host for this session, or say which step refused. The host is
/// asked once per process what it has ([`Ssh::probe_firecode`]), because a machine without
/// `firecode` or without `/dev/kvm` is a refusal a caller can act on.
fn vm_up(plain: &Ssh, host: &str, project: &str, found: VmHost) -> Result<RemoteVm, BackendError> {
    let vm = RemoteVm {
        firecode: found.firecode,
        project: project.to_string(),
    };
    plain
        .must_within(
            "booting the firecode VM",
            &vm.host_command("up", &["--project", project]),
            Retry::Never,
            VM_BOUND,
        )
        .map_err(|e| BackendError::Io(format!("booting a firecode VM on {host}: {e}")))?;
    remember_guest(plain, &vm);
    Ok(vm)
}

/// A file into the guest, which is not the host: `firecode in` does not carry the caller's stdin, so a
/// small file travels inside the command and a large one is staged on the host and carried across by
/// `firecode cp` (the split the local VM backend makes for the same reason).
fn write_into_guest(
    ssh: &Ssh,
    vm: &RemoteVm,
    path: &str,
    bytes: &[u8],
) -> Result<(), BackendError> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static ATTEMPT: AtomicU64 = AtomicU64::new(0);
    let n = ATTEMPT.fetch_add(1, Ordering::Relaxed);
    let dir = match path.rfind('/') {
        Some(0) => "/".to_string(),
        Some(i) => path[..i].to_string(),
        None => ".".to_string(),
    };
    let tmp = format!("{path}.letibot-{}-{n}", std::process::id());
    if bytes.len() <= VM_INLINE_WRITE {
        let b64 = crate::firecode::encode_base64(bytes);
        let script = format!(
            "set -e\n\
             mkdir -p {dir}\n\
             printf %s {b64} | base64 -d > {tmp}\n\
             got=$(wc -c < {tmp} | tr -d ' \\t')\n\
             [ \"$got\" = {len} ] || {{ echo \"the file arrived as $got bytes, not {len}\" >&2; \
             exit 41; }}\n\
             mv -f {tmp} {path}",
            dir = quote(&dir),
            b64 = quote(&b64),
            tmp = quote(&tmp),
            len = bytes.len(),
            path = quote(path)
        );
        ssh.must(&format!("writing {path} in the VM"), &script)?;
        return Ok(());
    }
    // Above what one argument may carry: onto the host first, where stdin works, then over.
    let staged = format!(
        "{}/.letibot-remote/vm-{}-{n}.bin",
        vm.project.trim_end_matches('/'),
        std::process::id()
    );
    let plain = ssh.without_vm();
    plain.write_abs(&staged, bytes)?;
    ssh.must(
        &format!("making room for {path} in the VM"),
        &format!("mkdir -p {}", quote(&dir)),
    )?;
    let name = staged.rsplit('/').next().unwrap_or("staged.bin");
    let handed = plain.must_within(
        "handing the file to the guest",
        &vm.host_command(
            "cp",
            &["--project", &vm.project, &staged, &format!("vm:{dir}/")],
        ),
        Retry::Never,
        VM_BOUND,
    );
    let _ = plain.must(
        "removing the staged file",
        &format!("rm -f {}", quote(&staged)),
    );
    handed?;
    ssh.must(
        &format!("renaming {path} into place in the VM"),
        &format!("mv -f {} {}", quote(&format!("{dir}/{name}")), quote(path)),
    )
    .map(|_| ())
}

/// Every guest this process booted and nothing has brought down yet, with the plain connection that
/// can reach its host: `down` is a verb of the host's own `firecode` client and is never wrapped in
/// `in`.
static GUESTS: std::sync::LazyLock<Mutex<Vec<(Ssh, RemoteVm)>>> =
    std::sync::LazyLock::new(|| Mutex::new(Vec::new()));

/// One placement as the last act reads it: the connection, and what its branch needs to come home.
/// A guest's is `None` - see [`SshBackend::home_for_the_last_act`].
struct Placement {
    ssh: Ssh,
    home: Option<Home>,
}

/// Every placement this process made and no session has closed yet. The last act's list, for the
/// reason [`guests_down`] is one: a forced exit closes no backend at all, and a session served on its
/// own thread holds its backend on that thread, where `Sessions::close_backends` cannot reach it.
static PLACEMENTS: std::sync::LazyLock<Mutex<Vec<Placement>>> =
    std::sync::LazyLock::new(|| Mutex::new(Vec::new()));

/// `(host, mark)` is what makes two placements different: one host can hold two sessions at once, and
/// a sweep keyed on the host alone would end the other one's work.
fn placement_key(ssh: &Ssh) -> (&str, Option<&str>) {
    (&ssh.host.name, ssh.mark.as_deref())
}

/// Take a placement onto the list the last act reads, with the fetch that brings its branch home.
fn remember_placement(ssh: &Ssh, home: Option<Home>) {
    let key = placement_key(ssh);
    let mut all = PLACEMENTS.lock().unwrap_or_else(|e| e.into_inner());
    if !all.iter().any(|p| placement_key(&p.ssh) == key) {
        all.push(Placement {
            ssh: ssh.clone(),
            home,
        });
    }
}

/// A closed session's placement is not the last act's to sweep again - and its sweep already ran.
fn forget_placement(ssh: &Ssh) {
    let key = placement_key(ssh);
    PLACEMENTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|p| placement_key(&p.ssh) != key);
}

/// End what every placement this process made and no session closed left on its host: the processes
/// it started, and the branch it committed. All at once and bounded by [`END_SESSION_BOUND`]: the
/// daemon is on its way out, and a host that has stopped answering must not be able to hold the exit
/// open. The branch is fetched here and not only at a close, because `--stop` reaches no close at all
/// - which is why a stopped package's commits never came home.
///
/// A sweep that got no slot in the host's queue never ran - the stop reaches this before it
/// interrupts the session's own calls, so a full queue is the ordinary case - and it goes back on
/// the list rather than with it: the next door to ask is the only chance that work has. Nothing is
/// fetched for it, because a process still writing in that checkout may be there when the fetch
/// reads it.
pub fn placements_down() -> Vec<String> {
    let all: Vec<Placement> =
        std::mem::take(&mut *PLACEMENTS.lock().unwrap_or_else(|e| e.into_inner()));
    let running: Vec<_> = all
        .into_iter()
        .map(|p| {
            std::thread::spawn(move || {
                let swept = p.ssh.sweep();
                (p, swept)
            })
        })
        .collect();
    let mut said = Vec::new();
    for t in running {
        // A thread that panicked has said nothing, and nothing can be said for it.
        let Ok((placement, swept)) = t.join() else {
            continue;
        };
        said.extend(swept.said);
        if !swept.asked {
            remember_placement(&placement.ssh, placement.home);
            continue;
        }
        if let Some(home) = &placement.home {
            let opening = format!("the session on {} ended", placement.ssh.host.name);
            said.push(home.fetch(&placement.ssh, &opening));
        }
    }
    said
}

/// What one sweep came to: the line it has to say, and whether the call was made at all.
///
/// The second half is what keeps a sweep alive: no slot in the connection queue means nothing was
/// sent, so the placement is still owed a sweep and [`placements_down`] puts it back.
struct Swept {
    said: Option<String>,
    asked: bool,
}

impl Swept {
    /// The call was made, so this placement owes nothing more: the line may still be a refusal.
    fn asked(said: Option<String>) -> Swept {
        Swept { said, asked: true }
    }
}

/// How long one guest's `down` gets when the daemon is on its way out: twice what a manual one
/// costs, and not a budget.
const VM_DOWN_AT_EXIT: Duration = Duration::from_secs(30);

/// How long the copy-out is waited for after that `down` at the daemon's last act, per guest.
const VM_COPY_AT_EXIT: Duration = Duration::from_secs(10);

/// Take a booted guest onto the list the last act reads.
fn remember_guest(plain: &Ssh, vm: &RemoteVm) {
    let mut guests = GUESTS.lock().unwrap_or_else(|e| e.into_inner());
    if !guests
        .iter()
        .any(|(s, g)| s.host.name == plain.host.name && g.project == vm.project)
    {
        guests.push((plain.clone(), vm.clone()));
    }
}

/// A guest the session's own close has brought down is not the last act's to bring down again.
fn forget_guest(host: &str, vm: &RemoteVm) {
    GUESTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|(s, g)| !(s.host.name == host && g.project == vm.project));
}

/// Bring down every guest this process booted and no session closed, and say where each one's copy
/// came out. A forced exit closes no backend at all, so without this a VM outlives the daemon with
/// whatever calls were stuck in it. All at once, and bounded.
pub fn guests_down() -> Vec<String> {
    let guests = std::mem::take(&mut *GUESTS.lock().unwrap_or_else(|e| e.into_inner()));
    let running: Vec<_> = guests
        .into_iter()
        .map(|(plain, vm)| std::thread::spawn(move || down_at_the_last_act(&plain, &vm)))
        .collect();
    running
        .into_iter()
        .map(|t| {
            t.join().unwrap_or_else(|_| {
                "a guest's last act panicked, so it may still be up".to_string()
            })
        })
        .collect()
}

/// One guest's `down` and copy-out, in one call so the two cannot report different moments: the
/// siblings that existed BEFORE it are what tell a fresh copy from an earlier run's.
fn down_at_the_last_act(plain: &Ssh, vm: &RemoteVm) -> String {
    let host = plain.host.name.clone();
    let script = format!(
        "p={p}\n\
         before=$(for d in \"$p\"-*; do [ -d \"$d\" ] && printf '%s\\n' \"$d\"; done)\n\
         said=$({fc} down --project \"$p\" 2>&1); rc=$?\n\
         printf 'SAID %s\\n' \"$said\"\n\
         i=0\n\
         while [ $i -lt {n} ]; do\n\
         \x20 for d in \"$p\"-*; do\n\
         \x20   [ -d \"$d\" ] || continue\n\
         \x20   case \"$before\" in *\"$d\"*) continue ;; esac\n\
         \x20   printf 'COPY %s\\n' \"$d\"; exit $rc\n\
         \x20 done\n\
         \x20 i=$((i+1)); sleep 1\n\
         done\n\
         exit $rc\n",
        p = quote(&vm.project),
        fc = quote(&vm.firecode),
        n = VM_COPY_AT_EXIT.as_secs()
    );
    let within = VM_DOWN_AT_EXIT + VM_COPY_AT_EXIT;
    match plain.sh_once_within("bringing the firecode VM down", &script, within) {
        Ok((out, _, exit)) => guest_line(&host, &vm.project, &out, exit),
        Err(e) => format!("the VM on {host} was NOT brought down: {e}"),
    }
}

/// What the last act says about one guest, in one line: whether it came down and where its tree
/// was left. A `down` that failed leaves the VM up, and that is said rather than smoothed over.
fn guest_line(host: &str, project: &str, out: &str, exit: i32) -> String {
    let copy = out.lines().find_map(|l| l.strip_prefix("COPY "));
    let said = out
        .lines()
        .find_map(|l| l.strip_prefix("SAID "))
        .unwrap_or("")
        .trim();
    let said = if said.is_empty() {
        String::new()
    } else {
        format!(": {said}")
    };
    match (exit, copy) {
        (0, Some(copy)) => {
            format!("the VM on {host} was brought down; the guest's tree was copied out to {copy}")
        }
        (0, None) => format!(
            "the VM on {host} was brought down; nothing was copied out: the guest changed \
             nothing, or firecode is still delivering it as {project}-<run id>"
        ),
        (code, _) => format!(
            "the VM on {host} was NOT brought down (firecode down exited {code}{said}), and it may \
             still be up on that machine"
        ),
    }
}

/// One page of reads, with the paths this box can reach over the wire and an answer for the rest. A
/// path outside the view is that path's answer and never the page's: an `Err` for the page means the
/// channel failed and says nothing about any file, which sends every path in it back to one round
/// trip of its own.
fn batched_page(
    paths: &[&str],
    in_view: impl Fn(&str) -> Result<String, BackendError>,
    fetch: impl FnOnce(&[&str]) -> Result<Vec<Result<Vec<u8>, BackendError>>, BackendError>,
) -> Result<Vec<Result<Vec<u8>, BackendError>>, BackendError> {
    let mut asked: Vec<String> = Vec::with_capacity(paths.len());
    let mut outside: Vec<Option<BackendError>> = Vec::with_capacity(paths.len());
    for path in paths {
        match in_view(path) {
            Ok(abs) => {
                asked.push(abs);
                outside.push(None);
            }
            Err(e) => outside.push(Some(e)),
        }
    }
    let mut read = if asked.is_empty() {
        Vec::new().into_iter()
    } else {
        let refs: Vec<&str> = asked.iter().map(|s| s.as_str()).collect();
        fetch(&refs)?.into_iter()
    };
    Ok(outside
        .into_iter()
        .map(|refused| match refused {
            Some(e) => Err(e),
            // The parser answers one slot per path it was given, or refuses the page.
            None => read.next().unwrap_or_else(|| {
                Err(BackendError::Io(format!(
                    "reading {} file(s): the answer came back short",
                    paths.len()
                )))
            }),
        })
        .collect())
}

impl ExecBackend for SshBackend {
    fn boundary(&self) -> Option<crate::exec::Boundary> {
        self.confinement()
    }

    fn boundary_refusal(&self) -> Option<String> {
        self.unconfined.clone()
    }
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
        // The command's remote process group is recorded as it starts, in this session's own
        // directory there, so a command that has to be abandoned can be ended there. The name
        // carries this process's id and a counter, so two runs cannot overwrite each other's.
        let pgid = format!(
            "{}/.letibot-remote/run-{}-{}.pgid",
            self.worktree,
            std::process::id(),
            RUNS.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        // `run` is the one call that keeps the tool's deadline and nothing else: the command may be
        // a build, and a command that loses its connection is not a failure, since it may be running
        // still - so it is reported as an unknown outcome and never re-sent.
        let script = match &self.confine {
            Some(_) => self
                .confined_script(&cwd, &cmd.env, &cmd.argv)
                .map_err(|e| BackendError::Io(e.to_string()))?,
            None => format!("cd {} && {env_prefix}{joined}", quote(&cwd)),
        };
        let (out, _stderr, exit) = self.ssh.sh_run("this command ran", &script, Some(&pgid))?;
        Ok(Output {
            stdout: out.into_bytes(),
            // Merged by ssh itself: see `Ssh::remote_command`.
            stderr: Vec::new(),
            exit,
        })
    }

    fn read(&self, path: &str) -> Result<Vec<u8>, BackendError> {
        self.ssh.read_abs(&self.in_view(path)?)
    }

    /// One round trip for a page of files, not one per file: a placed session's note corpus is
    /// twelve reads, and at a call each that is the verb's whole cost. The stream is the VM
    /// backend's own - each file behind a marker base64 cannot make - so a short answer is a
    /// failure for the CALL and the caller falls back to [`Self::read`].
    fn read_many(
        &self,
        paths: &[&str],
    ) -> Result<Vec<Result<Vec<u8>, BackendError>>, BackendError> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        batched_page(
            paths,
            |p| self.in_view(p),
            |asked| {
                let (out, _, exit) = self.ssh.sh_step(
                    &format!("reading {} file(s)", asked.len()),
                    &crate::firecode::batched_read_command(asked),
                )?;
                if exit != 0 {
                    return Err(BackendError::Io(format!(
                        "reading {} file(s) on {}: exit {exit}",
                        asked.len(),
                        self.host
                    )));
                }
                crate::firecode::parse_batched_reads(out.as_bytes(), asked)
            },
        )
    }

    fn write(&self, path: &str, bytes: &[u8]) -> Result<(), BackendError> {
        if !self.writable {
            return Err(BackendError::Unsupported(
                "this subagent was downgraded to no write; nothing on its host changed",
            ));
        }
        let abs = self.in_view(path)?;
        // One road for every write: `Ssh::write_abs` decides between the host's stdin and the
        // guest's command from the placement itself, so a caller cannot pick the wrong one.
        self.ssh.write_abs(&abs, bytes)
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

    /// What this backend will call the root of the boundary: the checkout, when a boundary was
    /// measured, and nothing when it was not. [`Self::in_view`] is the narrower truth, since the
    /// view holds the system roots and the toolchain read-only too. Unconfined it is `None`.
    fn root_path(&self) -> Option<String> {
        if self.vm.is_some() {
            return Some(self.worktree.clone());
        }
        self.confine.as_ref().map(|_| self.worktree.clone())
    }

    fn workspace_path(&self) -> Option<String> {
        Some(self.worktree.clone())
    }

    /// The machine the tools run on, for the gate's own sentence: a card that said only
    /// `/home/dead/letibot-remote/letibot/1a2b3c4d` would name a path nobody could tell apart from
    /// one on this box.
    fn host_label(&self) -> Option<String> {
        Some(match self.vm {
            Some(_) => format!("{} in a firecode VM", self.host),
            None => format!("{} over ssh", self.host),
        })
    }

    /// Where a tool's `cwd` starts: the checkout, not this machine's root.
    fn workdir(&self, cwd: &str) -> Result<String, BackendError> {
        self.in_view(cwd)
    }

    fn list(&self, path: &str) -> Result<Vec<DirEntry>, BackendError> {
        let q = format!("'{}'", self.in_view(path)?.replace('\'', r#"'\\''"#));
        let (out, _, exit) = self.ssh.sh_step(
            &format!("listing {path}"),
            &format!(
                "if [ ! -e {q} ]; then exit 22; fi\n\
                 if [ ! -d {q} ]; then exit 23; fi\n\
                 for f in {q}/* {q}/.[!.]*; do\n\
                   [ -e \"$f\" ] || [ -L \"$f\" ] || continue\n\
                   m=$(stat -c %Y \"$f\" 2>/dev/null || stat -f %m \"$f\" 2>/dev/null)\n\
                   if [ -d \"$f\" ]; then printf 'd\\t0\\t%s\\t%s\\n' \"$m\" \"$f\"; \
                   else printf 'f\\t%s\\t%s\\t%s\\n' \"$(wc -c < \"$f\")\" \"$m\" \"$f\"; fi\n\
                 done"
            ),
        )?;
        match exit {
            0 => {}
            22 => return Err(BackendError::NotFound(path.to_string())),
            23 => return Err(BackendError::NotADirectory(path.to_string())),
            _ => return Err(BackendError::Io(out)),
        }
        let mut entries = Vec::new();
        for line in out.lines() {
            // The time is the third field and the path the fourth: a path may hold a tab, and
            // nothing that follows one is a field.
            let mut parts = line.splitn(4, '\t');
            let (Some(kind), Some(size), Some(modified), Some(p)) =
                (parts.next(), parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            entries.push(DirEntry {
                path: p.to_string(),
                name: p.rsplit('/').next().unwrap_or(p).to_string(),
                is_dir: kind == "d",
                bytes: size.trim().parse().unwrap_or(0),
                modified: modified.trim().parse().ok(),
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
        let abs = self.in_view(root)?;
        let q = quote(&abs);
        let (_, _, gate) = self.ssh.sh_step(
            &format!("looking at {root}"),
            &format!(
                "if [ ! -e {q} ]; then exit 22; fi; if [ ! -d {q} ]; then exit 23; fi; exit 0"
            ),
        )?;
        match gate {
            0 => {}
            22 => return Err(BackendError::NotFound(root.to_string())),
            23 => return Err(BackendError::NotADirectory(root.to_string())),
            _ => {}
        }
        let (listing, _, _) = self.ssh.sh_step(
            &format!("walking {root}"),
            &format!("{} walk {q}", TREE_WALK),
        )?;
        Ok(crate::firecode::tree_in_walk_order(
            listing.as_bytes(),
            &abs,
            limit,
            skip,
        ))
    }

    fn stat(&self, path: &str) -> Option<DirEntry> {
        let abs = self.in_view(path).ok()?;
        let q = quote(&abs);
        let (out, _, exit) = self
            .ssh
            .sh_step(
                &format!("looking at {path}"),
                &format!(
                    "if [ -e {q} ] || [ -L {q} ]; then \
                       m=$(stat -c %Y {q} 2>/dev/null || stat -f %m {q} 2>/dev/null); \
                       if [ -d {q} ]; then printf 'd\\t0\\t%s\\n' \"$m\"; \
                       else printf 'f\\t%s\\t%s\\n' \"$(wc -c < {q})\" \"$m\"; fi; \
                     else exit 1; fi"
                ),
            )
            .ok()?;
        if exit != 0 {
            return None;
        }
        // The newline only: a `trim` would eat the empty time field and make an existing file
        // read as absent.
        let mut parts = out.trim_end_matches(['\n', '\r']).splitn(3, '\t');
        let (kind, size, modified) = (parts.next()?, parts.next()?, parts.next()?);
        Some(DirEntry {
            path: abs.clone(),
            name: abs.rsplit('/').next().unwrap_or(&abs).to_string(),
            is_dir: kind == "d",
            bytes: size.trim().parse().unwrap_or(0),
            modified: modified.trim().parse().ok(),
        })
    }

    fn describe(&self) -> String {
        // What the boundary IS, or why there is none, and never a fixed sentence: a banner that
        // asserts what it did not measure is the shape `Boundary::describe`'s own doc warns about.
        // `bwrap` was measured on the host, so the first arm prints the view that runs.
        let boundary = match (&self.confine, &self.unconfined) {
            // A VM is a boundary by construction rather than by measurement, so the sentence
            // says which it is instead of borrowing `bwrap`'s.
            _ if self.vm.is_some() => format!(
                "CONFINED by a firecode VM booted on that machine, holding a copy of {} at the \
                 same path: no host filesystem, no host process, no host device. What the session \
                 changes is collected from the guest when the VM comes down, and that is what \
                 comes home.",
                self.vm.as_ref().map(|v| v.project.as_str()).unwrap_or("")
            ),
            (Some(b), _) => format!(
                "CONFINED, and measured on that machine — {}. Every command and every job runs \
                 inside it: the checkout is writable, the toolchain is read-only, the rest of \
                 that machine is ABSENT from the view rather than denied, and a process it \
                 starts cannot see this box's daemon or the other sessions on that host.",
                crate::exec::Confinement::describe(b)
            ),
            (None, Some(why)) => format!(
                "NOT CONFINED — {why}. So it is your machine with your files, the gate is this \
                 box's own gate, and every command it runs is a decision rather than a bounded one."
            ),
            (None, None) => "NOT CONFINED: no boundary was measured for this host.".to_string(),
        };
        format!(
            "ssh host `{}` ({}): every tool call runs there — its files, its commands, its \
             processes — while this session's model, store and gate stay on this machine. {}\n\
             The tree is a checkout of {} at {}, on branch `{}`; what it commits comes back as \
             `{}` when the session ends, and uncommitted edits stay there. A command's stderr \
             arrives merged into its output (ssh carries one channel). {}",
            self.host,
            self.ssh.destination(),
            boundary,
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
        Some(self.settle())
    }
}

/// A depth-first walk, printed as `<d|f>\t<size>\t<path>`, the shape firecode's `find -printf`
/// produces. A shell function, because `-printf` is GNU's. The definition only, never a call: a
/// constant ending in a bare `walk` would run it with no argument, over the whole filesystem.
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

/// The process host of a remote session: its jobs live in files on that machine, under
/// `<worktree>/.letibot-remote/jobs/` - a launcher, a pid, the merged output, and an exit status
/// whose existence is the settlement. A foreground `bash` call is started the same way.
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
    /// The boundary this session's jobs start inside: the same one its commands use.
    confine: Option<crate::exec::Bwrap>,
    /// The toolchain environment a confined job carries. See [`toolchain_env`].
    confined_env: Vec<(String, String)>,
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
    /// Who is expected to end it, when somebody besides the command itself is: the deadline. Read
    /// when the pid goes away with no status file, so the settlement says *its deadline passed*
    /// rather than *something on that machine ended it*.
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
            confine: None,
            confined_env: Vec::new(),
        }
    }

    /// Put every job of this session inside the session's own boundary, and give it the same
    /// environment a command of the model's gets. A job and a foreground command go through
    /// [`wrap_confined`] alike, so a job cannot be the door that runs unconfined.
    pub fn confined(
        mut self,
        confine: Option<crate::exec::Bwrap>,
        env: Vec<(String, String)>,
    ) -> RemoteProcesses {
        self.confine = confine;
        self.confined_env = env;
        self
    }

    /// [`wrap_confined`] with this session's own environment folded in.
    fn confined_line(
        &self,
        cwd: &str,
        env: &[(String, String)],
        argv: &[String],
    ) -> Result<String, ExecError> {
        let mut pairs = self.confined_env.clone();
        pairs.extend(env.iter().cloned());
        match &self.confine {
            Some(c) => wrap_confined(c, cwd, &pairs, argv),
            None => Ok(String::new()),
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

    /// Ask the files what happened, and record it. One round trip per job: the exit status if there is
    /// one, whether the pid is still alive, and how much output there is.
    fn refresh(&self, job: &RemoteJob) {
        self.refresh_within(job, self.ssh.poll_bound());
    }

    /// [`RemoteProcesses::refresh`] with the caller's remaining time, so a `job_wait` honours its own
    /// deadline even when the host has stopped answering.
    fn refresh_within(&self, job: &RemoteJob, within: Duration) {
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
        // A poll, so [`POLL_BOUND`] and one attempt: a status question whose answer arrives after the
        // next tick has already been superseded, and asking it twice holds the caller's deadline open
        // for twice as long as it should.
        let Ok((line, _, 0)) = self
            .ssh
            .sh_poll_within("asking about a job", &script, within)
        else {
            return;
        };
        // `trim_end`, not `trim`: the fields are tab-separated and the first is EMPTY until the job
        // writes its exit status, so `trim()` ate that leading tab and every running job read as
        // "no pid, no output, no exit".
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
            // A second look at the status file, before this is called a kill: the poll reads the
            // exit file first and the pid second, so a job that finished in between answers "no
            // status, pid gone" like a killed one. The launcher writes the status before it exits.
            let again = self.ssh.sh_step_within(
                "reading the exit status one more time",
                &format!("cat {} 2>/dev/null || true", quote(&job.exit)),
                self.ssh.poll_bound(),
            );
            let Ok((said, _, _)) = again else {
                // A look that did not answer is not evidence either way; the next tick asks
                // again. Settling here would be inventing a kill out of a dropped connection.
                return;
            };
            let again = said.trim();
            if !again.is_empty() {
                job.settle(JobState::Exited {
                    code: again.parse().unwrap_or(-1),
                });
                return;
            }
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
            // The output of a job here is already a file, and it is on the other machine.
            // [`crate::exec::OutputFile`] is a path a reader on THIS box can open, so a remote
            // job's path here would be a locator that fails the one call it suggests.
            output_file: None,
        }
    }

    fn scope_id(&self, kind: ScopeKind, name: &str) -> ScopeId {
        ScopeId {
            kind,
            name: name.to_string(),
            // Not a cgroup, and spelled so it cannot be mistaken for one. Nothing on this machine owns
            // these processes; the remote's own session does.
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

    /// Start a job under `setsid`, so it survives this ssh call and this Mac. The launcher runs in
    /// the background with all three streams on `/dev/null`: ssh stays connected until every stream
    /// the command inherited is closed, so a job that kept stdout would hold the launch call open.
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
        // A SUBSHELL, not a brace group: `{ ...; }` runs in the launcher's own shell, so a command
        // containing `exit` ended the launcher before it wrote the status. `( ... )` is a child
        // shell, so its `exit` is the status and not the end.
        let command = match &self.confine {
            Some(_) => self
                .confined_line(
                    &cwd,
                    &req.env,
                    &["/bin/sh".into(), "-c".into(), req.command.clone()],
                )
                .map_err(|e| {
                    ExecError::Spawn(format!(
                        "this job cannot run inside the boundary on {}: {e}",
                        self.host
                    ))
                })?,
            None => req.command.clone(),
        };
        launcher.push_str(&format!(
            "( {} ) > {} 2>&1\necho $? > {}\n",
            command,
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

        // `setsid` where the host has it: the job leads its own process group, so `job_kill` reaches
        // everything it forked. Without it the job still outlives the connection; only the group kill
        // is weaker, and [`RemoteProcesses::kill`] checks which of the two it has.
        let launch = format!(
            "cd {jobs} && rm -f {exit} && \
             if command -v setsid >/dev/null 2>&1; then setsid bash {s} </dev/null >/dev/null 2>&1 & \
             else nohup bash {s} </dev/null >/dev/null 2>&1 & fi; echo started",
            jobs = quote(&jobs),
            exit = quote(&exit),
            s = quote(&script_path),
        );
        self.ssh
            .sh_step(
                &format!("making {jobs}"),
                &format!("mkdir -p {}", quote(&jobs)),
            )
            .map_err(|e| ExecError::Spawn(format!("making {} on {}: {e}", jobs, self.host)))?;
        // A job's start is not idempotent, so it is asked exactly once. The launcher has already been
        // written; a start that loses its connection may have started the job, and running it again
        // would be a second copy of the operator's work.
        let (said, stderr, code) = self
            .ssh
            .sh_run("the job was starting", &launch, None)
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
        // The pid is the evidence it started, polled rather than assumed: a launcher that could not
        // `cd` writes its pid and then exits 97, so no pid after two seconds is a job that never
        // began, which is better said here than by a `job_output` reading an empty file for ever.
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
            // What the launcher said, when it said anything: a job that never wrote its pid is either a
            // launcher that could not run or one that died between the two writes, and the difference is
            // in this file.
            let said = self
                .ssh
                .sh_step(
                    "reading what the launcher wrote",
                    &format!(
                        "echo '--- the launcher:'; cat {s} 2>/dev/null | head -c 400; \
                         echo '--- what it wrote:'; cat {o} 2>/dev/null | tail -c 400",
                        s = quote(&script_path),
                        o = quote(&out)
                    ),
                )
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

    /// End one job by killing its process group on the remote, and record what was there before and
    /// after: the same falsifier the local reaper keeps.
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
        // What is there before the kill, so a zero is falsifiable.
        if let Ok((said, _, 0)) = self.ssh.sh_step(
            &format!("looking at pid {pid} before the kill"),
            &format!("ps -p {pid} -o pid=,comm= 2>/dev/null || true"),
        ) && let Some(line) = said.lines().find(|l| !l.trim().is_empty())
        {
            let mut it = line.split_whitespace();
            let p = it.next().and_then(|p| p.parse().ok()).unwrap_or(pid);
            reaping.observed.push(Reaped {
                pid: p,
                comm: it.next().unwrap_or("(unnamed)").to_string(),
                cmdline: job.command.clone(),
            });
        }
        // A group kill only when the pid really leads a group. `kill -TERM -<pid>` on a pid that
        // is not a group leader signals this shell's group, which could take out something the
        // operator cares about. `ps -o pgid=` answers it on the remote before the signal.
        let pgid = self
            .ssh
            .sh_step(
                &format!("asking whether pid {pid} leads a group"),
                &format!("ps -p {pid} -o pgid= 2>/dev/null | tr -d ' '"),
            )
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
        // Who is ending it, recorded before the signal: the settle loop below takes a poll while
        // the signal lands, and without this a `job_kill` this daemon sent is recorded as
        // *something on the remote ended it*.
        *job.armed_kill.lock().unwrap_or_else(|e| e.into_inner()) = Some(by.to_string());
        // A second `kill -TERM` after a timeout could reach a pid the first one ended and
        // something else reused. Sent once; a failure is reported by the read-back below rather
        // than papered over.
        let _ = self.ssh.sh_once(
            &format!("ending pid {target}"),
            &format!("kill -TERM {target} 2>/dev/null || true"),
        );
        let settle_by = Duration::from_secs(2);
        let settle_started = Instant::now();
        while settle_started.elapsed() < settle_by {
            let left = settle_by.saturating_sub(settle_started.elapsed());
            self.refresh_within(job, left.min(self.ssh.poll_bound()));
            if !job.is_running() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if job.is_running() {
            let _ = self.ssh.sh_once(
                &format!("killing pid {target} hard"),
                &format!("kill -KILL {target} 2>/dev/null || true"),
            );
            reaping.note = Some(format!("{} ignored SIGTERM and was killed hard", job.id.0));
        }
        reaping.waited = started.elapsed();
        job.settle(JobState::Killed { by: by.into() });
        // Who is left, read back the way it was read before. When the read-back does not come,
        // that is said rather than counted as a clean kill: `removed` means *this daemon looked
        // and found nothing*, and a host that never answered has not been looked at.
        match self.ssh.sh_step(
            &format!("reading back whether pid {pid} is gone"),
            &format!("kill -0 {pid} 2>/dev/null && echo alive"),
        ) {
            Ok((said, _, _)) => {
                // The script's own protocol: it says `alive` when `kill -0` succeeded, and exits
                // non-zero when it did not, so an exit status here is *still an answer*.
                if said.contains("alive") {
                    reaping.survivors.push(pid);
                }
                reaping.removed = reaping.survivors.is_empty();
            }
            Err(e) => {
                // The job was told to stop and nothing on that machine confirmed it. Reported as
                // unknown, so nothing downstream reads this as a kill that completed.
                reaping.removed = false;
                reaping.note = Some(format!(
                    "{e} — so whether the process on {} is gone is UNKNOWN: nothing was read \
                     back",
                    self.host
                ));
            }
        }
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
        let boundary = match &self.confine {
            Some(b) => format!(
                "CONFINED, and measured on that machine: {}.",
                crate::exec::Confinement::describe(b)
            ),
            None => "NOT CONFINED: no boundary was measured for this host.".to_string(),
        };
        format!(
            "processes on ssh host `{}`: a job is started there under setsid, with its output \
             in `{}`, so it outlives the ssh call that started it and this machine going to \
             sleep. {} There is no cgroup on this machine — the remote's own process tree owns \
             it, and job_kill signals that group over ssh, which is why the sandbox does not take \
             a session of its own.",
            self.host, self.jobs_dir, boundary
        )
    }

    fn spawn(&self, req: &SpawnRequest) -> Result<JobId, ExecError> {
        self.launch(req)
    }

    /// The deadline is armed on the machine that runs the command, not on this one: a thread here
    /// dies with this daemon and the remote process does not, so a detached `sleep ... kill` is
    /// started over the same connection and [`RemoteProcesses::refresh`] reads its effect.
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
        // The group only when the pid really leads one: `kill -TERM -<pid>` on a pid that does not
        // signals this shell's group instead. Same test `kill` makes.
        let script = format!(
            "sleep {secs}\n\
             g=$(ps -p {pid} -o pgid= 2>/dev/null | tr -d ' ')\n\
             if [ \"$g\" = \"{pid}\" ]; then kill -TERM -{pid}; else kill -TERM {pid}; fi\n\
             sleep 2\n\
             if [ \"$g\" = \"{pid}\" ]; then kill -KILL -{pid} 2>/dev/null; \
             else kill -KILL {pid} 2>/dev/null; fi\n"
        );
        let _ = self.ssh.sh_once(
            "arming the job's deadline",
            &format!(
                "nohup bash -c {} </dev/null >/dev/null 2>&1 & echo armed",
                quote(&script)
            ),
        );
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
            let left = deadline.saturating_sub(started.elapsed());
            if left.is_zero() {
                return Ok(Waited::Deadline {
                    took: started.elapsed(),
                    produced: job.produced(),
                    since_last_output: None,
                });
            }
            self.refresh_within(&job, left.min(self.ssh.poll_bound()));
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
            let left = deadline.saturating_sub(started.elapsed());
            if left.is_zero() {
                return Ok(Waited::Deadline {
                    took: started.elapsed(),
                    produced: mine.iter().map(|j| j.produced()).sum(),
                    since_last_output: None,
                });
            }
            for j in &mine {
                self.refresh_within(j, left.min(self.ssh.poll_bound()));
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

    /// Where the whole of a job's output is on that machine: the job directory's `<n>.out`, which is
    /// the path the supervisor reads. This box cannot open it, which is why it is not
    /// [`JobView::output_file`].
    fn job_log(&self, job: &JobId) -> Option<String> {
        self.find(job).map(|j| j.out.clone())
    }

    /// The output is a file on the remote, so a read is `tail -c` from where the caller stopped: one
    /// round trip, and the bytes it asks for rather than the whole file every time.
    fn output(&self, job: &JobId, from: u64, limit: usize) -> Result<OutputSlice, ExecError> {
        let job = self
            .find(job)
            .ok_or_else(|| ExecError::NoSuchJob(job.0.clone()))?;
        self.refresh(&job);
        let (said, _, code) = self
            .ssh
            .sh_step(
                &format!("reading the output of a job on {}", self.host),
                &format!(
                    "tail -c +{} {} 2>/dev/null | base64 | tr -d '\\n'",
                    from + 1,
                    quote(&job.out)
                ),
            )
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
        // Nothing is dropped: the file on that machine keeps everything, so a reader that stops
        // early can ask again from where it stopped and get the bytes it missed. `produced` is the
        // file's size, and `to` is where this slice ends in it.
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

    /// Nothing is moved, because nothing needed moving: a job here is started detached from the
    /// ssh call that made it, and [`RemoteProcesses::launch`] redirects it into `{jobs}/{n}.out`
    /// before it starts, so nothing it prints is lost. The record says so.
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
                "it was started detached on {}, so it already outlives this turn: nothing \
                 moved. Its output is already a file there — `{}` — so nothing it prints is \
                 lost while it runs",
                self.host, j.out
            )),
            // See `RemoteProcesses::view`: this field is a path on THIS box, and the file is on
            // the host. The note above is where a reader is told where it is.
            output_file: None,
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

    /// Empty, and it is a fact rather than a gap: every pid this harness manages is here, and a
    /// command on the remote cannot signal it, since no process namespace is shared with that
    /// machine.
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

    /// The checkout, on the remote's filesystem. Nothing local joins a path to it: `monitors` is
    /// `None` here, and it is the one caller that would.
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
path     = ["~/.cargo/bin", "/opt/bin"]
root     = "~/letibot-remote"

[host."bench-2"]
hostname = "10.0.0.7"
user     = "root"
"#;

    /// A test's own home and config directory, with a key for `host` and its host key already
    /// accepted - the two facts [`Ssh::connect`] refuses without.
    fn paths_for(tag: &str, host: &str, hostname: &str) -> (PathBuf, Paths) {
        let dir = std::env::temp_dir().join(format!(
            "letibot-policy-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let paths = Paths::in_home(dir.join("home"));
        std::fs::create_dir_all(paths.keys()).expect("the keys directory");
        std::fs::write(paths.default_key(host), b"a key").expect("the dedicated key");
        std::fs::write(
            paths.known_hosts(),
            format!("{hostname} ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI\n"),
        )
        .expect("a known host");
        (dir, paths)
    }

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
        assert_eq!(
            lab.path,
            vec!["~/.cargo/bin".to_string(), "/opt/bin".to_string()],
            "a host's `path` list is read, and left as the REMOTE spells it"
        );
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

    /// A `~` in the wrapped directory is expanded on the REMOTE and never left quoted: `mise exec
    /// -C` resolves a literal `~` against the directory it is in, so a quoted tilde names the wrong
    /// tree. The same rule `path`'s entries follow ([`one_entry`]), asked of `-C`.
    #[test]
    fn a_tilde_directory_is_expanded_on_the_remote() {
        let h = Hosts::parse(FILE).expect("it parses");
        let host = h.get("lab2x1").expect("lab2x1").clone();
        let (dir, paths) = paths_for("tilde-dir", &host.name, &host.hostname);
        let host = Host {
            key: Some(paths.default_key("lab2x1")),
            mise: Some(Mise::At("/opt/mise".into())),
            ..host
        };
        let ssh = Ssh::connect(host, Some(PathBuf::from("/bin/false")), paths)
            .expect("the connection opens");

        let absolute = ssh
            .clone()
            .with_dir("/home/dead/letibot-remote/repo/01abcdef");
        let said = absolute.remote_command("true");
        assert!(
            said.starts_with(
                "/opt/mise exec -C /home/dead/letibot-remote/repo/01abcdef -- bash -lc "
            ),
            "an absolute directory is handed over as it is: {said}"
        );

        let tilde = ssh.with_dir("~/letibot-remote/repo/01abcdef");
        let said = tilde.remote_command("true");
        assert!(
            said.contains("exec -C \"$HOME/letibot-remote/repo/01abcdef\" --"),
            "a `~` is the REMOTE's home, expressed so its shell expands it: {said}"
        );
        assert!(
            !said.contains("'~") && !said.contains(" -C ~"),
            "and never handed over as a literal tilde: {said}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A refused mux session is read as transport, and nothing else is. Each sentence says the
    /// connection is up and this call got no channel; `Permission denied` and `Could not resolve
    /// hostname` are not transport and must not be retried under "nothing ran".
    #[test]
    fn the_four_ways_a_mux_loses_a_session_are_read_as_transport() {
        for (said, want) in [
            (
                "mux_client_request_session: session request failed",
                MuxRefusal::Session,
            ),
            ("mm_send_fd: sendmsg(1): Message too long", MuxRefusal::Fds),
            ("mm_send_fd: sendmsg(2): Message too long", MuxRefusal::Fds),
            (
                "channel 3: open failed: administratively prohibited: open failed",
                MuxRefusal::Channel,
            ),
            (
                "Control socket connect /tmp/letibot/ssh-lab2x1-7-call.sock: Connection refused",
                MuxRefusal::StaleSocket,
            ),
        ] {
            assert_eq!(mux_refusal(said), Some(want), "as transport: {said}");
            assert!(
                auth_refusal(said).is_none(),
                "and not as an auth refusal: {said}"
            );
        }
        for said in [
            "Permission denied (publickey).",
            "ssh: Could not resolve hostname gone.invalid: Name or service not known",
            "Connection timed out during banner exchange",
            "Host key verification failed.",
            "",
        ] {
            assert_eq!(mux_refusal(said), None, "not a refusal of the mux: {said}");
        }
    }

    /// Every daemon gets its own master, and the socket fits: a name of the host alone would
    /// multiplex every daemon on the box over one connection, dividing `MaxSessions` among them.
    /// Git gets a socket beside it, for an ssh this process never starts and so cannot count.
    #[test]
    fn one_daemon_has_one_master_and_git_has_a_second() {
        let mine = control_path("lab2x1", 4242);
        assert_eq!(mine, control_path("lab2x1", 4242));
        assert_ne!(control_path("lab2x1", 4243), mine);
        assert_ne!(git_control_path("lab2x1", 4242), mine);
        assert_ne!(
            git_control_path("lab2x1", 4243),
            git_control_path("lab2x1", 4242)
        );
        assert_eq!(
            mine,
            control_path("lab2x1", 4242),
            "and the same name again"
        );
        assert!(
            mine.as_os_str().len() <= 100,
            "under the `SUN_LEN` cap: {}",
            mine.display()
        );
        assert!(
            mine.file_name()
                .map(|n| n.to_string_lossy().contains("4242"))
                .unwrap_or(false),
            "the daemon is named in the file: {}",
            mine.display()
        );
    }

    /// One close asks for ONE transfer, so a slot wait, a run and the retry come out of one
    /// `GIT_BOUND`: the arithmetic that stops a refused fetch costing four of them.
    #[test]
    fn one_fetch_spends_one_budget_however_many_attempts() {
        let spent = FetchBudget {
            began: Instant::now() - (GIT_BOUND + Duration::from_secs(1)),
            within: GIT_BOUND,
        };
        assert_eq!(
            spent.left(),
            Duration::ZERO,
            "a spent fetch has nothing left"
        );
        assert_eq!(
            spent.wait(STEP_BOUND),
            Duration::ZERO,
            "and no wait may outlive it"
        );
        let fresh = FetchBudget::new(GIT_BOUND);
        assert_eq!(
            fresh.wait(STEP_BOUND),
            STEP_BOUND,
            "a fresh fetch waits as long as the connection is given"
        );
    }

    /// The refusal that caused a retry is the only clue pointing at the master's `MaxSessions`, so
    /// the report for a retry that found no slot names it. Dropping it would leave `busy`'s words,
    /// which say nothing was sent about a fetch whose first attempt did send and was refused.
    #[test]
    fn a_retry_that_finds_no_slot_still_says_what_was_refused() {
        let said = refused_then_no_slot(
            b"mux_client_request_session: session request failed\nmore words",
            "fake (git) already has 8 ssh calls in flight from this process",
        );
        assert!(
            said.contains("mux_client_request_session: session request failed"),
            "the refusal is quoted: {said}"
        );
        assert!(
            !said.contains("more words"),
            "one line, not the whole stderr: {said}"
        );
        assert!(
            said.contains("earlier attempt"),
            "said of which attempt: {said}"
        );
        assert!(
            said.contains("no slot on the retry") && said.contains("8 ssh calls in flight"),
            "and what the retry got instead: {said}"
        );
    }

    /// The ssh argv is the policy's, and nothing in it names the operator's ssh material. The
    /// assertions about what is missing are the point: a list with `-i` and no `IdentitiesOnly`
    /// looks complete.
    #[test]
    fn the_ssh_argv_is_the_contract_flags_then_dest_then_command() {
        let h = Hosts::parse(FILE).expect("it parses");
        let host = h.get("lab2x1").expect("lab2x1").clone();
        let (dir, paths) = paths_for("argv", &host.name, &host.hostname);
        // The key the FILE names is this fixture's home, not the operator's.
        let declared = paths.default_key("lab2x1");
        let host = Host {
            key: Some(declared.clone()),
            ..host
        };
        let ssh = Ssh::connect(host, Some(PathBuf::from("/bin/false")), paths.clone())
            .expect("the connection opens");
        let argv = ssh.argv("bash -lc 'true'");
        let flags = argv.join(" ");
        for want in [
            "-F /dev/null".to_string(),
            format!("-o UserKnownHostsFile={}", paths.known_hosts().display()),
            "-o GlobalKnownHostsFile=/dev/null".to_string(),
            "-o StrictHostKeyChecking=yes".to_string(),
            "-o BatchMode=yes".to_string(),
            "-o IdentitiesOnly=yes".to_string(),
            "-o IdentityAgent=none".to_string(),
            "-o ControlMaster=auto".to_string(),
            format!("-i {}", declared.display()),
        ] {
            assert!(flags.contains(&want), "`{want}` is missing from: {flags}");
        }
        assert!(
            !flags.contains(&paths.user_ssh().display().to_string()),
            "nothing on the command line names the operator's ssh material: {flags}"
        );
        assert_eq!(argv[argv.len() - 2], "dead@lab2x1");
        assert_eq!(argv[argv.len() - 1], "bash -lc 'true'");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Two refusals, before anything is arranged: a key that is not there (or is inside the
    /// operator's own ssh directory), and a host key nobody has accepted here.
    #[test]
    fn a_host_is_refused_by_name_before_anything_is_placed() {
        let h = Hosts::parse(FILE).expect("it parses");
        let host = h.get("lab2x1").expect("lab2x1").clone();
        let (dir, paths) = paths_for("refuse", &host.name, &host.hostname);

        // A host whose key is one of the operator's own.
        std::fs::create_dir_all(paths.user_ssh()).expect("their directory");
        let theirs = paths.user_ssh().join("id_ed25519");
        std::fs::write(&theirs, b"k").expect("their key");
        let why = Ssh::connect(
            Host {
                key: Some(theirs),
                ..host.clone()
            },
            None,
            paths.clone(),
        )
        .expect_err("a key inside the operator's directory");
        assert!(why.contains("forbidden to touch"), "{why}");

        // A key that is simply not there.
        let why = Ssh::connect(
            Host {
                key: Some(dir.join("missing")),
                ..host.clone()
            },
            None,
            paths.clone(),
        )
        .expect_err("a key with no file");
        assert!(why.contains("no file there"), "{why}");

        // A host key nobody has accepted: the refusal carries the command that adds it.
        std::fs::write(paths.known_hosts(), "").expect("an empty known_hosts");
        // The key left to its default, which `paths_for` made in this test's own home: the fixture's
        // `~/...` key is expanded against the REAL `$HOME` when the file is parsed, so a fixture that
        // relies on it passes only where that key exists.
        let why = Ssh::connect(Host { key: None, ..host }, None, paths.clone())
            .expect_err("an unknown host key");
        assert!(why.contains("ssh-keyscan -t ed25519"), "{why}");
        assert!(why.contains("Nothing was connected"), "{why}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A host's `path` list is in front of every remote command, and a host without one runs exactly
    /// the command it always did.
    #[test]
    fn a_hosts_path_list_is_a_prefix_on_every_remote_command() {
        assert_eq!(path_export(&[]), None);
        let export = path_export(&["~/.cargo/bin".into(), "/opt/x y/bin".into()]).expect("export");
        assert!(export.starts_with("export PATH="), "{export}");
        assert!(
            export.contains("\"$HOME/.cargo/bin\""),
            "a `~` is expanded by the REMOTE shell and never quoted into a literal tilde: {export}"
        );
        assert!(export.contains("'/opt/x y/bin'"), "{export}");
        assert!(export.ends_with(":\"$PATH\""), "{export}");
        assert_eq!(
            host_with_path(vec!["~/.cargo/bin".into()]).with_path("cd x && git status"),
            "export PATH=\"$HOME/.cargo/bin\":\"$PATH\"; cd x && git status"
        );
        assert_eq!(host_with_path(Vec::new()).with_path("true"), "true");
        assert_eq!(
            path_prefix(&["~/.cargo/bin".into()]).as_deref(),
            Some("PATH=\"$HOME/.cargo/bin\":\"$PATH\"")
        );
        // An entry that is not a tilde path is quoted as any word is, and an awkward one cannot
        // escape its quoting.
        assert_eq!(
            path_export(&["/opt/x y".into()]).as_deref(),
            Some("export PATH='/opt/x y':\"$PATH\"")
        );
        assert_eq!(
            path_export(&["~/a b".into()]).as_deref(),
            Some("export PATH=\"$HOME\"/'a b':\"$PATH\"")
        );
    }

    fn host_with_path(path: Vec<String>) -> Host {
        Host {
            name: "lab2x1".into(),
            hostname: "lab2x1".into(),
            user: Some("dead".into()),
            key: None,
            path,
            root: "~/letibot-remote".into(),
            // A fixture's host is not the operator's machine: no probe, nothing to find.
            mise: Some(Mise::Off),
            ..Host::default()
        }
    }

    /// The file's refusals: a key it does not have, a key stated twice, a `path` that is not a list,
    /// and a host that omits `key` getting the dedicated default.
    #[test]
    fn hosts_toml_refuses_what_it_does_not_understand() {
        let why =
            Hosts::parse("[host.a]\nhostname = \"a\"\nkeyy = \"x\"\n").expect_err("an unknown key");
        assert!(why.contains("keyy"), "{why}");
        let why =
            Hosts::parse("[host.a]\nhostname = \"a\"\nhostname = \"b\"\n").expect_err("twice");
        assert!(why.contains("twice"), "{why}");
        let why = Hosts::parse("[host.a]\npath = \"~/.cargo/bin\"\n").expect_err("not a list");
        assert!(why.contains("one line of quoted strings"), "{why}");

        let h = Hosts::parse("[host.a]\nhostname = \"a\"\n").expect("it parses");
        let a = h.get("a").expect("a");
        assert!(
            a.key.is_none(),
            "omitted, so the dedicated default answers it"
        );
        assert!(a.path.is_empty());
        assert_eq!(a.root, "~/letibot-remote");
        // The three keys a shared host needs default to nothing held back, which is the safe
        // direction: a host nobody declared anything for behaves exactly as it did before this
        // package, and only a file that says otherwise changes a teardown's reach.
        assert_eq!(a.reserve, 0);
        assert!(a.flags.is_empty());
        assert!(a.never_touch.is_empty());
    }

    /// `reserve`, `flags` and `never_touch`: what the operator says about sharing a machine, read
    /// strictly like every other key - a number, two lists, and nothing guessed.
    #[test]
    fn hosts_toml_reads_the_three_sharing_keys() {
        let h = Hosts::parse(
            "[host.lab2x1]\nhostname = \"lab2x1\"\nreserve = 4\n\
             flags = [\"heavy\", \"tmpfs\"]\nnever_touch = [\"pgfuzz\", \"etcd\"]\n",
        )
        .expect("it parses");
        let a = h.get("lab2x1").expect("lab2x1");
        assert_eq!(a.reserve, 4);
        assert_eq!(a.flags, vec!["heavy", "tmpfs"]);
        assert_eq!(a.never_touch, vec!["pgfuzz", "etcd"]);
        assert!(a.has_flag("TMPFS"), "a flag is a word a person types");
        // A substring, case-insensitively: the operator declared the workload and not its spelling, and
        // matching less than they meant is the failure this list exists to prevent.
        assert_eq!(a.never_touch_hit("pgfuzz -t 3600"), Some("pgfuzz"));
        assert_eq!(a.never_touch_hit("/usr/bin/ETCD-fuzz"), Some("etcd"));
        assert_eq!(a.never_touch_hit("cargo test --offline"), None);
        // cores minus load minus reserve, and a negative is reported rather than clamped.
        assert_eq!(a.spare_cores(32, "4.00 2.00 1.00"), Some(24.0));
        assert_eq!(a.spare_cores(8, "12.00 3.00 1.00"), Some(-8.0));
        assert_eq!(a.spare_cores(0, "1.00"), None);
        assert_eq!(a.spare_cores(8, ""), None);

        // A `reserve` that is not a number is refused by name rather than read as zero: a
        // scheduler that believes a busy machine is empty is the collision this is against.
        let why = Hosts::parse("[host.a]\nreserve = \"two\"\n").expect_err("not a number");
        assert!(why.contains("whole number of cores"), "{why}");
        let why = Hosts::parse("[host.a]\nflags = \"heavy\"\n").expect_err("not a list");
        assert!(why.contains("one line of quoted strings"), "{why}");
        assert!(why.contains("flags"), "{why}");
        let why = Hosts::parse("[host.a]\nnever_touch = pgfuzz\n").expect_err("not a list either");
        assert!(why.contains("never_touch"), "{why}");
    }

    /// A link into the ssh directory is the ssh directory, after every symlink and for a path that
    /// does not exist yet: the rule is about the bytes, not the spelling.
    #[test]
    fn a_path_that_resolves_into_the_ssh_directory_is_refused_by_name() {
        let dir = std::env::temp_dir().join(format!(
            "letibot-policy-link-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let paths = Paths::in_home(dir.join("home"));
        let store = paths.user_ssh();
        std::fs::create_dir_all(&store).expect("the store");
        std::fs::write(store.join("id_ed25519"), b"k").expect("a key");
        std::fs::create_dir_all(dir.join("workspace")).expect("ws");
        std::os::unix::fs::symlink(&store, dir.join("workspace/keys")).expect("link");

        assert!(inside_user_ssh(
            &dir.join("workspace/keys/id_ed25519"),
            paths.home()
        ));
        assert!(inside_user_ssh(
            &store.join("a-key-not-written-yet"),
            paths.home()
        ));
        assert!(!inside_user_ssh(
            &dir.join("workspace/ordinary.txt"),
            paths.home()
        ));
        let said = dot_ssh_refusal(&dir.join("workspace/keys/id_ed25519"), paths.home());
        assert!(said.contains("forbidden to touch"), "{said}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_walk_prints_what_tree_in_walk_order_parses() {
        // The format is firecode's `find -printf '%y\t%s\t%p'` by another hand, so the one parser
        // serves both substrates, and the walk runs on the far side of an ssh, where GNU's `find`
        // may not be.
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

    /// The fake remote of [`a_poll_that_finds_the_pid_gone_looks_at_the_status_file_again`]: its
    /// own directory, and a job table that answers from two files the test writes.
    struct PollRig {
        dir: PathBuf,
        ssh: PathBuf,
        /// Its answers, in order, one line per call.
        log: PathBuf,
        /// What the job's `.exit` file holds; empty is a job that wrote no status.
        status: PathBuf,
        /// Present once the job's pid is to be reported GONE.
        gone: PathBuf,
        jobs: String,
    }

    impl PollRig {
        fn new(tag: &str, status: &str) -> PollRig {
            let dir = std::env::temp_dir().join(format!(
                "letibot-poll-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ));
            let rig = PollRig {
                ssh: dir.join("ssh"),
                log: dir.join("ssh.log"),
                status: dir.join("status"),
                gone: dir.join("gone"),
                jobs: dir.join("remote").display().to_string(),
                dir,
            };
            std::fs::create_dir_all(&rig.dir).expect("the fixture directory");
            std::fs::write(&rig.status, status).expect("the status file");
            rig
        }

        /// The ssh, answering three commands and running everything else here. The poll is answered
        /// from [`PollRig::gone`], the status from [`PollRig::status`], and the launch is
        /// acknowledged without starting anything: a real launcher would write files of its own.
        fn written(self) -> PollRig {
            let script = format!(
                "#!/bin/bash\n\
                 printf '%s\\n' \"$*\" >> {log}\n\
                 for last; do :; done\n\
                 case \"$last\" in\n\
                 \x20 *'echo started'*) echo started; exit 0;;\n\
                 \x20 *\"lc 'e=;\"*)\n\
                 \x20   if [ -f {gone} ]; then printf '\\t0\\t19\\n'; \\\n\
                 \x20   else printf '\\t4242\\t19\\n'; fi; exit 0;;\n\
                 \x20 *'.exit 2>/dev/null || true'*) cat {status} 2>/dev/null; exit 0;;\n\
                 esac\n\
                 exec bash -lc \"$last\"\n",
                log = quote(&self.log.display().to_string()),
                gone = quote(&self.gone.display().to_string()),
                status = quote(&self.status.display().to_string()),
            );
            crate::testing::write_stub(&self.ssh, &script);
            self
        }

        /// Every ssh call this rig answered, in order.
        fn calls(&self) -> Vec<String> {
            std::fs::read_to_string(&self.log)
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect()
        }

        /// A host wired to this rig, and the job table over it.
        fn host(&self, tag: &str) -> RemoteProcesses {
            let (_, paths) = paths_for(tag, "fake", "fake.invalid");
            let host = Host {
                name: "fake".into(),
                hostname: "fake.invalid".into(),
                user: Some("tester".into()),
                key: None,
                path: Vec::new(),
                root: "~/letibot-remote".into(),
                mise: Some(Mise::Off),
                ..Host::default()
            };
            let ssh =
                Ssh::connect(host, Some(self.ssh.clone()), paths).expect("the connection opens");
            RemoteProcesses::new(ssh, "fake".into(), self.jobs.clone())
        }
    }

    /// A job whose pid is gone, as a view of it: started (so it has a pid), then the pid reported
    /// gone, then asked. The status file is whatever the rig was built with.
    fn started_then_gone(rig: &PollRig, tag: &str) -> JobView {
        let host = rig.host(tag);
        let job = host
            .spawn(&SpawnRequest {
                command: "true".into(),
                slug: None,
                cwd: String::new(),
                scope: ScopeKind::Session,
                scope_name: None,
                background: true,
                env: Vec::new(),
                tty: false,
                memory: None,
            })
            .expect("the job starts");
        // The finish the poll reads as a kill: the status file is written and the pid is gone by
        // the time the next question is asked.
        std::fs::write(&rig.gone, b"").expect("the pid is gone");
        host.job(&job).expect("the job")
    }

    /// A poll that comes back "no status and the pid gone" reads the status file once more. The
    /// launcher writes the status before it exits, so a pid that is gone cannot be about to write
    /// one: the second look is the difference between a finished job and a killed one.
    #[test]
    fn a_poll_that_finds_the_pid_gone_looks_at_the_status_file_again() {
        let exited = PollRig::new("status", "0\n").written();
        let view = started_then_gone(&exited, "poll-status");
        assert_eq!(
            view.state,
            JobState::Exited { code: 0 },
            "a status file that is there is the job's own ending, whatever the pid did: {view:?}"
        );
        assert!(
            exited
                .calls()
                .iter()
                .any(|c| c.contains(".exit 2>/dev/null || true")),
            "the status file was asked for a second time: {:?}",
            exited.calls()
        );
        std::fs::remove_dir_all(&exited.dir).ok();

        // An empty status file is still a killing: the second look must not turn every dead pid
        // into a graceful exit.
        let nothing = PollRig::new("nothing", "").written();
        let view = started_then_gone(&nothing, "poll-nothing");
        assert!(
            matches!(&view.state, JobState::Killed { by } if by.contains("wrote no exit status")),
            "a pid that went away and wrote nothing is still a kill: {view:?}"
        );
        std::fs::remove_dir_all(&nothing.dir).ok();
    }

    #[test]
    fn a_granted_gitconfig_is_the_global_one() {
        let grants = [
            Path::new("/home/dead/.cargo"),
            Path::new("/home/dead/.gitconfig"),
        ];
        let env = crate::exec::seat::env_for_grants(grants.iter().copied());
        assert!(env.contains(&("GIT_CONFIG_GLOBAL".into(), "/home/dead/.gitconfig".into())));
        assert!(env.contains(&("CARGO_HOME".into(), "/home/dead/.cargo".into())));
        let none =
            crate::exec::seat::env_for_grants([Path::new("/home/dead/.cargo")].iter().copied());
        assert!(none.iter().all(|(k, _)| k != "GIT_CONFIG_GLOBAL"));
    }

    /// The git master's bound belongs to the whole fetch and is taken once. Paid per step it is paid
    /// twice - once waiting for a slot, once running - and the daemon closes sessions one at a time,
    /// so every session of a fanout that ends inherits the sum.
    #[test]
    fn a_fetch_that_queued_for_a_slot_runs_on_what_is_left_of_its_bound() {
        let (dir, paths) = paths_for("git-deadline", "git-deadline", "git-deadline.invalid");
        let host = Host {
            name: "git-deadline".into(),
            hostname: "git-deadline.invalid".into(),
            user: Some("tester".into()),
            key: None,
            path: Vec::new(),
            root: "~/letibot-remote".into(),
            mise: Some(Mise::Off),
            ..Host::default()
        };
        let program = dir.join("git");
        crate::testing::write_stub(&program, "#!/bin/sh\n[ $# -eq 0 ] && exit 0\nsleep 30\n");
        let ssh = Ssh::connect(host, Some(program.clone()), paths).expect("the connection opens");

        // Eight fetches ahead of this one, as a fanout of eight closes would be: the slot comes free
        // 800 ms into a 2 s bound, so a fresh bound for the run would take the call to 2.8 s.
        let held: Vec<rpc::Entered> = (0..rpc::MAX_SESSIONS_PER_HOST)
            .map(|_| rpc::enter_git("git-deadline", Duration::from_secs(1)).expect("a free slot"))
            .collect();
        let queue = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(800));
            drop(held);
        });

        let bound = Duration::from_secs(2);
        let began = Instant::now();
        let out = ssh.fetch_within(
            &program,
            &dir,
            "https://example.invalid/repo",
            "main",
            bound,
        );
        let spent = began.elapsed();
        queue.join().expect("the queue thread");
        assert!(
            matches!(out, Ok(FetchEnded::Killed(_))),
            "a git that never answers is killed at the deadline: {out:?}"
        );
        assert!(
            spent < bound + Duration::from_millis(400),
            "one deadline for the whole fetch: the slot wait and the run are both measured against \
             the {} s it was given, and this call took {spent:?}",
            bound.as_secs()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A fetch killed at its deadline says which deadline. The production bound is `GIT_BOUND`, but a
    /// caller may give less, and a fetch with nothing left must not fork a git to kill on its first
    /// poll: both are read off the answer rather than assumed.
    #[test]
    fn a_fetch_that_is_killed_says_how_long_it_was_given() {
        let (dir, paths) = paths_for("git-killed", "git-killed", "git-killed.invalid");
        let host = Host {
            name: "git-killed".into(),
            hostname: "git-killed.invalid".into(),
            user: Some("tester".into()),
            key: None,
            path: Vec::new(),
            root: "~/letibot-remote".into(),
            mise: Some(Mise::Off),
            ..Host::default()
        };
        let program = dir.join("git");
        let ran = dir.join("ran");
        crate::testing::write_stub(
            &program,
            &format!(
                "#!/bin/sh\n[ $# -eq 0 ] && exit 0\ntouch {}\nsleep 30\n",
                ran.display()
            ),
        );
        let ssh = Ssh::connect(host, Some(program.clone()), paths).expect("the connection opens");

        let url = "https://example.invalid/repo";
        let nothing = ssh.fetch_within(&program, &dir, url, "main", Duration::ZERO);
        assert!(
            !ran.exists(),
            "a fetch with no time left in it started a program it could only kill: {nothing:?}"
        );
        assert!(
            matches!(nothing, Ok(FetchEnded::Killed(_))),
            "a fetch that ran nothing says so, and is not read as a transfer that timed out: \
             {nothing:?}"
        );
        let _ = std::fs::remove_file(&ran);

        let bound = Duration::from_millis(200);
        let began = Instant::now();
        let out = ssh.fetch_within(&program, &dir, url, "main", bound);
        let spent = began.elapsed();
        assert!(
            ran.exists(),
            "a fetch with time in it runs the program: {out:?}"
        );
        match out {
            Ok(FetchEnded::Killed(said)) => {
                assert!(
                    said >= bound && said <= spent,
                    "the kill is reported against the {bound:?} this fetch was given, rather than a \
                     bound it was not: it said {said:?} of {spent:?}"
                );
            }
            other => panic!("a git that never answers is killed at its deadline: {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// One path outside the view used to be the whole page's answer, which the trait reads as *the
    /// channel failed and says nothing about any file* - so every path in a page of 64 went back to
    /// one round trip of its own, for a verb's whole budget.
    #[test]
    fn a_page_with_one_path_outside_the_view_still_reads_the_rest() {
        let seen = std::cell::RefCell::new(Vec::new());
        let answers = batched_page(
            &["in/a.md", "/etc/passwd", "in/b.md"],
            |p| {
                if p.starts_with('/') {
                    Err(BackendError::Outside(p.to_string()))
                } else {
                    Ok(format!("/w/{p}"))
                }
            },
            |asked| {
                *seen.borrow_mut() = asked.iter().map(|s| s.to_string()).collect();
                Ok(vec![Ok(b"a".to_vec()), Ok(b"b".to_vec())])
            },
        )
        .expect("a path outside the view is not a failed page");
        assert_eq!(
            seen.borrow().as_slice(),
            ["/w/in/a.md", "/w/in/b.md"],
            "the page went over the wire once, with only the paths that are in the view"
        );
        assert_eq!(answers.len(), 3, "one answer per path asked, in order");
        assert!(matches!(&answers[0], Ok(b) if b == b"a"));
        assert!(
            matches!(&answers[1], Err(BackendError::Outside(p)) if p == "/etc/passwd"),
            "the path outside the view is its own refusal: {:?}",
            answers[1]
        );
        assert!(matches!(&answers[2], Ok(b) if b == b"b"));
    }
}
