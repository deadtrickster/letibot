//! Presence on a real coordinator socket: a seat's daemon reports facts, the roster derives the
//! state from them and from what only the room sees (the connection, open asks, queue waits).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use letibot_room::client::Client;
use letibot_room::seat::{Call, SeatFacts, SeatState, WorkingOn};
use letibot_room::server::Server;
use letibot_room::{
    Event, Group, Member, OPERATOR, Presence, Request, Response, Room, Seat, Speaker, room_name,
};

const A: &str = "aaaaaaaaaaaa";

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Scratch {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "letibot-room-seat-{}-{}",
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

fn seat(server: &Server, name: &str, seat: Seat) -> Client {
    let wake = Arc::new(|_: &Event| {});
    let room = room_name(&server.name(), if seat == Seat::Session { A } else { "" });
    Client::connect_as(server.path(), Speaker::top_level(name), room, seat, wake)
        .unwrap_or_else(|e| panic!("`{name}` must connect: {e}"))
}

fn top(session: &str) -> Member {
    Member {
        session: session.into(),
        label: session.into(),
        handle: String::new(),
        group: Group::Top,
        parent: None,
        depth: 0,
        live: true,
    }
}

fn roster(c: &Client) -> Vec<Presence> {
    match c
        .request(&Request::Roster {
            room: letibot_room::project_room(A),
        })
        .expect("an answer")
    {
        Response::Roster { rows, .. } => rows,
        other => panic!("{other:?}"),
    }
}

fn row(c: &Client, id: &str) -> Presence {
    roster(c)
        .into_iter()
        .find(|p| p.id == id)
        .unwrap_or_else(|| panic!("`{id}` is in the roster"))
}

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

#[test]
fn the_roster_derives_each_seats_state_from_its_facts_and_the_room() {
    let dir = Scratch::new();
    let server = Server::serve(&dir.0.join("room.sock"), Room::new()).expect("a coordinator");
    let a = seat(&server, "s-a", Seat::Session);
    let b = seat(&server, "s-b", Seat::Session);
    let op = seat(&server, OPERATOR, Seat::Operator);
    a.request(&Request::Members {
        rows: vec![top("s-a"), top("s-b"), top("s-c")],
        root: String::new(),
    })
    .unwrap();

    // Connected and silent is unknown, never idle; never connected is stopped.
    let s = row(&op, "s-a").seat.expect("a session has a seat view");
    assert!(matches!(s.state, SeatState::Unknown { .. }), "{s:?}");
    assert!(s.connected);
    let c = row(&op, "s-c").seat.unwrap();
    assert!(matches!(c.state, SeatState::Stopped { .. }), "{c:?}");
    assert!(
        row(&op, OPERATOR).seat.is_none(),
        "the operator has no seat view"
    );

    let now = letibot_metrics::now_ms();
    let facts = SeatFacts {
        turn_since_ms: Some(now - 5_000),
        last_call: Some(Call {
            ok: true,
            at_ms: now,
            error: String::new(),
        }),
        working: WorkingOn {
            row: "wire the port".into(),
            repo: "letibot".into(),
            branch: "z-r163".into(),
            files: vec!["crates/room/src/seat.rs".into()],
            ..WorkingOn::default()
        },
        ..SeatFacts::default()
    };
    assert!(matches!(
        a.request(&Request::Seat {
            facts: facts.clone()
        })
        .unwrap(),
        Response::Seated { .. }
    ));
    let s = row(&op, "s-a").seat.unwrap();
    assert!(s.says.starts_with("in turn 5s"), "{}", s.says);
    assert_eq!(
        s.doing,
        "on \"wire the port\" · letibot@z-r163 · crates/room/src/seat.rs"
    );

    // An open ask it made is a wait the room sees, without the daemon saying so.
    b.request(&Request::Seat {
        facts: SeatFacts::default(),
    })
    .unwrap();
    let asked = a
        .request(&Request::Ask {
            id: "q1".into(),
            room: String::new(),
            to: "s-b".into(),
            path: String::new(),
            topic: String::new(),
            question: "which port?".into(),
        })
        .unwrap();
    let Response::Asked { ask, .. } = asked else {
        panic!("{asked:?}")
    };
    let s = row(&op, "s-a").seat.unwrap();
    assert_eq!(
        s.says,
        format!("in turn, waiting on ask #{} of s-b", ask.seq)
    );

    // The connection closing is the stop, whatever the last facts said.
    drop(b);
    until("s-b is stopped", || {
        matches!(
            row(&op, "s-b").seat.unwrap().state,
            SeatState::Stopped { since_ms: Some(_) }
        )
    });
}

#[test]
fn a_seat_reports_only_its_own_facts() {
    let dir = Scratch::new();
    let server = Server::serve(&dir.0.join("room.sock"), Room::new()).expect("a coordinator");
    let a = seat(&server, "s-a", Seat::Session);
    let op = seat(&server, OPERATOR, Seat::Operator);
    // The operator's seat sends facts and nobody's presence moves.
    op.request(&Request::Seat {
        facts: SeatFacts {
            turn_since_ms: Some(1),
            ..SeatFacts::default()
        },
    })
    .unwrap();
    let s = row(&op, "s-a").seat.unwrap();
    assert!(matches!(s.state, SeatState::Unknown { .. }), "{s:?}");
    drop(a);
}

#[test]
fn an_ask_to_a_rate_limited_seat_escalates_at_once_and_clears_when_it_lifts() {
    let dir = Scratch::new();
    let server = Server::serve(&dir.0.join("room.sock"), Room::new()).expect("a coordinator");
    let a = seat(&server, "s-a", Seat::Session);
    let b = seat(&server, "s-b", Seat::Session);
    let now = letibot_metrics::now_ms();
    let limited = |until: Option<u64>| SeatFacts {
        limited_until_ms: until,
        ..SeatFacts::default()
    };
    b.request(&Request::Seat {
        facts: limited(Some(now + 600_000)),
    })
    .unwrap();
    let Response::Asked { ask, .. } = a
        .request(&Request::Ask {
            id: "q1".into(),
            room: String::new(),
            to: "s-b".into(),
            path: String::new(),
            topic: String::new(),
            question: "which port?".into(),
        })
        .unwrap()
    else {
        panic!("asked")
    };
    assert!(
        ask.escalated,
        "no grace for a seat that cannot act: {ask:?}"
    );
    assert_eq!(ask.cannot, "is rate-limited");
    b.request(&Request::Seat {
        facts: limited(None),
    })
    .unwrap();
    let Response::Asks { by_me, .. } = a.request(&Request::Asks).unwrap() else {
        panic!("asks")
    };
    assert!(
        !by_me[0].escalated && by_me[0].cannot.is_empty(),
        "{by_me:?}"
    );
}
