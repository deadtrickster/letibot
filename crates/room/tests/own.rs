//! Ownership on a real coordinator socket: claims refused by overlap naming the holder, a
//! closed-loop handover, the roster, the store, the signal ledger and a stopped owner flagged.

use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use letibot_room::client::Client;
use letibot_room::seat::{Edit, SeatFacts};
use letibot_room::server::Server;
use letibot_room::{
    Event, HOST_ROOM, Message, OPERATOR, Refused, Request, Response, Room, Seat, Speaker,
    project_room, room_name,
};

const A: &str = "aaaaaaaaaaaa";

fn scratch(tag: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("letibot-own-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn seat(server: &Server, name: &str, seat: Seat) -> (Client, Receiver<Message>) {
    let wake = Arc::new(|_: &Event| {});
    let project = if seat == Seat::Session { A } else { "" };
    let c = Client::connect_as(
        server.path(),
        Speaker::top_level(name),
        room_name(&server.name(), project),
        seat,
        wake,
    )
    .expect("connects");
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    c.on_chat(Arc::new(move |m, _| {
        let _ = tx.lock().unwrap().send(m.clone());
    }));
    (c, rx)
}

fn own(c: &Client, id: &str, scope: &str) -> Response {
    c.request(&Request::Own {
        id: id.into(),
        scope: scope.into(),
        ttl_ms: Some(60_000),
        thread: None,
    })
    .unwrap()
}

fn refusal(r: Response) -> String {
    match r {
        Response::Refused {
            why: Refused::Ownership { why },
            ..
        } => why,
        other => panic!("refused: {other:?}"),
    }
}

fn heard(rx: &Receiver<Message>, needle: &str) -> Message {
    loop {
        let m = rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap_or_else(|_| panic!("nothing pushed saying {needle:?}"));
        if m.text.contains(needle) {
            return m;
        }
    }
}

#[test]
fn a_claim_is_refused_by_overlap_handed_over_closed_loop_and_kept_in_the_store() {
    let dir = scratch("hand");
    let server = Server::serve(&dir.join("room.sock"), Room::new()).expect("a coordinator");
    server.with_item_store(&dir.join("room.items"));
    let (a, _) = seat(&server, "s-a", Seat::Session);
    let (b, b_heard) = seat(&server, "s-b", Seat::Session);
    let (op, _) = seat(&server, OPERATOR, Seat::Operator);

    assert!(matches!(
        own(&a, "1", "crates/room/**"),
        Response::Owned { .. }
    ));
    let why = refusal(own(&b, "2", "crates/room/src/own.rs"));
    assert!(why.contains("owned by `s-a`"), "{why}");
    assert!(matches!(
        own(&b, "3", "crates/tui/**"),
        Response::Owned { .. }
    ));

    // The roster shows who owns what.
    let rows = match op
        .request(&Request::Roster {
            room: project_room(A),
        })
        .unwrap()
    {
        Response::Roster { rows, .. } => rows,
        other => panic!("{other:?}"),
    };
    let a_row = rows.iter().find(|p| p.id == "s-a").unwrap();
    let owns = &a_row.seat.as_ref().unwrap().owns;
    assert!(owns[0].starts_with("crates/room/** "), "{owns:?}");

    // A hand tells the receiver, and the scope stays the old owner's until it accepts.
    let handed = a
        .request(&Request::Hand {
            id: "4".into(),
            scope: "crates/room/**".into(),
            to: "s-b".into(),
            note: "the wire half is done".into(),
        })
        .unwrap();
    assert!(matches!(handed, Response::Owned { .. }), "{handed:?}");
    let told = heard(&b_heard, "hands you `crates/room/**`");
    assert_eq!(told.to.as_deref(), Some("s-b"));
    assert!(told.text.contains("the wire half is done"));
    assert!(refusal(own(&b, "5", "crates/room/**")).contains("owned by `s-a`"));
    let took = b
        .request(&Request::Accept {
            id: "6".into(),
            scope: "crates/room/**".into(),
        })
        .unwrap();
    let Response::Owned { claim, .. } = took else {
        panic!("{took:?}")
    };
    assert_eq!(claim.owner, "s-b");
    assert!(
        refusal(
            a.request(&Request::Release {
                id: "7".into(),
                scope: "crates/room/**".into()
            })
            .unwrap()
        )
        .contains("`s-b`'s")
    );

    // A claim made in answer to a message is that message acted on, in the signal ledger.
    let asked = match a
        .request(&Request::Say {
            id: "9".into(),
            room: String::new(),
            text: "who takes the docs?".into(),
            to: None,
            thread: None,
            refs: Vec::new(),
            attachments: Vec::new(),
        })
        .unwrap()
    {
        Response::Said { message, .. } => message.seq,
        other => panic!("{other:?}"),
    };
    let took = b
        .request(&Request::Own {
            id: "10".into(),
            scope: "docs/**".into(),
            ttl_ms: None,
            thread: Some(asked),
        })
        .unwrap();
    assert!(matches!(took, Response::Owned { .. }), "{took:?}");
    server.flush_signals();
    let card = server.scorecard();
    let row = card.senders.iter().find(|r| r.key == "s-a").unwrap();
    assert_eq!(row.claims, 1, "{row:?}");

    // A restart brings the claims back from the store.
    server.stop();
    drop((a, b, op));
    let again = Server::serve(&dir.join("room2.sock"), Room::new()).expect("a coordinator");
    again.with_item_store(&dir.join("room.items"));
    let (c, _) = seat(&again, "s-c", Seat::Session);
    assert!(refusal(own(&c, "8", "crates/room/src")).contains("owned by `s-b`"));
    again.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn edits_in_scope_renew_and_a_stopped_owner_is_flagged_to_the_operator() {
    let dir = scratch("stop");
    let server = Server::serve(&dir.join("room.sock"), Room::new()).expect("a coordinator");
    server.with_ask_grace(0);
    let (a, _) = seat(&server, "s-a", Seat::Session);
    let (op, op_heard) = seat(&server, OPERATOR, Seat::Operator);
    let Response::Owned { claim, .. } = own(&a, "1", "crates/room/**") else {
        panic!()
    };
    let later = claim.renewed_ms + 1_000;
    let seated = a
        .request(&Request::Seat {
            facts: SeatFacts {
                edits: vec![Edit {
                    path: "crates/room/src/own.rs".into(),
                    at_ms: later,
                }],
                ..SeatFacts::default()
            },
        })
        .unwrap();
    let Response::Seated { owns } = seated else {
        panic!("{seated:?}")
    };
    assert_eq!(owns, ["crates/room/**"]);

    drop(a);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while server.connections() > 1 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    server.tick(letibot_room::server::now_ms());
    server.tick(letibot_room::server::now_ms() + 10);
    let m = heard(&op_heard, "stopped and still owns `crates/room/**`");
    assert_eq!(m.to.as_deref(), Some(OPERATOR));
    assert_eq!(m.room, project_room(A));
    // The operator's seat may release it.
    let gone = op
        .request(&Request::Release {
            id: "2".into(),
            scope: "crates/room/**".into(),
        })
        .unwrap();
    assert!(matches!(gone, Response::Owned { .. }), "{gone:?}");
    let _ = HOST_ROOM;
    server.stop();
    let _ = std::fs::remove_dir_all(&dir);
}
