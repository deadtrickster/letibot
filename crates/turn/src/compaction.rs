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

//! # The cache and the log are two different things
//!
//! The operator, after watching a batched rewrite get reverted: *"kv cache is a
//! one thing and conversation log is another"*.
//!
//! This is the distinction the whole evening turned on and I kept collapsing.
//! The LOG -- the ledger and its items -- is the record of what was said, and it
//! is the truth. The KV CACHE is an optimisation held by a server process:
//! per-slot, evictable, gone on a restart, and shared with whatever else is
//! running. Treating them as one fact produces two opposite mistakes, and both
//! were made here in one night.
//!
//! Designing the log AROUND the cache is the first: the ordinary compaction
//! appends one message so the server's copy stays valid, which is right, but it
//! is a performance property and not a correctness one, and reading it as a
//! constraint is what made a scratch session look impossible.
//!
//! Dismissing the cache because the log is fine is the second: a scratch session
//! is perfectly correct and cost a cold quarter-million-token prefill, ten
//! minutes before a summary began. Correct and unusable.
//!
//! # The strategy that is right depends on the backend
//!
//! The operator again, having got there faster than this crate did: *"if my
//! prefill was fast enough we would be sending first half and then
//! firsthalf_summary+second half. but this mangles kv cache. we can have it for
//! cloud models"*.
//!
//! That is a third strategy and it is the cleanest of the three. Summarise the
//! first half; then make the next prompt `first_half_summary ++ second_half`.
//! Nothing overlaps, nothing is summarised twice, and the order is the order it
//! happened in. What it costs is the cache: that prompt is a prefix of nothing
//! the server holds, so it prefills cold from token zero.
//!
//! Which is disqualifying HERE and not in general, and the reason is worth
//! stating precisely rather than as "cloud has no cache" -- it does. DeepSeek
//! prices cached input at a TENTH of uncached, automatically and by prefix; the
//! operator's own `providers.toml` carries the two rates. So a cold prompt costs
//! something on both sides. What differs is the CURRENCY.
//!
//! ```text
//! local llama.cpp   a miss costs TIME    200-375 tok/s -> ~10 minutes
//!                                    on a 240k prompt
//! DeepSeek          a miss costs MONEY   0.28 vs 0.028 per Mtok ->
//!                                    $0.067 against $0.0067
//! ```
//!
//! Ten minutes before a word is generated disqualifies a strategy. Six cents
//! does not. And a cloud provider takes MESSAGES rather than tokens
//! (`letibot_provider::messages::convert`), so none of the prefix machinery --
//! the stable prefix, the dialect render, the token replay -- applies to it at
//! all. There, the clean strategy is simply the right one.
//!
//! So the branch is on the BACKEND, not on the conversation:
//!
//!   * local server  -> the two-half plan below, which keeps the expensive half
//!     a true prefix of what the server already holds;
//!   * cloud provider -> summarise, fold, continue; no overlap, no seam, no
//!     duplicated region.
//!
//! Only the first is implemented. The second is a smaller piece of work than the
//! first and is written down here so it is a choice somebody makes rather than a
//! gap somebody finds.
//!
//! Held apart, the overrun plan below reads as two separate judgements rather
//! than one muddle. Summarising the old half is a prompt that is a true PREFIX of
//! the live token stream, so the cache serves it -- a cache judgement. Summarising
//! both halves rather than keeping one verbatim is what makes the new base small
//! enough to buy real runway -- a log judgement. And the overlap costs cache
//! reuse on the tail, which is free precisely because the tail was never cached.

use letibot_dialect::StablePrefix;
use letibot_transcript::{SystemOrigin, TranscriptItem, UserPart};

use crate::engine::{Session, TurnEngine, TurnFailure, TurnOk};
use crate::events::{EventSink, NullSink};
use crate::length::EmptyReason;

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

