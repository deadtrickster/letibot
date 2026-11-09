//! The room's state machine: one queue per tag, a claim that is a lease with a beat, and a raise
//! when a tag keeps being abandoned. It holds no clock: every verb takes the time it is asked
//! at, so a test can drive a ninety-second TTL in microseconds.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::item::{Abandonment, Claim, Item, Speaker, State, Wait, Why, duration};
use crate::store::Store;

/// Ninety seconds, the merge queue's own number: eighteen missed beats at a five second beat, so a
/// daemon held up by a slow write is not mistaken for a dead one, and a dead one is replaced within
/// a minute and a half.
pub const DEFAULT_LEASE_MS: u64 = 90_000;

/// How often a holder is expected to say it is alive - the merge queue's `HEARTBEAT_EVERY`.
pub const DEFAULT_BEAT_EVERY_MS: u64 = 5_000;

/// How many claimants an item may lose before it raises instead of cycling.
pub const ABANDON_THRESHOLD: u32 = 3;

/// How many verb replies are remembered for a reconnecting client: a duplicate `post` or a repeated
/// `claim` after a reconnect is answered from here instead of acted on twice. A client away longer
/// than this must re-read ([`Room::queue`], [`Room::holding`]), which is cheap and always right.
pub const REPLAY_WINDOW: usize = 256;

/// The room. See the module header.
#[derive(Debug)]
pub struct Room {
    lease_ms: u64,
    items: BTreeMap<u64, Item>,
    next_item: u64,
    questions: Vec<Question>,
    next_question: u64,
    /// Who the room has met: session id to the parent it declared, or `None` for a top-level one. A
    /// ruling may name a session ([`Decision::Hand`]) that has never spoken here, and the daemon
    /// vouches for it with [`Room::vouch`] - which is what makes "top-level only" true for a hand.
    seen: BTreeMap<String, Option<String>>,
    /// Who is in this room: the project's tree, as the daemon that holds it last described it.
    /// Not the connections, because a subagent never opens one.
    members: Vec<crate::tree::Member>,
    /// The project's folder as its daemon last named it, for the channel's label.
    root: String,
    replay: BTreeMap<(String, String), Outcome>,
    replay_order: VecDeque<(String, String)>,
    /// Which project's room this is, on store records only: the room's own name is the server's
    /// fact (a [`Question`] carries it filled by the server), but a record must say whose queue
    /// the row is, because every project's room shares one file.
    project: String,
    /// The store every change is appended to, or `None` for a room that keeps nothing - the
    /// state machine alone, as every test that does not ask for durability drives it.
    store: Option<Arc<Mutex<Store>>>,
}

/// What a verb did: the item as it now stands, and what a subscriber should be told. `events` is
/// the whole of what a connection has to fan out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    pub item: Item,
    pub events: Vec<Event>,
}

/// Something a session may need to be woken by. Beats are not events: a beat is a fact about one
/// claim, and a bell that rings five times a minute is a bell nobody listens to.
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
    /// Loud, by design. `count` is how many claimants the item has now lost and
    /// `back_in_the_queue` is whether it is claimable again (`count < ABANDON_THRESHOLD`).
    Abandoned {
        item: u64,
        tag: String,
        by: String,
        why: Why,
        count: u32,
        back_in_the_queue: bool,
    },
    /// The item reached the threshold and is frozen until a ruling - see [`Question`].
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

    /// The tag the event is about - the audience's own name: the sessions queued under a tag are
    /// the ones a claim or an abandonment under it concerns.
    pub fn tag(&self) -> &str {
        match self {
            Event::Posted { tag, .. }
            | Event::Claimed { tag, .. }
            | Event::Done { tag, .. }
            | Event::Abandoned { tag, .. }
            | Event::Raised { tag, .. }
            | Event::Asked { tag, .. } => tag,
            // A ruling names the item and the decision, not the tag: the server asks the room for
            // it ([`Room::item`]), because the room is the one place a tag is decided.
            Event::Ruled { .. } => "",
        }
    }

    /// The sessions this event NAMES beyond whoever is queued under its tag: the one who did it,
    /// and the one a ruling hands something to. A server routes to these by name even when they
    /// have no item under the tag.
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

