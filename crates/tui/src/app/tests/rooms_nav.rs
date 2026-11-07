//! The seam P4 fills: the message cursor, the jumps, the switcher and search. P0 has the bindings
//! and the entry points, and every one of them declines - so what is proved here is that the keys
//! are the composer's exactly as they were, which is what "today's behaviour unchanged" means for a
//! view that has no cursor yet.

use super::*;
use std::ops::ControlFlow;

/// The room's chat keys are bound and dispatch, and with no message under the cursor each one
/// declines: the letter reaches the composer and starts a line, as it always did.
#[test]
fn the_rooms_chat_keys_are_the_composers_until_a_cursor_exists() {
    for key in [
        Key::Char('R'),
        Key::Char('y'),
        Key::Char('t'),
        Key::Char('u'),
        Key::Char('G'),
        Key::Char('n'),
        Key::Char('N'),
        Key::Tab,
        Key::End,
    ] {
        let mut a = app();
        a.open_surface(Surface::Rooms);
        a.key(key.clone());
        let typed = a.editor.text();
        let expected = match &key {
            Key::Char(c) => c.to_string(),
            _ => String::new(),
        };
        assert_eq!(
            typed, expected,
            "{key:?} did not reach the composer: {typed:?}"
        );
    }
}

/// The entry points themselves answer nothing: no cursor, no jump, no switcher, no needle. That is
/// the state P4 starts from, and the one a screen is drawn in.
#[test]
fn the_navigation_seam_answers_nothing_yet() {
    let mut a = app();
    a.open_surface(Surface::Rooms);
    assert_eq!(a.rooms_cursor(), None);
    assert!(
        matches!(a.rooms_cursor_move(true), ControlFlow::Continue(())),
        "up is the composer's"
    );
    assert!(matches!(
        a.rooms_jump(crate::app::chat_nav::Jump::Unread),
        ControlFlow::Continue(())
    ));
    assert!(matches!(a.rooms_switcher(), ControlFlow::Continue(())));
    assert!(matches!(a.rooms_copy(), ControlFlow::Continue(())));
    assert!(matches!(a.rooms_thread(), ControlFlow::Continue(())));
    a.rooms_search("gate");
    assert_eq!(a.rooms_search_needle(), None, "P4 keeps the needle");
    // And no picture is drawn from a cache that does not exist yet.
    assert_eq!(a.room_picture("abc"), None);
}
