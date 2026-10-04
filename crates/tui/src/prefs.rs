//! **The head's own preferences**, on disk: `~/.config/letibot/head.toml`.
//!
//! What a head chooses about how it draws — the diff shape, the folds, raw
//! tool calls — used to live in the process and die with it, and was moved by
//! slash commands nobody remembered. The operator's ask (2026-09-16): *"i'd
//! prefer a config option and a pane with runtime-able configurations
//! editable"*. So the choices are a file beside `modes.tsv`, `permission.json`
//! and `providers.toml`, read at start, and written whenever the config pane
//! changes one.
//!
//! The format is the same flat `key = "value"` subset `providers.toml` already
//! uses, parsed by hand for the same reason: four keys do not earn a
//! dependency, and a file a person edits with `vi` must survive a comment and
//! a key this build does not know. Unknown lines are kept on write.

use std::path::{Path, PathBuf};

/// **How many retired notes one head remembers.**
///
/// A cap on *this reader's memory*, not on the log: the notes themselves are in
/// the session log whatever is here, and `/notes` lists every one the head still
/// holds. What falls off the end is a dismissal, so a note that fell off would
/// come back on the next snapshot — which is why the number is far above what any
/// session produces (R10's wall was 2 notes, and the head holds 64 in memory at
/// a time) rather than tuned to it.
pub const RETIRED_CAP: usize = 512;

/// Everything the head persists. Each field is a runtime-editable row in the
/// config pane; adding one here is adding a row there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadPrefs {
    /// `split` — two panels — or `unified` always. The toggle is the whole
    /// choice; the width is the renderer's business.
    pub diff: DiffPref,
    /// **A new project's starter todos** — leticl's `todo_template`, copied. `false` is off (the
    /// default); `true` reads the default template file beside this file
    /// (`todo-template.md`); a path string names another file. See `app`'s `seed_todos` — the
    /// items of that TODO.md are copied onto the operator's half of the board once per project.
    pub todo_template: TodoTemplate,
    /// **The projects that have had their starter todos**, hashed workspace paths — leticl's
    /// `todo_seed` table, held here as the same comma-list shape `retired` uses because this head
    /// has no database of its own. A record and not an *is the list empty* test: a starter row the
    /// operator deletes must not come back, and a project that re-seeds is the duplicate defect
    /// leticl extracted this feature from.
    pub todo_seed: Vec<String>,
    /// **The git field's template** — leticl's `git_format`, copied: `None` is the shipped
    /// default (`app::gitfield::GIT_FORMAT_DEFAULT`), `Some(t)` is the operator's own. A QUOTED
    /// template in the file; `false` for the default; `true` is refused by name, because it is
    /// a switch's word in a template's place and reading it as anything would be a guess at
    /// what somebody meant.
    pub git_format: Option<String>,
    /// `open` or `folded`.
    pub thinking: String,
    /// `open` or `folded`.
    pub tools: String,
    /// Show the model's `<function=…>` markup under each call.
    pub raw_calls: bool,
    /// **Which rung of the ladder this head draws at** (R37/R38) — `conversation`, `terse`,
    /// `normal` or `loud`.
    ///
    /// The operator: *"make versbosity a config option so it persists headrestarts."* It was
    /// the one setting the card could change and the file did not keep, so a reader who chose
    /// `conversation` got `normal` back on every restart — a setting that forgets is a setting
    /// the reader has to keep re-making.
    ///
    /// A `String` rather than the ladder's own enum, because this module is deliberately free
    /// of the app's vocabulary: it is a reader and writer of four words, and which words those
    /// are is [`crate::app::VERBOSITY_VALUES`]'s to say. A name this build does not know is
    /// kept in the file and reported, exactly as an unknown key is.
    pub verbosity: String,
    /// **The notes this reader has retired**, by key (R10).
    ///
    /// Written as one comma-separated value because a key is built to contain no
    /// comma and no whitespace — see `app::note_key`, which hashes the detail
    /// rather than quoting it, so a warning whose text runs to paragraphs does not
    /// have to be escaped into this file. Order is oldest first, and the list is
    /// truncated to [`RETIRED_CAP`] on the way in and on the way out.
    pub retired: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffPref {
    Split,
    Unified,
}

/// `todo_template`'s three shapes, as one type: off, the default file beside this one, or a path
/// the operator named. An enum and not a string so `true` cannot be mistaken for a path called
/// "true" — leticl's ruling, in its own reader: *"`todo_template = "typo.md"` must not silently
/// mean `on, using the default`"*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TodoTemplate {
    Off,
    /// The default: `todo-template.md` beside `head.toml`.
    Default,
    Path(String),
}

