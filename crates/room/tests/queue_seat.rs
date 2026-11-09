//! A merge-queue seat (gatekeeper) hears no broadcast and no mention. It hears the operator, and
//! sessions of its own project, by DM or ask.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use letibot_room::client::Client;
use letibot_room::server::Server;
use letibot_room::{Event, HOST_ROOM, Refused, Request, Response, Room, Seat, Speaker, room_name};

const A: &str = "aaaaaaaaaaaa";
const B: &str = "bbbbbbbbbbbb";
const GK: &str = "s-gatekeeper-aaaaaaaaaaaa";

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Scratch {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "letibot-room-qseat-{}-{}",
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
    heard: Receiver<letibot_room::Message>,
}

fn seat(server: &Server, name: &str, project: &str, kind: Seat) -> Seated {
    let (tx, heard) = mpsc::channel();
    let wake = Arc::new(|_: &Event| {});
    let room = room_name(&server.name(), project);
    let client = Client::connect_as(server.path(), Speaker::top_level(name), room, kind, wake)
        .unwrap_or_else(|e| panic!("`{name}` must connect: {e}"));
    let tx = Mutex::new(tx);
    client.on_chat(Arc::new(move |m, _| {
        let _ = tx.lock().unwrap().send(m.clone());
    }));
    Seated { client, heard }
}

impl Seated {
    fn call(&self, r: Request) -> Response {
        self.client.request(&r).expect("an answer")
    }

    fn say(&self, room: &str, text: &str, to: Option<&str>) -> Response {
        self.call(Request::Say {
            id: format!("say-{text}"),
            room: room.into(),
            text: text.into(),
            to: to.map(str::to_string),
            thread: None,
            refs: Vec::new(),
            attachments: Vec::new(),
            kind: Default::default(),
        })
    }

    fn dm(&self, to: &str, text: &str) -> Response {
        self.call(Request::Dm {
            id: format!("dm-{text}"),
            to: to.into(),
            text: text.into(),
            thread: None,
            attachments: Vec::new(),
        })
    }

    fn ask(&self, to: &str, q: &str) -> Response {
        self.call(Request::Ask {
            id: format!("ask-{q}"),
            room: String::new(),
            to: to.into(),
            path: String::new(),
            topic: String::new(),
            question: q.into(),
        })
    }

    fn next(&self) -> letibot_room::Message {
        self.heard
            .recv_timeout(Duration::from_secs(5))
            .expect("a pushed message")
    }
}

fn refused(r: Response) -> Refused {
    match r {
        Response::Refused { why, .. } => why,
        other => panic!("expected a refusal: {other:?}"),
    }
}

#[test]
fn a_host_broadcast_and_a_mention_do_not_reach_a_gatekeeper_but_a_project_dm_does() {
    let dir = Scratch::new();
    let server = Server::serve(&dir.0.join("room.sock"), Room::new()).expect("a coordinator");
    let gk = seat(&server, GK, A, Seat::Session);
    let mine = seat(&server, "s-mine", A, Seat::Session);
    let other = seat(&server, "s-other", B, Seat::Session);
    let operator = seat(&server, "operator", "", Seat::Operator);

    other.say(HOST_ROOM, "ping everyone", None);
    other.say(HOST_ROOM, &format!("hello @{GK}"), None);
    mine.say(
        &letibot_room::project_room(A),
        &format!("@{GK} are you there"),
        None,
    );
    operator.say(HOST_ROOM, "an unaddressed broadcast", None);
    mine.dm(GK, "how does a branch get in?");

    let got = gk.next();
    assert_eq!(got.text, "how does a branch get in?", "{got:?}");
    assert_eq!(got.room, letibot_room::DM_ROOM);
}

#[test]
fn another_projects_session_is_refused_a_dm_or_ask_and_the_operator_and_kin_may_ask() {
    let dir = Scratch::new();
    let server = Server::serve(&dir.0.join("room.sock"), Room::new()).expect("a coordinator");
    let gk = seat(&server, GK, A, Seat::Session);
    let mine = seat(&server, "s-mine", A, Seat::Session);
    let other = seat(&server, "s-other", B, Seat::Session);

    assert!(matches!(
        refused(other.dm(GK, "hi")),
        Refused::QueueSeatOwnProject { .. }
    ));
    assert!(matches!(
        refused(other.ask(GK, "hi?")),
        Refused::QueueSeatOwnProject { .. }
    ));
    assert!(matches!(
        mine.ask(GK, "which verb enqueues?"),
        Response::Asked { .. }
    ));
    let got = gk.next();
    assert_eq!(got.text, "which verb enqueues?");
}
