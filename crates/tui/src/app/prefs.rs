//! **The head's settings**: the preferences file, the config pane's rows and their edits,
//! and the git field's format.

use super::*;

impl App {
    /// **The theme `head.toml` names, and its `color.` lines, made the look of every role.**
    ///
    /// The file is `themes/NAME.toml` beside `head.toml`; `terminal` or no `theme` key is the
    /// terminal's own palette, which is what this head drew with before themes existed. What
    /// cannot be used — a missing theme, a role that does not exist, a look that does not
    /// read — is said in the conversation and not obeyed: a typo must not draw the same screen
    /// as a deliberate default.
    pub(crate) fn apply_theme(&mut self, head_toml: &std::path::Path, p: &crate::prefs::HeadPrefs) {
        let dir = head_toml
            .parent()
            .map(|d| d.join("themes"))
            .unwrap_or_default();
        let (theme, problems) = rano::theme::resolve(&dir, p.theme.as_deref(), &p.colors);
        rano::theme::set_active(theme);
        self.theme_problems = problems
            .into_iter()
            .map(|s| format!("head.toml: {s}"))
            .collect();
        self.redraw = true;
    }

    /// The session picker: every session this daemon holds, and how to go there.
    ///
    /// A screen and not a mode. There is no pointer in this head and no selection,
    /// so a cursor here would be a second keymap for a program whose whole input
    /// surface is one line — the same argument the folds settled. The affordance is
    /// the number in the left column, which you type into the composer that is
    /// still there under the list.
    /// The todos pane: two lists that are deliberately not one.
    ///
    /// The first is this session's plan — what the model last wrote through
    /// `todo_write`, and the only list here that anything in this session can
    /// change. The second is the repo's `TODO.md`, the **operator's** queue,
    /// shown as a section map and read-only on purpose: a pane that let a model
    /// tick the operator's boxes would let a plan edit its own backlog.
    /// Load the head's preferences from disk and apply them. Called once, before
    /// the first frame; the notes are what could not be read, said on the screen.
    ///
    /// **A path already named is kept.** `main` calls this before anything else and
    /// names nothing, so in production this is still `prefs::path()` — the one file the
    /// head writes. The reason it does not *replace* a path is that the retired notes
    /// are read here and nowhere else, so a test that wants to prove a dismissal
    /// survives a restart has to be able to point a head at the file it wrote; forcing
    /// the reader to reach into `$HOME` would have made the round trip untestable and
    /// left the property asserted nowhere.
    /// **Take a git reading, from the LOOP.** See `crate::gitfield` for why this is not done where
    /// the frame is drawn: it spawns a process.
    ///
    /// A workspace that CHANGED is read at once rather than waiting out the interval, because a
    /// session switch carries another path and the cached field would be the old tree's branch.
    ///
    /// **THE FORMAT IS APPLIED HERE, ON THE READER, AND NOWHERE ELSE** — leticl's `%git-refresh`
    /// rule: a paint draws cached pieces, it never parses and never formats, so a mistyped
    /// template stays a bad line rather than becoming a header that fails to draw. The STATE is
    /// kept beside the pieces for the one thing that changes the format without a new reading:
    /// `/config`'s cycle re-renders from the cache (`apply_git_format`).
    pub fn refresh_git(&mut self) {
        let ws = self.wiring.workspace.clone();
        if ws.is_empty() {
            self.git = None;
            self.git_state = None;
            return;
        }
        let (last_of, at) = &self.git_read;
        let stale = self.now_ms.saturating_sub(*at) >= crate::gitfield::GIT_REFRESH_MS;
        if last_of == &ws && !stale {
            return;
        }
        self.git_state = crate::gitfield::read_state(&ws);
        self.apply_git_format();
        self.git_read = (ws, self.now_ms);
    }

    /// **Render the cached state through the format in force** — the whole of what a format
    /// change needs to do, and the reason the state is cached: no process, no interval, the
    /// same facts re-said in the new template's words.
    pub(crate) fn apply_git_format(&mut self) {
        let format = self
            .git_format
            .clone()
            .unwrap_or_else(|| crate::gitfield::GIT_FORMAT_DEFAULT.to_string());
        self.git = self
            .git_state
            .as_ref()
            .map(|s| crate::gitfield::git_pieces(s, &format));
    }

