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
        if self.payload_sel.is_some() {
            /// How far one press pages. The same unit the transcript scrolls by.
            const BY: usize = 10;
            match k {
                Key::Esc => {
                    self.payload_sel = None;
                    self.payload_page = 0;
                    self.redraw = true;
                    return ControlFlow::Break(None);
                }
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
