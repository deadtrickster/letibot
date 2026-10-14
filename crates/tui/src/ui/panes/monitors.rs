//! **The monitors pane**: what this session is watching across turns — drawn by
//! `rano::agent::monitors` from the monitor table the daemon sends with its jobs.

use crate::app::*;
use crate::ui::render::row_strings;
use rano::agent::monitors::{MonitorRow, MonitorsPane};

impl App {
    /// **The monitors pane's facts, in rano's words**: the daemon's table and the cursor, and the
    /// clock the time left is counted against.
    pub(crate) fn monitors_view(&self) -> MonitorsPane {
        MonitorsPane {
            monitors: self
                .monitors
                .iter()
                .map(|m| MonitorRow {
                    name: m.name.clone(),
                    watching: m.watching.clone(),
                    owner: m.owner.clone(),
                    declared_by: m.declared_by.clone(),
                    repeat: m.repeat,
                    live: m.live,
                    ends_at_ms: m.ends_at_ms,
                    fired: m.fired,
                    ended: m.ended.clone(),
                })
                .collect(),
            now_ms: letibot_sessionlog::registry::now_ms(),
            selected: self.monitors_sel,
        }
    }

    /// The live monitors the cursor walks — indices into [`App::monitors`].
    pub(crate) fn monitor_stops(&self) -> Vec<usize> {
        self.monitors_view().stops()
    }

    /// The pane row the monitor at the cursor was drawn on. See [`App::jobs_row_of`].
    pub(crate) fn monitors_row_of(&self) -> usize {
        let at = self
            .monitors_sel
            .min(self.monitors_stop_rows.len().saturating_sub(1));
        self.monitors_stop_rows.get(at).copied().unwrap_or(0)
    }

    /// The pane's rows, recording where each stop landed (the arrows and a click read it).
    pub(crate) fn monitors_lines(&mut self, w: usize) -> Vec<String> {
        let content = self.monitors_view().content(w);
        self.monitors_stop_rows = content.stop_rows;
        row_strings(&content.lines, self.cfg.palette())
    }

    /// **`x`: retire the monitor under the cursor** — `/monitor retire NAME`, the daemon's verb,
    /// so the record says the operator retired it and the model's next notice says so.
    pub(crate) fn retire_selected_monitor(&mut self) -> Option<Action> {
        let name = self.monitors_view().selected_monitor()?.name.clone();
        if self.session_id.is_empty() {
            return None;
        }
        self.say(&format!("retiring monitor `{name}`"));
        self.redraw = true;
        Some(Action::Slash {
            line: format!("monitor retire {name}"),
        })
    }
}
