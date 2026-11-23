//! A card versus the line the operator is typing: a card that arrives over a draft takes neither
//! the keys nor the words, `tab` hands it the keyboard without the draft, and answering it gives
//! the draft back. Every case here is one the operator met in the wild - a permission or a
//! question arriving mid-sentence, and the only way to answer it being to delete what they wrote.

use super::*;

fn ask(a: &mut App, seq: u64, e: SessionEvent) {
    a.apply(ServerFrame::Event(env(seq, e)));
}

/// A card arriving over a draft. The line is not held, not cleared, not sent, and the digits stay
/// the composer's: the card itself says which key gives it the keyboard.
#[test]
fn a_card_arriving_over_a_draft_leaves_the_line_alone_and_answers_nothing() {
    let mut a = app();
    typed(&mut a, "hello");
    ask(&mut a, 1, testing::requested("r1", "rm"));

    assert_eq!(a.input(), "hello", "the card took the operator's line");
    assert!(a.pending_prompts.is_empty(), "a card sent something");
    assert!(
        !a.card_focus,
        "a card that arrived over a draft has no keys"
    );
    assert_eq!(a.open.len(), 1, "and it is still waiting");

    // The card says what to do about it, on the card and on the bar - one spelling.
    let card = a.decision_lines(&a.open[0].clone(), 100).join("\n");
    assert!(card.contains("tab answers this card"), "{card}");
    assert!(
        a.hint_bar(240).contains("tab answers this card"),
        "{}",
        a.hint_bar(240)
    );
    // A digit is the line's, not the ladder's: it types, and it marks nothing.
    assert_eq!(a.key(Key::Char('2')), None);
    assert_eq!(a.sel, 0, "a digit typed into the draft marked a rung");
    assert_eq!(a.input(), "hello2", "the digit did not reach the line");
}

/// The card is answerable without deleting the draft: `tab` takes the keyboard, the digit marks a
/// rung, Enter answers - and the draft comes back afterwards, exactly as it was.
#[test]
fn tab_answers_the_card_and_the_draft_comes_back_untouched() {
    let mut a = app();
    typed(&mut a, "hello");
    ask(&mut a, 1, testing::requested("r1", "rm"));

    assert_eq!(a.key(Key::Tab), None);
    assert!(a.card_focus, "tab did not hand the card the keys");
    assert_eq!(a.input(), "", "the line is the card's field now");
    assert_eq!(a.held_line.as_deref(), Some("hello"), "the line is kept");

    assert_eq!(
        a.key(Key::Char('2')),
        None,
        "a digit marks, it does not answer"
    );
    assert_eq!(a.sel, 1);
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::Answer {
            req_id: "r1".into(),
            option_id: "deny".into(),
            pattern: None,
            note: None,
        }),
        "the answer carries no words of the operator's"
    );
    assert_eq!(a.input(), "hello", "the draft did not come back");
    assert!(
        a.pending_prompts.is_empty(),
        "answering the card sent a message"
    );
    assert!(a.open.is_empty(), "the card is answered");
}

/// And the line the operator was writing goes where it was going: Enter sends it, alone, and never
/// answers a card.
#[test]
fn enter_sends_the_draft_alone_and_never_answers_a_card() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    typed(&mut a, "hello");
    ask(&mut a, 1, testing::requested("r1", "rm"));

    assert_eq!(
        a.key(Key::Enter),
        Some(Action::Prompt("hello".into())),
        "Enter in the composer sends the draft"
    );
    assert_eq!(a.open.len(), 1, "and answers no card");
    assert_eq!(a.input(), "", "the line was sent, not kept");
}

/// Text typed while the card has the keys is the card's: it becomes the reason the model is told,
/// and it never edits the line waiting behind it.
#[test]
fn words_typed_at_the_card_never_reach_the_draft() {
    let mut a = app();
    typed(&mut a, "hello");
    ask(&mut a, 1, testing::requested("r1", "rm"));
    assert_eq!(a.key(Key::Tab), None);

    // The refusal is the rung that takes words, so mark it first: the preview says where they go.
    assert_eq!(a.key(Key::Down), None);
    typed(&mut a, "not in prod");
    assert_eq!(a.input(), "not in prod", "the card's own field");
    let card = a.decision_lines(&a.open[0].clone(), 100).join("\n");
    assert!(card.contains("tells the model: not in prod"), "{card}");

    assert_eq!(
        a.key(Key::Enter),
        Some(Action::Answer {
            req_id: "r1".into(),
            option_id: "deny".into(),
            pattern: None,
            note: Some("not in prod".into()),
        })
    );
    assert_eq!(a.input(), "hello", "the draft the card arrived over");
}

