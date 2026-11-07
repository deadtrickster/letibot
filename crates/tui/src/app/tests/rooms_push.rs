//! A chat message the daemon PUSHED: the head merges it like a `read` row, keeps its seen and unread
//! marks right, and slows the poll while pushes carry the news - until a `seq` shows one was missed.

use super::rooms::{history, msg, opened, ops, reply, rooms_reply};
use super::*;
use crate::app::rooms::Chan;
use crate::app::rooms_push::PUSH_POLL_MS;
use serde_json::Value;

/// A project channel, the room list's shape for one.
const PROJECT: &str = "project/ab12cd34ef56";

fn pushed(a: &mut App, m: Value, names_you: bool) {
    a.apply(ServerFrame::RoomChatPushed {
        message: m,
        names_you,
    });
}

/// What the screen says, one row per line: a push is drawn where a poll's row would have been, and
/// nothing was asked of the daemon to bring it.
#[test]
fn a_pushed_message_draws_without_a_poll() {
    let mut a = opened(1_000);
    pushed(
        &mut a,
        msg(4, "host", "s-a", "session", "pushed, not polled"),
        false,
    );
    let asked = ops(&mut a);
    assert!(
        asked.iter().all(|op| op["type"] == "ack"),
        "a push must not send the head asking for history: {asked:?}"
    );
    let screen = a.screen(80, 24).join("\n");
    assert!(screen.contains("pushed, not polled"), "{screen}");
    // And the next poll is the slow one, because the push is what brought the news.
    assert_eq!(
        a.rooms.next_poll_ms,
        1_000 + PUSH_POLL_MS,
        "the poll slowed"
    );
}

/// A push for a channel the operator is not reading is unread until the poll says otherwise: the
/// count is on the channel, and the words stay off the screen.
#[test]
fn a_pushed_message_in_another_channel_counts_as_unread() {
    let mut a = opened(1_000);
    reply(
        &mut a,
        rooms_reply(&[("host", 2, 0, 0), (PROJECT, 2, 0, 0)]),
    );
    a.take_actions();
    pushed(
        &mut a,
        msg(9, PROJECT, "s-b", "session", "said elsewhere"),
        false,
    );
    a.take_actions();
    assert_eq!(
        a.rooms.unread(&Chan::Room(PROJECT.into())),
        1,
        "the other channel's ring never moved"
    );
    assert_eq!(
        a.rooms.unread(&Chan::Room("host".into())),
        0,
        "the channel being read counts nothing"
    );
    let screen = a.screen(80, 24).join("\n");
    assert!(!screen.contains("said elsewhere"), "{screen}");
    // And a message that names this reader is news even under the operator's own name, which is
    // the one case where `names_you` decides the ring.
    pushed(
        &mut a,
        msg(10, PROJECT, "operator", "operator", "for you"),
        true,
    );
    assert_eq!(a.rooms.unread(&Chan::Room(PROJECT.into())), 2);
}

/// A `seq` that skips one this head never read is a push that missed something: the poll comes at
/// once, asks from the last seq held without a hole, and is fast again afterwards.
#[test]
fn a_seq_gap_returns_to_the_fast_poll() {
    let mut a = opened(1_000);
    reply(
        &mut a,
        history(
            "host",
            vec![
                msg(1, "host", "s-a", "session", "one"),
                msg(2, "host", "s-a", "session", "two"),
            ],
        ),
    );
    a.take_actions();
    // Contiguous, so the push is the whole story and the poll waits.
    pushed(&mut a, msg(3, "host", "s-a", "session", "three"), false);
    assert_eq!(a.rooms.next_poll_ms, 1_000 + PUSH_POLL_MS);
    // 6 skips 4 and 5: the push cannot carry a message it never saw.
    pushed(&mut a, msg(6, "host", "s-a", "session", "six"), false);
    assert_eq!(a.rooms.next_poll_ms, 0, "a gap asks now");
    a.clock(2_000);
    let asked = ops(&mut a);
    let read = asked
        .iter()
        .find(|op| op["type"] == "read" && op["room"] == "host")
        .unwrap_or_else(|| panic!("no read of the host room: {asked:?}"));
    assert_eq!(
        read["since"], 3,
        "the read must start where the hole starts: {asked:?}"
    );
    assert_eq!(
        a.rooms.next_poll_ms,
        2_000 + crate::app::rooms::POLL_MS,
        "the fast poll is back"
    );
}

/// The same message from a poll and from a push is one row: a head that merges a push still polls,
/// and the two must not draw the line twice in either order.
#[test]
fn a_push_and_a_poll_of_one_message_draw_it_once() {
    let once = |poll_first: bool| {
        let mut a = opened(1_000);
        let m = msg(7, "host", "s-a", "session", "said once");
        if poll_first {
            reply(&mut a, history("host", vec![m.clone()]));
            pushed(&mut a, m, false);
        } else {
            pushed(&mut a, m.clone(), false);
            reply(&mut a, history("host", vec![m]));
        }
        let screen = a.screen(80, 24).join("\n");
        let drawn = screen.matches("said once").count();
        assert_eq!(
            a.rooms.msgs.get("host").map(Vec::len),
            Some(1),
            "one row: {:?}",
            a.rooms.msgs
        );
        assert_eq!(drawn, 1, "drawn once (poll first: {poll_first}):\n{screen}");
    };
    once(true);
    once(false);
}
