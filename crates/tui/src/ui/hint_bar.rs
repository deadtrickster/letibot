//! **The hint bar**: the keys that do something right now.
//!
//! Two sources, and which one is the decision this file makes (R69). **What is this head's is
//! generated from the tables its keys are dispatched from** — the surface in front
//! (`Surface::bindings`) and, with nothing in front, the conversation's chords
//! ([`CONVERSATION`]) — so the bar cannot name a key that does something else. **What is
//! rano's stays rano's**: the cards and the editor pane (`HintMode::QuitCard`, `Detached`,
//! `Deciding`, `Pick`, `Editor`, `EditorBehind`), whose sentences are rano's, pinned by tag.
//!
//! It is generated rather than left to rano's own conversation tail because only this head
//! knows its dispatch tables: a subagent's output, a slash listing, the standing pane and the
//! ctrl-v window have no rano sentence at all (R67 item 9), and the conversation's chords are
//! this head's. rano lays the bar out either way — `HintBar::line_with_tail` takes the tail
//! from here, and `hint_bar::centred` draws the diff popup's, whose first half is not the
//! composer's — so the centring is rano's and is not repeated here (rano 0.8.2).

use crate::app::*;
use rano::agent::hint_bar::{HintBar, HintMode, centred};
use rano::render::Line;
use rano::style::Role;

/// **One of the conversation's chords, as the bar names it** — read from the table its dispatch
/// reads where there is one (`Surface::chord`, `Show::chord`), so the two cannot drift.
enum Chord {
    Surface(Surface, &'static str),
    Show(Show),
    Says(&'static str, &'static str),
}

/// **The conversation's chords, in the order the bar names them.** The bar is over capacity at
/// 80 columns by construction and keeps its head, so the order is a ranking: the panes and the
/// reflexes first, `/help` — the index — last.
const CONVERSATION: &[Chord] = &[
    Chord::Surface(Surface::Picker, "sessions"),
    Chord::Says("ctrl-n", "clears notes"),
    Chord::Surface(Surface::Todos, "todos"),
    Chord::Surface(Surface::Subagents, "subagents"),
    Chord::Show(Show::Thinking),
    // **`ctrl-v` and `/t` are adjacent on purpose** (R40): they are the two things a reader
    // confuses, so the bar states both — one result against all of them.
    Chord::Says("ctrl-v", "newest result"),
    Chord::Says("/t", "all tool rows"),
    Chord::Surface(Surface::Jobs, "jobs"),
    Chord::Says("ctrl-p", "hold"),
    Chord::Says("/help", ""),
];

impl App {
    /// **The conversation's half of the bar**, from [`CONVERSATION`].
    pub(crate) fn conversation_hint(&self) -> String {
        CONVERSATION
            .iter()
            .filter_map(|c| match c {
                Chord::Surface(s, says) => s.chord().map(|(key, _)| format!("{key} {says}")),
                Chord::Show(s) => s.chord().map(|(key, _)| format!("{key} {}", s.name())),
                Chord::Says(key, "") => Some(key.to_string()),
                Chord::Says(key, says) => Some(format!("{key} {says}")),
            })
            .collect::<Vec<_>>()
            .join(" · ")
    }

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
        let p = self.cfg.palette();
        // **The surface in front is `drawn()`'s answer** (R69), the value `main_area` draws and
        // `App::key` routes on — so the bar cannot describe a pane that is open behind another.
        // The order below is the order `App::key` asks in: a card that owns every key, then the
        // diff popup, rano, the cards in the chrome, the surface, the conversation.
        let drawn = self.drawn();
        let rano_mode = if self.quit_card {
            Some(HintMode::QuitCard)
        } else if self.detached() {
            Some(HintMode::Detached)
        } else if drawn == Drawn::DiffPopup && !self.open.is_empty() {
            // **A decision under the diff popup has the card's keys** (`App::key_diff_popup`
            // yields them); the popup's own keys lead.
            let own = "the diff: pgup/pgdn scroll · esc closes it";
            return crate::ui::render::row(
                &centred(
                    Some(&Line::styled(own, Role::Faint)),
                    HintMode::Deciding.tail(),
                    w,
                ),
                p,
            );
        } else if drawn == Drawn::DiffPopup {
            // **A notice has no file behind it**, so its bar does not offer the editor.
            let notice = self.diff_popup.as_ref().is_some_and(|d| {
                matches!(d.body, crate::app::diff_popup::PopupBody::Notice { .. })
            });
            return crate::ui::render::row(
                &centred(
                    Some(&editor),
                    if notice {
                        "↑↓ pgup/pgdn scroll · esc closes it"
                    } else {
                        "↑↓ pgup/pgdn scroll · ctrl-] opens it in the editor · esc closes it"
                    },
                    w,
                ),
                p,
            );
        } else if self.editor_focused() {
            Some(HintMode::Editor)
        } else if self.editor_drawn() {
            Some(HintMode::EditorBehind)
        } else if self.pick.is_some() {
            // **The cards in the chrome are in front of the surface** — they take their keys
            // first in `App::key` — so the bar describes the card while one is up.
            Some(HintMode::Pick)
        } else if !self.open.is_empty() {
            Some(HintMode::Deciding)
        } else {
            None
        };
        if let Some(mode) = rano_mode {
            return crate::ui::render::row(&HintBar { editor, mode }.line(w), p);
        }
        let tail = match self.focused_surface() {
            Some(s) => self.surface_hint(s),
            // **The ctrl-v window** (R67 item 9): the arrows and the page keys page it and Esc
            // closes it (`key_payload_window`); the composer stays live under it.
            None if self.payload_sel.is_some() => {
                "↑↓ pgup/pgdn page the result · home/end its ends · enter still sends · esc closes it"
                    .to_string()
            }
            None => self.conversation_hint(),
        };
        crate::ui::render::row(
            &HintBar {
                editor,
                mode: HintMode::Conversation,
            }
            .line_with_tail(&tail, w),
            p,
        )
    }
}
