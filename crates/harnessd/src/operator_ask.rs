//! **A todo row that waits on the OPERATOR** — the daemon's half: the answer, the words the model
//! is told, and the loud state the heads draw.
//!
//! The operator's ask, verbatim: *"a todo can be dependent on todo, and todo can be dependend on
//! ME. if model says todo is dependent on ME the other heads of the todo DAG keep firing, model
//! told to try to figure it out anyways, and the heads show loud notification that im waited. i
//! guess we will force models to do choice for todo - so when it says it waits for ME it give me
//! that choice list with an optioin to enter my freetext. and then when i submit todo becomes
//! unblocked with reply attached"*.
//!
//! # The edge
//!
//! `TodoNeed::Operator { question, options }` on a row (the store's doc says what it is), set by
//! `todo_write` — refused with fewer than two options, *"we will force models to do choice"*. The
//! plan check treats it as blocked and never asks about the row; the rest of the plan keeps being
//! asked about, and the blocked line tells the model to keep going and to try to work the question
//! out itself (`detach` takes the wait off when it does). A plan whose only open work waits on the
//! operator is silent: nothing there is the model's to unblock.
//!
//! # Loud on the heads
//!
//! While any open row waits on the operator: the hub carries a `waiting_on_operator` row when a
//! question is asked (and `operator_answered` when it is settled), the settings row `asks` says
//! how many and quotes the first, the TUI draws a resident line with the key that opens the card,
//! the todo pane marks the row `waits on you`, and the session picker marks the session (from the
//! store's board, so a session nobody has opened since a restart is marked too).
//!
//! # The answer
//!
//! `ClientFrame::AnswerTodo` carries the row's words, the question, and a `QuestionAnswer` — the
//! card's vocabulary: an option, an option with a note, typed words, or an abstention. [`settle`]
//! validates it against the row's OWN options (`QuestionAnswer::validate`), replaces the edge with
//! `TodoNeed::Answered` carrying the reply, and returns the line the model is told AT ONCE: as a
//! steering message at the next step boundary when a turn is running, and as a turn of its own when
//! the session is idle — R79's rule for an operator's words, which are never held.

use letibot_sessionlog::question::QuestionAnswer;
use letibot_tokencore::store::{TodoItem, operator_asks};
use letibot_tools::builtins::todo::{OperatorAsk, TodoBoard};

/// **The reply, as a sentence** — what the row keeps and what the model is told after the
/// question. The option is quoted by its WORDS, not its number: the model offered the options, and
/// a number is a thing it would have to look up.
pub fn reply_words(ask: &OperatorAsk, answer: &QuestionAnswer) -> String {
    if answer.abstain {
        return "the operator abstained — they will not decide this one".into();
    }
    let note = answer
        .note
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty());
    let free = answer
        .free
        .as_deref()
        .map(str::trim)
        .filter(|f| !f.is_empty());
    match (answer.option.and_then(|i| ask.options.get(i)), note, free) {
        (Some(chosen), Some(note), _) => {
            format!("the operator chose \"{chosen}\" (note: {note})")
        }
        (Some(chosen), None, _) => format!("the operator chose \"{chosen}\""),
        (None, _, Some(free)) => format!("the operator wrote: \"{free}\""),
        // `validate` refuses every other shape before this is reached.
        (None, _, None) => "the operator answered with nothing".into(),
    }
}

/// **The line the model is told** — `[answer] on "<row>": you asked "<question>" — <reply>.`, and
/// what to do with an abstention, which is an answer and not permission: the row is unblocked
/// either way (the operator has answered), and an abstaining operator has said they will not
/// decide, so the model states the assumption it makes.
pub fn answer_line(row: &str, question: &str, reply: &str, abstained: bool) -> String {
    let mut line = format!(
        "[answer] on \"{}\": you asked \"{}\" — {}. The row no longer waits on the operator.",
        row.trim(),
        question.trim(),
        reply
    );
    if abstained {
        line.push_str(" They will not decide, so proceed and state the assumption you proceed on.");
    }
    line
}

/// **Apply the operator's answer to the board** — validated against the row's own options, the
/// edge replaced by the reply, and the line for the model returned. `Err` is a sentence for the
/// operator: no such row, no such question open any more, or an answer the card should not have
/// let through (an option the row never offered, a note with no option, an empty answer).
pub fn settle(
    board: &TodoBoard,
    content: &str,
    question: &str,
    answer: &QuestionAnswer,
) -> Result<String, String> {
    let ask = board.operator_ask(content, question)?;
    answer
        .validate(ask.options.len())
        .map_err(|defect| format!("the answer was not taken: {defect}"))?;
    let reply = reply_words(&ask, answer);
    board.answer_operator(&ask.content, &ask.question, &reply)?;
    Ok(answer_line(
        &ask.content,
        &ask.question,
        &reply,
        answer.abstain,
    ))
}

/// **What waits on the operator now**, as `(row, question)` pairs — the reading the daemon compares
/// across a flush to say when a question was asked and when it was settled.
pub fn waiting(todos: &[TodoItem]) -> Vec<(String, String)> {
    operator_asks(todos)
        .into_iter()
        .map(|(row, q, _)| (row.content.trim().to_string(), q.to_string()))
        .collect()
}

/// **The settings row's value** — `none`, or `2 — "which database?" on \`deploy\``. The head keys
/// its resident line on anything but `none`, and draws its own words from its board when it has
/// one; this is the sentence for a head that has not read the board yet.
pub fn setting_value(todos: &[TodoItem]) -> String {
    let asks = operator_asks(todos);
    match asks.first() {
        None => "none".into(),
        Some((row, q, _)) => format!(
            "{} — \"{}\" on `{}`",
            asks.len(),
            q.trim(),
            letibot_tokencore::store::todo_title(&row.content)
        ),
    }
}

/// **The hub row for a question just asked** — loud, because a person is being waited on.
pub fn asked_words(row: &str, question: &str) -> String {
    format!(
        "WAITING ON YOU: the model asked \"{}\" on the row `{}` — open the question card to answer \
         it (the rest of the plan keeps going meanwhile)",
        question.trim(),
        letibot_tokencore::store::todo_title(row)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask() -> OperatorAsk {
        OperatorAsk {
            content: "deploy".into(),
            question: "which db?".into(),
            options: vec!["postgres".into(), "sqlite".into()],
        }
    }

    /// **Every shape a person can answer in is said in words** — *"that choice list with an optioin
    /// to enter my freetext"*, and the card's note and abstention beside it.
    #[test]
    fn each_answer_shape_reads_as_a_sentence() {
        assert_eq!(
            reply_words(&ask(), &QuestionAnswer::choosing(0)),
            "the operator chose \"postgres\""
        );
        assert_eq!(
            reply_words(
                &ask(),
                &QuestionAnswer::choosing(1).with_note("only for tests")
            ),
            "the operator chose \"sqlite\" (note: only for tests)"
        );
        assert_eq!(
            reply_words(
                &ask(),
                &QuestionAnswer::free("neither — use the existing one")
            ),
            "the operator wrote: \"neither — use the existing one\""
        );
        assert!(
            reply_words(&ask(), &QuestionAnswer::abstaining())
                .starts_with("the operator abstained")
        );
        let line = answer_line(
            "deploy",
            "which db?",
            "the operator chose \"postgres\"",
            false,
        );
        assert_eq!(
            line,
            "[answer] on \"deploy\": you asked \"which db?\" — the operator chose \"postgres\". \
             The row no longer waits on the operator."
        );
    }
}
