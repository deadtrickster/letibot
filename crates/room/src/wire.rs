//! **The wire: the frames a coordinator's socket would take and send.**
//!
//! This is the *proposal*, made concrete enough to be compiled and round-tripped: the coordinator
//! daemon (next slice) is a loop over [`Request`] and [`Response`] around a [`Room`], and
//! [`Room::answer`] is that loop's dispatch with the socket taken out — so the protocol can be
//! tested before anybody writes a listener, and a frame that cannot be answered here is a frame
//! that cannot be answered there.
//!
//! # One connection per top-level session
//!
//! [`Request::Hello`] carries the [`Speaker`] — the session and the parent it speaks through, read
//! by the daemon from `session.parent_session_id`. A subagent is refused **at the door**, and the
//! connection is per session rather than per daemon for two reasons that are worth the sockets:
//!
//! * **A push has an address.** The bell ([`Response::Event`]) goes to the connections that own the
//!   sessions it is about; a multiplexed connection would have to route by hand.
//! * **The close is a fact.** A session whose daemon died, restarted or was stopped drops its
//!   socket, and the coordinator calls [`Room::note_gone`] *at once* instead of waiting out the
//!   lease. The socket is the liveness of the daemon; the beat is the liveness of the work — and
//!   they are different facts, which is why both exist. A session that is merely parked (its turn
//!   ended, its daemon fine) keeps its connection and loses its claim to the beat.
//!
//! # What is idempotent, and why it has to be
//!
//! A socket a session can reconnect to makes reconnection the normal case, not an error path — so
//! every mutating frame carries a client-minted `id`, and:
//!
//! | frame | a repeat does |
//! |---|---|
//! | `post` | answers the item it made the first time; **no second item** |
//! | `claim` (same tag) | answers the claim the same session already holds; `claim_ms` does not move |
//! | `claim_item` | the same, for one item |
//! | `beat` | stamps the same claim again — harmless by construction, and answered from the replay window anyway |
//! | `waiting` | sets the same text; the `since_ms` moves only if the text is set again later |
//! | `done`, `abandon` | answer the finished/abandoned item; the second is refused by name ([`Refused::NotHeld`]) if the replay window has forgotten the first, which is the honest answer |
//! | `dispute` | an item has at most one open question, so asking twice is the same question |
//!
//! The replay window is [`crate::REPLAY_WINDOW`] replies per room. **A client that has been away
//! for longer than that must re-read** ([`Request::Queue`], [`Request::Holding`],
//! [`Request::Reminders`]) rather than replay: every reply carries the item as it stands, so a
//! re-read is cheap and cannot be wrong, and a replay window is not a durable log.
//!
//! **A reply to a repeat carries no events.** The bell rang the first time; a client catching up on
//! a reply is not a second thing happening.
//!
//! # Versioning
//!
//! [`ROOM_PROTOCOL_VERSION`] is the room's own number, not the session protocol's
//! (`letibot_sessionlog::PROTOCOL_VERSION`): the coordinator is a different socket with a
//! different lifetime, and pairing a head with a daemon must not be entangled with pairing a
//! session with a room. A mismatch is [`Response::Bye`] with both numbers in the sentence — the
//! same clean refusal the daemon's `Hello` makes.

use serde::{Deserialize, Serialize};

use crate::item::{Item, Speaker};
use crate::room::{
    ABANDON_THRESHOLD, DEFAULT_BEAT_EVERY_MS, Decision, Holding, Question, Refused, Reminder, Room,
};

/// **The room's protocol version**, and the room's alone — see the module header.
pub const ROOM_PROTOCOL_VERSION: u16 = 1;

/// **What a client sends.**
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// The first frame on every connection: who is speaking, and in which protocol.
    Hello { protocol: u16, speaker: Speaker },
    /// Post a need under a tag. `body` is the need, in the poster's words.
    Post {
        id: String,
        tag: String,
        body: String,
    },
    /// Claim the head of a tag.
    Claim { id: String, tag: String },
    /// Claim one item — the operator's manual control, or a client that means *that* row. It must
    /// still be the head.
    ClaimItem { id: String, item: u64 },
    /// Say you are still working on it.
    Beat { id: String, item: u64 },
    /// Say what you are waiting on; empty text clears it.
    Waiting { id: String, item: u64, text: String },
    /// The work is done.
    Done { id: String, item: u64 },
    /// Give it up on purpose.
    Abandon { id: String, item: u64, why: String },
    /// *I am still on it* — ask the arbiter, rather than fight for the claim.
    Dispute { id: String, item: u64, says: String },
    /// Answer a question the room asked.
    Rule {
        id: String,
        question: u64,
        decision: Decision,
    },
    /// **A tag's queue, in order** — the read that makes polling unnecessary for a client that
    /// has just reconnected.
    Queue { tag: String },
    /// What a session holds and what it is waiting on. `session: None` means *me*.
    Holding { session: Option<String> },
    /// Every reminder the room owes.
    Reminders,
    /// The open questions, oldest first — what the tie-breaker is asked.
    Questions,
}

