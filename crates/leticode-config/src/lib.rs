//! The `leticode.toml` levels: which models answer in this project, how its subagents sample,
//! and the same file once per user.
//!
//! Like `.env`, the project file is found by walking up from the session's workspace, so a
//! session started in a subdirectory still finds it. It is a visible file, not a dotfile: a file
//! that changes which model answers should not be a surprise.
//!
//! # The keys
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
//! # or, per model - the limited model's own key, then `provider/*`, then `*`:
//! # [fallback]
//! # "claude/opus-5-5" = ["kimi-code-global/k3.1-pro", "deepseek"]
//! # "kimi-code-global/*" = ["glm-coding/glm-5.3"]
//! # "*" = ["deepseek"]
//!
//! [roles]                                 # a named-role override: a future role
//! reviewer = "deepseek/deepseek-reasoner" # needs no new key, only a line here
//! ```
//!
//! `[project] repository = false` stops a folder that is not inside a git repository from taking
//! a child repository as its project (`workspace::adopts_children`). The home directory never does.
//!
//! A fallback list is replaced by the higher level, never joined to it, so a project that names
//! two models does not inherit a third. A `[fallback]` table replaces every key of the lower
//! level's, and `fallback = [...]` is its `"*"`. `fallback_after_timeouts = 0` is refused: a move
//! on the first timeout is written `1`.
//!
//! `[provider]` also holds the model API deadlines: `slow_notice_seconds` (15, the red "model
//! slow" notice), `first_byte_seconds` (60) and `answer_seconds` (600), as defaults for every
//! provider, and the same three keys in `[provider.glm]` (a preset) or `[provider."glm-coding/glm-5.3"]`
//! (one model) for a slow one. The model's table wins over the preset's, the preset's over
//! `[provider]`, that over the built-in number. See [`ProviderKeys`].
//!
//! An absent key means the daemon's own default, and the startup disclosure says which models
//! the file set.
//!
//! # Sampler parameters, and the wall they may not move
//!
//! A role sets sampler parameters and nothing that describes the weights or the window. The same
//! weights are served at different `n_ctx` across the fleet, so a window copied from one box onto
//! a child running on another is wrong. `[roles.coder] temperature = 0.7` is read, and
//! `window = 57344` or `max_tokens = 2048` is refused by name ([`WINDOW_AND_CAP_KEYS`]).
//!
//! The dense78 values are the standing subagent defaults ([`RoleSampling::dense78`]), whatever
//! model answers: the metered transport drops what its API cannot carry (`provider_sampling` in
//! `harness.rs`). A project that wants a cloud child sampled greedily writes `temperature = 0`,
//! `top_k = 1`.
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
//! # Two levels, one parser
//!
//! * the project's `leticode.toml`, found by walking up from the session's workspace;
//! * the user's `~/.config/letibot/leticode.toml`, at a fixed path beside `providers.toml`,
//!   `prompts.toml` and `head.toml`.
//!
//! [`LeticodeConfig::load_as`] serves both ([`load_user`] is that call at the fixed path, with
//! [`Level::User`]), so the two files cannot disagree about what a key means: same keys, same
//! refusals, and each entry carries the level that wrote it.
//!
//! # Why the user level is not `[default]` in `providers.toml`
//!
//! `[default]` is read into `cfg.provider` only when `cfg.provider.is_none()` (`cli.rs`), and the
//! launcher passes `--model` on every start, which leaves `cfg.provider` at `None`. So a
//! `[default]` outranked a command-line model and `letibot --glm` answered on deepseek. This file
//! sits above `[default]`, so a line here outranks it.
//!
//! `[default]` also holds only a preset name and a model id on that preset, so it cannot name a
//! local alias like `glm-5.3-flash`. `main_model` takes both `provider/model` for a metered
//! provider and a bare alias for the local server.
//!
//! # The precedence
//!
//! Command line beats the project file, the project file beats the user file, the user file
//! beats the built-in default, and an unset key falls through. [`precedence`] is the one place
//! that rule lives.
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
//! `--model` alone is not the flag level: `scripts/letibot` passes it on every start as the local
//! alias the daemon binds, so treating it as a choice would make both files unreachable for every
//! launcher-started daemon. `--provider` says who answers and beats everything below it.
//!
//! A file's bare alias does not overwrite the alias `--model` bound, because the daemon tokenises
//! with its own GGUF and a mismatch is `400 Prompt contains invalid tokens` at the first turn. The
//! file's alias still clears the provider, and a disagreement is named at startup (see
//! `apply_main_model` in `cli.rs`).
//!
//! # What the file may not do
//!
//! Models are configuration; permissions are not. A file may set which model answers a role, and
//! may not seat a tool on a role or lift an access narrowing. [`LeticodeConfigFile`] has
//! `deny_unknown_fields`, so a typo'd `subagent_modle` or an `allow_bash = true` is refused by
//! name, not ignored.
//!
//! # Config, not a plugin: the numbers and the head's keys
//!
//! A fixed number or a list of names is a key here, not an extension point: a layer for them
//! would cost a build and a pin to change a number. Each table's default is today's value, so a
//! file that says nothing is the daemon that shipped:
//!
//! ```toml
//! [queue]
//! stuck_after_seconds = 900     # a row stuck this long is the census's (`Stuck::is_news`)
//! stuck_repeat_seconds = 7200   # and the census is said again this often
//! mute = ["stroppy-rust"]       # repositories of a folder project this session is not told about
//!
//! [nag]
//! after_seconds = 60            # the idle window before the plan-check (TODO_NAG_AFTER)
//! floor_seconds = 10            # the shortest re-arm an operator add earns (OPERATOR_ADD_NAG_AFTER)
//! repeat_cap_seconds = 1800     # the ceiling the doubled interval retreats to (NAG_REPEAT_CAP)
//!
//! [ui]
//! rooms_sidebar = false                 # Rooms, Sessions and Queues beside the transcript
//! rooms_sidebar_width = 40              # its starting width; a dragged width is kept in head.toml
//!
//! [room]
//! tie_breaker = "deepseek/deepseek-flash"   # the model that arbitrates the room's items
//!
//! [warnings]
//! mute = ["model_slow_first_byte"]  # codes the head files and never draws
//!
//! [keys]
//! "todos.d" = "f"                   # SURFACE.KEY as the bar spells them, to one other key
//!
//! [verbosity.mine]                  # a profile IS the list of things it shows
//! show = ["edits", "tools", "thinking=folded"]
//! ```
//!
//! The project's value wins over the user's, key by key. The suppression lists (`mute`) union
//! instead, `[skills] pin`'s rule: a project file is writable by the sessions it governs and must
//! not unsay what the operator muted for every project. A bad value is refused with its file and
//! line: `[nag] after_seconds = "60"` is an error, not a minute.
//!
//! `[queue] mute` names repositories of a one-org folder that the session is not woken for: a
//! name matches a repository by its directory name under the folder (`stroppy-rust`) or by its
//! repository key (the path `git::repository_key` gives). The queue still gates and lands a muted
//! repository and the pane still shows it.
//!
//! The head's three tables are read here and resolved in the head (`tui/src/prefs.rs`,
//! `visibility.rs` and `keys/bindings.rs`), because the surface names, warning codes and
//! verbosity switches are that process's vocabulary.
//!
//! # Failures are loud
//!
//! * a file that will not parse names the file, the line and what the parser expected;
//! * a key the version does not know is named, at the top level or inside a role's table;
//! * on success, the startup disclosure says which models and parameters the project file set.
//!
//! A file that does not parse does not take the session down, at either level: the daemon runs
//! on its own defaults and says so, as `prompts.toml` does.
//!
//! # The seat's instruction file
//!
//! The same layer decides which markdown a leticode reads as its instructions:
//! [`INSTRUCTION_FILE`], or [`CANONICAL_INSTRUCTION_FILE`] when a project wrote that one instead.
//! The two names live in the `instructions` module and nowhere in `letibot-harnessd`, which keeps
//! letibot a runtime and leticode the thing that reads.

use letibot_tools::exec::boundary::{BoundaryConfig, Entry, Origin};
use letibot_tools::exec::perimeter::{EnvValue, PerimeterTable, Secret};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub mod instructions;
pub mod workspace;

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

/// Load the user level at `user_path`. One parser, two levels: the level is what every entry's
/// origin carries, and a grant is only honoured from the file that says `user`. A missing file is
/// `Ok` with no models, so a box where nobody wrote one runs on the daemon's own answer.
pub fn load_user() -> Result<LeticodeConfig, String> {
    LeticodeConfig::load_as(&user_path(), Level::User)
}

/// Which level named the model that answers - the word the disclosure puts on
/// the screen, so where a model came from is readable rather than findable.
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

/// The sampler keys a role's table may set, in the order the dense78 profile writes them. The same five names `providers.toml`'s
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
    /// The standing numbers for a subagent, from `dense78`: a project with no `leticode.toml`
    /// still gets children that do not overthink, and one can override them key by key.
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
    /// `[fallback]` keys other than `"*"`: a model in `/models` spelling, or `provider/*`. See
    /// [`fallback_for`].
    pub fallback_for: BTreeMap<String, Vec<String>>,
    /// How many consecutive model timeouts earn that move. `None` is the daemon's own
    /// number, and `0` never reaches here: it is refused at the parser by name.
    pub fallback_after_timeouts: Option<u32>,
    /// The `[notes]` table: where the notes checkout is and the remote it pushes to.
    pub notes: NotesKeys,
    /// The `[browser]` table: the port and extension `letibot browser` serves.
    pub browser: BrowserKeys,
    /// The `[exec]` table: the foreground threshold and whether passing it backgrounds.
    pub exec: ExecKeys,
    /// The `[output]` table: the inline cap every tool result falls under. See [`output_cap`].
    pub output: OutputKeys,
    /// The `[mcp.NAME]` tables: MCP servers a session reaches through `mcp_search` and
    /// `mcp_call`. Read at both levels; only the user file's start. See [`McpServerKeys`].
    pub mcp: BTreeMap<String, McpServerKeys>,
    /// `[mcp_projects] trusted`: directories whose checkouts' `.mcp.json` the warning offers to
    /// pin. Starts nothing on its own. User file only.
    pub mcp_trusted: Vec<PathBuf>,
    /// `[mcp_projects] pinned`: a `.mcp.json` path and the sha256 its servers start at. User
    /// file only. See [`mcp_pin`].
    pub mcp_pinned: BTreeMap<PathBuf, String>,
    /// The `[provider]` table: what a session does when its provider says the model is limited.
    /// See [`ProviderKeys`] and [`LeticodeConfig::limit_policy`].
    pub provider: ProviderKeys,
    /// The `[nag]` table: the idle plan-check's window, floor and ceiling. `None`s are the
    /// clock's own numbers; see [`NagKeys`] and [`LeticodeConfig::nag_timings`].
    pub nag: NagKeys,
    /// The `[automode]` table: what an unattended session does with a call that would be a card,
    /// and the two block counts. See [`AutomodeKeys`].
    pub automode: AutomodeKeys,
    /// The `[queue]` table's two census numbers. See [`QueueKeys`].
    pub queue: QueueKeys,
    /// The `[room]` table: the model the room's tie-breaker arbitrates on.
    pub room: RoomKeys,
    /// The `[peering]` table: whether this box peers with the other boxes on its LAN, and whether
    /// this project is introduced to the same repository elsewhere. See [`PeeringKeys`].
    pub peering: PeeringKeys,
    /// The `[tools]` table: how one round's calls run. See [`ToolKeys`].
    pub tools: ToolKeys,
    /// The `[ui]` table: head layout switches. See [`UiKeys`].
    pub ui: UiKeys,
    /// The `[sessions]` table. See [`SessionKeys`].
    pub sessions: SessionKeys,
    /// The `[seat]` table: where a session with no head runs its tools. See [`SeatKeys`] and
    /// [`seat_default`].
    pub seat: SeatKeys,
    /// The `[catalogue]` table: whether this daemon keeps the model catalogue's cache fresh by
    /// itself. See [`CatalogueKeys`].
    pub catalogue: CatalogueKeys,
    /// The `[subagents]` table: how many children run at once. See [`SubagentKeys`].
    pub subagents: SubagentKeys,
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

/// What a session does when its provider answers a usage limit with a known reset. It never
/// fails on one, so these are the whole of the choice.
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

/// `wait_if_within` when no level sets it: a reset this close is waited for even with a fallback.
pub const DEFAULT_WAIT_IF_WITHIN: std::time::Duration = std::time::Duration::from_secs(45 * 60);

/// `limit_probe` when no level sets it: how long a park waits before each early re-try of the
/// parked request, the last step repeating until the reset. A limit that was not what it said
/// (a rate limit read as a window) costs minutes, not the window.
pub const DEFAULT_LIMIT_PROBE: &[std::time::Duration] = &[
    std::time::Duration::from_secs(2 * 60),
    std::time::Duration::from_secs(5 * 60),
    std::time::Duration::from_secs(15 * 60),
    std::time::Duration::from_secs(30 * 60),
    std::time::Duration::from_secs(60 * 60),
];

