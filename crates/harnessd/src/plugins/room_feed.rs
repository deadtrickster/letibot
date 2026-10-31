//! **The `room` feed** — one host's agents' queues, per tag, as a pane reads them.
//!
//! `docs/room.md` § *Still owed* 1 is the pane this feed exists for: the queues per tag, the
//! arbiter, and the person's four controls. The room keeps no store and lives in another process
//! (`harnessd --room`), so the daemon that draws the pane holds a [`RoomFeed`] — in production the
//! session's own connection to the room (`crate::room::link::SessionRoom`) — and this module is the
//! one place the snapshot it answers becomes the JSON a layer is handed.
//!
//! # What a row carries, and why each field is here
//!
//! | field | is | why |
//! |---|---|---|
//! | `tag` | the queue's name | a tag is a LOCK: the queue is the unit of contention |
//! | `state` | `open`/`claimed`/`abandoned`/`done`/`dropped` | the lifecycle, in the room's own words |
//! | `claimant` | who holds it | and `mine`, whether that is this session — the difference between *release* and nothing |
//! | `beat_age_ms` | how long since the holder last said it was working | the lease is measured on it, so `stale` is one comparison |
//! | `abandonments` | how many claimants it has lost | at [`letibot_room::ABANDON_THRESHOLD`] it freezes |
//! | `claimable` | may anybody take it now | the predicate, not `state == open` — an abandoned item is claimable and does not say `open` |
//!
//! # Two things only the host can answer
//!
//! * **The clock.** A guest compiled for `wasm32-unknown-unknown` has no `SystemTime` — asking for
//!   one is a panic, and a panic in a guest is a trap. So every age arrives as milliseconds against
//!   the host's `now_ms` and the layer subtracts nothing.
//! * **The room's own identity.** *Restarted clean* and *restarted empty* are the same thing here
//!   unless it is said: the room keeps no store, so an empty room is empty at every start, and
//!   `identity` is the sentence (`Server::identity`) that tells a reader how long it has been up and
//!   how much has happened since.
//!
//! # The clock is quantised, and that is not a detail
//!
//! The host answers a pane once per distinct `Input` (`sessions_pane::Host::ask` keys its cache on
//! the serialised input), so a feed carrying the clock to the millisecond is a cache that misses on
//! every frame: one wasm call and one serialisation of the whole room per draw. A row's finest fact
//! here is a `{}s` age, so a second's bucket cannot change a drawn frame — and it turns *a call per
//! tick* into *a call per second*. `.letibot/notes/a-clock-in-a-plugin-feed-is-a-wasm-call-per-frame.md`
//! is the measurement.

use std::collections::BTreeMap;

use serde_json::{Value, json};

/// The resolution of the feed's clock — one second, the finest thing a row can say.
pub const CLOCK_QUANTUM_MS: u64 = 1_000;

/// **Where a pane's view of the room comes from.** The daemon's one implementation speaks the room's
/// wire (`crate::room::link::SessionRoom`); a test stands in its own, which is what makes the shape
/// below testable with no socket and no coordinator.
pub trait RoomFeed: Send + Sync {
    /// The room as it stands, or the sentence saying why it could not be read.
    fn snapshot(&self) -> RoomSnapshot;

    /// **One of the person's four controls, on the entry a row names.** The HOST performs it; the
    /// layer only decided which row to draw. `Err` is the refusal, in the room's own words.
    fn act(&self, entry: &str, verb: &str) -> Result<String, String>;
}

