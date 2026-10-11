//! **The config pane**: the head's own settings and the session's — drawn by
//! `rano::agent::config` from the rows this head assembles.

use crate::app::*;
use crate::ui::render::row_strings;
use rano::agent::config::{ConfigPane, ConfigRow};

impl App {
    pub(crate) fn config_lines(&self, w: usize) -> Vec<String> {
        row_strings(&self.config_pane().lines(w), self.cfg.palette())
    }

    /// **The pane as rano draws it** — the rows this head assembles, the cursor, and the
    /// session's sentence while it has nothing to list. The lines and the click map's stop
    /// rows are both read from this one value.
    pub(crate) fn config_pane(&self) -> ConfigPane {
        ConfigPane {
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
        }
    }
}
