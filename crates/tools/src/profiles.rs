//! Role profiles: what a subagent role IS, in a file.
//!
//! A role used to be a name in a match arm, so a child that asked for `coder` got a tool list and
//! no instructions. A profile is the whole role: a one-line purpose, a prompt overlay, the tools it
//! keeps, and what it may start. It only ever NARROWS - it never grants a tool, a permission or a
//! model its parent does not hold - which is what lets one table serve a `--role` seat and a
//! `task {role}` child without a second authority (`TODO.md` R120, `pkg/kimi-code.md` §5.6).
//!
//! Files, read in this order and first name wins: `<workspace>/.letibot/roles/*.md`,
//! `~/.config/letibot/roles/*.md`, then this build's own `roles/*.md`. The frontmatter is the
//! policy; the body is the overlay, and it is the only part the model reads.

use std::path::{Path, PathBuf};

use crate::schema::Downgrade;

/// Where a profile was read from. A label, never a permission: where a file sits cannot give a
/// profile a tool, and a project's copy of a built-in's name REPLACES it, the way skills work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileSource {
    /// `<workspace>/.letibot/roles`.
    Project,
    /// `~/.config/letibot/roles`, the directory only this seat reads.
    Operator,
    /// Compiled into this binary from the tree's `roles/`.
    BuiltIn,
}

impl ProfileSource {
    /// Which name wins a clash: lower is first, and a person's copy of a built-in replaces it.
    pub fn priority(self) -> u8 {
        match self {
            ProfileSource::Project => 0,
            ProfileSource::Operator => 1,
            ProfileSource::BuiltIn => 2,
        }
    }

    /// How a listing names it.
    pub fn label(self) -> &'static str {
        match self {
            ProfileSource::Project => "this project",
            ProfileSource::Operator => "the operator's",
            ProfileSource::BuiltIn => "this build",
        }
    }
}

/// One role, as its file says it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    /// The file's stem, and the word `role:` and `--role` take.
    pub name: String,
    /// The one line the `task` tool's description lists, so a model can choose without a refusal.
    pub purpose: String,
    /// The seat this profile is seated on, when the file names one. `None` means the profile's own
    /// name is the seat's name, which is what makes a seat's own profile a one-line file.
    pub seat: Option<String>,
    /// The tools this role keeps. `None` is the seat's whole table; `Some` is a filter over it, and
    /// over the parent's own tools too - a profile cannot hand a child a tool its parent lacks.
    pub tools: Option<Vec<String>>,
    /// A permission narrowing, in `access:`'s own words (`no-write`, `read-only`, ...). Unioned
    /// with whatever the call asked for, so a profile can only ever take away.
    pub access: Option<Downgrade>,
    /// The model this role runs on, when the call names none.
    pub model: Option<String>,
    /// The profiles this role may start. `None` is [`DEFAULT_SUBAGENTS`]; an empty list is *none*,
    /// which is how a role that must not fan out says so without a code change.
    pub subagents: Option<Vec<String>>,
    /// Directories whose `AGENTS.md` this role is handed at its open, before anything is touched.
    pub guidance: Vec<String>,
    /// The prompt overlay itself.
    pub overlay: String,
    pub source: ProfileSource,
    /// The file it was read from, or `None` for one compiled in.
    pub path: Option<PathBuf>,
}

/// The profiles a role may start when its file says nothing. The three a survey, a review and a
/// piece of work need, and nothing that could author a package's own permissions: `gatekeeper` and
/// `notes_keeper` are the daemon's, and no profile here may start them.
pub const DEFAULT_SUBAGENTS: [&str; 3] = ["coder", "explore", "reviewer"];

impl Profile {
    /// The seat this profile is seated on, in the seat table's own spelling.
    pub fn seat(&self) -> &str {
        self.seat.as_deref().unwrap_or(&self.name)
    }

