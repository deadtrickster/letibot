//! An ask by SCOPE on a real coordinator socket: a live lease, the last editor, an `OWNERS` line,
//! and the project room when nothing owns it - each with the reason the asker is told. Each rule is
//! counted, because the share of asks an owner answered is the measure R165 is judged by.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use letibot_room::client::Client;
use letibot_room::seat::{Edit, SeatFacts};
use letibot_room::server::Server;
use letibot_room::{
    AskView, Event, Group, Member, OPERATOR, Refused, Request, Response, Room, SayKind, Seat,
    Speaker, project_room, room_name,
};

const A: &str = "aaaaaaaaaaaa";

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Scratch {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "letibot-room-scope-{}-{}",
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

    /// The scope ask: a `path` or a `topic`, and never a session.
    fn ask_scope(&self, id: &str, path: &str, topic: &str, question: &str) -> Response {
        self.call(Request::Ask {
            id: id.into(),
            room: String::new(),
            to: String::new(),
            path: path.into(),
            topic: topic.into(),
            question: question.into(),
        })
    }

    fn own(&self, id: &str, scope: &str) {
        let owned = self.call(Request::Own {
            id: id.into(),
            scope: scope.into(),
            ttl_ms: Some(60 * 60 * 1000),
            thread: None,
        });
        assert!(matches!(owned, Response::Owned { .. }), "{owned:?}");
    }

    /// The edits its daemon would report: what renews a claim, and what the last-edit rule reads.
    fn edited(&self, paths: &[(&str, u64)]) {
        let facts = SeatFacts {
            edits: paths
                .iter()
                .map(|(path, at_ms)| Edit {
                    path: (*path).into(),
                    at_ms: *at_ms,
                })
                .collect(),
            ..SeatFacts::default()
        };
        self.call(Request::Seat { facts });
    }

    /// The tree its daemon sends, with the project's folder: where the room reads `OWNERS`.
    fn tree(&self, root: &std::path::Path) {
        let rows = vec![Member {
            session: "s-a".into(),
            label: "arango".into(),
            handle: "arango".into(),
            group: Group::Top,
            parent: None,
            depth: 0,
            live: true,
        }];
        self.call(Request::Members {
            rows,
            root: root.to_string_lossy().into_owned(),
        });
    }

    fn next(&self) -> letibot_room::Message {
        self.heard
            .recv_timeout(Duration::from_secs(5))
            .expect("a pushed message")
    }

    /// A room line in the project room, addressed to nobody.
    fn say(&self, id: &str, text: &str) -> Response {
        self.say_in(String::new(), id, text)
    }

    /// The same into a room the caller names: the operator's seat is wide and has no project.
    fn say_in(&self, room: String, id: &str, text: &str) -> Response {
        self.call(Request::Say {
            id: id.into(),
            room,
            text: text.into(),
            to: None,
            thread: None,
            refs: Vec::new(),
            attachments: Vec::new(),
            kind: SayKind::Post,
        })
    }
}

fn asked(r: Response) -> AskView {
    match r {
        Response::Asked { ask, .. } => ask,
        other => panic!("expected an ask: {other:?}"),
    }
}

fn refused(r: Response) -> Refused {
    match r {
        Response::Refused { why, .. } => why,
        other => panic!("expected a refusal: {other:?}"),
    }
}

fn coordinator(s: &Scratch) -> Server {
    Server::serve(&s.0.join("room.sock"), Room::new()).expect("a coordinator")
}

/// The count of one word in the `signals` frame.
fn count(server: &Server, word: &str) -> u64 {
    server.scorecard().counts.get(word).copied().unwrap_or(0)
}

/// A live lease is the first rule: the ask lands on the holder, and the asker is told which scope
/// decided it. A lease somewhere else names nobody.
#[test]
fn a_scope_ask_lands_on_the_lease_holder_and_says_which_lease() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let a = seat(&server, "s-a", A, Seat::Session);
    let b = seat(&server, "s-b", A, Seat::Session);
    let c = seat(&server, "s-c", A, Seat::Session);
    a.own("1", "crates/room/**");
    b.own("2", "crates/tui/**");

    let ask = asked(c.ask_scope(
        "q1",
        "crates/room/src/server.rs",
        "",
        "does the ask path route a scope?",
    ));
    assert_eq!(ask.asker, "s-c");
    assert_eq!(
        ask.target, "s-a",
        "the owner of the path, not of another one"
    );
    assert!(ask.open(), "{ask:?}");
    assert!(
        ask.why.contains("live lease on `crates/room/**`"),
        "{:?}",
        ask.why
    );
    // The target is woken by the question, and the question names it.
    let question = a.next();
    assert_eq!(question.seq, ask.seq);
    assert_eq!(question.to.as_deref(), Some("s-a"));
    // And every later read of the ask carries the same reason.
    let (by_me, _) = match c.call(Request::Asks) {
        Response::Asks { by_me, of_me } => (by_me, of_me),
        other => panic!("{other:?}"),
    };
    assert_eq!(by_me[0].why, ask.why);
    assert!(
        by_me[0].line().contains("because `s-a` holds a live lease"),
        "{}",
        by_me[0].line()
    );
    assert_eq!(count(&server, "ask_by_lease"), 1);
}

