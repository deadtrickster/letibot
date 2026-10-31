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
use crate::rpc;

// ---------------------------------------------------------------------------
// hosts.toml
// ---------------------------------------------------------------------------

/// One machine the operator lets a session run on.
///
/// Read from **the operator's own file and never a project's** — the contract's
/// words, and the reason is a permission one: a repository must not be able to name
/// a machine to run on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Host {
    /// The name used everywhere else — `--host lab2x1`, `where: "host:lab2x1"`.
    pub name: String,
    /// What `ssh` connects to. Defaults to the name, which is what an alias in the
    /// ssh client's own config needs and is the common case.
    pub hostname: String,
    pub user: Option<String>,
    /// The identity file, as the file spelled it, with a leading `~` expanded on **this**
    /// machine — it names a file here. `None` means the dedicated
    /// [`Paths::default_key`], and either way [`identity_for`] is what turns it into the
    /// file ssh is given.
    pub key: Option<PathBuf>,
    /// **Directories to put in front of the remote `PATH`**, as the remote shell must spell
    /// them: `~` is expanded *there*, once, and the far side's own `PATH` follows.
    ///
    /// Measured on lab2x1 (2026-10-26): a login shell there has no `~/.cargo/bin` on `PATH`
    /// and no profile adds it, so `cargo` is a command a remote session cannot run without
    /// saying where it is. Per host rather than global, because it is a fact about that
    /// machine.
    pub path: Vec<String>,
    /// Where remote worktrees live, as the *remote* shell writes it. `~` is expanded
    /// there, once, at placement ([`SshBackend::place`]) — so the paths this backend
    /// hands around are absolute and mean the same thing to `git fetch` a minute later.
    pub root: String,
    /// **How the host's toolchains are found** — `None` is *probe and cache*, which is what
    /// every host without a `mise` line gets. See [`Mise`].
    pub mise: Option<Mise>,
    /// **Cores this machine's owner keeps for themselves** — `reserve` in `hosts.toml`.
    ///
    /// The Stroppy session's fleet runner placed work by *cores − load − reserve*, and the third
    /// term is the one a number this build invented would get wrong: lab2x1 has 32 cores and the
    /// operator's own etcd fuzz running on some of them. `reserve` is the operator's statement of
    /// how many are already spoken for, `load` is the machine's answer about right now, and
    /// [`Host::spare_cores`] is the subtraction — reported, never enforced, because a scheduler
    /// this build does not have would be the wrong place to start.
    pub reserve: u32,
    /// **What this machine is** — `flags` in `hosts.toml`, free-form words such as `heavy` (a box
    /// for long builds) or `tmpfs` (a box with a memory filesystem, where a database's data
    /// directory is free to write and gone on a reboot).
    ///
    /// Free-form on purpose and the *set* is not closed: they are facts for a person and for a
    /// model choosing where to place work, and a closed vocabulary would refuse the flag somebody
    /// needs next week. What is closed is the spelling — a list of strings, refused otherwise — so
    /// a typo is a parse error rather than a host that silently has no flags.
    pub flags: Vec<String>,
    /// **What letibot must never signal, stop or remove on this machine** — `never_touch` in
    /// `hosts.toml`.
    ///
    /// The operator's own workloads live on these hosts beside every agent's: *pgfuzz on lubuntu1,
    /// flowy-dogfood-pg on lubuntu3, the etcd fuzz on lab2x1* (`docs/remote-hosts.md` § *What an
    /// agent hand-rolled without builtins*, item 6). A teardown that removed *everything not mine*
    /// would take them with it, and nothing on the far side knows whose it is — so the list is the
    /// operator's declaration, it is checked before any signal, stop or remove
    /// ([`Host::never_touch_hit`]), and a refusal names the pattern. See that method for why the
    /// match is a substring.
    pub never_touch: Vec<String>,
}

/// **Toolchains through `mise`**, which is how the operator's machines are set up: *"regarding
/// cargo and stuff - we do it via mise generally"* (2026-10-26).
///
/// Measured on all four hosts that day: `mise` lives at `~/.local/bin/mise` and is **not on a
/// non-login shell's `PATH`**, the globally installed tools differ per machine, and a project
/// pins its own with `mise.toml` or `.tool-versions`. So a remote command that runs without
/// `mise` sees the wrong `cargo` or none at all — which is why the wrapper is the default rather
/// than an option, and why `path` (the list in front of `PATH`) stays as the escape hatch for a
/// machine that gets its toolchain some other way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mise {
    /// `mise = false` — do not wrap. A host whose toolchain is on `PATH`, or one where mise is
    /// broken, says so here instead of paying a probe and a wrapper on every call.
    Off,
    /// `mise = "~/.local/bin/mise"` — this program, expanded **on the remote**, once.
    At(String),
}

impl Host {
    /// `user@hostname`, or the bare name — the argument ssh takes.
    pub fn destination(&self) -> String {
        match &self.user {
            Some(u) => format!("{u}@{}", self.hostname),
            None => self.hostname.clone(),
        }
    }

    /// **The pattern on this host's never-touch list that matches `what`**, if one does —
    /// [`never_touch_hit`]'s rule, over this host's list.
    pub fn never_touch_hit(&self, what: &str) -> Option<&str> {
        never_touch_hit(&self.never_touch, what)
    }

    /// **What is left of this machine for anyone else** — `cores − load − reserve`, with the load
    /// read from the machine's own `/proc/loadavg` line (three numbers, and the first is the one a
    /// placement decision is about).
    ///
    /// `None` when the host did not answer either number, which is a different fact from `0.0`:
    /// a machine that is full and one nobody asked are not the same, and reporting the second as
    /// the first is how a placement goes somewhere it cannot work. Negative is reported as it is:
    /// an oversubscribed box is a fact, and clamping it to zero would hide the reason.
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

/// **The operator's hosts**, from `~/.config/letibot/hosts.toml`.
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
    /// **What the operator said about THIS box** — the `[local]` table.
    ///
    /// A section of its own rather than a `[host.local]`, because this box is not a placement: a
    /// `[host.…]` block names a machine ssh reaches, and `local` is the one that needs no ssh at
    /// all. What it holds is the one declaration a process on this box can be checked against, and
    /// it is the same list, matched the same way, as [`Host::never_touch`] — a `process` record
    /// with no host was the hole RH9 left open, and this is the sentence that closes it.
    local: LocalBox,
}

/// **The `[local]` table** — what this box's owner keeps for themselves.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalBox {
    /// **What letibot must never signal, stop or remove on this machine.** One key, because it is
    /// the only question a process-record check can ask about a box that is not a placement: a
    /// `reserve` or a `root` here would be a placement decision about a machine nobody places on.
    pub never_touch: Vec<String>,
}

impl LocalBox {
    /// See [`never_touch_hit`] — one list, one rule, one spelling of the match.
    pub fn never_touch_hit(&self, what: &str) -> Option<&str> {
        never_touch_hit(&self.never_touch, what)
    }
}

/// **The pattern on a never-touch list that matches `what`**, if one does — the ONE copy of the
/// rule, because three lists read it ([`Host::never_touch`], [`LocalBox::never_touch`] and the
/// leases) and two copies of one rule is how they start to disagree.
///
/// The match is **a case-insensitive substring**, and that is the safe direction rather than the
/// lazy one. A name is a process's command line, a compose project or a container — `etcd-fuzz`,
/// `/usr/local/bin/pgfuzz -t 3600`, `flowy-dogfood-pg-1` — and the operator declared the workload,
/// not its spelling. A rule that missed `etcd-fuzz-0` because it was written `etcd` would be the
/// one failure this list exists to prevent, and it would be silent: the thing is stopped, and
/// nobody is told. A rule that is too broad refuses a teardown the session could have had, and
/// **says which pattern refused it** — a failure a person can act on with one word of
/// `hosts.toml`.
pub fn never_touch_hit<'a>(list: &'a [String], what: &str) -> Option<&'a str> {
    let lower = what.to_lowercase();
    list.iter()
        .find(|p| !p.is_empty() && lower.contains(&p.to_lowercase()))
        .map(String::as_str)
}

/// Where the operator's hosts are written. `$LETIBOT_HOSTS` is a **test's** door and
/// nothing else's: it is an environment variable, and a project that could set one
/// could name a machine, which is the whole thing this file exists to prevent.
pub fn hosts_file() -> PathBuf {
    if let Some(p) = std::env::var_os("LETIBOT_HOSTS").filter(|p| !p.as_os_str().is_empty()) {
        return PathBuf::from(p);
    }
    // The config directory is [`Paths`]'s rule, asked rather than restated: two readers of one
    // file that disagree about where it is would be the defect this package exists to remove.
    Paths::from_env().config().join("hosts.toml")
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

    /// **The file's grammar, and its refusals** — `[host.NAME]` tables of nine keys, read the
    /// way `crates/letibot`'s migration already reads this file and **stricter than it used to
    /// be here**, because one of the keys names a file ssh reads.
    ///
    /// * An **unknown key is refused by name** rather than ignored: a typo in this file places a
    ///   session on a machine nobody named, points ssh at a key nobody meant, or leaves a
    ///   `never_touch` workload unprotected.
    /// * A key stated **twice** is refused, for the same reason.
    /// * `path`, `flags` and `never_touch` are lists of strings on one line:
    ///   `path = ["~/.cargo/bin", "/opt/bin"]`. One line rather than TOML's multi-line array
    ///   because this reader is a line reader and a quietly-ignored continuation would be the
    ///   strictness of this file in name only.
    /// * `reserve` is a whole number of cores, and anything else is refused.
    /// * A `#` starts a comment everywhere, including inside a quoted value — the one place this
    ///   reader is not TOML, and it is said rather than discovered.
    ///
    /// Nothing is validated beyond the grammar here: **whether a host may actually be used is
    /// asked when it is used** ([`Ssh::connect`]), where the answer can name the key file, the
    /// known-hosts file and the one command that fixes either. A file with three good hosts and
    /// one whose key is missing still places the three.
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
            // **`[local]` is THIS box**, and it is read before `[host.…]` because it is not one: a
            // host block names a machine ssh reaches and this section names what the machine
            // underneath the daemon keeps for itself. One key, refused by name like the host
            // blocks', because a `[local].never_tuch` typo would leave the operator's own workload
            // unprotected and silent — the one failure this list exists to prevent.
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
            // **Two more lists, one grammar.** `flags` and `never_touch` are the same shape as
            // `path` and are refused the same way, because the reason is the same one: a
            // quietly half-read list is a rule that is kept in name only. That matters differently
            // for the two — a `flags` a reader dropped is a host that looks like it has none, and a
            // `never_touch` dropped is a teardown that stops the operator's own workload.
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
            // **`reserve` is a count of cores**, so a number and nothing else. Refused rather than
            // defaulted: a `reserve = "two"` read as zero would be a scheduler that believes a
            // busy machine is empty, which is the collision the whole package is against.
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
            // **`mise` is two shapes in one key**: `false` turns the wrapper off, and a string is
            // the program to run. A `true` is refused rather than read as "find it yourself",
            // because that is what leaving the line out already means — and two spellings of one
            // meaning is how a file starts disagreeing with itself.
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

    /// **What this box's owner keeps for themselves** — the `[local]` table, empty when the file
    /// says nothing about this machine.
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

/// `["a", "b"]` → the strings, with the quotes and spaces off, and an empty list for `[]`.
///
/// A tiny reader rather than a TOML one: this file's grammar is a line of `key = value`, and this
/// is the only value that is not a plain string. What it refuses, it refuses by name (see
/// [`Hosts::parse`]) rather than reading half of it.
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

/// `~/x` in a key path, expanded **here**, because it names a file on this machine.
fn expand_tilde(p: &Path) -> PathBuf {
    expand(p, &Paths::from_env().home)
}

// ---------------------------------------------------------------------------
// The ssh policy — one place, for both callers
// ---------------------------------------------------------------------------
//
// The operator, 2026-10-26: *"letibot is forbidden to touch and even read `~/.ssh`"*. Both
// ssh callers in this tree — [`SshBackend`] (a session whose *tools* run elsewhere) and
// `letibot --migrate` (which moves a session's daemon) — used to spell their own flags, and the
// two spellings had already drifted: neither passed `-F`, so both read the operator's ssh client
// config, and neither checked the host key it was about to trust. Everything below is written
// once, and `crates/letibot`'s migration calls **this** code rather than a copy of it.

/// letibot's own known-hosts file, inside [`Paths::config`] — and not the operator's: a host key
/// accepted for letibot is not a host key accepted for their own `git push`.
pub const KNOWN_HOSTS_FILE: &str = "known_hosts";

/// Where a host's dedicated identity file lives, inside [`Paths::config`].
pub const KEYS_DIR: &str = "keys";

/// **The two directories an ssh call is decided against**: the operator's home, inside which one
/// directory is off limits, and letibot's own config directory, where everything it does read
/// lives.
///
/// A value rather than an environment read at each use, for the reason
/// [`crate::intent::Surroundings`] is one: a test pins the home it means, and a session's paths
/// are a property of the session rather than of whichever thread asked.
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

    /// A test's own, with the config directory somewhere other than under the home — the shape a
    /// box with `XDG_CONFIG_HOME` set has.
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

    /// `keys/<host>` under the config directory — **the key a host uses when the file says
    /// nothing**.
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

/// `~/x` → `<home>/x`. A `~` left unexpanded would name a directory called `~`, and a path that
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

/// **The real path of `path`, following symlinks — for a path that need not exist yet.**
///
/// `canonicalize` answers only about a path that is there, and a rule that quietly stops applying
/// to a path that is not — a key file inside the store that nobody has written yet — is the shape
/// of hole this tree has been bitten by before. So the deepest existing ancestor is canonicalised
/// and the remaining components are applied to it lexically: enough to answer *would this land
/// inside the store*, which is the question a write target asks.
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

/// **Is `path` inside the operator's ssh directory** — after symlinks, and about a path that may
/// not exist. The rule is about where the bytes are, so a link in the workspace named `keys` and
/// pointing at that directory is that directory.
pub fn inside_user_ssh(path: &Path, home: &Path) -> bool {
    let path = expand(path, home);
    let store = real(&home.join(".ssh"));
    let real_path = real(&path);
    real_path == store || real_path.starts_with(&store)
}

/// **The refusal the operator's rule gives**, in their own words.
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

/// **The identity file a host connects with** — the one `hosts.toml` names, or the dedicated
/// `keys/<host>` under the config directory — after the two refusals that make it letibot's own:
/// it is absolute, it exists, and it does not resolve into the operator's ssh directory.
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

/// **Is this host's key already in letibot's own known-hosts file?**
///
/// The format is ssh's: one entry per line, `<host>[,<host>…] <type> <base64>`, with a bracketed
/// `[host]:port` for a non-default port. Two things this deliberately does not do: a **hashed**
/// entry (`|1|…`, which `ssh-keyscan -H` writes) cannot be recognised here and reads as absent —
/// the way out is the same scan without `-H` — and a wildcard entry is not honoured, since
/// `StrictHostKeyChecking=yes` matching a pattern is the guessing this file exists to avoid.
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
            // `[host]:2222` — the same machine on a port ssh was told about.
            if let Some(inner) = name.strip_prefix('[').and_then(|n| n.split(']').next())
                && inner == hostname
            {
                return true;
            }
        }
    }
    false
}

