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
            thread: None,
            before: None
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
        before: None,
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
            before: None,
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
        before: None,
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
            Ok((m, _)) if m.kind != Seat::System => pushed.push(m.seq),
            Ok(_) => {}
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

/// Drain what a seat is pushed until it goes quiet.
fn drained(s: &Seated) -> Vec<Message> {
    let mut out = Vec::new();
    while let Ok((m, _)) = s.heard.recv_timeout(Duration::from_millis(300)) {
        out.push(m);
    }
    out
}

/// A member joining a busy room is told what was said as one digest, a mention to it stays
/// verbatim, and the history is still there to read.
#[test]
fn a_member_joining_a_busy_room_gets_a_digest_and_its_mention_verbatim() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let talker = seat(&server, "s-talker", A, Seat::Session);
    for i in 0..30 {
        said(talker.say(&format!("m{i}"), "", &format!("line {i}")));
    }
    said(talker.say("q", "", "who has the disk?"));
    said(talker.say("ping", "", "@s-new look at this"));

    let fresh = seat(&server, "s-new", A, Seat::Session);
    let got = drained(&fresh);
    let verbatim: Vec<&Message> = got.iter().filter(|m| m.kind != Seat::System).collect();
    let digests: Vec<&Message> = got.iter().filter(|m| m.kind == Seat::System).collect();
    assert_eq!(verbatim.len(), 1, "{got:?}");
    assert_eq!(verbatim[0].text, "@s-new look at this");
    assert_eq!(digests.len(), 1, "one digest for the room: {got:?}");
    let text = &digests[0].text;
    assert!(text.contains("s-talker 31"), "{text}");
    assert!(text.contains("unanswered: #31"), "{text}");
    assert!(text.contains("more, `room read"), "{text}");
    assert!(text.matches("\n  #").count() <= 3, "{text}");
    match fresh.call(Request::Read {
        room: String::new(),
        since: 0,
        last: None,
        thread: None,
        before: None,
    }) {
        Response::History(h) => assert_eq!(h.rows.len(), 32, "full history on demand"),
        other => panic!("{other:?}"),
    }
}

/// After a coordinator restart the reconnecting member is told a digest of what it missed.
#[test]
fn a_member_reconnecting_after_a_restart_gets_a_digest_of_what_it_missed() {
    let dir = Scratch::new();
    let first = stored(&dir);
    let away = seat(&first, "s-away", A, Seat::Session);
    let talker = seat(&first, "s-talker", A, Seat::Session);
    said(talker.say("a", "", "while connected"));
    drop(away);
    until("the connection to close", || first.connections() == 1);
    for i in 0..10 {
        said(talker.say(&format!("m{i}"), "", &format!("missed {i}")));
    }
    first.stop();
    drop(talker);
    std::thread::sleep(Duration::from_millis(50));

    let second = stored(&dir);
    let back = seat(&second, "s-away", A, Seat::Session);
    let got = drained(&back);
    assert_eq!(got.len(), 1, "one digest, not the messages: {got:?}");
    assert_eq!(got[0].kind, Seat::System);
    assert!(got[0].text.contains("said since"), "{}", got[0].text);
    assert!(got[0].text.contains("s-talker 10"), "{}", got[0].text);
    second.stop();
}

#[test]
fn a_short_bridge_name_is_the_one_connected_bridge_or_ambiguous() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let a1 = seat(&server, "s-a1", A, Seat::Session);
    let one = seat(&server, "cc@app", "", Seat::External);

    // One bridge: `claude-code` in `to` and `@cc` in the text both reach it, by its full name.
    let m = said(a1.call(Request::Say {
        id: "1".into(),
        room: "host".into(),
        text: "@cc look".into(),
        to: Some("claude-code".into()),
        thread: None,
        refs: Vec::new(),
    }));
    assert_eq!(m.to.as_deref(), Some("cc@app"));
    assert_eq!(m.text, "@cc@app look");
    let (heard, named) = one.next();
    assert_eq!(heard.seq, m.seq);
    assert!(named, "the mention names the bridge");

    // Two bridges: the short name is refused and lists both.
    let two = seat(&server, "cc@web", "", Seat::External);
    match refused(a1.say("2", "host", "@claude-code hi")) {
        Refused::Ambiguous { candidates, .. } => {
            assert_eq!(candidates, vec!["cc@app".to_string(), "cc@web".to_string()])
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        refused(a1.call(Request::Dm {
            id: "3".into(),
            to: "cc".into(),
            text: "hi".into(),
            thread: None,
        })),
        Refused::Ambiguous { .. }
    ));
    // The full name still works, and a name already held is refused.
    said(a1.call(Request::Dm {
        id: "4".into(),
        to: "cc@web".into(),
        text: "hi".into(),
        thread: None,
    }));
    drop(two);
    let again = Client::connect_as(
        server.path(),
        Speaker::top_level("cc@app"),
        room_name(&server.name(), ""),
        Seat::External,
        Arc::new(|_: &Event| {}),
    );
    assert!(
        again
            .err()
            .is_some_and(|e| e.to_string().contains("already speaks")),
        "a second `cc@app` is refused"
    );
}

