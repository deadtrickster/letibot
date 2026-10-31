//! **The room: one host's sessions, one queue per tag, and a claim that is a lease with a beat.**
//!
//! This module is the state machine and nothing else: no store, no socket, no clock. Every time in
//! it is handed in by the caller, every item lives in this process's memory, and the whole of the
//! transport is [`crate::wire`]. That is deliberate — the coordinator daemon that will own the
//! socket is the next slice, and a state machine that needs a socket to be tested is a state
//! machine whose bugs are found by hand.
//!
//! # A tag is a lock, and its queue is first come, first served
//!
//! **One live claim per tag.** A session claims *the head of a tag*, and while that claim lives no
//! other session may claim anything under that tag — it is refused [`Refused::Busy`], naming the
//! holder and the item. That is the RAM example's whole shape: three sessions each want the same
//! scarce thing, and only one of them may run.
//!
//! **Order is `(posted_ms, id)`** — first come, first served, and the room's own id breaks a tie
//! within one millisecond. **There is no priority number, on purpose.** A numeric priority invites
//! every session to claim 0, and then the number carries no information and the room has a tie it
//! cannot settle without the arbiter; a tag with one head and no ordering is what the RAM example
//! actually needs, and urgent work gets its own tag (`ram-urgent`) — a name a reader can see, in
//! place of a number nobody can rank.
//!
//! The ordering has one property worth stating because it is load-bearing: **an abandoned item
//! returns to the head**, since it was posted before everything behind it. The queue moves on from
//! a claimant, not from the request.
//!
//! # The claim is a lease, and there are two ways to lose it
//!
//! A claim carries `claim_ms` and `beat_ms` — the `queue_lease`/`host_lease` discipline
//! ([`crate::DEFAULT_LEASE_MS`] is that queue's own ninety seconds). It is lost in two ways, and
//! they are different facts:
//!
//! * **[`Room::sweep`] — the beat stopped.** `now - beat_ms > lease_ms` and the item is abandoned
//!   with [`Why::BeatStale`]. A session can be alive and simply not working on it (a turn ended, a
//!   model stopped calling `beat`), which is exactly the *"so they don't occupy it for ever"* case.
//! * **[`Room::note_gone`] — the session is known gone.** Its daemon said it exited, or the socket
//!   that owns it closed. A positive fact, so it does not wait out the lease.
//!
//! A third way is the holder's own [`Room::abandon`] — a voluntary release. It is still an
//! abandonment and still counted: an item everybody gives up on is a problem whether or not each
//! gave up politely, and a rule with an exception is a rule somebody has to explain.
//!
//! **An abandonment is loud.** It is an [`Event::Abandoned`] carrying the reason, the count and
//! whether the item is back in the queue, and the item keeps every abandonment in its history. The
//! item is *not* silently dropped: the state says `abandoned` so a reader cannot mistake a
//! re-queued item for one nobody ever tried.
//!
//! # The count, and when an item raises instead of cycling
//!
//! [`ABANDON_THRESHOLD`] is **three**. The first abandonment is usually an accident — a daemon
//! restarted, a session killed, a box that slept — and one is far too few to give up on a request.
//! The second says the work is fragile or the claimant picked badly. At the third, cycling costs
//! more than asking: three different claimants have now been killed by this item, and the next one
//! would be chosen by who is fastest rather than by who can do it. So the third abandonment
//! **raises** the item: it is marked abandoned, the count stops moving, and it is no longer
//! claimable until a ruling answers a [`Question`]. Its tag's queue moves past it to the next
//! item — which is the point: an item that keeps killing its claimants must not block the tag for
//! ever.
//!
//! # Where the tie-breaker decides
//!
//! The arbiter is a model, so it is asked **a question with a decision in it** — never a dashboard
//! to interpret. [`Question::text`] is that question and [`Question::facts`] is the evidence it
//! names, so the model can check the words against the numbers.
//!
//! Three situations need a ruling, and the first is the one this slice implements end to end:
//!
//! 1. **An item that has abandoned its claimant three times** — [`About::ItemRaised`]. The question
//!    names the item, its body, each claimant it lost and why, and who is queued behind it (the
//!    cost of keeping it), and asks for one of [`Decision::Retry`], [`Decision::Hand`] or
//!    [`Decision::Drop`]. Until it is answered the item is frozen.
//! 2. **An abandonment under dispute** — [`About::ClaimDisputed`]. A claimant comes back after its
//!    beat went stale (its daemon was paused, the box slept, its model was thinking) and finds the
//!    item abandoned and perhaps claimed by somebody else. The room refuses its late beat by name
//!    ([`Refused::NotHeld`]); [`Room::dispute`] is how it says *"I am still on it"* instead of
//!    fighting for the claim, and the arbiter is asked which of the two has it.
//! 3. **Two items at one tag's head** — the room's answer is `(posted_ms, id)`, deterministically,
//!    so the arbiter is never *needed* for it. It is reachable all the same: a session that
//!    believes its request should have gone first asks through [`Room::dispute`], and the question
//!    it produces carries both items' facts. That is the honest shape — the ordering rule is the
//!    room's, and a ruling is how a person overrides it, not a second ordering rule.
//!
//! **What is *not* a question for the arbiter: two coordinators.** This room is one process and one
//! clock, so "two claimants that each believe they are alive" cannot arise *inside* it — the room's
//! state is the truth. It arises only if two daemons both own a room, which is the next slice's
//! problem and is answered with a lease on the coordinator itself (the merge queue's own shape),
//! not with a model.
//!
//! # What the state machine assumes
//!
//! * **Time.** One clock, given as `now` in milliseconds, and it is the caller's. A clock that
//!   goes backwards is survived, not corrected: a beat stamps `now`, and the staleness test is
//!   `now.saturating_sub(beat_ms)`, so a beat from the future reads as fresh rather than as
//!   instantly stale.
//! * **Liveness.** The room knows two things about a session: the beat (it is working) and the
//!   caller's word (it is gone). It never asks the OS, never reads a store, and never guesses.
//! * **One writer.** No method here is `&self`-and-mutating behind a lock; the room is `&mut`
//!   throughout, so the process that owns it is the only writer by construction.
//! * **Ordering of events.** [`Room::sweep`] and [`Room::note_gone`] return the events they
//!   produced, in the order they happened; every other verb returns its own in [`Outcome::events`].
//!   A caller that drops them has dropped the bell, not the state.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::item::{Abandonment, Claim, Item, Speaker, State, Wait, Why, duration};

