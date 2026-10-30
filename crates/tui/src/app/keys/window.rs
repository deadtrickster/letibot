//! **Keys the ctrl-v window owns**: paging the open output, and handing the rest of a scroll
//! on to the conversation at its edges.

use super::*;
use std::ops::ControlFlow;

impl App {
    /// **An open payload view owns the arrows and Esc**, and it sits here — ahead of
    /// the transcript's own scrolling — for two reasons. The reader has said which row
    /// they are reading, so Up/Down must move *inside* it rather than moving the
    /// conversation underneath; and the seam it draws says `esc closes`, so Esc must
    /// mean that while it is up. It used to lose Esc to the scrollback arm above,
    /// which meant a reader who was parked in the history *and* had a window open got
    /// the transcript un-parked instead — a panel on the screen advertising a key that
    /// had just done something else. Ahead of the decision ladder too, because a
    /// payload view is opened deliberately and a permission that arrives while it is
    /// open should not steal the arrows from under it.
    ///
    /// The same bargain the subagent-output pane makes, and the rule behind both:
    /// whichever surface prints `esc closes` owns Esc, and only one can be up.
    pub(crate) fn key_payload_window(&mut self, k: &Key) -> ControlFlow<Option<Action>> {
        // **Only while the conversation is what is drawn** (R69): the window is a row of the
        // conversation opened wide, and a pane or rano over it is a screen whose arrows are its
        // own — it used to page a window nobody could see under any pane that did not bind ↑↓.
        if self.payload_sel.is_some() && self.drawn() == Drawn::Conversation {
            /// How far one press pages. The same unit the transcript scrolls by.
            const BY: usize = 10;
            match k {
                Key::Esc => {
                    self.payload_sel = None;
                    self.payload_page = 0;
                    // **The live pane's lift is part of the same act** — a click on a run of rows
                    // and a click on the turn's own marker both lift a kind, and Esc is the one
                    // way back for either. One writer takes the switch and the panel together
                    // (`App::drop_lift`), which is what stops the half a hand-written clear
                    // forgot: `set_verbosity` used to empty `payload_sel` and leave the pane's
                    // lift standing, and from then on no key could reach it — the operator's
                    // *"then all subsequent turns go with think open"*.
                    //
                    // **And that is all this arm does.** A panel opened on the thinking closes
                    // the thinking because the lift IS the thinking; a ctrl-v window on one
                    // result is a door onto a row, and closing it must not fold the reasoning —
                    // nor, through `set_thinking`'s one writer, rewrite `head.toml` for a key the
                    // reader pressed for something else. The fold's own Esc is the general arm
                    // (`keys/mod.rs`, after the composer), which reaches it with no panel up.
                    self.drop_lift();
                    // An open run's rows are in the history cache, so it is rebuilt — as
                    // ctrl-t's close does.
                    self.invalidate_history();
                    self.redraw = true;
                    return ControlFlow::Break(None);
                }
                // **An open run has no window to page** (see [`App::run_is_open`]): Esc above
                // folds it, and the arrows move the conversation it is drawn in by the same
                // step — what they did at a window's edge — while the page keys and the wheel
                // go on to the conversation's own scroll.
                Key::Up if self.run_is_open() => {
                    self.scroll_up(BY);
                    self.redraw = true;
                    return ControlFlow::Break(None);
                }
                Key::Down if self.run_is_open() => {
                    self.hold(BY as isize);
                    self.redraw = true;
                    return ControlFlow::Break(None);
                }
                _ if self.run_is_open() => {}
                Key::Up | Key::PageUp => {
                    self.page_payload(true, BY);
                    return ControlFlow::Break(None);
                }
                Key::Down | Key::PageDown => {
                    self.page_payload(false, BY);
                    return ControlFlow::Break(None);
                }
                // The ends, which a long build log is read from as often as its head.
                Key::Home => {
                    self.page_payload(true, usize::MAX);
                    return ControlFlow::Break(None);
                }
                Key::End => {
                    self.page_payload(false, usize::MAX);
                    return ControlFlow::Break(None);
                }
                _ => {}
            }
        }
        ControlFlow::Continue(())
    }
}
