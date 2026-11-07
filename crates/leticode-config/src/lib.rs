//! The `leticode.toml` levels: which models answer in this project, and how its
//! subagents sample - and the same file once per USER, so "on this box, always
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
//! `leticode.toml`, at the project root or in `~/.config/letibot/` - a file, not
//! a dotfile. A file that
//! changes which model answers you should not be a surprise: the operator opens
//! the project and sees it, the way they see a `.git` or a `Cargo.toml`. A
//! hidden file that picks the model is the shape of the defect this tree has
//! already paid for - a setting that is there and does not look like it is.
//!
//! # The keys, v1
//!
//! ```toml
//! main_model = "deepseek/deepseek-chat"   # the model the session's turns go to
//! subagent_model = "local"                # the default for `task` spawns
//! gatekeeper_model = "qwen3-4b"           # the guard's own model
//! judge_model = "glm-5.3-flash"           # the adjudicator
//!
//! fallback = ["deepseek", "glm-coding/glm-5.3"]  # where a session goes when its
//!                                         # model is out, tried in this order
//! fallback_after_timeouts = 3             # consecutive model timeouts that earn a move
//!
//! [roles]                                 # a named-role override: a future role
//! reviewer = "deepseek/deepseek-reasoner" # needs no new key, only a line here
//! ```
//!
//! A fallback list is replaced by the higher level, never joined to it. A project
//! that names two models must not silently inherit a third from the user file, so a
//! non-empty list here is the whole list. `fallback_after_timeouts = 0` is refused by
//! name: a move on the first timeout is written `1`.
//!
//! `[provider]` also holds the model API deadlines: `slow_notice_seconds` (15, the red "model
//! slow" notice), `first_byte_seconds` (60) and `answer_seconds` (600), as defaults for every
//! provider, and the same three keys in `[provider.glm]` (a preset) or `[provider."glm-coding/glm-5.3"]`
//! (one model) for a slow one. The model's table wins over the preset's, the preset's over
//! `[provider]`, that over the built-in number. See [`ProviderKeys`].
//!
//! An absent key means the daemon's own default, stated as such - not a
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
//! `dense78` is a `[model."dense78"]` block in `providers.toml` - `qwen-3.8-27b`
//! at `192.168.1.78:8082` - and the same weights are served at two `n_ctx` on
//! this fleet, so a window copied from one box onto a child running on another
//! is the defect `child_window_refusal` in `harness.rs` was written for. So a
//! role sets sampler parameters and nothing that describes the weights or
//! the room: a `[roles.coder]` table carrying `temperature = 0.7` is read, and
//! the same table carrying `window = 57344` or `max_tokens = 2048` is refused
//! by name - [`WINDOW_AND_CAP_KEYS`], and the refusal says why.
//!
//! The values the operator measured on dense78 are the standing defaults for a
//! subagent ([`RoleSampling::dense78`]), so a project that writes nothing gets
//! subagents that do not overthink, and a project that wants its own numbers
//! writes them per role. They are the standing answer for every spawn, whatever
//! model answers it - the metered transport already drops what its API cannot
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
//! * the project's `leticode.toml`, found by walking up from the session's
//!   workspace - a `.env` for one project, and the file above;
//! * the user's `~/.config/letibot/leticode.toml`, at one fixed path beside
//!   `providers.toml`, `prompts.toml` and `head.toml`.
//!
//! > *"nah i thought you will do the user module thing for me."*
//!
//! One parser serves both - [`LeticodeConfig::load`], and [`load_user`] is the
//! same call at the fixed path - because two readers of one file shape is how
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
//! # ~/.config/letibot/providers.toml - written by `/models deepseek`
//! [default]
//! provider = "deepseek"
//! model = "deepseek-flash"
//! ```
//!
//! `[default]` is read into `cfg.provider` only when `cfg.provider.is_none()`
//! (`cli.rs`), and `--model` leaves `cfg.provider` at `None`. The launcher passes `--model`
//! on EVERY start, because it is the alias whose vocabulary the daemon submits token ids
//! against. So a file-level `[default]` outranked a
//! command-line model, and `letibot --glm` answered on deepseek: the flag won
//! in one direction (`--provider deepseek` beats `[default]`) and lost in the
//! other (`--model glm-5.3-flash` does not). That asymmetry is what this level
//! fixes, and it is fixed rather than hidden: `[default]` is now the level BELOW
//! this file, so a line here outranks it.
//!
//! The two files are also shaped differently, and that is the second reason. A
//! `[default]` block can only name a provider - `deepseek`, `glm`, `grok`,
//! or `local`, which is the same as saying nothing - because `DefaultChoice`
//! holds a preset name and a model id on that preset. It cannot say
//! `glm-5.3-flash`, the local alias `letibot --glm` runs, and
//! `~/.config/letibot/providers.toml`'s own comment says why that matters:
//! *"this PRESET glm-4.6 at Zhipu, metered, via `/models glm`"* is a different
//! thing from *"the LOCAL glm-5.3-flash"*. `main_model` speaks both languages -
//! `provider/model` for a meter, a bare alias for the local server - so it is
//! the key that can say the thing the operator wanted to say.
//!
//! # The precedence, stated once
//!
//! Command line beats the project file, the project file beats the user file,
//! the user file beats the built-in default, and an unset key falls through.
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
//! |            | ledger is written in), not a choice of who answers - see below. |
//! | `project`  | the nearest `leticode.toml` at or above `--workspace`            |
//! | `user`     | `~/.config/letibot/leticode.toml`                                |
//! | `built-in` | `[default]` in `providers.toml` (what `/models NAME` writes),    |
//! |            | else the local server this daemon was launched against           |
//!
//! `--model` alone is not the flag level, and that is a deliberate reading of
//! this tree's own vocabulary rather than a convenience. `scripts/letibot`
//! labels the two facts apart in its own banner: `model` is the LOCAL alias, the vocab
//! the daemon binds, and `answers` is what actually answers the turns. It passes
//! `--model` on every start, including a start where the operator typed nothing about a model at all. So treating `--model` as "the
//! operator chose who answers" would make the project file and this file
//! unreachable for every launcher-started daemon, which is the defect this level
//! exists to fix rather than to reproduce. `--provider` is the flag that says who
//! answers, and it still beats everything below it.
//!
//! One consequence is written down because it is a real bound: a file's bare
//! alias does NOT overwrite the alias `--model` bound, because the daemon
//! tokenises with its own GGUF and a mismatch is `400 Prompt contains invalid
//! tokens` at the first turn. The file's alias still clears the provider - that
//! is the half that decides who answers - and a disagreement is named at startup
//! rather than resolved in silence (see `apply_main_model` in `cli.rs`).
//!
//! # What the file may not do
//!
//! Models are configuration; permissions are not. A file at either level may set
//! which model answers a role, and it may not seat a tool on a role or lift an
//! access narrowing. The shape enforces it: the file deserialises into
//! [`LeticodeConfigFile`], which is four model keys, a fallback list and a `[roles]` table
//! of model names and samplers, with `deny_unknown_fields` on the top level. A line
//! the version does not know - a typo'd `subagent_modle`, or an `allow_bash =
//! true` that would widen a capability - is refused by name, not ignored,
//! because a silently ignored key is the bug class being fixed and a silently
//! widened capability is the worse of the two.
//!
//! # Config, not a plugin: the numbers and the head's keys
//!
//! A fixed number or a list of names is a key here, not an extension point: a layer for them
//! would cost a build and a pin to change a number. Six tables, each with today's value as its
//! documented default, so a file that says nothing is the daemon that shipped:
//!
//! ```toml
//! [queue]
//! stuck_after_seconds = 900     # a row stuck this long is the census's (`Stuck::is_news`)
//! stuck_repeat_seconds = 7200   # and the census is said again this often
//!
//! [nag]
//! after_seconds = 60            # the idle window before the plan-check (TODO_NAG_AFTER)
//! floor_seconds = 10            # the shortest re-arm an operator add earns (OPERATOR_ADD_NAG_AFTER)
//! repeat_cap_seconds = 1800     # the ceiling the doubled interval retreats to (NAG_REPEAT_CAP)
//!
//! [room]
//! tie_breaker = "deepseek/deepseek-flash"   # the model that arbitrates the room's items
//!
//! [warnings]
//! mute = ["model_slow_first_byte"]  # codes the head files and never draws
//! coalesce = true                   # notices of one kind inside a minute are one counted row
//!
//! [keys]
//! "todos.d" = "f"                   # SURFACE.KEY as the bar spells them, to one other key
//!
//! [verbosity.mine]                  # a profile IS the list of things it shows
//! show = ["edits", "tools", "thinking=folded"]
//! ```
//!
//! Three rules, and they are the whole of it. **The project's value wins** over the user's,
//! key by key, as the models do - so a project and a box-wide setting are one question with one
//! answer. The two LISTs that are suppressions instead union, `[skills] pin`'s rule: `mute` says
//! the head must not draw a kind, and a project file - writable by the sessions it governs -
//! must not unsay what the operator muted for every project. And **a bad value says its file and
//! line**, because a number nobody reads and a number read wrongly are the same defect from the
//! screen: `[nag] after_seconds = "60"` is refused where it was written, rather than silently
//! being a minute.
//!
//! The head's three tables are read here and resolved THERE (`tui/src/prefs.rs`, `visibility.rs`
//! and `keys/bindings.rs`): the surface names, the warning codes and the verbosity switches are
//! that process's vocabulary, so a word it does not know is refused with the line it came from.
//!
//! # The loud failures
//!
//! The operator lost hours to a config that silently did not reach a session -
//! a `[default] provider` that no path consulted, and an `Unreadable` that said
//! nothing. So this reader says the fault rather than swallowing it, in the
//! three places a silent config is found:
//!
//! * a file that will not parse says so and says where - the file, the
//!   line, and what the parser expected, because that is the thing the operator
//!   has to fix;
//! * a key the version does not know is named, not ignored - a typo'd
//!   `subagent_modle` at the top level or a `temperatuer` inside a role's table
//!   is the bug class being fixed, and a reader that accepts it and reads
//!   nothing reproduces it one line lower;
//! * on success, the session's startup disclosure says which models the
//!   project file set - and the parameters it set - so "where did this model
//!   come from" is answerable from the screen.
//!
//! A file that does not parse does not take the session down: the daemon runs
//! on its own defaults and says so, the way `prompts.toml` already does. That is
//! true at both levels, and a user file that will not parse is reported with the
//! file and the line and the session still starts - the same sentence the project
//! file gets, because a user file that took every daemon on the box down would be
//! a worse failure than the one it reports.
//!
//! # The seat's other file: what a leticode reads before it starts
//!
//! `leticode.toml` says who answers. The same layer decides which markdown a
//! leticode reads as its instructions - [`INSTRUCTION_FILE`], or the canonical
//! [`CANONICAL_INSTRUCTION_FILE`] when a project wrote that one instead -
//! because that is a decision about a SEAT and not about the runtime serving it.
//! The runtime asks (`leticode_config::instructions`); the two names are spelled
//! in the `instructions` module and nowhere in `letibot-harnessd`, which is what
//! keeps letibot a runtime and leticode the thing that reads.

use letibot_tools::exec::boundary::{BoundaryConfig, Entry, Origin};
use letibot_tools::exec::perimeter::{EnvValue, PerimeterTable, Secret};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub mod instructions;

// The seat's instruction files, at the crate root - the daemon writes
// `leticode_config::instructions(..)` rather than a module path, because what it
// is asking for is the answer and not the module that holds it.
pub use instructions::{BoxDirs, CANONICAL_INSTRUCTION_FILE, INSTRUCTION_FILE, instructions};

/// The file's name, at the project root and in `~/.config/letibot/`. Visible,
/// not a dotfile: a file that changes which model answers you should not be a
/// surprise.
pub const FILE_NAME: &str = "leticode.toml";

/// The user level's path: `~/.config/letibot/leticode.toml`, derived from
/// `config_file` so one file answers where the config dir is. Fixed, with no
/// walk-up: "mine" does not depend on where you stand, a project file does.
pub fn user_path() -> PathBuf {
    letibot_provider::keys::config_file()
        .parent()
        .map(|p| p.join(FILE_NAME))
        .unwrap_or_else(|| PathBuf::from(FILE_NAME))
}

/// Load the user level at `user_path`. The same call as `load`, because it is the
/// same file shape - one parser, two levels. A missing file is `Ok` with no models,
/// so a box where nobody wrote one runs on the daemon's own answer and says so.
pub fn load_user() -> Result<LeticodeConfig, String> {
    LeticodeConfig::load(&user_path())
}

/// Which level named the model that answers - the word the disclosure puts on
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

    /// The level as a sentence names it. Each contains its own word from
    /// [`Level::as_str`], because that word is what a reader scans for; a prose name
    /// sharing no substring with it would be a second vocabulary for one fact.
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

/// The role every spawn lands on, when the `task` call named no other seat. A
/// `[roles.subagent]` table is the floor under every child and a seat's own table
/// refines it, so a project that wants every child to stop overthinking says it once.
pub const SUBAGENT_ROLE: &str = "subagent";

/// The sampler keys a role's table may set, in the order the operator's own
/// dense78 profile writes them. The same five names `providers.toml`'s
/// `[model.*]` blocks use, so one vocabulary covers both files.
pub const SAMPLER_KEYS: &[&str] = &["temperature", "top_p", "top_k", "min_p", "presence_penalty"];

/// The keys a role's table may not set: a window is MEASURED where the subagent runs
/// and a reply cap belongs to the call. Refusing them by name says "I know what you
/// meant, and this file does not get to decide it", not "I do not know that key".
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

/// How one role samples - each key `Option`, so "the file said nothing" stays a
/// different fact from "the file said `temperature = 0.0`". There is no window and no
/// reply cap here because [`WINDOW_AND_CAP_KEYS`] refuses them at the parser.
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
    /// The standing numbers for a subagent, measured on `dense78`: a project that
    /// writes no `leticode.toml` still gets children that do not overthink, and one
    /// that wants other numbers overrides them key by key.
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
    /// which is an instruction - see [`RoleSampling`].
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

/// One `[roles.X]` entry: the model that role runs on and how it samples. Either
/// alone is a complete entry - a role that sets only `model` runs on the standing
/// parameters, and one that sets only samplers keeps the model it would have had.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RoleSettings {
    /// The model this role runs on. `None` is the daemon's own default for it.
    pub model: Option<String>,
    /// How it samples. Empty is the daemon's own.
    pub sampling: RoleSampling,
}

