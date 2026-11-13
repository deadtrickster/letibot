//! A room put out of sight and brought back: what the list says about it, who may put it away, what
//! a page-back still reads, and what a message into it does. The bar is the plan's: a room with a
//! thousand messages is archived, leaves the list a person picks from, is listed as archived, and
//! pages back to message 1 after it is unarchived.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use letibot_room::client::Client;
use letibot_room::server::Server;
use letibot_room::{
    Event, HOST_ROOM, Message, OPERATOR, Refused, Request, Response, Room, RoomInfo, Seat, Speaker,
    project_room, room_name,
};

const A: &str = "aaaaaaaaaaaa";
const B: &str = "bbbbbbbbbbbb";

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Scratch {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "letibot-room-archive-{}-{}",
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
    heard: Receiver<(Message, bool)>,
}

fn seat(server: &Server, name: &str, project: &str, seat: Seat) -> Seated {
    let (tx, heard) = mpsc::channel();
    let wake = Arc::new(|_: &Event| {});
    let room = room_name(&server.name(), project);
    let client = Client::connect_as(server.path(), Speaker::top_level(name), room, seat, wake)
        .unwrap_or_else(|e| panic!("`{name}` must connect: {e}"));
    let tx = Mutex::new(tx);
    client.on_chat(Arc::new(move |m, names| {
        let _ = tx.lock().unwrap().send((m.clone(), names));
    }));
    Seated { client, heard }
}

fn coordinator(s: &Scratch) -> Server {
    Server::serve(&s.0.join("room.sock"), Room::new()).expect("a coordinator")
}

impl Seated {
    fn call(&self, r: Request) -> Response {
        self.client.request(&r).expect("an answer")
    }

    fn say(&self, id: &str, room: &str, text: &str) -> Response {
        self.call(Request::Say {
            id: id.into(),
            room: room.into(),
            text: text.into(),
            to: None,
            thread: None,
            refs: Vec::new(),
            attachments: Vec::new(),
            kind: Default::default(),
        })
    }

    fn rooms(&self) -> Vec<RoomInfo> {
        match self.call(Request::Rooms) {
            Response::Rooms { rows } => rows,
            other => panic!("{other:?}"),
        }
    }

    /// The room's row as this connection lists it.
    fn row(&self, room: &str) -> RoomInfo {
        self.rooms()
            .into_iter()
            .find(|r| r.id == room)
            .unwrap_or_else(|| panic!("`{room}` must be in the list"))
    }

    /// One page back from `before`: the newest `last` messages older than it.
    fn page(&self, room: &str, before: Option<u64>, last: u32) -> Vec<Message> {
        match self.call(Request::Read {
            room: room.into(),
            since: 0,
            last: Some(last),
            thread: None,
            before,
        }) {
            Response::History(h) => h.rows,
            other => panic!("{other:?}"),
        }
    }
}

fn said(r: Response) -> Message {
    match r {
        Response::Said { message, .. } => message,
        other => panic!("expected the message to be said: {other:?}"),
    }
}

fn refused(r: Response) -> Refused {
    match r {
        Response::Refused { why, .. } => why,
        other => panic!("expected a refusal: {other:?}"),
    }
}

fn archived(r: Response) -> (String, Option<u64>) {
    match r {
        Response::Archived { room, archived_ms } => (room, archived_ms),
        other => panic!("expected the archive to be answered: {other:?}"),
    }
}