/// **Ninety seconds, and it is the merge queue's own number** — eighteen missed beats at a five
/// second beat, so a daemon held up by a slow write is not mistaken for a dead one, and a dead one
/// is replaced within a minute and a half (`mergequeue::lease::LEASE_TTL_MS`).
pub const DEFAULT_LEASE_MS: u64 = 90_000;

/// How often a holder is expected to say it is alive — the merge queue's `HEARTBEAT_EVERY`.
pub const DEFAULT_BEAT_EVERY_MS: u64 = 5_000;

/// **How many claimants an item may lose before it raises instead of cycling.** See the module
/// header for why three.
pub const ABANDON_THRESHOLD: u32 = 3;

/// **How many verb replies are remembered for a reconnecting client** — a duplicate `post` or a
/// repeated `claim` after a reconnect is answered from here instead of acted on twice. A client
/// that has been away for longer than this must re-read ([`Room::queue`], [`Room::holding`])
/// rather than replay: every reply carries the item, so a re-read is cheap and always right.
pub const REPLAY_WINDOW: usize = 256;

/// **The room.** See the module header.
#[derive(Debug)]
pub struct Room {
    lease_ms: u64,
    items: BTreeMap<u64, Item>,
    next_item: u64,
    questions: Vec<Question>,
    next_question: u64,
    /// **Who the room has met** — session id to the parent it declared, or `None` for a top-level
    /// session. A ruling may name a session ([`Decision::Hand`]) that has never spoken here, and
    /// the daemon vouches for it with [`Room::vouch`]; this is what makes "top-level only" true
    /// for a hand as well as for a verb.
    seen: BTreeMap<String, Option<String>>,
    replay: BTreeMap<(String, String), Outcome>,
    replay_order: VecDeque<(String, String)>,
}

/// **What a verb did** — the item as it now stands, and what a subscriber should be told.
///
/// `events` is empty for a verb that is not news: a beat, a `waits_on`, and any *repeat* of a verb
/// already answered from the replay window (the bell rang the first time).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    pub item: Item,
    pub events: Vec<Event>,
}

/// **Something a session may need to be woken by.** Beats are not events — a beat is a fact about
/// one claim, and a bell that rings five times a minute is a bell nobody listens to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Posted {
        item: u64,
        tag: String,
        by: String,
    },
    Claimed {
        item: u64,
        tag: String,
        by: String,
    },
    Done {
        item: u64,
        tag: String,
        by: String,
    },
    /// **Loud, by design.** `count` is how many claimants the item has now lost and
    /// `back_in_the_queue` is whether it is claimable again (`count < ABANDON_THRESHOLD`).
    Abandoned {
        item: u64,
        tag: String,
        by: String,
        why: Why,
        count: u32,
        back_in_the_queue: bool,
    },
    /// The item reached the threshold and is frozen until a ruling — see [`Question`].
    Raised {
        item: u64,
        tag: String,
        question: u64,
        count: u32,
    },
    Asked {
        item: u64,
        tag: String,
        question: u64,
    },
    Ruled {
        item: u64,
        question: u64,
        decision: Decision,
        by: String,
    },
}

impl Event {
    /// The item the event is about. Every event is about one.
    pub fn item(&self) -> u64 {
        match self {
            Event::Posted { item, .. }
            | Event::Claimed { item, .. }
            | Event::Done { item, .. }
            | Event::Abandoned { item, .. }
            | Event::Raised { item, .. }
            | Event::Asked { item, .. }
            | Event::Ruled { item, .. } => *item,
        }
    }

    /// **The tag the event is about** — which is the audience's own name: the sessions queued under
    /// a tag are the ones a claim or an abandonment under it concerns.
    pub fn tag(&self) -> &str {
        match self {
            Event::Posted { tag, .. }
            | Event::Claimed { tag, .. }
            | Event::Done { tag, .. }
            | Event::Abandoned { tag, .. }
            | Event::Raised { tag, .. }
            | Event::Asked { tag, .. } => tag,
            // A ruling names the item and the decision, not the tag — the server asks the room for
            // it (`Room::item`), because the room is the one place a tag is decided.
            Event::Ruled { .. } => "",
        }
    }

    /// **The sessions this event NAMES**, beyond whoever is queued under its tag: the one who did
    /// it, and the one a ruling hands something to. A server routes to these by name even when they
    /// have no item under the tag — a ruling that hands an item to a session is the case.
    pub fn named(&self) -> Vec<&str> {
        match self {
            Event::Posted { by, .. }
            | Event::Claimed { by, .. }
            | Event::Done { by, .. }
            | Event::Abandoned { by, .. }
            | Event::Ruled { by, .. } => vec![by.as_str()],
            Event::Raised { .. } | Event::Asked { .. } => Vec::new(),
        }
    }

    /// The question the event is about, when it is about one.
    pub fn question(&self) -> Option<u64> {
        match self {
            Event::Raised { question, .. }
            | Event::Asked { question, .. }
            | Event::Ruled { question, .. } => Some(*question),
            _ => None,
        }
    }
}

