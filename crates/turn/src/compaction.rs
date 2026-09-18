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

use letibot_dialect::StablePrefix;
use letibot_transcript::{SystemOrigin, TranscriptItem, UserPart};

use crate::engine::{Session, TurnEngine, TurnFailure, TurnOk};
use crate::events::EventSink;
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
pub const UNFINISHED_REASONING_NOTICE: &str = "\
Your previous turn ended inside a reasoning block and said nothing; continue or say \
why not. Answer again, and put the summary before the reasoning if you reason at all.";

/// What is appended when the summary turn hits the output limit with nothing
/// usable in it (§5.7's hard fail, given the loop's salvage).
///
/// Worded from the harness's `EmptyLength` arm with one word changed: the answer
/// this turn owes is the summary.
fn empty_length_notice(reason: EmptyReason) -> String {
    format!(
        "Your previous turn hit the output token limit with nothing usable in it ({}). \
         Nothing was recorded. Answer again, and put the summary before the reasoning \
         if you are close to the limit.",
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



/// **What compaction is going to do, decided before any of it is done.**
///
/// The operator's design, 2026-09-18: *"after prefill finished calculate how
/// many tokens left and just tell that we cant afford summary here and offer to
/// either replace distance past with a summary and then summarize the rest once
/// context freed ... or, if we are at the limit or nothing really left - say
/// so"*.
///
/// Three answers, because there are three situations and collapsing them is how
/// the useless message gets printed. The arithmetic happening FIRST is what
/// catches the loop: a plan that says "this batch is 65536 tokens and the window
/// is 262144" cannot enter the failure it replaces, where a summary turn ran with
/// 1754 tokens, said nothing, was asked to try again, and spent the salvage
/// budget four times over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompactionPlan {
    /// The conversation already fits the tail budget. Summarising it would spend
    /// turns to replace history with a shorter description of the same history.
    NothingToDo,
    /// **The prefix alone leaves no usable room, so no summary can help.**
    ///
    /// The system prompt and the tool schemas are message zero and compaction
    /// never touches them — rewriting them is what forces a cold re-prefill, and
    /// it is the one thing a fork keeps. So when they do not leave room for a
    /// conversation, shortening the conversation is not the lever, and reporting
    /// "nothing to summarise" is true and useless. What helps is a bigger window,
    /// a shorter system prompt, or fewer seated tools.
    Hopeless {
        prefix_tokens: u64,
        window: u64,
    },
    /// Summarise the distant past in batches and keep the recent past verbatim.
    Batched {
        /// Items `[0, summarise_before)` are replaced by summaries, oldest first.
        summarise_before: usize,
        /// How those are split. Each entry is a half-open range of item indices.
        batches: Vec<std::ops::Range<usize>>,
        /// Tokens the kept tail is carrying, verbatim.
        tail_tokens: u64,
        /// Tokens the summarised part was carrying, before summarising.
        summarised_tokens: u64,
    },
}

/// How much of the USABLE room one batch may occupy on its way in.
///
/// A quarter, so three quarters are left to write the record into. That is far
/// more than any record needs, and the generosity is the point: the failure this
/// replaces was a summary turn with 0.7% of the window, and a batch size chosen
/// to be nearly-enough would be the same bug with a bigger constant.
const BATCH_NUMERATOR: u64 = 1;
const BATCH_DENOMINATOR: u64 = 4;

/// How much recent history stays verbatim rather than becoming prose.
///
/// An eighth. Compaction used to keep NONE of it -- the new base was the summary
/// and nothing else -- which is why a compacted session reads as though it just
/// woke up: the turn it was in the middle of is now a sentence about a turn. The
/// distant past is what prose serves; the last few exchanges are what the model
/// is actually doing.
const TAIL_NUMERATOR: u64 = 1;
const TAIL_DENOMINATOR: u64 = 8;

/// The least usable room worth planning against. Below this the batches and the
/// tail are rounding errors and the honest answer is [`CompactionPlan::Hopeless`].
const MIN_USABLE: u64 = 1024;

/// Plan a compaction over `item_tokens`, the per-item token counts in order.
///
/// `prefix_tokens` is message zero — the system prompt and the tool schemas —
/// which compaction cannot touch and every plan must therefore subtract first.
/// Measured 2026-09-18: a planner that forgot to answered "this conversation
/// already fits" about a session whose prefix alone was over the window.
pub fn plan_compaction(item_tokens: &[u64], prefix_tokens: u64, window: u64) -> CompactionPlan {
    let usable = window.saturating_sub(prefix_tokens);
    if usable < MIN_USABLE {
        return CompactionPlan::Hopeless { prefix_tokens, window };
    }
    let tail_budget = usable * TAIL_NUMERATOR / TAIL_DENOMINATOR;
    let batch_budget = usable * BATCH_NUMERATOR / BATCH_DENOMINATOR;

    // Walk back from the newest, keeping what fits in the tail budget. The split
    // lands on an item boundary, never inside one: half a tool result is not a
    // tool result.
    let mut tail_tokens = 0u64;
    let mut split = item_tokens.len();
    while split > 0 {
        let next = tail_tokens + item_tokens[split - 1];
        if next > tail_budget {
            break;
        }
        tail_tokens = next;
        split -= 1;
    }
    if split == 0 {
        return CompactionPlan::NothingToDo;
    }

    // Everything older, cut into batches that fit the batch budget. An item
    // larger than the budget on its own still gets its own batch -- it cannot be
    // split, and refusing it would stall compaction on one big tool result.
    let mut batches = Vec::new();
    let mut start = 0usize;
    let mut acc = 0u64;
    for (i, t) in item_tokens[..split].iter().enumerate() {
        if acc > 0 && acc + t > batch_budget {
            batches.push(start..i);
            start = i;
            acc = 0;
        }
        acc += t;
    }
    if start < split {
        batches.push(start..split);
    }

    CompactionPlan::Batched {
        summarise_before: split,
        batches,
        tail_tokens,
        summarised_tokens: item_tokens[..split].iter().sum(),
    }
}

#[cfg(test)]
mod planning {
    use super::*;

    /// 262144, the window this box serves.
    const W: u64 = 262_144;

    #[test]
    fn a_conversation_that_fits_is_not_compacted() {
        assert_eq!(plan_compaction(&[1000, 1000, 1000], 2_000, W), CompactionPlan::NothingToDo);
    }

    /// **The case an existing test caught.** The prefix alone is past the window,
    /// the items are a few hundred tokens, and the first version of this planner
    /// answered "already fits" -- true about the items and useless about the
    /// problem. Nothing that summarises the CONVERSATION can help here.
    #[test]
    fn a_prefix_that_does_not_leave_room_is_hopeless_not_nothing_to_do() {
        let p = plan_compaction(&[10, 10, 10], 2_700, 512);
        assert_eq!(p, CompactionPlan::Hopeless { prefix_tokens: 2_700, window: 512 });
    }

    /// The measured case: ~260k across many items. The tail is kept, the rest is
    /// batched, and every batch fits the budget with room to write.
    #[test]
    fn the_distant_past_is_batched_and_the_recent_past_is_kept() {
        let items: Vec<u64> = std::iter::repeat(1_000).take(260).collect();
        let CompactionPlan::Batched { summarise_before, batches, tail_tokens, .. } =
            plan_compaction(&items, 2_000, W)
        else {
            panic!("260k needs compacting");
        };

        let usable = W - 2_000;
        assert!(tail_tokens <= usable / 8, "tail within its budget: {tail_tokens}");
        assert!(tail_tokens > 0, "some recent history is kept verbatim");
        assert_eq!(summarise_before + (tail_tokens / 1000) as usize, items.len());

        for b in &batches {
            let n: u64 = items[b.clone()].iter().sum();
            assert!(n <= usable / 4, "batch {b:?} is {n}, over budget");
        }
        assert_eq!(batches.first().unwrap().start, 0);
        assert_eq!(batches.last().unwrap().end, summarise_before);
        for w in batches.windows(2) {
            assert_eq!(w[0].end, w[1].start, "no gap and no overlap");
        }
    }

    /// An item bigger than a whole batch budget still gets summarised, alone. It
    /// cannot be split, and stalling compaction on one huge tool result is how a
    /// session becomes permanently uncompactable.
    #[test]
    fn an_oversized_item_gets_its_own_batch() {
        let items = vec![W, 1_000, 1_000];
        let CompactionPlan::Batched { batches, .. } = plan_compaction(&items, 2_000, W) else {
            panic!("oversized still plans");
        };
        assert_eq!(batches[0], 0..1);
    }
}

/// What a batch is asked for, as distinct from the whole conversation.
///
/// The difference matters in one place and it is the one a reader trips on: a
/// batch summary is not the record of a session, it is the record of a STRETCH
/// of one, and the next stretch's summary follows it. Saying so stops each batch
/// from opening with its own "this conversation was about…" preamble and from
/// concluding things the later batches contradict.
pub const BATCH_SUMMARY_INSTRUCTION: &str = "\
The messages above are one stretch from the middle of a longer conversation, and \
this message asks for their record, which will stand in for them. Later stretches \
follow yours and will be recorded the same way, so do not open with a preamble, do \
not introduce the project, and do not conclude -- write only what THIS stretch \
established. A compact factual record, not prose: every decision taken and the \
reason for it; every file created or changed and what the change was; every command \
run and its outcome; every number, name and path that is still needed; every \
question left open. Drop tool output bodies and reasoning. Do not call tools. \
Answer with the record and nothing else.";

/// **Summarise a slice of history in a session of its own.**
///
/// The whole reason compaction could not afford itself: `run_compaction` appends
/// the instruction to the LIVE session, so the turn needs the entire history
/// resident and room to write from the same window -- and it only ever runs when
/// that window is nearly full. Measured 2026-09-18 at 260390 of 262144: 1754
/// tokens to summarise 260390, which produced nothing, then produced a summary
/// cut off mid-expression.
///
/// A summary turn does not need the conversation it is not summarising. Given a
/// scratch session holding one batch and nothing else, the room available is the
/// whole window minus the batch, and the batch is a size this function's caller
/// chooses. So the turn can always be afforded; it is only the all-at-once
/// framing that could not be.
///
/// The scratch session is never persisted and never linked: it exists to be
/// prompted once and dropped. Its `transcript_id` is only what the ledger calls
/// its rows.
pub fn summarise_batch(
    engine: &mut TurnEngine<'_>,
    prefix: &StablePrefix,
    scratch_id: &str,
    batch: &[TranscriptItem],
    sink: &mut dyn EventSink,
) -> Result<Harvest, TurnFailure> {
    let mut scratch = engine
        .open(scratch_id, prefix)
        .map_err(TurnFailure::from)?;
    scratch
        .append_items(engine, batch, sink)
        .map_err(TurnFailure::from)?;
    let instruction = TranscriptItem::System {
        text: BATCH_SUMMARY_INSTRUCTION.to_string(),
        origin: SystemOrigin::Update,
    };
    scratch
        .append_items(engine, &[instruction], sink)
        .map_err(TurnFailure::from)?;

    // Same lead as the whole-conversation summary, for the same reason: this is a
    // record and not a decision, and a batch that reasons first is spending room
    // the next batch also wants.
    let ok = engine.without_reasoning(|engine| -> Result<TurnOk, TurnFailure> {
        loop {
            match engine.run_turn(&mut scratch, sink) {
                Ok(ok) => break Ok(ok),
                Err(TurnFailure::UnfinishedReasoning { .. }) => {
                    append_salvage_notice(&mut scratch, engine, sink, UNFINISHED_REASONING_NOTICE)?;
                }
                Err(TurnFailure::EmptyLength { reason, .. }) => {
                    let notice = empty_length_notice(reason);
                    append_salvage_notice(&mut scratch, engine, sink, &notice)?;
                }
                Err(e) => return Err(e),
            }
        }
    })?;
    Ok(harvest(&ok.items))
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
