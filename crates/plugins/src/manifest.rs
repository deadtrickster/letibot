//! `plugin.toml` - what a layer declares about itself, and what the host will not load without.
//! ```toml
//! name = "plan-dashboard"
//! purpose = "a project plan's tiers, stages, agents and hosts at a glance"
//! hooks = ["pane_dashboard"]
//! capabilities = ["read"]
//! contract = 1
//! ```

use std::path::{Component, Path, PathBuf};

use crate::{CONTRACT, WASM, is_hook_kind};

/// Where a layer came from. The order is the precedence, session highest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Scope {
    Session,
    Project,
    User,
    BuiltIn,
}

impl Scope {
    /// The letter a layer id starts with: `s3`, `p1`, `u2`. Built-ins are named rather than
    /// numbered (`b-github-pr`), because they are a fixed set the host compiled in and their
    /// names are what a person reads in the stack view.
    pub fn prefix(self) -> &'static str {
        match self {
            Scope::Session => "s",
            Scope::Project => "p",
            Scope::User => "u",
            Scope::BuiltIn => "b",
        }
    }

    /// Where this scope sits in the stack, lowest rank on top. Used to place a new layer.
    pub(crate) fn rank(self) -> u8 {
        match self {
            Scope::Session => 0,
            Scope::Project => 1,
            Scope::User => 2,
            Scope::BuiltIn => 3,
        }
    }
}

impl std::fmt::Display for Scope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Scope::Session => "session",
            Scope::Project => "project",
            Scope::User => "user",
            Scope::BuiltIn => "built-in",
        })
    }
}

/// What a layer may ask the world for. Declared here, granted by the host - a capability in
/// a manifest is a request, not a permission, and the [`crate::Loader`] is handed exactly this
/// list so a module's imports cannot reach past it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Capability {
    /// Read files under the workspace.
    Read,
    /// Read files under ONE tree outside the workspace, named as it was declared. `read` alone is
    /// the workspace, so this spelling exists rather than a widening of it: the manifest line is
    /// the approval, and the string is the DECLARED spelling so `Display` prints back what was read.
    ReadPath(String),
    /// Run `<program>`, with arguments the plugin chooses, through the gate.
    Exec(String),
    /// GET from `<host>`.
    Http(String),
    /// Name blobs the host already holds, by id, without ever seeing their bytes (`X3`). One
    /// spelling per store, and `room` is the only store this build serves.
    Blobs(String),
}

impl Capability {
    /// Parse one entry of `capabilities = [...]`. The spelling is the contract, so an entry that
    /// is almost right is refused with the entry quoted rather than dropped.
    pub fn parse(s: &str) -> Result<Capability, String> {
        let s = s.trim();
        if s == "read" {
            return Ok(Capability::Read);
        }
        // `read:<path>` before the loop, because `read` is also a bare word and an entry
        // that is almost right must be refused rather than read as the workspace.
        if let Some(rest) = s.strip_prefix("read:") {
            return read_path(rest).map(Capability::ReadPath);
        }
        // `blobs:<store>`, refused by name for a store this build does not serve: a capability
        // nobody implements is a layer that draws an empty picture and looks broken.
        if let Some(rest) = s.strip_prefix("blobs:") {
            let rest = rest.trim();
            return match rest {
                "room" => Ok(Capability::Blobs("room".to_string())),
                "" => Err(
                    "capability `blobs:` names no store after it — `.letibot` knows `room`"
                        .to_string(),
                ),
                other => Err(format!(
                    "capability `blobs:{other}` names no store this host serves — it serves \
                     `blobs:room`"
                )),
            };
        }
        for (prefix, ctor) in [
            ("exec:", Capability::Exec as fn(String) -> Capability),
            ("http:", Capability::Http as fn(String) -> Capability),
        ] {
            if let Some(rest) = s.strip_prefix(prefix) {
                let rest = rest.trim();
                return if rest.is_empty() {
                    Err(format!("capability `{s}` names nothing after `{prefix}`"))
                } else {
                    Ok(ctor(rest.to_string()))
                };
            }
        }
        Err(format!(
            "unknown capability `{s}` — the spellings are `read`, `read:<path>`, `exec:<program>`, \
             `http:<host>` and `blobs:room`"
        ))
    }
}

/// One tree, named as a person would write it. The rules are the whole security story of
/// `read:<path>`, so they live here and not in the host: absolute or home-relative (a relative
/// path names a different tree on every machine), strictly below the root or home, and no `..`.
fn read_path(rest: &str) -> Result<String, String> {
    let path = rest.trim();
    if path.is_empty() {
        return Err(
            "capability `read:` names no path after it — the spelling is \
                  `read:<path>`, e.g. `read:~/.claude/projects`"
                .to_string(),
        );
    }
    if path == "/" || path == "~" || path == "~/" {
        return Err(format!(
            "capability `read:{path}` names every file there is — a `read:<path>` names ONE tree, \
             and the root and a home directory are not trees a reviewer can check"
        ));
    }
    if !(path.starts_with('/') || path == "~" || path.starts_with("~/")) {
        return Err(format!(
            "capability `read:{path}` is relative — a `read:<path>` is absolute (`/…`) or \
             home-relative (`~/…`), because a relative path names a different tree on every \
             machine"
        ));
    }
    if path.split('/').any(|part| part == "..") {
        return Err(format!(
            "capability `read:{path}` climbs with `..` — declare the tree it resolves to"
        ));
    }
    Ok(path.to_string())
}

/// A declared module is one file inside the layer's own directory. The rule is here rather than at
/// each reader because the pin, the hash and the poll all open this path: `../elsewhere.wasm` would
/// make a pin a statement about bytes the reviewer never saw. No `..`, no root; subdirectories are fine.
fn module_file(declared: &str) -> Result<String, String> {
    let text = declared.trim();
    if text.is_empty() {
        return Err(
            "module: names no file. A layer's module is a file beside its plugin.toml, and \
             `plugin.wasm` when the key is left out"
                .to_string(),
        );
    }
    if !Path::new(text)
        .components()
        .all(|c| matches!(c, Component::Normal(_)))
    {
        return Err(format!(
            "module: `{text}` leaves the layer's directory. A module is a relative path inside \
             it (`pane.wasm`, `dist/pane.wasm`), because the pin is a statement about what that \
             directory holds"
        ));
    }
    Ok(text.to_string())
}

