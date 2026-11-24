//! The queues' per-project settings - the `[queue]` table of the project's `leticode.toml`
//! found by walking up from the repository the way every `leticode.toml` is found.
//!
//! ```toml
//! [queue]
//! target = "feat/memory-engine"   # the branch this project lands on
//! merge_via_session = false       # a review that cannot run is handed to the
//!                                 #   project's main session to gate and merge (default: no -
//!                                 #   raise only)
//! red_target = "fix"              # when the target goes red, the reviewer of the
//!                                 #   red tip opens a fix branch through the queue ("fix",
//!                                 #   the default) or the operator is only told ("raise")
//! gate_carry = ["images", "vendor"] # gitignored inputs the gate needs, COPIED from the
//!                                 #   operator's checkout into the landing tree before every
//!                                 #   gate run (default: none)
//! reviewers = "host"              # where this project's reviewers run: `host` (the default -
//!                                 #   on this machine, as the children that author the
//!                                 #   branches do) or `vm` (each reviewer in a firecode
//!                                 #   microVM)
//! gate_host = "lab2x1"             # the gate's machine, when it is not this one: the gate's
//!                                 #   steps run on a host the operator declared in
//!                                 #   `hosts.toml` (default: empty - this machine)
//! gate_poll_seconds = 5.0         # how long the gate waits between two polls of a running
//!                                 #   step there, ±10% each time so a poll does not land on
//!                                 #   the same point of the host's own work (default: 5.0)
//! gate_train = 3                  # how many approved branches one gate may judge together:
//!                                 #   they are rebased one onto the last, gated ONCE, and
//!                                 #   landed one commit at a time. A red stack is bisected
//!                                 #   until the failing branch is parked (default: 1 - one
//!                                 #   branch, one gate)
//! reviewers_at_once = 4           # how many reviewers this queue runs AT ONCE. Default:
//!                                 #   `vm` is the box's VM slots; `host` is what the pool
//!                                 #   of machines can take, from their cores
//! gate_in = "firecode"            # run the gate's steps inside a firecode VM over the gate's
//!                                 #   checkout (read-only, nothing copied back). A machine
//!                                 #   without a working firecode leaves the tip UNJUDGED.
//!                                 #   Default: "host" - the steps run on the gate machine
//! reviewer_hosts = ["lab2x1"]     # opt in: the declared hosts a `host` reviewer may be placed
//!                                 #   on (`["*"]` is all of them). Default: this machine only
//! reviewers_run_suites = true     # opt in: a reviewer's `bash` may run whole test suites.
//!                                 #   Default: refused, a reviewer runs single named tests
//! gate_deadline_minutes = 45      # the deadline of one whole gate run, counted from when the
//!                                 #   box's gate lock is held. Default: 30
//! gate_cache_root = "/build/letibot-gate"  # where each gate's BUILD CACHE lives -
//!                                 #   `<root>/<repo>-<hex>` on the box's build disk rather
//!                                 #   than `<placement>/target` on whatever disk the placement
//!                                 #   landed on. Default: the placement's own `target/`
//! gate_cache_days = 3             # a gate placement's `target/` nothing has written to for
//!                                 #   this long is removed when the next gate starts, or the
//!                                 #   daemon does. Default: 3
//! gate_min_free = "30G"           # free space the filesystem holding a gate's target - and
//!                                 #   the landing root - must have before a gate starts: the
//!                                 #   stale caches are removed first, and if that is not
//!                                 #   enough the gate does NOT run. Default: 30G
//! [queue.claude]                  # the reviewers are headless `claude -p` runs instead of
//!                                 #   letibot children. The table absent is the letibot
//!                                 #   reviewer.
//! binary = "/usr/local/bin/claude" # the executable; an absolute path
//! allowed_tools = ["Read", "Grep"] # `--allowedTools`, default Read, Grep, Glob and the three
//!                                 #   read-only git commands a diff needs
//! permission_mode = "default"     # `--permission-mode`, default "default"
//! timeout_seconds = 3600          # the row's own deadline, default the queue's hour
//! ```
//!
//! `gate_carry` exists because the landing tree is a worktree with nothing gitignored. Entries stay
//! inside the repository: absolute, `..`, `.git` and the root are refused here, a symlink out, a
//! missing or a tracked path at gate time. Copied, never symlinked, so a gate cannot write back.

use std::path::{Path, PathBuf};

use crate::leticode_config::LeticodeConfig;

/// What a red target sets in motion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedTarget {
    /// The reviewer of the red tip opens a fix branch, through the queue like any other, and
    /// the operator is told.
    Fix,
    /// The operator is told, and nothing is opened.
    Raise,
}

impl RedTarget {
    pub fn as_str(self) -> &'static str {
        match self {
            RedTarget::Fix => "fix",
            RedTarget::Raise => "raise",
        }
    }
}

/// Where a project's reviewers run - `[queue] reviewers`, and the one thing a project
/// can say about the review queue's own children. The key is named for WHAT it chooses
/// (which of the queue's children, and where they run) rather than for the machinery
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Reviewers {
    /// On this machine - a reviewer is a `gatekeeper`-seated child on the host, in a
    /// checkout of the head it judges. The default.
    #[default]
    Host,
    /// In a firecode microVM - the VM is the boundary, so the reviewer's `bash` asks nobody.
    Vm,
}

impl Reviewers {
    pub fn as_str(self) -> &'static str {
        match self {
            Reviewers::Host => "host",
            Reviewers::Vm => "vm",
        }
    }

    /// The two words, and nothing else - a value this build does not know is refused by name
    /// with both of them in the sentence, rather than defaulted.
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim() {
            "host" => Ok(Reviewers::Host),
            "vm" => Ok(Reviewers::Vm),
            other => Err(format!(
                "reviewers = \"{other}\" — the words are \"host\" (a reviewer runs on this \
                 machine, in a checkout of the head it judges — the default) and \"vm\" (each \
                 reviewer is booted in a firecode microVM)"
            )),
        }
    }
}

