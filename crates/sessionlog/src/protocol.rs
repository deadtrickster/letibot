//! The head frame format: ATTACH, RESYNC, snapshot, event envelope, `expected_seq`
//! and `client_request_id`.
//!
//! §13.2 and §13.4 give this in prose; `docs/workstreams.md` names it as the
//! second-highest-value interface to fix early, because *"fixing it makes W8 a leaf
//! that can start immediately against a recorded log"*. This module is that fix.
//!
//! # Transport-independent on purpose
//!
//! §13.4 says the remote head speaks *"the same frames as the socket"*. So the
//! frames are plain serde types and the framing ([`crate::wire`]) is one
//! newline-delimited-JSON codec over any `Read`/`Write`. A WebSocket head is then a
//! different `Read`/`Write`, not a different protocol.
//!
//! # The three rules the frame shapes enforce
//!
//! 1. **`Ack` carries `seq`, `rendered` and `filtered`, and `seq` comes from the
//!    batch, not from the kept events.** See [`crate::cursor`]. There is no
//!    constructor that takes a "last kept seq", because that is the bug.
//! 2. **`Hello.dropped` is a bare `u64`.** Present and zero. No `skip_serializing_if`
//!    anywhere in this module, and a test says so.
//! 3. **Every mutating command carries `expected_seq` and `client_request_id`**
//!    (§13.2). A command with a stale `expected_seq` is *rejected with both
//!    numbers*, so the head can say what it was looking at.

use serde::{Deserialize, Serialize};

use crate::event::Envelope;
use crate::registry::{SessionBrief, SessionWiring};
use crate::scrub::ScrubReport;
use crate::view::Snapshot;