fn roster_names(who: &Seated) -> Vec<String> {
    match who.call(Request::Roster {
        room: "host".into(),
    }) {
        Response::Roster { rows, .. } => rows
            .into_iter()
            .filter(|r| r.kind == Seat::External)
            .map(|r| r.name)
            .collect(),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_short_bridge_name_resolves_when_unique_and_both_show_qualified_when_two_share_it() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let a1 = seat(&server, "s-a1", A, Seat::Session);
    let one = seat(
        &server,
        "cc@stroppy-io:stroppy-conformity",
        "",
        Seat::External,
    );

    let m = said(a1.say("1", "host", "@cc@stroppy-conformity hi"));
    assert_eq!(
        m.text, "@cc@stroppy-conformity hi",
        "the short form is shown"
    );
    let (heard, named) = one.next();
    assert_eq!(heard.text, "@cc@stroppy-conformity hi");
    assert!(named, "the short form reached the bridge");
    assert_eq!(roster_names(&a1), vec!["cc@stroppy-conformity".to_string()]);

    let two = seat(&server, "cc@other:stroppy-conformity", "", Seat::External);
    match refused(a1.say("2", "host", "@cc@stroppy-conformity hi")) {
        Refused::Ambiguous { candidates, .. } => assert_eq!(
            candidates,
            vec![
                "cc@other:stroppy-conformity".to_string(),
                "cc@stroppy-io:stroppy-conformity".to_string()
            ]
        ),
        other => panic!("{other:?}"),
    }
    let mut shown = roster_names(&a1);
    shown.sort();
    assert_eq!(
        shown,
        vec![
            "cc@other:stroppy-conformity".to_string(),
            "cc@stroppy-io:stroppy-conformity".to_string()
        ]
    );
    let q = said(a1.say("3", "host", "@cc@other:stroppy-conformity hi"));
    assert_eq!(q.text, "@cc@other:stroppy-conformity hi");
    let (heard, named) = one.next();
    assert_eq!(heard.seq, q.seq);
    assert!(!named, "the other bridge's qualified name is not this one");

    drop(two);
    until("the second bridge to leave", || {
        roster_names(&a1).len() == 1
    });
    assert_eq!(roster_names(&a1), vec!["cc@stroppy-conformity".to_string()]);
    let again = said(a1.say("4", "host", "@cc@stroppy-conformity hi"));
    assert_eq!(again.text, "@cc@stroppy-conformity hi");
}

#[test]
fn a_folder_mention_wakes_every_bridge_of_the_folder_and_an_exact_single_stays_single() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let a1 = seat(&server, "s-a1", A, Seat::Session);
    let g1 = seat(&server, "cc@stroppy-io:alpha", "", Seat::External);
    let g2 = seat(&server, "cc@stroppy-io:beta", "", Seat::External);
    let g3 = seat(&server, "cc@elsewhere:gamma", "", Seat::External);

    let m = said(a1.say("1", "host", "@cc@stroppy-io go"));
    assert_eq!(m.text, "@cc@stroppy-io:* go");
    assert!(g1.next().1, "alpha is addressed");
    assert!(g2.next().1, "beta is addressed");
    assert!(!g3.next().1, "a bridge of another folder is not");

    let solo = seat(&server, "cc@solo", "", Seat::External);
    let m = said(a1.say("2", "host", "@cc@solo hi"));
    assert_eq!(m.text, "@cc@solo hi", "the exact handle stays single");
    assert!(solo.next().1);
    assert!(!g1.next().1);
    let _ = (g2.next(), g3.next());

    let sibling = seat(&server, "cc@solo:other", "", Seat::External);
    let m = said(a1.say("3", "host", "@cc@solo hi"));
    assert_eq!(
        m.text, "@cc@solo:* hi",
        "a shared folder makes the folder form a group"
    );
    assert!(solo.next().1);
    assert!(sibling.next().1);
    let m = said(a1.say("4", "host", "@cc@solo:solo only"));
    assert_eq!(
        m.text, "@cc@solo only",
        "the qualified collapsed form is the handle"
    );
    assert!(solo.next().1);
    assert!(!sibling.next().1);
}

