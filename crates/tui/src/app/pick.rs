//! The pickers' state: which picker is open, its rows and the selection, and the mode a pick asks
//! for.

use super::*;
use letibot_plugin::pane::Act as RowAct;
use letibot_sessionlog::registry::SessionBrief;

impl App {
    /// Answer the session picker: a row number, or enough of an id to be unique. An ambiguous
    /// prefix is refused with the count, never resolved to the first match - switching to the
    /// wrong session is not a keystroke you can take back, since the next prompt lands there.
    pub(crate) fn pick(&mut self, typed: &str) -> Option<Action> {
        if typed.is_empty() {
            self.close_surface(Surface::Picker);
            return None;
        }
        // The number is the n-th SESSION, the same reading rano's list numbers its rows from - a
        // number that means one thing on the screen and another here is the two-enumerations
        // defect. The prefix search stays over every session, so `3 days of work` is still found.
        let numbered = typed.parse::<usize>().ok().filter(|n| *n >= 1);
        if let Some(n) = numbered
            && let Some(i) = self.session_at_number(n)
        {
            let id = self.sessions[i].session_id.clone();
            return self.switch_to(id);
        }
        let hits: Vec<&SessionBrief> = self
            .sessions
            .iter()
            .filter(|s| {
                s.session_id.starts_with(typed)
                    || (!s.title.is_empty()
                        && s.title
                            .to_ascii_lowercase()
                            .contains(&typed.to_ascii_lowercase()))
            })
            .collect();
        match hits.len() {
            1 => {
                let id = hits[0].session_id.clone();
                self.switch_to(id)
            }
            0 => {
                // A number nothing named is answered as a number - the list numbers its
                // sessions, so `no session matches "9"` would read like a naming problem rather
                // than a list that is shorter than what was typed.
                match numbered {
                    Some(n) => {
                        let sessions = self
                            .picker_rows()
                            .iter()
                            .filter(|r| r.number.is_some())
                            .count();
                        self.say(&format!(
                            "there is no session {n} in this list — it numbers its sessions, and \
                             this daemon has {sessions}"
                        ));
                    }
                    None => self.say(&format!(
                        "no session matches {typed:?} — esc closes the list"
                    )),
                }
                None
            }
            n => {
                self.say(&format!(
                    "{n} sessions match {typed:?}; type the number on the left instead"
                ));
                None
            }
        }
    }

    /// A click on the session list: the row under the pointer, and the act of the row it landed
    /// on. A group's line FOLDS, since a group has no session to mark for a later Enter - a click
    /// that only moved the cursor would do nothing. A session's row marks; Enter switches.
    pub(crate) fn click_picker(&mut self, k: usize) {
        self.picker_seat_pending = false;
        self.picker_sel = k;
        let Some(row) = self.picker_row() else {
            return;
        };
        if row.session.is_none()
            && let Some(RowAct::Fold { .. }) = row.enter
        {
            self.perform(&row.id, &RowAct::fold());
        }
    }

    /// Take a submitted line while the mode picker is up: a row number, or a mode name - exact,
    /// or a prefix only one mode shares. Normalization is `Mode::parse`'s own read-only side
    /// (case, `_` and a space fold to `-`), and an exact match wins before prefixes are counted.
    pub(crate) fn pick_mode(&mut self, typed: &str) -> Option<Action> {
        if typed.is_empty() {
            self.pick = None;
            return None;
        }
        let choices = self.mode_choices();
        if choices.is_empty() {
            self.say("this daemon does not send the mode list; use `/mode NAME`");
            return None;
        }
        if let Ok(n) = typed.parse::<usize>()
            && n >= 1
            && n <= choices.len()
        {
            let name = choices[n - 1].clone();
            return self.take_mode(name);
        }
        let norm = |s: &str| s.to_ascii_lowercase().replace(['_', ' '], "-");
        let want = norm(typed);
        if let Some(exact) = choices.iter().find(|c| norm(c) == want) {
            let name = exact.clone();
            return self.take_mode(name);
        }
        let mut hits: Vec<String> = choices
            .iter()
            .filter(|c| norm(c).starts_with(&want))
            .cloned()
            .collect();
        match hits.len() {
            1 => self.take_mode(hits.remove(0)),
            0 => {
                self.say(&format!("no mode matches {typed:?} — esc closes the list"));
                None
            }
            n => {
                self.say(&format!(
                    "{n} modes match {typed:?}; type the number on the left instead"
                ));
                None
            }
        }
    }

