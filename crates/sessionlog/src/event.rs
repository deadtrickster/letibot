//! §4.5's event enum, verbatim, plus the envelope that carries it.
//!
//! ```text
//! TurnStarted{turn_id, model, ledger_head}
//! PromptProgress{turn_id, total, cache, processed, time_ms}
//! Delta{turn_id, target: Text|Reasoning, text}
//! ToolCallProposed{turn_id, call_id, name, args_digest}
//! DecisionRequested{req_id, kind, call_id, summary, options, deadline, on_timeout}
//! DecisionAnswered{req_id, outcome, by, basis, late: bool}
//! ToolStarted / ToolProgress / ToolFinished{call_id, outcome, ...}
//! TurnFinished{turn_id, finish_reason, usage, timings}
//! TurnInterrupted{turn_id, reason, partial_kept: bool}
//! TranscriptAppended{item_id, kind, ledger_head}
//! TranscriptContent{item_id, item}          — not §4.5's; see the variant
//! HeadAttached / HeadDetached{head_id, kind, identity}
//! Warning{code, detail}
//! Explain{turn_id, plan}
//! ```
//!
//! Three notes on where this is *not* a transcription, and why.
//!
//! 1. **`ToolStarted` / `ToolProgress` are given by name only** in §4.5 (the `…`
//!    covers their fields). Their shapes were this crate's invention and W9 was
//!    the strand that would find out whether they were right. It has, by building
//!    the runtime that emits them, and the answer was: two of the three were
//!    short. `ToolStarted` gained `turn_id` and `access`; `ToolFinished` gained
//!    `turn_id`, the `inline`/`full` byte split, the spill locator and the repair
//!    count; `ToolProgress`'s free-text `note` was right as it stood. See
//!    `letibot_tools::events` for the argument in full.
//! 2. **`Explain{plan}` is a `serde_json::Value`.** W14 owes `ExplainPlan` as a
//!    type (§6.2 gives a rendered example and no field list). Typing it here would
//!    be inventing W14's contract from a screenshot.
//! 3. **`usage` carries `cached_tokens`.** §4.5 says "usage" without a field list;
//!    a head that cannot show cache reuse cannot show the one number this whole
//!    harness exists to move.
//!
//! **`Delta` carries only the increment.** Enforced the same way `letibot-turn`
//! enforces it: the variant has a `text` and there is nowhere to put a `full`.

use serde::{Deserialize, Serialize};

/// Which channel a delta belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeltaTarget {
    Text,
    Reasoning,
}

/// §5.6's prefill progress, as it reaches a head.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PromptProgress {
    pub total: u64,
    pub cache: u64,
    pub processed: u64,
    pub time_ms: u64,
}

/// Why generation stopped.
///
/// Shaped to match `letibot_turn::FinishReason` exactly, `Other` and its string
/// included. The engine's own note is the reason: *"a `finish_reason` nobody
/// recognises is exactly the thing that must not be silently normalised"* — and
/// normalising it on the way to a head is the same defect, one hop later.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Eos,
    Word,
    Length,
    Aborted,
    Other(String),
}

impl FinishReason {
    pub fn as_str(&self) -> &str {
        match self {
            FinishReason::Eos => "eos",
            FinishReason::Word => "word",
            FinishReason::Length => "length",
            FinishReason::Aborted => "aborted",
            FinishReason::Other(s) => s,
        }
    }
}

/// §4.5's `usage`.
///
/// `cached_tokens` is prompt tokens **reused**, not slot occupancy — the
/// distinction `turn_metrics` paid for (`timings.cache_n`, not `tokens_cached`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub cached_tokens: u64,
    pub predicted_tokens: u64,
}