/// Where a gate's steps run on the gate machine - `[queue] gate_in`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GateIn {
    /// Directly on the gate machine. The default.
    #[default]
    Host,
    /// Inside a firecode VM over the gate's checkout.
    Firecode,
    /// `[seat] default` decides, resolved when the gate starts. Never what a file says: a gate
    /// whose suite boots VMs cannot run in one, so an unplaced gate is `Host`.
    Seat,
}

impl GateIn {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim() {
            "host" => Ok(GateIn::Host),
            "firecode" => Ok(GateIn::Firecode),
            other => Err(format!(
                "gate_in = \"{other}\" - the words are \"host\" (the steps run on the gate \
                 machine - the default) and \"firecode\" (each step runs in a firecode VM)"
            )),
        }
    }

    /// The word a resolved gate runs under, for a row that has to say where it ran.
    pub fn as_str(self) -> &'static str {
        match self {
            GateIn::Host => "host",
            GateIn::Firecode => "firecode",
            GateIn::Seat => "the seat's default",
        }
    }
}

/// One project's queue settings, with where they came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueSettings {
    /// The branch this project lands on; `None` is *the repository's default* (`main`, else
    /// `master`, else what the main checkout has out).
    pub target: Option<String>,
    /// Decision 4's option. Default `false`: raise only.
    pub merge_via_session: bool,
    /// Decision 11's default. `Fix` unless the project says `raise`.
    pub red_target: RedTarget,
    /// Gitignored paths (relative to the repository's root) copied from the operator's checkout
    /// into the landing tree before every gate run. Empty by default. Each entry has passed
    /// [`super::carry::validate`].
    pub gate_carry: Vec<String>,
    /// Where this project's reviewers run ([`Reviewers`]). `Host` unless the project says `vm`.
    pub reviewers: Reviewers,
    /// How many approved branches one gate may judge together - `[queue] gate_train`. `1`
    /// is one branch and one gate, which is what every project gets until it says otherwise and
    /// what the queue did before this key existed: nothing about the default path changed.
    pub gate_train: usize,
    /// Where this project's GATE runs - `[queue] gate_host`, a name from the operator's
    /// `hosts.toml`, or `None` for this machine (the default, and what every project gets until
    /// it says otherwise). The name is not checked here
    pub gate_host: Option<String>,
    /// How long the gate waits between two polls of a running step - `[queue]
    /// gate_poll_seconds`. [`super::gate::GATE_POLL_DEFAULT`] unless the project says otherwise;
    /// the poll is also the step's poke, so [`super::gate::GATE_POLL_LONGEST`] is a ceiling.
    pub gate_poll: std::time::Duration,
    /// Whether the gate's steps run in a firecode VM - `[queue] gate_in`.
    pub gate_in: GateIn,
    /// The skills this project's reviewers are started with - `[queue] reviewer_skills`. The
    /// bodies are preloaded into each reviewer's prompt, on top of the index every session gets
    /// (`Config::skills`, which the review queue's session carries). Empty is the ordinary case.
    pub reviewer_skills: Vec<String>,
    /// `[queue] reviewers_at_once`. `None` is the placement's own number
    /// ([`super::capacity::review_limit`]).
    pub reviewers_at_once: Option<usize>,
    /// `[queue] reviewer_hosts`, the opt-in to fan reviewers out. Absent is this machine alone
    /// (`["local"]`). `None` is every declared host (`["*"]`). A list is exactly those names.
    pub reviewer_hosts: Option<Vec<String>>,
    /// `[queue] reviewers_run_suites`: a reviewer's `bash` may run whole test suites. Off by
    /// default, because the gate runs the suite and a reviewer that repeats it dies at its limits.
    pub reviewers_run_suites: bool,
    /// `[queue] gate_deadline_minutes`: one gate run's deadline. `None` is the daemon's thirty
    /// minutes ([`super::gate::GATE_DEADLINE`]).
    pub gate_deadline: Option<std::time::Duration>,
    /// `[queue] grant_ttl_hours`: how long a session answer on a relayed queue card covers the
    /// queue. It covers every later reviewer, so it is bounded in time rather than by one seat.
    pub grant_ttl_hours: u64,
    /// `[queue.claude]`, when this project's reviewers are Claude Code runs (`claude -p`) instead
    /// of letibot children. `None`, the table absent, is the letibot reviewer.
    pub claude: Option<super::review::claude::Settings>,
    /// `[queue] gate_min_free`: free space the filesystem holding a gate's target - and the
    /// landing root - must have before a gate starts. [`GATE_MIN_FREE`] unless the project says
    /// otherwise.
    pub gate_min_free: u64,
    /// `[queue] gate_cache_root`: where every gate's build cache lives, `<root>/<repo>-<hex>` - a
    /// directory on the box's build disk rather than under the placement. `None` keeps the
    /// placement's own `target/`, which is every gate's directory until an operator says this.
    pub gate_cache_root: Option<PathBuf>,
    /// `[queue] gate_cache_days`: a gate placement's `target/` nothing has written to for this
    /// many days is removed. [`GATE_CACHE_DAYS`] unless the project says otherwise.
    pub gate_cache_days: u64,
    /// The file they were read from, for the pane; `None` when there was none.
    pub path: Option<PathBuf>,
}