    /// Take the line typed while the model card is up. The line FILTERS - the card draws what
    /// it leaves - and Enter takes the marked row, else the exact name, else the best match.
    /// A row number is not read here as it is on the mode card: a model name is full of digits,
    /// so a digit is a filter character. The number keys still mark a row on an empty line.
    pub(crate) fn pick_model(&mut self, typed: &str) -> Option<Action> {
        if typed.trim().is_empty() {
            self.pick = None;
            return None;
        }
        let all = self.model_choices();
        if all.is_empty() {
            self.say("this daemon does not send the model list; use `/models PROVIDER/MODEL`");
            return None;
        }
        let rows = filter_models(&all, typed);
        if rows.is_empty() {
            // The line goes BACK: a filter that was refused is a word to correct, and the card
            // draws nothing without it, so losing it would leave an empty list and no reason.
            self.set_composer(typed);
            self.say(&format!("no model matches {typed:?} — esc closes the list"));
            return None;
        }
        // What the arrows marked, when they marked anything: the reader can see which row is
        // marked, so Enter must take that one and not the filter's own first guess.
        let marked = if self.pick_unseeded {
            None
        } else {
            rows.get(self.mode_sel).map(|(n, _)| n.clone())
        };
        let want = typed.trim().to_ascii_lowercase();
        let exact = rows
            .iter()
            .find(|(n, _)| {
                let lower = n.to_ascii_lowercase();
                lower == want || lower.rsplit('/').next() == Some(want.as_str())
            })
            .map(|(n, _)| n.clone());
        let name = marked.or(exact).unwrap_or_else(|| rows[0].0.clone());
        self.take_pick(name)
    }

    /// The `model` row's choices with the facts the daemon sent beside them, in step. A facts
    /// row of a different length is dropped rather than zipped: a misaligned line would give a
    /// model another one's window, which is worse than showing none.
    pub(crate) fn model_choices(&self) -> Vec<(String, String)> {
        let Some(row) = self
            .settings
            .iter()
            .find(|r| r.key == letibot_sessionlog::protocol::MODEL_KEY)
        else {
            return Vec::new();
        };
        let facts = self
            .settings
            .iter()
            .find(|r| r.key == letibot_sessionlog::protocol::MODEL_FACTS_KEY)
            .filter(|f| f.choices.len() == row.choices.len())
            .map(|f| f.choices.as_slice())
            .unwrap_or_default();
        row.choices
            .iter()
            .enumerate()
            .map(|(i, c)| (c.clone(), facts.get(i).cloned().unwrap_or_default()))
            .collect()
    }

    /// Leave the mode picker for the mode the operator chose. The mode the session already runs
    /// under closes the list and says so, the way the session picker answers Enter on its own
    /// row: a round trip to be told what the screen already showed is not worth its flicker.
    pub(crate) fn take_mode(&mut self, name: String) -> Option<Action> {
        self.pick = None;
        if name == self.mode_current() {
            self.say("already that mode");
            return None;
        }
        self.mode_action(name)
    }

    /// The mode row of the daemon's last settings answer, and the two facts
    /// the picker and the config pane both read from it. `None` is a daemon
    /// that has not answered yet, or one older than protocol 18.
    pub(crate) fn mode_row(&self) -> Option<&letibot_sessionlog::protocol::SettingRow> {
        self.settings.iter().find(|r| r.key == "mode")
    }

