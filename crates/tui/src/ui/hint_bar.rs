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
        // keys do right now. **A surface rano has no mode for says its keys in this head's half
        // (`own`), set in the SAME branch that picks the mode** — so the two halves of the bar
        // cannot come from two different surfaces.
        let mut own: Option<&str> = None;
        // **The surface in front is `drawn()`'s answer** (R69), the value `main_area` draws and
        // `App::key` routes on — so the bar cannot describe a pane that is open behind another.
        let drawn = self.drawn();
        let surface = self.focused_surface();
        let mode = if self.quit_card {
            HintMode::QuitCard
        } else if self.detached() {
            HintMode::Detached
        } else if drawn == Drawn::DiffPopup && !self.open.is_empty() {
            // **A decision under the diff popup has the card's keys** (`App::key_diff_popup`
            // yields them), so the bar says what the card's keys do; the popup's own keys are
            // named in this head's half.
            own = Some("the diff: pgup/pgdn scroll · esc closes it");
            HintMode::Deciding
        } else if drawn == Drawn::DiffPopup {
            // **The popup is a reading screen with no mode of its own in rano**: `Reading`'s
            // tail (`esc closes this`) is true of it, and its keys go in this head's half. It
            // used to fall through to whatever was under it — the conversation's chords, all
            // dead while the popup owns the keyboard.
            own = Some("↑↓ pgup/pgdn scroll · ctrl-] opens it in the editor");
            HintMode::Reading
        } else if self.editor_focused() {
            HintMode::Editor
        } else if self.editor_drawn() {
            HintMode::EditorBehind
        } else if self.pick.is_some() {
            // **The cards in the chrome are in front of the surface** — they take their keys
            // first in `App::key` — so the bar describes the card while one is up.
            HintMode::Pick
        } else if !self.open.is_empty() {
            HintMode::Deciding
        } else if let Some(s) = surface {
            match s {
                // **A slash listing is a reading screen** (R67 item 9): its keys are the arrows
                // and Esc.
                Surface::Listing => {
                    own = Some("↑↓ scrolls");
                    HintMode::Reading
                }
                Surface::Help | Surface::Stats => HintMode::Reading,
                Surface::Picker => HintMode::SessionPicker,
                // **The row's overlay and the todos pane are READING screens in rano's terms**:
                // rano keeps the bar's tail sentences and is pinned by tag, and its
                // `HintMode::Todos` sentence says *enter or tab unfolds* — false of this pane.
                // So `Reading`'s tail (`esc closes this`) and this head's own half.
                Surface::TodoRow | Surface::Todos => HintMode::Reading,
                Surface::Config => HintMode::Config,
                // **A subagent's output, open over the tree** (R67 item 9): its keys are not the
                // list's.
                Surface::SubOut => {
                    own = Some("↑↓ scroll · enter reads it again · esc back to the subagents");
                    HintMode::Reading
                }
                Surface::Subagents => HintMode::Subagents,
                Surface::JobOut => HintMode::JobOutput,
                Surface::QueueEntry => HintMode::QueueEntry,
                Surface::Queue => HintMode::Queue,
                Surface::Jobs => HintMode::Jobs,
                // **`Reading` and not a mode of its own**: rano is pinned by tag, so a new pane
                // has no `HintMode` to take; its keys go in this head's half.
                Surface::Standing => HintMode::Reading,
            }
        } else if self.payload_sel.is_some() {
            // **The ctrl-v window** (R67 item 9): the arrows and the page keys page it and Esc
            // closes it (`key_payload_window`); the composer stays live under it.
            own = Some("↑↓ pgup/pgdn page the result · home/end its ends · enter still sends");
            HintMode::Reading
        } else {
            HintMode::Conversation
        };
        // **The panes' own keys, in the half of the bar this head owns.** rano owns each mode's
        // tail sentence and is pinned by tag, so what a pane adds goes in the `editor` half,
        // which is the left half of the SAME row and is empty while a pane has the keyboard.
        // They lead there on purpose: the bar is over capacity at 80 columns by construction,
        // and the renderer keeps the head, so the keys a pane adds are the ones that survive.
        let cards_up = self.pick.is_some() || !self.open.is_empty();
        let editor = if let Some(own) = own {
            Line::styled(own, Role::Faint)
        } else if cards_up {
            editor
        } else {
            match surface {
                Some(Surface::TodoRow) => Line::styled(self.todos_open_hint(), Role::Faint),
                Some(Surface::Todos) => Line::styled(self.todos_hint(), Role::Faint),
                Some(Surface::Queue) => {
                    Line::styled("a approve · v veto · d drop · r restart", Role::Faint)
                }
                // **Where Enter goes, said on the bar.** The key opens the note in rano, which
                // is a different place from this list.
                Some(Surface::Standing) => {
                    Line::styled("↑↓ moves · enter opens the note in the editor", Role::Faint)
                }
                _ => editor,
            }
        };
        crate::ui::render::row(&HintBar { editor, mode }.line(w), self.cfg.palette())
    }
}
