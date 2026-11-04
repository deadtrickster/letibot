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

impl App {
    /// The picker's rows as one list - the tests' entry point, since the drawing path reads
    /// [`App::picker_layout`] for the row spans as well.
    #[cfg(test)]
    pub(crate) fn picker_lines(&self, w: usize) -> Vec<String> {
        self.picker_layout(w).0
    }

    /// The picker's rows, and the rows each session row `k` (an index into
    /// [`App::session_rows`], the order `picker_sel` counts in) was drawn on. rano draws two
    /// lines per session, so a click maps through its stop rows rather than counting rows.
    pub(crate) fn picker_layout(&self, w: usize) -> (Vec<String>, Vec<std::ops::Range<usize>>) {
        // The list is the plugin's when there is one (`app::sessions_pane`, the built-in
        // `sessions` layer); the head's own tree is the way out, and it draws through the same
        // primitive with the same frame - one door, `App::picker_list`.
        let content = self.picker_list(w).content;
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
            // Why the card is up, first, when the head opened it - the model a message was
            // about to go to could not be reached (`app::reach`). Under the list with the other
            // lines, not in the title: the click arithmetic counts the title as one row.
            consequence: self
                .reach
                .reason
                .iter()
                .filter(|_| subject == Pick::Model)
                .cloned()
                .chain(subject.consequence().iter().map(|l| l.to_string()))
                .collect(),
        };
        // The keys line, then the legend and the consequence, each one line - see
        // `SettingPicker::content`.
        let under = 1 + usize::from(picker.legend.is_some()) + picker.consequence.len();
        let content = picker.content(w);
        let stops = &content.stop_rows;
        let foot = content.lines.len().saturating_sub(under);
        let spans = stops
            .iter()
            .enumerate()
            .map(|(k, &start)| start..stops.get(k + 1).copied().unwrap_or(foot).max(start + 1))
            .collect();
        (row_strings(&content.lines, self.cfg.palette()), spans)
    }
}
