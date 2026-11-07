//! The seam P3 fills: the composer in a room. P0 has the entry points and every one of them
//! declines, so what is proved here is that the composer behaves exactly as it did - a typed line
//! goes out unchanged, with no blob ids, and no reply target is remembered.

use super::*;
use crate::app::rooms::Chan;

/// A line typed in a room goes out as itself: `rooms_outgoing` adds no blob reference and names no
/// attachment, because there is no chip and no upload until P3 has one.
#[test]
fn a_typed_line_goes_out_unchanged() {
    let mut a = app();
    a.open_surface(Surface::Rooms);
    let (text, blobs) = a.rooms_outgoing(">14 the gate is green");
    assert_eq!(text, ">14 the gate is green", "byte for byte");
    assert!(blobs.is_empty(), "nothing to upload: {blobs:?}");
}

/// The reply target, the draft switch and the two completion entry points are inert: each answers
/// *nothing to do*, which is what lets the key fall through to the composer.
#[test]
fn the_composer_seam_declines_everything_yet() {
    let mut a = app();
    a.open_surface(Surface::Rooms);
    assert_eq!(a.rooms_reply_target(), None);
    a.rooms_set_reply(Some(14));
    assert_eq!(a.rooms_reply_target(), None, "P3 keeps the target, not P0");
    assert!(!a.rooms_complete(), "tab is the composer's");
    assert!(!a.rooms_history(true), "up is the composer's");
    // A channel switch keeps nothing and restores nothing: P3 holds the drafts.
    let from = Chan::Room("host".into());
    let to = Chan::Dm("s-b".into());
    a.rooms_switched(Some(&from), &to);
    assert_eq!(a.editor.text(), "", "no draft was restored over the line");
}
