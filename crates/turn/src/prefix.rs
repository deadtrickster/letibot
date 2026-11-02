//! The generation-inclusive prefix invariant, checked after every turn: request N's prompt
//! plus what the model generated in turn N must be a prefix of request N+1's prompt.
//!
//! The exact form - hash the submitted prompt with the committed generated tokens, and re-hash
//! the same span next turn - is the assertion. The observable form cannot pass on this box: a
//! hybrid/recurrent model cannot roll its memory back, so llama.cpp resumes from a context
//! checkpoint and reuses fewer tokens than the span it holds - a measurement of the server,
//! reported beside the assertion rather than failing the turn. Skipping is loud, not a pass.

use letibot_backend::BackendCaps;
use letibot_tokencore::{TokenId, hash_tokens};

/// What turn N leaves behind so turn N+1 can check it: turn N's submitted prompt plus the
/// generated tokens that were committed, which is the generation-inclusive part of the
/// invariant and why one hash settles both halves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrefixWitness {
    /// `prompt_tokens(N) + committed_generated(N)`.
    pub covered_len: usize,
    /// `H(prompt_N ‖ committed_generated_N)`.
    pub covered_hash: [u8; 32],
    pub prompt_tokens: u64,
    /// Generated tokens the harness committed, not the server's `predicted`: a trailing stop
    /// token is a boundary the renderer owns and is stripped before commit, so counting it
    /// would show a shortfall of one per turn, forever.
    pub committed_generated: u64,
}

impl PrefixWitness {
    /// Record what turn N sent and kept.
    pub fn record(prompt: &[TokenId], committed_generated: &[TokenId]) -> PrefixWitness {
        let mut covered = Vec::with_capacity(prompt.len() + committed_generated.len());
        covered.extend_from_slice(prompt);
        covered.extend_from_slice(committed_generated);
        PrefixWitness {
            covered_len: covered.len(),
            covered_hash: hash_tokens(&covered),
            prompt_tokens: prompt.len() as u64,
            committed_generated: committed_generated.len() as u64,
        }
    }

    /// The observable expectation for the next turn.
    pub fn expected_cached_min(&self) -> u64 {
        self.prompt_tokens + self.committed_generated
    }
}

/// The verdict, per turn. Recorded in `turn_metrics`.
#[derive(Debug, Clone, PartialEq)]
pub enum PrefixCheck {
    /// Nothing to compare against yet. Not a pass and not a failure.
    FirstTurn,
    /// The invariant held on the wire. `shortfall` is how far the server's reuse
    /// fell below what the invariant permits — a fact about the server's cache,
    /// since the prompts have just been proven identical over that span.
    Held {
        expected_cached_min: u64,
        cached: u64,
        shortfall: u64,
    },
    /// **The invariant was violated.** Turn N+1's prompt does not begin with turn
    /// N's prompt plus what the model generated. This is the 612 GB failure with
    /// the lid off, and it is the only verdict here that is a defect in us.
    Violated { detail: String },
    /// The check did not run. Carries the backend's own sentence saying so.
    Skipped { reason: String },
}

impl PrefixCheck {
    /// Whether the invariant was actually observed to hold. `Skipped` and `FirstTurn` answer
    /// false, and that is the point: a caller treating "not checked" as "checked and fine" has
    /// to write the word `Skipped` to do it.
    pub fn held(&self) -> bool {
        matches!(self, PrefixCheck::Held { .. })
    }

    /// Is this verdict the operator's business, or only a diagnostic? A divergence is our
    /// defect and a skip means the check did not run, so both change what somebody would do;
    /// a shortfall changes nothing, and a warning nobody can act on teaches them to skim.
    pub fn diagnostic_only(&self) -> bool {
        matches!(self, PrefixCheck::Held { .. })
    }

    /// The `Warning` this verdict should raise, if any.
    pub fn warning(&self) -> Option<(&'static str, String)> {
        match self {
            PrefixCheck::FirstTurn => None,
            PrefixCheck::Held { shortfall: 0, .. } => None,
            PrefixCheck::Held {
                expected_cached_min,
                cached,
                shortfall,
            } => Some((
                "cache_reuse_shortfall",
                format!(
                    "the prefix invariant held exactly, and the server still reused only \
                     {cached} of the {expected_cached_min} token(s) it could have — short \
                     by {shortfall}. The prompts are proven identical over that span, so \
                     this is the server's cache, not a divergence: on a hybrid/recurrent \
                     model llama.cpp resumes from a context checkpoint and snaps n_past \
                     back to it."
                ),
            )),
            PrefixCheck::Violated { detail } => Some(("prefix_divergence", detail.clone())),
            PrefixCheck::Skipped { reason } => Some(("prefix_check_skipped", reason.clone())),
        }
    }
}

