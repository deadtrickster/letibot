//! **The operator's own example, as one test** — three top-level sessions that each need RAM, one
//! that runs, and a claimant whose beat stops.
//!
//! The brief's words: *"there is RAM shortage — agents should be able to talk thru it and
//! coordinate … once agent that claimed the queue item dies (or goes offline) that item is marked
//! as such and queue moves on."* The acceptance test is that sentence end to end, from the room's
//! API and with no socket in it.

use letibot_room::{DEFAULT_LEASE_MS, Event, Refused, Room, Speaker, Why};

/// The test the brief asks for, named after the example it is.
#[test]
fn three_sessions_need_ram_one_claims_and_a_stopped_beat_hands_it_on() {
    let mut room = Room::new();
    let (a, b, c) = (
        Speaker::top_level("s-a"),
        Speaker::top_level("s-b"),
        Speaker::top_level("s-c"),
    );
    let t0 = 1_000_000;

    // 1. Each of the three posts the same need under the same tag.
    let one = room.post(&a, "a-1", "ram", "8 GB, ~10 min", t0).unwrap();
    let two = room
        .post(&b, "b-1", "ram", "8 GB, ~10 min", t0 + 10)
        .unwrap();
    let three = room
        .post(&c, "c-1", "ram", "8 GB, ~10 min", t0 + 20)
        .unwrap();
    assert_eq!((one.item.id, two.item.id, three.item.id), (1, 2, 3));

    // 2. One head, first come first served: `s-a`'s, because it was posted first.
    assert_eq!(room.head("ram").map(|i| i.id), Some(1));

    // 3. One claims; the others are refused BY NAME and told who has it — the room's half of
    //    "the others are told rather than polling".
    let claimed = room.claim(&a, "a-2", "ram", t0 + 100).unwrap();
    assert_eq!(claimed.item.holder(), Some("s-a"));
    assert_eq!(
        claimed.events,
        vec![Event::Claimed {
            item: 1,
            tag: "ram".to_string(),
            by: "s-a".to_string(),
        }]
    );
    for (who, request) in [(&b, "b-2"), (&c, "c-2")] {
        match room.claim(who, request, "ram", t0 + 200) {
            Err(Refused::Busy { by, item, .. }) => {
                assert_eq!(by, "s-a");
                assert_eq!(item, 1);
            }
            other => panic!("the tag is held, so this must be Busy: {other:?}"),
        }
    }
    // …and the queue itself says so, which is what the others read.
    let queue = room.queue("ram");
    assert_eq!(queue.len(), 3);
    assert_eq!(queue[0].state_name(), "claimed");
    assert_eq!(queue[1].state_name(), "open");
    assert_eq!(queue[2].state_name(), "open");

    // 4. The claimer says what it is waiting on, and beats while its suite runs.
    room.waiting_on(&a, "a-3", 1, "the TPC-C suite", t0 + 1_000)
        .unwrap();
    let mut last_beat = t0;
    for k in 1..=5u64 {
        last_beat = t0 + k * 5_000;
        room.beat(&a, &format!("a-beat-{k}"), 1, last_beat).unwrap();
    }

    // What is it holding, and what is it waiting on — the reminder's facts.
    let holding = room.holding("s-a", last_beat + 1);
    assert_eq!(holding.len(), 1);
    assert_eq!(holding[0].item, 1);
    assert_eq!(
        holding[0].waits_on.as_ref().unwrap().text,
        "the TPC-C suite"
    );
    let reminder = room
        .reminders(last_beat + 1)
        .into_iter()
        .find(|r| r.session == "s-a")
        .expect("s-a holds one item");
    assert!(!reminder.stale);
    assert!(reminder.sentence().contains("the TPC-C suite"));

    // 5. The beat stops. One millisecond before the lease runs out nothing happens…
    assert!(
        room.sweep(last_beat + DEFAULT_LEASE_MS).is_empty(),
        "a beat at the boundary is still alive"
    );
    // …and one after, the pass marks it abandoned, LOUDLY, with the count up and back in the
    // queue.
    let events = room.sweep(last_beat + DEFAULT_LEASE_MS + 1);
    match &events[0] {
        Event::Abandoned {
            item,
            by,
            why,
            count,
            back_in_the_queue,
            ..
        } => {
            assert_eq!(*item, 1);
            assert_eq!(by, "s-a");
            assert_eq!(*count, 1);
            assert!(*back_in_the_queue, "one abandonment is not a raise");
            assert!(
                matches!(why, Why::BeatStale { age_ms } if *age_ms == DEFAULT_LEASE_MS + 1),
                "the reason names the age: {why:?}"
            );
        }
        other => panic!("the pass must abandon #1: {other:?}"),
    }
    let item = room.item(1).unwrap();
    assert_eq!(item.state_name(), "abandoned");
    assert_eq!(item.abandonments(), 1);
    assert!(item.claimable(), "an abandoned item is back in the queue");
    assert_eq!(room.head("ram").map(|i| i.id), Some(1), "at the head");

    // 6. The next session claims it — the same item, with its count still on it.
    let next = room
        .claim(&b, "b-3", "ram", last_beat + DEFAULT_LEASE_MS + 2)
        .unwrap();
    assert_eq!(next.item.id, 1);
    assert_eq!(next.item.holder(), Some("s-b"));
    assert_eq!(next.item.abandonments(), 1);
    // And the third is still waiting, now behind `s-b`.
    match room.claim(&c, "c-3", "ram", last_beat + DEFAULT_LEASE_MS + 3) {
        Err(Refused::Busy { by, .. }) => assert_eq!(by, "s-b"),
        other => panic!("`s-b` holds the tag now: {other:?}"),
    }

    // 7. `s-b` finishes, and the queue moves on to the next request.
    room.done(&b, "b-4", 1, last_beat + DEFAULT_LEASE_MS + 4_000)
        .unwrap();
    assert_eq!(room.item(1).unwrap().state_name(), "done");
    assert_eq!(room.head("ram").map(|i| i.id), Some(2));
    let c_claims = room
        .claim(&c, "c-4", "ram", last_beat + DEFAULT_LEASE_MS + 5_000)
        .unwrap();
    assert_eq!(c_claims.item.id, 2);
    assert_eq!(c_claims.item.holder(), Some("s-c"));
}
