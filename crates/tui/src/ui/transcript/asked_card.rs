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

/// **What the operator did with a question**, read off their answer row
/// (`Harness::operator_answer_text`) and the options the question row offered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Answered {
    /// They chose option `index` (from 0), and qualified it with `note` when they did.
    Chose { index: usize, note: Option<String> },
    /// They typed an answer of their own: no option is theirs.
    Free(String),
    /// They deliberately did not answer.
    Abstained,
}

/// **A question row and the answer under it, read together**: the question, its options (the
/// ones an option's own ` | ` split apart put back together, where the answer shows how), and
/// what the operator did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Exchange {
    pub(crate) question: String,
    pub(crate) options: Vec<String>,
    pub(crate) answered: Option<Answered>,
}

/// **Read a question row and, when there is one, the answer row the session wrote right after
/// it.**
///
/// The answer row is the operator's verbatim words, so what they did is read back from its
/// shape (`operator_answer_text`): an option's own text on the first line, then their note on
/// the lines after it; anything else they typed; or the one word `abstain`.
///
/// **The ` | ` between options is not escaped** — the session joins them with it and an option
/// may hold one. So the split is a guess, and the answer corrects it where it can: a chosen
/// option that the split cut in two is matched against the answer's first line and put back
/// together. An option nobody chose that holds ` | ` is still drawn as two.
pub(crate) fn exchange(question_row: &str, answer: Option<&str>) -> Option<Exchange> {
    let (question, pieces) = question_of(question_row)?;
    let mut options: Vec<String> = pieces.iter().map(|p| p.to_string()).collect();
    let answered = answer.map(|said| {
        let (first, rest) = said.split_once('\n').unwrap_or((said, ""));
        let note = Some(rest.trim())
            .filter(|n| !n.is_empty())
            .map(str::to_string);
        if let Some(index) = pieces.iter().position(|p| *p == first) {
            return Answered::Chose { index, note };
        }
        // The chosen option held a ` | `: the shortest run of pieces that spells it.
        for len in 2..=pieces.len() {
            if let Some(i) =
                (0..=pieces.len() - len).find(|&i| pieces[i..i + len].join(" | ") == first)
            {
                options.splice(i..i + len, [first.to_string()]);
                return Answered::Chose { index: i, note };
            }
        }
        if said.trim() == "abstain" {
            Answered::Abstained
        } else {
            Answered::Free(said.trim().to_string())
        }
    });
    Some(Exchange {
        question: question.to_string(),
        options,
        answered,
    })
}

/// The mark on the option the operator chose — the head's success mark (`✓`, as the queue
/// pane's passed gate wears it).
const CHOSEN: &str = "✓ ";

/// **The question whole, its options one per line, the operator's choice marked** — the popup a
/// click on the one-line card opens.
///
/// The operator, pasting this popup with the options on one `options offered: A | B | C | D`
/// line: *"here - lets render options offered vertically, and mark what i chose"*.
///
/// ```text
/// asked: The klick-tick benchmark lives in a scratch copy. Where do you want it — …
///
/// options:
///   1. Copy bench_test.go + BENCHMARKS.md into ~/Projects/klick-tick, leave README alone
/// ✓ 2. Copy both in and update the README's speed bullets with the measured numbers
///   3. Leave it in scratch for now — I'll read it there first
///   4. Only send me the numbers; don't touch the repo
/// ```
///
/// Each option wraps under its own text (a hanging indent past its number). A note they added
/// to the option they chose is drawn faint under it; a typed answer is `your answer: "…"` after
/// the list, with no option marked; an abstention is `you abstained`. Before an answer exists,
/// nothing is marked. `None` when `question_row` is not a question.
pub(crate) fn full_lines(
    question_row: &str,
    answer: Option<&str>,
    width: usize,
) -> Option<Vec<rano::render::Line>> {
    use crate::ui::render::wrap;
    use letibot_ui::text::without_control;
    use rano::render::{Line, Span};

    let ex = exchange(question_row, answer)?;
    let width = width.max(4);
    let mut rows: Vec<Line> = Vec::new();
    let text = |rows: &mut Vec<Line>, s: &str, role: Option<Role>| {
        for l in s.lines() {
            if l.is_empty() {
                rows.push(Line::default());
                continue;
            }
            for w in wrap(&without_control(l), width) {
                rows.push(match role {
                    Some(r) => Line::styled(w, r),
                    None => Line::raw(w),
                });
            }
        }
    };
    text(&mut rows, &format!("{ASKED}{}", ex.question), None);
    let chose = match &ex.answered {
        Some(Answered::Chose { index, note }) => Some((*index, note.as_deref())),
        _ => None,
    };
    if !ex.options.is_empty() {
        rows.push(Line::default());
        rows.push(Line::raw("options:"));
        let digits = ex.options.len().to_string().len();
        // `✓ 12. ` — the mark's two columns, the number, `. `.
        let hang = CHOSEN.chars().count() + digits + 2;
        let room = width.saturating_sub(hang).max(4);
        for (i, option) in ex.options.iter().enumerate() {
            let marked = chose.is_some_and(|(c, _)| c == i);
            let number = format!("{:>digits$}. ", i + 1);
            for (n, w) in wrap(&without_control(option), room).into_iter().enumerate() {
                let lead = match (n, marked) {
                    (0, true) => Span::role(CHOSEN, Role::Success),
                    _ => Span::raw("  "),
                };
                let number = if n == 0 {
                    number.clone()
                } else {
                    " ".repeat(digits + 2)
                };
                rows.push(Line::new(vec![lead, Span::raw(number), Span::raw(w)]));
            }
            if let Some((_, Some(note))) = chose.filter(|_| marked) {
                for l in note.lines().filter(|l| !l.trim().is_empty()) {
                    for w in wrap(&without_control(l), room) {
                        rows.push(Line::new(vec![
                            Span::raw(" ".repeat(hang)),
                            Span::role(w, Role::Faint),
                        ]));
                    }
                }
            }
        }
    }
    match &ex.answered {
        Some(Answered::Free(said)) => {
            rows.push(Line::default());
            text(&mut rows, &format!("your answer: \"{said}\""), None);
        }
        Some(Answered::Abstained) => {
            rows.push(Line::default());
            text(&mut rows, "you abstained", None);
        }
        _ => {}
    }
    Some(rows)
}

/// **The operator's answer to the question at `k`**: the row right after it, when that row is
/// theirs — the same pairing [`replied_to`] draws the other way (`item::question_before`).
pub(crate) fn answer_after(
    items: &[letibot_sessionlog::view::SnapshotItem],
    k: usize,
) -> Option<String> {
    use letibot_transcript::{Speaker, TranscriptItem};
    match items.get(k + 1)?.item.as_ref()? {
        TranscriptItem::User {
            parts,
            speaker: Speaker::Operator,
        } => Some(crate::ui::transcript::user::user_text(parts)),
        _ => None,
    }
}
