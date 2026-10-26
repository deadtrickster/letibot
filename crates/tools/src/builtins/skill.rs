//! `skill` — named, loadable capabilities (opencode's skills).
//!
//! A skill is a name, a one-line description and a body of instructions. The tool
//! lists what is loaded, or loads one by name so its instructions reach the model.
//! Session-scoped: it changes only what the model knows, nothing the operator owns.
//!
//! # Two sources, one tool
//!
//! Skills on **disk** (`SKILL.md` under `~/.claude/skills`, `~/.config/letibot/skills`)
//! are read once at startup. Skills on the **shelf** — the fabric's rows of
//! `kind=skill`, written by other seats and read from every project — are behind
//! a [`SkillShelf`] the daemon installs when it holds a seat. Measured on lab2x1,
//! 2026-09-14: 76 hand-built curls to `/api/artifacts` in one seat's transcripts,
//! half of them for `kind=skill`, plus guessed filters that answer nothing
//! (`type=skill`, `/api/search?type=skill`). A skill on the shelf is loaded the
//! way a skill on disk is, by name, through this tool — and `list` says which
//! is which.

use std::sync::{Arc, Mutex};

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
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        body: impl Into<String>,
    ) -> Self {
        Skill {
            name: name.into(),
            description: description.into(),
            body: body.into(),
        }
    }
}

/// One entry on a shelf: enough to list, not the body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShelfEntry {
    pub id: String,
    pub title: String,
    /// Who may read it — `fabric`, `shared`, `project` — as the shelf says.
    pub visibility: String,
}

/// Somewhere skills live that is not this disk. The fabric is the one
/// implementation; the trait is here so the tool does not depend on it.
pub trait SkillShelf: Send + Sync {
    /// One phrase for a listing: where this shelf is.
    fn describe(&self) -> String;
    /// Every skill on the shelf. An error is an honest absence — the node is
    /// away — and is reported as such, never as an empty shelf.
    fn list(&self) -> Result<Vec<ShelfEntry>, String>;
    /// One skill by id or by title (case-insensitive), with its body.
    fn load(&self, key: &str) -> Result<Skill, String>;
}

/// The loaded skills, shared with the tool. The disk half is immutable after
/// construction; the shelf is installed once, by the daemon, when it has one.
#[derive(Default)]
pub struct SkillRegistry {
    pub skills: Vec<Skill>,
    shelf: Mutex<Option<Arc<dyn SkillShelf>>>,
}

impl std::fmt::Debug for SkillRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SkillRegistry")
            .field("skills", &self.skills)
            .field("shelf", &self.shelf().map(|s| s.describe()))
            .finish()
    }
}

impl SkillRegistry {
    pub fn new(skills: Vec<Skill>) -> Self {
        SkillRegistry {
            skills,
            shelf: Mutex::new(None),
        }
    }

    /// Install the shelf. Once: a second shelf replaces the first and says so in
    /// the return, because two shelves under one tool would be two answers to
    /// "which skills are there".
    pub fn set_shelf(&self, shelf: Arc<dyn SkillShelf>) -> Option<Arc<dyn SkillShelf>> {
        self.shelf
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .replace(shelf)
    }

    pub fn shelf(&self) -> Option<Arc<dyn SkillShelf>> {
        self.shelf.lock().unwrap_or_else(|e| e.into_inner()).clone()
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
    ///
    /// **A skill compiled into this build is loaded first** ([`Self::built_in`]), so a box that
    /// installs nothing still has the ones this tree ships — and a name on disk REPLACES the
    /// built-in of the same name, so an operator who wants a different `plugin-pane` writes one
    /// and theirs wins.
    pub fn load_default() -> Self {
        let mut dirs: Vec<std::path::PathBuf> = Vec::new();
        if let Ok(home) = std::env::var("HOME") {
            dirs.push(std::path::PathBuf::from(&home).join(".claude/skills"));
            dirs.push(std::path::PathBuf::from(&home).join(".config/letibot/skills"));
        }
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            dirs.push(std::path::PathBuf::from(xdg).join("letibot/skills"));
        }
        Self::load_onto(Self::built_in(), &dirs)
    }

    /// **The skills this build carries**, compiled in from this repository's `skills/`.
    ///
    /// The one that matters is `plugin-pane` (`docs/plugins-plan.md` §5): the walk-through of
    /// writing a pane in a session, from the operator's request to the pane on their screen. It is
    /// compiled in rather than looked for beside the binary because a skill that is *nearly*
    /// always there is worse than one that is always there — a model that finds `skill list`
    /// empty concludes the loop is not available in this build.
    pub fn built_in() -> Vec<Skill> {
        [(
            "plugin-pane",
            include_str!("../../../../skills/plugin-pane/SKILL.md"),
        )]
        .into_iter()
        .filter_map(|(what, text)| {
            let skill = from_text(text);
            if skill.is_none() {
                // A compiled-in skill with no frontmatter is a defect in this crate, and it
                // is said in the one place a person would look rather than swallowed.
                eprintln!("letibot: the built-in skill `{what}` has no frontmatter");
            }
            skill
        })
        .collect()
    }