/// **The refusal for a host whose key nobody has accepted here**, with the one command that adds
/// it — printed for the operator, never run by letibot. A key it scanned and trusted on its own
/// is a key nobody checked, which is strict about nothing.
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

/// **Every flag every letibot ssh runs with, in one list.**
///
/// `-F /dev/null` comes first: the operator's ssh client config is not read at all, so not even
/// its `Host` blocks, `ProxyCommand`s or `IdentityFile`s can reach this call. Then the files ssh
/// may read, then what it may do, then the connection, then the identity.
///
/// **`-i` and `IdentitiesOnly=yes` are together on purpose**: `-i` *offers* a key and
/// `IdentitiesOnly` is what stops ssh adding the agent's and the config's. Either alone is the
/// defect — a key file named and then passed over for an agent key that answers first.
pub fn flags(key: &Path, control: &Path, paths: &Paths) -> Vec<String> {
    let mut v: Vec<String> = vec!["-F".into(), "/dev/null".into()];
    for (k, val) in [
        // **letibot's own file**, and the system's refused: the host keys letibot trusts are the
        // ones the operator put there for it.
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
        // **The agent is the operator's own keys.** ssh would offer every one of them, in ssh's
        // own order, before the one file it was given.
        ("IdentityAgent", "none".to_string()),
        ("IdentitiesOnly", "yes".to_string()),
        // One TCP connection per host: a command then costs a round trip, not a handshake. The
        // socket is in letibot's rundir ([`control_path`]).
        ("ControlMaster", "auto".to_string()),
        ("ControlPath", control.display().to_string()),
        ("ControlPersist", "120".to_string()),
        ("ConnectTimeout", "20".to_string()),
        // **A TCP connection that is up is not a host that is answering.** ConnectTimeout only
        // covers the handshake; a link that dies — a laptop's wifi, a suspended VM, a machine
        // rebooting — leaves a socket that accepts writes and answers nothing for ever. These two
        // ask for a sign of life every 5 s and give up after three misses, so the *connection* is
        // declared dead in about 15 s and the ssh waiting on it returns instead of hanging. It is
        // half the answer; the other half is the wall clock every call carries
        // ([`STEP_BOUND`]), because a master that is still up but wedged answers no keepalive.
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

/// **A host's `root` as the remote shell must spell it** — `~` expanded THERE, once, and the rest
/// quoted.
///
/// The rake this exists for, measured (RH1, and again by RH4): **`~` inside single quotes is a
/// literal character**, so `mkdir -p '~/letibot-remote'` makes a directory called `~` under the
/// current one, and every path this client builds from then on names a place nobody meant. Quoting
/// exists to stop the shell expanding things; a root is the one path that WANTS the expansion, so
/// it is spliced in as `"$HOME"/…` with only the remainder quoted — the same rule
/// [`Host::path`](struct.Host.html) entries follow, in one function so the two cannot drift.
pub fn expand_root(root: &str) -> String {
    one_entry(root)
}

/// letibot's runtime directory — the launcher's rule, restated here because this crate is
/// **below** the launcher (`crates/letibot` depends on this one, not the other way round).
pub fn rundir() -> PathBuf {
    let base = match std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.as_os_str().is_empty()) {
        Some(d) => PathBuf::from(d),
        None => std::env::temp_dir(),
    };
    base.join("letibot")
}

/// The `ControlMaster` socket for one host **and one daemon**, under [`rundir`].
///
/// **The pid is in the name, and that is R99's fix.** One socket per host, shared by every daemon
/// on the box, is one OpenSSH connection with `MaxSessions` (10) slots on it — and six package
/// daemons placing work, polling their jobs and writing their files ask for far more than ten at
/// once. The eleventh is refused by the *server* (`channel 0: open failed: administratively
/// prohibited`) or lost by the client (`mux_client_request_session: session request failed`), and
/// a write refused halfway is how a file ends up at zero bytes (FB's note, and
/// `crates/tools/src/builtins/fanout.rs` twice). A socket per daemon makes the in-process bound
/// ([`rpc::MAX_SESSIONS_PER_HOST`]) the only bound that matters: nothing else multiplexes on it,
/// so eight calls at once is eight sessions and not eight of the ten somebody else's daemon left.
///
/// The cost is one dial per daemon per host instead of one per host — measured on lab2x1
/// (32 cores, its own sshd on loopback): a full handshake, key exchange and public-key attempt is
/// **50-70 ms**, and `ControlPersist=120` makes it once per two minutes of idleness. Six daemons
/// therefore pay six dials where they paid one, and stop paying it in refusals.
///
/// **A unix socket path is capped** (`SUN_LEN`: 104 bytes on this Mac, 108 on Linux), and a
/// per-user temp directory under `/var/folders/…` is most of that before a name is added — so the
/// long answer falls back to `/tmp`, which is short on every box this runs on. Measured: a path
/// past the cap fails the bind with a sentence that reads like a bug in the caller.
pub fn control_path(host: &str, pid: u32) -> PathBuf {
    control_path_for(host, pid, "call")
}

/// The socket this daemon's **git** work multiplexes over — [`control_path`] with a role of its
/// own.
///
/// `git fetch` reaches `git-upload-pack` through an ssh that this crate does not run: it hands git
/// the flags ([`Ssh::git_flags`]) and git starts the process (`core.sshCommand`). That ssh never
/// passes [`rpc::enter`], so it is not counted by the eight-slot gate and it can be the ninth or
/// tenth session on a master — which is exactly the arithmetic R99 is about. A second socket makes
/// the two workloads unable to meet: the call master holds at most `MAX_SESSIONS_PER_HOST` and the
/// git master holds one session per fetch, and neither can refuse the other.
///
/// It is a second TCP connection per daemon per host, dialled only when a session is closed or a
/// placement asks for its branch back — a handful per hour, at 50-70 ms each.
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

/// **The host's `path` list as the export a remote command starts with** — `None` when the file
/// declares none, so a command's text is unchanged for every host that does not need it.
///
/// Expanded by the **remote** shell and not here: an entry is a path on that machine (`~` is
/// *its* home), so expanding it on this box would write this box's directories into a command
/// that runs somewhere else. Prepended, never replaced: the far side's own `PATH` is what finds
/// `git`, `bash` and coreutils.
pub fn path_export(entries: &[String]) -> Option<String> {
    let entries = quoted_entries(entries)?;
    Some(format!("export PATH={}:\"$PATH\"", entries.join(":")))
}

/// The same list as a **prefix assignment** rather than an export, for a remote command that is
/// not a script and so has nowhere to put an `export` — git's `upload-pack`, which ssh runs as
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

/// One `path` entry as the remote shell must see it.
///
/// **A `~` handed to `quote()` would be a literal character**, because quoting exists to stop the
/// shell expanding anything — so `~/.cargo/bin` would name a directory called `~` on the far side
/// and put nothing on `PATH` at all. The tilde is expanded **there, once**, as `"$HOME/…"`; the
/// rest of the entry is quoted as any other word is, and the two are written as adjacent words so
/// one awkward path cannot break out of the quoting.
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
    /// **What this host's `path` list does to a remote script**, or the script itself.
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

// ---------------------------------------------------------------------------
// The ssh channel
// ---------------------------------------------------------------------------

/// **How long one ssh call that is not a command may take** before this box stops waiting for it.
///
/// A command's deadline is the tool's (`bash`'s timeout, `job_wait`'s own deadline); this is the
/// other kind of call — the ones this backend makes **for itself**: a read, a write chunk, a
/// listing, a status poll, a probe, a job's output, a kill. None of them is work the operator
/// asked to run, so none of them is allowed to hold a daemon: a call past this bound has its
/// **local ssh killed** and the operation fails, naming the host and the step.
///
/// The number is chosen against the policy beside it: `ServerAliveInterval=5` with
/// `ServerAliveCountMax=3` gives up on a dead connection in about 15 s, and `ConnectTimeout=20`
/// gives up on an unreachable one in 20 s — so 20 s is the point past which nothing this side can
/// still call waiting.
pub const STEP_BOUND: Duration = Duration::from_secs(20);

/// The bound on one **status poll** ([`RemoteProcesses::refresh`]). Shorter than [`STEP_BOUND`]
/// because a poll's answer arrives for the *next* tick: a `job_wait` that polls every 120 ms must
/// still return within the deadline it was given, and a poll that takes half a minute to come back
/// describes a machine this daemon has already given up on.
const POLL_BOUND: Duration = Duration::from_secs(5);

/// The bound on the calls that only ask **about the connection** — `ssh -O check` and `-O exit`.
/// They are local IPC to a socket with no network in them at all, so anything past this is a
/// master that has stopped answering.
const MASTER_BOUND: Duration = Duration::from_secs(3);

/// The bound on the local `git` work a placement does: `bundle create`, the `fetch` back. Generous
/// because it is real work on a real repository — the number only has to be below "for ever".
pub const GIT_BOUND: Duration = Duration::from_secs(300);

/// **What a caller may do when a call does not answer at all.**
///
/// Retrying follows idempotency, which is the rule any RPC client lives by: a `read` whose answer
/// never arrived changed nothing on the other machine, so it is asked once more; a `bash` command
/// that may still be running there is never asked twice, because a second run is a second effect
/// and reporting it as a failure would be a lie about the first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Retry {
    /// Asking again is the same as asking once: nothing changed, or the operation lands
    /// atomically (a temp file and one `mv -f`).
    Once,
    /// Never asked twice: the far side may have acted, partly acted, or be acting still.
    Never,
}

/// **Whether a call owes the record a line** — rule 7's ring, and how a heartbeat stays out of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reported {
    /// Every attempt goes into the ring: this is a call somebody asked for.
    Always,
    /// **Only the attempts that did not answer.** A status poll is a heartbeat — [`POLL_BOUND`],
    /// once every 120 ms for as long as a job is waited on — and [`crate::rpc::RECORD_RING`]
    /// entries of it are half a minute's worth, which would push out every call that mattered. A
    /// poll that *fails* is news, and is kept like anything else.
    Failures,
}

/// **How one bounded ssh call ended** — the shape the retry rule is written against.
enum Call {
    /// The far side answered: what the command printed, ssh's own stderr, and the exit status.
    Answered(String, Vec<u8>, i32),
    /// The bound passed with nothing to show for it. The local ssh was killed.
    TimedOut(Duration),
    /// ssh failed before the command could say anything — exit 255 with nothing on stdout: the
    /// connection dropped, the name does not resolve.
    Dropped(Vec<u8>),
    /// **The far side said no and meant it** — `Permission denied (publickey)`, a host key that
    /// does not verify. Definite: not a lost connection, never retried, and its sentence names the
    /// fix ([`Ssh::denied`]).
    Denied(AuthRefusal, Vec<u8>),
    /// **The multiplexer refused a session, so the command never started** — a full master
    /// (`MaxSessions`), a control socket that went away, an fd that would not pass. It is a
    /// transport failure about *this call*, not the command's own exit and not the host being
    /// down, and it is retried on a fresh connection whatever the caller's policy says, because
    /// nothing ran ([`mux_refusal`]).
    MuxRefused(MuxRefusal, Vec<u8>),
}

