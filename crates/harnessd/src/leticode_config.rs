//! The `leticode.toml` levels: which models answer in this project, and how its
//! subagents sample — and the same file once per USER, so "on this box, always
//! my local glm" is one line in one place.
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
//! `leticode.toml`, at the project root or in `~/.config/letibot/` — a file, not
//! a dotfile. A file that
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
//! # The parameters, and the wall they may not move
//!
//! > *"when used in subagents mode we need to call qwen with the parameters
//! > similar to dense78 - otherwise it overthinks terribly"*
//! >
//! > *"we dont have the same context limit as with dense obviously"*
//!
//! Two facts in the operator's own words, and they are two DIFFERENT facts: what
//! a subagent samples with is dense78's, and how much room it has is not.
//! `dense78` is a `[model."dense78"]` block in `providers.toml` — `qwen-3.8-27b`
//! at `192.168.1.78:8082` — and the same weights are served at two `n_ctx` on
//! this fleet, so a window copied from one box onto a child running on another
//! is the defect `child_window_refusal` in `harness.rs` was written for. So a
//! role sets **sampler parameters** and nothing that describes the weights or
//! the room: a `[roles.coder]` table carrying `temperature = 0.7` is read, and
//! the same table carrying `window = 57344` or `max_tokens = 2048` is refused
//! **by name** — [`WINDOW_AND_CAP_KEYS`], and the refusal says why.
//!
//! The values the operator measured on dense78 are the standing defaults for a
//! subagent ([`RoleSampling::dense78`]), so a project that writes nothing gets
//! subagents that do not overthink, and a project that wants its own numbers
//! writes them per role. They are the standing answer for every spawn, whatever
//! model answers it — the metered transport already drops what its API cannot
//! carry (`provider_sampling` in `harness.rs`), and a project that wants a cloud
//! child sampled the way the daemon's own literal does says so:
//! `temperature = 0`, `top_k = 1`, which is what that literal is.
//!
//! ```toml
//! subagent_model = "local"
//!
//! [roles.subagent]        # every spawn, whatever seat it asked for
//! temperature = 0.7
//! top_p = 0.8
//! top_k = 20
//! min_p = 0
//! presence_penalty = 1.5
//!
//! [roles.coder]           # one seat's spawns, over the line above
//! temperature = 1.0
//! ```
//!
//! # Two levels, one file, one parser
//!
//! The same name and the same keys at two levels, discovered differently:
//!
//! * **the project's** `leticode.toml`, found by walking up from the session's
//!   workspace — a `.env` for one project, and the file above;
//! * **the user's** `~/.config/letibot/leticode.toml`, at one fixed path beside
//!   `providers.toml`, `prompts.toml`, `modes.tsv` and `head.toml`.
//!
//! > *"nah i thought you will do the user module thing for me."*
//!
//! One parser serves both — [`LeticodeConfig::load`], and [`load_user`] is the
//! same call at the fixed path — because two readers of one file shape is how
//! the two files start disagreeing about what a key means. A user file is a
//! *project* file that happens to sit in your home: same keys, same refusals,
//! same wall around what it may not set.
//!
//! # Why the user level is not `[default]` in `providers.toml`
//!
//! `~/.config/letibot/providers.toml` already carries a `[default]` block, and
//! it LOOKS like the answer to "on this box, always this model". It is not, and
//! the operator's own evening is why:
//!
//! ```toml
//! # ~/.config/letibot/providers.toml — written by `/models deepseek`
//! [default]
//! provider = "deepseek"
//! model = "deepseek-flash"
//! ```
//!
//! `[default]` is read into `cfg.provider` only when `cfg.provider.is_none()`
//! (`cli.rs`), and `--model` — which the launcher passes on EVERY start, because
//! `--model` is the alias whose vocabulary the daemon submits token ids against
//! — leaves `cfg.provider` at `None`. So a file-level `[default]` outranked a
//! command-line model, and `letibot --glm` answered on **deepseek**: the flag won
//! in one direction (`--provider deepseek` beats `[default]`) and lost in the
//! other (`--model glm-5.3-flash` does not). That asymmetry is what this level
//! fixes, and it is fixed rather than hidden: `[default]` is now the level BELOW
//! this file, so a line here outranks it.
//!
//! The two files are also shaped differently, and that is the second reason. A
//! `[default]` block can only name a **provider** — `deepseek`, `glm`, `grok`,
//! or `local`, which is the same as saying nothing — because `DefaultChoice`
//! holds a preset name and a model id on that preset. It cannot say
//! `glm-5.3-flash`, the local alias `letibot --glm` runs, and
//! `~/.config/letibot/providers.toml`'s own comment says why that matters:
//! *"this PRESET glm-4.6 at Zhipu, metered, via `/models glm`"* is a different
//! thing from *"the LOCAL glm-5.3-flash"*. `main_model` speaks both languages —
//! `provider/model` for a meter, a bare alias for the local server — so it is
//! the key that can say the thing the operator wanted to say.
//!
//! # The precedence, stated once
//!
//! **Command line beats the project file, the project file beats the user file,
//! the user file beats the built-in default, and an unset key falls through.**
//! That is the whole of it, and it is [`precedence`] below rather than a rule
//! remembered in four places: precedence that is not stated is precedence nobody
//! can reason about, and the operator lost hours to a config that silently did
//! not reach a session, so the rule that decides which model answers is written
//! down where it is enforced.
//!
//! The four levels, named as the disclosure names them:
//!
//! | level      | who speaks                                                     |
//! |------------|----------------------------------------------------------------|
//! | `flag`     | `--provider` on the command line. `--model` alone is the local  |
//! |            | BINDING (the alias the server is serving and the vocab the       |
//! |            | ledger is written in), not a choice of who answers — see below. |
//! | `project`  | the nearest `leticode.toml` at or above `--workspace`            |
//! | `user`     | `~/.config/letibot/leticode.toml`                                |
//! | `built-in` | `[default]` in `providers.toml` (what `/models NAME` writes),    |
//! |            | else the local server this daemon was launched against           |
//!
//! **`--model` alone is not the flag level, and that is a deliberate reading of
//! this tree's own vocabulary rather than a convenience.** `scripts/letibot`
//! labels the two facts apart in its own banner — `model` is the LOCAL alias,
//! the vocab the daemon binds, and `answers` is what actually answers the turns
//! — and it passes `--model` on every start, including a start where the
//! operator typed nothing about a model at all. So treating `--model` as "the
//! operator chose who answers" would make the project file and this file
//! unreachable for every launcher-started daemon, which is the defect this level
//! exists to fix rather than to reproduce. `--provider` is the flag that says who
//! answers, and it still beats everything below it.
//!
//! One consequence is written down because it is a real bound: a file's bare
//! alias does NOT overwrite the alias `--model` bound, because the daemon
//! tokenises with its own GGUF and a mismatch is `400 Prompt contains invalid
//! tokens` at the first turn. The file's alias still clears the provider — that
//! is the half that decides who answers — and a disagreement is named at startup
//! rather than resolved in silence (see `apply_main_model` in `cli.rs`).
//!
//! # What the file may not do
//!
//! **Models are configuration; permissions are not.** A file at either level may set
//! which model answers a role, and it may not seat a tool on a role or lift an
//! access narrowing. The shape enforces it: the file deserialises into
//! [`LeticodeConfigFile`], which is four model keys and a `[roles]` table of
//! model names and samplers, with `deny_unknown_fields` on the top level. A line
//! the version does not know — a typo'd `subagent_modle`, or an `allow_bash =
//! true` that would widen a capability — is refused by name, not ignored,
//! because a silently ignored key is the bug class being fixed and a silently
//! widened capability is the worse of the two.
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
//!   `subagent_modle` at the top level or a `temperatuer` inside a role's table
//!   is the bug class being fixed, and a reader that accepts it and reads
//!   nothing reproduces it one line lower;
//! * on success, the session's **startup disclosure** says which models the
//!   project file set — and the parameters it set — so "where did this model
//!   come from" is answerable from the screen.
//!
//! A file that does not parse does not take the session down: the daemon runs
//! on its own defaults and says so, the way `prompts.toml` already does. That is
//! true at both levels, and a user file that will not parse is reported with the
//! file and the line and the session still starts — the same sentence the project
//! file gets, because a user file that took every daemon on the box down would be
//! a worse failure than the one it reports.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The file's name, at the project root and in `~/.config/letibot/`. Visible,
/// not a dotfile: a file that changes which model answers you should not be a
/// surprise.
pub const FILE_NAME: &str = "leticode.toml";

