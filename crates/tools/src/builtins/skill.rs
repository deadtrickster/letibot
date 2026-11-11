//! `skill`: named, loadable capabilities.
//! The prompt carries one line per skill; a body arrives only when something asks. Each carries a
//! [`SkillSource`], because only the operator's own directory is theirs and the index shows it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use letibot_tokencore::Vocab;
use serde_json::json;

use crate::runtime::{Invocation, InvokeCtx, Tool};
use crate::schema::{Access, ToolSchema};

/// The most skills one prompt lists, because a box can hold hundreds.
pub const INDEX_MAX_ROWS: usize = 60;

/// The index's byte cap, for the same reason: a description is the author's words and one of
/// them can be a paragraph.
pub const INDEX_MAX_BYTES: usize = 4_000;

/// The index's share of the prompt, measured with the session's own token counter. It is spent
/// beside the standing notes and not out of them: a corpus of notes must not eat the skills.
pub const INDEX_BUDGET_TOKENS: usize = 800;

/// The floor a free-text `skill find` must clear before it loads anything. Asking is a decision the
/// model has already made, so it may be looser than the offer's own floor, which speaks unasked.
pub const QUERY_FLOOR: f64 = 0.15;

/// How many ranked names `skill find` scores before it keeps three. The extra room is for skills
/// this seat cannot use: they are skipped, and the one that fits must not lose its slot to them.
const FINDS_ASKED: usize = 8;

/// Where a skill was found, the fact the index shows beside every name. A label, never a
/// permission: a skill cannot grant itself anything by where it sits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillSource {
    /// `<workspace>/.letibot/skills` or `<workspace>/.claude/skills`.
    Project,
    /// `~/.config/letibot/skills`: the directory only this seat reads, so the operator's own.
    Operator,
    /// `~/.claude/skills`, shared with installers; `Some` is the origin the folder names.
    Installed(Option<String>),
    /// Compiled into this binary.
    BuiltIn,
    /// A row of the fabric's shelf: another seat's, read over the node.
    Fabric,
    /// A skill a model wrote, in `<data dir>/skills`, where no person edits by hand.
    Model,
}

impl SkillSource {
    /// How the index and the listing name it.
    pub fn label(&self) -> String {
        match self {
            SkillSource::Project => "this project".into(),
            SkillSource::Operator => "the operator's".into(),
            SkillSource::Installed(None) => "installed".into(),
            SkillSource::Installed(Some(origin)) => format!("installed from {origin}"),
            SkillSource::BuiltIn => "this build".into(),
            SkillSource::Fabric => "the fabric".into(),
            SkillSource::Model => "written by a model".into(),
        }
    }

    /// Which name wins a clash, and where a truncated index cuts. Lower is first, and `Installed`
    /// sits ahead of `BuiltIn`: a person's copy of a built-in's name replaces it, labelled or not.
    pub fn priority(&self) -> u8 {
        match self {
            SkillSource::Project => 0,
            SkillSource::Operator => 1,
            SkillSource::Installed(_) => 2,
            SkillSource::BuiltIn => 3,
            SkillSource::Fabric => 4,
            SkillSource::Model => 5,
        }
    }
}

/// One skill.
#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub body: String,
    pub source: SkillSource,
    /// Claude Code's `allowed-tools`, when the skill declares it: names, lowercased, with any
    /// `(...)` qualifier dropped. It can only narrow a session (`tool_ceiling`).
    pub allowed_tools: Option<Vec<String>>,
    /// The frontmatter's `tools`, lowercased: read only to tell which tools the skill needs.
    pub tools: Vec<String>,
    /// The frontmatter's `when_to_use`, when the author wrote one: the sentence a reader decides by
    /// and a ranker needs, since a `description` is often a title.
    pub when_to_use: Option<String>,
    /// The frontmatter's `keywords`, split on commas and spaces: the words a subject reaches a
    /// skill's subject by when the prose around them differs.
    pub keywords: Vec<String>,
    /// The frontmatter's `requires`: capabilities this seat must have for the skill to apply at
    /// all. A capability is a fact about the seat and never something a skill can grant itself.
    pub requires: Vec<String>,
    /// The frontmatter's `unless`: capabilities whose PRESENCE hides the skill - two skills for one
    /// job, and which one fits is whichever this seat can actually do.
    pub unless: Vec<String>,
    /// The `SKILL.md` it was read from; `None` for a skill compiled in or held on a shelf.
    pub path: Option<PathBuf>,
}

impl Skill {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        body: impl Into<String>,
        source: SkillSource,
    ) -> Self {
        Skill {
            name: name.into(),
            description: description.into(),
            body: body.into(),
            source,
            allowed_tools: None,
            tools: Vec::new(),
            when_to_use: None,
            keywords: Vec::new(),
            requires: Vec::new(),
            unless: Vec::new(),
            path: None,
        }
    }

    /// Why this seat cannot use the skill, or `None` when it can. `seat` is the lowercased names of
    /// the tools the session has. `requires`/`unless` are checked for every source; the `mcp__` and
    /// connector reads are a skill AUTHOR's guess about a server, so a built-in skips them.
    pub fn unmet(&self, seat: &BTreeSet<String>) -> Option<String> {
        self.unmet_with(seat, &BTreeMap::new())
    }

    /// [`unmet`](Self::unmet) with the operator's `[skills.when.<name>]`: a table for this name
    /// REPLACES what the frontmatter says, because an operator who has the seat knows and the
    /// author guessed.
    pub fn unmet_with(
        &self,
        seat: &BTreeSet<String>,
        when: &BTreeMap<String, SkillWhen>,
    ) -> Option<String> {
        let own = when.get(&self.name);
        let requires = own.map(|w| &w.requires[..]).unwrap_or(&self.requires);
        let unless = own.map(|w| &w.unless[..]).unwrap_or(&self.unless);
        // The unconditional half first: it is the one an operator's file also speaks about, and
        // the sentence a listing shows must be the same one a load refuses with.
        if let Some(missing) = requires.iter().find(|n| !capability(seat, n)) {
            return Some(format!(
                "requires `{missing}`, which this seat does not have"
            ));
        }
        if let Some(present) = unless.iter().find(|n| capability(seat, n)) {
            return Some(format!("hidden while `{present}` is on this seat"));
        }
        self.unmet_for_an_author(seat)
    }

    /// The half an author's own words are read for: `mcp__` servers and `the X connector`.
    fn unmet_for_an_author(&self, seat: &BTreeSet<String>) -> Option<String> {
        if matches!(self.source, SkillSource::BuiltIn) {
            return None;
        }
        let has = |server: &str| {
            let prefix = format!("mcp__{}__", server.to_ascii_lowercase());
            seat.iter().any(|t| t.starts_with(&prefix))
        };
        // Declared tools are all needed, one server at a time.
        let mut declared: Vec<String> = Vec::new();
        for t in self.allowed_tools.iter().flatten().chain(&self.tools) {
            if let Some(server) = mcp_server(t)
                && !declared.contains(&server)
            {
                declared.push(server);
            }
        }
        if let Some(missing) = declared.iter().find(|s| !has(s)) {
            return Some(format!(
                "declares tools of `mcp__{missing}__*`, none on this seat"
            ));
        }
        // Named in the body: the names are alternatives, one present is enough.
        let mut named: Vec<String> = Vec::new();
        let mut rest = self.body.as_str();
        while let Some(at) = rest.find("mcp__") {
            let tail = &rest[at..];
            let end = tail
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
                .unwrap_or(tail.len());
            if let Some(server) = mcp_server(&tail[..end])
                && !named.contains(&server)
            {
                named.push(server);
            }
            rest = &tail[end.max(5)..];
        }
        if !named.is_empty() && !named.iter().any(|s| has(s)) {
            let list: Vec<String> = named.iter().map(|s| format!("mcp__{s}__*")).collect();
            return Some(format!(
                "names tools {} and this seat has none",
                list.join(", ")
            ));
        }
        // "the docs connector": a connector word must appear in some mcp tool name on the seat.
        let lower = self.body.to_ascii_lowercase();
        let mut words: Vec<&str> = Vec::new();
        let mut prev = "";
        for w in lower.split_whitespace() {
            if w.trim_matches(|c: char| !c.is_ascii_alphanumeric()) == "connector"
                && prev.len() > 1
                && prev != "the"
                && prev.chars().all(|c| c.is_ascii_alphanumeric())
            {
                words.push(prev);
            }
            prev = w;
        }
        let connector_ok = |w: &str| seat.iter().any(|t| t.starts_with("mcp__") && t.contains(w));
        if let Some(w) = words.iter().find(|w| !connector_ok(w))
            && !words.iter().any(|w| connector_ok(w))
        {
            return Some(format!(
                "needs the `{w}` connector, no such tools on this seat"
            ));
        }
        None
    }

    /// What this skill is scored on: its whole frontmatter, never its body. A one-line
    /// `description` cannot carry "beamer" to a prompt that says "latex presentation", and the
    /// body is thousands of words that would drown the subject rather than describe it.
    pub fn document(&self) -> String {
        let mut out = format!("{} {}", self.name, self.description);
        if let Some(when) = &self.when_to_use {
            out.push(' ');
            out.push_str(when);
        }
        for keyword in &self.keywords {
            out.push(' ');
            out.push_str(keyword);
        }
        out
    }
}

