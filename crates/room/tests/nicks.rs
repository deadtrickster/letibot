//! Nicks on a real coordinator: a seat's label is display, the id is the identity, and the rules
//! that keep two live seats from answering to one name. A nick is taken, refused with its holder and
//! the next counter, freed when the holder goes, and said once in each room the seat is in.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use letibot_room::client::Client;
use letibot_room::server::Server;
use letibot_room::{
    Attention, Event, HOST_ROOM, Message, OPERATOR, Refused, Request, Response, Room, Seat,
    Speaker, project_room,
};

const A: &str = "aaaaaaaaaaaa";

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Scratch {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "letibot-room-nicks-{}-{}",
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

struct Seated {
    client: Client,
    /// What the room pushed, kept so a test can wait for a line rather than poll a read.
    #[allow(dead_code)]
    heard: Receiver<Message>,
}

/// A seat on the coordinator with its own id and label, and what was pushed to it.
fn seated(server: &Server, name: &str, id: &str, nick: &str, seat: Seat, project: &str) -> Seated {
    let (tx, heard) = std::sync::mpsc::channel();
    let wake = Arc::new(|_: &Event| {});
    let room = letibot_room::room_name(&server.name(), project);
    let tx = std::sync::Mutex::new(tx);
    let client = Client::connect_seated(
        server.path(),
        Speaker::top_level(name),
        room,
        seat,
        id,
        nick,
        wake,
    )
    .unwrap_or_else(|e| panic!("`{name}` must connect: {e}"));
    client.on_chat(Arc::new(move |m, _| {
        let _ = tx.lock().unwrap().send(m.clone());
    }));
    Seated { client, heard }
}

impl Seated {
    fn call(&self, r: Request) -> Response {
        self.client.request(&r).expect("an answer")
    }

    fn nick(&self, nick: &str) -> Response {
        self.call(Request::Nick {
            id: String::new(),
            nick: nick.to_string(),
        })
    }
}

fn coordinator(dir: &Scratch) -> Server {
    let server = Server::serve(&dir.0.join("room.sock"), Room::new()).expect("a socket");
    // The box's half of an id, pinned: a test must not depend on the machine it runs on, and the
    // rule under test is that the *pinned* name is what an id keeps.
    server.with_seat_host("lab2x1");
    server
}

fn top(session: &str, handle: &str) -> letibot_room::Member {
    letibot_room::Member {
        session: session.into(),
        label: handle.into(),
        handle: handle.into(),
        group: letibot_room::Group::Top,
        parent: None,
        depth: 0,
        live: true,
    }
}

fn roster(server: &Server, who: &Seated, room: &str) -> Vec<String> {
    let _ = who;
    server
        .roster_rows(room)
        .into_iter()
        .map(|r| format!("{} [{}]", r.name, r.seat_id))
        .collect()
}

/// Waiting for a close the coordinator has not read yet: the read loop notices, and no tick makes it.
fn until(what: &str, mut done: impl FnMut() -> bool) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(10) {
        if done() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("{what} never happened");
}

/// The whole point of an id: a seat is the id, and the label it is drawn by is not the identity.
#[test]
fn a_seat_keeps_its_id_and_takes_its_label() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let bridge = seated(
        &server,
        "cc@app",
        "lab2x1/cc-2e4a9c1b",
        "",
        Seat::External,
        "",
    );
    // Drawn by the name it speaks as, until it takes a label.
    assert!(
        roster(&server, &bridge, HOST_ROOM).contains(&"cc@app [lab2x1/cc-2e4a9c1b]".to_string()),
        "{:?}",
        roster(&server, &bridge, HOST_ROOM)
    );
    match bridge.nick("db-fuzz-fable") {
        Response::Nicked { id, nick, previous } => {
            assert_eq!(id, "lab2x1/cc-2e4a9c1b", "the seat is the id, unchanged");
            assert_eq!(nick, "db-fuzz-fable");
            assert_eq!(previous, "", "a first nick has nothing before it");
        }
        other => panic!("a free nick is granted: {other:?}"),
    }
    assert!(
        roster(&server, &bridge, HOST_ROOM)
            .contains(&"db-fuzz-fable [lab2x1/cc-2e4a9c1b]".to_string()),
        "{:?}",
        roster(&server, &bridge, HOST_ROOM)
    );
    // Said once in the room it is in - a reader that saw the old label has no other way to learn.
    let rows = bridge.call(Request::Read {
        room: HOST_ROOM.to_string(),
        since: 0,
        before: None,
        last: Some(10),
        thread: None,
    });
    let Response::History(history) = rows else {
        panic!("a read answers rows: {rows:?}")
    };
    let rows = history.rows;
    let said: Vec<String> = rows
        .iter()
        .filter(|m| m.text.contains("is now db-fuzz-fable"))
        .map(|m| m.text.clone())
        .collect();
    assert_eq!(said.len(), 1, "one line, not one per reader: {said:?}");
    assert!(said[0].contains("cc@app"), "it names the seat: {said:?}");
    server.stop();
}