/// A span as the file writes it - `45m`, `1h30m`, `90s`, `2d`, or a bare `0` - or a refusal
/// naming the shape. A bare number other than `0` is refused: minutes and seconds both read it.
pub fn parse_span(s: &str) -> Result<std::time::Duration, String> {
    let t = s.trim();
    if t == "0" {
        return Ok(std::time::Duration::ZERO);
    }
    let refuse = || {
        format!(
            "\"{t}\" is not a span: write a number and a unit - `45m`, `1h30m`, `90s`, `2d` - \
             or `0`"
        )
    };
    let (mut secs, mut digits, mut any) = (0u64, String::new(), false);
    for c in t.chars() {
        if c.is_ascii_digit() {
            digits.push(c);
            continue;
        }
        let per = match c {
            's' => 1,
            'm' => 60,
            'h' => 3600,
            'd' => 86_400,
            _ => return Err(refuse()),
        };
        let n: u64 = digits.parse().map_err(|_| refuse())?;
        secs = n
            .checked_mul(per)
            .and_then(|x| secs.checked_add(x))
            .ok_or_else(refuse)?;
        digits.clear();
        any = true;
    }
    if !any || !digits.is_empty() {
        return Err(refuse());
    }
    Ok(std::time::Duration::from_secs(secs))
}

/// A span in the file's own spelling, for the disclosure and the limit sentences: `45m`, `1h30m`,
/// `2d3h`, `0`. Seconds only below a minute, because a reset is not read to the second.
pub fn span_words(d: std::time::Duration) -> String {
    let s = d.as_secs();
    if s == 0 {
        return "0".into();
    }
    if s < 60 {
        return format!("{s}s");
    }
    let (days, hours, mins) = (s / 86_400, (s % 86_400) / 3600, (s % 3600) / 60);
    let mut out = String::new();
    for (n, unit) in [(days, 'd'), (hours, 'h'), (mins, 'm')] {
        if n > 0 {
            out.push_str(&format!("{n}{unit}"));
        }
    }
    out
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
/// wait_if_within = "45m"                         # a reset this close is waited for anyway
/// limit_probe = ["2m", "5m", "15m", "30m", "1h"] # a park re-tries early on these steps
/// offline_budget = "1h"                          # how long a turn retries while the box is offline
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
///
/// [provider.usage_warn]                          # percent of a window that is said once
/// 5h = [80, 95]
/// weekly = [70, 95]
/// monthly = [70, 95]
///
/// [provider.kimi-code.usage_warn]                # one preset: wins per key
/// 5h = [90]
/// ```
///
/// `on_limit` absent means `switch` when a fallback list exists and `wait` when nothing is set -
/// see [`LeticodeConfig::limit_policy`]. `fallback` here is the same list as the file's top-level
/// `fallback`, and a file that writes both to different lists is refused by name.
///
/// `wait_if_within` refines `switch`: a reset sooner than it parks the session on its own model,
/// a later one moves it to the fallback until the reset. `0` never waits while a fallback is free.
/// Absent means [`DEFAULT_WAIT_IF_WITHIN`]; `wait` ignores it.
///
/// `offline_budget` is how long a turn keeps taking the round again while the failure is the box
/// having no network - name resolution or no route - before it ends saying the box is offline.
/// Those waits spend no retry. Absent means the daemon's own hour; `0` and a negative span are
/// refused, since either is a turn that claims to be patient and gives up before the first retry.
///
/// The three deadline keys resolve per key: the `provider/model` table, then the preset's table,
/// then the keys here, then the built-in 15 / 60 / 600 - see [`LeticodeConfig::deadlines`]. A
/// level's key wins over the same key at the other level. `0` and a `first_byte_seconds` above
/// `answer_seconds` in one table are refused by file and line.
///
/// `usage_warn` resolves per key: the preset's table, then `[provider.usage_warn]`, then the
/// built-in lists - see [`LeticodeConfig::usage_warn`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProviderKeys {
    pub on_limit: Option<OnLimit>,
    pub wait_if_within: Option<std::time::Duration>,
    /// `limit_probe`: the steps a park re-tries on before its reset. `Some(empty)` never probes.
    pub limit_probe: Option<Vec<std::time::Duration>>,
    /// `offline_budget`: how long a turn retries a no-network failure before it gives up.
    pub offline_budget: Option<std::time::Duration>,
    pub fallback: Vec<String>,
    /// The deadline keys written directly under `[provider]`.
    pub deadlines: DeadlineKeys,
    /// `[provider.NAME]` tables: NAME is a preset or `provider/model`.
    pub overrides: BTreeMap<String, DeadlineKeys>,
    /// `[provider.usage_warn]`.
    pub usage_warn: UsageWarnKeys,
    /// `[provider.NAME.usage_warn]`, NAME a preset.
    pub usage_warn_presets: BTreeMap<String, UsageWarnKeys>,
}

/// One `usage_warn` table, each list absent until a level writes it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UsageWarnKeys {
    pub five_h: Option<Vec<u8>>,
    pub weekly: Option<Vec<u8>>,
    pub monthly: Option<Vec<u8>>,
}

impl UsageWarnKeys {
    /// Keys `other` sets win, the rest stay.
    fn overridden_by(&self, other: &UsageWarnKeys) -> UsageWarnKeys {
        UsageWarnKeys {
            five_h: other.five_h.clone().or_else(|| self.five_h.clone()),
            weekly: other.weekly.clone().or_else(|| self.weekly.clone()),
            monthly: other.monthly.clone().or_else(|| self.monthly.clone()),
        }
    }
}

/// A `usage_warn` table: `5h`, `weekly`, `monthly`, each a list of percents 1 to 100.
fn usage_warn_keys(
    v: &toml::Value,
    path: &Path,
    text: &str,
    scope: &str,
    after: &str,
) -> Result<UsageWarnKeys, String> {
    let keys = "the keys are `5h`, `weekly` and `monthly`, each a list of percents like [80, 95]";
    let line = |k: &str| format!("{}:{}", path.display(), line_after(text, after, k));
    let Some(table) = v.as_table() else {
        return Err(format!(
            "{}: [{scope}] is a table, not {}; {keys}",
            line(after),
            kind_of(v)
        ));
    };
    let mut out = UsageWarnKeys::default();
    for (k, v) in table {
        let list = v
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|n| n.as_integer().and_then(|n| u8::try_from(n).ok()))
                    .collect::<Option<Vec<u8>>>()
            })
            .and_then(|l| l.filter(|l| l.iter().all(|n| (1..=100).contains(n))));
        let Some(mut list) = list else {
            return Err(format!(
                "{}: [{scope}] {k} is a list of whole percents from 1 to 100, like [80, 95]",
                line(k)
            ));
        };
        list.sort_unstable();
        list.dedup();
        match k.as_str() {
            "5h" => out.five_h = Some(list),
            "weekly" => out.weekly = Some(list),
            "monthly" => out.monthly = Some(list),
            _ => return Err(format!("{}: [{scope}] `{k}` is not a key; {keys}", line(k))),
        }
    }
    Ok(out)
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

/// A no-network budget: a span like `1h`, and never zero or negative - either one is a turn that
/// says it is patient and gives up before its first retry.
fn offline_budget_at(s: &str, from: &Origin) -> Result<std::time::Duration, String> {
    if s.trim().starts_with('-') {
        return Err(format!(
            "{from}: [provider] offline_budget = \"{s}\" is negative; a budget is a length of time"
        ));
    }
    let d = parse_span(s).map_err(|why| format!("{from}: [provider] offline_budget = {why}"))?;
    if d.is_zero() {
        return Err(format!(
            "{from}: [provider] offline_budget = 0 would give up before the first retry; write a \
             span like \"1h\", or leave the key out for the daemon's own hour"
        ));
    }
    Ok(d)
}