/// **The user level's path**: `~/.config/letibot/leticode.toml`, the same config
/// dir `providers.toml` and `prompts.toml` live in.
///
/// Derived from [`letibot_provider::keys::config_file`] rather than from `$HOME`
/// spelled a second time, so the one file that knows where the config dir is is
/// the one that answers — a `$XDG_CONFIG_HOME` honoured by `providers.toml` and
/// ignored here would be two answers to one question.
///
/// **A fixed path, with no walk-up.** That is the whole difference from
/// [`LeticodeConfig::discover`]: a project file is *found*, because a session
/// started in a subdirectory has to reach the project's settings, and a user file
/// is *at an address*, because "mine" does not depend on where you stand.
pub fn user_path() -> PathBuf {
    letibot_provider::keys::config_file()
        .parent()
        .map(|p| p.join(FILE_NAME))
        .unwrap_or_else(|| PathBuf::from(FILE_NAME))
}

/// **Load the user level**, at [`user_path`]. The same call as
/// [`LeticodeConfig::load`], because it is the same file shape — one parser, two
/// levels.
///
/// A missing file is `Ok` with no models, exactly as at the project level: a box
/// where nobody wrote one runs on the daemon's own standing answer and says so,
/// rather than on a file that is not there.
pub fn load_user() -> Result<LeticodeConfig, String> {
    LeticodeConfig::load(&user_path())
}

/// **Which level named the model that answers** — the word the disclosure puts on
/// the screen, because *"why is my session on deepseek"* was a whole evening and
/// the answer has to be readable rather than findable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Level {
    /// `--provider` on the command line.
    Flag,
    /// The nearest `leticode.toml` at or above the workspace.
    Project,
    /// `~/.config/letibot/leticode.toml`.
    User,
    /// `[default]` in `providers.toml`, else the local server this daemon was
    /// launched against. The default variant, because it is the state a daemon
    /// with no file and no flag is in.
    #[default]
    Builtin,
}

impl Level {
    /// The word the disclosure uses. Lowercase and stable: it is a value a reader
    /// matches on, and a capitalised one is a second spelling of the same level.
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Flag => "flag",
            Level::Project => "project",
            Level::User => "user",
            Level::Builtin => "built-in",
        }
    }

    /// The level as a sentence names it. **Each one contains its own word from
    /// [`Level::as_str`]** — `flag`, `project`, `user`, `built-in` — because that word
    /// is what a reader scans for and what the operator asked to be told; a prose name
    /// that shared no substring with it would be a second vocabulary for one fact.
    pub fn describe(self) -> &'static str {
        match self {
            Level::Flag => "the flag level (`--provider` on the command line)",
            Level::Project => "the project level",
            Level::User => "the user level",
            Level::Builtin => "the built-in level",
        }
    }
}

impl std::fmt::Display for Level {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// **The role every spawn lands on**, when the `task` call named no other seat.
///
/// A `[roles.subagent]` table is the floor under every child, and a seat's own
/// table (`[roles.coder]`) refines it — because the operator's ask is about
/// *"subagents mode"* rather than about one seat, and a project that wants every
/// child to stop overthinking should have to say it once.
pub const SUBAGENT_ROLE: &str = "subagent";

/// The sampler keys a role's table may set, in the order the operator's own
/// dense78 profile writes them. The same five names `providers.toml`'s
/// `[model.*]` blocks use, so one vocabulary covers both files.
pub const SAMPLER_KEYS: &[&str] = &["temperature", "top_p", "top_k", "min_p", "presence_penalty"];

/// **The keys a role's table may not set, and the reason is the operator's own
/// second sentence.**
///
/// *"we dont have the same context limit as with dense obviously"* — a window is
/// a fact about the box that serves the weights, and it is MEASURED where the
/// subagent runs (see `child_window_refusal` in `harness.rs`, written after
/// three children planned against the parent's wall and reached 911k resident
/// tokens with no compaction item in their logs). A reply cap is the same kind
/// of fact: it belongs to the call, not to the project's idea of the model.
/// Naming them here rather than leaving them to the generic unknown-key refusal
/// is the difference between *"I do not know that key"* and *"I know exactly
/// what you meant, and this file does not get to decide it"*.
pub const WINDOW_AND_CAP_KEYS: &[&str] = &[
    "window",
    "context_window",
    "n_ctx",
    "max_tokens",
    "n_predict",
    "num_predict",
    "reply_cap",
    "thinking_budget_tokens",
];

/// **How one role samples** — the five keys measured on dense78, each one
/// `Option` so "the file said nothing" stays a different fact from "the file
/// said `temperature = 0.0`".
///
/// There is no window here and no reply cap, and that is the type doing the
/// work rather than a rule somebody has to remember: [`WINDOW_AND_CAP_KEYS`]
/// are refused at the parser, so nothing downstream can be handed a dense78
/// window along with dense78's sampling.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RoleSampling {
    /// How flat the distribution is. dense78: `0.7`.
    pub temperature: Option<f64>,
    /// Nucleus. dense78: `0.8`.
    pub top_p: Option<f64>,
    /// The local server's cut. dense78: `20`.
    pub top_k: Option<i64>,
    /// The floor. dense78: `0`, which matters: *set* to zero is an instruction,
    /// and absent is the server's own decision.
    pub min_p: Option<f64>,
    /// The anti-repetition penalty, and the one the operator names as the cure
    /// for a model that overthinks. dense78: `1.5`.
    pub presence_penalty: Option<f64>,
}

impl RoleSampling {
    /// **The operator's own numbers, measured on `dense78`.**
    ///
    /// > *"when used in subagents mode we need to call qwen with the parameters
    /// > similar to dense78 - otherwise it overthinks terribly"*
    ///
    /// These are the standing defaults for a subagent, so a project that writes
    /// no `leticode.toml` at all still gets children that do not overthink, and
    /// a project that wants other numbers overrides them key by key.
    pub fn dense78() -> RoleSampling {
        RoleSampling {
            temperature: Some(0.7),
            top_p: Some(0.8),
            top_k: Some(20),
            min_p: Some(0.0),
            presence_penalty: Some(1.5),
        }
    }

