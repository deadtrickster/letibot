//! Asks on a real coordinator socket: A asks B and B's answer settles it; a session that is gone
//! only ever escalates, never settles; and every refusal the design names happens before anything is
//! written, so the asking session learns it in the same call.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use letibot_room::client::Client;
use letibot_room::server::Server;
use letibot_room::{
    AskState, AskView, Event, Group, HOST_ROOM, Member, OPERATOR, Refused, Request, Response, Room,
    Seat, Speaker, room_name,
};

const A: &str = "aaaaaaaaaaaa";
const B: &str = "bbbbbbbbbbbb";

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Scratch {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "letibot-room-ask-{}-{}",
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

    fn ask(&self, id: &str, to: &str, question: &str) -> Response {
        self.call(Request::Ask {
            id: id.into(),
            room: String::new(),
            to: to.into(),
            path: String::new(),
            topic: String::new(),
            question: question.into(),
        })
    }

    fn reply(&self, id: &str, ask: u64, text: &str) -> Response {
        self.call(Request::Reply {
            id: id.into(),
            ask,
            text: text.into(),
        })
    }

    fn decline(&self, id: &str, ask: u64, text: &str) -> Response {
        self.call(Request::Decline {
            id: id.into(),
            ask,
            text: text.into(),
        })
    }

    fn withdraw(&self, id: &str, ask: u64) -> Response {
        self.call(Request::Withdraw { id: id.into(), ask })
    }

    fn asks(&self) -> (Vec<AskView>, Vec<AskView>) {
        match self.call(Request::Asks) {
            Response::Asks { by_me, of_me } => (by_me, of_me),
            other => panic!("expected the asks: {other:?}"),
        }
    }

    fn next(&self) -> letibot_room::Message {
        self.heard
            .recv_timeout(Duration::from_secs(5))
            .expect("a pushed message")
    }

    fn next_seq(&self, seq: u64) -> letibot_room::Message {
        loop {
            let got = self.next();
            if got.seq == seq {
                return got;
            }
        }
    }
}

fn coordinator(s: &Scratch) -> Server {
    Server::serve(&s.0.join("room.sock"), Room::new()).expect("a coordinator")
}

fn stored(s: &Scratch) -> Server {
    let server = coordinator(s);
    server.with_chat_store(&s.0.join("chat.json"));
    server
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

fn sub(session: &str, handle: &str, parent: &str) -> Member {
    Member {
        session: session.into(),
        label: handle.into(),
        handle: handle.into(),
        group: Group::Sub,
        parent: Some(parent.into()),
        depth: 1,
        live: true,
    }
}

fn tell(s: &Seated, rows: Vec<Member>) {
    s.call(Request::Members {
        rows,
        root: String::new(),
    });
}

/// Wait for a fact about the room, so a close on the peer's side is not raced.
fn until(what: &str, mut cond: impl FnMut() -> bool) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        if cond() {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("timed out waiting for {what}");
}

/// A question is a chat message addressed to the target, and its `seq` is the ask's id. The answer
/// is a thread reply in the same room, and it is the settle.
#[test]
fn a_question_is_a_message_and_its_answer_settles_the_ask() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let a = seat(&server, "s-a", A, Seat::Session);
    let b = seat(&server, "s-b", A, Seat::Session);

    let ask = asked(a.ask("q1", "s-b", "which port?"));
    assert_eq!(ask.asker, "s-a");
    assert_eq!(ask.target, "s-b");
    assert_eq!(ask.question, "which port?");
    assert_eq!(ask.room, letibot_room::project_room(A));
    assert!(ask.open(), "{ask:?}");

    // B is woken by the question, and the message names it.
    let question = b.next();
    assert_eq!(question.seq, ask.seq);
    assert_eq!(question.text, "which port?");
    assert_eq!(question.to.as_deref(), Some("s-b"));
    assert_eq!(question.refs[0].kind, "ask");

    // B's side of the room is told the ask is owed, with nothing gone.
    let (_, of_me) = b.asks();
    assert_eq!(of_me.len(), 1);
    assert!(of_me[0].open());
    assert_eq!(of_me[0].gone_since_ms, None);

    let settled = asked(b.reply("r1", ask.seq, "5433"));
    assert!(
        matches!(&settled.state, AskState::Answered { text, ms } if text == "5433" && *ms > 0),
        "{settled:?}"
    );

    // A reads the answer back from its own half, and the answer is in the thread.
    let (by_me, _) = a.asks();
    assert_eq!(by_me.len(), 1);
    assert!(matches!(
        &by_me[0].state,
        AskState::Answered { text, .. } if text == "5433"
    ));
    let answer = a.next_seq(ask.seq + 1);
    assert_eq!(answer.thread, Some(ask.seq));
    assert_eq!(answer.to.as_deref(), Some("s-a"));
    assert_eq!(answer.text, "`s-b` answered your ask #1: 5433");
    assert_eq!(answer.room, ask.room, "an answer stays in the ask's room");
}