/// **The room, as a pane reads it** — the state a `room` feed is built from, before the ages are
/// turned into milliseconds against one clock.
#[derive(Debug, Clone, Default)]
pub struct RoomSnapshot {
    /// `Server::identity()` — *started T ago and holding N item(s)*. The sentence that makes
    /// *restarted clean* and *restarted empty* two readings.
    pub identity: String,
    /// The session that arbitrates, when one is named.
    pub arbiter: Option<String>,
    /// Who is connected to the room now.
    pub connected: Vec<String>,
    pub lease_ms: u64,
    pub threshold: u32,
    /// Every item, oldest first, finished and dropped included.
    pub items: Vec<letibot_room::Item>,
    /// The open questions, oldest first.
    pub questions: Vec<letibot_room::Question>,
    /// **Why the room could not be read**, when it could not. The pane draws this sentence instead
    /// of an empty room: *nothing is queued* and *nobody answered* are two facts, and a screen that
    /// shows one for the other is a screen a person cannot act on.
    pub unreadable: Option<String>,
}

/// **The `room` feed as a pane is handed it** — the queues per tag, each row's ages against the
/// host's clock, the open questions, and the room's own identity.
pub fn room(source: &dyn RoomFeed, here: &str, now: u64) -> Value {
    let now = now / CLOCK_QUANTUM_MS * CLOCK_QUANTUM_MS;
    let snap = source.snapshot();
    let mut per_tag: BTreeMap<String, Vec<&letibot_room::Item>> = BTreeMap::new();
    for item in &snap.items {
        per_tag.entry(item.tag.clone()).or_default().push(item);
    }
    let mut tags: Vec<Value> = Vec::new();
    let mut all = Counts::default();
    for (tag, mut rows) in per_tag {
        // **First come, first served** — the room's own order (`(posted_ms, id)`), restated here
        // because this feed is a second reader of that rule and the two must agree about the head.
        rows.sort_by_key(|i| (i.posted_ms, i.id));
        let mut counts = Counts::default();
        let drawn: Vec<Value> = rows
            .iter()
            .map(|item| {
                counts.add(item);
                all.add(item);
                row_of(item, now, snap.lease_ms, here)
            })
            .collect();
        tags.push(json!({
            "tag": tag,
            "counts": counts.value(),
            "rows": drawn,
        }));
    }
    let questions: Vec<Value> = snap
        .questions
        .iter()
        .map(|q| {
            json!({
                "id": q.id,
                // **The entry a `rule` act names**, and the prefix the decision is appended to:
                // `q3:retry`, `q3:drop`, `q3:hand:<session>`. The decision is part of the row's
                // identity because it is what the person picks — a row that says *retry* is a row
                // whose Enter means retry.
                "entry": format!("q{}", q.id),
                "item": q.facts.item,
                "tag": q.facts.tag,
                "body": one_line(&q.facts.body),
                "about": match q.about {
                    letibot_room::About::ItemRaised { .. } => "item_raised",
                    letibot_room::About::ClaimDisputed { .. } => "claim_disputed",
                },
                "asked_age_ms": now.saturating_sub(q.asked_ms),
                "text": q.text,
                "lost": q.facts.abandonments.len(),
                "behind": q.facts.waiting.len(),
            })
        })
        .collect();
    json!({
        // **The host's clock**, so a layer that wants an age of its own can measure one.
        "now_ms": now,
        // **Which session's pane this is** — a row's `mine` is this session's claim.
        "here": here,
        "identity": snap.identity,
        "arbiter": snap.arbiter.as_ref().map(|session| json!({
            "session": session,
            // **Is it up?** The socket is the daemon's liveness, so *connected* is the answer —
            // and an arbiter that is not is a room whose questions wait for a person.
            "up": snap.connected.iter().any(|c| c == session),
        })),
        "connected": snap.connected,
        "lease_ms": snap.lease_ms,
        "threshold": snap.threshold,
        "unreadable": snap.unreadable,
        "tags": tags,
        "questions": questions,
        "counts": all.value(),
    })
}

/// What one tag's queue — or the whole room — holds, by state.
#[derive(Debug, Default, Clone, Copy)]
struct Counts {
    items: usize,
    live: usize,
    claimable: usize,
    claimed: usize,
    abandoned: usize,
    frozen: usize,
    done: usize,
    dropped: usize,
}

