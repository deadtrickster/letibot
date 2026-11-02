//! **A todo row that waits on the OPERATOR** — the daemon's half: the answer, the words the
//! model is told, and the loud state the heads draw. The operator's ask, verbatim: *"a todo
//! can be dependent on todo, and todo can be dependend on ME. if model says todo is
//! dependent on ME the other heads of the todo DAG keep firing, model told to try to figure
//! it out anyways, and the heads show loud notification that im waited. i guess we will
//! force models to do choice for todo - so when it says it waits for ME it give me that
//! choice list with an optioin to enter my freetext. and then when i submit todo becomes
//! unblocked with reply attached"*.

use letibot_sessionlog::question::QuestionAnswer;
use letibot_tokencore::store::{TodoItem, TodoNeed, TodoStatus, operator_asks};
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

/// **The line the model is told** — `[answer] on "<row>": you asked "<question>" —
/// <reply>.`, and what to do with an abstention, which is an answer and not permission:
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

/// **Apply the operator's answer to the board** — validated against the row's own options,
/// the edge replaced by the reply, and the line for the model returned. `Err` is a sentence
/// for the operator:
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

/// **Why a question stopped waiting**, in the sentence the settled notice carries — so a
/// reader can tell an ANSWER from a TAKE-BACK. Three routes end a question and only one of
/// them is a person's:
pub fn settled_words(todos: &[TodoItem], row: &str, question: &str) -> String {
    let tail = format!(
        "for \"{}\" on `{}`",
        question.trim(),
        letibot_tokencore::store::todo_title(row)
    );
    let Some(item) = todos.iter().find(|t| t.content.trim() == row.trim()) else {
        return format!("the row is gone, so nothing waits {tail}");
    };
    if item.needs.iter().any(|n| {
        matches!(n, TodoNeed::Answered { question: asked, .. } if asked.trim() == question.trim())
    }) {
        return format!("answered — no longer waiting on you {tail}");
    }
    match item.status {
        TodoStatus::Completed => format!("the row is done — no longer waiting on you {tail}"),
        TodoStatus::Cancelled => format!("the row was dropped — no longer waiting on you {tail}"),
        _ => format!("the model took its question back — no longer waiting on you {tail}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use letibot_tokencore::store::TodoBy;

    fn ask() -> OperatorAsk {
        OperatorAsk {
            content: "deploy".into(),
            question: "which db?".into(),
            options: vec!["postgres".into(), "sqlite".into()],
        }
    }

    /// A row of the board in the state a route left it in.
    fn row(status: TodoStatus, needs: Vec<TodoNeed>) -> TodoItem {
        TodoItem {
            content: "deploy".into(),
            status,
            by: TodoBy::Model,
            when: None,
            needs,
        }
    }

    /// **A question that stopped waiting says which route took it** — the operator's ask, once
    /// more: *"when question is resolved models are being able to cancel the ask"*.
    #[test]
    fn a_question_that_stops_waiting_says_which_route_took_it() {
        let asked = TodoNeed::Operator {
            question: "which db?".into(),
            options: vec!["postgres".into(), "sqlite".into()],
        };
        // The operator answered: the edge is the reply.
        let answered = row(
            TodoStatus::Pending,
            vec![TodoNeed::Answered {
                question: "which db?".into(),
                reply: "the operator chose \"postgres\"".into(),
            }],
        );
        // The model took it back: the edge is simply gone.
        let taken_back = row(TodoStatus::Pending, Vec::new());
        // And the row itself stopped being open work.
        let done = row(TodoStatus::Completed, vec![asked.clone()]);
        let dropped = row(TodoStatus::Cancelled, vec![asked.clone()]);
        // A restatement that replaced the question is not an answer to the one that was asked.
        let other = row(
            TodoStatus::Pending,
            vec![TodoNeed::Operator {
                question: "which db, really?".into(),
                options: vec!["postgres".into(), "sqlite".into()],
            }],
        );

        for (case, board, expected) in [
            ("answered", vec![answered], "answered — "),
            (
                "taken back",
                vec![taken_back],
                "the model took its question back — ",
            ),
            ("completed", vec![done], "the row is done — "),
            ("dropped", vec![dropped], "the row was dropped — "),
            (
                "re-asked",
                vec![other],
                "the model took its question back — ",
            ),
            ("gone", Vec::new(), "the row is gone, so nothing waits "),
        ] {
            let said = settled_words(&board, "deploy", "which db?");
            assert!(
                said.starts_with(expected),
                "{case}: expected {expected:?}, got {said:?}"
            );
            assert!(
                said.ends_with("for \"which db?\" on `deploy`"),
                "{case}: the question and the row are still named: {said:?}"
            );
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