/// What is appended when the summary turn stops inside its own reasoning block
/// and says nothing (R7).
///
/// The same steering the tool loop gives (`harnessd`'s `run_rounds`), with the ask
/// carried the way the `EmptyLength` arm carries it: a compaction turn that
/// reasons past its budget is exactly the turn that must hear *put the answer
/// first*, because the summary is the only thing this turn is for. Exported so a
/// test can assert the notice itself, not just that something was appended.
///
/// **Worded as the operator worded it**, because theirs is the only version with
/// evidence behind it. Measured in the transcript, 2026-09-18: three machine
/// notices in a row -- "continue or say why not", then "put the answer before
/// the reasoning if you are close to the limit", then the first again --
/// produced three more empty turns. The operator then typed "you keep
/// overthinking cut it short and do things", and the next reasoning block opened
/// "The operator is frustrated. Let me cut it short and just do the thing",
/// followed by the work, in 34 tokens of thinking instead of thousands.
///
/// What was wrong with the old ones: "continue or say why not" is an OPEN
/// QUESTION, and a model that has just overthought accepts the invitation to
/// deliberate; "if you are close to the limit" is a CONDITION it must evaluate,
/// which is more thinking, and it is already true. Neither said the thing that
/// worked. Imperative, short, no question, no condition.
pub const UNFINISHED_REASONING_NOTICE: &str = "\
You keep overthinking. Cut it short and do things. Your last turn thought until it ran \
out and said nothing. Write the summary now, first, before any reasoning.";

/// What is appended when the summary turn hits the output limit with nothing
/// usable in it (§5.7's hard fail, given the loop's salvage).
///
/// Worded from the harness's `EmptyLength` arm with one word changed: the answer
/// this turn owes is the summary.
fn empty_length_notice(reason: EmptyReason) -> String {
    format!(
        "You keep overthinking. Cut it short and do things. Your last turn spent its \
         whole output on reasoning ({}) and nothing was recorded. Write the summary now, \
         first, before any reasoning.",
        reason.as_str()
    )
}

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
    /// **The model ran out of room before it finished the summary.**
    ///
    /// The engine already stamps this on the assistant item from the finish
    /// reason, and this function used to discard it with a `..` — so a summary
    /// cut off mid-word was committed as the new base looking whole, and the
    /// only way to find out was to read it. Measured 2026-09-18: a summary of a
    /// 260390-token conversation ended at "`super::quarantine(call_id,
    /// &page.final_url, &page" and became the entire record of everything before
    /// it.
    ///
    /// Surfaced, not refused, and the distinction is the operator's: a session at
    /// the wall has nowhere else to go, so an incomplete record that SAYS it is
    /// incomplete beats both a silent one and a refusal to compact at all. The
    /// caller writes that sentence into the base.
    pub truncated: bool,
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
/// A failed turn fails the compaction — but the two *say-nothing* failures get
/// the same bounded salvage the tool loop gives them, because a compaction turn
/// is a turn and GLM ends those mid-thought exactly as it ends any other. On
/// [`TurnFailure::UnfinishedReasoning`] and [`TurnFailure::EmptyLength`] the
/// notice is appended as one more user item and the turn is taken again; the
/// engine's own salvage budget bounds the retries (each failed turn spends one
/// unit, and a spent budget comes back as [`TurnFailure::SalvageExhausted`],
/// which is not one of the two and so falls through to the caller). When the
/// budget is spent the failure propagates: nothing about the session has been
/// reduced, and the caller's `auto_compact_failed` — or `/compact`'s error —
/// still says an honest thing.
///
/// Nothing about the session has been reduced on the failure paths: the
/// instruction stays in the ledger, which is harmless and honest, because the
/// next compaction attempt appends a fresh instruction over it. The salvage
/// notices ride in the same span — the summary replaces everything before it,
/// instruction and notices included.
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

    // The salvage loop. A user item, not a system update: this is the harness
    // talking to the model mid-compaction, the same shape the tool loop's
    // `append_notice` appends, and a system update here would sit in the ledger
    // looking like an instruction the operator never saw. The failed turn
    // committed nothing (§5.7 and R7 both commit no items), so the notice lands
    // directly after the instruction and the retry's prompt extends the bytes
    // the server already prefilled — the prefix reuse the auto-compact message
    // promises survives the salvage.
    // **The summary turn does not think.**
    //
    // It runs when the window is nearly full -- that is the only time it runs --
    // and a lead that opens `<think>` spends what little is left before writing a
    // word of summary. Measured 2026-09-18 at 260390 of 262144: 1754 tokens of
    // room, four salvages, each one thinking first, and
    // `compaction FAILED: 4 consecutive length salvages; the cap is spent`.
    //
    // The whole loop is inside the scope, salvages included: a retry that thinks
    // again is the failure repeating itself with fewer tokens each time.
    let ok = engine.without_reasoning(|engine| -> Result<TurnOk, TurnFailure> {
        loop {
        match engine.run_turn(session, sink) {
            Ok(ok) => break Ok(ok),
            Err(TurnFailure::UnfinishedReasoning { .. }) => {
                append_salvage_notice(session, engine, sink, UNFINISHED_REASONING_NOTICE)?;
            }
            Err(TurnFailure::EmptyLength { reason, .. }) => {
                let notice = empty_length_notice(reason);
                append_salvage_notice(session, engine, sink, &notice)?;
            }
            // SalvageExhausted lands here, and so does everything that is not a
            // say-nothing turn: a guard trip, a socket error, a spent budget.
            // Propagating is the honest answer — a compaction that did not run
            // must say so, not disappear into a retry that never ends.
            Err(e) => return Err(e),
        }
        }
    })?;

    let Harvest { summary, tool_calls, truncated } = harvest(&ok.items);
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
        truncated,
        cached_tokens: ok.metrics.cached_tokens,
        reusable,
        generated_tokens: ok.metrics.predicted_tokens,
    })
}