/// Why a verb was refused. Every sentence names the party it is about: a refusal that says *no*
/// without saying *who* is a refusal somebody re-asks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "refused", rename_all = "snake_case")]
pub enum Refused {
    /// A subagent speaks through its parent. The rule, at the door.
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
    /// The tag has nothing claimable: empty, or everything under it is claimed or finished.
    NothingToClaim {
        tag: String,
    },
    /// You do not hold the item you are trying to beat, wait on, finish or abandon.
    NotHeld {
        session: String,
        item: u64,
        holder: Option<String>,
    },
    /// A tag is a lock: somebody holds a claim under it.
    Busy {
        tag: String,
        item: u64,
        by: String,
        /// How long the holder has had it, as a duration a person reads (`1m 4s`).
        held_for: String,
    },
    /// First come, first served - and this is not the one at the front.
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
    /// A dispute about an item you hold yourself: there is nothing to rule on.
    NotDisputed {
        item: u64,
    },
    /// A room that no derivation would have written. A project key is twelve hex characters
    /// ([`crate::is_project_key`]), so a frame naming anything else names a room that cannot exist -
    /// refused rather than created, because it would have no membership and could never be found.
    UnknownRoom {
        project: String,
    },
    /// Chat frames are the server's, not the queue's.
    ChatIsTheServers,
    ChatEmpty,
    ChatTooLong {
        max: u64,
        got: u64,
    },
    /// A post that only says it was not for the poster, or acks. Silence costs nothing.
    NullPost,
    /// A post that repeats one of the room's last few messages nearly word for word. Silence, or
    /// the new part alone, says it once.
    EchoPost {
        seq: u64,
    },
    /// A line in the box-wide host room that names nobody: that room carries announcements and
    /// mentions, and talk between two sessions belongs in their project room or a dm.
    HostRoomUnaddressed,
    /// An announcement that runs to more than one line. An announcement is one line, refusably.
    AnnounceNotOneLine,
    /// A chat room this connection may not see, or that does not exist.
    UnknownChatRoom {
        room: String,
    },
    /// Nobody answers to this name, so the message would be told to no one. `known` is what a
    /// sender could have written instead - every name the room answers to now.
    UnknownAddressee {
        name: String,
        #[serde(default)]
        known: Vec<String>,
    },
    /// A handle another project's session goes by. A session's dm keeps to its own room's names,
    /// and a name from elsewhere is refused rather than quietly handed to that session.
    HandleInAnotherProject {
        name: String,
        sessions: Vec<String>,
    },
    UnknownThread {
        thread: u64,
    },
    /// A message that names somebody other than the two people a dm thread belongs to.
    ThreadIsAnotherDm {
        thread: u64,
        with: String,
    },
    /// A name the connection may not speak under: the operator's, or a malformed handle.
    ReservedName {
        name: String,
    },
    /// Another live connection already speaks as this external name.
    NameInUse {
        name: String,
    },
    /// A short name that fits more than one connected member, so the message names no one.
    Ambiguous {
        name: String,
        candidates: Vec<String>,
    },
    /// Only a head's operator seat may do this.
    NotTheOperator,
    /// The operator's seat is not a claim. A connection asking for it must present the token the
    /// coordinator minted at its start, and the sentence names where that file is.
    OperatorSeat {
        why: String,
    },
    /// A ref names something other than a todo or a queue row, or is empty.
    BadRef,
    /// A blob id no upload finished: the bytes a message would carry are not in the store.
    UnknownBlob {
        id: String,
    },
    /// A blob past the store's cap for one file.
    BlobTooLarge {
        max: u64,
        got: u64,
    },
    /// A mime the store will not keep: a message carries pictures and plain text, not programs.
    BadMime {
        mime: String,
    },
    /// A part that is not the one the upload waits for, so its bytes would be assembled out of
    /// order and hash to an id nothing else can produce.
    BadPart {
        expected: u32,
        got: u32,
    },
    /// More attachments named than a message carries.
    TooManyAttachments {
        max: u64,
        got: u64,
    },
    /// A session asked itself: a row waiting on its own answer would wait for ever.
    AskSelf,
    /// The operator cannot be asked through the room - `wait_on` has their own door.
    AskOperator,
    /// A subagent has no room connection, so nothing asked of it could ever be delivered.
    AskSubagent {
        session: String,
        parent: String,
    },
    /// Another host's session. Handles are this box's, and cross-host identities are not stable yet.
    AskOtherHost {
        name: String,
    },
    /// The target may not see the room the question would be asked in.
    AskCannotSee {
        session: String,
        room: String,
    },
    /// A queue seat (gatekeeper, reviewer) answers only its own project's sessions and the operator.
    QueueSeatOwnProject {
        session: String,
    },
    /// Nobody on this host answers to the name, and these are the top-level sessions that do.
    AskUnknown {
        name: String,
        known: Vec<String>,
    },
    /// Only a session connection asks: an ask is a wait between two top-level sessions.
    AskNotASession,
    /// A `Reply` or `Decline` from somebody other than the session that was asked.
    AskNotTheTarget {
        ask: u64,
        target: String,
    },
    /// The ask has already ended, and an ended ask takes no second answer.
    AskSettled {
        ask: u64,
        state: String,
    },
    /// No ask with this id: the room was restarted empty, or the id is not one.
    UnknownAsk {
        ask: u64,
    },
    /// Only the asker - or the operator's seat - may take an ask back.
    AskNotYours {
        ask: u64,
        asker: String,
    },
    /// A `Retire` for a session this connection does not speak for. B's daemon retires B.
    NotTheOwners {
        session: String,
    },
    /// An ownership verb refused, in the sentence that names the holder or the scope.
    Ownership {
        why: String,
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
            Refused::UnknownRoom { project } => write!(
                f,
                "`{project}` is not a project key — a room is named by twelve hex characters \
                 derived from a project's path, and a room made for anything else could never be \
                 found again"
            ),
            Refused::ChatIsTheServers => write!(f, "chat is answered by the coordinator's server"),
            Refused::ChatEmpty => write!(f, "a message with no text says nothing"),
            Refused::ChatTooLong { max, got } => write!(
                f,
                "a message is at most {max} bytes and this is {got} - say it shorter, or file the \
                 long part as a row and send its id"
            ),
            Refused::NullPost => write!(
                f,
                "not posted: a message not for you needs no reply - end the turn and say nothing"
            ),
            Refused::EchoPost { seq } => write!(
                f,
                "not posted: this repeats #{seq} almost word for word - say only what is new, or \
                 point at #{seq}"
            ),
            Refused::HostRoomUnaddressed => write!(
                f,
                "not posted: `host` carries announcements and mentions - name who it is for with \
                 `@name`, dm them, or send it as `kind: announce`"
            ),
            Refused::AnnounceNotOneLine => write!(
                f,
                "not posted: an announcement is one line - say the first line as the announcement, \
                 and file the rest as a row or a record"
            ),
            Refused::UnknownChatRoom { room } => write!(
                f,
                "`{room}` is not a room you can see - list the rooms for the ones you can"
            ),
            Refused::UnknownAddressee { name, known } => {
                write!(
                    f,
                    "nobody on this host answers to `{name}` - a message to a typo tells no one"
                )?;
                if !known.is_empty() {
                    write!(f, ". The names here are: {}", known.join(", "))?;
                }
                Ok(())
            }
            Refused::HandleInAnotherProject { name, sessions } => write!(
                f,
                "`{name}` is not a name in this room's project - a session's dm keeps to its own \
                 project's handles, so nothing was sent. It is {}'s: name that session id to reach \
                 it",
                sessions.join(", ")
            ),
            Refused::UnknownThread { thread } => write!(
                f,
                "#{thread} is not in the history of any room you can see, so a reply cannot hang \
                 from it"
            ),
            Refused::ThreadIsAnotherDm { thread, with } => write!(
                f,
                "#{thread} is the dm between `{with}` and its author, so a message to somebody \
                 else cannot answer it - drop `thread`, or say it to `{with}`"
            ),
            Refused::ReservedName { name } => {
                write!(f, "`{name}` is not a name this connection may speak under")
            }
            Refused::NameInUse { name } => {
                write!(f, "another connection already speaks as `{name}`")
            }
            Refused::Ambiguous { name, candidates } => write!(
                f,
                "`{name}` fits more than one member - name one of: {}",
                candidates.join(", ")
            ),
            Refused::NotTheOperator => write!(f, "only the operator's seat may do this"),
            Refused::OperatorSeat { why } => write!(
                f,
                "the operator's seat is granted by a token, not by naming it: {why}"
            ),
            Refused::BadRef => write!(
                f,
                "a ref is `todo`, `queue` or `evidence` with an id, at most {} per message",
                crate::chat::MAX_REFS
            ),
            Refused::UnknownBlob { id } => write!(
                f,
                "there is no blob `{id}` in this room's store - upload it with `put_blob` first, \
                 or say the message without it"
            ),
            Refused::BlobTooLarge { max, got } => write!(
                f,
                "a blob is at most {max} bytes and this one is {got} - upload a smaller file, or \
                 name it in the text instead"
            ),
            Refused::BadMime { mime } => write!(
                f,
                "`{mime}` is not a kind of file a message carries - the store keeps {}",
                crate::blob::MIMES.join(", ")
            ),
            Refused::BadPart { expected, got } => write!(
                f,
                "part {got} is not the one this upload waits for ({expected}) - parts append in \
                 order, so nothing was added"
            ),
            Refused::TooManyAttachments { max, got } => write!(
                f,
                "a message carries at most {max} attachments and this one names {got}"
            ),
            Refused::AskSelf => write!(
                f,
                "you cannot wait on your own answer - ask another top-level session, or `wait_on` \
                 with `operator` to ask the person"
            ),
            Refused::AskOperator => write!(
                f,
                "the operator answers through `wait_on`, not through the room: a row that needs \
                 their answer spells `operator` there, and this box's person is asked directly"
            ),
            Refused::AskSubagent { session, parent } => write!(
                f,
                "`{session}` is a subagent of `{parent}` and has no room connection, so nothing \
                 asked of it could be delivered - ask its parent, which is a service"
            ),
            Refused::AskOtherHost { name } => write!(
                f,
                "`{name}` is not a session of this host: an ask is carried by this box's room, \
                 and identities are not stable across hosts yet. Ask the session here that \
                 drives that machine."
            ),
            Refused::AskCannotSee { session, room } => write!(
                f,
                "`{session}` cannot see `{room}`, so the question would be said where it cannot \
                 read it - ask in `host`, which every session on the box reads, or `invite` it \
                 there first"
            ),
            Refused::QueueSeatOwnProject { session } => write!(
                f,
                "`{session}` is a merge-queue seat: it hears the operator and sessions of its own \
                 project by DM or ask, and no broadcast. Ask a session of that project, or the \
                 operator, to put the question to it"
            ),
            Refused::AskUnknown { name, known } => write!(
                f,
                "nobody on this host answers to `{name}`, so an ask of it would wait for ever. \
                 The top-level sessions here are: {}",
                if known.is_empty() {
                    "(none - no daemon has reported a tree to the room yet)".to_string()
                } else {
                    known.join(", ")
                }
            ),
            Refused::AskNotASession => write!(
                f,
                "only a top-level session asks: an ask is a wait between two of them, and this \
                 seat has no board to wait on"
            ),
            Refused::AskNotTheTarget { ask, target } => write!(
                f,
                "ask #{ask} was put to `{target}`, and only `{target}` may answer or decline it"
            ),
            Refused::AskSettled { ask, state } => write!(
                f,
                "ask #{ask} is {state} already - an ended ask takes no second answer"
            ),
            Refused::UnknownAsk { ask } => write!(
                f,
                "there is no ask #{ask} in this room - `room asks` lists the ones that are yours"
            ),
            Refused::AskNotYours { ask, asker } => write!(
                f,
                "ask #{ask} is `{asker}`'s: only the session that asked - or the operator's seat - \
                 can take it back"
            ),
            Refused::Ownership { why } => write!(f, "{why}"),
            Refused::NotTheOwners { session } => write!(
                f,
                "`{session}` is not the session this connection speaks for: `retire` is sent by a \
                 session's own daemon, for itself, when its session is deleted"
            ),
        }
    }
}