fn provider_keys(
    value: &toml::Value,
    path: &Path,
    text: &str,
    user: bool,
) -> Result<ProviderKeys, String> {
    let keys = "the keys are `on_limit` (\"wait\" or \"switch\"), `wait_if_within` (\"45m\"), \
                `limit_probe` ([\"2m\", \"5m\"]), `offline_budget` (\"1h\"), `fallback`, \
                `slow_notice_seconds`, `first_byte_seconds`, `answer_seconds`, `[provider.usage_warn]`, \
                and `[provider.NAME]` tables (a preset or `provider/model`) holding the last three \
                and a preset's `usage_warn`";
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
            "wait_if_within" => {
                let s = v.as_str().ok_or_else(|| {
                    format!(
                        "{from}: [provider] wait_if_within is a string like \"45m\", not {}",
                        kind_of(v)
                    )
                })?;
                let d = parse_span(s)
                    .map_err(|why| format!("{from}: [provider] wait_if_within = {why}"))?;
                out.wait_if_within = Some(d);
            }
            "limit_probe" => {
                let list = v.as_array().ok_or_else(|| {
                    format!(
                        "{from}: [provider] limit_probe is a list of spans like [\"2m\", \"5m\"], \
                         not {}",
                        kind_of(v)
                    )
                })?;
                let mut steps = Vec::with_capacity(list.len());
                for item in list {
                    let s = item.as_str().ok_or_else(|| {
                        format!(
                            "{from}: [provider] limit_probe holds spans like \"2m\", not {}",
                            kind_of(item)
                        )
                    })?;
                    let d = parse_span(s)
                        .map_err(|why| format!("{from}: [provider] limit_probe = {why}"))?;
                    if d.is_zero() {
                        return Err(format!(
                            "{from}: [provider] limit_probe step \"{s}\" is zero; an empty list \
                             turns probing off"
                        ));
                    }
                    steps.push(d);
                }
                out.limit_probe = Some(steps);
            }
            "offline_budget" => {
                let s = v.as_str().ok_or_else(|| {
                    format!(
                        "{from}: [provider] offline_budget is a string like \"1h\", not {}",
                        kind_of(v)
                    )
                })?;
                out.offline_budget = Some(offline_budget_at(s, &from)?);
            }
            "fallback" => {
                out.fallback = fallback_list(v).map_err(|why| format!("{from}: {why}"))?;
            }
            "slow_notice_seconds" | "first_byte_seconds" | "answer_seconds" => {}
            "usage_warn" => {
                out.usage_warn =
                    usage_warn_keys(v, path, text, "provider.usage_warn", "usage_warn")?;
            }
            name => {
                let Some(sub) = v.as_table() else {
                    return Err(format!("{from}: [provider] `{name}` is not a key; {keys}"));
                };
                for k in sub.keys() {
                    if !matches!(
                        k.as_str(),
                        "slow_notice_seconds"
                            | "first_byte_seconds"
                            | "answer_seconds"
                            | "usage_warn"
                    ) {
                        return Err(format!(
                            "{}:{}: [provider.{name}] `{k}` is not a key here; a preset or model \
                             table holds `slow_notice_seconds`, `first_byte_seconds`, \
                             `answer_seconds` and a preset's `usage_warn`",
                            path.display(),
                            line_after(text, name, k)
                        ));
                    }
                }
                let scope = format!("provider.{name}");
                if let Some(w) = sub.get("usage_warn") {
                    let warn_scope = format!("{scope}.usage_warn");
                    out.usage_warn_presets.insert(
                        name.to_string(),
                        usage_warn_keys(w, path, text, &warn_scope, &warn_scope)?,
                    );
                }
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
/// job_dead_after_seconds = 1800 # a background job with no output and no CPU for this long is dead
/// io_wbps = "200M"         # io.max write bytes/s on every job (default: none)
/// io_wiops = 2000          # io.max write ops/s on every job (default: none)
/// disk_floor = "5%"        # free space below which the top writers are stopped; "20G"; 0 = off
/// disk_floor_action = "freeze" # or "kill"
/// io_pressure_full = 60    # system io "full" pressure, percent, that throttles the top writer; 0 = off
/// disk_watch = ["/build"]  # filesystems watched besides each job's cwd
/// tasks_max = 512          # processes one job's scope may hold; 0 = no cap
/// ```
///
/// `false` keeps the advice only. The project's file wins over the user's. `job_dead_after_seconds`
/// is the ONLY clock a background job has: a job with no deadline is ended when it is dead, never at
/// a fixed age, because ten minutes is nothing next to a fuzz campaign or a cold build. `0` turns the
/// check off, and then only `job_kill` ends a job that has wedged.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExecKeys {
    pub auto_background: Option<bool>,
    pub foreground_seconds: Option<u64>,
    pub job_dead_after_seconds: Option<u64>,
    pub io_wbps: Option<u64>,
    pub io_wiops: Option<u64>,
    pub disk_floor: Option<letibot_tools::exec::diskio::DiskFloor>,
    pub disk_floor_action: Option<letibot_tools::exec::diskio::FloorAction>,
    pub io_pressure_full: Option<f64>,
    pub disk_watch: Option<Vec<PathBuf>>,
    /// The most processes one job's scope may hold. A fork loop holds almost no memory and
    /// copies itself, so a memory cap never sees it; this is the bound that does.
    pub tasks_max: Option<u32>,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecTable {
    #[serde(default)]
    auto_background: Option<bool>,
    #[serde(default)]
    foreground_seconds: Option<u64>,
    #[serde(default)]
    job_dead_after_seconds: Option<u64>,
    #[serde(default)]
    io_wbps: Option<toml::Value>,
    #[serde(default)]
    io_wiops: Option<u64>,
    #[serde(default)]
    disk_floor: Option<toml::Value>,
    #[serde(default)]
    disk_floor_action: Option<String>,
    #[serde(default)]
    io_pressure_full: Option<f64>,
    #[serde(default)]
    tasks_max: Option<u32>,
    #[serde(default)]
    disk_watch: Option<Vec<PathBuf>>,
}

/// A size key: an integer of bytes, or a string with a unit (`"200M"`, `"20G"`, `"5%"` for a floor).
fn size_word(v: &toml::Value) -> Result<String, String> {
    match v {
        toml::Value::Integer(n) if *n >= 0 => Ok(n.to_string()),
        toml::Value::String(s) => Ok(s.clone()),
        other => Err(format!("`{other}` is not a size")),
    }
}

fn exec_keys(value: &toml::Value) -> Result<ExecKeys, String> {
    let t: ExecTable = value.clone().try_into().map_err(|e| {
        format!(
            "[exec]: {e}; the keys are `auto_background` (bool), `foreground_seconds`, \
             `job_dead_after_seconds` (0 turns the dead check off), `io_wbps`, `io_wiops`, \
             `disk_floor`, `disk_floor_action`, `io_pressure_full`, `disk_watch` and `tasks_max`"
        )
    })?;
    use letibot_tools::exec::diskio::{DiskFloor, FloorAction, parse_size};
    let io_wbps = match &t.io_wbps {
        None => None,
        Some(v) => Some(
            size_word(v)
                .and_then(|w| parse_size(&w))
                .map_err(|e| format!("[exec] io_wbps: {e}"))?,
        )
        .filter(|n| *n > 0),
    };
    let disk_floor = match &t.disk_floor {
        None => None,
        Some(v) => Some(
            size_word(v)
                .and_then(|w| DiskFloor::parse(&w))
                .map_err(|e| format!("[exec] disk_floor: {e}"))?,
        ),
    };
    let disk_floor_action = match &t.disk_floor_action {
        None => None,
        Some(a) => {
            Some(FloorAction::parse(a).map_err(|e| format!("[exec] disk_floor_action: {e}"))?)
        }
    };
    if t.io_pressure_full
        .is_some_and(|p| !(0.0..=100.0).contains(&p))
    {
        return Err("[exec] io_pressure_full is a percentage, from 0 (off) to 100".into());
    }
    if t.foreground_seconds == Some(0) {
        return Err("[exec] foreground_seconds must be at least 1".into());
    }
    Ok(ExecKeys {
        auto_background: t.auto_background,
        foreground_seconds: t.foreground_seconds,
        job_dead_after_seconds: t.job_dead_after_seconds,
        io_wbps,
        io_wiops: t.io_wiops.filter(|n| *n > 0),
        disk_floor,
        disk_floor_action,
        io_pressure_full: t.io_pressure_full,
        disk_watch: t.disk_watch,
        tasks_max: t.tasks_max,
    })
}

/// The `[tools]` table: how one round's calls run. One key, and it is a switch rather than a
/// number because the pool's width is the pool's own fact (`letibot_tools::runtime::POOL_WIDTH`).
///
/// ```toml
/// [tools]
/// parallel = false # a round's calls run one at a time, in the order the model proposed them
/// ```
///
/// `true` is a round of adjacent read-class calls running at once, up to the pool's width; every
/// other call - a write, a command, a session verb, an unknown name - is a barrier whether this is
/// on or off. Off is the escape hatch for a box where concurrent reads misbehave, and it costs the
/// turn wall clock rather than correctness. The project's file wins over the user's.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ToolKeys {
    pub parallel: Option<bool>,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolsTable {
    #[serde(default)]
    parallel: Option<bool>,
}

fn tools_keys(value: &toml::Value) -> Result<ToolKeys, String> {
    let t: ToolsTable = value.clone().try_into().map_err(|e: toml::de::Error| {
        format!(
            "[tools]: {e}; the key is `parallel` (bool), which runs a round's adjacent \
                 reads at once, and `false` runs every call one at a time"
        )
    })?;
    Ok(ToolKeys {
        parallel: t.parallel,
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

/// How long a background job with no deadline may go with neither output nor CPU before the dead
/// check ends it: `[exec] job_dead_after_seconds`, the project's over the user's, `None` for a file
/// that asked for `0` (the check off), and thirty minutes when no file said anything.
pub fn job_dead_after(
    user: &LeticodeConfig,
    workspace: &Path,
    read_project: bool,
) -> Option<Duration> {
    let mut window = Some(DEFAULT_JOB_DEAD_AFTER);
    let mut apply = |k: &ExecKeys| {
        if let Some(s) = k.job_dead_after_seconds {
            window = (s > 0).then(|| Duration::from_secs(s));
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
    window
}

/// The most processes one job's scope may hold: `[exec] tasks_max`, the project's over the
/// user's, [`DEFAULT_TASKS_MAX`] when no file said anything, and 0 for a file that turned the
/// bound off. A fork loop holds almost no memory, so this is the only cap that meets one.
pub fn tasks_max(user: &LeticodeConfig, workspace: &Path, read_project: bool) -> u32 {
    let mut n = letibot_tools::exec::host::DEFAULT_TASKS_MAX;
    let mut apply = |k: &ExecKeys| {
        if let Some(v) = k.tasks_max {
            n = v;
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
    n
}

/// The io policy a session's jobs run under: defaults, then the user file, then the nearest
/// project file. `disk_watch` replaces the default list rather than adding to it.
pub fn io_policy(
    user: &LeticodeConfig,
    workspace: &Path,
    read_project: bool,
) -> letibot_tools::exec::diskio::IoPolicy {
    let mut p = letibot_tools::exec::diskio::IoPolicy::default();
    let mut apply = |k: &ExecKeys| {
        if let Some(v) = k.io_wbps {
            p.wbps = Some(v);
        }
        if let Some(v) = k.io_wiops {
            p.wiops = Some(v);
        }
        if let Some(v) = k.disk_floor {
            p.floor = v;
        }
        if let Some(v) = k.disk_floor_action {
            p.action = v;
        }
        if let Some(v) = k.io_pressure_full {
            p.pressure_full = (v > 0.0).then_some(v);
        }
        if let Some(v) = &k.disk_watch {
            p.watch = v.clone();
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

/// `[mcp.NAME]` in a `leticode.toml`: one MCP server, stdio (`command`) or HTTP (`url`).
/// A table with neither or both is the daemon's to report as a server that did not start, so
/// one bad table does not refuse the file.
///
/// ```toml
/// [mcp.files]
/// command = "npx"
/// args = ["-y", "@modelcontextprotocol/server-filesystem", "/srv"]
/// env = { LOG = "warn" }
///
/// [mcp.docs]
/// url = "http://127.0.0.1:8931/mcp"
/// headers = { Authorization = "Bearer ..." }
/// timeout_seconds = 30
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct McpServerKeys {
    pub command: Option<String>,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub url: Option<String>,
    pub headers: BTreeMap<String, String>,
    /// Per request. `None` is the client's 60 s.
    pub timeout_seconds: Option<u64>,
    /// `false` keeps the table and starts nothing.
    pub enabled: bool,
    /// Where a stdio server runs: set for a project file's servers, which run in the checkout.
    pub cwd: Option<PathBuf>,
}

/// The project file read for MCP servers, the shape pi, Claude Code and Cursor share.
pub const MCP_JSON: &str = ".mcp.json";

/// A server name `server__tool` can be split back into.
fn mcp_name_ok(name: &str) -> bool {
    !name.is_empty()
        && !name.contains("__")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// A checkout's `.mcp.json`, read once: its path, the sha256 of the bytes parsed, and each
/// server or why it cannot be one.
#[derive(Debug, Clone, PartialEq)]
pub struct McpJson {
    pub path: PathBuf,
    pub sha256: String,
    pub servers: BTreeMap<String, Result<McpServerKeys, String>>,
}

/// `<workspace>/.mcp.json`'s `mcpServers`, each a server or why it cannot be one. Keys other
/// clients define are ignored, since the file is shared with them. `Ok(None)` is no file.
///
/// ```json
/// {"mcpServers": {"fs": {"command": "npx", "args": ["-y", "server-fs", "."]},
///                 "docs": {"type": "http", "url": "https://example.com/mcp"}}}
/// ```
pub fn mcp_json(workspace: &Path) -> Result<Option<McpJson>, String> {
    let path = workspace.join(MCP_JSON);
    if !path.is_file() {
        return Ok(None);
    }
    mcp_json_at(&path).map(Some)
}

/// One `.mcp.json` by its own path, for a verb that pins the file an operator named rather than
/// the one this checkout holds. Its relative command paths are joined to the file's directory.
/// `Err` for a path that is not there: a pin on a file nobody can read is a pin on nothing.
pub fn mcp_json_at(path: &Path) -> Result<McpJson, String> {
    if !path.is_file() {
        return Err(format!("{}: there is no such file", path.display()));
    }
    let workspace = path.parent().unwrap_or(Path::new("/"));
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let sha256 = sha256_hex(&bytes);
    let text = String::from_utf8(bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    let v: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut out = BTreeMap::new();
    if let Some(servers) = v.get("mcpServers") {
        let servers = servers
            .as_object()
            .ok_or_else(|| format!("{}: `mcpServers` is an object", path.display()))?;
        for (name, s) in servers {
            out.insert(name.clone(), mcp_json_server(name, s, workspace));
        }
    }
    Ok(McpJson {
        path: path.to_path_buf(),
        sha256,
        servers: out,
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// What `[mcp_projects] pinned` says about one `.mcp.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpPin {
    /// Pinned at these bytes: its servers start.
    Held,
    /// Pinned at other bytes: the file changed since the operator read it.
    Moved { pinned: String },
    /// Never pinned.
    Unpinned,
}

/// The pin for `file` at `sha256`, matched by exact path, so a pin covers one file and no
/// worktree beside or under it.
pub fn mcp_pin(pinned: &BTreeMap<PathBuf, String>, file: &Path, sha256: &str) -> McpPin {
    let Ok(file) = file.canonicalize() else {
        return McpPin::Unpinned;
    };
    let Some(want) = pinned
        .iter()
        .find(|(p, _)| p.canonicalize().is_ok_and(|p| p == file))
        .map(|(_, sha)| sha)
    else {
        return McpPin::Unpinned;
    };
    if want.eq_ignore_ascii_case(sha256) {
        McpPin::Held
    } else {
        McpPin::Moved {
            pinned: want.clone(),
        }
    }
}

/// `${VAR}` and `${VAR:-default}`, as Claude Code reads them in `.mcp.json`. A variable that is
/// unset and has no default refuses the server by name: an empty string is a value nobody wrote,
/// and the failure it causes is far from the file that made it.
fn mcp_expand(text: &str, lookup: &dyn Fn(&str) -> Option<String>) -> Result<String, String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("${") {
        out.push_str(&rest[..at]);
        let after = &rest[at + 2..];
        let Some(end) = after.find('}') else {
            return Err(format!("`${{{after}` has no closing `}}`"));
        };
        let inner = &after[..end];
        if inner.contains("${") {
            return Err(format!(
                "`${{{inner}}}` nests a variable, which is not expanded"
            ));
        }
        let (name, default) = match inner.split_once(":-") {
            Some((n, d)) => (n, Some(d)),
            None => (inner, None),
        };
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(format!("`${{{inner}}}` does not name a variable"));
        }
        // Set but empty is empty, as a shell reads `${VAR}`; `:-` is the form that takes the
        // default for an empty one too.
        match (lookup(name), default) {
            (Some(v), Some(d)) if v.is_empty() => out.push_str(d),
            (Some(v), _) => out.push_str(&v),
            (None, Some(d)) => out.push_str(d),
            (None, None) => {
                return Err(format!(
                    "`${{{name}}}` names `{name}`, which is not set, and it has no `:-` default"
                ));
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

fn mcp_json_server(
    name: &str,
    v: &serde_json::Value,
    workspace: &Path,
) -> Result<McpServerKeys, String> {
    if !mcp_name_ok(name) {
        return Err("a server name is letters, digits, `-` and `_`, without `__`".into());
    }
    let o = v.as_object().ok_or("its entry is not an object")?;
    let string = |key: &str| -> Result<Option<String>, String> {
        match o.get(key) {
            None => Ok(None),
            Some(x) => x
                .as_str()
                .map(|s| Some(s.to_string()))
                .ok_or_else(|| format!("`{key}` is not a string")),
        }
    };
    let strings = |key: &str| -> Result<BTreeMap<String, String>, String> {
        let Some(x) = o.get(key) else {
            return Ok(BTreeMap::new());
        };
        x.as_object()
            .ok_or_else(|| format!("`{key}` is not an object of strings"))?
            .iter()
            .map(|(k, v)| {
                v.as_str()
                    .map(|s| (k.clone(), s.to_string()))
                    .ok_or_else(|| format!("`{key}.{k}` is not a string"))
            })
            .collect()
    };
    match string("type")?.as_deref() {
        None | Some("stdio" | "http" | "streamable-http" | "streamableHttp") => {}
        Some(other) => {
            return Err(format!(
                "transport `{other}` is not supported; stdio and streamable http are"
            ));
        }
    }
    let args = match o.get("args") {
        None => Vec::new(),
        Some(a) => a
            .as_array()
            .and_then(|a| {
                a.iter()
                    .map(|x| x.as_str().map(str::to_string))
                    .collect::<Option<Vec<_>>>()
            })
            .ok_or("`args` is not a list of strings")?,
    };
    // The values a server is reached by, and not the names: Claude Code expands the five the
    // shape has, and a refusal says which file the operator has to read.
    let file = workspace.join(MCP_JSON);
    let expand = |s: &str| {
        mcp_expand(s, &|n: &str| std::env::var(n).ok())
            .map_err(|why| format!("{}: {why}", file.display()))
    };
    let args = args
        .iter()
        .map(|a| expand(a))
        .collect::<Result<Vec<_>, _>>()?;
    let values = |m: BTreeMap<String, String>| {
        m.into_iter()
            .map(|(k, v)| expand(&v).map(|v| (k, v)))
            .collect::<Result<BTreeMap<_, _>, _>>()
    };
    Ok(McpServerKeys {
        command: string("command")?.map(|s| expand(&s)).transpose()?,
        args,
        env: values(strings("env")?)?,
        url: string("url")?.map(|s| expand(&s)).transpose()?,
        headers: values(strings("headers")?)?,
        timeout_seconds: None,
        enabled: !o.get("disabled").and_then(|d| d.as_bool()).unwrap_or(false),
        cwd: Some(workspace.to_path_buf()),
    })
}

/// Whether `workspace` is at or under a directory in `[mcp_projects] trusted`.
pub fn mcp_trusts(trusted: &[PathBuf], workspace: &Path) -> bool {
    let Ok(ws) = workspace.canonicalize() else {
        return false;
    };
    trusted
        .iter()
        .filter_map(|t| t.canonicalize().ok())
        .any(|t| ws.starts_with(t))
}

/// `[mcp_projects]` as read: the `trusted` directories and the `pinned` files.
type McpProjects = (Vec<PathBuf>, BTreeMap<PathBuf, String>);

/// An absolute path from the user file, `~/` allowed.
fn mcp_user_path(from: &impl std::fmt::Display, key: &str, raw: &str) -> Result<PathBuf, String> {
    let p = match raw.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default()
            .join(rest),
        None => PathBuf::from(raw),
    };
    if !p.is_absolute() {
        return Err(format!(
            "{from}: [mcp_projects] `{key}` entry `{raw}` is not an absolute path"
        ));
    }
    Ok(p)
}

/// `[mcp_projects]`: the `.mcp.json` files, by path and bytes, the user lets start processes.
fn mcp_projects(
    value: &toml::Value,
    path: &Path,
    text: &str,
    user: bool,
) -> Result<McpProjects, String> {
    let keys = "the keys are `pinned`, a table of .mcp.json path to sha256, and `trusted`, a \
                list of directories";
    let t = table_at(value, path, text, "mcp_projects", keys)?;
    let from = at(path, text, "[mcp_projects]", user);
    let (mut trusted, mut pinned) = (Vec::new(), BTreeMap::new());
    for (key, v) in t {
        match key.as_str() {
            "trusted" => {
                let list = v
                    .as_array()
                    .and_then(|a| a.iter().map(|x| x.as_str()).collect::<Option<Vec<_>>>())
                    .ok_or_else(|| {
                        format!("{from}: [mcp_projects] `trusted` is a list of strings")
                    })?;
                for dir in list {
                    trusted.push(mcp_user_path(&from, "trusted", dir)?);
                }
            }
            "pinned" => {
                let table = v.as_table().ok_or_else(|| {
                    format!("{from}: [mcp_projects] `pinned` is a table of path to sha256")
                })?;
                for (file, sha) in table {
                    let sha = sha
                        .as_str()
                        .filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
                        .ok_or_else(|| {
                            format!(
                                "{from}: [mcp_projects] `pinned` entry `{file}` is not a sha256 \
                                 in hex"
                            )
                        })?;
                    pinned.insert(
                        mcp_user_path(&from, "pinned", file)?,
                        sha.to_ascii_lowercase(),
                    );
                }
            }
            _ => {
                return Err(format!(
                    "{from}: [mcp_projects] `{key}` is not a key; {keys}"
                ));
            }
        }
    }
    Ok((trusted, pinned))
}

const MCP_KEYS: &str = "the keys are `command`, `args`, `env` (stdio), `url`, `headers` (http), \
                        `timeout_seconds` and `enabled`";

fn mcp_tables(
    value: &toml::Value,
    path: &Path,
    text: &str,
    user: bool,
) -> Result<BTreeMap<String, McpServerKeys>, String> {
    let servers = table_at(
        value,
        path,
        text,
        "mcp",
        "each server is a [mcp.NAME] table",
    )?;
    let mut out = BTreeMap::new();
    for (name, v) in servers {
        let from = at(path, text, &format!("[mcp.{name}]"), user);
        // `__` splits `server__tool`, so a name holding it could not be called.
        if !mcp_name_ok(name) {
            return Err(format!(
                "{from}: [mcp.{name}]: a server name is letters, digits, `-` and `_`, without `__`"
            ));
        }
        let t = v
            .as_table()
            .ok_or_else(|| format!("{from}: [mcp.{name}] is a table; {MCP_KEYS}"))?;
        let mut k = McpServerKeys {
            enabled: true,
            ..Default::default()
        };
        let string = |key: &str, v: &toml::Value| {
            v.as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("{from}: [mcp.{name}] `{key}` is a string"))
        };
        let strings = |key: &str, v: &toml::Value| -> Result<BTreeMap<String, String>, String> {
            let t = v
                .as_table()
                .ok_or_else(|| format!("{from}: [mcp.{name}] `{key}` is a table of strings"))?;
            t.iter()
                .map(|(k, v)| {
                    v.as_str()
                        .map(|s| (k.clone(), s.to_string()))
                        .ok_or_else(|| format!("{from}: [mcp.{name}] `{key}.{k}` is a string"))
                })
                .collect()
        };
        for (key, v) in t {
            match key.as_str() {
                "command" => k.command = Some(string(key, v)?),
                "url" => k.url = Some(string(key, v)?),
                "args" => {
                    k.args = v
                        .as_array()
                        .and_then(|a| {
                            a.iter()
                                .map(|x| x.as_str().map(str::to_string))
                                .collect::<Option<Vec<_>>>()
                        })
                        .ok_or_else(|| {
                            format!("{from}: [mcp.{name}] `args` is a list of strings")
                        })?
                }
                "env" => k.env = strings(key, v)?,
                "headers" => k.headers = strings(key, v)?,
                "timeout_seconds" => {
                    k.timeout_seconds =
                        Some(v.as_integer().filter(|n| *n >= 1).ok_or_else(|| {
                            format!("{from}: [mcp.{name}] `timeout_seconds` is an int >= 1")
                        })? as u64)
                }
                "enabled" => {
                    k.enabled = v
                        .as_bool()
                        .ok_or_else(|| format!("{from}: [mcp.{name}] `enabled` is a bool"))?
                }
                other => {
                    return Err(format!(
                        "{from}: [mcp.{name}] `{other}` is not a key; {MCP_KEYS}"
                    ));
                }
            }
        }
        out.insert(name.clone(), k);
    }
    Ok(out)
}

/// `[output]` in a `leticode.toml`: the cap a tool result is spilled at, the head and tail kept
/// inline and the middle behind a `read_spill` locator. `0` turns that half of the cap off.
///
/// ```toml
/// [output]
/// inline_bytes = 51200
/// inline_lines = 2000
/// spill_budget = "2G"
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OutputKeys {
    pub inline_bytes: Option<u64>,
    pub inline_lines: Option<u64>,
    /// Bytes one project's spill store may hold; `0` keeps everything.
    pub spill_budget: Option<u64>,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct OutputTable {
    #[serde(default)]
    inline_bytes: Option<u64>,
    #[serde(default)]
    inline_lines: Option<u64>,
    #[serde(default)]
    spill_budget: Option<toml::Value>,
}

fn output_keys(value: &toml::Value) -> Result<OutputKeys, String> {
    let t: OutputTable = value.clone().try_into().map_err(|e| {
        format!(
            "[output]: {e}; the keys are `inline_bytes` and `inline_lines` (integers, 0 turns \
             that cap off) and `spill_budget` (a size like \"2G\")"
        )
    })?;
    let spill_budget = match t.spill_budget {
        None => None,
        Some(toml::Value::Integer(n)) if n >= 0 => Some(n as u64),
        Some(toml::Value::String(s)) => Some(
            letibot_tools::lifecycle::size_bytes(&s)
                .map_err(|e| format!("[output] spill_budget: {e}"))?,
        ),
        Some(other) => {
            return Err(format!(
                "[output] spill_budget: {other} is not a size; write it like \"2G\""
            ));
        }
    };
    Ok(OutputKeys {
        inline_bytes: t.inline_bytes,
        inline_lines: t.inline_lines,
        spill_budget,
    })
}

/// The budget each project's spill store is held to: 2 GiB, then the user file, then the
/// nearest project file. `None` is a `0`, which keeps every spill.
pub fn spill_budget(user: &LeticodeConfig, workspace: &Path, read_project: bool) -> Option<u64> {
    let mut budget = user
        .output
        .spill_budget
        .unwrap_or(letibot_tools::spill::DEFAULT_SPILL_BUDGET);
    if read_project
        && let Some(path) = LeticodeConfig::discover(workspace)
        && let Ok(project) = LeticodeConfig::load(&path)
        && let Some(b) = project.output.spill_budget
    {
        budget = b;
    }
    (budget > 0).then_some(budget)
}

/// The generic output cap a session runs under: 50 KiB / 2000 lines, then the user file, then
/// the nearest project file. A tool's own tighter cap is unaffected.
pub fn output_cap(
    user: &LeticodeConfig,
    workspace: &Path,
    read_project: bool,
) -> letibot_tools::OutputCap {
    let mut cap = letibot_tools::OutputCap::default();
    let mut apply = |k: &OutputKeys| {
        if let Some(b) = k.inline_bytes {
            cap.bytes = (b > 0).then_some(b as usize);
        }
        if let Some(l) = k.inline_lines {
            cap.lines = (l > 0).then_some(l as usize);
        }
    };
    apply(&user.output);
    if read_project && let Some(path) = LeticodeConfig::discover(workspace) {
        match LeticodeConfig::load(&path) {
            Ok(project) => apply(&project.output),
            Err(why) => {
                eprintln!("  leticode.toml: {why} - this session's [output] is the level below")
            }
        }
    }
    cap
}

/// `[seat] default` as a session is placed: the project file's word, else the user's, else
/// [`SeatDefault::Vm`]. Read where a HEADLESS seat is about to be started - a package, a
/// subagent, a queue gate - and never for an interactive session, which stays on the host.
///
/// A file that will not parse is not a refusal here: the placement is a preference with a
/// fallback, so the level below is used and the reason is said once, as the open says it.
pub fn seat_default(
    user: &LeticodeConfig,
    workspace: &Path,
    read_project: bool,
) -> (SeatDefault, Option<String>) {
    if read_project && let Some(path) = LeticodeConfig::discover(workspace) {
        match LeticodeConfig::load(&path) {
            Ok(project) => {
                if let Some(word) = project.seat.default {
                    return (word, None);
                }
            }
            Err(why) => return (user.seat.default.unwrap_or_default(), Some(why)),
        }
    }
    (user.seat.default.unwrap_or_default(), None)
}

/// Thirty minutes of neither output nor CPU: what a wedged job costs before the harness calls it
/// dead. The number the operator's `[exec]` overrides, and the one every sentence about the dead
/// check quotes when no file set another.
pub const DEFAULT_JOB_DEAD_AFTER: Duration = Duration::from_secs(1800);

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

/// `[browser]` in a `leticode.toml`: where `letibot browser` listens and which extension it
/// answers. Read from the user file only, because a project file is writable by its sessions
/// and must not choose who may prompt them.
///
/// ```toml
/// [browser]
/// port = 8790
/// extension_id = "kbaoealgdkldcgaojgdehmdaaekkmffd"
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BrowserKeys {
    pub port: Option<u16>,
    pub extension_id: Option<String>,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BrowserTable {
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    extension_id: Option<String>,
}

fn browser_keys(value: &toml::Value) -> Result<BrowserKeys, String> {
    let t: BrowserTable = value.clone().try_into().map_err(|e| {
        format!("[browser]: {e}; the keys are `port` (1-65535) and `extension_id` (32 letters a-p)")
    })?;
    if t.port == Some(0) {
        return Err("[browser]: `port = 0` is not a port; leave it out for 8790".into());
    }
    let extension_id = t.extension_id.as_deref().and_then(non_empty);
    if let Some(id) = &extension_id
        && (id.len() != 32 || !id.bytes().all(|b| (b'a'..=b'p').contains(&b)))
    {
        return Err(format!(
            "[browser]: extension_id `{id}` is not an extension id: 32 letters a-p, as \
             chrome://extensions shows it"
        ));
    }
    Ok(BrowserKeys {
        port: t.port,
        extension_id,
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
    let keys = "the keys are `grants_ro`, `grants_rw`, `hide` (lists of paths), `network` (reasons \
                this seat may reach the net), `firecode_only` (a list of program names) and `env` \
                (a table of constants)";
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
            // Reasons, not a switch: an entry is the sentence a disclosure and an audit row
            // repeat, so a seat never reaches the network without one.
            "network" => out.network = list("network")?,
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
    let keys = "the keys are `grants_ro`, `grants_rw`, `hide` (lists of paths), `network` (reasons \
                this seat may reach the net), `docker` (true or false), `secrets` (a list of \
                names) and `env` (constants, or `{ file = ... }` / `{ command = ... }`)";
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
                // Reasons, not a switch: an entry is the sentence a disclosure and an audit row
                // repeat, so a seat never reaches the network without one.
                "network" => t.network = list("network")?,
                "docker" => {
                    // A capability, not a path: the seat gets a socket under its owner root and
                    // the engine's own socket stays out of its view.
                    let Some(on) = v.as_bool() else {
                        return Err(format!(
                            "{}:{}: [perimeter.{name}] docker is `true` or `false`",
                            path.display(),
                            line_of(text, "docker")
                        ));
                    };
                    t.docker = Some(Entry {
                        value: on,
                        from: at("docker"),
                    });
                }
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
/// asked to do, and what each one needs of the seat.
///
/// ```toml
/// [skills]
/// pin = ["latex-presentation", "release"]
///
/// [skills.when.instrument-dashboard]
/// requires = ["flowy"]     # a status page fits a seat that can publish; without it, a pane
/// ```
///
/// Both levels' pins apply, so `pin` and `never_offer` are unions and not overrides: a pin is a
/// request to have a skill loaded, and a project file (which a session with `write` can edit)
/// must not be able to unsay what the operator pinned for every project on the box. A
/// `[skills.when.<name>]` table is the opposite question - what a skill NEEDS - so it REPLACES
/// the skill's own frontmatter for that name, and when both levels name one skill the operator's
/// table is the one in force, whole. A project file can leave a skill alone or make it fit this
/// project less; it cannot unhide what the operator hid.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkillKeys {
    pub pin: Vec<String>,
    /// Skills never offered or listed as usable, whatever their score: the operator's veto for
    /// skills that name tools this box lacks and that the automatic filter cannot see.
    pub never_offer: Vec<String>,
    /// Skills hidden from the model and kept for the operator's `/skills`, like the frontmatter's
    /// `disable-model-invocation: true`. A union, so a project cannot unhide what the operator hid.
    pub hide: Vec<String>,
    /// Whether a session on a brief (subagent, package, review queue, one-shot) is offered skills.
    /// `None` is the default, off: the brief already names what it needs.
    pub offer_children: Option<bool>,
    /// Per skill, what it needs of a seat before it applies at all.
    pub when: std::collections::BTreeMap<String, letibot_tools::builtins::skill::SkillWhen>,
}

impl SkillKeys {
    /// The two levels as one: this level's names in its own order, then the ones `other`
    /// adds, each once. See the type's note on why a pin unions rather than overrides.
    pub fn overridden_by(&self, other: &SkillKeys) -> SkillKeys {
        let union = |a: &[String], b: &[String]| {
            let mut out: Vec<String> = Vec::with_capacity(a.len() + b.len());
            for name in a.iter().chain(b.iter()) {
                if !out.contains(name) {
                    out.push(name.clone());
                }
            }
            out
        };
        // The project's tables first, then this (operator) level over them: a name both levels
        // speak about is the operator's answer, and a table replaces rather than merges.
        let mut when = other.when.clone();
        when.extend(
            self.when
                .iter()
                .map(|(name, table)| (name.clone(), table.clone())),
        );
        SkillKeys {
            pin: union(&self.pin, &other.pin),
            never_offer: union(&self.never_offer, &other.never_offer),
            hide: union(&self.hide, &other.hide),
            offer_children: other.offer_children.or(self.offer_children),
            when,
        }
    }

    /// Whether the file said anything at all, for the disclosure, which must not claim a
    /// setting nobody made.
    pub fn is_empty(&self) -> bool {
        self.pin.is_empty()
            && self.never_offer.is_empty()
            && self.hide.is_empty()
            && self.offer_children.is_none()
            && self.when.is_empty()
    }
}

/// The `[skills]` table as the file wrote it; unknown keys refused by name.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SkillTable {
    #[serde(default)]
    pin: Option<Vec<String>>,
    #[serde(default)]
    never_offer: Option<Vec<String>>,
    #[serde(default)]
    hide: Option<Vec<String>>,
    #[serde(default)]
    offer_children: Option<bool>,
    #[serde(default)]
    when: std::collections::BTreeMap<String, letibot_tools::builtins::skill::SkillWhen>,
}

fn skill_keys(value: &toml::Value) -> Result<SkillKeys, String> {
    let t: SkillTable = value.clone().try_into().map_err(|e| {
        format!(
            "[skills]: {e}; the keys are `pin`, `never_offer` and `hide` (lists of skill names), \
             `offer_children` (a bool) and `when.<name>` (a table of `requires`, `unless` and `command`)"
        )
    })?;
    Ok(SkillKeys {
        pin: t.pin.unwrap_or_default(),
        never_offer: t.never_offer.unwrap_or_default(),
        hide: t.hide.unwrap_or_default(),
        offer_children: t.offer_children,
        when: t.when,
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
/// The box-wide 64 MiB is right for `ls` and `grep`; a project that builds raises it in its own
/// file. `None` is the level below. Values are `exec::memory::Amount`s - a size or a fraction of
/// what is free - and anything else refuses the file by name.
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

/// `[warnings]` in a `leticode.toml`: what the head files and never draws. See the module doc for
/// the rule it inherits.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WarningKeys {
    /// Warning codes the head files but does not draw. A code and not a copy: `head.toml`'s
    /// `retired` retires one note at a time, and this retires the kind.
    pub mute: Vec<Entry<String>>,
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
/// daemon's own: `mergequeue::stuck::STUCK_AFTER_MS` and `builtins::todo::STUCK_REPEAT`, which
/// stay the defaults' one home.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueueKeys {
    pub stuck_after_seconds: Option<u64>,
    pub stuck_repeat_seconds: Option<u64>,
    /// Repositories the session is not told the gate's news of. A union across levels, because
    /// muting only narrows what a session hears.
    pub mute: Vec<String>,
}

/// `[nag]` in a `leticode.toml`: the idle plan-check's window, the shortest re-arm an operator
/// add earns, and the ceiling a repeated check retreats to. `None` is the daemon's
/// own, which is [`letibot_tools::builtins::todo::NagTimings::default`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NagKeys {
    pub after_seconds: Option<u64>,
    pub floor_seconds: Option<u64>,
    pub repeat_cap_seconds: Option<u64>,
}

/// `[automode]` in a `leticode.toml`: what a session with no head does with a call the reviewer
/// did not settle, and how many blocks it takes before the operator is asked instead.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AutomodeKeys {
    /// `review` (the default), `queue` or `refuse`; see [`Unattended`]. Validated at load, so a
    /// misspelt word is refused with its line rather than read as the default.
    pub unattended: Option<String>,
    /// Blocks in a row before the operator is asked. `None` is the daemon's own number.
    pub blocks_in_a_row: Option<u32>,
    /// Blocks in one session before the operator is asked. `None` is the daemon's own number.
    pub blocks_per_session: Option<u32>,
}

impl AutomodeKeys {
    /// Whether the file said anything, so the disclosure does not claim a setting nobody made.
    pub fn is_empty(&self) -> bool {
        self.unattended.is_none()
            && self.blocks_in_a_row.is_none()
            && self.blocks_per_session.is_none()
    }

    /// The policy this level names, `review` when it names none.
    pub fn unattended(&self) -> Unattended {
        self.unattended
            .as_deref()
            .and_then(Unattended::parse)
            .unwrap_or_default()
    }
}

/// What a session nobody is watching does with a call that would be a card.
///
/// A session with no head attached is not one where a card is a question: it is one where the card
/// waits out its whole budget and then fails closed, for as long as the run lasts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Unattended {
    /// Wait for a head, as a card always has: a head that attaches in time answers it, and one that
    /// never does leaves the call refused.
    #[default]
    Review,
    /// Do not wait: the ask is queued as its own row - `/gate recent` lists it and
    /// `/gate grant <row>` answers it, which is the queue the operator reads - and this call is
    /// refused at once with that row's id. Nothing is drawn on a screen nobody is at.
    Queue,
    /// Do not ask at all: refuse the call and say that the setting is why.
    Refuse,
}

impl Unattended {
    pub const ALL: [Unattended; 3] = [Unattended::Review, Unattended::Queue, Unattended::Refuse];

    pub fn as_str(self) -> &'static str {
        match self {
            Unattended::Review => "review",
            Unattended::Queue => "queue",
            Unattended::Refuse => "refuse",
        }
    }

    pub fn parse(s: &str) -> Option<Unattended> {
        let s = s.trim();
        Unattended::ALL.into_iter().find(|u| u.as_str() == s)
    }
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

/// The default the room's quiet window takes when `[room] held_quiet_seconds` says nothing: ten
/// minutes, the operator's number for *long enough that it is not a turn per line, short enough
/// that a passing line is not lost for the session's life*.
pub const ROOM_QUIET_DEFAULT: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// `[room]` in a `leticode.toml`: the model that arbitrates the room's items, and how long a room
/// line nothing rang for is held before it is said as one digest turn. `tie_breaker: None` is
/// `room::coordinator::DEEPSEEK`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoomKeys {
    pub tie_breaker: Option<String>,
    /// Seconds a session may hold room lines nothing rang for before they are said as one digest.
    pub held_quiet_seconds: Option<u64>,
}

/// `[ui]` in a `leticode.toml`: how the head lays itself out. `rooms_sidebar` keeps the Rooms,
/// Sessions and Queues list beside the transcript; `language` is the head's own language - `auto`
/// follows the environment's `LC_ALL` / `LC_MESSAGES` / `LANG`. `None` is off, and English.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UiKeys {
    pub rooms_sidebar: Option<bool>,
    /// The sidebar's starting width in columns; a width the operator drags to wins over it.
    pub rooms_sidebar_width: Option<u16>,
    /// A language code (`en`, `ru`) or `auto`. Read by the head (`tui/src/app/prefs.rs`), which is
    /// where the catalogs are: a code this build does not carry is refused there, by name.
    pub language: Option<String>,
}

/// `[sessions]` in a `leticode.toml`. `auto_reseat: None` is on: a seat nobody answers moves to
/// the current prompt before its turn.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionKeys {
    pub auto_reseat: Option<bool>,
}

/// `[catalogue]` in a `leticode.toml`. `refresh: None` is on: the daemon fetches models.dev when
/// the cache is a day old, in the background, and asks nobody.
///
/// `false` is for a box whose network is metered, or one that pins its own catalogue where
/// nothing may overwrite it. `LETIBOT_MODELS_JSON` says the same thing more sharply - a file the
/// operator named is not one this box may replace - and is read by
/// `letibot_provider::refresh::wanted`, not here.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CatalogueKeys {
    pub refresh: Option<bool>,
}

/// `[peering]` in a `leticode.toml`: whether this box's room coordinator links to the other boxes
/// on its LAN, and what a project of this box does about being introduced to the same repository
/// elsewhere.
///
/// The two levels own different keys on purpose. Whether a box peers, and on what port, is the
/// OPERATOR's and lives in the user file: a cloned repository must not be able to open a port on
/// the box that reads it. Whether a repository is introduced is the PROJECT's, because only the
/// project knows what its seats are working on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PeeringKeys {
    /// `false` keeps a box that has a cookie file from peering at all. `None` is on.
    pub enabled: Option<bool>,
    /// The TCP port links arrive at; `None` lets the kernel pick one.
    pub port: Option<u16>,
    /// The multicast group the beacons go to.
    pub group: Option<String>,
    /// The UDP port the beacons go to, which every box on one LAN shares.
    pub group_port: Option<u16>,
    /// `false` opts THIS project out of introductions, in both directions. `None` is in.
    pub introduce: Option<bool>,
}

impl PeeringKeys {
    /// Is a cookie enough to peer? The user file's answer, and on unless it said `false`.
    pub fn peers(&self) -> bool {
        self.enabled.unwrap_or(true)
    }

    /// Is this project introduced to its repository elsewhere? On unless it said `false`.
    pub fn introduces(&self) -> bool {
        self.introduce.unwrap_or(true)
    }
}

/// What a spawn over the running-children cap does: wait for a slot, or fail with a sentence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhenFull {
    Queue,
    Refuse,
}

impl WhenFull {
    pub fn as_str(self) -> &'static str {
        match self {
            WhenFull::Queue => "queue",
            WhenFull::Refuse => "refuse",
        }
    }
}

/// Children a root session's tree may run at once when no level says, as dsh pools them.
pub const DEFAULT_MAX_RUNNING: usize = 8;
/// Children the whole daemon may run at once when the user file does not say, as grok and omp cap them.
pub const DEFAULT_DAEMON_MAX_RUNNING: usize = 32;

/// `[seat]` in a `leticode.toml`: where a session with no head runs its tools. See
/// [`SeatDefault`] for the two words and what each means.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SeatKeys {
    pub default: Option<SeatDefault>,
}

/// `[seat] default`: the placement a HEADLESS seat gets - a package, a subagent, a queue gate,
/// anything nobody sits in front of. An interactive top-level session is the host whatever this
/// says, and a caller that asks for a placement (`vm`, `--vm`, `--host`) gets what it asked for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SeatDefault {
    /// A firecode VM. The default: the VM is the boundary, so nothing inside it can reach this
    /// box, and a fallback to the host when this machine cannot start one is named.
    #[default]
    Vm,
    /// This machine, with the boundary its perimeter asks for.
    Host,
}

impl SeatDefault {
    pub fn as_str(self) -> &'static str {
        match self {
            SeatDefault::Vm => "vm",
            SeatDefault::Host => "host",
        }
    }

    /// The word back, or a refusal naming the two the file may say.
    pub fn parse(s: &str) -> Result<SeatDefault, String> {
        match s.trim() {
            "vm" | "firecode" => Ok(SeatDefault::Vm),
            "host" | "here" => Ok(SeatDefault::Host),
            other => Err(format!(
                "[seat] default is \"vm\" (a firecode VM: the VM is the boundary, and a session \
                 with no head runs there) or \"host\" (this machine, as it is) - not \"{other}\""
            )),
        }
    }
}

fn seat_keys(value: &toml::Value, path: &Path, text: &str, user: bool) -> Result<SeatKeys, String> {
    let keys = "the key is `default` (\"vm\" or \"host\")";
    let table = table_at(value, path, text, "seat", keys)?;
    let mut out = SeatKeys::default();
    for (key, v) in table {
        let from = at(path, text, key, user);
        match key.as_str() {
            "default" => {
                let word = v.as_str().ok_or_else(|| {
                    format!(
                        "{from}: [seat] default is \"vm\" or \"host\", not {}",
                        kind_of(v)
                    )
                })?;
                out.default =
                    Some(SeatDefault::parse(word).map_err(|why| format!("{from}: {why}"))?);
            }
            other => return Err(format!("{from}: [seat] `{other}` is not a key; {keys}")),
        }
    }
    Ok(out)
}

/// `[subagents]` in a `leticode.toml`. `daemon_max_running` is read from the user file only: a
/// project file is writable by the sessions it governs, and the daemon's cap is not theirs to lift.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SubagentKeys {
    pub max_running: Option<usize>,
    pub daemon_max_running: Option<usize>,
    pub when_full: Option<WhenFull>,
}

/// The resolved running-children limits a spawn is admitted against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChildLimits {
    pub per_tree: usize,
    pub per_daemon: usize,
    pub when_full: WhenFull,
}

fn subagent_keys(
    value: &toml::Value,
    path: &Path,
    text: &str,
    user: bool,
) -> Result<SubagentKeys, String> {
    let keys = "the keys are `max_running` (children one session's tree runs at once), \
                `daemon_max_running` (the whole daemon, user file only) and `when_full` \
                (\"queue\" or \"refuse\")";
    let table = table_at(value, path, text, "subagents", keys)?;
    let mut out = SubagentKeys::default();
    for (key, v) in table {
        let from = at(path, text, key, user);
        let count = || {
            v.as_integer()
                .and_then(|n| usize::try_from(n).ok())
                .filter(|n| *n > 0)
                .ok_or_else(|| {
                    format!(
                        "{from}: [subagents] {key} is a whole number of children above 0, not {}; \
                         the cap can be raised but not switched off",
                        match v.as_integer() {
                            Some(n) => n.to_string(),
                            None => kind_of(v).to_string(),
                        }
                    )
                })
        };
        match key.as_str() {
            "max_running" => out.max_running = Some(count()?),
            "daemon_max_running" if !user => {
                return Err(format!(
                    "{from}: [subagents] daemon_max_running is the daemon's cap and is read from \
                     the user file only; a project sets `max_running`"
                ));
            }
            "daemon_max_running" => out.daemon_max_running = Some(count()?),
            "when_full" => {
                out.when_full = Some(match v.as_str().map(str::trim) {
                    Some("queue") => WhenFull::Queue,
                    Some("refuse") => WhenFull::Refuse,
                    _ => {
                        return Err(format!(
                            "{from}: [subagents] when_full is \"queue\" (wait for a running child \
                             to finish) or \"refuse\" (fail the spawn), not {v}"
                        ));
                    }
                });
            }
            other => {
                return Err(format!(
                    "{from}: [subagents] `{other}` is not a key; {keys}"
                ));
            }
        }
    }
    Ok(out)
}

fn session_keys(
    value: &toml::Value,
    path: &Path,
    text: &str,
    user: bool,
) -> Result<SessionKeys, String> {
    let keys = "the key is `auto_reseat` (true or false)";
    let table = table_at(value, path, text, "sessions", keys)?;
    let mut out = SessionKeys::default();
    for (key, v) in table {
        let from = at(path, text, key, user);
        match key.as_str() {
            "auto_reseat" => {
                out.auto_reseat = Some(v.as_bool().ok_or_else(|| {
                    format!(
                        "{from}: [sessions] auto_reseat is true or false, not {}",
                        kind_of(v)
                    )
                })?);
            }
            other => return Err(format!("{from}: [sessions] `{other}` is not a key; {keys}")),
        }
    }
    Ok(out)
}

/// `[catalogue]` in a `leticode.toml`: whether this box keeps the model catalogue's cache fresh
/// by fetching models.dev itself. On when no level says otherwise.
fn catalogue_keys(
    value: &toml::Value,
    path: &Path,
    text: &str,
    user: bool,
) -> Result<CatalogueKeys, String> {
    let keys = "the key is `refresh` (true or false): whether this daemon fetches models.dev \
                itself, in the background, when the catalogue's cache is a day old";
    let table = table_at(value, path, text, "catalogue", keys)?;
    let mut out = CatalogueKeys::default();
    for (key, v) in table {
        let from = at(path, text, key, user);
        match key.as_str() {
            "refresh" => {
                out.refresh = Some(v.as_bool().ok_or_else(|| {
                    format!(
                        "{from}: [catalogue] refresh is true or false, not {}",
                        kind_of(v)
                    )
                })?);
            }
            other => {
                return Err(format!(
                    "{from}: [catalogue] `{other}` is not a key; {keys}"
                ));
            }
        }
    }
    Ok(out)
}

fn peering_keys(
    value: &toml::Value,
    path: &Path,
    text: &str,
    user: bool,
) -> Result<PeeringKeys, String> {
    let keys = if user {
        "the keys are `enabled` (true or false), `port` (the TCP port links arrive at), `group` \
         (an IPv4 multicast address) and `group_port` (the UDP port the beacons go to)"
    } else {
        "the key is `introduce` (true or false): whether this project is introduced to the same \
         repository on another box"
    };
    let table = table_at(value, path, text, "peering", keys)?;
    let mut out = PeeringKeys::default();
    for (key, v) in table {
        let from = at(path, text, key, user);
        let bool_at = |v: &toml::Value, what: &str| -> Result<bool, String> {
            v.as_bool().ok_or_else(|| {
                format!(
                    "{from}: [peering] {what} is true or false, not {}",
                    kind_of(v)
                )
            })
        };
        let port_at = |v: &toml::Value, what: &str| -> Result<u16, String> {
            let n = v.as_integer().ok_or_else(|| {
                format!(
                    "{from}: [peering] {what} is a whole number between 1 and 65535, not {}",
                    kind_of(v)
                )
            })?;
            u16::try_from(n).ok().filter(|p| *p > 0).ok_or_else(|| {
                format!("{from}: [peering] {what} is a whole number between 1 and 65535, not {n}")
            })
        };
        match (key.as_str(), user) {
            ("enabled", true) => out.enabled = Some(bool_at(v, "enabled")?),
            ("port", true) => out.port = Some(port_at(v, "port")?),
            ("group_port", true) => out.group_port = Some(port_at(v, "group_port")?),
            ("group", true) => {
                let text = v.as_str().map(str::trim).unwrap_or("");
                if text.parse::<std::net::Ipv4Addr>().is_err() {
                    return Err(format!(
                        "{from}: [peering] group is an IPv4 multicast address as a string \
                         (\"239.255.41.7\"), not {}",
                        kind_of(v)
                    ));
                }
                out.group = Some(text.to_string());
            }
            ("introduce", false) => out.introduce = Some(bool_at(v, "introduce")?),
            // The levels own different keys, and a key in the wrong file is refused by name
            // rather than read: a project file that could open a port is a repository deciding
            // what this box does on its LAN.
            ("introduce", true) => {
                return Err(format!(
                    "{from}: [peering] introduce is a project's key, not the user file's - it opts \
                     one repository out, and this file names no repository. Write it in the \
                     project's own leticode.toml"
                ));
            }
            (other @ ("enabled" | "port" | "group" | "group_port"), false) => {
                return Err(format!(
                    "{from}: [peering] `{other}` is the box's, not this project's: whether this box \
                     peers and on what port is the user file's to say. The project's key is \
                     `introduce`"
                ));
            }
            (other, _) => return Err(format!("{from}: [peering] `{other}` is not a key; {keys}")),
        }
    }
    Ok(out)
}

fn ui_keys(value: &toml::Value, path: &Path, text: &str, user: bool) -> Result<UiKeys, String> {
    let keys = "the keys are `rooms_sidebar` (true or false), `rooms_sidebar_width` (columns) and \
                `language` (a two-letter code, or `auto`)";
    let table = table_at(value, path, text, "ui", keys)?;
    let mut out = UiKeys::default();
    for (key, v) in table {
        let from = at(path, text, key, user);
        match key.as_str() {
            "language" => {
                let word = v.as_str().ok_or_else(|| {
                    format!("{from}: [ui] language is a word, not {}", kind_of(v))
                })?;
                // The SHAPE is checked here and the words are the head's: which languages this
                // build carries is a fact about its catalogs, and a list here would be a second
                // copy of it that can drift.
                let ok = word.len() >= 2
                    && word
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
                if !ok {
                    return Err(format!(
                        "{from}: [ui] language `{word}` is a language code (`ru`, `en`) or `auto`"
                    ));
                }
                out.language = Some(word.to_string());
            }
            "rooms_sidebar" => {
                out.rooms_sidebar = Some(v.as_bool().ok_or_else(|| {
                    format!(
                        "{from}: [ui] rooms_sidebar is true or false, not {}",
                        kind_of(v)
                    )
                })?);
            }
            "rooms_sidebar_width" => {
                out.rooms_sidebar_width = Some(
                    v.as_integer()
                        .and_then(|n| u16::try_from(n).ok())
                        .filter(|n| *n > 0)
                        .ok_or_else(|| {
                            format!(
                                "{from}: [ui] rooms_sidebar_width is a column count above 0, not {}",
                                kind_of(v)
                            )
                        })?,
                );
            }
            other => return Err(format!("{from}: [ui] `{other}` is not a key; {keys}")),
        }
    }
    Ok(out)
}

fn warning_keys(
    value: &toml::Value,
    path: &Path,
    text: &str,
    user: bool,
) -> Result<WarningKeys, String> {
    let keys = "the only key is `mute` (a list of warning codes)";
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
            other => {
                return Err(format!("{from}: [warnings] `{other}` is not a key; {keys}"));
            }
        }
    }
    Ok(out)
}

impl WarningKeys {
    /// The two levels as one: the mutes union, each once. See the module doc for why a mute is a
    /// union.
    pub fn overridden_by(&self, other: &WarningKeys) -> WarningKeys {
        let mut mute = self.mute.clone();
        for e in &other.mute {
            if !mute.iter().any(|m| m.value == e.value) {
                mute.push(e.clone());
            }
        }
        WarningKeys { mute }
    }
}

fn key_keys(value: &toml::Value, path: &Path, text: &str, user: bool) -> Result<KeyKeys, String> {
    let keys =
        "every key is `\"SURFACE.KEY\" = \"key\"` - a surface's binding moved to another key";
    let table = table_at(value, path, text, "keys", keys)?;
    let mut out = KeyKeys::default();
    for (name, v) in table {
        let from = at(path, text, name, user);
        if !name.contains('.') {
            return Err(format!(
                "{from}: [keys] `{name}` is not a key: it names no surface - a binding is \
                 written \"SURFACE.KEY\", as the hint bar spells it (`todos.d`)"
            ));
        }
        let key = v.as_str().and_then(non_empty).ok_or_else(|| match v.as_str() {
            Some(_) => format!(
                "{from}: [keys] `{name}` = \"\" names no key - write the key it becomes, e.g. \"f\""
            ),
            None => format!(
                "{from}: [keys] `{name}` is one key, as a string, not {}; {keys}",
                kind_of(v)
            ),
        })?;
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
    let keys = "the keys are `stuck_after_seconds`, `stuck_repeat_seconds` and `mute` - the \
                merge queue's census and the repositories it is silent about; the rest of `[queue]` is the queue's own reader";
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
            "mute" => {
                let items = v.as_array().ok_or_else(|| {
                    format!(
                        "{from}: [queue] mute is a list of repository names, as strings; {keys}"
                    )
                })?;
                for item in items {
                    let name = item.as_str().and_then(non_empty).ok_or_else(|| {
                        format!(
                            "{from}: [queue] mute holds {} - every entry is a repository's \
                             directory name or key, as a non-empty string",
                            match item.as_str() {
                                Some(s) => format!("{s:?}"),
                                None => kind_of(item).to_string(),
                            }
                        )
                    })?;
                    if !out.mute.contains(&name) {
                        out.mute.push(name);
                    }
                }
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

fn automode_keys(
    value: &toml::Value,
    path: &Path,
    text: &str,
    user: bool,
) -> Result<AutomodeKeys, String> {
    let keys = "the keys are `unattended` (`review`, `queue` or `refuse`), `blocks_in_a_row` and \
                `blocks_per_session`";
    let table = table_at(value, path, text, "automode", keys)?;
    let mut out = AutomodeKeys::default();
    for (key, v) in table {
        let from = at(path, text, key, user);
        match key.as_str() {
            "unattended" => {
                let word = v.as_str().map(str::trim).unwrap_or_default();
                if Unattended::parse(word).is_none() {
                    return Err(format!(
                        "{from}: [automode] unattended = {} is not one of {}",
                        match v.as_str() {
                            Some(w) => format!("{w:?}"),
                            None => kind_of(v).to_string(),
                        },
                        Unattended::ALL
                            .iter()
                            .map(|u| format!("`{}`", u.as_str()))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
                out.unattended = Some(word.to_string());
            }
            "blocks_in_a_row" => out.blocks_in_a_row = Some(count_at(v, &from, "automode", key)?),
            "blocks_per_session" => {
                out.blocks_per_session = Some(count_at(v, &from, "automode", key)?)
            }
            other => return Err(format!("{from}: [automode] `{other}` is not a key; {keys}")),
        }
    }
    Ok(out)
}

/// A positive whole number, for a count rather than a schedule.
fn count_at(v: &toml::Value, from: &Origin, table: &str, key: &str) -> Result<u32, String> {
    let n = v.as_integer().ok_or_else(|| {
        format!(
            "{from}: [{table}] {key} is a whole number, not {}",
            kind_of(v)
        )
    })?;
    u32::try_from(n)
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| format!("{from}: [{table}] {key} = {n} — a count of blocks is at least one"))
}

fn room_keys(value: &toml::Value, path: &Path, text: &str, user: bool) -> Result<RoomKeys, String> {
    let keys = "the keys are `tie_breaker` (the model that arbitrates the room's items) and \
                `held_quiet_seconds` (how long a held room line waits for its digest turn)";
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
            "held_quiet_seconds" => {
                out.held_quiet_seconds = Some(seconds_at(v, &from, "room", key)?)
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
        Self::parse_text(&text, path, level)
    }

    /// The same reader over bytes already in hand. `config propose` judges a proposal with THIS
    /// rather than with a second parser: a file the daemon would refuse is refused there, in the
    /// words it would have used, and `path` is only ever a name in those words.
    pub fn parse_text(text: &str, path: &Path, level: Level) -> Result<LeticodeConfig, String> {
        let file: LeticodeConfigFile =
            toml::from_str(text).map_err(|e| format!("{}: {e}", path.display()))?;

        let mut roles = BTreeMap::new();
        for (role, value) in &file.roles {
            let settings =
                role_settings(role, value).map_err(|why| format!("{}: {why}", path.display()))?;
            roles.insert(role.clone(), settings);
        }

        workspace::validate(file.project.as_ref(), path, text, level == Level::User)?;
        let mcp_projects_read = match &file.mcp_projects {
            Some(v) => mcp_projects(v, path, text, level == Level::User)?,
            None => Default::default(),
        };
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
                Some(v) => boundary_keys(v, path, text, level == Level::User)?,
                None => BoundaryConfig::default(),
            },
            perimeters: match &file.perimeter {
                Some(v) => perimeter_tables(v, path, text, level == Level::User)?,
                None => BTreeMap::new(),
            },
            fallback: Vec::new(),
            fallback_for: BTreeMap::new(),
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
            browser: match &file.browser {
                Some(v) => browser_keys(v).map_err(|why| format!("{}: {why}", path.display()))?,
                None => BrowserKeys::default(),
            },
            exec: match &file.exec {
                Some(v) => exec_keys(v).map_err(|why| format!("{}: {why}", path.display()))?,
                None => ExecKeys::default(),
            },
            tools: match &file.tools {
                Some(v) => tools_keys(v).map_err(|why| format!("{}: {why}", path.display()))?,
                None => ToolKeys::default(),
            },
            output: match &file.output {
                Some(v) => output_keys(v).map_err(|why| format!("{}: {why}", path.display()))?,
                None => OutputKeys::default(),
            },
            mcp: match &file.mcp {
                Some(v) => mcp_tables(v, path, text, level == Level::User)?,
                None => BTreeMap::new(),
            },
            mcp_trusted: mcp_projects_read.0,
            mcp_pinned: mcp_projects_read.1,
            provider: match &file.provider {
                Some(v) => provider_keys(v, path, text, level == Level::User)?,
                None => ProviderKeys::default(),
            },
            // The six tables below refuse with their own file and line (`{from}`), so their
            // message is the whole sentence and there is nothing to prefix.
            nag: match &file.nag {
                Some(v) => nag_keys(v, path, text, level == Level::User)?,
                None => NagKeys::default(),
            },
            automode: match &file.automode {
                Some(v) => automode_keys(v, path, text, level == Level::User)?,
                None => AutomodeKeys::default(),
            },
            catalogue: match &file.catalogue {
                Some(v) => catalogue_keys(v, path, text, level == Level::User)?,
                None => CatalogueKeys::default(),
            },
            queue: match &file.queue {
                Some(v) => queue_keys(v, path, text, level == Level::User)?,
                None => QueueKeys::default(),
            },
            room: match &file.room {
                Some(v) => room_keys(v, path, text, level == Level::User)?,
                None => RoomKeys::default(),
            },
            peering: match &file.peering {
                Some(v) => peering_keys(v, path, text, level == Level::User)?,
                None => PeeringKeys::default(),
            },
            ui: match &file.ui {
                Some(v) => ui_keys(v, path, text, level == Level::User)?,
                None => UiKeys::default(),
            },
            sessions: match &file.sessions {
                Some(v) => session_keys(v, path, text, level == Level::User)?,
                None => SessionKeys::default(),
            },
            seat: match &file.seat {
                Some(v) => seat_keys(v, path, text, level == Level::User)?,
                None => SeatKeys::default(),
            },
            subagents: match &file.subagents {
                Some(v) => subagent_keys(v, path, text, level == Level::User)?,
                None => SubagentKeys::default(),
            },
            warnings: match &file.warnings {
                Some(v) => warning_keys(v, path, text, level == Level::User)?,
                None => WarningKeys::default(),
            },
            keys: match &file.keys {
                Some(v) => key_keys(v, path, text, level == Level::User)?,
                None => KeyKeys::default(),
            },
            verbosity: match &file.verbosity {
                Some(v) => verbosity_keys(v, path, text, level == Level::User)?,
                None => VerbosityKeys::default(),
            },
        };

        if let Some(v) = &file.fallback {
            (config.fallback, config.fallback_for) = fallback_keys(v, path, text)?;
        }

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
            // The whole table goes with the level that wrote any of it, as the list always did.
            fallback: if other.has_fallback() {
                other.fallback.clone()
            } else {
                self.fallback.clone()
            },
            fallback_for: if other.has_fallback() {
                other.fallback_for.clone()
            } else {
                self.fallback_for.clone()
            },
            fallback_after_timeouts: other
                .fallback_after_timeouts
                .or(self.fallback_after_timeouts),
            // The user file's only: a project file starting processes would run whatever a
            // cloned repository names.
            mcp: self.mcp.clone(),
            mcp_trusted: self.mcp_trusted.clone(),
            mcp_pinned: self.mcp_pinned.clone(),
            output: OutputKeys {
                inline_bytes: other.output.inline_bytes.or(self.output.inline_bytes),
                inline_lines: other.output.inline_lines.or(self.output.inline_lines),
                spill_budget: other.output.spill_budget.or(self.output.spill_budget),
            },
            tools: ToolKeys {
                parallel: other.tools.parallel.or(self.tools.parallel),
            },
            exec: ExecKeys {
                auto_background: other.exec.auto_background.or(self.exec.auto_background),
                foreground_seconds: other
                    .exec
                    .foreground_seconds
                    .or(self.exec.foreground_seconds),
                job_dead_after_seconds: other
                    .exec
                    .job_dead_after_seconds
                    .or(self.exec.job_dead_after_seconds),
                io_wbps: other.exec.io_wbps.or(self.exec.io_wbps),
                io_wiops: other.exec.io_wiops.or(self.exec.io_wiops),
                disk_floor: other.exec.disk_floor.or(self.exec.disk_floor),
                disk_floor_action: other.exec.disk_floor_action.or(self.exec.disk_floor_action),
                io_pressure_full: other.exec.io_pressure_full.or(self.exec.io_pressure_full),
                tasks_max: other.exec.tasks_max.or(self.exec.tasks_max),
                disk_watch: other
                    .exec
                    .disk_watch
                    .clone()
                    .or_else(|| self.exec.disk_watch.clone()),
            },
            // The user's only: see [`BrowserKeys`].
            browser: if other.level == Level::User {
                other.browser.clone()
            } else if self.level == Level::User {
                self.browser.clone()
            } else {
                BrowserKeys::default()
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
                wait_if_within: other
                    .provider
                    .wait_if_within
                    .or(self.provider.wait_if_within),
                limit_probe: other
                    .provider
                    .limit_probe
                    .clone()
                    .or_else(|| self.provider.limit_probe.clone()),
                offline_budget: other
                    .provider
                    .offline_budget
                    .or(self.provider.offline_budget),
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
                usage_warn: self
                    .provider
                    .usage_warn
                    .overridden_by(&other.provider.usage_warn),
                usage_warn_presets: {
                    let mut m = self.provider.usage_warn_presets.clone();
                    for (name, k) in &other.provider.usage_warn_presets {
                        let base = m.get(name).cloned().unwrap_or_default();
                        m.insert(name.clone(), base.overridden_by(k));
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
            automode: AutomodeKeys {
                unattended: other
                    .automode
                    .unattended
                    .clone()
                    .or(self.automode.unattended.clone()),
                blocks_in_a_row: other
                    .automode
                    .blocks_in_a_row
                    .or(self.automode.blocks_in_a_row),
                blocks_per_session: other
                    .automode
                    .blocks_per_session
                    .or(self.automode.blocks_per_session),
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
                mute: {
                    let mut m = self.queue.mute.clone();
                    for n in &other.queue.mute {
                        if !m.contains(n) {
                            m.push(n.clone());
                        }
                    }
                    m
                },
            },
            room: RoomKeys {
                tie_breaker: other
                    .room
                    .tie_breaker
                    .clone()
                    .or_else(|| self.room.tie_breaker.clone()),
                held_quiet_seconds: other
                    .room
                    .held_quiet_seconds
                    .or(self.room.held_quiet_seconds),
            },
            // The box's settings are the user file's and the repository's is the project's, so
            // each level keeps what it alone may write.
            peering: PeeringKeys {
                enabled: self.peering.enabled,
                port: self.peering.port,
                group: self.peering.group.clone(),
                group_port: self.peering.group_port,
                introduce: other.peering.introduce,
            },
            ui: UiKeys {
                rooms_sidebar: other.ui.rooms_sidebar.or(self.ui.rooms_sidebar),
                rooms_sidebar_width: other.ui.rooms_sidebar_width.or(self.ui.rooms_sidebar_width),
                // A project may set its own language: this is the head's own reading, and a
                // repository that says `ru` is saying it for the people who work in it.
                language: other.ui.language.clone().or(self.ui.language.clone()),
            },
            sessions: SessionKeys {
                auto_reseat: other.sessions.auto_reseat.or(self.sessions.auto_reseat),
            },
            // The project's word wins: a repository whose seats are the ones being placed is
            // the one that knows whether a VM suits it.
            seat: SeatKeys {
                default: other.seat.default.or(self.seat.default),
            },
            catalogue: CatalogueKeys {
                refresh: other.catalogue.refresh.or(self.catalogue.refresh),
            },
            subagents: SubagentKeys {
                max_running: other.subagents.max_running.or(self.subagents.max_running),
                daemon_max_running: other
                    .subagents
                    .daemon_max_running
                    .or(self.subagents.daemon_max_running),
                when_full: other.subagents.when_full.or(self.subagents.when_full),
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

    /// The `[queue] mute` names, both levels unioned.
    pub fn queue_mute(&self) -> &[String] {
        &self.queue.mute
    }

    /// Whether a seat nobody answers reseats itself on a stale prompt: `[sessions] auto_reseat`,
    /// on by default.
    pub fn auto_reseat(&self) -> bool {
        self.sessions.auto_reseat.unwrap_or(true)
    }

    /// Whether this daemon keeps the catalogue's cache fresh by itself: `[catalogue] refresh`, on
    /// when no level says otherwise. The operator's `LETIBOT_MODELS_JSON` is not read here - it
    /// names a file this box may not replace, and `letibot_provider::refresh::wanted` is where the
    /// two answers meet.
    pub fn catalogue_refresh(&self) -> bool {
        self.catalogue.refresh.unwrap_or(true)
    }

    /// Whether a round's adjacent read-class calls run at once: `[tools] parallel`, on by default,
    /// and `false` is when every call runs alone in the order the model proposed it.
    pub fn parallel_reads(&self) -> bool {
        self.tools.parallel.unwrap_or(true)
    }

    /// The running-children limits: `[subagents]` over the built-in 8 per tree, 32 per daemon, queue.
    pub fn child_limits(&self) -> ChildLimits {
        ChildLimits {
            per_tree: self.subagents.max_running.unwrap_or(DEFAULT_MAX_RUNNING),
            per_daemon: self
                .subagents
                .daemon_max_running
                .unwrap_or(DEFAULT_DAEMON_MAX_RUNNING),
            when_full: self.subagents.when_full.unwrap_or(WhenFull::Queue),
        }
    }

    /// How long a session may hold room lines nothing rang for before they are said as one digest
    /// turn: `[room] held_quiet_seconds`, or [`ROOM_QUIET_DEFAULT`].
    pub fn held_room_quiet(&self) -> std::time::Duration {
        self.room
            .held_quiet_seconds
            .map(std::time::Duration::from_secs)
            .unwrap_or(ROOM_QUIET_DEFAULT)
    }

    /// What this session does when its provider answers a usage limit with a known reset: the
    /// file's own word, else `switch` when a fallback list exists, else `wait`. Failing on a limit
    /// the provider put a time on would throw the turn away for nothing.
    pub fn limit_policy(&self) -> OnLimit {
        self.provider.on_limit.unwrap_or(if self.has_fallback() {
            OnLimit::Switch
        } else {
            OnLimit::Wait
        })
    }

    /// Did this level name any fallback - the list, or any key of the `[fallback]` table.
    pub fn has_fallback(&self) -> bool {
        !self.fallback.is_empty() || !self.fallback_for.is_empty()
    }

    /// How close a reset must be for `switch` to wait for it anyway: the file's span, else
    /// [`DEFAULT_WAIT_IF_WITHIN`].
    pub fn wait_if_within(&self) -> std::time::Duration {
        self.provider
            .wait_if_within
            .unwrap_or(DEFAULT_WAIT_IF_WITHIN)
    }

    /// The steps a park re-tries its request on before the reset: the file's list, else
    /// [`DEFAULT_LIMIT_PROBE`].
    pub fn limit_probe(&self) -> Vec<std::time::Duration> {
        self.provider
            .limit_probe
            .clone()
            .unwrap_or_else(|| DEFAULT_LIMIT_PROBE.to_vec())
    }

    /// How long a turn keeps retrying a no-network failure: the file's span, or `None` when no level
    /// said, which leaves the daemon's own hour in place.
    pub fn offline_budget(&self) -> Option<std::time::Duration> {
        self.provider.offline_budget
    }

    /// The usage thresholds for a session on preset `name`: per key, `[provider.name.usage_warn]`,
    /// then `[provider.usage_warn]`, then the built-in 80/95 for 5h and 70/95 for weekly and monthly.
    pub fn usage_warn(&self, name: &str) -> letibot_provider::usage_warn::Thresholds {
        let p = &self.provider;
        let keys = match p.usage_warn_presets.get(name) {
            Some(k) => p.usage_warn.overridden_by(k),
            None => p.usage_warn.clone(),
        };
        let d = letibot_provider::usage_warn::Thresholds::default();
        letibot_provider::usage_warn::Thresholds {
            five_h: keys.five_h.unwrap_or(d.five_h),
            weekly: keys.weekly.unwrap_or(d.weekly),
            monthly: keys.monthly.unwrap_or(d.monthly),
        }
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
        for (key, list) in &self.fallback_for {
            out.push(format!("fallback for {key} {}", list.join(", ")));
        }
        // And the answer to a limit, when the file gave one: an absent key is the daemon's own
        // default, which `limit_policy` states, and the disclosure must not claim a setting nobody
        // made.
        if let Some(o) = self.provider.on_limit {
            out.push(format!("on_limit {}", o.as_str()));
        }
        if let Some(d) = self.provider.wait_if_within {
            out.push(format!("wait_if_within {}", span_words(d)));
        }
        if let Some(steps) = &self.provider.limit_probe {
            let words: Vec<String> = steps.iter().map(|d| span_words(*d)).collect();
            out.push(format!("limit_probe [{}]", words.join(", ")));
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
    /// The `[automode]` table, see [`AutomodeKeys`].
    #[serde(default)]
    automode: Option<toml::Value>,
    /// The `[room]` table, see [`RoomKeys`].
    #[serde(default)]
    room: Option<toml::Value>,
    /// The `[catalogue]` table, see [`CatalogueKeys`].
    #[serde(default)]
    catalogue: Option<toml::Value>,
    /// The `[peering]` table, see [`PeeringKeys`].
    #[serde(default)]
    peering: Option<toml::Value>,
    /// The `[ui]` table, see [`UiKeys`].
    #[serde(default)]
    ui: Option<toml::Value>,
    /// The `[sessions]` table, see [`SessionKeys`].
    #[serde(default)]
    sessions: Option<toml::Value>,
    /// The `[seat]` table, see [`SeatKeys`].
    #[serde(default)]
    seat: Option<toml::Value>,
    /// The `[subagents]` table, see [`SubagentKeys`].
    #[serde(default)]
    subagents: Option<toml::Value>,
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
    /// The `[tools]` table, see [`ToolKeys`].
    #[serde(default)]
    tools: Option<toml::Value>,
    /// The `[output]` table, see [`OutputKeys`].
    #[serde(default)]
    output: Option<toml::Value>,
    /// The `[mcp.NAME]` tables, see [`McpServerKeys`].
    #[serde(default)]
    mcp: Option<toml::Value>,
    /// The `[mcp_projects]` table, see [`mcp_trusts`].
    #[serde(default)]
    mcp_projects: Option<toml::Value>,
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
    /// The `[wordfreq]` table, read with `[embeddings]` by harnessd: accepted for its reason.
    #[serde(default)]
    #[allow(dead_code)]
    wordfreq: Option<toml::Value>,
    /// The `[bridge]` table, read by `letibot_bridge::settings`. Accepted and not interpreted: it
    /// seats no tool, and a refusal would reject a user file the bridge wrote a key into.
    #[serde(default)]
    #[allow(dead_code)]
    bridge: Option<toml::Value>,
    /// The `[gc]` and `[docker]` tables, read by `letibot_tools::lifecycle::config` and
    /// `letibot_tools::docker::config`. Accepted and not interpreted: neither seats a tool nor
    /// sets a model, and `deny_unknown_fields` would otherwise refuse a file with `[gc] root`.
    #[serde(default)]
    #[allow(dead_code)]
    gc: Option<toml::Value>,
    #[serde(default)]
    #[allow(dead_code)]
    docker: Option<toml::Value>,
    /// `[firecode]`, read by `letibot_tools::lifecycle::config`; accepted for `[gc]`'s reason.
    #[serde(default)]
    #[allow(dead_code)]
    firecode: Option<toml::Value>,
    /// `[dotenv]`, read by `letibot_tools::dotenv::Settings`; accepted for `[gc]`'s reason, and
    /// because the host list a placement may carry `.env` files to is the operator's to give.
    #[serde(default)]
    #[allow(dead_code)]
    dotenv: Option<toml::Value>,
    /// The `[notes]` table, see [`NotesKeys`].
    #[serde(default)]
    notes: Option<toml::Value>,
    /// The `[browser]` table, see [`BrowserKeys`].
    #[serde(default)]
    browser: Option<toml::Value>,
    /// `[project] repository = false`, read by [`workspace`]; validated here so a wrong shape is
    /// refused with its line.
    #[serde(default)]
    project: Option<toml::Value>,
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

/// `[fallback]` keys other than `"*"`, each to its list.
pub type FallbackTable = BTreeMap<String, Vec<String>>;

/// The top-level `fallback`: a list (the `"*"` key) or a `[fallback]` table keyed by the limited
/// model. Returns the `"*"` list and the other keys. A bad key or an empty list is refused with
/// its line, since an empty list would read as "no fallback" for a model somebody named.
fn fallback_keys(
    value: &toml::Value,
    path: &Path,
    text: &str,
) -> Result<(Vec<String>, FallbackTable), String> {
    let toml::Value::Table(table) = value else {
        let list = fallback_list(value).map_err(|why| format!("{}: {why}", path.display()))?;
        return Ok((list, BTreeMap::new()));
    };
    let (mut star, mut out) = (Vec::new(), BTreeMap::new());
    for (key, v) in table {
        let line = line_after(text, "[fallback]", &format!("\"{key}\""));
        let at = |why: String| format!("{}:{line}: [fallback] \"{key}\" {why}", path.display());
        if !fallback_key_ok(key) {
            return Err(at(
                "is not a key: write a model as `/models` spells it (`claude/opus-5-5`), a \
                 provider's every model (`kimi-code-global/*`), or `*`"
                    .into(),
            ));
        }
        let list = fallback_list(v).map_err(|why| at(format!("- {why}")))?;
        if list.is_empty() {
            return Err(at(
                "is an empty list; a key with no models is no fallback, so leave it out".into(),
            ));
        }
        if key == "*" {
            star = list;
        } else {
            out.insert(key.clone(), list);
        }
    }
    Ok((star, out))
}

/// `*`, `provider/*`, or a model name with no `*` and no blank in it.
fn fallback_key_ok(key: &str) -> bool {
    if key == "*" {
        return true;
    }
    let name = key.strip_suffix("/*").unwrap_or(key);
    !name.is_empty() && !name.contains('*') && !name.chars().any(char::is_whitespace)
}

/// The list a session on `model` falls back along: its own key, then `provider/*`, then `"*"`
/// (`star`). Most specific wins whole - the lists are never joined.
pub fn fallback_for<'a>(
    table: &'a BTreeMap<String, Vec<String>>,
    star: &'a [String],
    model: &str,
) -> &'a [String] {
    by_model(table, model).map_or(star, Vec::as_slice)
}

/// The entry for `model` in a table keyed by model: its own key, then `provider/*`, then `*`.
/// Shared by `[fallback]` and the providers.toml `[nudge]` table so both pick the same way.
pub fn by_model<'a, T>(table: &'a BTreeMap<String, T>, model: &str) -> Option<&'a T> {
    by_model_keyed(table, model).map(|(_, v)| v)
}

/// [`by_model`] with the key that matched, for a disclosure that names it.
pub fn by_model_keyed<'a, T>(
    table: &'a BTreeMap<String, T>,
    model: &str,
) -> Option<(&'a str, &'a T)> {
    let provider = model.split_once('/').map_or(model, |(p, _)| p);
    [model.to_string(), format!("{provider}/*"), "*".to_string()]
        .into_iter()
        .find_map(|k| table.get_key_value(&k))
        .map(|(k, v)| (k.as_str(), v))
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
mod reference_tests;
#[cfg(test)]
#[cfg(test)]
mod tests;