/// What a summary turn produced, read off its items.
///
/// A free function because it is the part worth testing: `truncated` used to be
/// discarded here in a `..`, so a summary cut off mid-sentence was committed as
/// the new base looking whole. The engine stamps that flag from the finish
/// reason precisely so somebody downstream reads it, and the one caller that
/// most needed it was the one throwing it away.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Harvest {
    pub summary: String,
    pub tool_calls: usize,
    pub truncated: bool,
}

pub fn harvest(items: &[TranscriptItem]) -> Harvest {
    let mut h = Harvest::default();
    for item in items {
        if let TranscriptItem::Assistant { text, tool_calls, truncated, .. } = item {
            h.summary.push_str(text);
            h.tool_calls += tool_calls.len();
            // ANY truncated part truncates the whole: the summary is the
            // concatenation, so a cut in the middle is a cut in the result.
            h.truncated |= *truncated;
        }
    }
    h
}

#[cfg(test)]
mod harvesting_a_summary {
    use super::*;

    fn assistant(text: &str, truncated: bool) -> TranscriptItem {
        TranscriptItem::Assistant {
            text: text.into(),
            tool_calls: vec![],
            truncated,
        }
    }

    #[test]
    fn a_whole_summary_is_not_marked_cut() {
        let h = harvest(&[assistant("# Session Record\n…complete…", false)]);
        assert!(!h.truncated);
        assert_eq!(h.tool_calls, 0);
        assert!(h.summary.contains("complete"));
    }

    /// The measured case: the model ran out of room mid-expression.
    #[test]
    fn a_summary_that_ran_out_of_room_is_marked_cut() {
        let h = harvest(&[assistant("…`super::quarantine(call_id, &page.final_url, &page", true)]);
        assert!(h.truncated, "the flag the engine stamped must survive the harvest");
    }

    /// One cut part cuts the whole, because the summary is their concatenation.
    #[test]
    fn a_cut_anywhere_cuts_the_result() {
        let h = harvest(&[assistant("first ", false), assistant("second", true)]);
        assert!(h.truncated);
        assert_eq!(h.summary, "first second");
    }
}

/// Append one salvage notice as a user item, the tool loop's `append_notice`
/// shape minus the trail this crate does not have.
///
/// The trail is the harnessd side's job (`Speaker::Agent` there marks the notice
/// as the harness's own words in the authorisation ledger); what the turn crate
/// owes is the transcript row, and a user row is what the model reads as a
/// prompt. Appending through [`Session::append_items`] keeps the ledger, the
/// token region and the `TranscriptAppended` event in step with every other row.
fn append_salvage_notice(
    session: &mut Session,
    engine: &TurnEngine<'_>,
    sink: &mut dyn EventSink,
    text: &str,
) -> Result<(), TurnFailure> {
    let item = TranscriptItem::User {
        parts: vec![UserPart::Text { text: text.into() }],
    };
    session
        .append_items(engine, &[item], sink)
        .map_err(TurnFailure::from)
}

