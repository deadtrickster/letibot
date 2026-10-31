//! **The wire** — the frames a coordinator's socket would take and send, and the two idempotency
//! promises a reconnecting client depends on.
//!
//! These tests drive [`Room::answer`], which is the server's dispatch with the socket taken out, so
//! a frame that cannot be answered here is a frame that cannot be answered there.

use letibot_room::{
    Decision, Event, Item, ROOM_PROTOCOL_VERSION, Request, Response, Room, Speaker, State,
};

fn a_room() -> (Room, Speaker, Speaker) {
    (
        Room::new(),
        Speaker::top_level("s-a"),
        Speaker::top_level("s-b"),
    )
}

#[test]
fn every_frame_round_trips_through_json() {
    let speaker = Speaker::top_level("s-a");
    let frames = vec![
        Request::Hello {
            protocol: ROOM_PROTOCOL_VERSION,
            speaker: speaker.clone(),
            room: "lab2x1".to_string(),
        },
        Request::Post {
            id: "r1".to_string(),
            tag: "ram".to_string(),
            body: "8 GB, ~10 min".to_string(),
        },
        Request::Claim {
            id: "r2".to_string(),
            tag: "ram".to_string(),
        },
        Request::ClaimItem {
            id: "r3".to_string(),
            item: 1,
        },
        Request::Beat {
            id: "r4".to_string(),
            item: 1,
        },
        Request::Waiting {
            id: "r5".to_string(),
            item: 1,
            text: "the TPC-C suite".to_string(),
        },
        Request::Done {
            id: "r6".to_string(),
            item: 1,
        },
        Request::Abandon {
            id: "r7".to_string(),
            item: 1,
            why: "the suite blew up".to_string(),
        },
        Request::Dispute {
            id: "r8".to_string(),
            item: 1,
            says: "still on it".to_string(),
        },
        Request::Rule {
            id: "r9".to_string(),
            question: 1,
            decision: Decision::Hand {
                to: "s-b".to_string(),
            },
        },
        Request::Queue {
            tag: "ram".to_string(),
        },
        Request::Holding { session: None },
        Request::Reminders,
        Request::Questions,
        Request::Room,
    ];
    for frame in frames {
        let json = serde_json::to_string(&frame).expect("a frame serialises");
        let back: Request = serde_json::from_str(&json).expect("and reads back");
        assert_eq!(frame, back, "{json}");
    }

    let item = Item {
        id: 1,
        tag: "ram".to_string(),
        body: "8 GB".to_string(),
        posted_by: "s-a".to_string(),
        posted_ms: 1_000,
        state: State::Open,
        abandonments: Vec::new(),
        question: None,
    };
    let replies = vec![
        Response::Welcome {
            protocol: ROOM_PROTOCOL_VERSION,
            lease_ms: 90_000,
            beat_every_ms: 5_000,
            threshold: 3,
            identity: "the room on /tmp/room.sock — started 4s ago".to_string(),
        },
        Response::Posted {
            id: "r1".to_string(),
            item: item.clone(),
        },
        Response::Claimed {
            id: "r2".to_string(),
            item: item.clone(),
        },
        Response::Queue {
            tag: "ram".to_string(),
            rows: vec![item.clone()],
        },
        Response::Room {
            identity: "the room on /tmp/room.sock — started 4s ago".to_string(),
            arbiter: Some("s-arbiter".to_string()),
            connected: vec!["s-a".to_string()],
            items: vec![item],
            questions: Vec::new(),
        },
        Response::Reminders { rows: Vec::new() },
        Response::Event {
            event: Event::Posted {
                item: 1,
                tag: "ram".to_string(),
                by: "s-a".to_string(),
            },
        },
        Response::Bye {
            why: "the coordinator stopped".to_string(),
        },
    ];
    for frame in replies {
        let json = serde_json::to_string(&frame).expect("a reply serialises");
        let back: Response = serde_json::from_str(&json).expect("and reads back");
        assert_eq!(frame, back, "{json}");
    }
}

