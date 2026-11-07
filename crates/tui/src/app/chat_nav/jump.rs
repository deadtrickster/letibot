//! `End` and `G` go to the newest message; `u` goes to the unread divider - the place before the
//! first message that was newer than the channel's seen mark when it was opened.

use super::*;
use crate::app::rooms::RoomsView;

pub(super) fn to(a: &mut App, to: Jump) -> std::ops::ControlFlow<Option<Action>> {
    let Some(c) = a.rooms.selected() else {
        return std::ops::ControlFlow::Continue(());
    };
    match to {
        Jump::Tail => {
            let top = a.rooms.messages(&c).last().map(|m| m.seq);
            a.rooms_to_tail();
            a.rooms_cursor_set(top);
            std::ops::ControlFlow::Break(None)
        }
        Jump::Unread => {
            // The divider sits before the first message newer than the mark saved when the channel
            // was opened, so the cursor lands on that message: one step above it is the divider.
            let from = a
                .rooms
                .opened_seen
                .get(&RoomsView::key(&c))
                .copied()
                .unwrap_or(0);
            let first = a
                .rooms
                .messages(&c)
                .into_iter()
                .find(|m| m.seq > from)
                .map(|m| m.seq);
            match first {
                Some(seq) => {
                    // The cursor is the jump: the pane brings the entry under it into the window
                    // when it draws, and the divider is drawn above exactly this entry.
                    a.rooms_cursor_set(Some(seq));
                    std::ops::ControlFlow::Break(None)
                }
                None => {
                    a.say("nothing unread in this room");
                    std::ops::ControlFlow::Break(None)
                }
            }
        }
    }
}
