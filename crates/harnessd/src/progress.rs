//! **A progress detector, because a round counter measures effort and not progress.**
//!
//! `max_tool_rounds` was the only thing that could stop a runaway turn, and it cut
//! two legitimate sessions: one asked to *"look at the project and suggest
//! improvements"* and was cut at 12 rounds mid-investigation, another was two rounds
//! from finishing with every round doing new work. A count of rounds is an
//! **open-loop guard** in `docs/closed-loop.md`'s terms — it never looks at the
//! effect, only at the command. A model that is working hard and a model that is
//! stuck are indistinguishable to it, and it stopped the first one.
//!
//! The message was worse than the cut:
//!
//! ```text
//! the model called tools 12 times without answering
//! ```
//!
//! It *was* answering; it had not finished. That sentence teaches an operator to
//! distrust the model when the harness was at fault — the same defect as `grep`
//! reporting absence when it had opened no files.
//!
//! # The encoder
//!
//! What should stop a turn is a model that is **not getting anywhere**. This module
//! is the sensor for that, and it is a sensor rather than a heuristic because every
//! signal it reads is a fact the harness already computes:
//!
//! | signal | where it comes from |
//! |---|---|
//! | the same `(tool, arguments)` called again | [`letibot_turn::events::args_digest`], already computed for `ToolCallProposed` |
//! | a result byte-identical to one already seen this turn | [`letibot_tools::payload_digest`], already computed for `ToolEvent::Finished` |
//! | a run of abstentions, denials or `not_run`s | [`letibot_tools::builtins::intent::ledger::is_effect`] — **the ledger's own predicate**, not a second one beside it |
//!
//! That last row is the load-bearing one. `intent::close_the_turn` already draws the
//! line this needs: an `Ok` is an **effect**, and an abstention, a failure, a denial
//! and a `not_run` are **attempts**. Rather than restate it here, `is_effect` was
//! lifted out of `IntentLedger::record_effect` so there is one definition with two
//! readers. A second, subtly different notion of "did that work" is how two
//! instruments end up disagreeing about the same turn.
//!
//! # The rule, and why it is payload novelty rather than argument novelty
//!
//! > A round **made progress** when at least one of its calls returned `Ok` with a
//! > result this turn had not already seen.
//!
//! The strongest single signal is the repeated `(tool, arguments)` pair, and it is
//! still not the one the stop is built on — because the case that matters most is
//! the legitimate one. A model that edits a file and reads it back issues a
//! *byte-identical call* and gets a *different answer*, and that is progress. A model
//! that reads the same unchanged file three times issues the same call and gets the
//! same bytes, and that is not. Only the payload separates them, so the payload is
//! what decides, and the repeated call is kept for the **evidence** — which is what
//! an operator needs to argue with the stop.
//!
//! # The tolerance band
//!
//! `docs/closed-loop.md` §4: a closed loop corrects inside the band and faults
//! outside it. So the stop is two-stage — at `stall_rounds - 1` consecutive stalled
//! rounds the model is **told what the harness sees**, once, through the same
//! steering path T21.3 uses; only if the next round is also stalled does the turn
//! stop. A turn that was working gets a sentence and carries on; a turn that is
//! looping gets stopped one round later than it would have been. That asymmetry is
//! deliberate: **the measured cost of cutting a working turn is now two wasted
//! sessions**, and the cost of one extra round of a stuck one is one round.
//!
//! # What it says when it fires
//!
//! Never a count of rounds. It names what it saw, with the denominator
//! (`docs/tool-design-brief.md` §2.2) — *"the last 5 rounds ran 7 calls and 0 of 7
//! returned a result this turn had not already seen"* — because a stop that cites
//! evidence is one an operator can contradict, and `rounds: 12` is not.

use std::collections::{HashMap, HashSet};

use letibot_tools::builtins::intent::ledger::is_effect;
use letibot_transcript::{ToolCall, ToolOutcome};

/// What one round of the tool loop did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Round {
    /// At least one call returned `Ok` with a result this turn had not seen.
    Progress,
    /// Nothing in the round returned a result that was not already in hand.
    /// One of these is normal. A run of them is the stop condition.
    Stalled,
}

