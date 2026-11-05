//! macOS's boundary: `sandbox-exec -f <profile>`, a policy the kernel applies to every
//! operation the process and everything it execs performs. [`render_profile`] renders the
//! same [`ViewSpec`] Linux renders as namespaces, and the profile is measured, not believed.

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
/// refusal, which is what `tests/seatbelt.rs` does - the same seam [`super::confine::BWRAP_ENV`]
/// is on the other platform.
pub const SANDBOX_ENV: &str = "LETIBOT_SANDBOX_EXEC";

/// The read-only roots a command needs to be a command: the answer to *what does `/bin/sh`
/// need to exist*, not a security decision, which is everything not on it. `/Library` is
/// here for the `xcrun` shims and `/System` for the dyld shared cache.
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

/// The mach services a shell, `git`, `cargo`/`rustc`, `python3` and `make` need: a program wanting
/// one not here fails naming it, and the fix is one line. The keychain service is the top-level
/// seat's alone (`gh` keeps its token there); the trust daemons hold no secret, so both seats have them.
pub const MACH_SERVICES: &[&str] = &[
    "com.apple.system.logger",
    "com.apple.system.notification_center",
    "com.apple.cfprefsd.agent",
    "com.apple.SecurityServer",
    "com.apple.system.libinfo",
    "com.apple.distributed_notifications@Uv3",
    "com.apple.trustd",
    "com.apple.trustd.agent",
];

/// What a child's profile allows of [`MACH_SERVICES`]: everything a toolchain needs and not the
/// keychain, so `security find-generic-password -s gh:github.com -w` has nothing to ask.
pub fn mach_services(wide: bool) -> Vec<&'static str> {
    MACH_SERVICES
        .iter()
        .copied()
        .filter(|s| wide || *s != "com.apple.SecurityServer")
        .collect()
}

/// Denied for read and write after every allow, whatever the view grants, and relative to the
/// operator's home: a deny, because it has to win over a grant of the whole home. The credential
/// stores are [`crate::secrets::STORES`]; what is here is what only a policy can express.
pub const SEAT_UNDER_HOME: &[&str] = &[
    "Library/Cookies",
    "Library/Messages",
    "Library/Mail",
    ".local/share/letibot",
    ".config/letibot",
    ".zshrc",
    ".zprofile",
    ".zshenv",
    ".bashrc",
    ".bash_profile",
    ".profile",
    ".config/fish",
    // Files the operator's next login or shell start runs, so a write is code outside the seat.
    ".zlogin",
    ".oh-my-zsh/custom",
    "Library/LaunchAgents",
    // What the operator typed, secrets included.
    ".zsh_history",
    ".bash_history",
];

/// What a seat's profile denies, rel-to-home: the stores the gate knows, then this side's own.
/// `wide` is the top-level seat, which keeps `gh`'s config readable because that is what the
/// token it injects is for; a child has no token and no reason to read it.
pub fn deny_under_home(wide: bool) -> Vec<&'static str> {
    let mut out = crate::secrets::seat_denied(wide);
    out.extend(SEAT_UNDER_HOME.iter().copied());
    out
}

/// A file this module creates inside the session's `$HOME` so that the deny rule can be
/// measured winning over an allow: the directory is read-write in the profile and the file is
/// denied by name, so a successful read means the deny does not work and the probe refuses.
pub const CANARY: &str = ".letibot-canary";

/// The content of the canary. Not a secret and not read by anything: it exists so that a
/// successful read of it is unambiguous.
const CANARY_BODY: &str =
    "letibot: this file exists so that a boundary can be measured refusing to read it\n";

