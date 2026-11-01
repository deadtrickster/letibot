//! **The skills this box has** — `/skills`, one row each: the name, where it was found and its
//! one line. Enter hands that skill's body to the session.
//!
//! # Why the rows say where a skill came from
//!
//! A skill is material to follow only as far as it does not contradict the operator, and the
//! label is the fact a reader needs to weigh it: `the operator's` is their own instruction, and
//! `this project`, `installed from …`, `the fabric` and `written by a model` are not. The prompt's
//! index carries the same label for the same reason, and this pane draws the daemon's rows
//! unaltered so the two cannot come to different answers about one skill.
//!
//! # Enter, and why the head does not hold the body
//!
//! Enter sends `LoadSkill` (`Action::LoadSkill`): the DAEMON loads the body, writes the
//! operator's message and runs the turn. A head that fetched a body and composed a prompt would
//! be a second writer of a row, and a fabric skill's body is a read only the daemon may make.

use crate::app::*;
use rano::agent::pane::{self, PaneLines};
use rano::agent::text::clean_line;
use rano::render::{Line, Span};
use rano::style::Role;

impl App {
    /// The skills pane's rows, and the record of where each stop landed — which is what the
    /// arrows scroll by, never arithmetic over the list they drew from.
    pub(crate) fn skills_lines(&mut self, w: usize) -> Vec<String> {
        let (content, stops) = self.skills_view(w);
        self.skills_stop_rows = stops;
        crate::ui::render::row_strings(&content.lines, self.cfg.palette())
    }

    fn skills_view(&self, _w: usize) -> (PaneLines, Vec<usize>) {
        let mut out = PaneLines::new();
        let mut stops = Vec::new();
        let n = self.skills.len();
        out.push(pane::title(format!(
            "skills — {n} this box can hand you; enter gives one to the session"
        )));
        out.blank();
        if self.skills.is_empty() {
            out.push(pane::faint(
                "    none. A skill is a `SKILL.md` under `.letibot/skills/`, `.claude/skills/` or \
                 `~/.config/letibot/skills/`, a layer's — or one this build carries.",
            ));
            return (out, stops);
        }
        let cursor = self.skills_sel.min(n.saturating_sub(1));
        for (i, s) in self.skills.iter().enumerate() {
            let picked = i == cursor;
            let row = out.lines.len();
            out.push(Line::new(vec![
                Span::raw(format!("{} ", pane::mark(picked))),
                Span::raw(format!("{}  ", clean_line(&s.name))),
                Span::styled(format!("[{}]", clean_line(&s.source)), Role::Faint),
            ]));
            // The row is a line of its own that the cursor stops on; the description is the
            // faint line under it, the standing pane's shape.
            stops.push(row);
            if !s.description.trim().is_empty() {
                out.push(pane::faint(format!("    {}", clean_line(&s.description))));
            }
        }
        (out, stops)
    }

    /// **The pane row the cursor is on**, read off the record of where each row was DRAWN —
    /// the standing pane's rule, and for its reason: arithmetic over a list the pane may have
    /// wrapped differently is a scroll that lands somewhere else.
    pub(crate) fn skills_row_of(&self) -> usize {
        let at = self
            .skills_sel
            .min(self.skills_stop_rows.len().saturating_sub(1));
        self.skills_stop_rows.get(at).copied().unwrap_or(0)
    }

    /// **The skill row the cursor is on, by name** — what Enter hands over.
    pub(crate) fn selected_skill(&self) -> Option<String> {
        self.skills
            .get(self.skills_sel.min(self.skills.len().saturating_sub(1)))
            .map(|s| s.name.clone())
    }
}
