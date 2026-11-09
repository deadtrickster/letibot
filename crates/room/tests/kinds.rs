//! The `say` kinds, the per-sender budget, the depth cap on agent-only threads, the ranked digest
//! and the counters they share - driven through a real coordinator socket, except where the thing
//! under test is the decision itself.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use letibot_room::client::Client;
use letibot_room::server::Server;
use letibot_room::{
    Event, HOST_ROOM, Message, Ref, Refused, Request, Response, Room, SayKind, Seat, Speaker, chat,
    room_name,
};

const A: &str = "aaaaaaaaaaaa";

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Scratch {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "letibot-room-kinds-{}-{}",
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
    room: String,
    heard: Receiver<(Message, bool)>,
}

impl Seated {
    fn call(&self, r: Request) -> Response {
        self.client.request(&r).expect("an answer")
    }

    /// A `say` of one kind into this seat's project room, with a `to` when one is named.
    fn say(&self, id: &str, kind: SayKind, text: &str, to: Option<&str>) -> Response {
        self.call(Request::Say {
            id: id.into(),
            room: self.room.clone(),
            text: text.into(),
            to: to.map(str::to_string),
            thread: None,
            refs: Vec::new(),
            attachments: Vec::new(),
            kind,
        })
    }

    /// A reply of one kind into the thread rooted at `thread`.
    fn reply(&self, id: &str, thread: u64, text: &str) -> Response {
        self.call(Request::Say {
            id: id.into(),
            room: self.room.clone(),
            text: text.into(),
            to: None,
            thread: Some(thread),
            refs: Vec::new(),
            attachments: Vec::new(),
            kind: SayKind::Post,
        })
    }

    fn signals(&self) -> letibot_room::Scorecard {
        match self.call(Request::Signals) {
            Response::Signals { card } => card,
            other => panic!("{other:?}"),
        }
    }

    /// Everything pushed to this seat since the last call, with whether it named the seat.
    fn heard(&self) -> Vec<(Message, bool)> {
        let mut out = Vec::new();
        while let Ok(m) = self.heard.recv_timeout(Duration::from_millis(400)) {
            out.push(m);
        }
        out
    }
}

fn seat(server: &Server, name: &str, kind: Seat) -> Seated {
    let (tx, heard) = mpsc::channel();
    let client = Client::connect_as(
        server.path(),
        Speaker::top_level(name),
        room_name(&server.name(), A),
        kind,
        Arc::new(|_: &Event| {}),
    )
    .unwrap_or_else(|e| panic!("`{name}` must connect: {e}"));
    let tx = std::sync::Mutex::new(tx);
    client.on_chat(Arc::new(move |m, names| {
        let _ = tx.lock().unwrap().send((m.clone(), names));
    }));
    Seated {
        client,
        room: chat::project_room(A),
        heard,
    }
}

fn said(r: Response) -> Message {
    match r {
        Response::Said { message, .. } => message,
        other => panic!("expected the message to be said: {other:?}"),
    }
}

fn refused(r: Response) -> Refused {
    match r {
        Response::Refused { why, .. } => why,
        other => panic!("expected a refusal: {other:?}"),
    }
}

fn counted(card: &letibot_room::Scorecard, word: &str) -> u64 {
    card.counts.get(word).copied().unwrap_or(0)
}

