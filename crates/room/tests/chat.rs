//! Chat on a real coordinator socket: two projects and the host room on one server, sessions of each,
//! the operator and an external. Who sees what, who is told, and who is refused.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use letibot_room::client::Client;
use letibot_room::server::Server;
use letibot_room::{
    Attention, Event, HOST_ROOM, Message, OPERATOR, Refused, Request, Response, Room, Seat,
    Speaker, project_room, room_name,
};

const A: &str = "aaaaaaaaaaaa";
const B: &str = "bbbbbbbbbbbb";

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Scratch {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "letibot-room-chat-{}-{}",
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

    fn say(&self, id: &str, room: &str, text: &str) -> Response {
        self.call(Request::Say {
            id: id.into(),
            room: room.into(),
            text: text.into(),
            to: None,
            thread: None,
            refs: Vec::new(),
        })
    }

    fn next(&self) -> (Message, bool) {
        self.heard
            .recv_timeout(Duration::from_secs(5))
            .expect("a pushed message")
    }

    fn silent(&self) {
        assert!(
            self.heard.recv_timeout(Duration::from_millis(300)).is_err(),
            "nothing was pushed"
        );
    }

    fn rooms(&self) -> Vec<String> {
        match self.call(Request::Rooms) {
            Response::Rooms { rows } => rows.into_iter().map(|r| r.id).collect(),
            other => panic!("{other:?}"),
        }
    }
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

#[test]
fn two_projects_and_the_host_room_on_one_coordinator() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let a1 = seat(&server, "s-a1", A, Seat::Session);
    let b1 = seat(&server, "s-b1", B, Seat::Session);
    let op = seat(&server, OPERATOR, "", Seat::Operator);

    // Each session lists the host room and its own project's, and never the other's.
    assert_eq!(a1.rooms(), vec![HOST_ROOM.to_string(), project_room(A)]);
    assert_eq!(b1.rooms(), vec![HOST_ROOM.to_string(), project_room(B)]);
    // The operator lists all three.
    let mut all = op.rooms();
    all.sort();
    assert_eq!(
        all,
        vec![HOST_ROOM.to_string(), project_room(A), project_room(B)]
    );

    // Unnamed room means the project's own: B never hears A's, the operator does.
    let m = said(a1.say("1", "", "project a only"));
    assert_eq!(m.room, project_room(A));
    assert_eq!(op.next().0.text, "project a only");
    b1.silent();
    assert!(matches!(
        refused(b1.call(Request::Read {
            room: project_room(A),
            since: 0,
            last: None,
            thread: None
        })),
        Refused::UnknownChatRoom { .. }
    ));
    assert!(matches!(
        refused(a1.say("2", &project_room(B), "reach")),
        Refused::UnknownChatRoom { .. }
    ));

    // A host message reaches both projects and the operator, but not its author.
    said(a1.say("3", HOST_ROOM, "to the host"));
    assert_eq!(b1.next().0.text, "to the host");
    assert_eq!(op.next().0.text, "to the host");
    a1.silent();

    // The operator posts in all three rooms.
    for room in [HOST_ROOM.to_string(), project_room(A), project_room(B)] {
        let m = said(op.say("op", &room, &format!("op in {room}")));
        assert_eq!(m.author, OPERATOR);
        assert_eq!(m.kind, Seat::Operator);
    }
    assert_eq!(a1.next().0.room, HOST_ROOM);
    assert_eq!(a1.next().0.room, project_room(A));
    a1.silent();
    assert_eq!(b1.next().0.room, HOST_ROOM);
    assert_eq!(b1.next().0.room, project_room(B));
    b1.silent();
}

#[test]
fn nobody_connects_as_the_operator_without_the_seat_and_the_seat_needs_the_name() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let wake = Arc::new(|_: &Event| {});
    let named = |who: &str, project: &str, seat| {
        Client::connect_as(
            server.path(),
            Speaker::top_level(who),
            room_name(&server.name(), project),
            seat,
            wake.clone(),
        )
    };
    assert!(named(OPERATOR, A, Seat::Session).is_err());
    assert!(named("s-sneaky", "", Seat::Operator).is_err());
    assert!(named("s-sneaky", "", Seat::External).is_err());
    assert!(named("claude-code", "", Seat::External).is_ok());
    assert!(named("operator", "", Seat::External).is_err());
}

