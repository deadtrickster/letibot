//! Tests that match no theme closely.

use super::*;

/// A summary some other builder wrote is shown whole rather than mangled: the
/// head takes the target off the end only when the end IS the target.
#[test]
fn a_summary_that_does_not_end_in_its_target_is_left_alone() {
    assert_eq!(
        ask_without_target("`bash` wants exec access to `ls -la`", "ls -la").as_deref(),
        Some("`bash` wants exec access")
    );
    assert_eq!(
        ask_without_target("something else entirely", "ls -la"),
        None
    );
    assert_eq!(
        ask_without_target("`web_search` wants network access", ""),
        None
    );
}

/// **`/reseat` keeps the conversation; only `/reseat summarise` spends it.**
///
/// The operator flipped this: *"id say flip it - reset is loseless and reset
/// summarize will be not"*. The bare verb is the one people type without
/// reading, so it is the one that must not cost them anything they cannot
/// get back.
#[test]
fn a_bare_reseat_is_the_lossless_one() {
    let mut a = app();
    a.session_id = "s".into();
    typed(&mut a, "/reseat");
    assert_eq!(a.key(Key::Enter), Some(Action::Reseat { summarise: false }));
    for spelling in ["/reseat summarise", "/reseat summarize"] {
        let mut a = app();
        a.session_id = "s".into();
        typed(&mut a, spelling);
        assert_eq!(
            a.key(Key::Enter),
            Some(Action::Reseat { summarise: true }),
            "{spelling}"
        );
    }
    // And the old spellings still mean what they said — they asked to keep
    // the conversation, which is now simply what the bare verb does.
    for spelling in ["/reseat keep", "/reseat verbatim"] {
        let mut a = app();
        a.session_id = "s".into();
        typed(&mut a, spelling);
        assert_eq!(
            a.key(Key::Enter),
            Some(Action::Reseat { summarise: false }),
            "{spelling}"
        );
    }
}

#[test]
fn ctrl_c_clears_the_composer_and_only_a_double_tap_asks() {
    // The old rule quit an idle head on one press and interrupted a running
    // one, so there was no way to abandon a half-typed prompt and a stray
    // Ctrl+C killed the head. opencode's rule, via `letibot_ui::editor`.
    //
    // The double tap now opens the quit card rather than leaving outright —
    // see `the_quit_card_offers_both_exits_and_defaults_to_the_cheap_one`.
    let mut a = app();
    a.clock(1_000);
    typed(&mut a, "half a question I am still");
    assert_eq!(a.key(Key::CtrlC), None, "it clears, it does not quit");
    assert_eq!(a.input(), "");
    assert_eq!(a.key(Key::CtrlC), None, "one press on an empty composer");
    assert_eq!(
        a.key(Key::CtrlC),
        None,
        "the second opens the card, it does not leave"
    );
    assert!(a.quit_card, "the card is up");
}

#[test]
fn the_quit_card_offers_both_exits_and_defaults_to_the_cheap_one() {
    let mut a = app();
    a.clock(1_000);
    a.key(Key::CtrlC);
    assert_eq!(a.key(Key::CtrlC), None);
    assert!(a.quit_card);

    let screen = a.screen(100, 24).join("\n");
    assert!(screen.contains("leave this head"), "{screen}");
    assert!(screen.contains("leave and stop the daemon"), "{screen}");
    // The consequence is ON the row, not in a footnote: the two answers are
    // not alike and the card must not make them look it.
    assert!(screen.contains("prefills cold"), "{screen}");
    // **AND NO WORK IS CLAIMED TO BE RUNNING** — the warning is the running
    // work's, not the row's furniture, so an idle session's card reads as
    // idle. The words below are the warning's own; their absence here is
    // the assertion.
    assert!(
        !screen.contains("stop with the daemon"),
        "nothing is running, so nothing is claimed to die: {screen}"
    );
    // And the footer names the keys this card actually takes — it used to
    // say "ctrl+c again to exit", which a third press no longer does.
    assert!(screen.contains("esc stays"), "{screen}");
    assert!(!screen.contains("ctrl+c again to exit"), "{screen}");

    // Enter on an untouched card takes the smaller exit.
    assert_eq!(a.key(Key::Enter), Some(Action::Quit));
}

/// **The cat's centre does not move while it animates.**
///
/// The operator, watching it: *"why dont you fix cats center during
/// animation"*. Padding the frames to a common slot fixed the block's left
/// edge and left the face inside it growing and shrinking — `(=^.^=)` is
/// seven columns, `(=^.-.=)` is eight — so the thing the eye tracks still
/// moved half a column every frame.
///
/// Four claims, and the first two are what "centre" means here: one width for
/// every frame, the face's fixed glyphs in the same columns in every frame,
/// the expression at the face's own centre, and the tail outside the face
/// where it cannot push it. The last claim is that the animation still
/// animates — a single frame repeated eight times would satisfy everything
/// above.
#[test]
fn cat_frames_are_one_width_with_the_face_in_one_place() {
    let w = CAT_FRAMES[0].len();
    for f in CAT_FRAMES {
        assert!(
            f.is_ascii(),
            "the slot is measured in bytes and centred in columns, which is \
                 one number only for ASCII: {f:?}"
        );
        assert_eq!(f.len(), w, "every frame is one width: {f:?}");
        assert_eq!(&f[0..3], "(=^", "the left of the face never moves: {f:?}");
        assert_eq!(&f[4..7], "^=)", "nor the right: {f:?}");
    }
    // The expression is at the face's centre, so it changes IN PLACE rather
    // than by pushing one side of the face outward.
    assert_eq!(3, (w - 1) / 2, "column 3 is the middle of a {w}-wide slot");
    // And the tail is past the face, where flicking it cannot move anything.
    assert!(CAT_FRAMES.iter().any(|f| f.ends_with('~')));
    assert!(CAT_FRAMES.iter().any(|f| f.ends_with(' ')));
    // It still animates: without this, one frame eight times passes.
    let expressions: std::collections::HashSet<&str> =
        CAT_FRAMES.iter().map(|f| &f[3..4]).collect();
    assert!(
        expressions.len() >= 2,
        "the expression has to change: {expressions:?}"
    );
}

#[test]
fn the_counts_are_plain_and_the_seam_is_faint() {
    the_counts_are_plain_and_the_seam_is_faint_body()
}
