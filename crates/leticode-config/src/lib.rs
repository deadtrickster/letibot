//! The `leticode.toml` levels: which models answer in this project and how its subagents sample,
//! plus the same file once per user so "on this box, always my local glm" is one line in one place.
//!
//! It is a `.env` for models, found the way `.env` and `.git` are: by walking up from where you
//! stand, so a session started in a subdirectory still finds the project's settings.
//!
//! The file is `leticode.toml` at the project root or in `~/.config/letibot/`, visible rather than
//! hidden. A file that changes which model answers you should not be a surprise, and a hidden one
//! that picks the model is a setting that is there and does not look like it is.
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
//! An absent key means the daemon's own default for that model, and the startup disclosure says
//! which models the file set, so "where did this model come from" is answerable from the screen.
//!
//! A role sets sampler parameters and nothing that describes the weights or the room. The values
//! measured on the box's dense78 preset are the standing defaults for a subagent
//! ([`RoleSampling::dense78`]), so a project that writes nothing gets subagents that do not
//! overthink. Copying a window from one box onto a child running elsewhere is the defect
//! `child_window_refusal` exists for, so `window` and `max_tokens` are refused by name
//! ([`WINDOW_AND_CAP_KEYS`]) and the refusal says why.
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
//! Two levels, one file, one parser: the project's file, found by walking up from the session's
//! workspace, and the user's at one fixed path beside `providers.toml`. [`LeticodeConfig::load`]
//! serves both and [`load_user`] is the same call at the fixed path, because two readers of one file
//! shape is how the two files start disagreeing about what a key means. A user file is a project
//! file that happens to sit in your home: same keys, same refusals, same wall.
//!
//! The user level is not `[default]` in `providers.toml`, which looks like the answer and is not:
//!
//! ```toml
//! # ~/.config/letibot/providers.toml — written by `/models deepseek`
//! [default]
//! provider = "deepseek"
//! model = "deepseek-flash"
//! ```
//!
//! That block is read into `cfg.provider` only when nothing else set one, and `--model`, which the
//! launcher passes on every start, leaves it unset. So a file-level `[default]` outranked a
//! command-line model, and `letibot --glm` answered on deepseek. It can also only name a preset,
//! while `main_model` speaks both a `provider/model` for a meter and a bare alias for the local
//! server, which is the thing the operator wanted to say.
//!
//! The precedence: the command line beats the project file, the project file beats the user file, the
//! user file beats the built-in default, and an unset key falls through. It lives in [`precedence`]
//! rather than in a rule remembered in four places, because precedence that is not stated is
//! precedence nobody can reason about.
//!
//! `--model` alone is not the flag level: the launcher passes it on every start and it is the local
//! alias whose vocabulary the daemon submits ids against, not a choice of who answers. Reading it as
//! a choice would make both files unreachable for every launcher-started daemon. `--provider` is the
//! flag that says who answers. A file's bare alias does not overwrite the alias `--model` bound, since
//! a mismatch is `400 Prompt contains invalid tokens` at the first turn; it still clears the provider,
//! and a disagreement is named at startup rather than resolved in silence.
//!
//! Models are configuration; permissions are not. The file deserialises into [`LeticodeConfigFile`],
//! four model keys and a `[roles]` table, with `deny_unknown_fields` on the top level, so a typo or an
//! `allow_bash = true` is refused by name rather than ignored: a silently ignored key is the bug class
//! being fixed, and a silently widened capability is the worse of the two.
//!
//! The reader says the fault rather than swallowing it. A file that will not parse names the file, the
//! line and what the parser expected; a key the version does not know is named; and on success the
//! startup disclosure says which models and parameters the file set. A file that does not parse does
//! not take the session down at either level: the daemon runs on its own defaults and says so.
//!
//! `leticode.toml` says who answers, and the same layer decides which markdown a leticode reads as its
//! instructions, [`INSTRUCTION_FILE`] or the canonical [`CANONICAL_INSTRUCTION_FILE`] when a project
//! wrote that one, because that is a decision about a seat rather than about the runtime serving it.
//! The runtime asks; the two names live in the `instructions` module and nowhere in the daemon.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub mod instructions;