/// One tool result, as the detector saw it. Kept only for the stalled window, so
/// the stop can name the evidence rather than the count.
#[derive(Debug, Clone)]
struct Observation {
    /// `args_digest(name\0arguments)` — what "the same call" means.
    key: String,
    /// `read path=src/main.rs` — the call, rendered for a human.
    label: String,
    /// `ok`, `abstained`, `failed`, … The tally the evidence prints.
    outcome: &'static str,
    /// This turn had already made this exact `(tool, arguments)` call.
    repeat_call: bool,
    /// This turn had already seen these exact result bytes.
    repeat_payload: bool,
    /// `Ok`, by the ledger's predicate.
    effect: bool,
}

/// The per-turn progress encoder. One per user turn: what counts as "already seen"
/// is scoped to the turn, because a file read in an earlier turn is a legitimate
/// thing to read again now.
#[derive(Debug)]
pub struct ProgressDetector {
    /// Consecutive stalled rounds that stop the turn. `0` disables the check and
    /// leaves only the round backstop — a declared state, not a silent default.
    stall_rounds: usize,
    /// `args_digest(name\0arguments)` → the rendered label, and how many times this
    /// turn has made the call.
    calls: HashMap<String, (String, usize)>,
    /// Every result payload digest this turn has seen.
    payloads: HashSet<String>,
    /// Results observed since the last [`ProgressDetector::end_round`].
    pending: Vec<Observation>,
    /// Consecutive stalled rounds, ending at the last one observed.
    run: usize,
    /// The observations that make up the current stalled run. Cleared by progress.
    window: Vec<Observation>,
    /// The mid-run nudge is once per turn, for the same reason T21.3's is: one is
    /// the error signal, two is the harness insisting.
    nudged: bool,
}

impl ProgressDetector {
    pub fn new(stall_rounds: usize) -> Self {
        Self {
            stall_rounds,
            calls: HashMap::new(),
            payloads: HashSet::new(),
            pending: Vec::new(),
            run: 0,
            window: Vec::new(),
            nudged: false,
        }
    }

    /// Whether the check is on at all. `false` means the round backstop is the only
    /// thing that can stop a turn, which is the state this module exists to replace.
    pub fn armed(&self) -> bool {
        self.stall_rounds > 0
    }

    /// One tool result, as it came back. Call order, in call order.
    ///
    /// `payload` is the **rendered** result — the same string the transcript row
    /// carries and the same one `ToolEvent::Finished` digests, so novelty here means
    /// novelty in what the model actually got to read.
    pub fn observe(&mut self, call: &ToolCall, outcome: &ToolOutcome, payload: &str) {
        let key =
            letibot_turn::events::args_digest(&format!("{}\u{0}{}", call.name, call.arguments));
        let entry = self
            .calls
            .entry(key.clone())
            .or_insert_with(|| (label(call), 0usize));
        let repeat_call = entry.1 > 0;
        entry.1 += 1;
        let label = entry.0.clone();

        let repeat_payload = !self.payloads.insert(letibot_tools::payload_digest(payload));

        self.pending.push(Observation {
            key,
            label,
            outcome: outcome_word(outcome),
            repeat_call,
            repeat_payload,
            effect: is_effect(outcome),
        });
    }

    /// Close the round and score it. Call once per round that ran tools.
    pub fn end_round(&mut self) -> Round {
        let pending = std::mem::take(&mut self.pending);
        // The rule, in one line: an `Ok` carrying bytes this turn has not seen.
        if pending.iter().any(|o| o.effect && !o.repeat_payload) {
            self.run = 0;
            self.window.clear();
            Round::Progress
        } else {
            self.run += 1;
            self.window.extend(pending);
            Round::Stalled
        }
    }

    /// Consecutive stalled rounds ending at the last one scored.
    pub fn stalled_run(&self) -> usize {
        self.run
    }

    /// The turn should stop: `stall_rounds` rounds in a row produced nothing new.
    pub fn exhausted(&self) -> bool {
        self.armed() && self.run >= self.stall_rounds
    }

