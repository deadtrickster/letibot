//! Moving through the chat rather than through the channels: the message cursor, the jump to the
//! tail or to the unread divider, the `Ctrl+K` switcher and `/search`. P0 defines the seam and
//! declines every call, so today's keys keep their meaning; P4 fills them.

#![allow(dead_code)] // The seam is P0's and its readers are P4's.

use std::ops::ControlFlow;

use crate::app::*;

/// Where a jump goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Jump {
    /// The newest message: what `End` and `G` do.
    Tail,
    /// The divider before the first message the reader had not seen.
    Unread,
}

impl App {
    /// The `seq` under the message cursor, `None` while the composer has the keys.
    pub(crate) fn rooms_cursor(&self) -> Option<u64> {
        None
    }

    /// One message up or down. `Continue` is *not ours*: the key is the composer's as before.
    pub(crate) fn rooms_cursor_move(&mut self, _up: bool) -> ControlFlow<Option<Action>> {
        ControlFlow::Continue(())
    }

    pub(crate) fn rooms_jump(&mut self, _to: Jump) -> ControlFlow<Option<Action>> {
        ControlFlow::Continue(())
    }

    /// `Ctrl+K`: fuzzy over the channel labels and DM peers, unread first.
    pub(crate) fn rooms_switcher(&mut self) -> ControlFlow<Option<Action>> {
        ControlFlow::Continue(())
    }

    /// `/search TEXT`: a substring over the loaded messages of this channel.
    pub(crate) fn rooms_search(&mut self, _needle: &str) {}

    /// The needle the chat is drawn against, when a search is open.
    pub(crate) fn rooms_search_needle(&self) -> Option<&str> {
        None
    }

    /// `y`: the message under the cursor onto the operator's clipboard (OSC 52).
    pub(crate) fn rooms_copy(&mut self) -> ControlFlow<Option<Action>> {
        ControlFlow::Continue(())
    }

    /// `t`: the thread of the message under the cursor, root and replies.
    pub(crate) fn rooms_thread(&mut self) -> ControlFlow<Option<Action>> {
        ControlFlow::Continue(())
    }

    /// `n` and `N`: the next or the previous hit of the open search.
    pub(crate) fn rooms_search_step(&mut self, _forward: bool) -> ControlFlow<Option<Action>> {
        ControlFlow::Continue(())
    }
}