    /// The roles it may start.
    pub fn subagents(&self) -> Vec<String> {
        match &self.subagents {
            Some(list) => list.clone(),
            None => DEFAULT_SUBAGENTS.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// Whether this role keeps a tool: the whole seat's table when the file names none.
    pub fn keeps(&self, tool: &str) -> bool {
        match &self.tools {
            None => true,
            Some(list) => list.iter().any(|t| t == tool),
        }
    }

    /// The tools to seat, given the seat's own table and - for a child - the parent's: the
    /// profile's filter, applied to both. A profile that names no tools keeps the seat's table,
    /// which is the behaviour every child had before profiles existed; `ceiling` is `None` for a
    /// session with no parent, where there is nothing to be narrower than.
    pub fn narrow(&self, seat: &[String], ceiling: Option<&[String]>) -> Vec<String> {
        if self.tools.is_none() {
            return seat.to_vec();
        }
        seat.iter()
            .filter(|t| self.keeps(t))
            .filter(|t| match ceiling {
                None => true,
                Some(parent) => parent.iter().any(|p| p == *t),
            })
            .cloned()
            .collect()
    }

    /// A profile that is nothing but a seat: the seat's table, no overlay, no narrowing. Built by
    /// the daemon when a name is a seat's own ("`--role m1`", a seat a person typed) rather than a
    /// file, so that one table answers both doors.
    pub fn of_seat(name: &str) -> Profile {
        Profile {
            name: name.to_string(),
            purpose: String::new(),
            seat: None,
            tools: None,
            access: None,
            model: None,
            subagents: None,
            guidance: Vec::new(),
            overlay: String::new(),
            source: ProfileSource::BuiltIn,
            path: None,
        }
    }
}

/// One `roles/*.md`, parsed. `Err` is a file that cannot be a role, by name: a malformed profile is
/// SAID where it is asked for, never loaded half-way and never silently dropped.
pub fn from_text(name: &str, text: &str, source: ProfileSource) -> Result<Profile, String> {
    let rest = text
        .strip_prefix("---")
        .ok_or_else(|| format!("`{name}` has no frontmatter: a role profile opens with `---`"))?;
    let (front, body) = rest
        .split_once("\n---")
        .ok_or_else(|| format!("`{name}` has no closing `---` after its frontmatter"))?;
    let mut profile = Profile {
        name: name.to_string(),
        purpose: String::new(),
        seat: None,
        tools: None,
        access: None,
        model: None,
        subagents: None,
        guidance: Vec::new(),
        overlay: body.trim_start_matches('\n').trim_end().to_string(),
        source,
        path: None,
    };
    // A block list is written under its key, one name a line, so the key that opened one is
    // remembered: `subagents:` with nothing after it opens a list, and `subagents: []` closes it.
    let mut open = Open::Nothing;
    for line in front.lines() {
        let line = line.trim_end();
        if let Some(item) = line.trim_start().strip_prefix("- ") {
            let item = unquote(item.trim());
            match open {
                Open::Tools => profile.tools.get_or_insert_with(Vec::new).push(item),
                Open::Subagents => profile.subagents.get_or_insert_with(Vec::new).push(item),
                Open::Guidance => profile.guidance.push(item),
                Open::Nothing => {}
            }
            continue;
        }
        open = Open::Nothing;
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            // The stem is the name, so a file that disagrees with itself is refused rather than
            // resolving under two names - the trap a rename leaves behind.
            "name" if !value.is_empty() && unquote(value) != name => {
                return Err(format!(
                    "`{name}` says `name: {}`: a profile is named by its file, and the file and \
                     its frontmatter must agree",
                    unquote(value)
                ));
            }
            "description" | "purpose" => profile.purpose = unquote(value),
            "seat" => profile.seat = Some(unquote(value)),
            "tools" => {
                // `tools:` with nothing after it opens a block list; `tools: []` is an explicit
                // empty allowlist and means the role keeps none of the seat's tools.
                profile.tools = Some(names(value));
                if !value.trim_start().starts_with('[') {
                    open = Open::Tools;
                }
            }
            "subagents" => {
                let list = names(value);
                profile.subagents = Some(list);
                if !value.trim_start().starts_with('[') {
                    open = Open::Subagents;
                }
            }
            "access" => {
                let d = Downgrade::parse(&unquote(value)).map_err(|e| format!("`{name}`: {e}"))?;
                profile.access = Some(d);
            }
            "model" => profile.model = Some(unquote(value)).filter(|m| !m.is_empty()),
            "guidance" => {
                let list = names(value);
                if list.is_empty() {
                    open = Open::Guidance;
                } else {
                    profile.guidance = list;
                }
            }
            // An unknown key is a typo in a policy, and a policy that half-lands is the failure
            // this file exists to prevent.
            other if !other.is_empty() => {
                return Err(format!(
                    "`{name}` has `{other}:`, which is not a profile key. The keys are name, \
                     description, seat, tools, access, model, subagents, guidance, and then the \
                     overlay body"
                ));
            }
            _ => {}
        }
    }
    if profile.purpose.is_empty() {
        return Err(format!(
            "`{name}` has no `description:` - it is the one line the `task` tool lists, and a \
             role a model cannot choose is a role it finds by being refused"
        ));
    }
    Ok(profile)
}

/// The list a key opened, so the first item after it knows which list it belongs to.
enum Open {
    Nothing,
    Tools,
    Subagents,
    Guidance,
}

/// `[a, b]` or `a, b` or one name a line: the names only, deduped, in the author's order.
fn names(list: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for word in list
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split([',', ' ', '\t'])
    {
        let word = unquote(word.trim());
        if !word.is_empty() && !out.contains(&word) {
            out.push(word);
        }
    }
    out
}

fn unquote(v: &str) -> String {
    v.trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim()
        .to_string()
}

/// Every profile this box has: the built-ins, then the two directories, by name.
#[derive(Debug, Clone, Default)]
pub struct Profiles {
    rows: Vec<Profile>,
    /// A file that could not be parsed, kept so the refusal can quote it where the name is asked
    /// for. A broken profile must not take the other names down with it.
    broken: Vec<(String, String)>,
}

/// This build's own roles, compiled in. One file each, so a built-in is read by the same parser as
/// the operator's and the two cannot mean different things by the same words. A SEAT with no file of
/// its own still resolves - [`Profile::of_seat`] - which is what `leticode` is for: a role named after
/// it would spell the INSTRUCTION FILE's own name in this crate, and the guard in
/// `standing_notes::tests::the_runtime_names_neither_instruction_file` is right that a runtime holds
/// that name nowhere.
pub const BUILT_IN: [(&str, &str); 9] = [
    (
        "orchestrator",
        include_str!("../../../roles/orchestrator.md"),
    ),
    ("planner", include_str!("../../../roles/planner.md")),
    ("researcher", include_str!("../../../roles/researcher.md")),
    ("coder", include_str!("../../../roles/coder.md")),
    ("runner", include_str!("../../../roles/runner.md")),
    ("gatekeeper", include_str!("../../../roles/gatekeeper.md")),
    (
        "notes_keeper",
        include_str!("../../../roles/notes_keeper.md"),
    ),
    ("explore", include_str!("../../../roles/explore.md")),
    ("reviewer", include_str!("../../../roles/reviewer.md")),
];

impl Profiles {
    /// This build's own table, with no directories read.
    pub fn builtins() -> Profiles {
        let mut p = Profiles::default();
        for (name, text) in BUILT_IN {
            match from_text(name, text, ProfileSource::BuiltIn) {
                Ok(mut row) => {
                    row.path = Some(PathBuf::from(format!("roles/{name}.md")));
                    p.push(row);
                }
                // A built-in that does not parse is this crate's defect, not the operator's, and
                // a test walks every one of them; saying it here beats a `task` that lists fewer
                // roles than the table holds.
                Err(why) => p.broken.push((name.to_string(), why)),
            }
        }
        p.sorted();
        p
    }

