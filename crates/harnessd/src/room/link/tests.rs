use super::*;
use serde_json::json;

/// A scratch directory that removes itself however the test ends. The lease and the restart
/// note in it are read as history, so a copy left by a panic is a room this run already had.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let p = std::env::temp_dir().join(format!(
            "letibot-room-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).expect("a scratch directory");
        Scratch(p)
    }

    fn join(&self, name: &str) -> std::path::PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The tool's attachment table and the room's allowlist are one list: a name this daemon would
/// upload is a mime the coordinator takes, and a kind it refuses is one nothing uploads.
#[test]
fn the_attachment_table_is_the_rooms_allowlist() {
    for name in [
        "a.png",
        "b.jpg",
        "c.jpeg",
        "d.gif",
        "e.webp",
        "f.txt",
        "/x/y/shot.png",
    ] {
        let mime = letibot_tools::builtins::room::mime_of(name)
            .unwrap_or_else(|| panic!("{name} is a kind the daemon must carry"));
        assert!(
            letibot_room::blob::MIMES.contains(&mime),
            "{name} maps to {mime}, which the room refuses"
        );
    }
    for name in ["a.pdf", "b.png.exe", "no-extension"] {
        assert_eq!(letibot_tools::builtins::room::mime_of(name), None, "{name}");
    }
    // The sizes travel with the same table, and a bridge reads them from there: one number, so
    // a part that outgrows a frame or a blob past the cap is caught in one place.
    assert_eq!(
        letibot_tools::builtins::room::MAX_ATTACHMENT,
        letibot_room::blob::MAX_BLOB
    );
    assert_eq!(
        letibot_tools::builtins::room::PART_BYTES,
        letibot_room::blob::MAX_PART
    );
}

#[test]
fn every_verb_becomes_a_frame_with_a_request_id() {
    let cases = [
        json!({"verb": "post", "tag": "ram", "body": "8 GB"}),
        json!({"verb": "claim", "tag": "ram"}),
        json!({"verb": "claim_item", "item": 1}),
        json!({"verb": "beat", "item": 1}),
        json!({"verb": "waiting", "item": 1, "text": "the suite"}),
        json!({"verb": "done", "item": 1}),
        json!({"verb": "abandon", "item": 1, "why": "no"}),
        json!({"verb": "dispute", "item": 1, "says": "still on it"}),
        json!({"verb": "rule", "question": 1, "decision": "retry"}),
        json!({"verb": "queue", "tag": "ram"}),
        json!({"verb": "holding"}),
        json!({"verb": "reminders"}),
        json!({"verb": "questions"}),
        json!({"verb": "asks"}),
        json!({"verb": "answer", "ask": 4211, "text": "5433"}),
        json!({"verb": "decline", "ask": 4211, "text": "not my area"}),
    ];
    for args in cases {
        let verb = args["verb"].as_str().unwrap();
        let frame = request_of(verb, &args).unwrap_or_else(|e| panic!("{verb}: {e}"));
        // Every MUTATING frame carries an id - that is what makes a reconnect's repeat
        // answerable. A read has none by design (`Request::request_id` says so): there is
        // nothing to act on twice, and a re-read is cheap and cannot be wrong.
        let mutates = !matches!(
            frame,
            Request::Queue { .. }
                | Request::Holding { .. }
                | Request::Reminders
                | Request::Questions
                | Request::Asks
        );
        assert!(
            frame.request_id().is_some() == mutates,
            "{verb}: a mutating frame needs a request id and a read must not have one: {frame:?}"
        );
    }
}

/// The two verbs that settle an ask carry the number and the words, and the sentence a model
/// reads names the ask, the peer and the outcome - the whole of what it needs to carry on.
#[test]
fn an_answer_becomes_a_reply_and_its_sentence_names_the_ask() {
    match request_of(
        "answer",
        &json!({"verb": "answer", "ask": 4211, "text": " 5433 "}),
    )
    .expect("a frame")
    {
        Request::Reply { ask, text, .. } => {
            assert_eq!(ask, 4211);
            assert_eq!(text, "5433");
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        request_of("decline", &json!({"ask": 1, "text": "no"})).expect("a frame"),
        Request::Decline { .. }
    ));
    assert!(matches!(
        request_of("asks", &json!({})).expect("a frame"),
        Request::Asks
    ));
    // A verb without the number is refused here, and nothing is sent.
    let e = request_of("answer", &json!({"text": "5433"})).unwrap_err();
    assert!(e.contains("needs `ask`"), "{e}");

    let ask = |state, gone: bool| letibot_room::AskView {
        seq: 4211,
        room: "host".into(),
        asker: "s-a".into(),
        target: "s-b".into(),
        question: "which port?".into(),
        asked_ms: 1,
        gone_since_ms: gone.then_some(2),
        escalated: false,
        cannot: String::new(),
        state,
    };
    let answered = answer_sentence(
        &Response::Asked {
            id: "r1".into(),
            ask: ask(
                letibot_room::AskState::Answered {
                    text: "5433".into(),
                    ms: 3,
                },
                false,
            ),
        },
        "s-a",
    )
    .expect("a sentence");
    assert!(answered.contains("ask #4211"), "{answered}");
    assert!(answered.contains("`s-b`"), "{answered}");
    assert!(answered.contains("answered: 5433"), "{answered}");
    assert!(
        !answered.contains("not connected"),
        "an ended ask says nothing about liveness: {answered}"
    );

    // An open ask of a session that is not connected now says so: the model has to know its
    // question is waiting rather than lost.
    let waiting = answer_sentence(
        &Response::Asked {
            id: "q1".into(),
            ask: ask(letibot_room::AskState::Open, true),
        },
        "s-a",
    )
    .expect("a sentence");
    assert!(waiting.contains("not connected now"), "{waiting}");

    let list = answer_sentence(
        &Response::Asks {
            by_me: vec![ask(letibot_room::AskState::Open, false)],
            of_me: Vec::new(),
        },
        "s-a",
    )
    .expect("a sentence");
    assert!(list.contains("1 ask(s) you made, 0 owed to you"), "{list}");
    assert!(list.contains("which port?"), "{list}");
}

