//! The per-project `leticode.toml`: which models answer in this project.
//!
//! > *"we have this thing like .env files. I want the same for my leticode
//! > projects - a config file where i set up things like main model, subagent
//! > models, gatekeeper model, maybe somrthing else later"*
//!
//! A `.env` for models. `.env` and `.git` are found by walking up from where you
//! stand, and so is this: a session started in a subdirectory of the project
//! still finds the project's settings, because the search climbs from the
//! session's workspace to the project root rather than looking only in the
//! directory the daemon happened to be launched in.
//!
//! # The file, and why it is visible
//!
//! `leticode.toml`, at the project root — a file, not a dotfile. A file that
//! changes which model answers you should not be a surprise: the operator opens
//! the project and sees it, the way they see a `.git` or a `Cargo.toml`. A
//! hidden file that picks the model is the shape of the defect this tree has
//! already paid for — a setting that is there and does not look like it is.
//!
//! # The keys, v1
//!
//! ```toml
//! main_model = "deepseek/deepseek-chat"   # the model the session's turns go to
//! subagent_model = "local"                # the default for `task` spawns
//! gatekeeper_model = "qwen3-4b"           # the guard's own model
//! judge_model = "glm-5.3-flash"           # the adjudicator
//!
//! [roles]                                 # a named-role override: a future role
//! reviewer = "deepseek/deepseek-reasoner" # needs no new key, only a line here
//! ```
//!
//! **An absent key means the daemon's own default, stated as such** — not a
//! guess, and not silently a built-in. The default for each key is the one the
//! daemon already has for that model, and the disclosure says which models the
//! file set so "where did this model come from" is answerable from the screen.
//!
//! # The precedence, stated once
//!
//! Command line beats the project file, the project file beats
//! `~/.config/letibot/`, and an unset key falls through. That is the whole of
//! it, and it is [`precedence`] below rather than a rule remembered in four
//! places: precedence that is not stated is precedence nobody can reason about,
//! and the operator lost hours to a config that silently did not reach a
//! session, so the rule that decides which model answers is written down where
//! it is enforced.
//!
//! # What the file may not do
//!
//! **Models are configuration; permissions are not.** A project file may set
//! which model answers a role, and it may not seat a tool on a role or lift an
//! access narrowing. The shape enforces it: the file deserialises into
//! [`LeticodeConfigFile`], which is four model keys and a `[roles]` table of
//! model names, with `deny_unknown_fields` on the top level. A line the version
//! does not know — a typo'd `subagent_modle`, or an `allow_bash = true` that
//! would widen a capability — is refused by name, not ignored, because a
//! silently ignored key is the bug class being fixed and a silently widened
//! capability is the worse of the two.
//!
//! # The loud failures
//!
//! The operator lost hours to a config that silently did not reach a session —
//! a `[default] provider` that no path consulted, and an `Unreadable` that said
//! nothing. So this reader says the fault rather than swallowing it, in the
//! three places a silent config is found:
//!
//! * a file that will not parse says so **and says where** — the file, the
//!   line, and what the parser expected, because that is the thing the operator
//!   has to fix;
//! * a key the version does not know is **named**, not ignored — a typo'd
//!   `subagent_modle` is the bug class being fixed, and a reader that accepts it
//!   and reads nothing reproduces it one line lower;
//! * on success, the session's **startup disclosure** says which models the
//!   project file set, so "where did this model come from" is answerable from
//!   the screen.
//!
//! A file that does not parse does not take the session down: the daemon runs
//! on its own defaults and says so, the way `prompts.toml` already does.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The file's name, at the project root. Visible, not a dotfile: a file that
/// changes which model answers you should not be a surprise.
pub const FILE_NAME: &str = "leticode.toml";

