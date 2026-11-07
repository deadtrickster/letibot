//! A row that waits on another session's answer, end to end: two sessions with their own boards and
//! their own room links against one coordinator, the shipped `todo_write` and `room` tools, and the
//! shipped pass. A asks, B answers, and A's row is released. Then B's connection goes away: after
//! the grace the operator is asked to restart B and A's row stays blocked. B comes back and answers,
//! the card goes, and the row is released. Then B's session is deleted and a second ask ends `DOWN`.
//!
//! The grace is [`letibot_harnessd::room::ASK_GRACE_ENV`], so the run is seconds rather than five
//! minutes, and nothing here sleeps for a timeout to expire.

use letibot_harnessd::config::Config;
use letibot_harnessd::harness::Parts;
use letibot_harnessd::{Harness, room as daemon_room};
use letibot_room::{Room, Server};
use letibot_sessionlog::hub::Hub;
use letibot_tokencore::store::{TodoItem, TodoNeed};
use letibot_tools::{NullToolSink, ToolResult};
use letibot_transcript::{ToolCall, ToolOutcome};

fn scratch(what: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("letibot-room-{what}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a directory of this test's own");
    dir
}

/// One session: its own hub, board, socket link and pass - what a daemon gives a top-level session,
/// with the daemon loop left out because nothing here runs a turn.
struct Seat {
    h: Harness,
    id: String,
}

impl Seat {
    /// Both seats work in ONE workspace, which is what puts them in one project room: two sessions of
    /// one repository are the case an ask is for, and a session of another project cannot see it.
    fn open(workspace: &std::path::Path, id: &str) -> Seat {
        std::fs::create_dir_all(workspace).expect("a workspace");
        let mut cfg = Config::for_this_box(workspace);
        cfg.session_id = id.to_string();
        cfg.vocab_gguf = None;
        cfg.model = "local".to_string();
        cfg.room_link = true;
        let parts = Parts::load(&cfg).expect("the byte vocabulary needs no model file");
        let h = Harness::open(&parts, cfg, Hub::new(id)).expect("the session opens");
        assert!(h.has_room(), "`{id}` must have the room");
        Seat {
            h,
            id: id.to_string(),
        }
    }

    fn tool(&self, name: &str, arguments: serde_json::Value) -> ToolResult {
        let call = ToolCall {
            id: format!("call_{name}"),
            name: name.into(),
            arguments: arguments.to_string(),
        };
        let mut sink = NullToolSink;
        self.h.runtime_handle().invoke("t1", &call, &mut sink)
    }

    fn write(&self, arguments: serde_json::Value) -> String {
        let out = self.tool("todo_write", arguments);
        assert_eq!(out.outcome, ToolOutcome::Ok, "{out:?}");
        out.render()
    }

    fn room(&self, arguments: serde_json::Value) -> String {
        let out = self.tool("room", arguments);
        assert_eq!(out.outcome, ToolOutcome::Ok, "{out:?}");
        out.render()
    }

    fn rows(&self) -> Vec<TodoItem> {
        self.h.todo_list()
    }

    fn row(&self, content: &str) -> TodoItem {
        self.rows()
            .into_iter()
            .find(|r| r.content.trim() == content)
            .unwrap_or_else(|| {
                panic!(
                    "`{content}` is not on `{}`'s board: {:?}",
                    self.id,
                    self.rows()
                )
            })
    }
}

/// The ask a row carries, and the peer it names.
fn ask_of(row: &TodoItem) -> (u64, String) {
    row.needs
        .iter()
        .find_map(|n| match n {
            TodoNeed::Ask { ask, of, .. } => Some((*ask, of.clone())),
            _ => None,
        })
        .unwrap_or_else(|| panic!("`{}` carries no ask: {:?}", row.content, row.needs))
}

