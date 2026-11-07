//! A chat message the daemon PUSHED, rather than one a poll brought: the coordinator's read reaches
//! the operator's seat, the daemon fans it out, and the head merges it like any `read` row. P0
//! defines the entry and does nothing with it, so a head on a newer daemon is no noisier than
//! before; P1 merges it, keeps the seen marks and unread counts right, and drops the poll while
//! pushes arrive.

#![allow(dead_code)] // The seam is P0's and its reader is P1's.

use serde_json::Value;

use crate::app::*;

impl App {
    /// One message the coordinator pushed. `names_you` says it is addressed to this reader or
    /// mentions it - what a notice, and the unread ring, are decided on.
    pub(crate) fn on_room_pushed(&mut self, message: Value, names_you: bool) {
        // P0: the seam, and nothing else. A push that is dropped here is not lost - the poll
        // still fetches it - which is why the emptiest body is also the safest one.
        let _ = (message, names_you);
    }
}