/// **How a conversation that arrived over the budget gets compacted.**
///
/// The ordinary path appends [`SUMMARY_INSTRUCTION`] to the conversation itself,
/// which is right and cheap: the server already holds the prompt, so the prefill
/// is one message. It needs one thing to be true — that there is room left to
/// write the summary into — and that is exactly what is false here.
///
/// The operator, having watched a batched rewrite fail six ways and reset the
/// rules: *"overrun is when it arrives over the budget, either unlucky tool like
/// a file read or model change, chunk is whatever needed for S_old to be
/// summarizable again with some margin"*.
///
/// So this is the fallback and only the fallback. The conversation is cut once,
/// near the end: everything before the cut is summarised, the tail after it is
/// summarised, and the two summaries become the next turn's history in that
/// order. Both halves are summarised — an earlier attempt kept the tail verbatim
/// and produced a 44197-token base where the ordinary path produces about 7500,
/// which against a model spending 79000-118000 tokens a round is one round of
/// runway instead of ten.
///
/// # Where the cut goes, and why the cache survives it
///
/// The cut is the SMALLEST tail whose removal leaves the rest summarisable: the
/// largest `k` with `prefix + Σ items[0..k] + write_room + margin ≤ window`. Take
/// less and the old half still does not fit; take more and the tail is summarised
/// coarsely for no reason.
///
/// That choice is what keeps the prefix intact in the sense that matters.
/// Summarising the old half is a prompt of `prefix ++ items[0..k]`, which is a
/// true PREFIX of the live session's token stream — the server has those tokens
/// and reuses them. Only the tail's prompt is one the server has not seen, and
/// the tail is by construction just the excess. The expensive half is cached; the
/// cold half is small.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverrunPlan {
    /// There is room to summarise in place; use the ordinary path.
    NotOverrun,
    /// Cut here. `items[..cut]` is summarised as the old half, `items[tail_from..]`
    /// as the tail, and the two summaries become the new history in that order.
    ///
    /// **`tail_from` is at or before `cut`, so the two halves OVERLAP.** The
    /// operator's idea: *"to have it interesting you can do overlapped 2/3 top
    /// and then bottom ... so some context crossection happens, but it is just an
    /// idea on how to preserve contexrt relationships"*.
    ///
    /// A clean boundary loses precisely the relationships that span it: a
    /// decision taken just before the cut and revised just after is summarised
    /// twice, and neither summary can see what the other is looking at. With an
    /// overlap both summaries read the boundary region, so whichever one the
    /// relationship belongs to has the context to state it.
    ///
    /// It is close to free. `cut` is chosen so the OLD half only just fits, which
    /// leaves the tail — by construction only the excess — far inside its own
    /// budget. Extending the tail backwards spends room that was already reserved
    /// and would otherwise go unused.
    Cut {
        cut: usize,
        tail_from: usize,
        old_tokens: u64,
        tail_tokens: u64,
    },
    /// Even an empty conversation does not leave room to write a summary, so no
    /// cut can help: the prefix itself is too big for the window.
    Hopeless { prefix_tokens: u64, window: u64 },
}

/// **What decides overrun: the least room the in-place summary needs.**
///
/// This is NOT the same number as the trigger's headroom, and using one number
/// for both was the bug that sent every ordinary compaction down the slow path.
/// The trigger fires at `resident + headroom >= window` with headroom 16384; the
/// overrun test was `resident + 16384 > window`; so at the trigger point -- the
/// normal case, 247301 of 262144 with 14.8k of room -- overrun was true by
/// construction and the in-session path never ran. Measured 2026-09-19 on the
/// operator's leticl session: a compaction that used to take seconds became two
/// cold scratchpad prefills, minutes each, with the head shown the scratchpad's
/// token count as its own context. "compaction still extremely broken."
///
/// What the in-place summary actually needs is the instruction plus the record.
/// Every compaction summary in the store is between 2107 and 5895 tokens, and the
/// summary turn is handed a closed reasoning block so nothing is spent thinking.
/// 8192 covers the largest seen with room to spare; below it, the 1754-token
/// case that produced a truncated record is what overrun is for.
pub const MIN_SUMMARY_ROOM: u64 = 8_192;

