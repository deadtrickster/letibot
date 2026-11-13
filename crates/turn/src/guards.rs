//! The first guard over token ids as they arrive: a rolling n-gram check turns GLM's endless
//! `@@@@…` into a named event. The dialect declares what to watch; the engine owns what to do.
//! `ReasoningStall` is not a thinking cap: reasoning with no content span is non-termination.

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
                Guard::RepetitionNgram { window, times } => {
                    Some((*window as usize).saturating_mul(*times as usize))
                }
                _ => None,
            })
            .max()
            .unwrap_or(0);
        GuardSet {
            guards: guards.to_vec(),
            // The bound is known here, so reserve it once rather than reallocating up to it
            // through every doubling from the first token of every turn. Capped, since the
            // window comes from config and an absurd one must not allocate up front.
            recent: Vec::with_capacity(max_window.min(4096)),
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
    let need = window.saturating_mul(times);
    if recent.len() < need {
        return false;
    }
    let tail = &recent[recent.len() - need..];
    let first = &tail[..window];
    tail.chunks_exact(window).all(|c| c == first)
}

/// Shortest block the text guard looks for: below it a repeat is a stutter or a ruler line.
pub const TEXT_REPEAT_MIN_PERIOD: usize = 16;
/// Longest block: a looped thought is a few sentences, and the window is a multiple of this.
pub const TEXT_REPEAT_MAX_PERIOD: usize = 2048;
/// Copies of a block before it is a loop: three can be a quote, a template or a retry.
pub const TEXT_REPEAT_MIN_COPIES: usize = 4;
/// Characters the repeat must cover, so a short line repeated in code or a table passes.
pub const TEXT_REPEAT_MIN_SPAN: usize = 1024;
/// Normalised bytes between checks, so a check is not paid per one-character delta.
const TEXT_REPEAT_CHECK_EVERY: usize = 64;
const TEXT_REPEAT_WINDOW: usize = TEXT_REPEAT_MAX_PERIOD * TEXT_REPEAT_MIN_COPIES;

/// The text-level `repetition_collapse` for a stream that carries no token ids (a cloud
/// provider): trips when the whitespace-normalised tail is the same block over and over.
/// One per channel, so reasoning and answer text never form one period between them.
#[derive(Debug)]
pub struct TextRepeat {
    channel: &'static str,
    buf: Vec<u8>,
    last_space: bool,
    unchecked: usize,
}

impl TextRepeat {
    pub fn new(channel: &'static str) -> Self {
        TextRepeat {
            channel,
            buf: Vec::with_capacity(2 * TEXT_REPEAT_WINDOW),
            last_space: false,
            unchecked: 0,
        }
    }

    /// Feed one streamed fragment of this channel.
    pub fn observe(&mut self, text: &str) -> Option<Trip> {
        for &b in text.as_bytes() {
            if b.is_ascii_whitespace() {
                if self.last_space {
                    continue;
                }
                self.last_space = true;
                self.buf.push(b' ');
            } else {
                self.last_space = false;
                self.buf.push(b);
            }
            self.unchecked += 1;
        }
        if self.buf.len() > 2 * TEXT_REPEAT_WINDOW {
            let excess = self.buf.len() - TEXT_REPEAT_WINDOW;
            self.buf.drain(..excess);
        }
        if self.unchecked < TEXT_REPEAT_CHECK_EVERY {
            return None;
        }
        self.unchecked = 0;
        let (period, copies) = repeating_tail(&self.buf)?;
        let block = &self.buf[self.buf.len() - period..];
        let shown = String::from_utf8_lossy(&block[..block.len().min(120)]);
        Some(Trip {
            code: "repetition_collapse",
            detail: format!(
                "a {period}-character block of {} repeated {copies} times at the tail: \"{shown}\"",
                self.channel
            ),
        })
    }
}