/// Only the session that was asked answers, and only while the ask is open. A stranger's answer is
/// refused with the ask's target named, and the ask is untouched.
#[test]
fn only_the_session_that_was_asked_may_answer() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let a = seat(&server, "s-a", A, Seat::Session);
    let b = seat(&server, "s-b", A, Seat::Session);
    let c = seat(&server, "s-c", A, Seat::Session);
    let ask = asked(a.ask("q1", "s-b", "which port?"));

    assert!(matches!(
        refused(c.reply("r1", ask.seq, "3306")),
        Refused::AskNotTheTarget { ask: id, target, .. } if id == ask.seq && target == "s-b"
    ));
    let (by_me, _) = a.asks();
    assert!(by_me[0].open(), "a stranger's words settle nothing");

    asked(b.reply("r2", ask.seq, "5433"));
    // A second answer to one question is not a second settle.
    assert!(matches!(
        refused(b.reply("r3", ask.seq, "5434")),
        Refused::AskSettled { ask: id, .. } if id == ask.seq
    ));
    assert!(matches!(
        refused(b.reply("r4", 9_999, "nobody asked me")),
        Refused::UnknownAsk { ask: 9_999 }
    ));
}

/// B's connection closing is not B being shut down: the ask stays open, and a return inside the
/// grace is invisible - nothing escalated, nothing settled.
#[test]
fn a_target_back_inside_the_grace_is_still_open_and_never_escalated() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let a = seat(&server, "s-a", A, Seat::Session);
    let b = seat(&server, "s-b", A, Seat::Session);
    tell(&b, vec![top("s-b", "bbe")]);
    let ask = asked(a.ask("q1", "s-b", "which port?"));

    drop(b);
    until("`s-b` to be gone", || server.connections() == 1);
    server.tick(server.started_ms());
    let (by_me, _) = a.asks();
    assert!(by_me[0].open(), "a closed connection settles nothing");
    assert!(!by_me[0].escalated, "and it is not escalated yet");
    assert!(by_me[0].gone_since_ms.is_some(), "but the room knows");

    let b = seat(&server, "s-b", A, Seat::Session);
    let (by_me, _) = a.asks();
    assert_eq!(by_me[0].gone_since_ms, None, "a reconnect clears it");
    assert!(!by_me[0].escalated);
    assert_eq!(server.gone_since("s-b"), None);

    // And B can still answer: the ask was never touched.
    asked(b.reply("r1", ask.seq, "5433"));
}

/// Past the grace the ask is escalated - the operator is owed a restart - and it is still open. The
/// grace is the knob, so this needs no sleeping.
#[test]
fn a_target_gone_past_the_grace_escalates_and_a_reconnect_clears_it() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    server.with_ask_grace(0);
    let a = seat(&server, "s-a", A, Seat::Session);
    let b = seat(&server, "s-b", A, Seat::Session);
    tell(&b, vec![top("s-b", "bbe")]);
    let ask = asked(a.ask("q1", "s-b", "which port?"));
    assert!(!ask.escalated, "B is connected, so nothing is owed");

    drop(b);
    until("`s-b` to be gone", || server.connections() == 1);
    server.tick(server.started_ms());
    let (by_me, _) = a.asks();
    assert!(by_me[0].open(), "timeouts only escalate, they never settle");
    assert!(
        by_me[0].escalated,
        "the operator is owed a restart: {by_me:?}"
    );

    // B comes back: the escalation is cleared, and the ask still waits for the answer.
    let b = seat(&server, "s-b", A, Seat::Session);
    let (by_me, _) = a.asks();
    assert!(!by_me[0].escalated, "{by_me:?}");
    assert!(by_me[0].open());
    assert_eq!(by_me[0].gone_since_ms, None);
    let (_, of_me) = b.asks();
    assert!(!of_me[0].escalated);
}

