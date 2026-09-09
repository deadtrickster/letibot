//! §8.5's first guard, over token ids as they arrive.
//!
//! > GLM's historical failure mode emits endless `@@@@…` at ~3 t/s and *poisons the
//! > slot* until restart. A rolling n-gram check over the last K generated tokens,
//! > aborting the turn and raising `Warning{code: repetition_collapse}`, is a few
//! > lines and turns a silent garbage answer into a named event.
//!
//! The dialect declares *what* to watch for ([`Guard`], data); the engine owns
//! *what to do*. So this module is a pure state machine over ids with no model, no
//! server and no dialect knowledge — which is why it is exhaustively testable.
//!
//! [`Guard::ReasoningStall`] is watched here too, and it is worth being precise
//! about what it is not: it is **not a cap on thinking**. Long thinking is fine.
//! The signal is reasoning that has run past a bound *without producing a content
//! or tool-call span* — non-termination, not length.

use letibot_dialect::{ControlRole, Guard};
use letibot_tokencore::TokenId;

/// A guard that tripped, with enough detail for the `Warning` event to be useful.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trip {
    pub code: &'static str,
    pub detail: String,
}

/// Runs a dialect's declared guards over a generation, one token at a time.
#[derive(Debug)]
pub struct GuardSet {
    guards: Vec<Guard>,
    recent: Vec<TokenId>,
    run_token: Option<TokenId>,
    run_len: u32,
    /// Tokens generated while inside a reasoning block with nothing emitted after
    /// it. Reset by any content or tool-call boundary.
    reasoning_tokens: u32,
    in_reasoning: bool,
    max_window: usize,
}

impl GuardSet {
    pub fn new(guards: &[Guard]) -> Self {
        let max_window = guards
            .iter()
            .filter_map(|g| match g {
                Guard::RepetitionNgram { window, times } => Some((*window * *times) as usize),
                _ => None,
            })
            .max()
            .unwrap_or(0);
        GuardSet {
            guards: guards.to_vec(),
            recent: Vec::new(),
            run_token: None,
            run_len: 0,
            reasoning_tokens: 0,
            in_reasoning: false,
            max_window,
        }
    }

    /// Feed one generated token and its control role, if it has one.
    pub fn observe(&mut self, id: TokenId, role: Option<ControlRole>) -> Option<Trip> {
        match role {
            Some(ControlRole::ThinkOpen) => {
                self.in_reasoning = true;
                self.reasoning_tokens = 0;
            }
            Some(ControlRole::ThinkClose) => {
                self.in_reasoning = false;
                self.reasoning_tokens = 0;
            }
            Some(ControlRole::ToolCallOpen) => {
                self.reasoning_tokens = 0;
            }
            _ => {
                if self.in_reasoning {
                    self.reasoning_tokens += 1;
                }
            }
        }

        if Some(id) == self.run_token {
            self.run_len += 1;
        } else {
            self.run_token = Some(id);
            self.run_len = 1;
        }

        if self.max_window > 0 {
            self.recent.push(id);
            if self.recent.len() > self.max_window {
                let excess = self.recent.len() - self.max_window;
                self.recent.drain(..excess);
            }
        }

        for guard in &self.guards {
            match *guard {
                Guard::RepetitionRun { run } if self.run_len >= run => {
                    return Some(Trip {
                        code: "repetition_collapse",
                        detail: format!("token {id} repeated {} times consecutively", self.run_len),
                    });
                }
                Guard::RepetitionNgram { window, times } => {
                    if ngram_repeats(&self.recent, window as usize, times as usize) {
                        return Some(Trip {
                            code: "repetition_collapse",
                            detail: format!(
                                "an {window}-token n-gram repeated {times} times at the tail"
                            ),
                        });
                    }
                }
                Guard::ReasoningStall { tokens } if self.reasoning_tokens >= tokens => {
                    return Some(Trip {
                        code: "reasoning_stall",
                        detail: format!(
                            "{} reasoning tokens with no content or tool-call span. \
                             This is a non-termination signal, not a length limit.",
                            self.reasoning_tokens
                        ),
                    });
                }
                _ => {}
            }
        }
        None
    }
}