/// Room a scratchpad summary turn is given to write in, when planning the halves.
/// Generous on purpose, and separate from the decision above: the failure being
/// avoided here is a half that runs out, and erring wide only costs a smaller
/// tail.
pub const WRITE_ROOM: u64 = 16_384;

/// Slack on top, so a cut that only just works is not chosen. "with some margin".
const CUT_MARGIN: u64 = 8_192;

pub fn plan_overrun(item_tokens: &[u64], prefix_tokens: u64, window: u64) -> OverrunPlan {
    let resident = prefix_tokens + item_tokens.iter().sum::<u64>();
    if resident + MIN_SUMMARY_ROOM <= window {
        return OverrunPlan::NotOverrun;
    }
    if prefix_tokens + WRITE_ROOM + CUT_MARGIN >= window {
        return OverrunPlan::Hopeless { prefix_tokens, window };
    }

    // The largest prefix of the history that still leaves room to write about it.
    let budget = window - WRITE_ROOM - CUT_MARGIN;
    let mut acc = prefix_tokens;
    let mut cut = 0usize;
    for (i, t) in item_tokens.iter().enumerate() {
        if acc + t > budget {
            cut = i;
            break;
        }
        acc += t;
        cut = i + 1;
    }

    // A single item larger than the whole budget cannot be cut around: summarise
    // everything before it and let it be the tail on its own, rather than
    // refusing and leaving the session stuck.
    if cut == 0 {
        cut = 1.min(item_tokens.len());
    }

    // The tail reaches back PAST the cut, deliberately -- see `OverrunPlan::Cut`
    // -- but only so far. Reaching back as far as the budget allowed made both
    // halves 230000 tokens of a 237000-token conversation: the same conversation
    // summarised twice, at twice the cost, which is not an overlap but a
    // duplicate. The operator's shape is "2/3 top and then bottom", so the
    // overlap is a THIRD of the old half at most, and less when the tail's own
    // budget says so.
    let tail_budget = window - WRITE_ROOM - CUT_MARGIN - prefix_tokens;
    let overlap_cap = cut / 3;
    let floor = cut.saturating_sub(overlap_cap);
    let mut tail_acc = 0u64;
    let mut tail_from = item_tokens.len();
    while tail_from > floor {
        let next = tail_acc + item_tokens[tail_from - 1];
        if next > tail_budget {
            break;
        }
        tail_acc = next;
        tail_from -= 1;
    }

    OverrunPlan::Cut {
        cut,
        tail_from,
        old_tokens: item_tokens[..cut].iter().sum(),
        tail_tokens: item_tokens[tail_from..].iter().sum(),
    }
}

#[cfg(test)]
mod overrun_planning {
    use super::*;

    const W: u64 = 262_144;
    const P: u64 = 6_880;

    #[test]
    fn a_conversation_with_room_uses_the_ordinary_path() {
        let items: Vec<u64> = std::iter::repeat(1_000).take(100).collect();
        assert_eq!(plan_overrun(&items, P, W), OverrunPlan::NotOverrun);
    }

    /// **Past the trigger line with room to spare is the ordinary case.** The trigger
    /// fires at `resident + headroom >= window`, so every compaction arrives past
    /// that line; the wall lands wherever the last round stopped. The operator's
    /// leticl session on 2026-09-19: 247301 of 262144, 14.8k of room, every
    /// summary in the store under 6k -- and it was sent to the scratchpads.
    #[test]
    fn arriving_past_the_trigger_with_room_for_a_summary_is_not_an_overrun() {
        let resident = 247_301u64;
        let items: Vec<u64> = std::iter::repeat(1_000).take(((resident - P) / 1_000) as usize).collect();
        assert_eq!(
            plan_overrun(&items, P, W),
            OverrunPlan::NotOverrun,
            "14.8k of room is a normal compaction; the in-session summary runs there"
        );
    }

    /// And the case overrun exists for stays an overrun: 1754 tokens of room, the
    /// one that produced a record cut off mid-expression.
    #[test]
    fn the_case_that_truncated_a_summary_is_still_an_overrun() {
        let resident = 260_390u64;
        let items: Vec<u64> = std::iter::repeat(1_000).take(((resident - P) / 1_000) as usize).collect();
        assert!(matches!(plan_overrun(&items, P, W), OverrunPlan::Cut { .. }));
    }

