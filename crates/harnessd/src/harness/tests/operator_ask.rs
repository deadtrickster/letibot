//! **The operator's answer to a row's question, reaching a RUNNING turn** — at the next step
//! boundary, through the steering source the turn polls.
//!
//! The operator: *"and then when i submit todo becomes unblocked with reply attached"* — and an
//! operator's words are immediate (R79), so an answer given while the model is mid-turn does not
//! wait for the turn to end.

use super::*;

use letibot_sessionlog::question::QuestionAnswer;
use letibot_tokencore::store::{TodoBy, TodoNeed, TodoStatus};

const ROW: &str = "deploy the staging box";
const QUESTION: &str = "which database should staging use?";

fn board() -> Arc<TodoBoard> {
    Arc::new(TodoBoard::new(vec![TodoItem {
        content: ROW.into(),
        status: TodoStatus::Pending,
        by: TodoBy::Model,
        when: None,
        needs: vec![TodoNeed::Operator {
            question: QUESTION.into(),
            options: vec!["postgres".into(), "sqlite".into()],
        }],
    }]))
}

fn steering(hub: &Arc<Hub>, board: &Arc<TodoBoard>) -> HubSteering {
    HubSteering {
        todos: Some(board.clone()),
        ..HubSteering::new(hub.clone())
    }
}

fn answer(hub: &Arc<Hub>, question: &str, a: QuestionAnswer) {
    let head = hub.attach(
        "tui",
        "dead@lab2x1",
        letibot_sessionlog::protocol::Caps::default(),
        0,
    );
    let f = hub.submit(
        &head.head_id,
        "c1",
        0,
        letibot_sessionlog::CommandKind::AnswerTodo {
            content: ROW.into(),
            question: question.into(),
            answer: a,
        },
    );
    assert!(
        matches!(
            f,
            letibot_sessionlog::protocol::ServerFrame::Accepted { .. }
        ),
        "{f:?}"
    );
}

/// **An answer given mid-turn rides the next step boundary** — the row is unblocked with the reply
/// on it, and the model's next message is the `[answer]` line, as a notice of its own (not
/// coalesced into the operator's prompt text, and not dropped by a take-back).
#[test]
fn an_answer_given_mid_turn_rides_the_step_boundary() {
    let hub = Hub::new("s-ask");
    let board = board();
    let mut s = steering(&hub, &board);
    answer(
        &hub,
        QUESTION,
        QuestionAnswer::choosing(0).with_note("the shared one"),
    );
    let m = s
        .try_next()
        .expect("the answer is handed to the running turn");
    assert!(!m.urgent && !m.from_operator, "{m:?}");
    assert_eq!(
        m.text,
        "[answer] on \"deploy the staging box\": you asked \"which database should staging use?\" \
         — the operator chose \"postgres\" (note: the shared one). The row no longer waits on the \
         operator."
    );
    assert_eq!(
        board.snapshot()[0].needs,
        vec![TodoNeed::Answered {
            question: QUESTION.into(),
            reply: "the operator chose \"postgres\" (note: the shared one)".into()
        }]
    );
}

/// **An answer the row cannot take is said to the heads and changes nothing** — a question the
/// row no longer asks, or an option it never offered. The turn is not handed anything.
#[test]
fn an_answer_the_row_cannot_take_is_refused_on_the_hub() {
    let hub = Hub::new("s-ask-bad");
    let board = board();
    let mut s = steering(&hub, &board);
    answer(&hub, QUESTION, QuestionAnswer::choosing(7));
    answer(&hub, "a question nobody asked", QuestionAnswer::choosing(0));
    assert!(s.try_next().is_none());
    let refused: Vec<String> = hub
        .retained()
        .iter()
        .filter_map(|e| match &e.event {
            SessionEvent::Warning { code, detail, .. } if code == "todo_answer_refused" => {
                Some(detail.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(refused.len(), 2, "{refused:?}");
    assert!(refused[0].contains("option 7"), "{refused:?}");
    assert!(matches!(
        board.snapshot()[0].needs[0],
        TodoNeed::Operator { .. }
    ));
}