    pub fn load_prefs(&mut self) {
        self.prefs_path = self.prefs_path.clone().or_else(crate::prefs::path);
        let Some(path) = self.prefs_path.clone() else {
            return;
        };
        let (p, notes) = crate::prefs::load(&path);
        self.apply_theme(&path, &p);
        self.diff_split = p.diff == crate::prefs::DiffPref::Split;
        // **The set comes back too.** It is the one setting the card could change and the
        // file did not keep, so a reader who chose `conversation` got `normal` on every
        // restart. [`Visibility::parse`] is the same reader the card and the verb use — and
        // the head's own start is the BASE a `custom …` name is read against, which is what
        // makes the name a state rather than a caption.
        self.visibility = match Visibility::parse(&p.verbosity) {
            Ok(Change::Set(v)) => v,
            // A bare switch name in the file is a change against the head's start, for the
            // same reason it is one on the verb: *this switch, from where I am*.
            Ok(Change::Switch(s, l)) => Visibility::starting().with(s, l),
            // **An unreadable word is REPORTED and not obeyed.** §13.2b's rule for a setting:
            // silently starting at the default would make a typo and a deliberate `normal`
            // the same screen. The sentence is the parse's own, so a name this refuses is a
            // name the verb refuses with the same words.
            Err(said) => {
                self.say(&format!("head.toml: {said}"));
                Visibility::starting()
            }
        };
        self.reasoning = if p.thinking == "open" {
            Fold::Open
        } else {
            Fold::Folded
        };
        self.tools = if p.tools == "open" {
            Fold::Open
        } else {
            Fold::Folded
        };
        // **The fold keys move the switches they are, but only where the switch is SHOWING.**
        // The three keys are older than this vocabulary and a file written before it says
        // `verbosity = "conversation"` beside `thinking = "open"`, which is a fold on a row
        // nobody draws — not a fact about the screen, and not a reason to raise the profile
        // that hid it. `raw-calls` is the exception and is read straight: its level IS this
        // boolean, and the key that already meant it keeps meaning it.
        for (show, fold) in [(Show::Thinking, self.reasoning), (Show::Tools, self.tools)] {
            if self.visibility.shows(show) {
                self.visibility = self.visibility.with(
                    show,
                    if fold.is_open() {
                        Level::Open
                    } else {
                        Level::Folded
                    },
                );
            }
        }
        self.raw_calls = p.raw_calls;
        self.visibility = self.visibility.with(
            Show::RawCalls,
            if p.raw_calls {
                Level::Open
            } else {
                Level::Hidden
            },
        );
        // **The starter-todo switch and its record come with the rest** — the seed runs at the
        // attach, which is long after this, and a switch or record that lived only in this run
        // would re-seed every project on every restart, which is the duplicate defect the record
        // exists to prevent.
        self.todo_template = p.todo_template;
        self.todo_seed = p.todo_seed;
        self.git_format = p.git_format;
        // A format that was loaded before the first reading still has nothing to render
        // over — but a head that RESUMES into a session renders the header at once, so the
        // format is applied here too and not only on the reader's first tick.
        self.apply_git_format();
        // **R10's retired notes come from the file, not from the process.** A head
        // restart is one of the two things that used to replant the wall, so a
        // dismissal that lived only in this run would be a dismissal that lasts
        // until the next restart — which is the defect, not the fix.
        self.dismissed = p.retired.clone();
        // **One notice for all of them.** The notice is one line, and saying each problem in
        // turn kept only the last: a theme that did not load hid the bad `diff` above it.
        let mut all = notes;
        all.extend(std::mem::take(&mut self.theme_problems));
        if !all.is_empty() {
            self.say(&all.join(" · "));
        }
    }

    /// The head's current choices, as the file holds them.
    pub(crate) fn prefs(&self) -> crate::prefs::HeadPrefs {
        crate::prefs::HeadPrefs {
            diff: if self.diff_split {
                crate::prefs::DiffPref::Split
            } else {
                crate::prefs::DiffPref::Unified
            },
            thinking: fold_word(self.reasoning).into(),
            tools: fold_word(self.tools).into(),
            raw_calls: self.raw_calls,
            verbosity: self.visibility.as_str(),
            retired: self.dismissed.clone(),
            todo_template: self.todo_template.clone(),
            todo_seed: self.todo_seed.clone(),
            git_format: self.git_format.clone(),
            // Not the head's to write: `save` keeps the file's own `theme` and `color.` lines
            // where they are, so these are only what the struct needs to be whole.
            theme: rano::theme::active_name(),
            colors: Vec::new(),
        }
    }