    /// The measured case: arrives at ~260k of a 262k window, so the in-place
    /// summary has 1754 tokens to write in and cannot.
    #[test]
    fn a_conversation_that_arrived_over_the_budget_is_cut() {
        let items: Vec<u64> = std::iter::repeat(1_000).take(255).collect();
        let OverrunPlan::Cut { cut, tail_from, old_tokens, tail_tokens } = plan_overrun(&items, P, W) else {
            panic!("260k of a 262k window is an overrun");
        };

        // The old half fits, with room to write about it and the margin to spare.
        assert!(
            P + old_tokens + WRITE_ROOM + CUT_MARGIN <= W,
            "the old half must be summarisable: {old_tokens}"
        );
        // The tail is only the excess, not half the conversation.
        assert!(tail_tokens < old_tokens, "tail {tail_tokens} vs old {old_tokens}");
        // And the tail is itself summarisable, comfortably.
        assert!(P + tail_tokens + WRITE_ROOM <= W);
        // The halves overlap rather than abut: the tail starts at or before the
        // cut, and here it starts well before it.
        assert!(tail_from <= cut, "tail_from {tail_from} must not be past cut {cut}");
        assert_eq!(tail_from + (tail_tokens / 1_000) as usize, items.len());
        assert!(cut > tail_from, "there is a real overlap: {tail_from}..{cut}");
        // Bounded: an overlap is a seam, not a second copy of the conversation.
        assert!(
            cut - tail_from <= cut / 3 + 1,
            "the overlap is at most a third of the old half: {tail_from}..{cut}"
        );
        assert!(
            tail_tokens < old_tokens,
            "and the tail stays the smaller half: {tail_tokens} vs {old_tokens}"
        );
    }

    /// The cut is the SMALLEST one that works: one item further along and the old
    /// half would no longer fit.
    #[test]
    fn the_cut_is_the_smallest_tail_that_makes_the_rest_fit() {
        let items: Vec<u64> = std::iter::repeat(1_000).take(255).collect();
        let OverrunPlan::Cut { cut, .. } = plan_overrun(&items, P, W) else {
            panic!("overrun")
        };
        let one_less: u64 = P + items[..cut + 1].iter().sum::<u64>();
        assert!(
            one_less + WRITE_ROOM + CUT_MARGIN > W,
            "cutting one item later would not leave room, so this cut is minimal"
        );
    }

    /// A prefix that fills the window cannot be helped by cutting history.
    #[test]
    fn a_prefix_too_big_for_the_window_is_hopeless() {
        assert_eq!(
            plan_overrun(&[10, 10], 260_000, W),
            OverrunPlan::Hopeless { prefix_tokens: 260_000, window: W }
        );
    }

    /// One enormous tool result, bigger than the whole budget. It becomes the
    /// tail on its own rather than stalling compaction forever.
    #[test]
    fn a_single_item_larger_than_the_budget_becomes_the_tail() {
        let items = vec![W, 1_000];
        let OverrunPlan::Cut { cut, .. } = plan_overrun(&items, P, W) else {
            panic!("overrun")
        };
        assert_eq!(cut, 1, "summarise what is before it; it is the tail");
    }
}

