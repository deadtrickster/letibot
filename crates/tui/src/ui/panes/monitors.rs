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

    /// **Enter: the monitor under the cursor, whole** — the operator: *"i want to see monitor
    /// command in the details"*. What it watches and its facts in full, and the `monitor` call
    /// that armed it, as the model made it, found in the conversation by its name. Opened over the
    /// pane; Esc comes back to it.
    pub(crate) fn open_monitor_details(&mut self) -> bool {
        let Some(m) = self.monitors_view().selected_monitor().cloned() else {
            return false;
        };
        let now = letibot_sessionlog::registry::now_ms();
        let mut lines = vec![
            format!("monitor `{}`", m.name),
            String::new(),
            format!("  watching   {}", m.watching),
            format!(
                "  {}   {}",
                if m.repeat { "repeats " } else { "once    " },
                if m.fired == 0 {
                    "has not fired yet".to_string()
                } else {
                    format!("fired {}×", m.fired)
                }
            ),
        ];
        if m.ends_at_ms > 0 {
            lines.push(format!(
                "  time left  {}",
                rano::agent::text::duration(m.ends_at_ms.saturating_sub(now))
            ));
        }
        if !m.declared_by.is_empty() {
            lines.push(format!("  armed by   {}", m.declared_by));
        }
        if !m.owner.is_empty() {
            lines.push(format!("  ends with  its {}", m.owner));
        }
        lines.push(String::new());
        match self.monitor_call(&m.name) {
            Some(args) => {
                lines.push("the call that armed it:".into());
                lines.push(String::new());
                lines.push("  monitor".into());
                lines.extend(args.lines().map(|l| format!("  {l}")));
            }
            None => lines.push(
                "the call that armed it is not in the part of the conversation this head holds"
                    .into(),
            ),
        }
        self.slash_out = Some((format!("monitor {}", m.name), lines));
        self.push_surface(Surface::Listing);
        true
    }

    /// **The arguments of the newest `monitor` call that armed `name`**, pretty-printed — the
    /// conversation's own record of what the model asked for. A `renew` or `retire` of the same
    /// name is not the arming call and is passed over.
    fn monitor_call(&self, name: &str) -> Option<String> {
        self.items.iter().rev().find_map(|it| {
            let Some(letibot_transcript::TranscriptItem::Assistant { tool_calls, .. }) = &it.item
            else {
                return None;
            };
            tool_calls.iter().rev().find_map(|c| {
                if c.name != "monitor" {
                    return None;
                }
                let v: serde_json::Value = serde_json::from_str(&c.arguments).ok()?;
                let arms = v.get("name").and_then(|n| n.as_str()) == Some(name)
                    && v.get("action")
                        .and_then(|a| a.as_str())
                        .is_none_or(|a| a == "watch");
                arms.then(|| serde_json::to_string_pretty(&v).unwrap_or(c.arguments.clone()))
            })
        })
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
