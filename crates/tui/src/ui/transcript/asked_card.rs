//! **A question the model asked a person, drawn as ONE line** — and named on the person's
//! answer, so the reply says what it replies to.
//!
//! The operator, pasting what one `ask_user_question` left on the screen — the tool card with
//! its `[note]` lines, the `session · asked:` row with three paragraphs of question and its
//! options, and their own answer under it: *"again, too much on my screen, i saw the resulting
//! selector anyway. but what is interesting - my reply doesnt have any hint on the question.
//! lets mention the question i replied to"*.
//!
//! # The lines
//!
//! ```text
//! session · asked · Row 4: DuckDB's generated slt_good_* family is 605 files under test/random and test/index… · 4 options · click for details
//! ▌ A — in scope, via a DuckDB dialect frontend
//! ▌ re: "Row 4: DuckDB's generated slt_good_* family is 605 files under test/random and test/index…"
//! ```
//!
//! The question row (`Harness::question_row_text`, `asked: QUESTION` and, when it offered any,
//! `\noptions offered: A | B | …`) is a card in `line_cards`' registry: its summary is the first
//! clause of the question's first sentence ([`summary`]), backticks taken off, and a click opens
//! the question whole, options included. The operator already answered it in the selector, so
//! the transcript's copy only has to be findable.
//!
//! The answer row is the operator's verbatim words (`Harness::operator_answer_text`), which the
//! trail cites, so its text is not touched: the head pairs it with the question row the session
//! writes immediately before it (`asked_rows` writes the two together) and draws a faint `re:`
//! line under it. Nothing on the wire changed for this — the pairing is the head's, read off two
//! rows it already has.

use rano::style::Role;

use crate::ui::transcript::line_cards::{CLICK, Card, SessionKind, first_clause, flat, sentences};

/// What the question row opens with.
const ASKED: &str = "asked: ";
/// Where its options start, when it offered any.
const OPTIONS: &str = "\noptions offered: ";

/// **A question row, read**: the question, and the options it offered.
pub(crate) fn question_of(text: &str) -> Option<(&str, Vec<&str>)> {
    let rest = text.strip_prefix(ASKED)?;
    let (question, options) = match rest.rsplit_once(OPTIONS) {
        Some((q, o)) if !o.contains('\n') => (q, o.split(" | ").collect()),
        _ => (rest, Vec::new()),
    };
    (!question.trim().is_empty()).then_some((question, options))
}

/// **What a line says of a question**: the first clause of its first sentence, flattened, its
/// backticks taken off (a code span is a style the line cannot draw).
pub(crate) fn summary(question: &str) -> Option<String> {
    let text = flat(&question.replace('`', ""));
    let first = *sentences(&text).first()?;
    Some(first_clause(first))
}

/// **The question in the card registry** (`line_cards`). It is a row of its own — the session
/// writes it apart from every notice — so it has no last words to split at.
pub(crate) const SESSION: SessionKind = SessionKind {
    opens: opens_question,
    ends: &[],
    read: question_card,
};

fn opens_question(text: &str) -> bool {
    question_of(text).is_some()
}

fn question_card(text: &str) -> Option<Card> {
    let (question, options) = question_of(text)?;
    let summary = summary(question)?;
    let mut parts = vec!["asked".to_string(), summary.clone()];
    match options.len() {
        0 => {}
        1 => parts.push("1 option".into()),
        n => parts.push(format!("{n} options")),
    }
    parts.push(CLICK.into());
    Some(Card::new(
        parts.join(" · "),
        0,
        "asked",
        Role::Attention,
        format!("asked · {summary}"),
    ))
}

/// **The faint line under an answer**: `re: "<the question's summary>"`, or `None` when the row
/// before the answer is not a question.
pub(crate) fn replied_to(question_row: &str) -> Option<String> {
    let (question, _) = question_of(question_row)?;
    Some(format!("re: \"{}\"", summary(question)?))
}