/// **Summarise an over-budget conversation into a scratchpad, in two halves.**
///
/// `plan_overrun` decides where to cut; this carries it out. Two summary turns,
/// each in a session of its own on the SAME stable prefix — system and tool
/// schemas byte-identical, which is what "so prefix is intact" means and what
/// lets the server serve the expensive half from cache.
///
/// Order matters and is the operator's: the tail first, then everything before
/// it. The tail's prompt is one the server has not seen and is small; the old
/// half's prompt is a true prefix of the live token stream and is large. Running
/// the cheap cold one first leaves the cache the big one needs still warm.
///
/// The two summaries come back in conversation order — old, then tail — because
/// that is the order they happened in, and a reader that meets the recent past
/// before the distant one has to re-derive the sequence.
pub fn summarise_overrun(
    engine: &mut TurnEngine<'_>,
    prefix: &StablePrefix,
    scratch_id: &str,
    items: &[TranscriptItem],
    plan: &OverrunPlan,
    sink: &mut dyn EventSink,
) -> Result<Harvest, TurnFailure> {
    let OverrunPlan::Cut { cut, tail_from, .. } = plan else {
        return Ok(Harvest::default());
    };

    let tail = summarise_one(engine, prefix, &format!("{scratch_id}-tail"), &items[*tail_from..], sink)?;
    let old = summarise_one(engine, prefix, &format!("{scratch_id}-old"), &items[..*cut], sink)?;

    // The overlap is stated rather than hidden. A reader that finds the same
    // decision in both halves should know the halves were meant to share a seam,
    // or it will read the repetition as the conversation going round twice.
    let overlapped = cut.saturating_sub(*tail_from);
    let mut summary = String::new();
    summary.push_str(&old.summary);
    summary.push_str("\n\n");
    if overlapped > 0 {
        summary.push_str(&format!(
            "## The most recent {} exchange(s)\n\n_The record above and the one below \
             deliberately overlap by {overlapped} exchange(s), so that anything decided \
             across the seam is stated with its context on at least one side._\n\n",
            items.len() - tail_from
        ));
    } else {
        summary.push_str("## The most recent exchanges\n\n");
    }
    summary.push_str(&tail.summary);

    Ok(Harvest {
        summary,
        tool_calls: old.tool_calls + tail.tool_calls,
        truncated: old.truncated || tail.truncated,
    })
}

/// One summary turn over one slice, in a throwaway session.
fn summarise_one(
    engine: &mut TurnEngine<'_>,
    prefix: &StablePrefix,
    scratch_id: &str,
    slice: &[TranscriptItem],
    sink: &mut dyn EventSink,
) -> Result<Harvest, TurnFailure> {
    // `ProgressOnly`: the rows are a copy of history the operator has already
    // seen, so announcing them puts `waiting for the body of …` placeholders on
    // their screen for a transcript that is never persisted. The PREFILL is worth
    // showing -- it is minutes long and is the only honest answer to "what is it
    // doing".
    // Nothing to the head. Forwarding `PromptProgress` from here looked like a fix
    // for "tui doesnt show any prefill" and was worse: the head drew the scratch
    // prompt's token count as the SESSION's context, and the operator watched it
    // sit at "69k" over a 240k conversation that had not changed. Progress is the
    // caller's to report, in words, per half.
    let _ = sink;
    let mut quiet = NullSink;
    let mut scratch = engine.open(scratch_id, prefix).map_err(TurnFailure::from)?;
    scratch.append_items(engine, slice, &mut quiet).map_err(TurnFailure::from)?;
    let instruction = TranscriptItem::System {
        text: SUMMARY_INSTRUCTION.to_string(),
        origin: SystemOrigin::Update,
    };
    scratch.append_items(engine, &[instruction], &mut quiet).map_err(TurnFailure::from)?;

    let ok = engine.without_reasoning(|engine| -> Result<TurnOk, TurnFailure> {
        loop {
            match engine.run_turn(&mut scratch, &mut quiet) {
                Ok(ok) => break Ok(ok),
                Err(TurnFailure::UnfinishedReasoning { .. }) => {
                    append_salvage_notice(&mut scratch, engine, &mut quiet, UNFINISHED_REASONING_NOTICE)?;
                }
                Err(TurnFailure::EmptyLength { reason, .. }) => {
                    let notice = empty_length_notice(reason);
                    append_salvage_notice(&mut scratch, engine, &mut quiet, &notice)?;
                }
                Err(e) => return Err(e),
            }
        }
    })?;
    Ok(harvest(&ok.items))
}