/// The model configuration one `leticode.toml` level carries: names and samplers and
/// nothing else, so a project file cannot widen a capability or move a wall. `None`
/// means the level below, and [`precedence`] states that order once.
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
    /// The adjudicator's model. `None` is the daemon's own default. In this build the
    /// adjudicator and the guard are one oracle seat, so this and `gatekeeper_model`
    /// name the same thing, and `load` refuses a file that uses both differently.
    pub judge_model: Option<String>,
    /// The `[roles]` section: a named-role override, so a future role needs no
    /// new key. A role name maps to its model and its samplers.
    pub roles: BTreeMap<String, RoleSettings>,
    /// The path the config was read from, for the disclosure. `None` when no
    /// file was found, which is the absent-file case: the level below, and
    /// nothing to name.
    pub path: Option<PathBuf>,
    /// Which level this is: `Project` or `User` for a file this reader produced,
    /// `Builtin` for the empty value a missing file yields. Carried on the value so a
    /// disclosure cannot be handed a config and the wrong level for it.
    pub level: Level,
    /// The `[memory]` table - a tool call's default cap and ceiling for sessions in this
    /// project , over the box-wide `providers.toml` numbers. See
    /// [`memory_policy`].
    pub memory: MemoryKeys,
    /// The `[skills]` table: the skills this level pins. A pin asks for a skill to stay
    /// loaded, so the two levels add up instead of one replacing the other.
    pub skills: SkillKeys,
    /// The `[github]` table - whether the reviewer seat posts, and where its token is. See
    /// [`GithubKeys`].
    pub github: GithubKeys,
    /// The `[boundary]` table: what a command's view holds beyond the built-in list. Empty is
    /// today's behaviour. See [`letibot_tools::exec::boundary`].
    pub boundary: BoundaryConfig,
    /// The `[perimeter.<name>]` tables: the two seat levels, and any role a project names, as
    /// data. Empty is today's behaviour. See [`letibot_tools::exec::perimeter`].
    pub perimeters: BTreeMap<String, PerimeterTable>,
    /// Where this session goes when its model is out: model names, tried in order.
    /// A higher level's non-empty list replaces the whole list, because a list of models a
    /// session may answer on is one decision and not a set to union.
    pub fallback: Vec<String>,
    /// How many consecutive model timeouts earn that move. `None` is the daemon's own
    /// number, and `0` never reaches here: it is refused at the parser by name.
    pub fallback_after_timeouts: Option<u32>,
    /// The `[notes]` table: where the notes checkout is and the remote it pushes to.
    pub notes: NotesKeys,
    /// The `[exec]` table: the foreground threshold and whether passing it backgrounds.
    pub exec: ExecKeys,
    /// The `[provider]` table: what a session does when its provider says the model is limited.
    /// See [`ProviderKeys`] and [`LeticodeConfig::limit_policy`].
    pub provider: ProviderKeys,
    /// The `[nag]` table: the idle plan-check's window, floor and ceiling. `None`s are the
    /// clock's own numbers; see [`NagKeys`] and [`LeticodeConfig::nag_timings`].
    pub nag: NagKeys,
    /// The `[queue]` table's two census numbers. See [`QueueKeys`].
    pub queue: QueueKeys,
    /// The `[room]` table: the model the room's tie-breaker arbitrates on.
    pub room: RoomKeys,
    /// The `[warnings]` table: what the head files and never draws. Read by the head, in
    /// `tui/src/prefs.rs`, because the codes are that process's vocabulary.
    pub warnings: WarningKeys,
    /// The `[keys]` table: a surface's binding moved to another key. Read by the head, in
    /// `tui/src/app/keys/bindings.rs`.
    pub keys: KeyKeys,
    /// The `[verbosity.<profile>]` tables: the profiles the operator composes, each the list of
    /// what it shows. Read by the head, in `tui/src/app/visibility.rs`.
    pub verbosity: VerbosityKeys,
}

/// What a session does when its provider answers a usage limit with a known reset: the
/// operator's ruling is that it never fails on one, so these are the whole of the choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnLimit {
    /// Park the session until the reset and resume the same turn by itself.
    Wait,
    /// Move the session to the next fallback model it is not limited on, and move back after.
    Switch,
}

impl OnLimit {
    /// The word in the file, in the disclosure and in the status row.
    pub fn as_str(self) -> &'static str {
        match self {
            OnLimit::Wait => "wait",
            OnLimit::Switch => "switch",
        }
    }

    /// The word back, or a refusal naming the two the file may say.
    pub fn parse(s: &str) -> Result<OnLimit, String> {
        match s.trim() {
            "wait" => Ok(OnLimit::Wait),
            "switch" => Ok(OnLimit::Switch),
            other => Err(format!(
                "on_limit = \"{other}\" is not one of the two answers: `wait` parks the session \
                 until the reset and resumes the turn by itself, `switch` moves it to the next \
                 fallback model and moves it back after the reset"
            )),
        }
    }
}

/// The three model API deadlines, as the file wrote them: each is absent until some level says it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeadlineKeys {
    /// Silence before the "model slow" notice. A notice, never a failure.
    pub slow_notice_seconds: Option<u64>,
    /// Silence before the first byte that fails the round as a timeout.
    pub first_byte_seconds: Option<u64>,
    /// The whole answer's budget.
    pub answer_seconds: Option<u64>,
}

impl DeadlineKeys {
    /// Keys `other` sets win, the rest stay.
    fn overridden_by(self, other: DeadlineKeys) -> DeadlineKeys {
        DeadlineKeys {
            slow_notice_seconds: other.slow_notice_seconds.or(self.slow_notice_seconds),
            first_byte_seconds: other.first_byte_seconds.or(self.first_byte_seconds),
            answer_seconds: other.answer_seconds.or(self.answer_seconds),
        }
    }
}

/// The deadlines one provider/model runs under, every key resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Deadlines {
    pub slow_notice: std::time::Duration,
    pub first_byte: std::time::Duration,
    pub answer: std::time::Duration,
}

/// Today's numbers, for a key no level sets. They mirror `letibot_provider::openai::DEFAULT_*`
/// (60 s, 600 s) and the daemon's silence notice (15 s).
pub const DEFAULT_SLOW_NOTICE_SECS: u64 = 15;
pub const DEFAULT_FIRST_BYTE_SECS: u64 = 60;
pub const DEFAULT_ANSWER_SECS: u64 = 600;

/// `[provider]` in a `leticode.toml`.
///
/// ```toml
/// [provider]
/// on_limit = "switch"                            # or "wait"
/// fallback = ["deepseek", "glm-coding/glm-5.3"]  # where a switch goes, in order
/// slow_notice_seconds = 15                       # silence before the "model slow" notice
/// first_byte_seconds = 60                        # silence before the round fails
/// answer_seconds = 600                           # the whole answer's budget
///
/// [provider.glm]                                 # one preset: the same three keys
/// slow_notice_seconds = 90
///
/// [provider."glm-coding/glm-5.3"]                # one model: wins over its preset
/// first_byte_seconds = 120
/// ```
///
/// `on_limit` absent means `switch` when a fallback list exists and `wait` when nothing is set -
/// see [`LeticodeConfig::limit_policy`]. `fallback` here is the same list as the file's top-level
/// `fallback`, and a file that writes both to different lists is refused by name.
///
/// The three deadline keys resolve per key: the `provider/model` table, then the preset's table,
/// then the keys here, then the built-in 15 / 60 / 600 - see [`LeticodeConfig::deadlines`]. A
/// level's key wins over the same key at the other level. `0` and a `first_byte_seconds` above
/// `answer_seconds` in one table are refused by file and line.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProviderKeys {
    pub on_limit: Option<OnLimit>,
    pub fallback: Vec<String>,
    /// The deadline keys written directly under `[provider]`.
    pub deadlines: DeadlineKeys,
    /// `[provider.NAME]` tables: NAME is a preset or `provider/model`.
    pub overrides: BTreeMap<String, DeadlineKeys>,
}

/// The first line holding `key` after the first holding `after`; `after`'s own when none.
fn line_after(text: &str, after: &str, key: &str) -> usize {
    let start = line_of(text, after);
    text.lines()
        .enumerate()
        .skip(start)
        .find(|(_, l)| l.contains(key))
        .map_or(start, |(i, _)| i + 1)
}

/// The three deadline keys of one table (`scope` is its name in a refusal, `after` where to
/// start looking for its lines).
fn deadline_keys(
    table: &toml::Table,
    path: &Path,
    text: &str,
    user: bool,
    scope: &str,
    after: &str,
) -> Result<DeadlineKeys, String> {
    let mut out = DeadlineKeys::default();
    for key in [
        "slow_notice_seconds",
        "first_byte_seconds",
        "answer_seconds",
    ] {
        let Some(v) = table.get(key) else { continue };
        let from = Origin {
            file: path.to_path_buf(),
            line: line_after(text, after, key),
            user,
        };
        let n = seconds_at(v, &from, scope, key)?;
        match key {
            "slow_notice_seconds" => out.slow_notice_seconds = Some(n),
            "first_byte_seconds" => out.first_byte_seconds = Some(n),
            _ => out.answer_seconds = Some(n),
        }
    }
    if let (Some(f), Some(a)) = (out.first_byte_seconds, out.answer_seconds)
        && f > a
    {
        return Err(format!(
            "{}:{}: [{scope}] first_byte_seconds = {f} is above answer_seconds = {a} - the first \
             byte is part of the answer, so it cannot be given longer than the whole",
            path.display(),
            line_after(text, after, "first_byte_seconds")
        ));
    }
    Ok(out)
}

fn provider_keys(
    value: &toml::Value,
    path: &Path,
    text: &str,
    user: bool,
) -> Result<ProviderKeys, String> {
    let keys = "the keys are `on_limit` (\"wait\" or \"switch\"), `fallback`, \
                `slow_notice_seconds`, `first_byte_seconds`, `answer_seconds`, and `[provider.NAME]` \
                tables (a preset or `provider/model`) holding the last three";
    let table = table_at(value, path, text, "provider", keys)?;
    let mut out = ProviderKeys::default();
    for (key, v) in table {
        let from = at(path, text, key, user);
        match key.as_str() {
            "on_limit" => {
                let s = v.as_str().ok_or_else(|| {
                    format!(
                        "{from}: [provider] on_limit is a string, not {}",
                        kind_of(v)
                    )
                })?;
                out.on_limit = Some(OnLimit::parse(s).map_err(|why| format!("{from}: {why}"))?);
            }
            "fallback" => {
                out.fallback = fallback_list(v).map_err(|why| format!("{from}: {why}"))?;
            }
            "slow_notice_seconds" | "first_byte_seconds" | "answer_seconds" => {}
            name => {
                let Some(sub) = v.as_table() else {
                    return Err(format!("{from}: [provider] `{name}` is not a key; {keys}"));
                };
                for k in sub.keys() {
                    if k.as_str() == "on_limit" || k.as_str() == "fallback" {
                        return Err(format!(
                            "{}:{}: [provider.{name}] `{k}` is not a key here; a preset or model \
                             table holds `slow_notice_seconds`, `first_byte_seconds` and \
                             `answer_seconds`",
                            path.display(),
                            line_after(text, name, k)
                        ));
                    }
                }
                let scope = format!("provider.{name}");
                out.overrides.insert(
                    name.to_string(),
                    deadline_keys(sub, path, text, user, &scope, name)?,
                );
            }
        }
    }
    out.deadlines = deadline_keys(table, path, text, user, "provider", "[provider]")?;
    Ok(out)
}

/// `[exec]` in a `leticode.toml`.
///
/// ```toml
/// [exec]
/// auto_background = true   # move a call to the background past the threshold (default)
/// foreground_seconds = 60 # the threshold
/// ```
///
/// `false` keeps the advice only. The project's file wins over the user's.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExecKeys {
    pub auto_background: Option<bool>,
    pub foreground_seconds: Option<u64>,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecTable {
    #[serde(default)]
    auto_background: Option<bool>,
    #[serde(default)]
    foreground_seconds: Option<u64>,
}

fn exec_keys(value: &toml::Value) -> Result<ExecKeys, String> {
    let t: ExecTable = value.clone().try_into().map_err(|e| {
        format!("[exec]: {e}; the keys are `auto_background` (bool) and `foreground_seconds`")
    })?;
    if t.foreground_seconds == Some(0) {
        return Err("[exec] foreground_seconds must be at least 1".into());
    }
    Ok(ExecKeys {
        auto_background: t.auto_background,
        foreground_seconds: t.foreground_seconds,
    })
}

/// The foreground policy a session's `bash` runs under: defaults, then the user file, then the
/// nearest project file.
pub fn foreground_policy(
    user: &LeticodeConfig,
    workspace: &Path,
    read_project: bool,
) -> letibot_tools::builtins::bash::ForegroundPolicy {
    let mut p = letibot_tools::builtins::bash::ForegroundPolicy::default();
    let mut apply = |k: &ExecKeys| {
        if let Some(a) = k.auto_background {
            p.auto_background = a;
        }
        if let Some(s) = k.foreground_seconds {
            p.threshold = std::time::Duration::from_secs(s);
        }
    };
    apply(&user.exec);
    if read_project && let Some(path) = LeticodeConfig::discover(workspace) {
        match LeticodeConfig::load(&path) {
            Ok(project) => apply(&project.exec),
            Err(why) => {
                eprintln!("  leticode.toml: {why} - this session's [exec] is the level below")
            }
        }
    }
    p
}

/// `[notes]` in a `leticode.toml`: the git checkout notes are written into, outside every
/// project tree, and the remote `letibot notes push` sends it to.
///
/// ```toml
/// [notes]
/// repo = "~/.local/share/letibot/notes"
/// remote = "https://github.com/letiflow/letibot-notes.git"
/// ```
///
/// The user file wins over the project's, because a project file is writable by its sessions
/// and must not move where every project's notes go.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NotesKeys {
    pub repo: Option<String>,
    pub remote: Option<String>,
}

/// The `[notes]` table as the file wrote it; unknown keys refused by name.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct NotesTable {
    #[serde(default)]
    repo: Option<String>,
    #[serde(default)]
    remote: Option<String>,
}

fn notes_keys(value: &toml::Value) -> Result<NotesKeys, String> {
    let t: NotesTable = value.clone().try_into().map_err(|e| {
        format!(
            "[notes]: {e}; the keys are `repo` (the notes checkout) and `remote` (its push URL)"
        )
    })?;
    Ok(NotesKeys {
        repo: t.repo.as_deref().and_then(non_empty),
        remote: t.remote.as_deref().and_then(non_empty),
    })
}

/// `[github]` in a `leticode.toml` - the two facts the reviewer seat needs before it may post.
///
/// ```toml
/// [github]
/// post = true
/// token_path = "~/.config/letibot/github.token"
/// ```
///
/// Nothing leaves the box until both are set. `post` is what the operator means and the token
/// is what makes it possible; a seat told to post without one says *token missing* on its scope
/// rather than failing on every push. A file that says nothing is the shipped state: every review
/// is kept as a draft, and the pane draws the draft.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GithubKeys {
    pub post: Option<bool>,
    pub token_path: Option<String>,
}

impl GithubKeys {
    /// Whether a review is sent: both facts, and neither alone.
    pub fn posting(&self) -> bool {
        self.post.unwrap_or(false) && self.token_path.is_some()
    }

    /// The two levels as one. A project file may stop the seat and may not start it: it is a
    /// file a session with `write` can edit, and starting it posts under the operator's identity
    /// on a stranger's repository.
    pub fn overridden_by(&self, other: &GithubKeys) -> GithubKeys {
        GithubKeys {
            post: match (self.post, other.post) {
                (_, Some(false)) => Some(false),
                (mine, _) => mine,
            },
            // The user's path alone: `post` from one level and a token from the other is a seat
            // the project started, which is the defect the `post` arm already refuses.
            token_path: self.token_path.clone(),
        }
    }
}