// **The seat's instruction files, at the crate root** — the daemon writes
// `leticode_config::instructions(..)` rather than a module path, because what it
// is asking for is the answer and not the module that holds it.
pub use instructions::{BoxDirs, CANONICAL_INSTRUCTION_FILE, INSTRUCTION_FILE, instructions};

/// The file's name, at the project root and in `~/.config/letibot/`. Visible,
/// not a dotfile: a file that changes which model answers you should not be a
/// surprise.
pub const FILE_NAME: &str = "leticode.toml";

/// The user level's path, derived from the provider crate's config-dir answer rather than from
/// `$HOME` spelled again, and fixed with no walk-up: "mine" does not depend on where you stand.
pub fn user_path() -> PathBuf {
    letibot_provider::keys::config_file()
        .parent()
        .map(|p| p.join(FILE_NAME))
        .unwrap_or_else(|| PathBuf::from(FILE_NAME))
}

/// Load the user level at [`user_path`]: the same call as [`LeticodeConfig::load`], because
/// it is the same file shape. A missing file is `Ok` with no models.
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

    /// The level as a sentence names it, each containing its own word from [`Level::as_str`] because
    /// that word is what a reader scans for.
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

/// The role every spawn lands on when the `task` call named no other seat: a seat's own table refines
/// this one, so a project that wants every child to stop overthinking says it once.
pub const SUBAGENT_ROLE: &str = "subagent";

/// The sampler keys a role's table may set. The same five names `providers.toml`'s `[model.*]` blocks
/// use, so one vocabulary covers both files.
pub const SAMPLER_KEYS: &[&str] = &["temperature", "top_p", "top_k", "min_p", "presence_penalty"];

/// The keys a role's table may not set: a window and a reply cap belong to the box and to the
/// call. Naming them says "this file does not decide it" rather than "unknown key".
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

/// How one role samples: five keys, each an `Option` so "the file said nothing" stays a
/// different fact from "the file said zero". No window and no cap, because
/// [`WINDOW_AND_CAP_KEYS`] are refused at the parser.
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
    /// **The operator's own numbers, measured on dense78**: the standing defaults for a subagent, so a
    /// project that writes no `leticode.toml` still gets children that do not overthink.
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

/// One `[roles.X]` entry: the model that role runs on, and how it samples. Either alone is
/// complete -- a role that sets only samplers runs on the model it would have anyway.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RoleSettings {
    /// The model this role runs on. `None` is the daemon's own default for it.
    pub model: Option<String>,
    /// How it samples. Empty is the daemon's own.
    pub sampling: RoleSampling,
}

/// The model configuration one level carries. `None` means the level below, never a guess, and
/// the struct is the whole of what a file may set -- so it cannot widen a capability.
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
    /// The adjudicator's model. In this build the guard and the adjudicator are one oracle
    /// seat, so this and `gatekeeper_model` name the same thing and the file may use either.
    pub judge_model: Option<String>,
    /// The `[roles]` section: a named-role override, so a future role needs no
    /// new key. A role name maps to its model and its samplers.
    pub roles: BTreeMap<String, RoleSettings>,
    /// The path the config was read from, for the disclosure. `None` when no
    /// file was found, which is the absent-file case: the level below, and
    /// nothing to name.
    pub path: Option<PathBuf>,
    /// Which level this is, carried on the value rather than passed alongside it so the
    /// disclosure cannot be handed a config and the wrong level for it.
    pub level: Level,
    /// **The `[memory]` table** — a tool call's default cap and ceiling for sessions in this
    /// project (TODO.md R76 item 7), over the box-wide `providers.toml` numbers. See
    /// [`memory_policy`].
    pub memory: MemoryKeys,
    /// **The `[permissions]` table** — currently one key, `ask_unconfined_code`. See
    /// [`PermissionKeys`].
    pub permissions: PermissionKeys,
}

/// **`[permissions]` in a `leticode.toml`**: the gate settings a file may carry, which is to say the
/// ones that make the gate ask more.
///
/// ```toml
/// [permissions]
/// ask_unconfined_code = true   # R98(1): a host with no boundary asks about every
///                              # test runner, build and project script, every time
/// ```
///
/// One key, and it is off unless a file says otherwise: a gate that asks about every build on an
/// unconfined box does not buy safety, it buys `allow-all`, so the default is the behaviour that
/// predates the key.
///
/// Either level may turn it on and neither can turn it off. The merge is `or`, because a project file
/// (which a session with `write` can edit) must not loosen what the user asked for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PermissionKeys {
    pub ask_unconfined_code: Option<bool>,
}

