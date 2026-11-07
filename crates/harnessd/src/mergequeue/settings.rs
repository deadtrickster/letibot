//! The queues' per-project settings - the `[queue]` table of the project's `leticode.toml`
//! found by walking up from the repository the way every `leticode.toml` is found.
//!
//! ```toml
//! [queue]
//! target = "feat/memory-engine"   # decision 12: the branch this project lands on
//! merge_via_session = false       # decision 4: a review that cannot run is handed to the
//!                                 #   project's main session to gate and merge (default: no -
//!                                 #   raise only)
//! red_target = "fix"              # decision 11: when the target goes red, the reviewer of the
//!                                 #   red tip opens a fix branch through the queue ("fix",
//!                                 #   the default) or the operator is only told ("raise")
//! gate_carry = ["images", "vendor"] # gitignored inputs the gate needs, COPIED from the
//!                                 #   operator's checkout into the landing tree before every
//!                                 #   gate run (default: none)
//! reviewers = "host"              # where this project's reviewers run: `host` (the default -
//!                                 #   on this machine, as the children that author the
//!                                 #   branches do) or `vm` (each reviewer in a firecode
//!                                 #   microVM)
//! gate_host = "lab2x1"             # decision 9's machine, when it is not this one: the gate's
//!                                 #   steps run on a host the operator declared in
//!                                 #   `hosts.toml` (default: empty - this machine)
//! gate_train = 3                  # how many approved branches one gate may judge together:
//!                                 #   they are rebased one onto the last, gated ONCE, and
//!                                 #   landed one commit at a time (default: 1 - one branch,
//!                                 #   one gate, exactly as before)
//! reviewers_at_once = 4           # how many reviewers this queue runs AT ONCE. Default:
//!                                 #   `vm` is the box's VM slots; `host` is what the pool
//!                                 #   of machines can take, from their cores
//! gate_in = "firecode"            # run the gate's steps inside a firecode VM over the gate's
//!                                 #   checkout (read-only, nothing copied back). A machine
//!                                 #   without a working firecode leaves the tip UNJUDGED.
//!                                 #   Default: "host" - the steps run on the gate machine
//! reviewer_hosts = ["lab2x1"]     # opt in: the declared hosts a `host` reviewer may be placed
//!                                 #   on (`["*"]` is all of them). Default: this machine only
//! ```
//!
//! The first three are the operator's own rulings, and each per PROJECT because
//! the projects differ: pg-noop lands on `feat/memory-engine`, 91 commits ahead of its `main`
//! and a queue that measured its branches against `main` saw every one 56–60 commits ahead.
//!
//! `gate_carry` exists because the landing tree is a git worktree, and a worktree has nothing
//! gitignored: firecode's gate booted no VM because its `images/` and `vendor/` were not there
//! (`mergequeue::carry`). Each entry is a path relative to the repository's root and must stay
//! inside it - absolute, `..`, `.git` and the root itself are refused by name here; a symlink
//! out, a missing path or a TRACKED path is refused when the gate runs. Each is copied (`rsync -a
//! --delete` when rsync is there, else a fresh recursive copy), never symlinked, so a gate cannot
//! write into the operator's checkout.

use std::path::{Path, PathBuf};

use crate::leticode_config::LeticodeConfig;

/// What a red target sets in motion (decision 11).
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
    /// (`["local"]`, D18). `None` is every declared host (`["*"]`). A list is exactly those names.
    pub reviewer_hosts: Option<Vec<String>>,
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
            gate_in: GateIn::Host,
            reviewer_skills: Vec::new(),
            reviewers_at_once: None,
            reviewer_hosts: Some(vec!["local".to_string()]),
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
    /// The census's two numbers. Read by `leticode-config`'s `[queue]` parser, from the user's
    /// file and the project's over it - `Config::leticode`, not this reader - and accepted here
    /// only so that a project writing them does not make this reader refuse the table. Two
    /// readers of one number would be two answers.
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
struct FileWithQueue {
    #[serde(default)]
    queue: Option<toml::Value>,
}

/// The settings for the repository at `repo` - the nearest `leticode.toml` at or above it.
/// No file, or a file with no `[queue]`, is the defaults. A `[queue]` that does not parse, or
/// names a key this build does not know, is an `Err` naming the file - never a silent default.
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
    let mut gate_carry = Vec::new();
    for entry in &q.gate_carry {
        let rel = super::carry::validate(entry).map_err(|e| format!("{}: {e}", path.display()))?;
        gate_carry.push(rel.to_string_lossy().into_owned());
    }
    // **A number of reviewers, or nothing.** Zero is refused rather than read as "no reviewers":
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
    // Absent is this machine (D18). `["*"]` becomes `None`, every declared host. An empty entry
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
    Ok(QueueSettings {
        gate_carry,
        gate_in,
        reviewers,
        gate_host: q
            .gate_host
            .map(|h| h.trim().to_string())
            .filter(|h| !h.is_empty()),
        reviewers_at_once,
        reviewer_hosts,
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

    /// K9: the census's two numbers are `leticode-config`'s - read from the user's file and the
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
}