/// The `[github]` table as the file wrote it; unknown keys refused by name.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct GithubTable {
    #[serde(default)]
    post: Option<bool>,
    #[serde(default)]
    token_path: Option<String>,
}

fn github_keys(value: &toml::Value) -> Result<GithubKeys, String> {
    let t: GithubTable = value.clone().try_into().map_err(|e| {
        format!(
            "[github]: {e} — the keys are `post` (true posts reviews to GitHub) and `token_path` \
             (the file `gh` reads; mode 0600)"
        )
    })?;
    Ok(GithubKeys {
        post: t.post,
        token_path: t.token_path.as_deref().and_then(non_empty),
    })
}

/// The first line of `text` holding `needle`, 1-based; the table's own line when it is absent.
fn line_of(text: &str, needle: &str) -> usize {
    text.lines()
        .position(|l| l.contains(needle))
        .map_or(1, |i| i + 1)
}

/// `[boundary]` in a `leticode.toml`: four keys, each refused by name when misshapen. A string
/// list is `grants_ro`, `grants_rw` and `hide`; `env` is a table of constants. Every entry keeps
/// the file and line it came from, because a refusal later names them.
fn boundary_keys(
    value: &toml::Value,
    path: &Path,
    text: &str,
    user: bool,
) -> Result<BoundaryConfig, String> {
    let keys = "the keys are `grants_ro`, `grants_rw`, `hide` (lists of paths), `firecode_only` (a \
                list of program names) and `env` (a table of constants)";
    let Some(table) = value.as_table() else {
        return Err(format!(
            "{}:{}: [boundary] is a table; {keys}",
            path.display(),
            line_of(text, "boundary")
        ));
    };
    let at = |needle: &str| Origin {
        file: path.to_path_buf(),
        line: line_of(text, needle),
        user,
    };
    let mut out = BoundaryConfig::default();
    for (key, v) in table {
        let list = |what: &str| -> Result<Vec<Entry<String>>, String> {
            let bad = || {
                format!(
                    "{}:{}: [boundary] {what} is a list of path strings",
                    path.display(),
                    line_of(text, what)
                )
            };
            v.as_array()
                .ok_or_else(bad)?
                .iter()
                .map(|x| {
                    let s = x.as_str().ok_or_else(bad)?;
                    Ok(Entry {
                        value: s.to_string(),
                        from: at(&format!("\"{s}\"")),
                    })
                })
                .collect()
        };
        match key.as_str() {
            "grants_ro" => out.grants_ro = list("grants_ro")?,
            "grants_rw" => out.grants_rw = list("grants_rw")?,
            "hide" => out.hide = list("hide")?,
            "firecode_only" => {
                out.firecode_only = list("firecode_only")?;
                if let Some(bad) = out
                    .firecode_only
                    .iter()
                    .find(|e| e.value.is_empty() || e.value.contains(['/', ' ']))
                {
                    return Err(format!(
                        "{}: [boundary] firecode_only: `{}` is not a program name; write the \
                         bare name (`npm`), which also matches it by path",
                        bad.from, bad.value
                    ));
                }
            }
            "env" => {
                let Some(t) = v.as_table() else {
                    return Err(format!(
                        "{}:{}: [boundary] env is a table of NAME = \"value\"",
                        path.display(),
                        line_of(text, "env")
                    ));
                };
                for (name, val) in t {
                    let Some(val) = val.as_str() else {
                        return Err(format!(
                            "{}:{}: [boundary] env.{name} is a string",
                            path.display(),
                            line_of(text, name)
                        ));
                    };
                    out.env.push(Entry {
                        value: (name.clone(), val.to_string()),
                        from: at(name),
                    });
                }
            }
            other => {
                return Err(format!(
                    "{}:{}: [boundary] `{other}` is not a key; {keys}",
                    path.display(),
                    line_of(text, other)
                ));
            }
        }
    }
    Ok(out)
}

/// `[perimeter.<name>]` in a `leticode.toml`: the two seat levels, and any role a project names,
/// as data. Five keys, each refused by name when misshapen, and every entry keeps its file and
/// line so a refusal later names them.
///
/// ```toml
/// [perimeter.subagent]
/// grants_ro = ["~/shared"]        # USER file only: a project may not widen
/// hide = ["~/models"]             # denied after every grant
/// secrets = ["github"]            # which secrets it may carry
/// [perimeter.subagent.env]
/// RUST_LOG = "info"                                 # a constant
/// DEPLOY_TOKEN = { file = "~/token" }               # read at open, outside the box
/// DATE = { command = "date +%F" }                   # run at open, outside the box
/// ```
fn perimeter_tables(
    value: &toml::Value,
    path: &Path,
    text: &str,
    user: bool,
) -> Result<BTreeMap<String, PerimeterTable>, String> {
    let keys = "the keys are `grants_ro`, `grants_rw`, `hide` (lists of paths), `secrets` (a list \
                of names) and `env` (constants, or `{ file = ... }` / `{ command = ... }`)";
    let Some(names) = value.as_table() else {
        return Err(format!(
            "{}:{}: [perimeter] is a table of named perimeters; {keys}",
            path.display(),
            line_of(text, "perimeter")
        ));
    };
    let mut out = BTreeMap::new();
    for (name, body) in names {
        if name.trim().is_empty() {
            return Err(format!(
                "{}:{}: [perimeter] has a table with no name",
                path.display(),
                line_of(text, "perimeter")
            ));
        }
        let Some(table) = body.as_table() else {
            return Err(format!(
                "{}:{}: [perimeter.{name}] is a table; {keys}",
                path.display(),
                line_of(text, name)
            ));
        };
        let at = |needle: &str| Origin {
            file: path.to_path_buf(),
            line: line_of(text, needle),
            user,
        };
        let mut t = PerimeterTable {
            name: name.clone(),
            ..PerimeterTable::default()
        };
        for (key, v) in table {
            let list = |what: &str| -> Result<Vec<Entry<String>>, String> {
                let bad = || {
                    format!(
                        "{}:{}: [perimeter.{name}] {what} is a list of strings",
                        path.display(),
                        line_of(text, what)
                    )
                };
                v.as_array()
                    .ok_or_else(bad)?
                    .iter()
                    .map(|x| {
                        let s = x.as_str().ok_or_else(bad)?;
                        Ok(Entry {
                            value: s.to_string(),
                            from: at(&format!("\"{s}\"")),
                        })
                    })
                    .collect()
            };
            match key.as_str() {
                "grants_ro" => t.grants_ro = list("grants_ro")?,
                "grants_rw" => t.grants_rw = list("grants_rw")?,
                "hide" => t.hide = list("hide")?,
                "secrets" => {
                    for e in list("secrets")? {
                        let Some(s) = Secret::parse(&e.value) else {
                            return Err(format!(
                                "{}: [perimeter.{name}] secrets: `{}` is not a secret this build \
                                 knows; the names are: {}",
                                e.from,
                                e.value,
                                Secret::ALL
                                    .iter()
                                    .map(|s| s.name())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ));
                        };
                        t.secrets.push(Entry {
                            value: s,
                            from: e.from,
                        });
                    }
                }
                "env" => {
                    let Some(env) = v.as_table() else {
                        return Err(format!(
                            "{}:{}: [perimeter.{name}] env is a table of values",
                            path.display(),
                            line_of(text, "env")
                        ));
                    };
                    for (var, val) in env {
                        let bad = || {
                            format!(
                                "{}:{}: [perimeter.{name}] env.{var} is a string, or a table \
                                 with one of `file` or `command`",
                                path.display(),
                                line_of(text, var)
                            )
                        };
                        let value = if let Some(s) = val.as_str() {
                            EnvValue::Constant(s.to_string())
                        } else {
                            let t = val.as_table().ok_or_else(bad)?;
                            let one = |k: &str| t.get(k).and_then(|v| v.as_str());
                            match (one("file"), one("command"), t.len()) {
                                (Some(f), None, 1) => EnvValue::File(f.to_string()),
                                (None, Some(c), 1) => EnvValue::Command(c.to_string()),
                                _ => return Err(bad()),
                            }
                        };
                        t.env.push(Entry {
                            value: (var.clone(), value),
                            from: at(var),
                        });
                    }
                }
                other => {
                    return Err(format!(
                        "{}:{}: [perimeter.{name}] `{other}` is not a key; {keys}",
                        path.display(),
                        line_of(text, other)
                    ));
                }
            }
        }
        out.insert(name.clone(), t);
    }
    Ok(out)
}

/// `[skills]` in a `leticode.toml`: the skills a session keeps in hand whatever it is
/// asked to do.
///
/// ```toml
/// [skills]
/// pin = ["latex-presentation", "release"]
/// ```
///
/// Both levels' pins apply, so this is a union and not an override: a pin is a request
/// to have a skill loaded, and a project file (which a session with `write` can edit) must
/// not be able to unsay what the operator pinned for every project on the box.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkillKeys {
    pub pin: Vec<String>,
}

impl SkillKeys {
    /// The two levels as one: this level's names in its own order, then the ones `other`
    /// adds, each once. See the type's note on why a pin unions rather than overrides.
    pub fn overridden_by(&self, other: &SkillKeys) -> SkillKeys {
        let mut pin: Vec<String> = Vec::with_capacity(self.pin.len() + other.pin.len());
        for name in self.pin.iter().chain(other.pin.iter()) {
            if !pin.contains(name) {
                pin.push(name.clone());
            }
        }
        SkillKeys { pin }
    }

    /// Whether the file said anything at all, for the disclosure, which must not claim a
    /// setting nobody made.
    pub fn is_empty(&self) -> bool {
        self.pin.is_empty()
    }
}

/// The `[skills]` table as the file wrote it; unknown keys refused by name.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SkillTable {
    #[serde(default)]
    pin: Option<Vec<String>>,
}

fn skill_keys(value: &toml::Value) -> Result<SkillKeys, String> {
    let t: SkillTable = value.clone().try_into().map_err(|e| {
        format!(
            "[skills]: {e}; the one key is `pin`, a list of the skill names this level keeps \
             loaded"
        )
    })?;
    Ok(SkillKeys {
        pin: t.pin.unwrap_or_default(),
    })
}

/// `[memory]` in a `leticode.toml` - the two numbers `providers.toml` sets box-wide, set
/// for one project (or, in the user file, for every project of this user).
///
/// ```toml
/// [memory]
/// default = "4G"     # what a `bash` call that names no `memory` runs under
/// ceiling = "0.6"    # the most a call may take without asking the operator
/// ```
///
/// The operator, about a Rust project: *"when ran via tools cargo on that project dies with
/// 64mb cap"*. The box-wide 64 MiB is right for `ls` and `grep` everywhere; a project that
/// builds says so in its own file, and nothing else on the box moves. `None` is the level
/// below. The values are `exec::memory::Amount`s - a size or a fraction of what is free - and
/// a value that is not one refuses the file by name, like every other key here.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MemoryKeys {
    pub default: Option<letibot_tools::exec::memory::Amount>,
    pub ceiling: Option<letibot_tools::exec::memory::Amount>,
}

/// The `[memory]` table as the file wrote it; unknown keys refused by name.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct MemoryTable {
    #[serde(default)]
    default: Option<String>,
    #[serde(default)]
    ceiling: Option<String>,
}

fn memory_keys(value: &toml::Value) -> Result<MemoryKeys, String> {
    use letibot_tools::exec::memory::Amount;
    let t: MemoryTable = value.clone().try_into().map_err(|e| {
        format!("[memory]: {e} — the keys are `default` and `ceiling` (the box-wide `daemon_high` and `gate` live in providers.toml)")
    })?;
    let amount = |key: &str, v: Option<String>| {
        v.map(|v| Amount::parse(&v).map_err(|e| format!("[memory] {key}: {e}")))
            .transpose()
    };
    Ok(MemoryKeys {
        default: amount("default", t.default)?,
        ceiling: amount("ceiling", t.ceiling)?,
    })
}

/// The memory policy a session's `bash` runs under: the box-wide one, then the user's
/// `leticode.toml`, then the nearest project one above the workspace. A file may narrow
/// the ceiling and not raise it, because models are configuration; permissions are not.
pub fn memory_policy(
    box_wide: letibot_tools::exec::memory::MemoryPolicy,
    user: &LeticodeConfig,
    workspace: &Path,
    read_project: bool,
) -> letibot_tools::exec::memory::MemoryPolicy {
    let free = letibot_tools::exec::memory::Meminfo::read().map(|m| m.available);
    memory_policy_at(box_wide, user, workspace, read_project, free)
}

/// [`memory_policy`] with what is free given, so the narrowing rule is tested as a rule.
pub fn memory_policy_at(
    box_wide: letibot_tools::exec::memory::MemoryPolicy,
    user: &LeticodeConfig,
    workspace: &Path,
    read_project: bool,
    free: Option<u64>,
) -> letibot_tools::exec::memory::MemoryPolicy {
    use letibot_tools::exec::memory::PolicySource;
    let mut p = box_wide;
    let mut apply = |keys: &MemoryKeys, from: PolicySource, file: &Path| {
        // No `/proc/meminfo`: nothing to compare against, and no controller to cap with.
        let at = |a: letibot_tools::exec::memory::Amount| free.map(|f| a.resolve(f));
        if let Some(c) = keys.ceiling {
            match (at(c), at(p.ceiling)) {
                (Some(new), Some(old)) if new > old => eprintln!(
                    "  {}: [memory] ceiling = {c} would RAISE the ceiling above {} — a file may \
                     narrow it and not raise it, so {} stands",
                    file.display(),
                    p.ceiling,
                    p.ceiling
                ),
                _ => p.ceiling = c,
            }
        }
        if let Some(d) = keys.default {
            match (at(d), at(p.ceiling)) {
                (Some(new), Some(top)) if new > top => eprintln!(
                    "  {}: [memory] default = {d} is above the ceiling in force ({}) — a \
                     default past it would grant without asking what the operator is asked \
                     for, so {} stands",
                    file.display(),
                    p.ceiling,
                    p.default
                ),
                _ => {
                    p.default = d;
                    p.default_from = from;
                }
            }
        }
    };
    if let Some(path) = &user.path {
        apply(&user.memory, PolicySource::User, path);
    }
    if read_project && let Some(path) = LeticodeConfig::discover(workspace) {
        match LeticodeConfig::load(&path) {
            Ok(project) => apply(&project.memory, PolicySource::Project, &path),
            Err(why) => eprintln!(
                "  leticode.toml: {why} — this session's memory default is the level below"
            ),
        }
    }
    p
}

/// The file and line a value was written on, for a refusal that has to name it.
fn at(path: &Path, text: &str, needle: &str, user: bool) -> Origin {
    Origin {
        file: path.to_path_buf(),
        line: line_of(text, needle),
        user,
    }
}

/// The table a key should be, or a refusal naming the file and the table's line. Every table
/// below is walked by hand rather than by serde for this reason: serde names the table and this
/// names the line the operator wrote.
fn table_at<'a>(
    value: &'a toml::Value,
    path: &Path,
    text: &str,
    name: &str,
    keys: &str,
) -> Result<&'a toml::Table, String> {
    value.as_table().ok_or_else(|| {
        format!(
            "{}:{}: [{name}] is a table; {keys}",
            path.display(),
            line_of(text, name)
        )
    })
}