/// Whether the tail of `recent` is `times` copies of the same `window`-token block.
fn ngram_repeats(recent: &[TokenId], window: usize, times: usize) -> bool {
    if window == 0 || times < 2 {
        return false;
    }
    let need = window * times;
    if recent.len() < need {
        return false;
    }
    let tail = &recent[recent.len() - need..];
    let first = &tail[..window];
    tail.chunks_exact(window).all(|c| c == first)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_at_sign_collapse_is_caught_by_the_run_guard() {
        let mut g = GuardSet::new(&[Guard::RepetitionRun { run: 8 }]);
        let mut trip = None;
        for i in 0..20 {
            if let Some(t) = g.observe(64, None) {
                trip = Some((i, t));
                break;
            }
        }
        let (i, t) = trip.expect("a run of 20 identical tokens must trip a run-8 guard");
        assert_eq!(i, 7, "it trips at the eighth, not later");
        assert_eq!(t.code, "repetition_collapse");
    }

    #[test]
    fn a_repeating_phrase_trips_the_ngram_guard_where_the_run_guard_cannot() {
        let mut g = GuardSet::new(&[
            Guard::RepetitionRun { run: 8 },
            Guard::RepetitionNgram {
                window: 3,
                times: 4,
            },
        ]);
        let mut tripped = None;
        for i in 0..40 {
            if let Some(t) = g.observe([10, 11, 12][i % 3], None) {
                tripped = Some(t);
                break;
            }
        }
        assert_eq!(tripped.unwrap().code, "repetition_collapse");
    }

    #[test]
    fn ordinary_prose_does_not_trip_anything() {
        let mut g = GuardSet::new(&[
            Guard::RepetitionRun { run: 8 },
            Guard::RepetitionNgram {
                window: 4,
                times: 3,
            },
        ]);
        for id in [5u32, 9, 5, 3, 77, 5, 9, 12, 40, 5, 3, 9, 66, 5, 9, 5, 3, 77] {
            assert!(g.observe(id, None).is_none(), "id {id}");
        }
    }

    /// Long thinking is fine. The stall guard must not fire on a long *productive*
    /// reasoning block that then produces something.
    #[test]
    fn reasoning_that_terminates_is_not_a_stall_however_long_it_ran() {
        let mut g = GuardSet::new(&[Guard::ReasoningStall { tokens: 100 }]);
        g.observe(1, Some(ControlRole::ThinkOpen));
        for _ in 0..99 {
            assert!(g.observe(7, None).is_none());
        }
        g.observe(2, Some(ControlRole::ThinkClose));
        for _ in 0..500 {
            assert!(g.observe(7, None).is_none(), "content is not reasoning");
        }
    }

    #[test]
    fn reasoning_that_never_produces_anything_is_named_as_non_termination() {
        let mut g = GuardSet::new(&[Guard::ReasoningStall { tokens: 50 }]);
        g.observe(1, Some(ControlRole::ThinkOpen));
        let mut trip = None;
        for _ in 0..200 {
            if let Some(t) = g.observe(7, None) {
                trip = Some(t);
                break;
            }
        }
        let t = trip.unwrap();
        assert_eq!(t.code, "reasoning_stall");
        assert!(t.detail.contains("non-termination"), "{}", t.detail);
    }

    #[test]
    fn no_declared_guards_means_no_bookkeeping_and_no_trips() {
        let mut g = GuardSet::new(&[]);
        for i in 0..1000 {
            assert!(g.observe(i % 2, None).is_none());
        }
        assert!(g.recent.is_empty(), "an undeclared guard must cost nothing");
    }
}