    /// `other` over `self`, key by key: a key `other` did not set keeps the
    /// value it had. Overriding rather than replacing is what lets a project
    /// change `temperature` alone and keep the rest of the operator's cure.
    pub fn overridden_by(&self, other: &RoleSampling) -> RoleSampling {
        RoleSampling {
            temperature: other.temperature.or(self.temperature),
            top_p: other.top_p.or(self.top_p),
            top_k: other.top_k.or(self.top_k),
            min_p: other.min_p.or(self.min_p),
            presence_penalty: other.presence_penalty.or(self.presence_penalty),
        }
    }

    /// Whether the file said nothing at all. `false` for `temperature = 0.0`,
    /// which is an instruction — see [`RoleSampling`].
    pub fn is_empty(&self) -> bool {
        *self == RoleSampling::default()
    }

    /// The parameters as the `sampling` a call is made with, in the operator's
    /// own order. Only the keys that are set are present.
    pub fn to_sampling(&self) -> serde_json::Value {
        let mut out = serde_json::Map::new();
        if let Some(v) = self.temperature {
            out.insert("temperature".into(), v.into());
        }
        if let Some(v) = self.top_p {
            out.insert("top_p".into(), v.into());
        }
        if let Some(v) = self.top_k {
            out.insert("top_k".into(), v.into());
        }
        if let Some(v) = self.min_p {
            out.insert("min_p".into(), v.into());
        }
        if let Some(v) = self.presence_penalty {
            out.insert("presence_penalty".into(), v.into());
        }
        serde_json::Value::Object(out)
    }

    /// The same five as one line for a reply the operator reads.
    pub fn render(&self) -> String {
        let mut out = Vec::new();
        if let Some(v) = self.temperature {
            out.push(format!("temperature={v}"));
        }
        if let Some(v) = self.top_p {
            out.push(format!("top_p={v}"));
        }
        if let Some(v) = self.top_k {
            out.push(format!("top_k={v}"));
        }
        if let Some(v) = self.min_p {
            out.push(format!("min_p={v}"));
        }
        if let Some(v) = self.presence_penalty {
            out.push(format!("presence_penalty={v}"));
        }
        out.join(" ")
    }
}

/// **One `[roles.X]` entry**: the model that role runs on, and how it samples.
///
/// Both are optional and either alone is a complete entry — a role that sets
/// only `model` runs on that model with the standing parameters, and a role that
/// sets only samplers runs on whatever model it would have run on anyway, which
/// is what a project wants when it likes the model and not the verbosity.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RoleSettings {
    /// The model this role runs on. `None` is the daemon's own default for it.
    pub model: Option<String>,
    /// How it samples. Empty is the daemon's own.
    pub sampling: RoleSampling,
}

