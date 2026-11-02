//! What a prefilled prompt costs the model server's cache, and the size it may not pass.
//!
//! `TODO.md` T18: every distinct prompt prefilled becomes one prompt-cache entry, and on a
//! hybrid model the cost is dominated by a term that does not shrink with the prompt. The
//! measured split (`docs/qwen-attention.md`) is a fixed per-sequence recurrent accumulator and a
//! per-token attention half, so a sweep of many SHORT distinct prompts is far dearer than its
//! token count suggests - which is how the fifth OOM happened.
//!
//! Two things follow, and this type is both. The price of an entry is a function of its token
//! count and nothing else, so it can be computed before the prefill is asked for. And the number
//! to bound is the number of live entries, not their order: eviction at the server is by
//! capacity, so interleaving conversations costs nothing and merely HAVING them costs everything.
//!
//! So the size the operator sets is the cache's, not one prompt's: [`PromptCache::note`] is asked
//! before each prefill and answers whether it fits, evicting the least recently used entries to
//! make room. A single entry larger than the whole budget is refused rather than admitted and
//! then evicted - a cache that can hold nothing is a cache that pretends.

use std::collections::BTreeMap;

/// The per-sequence recurrent state: fixed, independent of length, and the reason a short prompt
/// is not cheap. Measured for Qwen3.8-Flash-Next, whose 36 linear/recurrent blocks hold it.
pub const RECURRENT_BYTES: u64 = 111 * 1024 * 1024;

/// The attention half. `docs/qwen-attention.md` measures ~24 KiB per token.
pub const ATTENTION_BYTES_PER_TOKEN: u64 = 24 * 1024;

/// What the box's server is given when nobody says otherwise - llama.cpp's own `--cache-ram`
/// default. A budget the operator has not set is still a budget, and it is stated rather than
/// implied so that "unbounded" is never the accidental answer.
pub const DEFAULT_LIMIT_BYTES: u64 = 8 * 1024 * 1024 * 1024;

/// Bytes one cache entry of `tokens` prompt tokens occupies.
pub const fn entry_bytes(tokens: u64) -> u64 {
    RECURRENT_BYTES + tokens * ATTENTION_BYTES_PER_TOKEN
}

/// What became of a prompt the caller was about to prefill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    /// It fits, and is now accounted for.
    Admitted,
    /// It fits after `n` older entries were given up.
    Evicted { n: usize },
    /// It cannot fit at any age, because one entry alone is larger than the whole budget.
    Refused { bytes: u64, limit: u64 },
}

impl Admission {
    pub fn admitted(&self) -> bool {
        !matches!(self, Admission::Refused { .. })
    }
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    bytes: u64,
    /// Order of last use. The clock is a counter rather than a wall time so that a test's
    /// verdict does not depend on how fast it ran.
    used_at: u64,
}

/// The server's prompt cache, as a budget. See the module docs.
#[derive(Debug, Clone)]
pub struct PromptCache {
    limit: u64,
    used: u64,
    clock: u64,
    entries: BTreeMap<String, Entry>,
}

impl PromptCache {
    pub fn new(limit: u64) -> PromptCache {
        PromptCache {
            limit,
            used: 0,
            clock: 0,
            entries: BTreeMap::new(),
        }
    }

    /// The size the operator set, in bytes - the number a disclosure prints.
    pub fn limit(&self) -> u64 {
        self.limit
    }

    /// What the cache holds right now, in bytes. Never above [`Self::limit`].
    pub fn used(&self) -> u64 {
        self.used
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// One line for the operator: what the cache may hold, and what it holds now.
    pub fn describe(&self) -> String {
        format!(
            "prompt cache {} of {}: room for about {} sequence(s) at {} each, plus {} per token",
            human(self.used),
            human(self.limit),
            self.limit / RECURRENT_BYTES.max(1),
            human(RECURRENT_BYTES),
            human(ATTENTION_BYTES_PER_TOKEN)
        )
    }

    /// Ask for room for a prompt of `tokens` tokens under `key`, giving up the least recently
    /// used entries to make it. Asking twice for a live key only marks it used.
    pub fn note(&mut self, key: &str, tokens: u64) -> Admission {
        self.clock += 1;
        let now = self.clock;
        if let Some(e) = self.entries.get_mut(key) {
            e.used_at = now;
            return Admission::Admitted;
        }
        let bytes = entry_bytes(tokens);
        if bytes > self.limit {
            return Admission::Refused {
                bytes,
                limit: self.limit,
            };
        }
        let mut evicted = 0;
        while self.used + bytes > self.limit {
            // The oldest use goes first. `BTreeMap` is keyed by name for a stable listing, so
            // the victim is found by scanning - the cache holds tens of entries, not thousands.
            let Some(victim) = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.used_at)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            if let Some(e) = self.entries.remove(&victim) {
                self.used -= e.bytes;
                evicted += 1;
            }
        }
        self.used += bytes;
        self.entries.insert(
            key.to_string(),
            Entry {
                bytes,
                used_at: now,
            },
        );
        if evicted == 0 {
            Admission::Admitted
        } else {
            Admission::Evicted { n: evicted }
        }
    }

    /// Forget an entry, for a conversation that is over. The budget is capacity, so a live
    /// prompt may stay; this is for the caller that knows it is not coming back.
    pub fn release(&mut self, key: &str) {
        if let Some(e) = self.entries.remove(key) {
            self.used -= e.bytes;
        }
    }
}