/// **Why a verb was refused.** Every sentence names the party it is about: a refusal that says
/// *no* without saying *who* is a refusal somebody re-asks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "refused", rename_all = "snake_case")]
pub enum Refused {
    /// **A subagent speaks through its parent.** The rule, at the door.
    Subagent {
        session: String,
        parent: String,
    },
    EmptyTag,
    EmptyBody,
    /// A verb with no request id cannot be answered twice, so it cannot survive a reconnect.
    EmptyRequest,
    UnknownItem {
        item: u64,
    },
    UnknownQuestion {
        question: u64,
    },
    UnknownSession {
        session: String,
    },
    AlreadyRuled {
        question: u64,
        by: String,
        ms: u64,
    },
    /// The tag has nothing claimable — empty, or everything under it is claimed or finished.
    NothingToClaim {
        tag: String,
    },
    /// You do not hold the item you are trying to beat, wait on, finish or abandon.
    NotHeld {
        session: String,
        item: u64,
        holder: Option<String>,
    },
    /// **A tag is a lock**: somebody holds a claim under it.
    Busy {
        tag: String,
        item: u64,
        by: String,
        /// How long the holder has had it, as a duration a person reads (`1m 4s`).
        held_for: String,
    },
    /// First come, first served — and this is not the one at the front.
    NotTheHead {
        item: u64,
        tag: String,
        head: u64,
    },
    /// Finished or dropped: not in the queue any more.
    Gone {
        item: u64,
        state: String,
    },
    /// Frozen until its question is answered.
    Raised {
        item: u64,
        count: u32,
        question: u64,
    },
    /// A dispute about an item you hold yourself — there is nothing to rule on.
    NotDisputed {
        item: u64,
    },
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refused::Subagent { session, parent } => write!(
                f,
                "`{session}` is a subagent of `{parent}` — a subagent speaks through its parent, \
                 so it cannot post or claim in the room"
            ),
            Refused::EmptyTag => write!(f, "a tag is the name of the queue and cannot be empty"),
            Refused::EmptyBody => write!(
                f,
                "an item with no body says nothing — put the need in the body"
            ),
            Refused::EmptyRequest => write!(
                f,
                "every verb needs a request id, so that a reconnecting client's repeat is \
                 answered instead of acted on twice"
            ),
            Refused::UnknownItem { item } => write!(f, "there is no item #{item} in the room"),
            Refused::UnknownQuestion { question } => {
                write!(f, "there is no question #{question} in the room")
            }
            Refused::UnknownSession { session } => write!(
                f,
                "`{session}` has never spoken in this room, so the room cannot say whether it is \
                 a top-level session — the daemon vouches for it first"
            ),
            Refused::AlreadyRuled { question, by, ms } => write!(
                f,
                "question #{question} was already answered by `{by}` at {ms}"
            ),
            Refused::NothingToClaim { tag } => {
                write!(f, "there is nothing to claim under `{tag}`")
            }
            Refused::NotHeld {
                session,
                item,
                holder,
            } => match holder {
                Some(h) => write!(f, "`{session}` does not hold #{item} — `{h}` does"),
                None => write!(f, "`{session}` does not hold #{item} — nobody does"),
            },
            Refused::Busy {
                tag,
                item,
                by,
                held_for,
            } => write!(
                f,
                "tag `{tag}` is held by `{by}` on #{item} for {held_for} — a tag is a lock, so \
                 wait for it or post under another tag"
            ),
            Refused::NotTheHead { item, tag, head } => write!(
                f,
                "#{item} is not the head of `{tag}` — #{head} is, and a tag is first come, first \
                 served"
            ),
            Refused::Gone { item, state } => {
                write!(f, "#{item} is {state} and is not in the queue any more")
            }
            Refused::Raised {
                item,
                count,
                question,
            } => write!(
                f,
                "#{item} has abandoned {count} claimants and is waiting for a ruling \
                 (question #{question})"
            ),
            Refused::NotDisputed { item } => {
                write!(f, "you hold #{item} yourself — there is nothing to rule on")
            }
        }
    }
}

impl std::error::Error for Refused {}

/// **A question for the tie-breaker** — see the module header for the three situations that
/// produce one, and for why the framing is a question rather than a dashboard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Question {
    pub id: u64,
    pub asked_ms: u64,
    pub about: About,
    /// The question, in words, with the facts in it. A model answers this with a [`Decision`].
    pub text: String,
    pub facts: Facts,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<Ruling>,
}

/// What a question is about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "about", rename_all = "snake_case")]
pub enum About {
    /// An item that has abandoned its claimant [`ABANDON_THRESHOLD`] times.
    ItemRaised { item: u64, tag: String },
    /// A claimant that says it is still working on an item the room took back.
    ClaimDisputed { item: u64, tag: String, by: String },
}

/// **The evidence a question names**, so the arbiter can check the words against the numbers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Facts {
    pub item: u64,
    pub tag: String,
    pub body: String,
    pub posted_by: String,
    pub posted_ms: u64,
    pub claimed_by: Option<String>,
    pub claim_ms: Option<u64>,
    pub beat_ms: Option<u64>,
    pub abandonments: Vec<Abandonment>,
    /// Who is queued behind it, oldest first — the cost of keeping it alive.
    pub waiting: Vec<Waiting>,
}

/// One item queued behind the one a question is about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Waiting {
    pub item: u64,
    pub by: String,
    pub since_ms: u64,
}

/// **What the arbiter may decide.** Three, because the question is about one item and the answers
/// are *try again*, *give it to somebody*, or *stop asking*.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum Decision {
    /// Offer it again. The count is kept, so a fourth abandonment raises it again at once.
    Retry,
    /// Give the claim to this session, now.
    Hand { to: String },
    /// Take it out of the queue for good, with the reason on the item.
    Drop { why: String },
}

impl fmt::Display for Decision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Decision::Retry => write!(f, "retry"),
            Decision::Hand { to } => write!(f, "hand it to `{to}`"),
            Decision::Drop { why } => write!(f, "drop it: {why}"),
        }
    }
}

/// **The answer**, and who gave it — a ruling is a decision with an author, because a pane that
/// cannot say who ruled cannot say whether to trust it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ruling {
    pub decision: Decision,
    pub by: String,
    pub ms: u64,
}

