//! A dm's addressee: what the log stores, who is pushed, and what a name nobody answers to is
//! refused with.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use letibot_room::client::Client;
use letibot_room::server::Server;
use letibot_room::{
    Event, Group, Member, Refused, Request, Response, Room, Seat, Speaker, room_name,
};

mod support;

use support::wait_until;

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
            kind: Default::default(),
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

/// `to` allows `from` across projects: the ask the room raised is answered `allow for the
/// session`, which is the consent the next dm reads.
fn consent_to(from: &Seated, to: &Seated, id: &str) {
    let r = refused(from.dm("0", id, "may I write to you?"));
    let Refused::DmNeedsConsent { ask, .. } = r else {
        panic!("a dm across projects must ask: {r:?}");
    };
    let answered = to.call(Request::Reply {
        id: "consent".into(),
        ask,
        text: "allow for the session".into(),
    });
    assert!(
        matches!(answered, Response::Asked { .. }),
        "the consent settles: {answered:?}"
    );
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
    // The box's half of every id, pinned: a refusal names the id a name meant.
    server.with_seat_host("lab2x1");
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

/// A session's dm to a Claude Code seat whose name is the handle of a session in another project
/// is refused, naming the id the name means, and the id itself still reaches it.
#[test]
fn the_1038_shape_a_handle_from_another_project_is_refused_and_not_re_addressed() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let me = seat(&server, "s-dbfuzz", A, Seat::Session);
    let arango = seat(&server, "s-arango", B, Seat::Session);
    tell(&me, vec![top("s-dbfuzz", "db-fuzz")]);
    tell(&arango, vec![top("s-arango", "serenedb")]);

    // No bridge by that name is connected, and the bare name is the arango session's handle - which
    // is a label, so it is refused with the id behind it before a project is even compared.
    for (id, to) in [("1", "serenedb"), ("2", "cc@serenedb")] {
        let why = refused(me.dm(id, to, "cc@serenedb: two questions"));
        match (to, why) {
            ("serenedb", Refused::NameIsLabel { name, id }) => {
                assert_eq!(name, "serenedb");
                assert_eq!(id, "lab2x1/s-arango");
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

    // The id is how a session reaches another project's session, and it is what the refusal said -
    // with that seat's consent, which is the one thing the id does not stand in for.
    consent_to(&me, &arango, "s-arango");
    let before = logged(&dir).len();
    said(me.dm("3", "s-arango", "cc@serenedb: two questions"));
    assert_eq!(
        logged(&dir).len(),
        before + 1,
        "naming the id, with consent, reaches it"
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

    // A dm across projects asks first, and this file is about the thread: the consent is given up
    // front, by the addressee, as it would be at a head.
    consent_to(&me, &arango, "s-arango");
    let root = said(me.dm("1", "s-arango", "about the pin")).seq;
    match refused(me.say("2", Some("cc@serenedb"), "cc@serenedb: look", Some(root))) {
        Refused::ThreadIsAnotherDm { thread, with } => {
            assert_eq!((thread, with.as_str()), (root, "s-arango"))
        }
        other => panic!("{other:?}"),
    }
    // Answering the thread's own party is the same conversation, so it is allowed - and the
    // sender reads back the handle of who it went to.
    let before = logged(&dir).len();
    let back = said(me.say("3", Some("s-arango"), "the pin is fine", Some(root)));
    assert_eq!(back.to.as_deref(), Some("arango"), "the sender is told");
    assert_eq!(
        logged(&dir).len(),
        before + 1,
        "the answer is one new row: {:?}",
        logged(&dir)
    );
    assert_eq!(
        logged(&dir).last().map(|m| m.3),
        Some(Some(root)),
        "and it rides the thread"
    );
}

/// A second bridge in one folder, spoken by hand: it lost the race for `family`, took a suffixed
/// name, and says so in its `hello` - the bridge's own wire, JSON lines.
struct Raw {
    w: std::os::unix::net::UnixStream,
    chat: Receiver<serde_json::Value>,
    replies: Receiver<serde_json::Value>,
}

fn raw_bridge(server: &Server, name: &str, family: &str) -> Raw {
    use std::io::{BufRead, BufReader, Write};
    let mut w = std::os::unix::net::UnixStream::connect(server.path()).expect("connect");
    let hello = serde_json::json!({
        "type": "hello", "protocol": letibot_room::ROOM_PROTOCOL_VERSION,
        "speaker": { "session": name, "parent": null },
        "room": room_name(&server.name(), ""), "seat": "external", "family": family,
    });
    writeln!(w, "{hello}").unwrap();
    let mut r = BufReader::new(w.try_clone().unwrap());
    let mut first = String::new();
    r.read_line(&mut first).unwrap();
    assert!(first.contains("\"welcome\""), "{first}");
    let (ctx, chat) = mpsc::channel();
    let (rtx, replies) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        while r.read_line(&mut line).unwrap_or(0) > 0 {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) {
                match v["type"].as_str() {
                    Some("chat") => drop(ctx.send(v)),
                    Some("event") => {}
                    _ => drop(rtx.send(v)),
                }
            }
            line.clear();
        }
    });
    Raw { w, chat, replies }
}

impl Raw {
    fn call(&mut self, frame: serde_json::Value) -> serde_json::Value {
        use std::io::Write;
        writeln!(self.w, "{frame}").unwrap();
        self.replies
            .recv_timeout(Duration::from_secs(5))
            .expect("a reply")
    }

    fn inbox(&mut self) -> Vec<u64> {
        let r = self.call(serde_json::json!({"type": "inbox", "attention": "to_me", "limit": 50}));
        r["rows"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|m| m["seq"].as_u64())
            .collect()
    }
}

fn inbox_of(s: &Seated) -> Vec<u64> {
    match s.call(Request::Inbox {
        attention: letibot_room::chat::Attention::ToMe,
        limit: Some(50),
        after: None,
    }) {
        Response::Inbox { rows, .. } => rows.iter().map(|m| m.seq).collect(),
        other => panic!("expected an inbox: {other:?}"),
    }
}

/// Two bridges of one name: `cc@letibot`, and `cc@letibot-8e0` that lost the race for it. A dm to
/// the name is pushed to both, each holds it under its own cursor, and one's ack hides nothing
/// from the other.
#[test]
fn a_dm_to_a_name_reaches_every_bridge_of_it_and_one_ack_hides_nothing() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let me = seat(&server, "s-rano", A, Seat::Session);
    tell(&me, vec![top("s-rano", "rano")]);
    let first = seat(&server, "cc@letibot", "", Seat::External);
    let mut second = raw_bridge(&server, "cc@letibot-8e0", "cc@letibot");

    let dm = said(me.dm("1", "cc@letibot", "is H6 gated?"));
    assert_eq!(dm.to.as_deref(), Some("cc@letibot"));
    let (heard, named) = first.next();
    assert!(heard.seq == dm.seq && named);
    let pushed = second
        .chat
        .recv_timeout(Duration::from_secs(5))
        .expect("the second bridge is pushed the dm too");
    assert_eq!(pushed["message"]["seq"], dm.seq);
    assert_eq!(pushed["names_you"], true, "it is addressed to this bridge");

    assert_eq!(inbox_of(&first), vec![dm.seq]);
    assert_eq!(second.inbox(), vec![dm.seq]);
    first.call(Request::Ack { seq: dm.seq });
    assert!(inbox_of(&first).is_empty());
    assert_eq!(
        second.inbox(),
        vec![dm.seq],
        "the first one's ack is its own"
    );
    second.call(serde_json::json!({"type": "ack", "seq": dm.seq}));
    assert!(second.inbox().is_empty());

    // And a name the first holds no more still reaches the one left. The room has to have read the
    // close first: three connections in, two after it.
    drop(first);
    wait_until("the room to forget the closed bridge", || {
        server.connections() == 2
    });
    let again = said(me.dm("2", "cc@letibot", "still there?"));
    let pushed = second
        .chat
        .recv_timeout(Duration::from_secs(5))
        .expect("the suffixed bridge reads the family's mail alone");
    assert_eq!(pushed["message"]["seq"], again.seq);
}

