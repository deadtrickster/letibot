//! A dm's addressee: what the log stores, who is pushed, and what a name nobody answers to is
//! refused with. The shape that filed R163 - a session's dm to a Claude Code seat stored against a
//! session of another project - is `the_1038_shape_...` below.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use letibot_room::client::Client;
use letibot_room::server::Server;
use letibot_room::{
    Event, Group, Member, Refused, Request, Response, Room, Seat, Speaker, room_name,
};

const A: &str = "aaaaaaaaaaaa";
const B: &str = "bbbbbbbbbbbb";

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Scratch {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "letibot-room-dm-{}-{}",
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
    heard: Receiver<(letibot_room::Message, bool)>,
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

impl Seated {
    fn call(&self, r: Request) -> Response {
        self.client.request(&r).expect("an answer")
    }

    fn dm(&self, id: &str, to: &str, text: &str) -> Response {
        self.call(Request::Dm {
            id: id.into(),
            to: to.into(),
            text: text.into(),
            thread: None,
            attachments: Vec::new(),
        })
    }

    fn say(&self, id: &str, to: Option<&str>, text: &str, thread: Option<u64>) -> Response {
        self.call(Request::Say {
            id: id.into(),
            room: String::new(),
            text: text.into(),
            to: to.map(str::to_string),
            thread,
            refs: Vec::new(),
            attachments: Vec::new(),
        })
    }

    fn silent(&self) {
        assert!(
            self.heard.recv_timeout(Duration::from_millis(300)).is_err(),
            "nothing was pushed"
        );
    }

    fn next(&self) -> (letibot_room::Message, bool) {
        self.heard
            .recv_timeout(Duration::from_secs(5))
            .expect("a pushed message")
    }
}

fn top(session: &str, handle: &str) -> Member {
    Member {
        session: session.into(),
        label: handle.into(),
        handle: handle.into(),
        group: Group::Top,
        parent: None,
        depth: 0,
        live: true,
    }
}

fn tell(s: &Seated, rows: Vec<Member>) {
    s.call(Request::Members {
        rows,
        root: String::new(),
    });
}

fn refused(r: Response) -> Refused {
    match r {
        Response::Refused { why, .. } => why,
        other => panic!("expected a refusal: {other:?}"),
    }
}

fn said(r: Response) -> letibot_room::Message {
    match r {
        Response::Said { message, .. } => message,
        other => panic!("expected the message to be said: {other:?}"),
    }
}

/// A coordinator with a chat store, so what the LOG holds can be asserted - not what a reader is
/// shown, which is the handle and hides exactly the re-addressing this file is about.
fn coordinator(s: &Scratch) -> Server {
    let server = Server::serve(&s.0.join("room.sock"), Room::new()).expect("a coordinator");
    server.with_chat_store(&s.0.join("chat.json"));
    server
}

/// `(seq, author, to, thread)` of every message in the log.
fn logged(s: &Scratch) -> Vec<(u64, String, Option<String>, Option<u64>)> {
    let text = std::fs::read_to_string(s.0.join("chat.json")).unwrap_or_default();
    text.lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter_map(|v| v.get("msg").and_then(|m| m.get("message")).cloned())
        .map(|m| {
            (
                m["seq"].as_u64().unwrap_or(0),
                m["author"].as_str().unwrap_or("").to_string(),
                m["to"].as_str().map(str::to_string),
                m["thread"].as_u64(),
            )
        })
        .collect()
}

/// A dm to `cc@<name>` is that bridge's: the log keeps the handle, the bridge is pushed the
/// message, and the other project's session whose handle is the bare name hears nothing.
#[test]
fn a_dm_to_a_bridge_handle_is_stored_as_the_handle_and_reaches_the_bridge() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let me = seat(&server, "s-dbfuzz", A, Seat::Session);
    let arango = seat(&server, "s-arango", B, Seat::Session);
    tell(&me, vec![top("s-dbfuzz", "db-fuzz")]);
    tell(&arango, vec![top("s-arango", "serenedb")]);
    let bridge = seat(&server, "cc@serenedb", "", Seat::External);

    let dm = said(me.dm("1", "cc@serenedb", "cc@serenedb: two questions"));
    assert_eq!(dm.room, letibot_room::chat::DM_ROOM);
    assert_eq!(
        dm.to.as_deref(),
        Some("cc@serenedb"),
        "the sender is told who it went to"
    );
    assert_eq!(
        logged(&dir),
        vec![(1, "s-dbfuzz".into(), Some("cc@serenedb".into()), None)],
        "the log holds the handle, not a session"
    );
    let (heard, named) = bridge.next();
    assert_eq!(heard.seq, 1);
    assert!(named, "a dm names the one it is for");
    arango.silent();
}

