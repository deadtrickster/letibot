//! The bridge and its `room-wait` waiter are one seat on a real coordinator - two connections of one
//! name and one id, so a reply is said from the seat that was addressed - while a second SEAT in the
//! same folder (another Claude Code session) still gets a suffixed one.

use std::sync::Arc;
use std::time::{Duration, Instant};

use letibot_bridge::rooms::{self, Attention, RoomLink};
use letibot_room::client::Client;
use letibot_room::server::Server;
use letibot_room::{Event, Request, Room, Seat, Speaker, room_name};

#[test]
fn the_waiter_joins_its_bridge_and_a_second_seat_is_suffixed() {
    // SAFETY: first thing in the only test of this file, before the handle is read.
    unsafe { std::env::set_var(rooms::NAME_ENV, "cc@letibot") };
    let dir = std::env::temp_dir().join(format!("letibot-room-share-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let server = Server::serve(&dir.join("room.sock"), Room::new()).expect("a coordinator");

    let bridge = RoomLink::at(server.path());
    bridge.ensure().expect("the bridge connects");
    assert_eq!(rooms::handle(), "cc@letibot");

    let waiter = RoomLink::at(server.path());
    let speaker = Client::connect_as(
        server.path(),
        Speaker::top_level("s-a"),
        room_name(&server.name(), ""),
        Seat::Session,
        Arc::new(|_: &Event| {}),
    )
    .expect("a session");
    let (code, said) = std::thread::scope(|scope| {
        let h = scope.spawn(|| {
            rooms::wait_marked(
                &waiter,
                Attention::ToMe,
                Instant::now() + Duration::from_secs(20),
                Some(&dir.join("wait-mark")),
            )
        });
        let until = Instant::now() + Duration::from_secs(10);
        while !waiter.connected() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(waiter.connected(), "the waiter must be admitted");
        speaker
            .request(&Request::Say {
                id: "1".into(),
                room: "host".into(),
                text: "disk is free".into(),
                to: Some("cc@letibot".into()),
                thread: None,
                refs: Vec::new(),
                attachments: Vec::new(),
                kind: Default::default(),
            })
            .expect("said");
        h.join().unwrap()
    });
    assert_eq!(code, 0, "{said}");
    assert!(said.contains("disk is free"), "{said}");

    // A second SEAT of the same folder - the other Claude Code session - is a different id, and the
    // name one seat speaks as is not one it may take: it asks for a suffix instead.
    let other = RoomLink::at(server.path());
    other.as_seat("lab2x1/cc-ffff0000");
    other.ensure().expect("the second seat connects");
    assert_ne!(rooms::handle(), "cc@letibot");
    assert!(rooms::handle().starts_with("cc@letibot-"));
    assert_eq!(rooms::family().as_deref(), Some("cc@letibot"));

    // Both bridges of the name get its mail, each under its own cursor: the first one's ack (and
    // its waiter's mark) hides nothing from the second.
    let (tx, rx) = std::sync::mpsc::channel::<u64>();
    let tx = std::sync::Mutex::new(tx);
    other.stream(Arc::new(move |m, _| {
        let _ = tx.lock().unwrap().send(m["seq"].as_u64().unwrap_or(0));
        true
    }));
    let said = speaker
        .request(&Request::Say {
            id: "2".into(),
            room: "host".into(),
            text: "is H6 gated?".into(),
            to: Some("cc@letibot".into()),
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
    // The waiter's message may come first: nothing acked it, so the family's inbox still holds it.
    let until = Instant::now() + Duration::from_secs(10);
    while rx.recv_timeout(Duration::from_millis(200)).ok() != Some(seq) {
        assert!(
            Instant::now() < until,
            "the suffixed bridge is pushed mail to its family"
        );
    }
    bridge.ack(seq).expect("the first bridge acks");
    let rows = other.inbox(Attention::ToMe, 50).expect("inbox");
    assert!(
        rows["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["seq"] == seq),
        "{rows}"
    );
    assert!(other.unseen().contains(&seq), "pushed, not yet seen");
    server.stop();
    let _ = std::fs::remove_dir_all(&dir);
}