/// The paths one profile names, resolved once. Public because the renderer is public and a
/// golden test must be able to say exactly what the layout was; [`Layout::of`] is the real
/// one and a test builds the struct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// The workspace, read-write at its own path.
    pub project: PathBuf,
    /// `$HOME` inside the view - created if a view only abstractly asked for one.
    pub home: PathBuf,
    /// Where the command's `TMPDIR` points and where its scratch belongs.
    pub tmp: PathBuf,
    /// The operator's home. Only ever a source of deny paths.
    pub operator_home: PathBuf,
    /// The toolchain directories that are really there, read-only.
    pub toolchain: Vec<PathBuf>,
    /// `CARGO_HOME`/`RUSTUP_HOME` for the wrapped command, when the toolchain is in the view.
    pub cargo_home: Option<PathBuf>,
    pub rustup_home: Option<PathBuf>,
    /// The operator's git config, which `GIT_CONFIG_GLOBAL` names because the session's `$HOME`
    /// is a throwaway and a commit would otherwise have no author.
    pub git_config: Option<PathBuf>,
    /// Denied for read and write after every allow: the operator's credentials and profiles,
    /// plus whatever a caller added (the probe's canary).
    pub deny: Vec<PathBuf>,
    /// The per-user temporary directory, where the macOS toolchain's `xcrun` cache lives.
    pub user_temp: PathBuf,
    /// The view's `$HOME` is the operator's own: the filesystem is readable and the home
    /// writable, and [`DENY_UNDER_HOME`] is the only fence. No toolchain list and no redirected
    /// `*_HOME`, because the real home already makes them right.
    pub wide: bool,
    /// Unix sockets and files the command may not connect to, read or write, in both the
    /// `/var` and `/private/var` spellings: the host room coordinator's, and the view's own
    /// `hidden` paths. Seatbelt names a unix socket by `network-outbound`, not by file access.
    pub hidden: Vec<PathBuf>,
}

/// Every spelling of a path seatbelt may be asked about: macOS keeps `/var`, `/tmp` and `/etc`
/// as links into `/private`, and the sandbox compares the resolved one.
pub fn both_spellings(p: &Path) -> Vec<PathBuf> {
    let mut out = vec![p.to_path_buf()];
    if let Ok(rest) = p.strip_prefix("/private") {
        out.push(Path::new("/").join(rest));
    } else if ["/var", "/tmp", "/etc"].iter().any(|r| p.starts_with(r)) {
        out.push(Path::new("/private").join(p.strip_prefix("/").unwrap_or(p)));
    }
    out
}

impl Layout {
    /// Resolve a view's paths, and create nothing: [`Seatbelt::probe`] makes the directories,
    /// because a resolve with side effects cannot be used by a test. `dir` is outside the
    /// checkout on purpose, because a policy the command can edit is not a policy.
    pub fn of(view: &ViewSpec, operator_home: &Path, dir: &Path) -> Layout {
        let home = match &view.home {
            HomeView::Tmpfs => dir.join("home"),
            HomeView::Dir(d) => d.clone(),
        };
        let tmp = match &view.tmp {
            TmpView::PerCommand => dir.join("tmp"),
            TmpView::Session(d) => d.clone(),
        };
        // A letibot-owned home is a directory of its own; the operator's home as `$HOME` is the
        // wide seat.
        let wide = matches!(&view.home, HomeView::Dir(d)
            if d == operator_home || d.canonicalize().ok() == operator_home.canonicalize().ok());
        let mut toolchain = Vec::new();
        let mut cargo_home = None;
        let mut rustup_home = None;
        let mut git_config = None;
        for rel in super::confine::GRANT_CANDIDATES.iter().filter(|_| !wide) {
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
        // The dotfile wins: with `GIT_CONFIG_GLOBAL` set git reads that one file alone.
        for rel in [".gitconfig", ".config/git/config"] {
            let p = operator_home.join(rel);
            if !wide && p.is_file() {
                git_config = Some(p);
                break;
            }
        }
        let mut deny: Vec<PathBuf> = deny_under_home(wide)
            .into_iter()
            .map(|rel| operator_home.join(rel))
            // The view's own `masked` paths (config `hide`) are denied like the stores: a deny
            // is the one thing the profile lets win over a grant.
            .chain(view.masked.iter().flat_map(|p| both_spellings(p)))
            .collect();
        deny.sort();
        deny.dedup();
        let mut user_temp = std::env::temp_dir();
        if let Ok(real) = user_temp.canonicalize() {
            user_temp = real;
        }
        let mut hidden: Vec<PathBuf> = view
            .hidden
            .iter()
            .cloned()
            .chain(super::confine::room_files())
            .flat_map(|p| both_spellings(&p))
            .collect();
        hidden.sort();
        hidden.dedup();
        Layout {
            hidden,
            project: view.project.clone(),
            home,
            tmp,
            operator_home: operator_home.to_path_buf(),
            toolchain,
            cargo_home,
            rustup_home,
            git_config,
            deny,
            user_temp,
            wide,
        }
    }

    /// The `xcrun` cache file the macOS toolchain shims write, and the only thing in the
    /// per-user temp directory this profile allows. Those shims cache into
    /// `confstr(_CS_DARWIN_USER_TEMP_DIR)`, which is not `TMPDIR` and cannot be redirected.
    pub fn xcrun_cache(&self) -> String {
        format!("{}/xcrun_db", self.user_temp.display())
    }
}

/// The SBPL text for a view: the macOS rendering of the one policy. Deterministic, so no
/// timestamps, no hash-map iteration and every path as written. The order is load-bearing,
/// because SBPL is last-match-wins: `(deny default)` first, the denies after every allow.
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
    // `same-sandbox`, not `self`: a test runner stops the child it started, and both are inside.
    s.push_str("(allow signal (target same-sandbox))\n");
    s.push_str("(allow mach-lookup\n");
    for m in mach_services(l.wide) {
        s.push_str(&format!("  (global-name \"{m}\")\n"));
    }
    s.push_str(")\n");