#[test]
fn the_hello_hands_out_the_lease_and_refuses_a_subagent_at_the_door() {
    let (mut room, a, _) = a_room();
    let welcome = room.answer(
        &a,
        &Request::Hello {
            protocol: ROOM_PROTOCOL_VERSION,
            speaker: a.clone(),
            room: String::new(),
        },
        1_000,
    );
    match welcome {
        Response::Welcome {
            protocol,
            lease_ms,
            threshold,
            beat_every_ms,
            ..
        } => {
            assert_eq!(protocol, ROOM_PROTOCOL_VERSION);
            assert_eq!(lease_ms, room.lease_ms());
            assert_eq!(threshold, 3);
            assert!(
                beat_every_ms * 3 < lease_ms,
                "a client can beat three times per lease"
            );
        }
        other => panic!("a hello is answered with a welcome: {other:?}"),
    }

    let child = Speaker::subagent("s-a-sub-1", "s-a");
    match room.answer(
        &child,
        &Request::Hello {
            protocol: ROOM_PROTOCOL_VERSION,
            speaker: child.clone(),
            room: String::new(),
        },
        1_001,
    ) {
        Response::Refused { id: None, why } => assert!(
            why.to_string().contains("subagent"),
            "the refusal says what it is: {why}"
        ),
        other => panic!("a subagent is refused at the door: {other:?}"),
    }

    match room.answer(
        &a,
        &Request::Hello {
            protocol: ROOM_PROTOCOL_VERSION + 1,
            speaker: a.clone(),
            room: String::new(),
        },
        1_002,
    ) {
        Response::Bye { why } => {
            assert!(why.contains(&ROOM_PROTOCOL_VERSION.to_string()), "{why}");
            assert!(
                why.contains(&(ROOM_PROTOCOL_VERSION + 1).to_string()),
                "{why}"
            );
        }
        other => panic!("a protocol mismatch is a clean Bye: {other:?}"),
    }
}

#[test]
fn a_duplicate_post_is_answered_and_not_posted_twice() {
    let (mut room, a, _) = a_room();
    let post = Request::Post {
        id: "r1".to_string(),
        tag: "ram".to_string(),
        body: "8 GB, ~10 min".to_string(),
    };
    let first = room.answer(&a, &post, 1_000);
    // The client reconnects and replays the frame it never saw the reply to.
    let again = room.answer(&a, &post, 1_001);
    assert_eq!(first, again, "the same reply, and the same item");
    assert_eq!(room.all().len(), 1, "one item, not two");
    match again {
        Response::Posted { item, .. } => assert_eq!(item.id, 1),
        other => panic!("a post is answered with the item: {other:?}"),
    }

    // At the module level the repeat also carries no events: the bell rang the first time.
    let repeat = room
        .post(&a, "r1", "ram", "8 GB, ~10 min", 1_002)
        .expect("answered, not refused");
    assert!(repeat.events.is_empty());
}

#[test]
fn a_repeated_claim_is_the_same_claim() {
    let (mut room, a, _) = a_room();
    room.answer(
        &a,
        &Request::Post {
            id: "p".to_string(),
            tag: "ram".to_string(),
            body: "8 GB".to_string(),
        },
        1_000,
    );
    let first = room.answer(
        &a,
        &Request::Claim {
            id: "c1".to_string(),
            tag: "ram".to_string(),
        },
        1_100,
    );
    // A new request id — a client that cannot remember what it sent — and the same claim comes
    // back, with the clock where it was.
    let second = room.answer(
        &a,
        &Request::Claim {
            id: "c2".to_string(),
            tag: "ram".to_string(),
        },
        1_900,
    );
    // The reply is correlated to the frame that was sent, so the ids differ; the ITEM is the same
    // claim, with the clock where it was.
    match (&first, &second) {
        (Response::Claimed { item: one, .. }, Response::Claimed { item: two, .. }) => {
            assert_eq!(one, two, "the same claim, not a second one");
            assert_eq!(two.claim().unwrap().claim_ms, 1_100);
            assert_eq!(two.claim().unwrap().beat_ms, 1_100);
        }
        other => panic!("a claim is answered with the item: {other:?}"),
    }
    assert_eq!(room.item(1).unwrap().abandonments(), 0);
}