/// A refused answer says where the ask stands, and a settle reaches the asker as one line that
/// says it is settled, by whom and with what - not a bare reply it could take for more chat.
#[test]
fn a_settle_says_so_to_the_asker_and_a_wrong_answerer_learns_the_asks_state() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let a = seat(&server, "s-a", A, Seat::Session);
    let b = seat(&server, "s-b", A, Seat::Session);
    tell(&a, vec![top("s-a", "mac"), top("s-b", "rano")]);

    let one = asked(a.ask("q1", "rano", "which port?"));
    let two = asked(a.ask("q2", "rano", "which user?"));
    let state_of = |r: Response| refused(r).to_string();
    // The asker answering its own ask is told it is still open, and whose it is.
    let open = state_of(a.reply("r0", one.seq, "never mind"));
    assert_eq!(
        open,
        format!(
            "ask #{} was put to `rano`; it is open, and only `rano` may answer or decline it",
            one.seq
        )
    );

    asked(b.reply("r1", one.seq, "5433"));
    let answer = a.next_seq(one.seq + 2);
    assert_eq!(
        answer.text,
        format!("`rano` answered your ask #{}: 5433", one.seq)
    );
    assert_eq!(answer.to.as_deref(), Some("mac"), "addressed to the asker");
    asked(b.decline("d1", two.seq, "not my area"));
    let declined = a.next_seq(answer.seq + 1);
    assert_eq!(
        declined.text,
        format!("`rano` declined your ask #{}: not my area", two.seq)
    );

    assert_eq!(
        state_of(a.reply("r2", one.seq, "again")),
        format!("ask #{} was put to `rano`; it was answered: 5433", one.seq)
    );
    assert_eq!(
        state_of(a.decline("r3", two.seq, "again")),
        format!(
            "ask #{} was put to `rano`; it was declined: not my area",
            two.seq
        )
    );
    // And the asker's own list says each one's state.
    let (by_me, _) = a.asks();
    let lines: Vec<String> = by_me.iter().map(|x| x.line()).collect();
    assert!(lines[0].ends_with("- answered: 5433"), "{lines:?}");
    assert!(lines[1].ends_with("- declined: not my area"), "{lines:?}");
}

/// A decline is an answer: the ask ends, and the asker's row learns which of the two it was.
#[test]
fn a_decline_settles_the_ask_and_says_so() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let a = seat(&server, "s-a", A, Seat::Session);
    let b = seat(&server, "s-b", A, Seat::Session);
    let ask = asked(a.ask("q1", "s-b", "which port?"));
    let settled = asked(b.decline("d1", ask.seq, "not my area"));
    assert!(matches!(
        &settled.state,
        AskState::Declined { text, .. } if text == "not my area"
    ));
    let (by_me, _) = a.asks();
    assert_eq!(by_me[0].state.line(), "declined: not my area");
}

/// A deleted session is the one `DOWN`, and only its own connection may say so. Its own asks are
/// withdrawn with it: nothing is left to wait on.
#[test]
fn retire_settles_every_ask_of_that_session_down_and_only_its_own_connection_may_send_it() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let a = seat(&server, "s-a", A, Seat::Session);
    let b = seat(&server, "s-b", A, Seat::Session);
    tell(&b, vec![top("s-b", "bbe")]);
    // Two asks of B, and one of B's own.
    let one = asked(a.ask("q1", "s-b", "which port?"));
    let two = asked(a.ask("q2", "s-b", "and the user?"));
    let theirs = asked(b.ask("q3", "s-a", "ready?"));

    assert!(matches!(
        refused(a.call(Request::Retire {
            id: "x".into(),
            session: "s-b".into(),
        })),
        Refused::NotTheOwners { session } if session == "s-b"
    ));
    // And it is not settled by the attempt.
    let (by_me, _) = a.asks();
    assert!(by_me.iter().all(|x| x.open()));

    match b.call(Request::Retire {
        id: "retire".into(),
        session: "s-b".into(),
    }) {
        Response::Asks { of_me, by_me } => {
            assert!(
                of_me
                    .iter()
                    .all(|x| matches!(x.state, AskState::Down { .. })),
                "{of_me:?}"
            );
            assert!(
                by_me
                    .iter()
                    .all(|x| matches!(&x.state, AskState::Withdrawn { by, .. } if by == "s-b")),
                "{by_me:?}"
            );
        }
        other => panic!("{other:?}"),
    }

    // A's rows learn both: the two it asked are down, the one it was asked was taken back.
    let (by_me, of_me) = a.asks();
    assert_eq!(by_me.len(), 2);
    for ask in &by_me {
        assert!(matches!(ask.state, AskState::Down { .. }), "{ask:?}");
    }
    assert_eq!(of_me.len(), 1);
    assert!(
        matches!(&of_me[0].state, AskState::Withdrawn { by, .. } if by == "s-b"),
        "{:?}",
        of_me[0]
    );
    assert_eq!(of_me[0].seq, theirs.seq);
    assert!(one.seq != two.seq);
}

