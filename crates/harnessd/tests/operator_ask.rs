//! **A todo row that waits on the OPERATOR, end to end through the daemon's own door** — the
//! loud state on the hub, the settings row and the picker's brief; the answer as a head sends it
//! (`CommandKind::AnswerTodo`) in every shape the card offers; the row unblocked with the reply on
//! it; the model told at once; and the question surviving a restart.
//!
//! The operator, verbatim: *"a todo can be dependent on todo, and todo can be dependend on ME. if
//! model says todo is dependent on ME the other heads of the todo DAG keep firing, model told to
//! try to figure it out anyways, and the heads show loud notification that im waited. i guess we
//! will force models to do choice for todo - so when it says it waits for ME it give me that choice
//! list with an optioin to enter my freetext. and then when i submit todo becomes unblocked with
//! reply attached"*.
//!
//! # What this needs
//!
//! The vocabulary GGUF (a `Harness` renders its stable prefix at open) and **not a model**: the
//! endpoint is a port nothing listens on. The turn an answer starts fails at the send — what is
//! asserted is that it was STARTED with the answer as its prompt, which is the row on the hub.

use letibot_harnessd::config::Config;
use letibot_harnessd::sessions::StoreSessions;
use letibot_harnessd::{Dialect, Parts, Sessions};
use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::hub::{CommandKind, Hub, QueuedCommand};
use letibot_sessionlog::question::QuestionAnswer;
use letibot_sessionlog::registry::Registry;
use letibot_tokencore::store::{SessionRecord, Store, TodoBy, TodoItem, TodoNeed, TodoStatus};
use letibot_turn::Endpoint;

fn config(store: &std::path::Path, session_id: &str) -> Config {
    let mut cfg = Config::for_this_box("/tmp");
    cfg.dialect = Dialect::Qwen;
    cfg.store = Some(store.to_path_buf());
    cfg.session_id = session_id.to_string();
    cfg.web_search = None;
    cfg.permission = Vec::new();
    cfg.endpoint = Endpoint::new("127.0.0.1", 1);
    cfg.http_retries = 0;
    cfg.vocab_gguf = std::env::var("LETIBOT_VOCAB_GGUF")
        .ok()
        .map(std::path::PathBuf::from)
        .or_else(letibot_tokencore::apparatus::present_gguf);
    cfg
}

fn asking(content: &str, question: &str) -> TodoItem {
    TodoItem {
        content: content.into(),
        status: TodoStatus::Pending,
        by: TodoBy::Model,
        when: None,
        needs: vec![TodoNeed::Operator {
            question: question.into(),
            options: vec!["postgres".into(), "sqlite".into()],
        }],
    }
}

/// Four rows, each asking the operator something — one per shape of answer — and one row that
/// asks nobody anything.
fn seeded(tag: &str) -> (std::path::PathBuf, String) {
    let dir = std::env::temp_dir().join(format!(
        "harnessd-operator-ask-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sessions.db");
    let session_id = format!("operator-ask-{tag}");
    let s = Store::open(&path).expect("seeding");
    s.put_session(&SessionRecord {
        id: session_id.clone(),
        title: Some("the asking session".into()),
        model_id: "m".into(),
        dialect_sha: "sha".into(),
        workspace_root: "/tmp".into(),
        owner: "dead".into(),
        role: None,
        approvers: vec![],
        parent_session_id: None,
    })
    .expect("the session row");
    s.put_todos(
        &session_id,
        &[
            asking("pick the db", "which db?"),
            asking("pick the db with a note", "which db, really?"),
            asking("pick in my own words", "which db, in words?"),
            asking("pick nothing", "which db, if any?"),
            TodoItem {
                content: "write the notes".into(),
                status: TodoStatus::Pending,
                by: TodoBy::Model,
                when: None,
                needs: Vec::new(),
            },
        ],
    )
    .expect("the todo rows");
    (path, session_id)
}

fn opened<'a>(
    parts: &'a Parts,
    path: &std::path::Path,
    session_id: &str,
) -> (Sessions<'a>, std::sync::Arc<Registry>) {
    let cfg = config(path, session_id);
    let registry = Registry::new();
    if let Some(src) = StoreSessions::open(&cfg) {
        registry.set_source(src);
    }
    registry
        .create(session_id, "", Sessions::wiring(&cfg))
        .expect("the session is in the registry");
    let sessions =
        Sessions::open_first(parts, cfg.clone(), registry.clone()).expect("the session opens");
    (sessions, registry)
}