impl Default for QueueSettings {
    fn default() -> Self {
        QueueSettings {
            target: None,
            merge_via_session: false,
            red_target: RedTarget::Fix,
            gate_carry: Vec::new(),
            reviewers: Reviewers::Host,
            gate_train: 1,
            gate_host: None,
            gate_poll: super::gate::GATE_POLL_DEFAULT,
            // A gate is a seat with no head: `[seat] default` decides, and the daemon resolves it
            // when the gate starts. A fixture that wants this machine says `GateIn::Host`.
            gate_in: GateIn::Host,
            reviewer_skills: Vec::new(),
            reviewers_at_once: None,
            reviewer_hosts: Some(vec!["local".to_string()]),
            reviewers_run_suites: false,
            gate_deadline: None,
            grant_ttl_hours: GRANT_TTL_HOURS,
            gate_min_free: GATE_MIN_FREE,
            gate_cache_root: None,
            gate_cache_days: GATE_CACHE_DAYS,
            claude: None,
            path: None,
        }
    }
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct QueueTable {
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    merge_via_session: Option<bool>,
    #[serde(default)]
    red_target: Option<String>,
    #[serde(default)]
    gate_carry: Vec<String>,
    #[serde(default)]
    reviewers: Option<String>,
    #[serde(default)]
    gate_host: Option<String>,
    /// A real number, because a poll every second and a half is a setting an operator may want.
    /// `f64` so that a zero, a negative and a nonsense are refused by name below.
    #[serde(default)]
    gate_poll_seconds: Option<f64>,
    #[serde(default)]
    gate_in: Option<String>,
    /// The reviewer's declared skills; a name that resolves to nothing is refused by the `task`
    /// tool before the child starts, not silently dropped.
    #[serde(default)]
    reviewer_skills: Vec<String>,
    /// `i64`, not `usize`, so that `0` and a negative number are refused with a sentence naming
    /// the key rather than with a deserialiser's complaint about a type.
    #[serde(default)]
    gate_train: Option<i64>,
    #[serde(default)]
    reviewers_at_once: Option<i64>,
    #[serde(default)]
    reviewer_hosts: Option<Vec<String>>,
    #[serde(default)]
    reviewers_run_suites: Option<bool>,
    #[serde(default)]
    grant_ttl_hours: Option<i64>,
    #[serde(default)]
    gate_deadline_minutes: Option<i64>,
    /// `[queue] gate_cache_root`: a path as the box reads it, `~` or absolute. Refused rather
    /// than guessed when it is neither.
    #[serde(default)]
    gate_cache_root: Option<String>,
    /// `i64` for the reason `gate_train` is: `0` is refused by name.
    #[serde(default)]
    gate_cache_days: Option<i64>,
    /// A size as `parse_size` reads it (`30G`, `512M`). A number nothing can parse is refused by
    /// name, never defaulted - a floor that quietly became 30 GiB is a rule nobody can read.
    #[serde(default)]
    gate_min_free: Option<String>,
    /// `[queue.claude]`: present is *this project's reviewers are Claude Code runs*.
    #[serde(default)]
    claude: Option<ClaudeTable>,
    /// The census's two numbers, read by `leticode-config`'s `[queue]` parser (`Config::leticode`).
    /// Accepted here only so a project writing them does not make this reader refuse the table.
    #[serde(default)]
    #[allow(dead_code)]
    stuck_after_seconds: Option<i64>,
    #[serde(default)]
    #[allow(dead_code)]
    stuck_repeat_seconds: Option<i64>,
    /// Read by `leticode-config` too; accepted here for the same reason.
    #[serde(default)]
    #[allow(dead_code)]
    mute: Vec<String>,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ClaudeTable {
    #[serde(default)]
    binary: Option<String>,
    #[serde(default)]
    allowed_tools: Vec<String>,
    #[serde(default)]
    permission_mode: Option<String>,
    /// `i64`, not `usize`, so that `0` and a negative number are refused with a sentence naming the
    /// key rather than with a deserialiser's complaint about a type.
    #[serde(default)]
    timeout_seconds: Option<i64>,
}

#[derive(Debug, Default, serde::Deserialize)]
struct FileWithQueue {
    #[serde(default)]
    queue: Option<toml::Value>,
}

/// A queue grant's life when `[queue] grant_ttl_hours` is not set.
pub const GRANT_TTL_HOURS: u64 = 24;

/// A gate's build cache is removed after this many days with nothing written to it - `[queue]
/// gate_cache_days`. Three days: a gate runs on every landing, so a cache untouched for three is
/// one whose repository stopped landing there, not one between two runs.
pub const GATE_CACHE_DAYS: u64 = 3;

/// Free space the filesystem holding a gate's target must have before a gate starts - `[queue]
/// gate_min_free`, and the same floor for the landing root. Thirty GiB: the incident this exists
/// for ended with 4 GiB free and firecode refusing to boot a VM under 6.
pub const GATE_MIN_FREE: u64 = 30 << 30;

/// The settings from the nearest `leticode.toml` at or above `repo`. No file or no `[queue]` is
/// the defaults. A `[queue]` that does not parse or names an unknown key is an `Err`, never a default.
pub fn for_repository(repo: &Path) -> Result<QueueSettings, String> {
    let Some(path) = LeticodeConfig::discover(repo) else {
        return Ok(QueueSettings::default());
    };
    parse(&path)
}

/// Parse one file's `[queue]` table.
pub fn parse(path: &Path) -> Result<QueueSettings, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let file: FileWithQueue =
        toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let Some(table) = file.queue else {
        return Ok(QueueSettings {
            path: Some(path.to_path_buf()),
            ..Default::default()
        });
    };
    let q: QueueTable = table
        .try_into()
        .map_err(|e| format!("{}: [queue]: {e}", path.display()))?;
    let red_target = match q.red_target.as_deref().map(str::trim) {
        None | Some("fix") => RedTarget::Fix,
        Some("raise") => RedTarget::Raise,
        Some(other) => {
            return Err(format!(
                "{}: [queue] red_target = \"{other}\" — the words are \"fix\" (the reviewer of the \
                 red tip opens a fix branch through the queue) and \"raise\" (the operator is \
                 told, nothing is opened)",
                path.display()
            ));
        }
    };
    let reviewers = match q.reviewers.as_deref().map(str::trim) {
        None => Reviewers::Host,
        Some(w) => Reviewers::parse(w).map_err(|e| format!("{}: [queue] {e}", path.display()))?,
    };
    let gate_in = match q.gate_in.as_deref() {
        // Nobody said: the host. A gate whose own suite boots VMs cannot run inside one, so the
        // seat default (a VM) never moves a queue gate; `gate_in = "firecode"` is the ask for one.
        None => GateIn::Host,
        Some(w) => GateIn::parse(w).map_err(|e| format!("{}: [queue] {e}", path.display()))?,
    };
    let gate_train = match q.gate_train {
        None => 1,
        Some(n) if n >= 1 => n as usize,
        Some(other) => {
            return Err(format!(
                "{}: [queue] gate_train = {other} — it is how many approved branches one gate may \
                 judge together, so it is 1 (one branch, one gate — the default) or more. `0` would \
                 mean a train that never runs a gate",
                path.display()
            ));
        }
    };
    let grant_ttl_hours = match q.grant_ttl_hours {
        None => GRANT_TTL_HOURS,
        Some(n) if n >= 1 => n as u64,
        Some(other) => {
            return Err(format!(
                "{}: [queue] grant_ttl_hours = {other} - it is how long a queue grant lasts, so \
                 it is 1 or more (the default is {GRANT_TTL_HOURS})",
                path.display()
            ));
        }
    };
    let gate_deadline = match q.gate_deadline_minutes {
        None => None,
        Some(n) if n >= 1 => Some(std::time::Duration::from_secs(n as u64 * 60)),
        Some(other) => {
            return Err(format!(
                "{}: [queue] gate_deadline_minutes = {other} - it is how long one gate run may \
                 take, so it is 1 or more (the default is 30)",
                path.display()
            ));
        }
    };
    // A poll is also the step's poke, so an interval the watchdog's window cannot carry would end
    // a healthy step: two polls, one of them missed, have to fit inside that window.
    let gate_poll = match q.gate_poll_seconds {
        None => super::gate::GATE_POLL_DEFAULT,
        Some(s) => {
            let longest = super::gate::GATE_POLL_LONGEST.as_secs_f64();
            // 0.1 s is below one ssh round trip, so a smaller number could only mean a tighter
            // loop - the one setting that would spend the CPU this parameter exists to save.
            if !(s.is_finite() && s >= 0.1 && s <= longest) {
                return Err(format!(
                    "{}: [queue] gate_poll_seconds = {s} - it is how long the gate waits between \
                     two polls of a running step, so it is a number of seconds from 0.1 to {longest} \
                     (the default is {}). Each poll is also what tells the step that the daemon is \
                     still there, and a longer gap than that ends a healthy step",
                    path.display(),
                    super::gate::GATE_POLL_DEFAULT.as_secs_f64()
                ));
            }
            std::time::Duration::from_secs_f64(s)
        }
    };
    let mut gate_carry = Vec::new();
    for entry in &q.gate_carry {
        let rel = super::carry::validate(entry).map_err(|e| format!("{}: {e}", path.display()))?;
        gate_carry.push(rel.to_string_lossy().into_owned());
    }
    // A number of reviewers, or nothing. Zero is refused rather than read as "no reviewers":
    // a queue that claims nothing is a queue nobody can tell from a broken one, and an operator
    // who wants none wants `reviewers` left alone and the target not red.
    let reviewers_at_once = match q.reviewers_at_once {
        None => None,
        Some(n) if n >= 1 => Some(n as usize),
        Some(n) => {
            return Err(format!(
                "{}: [queue] reviewers_at_once = {n} - a queue runs at least one reviewer at a \
                 time, so the smallest number that means anything here is 1 (and leaving the key \
                 out is the placement's own number)",
                path.display()
            ));
        }
    };
    // Absent is this machine. `["*"]` becomes `None`, every declared host. An empty entry
    // and a forbidden host are refused by name rather than dropped.
    let reviewer_hosts = match q.reviewer_hosts {
        None => Some(vec![super::review::hosts::LOCAL.to_string()]),
        Some(list) => {
            let mut out: Vec<String> = Vec::new();
            for entry in &list {
                let name = entry.trim();
                if name.is_empty() {
                    return Err(format!(
                        "{}: [queue] reviewer_hosts has an empty entry - every entry is a host \
                         name the operator declared in `hosts.toml`, or `\"local\"` for this \
                         machine",
                        path.display()
                    ));
                }
                if super::review::hosts::is_forbidden(name) {
                    return Err(format!(
                        "{}: [queue] reviewer_hosts names `{name}`, which is never used",
                        path.display()
                    ));
                }
                if !out.iter().any(|e| e == name) {
                    out.push(name.to_string());
                }
            }
            if out.iter().any(|e| e == "*") {
                None
            } else {
                Some(out)
            }
        }
    };
    // Where a gate's build cache goes, and the two numbers that decide when it is given back.
    let gate_cache_root = match q.gate_cache_root.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(p) if p.starts_with('/') || p.starts_with("~") => Some(PathBuf::from(p)),
        Some(other) => {
            return Err(format!(
                "{}: [queue] gate_cache_root = \"{other}\" - it is where every gate's build \
                 cache lives (`<root>/<repo>-<hex>`), so it is an absolute path or one starting \
                 with `~`: a relative path would name a directory inside whichever tree a gate \
                 happened to be gating",
                path.display()
            ));
        }
    };
    let gate_cache_days = match q.gate_cache_days {
        None => GATE_CACHE_DAYS,
        Some(n) if n >= 1 => n as u64,
        Some(other) => {
            return Err(format!(
                "{}: [queue] gate_cache_days = {other} - it is how many days a gate's build \
                 cache may go unwritten before it is removed, so it is 1 or more (the default is \
                 {GATE_CACHE_DAYS})",
                path.display()
            ));
        }
    };
    // A size, or nothing. A word nothing can read is refused by name: this floor is the one thing
    // between a full root disk and a gate run, and a typo must not silently mean the default.
    let gate_min_free = match q.gate_min_free.as_deref().map(str::trim) {
        None | Some("") => GATE_MIN_FREE,
        Some(size) => letibot_tools::exec::diskio::parse_size(size).map_err(|e| {
            format!(
                "{}: [queue] gate_min_free = \"{size}\" - {e}. It is the free space the \
                 filesystem holding a gate's target - and the landing root - must have before a \
                 gate starts (the default is {}), written as `30G` or `512M`",
                path.display(),
                letibot_tools::exec::scope::human_bytes(GATE_MIN_FREE)
            )
        })?,
    };
    Ok(QueueSettings {
        gate_carry,
        gate_in,
        reviewers,
        gate_host: q
            .gate_host
            .map(|h| h.trim().to_string())
            .filter(|h| !h.is_empty()),
        gate_poll,
        reviewers_at_once,
        reviewer_hosts,
        reviewers_run_suites: q.reviewers_run_suites.unwrap_or(false),
        gate_deadline,
        grant_ttl_hours,
        gate_min_free,
        gate_cache_root,
        gate_cache_days,
        claude: match q.claude {
            None => None,
            Some(c) => Some(claude_settings(&c, path)?),
        },
        target: q
            .target
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty()),
        merge_via_session: q.merge_via_session.unwrap_or(false),
        red_target,
        gate_train,
        reviewer_skills: q
            .reviewer_skills
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        path: Some(path.to_path_buf()),
    })
}