/// A whole number of seconds, at least one: every duration here is a schedule, and `0` would
/// arm a clock for ever rather than never.
fn seconds_at(v: &toml::Value, from: &Origin, table: &str, key: &str) -> Result<u64, String> {
    let n = v.as_integer().ok_or_else(|| {
        format!(
            "{from}: [{table}] {key} is a whole number of seconds, not {}",
            kind_of(v)
        )
    })?;
    u64::try_from(n).ok().filter(|n| *n > 0).ok_or_else(|| {
        format!(
            "{from}: [{table}] {key} = {n} — a schedule is at least one second, so `0` here \
             would arm a clock that never comes due"
        )
    })
}

/// One line of `[keys]`: `SURFACE.KEY = "key"`. Both words are the head's vocabulary, so this
/// reader keeps them as written and keeps the line to refuse them by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rebind {
    /// `SURFACE.KEY`, as the hint bar and `/help` spell them - `todos.d`.
    pub name: String,
    /// The key it becomes, as a key is spelled - `f`, `enter`, `ctrl-z`.
    pub key: String,
    /// The line it was written on: the head refuses a name or a key it does not know, there.
    pub from: Origin,
}

/// `[warnings]` in a `leticode.toml`: what the head files and never draws, and whether a burst of
/// one kind is one counted row. See the module doc for the two rules it inherits.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WarningKeys {
    /// Warning codes the head files but does not draw. A code and not a copy: `head.toml`'s
    /// `retired` retires one note at a time, and this retires the kind.
    pub mute: Vec<Entry<String>>,
    /// Whether notices of one kind inside a minute become one counted row. `None` is the head's
    /// own answer, which is *yes* - the burst is what keeps a hundred slow bytes one line.
    pub coalesce: Option<bool>,
}

/// `[keys]` in a `leticode.toml`: one surface's key, moved. A binding is rebound within its
/// surface and never a global chord, because a chord is the head's own escape.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct KeyKeys {
    pub rebind: Vec<Rebind>,
}

/// `[verbosity.<profile>]` in a `leticode.toml`: a profile IS the list of things it shows, and
/// the words are the head's switch names - `tools`, `thinking=folded`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileKeys {
    pub name: String,
    pub show: Vec<String>,
    pub from: Origin,
}

/// The `[verbosity]` table: every profile this level defines, in the file's order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VerbosityKeys {
    pub profiles: Vec<ProfileKeys>,
}

/// `[queue]` in a `leticode.toml`: the two numbers of the merge queue's census. `None` is the
/// daemon's own - fifteen minutes before a stuck row is the census's
/// (`mergequeue::stuck::STUCK_AFTER_MS`), and two hours between two sayings of it
/// (`builtins::todo::STUCK_REPEAT`) - and those constants stay the defaults' one home.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QueueKeys {
    pub stuck_after_seconds: Option<u64>,
    pub stuck_repeat_seconds: Option<u64>,
}

/// `[nag]` in a `leticode.toml`: the idle plan-check's window, the shortest re-arm an operator
/// add earns (X13's floor), and the ceiling a repeated check retreats to. `None` is the daemon's
/// own, which is [`letibot_tools::builtins::todo::NagTimings::default`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NagKeys {
    pub after_seconds: Option<u64>,
    pub floor_seconds: Option<u64>,
    pub repeat_cap_seconds: Option<u64>,
}

impl NagKeys {
    /// Whether the file said anything, so the disclosure does not claim a setting nobody made.
    pub fn is_empty(&self) -> bool {
        self.after_seconds.is_none()
            && self.floor_seconds.is_none()
            && self.repeat_cap_seconds.is_none()
    }

    /// The clock's numbers: today's four, overridden by what this level wrote.
    pub fn timings(&self) -> letibot_tools::builtins::todo::NagTimings {
        let mut t = letibot_tools::builtins::todo::NagTimings::default();
        let secs = std::time::Duration::from_secs;
        if let Some(s) = self.after_seconds {
            t.window = secs(s);
        }
        if let Some(s) = self.floor_seconds {
            t.floor = secs(s);
        }
        if let Some(s) = self.repeat_cap_seconds {
            t.cap = secs(s);
        }
        t
    }
}

/// `[room]` in a `leticode.toml`: the model that arbitrates the room's items. `None` is
/// `room::coordinator::DEEPSEEK`, the operator's own *"with deepseek as default"*.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoomKeys {
    pub tie_breaker: Option<String>,
}

fn warning_keys(
    value: &toml::Value,
    path: &Path,
    text: &str,
    user: bool,
) -> Result<WarningKeys, String> {
    let keys = "the keys are `mute` (a list of warning codes) and `coalesce` (true or false)";
    let table = table_at(value, path, text, "warnings", keys)?;
    let mut out = WarningKeys::default();
    for (key, v) in table {
        let from = at(path, text, key, user);
        match key.as_str() {
            "mute" => {
                let Some(items) = v.as_array() else {
                    return Err(format!(
                        "{from}: [warnings] mute is a list of warning codes, as strings; \
                         {keys}"
                    ));
                };
                for item in items {
                    let code = item.as_str().and_then(non_empty).ok_or_else(|| {
                        format!(
                            "{from}: [warnings] mute holds {} - every entry is a warning code, \
                             as a string",
                            match item.as_str() {
                                Some(s) => format!("{s:?}"),
                                None => kind_of(item).to_string(),
                            }
                        )
                    })?;
                    out.mute.push(Entry {
                        from: at(path, text, &format!("\"{code}\""), user),
                        value: code,
                    });
                }
            }
            "coalesce" => {
                out.coalesce = Some(v.as_bool().ok_or_else(|| {
                    format!(
                        "{from}: [warnings] coalesce is true or false, not {}",
                        kind_of(v)
                    )
                })?);
            }
            other => {
                return Err(format!("{from}: [warnings] `{other}` is not a key; {keys}"));
            }
        }
    }
    Ok(out)
}

impl WarningKeys {
    /// The two levels as one: the mutes union, each once, and `coalesce` is the project's answer
    /// when it gave one. See the module doc for why a mute is a union.
    pub fn overridden_by(&self, other: &WarningKeys) -> WarningKeys {
        let mut mute = self.mute.clone();
        for e in &other.mute {
            if !mute.iter().any(|m| m.value == e.value) {
                mute.push(e.clone());
            }
        }
        WarningKeys {
            mute,
            coalesce: other.coalesce.or(self.coalesce),
        }
    }
}

fn key_keys(value: &toml::Value, path: &Path, text: &str, user: bool) -> Result<KeyKeys, String> {
    let keys =
        "every key is `\"SURFACE.KEY\" = \"key\"` - a surface's binding moved to another key";
    let table = table_at(value, path, text, "keys", keys)?;
    let mut out = KeyKeys::default();
    for (name, v) in table {
        let from = at(path, text, name, user);
        let key = v.as_str().and_then(non_empty).ok_or_else(|| match v.as_str() {
            Some(_) => format!(
                "{from}: [keys] `{name}` = \"\" names no key - write the key it becomes, e.g. \"f\""
            ),
            None => format!(
                "{from}: [keys] `{name}` is one key, as a string, not {}; {keys}",
                kind_of(v)
            ),
        })?;
        if !name.contains('.') {
            return Err(format!(
                "{from}: [keys] `{name}` names no surface - a binding is written \
                 \"SURFACE.KEY\", as the hint bar spells it (`todos.d`)"
            ));
        }
        out.rebind.push(Rebind {
            name: name.clone(),
            key,
            from,
        });
    }
    Ok(out)
}

impl KeyKeys {
    /// The two levels as one, by name: a project's rebinding of one key replaces the user's, and
    /// every other name stands.
    pub fn overridden_by(&self, other: &KeyKeys) -> KeyKeys {
        let mut rebind: Vec<Rebind> = self
            .rebind
            .iter()
            .filter(|r| !other.rebind.iter().any(|o| o.name == r.name))
            .cloned()
            .collect();
        rebind.extend(other.rebind.iter().cloned());
        KeyKeys { rebind }
    }
}

fn verbosity_keys(
    value: &toml::Value,
    path: &Path,
    text: &str,
    user: bool,
) -> Result<VerbosityKeys, String> {
    let keys = "each profile is `[verbosity.NAME]` with one key, `show`, the list of things it \
                shows";
    let table = table_at(value, path, text, "verbosity", keys)?;
    let mut out = VerbosityKeys::default();
    for (name, v) in table {
        let from = at(path, text, name, user);
        let Some(body) = v.as_table() else {
            return Err(format!(
                "{from}: [verbosity.{name}] is a table with one key, `show`; {keys}"
            ));
        };
        let mut show: Option<Vec<String>> = None;
        for (key, list) in body {
            if key != "show" {
                return Err(format!(
                    "{from}: [verbosity.{name}] `{key}` is not a key; {keys}"
                ));
            }
            let Some(items) = list.as_array() else {
                return Err(format!(
                    "{from}: [verbosity.{name}] show is a list of the things it shows, as \
                     strings; {keys}"
                ));
            };
            let mut words = Vec::with_capacity(items.len());
            for item in items {
                let word = item.as_str().and_then(non_empty).ok_or_else(|| {
                    format!(
                        "{from}: [verbosity.{name}] show holds {} - every entry is a switch \
                         name, as a string",
                        match item.as_str() {
                            Some(s) => format!("{s:?}"),
                            None => kind_of(item).to_string(),
                        }
                    )
                })?;
                words.push(word);
            }
            show = Some(words);
        }
        // A profile with no `show` is a name for hiding everything, which `conversation` already
        // is - and a name that draws nothing is a setting that reads as a broken one.
        let Some(show) = show else {
            return Err(format!(
                "{from}: [verbosity.{name}] has no `show` list; {keys}"
            ));
        };
        out.profiles.push(ProfileKeys {
            name: name.clone(),
            show,
            from,
        });
    }
    Ok(out)
}

impl VerbosityKeys {
    /// The two levels as one, by name: a project's profile of the same name replaces the user's,
    /// so a project may tighten or widen a box-wide profile without renaming it.
    pub fn overridden_by(&self, other: &VerbosityKeys) -> VerbosityKeys {
        let mut profiles: Vec<ProfileKeys> = self
            .profiles
            .iter()
            .filter(|p| !other.profiles.iter().any(|o| o.name == p.name))
            .cloned()
            .collect();
        profiles.extend(other.profiles.iter().cloned());
        VerbosityKeys { profiles }
    }
}

fn queue_keys(
    value: &toml::Value,
    path: &Path,
    text: &str,
    user: bool,
) -> Result<QueueKeys, String> {
    let keys = "the keys are `stuck_after_seconds` and `stuck_repeat_seconds` - the merge \
                queue's census; the rest of `[queue]` is the queue's own reader";
    let table = table_at(value, path, text, "queue", keys)?;
    let mut out = QueueKeys::default();
    // The queue's own reader owns the other keys of this table and refuses a name neither knows,
    // so this one reads what it knows and leaves the rest where it is.
    for (key, v) in table {
        let from = at(path, text, key, user);
        match key.as_str() {
            "stuck_after_seconds" => {
                out.stuck_after_seconds = Some(seconds_at(v, &from, "queue", key)?)
            }
            "stuck_repeat_seconds" => {
                out.stuck_repeat_seconds = Some(seconds_at(v, &from, "queue", key)?)
            }
            _ => {}
        }
    }
    Ok(out)
}

fn nag_keys(value: &toml::Value, path: &Path, text: &str, user: bool) -> Result<NagKeys, String> {
    let keys = "the keys are `after_seconds`, `floor_seconds` and `repeat_cap_seconds`";
    let table = table_at(value, path, text, "nag", keys)?;
    let mut out = NagKeys::default();
    for (key, v) in table {
        let from = at(path, text, key, user);
        match key.as_str() {
            "after_seconds" => out.after_seconds = Some(seconds_at(v, &from, "nag", key)?),
            "floor_seconds" => out.floor_seconds = Some(seconds_at(v, &from, "nag", key)?),
            "repeat_cap_seconds" => {
                out.repeat_cap_seconds = Some(seconds_at(v, &from, "nag", key)?)
            }
            other => return Err(format!("{from}: [nag] `{other}` is not a key; {keys}")),
        }
    }
    // A floor above the cap is a ladder that cannot be climbed: every re-arm is clamped into
    // `[floor, cap]` and the two ends would cross.
    if let (Some(floor), Some(cap)) = (out.floor_seconds, out.repeat_cap_seconds)
        && floor > cap
    {
        return Err(format!(
            "{}: [nag] floor_seconds = {floor} is above repeat_cap_seconds = {cap} - the check \
             is clamped between the two, and a floor past the ceiling is a space no re-arm can \
             land in",
            path.display()
        ));
    }
    Ok(out)
}

fn room_keys(value: &toml::Value, path: &Path, text: &str, user: bool) -> Result<RoomKeys, String> {
    let keys = "the one key is `tie_breaker`, the model that arbitrates the room's items";
    let table = table_at(value, path, text, "room", keys)?;
    let mut out = RoomKeys::default();
    for (key, v) in table {
        let from = at(path, text, key, user);
        match key.as_str() {
            "tie_breaker" => {
                out.tie_breaker = Some(v.as_str().and_then(non_empty).ok_or_else(|| {
                    match v.as_str() {
                        Some(_) => format!("{from}: [room] tie_breaker = \"\" names no model"),
                        None => format!(
                            "{from}: [room] tie_breaker is a model name, as a string, not {}",
                            kind_of(v)
                        ),
                    }
                })?);
            }
            other => return Err(format!("{from}: [room] `{other}` is not a key; {keys}")),
        }
    }
    Ok(out)
}

impl LeticodeConfig {
    /// The walk-up: the nearest `leticode.toml` at or above `workspace`, the way `.env`
    /// and `.git` are found, so a session started in a subdirectory still reaches the
    /// project's settings. The nearest file wins and the walk stops at the root.
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

    /// Load one level of `leticode.toml`, the project's or the user's: the same shape at
    /// either path. A missing file is `Ok` with no models; a file that will not parse,
    /// names an unknown key, or gives one seat two models is `Err` with the path.
    pub fn load(path: &Path) -> Result<LeticodeConfig, String> {
        Self::load_as(path, Level::Project)
    }

    /// `load`, with the level named by the caller. The path is the only thing that
    /// differs between the two levels, so this is the one reader both go through.
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