/// **Summarise the first half, then continue on `summary ++ second half`.**
///
/// The operator's design for a backend whose prefill is cheap: *"if my prefill
/// was fast enough we would be sending first half and then
/// firsthalf_summary+second half"*.
///
/// It is the cleanest of the three strategies in this module and the one to
/// prefer wherever it can be afforded. Nothing overlaps, so nothing is summarised
/// twice and no seam has to be explained to the reader. The order is the order it
/// happened in: a record of the distant past, then the recent past VERBATIM --
/// not a summary of it, which is what the two-half plan settles for and what
/// loses the detail of whatever the session was in the middle of.
///
/// What it costs is the cache, and the cost is a different currency on each side:
///
///   * local llama.cpp -- the new prompt is a prefix of nothing the server holds,
///     so it prefills cold. Measured on this box at 200-375 tok/s: about ten
///     minutes for a quarter-million tokens, before a word is generated. That is
///     why `plan_overrun` exists and why it is uglier.
///   * a cloud provider -- there is no prefix machinery at all
///     (`letibot_provider::messages::convert` sends items as messages, never the
///     ledger's tokens), and a prefix-cache miss is priced rather than waited on:
///     DeepSeek charges 0.28 against 0.028 per million, so a 240k prompt is six
///     cents instead of seven tenths of one.
///
/// Six cents does not disqualify a strategy; ten minutes does.
///
/// # Where the split goes
///
/// At the halfway point BY TOKENS rather than by item count, since one tool
/// result can outweigh fifty exchanges. Both sides then have to be workable,
/// which for a conversation at the wall they comfortably are: the first half must
/// leave room to summarise it, and the second half plus that summary must leave
/// room to work in.
pub fn plan_fold(item_tokens: &[u64], prefix_tokens: u64, window: u64) -> Option<usize> {
    let total: u64 = item_tokens.iter().sum();
    if prefix_tokens + total + MIN_SUMMARY_ROOM <= window {
        return None;
    }
    let half = total / 2;
    let mut acc = 0u64;
    let mut split = 0usize;
    for (i, t) in item_tokens.iter().enumerate() {
        if acc + t > half {
            split = i;
            break;
        }
        acc += t;
        split = i + 1;
    }
    // Both halves have to be workable. The first must be summarisable; the second
    // must leave room for the summary in front of it and a turn after it.
    let first: u64 = item_tokens[..split].iter().sum();
    let second: u64 = item_tokens[split..].iter().sum();
    if prefix_tokens + first + WRITE_ROOM > window
        || prefix_tokens + second + WRITE_ROOM + CUT_MARGIN > window
    {
        return None;
    }
    (split > 0 && split < item_tokens.len()).then_some(split)
}

/// Carry out [`plan_fold`]: one summary turn over `items[..split]`, returned as
/// the record that stands in front of `items[split..]`.
///
/// The caller builds the new history as `System(summary) ++ items[split..]`. This
/// returns only the summary, because whether that system item is a fork, a
/// rewrite or a fresh session is the caller's business and not this crate's.
pub fn summarise_first_half(
    engine: &mut TurnEngine<'_>,
    prefix: &StablePrefix,
    scratch_id: &str,
    items: &[TranscriptItem],
    split: usize,
    sink: &mut dyn EventSink,
) -> Result<Harvest, TurnFailure> {
    summarise_one(engine, prefix, scratch_id, &items[..split], sink)
}

#[cfg(test)]
mod folding {
    use super::*;

    const W: u64 = 262_144;
    const P: u64 = 6_880;

    #[test]
    fn a_conversation_with_room_is_not_folded() {
        let items: Vec<u64> = std::iter::repeat(1_000).take(100).collect();
        assert_eq!(plan_fold(&items, P, W), None);
    }

    /// The split is halfway BY TOKENS, and both halves are workable.
    #[test]
    fn the_split_is_halfway_by_tokens() {
        let items: Vec<u64> = std::iter::repeat(1_000).take(255).collect();
        let split = plan_fold(&items, P, W).expect("255k of a 262k window folds");
        assert!((126..=129).contains(&split), "halfway, got {split}");

        let first: u64 = items[..split].iter().sum();
        let second: u64 = items[split..].iter().sum();
        assert!(P + first + WRITE_ROOM <= W, "the first half is summarisable");
        assert!(P + second + WRITE_ROOM <= W, "the second half leaves room to work");
    }

    /// One enormous item outweighs many small ones, and the split follows the
    /// tokens rather than the count.
    #[test]
    fn one_huge_item_moves_the_split() {
        let mut items = vec![200_000u64];
        items.extend(std::iter::repeat(1_000).take(55));
        // The first item alone is past half, so the split lands before it and the
        // first half would be empty -- there is no useful fold here.
        assert_eq!(plan_fold(&items, P, W), None);
    }
}
