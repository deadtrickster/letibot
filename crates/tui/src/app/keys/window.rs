//! Keys the ctrl-v window owns: paging the open output, and handing the rest of a scroll on to
//! the conversation at its edges.

use super::*;
use std::ops::ControlFlow;

impl App {
    /// An open payload view owns the arrows and Esc, ahead of the transcript's scroll and of the
    /// decision ladder: the reader said which row they are reading, so a card arriving must not
    /// steal the arrows. The rule behind it: whichever surface prints `esc closes` owns Esc.
    pub(crate) fn key_payload_window(&mut self, k: &Key) -> ControlFlow<Option<Action>> {
        // Only while the conversation is what is drawn: the window is a row of the conversation
        // opened wide, and a pane or rano over it is a screen whose arrows are its own.
        if self.payload_sel.is_some() && self.drawn() == Drawn::Conversation {
            /// How far one press pages. The same unit the transcript scrolls by.
            const BY: usize = 10;
            match k {
                Key::Esc => {
                    self.payload_sel = None;
                    self.payload_page = 0;
                    // The live pane's lift is part of the same act: Esc is the one way back for
                    // either lift, through one writer (`App::drop_lift`), so neither is left
                    // standing where no key could reach it. The fold's own Esc is the general arm.
                    self.drop_lift();
                    // An open run's rows are in the history cache, so it is rebuilt - as
                    // ctrl-t's close does.
                    self.invalidate_history();
                    self.redraw = true;
                    return ControlFlow::Break(None);
                }
                // An open run has no window to page (see [`App::run_is_open`]): Esc above folds it,
                // and the arrows move the conversation it is drawn in by the same step, while the
                // page keys and the wheel go on to the conversation's own scroll.
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
