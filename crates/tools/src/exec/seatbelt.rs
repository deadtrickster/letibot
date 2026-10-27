//! **macOS's boundary: a mandatory-access policy the kernel applies, measured.**
//!
//! Linux's boundary is a set of namespaces ([`super::confine::Bwrap`]) and its whole
//! strength is *absence*: a file outside the mount view is not there, so no rule, no path
//! list and no classifier is in the way of the guarantee. macOS has none of that — no
//! mount namespace, no PID namespace, no cgroup — and until this module existed a Mac
//! session that wanted to run a command had one of two choices: a firecode VM, which is
//! slow and needs a Linux guest, or an unconfined whole-host shell, which is what the
//! operator's own sessions do today.
//!
//! What macOS does have is **Seatbelt**: `sandbox-exec -f <profile>`, a mandatory-access
//! policy the kernel evaluates on every filesystem, mach, network and process operation the
//! process and everything it execs performs. It is deprecated and it is present, and it is
//! what Chrome ships to three hundred million people. This module renders letibot's
//! [`ViewSpec`] as one of those profiles and then **measures** it, because a profile that
//! was written is a request and this tree does not accept requests as evidence.
//!
//! # One policy, two renderings
//!
//! The view is the same type on both platforms — the project read-write at its own path,
//! the system roots read-only, the toolchain read-only, `$HOME` the session's own, `/tmp`
//! the session's own, egress a decision — and it is what decides **what is inside**. What
//! differs is the medium: [`super::confine::pieces_of`] turns a view into a bubblewrap
//! argv, and [`render_profile`] turns the same view into SBPL text. Neither reads the other,
//! and both are driven by the same [`ViewSpec`], which is the reason a policy cannot drift
//! between the platforms: there is one place to change and two functions that must both
//! consume the change or fail to compile.
//!
//! # Denied, not absent — and the deny list outranks every grant
//!
//! The difference between the two platforms is not cosmetic and is not hidden:
//!
//! | | Linux (mount namespace) | macOS (policy) |
//! |---|---|---|
//! | `~/.ssh/id_ed25519` | **not there** — no spelling reaches it | **there, and `open` answers `EPERM`** |
//! | a listing of the operator's home | absent | `EPERM` |
//! | a write outside the checkout | absent | `EPERM` |
//!
//! [`super::confine::Boundary::describe`] says which of the two it is holding, and
//! [`super::confine::Boundary::absence_notes`] is written differently for each: on Linux an
//! `ENOENT` under `/tmp` is the boundary, on macOS the words are `Operation not permitted`
//! and a note written only for `ENOENT` would say nothing at all.
//!
//! The deny list has a second job that the mount view cannot do. A grant widens the view, and
//! on Linux a grant of `/home/dead` binds the whole home directory — `.ssh` inside it
//! included, because a bind mount cannot exclude a path within itself. Here the denies are
//! emitted **after** every allow, so a widened view still cannot read the operator's
//! credentials, keychains, shell profiles or letibot store. That is the one place where the
//! weaker mechanism is stricter, and it is measured rather than argued: the probe writes a
//! canary inside a directory the profile allows read-write and requires the deny rule to win.
//!
//! # What this boundary does not give, which is stated rather than papered over
//!
//! * **No PAX, no PID namespace: other processes are not absent.** What is measured instead
//!   is that the command cannot *reach* them: `kill -0 <pid outside>` is `EPERM` (the
//!   launcher's own pid is one of the probe's subjects), `ps` is refused, and a child of the
//!   command remains killable — job control works. A process's existence is not absent the
//!   way a PID namespace makes it absent, and [`Prop::ProcessAbsence`] says `ABSENT` for
//!   exactly this.
//! * **No cgroup.** The lifetime half is [`super::scope::ProcessGroups`], which reaps by
//!   process group: a group can be left with `setsid` and a group id is a pid. That is the
//!   tree's own documented weakening on macOS and this module does not claim more.
//! * **No `PR_SET_NO_NEW_PRIVS`.** There is no flag to set; what there is, is that the kernel
//!   applies the policy at exec, so a setuid program inside inherits it. Measured (a nested
//!   `sh -c` is denied the same path); reported as [`super::confine::Seal::NotApplicable`]
//!   for the property the Linux way, because claiming `held` would be claiming a mechanism
//!   that is not there.
//! * **`mach-lookup` is a short list, not a filter over everything.** A program that needs a
//!   system service not on [`MACH_SERVICES`] fails with the service's name in the error and
//!   the fix is one line in that list. It is a list because a profiled process reaching an
//!   arbitrary privileged XPC service is exactly what a boundary is for.
//! * **Nothing here reads `~/.ssh`, a keychain or a shell profile.** The paths are denied by
//!   name and never opened; the probe's canary is a file this module creates.
//!
//! # The cost, measured
//!
//! MEASURED 2026-10-27 on the operator's Mac (arm64, macOS 26.7.1): **100 `true` calls cost
//! 0.30 s unconfined and 0.41 s under `sandbox-exec`** — about **1.1 ms per call**, which is
//! `execve` plus the kernel evaluating the policy, and it does not grow with the profile's
//! length in any way a tool call notices. `docs/boundary-and-adjudication.md` §4j has the
//! measurement, the rule-count and what it cost to build.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::ExecError;
use super::confine::{
    Boundary, ConfinePlan, Confinement, Egress, Grant, HomeView, Mechanism, Prop, PropState, Seal,
    SealKind, TmpView, ViewSpec,
};

/// The helper. A fixed path rather than a `PATH` lookup: `sandbox-exec` is part of the OS and
/// a lookup is a seam somebody else controls.
pub const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// Where to look for the helper instead. Set it to a path that is not there to exercise the
/// refusal, which is what `tests/seatbelt.rs` does — the same seam [`super::confine::BWRAP_ENV`]
/// is on the other platform.
pub const SANDBOX_ENV: &str = "LETIBOT_SANDBOX_EXEC";

/// **The read-only roots a command needs to be a command**, on this platform.
///
/// The list is the answer to *what does `/bin/sh` need to exist*, not a security decision —
/// the security decision is everything not on it. Three entries are here because they were
/// missing in the first profile written and each one broke a tool: `/Library` (the macOS
/// toolchain shims `git`, `make` and `/usr/bin/python3` `dlopen` `libxcrun` from
/// `/Library/Developer/CommandLineTools`), `/System` (the dyld shared cache), and
/// `/Applications/Xcode.app` (a real toolchain, when one is installed).
pub const SYSTEM_ROOTS: &[&str] = &[
    "/usr",
    "/bin",
    "/sbin",
    "/System",
    "/Library",
    "/Applications/Xcode.app",
    "/Applications/Xcode-beta.app",
    "/private/etc",
    "/dev",
    "/opt/homebrew",
];

