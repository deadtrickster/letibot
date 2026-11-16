//! An ask on a queue session's board reaches the main session's heads as a card, and the answer
//! goes back to the asker's row.

use letibot_sessionlog::protocol::Caps;
use letibot_sessionlog::registry::SessionWiring;
use letibot_tokencore::store::{SessionRecord, TodoBy, TodoItem, TodoNeed, TodoStatus};

use super::*;
use crate::mergequeue::serve::tests::TempDir;

const MAIN: &str = "s-main";
const GATEKEEPER: &str = "s-gatekeeper-k";
const REVIEWER: &str = "s-gatekeeper-k-sub-1";

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
            options: vec!["yes".into(), "no".into()],
        }],
    }
}

/// The main session and the gatekeeper are open; the reviewer is only in the store.
fn world(tag: &str) -> (TempDir, StoreAt, Arc<Registry>) {
    let dir = TempDir::new(&format!("queue-asks-{tag}"));
    let at = StoreAt::File(dir.0.join("sessions.db"));
    let store = at.open().expect("the store");
    for (id, parent) in [
        (GATEKEEPER, None),
        (REVIEWER, Some(GATEKEEPER)),
        ("s-someone-else", None),
    ] {
        store
            .put_session(&SessionRecord {
                id: id.to_string(),
                title: None,
                model_id: String::new(),
                dialect_sha: String::new(),
                workspace_root: String::new(),
                owner: String::new(),
                approvers: Vec::new(),
                role: None,
                parent_session_id: parent.map(str::to_string),
            })
            .unwrap();
    }
    store
        .put_todos(
            GATEKEEPER,
            &[asking("Raise the review memory cap", "4 GiB or 8 GiB?")],
        )
        .unwrap();
    store
        .put_todos(
            REVIEWER,
            &[asking("Run the suite", "may I run cargo test?")],
        )
        .unwrap();
    store
        .put_todos("s-someone-else", &[asking("theirs", "not the queue's")])
        .unwrap();
    let registry = Registry::new();
    for id in [MAIN, GATEKEEPER] {
        let hub = registry.new_hub(id.to_string());
        registry
            .adopt(hub, id.to_string(), wiring(), None)
            .expect("adopt");
    }
    (dir, at, registry)
}

fn open_cards(hub: &Hub) -> Vec<letibot_sessionlog::view::OpenDecision> {
    hub.snapshot().open_decisions
}

#[test]
fn a_queue_sessions_ask_is_a_card_on_the_main_session_and_its_answer_reaches_the_row() {
    let (_dir, at, registry) = world("answer");
    let store = at.open().unwrap();
    let asks = open_asks(&store, GATEKEEPER);
    assert_eq!(
        asks.len(),
        2,
        "the gatekeeper's and the reviewer's, not a stranger's: {asks:?}"
    );

    let root = registry.get(MAIN).unwrap();
    let answered = Answered::default();
    assert_eq!(
        relay(
            &root,
            &registry,
            Some(at.clone()),
            GATEKEEPER,
            &asks,
            &answered
        ),
        2
    );
    let cards = open_cards(&root);
    assert_eq!(cards.len(), 2, "both are on the main head: {cards:?}");
    let reviewer = cards
        .iter()
        .find(|c| c.subagent.as_ref().is_some_and(|s| s.handle == REVIEWER))
        .expect("the card names the asking session");
    assert_eq!(reviewer.kind, "question");
    assert_eq!(reviewer.summary, "may I run cargo test?");
    assert_eq!(reviewer.choices, vec!["yes".to_string(), "no".to_string()]);

    // Another pass posts nothing twice, and a restart's first pass posts them again.
    assert_eq!(
        relay(
            &root,
            &registry,
            Some(at.clone()),
            GATEKEEPER,
            &asks,
            &answered
        ),
        0
    );
    let fresh = Registry::new();
    let again = fresh.new_hub(MAIN.to_string());
    fresh
        .adopt(again, MAIN.to_string(), wiring(), None)
        .unwrap();
    let again = fresh.get(MAIN).unwrap();
    assert_eq!(
        relay(
            &again,
            &fresh,
            Some(at.clone()),
            GATEKEEPER,
            &open_asks(&store, GATEKEEPER),
            &Answered::default()
        ),
        2,
        "the boards are the durable half"
    );

    // The operator answers from the main head.
    let head = root.attach(
        "tui",
        "operator",
        Caps {
            can_decide: true,
            ..Caps::default()
        },
        0,
    );
    let req = reviewer.req_id.clone();
    let frame = root.submit(
        &head.head_id,
        "c1",
        root.head_seq(),
        CommandKind::Answer {
            req_id: req.clone(),
            reply: Reply::Question(QuestionAnswer {
                option: Some(0),
                note: None,
                free: None,
                abstain: false,
            }),
        },
    );
    assert!(
        matches!(
            frame,
            letibot_sessionlog::protocol::ServerFrame::Accepted { .. }
        ),
        "{frame:?}"
    );
    assert!(
        !open_cards(&root).iter().any(|c| c.req_id == req),
        "the card is off the main head"
    );
    // The reviewer is not open, so its stored board is settled.
    let todos = store.todos(REVIEWER).unwrap();
    assert!(
        operator_asks(&todos).is_empty(),
        "the reviewer's row no longer waits: {todos:?}"
    );

    // The gatekeeper's answer is a command for the open session's own reader.
    let gk = open_cards(&root)
        .into_iter()
        .find(|c| c.subagent.as_ref().is_some_and(|s| s.handle == GATEKEEPER))
        .unwrap();
    root.submit(
        &head.head_id,
        "c2",
        root.head_seq(),
        CommandKind::Answer {
            req_id: gk.req_id.clone(),
            reply: Reply::Question(QuestionAnswer {
                option: Some(1),
                note: None,
                free: None,
                abstain: false,
            }),
        },
    );
    let hub = registry.get(GATEKEEPER).unwrap();
    let queued = hub
        .try_command()
        .expect("the answer is queued on the gatekeeper");
    assert!(
        matches!(&queued.kind, CommandKind::AnswerTodo { content, .. } if content == "Raise the review memory cap"),
        "{:?}",
        queued.kind
    );
}

