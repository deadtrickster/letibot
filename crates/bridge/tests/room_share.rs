//! The bridge and its `room-wait` waiter share one name on a real coordinator, while a second
//! independent bridge in the same folder still gets a suffixed one.

use std::sync::Arc;
use std::time::{Duration, Instant};

use letibot_bridge::rooms::{self, Attention, RoomLink};
use letibot_room::client::Client;
use letibot_room::server::Server;
use letibot_room::{Event, Request, Room, Seat, Speaker, room_name};

#[test]
fn the_waiter_joins_its_bridge_and_a_second_bridge_is_suffixed() {
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
            rooms::wait(
                &waiter,
                Attention::ToMe,
                Instant::now() + Duration::from_secs(20),
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

    // Two independent bridges still may not share: the second takes a suffixed name.
    let other = RoomLink::at(server.path());
    other.ensure().expect("the second bridge connects");
    assert_ne!(rooms::handle(), "cc@letibot");
    assert!(rooms::handle().starts_with("cc@letibot-"));
    server.stop();
    let _ = std::fs::remove_dir_all(&dir);
}