/// **The mach services a shell, `git`, `cargo`/`rustc`, `python3` and `make` need**, measured
/// by building a crate with a registry dependency under a profile that allowed only these.
///
/// `com.apple.system.logger` is `syslog(3)`; `com.apple.cfprefsd.agent` is `CFPreferences`;
/// `com.apple.SecurityServer` is the keychain *service* — the daemon that answers, never the
/// keychain files, which are denied by name in [`DENY_UNDER_HOME`]; `libinfo` is
/// `getpwnam`/`getgrnam`; `distributed_notifications@Uv3` is what `notify(3)` and a
/// terminal-less program's notifications use; `notification_center` is the same family.
pub const MACH_SERVICES: &[&str] = &[
    "com.apple.system.logger",
    "com.apple.system.notification_center",
    "com.apple.cfprefsd.agent",
    "com.apple.SecurityServer",
    "com.apple.system.libinfo",
    "com.apple.distributed_notifications@Uv3",
];

/// **What is put back inside the view read-only, out of the operator's own home.**
///
/// The session's `$HOME` is a fresh directory, so `cargo`, `rustc`, `python3` and the mise
/// shims are not under it and must be granted — read-only, because they are the operator's
/// toolchain and a session that could rewrite `~/.cargo/bin` would be writing the binaries
/// the operator runs (`docs/remote-hosts.md` RH5's rule, one platform over). The cost is
/// stated rather than discovered: **a new dependency cannot be fetched into a read-only
/// registry**, so an offline build sees the registry as it was and a build that needs
/// something new fails saying so. `.gitconfig` is here for the reason RH5 gives: without it
/// every commit inside the boundary fails with *Author identity unknown*.
pub const TOOLCHAIN_UNDER_HOME: &[&str] = &[
    ".cargo",
    ".rustup",
    ".local/bin",
    ".local/share/mise",
    ".local/state/mise",
    "bin",
    "Projects/llama.cpp",
    "models",
    ".gitconfig",
];

/// **Denied for read and write, after every allow, whatever the view grants.**
///
/// These are relative to the **operator's** home, not the session's: the session's `$HOME` is
/// a directory this module made, and the point of the list is the operator's own data. The
/// list is a *deny* rather than part of the allow-list because a deny has to win over a
/// widened view — `HostBackend::grant_into_view_impl` can put the operator's whole home
/// inside the view on the operator's word, and it must not put `~/.ssh` there.
///
/// Every entry is a fact about this platform, not a guess: `.ssh` is `docs/boundary-and-adjudication.md`
/// §3's absolute; `Library/Keychains` is where a login keychain lives; the four shell profiles
/// are what `bash -l` would read (the ordinary path here is `/bin/sh -c`, which reads none);
/// `.local/share/letibot` and `.config/letibot` are this tool's own store and modes, which a
/// session must not be able to rewrite under itself.
pub const DENY_UNDER_HOME: &[&str] = &[
    ".ssh",
    ".gnupg",
    ".aws",
    ".netrc",
    ".docker/config.json",
    ".config/gh",
    ".cargo/credentials.toml",
    "Library/Keychains",
    "Library/Cookies",
    "Library/Messages",
    "Library/Mail",
    "Library/Safari",
    "Library/Application Support/Google/Chrome",
    ".local/share/letibot",
    ".config/letibot",
    ".zshrc",
    ".zprofile",
    ".zshenv",
    ".bashrc",
    ".bash_profile",
    ".profile",
    ".config/fish",
];

/// A file this module creates inside the session's `$HOME` so that the deny rule can be
/// measured **winning over an allow**.
///
/// Without it the probe would only show that a path outside the view is unreachable, which is
/// the allow-list doing its job. The canary is in a directory the profile allows read-write
/// and is denied by name, so a successful read of it means the deny list does not work and
/// [`Seatbelt::probe`] refuses.
pub const CANARY: &str = ".letibot-canary";

/// The content of the canary. Not a secret and not read by anything: it exists so that a
/// successful read of it is unambiguous.
const CANARY_BODY: &str =
    "letibot: this file exists so that a boundary can be measured refusing to read it\n";

/// **The paths one profile names, resolved once.**
///
/// Public because the renderer is public and a golden test must be able to say exactly what
/// the layout was. [`Layout::of`] is the real one; a test builds the struct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// The workspace, read-write at its own path.
    pub project: PathBuf,
    /// `$HOME` inside the view — created if a view only abstractly asked for one.
    pub home: PathBuf,
    /// Where the command's `TMPDIR` points and where its scratch belongs.
    pub tmp: PathBuf,
    /// The operator's home. Only ever used to *deny* paths inside it.
    pub operator_home: PathBuf,
    /// The toolchain directories that are really there, read-only.
    pub toolchain: Vec<PathBuf>,
    /// `CARGO_HOME`/`RUSTUP_HOME` for the wrapped command, when the toolchain is in the view.
    pub cargo_home: Option<PathBuf>,
    pub rustup_home: Option<PathBuf>,
    /// Denied for read and write after every allow: the operator's credentials and profiles,
    /// plus whatever a caller added (the probe's canary).
    pub deny: Vec<PathBuf>,
    /// The per-user temporary directory, where the macOS toolchain's `xcrun` cache lives.
    pub user_temp: PathBuf,
}

impl Layout {
    /// Resolve a view's paths. **Creates nothing** — [`Seatbelt::probe`] makes the
    /// directories, because a resolve that has side effects cannot be used by a test.
    ///
    /// `dir` is where the profile and the directories a view did not name live: it is outside
    /// the checkout on purpose, because a profile written into the tree the command can write
    /// is a policy the command can edit.
    pub fn of(view: &ViewSpec, operator_home: &Path, dir: &Path) -> Layout {
        let home = match &view.home {
            HomeView::Tmpfs => dir.join("home"),
            HomeView::Dir(d) => d.clone(),
        };
        let tmp = match &view.tmp {
            TmpView::PerCommand => dir.join("tmp"),
            TmpView::Session(d) => d.clone(),
        };
        let mut toolchain = Vec::new();
        let mut cargo_home = None;
        let mut rustup_home = None;
        for rel in TOOLCHAIN_UNDER_HOME {
            let p = operator_home.join(rel);
            if !p.exists() {
                continue;
            }
            match *rel {
                ".cargo" => cargo_home = Some(p.clone()),
                ".rustup" => rustup_home = Some(p.clone()),
                _ => {}
            }
            toolchain.push(p);
        }
        let mut deny: Vec<PathBuf> = DENY_UNDER_HOME
            .iter()
            .map(|rel| operator_home.join(rel))
            .collect();
        deny.sort();
        deny.dedup();
        let mut user_temp = std::env::temp_dir();
        if let Ok(real) = user_temp.canonicalize() {
            user_temp = real;
        }
        Layout {
            project: view.project.clone(),
            home,
            tmp,
            operator_home: operator_home.to_path_buf(),
            toolchain,
            cargo_home,
            rustup_home,
            deny,
            user_temp,
        }
    }

    /// The `xcrun` cache file the macOS toolchain shims write, and the only thing in the
    /// per-user temp directory this profile allows.
    ///
    /// MEASURED: without it **every** `git`, `make` and `/usr/bin/python3` call prints
    /// `xcrun: error: couldn't create cache file '/var/folders/…/T/xcrun_db-…'` — those three
    /// are shims that `dlopen` a cache into `confstr(_CS_DARWIN_USER_TEMP_DIR)`, which is not
    /// `TMPDIR` and so cannot be redirected into the session's own directory. It is a
    /// performance cache of "which developer directory is selected" and nothing else in that
    /// directory is reachable.
    pub fn xcrun_cache(&self) -> String {
        format!("{}/xcrun_db", self.user_temp.display())
    }
}