/// Bumped when a frame's meaning changes. Both sides refuse a mismatch loudly —
/// §17-S6's rule, applied here because the head protocol has the same failure mode
/// as the control channel: a silent version skew that looks like a bug in the other
/// half.
///
/// **4** since a head can reach sessions that are not in the daemon yet.
///
/// `ResumeSession` and `RenameSession` are new client frames and
/// `SessionEvent::SessionRenamed` is a new event, so a version-3 head talking to a
/// version-4 daemon would fail to parse an event it is sent mid-session — which is a
/// deserialization error in the middle of a turn, and the worst possible place for
/// one. Both sides refuse the mismatch at ATTACH instead.
///
/// What made it necessary: a stored session is now **resumable**, and a head is
/// where the operator asks for that. Without a frame for it, `letibot --continue`
/// against a *running* daemon could only work by killing the daemon and restarting
/// it with `--session` — which would take down every other session on the box to
/// open one, and this box runs several.
///
/// **3** since a tool call's body got a channel of its own.
///
/// `DeltaTarget` gained `ToolCall` and `TurnView` gained `raw_calls`, closing
/// T13.5: the raw `<function=…>` markup a model writes inside `<tool_call>` used
/// to be announced as `Text`, so every head rendered it as prose until the closing
/// tag arrived and it was replaced by a card. That is a *protocol* fault — the
/// boundary exists in the engine, which is walking vocabulary ids, and is gone by
/// the time the markup is a string — so it is fixed here rather than guessed at in
/// a head. A version-2 head talking to a version-3 daemon would fail to parse the
/// new `target` value; a version-3 head talking to a version-2 daemon would show
/// the markup again. Both sides refuse a mismatch by name, so the failure is one
/// line in a terminal instead.
///
/// **2** was the daemon growing more than one session: `Hello` carrying what the
/// head is attached *to* (§4.4) and the session list a picker is drawn from,
/// `Attach`'s `session_id` naming one of several rather than being checked against
/// the only one, and `ToolCallProposed` carrying a display target (§4.1).
/// # 3 → 5, and why it skips 4
///
/// 4 is the `session-resume` branch's, landing separately. D10 asked for the
/// question-answer vocabulary to **coordinate to 5 rather than race it**, so this
/// takes 5 and leaves 4 where it was going. Both sides already refuse a mismatch by
/// name (`crates/sessionlog/src/server.rs` compares this constant and says both
/// numbers), so a head built against 3 or 4 is told which version it is speaking to
/// rather than failing on the first frame it does not understand.
///
/// What 5 adds: [`ClientFrame::AnswerQuestion`] and
/// [`crate::question::QuestionAnswer`] — a head answering a **question** rather than
/// granting a **permission**. See `crates/sessionlog/src/question.rs` for why those
/// are two vocabularies and not one.
///
/// # 5 → 6: a denial the operator can see
///
/// `docs/boundary-and-adjudication.md` §4b, which is a requirement and not a
/// nicety: *"a denial the operator cannot see manufactures the workaround"* — the
/// model infers the approach was wrong rather than forbidden, tries a variant, and
/// the task dies with the operator seeing only a dead task. Nothing on this wire
/// could carry a refusal. `Warning` is for §18's assertions and using it here would
/// make a decision taken on the operator's behalf look like a defect, which is the
/// same abuse [`crate::SessionEvent::CommandIssued`] exists to avoid one variant
/// along.
///
/// So 6 adds [`crate::SessionEvent::DenialRaised`], published **at the moment the
/// gate decides** rather than at turn end. Both sides refuse a mismatch by name, so
/// a head built against 5 is told which version it is speaking to rather than
/// silently missing every refusal — which would be the very defect, one layer down.
///
/// # 7: a head can answer
///
/// 6 gave the operator the *sight* of a refusal and 7 gives them the **reply**, which
/// is the half §4b actually turns on: *"the grant path is reachable at the moment of
/// denial, not after the task has died"*, and a path that only goes one way is not a
/// path. The frames to reply with have existed since 5; what did not exist was
/// anything on the daemon side that a reply could reach, because the queue they
/// landed on is drained by the thread that is waiting for them.
///
/// Two payload changes, and they are one seam rather than two because they are the
/// same missing half:
///
/// - [`crate::SessionEvent::DecisionRequested`] grows `choices` and `because`. A
///   question's plain-text options had nowhere to sit — `options` carries
///   `OptionKind`, an adjudication vocabulary answering *may this run* — so
///   `ask_user_question` could be posed only by discarding the choices, which is why
///   T25/D10 was specified and not built.
/// - [`crate::event::OptionKind`] grows `AllowSession`, so the widest an *answer*
///   goes has a spelling. Anything standing beyond one session is a **mode** rather
///   than a grant, and a mode is not an option on a prompt.
///
/// Both sides refuse a mismatch by name. A head built against 6 that was handed a 7
/// question would render an empty option list and ask a person to choose between
/// nothing.
///
/// # 8: a head can compact
///
/// [`ClientFrame::CompactSession`] is a new client frame, so a version-7 head
/// talking to a version-8 daemon is fine (it never sends the frame) but a
/// version-8 head talking to a version-7 daemon would send a frame the daemon
/// fails to parse — the same mid-session deserialization failure that forced
/// version 4, and the same ATTACH-time refusal applies. The daemon's answer to
/// the frame is the ordinary `Accepted`/`Rejected` pair; the compaction itself is
/// disclosed on the session's log as the turn and the summary item it produces,
/// so no new event kind was needed.
///
/// # 9: a session has a todo list
///
/// [`crate::SessionEvent::TodosUpdated`] is a new event, and a version-8 head
/// receiving one mid-session would fail to parse it — the version-4 argument
/// again, and the same ATTACH-time refusal. The event carries the whole list, in
/// the order the model wrote it; the pane that renders it also shows the repo's
/// own `TODO.md`, read-only, because an agent's plan and the operator's queue are
/// different lists and a head that conflated them would let one edit the other.
pub const PROTOCOL_VERSION: u32 = 9;

/// A `Caps.features` string: this head can render a question with model-provided
/// options, let a person attach a note to a choice, and let them type a free answer.
///
/// It rides on the existing `features` list rather than a new `Caps` field, because
/// that list exists for exactly this and adding a bool per affordance is how a
/// capability struct becomes a changelog.
///
/// A head that does **not** advertise it can still be sent a question — and the
/// honest thing then is that it will not answer, which becomes `not_run` (*nobody
/// answered*) rather than a default. That is the same rule `Caps::can_decide`
/// already states: a head that cannot answer must say so, or a question routed to
/// it waits for its deadline and then times out, *"which is a real answer given for
/// a fake reason."*
pub const FEATURE_QUESTION_ANSWERS: &str = "question_answers_v1";

