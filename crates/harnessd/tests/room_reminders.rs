//! **A holder of a room item gets a todo row, and the idle check carries it** — `docs/room.md`
//! § *Still owed* item 3, end to end: a real socket, a real `Harness`, the real `room` tool, and
//! the real nag.
//!
//! # The five things this file exists to have caught
//!
//! 1. **One row per item, updated not appended.** The pass runs every few seconds, and the board's
//!    key is a row's exact `content` — so a row that carried a clock (`… for 4m`) would be a new row
//!    every pass, which is a nag machine. This drives the pass three times over one claim and
//!    asserts the board grew by one row and then by none.
//! 2. **Retired when the item is.** `done` takes the row away in the pass that sees it, and the
//!    room's record is the reason: a room row is the ROOM seen from a session, not a plan.
//! 3. **Top-level sessions only.** A session with a `parent_session_id` gets no link at all, so the
//!    pass has nothing to ask and writes nothing — and a claim by *another* session is not this
//!    session's row.
//! 4. **`wait_on` names the room item**, and that edge is the whole of quiet-or-loud: while it is on
//!    the row the idle check says nothing; the moment it comes off, the check names the row.
//! 5. **The row is the session's own** (`by: Model`, which the nag draws as *yours*), never the
//!    operator's — and a row that already says the same thing is reused rather than doubled.
//!
//! # What it needs, and what it does not
//!
//! No model, no store, no GGUF: the byte vocabulary, a `Server::serve`d room on a socket of this
//! test's own, and no turn is ever run. The room is the plain [`letibot_room::Server`] rather than a
//! `--room` coordinator, because the coordinator's arbiter is a session and this file is about a
//! different session entirely.
//!
//! # One test per binary
//!
//! `LETIBOT_ROOM_SOCKET` is process-wide, so this file holds **one** test: a second one would race
//! it for the variable.

use std::sync::Arc;

use letibot_harnessd::config::Config;
use letibot_harnessd::harness::Parts;
use letibot_harnessd::{Harness, room as daemon_room};
use letibot_room::client::Client;
use letibot_room::{Request, Response, Room, Server, Speaker};
use letibot_sessionlog::hub::Hub;
use letibot_tokencore::store::{TodoBy, TodoNeed, TodoStatus};
use letibot_tools::NullToolSink;
use letibot_transcript::{ToolCall, ToolOutcome};

/// A directory of this test's own, so nothing touches the box's data directory.
fn scratch(what: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("letibot-room-{what}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a directory of this test's own");
    dir
}

/// One `room` call, through the runtime the session actually invokes through.
fn room(h: &Harness, args: serde_json::Value) -> String {
    let call = ToolCall {
        id: format!("call_{}", serde_json::to_string(&args).expect("args")),
        name: "room".into(),
        arguments: args.to_string(),
    };
    let mut sink = NullToolSink;
    let out = h.runtime_handle().invoke("t1", &call, &mut sink);
    assert_eq!(out.outcome, ToolOutcome::Ok, "{out:?}");
    out.render()
}

