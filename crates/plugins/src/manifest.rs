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
//! Every field is required, and none of them is decoration:
//!
//! * `purpose` is the sentence an operator is asked to consent to when a **session** layer wants a
//!   capability (`docs/plugins-plan.md` §3). A manifest with no purpose cannot be consented to, so
//!   it is refused rather than loaded and asked about with an empty card.
//! * `hooks` is what the review reads: a layer that exports more than it declares is a diff
//!   nobody approved. [`Manifest::admit`] compares it with the module's exports.
//! * `capabilities` is the whole of the sandbox: it is handed to the [`crate::Loader`], which
//!   grants nothing beyond it.
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
            "unknown capability `{s}` — the spellings are `read`, `exec:<program>` and `http:<host>`"
        ))
    }
}

impl std::fmt::Display for Capability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Capability::Read => f.write_str("read"),
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

        Ok(Manifest {
            name,
            purpose,
            hooks,
            capabilities,
            contract,
        })
    }

    /// Whether this layer may answer at all: the contract this host speaks, and a declared hook
    /// list that is exactly what the module exports.
    ///
    /// Both halves are checked wherever a module is loaded — at discovery and at the gate — so a
    /// layer the gate would refuse is not already answering in the live stack, and a layer that
    /// answers cannot be one whose exports the review never saw.
    pub fn admit(&self, exports: &[String]) -> Result<(), String> {
        if self.contract != CONTRACT {
            return Err(format!(
                "contract {} — this host speaks {CONTRACT}",
                self.contract
            ));
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

    #[test]
    fn a_hook_kind_needs_a_name_after_its_prefix() {
        assert!(is_hook_kind("pane_dashboard"));
        assert!(is_hook_kind("on_tool_call"));
        assert!(!is_hook_kind("pane_"));
        assert!(!is_hook_kind("dashboard"));
        assert!(!is_hook_kind(""));
    }
}
