//! **An item, its claim, and the two facts a lease is made of.**
//!
//! Nothing here reads a clock, a store or a socket: an [`Item`] is a value, and every time in it
//! was handed to the room by its caller (see [`crate::Room`]'s note on time).
//!
//! # The lifecycle, and the one move that is not on the arrow
//!
//! ```text
//! open ──claim──▶ claimed ──done──▶ done
//!   ▲                │
//!   └──abandoned◀────┘        (a claim whose beat stopped, whose session went, or that its
//!                              holder gave up on purpose)
//! ```
//!
//! **`abandoned` is not a terminal state.** The item goes back to the queue — it is claimable
//! again, at the head of its tag, with its count up — and the state names the last thing that
//! happened to it so that a reader cannot mistake a re-queued item for one nobody ever tried.
//! [`Item::claimable`] is the one predicate a reader should use to decide whether an item is up
//! for grabs; `state == Open` is not it.
//!
//! Two states end an item for good: [`State::Done`] (the work was done) and [`State::Dropped`]
//! (a ruling took it out — see [`crate::Question`]). Neither is claimable.

use std::fmt;

use serde::{Deserialize, Serialize};

/// **Who is speaking** — a session id, and the parent it speaks through if it is a subagent.
///
/// The room is for **top-level sessions only**: a subagent's words and claims go through the
/// session that spawned it, so there is one voice per project and no fan-out of identities. The
/// rule is flowy's as well, and the operator's own, and it is enforced at every verb rather than
/// remembered ([`crate::Refused::Subagent`]).
///
/// `parent` is the daemon's answer, not the room's: the room never sees `sessions.db`. The daemon
/// reads `session.parent_session_id` when it opens the connection and puts it here; a daemon that
/// lied about its own sessions would be lying to itself, and this is one host and one user.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Speaker {
    pub session: String,
    /// `Some(parent)` for a subagent, `None` for a top-level session.
    pub parent: Option<String>,
}

impl Speaker {
    /// A session that may speak in the room.
    pub fn top_level(session: impl Into<String>) -> Self {
        Self {
            session: session.into(),
            parent: None,
        }
    }

    /// A subagent — every verb it asks is refused by name.
    pub fn subagent(session: impl Into<String>, parent: impl Into<String>) -> Self {
        Self {
            session: session.into(),
            parent: Some(parent.into()),
        }
    }

    pub fn is_top_level(&self) -> bool {
        self.parent.is_none()
    }
}

impl fmt::Display for Speaker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.parent {
            None => write!(f, "`{}`", self.session),
            Some(p) => write!(f, "`{}` (a subagent of `{p}`)", self.session),
        }
    }
}

/// **One row of a tag's queue.**
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    /// Minted by the room, in order, from 1. Stable for the item's whole life, including across
    /// an abandonment — that is what makes "the same item came back" sayable.
    pub id: u64,
    /// Normalised: trimmed and lowercased by [`crate::Room::post`], so `RAM` and `ram` are one
    /// queue. The tag is the unit of contention — a tag is a lock — so two spellings of one
    /// resource must not be two queues.
    pub tag: String,
    /// What is needed, in the poster's words: `8 GB, ~10 min`, `the staging database`, `a
    /// reviewer for #412`. The room never parses it.
    pub body: String,
    pub posted_by: String,
    pub posted_ms: u64,
    pub state: State,
    /// **Every abandonment, oldest first** — the count is `len()`, and the reasons are what the
    /// tie-breaker is shown when the count reaches the threshold.
    #[serde(default)]
    pub abandonments: Vec<Abandonment>,
    /// The open question about this item, if there is one. A question freezes the item: it is not
    /// claimable until it is answered ([`crate::Room::rule`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question: Option<u64>,
}

/// **Where an item is** — see the module header for the arrow and for why `abandoned` is not
/// terminal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum State {
    Open,
    Claimed {
        claim: Claim,
    },
    Done {
        by: String,
        ms: u64,
    },
    Abandoned {
        last: Abandonment,
    },
    /// Taken out of the queue by a ruling, with the reason on it. Not `done`: nobody did the work.
    Dropped {
        by: String,
        why: String,
        ms: u64,
    },
}

