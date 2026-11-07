//! A row that waits on ANOTHER SESSION's answer: the `ask` edge made through the room, the `owed`
//! edge the pass puts on the peer's board, and the four ways one ends.

use super::*;

const ROW: &str = "wire the port";
const QUESTION: &str = "which port?";

/// The edge a row carries once the room has taken the question: the id is the room's, and the room
/// is the one it resolved rather than the one the call left out.
fn asked(ask: u64) -> TodoNeed {
    TodoNeed::Ask {
        ask,
        of: "serenedb".into(),
        room: "host".into(),
        question: QUESTION.into(),
    }
}

fn waiting(content: &str) -> TodoItem {
    TodoItem {
        needs: vec![asked(4211)],
        ..item(content, TodoStatus::Pending)
    }
}

fn invoke(tool: &TodoWriteTool, args: &Value) -> Invocation {
    tool.write(args, None)
}

/// A `wait_on` call - the one field that leaves this session, over a job table that is empty.
fn waits(tool: &TodoWriteTool, v: Value) -> Invocation {
    tool.wait_on(&v, None)
}

/// A restated plan, as the model sends it: the whole list, and nothing else.
fn restated(tool: &TodoWriteTool, content: &str) -> Invocation {
    invoke(
        tool,
        &json!({"todos": [{"content": content, "status": "pending"}]}),
    )
}

/// The one door that asks: `wait_on` with `ask`. The room's id is what the edge carries, the room
/// it resolved is what the row records, and the reply shows the question back with the peer named.
#[test]
fn an_ask_made_through_the_room_becomes_an_edge_on_the_row() {
    let board = Arc::new(TodoBoard::new(vec![item(ROW, TodoStatus::Pending)]));
    let (tool, room) = ask_tool(board.clone());
    let r = waits(
        &tool,
        json!([{"content": ROW, "ask": {"session": "serenedb", "room": "host", "question": QUESTION}}]),
    );
    assert_eq!(r.outcome, ToolOutcome::Ok, "{}", r.payload);
    assert_eq!(board.snapshot()[0].needs, vec![asked(4211)]);
    assert_eq!(
        room.asked.lock().unwrap().as_slice(),
        [(
            "serenedb".to_string(),
            Some("host".to_string()),
            QUESTION.to_string()
        )],
        "the tool asked the room once, for the row's own words"
    );
    assert!(
        r.payload
            .contains("the session `serenedb` - asked \"which port?\" (ask #4211 in host)"),
        "{}",
        r.payload
    );
    assert!(
        r.payload.contains("Restating the row keeps the ask"),
        "the model is told what a restatement does to it: {}",
        r.payload
    );
}

/// A call with no `room` leaves the choice to the room, as `say` does: the edge records the room
/// the link resolved, not the field the call omitted.
#[test]
fn an_ask_with_no_room_takes_the_one_the_room_resolves() {
    let board = Arc::new(TodoBoard::new(vec![item(ROW, TodoStatus::Pending)]));
    let (tool, room) = ask_tool(board.clone());
    let r = waits(
        &tool,
        json!([{"content": ROW, "ask": {"session": "serenedb", "question": QUESTION}}]),
    );
    assert_eq!(r.outcome, ToolOutcome::Ok, "{}", r.payload);
    assert_eq!(
        room.asked.lock().unwrap().as_slice(),
        [("serenedb".to_string(), None, QUESTION.to_string())],
        "the call passed no room through, so the link chooses"
    );
    assert_eq!(
        board.snapshot()[0].needs,
        vec![TodoNeed::Ask {
            ask: 4211,
            of: "serenedb".into(),
            room: "proj".into(),
            question: QUESTION.into(),
        }]
    );
}

