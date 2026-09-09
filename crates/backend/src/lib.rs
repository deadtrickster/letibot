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
/// The plan's claim (§4.3) is that a prefix violation is *inexpressible*: request
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
    pub max_output_tokens: Option<u32>,
}

pub trait Backend {
    fn caps(&self) -> BackendCaps;

    /// Human-readable, for `EXPLAIN` and for test skip messages. A skip that does
    /// not say which backend caused it wastes the reader's time.
    fn name(&self) -> &str;
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
        let c = TurnCost { meter: Meter::WallClock, prompt_tokens: 10, cached_tokens: 8,
                           generated_tokens: 4, wall_ms: 120, micros_usd: None };
        assert!(c.micros_usd.is_none());
    }

    #[test]
    fn guarantees_are_ordered_so_a_suite_can_compare_them() {
        assert!(PrefixGuarantee::Structural < PrefixGuarantee::Asserted);
        assert!(PrefixGuarantee::Asserted < PrefixGuarantee::None);
    }
}