/// **A claim is a lease with a beat** — `claimed_by`, `claim_ms`, `beat_ms`, the same discipline as
/// the merge queue's `queue_lease` and the hosts' `host_lease`.
///
/// `claim_ms` is when it was taken, and it never moves (a repeated claim is the same claim).
/// `beat_ms` is when the holder last said it is still working on it, and it is the whole of the
/// staleness test: [`crate::Room::sweep`] abandons a claim whose beat is older than the lease.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    pub by: String,
    pub claim_ms: u64,
    pub beat_ms: u64,
    /// What the holder says it is waiting on. The shape a todo reminder needs — the room answers
    /// *what is this session holding, and what is it waiting on* — and not a delivery mechanism.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waits_on: Option<Wait>,
}

/// **What a holder is waiting on**, in its own words, and since when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Wait {
    pub text: String,
    pub since_ms: u64,
}

/// **One time an item lost its claimant** — who, when, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Abandonment {
    pub by: String,
    pub claim_ms: u64,
    /// The last beat the room saw. For [`Why::BeatStale`] this is the whole of the evidence.
    pub beat_ms: u64,
    /// When the room marked it abandoned.
    pub ms: u64,
    pub why: Why,
}

/// **Why an item went back to the queue.** Three ways, and they are different facts:
///
/// * [`Why::BeatStale`] — nobody said this session is still working on it. The session may be
///   alive and merely not working (a parked turn, a model that stopped calling `beat`), which is
///   exactly the case the operator's *"so they don't occupy it for ever"* is about.
/// * [`Why::SessionGone`] — a positive fact from the daemon: the session exited, its daemon was
///   restarted, or the socket that owns it closed. This one is known at once, so it does not wait
///   out the lease.
/// * [`Why::Released`] — the holder gave it up on purpose. Still an abandonment, and still
///   counted: an item everybody gives up on is a problem whether or not each gave up politely.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "why", rename_all = "snake_case")]
pub enum Why {
    BeatStale { age_ms: u64 },
    SessionGone { says: String },
    Released { says: String },
}

impl Item {
    /// **May anybody claim this now?** The one predicate a reader should use. `false` for a
    /// claimed item, a finished one, a dropped one, and one whose question is open.
    pub fn claimable(&self) -> bool {
        self.question.is_none() && matches!(self.state, State::Open | State::Abandoned { .. })
    }

    /// Still in the queue at all — claimed or not, but not finished and not dropped.
    pub fn live(&self) -> bool {
        !matches!(self.state, State::Done { .. } | State::Dropped { .. })
    }

    pub fn claim(&self) -> Option<&Claim> {
        match &self.state {
            State::Claimed { claim } => Some(claim),
            _ => None,
        }
    }

    pub fn holder(&self) -> Option<&str> {
        self.claim().map(|c| c.by.as_str())
    }

    /// How many claimants this item has lost — the number the threshold is measured on.
    pub fn abandonments(&self) -> u32 {
        self.abandonments.len() as u32
    }

    /// The last abandonment, if there is one.
    pub fn last_abandonment(&self) -> Option<&Abandonment> {
        self.abandonments.last()
    }

    /// The state's name, for a sentence — `open`, `claimed`, `done`, `abandoned`, `dropped`.
    pub fn state_name(&self) -> &'static str {
        match self.state {
            State::Open => "open",
            State::Claimed { .. } => "claimed",
            State::Done { .. } => "done",
            State::Abandoned { .. } => "abandoned",
            State::Dropped { .. } => "dropped",
        }
    }

    /// `#7` — how an item is named in a sentence.
    pub fn name(&self) -> String {
        format!("#{}", self.id)
    }
}

impl fmt::Display for Why {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Why::BeatStale { age_ms } => write!(
                f,
                "its beat stopped — the last one was {} old",
                duration(*age_ms)
            ),
            Why::SessionGone { says } => write!(f, "{says}"),
            Why::Released { says } => write!(f, "the holder gave it up: {says}"),
        }
    }
}

/// **A duration a person reads** — `9s`, `1m 36s`, `2h 5m`. Sentences about leases are read at a
/// glance, and `96000` is not a glance.
pub fn duration(ms: u64) -> String {
    if ms < 1_000 {
        return format!("{ms}ms");
    }
    let s = ms / 1_000;
    if s < 60 {
        return format!("{s}s");
    }
    let (m, rs) = (s / 60, s % 60);
    if m < 60 {
        return if rs == 0 {
            format!("{m}m")
        } else {
            format!("{m}m {rs}s")
        };
    }
    let (h, rm) = (m / 60, m % 60);
    if rm == 0 {
        format!("{h}h")
    } else {
        format!("{h}h {rm}m")
    }
}