/// The same for a question: a draft is not the answer, and is neither sent nor spent on one.
#[test]
fn a_question_over_a_draft_is_answered_from_the_card_and_the_draft_stays() {
    let mut a = app();
    typed(&mut a, "hello");
    ask(
        &mut a,
        1,
        testing::asked("q1", "which branch?", &["main", "release"], ""),
    );
    assert_eq!(a.input(), "hello");
    assert!(!a.card_focus);

    // The digits are the line's here too, since a question has no numbered rows to mark.
    assert_eq!(a.key(Key::Char('2')), None);
    assert_eq!(a.input(), "hello2");

    // Enter sends the line: it is not the card's answer, and the question is still open.
    assert!(matches!(a.key(Key::Enter), Some(Action::Prompt(_))));
    assert_eq!(a.open.len(), 1, "the draft answered the question");

    // `tab` hands the card the keys, the words typed there are its answer, and the line returns.
    let mut b = app();
    typed(&mut b, "hello");
    ask(
        &mut b,
        1,
        testing::asked("q1", "which branch?", &["main", "release"], ""),
    );
    assert_eq!(b.key(Key::Tab), None);
    assert_eq!(b.key(Key::Char('r')), None);
    let Some(Action::AnswerQuestion { req_id, answer }) = b.key(Key::Enter) else {
        panic!("the words typed at the card are its answer");
    };
    assert_eq!(req_id, "q1");
    assert_eq!(answer.free.as_deref(), Some("r"), "the card's own words");
    assert_eq!(b.input(), "hello", "the draft came back");
    assert!(b.open.is_empty(), "the question was answered");
}

/// Esc over a draft puts the card away and carries nothing with it: the line is not the card's
/// draft to stash, so an away-card does not hold a copy of it.
#[test]
fn esc_puts_the_card_away_and_leaves_the_draft_where_it_is() {
    let mut a = app();
    typed(&mut a, "hello");
    ask(&mut a, 1, testing::requested("r1", "rm"));
    assert_eq!(a.key(Key::Esc), None);
    assert!(a.open.is_empty(), "the card is put away");
    assert_eq!(a.input(), "hello", "the card took the line with it");
    assert_eq!(a.held_line, None, "the line was not held aside");
}

/// Today's behaviour is untouched where there is nothing to lose: a card that arrives over an empty
/// composer has the keys from the start, so a digit marks and Enter answers without `tab`.
#[test]
fn an_empty_composer_still_answers_a_card_on_its_digits() {
    let mut a = app();
    ask(&mut a, 1, testing::requested("r1", "rm"));
    assert!(a.card_focus, "a card over an empty composer owns the keys");
    assert_eq!(a.key(Key::Char('2')), None);
    assert_eq!(a.sel, 1);
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::Answer {
            req_id: "r1".into(),
            option_id: "deny".into(),
            pattern: None,
            note: None,
        })
    );
}

/// `tab` twice is round and round: the keys back to the card, then back to the line, with the
/// draft in the composer and the card still waiting on its answer.
#[test]
fn tab_gives_the_keys_back_to_the_line() {
    let mut a = app();
    typed(&mut a, "hello");
    ask(&mut a, 1, testing::requested("r1", "rm"));
    assert_eq!(a.key(Key::Tab), None);
    assert!(a.card_focus);
    assert_eq!(a.key(Key::Tab), None);
    assert!(!a.card_focus, "the line has the keys again");
    assert_eq!(a.input(), "hello");
    assert_eq!(a.open.len(), 1, "the card answers nothing on a tab");
    // And the ladder's own keys are not promised while it does not hold them.
    let card = a.decision_lines(&a.open[0].clone(), 100).join("\n");
    assert!(card.contains("tab answers this card"), "{card}");
    assert!(!card.contains("enter answers"), "{card}");
}

/// The setup cards own their own field already; what is pinned here is that they own nothing else.
/// A key card up over a draft takes what is typed at it, leaves the draft exactly as it was, and
/// hands the line back when it closes.
#[test]
fn a_key_card_over_a_draft_leaves_the_line_alone() {
    let mut a = app();
    typed(&mut a, "hello");
    a.key_ask = Some(crate::app::asks::KeyAsk {
        choice: "deepseek/deepseek-chat".into(),
        provider: "deepseek".into(),
    });
    typed(&mut a, "sk-123");
    assert_eq!(a.key_buf, "sk-123", "the key is the card's own field");
    assert_eq!(a.input(), "hello", "and the draft is not");
    assert_eq!(a.key(Key::Esc), None);
    assert!(a.key_ask.is_none(), "esc cancels the card");
    assert_eq!(a.input(), "hello");
    assert!(a.pending_prompts.is_empty(), "nothing was sent");
}

/// Two cards, one after the other: the second is answered from the card too, and the draft the
/// first one arrived over is still there at the end.
#[test]
fn a_second_card_behind_the_first_does_not_take_the_line_either() {
    let mut a = app();
    typed(&mut a, "hello");
    ask(&mut a, 1, testing::requested("r1", "rm"));
    ask(&mut a, 2, testing::requested("r2", "rm -rf"));
    assert_eq!(a.input(), "hello", "the queued card took the line");
    assert!(!a.card_focus);

    // The front card is answered from the card, and the one behind it takes the slot: the line is
    // already aside, so the second card has a field of its own and neither one spends the draft.
    assert_eq!(a.key(Key::Tab), None);
    assert!(
        a.key(Key::Enter).is_some(),
        "a bare answer to the front card"
    );
    assert_eq!(a.open[0].req_id, "r2", "the second card is the one up");
    assert!(
        a.key(Key::Enter).is_some(),
        "and it is answered the same way"
    );
    assert!(a.open.is_empty());
    assert_eq!(a.input(), "hello", "the line survived both");
    assert!(a.pending_prompts.is_empty(), "no card sent a message");
}
