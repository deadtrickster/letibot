//! **The lease, and what happens when it runs out** — the boundary, a session the daemon reports
//! gone, the count that raises an item instead of cycling, and a late claimant's dispute.

use letibot_room::{ABANDON_THRESHOLD, About, Decision, Event, Refused, Room, Speaker, State, Why};

fn room_with_lease(lease_ms: u64) -> Room {
    Room::with_lease_ms(lease_ms)
}

#[test]
fn a_beat_at_the_lease_boundary_is_still_alive_and_one_ms_later_is_not() {
    let mut room = room_with_lease(10_000);
    let a = Speaker::top_level("s-a");
    let t0 = 1_000;
    room.post(&a, "p", "ram", "8 GB", t0).unwrap();
    room.claim(&a, "c", "ram", t0 + 1_000).unwrap();

    assert!(
        room.sweep(t0 + 1_000 + 10_000).is_empty(),
        "a beat exactly at the lease is still alive — the queue lease's own comparison"
    );
    assert_eq!(room.item(1).unwrap().holder(), Some("s-a"));

    let events = room.sweep(t0 + 1_000 + 10_001);
    assert_eq!(events.len(), 1, "one millisecond later it is not");
    assert!(matches!(
        &events[0],
        Event::Abandoned {
            why: Why::BeatStale { age_ms: 10_001 },
            ..
        }
    ));
}

#[test]
fn a_session_the_daemon_reports_gone_loses_its_claim_at_once() {
    let mut room = Room::new();
    let a = Speaker::top_level("s-a");
    let b = Speaker::top_level("s-b");
    room.post(&a, "p", "ram", "8 GB", 1_000).unwrap();
    room.claim(&a, "c", "ram", 1_000).unwrap();

    // No sweep, no lease: the daemon knows, and says so.
    let events = room.note_gone("s-a", "its daemon exited", 2_000);
    match &events[0] {
        Event::Abandoned {
            by,
            why: Why::SessionGone { says },
            count,
            ..
        } => {
            assert_eq!(by, "s-a");
            assert_eq!(says, "its daemon exited");
            assert_eq!(*count, 1);
        }
        other => panic!("expected a loud abandonment: {other:?}"),
    }
    assert_eq!(room.item(1).unwrap().state_name(), "abandoned");
    // And the next session may take it immediately.
    assert_eq!(room.claim(&b, "c2", "ram", 2_001).unwrap().item.id, 1);
}

#[test]
fn three_abandonments_raise_the_item_and_the_tag_moves_past_it() {
    let mut room = Room::new();
    let (a, b, c, d) = (
        Speaker::top_level("s-a"),
        Speaker::top_level("s-b"),
        Speaker::top_level("s-c"),
        Speaker::top_level("s-d"),
    );
    let t = 1_000;
    room.post(&a, "p1", "ram", "8 GB, ~10 min", t).unwrap();
    room.post(&d, "p2", "ram", "2 GB, ~1 min", t + 1).unwrap();

    for (k, who) in [(1u64, &a), (2, &b), (3, &c)] {
        room.claim(who, &format!("c{k}"), "ram", t + k * 100)
            .unwrap();
        let out = room
            .abandon(
                who,
                &format!("a{k}"),
                1,
                "the suite blew up",
                t + k * 100 + 1,
            )
            .unwrap();
        let abandoned = out
            .events
            .iter()
            .find(|e| matches!(e, Event::Abandoned { .. }))
            .expect("every abandonment is loud");
        match abandoned {
            Event::Abandoned {
                count,
                back_in_the_queue,
                ..
            } => {
                assert_eq!(*count, k as u32);
                assert_eq!(
                    *back_in_the_queue,
                    k < ABANDON_THRESHOLD as u64,
                    "the third is the one that raises"
                );
            }
            _ => unreachable!(),
        }
        if k < 3 {
            assert!(
                room.item(1).unwrap().claimable(),
                "below the threshold it goes back in the queue"
            );
        }
    }

    // The third one raised it: abandoned, frozen, and its tag's queue moved past it.
    let item = room.item(1).unwrap();
    assert_eq!(item.state_name(), "abandoned");
    assert_eq!(item.abandonments(), 3);
    assert!(!item.claimable(), "a raised item is frozen");
    assert_eq!(room.head("ram").map(|i| i.id), Some(2));
    assert_eq!(room.claim(&d, "c4", "ram", t + 1_000).unwrap().item.id, 2);

    // The refusal names the item, the count and the question.
    match room.claim_item(&d, "c5", 1, t + 1_001) {
        Err(Refused::Raised {
            item,
            count,
            question,
        }) => {
            assert_eq!((item, count, question), (1, 3, 1));
        }
        other => panic!("a raised item is not claimable: {other:?}"),
    }

    // And the question is a question, with a decision in it.
    let open = room.open_questions();
    assert_eq!(open.len(), 1);
    let q = open[0];
    assert_eq!(
        q.about,
        About::ItemRaised {
            item: 1,
            tag: "ram".to_string()
        }
    );
    assert_eq!(q.facts.item, 1);
    assert_eq!(q.facts.abandonments.len(), 3);
    for wanted in [
        "Retry",
        "Hand",
        "Drop",
        "s-a",
        "s-b",
        "s-c",
        "8 GB, ~10 min",
    ] {
        assert!(
            q.text.contains(wanted),
            "the question must say {wanted}: {}",
            q.text
        );
    }
}