    /// The one mid-run correction, or `None`.
    ///
    /// Fires one round before [`ProgressDetector::exhausted`] would, so a turn that
    /// is in fact working gets told what the harness sees and can say so, and a turn
    /// that is looping is stopped one round later. See the module header on the
    /// tolerance band.
    pub fn nudge(&mut self) -> Option<String> {
        if !self.armed() || self.nudged || self.run + 1 < self.stall_rounds || self.run == 0 {
            return None;
        }
        self.nudged = true;
        let mut msg = format!(
            "The harness is watching what your tool calls return, not how many you \
             make, and {}.",
            self.window_summary()
        );
        match self.repeats_clause() {
            Some(r) => msg.push_str(&format!(" Calls {r}.")),
            None => msg.push_str(
                " No call was repeated, so the questions were new and nothing could \
                 answer them.",
            ),
        }
        // **Say what happens next.** The version before this one invited the model
        // to "say so and carry on", which promised latitude the harness does not
        // grant: one more stalled round and the turn stops regardless. A steer that
        // offers a choice it cannot honour is the same defect one layer up as a gate
        // returning *allowed* because nothing is wired.
        msg.push_str(
            "\n\nIf you already have what you need, answer now. If you do not, ask a \
             *different* question \u{2014} the same one returns the same bytes. If the \
             next round also produces nothing new the turn stops and the operator is \
             shown this, so say in that round what you are still looking for: an \
             operator who can see what you were after can widen the search, and one \
             who cannot see it can only see that you stopped.",
        );
        Some(msg)
    }

    /// What the stalled window contained, with its denominator.
    ///
    /// `docs/tool-design-brief.md` §2.2: the size of what was examined travels with
    /// the number. *"0"* on its own is indistinguishable from a window with no calls
    /// in it, which is a different fact and would be a different bug.
    pub fn window_summary(&self) -> String {
        let calls = self.window.len();
        let mut tally: Vec<(&'static str, usize)> = Vec::new();
        for o in &self.window {
            match tally.iter_mut().find(|(w, _)| *w == o.outcome) {
                Some((_, n)) => *n += 1,
                None => tally.push((o.outcome, 1)),
            }
        }
        let tally = tally
            .iter()
            .map(|(w, n)| format!("{n} {w}"))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "the last {} round{} ran {calls} call{} and 0 of {calls} returned a result \
             this turn had not already seen ({tally})",
            self.run,
            if self.run == 1 { "" } else { "s" },
            if calls == 1 { "" } else { "s" },
        )
    }