impl std::fmt::Display for Capability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Capability::Read => f.write_str("read"),
            Capability::ReadPath(p) => write!(f, "read:{p}"),
            Capability::Exec(p) => write!(f, "exec:{p}"),
            Capability::Http(h) => write!(f, "http:{h}"),
            Capability::Blobs(store) => write!(f, "blobs:{store}"),
        }
    }
}

/// A layer's `plugin.toml`, parsed. Every field is required except `module`, and unknown keys are
/// ignored rather than refused: a newer layer may carry metadata this host has no use for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub name: String,
    pub purpose: String,
    /// The file this layer's answers come from, relative to the layer's own directory, and
    /// `plugin.wasm` when the manifest says nothing. One file and not a search: the pin, the hash,
    /// `poll_changes` and the gate all read it, so the ladder is about the bytes that answer.
    pub module: String,
    pub hooks: Vec<String>,
    pub capabilities: Vec<Capability>,
    pub contract: u32,
    /// The version of each hook kind this layer was written against, for the kinds whose
    /// contract has moved ([`crate::MOVED`]). A kind that is absent is at `contract`, which is
    /// what a layer written before the move says.
    pub contracts: std::collections::BTreeMap<String, u32>,
    /// The feeds a `pane` layer reads - data the host already has (`letibot_plugin::pane::Feed`).
    /// A DECLARATION and not a capability: a feed is a read of something the host is already
    /// showing, so declaring it grants it and no adjudication is skipped.
    pub feeds: Vec<letibot_plugin::pane::Feed>,
    /// The acts a pane's rows may ask the HOST to perform (`letibot_plugin::pane::Act`). A view
    /// whose rows carry an act this list does not name is refused, so a layer cannot reach a verb
    /// nobody reviewed; the next verb a pane needs is one more word here.
    pub actions: Vec<String>,
    /// The tools this layer offers the model - `[[tools]]`, one entry per tool: its name, the one
    /// line the prompt carries, and the JSON Schema of its arguments. A declaration IS the prompt
    /// bytes, so it is reviewed as prompt bytes.
    pub tools: Vec<ToolDecl>,
}

/// One tool a layer declares: what the model is offered, and - through [`Manifest::admit`] - the
/// hook that has to answer it. The hook's name is not a field: it is `tool_<name>`, the convention
/// every kind follows, so a declared tool cannot point at somebody else's hook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolDecl {
    /// What the model calls it. The export is `tool_<name>`.
    pub name: String,
    /// The prompt's one line for it - the same sentence `letibot_tools::schema::lint_description`
    /// has an opinion about, and `crate::plugins::tool` is where that opinion is applied.
    pub description: String,
    /// JSON Schema for the arguments. The runtime's `args::salvage` repairs a malformed call
    /// against it,
    /// so a schema that says `integer` is what turns `"limit": "40"` into a repair.
    pub schema: serde_json::Value,
}

impl ToolDecl {
    /// The export this tool's answers come from.
    pub fn hook(&self) -> String {
        format!("tool_{}", self.name)
    }
}

/// A tool name is an export name and a JSON key, so it is one word: lowercase, digits and `_`,
/// starting with a letter. Refused rather than rewritten, because a name is what a model calls
/// and what a manifest says - two spellings of it is a tool nothing answers.
fn tool_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

impl Manifest {
    /// The file this layer's answers come from, in its own directory.
    /// One spelling of it, because seven callers need the answer: the pin, the module hash,
    /// `poll_changes`, the gate's build and load, and the *a manifest and no module* rule.
    pub fn module_path(&self, dir: &Path) -> PathBuf {
        dir.join(&self.module)
    }

    /// The module file a layer directory's manifest names, and `plugin.wasm` when it names none or
    /// will not parse. A builder and a pin need the answer before a `Manifest` is available, and a
    /// build that wrote one file while the loader read another would be a gate over nothing.
    pub fn module_name(dir: &Path) -> String {
        Manifest::read(dir)
            .map(|manifest| manifest.module)
            .unwrap_or_else(|_| WASM.to_string())
    }

    /// Read and parse `<dir>/plugin.toml`. Every error names the file and the key, because the
    /// reader of it is a model about to fix the file.
    pub fn read(dir: &Path) -> Result<Manifest, String> {
        let path = dir.join(crate::MANIFEST);
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        Manifest::parse(&text).map_err(|why| format!("{}: {why}", path.display()))
    }

    /// Parse the text of a `plugin.toml`.
    pub fn parse(text: &str) -> Result<Manifest, String> {
        let table: toml::Table = toml::from_str(text).map_err(|e| format!("not TOML: {e}"))?;

        let name = string(&table, "name")?;
        if name.is_empty() {
            return Err("name: a layer needs a name".to_string());
        }
        let purpose = string(&table, "purpose")?;
        if purpose.is_empty() {
            return Err(
                "purpose: a session layer's capabilities are consented to by its one-line purpose, \
                 so an empty one cannot be asked about"
                    .to_string(),
            );
        }
        let hooks = strings(&table, "hooks")?;
        let module = match table.get("module") {
            Some(toml::Value::String(declared)) => module_file(declared)?,
            Some(other) => {
                return Err(format!(
                    "module: expected a file name, found {}",
                    other.type_str()
                ));
            }
            None => WASM.to_string(),
        };
        for hook in &hooks {
            if !is_hook_kind(hook) {
                return Err(format!(
                    "hooks: `{hook}` is not a hook kind this host knows (one of {})",
                    crate::HOOK_PREFIXES.join(", ")
                ));
            }
        }
        let capabilities = strings(&table, "capabilities")?
            .iter()
            .map(|c| Capability::parse(c))
            .collect::<Result<Vec<_>, String>>()
            .map_err(|why| format!("capabilities: {why}"))?;
        let contract = match table.get("contract") {
            Some(toml::Value::Integer(n)) if (0..=u32::MAX as i64).contains(n) => *n as u32,
            Some(other) => {
                return Err(format!(
                    "contract: expected an integer, found {}",
                    other.type_str()
                ));
            }
            None => return Err("contract: missing (this host speaks 1)".to_string()),
        };
        // Both defaulted, so a layer written before a pane could read a feed or perform an act
        // keeps loading: an added key is not a version.
        let feeds = strings_opt(&table, "feeds")?
            .iter()
            .map(|f| letibot_plugin::pane::Feed::parse(f))
            .collect::<Result<Vec<_>, String>>()
            .map_err(|why| format!("feeds: {why}"))?;
        let actions = strings_opt(&table, "actions")?;
        let tools = tools_opt(&table)?;
        let contracts = contracts_opt(&table, &hooks)?;

        Ok(Manifest {
            name,
            purpose,
            module,
            hooks,
            capabilities,
            contract,
            contracts,
            feeds,
            actions,
            tools,
        })
    }