#[test]
fn a_ruling_retries_hands_or_drops() {
    /// Three abandonments of item #1, then whatever the arbiter says.
    fn raised() -> (Room, Speaker) {
        let mut room = Room::new();
        let a = Speaker::top_level("s-a");
        room.post(&a, "p", "ram", "8 GB", 1_000).unwrap();
        for k in 1..=3u64 {
            room.claim(&a, &format!("c{k}"), "ram", 1_000 + k * 10)
                .unwrap();
            room.abandon(&a, &format!("a{k}"), 1, "no", 1_000 + k * 10 + 1)
                .unwrap();
        }
        (room, a)
    }

    // Retry — back in the queue, the count kept, so the next loss raises at once.
    let (mut room, a) = raised();
    room.rule(1, Decision::Retry, "s-tie", 2_000).unwrap();
    assert!(room.item(1).unwrap().claimable());
    assert_eq!(room.item(1).unwrap().abandonments(), 3, "the count is kept");
    room.claim(&a, "c9", "ram", 2_001).unwrap();
    let again = room.abandon(&a, "a9", 1, "no", 2_002).unwrap();
    assert!(
        again
            .events
            .iter()
            .any(|e| matches!(e, Event::Raised { .. })),
        "a fourth loss raises it again without three more"
    );

    // Hand — the claim is made for the named session, now.
    let (mut room, _) = raised();
    room.vouch(&Speaker::top_level("s-b")).unwrap();
    room.rule(
        1,
        Decision::Hand {
            to: "s-b".to_string(),
        },
        "s-tie",
        2_000,
    )
    .unwrap();
    let item = room.item(1).unwrap();
    assert_eq!(item.holder(), Some("s-b"));
    assert!(matches!(item.state, State::Claimed { .. }));
    assert!(!item.claimable());
    assert_eq!(room.holding("s-b", 2_500).len(), 1);

    // Drop — out of the queue for good, with the reason on the item.
    let (mut room, _) = raised();
    room.rule(
        1,
        Decision::Drop {
            why: "the box has 4 GB and the need is 8".to_string(),
        },
        "s-tie",
        2_000,
    )
    .unwrap();
    let item = room.item(1).unwrap();
    assert_eq!(item.state_name(), "dropped");
    assert!(room.queue("ram").is_empty());
    assert_eq!(room.item(1).unwrap().state_name(), "dropped");

    // A question is answered once.
    match room.rule(1, Decision::Retry, "s-tie", 3_000) {
        Err(Refused::AlreadyRuled { question, by, .. }) => {
            assert_eq!((question, by.as_str()), (1, "s-tie"));
        }
        other => panic!("a ruled question cannot be re-answered: {other:?}"),
    }

    // A hand to a session the room has never met is refused, and so is one to a subagent.
    let (mut room, _) = raised();
    match room.rule(
        1,
        Decision::Hand {
            to: "s-nobody".to_string(),
        },
        "s-tie",
        2_000,
    ) {
        Err(Refused::UnknownSession { session }) => assert_eq!(session, "s-nobody"),
        other => panic!("the room cannot vouch for a stranger: {other:?}"),
    }
    room.vouch(&Speaker::subagent("s-kid", "s-a"))
        .expect_err("a subagent cannot be vouched for either");
    assert!(
        room.item(1).unwrap().question.is_some(),
        "the item stays frozen"
    );
}