/// **The model configuration one `leticode.toml` level carries**, as parsed from
/// the project's file or the user's.
///
/// Every model field is `None` when the file did not set it, and `None` means
/// **the level below** — not a guess, and not silently a built-in. The precedence
/// that turns these into the session's models is stated once, in
/// [`precedence`]: command line beats the project file, the project file beats
/// the user file, the user file beats the built-in default, and an unset key
/// falls through.
///
/// **This struct is the whole of what the file may set.** It carries model
/// names, sampler parameters and nothing else — no seat, no tool list, no access
/// narrowing, and no window — so a project file cannot widen a capability and
/// cannot move a wall. The `[roles]` table maps a role name to the model it runs
/// on and how it samples; it is an override, not a seating, and a role name the
/// daemon does not know is still a model the operator asked for, so it is
/// carried rather than refused (the refusal is for a key the version does not
/// know, which is a typo, not a future role).
#[derive(Debug, Clone, Default, PartialEq)]
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
    ///
    /// In this build the adjudicator and the guard are the one oracle seat, so
    /// this and [`LeticodeConfig::gatekeeper_model`] name the same thing — the
    /// file may use either word, and [`LeticodeConfig::load`] refuses the file
    /// that uses both to mean two different models.
    pub judge_model: Option<String>,
    /// The `[roles]` section: a named-role override, so a future role needs no
    /// new key. A role name maps to its model and its samplers.
    pub roles: BTreeMap<String, RoleSettings>,
    /// The path the config was read from, for the disclosure. `None` when no
    /// file was found, which is the absent-file case: the level below, and
    /// nothing to name.
    pub path: Option<PathBuf>,
    /// **Which level this is.** [`Level::Project`] or [`Level::User`] for a file
    /// this reader produced; [`Level::Builtin`] for the empty value a missing
    /// file yields. Carried on the value rather than passed alongside it so the
    /// disclosure cannot be handed a config and the wrong level for it.
    pub level: Level,
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

    /// Load one level of `leticode.toml` — the project's or the user's, the same
    /// shape at either path. A missing file is `Ok` with no models — the level
    /// below, not an error. A file that does not parse, that
    /// names a key this version does not know, or that gives one seat two
    /// different models, is `Err` with the parser's own message (or the key's
    /// name) and the path, so the operator can see which file said what.
    ///
    /// **The loud failures, in the order they are found:**
    ///
    /// * a file that will not parse says so and says where — the file, the line,
    ///   and what the parser expected;
    /// * a key the version does not know is named, not ignored —
    ///   `deny_unknown_fields` on the top level is the report for
    ///   `subagent_modle`, and [`role_settings`] names a bad key inside
    ///   `[roles.X]` itself, because serde's report for a table it cannot
    ///   deserialise names the table and not the line;
    /// * a window or a reply cap in a role's table is refused by name with the
    ///   operator's own reason (see [`WINDOW_AND_CAP_KEYS`]) — models and
    ///   samplers are configuration, the room a model has is a measurement;
    /// * a file that sets a capability rather than a model is refused by name,
    ///   because the file may set which model answers a role and may not seat a
    ///   tool on one or lift an access narrowing.
    pub fn load(path: &Path) -> Result<LeticodeConfig, String> {
        Self::load_as(path, Level::Project)
    }

    /// [`LeticodeConfig::load`], with the level the value carries named by the
    /// caller. The path is the only thing that differs between the two levels, so
    /// this is the one reader both go through; `load` is the project level, and
    /// [`load_user`] the user one.
    pub fn load_as(path: &Path, level: Level) -> Result<LeticodeConfig, String> {
        if !path.is_file() {
            return Ok(LeticodeConfig::default());
        }
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let file: LeticodeConfigFile =
            toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;

        let mut roles = BTreeMap::new();
        for (role, value) in &file.roles {
            let settings =
                role_settings(role, value).map_err(|why| format!("{}: {why}", path.display()))?;
            roles.insert(role.clone(), settings);
        }

        let config = LeticodeConfig {
            main_model: file.main_model.as_deref().and_then(non_empty),
            subagent_model: file.subagent_model.as_deref().and_then(non_empty),
            gatekeeper_model: file.gatekeeper_model.as_deref().and_then(non_empty),
            judge_model: file.judge_model.as_deref().and_then(non_empty),
            roles,
            path: Some(path.to_path_buf()),
            level,
        };

        // **One seat, two answers.** The guard and the adjudicator are the one
        // oracle in this build (`Config::oracle_model`), so `gatekeeper_model`
        // and `judge_model` are two words for one setting. A file that uses both
        // to name two DIFFERENT models is not a typo and not a future feature —
        // it is the file disagreeing with itself about who guards, so it is
        // refused rather than resolved by whichever line the reader happened to
        // apply last.
        if let (Some(g), Some(j)) = (&config.gatekeeper_model, &config.judge_model) {
            if g != j {
                return Err(format!(
                    "{}: gatekeeper_model = \"{g}\" and judge_model = \"{j}\" name ONE seat — \
                     this build's guard and adjudicator are the single oracle at the \
                     `[gatekeeper]` endpoint — so the file may set either and may not set both \
                     to different models. Set one, or set both to the same model",
                    path.display()
                ));
            }
        }

        Ok(config)
    }

    /// The model the file set for a role, or `None` when the file did not name
    /// one. A role the file does not know is `None`, which is the daemon's own
    /// default for that role — stated, not guessed.
    pub fn role_model(&self, role: &str) -> Option<&str> {
        self.roles.get(role).and_then(|r| r.model.as_deref())
    }

    /// **The model a spawn of `role` runs on, when the `task` call named none.**
    ///
    /// The seat's own `[roles.<seat>] model` first, then the file's
    /// `subagent_model`, and `None` for a file that set neither — which leaves
    /// the caller's own standing answer (`local`, the daemon's server) where it
    /// was. This is the reader that makes `subagent_model` a setting rather than
    /// a line in a disclosure: a key the file sets and nothing reads is the
    /// defect this feature exists to forbid.
    pub fn spawn_model(&self, role: &str) -> Option<&str> {
        self.role_model(role).or(self.subagent_model.as_deref())
    }

    /// **How a spawn of `role` samples.**
    ///
    /// The subagent's own numbers as the floor — the dense78 set the operator
    /// measured — then `[roles.subagent]` over them, then the seat's own
    /// `[roles.<role>]` over that, key by key. So a project can change one
    /// number for every child, or one number for one seat, and the rest of the
    /// operator's cure stays in force.
    ///
    /// **Never empty, and that is the point of the feature**: the operator's ask
    /// is that a subagent does not overthink, so a project that says nothing
    /// still gets the dense78 numbers rather than the daemon's own greedy
    /// literal. There is no window in the answer because there is none in
    /// [`RoleSampling`] — the file cannot state one.
    pub fn parameters_for(&self, role: &str) -> RoleSampling {
        let mut out = RoleSampling::dense78();
        if let Some(r) = self.roles.get(SUBAGENT_ROLE) {
            out = out.overridden_by(&r.sampling);
        }
        if role != SUBAGENT_ROLE {
            if let Some(r) = self.roles.get(role) {
                out = out.overridden_by(&r.sampling);
            }
        }
        out
    }

    /// **The two file levels as one, the project's over the user's, key by key.**
    ///
    /// This is the merge the precedence describes, applied to the keys that are
    /// not `main_model` — `subagent_model`, `gatekeeper_model`, `judge_model` and
    /// `[roles]`. Overriding rather than replacing, per key and per role, so a
    /// project that changes only `subagent_model` keeps the user file's
    /// `[roles.subagent]` samplers, which is the same rule
    /// [`RoleSampling::overridden_by`] applies one level down.
    ///
    /// `self` is the LOWER level and `other` the higher one. `path` and `level`
    /// come from `other` when it has a file, so the merged value names the file
    /// that spoke most recently rather than a file that set one key.
    pub fn overridden_by(&self, other: &LeticodeConfig) -> LeticodeConfig {
        let mut roles = self.roles.clone();
        for (role, higher) in &other.roles {
            let merged = match roles.get(role) {
                None => higher.clone(),
                Some(lower) => RoleSettings {
                    model: higher.model.clone().or_else(|| lower.model.clone()),
                    sampling: lower.sampling.overridden_by(&higher.sampling),
                },
            };
            roles.insert(role.clone(), merged);
        }
        LeticodeConfig {
            main_model: other.main_model.clone().or_else(|| self.main_model.clone()),
            subagent_model: other
                .subagent_model
                .clone()
                .or_else(|| self.subagent_model.clone()),
            gatekeeper_model: other
                .gatekeeper_model
                .clone()
                .or_else(|| self.gatekeeper_model.clone()),
            judge_model: other
                .judge_model
                .clone()
                .or_else(|| self.judge_model.clone()),
            roles,
            path: other.path.clone().or_else(|| self.path.clone()),
            level: if other.path.is_some() {
                other.level
            } else {
                self.level
            },
        }
    }

    /// Which models the file set, as the disclosure says them — and, for a role
    /// that set parameters, the parameters. Empty when the file set nothing,
    /// which is the absent-file case and the byte-identical case: a daemon with
    /// no file at this level discloses nothing from it.
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
        for (role, r) in &self.roles {
            let mut line = role.clone();
            if let Some(m) = &r.model {
                line.push(' ');
                line.push_str(m);
            }
            if !r.sampling.is_empty() {
                line.push_str(&format!(" ({})", r.sampling.render()));
            }
            // A role table that set neither is a line that said nothing, and a
            // disclosure of nothing is a disclosure that is a guess.
            if line != *role {
                out.push(line);
            }
        }
        out
    }
}

/// One `[roles.X]` entry, as the file wrote it: either a bare model name (the v1
/// shape) or a table of `model` plus samplers.
///
/// The keys are checked here rather than by serde for the reason `prompts.toml`
/// checks its section names by hand: the shape is *the operator's own keys*, and
/// serde's report for a table it cannot deserialise names the table rather than
/// the offending line. So the value is read as a [`toml::Value`] and each key is
/// answered by name — including the ones this file deliberately does not get to
/// set, which get the longer sentence in [`WINDOW_AND_CAP_KEYS`].
fn role_settings(role: &str, value: &toml::Value) -> Result<RoleSettings, String> {
    let table = match value {
        // The v1 shape: `reviewer = "deepseek/deepseek-reasoner"`.
        toml::Value::String(model) => {
            return Ok(RoleSettings {
                model: non_empty(model),
                sampling: RoleSampling::default(),
            });
        }
        toml::Value::Table(t) => t,
        other => {
            return Err(format!(
                "[roles.{role}] is {}, not a model name or a table of parameters — write \
                 `[roles.{role}] model = \"NAME\"` (plus {} as wanted), or \
                 `{role} = \"NAME\"`",
                kind_of(other),
                SAMPLER_KEYS.join(", ")
            ));
        }
    };

    let mut out = RoleSettings::default();
    for (key, value) in table {
        if key == "model" {
            out.model = match value.as_str() {
                Some(s) => non_empty(s),
                None => {
                    return Err(format!(
                        "[roles.{role}] model = {}, not a model name",
                        kind_of(value)
                    ));
                }
            };
            continue;
        }
        // **Samplers, or a refusal that says which of the two kinds of mistake
        // this is.** An unknown key names itself; a window or a cap names itself
        // AND says why this file is the wrong place for it, because the operator
        // who wrote `window = 57344` here has a real model in mind and the
        // sentence has to tell them where that number belongs (the `[model.*]`
        // block in providers.toml, or nowhere: the child's wall is measured on
        // the box that answers it).
        if WINDOW_AND_CAP_KEYS.contains(&key.as_str()) {
            return Err(format!(
                "[roles.{role}] sets `{key}`, which a project file does not get to decide: the \
                 operator's own rule is that a subagent's SAMPLERS are dense78's and its context \
                 limit is not — *\"we dont have the same context limit as with dense \
                 obviously\"* — so how much room a model has is measured on the box that answers \
                 it, and a reply cap belongs to the call. A role here sets {} (and `model`)",
                SAMPLER_KEYS.join(", ")
            ));
        }
        if !SAMPLER_KEYS.contains(&key.as_str()) {
            return Err(format!(
                "[roles.{role}] names `{key}`, which this version does not know — a role sets \
                 `model` and {}",
                SAMPLER_KEYS.join(", ")
            ));
        }
        let number = number_of(value).ok_or_else(|| {
            format!(
                "[roles.{role}] {key} = {}, which is not a number",
                kind_of(value)
            )
        })?;
        match key.as_str() {
            "temperature" => out.sampling.temperature = Some(number),
            "top_p" => out.sampling.top_p = Some(number),
            "top_k" => out.sampling.top_k = Some(number as i64),
            "min_p" => out.sampling.min_p = Some(number),
            "presence_penalty" => out.sampling.presence_penalty = Some(number),
            _ => unreachable!("checked against SAMPLER_KEYS above"),
        }
    }
    Ok(out)
}