    /// The mode names, as the daemon spelled them. Empty when it sent none  -
    /// the head keeps no list of its own to fall back on, because a second
    /// copy of a list is a copy that drifts.
    pub(crate) fn mode_choices(&self) -> Vec<String> {
        self.mode_row()
            .map(|r| r.choices.clone())
            .unwrap_or_default()
    }

    /// The settings row the open card is choosing from, or `None` for a setting this head owns:
    /// `Verbosity` and the diff style have no daemon row. One function for the four, because the
    /// choice of row is the only thing that differs - see [`Pick::row_key`].
    pub(crate) fn pick_row(&self) -> Option<&letibot_sessionlog::protocol::SettingRow> {
        let key = self.pick?.row_key()?;
        self.settings.iter().find(|r| r.key == key)
    }

    /// What the open card offers, as values with meanings. A daemon's setting is read from its
    /// own `SettingRow` and its values carry NO sentence - the head does not know what
    /// `automode-edits` means, and a gloss would be writing the other half's documentation.
    pub(crate) fn pick_values(&self) -> Vec<(String, String)> {
        let Some(subject) = self.pick else {
            return Vec::new();
        };
        // The verbosity card is the PROFILE TABLE, not a second list built beside it: a copy
        // drifts, and the copy this replaced was missing a rung, so `/v` could cycle onto a
        // profile the card had no row for. One table, so `Profile::ALL` appears by construction.
        if subject == Pick::Verbosity {
            let mut rows: Vec<(String, String)> = Profile::ALL
                .iter()
                .map(|p| (p.name.to_string(), p.why.to_string()))
                .collect();
            // And the profiles `leticode.toml` defines, after the head's five: a profile a file adds
            // is a profile the card must offer, or the only way to it is the file again. Its `why`
            // is the list itself, because that IS the profile - spelled as the switches the head
            // draws, so a reader can see what they are choosing.
            rows.extend(self.profiles.named().iter().map(|(name, vis)| {
                let shows: Vec<String> = Show::ALL
                    .iter()
                    .filter(|s| vis.shows(**s))
                    .map(|s| format!("{}={}", s.name(), vis.level(*s).as_str()))
                    .collect();
                (
                    name.clone(),
                    format!(
                        "the file's own — {}",
                        if shows.is_empty() {
                            "nothing but the conversation".to_string()
                        } else {
                            shows.join(" ")
                        }
                    ),
                )
            }));
            // And the set in force, when no profile names it: the rows above are the table and the
            // set is runtime state, so a set off the ladder would have no row. Its name is the
            // `custom ...` string `as_str` writes to be typed back, which is what `pick_current` marks.
            if self.profiles.name_of(self.visibility).is_none()
                && self.visibility.profile().is_none()
                && !rows.iter().any(|(v, _)| v == &self.verbosity_name())
            {
                rows.push((
                    self.verbosity_name(),
                    "the set in force — no profile names it, and Enter on this row keeps it"
                        .to_string(),
                ));
            }
            return rows;
        }
        if subject == Pick::Repo {
            return self.repo_rows.clone();
        }
        if !subject.values().is_empty() {
            return subject
                .values()
                .iter()
                .map(|(v, why)| ((*v).to_string(), (*why).to_string()))
                .collect();
        }
        // The model card is the one list that does not fit on a screen, so it is the one that
        // filters as a line is typed. Filtering HERE and not in the drawing keeps the drawn rows,
        // the numbers on them and what Enter takes one list - the defect the spans already avoid.
        if subject == Pick::Model {
            return filter_models(&self.model_choices(), self.editor.text());
        }
        self.pick_row()
            .map(|r| {
                r.choices
                    .iter()
                    .map(|c| (c.clone(), String::new()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// What the open card is already on: the profile when the set is one, the set's own
    /// `custom ...` name when it is not - the same name `pick_values` draws a row under, so a
    /// custom set is marked like any other state instead of matching nothing.
    pub(crate) fn pick_current(&self) -> String {
        match self.pick {
            Some(Pick::Verbosity) => self.verbosity_name(),
            Some(Pick::Repo) => self
                .git_pinned
                .as_deref()
                .map(repo_name)
                .unwrap_or_else(|| FOLLOW.to_string()),
            Some(Pick::Diff) => {
                if self.diff_split {
                    "split".into()
                } else {
                    "unified".into()
                }
            }
            _ => {
                let Some(r) = self.pick_row() else {
                    return String::new();
                };
                match named_choice(&r.value, &r.choices) {
                    Some(c) => c.to_string(),
                    None => r.value.clone(),
                }
            }
        }
    }

    /// Commit the highlighted row. The four subjects differ only here.
    pub(crate) fn take_pick(&mut self, name: String) -> Option<Action> {
        match self.pick {
            // The daemon's two: a mode is a protocol command this head already has, a model
            // is a daemon verb.
            Some(Pick::Mode) => self.take_mode(name),
            Some(Pick::Model) => {
                self.pick = None;
                if self.session_id.is_empty() {
                    self.say("not attached to a session yet");
                    return None;
                }
                // A row this box holds no key behind ASKS rather than switching - left to the daemon
                // the switch came back as a refusal telling the operator to type a command. Only
                // when the daemon NAMED the keyless row: absent means no greening, never no keys.
                if self.keys_row_present() && !self.choice_ready(&name) {
                    let provider = name.split('/').next().unwrap_or(&name).trim().to_string();
                    self.say(&format!(
                        "no key held for {provider} — paste it below; enter stores it (mode 600) \
                         and takes the row, esc cancels"
                    ));
                    self.key_ask = Some(KeyAsk {
                        choice: name,
                        provider,
                    });
                    self.key_buf.clear();
                    return None;
                }
                self.say(&format!("switching to {name}…"));
                // The switch, then a re-read of the rows it changed - in that order, which
                // the daemon honours, so the header names what answers now rather than what
                // answered a moment ago.
                self.queued.push(Action::Settings);
                // A cloud row taken because the local model could not be reached becomes the
                // default, and the message held for it follows the switch - `app::reach`.
                if !self.keyless_choices().iter().any(|k| k == &name) {
                    self.took_cloud_row(&name);
                }
                Some(Action::Slash {
                    line: format!("models {name}"),
                })
            }
            // The head's own two are local settings, so taking one is a write to this
            // head's config and not a frame - the same act `/verbosity NAME` and `/diff NAME`
            // perform, through the same function, so a card and a typed word cannot disagree.
            Some(Pick::Verbosity) => {
                self.pick = None;
                self.set_verbosity(&name)
            }
            Some(Pick::Diff) => {
                self.pick = None;
                self.set_diff(&name)
            }
            Some(Pick::Repo) => {
                self.pick = None;
                self.git_pinned = self
                    .git_repos
                    .iter()
                    .find(|r| repo_name(r) == name)
                    .cloned();
                self.git_read.1 = 0;
                self.refresh_git();
                match &self.git_pinned {
                    Some(r) => self.say(&format!("the header shows {}", repo_name(r))),
                    None => self.say("the header follows the work again"),
                }
                None
            }
            None => None,
        }
    }

    /// The mode this session runs under, as one of the daemon's own names - see [`named_choice`].
    /// It cannot be the first whitespace-separated word: `writes allowed` is one name, so that
    /// reading names no choice, and both readers of it fail together.
    pub(crate) fn mode_current(&self) -> String {
        let Some(r) = self.mode_row() else {
            return String::new();
        };
        match named_choice(&r.value, &r.choices) {
            Some(c) => c.to_string(),
            // No row to mark, and the value is returned as it stands: a value naming none of the
            // choices is a real state (a daemon listing fewer modes than it accepts), and the
            // honest render is no mark anywhere rather than one on the wrong row.
            None => r.value.clone(),
        }
    }

    /// Put the open picker's cursor on the row that answers now, and remember that nothing has
    /// touched it yet - see [`App::pick_unseeded`]. One function for the four cards, because
    /// `pick_current` reads a row's value through `named_choice`, so they all seed identically.
    pub(crate) fn seed_pick(&mut self) {
        let values = self.pick_values();
        let now = self.pick_current();
        self.mode_sel = values.iter().position(|(n, _)| *n == now).unwrap_or(0);
        self.pick_unseeded = true;
    }
}

/// Which setting a card is choosing: two are the daemon's (`Mode`, `Model`), two are this head's
/// own. They share one card because they are one KIND of act, and because a setting with more
/// than two values is chosen from a card - only a true toggle may cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    /// The mode this session runs under - the daemon's names.
    Mode,
    /// What answers this conversation - the daemon's models.
    Model,
    /// How much of the stream reaches the transcript.
    Verbosity,
    /// How a diff is laid out.
    Diff,
    /// Which repository the header follows, in a workspace of several - the operator: *"I
    /// guess we can make git thing clickable with a popup"*. Opened by a click on the git field
    /// or `/repo`; this head's own setting, and not kept across a restart.
    Repo,
}

impl Pick {
    /// What the card is asking, as its title.
    pub(crate) fn title(self) -> &'static str {
        match self {
            Pick::Mode => "the mode this session runs under",
            Pick::Model => "what answers this conversation",
            Pick::Verbosity => "how much reaches the transcript",
            Pick::Diff => "how a diff is drawn",
            Pick::Repo => "which repository the header follows",
        }
    }

    /// The settings row the daemon publishes the choices on, or `None` for a setting this
    /// head owns. The daemon's lists are read, never kept - the mistake the `mode` row's
    /// own comment records.
    pub(crate) fn row_key(self) -> Option<&'static str> {
        match self {
            Pick::Mode => Some("mode"),
            Pick::Model => Some("model"),
            Pick::Verbosity | Pick::Diff | Pick::Repo => None,
        }
    }

    /// What this setting can be, with what each value MEANS: `Terse`, `Normal`, `Loud` are not
    /// self-describing, and the reader is choosing what will be on their screen. The daemon's two
    /// have no sentences here - it renders its choices verbatim, and a gloss would be a guess.
    pub(crate) fn values(self) -> &'static [(&'static str, &'static str)] {
        match self {
            // The daemon's own names arrive at runtime; see `SettingPick::daemon_lists`.
            Pick::Mode | Pick::Model => &[],
            // The head's own two carry their sentences from [`Pick::values`]; the verbosity card is
            // the PROFILE TABLE itself, built in `App::pick_values` rather than listed here, so a
            // profile added to `Profile::ALL` appears there by construction.
            Pick::Verbosity => &[],
            Pick::Diff => DIFF_VALUES,
            // The workspace's own repositories, read when the card opens (`App::pick_values`).
            Pick::Repo => &[],
        }
    }

    /// The lines under the list: what taking a row DOES, which differs per subject and is the
    /// difference a reader is most likely to get wrong. A slice and not one line, because the
    /// model card has two facts and this file's rule is one fact per line, trimmed not wrapped.
    pub(crate) fn consequence(self) -> &'static [&'static str] {
        match self {
            Pick::Mode => &[
                "a mode change moves THIS session from its next call, and every later session \
                 in this project.",
            ],
            Pick::Model => &[
                "this conversation only, from the next turn; the transcript and the tools are \
                 untouched",
                "`/default-model NAME` is what new sessions start on · this is not that",
            ],
            // The one that surprises people: the
            // ladder is applied to the whole transcript at once, so a rung takes effect on
            // what is already drawn rather than on what comes next.
            Pick::Verbosity => &[
                "this applies to the WHOLE transcript, already drawn — switch back and the rows \
                 you had hidden are there again.",
            ],
            Pick::Diff => &[
                "every edit card, drawn and future — the excerpt is the same either way; only \
                 the layout changes.",
            ],
            Pick::Repo => &[
                "this head's header only — the session works wherever it works; following picks \
                 the repository of its newest edit or command.",
            ],
        }
    }
}

