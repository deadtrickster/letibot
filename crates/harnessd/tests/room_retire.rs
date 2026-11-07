//! Deleting a session tells the room: `harnessd --delete` is the one ending that settles an ask
//! `Down`, because the board it waited on is gone for good. Every other way a session stops - a
//! daemon exit, a crash, a `--stop` - is a restart the operator owes, and settles nothing.

use std::sync::Arc;

use letibot_harnessd::room as daemon_room;
use letibot_room::client::Client;
use letibot_room::server::Server;
use letibot_room::{AskState, Event, Request, Response, Room, Speaker};

fn scratch(what: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("letibot-room-{what}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a directory of this test's own");
    dir
}

fn seat(socket: &std::path::Path, name: &str) -> Client {
    let wake: Arc<dyn Fn(&Event) + Send + Sync> = Arc::new(|_: &Event| {});
    Client::connect(socket, Speaker::top_level(name), wake).expect("a connection")
}

#[test]
fn a_deleted_session_is_retired_in_the_room_and_its_asks_end_down() {
    let dir = scratch("retire");
    let socket = dir.join("room.sock");
    // SAFETY: this binary holds one test, so nothing else in the process reads this variable while
    // it is set. It is the door `daemon_room::socket_path` reads.
    unsafe { std::env::set_var(daemon_room::SOCKET_ENV, &socket) };
    let server = Server::serve(&socket, Room::new()).expect("a coordinator");
    let a = seat(&socket, "s-a");
    let b = seat(&socket, "s-b");

    // A asks B, and B's own connection is what makes it a live ask.
    let asked = match a
        .request(&Request::Ask {
            id: "q1".into(),
            room: String::new(),
            to: "s-b".into(),
            question: "which port?".into(),
        })
        .expect("an answer")
    {
        Response::Asked { ask, .. } => ask,
        other => panic!("{other:?}"),
    };
    drop(b);
    // The connection closing is not a shutdown: the ask is still open.
    let open = |a: &Client| match a.request(&Request::Asks).expect("an answer") {
        Response::Asks { by_me, .. } => by_me,
        other => panic!("{other:?}"),
    };
    assert!(open(&a).iter().any(|x| x.seq == asked.seq && x.open()));

    // The delete path's own act: the session is gone for good, so its asks end.
    let said = daemon_room::link::retire("s-b").expect("the room is told");
    assert!(said.contains("ended") || said.contains("ask"), "{said}");
    let by_me = open(&a);
    let one = by_me.iter().find(|x| x.seq == asked.seq).expect("the ask");
    assert!(
        matches!(one.state, AskState::Down { .. }),
        "a deleted session is the one DOWN: {one:?}"
    );
    assert_eq!(one.asker, "s-a");

    // A box with no coordinator is not an error to shout about: there is nothing to settle.
    server.stop();
    let _ = std::fs::remove_file(&socket);
    let why = daemon_room::link::retire("s-b").expect_err("no room");
    assert!(why.contains("no coordinator"), "{why}");
    let _ = std::fs::remove_dir_all(&dir);
}