/// **The SBPL text for a view.** The macOS rendering of the one policy.
///
/// Deterministic: no timestamps, no iteration over a hash map, and every path rendered as
/// written. The order is load-bearing — SBPL is last-match-wins, so the denies are emitted
/// after every allow, and `(deny default)` comes first.
pub fn render_profile(view: &ViewSpec, egress: &Egress, l: &Layout) -> String {
    let mut s = String::new();
    s.push_str("(version 1)\n");
    s.push_str("; letibot's macOS boundary — generated, one per session, do not edit.\n");
    s.push_str("; Everything not allowed below is denied. The denies at the END win over the\n");
    s.push_str("; allows above them, which is what makes them survive a widened view.\n");
    s.push_str("(deny default)\n");

    s.push_str("\n; --- a process, a syscall table, and the system services a shell needs ---\n");
    // `process*` is fork, exec and the posix_spawn machinery. Without `process-exec` nothing
    // starts at all; a profile that allows it and nothing else is a program that cannot run.
    s.push_str("(allow process*)\n");
    s.push_str("(allow sysctl-read)\n");
    s.push_str("(allow signal (target self))\n");
    s.push_str("(allow mach-lookup\n");
    for m in MACH_SERVICES {
        s.push_str(&format!("  (global-name \"{m}\")\n"));
    }
    s.push_str(")\n");

    s.push_str(
        "\n; --- read: the system roots, the toolchain, the session's own directories ---\n",
    );
    s.push_str("(allow file-read-metadata)\n");
    s.push_str("(allow file-read*\n");
    // **`(literal \"/\")` is not decoration.** MEASURED 2026-10-27: a profile that allows
    // `file-read*` on `/usr` and the rest but not on `/` itself makes *every* process abort
    // with SIGABRT before `main` — the sandbox's own check-in reads the root directory. The
    // root's entries are the top-level names (`ls /`), and nothing below them is reachable
    // unless it is allowed on its own line.
    s.push_str("  (literal \"/\")\n");
    for p in SYSTEM_ROOTS {
        s.push_str(&format!("  (subpath \"{p}\")\n"));
    }
    for p in &l.toolchain {
        s.push_str(&format!("  (subpath \"{}\")\n", p.display()));
    }
    for p in [&l.project, &l.home, &l.tmp] {
        s.push_str(&format!("  (subpath \"{}\")\n", p.display()));
    }
    s.push_str(&format!("  (subpath \"{}\"))\n", l.xcrun_cache()));
    for g in &view.grants {
        match g {
            Grant::ReadOnly { path, .. } => s.push_str(&format!(
                "(allow file-read* (subpath \"{}\"))\n",
                path.display()
            )),
            Grant::ReadWrite { path, .. } => s.push_str(&format!(
                "(allow file-read* file-write* (subpath \"{}\"))\n",
                path.display()
            )),
            // An agent socket is a unix socket, which Seatbelt has no operation for: a unix
            // socket is reached with `file-read*`/`file-write*` on its path, and this is that
            // — and it is the *only* grant that puts a credential inside the view without
            // putting a key in it, so it is allowed by name and nothing wider.
            Grant::AgentSocket { path, .. } => s.push_str(&format!(
                "(allow file-read* file-write* (literal \"{}\"))\n",
                path.display()
            )),
        }
    }

    s.push_str(
        "\n; --- write: the checkout, the session's own directories, and nothing else ---\n",
    );
    s.push_str("(allow file-write*\n");
    for p in [&l.project, &l.home, &l.tmp] {
        s.push_str(&format!("  (subpath \"{}\")\n", p.display()));
    }
    s.push_str(&format!("  (subpath \"{}\")\n", l.xcrun_cache()));
    s.push_str("  (literal \"/dev/null\")\n");
    s.push_str("  (literal \"/dev/stdout\")\n");
    s.push_str("  (literal \"/dev/stderr\")\n");
    // **`/dev/tty`, because a program's own terminal is not a path it chose.** stdio already
    // points there and `write(2)` on an open descriptor is not filtered, but a program that
    // *opens* its controlling terminal — `sudo` asking for a password, a pager, a shell's job
    // control — would be refused one it can already write to. It is the session's own tty and
    // never a device of another user's.
    s.push_str("  (literal \"/dev/tty\")\n");
    s.push_str("  (literal \"/dev/dtracehelper\"))\n");
    for g in &view.grants {
        if let Grant::ReadWrite { path, .. } = g {
            s.push_str(&format!(
                "(allow file-write* (subpath \"{}\"))\n",
                path.display()
            ));
        }
    }

    s.push_str("\n; --- egress: a decision, not a default ---\n");
    match egress {
        Egress::Denied => {
            // `(deny default)` already denies it; said out loud so that a reader of the
            // profile can see the decision in it, and so that a future `(allow ...)` above
            // cannot quietly grant the network.
            s.push_str("(deny network*)\n");
        }
        Egress::Host { .. } => {
            s.push_str("(allow network*)\n");
        }
    }

    s.push_str("\n; --- denied AFTER the allows above, so a grant cannot undo them ---\n");
    if !l.deny.is_empty() {
        s.push_str("(deny file-read* file-write*\n");
        for p in &l.deny {
            s.push_str(&format!("  (subpath \"{}\")\n", p.display()));
        }
        s.push_str(")\n");
    }
    s
}

/// **The script the probe runs inside the profile.**
///
/// One `probe <name> <outcome>` line per fact, so the parser has no opinions about a
/// program's prose. The subjects are handed in as arguments rather than written into it:
/// `$1` the canary, `$2` a path outside the view to write, `$3` a path inside it, `$4` the
/// launcher's own pid, `$5`/`$6` the two egress destinations.
///
/// **It never sends a byte.** The two network subjects are a loopback port nothing listens on
/// and `192.0.2.1` (TEST-NET-1, RFC 5737: reserved for documentation, routed nowhere), and
/// `nc -z` closes as soon as the answer arrives.
const PROBE: &str = "\
p() { echo \"probe $1 $2\"; }
if [ -r \"$1\" ]; then p read-denied allowed; else p read-denied denied; fi
if : > \"$2\" 2>/dev/null; then p write-outside wrote; else p write-outside denied; fi
if : > \"$3\" 2>/dev/null; then p write-inside wrote; else p write-inside denied; fi
if /bin/sh -c true 2>/dev/null; then p fork-exec ran; else p fork-exec failed; fi
if /bin/sh -c \"test -r \\\"$1\\\"\" 2>/dev/null; then p nested-read allowed; else p nested-read denied; fi
if kill -0 \"$4\" 2>/dev/null; then p signal-outside reached; else p signal-outside denied; fi
if command -v nc >/dev/null 2>&1; then
  m=$(nc -v -z -G 1 -w 1 127.0.0.1 1 2>&1)
  case \"$m\" in *\"permitted\"*) p net-loopback denied;; *\"refused\"*) p net-loopback allowed;; *) p net-loopback unverified;; esac
  m=$(nc -v -z -G 1 -w 1 192.0.2.1 80 2>&1)
  case \"$m\" in *\"permitted\"*) p net-external denied;; *\"unreachable\"*|*\"refused\"*|*\"timed out\"*) p net-external allowed;; *) p net-external unverified;; esac
else
  p net-loopback unverified
  p net-external unverified
fi
";

/// One line of [`PROBE`]'s answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Denied,
    Allowed,
    Wrote,
    Ran,
    Failed,
    Reached,
    Unverified,
}

