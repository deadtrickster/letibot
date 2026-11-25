//! A Claude Code seat's question to the operator reaches the head as an ask, and the answer
//! settles the seat's row where the bridge reads it.

use letibot_sessionlog::hub::CommandKind;
use letibot_sessionlog::protocol::Caps;
use letibot_sessionlog::registry::SessionWiring;
use letibot_tokencore::store::{TodoBy, TodoItem, TodoNeed, TodoStatus};

use super::*;
use crate::mergequeue::serve::tests::TempDir;

const MAIN: &str = "s-main";
const SEAT: &str = "cc@stroppy-io";

fn wiring() -> SessionWiring {
    SessionWiring {
        model: "m".into(),
        dialect: "d".into(),
        endpoint: "http://127.0.0.1:1".into(),
        workspace: "/w".into(),
    }
}

fn asking(row: &str, question: &str) -> TodoItem {
    TodoItem {
        content: row.into(),
        status: TodoStatus::Pending,
        by: TodoBy::Model,
        when: None,
        needs: vec![TodoNeed::Operator {
            question: question.into(),
            options: vec!["go ahead".into(), "hold".into()],
        }],
    }
}

/// One seat with a question on its board, one row that asks nobody anything, and a daemon store.
fn world(tag: &str) -> (TempDir, StoreAt, Arc<Registry>) {
    let dir = TempDir::new(&format!("seat-asks-{tag}"));
    let at = StoreAt::File(dir.0.join("sessions.db"));
    let store = at.open().expect("the store");
    store
        .put_seat_todos(
            SEAT,
            &[
                asking("hand the lab1 hosts to another project", "hand them over?"),
                TodoItem {
                    content: "drain the background jobs".into(),
                    status: TodoStatus::InProgress,
                    by: TodoBy::Model,
                    when: None,
                    needs: Vec::new(),
                },
            ],
        )
        .unwrap();
    store
        .put_seat_todos(
            "cc@quiet",
            &[TodoItem {
                content: "nothing to ask".into(),
                status: TodoStatus::Pending,
                by: TodoBy::Model,
                when: None,
                needs: Vec::new(),
            }],
        )
        .unwrap();
    let registry = Registry::new();
    let hub = registry.new_hub(MAIN.to_string());
    registry
        .adopt(hub, MAIN.to_string(), wiring(), None)
        .expect("adopt");
    (dir, at, registry)
}

fn answer(root: &Arc<Hub>, head: &str, req: &str, option: usize) {
    root.submit(
        head,
        "c1",
        root.head_seq(),
        CommandKind::Answer {
            req_id: req.to_string(),
            reply: Reply::Question(QuestionAnswer {
                option: Some(option),
                note: None,
                free: None,
                abstain: false,
            }),
        },
    );
}

/// The seat's question is a card on the main session - a question, its choices the row's options,
/// naming the seat - and answering it writes the answer on the seat's row.
#[test]
fn a_seats_operator_question_is_a_card_on_the_head_and_its_answer_frees_the_row() {
    let (_dir, at, registry) = world("answer");
    let store = at.open().unwrap();
    let asks = open_asks(&store);
    assert_eq!(asks.len(), 1, "one question, not the quiet seat: {asks:?}");
    assert_eq!(asks[0].seat, SEAT);
    assert_eq!(asks[0].row, "hand the lab1 hosts to another project");

    let root = registry.get(MAIN).unwrap();
    let answered = Answered::default();
    assert_eq!(
        relay(&root, &registry, Some(at.clone()), &asks, &answered),
        1
    );
    let cards = root.snapshot().open_decisions;
    let card = cards
        .iter()
        .find(|c| c.req_id == asks[0].req_id())
        .unwrap_or_else(|| panic!("the seat's question is on the head: {cards:?}"));
    assert_eq!(card.kind, "question");
    assert_eq!(card.summary, "hand them over?");
    assert_eq!(
        card.choices,
        vec!["go ahead".to_string(), "hold".to_string()]
    );
    assert_eq!(
        card.subagent.as_ref().map(|s| s.handle.as_str()),
        Some(SEAT),
        "the card names the seat that asked"
    );
    // The daemon's pending set is what a head attaching later is handed.
    assert!(
        registry
            .pending_asks()
            .pending()
            .iter()
            .any(|p| p.ask == asks[0].req_id()
                && p.kind == letibot_transcript::pending::PendingKind::Question),
        "typed into the pending asks"
    );

    // Another pass posts nothing twice.
    assert_eq!(
        relay(&root, &registry, Some(at.clone()), &asks, &answered),
        0
    );

    // The operator answers from the head.
    let head = root.attach(
        "tui",
        "operator",
        Caps {
            can_decide: true,
            ..Caps::default()
        },
        0,
    );
    answer(&root, &head.head_id, &asks[0].req_id(), 1);
    assert!(
        !root
            .snapshot()
            .open_decisions
            .iter()
            .any(|c| c.req_id == asks[0].req_id()),
        "the card is off the head"
    );
    // The seat's row carries the answer the person chose, in its own words.
    let rows = store.seat_todos(SEAT).unwrap();
    let row = rows
        .iter()
        .find(|r| r.content == "hand the lab1 hosts to another project")
        .expect("the row");
    assert_eq!(
        row.needs,
        vec![TodoNeed::Answered {
            question: "hand them over?".into(),
            reply: "the operator chose \"hold\"".into(),
            ask: None,
        }],
        "the wait is the answer now: {row:?}"
    );
    // And it asks nobody anything any more, so the next pass withdraws nothing and posts nothing.
    assert!(open_asks(&store).is_empty());
    assert_eq!(
        relay(
            &root,
            &registry,
            Some(at.clone()),
            &open_asks(&store),
            &answered
        ),
        0
    );
}

/// A row the seat retires takes its card off the head: the board is the truth, and a question
/// nobody is waiting on any more is not a pending ask.
#[test]
fn a_question_the_seat_takes_back_is_withdrawn_from_the_head() {
    let (_dir, at, registry) = world("withdraw");
    let store = at.open().unwrap();
    let root = registry.get(MAIN).unwrap();
    let answered = Answered::default();
    let asks = open_asks(&store);
    relay(&root, &registry, Some(at.clone()), &asks, &answered);
    assert_eq!(root.snapshot().open_decisions.len(), 1);

    store
        .put_seat_todos(
            SEAT,
            &[TodoItem {
                content: "hand the lab1 hosts to another project".into(),
                status: TodoStatus::Pending,
                by: TodoBy::Model,
                when: None,
                needs: Vec::new(),
            }],
        )
        .unwrap();
    relay(
        &root,
        &registry,
        Some(at.clone()),
        &open_asks(&store),
        &answered,
    );
    assert!(
        root.snapshot().open_decisions.is_empty(),
        "the card is withdrawn with the question: {:?}",
        root.snapshot().open_decisions
    );
}
