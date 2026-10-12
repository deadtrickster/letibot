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
/// The same row as the session wrote it before R90.24 — options on one ` | ` line, the shape
/// every stored transcript from before it holds.
const QUESTION_V1: &str = include_str!("fixtures/asked_question_row_v1.txt");
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
        "options:",
        "C — in scope, no dialect layer: count it all as engine failures",
        "Not now — leave the row open and the directories unenumerated",
    ] {
        assert!(holds(&whole, needle), "the popup lost `{needle}`: {whole}");
    }
    assert!(
        whole.contains("✓ 1. A — in scope, via a DuckDB dialect frontend"),
        "the operator's choice is marked: {whole}"
    );
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

/// **The question the operator pasted from the popup**, as the session writes it
/// (`Harness::question_row_text`): `asked: QUESTION`, then the options on one line joined with
/// ` | `.
const KLICK: &str = "asked: The klick-tick benchmark lives in a scratch copy. Where do you want \
    it — and do you want the README's \"Where we can go from here\" section updated with the \
    measured numbers?\n\
    options offered: Copy bench_test.go + BENCHMARKS.md into ~/Projects/klick-tick, leave README \
    alone | Copy both in and update the README's speed bullets with the measured numbers | Leave \
    it in scratch for now — I'll read it there first | Only send me the numbers; don't touch the \
    repo";
const K1: &str =
    "Copy bench_test.go + BENCHMARKS.md into ~/Projects/klick-tick, leave README alone";
const K2: &str = "Copy both in and update the README's speed bullets with the measured numbers";
const K3: &str = "Leave it in scratch for now — I'll read it there first";
const K4: &str = "Only send me the numbers; don't touch the repo";

/// A head holding the question row `question` and, when there is one, the operator's `answer`
/// row right after it — with the question's popup open and laid out at `cols`.
fn popup_for(question: &str, answer: Option<&str>, cols: usize) -> App {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    shell_row(
        &mut a,
        1,
        "s.1",
        "user",
        TranscriptItem::User {
            speaker: letibot_transcript::Speaker::Agent,
            parts: vec![UserPart::Text {
                text: question.into(),
            }],
        },
    );
    if let Some(said) = answer {
        shell_row(&mut a, 3, "s.2", "user", operator_row(said));
    }
    a.screen(cols, 30);
    let (y, _) = a.notice_rows[0];
    assert_eq!(a.key(Key::Click { x: 10, y: y as u16 }), None);
    assert!(a.diff_popup.is_some(), "the click opened the popup");
    a.screen(cols, 30);
    a
}

/// The popup's body rows, laid out.
fn laid(a: &App) -> Vec<rano::render::Line> {
    a.diff_popup
        .as_ref()
        .and_then(|p| p.laid.as_ref())
        .expect("the popup was laid out")
        .rows
        .clone()
}

fn plain(a: &App) -> Vec<String> {
    laid(a).iter().map(|l| l.plain()).collect()
}

/// The rows from `options:` on.
fn options_block(a: &App) -> Vec<String> {
    let rows = plain(a);
    let at = rows
        .iter()
        .position(|r| r == "options:")
        .unwrap_or_else(|| panic!("no options: {rows:#?}"));
    rows[at..].to_vec()
}

/// **The options are one per line, and the one the operator chose is marked.** The operator,
/// pasting this popup with every option on one `options offered:` line: *"here - lets render
/// options offered vertically, and mark what i chose"*.
#[test]
fn the_popup_draws_the_options_vertically_and_marks_the_choice() {
    let a = popup_for(KLICK, Some(K2), 200);
    assert_eq!(
        options_block(&a),
        vec![
            "options:".to_string(),
            format!("  1. {K1}"),
            format!("✓ 2. {K2}"),
            format!("  3. {K3}"),
            format!("  4. {K4}"),
        ]
    );
    let rows = plain(&a);
    assert!(
        rows[0].starts_with("asked: The klick-tick benchmark"),
        "{rows:#?}"
    );
    assert!(
        !rows.iter().any(|r| r.contains("options offered")),
        "{rows:#?}"
    );
    // The mark is the head's success mark, in the success colour.
    let marked = laid(&a)
        .into_iter()
        .find(|l| l.plain().starts_with("✓ 2."))
        .expect("the marked row");
    assert_eq!(
        marked.spans[0],
        rano::render::Span::role("✓ ", rano::style::Role::Success)
    );
}

/// **Before an answer exists, nothing is marked** — the options are still one per line.
#[test]
fn an_unanswered_question_marks_nothing() {
    let a = popup_for(KLICK, None, 200);
    assert_eq!(
        options_block(&a),
        vec![
            "options:".to_string(),
            format!("  1. {K1}"),
            format!("  2. {K2}"),
            format!("  3. {K3}"),
            format!("  4. {K4}"),
        ]
    );
}