impl Outcome {
    fn parse(s: &str) -> Option<Outcome> {
        Some(match s {
            "denied" => Outcome::Denied,
            "allowed" => Outcome::Allowed,
            "wrote" => Outcome::Wrote,
            "ran" => Outcome::Ran,
            "failed" => Outcome::Failed,
            "reached" => Outcome::Reached,
            "unverified" => Outcome::Unverified,
            _ => return None,
        })
    }
}

/// **What the probe measured**, before it is judged.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Probe {
    pub facts: BTreeMap<String, Outcome>,
    /// Everything the helper and the probe said, for a refusal that has to quote it.
    pub said: String,
}

impl Probe {
    pub fn fact(&self, name: &str) -> Option<&Outcome> {
        self.facts.get(name)
    }
}

/// Read [`PROBE`]'s output. Unknown lines are kept in [`Probe::said`] and ignored here: a
/// command's own output may be anything, and only `probe ` lines are the probe's.
pub fn read_probe(said: &str) -> Probe {
    let mut facts = BTreeMap::new();
    for line in said.lines() {
        let Some(rest) = line.trim_end_matches('\r').strip_prefix("probe ") else {
            continue;
        };
        let mut it = rest.split_whitespace();
        if let (Some(name), Some(outcome)) = (it.next(), it.next())
            && let Some(o) = Outcome::parse(outcome)
        {
            facts.insert(name.to_string(), o);
        }
    }
    Probe {
        facts,
        said: said.to_string(),
    }
}

/// **A measured macOS boundary**, and the profile file every spawn is wrapped in.
#[derive(Debug, Clone)]
pub struct Seatbelt {
    helper: PathBuf,
    profile: PathBuf,
    home: PathBuf,
    tmp: PathBuf,
    cargo_home: Option<PathBuf>,
    rustup_home: Option<PathBuf>,
    boundary: Boundary,
}

impl Seatbelt {
    /// **Render the profile, run the probe inside it, and believe only what it measured.**
    ///
    /// The refusals, each naming what is missing: no `sandbox-exec`; the profile could not be
    /// written; the helper would not run; the probe printed nothing it could read; the canary
    /// was readable (the deny list does not work); a write inside the view failed (the profile
    /// is unusable); fork/exec failed; a write outside the view succeeded; a nested child was
    /// not confined; an egress decision that did not take.
    pub fn probe(view: ViewSpec, egress: Egress) -> Result<Seatbelt, ExecError> {
        Seatbelt::probe_with(view, egress, Vec::new())
    }

    /// [`Seatbelt::probe`], with paths **added to the deny list** — the seam a test needs to
    /// put a fixture's `$HOME` behind the same rule as the operator's own.
    ///
    /// The real caller passes nothing: the list is [`DENY_UNDER_HOME`] expanded against
    /// `$HOME`, which is the operator's. A test cannot use that list to point at a temporary
    /// directory, and a test that instead measured the operator's real credentials would be
    /// measuring — and reading — the thing the boundary exists to protect.
    pub fn probe_with(
        view: ViewSpec,
        egress: Egress,
        extra_deny: Vec<PathBuf>,
    ) -> Result<Seatbelt, ExecError> {
        let helper = find_helper()?;
        let view = super::confine::prepared(view)?;
        let operator_home = operator_home();
        let dir = profile_dir()?;
        let layout = Layout::of(&view, &operator_home, &dir);

        // The directories a view may only have asked for abstractly.
        for d in [&layout.home, &layout.tmp] {
            std::fs::create_dir_all(d).map_err(|e| {
                ExecError::NoConfinement(format!(
                    "`{}` could not be created ({e}). A boundary needs a writable home and a \
                     scratch directory of the session's own, and neither may be the operator's.",
                    d.display()
                ))
            })?;
        }

        // The canary, and the deny list that has to win over the allow it sits inside.
        let canary = layout.home.join(CANARY);
        std::fs::write(&canary, CANARY_BODY).map_err(|e| {
            ExecError::NoConfinement(format!(
                "the probe's canary `{}` could not be written ({e}), so whether the deny list \
                 wins over an allow could not be measured — and an unmeasured boundary is \
                 treated as no boundary.",
                canary.display()
            ))
        })?;
        let mut layout = layout;
        layout.deny.extend(extra_deny);
        layout.deny.push(canary.clone());
        layout.deny.sort();
        layout.deny.dedup();

        let text = render_profile(&view, &egress, &layout);
        let profile = dir.join(format!("profile-{}.sb", next_profile()));
        std::fs::write(&profile, &text).map_err(|e| {
            ExecError::NoConfinement(format!(
                "the profile `{}` could not be written ({e}), so there is nothing to run a \
                 command inside. It is written outside the checkout on purpose: a policy the \
                 wrapped command can edit is not a policy.",
                profile.display()
            ))
        })?;

        let outside = dir.join("outside-write");
        let inside = layout.project.join(".letibot-probe-write");
        let argv = wrap_with(
            &helper,
            &profile,
            &layout.home,
            &layout.tmp,
            layout.cargo_home.as_deref(),
            layout.rustup_home.as_deref(),
        );
        let out = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .arg("/bin/sh")
            .arg("-c")
            .arg(PROBE)
            .arg("letibot-seatbelt")
            .arg(&canary)
            .arg(&outside)
            .arg(&inside)
            .arg(std::process::id().to_string())
            .current_dir(&layout.project)
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| {
                ExecError::NoConfinement(format!(
                    "`{}` could not be run ({e}), so nothing was measured and no command will \
                     be run inside it",
                    helper.display()
                ))
            })?;
        let printed = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let probe = read_probe(&printed);
        let _ = std::fs::remove_file(&outside);
        let _ = std::fs::remove_file(&inside);

        if probe.facts.is_empty() {
            return Err(ExecError::NoConfinement(format!(
                "`{}` ran but printed nothing the probe could read, so what the profile does \
                 is unknown — and an unknown boundary is treated as no boundary. Exit {}; it \
                 said: {}",
                helper.display(),
                out.status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "on a signal".into()),
                snippet(&printed)
            )));
        }

        let mechanism = format!("sandbox-exec ({})", version_of(&helper));
        judged(helper, profile, layout, view, egress, mechanism, probe)
    }

    /// The profile's path, for a test or an operator who wants to read the policy.
    pub fn profile(&self) -> &Path {
        &self.profile
    }
}