/// **The per-project model configuration, as parsed from `leticode.toml`.**
///
/// Every model field is `None` when the file did not set it, and `None` means
/// **the daemon's own default** — not a guess, and not silently a built-in. The
/// precedence that turns these into the session's models is stated once, in
/// [`precedence`]: command line beats the project file, the project file beats
/// `~/.config/letibot/`, and an unset key falls through.
///
/// **This struct is the whole of what the file may set.** It carries model
/// names and nothing else — no seat, no tool list, no access narrowing — so a
/// project file cannot widen a capability. The `[roles]` table maps a role name
/// to the model that role runs on; it is a model override, not a seating, and a
/// role name the daemon does not know is still a model the operator asked for,
/// so it is carried rather than refused (the refusal is for a top-level key the
/// version does not know, which is a typo, not a future role).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LeticodeConfig {
    /// The model the session's turns go to. `None` is the daemon's own default
    /// (`--model`/`--provider`, else `[default]` in providers.toml, else the
    /// local server).
    pub main_model: Option<String>,
    /// The default model for `task` spawns. `None` is the daemon's own default
    /// (the child inherits the parent's model).
    pub subagent_model: Option<String>,
    /// The gatekeeper's model. `None` is the daemon's own default (`[gatekeeper]
    /// model` in providers.toml, else the session's own model).
    pub gatekeeper_model: Option<String>,
    /// The adjudicator's model. `None` is the daemon's own default.
    pub judge_model: Option<String>,
    /// The `[roles]` section: a named-role override, so a future role needs no
    /// new key. A role name maps to the model that role runs on.
    pub roles: BTreeMap<String, String>,
    /// The path the config was read from, for the disclosure. `None` when no
    /// file was found, which is the absent-file case: the daemon's own defaults,
    /// and nothing to name.
    pub path: Option<PathBuf>,
}

impl LeticodeConfig {
    /// **The walk-up: find the nearest `leticode.toml` at or above `workspace`.**
    ///
    /// The way `.env` and `.git` are found: a session started in a subdirectory
    /// of the project still finds the project's settings, because the search
    /// climbs from the session's workspace rather than looking only in the
    /// directory the daemon happened to be launched in. The nearest file wins —
    /// the first one met climbing up — and the walk stops at the filesystem
    /// root, so a box with no project file anywhere is a silent no-file rather
    /// than a search that never ends.
    ///
    /// `None` is the honest answer for a workspace with no `leticode.toml` at or
    /// above it: the daemon runs on its own defaults, and the disclosure says so
    /// rather than naming a file that is not there.
    pub fn discover(workspace: &Path) -> Option<PathBuf> {
        let mut dir = workspace.to_path_buf();
        loop {
            let candidate = dir.join(FILE_NAME);
            if candidate.is_file() {
                return Some(candidate);
            }
            // `pop` returns `false` at the filesystem root, which is where the
            // walk stops: a box with no project file anywhere is a no-file, not a
            // loop.
            if !dir.pop() {
                return None;
            }
        }
    }

    /// Load `leticode.toml`. A missing file is `Ok` with no models — the
    /// daemon's own defaults, not an error. A file that does not parse, or that
    /// names a key this version does not know, is `Err` with the parser's own
    /// message (or the key's name) and the path, so the operator can see which
    /// file said what.
    ///
    /// **The loud failures, in the order they are found:**
    ///
    /// * a file that will not parse says so and says where — the file, the line,
    ///   and what the parser expected;
    /// * a key the version does not know is named, not ignored — `deny_unknown_fields`
    ///   on [`LeticodeConfigFile`] is the report, and a typo'd `subagent_modle` is
    ///   the bug class being fixed;
    /// * a file that sets a capability rather than a model is refused by name,
    ///   because the file may set which model answers a role and may not seat a
    ///   tool on one or lift an access narrowing.
    pub fn load(path: &Path) -> Result<LeticodeConfig, String> {
        if !path.is_file() {
            return Ok(LeticodeConfig::default());
        }
        let text =
            std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let file: LeticodeConfigFile =
            toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(LeticodeConfig {
            main_model: file.main_model.filter(|m| !m.trim().is_empty()),
            subagent_model: file.subagent_model.filter(|m| !m.trim().is_empty()),
            gatekeeper_model: file.gatekeeper_model.filter(|m| !m.trim().is_empty()),
            judge_model: file.judge_model.filter(|m| !m.trim().is_empty()),
            roles: file
                .roles
                .into_iter()
                .filter(|(_, m)| !m.trim().is_empty())
                .collect(),
            path: Some(path.to_path_buf()),
        })
    }

