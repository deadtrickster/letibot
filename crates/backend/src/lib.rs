//! The seam between the turn engine and whatever actually runs the model.
//!
//! # Why this exists before there is a second implementation
//!
//! Today there is one backend: a llama.cpp server we control, which we render for
//! and submit token ids to. A token-metered provider API is a later milestone
//! (`TODO.md` T8) — but the seam is reserved now, because retrofitting it means
//! touching the turn engine, compaction, EXPLAIN and every metric. That is the same
//! lesson `max_inline_bytes` taught one level down: a decision expressed as an
//! interface costs nothing today.
//!
//! # The three modes, and why only one of them is hard
//!
//! | mode | who renders | submit | a token costs |
//! |---|---|---|---|
//! | local llama.cpp | us | token ids | wall clock |
//! | our own cloud compute | us | token ids | wall clock (hardware time) |
//! | token-metered API | **the provider** | `messages` | **money** |
//!
//! The middle mode is architecturally the first: we still run the server, so
//! self-rendering, the token ledger and the structural prefix invariant all survive.
//! Only the metered API is a different shape.
//!
//! # The rule this crate exists to enforce
//!
//! **A backend must declare what it cannot guarantee, and the tests must skip
//! loudly rather than pass vacuously.** A prefix-stability suite that silently
//! degrades to "the provider's cache seemed fine" against an API, while claiming
//! the same green tick as the structural check, is precisely the failure this
//! project was started to remove. `PrefixGuarantee` is not documentation; it is the
//! input to that decision.
//!
//! This crate deliberately depends on `letibot-transcript` and nothing else — no
//! dialect, no tokenizer, no FFI. A provider backend must not have to link the
//! rendering stack it will never use.

use letibot_transcript::TranscriptItem;

/// What a backend can actually promise. Every field is a fact about the backend,
/// not a preference, and every one of them changes what a test may assert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackendCaps {
    /// We render the prompt and submit tokens. False for any provider API.
    pub renders_locally: bool,
    /// `/completion` with a token array. False means `messages` only.
    pub accepts_token_ids: bool,
    /// Per-request cache accounting good enough for `EXPLAIN` to attribute a
    /// prefill. A provider reporting only `cached_tokens` is `Coarse`.
    pub cache_reporting: CacheReporting,
    /// What a token costs, which decides whether compaction is a latency
    /// optimisation or a spend control.
    pub meter: Meter,
    pub prefix: PrefixGuarantee,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheReporting {
    /// Per-stage counters: which prefix matched, where it diverged, what that cost.
    /// Only obtainable from a server we run.
    PerStage,
    /// A single `cached_tokens`-shaped number.
    Coarse,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Meter {
    /// Tokens cost wall clock. Local, and our own cloud compute — the operator's
    /// point is that renting the hardware makes these the same case, because you
    /// pay for time either way.
    WallClock,
    /// Tokens cost money. Compaction stops being a latency optimisation.
    Metered,
}

/// How strongly the append-only prompt invariant holds.
///
/// The plan's claim (§4.3) is that a prefix violation is *blocked*: request
/// N+1 is the same append-only token region read to a greater length, so there is
/// no rewrite operation to call. That claim is a property of submitting token ids
/// over memory we own. It does not survive a `messages` API, and pretending it does
/// would be worse than not having it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PrefixGuarantee {
    /// Violation is unrepresentable. Assert the region property directly.
    Structural,
    /// We send messages; the provider renders. We can only check afterwards that
    /// its cache behaved as though the prefix was stable — a weaker, statistical
    /// claim about the past.
    Asserted,
    /// Nothing to check. A suite reaching this must **skip and say so**.
    None,
}

/// What a turn cost, in the unit that backend actually charges.
///
/// Kept as one type with an explicit meter rather than two, so a report cannot
/// silently add wall-clock milliseconds to dollars.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TurnCost {
    pub meter: Meter,
    pub prompt_tokens: u64,
    pub cached_tokens: u64,
    pub generated_tokens: u64,
    pub wall_ms: u64,
    /// Only meaningful when `meter == Metered`. `None` under `WallClock` — not
    /// zero, because zero is a number somebody will sum.
    pub micros_usd: Option<u64>,
}