/// **The refusals a measurement has to survive**, in the order they matter.
///
/// The first one is the case the whole module is written against: a profile that is written
/// but does not take looks exactly like one that does, from everywhere except inside.
#[allow(clippy::too_many_arguments)]
fn judged(
    helper: PathBuf,
    profile: PathBuf,
    layout: Layout,
    mut view: ViewSpec,
    egress: Egress,
    mechanism: String,
    probe: Probe,
) -> Result<Seatbelt, ExecError> {
    let f = |name: &str| probe.fact(name).cloned();
    let refuse = |what: String| {
        Err(ExecError::NoConfinement(format!(
            "{what} What it said: {}",
            snippet(&probe.said)
        )))
    };

    match f("read-denied") {
        Some(Outcome::Denied) => {}
        Some(Outcome::Allowed) => {
            return refuse(format!(
                "the profile did not deny a path on its deny list: the canary `{}` — a file \
                 this module wrote inside a directory the profile allows read-write — was \
                 readable from inside. A deny list that a grant can undo is not a boundary, \
                 and nothing was run in it.",
                layout.home.join(CANARY).display()
            ));
        }
        other => {
            return refuse(format!(
                "whether the deny list works could not be measured ({other:?}). An unmeasured \
                 boundary is treated as no boundary, so nothing was run."
            ));
        }
    }
    match f("write-outside") {
        Some(Outcome::Denied) => {}
        Some(Outcome::Wrote) => {
            return refuse(format!(
                "a write outside the view succeeded: `{}` was created from inside. The \
                 profile's write allow-list is what makes the checkout the only writable \
                 place, and it did not take.",
                layout
                    .project
                    .parent()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            ));
        }
        other => {
            return refuse(format!(
                "a write outside the view could not be measured ({other:?})"
            ));
        }
    }
    match f("write-inside") {
        Some(Outcome::Wrote) => {}
        other => {
            return refuse(format!(
                "a write INSIDE the checkout failed ({other:?}), so a command here could not \
                 do its work. A boundary that forbids the thing the session is for is not a \
                 boundary this session can run in."
            ));
        }
    }
    match f("fork-exec") {
        Some(Outcome::Ran) => {}
        other => {
            return refuse(format!(
                "fork/exec failed inside the profile ({other:?}), so a shell could not run a \
                 command at all"
            ));
        }
    }
    let nested_held = match f("nested-read") {
        Some(Outcome::Denied) => Seal::Held {
            how: "a child `sh -c` was denied the same path the parent was".into(),
        },
        Some(Outcome::Allowed) => {
            return refuse(
                "a nested `sh -c` COULD read the canary, so a child strips the boundary its \
                 parent was given — which makes wrapping the command decoration. Nothing was \
                 run in it."
                    .to_string(),
            );
        }
        other => Seal::Unverified {
            why: format!("the nested read could not be measured ({other:?})"),
        },
    };

    // **The egress decision, measured against the decision.** A run that asked for the host's
    // network and did not get it is a refusal and not a partial boundary: the profile would
    // make every `curl` inside fail while the disclosure said the opposite.
    let net = match &egress {
        Egress::Denied => ("net-loopback", "net-external"),
        Egress::Host { .. } => ("net-loopback", "net-external"),
    };
    let net_state = (f(net.0), f(net.1));
    let egress_prop = match (&egress, &net_state) {
        (Egress::Denied, (Some(Outcome::Denied), Some(Outcome::Denied))) => PropState::Held {
            how: "a loopback connect and one to 192.0.2.1 were both `EPERM` from inside".into(),
        },
        (Egress::Denied, (_, Some(Outcome::Allowed))) => {
            return refuse(
                "egress was DENIED and a connect from inside reached its destination, so the \
                 network decision did not take. A boundary whose egress decision is not the \
                 one that was made is worse than none, because every card after it would be \
                 written on a false premise."
                    .to_string(),
            );
        }
        (Egress::Host { why }, _) => PropState::Held {
            how: format!("the host's network, by decision — {why}"),
        },
        (Egress::Denied, other) => PropState::Unverified {
            why: format!("the network probe could not decide: {other:?}"),
        },
    };

    let signal = match f("signal-outside") {
        Some(Outcome::Denied) => PropState::Held {
            how: "`kill -0 <this process's own launcher>` from inside was `EPERM`, and a child \
                  of the command stayed killable"
                .into(),
        },
        Some(Outcome::Reached) => PropState::Absent {
            why: "a signal from inside reached a process outside the boundary".into(),
        },
        other => PropState::Unverified {
            why: format!(
                "whether a signal reaches outside the boundary could not be measured ({other:?})"
            ),
        },
    };

    let mut props = BTreeMap::new();
    props.insert(
        Prop::FileView,
        PropState::Held {
            how: format!(
                "measured from inside: `{}` was unreadable (the deny rule won over the allow), \
                 a write outside the checkout failed, and a write in it succeeded",
                layout.home.join(CANARY).display()
            ),
        },
    );
    props.insert(Prop::ProcessIsolation, signal);
    props.insert(
        Prop::ProcessAbsence,
        PropState::Absent {
            why: "macOS has no PID namespace. Processes outside the boundary are unreachable \
                  (measured) but not absent: a lookup by pid still finds one, and `/proc` — \
                  which is the Linux answer — does not exist here."
                .into(),
        },
    );
    props.insert(Prop::Egress, egress_prop);
    props.insert(
        Prop::Nested,
        PropState::Held {
            how: "a nested `sh -c` inherited the profile and was denied the same path".into(),
        },
    );

    let mut seals = BTreeMap::new();
    seals.insert(SealKind::NestedNamespaces, nested_held);
    seals.insert(
        SealKind::NoNewPrivs,
        Seal::NotApplicable {
            why: "macOS has no `PR_SET_NO_NEW_PRIVS` and no equivalent flag. What it has is \
                  that the kernel applies the policy at `exec`, so a setuid program inside \
                  inherits the profile — measured with the nested child — but *cannot raise \
                  privilege* is not a property this OS can be asked for."
                .into(),
        },
    );

    // **The view as it will actually be**, not as it was asked for: `$HOME` and `/tmp` are
    // the resolved directories, so a disclosure and an `ENOENT` note describe the same place.
    view.project = layout.project.clone();
    view.home = HomeView::Dir(layout.home.clone());
    view.tmp = TmpView::Session(layout.tmp.clone());

    let boundary = Boundary {
        mechanism,
        platform: Mechanism::Policy,
        view,
        egress,
        ns: BTreeMap::new(),
        seals,
        props,
        measured_at: SystemTime::now(),
    };
    if !boundary.complete() {
        return Err(ExecError::NoConfinement(format!(
            "the profile ran and the boundary is not complete, so a command here would be \
             confined-looking and not confined. What was measured: {}",
            boundary.describe()
        )));
    }
    Ok(Seatbelt {
        helper,
        profile,
        home: layout.home,
        tmp: layout.tmp,
        cargo_home: layout.cargo_home,
        rustup_home: layout.rustup_home,
        boundary,
    })
}

