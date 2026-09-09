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
pub const PROTOCOL_VERSION: u32 = 3;

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
    /// Answer an open decision.
    Answer {
        client_request_id: String,
        req_id: String,
        option_id: String,
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
        /// A human name, or empty. Never derived from a prompt here: a title
        /// guessed from content is a title that changes under you.
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
pub const REJECT_UNKNOWN_DECISION: &str = "no such open decision";
pub const REJECT_READ_ONLY: &str = "this head declared can_decide: false";
/// A `Switch` or an `Attach` named a session this daemon does not hold.
///
/// Refused by name rather than answered with the default session: a typo that
/// seats you in somebody else's conversation looks exactly like a working attach
/// to an empty one, and you find out by prompting into it.
pub const REJECT_UNKNOWN_SESSION: &str = "no such session";

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