#[test]
fn a_card_whose_ask_was_settled_elsewhere_is_withdrawn() {
    let (_dir, at, registry) = world("withdraw");
    let store = at.open().unwrap();
    let root = registry.get(MAIN).unwrap();
    let answered = Answered::default();
    relay(
        &root,
        &registry,
        Some(at.clone()),
        GATEKEEPER,
        &open_asks(&store, GATEKEEPER),
        &answered,
    );
    store.put_todos(REVIEWER, &[]).unwrap();
    relay(
        &root,
        &registry,
        Some(at.clone()),
        GATEKEEPER,
        &open_asks(&store, GATEKEEPER),
        &answered,
    );
    let cards = open_cards(&root);
    assert_eq!(cards.len(), 1, "{cards:?}");
    assert_eq!(cards[0].subagent.as_ref().unwrap().handle, GATEKEEPER);
}

/// A board ask is a typed question under the id its relayed card carries, so a reader of the
/// session list and the main session's card name one ask.
#[test]
fn a_board_ask_is_pending_under_the_relayed_cards_id() {
    let mut done = asking("settled", "moot?");
    done.status = TodoStatus::Completed;
    let todos = vec![asking("raise the cap", "4 or 8?"), done];
    let pending = board_pending(REVIEWER, &todos);
    let card = Ask {
        asker: REVIEWER.into(),
        row: "raise the cap".into(),
        question: "4 or 8?".into(),
        options: vec!["yes".into(), "no".into()],
    };
    assert_eq!(
        pending,
        vec![letibot_transcript::pending::Pending::new(
            letibot_transcript::pending::PendingKind::Question,
            REVIEWER,
            card.req_id(),
        )]
    );
}

/// The queue's sessions are named by role and work from the rows: a reviewer by the branch it
/// judges, with its clone in short, and a row whose reviewer is outside the tree is not named.
#[test]
fn the_queue_sessions_are_named_by_role_and_work() {
    use letibot_tokencore::store::{MergePriority, ReviewKind, ReviewRow, ReviewState};
    let row = |id: &str, branch: &str, reviewer: Option<&str>| ReviewRow {
        id: id.into(),
        repository: "/r".into(),
        target: "master".into(),
        kind: ReviewKind::Branch,
        session_id: "s-author".into(),
        branch: branch.into(),
        worktree: None,
        base_sha: "b".into(),
        head_sha: "h".into(),
        brief: "You are the gatekeeper.".into(),
        priority: MergePriority::Subagent,
        fixes: None,
        state: ReviewState::Reviewing,
        owner: String::new(),
        reviewer: reviewer.map(Into::into),
        deadline_ms: None,
        attempts: 0,
        failure: String::new(),
        verdict: None,
        verdict_by: None,
        reviewed_sha: None,
        reasons: vec![],
        files: vec![],
        commands: vec![],
        evidence: String::new(),
        created_ms: 1,
        state_ms: 1,
        updated_ms: 1,
    };
    let clones = std::path::Path::new("/tmp/letibot-review-clones");
    let named: HashMap<_, _> = askers(
        GATEKEEPER,
        "rano",
        clones,
        &[
            row(
                "agent-p2-followups-34db3e56",
                "agent-p2-followups",
                Some(REVIEWER),
            ),
            row("other-1", "other", Some("s-gatekeeper-elsewhere-sub-9")),
        ],
    )
    .into_iter()
    .collect();
    assert_eq!(named.len(), 2, "{named:?}");
    assert_eq!(named[GATEKEEPER].role, "rano review queue's gatekeeper");
    let r = &named[REVIEWER];
    assert_eq!(r.role, "reviewer of agent-p2-followups (rano review queue)");
    assert_eq!(
        r.workspace,
        "/tmp/letibot-review-clones/review-agent-p2-followups-34db3e56"
    );
    assert_eq!(r.workspace_short, "review clone of agent-p2-followups");

    let card = letibot_sessionlog::pending_asks::named(
        SessionEvent::DecisionRequested {
            req_id: "adj-1".into(),
            subagent: Some(SubagentAsk {
                handle: REVIEWER.into(),
                ..Default::default()
            }),
            kind: "permission".into(),
            call_id: None,
            access: String::new(),
            summary: String::new(),
            target: String::new(),
            write_targets: Vec::new(),
            edits: Vec::new(),
            detail: String::new(),
            options: Vec::new(),
            options_note: String::new(),
            choices: Vec::new(),
            because:
                "workspace: /tmp/letibot-review-clones/review-agent-p2-followups-34db3e56-92726-0"
                    .into(),
            advice: None,
            shadow: None,
            deadline: None,
            on_timeout: OnTimeout::Deny,
        },
        r,
    );
    let SessionEvent::DecisionRequested {
        subagent, because, ..
    } = card
    else {
        panic!("a card");
    };
    assert_eq!(because, "workspace: review clone of agent-p2-followups");
    assert_eq!(subagent.unwrap().role, r.role);
}