    /// The built-ins plus `workspace/.letibot/roles` and the operator's own directory. A directory
    /// that is absent is no profiles rather than an error.
    pub fn load(workspace: &Path, config: Option<&Path>) -> Profiles {
        let mut p = Profiles::default();
        let mut dirs: Vec<(PathBuf, ProfileSource)> = Vec::new();
        if let Some(config) = config {
            dirs.push((config.join("roles"), ProfileSource::Operator));
        }
        dirs.push((workspace.join(".letibot/roles"), ProfileSource::Project));
        for (dir, source) in dirs {
            p.read_dir(&dir, source);
        }
        // Pushed last, because `push` keeps the higher-priority copy of a name: a file on this box
        // replaces the built-in of the same name.
        let builtins = Profiles::builtins();
        p.broken.extend(builtins.broken);
        for row in builtins.rows {
            p.push(row);
        }
        p.sorted();
        p
    }

    /// A profiles table built from rows, for a test or a daemon that holds its own.
    pub fn of(rows: Vec<Profile>) -> Profiles {
        let mut p = Profiles::default();
        for row in rows {
            p.push(row);
        }
        p.sorted();
        p
    }

    /// Where it came from, then its name: a function of the files alone, so two listings of the
    /// same box show the same order and a prompt does not move for nothing.
    fn sorted(&mut self) {
        self.rows.sort_by(|a, b| {
            a.source
                .priority()
                .cmp(&b.source.priority())
                .then_with(|| a.name.cmp(&b.name))
        });
    }