    /// Load skills from each directory; a later directory's skill of the same name
    /// wins, so `letibot/skills` can override a `.claude` skill.
    pub fn load_from_dirs(dirs: &[std::path::PathBuf]) -> Self {
        Self::load_onto(Vec::new(), dirs)
    }

    /// [`Self::load_from_dirs`] with skills seeded first, so a name on disk replaces a built-in of
    /// the same name and nothing is held twice.
    pub fn load_onto(mut skills: Vec<Skill>, dirs: &[std::path::PathBuf]) -> Self {
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
        SkillRegistry::new(skills)
    }

    pub fn names(&self) -> Vec<&str> {
        self.skills.iter().map(|s| s.name.as_str()).collect()
    }

    /// A skill on disk, by name.
    pub fn load(&self, name: &str) -> Option<&str> {
        self.skills
            .iter()
            .find(|s| s.name == name)
            .map(|s| s.body.as_str())
    }

    /// A skill from either source: disk by name first, then the shelf by id or
    /// title. `Err` carries the reason a shelf could not answer, which is a
    /// different fact from "no such skill".
    pub fn load_any(&self, key: &str) -> Result<Skill, String> {
        if let Some(s) = self.skills.iter().find(|s| s.name == key) {
            return Ok(s.clone());
        }
        let Some(shelf) = self.shelf() else {
            return Err(format!(
                "no skill named `{key}` on disk, and no shelf is attached (the daemon holds \
                 no seat, so the fabric's skills are not reachable from here). On disk: {}",
                self.names_or_none()
            ));
        };
        shelf.load(key).map_err(|e| {
            format!(
                "no skill named `{key}` on disk, and the shelf ({}) says: {e}. On disk: {}",
                shelf.describe(),
                self.names_or_none()
            )
        })
    }

    fn names_or_none(&self) -> String {
        if self.skills.is_empty() {
            "none".into()
        } else {
            self.names().join(", ")
        }
    }

    fn list(&self) -> String {
        let mut out = String::new();
        if self.skills.is_empty() {
            out.push_str("no skills are loaded — none on disk and none in this build.\n");
        } else {
            out.push_str(&format!(
                "{} skill(s) loaded — this build's own, then yours (a name on disk replaces its \
                 built-in):\n",
                self.skills.len()
            ));
            for s in &self.skills {
                out.push_str(&format!("- {}: {}\n", s.name, s.description));
            }
        }
        match self.shelf() {
            None => out.push_str(
                "no shelf: the daemon holds no seat, so the fabric's skills are not \
                 reachable from here.\n",
            ),
            Some(shelf) => match shelf.list() {
                Ok(entries) if entries.is_empty() => {
                    out.push_str(&format!("the shelf ({}) is empty.\n", shelf.describe()));
                }
                Ok(entries) => {
                    out.push_str(&format!(
                        "{} skill(s) on the shelf ({}); load one by id or title:\n",
                        entries.len(),
                        shelf.describe()
                    ));
                    for e in entries {
                        out.push_str(&format!("- {}  [{}]  {}\n", e.id, e.visibility, e.title));
                    }
                }
                Err(e) => out.push_str(&format!(
                    "the shelf ({}) could not be read: {e}. That is not an empty shelf.\n",
                    shelf.describe()
                )),
            },
        }
        out
    }
}

/// One skill directory's `SKILL.md`: frontmatter `name` + `description`, then the
/// body. `None` when the file is absent or the frontmatter cannot be read — a
/// malformed skill is skipped, not loaded half-way.
fn load_one(dir: &std::path::Path) -> Option<Skill> {
    from_text(&std::fs::read_to_string(dir.join("SKILL.md")).ok()?)
}