#[test]
fn a_room_is_labelled_by_its_project_folder() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let root = dir.0.join("rano");
    std::fs::create_dir_all(&root).unwrap();
    let key = letibot_room::project_key(&root.display().to_string());
    let a1 = seat(&server, "s-a1", &key, Seat::Session);
    a1.call(Request::Members {
        rows: Vec::new(),
        root: root.display().to_string(),
    });
    let op = seat(&server, "cc@app", "", Seat::External);
    match op.call(Request::Rooms) {
        Response::Rooms { rows } => {
            let row = rows.iter().find(|r| r.id.starts_with("project/")).unwrap();
            assert_eq!(row.label, "rano");
        }
        other => panic!("{other:?}"),
    }
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

fn tell(s: &Seated, rows: Vec<letibot_room::Member>) {
    s.call(Request::Members {
        rows,
        root: String::new(),
    });
}

fn say_to(s: &Seated, id: &str, to: &str, text: &str) -> Response {
    s.call(Request::Say {
        id: id.into(),
        room: String::new(),
        text: text.into(),
        to: Some(to.into()),
        thread: None,
        refs: Vec::new(),
    })
}

/// A named session is spoken to, and spoken as, by its handle. The log keeps the id, so the id
/// still addresses it, and a rename changes how old lines read.
#[test]
fn a_named_session_is_addressed_and_authored_by_its_handle() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let named = seat(&server, "s-1", A, Seat::Session);
    let plain = seat(&server, "s-2", A, Seat::Session);
    tell(
        &named,
        vec![top("s-1", "stroppy-conformity"), top("s-2", "")],
    );

    // `to` and `@` by handle reach the session, and the line reads with the handle.
    let m = said(say_to(
        &plain,
        "1",
        "@stroppy-conformity",
        "hi @stroppy-conformity",
    ));
    assert_eq!(m.author, "s-2");
    assert_eq!(m.to.as_deref(), Some("stroppy-conformity"));
    let (heard, names_you) = named.next();
    assert!(names_you, "the handle resolved to the session: {heard:?}");
    assert_eq!(heard.text, "hi @stroppy-conformity");

    // The id still works.
    said(say_to(&plain, "2", "s-1", "by id"));
    let (heard, names_you) = named.next();
    assert!(names_you, "{heard:?}");
    assert_eq!(heard.to.as_deref(), Some("stroppy-conformity"));

    // The named session speaks under its handle.
    said(named.say("3", "", "reply"));
    let (heard, _) = plain.next();
    assert_eq!(heard.author, "stroppy-conformity");

    // The roster names it by handle and keeps the id.
    match plain.call(Request::Roster {
        room: String::new(),
    }) {
        Response::Roster { rows, .. } => {
            let p = rows.iter().find(|p| p.id == "s-1").expect("the member");
            assert_eq!(p.name, "stroppy-conformity");
            let q = rows.iter().find(|p| p.id == "s-2").expect("the other");
            assert_eq!(q.name, "s-2", "unnamed keeps its id");
        }
        other => panic!("{other:?}"),
    }

    // A rename changes how the same lines read, and the old handle stops resolving.
    tell(&named, vec![top("s-1", "renamed"), top("s-2", "")]);
    match plain.call(Request::Read {
        room: String::new(),
        since: 0,
        last: None,
        thread: None,
        before: None,
    }) {
        Response::History(h) => {
            assert!(
                h.rows.iter().any(|r| r.author == "renamed"),
                "old lines read with the new name: {:?}",
                h.rows
            );
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        refused(say_to(&plain, "4", "stroppy-conformity", "gone")),
        Refused::UnknownAddressee { .. }
    ));
    said(say_to(&plain, "5", "renamed", "new name"));
}

/// The same handle in two projects: the sender's own project wins, and a sender in neither is
/// refused with the ids.
#[test]
fn a_handle_in_two_projects_is_the_senders_own_or_ambiguous() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let a1 = seat(&server, "s-a1", A, Seat::Session);
    let b1 = seat(&server, "s-b1", B, Seat::Session);
    let op = seat(&server, "cc@app", "", Seat::External);
    tell(&a1, vec![top("s-a1", "same")]);
    tell(&b1, vec![top("s-b1", "same")]);

    let a2 = seat(&server, "s-a2", A, Seat::Session);
    said(say_to(&a2, "1", "same", "own project"));
    let (_, names_you) = a1.next();
    assert!(names_you, "the sender's own project holds the handle");
    b1.silent();
    match refused(say_to(&op, "2", "same", "which")) {
        Refused::Ambiguous { candidates, .. } => {
            assert_eq!(candidates, vec!["s-a1".to_string(), "s-b1".to_string()])
        }
        other => panic!("{other:?}"),
    }
}