impl Request {
    /// **The correlation id**, when the frame has one. A `queue`/`holding`/`reminders` read has
    /// none, and neither has `hello`: a read is not a verb and cannot be acted on twice.
    pub fn request_id(&self) -> Option<&str> {
        match self {
            Request::Hello { .. }
            | Request::Queue { .. }
            | Request::Holding { .. }
            | Request::Reminders
            | Request::Questions => None,
            Request::Post { id, .. }
            | Request::Claim { id, .. }
            | Request::ClaimItem { id, .. }
            | Request::Beat { id, .. }
            | Request::Waiting { id, .. }
            | Request::Done { id, .. }
            | Request::Abandon { id, .. }
            | Request::Dispute { id, .. }
            | Request::Rule { id, .. } => Some(id),
        }
    }
}

/// **What the server sends** — a reply, or a push the client did not ask for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Welcome {
        protocol: u16,
        /// The lease the room abandons a silent claim after — so a client can beat at a third of
        /// it rather than guess.
        lease_ms: u64,
        beat_every_ms: u64,
        threshold: u32,
    },
    Refused {
        /// The frame's id, when it had one.
        id: Option<String>,
        why: Refused,
    },
    Posted {
        id: String,
        item: Item,
    },
    Claimed {
        id: String,
        item: Item,
    },
    Beaten {
        id: String,
        item: Item,
    },
    Waited {
        id: String,
        item: Item,
    },
    Finished {
        id: String,
        item: Item,
    },
    Abandoned {
        id: String,
        item: Item,
    },
    Queue {
        tag: String,
        rows: Vec<Item>,
    },
    Holding {
        session: String,
        rows: Vec<Holding>,
    },
    Reminders {
        rows: Vec<Reminder>,
    },
    /// The question a `dispute` produced (or the one already open about that item).
    Question {
        id: String,
        question: Question,
    },
    Ruled {
        id: String,
        question: Question,
    },
    Questions {
        rows: Vec<Question>,
    },
    /// **The bell** — an unsolicited event. A client that only ever answers a prompt cannot be
    /// woken by this; see `docs/room.md` § *The bell, and the client that cannot hear it*.
    Event {
        event: crate::room::Event,
    },
    /// The connection ends, with the reason in the sentence — a protocol mismatch, or the
    /// coordinator stopping.
    Bye {
        why: String,
    },
}

/// **One frame's whole answer** — the reply, and the events the verb produced.
///
/// The reply is what the asking connection reads; the events are what the SERVER rings other
/// connections with. They are one value because they come out of one act: a `claim` that replied
/// `Claimed` and forgot to ring the sessions queued behind it would be a room that only answers
/// whoever asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub response: Response,
    pub events: Vec<crate::room::Event>,
}

impl Answer {
    /// A reply with nothing to ring about — a read, a `Hello`, a `Bye`.
    fn quiet(response: Response) -> Answer {
        Answer {
            response,
            events: Vec::new(),
        }
    }

    /// A verb's outcome, as its reply and its events.
    fn verb(
        id: &str,
        out: Result<crate::Outcome, Refused>,
        ok: impl FnOnce(Item) -> Response,
    ) -> Answer {
        match out {
            Ok(o) => Answer {
                response: ok(o.item),
                events: o.events,
            },
            Err(why) => Answer {
                response: Response::Refused {
                    id: Some(id.to_string()),
                    why,
                },
                events: Vec::new(),
            },
        }
    }
}

