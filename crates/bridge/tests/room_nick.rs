//! `room_nick` on a real coordinator: the handle changes at once and shows in the roster, the nick
//! outlives the connection, and a handle a live member holds is refused with the old one kept.
//! One test, one process: the handle is process-wide.

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
fn a_nick_changes_the_handle_survives_a_reconnect_and_is_refused_when_taken() {
    let dir = std::env::temp_dir().join(format!("letibot-room-nick-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let work = dir.join("stroppy-io");
    std::fs::create_dir_all(&work).unwrap();
    // SAFETY: first thing in the only test of this file, before the handle is read.
    unsafe {
        std::env::remove_var(rooms::NAME_ENV);
        std::env::set_var("XDG_DATA_HOME", dir.join("data"));
    }
    std::env::set_current_dir(&work).unwrap();
    let server = Server::serve(&dir.join("room.sock"), Room::new()).expect("a coordinator");
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

    let holder = Client::connect_as(
        server.path(),
        Speaker::top_level("cc@stroppy-io:taken"),
        room_name(&server.name(), ""),
        Seat::External,
        Arc::new(|_: &Event| {}),
    )
    .expect("a holder");
    let refused = rooms::call(&link, "room_nick", &json!({"nick": "taken"})).unwrap_err();
    assert!(refused.contains("cc@stroppy-io:taken"), "{refused}");
    assert_eq!(rooms::handle(), "cc@stroppy-io", "the old handle is kept");
    assert!(
        link.request(&json!({"type": "rooms"})).is_ok(),
        "and still connected"
    );
    assert!(rooms::call(&link, "room_nick", &json!({"nick": "Bad Nick"})).is_err());
    drop(holder);

    let done = rooms::call(&link, "room_nick", &json!({"nick": "conformity"})).unwrap();
    assert_eq!(done["handle"], "cc@stroppy-io:conformity");
    assert_eq!(rooms::handle(), "cc@stroppy-io:conformity");
    // Unique, so the room shows the short form, once the dropped holder's close is seen.
    let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut seen = roster(&server, &asker);
    while seen != vec!["cc@conformity".to_string()] && std::time::Instant::now() < until {
        std::thread::sleep(std::time::Duration::from_millis(50));
        seen = roster(&server, &asker);
    }
    assert_eq!(seen, vec!["cc@conformity".to_string()]);

    drop(link);
    let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !roster(&server, &asker).is_empty() && std::time::Instant::now() < until {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let again = RoomLink::at(server.path());
    again.ensure().expect("a new connection");
    assert_eq!(rooms::handle(), "cc@stroppy-io:conformity");
    assert_eq!(roster(&server, &asker), vec!["cc@conformity".to_string()]);
    let _ = std::fs::remove_dir_all(&dir);
}