/// **What one session holds**, and what it says it is waiting on — the answer to *what am I
/// holding*, which is the shape a todo reminder needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Holding {
    pub item: u64,
    pub tag: String,
    pub body: String,
    pub claim_ms: u64,
    pub beat_ms: u64,
    /// How long it has been held, as of the `now` the question was asked at.
    pub held_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waits_on: Option<Wait>,
}

/// **One line a session should be reminded of** — a holding, plus whether its beat has gone quiet
/// enough that the next [`Room::sweep`] will take the item back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reminder {
    pub session: String,
    pub holding: Holding,
    /// `true` when `now - beat_ms > lease_ms`: the room will take this back at its next pass.
    pub stale: bool,
}

impl Reminder {
    /// The sentence a todo reminder shows. Not a delivery mechanism — the sentence is the shape.
    pub fn sentence(&self) -> String {
        let h = &self.holding;
        let mut s = format!(
            "you hold `{}` #{} — `{}` — for {}",
            h.tag,
            h.item,
            h.body,
            duration(h.held_ms)
        );
        if let Some(w) = &h.waits_on {
            s.push_str(&format!(
                "; you are waiting on `{}` ({})",
                w.text,
                duration(
                    self.holding
                        .held_ms
                        .saturating_sub(w.since_ms.saturating_sub(h.claim_ms))
                )
            ));
        }
        if self.stale {
            s.push_str(
                "; your beat has stopped and the room will take it back — beat or release it",
            );
        }
        s
    }
}

impl Default for Room {
    fn default() -> Self {
        Self::new()
    }
}

impl Room {
    /// A room with the merge queue's own lease ([`DEFAULT_LEASE_MS`]).
    pub fn new() -> Self {
        Self::with_lease_ms(DEFAULT_LEASE_MS)
    }

    /// A room whose claims go stale after `lease_ms` of silence.
    pub fn with_lease_ms(lease_ms: u64) -> Self {
        Self {
            lease_ms,
            items: BTreeMap::new(),
            next_item: 0,
            questions: Vec::new(),
            next_question: 0,
            seen: BTreeMap::new(),
            replay: BTreeMap::new(),
            replay_order: VecDeque::new(),
        }
    }

    pub fn lease_ms(&self) -> u64 {
        self.lease_ms
    }

    /// **Declare a session the room has not met** — the daemon's word for a session that has not
    /// spoken yet (the tie-breaker's own, or one the operator names in the pane). A subagent is
    /// refused here exactly as it is at a verb.
    pub fn vouch(&mut self, who: &Speaker) -> Result<(), Refused> {
        who.may_speak()?;
        self.seen.insert(who.session.clone(), who.parent.clone());
        Ok(())
    }

    // ===== The verbs =====

    /// **Post a need under a tag.**
    pub fn post(
        &mut self,
        who: &Speaker,
        request: &str,
        tag: &str,
        body: &str,
        now: u64,
    ) -> Result<Outcome, Refused> {
        // **Identity before dedupe**: a subagent is refused by name even when it reuses an id.
        who.may_speak()?;
        if let Some(o) = self.replayed(who, request)? {
            return Ok(o);
        }
        let tag = normalise_tag(tag)?;
        let body = body.trim();
        if body.is_empty() {
            return Err(Refused::EmptyBody);
        }
        self.next_item += 1;
        let id = self.next_item;
        let item = Item {
            id,
            tag: tag.clone(),
            body: body.to_string(),
            posted_by: who.session.clone(),
            posted_ms: now,
            state: State::Open,
            abandonments: Vec::new(),
            question: None,
        };
        self.items.insert(id, item.clone());
        self.met(who);
        let out = Outcome {
            item,
            events: vec![Event::Posted {
                item: id,
                tag,
                by: who.session.clone(),
            }],
        };
        self.remember(who, request, &out);
        Ok(out)
    }

    /// **Claim the head of a tag** — refused [`Refused::Busy`] while somebody else holds the tag,
    /// naming them and the item. A repeat by the session that already holds it is the same claim:
    /// no second `claim_ms`, and no second event.
    pub fn claim(
        &mut self,
        who: &Speaker,
        request: &str,
        tag: &str,
        now: u64,
    ) -> Result<Outcome, Refused> {
        // **Identity before dedupe**: a subagent is refused by name even when it reuses an id.
        who.may_speak()?;
        if let Some(o) = self.replayed(who, request)? {
            return Ok(o);
        }
        let tag = normalise_tag(tag)?;
        if let Some(held) = self.live_claim(&tag) {
            if held.by == who.session {
                return Ok(Outcome {
                    item: held.item,
                    events: Vec::new(),
                });
            }
            return Err(Refused::Busy {
                tag,
                item: held.item_id,
                by: held.by,
                held_for: duration(now.saturating_sub(held.claim_ms)),
            });
        }
        let head = self
            .head(&tag)
            .map(|i| i.id)
            .ok_or_else(|| Refused::NothingToClaim { tag: tag.clone() })?;
        self.take(who, head, now)
    }

    /// **Claim one particular item** — the operator's manual control, and a client that read the
    /// queue and means *that* row. It must still be the head: a tag is first come, first served,
    /// and an item id is not a way to jump the queue.
    pub fn claim_item(
        &mut self,
        who: &Speaker,
        request: &str,
        item: u64,
        now: u64,
    ) -> Result<Outcome, Refused> {
        // **Identity before dedupe**: a subagent is refused by name even when it reuses an id.
        who.may_speak()?;
        if let Some(o) = self.replayed(who, request)? {
            return Ok(o);
        }
        let it = self
            .item(item)
            .ok_or(Refused::UnknownItem { item })?
            .clone();
        if let Some(claim) = it.claim() {
            if claim.by == who.session {
                return Ok(Outcome {
                    item: it,
                    events: Vec::new(),
                });
            }
            return Err(Refused::Busy {
                tag: it.tag.clone(),
                item,
                by: claim.by.clone(),
                held_for: duration(now.saturating_sub(claim.claim_ms)),
            });
        }
        if !it.live() {
            return Err(Refused::Gone {
                item,
                state: it.state_name().to_string(),
            });
        }
        if let Some(q) = it.question {
            return Err(Refused::Raised {
                item,
                count: it.abandonments(),
                question: q,
            });
        }
        if let Some(held) = self.live_claim(&it.tag) {
            return Err(Refused::Busy {
                tag: it.tag.clone(),
                item: held.item_id,
                by: held.by,
                held_for: duration(now.saturating_sub(held.claim_ms)),
            });
        }
        let head = self
            .head(&it.tag)
            .map(|i| i.id)
            .ok_or_else(|| Refused::NothingToClaim {
                tag: it.tag.clone(),
            })?;
        if head != item {
            return Err(Refused::NotTheHead {
                item,
                tag: it.tag.clone(),
                head,
            });
        }
        self.take(who, item, now)
    }