impl Counts {
    fn add(&mut self, item: &letibot_room::Item) {
        self.items += 1;
        if item.live() {
            self.live += 1;
        }
        if item.claimable() {
            self.claimable += 1;
        }
        match &item.state {
            letibot_room::State::Claimed { .. } => self.claimed += 1,
            letibot_room::State::Abandoned { .. } => self.abandoned += 1,
            letibot_room::State::Done { .. } => self.done += 1,
            letibot_room::State::Dropped { .. } => self.dropped += 1,
            letibot_room::State::Open => {}
        }
        // **Frozen is its own count**, not a sixth state: a raised item is `abandoned` AND
        // unclaimable, and a reader counting states alone would say its tag has work in it.
        if item.question.is_some() {
            self.frozen += 1;
        }
    }

    fn value(self) -> Value {
        json!({
            "items": self.items,
            "live": self.live,
            "claimable": self.claimable,
            "claimed": self.claimed,
            "abandoned": self.abandoned,
            "frozen": self.frozen,
            "done": self.done,
            "dropped": self.dropped,
        })
    }
}

/// **One item, as a row is drawn from it** — the state, the claimant, the age of its beat and the
/// count of claimants it has lost, which is the whole of what a reader needs to decide whether to
/// leave it alone or take it.
fn row_of(item: &letibot_room::Item, now: u64, lease_ms: u64, here: &str) -> Value {
    let claim = item.claim();
    let beat_age = claim.map(|c| now.saturating_sub(c.beat_ms));
    json!({
        "id": item.id,
        // **The entry the acts name** — `i12`. The `i` is what tells the host's parse which of the
        // two kinds of entry it was handed.
        "entry": format!("i{}", item.id),
        "tag": item.tag,
        "body": one_line(&item.body),
        "state": item.state_name(),
        "posted_by": item.posted_by,
        "posted_age_ms": now.saturating_sub(item.posted_ms),
        "claimant": claim.map(|c| c.by.clone()),
        "claim_age_ms": claim.map(|c| now.saturating_sub(c.claim_ms)),
        // **The age of its beat** — the number the lease is measured on, and the one fact that says
        // whether this claim is about to be taken back.
        "beat_age_ms": beat_age,
        "stale": beat_age.is_some_and(|age| age > lease_ms),
        "waits_on": claim.and_then(|c| c.waits_on.as_ref().map(|w| w.text.clone())),
        "abandonments": item.abandonments(),
        "last_why": item.last_abandonment().map(|a| a.why.to_string()),
        "claimable": item.claimable(),
        "frozen": item.question.is_some(),
        "question": item.question,
        // **Is this session the claimant?** A row this session holds leads with `release`, so the
        // pane has to be told — a layer cannot work it out from an id it was never given.
        "mine": claim.is_some_and(|c| c.by == here),
        "live": item.live(),
    })
}