#[test]
fn a_late_beat_is_refused_by_name_and_a_dispute_asks_the_arbiter() {
    let mut room = Room::new();
    let (a, b) = (Speaker::top_level("s-a"), Speaker::top_level("s-b"));
    let t = 1_000;
    room.post(&a, "p", "ram", "8 GB", t).unwrap();
    room.claim(&a, "c", "ram", t).unwrap();
    // `s-a`'s daemon was asleep: the beat stopped, the pass took the item back, and `s-b` has it.
    room.sweep(t + 200_000);
    room.claim(&b, "c2", "ram", t + 200_001).unwrap();

    match room.beat(&a, "late", 1, t + 200_002) {
        Err(Refused::NotHeld {
            session,
            item,
            holder,
        }) => {
            assert_eq!(session, "s-a");
            assert_eq!(item, 1);
            assert_eq!(holder.as_deref(), Some("s-b"));
        }
        other => panic!("a late beat is refused, and names who has it: {other:?}"),
    }

    // Rather than fight for it, `s-a` says it is still on it — and the arbiter is asked.
    let asked = room
        .dispute(
            &a,
            "d1",
            1,
            "the model was thinking, the suite is still running",
            t + 200_003,
        )
        .unwrap();
    let question = asked
        .item
        .question
        .and_then(|q| room.question(q))
        .expect("a dispute asks a question");
    assert!(matches!(
        question.about,
        About::ClaimDisputed { item: 1, .. }
    ));
    assert!(question.text.contains("s-a") && question.text.contains("s-b"));
    assert!(question.text.contains("still running"));
    assert!(question.text.contains("Hand") && question.text.contains("Retry"));

    // Asking twice is the same question, not a second one.
    let again = room
        .dispute(&a, "d2", 1, "still running", t + 200_004)
        .unwrap();
    assert_eq!(again.item.question, asked.item.question);
    assert_eq!(room.open_questions().len(), 1);

    // The arbiter gives it back to `s-a`; `s-b` is refused by name from then on.
    room.rule(
        asked.item.question.unwrap(),
        Decision::Hand {
            to: "s-a".to_string(),
        },
        "s-tie",
        t + 200_005,
    )
    .unwrap();
    assert_eq!(room.item(1).unwrap().holder(), Some("s-a"));
    assert!(matches!(
        room.beat(&b, "b-late", 1, t + 200_006),
        Err(Refused::NotHeld { holder: Some(h), .. }) if h == "s-a"
    ));
    // And the claim it was given is fresh: its beat is the ruling's own moment.
    assert_eq!(room.item(1).unwrap().claim().unwrap().beat_ms, t + 200_005);
}

#[test]
fn a_holder_disputing_its_own_item_is_told_there_is_nothing_to_rule_on() {
    let mut room = Room::new();
    let a = Speaker::top_level("s-a");
    room.post(&a, "p", "ram", "8 GB", 1_000).unwrap();
    room.claim(&a, "c", "ram", 1_000).unwrap();
    assert!(matches!(
        room.dispute(&a, "d", 1, "I have it", 1_001),
        Err(Refused::NotDisputed { item: 1 })
    ));
}