impl std::error::Error for Refused {}

/// A question for the tie-breaker: a decision to make, never a dashboard to interpret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Question {
    pub id: u64,
    pub asked_ms: u64,
    pub about: About,
    /// The question, in words, with the facts in it. A model answers this with a [`Decision`].
    pub text: String,
    pub facts: Facts,
    /// Which room asked it: the one field the room itself cannot fill, because a room is a
    /// project's and the arbiter hears every project's question.
    #[serde(default)]
    pub project: String,
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

/// The evidence a question names, so the arbiter can check the words against the numbers.
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
    /// Who is queued behind it, oldest first - the cost of keeping it alive.
    pub waiting: Vec<Waiting>,
}

/// One item queued behind the one a question is about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Waiting {
    pub item: u64,
    pub by: String,
    pub since_ms: u64,
}

/// What the arbiter may decide. Three, because the question is about one item and the answers are
/// *try again*, *give it to somebody*, or *stop asking*.
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

/// The answer, and who gave it: a ruling is a decision with an author, because a pane that cannot
/// say who ruled cannot say whether to trust it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ruling {
    pub decision: Decision,
    pub by: String,
    pub ms: u64,
}

/// What one session holds, and what it says it is waiting on - the answer to *what am I holding*,
/// which is the shape a todo reminder needs.
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
    /// What the holder said it is doing with it, when it took it - [`Claim::said`]. Never a reason
    /// for a reminder to be quiet: see [`Holding::waits_on`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub said: Option<Wait>,
}