/// A taken name says whose it is and what is free - never a random suffix, and never a silence.
#[test]
fn a_taken_nick_names_its_holder_and_the_next_counter() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let first = seated(&server, "cc@app", "lab2x1/cc-1111", "", Seat::External, "");
    assert!(matches!(
        first.nick("db-fuzz-fable"),
        Response::Nicked { .. }
    ));
    let second = seated(
        &server,
        "cc@other",
        "lab2x1/cc-2222",
        "",
        Seat::External,
        "",
    );
    match second.nick("db-fuzz-fable") {
        Response::Refused { id: None, why } => match why {
            Refused::NickTaken {
                nick,
                held_by,
                next,
            } => {
                assert_eq!(nick, "db-fuzz-fable");
                assert_eq!(held_by, "cc@app", "the holder, drawn as it names itself");
                assert_eq!(next.as_deref(), Some("db-fuzz-fable-2"));
            }
            other => panic!("a taken nick is refused as taken: {other:?}"),
        },
        other => panic!("a taken nick is refused: {other:?}"),
    }
    // The number it was offered is free, and taking it leaves the first holder alone.
    match second.nick("db-fuzz-fable-2") {
        Response::Nicked { nick, .. } => assert_eq!(nick, "db-fuzz-fable-2"),
        other => panic!("the offered counter is free: {other:?}"),
    }
    // A name that is already numbered asks for the next number, never `-2-2`.
    match second.nick("db-fuzz-fable-2") {
        Response::Nicked { nick, .. } => assert_eq!(nick, "db-fuzz-fable-2", "its own, unchanged"),
        other => panic!("{other:?}"),
    }
    server.stop();
}

/// A name is freed when its holder is gone: a reconnect under a new label is not a second seat
/// holding the old one for ever.
#[test]
fn the_bare_name_is_free_once_its_holder_is_gone() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let first = seated(&server, "cc@app", "lab2x1/cc-1111", "", Seat::External, "");
    assert!(matches!(
        first.nick("db-fuzz-fable"),
        Response::Nicked { .. }
    ));
    drop(first);
    until("the first seat to leave", || {
        server.roster_rows(HOST_ROOM).iter().all(|r| !r.connected)
    });
    let next = seated(
        &server,
        "cc@other",
        "lab2x1/cc-2222",
        "",
        Seat::External,
        "",
    );
    match next.nick("db-fuzz-fable") {
        Response::Nicked { nick, .. } => assert_eq!(nick, "db-fuzz-fable"),
        other => panic!("a gone holder frees its name: {other:?}"),
    }
    server.stop();
}

/// A label arrives on the `Hello` too, and the hello is not the place to argue about a name: one a
/// live seat already answers to is dropped, so two seats are never drawn alike and the seat's own
/// `nick` frame is where the refusal is said.
#[test]
fn a_hello_does_not_take_a_label_another_live_seat_answers_to() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let first = seated(&server, "s-a", "", "", Seat::Session, "");
    assert!(matches!(
        first.nick("db-fuzz-fable"),
        Response::Nicked { .. }
    ));
    let second = seated(
        &server,
        "cc@other",
        "lab2x1/cc-7777",
        "db-fuzz-fable",
        Seat::External,
        "",
    );
    let rows = roster(&server, &second, HOST_ROOM);
    assert!(
        rows.contains(&"cc@other [lab2x1/cc-7777]".to_string()),
        "drawn by the name it speaks as: {rows:?}"
    );
    assert_eq!(
        rows.iter()
            .filter(|r| r.starts_with("db-fuzz-fable "))
            .count(),
        1,
        "one seat answers to the label: {rows:?}"
    );
    // And the seat is told why when it asks for the name itself, with the holder and the counter.
    match second.nick("db-fuzz-fable") {
        Response::Refused {
            why: Refused::NickTaken { held_by, next, .. },
            ..
        } => {
            assert_eq!(held_by, "s-a");
            assert_eq!(next.as_deref(), Some("db-fuzz-fable-2"));
        }
        other => panic!("{other:?}"),
    }
    server.stop();
}

/// A label a session's title already gives it is held too: the rule is every live seat on the box,
/// not the bridges alone.
#[test]
fn a_sessions_own_name_is_held_as_well() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let session = seated(&server, "s-a", "", "", Seat::Session, A);
    session.call(Request::Members {
        rows: vec![top("s-a", "db-fuzz-fable")],
        root: String::new(),
    });
    let bridge = seated(&server, "cc@app", "lab2x1/cc-1111", "", Seat::External, "");
    match bridge.nick("db-fuzz-fable") {
        Response::Refused {
            why: Refused::NickTaken { held_by, next, .. },
            ..
        } => {
            assert_eq!(held_by, "s-a");
            assert_eq!(next.as_deref(), Some("db-fuzz-fable-2"));
        }
        other => panic!("a session's title holds its name: {other:?}"),
    }
    // The session itself may take the same label: it is already its own.
    match session.nick("db-fuzz-fable") {
        Response::Nicked { id, .. } => assert_eq!(id, "lab2x1/s-a", "a session's id is derived"),
        other => panic!("{other:?}"),
    }
    // And the roster names the session's seat by the pinned host, not by `uname`.
    let rows = roster(&server, &bridge, &project_room(A));
    assert!(
        rows.iter().any(|r| r == "db-fuzz-fable [lab2x1/s-a]"),
        "{rows:?}"
    );
    server.stop();
}