/// `[queue.claude]` as the reviewer's settings. A missing or relative binary, an empty tool and a
/// timeout of nothing are refused BY NAME - the table is the one switch a project has for running
/// a reviewer that is not a session, and a typo in it must not be read as *off*.
fn claude_settings(
    c: &ClaudeTable,
    path: &Path,
) -> Result<super::review::claude::Settings, String> {
    let binary = c.binary.as_deref().map(str::trim).unwrap_or_default();
    if binary.is_empty() {
        return Err(format!(
            "{}: [queue.claude] names no `binary` - this table is how a project says its reviewers \
             are headless `claude -p` runs, so it is the path of that executable, or the table is \
             left out and the letibot reviewer runs as before",
            path.display()
        ));
    }
    if !Path::new(binary).is_absolute() {
        return Err(format!(
            "{}: [queue.claude] binary = \"{binary}\" - it is an absolute path to the `claude` \
             executable, because a run's working directory is the reviewer's clone of the branch \
             and a relative path would name a file inside it",
            path.display()
        ));
    }
    let mut settings = super::review::claude::Settings::new(PathBuf::from(binary));
    if !c.allowed_tools.is_empty() {
        let mut tools: Vec<String> = Vec::new();
        for entry in &c.allowed_tools {
            let name = entry.trim();
            if name.is_empty() {
                return Err(format!(
                    "{}: [queue.claude] allowed_tools has an empty entry - every entry is a tool \
                     name the run may use, and a reviewer allowed nothing can read nothing",
                    path.display()
                ));
            }
            if !tools.iter().any(|t| t == name) {
                tools.push(name.to_string());
            }
        }
        settings.allowed_tools = tools;
    }
    if let Some(mode) = c.permission_mode.as_deref().map(str::trim) {
        if mode.is_empty() {
            return Err(format!(
                "{}: [queue.claude] permission_mode is empty - leave the key out for the run's \
                 own mode (`{}`), or name the one Claude Code should start in",
                path.display(),
                super::review::claude::DEFAULT_PERMISSION_MODE
            ));
        }
        settings.permission_mode = mode.to_string();
    }
    match c.timeout_seconds {
        None => {}
        Some(n) if n >= 1 => settings.timeout = std::time::Duration::from_secs(n as u64),
        Some(n) => {
            return Err(format!(
                "{}: [queue.claude] timeout_seconds = {n} - it is how long a review run may take \
                 before the queue ends it with SIGINT, so the smallest number that means anything \
                 here is 1 (and leaving the key out is the queue's own hour)",
                path.display()
            ));
        }
    }
    Ok(settings)
}