    /// Write the head's choices. Returns the suffix for the confirmation line:
    /// where it went, or why it did not — a change that silently failed to
    /// persist would be found at the next start, as a surprise.
    ///
    /// **`retired` is written according to `retired`, and it cannot be one rule.** A
    /// dismissal and a restore are *opposite* assertions about one key, so the write that
    /// expresses one cannot express the other — see [`RetiredWrite`].
    pub(crate) fn save_prefs(&self, retired: RetiredWrite) -> String {
        match &self.prefs_path {
            None => " (not saved: no $HOME or $XDG_CONFIG_HOME)".into(),
            Some(path) => {
                let mut p = self.prefs();
                p.retired = match retired {
                    RetiredWrite::Union => crate::prefs::merge_retired(path, &self.dismissed),
                    RetiredWrite::Replace => self.dismissed.clone(),
                };
                match crate::prefs::save(path, &p) {
                    Ok(()) => String::new(),
                    Err(e) => format!(" (not saved: {e})"),
                }
            }
        }
    }

    /// The pane's rows, in order. Rebuilt on every draw and every key, so the
    /// cursor and the screen can never disagree about what row N is.
    /// **The pane line the config cursor is on**, in the layout `rano::agent::config` draws:
    /// the title and a blank, then each section's header (a blank before every one but the
    /// first) and one line per row. The selected row's `from …` line comes AFTER it, so it
    /// shifts nothing above. `the_config_cursor_line_is_the_line_rano_marks` holds this to the
    /// widget's real output, so the two cannot drift.
    pub(crate) fn config_sel_line(&self) -> usize {
        let rows = self.config_rows();
        let sel = self.config_sel.min(rows.len().saturating_sub(1));
        let mut line = 2;
        let mut section = "";
        for (i, r) in rows.iter().enumerate() {
            if r.section != section {
                if !section.is_empty() {
                    line += 1;
                }
                line += 1;
                section = r.section;
            }
            if i == sel {
                return line;
            }
            line += 1;
        }
        line
    }

    /// **The view follows the config cursor** — the pane's rows outgrow a short terminal, and
    /// an arrow that walked the cursor off the bottom looked like a key that did nothing until
    /// it had gone round every row: *"the selector eventually goes out of view and doesnt
    /// wrap … until i reenter config pane"* (2026-10-08). Its `from …` line is kept in view
    /// too, and the first row shows the pane from its title.
    pub(crate) fn config_follow(&mut self) {
        if self.config_sel == 0 {
            self.pane_scroll = 0;
            return;
        }
        let line = self.config_sel_line();
        self.scroll_into_view(line + 1);
        self.scroll_into_view(line);
    }