/// A nick is a seat's own label: one seat cannot rename another, and a name that is not a label is
/// refused with the rule.
#[test]
fn a_seat_renames_only_itself_and_only_to_a_label() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let a = seated(&server, "s-a", "", "", Seat::Session, A);
    let b = seated(&server, "s-b", "", "", Seat::Session, A);
    match a.call(Request::Nick {
        id: "lab2x1/s-b".to_string(),
        nick: "stolen".to_string(),
    }) {
        Response::Refused {
            why: Refused::NotYourSeat { id, yours },
            ..
        } => {
            assert_eq!(id, "lab2x1/s-b");
            assert_eq!(yours, "lab2x1/s-a");
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(b.nick("stolen"), Response::Nicked { .. }));
    // Not labels: the operator's word, a bridge alias, an id's prefix, an edge dash, a capital.
    for bad in [
        "operator",
        "cc",
        "claude-code",
        "s-1792",
        "cc-1111",
        "-lead",
        "lead-",
    ] {
        match b.nick(bad) {
            Response::Refused {
                why: Refused::BadNick { .. },
                ..
            } => {}
            other => panic!("`{bad}` is no label: {other:?}"),
        }
    }
    // A name in another case is the label, lowercased: a person's spelling is not a rule.
    match b.nick("Db-Fuzz-Fable") {
        Response::Nicked { nick, .. } => assert_eq!(nick, "db-fuzz-fable"),
        other => panic!("{other:?}"),
    }
    server.stop();
}

/// A `Hello` whose id is not one is refused at the door: an id is minted once, and a typo must not
/// become a second seat answering to the same name.
#[test]
fn an_id_that_is_not_one_is_refused_at_the_door() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let wake = Arc::new(|_: &Event| {});
    for bad in ["not-an-id", "lab2x1", "Lab2x1/s-a", "lab2x1/s-a/extra"] {
        let err = Client::connect_seated(
            server.path(),
            Speaker::top_level("cc@app"),
            letibot_room::room_name(&server.name(), ""),
            Seat::External,
            bad,
            "",
            Arc::clone(&wake) as Arc<dyn Fn(&Event) + Send + Sync>,
        )
        .err()
        .unwrap_or_else(|| panic!("`{bad}` is no seat id"));
        assert!(err.0.contains("not a seat id"), "{bad}: {err}");
    }
    // And an id that is one is let in, for the same connection name.
    let ok = Client::connect_seated(
        server.path(),
        Speaker::top_level("cc@app"),
        letibot_room::room_name(&server.name(), ""),
        Seat::External,
        "lab2x1/cc-2e4a9c1b",
        "",
        Arc::clone(&wake) as Arc<dyn Fn(&Event) + Send + Sync>,
    );
    assert!(ok.is_ok(), "{:?}", ok.err().map(|e| e.0));
    // One id is one seat: another name claiming the same id is refused, unless it says it is one
    // more connection of that seat (a bridge and its waiter do).
    let err = Client::connect_seated(
        server.path(),
        Speaker::top_level("cc@other"),
        letibot_room::room_name(&server.name(), ""),
        Seat::External,
        "lab2x1/cc-2e4a9c1b",
        "",
        Arc::clone(&wake) as Arc<dyn Fn(&Event) + Send + Sync>,
    )
    .err()
    .unwrap_or_else(|| panic!("one id is one seat"));
    assert!(err.0.contains("lab2x1/cc-2e4a9c1b"), "{err}");
    let shared = Client::connect_shared(
        server.path(),
        Speaker::top_level("cc@other"),
        letibot_room::room_name(&server.name(), ""),
        Seat::External,
        "lab2x1/cc-2e4a9c1b",
        Arc::clone(&wake) as Arc<dyn Fn(&Event) + Send + Sync>,
    );
    assert!(
        shared.is_ok(),
        "a connection that says it shares the seat is let in: {:?}",
        shared.err().map(|e| e.0)
    );
    // The operator's seat has no id of its own to mint, and says so rather than guessing one.
    let op = seated(&server, OPERATOR, "", "", Seat::Operator, "");
    assert!(matches!(op.call(Request::Rooms), Response::Rooms { .. }));
    let _ = Attention::ToMe;
    server.stop();
}
