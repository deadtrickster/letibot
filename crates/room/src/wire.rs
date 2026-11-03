//! The wire: the frames a coordinator's socket takes and sends. [`Room::answer`] is the dispatch with
//! the socket taken out, so a protocol can be tested before anybody writes a listener.
//!
//! One connection per top-level session, because a push has an address and a close is a fact: the
//! coordinator calls [`Room::note_gone`] at once instead of waiting out the lease.
//!
//! Every mutating frame carries a client-minted `id`, because reconnection is the normal case: a repeat
//! is answered from the replay window and carries no events; a version mismatch is [`Response::Bye`].

use serde::{Deserialize, Serialize};

use crate::item::{Item, Speaker};
use crate::room::{
    ABANDON_THRESHOLD, DEFAULT_BEAT_EVERY_MS, Decision, Holding, Question, Refused, Reminder, Room,
};

/// The room's protocol version, and the room's alone - see the module header. The number moves at
/// RELEASE POINTS: a frame added mid-series rides the next number with whatever else lands before it,
/// because the room has no users outside this tree to be compatible with.
pub const ROOM_PROTOCOL_VERSION: u16 = 6;

/// The sentence a protocol mismatch is refused with, or `None` when the two agree. One function because
/// two doors refuse it - a [`crate::Server`]'s and [`crate::Room::answer_traced`]'s - and two sentences
/// about one skew would drift. It says which side is older, because a reader has to work that out.
pub fn protocol_refusal(theirs: u16) -> Option<String> {
    if theirs == ROOM_PROTOCOL_VERSION {
        return None;
    }
    let older = if theirs < ROOM_PROTOCOL_VERSION {
        "the client"
    } else {
        "this room"
    };
    Some(format!(
        "this room speaks protocol {ROOM_PROTOCOL_VERSION} and the client speaks {theirs} — {older} \
         is the older of the two, so a frame one of them does not know would be obeyed wrongly and \
         nothing was done: upgrade {older}"
    ))
}

