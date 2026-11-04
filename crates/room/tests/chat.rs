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

fn stored(s: &Scratch) -> Server {
    let server = coordinator(s);
    server.with_chat_store(&s.0.join("room.chat"));
    server
}

fn instance_of(c: &Client) -> u64 {
    c.welcome().instance
}

#[test]
fn chat_history_seq_cursor_and_dedup_survive_a_coordinator_restart() {
    let dir = Scratch::new();
    let first = stored(&dir);
    let a = seat(&first, "s-a1", A, Seat::Session);
    let cc = seat(&first, "claude-code", "", Seat::External);
    let before = instance_of(&a.client);
    assert_ne!(before, 0);
    let one = said(a.say("r1", "", "before the restart"));
    let two = said(a.say("r2", "", "second"));
    // cc reads its inbox and acks the first only.
    cc.call(Request::Ack { seq: one.seq });
    first.stop();
    drop((a, cc));
    std::thread::sleep(Duration::from_millis(20));

    let second = stored(&dir);
    let a = seat(&second, "s-a1", A, Seat::Session);
    let cc = seat(&second, "claude-code", "", Seat::External);
    assert_ne!(
        instance_of(&a.client),
        before,
        "a restart changes the instance"
    );
    match a.call(Request::Read {
        room: String::new(),
        since: 0,
        last: None,
        thread: None,
    }) {
        Response::History(h) => assert_eq!(
            h.rows.iter().map(|m| m.text.as_str()).collect::<Vec<_>>(),
            vec!["before the restart", "second"]
        ),
        other => panic!("{other:?}"),
    }
    // A repeated request id is still one message, and seq continues.
    assert_eq!(said(a.say("r2", "", "second")).seq, two.seq);
    let three = said(a.say("r3", "", "after"));
    assert_eq!(three.seq, two.seq + 1);
    match cc.call(Request::Inbox {
        attention: Attention::All,
        limit: Some(10),
    }) {
        Response::Inbox { rows, .. } => assert_eq!(
            rows.iter().map(|m| m.seq).collect::<Vec<_>>(),
            vec![two.seq, three.seq],
            "the cursor was kept"
        ),
        other => panic!("{other:?}"),
    }
    second.stop();
}

#[test]
fn a_restart_without_a_store_starts_chat_over_and_changes_the_instance() {
    let dir = Scratch::new();
    let first = coordinator(&dir);
    let a = seat(&first, "s-a1", A, Seat::Session);
    let id = instance_of(&a.client);
    said(a.say("r1", "", "gone"));
    first.stop();
    drop(a);
    std::thread::sleep(Duration::from_millis(20));
    let second = coordinator(&dir);
    let a = seat(&second, "s-a1", A, Seat::Session);
    assert_eq!(second.chat_head(), 0);
    assert_ne!(instance_of(&a.client), id);
    second.stop();
}

#[test]
fn a_linked_client_redials_a_restarted_coordinator_and_hears_what_follows() {
    let dir = Scratch::new();
    let first = stored(&dir);
    let a = seat(&first, "s-a1", A, Seat::Session);
    let before = instance_of(&a.client);
    first.stop();
    std::thread::sleep(Duration::from_millis(50));
    let second = stored(&dir);
    // No request from `a`: the redial alone brings the connection back.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !(a.client.connected() && instance_of(&a.client) != before) {
        assert!(std::time::Instant::now() < deadline, "never redialled");
        std::thread::sleep(Duration::from_millis(50));
    }
    let b = seat(&second, "s-a2", A, Seat::Session);
    said(b.say("r1", "", "@s-a1 after the restart"));
    let (m, named) = a.next();
    assert_eq!(m.text, "@s-a1 after the restart");
    assert!(named);
    // And a request on the redialled connection works.
    said(a.say("r2", "", "back"));
    second.stop();
}

/// Wait until `f` says so, polling - for a fact that arrives on a thread (a connection served, a
/// close noticed) rather than as a frame.
fn until(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("waited five seconds for {what}");
}

/// A message to a session that is not connected is neither pushed nor lost: the room keeps it and
/// pushes it when that session connects, and its inbox cursor counts it unread in the meantime.
#[test]
fn a_message_to_a_session_that_is_away_is_delivered_when_it_connects() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    // It connects once, so the room has a cursor for it, and then goes.
    let away = seat(&server, "s-away", A, Seat::Session);
    drop(away);
    until("the away session's connection to close", || {
        server.connections() == 0
    });
    let here = seat(&server, "s-here", A, Seat::Session);
    let m = said(here.say("1", "", "@s-away: the disk is free now"));
    // It comes back, and the message it was not connected for arrives without it asking.
    let back = seat(&server, "s-away", A, Seat::Session);
    let (got, names) = back.next();
    assert_eq!(got.seq, m.seq, "the message it missed");
    assert!(names, "and it names the reader");
    // Counted while it was away: the cursor did not move, so the room's own read says one unread.
    match back.call(Request::Rooms) {
        Response::Rooms { rows } => {
            let project = rows
                .iter()
                .find(|r| r.id == project_room(A))
                .unwrap_or_else(|| panic!("its own room: {rows:?}"));
            assert_eq!(project.unread, 1, "counted as unread until it acks");
        }
        other => panic!("{other:?}"),
    }
    // And an inbox read has it too: the push is a convenience, the record is the log.
    match back.call(Request::Inbox {
        attention: Attention::All,
        limit: None,
    }) {
        Response::Inbox { rows, .. } => {
            assert!(rows.iter().any(|r| r.seq == m.seq), "{rows:?}");
        }
        other => panic!("{other:?}"),
    }
}

/// The inbox one absent member can grow is bounded, and what does not fit is a push that was not
/// made - the ring still has every message, so the oldest is dropped from the PUSH, never lost.
#[test]
fn an_absent_members_inbox_is_bounded_and_the_ring_still_has_the_rest() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let away = seat(&server, "s-away", A, Seat::Session);
    drop(away);
    until("the away session's connection to close", || {
        server.connections() == 0
    });
    let here = seat(&server, "s-here", A, Seat::Session);
    let bound = letibot_room::server::MAX_PENDING;
    let sent = bound + 10;
    for i in 0..sent {
        said(here.say(&format!("m{i}"), "", &format!("@s-away line {i}")));
    }
    let back = seat(&server, "s-away", A, Seat::Session);
    let mut pushed = Vec::new();
    while pushed.len() < sent {
        match back.heard.recv_timeout(Duration::from_millis(200)) {
            Ok((m, _)) => pushed.push(m.seq),
            Err(_) => break,
        }
    }
    assert_eq!(pushed.len(), bound, "the newest {bound} are pushed");
    assert!(
        pushed.windows(2).all(|w| w[0] < w[1]),
        "in the order they were said"
    );
    // Nothing was cut from the record: the oldest of them is still readable.
    match back.call(Request::Inbox {
        attention: Attention::All,
        limit: None,
    }) {
        Response::Inbox { rows, .. } => {
            assert_eq!(rows.len(), sent, "the ring still has all {sent}");
        }
        other => panic!("{other:?}"),
    }
}
