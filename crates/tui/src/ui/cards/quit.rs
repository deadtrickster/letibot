//! **The quit card**: what leaving would stop, and the choices for it — drawn by
//! `rano::agent::quit` from the counts this head keeps.

use crate::app::*;
use crate::ui::render::row_strings;
use rano::agent::quit::QuitCard;

impl App {
    /// The mode card: the daemon's own mode names, in the ask card's slot at
    /// the bottom of the screen — the transcript stays visible above it, the
    /// way approvals sit, instead of the card taking the whole body the way
    /// the session picker does.
    ///
    /// The names are `SettingRow::choices` verbatim — the head keeps no list
    /// of its own, because a second copy of a list is a copy that drifts. A
    /// daemon that sent none gets one dim line saying so, and `/mode NAME`
    /// keeps working for an operator who knows the name anyway.
    ///
    /// The card's shape is load-bearing: the first line is the title and the
    /// second is the first choice, because the click arithmetic in `screen`
    /// counts on it. No blank between them.
    /// The quit card's rows: what Enter does, and the consequence of it.
    ///
    /// The consequence is on the row rather than in a footnote because it is
    /// the whole reason the card exists — one of these two is cheap and the
    /// other is not, and a card that made them look alike would be a card that
    /// answered for the operator.
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