/// The file's shape, as parsed. `deny_unknown_fields` on the top level is the
/// report for a key this version does not know: a `subagent_modle` is a parse
/// error carrying the parser's own message, not a silently ignored line. The
/// `[roles]` table is a map of [`toml::Value`], checked key by key by
/// [`role_settings`], so a role name the daemon does not know is carried rather
/// than refused — a future role needs no new key, only a line.
///
/// **The shape is the capability boundary.** The struct is four model keys and
/// a table of role settings, and nothing else. There is no field for a seat, a
/// tool list, an access narrowing or a context window, so a project file cannot
/// widen a capability: a line that would — an `allow_bash = true`, a
/// `role = "runner"` — is a top-level key the version does not know, and
/// `deny_unknown_fields` refuses it by name.
#[derive(Debug, Clone, Default, PartialEq, serde::Deserialize)]
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
    roles: BTreeMap<String, toml::Value>,
}

/// A model name that is blank is no model at all: an empty string would
/// otherwise reach a call as a model named `""`.
fn non_empty(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// A TOML number, whatever it was written as. `min_p = 0` is an INTEGER in TOML
/// and `min_p = 0.0` is a float, and both are the same instruction to a server,
/// so a key that wants a float takes either and `top_k` keeps its type.
fn number_of(value: &toml::Value) -> Option<f64> {
    match value {
        toml::Value::Integer(i) => Some(*i as f64),
        toml::Value::Float(f) => Some(*f),
        _ => None,
    }
}

/// How to say a value's kind in a refusal, so the sentence reads *"is a string,
/// not a model name"* rather than quoting bytes.
fn kind_of(value: &toml::Value) -> &'static str {
    match value {
        toml::Value::String(_) => "a string",
        toml::Value::Integer(_) => "a whole number",
        toml::Value::Float(_) => "a number",
        toml::Value::Boolean(_) => "a yes/no",
        toml::Value::Datetime(_) => "a date",
        toml::Value::Array(_) => "a list",
        toml::Value::Table(_) => "a table",
    }
}