impl Default for PromptCache {
    fn default() -> Self {
        PromptCache::new(DEFAULT_LIMIT_BYTES)
    }
}

fn human(bytes: u64) -> String {
    const UNITS: [(&str, u64); 4] = [
        ("GiB", 1 << 30),
        ("MiB", 1 << 20),
        ("KiB", 1 << 10),
        ("B", 1),
    ];
    for (name, size) in UNITS {
        if bytes >= size {
            let n = bytes as f64 / size as f64;
            return if size == 1 {
                format!("{bytes} B")
            } else {
                format!("{n:.1} {name}")
            };
        }
    }
    "0 B".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One entry of this many tokens, so a test can say how many fit.
    const SMALL: u64 = 1_000;

    fn per_entry() -> u64 {
        entry_bytes(SMALL)
    }

    #[test]
    fn an_entry_costs_the_recurrent_state_plus_its_tokens() {
        assert_eq!(entry_bytes(0), RECURRENT_BYTES);
        assert_eq!(entry_bytes(1), RECURRENT_BYTES + ATTENTION_BYTES_PER_TOKEN);
        // The point of T18: for a short prompt the fixed term is most of the bill, so a sweep of
        // many short distinct prompts costs far more than their token count suggests.
        assert!(
            SMALL * ATTENTION_BYTES_PER_TOKEN < RECURRENT_BYTES,
            "a {SMALL}-token prompt is still mostly its sequence, not its tokens"
        );
    }

    /// The test T18 asks for: fill past the bound and watch the cache stay inside it.
    #[test]
    fn filling_past_the_bound_evicts_and_never_exceeds_it() {
        // Room for two entries and no more.
        let mut cache = PromptCache::new(2 * per_entry() + per_entry() / 2);
        assert_eq!(cache.limit(), 2 * per_entry() + per_entry() / 2);
        assert!(cache.used() <= cache.limit());

        assert_eq!(cache.note("a", SMALL), Admission::Admitted);
        assert_eq!(cache.note("b", SMALL), Admission::Admitted);
        assert_eq!(cache.len(), 2);

        // The third does not fit, so the least recently used goes.
        assert_eq!(cache.note("c", SMALL), Admission::Evicted { n: 1 });
        assert_eq!(cache.len(), 2);
        assert!(cache.used() <= cache.limit(), "{}", cache.describe());

        // Ten more, and the bound still holds at every step.
        for i in 0..10 {
            let a = cache.note(&format!("n{i}"), SMALL);
            assert!(a.admitted(), "{a:?}");
            assert!(
                cache.used() <= cache.limit(),
                "step {i}: {} over {}",
                cache.used(),
                cache.limit()
            );
            assert!(cache.len() <= 2, "step {i}: {}", cache.len());
        }
        assert!(cache.describe().contains("prompt cache"));
    }

    #[test]
    fn the_least_recently_used_entry_is_the_one_given_up() {
        let mut cache = PromptCache::new(2 * per_entry());
        cache.note("a", SMALL);
        cache.note("b", SMALL);
        // `a` is used again, so `b` is now the oldest.
        cache.note("a", SMALL);
        cache.note("c", SMALL);
        assert!(cache.entries.contains_key("a"), "{:?}", cache.entries);
        assert!(!cache.entries.contains_key("b"));
        assert!(cache.entries.contains_key("c"));
    }

    #[test]
    fn asking_twice_for_a_live_prompt_costs_nothing() {
        let mut cache = PromptCache::new(4 * per_entry());
        cache.note("a", SMALL);
        let after = cache.used();
        assert_eq!(cache.note("a", SMALL), Admission::Admitted);
        assert_eq!(cache.used(), after);
        assert_eq!(cache.len(), 1);
    }

    /// A budget smaller than one entry holds nothing, and says so rather than thrashing: an
    /// entry admitted and immediately evicted would report a cache that never caches.
    #[test]
    fn a_prompt_larger_than_the_whole_budget_is_refused() {
        let mut cache = PromptCache::new(RECURRENT_BYTES / 2);
        let refused = cache.note("huge", SMALL);
        assert_eq!(
            refused,
            Admission::Refused {
                bytes: per_entry(),
                limit: RECURRENT_BYTES / 2
            }
        );
        assert!(!refused.admitted());
        assert!(cache.is_empty());
        assert_eq!(cache.used(), 0);
    }

    #[test]
    fn releasing_a_prompt_gives_its_bytes_back() {
        let mut cache = PromptCache::new(2 * per_entry());
        cache.note("a", SMALL);
        cache.note("b", SMALL);
        cache.release("a");
        assert_eq!(cache.used(), per_entry());
        // The room is real: another entry goes in without evicting `b`.
        assert_eq!(cache.note("c", SMALL), Admission::Admitted);
        assert!(cache.entries.contains_key("b"));
        // Releasing what is not there is not an underflow.
        cache.release("gone");
        assert_eq!(cache.used(), 2 * per_entry());
    }

    #[test]
    fn the_default_is_stated_rather_than_implied() {
        let d = PromptCache::default();
        assert_eq!(d.limit(), DEFAULT_LIMIT_BYTES);
        assert_eq!(d.used(), 0);
    }
}
