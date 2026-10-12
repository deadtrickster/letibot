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

// ──────────────── R90.2 / R90.23: a row number marks a numbered row ────────────────

/// `16` typed as a free answer to "how many workers?" answered choice 1 at the `1`. A digit now
/// marks the row it names — and the numbers are drawn — and only Enter answers.
#[test]
fn a_digit_on_a_question_marks_its_numbered_row_and_enter_answers() {
    let mut a = app();
    ask(
        &mut a,
        1,
        testing::asked("q1", "how many workers?", &["2", "4", "8"], ""),
    );
    let card = a.decision_lines(&a.open[0].clone(), 80).join("\n");
    assert!(card.contains("▸ 1  2"), "{card}");
    assert!(card.contains("  3  8"), "{card}");
    assert!(card.contains("a row number marks"), "{card}");
    assert_eq!(a.key(Key::Char('3')), None, "a digit answers nothing");
    assert_eq!(a.sel, 2);
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::AnswerQuestion {
            req_id: "q1".into(),
            answer: QuestionAnswer::choosing(2),
        })
    );
    // On a permission the same: the bar says so, and the rows carry their numbers.
    let mut b = app();
    ask(&mut b, 1, testing::requested("r1", "rm"));
    let card = b.decision_lines(&b.open[0].clone(), 80).join("\n");
    assert!(
        card.contains("▸ 1  Allow once") && card.contains("2  Deny"),
        "{card}"
    );
    assert!(b.hint_bar(200).contains("a row number marks"));
    assert_eq!(b.key(Key::Char('2')), None);
    assert_eq!(b.key(Key::Enter), answer("r1", "deny"));
}

/// A question with no choices has no ladder to walk; an empty choice is not a blank row.
#[test]
fn a_question_without_choices_offers_no_arrows_and_an_empty_choice_is_named() {
    let mut a = app();
    ask(
        &mut a,
        1,
        testing::asked("q1", "what is it called?", &[], ""),
    );
    let card = a.decision_lines(&a.open[0].clone(), 80).join("\n");
    assert!(!card.contains("↑↓"), "{card}");
    assert!(card.contains("type your answer"), "{card}");
    assert!(!a.hint_bar(200).contains("↑↓"), "{}", a.hint_bar(200));
    typed(&mut a, "an old prompt");
    a.key(Key::CtrlC);
    assert_eq!(a.key(Key::Up), None);
    assert_eq!(a.input(), "", "↑ does not recall history into the answer");
    assert_eq!(a.key(Key::Enter), None);
    assert!(
        a.notice
            .as_deref()
            .unwrap_or("")
            .contains("type your answer")
    );

    let mut b = app();
    ask(&mut b, 1, testing::asked("q1", "suffix?", &["", "-v2"], ""));
    let card = b.decision_lines(&b.open[0].clone(), 80).join("\n");
    assert!(card.contains("▸ 1  (an empty answer)"), "{card}");
}

// ───────────────────── R90.5: the prompt card has a way back ─────────────────────

/// Esc puts the prompt card away; a resident line says the command waits, and alt-a brings the
/// card back.
#[test]
fn the_prompt_card_put_away_has_a_resident_line_and_alt_a_brings_it_back() {
    let mut a = app();
    ask(
        &mut a,
        1,
        SessionEvent::PromptRequested {
            req_id: "p1".into(),
            job: "j1".into(),
            command: "sudo apt install mc".into(),
            question: Some("Continue? [Y/n]".into()),
            reading: letibot_sessionlog::PromptReading::Blocked,
        },
    );
    a.key(Key::Esc);
    assert!(a.prompt_away);
    let line = a.waiting_line(200).join("");
    assert!(
        line.contains("YOUR COMMAND IS WAITING") && line.contains("alt-a"),
        "{line}"
    );
    a.key(Key::AltA);
    assert!(!a.prompt_away, "alt-a brought the card back");
    assert!(a.waiting_line(200).is_empty());
    typed(&mut a, "y");
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::PromptAnswer {
            req_id: "p1".into(),
            line: "y".into()
        })
    );
}