#[test]
fn a_hand_without_a_target_is_refused_here_and_never_sent() {
    let e = request_of("rule", &json!({"question": 1, "decision": "hand"})).unwrap_err();
    assert!(e.contains("needs `to`"), "{e}");
    let e = request_of("rule", &json!({"question": 1, "decision": "maybe"})).unwrap_err();
    assert!(e.contains("not a decision"), "{e}");
    let e = request_of("nonsense", &json!({})).unwrap_err();
    assert!(e.contains("not a room verb"), "{e}");
}

/// What backs a claim about the world rides `refs` as an `evidence` ref, so the frame's shape did
/// not change and a `say` without one carries no such ref at all.
#[test]
fn an_evidence_ref_travels_with_the_say_that_carries_it() {
    let Request::Say { refs, .. } = request_of(
        "say",
        &json!({"text": "landed", "evidence": " 2b97a092 ", "queue": "m-7"}),
    )
    .expect("a frame") else {
        panic!("a say is a `Request::Say`")
    };
    let kinds: Vec<(&str, &str)> = refs
        .iter()
        .map(|r| (r.kind.as_str(), r.id.as_str()))
        .collect();
    assert_eq!(kinds, vec![("queue", "m-7"), ("evidence", "2b97a092")]);
    let Request::Say { refs, .. } =
        request_of("say", &json!({"text": "who has the disk?"})).expect("a frame")
    else {
        panic!("a say is a `Request::Say`")
    };
    assert!(refs.is_empty(), "nothing claimed, nothing to back");
}

/// A session wrote "arango session: ..." with no `to`, which asks nobody for a reply. The
/// reply says so, and an addressed message does not.
#[test]
fn a_say_that_names_nobody_says_it_asked_nobody() {
    let said = |to: Option<&str>, text: &str| Response::Said {
        id: "c1".into(),
        message: letibot_room::Message {
            seq: 79,
            ts: 0,
            room: "host".into(),
            author: "s-a".into(),
            kind: letibot_room::Seat::Session,
            to: to.map(str::to_string),
            thread: None,
            refs: Vec::new(),
            attachments: Vec::new(),
            text: text.into(),
        },
    };
    let plain = answer_sentence(&said(None, "arango session: two questions"), "s-a").unwrap();
    assert!(plain.contains("nobody is asked to reply"), "{plain}");
    for (to, text) in [(Some("s-b"), "two questions"), (None, "@s-b two questions")] {
        let named = answer_sentence(&said(to, text), "s-a").unwrap();
        assert!(!named.contains("nobody is asked"), "{named}");
    }
}

#[test]
fn a_refusal_from_the_room_is_a_sentence_and_not_a_panic() {
    let refused = Response::Refused {
        id: Some("c1".into()),
        why: letibot_room::Refused::Busy {
            tag: "ram".into(),
            item: 1,
            by: "s-b".into(),
            held_for: "40s".into(),
        },
    };
    let e = answer_sentence(&refused, "s-a").unwrap_err();
    assert!(e.contains("held by `s-b`"), "{e}");
    assert!(e.contains("a tag is a lock"), "{e}");
}

