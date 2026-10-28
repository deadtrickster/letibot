//! `plugin.toml` — what a layer declares about itself, and what the host will not load without.
//!
//! The file is small on purpose (`docs/plugins-slice1.md`, C2):
//!
//! ```toml
//! name = "plan-dashboard"
//! purpose = "a project plan's tiers, stages, agents and hosts at a glance"
//! hooks = ["pane_dashboard"]
//! capabilities = ["read"]
//! contract = 1
//! ```
//!
//! # Two spellings a layer may add, and both are a person writing something down
//!
//! `[[tools]]` declares a tool the model calls — its name, its one-line description and the JSON
//! Schema of its arguments — and `read:<path>` names one tree outside the workspace the layer may
//! read. The second exists because `read` alone means *the workspace*, and Claude Code's
//! transcripts live in `~/.claude/projects`: a layer that reads them must say so in a line a
//! reviewer can read, and `admit` is what checks the module against it. See [`ToolDecl`] and
//! [`Capability::ReadPath`].
//!
//! Every field is required, and none of them is decoration:
//!
//! * `purpose` is the sentence an operator is asked to consent to when a **session** layer wants a
//!   capability (`docs/plugins-plan.md` §3). A manifest with no purpose cannot be consented to, so
//!   it is refused rather than loaded and asked about with an empty card.
//! * `hooks` is what the review reads: a layer that exports more than it declares is a diff
//!   nobody approved. [`Manifest::admit`] compares it with the module's exports.
//! * `capabilities` is the whole of the sandbox: it is handed to the [`crate::Loader`], which
//!   grants nothing beyond it. `read:<path>` is the one entry that names a tree rather than the
//!   workspace, so a manifest can be read as *exactly what this layer may look at*.
//! * `tools` is what the model is offered: a declared tool whose hook the module does not export
//!   is refused by [`Manifest::admit`], because a tool in the prompt that nothing answers is the
//!   harness telling the model about a capability it does not have.
//! * `contract` is the version of the JSON contracts the hooks speak; a host that moved does not
//!   guess, it refuses by name (§1, principle 6).
//!
//! Unknown keys are ignored rather than refused: a newer layer may carry metadata this host has
//! no use for, and refusing it would make every forward-compatible field a version bump.

use std::path::Path;

use crate::{CONTRACT, is_hook_kind};

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

/// What a layer may ask the world for. Declared here, granted by the host (§3) — a capability in
/// a manifest is a request, not a permission, and the [`crate::Loader`] is handed exactly this
/// list so a module's imports cannot reach past it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Capability {
    /// Read files under the workspace.
    Read,
    /// **Read files under ONE tree outside the workspace**, named as it was declared —
    /// `read:~/.claude/projects`.
    ///
    /// `read` means the workspace and nothing above it, which is the right default and is why
    /// this spelling exists rather than a widening of that one: a layer that reads Claude Code's
    /// own transcripts is reading something no repository holds, and the manifest line is the
    /// approval. A person wrote the path down; a reviewer read it; `admit` and the host's look
    /// runner are what make the module unable to reach past it.
    ///
    /// The string is the DECLARED spelling and not a resolved path: it is what `Display` prints
    /// back into the stack view and into a refusal, so `~/` in a manifest reads as `~/` in the
    /// answer rather than as whoever's home the daemon happens to have. Expansion is the host's
    /// act, at the moment of a look (`crate::plugins::look`).
    ReadPath(String),
    /// Run `<program>`, with arguments the plugin chooses, through the gate.
    Exec(String),
    /// GET from `<host>`.
    Http(String),
}

impl Capability {
    /// Parse one entry of `capabilities = [...]`. The spelling is the contract, so an entry that
    /// is almost right is refused with the entry quoted rather than dropped.
    pub fn parse(s: &str) -> Result<Capability, String> {
        let s = s.trim();
        if s == "read" {
            return Ok(Capability::Read);
        }
        // **`read:<path>` before the loop**, because `read` is also a bare word and an entry
        // that is almost right must be refused rather than read as the workspace.
        if let Some(rest) = s.strip_prefix("read:") {
            return read_path(rest).map(Capability::ReadPath);
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
            "unknown capability `{s}` — the spellings are `read`, `read:<path>`, `exec:<program>` \
             and `http:<host>`"
        ))
    }
}

/// **One tree, named as a person would write it.** The rules are the whole security story of
/// `read:<path>`, so they are here and not in the host that expands the path:
///
/// * absolute (`/…`) or home-relative (`~/…`) — a relative path names a different tree on every
///   machine, which is not something a reviewer can approve;
/// * strictly deeper than the root or the home directory: `read:/` and `read:~` are *everything*
///   spelled as one word, and a manifest that means everything should have to say it as three
///   lines a person stops at;
/// * no `..` segment, for the same reason `look` canonicalises: a path that climbs out of the tree
///   it names is a path whose declaration is not what it reads.
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

impl std::fmt::Display for Capability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Capability::Read => f.write_str("read"),
            Capability::ReadPath(p) => write!(f, "read:{p}"),
            Capability::Exec(p) => write!(f, "exec:{p}"),
            Capability::Http(h) => write!(f, "http:{h}"),
        }
    }
}

