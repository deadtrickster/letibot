//! One Claude Code session is one seat. The plugin's waiter and the MCP bridge of one session are
//! two connections of the same seat id (`Hello.share`), so a reply is said from the seat that was
//! addressed; a seat that reconnects on its own name takes it back rather than being refused by the
//! connection the room still holds; and a seat whose facts a bridge reports is in the roster by its
//! repository, its branch and its row.

use std::path::PathBuf;
use std::sync::Arc;

use letibot_room::client::Client;
use letibot_room::server::Server;
use letibot_room::{Event, Request, Response, Room, Seat, SeatFacts, Speaker};

mod support;

use support::wait_until;

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Scratch {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "letibot-room-shared-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&p).expect("a scratch directory");
        Scratch(p)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn coordinator(dir: &Scratch) -> Server {
    let server = Server::serve(&dir.0.join("room.sock"), Room::new()).expect("a coordinator");
    server.with_seat_host("lab2x1");
    server
}

fn wake() -> Arc<dyn Fn(&Event) + Send + Sync> {
    Arc::new(|_: &Event| {})
}

/// A connection of `name` as the seat `id`, sharing it with whatever already holds it, the way a
/// bridge and its waiter do.
fn shared(server: &Server, name: &str, id: &str) -> Result<Client, String> {
    Client::connect_shared(
        server.path(),
        Speaker::top_level(name),
        letibot_room::room_name(&server.name(), "aaaaaaaaaaaa"),
        Seat::External,
        id,
        wake(),
    )
    .map_err(|e| e.0)
}

/// A connection of `name` as the seat `id` that does NOT say it shares: a seat reconnecting.
fn seated(server: &Server, name: &str, id: &str) -> Result<Client, String> {
    Client::connect_seated(
        server.path(),
        Speaker::top_level(name),
        letibot_room::room_name(&server.name(), "aaaaaaaaaaaa"),
        Seat::External,
        id,
        "",
        wake(),
    )
    .map_err(|e| e.0)
}

#[test]
fn a_waiters_connection_and_its_bridges_are_one_seat() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let id = "lab2x1/cc-0e5a1c77";
    // The plugin's monitor dials first, saying it shares the seat.
    let waiter = shared(&server, "cc@letibot", id).expect("the waiter connects");
    // The MCP server of the same session arrives saying the same: it is one more connection of one
    // seat, not a second seat that needs a suffixed name.
    let bridge = shared(&server, "cc@letibot", id)
        .unwrap_or_else(|e| panic!("the bridge of the same seat was refused: {e}"));
    assert_eq!(server.connections(), 2, "two connections, and one seat");
    // One roster row for the seat, drawn by its id - a reply said here is said by the seat that was
    // addressed, whichever of the two connections sends it.
    let rows = server.roster_rows(&letibot_room::chat::project_room("aaaaaaaaaaaa"));
    let mine: Vec<_> = rows.iter().filter(|r| r.seat_id == id).collect();
    assert_eq!(mine.len(), 1, "one row for one seat: {rows:?}");
    assert_eq!(
        server.seat_rows().iter().filter(|r| r.id == id).count(),
        1,
        "the mesh sees one seat too"
    );
    // A DIFFERENT Claude Code session in the same folder is a different seat: it is refused the
    // name (so the bridge takes a suffix, as it always did) rather than being folded into this one.
    let other = shared(&server, "cc@letibot", "lab2x1/cc-ffff0000")
        .err()
        .unwrap_or_else(|| panic!("a second seat took one name"));
    assert!(other.contains("cc@letibot"), "{other}");
    drop(waiter);
    drop(bridge);
    server.stop();
}

#[test]
fn a_seat_that_reconnects_takes_its_own_name_back_from_the_room() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let id = "lab2x1/cc-1b2c3d4e";
    // The connection the room still holds is the seat's own, and its socket is gone: a redial does
    // not ask to share, because it is the seat and not one more connection of it.
    let stale = seated(&server, "cc@letibot", id).expect("the first connection");
    let again = seated(&server, "cc@letibot", id)
        .unwrap_or_else(|e| panic!("a seat could not reconnect on its own name: {e}"));
    // The stale one is ended rather than left to hold the name, and the new one answers.
    wait_until("the stale connection is let go", || {
        server.connections() == 1
    });
    assert!(matches!(
        again.request(&Request::Rooms).expect("an answer"),
        Response::Rooms { .. }
    ));
    drop(stale);
    drop(again);
    server.stop();
}

#[test]
fn a_seat_that_never_said_which_one_it_is_keeps_the_name_it_took() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    // A client that named no id predates ids: the name is all there is to tell it by, so a second
    // connection under that name is the duplicate it always was.
    let first = shared(&server, "cc@old", "").expect("a connection with no id");
    let second = seated(&server, "cc@old", "").err();
    assert!(
        second
            .as_ref()
            .is_some_and(|e| e.contains("already speaks as")),
        "an id-less duplicate must still be refused: {second:?}"
    );
    drop(first);
    server.stop();
}

#[test]
fn a_bridges_facts_are_in_the_roster_by_repository_branch_and_row() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let id = "lab2x1/cc-77aa13b0";
    let client = shared(&server, "cc@letibot", id).expect("the bridge connects");
    // What a Claude Code bridge sends: its folder (which is the project's root here), and the
    // facts a daemon would otherwise report - the repository, the branch and the row in progress.
    let answer = client
        .request(&Request::Seat {
            facts: SeatFacts {
                working: letibot_room::seat::WorkingOn {
                    row: "the LAN run's seven fixes".into(),
                    repo: "letibot".into(),
                    branch: "remote/10860790".into(),
                    ..letibot_room::seat::WorkingOn::default()
                },
                ..SeatFacts::default()
            },
            root: "/home/dead/Projects/letibot/letibot".into(),
        })
        .expect("an answer");
    assert!(matches!(answer, Response::Seated { .. }), "{answer:?}");
    let rows = server.seat_rows();
    let row = rows
        .iter()
        .find(|r| r.id == id)
        .unwrap_or_else(|| panic!("the bridge is not a seat of this box: {rows:?}"));
    assert_eq!(row.branch, "remote/10860790");
    assert_eq!(row.task, "the LAN run's seven fixes");
    assert_eq!(row.project, "aaaaaaaaaaaa");
    // And the root it named is the project's, which is what another box turns into a repository
    // identity: a bridge-only project has no daemon to send `Members`.
    assert_eq!(
        server.project_root("aaaaaaaaaaaa"),
        "/home/dead/Projects/letibot/letibot"
    );
    drop(client);
    server.stop();
}
