//! The generation-inclusive prefix invariant, checked after every turn: request N's prompt plus
//! turn N's generation must prefix request N+1's prompt. Lower server reuse on hybrid models is
//! reported, not failed. Skipping is loud, not a pass.

use letibot_backend::BackendCaps;
use letibot_tokencore::{TokenId, hash_tokens};

/// What turn N leaves behind so turn N+1 can check it: the LEDGER the turn committed, which is
/// what the next prompt begins with, and the counts a violation is reported in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrefixWitness {
    /// The ledger after turn N's commit - the span turn N+1's prompt must extend.
    pub covered_len: usize,
    /// `H(that ledger)`.
    pub covered_hash: [u8; 32],
    /// What turn N submitted, the tail and the generation prompt included, for the message.
    pub prompt_tokens: u64,
    /// Generated tokens the harness committed, not the server's `predicted`: a trailing stop
    /// token is a boundary the renderer owns and is stripped before commit, so counting it
    /// would show a shortfall of one per turn, forever.
    pub committed_generated: u64,
}

impl PrefixWitness {
    /// Record what turn N sent and kept, when the whole prompt is the ledger: no tail, nothing
    /// elided. The engine uses [`Self::record_committed`], which is this over the real ledger.
    pub fn record(prompt: &[TokenId], committed_generated: &[TokenId]) -> PrefixWitness {
        let mut covered = Vec::with_capacity(prompt.len() + committed_generated.len());
        covered.extend_from_slice(prompt);
        covered.extend_from_slice(committed_generated);
        PrefixWitness::over(
            &covered,
            prompt.len() as u64,
            committed_generated.len() as u64,
        )
    }

    /// Record the ledger a turn left. Not the prompt it sent: a tail rides on the round and is
    /// committed nowhere, and an elided thought is committed as its replacement, so hashing either
    /// made the next ordinary turn read as a divergence.
    pub fn record_committed(
        sent: u64,
        committed_generated: u64,
        ledger: &[TokenId],
    ) -> PrefixWitness {
        PrefixWitness::over(ledger, sent, committed_generated)
    }

    fn over(covered: &[TokenId], sent: u64, committed_generated: u64) -> PrefixWitness {
        PrefixWitness {
            covered_len: covered.len(),
            covered_hash: hash_tokens(covered),
            prompt_tokens: sent,
            committed_generated,
        }
    }

    /// The observable expectation for the next turn: the span the prompts are proven identical
    /// over, which is the whole of what the server can have cached of it.
    pub fn expected_cached_min(&self) -> u64 {
        self.covered_len as u64
    }
}

/// The verdict, per turn. Recorded in `turn_metrics`.
#[derive(Debug, Clone, PartialEq)]
pub enum PrefixCheck {
    /// Nothing to compare against yet. Not a pass and not a failure.
    FirstTurn,
    /// The invariant held on the wire. `shortfall` is how far the server's reuse
    /// fell below what the invariant permits - a fact about the server's cache,
    /// since the prompts have just been proven identical over that span.
    Held {
        expected_cached_min: u64,
        cached: u64,
        shortfall: u64,
    },
    /// The invariant was violated. Turn N+1's prompt does not begin with turn
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
        // The one verdict that changes nothing is the one that HELD: read off `held` so the two
        // cannot drift, and so the guarantee is a caller's and not only the tests'.
        self.held()
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

    /// The head's *only a diagnostic* answer is read off the check's own `held`, so the two cannot
    /// come apart and silence a divergence or draw a shortfall as a defect.
    #[test]
    fn only_a_held_check_is_a_diagnostic() {
        for c in [
            PrefixCheck::FirstTurn,
            PrefixCheck::Held {
                expected_cached_min: 1,
                cached: 1,
                shortfall: 0,
            },
            PrefixCheck::Violated { detail: "x".into() },
            PrefixCheck::Skipped {
                reason: "asserted".into(),
            },
        ] {
            assert_eq!(c.diagnostic_only(), c.held(), "{c:?}");
        }
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

    /// The witness is the LEDGER a turn left, and the tail a round carried is not in it: hashed over
    /// the prompt, one offer made the next ordinary turn read as the 612 GB failure with the lid off.
    #[test]
    fn the_witness_is_the_ledger_and_leaves_the_rounds_tail_out() {
        // The prompt that went out: the ledger's head, then the tail, then the generation prompt.
        let prompt = [1, 2, 3, 90, 91, 92];
        // What the commit left: the same head, then what the model wrote. No tail.
        let ledger = [1, 2, 3, 4, 5];
        let w = PrefixWitness::record_committed(prompt.len() as u64, 2, &ledger);
        assert_eq!(w.covered_len, ledger.len());
        assert_eq!(w.covered_hash, hash_tokens(&ledger));
        assert_eq!(w.expected_cached_min(), ledger.len() as u64);
        assert_eq!(w.prompt_tokens, 6, "what the server was given is reported");

        // The next prompt is that ledger plus its own generation prompt.
        let next = [1, 2, 3, 4, 5, 6];
        assert!(
            check(&BackendCaps::OWN_SERVER, Some(&w), &next, 5).held(),
            "the ledger is a prefix of the next prompt, so the invariant holds"
        );
        // The control: the witness over the PROMPT does not, which is the false alarm this fixes.
        let stale = PrefixWitness::record(&prompt, &[4, 5]);
        assert!(
            matches!(
                check(&BackendCaps::OWN_SERVER, Some(&stale), &next, 5),
                PrefixCheck::Violated { .. }
            ),
            "a prompt-plus-generation witness hashes a tail the ledger never carried"
        );
    }
}
