//! Chat through the daemon's end: sessions of two projects on one coordinator speak with the
//! `room` tool's verbs, the operator speaks through its seat, and the daemon's own facts (a todo
//! row, a queue row) arrive as messages.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use letibot_harnessd::room::link::SessionRoom;
use letibot_harnessd::room::operator::OperatorSeat;
use letibot_harnessd::room::wake;
use letibot_room::server::Server;
use letibot_room::{Room, project_room};
use letibot_sessionlog::event::{TodoBy, TodoEntry, TodoStatus};
use serde_json::json;

const A: &str = "aaaaaaaaaaaa";
const B: &str = "bbbbbbbbbbbb";

fn wait_for<T>(what: &str, mut probe: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if let Some(v) = probe() {
            return v;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("timed out waiting for {what}");
}

fn todo(content: &str, status: TodoStatus) -> TodoEntry {
    TodoEntry {
        content: content.into(),
        status,
        by: TodoBy::default(),
        when: None,
        needs: Vec::new(),
    }
}

#[test]
fn sessions_the_operator_and_the_daemons_facts_share_the_rooms() {
    let dir = std::env::temp_dir().join(format!("letibot-chat-daemon-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("room.sock");
    let server = Server::serve(&path, Room::new()).expect("a coordinator");

    let rung: Arc<Mutex<Vec<String>>> = Arc::default();
    let bell: Arc<dyn Fn(&str) + Send + Sync> = {
        let rung = Arc::clone(&rung);
        Arc::new(move |s: &str| rung.lock().unwrap().push(s.to_string()))
    };
    let connect = |id: &str, project: &str| {
        SessionRoom::connect(id, &path, project, None, bell.clone(), None).expect("connects")
    };
    let a1 = connect("s-chat-a1", A);
    let b1 = connect("s-chat-b1", B);
    let seat = OperatorSeat::new(path.clone());

    // A session can say in the host room and name another by `to`; the other's daemon is woken,
    // the author's is not.
    let said = a1
        .call(
            "say",
            &json!({"room": "host", "text": "who has the disk?", "to": "s-chat-b1"}),
        )
        .expect("a say");
    assert!(said.contains("in dm to `s-chat-b1`"), "{said}");
    let woken = wait_for("b1 woken", || {
        let got = wake::take("s-chat-b1");
        (!got.is_empty()).then_some(got)
    });
    assert!(
        woken[0].contains("from s-chat-a1 (session) to dm\n> who has the disk?"),
        "{woken:?}"
    );
    assert!(rung.lock().unwrap().iter().any(|s| s == "s-chat-b1"));
    assert!(
        wake::take("s-chat-a1").is_empty(),
        "an author is not woken by itself"
    );

    // A session's announcement in the host room wakes no other session: the host room is
    // nobody's own room, so a line there is read when it is read, not a turn for everyone.
    a1.call(
        "say",
        &json!({"room": "host", "text": "just thinking aloud", "kind": "announce"}),
    )
    .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(500));
    let woken = wake::take("s-chat-b1");
    assert!(
        !woken.iter().any(|l| l.contains("just thinking aloud")),
        "a host announcement woke a session of another project: {woken:?}"
    );

    // The operator's unaddressed word in the host room wakes both.
    let r = seat.ask(
        "h1",
        json!({"type": "say", "id": "o1", "room": "host", "text": "status?"}),
    );
    assert_eq!(r["message"]["author"], "operator", "{r}");
    for who in ["s-chat-a1", "s-chat-b1"] {
        wait_for(who, || {
            let got = wake::take(who);
            got.iter().any(|l| l.contains("status?")).then_some(())
        });
    }

    // A session reads its own project's room by default, never the other's.
    let r = a1.call("read", &json!({"room": "host"})).unwrap();
    assert!(r.contains("status?"), "{r}");
    // The line with a `to` was a private message, so it is in the dm, not the host room.
    assert!(!r.contains("who has the disk?"), "{r}");
    let dm = a1.call("read", &json!({"room": "dm"})).unwrap();
    assert!(dm.contains("who has the disk?"), "{dm}");
    let err = a1
        .call("read", &json!({"room": project_room(B)}))
        .expect_err("another project's room is refused");
    assert!(err.contains("is another project's room"), "{err}");

    // The operator lists all three rooms.
    let rooms = seat.ask("h1", json!({"type": "rooms"}));
    let ids: Vec<String> = rooms["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids.len(), 3, "{ids:?}");

    // The daemon's own facts: a todo row filed. The first update only
    // records the board.
    a1.todos_changed(&[todo("write the doc", TodoStatus::Pending)]);
    a1.todos_changed(&[
        todo("write the doc", TodoStatus::Completed),
        todo("ship it", TodoStatus::Pending),
    ]);
    let lines = wait_for("the facts", || {
        let r = seat.ask("h1", json!({"type": "read", "room": project_room(A)}));
        let text: Vec<String> = r["rows"]
            .as_array()?
            .iter()
            .map(|m| m["text"].as_str().unwrap_or("").to_string())
            .collect();
        (text.len() >= 2).then_some((text, r))
    });
    let (text, raw) = lines;
    assert!(
        text.contains(&"todo done: write the doc".to_string()),
        "{text:?}"
    );
    assert!(
        text.contains(&"todo filed: ship it".to_string()),
        "{text:?}"
    );
    let kinds: Vec<&str> = raw["rows"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|m| m["refs"].as_array().unwrap())
        .map(|r| r["kind"].as_str().unwrap())
        .collect();
    assert!(
        kinds.contains(&"todo") && !kinds.contains(&"queue"),
        "{kinds:?}"
    );
    // And B never saw project A's facts.
    let b = b1.call("read", &json!({"room": "project"})).unwrap();
    assert!(!b.contains("m-7"), "{b}");

    // Nobody connects as the operator through the session door.
    assert!(
        SessionRoom::connect("operator", &path, A, None, bell.clone(), None).is_err(),
        "the operator's name is not a session's"
    );

    server.stop();
}

/// An `fyi` is a note to the room it is said in and an address to the one it names: the named
/// reader is woken, nobody else is, and an `fyi` naming nobody rings nothing. Covers a dm answered
/// in the host room as `@x ...` with kind `fyi`.
#[test]
fn an_fyi_rings_only_the_session_it_names() {
    let dir = std::env::temp_dir().join(format!("letibot-chat-fyi-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("room.sock");
    let server = Server::serve(&path, Room::new()).expect("a coordinator");

    let rung: Arc<Mutex<Vec<String>>> = Arc::default();
    let bell: Arc<dyn Fn(&str) + Send + Sync> = {
        let rung = Arc::clone(&rung);
        Arc::new(move |s: &str| rung.lock().unwrap().push(s.to_string()))
    };
    let connect = |id: &str| {
        SessionRoom::connect(id, &path, "f4f4f4f4f4f4", None, bell.clone(), None).expect("connects")
    };
    let me = connect("s-fyi-me");
    let peer = connect("s-fyi-peer");

    // Nobody named: every session of the room is handed the line as a note, and no bell rings.
    me.call(
        "say",
        &json!({"text": "the cache key moved", "kind": "fyi"}),
    )
    .expect("a say");
    wait_for("the note", || {
        (wake::waiting("s-fyi-peer") > 0).then_some(())
    });
    assert!(
        !rung.lock().unwrap().iter().any(|s| s == "s-fyi-peer"),
        "an fyi that names nobody rang a session: {:?}",
        rung.lock().unwrap()
    );
    assert!(
        wake::take("s-fyi-peer")[0].contains(", passing by"),
        "an unnamed fyi is not a passing note"
    );

    // Naming the peer: this is the peer's to answer, so its daemon is woken, and only its.
    me.call(
        "say",
        &json!({"text": "@s-fyi-peer the port moved to 5433", "kind": "fyi"}),
    )
    .expect("a say");
    wait_for("the peer rung", || {
        rung.lock()
            .unwrap()
            .iter()
            .any(|s| s == "s-fyi-peer")
            .then_some(())
    });
    let said = wake::take("s-fyi-peer");
    assert!(said[0].contains("the port moved to 5433"), "{said:?}");
    assert!(
        !said[0].contains("passing by"),
        "the session it names is told it only passed by: {}",
        said[0]
    );
    assert!(
        rung.lock().unwrap().iter().all(|s| s != "s-fyi-me"),
        "an author was rung by its own line"
    );
    // And the other side of the rule holds as tightly: an `fyi` naming nobody rings nobody, not
    // even the session whose work the room is talking about.
    peer.call(
        "say",
        &json!({"text": "the disk is fine again", "kind": "fyi"}),
    )
    .expect("a say");
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        rung.lock()
            .unwrap()
            .iter()
            .filter(|s| *s == "s-fyi-peer")
            .count()
            == 1,
        "a peer's unnamed fyi rang it again: {:?}",
        rung.lock().unwrap()
    );
    server.stop();
}

/// A long post is filed as a record: the session it was sent gets its first line and a pointer,
/// and the whole text is read in that SESSION's name - a dm is its two ends' to read, and the
/// operator's own seat is neither of them.
#[test]
fn a_record_is_read_whole_in_the_sessions_own_name() {
    let dir = std::env::temp_dir().join(format!("letibot-chat-record-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("room.sock");
    let server = Server::serve(&path, Room::new()).expect("a coordinator");

    let bell: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|_: &str| {});
    let _me = SessionRoom::connect("s-rec-me", &path, "c1c1c1c1c1c1", None, bell, None)
        .expect("connects");

    let text = "Tranche, in order of value:\n\n1. the parser takes the caller's shape\n\
                2. the caller keeps its own copy\n3. the cache follows the parser";
    let quiet: letibot_room::client::Wake = Arc::new(|_: &letibot_room::Event| {});
    let talk = letibot_room::client::Client::connect(
        &path,
        letibot_room::Speaker::top_level("s-rec-peer"),
        quiet,
    )
    .expect("a second connection");
    let filed = match talk.request(&letibot_room::Request::Dm {
        id: "d1".into(),
        to: "s-rec-me".into(),
        text: text.into(),
        thread: None,
        attachments: Vec::new(),
    }) {
        Ok(letibot_room::Response::Said { message, .. }) => message,
        other => panic!("the dm was not filed: {other:?}"),
    };
    // What the session was sent: the first line, and the pointer to the rest.
    assert!(filed.text.contains("[record #"), "{}", filed.text);
    assert!(!filed.text.contains("the cache follows"), "{}", filed.text);

    let seat = OperatorSeat::new(path.clone());
    seat.reading_records(Arc::new(letibot_harnessd::room::link::record));
    let op = |seq: u64| json!({"type": "record", "session": "s-rec-me", "room": "dm", "seq": seq});
    let whole = seat.ask("h1", op(filed.seq));
    assert_eq!(whole["text"], text, "{whole}");

    // A number the room has nothing for is one sentence, and never an empty text that reads as
    // "the message was empty".
    let gone = seat.ask("h1", op(filed.seq + 9));
    assert!(gone["text"].is_null(), "{gone}");
    assert!(
        gone["error"]
            .as_str()
            .unwrap_or_default()
            .contains("no longer holds"),
        "{gone}"
    );

    // And the session's own seat reads what it always could: the record is not a widening of the
    // operator's reach, it is the reader's.
    let theirs = seat.ask("h1", op(filed.seq));
    assert!(theirs["error"].is_null(), "{theirs}");
    server.stop();
}