#[test]
fn a_queue_read_is_in_order_and_a_holding_read_answers_what_a_session_has() {
    let (mut room, a, b) = a_room();
    for (who, request, body) in [(&a, "p1", "first"), (&b, "p2", "second")] {
        room.answer(
            who,
            &Request::Post {
                id: request.to_string(),
                tag: "ram".to_string(),
                body: body.to_string(),
            },
            1_000,
        );
    }
    match room.answer(
        &a,
        &Request::Queue {
            tag: "RAM".to_string(),
        },
        1_100,
    ) {
        Response::Queue { tag, rows } => {
            assert_eq!(tag, "ram");
            let bodies: Vec<&str> = rows.iter().map(|i| i.body.as_str()).collect();
            assert_eq!(bodies, ["first", "second"]);
        }
        other => panic!("a queue read: {other:?}"),
    }
    room.answer(
        &a,
        &Request::Claim {
            id: "c".to_string(),
            tag: "ram".to_string(),
        },
        1_200,
    );
    room.answer(
        &a,
        &Request::Waiting {
            id: "w".to_string(),
            item: 1,
            text: "the TPC-C suite".to_string(),
        },
        1_300,
    );
    match room.answer(&a, &Request::Holding { session: None }, 1_400) {
        Response::Holding { session, rows } => {
            assert_eq!(session, "s-a");
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].waits_on.as_ref().unwrap().text, "the TPC-C suite");
        }
        other => panic!("a holding read: {other:?}"),
    }
    match room.answer(
        &b,
        &Request::Holding {
            session: Some("s-a".to_string()),
        },
        1_400,
    ) {
        Response::Holding { rows, .. } => {
            assert_eq!(rows.len(), 1, "one session may read another's")
        }
        other => panic!("a holding read for another session: {other:?}"),
    }
}

#[test]
fn a_dispute_and_its_ruling_cross_the_wire() {
    let (mut room, a, b) = a_room();
    room.answer(
        &a,
        &Request::Post {
            id: "p".to_string(),
            tag: "ram".to_string(),
            body: "8 GB".to_string(),
        },
        1_000,
    );
    room.answer(
        &a,
        &Request::Claim {
            id: "c".to_string(),
            tag: "ram".to_string(),
        },
        1_000,
    );
    room.sweep(1_000 + 90_001);
    room.answer(
        &b,
        &Request::Claim {
            id: "c2".to_string(),
            tag: "ram".to_string(),
        },
        100_000,
    );
    let asked = room.answer(
        &a,
        &Request::Dispute {
            id: "d".to_string(),
            item: 1,
            says: "still on it".to_string(),
        },
        100_001,
    );
    let question = match asked {
        Response::Question { question, .. } => question,
        other => panic!("a dispute asks a question: {other:?}"),
    };
    match room.answer(
        &a,
        &Request::Rule {
            id: "r".to_string(),
            question: question.id,
            decision: Decision::Hand {
                to: "s-a".to_string(),
            },
        },
        100_002,
    ) {
        Response::Ruled { question, .. } => {
            assert_eq!(question.answer.as_ref().unwrap().by, "s-a");
        }
        other => panic!("a ruling is answered with the question: {other:?}"),
    }
    assert_eq!(room.item(1).unwrap().holder(), Some("s-a"));
    match room.answer(&b, &Request::Questions, 100_003) {
        Response::Questions { rows } => assert!(rows.is_empty(), "it was answered"),
        other => panic!("the open questions: {other:?}"),
    }
}

/// **The pane's read is a read**: every item (the finished ones too — a row that vanished is a row
/// a person cannot account for), every open question, and the three fields the ROOM cannot answer
/// left empty for the server to fill.
#[test]
fn the_rooms_read_answers_every_item_and_the_open_questions() {
    let (mut room, a, b) = a_room();
    room.post(&a, "p1", "ram", "8 GB", 1_000).expect("a post");
    room.post(&b, "p2", "ram", "2 GB", 1_001).expect("a post");
    room.post(&a, "p3", "disk", "40 GB", 1_002).expect("a post");
    room.claim(&a, "c1", "ram", 1_003).expect("a claim");
    room.done(&a, "d1", 1, 1_004).expect("finished");

    match room.answer(&a, &Request::Room, 1_005) {
        Response::Room {
            identity,
            arbiter,
            connected,
            items,
            questions,
        } => {
            assert_eq!(
                items.iter().map(|i| i.id).collect::<Vec<_>>(),
                vec![1, 2, 3],
                "oldest first, and the finished one is still here"
            );
            assert_eq!(items[0].state_name(), "done");
            assert_eq!(items[1].holder(), None, "`s-b` has not claimed anything");
            assert!(questions.is_empty());
            // **The three the state machine cannot answer** — the SERVER fills them, and a room
            // that invented them would be a second answer to *is the arbiter up*.
            assert!(identity.is_empty(), "{identity}");
            assert_eq!(arbiter, None);
            assert!(connected.is_empty(), "{connected:?}");
        }
        other => panic!("a room read answers a room: {other:?}"),
    }
}