/// With no lease, the session that edited the path last in this project is asked - the room's own
/// working-on facts, not a store of its own.
#[test]
fn with_no_lease_the_last_editor_is_asked() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let a = seat(&server, "s-a", A, Seat::Session);
    let b = seat(&server, "s-b", A, Seat::Session);
    let c = seat(&server, "s-c", A, Seat::Session);
    a.edited(&[("crates/room/src/chat.rs", 1_000)]);
    b.edited(&[("crates/room/src/chat.rs", 2_000)]);

    let ask = asked(c.ask_scope(
        "q1",
        "crates/room/src/chat.rs",
        "",
        "where is the echo window?",
    ));
    assert_eq!(ask.target, "s-b", "the newest edit, not the oldest");
    assert!(
        ask.why.contains("edited `crates/room/src/chat.rs` last"),
        "{:?}",
        ask.why
    );
    assert_eq!(count(&server, "ask_by_edit"), 1);
}

/// An `OWNERS` file in the project's folder names the target of a path no lease and no edit covers.
#[test]
fn an_owners_line_names_the_target() {
    let dir = Scratch::new();
    let repo = dir.0.join("repo");
    std::fs::create_dir_all(&repo).expect("a repo");
    std::fs::write(
        repo.join("OWNERS"),
        "# who reads what\n\
         crates/room/src/chat.rs s-b\n\
         * s-c\n",
    )
    .expect("an OWNERS file");
    let server = coordinator(&dir);
    let a = seat(&server, "s-a", A, Seat::Session);
    let b = seat(&server, "s-b", A, Seat::Session);
    a.tree(&repo);

    let ask = asked(a.ask_scope("q1", "crates/room/src/chat.rs", "", "who owns this file?"));
    assert_eq!(ask.target, "s-b");
    assert!(ask.why.contains("OWNERS"), "{:?}", ask.why);
    assert_eq!(count(&server, "ask_by_owners"), 1);
    assert_eq!(b.next().to.as_deref(), Some("s-b"));
}

/// A file the project's folder says nothing about falls to the next rule, which is the room.
#[test]
fn nothing_owns_it_so_the_project_room_gets_the_question() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let a = seat(&server, "s-a", A, Seat::Session);
    let b = seat(&server, "s-b", A, Seat::Session);

    let answered = a.ask_scope("q1", "crates/unknown/x.rs", "", "who knows this file?");
    let Response::Said { message, note, .. } = answered else {
        panic!("an unowned question is posted, not held: {answered:?}");
    };
    assert_eq!(
        message.room,
        project_room(A),
        "the project room, and not a dm"
    );
    assert_eq!(message.to, None, "nobody was asked by name");
    assert_eq!(message.text, "who knows this file?");
    assert!(
        note.contains("nothing owns `crates/unknown/x.rs`"),
        "{note}"
    );
    // B, being in the room, reads it.
    assert_eq!(b.next().text, "who knows this file?");
    assert_eq!(count(&server, "ask_by_room"), 1);
    // And no ask was held, because there is no session whose answer it is.
    let Response::Asks { by_me, .. } = a.call(Request::Asks) else {
        panic!("asks")
    };
    assert!(by_me.is_empty(), "{by_me:?}");
}

/// The asker is never its own target: a rule naming it falls through to the next one.
#[test]
fn the_asker_is_never_its_own_target() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let a = seat(&server, "s-a", A, Seat::Session);
    let b = seat(&server, "s-b", A, Seat::Session);
    // A holds the lease and edited the file; B is only in the room.
    a.own("1", "crates/room/**");
    a.edited(&[("crates/room/src/chat.rs", 1_000)]);

    let answered = a.ask_scope("q1", "crates/room/src/chat.rs", "", "who knows this?");
    let Response::Said { note, .. } = answered else {
        panic!("A cannot ask itself, so the room gets it: {answered:?}");
    };
    assert!(note.contains("nothing owns"), "{note}");
    let _ = b;
}

