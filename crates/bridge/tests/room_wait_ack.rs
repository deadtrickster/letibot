//! A waiter prints a message once, and an ack by the bridge stops it - against a real room
//! `Server`, which the bridge's own source may not name.

use letibot_bridge::rooms::{Attention, NAME_ENV, RoomLink, handle, wait_marked};

/// A waiter prints a message ONCE, and an ack by the bridge stops it: the mark is the seat's,
/// so the next run of the same seat finds it, and the cursor is the seat's, so a message its
/// session has already taken is not the waiter's to print again.
#[test]
fn a_wait_prints_a_message_once_and_an_ack_stops_it() {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use letibot_room::client::Client;
    use letibot_room::server::Server;
    use letibot_room::{Event, Request, Room, Seat, Speaker, room_name};

    let _held = letibot_testenv::EnvGuard::lock();
    let dir = std::env::temp_dir().join(format!("letibot-wait-once-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // SAFETY: under the binary's one env lock, held above.
    unsafe {
        std::env::set_var(NAME_ENV, "cc@wait-once");
        std::env::set_var("XDG_DATA_HOME", &dir);
    }
    let server = Server::serve(&dir.join("room.sock"), Room::new()).expect("a coordinator");
    let bridge = RoomLink::at(server.path());
    bridge.ensure().expect("the bridge connects");
    let speaker = Client::connect_as(
        server.path(),
        Speaker::top_level("s-a"),
        room_name(&server.name(), ""),
        Seat::Session,
        Arc::new(|_: &Event| {}),
    )
    .expect("a session");
    let said = speaker
        .request(&Request::Say {
            id: "1".into(),
            room: "host".into(),
            text: "disk is free".into(),
            to: Some(handle()),
            thread: None,
            refs: Vec::new(),
            attachments: Vec::new(),
            kind: Default::default(),
        })
        .expect("said");
    let seq = match said {
        letibot_room::Response::Said { message, .. } => message.seq,
        other => panic!("{other:?}"),
    };

    let mark = dir.join("wait-mark");
    let waiter = RoomLink::at(server.path());
    let (code, printed) = wait_marked(
        &waiter,
        Attention::ToMe,
        Instant::now() + Duration::from_secs(20),
        Some(&mark),
    );
    assert_eq!(code, 0, "{printed}");
    assert!(printed.contains("disk is free"), "{printed}");

    // The next run of the same seat, with the same mark, has nothing to print.
    let again = RoomLink::at(server.path());
    let (code, printed) = wait_marked(
        &again,
        Attention::ToMe,
        Instant::now() + Duration::from_secs(1),
        Some(&mark),
    );
    assert_eq!(code, 30, "the same run printed it again: {printed}");

    // And a waiter that has never printed it does not print what the bridge acked: the cursor
    // is the seat's, and the msg is its session's to read.
    bridge.ack(seq).expect("the bridge acks");
    let fresh = RoomLink::at(server.path());
    let (code, printed) = wait_marked(
        &fresh,
        Attention::ToMe,
        Instant::now() + Duration::from_secs(1),
        Some(&dir.join("fresh-mark")),
    );
    assert_eq!(code, 30, "an acked message was printed: {printed}");

    server.stop();
    let _ = std::fs::remove_dir_all(&dir);
}
