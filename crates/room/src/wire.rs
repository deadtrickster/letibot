//! The wire: frames a coordinator's socket takes and sends. [`Room::answer`] is the dispatch without the socket.
//! Every mutating frame carries a client-minted `id`: a repeat is answered from the replay window
//! with no events, and a version mismatch is [`Response::Bye`].

use serde::{Deserialize, Serialize};

use crate::item::{Item, Speaker};
use crate::room::{
    ABANDON_THRESHOLD, DEFAULT_BEAT_EVERY_MS, Decision, Holding, Question, Refused, Reminder, Room,
};

/// The room's protocol version, and the room's alone - see the module header. The number moves at
/// RELEASE POINTS: a frame added mid-series rides the next number with whatever else lands first.
/// 10 is the ask: a version-9 coordinator knows no `Ask`, and would answer one with a refusal a
/// session would read as *this peer cannot be asked*. 11 is the ask by SCOPE: a version-10
/// coordinator knows no `path` or `topic`, and would read the asker's scope as a session name.
pub const ROOM_PROTOCOL_VERSION: u16 = 11;

/// The sentence a protocol mismatch is refused with, or `None` when the two agree. One function because
/// two doors refuse it - a [`crate::Server`]'s and [`crate::Room::answer_traced`]'s - and two sentences
/// about one skew would drift. It says which side is older, because a reader has to work that out.
pub fn protocol_refusal(theirs: u16) -> Option<String> {
    if theirs == ROOM_PROTOCOL_VERSION {
        return None;
    }
    let (older, how) = if theirs < ROOM_PROTOCOL_VERSION {
        ("the client", CLIENT_UPGRADE)
    } else {
        ("this room", ROOM_UPGRADE)
    };
    Some(format!(
        "this room speaks protocol {ROOM_PROTOCOL_VERSION} and the client speaks {theirs} — {older} \
         is the older of the two, so a frame one of them does not know would be obeyed wrongly and \
         nothing was done: {how}"
    ))
}

/// The command that replaces an older coordinator with the installed build.
pub const ROOM_UPGRADE: &str =
    "restart the coordinator on the current build with `letibot --room --stop && letibot --room`";
/// What replaces an older daemon or head.
pub const CLIENT_UPGRADE: &str =
    "restart the daemon on the current build with `letibot --stop`, then start `letibot` again";

/// The command to add to a refusal that a coordinator of any build wrote ("this room speaks
/// protocol X and the client speaks Y"), `None` when `why` is not that refusal.
pub fn upgrade_hint(why: &str) -> Option<&'static str> {
    let rest = why.split("this room speaks protocol ").nth(1)?;
    let (room, rest) = rest.split_once(" and the client speaks ")?;
    let room: u16 = room.trim().parse().ok()?;
    let client: u16 = rest
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()?;
    match room.cmp(&client) {
        std::cmp::Ordering::Less => Some(ROOM_UPGRADE),
        std::cmp::Ordering::Greater => Some(CLIENT_UPGRADE),
        std::cmp::Ordering::Equal => None,
    }
}

/// What a `say` is. `post` is a room line and the default every frame written before this decodes
/// to; the other words are what the line asks of its readers, so they change who is woken.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SayKind {
    #[default]
    Post,
    /// The one host-room line that may name nobody, and it is one line by construction.
    Announce,
    /// Nothing is asked of any reader: a note for the next turn, never a turn of its own. A dm
    /// addressed to a reader still wakes them, because a dm is the one thing that is theirs.
    Fyi,
    /// A question for one named principal, held as a room ask - see [`Request::Ask`].
    Ask,
    /// Something that stops the poster until it is answered: it rings its target and flags the
    /// operator, because a blocked session is the person's to unblock.
    Blocker,
    /// A conclusion the room keeps: the digest ranks it above the talk it settles.
    Decision,
    /// A word this build does not know: an ordinary line, because a frame from a newer client (or
    /// a careless one) should say something rather than fail to parse.
    #[serde(other)]
    Other,
}