    pub(crate) fn config_rows(&self) -> Vec<ConfigRow> {
        let mut rows = Vec::new();
        let head = |key: &str, value: String, edit: ConfigEdit| ConfigRow {
            choices: Vec::new(),
            section: "head — this window",
            key: key.into(),
            value,
            source: self
                .prefs_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "not persisted".into()),
            edit,
        };
        rows.push(head(
            "diff view",
            if self.diff_split {
                "split".into()
            } else {
                "unified".into()
            },
            ConfigEdit::Head(HeadSetting::Diff),
        ));
        rows.push(head(
            "verbosity",
            self.visibility.as_str(),
            ConfigEdit::Head(HeadSetting::Verbosity),
        ));
        rows.push(head(
            "thinking",
            fold_word(self.reasoning).into(),
            ConfigEdit::Head(HeadSetting::Thinking),
        ));
        rows.push(head(
            "tool output",
            fold_word(self.tools).into(),
            ConfigEdit::Head(HeadSetting::Tools),
        ));
        rows.push(head(
            "raw tool calls",
            if self.raw_calls {
                "shown".into()
            } else {
                "hidden".into()
            },
            ConfigEdit::Head(HeadSetting::RawCalls),
        ));
        // **THE GIT FIELD'S FORMAT IS HERE BECAUSE THE OPERATOR LOOKED FOR IT HERE** —
        // leticl's `72a4314` finding, and the row names the template IN FORCE (so the pane
        // and the row can never disagree), with the first stop of the cycle the built-in
        // default.
        rows.push(head(
            "git format",
            self.git_format
                .clone()
                .unwrap_or_else(|| format!("default ({})", crate::gitfield::GIT_FORMAT_DEFAULT)),
            ConfigEdit::Head(HeadSetting::GitFormat),
        ));
        for r in &self.settings {
            rows.push(ConfigRow {
                section: "session — the daemon",
                key: r.key.clone(),
                value: r.value.clone(),
                source: r.source.clone(),
                choices: r.choices.clone(),
                edit: if r.editable.is_empty() {
                    ConfigEdit::No("takes a restart of the daemon")
                } else {
                    ConfigEdit::Session(r.key.clone(), r.editable.clone())
                },
            });
        }
        for f in [
            "modes.tsv",
            "permission.json",
            "providers.toml",
            // **The system prompt's per-model overrides** — a file the operator edits by hand
            // like the three above it, and the one they would never find: nothing else in the
            // tree names it, and an override that is silently not read looks exactly like an
            // override that is. See `harnessd::config::Prompts`.
            "prompts.toml",
            "sensitive.json",
        ] {
            let path = self
                .prefs_path
                .as_ref()
                .and_then(|p| p.parent())
                .map(|d| d.join(f));
            let (value, source) = match &path {
                Some(p) if p.is_file() => {
                    let n = std::fs::read_to_string(p)
                        .map(|t| t.lines().count())
                        .unwrap_or(0);
                    (format!("{n} lines"), p.display().to_string())
                }
                Some(p) => ("not present".into(), p.display().to_string()),
                None => ("no config directory".into(), String::new()),
            };
            rows.push(ConfigRow {
                section: "files — edit with an editor",
                key: f.into(),
                value,
                source,
                choices: Vec::new(),
                edit: ConfigEdit::No("a file the guard protects: a person edits it, not a pane"),
            });
        }
        rows
    }

    /// Enter on the selected row.
    pub(crate) fn config_change(&mut self) -> Option<Action> {
        let rows = self.config_rows();
        let Some(row) = rows.get(self.config_sel.min(rows.len().saturating_sub(1))) else {
            return None;
        };
        self.redraw = true;
        match &row.edit {
            ConfigEdit::Head(which) => {
                match which {
                    HeadSetting::Diff => {
                        self.diff_split = !self.diff_split;
                        self.invalidate_history();
                    }
                    HeadSetting::Verbosity => {
                        // **It opens the card rather than cycling.** Four values are chosen
                        // from a card and not walked through (R38), so the pane points at the
                        // verb instead of doing the thing the card exists to stop.
                        self.say("`/verbosity` with nothing after it opens the card");
                        return None;
                    }
                    HeadSetting::Thinking => {
                        self.reasoning = self.reasoning.flip();
                        self.invalidate_history();
                    }
                    HeadSetting::Tools => {
                        self.tools = self.tools.flip();
                        self.invalidate_history();
                    }
                    HeadSetting::RawCalls => {
                        self.raw_calls = !self.raw_calls;
                        self.invalidate_history();
                    }
                    HeadSetting::GitFormat => {
                        // **Three stops, and the default is `None` rather than a fourth
                        // string** — the default has to stay one value in one place
                        // (`GIT_FORMAT_DEFAULT`), so the cycle passes through `None` and
                        // not through a copy of it.
                        self.git_format = match self.git_format.as_deref() {
                            None => Some("%b %!%+".into()),
                            Some("%b %!%+") => Some("%b".into()),
                            _ => None,
                        };
                        // **Re-rendered from the cache at once** — the reading is the
                        // reader's business and the template is this head's, so a change
                        // must not wait out the interval to be seen (`apply_git_format`).
                        self.apply_git_format();
                    }
                }
                // **Union.** A fold or a raw-call toggle is not a statement about the retired
                // set at all, so it must not discard another head's dismissals on its way past.
                let saved = self.save_prefs(RetiredWrite::Union);
                let rows = self.config_rows();
                if let Some(r) = rows.get(self.config_sel) {
                    let line = format!("{} → {}{saved}", r.key, r.value);
                    self.say(&line);
                }
                None
            }
            ConfigEdit::Session(key, how) => {
                // The verbs that already exist, so the pane is a way to see and
                // not a second way to set.
                match key.as_str() {
                    // **The names come from the daemon, on the row.** This was a
                    // `const NAMES` here, and a second copy of a list is a copy
                    // that drifts: it offered `supervised`, which is not a mode,
                    // and not `automode-edits`, which is — so the pane could not
                    // reach the point the daemon was already standing at. The
                    // operator, 2026-09-17: *"I started leticode and there is no
                    // automode-edits"*. A row with no choices is a daemon older
                    // than protocol 18, and then the pane says so rather than
                    // cycling a list it made up.
                    "mode" => {
                        if row.choices.is_empty() {
                            self.say("this daemon does not send the mode list; use `/mode NAME`");
                            return None;
                        }
                        // **The same reading the card makes**, or the cycle starts from the wrong
                        // place: this took the value's first word, so at `writes allowed` it found
                        // no row (`writes` is not a mode) and wrapped to the FIRST one — the pane's
                        // mode row cycled to `always-ask` from a point in the middle of the list.
                        // Found by fixing the card and watching this test fail beside it; the two
                        // are one defect and they were three readers apart.
                        let cur = match named_choice(&row.value, &row.choices) {
                            Some(c) => c.to_string(),
                            None => row.value.clone(),
                        };
                        let at = row.choices.iter().position(|n| *n == cur).unwrap_or(0);
                        let next = row.choices[(at + 1) % row.choices.len()].clone();
                        self.say(&format!("mode → {next} (asking the daemon)"));
                        self.mode_action(next)
                    }
                    "supervise" => {
                        let on = row.value.starts_with("on");
                        let line = format!("supervise {}", if on { "off" } else { "on" });
                        self.say(&format!("{line} (asking the daemon)"));
                        Some(Action::Slash { line })
                    }
                    _ => {
                        let line = format!("change it with {how}");
                        self.say(&line);
                        None
                    }
                }
            }
            ConfigEdit::No(why) => {
                let line = format!("{}: {why}", row.key);
                self.say(&line);
                None
            }
        }
    }
}