/// One turn's worth of work, expressed in terms both a local and a remote backend
/// can realise.
///
/// Note it carries the **transcript**, not a rendered prompt. Realisation is the
/// backend's job: the local one renders, tokenizes and appends to the ledger; a
/// provider one converts to that provider's message shape. Handing a rendered
/// string across this seam would force every backend through the rendering stack.
#[derive(Debug, Clone)]
pub struct TurnRequest<'a> {
    pub system: &'a str,
    pub tools_json: &'a [String],
    pub items: &'a [TranscriptItem],
    /// **Items this request carries and the transcript does not.**
    ///
    /// Rendered after everything `items` holds and committed nowhere — the
    /// session's log, and therefore the next request's cached prefix, is exactly
    /// `items`. The one caller today is the standing-notes offer
    /// (`letibot_harnessd::notes_offer`), and the reason it must be here rather
    /// than in the log is that module's whole subject: a row cannot be un-said, an
    /// assembly can be recomposed. An offer the round does not take leaves no
    /// trace because it was never anything but this slice.
    ///
    /// **Not a `System` item.** MEASURED against the live API (`crate::messages`'s
    /// `trailing_system_becomes_user`): a request that ends on a `system` message
    /// while carrying `tools` is refused with *"The `reasoning_content` in the
    /// thinking mode must be passed back to the API"*, which is why a trailing
    /// item is rendered as the user-side row it is.
    pub tail: &'a [TranscriptItem],
    pub max_output_tokens: Option<u32>,
}

pub trait Backend {
    fn caps(&self) -> BackendCaps;

    /// Human-readable, for `EXPLAIN` and for test skip messages. A skip that does
    /// not say which backend caused it wastes the reader's time.
    fn name(&self) -> &str;
}

/// One piece of a streamed answer, as a `messages` API hands it over. The
/// turn engine forwards these to its event sink the way it forwards a local
/// server's token deltas, so a head draws a cloud turn and a local one alike.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delta {
    Text(String),
    /// `reasoning_content` (DeepSeek, GLM, Grok) — the thinking, kept apart from
    /// the answer by the API rather than by a parser.
    Reasoning(String),
    /// A fragment of a tool call. The first fragment for an `index` carries the
    /// id and the name; every fragment appends to the arguments.
    ToolCall {
        index: usize,
        id: Option<String>,
        name: Option<String>,
        arguments: String,
    },
}

/// Why the answer stopped, in the API's own terms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finish {
    Stop,
    Length,
    ToolCalls,
    /// The API said something this crate does not model; kept verbatim.
    Other(String),
}

/// A finished answer: everything the deltas built, plus what it cost.
#[derive(Debug, Clone, PartialEq)]
pub struct Completion {
    pub text: String,
    pub reasoning: String,
    pub tool_calls: Vec<letibot_transcript::ToolCall>,
    pub finish: Finish,
    pub cost: TurnCost,
    /// The provider's `usage` object verbatim, for a metrics row somebody reads
    /// later without this crate's model of it.
    pub raw_usage: Option<String>,
}

/// Whether to keep streaming.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamFlow {
    Continue,
    /// Close the connection now — an urgent steering message, a guard.
    Stop,
}

#[derive(Debug)]
pub enum BackendError {
    /// The provider could not be reached, or the connection died mid-answer.
    Unreachable(String),
    /// The provider answered and said no: a bad key, a bad model, a quota.
    Refused { status: u16, body: String },
    /// 2xx, but not the shape expected.
    Malformed(String),
    /// The caller asked to stop; nothing is recorded as a finished answer.
    Aborted,
}

impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BackendError::Unreachable(m) => write!(f, "provider unreachable: {m}"),
            BackendError::Refused { status, body } => {
                write!(f, "provider refused ({status}): {body}")
            }
            BackendError::Malformed(m) => write!(f, "provider answered something unexpected: {m}"),
            BackendError::Aborted => write!(f, "the turn was aborted by its caller"),
        }
    }
}

impl std::error::Error for BackendError {}

/// A backend that takes the **transcript** and answers with a completion — a
/// `messages` API. D10: realisation is the backend's job; the transcript
/// crosses this seam, never a rendered prompt and never token ids.
///
/// The turn engine's `run_turn_messages` drives this and keeps its token
/// ledger as the local **record** of the conversation (tokenised with the
/// session's own vocabulary, which is an encoding for the store and the
/// compaction arithmetic, and is never sent anywhere). The invariant suites
/// skip loudly: `caps().skip_reason()` says the structural prefix check did
/// not run.
pub trait MessagesBackend: Send + Sync {
    fn caps(&self) -> BackendCaps;