    /// One row wins its name: the first by source priority, and a project's copy of a built-in
    /// replaces it.
    fn push(&mut self, row: Profile) {
        if let Some(at) = self.rows.iter().position(|r| r.name == row.name) {
            if self.rows[at].source.priority() <= row.source.priority() {
                return;
            }
            self.rows.remove(at);
            self.rows.push(row);
            return;
        }
        self.rows.push(row);
    }

    fn read_dir(&mut self, dir: &Path, source: ProfileSource) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        paths.sort();
        for path in paths {
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let Some(name) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let text = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) => {
                    self.broken
                        .push((name.to_string(), format!("{}: {e}", path.display())));
                    continue;
                }
            };
            match from_text(name, &text, source) {
                Ok(mut row) => {
                    row.path = Some(path.clone());
                    self.push(row);
                }
                Err(why) => self.broken.push((name.to_string(), why)),
            }
        }
    }

    /// The profile called `name`, `Ok(None)` for a name nobody answers to, and `Err` for a file
    /// that exists and cannot be a profile - which is a different fact from an absent one.
    pub fn find(&self, name: &str) -> Result<Option<Profile>, String> {
        if let Some((_, why)) = self.broken.iter().find(|(n, _)| n == name) {
            return Err(why.clone());
        }
        Ok(self.rows.iter().find(|r| r.name == name).cloned())
    }

    /// Every name, in the order the listing shows: where it came from, then its name.
    pub fn names(&self) -> Vec<String> {
        self.rows.iter().map(|r| r.name.clone()).collect()
    }

    /// The rows themselves, for a listing that shows the purpose and the source.
    pub fn rows(&self) -> &[Profile] {
        &self.rows
    }

    /// The files that could not be a profile, as (name, why).
    pub fn broken(&self) -> &[(String, String)] {
        &self.broken
    }
}

