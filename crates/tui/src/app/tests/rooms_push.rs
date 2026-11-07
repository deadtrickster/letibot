//! The seam P1 fills: a chat message the daemon PUSHED, merged like a `read` row instead of waiting
//! for the next poll. P0 has the entry and no behaviour, so what is proved here is that a push is
//! harmless - it is applied, and nothing the head draws changes until P1 merges it.

use super::*;
use serde_json::json;

/// A push reaches the app with no request and no read: the frame is a `Control` disposition, the
/// /rooms view is untouched, and the screen is what it was.
#[test]
fn a_pushed_message_is_applied_and_changes_nothing_yet() {
    let mut a = app();
    a.push_surface(Surface::Rooms);
    let before = a.screen(80, 24);
    let disposition = a.apply(ServerFrame::RoomChatPushed {
        message: json!({"seq": 7, "ts": 1_793_735_794_000u64, "room": "host", "author": "s-a",
                        "kind": "session", "text": "pushed, not polled"}),
        names_you: true,
    });
    assert_eq!(disposition, Disposition::Control, "not an event to count");
    assert_eq!(a.rooms.read_to.len(), 0, "no read was made");
    assert!(
        a.rooms.msgs.is_empty(),
        "P1 merges; P0 draws nothing: {:?}",
        a.rooms.msgs
    );
    assert_eq!(a.screen(80, 24), before, "the screen is what it was");
}