/// What a head can do and what it wants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Caps {
    /// How many events the daemon will buffer for this head before demoting it to
    /// resync. The head asks, the daemon clamps. A head that renders slowly can ask
    /// for a bigger queue instead of resyncing constantly.
    pub queue: usize,
    /// Whether this head is willing to answer decisions. A read-only head must say
    /// so, or a question routed to it waits for its deadline and then times out —
    /// which is a real answer given for a fake reason.
    pub can_decide: bool,
    /// Free-form feature names, for forward compatibility.
    #[serde(default)]
    pub features: Vec<String>,
}

impl Default for Caps {
    fn default() -> Self {
        Caps {
            queue: 1024,
            can_decide: true,
            features: Vec::new(),
        }
    }
}

/// Head → daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "frame", rename_all = "snake_case")]
pub enum ClientFrame {
    /// `ATTACH {session_id, since_seq, identity, caps}` (§13.2).
    ///
    /// `since_seq = 0` means "I have seen nothing" and gets a snapshot. Any other
    /// value is a resume: the gap is delivered, or `RESYNC` is answered, and
    /// **resync is a normal outcome, never an error**.
    Attach {
        protocol_version: u32,
        session_id: String,
        since_seq: u64,
        /// `tui`, `remote`, `flowy`, `acp` — §13.4's table.
        kind: String,
        identity: String,
        #[serde(default)]
        caps: Caps,
    },
    /// The read mark. Sent **after** the head has written the batch out, never on
    /// receipt: a crash then costs a duplicate, never a silence (§13.2b).
    Ack(Ack),
    /// The head gave up on its own state and wants a fresh snapshot. Also what a
    /// head sends after it receives [`ServerFrame::Resync`].
    Resync,
    /// A user message. If a turn is running this is **queued as a follow-up user
    /// item**, not rejected (§13.2), and the queuing is announced.
    Prompt {
        client_request_id: String,
        expected_seq: u64,
        text: String,
    },
    /// Idempotent, issuable by any attached head, announced with the issuer.
    Interrupt {
        client_request_id: String,
        expected_seq: u64,
        reason: String,
    },
    /// Compact this session: one summary turn over the history as it stands,
    /// then the history is replaced by that summary through a transcript fork.
    ///
    /// The frame only *asks*; what happens next is the daemon's, and it is
    /// disclosed on the session's own log — the summary turn streams like any
    /// turn, and the forked transcript's first item says what replaced the
    /// history. Queued like a prompt (it runs a turn) and accepted on a stale
    /// `expected_seq` for the same reason.
    CompactSession {
        client_request_id: String,
        expected_seq: u64,
    },
    /// Answer an open **permission**: grant or deny, by option id.
    ///
    /// This is the adjudication half. A question's answer is
    /// [`ClientFrame::AnswerQuestion`], and they are two frames because they are two
    /// vocabularies with different consequences — a permission that goes wrong runs
    /// something, an answer that goes wrong is attributed to a person.
    Answer {
        client_request_id: String,
        req_id: String,
        option_id: String,
    },
    /// Answer an open **question**: a choice, a note on that choice, a typed reply,
    /// or a choice and a note together (§D10). Added at `PROTOCOL_VERSION` 5.
    ///
    /// There is no variant for *"not now"*. A head that wants to defer simply does
    /// not send this, and the question stays open until its deadline, at which point
    /// the tool reports `not_run` — *nobody answered*. Claude Code's *"chat later"*
    /// is the thing this absence is designed as: a deferral that travels as an
    /// answer is how a turn continues on an assumption nobody made.
    AnswerQuestion {
        client_request_id: String,
        req_id: String,
        answer: crate::question::QuestionAnswer,
    },
    /// What sessions does this daemon hold? Answered with [`ServerFrame::Sessions`].
    ///
    /// Read-only and unserialised: it does not go through the command queue,
    /// because a list is a question about the daemon rather than an act on a
    /// session, and making it wait behind a running turn would mean a head could
    /// not open the picker while the model was talking.
    ListSessions,
    /// Make a new session in this daemon.
    ///
    /// It does **not** switch to it — the head does that with [`ClientFrame::Switch`]
    /// once it has seen the id in the `Sessions` reply. Two frames rather than one
    /// because "create" and "go there" are separately useful: a head that wants a
    /// session ready for later should not have to leave the one it is in.
    NewSession {
        client_request_id: String,
        /// A human name, or empty. A title is set **once**; a daemon may name an
        /// unnamed session from the message that opened it, and after that only a
        /// deliberate rename changes it. What must not happen is a row that renames
        /// itself as the conversation goes on.
        title: String,
        /// The tree this session is about, from the head that asked.
        ///
        /// Empty means "wherever the daemon is", which is what a head that does not
        /// know sends. It is here because the daemon's own working directory is a
        /// fact about the daemon and not about the conversation: a `letibot --new`
        /// typed in `~/Projects/rano` against a daemon started in `~` used to seat
        /// the new session's read-only tools at `~`, and every path in it resolved,
        /// so the only symptom was answers about the wrong tree.
        workspace: String,
    },
    /// Bring a session that is **in the store but not in this daemon** back to life.
    ///
    /// Separate from [`ClientFrame::NewSession`] because the two differ in the one
    /// way that matters: this one **names** the session and `NewSession` deliberately
    /// does not. An id minted daemon-side is right for a new session (two heads
    /// racing to create "scratch" must not collide) and wrong for a resume, where the
    /// whole point is *that* conversation and no other.
    ///
    /// Idempotent. A session the daemon already holds is answered with the list and
    /// its own id, not refused: "resume the one I am already in" is a no-op the
    /// operator is allowed to ask for, and a refusal there would send `letibot
    /// --continue` down an error path on the most ordinary case there is.
    ///
    /// It does **not** switch to it, for the same reason `NewSession` does not: the
    /// head sends [`ClientFrame::Switch`] once it has the id.
    ResumeSession {
        client_request_id: String,
        session_id: String,
    },
    /// Name a session, or clear its name with an empty title.
    ///
    /// Carries a `session_id` rather than acting on the current one: a picker is
    /// where renaming is wanted, and in a picker the session you are looking at is
    /// usually not the session you are in.
    RenameSession {
        client_request_id: String,
        session_id: String,
        title: String,
    },
    /// Move this connection to another session.
    ///
    /// The head detaches from the session it is in and attaches to the named one,
    /// **on the same socket**, and is answered with a second `Hello`. Reconnecting
    /// would do as well and is what a first cut does; it costs the head its
    /// `client_request_id` sequence and, on a busy box, a window in which it is
    /// attached to neither — which is the window a mid-turn attach exists to close.
    Switch {
        session_id: String,
        /// As `Attach`: `0` takes a snapshot, anything else resumes. A head that
        /// is coming *back* to a session it was watching sends the seq it had.
        since_seq: u64,
    },
    /// A clean goodbye. **Not** required: TCP close is detach too, and detach is
    /// never abort (§13.2).
    Detach,
}