/// What a client sends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// The first frame on every connection: who is speaking, in which protocol, and which room the client
    /// believes it reached. `room` is `<host key>/<project key>`, because two boxes can serve one repository
    /// path and two projects can live on one box; empty is *I do not say*.
    Hello {
        protocol: u16,
        speaker: Speaker,
        #[serde(default)]
        room: String,
        /// Who is connecting: a session (the default), the operator, or a named external.
        #[serde(default)]
        seat: crate::chat::Seat,
    },
    /// The rooms this connection may see, with unread counts.
    Rooms,
    /// Say something. `room` empty means the session's project room, or the host's if it has none.
    Say {
        id: String,
        #[serde(default)]
        room: String,
        text: String,
        #[serde(default)]
        to: Option<String>,
        #[serde(default)]
        thread: Option<u64>,
        #[serde(default)]
        refs: Vec<crate::chat::Ref>,
    },
    /// A direct message: read by its author and `to` only.
    Dm {
        id: String,
        to: String,
        text: String,
        #[serde(default)]
        thread: Option<u64>,
    },
    /// The harness reports a fact. Only a session connection (a daemon) may send it, into the host
    /// room or that session's project room, authored by `source` with kind `system`.
    Notify {
        id: String,
        #[serde(default)]
        room: String,
        source: String,
        text: String,
        #[serde(default)]
        refs: Vec<crate::chat::Ref>,
    },
    /// A room's messages after `since`. Moves no cursor. `room` `dm` reads this principal's DMs.
    Read {
        room: String,
        #[serde(default)]
        since: u64,
        #[serde(default)]
        last: Option<u32>,
        #[serde(default)]
        thread: Option<u64>,
    },
    /// Who is in a room.
    Roster { room: String },
    /// What this principal missed since its cursor, through an attention filter. Moves nothing.
    Inbox {
        #[serde(default)]
        attention: crate::chat::Attention,
        #[serde(default)]
        limit: Option<u32>,
    },
    /// Move this principal's inbox cursor to `seq`.
    Ack { seq: u64 },
    /// Post a need under a tag. `body` is the need, in the poster's words.
    Post {
        id: String,
        tag: String,
        body: String,
    },
    /// Claim the head of a tag.
    Claim { id: String, tag: String },
    /// Claim one item - the operator's manual control, or a client that means *that* row. It must
    /// still be the head.
    ClaimItem { id: String, item: u64 },
    /// Say you are still working on it.
    Beat { id: String, item: u64 },
    /// Say what you are waiting on; empty text clears it.
    Waiting { id: String, item: u64, text: String },
    /// Say what you are DOING with it - the acknowledgement, which is not a wait. Empty text clears
    /// it. See [`crate::Claim::said`].
    Acknowledge { id: String, item: u64, text: String },
    /// The work is done.
    Done { id: String, item: u64 },
    /// Give it up on purpose.
    Abandon { id: String, item: u64, why: String },
    /// *I am still on it* - ask the arbiter, rather than fight for the claim.
    Dispute { id: String, item: u64, says: String },
    /// Answer a question the room asked.
    Rule {
        id: String,
        question: u64,
        decision: Decision,
        /// Which room the question is in, for a client that hears more than one: the arbiter does,
        /// and two rooms number their questions from 1. Naming none means *the room this
        /// connection is in*, which is every ordinary session's answer.
        #[serde(default)]
        project: Option<String>,
    },
    /// The project's tree, as the daemon holding it sees it - who is in this room. The room cannot work
    /// this out: it holds no store, and a subagent never opens a connection, so the connections are not the
    /// membership either. A replacement and not a merge, because a merge would keep a session closed since.
    Members { rows: Vec<crate::tree::Member> },
    /// A tag's queue, in order - the read that makes polling unnecessary after a reconnect.
    Queue { tag: String },
    /// What a session holds and what it is waiting on. `session: None` means *me*.
    Holding { session: Option<String> },
    /// Every reminder the room owes.
    Reminders,
    /// The open questions, oldest first - what the tie-breaker is asked.
    Questions,
    /// The whole room in one read - what a PANE is drawn from. [`Request::Queue`] answers one tag, and a
    /// pane that draws the queues per tag would have to ask for a tag whose name it does not know. A read,
    /// adding no state: the pane it serves is a view of the same in-memory room.
    Room,
}

impl Request {
    /// The correlation id, when the frame has one. A read has none, and neither has `hello`: a read
    /// is not a verb and cannot be acted on twice.
    pub fn request_id(&self) -> Option<&str> {
        match self {
            Request::Hello { .. }
            | Request::Rooms
            | Request::Read { .. }
            | Request::Roster { .. }
            | Request::Inbox { .. }
            | Request::Ack { .. }
            | Request::Queue { .. }
            | Request::Holding { .. }
            | Request::Reminders
            | Request::Questions
            | Request::Members { .. }
            | Request::Room => None,
            Request::Say { id, .. }
            | Request::Dm { id, .. }
            | Request::Notify { id, .. }
            | Request::Post { id, .. }
            | Request::Claim { id, .. }
            | Request::ClaimItem { id, .. }
            | Request::Beat { id, .. }
            | Request::Waiting { id, .. }
            | Request::Acknowledge { id, .. }
            | Request::Done { id, .. }
            | Request::Abandon { id, .. }
            | Request::Dispute { id, .. }
            | Request::Rule { id, .. } => Some(id),
        }
    }
}