    /// Whether this layer may answer at all: the contract, a declared hook list that is exactly
    /// what the module exports, and - for a tool - the export its tool names. A tool in the prompt
    /// that nothing answers tells a model about a capability it does not have, so it is refused here.
    pub fn admit(&self, exports: &[String]) -> Result<(), String> {
        // The module's own path first: a manifest whose module leaves the folder describes a
        // layer whose bytes are not the ones a person can review.
        module_file(&self.module)?;
        if self.contract != CONTRACT {
            return Err(format!(
                "contract {} — this host speaks {CONTRACT}",
                self.contract
            ));
        }
        // A kind whose shape moved needs a witness: the wire cannot tell a row drawn as a group
        // from a row drawn as a branch, so a layer declaring no version for such a kind is taken to
        // be the version before the move and refused here rather than drawn against bad data.
        for moved in crate::MOVED {
            if !self.hooks.iter().any(|h| h == moved.hook) {
                continue;
            }
            let declared = self
                .contracts
                .get(moved.hook)
                .copied()
                .unwrap_or(self.contract);
            if declared != moved.version {
                return Err(format!(
                    "the layer `{}` draws `{}` at contract {declared} and this host speaks {}: \
                     {}. Declare `[contracts]` with `{} = {}` in plugin.toml when the layer \
                     draws that shape",
                    self.name, moved.hook, moved.version, moved.why, moved.hook, moved.version
                ));
            }
        }
        // A layer may ADD a `/` verb and may never take one. A `command_<verb>` naming a verb the
        // head or the daemon already answers would be a hook that can never fire, so it is refused
        // here rather than discovered as a layer that does nothing (X23).
        for hook in &self.hooks {
            let Some(verb) = hook.strip_prefix(letibot_plugin::command::PREFIX) else {
                continue;
            };
            if let Some(self_) = letibot_plugin::command::self_verb_for(verb) {
                return Err(format!(
                    "the layer `{}` declares `{hook}`: `{verb}` is the operator's own verb, and a \
                     person's verb is not a layer's to answer. The model's own door is `{self_}`, \
                     native and scoped to the calling session - a plugin needs no hook for it",
                    self.name
                ));
            }
            if letibot_plugin::command::is_operator_verb(verb) {
                return Err(format!(
                    "the layer `{}` declares `{hook}`: `{verb}` is one of the operator's verbs and \
                     stays theirs. A layer adds a verb the host has not got; it cannot take one a \
                     person answers",
                    self.name
                ));
            }
            if letibot_plugin::command::is_host_verb(verb) {
                return Err(format!(
                    "the layer `{}` declares `{hook}`, and `/{verb}` is a verb this host already \
                     answers - the head's own or the daemon's, which a `HOST_KEYS` key is to a pane. \
                     Choose a word the host has not got",
                    self.name
                ));
            }
        }
        // The acts a manifest asks for: the operator's three are a person's act, and a plugin that
        // could perform one would be a layer reseating, compacting or ruling on the queue (X24).
        for act in &self.actions {
            if letibot_plugin::command::is_operator_act(act) {
                return Err(format!(
                    "the layer `{}` lists the act `{act}` in `actions`: that is the operator's own \
                     act, performed by their head and by nothing else. The model's self-scoped verbs \
                     (`{}`) are native, and a layer asks for none of them",
                    self.name,
                    letibot_plugin::command::SELF_VERBS.join("`, `")
                ));
            }
        }
        // A `lifecycle_` export names one of the four events; another word is a typo that would
        // look like a layer and never be asked.
        for hook in &self.hooks {
            let Some(event) = hook.strip_prefix(letibot_plugin::lifecycle::PREFIX) else {
                continue;
            };
            if !letibot_plugin::lifecycle::EVENTS.contains(&event) {
                return Err(format!(
                    "the layer `{}` declares `{hook}`: the events are {}",
                    self.name,
                    letibot_plugin::lifecycle::EVENTS
                        .iter()
                        .map(|e| letibot_plugin::lifecycle::hook(e))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
        // A `steer_` or `compact_` export is the ONE name the host asks: the turn has one end and a
        // compaction one record, so a layer under another name would sit on the stack and never fire.
        for (prefix, asked) in [
            (letibot_plugin::steer::PREFIX, letibot_plugin::steer::HOOK),
            (
                letibot_plugin::compact::PREFIX,
                letibot_plugin::compact::HOOK,
            ),
        ] {
            if let Some(hook) = self
                .hooks
                .iter()
                .find(|h| h.starts_with(prefix) && h.as_str() != asked)
            {
                return Err(format!(
                    "the layer `{}` declares `{hook}`, and this host asks `{asked}` and nothing \
                     else: a name nothing asks would sit on the stack and never fire",
                    self.name
                ));
            }
        }
        // The tool half first: its sentence names the tool AND the export, which is what a model
        // about to fix the manifest needs.
        for tool in &self.tools {
            let hook = tool.hook();
            if !self.hooks.contains(&hook) {
                return Err(format!(
                    "tools: `{}` declares the tool `{}` and its answers come from the export \
                     `{hook}`, which `hooks` does not list — a tool the model is offered and \
                     nothing answers is a capability this build does not have",
                    self.name, tool.name
                ));
            }
        }
        let extra: Vec<&str> = exports
            .iter()
            .map(String::as_str)
            .filter(|e| !self.hooks.iter().any(|h| h == e))
            .collect();
        let missing: Vec<&str> = self
            .hooks
            .iter()
            .map(String::as_str)
            .filter(|h| !exports.iter().any(|e| e == h))
            .collect();
        if extra.is_empty() && missing.is_empty() {
            return Ok(());
        }
        let mut why = Vec::new();
        if !extra.is_empty() {
            why.push(format!(
                "exports not declared in plugin.toml: {}",
                extra.join(", ")
            ));
        }
        if !missing.is_empty() {
            why.push(format!("declared but not exported: {}", missing.join(", ")));
        }
        Err(format!(
            "manifest hooks and module exports disagree — {}",
            why.join("; ")
        ))
    }

    /// [`Manifest::admit`], and the rules about the LAYER rather than about its answers: only a
    /// built-in or a PINNED layer may read the `chat` feed, and a session's own layer may not answer
    /// the gate's floor or a review hook at all (the review's questions 9 and 4, on their defaults).
    pub fn admit_for(&self, exports: &[String], scope: Scope, pinned: bool) -> Result<(), String> {
        self.admit(exports)?;
        if scope == Scope::Session
            && let Some(why) = self.answered_by_a_session()
        {
            return Err(why);
        }
        if self.feeds.contains(&letibot_plugin::pane::Feed::Chat)
            && scope != Scope::BuiltIn
            && !pinned
        {
            return Err(format!(
                "the layer `{}` is a {scope} layer, asks for the `chat` feed, and is not pinned. \
                 That feed is every message in every room the operator sees, DMs included, so it \
                 is handed to a built-in or a pinned layer only — pin it (`/plugin trust {}`), or \
                 drop `chat` from `feeds`",
                self.name, self.name
            ));
        }
        Ok(())
    }

    /// The sentence a session's own layer reads when it answers a hook only a project, the operator
    /// or a built-in may: `None` when it answers none of them. Reviewed policy is not the governed
    /// agent's to write — the gate's floor and the review of the agent's own branch are the two.
    fn answered_by_a_session(&self) -> Option<String> {
        let floor = self
            .hooks
            .iter()
            .find(|h| h.starts_with(letibot_plugin::adjudicate::PREFIX));
        if let Some(hook) = floor {
            return Some(format!(
                "the layer `{}` is a session's own and answers `{hook}`. The floor at the gate \
                 is reviewed policy — a project's production hosts and its own secret stores — \
                 and the layer of the session it governs may not raise or lower it, so this hook \
                 is a built-in's, a project's or the operator's. Move it to \
                 `~/.config/letibot/plugins` (a user layer) or to the project's \
                 `.letibot/plugins`",
                self.name
            ));
        }
        let review = self
            .hooks
            .iter()
            .find(|h| letibot_plugin::review::is_review_hook(h))?;
        Some(format!(
            "the layer `{}` is a session's own and answers `{review}`. A review is somebody \
             else's judgement of this session's work — its brief and its verdict's floor — so the \
             layer of the session under review may not write either: that would be the branch \
             grading itself. This hook is a built-in's, a project's or the operator's. Move it to \
             `~/.config/letibot/plugins` (a user layer) or to the project's `.letibot/plugins`",
            self.name
        ))
    }
}

fn string(table: &toml::Table, key: &str) -> Result<String, String> {
    match table.get(key) {
        Some(toml::Value::String(s)) => Ok(s.trim().to_string()),
        Some(other) => Err(format!(
            "{key}: expected a string, found {}",
            other.type_str()
        )),
        None => Err(format!("{key}: missing")),
    }
}

/// `[contracts]`: one version per hook kind the layer answers, for the kinds whose contract
/// has moved. A key that names a hook the layer does not answer is refused: it is a typo standing
/// where a declaration should be, and the version it carries would never be checked.
fn contracts_opt(
    table: &toml::Table,
    hooks: &[String],
) -> Result<std::collections::BTreeMap<String, u32>, String> {
    let Some(value) = table.get("contracts") else {
        return Ok(std::collections::BTreeMap::new());
    };
    let Some(entries) = value.as_table() else {
        return Err(format!(
            "contracts: expected a table of `hook = version`, found {}",
            value.type_str()
        ));
    };
    let mut out = std::collections::BTreeMap::new();
    for (hook, found) in entries {
        if !hooks.iter().any(|h| h == hook) {
            return Err(format!(
                "contracts: `{hook}` is not in `hooks`: a version for a hook this layer does not \
                 answer is never checked"
            ));
        }
        match found {
            toml::Value::Integer(n) if (1..=u32::MAX as i64).contains(n) => {
                out.insert(hook.clone(), *n as u32);
            }
            other => {
                return Err(format!(
                    "contracts: `{hook}` takes a version as an integer, found {} in it",
                    other.type_str()
                ));
            }
        }
    }
    Ok(out)
}

fn strings(table: &toml::Table, key: &str) -> Result<Vec<String>, String> {
    match table.get(key) {
        Some(toml::Value::Array(items)) => items
            .iter()
            .map(|v| match v {
                toml::Value::String(s) => Ok(s.trim().to_string()),
                other => Err(format!(
                    "{key}: expected an array of strings, found {} in it",
                    other.type_str()
                )),
            })
            .collect(),
        Some(other) => Err(format!(
            "{key}: expected an array of strings, found {}",
            other.type_str()
        )),
        None => Err(format!("{key}: missing")),
    }
}

/// The same, for a key an older layer does not have - absent is an empty list.
fn strings_opt(table: &toml::Table, key: &str) -> Result<Vec<String>, String> {
    match table.get(key) {
        Some(_) => strings(table, key),
        None => Ok(Vec::new()),
    }
}

/// `[[tools]]` - absent is a layer that declares none, which is every layer that is not a tool.
/// The schema is carried as JSON rather than as TOML because `toml::Value` serialises, so it
/// arrives at the runtime as the same document a built-in tool writes by hand.
fn tools_opt(table: &toml::Table) -> Result<Vec<ToolDecl>, String> {
    let Some(value) = table.get("tools") else {
        return Ok(Vec::new());
    };
    let toml::Value::Array(items) = value else {
        return Err(format!(
            "tools: expected an array of tables (`[[tools]]`), found {}",
            value.type_str()
        ));
    };
    let mut out: Vec<ToolDecl> = Vec::with_capacity(items.len());
    for (n, item) in items.iter().enumerate() {
        let toml::Value::Table(entry) = item else {
            return Err(format!(
                "tools: entry {} is a {}, and a tool is a table with `name`, `description` and \
                 `schema`",
                n + 1,
                item.type_str()
            ));
        };
        let name = string(entry, "name").map_err(|why| format!("tools[{}]: {why}", n + 1))?;
        if !tool_name(&name) {
            return Err(format!(
                "tools[{}]: `{name}` is not a tool name — lowercase letters, digits and `_`, \
                 starting with a letter (it is an export name: `tool_{name}`)",
                n + 1
            ));
        }
        if out.iter().any(|t| t.name == name) {
            return Err(format!("tools: `{name}` is declared twice"));
        }
        let description =
            string(entry, "description").map_err(|why| format!("tools[{name}]: {why}"))?;
        if description.is_empty() {
            return Err(format!(
                "tools[{name}]: an empty description is a tool the model is offered with no \
                 sentence on how to use it"
            ));
        }
        let schema = match entry.get("schema") {
            Some(toml::Value::Table(schema)) => serde_json::to_value(schema)
                .map_err(|e| format!("tools[{name}]: schema is not expressible as JSON: {e}"))?,
            Some(other) => {
                return Err(format!(
                    "tools[{name}]: schema must be a table (a JSON Schema object), found {}",
                    other.type_str()
                ));
            }
            None => {
                return Err(format!(
                    "tools[{name}]: schema: missing — a tool with no schema is a call the runtime \
                     cannot salvage"
                ));
            }
        };
        out.push(ToolDecl {
            name,
            description,
            schema,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAN: &str = r#"
name = "plan-dashboard"
purpose = "a project plan's tiers, stages, agents and hosts at a glance"
hooks = ["pane_dashboard"]
capabilities = ["read", "exec:gh", "http:api.github.com"]
contract = 1
"#;

    #[test]
    fn a_manifest_from_the_docs_parses_into_every_capability_kind() {
        let m = Manifest::parse(PLAN).expect("the C2 example must parse");
        assert_eq!(m.name, "plan-dashboard");
        assert_eq!(
            m.purpose,
            "a project plan's tiers, stages, agents and hosts at a glance"
        );
        assert_eq!(m.hooks, vec!["pane_dashboard".to_string()]);
        assert_eq!(
            m.capabilities,
            vec![
                Capability::Read,
                Capability::Exec("gh".to_string()),
                Capability::Http("api.github.com".to_string()),
            ]
        );
        assert_eq!(m.contract, 1);
        assert_eq!(m.capabilities[1].to_string(), "exec:gh");
    }

    #[test]
    fn a_layer_names_its_module_and_plugin_wasm_is_the_default() {
        let m = Manifest::parse(PLAN).expect("the C2 example must parse");
        assert_eq!(
            m.module, WASM,
            "a manifest that says nothing means plugin.wasm"
        );

        let named = Manifest::parse(&format!("{PLAN}module = \"sessions.js\"\n"))
            .expect("a manifest may name its module");
        assert_eq!(named.module, "sessions.js");
        assert_eq!(
            named.module_path(Path::new("a/b")),
            Path::new("a/b/sessions.js")
        );
    }

    #[test]
    fn a_module_that_leaves_the_layers_directory_is_refused() {
        for named in ["../elsewhere.wasm", "a/../../b.wasm", "/etc/passwd"] {
            let why = Manifest::parse(&format!("{PLAN}module = \"{named}\"\n"))
                .expect_err("a module is a path inside the layer's own directory");
            assert!(why.contains("module:"), "{named}: {why}");
            assert!(why.contains(named), "{why}");
        }
        // The manifest's fields are public, so a hand-built one goes through `admit`.
        let mut hand_built = Manifest::parse(PLAN).expect("the C2 example must parse");
        hand_built.module = "../elsewhere.wasm".to_string();
        let why = hand_built
            .admit(&["pane_dashboard".to_string()])
            .expect_err("an escaping module is never admitted");
        assert!(why.contains("leaves the layer's directory"), "{why}");
    }

    #[test]
    fn unknown_keys_are_ignored_and_a_bad_capability_is_named() {
        let m = Manifest::parse("name = \"a\"\npurpose = \"p\"\nhooks = []\ncapabilities = []\ncontract = 1\nauthor = \"someone\"\n")
            .expect("an unknown key is forward compatibility, not an error");
        assert!(m.hooks.is_empty());

        let why = Manifest::parse("name = \"a\"\npurpose = \"p\"\nhooks = []\ncapabilities = [\"exe:gh\"]\ncontract = 1\n")
            .expect_err("`exe:gh` is a typo for `exec:gh` and must not be granted read");
        assert!(why.contains("exe:gh"), "{why}");
    }

    #[test]
    fn every_required_key_is_required() {
        for (text, key) in [
            (
                "purpose = \"p\"\nhooks = []\ncapabilities = []\ncontract = 1\n",
                "name",
            ),
            (
                "name = \"a\"\nhooks = []\ncapabilities = []\ncontract = 1\n",
                "purpose",
            ),
            (
                "name = \"a\"\npurpose = \"p\"\ncapabilities = []\ncontract = 1\n",
                "hooks",
            ),
            (
                "name = \"a\"\npurpose = \"p\"\nhooks = []\ncontract = 1\n",
                "capabilities",
            ),
            (
                "name = \"a\"\npurpose = \"p\"\nhooks = []\ncapabilities = []\n",
                "contract",
            ),
            (
                "name = \"a\"\npurpose = \"p\"\nhooks = [\"pane_\"]\ncapabilities = []\ncontract = 1\n",
                "pane_",
            ),
        ] {
            let why = Manifest::parse(text).expect_err("a missing or malformed key is refused");
            assert!(why.contains(key), "`{key}` should be named in `{why}`");
        }
    }

    /// A kind whose shape moved is spoken at its own version, and a layer that names none is
    /// taken to be the shape before the move. It is the only detector a shape change can have.
    #[test]
    fn a_layer_that_names_no_shape_for_a_moved_kind_is_refused_by_name() {
        let old = "name = \"sessions\"\npurpose = \"p\"\nhooks = [\"pane_sessions\"]\n\
                   capabilities = []\ncontract = 1\n";
        let m = Manifest::parse(old).expect("the old spelling still parses");
        assert!(m.contracts.is_empty());
        let why = m
            .admit(&["pane_sessions".to_string()])
            .expect_err("a layer written for the groups is refused");
        assert!(why.contains("`sessions`"), "the layer is named: {why}");
        assert!(why.contains("contract 1"), "what it expects: {why}");
        assert!(why.contains("this host speaks 3"), "what we speak: {why}");
        assert!(
            why.contains("pane_sessions = 3"),
            "and the line that fixes it: {why}"
        );

        let now = format!(
            "{old}[contracts]\npane_sessions = {}\n",
            crate::speaks("pane_sessions")
        );
        Manifest::parse(&now)
            .expect("the declaration parses")
            .admit(&["pane_sessions".to_string()])
            .expect("the shape this host draws is admitted");

        // A hook whose shape never moved needs no declaration at all.
        Manifest::parse(PLAN)
            .expect("the dashboard layer")
            .admit(&["pane_dashboard".to_string()])
            .expect("a kind nobody moved is at the manifest's contract");
    }

    /// A version for a hook the layer does not answer is a typo, and it is refused rather than
    /// kept where it would never be checked.
    #[test]
    fn a_contract_for_a_hook_the_layer_does_not_answer_is_refused() {
        let why = Manifest::parse(
            "name = \"a\"\npurpose = \"p\"\nhooks = [\"pane_x\"]\ncapabilities = []\n\
             contract = 1\n[contracts]\npane_sessions = 3\n",
        )
        .expect_err("a hook outside `hooks` cannot be declared");
        assert!(why.contains("pane_sessions"), "{why}");
        assert!(why.contains("not in `hooks`"), "{why}");

        let why = Manifest::parse(
            "name = \"a\"\npurpose = \"p\"\nhooks = [\"pane_sessions\"]\ncapabilities = []\n\
             contract = 1\n[contracts]\npane_sessions = \"three\"\n",
        )
        .expect_err("a version is a number");
        assert!(why.contains("integer"), "{why}");
    }

    #[test]
    fn admission_is_the_contract_and_the_exports_together() {
        let m = Manifest::parse(PLAN).unwrap();
        m.admit(&["pane_dashboard".to_string()])
            .expect("declared and exported");

        let why = m
            .admit(&["pane_dashboard".to_string(), "on_tool_call".to_string()])
            .expect_err("an undeclared export is a diff nobody reviewed");
        assert!(why.contains("on_tool_call"), "{why}");

        let why = m
            .admit(&[])
            .expect_err("a declared hook nobody exports is refused");
        assert!(why.contains("pane_dashboard"), "{why}");

        let mut other = m.clone();
        other.contract = 2;
        let why = other
            .admit(&["pane_dashboard".to_string()])
            .expect_err("this host speaks 1");
        assert!(why.contains("contract 2"), "{why}");
    }
    /// A layer that offers the model a tool. The manifest is the prompt bytes; the export is
    /// `tool_<name>`, so a declaration and its hook cannot drift apart.
    const A_TOOL: &str = r#"
name = "claude-transcript"
purpose = "read Claude Code's own transcripts"
hooks = ["tool_claude_transcript"]
capabilities = ["read:~/.claude/projects"]
contract = 1

[[tools]]
name = "claude_transcript"
description = "read another agent's transcripts"

[tools.schema]
type = "object"

[tools.schema.properties.what]
type = "string"
"#;

    #[test]
    fn a_declared_tool_is_parsed_with_its_schema_and_its_hook() {
        let m = Manifest::parse(A_TOOL).expect("the transcript layer's manifest must parse");
        assert_eq!(m.tools.len(), 1);
        assert_eq!(m.tools[0].name, "claude_transcript");
        assert_eq!(m.tools[0].hook(), "tool_claude_transcript");
        assert_eq!(m.tools[0].description, "read another agent's transcripts");
        // The schema arrives as JSON, which is what the runtime's `salvage` reads and what a
        // built-in tool writes by hand - nothing downstream knows which of the two it got.
        assert_eq!(m.tools[0].schema["type"], serde_json::json!("object"));
        assert_eq!(
            m.tools[0].schema["properties"]["what"]["type"],
            serde_json::json!("string")
        );
        assert_eq!(
            m.capabilities,
            vec![Capability::ReadPath("~/.claude/projects".to_string())]
        );
        m.admit(&["tool_claude_transcript".to_string()])
            .expect("declared and exported");
    }

    #[test]
    fn a_tool_nothing_answers_is_refused_by_admit() {
        // The manifest names the tool and the hooks list does not carry its export: the prompt
        // would offer the model a tool that no layer answers.
        let text = A_TOOL.replace(
            "hooks = [\"tool_claude_transcript\"]",
            "hooks = [\"pane_dashboard\"]",
        );
        let m = Manifest::parse(&text).expect("the manifest parses");
        let why = m
            .admit(&["pane_dashboard".to_string()])
            .expect_err("a tool with no hook is refused");
        assert!(why.contains("claude_transcript"), "{why}");
        assert!(why.contains("tool_claude_transcript"), "{why}");
    }

    #[test]
    fn a_tool_declaration_is_checked_where_it_is_written() {
        let base = |tool: &str| {
            format!(
                "name = \"a\"\npurpose = \"p\"\nhooks = [\"tool_x\"]\ncapabilities = []\n\
                 contract = 1\n[[tools]]\n{tool}\n"
            )
        };
        for (tool, want) in [
            (
                "name = \"Not A Name\"\ndescription = \"d\"\nschema = { type = \"object\" }",
                "not a tool name",
            ),
            (
                "name = \"x\"\nschema = { type = \"object\" }",
                "description",
            ),
            ("name = \"x\"\ndescription = \"d\"", "schema"),
            (
                "name = \"x\"\ndescription = \"d\"\nschema = \"object\"",
                "must be a table",
            ),
        ] {
            let why = Manifest::parse(&base(tool)).expect_err("a bad declaration is refused");
            assert!(why.contains(want), "`{want}` should be in `{why}`");
        }
        // Two tools with one name is two answers to *what does this tool do*.
        let twice = "name = \"a\"\npurpose = \"p\"\nhooks = [\"tool_x\"]\ncapabilities = []\n\
                     contract = 1\n[[tools]]\nname = \"x\"\ndescription = \"d\"\n\
                     schema = { type = \"object\" }\n[[tools]]\nname = \"x\"\ndescription = \"d\"\n\
                     schema = { type = \"object\" }\n";
        let why = Manifest::parse(twice).expect_err("a duplicate tool name is refused");
        assert!(why.contains("declared twice"), "{why}");
    }

    #[test]
    fn a_read_path_names_one_tree_and_says_so_when_it_names_everything() {
        let ok = Capability::parse("read:~/.claude/projects").expect("one tree");
        assert_eq!(ok, Capability::ReadPath("~/.claude/projects".into()));
        // The DECLARED spelling round-trips, because it is what the stack view and a refusal print.
        assert_eq!(ok.to_string(), "read:~/.claude/projects");
        assert_eq!(
            Capability::parse("read:/var/log").unwrap(),
            Capability::ReadPath("/var/log".into())
        );
        // `read` alone is unchanged and still means the workspace.
        assert_eq!(Capability::parse("read").unwrap(), Capability::Read);

        for (entry, want) in [
            ("read:", "names no path"),
            ("read:/", "names every file there is"),
            ("read:~", "names every file there is"),
            ("read:~/ ", "names every file there is"),
            ("read:projects", "is relative"),
            ("read:~/../etc", "climbs with `..`"),
        ] {
            let why = Capability::parse(entry).expect_err("a bad tree is refused");
            assert!(
                why.contains(want),
                "`{want}` should be in `{why}` for `{entry}`"
            );
        }
    }

    #[test]
    fn a_hook_kind_needs_a_name_after_its_prefix() {
        assert!(is_hook_kind("pane_dashboard"));
        assert!(is_hook_kind("on_tool_call"));
        assert!(is_hook_kind("complete_chat"), "X6's hook kind");
        assert!(!is_hook_kind("pane_"));
        assert!(!is_hook_kind("dashboard"));
        assert!(!is_hook_kind(""));
    }

    /// The chat layer's own manifest: the hooks X6 added, the acts X7 added, and the blob store
    /// X3 added. Every one of them is a word this host now speaks, and a manifest is where a layer
    /// says it means to use it.
    const A_CHAT_LAYER: &str = r#"
name = "chat"
purpose = "the room's messages, drawn the way this project reads them"
hooks = ["pane_chat", "command_chat", "complete_chat", "on_tool_call"]
capabilities = ["blobs:room"]
feeds = ["chat", "room"]
actions = ["room_say", "room_dm", "room_read", "room_invite", "room_leave", "copy", "open", "pick"]
contract = 1
"#;

    #[test]
    fn a_manifest_declaring_a_new_act_and_the_complete_hook_is_accepted() {
        let m = Manifest::parse(A_CHAT_LAYER).expect("the chat layer's manifest must parse");
        assert!(m.hooks.contains(&"complete_chat".to_string()));
        assert_eq!(
            m.feeds,
            vec![
                letibot_plugin::pane::Feed::Chat,
                letibot_plugin::pane::Feed::Room
            ]
        );
        assert!(
            m.actions.iter().any(|a| a == "room_say"),
            "an act is one more word here: {:?}",
            m.actions
        );
        assert_eq!(m.capabilities, vec![Capability::Blobs("room".to_string())]);
        // The declared spelling is what the stack view prints back.
        assert_eq!(m.capabilities[0].to_string(), "blobs:room");
        m.admit_for(
            &[
                "pane_chat".to_string(),
                "command_chat".to_string(),
                "complete_chat".to_string(),
                "on_tool_call".to_string(),
            ],
            Scope::BuiltIn,
            false,
        )
        .expect("a built-in is admitted");

        // A store this host does not serve is refused by name, not silently granted.
        let why = Manifest::parse(&A_CHAT_LAYER.replace("blobs:room", "blobs:github"))
            .expect_err("no such store");
        assert!(why.contains("blobs:github"), "{why}");
        assert!(why.contains("blobs:room"), "and what it serves: {why}");
    }

    /// The operator's conversations are not data a layer nobody vouched for gets to read. A pinned
    /// session layer may; an unpinned one is refused at admit, and the sentence says how to fix it.
    #[test]
    fn an_unpinned_layer_asking_for_the_chat_feed_is_refused_at_admit() {
        let m = Manifest::parse(A_CHAT_LAYER).expect("the chat layer's manifest parses");
        let exports: Vec<String> = m.hooks.clone();

        // A built-in runs freely by definition, and a pinned layer has been vouched for.
        m.admit_for(&exports, Scope::BuiltIn, false)
            .expect("a built-in may read the chat");
        m.admit_for(&exports, Scope::Session, true)
            .expect("a pinned session layer may read the chat");

        let why = m
            .admit_for(&exports, Scope::Session, false)
            .expect_err("an unpinned session layer may not");
        assert!(why.contains("`chat`"), "the feed is named: {why}");
        assert!(why.contains("not pinned"), "and why: {why}");
        assert!(why.contains("session"), "and the scope: {why}");
        assert!(
            why.contains("/plugin trust chat"),
            "and the one command that fixes it: {why}"
        );
        // A project or user layer is no more pinned than a session's, and is refused the same way.
        for scope in [Scope::Project, Scope::User] {
            assert!(m.admit_for(&exports, scope, false).is_err(), "{scope}");
        }

        // The rule is about the chat feed alone: the same layer without it is admitted anywhere.
        let other = Manifest::parse(
            &A_CHAT_LAYER.replace("feeds = [\"chat\", \"room\"]", "feeds = [\"room\"]"),
        )
        .expect("a room-only layer parses");
        other
            .admit_for(&exports, Scope::Session, false)
            .expect("no chat, no refusal");
    }

    /// The floor at the gate is reviewed policy, and the layer of the session it governs is not a
    /// reviewer. A session layer that answers it is refused at its door - pinned or not, because a
    /// pin says who wrote the bytes, not that the agent may grade its own calls.
    const A_FLOOR_LAYER: &str = r#"
name = "prod-floor"
purpose = "this project's production hosts ask whatever the mode says"
hooks = ["adjudicate_floor"]
capabilities = ["read"]
contract = 1
"#;

    #[test]
    fn a_session_scope_adjudicate_layer_is_refused_at_admit() {
        let m = Manifest::parse(A_FLOOR_LAYER).expect("the floor layer's manifest parses");
        let exports: Vec<String> = m.hooks.clone();
        // A project's or the operator's own file is exactly where this belongs.
        for scope in [Scope::Project, Scope::User, Scope::BuiltIn] {
            m.admit_for(&exports, scope, false)
                .unwrap_or_else(|why| panic!("{scope} may answer the floor: {why}"));
        }
        for pinned in [false, true] {
            let why = m
                .admit_for(&exports, Scope::Session, pinned)
                .expect_err("a session layer never answers the floor, pinned or not");
            assert!(
                why.contains("`adjudicate_floor`"),
                "the hook is named: {why}"
            );
            assert!(why.contains("session"), "and the scope: {why}");
            assert!(
                why.contains("`.letibot/plugins`") && why.contains("`~/.config/letibot/plugins`"),
                "and where it goes instead: {why}"
            );
        }
    }

    /// A layer ADDS a verb the host has not got and never takes one: `HOST_VERBS` is what
    /// `HOST_KEYS` is to a pane, and the sentence names the word that was already taken.
    #[test]
    fn a_layer_cannot_shadow_a_host_verb() {
        let layer = |hook: &str| {
            format!(
                "name = \"standup\"\npurpose = \"a verb the host has not got\"\n\
                 hooks = [\"{hook}\"]\ncapabilities = []\ncontract = 1\n"
            )
        };
        for (verb, why) in [
            // The head's.
            ("help", "already answers"),
            ("models", "already answers"),
            ("status", "already answers"),
            // The daemon's.
            ("queue", "operator's verbs"),
            ("flowy", "already answers"),
            // A door tool, offered as a head verb.
            ("web-search", "already answers"),
            // The operator's own, which names the model's door instead.
            ("reseat", "operator's own verb"),
            ("compact", "operator's own verb"),
        ] {
            let m = Manifest::parse(&layer(&format!("command_{verb}"))).expect("it parses");
            let said = m
                .admit(&[format!("command_{verb}")])
                .expect_err("the verb is the host's");
            assert!(said.contains(&format!("command_{verb}")), "{said}");
            assert!(said.contains(why), "`{verb}`: {said}");
        }
        // And the sentence for a verb with a self stand-in names it, because the author of a layer
        // that wants `/reseat` is a session's parent, and the model's own door is what it wants.
        let m = Manifest::parse(&layer("command_reseat")).expect("it parses");
        assert!(
            m.admit(&["command_reseat".to_string()])
                .unwrap_err()
                .contains("reseat_self")
        );

        // A word the host has not got is what `command_` is for, and is admitted.
        let own = Manifest::parse(&layer("command_standup")).expect("it parses");
        own.admit(&["command_standup".to_string()])
            .expect("a new verb is the hook's whole purpose");
        // And a lifecycle export has to name one of the four events: another word is a typo that
        // would look like a layer nobody asks.
        let typo = Manifest::parse(&layer("lifecycle_reseat_now")).expect("it parses");
        let why = typo
            .admit(&["lifecycle_reseat_now".to_string()])
            .expect_err("no such event");
        assert!(
            why.contains("lifecycle_open"),
            "the events are named: {why}"
        );
        // And a turn-end or a compaction export is the ONE name the host asks: a second name would
        // sit on the stack and never fire, which looks exactly like a layer that does nothing.
        for (declared, asked) in [
            ("steer_continue", letibot_plugin::steer::HOOK),
            ("compact_short", letibot_plugin::compact::HOOK),
        ] {
            let m = Manifest::parse(&layer(declared)).expect("it parses");
            let why = m
                .admit(&[declared.to_string()])
                .expect_err("a name nothing asks");
            assert!(why.contains(asked), "`{declared}`: {why}");
            let good = Manifest::parse(&layer(asked)).expect("it parses");
            good.admit(&[asked.to_string()])
                .expect("the name the host asks is admitted");
        }
    }

    /// The operator's own acts are not a layer's to perform: `queue_op` is a person's verdict on the
    /// merge queue, and the three self verbs are the model's own door (X24).
    #[test]
    fn a_manifest_asking_for_an_operator_verb_is_refused() {
        let with_acts = |act: &str| {
            format!(
                "name = \"queue\"\npurpose = \"a pane that wants the operator's verdict\"\n\
                 hooks = [\"pane_queue\"]\ncapabilities = []\nactions = [\"{act}\"]\ncontract = 1\n"
            )
        };
        for act in letibot_plugin::command::OPERATOR_ACTS {
            let m = Manifest::parse(&with_acts(act)).expect("it parses");
            let why = m
                .admit(&["pane_queue".to_string()])
                .expect_err("an operator act is not a layer's");
            assert!(why.contains(act), "the act is named: {why}");
            assert!(why.contains("operator"), "and whose it is: {why}");
        }
        // The acts a layer DOES perform are untouched: a `/` verb writing a board row is the
        // example the whole hook exists for.
        let mine = Manifest::parse(&with_acts("add_todo")).expect("it parses");
        mine.admit(&["pane_queue".to_string()])
            .expect("`add_todo` is a layer's act");
    }
}
