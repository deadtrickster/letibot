//! A session that opens before the coordinator links to it once the coordinator is up: the room
//! pass retries, and the `room` tool starts to work without reopening the session.

use letibot_harnessd::config::Config;
use letibot_harnessd::harness::Parts;
use letibot_harnessd::{Harness, room as daemon_room};
use letibot_room::{Room, Server};
use letibot_sessionlog::hub::Hub;
use letibot_tools::NullToolSink;
use letibot_transcript::{ToolCall, ToolOutcome};

fn room(h: &Harness, args: serde_json::Value) -> (ToolOutcome, String) {
    let call = ToolCall {
        id: format!("call_{}", serde_json::to_string(&args).expect("args")),
        name: "room".into(),
        arguments: args.to_string(),
    };
    let mut sink = NullToolSink;
    let out = h.runtime_handle().invoke("t1", &call, &mut sink);
    (out.outcome.clone(), out.render())
}

#[test]
fn a_session_links_when_the_coordinator_starts_after_it() {
    let dir = std::env::temp_dir().join(format!("letibot-room-late-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a directory of this test's own");
    let socket = dir.join("room.sock");
    // SAFETY: this binary holds one test, so nothing else reads the variable while it is set.
    unsafe { std::env::set_var(daemon_room::SOCKET_ENV, &socket) };

    let workspace = dir.join("workspace");
    std::fs::create_dir_all(&workspace).expect("a workspace");
    let mut cfg = Config::for_this_box(&workspace);
    cfg.vocab_gguf = None;
    cfg.model = "local".to_string();
    cfg.room_link = true;
    let me = cfg.session_id.clone();
    let parts = Parts::load(&cfg).expect("the byte vocabulary needs no model file");
    let mut h = Harness::open(&parts, cfg, Hub::new(&me)).expect("the session opens");
    assert!(!h.has_room() && h.wants_room(), "no coordinator yet");
    let (outcome, text) = room(&h, serde_json::json!({"verb": "queue", "tag": "ram"}));
    assert_ne!(outcome, ToolOutcome::Ok, "{text}");
    h.fill_room_board();
    assert!(!h.has_room(), "a pass with no socket links nothing");

    let server = Server::serve(&socket, Room::new()).expect("a room on this test's socket");
    h.fill_room_board();
    assert!(h.has_room() && !h.wants_room(), "the pass linked it");
    let (outcome, text) = room(
        &h,
        serde_json::json!({"verb": "post", "tag": "ram", "body": "8 GB"}),
    );
    assert_eq!(outcome, ToolOutcome::Ok, "{text}");
    server.stop();
    let _ = std::fs::remove_dir_all(&dir);
}
