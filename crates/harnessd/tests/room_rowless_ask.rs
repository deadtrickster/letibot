//! An ask made with the `room` tool and no board row behind it, end to end: two sessions with
//! their own boards and links against one coordinator, and the shipped pass between the ask and
//! the answer. The pass must not take the ask back, and the answer must wake the asker saying the
//! ask is settled.

use letibot_harnessd::config::Config;
use letibot_harnessd::harness::Parts;
use letibot_harnessd::{Harness, room as daemon_room};
use letibot_room::{Room, Server};
use letibot_sessionlog::hub::Hub;
use letibot_tools::{NullToolSink, ToolResult};
use letibot_transcript::{ToolCall, ToolOutcome};

fn open(workspace: &std::path::Path, id: &str) -> Harness {
    std::fs::create_dir_all(workspace).expect("a workspace");
    let mut cfg = Config::for_this_box(workspace);
    cfg.session_id = id.to_string();
    cfg.vocab_gguf = None;
    cfg.model = "local".to_string();
    cfg.room_link = true;
    let parts = Parts::load(&cfg).expect("the byte vocabulary needs no model file");
    let h = Harness::open(&parts, cfg, Hub::new(id)).expect("the session opens");
    assert!(h.has_room(), "`{id}` must have the room");
    h
}

fn room(h: &Harness, arguments: serde_json::Value) -> ToolResult {
    let call = ToolCall {
        id: "call_room".into(),
        name: "room".into(),
        arguments: arguments.to_string(),
    };
    h.runtime_handle().invoke("t1", &call, &mut NullToolSink)
}

#[test]
fn a_rowless_ask_survives_the_pass_and_its_answer_wakes_the_asker_saying_so() {
    let dir = std::env::temp_dir().join(format!("letibot-room-rowless-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a directory of this test's own");
    let socket = dir.join("room.sock");
    // SAFETY: this binary holds one test, so nothing else in the process reads this variable
    // while it is set. It keeps the room inside `dir`.
    unsafe {
        std::env::set_var(daemon_room::SOCKET_ENV, &socket);
    }
    let _server = Server::serve(&socket, Room::new()).expect("a coordinator");
    let workspace = dir.join("ws");
    let mut a = open(&workspace, "s-rowless-a");
    let mut b = open(&workspace, "s-rowless-b");

    let out = room(
        &a,
        serde_json::json!({"verb": "say", "kind": "ask", "to": "s-rowless-b",
                           "text": "which port?"}),
    );
    assert_eq!(out.outcome, ToolOutcome::Ok, "{out:?}");
    let asks = a
        .room_link()
        .expect("A's link")
        .asks()
        .expect("an asks read");
    let ask = asks.0.first().expect("the ask is held").seq;

    // The pass runs between the ask and the answer, as it does every five seconds.
    a.fill_room_board();
    b.fill_room_board();
    let (by_me, _) = a
        .room_link()
        .expect("A's link")
        .asks()
        .expect("an asks read");
    assert!(
        by_me.iter().any(|x| x.seq == ask && x.open()),
        "an ask no row carries was taken back by its own asker's pass: {by_me:?}"
    );

    let out = room(
        &b,
        serde_json::json!({"verb": "answer", "ask": ask, "text": "5433"}),
    );
    assert_eq!(out.outcome, ToolOutcome::Ok, "{out:?}");

    let want = format!("answered your ask #{ask}: 5433");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let mut said: Vec<String> = Vec::new();
    while std::time::Instant::now() < deadline && !said.iter().any(|l| l.contains(&want)) {
        said.extend(daemon_room::wake::take("s-rowless-a"));
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let line = said
        .iter()
        .find(|l| l.contains(&want))
        .unwrap_or_else(|| panic!("the asker was never told `{want}`: {said:?}"));
    assert!(
        line.contains("to you"),
        "it is addressed to the asker: {line}"
    );
    assert!(
        !line.contains("passing by"),
        "it is not a note that passes: {line}"
    );

    // And the asker's own list says how it ended.
    let out = room(&a, serde_json::json!({"verb": "asks"}));
    assert!(out.render().contains("answered: 5433"), "{}", out.render());
    drop(a);
    drop(b);
    let _ = std::fs::remove_dir_all(&dir);
}