/// **The precedence, stated once.**
///
/// Command line beats the project file, the project file beats the user file, the
/// user file beats the built-in default, and an unset key falls through to the
/// level below. That is the whole of it, and it is a function rather than a rule
/// remembered in four places, because precedence that is not stated is precedence
/// nobody can reason about — and the operator lost hours to a config that
/// silently did not reach a session, so the rule that decides which model answers
/// is written down where it is enforced.
///
/// `None` from this is the fall-through all the way out: the caller applies the
/// built-in default, which is [`Level::Builtin`] and is stated rather than
/// guessed. So the four levels and this function agree by construction — a level
/// that speaks is a level this returns, and a level that does not is not a
/// `Some` here.
///
/// **What counts as the `cli` argument is the caller's to decide, and the caller
/// decides it by the same rule the rest of the tree uses**: the command line is
/// the `cli` level when it chose WHO ANSWERS (`--provider`), and `--model` alone
/// is the local BINDING rather than that choice — see the module docs, and
/// `cli.rs`'s `apply_main_model`. Passing `--model` here would make the two file
/// levels unreachable for every launcher-started daemon, because the launcher
/// passes `--model` on every start.
pub fn precedence(
    cli: Option<&str>,
    project: Option<&str>,
    user: Option<&str>,
) -> Option<(String, Level)> {
    if let Some(m) = cli {
        return Some((m.to_string(), Level::Flag));
    }
    if let Some(m) = project {
        return Some((m.to_string(), Level::Project));
    }
    user.map(|m| (m.to_string(), Level::User))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temp dir for a `leticode.toml` fixture, unique per test so parallel
    /// tests do not share a file. Temp files, not the tree: nothing removes them,
    /// and nothing outside `std::env::temp_dir` is ever written.
    fn project_dir(test: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("letibot-leticode-{}-{test}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// The file a test case is about, written fresh so a rerun cannot see the
    /// previous case's bytes.
    fn write_file(dir: &Path, text: &str) -> PathBuf {
        let path = dir.join(FILE_NAME);
        std::fs::write(&path, text).unwrap();
        path
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
        assert_eq!(
            found, root_file,
            "the nearest file at or above the workspace"
        );
        let cfg = LeticodeConfig::load(&found).unwrap();
        assert_eq!(cfg.main_model.as_deref(), Some("root-model"));

        // A file in the workspace beats one in a parent: the nearest wins.
        let sub_file = sub.join(FILE_NAME);
        std::fs::write(&sub_file, "main_model = \"sub-model\"\n").unwrap();
        let found = LeticodeConfig::discover(&sub).expect("the walk-up finds the sub file");
        assert_eq!(
            found, sub_file,
            "the workspace's own file is nearer than the root's"
        );
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
    /// project file, the project file beats the user file, the user file beats the
    /// built-in default, and an unset key falls through. A regression here would
    /// be a model the operator set in one place that is answered by another,
    /// which is the defect this feature exists to forbid.
    ///
    /// **The level comes back with the name**, because "which model" without
    /// "which file" is the question the operator spent an evening on: the
    /// disclosure reads this pair off the screen rather than re-deriving it.
    #[test]
    fn the_precedence_is_flag_then_project_then_user_then_fall_through() {
        // The flag beats both files.
        assert_eq!(
            precedence(Some("cli"), Some("project"), Some("user")),
            Some(("cli".to_string(), Level::Flag))
        );
        // The project file beats the user file.
        assert_eq!(
            precedence(None, Some("project"), Some("user")),
            Some(("project".to_string(), Level::Project))
        );
        // The user file is the fall-through when the flag and the project are silent.
        assert_eq!(
            precedence(None, None, Some("user")),
            Some(("user".to_string(), Level::User))
        );
        // All unset is the fall-through to the built-in default, which the caller
        // names — `Level::Builtin` is the default variant for exactly this reason.
        assert_eq!(precedence(None, None, None), None);
        assert_eq!(Level::default(), Level::Builtin);
    }

    /// **The user level is the same file, read at a fixed path.** The parser is
    /// one call — [`LeticodeConfig::load_as`] — so the two levels cannot start
    /// disagreeing about what a key means, and the user level is not a second
    /// grammar to learn.
    #[test]
    fn the_user_file_is_the_same_parser_at_a_fixed_path() {
        let dir = project_dir("user_level");
        let path = write_file(&dir, "main_model = \"glm-5.3-flash\"\n");
        let user = LeticodeConfig::load_as(&path, Level::User).expect("the same reader");
        assert_eq!(user.main_model.as_deref(), Some("glm-5.3-flash"));
        assert_eq!(user.level, Level::User, "the level rides the value");
        assert_eq!(user.path.as_deref(), Some(path.as_path()));

        // The same bytes at the project level parse identically and say so.
        let project = LeticodeConfig::load(&path).expect("the project level");
        assert_eq!(project.main_model, user.main_model);
        assert_eq!(project.level, Level::Project);

        // A path with no file is `Ok` with nothing, at either level: the level
        // below, not an error.
        let absent = LeticodeConfig::load_as(&dir.join("not-here.toml"), Level::User)
            .expect("a missing file is not an error");
        assert!(absent.main_model.is_none());
        assert!(absent.path.is_none());
        assert!(absent.set_models().is_empty());

        // And the fixed path is beside `providers.toml`, not `$HOME` spelled a
        // second time: one file knows where the config dir is.
        assert_eq!(user_path().file_name().unwrap(), FILE_NAME);
        assert_eq!(
            user_path().parent(),
            letibot_provider::keys::config_file().parent()
        );
    }

    /// **An unparsable user file is loud and does not take the daemon down.** The
    /// fault is the file and the parser's own line number — the thing the
    /// operator has to fix — and the reader answers `Err` rather than a config
    /// that silently set nothing. `cli.rs` turns that `Err` into a sentence and a
    /// session that still starts, which is the behaviour `prompts.toml` already
    /// has.
    #[test]
    fn an_unparsable_user_file_names_the_file_and_the_line() {
        let dir = project_dir("user_unparsable");
        let path = write_file(
            &dir,
            "main_model = \"glm-5.3-flash\"\nthis line has no equals sign\n",
        );
        let why = LeticodeConfig::load_as(&path, Level::User)
            .expect_err("a file that does not parse is not a silent nothing");
        assert!(
            why.contains(&path.display().to_string()),
            "the refusal names the file: {why}"
        );
        assert!(
            why.contains("line 2"),
            "and the line the parser stopped on: {why}"
        );

        // **And it does not take the daemon down.** `cli.rs` turns this `Err` into a
        // sentence and runs on the level below; the value it runs on is the empty
        // config, which is a working config rather than a half-read one — no model,
        // no roles, nothing half-applied. That is the whole shape of the fallback, and
        // it is asserted here because the caller's `unwrap_or_default` is where the
        // "still starts" half lives.
        let fell_back = LeticodeConfig::load_as(&path, Level::User).unwrap_or_default();
        assert!(fell_back.main_model.is_none());
        assert!(fell_back.roles.is_empty());
        assert!(
            fell_back.path.is_none(),
            "and it names no file it did not read"
        );
        assert!(fell_back.set_models().is_empty());
        // The floor under a spawn is still the operator's own numbers, so a refused
        // file does not silently hand children the daemon's greedy literal either.
        assert_eq!(
            fell_back.parameters_for(SUBAGENT_ROLE).temperature,
            Some(0.7)
        );
    }

    /// **An unknown key at the user level is named too.** `deny_unknown_fields`
    /// is the same wall at both levels: a user file is not the place a capability
    /// sneaks in just because it is yours. `allow_bash` is the shape that would
    /// widen one, and it is refused by name rather than ignored.
    #[test]
    fn an_unknown_key_in_the_user_file_is_named() {
        let dir = project_dir("user_unknown_key");
        for (name, text, wanted) in [
            ("typo", "subagent_modle = \"local\"\n", "subagent_modle"),
            ("capability", "allow_bash = true\n", "allow_bash"),
        ] {
            let path = dir.join(format!("{name}.toml"));
            std::fs::write(&path, text).unwrap();
            let why = LeticodeConfig::load_as(&path, Level::User)
                .expect_err("a key this version does not know is refused, not ignored");
            assert!(why.contains(wanted), "{wanted} must be named: {why}");
        }
    }

    /// **The two file levels merge, key by key, the project's over the user's.**
    /// This is what makes `subagent_model` in one file and `[roles.subagent]` in
    /// the other both land: a project that changes one number keeps the user's
    /// other keys, the same rule [`RoleSampling::overridden_by`] applies one
    /// level down.
    #[test]
    fn the_project_level_overrides_the_user_level_key_by_key() {
        let dir = project_dir("user_merge");
        let user_path = dir.join("user.toml");
        std::fs::write(
            &user_path,
            "main_model = \"glm-5.3-flash\"\nsubagent_model = \"local\"\n\
             gatekeeper_model = \"qwen-3.8-27b\"\n\
             [roles.subagent]\ntemperature = 0.7\ntop_p = 0.8\n\
             [roles.coder]\ntemperature = 1.0\n",
        )
        .unwrap();
        let project_path = dir.join("project.toml");
        std::fs::write(
            &project_path,
            "main_model = \"deepseek/deepseek-chat\"\n\
             [roles.subagent]\ntemperature = 1.0\n",
        )
        .unwrap();
        let user = LeticodeConfig::load_as(&user_path, Level::User).unwrap();
        let project = LeticodeConfig::load(&project_path).unwrap();
        let merged = user.overridden_by(&project);

        assert_eq!(
            merged.main_model.as_deref(),
            Some("deepseek/deepseek-chat"),
            "the project's main_model wins"
        );
        assert_eq!(
            merged.subagent_model.as_deref(),
            Some("local"),
            "a key only the user file set survives the merge"
        );
        assert_eq!(merged.gatekeeper_model.as_deref(), Some("qwen-3.8-27b"));
        assert_eq!(merged.path.as_deref(), Some(project_path.as_path()));
        assert_eq!(merged.level, Level::Project);

        // Key by key inside a role: the project's temperature, the user's top_p.
        let sub = merged.parameters_for(SUBAGENT_ROLE);
        assert_eq!(sub.temperature, Some(1.0), "the project's number");
        assert_eq!(sub.top_p, Some(0.8), "the user's number, untouched");
        assert_eq!(
            sub.presence_penalty,
            Some(1.5),
            "and dense78 underneath both"
        );
        // A role only the user file knows is still there.
        assert_eq!(merged.role_model("coder"), None);
        assert_eq!(merged.parameters_for("coder").temperature, Some(1.0));

        // Nothing in either file is the empty merge.
        let empty = LeticodeConfig::default().overridden_by(&LeticodeConfig::default());
        assert!(empty.set_models().is_empty());
        assert!(empty.main_model.is_none());
    }

    /// **A key the version does not know is named, not ignored.** A typo'd
    /// `subagent_modle` is the bug class being fixed: a reader that accepts it
    /// and reads nothing reproduces the defect one line lower, so the reader
    /// refuses it by name, with the file and the parser's own message.
    #[test]
    fn an_unknown_key_is_named_not_ignored() {
        let dir = project_dir("unknown_key");
        let path = write_file(&dir, "subagent_modle = \"typo\"\n");
        let err = LeticodeConfig::load(&path).expect_err("an unknown key is Err");
        assert!(err.contains("subagent_modle"), "the typo is named: {err}");
        assert!(
            err.contains(FILE_NAME) || err.contains(&path.display().to_string()),
            "the file is named: {err}"
        );
    }

    /// **A typo INSIDE a role's table is named too**, and this is the half serde
    /// cannot do for us: the top-level command has `deny_unknown_fields`, but a
    /// `[roles.coder]` table is read key by key, so the report has to be written
    /// here. `temperatuer = 0.7` that is accepted and read as nothing is exactly
    /// the defect: the operator writes a number, sees no change, and has no way
    /// to tell they were ignored.
    #[test]
    fn a_typo_inside_a_role_table_is_named_not_ignored() {
        let dir = project_dir("role_typo");
        let path = write_file(&dir, "[roles.coder]\ntemperatuer = 0.7\n");
        let err = LeticodeConfig::load(&path).expect_err("a typo in a role table is Err");
        assert!(err.contains("temperatuer"), "the typo is named: {err}");
        assert!(err.contains("coder"), "the role is named: {err}");
        assert!(
            err.contains("temperature"),
            "and the keys it could have meant are listed: {err}"
        );
    }

    /// **A window or a reply cap in a role's table is refused by name, and the
    /// sentence says why.** The operator's own rule is that a subagent's
    /// SAMPLERS are dense78's and its context limit is not — *"we dont have the
    /// same context limit as with dense obviously"* — so this file has no way to
    /// state a wall: a child's window is measured on the box that answers it
    /// (`child_window_refusal` in `harness.rs`), and a reply cap belongs to the
    /// call. A file that tries gets the generic unknown-key refusal only if this
    /// test fails, which is the point: silence here would hand a child dense78's
    /// sampling AND dense78's wall, and the wall is the half that kills it.
    #[test]
    fn a_window_or_a_reply_cap_in_a_role_table_is_refused_by_name() {
        let dir = project_dir("no_window");
        for key in ["window", "context_window", "max_tokens", "n_ctx"] {
            let path = write_file(&dir, &format!("[roles.subagent]\n{key} = 57344\n"));
            let err = LeticodeConfig::load(&path).expect_err("a window is refused");
            assert!(err.contains(key), "the key is named: {err}");
            assert!(
                err.contains("context limit") || err.contains("room"),
                "and the operator's own reason is given: {err}"
            );
            assert!(
                err.contains("temperature"),
                "and the keys this file does get to set are listed: {err}"
            );
        }
        // The refusal is about the ROLE's parameters, not a ban on the word:
        // `[model."dense78"]` in providers.toml still states its window, which is
        // where a window belongs.
        let ok = write_file(&dir, "subagent_model = \"local\"\n");
        assert!(LeticodeConfig::load(&ok).is_ok());
    }

    /// **A file that will not parse says so and says where** — the file, the
    /// line, and what the parser expected — and it does not take the session
    /// down: the daemon runs on its own defaults and says so, the way
    /// `prompts.toml` already does.
    #[test]
    fn an_unparseable_file_is_loud_and_does_not_take_the_session_down() {
        let dir = project_dir("unparseable");
        let path = write_file(&dir, "main_model = \n");
        let err = LeticodeConfig::load(&path).expect_err("an unparseable file is Err");
        assert!(
            err.contains(&path.display().to_string()),
            "the file is named: {err}"
        );
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
        // A capability key, not a model key.
        let path = write_file(&dir, "allow_bash = true\n");
        let err = LeticodeConfig::load(&path).expect_err("a capability key is refused");
        assert!(
            err.contains("allow_bash"),
            "the capability key is named: {err}"
        );
        // A seat key, the other direction of the same widening.
        let path = write_file(&dir, "role = \"runner\"\n");
        let err = LeticodeConfig::load(&path).expect_err("a seat key is refused");
        assert!(err.contains("role"), "the seat key is named: {err}");
    }

    /// **The well-formed file gives the models it set**, and the disclosure
    /// names them, so "where did this model come from" is answerable from the
    /// screen — the parameters it set too, because a project that changes how a
    /// subagent samples has changed something the operator can only see here. A
    /// file that sets nothing discloses nothing, which is the absent-file case
    /// and the byte-identical case.
    #[test]
    fn a_well_formed_file_gives_its_models_and_the_disclosure_names_them() {
        let dir = project_dir("well_formed");
        let path = write_file(
            &dir,
            "main_model = \"deepseek/deepseek-chat\"\n\
             subagent_model = \"local\"\n\
             gatekeeper_model = \"qwen3-4b\"\n\
             judge_model = \"qwen3-4b\"\n\n\
             [roles]\n\
             reviewer = \"deepseek/deepseek-reasoner\"\n\n\
             [roles.subagent]\n\
             temperature = 0.7\n\
             top_p = 0.8\n\
             top_k = 20\n\
             min_p = 0\n\
             presence_penalty = 1.5\n",
        );
        let cfg = LeticodeConfig::load(&path).expect("a well-formed file is Ok");
        assert_eq!(cfg.main_model.as_deref(), Some("deepseek/deepseek-chat"));
        assert_eq!(cfg.subagent_model.as_deref(), Some("local"));
        assert_eq!(cfg.gatekeeper_model.as_deref(), Some("qwen3-4b"));
        assert_eq!(cfg.judge_model.as_deref(), Some("qwen3-4b"));
        assert_eq!(
            cfg.role_model("reviewer"),
            Some("deepseek/deepseek-reasoner")
        );
        assert_eq!(
            cfg.role_model("coder"),
            None,
            "a role the file did not name"
        );
        let set = cfg.set_models();
        assert!(
            set.iter()
                .any(|s| s.contains("main deepseek/deepseek-chat")),
            "{set:?}"
        );
        assert!(set.iter().any(|s| s.contains("subagent local")), "{set:?}");
        assert!(
            set.iter().any(|s| s.contains("gatekeeper qwen3-4b")),
            "{set:?}"
        );
        assert!(set.iter().any(|s| s.contains("judge qwen3-4b")), "{set:?}");
        assert!(
            set.iter()
                .any(|s| s.contains("reviewer deepseek/deepseek-reasoner")),
            "{set:?}"
        );
        assert!(
            set.iter()
                .any(|s| s.contains("temperature=0.7") && s.contains("presence_penalty=1.5")),
            "the parameters the file set are disclosed too: {set:?}"
        );
    }

    /// **A role the file does not know is carried, not refused.** The `[roles]`
    /// table is a named-role override, so a future role needs no new key, only a
    /// line. A role name the daemon does not know is still a model the operator
    /// asked for, so it is carried — the refusal is for a key the version does
    /// not know, which is a typo, not a future role.
    #[test]
    fn a_role_the_file_does_not_know_is_carried_not_refused() {
        let dir = project_dir("future_role");
        let path = write_file(&dir, "[roles]\nsome_future_role = \"a-model\"\n");
        let cfg = LeticodeConfig::load(&path).expect("a future role is Ok");
        assert_eq!(cfg.role_model("some_future_role"), Some("a-model"));
    }

    /// **The subagent runs on dense78's parameters, and the file does not have
    /// to say so.** The operator: *"when used in subagents mode we need to call
    /// qwen with the parameters similar to dense78 - otherwise it overthinks
    /// terribly"*. So the standing answer for a spawn is their five measured
    /// numbers, and a box with no `leticode.toml` at all gets them — the
    /// alternative is a fix that only works in projects that already knew about
    /// it. And the answer carries the five and nothing else: no window, no reply
    /// cap, because the operator's next sentence is that this is not dense78's
    /// context limit (*"we dont have the same context limit as with dense
    /// obviously"*), and the box that serves a child is not always the box that
    /// measured 57344.
    #[test]
    fn the_subagent_runs_on_the_parameters_measured_on_dense78() {
        // The absent-file case: the operator's numbers, stated in code.
        let cfg = LeticodeConfig::default();
        let p = cfg.parameters_for(SUBAGENT_ROLE);
        assert_eq!(p.temperature, Some(0.7));
        assert_eq!(p.top_p, Some(0.8));
        assert_eq!(p.top_k, Some(20));
        assert_eq!(p.min_p, Some(0.0));
        assert_eq!(p.presence_penalty, Some(1.5));

        // And as the call sees them: the five keys, in the operator's order, and
        // no key that describes the weights or the room.
        let sampling = p.to_sampling();
        let keys: Vec<&String> = sampling.as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            ["temperature", "top_p", "top_k", "min_p", "presence_penalty"],
            "the five measured on dense78 and nothing else"
        );
        assert_eq!(sampling["temperature"], 0.7);
        assert_eq!(sampling["top_k"], 20);
        assert_eq!(sampling["presence_penalty"], 1.5);
        for absent in ["window", "context_window", "max_tokens", "seed"] {
            assert!(
                sampling.get(absent).is_none(),
                "`{absent}` is not a subagent parameter: {sampling}"
            );
        }
        assert!(
            !LeticodeConfig::default()
                .parameters_for(SUBAGENT_ROLE)
                .is_empty(),
            "a subagent is never handed an empty profile -- the daemon's greedy literal is what \
             makes it overthink"
        );
    }

    /// **A role's table sets its own numbers over the operator's, key by key**,
    /// and the two levels are distinct: `[roles.subagent]` is every spawn,
    /// `[roles.coder]` is coder spawns, and a seat that says nothing keeps the
    /// rest of the operator's cure. A file that changes `temperature` alone must
    /// not lose `presence_penalty` — that is the key they named as the cure.
    #[test]
    fn a_role_table_sets_the_parameters_key_by_key() {
        let dir = project_dir("role_params");
        let path = write_file(
            &dir,
            "[roles.subagent]\ntemperature = 0.7\ntop_p = 0.8\ntop_k = 20\nmin_p = 0\n\
             presence_penalty = 1.5\n\n\
             [roles.coder]\ntemperature = 1.0\ntop_k = 40\n",
        );
        let cfg = LeticodeConfig::load(&path).expect("a role's parameters are Ok");

        // The subagent's own table: the operator's numbers, written down.
        let sub = cfg.parameters_for(SUBAGENT_ROLE);
        assert_eq!(sub.temperature, Some(0.7));
        assert_eq!(sub.presence_penalty, Some(1.5));

        // A coder spawn: its two numbers over the subagent's, the rest kept.
        let coder = cfg.parameters_for("coder");
        assert_eq!(coder.temperature, Some(1.0), "the seat's own temperature");
        assert_eq!(coder.top_k, Some(40), "and the seat's own top_k");
        assert_eq!(coder.top_p, Some(0.8), "a key it did not set is kept");
        assert_eq!(
            coder.presence_penalty,
            Some(1.5),
            "including the one the operator named as the cure"
        );

        // A project that changes ONE number keeps the rest: the same merge, one
        // level down, which is what makes the five a profile rather than five
        // unrelated knobs.
        let path = write_file(&dir, "[roles.subagent]\ntemperature = 0.2\n");
        let cfg = LeticodeConfig::load(&path).expect("one number is enough");
        let p = cfg.parameters_for(SUBAGENT_ROLE);
        assert_eq!(p.temperature, Some(0.2));
        assert_eq!(p.top_p, Some(0.8), "the operator's own dense78 top_p");
        assert_eq!(p.min_p, Some(0.0), "and a min_p SET to zero is not absent");
    }

    /// **The model a spawn runs on, and where each name comes from.** A
    /// `subagent_model` is the file's word for every child; a `[roles.X] model`
    /// is its word for one seat's children and wins over it; and a file that set
    /// neither answers `None`, which leaves the caller's own `local` where it
    /// was. Before this reader existed both keys were parsed, disclosed and read
    /// by nobody — the defect this whole file is written against.
    #[test]
    fn a_seat_model_wins_over_the_subagent_model_and_none_means_the_callers_own() {
        let dir = project_dir("spawn_model");
        let path = write_file(
            &dir,
            "subagent_model = \"local\"\n\n\
             [roles.researcher]\nmodel = \"deepseek/deepseek-reasoner\"\n",
        );
        let cfg = LeticodeConfig::load(&path).expect("both shapes are Ok");
        assert_eq!(
            cfg.spawn_model("researcher"),
            Some("deepseek/deepseek-reasoner")
        );
        assert_eq!(
            cfg.spawn_model("coder"),
            Some("local"),
            "a seat the file did not name falls to subagent_model"
        );
        assert_eq!(
            LeticodeConfig::default().spawn_model("coder"),
            None,
            "and a file that said nothing leaves `local` to the caller"
        );
    }

    /// **One seat, two answers, refused by name.** The guard and the adjudicator
    /// are the single oracle in this build, so `gatekeeper_model` and
    /// `judge_model` are two words for one setting: either may be used, the same
    /// model twice is the same statement twice, and two DIFFERENT models is the
    /// file disagreeing with itself about who guards. Resolving that by
    /// whichever line the reader applied last is the silent-config defect; the
    /// file is refused and the session runs on the daemon's own models instead.
    #[test]
    fn the_two_names_for_the_adjudicator_may_not_disagree() {
        let dir = project_dir("one_oracle");
        let path = write_file(
            &dir,
            "gatekeeper_model = \"qwen3-4b\"\njudge_model = \"glm-5.3-flash\"\n",
        );
        let err = LeticodeConfig::load(&path).expect_err("two models for one seat is Err");
        assert!(err.contains("gatekeeper_model"), "named: {err}");
        assert!(err.contains("judge_model"), "named: {err}");
        assert!(
            err.contains(&path.display().to_string()),
            "and the file: {err}"
        );

        // The same model twice, or either alone, is not a disagreement.
        let path = write_file(
            &dir,
            "gatekeeper_model = \"qwen3-4b\"\njudge_model = \"qwen3-4b\"\n",
        );
        assert!(LeticodeConfig::load(&path).is_ok());
        let path = write_file(&dir, "judge_model = \"glm-5.3-flash\"\n");
        let cfg = LeticodeConfig::load(&path).expect("one word is enough");
        assert_eq!(cfg.judge_model.as_deref(), Some("glm-5.3-flash"));
    }
}
