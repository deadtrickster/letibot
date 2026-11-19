//! A seat's inbox cursor and its unread count are the SEAT's, not the name it speaks as: a bridge
//! that reconnects under another label with the same id reads on from where it stopped, instead of
//! being handed everything it had already read or losing what it had not.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use letibot_room::client::Client;
use letibot_room::server::Server;
use letibot_room::{
    Attention, Event, Message, Request, Response, Room, Seat, Speaker, project_room,
};

/// The project both seats are in: the host room refuses a line that names nobody, and this test is
/// about the cursor rather than about etiquette.
const A: &str = "aaaaaaaaaaaa";

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Scratch {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "letibot-room-cursors-{}-{}",
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
    #[allow(dead_code)]
    heard: Receiver<Message>,
}

fn seated(server: &Server, name: &str, id: &str, seat: Seat, project: &str) -> Seated {
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
        "",
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

    fn say(&self, id: &str, text: &str) -> Response {
        self.call(Request::Say {
            id: id.into(),
            room: project_room(A),
            text: text.into(),
            to: None,
            thread: None,
            refs: Vec::new(),
            attachments: Vec::new(),
            kind: Default::default(),
        })
    }

    fn inbox(&self) -> (Vec<u64>, u64) {
        match self.call(Request::Inbox {
            attention: Attention::All,
            limit: None,
            after: None,
        }) {
            Response::Inbox { rows, cursor, .. } => (rows.iter().map(|m| m.seq).collect(), cursor),
            other => panic!("{other:?}"),
        }
    }

    fn unread(&self) -> u64 {
        match self.call(Request::Rooms) {
            Response::Rooms { rows } => rows
                .iter()
                .find(|r| r.id == project_room(A))
                .map(|r| r.unread)
                .unwrap_or(0),
            other => panic!("{other:?}"),
        }
    }
}

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

/// The lead's own case: a bridge whose handle changed (`cc@app`, then `cc@app-abc` after a twin)
/// is one seat, and what it read is still read.
#[test]
fn a_seat_that_reconnects_under_another_name_keeps_its_cursor_and_its_unread() {
    let dir = Scratch::new();
    let server = Server::serve(&dir.0.join("room.sock"), Room::new()).expect("a socket");
    server.with_seat_host("lab2x1");
    let speaker = seated(&server, "s-a", "", Seat::Session, A);
    let first = seated(&server, "cc@app", "lab2x1/cc-7001", Seat::External, A);

    speaker.say("m1", "one");
    speaker.say("m2", "two");
    let (rows, _) = first.inbox();
    assert_eq!(rows, vec![1, 2], "the seat reads what it has not read");
    assert_eq!(first.unread(), 2);
    // Read up to the second message: the cursor is the seat's from here on.
    assert!(matches!(
        first.call(Request::Ack { seq: 2 }),
        Response::Acked { cursor: 2 }
    ));
    assert_eq!(first.unread(), 0);
    drop(first);
    // The room refuses a second connection of one id under another name, so the old connection's
    // close has to be read before the seat can come back as its new self.
    until("the first connection to close", || {
        server
            .roster_rows(&project_room(A))
            .iter()
            .all(|r| !(r.id == "cc@app" && r.connected))
    });

    speaker.say("m3", "three");
    // The same seat under the name it took after a twin: same id, another label.
    let again = seated(&server, "cc@app-abc", "lab2x1/cc-7001", Seat::External, A);
    let (rows, cursor) = again.inbox();
    assert_eq!(
        rows,
        vec![3],
        "only what it had not read: the cursor came back with the seat"
    );
    assert_eq!(cursor, 2, "and it is where the seat left it");
    assert_eq!(again.unread(), 1);

    // A different seat is not handed that cursor, and it is not handed the backlog either: a seat
    // armed now hears what happens next, which is the room's rule for a reader that has none.
    let other = seated(&server, "cc@other", "lab2x1/cc-7002", Seat::External, A);
    let (rows, cursor) = other.inbox();
    assert!(rows.is_empty(), "a new seat starts at the head: {rows:?}");
    assert_eq!(cursor, 3, "its own cursor is where its arm put it");
    speaker.say("m4", "four");
    let (rows, _) = other.inbox();
    assert_eq!(rows, vec![4], "and it hears what happens after it arrived");
    server.stop();
}

/// An ask is the seat's too: opened at `cc@app`, answered after the same seat came back as
/// `cc@app-abc`, which is the whole reason a wait or a question may not be keyed on a label.
#[test]
fn an_ask_survives_the_answerers_rename_and_is_still_answerable() {
    let dir = Scratch::new();
    let server = Server::serve(&dir.0.join("room.sock"), Room::new()).expect("a socket");
    server.with_seat_host("lab2x1");
    let asker = seated(&server, "s-a", "", Seat::Session, A);
    let answerer = seated(&server, "cc@app", "lab2x1/cc-7001", Seat::External, A);

    let ask = match asker.call(Request::Ask {
        id: "q1".into(),
        room: project_room(A),
        to: "cc@app".into(),
        path: String::new(),
        topic: String::new(),
        question: "which port?".into(),
    }) {
        Response::Asked { ask, .. } => ask.seq,
        other => panic!("an ask is asked: {other:?}"),
    };
    // It is owed to the seat under the name it had.
    match answerer.call(Request::Asks) {
        Response::Asks { of_me, .. } => {
            assert_eq!(of_me.len(), 1, "{of_me:?}");
            assert_eq!(of_me[0].asker, "s-a");
        }
        other => panic!("{other:?}"),
    }
    drop(answerer);
    until("the answerer to close", || {
        server
            .roster_rows(&project_room(A))
            .iter()
            .all(|r| !(r.id == "cc@app" && r.connected))
    });

    // The same seat, answering under the label it took after the twin: accepted, and settled.
    let renamed = seated(&server, "cc@app-abc", "lab2x1/cc-7001", Seat::External, A);
    match renamed.call(Request::Asks) {
        Response::Asks { of_me, .. } => {
            assert_eq!(
                of_me.len(),
                1,
                "the question is still this seat's: {of_me:?}"
            );
            assert_eq!(of_me[0].seq, ask);
        }
        other => panic!("{other:?}"),
    }
    match renamed.call(Request::Reply {
        id: "a1".into(),
        ask,
        text: "5433".into(),
    }) {
        Response::Asked { ask: settled, .. } => {
            assert!(!settled.open(), "the answer settled it: {settled:?}");
            assert_eq!(
                settled.target, "cc@app",
                "filed under the seat it was asked of"
            );
        }
        other => panic!("the seat answers its own ask after a rename: {other:?}"),
    }
    match asker.call(Request::Asks) {
        Response::Asks { by_me, .. } => {
            assert!(by_me.iter().any(|a| a.seq == ask && !a.open()), "{by_me:?}");
        }
        other => panic!("{other:?}"),
    }
    server.stop();
}
