//! **Top-level sessions only** — a subagent is refused by name, at every verb, and the refusal is
//! the sentence that says why: a subagent speaks through its parent.

use letibot_room::{Refused, Room, Speaker};

#[test]
fn a_subagent_cannot_post_or_claim_and_the_refusal_names_both_of_them() {
    let mut room = Room::new();
    let parent = Speaker::top_level("s-a");
    let child = Speaker::subagent("s-a-sub-1", "s-a");

    room.post(&parent, "p", "ram", "8 GB", 1_000).unwrap();

    let refusals = [
        room.post(&child, "c1", "ram", "8 GB", 1_001).err(),
        room.claim(&child, "c2", "ram", 1_002).err(),
        room.claim_item(&child, "c3", 1, 1_003).err(),
        room.beat(&child, "c4", 1, 1_004).err(),
        room.waiting_on(&child, "c5", 1, "the suite", 1_005).err(),
        room.done(&child, "c6", 1, 1_006).err(),
        room.abandon(&child, "c7", 1, "cannot", 1_007).err(),
        room.dispute(&child, "c8", 1, "mine", 1_008).err(),
    ];
    for refusal in refusals {
        match refusal {
            Some(Refused::Subagent { session, parent }) => {
                assert_eq!(session, "s-a-sub-1");
                assert_eq!(parent, "s-a");
            }
            other => panic!("a subagent is refused at every verb: {other:?}"),
        }
    }

    // The refusal is a sentence a model can act on: it names the child and the parent it must go
    // through.
    let why = Refused::Subagent {
        session: "s-a-sub-1".to_string(),
        parent: "s-a".to_string(),
    }
    .to_string();
    assert!(
        why.contains("s-a-sub-1") && why.contains("s-a") && why.contains("parent"),
        "{why}"
    );

    // And nothing it tried changed the room.
    assert_eq!(room.item(1).unwrap().state_name(), "open");
    assert_eq!(room.all().len(), 1);
    assert!(room.holding("s-a-sub-1", 2_000).is_empty());
}

#[test]
fn the_parent_may_speak_for_the_work_and_the_child_may_not() {
    let mut room = Room::new();
    let parent = Speaker::top_level("s-a");
    room.post(&parent, "p", "ram", "8 GB", 1_000).unwrap();
    let claimed = room.claim(&parent, "c", "ram", 1_001).unwrap();
    assert_eq!(claimed.item.holder(), Some("s-a"));

    // A subagent cannot even be vouched for — the rule is the room's, not the door's.
    assert!(room.vouch(&Speaker::subagent("s-a-sub-1", "s-a")).is_err());
    assert_eq!(room.holding("s-a", 1_002).len(), 1);
}