/// What the room says about a target that is not connected now reaches the model in this call: it
/// starts a grace on the room's clock, and a model that did not know would think the ask was lost.
#[test]
fn the_rooms_own_note_about_the_target_is_said_in_the_same_call() {
    let board = Arc::new(TodoBoard::new(vec![item(ROW, TodoStatus::Pending)]));
    let (tool, _room) = ask_tool(board.clone());
    let r = waits(
        &tool,
        json!([{"content": ROW, "ask": {"session": "arango", "question": "which port?"}}]),
    );
    assert_eq!(r.outcome, ToolOutcome::Ok, "{}", r.payload);
    assert!(
        r.payload
            .contains("`arango` is not connected now — the question waits for it"),
        "{}",
        r.payload
    );
}

/// Every refusal the room makes is the model's, in the same call, and nothing is written: the edge
/// is only ever put on after the question was taken.
#[test]
fn a_refused_ask_writes_nothing_and_says_why() {
    let board = Arc::new(TodoBoard::new(vec![item(ROW, TodoStatus::Pending)]));
    let (tool, _room) = ask_tool(board.clone());
    for (entry, why) in [
        (
            json!([{"content": ROW, "ask": {"session": "this session", "question": QUESTION}}]),
            "you cannot wait on your own answer",
        ),
        (
            json!([{"content": ROW, "ask": {"session": "nobody", "question": QUESTION}}]),
            "`nobody` is not a top-level session on this host",
        ),
    ] {
        let r = waits(&tool, entry.clone());
        assert_ne!(r.outcome, ToolOutcome::Ok, "{entry} was accepted");
        // The room's own sentence is the failed row's reason; the shape is the payload.
        let said = format!("{:?} {}", r.outcome, r.payload);
        assert!(said.contains(why), "{entry}: {said}");
        assert!(said.contains("`ask`"), "the shape is shown too: {said}");
        assert!(
            board.snapshot()[0].needs.is_empty(),
            "{entry} wrote an edge"
        );
    }
    // And a call that names no session, or asks nothing, is refused before the room is reached.
    for entry in [
        json!([{"content": ROW, "ask": {"question": QUESTION}}]),
        json!([{"content": ROW, "ask": {"session": "serenedb", "question": "  "}}]),
        json!([{"content": ROW, "ask": {"session": "serenedb", "question": QUESTION, "why": "x"}}]),
    ] {
        let r = waits(&tool, entry.clone());
        assert_ne!(r.outcome, ToolOutcome::Ok, "{entry} was accepted");
        assert!(
            board.snapshot()[0].needs.is_empty(),
            "{entry} wrote an edge"
        );
    }
}

/// A runtime with no room refuses an ask by name rather than not asking: a wait nothing will end
/// is worse than a refusal.
#[test]
fn an_ask_with_no_room_behind_it_is_refused() {
    let board = Arc::new(TodoBoard::new(vec![item(ROW, TodoStatus::Pending)]));
    let tool = TodoWriteTool::new(board.clone());
    let r = waits(
        &tool,
        json!([{"content": ROW, "ask": {"session": "serenedb", "question": QUESTION}}]),
    );
    assert_ne!(r.outcome, ToolOutcome::Ok);
    let said = format!("{:?} {}", r.outcome, r.payload);
    assert!(said.contains("has no room"), "{said}");
    assert!(board.snapshot()[0].needs.is_empty());
}

/// `needs` cannot spell `ask`: asking is a side effect in a room, and a plan is restated. The
/// refusal names the door that does ask, so the model is not left guessing.
#[test]
fn ask_is_refused_in_needs_by_name() {
    let (rt, board) = runtime();
    let r = invoke(
        &TodoWriteTool::new(board.clone()),
        &json!({"todos": [{"content": ROW, "status": "pending",
                    "needs": [{"kind": "ask", "ask": 4211, "of": "serenedb", "room": "host",
                               "question": QUESTION}]}]}),
    );
    assert_ne!(r.outcome, ToolOutcome::Ok, "{}", r.payload);
    let said = format!("{:?} {}", r.outcome, r.payload);
    assert!(said.contains("kind `ask`"), "{said}");
    assert!(said.contains("`wait_on`"), "{said}");
    assert!(board.snapshot().is_empty(), "nothing was written");
    // …and the same through the tool a runtime seats, so the refusal is not the free function's.
    let mut sink = RecordingToolSink::new();
    let r = rt.invoke(
        "t1",
        &call(
            r#"{"todos": [{"content": "deploy", "status": "pending",
                     "needs": [{"kind": "ask", "ask": 1, "of": "serenedb", "room": "host",
                                "question": "which port?"}]}]}"#,
        ),
        &mut sink,
    );
    assert_ne!(r.outcome, ToolOutcome::Ok);
    assert!(board.snapshot().is_empty());
}