fn answer(sessions: &mut Sessions, id: &str, content: &str, question: &str, a: QuestionAnswer) {
    sessions.dispatch(
        id,
        &QueuedCommand {
            head_id: "test-head".into(),
            identity: "dead".into(),
            client_request_id: "req-1".into(),
            at_seq: 0,
            enqueued_ms: 0,
            kind: CommandKind::AnswerTodo {
                content: content.into(),
                question: question.into(),
                answer: a,
            },
        },
    );
}

fn asks_row(registry: &Registry, id: &str) -> String {
    registry
        .settings(id)
        .into_iter()
        .find(|r| r.key == "asks")
        .map(|r| r.value)
        .expect("the settings carry an `asks` row")
}

fn rows(hub: &Hub, code: &str) -> Vec<String> {
    hub.retained()
        .iter()
        .filter_map(|e| match &e.event {
            SessionEvent::Warning {
                code: c, detail, ..
            } if c == code => Some(detail.clone()),
            _ => None,
        })
        .collect()
}

/// The operator's prompts the hub has seen, as text — the turns the answers started.
fn prompts(hub: &Hub) -> Vec<String> {
    hub.retained()
        .iter()
        .filter_map(|e| match &e.event {
            SessionEvent::TranscriptContent { item, .. } => match &**item {
                letibot_transcript::TranscriptItem::User { .. } => {
                    Some(serde_json::to_string(&**item).unwrap_or_default())
                }
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn needs_of(path: &std::path::Path, id: &str, content: &str) -> Vec<TodoNeed> {
    Store::open(path)
        .unwrap()
        .todos(id)
        .unwrap()
        .into_iter()
        .find(|t| t.content == content)
        .expect("the row is on the board")
        .needs
}

/// **The heads are told loudly, and the four answers each unblock their row** — *"the heads show
/// loud notification that im waited"* and *"when i submit todo becomes unblocked with reply
/// attached"*. The settings row and the picker's brief say how many questions wait; an option,
/// an option with a note, typed words and an abstention each replace the question with the reply
/// on the row (the title untouched), start a turn whose prompt is the `[answer]` line, and the
/// loud state clears when the last one is answered.
#[test]
fn every_answer_shape_unblocks_its_row_and_tells_the_model() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let (path, id) = seeded("shapes");
    let parts = Parts::load(&config(&path, &id)).expect("vocabulary");
    let (mut sessions, registry) = opened(&parts, &path, &id);
    assert!(
        asks_row(&registry, &id).starts_with("4 — \"which db?\""),
        "{}",
        asks_row(&registry, &id)
    );
    let brief = registry
        .list()
        .into_iter()
        .find(|b| b.session_id == id)
        .expect("listed");
    assert_eq!(brief.waits_on_operator, 4, "the picker's mark");

    answer(
        &mut sessions,
        &id,
        "pick the db",
        "which db?",
        QuestionAnswer::choosing(0),
    );
    answer(
        &mut sessions,
        &id,
        "pick the db with a note",
        "which db, really?",
        QuestionAnswer::choosing(1).with_note("only for tests"),
    );
    answer(
        &mut sessions,
        &id,
        "pick in my own words",
        "which db, in words?",
        QuestionAnswer::free("the one already running"),
    );
    answer(
        &mut sessions,
        &id,
        "pick nothing",
        "which db, if any?",
        QuestionAnswer::abstaining(),
    );

    let replies = [
        (
            "pick the db",
            "which db?",
            "the operator chose \"postgres\"",
        ),
        (
            "pick the db with a note",
            "which db, really?",
            "the operator chose \"sqlite\" (note: only for tests)",
        ),
        (
            "pick in my own words",
            "which db, in words?",
            "the operator wrote: \"the one already running\"",
        ),
        (
            "pick nothing",
            "which db, if any?",
            "the operator abstained — they will not decide this one",
        ),
    ];
    let hub = registry.get(&id).expect("hub");
    let said = prompts(&hub);
    for (row, question, reply) in replies {
        assert_eq!(
            needs_of(&path, &id, row),
            vec![TodoNeed::Answered {
                question: question.into(),
                reply: reply.into()
            }],
            "{row}: unblocked, with the reply attached"
        );
        let line = format!("[answer] on \\\"{row}\\\": you asked \\\"{question}\\\"");
        assert!(
            said.iter().any(|p| p.contains(&line)),
            "{row}: the model was told — {said:#?}"
        );
    }
    assert!(
        said.iter().any(|p| p.contains("Decide it yourself")),
        "an abstention tells the model to state its assumption"
    );
    assert_eq!(asks_row(&registry, &id), "none", "the loud state clears");
    assert_eq!(rows(&hub, "operator_question_settled").len(), 4);
    let brief = registry
        .list()
        .into_iter()
        .find(|b| b.session_id == id)
        .expect("listed");
    assert_eq!(brief.waits_on_operator, 0, "the picker's mark clears");
}

/// **A question asked from here on is announced; a refused answer is said; a restart keeps the
/// waiting row.** The operator's own row gets a question through the head's push (the same door
/// the pane echoes edges through), which publishes `waiting_on_operator`; an option the row never
/// offered is refused by name and leaves the question open; and a daemon opened again on the same
/// store reads the row back still waiting and says so in its settings.
#[test]
fn a_waiting_row_is_announced_survives_a_refusal_and_a_restart() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let (path, id) = seeded("restart");
    let parts = Parts::load(&config(&path, &id)).expect("vocabulary");
    {
        let (mut sessions, registry) = opened(&parts, &path, &id);
        let hub = registry.get(&id).expect("hub");
        assert!(
            rows(&hub, "waiting_on_operator").is_empty(),
            "what the board already waited on at open is state, not news"
        );
        sessions.dispatch(
            &id,
            &QueuedCommand {
                head_id: "test-head".into(),
                identity: "dead".into(),
                client_request_id: "req-1".into(),
                at_seq: 0,
                enqueued_ms: 0,
                kind: CommandKind::SetOperatorTodos {
                    items: vec![letibot_sessionlog::event::TodoEntry {
                        content: "ship it".into(),
                        status: letibot_sessionlog::event::TodoStatus::Pending,
                        by: letibot_sessionlog::event::TodoBy::Operator,
                        when: None,
                        needs: vec![letibot_sessionlog::event::TodoNeed::Operator {
                            question: "today or tomorrow?".into(),
                            options: vec!["today".into(), "tomorrow".into()],
                        }],
                    }],
                    moved: Vec::new(),
                },
            },
        );
        let loud = rows(&hub, "waiting_on_operator");
        assert_eq!(loud.len(), 1, "{loud:?}");
        assert!(loud[0].contains("today or tomorrow?"), "{loud:?}");
        assert!(asks_row(&registry, &id).starts_with("5 — "));

        answer(
            &mut sessions,
            &id,
            "pick the db",
            "which db?",
            QuestionAnswer::choosing(5),
        );
        let refused = rows(&hub, "todo_answer_refused");
        assert_eq!(refused.len(), 1, "{refused:?}");
        assert!(matches!(
            needs_of(&path, &id, "pick the db")[0],
            TodoNeed::Operator { .. }
        ));
    }
    // **The restart**: a new daemon on the same store.
    let (_sessions, registry) = opened(&parts, &path, &id);
    assert!(
        asks_row(&registry, &id).starts_with("5 — "),
        "{}",
        asks_row(&registry, &id)
    );
    assert!(matches!(
        needs_of(&path, &id, "ship it")[0],
        TodoNeed::Operator { .. }
    ));
}