/// The shortest period in the guard's range whose copies cover the tail far enough, and how
/// many copies that is. Each candidate stops at its first mismatch, so prose costs one compare.
fn repeating_tail(s: &[u8]) -> Option<(usize, usize)> {
    let n = s.len();
    for p in TEXT_REPEAT_MIN_PERIOD..=TEXT_REPEAT_MAX_PERIOD {
        let copies = TEXT_REPEAT_MIN_COPIES.max(TEXT_REPEAT_MIN_SPAN.div_ceil(p));
        let need = p * copies;
        if need > n {
            if p * TEXT_REPEAT_MIN_COPIES > n {
                break;
            }
            continue;
        }
        let matched = (n - need + p..n)
            .rev()
            .take_while(|&i| s[i] == s[i - p])
            .count();
        if matched < need - p {
            continue;
        }
        // A sub-floor stutter ("ok ok ok") is periodic at every multiple too: not this guard's.
        let tail = &s[n - need..];
        if (1..TEXT_REPEAT_MIN_PERIOD).any(|q| (q..tail.len()).all(|i| tail[i] == tail[i - q])) {
            return None;
        }
        return Some((p, copies));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_times_product_past_u32_does_not_overflow() {
        let mut g = GuardSet::new(&[Guard::RepetitionNgram {
            window: u32::MAX,
            times: u32::MAX,
        }]);
        assert!(g.observe(1, None).is_none());
    }

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

    /// The thinking a k3.1-pro round looped on for minutes, verbatim.
    const K3_LOOP: &str = "Given the scale, let me first give the assessment and the plan, \
        then implement.\nGiven the size, maybe I should ask a clarifying question? No \u{2014} the \
        operator was clear.\nLet me check what vt100 looks like (a crate) and whether I can add it.\n";

    /// Feeds `text` in fragments of the given sizes, cycling, and returns the first trip with
    /// the byte offset it came at.
    fn feed(g: &mut TextRepeat, text: &str, sizes: &[usize]) -> Option<(usize, Trip)> {
        let (mut at, mut k) = (0, 0);
        while at < text.len() {
            let mut end = (at + sizes[k % sizes.len()]).min(text.len());
            while !text.is_char_boundary(end) {
                end += 1;
            }
            k += 1;
            if let Some(t) = g.observe(&text[at..end]) {
                return Some((end, t));
            }
            at = end;
        }
        None
    }

    #[test]
    fn the_k3_thinking_loop_trips_within_a_few_copies() {
        let looped = K3_LOOP.repeat(300);
        let mut g = TextRepeat::new("reasoning");
        let (at, t) = feed(&mut g, &looped, &[1, 7, 3, 29, 2, 64, 5])
            .expect("three sentences repeated 300 times must trip");
        assert_eq!(t.code, "repetition_collapse");
        assert!(t.detail.contains("reasoning"), "{}", t.detail);
        assert!(at <= 8 * K3_LOOP.len(), "tripped only after {at} bytes");
    }

    #[test]
    fn whitespace_differences_between_copies_do_not_hide_the_loop() {
        let mut looped = String::new();
        for i in 0..40 {
            looped.push_str(&K3_LOOP.replace('\n', if i % 2 == 0 { "\n\n" } else { "  " }));
        }
        let mut g = TextRepeat::new("text");
        assert!(feed(&mut g, &looped, &[13]).is_some());
    }

    #[test]
    fn three_copies_of_a_paragraph_do_not_trip() {
        let mut g = TextRepeat::new("text");
        let text = format!("{}And then something else entirely.", K3_LOOP.repeat(3));
        assert!(feed(&mut g, &text, &[1, 9]).is_none());
    }

    #[test]
    fn a_table_of_similar_rows_does_not_trip() {
        let mut text = String::from("| file | lines | status |\n|---|---|---|\n");
        for i in 0..300 {
            text.push_str(&format!(
                "| crates/turn/src/mod{i}.rs | {} | ok |\n",
                100 + i % 7
            ));
        }
        let mut g = TextRepeat::new("text");
        assert!(feed(&mut g, &text, &[5, 17]).is_none());
    }

    #[test]
    fn a_repeated_short_word_does_not_trip() {
        let mut g = TextRepeat::new("text");
        assert!(feed(&mut g, &"ok ".repeat(2000), &[2, 3]).is_none());
        let mut g = TextRepeat::new("text");
        assert!(feed(&mut g, &"=".repeat(5000), &[40]).is_none());
    }

    #[test]
    fn code_with_a_few_repeated_lines_does_not_trip() {
        let mut text = String::from("fn render(out: &mut String) {\n");
        for i in 0..60 {
            for _ in 0..3 {
                text.push_str("    out.push_str(\"\\n\");\n");
            }
            text.push_str(&format!("    out.push_str(&row({i}));\n"));
        }
        text.push_str("}\n");
        let mut g = TextRepeat::new("text");
        assert!(feed(&mut g, &text, &[11]).is_none());
    }
}
