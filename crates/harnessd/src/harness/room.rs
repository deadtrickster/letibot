//! **The box's room, as this session speaks in it**: the link a `room` tool and a room pane read,
//! and the reminders the idle check puts on this session's own board.

use super::*;

impl Harness {
    /// This session's connection to the box's room, when it has one: the `room` tool's half, and what a
    /// room pane is drawn from. `None` on a box with no coordinator and on a subagent, which speaks
    /// through its parent, and both are ordinary states rather than failures.
    pub fn room_link(&self) -> Option<Arc<crate::room::link::SessionRoom>> {
        self.room.clone()
    }

    /// **Whether this session speaks in the box's room** - the daemon's own question, and the one
    /// the room pass's clock asks before it makes itself a reason to wake.
    pub fn has_room(&self) -> bool {
        self.room.is_some()
    }

    /// The room's reminders, onto this session's own board: one row per item it holds, carried by the
    /// idle check so a holder is reminded without a model call. A subagent returns 0 first, because the
    /// room's rule is top-level sessions only. A room that does not answer clears the rows.
    pub fn fill_room_board(&mut self) -> usize {
        // **The room's rule, said here as well as at the door.** See this function's own doc.
        if self.cfg.parent_session_id.is_some() {
            return 0;
        }
        let Some(room) = self.room.clone() else {
            return 0;
        };
        let me = self.hub.session_id();
        let rows = match room.reminders() {
            Ok(all) => crate::room::remind::rows(&me, &all),
            Err(_) => Vec::new(),
        };
        let n = self.todos.set_room_rows(rows);
        self.flush_todos_or_say("the room's reminders");
        n
    }
}