/// **The four ways OpenSSH's multiplexer loses a session, and what each one proves.**
///
/// They look like four different faults and they are one: the connection is up, and this call did
/// not get a channel on it. That is why they are separated from [`Call::Dropped`] — which says
/// *the connection went away and nobody knows what happened* — and why they are retried even for
/// an at-most-once operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MuxRefusal {
    /// `mux_client_request_session: session request failed` — the master refused to open the
    /// session. With `SendEnv`/`SetEnv` or a full session table, the request never became a
    /// channel.
    Session,
    /// `mm_send_fd: sendmsg(1): Message too long` — the client could not hand the session's file
    /// descriptors to the master. **Measured 2026-10-27** as the sentence an `edit` through the
    /// git placement died of (FB's note), and it is the one that reads most like a broken pipe.
    Fds,
    /// `channel 0: open failed: administratively prohibited` — the *server* refused the channel:
    /// `MaxSessions` reached, or the server's own limit on the account.
    Channel,
    /// `Control socket connect …: Connection refused` — the socket file is there and nothing is
    /// listening on it: a master that died or was killed.
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

/// **The two ways ssh says *not you*.** They are separated because the fix differs — a key that is
/// not installed, versus a machine whose key is not the one this box recorded — and a sentence that
/// offered both would send the operator to the wrong file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthRefusal {
    /// `Permission denied (publickey[,…]).`
    Key,
    /// `Host key verification failed.` / `No … host key is known for …`.
    HostKey,
}

/// **Did the far side say anything at all?** A refusal counts — `Permission denied` is a machine
/// that is up — and a lost connection or a passed deadline does not.
///
/// **A refused mux session counts as connected, and that is deliberate**: a master with no free
/// session slot is a busy *path*, not a host that is down, and a stale socket is this box's own
/// litter. Counting either as a connection failure would open the breaker on a healthy host the
/// moment six packages wrote at once — which is exactly the load R99 is about.
fn connected(call: &Call) -> bool {
    matches!(
        call,
        Call::Answered(..) | Call::Denied(..) | Call::MuxRefused(..)
    )
}

/// **Read ssh's own words as *refused* or *lost*.** OpenSSH prints these and exits 255 with
/// nothing on stdout, which is exactly the shape a dropped connection has — so the difference has
/// to be read from stderr, and it has to be read here rather than guessed at by a caller.
///
/// Matched as substrings rather than whole lines: these arrive prefixed by `user@host:`, and
/// `Host key verification failed.` is preceded by the message that says which key and why
/// (`No ED25519 host key is known for x and you have requested strict checking.`).
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

/// **Read ssh's words for *the connection is fine, this session was not*** — R99's
/// classification.
///
/// These four arrive exactly like a dropped connection: exit 255, nothing on stdout, and a line on
/// stderr that mentions something a reader has to know OpenSSH to understand. Reported as
/// [`Call::Dropped`] they cost two things, both measured on 2026-10-27 with six packages on
/// lab2x1: a write's chunk was **retried by the whole-write rule and its stderr thrown away** (so
/// the model was told the connection was lost, or worse, nothing at all), and a `run` — which is
/// at-most-once — was reported as *it may have run*, when in fact no channel ever existed.
///
/// The last one carries the fact that makes the retry safe: `mm_send_fd` failing means the
/// session's descriptors never reached the master, so the remote shell was never started. The same
/// is true of a refused session request, of a channel the server refused, and of a socket nothing
/// listens on. **None of them can have run the command**, so rule 3 permits the retry even for an
/// operation that is otherwise at-most-once.
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

/// **A command wrapped so its remote process group is known** — rule 6's half that runs there.
///
/// `setsid` makes the shell a session leader, so its pid *is* its process group and
/// `kill -TERM -<pid>` reaches everything it forks. The pid is written to `file` before the
/// command starts, and removed when it ends normally, so the file is either the live group or
/// nothing — never a stale id a later cancel could signal by mistake. Where the host has no
/// `setsid` the wrapper still runs, and [`Ssh::cancel_group`] asks `ps -o pgid=` before it
/// signals, so a pid that does not lead a group gets a single-process kill and never a group one.
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

/// **The bound on ending a remote process group** — [`Ssh::cancel_group`]'s call.
///
/// Ten seconds: long enough to dial a fresh master and signal, short enough that a command which
/// already spent its own bound is not made to spend a second one twice. It is a cleanup, and the
/// sentence the caller already has says the outcome there is unknown when it cannot be confirmed.
const CANCEL_BOUND: Duration = Duration::from_secs(10);

/// **Which `mise` a host turned out to have**, per process — `None` meaning *asked, and it has
/// none*.
///
/// A cache because the answer costs a round trip and does not change under a running daemon; a
/// negative entry because a host without mise must pay for that fact once and not once per
/// command.
static MISE: std::sync::LazyLock<Mutex<BTreeMap<String, Option<String>>>> =
    std::sync::LazyLock::new(|| Mutex::new(BTreeMap::new()));

/// **What a host said when it was asked whether it can confine a command**, per process.
///
/// Keyed by host name because that is the identity of the machine and the answer is a fact
/// about its kernel, its `bwrap` and its `PATH` — not about a session. **The measurement is
/// cached, not a boundary**: the view is the session's (its own checkout, its own grants), so a
/// second session on the same host re-applies the same measurement to its own view rather than
/// paying for another round trip.
///
/// A probe that did **not answer** is not cached, for the reason [`MISE`] gives: a host that was
/// briefly unreachable has not told us it cannot confine, and remembering a timeout as a fact is
/// how a session spends the rest of its life unconfined. An answer that says *`bwrap` is not
/// there*, or one where `bwrap` ran and did not enter the namespaces, **is** cached: those are
/// facts about the machine.
static CONFINE: std::sync::LazyLock<Mutex<BTreeMap<String, crate::exec::confine::HostProbe>>> =
    std::sync::LazyLock::new(|| Mutex::new(BTreeMap::new()));

/// **Where a confined remote command's `$HOME` is** — under the host's own `root`, never the
/// operator's home.
///
/// It has to be writable and it has to be somewhere the host already keeps letibot's work: a
/// command needs a `$HOME` that is not read-only (a compiler writes there by habit), and it must
/// not be the operator's, because that directory is the one the boundary exists to leave
/// outside. `~/.cargo` and `~/.rustup` are still the operator's, read-only — see
/// [`crate::exec::confine::GRANT_CANDIDATES`].
///
/// **`<HOME_DIR>/<short>/tmp` is the session's `/tmp`**, bound there by
/// [`confine_a_checkout`]: one directory per session, beside that session's home, so a file one
/// command writes survives to the next and nothing of the host's `/tmp` is visible. See
/// [`crate::exec::confine::TmpView`].
pub const HOME_DIR: &str = ".letibot-home";

/// **One host's ssh connection**, opened by [`Ssh::connect`] after the two refusals that make
/// it letibot's own: its identity file, and its host key.
///
/// Everything about the command line is [`flags`]'s, and there is nothing here that a caller can
/// restate: `-F /dev/null` (no operator ssh client config), letibot's own known-hosts file, the
/// system's refused, `StrictHostKeyChecking=yes` under `BatchMode=yes`, `IdentityAgent=none`,
/// the one `-i` key and `IdentitiesOnly=yes`, and a `ControlMaster` socket in letibot's rundir so
/// a command costs a round trip rather than a handshake. See the policy section above for why
/// each one is there.
///
/// The program is a field (`$LETIBOT_SSH` by default) so a test can substitute a fake that runs
/// the command locally; nothing else about this file changes for it.
#[derive(Clone)]
pub struct Ssh {
    host: Host,
    program: PathBuf,
    /// The identity file ssh is given, resolved and checked once, at [`Ssh::connect`].
    key: PathBuf,
    /// This daemon's master for its own RPC calls — [`control_path`], and the socket
    /// [`rpc::MAX_SESSIONS_PER_HOST`] is a promise about.
    control: PathBuf,
    /// This daemon's master for **git's** ssh, which this process does not run and therefore
    /// cannot count against that bound ([`git_control_path`]).
    git_control: PathBuf,
    paths: Paths,
    /// The wall clock on every call that is *not* a command — [`STEP_BOUND`] unless a caller (a
    /// test, mostly) names another.
    bound: Duration,
    /// **Where the remote command's work is**, as an absolute path on that machine — the
    /// checkout once a placement has made one, and the host's `root` before that.
    ///
    /// It is `mise exec -C <dir>`'s argument, and it is why this is a field: a toolchain is
    /// pinned by a `mise.toml` or a `.tool-versions` in the *tree the work is in*, so the
    /// wrapper must be told which tree. `None` is the remote's home, which is what a probe or a
    /// connection test has.
    dir: Option<String>,
}

