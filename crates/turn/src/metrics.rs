//! `turn_metrics` (§4.4), one row per request.
//!
//! > per request: prompt/cached/predicted tokens, `finish_reason`, `prompt_ms`,
//! > `predicted_ms`, `draft_n`, `draft_n_accepted`, model, dialect, ledger head
//! > sent, and the computed expectations of §18.
//!
//! Two fields deserve an argument.
//!
//! **`cached_tokens` comes from `timings.cache_n`, not from `tokens_cached`.** The
//! `/completion` final frame carries both, and they are different quantities:
//! `cache_n` is "prompt tokens reused from cache" (`server-common.cpp:69`), while
//! `tokens_cached` is `slot.prompt.n_tokens()` *after* generation
//! (`server-context.cpp:4340`) — the slot's occupancy. Reading occupancy as reuse
//! inflates `f_keep` by the whole generation and would have reported this box's
//! first cold turn as 114% cached.
//!
//! **`cost.micros_usd` is `None` under `WallClock`, never zero.** Zero is a number
//! somebody will sum into a total.

use letibot_backend::{Meter, TurnCost};

use crate::completion::{FinishReason, Timings};
use crate::prefix::PrefixCheck;

#[derive(Debug, Clone, PartialEq)]
pub struct TurnMetrics {
    pub turn_id: String,
    pub model: String,
    /// The dialect's identity, which §4.4 makes the `template_sha` — so a model
    /// update that changes the template shows up as a different dialect rather
    /// than as an unexplained cache miss.
    pub dialect_template_sha: String,
    /// The ledger head that was submitted, hex. This is the `prefix_handle` of
    /// §3.10-B, recorded even though the fallback path cannot assert it server-side.
    pub ledger_head_sent: String,

    pub prompt_tokens: u64,
    /// Prompt tokens reused from the server's cache. See the module note.
    pub cached_tokens: u64,
    pub predicted_tokens: u64,

    /// How many prompt tokens the **server** says it received, and how many of them
    /// it actually processed (`n_prompt_tokens` and `timings.prompt_n`).
    ///
    /// Added by T17, because §14.3's C10 — deepseek's disjointness invariant,
    /// *"reported new input tokens equal `prompt_tokens − cached_tokens`"* — was not
    /// checkable from this struct: `prompt_tokens` above is **our** count of what we
    /// submitted, and with only that and `cached_tokens` the identity is a tautology
    /// rather than a measurement. It becomes a measurement when the server's own two
    /// numbers are here to disagree with ours.
    ///
    /// `prompt_tokens_server != prompt_tokens` means the server counted a different
    /// prompt than the one we built, which is the shape a truncated or rewritten
    /// prompt has and is worth failing loudly on.
    pub prompt_tokens_server: u64,
    /// Prompt tokens the server prefilled this turn — the complement of
    /// `cached_tokens`, from the server rather than from subtraction.
    pub prompt_processed: u64,

    pub finish_reason: FinishReason,
    pub prompt_ms: f64,
    pub predicted_ms: f64,
    pub draft_n: u64,
    pub draft_n_accepted: u64,
    /// Which slot served the turn, or -1. On a five-slot box a turn that lands on a
    /// slot which does not hold its history reports zero reuse, and that is a
    /// scheduling fact, not a prefix divergence. Recording it is what lets the two
    /// be told apart afterwards.
    pub id_slot: i64,

    /// §18.2's trap: "a tok/s number without its concurrency is not a number".
    /// `None` when the harness did not look — absent rather than an invented 1.
    pub n_busy_slots: Option<u32>,

    /// §18.1-I1, computed for this turn against the previous one.
    pub prefix_check: PrefixCheck,

    pub cost: TurnCost,
    pub wall_ms: u64,
}

impl TurnMetrics {
    /// `f_keep` — the fraction of the prompt that did not have to be prefilled.
    ///
    /// `None` for an empty prompt rather than 0.0 or 1.0: neither is true, and both
    /// would be averaged into a session figure.
    pub fn f_keep(&self) -> Option<f64> {
        if self.prompt_tokens == 0 {
            None
        } else {
            Some(self.cached_tokens as f64 / self.prompt_tokens as f64)
        }
    }

    /// Tokens that had to be prefilled. §18.2's p99 re-prefill is a distribution
    /// over this.
    pub fn reprefill(&self) -> u64 {
        self.prompt_tokens.saturating_sub(self.cached_tokens)
    }

    pub fn decode_per_second(&self) -> Option<f64> {
        if self.predicted_ms <= 0.0 {
            None
        } else {
            Some(self.predicted_tokens as f64 * 1000.0 / self.predicted_ms)
        }
    }

    /// Draft acceptance, `None` when no drafting happened.
    pub fn draft_acceptance(&self) -> Option<f64> {
        if self.draft_n == 0 {
            None
        } else {
            Some(self.draft_n_accepted as f64 / self.draft_n as f64)
        }
    }
}

/// Build the cost in the unit the backend actually charges.
pub fn cost_from(meter: Meter, timings: &Timings, prompt_tokens: u64, wall_ms: u64) -> TurnCost {
    TurnCost {
        meter,
        prompt_tokens,
        cached_tokens: timings.cache_n,
        generated_tokens: timings.predicted_n,
        wall_ms,
        micros_usd: match meter {
            Meter::WallClock => None,
            // No metered backend exists yet; the field is reserved, and inventing a
            // figure here would be worse than leaving the gap visible.
            Meter::Metered => None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prefix::PrefixCheck;
    use letibot_backend::BackendCaps;

    fn metrics(prompt: u64, cached: u64) -> TurnMetrics {
        TurnMetrics {
            turn_id: "t".into(),
            model: "m".into(),
            dialect_template_sha: "00".into(),
            ledger_head_sent: "ab".into(),
            prompt_tokens: prompt,
            cached_tokens: cached,
            predicted_tokens: 4,
            prompt_tokens_server: prompt,
            prompt_processed: prompt - cached,
            finish_reason: FinishReason::Eos,
            prompt_ms: 100.0,
            predicted_ms: 50.0,
            draft_n: 10,
            draft_n_accepted: 7,
            id_slot: 0,
            n_busy_slots: None,
            prefix_check: PrefixCheck::FirstTurn,
            cost: cost_from(Meter::WallClock, &Timings::default(), prompt, 150),
            wall_ms: 150,
        }
    }

    #[test]
    fn wall_clock_turns_carry_no_money_figure() {
        let m = metrics(100, 90);
        assert_eq!(m.cost.meter, Meter::WallClock);
        assert!(m.cost.micros_usd.is_none());
        assert!(BackendCaps::OWN_SERVER.meter == Meter::WallClock);
    }

    #[test]
    fn f_keep_is_absent_rather_than_invented_for_an_empty_prompt() {
        assert_eq!(metrics(0, 0).f_keep(), None);
        assert_eq!(metrics(100, 90).f_keep(), Some(0.9));
    }

    #[test]
    fn reprefill_is_the_number_the_p99_is_taken_over() {
        assert_eq!(metrics(1000, 940).reprefill(), 60);
    }

    #[test]
    fn draft_acceptance_is_absent_when_nothing_was_drafted() {
        let mut m = metrics(10, 0);
        assert_eq!(m.draft_acceptance(), Some(0.7));
        m.draft_n = 0;
        assert_eq!(m.draft_acceptance(), None);
    }
}
