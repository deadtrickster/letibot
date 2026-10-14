//! **A row that waits on the OPERATOR** — the `operator` edge, its refusals, the check's reading of
//! it, and the answer that unblocks it.
//!
//! The operator's ask, verbatim, which every test here quotes from: *"a todo can be dependent on
//! todo, and todo can be dependend on ME. if model says todo is dependent on ME the other heads of
//! the todo DAG keep firing, model told to try to figure it out anyways, and the heads show loud
//! notification that im waited. i guess we will force models to do choice for todo - so when it
//! says it waits for ME it give me that choice list with an optioin to enter my freetext. and then
//! when i submit todo becomes unblocked with reply attached"*.

use super::*;

const DEPLOY: &str = "deploy the staging box";
const QUESTION: &str = "which database should staging use?";

fn asked() -> TodoNeed {
    TodoNeed::Operator {
        question: QUESTION.into(),
        options: vec!["postgres".into(), "sqlite".into()],
    }
}

fn waiting(content: &str) -> TodoItem {
    TodoItem {
        needs: vec![asked()],
        ..item(content, TodoStatus::Pending)
    }
}

fn invoke(rt: &mut ToolRuntime, args: &str) -> crate::result::ToolResult {
    let mut sink = RecordingToolSink::new();
    rt.invoke("t1", &call(args), &mut sink)
}

/// **The model sets the edge on its OWN row through `needs`.** *"todo can be dependend on ME"* —
/// the tagged form carries the question and the options, and the reply shows the row waiting on
/// the operator with the question quoted.
#[test]
fn the_model_can_make_its_own_row_wait_on_the_operator() {
    let (mut rt, board) = runtime();
    let r = invoke(
        &mut rt,
        r#"{"todos": [{"content": "deploy the staging box", "status": "pending",
             "needs": [{"kind": "operator", "question": "which database should staging use?",
                        "options": ["postgres", "sqlite"]}]}]}"#,
    );
    assert_eq!(r.outcome, ToolOutcome::Ok, "{}", r.payload);
    assert_eq!(board.snapshot()[0].needs, vec![asked()]);
    assert!(
        r.payload
            .contains("waiting on the OPERATOR — asked \"which database should staging use?\""),
        "{}",
        r.payload
    );
}

/// **And on the OPERATOR's row through `wait_on`** — a row already on the board, either half,
/// without restating it.
#[test]
fn wait_on_puts_an_operator_question_on_the_operators_own_row() {
    let board = Arc::new(TodoBoard::new(Vec::new()));
    board.set_operator(vec![TodoItem {
        by: TodoBy::Operator,
        ..item(DEPLOY, TodoStatus::Pending)
    }]);
    let tool = TodoWriteTool::new(board.clone());
    let r = tool.wait_on(
        &json!([{"content": DEPLOY, "operator": {"question": QUESTION, "options": ["postgres", "sqlite"]}}]),
        None,
    );
    assert_eq!(r.outcome, ToolOutcome::Ok, "{}", r.payload);
    assert_eq!(board.operator_snapshot()[0].needs, vec![asked()]);
    assert!(
        r.payload.contains("rest of the plan keeps going"),
        "{}",
        r.payload
    );
}