/// What the server sends: a reply, or a push the client did not ask for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Welcome {
        protocol: u16,
        /// The lease the room abandons a silent claim after, so a client can beat at a third of it
        /// rather than guess.
        lease_ms: u64,
        beat_every_ms: u64,
        threshold: u32,
        /// `Server::identity()`, verbatim, so the client is TOLD what it connected to. It is the sentence
        /// as of this moment: a reader that wants the age now asks [`Request::Room`], whose own `identity`
        /// is the same function taken at the read.
        #[serde(default)]
        identity: String,
        /// How many events this session was addressed by while it had no connection. The router counts an
        /// event once per session it would have reached, so the number is *how many bells rang where
        /// nobody was*, not how many events happened.
        #[serde(default)]
        missed: u64,
        /// When the first of them was routed, so the sentence a client composes can say how long the
        /// gap was. `0` when nothing was missed.
        #[serde(default)]
        missed_since_ms: u64,
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
    Acknowledged {
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
    /// The room, as a pane reads it - the answer to [`Request::Room`]. The first three fields are the
    /// SERVER's, not the room's: the state machine cannot say what a process is, who arbitrates it or who
    /// is connected to it, and [`crate::Server`]'s dispatch fills them in one place.
    Room {
        /// `Server::identity()`: *started T ago and holding N item(s)*, and the sentence saying the
        /// room keeps no store. This is the field that makes *restarted clean* and *restarted empty*
        /// two readings, because an in-memory room is empty at every start.
        identity: String,
        /// The session that arbitrates this room, when one is named.
        arbiter: Option<String>,
        /// Every session connected to this room now, sorted. The socket is the DAEMON's liveness, so
        /// this is who is up rather than who is beating - and an `arbiter` absent from it is a room
        /// whose questions wait.
        connected: Vec<String>,
        /// Every item, oldest first, finished and dropped included: a pane shows the history, and a
        /// row that vanished would be a row a person cannot account for.
        items: Vec<Item>,
        /// The open questions, oldest first.
        questions: Vec<Question>,
        /// The tags that have an agent, in tag order: who it is, how many times it had to be
        /// reseated, and when the one now seated began. A tag absent from this list has no agent,
        /// which is a fact the pane says rather than a column it leaves blank.
        #[serde(default)]
        agents: Vec<crate::agent::TagAgent>,
        /// The project's tree: every session in this room, in the order the tree draws them - the project's
        /// own sessions and their subagents, then the `system` agents that serve them. Filled from the last
        /// [`Request::Members`] its daemon sent.
        #[serde(default)]
        members: Vec<crate::tree::Member>,
    },
    /// The room now holds this tree - the answer to [`Request::Members`].
    Members {
        count: u64,
    },
    Rooms {
        rows: Vec<crate::chat::RoomInfo>,
    },
    Said {
        id: String,
        message: crate::chat::Message,
    },
    History(crate::chat::History),
    Roster {
        room: String,
        rows: Vec<crate::chat::Presence>,
    },
    Inbox {
        rows: Vec<crate::chat::Message>,
        /// Messages the attention filter left out.
        skipped: u64,
        cursor: u64,
        head: u64,
    },
    Acked {
        cursor: u64,
    },
    /// A chat message pushed to a connection that may see it. Never sent to its author.
    /// `names_you` says the message names this reader (addressed or `@`-mentioned).
    Chat {
        message: crate::chat::Message,
        names_you: bool,
    },
    /// The bell - an unsolicited event. A client that only ever answers a prompt cannot be woken by
    /// this.
    Event {
        event: crate::room::Event,
    },
    /// The connection ends, with the reason in the sentence - a protocol mismatch, or the
    /// coordinator stopping.
    Bye {
        why: String,
    },
}

/// One frame's whole answer: the reply, and the events the verb produced. The reply is what the asking
/// connection reads, the events are what the SERVER rings other connections with, and they are one value
/// because they come out of one act.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub response: Response,
    pub events: Vec<crate::room::Event>,
}

impl Answer {
    /// A reply with nothing to ring about - a read, a `Hello`, a `Bye`.
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
    /// Answer one frame - the coordinator's dispatch with the socket taken out. A push
    /// ([`Response::Event`]) is not produced here: the sweep, the close and every [`crate::Outcome`] carry
    /// the events, and the server decides which connections they go to.
    pub fn answer(&mut self, who: &Speaker, request: &Request, now: u64) -> Response {
        self.answer_traced(who, request, now).response
    }