    /// `` `read path=src/main.rs` ×3, `grep pattern=foo` ×2 ``, or `None` when no
    /// call in the window had been made before — a real and different state, and the
    /// one where the model was asking new questions that nothing could answer.
    ///
    /// The count is **calls this turn**, not calls in the window, and it is worded
    /// that way where it is printed. The window is the last few rounds; the third
    /// `read` of a file is evidence whether or not the first two are still inside it.
    pub fn repeats_clause(&self) -> Option<String> {
        let mut counts: Vec<(String, usize)> = Vec::new();
        for o in self.window.iter().filter(|o| o.repeat_call) {
            if counts.iter().any(|(l, _)| *l == o.label) {
                continue;
            }
            let n = self.calls.get(&o.key).map(|(_, n)| *n).unwrap_or(2);
            counts.push((o.label.clone(), n));
        }
        if counts.is_empty() {
            return None;
        }
        counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        counts.truncate(4);
        Some(format!(
            "already called this turn: {}",
            counts
                .iter()
                .map(|(l, n)| format!("`{l}` \u{d7}{n}"))
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }

    /// The stop, as a sentence naming what it saw.
    pub fn evidence(&self, rounds_run: usize, backstop: usize) -> String {
        let mut s = format!(
            "stopped after {rounds_run} round{}: {}",
            if rounds_run == 1 { "" } else { "s" },
            self.window_summary()
        );
        if let Some(r) = self.repeats_clause() {
            s.push_str("; ");
            s.push_str(&r);
        }
        s.push_str(&format!(
            ". The {backstop}-round backstop was not the reason — this is the \
             progress check. If the turn was working, raise --stall-rounds."
        ));
        s
    }
}

/// The outcome, as one word, for the tally.
///
/// Deliberately not `Debug`: `NotRun { why: "…" }` would put a whole sentence into
/// a count, and this is the axis a reader scans.
fn outcome_word(outcome: &ToolOutcome) -> &'static str {
    match outcome {
        ToolOutcome::Ok => "ok",
        ToolOutcome::Abstained { .. } => "abstained",
        ToolOutcome::Failed { .. } => "failed",
        ToolOutcome::Denied { .. } => "denied",
        ToolOutcome::Timeout => "timed out",
        ToolOutcome::NotRun { .. } => "not run",
        ToolOutcome::Backgrounded { .. } => "backgrounded",
    }
}

/// `read path=src/main.rs` — the call as a person would name it.
///
/// Every scalar argument, in the order the model wrote them, rather than a guess at
/// which one is the interesting one. A guess would be wrong exactly on the tools
/// nobody anticipated, and the label is evidence: it has to be the call that ran.
fn label(call: &ToolCall) -> String {
    let mut s = call.name.clone();
    if let Ok(serde_json::Value::Object(map)) =
        serde_json::from_str::<serde_json::Value>(&call.arguments)
    {
        let mut parts = Vec::new();
        for (k, v) in map.iter() {
            let rendered = match v {
                serde_json::Value::String(t) => t.clone(),
                serde_json::Value::Number(n) => n.to_string(),
                serde_json::Value::Bool(b) => b.to_string(),
                // An array or an object would be the whole payload of a `write`.
                _ => continue,
            };
            parts.push(format!("{k}={}", clip(rendered.trim(), 40)));
        }
        if !parts.is_empty() {
            s.push(' ');
            s.push_str(&parts.join(" "));
        }
    }
    clip(&s, 90)
}

/// Truncate on a char boundary. A label is built from model-supplied text, so a
/// byte slice would panic on the first non-ASCII path anybody has.
fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.replace(['\n', '\r'], " ");
    }
    let head: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", head.replace(['\n', '\r'], " "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, args: &str) -> ToolCall {
        ToolCall {
            id: "c0".into(),
            name: name.into(),
            arguments: args.into(),
        }
    }

    fn ok() -> ToolOutcome {
        ToolOutcome::Ok
    }

    #[test]
    fn new_work_every_round_never_stalls() {
        // The defect this module exists for: an investigation is long and every
        // round of it is legitimate. Twenty rounds of it must not be stopped.
        let mut d = ProgressDetector::new(5);
        for i in 0..20 {
            d.observe(&call("read", &format!(r#"{{"path":"f{i}"}}"#)), &ok(), &format!("body {i}"));
            assert_eq!(d.end_round(), Round::Progress, "round {i}");
        }
        assert!(!d.exhausted());
        assert_eq!(d.stalled_run(), 0);
    }

    #[test]
    fn re_reading_an_unchanged_file_stops_the_turn_and_names_the_file() {
        // The first read is progress: those bytes were new. The three after it are
        // not, which is exactly the run the detector is counting.
        let mut d = ProgressDetector::new(3);
        for _ in 0..4 {
            d.observe(&call("read", r#"{"path":"src/main.rs"}"#), &ok(), "the same body");
            d.end_round();
        }
        assert!(d.exhausted());
        let e = d.evidence(9, 200);
        assert!(e.contains("0 of 3"), "the denominator must travel: {e}");
        assert!(
            e.contains("read path=src/main.rs"),
            "the stop must name the call: {e}"
        );
        assert!(e.contains("\u{d7}4"), "and how many times it was made: {e}");
        assert!(
            !e.contains("without answering"),
            "never blame the model for working: {e}"
        );
    }

    #[test]
    fn a_read_after_an_edit_is_progress_even_though_the_call_repeats() {
        // The false positive that would matter most, and the reason the stop is on
        // payload novelty rather than on the repeated call.
        let mut d = ProgressDetector::new(2);
        d.observe(&call("read", r#"{"path":"a"}"#), &ok(), "before");
        assert_eq!(d.end_round(), Round::Progress);
        d.observe(&call("edit", r#"{"path":"a"}"#), &ok(), "edited 1 line");
        assert_eq!(d.end_round(), Round::Progress);
        d.observe(&call("read", r#"{"path":"a"}"#), &ok(), "after");
        assert_eq!(
            d.end_round(),
            Round::Progress,
            "the same call returned different bytes, which is the whole point"
        );
        assert!(!d.exhausted());
    }

    #[test]
    fn a_retry_after_a_transient_failure_is_progress() {
        let mut d = ProgressDetector::new(3);
        d.observe(
            &call("bash", r#"{"command":"curl x"}"#),
            &ToolOutcome::Failed { reason: "connection reset".into() },
            "connection reset",
        );
        assert_eq!(d.end_round(), Round::Stalled);
        d.observe(&call("bash", r#"{"command":"curl x"}"#), &ok(), "200 OK");
        assert_eq!(d.end_round(), Round::Progress);
        assert_eq!(d.stalled_run(), 0, "one success clears the run");
    }

    #[test]
    fn a_run_of_abstentions_stalls_and_says_the_queries_were_new() {
        let mut d = ProgressDetector::new(3);
        for i in 0..3 {
            d.observe(
                &call("grep", &format!(r#"{{"pattern":"p{i}"}}"#)),
                &ToolOutcome::Abstained { reason: "no match".into() },
                &format!("0 of 20431 lines, pattern p{i}"),
            );
            d.end_round();
        }
        assert!(d.exhausted());
        assert!(d.repeats_clause().is_none(), "no call was repeated here");
        let e = d.evidence(3, 200);
        assert!(e.contains("3 abstained"), "the tally is the evidence: {e}");
    }

    #[test]
    fn the_nudge_fires_one_round_before_the_stop_and_only_once() {
        let mut d = ProgressDetector::new(3);
        d.observe(&call("read", r#"{"path":"a"}"#), &ok(), "same");
        assert_eq!(d.end_round(), Round::Progress, "the first read was new");
        assert!(d.nudge().is_none());
        d.observe(&call("read", r#"{"path":"a"}"#), &ok(), "same");
        d.end_round();
        assert!(d.nudge().is_none(), "one stalled round is normal");
        d.observe(&call("read", r#"{"path":"a"}"#), &ok(), "same");
        d.end_round();
        let n = d.nudge().expect("the round before the stop");
        assert!(n.contains("read path=a"), "the nudge names the call too: {n}");
        assert!(!d.exhausted(), "the nudge is a correction, not the stop");
        d.observe(&call("read", r#"{"path":"a"}"#), &ok(), "same");
        d.end_round();
        assert!(
            d.nudge().is_none(),
            "one is the error signal, two is the harness insisting"
        );
        assert!(d.exhausted());
    }

    #[test]
    fn zero_disarms_and_says_so() {
        let mut d = ProgressDetector::new(0);
        assert!(!d.armed());
        for _ in 0..50 {
            d.observe(&call("read", r#"{"path":"a"}"#), &ok(), "same");
            d.end_round();
        }
        assert!(!d.exhausted(), "off is off; the backstop is the only stop");
        assert!(d.nudge().is_none());
    }

    #[test]
    fn a_mixed_round_with_one_new_result_is_progress() {
        // Three repeats and one novel call in the same round: the turn is working.
        let mut d = ProgressDetector::new(2);
        d.observe(&call("read", r#"{"path":"a"}"#), &ok(), "a");
        d.end_round();
        d.observe(&call("read", r#"{"path":"a"}"#), &ok(), "a");
        d.observe(&call("read", r#"{"path":"b"}"#), &ok(), "b");
        assert_eq!(d.end_round(), Round::Progress);
    }

    #[test]
    fn labels_survive_non_ascii_and_do_not_carry_a_payload() {
        let l = label(&call("write", r#"{"path":"файл.rs","content":"…"}"#));
        assert!(l.contains("path=файл.rs"), "{l}");
        let long = "x".repeat(500);
        let l = label(&call("bash", &format!(r#"{{"command":"{long}"}}"#)));
        assert!(l.chars().count() <= 90, "the label is bounded: {}", l.chars().count());
    }

    #[test]
    fn two_identical_calls_in_one_round_are_a_repeat() {
        let mut d = ProgressDetector::new(1);
        d.observe(&call("read", r#"{"path":"a"}"#), &ok(), "a");
        d.observe(&call("read", r#"{"path":"a"}"#), &ok(), "a");
        assert_eq!(
            d.end_round(),
            Round::Progress,
            "the first of the two was new, so the round did produce something"
        );
        d.observe(&call("read", r#"{"path":"a"}"#), &ok(), "a");
        assert_eq!(d.end_round(), Round::Stalled);
        assert_eq!(
            d.repeats_clause().as_deref(),
            Some("already called this turn: `read path=a` ×3"),
            "the count is calls this turn, and it is worded that way"
        );
    }
}
