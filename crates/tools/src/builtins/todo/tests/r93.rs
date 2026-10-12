//! **R93's todo items** — what the model is told: a plan blocked only on running work is silent
//! (B5), the `wait_on` offer only where a row could take it (B6), and a `needs` schema a
//! constrained model can emit the tagged forms under (B10).

use super::*;

/// **B5: a plan blocked only on a young child is silent, as `todo_write` promises.** An OLD child
/// is the one the check has to steer at, so it still speaks.
#[test]
fn a_plan_blocked_only_on_a_young_child_says_nothing() {
    let plan = vec![waiting_on_a_child("merge the review", "s-child")];
    let young = ChildSessions::none().running("s-child", child(60, Some(10), true));
    assert_eq!(nag_with(&plan, &young, &no_jobs()), None);
    let old = ChildSessions::none().running("s-child", child(2 * 3600, Some(10), true));
    let said = nag_with(&plan, &old, &no_jobs()).expect("an old child is steered at");
    assert!(said.contains("NOTHING can start"), "{said}");
}

/// A tool that reports what `attach_now` would append to a start under the runtime it is in.
struct Probe;

impl Tool for Probe {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "probe",
            "Reports the wait_on offer a background start would carry.",
            json!({"type": "object", "properties": {}}),
            Access::Read,
        )
    }

    fn invoke(&self, ctx: &mut InvokeCtx<'_>, _args: &Value) -> Invocation {
        Invocation::ok(format!("[{}]", attach_now(ctx, "job", "j7")))
    }
}

fn probe_in(rows: Option<Vec<TodoItem>>) -> String {
    let mut reg = Registry::new();
    if let Some(rows) = rows {
        reg.register(Box::new(TodoWriteTool::new(Arc::new(TodoBoard::new(rows)))))
            .unwrap();
    }
    reg.register(Box::new(Probe)).unwrap();
    let d = TempDir::new();
    let backend = HostBackend::new(d.path()).unwrap();
    let rt = ToolRuntime::new(reg, Box::new(backend));
    let mut sink = RecordingToolSink::new();
    let r = rt.invoke(
        "t1",
        &ToolCall {
            id: "c0".into(),
            name: "probe".into(),
            arguments: "{}".into(),
        },
        &mut sink,
    );
    r.payload
}

/// **B6: the `wait_on` offer is said only where it can be taken** — `todo_write` seated and a row
/// open. It was on every background start, to seats with no plan and seats with no `todo_write`.
#[test]
fn the_wait_on_offer_needs_a_seated_todo_write_and_an_open_row() {
    assert_eq!(probe_in(None), "[]", "no todo_write seated");
    assert_eq!(probe_in(Some(Vec::new())), "[]", "an empty plan");
    assert_eq!(
        probe_in(Some(vec![item("done already", TodoStatus::Completed)])),
        "[]",
        "nothing open"
    );
    let offered = probe_in(Some(vec![item("push it", TodoStatus::Pending)]));
    assert!(
        offered.contains("`todo_write` {\"wait_on\"") && offered.contains("\"job\": \"j7\""),
        "{offered}"
    );
}

/// **B10: `needs` takes a string or a tagged object, and the schema says both.**
#[test]
fn the_needs_schema_admits_the_tagged_forms() {
    let s = TodoWriteTool::new(Arc::new(TodoBoard::new(Vec::new()))).schema();
    let items = &s.parameters["properties"]["todos"]["items"]["properties"]["needs"]["items"];
    let any = items["anyOf"].as_array().expect("anyOf");
    assert!(any.iter().any(|a| a["type"] == "string"), "{items}");
    assert!(any.iter().any(|a| a["type"] == "object"), "{items}");
}

/// **B3: a merge queue's row is not nagged while its review is in flight** — waiting in the queue
/// for a reviewer, or under a reviewer that is running and moving. It is asked about once the
/// reviewer has finished (no longer running) or has been quiet past the cap.
#[test]
fn a_queue_row_is_quiet_while_its_review_is_in_flight() {
    let queued = TodoItem {
        content: "review x (entry r1): do x".into(),
        status: TodoStatus::Pending,
        by: TodoBy::Queue,
        when: None,
        needs: Vec::new(),
    };
    assert!(queue_review_in_flight(&queued, &ChildSessions::none()));
    let reviewing = TodoItem {
        status: TodoStatus::InProgress,
        needs: vec![TodoNeed::Child { id: "s-rev".into() }],
        ..queued.clone()
    };
    let moving = ChildSessions::none().running("s-rev", child(3 * 3600, Some(60), true));
    assert!(queue_review_in_flight(&reviewing, &moving));
    assert_eq!(
        when_a_queue_reviewer_goes_quiet(std::slice::from_ref(&reviewing), &moving),
        Some(REVIEWER_QUIET_CAP - Duration::from_secs(60))
    );
    let wedged = ChildSessions::none().running("s-rev", child(3 * 3600, Some(31 * 60), true));
    assert!(!queue_review_in_flight(&reviewing, &wedged));
    assert!(
        !queue_review_in_flight(&reviewing, &ChildSessions::none()),
        "finished"
    );
    // The model's own row is never this predicate's business.
    let mine = TodoItem {
        by: TodoBy::Model,
        ..queued
    };
    assert!(!queue_review_in_flight(&mine, &ChildSessions::none()));
}

/// **B11: one rule for an abstention.** `ask_user_question` and an operator wait on a
/// `todo_write` row say the same thing — an abstention is an answer, and the model proceeds on
/// an assumption it states — where one used to call it "not permission to proceed".
#[test]
fn an_abstention_means_the_same_in_ask_and_in_a_todo_wait() {
    let rule = "proceed and state the assumption you proceed on";
    let todo = TodoWriteTool::new(Arc::new(TodoBoard::new(Vec::new()))).schema();
    let ask =
        crate::builtins::intent::AskUserQuestion::new(Arc::new(crate::builtins::intent::Headless))
            .schema();
    for d in [&todo.description, &ask.description] {
        assert!(d.contains(rule), "{d}");
        assert!(!d.contains("not permission to proceed"), "{d}");
    }
}