/// **Fewer than two options is refused, and so is an empty question** — *"we will force models to
/// do choice"*. Through both doors, with a sentence that says why, and nothing is written.
#[test]
fn an_operator_question_with_fewer_than_two_options_or_no_words_is_refused() {
    let (mut rt, board) = runtime();
    for args in [
        r#"{"todos": [{"content": "a", "status": "pending", "needs": [{"kind": "operator", "question": "q?", "options": ["only one"]}]}]}"#,
        r#"{"todos": [{"content": "a", "status": "pending", "needs": [{"kind": "operator", "question": "q?", "options": []}]}]}"#,
        r#"{"todos": [{"content": "a", "status": "pending", "needs": [{"kind": "operator", "question": "q?"}]}]}"#,
        r#"{"todos": [{"content": "a", "status": "pending", "needs": [{"kind": "operator", "question": "  ", "options": ["x", "y"]}]}]}"#,
        r#"{"todos": [{"content": "a", "status": "pending", "needs": [{"kind": "operator", "question": "q?", "options": ["x", "x"]}]}]}"#,
    ] {
        let r = invoke(&mut rt, args);
        assert_ne!(r.outcome, ToolOutcome::Ok, "{args} was accepted");
        assert!(board.snapshot().is_empty(), "{args} wrote a row");
    }
    let r = invoke(
        &mut rt,
        r#"{"todos": [{"content": "a", "status": "pending", "needs": [{"kind": "operator", "question": "q?", "options": ["only one"]}]}]}"#,
    );
    let said = format!("{:?} {}", r.outcome, r.payload);
    assert!(said.contains("at least two"), "{said}");

    let board = Arc::new(TodoBoard::new(vec![item(DEPLOY, TodoStatus::Pending)]));
    let tool = TodoWriteTool::new(board.clone());
    let r = tool.wait_on(
        &json!([{"content": DEPLOY, "operator": {"question": QUESTION, "options": ["postgres"]}}]),
        None,
    );
    assert_ne!(r.outcome, ToolOutcome::Ok);
    let said = format!("{:?} {}", r.outcome, r.payload);
    assert!(said.contains("at least two"), "{said}");
    assert!(board.snapshot()[0].needs.is_empty());
    // **And a model cannot write itself an answer.**
    let (mut rt, board) = runtime();
    let r = invoke(
        &mut rt,
        r#"{"todos": [{"content": "a", "status": "pending", "needs": [{"kind": "answered", "question": "q?", "reply": "yes"}]}]}"#,
    );
    assert_ne!(r.outcome, ToolOutcome::Ok);
    assert!(board.snapshot().is_empty());
}

/// **The check skips the row and keeps asking about the others** — *"the other heads of the todo
/// DAG keep firing"* — and its blocked line quotes the question and tells the model to keep going
/// and to try to figure it out — *"model told to try to figure it out anyways"*.
#[test]
fn the_check_asks_about_the_other_rows_and_tells_the_model_to_keep_going() {
    let plan = vec![
        waiting(DEPLOY),
        item("write the release notes", TodoStatus::Pending),
    ];
    let said = nag_for(&plan).expect("the other row is asked about");
    assert!(said.contains("  - write the release notes"), "{said}");
    assert!(
        !said.contains(&format!("  - {DEPLOY}")),
        "the waiting row is not asked about: {said}"
    );
    assert!(said.contains("waiting on the operator"), "{said}");
    assert!(
        said.contains(&format!("they were asked \"{QUESTION}\"")),
        "{said}"
    );
    assert!(said.contains("Keep going"), "{said}");
    assert!(
        said.contains("try to work the question out yourself"),
        "{said}"
    );
    assert!(said.contains("\"detach\": true"), "{said}");
}

/// **A plan whose only open work waits on the operator is SILENT** — directly, or through a row
/// that waits on one that does. Nothing is the model's to unblock; the heads are the loud half.
#[test]
fn a_plan_waiting_only_on_the_operator_is_not_nagged() {
    assert_eq!(nag_for(&[waiting(DEPLOY)]), None);
    let after = needs("announce it", TodoStatus::Pending, &[DEPLOY]);
    assert_eq!(nag_for(&[waiting(DEPLOY), after]), None);
    // A broken edge beside it is still said: that one IS the model's to fix.
    let broken = needs("tidy up", TodoStatus::Pending, &["no such row"]);
    assert!(nag_for(&[waiting(DEPLOY), broken]).is_some());
}

