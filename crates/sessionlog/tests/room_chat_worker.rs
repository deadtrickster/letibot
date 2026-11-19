//! `RoomChat` is answered off the reader loop, on the connection's own worker: a coordinator
//! that takes seconds to answer must not stop the head's Interrupts being read, and the head's
//! chat state is retired when its connection ends.

use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use letibot_sessionlog::hub::CommandKind;
use letibot_sessionlog::protocol::{Caps, ClientFrame, PROTOCOL_VERSION, ServerFrame};
use letibot_sessionlog::registry::{Registry, RoomChatSeat, SessionWiring};
use letibot_sessionlog::server::serve_conn;
use letibot_sessionlog::testing::{PATIENCE, wait_until, wait_within};
use letibot_sessionlog::wire::{FrameReader, FrameWriter};
use serde_json::{Value, json};

fn start() -> Arc<Registry> {
    let registry = Registry::new();
    registry
        .create(
            "s-1",
            "one",
            SessionWiring {
                model: "qwen3-4b".into(),
                dialect: "qwen".into(),
                endpoint: "127.0.0.1:8080".into(),
                workspace: "/tmp/ws".into(),
            },
        )
        .expect("create");
    registry
}

fn attach(registry: &Arc<Registry>) -> (FrameWriter<UnixStream>, FrameReader<UnixStream>) {
    let (a, b) = UnixStream::pair().expect("pair");
    let reg = registry.clone();
    std::thread::spawn(move || {
        let _ = serve_conn(reg, a);
    });
    let mut w = FrameWriter::new(b.try_clone().expect("clone"));
    let mut r = FrameReader::new(b);
    w.write(&ClientFrame::Attach {
        protocol_version: PROTOCOL_VERSION,
        session_id: "s-1".into(),
        since_seq: 0,
        kind: "tui".into(),
        identity: "test".into(),
        caps: Caps::default(),
    })
    .expect("attach");
    assert!(matches!(
        r.read::<ServerFrame>().expect("hello"),
        ServerFrame::Hello { .. }
    ));
    (w, r)
}

/// A chat answer that takes longer than the test is willing to wait for the interrupt: the
/// interrupt is read and queued while the answer is still being computed, and the reply for the
/// ask arrives afterwards. Before the worker, the reader loop itself sat in the ask.
#[test]
fn a_slow_chat_answer_does_not_stop_the_reader_loop() {
    let registry = start();
    let ask_started = Arc::new(AtomicBool::new(false));
    let asked = Arc::new(Mutex::new(Vec::<String>::new()));
    let gone = Arc::new(Mutex::new(Vec::<String>::new()));

    let seat = {
        let started = Arc::clone(&ask_started);
        let heads = Arc::clone(&asked);
        // The answer is not finished until the test has seen the interrupt: what this test is
        // about is which of the two came first, not how long a round trip took.
        let (may_answer, answer_now) = std::sync::mpsc::channel::<()>();
        let answer_now = Arc::new(Mutex::new(answer_now));
        let ask = Arc::new(move |head: &str, _op: Value| {
            heads.lock().unwrap().push(head.to_string());
            started.store(true, Ordering::SeqCst);
            let _ = answer_now.lock().unwrap().recv();
            json!({"type": "rooms", "rows": []})
        });
        let left = Arc::clone(&gone);
        let gone = Arc::new(move |head: &str| left.lock().unwrap().push(head.to_string()));
        (RoomChatSeat::new(ask, gone), may_answer)
    };
    let (seat, may_answer) = seat;
    registry.set_room_chat(seat);
    let hub = registry.get("s-1").expect("the session");
    let (mut w, mut r) = attach(&registry);

    w.write(&ClientFrame::RoomChat {
        client_request_id: "c1".into(),
        op: json!({"type": "rooms"}),
    })
    .expect("ask");
    wait_until("the ask to start", || ask_started.load(Ordering::SeqCst));

    // An Interrupt sent while the chat answer is still being computed: it must reach the hub's
    // command queue well inside the ask's own window, because the reader loop kept reading.
    w.write(&ClientFrame::Interrupt {
        client_request_id: "c2".into(),
        expected_seq: hub.head_seq(),
        reason: "the operator changed their mind".into(),
    })
    .expect("interrupt");
    let interrupted = wait_within(PATIENCE, || {
        hub.try_command()
            .is_some_and(|c| matches!(c.kind, CommandKind::Interrupt { .. }))
    });
    assert!(
        interrupted,
        "the interrupt was not read while the chat answer was still being computed"
    );
    // The answer is only now allowed to finish, so the reply below is the one sent after it.
    let _ = may_answer.send(());

    // The chat reply still arrives, under its own request id.
    let mut replied = false;
    let deadline = Instant::now() + PATIENCE;
    while Instant::now() < deadline {
        if matches!(
            r.read::<ServerFrame>().expect("a frame"),
            ServerFrame::RoomChat { ref client_request_id, ref reply }
                if client_request_id == "c1" && reply["type"] == "rooms"
        ) {
            replied = true;
            break;
        }
    }
    assert!(replied, "the slow chat reply never arrived");
    let heads = asked.lock().unwrap().clone();
    assert_eq!(heads.len(), 1, "the ask was answered for one head");
    assert!(!heads[0].is_empty(), "the head was not named to the seat");

    // The connection ends and the head's chat state is retired: the worker outlives the reader
    // loop by exactly its last reply, and `gone` is its exit path.
    drop(w);
    drop(r);
    wait_until("the head to be retired", || {
        !gone.lock().unwrap().is_empty()
    });
    assert_eq!(gone.lock().unwrap()[0], heads[0]);
}