        let mut config = LeticodeConfig {
            main_model: file.main_model.as_deref().and_then(non_empty),
            subagent_model: file.subagent_model.as_deref().and_then(non_empty),
            gatekeeper_model: file.gatekeeper_model.as_deref().and_then(non_empty),
            judge_model: file.judge_model.as_deref().and_then(non_empty),
            roles,
            path: Some(path.to_path_buf()),
            level,
            memory: match &file.memory {
                Some(v) => memory_keys(v).map_err(|why| format!("{}: {why}", path.display()))?,
                None => MemoryKeys::default(),
            },
            skills: match &file.skills {
                Some(v) => skill_keys(v).map_err(|why| format!("{}: {why}", path.display()))?,
                None => SkillKeys::default(),
            },
            github: match &file.github {
                Some(v) => github_keys(v).map_err(|why| format!("{}: {why}", path.display()))?,
                None => GithubKeys::default(),
            },
            boundary: match &file.boundary {
                Some(v) => boundary_keys(v, path, &text, level == Level::User)?,
                None => BoundaryConfig::default(),
            },
            perimeters: match &file.perimeter {
                Some(v) => perimeter_tables(v, path, &text, level == Level::User)?,
                None => BTreeMap::new(),
            },
            fallback: match &file.fallback {
                Some(v) => fallback_list(v).map_err(|why| format!("{}: {why}", path.display()))?,
                None => Vec::new(),
            },
            fallback_after_timeouts: match &file.fallback_after_timeouts {
                Some(v) => {
                    Some(fallback_timeouts(v).map_err(|why| format!("{}: {why}", path.display()))?)
                }
                None => None,
            },
            notes: match &file.notes {
                Some(v) => notes_keys(v).map_err(|why| format!("{}: {why}", path.display()))?,
                None => NotesKeys::default(),
            },
            exec: match &file.exec {
                Some(v) => exec_keys(v).map_err(|why| format!("{}: {why}", path.display()))?,
                None => ExecKeys::default(),
            },
            provider: match &file.provider {
                Some(v) => provider_keys(v, path, &text, level == Level::User)?,
                None => ProviderKeys::default(),
            },
            // The six tables below refuse with their own file and line (`{from}`), so their
            // message is the whole sentence and there is nothing to prefix.
            nag: match &file.nag {
                Some(v) => nag_keys(v, path, &text, level == Level::User)?,
                None => NagKeys::default(),
            },
            queue: match &file.queue {
                Some(v) => queue_keys(v, path, &text, level == Level::User)?,
                None => QueueKeys::default(),
            },
            room: match &file.room {
                Some(v) => room_keys(v, path, &text, level == Level::User)?,
                None => RoomKeys::default(),
            },
            warnings: match &file.warnings {
                Some(v) => warning_keys(v, path, &text, level == Level::User)?,
                None => WarningKeys::default(),
            },
            keys: match &file.keys {
                Some(v) => key_keys(v, path, &text, level == Level::User)?,
                None => KeyKeys::default(),
            },
            verbosity: match &file.verbosity {
                Some(v) => verbosity_keys(v, path, &text, level == Level::User)?,
                None => VerbosityKeys::default(),
            },
        };

        // One list of models, two places to write it. `[provider] fallback` is the newer home of
        // the top-level key, so a file may carry either - and a file that carries both to
        // different lists is refused rather than resolved, because the answer would be a guess.
        if !config.provider.fallback.is_empty() {
            if !config.fallback.is_empty() && config.fallback != config.provider.fallback {
                return Err(format!(
                    "{}: `fallback` and `[provider] fallback` are one list, and this file gives \
                     them different ones. Write it once, under `[provider]`",
                    path.display()
                ));
            }
            config.fallback = config.provider.fallback.clone();
        }

        // One seat, two answers. The guard and the adjudicator are one oracle in this
        // build, so `gatekeeper_model` and `judge_model` are two words for one setting;
        // a file that names two different models is refused rather than resolved.
        if let (Some(g), Some(j)) = (&config.gatekeeper_model, &config.judge_model)
            && g != j
        {
            return Err(format!(
                "{}: gatekeeper_model = \"{g}\" and judge_model = \"{j}\" name ONE seat — \
                     this build's guard and adjudicator are the single oracle at the \
                     `[gatekeeper]` endpoint — so the file may set either and may not set both \
                     to different models. Set one, or set both to the same model",
                path.display()
            ));
        }

        Ok(config)
    }

    /// The model the file set for a role, or `None` when the file did not name
    /// one. A role the file does not know is `None`, which is the daemon's own
    /// default for that role - stated, not guessed.
    pub fn role_model(&self, role: &str) -> Option<&str> {
        self.roles.get(role).and_then(|r| r.model.as_deref())
    }

    /// The model a spawn of `role` runs on, when the `task` call named none: the seat's
    /// own `[roles.<seat>] model` first, then `subagent_model`, then `None`, which leaves
    /// the caller's own answer where it was. A key nothing reads is the defect here.
    pub fn spawn_model(&self, role: &str) -> Option<&str> {
        self.role_model(role).or(self.subagent_model.as_deref())
    }

    /// How a spawn of `role` samples: the subagent's own numbers as the floor, then
    /// `[roles.subagent]` over them, then `[roles.<role>]` over that, key by key. Never
    /// empty, so a project that says nothing still gets numbers that do not overthink.
    pub fn parameters_for(&self, role: &str) -> RoleSampling {
        let mut out = RoleSampling::dense78();
        if let Some(r) = self.roles.get(SUBAGENT_ROLE) {
            out = out.overridden_by(&r.sampling);
        }
        if role != SUBAGENT_ROLE
            && let Some(r) = self.roles.get(role)
        {
            out = out.overridden_by(&r.sampling);
        }
        out
    }

    /// The two file levels as one, the project's over the user's, key by key: overriding
    /// rather than replacing, so a project that changes only `subagent_model` keeps the
    /// user file's samplers. `path` and `level` come from `other` when it has a file.
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
            memory: MemoryKeys {
                default: other.memory.default.or(self.memory.default),
                ceiling: other.memory.ceiling.or(self.memory.ceiling),
            },
            skills: self.skills.overridden_by(&other.skills),
            github: self.github.overridden_by(&other.github),
            boundary: self.boundary.overridden_by(&other.boundary),
            // The project wins per NAME, and only what it wrote: a project that says nothing
            // about `reviewer` keeps the user file's `reviewer`.
            perimeters: {
                let mut out = self.perimeters.clone();
                for (name, table) in &other.perimeters {
                    let merged = match out.get(name) {
                        Some(lower) => lower.overridden_by(table),
                        None => table.clone(),
                    };
                    out.insert(name.clone(), merged);
                }
                out
            },
            // Replaced, not joined: a project that names two models must not inherit a third.
            fallback: if other.fallback.is_empty() {
                self.fallback.clone()
            } else {
                other.fallback.clone()
            },
            fallback_after_timeouts: other
                .fallback_after_timeouts
                .or(self.fallback_after_timeouts),
            exec: ExecKeys {
                auto_background: other.exec.auto_background.or(self.exec.auto_background),
                foreground_seconds: other
                    .exec
                    .foreground_seconds
                    .or(self.exec.foreground_seconds),
            },
            // The user's over the project's, the reverse of the models: see [`NotesKeys`].
            notes: NotesKeys {
                repo: self.notes.repo.clone().or_else(|| other.notes.repo.clone()),
                remote: self
                    .notes
                    .remote
                    .clone()
                    .or_else(|| other.notes.remote.clone()),
            },
            provider: ProviderKeys {
                // The project's answer wins, and an absent one says nothing.
                on_limit: other.provider.on_limit.or(self.provider.on_limit),
                fallback: if other.provider.fallback.is_empty() {
                    self.provider.fallback.clone()
                } else {
                    other.provider.fallback.clone()
                },
                deadlines: self
                    .provider
                    .deadlines
                    .overridden_by(other.provider.deadlines),
                overrides: {
                    let mut m = self.provider.overrides.clone();
                    for (name, k) in &other.provider.overrides {
                        let base = m.get(name).copied().unwrap_or_default();
                        m.insert(name.clone(), base.overridden_by(*k));
                    }
                    m
                },
            },
            // The three numbers the runtime threads, and the head's three tables: the project's
            // answer for each key, and the two lists that are suppressions (`mute`, and a profile
            // of the same name) resolve in their own types' `overridden_by`.
            nag: NagKeys {
                after_seconds: other.nag.after_seconds.or(self.nag.after_seconds),
                floor_seconds: other.nag.floor_seconds.or(self.nag.floor_seconds),
                repeat_cap_seconds: other.nag.repeat_cap_seconds.or(self.nag.repeat_cap_seconds),
            },
            queue: QueueKeys {
                stuck_after_seconds: other
                    .queue
                    .stuck_after_seconds
                    .or(self.queue.stuck_after_seconds),
                stuck_repeat_seconds: other
                    .queue
                    .stuck_repeat_seconds
                    .or(self.queue.stuck_repeat_seconds),
            },
            room: RoomKeys {
                tie_breaker: other
                    .room
                    .tie_breaker
                    .clone()
                    .or_else(|| self.room.tie_breaker.clone()),
            },
            warnings: self.warnings.overridden_by(&other.warnings),
            keys: self.keys.overridden_by(&other.keys),
            verbosity: self.verbosity.overridden_by(&other.verbosity),
        }
    }

    /// The idle plan-check's clock, as this file configures it: `[nag]`'s three numbers over the
    /// clock's own, and the census's repeat from `[queue]` - one value, because one clock arms
    /// both and a session that took two would drift.
    pub fn nag_timings(&self) -> letibot_tools::builtins::todo::NagTimings {
        let mut t = self.nag.timings();
        if let Some(s) = self.queue.stuck_repeat_seconds {
            t.census_repeat = std::time::Duration::from_secs(s);
        }
        t
    }

    /// How long a row must be stuck before the census says so: `[queue] stuck_after_seconds`, or
    /// the daemon's own number - `None` is the caller's [`crate::QueueKeys`] default, which lives
    /// beside the constant (`mergequeue::stuck::STUCK_AFTER_MS`).
    pub fn stuck_after_seconds(&self) -> Option<u64> {
        self.queue.stuck_after_seconds
    }

    /// What this session does when its provider answers a usage limit with a known reset: the
    /// file's own word, else `switch` when a fallback list exists, else `wait` - the two defaults
    /// are the operator's ruling, since a session that fails on a limit the provider put a time on
    /// has thrown the turn away for nothing.
    pub fn limit_policy(&self) -> OnLimit {
        self.provider
            .on_limit
            .unwrap_or(if self.fallback.is_empty() {
                OnLimit::Wait
            } else {
                OnLimit::Switch
            })
    }

    /// The deadlines for a session on `name` (a preset) and `model`: per key, the
    /// `[provider."name/model"]` table, then `[provider.name]`, then `[provider]`, then the
    /// built-in numbers. A first byte above the answer budget (two levels disagreeing) is cut to it.
    pub fn deadlines(&self, name: &str, model: Option<&str>) -> Deadlines {
        let p = &self.provider;
        let pick = |f: fn(&DeadlineKeys) -> Option<u64>, default: u64| {
            model
                .and_then(|m| p.overrides.get(&format!("{name}/{m}")))
                .and_then(f)
                .or_else(|| p.overrides.get(name).and_then(f))
                .or_else(|| f(&p.deadlines))
                .unwrap_or(default)
        };
        let secs = std::time::Duration::from_secs;
        let answer = secs(pick(|k| k.answer_seconds, DEFAULT_ANSWER_SECS));
        Deadlines {
            slow_notice: secs(pick(|k| k.slow_notice_seconds, DEFAULT_SLOW_NOTICE_SECS)),
            first_byte: secs(pick(|k| k.first_byte_seconds, DEFAULT_FIRST_BYTE_SECS)).min(answer),
            answer,
        }
    }

    /// Which models the file set, as the disclosure says them, and for a role that set
    /// parameters, the parameters. Empty when the file set nothing, which is the
    /// absent-file case: a daemon with no file at this level discloses nothing.
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
        // A fallback is a model this session may answer on, so it is disclosed here too.
        // The budget is a number and not a model, so it stays out of this list.
        for m in &self.fallback {
            out.push(format!("fallback {m}"));
        }
        // And the answer to a limit, when the file gave one: an absent key is the daemon's own
        // default, which `limit_policy` states, and the disclosure must not claim a setting nobody
        // made.
        if let Some(o) = self.provider.on_limit {
            out.push(format!("on_limit {}", o.as_str()));
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

/// One `[roles.X]` entry, as the file wrote it: a bare model name or a table of `model`
/// plus samplers. The keys are checked here rather than by serde because serde's report
/// names the table, not the line, and these are the operator's own keys.
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
        // Samplers, or a refusal that says which of the two mistakes this is: an unknown
        // key names itself; a window or a cap names itself and says why this file is the
        // wrong place for it, because the number has a real model behind it.
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

/// The file's shape, as parsed. `deny_unknown_fields` on the top level is the report for
/// a key this version does not know, and the shape IS the capability boundary: four model
/// keys, a fallback list and a role table, with no field for a seat, a tool list or a window.
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
    /// The `[queue]` table, read by its own parser: the two census numbers are
    /// [`queue_keys`]'s, and the rest belongs to the queue's own reader
    /// (`harnessd/src/mergequeue/settings.rs`), which refuses a name neither knows.
    #[serde(default)]
    queue: Option<toml::Value>,
    /// The `[nag]` table, see [`NagKeys`].
    #[serde(default)]
    nag: Option<toml::Value>,
    /// The `[room]` table, see [`RoomKeys`].
    #[serde(default)]
    room: Option<toml::Value>,
    /// The `[warnings]` table, see [`WarningKeys`].
    #[serde(default)]
    warnings: Option<toml::Value>,
    /// The `[keys]` table, see [`KeyKeys`].
    #[serde(default)]
    keys: Option<toml::Value>,
    /// The `[verbosity]` table, see [`VerbosityKeys`].
    #[serde(default)]
    verbosity: Option<toml::Value>,
    /// The `[memory]` table - see [`MemoryKeys`]. A cap is not a capability: it narrows
    /// what a call may hold, and the ceiling above which the operator is asked still holds.
    #[serde(default)]
    memory: Option<toml::Value>,
    /// The `[skills]` table: a pin says which skills stay loaded, so it sits on this side
    /// of the wall, seating no tool and lifting no narrowing.
    #[serde(default)]
    skills: Option<toml::Value>,
    /// The `[github]` table - see [`GithubKeys`]. Accepted here so the seat reads one file
    /// for both levels; which level may start it is the merge's own rule, not this reader's.
    #[serde(default)]
    github: Option<toml::Value>,
    /// The `[boundary]` table, see [`boundary_keys`].
    #[serde(default)]
    boundary: Option<toml::Value>,
    /// The `[perimeter.<name>]` tables, see [`perimeter_tables`].
    #[serde(default)]
    perimeter: Option<toml::Value>,
    /// The `[exec]` table, see [`ExecKeys`].
    #[serde(default)]
    exec: Option<toml::Value>,
    /// The fallback list and its budget, see [`fallback_list`] and [`fallback_timeouts`].
    /// Raw here so a value of the wrong shape is refused by name rather than by serde's
    /// message, which names the table and not the key the operator wrote.
    #[serde(default)]
    fallback: Option<toml::Value>,
    /// The `[provider]` table - what a session does when the model is limited, and where it goes
    /// instead. See [`ProviderKeys`].
    #[serde(default)]
    provider: Option<toml::Value>,
    #[serde(default)]
    fallback_after_timeouts: Option<toml::Value>,
    /// The `[embeddings]` table, read by `letibot_embeddings::Settings` from this same file.
    /// Accepted and not interpreted: it seats no tool and sets no model, so refusing the file for
    /// carrying it would refuse a key this reader has no opinion about, as `[queue]` is accepted.
    #[serde(default)]
    #[allow(dead_code)]
    embeddings: Option<toml::Value>,
    /// The `[notes]` table, see [`NotesKeys`].
    #[serde(default)]
    notes: Option<toml::Value>,
}

