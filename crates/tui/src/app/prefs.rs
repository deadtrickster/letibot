//! The head's settings: the preferences file, the config pane's rows and their edits, and the
//! git field's format.

use super::*;

/// Is this a warning code - or a family of codes - that this build knows? A mute names a KIND,
/// and the kind a note is drawn under is `warning::family`'s answer, so both spellings count.
fn known_warning(code: &str) -> bool {
    letibot_sessionlog::warning::TABLE
        .iter()
        .any(|(c, _)| *c == code || letibot_sessionlog::warning::family(c) == code)
}

impl App {
    /// The theme `head.toml` names, and its `color.` lines, made the look of every role. The file
    /// is `themes/NAME.toml` beside it; `terminal` or no key is the terminal's own palette. What
    /// cannot be used is said and not obeyed: a typo must not draw as a deliberate default.
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
    }

    /// Take a git reading from the LOOP, on a thread: it spawns a process, so the frame cannot draw
    /// it and the loop must never wait for it. The format is applied here, on the reader, so a
    /// mistyped template stays a bad line rather than a header that fails to draw.
    pub fn refresh_git(&mut self) {
        let ws = self.wiring.workspace.clone();
        if ws.is_empty() {
            self.git = None;
            self.git_state = None;
            self.git_job = None;
            return;
        }
        if crate::gitfield::plain_home(&ws) {
            self.git_job = None;
            self.git_state = None;
            self.apply_git_format();
            return;
        }
        // Which repository, in a workspace of several: the pinned one, else the one the work is
        // in (see `followed_repo`). The reading's key carries it, so following another repository
        // reads at once, and a reading of the one the field has left is dropped.
        let repos = crate::gitfield::repos_in(&ws);
        let dir = if repos.len() > 1 {
            self.git_pinned
                .clone()
                .filter(|p| repos.contains(p))
                .or_else(|| self.followed_repo(&ws))
        } else {
            None
        };
        let key = match &dir {
            Some(d) => format!("{ws}\n{}", d.display()),
            None => ws.clone(),
        };
        self.git_repos = repos.clone();
        self.git_dir = dir.clone();
        // Several and nothing to follow yet: no reading at all - the field says how many, and a
        // click on it opens the card that lists them, rather than no field, which read as no
        // repository here.
        if dir.is_none() && repos.len() > 1 {
            self.git_job = None;
            self.git_state = None;
            self.git = Some(vec![(
                format!("{} repos", repos.len()),
                crate::gitfield::GitRole::BranchClean,
            )]);
            self.git_read = (key, self.now_ms);
            return;
        }
        if let Some((of, rx)) = &self.git_job {
            match rx.try_recv() {
                // Still reading this one: one at a time, and nothing to draw yet.
                Err(std::sync::mpsc::TryRecvError::Empty) if of == &key => return,
                // Reading a workspace (or a repository) the head has left: abandoned, and its
                // answer with it - the thread's send fails on a dropped receiver and it ends.
                Err(std::sync::mpsc::TryRecvError::Empty) => self.git_job = None,
                Ok(state) => {
                    let of = of.clone();
                    self.git_job = None;
                    if of == key {
                        self.git_state = state;
                        self.apply_git_format();
                        self.git_read = (key, self.now_ms);
                        return;
                    }
                }
                // The reader died without answering: an absence, stamped, so a reader that
                // panics is not restarted on every tick.
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    let of = of.clone();
                    self.git_job = None;
                    if of == key {
                        self.git_state = None;
                        self.apply_git_format();
                        self.git_read = (key, self.now_ms);
                        return;
                    }
                }
            }
        }
        let (last_of, at) = &self.git_read;
        let stale = self.now_ms.saturating_sub(*at) >= crate::gitfield::GIT_REFRESH_MS;
        if last_of == &key && !stale {
            return;
        }
        // Another tree's branch is not drawn over this one while its own reading runs: a switch
        // blanks the field rather than leaving the old repository's state up.
        if last_of != &key {
            self.git = None;
            self.git_state = None;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let reader = self.git_reader.clone();
        // The reader reads a path the way `read_state` does: the followed repository's own, or
        // the workspace (which `project_dir` resolves).
        let target = dir
            .as_ref()
            .map(|d| d.display().to_string())
            .unwrap_or_else(|| ws.clone());
        let spawned = std::thread::Builder::new()
            .name("git-field".into())
            .spawn(move || {
                let _ = tx.send(reader(&target));
            });
        match spawned {
            Ok(_) => self.git_job = Some((key, rx)),
            // No thread, no reading: said as an absence and tried again after the interval.
            Err(_) => {
                self.git = None;
                self.git_state = None;
                self.git_read = (key, self.now_ms);
            }
        }
    }

    /// The repository the session's newest work is in, among the workspace's: the newest edit's
    /// path, else the newest tool call's path or working directory, read off the rows this head
    /// holds and the live turn's calls. Derived each reading, not kept.
    pub(crate) fn followed_repo(&self, ws: &str) -> Option<std::path::PathBuf> {
        use letibot_sessionlog::view::CallState;
        use letibot_transcript::TranscriptItem as T;
        let holding = |p: &str| crate::gitfield::repo_holding(ws, p);
        if let Some(t) = &self.turn {
            for c in t.calls.iter().rev() {
                if let CallState::Finished { edit: Some(e), .. } = &c.state
                    && let Some(r) = holding(&e.path)
                {
                    return Some(r);
                }
            }
        }
        for it in self.items.iter().rev().take(400) {
            let found = match it.item.as_ref() {
                Some(T::ToolResult { edit, .. }) => self
                    .call_edits
                    .get(&it.item_id)
                    .or(edit.as_ref())
                    .and_then(|e| holding(&e.path)),
                Some(T::Assistant { tool_calls, .. }) => tool_calls
                    .iter()
                    .rev()
                    .filter_map(|c| crate::gitfield::call_path(&c.arguments))
                    .find_map(|p| holding(&p)),
                _ => None,
            };
            if found.is_some() {
                return found;
            }
        }
        None
    }

    /// The workspace as the header names it: the followed or pinned repository's own path, in a
    /// workspace of several - so the path beside the branch is the tree the branch is of.
    pub(crate) fn header_workspace(&self) -> String {
        match &self.git_dir {
            Some(d) => d.display().to_string(),
            // The repository, not the folder that holds it: a workspace of several would read as
            // one path for every session, and none would say which tree it is in. `repo_dir`
            // answers the one the git field reads, and nothing resolving leaves the workspace.
            None => {
                let repo = crate::gitfield::repo_dir(&self.wiring.workspace);
                if repo.display().to_string() != self.wiring.workspace {
                    repo.display().to_string()
                } else {
                    self.wiring.workspace.clone()
                }
            }
        }
    }

    /// Render the cached state through the format in force - the whole of what a format change
    /// needs to do, and the reason the state is cached: no process, no interval, the same facts
    /// re-said in the new template's words.
    pub(crate) fn apply_git_format(&mut self) {
        let format = self
            .git_format
            .clone()
            .unwrap_or_else(|| crate::gitfield::GIT_FORMAT_DEFAULT.to_string());
        self.git = self
            .git_state
            .as_ref()
            .map(|s| crate::gitfield::git_pieces(s, &format))
            .or_else(|| {
                crate::gitfield::plain_home(&self.wiring.workspace).then(|| {
                    vec![(
                        crate::gitfield::PLAIN_HOME.to_string(),
                        crate::gitfield::GitRole::BranchClean,
                    )]
                })
            });
    }

    pub fn load_prefs(&mut self) {
        // The head's three `leticode.toml` tables first, because the verbosity below is read
        // against the profiles they define - and a `[keys]` line decides what a key does from the
        // first press. They are independent of `head.toml`, so they are read before its absence
        // can stop this function.
        self.load_rules();
        self.prefs_path = self.prefs_path.clone().or_else(crate::prefs::path);
        let Some(path) = self.prefs_path.clone() else {
            return;
        };
        let (p, notes) = crate::prefs::load(&path);
        self.apply_theme(&path, &p);
        self.diff_split = p.diff == crate::prefs::DiffPref::Split;
        // The set comes back too: the card could change it and the file did not keep it, so a
        // reader who chose `conversation` got `normal` on every restart. `Visibility::parse` is
        // the reader the card and the verb use, and the head's start is the base it reads against.
        self.visibility = match Visibility::parse_with(&p.verbosity, &self.profiles) {
            Ok(Change::Set(v)) => v,
            // A bare switch name in the file is a change against the head's start, for the
            // same reason it is one on the verb: *this switch, from where I am*.
            Ok(Change::Switch(s, l)) => Visibility::starting().with(s, l),
            // An unreadable word is REPORTED and not obeyed: silently starting at the default
            // would make a typo and a deliberate `normal` the same screen. The sentence is the
            // parse's own, so a name this refuses is a name the verb refuses with the same words.
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
        // The fold keys move the switches they are, but only where the switch is SHOWING: an old
        // file says `thinking = "open"` beside a profile that hides the row, which is no fact
        // about the screen. `raw-calls` is read straight: its level IS this boolean.
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
        // The todos pane's fold comes with the rest: one line in the file, and a reader who left
        // the done group shut yesterday gets the same board today.
        self.todos_fold_done = p.todos_fold_done;
        self.visibility = self.visibility.with(
            Show::RawCalls,
            if p.raw_calls {
                Level::Open
            } else {
                Level::Hidden
            },
        );
        // The starter-todo switch and its record come with the rest: the seed runs at the attach,
        // long after this, and a switch or record that lived only in this run would re-seed every
        // project on every restart - the duplicate the record exists to prevent.
        self.todo_template = p.todo_template;
        self.todo_seed = p.todo_seed;
        self.git_format = p.git_format;
        self.sidebar.width = p.rooms_sidebar_width.map(usize::from);
        // A format that was loaded before the first reading still has nothing to render
        // over - but a head that RESUMES into a session renders the header at once, so the
        // format is applied here too and not only on the reader's first tick.
        self.apply_git_format();
        // The retired notes come from the file, not from the process: a dismissal that lived only
        // in this run would last until the next restart, which is the defect rather than the fix.
        self.dismissed = p.retired.clone();
        // One notice for all of them: the notice is one line, and saying each problem in turn
        // kept only the last - a theme that did not load hid the bad `diff` above it.
        let mut all = notes;
        all.extend(std::mem::take(&mut self.theme_problems));
        if !all.is_empty() {
            self.say(&all.join(" · "));
        }
    }

    /// The head's own name for the set in force, as the status row, `/status` and `head.toml` say
    /// it: a config profile's name when the set is one, the head's own five next, and `custom …`
    /// otherwise. One reader, so the three cannot disagree about what the screen is called.
    pub(crate) fn verbosity_name(&self) -> String {
        self.visibility.as_str_with(&self.profiles)
    }

    /// Take the `leticode.toml` levels as this head reads them: `[warnings]`' mute list and
    /// `coalesce`, `[keys]`' rebindings and `[verbosity.<name>]`' profiles. Each word is the head's
    /// vocabulary, so the resolution lives in the module that owns it and every refusal names the
    /// file and the line it was written on - and is SAID, not obeyed, which is `head.toml`'s rule
    /// for a value this build cannot read.
    pub(crate) fn load_rules(&mut self) {
        let (cfg, said) = crate::prefs::leticode(&self.wiring.workspace);
        let mut problems = said;
        self.quiet = crate::app::Quiet {
            muted: cfg.warnings.mute.iter().map(|e| e.value.clone()).collect(),
            coalesce: cfg.warnings.coalesce.unwrap_or(true),
        };
        // A code nothing knows is a mute that would never fire, which is the silently-ignored
        // key this tree names: it is said with the line, and the rest of the list stands.
        for e in &cfg.warnings.mute {
            if !known_warning(&e.value) {
                problems.push(format!(
                    "{}: [warnings] mute `{}` is not a warning code this build knows - the codes \
                     are in `letibot_sessionlog::warning::TABLE`",
                    e.from, e.value
                ));
            }
        }
        self.quiet.muted.retain(|code| known_warning(code));
        self.sidebar_set(cfg.ui.rooms_sidebar.unwrap_or(false));
        self.sidebar.default_width = cfg.ui.rooms_sidebar_width.map(usize::from);
        let (overrides, key_said) = KeyOverrides::resolve(&cfg.keys);
        self.key_overrides = overrides;
        problems.extend(key_said);
        let (profiles, profile_said) = Profiles::resolve(&cfg.verbosity);
        self.profiles = profiles;
        problems.extend(profile_said);
        if !problems.is_empty() {
            self.say(&problems.join(" · "));
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
            todos_fold_done: self.todos_fold_done,
            verbosity: self.verbosity_name(),
            retired: self.dismissed.clone(),
            todo_template: self.todo_template.clone(),
            todo_seed: self.todo_seed.clone(),
            git_format: self.git_format.clone(),
            rooms_sidebar_width: self.sidebar.width.and_then(|n| u16::try_from(n).ok()),
            // Not the head's to write: `save` keeps the file's own `theme` and `color.` lines
            // where they are, so these are only what the struct needs to be whole.
            theme: rano::theme::active_name(),
            colors: Vec::new(),
        }
    }

    /// Write the head's choices. Returns the suffix for the confirmation line: where it went, or
    /// why it did not, since a silent failure is found at the next start. `retired` cannot be one
    /// rule: a dismissal and a restore are opposite assertions about one key ([`RetiredWrite`]).
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

    /// The pane line the config cursor is on, in the layout `rano::agent::config` draws.
    pub(crate) fn config_sel_line(&self) -> usize {
        let lines = self.config_stop_rows();
        let sel = self.config_sel.min(lines.len().saturating_sub(1));
        lines.get(sel).copied().unwrap_or(2)
    }

    /// The pane line of EVERY row: rano's `ConfigPane::stop_rows`, at the width the pane was last
    /// drawn at (`cfg.width`, the inner one). The click map's input: a click on a row moves the
    /// cursor to the row drawn there.
    pub(crate) fn config_stop_rows(&self) -> Vec<usize> {
        self.config_pane().stop_rows(self.cfg.width)
    }

    /// The view follows the config cursor: the pane's rows outgrow a short terminal, and an arrow
    /// that walked the cursor off the bottom looked like a key that did nothing. Its `from ...`
    /// line is kept in view too, and the first row shows the pane from its title.
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
            self.verbosity_name(),
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
        // The todos pane's `d`, here because every switch this head keeps is here, and the same
        // value the pane's bar names, so the two cannot disagree. `folded` is the word, not
        // `hidden`: the rows are in the pane either way, and the switch is how their group stands.
        rows.push(head(
            "todo done rows",
            if self.todos_fold_done {
                "folded".into()
            } else {
                "shown".into()
            },
            ConfigEdit::Head(HeadSetting::FoldDone),
        ));
        // The git field's format is here because the operator looked for it here, and the row
        // names the template IN FORCE, so the pane and the row cannot disagree; the first stop of
        // the cycle is the built-in default.
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
                edit: if r.editable.is_empty() && r.key.starts_with(LISTEN_ROW) {
                    ConfigEdit::No("muted in leticode.toml - edit the file, /listen cannot lift it")
                } else if r.editable.is_empty() {
                    ConfigEdit::No("takes a restart of the daemon")
                } else {
                    ConfigEdit::Session(r.key.clone(), r.editable.clone())
                },
            });
        }
        for f in [
            "permission.json",
            "providers.toml",
            // The system prompt's per-model overrides: a file the operator edits by hand like the
            // three above, and the one they would never find, since nothing else names it - and an
            // override silently not read looks exactly like one that is.
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
        let row = rows.get(self.config_sel.min(rows.len().saturating_sub(1)))?;
        match &row.edit {
            ConfigEdit::Head(which) => {
                match which {
                    HeadSetting::Diff => {
                        self.diff_split = !self.diff_split;
                        self.invalidate_history();
                    }
                    HeadSetting::Verbosity => {
                        // It opens the card rather than cycling: four values are chosen from a card
                        // and not walked through, so the pane points at the verb instead of doing the
                        // thing the card exists to stop.
                        self.say("`/verbosity` with nothing after it opens the card");
                        return None;
                    }
                    HeadSetting::Thinking => {
                        // The one verb (`App::flip_thinking`), so the pane's row cannot write a
                        // fold that `ctrl-r` and the marker's clause do not read.
                        self.flip_thinking();
                    }
                    HeadSetting::Tools => {
                        self.tools = self.tools.flip();
                        self.invalidate_history();
                    }
                    HeadSetting::RawCalls => {
                        self.raw_calls = !self.raw_calls;
                        self.invalidate_history();
                    }
                    // The pane's own switch, flipped from the pane's own key: one toggle behind two
                    // doors, so the file, the bar and the `/config` row cannot drift. It saves and
                    // says its piece itself, which is why this arm returns early.
                    HeadSetting::FoldDone => {
                        self.toggle_todos_fold_done();
                        return None;
                    }
                    HeadSetting::GitFormat => {
                        // Three stops, and the default is `None` rather than a fourth string: the
                        // default has to stay one value in one place (`GIT_FORMAT_DEFAULT`), so
                        // the cycle passes through `None` and not through a copy of it.
                        self.git_format = match self.git_format.as_deref() {
                            None => Some("%b %!%+".into()),
                            Some("%b %!%+") => Some("%b".into()),
                            _ => None,
                        };
                        // Re-rendered from the cache at once: the reading is the reader's business
                        // and the template is this head's, so a change must not wait out the
                        // interval to be seen (`apply_git_format`).
                        self.apply_git_format();
                    }
                }
                // Union. A fold or a raw-call toggle is not a statement about the retired set at
                // all, so it must not discard another head's dismissals on its way past.
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
                    // The names come from the daemon, on the row: a second copy of a list drifts,
                    // and a pane with its own list could not reach the point the daemon stood at.
                    // No choices means a daemon older than the list, and the pane says so.
                    "mode" => {
                        if row.choices.is_empty() {
                            self.say("this daemon does not send the mode list; use `/mode NAME`");
                            return None;
                        }
                        // The same reading the card makes, or the cycle starts from the wrong
                        // place: taking the value's first word found no row at `writes allowed`
                        // and wrapped to the FIRST one. The two readers are one defect.
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
                    // A repository's checkbox: flip it with `/listen`, then re-read the rows, since
                    // the daemon pushes nothing when a mute changes.
                    k if k.starts_with(LISTEN_ROW) => {
                        let name = &k[LISTEN_ROW.len()..];
                        let sign = if row.value.starts_with("[x]") {
                            '+'
                        } else {
                            '-'
                        };
                        let line = format!("listen {sign}{name}");
                        self.say(&format!("/{line} (asking the daemon)"));
                        self.queued.push(Action::Settings);
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

/// The key prefix of a repository's mute checkbox (`listen.NAME`), as the daemon publishes it.
pub(crate) const LISTEN_ROW: &str = "listen.";

/// One row of the config pane.
#[derive(Debug, Clone)]
pub(crate) struct ConfigRow {
    pub(crate) section: &'static str,
    pub(crate) key: String,
    pub(crate) value: String,
    /// Where the value came from - a path, a flag, "default" - shown under the
    /// selected row. Empty when nobody tracks it.
    pub(crate) source: String,
    /// The values this row may take, as the DAEMON sent them. Empty for a row
    /// with no closed set, and for a daemon older than protocol 18 - the pane
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
    /// The rung of the ladder, persisted like the rest. The pane's row is a READER of the setting
    /// rather than a second way to set it: Enter says which verb opens the card, because four
    /// values are chosen from a card and not cycled - a cycling row would be that interface again.
    Verbosity,
    Thinking,
    Tools,
    RawCalls,
    /// The todos pane's `d`: the done group folded away, or shown again. Enter goes through
    /// [`App::toggle_todos_fold_done`], the one implementation, so the notice and the save are the
    /// same from either door.
    FoldDone,
    /// The git field's template: cycles three stops, the shipped default, a spaced one, and the
    /// branch alone. `None` is the default rather than a fourth string, so the default stays one
    /// value in one place. A FREE template stays the file's business, and the pane cycles presets.
    GitFormat,
}
