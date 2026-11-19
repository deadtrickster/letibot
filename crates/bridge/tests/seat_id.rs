//! A bridge's seat: the id Claude Code hands its MCP servers, minted once and kept in the working
//! folder. Two bridges of one Claude Code session are one seat; a fork (a new session id) is
//! another; and a bridge started without the variable is still the folder's seat.
//! One test, one process: the seat is process-wide.

use std::sync::Arc;

use letibot_bridge::rooms::{self, RoomLink};
use letibot_room::client::Client;
use letibot_room::server::Server;
use letibot_room::{Event, Request, Response, Room, Seat, Speaker, room_name};

/// The Claude Code session id of this test, in the shape Claude Code passes (a UUID).
const CLAUDE: &str = "2E4A9C1B-9F3D-4A21-8B77-5C1D0E9A1234";

#[test]
fn the_seat_is_the_claude_code_session_id_and_the_room_files_the_bridge_under_it() {
    let dir = std::env::temp_dir().join(format!("letibot-bridge-seat-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let work = dir.join("stroppy-io");
    std::fs::create_dir_all(&work).unwrap();
    // SAFETY: first thing in the only test of this file, before the seat is read.
    unsafe {
        std::env::remove_var(rooms::NAME_ENV);
        std::env::set_var(rooms::SESSION_ENV, CLAUDE);
        std::env::set_var("XDG_DATA_HOME", dir.join("data"));
    }
    std::env::set_current_dir(&work).unwrap();

    let server = Server::serve(&dir.join("room.sock"), Room::new()).expect("a coordinator");
    server.with_seat_host("lab2x1");
    let link = RoomLink::at(server.path());
    link.ensure().expect("the bridge connects");

    // `<host>/cc-<session id>`, lowercased: the same seat the room's own `SeatId` minting writes.
    // The host half is the box that minted it, which is the bridge's own and not the one a test
    // pinned on the coordinator - an id is minted where the seat is.
    let seat = rooms::seat();
    assert!(
        seat.ends_with(&format!("/cc-{}", CLAUDE.to_lowercase())),
        "{seat}"
    );
    let parsed = letibot_room::SeatId::parse(&seat).expect("a seat id");
    assert!(parsed.is_bridge(), "{seat}");

    // The folder keeps it, which is what a bridge started without the variable reuses.
    let data = dir.join("data").join("letibot");
    assert_eq!(
        letibot_room::id::read_folder(&data, &work).map(|i| i.to_string()),
        Some(seat.clone()),
        "kept under the data dir, keyed by the folder"
    );

    // The room draws the seat by its id, whatever the handle it speaks as.
    let asker = Client::connect_as(
        server.path(),
        Speaker::top_level("s-a"),
        room_name(&server.name(), ""),
        Seat::Session,
        Arc::new(|_: &Event| {}),
    )
    .expect("a session");
    let rows = match asker
        .request(&Request::Roster {
            room: "host".into(),
        })
        .expect("a roster")
    {
        Response::Roster { rows, .. } => rows,
        other => panic!("{other:?}"),
    };
    let mine = rows
        .iter()
        .find(|r| r.kind == Seat::External)
        .unwrap_or_else(|| panic!("the bridge is in the roster: {rows:?}"));
    assert_eq!(mine.seat_id, seat, "the roster carries the seat id");
    assert_eq!(mine.name, "cc@stroppy-io", "and the handle it is drawn by");

    drop(link);
    server.stop();
    let _ = std::fs::remove_dir_all(&dir);
}
