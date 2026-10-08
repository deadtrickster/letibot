//! **The pickers**: sessions, and a setting's values — drawn by `rano::agent::picker` from
//! the daemon's session list and the setting's choices this head holds.

use crate::app::*;
use crate::ui::render::row_strings;
use crate::ui::*;
use letibot_sessionlog::registry::short_id;
use rano::agent::picker::{SessionPicker, SessionRow as PickRow, SettingPicker, SettingValue};

impl App {
    pub(crate) fn picker_lines(&self, w: usize) -> Vec<String> {
        let content = self.session_picker().content(w);
        row_strings(&content.lines, self.cfg.palette())
    }

    /// **The session list's facts, in rano's words** — through the ONE enumeration
    /// ([`App::session_rows`]): a sub-session indented under the conversation that spawned it,
    /// a conversation with children carrying the fold every tree in this head uses (`-` open,
    /// `+` folded), and the numbering the ROW's, so the number a reader counts to is the number
    /// `/switch N` takes. The rest — the reserved fold column, the number field as wide as the
    /// list is long, the facts at the right edge — is rano's, with the measurements that
    /// decided it.
    pub(crate) fn session_picker(&self) -> SessionPicker {
        let rows = self.session_rows();
        let kids_of = |id: &str| {
            self.sessions
                .iter()
                .filter(|s| s.parent_session_id.as_deref() == Some(id))
                .count()
        };
        SessionPicker {
            rows: rows
                .iter()
                .map(|row| {
                    let s = &self.sessions[row.idx];
                    let children = (kids_of(&s.session_id) > 0).then(|| {
                        self.family_open(&s.session_id, row.depth)
                            || self.expanded.iter().any(|e| *e == s.session_id)
                    });
                    let stored = s.stored_items as usize;
                    PickRow {
                        depth: row.depth,
                        name: if s.title.is_empty() {
                            short_id(&s.session_id)
                        } else {
                            s.title.clone()
                        },
                        session_id: s.session_id.clone(),
                        here: s.session_id == self.session_id,
                        children,
                        generating: s.status.running,
                        live: s.live,
                        rows: if stored > 0 { stored } else { s.status.items },
                        heads: s.status.heads,
                        model: s.wiring.model.clone(),
                        workspace: if s.wiring.workspace.is_empty() {
                            String::new()
                        } else {
                            tilde(&s.wiring.workspace)
                        },
                    }
                })
                .collect(),
            selected: self.picker_sel,
        }
    }

    /// **The setting card** — one renderer for every setting a reader chooses from (R38).
    ///
    /// `/mode`, `/models`, `/verbosity` and `/diff` are four questions of one kind, so they get
    /// one card in one place. That is not tidiness: this file has been burned twice by a second
    /// copy of a list that then drifted (the `mode` settings row it used to keep, and the
    /// completion table R32 found), and a third card would be a third thing to keep in step.
    ///
    /// # What R38 requires of it, and where each one is
    ///
    /// * **Every value, the current one marked.** The marker is `← now` in the faint register
    ///   and the value itself is bold, which is the split the session picker draws between its
    ///   bold row and its inverse one: *where am I* and *what does Enter take* stay two
    ///   readable facts even when they are different rows.
    /// * **What each value MEANS.** For the head's own two settings the sentence is on the row
    ///   (see [`Pick::values`]); a daemon's setting carries none, because the head does not
    ///   know what `automode-edits` means and a gloss it invented would be the other half's
    ///   documentation written wrongly.
    /// * **It takes effect on what is ALREADY DRAWN**, which for the ladder is the surprising
    ///   half and is why [`Pick::consequence`] says so under the list.
    /// * **`esc` is a real answer** — it closes the card and leaves the setting alone; the
    ///   silence is the answer, and the arm that handles it says so.
    ///
    /// The shape is the mode card's, deliberately: title, one row per value with its number on
    /// the left, `← now` on the right of the current one, then the keys, then the consequence.
    /// The click arithmetic in [`App::screen`] counts on this: the title is one row and the
    /// first value is the next one.
    pub(crate) fn setting_picker_lines(&self, w: usize) -> Vec<String> {
        let Some(subject) = self.pick else {
            return Vec::new();
        };
        let current = self.pick_current();
        let picker = SettingPicker {
            title: subject.title().to_string(),
            values: self
                .pick_values()
                .into_iter()
                .map(|(name, why)| SettingValue {
                    ready: subject == Pick::Model && self.choice_ready(&name),
                    name,
                    why,
                })
                .collect(),
            current,
            selected: self.mode_sel,
            // The names are the daemon's verbatim, so a daemon that sent none gets one line
            // saying so, and the verb keeps working for an operator who knows the name.
            empty: match subject.row_key() {
                Some("model") => {
                    "this daemon has not named its models — `/models PROVIDER/MODEL` \
                     still works, if you know the name."
                }
                _ => {
                    "this daemon has not named its modes — `/mode NAME` still works, if \
                     you know the name."
                }
            }
            .to_string(),
            legend: (subject == Pick::Model).then(|| {
                "green: this box holds a key for it; the others ask for one when you take them"
                    .to_string()
            }),
            consequence: subject
                .consequence()
                .iter()
                .map(|l| l.to_string())
                .collect(),
        };
        row_strings(&picker.content(w).lines, self.cfg.palette())
    }
}