impl SayKind {
    /// The kinds a sender may name, in the order a reader needs them - the schema's list and the
    /// one this crate answers to.
    pub const NAMED: [SayKind; 6] = [
        SayKind::Post,
        SayKind::Fyi,
        SayKind::Ask,
        SayKind::Blocker,
        SayKind::Decision,
        SayKind::Announce,
    ];

    /// The kind a word names; anything unknown is an ordinary line, so an old client and a
    /// misspelling both say something rather than fail.
    pub fn of(word: &str) -> SayKind {
        match word.trim().to_ascii_lowercase().as_str() {
            "announce" => SayKind::Announce,
            "fyi" => SayKind::Fyi,
            "ask" => SayKind::Ask,
            "blocker" => SayKind::Blocker,
            "decision" => SayKind::Decision,
            _ => SayKind::Post,
        }
    }

    /// The word a client writes for this kind - what [`SayKind::of`] reads back, and what a stored
    /// message keeps in its [`crate::chat::SAY`] ref.
    pub fn word(self) -> &'static str {
        match self {
            SayKind::Post | SayKind::Other => "post",
            SayKind::Announce => "announce",
            SayKind::Fyi => "fyi",
            SayKind::Ask => "ask",
            SayKind::Blocker => "blocker",
            SayKind::Decision => "decision",
        }
    }

    /// Does this kind need a target named in the frame? An ask does: a question nobody was asked
    /// is not held for anybody to answer.
    pub fn needs_target(self) -> bool {
        self == SayKind::Ask
    }

    /// Is this a word a stored message has to carry, because it changes how a reader treats it?
    /// `post`, an announcement and an unknown word do not: they read as they always did.
    pub fn recorded(self) -> bool {
        matches!(
            self,
            SayKind::Fyi | SayKind::Ask | SayKind::Blocker | SayKind::Decision
        )
    }
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
        /// The coordinator's operator token, for a connection asking for the operator's seat: a
        /// file's contents, not a secret in the frame. The room writes `<socket>.token` at its start
        /// (mode 0600, hidden from a confined tool), so only that file's reader can be the person.
        #[serde(default)]
        token: Option<String>,
        /// An external asking to be one more connection of a name that is already connected (a
        /// bridge and its waiter). The socket is the same user's, so the claim is the user's own.
        #[serde(default)]
        share: bool,
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
        /// Blob ids from [`Request::PutBlob`]: the pictures and files the message carries. The
        /// server fills the message's attachments from its store and refuses an id it does not hold.
        #[serde(default)]
        attachments: Vec<String>,
        /// What the line asks of its readers - see [`SayKind`]. Absent from every frame written
        /// before it, which is an ordinary room line.
        #[serde(default)]
        kind: SayKind,
    },
    /// A direct message: read by its author and `to` only.
    Dm {
        id: String,
        to: String,
        text: String,
        #[serde(default)]
        thread: Option<u64>,
        #[serde(default)]
        attachments: Vec<String>,
    },
    /// One part of a file, on its way into the blob store. `data` is base64, so a copy that is not
    /// UTF-8 still crosses a frame that is one line of JSON.
    PutBlob {
        /// The client's own name for this upload, unique among its uploads in flight.
        upload: String,
        name: String,
        mime: String,
        /// The 0-based index of this part: parts append in order, and a gap is refused.
        part: u32,
        /// This is the last part, so the store hashes what it holds and answers the attachment.
        last: bool,
        data: String,
    },
    /// Ask for one part of a stored blob, from `offset` bytes in.
    GetBlob {
        id: String,
        #[serde(default)]
        offset: u64,
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
        /// Only messages older than this `seq`, the newest `last` of them: one page back.
        #[serde(default)]
        before: Option<u64>,
    },
    /// A room's messages whose text holds `text`, newest first - `/search` over the log, so a hit
    /// older than the ring is found too. A read: it moves nothing.
    Search {
        room: String,
        text: String,
        #[serde(default)]
        last: Option<u32>,
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
    /// The signal scorecard: what followed each sender's and each kind's messages.
    Signals,
    /// Something followed message `seq` (a todo row naming it, an ownership claim). The caller is
    /// the actor. Also the hook R163 calls when a claim comes of a message.
    Acted { seq: u64, act: crate::signal::Act },
    /// A wake caused by these messages ended: whether it said or did anything, and the tokens the
    /// turn spent. Reported by the session's daemon, which is the only one that knows.
    WakeEnded {
        seqs: Vec<u64>,
        cheap: bool,
        silent: bool,
        tokens: u64,
    },
    /// Invite `session` (an id or a handle) into a project room that is not its own: it reads
    /// and is told that room's chat until it leaves. `room` is `project/<key>` or the room's label.
    Invite { room: String, session: String },
    /// Take `session` out of a room it was invited into; `None` is the caller itself.
    Leave {
        room: String,
        #[serde(default)]
        session: Option<String>,
    },
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
    /// The project's tree, as the daemon holding it sees it - who is in this room. The room cannot
    /// work this out: it keeps no tree and a subagent never opens a connection. A replacement and
    /// not a merge, because a merge would keep a session closed since.
    Members {
        rows: Vec<crate::tree::Member>,
        #[serde(default)]
        root: String,
    },
    /// This session's facts, from its daemon, every tick: what its roster presence is derived
    /// from. A replacement with no id, like [`Request::Members`].
    Seat { facts: crate::seat::SeatFacts },
    /// Own `scope` (a path glob or a topic) for `ttl_ms`, or renew this session's own claim on it.
    /// `thread` is the message the claim answers, for the signal ledger.
    Own {
        id: String,
        scope: String,
        #[serde(default)]
        ttl_ms: Option<u64>,
        #[serde(default)]
        thread: Option<u64>,
    },
    /// Offer an owned scope to `to`; the owner holds it until `to` accepts.
    Hand {
        id: String,
        scope: String,
        to: String,
        #[serde(default)]
        note: String,
    },
    /// Take a scope handed to this session.
    Accept { id: String, scope: String },
    /// End a claim: its owner, the operator's seat, or the receiver declining a handover.
    Release { id: String, scope: String },
    /// A tag's queue, in order - the read that makes polling unnecessary after a reconnect.
    Queue { tag: String },
    /// One item by the number a sentence gave, out of the live window or the closed segment: a
    /// settled row that aged out of memory is still answerable, which is what keeps a queue deeper
    /// than the window accountable. A read: it moves nothing.
    Item { item: u64 },
    /// What a session holds and what it is waiting on. `session: None` means *me*.
    Holding { session: Option<String> },
    /// Every reminder the room owes.
    Reminders,
    /// The open questions, oldest first - what the tie-breaker is asked.
    Questions,
    /// Ask `to` `question` in a room, and hold the ask: its id is the `seq` the question is said
    /// with, and the room answers with [`Response::Asked`]. `room` empty is the room `say` would
    /// default to. The same asker, target and question already open is that ask, not a second one.
    ///
    /// An ask may name a SCOPE instead - `path` (repo-relative) or `topic` - and the room resolves
    /// it to the session that owns it ([`crate::scope`]). A scope and no `to` is the owner's ask.
    Ask {
        id: String,
        #[serde(default)]
        room: String,
        #[serde(default)]
        to: String,
        #[serde(default)]
        path: String,
        #[serde(default)]
        topic: String,
        question: String,
    },
    /// The target's answer to ask `ask` - a thread reply in the ask's room, addressed to the asker.
    /// Only the target may send it, and only while the ask is open.
    Reply { id: String, ask: u64, text: String },
    /// The target will not answer. Posted the same way as a [`Request::Reply`].
    Decline { id: String, ask: u64, text: String },
    /// The asker - or the operator's seat - takes the ask back. A note in the thread tells the
    /// target, so a question it was nagged about does not simply vanish.
    Withdraw { id: String, ask: u64 },
    /// `session` is shut down for good: its session was deleted, so every open ask of it and by it
    /// ends. Sent by the daemon that owns it, for that session only, before its connection closes.
    /// This is the only `DOWN`: a daemon exit or a crash is a restart the operator owes.
    Retire { id: String, session: String },
    /// What this session made (`by_me`) and what it was asked (`of_me`), settled ones included -
    /// the one read the room pass applies. A read: it moves nothing.
    Asks,
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
            | Request::Search { .. }
            | Request::Roster { .. }
            | Request::Inbox { .. }
            | Request::Ack { .. }
            | Request::Signals
            | Request::Acted { .. }
            | Request::WakeEnded { .. }
            | Request::Invite { .. }
            | Request::Leave { .. }
            | Request::Queue { .. }
            | Request::Item { .. }
            | Request::Holding { .. }
            | Request::Reminders
            | Request::Questions
            | Request::Members { .. }
            | Request::Seat { .. }
            | Request::PutBlob { .. }
            | Request::GetBlob { .. }
            | Request::Room
            | Request::Asks => None,
            Request::Say { id, .. }
            | Request::Dm { id, .. }
            | Request::Notify { id, .. }
            | Request::Ask { id, .. }
            | Request::Reply { id, .. }
            | Request::Decline { id, .. }
            | Request::Withdraw { id, .. }
            | Request::Retire { id, .. }
            | Request::Own { id, .. }
            | Request::Hand { id, .. }
            | Request::Accept { id, .. }
            | Request::Release { id, .. }
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
        /// Which coordinator start answered. A client that sees it change knows the room restarted and
        /// drops marks it kept against the old one. `0` from a `Room` driven without a server.
        #[serde(default)]
        instance: u64,
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
    /// One item by its number - the answer to [`Request::Item`]. `None` is *this room never minted
    /// that number*, said rather than refused: a read is not a verb.
    Item {
        item: Option<Item>,
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
        /// `Server::identity()`: *started T ago and holding N item(s)*, and whether the room keeps a
        /// store. That second half is what makes *restarted clean* and *restarted empty* two
        /// readings, since an in-memory room is empty at every start.
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
    /// The answer to [`Request::Seat`].
    Seated {
        /// This session's live claims, by scope: what its daemon judges a passing line against.
        #[serde(default)]
        owns: Vec<String>,
    },
    /// The claim an ownership verb made, moved or ended, and the sentence for the caller.
    Owned {
        id: String,
        claim: crate::own::Claim,
        said: String,
    },
    Rooms {
        rows: Vec<crate::chat::RoomInfo>,
    },
    Said {
        id: String,
        message: crate::chat::Message,
        /// What the room wants the sender to know about a post it did not refuse: that an owner
        /// could have answered this question, or why the question went to the room instead. Empty
        /// for a post the room has nothing to add to.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        note: String,
    },
    History(crate::chat::History),
    /// The ask a frame made or settled, as the room holds it now - the answer to [`Request::Ask`],
    /// [`Request::Reply`], [`Request::Decline`] and [`Request::Withdraw`].
    Asked {
        id: String,
        ask: crate::chat::AskView,
    },
    /// The answer to [`Request::Asks`]: this session's own asks, both halves, oldest first.
    Asks {
        by_me: Vec<crate::chat::AskView>,
        of_me: Vec<crate::chat::AskView>,
    },
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
    /// The answer to [`Request::Signals`].
    Signals {
        card: crate::signal::Scorecard,
    },
    /// The answer to [`Request::Acted`] and [`Request::WakeEnded`].
    Noted,
    /// A chat message pushed to a connection that may see it. Never sent to its author.
    /// `names_you` says the message names this reader (addressed or `@`-mentioned).
    Chat {
        message: crate::chat::Message,
        names_you: bool,
    },
    /// The answer to [`Request::PutBlob`]'s last part: the blob is stored, and this is what a
    /// message names. `None` from a part that is not the last - there is no blob until the bytes are
    /// whole, so a client reads the id only from the frame that finished the upload.
    Blob {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attachment: Option<crate::chat::Attachment>,
    },
    /// The answer to [`Request::GetBlob`]: one part of the bytes from `offset` in. `last` says no
    /// part follows, so a reader stops asking.
    BlobPart {
        attachment: crate::chat::Attachment,
        offset: u64,
        /// Base64 of at most [`crate::blob::MAX_PART`] bytes.
        data: String,
        last: bool,
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
                token: _,
                share: _,
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
                    instance: 0,
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
            | Request::Search { .. }
            | Request::Roster { .. }
            | Request::Inbox { .. }
            | Request::Ack { .. }
            | Request::Signals
            | Request::Acted { .. }
            | Request::WakeEnded { .. }
            | Request::Invite { .. }
            | Request::PutBlob { .. }
            | Request::GetBlob { .. }
            | Request::Ask { .. }
            | Request::Reply { .. }
            | Request::Decline { .. }
            | Request::Withdraw { .. }
            | Request::Retire { .. }
            | Request::Asks
            | Request::Seat { .. }
            | Request::Own { .. }
            | Request::Hand { .. }
            | Request::Accept { .. }
            | Request::Release { .. }
            | Request::Leave { .. } => Answer::quiet(Response::Refused {
                id: request.request_id().map(str::to_string),
                why: Refused::ChatIsTheServers,
            }),
            Request::Queue { tag } => Answer::quiet(Response::Queue {
                tag: tag.trim().to_lowercase(),
                rows: self.queue(tag).into_iter().cloned().collect(),
            }),
            Request::Item { item } => Answer::quiet(Response::Item {
                item: self.read_item(*item),
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
            Request::Members { rows, root } => {
                self.set_members(rows.clone());
                self.set_root(root.clone());
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The number is the contract: a version-10 coordinator knows no `path` or `topic` and would
    /// read a scope ask as an ask of a session called `""`.
    #[test]
    fn the_protocol_is_eleven() {
        assert_eq!(ROOM_PROTOCOL_VERSION, 11);
        assert_eq!(crate::server::protocol(), 11);
        assert_eq!(
            protocol_refusal(11),
            None,
            "the same build speaks to itself"
        );
        let older = protocol_refusal(10).expect("a version-10 client is told");
        assert!(older.contains("this room speaks protocol 11"), "{older}");
        assert!(older.contains("the client is the older"), "{older}");
        assert!(older.contains(CLIENT_UPGRADE), "{older}");
        let newer = protocol_refusal(12).expect("a newer client is told");
        assert!(newer.contains(ROOM_UPGRADE), "{newer}");
        // And an old client's frame is still a frame: the attachments it cannot know are defaulted,
        // so a version-9 head reading a 10 daemon's `say` sees exactly what it always saw.
        let old: Request = serde_json::from_str(r#"{"type":"say","id":"1","text":"hi"}"#).unwrap();
        match old {
            Request::Say {
                attachments, kind, ..
            } => {
                assert!(attachments.is_empty());
                assert_eq!(
                    kind,
                    SayKind::Post,
                    "a frame with no kind is an ordinary line"
                );
            }
            other => panic!("a say is a say: {other:?}"),
        }
        // The one kind a host room takes from a session that names nobody, and the default for
        // every other word: a misspelling says something rather than failing.
        let announce: Request =
            serde_json::from_str(r#"{"type":"say","id":"1","text":"hi","kind":"announce"}"#)
                .unwrap();
        match announce {
            Request::Say { kind, .. } => assert_eq!(kind, SayKind::Announce),
            other => panic!("a say is a say: {other:?}"),
        }
        assert_eq!(SayKind::of("announce"), SayKind::Announce);
        assert_eq!(SayKind::of("announce "), SayKind::Announce);
        assert_eq!(SayKind::of("shout"), SayKind::Post);
        // A word this build does not know is a line, not a frame nobody can read: a newer client's
        // next kind must not make an older room refuse the message outright.
        let odd: Request =
            serde_json::from_str(r#"{"type":"say","id":"1","text":"hi","kind":"shout"}"#).unwrap();
        match odd {
            Request::Say { kind, .. } => assert_ne!(kind, SayKind::Announce),
            other => panic!("a say is a say: {other:?}"),
        }
    }

    /// An `ask` frame from before the scope names a session and nothing else: the two scope fields
    /// default to empty, so an older client's ask is the ask it always was.
    #[test]
    fn an_ask_frame_from_before_the_scope_still_parses() {
        let old: Request =
            serde_json::from_str(r#"{"type":"ask","id":"1","to":"s-b","question":"port?"}"#)
                .unwrap();
        match old {
            Request::Ask {
                to, path, topic, ..
            } => {
                assert_eq!(to, "s-b");
                assert!(path.is_empty() && topic.is_empty());
            }
            other => panic!("an ask is an ask: {other:?}"),
        }
    }
}
