//! An `external` wait: quiet for a bounded window from its stamp, then asked about again, and
//! steered to an ask or an operator wait when its words name something letibot can track.

use super::*;

use std::time::Instant;

const ROW: &str = "push once the review lands";
const T0: u64 = 1_700_000_000_000;

fn hours(h: u64) -> u64 {
    h * 3_600_000
}

fn waiting(what: &str, since_ms: Option<u64>) -> TodoItem {
    TodoItem {
        needs: vec![TodoNeed::External {
            what: what.into(),
            since_ms,
        }],
        ..item(ROW, TodoStatus::InProgress)
    }
}

/// Inside the window the row is silent and the clock is armed for its end; past it the row is
/// asked about with the three acts that end the question, and then again on the ladder.
#[test]
fn an_external_wait_is_quiet_for_its_window_and_asked_about_after_it() {
    let board = vec![waiting("R156's review at arangodb", Some(T0))];
    let none = ChildSessions::none();
    let inside = RunningJobs::none().at(T0 + hours(1));
    assert!(!the_check_may_ask_about(&board[0], &none, &inside));
    assert!(the_plan_as_checked(&board, &none, &inside).is_empty());
    assert_eq!(
        nag_with(&board, &none, &inside),
        None,
        "rooted inside the window"
    );
    assert_eq!(
        when_an_external_wait_lapses(&board, &inside),
        Some(Duration::from_millis(hours(1)) + Duration::from_secs(1)),
    );

    let mut clock = TodoNagClock::with_window(Duration::from_secs(60));
    let t0 = Instant::now();
    clock.rearm(&board, t0, &none, &inside);
    assert_eq!(
        clock.due_at(),
        Some(t0 + Duration::from_millis(hours(1)) + Duration::from_secs(1)),
        "the window's end is a deadline, so the silence ends with no row movement"
    );

    let after = RunningJobs::none().at(T0 + hours(2) + 1);
    assert!(the_check_may_ask_about(&board[0], &none, &after));
    let askable = the_plan_as_checked(&board, &none, &after);
    let text = unfinished_plan(&askable, &board, &none, &after).expect("asked about again");
    assert!(text.contains(ROW), "{text}");
    assert!(text.contains("waited on for over 2 hours"), "{text}");
    assert!(text.contains("\"detach\": true"), "{text}");
    assert_eq!(when_an_external_wait_lapses(&board, &after), None);

    // Delivered once, then asked again on the ladder rather than going quiet.
    let t1 = t0 + Duration::from_millis(hours(1)) + Duration::from_secs(1);
    assert!(clock.due_now(t1));
    clock.rearm(&board, t1, &none, &after);
    assert_eq!(
        clock.due_at(),
        Some(t1 + Duration::from_secs(60)),
        "a new plan"
    );
    clock.said(Some(text), t1 + Duration::from_secs(60));
    clock.rearm(&board, t1 + Duration::from_secs(61), &none, &after);
    assert_eq!(
        clock.due_at(),
        Some(t1 + Duration::from_secs(60) + Duration::from_secs(120)),
        "the next ladder step, doubled"
    );
}

/// A wait stored before the window existed has no stamp and is past it.
#[test]
fn an_unstamped_external_wait_is_past_its_window() {
    let row = waiting("upstream CI", None);
    assert!(the_check_may_ask_about(
        &row,
        &ChildSessions::none(),
        &RunningJobs::none().at(T0)
    ));
}

/// Restating the same wait in `todos` keeps its stamp; attaching it again with `wait_on` restarts
/// the window. Otherwise a model restating its list every turn would never be asked.
#[test]
fn a_restatement_keeps_the_stamp_and_a_reattach_restarts_it() {
    let board = Arc::new(TodoBoard::new(vec![waiting("upstream CI", Some(T0))]));
    let tool = TodoWriteTool::new(board.clone());
    let r = tool.write(
        &json!({"todos": [{"content": ROW, "status": "in_progress",
                "needs": [{"kind": "external", "what": "upstream CI"}]}]}),
        None,
    );
    assert_eq!(r.outcome, ToolOutcome::Ok, "{}", r.payload);
    assert_eq!(
        board.snapshot()[0].needs,
        waiting("upstream CI", Some(T0)).needs
    );

    let r = tool.wait_on(&json!([{"content": ROW, "external": "upstream CI"}]), None);
    assert_eq!(r.outcome, ToolOutcome::Ok, "{}", r.payload);
    let needs = board.snapshot()[0].needs.clone();
    assert!(
        matches!(needs.as_slice(), [TodoNeed::External { since_ms: Some(s), .. }] if *s > T0),
        "one wait, stamped now: {needs:?}"
    );
}