/// A family is only the name's own prefix: a bridge cannot declare somebody else's mail its own.
#[test]
fn a_family_that_is_not_the_names_prefix_reads_nothing() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let me = seat(&server, "s-rano", A, Seat::Session);
    tell(&me, vec![top("s-rano", "rano")]);
    let _owner = seat(&server, "cc@serenedb", "", Seat::External);
    let mut thief = raw_bridge(&server, "cc@letibot-1", "cc@serenedb");
    said(me.dm("1", "cc@serenedb", "private"));
    assert!(
        thief.chat.recv_timeout(Duration::from_millis(300)).is_err(),
        "nothing was pushed"
    );
    assert!(thief.inbox().is_empty());
}

/// A dm across PROJECTS needs the addressee's consent, and by SESSION ID too - which is the door
/// the operator found open. Same project is free, and the answer is the consent: `allow for the
/// session` lets the next one through, and a refusal leaves the pair where it was.
#[test]
fn a_dm_across_projects_asks_the_addressee_and_needs_its_yes() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let me = seat(&server, "s-mine", A, Seat::Session);
    let them = seat(&server, "s-theirs", B, Seat::Session);
    tell(&me, vec![top("s-mine", "mine")]);
    tell(&them, vec![top("s-theirs", "theirs")]);

    // The id form, which used to cross unasked.
    let refusal = refused(me.dm("1", "s-theirs", "a word in private"));
    assert!(
        matches!(refusal, Refused::DmNeedsConsent { .. }),
        "{refusal:?}"
    );
    let Refused::DmNeedsConsent { ask, .. } = refusal else {
        unreachable!()
    };
    // What the addressee is pushed is the CONSENT REQUEST, not the message: the dm is not
    // delivered, and the log's one row is the ask's own question.
    let (heard, _) = them.next();
    assert!(
        heard.text.contains("asks to send you direct messages"),
        "the addressee gets the ask, not the dm: {}",
        heard.text
    );
    assert!(!heard.text.contains("a word in private"), "{}", heard.text);

    // The ask is on the addressee's board, and its answer is the consent.
    let asks = them.call(Request::Asks);
    let Response::Asks { of_me, .. } = asks else {
        panic!("the asks are a listing: {asks:?}");
    };
    assert!(
        of_me.iter().any(|a| a.seq == ask),
        "the consent request is a row of the addressee's: {of_me:?}"
    );
    let answered = them.call(Request::Reply {
        id: "r1".into(),
        ask,
        text: "allow for the session".into(),
    });
    assert!(
        matches!(answered, Response::Asked { .. }),
        "the ask settles: {answered:?}"
    );

    // Now it goes through, and says who it went to.
    let dm = said(me.dm("2", "s-theirs", "a word in private"));
    assert_eq!(
        dm.to.as_deref(),
        Some("theirs"),
        "the sender is told the addressee by its handle"
    );
    let (heard, _) = them.next();
    assert_eq!(heard.text, "a word in private");

    // A different pair is still asked: one consent is one pair.
    let other = seat(&server, "s-third", "cccccccccccc", Seat::Session);
    tell(&other, vec![top("s-third", "third")]);
    let again = me.dm("3", "s-third", "hello");
    assert!(
        matches!(refused(again), Refused::DmNeedsConsent { .. }),
        "a new project is a new ask"
    );
}