/// What a skill needs of a seat before it applies, as `[skills.when.<name>]` in a `leticode.toml`
/// writes it. Operator configuration rather than a skill's words, so it is the same type both sides
/// read: a table here REPLACES the skill's own frontmatter for that name, whole.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillWhen {
    /// Every one of these must be on the seat, or the skill is not for it.
    #[serde(default)]
    pub requires: Vec<String>,
    /// Any one of these on the seat hides the skill.
    #[serde(default)]
    pub unless: Vec<String>,
}

impl SkillWhen {
    /// Whether the table says anything. An empty one unhides what the frontmatter hid, which is
    /// the operator's call and not a mistake this crate refuses.
    pub fn is_empty(&self) -> bool {
        self.requires.is_empty() && self.unless.is_empty()
    }
}

/// Whether this seat has a capability a skill named: a tool it seats (`bash`, `flowy`), or an
/// `mcp__server` / `mcp__server__*`, which is any tool of that server.
fn capability(seat: &BTreeSet<String>, name: &str) -> bool {
    match mcp_server(name) {
        Some(server) => {
            let prefix = format!("mcp__{server}__");
            seat.iter().any(|t| t.starts_with(&prefix))
        }
        None => seat.contains(&name.to_ascii_lowercase()),
    }
}

/// Every directory a skill can be found in, by source. The shape exists so the crate that owns
/// the policy names the directories, and this crate only reads them.
#[derive(Debug, Clone, Default)]
pub struct SkillDirs {
    pub project: Vec<PathBuf>,
    pub operator: Vec<PathBuf>,
    pub installed: Vec<PathBuf>,
    pub model: Vec<PathBuf>,
}

impl SkillDirs {
    /// This box's: the project's two conventions, the operator's own, the shared install directory,
    /// and the data dir a model writes into. The one reader of `$HOME` here, called once at
    /// startup, so a fixture's skills do not become a claim about whose home the suite runs in.
    pub fn for_this_box(workspace: Option<&Path>, data: Option<&Path>) -> SkillDirs {
        let mut dirs = SkillDirs::default();
        if let Some(ws) = workspace {
            dirs.project = project_roots(ws);
        }
        if let Ok(config) = std::env::var("XDG_CONFIG_HOME") {
            dirs.operator
                .push(PathBuf::from(config).join("letibot/skills"));
        }
        if let Ok(home) = std::env::var("HOME") {
            dirs.operator
                .push(PathBuf::from(&home).join(".config/letibot/skills"));
            dirs.installed
                .push(PathBuf::from(&home).join(".claude/skills"));
        }
        if let Some(data) = data {
            dirs.model.push(data.join("skills"));
        }
        dirs
    }

    /// These roots with the project's half taken from `workspace`: the box's half travels on a
    /// session's config, and the project's is whichever tree that session opened in.
    pub fn in_workspace(&self, workspace: &Path) -> SkillDirs {
        let mut dirs = self.clone();
        dirs.project = project_roots(workspace);
        dirs
    }

    /// Every directory with the source it gives its skills, in priority order.
    fn walk(&self) -> Vec<(&PathBuf, SkillSource)> {
        let mut out: Vec<(&PathBuf, SkillSource)> = Vec::new();
        out.extend(self.project.iter().map(|d| (d, SkillSource::Project)));
        out.extend(self.operator.iter().map(|d| (d, SkillSource::Operator)));
        for dir in &self.installed {
            out.push((dir, SkillSource::Installed(None)));
        }
        out.extend(self.model.iter().map(|d| (d, SkillSource::Model)));
        out
    }
}

/// The two conventions a project's own skills are found under, in the order they are read.
pub fn project_roots(workspace: &Path) -> Vec<PathBuf> {
    vec![
        workspace.join(".letibot/skills"),
        workspace.join(".claude/skills"),
    ]
}

/// One line of the prompt's skills index: a name, its one line, and where it was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexRow {
    pub name: String,
    /// The shelf's own id, when this row came from one. `skill list` prints it first, so it is what
    /// an operator copies into `[skills] pin`, and [`SkillShelf::load`] takes it.
    pub id: Option<String>,
    pub description: String,
    pub source: SkillSource,
    /// The skill's `SKILL.md`, when it is a file on this box.
    pub path: Option<PathBuf>,
}

impl IndexRow {
    /// Whether this row is the one `pin` names: by its name, or by the id the listing leads with.
    pub fn answers_to(&self, pin: &str) -> bool {
        self.name == pin || self.id.as_deref() == Some(pin)
    }
}

/// One entry on a shelf: enough to list, not the body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShelfEntry {
    pub id: String,
    pub title: String,
    /// Who may read it (`fabric`, `shared`, `project`), as the shelf says.
    pub visibility: String,
}

/// Somewhere skills live that is not this disk. The fabric is the one
/// implementation; the trait is here so the tool does not depend on it.
pub trait SkillShelf: Send + Sync {
    /// One phrase for a listing: where this shelf is.
    fn describe(&self) -> String;
    /// Every skill on the shelf. An error is an honest absence (the node is away) and is
    /// reported as such, never as an empty shelf.
    fn list(&self) -> Result<Vec<ShelfEntry>, String>;
    /// One skill by id or by title (case-insensitive), with its body.
    fn load(&self, key: &str) -> Result<Skill, String>;
}

/// Said wherever the shelf is missing, so a model does not read "no shelf" as "no skills".
const DISK_DIRS: &str = "Skills on disk still load from the project's \
    .letibot/skills and .claude/skills, ~/.config/letibot/skills and ~/.claude/skills.";