#[test]
fn an_ask_releases_a_row_escalates_when_its_target_stops_and_ends_when_the_target_is_deleted() {
    let dir = scratch("asks-e2e");
    let socket = dir.join("room.sock");
    // SAFETY: this binary holds one test, so nothing else in the process reads these variables
    // while they are set. They are the doors the room and its grace are read through, and they
    // keep the whole test inside `dir`.
    unsafe {
        std::env::set_var(daemon_room::SOCKET_ENV, &socket);
        std::env::set_var(daemon_room::ASK_GRACE_ENV, "0");
    }
    let server = Server::serve(&socket, Room::new()).expect("a coordinator");
    // The same knob `Coordinator::start` reads: the grace is configuration, so this run does not
    // wait five minutes for a card to be raised.
    let grace = daemon_room::ask_grace_ms().expect("this test sets the grace");
    server.with_ask_grace(grace);
    let workspace = dir.join("ws");
    let mut a = Seat::open(&workspace, "s-a");
    let mut b = Seat::open(&workspace, "s-b");

    // A's plan has a row it cannot do alone, and it asks B for the port.
    a.write(serde_json::json!({
        "todos": [{"content": "wire the port", "status": "in_progress"}]
    }));
    let said = a.write(serde_json::json!({
        "wait_on": [{"content": "wire the port",
                     "ask": {"session": "s-b", "question": "which port?"}}]
    }));
    assert!(said.contains("which port?"), "{said}");
    let (first, of) = ask_of(&a.row("wire the port"));
    assert_eq!(of, "s-b");

    // B's pass turns the ask into a row it owes - the nag that says a peer is blocked on it.
    b.h.fill_room_board();
    let owed: Vec<TodoItem> = b
        .rows()
        .into_iter()
        .filter(|r| {
            r.needs
                .iter()
                .any(|n| matches!(n, TodoNeed::Owed { ask } if *ask == first))
        })
        .collect();
    assert_eq!(owed.len(), 1, "one row for one ask: {:?}", b.rows());
    assert!(
        owed[0].content.contains("`s-a` asked you"),
        "the row names the asker: {}",
        owed[0].content
    );
    assert!(
        owed[0].content.contains("which port?"),
        "and the question: {}",
        owed[0].content
    );

    // B answers with the verb the ask is settled by.
    let said = b.room(serde_json::json!({"verb": "answer", "ask": first, "text": "5433"}));
    assert!(said.contains("answered: 5433"), "{said}");

    // A's pass applies the settle: the edge becomes the reply, in place, and the row is released.
    a.h.fill_room_board();
    let row = a.row("wire the port");
    assert!(
        matches!(&row.needs[0], TodoNeed::Answered { reply, ask: Some(id), .. }
            if reply.contains("5433") && *id == first),
        "{:?}",
        row.needs
    );
    b.h.fill_room_board();
    assert!(
        !b.rows().iter().any(|r| r
            .needs
            .iter()
            .any(|n| matches!(n, TodoNeed::Owed { ask } if *ask == first))),
        "and the row it owed is retired: {:?}",
        b.rows()
    );

    // ---------------------------------------------------------------------------------------
    // B's connection goes away. That is a fault to repair, never an answer: the ask stays open,
    // and past the grace the operator is asked to restart B.
    // ---------------------------------------------------------------------------------------
    let second = {
        a.write(serde_json::json!({
            "todos": [{"content": "name the user", "status": "in_progress"}]
        }));
        a.write(serde_json::json!({
            "wait_on": [{"content": "name the user",
                         "ask": {"session": "s-b", "question": "and the user?"}}]
        }));
        ask_of(&a.row("name the user")).0
    };
    drop(b);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline && !server.connected().iter().any(|s| s == "s-a") {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    while std::time::Instant::now() < deadline && server.gone_since("s-b").is_none() {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        server.gone_since("s-b").is_some(),
        "the room must have noticed B's connection closing"
    );
    let _ = server.tick(letibot_room::server::now_ms());
    // The room's own answer first: the ask is still open and marked escalated.
    let (by_me, _) =
        a.h.room_link()
            .expect("A's link")
            .asks()
            .expect("the room answers an asks read");
    let marked = by_me
        .iter()
        .find(|x| x.seq == second)
        .unwrap_or_else(|| panic!("ask {second} is not in {by_me:?}"));
    assert!(
        marked.open() && marked.escalated,
        "the room must mark the ask escalated, and only escalate: {marked:?}"
    );
    a.h.fill_room_board();
    let row = a.row("name the user");
    assert!(
        row.needs
            .iter()
            .any(|n| matches!(n, TodoNeed::Ask { ask, .. } if *ask == second)),
        "a timeout settles nothing: the row still waits on B - {:?}",
        row.needs
    );
    let card = row
        .needs
        .iter()
        .find_map(|n| match n {
            TodoNeed::Operator { question, .. } => Some(question.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("the operator must be asked to restart B: {:?}", row.needs));
    assert!(card.contains("`s-b` stopped"), "{card}");
    assert!(
        card.contains("which port?") || card.contains("and the user?"),
        "{card}"
    );
    assert!(card.contains("restart"), "{card}");

    // ---------------------------------------------------------------------------------------
    // B comes back: the card is withdrawn the moment it is connected, and the ask is still the
    // same ask - so the answer that arrives settles the row that was waiting all along.
    // ---------------------------------------------------------------------------------------
    let mut b = Seat::open(&workspace, "s-b");
    a.h.fill_room_board();
    let row = a.row("name the user");
    assert!(
        !row.needs
            .iter()
            .any(|n| matches!(n, TodoNeed::Operator { .. })),
        "a reconnect clears the escalation: {:?}",
        row.needs
    );
    assert!(
        row.needs
            .iter()
            .any(|n| matches!(n, TodoNeed::Ask { ask, .. } if *ask == second)),
        "and the ask is untouched: {:?}",
        row.needs
    );
    b.h.fill_room_board();
    let said = b.room(serde_json::json!({"verb": "answer", "ask": second, "text": "admin"}));
    assert!(said.contains("answered: admin"), "{said}");
    a.h.fill_room_board();
    let row = a.row("name the user");
    assert!(
        matches!(&row.needs[0], TodoNeed::Answered { reply, .. } if reply.contains("admin")),
        "{:?}",
        row.needs
    );

    // ---------------------------------------------------------------------------------------
    // B's session is DELETED - the one `DOWN`. A third ask ends with no answer, and the row says
    // why rather than waiting for a board that no longer exists.
    // ---------------------------------------------------------------------------------------
    a.write(serde_json::json!({
        "todos": [{"content": "pick the schema", "status": "in_progress"}]
    }));
    a.write(serde_json::json!({
        "wait_on": [{"content": "pick the schema",
                     "ask": {"session": "s-b", "question": "tenant or shared?"}}]
    }));
    let third = ask_of(&a.row("pick the schema")).0;
    let said = daemon_room::link::retire("s-b").expect("the room is told");
    assert!(!said.is_empty(), "{said}");
    a.h.fill_room_board();
    let row = a.row("pick the schema");
    assert!(
        matches!(&row.needs[0], TodoNeed::Answered { reply, ask: Some(id), .. }
            if reply.contains("shut down for good") && reply.contains("`s-b`") && *id == third),
        "a deleted session ends the ask with no answer: {:?}",
        row.needs
    );

    // Nothing here left a card for the operator behind: a deleted target is the one ending that
    // needs no restart.
    for row in a.rows() {
        assert!(
            !row.needs
                .iter()
                .any(|n| matches!(n, TodoNeed::Operator { .. })),
            "no operator card is left: {row:?}"
        );
    }

    drop(a);
    drop(b);
    drop(server);
    let _ = std::fs::remove_dir_all(&dir);
}