/// One line a session should be reminded of: a holding, plus whether its beat has gone quiet enough
/// that the next [`Room::sweep`] will take the item back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reminder {
    pub session: String,
    pub holding: Holding,
    /// `true` when `now - beat_ms > lease_ms`: the room will take this back at its next pass.
    pub stale: bool,
}

impl Reminder {
    /// The sentence a todo reminder shows. Not a delivery mechanism - the sentence is the shape.
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
            members: Vec::new(),
            root: String::new(),
            replay: BTreeMap::new(),
            replay_order: VecDeque::new(),
            project: String::new(),
            store: None,
        }
    }

    pub fn lease_ms(&self) -> u64 {
        self.lease_ms
    }

    /// Load this project's rows from the store and keep the store from now on. The same
    /// [`Room::sweep`] the clock runs takes back a claim whose beat stopped while the coordinator
    /// was down, and a reload has no connection to route the events to, so they are the caller's.
    pub fn load(&mut self, store: &Arc<Mutex<Store>>, project: &str, now: u64) -> Vec<Event> {
        self.project = project.to_string();
        {
            let s = store.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(rows) = s.items_in(project) {
                self.next_item = self
                    .next_item
                    .max(rows.iter().map(|i| i.id).max().unwrap_or(0));
                self.items = rows.into_iter().map(|i| (i.id, i)).collect();
            }
            if let Some(rows) = s.questions_in(project) {
                self.next_question = self
                    .next_question
                    .max(rows.iter().map(|q| q.id).max().unwrap_or(0));
                self.questions = rows;
            }
        }
        self.store = Some(Arc::clone(store));
        self.sweep(now)
    }

    /// [`Room::load`], once: a room that already keeps a store has already loaded, and loading
    /// again would trade its live rows for the older shadow.
    pub fn ensure_loaded(&mut self, store: &Arc<Mutex<Store>>, project: &str, now: u64) {
        if self.store.is_none() {
            self.load(store, project, now);
        }
    }

    /// Replace the room's membership with the tree the daemon just described: a replacement and
    /// not a merge, because a session that left the tree has left the room.
    pub fn set_members(&mut self, rows: Vec<crate::tree::Member>) {
        self.members = rows;
        crate::tree::tree_order(&mut self.members);
    }

    /// Record the project's folder.
    pub fn set_root(&mut self, root: String) {
        if !root.trim().is_empty() {
            self.root = root;
        }
    }

    /// The project's folder, empty until a daemon named it.
    pub fn root(&self) -> &str {
        &self.root
    }

    /// Who is in this room, in the tree's order.
    pub fn members(&self) -> &[crate::tree::Member] {
        &self.members
    }

    /// The member with this session id, if the tree the daemon described holds one.
    pub fn member(&self, session: &str) -> Option<&crate::tree::Member> {
        self.members.iter().find(|m| m.session == session)
    }

    /// Declare a session the room has not met: the daemon's word for one that has not spoken yet.
    /// A subagent is refused here exactly as it is at a verb.
    pub fn vouch(&mut self, who: &Speaker) -> Result<(), Refused> {
        who.may_speak()?;
        self.seen.insert(who.session.clone(), who.parent.clone());
        Ok(())
    }

    /// Post a need under a tag.
    pub fn post(
        &mut self,
        who: &Speaker,
        request: &str,
        tag: &str,
        body: &str,
        now: u64,
    ) -> Result<Outcome, Refused> {
        // Identity before dedupe: a subagent is refused by name even when it reuses an id.
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
        self.put(&item);
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

    /// Claim the head of a tag - refused [`Refused::Busy`] while somebody else holds the tag, naming
    /// them and the item. A repeat by the session that already holds it is the same claim: no second
    /// `claim_ms`, and no second event.
    pub fn claim(
        &mut self,
        who: &Speaker,
        request: &str,
        tag: &str,
        now: u64,
    ) -> Result<Outcome, Refused> {
        // Identity before dedupe: a subagent is refused by name even when it reuses an id.
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

    /// Claim one particular item - the operator's manual control, and a client that read the queue
    /// and means *that* row. It must still be the head: an item id is not a way to jump the queue.
    pub fn claim_item(
        &mut self,
        who: &Speaker,
        request: &str,
        item: u64,
        now: u64,
    ) -> Result<Outcome, Refused> {
        // Identity before dedupe: a subagent is refused by name even when it reuses an id.
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

    /// Say you are still working on it. Only the holder may beat; a late beat from a session whose
    /// claim was taken is refused by name ([`Refused::NotHeld`]) - see [`Room::dispute`].
    pub fn beat(
        &mut self,
        who: &Speaker,
        request: &str,
        item: u64,
        now: u64,
    ) -> Result<Outcome, Refused> {
        // Identity before dedupe: a subagent is refused by name even when it reuses an id.
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
        self.put(&it);
        self.met(who);
        let out = Outcome {
            item: it,
            events: Vec::new(),
        };
        self.remember(who, request, &out);
        Ok(out)
    }

    /// Say what you are waiting on, in your own words. Empty text clears it. Only the holder may, and
    /// it is the answer to *so they don't occupy it for ever*: the reminder carries it back, and the
    /// queue shows it to whoever is waiting behind.
    pub fn waiting_on(
        &mut self,
        who: &Speaker,
        request: &str,
        item: u64,
        text: &str,
        now: u64,
    ) -> Result<Outcome, Refused> {
        // Identity before dedupe: a subagent is refused by name even when it reuses an id.
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
        self.put(&it);
        self.met(who);
        let out = Outcome {
            item: it,
            events: Vec::new(),
        };
        self.remember(who, request, &out);
        Ok(out)
    }

    /// Say what you are doing with it: the acknowledgement, and the opposite fact from declaring
    /// a wait, because a holder that is working is not a holder somebody should take from.
    pub fn acknowledge(
        &mut self,
        who: &Speaker,
        request: &str,
        item: u64,
        text: &str,
        now: u64,
    ) -> Result<Outcome, Refused> {
        // Identity before dedupe: a subagent is refused by name even when it reuses an id.
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
            claim.said = if text.is_empty() {
                None
            } else {
                Some(Wait {
                    text: text.to_string(),
                    since_ms: now,
                })
            };
        }
        let it = it.clone();
        self.put(&it);
        self.met(who);
        let out = Outcome {
            item: it,
            events: Vec::new(),
        };
        self.remember(who, request, &out);
        Ok(out)
    }

    /// The work is done. Only the holder may.
    pub fn done(
        &mut self,
        who: &Speaker,
        request: &str,
        item: u64,
        now: u64,
    ) -> Result<Outcome, Refused> {
        // Identity before dedupe: a subagent is refused by name even when it reuses an id.
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
        self.put(&it);
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

    /// Give it up on purpose. Still an abandonment, still counted: an item everybody gives up on is
    /// a problem whether or not each gave up politely.
    pub fn abandon(
        &mut self,
        who: &Speaker,
        request: &str,
        item: u64,
        why: &str,
        now: u64,
    ) -> Result<Outcome, Refused> {
        // Identity before dedupe: a subagent is refused by name even when it reuses an id.
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

    /// *I am still on it* - a claimant whose beat went stale says so instead of fighting for the
    /// claim, and the arbiter is asked which of the two has it. One open question per item: asking
    /// twice is the same question.
    pub fn dispute(
        &mut self,
        who: &Speaker,
        request: &str,
        item: u64,
        says: &str,
        now: u64,
    ) -> Result<Outcome, Refused> {
        // Identity before dedupe: a subagent is refused by name even when it reuses an id.
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
        let it = self.items[&item].clone();
        self.put(&it);
        let tag = it.tag.clone();
        let out = Outcome {
            item: it,
            events: vec![Event::Asked {
                item,
                tag,
                question: q.id,
            }],
        };
        self.remember(who, request, &out);
        Ok(out)
    }

    /// Answer a question. The decision is applied and the item unfreezes - [`Decision::Retry`] back
    /// to the head with its count kept, [`Decision::Hand`] a claim for the named session now,
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
        // Validate before touching anything: a ruling that is refused leaves the item frozen and the
        // question open, which is the only honest outcome.
        if let Decision::Hand { to } = &decision {
            // The tree is the room's answer to *has it met this one*: a member the daemon described
            // is a session of this project, and a hand to one needs no separate vouch.
            let known = match self.seen.get(to) {
                Some(parent) => Some(parent.clone()),
                None => self.member(to).map(|m| m.parent.clone()),
            };
            match known {
                None => {
                    return Err(Refused::UnknownSession {
                        session: to.clone(),
                    });
                }
                Some(Some(parent)) => {
                    return Err(Refused::Subagent {
                        session: to.clone(),
                        parent,
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
                            // A ruling hands the item to somebody who has not spoken about it, so
                            // there is nothing they said to record.
                            said: None,
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
        if let Some(answered) = self.question(question).cloned() {
            self.put_question(&answered);
        }
        let it = self.items[&item].clone();
        self.put(&it);
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

    /// Take back every claim whose beat has stopped: `now - beat_ms > lease_ms`, exactly the queue
    /// lease's comparison, so a beat at the boundary is still alive.
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

    /// The session is gone: its daemon exited, was restarted, or the socket that owns it closed.
    /// Every claim it holds is abandoned at once, with the caller's reason on it, rather than after
    /// the lease.
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

    pub fn item(&self, id: u64) -> Option<&Item> {
        self.items.get(&id)
    }

    /// Every item in the room, oldest first - what the pane draws, finished and dropped included.
    pub fn all(&self) -> Vec<&Item> {
        self.items.values().collect()
    }

    /// A tag's queue, in order: `(posted_ms, id)`, the claimed and the abandoned rows included, the
    /// finished and dropped ones out.
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

    /// The next item anybody may claim - the first claimable row, so a raised item at the head does
    /// not block the tag.
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

    /// What this session holds, and what it is waiting on - the reminder's facts.
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
                    said: c.said.clone(),
                })
            })
            .collect()
    }

    /// Every reminder the room owes, one per live claim - what the todo pass turns into rows.
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
                        said: c.said.clone(),
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

    /// The open questions, oldest first - what the tie-breaker is asked, in the order it should
    /// answer them.
    pub fn open_questions(&self) -> Vec<&Question> {
        self.questions
            .iter()
            .filter(|q| q.answer.is_none())
            .collect()
    }

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
                said: None,
            },
        };
        it.question = None;
        let it = it.clone();
        self.put(&it);
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

    /// Lose a claim - the one path every abandonment goes through, so the count, the event and the
    /// raise cannot drift apart.
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
        // Read it back after the raise, so the outcome carries the question id too: a caller that
        // holds an item with `question: None` would draw a frozen item as claimable.
        let it = self.items[&item].clone();
        self.put(&it);
        Outcome { item: it, events }
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
            project: String::new(),
            answer: None,
        };
        self.questions.push(q.clone());
        self.put_question(&q);
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
            project: String::new(),
            answer: None,
        };
        self.questions.push(q.clone());
        self.put_question(&q);
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

    /// Append the item as it now stands, if the room keeps a store. Every state change ends in a
    /// put, so the file cannot drift from the room; the lock is taken with `into_inner` because a
    /// poisoned sibling would otherwise cost every verb its durability for no gain.
    fn put(&self, item: &Item) {
        let Some(store) = &self.store else { return };
        store
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .put_item(&self.project, item.clone());
    }

    /// Append the question as it now stands - asked, or answered.
    fn put_question(&self, question: &Question) {
        let Some(store) = &self.store else { return };
        store
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .put_question(&self.project, question.clone());
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
                // The bell rang the first time: a repeat is the client catching up on the REPLY,
                // not a second thing happening.
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

/// A tag as the room spells it: trimmed and lowercased, and empty is refused by name. Public
/// because a caller that shows a tag must spell it the way the room will.
pub fn normalise_tag(tag: &str) -> Result<String, Refused> {
    let tag = tag.trim().to_lowercase();
    if tag.is_empty() {
        return Err(Refused::EmptyTag);
    }
    Ok(tag)
}

impl Speaker {
    /// The rule, at the door. A subagent is refused by name; everything else passes.
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
