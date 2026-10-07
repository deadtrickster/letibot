//! **The config pane**: the head's own settings and the session's.

use crate::app::*;
use crate::ui::render::{sgr, trim_to};
use crate::ui::*;

impl App {
    pub(crate) fn config_lines(&self, w: usize) -> Vec<String> {
        let rows = self.config_rows();
        let mut out = vec![colour(&self.cfg, sgr::BOLD, "config")];
        out.push(String::new());
        let keyw = rows
            .iter()
            .map(|r| r.key.chars().count())
            .max()
            .unwrap_or(8)
            .min(28);
        let mut section = "";
        let sel = self.config_sel.min(rows.len().saturating_sub(1));
        for (i, r) in rows.iter().enumerate() {
            if r.section != section {
                if !section.is_empty() {
                    out.push(String::new());
                }
                out.push(dim(&self.cfg, &format!("  {}", r.section)));
                section = r.section;
            }
            let mark = match &r.edit {
                ConfigEdit::Head(_) | ConfigEdit::Session(..) => "✎",
                ConfigEdit::No(_) => " ",
            };
            let line = format!(
                "{} {mark} {:<keyw$}  {}",
                if i == sel { "▸" } else { " " },
                r.key,
                r.value
            );
            let line = trim_to(&line, w.saturating_sub(2));
            out.push(if i == sel {
                colour(&self.cfg, sgr::REVERSE, &line)
            } else {
                line
            });
            if i == sel && !r.source.is_empty() {
                out.push(dim(&self.cfg, &format!("       from {}", r.source)));
            }
        }
        if self.settings.is_empty() {
            out.push(String::new());
            out.push(dim(
                &self.cfg,
                if self.session_id.is_empty() {
                    "  session — not attached, so nothing to list"
                } else {
                    "  session — asked the daemon; nothing back yet"
                },
            ));
        }
        out.push(String::new());
        out.push(dim(
            &self.cfg,
            "    ✎ changes now and is kept (head → head.toml, mode → project store); \
             the rest shows its source and takes a restart",
        ));
        out
    }
}