    /// The model the file set for a role, or `None` when the file did not name
    /// one. A role the file does not know is `None`, which is the daemon's own
    /// default for that role — stated, not guessed.
    pub fn role_model(&self, role: &str) -> Option<&str> {
        self.roles.get(role).map(String::as_str)
    }

    /// Which models the file set, as the disclosure says them. Empty when the
    /// file set nothing, which is the absent-file case and the byte-identical
    /// case: a daemon with no project file discloses nothing from it.
    pub fn set_models(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(m) = &self.main_model {
            out.push(format!("main {m}"));
        }
        if let Some(m) = &self.subagent_model {
            out.push(format!("subagent {m}"));
        }
        if let Some(m) = &self.gatekeeper_model {
            out.push(format!("gatekeeper {m}"));
        }
        if let Some(m) = &self.judge_model {
            out.push(format!("judge {m}"));
        }
        for (role, m) in &self.roles {
            out.push(format!("{role} {m}"));
        }
        out
    }
}

/// The file's shape, as parsed. `deny_unknown_fields` on the top level is the
/// report for a key this version does not know: a `subagent_modle` is a parse
/// error carrying the parser's own message, not a silently ignored line. The
/// `[roles]` table is a `BTreeMap`, so a role name the daemon does not know is
/// carried rather than refused — a future role needs no new key, only a line.
///
/// **The shape is the capability boundary.** The struct is four model keys and
/// a table of model names, and nothing else. There is no field for a seat, a
/// tool list or an access narrowing, so a project file cannot widen a
/// capability: a line that would — an `allow_bash = true`, a `role = "runner"` —
/// is a top-level key the version does not know, and `deny_unknown_fields`
/// refuses it by name.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LeticodeConfigFile {
    #[serde(default)]
    main_model: Option<String>,
    #[serde(default)]
    subagent_model: Option<String>,
    #[serde(default)]
    gatekeeper_model: Option<String>,
    #[serde(default)]
    judge_model: Option<String>,
    #[serde(default)]
    roles: BTreeMap<String, String>,
}