/// The room outlives the daemons: an ask is in the chat store, so a coordinator restart brings it
/// back - open if it was open, settled if it was settled.
#[test]
fn an_ask_survives_a_coordinator_restart_over_its_store() {
    let dir = Scratch::new();
    let first = stored(&dir);
    let a = seat(&first, "s-a", A, Seat::Session);
    let b = seat(&first, "s-b", A, Seat::Session);
    let open = asked(a.ask("q1", "s-b", "which port?"));
    let settled = asked(a.ask("q2", "s-b", "and the user?"));
    asked(b.reply("r1", settled.seq, "admin"));
    drop(a);
    drop(b);
    first.stop();
    until("the first coordinator to end", || first.connections() == 0);

    let second = stored(&dir);
    let a = seat(&second, "s-a", A, Seat::Session);
    let (by_me, _) = a.asks();
    assert_eq!(by_me.len(), 2, "{by_me:?}");
    let reopened = by_me
        .iter()
        .find(|x| x.seq == open.seq)
        .expect("the open one");
    assert!(reopened.open(), "an open ask is never dropped");
    let reanswered = by_me
        .iter()
        .find(|x| x.seq == settled.seq)
        .expect("the settled one");
    assert!(matches!(
        &reanswered.state,
        AskState::Answered { text, .. } if text == "admin"
    ));
    // And the question is still in the ring, so a thread reply has a root to hang from.
    match a.call(Request::Read {
        room: open.room.clone(),
        since: 0,
        last: None,
        thread: Some(open.seq),
        before: None,
    }) {
        Response::History(h) => assert!(!h.rows.is_empty(), "the question is still there"),
        other => panic!("{other:?}"),
    }
}

/// The same question twice is one ask: several rows carry one id, and one settle releases them all.
#[test]
fn the_same_question_asked_twice_is_one_ask() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let a = seat(&server, "s-a", A, Seat::Session);
    let b = seat(&server, "s-b", A, Seat::Session);
    let one = asked(a.ask("q1", "s-b", "which port?"));
    let two = asked(a.ask("q2", "s-b", "which port?"));
    assert_eq!(one.seq, two.seq, "one ask, one id");
    let (by_me, _) = a.asks();
    assert_eq!(by_me.len(), 1);
    // A different question is a second ask, and so is a different target.
    let other = asked(a.ask("q3", "s-b", "and the user?"));
    assert_ne!(other.seq, one.seq);
    // A repeat of the request id is answered with the ask it made.
    let again = asked(a.ask("q1", "s-b", "which port?"));
    assert_eq!(again.seq, one.seq);
    let _ = b;
}

/// Every target the design refuses, refused by name before anything is written.
#[test]
fn self_operator_subagent_and_ambiguous_targets_are_refused() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let a = seat(&server, "s-a", A, Seat::Session);
    tell(
        &a,
        vec![
            top("s-a", "arango"),
            top("s-b", "serenedb"),
            top("s-b2", "serenedb"),
            sub("s-child", "kid", "s-b"),
        ],
    );
    assert!(matches!(
        refused(a.ask("q1", "s-a", "me?")),
        Refused::AskSelf
    ));
    assert!(matches!(
        refused(a.ask("q2", OPERATOR, "you?")),
        Refused::AskOperator
    ));
    assert!(matches!(
        refused(a.ask("q3", "s-child", "you?")),
        Refused::AskSubagent { session, parent } if session == "s-child" && parent == "s-b"
    ));
    assert!(matches!(
        refused(a.ask("q4", "serenedb", "you?")),
        Refused::Ambiguous { candidates, .. } if candidates.len() == 2
    ));
    // An unknown name is refused with the handles that do answer, so a typo is fixable.
    match refused(a.ask("q5", "nobody", "you?")) {
        Refused::AskUnknown { name, known } => {
            assert_eq!(name, "nobody");
            assert!(known.contains(&"arango".to_string()), "{known:?}");
        }
        other => panic!("{other:?}"),
    }
    // A session id of another box is not this box's room's to carry.
    assert!(matches!(
        refused(a.ask("q6", "s-abc@lab2x1", "you?")),
        Refused::AskOtherHost { .. }
    ));
    // The refusal is said in the same call, so nothing was written.
    let (by_me, _) = a.asks();
    assert!(by_me.is_empty(), "{by_me:?}");
}