impl TodoTemplate {
    /// The words the file holds. A path is written quoted like every other value; `true` and
    /// `false` are written bare because they are the words a person types for a switch.
    pub fn as_str(&self) -> String {
        match self {
            TodoTemplate::Off => "false".into(),
            TodoTemplate::Default => "true".into(),
            TodoTemplate::Path(p) => p.clone(),
        }
    }

    pub fn parse(v: &str) -> Option<TodoTemplate> {
        match v.trim() {
            "false" | "no" | "off" => Some(TodoTemplate::Off),
            "true" | "yes" | "on" => Some(TodoTemplate::Default),
            // An empty value is neither off nor a file: it is a line that says nothing, and the
            // reader reports it rather than guessing which side of the switch nothing means.
            "" => None,
            other => Some(TodoTemplate::Path(other.to_string())),
        }
    }
}

impl DiffPref {
    pub fn as_str(self) -> &'static str {
        match self {
            DiffPref::Split => "split",
            DiffPref::Unified => "unified",
        }
    }
    pub fn parse(s: &str) -> Option<DiffPref> {
        match s.trim() {
            "split" | "side-by-side" | "auto" => Some(DiffPref::Split),
            "unified" | "single" => Some(DiffPref::Unified),
            _ => None,
        }
    }
    pub fn flip(self) -> DiffPref {
        match self {
            DiffPref::Split => DiffPref::Unified,
            DiffPref::Unified => DiffPref::Split,
        }
    }
}

impl Default for HeadPrefs {
    fn default() -> Self {
        HeadPrefs {
            diff: DiffPref::Split,
            thinking: "folded".into(),
            tools: "folded".into(),
            raw_calls: false,
            // The rung the head has always started at when nothing said otherwise.
            verbosity: "normal".into(),
            retired: Vec::new(),
            todo_template: TodoTemplate::Off,
            todo_seed: Vec::new(),
            git_format: None,
        }
    }
}

/// `$XDG_CONFIG_HOME/letibot/head.toml`, or `~/.config/letibot/head.toml`.
/// `None` when neither variable is set, which is a head with nowhere to write
/// and says so rather than writing into the working directory.
pub fn path() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .map(|d| d.join("letibot").join("head.toml"))
}

/// What one line of the file says.
enum Line {
    Pair(String, String),
    Other(String),
}

fn parse(text: &str) -> Vec<Line> {
    text.lines()
        .map(|raw| {
            let l = raw.trim();
            if l.is_empty() || l.starts_with('#') || l.starts_with('[') {
                return Line::Other(raw.to_string());
            }
            match l.split_once('=') {
                Some((k, v)) => Line::Pair(
                    k.trim().to_string(),
                    v.trim().trim_matches('"').trim_matches('\'').to_string(),
                ),
                None => Line::Other(raw.to_string()),
            }
        })
        .collect()
}

/// **One file, every head on the box — so a save is a UNION and never a replacement.**
///
/// Measured 2026-09-22, which is the whole reason this exists: two `letibot-tui`
/// processes were running (pids 2076943 and 2350959, 1d15h and 8h45m old) against the
/// one `~/.config/letibot/head.toml` that `path()` names. Each loads `retired` **once,
/// at startup**, and each save wrote its own list back whole. So head A dismisses a
/// note, head B saves for its own reasons, and A's key is gone from the file — leaving
/// the two disagreeing, and the notes A had dismissed coming back the next time anything
/// read the file. *"i dismissed letibot notes but they stay."*
///
/// The union is the correct write for a shared record: a dismissal is an assertion that
/// a key is retired, and no other head's save is evidence to the contrary. `restore`
/// still empties the list, because it empties its own and saves, and a file with no keys
/// unions to nothing.
pub fn merge_retired(path: &Path, ours: &[String]) -> Vec<String> {
    let (existing, _) = load(path);
    let mut out = existing.retired;
    for k in ours {
        if !out.contains(k) {
            out.push(k.clone());
        }
    }
    let over = out.len().saturating_sub(RETIRED_CAP);
    if over > 0 {
        out.drain(..over);
    }
    out
}

/// The seeded-projects record, unioned with whatever the file already holds — `merge_retired`'s
/// twin, for the same reason: two heads share this file, each loads once, and a replacement write
/// from either drops the other's projects. A dropped record is a project that re-seeds, which is
/// the duplicate defect this feature exists to avoid, so the union is not a courtesy.
pub fn merge_todo_seed(path: &Path, ours: &[String]) -> Vec<String> {
    let (existing, _) = load(path);
    let mut out = existing.todo_seed;
    for k in ours {
        if !out.contains(k) {
            out.push(k.clone());
        }
    }
    out
}