/// A restated row keeps its ask, as it keeps an answer: the peer is still being nagged, so a
/// restatement that leaves the edge out must not release the asker's row.
#[test]
fn a_restated_row_keeps_its_ask() {
    let board = Arc::new(TodoBoard::new(vec![item(ROW, TodoStatus::Pending)]));
    let (tool, _room) = ask_tool(board.clone());
    let r = waits(
        &tool,
        json!([{"content": ROW, "ask": {"session": "serenedb", "room": "host", "question": QUESTION}}]),
    );
    assert_eq!(r.outcome, ToolOutcome::Ok, "{}", r.payload);
    let r = restated(&tool, ROW);
    assert_eq!(r.outcome, ToolOutcome::Ok, "{}", r.payload);
    assert_eq!(
        board.snapshot()[0].needs,
        vec![asked(4211)],
        "the restatement did not take the question back"
    );
}

/// `detach` takes it off - the door the blocked line names - and the edge is the only thing that
/// moves.
#[test]
fn detach_takes_the_ask_off() {
    let board = Arc::new(TodoBoard::new(vec![waiting(ROW)]));
    let (tool, _room) = ask_tool(board.clone());
    let r = waits(&tool, json!([{"content": ROW, "detach": true}]));
    assert_eq!(r.outcome, ToolOutcome::Ok, "{}", r.payload);
    assert!(board.snapshot()[0].needs.is_empty());
    assert_eq!(board.snapshot()[0].content, ROW, "the words are untouched");
}

/// The four endings, in the words the design spells: each replaces the ask edge in place with an
/// answer that keeps the room's id, and a second application moves nothing - the daemon's pass may
/// run again after a restart.
#[test]
fn each_settle_replaces_the_edge_with_the_one_sentence() {
    for (settle, said) in [
        (
            AskSettle::Answered("5433".into()),
            "`serenedb` answered: 5433",
        ),
        (
            AskSettle::Declined("not my area, ask arango".into()),
            "`serenedb` declined: not my area, ask arango",
        ),
        (
            AskSettle::Down,
            "`serenedb` was shut down for good (its session was deleted) - no answer will arrive",
        ),
        (
            AskSettle::Withdrawn("the operator".into()),
            "the ask was withdrawn by the operator",
        ),
    ] {
        let board = TodoBoard::new(vec![
            waiting(ROW),
            item("write the notes", TodoStatus::Pending),
        ]);
        let v = board.version();
        assert_eq!(board.settle_ask(4211, &settle), 1, "{said}");
        assert!(board.version() > v, "{said}: the board is news");
        let rows = board.snapshot();
        assert_eq!(
            rows[0].needs,
            vec![TodoNeed::Answered {
                question: QUESTION.into(),
                reply: said.to_string(),
                ask: Some(4211),
            }],
            "{said}"
        );
        assert_eq!(
            rows[0].status,
            TodoStatus::Pending,
            "{said}: the row is released, not moved"
        );
        assert_eq!(board.settle_ask(4211, &settle), 0, "{said}: idempotent");
        // The row is work again, and the settled sentence is on it for the model to read.
        let text = board_text(&board.snapshot());
        assert!(
            text.contains(&format!("(answered: you asked \"which port?\" — {said})")),
            "{text}"
        );
        assert!(
            nag_for(&board.snapshot())
                .expect("the row is asked about again")
                .contains(&format!("  - {ROW}")),
            "{said}: the row is released"
        );
    }
}