/// The read mark, as a type.
///
/// `seq` is *the last seq this head consumed*, whether or not it rendered it. It is
/// produced by [`crate::cursor::Batch::last_seq`] and there is deliberately no
/// other way to obtain one: a mark that advances only over what a filtering
/// consumer kept makes it reread its own output forever, which §13.2b calls the
/// single most reusable bug in flowy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ack {
    pub seq: u64,
    /// How many of those the head actually displayed.
    pub rendered: u64,
    /// How many it suppressed. *"Busy, and none of it was for me"* is a different
    /// fact from *"quiet"*, and this field is the difference. Not `Option`: a head
    /// that filters nothing reports zero.
    pub filtered: u64,
}

/// Daemon → head.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "frame", rename_all = "snake_case")]
pub enum ServerFrame {
    /// The answer to ATTACH. Always first, and **once per attachment**.
    ///
    /// It used to be "exactly once", which was true while a connection could only
    /// ever be in one session. [`ClientFrame::Switch`] makes a connection able to
    /// leave one and join another, and the frame that says *"you are now in this
    /// session, here is its state as of seq N"* is exactly this one — inventing a
    /// second frame that said the same thing would leave two attach paths to keep
    /// in step, which is how a resync path rots.
    Hello {
        protocol_version: u32,
        session_id: String,
        head_id: String,
        /// Events that fell off the back of the scrollback and this head will never
        /// see. **Present and zero** — the disclosure is the field, not its absence.
        dropped: u64,
        /// `Some` for a snapshot attach or a demoted resume; `None` for a resume
        /// that was served from the scrollback, whose gap follows as `Event` frames.
        ///
        /// Boxed because a snapshot is two orders of magnitude larger than every
        /// other frame, and an unboxed one makes *every* `ServerFrame` — including
        /// the `Event` that carries a three-character delta — 528 bytes. On the
        /// hot path there is one delta per token and one Hello per lifetime.
        snapshot: Option<Box<Snapshot>>,
        /// For a resume: the seq the gap starts after.
        resumed_from: Option<u64>,
        /// What the replay scrub stripped on the way here. The daemon's half of
        /// "report what was filtered": zero here on a live-only attach, nonzero
        /// whenever a decision had already settled.
        scrubbed: ScrubReport,
        /// **What this session is talking to** — `crates/ui/DESIGN.md` §4.4.
        ///
        /// `TurnStarted { model }` was the only one of these that ever reached a
        /// head, and only when a turn started, so a freshly attached head with no
        /// turn yet could say nothing at all and rendered `no turn yet`. The daemon
        /// has known all four since its own command line was parsed. Two daemons on
        /// one box serving two models on two ports is the normal case here, and a
        /// head that cannot say which one it is attached to is a head you have to
        /// guess about.
        ///
        /// Empty strings when the daemon's owner supplied none — never a plausible
        /// default, which would be a guess a head then quotes as a fact.
        wiring: SessionWiring,
        /// Every session this daemon holds, so a picker is populated by the attach
        /// itself and not by a second round trip. Includes the one just joined.
        sessions: Vec<SessionBrief>,
    },
    /// The answer to [`ClientFrame::ListSessions`] and to
    /// [`ClientFrame::NewSession`].
    ///
    /// `NewSession` is answered with the whole list rather than with the new id
    /// alone, because a head that has just created a session is a head about to
    /// draw a picker, and the list it would then ask for is this one.
    Sessions {
        sessions: Vec<SessionBrief>,
        /// The session this connection is in right now.
        current: String,
        /// The id `NewSession` created, when that is what this is answering.
        /// `None` for a plain list — present and null, not omitted.
        created: Option<String>,
    },
    /// One appended event, in seq order, with no gaps between consecutive frames.
    Event(Envelope),
    /// The head's queue overflowed, or its resume gap was too large. **Not an
    /// error.** The head resets to the enclosed snapshot and carries on from
    /// `snapshot.seq + 1`.
    Resync {
        reason: String,
        dropped: u64,
        snapshot: Box<Snapshot>,
        scrubbed: ScrubReport,
    },
    /// A command was serialized and applied.
    Accepted {
        client_request_id: String,
        /// The seq at which its effect is visible.
        seq: u64,
        note: String,
    },
    /// A command was refused. Both numbers travel so the head can say what it was
    /// looking at when it acted.
    Rejected {
        client_request_id: String,
        reason: String,
        expected_seq: u64,
        actual_seq: u64,
    },
    /// The daemon is going away. Detach is not abort; this is the case that is.
    Bye { reason: String },
}