    s.push_str(
        "\n; --- read: the system roots, the toolchain, the session's own directories ---\n",
    );
    s.push_str("(allow file-read-metadata)\n");
    s.push_str("(allow file-read*\n");
    // `(literal "/")` is not decoration: without it every process aborts with SIGABRT before
    // `main`, because the sandbox's own check-in reads the root directory. That reaches the
    // top-level names only; anything below needs a line of its own.
    s.push_str("  (literal \"/\")\n");
    for p in SYSTEM_ROOTS {
        s.push_str(&format!("  (subpath \"{p}\")\n"));
    }
    if l.wide {
        // The whole filesystem is readable; the denies at the end are the fence.
        s.push_str("  (subpath \"/\")\n");
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
            // Seatbelt has no operation for a unix socket, so it is reached with
            // `file-read*`/`file-write*` on its path: the one grant that puts a credential
            // inside the view without putting a key in it, so it is allowed by name only.
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
    // `/dev/tty`, because a program's own terminal is not a path it chose: stdio already
    // points there, but a program that opens its controlling terminal would be refused one it
    // can already write to. It is the session's own tty, never another user's device.
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
    // Read-only inside a read-write grant, after it.
    if !view.protected.is_empty() {
        s.push_str("\n; --- read-only inside a read-write grant ---\n(deny file-write*\n");
        for p in &view.protected {
            for q in both_spellings(&p.path) {
                s.push_str(&format!("  (subpath \"{}\")\n", q.display()));
            }
        }
        s.push_str(")\n");
    }
    // The coordinator's socket and files: no connect, no read, whatever the egress decision and
    // whatever a grant covers. Last, so nothing above can undo it.
    if !l.hidden.is_empty() {
        s.push_str("\n; --- the room coordinator: not connectable, not visible ---\n");
        for p in &l.hidden {
            s.push_str(&format!(
                "(deny network-outbound (remote unix-socket (path-literal \"{}\")))\n",
                p.display()
            ));
        }
        s.push_str("(deny file-read* file-write*\n");
        for p in &l.hidden {
            s.push_str(&format!("  (literal \"{}\")\n", p.display()));
        }
        s.push_str(")\n");
    }
    s
}

/// The script the probe runs inside the profile: one `probe <name> <outcome>` line per fact,
/// subjects handed in as arguments (`$1` the canary, `$2` outside, `$3` inside, `$4` the
/// launcher's pid). It never sends a byte: the far subject is TEST-NET-1, routed nowhere.
const PROBE: &str = "\
p() { echo \"probe $1 $2\"; }
if [ -r \"$1\" ]; then p read-denied allowed; else p read-denied denied; fi
if : > \"$2\" 2>/dev/null; then p write-outside wrote; else p write-outside denied; fi
if : > \"$3\" 2>/dev/null; then p write-inside wrote; else p write-inside denied; fi
if /bin/sh -c true 2>/dev/null; then p fork-exec ran; else p fork-exec failed; fi
if /bin/sh -c \"test -r \\\"$1\\\"\" 2>/dev/null; then p nested-read allowed; else p nested-read denied; fi
if kill -0 \"$4\" 2>/dev/null; then p signal-outside reached; else p signal-outside denied; fi
sleep 5 & c=$!; if kill $c 2>/dev/null; then p signal-child reached; else p signal-child denied; fi
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

/// What the probe measured, before it is judged.
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

/// A measured macOS boundary, and the profile file every spawn is wrapped in.
#[derive(Debug, Clone)]
pub struct Seatbelt {
    helper: PathBuf,
    profile: PathBuf,
    home: PathBuf,
    tmp: PathBuf,
    cargo_home: Option<PathBuf>,
    rustup_home: Option<PathBuf>,
    git_config: Option<PathBuf>,
    boundary: Boundary,
}

impl Seatbelt {
    /// Render the profile, run the probe inside it, and believe only what it measured. The
    /// refusals: no helper, an unwritable profile, a canary that was readable, a write inside
    /// that failed or outside that succeeded, a child unconfined, an egress that did not take.
    pub fn probe(view: ViewSpec, egress: Egress) -> Result<Seatbelt, ExecError> {
        Seatbelt::probe_with(view, egress, Vec::new())
    }

    /// [`Seatbelt::probe`], with paths added to the deny list: the seam a test needs to put a
    /// fixture's `$HOME` behind the same rule as the operator's own. The real caller passes
    /// nothing, and a test must not point the operator's real credentials at a measurement.
    pub fn probe_with(
        view: ViewSpec,
        egress: Egress,
        extra_deny: Vec<PathBuf>,
    ) -> Result<Seatbelt, ExecError> {
        Seatbelt::probe_as(view, egress, extra_deny, operator_home())
    }

    /// [`Seatbelt::probe_with`] for a named operator home, so a test can put a fixture's
    /// `.gitconfig` where the real one would be.
    pub fn probe_as(
        view: ViewSpec,
        egress: Egress,
        extra_deny: Vec<PathBuf>,
        operator_home: PathBuf,
    ) -> Result<Seatbelt, ExecError> {
        let helper = find_helper()?;
        let view = super::confine::prepared(view)?;
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
            layout.git_config.as_deref(),
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
        // The operator's own home is not a place to leave a marker in.
        if layout.wide {
            let _ = std::fs::remove_file(&canary);
        }

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

/// The refusals a measurement has to survive, in the order they matter. The first is what the
/// module is written against: a profile that is written but does not take looks exactly like
/// one that does, from everywhere except inside.
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

    // The egress decision, measured against the decision. A run that asked for the host's
    // network and did not get it is a refusal and not a partial boundary: every `curl` inside
    // would fail while the disclosure said the opposite.
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

    let signal = match (f("signal-outside"), f("signal-child")) {
        (Some(Outcome::Denied), Some(Outcome::Denied)) => PropState::Absent {
            why: "a command inside could not stop its own background child".into(),
        },
        (Some(Outcome::Denied), _) => PropState::Held {
            how: "`kill -0 <this process's own launcher>` from inside was `EPERM`, and a child \
                  of the command stayed killable"
                .into(),
        },
        (Some(Outcome::Reached), _) => PropState::Absent {
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

    // The view as it will actually be, not as it was asked for: `$HOME` and `/tmp` are the
    // resolved directories, so a disclosure and an `ENOENT` note describe the same place.
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
        git_config: layout.git_config,
        boundary,
    })
}

/// The argv, from the helper up to but not including the command: `sandbox-exec -f <profile>`
/// and then a `/bin/sh` shim, because a profile cannot set an environment and `wrap` returns
/// an argv. `HOME`, `TMPDIR`, `CARGO_HOME` and `RUSTUP_HOME` all have to name an allowed path.
fn wrap_with(
    helper: &Path,
    profile: &Path,
    home: &Path,
    tmp: &Path,
    cargo_home: Option<&Path>,
    rustup_home: Option<&Path>,
    git_config: Option<&Path>,
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
    if let Some(g) = git_config {
        export.push_str(&format!(
            " GIT_CONFIG_GLOBAL={}",
            crate::remote::quote(&g.display().to_string())
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

    fn spares_operator(&self) -> bool {
        true
    }

    fn wrap(&self, _plan: &ConfinePlan<'_>) -> Result<Vec<String>, ExecError> {
        // The cwd is not the profile's business: the caller sets it on the `Command`, and the
        // profile names the checkout as writable wherever the command starts from.
        Ok(wrap_with(
            &self.helper,
            &self.profile,
            &self.home,
            &self.tmp,
            self.cargo_home.as_deref(),
            self.rustup_home.as_deref(),
            self.git_config.as_deref(),
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

    /// `sandbox-exec`'s own diagnostics are prefixed `sandbox-exec: `, the same heuristic
    /// [`super::confine::Bwrap`] uses for `bwrap: ` and with the same caveat: a command that
    /// printed such a line is misattributed, which beats calling a failed launch the answer.
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

/// A directory of this daemon's own for profiles, outside every checkout: per process and in
/// the per-user temp directory, like `ProcessGroups::probe`'s scope root, so the operator can
/// find the policy but it never sits inside the tree the command can write.
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

// The `Piece` form is unneeded by a local spawn - a policy has no far side - and the
// [`Confinement`] default is exactly right for it, so nothing here overrides it.

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
            git_config: None,
            deny: vec![
                PathBuf::from("/Users/x/.ssh"),
                PathBuf::from("/Users/x/.zshrc"),
            ],
            user_temp: PathBuf::from("/private/var/folders/aa/bb/T"),
            wide: false,
            hidden: Vec::new(),
        }
    }

    #[test]
    fn the_coordinator_socket_is_denied_to_connect_in_both_spellings_after_every_allow() {
        let mut l = layout();
        l.hidden = both_spellings(Path::new("/var/folders/aa/room-k.sock"));
        let view = ViewSpec::project_only("/Users/x/proj");
        for egress in [
            Egress::Denied,
            Egress::Host {
                why: "the build needs a registry".into(),
            },
        ] {
            let p = render_profile(&view, &egress, &l);
            for path in [
                "/var/folders/aa/room-k.sock",
                "/private/var/folders/aa/room-k.sock",
            ] {
                let rule = format!(
                    "(deny network-outbound (remote unix-socket (path-literal \"{path}\")))"
                );
                let at = p.find(&rule).unwrap_or_else(|| panic!("{rule} in\n{p}"));
                if let Some(allow) = p.rfind("(allow ") {
                    assert!(at > allow, "the deny must come after every allow:\n{p}");
                }
            }
            assert!(
                p.contains("(literal \"/private/var/folders/aa/room-k.sock\")"),
                "{p}"
            );
        }
    }

    #[test]
    fn a_private_path_has_its_plain_spelling_too() {
        assert_eq!(
            both_spellings(Path::new("/private/var/x")),
            vec![PathBuf::from("/private/var/x"), PathBuf::from("/var/x")]
        );
        assert_eq!(both_spellings(Path::new("/Users/x/a")).len(), 1);
    }

    /// The golden: the whole profile, character for character. A policy that changes without
    /// anybody noticing is what this test prevents, and a `contains` assertion would let a
    /// rule be added or reordered silently.
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
(allow signal (target same-sandbox))
(allow mach-lookup
  (global-name \"com.apple.system.logger\")
  (global-name \"com.apple.system.notification_center\")
  (global-name \"com.apple.cfprefsd.agent\")
  (global-name \"com.apple.system.libinfo\")
  (global-name \"com.apple.distributed_notifications@Uv3\")
  (global-name \"com.apple.trustd\")
  (global-name \"com.apple.trustd.agent\")
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
        // The one place the weaker mechanism is stricter, and why the denies are emitted last:
        // a grant is the operator widening the view, and a bind of the home directory on Linux
        // would carry `~/.ssh` inside it. Here it cannot.
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

    /// The keychain is a mach service, and `gh` keeps its token in it: the top-level seat is given
    /// a token and may ask, a child has neither and is not allowed the service at all.
    #[test]
    fn the_keychain_service_is_the_top_level_seats_alone() {
        let view = ViewSpec::project_only("/Users/x/proj");
        let tight = render_profile(&view, &Egress::Denied, &layout());
        assert!(
            !tight.contains("com.apple.SecurityServer"),
            "a child's profile may not reach the keychain:\n{tight}"
        );
        assert!(
            tight.contains("com.apple.cfprefsd.agent"),
            "and everything a toolchain needs is still allowed: {tight}"
        );
        let mut wide = layout();
        wide.wide = true;
        let wide = render_profile(&view, &Egress::Denied, &wide);
        assert!(
            wide.contains("com.apple.SecurityServer"),
            "the top-level seat asks the keychain for the token it pushes with:\n{wide}"
        );
    }

    #[test]
    fn certificate_trust_evaluation_is_allowed_in_both_seats() {
        let view = ViewSpec::project_only("/Users/x/proj");
        let mut wide = layout();
        wide.wide = true;
        for l in [layout(), wide] {
            let p = render_profile(&view, &Egress::Denied, &l);
            for m in ["com.apple.trustd", "com.apple.trustd.agent"] {
                assert!(p.contains(&format!("(global-name \"{m}\")")), "{m}: {p}");
            }
        }
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
        for rel in deny_under_home(false) {
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
        // this daemon made, and the boundary records it as such - see `Seatbelt::probe`.
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
        // Every required property held, egress denied, and the one seal macOS cannot have is
        // not applicable rather than missing.
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
            git_config: None,
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
