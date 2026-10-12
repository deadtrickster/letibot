//! **R90: keys, cards and popups agree** — each case a key sequence that used to send an answer,
//! a `/reseat` or a quit the operator did not choose, pinned so it cannot come back.

use super::*;
use letibot_sessionlog::question::QuestionAnswer;

fn ask(a: &mut App, seq: u64, e: SessionEvent) {
    a.apply(ServerFrame::Event(env(seq, e)));
}

fn answer(req_id: &str, option_id: &str) -> Option<Action> {
    Some(Action::Answer {
        req_id: req_id.into(),
        option_id: option_id.into(),
        pattern: None,
        note: None,
    })
}

// ─────────────────────── R90.4: the highlight follows the front card ───────────────────────

/// ↓ to deny, a second permission arrives behind the card on screen, Enter still denies.
#[test]
fn a_decision_arriving_behind_the_card_does_not_move_its_highlight() {
    let mut a = app();
    ask(&mut a, 1, testing::requested("r1", "rm"));
    assert_eq!(a.key(Key::Down), None);
    ask(&mut a, 2, testing::requested("r2", "rm -rf"));
    assert_eq!(a.open[0].req_id, "r1", "the new card queues behind");
    assert_eq!(a.key(Key::Enter), answer("r1", "deny"));
}

/// Card A answered on its second row; card B comes up at its first, not A's.
#[test]
fn the_next_card_up_starts_at_its_own_top() {
    let mut a = app();
    ask(&mut a, 1, testing::requested("r1", "rm"));
    ask(&mut a, 2, testing::requested("r2", "rm -rf"));
    a.key(Key::Down);
    assert_eq!(a.key(Key::Enter), answer("r1", "deny"));
    ask(&mut a, 3, testing::answered("r1", "deny"));
    assert_eq!(a.open[0].req_id, "r2");
    assert_eq!(a.key(Key::Enter), answer("r2", "allow"));
}

// ───────────────── R90.3: a question over typed words does not take them ─────────────────

/// A question that arrives while a prompt is half typed: the words are held, the card's field is
/// empty, Enter answers the marked row, and the words come back when the card is gone.
#[test]
fn a_question_arriving_over_typed_words_holds_them_instead_of_taking_them() {
    let mut a = app();
    typed(&mut a, "check the logs first");
    ask(
        &mut a,
        1,
        testing::asked("q1", "which branch?", &["main", "release"], ""),
    );
    assert_eq!(a.input(), "", "the card's field starts empty");
    assert!(
        a.notice
            .as_deref()
            .unwrap_or("")
            .contains("your line is held"),
        "{:?}",
        a.notice
    );
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::AnswerQuestion {
            req_id: "q1".into(),
            answer: QuestionAnswer::choosing(0),
        }),
        "Enter answers the marked row, not the operator's prompt"
    );
    ask(&mut a, 2, testing::answered("q1", "main"));
    assert!(a.open.is_empty());
    assert_eq!(a.input(), "check the logs first", "the line is back");
}

/// Esc the question away, start typing, alt-a to reread it, Enter: the question is answered with
/// its marked row and the typed line survives.
#[test]
fn a_question_brought_back_over_typed_words_holds_them() {
    let mut a = app();
    ask(
        &mut a,
        1,
        testing::asked("q1", "which branch?", &["main", "release"], ""),
    );
    a.key(Key::Esc);
    assert!(a.open.is_empty(), "put away");
    typed(&mut a, "check the logs first");
    a.key(Key::AltA);
    assert_eq!(a.open[0].req_id, "q1", "it came back");
    assert_eq!(a.input(), "");
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::AnswerQuestion {
            req_id: "q1".into(),
            answer: QuestionAnswer::choosing(0),
        })
    );
    // Put away again instead: the words still come back.
    let mut b = app();
    ask(
        &mut b,
        1,
        testing::asked("q1", "which branch?", &["main", "release"], ""),
    );
    b.key(Key::Esc);
    typed(&mut b, "my prompt");
    b.key(Key::AltA);
    typed(&mut b, "rel");
    b.key(Key::Esc);
    assert_eq!(
        b.input(),
        "my prompt",
        "the held line, not the card's draft"
    );
    b.key(Key::AltA);
    assert_eq!(b.input(), "rel", "the card's own draft comes back with it");
}