/// A wait naming a roster seat is an ask of it: the room settles it when the seat answers.
#[test]
fn an_external_naming_a_roster_seat_becomes_an_ask() {
    let board = Arc::new(TodoBoard::new(vec![item(ROW, TodoStatus::Pending)]));
    let (tool, room) = ask_tool(board.clone());
    let what = "cc@etcd-752's go-ahead in the room";
    let r = tool.wait_on(&json!([{"content": ROW, "external": what}]), None);
    assert_eq!(r.outcome, ToolOutcome::Ok, "{}", r.payload);
    assert_eq!(
        board.snapshot()[0].needs,
        vec![TodoNeed::Ask {
            ask: 4213,
            of: "lab2x1/cc-etcd752".into(),
            room: "proj".into(),
            question: what.into(),
        }]
    );
    assert_eq!(room.asked.lock().unwrap().len(), 1);
    assert!(
        r.payload.contains("became an `ask` of `lab2x1/cc-etcd752`"),
        "{}",
        r.payload
    );
}

/// A wait naming the operator is an operator wait, with a choice they can answer.
#[test]
fn an_external_naming_the_operator_becomes_an_operator_wait() {
    let board = Arc::new(TodoBoard::new(vec![item(ROW, TodoStatus::Pending)]));
    let (tool, room) = ask_tool(board.clone());
    let what = "the operator's word or go";
    let r = tool.wait_on(&json!([{"content": ROW, "external": what}]), None);
    assert_eq!(r.outcome, ToolOutcome::Ok, "{}", r.payload);
    assert_eq!(
        board.snapshot()[0].needs,
        vec![TodoNeed::Operator {
            question: what.into(),
            options: vec!["go ahead".into(), "hold".into()],
        }]
    );
    assert!(
        room.asked.lock().unwrap().is_empty(),
        "nobody asked in the room"
    );
    assert!(
        r.payload.contains("became an `operator` wait"),
        "{}",
        r.payload
    );
}

/// The seat comes first in the words, so the wait is the seat's: the operator's word follows it.
#[test]
fn the_first_thing_named_wins() {
    let board = Arc::new(TodoBoard::new(vec![item(ROW, TodoStatus::Pending)]));
    let (tool, _room) = ask_tool(board.clone());
    let what = "cc@etcd-752 reading the server archive, then the operator's word";
    let r = tool.wait_on(&json!([{"content": ROW, "external": what}]), None);
    assert_eq!(r.outcome, ToolOutcome::Ok, "{}", r.payload);
    assert!(
        matches!(board.snapshot()[0].needs.as_slice(), [TodoNeed::Ask { of, .. }] if of == "lab2x1/cc-etcd752")
    );
}

/// Words that name nothing on the roster and no operator phrase stay external, stamped.
#[test]
fn a_truly_external_wait_stays_external() {
    for what in [
        "upstream CI",
        "github issue response",
        "etcd-operator's next release",
        "the operators' forum thread at serenedb-io",
    ] {
        let board = Arc::new(TodoBoard::new(vec![item(ROW, TodoStatus::Pending)]));
        let (tool, room) = ask_tool(board.clone());
        let r = tool.wait_on(&json!([{"content": ROW, "external": what}]), None);
        assert_eq!(r.outcome, ToolOutcome::Ok, "{}", r.payload);
        assert!(
            matches!(board.snapshot()[0].needs.as_slice(),
                [TodoNeed::External { what: w, since_ms: Some(_) }] if w == what),
            "{what}: {:?}",
            board.snapshot()[0].needs
        );
        assert!(room.asked.lock().unwrap().is_empty(), "{what}");
        assert!(r.payload.contains("stays `external`"), "{}", r.payload);
    }
}

/// A row waiting on a running process arms the clock for the next read of the process table:
/// nothing reports its exit, so without the poll the wait never ends.
#[test]
fn a_wait_on_a_running_process_arms_the_clock_for_its_next_read() {
    let board = vec![TodoItem {
        needs: vec![TodoNeed::Pid {
            pid: 4242,
            start: 7,
        }],
        ..item(ROW, TodoStatus::InProgress)
    }];
    let none = ChildSessions::none();
    let up = RunningJobs::none().pid(4242, 7);
    assert!(
        the_plan_as_checked(&board, &none, &up).is_empty(),
        "quiet while it runs"
    );
    let mut clock = TodoNagClock::with_window(Duration::from_secs(600));
    let t0 = Instant::now();
    clock.rearm(&board, t0, &none, &up);
    assert_eq!(clock.due_at(), Some(t0 + PID_POLL), "read again, not never");
    let gone = RunningJobs::none();
    assert_eq!(when_a_waited_process_is_read_again(&board, &gone), None);
    assert!(
        !the_plan_as_checked(&board, &none, &gone).is_empty(),
        "and once it is gone the row is asked about"
    );
}