#[test]
fn dms_mentions_threads_and_the_inbox() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let a1 = seat(&server, "s-a1", A, Seat::Session);
    let a2 = seat(&server, "s-a2", A, Seat::Session);
    let b1 = seat(&server, "s-b1", B, Seat::Session);
    let cc = seat(&server, "claude-code", "", Seat::External);

    // A mention names its reader in the push; a bystander is not named.
    said(a1.say("1", "", "@s-a2 look at this"));
    assert!(a2.next().1, "named");
    let root = said(a1.say("2", "", "plain"));
    assert!(!a2.next().1);

    // A reply hangs from its root, and a read by thread returns both.
    let reply = said(a2.call(Request::Say {
        id: "3".into(),
        room: String::new(),
        text: "answer".into(),
        to: Some("s-a1".into()),
        thread: Some(root.seq),
        refs: Vec::new(),
    }));
    assert_eq!(reply.thread, Some(root.seq));
    match a1.call(Request::Read {
        room: String::new(),
        since: 0,
        last: None,
        thread: Some(root.seq),
    }) {
        Response::History(h) => assert_eq!(
            h.rows.iter().map(|m| m.seq).collect::<Vec<_>>(),
            vec![root.seq, reply.seq]
        ),
        other => panic!("{other:?}"),
    }

    // A DM goes to its addressee only, and only they and the author can read it.
    let dm = said(a1.call(Request::Dm {
        id: "4".into(),
        to: "claude-code".into(),
        text: "private".into(),
        thread: None,
    }));
    assert_eq!(dm.room, "dm");
    assert_eq!(cc.next().0.text, "@s-a2 look at this");
    let mut texts = Vec::new();
    while let Ok((m, _)) = cc.heard.recv_timeout(Duration::from_millis(300)) {
        texts.push(m.text);
    }
    assert!(texts.contains(&"private".to_string()), "{texts:?}");
    for outsider in [&a2, &b1] {
        match outsider.call(Request::Read {
            room: "dm".into(),
            since: 0,
            last: None,
            thread: None,
        }) {
            Response::History(h) => assert!(h.rows.is_empty(), "{:?}", h.rows),
            other => panic!("{other:?}"),
        }
    }
    // A name nobody answers to is refused, not delivered to no one.
    assert!(matches!(
        refused(a1.call(Request::Dm {
            id: "5".into(),
            to: "nobody-here".into(),
            text: "x".into(),
            thread: None
        })),
        Refused::UnknownAddressee { .. }
    ));

    // The inbox replays what a reader has not acknowledged, counts what the filter left out, and
    // an ack moves the cursor.
    match a2.call(Request::Inbox {
        attention: Attention::Mentions,
        limit: None,
    }) {
        Response::Inbox {
            rows,
            skipped,
            cursor,
            head,
        } => {
            assert_eq!(rows.len(), 1, "only the mention: {rows:?}");
            assert!(skipped >= 1);
            assert_eq!(cursor, 0, "no ack yet");
            assert!(head >= 4);
            assert!(matches!(
                a2.call(Request::Ack { seq: head }),
                Response::Acked { cursor } if cursor == head
            ));
        }
        other => panic!("{other:?}"),
    }
    match a2.call(Request::Inbox {
        attention: Attention::All,
        limit: None,
    }) {
        Response::Inbox { rows, .. } => assert!(rows.is_empty()),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_daemon_reports_facts_and_nobody_else_may() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let a1 = seat(&server, "s-a1", A, Seat::Session);
    let op = seat(&server, OPERATOR, "", Seat::Operator);
    let notify = |who: &Seated, source: &str| {
        who.call(Request::Notify {
            id: "n1".into(),
            room: String::new(),
            source: source.into(),
            text: "filed t-7".into(),
            refs: vec![letibot_room::Ref {
                kind: "todo".into(),
                id: "t-7".into(),
            }],
        })
    };
    let m = said(notify(&a1, "todo"));
    assert_eq!((m.kind, m.author.as_str()), (Seat::System, "todo"));
    assert_eq!(m.room, project_room(A));
    let (heard, _) = op.next();
    assert_eq!(heard.refs[0].id, "t-7");
    assert!(matches!(
        refused(notify(&op, "todo")),
        Refused::ReservedName { .. }
    ));
    assert!(matches!(
        refused(notify(&a1, "operator")),
        Refused::ReservedName { .. }
    ));
}
