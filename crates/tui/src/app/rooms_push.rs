//! A chat message the daemon PUSHED, rather than one a poll brought: the coordinator's read reaches
//! the operator's seat, the daemon fans it out, and the head merges it like any `read` row. The poll
//! stays as the net under the push, and slows down while the push is doing the carrying.

use serde_json::Value;

use crate::app::rooms::{Chan, Msg, OPERATOR};
use crate::app::*;

/// How often the view asks while pushes are arriving: the push is how the news travels now, so the
/// poll is only the safety net - and a net does not have to be cast every two seconds.
pub(crate) const PUSH_POLL_MS: u64 = 15_000;

impl App {
    /// One message the coordinator pushed. `names_you` says it is addressed to this reader or
    /// mentions it - what a notice, and the unread ring, are decided on.
    pub(crate) fn on_room_pushed(&mut self, message: Value, names_you: bool) {
        let room = message
            .get("room")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let Ok(m) = serde_json::from_value::<Msg>(message) else {
            return;
        };
        if room.is_empty() {
            return;
        }
        // The channel the operator is looking at has this message in front of them, so it is read:
        // the ring drops the count a poll would have carried and the tail may be acked.
        let drawn = self.is_open(Surface::Rooms)
            && match self.rooms.sel.as_ref() {
                Some(Chan::Room(id)) => *id == room,
                Some(Chan::Dm(_) | Chan::Session(_)) => room == "dm",
                None => false,
            };
        if let Some(row) = self.rooms.rows.iter_mut().find(|r| r.id == room) {
            row.last_seq = row.last_seq.max(m.seq);
            if drawn {
                row.unread = 0;
            } else if m.author != OPERATOR || names_you {
                // News for this head, not its own words echoed back: the ring moves now instead of
                // fifteen seconds from now. A poll replaces the row, so a wrong count is short.
                row.unread = row.unread.saturating_add(1);
            }
        }
        // A seq this head never asked for leaves a hole behind the push: the poll comes at once and
        // asks from the last seq held without one, so nothing in between is skipped.
        let before = self.rooms.read_to.get(&room).copied().unwrap_or(0);
        let gap = before > 0 && m.seq > before.saturating_add(1);
        self.rooms.merge(&room, vec![m]);
        if gap {
            self.rooms.read_to.insert(room, before);
            self.rooms.next_poll_ms = 0;
        } else {
            self.rooms.next_poll_ms = self.now_ms.saturating_add(PUSH_POLL_MS);
        }
        if drawn {
            // The push landed where a poll's row would: the tail this head has drawn may be acked.
            self.rooms_shown();
        }
    }
}
