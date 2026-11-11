//! A seat's rooms: a Claude Code seat is scoped like a session. It is in the host room and in its own
//! project's, the host room's roster is every top-level seat on the host, and only the operator is in
//! every room. Read and post keep to those two; a dm crosses for one message.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use letibot_room::client::Client;
use letibot_room::server::Server;
use letibot_room::{
    Attention, Event, Group, HOST_ROOM, Member, Message, OPERATOR, Presence, Refused, Request,
    Response, Room, Seat, Speaker, project_room, room_name,
};

const A: &str = "aaaaaaaaaaaa";
const B: &str = "bbbbbbbbbbbb";

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Scratch {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "letibot-room-rooms-{}-{}",
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

impl Seated {
    fn call(&self, r: Request) -> Response {
        self.client.request(&r).expect("an answer")
    }

    fn rooms(&self) -> Vec<String> {
        match self.call(Request::Rooms) {
            Response::Rooms { rows } => rows.into_iter().map(|r| r.id).collect(),
            other => panic!("{other:?}"),
        }
    }

    fn roster(&self, room: &str) -> Vec<Presence> {
        match self.call(Request::Roster {
            room: room.to_string(),
        }) {
            Response::Roster { rows, .. } => rows,
            other => panic!("{other:?}"),
        }
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

    fn read(&self, room: &str) -> Response {
        self.call(Request::Read {
            room: room.into(),
            since: 0,
            last: None,
            thread: None,
            before: None,
        })
    }

    fn next(&self) -> (Message, bool) {
        self.heard
            .recv_timeout(Duration::from_secs(5))
            .expect("a pushed message")
    }

    fn next_seq(&self, seq: u64) -> (Message, bool) {
        loop {
            let got = self.next();
            if got.0.seq == seq {
                return got;
            }
        }
    }

    fn silent(&self) {
        assert!(
            self.heard.recv_timeout(Duration::from_millis(300)).is_err(),
            "nothing was pushed"
        );
    }
}

fn top(session: &str, label: &str) -> Member {
    Member {
        session: session.into(),
        label: label.into(),
        handle: String::new(),
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

fn said(r: Response) -> Message {
    match r {
        Response::Said { message, .. } => message,
        other => panic!("expected the message to be said: {other:?}"),
    }
}

fn coordinator(s: &Scratch) -> Server {
    Server::serve(&s.0.join("room.sock"), Room::new()).expect("a coordinator")
}

/// An external's rooms are the two a session's are: it lists them, and it is in neither of another
/// project's roster's.
#[test]
fn an_external_in_a_is_in_as_roster_and_hosts_and_not_bs() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let b1 = seat(&server, "s-b1", B, Seat::Session);
    let cc = seat(&server, "cc@stroppy-io", A, Seat::External);
    tell(&b1, vec![top("s-b1", "db-fuzz")]);

    assert_eq!(
        cc.rooms(),
        vec![HOST_ROOM.to_string(), project_room(A)],
        "the host room and its own project's, never the third"
    );
    let a = cc.roster(&project_room(A));
    assert!(
        a.iter().any(|r| r.id == "cc@stroppy-io" && r.connected),
        "{a:?}"
    );
    let b1_view = b1.roster(&project_room(B));
    assert!(
        b1_view.iter().all(|r| r.id != "cc@stroppy-io"),
        "B's own sessions do not list it: {b1_view:?}"
    );
    assert!(
        matches!(
            refused(cc.read(&project_room(B))),
            Refused::NotYourRoom { .. }
        ),
        "and it cannot read B either"
    );
    let host = cc.roster(HOST_ROOM);
    assert!(
        host.iter()
            .any(|r| r.id == "cc@stroppy-io" && r.kind == Seat::External),
        "the host room lists every top-level seat: {host:?}"
    );
    // And a line in B is not pushed to it: reading B is not what a refusal takes away.
    said(b1.say("b1", "", "only in b"));
    cc.silent();
    // Nor kept for it: the inbox is the two rooms a seat is in and its dms.
    match cc.call(Request::Inbox {
        attention: Attention::All,
        limit: None,
    }) {
        Response::Inbox { rows, .. } => {
            assert!(rows.iter().all(|m| m.room != project_room(B)), "{rows:?}")
        }
        other => panic!("{other:?}"),
    }
    // An `@` from B's room brings it in for that conversation - the one door into another
    // project's room, and the same door a session of another project takes.
    let m = said(b1.say("b2", &project_room(B), "@cc@stroppy-io look at this"));
    assert_eq!(cc.next_seq(m.seq).0.room, project_room(B));
    assert!(
        cc.rooms().contains(&project_room(B)),
        "invited: {:?}",
        cc.rooms()
    );
    server.stop();
}

/// The host room's roster is every project's top-level sessions and the externals, and each row says
/// which project it is in - a session of `etcd` is a host member without leaving its own room.
#[test]
fn the_host_roster_holds_every_projects_toplevel_and_carries_the_project() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let a1 = seat(&server, "s-a1", A, Seat::Session);
    let b1 = seat(&server, "s-b1", B, Seat::Session);
    let _cc = seat(&server, "cc@stroppy-io", A, Seat::External);
    tell(&a1, vec![top("s-a1", "db-fuzz")]);
    tell(&b1, vec![top("s-b1", "serenedb")]);

    let host = a1.roster(HOST_ROOM);
    for (id, project, label) in [
        ("s-a1", A, "db-fuzz"),
        ("s-b1", B, "serenedb"),
        ("cc@stroppy-io", A, ""),
    ] {
        let row = host
            .iter()
            .find(|r| r.id == id)
            .unwrap_or_else(|| panic!("`{id}` is not in the host room: {host:?}"));
        assert_eq!(row.project, project, "`{id}`: {row:?}");
        assert!(row.connected, "`{id}`: {row:?}");
        if !label.is_empty() {
            assert_eq!(row.label, label, "`{id}`: {row:?}");
        }
    }
    // The same session in its own room, where the project is the room's.
    let own = b1.roster(&project_room(B));
    assert!(
        own.iter().any(|r| r.id == "s-b1" && r.label == "serenedb"),
        "{own:?}"
    );
    server.stop();
}

/// A bridge whose folder is in no project is in the host room alone: the `""` Hello an older bridge
/// still sends reads as *I do not say*, and there is no project room to be in.
#[test]
fn an_external_with_no_project_is_in_host_only() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let a1 = seat(&server, "s-a1", A, Seat::Session);
    let cc = seat(&server, "cc@letibot", "", Seat::External);

    assert_eq!(cc.rooms(), vec![HOST_ROOM.to_string()]);
    let host = a1.roster(HOST_ROOM);
    assert!(
        host.iter()
            .any(|r| r.id == "cc@letibot" && r.project.is_empty()),
        "{host:?}"
    );
    assert!(matches!(
        refused(cc.read(&project_room(A))),
        Refused::NotYourRoom { .. }
    ));
    server.stop();
}

/// The operator is in every room and posts in all of them; that is the one seat left unscoped.
#[test]
fn the_operator_is_in_every_room() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let a1 = seat(&server, "s-a1", A, Seat::Session);
    let op = seat(&server, OPERATOR, "", Seat::Operator);
    let _b1 = seat(&server, "s-b1", B, Seat::Session);

    let mut all = op.rooms();
    all.sort();
    assert_eq!(
        all,
        vec![HOST_ROOM.to_string(), project_room(A), project_room(B)]
    );
    for room in [HOST_ROOM, &project_room(A), &project_room(B)] {
        assert!(
            op.roster(room).iter().any(|r| r.id == OPERATOR),
            "the operator is in {room}"
        );
    }
    let m = said(op.say("op", &project_room(B), "op in b"));
    assert_eq!(
        (m.room.as_str(), m.kind),
        (project_room(B).as_str(), Seat::Operator)
    );
    assert!(a1.roster(&project_room(A)).iter().all(|r| r.id != "s-b1"));
    server.stop();
}