#[test]
fn a_holder_is_reminded_by_its_own_row_and_the_idle_check_carries_it() {
    let dir = scratch("reminders");
    let socket = dir.join("room.sock");
    // SAFETY: this binary holds one test, so nothing else in the process reads this variable while
    // it is set. It is the door `daemon_room::socket_path` reads, and it is what keeps the whole
    // test inside `dir` — no fixture here touches the box's own room.
    unsafe { std::env::set_var(daemon_room::SOCKET_ENV, &socket) };
    let server = Server::serve(&socket, Room::new()).expect("a room on this test's socket");

    let workspace = dir.join("workspace");
    std::fs::create_dir_all(&workspace).expect("a workspace");
    let mut cfg = Config::for_this_box(&workspace);
    cfg.vocab_gguf = None;
    cfg.model = "local".to_string();
    // The one flag that matters: this session may speak in the room. The link is built at open,
    // and only when the socket exists — which is why the server is started first.
    cfg.room_link = true;
    let me = cfg.session_id.clone();
    let parts = Parts::load(&cfg).expect("the byte vocabulary needs no model file");
    let mut h = Harness::open(&parts, cfg, Hub::new(&me)).expect("the session opens");
    assert!(h.has_room(), "a top-level session with a socket must have the room");

    // -----------------------------------------------------------------------------------------
    // The hold. `post` then `claim` is the whole of it, and the reply names the item.
    // -----------------------------------------------------------------------------------------
    room(
        &h,
        serde_json::json!({"verb": "post", "tag": "ram", "body": "8 GB, ~10 min"}),
    );
    let said = room(&h, serde_json::json!({"verb": "claim", "tag": "ram"}));
    assert!(said.contains("you hold #1"), "{said}");

    // -----------------------------------------------------------------------------------------
    // 1 + 5. One row, the session's own, and its words carry no clock.
    // -----------------------------------------------------------------------------------------
    assert_eq!(h.fill_room_board(), 1, "the pass writes one row for one claim");
    let rows = h.todo_list();
    let mine: Vec<_> = rows
        .iter()
        .filter(|r| r.content.starts_with("you hold `ram` #1"))
        .collect();
    assert_eq!(mine.len(), 1, "one row for one item: {rows:?}");
    let row = mine[0];
    assert_eq!(
        row.content, "you hold `ram` #1 — 8 GB, ~10 min",
        "the words are stable: tag, number, body, and no duration"
    );
    assert_eq!(
        row.by,
        TodoBy::Model,
        "the row is the session's OWN — the nag draws it as `yours`, and `update` disposes of it"
    );
    assert_ne!(row.by, TodoBy::Operator, "never the operator's");
    assert_eq!(row.status, TodoStatus::InProgress);
    assert_eq!(
        row.needs,
        vec![TodoNeed::Room {
            item: 1,
            working: false
        }],
        "nothing was declared as a wait, so nothing is coming: the edge says so on the row"
    );

    // **And the existing nag carries it, with no model call.** This is the whole deliverable: the
    // sentence the idle check composes names the row, so a holder that never asks is still told.
    let nag = h.nag_notice().expect("an unfinished plan must be nagged about");
    assert!(
        nag.contains("you hold `ram` #1 — 8 GB, ~10 min"),
        "the check must name the row: {nag}"
    );
    assert!(nag.contains("yours"), "and it must be the session's own: {nag}");

    // -----------------------------------------------------------------------------------------
    // 1 again. A SECOND pass is not a second row — the nag machine this design exists to avoid.
    // -----------------------------------------------------------------------------------------
    let before = h.todo_list().len();
    assert_eq!(h.fill_room_board(), 1);
    assert_eq!(h.fill_room_board(), 1);
    assert_eq!(
        h.todo_list().len(),
        before,
        "two more passes over one claim add nothing: {:?}",
        h.todo_list()
    );

    // -----------------------------------------------------------------------------------------
    // 4. The holder marks what it waits on, and the row goes QUIET — `wait_on` names the item.
    // -----------------------------------------------------------------------------------------
    room(
        &h,
        serde_json::json!({"verb": "waiting", "item": 1, "text": "the suite"}),
    );
    assert_eq!(h.fill_room_board(), 1, "the same row, updated");
    let rows = h.todo_list();
    let row = rows
        .iter()
        .find(|r| r.content.starts_with("you hold `ram` #1"))
        .expect("the row is still there");
    assert_eq!(
        row.needs,
        vec![TodoNeed::Room {
            item: 1,
            working: true
        }],
        "the edge names the room item, and it is the whole of quiet-or-loud"
    );
    assert_eq!(
        rows.len(),
        before,
        "marking a wait is an UPDATE of one row, never a second: {rows:?}"
    );
    assert!(
        h.nag_notice().is_none(),
        "the room is working on what the holder waits for, so the check says nothing: {:?}",
        h.nag_notice()
    );

    // …and clearing the wait makes it loud again, in the pass that sees it.
    room(
        &h,
        serde_json::json!({"verb": "waiting", "item": 1, "text": ""}),
    );
    assert_eq!(h.fill_room_board(), 1);
    assert_eq!(
        h.todo_list()
            .iter()
            .find(|r| r.content.starts_with("you hold `ram` #1"))
            .expect("the row")
            .needs,
        vec![TodoNeed::Room {
            item: 1,
            working: false
        }],
        "the wait ended, so the holder is the one who must act"
    );
    assert!(
        h.nag_notice().is_some_and(|nag| nag.contains("you hold `ram` #1")),
        "and the check names it again"
    );

    // -----------------------------------------------------------------------------------------
    // 5 again. A row that already says the same thing is REUSED, not doubled.
    // -----------------------------------------------------------------------------------------
    let twice = {
        // The pass writes the same words the model already has on its board.
        let rows = h.todo_list();
        let same = rows
            .iter()
            .find(|r| r.content.starts_with("you hold `ram` #1"))
            .cloned()
            .expect("the row");
        let n = h.todo_list().len();
        // A second identical pass is the cheapest possible way to say it: nothing is appended.
        h.fill_room_board();
        let after = h.todo_list();
        assert_eq!(after.len(), n, "no second copy of one sentence: {after:?}");
        same
    };
    assert_eq!(twice.content, "you hold `ram` #1 — 8 GB, ~10 min");

    // -----------------------------------------------------------------------------------------
    // 2. `done` retires the row, in the same pass that sees it.
    // -----------------------------------------------------------------------------------------
    room(&h, serde_json::json!({"verb": "done", "item": 1}));
    assert_eq!(h.fill_room_board(), 0, "nothing is held any more");
    assert!(
        h.todo_list()
            .iter()
            .all(|r| !r.content.starts_with("you hold `ram` #1")),
        "the row went with the item: {:?}",
        h.todo_list()
    );
    assert!(
        h.nag_notice().is_none(),
        "and a board with nothing on it says nothing"
    );

    // -----------------------------------------------------------------------------------------
    // 3. A claim by ANOTHER session is not this session's row.
    // -----------------------------------------------------------------------------------------
    let other = Client::connect(&socket, Speaker::top_level("s-other"), Arc::new(|_| {}))
        .expect("another session joins the room");
    let _ = other.request(&Request::Post {
        id: "p2".into(),
        tag: "cpu".into(),
        body: "four cores".into(),
    });
    let claimed = other
        .request(&Request::Claim {
            id: "c2".into(),
            tag: "cpu".into(),
        })
        .expect("a claim");
    assert!(matches!(claimed, Response::Claimed { .. }), "{claimed:?}");
    assert_eq!(
        h.fill_room_board(),
        0,
        "somebody else's claim is not this session's reminder"
    );
    assert!(
        h.todo_list()
            .iter()
            .all(|r| !r.content.contains("`cpu`")),
        "{:?}",
        h.todo_list()
    );

    // -----------------------------------------------------------------------------------------
    // 3 again. A subagent gets no link and no row — the room's own rule, twice.
    // -----------------------------------------------------------------------------------------
    let child_cfg = {
        let mut c = Config::for_this_box(&workspace);
        c.vocab_gguf = None;
        c.model = "local".to_string();
        c.room_link = true;
        c.parent_session_id = Some(me.clone());
        c
    };
    let child_id = child_cfg.session_id.clone();
    let mut child = Harness::open(&parts, child_cfg, Hub::new(&child_id))
        .expect("a subagent's session opens");
    assert!(
        !child.has_room(),
        "a subagent speaks through its parent: `Harness::open` gives it no link at all"
    );
    assert_eq!(child.fill_room_board(), 0);
    assert!(
        child.todo_list().is_empty(),
        "and it gets no room row it did not hold: {:?}",
        child.todo_list()
    );

    drop(server);
}
