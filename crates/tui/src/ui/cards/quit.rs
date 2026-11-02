//! **The quit card**: what leaving would stop, and the choices for it - drawn by
//! `rano::agent::quit` from the counts this head keeps.

use crate::app::*;
use crate::ui::render::row_strings;
use rano::agent::quit::QuitCard;

impl App {
    /// The quit card's rows: what Enter does, and the consequence of it. The consequence rides the
    /// row because it is the whole reason the card exists - the two choices are not alike, and a
    /// card that drew them alike would answer for the operator.
    #[cfg(test)]
    pub(crate) fn quit_choices(&self) -> [(&'static str, String); 2] {
        self.quit_view().choices()
    }

    /// The card's facts: the selection, and what stopping the daemon would end.
    pub(crate) fn quit_view(&self) -> QuitCard {
        QuitCard {
            selected: self.quit_sel,
            other_heads: self.heads.saturating_sub(1),
            running_jobs: self.jobs.iter().filter(|j| j.running).count(),
            running_subagents: self
                .subagents
                .iter()
                .filter(|s| s.state == "running")
                .count(),
        }
    }

    pub(crate) fn quit_card_lines(&self, w: usize) -> Vec<String> {
        row_strings(&self.quit_view().lines(w), self.cfg.palette())
    }
}
