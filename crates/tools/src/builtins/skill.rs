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