/// Another project's room is refused, by name and with the room the seat is in - `unknown_room` would
/// read as a typo it should fix, and there is nothing to fix.
#[test]
fn an_externals_read_and_say_of_another_projects_room_are_refused() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let a1 = seat(&server, "s-a1", A, Seat::Session);
    let cc = seat(&server, "cc@stroppy-io", A, Seat::External);
    told(&a1, "the first line");

    for answer in [
        cc.read(&project_room(B)),
        cc.say("1", &project_room(B), "hi"),
    ] {
        match refused(answer) {
            Refused::NotYourRoom { room, own } => {
                assert_eq!(room, project_room(B));
                assert_eq!(own, project_room(A));
                let sentence = Refused::NotYourRoom { room, own }.to_string();
                assert!(sentence.contains(&project_room(B)), "{sentence}");
                assert!(sentence.contains(&project_room(A)), "{sentence}");
            }
            other => panic!("{other:?}"),
        }
    }
    // Its own project's room and the host room both take it.
    said(cc.say("2", &project_room(A), "in a"));
    said(cc.say("3", HOST_ROOM, "@s-a1 in host"));
    assert_eq!(a1.next().0.room, project_room(A));
    assert_eq!(a1.next().0.room, HOST_ROOM);
    // A scope ask with no room lands where a session's does: the seat's own project room.
    match cc.call(Request::Ask {
        id: "4".into(),
        room: String::new(),
        to: String::new(),
        path: String::new(),
        topic: "wire".into(),
        question: "which version?".into(),
    }) {
        Response::Asked { .. } | Response::Said { .. } => {}
        other => panic!("{other:?}"),
    }
    assert_eq!(a1.next().0.room, project_room(A), "the question is in a");
    server.stop();
}

fn told(s: &Seated, text: &str) -> Message {
    said(s.call(Request::Say {
        id: format!("t-{text}"),
        room: String::new(),
        text: text.into(),
        to: None,
        thread: None,
        refs: Vec::new(),
        attachments: Vec::new(),
        kind: Default::default(),
    }))
}

/// A dm is between two seats, not two rooms: the bridge in `etcd` reaches `serenedb`'s session, and
/// the two talk where nothing of the room they are in is written.
#[test]
fn a_dm_across_projects_is_delivered() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let b1 = seat(&server, "s-b1", B, Seat::Session);
    let cc = seat(&server, "cc@stroppy-io", A, Seat::External);

    let dm = said(cc.call(Request::Dm {
        id: "1".into(),
        to: "s-b1".into(),
        text: "about the pin in serenedb".into(),
        thread: None,
        attachments: Vec::new(),
    }));
    assert_eq!(dm.room, "dm");
    assert_eq!(dm.to.as_deref(), Some("s-b1"));
    let (heard, names) = b1.next_seq(dm.seq);
    assert!(names, "a dm names the one it is for");
    assert_eq!(heard.text, "about the pin in serenedb");

    // And the answer crosses the same way, back to the bridge.
    let back = said(b1.call(Request::Dm {
        id: "2".into(),
        to: "cc@stroppy-io".into(),
        text: "it is fine".into(),
        thread: None,
        attachments: Vec::new(),
    }));
    assert_eq!(cc.next_seq(back.seq).0.text, "it is fine");

    // Nothing of either is in the other's room.
    match cc.read(&project_room(A)) {
        Response::History(h) => assert!(h.rows.iter().all(|m| m.room != "dm"), "{:?}", h.rows),
        other => panic!("{other:?}"),
    }
    server.stop();
}