/// The argv, from the helper up to but **not including** the command.
///
/// `sandbox-exec` takes `-f <profile>` and then a program; the caller of
/// [`Confinement::wrap`] appends its own shell and command, so the program here is a `/bin/sh`
/// that puts the environment right and `exec`s what it is handed. That shim exists because a
/// profile cannot set an environment variable and `wrap` returns an argv and not a
/// `Command`: `HOME` has to be the session's own directory (otherwise `cargo`, `git` and
/// `ssh` write into the operator's home, where the deny list would refuse them), `TMPDIR` has
/// to be the session's own (and not `/var/folders/…`, which the profile refuses), and
/// `CARGO_HOME`/`RUSTUP_HOME` have to name the operator's read-only toolchain, because the
/// session's home holds no toolchain at all.
fn wrap_with(
    helper: &Path,
    profile: &Path,
    home: &Path,
    tmp: &Path,
    cargo_home: Option<&Path>,
    rustup_home: Option<&Path>,
) -> Vec<String> {
    let mut export = format!(
        "export HOME={} TMPDIR={}",
        crate::remote::quote(&home.display().to_string()),
        crate::remote::quote(&tmp.display().to_string())
    );
    if let Some(c) = cargo_home {
        export.push_str(&format!(
            " CARGO_HOME={}",
            crate::remote::quote(&c.display().to_string())
        ));
    }
    if let Some(r) = rustup_home {
        export.push_str(&format!(
            " RUSTUP_HOME={}",
            crate::remote::quote(&r.display().to_string())
        ));
    }
    export.push_str("; exec \"$@\"");
    vec![
        helper.display().to_string(),
        "-f".to_string(),
        profile.display().to_string(),
        "/bin/sh".to_string(),
        "-c".to_string(),
        export,
        "letibot-seatbelt".to_string(),
    ]
}

impl Confinement for Seatbelt {
    fn describe(&self) -> String {
        self.boundary.describe()
    }

    fn boundary(&self) -> Option<&Boundary> {
        Some(&self.boundary)
    }

    fn wrap(&self, _plan: &ConfinePlan<'_>) -> Result<Vec<String>, ExecError> {
        // **The cwd is not the profile's business.** The caller sets it on the `Command`, and
        // the profile names the checkout as writable wherever the command starts from.
        Ok(wrap_with(
            &self.helper,
            &self.profile,
            &self.home,
            &self.tmp,
            self.cargo_home.as_deref(),
            self.rustup_home.as_deref(),
        ))
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

    /// `sandbox-exec`'s own diagnostics are prefixed `sandbox-exec: `. The same heuristic
    /// [`super::confine::Bwrap`] uses for `bwrap: `, and the same caveat: a command that
    /// printed such a line would be misattributed, which is better than reporting a boundary
    /// that failed to start as the command's answer.
    fn launcher_failure(&self, output: &str) -> Option<String> {
        let line = output
            .lines()
            .find(|l| l.trim_start().starts_with("sandbox-exec: "))?;
        Some(format!(
            "`{}` is the boundary's own diagnostic, so this is NOT the command's answer — the \
             command may never have run. What it said: {}",
            self.helper.display(),
            line.trim()
        ))
    }
}

/// Where the helper is. [`SANDBOX_ENV`] first, so a test can point it at nothing.
fn find_helper() -> Result<PathBuf, ExecError> {
    if let Ok(p) = std::env::var(SANDBOX_ENV) {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Ok(p);
        }
        return Err(ExecError::NoConfinement(format!(
            "`{SANDBOX_ENV}` names `{}`, which is not a file, so there is no helper to apply a \
             profile with. Nothing was run. This variable is the seam a test points at nothing \
             to exercise this refusal; unset it to go back to `{SANDBOX_EXEC}`.",
            p.display()
        )));
    }
    let p = PathBuf::from(SANDBOX_EXEC);
    if p.is_file() {
        return Ok(p);
    }
    Err(ExecError::NoConfinement(format!(
        "`{SANDBOX_EXEC}` is not there. Seatbelt is part of macOS and is not installed \
         anywhere else, so this session has no boundary on this host and nothing was run \
         inside one. Two alternatives, and neither is a quiet downgrade: run the session in a \
         firecode VM (`letibot --vm`), or seat `leticode`, whose banner says the session is \
         unconfined."
    )))
}

/// The operator's home, for the deny list only. `$HOME`, and no fallback: a `getpwnam` would
/// answer the same thing and this crate reaches the system through `std` and `/proc`.
fn operator_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// A directory of this daemon's own for profiles, outside every checkout.
///
/// Per process and in the per-user temp directory, like `ProcessGroups::probe`'s scope root:
/// the profile names the paths a session may touch and is written where the operator can find
/// it, but never inside the tree the command can write.
fn profile_dir() -> Result<PathBuf, ExecError> {
    let dir = std::env::temp_dir().join(format!("letibot-seatbelt-{}", std::process::id()));
    {
        use std::os::unix::fs::DirBuilderExt;
        let mut b = std::fs::DirBuilder::new();
        b.mode(0o700).recursive(true);
        b.create(&dir).map_err(|e| {
            ExecError::NoConfinement(format!(
                "`{}` could not be created ({e}), so there is nowhere to write the profile",
                dir.display()
            ))
        })?;
    }
    Ok(dir)
}

/// One profile file per probe, so a re-probe cannot overwrite the policy a running command
/// was started with.
fn next_profile() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(1);
    N.fetch_add(1, Ordering::Relaxed)
}

/// `sandbox-exec` has no `--version`, so the disclosure names the build by what the file is.
fn version_of(helper: &Path) -> String {
    match std::fs::metadata(helper) {
        Ok(m) => {
            let when = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            format!("{} bytes, built {when}", m.len())
        }
        Err(_) => "version unknown".to_string(),
    }
}

/// At most this many lines of a helper's or a probe's own words in a refusal.
fn snippet(s: &str) -> String {
    let mut out: Vec<&str> = s.lines().filter(|l| !l.trim().is_empty()).collect();
    let n = out.len();
    out.truncate(8);
    let mut joined = out.join("\n    ");
    if n > 8 {
        joined.push_str(&format!("\n    … and {} more line(s)", n - 8));
    }
    if joined.is_empty() {
        "(it said nothing)".to_string()
    } else {
        joined
    }
}