/// The repo card's first row: no pin - the header follows the newest edit or command.
pub(crate) const FOLLOW: &str = "follow the work";

/// A repository's name on the card: its folder's.
pub(crate) fn repo_name(p: &std::path::Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| p.display().to_string())
}

impl App {
    /// The repo card's rows: following the work, then each repository with its branch and
    /// marks as `git status` reads them - one reading per repository, all at once, when the card
    /// opens (`/repo`); kept in `App::repo_rows` while it is up.
    pub(crate) fn read_repo_rows(&self) -> Vec<(String, String)> {
        let ws = self.wiring.workspace.clone();
        let following = self
            .followed_repo(&ws)
            .map(|r| format!("now {}", repo_name(&r)))
            .unwrap_or_else(|| "nothing to follow yet".into());
        let mut rows = vec![(
            FOLLOW.to_string(),
            format!("the repository of the newest edit or command — {following}"),
        )];
        let readings: Vec<_> = self
            .git_repos
            .iter()
            .map(|r| {
                let r = r.clone();
                std::thread::spawn(move || {
                    crate::gitfield::read_state_at(&r).map(|s| crate::gitfield::git_text(&s))
                })
            })
            .collect();
        for (r, t) in self.git_repos.iter().zip(readings) {
            let state = t
                .join()
                .ok()
                .flatten()
                .unwrap_or_else(|| "not readable".into());
            rows.push((repo_name(r), state));
        }
        rows
    }
}