/// **A body on one line** — an item's body is a person's sentence and may be several, and a control
/// character in a row is a row that breaks the grid it is drawn in.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use letibot_room::{Item, State};

    /// The host's clock in a fixture: every age below is a subtraction from it.
    const NOW: u64 = 1_000_000;

    fn open(id: u64, tag: &str, body: &str, by: &str, posted_ms: u64) -> Item {
        Item {
            id,
            tag: tag.to_string(),
            body: body.to_string(),
            posted_by: by.to_string(),
            posted_ms,
            state: State::Open,
            abandonments: Vec::new(),
            question: None,
        }
    }

    fn claimed(id: u64, tag: &str, by: &str, claim_ms: u64, beat_ms: u64) -> Item {
        Item {
            state: State::Claimed {
                claim: letibot_room::Claim {
                    by: by.to_string(),
                    claim_ms,
                    beat_ms,
                    waits_on: None,
                },
            },
            // **Posted before it was claimed**, which is the only order the room can produce —
            // and the one the feed's sort is measured against.
            ..open(id, tag, "8 GB", "s-a", claim_ms)
        }
    }

    fn abandoned(id: u64, tag: &str, times: usize) -> Item {
        let mut item = open(id, tag, "the staging database", "s-a", 0);
        item.state = State::Abandoned {
            last: letibot_room::Abandonment {
                by: "s-b".to_string(),
                claim_ms: 1,
                beat_ms: 2,
                ms: 3,
                why: letibot_room::Why::BeatStale { age_ms: 90_001 },
            },
        };
        item.abandonments = (0..times)
            .map(|_| letibot_room::Abandonment {
                by: "s-b".to_string(),
                claim_ms: 1,
                beat_ms: 2,
                ms: 3,
                why: letibot_room::Why::BeatStale { age_ms: 90_001 },
            })
            .collect();
        item
    }

    fn snapshot(items: Vec<Item>) -> RoomSnapshot {
        RoomSnapshot {
            identity: "the room on /tmp/room.sock — started 4s ago and holding 3 item(s)"
                .to_string(),
            arbiter: Some("s-room-abc".to_string()),
            connected: vec!["s-a".to_string()],
            lease_ms: 90_000,
            threshold: 3,
            items,
            questions: Vec::new(),
            unreadable: None,
        }
    }

    struct Fixed(RoomSnapshot);

    impl RoomFeed for Fixed {
        fn snapshot(&self) -> RoomSnapshot {
            self.0.clone()
        }
        fn act(&self, _entry: &str, _verb: &str) -> Result<String, String> {
            Err("a fixture performs nothing".to_string())
        }
    }

    fn feed(snap: RoomSnapshot) -> Value {
        room(&Fixed(snap), "s-a", NOW)
    }

    /// **The feed's shape, on the three rows a pane is read for**: a tag with two items, one of them
    /// claimed with the age of its beat, and an abandoned one with its count.
    #[test]
    fn a_tag_with_two_items_and_a_claimed_one_carries_its_beat_age() {
        let value = feed(snapshot(vec![
            open(1, "ram", "8 GB", "s-a", NOW - 60_000),
            claimed(2, "ram", "s-b", NOW - 30_000, NOW - 2_000),
            abandoned(3, "disk", 2),
        ]));
        assert_eq!(value["now_ms"], json!(NOW));
        assert_eq!(value["here"], json!("s-a"));
        assert_eq!(value["lease_ms"], json!(90_000));
        assert_eq!(value["threshold"], json!(3));

        // **Two tags, in name order, each with its own queue.**
        assert_eq!(value["tags"].as_array().unwrap().len(), 2);
        assert_eq!(value["tags"][0]["tag"], json!("disk"));
        assert_eq!(value["tags"][1]["tag"], json!("ram"));

        let ram = &value["tags"][1]["rows"];
        assert_eq!(ram.as_array().unwrap().len(), 2, "first come, first served");
        assert_eq!(ram[0]["entry"], json!("i1"));
        assert_eq!(ram[0]["state"], json!("open"));
        assert_eq!(ram[0]["claimable"], json!(true));
        assert_eq!(ram[0]["posted_age_ms"], json!(60_000));
        assert_eq!(ram[0]["claimant"], json!(null));

        // **The claimed one**: who holds it, the age of its beat, and the held time.
        assert_eq!(ram[1]["entry"], json!("i2"));
        assert_eq!(ram[1]["state"], json!("claimed"));
        assert_eq!(ram[1]["claimant"], json!("s-b"));
        assert_eq!(ram[1]["beat_age_ms"], json!(2_000));
        assert_eq!(ram[1]["claim_age_ms"], json!(30_000));
        assert_eq!(ram[1]["stale"], json!(false), "two seconds is not ninety");
        assert_eq!(ram[1]["claimable"], json!(false));
        assert_eq!(ram[1]["mine"], json!(false), "`s-a` is not the claimant");

        // **The abandoned one**: its count, and that it is claimable again — the state says
        // `abandoned` and the predicate says yes, which is why `claimable` is a field.
        let disk = &value["tags"][0]["rows"][0];
        assert_eq!(disk["state"], json!("abandoned"));
        assert_eq!(disk["abandonments"], json!(2));
        assert_eq!(disk["claimable"], json!(true));
        assert_eq!(
            disk["last_why"],
            json!("its beat stopped — the last one was 1m 30s old")
        );

        // And the counts a header reads.
        assert_eq!(value["counts"]["items"], json!(3));
        assert_eq!(value["counts"]["claimed"], json!(1));
        assert_eq!(value["counts"]["abandoned"], json!(1));
        assert_eq!(value["counts"]["claimable"], json!(2));
        assert_eq!(value["tags"][1]["counts"]["items"], json!(2));
    }

    /// **A claim whose beat has stopped is `stale`**, and the lease is what decides it.
    #[test]
    fn a_beat_older_than_the_lease_is_stale_and_one_at_the_boundary_is_not() {
        let value = feed(snapshot(vec![
            claimed(1, "ram", "s-b", NOW - 200_000, NOW - 90_000),
            claimed(2, "ram", "s-b", NOW - 200_000, NOW - 90_001),
        ]));
        let rows = &value["tags"][0]["rows"];
        assert_eq!(
            rows[0]["stale"],
            json!(false),
            "a beat at the boundary is alive"
        );
        assert_eq!(rows[1]["stale"], json!(true), "one past it is not");
    }

    /// **The identity, the arbiter and who is connected** — the three facts that make an empty room
    /// readable, and the ones a pane prints before it prints a queue.
    #[test]
    fn the_room_says_who_it_is_and_whether_the_arbiter_is_up() {
        let mut snap = snapshot(Vec::new());
        let value = room(&Fixed(snap.clone()), "s-a", NOW);
        assert_eq!(value["identity"], json!(snap.identity));
        assert_eq!(value["arbiter"]["session"], json!("s-room-abc"));
        assert_eq!(
            value["arbiter"]["up"],
            json!(false),
            "the arbiter is named and is not connected — a question would wait"
        );
        assert_eq!(value["counts"]["items"], json!(0));

        snap.connected.push("s-room-abc".to_string());
        let value = room(&Fixed(snap), "s-a", NOW);
        assert_eq!(value["arbiter"]["up"], json!(true));
    }

    /// **A room that could not be read says so** rather than drawing an empty queue.
    #[test]
    fn an_unreadable_room_carries_the_sentence() {
        let snap = RoomSnapshot {
            unreadable: Some("the room at /tmp/room.sock did not answer in 10s".to_string()),
            ..RoomSnapshot::default()
        };
        let value = feed(snap);
        assert_eq!(
            value["unreadable"],
            json!("the room at /tmp/room.sock did not answer in 10s")
        );
        assert_eq!(value["tags"], json!([]));
    }

    /// **A row's body is one line**, whatever a person typed into it.
    #[test]
    fn a_body_is_flattened_to_one_line() {
        let mut item = open(1, "ram", "8 GB\nand a\tfast disk", "s-a", 0);
        item.body = "8 GB\nand a\tfast disk".to_string();
        let value = feed(snapshot(vec![item]));
        assert_eq!(
            value["tags"][0]["rows"][0]["body"],
            json!("8 GB and a fast disk")
        );
    }

    /// **The clock is quantised to the second**, so a pane's cache hits for the rest of it.
    #[test]
    fn the_clock_moves_once_a_second() {
        let snap = snapshot(vec![claimed(1, "ram", "s-b", NOW, NOW - 4_000)]);
        let first = room(&Fixed(snap.clone()), "s-a", NOW + 100);
        let second = room(&Fixed(snap.clone()), "s-a", NOW + 900);
        assert_eq!(first, second, "two frames inside one second are one feed");
        let third = room(&Fixed(snap), "s-a", NOW + 1_000);
        assert_ne!(first, third, "the frame after the tick is the next second");
        assert_eq!(first["now_ms"], json!(NOW));
        assert_eq!(third["now_ms"], json!(NOW + 1_000));
    }
}