impl PermissionKeys {
    /// The two levels as one, `or` per key — see the type's own note on why it is not `over`.
    pub fn overridden_by(&self, other: &PermissionKeys) -> PermissionKeys {
        PermissionKeys {
            ask_unconfined_code: match (self.ask_unconfined_code, other.ask_unconfined_code) {
                (Some(a), Some(b)) => Some(a || b),
                (a, b) => a.or(b),
            },
        }
    }

    /// **Whether R98(1) is in force**: a host with no boundary asks about the project's own
    /// code, every time. `false` for a file that said nothing, which is every file that does
    /// not carry the key.
    pub fn ask_unconfined_code(&self) -> bool {
        self.ask_unconfined_code.unwrap_or(false)
    }

    /// Whether the file said anything at all — for the disclosure, which must not claim a
    /// setting nobody made.
    pub fn is_empty(&self) -> bool {
        self.ask_unconfined_code.is_none()
    }
}

/// The `[permissions]` table as the file wrote it; unknown keys refused by name.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PermissionTable {
    #[serde(default)]
    ask_unconfined_code: Option<bool>,
}

fn permission_keys(value: &toml::Value) -> Result<PermissionKeys, String> {
    let t: PermissionTable = value.clone().try_into().map_err(|e| {
        format!(
            "[permissions]: {e} — the one key is `ask_unconfined_code` (true makes a host with \
             no confinement ask about every test runner, build and project script)"
        )
    })?;
    Ok(PermissionKeys {
        ask_unconfined_code: t.ask_unconfined_code,
    })
}

/// **`[memory]` in a `leticode.toml`**: the two numbers `providers.toml` sets box-wide, set for one
/// project or for every project of this user.
///
/// ```toml
/// [memory]
/// default = "4G"     # what a `bash` call that names no `memory` runs under
/// ceiling = "0.6"    # the most a call may take without asking the operator
/// ```
///
/// The box-wide cap is right for `ls` and `grep` everywhere; a project that builds says so in its own
/// file and nothing else on the box moves. `None` is the level below, and a value that is not an
/// `Amount` refuses the file by name.
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

/// The memory policy a session's `bash` runs under: box-wide, then the user's file, then the
/// nearest project file. A file may narrow the ceiling and not raise it, because the ceiling is
/// where the operator is asked and a file in the repository is one any session can edit.
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

impl LeticodeConfig {
    /// The walk-up: the nearest `leticode.toml` at or above `workspace`. `None` is the honest
    /// answer for a workspace with no file above it.
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

    /// Load one level. A missing file is `Ok` with no models; a file that will not parse, names
    /// an unknown key, names two models for one seat or sets a capability is `Err` with the
    /// parser's message and the path.
    pub fn load(path: &Path) -> Result<LeticodeConfig, String> {
        Self::load_as(path, Level::Project)
    }

    /// [`LeticodeConfig::load`], with the level the value carries named by the caller. The path is the
    /// only thing that differs between the levels, so both go through this one reader.
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
            memory: match &file.memory {
                Some(v) => memory_keys(v).map_err(|why| format!("{}: {why}", path.display()))?,
                None => MemoryKeys::default(),
            },
            permissions: match &file.permissions {
                Some(v) => {
                    permission_keys(v).map_err(|why| format!("{}: {why}", path.display()))?
                }
                None => PermissionKeys::default(),
            },
        };

        // One seat, two answers: the guard and the adjudicator are one oracle here, so a file
        // naming two DIFFERENT models is the file disagreeing with itself about who guards. It is
        // refused rather than resolved by whichever line the reader applied last.
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
    /// default for that role — stated, not guessed.
    pub fn role_model(&self, role: &str) -> Option<&str> {
        self.roles.get(role).and_then(|r| r.model.as_deref())
    }

    /// The model a spawn of `role` runs on when the `task` call named none: the seat's own
    /// table, then `subagent_model`, then the caller's standing answer. Without this reader both
    /// keys were parsed, disclosed and read by nobody.
    pub fn spawn_model(&self, role: &str) -> Option<&str> {
        self.role_model(role).or(self.subagent_model.as_deref())
    }

    /// How a spawn of `role` samples: dense78 as the floor, then `[roles.subagent]`, then the
    /// seat's own table, key by key. Never empty, so a project that says nothing still gets
    /// children that do not overthink.
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

    /// The two file levels as one, the project's over the user's, key by key. `path` and `level`
    /// come from the higher one, so the merged value names the file that spoke most recently.
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
            permissions: self.permissions.overridden_by(&other.permissions),
        }
    }

    /// Which models the file set, as the disclosure says them. Empty when the file set nothing, which
    /// is the absent-file case: a daemon with no file at this level discloses nothing from it.
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