/// The loaded skills, shared with the tool. `skills` is the snapshot taken at open and feeds the
/// prompt; the tool re-reads the folders per call. The shelf is installed by the daemon.
#[derive(Default)]
pub struct SkillRegistry {
    pub skills: Vec<Skill>,
    /// Where `skills` was read from, so the tool's live answers can read the folders again. The
    /// prompt's index stays on `skills`: a pin is fixed at open.
    dirs: Option<SkillDirs>,
    /// The names the operator pinned for this project: the only rows the prompt's index carries.
    /// A pin is not a load - the body still arrives only when `skill` is asked for it.
    pins: BTreeSet<String>,
    /// `[skills.when.<name>]`, both levels resolved: what an operator says a skill needs of a seat,
    /// over what its frontmatter says. Read here because every door asks it.
    when: BTreeMap<String, SkillWhen>,
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
            dirs: None,
            pins: BTreeSet::new(),
            when: BTreeMap::new(),
            shelf: Mutex::new(None),
        }
    }

    /// The registry with the operator's `[skills.when.<name>]` tables. A table REPLACES its
    /// skill's own frontmatter whole, `requires` and `unless` together, so an operator cannot
    /// half-overrule an author and leave the other half standing.
    pub fn with_when(mut self, when: BTreeMap<String, SkillWhen>) -> Self {
        self.when = when;
        self
    }

    /// Why this seat cannot use this skill, or `None`. The one answer every door reads, so a name
    /// the listing hides is a name `find` does not offer and `load` refuses - with this sentence.
    pub fn unmet(&self, skill: &Skill, seat: &BTreeSet<String>) -> Option<String> {
        skill.unmet_with(seat, &self.when)
    }

    /// The registry with the operator's pins, set once before it is shared: the prompt is composed
    /// from it at open, and a pin that changed mid-session would move the prefix.
    pub fn with_pins<I, S>(mut self, pins: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.pins = pins.into_iter().map(Into::into).collect();
        self
    }

    /// The names the prompt lists in full: the set is what `index` reads, and what
    /// [`Self::unknown_pins`] is measured against.
    pub fn pins(&self) -> &BTreeSet<String> {
        &self.pins
    }

    /// The pinned names nothing on this box answers to, sorted. A pin is the operator's own sentence,
    /// and a typo in one is silent without this: both the prompt and the session's opening say it.
    pub fn unknown_pins(&self) -> Vec<&str> {
        let (rows, _) = self.index_rows();
        self.pins
            .iter()
            .filter(|pin| !rows.iter().any(|r| r.answers_to(pin)))
            .map(String::as_str)
            .collect()
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

    /// Load the skills this box has, by where they were found: the one loader every door uses. A
    /// name found twice is held once, by the higher-priority source, and a directory that cannot be
    /// read is skipped rather than fatal.
    pub fn load_for(dirs: &SkillDirs) -> Self {
        let mut reg = SkillRegistry::new(scan(dirs));
        reg.dirs = Some(dirs.clone());
        reg
    }

    /// The skills as the disk has them now: built-ins plus a fresh read of the folders. Without
    /// folders (a registry built from a list) it is the list as given.
    pub fn live_skills(&self) -> Vec<Skill> {
        match &self.dirs {
            Some(dirs) => scan(dirs),
            None => self.skills.clone(),
        }
    }

    /// This box's directories ([`SkillDirs::for_this_box`]) with nothing named: the default a
    /// caller with no workspace has, so no project and no data dir.
    pub fn load_default() -> Self {
        Self::load_for(&SkillDirs::for_this_box(None, None))
    }

    /// The skills this build carries, compiled in from this repository's `skills/`: a model that
    /// finds `skill list` empty concludes the loop is not available in this build.
    pub fn built_in() -> Vec<Skill> {
        [
            (
                "plugin-pane",
                include_str!("../../../../skills/plugin-pane/SKILL.md"),
            ),
            // The four patch points: which kind of harness problem lives at which, and the
            // same loop. Separate from the pane's because a model's first question is which
            // hook, and a page answering both would bury it.
            (
                "plugin-hooks",
                include_str!("../../../../skills/plugin-hooks/SKILL.md"),
            ),
            // How to look at a UI change with the `screen` tool.
            ("screen", include_str!("../../../../skills/screen/SKILL.md")),
        ]
        .into_iter()
        .filter_map(|(what, text)| {
            let skill = from_text(text, SkillSource::BuiltIn);
            if skill.is_none() {
                // A compiled-in skill with no frontmatter is a defect in this crate, and it
                // is said in the one place a person would look rather than swallowed.
                eprintln!("letibot: the built-in skill `{what}` has no frontmatter");
            }
            skill
        })
        .collect()
    }

    /// Load skills from each directory as the operator's; a later directory's skill of the same
    /// name wins, so `letibot/skills` can override a `.claude` skill.
    pub fn load_from_dirs(dirs: &[std::path::PathBuf]) -> Self {
        Self::load_onto(Vec::new(), dirs)
    }

    /// [`Self::load_from_dirs`] with skills seeded first, so a name on disk replaces a built-in of
    /// the same name and nothing is held twice.
    pub fn load_onto(mut skills: Vec<Skill>, dirs: &[std::path::PathBuf]) -> Self {
        for dir in dirs {
            for skill in read_dir(dir, &SkillSource::Operator) {
                match skills.iter_mut().find(|s| s.name == skill.name) {
                    Some(existing) => *existing = skill,
                    None => skills.push(skill),
                }
            }
        }
        sort_for_display(&mut skills);
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
        let live = self.live_skills();
        if let Some(s) = live.iter().find(|s| s.name == key) {
            return Ok(s.clone());
        }
        let Some(shelf) = self.shelf() else {
            return Err(format!(
                "no skill named `{key}` on disk, and no shelf is attached (no fabric seat on \
                 this daemon). {DISK_DIRS} On disk: {}",
                names_or_none(&live)
            ));
        };
        shelf.load(key).map_err(|e| {
            format!(
                "no skill named `{key}` on disk, and the shelf ({}) says: {e}. On disk: {}",
                shelf.describe(),
                names_or_none(&live)
            )
        })
    }

    /// Every skill there is, usable ones first and then the ones this seat cannot use, each with
    /// the reason. Naming the reason is the point: a model that silently loses a skill from the
    /// list cannot tell a rule from a bug, and picks the wrong one of two that answer one job.
    fn list(&self, seat: Option<&BTreeSet<String>>) -> String {
        let skills = self.live_skills();
        let why = |s: &Skill| seat.and_then(|seat| self.unmet(s, seat));
        let (hidden, usable): (Vec<&Skill>, Vec<&Skill>) =
            skills.iter().partition(|s| why(s).is_some());
        let mut out = String::new();
        if skills.is_empty() {
            out.push_str("no skills are loaded — none on disk and none in this build.\n");
        } else {
            out.push_str(&format!(
                "{} skill(s) — the label says where each was found, and only `the operator's` is \
                 an instruction of theirs:\n",
                usable.len()
            ));
            for s in &usable {
                out.push_str(&format!(
                    "- {}  [{}]  {}\n",
                    s.name,
                    s.source.label(),
                    s.description
                ));
            }
            if !hidden.is_empty() {
                out.push_str(&format!(
                    "{} skill(s) not for this seat: they stay out of the index and `load` refuses \
                     them, with the reason here\n",
                    hidden.len()
                ));
                for s in &hidden {
                    let reason = why(s).unwrap_or_default();
                    out.push_str(&format!("- {}  [{}]  {reason}\n", s.name, s.source.label()));
                }
            }
        }
        match self.shelf() {
            None => out.push_str(&format!(
                "no shelf is attached (no fabric seat on this daemon). {DISK_DIRS}\n"
            )),
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

    /// Every skill this box can reach, in the order the index lists them: the disk half by
    /// source priority, then the shelf's rows. A shelf that cannot be read comes back as the
    /// sentence, never as an empty shelf.
    pub fn index_rows(&self) -> (Vec<IndexRow>, Option<String>) {
        let mut rows: Vec<IndexRow> = self
            .skills
            .iter()
            .map(|s| IndexRow {
                name: s.name.clone(),
                id: None,
                description: s.description.clone(),
                source: s.source.clone(),
                path: s.path.clone(),
            })
            .collect();
        let mut said = None;
        match self.shelf() {
            None => {}
            Some(shelf) => match shelf.list() {
                Ok(entries) => rows.extend(entries.into_iter().map(|e| IndexRow {
                    name: e.title,
                    id: Some(e.id),
                    description: format!("({})", e.visibility),
                    source: SkillSource::Fabric,
                    path: None,
                })),
                Err(e) => {
                    said = Some(format!(
                        "the shelf ({}) could not be read: {e}",
                        shelf.describe()
                    ))
                }
            },
        }
        rows.sort_by(|a, b| {
            a.source
                .priority()
                .cmp(&b.source.priority())
                .then_with(|| a.name.cmp(&b.name))
        });
        (rows, said)
    }

    /// The index the prompt carries: the pinned skills one line each, and a count of the rest, or
    /// `None` when the box holds nothing. Never a source in full - the model reaches the rest with
    /// `skill list` and `skill find`.
    pub fn index(&self, vocab: &Vocab) -> Option<String> {
        let (rows, said) = self.index_rows();
        if rows.is_empty() && said.is_none() {
            return None;
        }
        let listed: Vec<&IndexRow> = rows
            .iter()
            .filter(|r| self.pins.iter().any(|pin| r.answers_to(pin)))
            .collect();
        let mut out = format!(
            "### skills — {} pinned of {}; `skill` loads a body, `skill list` every one, `skill \
             find` the nearest\n",
            listed.len(),
            rows.len()
        );
        let mut used = 0usize;
        let mut shown = 0usize;
        for row in listed {
            let line = format!(
                "- {}  [{}]  {}\n",
                row.name,
                row.source.label(),
                row.description
            );
            let cost = vocab
                .tokenize_text(&line)
                .map(|t| t.len())
                .unwrap_or(usize::MAX);
            let over = shown >= INDEX_MAX_ROWS
                || out.len() + line.len() > INDEX_MAX_BYTES
                || used + cost > INDEX_BUDGET_TOKENS;
            if over {
                break;
            }
            out.push_str(&line);
            used += cost;
            shown += 1;
        }
        if shown < rows.len() {
            out.push_str(&format!(
                "… and {} more that `skill list` holds\n",
                rows.len() - shown
            ));
        }
        // A pin nothing answers to is the operator's mistake, and the prompt says it rather than
        // silently listing one row fewer than was asked for.
        for name in self.unknown_pins() {
            out.push_str(&format!("pinned `{name}` names no skill on this box\n"));
        }
        if let Some(said) = said {
            out.push_str(&format!("{said}\n"));
        }
        Some(out)
    }

    /// The skills most like a free-text query: the same arithmetic the notes use, over each
    /// skill's whole frontmatter ([`Skill::document`]) - a `description` alone is a title, and a
    /// title cannot carry "beamer" to a query that says "latex presentation".
    pub fn ranked(&self, query: &str, most: usize) -> Vec<(String, f64)> {
        self.ranked_with(query, most, None)
    }

    /// [`ranked`](Self::ranked) with the box's second tier attached: a subject in none of a
    /// skill's words is offered on its meaning. `tier` is filled with this registry's documents,
    /// keyed by the name every door here keys by.
    pub fn ranked_with(
        &self,
        query: &str,
        most: usize,
        tier: Option<&crate::similarity::Tier>,
    ) -> Vec<(String, f64)> {
        let docs = self.documents(&self.live_skills());
        let corpus = crate::similarity::Corpus::of_texts(&docs);
        // Filled from THIS registry, through the door that keys it as the score reads it. A fill
        // that cannot answer leaves no vectors, so the words stand alone.
        let _ = corpus.fill(tier);
        // One name is one skill here - a pin, a `load` and an offer all key by it - so a second
        // document under the same name would be the same row twice in a request that carries both.
        let mut out: Vec<(String, f64)> = Vec::with_capacity(most.min(docs.len()));
        for (path, score) in corpus.ranked_with(query, usize::MAX, tier) {
            if out.len() >= most {
                break;
            }
            let name = path.display().to_string();
            if out.iter().any(|(seen, _)| *seen == name) {
                continue;
            }
            out.push((name, score));
        }
        out
    }

    /// What this registry is scored on, in the corpus's own shape: one document per skill, and
    /// one per shelf entry, whose title and visibility are all that is known about it.
    fn documents(&self, skills: &[Skill]) -> Vec<(PathBuf, String)> {
        let mut docs: Vec<(PathBuf, String)> = skills
            .iter()
            .map(|s| (PathBuf::from(&s.name), s.document()))
            .collect();
        if let Some(shelf) = self.shelf()
            && let Ok(entries) = shelf.list()
        {
            docs.extend(entries.into_iter().map(|e| {
                (
                    PathBuf::from(&e.title),
                    format!("{} {}", e.title, e.visibility),
                )
            }));
        }
        docs
    }

    /// The tools a set of declared skills allows: the union of their `allowed-tools`, or `None`
    /// when none of them declares one. It can only narrow: a name here is never a permission, and
    /// a skill that declares nothing restricts nothing.
    pub fn tool_ceiling(&self, names: &[String]) -> Option<BTreeSet<String>> {
        let mut out: BTreeSet<String> = BTreeSet::new();
        let mut declared = false;
        for name in names {
            let Some(skill) = self.skills.iter().find(|s| &s.name == name) else {
                continue;
            };
            if let Some(tools) = &skill.allowed_tools {
                declared = true;
                out.extend(tools.iter().cloned());
            }
        }
        declared.then_some(out)
    }
}

/// `mcp__server__tool*` or `mcp__server`: the server part, lowercased.
fn mcp_server(name: &str) -> Option<String> {
    let rest = name.to_ascii_lowercase();
    let rest = rest.strip_prefix("mcp__")?;
    let server = rest.split("__").next()?.trim_matches(['*', '_']);
    (!server.is_empty()).then(|| server.to_string())
}

fn names_or_none(skills: &[Skill]) -> String {
    if skills.is_empty() {
        "none".into()
    } else {
        skills
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Built-ins with the folders read over them: a name found twice is held once, by the
/// higher-priority source, and a directory that cannot be read is skipped.
fn scan(dirs: &SkillDirs) -> Vec<Skill> {
    let mut skills = SkillRegistry::built_in();
    for (dir, source) in dirs.walk() {
        let mut found = read_dir(dir, &source);
        // Claude Code's synced skills sit one level further down; only that shape is read.
        if matches!(source, SkillSource::Installed(_)) {
            found.extend(read_synced(dir, &source));
        }
        for skill in found {
            match skills.iter_mut().find(|s| s.name == skill.name) {
                Some(existing) if existing.source.priority() < skill.source.priority() => {
                    continue;
                }
                Some(existing) => *existing = skill,
                None => skills.push(skill),
            }
        }
    }
    sort_for_display(&mut skills);
    skills
}

/// `<root>/synced/<uuid>/<name>/SKILL.md`, in directory order made stable by the sort.
fn read_synced(root: &std::path::Path, source: &SkillSource) -> Vec<Skill> {
    let Ok(entries) = std::fs::read_dir(root.join("synced")) else {
        return Vec::new();
    };
    let mut groups: Vec<_> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    groups.sort();
    groups.iter().flat_map(|g| read_dir(g, source)).collect()
}

/// One skill directory's `SKILL.md`: frontmatter `name` + `description`, then the body. `None`
/// when the file is absent or the frontmatter cannot be read: a malformed skill is skipped, not
/// loaded half-way.
fn load_one(dir: &std::path::Path, source: &SkillSource) -> Option<Skill> {
    let skill = Skill {
        path: Some(dir.join("SKILL.md")),
        ..from_text(
            &std::fs::read_to_string(dir.join("SKILL.md")).ok()?,
            source.clone(),
        )?
    };
    Some(match source {
        // The folder says whose it is, or the label says only *installed*: `~/.claude/skills`
        // is where `npx skills add` and friends put other people's skills, so an unnamed one is
        // not the operator's words.
        SkillSource::Installed(_) => Skill {
            source: SkillSource::Installed(origin_of(dir)),
            ..skill
        },
        _ => skill,
    })
}

/// The origin an installed skill's folder names: a `.origin` marker, or `origin:` in the
/// frontmatter. `None` is the ordinary case and the label is then *installed*.
fn origin_of(dir: &std::path::Path) -> Option<String> {
    if let Ok(text) = std::fs::read_to_string(dir.join(".origin"))
        && let Some(line) = text
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with('#'))
    {
        return Some(line.to_string());
    }
    let text = std::fs::read_to_string(dir.join("SKILL.md")).ok()?;
    let front = text.strip_prefix("---")?.split_once("---")?.0;
    front.lines().find_map(|l| {
        l.trim()
            .strip_prefix("origin:")
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    })
}

/// Every skill in one directory, tagged with the source that directory gives them.
fn read_dir(dir: &std::path::Path, source: &SkillSource) -> Vec<Skill> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .filter_map(|p| load_one(&p, source))
        .collect()
}

/// The listing's order: where it came from, then its name. A function of the files alone, so
/// two runs of the same box show the same block and the prefix cache is not spent for nothing.
fn sort_for_display(skills: &mut [Skill]) {
    skills.sort_by(|a, b| {
        a.source
            .priority()
            .cmp(&b.source.priority())
            .then_with(|| a.name.cmp(&b.name))
    });
}

/// One `SKILL.md`, parsed: the one reader both sources go through, so a skill compiled into this
/// build and a skill on the operator's disk cannot mean different things by the same file.
fn from_text(text: &str, source: SkillSource) -> Option<Skill> {
    let body = text.strip_prefix("---")?;
    let (front, rest) = body.split_once("---")?;
    let mut name = None;
    let mut description = None;
    let mut allowed = None;
    let mut tools_line = Vec::new();
    let mut when = None;
    let mut keywords = Vec::new();
    let mut requires = Vec::new();
    let mut unless = Vec::new();
    // A block list is written under its key, one name a line, so the key that ended the last
    // value is remembered: `requires:` with nothing after it opens one.
    let mut open = Names::None;
    for line in front.lines() {
        let line = line.trim();
        if let Some(item) = line.strip_prefix("- ") {
            match open {
                Names::Requires => requires.extend(name_list(item)),
                Names::Unless => unless.extend(name_list(item)),
                Names::None => {}
            }
            continue;
        }
        open = Names::None;
        if let Some(v) = line.strip_prefix("name:") {
            name = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("description:") {
            description = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("allowed-tools:") {
            let tools = tool_names(v);
            if !tools.is_empty() {
                allowed = Some(tools);
            }
        } else if let Some(v) = line.strip_prefix("tools:") {
            tools_line = tool_names(v);
        } else if let Some(v) = line.strip_prefix("when_to_use:") {
            when = Some(v.trim().to_string()).filter(|v| !v.is_empty());
        } else if let Some(v) = line.strip_prefix("requires:") {
            requires = name_list(v);
            if v.trim().is_empty() {
                open = Names::Requires;
            }
        } else if let Some(v) = line.strip_prefix("unless:") {
            unless = name_list(v);
            if v.trim().is_empty() {
                open = Names::Unless;
            }
        } else if let Some(v) = line.strip_prefix("keywords:") {
            keywords = name_list(v);
        }
    }
    let name = name.filter(|n| !n.is_empty())?;
    Some(Skill {
        name,
        description: description.unwrap_or_default(),
        body: rest.trim_start().to_string(),
        source,
        allowed_tools: allowed,
        tools: tools_line,
        when_to_use: when,
        keywords,
        requires,
        unless,
        path: None,
    })
}

/// Which of the two name lists a block item below a key belongs to.
#[derive(Clone, Copy)]
enum Names {
    None,
    Requires,
    Unless,
}

/// One list of names as an author writes it: `flowy, bash`, `[flowy, bash]`, or quoted. The two
/// spellings `keywords` has always taken, so a new key does not invent a third.
fn name_list(list: &str) -> Vec<String> {
    list.trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split([',', ' '])
        .map(|w| w.trim().trim_matches(['"', '\'']).to_string())
        .filter(|w| !w.is_empty())
        .collect()
}

/// `allowed-tools: Read, Bash(git:*)`: the names only, lowercased, because the qualifier is Claude
/// Code's spelling of a narrower thing and this tree's tools answer to the bare name.
fn tool_names(list: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for word in list.split([',', ' ']) {
        let word = word.trim();
        let name = match word.split_once('(') {
            Some((name, _)) => name.trim(),
            None => word,
        };
        let name = name.to_ascii_lowercase();
        if !name.is_empty() && !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

/// The tools a session really seats, filled by its daemon once the prefix is composed. A cell and
/// not a value because the registry is the daemon's and the seat is the session's; unset (a
/// runtime with no daemon behind it) says nothing about the seat rather than refusing everything.
#[derive(Clone, Default)]
pub struct Seat(Arc<Mutex<Option<BTreeSet<String>>>>);

impl Seat {
    pub fn new() -> Seat {
        Seat::default()
    }

    /// Record the session's tool names. Lowercased here, because every name a skill declares is
    /// matched that way and a second spelling of the same tool is a second answer.
    pub fn set<I: IntoIterator<Item = S>, S: AsRef<str>>(&self, tools: I) {
        let set = tools
            .into_iter()
            .map(|t| t.as_ref().to_ascii_lowercase())
            .collect();
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(set);
    }

    /// The set, or `None` when nothing has told this cell what the seat holds.
    pub fn get(&self) -> Option<BTreeSet<String>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// The `skill` tool.
pub struct SkillTool {
    registry: Arc<SkillRegistry>,
    seat: Seat,
}

impl SkillTool {
    pub fn new(registry: Arc<SkillRegistry>) -> Self {
        SkillTool {
            registry,
            seat: Seat::new(),
        }
    }

    /// The cell the session's tool names arrive in. The daemon fills it after composing the
    /// prefix, since only then is the seat's own list final.
    pub fn with_seat(mut self, seat: Seat) -> Self {
        self.seat = seat;
        self
    }

    /// Why this seat cannot use the named skill. A shelf row is not looked up: its frontmatter is
    /// not read before it is loaded, and a name this cell cannot explain is not refused unseen.
    fn unmet(
        &self,
        seat: Option<&BTreeSet<String>>,
        skills: &[Skill],
        name: &str,
    ) -> Option<String> {
        let seat = seat?;
        let skill = skills.iter().find(|s| s.name == name)?;
        self.registry.unmet(skill, seat)
    }
}

impl Tool for SkillTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "skill",
            "The skills this box has: a name, one line and a body of instructions, each shown \
             with where it was found. `action` \"list\" shows every one; \"load\" with \
             `name` reads that body into the conversation; \"find\" with `query` (\"postgres \
             migration safety\") answers the nearest skill's body and names the next two — \
             a query nothing matches says so rather than loading something weak. A `query` \
             on its own is \"find\".",
            json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["list", "load", "find"]},
                    "name": {"type": "string", "description": "The skill to load, for action \"load\": a name from the index, or an id or title from the shelf list."},
                    "query": {"type": "string", "description": "What you want to do, in your own words, for action \"find\": the nearest skill is loaded for you."}
                },
                "required": []
            }),
            Access::Session,
        )
    }

    fn invoke(&self, _ctx: &mut InvokeCtx<'_>, args: &serde_json::Value) -> Invocation {
        let query = args.get("query").and_then(|v| v.as_str());
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .or(query.map(|_| "find"));
        let Some(action) = action else {
            return Invocation::failed(
                "skill needs an action",
                "call `skill` with `action` = \"list\", \"load\" (with `name`) or \"find\" \
                 (with `query`).",
            );
        };
        let seat = self.seat.get();
        let seat = seat.as_ref();
        match action {
            "list" => Invocation::ok(self.registry.list(seat)),
            "load" => {
                let Some(name) = args.get("name").and_then(|v| v.as_str()) else {
                    return Invocation::failed(
                        "skill load needs a name",
                        "call `skill` with `action` = \"load\" and `name` set to one of \
                         the names `skill` action=\"list\" reported.",
                    );
                };
                match self.registry.load_any(name) {
                    Ok(skill) => match seat.and_then(|st| self.registry.unmet(&skill, st)) {
                        Some(why) => Invocation::failed(
                            format!("`{name}` is not for this seat"),
                            format!(
                                "it {why}. Every door agrees: the listing shows it under `not for \
                                 this seat`, `find` skips it, and this load refuses it. `skill` \
                                 action=\"list\" names the skill that fits here."
                            ),
                        ),
                        None => Invocation::ok(loaded(&skill)),
                    },
                    Err(why) => Invocation::failed(format!("no skill named `{name}`"), why),
                }
            }
            // The nearest by the notes' own arithmetic, not a new matcher, and nothing is loaded
            // below the floor: a weak match spends the model's context on a page about something
            // else.
            "find" => {
                let Some(query) = query else {
                    return Invocation::failed(
                        "skill find needs a query",
                        "call `skill` with `action` = \"find\" and `query` set to what you want \
                         to do, in your own words.",
                    );
                };
                // Asked for more than the three answered, so a skill this seat cannot use does
                // not take one of the three slots from the one that fits here.
                let live = self.registry.live_skills();
                let ranked: Vec<(String, f64)> = self
                    .registry
                    .ranked(query, FINDS_ASKED)
                    .into_iter()
                    .filter(|(name, _)| self.unmet(seat, &live, name).is_none())
                    .take(3)
                    .collect();
                let Some((best, rest)) = ranked.split_first() else {
                    return Invocation::failed(
                        format!("nothing in the skills index is like `{query}`"),
                        "`skill` action=\"list\" shows every skill there is.",
                    );
                };
                let (name, score) = (best.0.clone(), best.1);
                if score < QUERY_FLOOR {
                    return Invocation::failed(
                        format!(
                            "nothing in the skills index is close to `{query}` (the nearest is \
                             `{name}` at {score:.2}, under {QUERY_FLOOR:.2})"
                        ),
                        "`skill` action=\"list\" shows every skill there is, and `action` \
                         =\"load\" with one of those names reads it anyway.",
                    );
                }
                let body = match self.registry.load_any(&name) {
                    Ok(skill) => loaded(&skill),
                    Err(why) => return Invocation::failed(format!("no skill named `{name}`"), why),
                };
                let next: Vec<String> = rest
                    .iter()
                    .map(|(name, s)| format!("{name} ({s:.2})"))
                    .collect();
                Invocation::ok(if next.is_empty() {
                    body
                } else {
                    format!("{body}\n\nnext nearest: {}", next.join(", "))
                })
            }
            other => Invocation::failed(
                format!("unknown skill action `{other}`"),
                "call `skill` with `action` = \"list\", \"load\" or \"find\".",
            ),
        }
    }
}

