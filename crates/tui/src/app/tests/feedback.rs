//! **The operator's marks** (TODO R81): `/fb`, `/fb+`, `/fb-`, alt-g and alt-b — what each one
//! sends, and alt-b staying word-left wherever there is a line to move over.

use super::*;

fn attached() -> App {
    let mut a = app();
    a.session_id = "s1".into();
    a
}

fn mark(a: Option<Action>) -> (String, String, Option<String>) {
    match a {
        Some(Action::Feedback {
            polarity,
            note,
            item_id,
            at_ms,
        }) => {
            assert!(at_ms > 0, "stamped at the press");
            (polarity, note, item_id)
        }
        other => panic!("not a mark: {other:?}"),
    }
}

#[test]
fn the_three_verbs_send_their_polarity_and_the_note_with_its_tags() {
    let mut a = attached();
    assert_eq!(
        mark(a.command("fb- #distracted got pulled off by j975")),
        ("bad".into(), "#distracted got pulled off by j975".into(), None)
    );
    assert_eq!(mark(a.command("fb+")), ("good".into(), String::new(), None));
    assert_eq!(
        mark(a.command("fb this is where it got dumb")),
        ("note".into(), "this is where it got dumb".into(), None)
    );
    assert!(a.command("fb").is_none(), "a bare /fb is the help line");
    assert!(
        a.feedback_command("fbx").is_none(),
        "a longer word is not the verb"
    );
}

#[test]
fn a_mark_while_scrolled_back_names_the_row_the_view_is_held_on() {
    let mut a = attached();
    a.anchor = Some(crate::app::scroll::Held {
        item_id: "tr.7".into(),
        ordinal: 7,
        into: 0,
    });
    assert_eq!(mark(a.command("fb-")).2.as_deref(), Some("tr.7"));
}

#[test]
fn alt_g_and_alt_b_mark_now_and_a_second_press_opens_the_note() {
    let mut a = attached();
    assert_eq!(mark(a.key(Key::AltG)).0, "good");
    assert!(a.key(Key::AltG).is_none(), "the second press sends nothing");
    assert_eq!(a.editor.text(), "/fb+ ", "it opens the note");
    let mut b = attached();
    assert_eq!(mark(b.key(Key::AltB)).0, "bad");
}

/// **alt-b was word-left before it was a mark**, and on a line it still is.
#[test]
fn alt_b_on_a_line_being_typed_is_still_word_left() {
    let mut a = attached();
    for c in "two words".chars() {
        a.key(Key::Char(c));
    }
    let before = a.editor.cursor();
    assert!(a.key(Key::AltB).is_none(), "no mark");
    assert!(a.editor.cursor() < before, "the cursor moved a word left");
    assert!(
        matches!(a.key(Key::AltG), Some(Action::Feedback { .. })),
        "alt-g marks over a line being typed: it was nobody's key"
    );
}

#[test]
fn a_head_on_no_session_has_nothing_to_mark() {
    let mut a = app();
    assert!(a.key(Key::AltG).is_none());
    assert!(a.command("fb+ fine").is_none());
}