/// **The answer unblocks the row and keeps the reply on it** — *"when i submit todo becomes
/// unblocked with reply attached"*. The title is untouched (it is the row's key), the edge becomes
/// `answered`, the check asks about the row again, and the reply prints it.
#[test]
fn an_answer_unblocks_the_row_and_attaches_the_reply() {
    let board = TodoBoard::new(vec![waiting(DEPLOY)]);
    let ask = board.operator_ask(DEPLOY, QUESTION).expect("the row asks");
    assert_eq!(
        ask.options,
        vec!["postgres".to_string(), "sqlite".to_string()]
    );
    let v = board.version();
    board
        .answer_operator(DEPLOY, QUESTION, "the operator chose \"postgres\"")
        .expect("answered");
    assert!(board.version() > v);
    let row = &board.snapshot()[0];
    assert_eq!(row.content, DEPLOY, "the title is intact");
    assert_eq!(
        row.needs,
        vec![TodoNeed::Answered {
            question: QUESTION.into(),
            reply: "the operator chose \"postgres\"".into()
        }]
    );
    let said = nag_for(&board.snapshot()).expect("the row is asked about again");
    assert!(said.contains(&format!("  - {DEPLOY}")), "{said}");
    let text = board_text(&board.snapshot());
    assert!(
        text.contains("(answered: you asked \"which database should staging use?\" — the operator chose \"postgres\")"),
        "{text}"
    );
    // A second answer to the same question finds nothing open.
    assert!(board.answer_operator(DEPLOY, QUESTION, "again").is_err());
    assert!(
        board
            .operator_ask(DEPLOY, QUESTION)
            .unwrap_err()
            .contains("already answered")
    );
}

/// **A restatement keeps the answer and does not re-ask it.** The model restating its plan from an
/// older copy (the operator edge still on it) must not put the card back in front of somebody who
/// answered it; and `wait_on` asking the same question again is refused with the answer quoted.
#[test]
fn a_restated_row_keeps_its_answer_and_is_not_asked_again() {
    let (mut rt, board) = runtime();
    let set = r#"{"todos": [{"content": "deploy the staging box", "status": "pending",
             "needs": [{"kind": "operator", "question": "which database should staging use?",
                        "options": ["postgres", "sqlite"]}]}]}"#;
    assert_eq!(invoke(&mut rt, set).outcome, ToolOutcome::Ok);
    board
        .answer_operator(DEPLOY, QUESTION, "the operator abstained")
        .unwrap();
    assert_eq!(invoke(&mut rt, set).outcome, ToolOutcome::Ok);
    assert_eq!(
        board.snapshot()[0].needs,
        vec![TodoNeed::Answered {
            question: QUESTION.into(),
            reply: "the operator abstained".into()
        }]
    );
    let tool = TodoWriteTool::new(board.clone());
    let r = tool.wait_on(
        &json!([{"content": DEPLOY, "operator": {"question": QUESTION, "options": ["a", "b"]}}]),
        None,
    );
    assert_ne!(r.outcome, ToolOutcome::Ok);
    let said = format!("{:?} {}", r.outcome, r.payload);
    assert!(said.contains("already answered"), "{said}");
}

/// **The model that worked it out takes the wait off with `detach`**, and the row is ready again —
/// the door the blocked line names.
#[test]
fn detach_takes_the_operator_question_off() {
    let board = Arc::new(TodoBoard::new(vec![waiting(DEPLOY)]));
    let tool = TodoWriteTool::new(board.clone());
    let r = tool.wait_on(&json!([{"content": DEPLOY, "detach": true}]), None);
    assert_eq!(r.outcome, ToolOutcome::Ok, "{}", r.payload);
    assert!(board.snapshot()[0].needs.is_empty());
    assert!(board.operator_ask(DEPLOY, QUESTION).is_err());
}

/// **The call an expired question offers is one `todo_write` takes** — the offer is the way an
/// unanswered `ask_user_question` unblocks the model, so it is fed back here exactly as written.
#[test]
fn the_call_an_expired_question_offers_is_one_todo_write_takes() {
    let q = crate::builtins::intent::Question {
        text: "should I refactor the parser while I am here?".into(),
        options: vec!["yes, now".into(), "no, later".into()],
        ..Default::default()
    };
    let offer = crate::builtins::intent::ask::todo_offer(&q);
    let call = offer
        .lines()
        .find(|l| l.starts_with('{'))
        .expect("the offer writes the call out");
    let (mut rt, board) = runtime();
    let r = invoke(&mut rt, call);
    assert_eq!(r.outcome, ToolOutcome::Ok, "{call}\n{}", r.payload);
    let rows = board.snapshot();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(
        r.payload.contains("waiting on the OPERATOR"),
        "the row waits on the operator: {}",
        r.payload
    );
}