/// **One `SKILL.md`, parsed** — the one reader both sources go through, so a skill compiled into
/// this build and a skill on the operator's disk cannot mean different things by the same file.
fn from_text(text: &str) -> Option<Skill> {
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
            "List the available skills — on disk and on the fabric's shelf — or load \
             one to read its instructions into the conversation. Give `action` \
             \"list\" to see them, or `action` \"load\" with `name` (a disk skill's \
             name, or a shelf skill's id or title) to load that skill.",
            json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["list", "load"]},
                    "name": {"type": "string", "description": "The skill to load, for action \"load\": a name from the disk list, or an id or title from the shelf list."}
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
                match self.registry.load_any(name) {
                    Ok(skill) => Invocation::ok(format!(
                        "# {}\n{}\n\n{}",
                        skill.name,
                        if skill.description.is_empty() {
                            String::new()
                        } else {
                            skill.description.clone()
                        },
                        skill.body
                    )),
                    Err(why) => Invocation::failed(format!("no skill named `{name}`"), why),
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

    /// **The plugin loop is compiled into this build**, so a box that installs nothing still has
    /// the walk-through — and a name on disk replaces it rather than being held twice.
    #[test]
    fn the_plugin_loop_is_a_skill_this_build_carries() {
        let built = SkillRegistry::built_in();
        let pane = built
            .iter()
            .find(|s| s.name == "plugin-pane")
            .unwrap_or_else(|| panic!("no plugin-pane skill: {:?}", built.len()));
        assert!(!pane.description.is_empty(), "a listing needs its one line");
        // The body is the loop, not a table of contents: every step is named in it.
        for step in [
            "scaffold",
            "edit",
            "build",
            "test",
            "load",
            "disable",
            "plugin_hooks",
        ] {
            assert!(pane.body.contains(step), "the skill never names `{step}`");
        }
        // And the frontmatter is not in the body: a skill whose instructions open with `---`
        // teaches a model to paste YAML into a pane.
        assert!(!pane.body.trim_start().starts_with("---"));

        // A skill on disk of the same name wins, and there is still only one of it.
        let dir = std::env::temp_dir().join(format!(
            "letibot-skills-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let mine = dir.join("plugin-pane");
        std::fs::create_dir_all(&mine).expect("a skill dir");
        std::fs::write(
            mine.join("SKILL.md"),
            "---\nname: plugin-pane\ndescription: mine\n---\n\nmy own loop\n",
        )
        .expect("a skill");
        let loaded =
            SkillRegistry::load_onto(SkillRegistry::built_in(), std::slice::from_ref(&dir));
        let named: Vec<&Skill> = loaded
            .skills
            .iter()
            .filter(|s| s.name == "plugin-pane")
            .collect();
        assert_eq!(named.len(), 1, "the built-in is replaced, not duplicated");
        assert_eq!(named[0].description, "mine");
        assert!(loaded.load("plugin-pane").unwrap().contains("my own loop"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lists_and_loads() {
        let r = registry();
        assert_eq!(r.names(), vec!["commit"]);
        assert!(r.load("commit").unwrap().contains("conventional-commits"));
        assert!(r.load("nope").is_none());
        assert!(r.list().contains("commit"), "{}", r.list());
        assert!(r.list().contains("no shelf"), "{}", r.list());
        assert!(
            r.load_any("nope")
                .unwrap_err()
                .contains("no shelf is attached")
        );
    }

    struct FakeShelf(Result<Vec<ShelfEntry>, String>);

    impl SkillShelf for FakeShelf {
        fn describe(&self) -> String {
            "a fake node".into()
        }
        fn list(&self) -> Result<Vec<ShelfEntry>, String> {
            self.0.clone()
        }
        fn load(&self, key: &str) -> Result<Skill, String> {
            let entries = self.0.clone()?;
            entries
                .iter()
                .find(|e| e.id == key || e.title.eq_ignore_ascii_case(key))
                .map(|e| Skill::new(e.title.clone(), "", format!("body of {}", e.id)))
                .ok_or_else(|| format!("nothing on the shelf is called `{key}`"))
        }
    }

    #[test]
    fn a_shelf_skill_loads_by_id_or_title_and_the_disk_wins_a_name_clash() {
        let r = registry();
        r.set_shelf(Arc::new(FakeShelf(Ok(vec![
            ShelfEntry {
                id: "01M1".into(),
                title: "doing diagrams".into(),
                visibility: "fabric".into(),
            },
            ShelfEntry {
                id: "01M2".into(),
                title: "commit".into(),
                visibility: "project".into(),
            },
        ]))));
        let l = r.list();
        assert!(l.contains("2 skill(s) on the shelf (a fake node)"), "{l}");
        assert!(l.contains("- 01M1  [fabric]  doing diagrams"), "{l}");
        assert_eq!(r.load_any("01M1").unwrap().body, "body of 01M1");
        assert_eq!(r.load_any("Doing Diagrams").unwrap().body, "body of 01M1");
        // The disk skill of the same name is the one loaded: it is the operator's.
        assert!(
            r.load_any("commit")
                .unwrap()
                .body
                .contains("conventional-commits")
        );
        let e = r.load_any("nope").unwrap_err();
        assert!(e.contains("nothing on the shelf is called `nope`"), "{e}");
    }

    #[test]
    fn a_shelf_that_cannot_be_read_is_not_an_empty_shelf() {
        let r = registry();
        r.set_shelf(Arc::new(FakeShelf(Err("node unreachable".into()))));
        let l = r.list();
        assert!(
            l.contains("could not be read: node unreachable. That is not an empty shelf"),
            "{l}"
        );
    }
}
