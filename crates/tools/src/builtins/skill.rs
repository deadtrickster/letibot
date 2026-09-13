//! `skill` — named, loadable capabilities (opencode's skills).
//!
//! A skill is a name, a one-line description and a body of instructions. The tool
//! lists what is loaded, or loads one by name so its instructions reach the model.
//! Session-scoped: it changes only what the model knows, nothing the operator owns.

use std::sync::Arc;

use serde_json::json;

use crate::runtime::{Invocation, InvokeCtx, Tool};
use crate::schema::{Access, ToolSchema};

/// One skill.
#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub body: String,
}

impl Skill {
    pub fn new(name: impl Into<String>, description: impl Into<String>, body: impl Into<String>) -> Self {
        Skill {
            name: name.into(),
            description: description.into(),
            body: body.into(),
        }
    }
}

/// The loaded skills, shared with the tool. Immutable after construction.
#[derive(Debug, Clone, Default)]
pub struct SkillRegistry {
    pub skills: Vec<Skill>,
}

impl SkillRegistry {
    pub fn new(skills: Vec<Skill>) -> Self {
        SkillRegistry { skills }
    }

    /// Load the skills an operator has on disk, in the convention opencode and
    /// Claude Code share: a directory of skills, each a subdirectory holding a
    /// `SKILL.md` whose frontmatter names it and describes it.
    ///
    /// Scanned locations, first hit wins per name:
    ///
    /// * `~/.claude/skills/*/SKILL.md`
    /// * `$XDG_CONFIG_HOME/letibot/skills/*/SKILL.md` (or `~/.config/letibot/skills`)
    ///
    /// A directory that cannot be read is skipped, not fatal: a skill is a
    /// convenience, and a daemon that refused to start over one unreadable file
    /// would be the convenience turned into an outage.
    pub fn load_default() -> Self {
        let mut dirs: Vec<std::path::PathBuf> = Vec::new();
        if let Ok(home) = std::env::var("HOME") {
            dirs.push(std::path::PathBuf::from(&home).join(".claude/skills"));
            dirs.push(std::path::PathBuf::from(&home).join(".config/letibot/skills"));
        }
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            dirs.push(std::path::PathBuf::from(xdg).join("letibot/skills"));
        }
        Self::load_from_dirs(&dirs)
    }

    /// Load skills from each directory; a later directory's skill of the same name
    /// wins, so `letibot/skills` can override a `.claude` skill.
    pub fn load_from_dirs(dirs: &[std::path::PathBuf]) -> Self {
        let mut skills: Vec<Skill> = Vec::new();
        for dir in dirs {
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let skill_dir = entry.path();
                if !skill_dir.is_dir() {
                    continue;
                }
                let Some(skill) = load_one(&skill_dir) else {
                    continue;
                };
                if let Some(existing) = skills.iter_mut().find(|s| s.name == skill.name) {
                    *existing = skill;
                } else {
                    skills.push(skill);
                }
            }
        }
        // Deterministic order for a stable listing.
        skills.sort_by(|a, b| a.name.cmp(&b.name));
        SkillRegistry { skills }
    }

    pub fn names(&self) -> Vec<&str> {
        self.skills.iter().map(|s| s.name.as_str()).collect()
    }

    pub fn load(&self, name: &str) -> Option<&str> {
        self.skills.iter().find(|s| s.name == name).map(|s| s.body.as_str())
    }

    fn list(&self) -> String {
        if self.skills.is_empty() {
            return "no skills are loaded.".into();
        }
        let mut out = format!("{} skill(s):\n", self.skills.len());
        for s in &self.skills {
            out.push_str(&format!("- {}: {}\n", s.name, s.description));
        }
        out
    }
}

/// One skill directory's `SKILL.md`: frontmatter `name` + `description`, then the
/// body. `None` when the file is absent or the frontmatter cannot be read — a
/// malformed skill is skipped, not loaded half-way.
fn load_one(dir: &std::path::Path) -> Option<Skill> {
    let text = std::fs::read_to_string(dir.join("SKILL.md")).ok()?;
    let body = text.strip_prefix("---")?;
    let (front, rest) = body.split_once("---")?;
    let mut name = None;
    let mut description = None;
    for line in front.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("name:") {
            name = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("description:") {
            description = Some(v.trim().to_string());
        }
    }
    let name = name.filter(|n| !n.is_empty())?;
    Some(Skill::new(
        name,
        description.unwrap_or_default(),
        rest.trim_start().to_string(),
    ))
}

/// The `skill` tool.
pub struct SkillTool {
    registry: Arc<SkillRegistry>,
}

impl SkillTool {
    pub fn new(registry: Arc<SkillRegistry>) -> Self {
        SkillTool { registry }
    }
}

impl Tool for SkillTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "skill",
            "List the available skills, or load one to read its instructions into the \
             conversation. Give `action` \"list\" to see them, or `action` \"load\" \
             with `name` to load that skill.",
            json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["list", "load"]},
                    "name": {"type": "string", "description": "The skill to load, for action \"load\"."}
                },
                "required": ["action"]
            }),
            Access::Session,
        )
    }

    fn invoke(&self, _ctx: &mut InvokeCtx<'_>, args: &serde_json::Value) -> Invocation {
        let Some(action) = args.get("action").and_then(|v| v.as_str()) else {
            return Invocation::failed(
                "skill needs an action",
                "call `skill` with `action` = \"list\" or \"load\".",
            );
        };
        match action {
            "list" => Invocation::ok(self.registry.list()),
            "load" => {
                let Some(name) = args.get("name").and_then(|v| v.as_str()) else {
                    return Invocation::failed(
                        "skill load needs a name",
                        "call `skill` with `action` = \"load\" and `name` set to one of \
                         the names `skill` action=\"list\" reported.",
                    );
                };
                match self.registry.load(name) {
                    Some(body) => Invocation::ok(body.to_string()),
                    None => Invocation::failed(
                        format!("no skill named `{name}`"),
                        format!(
                            "the loaded skills are: {}",
                            self.registry.names().join(", ")
                        ),
                    ),
                }
            }
            other => Invocation::failed(
                format!("unknown skill action `{other}`"),
                "call `skill` with `action` = \"list\" or \"load\".",
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> SkillRegistry {
        SkillRegistry::new(vec![Skill::new(
            "commit",
            "write a conventional commit",
            "Commit messages follow conventional-commits: type(scope): summary.",
        )])
    }

    #[test]
    fn lists_and_loads() {
        let r = registry();
        assert_eq!(r.names(), vec!["commit"]);
        assert!(r.load("commit").unwrap().contains("conventional-commits"));
        assert!(r.load("nope").is_none());
        assert!(r.list().contains("commit"), "{}", r.list());
    }
}