/// Another session's unaddressed chat wakes this one, told that no reply is asked. A session
/// wrote "arango session: ..." with no `to`, and arango never heard it.
#[test]
fn a_sessions_unaddressed_chat_wakes_every_other_session() {
    let me_id = "s-a-chatwake";
    let other_id = "s-b-chatwake";
    let dir = std::env::temp_dir().join(format!("letibot-room-chatwake-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("room.sock");
    let server =
        letibot_room::Server::serve(&path, letibot_room::Room::new()).expect("a room to talk to");
    let rung: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let bell: Arc<dyn Fn(&str) + Send + Sync> = {
        let rung = Arc::clone(&rung);
        Arc::new(move |session: &str| rung.lock().unwrap().push(session.to_string()))
    };
    let _me = SessionRoom::connect(
        me_id,
        &path,
        letibot_room::DEFAULT_PROJECT,
        None,
        bell.clone(),
        None,
    )
    .expect("s-a connects");
    let other = SessionRoom::connect(
        other_id,
        &path,
        letibot_room::DEFAULT_PROJECT,
        None,
        bell,
        None,
    )
    .expect("s-b connects");
    other
        .call(
            "say",
            &serde_json::json!({"room": "host", "text": "arango session: two questions"}),
        )
        .expect("a say");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let mut said: Vec<String> = Vec::new();
    while std::time::Instant::now() < deadline {
        said.extend(wake::take(me_id));
        if said.iter().any(|s| s.contains("two questions")) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    // The room's own fact about another session's row wakes nobody it does not name.
    other.notify_in(
        "host",
        "todo",
        format!("todo:{other_id}:done:a row"),
        "a row is done".to_string(),
        Vec::new(),
    );
    std::thread::sleep(std::time::Duration::from_millis(500));
    said.extend(wake::take(me_id));
    assert!(
        !said.iter().any(|s| s.contains("a row is done")),
        "a todo fact woke `{me_id}`: {said:?}"
    );
    let line = said
        .iter()
        .find(|s| s.contains("two questions"))
        .unwrap_or_else(|| panic!("the chat never reached `{me_id}`: {said:?}"));
    assert!(line.contains("to all, passing by"), "{line}");
    assert!(
        rung.lock().unwrap().iter().any(|s| s == me_id),
        "the bell did not ring for `{me_id}`: {:?}",
        rung.lock().unwrap()
    );
    // Taking the note is what marks it seen: a push alone left a restarted daemon's model
    // without it, and the join digest skipped it.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while server.seen(me_id).unwrap_or(0) == 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        server.seen(me_id).unwrap_or(0) > 0,
        "the taken note never reached the room as a seen mark"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `read` with `check: true` judges each state claim in the room against the store's queue rows,
/// naming the row that decided it; a session with no store says the check could not be made
/// rather than letting NOT COVERED stand in for every line.
#[test]
fn a_read_can_check_the_rooms_claims_against_the_queues_rows() {
    let dir = std::env::temp_dir().join(format!("letibot-room-check-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("room.sock");
    let _server =
        letibot_room::Server::serve(&path, letibot_room::Room::new()).expect("a room to talk to");
    let db = dir.join("sessions.db");
    let store = letibot_tokencore::store::Store::open(&db).expect("a store");
    store
        .connection()
        .execute_batch(
            "INSERT INTO merge_queue (id, review_id, repository, target, branch, approved_sha,
                approved_by, priority, state, landed_sha, pushed, evidence, created_ms, state_ms,
                updated_ms)
             VALUES ('m-3', 'r-3', '/box/letibot', 'main', 'topic/y', 'aaaaaaa1111', 'queue',
                'urgent', 'landed', '2b97a092', 'pushed origin/main', 'landed', 1, 1, 1);",
        )
        .expect("a landed row");
    let bell: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|_: &str| {});
    let me = SessionRoom::connect(
        "s-a-check",
        &path,
        letibot_room::DEFAULT_PROJECT,
        None,
        bell.clone(),
        None,
    )
    .expect("s-a connects");
    me.set_store(Some(db.clone()));
    for text in ["landed in 2b97a092", "landed as m-9, the gate is green"] {
        me.call("say", &json!({"room": "host", "text": text}))
            .expect("a say");
    }
    let read = me
        .call("read", &json!({"room": "host", "check": true, "last": 10}))
        .expect("a read");
    assert!(read.contains("SUPPORTED - merge m-3 is `landed`"), "{read}");
    assert!(
        read.contains("NOT COVERED - no merge or gate row names `m-9`"),
        "{read}"
    );
    assert!(
        read.contains("check (2 claim(s) against 1 merge row(s))"),
        "{read}"
    );
    // The same read without `check` says nothing about verdicts: it is a read, not a judgement.
    let plain = me
        .call("read", &json!({"room": "host", "last": 10}))
        .expect("a read");
    assert!(!plain.contains("SUPPORTED"), "{plain}");
    // A session whose daemon has no store says so, once, rather than judging nothing.
    let blind = SessionRoom::connect(
        "s-b-check",
        &path,
        letibot_room::DEFAULT_PROJECT,
        None,
        bell,
        None,
    )
    .expect("s-b connects");
    blind
        .call(
            "say",
            &json!({"room": "host", "text": "landed in 2b97a092"}),
        )
        .expect("a say");
    let read = blind
        .call("read", &json!({"room": "host", "check": true}))
        .expect("a read");
    assert!(read.contains("this session has no store"), "{read}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Rule 5, end to end: a session is woken by its own room, by a dm to it and by the operator's
/// own words - and not by the host room's talk between two others.
#[test]
fn a_host_line_between_others_does_not_wake_a_project_session() {
    let me_id = "s-a-routing";
    let same_id = "s-a2-routing";
    let other_id = "s-b-routing";
    let (key_a, key_b) = ("aaaaaaaaaaaa", "bbbbbbbbbbbb");
    let dir = std::env::temp_dir().join(format!("letibot-room-routing-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("room.sock");
    let server =
        letibot_room::Server::serve(&path, letibot_room::Room::new()).expect("a room to talk to");
    let bell: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|_: &str| {});
    let me = SessionRoom::connect(me_id, &path, key_a, None, bell.clone(), None)
        .expect("my own room's session");
    let same = SessionRoom::connect(same_id, &path, key_a, None, bell.clone(), None)
        .expect("the other session of my project");
    let other = SessionRoom::connect(other_id, &path, key_b, None, bell, None)
        .expect("a session of another project");
    let operator = letibot_room::client::Client::connect_as(
        &path,
        Speaker::top_level(letibot_room::OPERATOR),
        server.name(),
        letibot_room::Seat::Operator,
        Arc::new(|_: &letibot_room::Event| {}),
    )
    .expect("the operator's seat");

    // My own project's room, said to the room: it wakes me.
    same.call(
        "say",
        &serde_json::json!({"room": "project", "text": "the build is red"}),
    )
    .expect("a say");
    let line = until_note(me_id, "the build is red");
    assert!(line.contains("to all, passing by"), "{line}");

    // The host room's chatter between two others: a broadcast, and a mention of the third
    // session. Neither is mine, and neither is a room I sit in.
    other
        .call(
            "say",
            &serde_json::json!({"room": "host", "text": "anyone seen the fleet key?"}),
        )
        .expect("a say");
    other
        .call(
            "say",
            &serde_json::json!({"room": "host", "text": format!("@{same_id} did you see that?")}),
        )
        .expect("a say");
    // The one it names is woken where it was named; the bystander of another project is not.
    let named = until_note(same_id, "did you see that?");
    assert!(named.contains("to @you"), "{named}");
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert_eq!(
        wake::waiting(me_id),
        0,
        "host chatter between others woke `{me_id}`: {:?}",
        wake::take(me_id)
    );

    // A `to` is a private message: the tool answer says where it went, and it wakes the one.
    let answer = other
        .call(
            "say",
            &serde_json::json!({"to": me_id, "text": "just between us"}),
        )
        .expect("a say");
    assert!(answer.contains("in dm"), "{answer}");
    assert!(answer.contains("only the two of you read it"), "{answer}");
    let line = until_note(me_id, "just between us");
    assert!(
        line.contains(") to dm\n"),
        "a dm does not read as private: {line}"
    );

    // And the operator's own words reach a session wherever they are said.
    let said = operator
        .request(&letibot_room::Request::Say {
            id: "op1".into(),
            room: letibot_room::chat::HOST_ROOM.into(),
            text: "everyone: the box restarts at six".into(),
            to: None,
            thread: None,
            refs: Vec::new(),
            attachments: Vec::new(),
            kind: Default::default(),
        })
        .expect("the operator says");
    assert!(
        matches!(said, letibot_room::Response::Said { .. }),
        "{said:?}"
    );
    let line = until_note(me_id, "restarts at six");
    assert!(line.contains("(operator) to all\n"), "{line}");

    drop((me, same, other));
    server.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The sentence `session` is woken by that carries `needle`, or a panic naming what it got.
fn until_note(session: &str, needle: &str) -> String {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let mut said: Vec<String> = Vec::new();
    while std::time::Instant::now() < deadline {
        said.extend(wake::take(session));
        if let Some(line) = said.iter().find(|s| s.contains(needle)) {
            return line.clone();
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    panic!("`{session}` was never woken by `{needle}`: {said:?}");
}

/// The wake, end to end minus the harness: a real server, two real connections, and the
/// bell the daemon rings - an event about `s-a` becomes a sentence and rings, while a session
/// with nothing under the tag is not rung. `Harness::wake` composing it into a turn is `wake`'s.
#[test]
fn an_event_about_a_session_rings_its_bell_and_a_reader_is_not_rung() {
    // Ids of this test's own, because `wake` is process-wide and keyed by session id. Four
    // other tests in this module connect as `s-a` and post or claim through it, and their
    // sentences landed in THIS test's queue.
    let me_id = "s-a-wake";
    let other_id = "s-b-wake";
    let dir = std::env::temp_dir().join(format!("letibot-room-link-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("room.sock");
    let server =
        letibot_room::Server::serve(&path, letibot_room::Room::new()).expect("a room to talk to");

    let rung: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let bell: Arc<dyn Fn(&str) + Send + Sync> = {
        let rung = Arc::clone(&rung);
        Arc::new(move |session: &str| {
            rung.lock().unwrap().push(session.to_string());
        })
    };
    let me = SessionRoom::connect(
        me_id,
        &path,
        letibot_room::DEFAULT_PROJECT,
        None,
        bell.clone(),
        None,
    )
    .expect("s-a connects");
    let other = Client::connect(
        &path,
        Speaker::top_level(other_id),
        Arc::new(|_: &letibot_room::Event| {}),
    )
    .expect("s-b connects");

    // `s-a-wake` posts, so it is a MEMBER of `ram` - a reader is not.
    me.call("post", &serde_json::json!({"tag": "ram", "body": "8 GB"}))
        .expect("a post");
    let _ = other.request(&Request::Post {
        id: "p2".into(),
        tag: "ram".into(),
        body: "2 GB".into(),
    });
    // And somebody else's claim under that tag is news this session must be woken by.
    let claimed = other
        .request(&Request::Claim {
            id: "c1".into(),
            tag: "ram".into(),
        })
        .expect("a claim");
    assert!(matches!(claimed, Response::Claimed { .. }), "{claimed:?}");

    // Drained until the sentence this test is about arrives, not until the queue is not
    // empty: the first sentence `me` is woken by is its OWN post, and the claim lands a
    // moment later.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let mut said: Vec<String> = Vec::new();
    while std::time::Instant::now() < deadline {
        said.extend(wake::take(me_id));
        if said
            .iter()
            .any(|s| s.contains("[room]") && s.contains(other_id))
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        rung.lock().unwrap().iter().any(|s| s == me_id),
        "the bell must ring for the session the event is about: {:?}",
        rung.lock().unwrap()
    );
    assert!(
        !said.is_empty(),
        "a ring with nothing behind it is not a wake"
    );
    assert!(
        said.iter()
            .any(|s| s.contains("[room]") && s.contains(other_id)),
        "the sentence names who did it: {said:?}"
    );

    // The arbiter is not woken by the queue's traffic. It hears every event on its
    // connection - that is how a question reaches it - but a `post` is not a turn.
    let arbiter = super::super::session_id();
    let rung_arbiter: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));
    let bell_arbiter: Arc<dyn Fn(&str) + Send + Sync> = {
        let n = Arc::clone(&rung_arbiter);
        Arc::new(move |_: &str| *n.lock().unwrap() += 1)
    };
    let host = SessionRoom::connect(
        &arbiter,
        &path,
        letibot_room::DEFAULT_PROJECT,
        None,
        bell_arbiter,
        None,
    )
    .expect("the arbiter");
    let _ = other.request(&Request::Post {
        id: "p3".into(),
        tag: "ram".into(),
        body: "1 GB".into(),
    });
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert_eq!(
        *rung_arbiter.lock().unwrap(),
        0,
        "a post is not a question: the arbiter is woken by a raise, not by the queue moving"
    );
    assert_eq!(wake::waiting(&arbiter), 0);
    drop(host);
    drop(me);
    server.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A tag's agent hears its own tag, and only a dispute rings it. The operator's cost answer
/// made structural: a claim, a healthy beat and a finished item are comparisons and reach it as
/// NOTES; only the question rings. And another tag's traffic does not reach it at all.
#[test]
fn a_tags_agent_is_note_woken_by_its_queue_and_rung_only_by_a_dispute() {
    let dir = std::env::temp_dir().join(format!("letibot-room-tag-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("room.sock");
    let server =
        letibot_room::Server::serve(&path, letibot_room::Room::new()).expect("a room to talk to");

    let rung: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let bell: Arc<dyn Fn(&str) + Send + Sync> = {
        let rung = Arc::clone(&rung);
        Arc::new(move |session: &str| rung.lock().unwrap().push(session.to_string()))
    };
    // The tag's agent, by the derivation - not a hand-written id, because the router
    // addresses it by the same function and a test that spelled its own would prove nothing.
    let key = letibot_room::host_key();
    let agent = letibot_room::tag_session(&key, "ram").expect("an agent for ram");
    let mine = SessionRoom::connect(
        &agent,
        &path,
        letibot_room::DEFAULT_PROJECT,
        None,
        bell,
        None,
    )
    .expect("the tag's agent");
    let other = Client::connect(
        &path,
        Speaker::top_level("s-b"),
        Arc::new(|_: &letibot_room::Event| {}),
    )
    .expect("s-b connects");

    // A post, a claim, and an abandonment - the queue moving, and none of it a judgement.
    let posted = other
        .request(&Request::Post {
            id: "p1".into(),
            tag: "ram".into(),
            body: "8 GB".into(),
        })
        .expect("a post");
    assert!(matches!(posted, Response::Posted { .. }), "{posted:?}");
    let _ = other.request(&Request::Claim {
        id: "c1".into(),
        tag: "ram".into(),
    });
    let _ = other.request(&Request::Abandon {
        id: "a1".into(),
        item: 1,
        why: "the suite blew up".into(),
    });
    // And another tag's post, which must not reach it at all.
    let _ = other.request(&Request::Post {
        id: "p2".into(),
        tag: "disk".into(),
        body: "2 TB".into(),
    });

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline && wake::waiting(&agent) < 3 {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(
        rung.lock().unwrap().len(),
        0,
        "a claim and an abandonment are comparisons: no turn is opened for them"
    );
    let notes = wake::take(&agent);
    assert!(
        notes.iter().any(|n| n.contains("claimed #1")),
        "the claim reaches it as a note: {notes:?}"
    );
    assert!(
        notes.iter().any(|n| n.contains("lost `s-b`")),
        "and so does the stale beat: {notes:?}"
    );
    assert!(
        !notes.iter().any(|n| n.contains("disk")),
        "and nothing about another tag's item: {notes:?}"
    );

    // A dispute rings it. That is the one thing a tag's agent exists to rule on - and the
    // disputing session must NOT be the holder, which is the case here: `s-b` lost the item to a
    // stale beat and says *I am still on it* after `s-c` has taken it.
    let c = Client::connect(
        &path,
        Speaker::top_level("s-c"),
        Arc::new(|_: &letibot_room::Event| {}),
    )
    .expect("s-c connects");
    let _ = c.request(&Request::Claim {
        id: "c2".into(),
        tag: "ram".into(),
    });
    let asked = other
        .request(&Request::Dispute {
            id: "d1".into(),
            item: 1,
            says: "still on it".into(),
        })
        .expect("a dispute");
    assert!(matches!(asked, Response::Question { .. }), "{asked:?}");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline && rung.lock().unwrap().is_empty() {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(
        rung.lock().unwrap().as_slice(),
        std::slice::from_ref(&agent),
        "exactly one ring, and it is this tag's agent"
    );
    let said = wake::take(&agent);
    assert!(
        said.iter().any(|s| s.contains("question")),
        "the ring has the question behind it: {said:?}"
    );
    drop(mine);
    drop(other);
    server.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A row's entry and verb become the frame the room acts on, and a word the pane does not
/// have is refused here rather than sent.
#[test]
fn a_panes_entry_and_verb_become_a_frame() {
    let id = |frame: &Request| frame.request_id().map(str::to_string);
    // `claim` on an item is `claim_item` - the operator's own control, and it must still be the
    // head (the room refuses `NotTheHead`, which is the whole ordering rule).
    let claim = pane_frame("claim", "i12").expect("claim");
    assert!(matches!(claim, Request::ClaimItem { item: 12, .. }));
    assert!(id(&claim).is_some(), "a mutating frame carries an id");

    // `release` and `abandon` are the room's one verb and two sentences.
    let release = pane_frame("release", "i12").expect("release");
    let abandon = pane_frame("abandon", "i12").expect("abandon");
    match (&release, &abandon) {
        (
            Request::Abandon {
                item: 12, why: one, ..
            },
            Request::Abandon {
                item: 12, why: two, ..
            },
        ) => {
            assert!(one.contains("released"), "{one}");
            assert!(two.contains("abandoned"), "{two}");
            assert_ne!(one, two, "the difference the person gets is the sentence");
        }
        other => panic!("both are abandonments of the same item: {other:?}"),
    }

    // `rule` names the decision in the entry, because the decision is what was picked.
    assert!(matches!(
        pane_frame("rule", "q3:retry"),
        Ok(Request::Rule {
            question: 3,
            decision: Decision::Retry,
            ..
        })
    ));
    assert!(matches!(
        pane_frame("rule", "q3:drop"),
        Ok(Request::Rule {
            question: 3,
            decision: Decision::Drop { .. },
            ..
        })
    ));
    assert!(matches!(
        pane_frame("rule", "q3:hand:s-a"),
        Ok(Request::Rule {
            question: 3,
            decision: Decision::Hand { to },
            ..
        }) if to == "s-a"
    ));

    // And the refusals are this door's own, in words a person can act on.
    let hand = pane_frame("rule", "q3:hand").unwrap_err();
    assert!(hand.contains("names no session"), "{hand}");
    let decision = pane_frame("rule", "q3:maybe").unwrap_err();
    assert!(decision.contains("not a decision"), "{decision}");
    let no_question = pane_frame("rule", "q3").unwrap_err();
    assert!(no_question.contains("names no decision"), "{no_question}");
    let no_item = pane_frame("claim", "q3:retry").unwrap_err();
    assert!(no_item.contains("names no item"), "{no_item}");
    let other = pane_frame("post", "i1").unwrap_err();
    assert!(other.contains("four controls"), "{other}");
}

/// The four controls, round trip: a row's key, the room's state machine, the state it made.
/// The layer names a verb and an entry, the HOST performs it over the session's own connection
/// and the room is what decides.
#[test]
fn the_four_controls_reach_the_room_and_change_it() {
    let dir = std::env::temp_dir().join(format!("letibot-room-acts-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("room.sock");
    let server =
        letibot_room::Server::serve(&path, letibot_room::Room::new()).expect("a room to talk to");
    let me = SessionRoom::connect(
        "s-a",
        &path,
        letibot_room::DEFAULT_PROJECT,
        None,
        Arc::new(|_: &str| {}),
        None,
    )
    .expect("s-a connects");

    // Two items under one tag, posted by another session so `s-a` can claim the head.
    let other =
        Client::connect(&path, Speaker::top_level("s-b"), Arc::new(|_| {})).expect("s-b connects");
    for (id, body) in [("p1", "8 GB"), ("p2", "2 GB")] {
        other
            .request(&Request::Post {
                id: id.into(),
                tag: "ram".into(),
                body: body.into(),
            })
            .expect("a post");
    }

    // `claim` - on this session's behalf, and the room says so.
    let said = me.act("i1", "claim").expect("a claim");
    assert!(said.contains("you hold #1"), "{said}");
    assert_eq!(
        server.items()[0].holder(),
        Some("s-a"),
        "the item is held by the session whose pane it is"
    );

    // `release` - the same session gives it up, and it goes back to the head of `ram`.
    let said = me.act("i1", "release").expect("a release");
    assert!(said.contains("went back to `ram`"), "{said}");
    let item = &server.items()[0];
    assert_eq!(item.holder(), None);
    assert_eq!(item.abandonments(), 1, "a voluntary release is counted");
    assert!(
        matches!(&item.abandonments[0].why, letibot_room::Why::Released { says } if says.contains("released from the room pane")),
        "the sentence the host wrote is on the item: {:?}",
        item.abandonments[0].why
    );

    // `abandon` - the same verb, the harder sentence, and it is still one abandonment.
    me.act("i1", "claim").expect("a claim");
    me.act("i1", "abandon").expect("an abandonment");
    let item = &server.items()[0];
    assert_eq!(item.abandonments(), 2);
    assert!(
        matches!(&item.abandonments[1].why, letibot_room::Why::Released { says } if says.contains("marked abandoned")),
        "{:?}",
        item.abandonments[1].why
    );

    // And the third raises a question, which the last control answers.
    me.act("i1", "claim").expect("a claim");
    me.act("i1", "abandon").expect("the third abandonment");
    let questions = server.open_questions();
    assert_eq!(questions.len(), 1, "three lost claimants freeze the item");
    let entry = format!("q{}:retry", questions[0].id);
    let said = me.act(&entry, "rule").expect("a ruling");
    assert!(said.contains("is answered"), "{said}");
    assert!(
        server.open_questions().is_empty(),
        "and the question is closed"
    );
    assert!(
        server.items()[0].claimable(),
        "`retry` puts it back at the head with its count kept"
    );

    // A key the room will not take is refused in the room's own words - the person is told
    // what happened to it rather than left with a key that did nothing.
    let refused = me
        .act("i2", "release")
        .expect_err("`s-a` holds nothing on #2");
    assert!(refused.contains("does not hold"), "{refused}");

    drop(other);
    drop(me);
    server.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The pane's picture of the room is read off the socket, and the identity comes with it.
#[test]
fn the_feed_snapshot_carries_the_rooms_identity_and_its_items() {
    let dir = std::env::temp_dir().join(format!("letibot-room-snap-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("room.sock");
    let server =
        letibot_room::Server::serve(&path, letibot_room::Room::new()).expect("a room to talk to");
    server.with_arbiter("s-arbiter");
    let me = SessionRoom::connect(
        "s-a",
        &path,
        letibot_room::DEFAULT_PROJECT,
        None,
        Arc::new(|_: &str| {}),
        None,
    )
    .expect("connects");
    me.call("post", &json!({"tag": "ram", "body": "8 GB"}))
        .expect("a post");

    // The picture arrives from a thread of its own, so the test waits for it rather than
    // sleeping a fixed second: a pane opened before the first read lands says *not read yet*.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut snap = me.snapshot();
    // The first read is a promise and the second is the answer - the frame it lands may be
    // the one taken BEFORE this test's post, so wait for the item as well as for the read.
    while (snap.unreadable.is_some() || snap.items.is_empty())
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(std::time::Duration::from_millis(10));
        snap = me.snapshot();
    }
    assert_eq!(snap.unreadable, None, "{:?}", snap.unreadable);
    assert!(snap.identity.contains("no item store"), "{}", snap.identity);
    assert_eq!(snap.arbiter.as_deref(), Some("s-arbiter"));
    assert!(
        snap.connected.iter().any(|c| c == "s-a"),
        "who is up: {:?}",
        snap.connected
    );
    assert_eq!(snap.items.len(), 1);
    assert_eq!(snap.items[0].tag, "ram");
    assert_eq!(snap.lease_ms, letibot_room::DEFAULT_LEASE_MS);

    drop(me);
    server.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The restart is a FRAME to the supervisor, and what it found decides the note's cause.
/// The first act has no lease on the socket - nothing was running, so the key is a start; the
/// second has a live lease, which is a restart and names the session whose pane it was.
#[test]
fn the_restart_is_a_frame_to_the_supervisor_and_the_note_says_what_it_found() {
    use std::sync::Mutex as StdMutex;

    struct Recorder(StdMutex<Vec<RestartFrame>>);
    impl RoomSupervisor for Recorder {
        fn restart(&self, frame: &RestartFrame) -> Result<String, String> {
            self.0.lock().unwrap().push(frame.clone());
            Ok("the room was stopped and started again by a fixture".to_string())
        }
    }

    let scratch = Scratch::new("restart");
    let path = scratch.join("room.sock");
    let server =
        letibot_room::Server::serve(&path, letibot_room::Room::new()).expect("a room to talk to");
    let rec = Arc::new(Recorder(StdMutex::new(Vec::new())));
    let me = SessionRoom::connect(
        "s-a",
        &path,
        letibot_room::DEFAULT_PROJECT,
        None,
        Arc::new(|_: &str| {}),
        Some(rec.clone()),
    )
    .expect("s-a connects");

    // No lease: nothing was running. The act is a start, and the note says so - the key did
    // not empty the queue, the room had already gone.
    let said = me.restart().expect("the supervisor answers");
    assert!(
        said.contains("read back from the store"),
        "the act says where the queues come back from: {said}"
    );
    let frames = rec.0.lock().unwrap().clone();
    assert_eq!(frames.len(), 1, "one act, one frame");
    assert_eq!(frames[0].by, "s-a");
    assert!(!frames[0].was_running, "there was nothing to stop");
    assert!(
        frames[0].why.contains("pressed restart"),
        "{}",
        frames[0].why
    );
    let note = letibot_room::read_marker(&path).expect("the note is written");
    assert_eq!(note.cause, Cause::Daemon);
    assert_eq!(note.by, "s-a");
    assert_eq!(note.room_started_ms, 0, "there is no lease to read");

    // A live lease: this IS a restart, and the note names the session and the room it made.
    // The pane's reader polls once a second and reads a room it has not seen as a crash, so
    // this link is told the instance first - the act must not race a note about itself.
    *me.seen.lock().unwrap() = Some(7000);
    let lease = letibot_room::CoordinatorLease::owned_by(path.with_extension("lease"), "box/1/1");
    std::fs::write(
        lease.path(),
        format!("box/1/1\t{}\t5000\t7000\n", std::process::id()),
    )
    .unwrap();
    me.restart().expect("the supervisor answers again");
    let frames = rec.0.lock().unwrap().clone();
    assert_eq!(frames.len(), 2);
    assert!(frames[1].was_running, "there was a room to stop");
    let note = letibot_room::read_marker(&path).expect("the note is rewritten");
    assert_eq!(note.cause, Cause::Asked);
    assert_eq!(note.room_started_ms, 7000, "the room this note is about");
    assert!(
        note.sentence(0).contains("pressed restart"),
        "{}",
        note.sentence(0)
    );

    drop(me);
    server.stop();
}

/// A supervisor that refuses leaves the room alone and says why - and writes no note
/// because a restart that did not happen is not a restart.
#[test]
fn a_refused_restart_is_a_sentence_and_leaves_no_note() {
    struct Refuse;
    impl RoomSupervisor for Refuse {
        fn restart(&self, _frame: &RestartFrame) -> Result<String, String> {
            Err("`letibot --room --stop` answered exit status: 1: no room on this box".into())
        }
    }

    let dir = std::env::temp_dir().join(format!("letibot-room-norestart-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("room.sock");
    let server =
        letibot_room::Server::serve(&path, letibot_room::Room::new()).expect("a room to talk to");
    let me = SessionRoom::connect(
        "s-a",
        &path,
        letibot_room::DEFAULT_PROJECT,
        None,
        Arc::new(|_: &str| {}),
        Some(Arc::new(Refuse)),
    )
    .expect("s-a connects");

    let why = me.restart().expect_err("the supervisor refused");
    assert!(why.contains("no room on this box"), "{why}");
    assert_eq!(
        letibot_room::read_marker(&path),
        None,
        "a restart that did not happen is not a restart"
    );

    drop(me);
    server.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A host with no supervisor refuses in words, and the sentence says which door is which
/// the launcher starts coordinators, and the room's own daemon is never given one.
#[test]
fn a_host_with_no_supervisor_refuses_the_restart_in_words() {
    let dir = std::env::temp_dir().join(format!("letibot-room-nosup-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("room.sock");
    let server =
        letibot_room::Server::serve(&path, letibot_room::Room::new()).expect("a room to talk to");
    let me = SessionRoom::connect(
        "s-a",
        &path,
        letibot_room::DEFAULT_PROJECT,
        None,
        Arc::new(|_: &str| {}),
        None,
    )
    .expect("s-a connects");
    let why = me.restart().expect_err("no supervisor, no restart");
    assert!(why.contains("no supervisor"), "{why}");
    assert!(why.contains("letibot --room"), "{why}");
    assert!(why.contains("kill the feed that reports it"), "{why}");
    drop(me);
    server.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A room that came back with nobody asking is written as a crash - once, and never over an
/// act's own note. The instance is the lease's `started_ms`, which is the only trace a room
/// leaves when it is not answering.
#[test]
fn an_instance_that_changes_with_nobody_asking_is_a_crash_and_an_act_is_not_overwritten() {
    let dir = std::env::temp_dir().join(format!("letibot-room-crash-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("room.sock");
    let server =
        letibot_room::Server::serve(&path, letibot_room::Room::new()).expect("a room to talk to");
    let lease = path.with_extension("lease");
    std::fs::write(
        &lease,
        format!("box/1/1\t{}\t5000\t7000\n", std::process::id()),
    )
    .unwrap();
    let me = SessionRoom::connect(
        "s-a",
        &path,
        letibot_room::DEFAULT_PROJECT,
        None,
        Arc::new(|_: &str| {}),
        None,
    )
    .expect("s-a connects");
    assert_eq!(letibot_room::read_marker(&path), None, "nothing yet");

    // The room is a new one: a different `started_ms`, and no note accounting for it.
    std::fs::write(
        &lease,
        format!("box/1/1\t{}\t9000\t9001\n", std::process::id()),
    )
    .unwrap();
    me.note_instance();
    let note = letibot_room::read_marker(&path).expect("the crash is written");
    assert_eq!(note.cause, Cause::Crashed);
    assert_eq!(note.room_started_ms, 9001);
    assert!(
        note.sentence(9_500).contains("nobody asked"),
        "{}",
        note.sentence(9_500)
    );

    // An act's note is not overwritten by the observation - a marker that already claims
    // the room the reader is looking at is left alone.
    let asked = Marker::new(9_100, 9001, Cause::Asked).by("s-a");
    asked.write(&letibot_room::marker_path(&path)).unwrap();
    *me.seen.lock().unwrap() = Some(7000);
    me.note_instance();
    assert_eq!(
        letibot_room::read_marker(&path),
        Some(asked),
        "the act's note stands"
    );

    drop(me);
    server.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A session's picture, end to end: the file is read here, uploaded in the room's parts, and the
/// message that follows names it - so a model that says "look at this" hands over the bytes.
#[test]
fn a_say_with_an_attachment_uploads_it_and_references_it() {
    let id = "s-a-attach";
    let dir = std::env::temp_dir().join(format!("letibot-room-attach-link-{}", std::process::id()));
    // A killed run leaves a socket file behind, and binding it again is a refusal rather than a
    // fresh room: the fixture starts from nothing.
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("shots")).unwrap();
    let shot = dir.join("shots/shot.png");
    std::fs::write(&shot, b"\x89PNG\r\n\x1a\n").unwrap();
    let path = dir.join("room.sock");
    let server =
        letibot_room::Server::serve(&path, letibot_room::Room::new()).expect("a room to talk to");
    let bell: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|_: &str| {});
    let link = SessionRoom::connect(id, &path, letibot_room::DEFAULT_PROJECT, None, bell, None)
        .expect("a session link");

    let said = link
        .call(
            "say",
            &serde_json::json!({"room": "host", "text": "look at this",
                "attachments": [shot.to_string_lossy()]}),
        )
        .expect("the say");
    assert!(said.starts_with("said #"), "{said}");
    let read = link
        .client
        .request(&Request::Read {
            room: "host".into(),
            since: 0,
            last: Some(5),
            thread: None,
            before: None,
        })
        .expect("a read of what was said");
    let Response::History(history) = read else {
        panic!("the room answered a frame that is not a history: {read:?}")
    };
    let message = history.rows.last().expect("the message that was said");
    assert_eq!(message.attachments.len(), 1, "{message:?}");
    let picture = &message.attachments[0];
    assert_eq!(picture.name, "shot.png");
    assert_eq!(picture.mime, "image/png");
    assert_eq!(picture.bytes, 8);
    assert!(
        message
            .text
            .contains(&format!("![shot.png](blob:{})", picture.id)),
        "the words reference the picture: {:?}",
        message.text
    );

    // A path the tool did not resolve is refused by the daemon too: the daemon reads no
    // directory of its own choosing.
    let why = link
        .call(
            "say",
            &serde_json::json!({"room": "host", "text": "sneaky",
                "attachments": ["shots/shot.png"]}),
        )
        .expect_err("a relative path is refused");
    assert!(why.contains("absolute"), "{why}");
    server.stop();
    let _ = std::fs::remove_dir_all(&dir);
}