/// A question would be said where the target cannot read it, so it is refused with the room and the
/// way out - `host`, or an `Invite`.
#[test]
fn an_ask_into_a_room_the_target_cannot_see_is_refused() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let a = seat(&server, "s-a", A, Seat::Session);
    let _b = seat(&server, "s-b", B, Seat::Session);
    match refused(a.call(Request::Ask {
        id: "q1".into(),
        room: letibot_room::project_room(A),
        to: "s-b".into(),
        path: String::new(),
        topic: String::new(),
        question: "which port?".into(),
    })) {
        Refused::AskCannotSee { session, room } => {
            assert_eq!(session, "s-b");
            assert_eq!(room, letibot_room::project_room(A));
        }
        other => panic!("{other:?}"),
    }
    // In the host room, every session hears it.
    let ask = asked(a.call(Request::Ask {
        id: "q2".into(),
        room: HOST_ROOM.into(),
        to: "s-b".into(),
        path: String::new(),
        topic: String::new(),
        question: "which port?".into(),
    }));
    assert_eq!(ask.room, HOST_ROOM);
}

/// The asker may take its question back, and so may the operator's seat; nobody else may. The
/// withdrawal is said in the thread, so the target is not left nagged by a question that vanished.
#[test]
fn the_asker_withdraws_and_the_operator_may_too() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let a = seat(&server, "s-a", A, Seat::Session);
    let b = seat(&server, "s-b", A, Seat::Session);
    let c = seat(&server, "s-c", A, Seat::Session);
    let ask = asked(a.ask("q1", "s-b", "which port?"));
    assert!(matches!(
        refused(c.withdraw("w1", ask.seq)),
        Refused::AskNotYours { ask: id, .. } if id == ask.seq
    ));
    let gone = asked(a.withdraw("w2", ask.seq));
    assert!(matches!(
        &gone.state,
        AskState::Withdrawn { by, .. } if by == "s-a"
    ));
    // B is told in the thread.
    let note = b.next_seq(ask.seq + 1);
    assert_eq!(note.thread, Some(ask.seq));
    assert_eq!(note.kind, Seat::System);
    assert!(note.text.contains("withdrawn by s-a"), "{}", note.text);

    // The operator's seat takes another back, and the note says it was them.
    let second = asked(a.ask("q2", "s-b", "and the user?"));
    let op = seat(&server, OPERATOR, "", Seat::Operator);
    let gone = asked(op.withdraw("w3", second.seq));
    assert!(matches!(
        &gone.state,
        AskState::Withdrawn { by, .. } if by == OPERATOR
    ));
}

/// An ask of a session that is not connected now is accepted - a service that is briefly down is a
/// fault to repair, not an answer - and, since that seat cannot act, it escalates at once.
#[test]
fn an_ask_of_a_session_that_is_not_connected_escalates_at_once() {
    let dir = Scratch::new();
    let server = coordinator(&dir);
    let a = seat(&server, "s-a", A, Seat::Session);
    // B is in the tree (a daemon reported it) and has no connection now.
    tell(&a, vec![top("s-a", "arango"), top("s-b", "serenedb")]);
    let ask = asked(a.ask("q1", "serenedb", "which port?"));
    assert!(ask.open());
    assert!(
        ask.gone_since_ms.is_some(),
        "the reply says the target is not connected now: {ask:?}"
    );
    assert!(
        ask.escalated,
        "a seat that cannot act is no grace's business"
    );
    assert_eq!(ask.cannot, "was stopped when asked");
    assert!(server.gone_since("s-b").is_some());
    // Back, the escalation is withdrawn.
    let _b = seat(&server, "s-b", A, Seat::Session);
    let (by_me, _) = a.asks();
    assert!(
        !by_me[0].escalated && by_me[0].cannot.is_empty(),
        "{by_me:?}"
    );
}
