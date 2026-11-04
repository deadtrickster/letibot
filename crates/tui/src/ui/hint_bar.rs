//! The hint bar: the keys that do something right now. This head's half is generated from the
//! tables its keys are dispatched from, so the bar cannot name a key that does something else;
//! rano's cards and editor pane keep rano's own sentences, pinned by tag.

use crate::app::*;
use rano::agent::hint_bar::{HintBar, HintMode, centred};
use rano::render::Line;
use rano::style::Role;

/// One of the conversation's chords, as the bar names it - read from the table its dispatch
/// reads where there is one (`Surface::chord`, `Show::chord`), so the two cannot drift.
enum Chord {
    Surface(Surface, &'static str),
    Show(Show),
    Says(&'static str, &'static str),
}

/// The conversation's chords, in the order the bar names them. The bar is over capacity at
/// 80 columns by construction and keeps its head, so the order is a ranking: the panes and the
/// reflexes first, `/help` - the index - last.
const CONVERSATION: &[Chord] = &[
    Chord::Surface(Surface::Picker, "sessions"),
    Chord::Says("ctrl-n", "clears notes"),
    Chord::Surface(Surface::Todos, "todos"),
    Chord::Surface(Surface::Subagents, "subagents"),
    Chord::Show(Show::Thinking),
    // `ctrl-v` and `/t` are adjacent on purpose: they are the two things a reader
    // confuses, so the bar states both - one result against all of them.
    Chord::Says("ctrl-v", "newest result"),
    Chord::Says("/t", "all tool rows"),
    // A notice line's details from the keyboard: the click's other half.
    Chord::Says("alt-n", "newest notice"),
    Chord::Surface(Surface::Jobs, "jobs"),
    Chord::Says("ctrl-p", "hold"),
    Chord::Says("/help", ""),
];

impl App {
    /// The conversation's half of the bar, from [`CONVERSATION`].
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

    /// The open decision's half of the bar - this head's, not rano's `Deciding`
    /// tail, which says *"a row number answers"*: a row number marks the row, and Enter answers.
    pub(crate) fn deciding_tail(&self) -> String {
        use crate::ui::cards::decision::{LADDER_HINT, PUT_AWAY_HINT, QUESTION_LADDER_HINT};
        match self.open.first() {
            Some(d) if d.kind == "question" && decision_rows(d) == 0 => {
                format!("type your answer · enter sends it · {PUT_AWAY_HINT}")
            }
            // A question's digits are the answer being typed, so its tail offers no row number.
            Some(d) if d.kind == "question" => {
                format!("{QUESTION_LADDER_HINT} · {PUT_AWAY_HINT}")
            }
            _ => format!("{LADDER_HINT} · or type an option · {PUT_AWAY_HINT}"),
        }
    }

    /// The bar of a card that owns the keyboard: the tail, and whether the composer's
    /// half stays (only the todo card types into the composer). Which card is up is
    /// [`App::card_up`]'s answer, so this bar, `App::key` and `App::focused_surface` read one list.
    pub(crate) fn card_hint(&self) -> Option<(String, bool)> {
        use crate::app::Card;
        use crate::ui::cards::decision::PUT_AWAY_HINT;
        let (tail, composer) = match self.card_up()? {
            Card::ModeConfirm => (
                "y or enter confirms allow-all · any other key cancels".to_string(),
                false,
            ),
            Card::Secret => (
                "type the password · enter gives it · esc refuses".to_string(),
                false,
            ),
            Card::TermAsk => (
                "y ends it · any other key keeps it running".to_string(),
                false,
            ),
            Card::Prompt => (
                format!("type the line · enter sends it · {PUT_AWAY_HINT}"),
                false,
            ),
            Card::KeyAsk => (
                "type the key · enter stores it · esc cancels".to_string(),
                false,
            ),
            Card::TodoDraft => (
                "tab next field · enter adds it · esc cancels".to_string(),
                true,
            ),
        };
        Some((tail, composer))
    }

