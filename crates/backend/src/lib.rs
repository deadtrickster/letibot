//! The seam between the turn engine and whatever runs the model.
//! A backend declares what it cannot guarantee so tests skip loudly.
//! Depends on `letibot-transcript` alone, so a provider backend does not link the rendering stack.

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
    /// Tokens cost wall clock. Local, and our own cloud compute - the operator's
    /// point is that renting the hardware makes these the same case, because you
    /// pay for time either way.
    WallClock,
    /// Tokens cost money. Compaction stops being a latency optimisation.
    Metered,
}

/// How strongly the append-only prompt invariant holds. The claim that a prefix
/// violation is blocked holds only for token ids over memory we own; it does not
/// survive a `messages` API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PrefixGuarantee {
    /// Violation is unrepresentable. Assert the region property directly.
    Structural,
    /// We send messages; the provider renders. We can only check afterwards that
    /// its cache behaved as though the prefix was stable - a weaker, statistical
    /// claim about the past.
    Asserted,
    /// Nothing to check. A suite reaching this must skip and say so.
    None,
}

/// What a turn cost, in the unit that backend actually charges.
///
/// One type with an explicit meter, so a report cannot add milliseconds to dollars.
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

/// One turn's work, in terms a local and a remote backend can both realise. It
/// carries the transcript, not a rendered prompt: realisation is the backend's
/// job, and a rendered string would force every backend through the render stack.
#[derive(Debug, Clone)]
pub struct TurnRequest<'a> {
    pub system: &'a str,
    pub tools_json: &'a [String],
    pub items: &'a [TranscriptItem],
    /// Items this request carries and the transcript does not: rendered after
    /// `items`, committed nowhere, so the next request's cached prefix is exactly
    /// `items`. Never a `System` item - the provider refuses a trailing one.
    pub tail: &'a [TranscriptItem],
    pub max_output_tokens: Option<u32>,
    /// `None` keeps the backend's own setting. `Some(false)` asks this one request not to think:
    /// a cheap turn whose expected end is silence.
    pub thinking: Option<bool>,
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
    /// The provider could not be reached, or the connection died mid-answer. `kind` is the io
    /// failure underneath when there was one, so a deadline stays a deadline for the caller's retry
    /// rule instead of being read back out of the sentence.
    Unreachable {
        message: String,
        kind: Option<std::io::ErrorKind>,
    },
    /// The provider answered and said no: a bad key, a bad model, a quota.
    Refused {
        status: u16,
        body: String,
        /// `Retry-After` in seconds, when the provider sent one: the only form of *when may I come
        /// back* a caller may sleep on.
        retry_after: Option<u64>,
    },
    /// 2xx, but not the shape expected.
    Malformed(String),
    /// The caller asked to stop; nothing is recorded as a finished answer.
    Aborted,
}

impl BackendError {
    /// A transport failure, with the io kind under it when there is one.
    pub fn unreachable(message: impl Into<String>, kind: Option<std::io::ErrorKind>) -> Self {
        BackendError::Unreachable {
            message: message.into(),
            kind,
        }
    }

    /// Whether this was a deadline rather than a reset: one `io::ErrorKind`, never a substring.
    pub fn timed_out(&self) -> bool {
        matches!(
            self,
            BackendError::Unreachable {
                kind: Some(std::io::ErrorKind::TimedOut),
                ..
            }
        )
    }
}

impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BackendError::Unreachable { message, .. } => {
                write!(f, "provider unreachable: {message}")
            }
            BackendError::Refused { status, body, .. } => {
                write!(f, "provider refused ({status}): {body}")
            }
            BackendError::Malformed(m) => write!(f, "provider answered something unexpected: {m}"),
            BackendError::Aborted => write!(f, "the turn was aborted by its caller"),
        }
    }
}

impl std::error::Error for BackendError {}

/// A backend that takes the transcript and answers with a completion. The turn
/// engine keeps its token ledger as the local record, tokenised with the session's
/// own vocabulary and never sent anywhere.
pub trait MessagesBackend: Send + Sync {
    fn caps(&self) -> BackendCaps;

    /// `deepseek`, `glm`, `grok` - the preset's name.
    fn name(&self) -> &str;

    /// The model id sent on the wire.
    fn model(&self) -> &str;

    /// `HOST:PORT` of the thing this backend actually talks to, for a message that
    /// has to name it: read from the URL the request is posted to, not stored
    /// beside it where it could drift away from the daemon's configuration.
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
    /// The shape of a llama.cpp server we run ourselves - local or rented, the
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