/// The consent is the ROOM's state, not the process's memory: a coordinator that restarts keeps the
/// pair it was told, and the seats do not have to ask again.
#[test]
fn a_consent_survives_a_coordinator_restart() {
    let dir = Scratch::new();
    let sock = dir.0.join("room.sock");
    {
        let server = coordinator(&dir);
        let me = seat(&server, "s-mine", A, Seat::Session);
        let them = seat(&server, "s-theirs", B, Seat::Session);
        tell(&me, vec![top("s-mine", "mine")]);
        tell(&them, vec![top("s-theirs", "theirs")]);
        consent_to(&me, &them, "s-theirs");
        // And it holds while this coordinator is up.
        let dm = said(me.dm("2", "s-theirs", "one"));
        assert_eq!(dm.to.as_deref(), Some("theirs"));
        server.stop();
        support::wait_socket_gone(&sock);
    }

    let server = coordinator(&dir);
    let me = seat(&server, "s-mine", A, Seat::Session);
    let them = seat(&server, "s-theirs", B, Seat::Session);
    tell(&me, vec![top("s-mine", "mine")]);
    tell(&them, vec![top("s-theirs", "theirs")]);
    // Delivered, with no ask: the room wrote the consent down and read it back.
    let dm = said(me.dm("3", "s-theirs", "two"));
    assert_eq!(
        dm.to.as_deref(),
        Some("theirs"),
        "the pair survived the restart"
    );
}

/// "Allow for the session" is a sentence about a session, so it ends when one of the two seats
/// does: the pair is asked again.
#[test]
fn a_session_consent_ends_with_the_session() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let me = seat(&server, "s-mine", A, Seat::Session);
    let them = seat(&server, "s-theirs", B, Seat::Session);
    tell(&me, vec![top("s-mine", "mine")]);
    tell(&them, vec![top("s-theirs", "theirs")]);
    consent_to(&me, &them, "s-theirs");
    assert_eq!(
        said(me.dm("2", "s-theirs", "one")).to.as_deref(),
        Some("theirs")
    );

    // The addressee's session is DELETED, which the daemon tells the room.
    let retired = them.call(Request::Retire {
        id: "retire".into(),
        session: "s-theirs".into(),
    });
    assert!(
        matches!(retired, Response::Asks { .. }),
        "a retire answers with the asks it ended: {retired:?}"
    );
    let again = me.dm("3", "s-theirs", "two");
    assert!(
        matches!(refused(again), Refused::DmNeedsConsent { .. }),
        "a consent for the session goes with the session"
    );
}
