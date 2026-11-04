//! `turn_metrics`: one row per request. `cached_tokens` is `timings.cache_n` (prompt tokens
//! reused), never `tokens_cached` (slot occupancy). `cost.micros_usd` is `None` under
//! `WallClock`, never zero. `f_keep` and `f_sim` differ: only `f_keep` ignores conversation growth.

use letibot_backend::{Meter, TurnCost};

use crate::completion::{FinishReason, Timings};
use crate::prefix::PrefixCheck;

#[derive(Debug, Clone, PartialEq)]
pub struct TurnMetrics {
    pub turn_id: String,
    pub model: String,
    /// The dialect's identity, which §4.4 makes the `template_sha` - so a model
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

    /// How many prompt tokens the SERVER says it received, and how many it actually processed.
    /// With only our own count the C10 identity is a tautology rather than a measurement; when
    /// the server's number differs from ours, the prompt it counted is not the one we built.
    pub prompt_tokens_server: u64,
    /// Prompt tokens the server prefilled this turn - the complement of
    /// `cached_tokens`, from the server rather than from subtraction.
    pub prompt_processed: u64,

    pub finish_reason: FinishReason,
    pub prompt_ms: f64,
    pub predicted_ms: f64,
    pub draft_n: u64,
    pub draft_n_accepted: u64,
    /// Which slot served the turn, or -1. A turn that lands on a slot which does not hold its
    /// history reports zero reuse, and that is a scheduling fact rather than a prefix
    /// divergence; recording it is what lets the two be told apart afterwards.
    pub id_slot: i64,

    /// §18.2's trap: "a tok/s number without its concurrency is not a number".
    /// `None` when the harness did not look - absent rather than an invented 1.
    pub n_busy_slots: Option<u32>,

    /// §18.1-I1, computed for this turn against the previous one.
    pub prefix_check: PrefixCheck,

    pub cost: TurnCost,
    pub wall_ms: u64,
    /// Time to the first generated token, ms from the request: the first frame that carried an
    /// id, or the first delta on a cloud turn. `None` when nothing was generated, which is not
    /// the same as instant.
    pub first_byte_ms: Option<u64>,
}

impl TurnMetrics {
    /// `f_keep` - `lcp / cached_entry`: how much of the entry we left in the server's cache last
    /// turn came back to us this turn.
    ///
    /// ```text
    /// f_keep(N+1) = cached_tokens(N+1) / (prompt_tokens(N) + committed_generated(N))
    /// ```
    ///
    /// Its denominator is what was cached, so it is indifferent to how much the conversation
    /// grew - the property [`Self::f_sim`] does not have. It reads that denominator from
    /// [`PrefixCheck::Held`] rather than recomputing it, so the two readings cannot disagree.
    /// `None` when there is no cached entry to measure against, which is neither 0.0 nor 1.0.
    ///
    /// It can exceed 1 and is deliberately not clamped: the denominator is our lower bound on
    /// the entry the server kept, so a value above 1 says the bound is conservative, and
    /// clamping would erase the only signal that says so.
    pub fn f_keep(&self) -> Option<f64> {
        match &self.prefix_check {
            PrefixCheck::Held {
                expected_cached_min: 0,
                ..
            } => None,
            PrefixCheck::Held {
                expected_cached_min,
                cached,
                ..
            } => Some(*cached as f64 / *expected_cached_min as f64),
            PrefixCheck::FirstTurn | PrefixCheck::Violated { .. } | PrefixCheck::Skipped { .. } => {
                None
            }
        }
    }

    /// `f_sim` - `lcp / new_prompt`, the fraction of THIS prompt that did not have to be
    /// prefilled. Not `f_keep`: its denominator is the new prompt, so it falls as the
    /// conversation grows, and no `f_keep` threshold may be applied to it.
    pub fn f_sim(&self) -> Option<f64> {
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
            first_byte_ms: None,
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
    fn f_sim_is_absent_rather_than_invented_for_an_empty_prompt() {
        assert_eq!(metrics(0, 0).f_sim(), None);
        assert_eq!(metrics(100, 90).f_sim(), Some(0.9));
    }

    /// `f_keep`'s denominator is the entry we left in the cache, so it does
    /// not move when the conversation grows. Worked both ways.
    #[test]
    fn f_keep_is_indifferent_to_growth_and_f_sim_is_not() {
        // A perfect turn: the whole 9,000-token entry came back, and the prompt
        // grew by a 1,093-token tool result.
        let mut m = metrics(10_093, 9_000);
        m.prefix_check = PrefixCheck::Held {
            expected_cached_min: 9_000,
            cached: 9_000,
            shortfall: 0,
        };
        assert_eq!(m.f_keep(), Some(1.0));
        assert!((m.f_sim().unwrap() - 0.892).abs() < 0.001);
    }

    /// `f_keep` has no denominator on a turn with nothing cached to measure
    /// against, and says so rather than inventing 0.0 or 1.0.
    #[test]
    fn f_keep_is_absent_when_there_is_no_cached_entry_to_measure_against() {
        let mut m = metrics(100, 0);
        assert_eq!(m.prefix_check, PrefixCheck::FirstTurn);
        assert_eq!(m.f_keep(), None, "a first turn cached nothing on purpose");
        m.prefix_check = PrefixCheck::Skipped {
            reason: "no".into(),
        };
        assert_eq!(m.f_keep(), None, "a skip is not a 1.0");
        m.prefix_check = PrefixCheck::Violated {
            detail: "no".into(),
        };
        assert_eq!(m.f_keep(), None, "a divergence has no meaningful margin");
    }

    /// The denominator is a lower bound on the entry the server kept, so `f_keep`
    /// can exceed 1 and is reported unclamped. Measured at +2 tokens on most turns
    /// and +745 after a turn that failed after prefilling.
    #[test]
    fn f_keep_above_one_is_reported_rather_than_clamped() {
        let mut m = metrics(1_002, 1_002);
        m.prefix_check = PrefixCheck::Held {
            expected_cached_min: 1_000,
            cached: 1_002,
            shortfall: 0,
        };
        assert_eq!(m.f_keep(), Some(1.002));
    }

    /// C3 and C4 are one measurement read two ways, so the shortfall and the
    /// ratio cannot disagree: `f_keep < 1` exactly when C3 reports a shortfall.
    #[test]
    fn f_keep_is_the_ratio_form_of_c3s_shortfall() {
        let mut m = metrics(1_000, 940);
        m.prefix_check = PrefixCheck::Held {
            expected_cached_min: 1_000,
            cached: 940,
            shortfall: 60,
        };
        assert_eq!(m.f_keep(), Some(0.94));
        let PrefixCheck::Held { shortfall, .. } = m.prefix_check else {
            unreachable!()
        };
        assert_eq!(shortfall > 0, m.f_keep().unwrap() < 1.0);
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