impl Usage {
    /// **`f_sim`** — the fraction of *this* prompt that did not have to be
    /// prefilled. Deliberately **not** called `f_keep`.
    ///
    /// `usage` carries this turn's three numbers and nothing about the previous
    /// turn, so `f_keep` — `cached(N+1) / (prompt(N) + committed_generated(N))`,
    /// D11's settled C4 — is **not computable from this struct at all**. Naming
    /// this one `f_keep` is exactly the confusion T22 records: a head would then
    /// display a number that falls whenever the conversation grows and call it the
    /// cache metric with the 0.99 bar on it. `letibot_turn::TurnMetrics` has both,
    /// because it has the witness this struct does not.
    ///
    /// `None` for an empty prompt, for the same reason `TurnMetrics::f_sim` is:
    /// neither 0.0 nor 1.0 is true and both get averaged into a session figure.
    pub fn f_sim(&self) -> Option<f64> {
        if self.prompt_tokens == 0 {
            None
        } else {
            Some(self.cached_tokens as f64 / self.prompt_tokens as f64)
        }
    }
}

/// §4.5's `timings`.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Timings {
    pub prompt_ms: f64,
    pub predicted_ms: f64,
    pub wall_ms: u64,
}

/// ACP's vocabulary, as §13.4 requires: the adapter is then a mapping and not a
/// translation. `PermissionOption{option_id, label, kind}` with
/// `PermissionOptionKind in AllowOnce | AllowAlways | RejectOnce | RejectAlways`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionOption {
    pub option_id: String,
    pub label: String,
    pub kind: OptionKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OptionKind {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
}

/// What happens when nobody answers. Ours, not ACP's — ACP has no timeout at the
/// protocol layer at all (§13.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnTimeout {
    Deny,
    Allow,
    Ask,
}

/// How a decision settled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum DecisionOutcome {
    Selected { option_id: String },
    Cancelled,
    TimedOut,
}

/// Who settled it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decider {
    /// `human`, `policy`, `boundary`, `model`, `timeout` — §11's three adjudicators
    /// plus the two that are not adjudicators.
    pub kind: String,
    /// The identity, where there is one. A head id, a policy rule name, "".
    pub identity: String,
}