    /// `deepseek`, `glm`, `grok` — the preset's name.
    fn name(&self) -> &str;

    /// The model id sent on the wire.
    fn model(&self) -> &str;

    /// **`HOST:PORT` of the thing this backend actually talks to**, for a message that
    /// has to name it.
    ///
    /// A warning about a failed round used to name the LOCAL endpoint
    /// (`cfg.endpoint.authority()`) on a path that also serves cloud turns — so a
    /// resolver failure against `api.deepseek.com` was reported as a problem with
    /// `127.0.0.1:8080`, and the operator went to look at a `llama-server` that was
    /// answering. MEASURED 2026-10-01.
    ///
    /// So the host comes from the backend being used rather than from the daemon's
    /// configuration, and it is read from the same URL the request is posted to — not
    /// stored beside it, where it could drift.
    fn authority(&self) -> String;

    /// One completion over the request, streaming deltas to `on_delta` as they
    /// arrive. `StreamFlow::Stop` from the callback closes the connection and
    /// the result is `Err(BackendError::Aborted)`.
    fn complete(
        &self,
        req: &TurnRequest<'_>,
        on_delta: &mut dyn FnMut(&Delta) -> StreamFlow,
    ) -> Result<Completion, BackendError>;
}

impl BackendCaps {
    /// The shape of a llama.cpp server we run ourselves — local or rented, the
    /// operator's distinction being that rented compute is still billed by time.
    pub const OWN_SERVER: BackendCaps = BackendCaps {
        renders_locally: true,
        accepts_token_ids: true,
        cache_reporting: CacheReporting::PerStage,
        meter: Meter::WallClock,
        prefix: PrefixGuarantee::Structural,
    };

    /// The shape of a token-metered provider API. Not implemented; reserved so the
    /// tests that must weaken against it can be written now.
    pub const METERED_API: BackendCaps = BackendCaps {
        renders_locally: false,
        accepts_token_ids: false,
        cache_reporting: CacheReporting::Coarse,
        meter: Meter::Metered,
        prefix: PrefixGuarantee::Asserted,
    };

    /// Whether an invariant suite may assert the structural prefix property, or
    /// must weaken or skip. Call this instead of testing fields ad hoc, so the
    /// decision is made in one place.
    pub fn may_assert_structural_prefix(&self) -> bool {
        self.prefix == PrefixGuarantee::Structural
    }

    /// The message a suite must print when it cannot check what it was written to
    /// check. Returning a message rather than a bool is deliberate: it makes the
    /// silent-skip harder to write than the loud one.
    pub fn skip_reason(&self, what: &str) -> Option<String> {
        match self.prefix {
            PrefixGuarantee::Structural => None,
            g => Some(format!(
                "SKIPPED {what}: this backend guarantees {g:?}, not Structural. \
                 The check did not run and this is not a pass."
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_metered_api_cannot_claim_the_structural_invariant() {
        assert!(BackendCaps::OWN_SERVER.may_assert_structural_prefix());
        assert!(!BackendCaps::METERED_API.may_assert_structural_prefix());
    }

    #[test]
    fn skipping_produces_a_reason_that_denies_it_is_a_pass() {
        assert!(BackendCaps::OWN_SERVER.skip_reason("I1").is_none());
        let r = BackendCaps::METERED_API.skip_reason("I1").unwrap();
        assert!(r.contains("did not run"), "{r}");
        assert!(r.contains("not a pass"), "{r}");
    }

    #[test]
    fn wall_clock_cost_carries_no_money_figure() {
        // zero dollars is a number somebody will sum into a total; absent is not
        let c = TurnCost {
            meter: Meter::WallClock,
            prompt_tokens: 10,
            cached_tokens: 8,
            generated_tokens: 4,
            wall_ms: 120,
            micros_usd: None,
        };
        assert!(c.micros_usd.is_none());
    }

    #[test]
    fn guarantees_are_ordered_so_a_suite_can_compare_them() {
        assert!(PrefixGuarantee::Structural < PrefixGuarantee::Asserted);
        assert!(PrefixGuarantee::Asserted < PrefixGuarantee::None);
    }
}
