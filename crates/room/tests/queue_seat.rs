//! A merge-queue seat (gatekeeper) hears no broadcast and no mention. It hears the operator, and
//! sessions of its own project, by DM or ask.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use letibot_room::client::Client;
use letibot_room::server::Server;
use letibot_room::{
    Event, HOST_ROOM, Refused, Request, Response, Room, SayKind, Seat, Speaker, room_name,
};

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

/// Decision 21: another project's session may brief a queue seat. The `brief` kind is
/// filed - delivered to the seat, kept on the row - and never authority: it reaches its
/// reader as a passing note (`Passing`), so nothing rings and no turn is run for it.
#[test]
fn a_brief_from_another_project_is_filed_and_is_never_authority() {
    let dir = Scratch::new();
    let server = Server::serve(&dir.0.join("room.sock"), Room::new()).expect("a coordinator");
    let gk = seat(&server, GK, A, Seat::Session);
    let other = seat(&server, "s-other", B, Seat::Session);

    // The plain line from another project stays refused.
    assert!(matches!(
        refused(other.say(letibot_room::HOST_ROOM, "psst", Some(GK))),
        Refused::QueueSeatOwnProject { .. }
    ));
    // The brief is posted, and the seat hears it: the row is filed, not dropped.
    let brief = other.call(Request::Say {
        id: "brief-1".into(),
        room: letibot_room::HOST_ROOM.into(),
        text: "context for the queue: the rano pin is off by one until friday".into(),
        to: Some(GK.into()),
        thread: None,
        refs: Vec::new(),
        attachments: Vec::new(),
        kind: SayKind::Brief,
    });
    assert!(
        matches!(brief, Response::Said { .. }),
        "a brief is filed, not refused: {brief:?}"
    );
    let got = gk.next();
    assert_eq!(got.say_kind(), SayKind::Brief, "{got:?}");
    // The room resolved the seat to its speaking handle; that the seat's own connection
    // received the message is the proof the row was filed.
    assert!(
        got.to.as_deref().is_some_and(|t| t.contains("gatekeeper")),
        "{got:?}"
    );
    // Filed, never authority: it reaches the seat as a passing note even though it names
    // it, so the seat's link holds the line instead of ringing a turn for it.
    assert_eq!(
        got.reach(true),
        letibot_room::chat::Reach::Passing,
        "{got:?}"
    );
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
        mine.ask(GK, "which verb enqueues?"),
        Response::Asked { .. }
    ));
    let got = gk.next();
    assert_eq!(got.text, "which verb enqueues?");
    // An ask is a one-to-one door any project may use.
    let r = other.ask(GK, "hi?");
    assert!(matches!(r, Response::Asked { .. }), "{r:?}");
    assert_eq!(gk.next().text, "hi?");
}

impl Seated {
    fn say_kind(&self, to: &str, text: &str, kind: SayKind, thread: Option<u64>) -> Response {
        self.call(Request::Say {
            id: format!("say-{text}"),
            room: String::new(),
            text: text.into(),
            to: Some(to.into()),
            thread,
            refs: Vec::new(),
            attachments: Vec::new(),
            kind,
        })
    }
}

fn seq_of(r: Response) -> u64 {
    match r {
        Response::Said { message, .. } => message.seq,
        other => panic!("expected it said: {other:?}"),
    }
}