/// **A note on the chosen option is drawn under it, faint**, at the option's text.
#[test]
fn a_note_is_drawn_faint_under_the_choice() {
    let a = popup_for(KLICK, Some(&format!("{K3}\nonly until friday")), 200);
    assert_eq!(
        options_block(&a),
        vec![
            "options:".to_string(),
            format!("  1. {K1}"),
            format!("  2. {K2}"),
            format!("✓ 3. {K3}"),
            "     only until friday".to_string(),
            format!("  4. {K4}"),
        ]
    );
    let note = laid(&a)
        .into_iter()
        .find(|l| l.plain().contains("only until friday"))
        .expect("the note row");
    assert_eq!(
        note.spans[1].style,
        rano::render::Style::of(rano::style::Role::Faint)
    );
}

/// **A typed answer marks no option** and is quoted after the list.
#[test]
fn free_text_marks_nothing_and_is_quoted_after_the_list() {
    let a = popup_for(KLICK, Some("put it in ~/bench instead"), 200);
    let block = options_block(&a);
    assert!(!block.iter().any(|r| r.starts_with('✓')), "{block:#?}");
    assert_eq!(
        &block[5..],
        &[
            "".to_string(),
            "your answer: \"put it in ~/bench instead\"".to_string()
        ]
    );
}

/// **An abstention marks no option** and says so after the list.
#[test]
fn an_abstention_says_so() {
    let a = popup_for(KLICK, Some("abstain"), 200);
    let block = options_block(&a);
    assert!(!block.iter().any(|r| r.starts_with('✓')), "{block:#?}");
    assert_eq!(&block[5..], &["".to_string(), "you abstained".to_string()]);
}

/// **An option holding ` | `** is cut in two by the split (the session does not escape the
/// separator); when it is the chosen one, the answer row spells it whole and the popup puts it
/// back together.
#[test]
fn a_chosen_option_holding_the_separator_is_one_option() {
    let q = "asked: which pipe?\noptions offered: a | b | c | d";
    let a = popup_for(q, Some("b | c"), 200);
    assert_eq!(
        options_block(&a),
        vec!["options:", "  1. a", "✓ 2. b | c", "  3. d"]
    );
}

/// **R90.24: the options are a list**, so an option holding ` | ` is one option — and a row
/// written before the list shape still reads as it did.
#[test]
fn the_options_are_a_list_and_the_old_line_still_parses() {
    use crate::ui::transcript::asked_card::question_of;
    let (_, now) = question_of(QUESTION).expect("the session's row");
    let (_, then) = question_of(QUESTION_V1).expect("a stored row");
    assert_eq!(now.len(), 4);
    assert_eq!(now, then);
    let q = "asked: which pipe?\noptions offered:\n- a\n- b | c\n- d";
    let a = popup_for(q, Some("b | c"), 200);
    assert_eq!(
        options_block(&a),
        vec!["options:", "  1. a", "✓ 2. b | c", "  3. d"]
    );
}

/// **Narrow, each option wraps under its own text** — a hanging indent past its number, and the
/// mark stays on the option's first row.
#[test]
fn a_narrow_popup_wraps_each_option_under_its_text() {
    let a = popup_for(KLICK, Some(K2), 40);
    let block = options_block(&a);
    let inner = 40 - 4;
    for r in &block {
        assert!(
            visible_width(r) <= inner,
            "`{r}` is wider than {inner}: {block:#?}"
        );
    }
    let two = block
        .iter()
        .position(|r| r.starts_with("✓ 2. Copy both in"))
        .unwrap_or_else(|| panic!("{block:#?}"));
    assert!(
        block[two + 1].starts_with("     ") && !block[two + 1].trim().is_empty(),
        "{block:#?}"
    );
    assert!(
        block[two + 1]
            .trim_start()
            .starts_with(|c: char| c.is_alphanumeric() || c == '\'')
    );
    let three = block
        .iter()
        .position(|r| r.starts_with("  3. "))
        .unwrap_or_else(|| panic!("{block:#?}"));
    // Everything between the marked row and option 3 is option 2's text, indented under it.
    let wrapped: String = block[two..three]
        .iter()
        .map(|r| r[r.char_indices().nth(5).map_or(r.len(), |(i, _)| i)..].to_string())
        .collect::<Vec<_>>()
        .join(" ");
    assert_eq!(wrapped, K2);
    assert!(block[two + 1..three].iter().all(|r| r.starts_with("     ")));
}