    /// **Say you are still working on it.** Only the holder may beat; a late beat from a session
    /// whose claim was taken is refused by name ([`Refused::NotHeld`]) — see [`Room::dispute`] for
    /// what to do about it.
    pub fn beat(
        &mut self,
        who: &Speaker,
        request: &str,
        item: u64,
        now: u64,
    ) -> Result<Outcome, Refused> {
        // **Identity before dedupe**: a subagent is refused by name even when it reuses an id.
        who.may_speak()?;
        if let Some(o) = self.replayed(who, request)? {
            return Ok(o);
        }
        let it = self
            .item(item)
            .ok_or(Refused::UnknownItem { item })?
            .clone();
        let claim = it.claim().ok_or(Refused::NotHeld {
            session: who.session.clone(),
            item,
            holder: None,
        })?;
        if claim.by != who.session {
            return Err(Refused::NotHeld {
                session: who.session.clone(),
                item,
                holder: Some(claim.by.clone()),
            });
        }
        let it = self.items.get_mut(&item).expect("checked above");
        if let State::Claimed { claim } = &mut it.state {
            claim.beat_ms = now;
        }
        let it = it.clone();
        self.met(who);
        let out = Outcome {
            item: it,
            events: Vec::new(),
        };
        self.remember(who, request, &out);
        Ok(out)
    }

    /// **Say what you are waiting on**, in your own words. Empty text clears it. Only the holder
    /// may, and it is the answer to *"so they don't occupy it for ever"*: the reminder carries it
    /// back, and the queue shows it to whoever is waiting behind.
    pub fn waiting_on(
        &mut self,
        who: &Speaker,
        request: &str,
        item: u64,
        text: &str,
        now: u64,
    ) -> Result<Outcome, Refused> {
        // **Identity before dedupe**: a subagent is refused by name even when it reuses an id.
        who.may_speak()?;
        if let Some(o) = self.replayed(who, request)? {
            return Ok(o);
        }
        let it = self
            .item(item)
            .ok_or(Refused::UnknownItem { item })?
            .clone();
        let claim = it.claim().ok_or(Refused::NotHeld {
            session: who.session.clone(),
            item,
            holder: None,
        })?;
        if claim.by != who.session {
            return Err(Refused::NotHeld {
                session: who.session.clone(),
                item,
                holder: Some(claim.by.clone()),
            });
        }
        let text = text.trim();
        let it = self.items.get_mut(&item).expect("checked above");
        if let State::Claimed { claim } = &mut it.state {
            claim.waits_on = if text.is_empty() {
                None
            } else {
                Some(Wait {
                    text: text.to_string(),
                    since_ms: now,
                })
            };
        }
        let it = it.clone();
        self.met(who);
        let out = Outcome {
            item: it,
            events: Vec::new(),
        };
        self.remember(who, request, &out);
        Ok(out)
    }

    /// **The work is done.** Only the holder may.
    pub fn done(
        &mut self,
        who: &Speaker,
        request: &str,
        item: u64,
        now: u64,
    ) -> Result<Outcome, Refused> {
        // **Identity before dedupe**: a subagent is refused by name even when it reuses an id.
        who.may_speak()?;
        if let Some(o) = self.replayed(who, request)? {
            return Ok(o);
        }
        let it = self
            .item(item)
            .ok_or(Refused::UnknownItem { item })?
            .clone();
        let claim = it.claim().ok_or(Refused::NotHeld {
            session: who.session.clone(),
            item,
            holder: None,
        })?;
        if claim.by != who.session {
            return Err(Refused::NotHeld {
                session: who.session.clone(),
                item,
                holder: Some(claim.by.clone()),
            });
        }
        let it = self.items.get_mut(&item).expect("checked above");
        it.state = State::Done {
            by: who.session.clone(),
            ms: now,
        };
        it.question = None;
        let it = it.clone();
        self.met(who);
        let out = Outcome {
            item: it.clone(),
            events: vec![Event::Done {
                item,
                tag: it.tag.clone(),
                by: who.session.clone(),
            }],
        };
        self.remember(who, request, &out);
        Ok(out)
    }

    /// **Give it up on purpose.** Still an abandonment, still counted — see the module header.
    pub fn abandon(
        &mut self,
        who: &Speaker,
        request: &str,
        item: u64,
        why: &str,
        now: u64,
    ) -> Result<Outcome, Refused> {
        // **Identity before dedupe**: a subagent is refused by name even when it reuses an id.
        who.may_speak()?;
        if let Some(o) = self.replayed(who, request)? {
            return Ok(o);
        }
        let it = self
            .item(item)
            .ok_or(Refused::UnknownItem { item })?
            .clone();
        let claim = it.claim().ok_or(Refused::NotHeld {
            session: who.session.clone(),
            item,
            holder: None,
        })?;
        if claim.by != who.session {
            return Err(Refused::NotHeld {
                session: who.session.clone(),
                item,
                holder: Some(claim.by.clone()),
            });
        }
        self.met(who);
        let out = self.lose(
            item,
            Abandonment {
                by: who.session.clone(),
                claim_ms: claim.claim_ms,
                beat_ms: claim.beat_ms,
                ms: now,
                why: Why::Released {
                    says: if why.trim().is_empty() {
                        "no reason given".to_string()
                    } else {
                        why.trim().to_string()
                    },
                },
            },
            now,
        );
        self.remember(who, request, &out);
        Ok(out)
    }