/// One event. `(session_id, seq, ts)` live on [`Envelope`], not here, because an
/// event that has not been appended yet has none of them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum SessionEvent {
    TurnStarted {
        turn_id: String,
        model: String,
        ledger_head: String,
    },
    /// Nothing surveyed reads this one, and §8.5 requires it to count as liveness.
    /// It is also the archetype of an *interactive* frame: see [`crate::scrub`].
    PromptProgress {
        turn_id: String,
        #[serde(flatten)]
        progress: PromptProgress,
    },
    Delta {
        turn_id: String,
        target: DeltaTarget,
        text: String,
    },
    ToolCallProposed {
        turn_id: String,
        call_id: String,
        name: String,
        args_digest: String,
    },
    DecisionRequested {
        req_id: String,
        kind: String,
        call_id: Option<String>,
        summary: String,
        options: Vec<DecisionOption>,
        /// Unix millis. `None` means §11.5's "wait forever", which is a policy a
        /// human head may choose and an automated one may not.
        deadline: Option<u64>,
        on_timeout: OnTimeout,
    },
    DecisionAnswered {
        req_id: String,
        outcome: DecisionOutcome,
        by: Decider,
        basis: String,
        /// The answer arrived after the deadline had already settled it. Recorded
        /// rather than dropped: "an answer that arrived too late" and "no answer"
        /// are different facts.
        late: bool,
    },
    /// **Shape revised by W9**, which is what T13.4 said would settle it: `turn_id`
    /// because every other turn-scoped event carries one and a head cannot
    /// attribute a call without it once §8.4's subagents run concurrently, and
    /// `access` because §8.1 clause 4 declares it in the schema and it is the fact
    /// that explains why a call did or did not stop for a decision.
    ToolStarted {
        turn_id: String,
        call_id: String,
        name: String,
        /// `read | write | exec | network`, from the tool's schema.
        access: String,
    },
    /// Interactive. Partial tool output, which a late head must never be replayed —
    /// see [`crate::scrub`].
    ///
    /// `note` survived contact with W9 unchanged: `grep` and `glob` do not know a
    /// total until they have finished walking, so a `done/total` pair would be a
    /// denominator invented for the display.
    ToolProgress {
        turn_id: String,
        call_id: String,
        note: String,
    },
    /// **Shape revised by W9.** `bytes` alone could not answer the question a
    /// spilling runtime raises — how much the model got versus how much there was —
    /// and the locator has to reach a head or "there is more" is a dead end.
    ToolFinished {
        turn_id: String,
        call_id: String,
        outcome: letibot_transcript::ToolOutcome,
        /// A digest, for the same reason `ToolCallProposed` carries one: the payload
        /// is in the transcript, and an event fans out to every head.
        payload_digest: String,
        /// What the model received.
        inline_bytes: u64,
        /// What the tool produced. Larger than `inline_bytes` exactly when the
        /// output spilled (§8.3).
        full_bytes: u64,
        /// The spill locator, when there is one: a head can offer the rest.
        spill: Option<String>,
        /// How many of §8.1 clause 2's repairs the call needed. A head that cannot
        /// see this cannot see a model steadily emitting malformed calls.
        repairs: u32,
    },
    TurnFinished {
        turn_id: String,
        finish_reason: FinishReason,
        usage: Usage,
        timings: Timings,
    },
    TurnInterrupted {
        turn_id: String,
        reason: String,
        partial_kept: bool,
    },
    TranscriptAppended {
        item_id: String,
        kind: String,
        ledger_head: String,
    },
    /// **The second addition to §4.5, and it is T13.1 being paid rather than
    /// deferred.**
    ///
    /// §4.5's `TranscriptAppended` announces a row and carries no body, and until
    /// now the body reached a head *only* through the snapshot
    /// ([`crate::view::SessionView::record_item`]). That works for a head that
    /// attaches after the row exists and fails permanently for one that was already
    /// attached: it is told a row landed, no later frame ever carries the content,
    /// and its placeholder is not a loading state — it is the final state. Measured
    /// on a live session: the operator's own prompt renders as
    /// `[user … — content not loaded]` forever.
    ///
    /// T13.1 also argues the log should be a **sufficient record of a session**, and
    /// it is not one today: `letibot-tui --replay recorded.jsonl` shows a placeholder
    /// for every row, because the only copy of the content was in a snapshot that
    /// was never written down. Assistant text is *nearly* recoverable from `Delta`,
    /// but the item boundaries are not, and a `ToolResult` payload is not on the
    /// wire at all — `ToolFinished` carries a digest by design.
    ///
    /// So the content goes in the log. **The right shape is a body on
    /// `TranscriptAppended` itself** — one row, one event, and no
    /// announced-but-empty state for anyone to render. That shape needs
    /// `letibot_turn::TurnEvent::TranscriptAppended` to carry the item too, because
    /// the engine is what emits the announcement and the daemon does not hold the
    /// item until the engine's call returns. Until that seam is widened, this
    /// variant carries the body a moment later, on the same log, and a head handles
    /// it with the same code it will use afterwards: fill the row this names.
    /// Deleting this variant is then the whole of the migration.
    ///
    /// Boxed for the reason `Hello.snapshot` is: a `TranscriptItem` is two orders
    /// of magnitude larger than a `Delta`, and an unboxed one would make every
    /// event on the hot path pay for it.
    TranscriptContent {
        item_id: String,
        item: Box<letibot_transcript::TranscriptItem>,
    },
    HeadAttached {
        head_id: String,
        kind: String,
        identity: String,
    },
    HeadDetached {
        head_id: String,
        kind: String,
        identity: String,
    },
    /// §18's post-flight assertions land here, and so do §8.5's guards.
    Warning {
        code: String,
        detail: String,
    },
    /// §6. `plan` is untyped until W14 says what `ExplainPlan` is.
    Explain {
        turn_id: String,
        plan: serde_json::Value,
    },

    /// **The one addition to §4.5, and it is required by §13.2.**
    ///
    /// > "a second head's prompt is **queued as a follow-up user item** rather than
    /// > rejected … and the queuing is **announced as an event so both heads see it
    /// > and who did it**. Interrupt/abort is idempotent, any attached head may
    /// > issue it, and it is **announced with the issuer's identity**."
    ///
    /// Nothing in §4.5 can carry that. `TranscriptAppended` has no issuer;
    /// `HeadAttached` has an identity but no command; `Warning` is for §18's
    /// assertions and using it here would make a normal multi-head action look like
    /// a defect. Two heads sharing a session and being unable to see which of them
    /// interrupted the turn is precisely the failure §13.2 is written against, so
    /// the honest fix is a variant rather than an abuse of one.
    ///
    /// Written up in the W7 report as a proposed amendment to §4.5, not as a silent
    /// widening.
    CommandIssued {
        head_id: String,
        identity: String,
        /// `prompt`, `interrupt`, `answer`.
        command: String,
        client_request_id: String,
        /// What the daemon did with it, in the words the issuing head was given.
        note: String,
    },
}

