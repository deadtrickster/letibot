//! The pickers: sessions, and a setting's values - drawn by `rano::agent::picker` from
//! the daemon's session list and the setting's choices this head holds.

use crate::app::*;
use crate::ui::render::row_strings;
use rano::agent::picker::{SettingPicker, SettingValue};

/// How the setting card's keys are named on the hint bar: a row number MARKS the row and Enter
/// answers it, as on every numbered card. rano's own `HintMode::Pick` line promises the number
/// switches, and a bare digit did answer the card until this head stopped granting that.
pub(crate) const SETTING_BAR: &str =
    "a row number marks · ↑↓ then enter · or type a name · esc closes";

/// The bar under the model card: its line is a SEARCH FIELD on the card, so a letter narrows the
/// list and never starts a message, and its two levels have a key out of each.
pub(crate) const MODEL_BAR: &str =
    "type to narrow · ↑↓ moves · enter opens or takes · ← back · esc closes";

impl App {
    /// The picker's rows as one list - the tests' entry point, since the drawing path reads
    /// [`App::picker_layout`] for the row spans as well.
    #[cfg(test)]
    pub(crate) fn picker_lines(&self, w: usize) -> Vec<String> {
        // A stated height: the picture does not depend on it, and a test that also presses a key
        // leaves the same size behind for the keys to ask about.
        self.picker_layout(w, 40).0
    }

    /// The picker's rows, and the rows each session row `k` (an index into
    /// [`App::session_rows`], the order `picker_sel` counts in) was drawn on. rano draws two
    /// lines per session, so a click maps through its stop rows rather than counting rows.
    pub(crate) fn picker_layout(
        &self,
        w: usize,
        h: usize,
    ) -> (Vec<String>, Vec<std::ops::Range<usize>>) {
        // The list is the plugin's - the built-in `sessions` layer answers it and this head draws
        // the answer, one drawing. While the stack is being built the pane is one line about it.
        let content = self.picker_list(w, h).content;
        let stops = &content.stop_rows;
        let spans = stops
            .iter()
            .enumerate()
            .map(|(k, &start)| {
                let end = stops.get(k + 1).copied().unwrap_or_else(|| {
                    (start + 1..content.lines.len())
                        .find(|&r| content.lines[r] == rano::render::text::Line::default())
                        .unwrap_or(content.lines.len())
                });
                start..end
            })
            .collect();
        (row_strings(&content.lines, self.cfg.palette()), spans)
    }

    /// The setting card: one renderer for `/mode`, `/models`, `/verbosity` and `/diff`, because
    /// a second copy of a list drifts. `spans[k]` is read off rano's stop rows, since a value's
    /// `why` wraps under it and one-row-per-value arithmetic then clicks the wrong value.
    pub(crate) fn setting_picker_layout(
        &self,
        w: usize,
    ) -> (Vec<String>, Vec<std::ops::Range<usize>>) {
        // The model card is not a list of values: it has two levels, a search field of its own and
        // columns, so it draws itself. One renderer for the other four still, because a second copy
        // of a list drifts.
        if self.pick == Some(Pick::Model) {
            return self.model_card_layout(w);
        }
        let Some(subject) = self.pick else {
            return (Vec::new(), Vec::new());
        };
        let current = self.pick_current();
        let picker = SettingPicker {
            title: subject.title().to_string(),
            values: self
                .pick_values()
                .into_iter()
                .map(|(name, why)| SettingValue {
                    ready: true,
                    name,
                    why,
                })
                .collect(),
            current,
            selected: self.mode_sel,
            // The names are the daemon's verbatim, so a daemon that sent none gets one line
            // saying so, and the verb keeps working for an operator who knows the name.
            empty: "this daemon has not named its modes — `/mode NAME` still works, if you know \
                    the name."
                .to_string(),
            legend: None,
            // The model card's own line about the unreachable model is drawn by the card
            // (`ui::cards::model`); these are the four value cards' consequences.
            consequence: subject
                .consequence()
                .iter()
                .map(|l| l.to_string())
                .collect(),
        };
        // The keys line, the legend (none here) and the consequence, each one line - see
        // `SettingPicker::content`.
        let under = 1 + usize::from(picker.legend.is_some()) + picker.consequence.len();
        let content = picker.content(w);
        let stops = &content.stop_rows;
        let foot = content.lines.len().saturating_sub(under);
        let spans: Vec<std::ops::Range<usize>> = stops
            .iter()
            .enumerate()
            .map(|(k, &start)| start..stops.get(k + 1).copied().unwrap_or(foot).max(start + 1))
            .collect();
        (row_strings(&content.lines, self.cfg.palette()), spans)
    }
}
