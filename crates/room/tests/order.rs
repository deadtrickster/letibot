//! **The ordering rule, stated as tests** — a tag is a queue, first come first served, and a tag
//! is a lock.

use letibot_room::{Refused, Room, Speaker};

#[test]
fn a_tag_is_a_queue_in_the_order_it_was_posted() {
    let mut room = Room::new();
    let (a, b, c) = (
        Speaker::top_level("s-a"),
        Speaker::top_level("s-b"),
        Speaker::top_level("s-c"),
    );
    room.post(&a, "a", "ram", "first", 1_000).unwrap();
    room.post(&b, "b", "ram", "second", 1_001).unwrap();
    room.post(&c, "c", "ram", "third", 1_002).unwrap();

    let bodies: Vec<&str> = room.queue("ram").iter().map(|i| i.body.as_str()).collect();
    assert_eq!(bodies, ["first", "second", "third"]);

    // Claimed, done, and the queue moves on in the same order.
    assert_eq!(
        room.claim(&a, "a2", "ram", 2_000).unwrap().item.body,
        "first"
    );
    room.done(&a, "a3", 1, 2_100).unwrap();
    assert_eq!(room.head("ram").map(|i| i.body.as_str()), Some("second"));
    assert_eq!(
        room.claim(&b, "b2", "ram", 2_200).unwrap().item.body,
        "second"
    );
}

#[test]
fn one_millisecond_is_the_tiebreak_and_the_room_id_breaks_that() {
    let mut room = Room::new();
    let (a, b) = (Speaker::top_level("s-a"), Speaker::top_level("s-b"));
    // Two posts at the SAME millisecond: the room's own id decides, so the order is never a
    // property of the sort.
    room.post(&a, "a", "ram", "first", 1_000).unwrap();
    room.post(&b, "b", "ram", "second", 1_000).unwrap();
    let ids: Vec<u64> = room.queue("ram").iter().map(|i| i.id).collect();
    assert_eq!(ids, [1, 2]);
}

#[test]
fn a_tag_is_normalised_so_two_spellings_are_one_queue() {
    let mut room = Room::new();
    let a = Speaker::top_level("s-a");
    room.post(&a, "a", " RAM ", "8 GB", 1_000).unwrap();
    assert_eq!(room.tags(), vec!["ram".to_string()]);
    assert_eq!(room.queue("Ram").len(), 1);
    assert!(matches!(
        room.post(&a, "b", "   ", "8 GB", 1_001),
        Err(Refused::EmptyTag)
    ));
    assert!(matches!(
        room.post(&a, "c", "ram", "   ", 1_002),
        Err(Refused::EmptyBody)
    ));
}

#[test]
fn a_tag_is_a_lock_so_a_second_claim_under_it_is_refused() {
    let mut room = Room::new();
    let (a, b) = (Speaker::top_level("s-a"), Speaker::top_level("s-b"));
    room.post(&a, "a", "ram", "8 GB", 1_000).unwrap();
    room.post(&b, "b", "ram", "8 GB", 1_001).unwrap();
    room.post(&b, "g", "gpu", "one card", 1_002).unwrap();

    room.claim(&a, "a2", "ram", 2_000).unwrap();
    // `s-b`'s own request is *behind* the held one, and the tag is a lock — so no second claim
    // under `ram` at all, not even on its own item.
    assert!(matches!(
        room.claim(&b, "b2", "ram", 2_001),
        Err(Refused::Busy { .. })
    ));
    assert!(matches!(
        room.claim_item(&b, "b3", 2, 2_002),
        Err(Refused::Busy { .. })
    ));
    // Another tag is a different lock.
    assert_eq!(room.claim(&b, "b4", "gpu", 2_003).unwrap().item.id, 3);
}

#[test]
fn claiming_a_row_that_is_not_the_head_is_refused_and_names_the_head() {
    let mut room = Room::new();
    let (a, b) = (Speaker::top_level("s-a"), Speaker::top_level("s-b"));
    room.post(&a, "a", "ram", "first", 1_000).unwrap();
    room.post(&a, "b", "ram", "second", 1_001).unwrap();
    match room.claim_item(&b, "b2", 2, 2_000) {
        Err(Refused::NotTheHead { item, tag, head }) => {
            assert_eq!((item, tag.as_str(), head), (2, "ram", 1));
        }
        other => panic!("an id is not a way to jump the queue: {other:?}"),
    }
    // The head itself, by id, is fine.
    assert_eq!(room.claim_item(&b, "b3", 1, 2_001).unwrap().item.id, 1);
}

#[test]
fn an_abandoned_item_returns_to_the_head_of_its_tag() {
    let mut room = Room::new();
    let (a, b) = (Speaker::top_level("s-a"), Speaker::top_level("s-b"));
    room.post(&a, "a", "ram", "first", 1_000).unwrap();
    room.post(&b, "b", "ram", "second", 5_000).unwrap();
    room.claim(&a, "a2", "ram", 6_000).unwrap();
    room.sweep(6_000 + 90_001);

    // The old request is in front of the newer one, because it was posted first — the queue moves
    // on from a claimant, not from the request.
    let ids: Vec<u64> = room.queue("ram").iter().map(|i| i.id).collect();
    assert_eq!(ids, [1, 2]);
    assert_eq!(room.head("ram").map(|i| i.id), Some(1));
}

#[test]
fn a_finished_or_dropped_item_leaves_the_queue() {
    let mut room = Room::new();
    let a = Speaker::top_level("s-a");
    room.post(&a, "a", "ram", "8 GB", 1_000).unwrap();
    room.claim(&a, "b", "ram", 1_001).unwrap();
    room.done(&a, "c", 1, 1_002).unwrap();
    assert!(room.queue("ram").is_empty());
    assert_eq!(room.all().len(), 1, "the item itself is still on the board");
    assert!(matches!(
        room.claim(&a, "d", "ram", 1_003),
        Err(Refused::NothingToClaim { .. })
    ));
    assert!(matches!(
        room.claim_item(&a, "e", 1, 1_004),
        Err(Refused::Gone { .. })
    ));
}