/// The bar, end to end: a thousand messages, the room put away, listed as archived, and the log
/// still pages back to message 1 byte for byte once it is back.
#[test]
fn a_thousand_messages_are_archived_listed_and_still_page_back_to_the_first() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    server.with_chat_store(&dir.0.join("room.chat"));
    let op = seat(&server, OPERATOR, "", Seat::Operator);
    let a1 = seat(&server, "s-a1", A, Seat::Session);
    let room = project_room(A);

    // A thousand lines, said by the operator: one session is held to its budget, and the room this
    // test is about is the log, not the bucket.
    for i in 1..=1000u64 {
        said(op.say(
            &format!("op{i}"),
            &room,
            &format!("line {i} of the project"),
        ));
    }
    // `a1` is pushed every line and the room never pushes to its author, so `op` hears none.
    for _ in 0..1000 {
        let _ = a1
            .heard
            .recv_timeout(Duration::from_secs(20))
            .expect("a pushed message");
    }

    // A session of the room may put it away, and the answer says when.
    let (named, swept) = archived(a1.call(Request::ArchiveRoom { room: room.clone() }));
    assert_eq!(named, room, "the answer names the room it archived");
    assert!(swept.is_some(), "an archived room carries when");

    // It is still listed - and it is listed AS archived, which is how a person finds it again.
    let row = a1.row(&room);
    assert_eq!(
        row.archived_ms, swept,
        "the list says when, or says nothing"
    );
    assert!(
        row.archived_ms.is_some(),
        "the room is in the list as archived"
    );
    assert!(a1.row(HOST_ROOM).archived_ms.is_none(), "host is never");

    // And it pages back to message 1: the archive is a listing's fact, not a loss.
    let mut before: Option<u64> = None;
    let mut first = None;
    for _ in 0..20 {
        let page = a1.page(&room, before, 200);
        let Some(oldest) = page.first() else { break };
        before = Some(oldest.seq);
        first = Some(oldest.clone());
        if oldest.seq == 1 {
            break;
        }
    }
    let first = first.expect("the log is not empty");
    assert_eq!(
        first.seq, 1,
        "the page-back reached the room's first message"
    );
    assert_eq!(first.text, "line 1 of the project");

    // Back in the list, and the log reads the same way it did.
    let (named, back) = archived(a1.call(Request::UnarchiveRoom { room: room.clone() }));
    assert_eq!(named, room);
    assert_eq!(
        back, None,
        "a room back in the list carries no archive time"
    );
    assert!(a1.row(&room).archived_ms.is_none());
    let tail = a1.page(&room, None, 200);
    assert_eq!(tail.last().map(|m| m.seq), Some(1000));
    assert_eq!(
        tail.last().map(|m| m.text.clone()),
        Some("line 1000 of the project".into())
    );
}

/// A message into a room that was put away brings it back: the room was empty, and it is not now.
#[test]
fn a_message_into_an_archived_room_brings_it_back() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let a1 = seat(&server, "s-a1", A, Seat::Session);
    let room = project_room(A);
    said(a1.say("1", "", "before the archive"));
    archived(a1.call(Request::ArchiveRoom { room: room.clone() }));
    assert!(a1.row(&room).archived_ms.is_some());

    let m = said(a1.say("2", "", "and now something is happening"));
    assert_eq!(m.room, room);
    assert_eq!(
        a1.row(&room).archived_ms,
        None,
        "the room came back with the message that needed it"
    );
}

/// The host room is never archived, and another project's room is not a session's to put away.
#[test]
fn the_host_room_is_kept_and_another_projects_room_is_not_yours() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let a1 = seat(&server, "s-a1", A, Seat::Session);
    let b1 = seat(&server, "s-b1", B, Seat::Session);
    let op = seat(&server, OPERATOR, "", Seat::Operator);
    said(a1.say("1", "", "a line so both project rooms exist"));
    said(b1.say("1", "", "and one in the other project"));

    assert!(matches!(
        refused(a1.call(Request::ArchiveRoom {
            room: HOST_ROOM.into()
        })),
        Refused::HostRoomKept
    ));
    assert!(matches!(
        refused(op.call(Request::ArchiveRoom {
            room: HOST_ROOM.into()
        })),
        Refused::HostRoomKept,
    ));
    assert!(matches!(
        refused(a1.call(Request::ArchiveRoom {
            room: project_room(B)
        })),
        Refused::NotYourRoom { .. }
    ));
    // A room nobody derived is refused as one that cannot exist, not created by the asking.
    assert!(matches!(
        refused(a1.call(Request::ArchiveRoom {
            room: "project/not-a-key".into()
        })),
        Refused::UnknownChatRoom { .. }
    ));
}