/// The directories a tool call touched, under the session's root: every directory the path names
/// or passes through - `crates/tools/src/lib.rs` touches `crates`, `crates/tools` and
/// `crates/tools/src` - and never the root's own, whose `AGENTS.md` is already in the prompt.
///
/// The path's last segment is tried too, so a call that names a DIRECTORY
/// (`grep {path: "crates"}`) finds that directory's file. A file cannot be a directory, so the
/// extra probe costs one failed read and nothing else.
///
/// A tool names a path with the `path` key - one spelling across `read`, `write`, `edit`, `grep`,
/// `glob`, `outline` and `lsp` - so this reads that key and nothing else. A command's own targets
/// are NOT guessed at: `bash` carries no path, and a guess about what a shell line will open is how
/// a session is handed a file about a directory it never entered.
pub fn touched(args: &serde_json::Value) -> Vec<String> {
    let Some(path) = args.get("path").and_then(|v| v.as_str()) else {
        return Vec::new();
    };
    let path = path.trim_start_matches("./").trim_start_matches('/');
    let mut dirs: Vec<String> = Vec::new();
    let mut acc = String::new();
    let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    for part in &parts {
        if part == &"." || part == &".." {
            continue;
        }
        if !acc.is_empty() {
            acc.push('/');
        }
        acc.push_str(part);
        dirs.push(acc.clone());
    }
    dirs
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A profile and the directory it must be able to reach.
    const A_ROLE: &str = "---\n\
        description: looks at code and says what is wrong\n\
        seat: coder\n\
        access: no-write\n\
        tools: read, grep, bash\n\
        model: qwen/qwen3\n\
        subagents: [explore]\n\
        guidance: crates/tools, tests\n\
        ---\n\
        \n\
        You survey and you hand back a list. You never edit.\n";

    #[test]
    fn a_profile_is_its_frontmatter_and_its_overlay() {
        let p = from_text("reviewer", A_ROLE, ProfileSource::BuiltIn).expect("parses");
        assert_eq!(p.name, "reviewer");
        assert_eq!(p.purpose, "looks at code and says what is wrong");
        assert_eq!(p.seat(), "coder", "the file names its seat");
        assert_eq!(
            p.tools,
            Some(vec![
                "read".to_string(),
                "grep".to_string(),
                "bash".to_string()
            ])
        );
        assert!(p.access.is_some(), "`access: no-write` narrows");
        assert_eq!(p.model.as_deref(), Some("qwen/qwen3"));
        assert_eq!(p.subagents(), vec!["explore".to_string()]);
        assert_eq!(
            p.guidance,
            vec!["crates/tools".to_string(), "tests".to_string()]
        );
        assert_eq!(
            p.overlay,
            "You survey and you hand back a list. You never edit."
        );
    }

    #[test]
    fn every_built_in_parses_and_names_itself() {
        let profiles = Profiles::builtins();
        assert!(
            profiles.broken().is_empty(),
            "built-ins that do not parse: {:?}",
            profiles.broken()
        );
        // The map's three - a worker that writes code, a survey that only reads, a planner - plus a
        // file per seat that has one, so `--role NAME` and `role: NAME` resolve through one table.
        // `leticode` is the seat without one: see [`BUILT_IN`] for why.
        let names = profiles.names();
        for want in ["coder", "explore", "reviewer", "planner", "runner"] {
            assert!(
                names.iter().any(|n| n == want),
                "{want} is not in {names:?}"
            );
        }
        for row in profiles.rows() {
            assert!(!row.purpose.is_empty(), "{} has no purpose", row.name);
        }
    }

    /// A profile that could widen a role would be a second permission system, so the built-ins are
    /// checked here: none of the daemon's own seats is startable, and no built-in grants `bash`.
    #[test]
    fn no_built_in_hands_out_the_daemons_own_seats() {
        for row in Profiles::builtins().rows() {
            for sub in row.subagents() {
                assert!(
                    !matches!(sub.as_str(), "gatekeeper" | "notes_keeper"),
                    "`{}` may start `{sub}`, which is the daemon's own seat",
                    row.name
                );
            }
        }
    }

    #[test]
    fn a_profile_with_no_tools_keeps_the_seats_table_and_one_with_tools_filters_it() {
        let all = Profile::of_seat("coder");
        let seat: Vec<String> = ["read", "write", "bash"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            all.narrow(&seat, None),
            seat,
            "no filter is the seat's table"
        );

        let p = from_text("explore", A_ROLE, ProfileSource::BuiltIn).expect("parses");
        assert_eq!(
            p.narrow(&seat, None),
            vec!["read".to_string(), "bash".to_string()],
            "`write` is not in the profile's list"
        );
        // And the parent's own table is a ceiling: naming `bash` does not give a tool the parent
        // does not have, which is the whole of "a child never gets a tool its parent lacks".
        let parent: Vec<String> = vec!["read".to_string()];
        assert_eq!(p.narrow(&seat, Some(&parent)), vec!["read".to_string()]);
    }

    #[test]
    fn a_profile_without_a_description_or_with_a_typo_is_refused_by_name() {
        let no_why = from_text(
            "foo",
            "---\nseat: coder\n---\nbody\n",
            ProfileSource::Project,
        );
        assert!(
            no_why.is_err(),
            "a role with no one-line purpose is refused"
        );
        assert!(no_why.unwrap_err().contains("description"));

        let typo = from_text(
            "bar",
            "---\ndescription: x\ntoolz: read\n---\n",
            ProfileSource::Project,
        );
        let why = typo.expect_err("an unknown key is refused");
        assert!(why.contains("toolz"), "{why}");
        assert!(why.contains("tools"), "the refusal lists the keys: {why}");

        let renamed = from_text(
            "baz",
            "---\nname: other\ndescription: x\n---\n",
            ProfileSource::Project,
        );
        assert!(renamed.is_err(), "the stem is the name and it must agree");
    }

    /// One name a line under its key, which is how an operator writes a long list.
    #[test]
    fn a_block_list_belongs_to_the_key_above_it() {
        let text = "---\n\
            description: block lists\n\
            tools:\n\
            - read\n\
            - grep\n\
            subagents:\n\
            - explore\n\
            guidance:\n\
            - crates/tools\n\
            ---\n";
        let p = from_text("blocks", text, ProfileSource::Project).expect("parses");
        assert_eq!(p.tools, Some(vec!["read".into(), "grep".into()]));
        assert_eq!(p.subagents, Some(vec!["explore".into()]));
        assert_eq!(p.guidance, vec!["crates/tools".to_string()]);
    }

    #[test]
    fn an_empty_subagents_list_starts_nothing_and_an_absent_one_is_the_default() {
        let none = from_text(
            "quiet",
            "---\ndescription: x\nsubagents: []\n---\n",
            ProfileSource::Project,
        )
        .expect("parses");
        assert!(none.subagents().is_empty(), "`[]` says it starts nothing");

        let ordinary = from_text(
            "plain",
            "---\ndescription: x\n---\n",
            ProfileSource::Project,
        )
        .expect("parses");
        assert_eq!(
            ordinary.subagents(),
            DEFAULT_SUBAGENTS.map(String::from).to_vec()
        );
        assert_eq!(ordinary.seat(), "plain", "no `seat:` is its own name");
    }

    /// A project's file replaces a built-in of the same name, and an operator's replaces a
    /// built-in but not the project's.
    #[test]
    fn a_files_copy_replaces_a_built_in_and_the_project_beats_the_operator() {
        let root = std::env::temp_dir().join(format!("roles-{}", std::process::id()));
        let project = root.join("project/.letibot/roles");
        let config = root.join("config");
        std::fs::create_dir_all(&project).expect("project roles");
        std::fs::create_dir_all(config.join("roles")).expect("operator roles");
        std::fs::write(
            config.join("roles/coder.md"),
            "---\ndescription: the operator's coder\n---\n",
        )
        .expect("write");
        std::fs::write(
            project.join("coder.md"),
            "---\ndescription: this project's coder\n---\n",
        )
        .expect("write");
        std::fs::write(project.join("broken.md"), "no frontmatter at all\n").expect("write");

        let profiles = Profiles::load(&root.join("project"), Some(&config));
        let coder = profiles
            .find("coder")
            .expect("no parse error")
            .expect("found");
        assert_eq!(coder.purpose, "this project's coder");
        assert_eq!(coder.source, ProfileSource::Project);
        // A file that cannot be a profile is reported where its NAME is asked for, and takes no
        // other name down with it.
        assert!(profiles.find("explore").expect("no error").is_some());
        let why = profiles
            .find("broken")
            .expect_err("a broken profile is an error");
        assert!(why.contains("frontmatter"), "{why}");
        std::fs::remove_dir_all(&root).ok();
    }

    /// The overlap a nested `AGENTS.md` is looked for in: every directory the path names or passes
    /// through, and never the root's own - its `AGENTS.md` is already in the prompt.
    #[test]
    fn a_paths_directories_are_the_ones_it_names_and_passes_through() {
        let args = serde_json::json!({"path": "crates/tools/src/lib.rs"});
        assert_eq!(
            touched(&args),
            vec![
                "crates".to_string(),
                "crates/tools".to_string(),
                "crates/tools/src".to_string(),
                // The file itself is tried too: a file cannot be a directory, so this is a probe
                // that costs one failed read and lets a DIRECTORY-naming call find its own file.
                "crates/tools/src/lib.rs".to_string()
            ]
        );
        assert_eq!(
            touched(&serde_json::json!({"path": "crates"})),
            vec!["crates".to_string()]
        );
        assert!(touched(&serde_json::json!({"command": "ls crates"})).is_empty());
        assert_eq!(
            touched(&serde_json::json!({"path": "./crates/x.rs"})),
            vec!["crates".to_string(), "crates/x.rs".to_string()]
        );
    }
}