impl SessionEvent {
    /// The variant name, for counters and for a head's status line.
    pub fn kind(&self) -> &'static str {
        match self {
            SessionEvent::TurnStarted { .. } => "TurnStarted",
            SessionEvent::PromptProgress { .. } => "PromptProgress",
            SessionEvent::Delta { .. } => "Delta",
            SessionEvent::ToolCallProposed { .. } => "ToolCallProposed",
            SessionEvent::DecisionRequested { .. } => "DecisionRequested",
            SessionEvent::DecisionAnswered { .. } => "DecisionAnswered",
            SessionEvent::ToolStarted { .. } => "ToolStarted",
            SessionEvent::ToolProgress { .. } => "ToolProgress",
            SessionEvent::ToolFinished { .. } => "ToolFinished",
            SessionEvent::TurnFinished { .. } => "TurnFinished",
            SessionEvent::TurnInterrupted { .. } => "TurnInterrupted",
            SessionEvent::TranscriptAppended { .. } => "TranscriptAppended",
            SessionEvent::TranscriptContent { .. } => "TranscriptContent",
            SessionEvent::HeadAttached { .. } => "HeadAttached",
            SessionEvent::HeadDetached { .. } => "HeadDetached",
            SessionEvent::Warning { .. } => "Warning",
            SessionEvent::Explain { .. } => "Explain",
            SessionEvent::CommandIssued { .. } => "CommandIssued",
        }
    }
}

/// An event with its `(session_id, seq, ts)`.
///
/// `seq` is monotonic and gap-free **per session**, and it is the only thing a
/// head's read mark or a command's `expected_seq` ever refers to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub session_id: String,
    pub seq: u64,
    /// Unix millis.
    pub ts: u64,
    #[serde(flatten)]
    pub event: SessionEvent,
}

/// Unix millis now. Fails backwards to 0 rather than panicking: a clock before the
/// epoch is a broken clock, not a reason to lose a session.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_delta_has_nowhere_to_put_accumulated_text() {
        // Same assertion `letibot-turn` carries, at the other end of the seam:
        // adding a `full` field has to delete a test that says why not.
        let json = serde_json::to_string(&SessionEvent::Delta {
            turn_id: "t1".into(),
            target: DeltaTarget::Text,
            text: "abc".into(),
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"event":"delta","turn_id":"t1","target":"text","text":"abc"}"#
        );
    }

    #[test]
    fn every_variant_round_trips_through_the_wire() {
        for e in crate::testing::one_of_each() {
            let s = serde_json::to_string(&e).unwrap();
            let back: SessionEvent = serde_json::from_str(&s).unwrap();
            assert_eq!(e, back, "{s}");
        }
    }

    #[test]
    fn f_sim_is_absent_rather_than_invented_for_an_empty_prompt() {
        assert_eq!(Usage::default().f_sim(), None);
        let u = Usage {
            prompt_tokens: 100,
            cached_tokens: 90,
            predicted_tokens: 5,
        };
        assert_eq!(u.f_sim(), Some(0.9));
    }
}