/// Run the post-flight check. `prompt` is what this turn submitted; `cached` is
/// `timings.cache_n` from its final frame.
pub fn check(
    caps: &BackendCaps,
    previous: Option<&PrefixWitness>,
    prompt: &[TokenId],
    cached: u64,
) -> PrefixCheck {
    if let Some(reason) = caps.skip_reason("I1 (the generation-inclusive prefix invariant)") {
        return PrefixCheck::Skipped { reason };
    }
    debug_assert!(caps.may_assert_structural_prefix());

    let Some(prev) = previous else {
        return PrefixCheck::FirstTurn;
    };

    if prompt.len() < prev.covered_len {
        return PrefixCheck::Violated {
            detail: format!(
                "this turn's prompt is {} token(s); the previous turn sent {} and generated \
                 {} committed token(s), so {} were required before anything new",
                prompt.len(),
                prev.prompt_tokens,
                prev.committed_generated,
                prev.covered_len
            ),
        };
    }
    let rehash = hash_tokens(&prompt[..prev.covered_len]);
    if rehash != prev.covered_hash {
        return PrefixCheck::Violated {
            detail: format!(
                "the first {} token(s) of this turn's prompt hash to {} but the previous \
                 turn's prompt-plus-generation hashed to {}. Request N+1 does not extend \
                 request N.",
                prev.covered_len,
                hex(&rehash),
                hex(&prev.covered_hash)
            ),
        };
    }

    let expected = prev.expected_cached_min();
    PrefixCheck::Held {
        expected_cached_min: expected,
        cached,
        shortfall: expected.saturating_sub(cached),
    }
}

fn hex(bytes: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_metered_backend_skips_loudly_and_the_result_denies_being_a_pass() {
        let c = check(&BackendCaps::METERED_API, None, &[1, 2, 3], 999);
        let PrefixCheck::Skipped { reason } = &c else {
            panic!("{c:?}")
        };
        assert!(reason.contains("did not run"), "{reason}");
        assert!(reason.contains("not a pass"), "{reason}");
        assert!(!c.held(), "a skip must never answer `held`");
        assert_eq!(c.warning().unwrap().0, "prefix_check_skipped");
    }

    #[test]
    fn the_first_turn_has_nothing_to_compare_and_says_so_rather_than_passing() {
        let c = check(&BackendCaps::OWN_SERVER, None, &[1, 2, 3], 0);
        assert_eq!(c, PrefixCheck::FirstTurn);
        assert!(!c.held());
    }

    #[test]
    fn a_true_extension_holds_and_reports_the_servers_reuse_beside_it() {
        let w = PrefixWitness::record(&[1, 2, 3, 4, 5], &[6, 7]);
        let next = [1, 2, 3, 4, 5, 6, 7, 8, 9];
        let c = check(&BackendCaps::OWN_SERVER, Some(&w), &next, 7);
        assert_eq!(
            c,
            PrefixCheck::Held {
                expected_cached_min: 7,
                cached: 7,
                shortfall: 0
            }
        );
        assert!(c.held());
        assert!(c.warning().is_none());
    }

    /// The measured case on this box: identical prompts, and the server still
    /// reuses less. The verdict must say the invariant held.
    #[test]
    fn a_checkpoint_bounded_reuse_is_a_shortfall_and_not_a_divergence() {
        let w = PrefixWitness::record(&[1, 2, 3, 4, 5], &[6, 7]);
        let next = [1, 2, 3, 4, 5, 6, 7, 8];
        let c = check(&BackendCaps::OWN_SERVER, Some(&w), &next, 4);
        assert!(c.held(), "the invariant did hold: {c:?}");
        let (code, detail) = c.warning().unwrap();
        assert_eq!(code, "cache_reuse_shortfall");
        assert!(detail.contains("not a divergence"), "{detail}");
        assert!(detail.contains("checkpoint"), "{detail}");
    }

    /// The generation-inclusive half: dropping what the model generated is a
    /// violation even though the *prompt* half is still a clean prefix.
    #[test]
    fn losing_the_generated_tokens_is_a_violation_not_merely_a_short_prompt() {
        let w = PrefixWitness::record(&[1, 2, 3], &[4, 5]);
        // A harness that re-rendered the turn from text and lost token 5.
        let next = [1, 2, 3, 4, 9, 9];
        let c = check(&BackendCaps::OWN_SERVER, Some(&w), &next, 5);
        let PrefixCheck::Violated { detail } = &c else {
            panic!("{c:?}")
        };
        assert!(detail.contains("does not extend"), "{detail}");
        assert!(!c.held());
        assert_eq!(c.warning().unwrap().0, "prefix_divergence");
    }

    #[test]
    fn a_prompt_shorter_than_the_witness_is_a_violation_before_any_hashing() {
        let w = PrefixWitness::record(&[1, 2, 3, 4], &[5, 6]);
        let c = check(&BackendCaps::OWN_SERVER, Some(&w), &[1, 2, 3], 0);
        assert!(matches!(c, PrefixCheck::Violated { .. }), "{c:?}");
    }

    #[test]
    fn the_witness_covers_the_generation_and_not_only_the_prompt() {
        let w = PrefixWitness::record(&[1, 2, 3], &[4, 5]);
        assert_eq!(w.covered_len, 5);
        assert_eq!(w.prompt_tokens, 3);
        assert_eq!(w.committed_generated, 2);
        assert_eq!(w.expected_cached_min(), 5);
        assert_eq!(w.covered_hash, hash_tokens(&[1, 2, 3, 4, 5]));
    }
}