/// A bridge that is not connected answers nothing: the dm is refused, and the refusal lists the
/// names that would have reached somebody. Nothing is written.
#[test]
fn a_dm_to_a_bridge_that_is_away_is_refused_naming_the_ones_that_answer() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let me = seat(&server, "s-dbfuzz", A, Seat::Session);
    tell(&me, vec![top("s-dbfuzz", "db-fuzz"), top("s-peer", "peer")]);
    let _other = seat(&server, "cc@letibot", "", Seat::External);

    match refused(me.dm("1", "cc@serenedb", "anyone?")) {
        Refused::UnknownAddressee { name, known } => {
            assert_eq!(name, "cc@serenedb");
            assert!(known.contains(&"cc@letibot".to_string()), "{known:?}");
            assert!(known.contains(&"peer".to_string()), "{known:?}");
            let sentence = Refused::UnknownAddressee { name, known }.to_string();
            assert!(sentence.contains("cc@letibot"), "{sentence}");
        }
        other => panic!("{other:?}"),
    }
    assert!(logged(&dir).is_empty(), "a refused dm writes nothing");
}

/// The shape R163 was filed on: a session's dm to a Claude Code seat, and the name it wrote is the
/// handle of a session in another project. It used to be stored against that session, silently;
/// it is refused now, with the ids that do go by the name, and the id itself still reaches it.
#[test]
fn the_1038_shape_a_handle_from_another_project_is_refused_and_not_re_addressed() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let me = seat(&server, "s-dbfuzz", A, Seat::Session);
    let arango = seat(&server, "s-arango", B, Seat::Session);
    tell(&me, vec![top("s-dbfuzz", "db-fuzz")]);
    tell(&arango, vec![top("s-arango", "serenedb")]);

    // No bridge by that name is connected, and the bare name is the arango session's handle.
    for (id, to) in [("1", "serenedb"), ("2", "cc@serenedb")] {
        let why = refused(me.dm(id, to, "cc@serenedb: two questions"));
        match (to, why) {
            ("serenedb", Refused::HandleInAnotherProject { name, sessions }) => {
                assert_eq!(name, "serenedb");
                assert_eq!(sessions, vec!["s-arango".to_string()]);
            }
            ("cc@serenedb", Refused::UnknownAddressee { name, .. }) => {
                assert_eq!(name, "cc@serenedb")
            }
            (to, other) => panic!("`{to}` was answered {other:?}"),
        }
    }
    assert!(
        logged(&dir).is_empty(),
        "neither dm may be stored, and least of all against another session: {:?}",
        logged(&dir)
    );
    arango.silent();

    // The id is how a session reaches another project's session, and it is what the refusal said.
    said(me.dm("3", "s-arango", "cc@serenedb: two questions"));
    assert_eq!(
        logged(&dir),
        vec![(1, "s-dbfuzz".into(), Some("s-arango".into()), None)],
        "naming the id reaches it, and the sender is told who it went to"
    );
}

/// A dm thread is between the two people in it. A `say` that names a third stays refused rather
/// than being handed to the thread's other party - which is the second way a dm to a bridge was
/// stored against an unrelated session.
#[test]
fn a_say_that_names_somebody_else_does_not_ride_another_pairs_dm_thread() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let me = seat(&server, "s-bridge-1", A, Seat::Session);
    let arango = seat(&server, "s-arango", B, Seat::Session);
    tell(&me, vec![top("s-bridge-1", "db-fuzz")]);
    tell(&arango, vec![top("s-arango", "arango")]);
    let _bridge = seat(&server, "cc@serenedb", "", Seat::External);

    let root = said(me.dm("1", "s-arango", "about the pin")).seq;
    match refused(me.say("2", Some("cc@serenedb"), "cc@serenedb: look", Some(root))) {
        Refused::ThreadIsAnotherDm { thread, with } => {
            assert_eq!((thread, with.as_str()), (root, "s-arango"))
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        logged(&dir),
        vec![(1, "s-bridge-1".into(), Some("s-arango".into()), None)],
        "the refused say added no row"
    );

    // Answering the thread's own party is the same conversation, so it is allowed - and the
    // sender reads back the handle of who it went to.
    let back = said(me.say("3", Some("s-arango"), "the pin is fine", Some(root)));
    assert_eq!(back.to.as_deref(), Some("arango"), "the sender is told");
    assert_eq!(
        logged(&dir).last(),
        Some(&(2, "s-bridge-1".into(), Some("s-arango".into()), Some(root)))
    );
}
