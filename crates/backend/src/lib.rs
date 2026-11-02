//! The seam between the turn engine and whatever actually runs the model.
//!
//! A backend states what it can promise in [`BackendCaps`], and that statement is
//! what an invariant suite reads to decide whether it may assert the structural
//! prefix property or must skip and say so. A skip carries a sentence rather than a
//! bool because a silent degradation that reads as a green tick is the failure this
//! seam exists to prevent.
//!
//! Only `letibot-transcript` is a dependency: a provider backend must not have to
//! link the rendering stack it will never use.

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
    /// Tokens cost wall clock: local work, or hardware rented by the hour.
    WallClock,
    /// Tokens cost money. Compaction stops being a latency optimisation.
    Metered,
}

/// How strongly the append-only prompt invariant holds.
///
/// The invariant is a property of submitting token ids over memory we own: request
/// N+1 reads the same region further, so there is no rewrite to represent. A
/// `messages` API cannot offer that, hence this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PrefixGuarantee {
    /// Violation is unrepresentable. Assert the region property directly.
    Structural,
    /// We send messages and the provider renders, so the check is afterwards: did
    /// its cache behave as though the prefix was stable?
    Asserted,
    /// Nothing to check. A suite reaching this must skip and say so.
    None,
}

/// What a turn cost, in the unit that backend actually charges.
///
/// One type with an explicit meter rather than two, so a report cannot silently
/// add wall-clock milliseconds to dollars.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TurnCost {
    pub meter: Meter,
    pub prompt_tokens: u64,
    pub cached_tokens: u64,
    pub generated_tokens: u64,
    pub wall_ms: u64,
    /// Only meaningful when `meter == Metered`. `None` under `WallClock` - not
    /// zero, because zero is a number somebody will sum.
    pub micros_usd: Option<u64>,
}

/// One turn's worth of work, expressed in terms both a local and a remote backend
/// can realise.
///
/// It carries the transcript, not a rendered prompt: realisation is the backend's
/// job, and a rendered string across this seam would force every backend through
/// the rendering stack.
#[derive(Debug, Clone)]
pub struct TurnRequest<'a> {
    pub system: &'a str,
    pub tools_json: &'a [String],
    pub items: &'a [TranscriptItem],
    /// Items this request carries and the transcript does not.
    ///
    /// Rendered after everything `items` holds and committed nowhere, so the cached
    /// prefix stays exactly `items`. An assembly can be recomposed where a row
    /// cannot be un-said, so an offer the round declines leaves no trace.
    ///
    /// Not a `System` item: the API refuses a request that ends on a `system`
    /// message while carrying tools, so a trailing item renders on the user side.
    pub tail: &'a [TranscriptItem],
    pub max_output_tokens: Option<u32>,
}

pub trait Backend {
    fn caps(&self) -> BackendCaps;

    /// Human-readable, for `EXPLAIN` and for test skip messages. A skip that does
    /// not say which backend caused it wastes the reader's time.
    fn name(&self) -> &str;
}

/// One piece of a streamed answer, as a `messages` API hands it over.
///
/// The turn engine forwards these to its event sink the way it forwards a local
/// server's token deltas, so a head draws a cloud turn and a local one alike.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delta {
    Text(String),
    /// `reasoning_content` (DeepSeek, GLM, Grok) - the thinking, kept apart from
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
    /// Close the connection now - an urgent steering message, a guard.
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

/// A backend that takes the transcript and answers with a completion: a `messages`
/// API.
///
/// Realisation stays the backend's job, so the transcript crosses this seam and
/// never a rendered prompt or token ids. The turn engine keeps its token ledger as
/// the local record of the conversation, encoded with the session's own vocabulary,
/// which is never sent anywhere.
pub trait MessagesBackend: Send + Sync {
    fn caps(&self) -> BackendCaps;

    /// `deepseek`, `glm`, `grok` - the preset's name.
    fn name(&self) -> &str;

    /// The model id sent on the wire.
    fn model(&self) -> &str;

    /// `HOST:PORT` of the thing this backend actually talks to, for a message that
    /// has to name it.
    ///
    /// Read from the URL the request is posted to, not stored beside it where it
    /// could drift: a warning that names the wrong host sends the reader to look at
    /// a server that was answering.
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
    /// The shape of a llama.cpp server we run ourselves, local or rented.
    pub const OWN_SERVER: BackendCaps = BackendCaps {
        renders_locally: true,
        accepts_token_ids: true,
        cache_reporting: CacheReporting::PerStage,
        meter: Meter::WallClock,
        prefix: PrefixGuarantee::Structural,
    };

    /// The shape of a token-metered provider API. Not implemented yet; reserved so
    /// the tests that must weaken against it exist first.
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
