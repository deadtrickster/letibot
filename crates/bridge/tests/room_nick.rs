//! `room_nick` on a real coordinator: the label the room draws this bridge by, taken from the room
//! because the room is the one place that sees every live seat. The handle it speaks as does not
//! move - the seat's id is its identity - and a label a live seat already answers to is refused with
//! whose it is and which counter is free.
//! One test, one process: the seat and the handle are process-wide.

use std::sync::Arc;

use letibot_bridge::rooms::{self, RoomLink};
use letibot_room::client::Client;
use letibot_room::server::Server;
use letibot_room::{Event, Request, Response, Room, Seat, Speaker, room_name};
use serde_json::json;

fn roster(server: &Server, asker: &Client) -> Vec<String> {
    let _ = server;
    match asker
        .request(&Request::Roster {
            room: "host".into(),
        })
        .expect("a roster")
    {
        Response::Roster { rows, .. } => rows
            .into_iter()
            .filter(|r| r.kind == Seat::External)
            .map(|r| r.name)
            .collect(),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_nick_is_the_label_the_room_draws_and_the_seat_keeps_its_id() {
    let dir = std::env::temp_dir().join(format!("letibot-room-nick-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let work = dir.join("stroppy-io");
    std::fs::create_dir_all(&work).unwrap();
    // SAFETY: first thing in the only test of this file, before the handle is read.
    unsafe {
        std::env::remove_var(rooms::NAME_ENV);
        // A Claude Code session id in the environment would be another seat; this test is the one
        // a bridge started without it is.
        std::env::remove_var(rooms::SESSION_ENV);
        std::env::set_var("XDG_DATA_HOME", dir.join("data"));
    }
    std::env::set_current_dir(&work).unwrap();
    let server = Server::serve(&dir.join("room.sock"), Room::new()).expect("a coordinator");
    server.with_seat_host("lab2x1");
    let asker = Client::connect_as(
        server.path(),
        Speaker::top_level("s-a"),
        room_name(&server.name(), ""),
        Seat::Session,
        Arc::new(|_: &Event| {}),
    )
    .expect("a session");

    let link = RoomLink::at(server.path());
    link.ensure().expect("the bridge connects");
    assert_eq!(rooms::handle(), "cc@stroppy-io");
    assert_eq!(roster(&server, &asker), vec!["cc@stroppy-io".to_string()]);
    // The seat is the box's half and the folder's, and it is what the room files this bridge under.
    // The host half is the box that minted it - this one, pinned in the data dir - not the name a
    // test pinned on the coordinator: an id is minted where the seat is.
    let seat = rooms::seat();
    let (host, local) = seat.split_once('/').unwrap_or_else(|| panic!("{seat}"));
    assert!(!host.is_empty(), "{seat}");
    assert!(local.starts_with("cc-"), "{seat}");
    assert!(letibot_room::SeatId::parse(&seat).is_some(), "{seat}");

    // A label a live seat already answers to is refused with whose it is and the next counter, and
    // the bridge keeps the one it had - it is not disconnected and it is not renamed.
    let holder = Client::connect_as(
        server.path(),
        Speaker::top_level("s-b"),
        room_name(&server.name(), ""),
        Seat::Session,
        Arc::new(|_: &Event| {}),
    )
    .expect("a holder");
    holder
        .request(&Request::Nick {
            id: String::new(),
            nick: "taken".into(),
        })
        .expect("a free label");
    let refused = rooms::call(&link, "room_nick", &json!({ "nick": "taken" })).unwrap_err();
    assert!(refused.contains("taken-2"), "the next counter: {refused}");
    assert!(refused.contains("s-b"), "whose it is: {refused}");
    assert_eq!(rooms::handle(), "cc@stroppy-io", "the handle is kept");
    assert!(
        link.request(&json!({"type": "rooms"})).is_ok(),
        "and still connected"
    );
    // A label that is not one is refused before it reaches the room.
    let bad = rooms::call(&link, "room_nick", &json!({ "nick": "Bad Nick" })).unwrap_err();
    assert!(bad.contains("not a nick"), "{bad}");
    drop(holder);

    let done = rooms::call(&link, "room_nick", &json!({ "nick": "conformity" })).unwrap();
    assert_eq!(done["nick"], "conformity");
    assert_eq!(done["seat"], seat, "the seat is unchanged by a rename");
    assert_eq!(rooms::handle(), "cc@stroppy-io", "and so is the handle");
    assert_eq!(roster(&server, &asker), vec!["conformity".to_string()]);

    // A restart of the bridge is the same seat under the same label: both are kept in the folder.
    drop(link);
    let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !roster(&server, &asker).is_empty() && std::time::Instant::now() < until {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let again = RoomLink::at(server.path());
    again.ensure().expect("a new connection");
    assert_eq!(
        rooms::handle(),
        "cc@stroppy-io",
        "a label is not the handle"
    );
    assert_eq!(rooms::seat(), seat, "the same seat, from the folder's file");
    assert_eq!(roster(&server, &asker), vec!["conformity".to_string()]);
    let _ = std::fs::remove_dir_all(&dir);
}