/// A settle lands on every row that carries the id: one ask can be waited on by more than one row,
/// and all of them are released together.
#[test]
fn one_settle_releases_every_row_that_carries_the_ask() {
    let board = TodoBoard::new(vec![waiting(ROW), waiting("and announce it")]);
    assert_eq!(board.settle_ask(4211, &AskSettle::Down), 2);
    for row in board.snapshot() {
        assert!(matches!(
            row.needs.as_slice(),
            [TodoNeed::Answered {
                ask: Some(4211),
                ..
            }]
        ));
    }
    // And an id nothing carries is not a move.
    assert_eq!(board.settle_ask(9999, &AskSettle::Down), 0);
}

/// The row waits on a peer, so it is listed as blocked while the check asks about the rest of the
/// plan - and the sentence names the peer, the question, and the detach door.
#[test]
fn a_row_waiting_on_a_session_is_blocked_and_the_rest_of_the_plan_continues() {
    let plan = vec![waiting(ROW), item("write the notes", TodoStatus::Pending)];
    let said = nag_for(&plan).expect("the other row is asked about");
    assert!(said.contains("  - write the notes"), "{said}");
    assert!(
        !said.contains(&format!("  - {ROW}")),
        "the waiting row is not asked about: {said}"
    );
    assert!(said.contains("waiting on another session"), "{said}");
    assert!(
        said.contains("the SESSION `serenedb` — \"which port?\" (ask #4211)"),
        "{said}"
    );
    assert!(said.contains("Keep going"), "{said}");
    assert!(said.contains("\"detach\": true"), "{said}");
}

/// A plan whose only open work waits on a peer is silent: nothing there is the model's to unblock,
/// and the settlement arrives by itself.
#[test]
fn a_plan_waiting_only_on_a_session_is_not_nagged() {
    assert_eq!(nag_for(&[waiting(ROW)]), None);
    let after = needs("announce it", TodoStatus::Pending, &[ROW]);
    assert_eq!(nag_for(&[waiting(ROW), after]), None);
    // A broken edge beside it is still said: that one IS the model's to fix.
    let broken = needs("tidy up", TodoStatus::Pending, &["no such row"]);
    assert!(nag_for(&[waiting(ROW), broken]).is_some());
}

/// The other side of an ask is the PEER's board: a row carrying `owed` is ready, is asked about,
/// and says which ask it answers.
#[test]
fn an_owed_row_is_ready_and_is_asked_about() {
    let owed = TodoItem {
        needs: vec![TodoNeed::Owed { ask: 4211 }],
        ..item(
            "`arango` asked you #4211 — wire the port: which port?",
            TodoStatus::InProgress,
        )
    };
    let said = nag_for(std::slice::from_ref(&owed)).expect("the row is the model's work");
    assert!(said.contains("arango` asked you #4211"), "{said}");
    let text = board_text(&[owed]);
    assert!(text.contains("[~] `arango` asked you #4211"), "{text}");
    assert!(
        !text.contains("waiting on"),
        "an owed edge is met, so nothing is listed as waited on: {text}"
    );
}

/// An ask on a row that is already answered is not a second wait, and the id is what tells the
/// operator's answers and the room's apart.
#[test]
fn an_answered_ask_is_met_and_carries_its_id() {
    let settled = TodoItem {
        needs: vec![TodoNeed::Answered {
            question: QUESTION.into(),
            reply: "`serenedb` answered: 5433".into(),
            ask: Some(4211),
        }],
        ..item(ROW, TodoStatus::Pending)
    };
    assert!(nag_for(&[settled]).is_some(), "the row is work again");
    // And the operator's answer, which carries no id, reads the same way.
    let theirs = TodoItem {
        needs: vec![TodoNeed::Answered {
            question: "which db?".into(),
            reply: "postgres".into(),
            ask: None,
        }],
        ..item(ROW, TodoStatus::Pending)
    };
    assert!(nag_for(&[theirs]).is_some());
}