#[test]
fn another_project_finds_the_seat_in_the_directory_and_its_decision_dm_is_delivered() {
    let dir = Scratch::new();
    let server = Server::serve(&dir.0.join("room.sock"), Room::new()).expect("a coordinator");
    let gk = seat(&server, GK, A, Seat::Session);
    let other = seat(&server, "s-other", B, Seat::Session);

    let rows = match other.call(Request::Roster {
        room: letibot_room::SEATS.into(),
    }) {
        Response::Roster { room, rows } => {
            assert_eq!(room, letibot_room::SEATS);
            rows
        }
        r => panic!("a roster: {r:?}"),
    };
    let row = rows
        .iter()
        .find(|r| r.id == GK)
        .unwrap_or_else(|| panic!("the queue seat is listed: {rows:?}"));
    assert!(row.name.starts_with("gatekeeper@"), "{row:?}");
    assert_eq!(row.project, A);
    assert!(row.connected && row.seat.is_some(), "{row:?}");
    assert!(rows.iter().all(|r| r.id != "s-other"), "{rows:?}");

    // Plain chatter is refused, and the refusal names what would be taken.
    let why = refused(other.say_kind(GK, "just saying hi", SayKind::Post, None));
    assert!(
        matches!(why, Refused::QueueSeatOwnProject { .. }),
        "{why:?}"
    );
    let said = why.to_string();
    assert!(
        said.contains("kind: decision") && said.contains("ask") && said.contains("operator"),
        "{said}"
    );
    assert!(matches!(
        refused(other.dm(GK, "hello there")),
        Refused::QueueSeatOwnProject { .. }
    ));

    other.say_kind(GK, "reviewers are deepseek now", SayKind::Decision, None);
    let got = gk.next();
    assert_eq!(got.text, "reviewers are deepseek now");
    assert_eq!(got.room, letibot_room::DM_ROOM);
}

#[test]
fn a_dm_relaying_the_operators_message_reaches_another_projects_seat() {
    let dir = Scratch::new();
    let server = Server::serve(&dir.0.join("room.sock"), Room::new()).expect("a coordinator");
    let gk = seat(&server, GK, A, Seat::Session);
    let other = seat(&server, "s-other", B, Seat::Session);
    let operator = seat(&server, "operator", "", Seat::Operator);

    let op = seq_of(operator.say(
        HOST_ROOM,
        "tell rano's gatekeeper to use two reviewers",
        None,
    ));
    let mine = seq_of(other.say(
        HOST_ROOM,
        "@operator which reviewer model does rano use now",
        None,
    ));
    assert!(matches!(
        refused(other.say_kind(GK, "from a peer", SayKind::Post, Some(mine))),
        Refused::QueueSeatOwnProject { .. }
    ));
    other.say_kind(GK, "operator says two reviewers", SayKind::Post, Some(op));
    let got = gk.next();
    assert!(got.text.contains("operator says two reviewers"), "{got:?}");
    assert!(
        got.refs
            .iter()
            .any(|r| r.kind == letibot_room::RELAYS && r.id == op.to_string()),
        "{got:?}"
    );
}

#[test]
fn a_prompt_across_projects_needs_the_operators_word() {
    let dir = Scratch::new();
    let server = Server::serve(&dir.0.join("room.sock"), Room::new()).expect("a coordinator");
    let other = seat(&server, "s-other", B, Seat::Session);
    let operator = seat(&server, "operator", "", Seat::Operator);
    let ask = |who: &Seated, to: &str, project: &str, because: Option<u64>| {
        who.call(Request::MayPrompt {
            id: format!("p-{to}-{because:?}"),
            to: to.into(),
            project: project.into(),
            because,
        })
    };

    assert!(matches!(
        ask(&other, "s-kin", B, None),
        Response::MayPrompt { session, .. } if session == "s-kin"
    ));
    let why = refused(ask(&other, "s-far", A, None));
    assert!(
        matches!(why, Refused::PromptNeedsOperator { .. }),
        "{why:?}"
    );
    assert!(why.to_string().contains("because"), "{why}");
    assert!(matches!(
        refused(ask(&other, GK, "", None)),
        Refused::PromptNeedsOperator { .. }
    ));

    let mine = seq_of(other.say(HOST_ROOM, "@operator may I?", None));
    assert!(matches!(
        refused(ask(&other, "s-far", A, Some(mine))),
        Refused::PromptCitesNotOperator { .. }
    ));
    let op = seq_of(operator.say(HOST_ROOM, "yes, tell s-far", None));
    assert!(matches!(
        ask(&other, "s-far", A, Some(op)),
        Response::MayPrompt { session, .. } if session == "s-far"
    ));
    assert!(matches!(
        ask(&operator, "s-far", A, None),
        Response::MayPrompt { .. }
    ));
}