    /// **"I am still on it"** — a claimant whose beat went stale says so instead of fighting for
    /// the claim, and the arbiter is asked which of the two has it. One open question per item:
    /// asking twice is the same question.
    pub fn dispute(
        &mut self,
        who: &Speaker,
        request: &str,
        item: u64,
        says: &str,
        now: u64,
    ) -> Result<Outcome, Refused> {
        // **Identity before dedupe**: a subagent is refused by name even when it reuses an id.
        who.may_speak()?;
        if let Some(o) = self.replayed(who, request)? {
            return Ok(o);
        }
        let it = self
            .item(item)
            .ok_or(Refused::UnknownItem { item })?
            .clone();
        if it.holder() == Some(who.session.as_str()) {
            return Err(Refused::NotDisputed { item });
        }
        if let Some(q) = it.question {
            let tag = it.tag.clone();
            return Ok(Outcome {
                item: it,
                events: vec![Event::Asked {
                    item,
                    tag,
                    question: q,
                }],
            });
        }
        if !it.live() {
            return Err(Refused::Gone {
                item,
                state: it.state_name().to_string(),
            });
        }
        let q = self.ask_disputed(&it, who, says, now);
        self.items.get_mut(&item).expect("checked above").question = Some(q.id);
        self.met(who);
        let out = Outcome {
            item: self.items[&item].clone(),
            events: vec![Event::Asked {
                item,
                tag: it.tag.clone(),
                question: q.id,
            }],
        };
        self.remember(who, request, &out);
        Ok(out)
    }

    /// **Answer a question.** The decision is applied and the item unfreezes — [`Decision::Retry`]
    /// back to the head with its count kept, [`Decision::Hand`] a claim for the named session now,
    /// [`Decision::Drop`] out of the queue for good.
    pub fn rule(
        &mut self,
        question: u64,
        decision: Decision,
        by: &str,
        now: u64,
    ) -> Result<Outcome, Refused> {
        let q = self
            .questions
            .iter()
            .find(|q| q.id == question)
            .ok_or(Refused::UnknownQuestion { question })?
            .clone();
        if let Some(a) = &q.answer {
            return Err(Refused::AlreadyRuled {
                question,
                by: a.by.clone(),
                ms: a.ms,
            });
        }
        let item = q.facts.item;
        let it = self
            .item(item)
            .ok_or(Refused::UnknownItem { item })?
            .clone();
        // **Validate before touching anything** — a ruling that is refused leaves the item frozen
        // and the question open, which is the only honest outcome.
        if let Decision::Hand { to } = &decision {
            match self.seen.get(to) {
                None => {
                    return Err(Refused::UnknownSession {
                        session: to.clone(),
                    });
                }
                Some(Some(parent)) => {
                    return Err(Refused::Subagent {
                        session: to.clone(),
                        parent: parent.clone(),
                    });
                }
                Some(None) => {}
            }
            if let Some(held) = self.live_claim(&it.tag)
                && held.item_id != item
            {
                return Err(Refused::Busy {
                    tag: it.tag.clone(),
                    item: held.item_id,
                    by: held.by,
                    held_for: duration(now.saturating_sub(held.claim_ms)),
                });
            }
        }
        {
            let it = self.items.get_mut(&item).expect("checked above");
            it.question = None;
            match &decision {
                Decision::Retry => it.state = State::Open,
                Decision::Hand { to } => {
                    it.state = State::Claimed {
                        claim: Claim {
                            by: to.clone(),
                            claim_ms: now,
                            beat_ms: now,
                            waits_on: None,
                        },
                    };
                }
                Decision::Drop { why } => {
                    it.state = State::Dropped {
                        by: by.to_string(),
                        why: why.clone(),
                        ms: now,
                    };
                }
            }
        }
        let ruling = Ruling {
            decision: decision.clone(),
            by: by.to_string(),
            ms: now,
        };
        if let Some(q) = self.questions.iter_mut().find(|q| q.id == question) {
            q.answer = Some(ruling);
        }
        let it = self.items[&item].clone();
        Ok(Outcome {
            item: it.clone(),
            events: vec![Event::Ruled {
                item,
                question,
                decision,
                by: by.to_string(),
            }],
        })
    }

    // ===== The pass =====

    /// **Take back every claim whose beat has stopped.** `now - beat_ms > lease_ms`, exactly the
    /// queue lease's comparison: a beat at the boundary is still alive.
    pub fn sweep(&mut self, now: u64) -> Vec<Event> {
        let stale: Vec<(u64, Abandonment)> = self
            .items
            .values()
            .filter_map(|it| {
                let c = it.claim()?;
                let age = now.saturating_sub(c.beat_ms);
                if age <= self.lease_ms {
                    return None;
                }
                Some((
                    it.id,
                    Abandonment {
                        by: c.by.clone(),
                        claim_ms: c.claim_ms,
                        beat_ms: c.beat_ms,
                        ms: now,
                        why: Why::BeatStale { age_ms: age },
                    },
                ))
            })
            .collect();
        stale
            .into_iter()
            .flat_map(|(item, ab)| self.lose(item, ab, now).events)
            .collect()
    }

    /// **The session is gone** — its daemon exited, was restarted, or the socket that owns it
    /// closed. Every claim it holds is abandoned at once, with the caller's reason on it, rather
    /// than after the lease.
    pub fn note_gone(&mut self, session: &str, says: &str, now: u64) -> Vec<Event> {
        let held: Vec<(u64, Abandonment)> = self
            .items
            .values()
            .filter_map(|it| {
                let c = it.claim()?;
                if c.by != session {
                    return None;
                }
                Some((
                    it.id,
                    Abandonment {
                        by: c.by.clone(),
                        claim_ms: c.claim_ms,
                        beat_ms: c.beat_ms,
                        ms: now,
                        why: Why::SessionGone {
                            says: if says.trim().is_empty() {
                                format!("`{session}` went away")
                            } else {
                                says.trim().to_string()
                            },
                        },
                    },
                ))
            })
            .collect();
        held.into_iter()
            .flat_map(|(item, ab)| self.lose(item, ab, now).events)
            .collect()
    }