/// One row of the config pane.
#[derive(Debug, Clone)]
pub(crate) struct ConfigRow {
    pub(crate) section: &'static str,
    pub(crate) key: String,
    pub(crate) value: String,
    /// Where the value came from — a path, a flag, "default" — shown under the
    /// selected row. Empty when nobody tracks it.
    pub(crate) source: String,
    /// The values this row may take, as the DAEMON sent them. Empty for a row
    /// with no closed set, and for a daemon older than protocol 18 — the pane
    /// then says it cannot cycle rather than cycling a list it invented.
    pub(crate) choices: Vec<String>,
    pub(crate) edit: ConfigEdit,
}

#[derive(Debug, Clone)]
pub(crate) enum ConfigEdit {
    /// This head's own: Enter flips it and writes `head.toml`.
    Head(HeadSetting),
    /// The session's, changeable now by an existing verb; `(key, how)`.
    Session(String, String),
    /// Not now, and why.
    No(&'static str),
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum HeadSetting {
    Diff,
    /// **The rung of the ladder** (R37/R38), persisted like the rest. The pane's row is a
    /// *reader* of the setting rather than a second way to set it: Enter says which verb
    /// opens the card, because four values are chosen from a card and not cycled (R38) — a
    /// pane row that cycled them would be the interface R38 removed, one screen over.
    Verbosity,
    Thinking,
    Tools,
    RawCalls,
    /// **The git field's template** — cycles three stops: the shipped default, a spaced
    /// one, and the branch alone. leticl's own cycle (`%flip-head-setting`: *"Three stops:
    /// the shipped default, a spaced one, and the branch alone. `nil` is the default rather
    /// than a fourth string, because the default has to stay one value in one place"*). A
    /// FREE template stays the file's business — `git_format` takes any of them, and the
    /// pane cycles presets rather than pretending to edit text.
    GitFormat,
}