/// One skill as the tool hands it over: the name, its one line, where it was found, and the body.
fn loaded(skill: &Skill) -> String {
    format!(
        "# {}  [{}]\n{}\n\n{}",
        skill.name,
        skill.source.label(),
        skill.description,
        skill.body
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> SkillRegistry {
        SkillRegistry::new(vec![Skill::new(
            "commit",
            "write a conventional commit",
            "Commit messages follow conventional-commits: type(scope): summary.",
            SkillSource::Operator,
        )])
    }

    /// The plugin loop is compiled into this build, so a box that installs nothing still has the
    /// walk-through, and a name on disk replaces it rather than being held twice.
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

    /// The `plugin-hooks` skill teaches every hook kind the host asks, one example each - the wall
    /// was a skill naming four kinds and leaving out `tool_`, `command_` and `pane_`, so agents
    /// concluded in the room that a plugin cannot add a tool (R151).
    #[test]
    fn the_plugin_hooks_skill_names_every_hook_kind_the_host_serves() {
        let built = SkillRegistry::built_in();
        let skill = built
            .iter()
            .find(|s| s.name == "plugin-hooks")
            .expect("the plugin-hooks skill");
        for kind in letibot_plugins::SERVED_KINDS {
            assert!(
                skill.body.contains(&format!("`{kind}")),
                "the host asks `{kind}` and the skill never names it"
            );
        }
        // The two lists together are the host's prefixes, so a new kind cannot arrive without a
        // decision about this skill: it must be served (and taught) or named as never asked.
        let mut listed: Vec<&str> = letibot_plugins::SERVED_KINDS
            .iter()
            .chain(letibot_plugins::UNSERVED_KINDS.iter())
            .copied()
            .collect();
        listed.sort_unstable();
        let mut prefixes = letibot_plugins::HOOK_PREFIXES.to_vec();
        prefixes.sort_unstable();
        assert_eq!(
            listed, prefixes,
            "every prefix the host recognises is either served or named unserved"
        );
    }

    /// A project's skill is the project's, and an installed one is nobody's words: the two facts
    /// the index's labels exist for, over real directories.
    #[test]
    fn a_project_skill_is_found_and_an_installed_one_is_not_labelled_the_operators() {
        let root = std::env::temp_dir().join(format!("letibot-skilldirs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let project = root.join("project/.letibot/skills/release");
        let installed = root.join("installed/review");
        let op = root.join("operator/house-style");
        for (dir, name) in [
            (&project, "release"),
            (&installed, "review"),
            (&op, "house-style"),
        ] {
            std::fs::create_dir_all(dir).expect("a skill dir");
            std::fs::write(
                dir.join("SKILL.md"),
                format!("---\nname: {name}\ndescription: the {name} one\n---\n\nbody of {name}\n"),
            )
            .expect("a skill");
        }
        std::fs::write(installed.join(".origin"), "npx skills add\n").expect("an origin");
        let dirs = SkillDirs {
            project: vec![root.join("project/.letibot/skills")],
            operator: vec![root.join("operator")],
            installed: vec![root.join("installed")],
            model: Vec::new(),
        };
        let r = SkillRegistry::load_for(&dirs);
        let of = |name: &str| {
            r.skills
                .iter()
                .find(|s| s.name == name)
                .expect(name)
                .clone()
        };
        assert_eq!(of("release").source, SkillSource::Project);
        assert_eq!(of("house-style").source, SkillSource::Operator);
        assert_eq!(
            of("review").source,
            SkillSource::Installed(Some("npx skills add".into())),
            "the origin the folder names is the label"
        );
        assert_eq!(of("review").source.label(), "installed from npx skills add");
        // The project's own comes first in the listing, and the built-ins are still there.
        let order: Vec<&str> = r.names();
        assert_eq!(order[0], "release", "a project's skill leads the index");
        assert!(
            order.contains(&"plugin-pane"),
            "the built-ins are still here"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The frontmatter is the document a ranker reads: `when_to_use` and `keywords` reach it, the
    /// body does not, and a skill described as beamer is reachable from a prompt saying "latex
    /// presentation" - the pair no one-line `description` connects.
    #[test]
    fn the_whole_frontmatter_is_the_document_and_the_body_is_not() {
        let root = std::env::temp_dir().join(format!("letibot-skilldoc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("slides");
        std::fs::create_dir_all(&dir).expect("a skill dir");
        std::fs::write(
            dir.join("SKILL.md"),
            "---\nname: slides\ndescription: build a deck for a talk\nwhen_to_use: the person \
             asks for a latex presentation\nkeywords: [beamer, frames]\n---\n\nTHE BODY\n",
        )
        .expect("a skill");
        let r = SkillRegistry::load_onto(
            vec![Skill::new(
                "commit",
                "write a conventional commit",
                "type(scope): summary",
                SkillSource::Operator,
            )],
            std::slice::from_ref(&root),
        );
        let slides = r
            .skills
            .iter()
            .find(|s| s.name == "slides")
            .expect("slides");
        let doc = slides.document();
        for part in ["slides", "deck", "latex presentation", "beamer", "frames"] {
            assert!(doc.contains(part), "`{part}` is not in the document: {doc}");
        }
        assert!(!doc.contains("THE BODY"), "the body is not scored: {doc}");
        assert_eq!(slides.keywords, vec!["beamer", "frames"]);
        // And the ranker reads it: a query about a presentation finds the deck, which its
        // name and one line would never have reached.
        let ranked = r.ranked("make me a latex presentation for the offsite", 3);
        assert_eq!(ranked[0].0, "slides", "{ranked:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The index lists the pinned skills and a count of the rest, never a source in full: a box
    /// with hundreds of installed skills would otherwise spend the prompt's first screen on names
    /// nobody asked to pin. What it does not list is still reachable by `skill list` and `skill find`.
    #[test]
    fn the_index_lists_only_the_pinned_skills_and_counts_the_rest() {
        let vocab = letibot_tokencore::Vocab::bytes([], []);
        let r = SkillRegistry::new(vec![
            Skill::new(
                "commit",
                "write a conventional commit",
                "a body",
                SkillSource::Operator,
            ),
            Skill::new(
                "release",
                "cut a tagged release",
                "a body",
                SkillSource::Project,
            ),
            Skill::new(
                "review",
                "judge a branch",
                "a body",
                SkillSource::Installed(None),
            ),
        ])
        .with_pins(["release"]);
        let index = r.index(&vocab).expect("an index");
        assert!(
            index.contains("- release  [this project]  cut a tagged release"),
            "{index}"
        );
        assert!(index.contains("1 pinned of 3"), "{index}");
        assert!(index.contains("2 more that `skill list` holds"), "{index}");
        assert!(
            !index.contains("- commit") && !index.contains("- review"),
            "an unpinned skill is not named in the prompt: {index}"
        );
        // The verbs are how the rest is reached, so the index says them.
        assert!(
            index
                .contains("`skill` loads a body, `skill list` every one, `skill find` the nearest"),
            "{index}"
        );
        // A box that pins nothing lists no source at all, and still says what it holds.
        let plain = SkillRegistry::new(vec![Skill::new(
            "commit",
            "write a conventional commit",
            "a body",
            SkillSource::Operator,
        )]);
        let index = plain.index(&vocab).expect("an index");
        assert!(index.contains("0 pinned of 1"), "{index}");
        assert!(!index.contains("\n- "), "no row and no source: {index}");
        assert!(index.contains("1 more that `skill list` holds"), "{index}");

        // A pin nothing answers to is NAMED in the prompt and not silently dropped: the operator
        // typed a name, and a pin that did nothing is the one thing they cannot see from outside.
        assert!(
            plain.unknown_pins().is_empty(),
            "a box with no pins has none"
        );
        let ghost = plain.with_pins(["ghost-skill"]);
        assert_eq!(ghost.unknown_pins(), vec!["ghost-skill"]);
        let index = ghost.index(&vocab).expect("an index");
        assert!(
            index.contains("pinned `ghost-skill` names no skill on this box"),
            "{index}"
        );
    }

    /// The index names a pinned skill with its source, and carries no body. The bodies are what
    /// `skill` is for; a prompt that held them would be a corpus, not an index.
    #[test]
    fn the_index_names_each_pinned_skill_with_its_source_and_carries_no_body() {
        let r = registry().with_pins(["commit"]);
        let vocab = letibot_tokencore::Vocab::bytes([], []);
        let index = r.index(&vocab).expect("an index");
        assert!(
            index.contains("- commit  [the operator's]  write a conventional commit"),
            "{index}"
        );
        assert!(
            index.contains("`skill` loads a body, `skill list` every one"),
            "{index}"
        );
        assert!(
            !index.contains("conventional-commits:"),
            "the body is not in the prompt: {index}"
        );
        // And a registry with nothing in it has no index at all rather than an empty heading.
        assert!(SkillRegistry::new(Vec::new()).index(&vocab).is_none());
    }

    /// The cap is a count, and the tail says how many it left out: an operator who pins a hundred
    /// skills must not be handed a prompt that no longer fits.
    #[test]
    fn the_index_stops_at_its_cap_and_says_how_many_more_there_are() {
        let names: Vec<String> = (0..INDEX_MAX_ROWS + 5)
            .map(|i| format!("skill-{i:03}"))
            .collect();
        let many: Vec<Skill> = names
            .iter()
            .map(|name| {
                Skill::new(
                    name.clone(),
                    "a line",
                    "a body",
                    SkillSource::Installed(None),
                )
            })
            .collect();
        let total = many.len();
        let r = SkillRegistry::new(many).with_pins(names);
        // The byte vocabulary prices one token per byte, so the token cap binds first here; the
        // count cap is asserted by the same arithmetic, one line up.
        let vocab = letibot_tokencore::Vocab::bytes([], []);
        let index = r.index(&vocab).expect("an index");
        let shown = index.matches("\n- skill-").count();
        assert!(
            shown > 0 && shown <= INDEX_MAX_ROWS,
            "shown {shown}: {index}"
        );
        let said = index
            .lines()
            .find(|l| l.starts_with("… and "))
            .unwrap_or_else(|| panic!("no tail: {index}"));
        let more: usize = said
            .trim_start_matches("… and ")
            .split_whitespace()
            .next()
            .and_then(|n| n.parse().ok())
            .expect("a count");
        assert_eq!(shown + more, total, "{index}");
    }

    /// A free-text query picks the nearest skill, and a weak one loads nothing: the floor is what
    /// stops a query that only shares the word "the" from spending the model's context on a page
    /// about something else.
    #[test]
    fn a_query_answers_the_nearest_body_and_nothing_below_the_floor() {
        let r = SkillRegistry::new(vec![
            Skill::new(
                "postgres-migrations",
                "change a schema safely on a live database",
                "lock the table, then backfill in batches",
                SkillSource::Operator,
            ),
            Skill::new(
                "plugin-pane",
                "write a pane plugin in a session",
                "scaffold, edit, build, test, load",
                SkillSource::BuiltIn,
            ),
            Skill::new(
                "release",
                "cut a release",
                "tag, push, write the notes",
                SkillSource::Operator,
            ),
        ]);
        let ranked = r.ranked("postgres migration safety", 3);
        assert_eq!(ranked[0].0, "postgres-migrations", "{ranked:?}");
        assert!(ranked[0].1 >= QUERY_FLOOR, "{ranked:?}");
        // A query about nothing here is below the floor, so the tool says so and loads nothing.
        let off = r.ranked("sourdough starter feeding", 3);
        assert!(off.first().is_none_or(|(_, s)| *s < QUERY_FLOOR), "{off:?}");
    }

    /// `allowed-tools` is parsed and it can only narrow: the union over the skills a session was
    /// started with, `None` when none of them declares one.
    #[test]
    fn allowed_tools_is_a_ceiling_and_never_a_permission() {
        let root = std::env::temp_dir().join(format!("letibot-skilltools-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("strict");
        std::fs::create_dir_all(&dir).expect("a skill dir");
        std::fs::write(
            dir.join("SKILL.md"),
            "---\nname: strict\ndescription: read only\nallowed-tools: Read, Grep(src/**)\n---\n\nread it\n",
        )
        .expect("a skill");
        let r = SkillRegistry::load_onto(Vec::new(), std::slice::from_ref(&root));
        let strict = r
            .skills
            .iter()
            .find(|s| s.name == "strict")
            .expect("strict");
        assert_eq!(
            strict.allowed_tools.as_deref(),
            Some(["read".to_string(), "grep".to_string()].as_slice()),
            "the names, lowercased, with the qualifier dropped"
        );
        let ceiling = r.tool_ceiling(&["strict".to_string()]).expect("a ceiling");
        assert!(
            ceiling.contains("read") && !ceiling.contains("bash"),
            "{ceiling:?}"
        );
        // A session started with a skill that declares nothing restricts nothing.
        let plain = SkillRegistry::new(vec![Skill::new(
            "plain",
            "",
            "a body",
            SkillSource::Operator,
        )]);
        assert!(plain.tool_ceiling(&["plain".to_string()]).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn lists_and_loads() {
        let r = registry();
        assert_eq!(r.names(), vec!["commit"]);
        assert!(r.load("commit").unwrap().contains("conventional-commits"));
        assert!(r.load("nope").is_none());
        assert!(r.list(None).contains("commit"), "{}", r.list(None));
        assert!(r.list(None).contains("no shelf"), "{}", r.list(None));
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
                .map(|e| {
                    Skill::new(
                        e.title.clone(),
                        "",
                        format!("body of {}", e.id),
                        SkillSource::Fabric,
                    )
                })
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
        let l = r.list(None);
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

    /// A pin is spelled the way `skill list` prints it. A shelf row is listed by its id, and the
    /// check compared the pin with the title: the operator's own spelling was reported as naming
    /// nothing on this box and was not listed.
    #[test]
    fn a_pin_spelled_the_way_skill_list_prints_it_is_known() {
        let r = registry().with_pins(["sk-42"]);
        r.set_shelf(Arc::new(FakeShelf(Ok(vec![ShelfEntry {
            id: "sk-42".into(),
            title: "Beamer Slides".into(),
            visibility: "public".into(),
        }]))));
        assert!(
            r.list(None).contains("- sk-42  [public]  Beamer Slides"),
            "the listing prints the id first: {}",
            r.list(None)
        );
        assert!(
            r.unknown_pins().is_empty(),
            "the pin names the skill `skill list` shows: {:?}",
            r.unknown_pins()
        );
        let index = r
            .index(&letibot_tokencore::Vocab::bytes([], []))
            .expect("an index");
        assert!(
            index.contains("Beamer Slides"),
            "and the pinned row is listed in the prompt: {index}"
        );
    }

    #[test]
    fn a_shelf_that_cannot_be_read_is_not_an_empty_shelf() {
        let r = registry();
        r.set_shelf(Arc::new(FakeShelf(Err("node unreachable".into()))));
        let l = r.list(None);
        assert!(
            l.contains("could not be read: node unreachable. That is not an empty shelf"),
            "{l}"
        );
    }

    fn write_skill(dir: &Path, name: &str, desc: &str) {
        std::fs::create_dir_all(dir).expect("a skill dir");
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {desc}\n---\n\nbody of {name}\n"),
        )
        .expect("a skill");
    }

    fn temp_root(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("letibot-skill-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    /// The tool answers from the disk as it is now, and the prompt's index stays as composed.
    #[test]
    fn a_skill_added_after_open_is_live_in_the_tool_and_not_in_the_index() {
        let root = temp_root("live");
        let op = root.join("operator");
        write_skill(&op.join("first"), "first", "the first one");
        let dirs = SkillDirs {
            operator: vec![op.clone()],
            ..SkillDirs::default()
        };
        let r = SkillRegistry::load_for(&dirs);
        let vocab = letibot_tokencore::Vocab::bytes([], []);
        let before = r.index(&vocab);

        write_skill(
            &op.join("zebra-grooming"),
            "zebra-grooming",
            "groom a zebra",
        );
        assert!(r.list(None).contains("zebra-grooming"), "{}", r.list(None));
        assert!(
            r.load_any("zebra-grooming")
                .unwrap()
                .body
                .contains("body of")
        );
        let found = r.ranked("groom a zebra", 3);
        assert_eq!(found.first().map(|f| f.0.as_str()), Some("zebra-grooming"));
        assert_eq!(r.index(&vocab), before, "the pinned index is fixed at open");
        assert!(!r.names().contains(&"zebra-grooming"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_synced_skill_two_levels_down_is_installed() {
        let root = temp_root("synced");
        let installed = root.join(".claude/skills");
        write_skill(
            &installed.join("synced/abc-123/deck"),
            "deck",
            "make a deck",
        );
        write_skill(
            &installed.join("synced/abc-123/nested/too/deep"),
            "deep",
            "no",
        );
        let dirs = SkillDirs {
            installed: vec![installed],
            ..SkillDirs::default()
        };
        let r = SkillRegistry::load_for(&dirs);
        let deck = r.skills.iter().find(|s| s.name == "deck").expect("deck");
        assert_eq!(deck.source.label(), "installed");
        assert!(!r.skills.iter().any(|s| s.name == "deep"));
        let _ = std::fs::remove_dir_all(&root);
    }

    fn seat(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|n| n.to_ascii_lowercase()).collect()
    }

    /// `requires` and `unless` are read by the one frontmatter reader, in both spellings an author
    /// writes: one line of names, and a block of them under the key.
    #[test]
    fn the_frontmatter_reads_requires_and_unless_in_both_spellings() {
        let inline = from_text(
            "---\nname: dashboard\ndescription: a status page\nrequires: flowy, bash\n\
             unless: [screen]\n---\n\nbody\n",
            SkillSource::Installed(None),
        )
        .expect("inline lists parse");
        assert_eq!(inline.requires, ["flowy", "bash"]);
        assert_eq!(inline.unless, ["screen"]);

        let block = from_text(
            "---\nname: dashboard\ndescription: a status page\nrequires:\n  - flowy\n  - bash\n\
             unless:\n  - screen\n---\n\nbody\n",
            SkillSource::Installed(None),
        )
        .expect("block lists parse");
        assert_eq!(block.requires, ["flowy", "bash"]);
        assert_eq!(block.unless, ["screen"]);

        // A skill that says neither needs nothing special, which is every skill written before
        // this key existed.
        let plain = from_text(
            "---\nname: plain\ndescription: nothing\n---\n\nbody\n",
            SkillSource::Installed(None),
        )
        .expect("a plain skill");
        assert!(plain.requires.is_empty() && plain.unless.is_empty());
        assert!(plain.unmet(&seat(&["read"])).is_none());
    }

    /// A missing capability hides the skill and says which one; a seat that has it shows the
    /// skill. `mcp__server` and `mcp__server__*` are the same server, checked the way the declared
    /// `mcp__` tools already were.
    #[test]
    fn a_missing_capability_names_itself_and_a_present_one_satisfies_it() {
        let skill = Skill {
            requires: vec!["flowy".into(), "mcp__github".into(), "mcp__jira__*".into()],
            ..Skill::new("dash", "a panel", "body", SkillSource::Operator)
        };
        let bare = seat(&["read", "bash"]);
        let why = skill.unmet(&bare).expect("nothing it needs is here");
        assert!(
            why.contains("requires `flowy`") && why.contains("does not have"),
            "the reason names the first missing capability: {why}"
        );

        let flock = seat(&["read", "flowy"]);
        let why = skill.unmet(&flock).expect("github is still missing");
        assert!(why.contains("`mcp__github`"), "{why}");

        let all = seat(&[
            "read",
            "flowy",
            "mcp__github__create_issue",
            "mcp__jira__search",
        ]);
        assert!(
            skill.unmet(&all).is_none(),
            "one tool of the server is the server, and `mcp__server__*` is the same read"
        );
    }

    /// `unless` is the other half: the capability being PRESENT is what hides the skill, so the
    /// skill that fits a seat without flowy is offered exactly there.
    #[test]
    fn a_capability_in_unless_hides_the_skill_only_while_it_is_present() {
        let skill = Skill {
            unless: vec!["flowy".into()],
            ..Skill::new("status-page", "an html page", "body", SkillSource::Operator)
        };
        assert!(skill.unmet(&seat(&["read"])).is_none(), "the seat it fits");
        let why = skill
            .unmet(&seat(&["read", "flowy"]))
            .expect("flowy is here, so it is not");
        assert!(
            why.contains("hidden while `flowy` is on this seat"),
            "the reason says the capability is PRESENT, not missing: {why}"
        );
    }

    /// A built-in is the build's own words, so the `mcp__`/connector half its author guessed is
    /// skipped - but `requires` is a fact about the SEAT, and every source pays it.
    #[test]
    fn a_built_in_skill_honours_requires_and_unless() {
        let built_in = Skill {
            requires: vec!["flowy".into()],
            ..Skill::new(
                "flowy-pane",
                "a pane fed by the fabric",
                "names no mcp__ tools at all",
                SkillSource::BuiltIn,
            )
        };
        let why = built_in
            .unmet(&seat(&["read"]))
            .expect("a built-in is not exempt from a seat it cannot use");
        assert!(why.contains("`flowy`"), "{why}");
        assert!(built_in.unmet(&seat(&["flowy"])).is_none());

        // And the author's half is still skipped for a built-in: it names a server and nothing
        // is behind it, which is not this build's claim to make.
        let ours = Skill::new(
            "ours",
            "",
            "call mcp__nothing__here to check",
            SkillSource::BuiltIn,
        );
        assert!(ours.unmet(&seat(&["read"])).is_none());
    }

    /// The operator's table is the one in force: it REPLACES the frontmatter whole, so it can
    /// hide a skill the author left open and unhide one they closed.
    #[test]
    fn the_operators_table_replaces_the_frontmatter_for_that_name() {
        let open = Skill::new("dash", "a panel", "body", SkillSource::Operator);
        let gated = Skill {
            requires: vec!["flowy".into()],
            ..Skill::new("page", "a page", "body", SkillSource::Operator)
        };
        let mut when = BTreeMap::new();
        when.insert(
            "dash".to_string(),
            SkillWhen {
                requires: Vec::new(),
                unless: vec!["bash".into()],
            },
        );
        when.insert(
            "page".to_string(),
            SkillWhen {
                requires: vec!["read".into()],
                unless: Vec::new(),
            },
        );
        let r = SkillRegistry::new(vec![open.clone(), gated.clone()]).with_when(when);
        // The operator hid a skill the author left open, on the capability that is here.
        assert!(r.unmet(&open, &seat(&["read"])).is_none());
        assert!(
            r.unmet(&open, &seat(&["read", "bash"]))
                .is_some_and(|w| w.contains("hidden while `bash`")),
            "the table's `unless` is the one asked"
        );
        // And replaced an author's `requires: flowy` with `requires: read`, which is here.
        assert!(r.unmet(&gated, &seat(&["read"])).is_none());
        assert!(
            gated.unmet(&seat(&["read"])).is_some(),
            "the frontmatter alone would have hidden it"
        );
    }

    /// The listing names the reason, so a model choosing between two skills that answer one job
    /// can see which one this seat has, and the registry's `unmet` is the one sentence both the
    /// listing and the load read.
    #[test]
    fn a_hidden_skill_is_listed_with_its_reason_and_not_as_usable() {
        let r = SkillRegistry::new(vec![
            Skill::new("plain", "always fits", "body", SkillSource::Operator),
            Skill {
                requires: vec!["flowy".into()],
                ..Skill::new(
                    "dashboard",
                    "needs the fabric",
                    "body",
                    SkillSource::Operator,
                )
            },
        ]);
        let read_only = seat(&["read"]);
        let listed = r.list(Some(&read_only));
        assert!(listed.contains("1 skill(s) —"), "{listed}");
        assert!(listed.contains("1 skill(s) not for this seat"), "{listed}");
        assert!(
            listed.contains("requires `flowy`, which this seat does not have"),
            "the listing carries the reason a load refuses with: {listed}"
        );
        // No seat known (a runtime with no daemon behind it) filters nothing.
        assert!(r.list(None).contains("2 skill(s) —"), "{}", r.list(None));
    }
}
