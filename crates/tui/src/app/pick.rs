//! **The pickers' state**: which picker is open, its rows and the selection, and the mode a
//! pick asks for.

use super::*;
use letibot_sessionlog::registry::SessionBrief;

impl App {
    /// Answer the session picker: a row number, or enough of an id to be unique.
    ///
    /// An ambiguous prefix is **refused with the count**, not resolved to the first
    /// match. Switching to the wrong session is not a keystroke you can take back —
    /// the prompt you type next lands there.
    pub(crate) fn pick(&mut self, typed: &str) -> Option<Action> {
        if typed.is_empty() {
            self.picker = false;
            self.redraw = true;
            return None;
        }
        // **The number is the row the picker DREW.** They are the same list only while nothing is
        // expanded, and a number that means one thing on the screen and another in this function is
        // the two-enumerations defect. The prefix search below stays over every session on purpose:
        // a collapsed child is still reachable by name, which is what collapsing is for.
        let rows = self.session_rows();
        if let Ok(n) = typed.parse::<usize>()
            && n >= 1
            && n <= rows.len()
        {
            let id = self.sessions[rows[n - 1].idx].session_id.clone();
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
                self.say(&format!(
                    "no session matches {typed:?} — esc closes the list"
                ));
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

    /// Take a submitted line while the mode picker is up: a row number, or a
    /// mode name — exact, or a prefix only one mode shares.
    ///
    /// The normalization is `Mode::parse`'s own, read-only side: case folds,
    /// and `_` and a space fold to `-`, so `automode_edits` and `Automode
    /// Edits` reach the mode the daemon spells `automode-edits`. An exact
    /// match wins before prefixes are counted, so `automode` reaches
    /// `automode` even though `automode-edits` also starts with it.
    pub(crate) fn pick_mode(&mut self, typed: &str) -> Option<Action> {
        if typed.is_empty() {
            self.pick = None;
            self.redraw = true;
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

    /// Leave the mode picker for the mode the operator chose. The mode the
    /// session already runs under closes the list and says so, the way the
    /// session picker answers Enter on its own row — a round trip to the
    /// daemon to be told what the screen already showed is not worth its
    /// flicker.
    pub(crate) fn take_mode(&mut self, name: String) -> Option<Action> {
        self.pick = None;
        self.redraw = true;
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

    /// The mode names, as the daemon spelled them. Empty when it sent none —
    /// the head keeps no list of its own to fall back on, because a second
    /// copy of a list is a copy that drifts.
    pub(crate) fn mode_choices(&self) -> Vec<String> {
        self.mode_row()
            .map(|r| r.choices.clone())
            .unwrap_or_default()
    }

    /// **The settings row the open card is choosing from**, or `None` for a setting this
    /// head owns: `Verbosity` and the diff style are the head's own and have no daemon row.
    ///
    /// One function for the four, because the choice of row is the only thing that differs
    /// between a daemon's setting and the head's — see [`Pick::row_key`].
    pub(crate) fn pick_row(&self) -> Option<&letibot_sessionlog::protocol::SettingRow> {
        let key = self.pick?.row_key()?;
        self.settings.iter().find(|r| r.key == key)
    }

    /// **What the open card offers, as values with meanings** (R38).
    ///
    /// Two sources, and the difference is where the knowledge lives. A daemon's setting is
    /// read from its own `SettingRow` — the head keeping its own copy of a list is the
    /// mistake the `mode` row's comment records — and the values carry **no sentence**,
    /// because the head does not know what `automode-edits` means and inventing a gloss would
    /// be writing the other half's documentation. The head's own two settings carry the
    /// sentences from [`Pick::values`].
    pub(crate) fn pick_values(&self) -> Vec<(String, String)> {
        let Some(subject) = self.pick else {
            return Vec::new();
        };
        // **The verbosity card is the PROFILE TABLE, and not a second list built beside it.**
        //
        // The rows used to be a hand-written const (`VERBOSITY_VALUES`) and the copy drifted the way
        // a copy does: it was missing `read-edits` — the rung that is *conversation plus the edit
        // cards* — so a rung `/v` cycles onto had no row on the card, and the profile the operator
        // asked for by name did not exist as far as the card was concerned. One table, so a profile
        // added to `Profile::ALL` appears here by construction.
        if subject == Pick::Verbosity {
            let mut rows: Vec<(String, String)> = Profile::ALL
                .iter()
                .map(|p| (p.name.to_string(), p.why.to_string()))
                .collect();
            // **And the set in force, when no profile is it.** The rows above are the table; the set
            // is runtime state, so a set off the ladder had no row at all — the operator, having
            // typed one: *"it is not saved - when i do /verbosity there is no custom"*. The row is
            // named by the set's own `custom …` string, which `as_str` writes so that it can be
            // typed back; and since the row IS that string, it is also the row the marker lands
            // on — `pick_current` reads the same one.
            if self.visibility.profile().is_none()
                && !rows.iter().any(|(v, _)| v == &self.visibility.as_str())
            {
                rows.push((
                    self.visibility.as_str(),
                    "the set in force — no profile names it, and Enter on this row keeps it"
                        .to_string(),
                ));
            }
            return rows;
        }
        if !subject.values().is_empty() {
            return subject
                .values()
                .iter()
                .map(|(v, why)| ((*v).to_string(), (*why).to_string()))
                .collect();
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

    /// What the open card is already on.
    ///
    /// **The profile when the set is one, and the set's own `custom …` name when it is not** — and
    /// since `pick_values` now draws a row under exactly that name, a custom set is marked on the
    /// card like any other state rather than matching nothing. (It used to match no row at all,
    /// which read as the card having lost the setting; see that function.)
    pub(crate) fn pick_current(&self) -> String {
        match self.pick {
            Some(Pick::Verbosity) => self.visibility.as_str(),
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

    /// **Commit the highlighted row.** The four subjects differ only here.
    pub(crate) fn take_pick(&mut self, name: String) -> Option<Action> {
        match self.pick {
            // The daemon's two: a mode is a protocol command this head already has, a model
            // is a daemon verb.
            Some(Pick::Mode) => return self.take_mode(name),
            Some(Pick::Model) => {
                self.pick = None;
                self.redraw = true;
                if self.session_id.is_empty() {
                    self.say("not attached to a session yet");
                    return None;
                }
                // **A row this box holds no key behind ASKS rather than switching.**
                //
                // The operator's row: *"if i choose a model without key picker should ask for the key."*
                // Left as it was, the switch went to the daemon and came back as its refusal
                // (`NO KEY — /models … --key PASTE`) — the picker telling the operator to type a
                // command it could have collected on the spot.
                //
                // **Only when the daemon named the keyless row** (`models.keys` present): an
                // absent row is *no greening*, never *no keys*, and asking on its absence would
                // block every switch behind a prompt for a key this box may well hold. `local`
                // never asks — it needs no credential, and `choice_ready` says so.
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
                // The switch, then a re-read of the rows it changed — in that order, which
                // the daemon honours, so the header names what answers now rather than what
                // answered a moment ago.
                self.queued.push(Action::Settings);
                return Some(Action::Slash {
                    line: format!("models {name}"),
                });
            }
            // **The head's own two are local settings**, so taking one is a write to this
            // head's config and not a frame — the same act `/verbosity NAME` and `/diff NAME`
            // perform, through the same function, so a card and a typed word cannot disagree.
            Some(Pick::Verbosity) => {
                self.pick = None;
                return self.set_verbosity(&name);
            }
            Some(Pick::Diff) => {
                self.pick = None;
                return self.set_diff(&name);
            }
            None => None,
        }
    }

    /// The mode this session runs under, **as one of the daemon's own names** — see
    /// [`named_choice`], which is the whole of the reading.
    ///
    /// It used to be `value.split_whitespace().next()`, and that is wrong for a name that
    /// contains a space: `Mode::WRITES_ALLOWED` is spelled `writes allowed`, so the first word is
    /// `writes`, which names no choice. Both things that read this — the picker's cursor and the
    /// card's `← now` — then failed together, which is the operator's report exactly: *"permission
    /// mode menu no longer highlights the current mode when opened"*.
    pub(crate) fn mode_current(&self) -> String {
        let Some(r) = self.mode_row() else {
            return String::new();
        };
        match named_choice(&r.value, &r.choices) {
            Some(c) => c.to_string(),
            // **No row to mark, and the value is returned as it stands.** A current value that
            // names none of the choices is a real state — a daemon that lists fewer modes than it
            // accepts — and the honest render is no `← now` anywhere rather than one on the wrong
            // row. Returning the first word here is what made that case indistinguishable from a
            // name the head had failed to read.
            None => r.value.clone(),
        }
    }

    /// **Put the open picker's cursor on the row that answers now**, and remember that nothing
    /// has touched it yet — see [`App::pick_unseeded`].
    ///
    /// One function for the four cards, because they seed identically now that
    /// [`App::pick_current`] reads a row's value through [`named_choice`]: the mode, the model,
    /// the rung and the diff style all pick whichever of their values the current one names.
    /// They used to seed at four call sites with two spellings of the same rule, which is the
    /// shape this file keeps deleting.
    pub(crate) fn seed_pick(&mut self) {
        let values = self.pick_values();
        let now = self.pick_current();
        self.mode_sel = values.iter().position(|(n, _)| *n == now).unwrap_or(0);
        self.pick_unseeded = true;
    }
}

/// **Which setting a card is choosing** — R38.
///
/// Two of these are the daemon's (`Mode` from a `SettingRow`'s `choices`, `Model` from the
/// catalogue); two are this head's own (`Verbosity`, the diff style). They share one card
/// because they are one KIND of act — the reader is choosing between named values and can see
/// all of them — and one card is what keeps them from becoming three vocabularies.
///
/// **`Verbosity::Terse` and its neighbours are the reason this exists.** `/verbosity` used to
/// cycle, which requires the reader to hold four rungs in their head and to find the current
/// one by changing it: three presses and three repaints for the value they wanted, and no
/// screen anywhere saying what the four were. R38's rule: **a setting with more than two
/// values is chosen from a card; only a true toggle may cycle.**
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    /// The mode this session runs under — the daemon's names.
    Mode,
    /// What answers this conversation — the daemon's models.
    Model,
    /// **How much of the stream reaches the transcript** — R37's ladder.
    Verbosity,
    /// **How a diff is laid out** — R38's new setting.
    Diff,
}

impl Pick {
    /// What the card is asking, as its title.
    pub(crate) fn title(self) -> &'static str {
        match self {
            Pick::Mode => "the mode this session runs under",
            Pick::Model => "what answers this conversation",
            Pick::Verbosity => "how much reaches the transcript",
            Pick::Diff => "how a diff is drawn",
        }
    }

    /// The settings row the daemon publishes the choices on, or `None` for a setting this
    /// head owns. **The daemon's lists are read, never kept** — the mistake the `mode` row's
    /// own comment records.
    pub(crate) fn row_key(self) -> Option<&'static str> {
        match self {
            Pick::Mode => Some("mode"),
            Pick::Model => Some("model"),
            Pick::Verbosity | Pick::Diff => None,
        }
    }

    /// What this setting can be, with what each value MEANS.
    ///
    /// **Every value carries its meaning on the card and not only its name** (R38). `Terse`,
    /// `Normal`, `Loud` and `Conversation` are not self-describing, and a reader choosing
    /// between them is choosing between *what will be on my screen*, which the card can state
    /// and the name cannot. The daemon's two settings have no sentences here because the head
    /// does not know what they mean — it renders the daemon's choices verbatim and says
    /// nothing more, which is the same rule as its hints.
    pub(crate) fn values(self) -> &'static [(&'static str, &'static str)] {
        match self {
            // The daemon's own names arrive at runtime; see `SettingPick::daemon_lists`.
            Pick::Mode | Pick::Model => &[],
            // The head's own two carry their sentences from [`Pick::values`] — and the verbosity
            // card is the PROFILE TABLE itself, built in `App::pick_values` rather than listed
            // here. It used to be a hand-written list beside this one, and the two drifted: the
            // copy was missing `read-edits` entirely, so a rung `/v` cycles onto had no row.
            Pick::Verbosity => &[],
            Pick::Diff => DIFF_VALUES,
        }
    }

    /// The lines under the list: what taking a row DOES, which differs per subject and is the
    /// difference a reader is most likely to get wrong.
    ///
    /// **A slice and not one line**, because the model card has two facts and the file's own
    /// rule is one fact per line: these are trimmed rather than wrapped, and measured at 110
    /// columns a two-fact version read *"It also become…"* with its useful half never reaching
    /// the screen. The second line is the verb for the OTHER thing, which the operator went
    /// looking for — one verb doing both is what sent them.
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
            // **The one that surprises people**, and R38 asks for it in as many words: the
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
        }
    }
}

/// **What the diff style means** — named here so both heads spell one setting one way (R38,
/// §11.6).
///
/// # The two axes, and the one that is NOT a value
///
/// The operator named two: *unified against side-by-side*, and *whether colour or the `+`/`-`
/// marks carry the meaning*. **The first is the setting. The second is not a choice and this
/// is the ruling:** the marks are drawn in BOTH layouts, always, because they are the diff's
/// meaning and colour is reinforcement of it. A value that removed the marks would be a value
/// that makes the diff unreadable on exactly the terminals the operator is worried about — a
/// pipe, a `--replay`, a light theme, a reader who cannot tell red from green — and R20's own
/// argument is that an appearance which collapses in half the terminals it is read in is not
/// an appearance at all. So there is no `marks: off`, and no `colour` value either: colour is
/// a property of the terminal, which the head already knows about (`RenderConfig::color`),
/// not a preference to be stored.
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