    // ===== The queries =====

    pub fn item(&self, id: u64) -> Option<&Item> {
        self.items.get(&id)
    }

    /// **Every item in the room**, oldest first — what the pane draws, including the finished and
    /// dropped ones.
    pub fn all(&self) -> Vec<&Item> {
        self.items.values().collect()
    }

    /// **A tag's queue, in order** — `(posted_ms, id)`, the claimed and the abandoned rows
    /// included, the finished and dropped ones out.
    pub fn queue(&self, tag: &str) -> Vec<&Item> {
        let tag = tag.trim().to_lowercase();
        let mut rows: Vec<&Item> = self
            .items
            .values()
            .filter(|i| i.tag == tag && i.live())
            .collect();
        rows.sort_by_key(|i| (i.posted_ms, i.id));
        rows
    }

    /// **The next item anybody may claim** — the first claimable row, so a raised item at the head
    /// does not block the tag.
    pub fn head(&self, tag: &str) -> Option<&Item> {
        self.queue(tag).into_iter().find(|i| i.claimable())
    }

    /// Every tag the room has heard of, sorted.
    pub fn tags(&self) -> Vec<String> {
        let mut tags: Vec<String> = self.items.values().map(|i| i.tag.clone()).collect();
        tags.sort();
        tags.dedup();
        tags
    }

    /// **What this session holds, and what it is waiting on** — the reminder's facts.
    pub fn holding(&self, session: &str, now: u64) -> Vec<Holding> {
        self.items
            .values()
            .filter_map(|it| {
                let c = it.claim()?;
                if c.by != session {
                    return None;
                }
                Some(Holding {
                    item: it.id,
                    tag: it.tag.clone(),
                    body: it.body.clone(),
                    claim_ms: c.claim_ms,
                    beat_ms: c.beat_ms,
                    held_ms: now.saturating_sub(c.claim_ms),
                    waits_on: c.waits_on.clone(),
                })
            })
            .collect()
    }

    /// **Every reminder the room owes**, one per live claim — what the todo pass turns into rows.
    pub fn reminders(&self, now: u64) -> Vec<Reminder> {
        let mut out: Vec<Reminder> = self
            .items
            .values()
            .filter_map(|it| {
                let c = it.claim()?;
                Some(Reminder {
                    session: c.by.clone(),
                    holding: Holding {
                        item: it.id,
                        tag: it.tag.clone(),
                        body: it.body.clone(),
                        claim_ms: c.claim_ms,
                        beat_ms: c.beat_ms,
                        held_ms: now.saturating_sub(c.claim_ms),
                        waits_on: c.waits_on.clone(),
                    },
                    stale: now.saturating_sub(c.beat_ms) > self.lease_ms,
                })
            })
            .collect();
        out.sort_by(|a, b| (&a.session, a.holding.item).cmp(&(&b.session, b.holding.item)));
        out
    }

    pub fn questions(&self) -> &[Question] {
        &self.questions
    }

    pub fn question(&self, id: u64) -> Option<&Question> {
        self.questions.iter().find(|q| q.id == id)
    }

    /// **The open questions**, oldest first — what the tie-breaker is asked, in the order it
    /// should answer them.
    pub fn open_questions(&self) -> Vec<&Question> {
        self.questions
            .iter()
            .filter(|q| q.answer.is_none())
            .collect()
    }

    // ===== The internals =====

    /// The live claim under a tag, if there is one.
    fn live_claim(&self, tag: &str) -> Option<Live> {
        self.items.values().find_map(|it| {
            let c = it.claim()?;
            if it.tag != tag {
                return None;
            }
            Some(Live {
                item: it.clone(),
                item_id: it.id,
                by: c.by.clone(),
                claim_ms: c.claim_ms,
            })
        })
    }

    /// Take the claim on `item`, which the caller has already checked is claimable and at the
    /// head of a free tag.
    fn take(&mut self, who: &Speaker, item: u64, now: u64) -> Result<Outcome, Refused> {
        let it = self.items.get_mut(&item).expect("checked by the caller");
        it.state = State::Claimed {
            claim: Claim {
                by: who.session.clone(),
                claim_ms: now,
                beat_ms: now,
                waits_on: None,
            },
        };
        it.question = None;
        let it = it.clone();
        self.met(who);
        Ok(Outcome {
            item: it.clone(),
            events: vec![Event::Claimed {
                item,
                tag: it.tag.clone(),
                by: who.session.clone(),
            }],
        })
    }

    /// **Lose a claim** — the one path every abandonment goes through, so the count, the event and
    /// the raise cannot drift apart.
    fn lose(&mut self, item: u64, ab: Abandonment, now: u64) -> Outcome {
        let it = self.items.get_mut(&item).expect("a claim was found on it");
        it.abandonments.push(ab.clone());
        it.state = State::Abandoned { last: ab.clone() };
        it.question = None;
        let count = it.abandonments();
        let tag = it.tag.clone();
        let mut events = vec![Event::Abandoned {
            item,
            tag: tag.clone(),
            by: ab.by.clone(),
            why: ab.why.clone(),
            count,
            back_in_the_queue: count < ABANDON_THRESHOLD,
        }];
        let it = it.clone();
        if count >= ABANDON_THRESHOLD {
            let q = self.raise(&it, now);
            self.items.get_mut(&item).expect("just read").question = Some(q.id);
            events.push(Event::Raised {
                item,
                tag,
                question: q.id,
                count,
            });
        }
        // **Read it back after the raise**, so the outcome carries the question id too: a caller
        // that holds an item with `question: None` would draw a frozen item as claimable.
        Outcome {
            item: self.items[&item].clone(),
            events,
        }
    }