/// One `[roles.X]` entry as the file wrote it. The keys are checked here rather than by serde,
/// whose report for a table it cannot deserialise names the table and not the offending line.
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
        // Samplers, or a refusal that says which of the two kinds of mistake this is: an unknown key
        // names itself, and a window or a cap names itself and says where that number belongs.
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

/// The file's shape: `deny_unknown_fields` is the report for a key this version does not know.
/// Four model keys and a table of role settings, with no field for a seat, a tool list or a
/// window -- the shape is the capability boundary.
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
    /// The `[queue]` table: the queues' per-project settings, read by their own parser. Accepted
    /// here so this reader does not refuse a file for carrying it.
    #[serde(default)]
    #[allow(dead_code)]
    queue: Option<toml::Value>,
    /// **The `[memory]` table**, see [`MemoryKeys`]. A cap is not a capability: it narrows what a
    /// call may hold, and the ceiling above which the operator is asked still holds.
    #[serde(default)]
    memory: Option<toml::Value>,
    /// **The `[permissions]` table**, see [`PermissionKeys`]. A key that makes the gate ask more
    /// cannot widen a capability, so it belongs on this side of the wall rather than beside the
    /// models.
    #[serde(default)]
    permissions: Option<toml::Value>,
}

/// A model name that is blank is no model at all: an empty string would otherwise reach a call as a
/// model named `""`.
fn non_empty(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// A TOML number, whatever it was written as: `min_p = 0` is an integer and `min_p = 0.0` a float,
/// and both are the same instruction to a server.
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

/// The precedence, stated once: the flag beats the project file, which beats the user file,
/// which beats the built-in default, and an unset key falls through. `--model` alone is the
/// local binding rather than a choice, which is the caller's to decide.
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

    /// A temp dir for a `leticode.toml` fixture, unique per test so parallel tests do not share a
    /// file. Nothing outside `std::env::temp_dir` is ever written.
    fn project_dir(test: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("letibot-leticode-{}-{test}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// The file a test case is about, written fresh so a rerun cannot see the previous case's bytes.
    fn write_file(dir: &Path, text: &str) -> PathBuf {
        let path = dir.join(FILE_NAME);
        std::fs::write(&path, text).unwrap();
        path
    }

    /// The walk-up finds the nearest file and stops at the root, so a session started in a
    /// subdirectory still finds the project's settings and a parent does not shadow the
    /// workspace.
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

    /// The precedence, checked here, and the level comes back with the name: "which model"
    /// without "which file" is the question the operator spent an evening on.
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

    /// The user level is the same file at a fixed path, through the same parser, so the two
    /// levels cannot start disagreeing about what a key means.
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

    /// An unparsable user file is loud and does not take the daemon down: the reader answers
    /// `Err` with the file and the line, and `cli.rs` turns that into a sentence and a session.
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

        // The fallback is the empty config, which is a working config rather than a half-read
        // one: no model, no roles, nothing half-applied.
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

    /// An unknown key at the user level is named too: a user file is not the place a capability
    /// sneaks in just because it is yours.
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

    /// The two file levels merge key by key, the project's over the user's, so a project that
    /// changes one number keeps the user's other keys.
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

    /// A key the version does not know is named, not ignored: a reader that accepted a typo and
    /// read nothing reproduces the defect one line lower.
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

    /// A typo inside a role's table is named too, and this is the half serde cannot do: the
    /// table is read key by key, so `temperatuer = 0.7` accepted and read as nothing is exactly
    /// the defect.
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

    /// A window or a reply cap in a role's table is refused by name, with the reason: samplers
    /// are dense78's and its context limit is not, since a child's window is measured on the box
    /// that answers it.
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

    /// A file that will not parse says so and says where, and does not take the session down:
    /// the daemon runs on its own defaults and says so.
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

    /// An absent file is simply the defaults. A complaint on every start of a box with no project
    /// file would be a regression, and that is most of them.
    #[test]
    fn an_absent_file_is_silently_the_defaults() {
        let dir = project_dir("absent");
        let missing = dir.join(FILE_NAME);
        assert!(!missing.exists());
        let cfg = LeticodeConfig::load(&missing).expect("a missing file is Ok");
        assert_eq!(cfg, LeticodeConfig::default());
        assert!(cfg.set_models().is_empty(), "nothing to disclose");
    }

    /// The file may set a model and may not widen a capability: a line that would seat a tool or
    /// lift a narrowing is a top-level key the version does not know.
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

    /// The well-formed file gives the models it set, and the disclosure names them so "where did
    /// this model come from" is answerable from the screen. A file that sets nothing discloses
    /// nothing.
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

    /// A role the file does not know is carried, not refused: the table is an override, so a
    /// future role needs a line rather than a new key.
    #[test]
    fn a_role_the_file_does_not_know_is_carried_not_refused() {
        let dir = project_dir("future_role");
        let path = write_file(&dir, "[roles]\nsome_future_role = \"a-model\"\n");
        let cfg = LeticodeConfig::load(&path).expect("a future role is Ok");
        assert_eq!(cfg.role_model("some_future_role"), Some("a-model"));
    }

    /// The subagent runs on dense78's parameters and the file does not have to say so, or a box
    /// with no file would get children that overthink. The answer carries the five and nothing
    /// else: no window, because the box serving a child is not always the one that measured it.
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
    /// is every spawn, `[roles.coder]` is coder spawns, and a seat that says nothing keeps the
    /// rest of the cure.
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

    /// The model a spawn runs on: `subagent_model` for every child, a `[roles.X] model` for one
    /// seat's and over it, and `None` leaving the caller's own answer where it was.
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

    /// One seat, two answers, refused by name: the guard and the adjudicator are one oracle, so
    /// two DIFFERENT models is the file disagreeing with itself about who guards.
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

    /// The project's `[memory] default` beats the box-wide one for a session in it, and the
    /// policy names the project file as the source. A session elsewhere keeps the box's number.
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

    /// **A file may narrow the ceiling and may not raise it**, and its default may not pass
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
    /// the file by name — the loud failure every other key here gets.
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

    /// **`[permissions] ask_unconfined_code` is read, and one key this build does not know is
    /// refused by name.** R98(1) is the one setting here: off unless a file says so.
    #[test]
    fn the_permissions_table_is_read_and_its_one_key_is_named() {
        let dir = project_dir("permissions");
        let path = write_file(&dir, "[permissions]\nask_unconfined_code = true\n");
        let cfg = LeticodeConfig::load(&path).expect("one key parses");
        assert!(cfg.permissions.ask_unconfined_code());
        assert!(
            !LeticodeConfig::default().permissions.ask_unconfined_code(),
            "the default is OFF — the behaviour that predates the rule"
        );

        let path = write_file(&dir, "[permissions]\nask_unconfined_cod = true\n");
        let err = LeticodeConfig::load(&path).unwrap_err();
        assert!(
            err.contains("ask_unconfined_cod") && err.contains("[permissions]"),
            "{err}"
        );
    }

    /// **Either level may turn it on and neither can turn it off.** The user file is the
    /// operator's own; a project file is one a session with `write` can edit, so a `false` in
    /// the repository must not loosen what the user asked for.
    #[test]
    fn a_project_file_cannot_turn_the_ask_back_off() {
        let on = PermissionKeys {
            ask_unconfined_code: Some(true),
        };
        let off = PermissionKeys {
            ask_unconfined_code: Some(false),
        };
        assert!(
            on.overridden_by(&off).ask_unconfined_code(),
            "a project file turned the user's ask off"
        );
        assert!(off.overridden_by(&on).ask_unconfined_code());
        assert!(
            !off.overridden_by(&PermissionKeys::default())
                .ask_unconfined_code()
        );
    }
}
