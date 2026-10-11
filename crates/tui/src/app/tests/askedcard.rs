//! **A question to a person is one line, and their answer says what it answers.**
//!
//! The operator, pasting an `ask_user_question` card, the `session · asked:` row with three
//! paragraphs of question and its options, and their own answer under it: *"again, too much on
//! my screen, i saw the resulting selector anyway. but what is interesting - my reply doesnt
//! have any hint on the question. lets mention the question i replied to"*. See
//! `ui::transcript::asked_card`.

use super::redcard::{holds, popup_text, rows_with};
use super::*;

/// The question row as the session writes it (`Harness::question_row_text`) — pinned byte for
/// byte by `harnessd`'s `the_tuis_asked_card_fixtures_are_what_the_session_writes`.
const QUESTION: &str = include_str!("fixtures/asked_question_row.txt");
/// The operator's answer row (`Harness::operator_answer_text`), pinned by the same test.
const ANSWER: &str = include_str!("fixtures/asked_answer_row.txt");

/// What the tool's result says, the shape the operator pasted: the answer, attributed, and the
/// note to the model — nine lines.
const PAYLOAD: &str = "answer: A — in scope, via a DuckDB dialect frontend\n\
    [note] this answer is attributed to dead; it is what they said, not what the harness \
    inferred. A note qualifies the option it came with — do not read it as a separate answer.\n\
    question: Row 4: DuckDB's generated `slt_good_*` family is 605 files\n\
    options offered: A | B | C | Not now\n\
    chosen: 1 of 4\n\
    by: dead\n\
    at: 00:14:11\n\
    outcome: ok\n\
    end";

const LINE: &str = "asked · Row 4: DuckDB's generated slt_good_* family is 605 files under \
                    test/random and test/index… · 4 options · click for details";
const RE: &str = "re: \"Row 4: DuckDB's generated slt_good_* family is 605 files under \
                  test/random and test/index…\"";

/// A head attached to `s` with the exchange the operator pasted: the call's result, the
/// question row, and their answer.
fn with_exchange() -> App {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    shell_row(
        &mut a,
        1,
        "s.0",
        "tool_result",
        TranscriptItem::ToolResult {
            call_id: "c1".into(),
            name: "ask_user_question".into(),
            outcome: letibot_transcript::ToolOutcome::Ok,
            payload: PAYLOAD.into(),
            edit: None,
            origin: Some(letibot_transcript::CallOrigin::Operator { who: "dead".into() }),
            media: None,
        },
    );
    shell_row(
        &mut a,
        3,
        "s.1",
        "user",
        TranscriptItem::User {
            speaker: letibot_transcript::Speaker::Agent,
            parts: vec![UserPart::Text {
                text: QUESTION.into(),
            }],
        },
    );
    shell_row(&mut a, 5, "s.2", "user", operator_row(ANSWER));
    a
}

/// **The question is ONE line**: the first clause of its first sentence, how many options it
/// offered, and where the rest is — none of the paragraphs, none of the options' words.
#[test]
fn the_question_draws_as_one_line() {
    let mut a = with_exchange();
    let screen = a.screen(200, 40);
    let joined = screen.join("\n");
    assert_eq!(rows_with(&screen, LINE).len(), 1, "{joined}");
    for gone in [
        "nothing judges them",
        "options offered",
        "B — exclude it",
        "treat them as in scope",
    ] {
        assert!(!joined.contains(gone), "`{gone}` is drawn: {joined}");
    }
    assert_eq!(
        a.notice_rows.len(),
        1,
        "one clickable line: {:?}",
        a.notice_rows
    );
}

/// **The answer names the question it answers**, on a faint line under the operator's own
/// words, which are drawn as they always were.
#[test]
fn the_answer_says_what_it_answers() {
    let mut a = with_exchange();
    let screen = a.screen(200, 40);
    let joined = screen.join("\n");
    let answer = screen
        .iter()
        .position(|l| l.contains("▌ A — in scope, via a DuckDB dialect frontend"))
        .unwrap_or_else(|| panic!("the answer is drawn: {joined}"));
    assert!(
        screen[answer + 1].contains(&format!("▌ {RE}")),
        "the line under the answer names the question: {joined}"
    );
    // Narrow: cut, never wrapped.
    let screen = a.screen(60, 40);
    let re = rows_with(&screen, "▌ re: \"Row 4");
    assert_eq!(re.len(), 1, "{}", screen.join("\n"));
    assert!(re[0].contains('…') && visible_width(re[0]) <= 60, "{re:?}");
}

/// **An operator's row after anything else names nothing** — only the session's question
/// right before it is what it answers.
#[test]
fn a_reply_to_nothing_is_drawn_as_before() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    shell_row(&mut a, 1, "s.0", "user", operator_row(ANSWER));
    let joined = a.screen(200, 20).join("\n");
    assert!(joined.contains("A — in scope"), "{joined}");
    assert!(!joined.contains("re: "), "{joined}");
}

/// **A click on the question opens it whole**, every paragraph and every option; End reaches
/// the options, Esc closes it.
#[test]
fn a_click_on_the_question_opens_it_whole() {
    let mut a = with_exchange();
    let before = a.screen(120, 20);
    let (y, _) = a.notice_rows[0];
    assert_eq!(a.key(Key::Click { x: 10, y: y as u16 }), None);
    assert!(a.diff_popup.is_some(), "the click opened the popup");
    let edge = a.screen(120, 20).join("\n");
    assert!(edge.contains("asked · Row 4"), "{edge}");
    let whole = popup_text(&mut a);
    for needle in [
        "asked: Row 4: DuckDB's generated `slt_good_*` family is 605 files",
        "so nothing judges them.",
        "B — exclude the family, with the reason written down in the gate",
        "options offered: A — in scope, via a DuckDB dialect frontend | B — exclude it, reason \
         written down | C — in scope, no dialect layer: count it all as engine failures | Not \
         now — leave the row open and the directories unenumerated",
    ] {
        assert!(holds(&whole, needle), "the popup lost `{needle}`: {whole}");
    }
    a.screen(120, 20);
    assert_eq!(a.key(Key::End), None);
    let end = a.screen(120, 20).join("\n");
    assert!(end.contains("unenumerated"), "{end}");
    assert_eq!(a.key(Key::Esc), None);
    assert!(a.diff_popup.is_none());
    assert_eq!(a.screen(120, 20), before);
}

/// **The question's tool card is its header**, folded: no `[note]` lines and no seam — the
/// exchange is the two rows under it. `/t` unfolds it as it unfolds every card.
#[test]
fn the_question_card_is_its_header_folded() {
    let mut a = with_exchange();
    let screen = a.screen(200, 40);
    let joined = screen.join("\n");
    let head = rows_with(&screen, "ask_user_question");
    assert_eq!(head.len(), 1, "{joined}");
    assert!(
        head[0].contains("9 lines"),
        "the header still counts: {head:?}"
    );
    assert!(!joined.contains("[note]"), "{joined}");
    assert!(!joined.contains("+8 lines"), "{joined}");
    // A model's other cards keep their preview and seam (`model_bash_rows`).
    let mut b = app();
    b.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    model_bash_rows(&mut b, 1, "s.0", PAYLOAD);
    let other = b.screen(200, 40).join("\n");
    assert!(
        other.contains("+8 lines") || other.contains("[note]"),
        "{other}"
    );
    // Unfolded, the payload is there.
    a.tools = Fold::Open;
    a.refold();
    let open = a.screen(200, 60).join("\n");
    assert!(open.contains("[note]"), "{open}");
}