    /// The bottom bar: what the keys do, right now. The composer owns the first half and changes it
    /// after the first Esc or Ctrl+C, which is how anyone finds out a double-tap exists; the head
    /// owns the second half, which is its own keys.
    pub(crate) fn hint_bar(&self, w: usize) -> String {
        // The editor's own half is about the composer's double-taps, and while the quit card is up
        // there is no double-tap left to learn - the card IS the second press - so its "ctrl+c
        // again to exit" would name a key that closes the card. rano drops it for `QuitCard`.
        let editor = match self.editor.hint_text(self.now_ms) {
            Some(s) => Line::styled(s, Role::Attention),
            None => Line::default(),
        };
        let p = self.cfg.palette();
        // The surface in front is `drawn()`'s answer, the value `main_area` draws and `App::key`
        // routes on, so the bar cannot describe a pane open behind another. The order below is the
        // order `App::key` asks in: a card owning every key, the popup, rano, the chrome, the surface.
        let drawn = self.drawn();
        // The cards that own the keyboard come first, in `App::key`'s order: every key is theirs,
        // so the composer's half is dropped - except under the todo card, whose fields ARE the
        // composer. A bar naming a chord the card swallows is a lie that cancels the card.
        if let Some((tail, composer)) = self.card_hint() {
            let head = composer.then_some(&editor);
            return crate::ui::render::row(&centred(head, &tail, w), p);
        }
        // Then the order `App::key` asks in and the slot draws in: the diff popup, the editor pane
        // while it has the keyboard (rano takes every key before `App::key` sees one), an open
        // decision, the quit card, the setting card. Draw order and ask order must not diverge.
        let rano_mode = if drawn == Drawn::DiffPopup && !self.open.is_empty() {
            // A decision under the diff popup has the card's keys (`App::key_diff_popup` yields
            // them), and the popup's own keys lead. The spelling matches the popup edge's, and it
            // says "the popup" because a notice has no diff.
            let own = "the popup: pgup/pgdn scroll · esc closes it";
            return crate::ui::render::row(
                &centred(
                    Some(&Line::styled(own, Role::Faint)),
                    &self.deciding_tail(),
                    w,
                ),
                p,
            );
        } else if drawn == Drawn::DiffPopup {
            // A notice has no file behind it, so its bar does not offer the editor.
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
        } else if !self.open.is_empty() {
            // The decision's own bar - a question's names the way to step away, and
            // a permission's says a row number marks rather than answers.
            return crate::ui::render::row(&centred(Some(&editor), &self.deciding_tail(), w), p);
        } else if self.quit_card {
            Some(HintMode::QuitCard)
        } else if self.detached() {
            Some(HintMode::Detached)
        } else if self.editor_drawn() {
            Some(HintMode::EditorBehind)
        } else if self.pick.is_some() {
            // The cards in the chrome are in front of the surface - they take their keys
            // first in `App::key` - so the bar describes the card while one is up.
            Some(HintMode::Pick)
        } else {
            None
        };
        if let Some(mode) = rano_mode {
            return crate::ui::render::row(&HintBar { editor, mode }.line(w), p);
        }
        let tail = match self.focused_surface() {
            Some(s) => self.surface_hint(s),
            // The ctrl-v window: the arrows and the page keys page it and Esc
            // closes it (`key_payload_window`); the composer stays live under it.
            // An open run is the conversation's rows, scrolled as they are; Esc folds it.
            None if self.run_is_open() => "esc folds the run back · click or /t unfolds its rows".to_string(),
            None if self.payload_sel.is_some() => {
                "↑↓ pgup/pgdn or the wheel page the result · a click reads from there · esc closes · enter still sends"
                    .to_string()
            }
            None => self.conversation_hint(),
        };
        // The plugin pane's two dynamic halves come FIRST: the legend's words for the row the
        // pointer is over, and the selected stop's own action letters. Both before the key hints,
        // which fall off a narrow bar's right - a reader pointing at a word wants the word explained.
        let mut lead: Vec<String> = Vec::new();
        if self.drawn() == Drawn::Surface(Surface::PluginPane) {
            lead.extend(self.plugin_hover_legend().map(str::to_string));
            lead.extend(self.plugin_actions_hint());
        }
        let tail = if lead.is_empty() {
            tail
        } else {
            format!("{} · {tail}", lead.join(" · "))
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