impl Room {
    /// **Answer one frame** — the coordinator's dispatch with the socket taken out.
    ///
    /// `who` is the connection's [`Speaker`] (from its `Hello`); `now` is the coordinator's clock.
    /// A push ([`Response::Event`]) is *not* produced here: [`Room::sweep`], [`Room::note_gone`]
    /// and every [`crate::Outcome`] carry the events, and the server decides which connections
    /// they go to. That split is deliberate — routing a bell is a property of the connections, and
    /// this crate has none.
    ///
    /// [`Room::answer_traced`] is the same dispatch with those events handed back, and it is what a
    /// server calls: a reply without its events is a bell nobody rang.
    pub fn answer(&mut self, who: &Speaker, request: &Request, now: u64) -> Response {
        self.answer_traced(who, request, now).response
    }

    /// **[`Room::answer`], with the events the frame produced.**
    pub fn answer_traced(&mut self, who: &Speaker, request: &Request, now: u64) -> Answer {
        match request {
            Request::Hello { protocol, speaker } => {
                if *protocol != ROOM_PROTOCOL_VERSION {
                    return Answer::quiet(Response::Bye {
                        why: format!(
                            "this room speaks protocol {ROOM_PROTOCOL_VERSION} and the client \
                             speaks {protocol} — upgrade one of them"
                        ),
                    });
                }
                if let Err(why) = speaker.may_speak() {
                    return Answer::quiet(Response::Refused { id: None, why });
                }
                // The room now knows this session, so a ruling may hand it an item even if it
                // has not spoken yet.
                let _ = self.vouch(speaker);
                Answer::quiet(Response::Welcome {
                    protocol: ROOM_PROTOCOL_VERSION,
                    lease_ms: self.lease_ms(),
                    beat_every_ms: DEFAULT_BEAT_EVERY_MS,
                    threshold: ABANDON_THRESHOLD,
                })
            }
            Request::Post { id, tag, body } => Answer::verb(
                id,
                self.post(who, id, tag, body, now),
                |item| Response::Posted { id: id.clone(), item },
            ),
            Request::Claim { id, tag } => Answer::verb(
                id,
                self.claim(who, id, tag, now),
                |item| Response::Claimed { id: id.clone(), item },
            ),
            Request::ClaimItem { id, item } => Answer::verb(
                id,
                self.claim_item(who, id, *item, now),
                |item| Response::Claimed { id: id.clone(), item },
            ),
            Request::Beat { id, item } => Answer::verb(
                id,
                self.beat(who, id, *item, now),
                |item| Response::Beaten { id: id.clone(), item },
            ),
            Request::Waiting { id, item, text } => Answer::verb(
                id,
                self.waiting_on(who, id, *item, text, now),
                |item| Response::Waited { id: id.clone(), item },
            ),
            Request::Done { id, item } => Answer::verb(
                id,
                self.done(who, id, *item, now),
                |item| Response::Finished { id: id.clone(), item },
            ),
            Request::Abandon { id, item, why } => Answer::verb(
                id,
                self.abandon(who, id, *item, why, now),
                |item| Response::Abandoned { id: id.clone(), item },
            ),
            Request::Dispute { id, item, says } => match self.dispute(who, id, *item, says, now) {
                Ok(o) => {
                    let question = o
                        .item
                        .question
                        .and_then(|q| self.question(q).cloned())
                        .expect("a dispute leaves an open question on the item");
                    Answer {
                        response: Response::Question {
                            id: id.clone(),
                            question,
                        },
                        events: o.events,
                    }
                }
                Err(why) => Answer::quiet(Response::Refused {
                    id: Some(id.clone()),
                    why,
                }),
            },
            Request::Rule {
                id,
                question,
                decision,
            } => match self.rule(*question, decision.clone(), &who.session, now) {
                Ok(o) => Answer {
                    response: Response::Ruled {
                        id: id.clone(),
                        question: self.question(*question).cloned().expect("just ruled on it"),
                    },
                    events: o.events,
                },
                Err(why) => Answer::quiet(Response::Refused {
                    id: Some(id.clone()),
                    why,
                }),
            },
            Request::Queue { tag } => Answer::quiet(Response::Queue {
                tag: tag.trim().to_lowercase(),
                rows: self.queue(tag).into_iter().cloned().collect(),
            }),
            Request::Holding { session } => {
                let session = session.clone().unwrap_or_else(|| who.session.clone());
                Answer::quiet(Response::Holding {
                    rows: self.holding(&session, now),
                    session,
                })
            }
            Request::Reminders => Answer::quiet(Response::Reminders {
                rows: self.reminders(now),
            }),
            Request::Questions => Answer::quiet(Response::Questions {
                rows: self.open_questions().into_iter().cloned().collect(),
            }),
        }
    }
}