/// Read the file. A missing file is the defaults; a line this build does not
/// understand is reported by name and otherwise ignored, never a refusal to
/// start the head.
pub fn load(path: &Path) -> (HeadPrefs, Vec<String>) {
    let mut p = HeadPrefs::default();
    let mut notes = Vec::new();
    let Ok(text) = std::fs::read_to_string(path) else {
        return (p, notes);
    };
    for line in parse(&text) {
        let Line::Pair(k, v) = line else { continue };
        match k.as_str() {
            "diff" => match DiffPref::parse(&v) {
                Some(d) => p.diff = d,
                None => notes.push(format!("head.toml: diff = {v:?} is not split or unified")),
            },
            "thinking" | "tools" => match v.as_str() {
                "open" | "folded" => {
                    if k == "thinking" {
                        p.thinking = v;
                    } else {
                        p.tools = v;
                    }
                }
                _ => notes.push(format!("head.toml: {k} = {v:?} is not open or folded")),
            },
            "raw_calls" => match v.as_str() {
                "true" | "yes" | "on" => p.raw_calls = true,
                "false" | "no" | "off" => p.raw_calls = false,
                _ => notes.push(format!("head.toml: raw_calls = {v:?} is not true or false")),
            },
            // **A rung by NAME, listed from the ladder itself.** The names live in
            // `app::VERBOSITY_VALUES` and are not repeated here: a second list is a second
            // answer to *what is a rung*, and this file's whole comment is about not keeping
            // one. An unknown word is reported by name and LEFT IN THE FILE — the reader gets
            // the default rung and a sentence saying which word this build could not read.
            "verbosity" => {
                if crate::app::VERBOSITY_VALUES.iter().any(|(n, _)| *n == v) {
                    p.verbosity = v;
                } else {
                    notes.push(format!(
                        "head.toml: verbosity = {v:?} is not one of {}",
                        crate::app::VERBOSITY_VALUES
                            .iter()
                            .map(|(n, _)| *n)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
            }
            // **The starter-todo switch, in its three shapes** — see `TodoTemplate`. A value this
            // build cannot read is reported and left in the file, exactly as `verbosity`'s is.
            "todo_template" => match TodoTemplate::parse(&v) {
                Some(t) => p.todo_template = t,
                None => notes.push(
                    "head.toml: todo_template = \"\" is true, false, or a quoted path".to_string(),
                ),
            },
            // The once-per-project record. Same comma-list shape as `retired`, same cap, and the
            // cap is the same trade: a key that falls off after `RETIRED_CAP` projects would
            // re-seed that project — 512 projects in, on a box whose operator runs a handful at
            // a time, is a corner rather than a plan.
            "todo_seed" => {
                p.todo_seed = v
                    .split(',')
                    .map(str::trim)
                    .filter(|k| !k.is_empty())
                    .map(str::to_string)
                    .collect();
            }
            // **The git field's template: a quoted template, or `false` for the default.** leticl's
            // own refusal is copied with it — `true` is a switch's word in a template's place, and
            // a reader that took it for `the default, please` would be guessing at a sentence
            // somebody may have mistyped. An empty value is the default too (a blank line is no
            // template), and is not reported — it is indistinguishable from the key's absence.
            "git_format" => match v.trim() {
                "" | "false" | "no" | "off" => p.git_format = None,
                "true" | "yes" | "on" => notes.push(
                    "head.toml: git_format = true is a QUOTED template, or false for the default"
                        .into(),
                ),
                other => p.git_format = Some(other.to_string()),
            },
            // R10. An empty value is a real value — "nothing is retired" — and not
            // a key this build does not know, so it is not reported as one.
            "retired" => {
                p.retired = v
                    .split(',')
                    .map(str::trim)
                    .filter(|k| !k.is_empty())
                    .map(str::to_string)
                    .collect();
                let over = p.retired.len().saturating_sub(RETIRED_CAP);
                if over > 0 {
                    p.retired.drain(..over);
                }
            }
            other => notes.push(format!("head.toml: `{other}` is not a key this head knows")),
        }
    }
    (p, notes)
}

/// Write the file, keeping every line that is not one of ours — a comment, a
/// key from a newer build — where it was. Creates the directory.
pub fn save(path: &Path, p: &HeadPrefs) -> Result<(), String> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let ours: [(&str, String); 9] = [
        ("diff", format!("\"{}\"", p.diff.as_str())),
        ("thinking", format!("\"{}\"", p.thinking)),
        ("tools", format!("\"{}\"", p.tools)),
        ("raw_calls", p.raw_calls.to_string()),
        ("verbosity", format!("\"{}\"", p.verbosity)),
        // Quoted like the rest, and never multi-line: no key contains a comma or a
        // space, which is what keeps a hand-edited file honest.
        ("retired", format!("\"{}\"", p.retired.join(","))),
        // The switch's own words — a path is quoted, `true`/`false` are bare — and the record,
        // which is hashed keys for the same reason `retired`'s are.
        (
            "todo_template",
            match &p.todo_template {
                TodoTemplate::Path(s) => format!("\"{}\"", s),
                other => other.as_str(),
            },
        ),
        ("todo_seed", format!("\"{}\"", p.todo_seed.join(","))),
        // The template itself quoted, or `false` bare — the two words the file may hold.
        (
            "git_format",
            match &p.git_format {
                Some(t) => format!("\"{}\"", t),
                None => "false".into(),
            },
        ),
    ];
    let mut written: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for line in parse(&existing) {
        match line {
            Line::Pair(k, _) if ours.iter().any(|(n, _)| *n == k) => {
                let (n, v) = ours.iter().find(|(n, _)| *n == k).unwrap();
                out.push(format!("{n} = {v}"));
                written.insert(n);
            }
            Line::Pair(k, v) => out.push(format!("{k} = \"{v}\"")),
            Line::Other(raw) => out.push(raw),
        }
    }
    if out.is_empty() {
        out.push("# letibot head preferences — edited by the /config pane, or by hand".into());
    }
    for (n, v) in &ours {
        if !written.contains(n) {
            out.push(format!("{n} = {v}"));
        }
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let mut body = out.join("\n");
    body.push('\n');
    // **Through a temporary file, then one rename.** `fs::write` truncates and then
    // writes, so a second process reading at the wrong moment sees a PARTIAL list —
    // and a head that loaded a partial `retired` would then save the partial one back,
    // which is how a dismissal gets lost with nothing to point at. A rename is atomic
    // on any filesystem this runs on, so a reader sees either the old file or the new
    // one and never a half of either.
    let tmp = match path.file_name().and_then(|n| n.to_str()) {
        Some(n) => path.with_file_name(format!(".{n}.tmp")),
        None => {
            return Err(format!(
                "{}: has no file name to write beside",
                path.display()
            ));
        }
    };
    std::fs::write(&tmp, body).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("letibot-prefs-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d.join("head.toml")
    }

    /// **A save leaves no half-written file behind, and no debris beside it.**
    ///
    /// `fs::write` truncates then writes, so a second head reading at the wrong moment
    /// sees a partial `retired` list — and a head that loaded a partial one would save the
    /// partial one back, which is how a dismissal gets lost with nothing to point at. The
    /// write goes through a temporary file and one rename; this asserts the rename
    /// happened and the temporary is gone.
    #[test]
    fn a_save_replaces_the_file_whole_and_leaves_nothing_beside_it() {
        let p = tmp("atomic");
        let many: Vec<String> = (0..40)
            .map(|i| format!("w|code{i}|17900000000{i:02}|deadbeef{i:08}"))
            .collect();
        save(
            &p,
            &HeadPrefs {
                retired: many.clone(),
                ..Default::default()
            },
        )
        .unwrap();
        let (back, notes) = load(&p);
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(
            back.retired, many,
            "the list did not survive the round trip"
        );

        let dir = p.parent().unwrap();
        let debris: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n != "head.toml")
            .collect();
        assert!(debris.is_empty(), "the save left these behind: {debris:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// **A key another head recorded survives this head's save.**
    ///
    /// The file is one file for every head on the box — measured 2026-09-22, two
    /// `letibot-tui` processes against one `~/.config/letibot/head.toml` — and each head
    /// loads it once. A save that wrote its own list back whole would discard whatever
    /// the other had retired since, which is the shape of *"i dismissed letibot notes but
    /// they stay."*
    #[test]
    fn a_merge_keeps_both_heads_keys_and_stays_capped() {
        let p = tmp("merge");
        save(
            &p,
            &HeadPrefs {
                retired: vec!["w|theirs|1|aaaa".into()],
                ..Default::default()
            },
        )
        .unwrap();
        let merged = merge_retired(
            &p,
            &["w|mine|2|bbbb".to_string(), "w|theirs|1|aaaa".to_string()],
        );
        assert!(
            merged.contains(&"w|theirs|1|aaaa".to_string()),
            "{merged:?}"
        );
        assert!(merged.contains(&"w|mine|2|bbbb".to_string()), "{merged:?}");
        assert_eq!(merged.len(), 2, "a duplicate was kept: {merged:?}");

        // And the cap holds, with the oldest out.
        let flood: Vec<String> = (0..(RETIRED_CAP + 5))
            .map(|i| format!("w|f{i}|0|0"))
            .collect();
        assert_eq!(merge_retired(&p, &flood).len(), RETIRED_CAP);
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn a_missing_file_is_the_defaults_and_a_save_round_trips() {
        let p = tmp("rt");
        let (prefs, notes) = load(&p);
        assert_eq!(prefs, HeadPrefs::default());
        assert!(notes.is_empty());
        let changed = HeadPrefs {
            diff: DiffPref::Unified,
            thinking: "open".into(),
            tools: "folded".into(),
            raw_calls: true,
            // The rung, which is the operator's *"persists headrestarts"*.
            verbosity: "conversation".into(),
            // R10's half of the round trip, in one key: the comma is the separator
            // and the key contains none, which is what makes this one line.
            retired: vec!["w|gate-timeout|1789000000000|5f2c9a0b1d3e4f67".into()],
            ..Default::default()
        };
        save(&p, &changed).unwrap();
        let (back, notes) = load(&p);
        assert_eq!(back, changed);
        assert!(notes.is_empty(), "{notes:?}");
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    /// A person's comment and a key from a newer build survive a save; a value
    /// this build cannot read is named, not swallowed.
    #[test]
    fn unknown_lines_are_kept_and_bad_values_are_named() {
        let p = tmp("keep");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(
            &p,
            "# mine\ndiff = \"unified\"\nfuture_key = \"x\"\nthinking = \"sideways\"\n",
        )
        .unwrap();
        let (prefs, notes) = load(&p);
        assert_eq!(prefs.diff, DiffPref::Unified);
        assert_eq!(
            prefs.thinking, "folded",
            "a bad value fell back to the default"
        );
        assert!(notes.iter().any(|n| n.contains("future_key")), "{notes:?}");
        assert!(notes.iter().any(|n| n.contains("sideways")), "{notes:?}");
        save(
            &p,
            &HeadPrefs {
                diff: DiffPref::Split,
                ..prefs
            },
        )
        .unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.starts_with("# mine\n"), "{text}");
        assert!(text.contains("diff = \"split\""), "{text}");
        assert!(text.contains("future_key = \"x\""), "{text}");
        assert_eq!(text.matches("diff =").count(), 1, "{text}");
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    /// **`todo_template`'s three shapes round-trip**, and a shape this build cannot read is
    /// reported rather than guessed at — leticl's own refusal, copied: an empty value is
    /// neither off nor a path.
    #[test]
    fn todo_template_round_trips_its_three_shapes() {
        let dir = std::env::temp_dir().join(format!("letibot-tpl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("head.toml");
        for (value, parsed) in [
            ("false", TodoTemplate::Off),
            ("true", TodoTemplate::Default),
            (
                "\"~/checklists/x.md\"",
                TodoTemplate::Path("~/checklists/x.md".into()),
            ),
        ] {
            std::fs::write(&p, format!("todo_template = {value}\n")).unwrap();
            let (prefs, notes) = load(&p);
            assert_eq!(prefs.todo_template, parsed, "reading {value}: {notes:?}");
            save(&p, &prefs).unwrap();
            let (again, notes) = load(&p);
            assert_eq!(again.todo_template, parsed, "round trip {value}: {notes:?}");
        }
        // The words this build refuses.
        std::fs::write(&p, "todo_template = \"\"\n").unwrap();
        let (prefs, notes) = load(&p);
        assert_eq!(prefs.todo_template, TodoTemplate::Off, "off, not a guess");
        assert!(
            notes.iter().any(|n| n.contains("todo_template")),
            "said: {notes:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **The seeded-projects record unions on write** — `merge_retired`'s twin, and for its
    /// reason: two heads share this file, and a dropped record is a project that re-seeds.
    #[test]
    fn todo_seed_unions_across_heads() {
        let dir = std::env::temp_dir().join(format!("letibot-seedrec-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("head.toml");
        std::fs::write(&p, "todo_seed = \"aaaaaaaaaaaaaaaa,bbbbbbbbbbbbbbbb\"\n").unwrap();
        let ours = vec![
            "cccccccccccccccc".to_string(),
            "aaaaaaaaaaaaaaaa".to_string(),
        ];
        let merged = merge_todo_seed(&p, &ours);
        assert_eq!(
            merged,
            vec![
                "aaaaaaaaaaaaaaaa".to_string(),
                "bbbbbbbbbbbbbbbb".to_string(),
                "cccccccccccccccc".to_string(),
            ],
            "the file's keys survived a head that never saw them"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