    /// [`Room::answer`], with the events the frame produced.
    pub fn answer_traced(&mut self, who: &Speaker, request: &Request, now: u64) -> Answer {
        match request {
            Request::Hello {
                protocol,
                speaker,
                room: _,
                seat: _,
            } => {
                if let Some(why) = protocol_refusal(*protocol) {
                    return Answer::quiet(Response::Bye { why });
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
                    // The identity is the SERVER's, and this dispatch has none - the same reason
                    // `Response::Room`'s server fields are filled by `Server::answer_frame`. A
                    // `Room` driven directly by a test gets the empty string; a client never does.
                    identity: String::new(),
                    // A `Room` has no connections, so it has never failed to deliver one. The count
                    // is the SERVER's for the same reason the identity is: it is a fact about a
                    // socket, and this dispatch has no socket.
                    missed: 0,
                    missed_since_ms: 0,
                })
            }
            Request::Post { id, tag, body } => {
                Answer::verb(id, self.post(who, id, tag, body, now), |item| {
                    Response::Posted {
                        id: id.clone(),
                        item,
                    }
                })
            }
            Request::Claim { id, tag } => Answer::verb(id, self.claim(who, id, tag, now), |item| {
                Response::Claimed {
                    id: id.clone(),
                    item,
                }
            }),
            Request::ClaimItem { id, item } => {
                Answer::verb(id, self.claim_item(who, id, *item, now), |item| {
                    Response::Claimed {
                        id: id.clone(),
                        item,
                    }
                })
            }
            Request::Beat { id, item } => {
                Answer::verb(id, self.beat(who, id, *item, now), |item| {
                    Response::Beaten {
                        id: id.clone(),
                        item,
                    }
                })
            }
            Request::Waiting { id, item, text } => {
                Answer::verb(id, self.waiting_on(who, id, *item, text, now), |item| {
                    Response::Waited {
                        id: id.clone(),
                        item,
                    }
                })
            }
            Request::Acknowledge { id, item, text } => {
                Answer::verb(id, self.acknowledge(who, id, *item, text, now), |item| {
                    Response::Acknowledged {
                        id: id.clone(),
                        item,
                    }
                })
            }
            Request::Done { id, item } => {
                Answer::verb(id, self.done(who, id, *item, now), |item| {
                    Response::Finished {
                        id: id.clone(),
                        item,
                    }
                })
            }
            Request::Abandon { id, item, why } => {
                Answer::verb(id, self.abandon(who, id, *item, why, now), |item| {
                    Response::Abandoned {
                        id: id.clone(),
                        item,
                    }
                })
            }
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
                // The room ignores it, and that is the point: which room a question is in is the
                // SERVER's fact, and a room that could be asked about another room's question would
                // be two rooms.
                project: _,
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
            Request::Rooms
            | Request::Say { .. }
            | Request::Dm { .. }
            | Request::Notify { .. }
            | Request::Read { .. }
            | Request::Roster { .. }
            | Request::Inbox { .. }
            | Request::Ack { .. } => Answer::quiet(Response::Refused {
                id: request.request_id().map(str::to_string),
                why: Refused::ChatIsTheServers,
            }),
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
            // The tree, replaced. No id and no event: a membership is a set, a repeat is the same
            // set, and a bell for *somebody else joined the project* would be a turn for every
            // session on every change.
            Request::Members { rows } => {
                self.set_members(rows.clone());
                Answer::quiet(Response::Members {
                    count: self.members().len() as u64,
                })
            }
            Request::Questions => Answer::quiet(Response::Questions {
                rows: self.open_questions().into_iter().cloned().collect(),
            }),
            // The pane's read, and the only frame with fields the room cannot fill: what this coordinator
            // IS, who arbitrates it and who is connected to it are facts about a process, and this type has
            // no clock and no socket. It answers the half it owns and `Server::answer_frame` fills the rest.
            Request::Room => Answer::quiet(Response::Room {
                identity: String::new(),
                arbiter: None,
                connected: Vec::new(),
                items: self.all().into_iter().cloned().collect(),
                questions: self.questions().to_vec(),
                agents: Vec::new(),
                members: self.members().to_vec(),
            }),
        }
    }
}