/// The fallback list as the file wrote it: an array of model names, in the order they are
/// tried. Each entry is refused by name, because a list that kept a blank or a number would
/// hand a call a model nobody named.
fn fallback_list(value: &toml::Value) -> Result<Vec<String>, String> {
    let items = match value {
        toml::Value::Array(items) => items,
        other => {
            return Err(format!(
                "fallback is {}, not a list of model names; write \
                 `fallback = [\"NAME\", \"NAME\"]`",
                kind_of(other)
            ));
        }
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let name = match item.as_str() {
            Some(s) => non_empty(s).ok_or_else(|| {
                "fallback holds an empty name, which is no model at all".to_string()
            })?,
            None => {
                return Err(format!(
                    "fallback holds {}, not a model name: every entry is a name, \
                     as a string",
                    kind_of(item)
                ));
            }
        };
        out.push(name);
    }
    Ok(out)
}

/// The timeout budget as the file wrote it: a whole number of at least one. Zero is refused
/// by name because a file that wrote `0` wanted a move on the first timeout, which is `1`.
fn fallback_timeouts(value: &toml::Value) -> Result<u32, String> {
    let n = match value {
        toml::Value::Integer(i) => *i,
        other => {
            return Err(format!(
                "fallback_after_timeouts = {}, not a whole number of consecutive timeouts",
                kind_of(other)
            ));
        }
    };
    match u32::try_from(n).ok().filter(|n| *n >= 1) {
        Some(n) => Ok(n),
        None => Err(format!(
            "fallback_after_timeouts = {n}, and the budget is a whole number of timeouts, at \
             least one; a move on the first timeout is `1`"
        )),
    }
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

/// The precedence, stated once: command line beats the project file, the project file
/// beats the user file, the user file beats the built-in default, and an unset key falls
/// through. `--model` is not the command-line level; it names the local binding.
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

    /// A fresh subdirectory, for a test that needs two files in one temp tree.
    fn sub(dir: &Path, name: &str) -> PathBuf {
        let d = dir.join(name);
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

    /// The walk-up finds the nearest file and stops at the root, so a session started in a
    /// subdirectory finds the project's settings and a file in a parent does not shadow
    /// one in the workspace.
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

    /// The precedence, stated once and checked here: command line, project file, user
    /// file, built-in default, with an unset key falling through. The level comes back
    /// with the name, because "which model" without "which file" is half an answer.
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
        // names - `Level::Builtin` is the default variant for exactly this reason.
        assert_eq!(precedence(None, None, None), None);
        assert_eq!(Level::default(), Level::Builtin);
    }

    /// The user level is the same file at a fixed path, read by the same call, so the two
    /// levels cannot disagree about what a key means and the user level is not a second
    /// grammar to learn.
    #[test]
    fn the_user_files_notes_keys_win_over_the_projects() {
        let u = project_dir("notes_user");
        let p = project_dir("notes_project");
        let user = LeticodeConfig::load_as(
            &write_file(&u, "[notes]\nrepo = \"~/notes\"\n"),
            Level::User,
        )
        .expect("user");
        let project = LeticodeConfig::load(&write_file(
            &p,
            "[notes]\nrepo = \"/elsewhere\"\nremote = \"https://example.invalid/n.git\"\n",
        ))
        .expect("project");
        let merged = user.overridden_by(&project);
        assert_eq!(merged.notes.repo.as_deref(), Some("~/notes"));
        assert_eq!(
            merged.notes.remote.as_deref(),
            Some("https://example.invalid/n.git"),
            "a key the user left unset falls through to the project's"
        );
        let bad = LeticodeConfig::load(&write_file(&p, "[notes]\ndir = \"x\"\n"));
        assert!(bad.unwrap_err().contains("[notes]"));
    }

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

    /// An unparsable user file is loud and does not take the daemon down: the reader
    /// answers `Err` rather than a config that silently set nothing, and the caller turns
    /// that into a sentence and a session that still starts.
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

        // And it does not take the daemon down: the caller turns this `Err` into a
        // sentence and runs on the empty config, which is a working config rather than a
        // half-read one. Asserted here because the caller's fallback is where it lives.
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

    /// An unknown key at the user level is named too: `deny_unknown_fields` is the same
    /// wall at both levels, so a user file is not the place a capability sneaks in just
    /// because it is yours.
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

    /// The two file levels merge key by key, the project's over the user's, so
    /// `subagent_model` in one file and `[roles.subagent]` in the other both land.
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

    /// A key the version does not know is named, not ignored: a reader that accepts a
    /// typo'd `subagent_modle` and reads nothing reproduces the defect one line lower.
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

    /// The `[embeddings]` table is carried and not interpreted: it names where a vector comes
    /// from, so refusing the file for carrying it would make the mode unwritable on every box
    /// that declared one, and this reader seats no tool and sets no model from it.
    #[test]
    fn the_embeddings_table_is_carried_not_refused() {
        let dir = project_dir("embeddings_table");
        let path = write_file(
            &dir,
            "[embeddings]\nmode = \"internal\"\nmodel = \"/tmp/nowhere\"\n",
        );
        let cfg = LeticodeConfig::load(&path).expect("the section is carried");
        assert!(cfg.main_model.is_none(), "and sets no model at this level");
    }

    /// A typo inside a role's table is named too, and that is the half serde cannot do:
    /// `[roles.coder]` is read key by key, so a `temperatuer` that was accepted and read
    /// as nothing would leave the operator no way to tell they were ignored.
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

    /// A window or a reply cap in a role's table is refused by name, and the sentence says
    /// why: a child's samplers are the operator's numbers but its context limit is not,
    /// because that is measured on the box that answers it.
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

    /// A file that will not parse says so and says where, and it does not take the session
    /// down: the daemon runs on its own defaults and says so.
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

    /// An absent file is simply the defaults: the daemon's own models and nothing to name.
    /// A regression here would put a complaint on every start of every daemon with no
    /// project file, which is most of them.
    #[test]
    fn an_absent_file_is_silently_the_defaults() {
        let dir = project_dir("absent");
        let missing = dir.join(FILE_NAME);
        assert!(!missing.exists());
        let cfg = LeticodeConfig::load(&missing).expect("a missing file is Ok");
        assert_eq!(cfg, LeticodeConfig::default());
        assert!(cfg.set_models().is_empty(), "nothing to disclose");
    }

    /// The file may set a model and may not widen a capability: an `allow_bash` or a
    /// `role = "runner"` is a top-level key the version does not know, and
    /// `deny_unknown_fields` refuses it by name.
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

    /// The well-formed file gives the models it set and the parameters it set, so "where
    /// did this model come from" is answerable from the screen. A file that sets nothing
    /// discloses nothing, which is the absent-file case.
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

    /// A role the file does not know is carried, not refused: `[roles]` is a named-role
    /// override, so a future role needs a line and no new key. The refusal is for a key
    /// the version does not know, which is a typo.
    #[test]
    fn a_role_the_file_does_not_know_is_carried_not_refused() {
        let dir = project_dir("future_role");
        let path = write_file(&dir, "[roles]\nsome_future_role = \"a-model\"\n");
        let cfg = LeticodeConfig::load(&path).expect("a future role is Ok");
        assert_eq!(cfg.role_model("some_future_role"), Some("a-model"));
    }

    /// The subagent runs on the operator's measured parameters and the file does not have
    /// to say so, because a fix that only works in projects that knew about it is no fix.
    /// The answer carries samplers and no window: a child's wall is measured where it runs.
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

    /// A role's table sets its own numbers over the operator's, key by key: `[roles.subagent]`
    /// is every spawn and `[roles.coder]` is coder spawns. A file that changes `temperature`
    /// alone must not lose `presence_penalty`, which is the key they named as the cure.
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

    /// The model a spawn runs on, and where each name comes from: `subagent_model` is the
    /// file's word for every child, a `[roles.X] model` is its word for one seat's children
    /// and wins, and a file that set neither answers `None`.
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

    /// One seat, two answers, refused by name: the guard and the adjudicator are the single
    /// oracle in this build, so `gatekeeper_model` and `judge_model` are two words for one
    /// setting. Two different models is the file disagreeing with itself about who guards.
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

    /// `[exec]` sets the threshold and the switch; absent is on at 60 s; a zero or unknown key
    /// refuses the file.
    #[test]
    fn exec_keys_set_the_foreground_policy() {
        let dir = project_dir("exec_keys");
        let none = LeticodeConfig::default();
        let p = foreground_policy(&none, &dir, true);
        assert!(p.auto_background);
        assert_eq!(p.threshold, std::time::Duration::from_secs(60));
        write_file(
            &dir,
            "[exec]\nauto_background = false\nforeground_seconds = 7\n",
        );
        let p = foreground_policy(&none, &dir, true);
        assert!(!p.auto_background);
        assert_eq!(p.threshold, std::time::Duration::from_secs(7));
        assert!(foreground_policy(&none, &dir, false).auto_background);
        let path = write_file(&dir, "[exec]\nforeground_seconds = 0\n");
        assert!(LeticodeConfig::load(&path).is_err());
        let path = write_file(&dir, "[exec]\nauto_bg = true\n");
        assert!(LeticodeConfig::load(&path).unwrap_err().contains("[exec]"));
    }

    /// The project's `[memory] default` beats the box-wide one for a session in it, and the
    /// policy names the project file as the source, which is what the `bash` description
    /// then says. A session elsewhere keeps the box's number.
    #[test]
    fn a_projects_memory_default_overrides_the_box_wide_one_and_says_so() {
        use letibot_tools::exec::memory::{Amount, MemoryPolicy, PolicySource};
        const GIB: u64 = 1 << 30;
        let dir = project_dir("memory-default");
        write_file(&dir, "[memory]\ndefault = \"4G\"\n");
        let sub = dir.join("crates/x");
        std::fs::create_dir_all(&sub).unwrap();
        let box_wide = MemoryPolicy {
            default: Amount::Bytes(64 << 20),
            default_from: PolicySource::Box,
            ..MemoryPolicy::default()
        };
        let none = LeticodeConfig::default();
        let p = memory_policy_at(box_wide, &none, &sub, true, Some(100 * GIB));
        assert_eq!(p.default, Amount::Bytes(4 * GIB));
        assert_eq!(p.default_from, PolicySource::Project);
        assert_eq!(p.ceiling, Amount::Fraction(0.5), "the ceiling was not set");
        // `--no-project-config`, and a workspace with no file: the box's number, named so.
        let off = memory_policy_at(box_wide, &none, &sub, false, Some(100 * GIB));
        assert_eq!(off, box_wide);
        // And the user level sits between the two: the project still wins.
        let user = LeticodeConfig {
            memory: MemoryKeys {
                default: Some(Amount::Bytes(GIB)),
                ceiling: None,
            },
            path: Some(PathBuf::from("/h/.config/letibot/leticode.toml")),
            level: Level::User,
            ..Default::default()
        };
        let u = memory_policy_at(box_wide, &user, &sub, false, Some(100 * GIB));
        assert_eq!(
            (u.default, u.default_from),
            (Amount::Bytes(GIB), PolicySource::User)
        );
        let both = memory_policy_at(box_wide, &user, &sub, true, Some(100 * GIB));
        assert_eq!(both.default_from, PolicySource::Project);
    }

    /// A file may narrow the ceiling and may not raise it, and its default may not pass
    /// it: the ceiling is where the operator is asked, and the file is one a session can write.
    #[test]
    fn a_project_file_may_narrow_the_memory_ceiling_and_not_raise_it() {
        use letibot_tools::exec::memory::{Amount, MemoryPolicy};
        const GIB: u64 = 1 << 30;
        let none = LeticodeConfig::default();
        let dir = project_dir("memory-raise");
        write_file(&dir, "[memory]\nceiling = \"90%\"\ndefault = \"80G\"\n");
        let p = memory_policy_at(MemoryPolicy::default(), &none, &dir, true, Some(100 * GIB));
        assert_eq!(
            p,
            MemoryPolicy::default(),
            "neither the raise nor the default past it"
        );
        let dir = project_dir("memory-narrow");
        write_file(&dir, "[memory]\nceiling = \"8G\"\ndefault = \"2G\"\n");
        let p = memory_policy_at(MemoryPolicy::default(), &none, &dir, true, Some(100 * GIB));
        assert_eq!(p.ceiling, Amount::Bytes(8 * GIB));
        assert_eq!(p.default, Amount::Bytes(2 * GIB));
    }

    /// A `[memory]` key this build does not know, or a value that is not an amount, refuses
    /// the file by name - the loud failure every other key here gets.
    #[test]
    fn a_bad_memory_table_is_refused_by_name() {
        let dir = project_dir("memory-bad");
        let path = write_file(&dir, "[memory]\ndefualt = \"4G\"\n");
        let err = LeticodeConfig::load(&path).unwrap_err();
        assert!(err.contains("defualt") && err.contains("[memory]"), "{err}");
        let path = write_file(&dir, "[memory]\ndefault = \"lots\"\n");
        let err = LeticodeConfig::load(&path).unwrap_err();
        assert!(
            err.contains("[memory] default") && err.contains("lots"),
            "{err}"
        );
    }

    /// `[skills] pin` is read in the order the file wrote it, and one key this build does
    /// not know is refused by name.
    #[test]
    fn the_skills_table_is_read_and_its_one_key_is_named() {
        let dir = project_dir("skills");
        let path = write_file(
            &dir,
            "[skills]\npin = [\"latex-presentation\", \"release\"]\n",
        );
        let cfg = LeticodeConfig::load(&path).expect("one key parses");
        assert_eq!(
            cfg.skills.pin,
            ["latex-presentation", "release"],
            "the names, in the file's order"
        );
        assert!(
            LeticodeConfig::default().skills.is_empty(),
            "a file that says nothing pins nothing"
        );

        let path = write_file(&dir, "[skills]\npi = [\"x\"]\n");
        let err = LeticodeConfig::load(&path).unwrap_err();
        assert!(err.contains("`pi`") && err.contains("[skills]"), "{err}");
    }

    /// Both levels' pins apply: the user's names first, then the project's new ones, each
    /// once. A pin asks for a skill, so a project cannot unsay the operator's list.
    #[test]
    fn the_two_levels_pin_skills_by_union() {
        let user_dir = project_dir("skills-user");
        let user_path = write_file(&user_dir, "[skills]\npin = [\"a\", \"b\"]\n");
        let user = LeticodeConfig::load_as(&user_path, Level::User).expect("the user file parses");
        let project_root = project_dir("skills-project");
        let project_path = write_file(&project_root, "[skills]\npin = [\"b\", \"c\"]\n");
        let project = LeticodeConfig::load(&project_path).expect("the project file parses");
        assert_eq!(
            user.overridden_by(&project).skills.pin,
            ["a", "b", "c"],
            "the union, each name once, the user's order first"
        );
    }

    /// `[github] post` needs its token and nothing is posted without both, and a project file
    /// may stop the seat and may not start it: a repository a session can write must not begin
    /// posting under the operator's identity.
    #[test]
    fn the_github_table_reads_both_keys_and_a_project_file_cannot_start_posting() {
        let dir = project_dir("github");
        let path = write_file(
            &dir,
            "[github]\npost = true\ntoken_path = \"~/.config/letibot/github.token\"\n",
        );
        let cfg = LeticodeConfig::load(&path).expect("both keys parse");
        assert!(cfg.github.posting());
        assert_eq!(
            cfg.github.token_path.as_deref(),
            Some("~/.config/letibot/github.token")
        );
        // Neither key alone is posting.
        assert!(!GithubKeys::default().posting());
        assert!(
            !GithubKeys {
                post: Some(true),
                token_path: None
            }
            .posting()
        );
        assert!(
            !GithubKeys {
                post: None,
                token_path: Some("/t".into())
            }
            .posting()
        );

        // The user's yes, then a project's no: the seat stops. A project's yes alone is nothing.
        let user = GithubKeys {
            post: Some(true),
            token_path: Some("/t".into()),
        };
        let project_off = GithubKeys {
            post: Some(false),
            token_path: None,
        };
        assert!(!user.overridden_by(&project_off).posting());
        assert!(user.overridden_by(&GithubKeys::default()).posting());
        let project_on = GithubKeys {
            post: Some(true),
            token_path: Some("/other".into()),
        };
        assert!(!GithubKeys::default().overridden_by(&project_on).posting());

        // The two facts may not come from two levels: a project that names only a token has
        // started the seat the user merely allowed.
        let user_yes = GithubKeys {
            post: Some(true),
            token_path: None,
        };
        let project_token = GithubKeys {
            post: None,
            token_path: Some("/other".into()),
        };
        assert!(!user_yes.overridden_by(&project_token).posting());
        assert_eq!(
            user.overridden_by(&project_token).token_path.as_deref(),
            Some("/t"),
            "a project names no token, so it can neither add one nor replace the user's"
        );

        let path = write_file(&dir, "[github]\npostt = true\n");
        let err = LeticodeConfig::load(&path).unwrap_err();
        assert!(err.contains("postt") && err.contains("[github]"), "{err}");
    }

    /// `[boundary]` is read with the file and line of each entry, a project adds to the user's
    /// and a misshapen key is refused with its line rather than defaulted.
    #[test]
    fn the_boundary_table_keeps_its_lines_and_a_project_adds_to_the_users() {
        let dir = project_dir("boundary");
        let path = write_file(
            &dir,
            "main_model = \"x\"\n[boundary]\ngrants_ro = [\"~/a\", \"~/b\"]\nhide = [\"~/c\"]\n\
             [boundary.env]\nRUST_LOG = \"info\"\n",
        );
        let user = LeticodeConfig::load_as(&path, Level::User).expect("parses");
        let ro: Vec<(&str, usize)> = user
            .boundary
            .grants_ro
            .iter()
            .map(|e| (e.value.as_str(), e.from.line))
            .collect();
        assert_eq!(ro, vec![("~/a", 3), ("~/b", 3)]);
        assert!(user.boundary.grants_ro[0].from.user);
        assert_eq!(user.boundary.env[0].value.0, "RUST_LOG");
        assert_eq!(user.boundary.env[0].from.line, 6);

        let dir = project_dir("boundary-project");
        let path = write_file(&dir, "[boundary]\ngrants_ro = [\"~/b\", \"~/d\"]\n");
        let project = LeticodeConfig::load(&path).expect("parses");
        assert!(!project.boundary.grants_ro[0].from.user);
        let merged = user.overridden_by(&project);
        let all: Vec<&str> = merged
            .boundary
            .grants_ro
            .iter()
            .map(|e| e.value.as_str())
            .collect();
        assert_eq!(all, vec!["~/a", "~/b", "~/d"]);
        assert!(LeticodeConfig::default().boundary.is_empty());
    }

    /// `firecode_only` is a project key: it narrows what a session runs, and the levels union.
    #[test]
    fn firecode_only_is_read_from_a_project_and_unions_with_the_users() {
        let dir = project_dir("boundary-fconly");
        let path = write_file(&dir, "[boundary]\nfirecode_only = [\"npm\", \"node\"]\n");
        let project = LeticodeConfig::load(&path).expect("a project may set it");
        assert_eq!(project.boundary.firecode_only[0].value, "npm");
        assert_eq!(project.boundary.firecode_only[1].from.line, 2);
        let dir = project_dir("boundary-fconly-user");
        let path = write_file(&dir, "[boundary]\nfirecode_only = [\"node\", \"yarn\"]\n");
        let user = LeticodeConfig::load_as(&path, Level::User).expect("parses");
        let merged = user.boundary.overridden_by(&project.boundary);
        let all: Vec<&str> = merged
            .firecode_only
            .iter()
            .map(|e| e.value.as_str())
            .collect();
        assert_eq!(all, vec!["node", "yarn", "npm"]);
        let bad = write_file(
            &project_dir("boundary-fconly-bad"),
            "[boundary]\nfirecode_only = [\"/bin/npm\"]\n",
        );
        let err = LeticodeConfig::load(&bad).expect_err("a path is not a program name");
        assert!(err.contains("firecode_only") && err.contains(":2"), "{err}");
    }

    /// A file that does not parse as a boundary is a refusal with file and line, never an
    /// empty config: an empty one is today's wide default.
    #[test]
    fn a_bad_boundary_table_is_refused_with_its_line() {
        let dir = project_dir("boundary-bad");
        for (body, want) in [
            ("[boundary]\ngrants = [\"~/a\"]\n", "`grants` is not a key"),
            ("[boundary]\ngrants_ro = \"~/a\"\n", "grants_ro is a list"),
            ("[boundary]\nhide = [1]\n", "hide is a list"),
            ("[boundary]\nenv = { A = 1 }\n", "env.A is a string"),
            ("[boundary\nhide = []\n", "line 1"),
        ] {
            let path = write_file(&dir, body);
            let err = LeticodeConfig::load(&path).unwrap_err();
            assert!(err.contains(want), "{body:?}: {err}");
            assert!(err.contains(&path.display().to_string()), "{err}");
        }
        let path = write_file(&dir, "[boundary]\nhide = []\ngrants = []\n");
        let err = LeticodeConfig::load(&path).unwrap_err();
        assert!(err.contains(":3:"), "{err}");
    }

    /// `[perimeter.<name>]` is read at both levels with each entry's file and line, the project's
    /// table of one name adding to the user's; the three ways an env value is computed survive.
    #[test]
    fn the_perimeter_tables_keep_their_lines_and_a_project_adds_per_name() {
        let dir = project_dir("perimeter");
        let user_path = dir.join("user.toml");
        std::fs::write(
            &user_path,
            "[perimeter.subagent]\nhide = [\"~/models\"]\nsecrets = [\"github\"]\n\
             [perimeter.subagent.env]\nRUST_LOG = \"warn\"\nTOKEN = { file = \"~/tok\" }\n\
             STAMP = { command = \"date +%F\" }\n\
             [perimeter.reviewer]\ngrants_ro = [\"~/reviews\"]\n",
        )
        .unwrap();
        let user = LeticodeConfig::load_as(&user_path, Level::User).unwrap();
        let sub = user.perimeters.get("subagent").expect("the subagent table");
        assert_eq!(sub.hide[0].value, "~/models");
        assert!(sub.hide[0].from.user);
        assert!(sub.hide[0].from.line >= 2);
        assert_eq!(sub.secrets[0].value, Secret::Github);
        assert!(
            sub.env.iter().any(|e| e.value.0 == "TOKEN"
                && matches!(e.value.1, EnvValue::File(ref f) if f == "~/tok"))
        );
        assert!(sub.env.iter().any(|e| e.value.0 == "STAMP"
            && matches!(e.value.1, EnvValue::Command(ref c) if c == "date +%F")));
        assert!(sub.env.iter().any(|e| e.value.0 == "RUST_LOG"
            && matches!(e.value.1, EnvValue::Constant(ref v) if v == "warn")));
        assert_eq!(user.perimeters["reviewer"].grants_ro[0].value, "~/reviews");

        // The project's table of one name adds to the user's, and a name only the user file has
        // survives the merge untouched.
        let project_path = write_file(&dir, "[perimeter.subagent]\nhide = [\"/opt/x\"]\n");
        let project = LeticodeConfig::load(&project_path).unwrap();
        assert!(!project.perimeters["subagent"].hide[0].from.user);
        let merged = user.overridden_by(&project);
        let hides: Vec<&str> = merged.perimeters["subagent"]
            .hide
            .iter()
            .map(|e| e.value.as_str())
            .collect();
        assert!(
            hides.contains(&"~/models") && hides.contains(&"/opt/x"),
            "{hides:?}"
        );
        assert!(merged.perimeters.contains_key("reviewer"));
        assert!(LeticodeConfig::default().perimeters.is_empty());
    }

    /// A perimeter table that is misshapen is refused with its file and line, naming the key -
    /// never silently read as the empty table, which would be a view nobody asked for.
    #[test]
    fn a_bad_perimeter_table_is_refused_with_its_line() {
        let dir = project_dir("perimeter-bad");
        for (body, want) in [
            (
                "[perimeter]\nsubagent = 3\n",
                "[perimeter.subagent] is a table",
            ),
            ("[perimeter.subagent]\nsize = 3\n", "`size` is not a key"),
            ("[perimeter.subagent]\nhide = \"~/a\"\n", "hide is a list"),
            (
                "[perimeter.subagent]\nsecrets = [\"ssh\"]\n",
                "not a secret this build knows",
            ),
            (
                "[perimeter.subagent]\n[perimeter.subagent.env]\nA = { file = 3 }\n",
                "env.A is a string",
            ),
            (
                "[perimeter.subagent]\n[perimeter.subagent.env]\nA = { file = \"x\", command = \"y\" }\n",
                "env.A is a string",
            ),
        ] {
            let path = write_file(&dir, body);
            let err = LeticodeConfig::load(&path).unwrap_err();
            assert!(err.contains(want), "{body:?}: {err}");
            assert!(err.contains(&path.display().to_string()), "{err}");
        }
    }

    /// A fallback list is replaced, not grown. A project that names two models must not
    /// silently inherit a third from the user file: the list is one decision, not a set.
    #[test]
    fn a_project_fallback_list_replaces_the_users_rather_than_growing_it() {
        let dir = project_dir("fallback_merge");
        let user_path = dir.join("user.toml");
        std::fs::write(
            &user_path,
            "fallback = [\"local\", \"qwen-3.8-27b\"]\nfallback_after_timeouts = 5\n",
        )
        .unwrap();
        let project_path = dir.join("project.toml");
        std::fs::write(&project_path, "fallback = [\"deepseek\"]\n").unwrap();
        let user = LeticodeConfig::load_as(&user_path, Level::User).unwrap();
        let project = LeticodeConfig::load(&project_path).unwrap();
        let merged = user.overridden_by(&project);
        assert_eq!(
            merged.fallback,
            ["deepseek"],
            "the project's list replaced the user's whole list: {:?}",
            merged.fallback
        );
        assert_eq!(
            merged.fallback_after_timeouts,
            Some(5),
            "a key only the user file set survives the merge"
        );

        // A project that says nothing about a fallback keeps the user's list, and one
        // that names none is no fallback at all rather than the level above.
        let silent = dir.join("silent.toml");
        std::fs::write(&silent, "main_model = \"local\"\n").unwrap();
        let merged = user.overridden_by(&LeticodeConfig::load(&silent).unwrap());
        assert_eq!(merged.fallback, ["local", "qwen-3.8-27b"]);
        assert!(LeticodeConfig::default().fallback.is_empty());
        assert_eq!(LeticodeConfig::default().fallback_after_timeouts, None);
    }

    /// A budget of zero is refused by name: a move on the first timeout is `1`, and a file
    /// that wrote `0` said something it did not mean.
    #[test]
    fn a_zero_timeout_budget_is_refused_by_name() {
        let dir = project_dir("fallback_zero");
        let path = write_file(&dir, "fallback_after_timeouts = 0\n");
        let err = LeticodeConfig::load(&path).expect_err("zero is refused");
        assert!(
            err.contains("fallback_after_timeouts"),
            "the key is named: {err}"
        );
        assert!(err.contains("`1`"), "and the number it meant: {err}");
        assert!(
            err.contains(&path.display().to_string()),
            "and the file: {err}"
        );
    }

    /// A fallback that is not a list of model names is refused by name with the file: a
    /// string, a number in the list and an empty name are three different mistakes.
    #[test]
    fn a_fallback_that_is_not_a_list_of_model_names_is_refused_by_name() {
        let dir = project_dir("fallback_bad");
        for (text, wanted) in [
            ("fallback = \"deepseek\"\n", "not a list"),
            ("fallback = [\"deepseek\", 3]\n", "a whole number"),
            ("fallback = [\"\"]\n", "an empty name"),
        ] {
            let path = write_file(&dir, text);
            let err = LeticodeConfig::load(&path).expect_err("a bad list is Err");
            assert!(err.contains("fallback"), "the key is named: {err}");
            assert!(err.contains(wanted), "{wanted} must be said: {err}");
            assert!(
                err.contains(&path.display().to_string()),
                "and the file: {err}"
            );
        }
    }

    /// A budget that is not a whole number of timeouts is refused by name, so a negative or
    /// a word never reaches the move as some number nobody wrote.
    #[test]
    fn a_timeout_budget_that_is_not_a_whole_number_is_refused_by_name() {
        let dir = project_dir("fallback_budget_bad");
        for (text, wanted) in [
            ("fallback_after_timeouts = -3\n", "-3"),
            ("fallback_after_timeouts = \"three\"\n", "a string"),
            ("fallback_after_timeouts = 3.5\n", "a number"),
        ] {
            let path = write_file(&dir, text);
            let err = LeticodeConfig::load(&path).expect_err("a bad budget is Err");
            assert!(
                err.contains("fallback_after_timeouts"),
                "the key is named: {err}"
            );
            assert!(err.contains(wanted), "{wanted} must be said: {err}");
        }
    }

    /// The fallback names are models the file set, so the disclosure says them in its own
    /// style: where the model that answers after a timeout came from must be answerable.
    #[test]
    fn the_fallback_models_are_disclosed_as_fallback_lines() {
        let dir = project_dir("fallback_disclosure");
        let path = write_file(
            &dir,
            "fallback = [\"deepseek\", \"glm-coding/glm-5.3\"]\nfallback_after_timeouts = 3\n",
        );
        let cfg = LeticodeConfig::load(&path).expect("a fallback list is Ok");
        assert_eq!(cfg.fallback, ["deepseek", "glm-coding/glm-5.3"]);
        assert_eq!(cfg.fallback_after_timeouts, Some(3));
        let set = cfg.set_models();
        assert_eq!(
            set,
            ["fallback deepseek", "fallback glm-coding/glm-5.3"],
            "the list, in the order the file wrote it: {set:?}"
        );
        assert!(LeticodeConfig::default().set_models().is_empty());
    }

    /// The operator's ruling, in the file: what a session does when its provider puts a time on a
    /// limit. `switch` when a fallback list exists, `wait` when nothing is set, and the file's own
    /// word over both.
    #[test]
    fn the_provider_table_says_what_a_limit_does_and_its_defaults_are_the_rulings() {
        let dir = project_dir("provider_on_limit");
        let written = write_file(
            &dir,
            "[provider]\non_limit = \"switch\"\nfallback = [\"deepseek\", \"glm-coding/glm-5.3\"]\n",
        );
        let cfg = LeticodeConfig::load(&written).expect("[provider] is read");
        assert_eq!(cfg.provider.on_limit, Some(OnLimit::Switch));
        assert_eq!(
            cfg.fallback,
            ["deepseek", "glm-coding/glm-5.3"],
            "[provider] fallback is the one list, so the daemon's fallback reads it"
        );
        assert_eq!(cfg.limit_policy(), OnLimit::Switch);
        assert!(
            cfg.set_models().contains(&"on_limit switch".to_string()),
            "the disclosure says what the file decided: {:?}",
            cfg.set_models()
        );

        // The two defaults, and the file's word beats both.
        assert_eq!(LeticodeConfig::default().limit_policy(), OnLimit::Wait);
        let list_only = write_file(&sub(&dir, "list"), "fallback = [\"deepseek\"]\n");
        let cfg = LeticodeConfig::load(&list_only).unwrap();
        assert_eq!(cfg.limit_policy(), OnLimit::Switch, "a list means switch");
        let wait = write_file(
            &sub(&dir, "wait"),
            "[provider]\non_limit = \"wait\"\nfallback = [\"deepseek\"]\n",
        );
        assert_eq!(
            LeticodeConfig::load(&wait).unwrap().limit_policy(),
            OnLimit::Wait,
            "a file that says wait keeps the list and waits"
        );
    }

    /// A `[provider]` table is refused by name when it is malformed, and the two spellings of one
    /// list are refused when they disagree.
    #[test]
    fn a_bad_provider_table_and_two_disagreeing_fallbacks_are_refused_by_name() {
        let dir = project_dir("provider_refusals");
        let bad = write_file(&sub(&dir, "bad"), "[provider]\non_limit = \"sleep\"\n");
        let err = LeticodeConfig::load(&bad).expect_err("sleep is not an answer");
        assert!(err.contains("on_limit"), "{err}");
        assert!(err.contains("wait"), "the two answers are named: {err}");

        let unknown = write_file(&sub(&dir, "unknown"), "[provider]\nfall_back = []\n");
        let err = LeticodeConfig::load(&unknown).expect_err("an unknown key");
        assert!(err.contains("provider"), "{err}");

        let two = write_file(
            &sub(&dir, "two"),
            "fallback = [\"deepseek\"]\n[provider]\nfallback = [\"glm\"]\n",
        );
        let err = LeticodeConfig::load(&two).expect_err("one list, two answers");
        assert!(err.contains("`[provider] fallback`"), "{err}");
        let same = write_file(
            &sub(&dir, "same"),
            "fallback = [\"deepseek\"]\n[provider]\nfallback = [\"deepseek\"]\n",
        );
        assert_eq!(
            LeticodeConfig::load(&same).unwrap().fallback,
            ["deepseek"],
            "the same list twice is one list"
        );
    }

    /// The deadline keys parse, resolve model over preset over table over built-in, per key, and
    /// the project's key beats the user's.
    #[test]
    fn provider_deadlines_resolve_by_model_then_preset_then_table_then_builtin() {
        let dir = project_dir("deadlines");
        let none = LeticodeConfig::default().deadlines("glm", Some("glm-5.3"));
        assert_eq!(
            (
                none.slow_notice.as_secs(),
                none.first_byte.as_secs(),
                none.answer.as_secs()
            ),
            (15, 60, 600)
        );
        let user = write_file(
            &sub(&dir, "user"),
            "[provider]\nslow_notice_seconds = 20\nanswer_seconds = 900\n\
             [provider.glm]\nslow_notice_seconds = 90\nfirst_byte_seconds = 100\n\
             [provider.\"glm/glm-5.3\"]\nfirst_byte_seconds = 120\n",
        );
        let project = write_file(
            &sub(&dir, "project"),
            "[provider.glm]\nslow_notice_seconds = 45\n",
        );
        let merged = LeticodeConfig::load_as(&user, Level::User)
            .unwrap()
            .overridden_by(&LeticodeConfig::load(&project).unwrap());
        let d = |name, model| {
            let d = merged.deadlines(name, model);
            (
                d.slow_notice.as_secs(),
                d.first_byte.as_secs(),
                d.answer.as_secs(),
            )
        };
        assert_eq!(
            d("glm", Some("glm-5.3")),
            (45, 120, 900),
            "model, preset, table"
        );
        assert_eq!(d("glm", Some("other")), (45, 100, 900), "the preset only");
        assert_eq!(
            d("deepseek", None),
            (20, 60, 900),
            "the table, then built-in"
        );
    }

    /// Nonsense deadlines are refused naming file, line and key.
    #[test]
    fn nonsense_deadlines_are_refused_with_file_line_and_key() {
        let dir = project_dir("deadline_refusals");
        for (tag, body, want, line) in [
            (
                "zero",
                "\n[provider]\nslow_notice_seconds = 0\n",
                "slow_notice_seconds",
                3,
            ),
            (
                "crossed",
                "\n[provider]\nfirst_byte_seconds = 90\nanswer_seconds = 30\n",
                "first_byte_seconds",
                3,
            ),
            (
                "preset_zero",
                "\n[provider.glm]\nanswer_seconds = 0\n",
                "[provider.glm] answer_seconds",
                3,
            ),
            (
                "preset_key",
                "\n[provider.glm]\non_limit = \"wait\"\n",
                "on_limit",
                3,
            ),
        ] {
            let path = write_file(&sub(&dir, tag), body);
            let err = LeticodeConfig::load(&path).expect_err(tag);
            assert!(
                err.contains(&format!("leticode.toml:{line}:")),
                "{tag}: {err}"
            );
            assert!(err.contains(want), "{tag}: {err}");
        }
    }

    // ---- K9: config, not a plugin (the plan's §4.3, §6 K9) ----

    /// The six tables read, every key, and a file that says nothing is the daemon that shipped:
    /// no `[nag]`/`[queue]` number at all, which is `NagTimings::default` and the queue's own
    /// constants. This is the "byte-identical at the defaults" half of K9's target.
    #[test]
    fn the_six_tables_are_read_and_an_empty_file_is_the_shipped_daemon() {
        let dir = project_dir("k9_read");
        let empty = write_file(&sub(&dir, "empty"), "main_model = \"local\"\n");
        let none = LeticodeConfig::load(&empty).unwrap();
        assert_eq!(none.nag, NagKeys::default());
        assert_eq!(none.queue, QueueKeys::default());
        assert_eq!(none.room, RoomKeys::default());
        assert!(none.warnings.mute.is_empty());
        assert_eq!(none.warnings.coalesce, None);
        assert!(none.keys.rebind.is_empty());
        assert!(none.verbosity.profiles.is_empty());
        assert_eq!(
            none.nag_timings(),
            letibot_tools::builtins::todo::NagTimings::default(),
            "no key is the clock every daemon already had"
        );
        assert_eq!(none.stuck_after_seconds(), None);

        let full = write_file(
            &sub(&dir, "full"),
            "[queue]\nstuck_after_seconds = 900\nstuck_repeat_seconds = 7200\n\
             [nag]\nafter_seconds = 60\nfloor_seconds = 10\nrepeat_cap_seconds = 1800\n\
             [room]\ntie_breaker = \"glm-coding/glm-5.3\"\n\
             [warnings]\nmute = [\"model_slow_first_byte\", \"denied\"]\ncoalesce = false\n\
             [keys]\n\"todos.d\" = \"f\"\n\
             [verbosity.mine]\nshow = [\"edits\", \"thinking=folded\"]\n",
        );
        let c = LeticodeConfig::load(&full).unwrap();
        assert_eq!(c.queue.stuck_after_seconds, Some(900));
        assert_eq!(c.queue.stuck_repeat_seconds, Some(7200));
        assert_eq!(c.nag.after_seconds, Some(60));
        assert_eq!(c.nag.floor_seconds, Some(10));
        assert_eq!(c.nag.repeat_cap_seconds, Some(1800));
        assert_eq!(c.room.tie_breaker.as_deref(), Some("glm-coding/glm-5.3"));
        assert_eq!(
            c.warnings
                .mute
                .iter()
                .map(|e| e.value.as_str())
                .collect::<Vec<_>>(),
            ["model_slow_first_byte", "denied"]
        );
        assert_eq!(c.warnings.coalesce, Some(false));
        assert_eq!(c.keys.rebind.len(), 1);
        assert_eq!(c.keys.rebind[0].name, "todos.d");
        assert_eq!(c.keys.rebind[0].key, "f");
        assert_eq!(
            c.verbosity.profiles[0].name, "mine",
            "a profile is its name and the list of what it shows"
        );
        assert_eq!(c.verbosity.profiles[0].show, ["edits", "thinking=folded"]);

        // The clock's four, as the file and the shipped default make them: `[nag]`'s three and
        // the census's repeat from `[queue]`.
        let t = c.nag_timings();
        assert_eq!(t.window, std::time::Duration::from_secs(60));
        assert_eq!(t.floor, std::time::Duration::from_secs(10));
        assert_eq!(t.cap, std::time::Duration::from_secs(1800));
        assert_eq!(t.census_repeat, std::time::Duration::from_secs(7200));
        assert_eq!(c.stuck_after_seconds(), Some(900));
    }

    /// A bad value says its FILE AND LINE, table by table - the operator's target for K9, and the
    /// reason these six tables are walked by hand instead of by serde, which names the table.
    #[test]
    fn a_bad_value_in_every_table_is_refused_with_its_file_and_line() {
        let dir = project_dir("k9_lines");
        // The line a refusal names is the one the value was WRITTEN on: a table's key is on the
        // file's third line here, and a `[verbosity.<name>]` profile's own line is the second -
        // its `show` list is refused against the profile, which is what the operator edits.
        for (tag, body, want, line) in [
            (
                "nag",
                "\n[nag]\nafter_seconds = \"60\"\n",
                "[nag] after_seconds",
                3,
            ),
            (
                "nagzero",
                "\n[nag]\nfloor_seconds = 0\n",
                "at least one second",
                3,
            ),
            (
                "queue",
                "\n[queue]\nstuck_after_seconds = -2\n",
                "[queue] stuck_after_seconds",
                3,
            ),
            (
                "queue_kind",
                "\n[queue]\nstuck_repeat_seconds = true\n",
                "[queue]",
                3,
            ),
            (
                "coalesce",
                "\n[warnings]\ncoalesce = \"yes\"\n",
                "coalesce is true or false",
                3,
            ),
            ("mute", "\n[warnings]\nmute = \"a\"\n", "mute is a list", 3),
            ("keys", "\n[keys]\nd = \"f\"\n", "names no surface", 3),
            (
                "keys_value",
                "\n[keys]\n\"todos.d\" = 3\n",
                "`todos.d` is one key",
                3,
            ),
            (
                "profile",
                "\n[verbosity.mine]\nshow = \"tools\"\n",
                "show is a list",
                2,
            ),
            (
                "profile_key",
                "\n[verbosity.mine]\nhide = []\n",
                "`hide` is not a key",
                2,
            ),
            (
                "room",
                "\n[room]\ntie_breaker = 3\n",
                "tie_breaker is a model name",
                3,
            ),
            (
                "room_key",
                "\n[room]\nmodel = \"x\"\n",
                "`model` is not a key",
                3,
            ),
        ] {
            let path = write_file(&sub(&dir, tag), body);
            let err = LeticodeConfig::load(&path).expect_err(tag);
            assert!(
                err.contains(&format!("leticode.toml:{line}:")),
                "{tag}: the refusal names the file and the line: {err}"
            );
            assert!(err.contains(want), "{tag}: {err}");
        }
    }

    /// A floor above the ceiling is a ladder with no room in it, refused by name.
    #[test]
    fn a_nag_floor_above_the_repeat_cap_is_refused() {
        let dir = project_dir("k9_floor");
        let bad = write_file(
            &sub(&dir, "crossed"),
            "[nag]\nfloor_seconds = 3600\nrepeat_cap_seconds = 60\n",
        );
        let err = LeticodeConfig::load(&bad).expect_err("the two ends crossed");
        assert!(err.contains("floor_seconds"), "{err}");
        assert!(err.contains("repeat_cap_seconds"), "{err}");
    }

    /// The project's value wins over the user's, key by key - the target's third clause - and the
    /// two suppression lists (`mute`, and a profile of the same name) resolve as they should: a
    /// mute unions, because a project must not unsay what the operator muted for every project.
    #[test]
    fn the_projects_value_wins_and_a_mute_unions_across_the_levels() {
        let dir = project_dir("k9_levels");
        let user = write_file(
            &sub(&dir, "user"),
            "[nag]\nafter_seconds = 90\nrepeat_cap_seconds = 900\n\
             [queue]\nstuck_after_seconds = 600\n\
             [room]\ntie_breaker = \"deepseek/deepseek-flash\"\n\
             [warnings]\nmute = [\"model_slow_first_byte\"]\ncoalesce = true\n\
             [keys]\n\"todos.d\" = \"f\"\n\"rooms.r\" = \"u\"\n\
             [verbosity.mine]\nshow = [\"tools\"]\n",
        );
        let project = write_file(
            &sub(&dir, "project"),
            "[nag]\nafter_seconds = 30\n\
             [queue]\nstuck_after_seconds = 120\n\
             [room]\ntie_breaker = \"glm-coding/glm-5.3\"\n\
             [warnings]\nmute = [\"denied\"]\ncoalesce = false\n\
             [keys]\n\"todos.d\" = \"x\"\n\
             [verbosity.mine]\nshow = [\"edits\", \"tools\"]\n",
        );
        let merged = LeticodeConfig::load_as(&user, Level::User)
            .unwrap()
            .overridden_by(&LeticodeConfig::load(&project).unwrap());
        assert_eq!(
            merged.nag.after_seconds,
            Some(30),
            "the project's window, not the user's"
        );
        assert_eq!(
            merged.nag.repeat_cap_seconds,
            Some(900),
            "and the user's cap stays where the project said nothing"
        );
        assert_eq!(merged.queue.stuck_after_seconds, Some(120));
        assert_eq!(
            merged.room.tie_breaker.as_deref(),
            Some("glm-coding/glm-5.3")
        );
        assert_eq!(merged.warnings.coalesce, Some(false));
        assert_eq!(
            merged
                .warnings
                .mute
                .iter()
                .map(|e| e.value.as_str())
                .collect::<Vec<_>>(),
            ["model_slow_first_byte", "denied"],
            "both levels' mutes, the user's first"
        );
        assert_eq!(merged.keys.rebind.len(), 2, "one replaced, one kept");
        assert_eq!(
            merged
                .keys
                .rebind
                .iter()
                .find(|r| r.name == "todos.d")
                .map(|r| r.key.as_str()),
            Some("x"),
            "the project's rebinding of one key replaces the user's"
        );
        assert_eq!(
            merged
                .verbosity
                .profiles
                .iter()
                .find(|p| p.name == "mine")
                .map(|p| p.show.clone()),
            Some(vec!["edits".to_string(), "tools".to_string()]),
            "a project's profile of the same name replaces the user's"
        );
        // And the mute keeps the LINE it came from, because the head refuses an unknown code there.
        let denied = merged
            .warnings
            .mute
            .iter()
            .find(|e| e.value == "denied")
            .expect("the project's mute");
        assert_eq!(denied.from.file, project);
        assert_eq!(denied.from.line, 8, "the line the code is written on");
    }
}