impl Ssh {
    /// **Open the connection a host is reached through**, or say why it cannot be.
    ///
    /// Two refusals happen here and both name the operator's rule and the fix: a key that is
    /// missing or inside their own ssh directory ([`identity_for`]), and a host key nobody has
    /// accepted in letibot's own known-hosts file ([`unknown_host_key`]). They happen **before**
    /// anything is arranged on the machine, which is the whole point of this being the
    /// constructor rather than a check somewhere down the command path: a placement that failed
    /// half way would have a checkout on the far side and no way to say which step refused.
    pub fn connect(host: Host, program: Option<PathBuf>, paths: Paths) -> Result<Ssh, String> {
        let key = identity_for(&host.name, host.key.as_deref(), &paths)?;
        if !host_key_known(&paths.known_hosts(), &host.hostname) {
            return Err(unknown_host_key(&host.hostname, &paths));
        }
        let control = control_path(&host.name, std::process::id());
        let git_control = git_control_path(&host.name, std::process::id());
        // The socket's directory has to exist or ssh cannot make the master — and a missing
        // master is not an error, it is a handshake per command, which nobody asked for. A
        // failure here is not fatal: ssh says so on its own stderr.
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
        })
    }

    /// **Tell the connection which tree the work is in**, once a placement knows: the remote
    /// checkout, absolute. It is `mise exec -C`'s argument, so it decides which `mise.toml` a
    /// remote command's toolchain is read from.
    pub fn with_dir(mut self, dir: impl Into<String>) -> Ssh {
        self.dir = Some(dir.into());
        self
    }

    /// The directory a wrapped command is run in — `-C`'s argument.
    ///
    /// **`one_entry`, not `quote`.** This is a path on the *remote*, so a `~` in it must be
    /// expanded **there**, exactly as `path`'s entries are: `quote` would hand mise a literal
    /// `~`, which mise resolves against the directory it happens to be in, not against that
    /// machine's home. Measured on a host with a real mise (2026-10-26): the wrapper is the one
    /// thing between a session and *every* remote command, so a misspelling here is a shape that
    /// fails on all four of the operator's machines and on none of this box's tests.
    fn dir_arg(&self) -> String {
        match &self.dir {
            Some(d) => one_entry(d),
            None => "\"$HOME\"".to_string(),
        }
    }

    /// **The wall clock this connection's own work is given.** A test that wants a fake ssh's
    /// silence to be measured in a second rather than in [`STEP_BOUND`] says so here; production
    /// never calls it, which is why the default is the constant and not a field a caller has to
    /// fill in.
    pub fn with_bound(mut self, within: Duration) -> Ssh {
        self.bound = within;
        self
    }

    /// The bound this connection's non-command calls carry.
    pub fn bound(&self) -> Duration {
        self.bound
    }

    /// **The bound on one status poll** — [`POLL_BOUND`], and never longer than the connection's
    /// own bound: a caller that asked for a second of patience did not ask for five times that on
    /// every tick of a wait loop.
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

    /// The identity file this connection was opened with — resolved, existing, and outside the
    /// operator's ssh directory.
    pub fn key(&self) -> &Path {
        &self.key
    }

    /// The flags, and nothing else — what `git` needs for `core.sshCommand`, where the
    /// destination and the command are git's to append.
    pub fn flags(&self) -> Vec<String> {
        flags(&self.key, &self.control, &self.paths)
    }

    /// **The same flags, pointed at the git master** — what `core.sshCommand` is built from.
    ///
    /// A fetch is an ssh this process never sees: git starts it, so it takes no place in
    /// [`rpc::enter`]'s queue and it would be the ninth session on the master the calls are
    /// bounded on. One socket for the calls and one for git is the whole of the fix, and it is why
    /// `MAX_SESSIONS_PER_HOST` is a guarantee rather than an average.
    pub fn git_flags(&self) -> Vec<String> {
        flags(&self.key, &self.git_control, &self.paths)
    }

    /// The socket this connection's own calls multiplex over — a test asks this, production does
    /// not: it is the one fact that says *one master per daemon*.
    pub fn control_socket(&self) -> &Path {
        &self.control
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

    /// **Every remote command this connection runs, spelled once** — the contract's
    /// `ssh host '<mise> exec -C <worktree> -- bash -lc …'`.
    ///
    /// Three things are put around a script, and each is a rule from `docs/remote-hosts.md`:
    ///
    /// * **`bash -lc`**, because a login shell is what reads the remote's profiles.
    /// * **The host's `path` list in front of `PATH`, inside that shell and not beside it** — a
    ///   login shell reads the remote's profiles, and a profile that sets `PATH` would otherwise
    ///   undo a prefix applied before it.
    /// * **`mise exec -C <dir> --` around the lot**, when the host has mise: the operator's
    ///   toolchains are pinned per project (*"regarding cargo and stuff - we do it via mise
    ///   generally"*), so the wrapper is what makes `cargo` mean the pinned one. A `mise exec`
    ///   may install a pinned tool on its first use — network, on the remote, as the command
    ///   asked for.
    ///
    /// **Every door goes through here** — a foreground command, a step of this backend's own
    /// work, a job's launcher, a probe, git's `upload-pack` — so the three cannot drift apart.
    ///
    /// The returned stderr is the ssh client's own (a refused key, a name that does not
    /// resolve) and never the remote command's: ssh does not carry two channels apart
    /// into two pipes. That is [`crate::firecode`]'s trade as well, and it is stated in
    /// [`SshBackend::describe`] rather than left to be discovered.
    pub fn remote_command(&self, script: &str) -> String {
        let body = format!("bash -lc {}", quote(&self.host.with_path(script)));
        match self.mise_program() {
            Some(program) => format!("{program} exec -C {} -- {body}", self.dir_arg()),
            None => body,
        }
    }

    /// **The `mise` this host runs with, or `None`** — [`Mise::Off`], a host with no mise, or a
    /// probe that could not answer.
    ///
    /// `hosts.toml` wins when it says anything. Otherwise the answer is **probed once per host
    /// per process** and cached, including *no mise*: `~/.local/bin/mise` first (that is where
    /// the operator's hosts keep it), then `PATH`, in one bounded call. Caching the negative is
    /// the point — a host without mise must pay one round trip for that fact and not one per
    /// command.
    ///
    /// A probe that did **not answer** is not cached: a host that was briefly unreachable has
    /// not told us it has no mise, and remembering a timeout as a fact is how a session spends
    /// the rest of its life without a toolchain.
    fn mise_program(&self) -> Option<String> {
        match &self.host.mise {
            Some(Mise::Off) => None,
            // Expanded THERE, once — `~` is that machine's home.
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

    /// **Ask the host where its mise is**, once, or answer `None` when it has none — or when the
    /// question did not get an answer, which the caller deliberately does not remember.
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

    /// **Ask the host whether it can confine a command, once per process** — RH3's one round
    /// trip.
    ///
    /// The script is [`crate::exec::confine::probe_script`]'s, run the way every other remote
    /// command is run (`bash -lc` with the host's `PATH` in front, inside `mise exec` where the
    /// host has mise) so that the argv it measures is the argv a command will be wrapped in:
    /// the keep-list of that run is that shell's, which is the whole reason a remote probe is
    /// not [`crate::exec::Bwrap::probe`].
    ///
    /// The answer is cached per host ([`CONFINE`]), and only when the host **answered**: a
    /// timeout or a dropped connection is not a fact about what the machine has installed, so
    /// it is not remembered and the next attempt asks again.
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
        // **stderr is read too**, and it is where a helper that refused says so: ssh carries one
        // channel for the command's own output but this call is not wrapped in one shell, so the
        // two arrive separately here. A `bwrap: ` line is the sentence a reader needs.
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

    /// **The prefix git's `upload-pack` is invoked with**, for the fetch that brings a branch
    /// home.
    ///
    /// sshd runs the far side of a `git` fetch as `<upload-pack> '<path>'` — not a login shell,
    /// so no profile and no mise — which is why the toolchain wrapper has to be spliced here too.
    /// "Background jobs and git steps run through the same wrapper" is the contract's sentence,
    /// and this is the git half of it.
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

    /// **One command of the model's — [`SshBackend::run`]'s door, and the only caller.**
    ///
    /// A command keeps the deadline it is given and nothing else ([`Ssh::bound`]), and it is
    /// **at most once**: a call that died in flight may have run, partly run, or still be running
    /// there, so it is reported by [`Ssh::unknown`] and never re-sent.
    ///
    /// When `pgid` is given, the command is wrapped so **its remote process group is known** —
    /// `setsid bash -c 'echo $$ > <file>; …'` — and a call that stopped answering kills that
    /// group ([`Ssh::cancel_group`]). That is rule 6: an interrupt or a timeout leaves nothing
    /// running on the far side, rather than only closing the local ssh and hoping.
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

    /// **Rule 6: kill the remote process group of a command that stopped answering.**
    ///
    /// Its own bounded call, because the call that timed out had its local ssh killed and that
    /// is exactly the case where the far side may still be running: sshd's hang-up usually takes
    /// the group with it, and "usually" is not a rule. The group is signalled **only when the pid
    /// really leads one** — `kill -TERM -<pid>` on a pid that does not signals *this shell's*
    /// group — and the read-back is inside the same call so a group that ignores `TERM` is still
    /// gone when this returns.
    ///
    /// Best effort by construction: if the connection is gone, the sentence the caller already
    /// has says the outcome is unknown, and this adds nothing false to it.
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
        let command = format!("bash -lc {}", quote(&self.host.with_path(&script)));
        let _ = self.raw("ending the remote process group", &command, CANCEL_BOUND);
    }

    /// **One step of this backend's own work** — bounded by [`Ssh::bound`], and asked again once
    /// when the operation is idempotent and the first attempt did not answer.
    ///
    /// `step` names the operation for the refusal (*reading a path*, *listing a directory*, *the
    /// git bundle*), because an operation that stops waiting has to say which one stopped: a
    /// sentence that says only "ssh failed" sends the operator to the wrong machine.
    fn sh_step(&self, step: &str, script: &str) -> Result<(String, Vec<u8>, i32), BackendError> {
        self.sh_step_within(step, script, self.bound)
    }

    /// [`Ssh::sh_step`] with a bound of its own — a status poll is shorter than a write.
    fn sh_step_within(
        &self,
        step: &str,
        script: &str,
        within: Duration,
    ) -> Result<(String, Vec<u8>, i32), BackendError> {
        match self.call(step, script, Retry::Once, within, Reported::Always, None)? {
            Call::Answered(out, stderr, exit) => Ok((out, stderr, exit)),
            other => Err(BackendError::Io(self.no_answer(step, &other, within))),
        }
    }

    /// **One step, exactly once, and silence is a refusal** — for the steps that must not be asked
    /// twice even though they are not commands: a write's chunk (a retry would append it again),
    /// and a job's start (a retry would start a second one).
    pub fn sh_once(
        &self,
        step: &str,
        script: &str,
    ) -> Result<(String, Vec<u8>, i32), BackendError> {
        self.sh_once_within(step, script, self.bound)
    }

    /// [`Ssh::sh_once`] with a bound of its own — a status poll is shorter than a write.
    fn sh_once_within(
        &self,
        step: &str,
        script: &str,
        within: Duration,
    ) -> Result<(String, Vec<u8>, i32), BackendError> {
        match self.call(step, script, Retry::Never, within, Reported::Always, None)? {
            Call::Answered(out, stderr, exit) => Ok((out, stderr, exit)),
            other => Err(BackendError::Io(self.no_answer(step, &other, within))),
        }
    }

    /// **A status poll**: the same call, but only a poll that fails is recorded.
    ///
    /// [`RemoteProcesses::refresh`] runs this every 120 ms for as long as a job is waited on. The
    /// record ring is 256 entries, so a `job_wait` of thirty seconds' worth of *successful*
    /// heartbeats would evict every call a reader could act on — and a successful poll says only
    /// *the machine is still there*, which is the one thing a report does not need.
    fn sh_poll_within(
        &self,
        step: &str,
        script: &str,
        within: Duration,
    ) -> Result<(String, Vec<u8>, i32), BackendError> {
        match self.call(step, script, Retry::Never, within, Reported::Failures, None)? {
            Call::Answered(out, stderr, exit) => Ok((out, stderr, exit)),
            other => Err(BackendError::Io(self.no_answer(step, &other, within))),
        }
    }

    /// **The same call, but a non-zero exit is an error carrying what the host said.** Used by
    /// everything that is arranging rather than running the model's work.
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
                other => return Err(BackendError::Io(self.no_answer(what, &other, within))),
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

    /// **One step, with the retry its caller allows** — the rule lives here and nowhere else —
    /// and with rules 5, 7 and 8 around it.
    ///
    /// The order is the design:
    ///
    /// 1. **The breaker** ([`rpc::before`]) refuses at once when this host is known to be down,
    ///    rather than paying another timeout to find that out again.
    /// 2. A stale `ControlMaster` socket is cleared ([`Ssh::fresh_master`]), because ssh would
    ///    otherwise try to multiplex through a master that is gone and fail — or wait — on the
    ///    socket rather than on the host.
    /// 3. The attempts, each of which records itself ([`rpc::record`]) and queues for a slot
    ///    ([`rpc::enter`]).
    /// 4. **The breaker's verdict** ([`rpc::after`]), with *connected* meaning *the far side said
    ///    something* — which a `Permission denied` does, and counting it as a lost connection
    ///    would open the breaker on a host whose only problem is a key nobody installed.
    ///
    /// A step's retry is not itself subject to the breaker: it is the same call, and letting the
    /// breaker refuse the second half of it would replace *it did not answer* with a sentence
    /// about the host's history. **Both attempts are counted**, though — a step that retried once
    /// and failed twice is two connection failures, which is what the threshold's arithmetic
    /// (three attempts) is written against.
    ///
    /// **A refused mux session is retried whatever `retry` says** ([`Call::MuxRefused`]): the one
    /// thing `Retry::Never` exists to prevent is a second *effect*, and a session that never opened
    /// had none. That is R99's other half — the sentence ssh printed is not the command's own exit,
    /// and reporting it as one is how an `edit` through the placement came back *nothing was
    /// written* over a file at zero bytes.
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
        // **The things worth asking twice** — and both of them for the same reason: nothing on the
        // far side can have changed. Either no answer arrived at all (which the caller's policy
        // allows to be retried when its operation is idempotent), or no session was ever opened
        // (which allows the retry for any operation, because there was nothing to be at-most-once
        // about). A refusal is definite, so it is not retried — a key that is not authorised does
        // not become authorised by being offered again.
        let again = match &first {
            Call::Answered(..) | Call::Denied(..) => false,
            Call::MuxRefused(..) => true,
            _ => retry == Retry::Once,
        };
        if again {
            // The master may be what died, and a second attempt through the same dead socket would
            // fail the same way — `attempt` has already dropped a socket it was refused on. This
            // one is not a probe: the cooldown's one call was already spent.
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

    /// **One attempt, with the wall clock on it and a record of how it went.**
    ///
    /// The local ssh is killed at the deadline. What happens on the far side is [`Ssh::sh_run`]'s
    /// business (rule 6) or nobody's: a step of this backend's own work is a question, and a
    /// question that is abandoned leaves nothing to end.
    ///
    /// The call first takes a place in the host's queue ([`rpc::enter`], rule 8) — **after** the
    /// command is built, because building it may itself ask the host a question (where is mise?)
    /// and a slot held across another call's slot is how a queue becomes a deadlock. It gives the
    /// place up when it returns, whichever way it returns.
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
        // **The one place a call is written down** (rule 7), and [`Reported`] is the whole of the
        // policy: a heartbeat is only news when it fails.
        let note = |outcome: rpc::Outcome| {
            if reported == Reported::Always || outcome != rpc::Outcome::Ok {
                rpc::record(&self.host.name, step, began.elapsed(), outcome);
            }
        };
        let argv = self.argv(&command);
        let ended = match fed {
            // **The bytes go on stdin and nowhere near the argv** — a command line is capped
            // (`ARG_MAX`), which is why this used to be hundreds of calls of 48 KiB each and one
            // of them a refusal away from a half-written file (R99).
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
                // **The master may be the thing that stopped answering**, and a socket that is
                // still there would make the next call wait on it too.
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
                    // **A session the multiplexer refused is not the command's exit.** Classified
                    // before the catch-all, because the retry rule differs: `Dropped` is
                    // *may have run*, and this is *never ran* ([`mux_refusal`]).
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

    /// **A call nobody asked the breaker about and nobody will be told of**: the mise probe, and
    /// the kill that ends a cancelled command's remote group.
    ///
    /// It takes a slot and records itself like any other call — a remote call is a remote call —
    /// but it is deliberately outside the retry rule and outside the breaker's counting: it is
    /// infrastructure this backend uses to make the *real* call correctly, and a probe that fails
    /// must not be what opens a breaker.
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
                    // **A probe that was refused a session has not answered**, and the caller's
                    // cache rules already say such an answer is not remembered — which is why
                    // this arm exists here too: a `mise` probe reported as `Dropped` was fine by
                    // luck, and one reported as a *command that exited 255* would have been
                    // remembered as `no mise on that host`.
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
    /// what was done about it.
    fn no_answer(&self, step: &str, ended: &Call, within: Duration) -> String {
        if let Call::Denied(kind, stderr) = ended {
            return self.denied(step, *kind, stderr);
        }
        // **A refused session gets its own sentence**, and it is the one a reader needs: ssh's
        // words were not the command's, nothing ran, and the call was already asked again on a
        // fresh connection. Saying *lost the connection* here is what made R99 take a day to find.
        if let Call::MuxRefused(kind, stderr) = ended {
            return format!(
                "`{step}` on {} never started: ssh's multiplexer lost the session ({}) — ssh said \
                 `{}`. Nothing ran there, so nothing on that machine changed; the socket was \
                 dropped and the call was asked again on a fresh connection, which failed the same \
                 way. This is a refused session, not your command's exit status.",
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
        format!(
            "`{step}` on {} {how}: the local ssh was killed rather than waited on, and the call \
             was asked once more with a fresh connection. Nothing is left waiting — but the \
             operation may not have happened, so check before relying on it.",
            self.host.name
        )
    }

    /// **A step whose outcome nobody knows.** `run` and a job's start are the two operations that
    /// are not idempotent, so a call that died in flight is reported as exactly that — never as a
    /// failure, which would read as *it did not happen* and could be run again on that reading.
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

    /// **The sentence a caller that is not this file needs for a refusal** — `None` when the words
    /// are not one.
    ///
    /// `letibot --migrate` is the other caller of [`Ssh`], and it reached the same defect this file
    /// did: `Permission denied (publickey)` arrived as *lost the connection, it may not have
    /// happened* and was retried. The classification is here, once, so the two callers cannot
    /// disagree about what ssh's words mean.
    pub fn refusal(&self, step: &str, stderr: &[u8]) -> Option<String> {
        auth_refusal(&String::from_utf8_lossy(stderr)).map(|kind| self.denied(step, kind, stderr))
    }

    /// **A refusal that is an answer, not a lost connection.**
    ///
    /// `Permission denied (publickey)` and a host key that does not verify used to arrive as
    /// [`Call::Dropped`] — *lost the connection, it may not have happened* — and be **retried**,
    /// which was measured live on 2026-10-26 against three hosts whose key had not been installed:
    /// every call said the connection was lost and every call was made twice. Neither is a
    /// transport problem: the machine answered, and it will go on answering the same way until a
    /// key is installed. So it is its own outcome ([`rpc::Outcome::Refused`]), it is never
    /// retried, and the sentence names the one thing that fixes it.
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

    /// `ssh -O <verb>` for this host's `ControlMaster`, bounded.
    ///
    /// Every one of these goes through `-F /dev/null` like every other call: checking or dropping
    /// the master is still an ssh invocation, and one that read the operator's client config would
    /// be the hole in every other flag.
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

    /// **A stale master must not hang the next call.** A `ControlMaster` socket outlives the
    /// connection it belongs to (a `ControlPersist=120`, a crashed master, a network that went),
    /// and ssh will try to multiplex through it and fail — or wait — instead of dialling the host.
    /// So when the socket is there it is asked whether anything is home, with a few seconds on the
    /// question, and a socket nobody answers on is removed. It is ours, in letibot's own runtime
    /// directory, so nothing else is touched.
    fn fresh_master(&self) {
        if !self.control.exists() {
            return;
        }
        if self.master("check").is_some_and(|o| o.status.success()) {
            return;
        }
        let _ = std::fs::remove_file(&self.control);
    }

    /// **Drop the socket of a master that has just failed us — and do it without speaking to it.**
    ///
    /// `-O exit` is the polite way to end a master and it is what [`Ssh::exit_master`] does at the
    /// end of a session. It is the wrong move *here*: this is the path a call takes when the
    /// connection stopped answering, so asking that same connection to shut down cleanly waits out
    /// another `MASTER_BOUND` — measured, that made a 2 s bound cost 10 s, because every failed
    /// attempt paid it. Removing the socket is what actually stops the next call multiplexing
    /// through it; a master process left behind expires on its own `ControlPersist=120`.
    fn drop_socket(&self) {
        let _ = std::fs::remove_file(&self.control);
    }

    /// A file's bytes, base64 over the wire — **binary-safe**, which a `cat` is not
    /// once the connection is a pipe with a locale on it.
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

    /// **Written atomically, in ONE call, and only if the far side agrees about the bytes.**
    ///
    /// R99's third half. What was here sent the file as `ceil(len / 48 KiB)` separate ssh calls,
    /// each splicing a base64 slice into an argv, because a command line is capped (`ARG_MAX`) — a
    /// 19 MB bundle was **~400 calls**, and a placement on a busy host lost one of them part way
    /// and left `incoming.bundle.letibot-<pid>` at 2.8 MB. Two properties were worse than the
    /// cost: **every chunk's exit status was discarded**, and the `mv -f` that put the temp file in
    /// place was unconditional — so a chunk the far side failed to decode still moved whatever the
    /// temp file held over the target. A file at zero bytes with a tool result that said *nothing
    /// was written* (FB's note, `fanout.rs` twice) is that sequence.
    ///
    /// Now: the bytes go **on stdin of one ssh call**, the far side counts them and hashes them,
    /// and the `mv` is inside the same script after both checks. A stream that is cut — the ssh
    /// killed at the deadline, a session that went away — is short, the count catches it, the
    /// script removes its own temp file, and the target is byte-identical to what it was.
    ///
    /// **The retry is the whole write's, once** — idempotent by construction, since the temp name
    /// is this attempt's own and the target is only touched by the last `mv`.
    fn write_abs(&self, path: &str, bytes: &[u8]) -> Result<(), BackendError> {
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
        // **A temp name that belongs to this attempt and no other.** It used to be
        // `{path}.letibot-{pid}`, which is one name per path per PROCESS: two writes to the same
        // file at once — a retry started while the attempt it replaces was still running on the
        // far side — truncated each other's temp, and an `mv` from either moved whatever was in
        // it, including nothing. The counter makes the collision impossible rather than unlikely;
        // the checks below make a short temp un-movable even if one happened.
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

    /// **The wall clock one streamed write gets** — the connection's bound, plus a second for every
    /// 512 KiB it carries.
    ///
    /// A write is the one step whose cost is the payload's, and a single 20 s bound would have
    /// been a regression for the very case R99 came from: the 19 MB bundle used to be 400 calls of
    /// 48 KiB, each with its own `STEP_BOUND`, and one call streaming all of it is 58 s here — a
    /// 330 KB/s link. Every other step keeps exactly the bound it had.
    fn write_bound(&self, len: usize) -> Duration {
        self.bound + Duration::from_secs((len / (512 * 1024)) as u64)
    }

    /// **A step that is fed on stdin**, and the only one: [`Ssh::write_abs`].
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
                other => return Err(BackendError::Io(self.no_answer(what, &other, within))),
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

    /// Drop the ControlMaster, if one was made. Best effort: an ssh that says it has no
    /// master is not a failure of anything — and it is bounded, because a master that has
    /// stopped answering is exactly the case this is called in.
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

/// **The first line of something a machine said**, for a sentence that has to stay one sentence.
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

/// **Run a program to completion, or kill it at the deadline** — [`rpc::run_bounded`]'s question,
/// asked by the calls that only want an answer or nothing.
///
/// `None` is *the bound passed and it was killed*; a program that cannot be started at all is
/// also `None`, because every caller of this is asking a question it can do without
/// (`is the master alive?`, `drop the master`). The runner itself is one implementation for the
/// whole tree ([`rpc::run_bounded`]), so this file and `letibot --migrate` cannot drift apart on
/// what "bounded" means.
fn run_bounded(program: &Path, args: &[String], within: Duration) -> Option<std::process::Output> {
    match rpc::run_bounded(program, args, within) {
        rpc::Ended::Answered(out) => Some(out),
        rpc::Ended::TimedOut | rpc::Ended::Unstartable(_) => None,
    }
}

// ---------------------------------------------------------------------------
// What a host is doing, asked and never written
// ---------------------------------------------------------------------------

/// **What one probe of a remote host answered** — the live facts a placement decision needs.
///
/// Every field is what the machine itself said, and each one is optional or zero-able on
/// purpose: a host without `/proc` answers no memory, and a fact this box could not read is
/// reported as absent rather than as a zero that reads like a full disk.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Facts {
    /// `uname -n` — the machine's own name, which a `hostname = "10.0.0.7"` line does not tell
    /// you.
    pub uname: String,
    /// `nproc`, or the portable `getconf` fallback. 0 means neither answered.
    pub cores: u64,
    /// The three load averages as the machine printed them — a string because that is the shape
    /// the two platforms agree on, and a model reads it directly.
    pub load: String,
    pub mem_total_kb: Option<u64>,
    pub mem_avail_kb: Option<u64>,
    /// Which path the disk figure is about: the host's `root`, or its home when the root has not
    /// been made yet.
    pub disk_path: String,
    pub disk_free_kb: Option<u64>,
    /// Where `git` is, when the host's own `PATH` — with its `path` list in front — finds it. This
    /// is one of the two facts that decide whether a session can work there at all.
    pub git: Option<String>,
    /// Where `cargo` is on `PATH`. **Reported only as a fallback**: on a host with mise the
    /// toolchain is whatever mise pins, and `cargo` on a non-login `PATH` is not the question.
    pub cargo: Option<String>,
    /// **`mise --version`, first line**, when the host has mise at all — the fact that says the
    /// toolchains on this machine are pinned per project rather than installed globally.
    pub mise: Option<String>,
    /// **`mise ls --current`** in the host's root: one entry per tool, as `name version`.
    pub mise_tools: Vec<String>,
    /// **The confinement helper's version**, when the host has one at all — `bubblewrap 0.11.1`.
    ///
    /// A presence fact, and the cheap half of RH3's question. Whether the helper *works* cannot be
    /// read off a version: it is measured, by a placement, with the real argv and against that
    /// box's own namespace inodes — and that answer is cached per host for this process, so
    /// [`Facts::confine`] carries it once anything has asked.
    pub bwrap: Option<String>,
    /// **Whether a command on this host would run inside a boundary**, from the cache a placement
    /// fills: `confined (bubblewrap 0.11.1 …)`, or the measured refusal. `None` means *nothing has
    /// asked in this process*, which is said rather than guessed either way — a boundary nobody
    /// measured is exactly what RH3 took out of the banner.
    pub confine: Option<String>,
    /// **What this host's ssh calls have been doing**, as [`crate::rpc::report`] renders it — the
    /// last few and the slowest. Empty when nothing in this process has called the host yet.
    pub calls: String,
}

/// **The one script a probe runs**, and it only ever reads.
///
/// `key=value` lines rather than a formatted report, because the parser on this side then has no
/// opinions about the far side's locale, and every fact is one line whose absence is meaningful.
/// Nothing here writes, creates, moves or signals: a probe is a question, and the host it asks is
/// the operator's machine.
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

/// **Ask one host what it is doing, and never write there.**
///
/// The connection is [`Ssh::connect`]'s, so the two refusals it can give — a key that is missing
/// or inside the operator's own ssh directory, and a host key nobody has accepted — arrive as
/// sentences the caller can show, before anything is asked. Then one bounded call
/// ([`STEP_BOUND`], or `within` when the caller has a shorter budget for the whole probe), and
/// the facts are parsed out of it.
///
/// A host that does not answer within `within` is reported by the sentence [`Ssh::no_answer`]
/// gives, so a probe of four machines with one dead host takes a bound, not four.
///
/// The answer also carries **what this process's calls to the host have been doing**
/// ([`crate::rpc::report`]) — and so does a refusal, because a host that cannot be reached is the
/// one whose history the reader most needs.
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
            // **The records go out with the refusal.** A probe that failed is the one that has
            // something to say about why, and a caller that read only the sentence would be back
            // to guessing at a flaky host.
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
    // **What this process has already measured about a boundary on the host**, when anything has.
    // It is the cache a placement fills ([`CONFINE`]) and not a second probe: `hosts {probe}` is a
    // question about capacity, and the boundary question costs a full readback — so it is answered
    // here from the one place that already asked it, and its absence is reported as *nothing has
    // asked yet* rather than guessed at.
    facts.confine = confined_here(&host.name);
    Ok(facts)
}

/// **The boundary a host was measured with, in this process** — the cache, or `None`.
///
/// `Some("confined (…)")` is a boundary that really runs; `Some("nothing was confined — …")` is a
/// host that answered and cannot; `None` is *nobody has asked here yet*.
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

/// **The command line a confined remote spawn becomes** — the helper's argv, `--`, then the
/// command's own argv.
///
/// One function for both spawns (a foreground `run` and a job's launcher) because they differ in
/// nothing this cares about, and two spellings of it is how the *group kill* half gets lost: the
/// session is [`crate::exec::Session::Kept`] in both, so everything the helper starts stays in the
/// process group the remote side recorded, and `kill -TERM -<pgid>` reaches it.
///
/// **The refusal is returned, not swallowed.** A cwd outside the view — `bash {cwd: "/tmp"}` in a
/// session whose view is its checkout — makes [`crate::exec::Bwrap::remote_pieces`] refuse, and
/// the two answers available to a caller are *refuse the command* and *run it unconfined*; the
/// second is the boundary quietly not being there, which is the defect this module exists against.
/// So this returns the refusal and each caller turns it into its own error.
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

/// **One session's view on a host, and the boundary measured for it** — or the reason there is
/// none.
///
/// The view is the same policy a local confined session gets, spelled for the far machine: the
/// **checkout** read-write at its own path (so a compiler's error names the file the tools name),
/// the **system roots** read-only, the **toolchain** read-only from what the probe found, a
/// `$HOME` that is letibot's own directory on that host and not the operator's, and the session's
/// egress rule.
///
/// **Two views, one boundary.** The probe is run with the *provisional* view — the checkout, the
/// system roots and that home — because the probe's own argv needs binds and the toolchain list
/// is what the probe is there to discover. The measurement it returns is about the namespaces and
/// the two seals, which no bind can change; the grants are added afterwards. A bind that then
/// fails is loud rather than silent: the helper prints `bwrap: …` and
/// [`crate::exec::Confinement::launcher_failure`] reports *the boundary failed, this is not the
/// command's answer*.
fn confine_a_checkout(
    ssh: &Ssh,
    home: &str,
    worktree: &str,
    mirror: &str,
    egress: &crate::exec::Egress,
) -> (Option<crate::exec::Bwrap>, Option<String>) {
    use crate::exec::confine::{Grant, HomeView, ViewSpec};
    // **The mirror is part of the checkout, as git sees it.** The checkout is a `git worktree` of
    // the bare mirror beside it: its `.git` is a one-line pointer into
    // `<mirror>/worktrees/<short>`, and every object, ref and index lives in the mirror. A view
    // that bound only the checkout handed the session a tree with no repository — MEASURED
    // 2026-10-27, the first two packages placed after RH3 (`fatal: not a git repository`), one of
    // which built a fresh repository to have somewhere to commit. Writable, because a commit
    // writes objects and moves the branch's ref there, and it is this session's own mirror.
    let provisional = ViewSpec::project_only(worktree)
        .with_home(HomeView::Dir(PathBuf::from(home)))
        // **`/tmp` is this session's own directory on that machine** — created beside the home
        // by `place`, and bound at `/tmp` so a file one call writes is there for the next (see
        // `TmpView`). A fresh tmpfs per command is what a two-call scratch idiom silently
        // loses: a patch written by one call and applied by the next.
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
    // **What the host said is there, in the order [`crate::exec::confine::GRANT_CANDIDATES`] lists
    // it** — the probe answers in the shell's glob order, which is not an order a reader can
    // reason about, and a view whose `describe` lists `.gitconfig` before `.cargo` reads as though
    // something had been decided.
    let mut ordered: Vec<&String> = probe.toolchains.iter().collect();
    let rank = |p: &str| {
        crate::exec::confine::GRANT_CANDIDATES
            .iter()
            .position(|c| {
                let leaf = c.trim_start_matches("$HOME");
                p.ends_with(leaf)
            })
            .unwrap_or(usize::MAX)
    };
    ordered.sort_by_key(|p| rank(p));
    let mut view = provisional;
    for path in ordered {
        let why = if path.ends_with(".gitconfig") {
            "the identity a commit is recorded under — without it every `git commit` in this \
             view fails, and the branch is the whole deliverable"
        } else {
            "the toolchain on that machine, so a build does not re-download what is already \
             installed — READABLE INTO CONTEXT, and read-only: a new dependency cannot be \
             fetched into it from inside the boundary"
        };
        view = view.granting(Grant::ReadOnly {
            path: PathBuf::from(path),
            why: why.to_string(),
        });
    }
    match crate::exec::confine::measured_from_probe(&probe, view, egress.clone()) {
        Ok(b) => (Some(b), None),
        Err(e) => (None, Some(e.to_string())),
    }
}

/// **The environment that makes the operator's toolchain the one that runs**, when it is in the
/// view.
///
/// Measured 2026-10-26 on `lab2x1`: with `$HOME` a fresh directory, `cargo` is on `PATH` (the far
/// shell's keep-list) but `rustup` looks for its toolchain under `$HOME/.rustup` and answers
/// *rustup could not choose a version of cargo to run*. The toolchains are granted read-only at the
/// operator's paths, so the two variables are set to those paths and the toolchain works from a
/// home that carries nothing.
fn toolchain_env(confine: Option<&crate::exec::Bwrap>) -> Vec<(String, String)> {
    let Some(b) = confine else {
        return Vec::new();
    };
    let Some(boundary) = crate::exec::Confinement::boundary(b) else {
        return Vec::new();
    };
    env_for_grants(boundary.view.grants.iter().map(|g| g.path()))
}

/// The variables that point a tool at a granted path, since the view's `$HOME` is fresh.
fn env_for_grants<'a>(grants: impl Iterator<Item = &'a Path> + Clone) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (var, leaf) in [
        ("CARGO_HOME", ".cargo"),
        ("RUSTUP_HOME", ".rustup"),
        // mise keeps its installs and its state apart, and `mise` itself reads both.
        ("MISE_DATA_DIR", ".local/share/mise"),
        ("MISE_STATE_DIR", ".local/state/mise"),
        // Without it git reads the empty `$HOME` and commits under `user@host`.
        ("GIT_CONFIG_GLOBAL", ".gitconfig"),
    ] {
        if let Some(p) = grants.clone().find(|p| p.ends_with(leaf)) {
            out.push((var.to_string(), p.display().to_string()));
        }
    }
    out
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

// ---------------------------------------------------------------------------
// The backend
// ---------------------------------------------------------------------------

/// **How many commands this process has run on a remote host**, for the per-command name of the
/// file that records its process group. A counter rather than a timestamp: two runs in the same
/// millisecond would otherwise share a file, and one of them would then cancel the other's group.
static RUNS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

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
    /// **Where letibot's ssh material is**, when the caller says (a test's own home and config
    /// directory). `None` is [`Paths::from_env`] — the operator's, which is what every real
    /// session uses. A field rather than an environment variable for the reason
    /// [`crate::builtins::task::TaskTool`]'s `hosts` is one: a test that set `$HOME` would race
    /// every other reader of it in the same process.
    pub paths: Option<Paths>,
    /// **The wall clock every call that is not a command is given**, when the caller names one.
    /// `None` is [`STEP_BOUND`]. A test's door: a fake ssh that never answers should be measured
    /// in a second rather than in twenty.
    pub bound: Option<Duration>,
    /// **The session's network rule, as the confined view expresses it.** A boundary denies the
    /// network by default (`Egress::Denied`, which is [`crate::exec::Bwrap::project`]'s answer and
    /// the local Layer 1 default); a point that seats network calls passes
    /// [`crate::exec::Egress::Host`] with the sentence that decided it, and the boundary shares
    /// that machine's network instead of taking it away.
    ///
    /// It is a field of the SPEC rather than a constant because the two answers are a decision
    /// somebody makes above this file: a session whose mode admits `web_fetch` must not have its
    /// `curl` fail at the socket after the gate said yes, and a session whose mode does not must
    /// not be able to reach out because a sandbox defaulted the other way.
    pub egress: crate::exec::Egress,
    /// **Whether to ask this host whether it can confine** — `None` (production) asks, and is the
    /// only value a real caller passes.
    ///
    /// `Some(false)` is a fixture's door and it exists for the reason `mise = false` does: the
    /// probe runs ON THE MACHINE BEHIND THE FAKE, which is the machine running the test, so the
    /// same test would take two different paths on two boxes — confined on a Linux host with
    /// `bwrap`, unconfined on a Mac without one — and an assertion about the shape of a command
    /// line would mean two things. `Some(true)` is the other half: a test that means *this host
    /// confines* says so, and the probe still has to answer.
    pub confine: Option<bool>,
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
    /// **The boundary this session's commands run inside, when there is one** — measured on the
    /// host by [`Ssh::confinement_probe`] and applied to this session's own view. `None` with
    /// [`SshBackend::unconfined`] set is a host that cannot confine, and that is a fact the mode,
    /// the banner and every card have to be able to read.
    confine: Option<crate::exec::Bwrap>,
    /// **The view, kept beside the confinement** so the file tools can ask the same question a
    /// command's view answers — *is this path in here at all* — without cloning a `Boundary` on
    /// every call.
    view: Option<crate::exec::ViewSpec>,
    /// The environment a confined command carries so that the *operator's* toolchain is what runs
    /// — `RUSTUP_HOME` and `CARGO_HOME` at the read-only grants. Without these a confined `cargo`
    /// looks for its toolchain under the session's own (empty) `$HOME` and answers *rustup could
    /// not choose a version of cargo to run*, measured 2026-10-26 on `lab2x1`.
    confined_env: Vec<(String, String)>,
    /// Why it is not confined, when it is not: the probe's own reason.
    unconfined: Option<String>,
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
        let host = spec.host.name.clone();
        // **The connection is opened first, and the two refusals it can give are the two that
        // have to happen before anything is arranged**: a key that is missing or inside the
        // operator's own ssh directory, and a host key nobody has accepted for letibot. A
        // placement that discovered either of them half way would have a mirror on the far side
        // and no way to say which step refused.
        let mut ssh = Ssh::connect(
            spec.host.clone(),
            spec.ssh.clone(),
            spec.paths.clone().unwrap_or_else(Paths::from_env),
        )
        .map_err(|why| BackendError::Io(format!("placing this session on {host}: {why}")))?;
        if let Some(within) = spec.bound {
            ssh = ssh.with_bound(within);
        }
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

        // **`mise exec -C` needs a directory that EXISTS**, and the checkout below does not exist
        // until `git worktree add` has run — so the wrapper is pointed at the root until then. It
        // is not a detail: mise refuses a missing `-C` (measured, 2026-10-26 —
        // `mise ERROR Directory specified with --cd does not exist: …`, exit 1), which on every
        // machine whose toolchain comes from mise failed the placement at `making the mirror`.
        // mise prints the path it was given with `$HOME` abbreviated to `~`, which is what made
        // the refusal read like a quoting bug.
        ssh = ssh.with_dir(root.clone());

        // **A bare mirror, made COMPLETE by whichever placement gets there first.** Reused by
        // every later session onto the same repository, so a second child on the same host sends
        // only its own commits.
        //
        // The answer is the directory's `objects/`, not `git init`'s exit status, and that is the
        // whole of it: three placements starting together on one host each tested `[ ! -d ]` and
        // then ran `git init --bare` on the same path, and **two of the three exited 128** in
        // every one of thirty measured rounds (MEASURED 2026-10-28, `git init --bare` three at
        // once, one path). A loser also leaves the directory behind, so the next placement's
        // `[ ! -d ]` skips the init and the fetch that follows fails on a directory that is not a
        // repository — the same sentence the operator saw, *handing the bundle to the remote
        // repository*, from a race that finished minutes earlier.
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

        // **The tree itself, as a bundle**, written into the mirror where the remote `git`
        // can read it. `git bundle` fails on a repository with no commits, and that
        // failure is worth its own sentence: an empty repository is a real answer.
        //
        // The staging file is unique per call — the pid, a per-process counter and a timestamp —
        // because two sessions placing at once in one daemon would otherwise write one bundle and
        // meet each other's `.lock`. **The counter is the part that guarantees it.** A clock alone
        // did not: macOS's has microsecond resolution, and two placements in one test binary got
        // the same name (MEASURED 2026-10-27, the Mac gate: `….bundle.lock': File exists`).
        //
        // **The same counter names the copy that goes over**, and that is R101: the far side's
        // name used to be `{mirror}/incoming.bundle` — one name per MIRROR, so every placement of
        // one repository on one host wrote and fetched through the same file. Two at once and the
        // fetch that landed second read the first one's bytes: the ref it creates is its own, so
        // nothing fails — the session is simply handed a checkout of somebody else's commit
        // (MEASURED, and forced by `two_placements_at_once_never_read_each_others_bundle`). A
        // mirror that kept every bundle also kept 19 MB per placement for ever.
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
        let incoming = format!("{mirror}/incoming-{}-{}-{}.bundle", std::process::id(), run, short);
        ssh.write_abs(&incoming, &bundle).map_err(place)?;
        let staged = format!("refs/heads/letibot/{short}");
        ssh.must(
            "handing the bundle to the remote repository",
            &format!(
                // **The fetch's own words, and the bundle removed only after it.** `2>&1` into
                // /dev/null was why the operator's failure said *handing the bundle to the remote
                // repository* and nothing else — no branch, no reason. And a step that is asked
                // again (this one is, when a call did not answer) must not fail because the first
                // attempt already consumed the file: gone means the ref is what is left to check.
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

        // **The checkout.** A second session on the same id (a resume) finds its worktree
        // already there and keeps it — the branch and everything committed on it.
        let existing = ssh
            .sh_step(
                "checking whether the checkout is already there",
                &format!("[ -d {} ] && echo yes", quote(&worktree)),
            )
            .map_err(place)?
            .0;
        if !existing.trim().eq("yes") {
            // **Not retried**, and this is the one arranging step where that matters: `git worktree
            // add -b` is not idempotent, so a second attempt after a call that may have worked
            // would fail with *already exists* and report a placement that is in fact fine. A
            // timeout here is reported as it is (see [`Ssh::no_answer`]).
            ssh.must_within(
                "checking the branch out",
                &format!(
                    "git -C {m} worktree add -b {br} {wt} {st} >/dev/null",
                    m = quote(&mirror),
                    br = quote(&branch),
                    wt = quote(&worktree),
                    st = quote(&staged)
                ),
                Retry::Never,
                ssh.bound(),
            )
            .map_err(place)?;
        }

        // **Now the checkout exists, so the wrapper can be pointed at it.** A project pins its
        // versions in a `mise.toml` in ITS tree, so `-C` has to be the tree the work is in and not
        // the root; but it can only be named once `git worktree add` (or a resumed session's own
        // checkout) has made it.
        ssh = ssh.with_dir(worktree.clone());

        // **The session's own job directory is not part of the tree.** `.letibot-remote/` holds a
        // background job's launcher, output, pid and exit file, so it appears the moment the
        // session runs anything in the background — and it appeared on close as
        // `?? .letibot-remote/`, a change the operator was told was theirs to look at. It is
        // ours, so the checkout's own exclude file says so, `git` answering where that file is
        // (`--git-path`, which relocates through a linked worktree exactly as git's own read
        // does) rather than this side guessing a path that differs per git version.
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

        // **RH3: can this machine bound a command, and if so in what view.** Asked once per host
        // per process ([`CONFINE`]), after the checkout exists because the checkout is the
        // writable root of the view.
        //
        // A machine that cannot confine is **not** a failed placement: the session runs as it did
        // before RH3 — unconfined, every call a decision — and says so, in `describe` and in the
        // note this returns. What it must never do is *claim* the boundary: `backend_confined`
        // is read off this answer by `harnessd`, so a host whose probe failed no longer counts as
        // a confinement for the mode's prerequisite or for a card's facts. That claim was the
        // live defect RH3 fixes — the remote arm of `Harness::open` hardcoded it to `true`.
        let mut home = None;
        let (confine, unconfined) = if !spec.exec {
            // **Nothing runs here, so there is nothing to bound.** A downgraded child seats no exec
            // tools, and probing a host for a boundary no command will use is a round trip spent
            // on a question nobody asked.
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
                    // **The session's home, and its `/tmp` beside it.** A confined command's
                    // `/tmp` was a fresh tmpfs there too, so a file one call wrote was gone in
                    // the next (see `TmpView`). `dir/tmp` is the same lifetime as `dir` — both
                    // are letibot's own directories on that machine, both are made here, and
                    // neither is removed when the session ends, because the checkout, its
                    // commits and what a failed session left in its home are what an operator
                    // goes looking for afterwards.
                    ssh.must(
                        "making the session's own home and its /tmp on that machine",
                        &format!("mkdir -p {} {}", quote(&dir), quote(&format!("{dir}/tmp"))),
                    )
                    .map_err(place)?;
                    let measured = confine_a_checkout(&ssh, &dir, &worktree, &mirror, &spec.egress);
                    home = Some(dir);
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
                home.as_deref().unwrap_or("the session's own directory")
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
            view: confine
                .as_ref()
                .and_then(crate::exec::Confinement::boundary)
                .map(|b| b.view.clone()),
            confine,
            confined_env,
            unconfined,
            brought_back: Mutex::new(None),
        })
    }

    /// The host's name, for a card, a progress line or a test.
    pub fn host_name(&self) -> &str {
        &self.host
    }

    /// **The boundary this session's commands run inside**, when there is one.
    ///
    /// The measured view, so a caller can answer *is this path inside it* and *what is the
    /// disclosure* from the same fact that produced the argv — rather than from a second
    /// opinion about what the confinement is.
    pub fn confinement(&self) -> Option<crate::exec::Boundary> {
        self.confine
            .as_ref()
            .and_then(crate::exec::Confinement::boundary)
            .cloned()
    }

    /// **Why a command on this host is not confined**, when it is not: the probe's own reason,
    /// with the host in it.
    pub fn unconfined_reason(&self) -> Option<&str> {
        self.unconfined.as_deref()
    }

    /// Is a command run by this backend inside a measured boundary? `false` for a host whose
    /// probe failed, which is the fact `harnessd` reads as `backend_confined` and the gate reads
    /// for its boundary facts.
    pub fn is_confined(&self) -> bool {
        self.confine.is_some()
    }

    /// **The script a command becomes**, confined where there is a boundary: the helper's argv,
    /// then `--`, then the command's own `argv`.
    ///
    /// The argv is spliced rather than wrapped in another shell: `Command::argv` is already the
    /// shell the caller chose (`/bin/sh -c …`), so a second one would change what `$0` and the
    /// quoting mean.
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

    /// **What the placement had to say** — the worktree, the branch that comes back, and
    /// the local edits that were left behind. Printed by the opener, not by the session.
    pub fn placement_notes(&self) -> &[String] {
        &self.notes
    }

    /// **Is this path in the view at all?** — the question a command's own sandbox answers, asked
    /// by the file tools.
    ///
    /// `Ok(abs)` when there is no view (nothing was confined, so nothing is claimed) or when the
    /// path is inside it; [`BackendError::Outside`] when it is not. **This is what makes RH5's
    /// refusal hold on a placed session**: that rule resolves a path on *this* box, so a path on
    /// the far machine — the operator's `~/.ssh` over there — resolves to nothing here and would
    /// pass it. A view has no such trouble: that directory is not in it, so it is not there.
    ///
    /// It is the VIEW's rule, not the checkout's: `/etc/passwd` is bound read-only into a command's
    /// view, so `read` reaches it too — what a command can see, the tools see, which is one answer
    /// to one question rather than two.
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
        // `git fetch` over ssh, with **this backend's ssh**: the same flags, and the **git**
        // ControlMaster — not the one the calls are multiplexed on. git starts that ssh itself, so
        // it takes no slot in the gate that keeps a daemon's calls under `MaxSessions`; giving it a
        // socket of its own is what stops a fetch being the ninth session on a master whose eighth
        // was promised to a call ([`Ssh::git_flags`]).
        let ssh_command = std::iter::once(self.ssh.program().display().to_string())
            .chain(self.ssh.git_flags())
            .map(|a| quote(&a))
            .collect::<Vec<_>>()
            .join(" ");
        let fetched = {
            let mut argv: Vec<String> = vec![
                "-C".into(),
                self.local_repo.display().to_string(),
                "-c".into(),
                format!("core.sshCommand={ssh_command}"),
                "fetch".into(),
                "--no-tags".into(),
            ];
            // **The far side's `git-upload-pack` is a remote command**, and ssh runs it as
            // `<this> '<path>'` — a login shell does not read a profile for it, so a host whose
            // `git` is not on the default `PATH` needs the prefix here too, and a host whose
            // toolchain is pinned by mise needs the same wrapper every other remote command gets
            // ([`Ssh::upload_pack_prefix`]). `--upload-pack` is the only door git gives for it,
            // and its argument is spliced into the remote command line, so it is a **prefix
            // assignment** rather than an `export`.
            if let Some(prefix) = self.ssh.upload_pack_prefix() {
                argv.push("--upload-pack".into());
                argv.push(format!("{prefix} git-upload-pack"));
            }
            argv.push(url.clone());
            argv.push(refspec.clone());
            // **Bounded, because this fetch's transport is an ssh connection** ([`GIT_BOUND`]):
            // a close that waits for ever on a dead host is the same defect as a `read` that does.
            run_bounded(Path::new("git"), &argv, GIT_BOUND)
        };
        let mut said = match fetched {
            Some(out) if out.status.success() => {
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
            Some(out) => format!(
                "the session on {} ended and its branch did NOT come back: `git fetch` said \
                 \"{}\". The work is still there, in {} on that machine (branch `{}`) — reach \
                 it with `git fetch {} {refspec}` once the cause is fixed",
                self.host,
                String::from_utf8_lossy(&out.stderr).trim(),
                self.worktree,
                self.branch,
                url
            ),
            None => format!(
                "the session on {} ended and its branch did NOT come back: `git fetch` got no \
                 answer within {} s and was killed, so the connection is the suspect. The work is \
                 still there, in {} on that machine (branch `{}`) — fetch it yourself once the \
                 host answers: `git fetch {} {refspec}`",
                self.host,
                GIT_BOUND.as_secs(),
                self.worktree,
                self.branch,
                url
            ),
        };
        // **What was left uncommitted is not in that branch.** A session that ends with a
        // dirty tree has work the branch does not hold, and a report that named only the
        // branch would be a false "it is all here".
        //
        // **`.letibot-remote/` is not the session's work.** It is this backend's own job
        // directory — a launcher, its output, its pid, its exit status — and a close that counted
        // it as an uncommitted change told the operator to go and review files nothing typed. The
        // checkout's exclude file makes `git status` leave it out ([`SshBackend::place`]); this
        // filter is the belt beside it, for a checkout made by a build that predates that.
        if let Ok((dirty, _, 0)) = self.ssh.sh_step(
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
    git_within(repo, args, GIT_BOUND)
}

/// **One local `git` call with a wall clock on it**, for the steps that can reach another machine:
/// `bundle create` on a large repository, and the `fetch` whose transport is ssh.
///
/// A `git` left waiting on a dead connection holds the op that asked for it — the placement, or the
/// session's close — so it is killed at the deadline like everything else here, and the sentence
/// says which step it was.
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
        // **Rule 6: the command's remote process group is recorded as it starts**, in this
        // session's own directory on that machine, so a command that has to be abandoned can be
        // ended *there* rather than only by closing the local ssh. The name carries this
        // process's id and a counter, so two runs cannot overwrite each other's group — a stale id
        // is a kill pointed at whatever now owns it.
        let pgid = format!(
            "{}/.letibot-remote/run-{}-{}.pgid",
            self.worktree,
            std::process::id(),
            RUNS.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        // **`run` is the one call that keeps the tool's deadline and nothing else.** The command
        // may be a build, and this file has no idea how long that takes; what this file does know
        // is that a command which loses its connection is not a failure — it may be running still
        // — so it is reported as an unknown outcome and never re-sent.
        //
        // **RH3: the command runs inside the boundary**, in the same view the probe measured — the
        // checkout writable, the toolchain read-only, the rest of that machine absent. The
        // `cd`/`ENV=` shape is what an unconfined command keeps, and it is not an alternative to
        // the confined one: a boundary's `--chdir` is the `cd`, and its `--clearenv`/`--setenv` is
        // the environment, so splicing both would set the variables twice in two places.
        let script = match &self.confine {
            Some(_) => self
                .confined_script(&cwd, &cmd.env, &cmd.argv)
                .map_err(|e| BackendError::Io(e.to_string()))?,
            None => format!("cd {} && {env_prefix}{joined}", quote(&cwd)),
        };
        let (out, _stderr, exit) = self.ssh.sh_run("this command ran", &script, Some(&pgid))?;
        Ok(Output {
            stdout: out.into_bytes(),
            // Merged by ssh itself — see `Ssh::remote_command`.
            stderr: Vec::new(),
            exit,
        })
    }

    fn read(&self, path: &str) -> Result<Vec<u8>, BackendError> {
        self.ssh.read_abs(&self.in_view(path)?)
    }

    fn write(&self, path: &str, bytes: &[u8]) -> Result<(), BackendError> {
        if !self.writable {
            return Err(BackendError::Unsupported(
                "this subagent was downgraded to no write; nothing on its host changed",
            ));
        }
        self.ssh.write_abs(&self.in_view(path)?, bytes)
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

    /// **What this backend will call the root of the boundary** — the checkout, when a boundary
    /// was measured, and nothing when it was not.
    ///
    /// Two different questions meet here and the docs of both are worth keeping apart.
    /// `root_path` answers *is this path inside the boundary*, and for a substrate that only knows
    /// a root the honest answer is the writable one. **The narrower truth is
    /// [`Self::in_view`]**: the view holds the system roots and the toolchain read-only as well,
    /// so the file tools admit more than this path and refuse what the view does not contain. This
    /// method is what a caller that has only one path to print gets.
    ///
    /// Unconfined it is `None`, because inventing `/home/dead/letibot-remote` here would make the
    /// gate answer confidently about a filesystem it is not looking at.
    /// [`Self::workspace_path`] is the other question — *which directory is this session working
    /// in* — and that one has an answer either way.
    fn root_path(&self) -> Option<String> {
        self.confine.as_ref().map(|_| self.worktree.clone())
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
                   if [ -d \"$f\" ]; then printf 'd\\t0\\t%s\\n' \"$f\"; \
                   else printf 'f\\t%s\\t%s\\n' \"$(wc -c < \"$f\")\" \"$f\"; fi\n\
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
                       if [ -d {q} ]; then printf 'd\\t0\\n'; \
                       else printf 'f\\t%s\\n' \"$(wc -c < {q})\"; fi; \
                     else exit 1; fi"
                ),
            )
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
        // **What the boundary IS, or why there is none** — never a fixed sentence. `NOT CONFINED`
        // was true until RH3 and is now one of two answers, and the old constant is exactly the
        // shape `Boundary::describe`'s own doc warns about: a banner that asserts what it did not
        // measure. `bwrap` was measured on the host, so the first arm prints the view that runs.
        let boundary = match (&self.confine, &self.unconfined) {
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
    /// The boundary this session's jobs start inside — the same one its commands use.
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
            confine: None,
            confined_env: Vec::new(),
        }
    }

    /// **Put every job of this session inside the session's own boundary**, and give it the same
    /// environment a command of the model's gets — the operator's toolchain at its read-only
    /// grants.
    ///
    /// A job and a foreground command are the same spawn here: both go through
    /// [`wrap_confined`], so a job cannot be the door that runs unconfined in a session whose
    /// commands are confined.
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

    /// **Ask the files what happened**, and record it. One round trip per job: the exit
    /// status if there is one, whether the pid is still alive, and how much output there
    /// is.
    fn refresh(&self, job: &RemoteJob) {
        self.refresh_within(job, self.ssh.poll_bound());
    }

    /// [`RemoteProcesses::refresh`] with the caller's remaining time — so a `job_wait` honours
    /// its own deadline even when the host has stopped answering.
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
        // **A poll, so [`POLL_BOUND`] and one attempt**: a status question whose answer arrives
        // after the next tick has already been superseded, and asking it twice holds the caller's
        // deadline open for twice as long as it should.
        let Ok((line, _, 0)) = self
            .ssh
            .sh_poll_within("asking about a job", &script, within)
        else {
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
            // **A second look at the status file, before this is called a kill.** The poll asked
            // the exit file first and the pid second, and between the two there is a fork (the
            // `cat`) and a round trip — so a job that finished while the question was in flight
            // answers *status empty* at the first read and *pid gone* at the second, which reads
            // exactly like a job something killed. Measured 2026-10-27 on the Mac at `bb66c6b1`,
            // `cargo test -p letibot-tools --test scenario`: the `client` role of
            // `after_orders_the_starts_and_a_roles_facts_resolve` finished (its 19 bytes of
            // output were there, and its own `.exit` file says `0`) and the row still read
            // *killed by something on the remote ended it and it wrote no exit status* — one run
            // in six with its siblings beside it, never alone.
            //
            // The launcher writes the status BEFORE it exits, so a pid that is gone cannot be
            // about to write one: an empty answer here is the whole truth, and a status found
            // here is the job's own ending.
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
            // **The output of a job here is already a file, and it is on the other machine.**
            // [`crate::exec::OutputFile`] is a path a reader on THIS box can open — `read`,
            // `tail` — so a remote job's `ssh://host:/…/j7.out` in this field would be a locator
            // that fails the one call it suggests. The promotion's own note names it instead, and
            // `job_output` reads it over the connection.
            output_file: None,
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
        //
        // **RH3: inside the boundary when there is one**, and still a child of this launcher —
        // which is `setsid`'d, so the sandbox is in the job's process group and `job_kill`'s
        // group kill reaches it. The command is handed to `/bin/sh -c` exactly as before, so a
        // job means what it meant; only the shell it runs in is the sandbox's.
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
            .sh_step(
                &format!("making {jobs}"),
                &format!("mkdir -p {}", quote(&jobs)),
            )
            .map_err(|e| ExecError::Spawn(format!("making {} on {}: {e}", jobs, self.host)))?;
        // **A job's start is not idempotent, so it is asked exactly once.** The launcher has
        // already been written; a start that loses its connection may have started the job, and
        // running it again would be a second copy of the operator's work. The sentence the failure
        // carries says so ([`Ssh::unknown`]).
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
        // **A group kill only when the pid really leads a group.** `kill -TERM -<pid>`
        // on a pid that is not a process-group leader signals *this* shell's group, and
        // that is the one mistake here that could take out something the operator cares
        // about. `ps -o pgid=` answers it, on the remote, before the signal.
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
        // **Who is ending it, recorded before the signal** — the field's own purpose (it is how a
        // deadline kill is said as one), asked here of an explicit kill too. The settle loop below
        // takes a poll while the signal lands, and without this a `job_kill` this daemon sent is
        // recorded as *something on the remote ended it*: a mystery where the truth is the
        // caller's own sentence.
        *job.armed_kill.lock().unwrap_or_else(|e| e.into_inner()) = Some(by.to_string());
        // **A signal is not idempotent here in the sense that matters** — sending `kill -TERM`
        // twice to a group is harmless, but a second one after a timeout could reach a pid the
        // first one ended and something else reused. Asked once, and a failure is reported by the
        // read-back below rather than papered over.
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
        // **Who is left**, read back the way it was read before. And when the read-back does not
        // come, that is said rather than counted as a clean kill: `removed` means *this daemon
        // looked and found nothing*, and a host that never answered has not been looked at.
        match self.ssh.sh_step(
            &format!("reading back whether pid {pid} is gone"),
            &format!("kill -0 {pid} 2>/dev/null && echo alive"),
        ) {
            Ok((said, _, _)) => {
                // The script's own protocol: it says `alive` when `kill -0` succeeded, and exits
                // non-zero when it did not — so an exit status here is *still an answer*.
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

    /// **Where the whole of a job's output is on that machine** — the job directory's `<n>.out`,
    /// which is the path the supervisor reads and the exit file's name is derived from. This box
    /// cannot open it, which is why it is not [`JobView::output_file`].
    fn job_log(&self, job: &JobId) -> Option<String> {
        self.find(job).map(|j| j.out.clone())
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
    ///
    /// **And its output was never only in memory, which is the other half of the promise.**
    /// [`RemoteProcesses::launch`] redirects the command into `{jobs}/{n}.out` on the host
    /// before it starts, so everything a promoted run prints is already in a file there — the
    /// note names it, and `job_output` reads it back over the connection (a `tail -c` from the
    /// offset the caller asked for). There is nothing for this method to start doing.
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
path     = ["~/.cargo/bin", "/opt/bin"]
root     = "~/letibot-remote"

[host."bench-2"]
hostname = "10.0.0.7"
user     = "root"
"#;

    /// A test's own home and config directory, with a key for `host` and its host key already
    /// accepted — the two facts [`Ssh::connect`] refuses without.
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

    /// **A `~` in the wrapped directory is expanded on the REMOTE, and never left quoted.**
    ///
    /// `mise exec -C` resolves a literal `~` against the directory it happens to be in, so a
    /// quoted tilde is a `-C` that names the wrong tree — or none. The same rule `path`'s entries
    /// already follow ([`one_entry`]), asked of `-C`; measured 2026-10-26 after a gate run on a
    /// host with a real mise.
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

    /// **A refused mux session is read as transport, and nothing else is.**
    ///
    /// Four sentences were taken from OpenSSH's own source and its bug reports, and each one has to
    /// be recognised for what it is: *the connection is up and this call did not get a channel*. The
    /// negative half matters as much — `Permission denied` is a definite refusal (`auth_refusal`
    /// takes it first) and `Could not resolve hostname` is a host that is not there, and neither may
    /// be retried under *nothing ran*.
    ///
    /// Measured 2026-10-27: `mm_send_fd: sendmsg(1): Message too long` is the sentence an `edit`
    /// through the git placement died of, and it arrived at the model as *lost the connection*.
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

    /// **Every daemon gets its own master, and the socket fits.**
    ///
    /// R99: the name used to be the host's alone, so every daemon on the box multiplexed over one
    /// connection and `MaxSessions` was divided among all of them. The pid in the name is the fix;
    /// git's socket beside it is what keeps a `git fetch` — an ssh this process never starts and so
    /// cannot count — off the master the calls are bounded on.
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

    /// **The ssh argv is the policy's, and nothing in it names the operator's ssh material.**
    /// The assertions about what is *missing* are the point: a list with `-i` and no
    /// `IdentitiesOnly` looks complete.
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

    /// **Two refusals, before anything is arranged**: a key that is not there (or is inside the
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
        // **The key left to its default**, which `paths_for` made in this test's own home: the
        // fixture's `~/…` key is expanded against the REAL `$HOME` when the file is parsed, so
        // this check passed only on a box where that real key happened to exist (the operator's
        // Mac) and failed on lab2x1, the first Linux gate to run it (2026-10-26).
        let why = Ssh::connect(Host { key: None, ..host }, None, paths.clone())
            .expect_err("an unknown host key");
        assert!(why.contains("ssh-keyscan -t ed25519"), "{why}");
        assert!(why.contains("Nothing was connected"), "{why}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// **A host's `path` list is in front of every remote command**, and a host without one runs
    /// exactly the command it always did.
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

    /// **The file's refusals**: a key it does not have, a key stated twice, a `path` that is not
    /// a list, and a host that omits `key` getting the dedicated default.
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
        // **The three keys a shared host needs default to *nothing held back*** — which is the
        // safe direction: a host nobody declared anything for behaves exactly as it did before
        // this package, and only a file that says otherwise changes a teardown's reach.
        assert_eq!(a.reserve, 0);
        assert!(a.flags.is_empty());
        assert!(a.never_touch.is_empty());
    }

    /// **`reserve`, `flags` and `never_touch`** (RH9) — what the operator says about sharing a
    /// machine, read strictly like every other key: a number, two lists, and nothing guessed.
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
        // **A substring, case-insensitively** — the operator declared the workload and not its
        // spelling, and matching less than they meant is the failure this list exists to prevent.
        assert_eq!(a.never_touch_hit("pgfuzz -t 3600"), Some("pgfuzz"));
        assert_eq!(a.never_touch_hit("/usr/bin/ETCD-fuzz"), Some("etcd"));
        assert_eq!(a.never_touch_hit("cargo test --offline"), None);
        // **`cores − load − reserve`**, and a negative is reported rather than clamped.
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

    /// **A link into the ssh directory is the ssh directory**, after every symlink and for a
    /// path that does not exist yet — the rule is about the bytes, not the spelling.
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

        /// **The ssh, answering three commands and running everything else here.** The poll is
        /// answered from [`PollRig::gone`], the status from [`PollRig::status`], and the launch is
        /// acknowledged without starting anything — a real launcher would write files of its own
        /// and there would be nothing left for the answers to be about.
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

        /// **A host wired to this rig**, and the job table over it.
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

    /// **A job whose pid is gone, as a view of it** — started (so it has a pid), then the pid is
    /// reported gone, then asked. The status file is whatever the rig was built with.
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
        // **The finish the poll used to read as a kill**: the status file is already written and
        // the pid is gone by the time the next question is asked.
        std::fs::write(&rig.gone, b"").expect("the pid is gone");
        host.job(&job).expect("the job")
    }

    /// **A poll that comes back *no status and the pid gone* reads the status file once more**,
    /// and the window is forced here rather than raced.
    ///
    /// The two answers the poll gives are read in one round trip, and between them sits a fork (the
    /// `cat`) — so a job that finishes while the question is in flight answers *no status* at the
    /// first read and *pid gone* at the second, which is exactly what a job something killed answers
    /// too. Measured 2026-10-27 on the Mac at `bb66c6b1` in `cargo test -p letibot-tools --test
    /// scenario`: one run in six, the `client` role of
    /// `after_orders_the_starts_and_a_roles_facts_resolve` was reported as *killed by something on
    /// the remote ended it and it wrote no exit status* with its own `.exit` file saying `0`.
    ///
    /// The launcher writes the status BEFORE it exits, so a pid that is gone cannot be about to
    /// write one: the second look is the whole difference between a finished job and a killed one.
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

        // **And an empty status file is still a killing**: the second look must not turn every
        // dead pid into a graceful exit.
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
        let env = env_for_grants(grants.iter().copied());
        assert!(env.contains(&("GIT_CONFIG_GLOBAL".into(), "/home/dead/.gitconfig".into())));
        assert!(env.contains(&("CARGO_HOME".into(), "/home/dead/.cargo".into())));
        let none = env_for_grants([Path::new("/home/dead/.cargo")].iter().copied());
        assert!(none.iter().all(|(k, _)| k != "GIT_CONFIG_GLOBAL"));
    }
}