/// **The precedence, stated once.**
///
/// Command line beats the project file, the project file beats
/// `~/.config/letibot/`, and an unset key falls through to the daemon's own
/// default. That is the whole of it, and it is a function rather than a rule
/// remembered in four places, because precedence that is not stated is
/// precedence nobody can reason about — and the operator lost hours to a config
/// that silently did not reach a session, so the rule that decides which model
/// answers is written down where it is enforced.
///
/// `None` from this is the fall-through: the caller applies the daemon's own
/// default for the model, which is the stated default rather than a guess.
pub fn precedence(
    cli: Option<&str>,
    project: Option<&str>,
    user: Option<&str>,
) -> Option<String> {
    cli.map(str::to_string)
        .or_else(|| project.map(str::to_string))
        .or_else(|| user.map(str::to_string))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temp dir for a `leticode.toml` fixture, unique per test so parallel
    /// tests do not share a file. The caller removes it.
    fn project_dir(test: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("letibot-leticode-{}-{test}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// **The walk-up finds the nearest file and stops at the root.** A session
    /// started in a subdirectory of the project finds the project's settings,
    /// because the search climbs from the workspace rather than looking only in
    /// the directory the daemon was launched in. The nearest file wins — the
    /// first one met climbing up — and a file in a parent does not shadow one in
    /// the workspace.
    #[test]
    fn the_walk_up_finds_the_nearest_file_and_stops_at_the_root() {
        let root = project_dir("walk_up");
        // A file at the project root.
        let root_file = root.join(FILE_NAME);
        std::fs::write(&root_file, "main_model = \"root-model\"\n").unwrap();
        // A session started two levels down, with no file of its own.
        let sub = root.join("crates").join("harnessd");
        std::fs::create_dir_all(&sub).unwrap();
        let found = LeticodeConfig::discover(&sub).expect("the walk-up finds the root file");
        assert_eq!(found, root_file, "the nearest file at or above the workspace");
        let cfg = LeticodeConfig::load(&found).unwrap();
        assert_eq!(cfg.main_model.as_deref(), Some("root-model"));

        // A file in the workspace beats one in a parent: the nearest wins.
        let sub_file = sub.join(FILE_NAME);
        std::fs::write(&sub_file, "main_model = \"sub-model\"\n").unwrap();
        let found = LeticodeConfig::discover(&sub).expect("the walk-up finds the sub file");
        assert_eq!(found, sub_file, "the workspace's own file is nearer than the root's");
        let cfg = LeticodeConfig::load(&found).unwrap();
        assert_eq!(cfg.main_model.as_deref(), Some("sub-model"));

        // A workspace with no file at or above it is a no-file, and the walk
        // stops at the filesystem root rather than looping.
        let empty = project_dir("walk_up_empty");
        std::fs::remove_file(&root_file).unwrap();
        std::fs::remove_file(&sub_file).unwrap();
        assert!(
            LeticodeConfig::discover(&empty).is_none(),
            "no file at or above the workspace is a no-file"
        );
    }

    /// **The precedence, stated once and checked here:** command line beats the
    /// project file, the project file beats the user config, and an unset key
    /// falls through. A regression here would be a model the operator set in one
    /// place that is answered by another, which is the defect this feature
    /// exists to forbid.
    #[test]
    fn the_precedence_is_cli_then_project_then_user_then_fall_through() {
        // CLI beats project.
        assert_eq!(
            precedence(Some("cli"), Some("project"), Some("user")).as_deref(),
            Some("cli")
        );
        // Project beats user.
        assert_eq!(
            precedence(None, Some("project"), Some("user")).as_deref(),
            Some("project")
        );
        // User is the fall-through when CLI and project are unset.
        assert_eq!(precedence(None, None, Some("user")).as_deref(), Some("user"));
        // All unset is the fall-through to the daemon's own default.
        assert_eq!(precedence(None, None, None), None);
    }

    /// **A key the version does not know is named, not ignored.** A typo'd
    /// `subagent_modle` is the bug class being fixed: a reader that accepts it
    /// and reads nothing reproduces the defect one line lower, so the reader
    /// refuses it by name, with the file and the parser's own message.
    #[test]
    fn an_unknown_key_is_named_not_ignored() {
        let dir = project_dir("unknown_key");
        let path = dir.join(FILE_NAME);
        std::fs::write(&path, "subagent_modle = \"typo\"\n").unwrap();
        let err = LeticodeConfig::load(&path).expect_err("an unknown key is Err");
        assert!(err.contains("subagent_modle"), "the typo is named: {err}");
        assert!(
            err.contains(FILE_NAME) || err.contains(&path.display().to_string()),
            "the file is named: {err}"
        );
    }

    /// **A file that will not parse says so and says where** — the file, the
    /// line, and what the parser expected — and it does not take the session
    /// down: the daemon runs on its own defaults and says so, the way
    /// `prompts.toml` already does.
    #[test]
    fn an_unparseable_file_is_loud_and_does_not_take_the_session_down() {
        let dir = project_dir("unparseable");
        let path = dir.join(FILE_NAME);
        std::fs::write(&path, "main_model = \n").unwrap();
        let err = LeticodeConfig::load(&path).expect_err("an unparseable file is Err");
        assert!(err.contains(&path.display().to_string()), "the file is named: {err}");
        // The parser's own message carries the line and what was expected.
        assert!(err.contains("line 1"), "the line is named: {err}");
        // And the absent-file case is the daemon's own defaults, not an error:
        // the session runs, it just runs on what the daemon has.
        let missing = dir.join("no-such-file.toml");
        let cfg = LeticodeConfig::load(&missing).expect("a missing file is Ok");
        assert_eq!(cfg, LeticodeConfig::default(), "absent is the defaults");
    }

    /// **An absent file is simply the defaults.** A box with no `leticode.toml`
    /// runs exactly as it did: the daemon's own models, and nothing to name. A
    /// regression here would put a complaint on every start of every daemon on a
    /// box that has no project file, which is most of them.
    #[test]
    fn an_absent_file_is_silently_the_defaults() {
        let dir = project_dir("absent");
        let missing = dir.join(FILE_NAME);
        assert!(!missing.exists());
        let cfg = LeticodeConfig::load(&missing).expect("a missing file is Ok");
        assert_eq!(cfg, LeticodeConfig::default());
        assert!(cfg.set_models().is_empty(), "nothing to disclose");
    }

    /// **The file may set a model and may not widen a capability.** A line that
    /// would seat a tool on a role or lift an access narrowing — an
    /// `allow_bash = true`, a `role = "runner"` — is a top-level key the version
    /// does not know, and `deny_unknown_fields` refuses it by name. Models are
    /// configuration; permissions are not, and the shape of the struct is the
    /// boundary that says so.
    #[test]
    fn a_project_file_cannot_widen_a_capability() {
        let dir = project_dir("no_widen");
        let path = dir.join(FILE_NAME);
        // A capability key, not a model key.
        std::fs::write(&path, "allow_bash = true\n").unwrap();
        let err = LeticodeConfig::load(&path).expect_err("a capability key is refused");
        assert!(err.contains("allow_bash"), "the capability key is named: {err}");
        // A seat key, the other direction of the same widening.
        std::fs::write(&path, "role = \"runner\"\n").unwrap();
        let err = LeticodeConfig::load(&path).expect_err("a seat key is refused");
        assert!(err.contains("role"), "the seat key is named: {err}");
    }

    /// **The well-formed file gives the models it set**, and the disclosure
    /// names them, so "where did this model come from" is answerable from the
    /// screen. A file that sets nothing discloses nothing, which is the
    /// absent-file case and the byte-identical case.
    #[test]
    fn a_well_formed_file_gives_its_models_and_the_disclosure_names_them() {
        let dir = project_dir("well_formed");
        let path = dir.join(FILE_NAME);
        std::fs::write(
            &path,
            "main_model = \"deepseek/deepseek-chat\"\n\
             subagent_model = \"local\"\n\
             gatekeeper_model = \"qwen3-4b\"\n\
             judge_model = \"glm-5.3-flash\"\n\n\
             [roles]\n\
             reviewer = \"deepseek/deepseek-reasoner\"\n",
        )
        .unwrap();
        let cfg = LeticodeConfig::load(&path).expect("a well-formed file is Ok");
        assert_eq!(cfg.main_model.as_deref(), Some("deepseek/deepseek-chat"));
        assert_eq!(cfg.subagent_model.as_deref(), Some("local"));
        assert_eq!(cfg.gatekeeper_model.as_deref(), Some("qwen3-4b"));
        assert_eq!(cfg.judge_model.as_deref(), Some("glm-5.3-flash"));
        assert_eq!(cfg.role_model("reviewer"), Some("deepseek/deepseek-reasoner"));
        assert_eq!(cfg.role_model("coder"), None, "a role the file did not name");
        let set = cfg.set_models();
        assert!(set.iter().any(|s| s.contains("main deepseek/deepseek-chat")), "{set:?}");
        assert!(set.iter().any(|s| s.contains("subagent local")), "{set:?}");
        assert!(set.iter().any(|s| s.contains("gatekeeper qwen3-4b")), "{set:?}");
        assert!(set.iter().any(|s| s.contains("judge glm-5.3-flash")), "{set:?}");
        assert!(set.iter().any(|s| s.contains("reviewer deepseek/deepseek-reasoner")), "{set:?}");
    }

    /// **A role the file does not know is carried, not refused.** The `[roles]`
    /// table is a named-role override, so a future role needs no new key, only a
    /// line. A role name the daemon does not know is still a model the operator
    /// asked for, so it is carried — the refusal is for a top-level key the
    /// version does not know, which is a typo, not a future role.
    #[test]
    fn a_role_the_file_does_not_know_is_carried_not_refused() {
        let dir = project_dir("future_role");
        let path = dir.join(FILE_NAME);
        std::fs::write(&path, "[roles]\nsome_future_role = \"a-model\"\n").unwrap();
        let cfg = LeticodeConfig::load(&path).expect("a future role is Ok");
        assert_eq!(cfg.role_model("some_future_role"), Some("a-model"));
    }
}
