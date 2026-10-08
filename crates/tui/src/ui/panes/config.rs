//! **The config pane**: the head's own settings and the session's — drawn by
//! `rano::agent::config` from the rows this head assembles.

use crate::app::*;
use crate::ui::render::row_strings;
use rano::agent::config::{ConfigPane, ConfigRow};

impl App {
    pub(crate) fn config_lines(&self, w: usize) -> Vec<String> {
        let pane = ConfigPane {
            rows: self
                .config_rows()
                .into_iter()
                .map(|r| ConfigRow {
                    section: r.section.to_string(),
                    editable: !matches!(r.edit, ConfigEdit::No(_)),
                    key: r.key,
                    value: r.value,
                    source: r.source,
                })
                .collect(),
            selected: self.config_sel,
            session_note: self.settings.is_empty().then(|| {
                if self.session_id.is_empty() {
                    "session — not attached, so nothing to list"
                } else {
                    "session — asked the daemon; nothing back yet"
                }
                .to_string()
            }),
        };
        row_strings(&pane.lines(w), self.cfg.palette())
    }
}