    /// Build the question an item at the threshold asks.
    fn raise(&mut self, it: &Item, now: u64) -> Question {
        self.next_question += 1;
        let id = self.next_question;
        let facts = self.facts(it);
        let lost = facts
            .abandonments
            .iter()
            .map(|a| format!("`{}` at {} ({})", a.by, a.ms, a.why))
            .collect::<Vec<_>>()
            .join("; ");
        let behind = if facts.waiting.is_empty() {
            "nothing is queued behind it".to_string()
        } else {
            format!(
                "{} item(s) are queued behind it (oldest `{}`'s #{} since {})",
                facts.waiting.len(),
                facts.waiting[0].by,
                facts.waiting[0].item,
                facts.waiting[0].since_ms
            )
        };
        let text = format!(
            "Item #{} under `{tag}` (`{body}`, posted by `{by}` at {posted}) has abandoned {n} \
             claimants: {lost}. The room has stopped offering it, and {behind}. Decide: Retry \
             (offer it again, count kept), Hand{{to}} (give it to a named session), or \
             Drop{{why}} (take it out of the queue).",
            it.id,
            tag = facts.tag,
            body = facts.body,
            by = facts.posted_by,
            posted = facts.posted_ms,
            n = facts.abandonments.len(),
            lost = lost,
            behind = behind,
        );
        let q = Question {
            id,
            asked_ms: now,
            about: About::ItemRaised {
                item: it.id,
                tag: it.tag.clone(),
            },
            text,
            facts,
            answer: None,
        };
        self.questions.push(q.clone());
        q
    }

    /// Build the question a dispute asks.
    fn ask_disputed(&mut self, it: &Item, who: &Speaker, says: &str, now: u64) -> Question {
        self.next_question += 1;
        let id = self.next_question;
        let facts = self.facts(it);
        let took = match it.last_abandonment() {
            Some(a) => format!("the room took it back at {} ({})", a.ms, a.why),
            None => "the room has no claim on it".to_string(),
        };
        let held = match (&facts.claimed_by, facts.claim_ms) {
            (Some(by), Some(ms)) => format!("`{by}` has held it since {ms}"),
            _ => "nobody holds it now".to_string(),
        };
        let text = format!(
            "`{who}` says it is still working on item #{} under `{tag}` (`{body}`) and asks to \
             keep it: \"{says}\". {took}, and {held}. Decide: Hand{{to}} (say which of them has \
             it) or Retry (drop any claim and put it back at the head of `{tag}`).",
            it.id,
            tag = facts.tag,
            body = facts.body,
            who = who.session,
            says = says.trim(),
            took = took,
            held = held,
        );
        let q = Question {
            id,
            asked_ms: now,
            about: About::ClaimDisputed {
                item: it.id,
                tag: it.tag.clone(),
                by: who.session.clone(),
            },
            text,
            facts,
            answer: None,
        };
        self.questions.push(q.clone());
        q
    }

    /// The facts a question carries: the item, its claim, its history, and who is behind it.
    fn facts(&self, it: &Item) -> Facts {
        let claim = it.claim();
        Facts {
            item: it.id,
            tag: it.tag.clone(),
            body: it.body.clone(),
            posted_by: it.posted_by.clone(),
            posted_ms: it.posted_ms,
            claimed_by: claim.map(|c| c.by.clone()),
            claim_ms: claim.map(|c| c.claim_ms),
            beat_ms: claim.map(|c| c.beat_ms),
            abandonments: it.abandonments.clone(),
            waiting: self
                .queue(&it.tag)
                .into_iter()
                .filter(|other| other.id != it.id)
                .map(|other| Waiting {
                    item: other.id,
                    by: other.posted_by.clone(),
                    since_ms: other.posted_ms,
                })
                .collect(),
        }
    }

    fn met(&mut self, who: &Speaker) {
        self.seen
            .entry(who.session.clone())
            .or_insert_with(|| who.parent.clone());
    }

    fn replayed(&self, who: &Speaker, request: &str) -> Result<Option<Outcome>, Refused> {
        if request.trim().is_empty() {
            return Err(Refused::EmptyRequest);
        }
        Ok(self
            .replay
            .get(&(who.session.clone(), request.to_string()))
            .map(|o| Outcome {
                item: o.item.clone(),
                // **The bell rang the first time.** A repeat is the client catching up on the
                // REPLY, not a second thing happening.
                events: Vec::new(),
            }))
    }

    fn remember(&mut self, who: &Speaker, request: &str, out: &Outcome) {
        let key = (who.session.clone(), request.to_string());
        if self.replay.insert(key.clone(), out.clone()).is_none() {
            self.replay_order.push_back(key);
            while self.replay_order.len() > REPLAY_WINDOW {
                if let Some(old) = self.replay_order.pop_front() {
                    self.replay.remove(&old);
                }
            }
        }
    }
}

/// A live claim, copied out of the item so the caller can mutate the room.
struct Live {
    item: Item,
    item_id: u64,
    by: String,
    claim_ms: u64,
}

/// **A tag as the room spells it** — trimmed and lowercased, and empty is refused by name.
///
/// Public because a tag is now two things: a queue's name *and* the suffix of the agent that owns
/// that queue ([`crate::lease::tag_session`]). A second normalisation beside this one would give
/// `RAM` and `ram` one queue and two agents — the note
/// `.letibot/notes/the-activity-and-the-delivery-had-two-copies-of-one-rule.md` is that mistake in
/// another module.
pub fn normalise_tag(tag: &str) -> Result<String, Refused> {
    let tag = tag.trim().to_lowercase();
    if tag.is_empty() {
        return Err(Refused::EmptyTag);
    }
    Ok(tag)
}

impl Speaker {
    /// **The rule, at the door.** A subagent is refused by name; everything else passes.
    pub fn may_speak(&self) -> Result<(), Refused> {
        match &self.parent {
            None => Ok(()),
            Some(parent) => Err(Refused::Subagent {
                session: self.session.clone(),
                parent: parent.clone(),
            }),
        }
    }
}