/// The model card's rows under what has been typed on it. The order within one rank is the
/// daemon's own - largest window first - because a filter decides WHICH rows, not which is best.
pub(crate) fn filter_models(rows: &[(String, String)], typed: &str) -> Vec<(String, String)> {
    let want = typed.trim().to_ascii_lowercase();
    if want.is_empty() {
        return rows.to_vec();
    }
    let mut hits: Vec<(u8, (String, String))> = rows
        .iter()
        .filter_map(|r| rank(&r.0, &want).map(|k| (k, r.clone())))
        .collect();
    hits.sort_by_key(|h| h.0);
    hits.into_iter().map(|(_, r)| r).collect()
}

/// How well one row's name answers what was typed, lowest first: the whole name, the model part
/// alone, a model part that begins with it, anywhere in it, and last its letters in order -
/// which is what makes `dflash` enough to reach `deepseek/deepseek-v4-flash`.
fn rank(name: &str, want: &str) -> Option<u8> {
    let lower = name.to_ascii_lowercase();
    let model = lower.rsplit('/').next().unwrap_or(&lower);
    if lower == want {
        return Some(0);
    }
    if model == want {
        return Some(1);
    }
    if model.starts_with(want) {
        return Some(2);
    }
    if lower.contains(want) {
        return Some(3);
    }
    let mut letters = lower.chars();
    want.chars().all(|c| letters.any(|x| x == c)).then_some(4)
}

/// What the diff style means. Unified against side-by-side is the setting; colour versus the
/// `+`/`-` marks is NOT a choice - the marks are drawn in both layouts always, because they carry
/// the meaning and colour only reinforces it. Colour is the terminal's (`RenderConfig::color`).
pub(crate) const DIFF_VALUES: &[(&str, &str)] = &[
    (
        "unified",
        "one column: `-` lines removed, `+` lines added, in order — best on a narrow terminal",
    ),
    (
        "split",
        "two columns: the old text left, the new right, lined up — best when there is width",
    ),
];
