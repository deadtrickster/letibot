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
        } else if self.diff_popup.is_some() && !self.open.is_empty() {
            // **A decision under the diff popup has the card's keys** (`App::key_diff_popup`
            // yields them), so the bar says what the card's keys do; the popup's own keys are
            // named in this head's half below.
            HintMode::Deciding
        } else if self.diff_popup.is_some() {
            // **The popup is a reading screen with no mode of its own in rano**: `Reading`'s
            // tail (`esc closes this`) is true of it, and its keys go in this head's half. It
            // used to fall through to whatever was under it — the conversation's chords, all
            // dead while the popup owns the keyboard.
            HintMode::Reading
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
        } else if self.todos_open.is_some() {
            // **The row's overlay is a READING screen**, like the standing pane's: it has no field on
            // it and its keys are the arrows and the page keys. rano keeps the bar's tail sentences
            // and is pinned by tag, so `Reading`'s tail (`esc closes this`) is taken and this head's
            // own half says what the keys do — exactly as the pane's own branch below does.
            HintMode::Reading
        } else if self.todos_pane {
            // **The todos pane's keys, composed HERE.** rano keeps the bar's tail sentences and is
            // pinned by tag, and its `HintMode::Todos` sentence says *enter or tab unfolds* — true of
            // rano's pane, false of this one, where those two keys open the card on the add row,
            // read a row whole (enter) and act on it (tab). So this pane takes `Reading`'s tail (`esc
            // closes this`, which is true of it) and its own keys go in the half this head owns,
            // exactly as the queue pane's verbs and the standing-notes pane do below.
            HintMode::Reading
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
        } else if self.standing_pane {
            // **`Reading` and not a mode of its own.** rano keeps the bar's tail sentences and
            // is pinned by tag, so a new pane has no `HintMode` to take; the head's half of the
            // bar is where this pane's own keys go, exactly as the queue pane's verbs do a few
            // lines down. `Reading`'s tail — `esc closes this` — is true of the list, and the
            // keys that matter are named on the left.
            HintMode::Reading
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
        let editor = if self.diff_popup.is_some() && !self.open.is_empty() {
            Line::styled("the diff: pgup/pgdn scroll · esc closes it", Role::Faint)
        } else if self.diff_popup.is_some() {
            Line::styled(
                "↑↓ pgup/pgdn scroll · ctrl-] opens it in the editor",
                Role::Faint,
            )
        } else if self.todos_open.is_some() {
            Line::styled(self.todos_open_hint(), Role::Faint)
        } else if self.todos_pane {
            Line::styled(self.todos_hint(), Role::Faint)
        } else if self.queue_pane && self.queue_open.is_none() {
            Line::styled("a approve · v veto · d drop · r restart", Role::Faint)
        } else if self.standing_pane && !self.editor_drawn() {
            // **Where Enter goes, said on the bar.** The key opens the note in rano, which is a
            // different place from this list, and a reader who pressed it looking for an
            // overlay of this head's would otherwise be surprised by the whole screen changing.
            //
            // **And not while rano is drawn over the list**, which is the state Enter leaves
            // behind: these keys are off then (`App::key_standing_pane`), and the bar must not
            // name keys that do nothing — the editor's own half is what the left of the row is
            // for then.
            Line::styled("↑↓ moves · enter opens the note in the editor", Role::Faint)
        } else {
            editor
        };
        crate::ui::render::row(&HintBar { editor, mode }.line(w), self.cfg.palette())
    }
}