/// A topic is resolved by a topic lease, and asks in the project room as any ask does.
#[test]
fn a_topic_lease_resolves_the_ask() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let a = seat(&server, "s-a", A, Seat::Session);
    let b = seat(&server, "s-b", A, Seat::Session);
    a.own("1", "wire");
    let ask = asked(b.ask_scope("q1", "", "wire", "is the ask frame's version bumped?"));
    assert_eq!(ask.target, "s-a");
    assert!(ask.why.contains("live lease on `wire`"), "{:?}", ask.why);
    assert_eq!(count(&server, "ask_by_lease"), 1);
}

/// Asking a session by name is what it always was: no scope, no rule, and no reason to explain.
#[test]
fn asking_a_session_by_name_still_works() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let a = seat(&server, "s-a", A, Seat::Session);
    let b = seat(&server, "s-b", A, Seat::Session);
    let ask = asked(a.call(Request::Ask {
        id: "q1".into(),
        room: String::new(),
        to: "s-b".into(),
        path: String::new(),
        topic: String::new(),
        question: "which port?".into(),
    }));
    assert_eq!(ask.target, "s-b");
    assert_eq!(ask.why, "", "a named ask has nothing to explain");
    assert_eq!(count(&server, "ask_by_lease"), 0);
    assert_eq!(b.next().to.as_deref(), Some("s-b"));
}

/// A scope ask that names nothing, or names both a path and a topic, is refused before anything is
/// written, and the refusal says which.
#[test]
fn an_ask_with_no_scope_and_no_session_is_refused() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let a = seat(&server, "s-a", A, Seat::Session);
    let why = refused(a.call(Request::Ask {
        id: "q1".into(),
        room: String::new(),
        to: String::new(),
        path: String::new(),
        topic: String::new(),
        question: "anyone?".into(),
    }));
    assert!(why.to_string().contains("names a session"), "{why}");
    let why = refused(a.ask_scope("q2", "crates/room/**", "wire", "anyone?"));
    assert!(why.to_string().contains("not both"), "{why}");
    let Response::Asks { by_me, .. } = a.call(Request::Asks) else {
        panic!("asks")
    };
    assert!(by_me.is_empty(), "{by_me:?}");
}

/// The operator's seat is not a session: it asks nobody through the room, exactly as before.
#[test]
fn the_operators_seat_cannot_hold_an_ask() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let op = seat(&server, OPERATOR, "", Seat::Operator);
    let why = refused(op.ask_scope("q1", "crates/room/**", "", "anyone?"));
    assert!(matches!(why, Refused::AskNotASession), "{why:?}");
}

/// A broadcast question in the project room that names something an owner has is nudged toward the
/// ask: the line still lands, the nudge is advice, and the room counts it.
#[test]
fn a_broadcast_question_is_nudged_toward_the_owner() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let a = seat(&server, "s-a", A, Seat::Session);
    let b = seat(&server, "s-b", A, Seat::Session);
    a.own("1", "crates/room/**");

    let answered = b.say("s1", "does `crates/room/src/server.rs` route the ask?");
    let Response::Said { message, note, .. } = answered else {
        panic!("a broadcast question is posted, not refused: {answered:?}");
    };
    assert_eq!(message.room, project_room(A));
    assert!(
        note.contains("`s-a` owns this (lease on `crates/room/**`)"),
        "{note}"
    );
    // The ask it points at is the word the post named, which resolves to the same owner.
    assert!(
        note.contains("room ask path=crates/room/src/server.rs"),
        "{note}"
    );
    // A, being in the room, still reads the line: the nudge is advice, not a filter.
    assert!(a.next().text.contains("route the ask"));
    assert_eq!(count(&server, "broadcast_question"), 1);
}

/// A post that is not a question is nobody's nudge, and neither is one that already names someone,
/// nor one the operator says.
#[test]
fn only_an_unaddressed_question_of_a_project_room_is_nudged() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let a = seat(&server, "s-a", A, Seat::Session);
    let b = seat(&server, "s-b", A, Seat::Session);
    let op = seat(&server, OPERATOR, "", Seat::Operator);
    a.own("1", "crates/room/**");
    for (id, text) in [
        (
            "s1",
            "`crates/room/src/server.rs` is where the ask path lives",
        ),
        (
            "s2",
            "@arango does `crates/room/src/server.rs` route the ask?",
        ),
    ] {
        let Response::Said { note, .. } = b.say(id, text) else {
            panic!("{text}");
        };
        assert_eq!(note, "", "{text}");
    }
    let Response::Said { note, .. } = op.say_in(
        project_room(A),
        "o1",
        "does `crates/room/src/server.rs` route the ask?",
    ) else {
        panic!("the operator posts")
    };
    assert_eq!(note, "");
    assert_eq!(count(&server, "broadcast_question"), 0);
}