/// Every frame written before kinds still decodes as an ordinary post, and a word this build does
/// not know is an ordinary post too: a newer client says something rather than failing to parse.
#[test]
fn an_old_frame_and_an_unknown_word_both_decode_as_an_ordinary_line() {
    let old: Request = serde_json::from_str(
        r#"{"type":"say","id":"1","room":"host","text":"the box restarts at six"}"#,
    )
    .expect("a frame from before kinds");
    match old {
        Request::Say { kind, .. } => assert_eq!(kind, SayKind::Post),
        other => panic!("{other:?}"),
    }
    let unknown: Request =
        serde_json::from_str(r#"{"type":"say","id":"2","text":"hello","kind":"shout"}"#)
            .expect("a word from a newer build");
    match unknown {
        Request::Say { kind, .. } => {
            assert_eq!(kind, SayKind::Other);
            // An unknown word asks nothing of anybody, so it is read as the line it is.
            assert_eq!(kind.word(), "post");
            assert!(!kind.recorded());
        }
        other => panic!("{other:?}"),
    }
    for word in ["post", "fyi", "ask", "blocker", "decision", "announce"] {
        assert_eq!(SayKind::of(word).word(), word, "`{word}` round-trips");
    }
}

/// The kind rides the refs, so what the room stores is what the reader is told: a post and an
/// announcement record nothing, and an `fyi`, a `blocker` and a `decision` come back as themselves.
#[test]
fn a_kind_is_read_back_from_the_message_the_room_stored() {
    let dir = Scratch::new();
    let server = Server::serve(&dir.0.join("room.sock"), Room::new()).expect("a coordinator");
    let a = seat(&server, "s-a", Seat::Session);

    let post = said(a.say("p", SayKind::Post, "the build is green", None));
    assert_eq!(post.say_kind(), SayKind::Post);
    assert!(!post.line().contains(", blocker"), "{}", post.line());

    let fyi = said(a.say("f", SayKind::Fyi, "the port moved to 5433", None));
    assert_eq!(fyi.say_kind(), SayKind::Fyi);
    assert!(fyi.line().contains(", fyi"), "{}", fyi.line());

    let blocker = said(a.say("b", SayKind::Blocker, "the disk is full", None));
    assert_eq!(blocker.say_kind(), SayKind::Blocker);
    assert!(blocker.line().contains(", blocker"), "{}", blocker.line());

    let decision = said(a.say("d", SayKind::Decision, "land m-3 before m-4", None));
    assert_eq!(decision.say_kind(), SayKind::Decision);
    assert!(
        decision.line().contains(", decision"),
        "{}",
        decision.line()
    );

    // The refs a reader is shown are the rows the message points at, not the kind.
    for m in [&fyi, &blocker, &decision] {
        assert!(
            m.rows_refs().next().is_none(),
            "the kind is not shown as a ref: {}",
            m.line()
        );
    }
    // And the store keeps it: a read after the fact still knows what each line asked.
    let rows = match a.call(Request::Read {
        room: chat::project_room(A),
        since: 0,
        last: None,
        thread: None,
        before: None,
    }) {
        Response::History(h) => h.rows,
        other => panic!("{other:?}"),
    };
    let kinds: Vec<SayKind> = rows.iter().map(|m| m.say_kind()).collect();
    assert_eq!(
        kinds,
        vec![
            SayKind::Post,
            SayKind::Fyi,
            SayKind::Blocker,
            SayKind::Decision
        ]
    );
}

/// `kind: ask` is the room's own ask: it must name somebody, and then it is held for them, appears
/// among the asks they owe, and is settled by `answer` and by nothing else.
#[test]
fn an_ask_says_who_it_is_for_and_is_held_for_them() {
    let dir = Scratch::new();
    let server = Server::serve(&dir.0.join("room.sock"), Room::new()).expect("a coordinator");
    let a = seat(&server, "s-a", Seat::Session);
    let b = seat(&server, "s-b", Seat::Session);

    let nobody = refused(a.say("n", SayKind::Ask, "which port is free?", None));
    assert_eq!(nobody, Refused::AskNamesNobody);
    assert!(
        nobody.to_string().contains("named nobody"),
        "the refusal says what is missing: {nobody}"
    );

    let asked = match a.say("q", SayKind::Ask, "which port is free?", Some("s-b")) {
        Response::Asked { ask, .. } => ask,
        other => panic!("expected a held ask: {other:?}"),
    };
    assert_eq!(asked.target, "s-b");
    assert_eq!(asked.asker, "s-a");
    assert!(asked.open());
    let owed = match b.call(Request::Asks) {
        Response::Asks { of_me, .. } => of_me,
        other => panic!("{other:?}"),
    };
    assert_eq!(owed.len(), 1, "the target owes the answer: {owed:?}");
    assert_eq!(owed[0].seq, asked.seq);
    // The question on the wire is the ask's message, and it reads as an ask to its reader too.
    let rows = match b.call(Request::Read {
        room: chat::project_room(A),
        since: 0,
        last: None,
        thread: None,
        before: None,
    }) {
        Response::History(h) => h.rows,
        other => panic!("{other:?}"),
    };
    let question = rows
        .iter()
        .find(|m| m.seq == asked.seq)
        .unwrap_or_else(|| panic!("the question is not in the room: {rows:?}"));
    assert!(question.refs.iter().any(|r| r.kind == "ask"));
    assert_eq!(question.to.as_deref(), Some("s-b"));
}

/// An `fyi` asks nothing of anybody: it wakes nobody it sits in and only the dm it is. A `blocker`
/// is the opposite - it rings the room it was said in and the target it names.
#[test]
fn an_fyi_wakes_nobody_and_a_blocker_wakes_its_room_and_its_target() {
    let dir = Scratch::new();
    let server = Server::serve(&dir.0.join("room.sock"), Room::new()).expect("a coordinator");
    let a = seat(&server, "s-a", Seat::Session);
    let _b = seat(&server, "s-b", Seat::Session);
    let h = HOST_ROOM;

    let fyi = said(a.say("f", SayKind::Fyi, "the port moved to 5433", None));
    // An `fyi` reaches its room and never rings: its reason for being read is `passing by`.
    assert!(fyi.wakes("s-b", false, |r| r == chat::project_room(A)));
    assert!(
        !fyi.wakes("s-b", false, |_| false),
        "an fyi reached a session of another room"
    );
    assert_eq!(fyi.reach(false), chat::Reach::Passing);
    assert_eq!(fyi.reach(true), chat::Reach::Passing);

    let blocker = said(a.say("b", SayKind::Blocker, "the disk is full", None));
    assert!(
        blocker.wakes("s-b", false, |r: &str| r == chat::project_room(A)),
        "a blocker did not wake the room it was said in"
    );
    assert_eq!(blocker.reach(false), chat::Reach::Addressed);
    assert!(
        !blocker.wakes("s-b", false, |_| false),
        "a blocker woke a session in another room"
    );

    // A dm is addressed to one person, so even an fyi wakes that person and nobody else.
    let fyi_dm = said(a.call(Request::Say {
        id: "fd".into(),
        room: h.into(),
        text: "the port moved to 5433".into(),
        to: Some("s-b".into()),
        thread: None,
        refs: Vec::new(),
        attachments: Vec::new(),
        kind: SayKind::Fyi,
    }));
    assert_eq!(fyi_dm.room, chat::DM_ROOM);
    assert_eq!(fyi_dm.reach(true), chat::Reach::Dm);
    assert_eq!(fyi_dm.reach(false), chat::Reach::Passing);
}

/// A blocker is the operator's to unblock: it reaches the person's connection as if it named them,
/// while an ordinary line to the same room is merely pushed. And it is counted, once, as a flag.
#[test]
fn a_blocker_flags_the_operator_and_is_counted() {
    let dir = Scratch::new();
    let server = Server::serve(&dir.0.join("room.sock"), Room::new()).expect("a coordinator");
    let a = seat(&server, "s-a", Seat::Session);
    let op = seat(&server, "operator", Seat::Operator);
    let b = seat(&server, "s-b", Seat::Session);

    said(a.say("p", SayKind::Post, "the build is green", None));
    let plain = op.heard();
    let (_m, names_you) = plain
        .iter()
        .find(|(m, _)| m.text.contains("build is green"))
        .unwrap_or_else(|| panic!("the post never reached the operator: {plain:?}"));
    assert!(!names_you, "an ordinary line flagged the operator");

    let blocker = said(a.say("b", SayKind::Blocker, "the disk is full", None));
    let pushed = op.heard();
    let (m, names_you) = pushed
        .iter()
        .find(|(m, _)| m.seq == blocker.seq)
        .unwrap_or_else(|| panic!("the blocker never reached the operator: {pushed:?}"));
    assert!(names_you, "the blocker did not flag the operator");
    assert!(m.line().contains(", blocker"), "{}", m.line());
    assert!(b.heard().iter().any(|(m, _)| m.seq == blocker.seq));

    let card = a.signals();
    assert_eq!(counted(&card, "blocker_flagged"), 1);
}

/// The budget is per sender and per room: a burst of ten is fine, the eleventh is refused with the
/// number and the wait, the operator is exempt, and the refusal is counted.
#[test]
fn the_budget_refuses_past_its_number_and_says_when_the_next_post_is_allowed() {
    let dir = Scratch::new();
    let server = Server::serve(&dir.0.join("room.sock"), Room::new()).expect("a coordinator");
    server.with_budget(3);
    let a = seat(&server, "s-a", Seat::Session);
    let op = seat(&server, "operator", Seat::Operator);

    for n in 0..3 {
        said(a.say(
            &format!("p{n}"),
            SayKind::Post,
            &format!("line {n} of three"),
            None,
        ));
    }
    let why = refused(a.say("p3", SayKind::Post, "the fourth line", None));
    match &why {
        Refused::Budget {
            posts,
            refill_ms,
            wait_ms,
        } => {
            assert_eq!(*posts, 3);
            assert_eq!(*refill_ms, letibot_room::budget::BUDGET_REFILL_MS);
            assert!(*wait_ms <= *refill_ms);
        }
        other => panic!("{other:?}"),
    }
    let sentence = why.to_string();
    assert!(sentence.contains("budget is 3 posts"), "{sentence}");
    assert!(sentence.contains("next post is allowed in"), "{sentence}");
    assert!(sentence.contains("etiquette"), "{sentence}");
    // The refusal is counted, and the room is that many posts the shorter for it.
    assert_eq!(counted(&a.signals(), "budget_refused"), 1);

    // A person is not rationed, and neither is the room's own second sender.
    said(op.say("o1", SayKind::Post, "the operator is not rationed", None));
    said(seat(&server, "s-b", Seat::Session).say("b1", SayKind::Post, "another sender", None));
    // One token comes back with the clock: the refill is the module's, so wait for less than it.
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(
        counted(&a.signals(), "budget_refused"),
        1,
        "a wait spends nothing"
    );
}

/// A thread the operator is not in stops at the depth cap, and the refusal says what to do with the
/// answer instead. A thread the operator has spoken in runs on.
#[test]
fn a_thread_with_no_operator_in_it_stops_at_the_depth_cap() {
    let dir = Scratch::new();
    let server = Server::serve(&dir.0.join("room.sock"), Room::new()).expect("a coordinator");
    server.with_budget(40);
    let a = seat(&server, "s-a", Seat::Session);
    let b = seat(&server, "s-b", Seat::Session);
    let op = seat(&server, "operator", Seat::Operator);

    let root = said(a.say("r", SayKind::Post, "how should the split land?", None));
    let mut last = root.seq;
    // Depth 1 answers the root, so the cap of 6 lets six answers through and refuses the seventh.
    for depth in 1..=chat::MAX_AGENT_DEPTH {
        let who = if depth % 2 == 1 { &b } else { &a };
        let m = said(who.reply(&format!("d{depth}"), last, &format!("answer {depth}")));
        assert_eq!(m.thread, Some(last));
        last = m.seq;
    }
    let why = refused(a.reply("d7", last, "and one more thing"));
    match &why {
        Refused::ThreadTooDeep {
            depth,
            max,
            root: at,
        } => {
            assert_eq!(*depth, chat::MAX_AGENT_DEPTH as u64 + 1);
            assert_eq!(*max, chat::MAX_AGENT_DEPTH as u64);
            assert_eq!(*at, root.seq);
        }
        other => panic!("{other:?}"),
    }
    let sentence = why.to_string();
    assert!(
        sentence.contains("file a row or ask the operator"),
        "{sentence}"
    );
    assert_eq!(counted(&a.signals(), "depth_capped"), 1);

    // The operator's word in a thread makes it theirs, and it takes replies at any depth.
    let theirs = said(a.say("r2", SayKind::Post, "which of the two lands first?", None));
    said(op.reply("op", theirs.seq, "land the smaller one first"));
    let mut last = theirs.seq;
    for depth in 1..=(chat::MAX_AGENT_DEPTH + 2) {
        let who = if depth % 2 == 1 { &b } else { &a };
        let m = said(who.reply(
            &format!("t{depth}"),
            last,
            &format!("thread answer {depth}"),
        ));
        last = m.seq;
    }
}

/// The said-since digest is RANKED: what the reader owes, then what stops somebody, then what was
/// concluded, then lines touching the room's rows and files, and the counts after all of them.
#[test]
fn the_digest_ranks_the_asks_blockers_decisions_and_their_rows_before_the_counts() {
    let dir = Scratch::new();
    let server = Server::serve(&dir.0.join("room.sock"), Room::new()).expect("a coordinator");
    let a = seat(&server, "s-a", Seat::Session);
    let b = seat(&server, "s-b", Seat::Session);

    said(a.say("p", SayKind::Post, "morning, all", None));
    said(a.say("b", SayKind::Blocker, "the disk is full", None));
    said(a.say("d", SayKind::Decision, "land m-3 before m-4", None));
    said(a.call(Request::Say {
        id: "t".into(),
        room: chat::project_room(A),
        text: "the split lives in crates/room/src/chat.rs".into(),
        to: None,
        thread: None,
        refs: vec![Ref {
            kind: "todo".into(),
            id: "s-a/land the split".into(),
        }],
        attachments: Vec::new(),
        kind: SayKind::Post,
    }));
    let asked = match a.say("q", SayKind::Ask, "which port is free?", Some("s-b")) {
        Response::Asked { ask, .. } => ask,
        other => panic!("{other:?}"),
    };
    let owed = match b.call(Request::Asks) {
        Response::Asks { of_me, .. } => of_me,
        other => panic!("{other:?}"),
    };
    let rows = match b.call(Request::Read {
        room: chat::project_room(A),
        since: 0,
        last: None,
        thread: None,
        before: None,
    }) {
        Response::History(h) => h.rows,
        other => panic!("{other:?}"),
    };
    let text = chat::summarise(&chat::project_room(A), "#0", &rows, &[], &owed)
        .expect("a digest of what there is");
    let at = |needle: &str| {
        text.find(needle)
            .unwrap_or_else(|| panic!("`{needle}` is not in the digest:\n{text}"))
    };
    let ask = at("ask #");
    let blocker = at("blocker #");
    let decision = at("decision #");
    let touching = at("touching #");
    let counts = at("since #0");
    assert!(
        text[ask..].starts_with(&format!("ask #{} ", asked.seq)),
        "{text}"
    );
    assert!(
        ask < blocker && blocker < decision && decision < touching && touching < counts,
        "the digest is not ranked:\n{text}"
    );
    // The seqs are kept, so every ranked line can be read or answered.
    assert!(text.contains(&format!("blocker #{}", 2)), "{text}");
    assert!(text.contains("the disk is full"), "{text}");
    assert!(text.contains("land m-3 before m-4"), "{text}");
    assert!(text.contains("crates/room/src/chat.rs"), "{text}");
    assert!(text.contains("asks you \"which port is free?\""), "{text}");
}

/// The scorecard counts a wake against the kind that rang it, so the room can say which kinds are
/// worth a turn.
#[test]
fn the_scorecard_counts_wakes_by_the_kind_that_rang_them() {
    let dir = Scratch::new();
    let server = Server::serve(&dir.0.join("room.sock"), Room::new()).expect("a coordinator");
    let a = seat(&server, "s-a", Seat::Session);
    let b = seat(&server, "s-b", Seat::Session);

    let blocker = said(a.say("b", SayKind::Blocker, "the disk is full", None));
    let decision = said(a.say("d", SayKind::Decision, "land m-3 before m-4", None));
    for (who, seq) in [(&b, blocker.seq), (&b, decision.seq)] {
        who.call(Request::WakeEnded {
            seqs: vec![seq],
            cheap: false,
            silent: false,
            tokens: 100,
        });
    }
    let card = a.signals();
    let row = |key: &str| {
        card.kinds
            .iter()
            .find(|r| r.key == key)
            .unwrap_or_else(|| panic!("no `{key}` row in {:?}", card.kinds))
            .clone()
    };
    assert_eq!(row("blocker").wakes, 1);
    assert_eq!(row("decision").wakes, 1);
    // Nothing followed either one and neither window has closed, so both are still open.
    assert_eq!(row("blocker").pending, 1);
    assert_eq!(row("decision").pending, 1);
    assert!(
        row("blocker").line().contains("silent 0/1"),
        "{:?}",
        row("blocker")
    );
}
