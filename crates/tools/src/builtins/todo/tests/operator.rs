//! A row that waits on the OPERATOR: the `operator` edge, its refusals, the check's reading of it,
//! and the answer that unblocks it.

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

/// The model sets the edge on its OWN row through `needs`. *"todo can be dependend on ME"*  -
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

/// And on the OPERATOR's row through `wait_on` - a row already on the board, either half,
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

/// Fewer than two options is refused, and so is an empty question - *"we will force models to
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
    // And a model cannot write itself an answer.
    let (mut rt, board) = runtime();
    let r = invoke(
        &mut rt,
        r#"{"todos": [{"content": "a", "status": "pending", "needs": [{"kind": "answered", "question": "q?", "reply": "yes"}]}]}"#,
    );
    assert_ne!(r.outcome, ToolOutcome::Ok);
    assert!(board.snapshot().is_empty());
}

/// The check skips the row and keeps asking about the others - *"the other heads of the todo
/// DAG keep firing"* - and its blocked line quotes the question and tells the model to keep going
/// and to try to figure it out - *"model told to try to figure it out anyways"*.
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

/// A plan whose every open row waits on the operator - directly, or through a row that waits on
/// one that does - is PARKED: said once as itself, and never as a plan to unblock. The operator's
/// case, 2026-11-26: a seat's rows left open on purpose until they answer.
#[test]
fn a_plan_parked_on_the_operator_says_parked_and_never_that_nothing_can_start() {
    let after = needs("announce it", TodoStatus::Pending, &[DEPLOY]);
    let further = needs(
        "tell the other project",
        TodoStatus::Pending,
        &["announce it"],
    );
    for plan in [
        vec![waiting(DEPLOY)],
        vec![waiting(DEPLOY), after.clone()],
        vec![waiting(DEPLOY), after.clone(), further],
    ] {
        let said = nag_for(&plan).expect("a parked plan is said, once");
        assert!(
            said.starts_with("[todo check] parked: waiting on the operator's answer to"),
            "the state has its own words: {said}"
        );
        assert!(
            said.contains(&format!("\"{QUESTION}\"")),
            "and names the question the person was asked: {said}"
        );
        assert!(
            !said.contains("NOTHING can start"),
            "**parked is not stuck**: {said}"
        );
        assert!(
            !said.contains("unblock one of these"),
            "and names no act here, because there is none: {said}"
        );
    }
    // A row that can start is the ordinary check, whatever else waits on the operator.
    let free = vec![
        waiting(DEPLOY),
        item("write the release notes", TodoStatus::Pending),
    ];
    let said = nag_for(&free).expect("the free row is asked about");
    assert!(said.contains("  - write the release notes"), "{said}");
    assert!(!said.contains("parked"), "{said}");
    // And a plan that waits on anything else keeps the check it has today: a broken edge is the
    // model's to fix, so it is not parked.
    let broken = needs("tidy up", TodoStatus::Pending, &["no such row"]);
    let said = nag_for(&[waiting(DEPLOY), broken]).expect("the broken edge is said");
    assert!(!said.contains("parked"), "{said}");
    assert!(said.contains("no such row on this board"), "{said}");
    // A row waiting on a job or a peer is the plan's own too, and parked says nothing about it.
    let peer = TodoItem {
        needs: vec![TodoNeed::Ask {
            ask: 7,
            of: "cc@etcd-752".into(),
            room: "proj".into(),
            question: "is the fix pushed?".into(),
        }],
        ..item("merge the peer's fix", TodoStatus::Pending)
    };
    assert_eq!(nag_for(&[waiting(DEPLOY), peer]), None);
}

/// Said ONCE: the answer is what wakes the session, so the parked line is not re-said on the
/// ladder. A different question is news, and arms the check again.
#[test]
fn a_parked_plan_is_said_once_and_not_again_on_the_ladder() {
    use std::time::Instant;
    let window = Duration::from_secs(60);
    let t0 = Instant::now();
    let none = ChildSessions::none();
    let plan = vec![
        waiting(DEPLOY),
        needs("announce it", TodoStatus::Pending, &[DEPLOY]),
    ];
    for (i, mut clock) in [
        TodoNagClock::with_window(window),
        TodoNagClock::with_window(window).keyed_by_plan(),
    ]
    .into_iter()
    .enumerate()
    {
        let seat = i == 1;
        clock.rearm_from(&plan, t0, &none, &no_jobs());
        assert!(clock.due_now(t0 + window), "the first check is due");
        let text = nag_for(&plan).expect("a parked plan is said");
        match seat {
            // A seat records the PLAN it asked about; a session records its sentence.
            true => clock.said_plan(&plan, Some(text), t0 + window),
            false => clock.said(Some(text), t0 + window),
        }
        // Ten minutes of an unchanged parked plan: nothing is armed, so nothing repeats.
        for minute in 1..=10 {
            clock.rearm_from(&plan, t0 + window * (1 + minute), &none, &no_jobs());
            assert_eq!(
                clock.due_at(),
                None,
                "minute {minute} re-armed a parked plan"
            );
        }
        // A new question is a new thing to say.
        let asked_again = vec![TodoItem {
            needs: vec![TodoNeed::Operator {
                question: "which database, now that staging moved?".into(),
                options: vec!["postgres".into(), "sqlite".into()],
            }],
            ..item(DEPLOY, TodoStatus::Pending)
        }];
        clock.rearm_from(&asked_again, t0 + window * 12, &none, &no_jobs());
        assert_eq!(
            clock.due_at(),
            Some(t0 + window * 13),
            "a different parked question is said at the base window"
        );
    }
}

/// The answer unblocks the row and keeps the reply on it - *"when i submit todo becomes
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
            reply: "the operator chose \"postgres\"".into(),
            ask: None,
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

/// A restatement keeps the answer and does not re-ask it. The model restating its plan from an
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
            reply: "the operator abstained".into(),
            ask: None,
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

/// The model that worked it out takes the wait off with `detach`, and the row is ready again  -
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

/// The call an expired question offers is one `todo_write` takes - the offer is the way an
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