/// The target for `repo` - the project's, else the repository's default.
pub fn target_for(repo: &Path, settings: &QueueSettings) -> Option<String> {
    settings
        .target
        .clone()
        .or_else(|| super::git::default_target(repo))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(text: &str) -> (PathBuf, PathBuf) {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "letibot-queue-settings-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(dir.join("repo")).unwrap();
        std::fs::write(dir.join("leticode.toml"), text).unwrap();
        (dir.clone(), dir.join("repo"))
    }

    /// Decisions 4, 11 and 12 are per project, read from the project's own file, walked up to.
    #[test]
    fn a_project_names_its_target_its_red_policy_and_its_fallback() {
        let (dir, repo) = file(
            "main_model = \"local\"\n[queue]\ntarget = \"feat/memory-engine\"\n\
             merge_via_session = true\nred_target = \"raise\"\n",
        );
        let s = for_repository(&repo).unwrap();
        assert_eq!(s.target.as_deref(), Some("feat/memory-engine"));
        assert!(s.merge_via_session);
        assert_eq!(s.red_target, RedTarget::Raise);
        // The model reader still reads the same file.
        assert!(LeticodeConfig::load(&dir.join("leticode.toml")).is_ok());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn gate_deadline_minutes_is_read_and_zero_is_refused() {
        let (dir, repo) = file("[queue]\ngate_deadline_minutes = 45\n");
        assert_eq!(
            for_repository(&repo).unwrap().gate_deadline,
            Some(std::time::Duration::from_secs(45 * 60))
        );
        std::fs::write(
            dir.join("leticode.toml"),
            "[queue]\ngate_deadline_minutes = 0\n",
        )
        .unwrap();
        let e = for_repository(&repo).unwrap_err();
        assert!(e.contains("gate_deadline_minutes = 0"), "{e}");
        std::fs::write(dir.join("leticode.toml"), "[queue]\n").unwrap();
        assert_eq!(for_repository(&repo).unwrap().gate_deadline, None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn no_table_is_the_defaults_and_an_unknown_key_is_refused_by_name() {
        let (dir, repo) = file("main_model = \"local\"\n");
        let s = for_repository(&repo).unwrap();
        assert_eq!(s.target, None);
        assert!(
            !s.merge_via_session,
            "decision 4: raise only, unless the project opts in"
        );
        assert_eq!(s.red_target, RedTarget::Fix);
        assert!(s.gate_carry.is_empty(), "nothing is carried unless named");
        std::fs::write(
            dir.join("leticode.toml"),
            "[queue]\ntarget_branch = \"x\"\n",
        )
        .unwrap();
        let e = for_repository(&repo).unwrap_err();
        assert!(e.contains("target_branch"), "{e}");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// `reviewers` says where this project's reviewers run, and `host` is what a project that
    /// says nothing gets. A word this build does not
    /// know is refused BY NAME with both of them in the sentence, like every other key here.
    #[test]
    fn reviewers_defaults_to_host_and_a_word_this_build_does_not_know_is_refused() {
        let (dir, repo) = file("main_model = \"local\"\n[queue]\ntarget = \"main\"\n");
        assert_eq!(for_repository(&repo).unwrap().reviewers, Reviewers::Host);
        std::fs::write(dir.join("leticode.toml"), "[queue]\nreviewers = \"vm\"\n").unwrap();
        assert_eq!(for_repository(&repo).unwrap().reviewers, Reviewers::Vm);
        std::fs::write(
            dir.join("leticode.toml"),
            "[queue]\nreviewers = \"firecode\"\n",
        )
        .unwrap();
        let e = for_repository(&repo).unwrap_err();
        assert!(e.contains("\"firecode\""), "{e}");
        assert!(e.contains("\"host\"") && e.contains("\"vm\""), "{e}");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The census's two numbers are `leticode-config`'s - read from the user's file and the
    /// project's over it - and this reader ACCEPTS them rather than refusing the whole table. A
    /// project that moves its census's threshold must not lose its target.
    #[test]
    fn the_census_numbers_are_accepted_here_and_read_by_the_config() {
        let (dir, repo) = file(
            "[queue]\ntarget = \"main\"\nstuck_after_seconds = 900\nstuck_repeat_seconds = \
             7200\n",
        );
        let s = for_repository(&repo).unwrap();
        assert_eq!(s.target.as_deref(), Some("main"));
        // And the reader that owns them reads the same file.
        let cfg = LeticodeConfig::load(&dir.join("leticode.toml")).unwrap();
        assert_eq!(cfg.queue.stuck_after_seconds, Some(900));
        assert_eq!(cfg.queue.stuck_repeat_seconds, Some(7200));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// `gate_host` is a machine's name or nothing - a value that is only whitespace is the
    /// default (this machine), not a host called `""`, and a name is passed on as written.
    #[test]
    fn gate_host_is_a_name_or_nothing() {
        let (dir, repo) = file("main_model = \"local\"\n[queue]\ntarget = \"main\"\n");
        assert_eq!(for_repository(&repo).unwrap().gate_host, None);
        std::fs::write(
            dir.join("leticode.toml"),
            "[queue]\ngate_host = \"lab2x1\"\n",
        )
        .unwrap();
        assert_eq!(
            for_repository(&repo).unwrap().gate_host.as_deref(),
            Some("lab2x1")
        );
        std::fs::write(dir.join("leticode.toml"), "[queue]\ngate_host = \"  \"\n").unwrap();
        assert_eq!(for_repository(&repo).unwrap().gate_host, None);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// `[queue] gate_cache_root`, `gate_cache_days` and `gate_min_free`: the build disk a gate
    /// writes its cache to, how long a cache may sit unwritten, and the floor below which a gate
    /// does not start. Absent is every gate's behaviour before this key existed, and each refusal
    /// names its own key rather than letting a typo become a default.
    #[test]
    fn the_gate_cache_keys_are_read_and_each_refusal_names_its_key() {
        let (dir, repo) = file("[queue]\ntarget = \"main\"\n");
        let s = for_repository(&repo).unwrap();
        assert_eq!(s.gate_cache_root, None, "the placement's own `target/`");
        assert_eq!(s.gate_cache_days, GATE_CACHE_DAYS);
        assert_eq!(s.gate_min_free, GATE_MIN_FREE);

        std::fs::write(
            dir.join("leticode.toml"),
            "[queue]\ngate_cache_root = \"/build/letibot-gate\"\ngate_cache_days = 7\n\
             gate_min_free = \"50G\"\n",
        )
        .unwrap();
        let s = for_repository(&repo).unwrap();
        assert_eq!(
            s.gate_cache_root.as_deref(),
            Some(std::path::Path::new("/build/letibot-gate"))
        );
        assert_eq!(s.gate_cache_days, 7);
        assert_eq!(s.gate_min_free, 50 << 30);

        // `~` is kept as written: it is THIS box's home here and the far machine's there.
        std::fs::write(
            dir.join("leticode.toml"),
            "[queue]\ngate_cache_root = \"~/build/gate\"\n",
        )
        .unwrap();
        assert_eq!(
            for_repository(&repo).unwrap().gate_cache_root.as_deref(),
            Some(std::path::Path::new("~/build/gate"))
        );

        for (text, want) in [
            (
                "[queue]\ngate_cache_root = \"build/gate\"\n",
                "gate_cache_root",
            ),
            ("[queue]\ngate_cache_days = 0\n", "gate_cache_days"),
            ("[queue]\ngate_cache_days = -1\n", "gate_cache_days"),
            (
                "[queue]\ngate_min_free = \"thirty gigs\"\n",
                "gate_min_free",
            ),
        ] {
            std::fs::write(dir.join("leticode.toml"), text).unwrap();
            let e = for_repository(&repo).unwrap_err();
            assert!(e.contains(want), "{text}: {e}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    /// `gate_poll_seconds` is the wait between two polls: five seconds unless named, a real number
    /// (not a whole one), and refused BY NAME where the step's watchdog would read the gap as a
    /// dead daemon rather than a slow one.
    #[test]
    fn gate_poll_is_seconds_and_refused_past_the_watchdog_window() {
        let (dir, repo) = file("main_model = \"local\"\n[queue]\ntarget = \"main\"\n");
        assert_eq!(
            for_repository(&repo).unwrap().gate_poll,
            crate::mergequeue::gate::GATE_POLL_DEFAULT,
            "the default is the tick every step's watchdog already counts"
        );
        std::fs::write(
            dir.join("leticode.toml"),
            "[queue]\ngate_poll_seconds = 1.5\n",
        )
        .unwrap();
        assert_eq!(
            for_repository(&repo).unwrap().gate_poll,
            std::time::Duration::from_millis(1500)
        );
        for bad in ["0", "-2", "0.01", "60"] {
            std::fs::write(
                dir.join("leticode.toml"),
                format!("[queue]\ngate_poll_seconds = {bad}\n"),
            )
            .unwrap();
            let e = for_repository(&repo).unwrap_err();
            assert!(e.contains("gate_poll_seconds"), "{bad}: {e}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    /// `gate_train` is one branch and one gate unless a project says more, and `0` is refused
    /// by name rather than read as *a train that never runs a gate*.
    #[test]
    fn gate_train_is_one_unless_named_and_zero_is_refused() {
        let (dir, repo) = file("main_model = \"local\"\n[queue]\ntarget = \"main\"\n");
        assert_eq!(
            for_repository(&repo).unwrap().gate_train,
            1,
            "the default is today's behaviour: one branch, one gate"
        );
        assert_eq!(QueueSettings::default().gate_train, 1);
        std::fs::write(dir.join("leticode.toml"), "[queue]\ngate_train = 3\n").unwrap();
        assert_eq!(for_repository(&repo).unwrap().gate_train, 3);
        for bad in ["0", "-2"] {
            std::fs::write(
                dir.join("leticode.toml"),
                format!("[queue]\ngate_train = {bad}\n"),
            )
            .unwrap();
            let e = for_repository(&repo).unwrap_err();
            assert!(e.contains("gate_train"), "{bad}: {e}");
            assert!(e.contains(bad), "{bad}: {e}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    /// `reviewers_at_once` and `reviewer_hosts` are read per project. Absent hosts mean this
    /// machine alone, `["*"]` is every declared host, and `lubuntu3` is refused by name.
    #[test]
    fn the_reviewers_limit_and_its_pool_are_read_or_refused_by_name() {
        let (dir, repo) = file("main_model = \"local\"\n[queue]\ntarget = \"main\"\n");
        let s = for_repository(&repo).unwrap();
        assert_eq!(s.reviewers_at_once, None, "the placement's own number");
        assert_eq!(
            s.reviewer_hosts,
            Some(vec!["local".to_string()]),
            "reviewers stay on this machine unless the project opts in"
        );
        assert_eq!(QueueSettings::default().reviewer_hosts, s.reviewer_hosts);

        std::fs::write(
            dir.join("leticode.toml"),
            "[queue]\nreviewers_at_once = 6\nreviewer_hosts = [\"lab2x1\", \" lubuntu2 \", \
             \"lab2x1\"]\n",
        )
        .unwrap();
        let s = for_repository(&repo).unwrap();
        assert_eq!(s.reviewers_at_once, Some(6));
        assert_eq!(
            s.reviewer_hosts.as_deref(),
            Some(["lab2x1".to_string(), "lubuntu2".to_string()].as_slice()),
            "trimmed, and a name twice is one machine"
        );

        std::fs::write(
            dir.join("leticode.toml"),
            "[queue]\nreviewer_hosts = [\"*\"]\n",
        )
        .unwrap();
        assert_eq!(
            for_repository(&repo).unwrap().reviewer_hosts,
            None,
            "`*` is every declared host"
        );

        for (text, want) in [
            ("[queue]\nreviewers_at_once = 0\n", "reviewers_at_once"),
            ("[queue]\nreviewers_at_once = -2\n", "reviewers_at_once"),
            (
                "[queue]\nreviewer_hosts = [\"lab2x1\", \"\"]\n",
                "reviewer_hosts",
            ),
            (
                "[queue]\nreviewer_hosts = [\"lab2x1\", \"lubuntu3\"]\n",
                "lubuntu3",
            ),
        ] {
            std::fs::write(dir.join("leticode.toml"), text).unwrap();
            let e = for_repository(&repo).unwrap_err();
            assert!(e.contains(want), "{text}: {e}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    /// `gate_carry` is read as paths inside the repository, and an entry outside it is refused
    /// by name - the whole `[queue]` with it, never silently dropped.
    #[test]
    fn gate_carry_is_paths_inside_the_repository_and_one_outside_is_refused_by_name() {
        let (dir, repo) = file("[queue]\ngate_carry = [\"images\", \"./vendor/\"]\n");
        let s = for_repository(&repo).unwrap();
        assert_eq!(
            s.gate_carry,
            vec!["images".to_string(), "vendor".to_string()]
        );
        for bad in ["/etc", "../sibling", "images/../../x", ".git", ".", ""] {
            std::fs::write(
                dir.join("leticode.toml"),
                format!("[queue]\ngate_carry = [\"images\", \"{bad}\"]\n"),
            )
            .unwrap();
            let e = for_repository(&repo).unwrap_err();
            assert!(e.contains(&format!("\"{bad}\"")), "{bad}: {e}");
            assert!(e.contains("gate_carry"), "{bad}: {e}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    /// `[queue.claude]` is what makes a project's reviewers headless `claude -p` runs, and the
    /// table absent is the letibot reviewer - so OFF is the default every project already had. The
    /// binary, the tools and the timeout are the project's; the rest is the module's own default.
    #[test]
    fn the_claude_table_is_off_until_a_project_names_a_binary() {
        let (dir, repo) = file("[queue]\ntarget = \"main\"\n");
        assert_eq!(
            for_repository(&repo).unwrap().claude,
            None,
            "a queue nobody configured runs letibot reviewers, exactly as before"
        );

        std::fs::write(
            dir.join("leticode.toml"),
            "[queue.claude]\nbinary = \"/usr/local/bin/claude\"\n",
        )
        .unwrap();
        let claude = for_repository(&repo).unwrap().claude.expect("the run");
        assert_eq!(claude.binary, PathBuf::from("/usr/local/bin/claude"));
        assert_eq!(
            claude.allowed_tools,
            super::super::review::claude::DEFAULT_TOOLS
                .iter()
                .map(|t| t.to_string())
                .collect::<Vec<_>>(),
            "a reader, a searcher and a lister: nothing that can write"
        );
        assert_eq!(
            claude.permission_mode,
            super::super::review::claude::DEFAULT_PERMISSION_MODE
        );
        assert_eq!(
            claude.timeout,
            super::super::review::claude::DEFAULT_TIMEOUT,
            "the queue's own hour, until a project says otherwise"
        );

        std::fs::write(
            dir.join("leticode.toml"),
            "[queue.claude]\nbinary = \"/opt/claude\"\nallowed_tools = [\"Read\", \"Bash(git diff:*)\", \
             \"Read\"]\npermission_mode = \"bypassPermissions\"\ntimeout_seconds = 90\n",
        )
        .unwrap();
        let claude = for_repository(&repo).unwrap().claude.expect("the run");
        assert_eq!(claude.binary, PathBuf::from("/opt/claude"));
        assert_eq!(
            claude.allowed_tools,
            vec!["Read".to_string(), "Bash(git diff:*)".to_string()],
            "trimmed, and a tool named twice is one tool"
        );
        assert_eq!(claude.permission_mode, "bypassPermissions");
        assert_eq!(claude.timeout, std::time::Duration::from_secs(90));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A `[queue.claude]` this build cannot obey is refused BY NAME - the whole table with it. A
    /// binary nobody can run, an empty tool and a timeout of nothing are a project that meant to
    /// have Claude reviewers and would otherwise be read as *off*.
    #[test]
    fn a_claude_table_that_cannot_be_obeyed_is_refused_by_name() {
        let (dir, repo) = file("[queue.claude]\nbinary = \"/usr/local/bin/claude\"\n");
        for (text, want) in [
            ("[queue.claude]\nallowed_tools = [\"Read\"]\n", "binary"),
            ("[queue.claude]\nbinary = \"\"\n", "binary"),
            ("[queue.claude]\nbinary = \"claude\"\n", "absolute"),
            (
                "[queue.claude]\nbinary = \"/opt/claude\"\nallowed_tools = [\"Read\", \"\"]\n",
                "allowed_tools",
            ),
            (
                "[queue.claude]\nbinary = \"/opt/claude\"\npermission_mode = \"  \"\n",
                "permission_mode",
            ),
            (
                "[queue.claude]\nbinary = \"/opt/claude\"\ntimeout_seconds = 0\n",
                "timeout_seconds",
            ),
            (
                "[queue.claude]\nbinary = \"/opt/claude\"\ntimeout_seconds = -5\n",
                "timeout_seconds",
            ),
            (
                "[queue.claude]\nbinary = \"/opt/claude\"\nbinaryy = \"/x\"\n",
                "binaryy",
            ),
        ] {
            std::fs::write(dir.join("leticode.toml"), text).unwrap();
            let e = for_repository(&repo).unwrap_err();
            assert!(e.contains(want), "{text}: {e}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    /// `[queue.claude]` is the QUEUE's table, and the model reader must still read the file: a
    /// session of that project losing its models because a reviewer was configured would be a
    /// second, silent cost of one key.
    #[test]
    fn the_model_reader_still_reads_a_file_with_a_claude_table() {
        let (dir, _repo) = file(
            "main_model = \"local\"\n[queue.claude]\nbinary = \"/usr/local/bin/claude\"\n\
             timeout_seconds = 600\n",
        );
        assert!(
            LeticodeConfig::load(&dir.join("leticode.toml")).is_ok(),
            "the `[queue]` table is the queue's own reader's, and this one ignores what it does \
             not know"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[cfg(test)]
mod reference_tests {
    use std::collections::BTreeSet;

    use letibot_config_ref::{files::LETICODE, probe};

    use super::*;

    fn fields<'de, T: serde::Deserialize<'de>>() -> BTreeSet<&'static str> {
        probe::serde_fields::<T>()
            .unwrap()
            .iter()
            .copied()
            .collect()
    }

    #[test]
    fn queue_tables_are_listed() {
        let mut queue = probe::keys_of(LETICODE.table("queue").unwrap());
        queue.insert("claude");
        assert_eq!(fields::<QueueTable>(), queue);
        assert_eq!(
            fields::<ClaudeTable>(),
            probe::keys_of(LETICODE.table("queue.claude").unwrap())
        );
    }
}
