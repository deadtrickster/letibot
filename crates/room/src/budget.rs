//! The per-sender budget for one room: a token bucket, so one session's chatter cannot fill every
//! reader's context. A refused post names the budget and when the next one is allowed, because a
//! refusal a model cannot act on is a turn spent guessing.

use std::collections::BTreeMap;

use crate::Refused;

/// Posts one sender may have in one room before it must wait for a token to come back.
pub const BUDGET_POSTS: u32 = 10;

/// How long one spent post takes to come back.
pub const BUDGET_REFILL_MS: u64 = 30_000;

/// How many (sender, room) buckets are kept. A box is tens of sessions and a few rooms, so this is
/// a bound on a name that is never closed again, not on real traffic.
pub const MAX_BUCKETS: usize = 4_096;

/// One sender's tokens in one room.
#[derive(Debug, Clone, Copy)]
struct Held {
    tokens: u32,
    /// When the tokens were last topped up, so a partial refill is not lost to a comparison.
    at_ms: u64,
}

/// Every sender's tokens, by (sender, room).
#[derive(Debug, Clone)]
pub struct Budget {
    posts: u32,
    refill_ms: u64,
    held: BTreeMap<(String, String), Held>,
}

impl Default for Budget {
    fn default() -> Budget {
        Budget::new(BUDGET_POSTS, BUDGET_REFILL_MS)
    }
}

impl Budget {
    pub fn new(posts: u32, refill_ms: u64) -> Budget {
        Budget {
            posts: posts.max(1),
            refill_ms: refill_ms.max(1),
            held: BTreeMap::new(),
        }
    }

    /// The posts one sender may spend in one room.
    pub fn posts(&self) -> u32 {
        self.posts
    }

    pub fn refill_ms(&self) -> u64 {
        self.refill_ms
    }

    /// What `who` holds in `room` at `now`, with the clock's refill counted: the tokens, and the
    /// moment the last of them came back.
    fn state(&self, who: &str, room: &str, now: u64) -> Held {
        let (posts, refill_ms) = (self.posts, self.refill_ms);
        let held = self
            .held
            .get(&(who.to_string(), room.to_string()))
            .copied()
            .unwrap_or(Held {
                tokens: posts,
                at_ms: now,
            });
        let back = now.saturating_sub(held.at_ms) / refill_ms;
        Held {
            tokens: (held.tokens + back.min(posts as u64) as u32).min(posts),
            at_ms: held.at_ms + back * refill_ms,
        }
    }

    /// Is one of `who`'s posts in `room` allowed now? Nothing is spent here, so a post that is
    /// then refused for another reason costs its sender nothing.
    pub fn allowed(&self, who: &str, room: &str, now: u64) -> Result<(), Refused> {
        let state = self.state(who, room, now);
        if state.tokens > 0 {
            return Ok(());
        }
        Err(Refused::Budget {
            posts: self.posts as u64,
            refill_ms: self.refill_ms,
            wait_ms: self
                .refill_ms
                .saturating_sub(now.saturating_sub(state.at_ms)),
        })
    }

    /// Spend one of `who`'s posts in `room` - [`Budget::allowed`], and the token that goes with it.
    pub fn spend(&mut self, who: &str, room: &str, now: u64) -> Result<(), Refused> {
        self.allowed(who, room, now)?;
        let mut state = self.state(who, room, now);
        if !self.held.contains_key(&(who.to_string(), room.to_string()))
            && self.held.len() >= MAX_BUCKETS
            && let Some(gone) = self
                .held
                .iter()
                .min_by_key(|(_, h)| h.at_ms)
                .map(|(k, _)| k.clone())
        {
            self.held.remove(&gone);
        }
        state.tokens -= 1;
        self.held.insert((who.to_string(), room.to_string()), state);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_burst_of_ten_is_allowed_and_the_eleventh_says_when_the_next_is() {
        let mut b = Budget::new(3, 1_000);
        for n in 0..3 {
            assert!(b.spend("s-a", "host", 5_000).is_ok(), "post {n}");
        }
        let refused = b.spend("s-a", "host", 5_200).expect_err("the fourth");
        match refused {
            Refused::Budget {
                posts,
                refill_ms,
                wait_ms,
            } => {
                assert_eq!((posts, refill_ms, wait_ms), (3, 1_000, 800));
            }
            other => panic!("{other:?}"),
        }
        // A refusal spends nothing: the first token back is the one that is used.
        assert!(b.spend("s-a", "host", 6_000).is_ok());
        assert!(b.spend("s-a", "host", 6_000).is_err());
    }

    #[test]
    fn each_room_and_each_sender_has_its_own_tokens() {
        let mut b = Budget::new(1, 1_000);
        assert!(b.spend("s-a", "host", 0).is_ok());
        assert!(b.spend("s-a", "host", 0).is_err());
        assert!(b.spend("s-b", "host", 0).is_ok());
        assert!(b.spend("s-a", "project/p1", 0).is_ok());
    }

    #[test]
    fn a_token_comes_back_once_per_refill_and_never_past_the_cap() {
        let mut b = Budget::new(2, 1_000);
        assert!(b.spend("s-a", "host", 0).is_ok());
        assert!(b.spend("s-a", "host", 0).is_ok());
        assert!(b.spend("s-a", "host", 0).is_err());
        assert!(b.spend("s-a", "host", 1_000).is_ok());
        assert!(b.spend("s-a", "host", 1_000).is_err());
        // An hour of silence gives back the cap and no more.
        assert!(b.spend("s-a", "host", 3_600_000).is_ok());
        assert!(b.spend("s-a", "host", 3_600_000).is_ok());
        assert!(b.spend("s-a", "host", 3_600_000).is_err());
    }
}