/// Why a `Rejected` was sent, as a stable code a head can branch on.
pub const REJECT_STALE_SEQ: &str = "stale expected_seq";
/// The `note` on a prompt that was accepted with nothing unusual about it.
///
/// A constant rather than a literal in two places because a head has a reason to
/// recognise it: telling the operator who just pressed enter that their prompt was
/// queued is not news, while telling them that *another head's* prompt was queued
/// is the whole point of §13.2's announcement.
pub const NOTE_PROMPT_QUEUED: &str = "queued as a user item";
/// The `note` on a `/compact` that was accepted with nothing unusual about it.
///
/// Distinct from [`NOTE_PROMPT_QUEUED`] on purpose: a compaction that lands
/// behind an already-running turn happens **after** that turn, and the operator
/// who asked for it should be able to tell "queued behind the running turn" from
/// "queued as something the model will read" — the two notes are both a queue,
/// but they are not the same queue.
pub const NOTE_COMPACT_QUEUED: &str = "queued after the running turn";
pub const REJECT_UNKNOWN_DECISION: &str = "no such open decision";
pub const REJECT_READ_ONLY: &str = "this head declared can_decide: false";
/// A `Switch` or an `Attach` named a session this daemon does not hold.
///
/// Refused by name rather than answered with the default session: a typo that
/// seats you in somebody else's conversation looks exactly like a working attach
/// to an empty one, and you find out by prompting into it.
pub const REJECT_UNKNOWN_SESSION: &str = "no such session";
/// A `ResumeSession` named a session that is in neither the daemon nor the store.
///
/// Distinct from [`REJECT_UNKNOWN_SESSION`] on purpose: "this daemon does not hold
/// it" and "nothing anywhere has ever heard of it" send an operator to two different
/// places, and collapsing them is how a typo becomes half an hour with a database.
pub const REJECT_NOT_IN_STORE: &str = "no such session in the store";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::{SessionView, ViewBounds};

    #[test]
    fn dropped_is_present_and_zero_on_hello() {
        let f = ServerFrame::Hello {
            protocol_version: PROTOCOL_VERSION,
            session_id: "s".into(),
            head_id: "h1".into(),
            dropped: 0,
            snapshot: Some(Box::new(
                SessionView::new("s", ViewBounds::default()).snapshot(0, 0),
            )),
            resumed_from: None,
            scrubbed: ScrubReport::default(),
            wiring: SessionWiring::default(),
            sessions: Vec::new(),
        };
        let json = serde_json::to_string(&f).unwrap();
        assert!(json.contains(r#""dropped":0"#), "{json}");
        assert!(json.contains(r#""prompt_progress":0"#), "{json}");
        // The §4.4 fields are present and empty rather than absent, for the same
        // reason `dropped` is present and zero: "this daemon does not know what it
        // is talking to" and "this build does not report it" must not be the same
        // bytes.
        assert!(json.contains(r#""endpoint":"""#), "{json}");
        assert!(json.contains(r#""sessions":[]"#), "{json}");
    }

    #[test]
    fn no_frame_field_is_elided_when_zero_or_empty() {
        // The whole module is a disclosure surface. `skip_serializing_if` here would
        // make "nothing was filtered" and "this build does not report filtering"
        // identical on the wire.
        // Assembled at runtime so the assertion does not match itself, and applied
        // to attribute lines only so that prose about the rule is not the rule.
        let needle = format!("skip_serializing{}if", "_");
        let offender = include_str!("protocol.rs")
            .lines()
            .map(str::trim)
            .find(|l| l.starts_with("#[serde") && l.contains(&needle));
        assert_eq!(
            offender, None,
            "an absent field and a zero field must not look the same"
        );
    }

    #[test]
    fn every_frame_round_trips() {
        let ack = ClientFrame::Ack(Ack {
            seq: 7,
            rendered: 3,
            filtered: 4,
        });
        for f in [
            ClientFrame::Attach {
                protocol_version: PROTOCOL_VERSION,
                session_id: "s".into(),
                since_seq: 0,
                kind: "tui".into(),
                identity: "dead@lab2x1".into(),
                caps: Caps::default(),
            },
            ack,
            ClientFrame::Resync,
            ClientFrame::Prompt {
                client_request_id: "r1".into(),
                expected_seq: 12,
                text: "hello".into(),
            },
            ClientFrame::Interrupt {
                client_request_id: "r2".into(),
                expected_seq: 12,
                reason: "wrong file".into(),
            },
            ClientFrame::Answer {
                client_request_id: "r3".into(),
                req_id: "d1".into(),
                option_id: "allow_once".into(),
            },
            ClientFrame::ListSessions,
            ClientFrame::NewSession {
                client_request_id: "r4".into(),
                title: "the cache question".into(),
                workspace: "/home/dead/Projects/letibot".into(),
            },
            ClientFrame::ResumeSession {
                client_request_id: "r5".into(),
                session_id: "s-1788987496351498881".into(),
            },
            ClientFrame::RenameSession {
                client_request_id: "r6".into(),
                session_id: "s-2".into(),
                title: "the cache question".into(),
            },
            ClientFrame::Switch {
                session_id: "s-2".into(),
                since_seq: 0,
            },
            ClientFrame::Detach,
        ] {
            let s = serde_json::to_string(&f).unwrap();
            assert_eq!(f, serde_json::from_str::<ClientFrame>(&s).unwrap(), "{s}");
        }
    }

    #[test]
    fn a_sessions_frame_says_which_one_you_are_in() {
        // A list with no "you are here" is a list you cannot act on: every row
        // looks equally switchable and one of them is a no-op.
        let f = ServerFrame::Sessions {
            sessions: Vec::new(),
            current: "s-1".into(),
            created: None,
        };
        let json = serde_json::to_string(&f).unwrap();
        assert!(json.contains(r#""current":"s-1""#), "{json}");
        assert!(json.contains(r#""created":null"#), "{json}");
        assert_eq!(f, serde_json::from_str::<ServerFrame>(&json).unwrap());
    }
}
