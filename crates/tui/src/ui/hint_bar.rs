//! **The hint bar**: the keys that do something right now — drawn by `rano::agent::hint_bar`,
//! which keeps the sentences; this head says which one is true now.

use crate::app::*;
use rano::agent::hint_bar::{HintBar, HintMode};
use rano::render::Line;
use rano::style::Role;

impl App {
    /// The bottom bar: what the keys do, right now.
    ///
    /// The composer owns the first half and changes it after the first Esc or
    /// Ctrl+C — that is how anyone finds out a double-tap exists. The head owns
    /// the second half, which is its own keys.
    pub(crate) fn hint_bar(&self, w: usize) -> String {
        // The editor's own half is about the composer's double-taps. While the quit card is up
        // there is no double-tap left to learn — the card IS the second press — and the
        // editor's "ctrl+c again to exit" would name a key that now closes the card instead.
        // So the card's line stands alone (rano drops the editor's half for `QuitCard`).
        let editor = match self.editor.hint_text(self.now_ms) {
            Some(s) => Line::styled(s, Role::Attention),
            None => Line::default(),
        };
        // The order is the order of precedence: the first of these that is true is what the
        // keys do right now.
        let mode = if self.quit_card {
            HintMode::QuitCard
        } else if self.detached() {
            HintMode::Detached
        } else if self.editor_focused() {
            HintMode::Editor
        } else if self.editor_drawn() {
            HintMode::EditorBehind
        } else if self.help || self.stats {
            HintMode::Reading
        } else if self.picker {
            HintMode::SessionPicker
        } else if self.pick.is_some() {
            HintMode::Pick
        } else if self.todos_pane {
            HintMode::Todos
        } else if self.config_pane {
            HintMode::Config
        } else if self.subagents_pane {
            HintMode::Subagents
        } else if self.job_out.is_some() {
            HintMode::JobOutput
        } else if self.queue_open.is_some() {
            HintMode::QueueEntry
        } else if self.queue_pane {
            HintMode::Queue
        } else if self.jobs_pane {
            HintMode::Jobs
        } else if !self.open.is_empty() {
            HintMode::Deciding
        } else {
            HintMode::Conversation
        };
        // **The queue pane's person-verbs, in the half of the bar this head owns.** rano owns
        // `HintMode::Queue`'s tail sentence (the pane's own keys — enter, the arrows, esc — and
        // rano is pinned by tag), so `a`/`v`/`d`/`r` go in the `editor` half, which is the left half
        // of the SAME row and is empty while a pane has the keyboard. They lead there on purpose:
        // the bar is over capacity at 80 columns by construction, and the renderer keeps the head,
        // so the keys this pane adds are the ones that survive the cut.
        let editor = if self.queue_pane && self.queue_open.is_none() {
            Line::styled("a approve · v veto · d drop · r restart", Role::Faint)
        } else {
            editor
        };
        crate::ui::render::row(&HintBar { editor, mode }.line(w), self.cfg.palette())
    }
}