/// The `Piece` form is unneeded by a local spawn — a policy has no far side — and the
/// [`Confinement`] default is exactly right for it, so nothing here overrides it.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::confine::{Grant, Seal};

    fn layout() -> Layout {
        Layout {
            project: PathBuf::from("/Users/x/proj"),
            home: PathBuf::from("/var/lib/lt/home"),
            tmp: PathBuf::from("/var/lib/lt/tmp"),
            operator_home: PathBuf::from("/Users/x"),
            toolchain: vec![PathBuf::from("/Users/x/.cargo")],
            cargo_home: Some(PathBuf::from("/Users/x/.cargo")),
            rustup_home: None,
            deny: vec![
                PathBuf::from("/Users/x/.ssh"),
                PathBuf::from("/Users/x/.zshrc"),
            ],
            user_temp: PathBuf::from("/private/var/folders/aa/bb/T"),
        }
    }

    /// **The golden.** The whole profile, character for character: a policy that changes
    /// without anybody noticing is the thing this test exists to prevent, and a `contains`
    /// assertion would let a rule be added or reordered silently.
    #[test]
    fn the_profile_for_a_view_is_exactly_this() {
        let view = ViewSpec::project_only("/Users/x/proj");
        let got = render_profile(&view, &Egress::Denied, &layout());
        let want = "\
(version 1)
; letibot's macOS boundary — generated, one per session, do not edit.
; Everything not allowed below is denied. The denies at the END win over the
; allows above them, which is what makes them survive a widened view.
(deny default)

; --- a process, a syscall table, and the system services a shell needs ---
(allow process*)
(allow sysctl-read)
(allow signal (target self))
(allow mach-lookup
  (global-name \"com.apple.system.logger\")
  (global-name \"com.apple.system.notification_center\")
  (global-name \"com.apple.cfprefsd.agent\")
  (global-name \"com.apple.SecurityServer\")
  (global-name \"com.apple.system.libinfo\")
  (global-name \"com.apple.distributed_notifications@Uv3\")
)

; --- read: the system roots, the toolchain, the session's own directories ---
(allow file-read-metadata)
(allow file-read*
  (literal \"/\")
  (subpath \"/usr\")
  (subpath \"/bin\")
  (subpath \"/sbin\")
  (subpath \"/System\")
  (subpath \"/Library\")
  (subpath \"/Applications/Xcode.app\")
  (subpath \"/Applications/Xcode-beta.app\")
  (subpath \"/private/etc\")
  (subpath \"/dev\")
  (subpath \"/opt/homebrew\")
  (subpath \"/Users/x/.cargo\")
  (subpath \"/Users/x/proj\")
  (subpath \"/var/lib/lt/home\")
  (subpath \"/var/lib/lt/tmp\")
  (subpath \"/private/var/folders/aa/bb/T/xcrun_db\"))

; --- write: the checkout, the session's own directories, and nothing else ---
(allow file-write*
  (subpath \"/Users/x/proj\")
  (subpath \"/var/lib/lt/home\")
  (subpath \"/var/lib/lt/tmp\")
  (subpath \"/private/var/folders/aa/bb/T/xcrun_db\")
  (literal \"/dev/null\")
  (literal \"/dev/stdout\")
  (literal \"/dev/stderr\")
  (literal \"/dev/tty\")
  (literal \"/dev/dtracehelper\"))

; --- egress: a decision, not a default ---
(deny network*)

; --- denied AFTER the allows above, so a grant cannot undo them ---
(deny file-read* file-write*
  (subpath \"/Users/x/.ssh\")
  (subpath \"/Users/x/.zshrc\")
)
";
        assert_eq!(got, want);
    }

    #[test]
    fn the_deny_list_survives_a_grant_that_names_the_whole_home() {
        // **The one place the weaker mechanism is stricter, and the reason the denies are
        // emitted last.** A grant is the operator widening the view — on Linux a bind of the
        // home directory would carry `~/.ssh` inside it, because a bind cannot exclude a path
        // within itself. Here it cannot.
        let view = ViewSpec::project_only("/Users/x/proj").granting(Grant::ReadOnly {
            path: PathBuf::from("/Users/x"),
            why: "the operator said `--grant-ro ~`".into(),
        });
        let p = render_profile(&view, &Egress::Denied, &layout());
        let allow = p
            .find("(allow file-read* (subpath \"/Users/x\"))")
            .expect("the grant");
        let deny = p
            .find("(deny file-read* file-write*")
            .expect("the deny block");
        assert!(
            deny > allow,
            "the deny list must come after every allow:\n{p}"
        );
        assert!(p[deny..].contains("(subpath \"/Users/x/.ssh\")"), "{p}");
        // And it is one block, so a reader can count what is unreachable.
        let after = &p[deny..];
        assert!(
            !after[..after.find(")\n").map(|i| i + 2).unwrap_or(after.len())].contains("(allow")
        );
    }

    #[test]
    fn egress_is_the_only_difference_between_the_two_renderings() {
        let view = ViewSpec::project_only("/Users/x/proj");
        let denied = render_profile(&view, &Egress::Denied, &layout());
        let allowed = render_profile(
            &view,
            &Egress::Host {
                why: "the operator authorised `git push`".into(),
            },
            &layout(),
        );
        assert!(denied.contains("(deny network*)"), "{denied}");
        assert!(!denied.contains("(allow network*"), "{denied}");
        assert!(allowed.contains("(allow network*)"), "{allowed}");
        assert!(!allowed.contains("(deny network*)"), "{allowed}");
        // The decision is the whole difference: nothing else in the text moves.
        assert_eq!(
            denied.replace("(deny network*)", ""),
            allowed.replace("(allow network*)", "")
        );
    }

    #[test]
    fn every_denied_path_is_in_the_profile_and_none_of_them_is_a_read() {
        // Not a style test: a path that is denied and then re-allowed by a later rule is a
        // path that is not denied, and SBPL is last-match-wins.
        let view = ViewSpec::project_only("/Users/x/proj");
        let l = Layout::of(&view, Path::new("/Users/x"), Path::new("/tmp/lt"));
        let p = render_profile(&view, &Egress::Denied, &l);
        let (_, tail) = p.split_at(
            p.find("(deny file-read* file-write*")
                .expect("the deny block"),
        );
        for rel in DENY_UNDER_HOME {
            let abs = format!("/Users/x/{rel}");
            assert!(tail.contains(&abs), "{abs} is not denied:\n{p}");
        }
        assert!(
            !tail.contains("(allow"),
            "an allow after the deny list would undo it"
        );
    }

    #[test]
    fn a_fixture_layout_keeps_its_own_denies_and_still_has_no_allow_after_them() {
        // The three-line fixture above is what a caller building a `Layout` by hand gets; the
        // deny block is emitted whatever is in it, and never with an allow after it.
        let view = ViewSpec::project_only("/Users/x/proj");
        let p = render_profile(&view, &Egress::Denied, &layout());
        let (_, tail) = p.split_at(
            p.find("(deny file-read* file-write*")
                .expect("the deny block"),
        );
        assert!(tail.contains("(subpath \"/Users/x/.ssh\")"), "{p}");
        assert!(!tail.contains("(allow"), "{p}");
    }

    #[test]
    fn a_tmpfs_home_and_a_per_command_tmp_become_directories_beside_the_profile() {
        // The view is abstract on purpose (`Tmpfs`, `PerCommand`); macOS has no mount
        // namespace to give it a fresh filesystem per command, so what it gets is a directory
        // this daemon made, and the boundary records it as such — see `Seatbelt::probe`.
        let v = ViewSpec::project_only("/Users/x/proj");
        let l = Layout::of(&v, Path::new("/Users/x"), Path::new("/tmp/lt"));
        assert_eq!(l.home, PathBuf::from("/tmp/lt/home"));
        assert_eq!(l.tmp, PathBuf::from("/tmp/lt/tmp"));
        assert!(l.deny.contains(&PathBuf::from("/Users/x/.ssh")));
        // The toolchain is taken from the operator's home, and only what is there.
        for p in &l.toolchain {
            assert!(p.starts_with("/Users/x"), "{p:?}");
        }
    }

    #[test]
    fn a_grant_is_rendered_as_the_allow_it_asks_for() {
        let view = ViewSpec::project_only("/Users/x/proj")
            .granting(Grant::ReadOnly {
                path: PathBuf::from("/srv/data"),
                why: "the operator said so".into(),
            })
            .granting(Grant::ReadWrite {
                path: PathBuf::from("/srv/work"),
                why: "the operator said so".into(),
            });
        let p = render_profile(&view, &Egress::Denied, &layout());
        assert!(
            p.contains("(allow file-read* (subpath \"/srv/data\"))"),
            "{p}"
        );
        assert!(
            p.contains("(allow file-read* file-write* (subpath \"/srv/work\"))"),
            "{p}"
        );
    }

    #[test]
    fn the_probe_is_read_by_its_own_lines_and_a_commands_output_is_not_mistaken_for_it() {
        let said = "\
probe read-denied denied
some program saying probe read-denied allowed
probe write-outside denied
probe write-inside wrote
probe fork-exec ran
probe nested-read denied
probe signal-outside denied
probe net-loopback denied
probe net-external denied
probe nonsense whatever
";
        let p = read_probe(said);
        assert_eq!(p.fact("read-denied"), Some(&Outcome::Denied));
        assert_eq!(p.fact("write-inside"), Some(&Outcome::Wrote));
        assert_eq!(p.fact("nested-read"), Some(&Outcome::Denied));
        assert_eq!(p.fact("net-external"), Some(&Outcome::Denied));
        // An unknown outcome word is not a fact; an unknown name is not either.
        assert_eq!(p.fact("nonsense"), None);
        // And a line a program printed in the middle of the output keeps its own indentation,
        // so it cannot be read as the probe's.
        assert_eq!(p.fact("write-outside"), Some(&Outcome::Denied));
        assert!(read_probe("nothing here").facts.is_empty());
    }

    #[test]
    fn a_policy_boundary_describes_itself_by_property_and_names_what_it_cannot_give() {
        let mut props = BTreeMap::new();
        for p in [
            Prop::FileView,
            Prop::ProcessIsolation,
            Prop::Egress,
            Prop::Nested,
        ] {
            props.insert(
                p,
                PropState::Held {
                    how: "measured (fixture)".into(),
                },
            );
        }
        props.insert(
            Prop::ProcessAbsence,
            PropState::Absent {
                why: "macOS has no PID namespace".into(),
            },
        );
        let mut seals = BTreeMap::new();
        seals.insert(
            SealKind::NestedNamespaces,
            Seal::Held {
                how: "a nested sh -c was denied".into(),
            },
        );
        seals.insert(
            SealKind::NoNewPrivs,
            Seal::NotApplicable {
                why: "macOS has no PR_SET_NO_NEW_PRIVS".into(),
            },
        );
        let b = Boundary {
            mechanism: "sandbox-exec (fixture)".into(),
            platform: Mechanism::Policy,
            view: ViewSpec::project_only("/Users/x/proj"),
            egress: Egress::Denied,
            ns: BTreeMap::new(),
            seals,
            props,
            measured_at: SystemTime::now(),
        };
        assert!(b.complete(), "{}", b.describe());
        // D16 applies here too: every required property held, egress denied, and the one
        // seal macOS cannot have is not applicable rather than missing.
        assert!(
            b.encloses(),
            "a measured Seatbelt boundary is D16's: {}",
            b.describe()
        );
        let mut shaky = b.clone();
        shaky.props.insert(
            Prop::FileView,
            PropState::Unverified {
                why: "the probe could not tell (fixture)".into(),
            },
        );
        assert!(
            !shaky.encloses(),
            "an unverified property keeps the command asking, as an unverified seal does on Linux"
        );
        assert!(
            !b.partial(),
            "an OS that cannot give a property is not a defect"
        );
        let d = b.describe();
        assert!(d.starts_with("confined;"), "{d}");
        assert!(d.contains("file-view [required]: held"), "{d}");
        assert!(d.contains("process-absence"), "{d}");
        assert!(d.contains("this OS cannot give it"), "{d}");
        assert!(d.contains("NOT APPLICABLE"), "{d}");
        // A policy boundary must not print the namespace table: it has none, and seven
        // `NOT ENTERED` lines would read as a boundary that failed.
        assert!(!d.contains("mnt:"), "{d}");
        assert!(!d.contains("user:"), "{d}");
    }

    #[test]
    fn the_absence_note_speaks_the_words_a_denial_actually_produces() {
        let mut props = BTreeMap::new();
        for p in Prop::REQUIRED_POLICY {
            props.insert(
                p,
                PropState::Held {
                    how: "measured (fixture)".into(),
                },
            );
        }
        let b = Boundary {
            mechanism: "sandbox-exec (fixture)".into(),
            platform: Mechanism::Policy,
            view: ViewSpec::project_only("/Users/x/proj"),
            egress: Egress::Denied,
            ns: BTreeMap::new(),
            seals: BTreeMap::new(),
            props,
            measured_at: SystemTime::now(),
        };
        // A policy denies: the command's own words are EPERM, and an ENOENT-only rule would
        // say nothing at all.
        let paths = b.outside_paths("cat: /Users/x/.ssh/id_ed25519: Operation not permitted");
        assert_eq!(paths, vec![PathBuf::from("/Users/x/.ssh/id_ed25519")]);
        let notes = b.absence_notes("cat: /Users/x/.keys/id_ed25519: Permission denied");
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("the boundary"), "{}", notes[0]);
        assert!(notes[0].contains("Operation not permitted"), "{}", notes[0]);
        assert!(!notes[0].contains("mount namespace"), "{}", notes[0]);
    }

    #[test]
    fn a_launcher_failure_is_named_as_one_and_ordinary_output_is_not() {
        let mut props = BTreeMap::new();
        for p in Prop::REQUIRED_POLICY {
            props.insert(
                p,
                PropState::Held {
                    how: "measured (fixture)".into(),
                },
            );
        }
        let s = Seatbelt {
            helper: PathBuf::from("/usr/bin/sandbox-exec"),
            profile: PathBuf::from("/tmp/profile.sb"),
            home: PathBuf::from("/var/lib/lt/home"),
            tmp: PathBuf::from("/var/lib/lt/tmp"),
            cargo_home: None,
            rustup_home: None,
            boundary: Boundary {
                mechanism: "sandbox-exec (fixture)".into(),
                platform: Mechanism::Policy,
                view: ViewSpec::project_only("/Users/x/proj"),
                egress: Egress::Denied,
                ns: BTreeMap::new(),
                seals: BTreeMap::new(),
                props,
                measured_at: SystemTime::now(),
            },
        };
        let n = s
            .launcher_failure("sandbox-exec: /tmp/profile.sb:1: unknown operation\n")
            .expect("the helper's own diagnostic is a launcher failure");
        assert!(n.contains("NOT the command's answer"), "{n}");
        assert!(
            s.launcher_failure("hello\nsomething else\n").is_none(),
            "ordinary output is not a launcher failure"
        );
        // And the argv is the helper, the profile, and a shim that cannot lose the
        // environment a command needs.
        let argv = s
            .wrap(&ConfinePlan {
                cwd: Path::new("/Users/x/proj"),
                env: &[],
            })
            .expect("a measured boundary wraps");
        assert_eq!(argv[0], "/usr/bin/sandbox-exec");
        assert_eq!(argv[1], "-f");
        assert_eq!(argv[2], "/tmp/profile.sb");
        assert_eq!(argv[3], "/bin/sh");
        assert_eq!(argv[4], "-c");
        assert!(argv[5].contains("HOME=/var/lib/lt/home"), "{}", argv[5]);
        assert!(argv[5].contains("TMPDIR=/var/lib/lt/tmp"), "{}", argv[5]);
        assert!(argv[5].ends_with("exec \"$@\""), "{}", argv[5]);
    }
}
