//! Compaction as one more turn, not a special shape (C1).
//!
//! The operator's direction, 2026-09-13: *"compaction is a normal message that
//! says give summary so kv cache is reused … for now we dont have anything so
//! simple cache is ok."*
//!
//! The measured stakes are opencode's `1ee74df`/`4efae6c`, on this box: flatten
//! compaction of a 144,436-token conversation ran with `n_prompt_tokens_cache =
//! 0` and spent about 13 minutes prefilling at 182 t/s before the first summary
//! token, because a flattened conversation shares no prefix with anything — not
//! the system prompt, not the tool definitions, not the message framing. The
//! cached strategy re-sends the request that just ran and appends the
//! instruction as one more message, so the prefix is byte-identical to what the
//! previous turn already prefilled and only the suffix is new.
//!
//! **This engine gets the strategy by construction.** A turn's prompt is the
//! ledger's whole token region, and the prefix invariant ([`crate::prefix`])
//! proves each turn's prompt extends the previous turn's prompt-plus-generation
//! on the wire. So appending the instruction as a [`TranscriptItem`] and running
//! a normal turn *is* the cached strategy: the model is asked for the summary
//! over a prefix the server is already holding. The turn's own check measures
//! the reuse, and [`CompactionOutcome`] carries the numbers so a caller can
//! disclose them rather than assume them.
//!
//! What this module deliberately does **not** do:
//!
//! * **Build the new base.** Replacing the resident history with the summary —
//!   a new `StablePrefix`, a fresh ledger, and the one cold prefill that
//!   `docs/compaction.md` §3 correctly says is unavoidable — is the caller's
//!   act, because it reaches the store, the resume chain and the session
//!   registry, and this module has none of them. The outcome carries what that
//!   act needs.
//! * **Choose the trigger.** `/compact` from a head, or the wall — later, and
//!   auto-at-the-wall wants `docs/compaction.md` §1's policy rather than a
//!   constant.
//! * **The structural map.** §4's zoomable map is the refinement this simple
//!   version steps aside for; the instruction below already asks for the shape
//!   a map would keep — events, decisions, outcomes — so the summary this
//!   version produces is the material that map would be built from.

use letibot_transcript::{SystemOrigin, TranscriptItem};

use crate::engine::{Session, TurnEngine, TurnFailure};
use crate::events::EventSink;

/// What is appended as the compaction turn's instruction.
///
/// Worded for what `docs/compaction.md` §4 argues a summary loses: the events.
/// A prose retelling flattens `read src/foo.rs:1-200`, `cargo test → 3
/// failures`, `decided A because B` into English; the ask below keeps them as
/// facts. It also says *do not call tools*, because a summary turn that starts
/// working is not a summary — and if the model does anyway, the outcome
/// surfaces the calls instead of swallowing them.
pub const SUMMARY_INSTRUCTION: &str = "\
The conversation above is long, and this message asks for its summary, which will \
stand in for everything said before it. Write a compact factual record, not prose: \
every decision taken and the reason for it; every file created or changed and what \
the change was; every command run and its outcome; every number, name and path that \
is still needed; every question left open. Drop tool output bodies and reasoning. \
Do not call tools. Answer with the record and nothing else.";

/// What a compaction turn leaves behind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionOutcome {
    pub turn_id: String,
    /// The summary text, verbatim as the model wrote it. Nothing here has edited
    /// it; the caller builds the new base from it.
    pub summary: String,
    /// Tool calls the summary turn proposed. A summary turn has no business
    /// calling tools; surfaced rather than swallowed, and a caller that is about
    /// to trust the summary should refuse on a non-zero count.
    pub tool_calls: usize,
    /// `cached_tokens` from the turn's metrics: how much of the prefix the
    /// server says it reused. This is the point of the whole design, and it is
    /// disclosed rather than assumed — on the hybrid/recurrent model this box
    /// serves, the checkpoint policy can make it fall short of `reusable`
    /// without anything being wrong (see [`crate::prefix`]).
    pub cached_tokens: u64,
    /// What the prefix invariant permitted the server to reuse: the previous
    /// turn's prompt plus its committed generation. The gap between this and
    /// `cached_tokens` is the server's, not ours — the prompts are proven
    /// identical over the span.
    pub reusable: u64,
    /// Tokens the summary turn generated — reasoning included. The summary's own
    /// size in the new base is at most the visible part of this.
    pub generated_tokens: u64,
}

/// Run the compaction turn: append [`SUMMARY_INSTRUCTION`] as a system update
/// and take the turn that answers it.
///
/// The instruction is `SystemOrigin::Update` rather than a user message: it is
/// appended after the cached history instead of rewriting anything (the same
/// cache reason the transcript crate records on that variant), it is
/// distinguishable in the store from a turn anybody typed, and the next
/// session's base will not carry it — the summary replaces the span it asked
/// about, instruction included.
///
/// A failed turn fails the compaction: [`TurnFailure`] comes straight out, and
/// nothing about the session has been reduced — the instruction stays in the
/// ledger, which is harmless and honest, because the next compaction attempt
/// appends a fresh instruction over it.
pub fn run_compaction(
    engine: &mut TurnEngine<'_>,
    session: &mut Session,
    sink: &mut dyn EventSink,
) -> Result<CompactionOutcome, TurnFailure> {
    let instruction = TranscriptItem::System {
        text: SUMMARY_INSTRUCTION.to_string(),
        origin: SystemOrigin::Update,
    };
    session
        .append_items(engine, &[instruction], sink)
        .map_err(TurnFailure::from)?;

    let ok = engine.run_turn(session, sink)?;

    let mut summary = String::new();
    let mut tool_calls = 0usize;
    for item in &ok.items {
        match item {
            TranscriptItem::Assistant { text, tool_calls: c, .. } => {
                summary.push_str(text);
                tool_calls += c.len();
            }
            _ => {}
        }
    }
    let reusable = match &ok.metrics.prefix_check {
        crate::prefix::PrefixCheck::Held {
            expected_cached_min, ..
        } => *expected_cached_min,
        // FirstTurn and Skipped both mean nobody measured a reuse: a session
        // with no previous turn has nothing to reuse, and a skipped check is
        // reported as a zero, never as a pass.
        _ => 0,
    };
    Ok(CompactionOutcome {
        turn_id: ok.turn_id,
        summary,
        tool_calls,
        cached_tokens: ok.metrics.cached_tokens,
        reusable,
        generated_tokens: ok.metrics.predicted_tokens,
    })
}