/// A layer's `plugin.toml`, parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub name: String,
    pub purpose: String,
    pub hooks: Vec<String>,
    pub capabilities: Vec<Capability>,
    pub contract: u32,
    /// **The feeds a `pane` layer reads** — data the host already has (`docs/plugins-plan.md`
    /// §4.9, `letibot_plugin::pane::Feed`). Empty is a layer that reads none, which is every
    /// layer that is not a pane and a pane that draws only what it is handed in `state`.
    ///
    /// This is a DECLARATION and not a capability: a feed is a read of something the host is
    /// already showing (the session list, the monitors), so it is granted by being declared and
    /// reviewed with the layer, and no adjudication is skipped by it.
    pub feeds: Vec<letibot_plugin::pane::Feed>,
    /// **The acts a pane's rows may ask the HOST to perform** — `switch`, `fold`
    /// (`letibot_plugin::pane::Act`). A view whose rows carry an act this list does not name is
    /// refused, so a layer cannot reach a verb nobody reviewed; the next verb a pane needs is one
    /// more word here and no new mechanism.
    pub actions: Vec<String>,
    /// **The tools this layer offers the model** — `[[tools]]`, one entry per tool: its name, the
    /// one line the prompt carries, and the JSON Schema of its arguments.
    ///
    /// A declaration is the whole of what the model sees, so it is prompt bytes and it is reviewed
    /// as prompt bytes. Empty is every layer that is not a tool, which is most of them.
    pub tools: Vec<ToolDecl>,
}

/// One tool a layer declares: what the model is offered, and — through [`Manifest::admit`] — the
/// hook that has to answer it.
///
/// The hook's name is not a field. It is `tool_<name>`, the export convention every hook kind
/// follows (`pane_`, `condition_`, `notice_`), so a declared tool cannot point at a hook somebody
/// else's manifest named, and the two spellings cannot drift apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolDecl {
    /// What the model calls it. The export is `tool_<name>`.
    pub name: String,
    /// The prompt's one line for it — the same sentence `letibot_tools::schema::lint_description`
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
/// and what a manifest says — two spellings of it is a tool nothing answers.
fn tool_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

impl Manifest {
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
        // **Both defaulted**, so a layer written before a pane could read a feed or perform an act
        // keeps loading: an added key is not a version (§1, principle 6).
        let feeds = strings_opt(&table, "feeds")?
            .iter()
            .map(|f| letibot_plugin::pane::Feed::parse(f))
            .collect::<Result<Vec<_>, String>>()
            .map_err(|why| format!("feeds: {why}"))?;
        let actions = strings_opt(&table, "actions")?;
        let tools = tools_opt(&table)?;

        Ok(Manifest {
            name,
            purpose,
            hooks,
            capabilities,
            contract,
            feeds,
            actions,
            tools,
        })
    }

    /// Whether this layer may answer at all: the contract this host speaks, a declared hook
    /// list that is exactly what the module exports, and — for a tool — the export its tool names.
    ///
    /// All of it is checked wherever a module is loaded — at discovery and at the gate — so a
    /// layer the gate would refuse is not already answering in the live stack, and a layer that
    /// answers cannot be one whose exports the review never saw.
    ///
    /// **A declared tool with no hook is refused here**, and that is the check the model's prompt
    /// depends on: the host seats the tool before any module is loaded (the prompt is composed
    /// first), so `admit` is the only place that can say the layer offering it does not answer it.
    /// A tool in the prompt that nothing answers is the harness telling a model about a capability
    /// it does not have, which is the defect every refusal in this crate exists against.
    pub fn admit(&self, exports: &[String]) -> Result<(), String> {
        if self.contract != CONTRACT {
            return Err(format!(
                "contract {} — this host speaks {CONTRACT}",
                self.contract
            ));
        }
        // The tool half first: its sentence names the tool AND the export, which is what a model
        // about to fix the manifest needs.
        for tool in &self.tools {
            let hook = tool.hook();
            if !self.hooks.iter().any(|h| *h == hook) {
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

/// The same, for a key an older layer does not have — absent is an empty list.
fn strings_opt(table: &toml::Table, key: &str) -> Result<Vec<String>, String> {
    match table.get(key) {
        Some(_) => strings(table, key),
        None => Ok(Vec::new()),
    }
}

/// `[[tools]]` — absent is a layer that declares none, which is every layer that is not a tool.
///
/// The schema is carried as JSON rather than as TOML: `toml::Value` serialises, so a schema written
/// as a TOML table arrives at the runtime as the same document a built-in tool writes by hand, and
/// nothing downstream has to know which of the two it got.
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
    /// **A layer that offers the model a tool.** The manifest is the prompt bytes; the export is
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
        // built-in tool writes by hand — nothing downstream knows which of the two it got.
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
        assert!(!is_hook_kind("pane_"));
        assert!(!is_hook_kind("dashboard"));
        assert!(!is_hook_kind(""));
    }
}
