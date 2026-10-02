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

/// What is appended as the compaction turn's instruction — **on both the local and
/// the remote path**, because it is the record's contract and not the model's.
///
/// # The template, and why it is fixed sections rather than an ask for a record
///
/// Ruled by the operator, 2026-09-23 (`head-parity-2026-09-21.md` R27): *"I think
/// we can reuse the summary template, and indeed send some recent turns
/// verbatim."* The template is opencode's `SUMMARY_TEMPLATE`
/// (`packages/core/src/session/compaction.ts:17`), which is the only part of
/// their compaction this crate takes wholesale.
///
/// A free-prose ask produced records of wildly different shapes, which is the
/// defect the sections exist for: **a section is a question, and an absent
/// section is a question nobody was asked.** "Nothing is blocked" and "nobody
/// said" are different facts, and only a kept-and-marked section can carry the
/// second. That is why *keep every section, even when empty* is a rule and not a
/// preference.
///
/// **This replaces the prose ask of 2026-09-22**, which said *"every decision
/// taken and the reason for it; every file created or changed …"* and left the
/// shape to the model. The content it asked for survives inside the sections —
/// decisions and why are *Important Details*, commands run and their outcomes are
/// *Work State*, and the files are *Relevant Files*.
///
/// # The one thing here that is not the template, and the wording it is in
///
/// *"anything you do not carry into it is lost"* — the loss stated to the model
/// rather than left implicit. A summariser that is not told the prior record is
/// discarded will reasonably assume it survives, and on a second compaction the
/// prior record sits in the conversation like any other item.
///
/// Worded as a **contract about the record**, not as a claim about the tail: on
/// the remote path the newest exchanges are also carried verbatim and on the
/// local path they are not, so an instruction that named the tail would either
/// invite the model to skip the recent past (losing it outright locally) or be
/// false locally. The reader is told the shape by the fork's own note, which is
/// written per path; the model is told its contract, which is the same on both.
///
/// *Do not call tools* stays, and is load-bearing: the harness refuses a fork
/// whose summary turn proposed a call, and the refusal is a real one rather than
/// a trap — a summary turn that starts working is not a summary.
pub const SUMMARY_INSTRUCTION: &str = "\
The conversation above is what this record stands in for: it is what a reader will \
have instead of it, and anything you do not carry into it is lost. Write the record \
in exactly these sections, in this order, and keep every one of them even when it is \
empty:\n\
## Objective\n\
- one or two brief sentences: what the operator is trying to accomplish\n\
## Important Details\n\
- constraints, preferences, decisions and why they were taken, commands run and what \
they did, facts and assumptions, numbers still needed — or \"(none)\"\n\
## Work State\n\
### Completed\n\
- finished work, verified facts, changes made; otherwise \"(none)\"\n\
### Active\n\
- current work, partial changes, what is being investigated; otherwise \"(none)\"\n\
### Blocked\n\
- blockers, failing commands, unknowns; otherwise \"(none)\"\n\
## Next Move\n\
1. the immediate concrete action, or \"(none)\"\n\
2. the next action if known, or \"(none)\"\n\
## Relevant Files\n\
- file or directory path: why it matters, or \"(none)\"\n\
Rules: terse bullets, not prose paragraphs. Drop tool output bodies and reasoning. \
Preserve exact file paths, symbols, commands, error strings, URLs and identifiers \
where you know them. If you read or were sent an IMAGE, say so and say what was in it \
— the picture itself does not survive this record and cannot be looked at again, so \
what you saw is the only thing that can be carried. Do not call tools. Do not write \
about this record or about the conversation being shortened. Answer with the record \
and nothing else.";

/// The sections [`SUMMARY_INSTRUCTION`] promises, in order.
///
/// **A contract with the next reader**, so it is a list a test can hold the
/// instruction to rather than prose in a doc comment: a template that loses a
/// section stops asking the question that section stands for, and nothing else
/// in the system would notice.
pub const SUMMARY_SECTIONS: [&str; 8] = [
    "## Objective",
    "## Important Details",
    "## Work State",
    "### Completed",
    "### Active",
    "### Blocked",
    "## Next Move",
    "## Relevant Files",
];

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

/// **Who answers a summary turn.**
///
/// Compaction called [`TurnEngine::run_turn`] unconditionally, and that is the
/// LOCAL endpoint — the engine holds one `endpoint` and one `model` and knows
/// nothing about a provider, which only ever arrives as an argument to
/// [`TurnEngine::run_turn_messages`]. So a conversation running on a metered
/// provider had its summaries sent to the daemon's own model, with the whole
/// history in front of them.
///
/// Measured on the operator's box, 2026-09-20, in one log four lines apart:
///
/// ```text
/// http 400: This model's maximum context length is 1048576 tokens.
///           However, you requested 1048607 tokens          <- deepseek, the turn
/// http 400: request (1504198 tokens) exceeds the available
///           context size (262144 tokens)                   <- qwen, the SUMMARY
/// ```
///
/// The second is this bug with its own error message: 1.5M tokens of a
/// deepseek conversation handed to a local server with a 262144-token window,
/// which is why compaction could not rescue a session that had overrun. Their
/// question, which was the right one: *"maybe qwen was asked for summary?"*
pub enum Answerer<'a> {
    /// The daemon's own endpoint, for a session that runs there anyway.
    Local,
    /// The session's provider, with the prefix's system prompt **and its tools.**
    ///
    /// # Why a summary turn carries the tools, which is not obvious
    ///
    /// A summary cannot call a tool — [`crate::compaction::harvest`] counts whatever
    /// one proposes and nothing executes it — so a tool list here looks like dead
    /// weight. It is not. **The reason is the PREFIX, and it is a local-model reason.**
    ///
    /// Every other turn in the session sends this same prefix: the same system text
    /// and the same tool schemas, byte for byte. A local server caches that prefix, so
    /// the summary turn inherits a warm one and pays almost no prefill. Send a
    /// DIFFERENT prefix — and dropping the tools makes it different — and the one call
    /// that runs when the window is nearly full, over the largest conversation the
    /// session will ever have, becomes the one call that reads its whole history cold.
    /// That is the opposite of what a compaction is for.
    ///
    /// The cost is invisible on a metered provider, which is where this was first
    /// reasoned about, and where the argument that follows was written.
    ///
    /// # The argument that stood here, and what disproved it
    ///
    /// This variant briefly carried NO tools, on the reasoning that carrying them is
    /// what forces the provider to demand every previous turn's `reasoning_content`
    /// back — making the summary request ~1.6x the size of the ledger it summarises.
    /// That rule is in the vendor's guide and was taken for the API's behaviour.
    ///
    /// **It is not the API's behaviour.** Measured against the live API 2026-10-02 on
    /// `deepseek-flash`, every one of these answers 200:
    ///
    /// ```text
    /// no tools, assistant without reasoning_content                 200
    /// tools,    assistant without reasoning_content                 200
    /// tools,    assistant with tool_calls, no reasoning_content     200
    /// tools,    assistant with tool_calls, with reasoning_content   200
    /// no tools, assistant with tool_calls, no reasoning_content     200
    /// ```
    ///
    /// and the change did not fix what it was for: on the daemon that had it, a
    /// compaction carrying no tools was refused for not passing `reasoning_content`
    /// back. Whatever provokes that refusal, it is not the presence of `tools`.
    ///
    /// Two things about carrying tools are true, and neither outweighs the prefix:
    ///
    ///   * **It cannot call one.** The guard that refuses a summary's tool calls is in
    ///     `compact_inner`, not here.
    ///   * **A compaction is the one call that must FIT** — it runs when the window is
    ///     nearly full, which is the only time it runs. So the request is as large as
    ///     it has to be, and the fit is the caller's problem: `plan_overrun` exists to
    ///     size a summary for a span that cannot all be held at once.
    Provider {
        backend: &'a dyn letibot_backend::MessagesBackend,
        system: &'a str,
        tools_json: &'a [String],
    },
}

impl Answerer<'_> {
    fn run(
        &self,
        engine: &mut TurnEngine<'_>,
        session: &mut Session,
        sink: &mut dyn EventSink,
    ) -> Result<TurnOk, TurnFailure> {
        match self {
            Answerer::Local => engine.run_turn(session, sink),
            Answerer::Provider {
                backend,
                system,
                tools_json,
            } => engine.run_turn_messages(
                session,
                sink,
                &mut crate::steering::NoSteering,
                *backend,
                system,
                // **The same prefix every other turn sends**, which is the whole point
                // of carrying them: a different one turns the summary into a cold
                // prefill on the largest conversation in the session. See this type's
                // own docs.
                tools_json,
                None,
            ),
        }
    }
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
    answerer: &Answerer<'_>,
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
            match answerer.run(engine, session, sink) {
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

    let Harvest {
        summary,
        tool_calls,
        truncated,
    } = harvest(&ok.items);
    let reusable = match &ok.metrics.prefix_check {
        crate::prefix::PrefixCheck::Held {
            expected_cached_min,
            ..
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
        if let TranscriptItem::Assistant {
            text,
            tool_calls,
            truncated,
            ..
        } = item
        {
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
        let h = harvest(&[assistant(
            "…`super::quarantine(call_id, &page.final_url, &page",
            true,
        )]);
        assert!(
            h.truncated,
            "the flag the engine stamped must survive the harvest"
        );
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
        // **`Agent`, and this crate is why the field is on the row rather than the trail.**
        // The trail is harnessd's and says the same thing; the ROW is what a head draws, and
        // a salvage notice drawn as the operator's words is the lie R42 is about.
        speaker: letibot_transcript::Speaker::Agent,
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
///
/// **The measurement behind that range was taken under the free-prose instruction
/// of 2026-09-22, and the fixed sections are not the same ask.** Whether eight
/// sections (Objective / Important Details / Work State / Next Move / Relevant
/// Files) produce a longer record is not known — nothing has measured one. The
/// constant is kept on the old range rather than adjusted by a guess, and this is
/// the sentence to change if a template-shaped record turns out to be longer.
pub const MIN_SUMMARY_ROOM: u64 = 8_192;

/// Room a scratchpad summary turn is given to write in, when planning the halves.
/// Generous on purpose, and separate from the decision above: the failure being
/// avoided here is a half that runs out, and erring wide only costs a smaller
/// tail.
pub const WRITE_ROOM: u64 = 16_384;

/// Slack on top, so a cut that only just works is not chosen. "with some margin".
const CUT_MARGIN: u64 = 8_192;

/// The least of a new base that may be a verbatim tail, and the most.
///
/// The clamp is opencode's own shape — `clamp(2_000, 15_000, usable / 4)`,
/// `packages/opencode/src/session/compaction.ts:116` — and both ends are real
/// configuration rather than politeness. A window small enough that a quarter of
/// it is a few hundred tokens must not spend that quarter on a tail; a 262144-token
/// window must not put 65k of verbatim history back into a base whose whole
/// purpose was to be small. The same file's `MIN_PRESERVE_RECENT_TOKENS` and
/// `MAX_PRESERVE_RECENT_TOKENS` are these two numbers.
pub const MIN_TAIL_TOKENS: u64 = 2_000;
pub const MAX_TAIL_TOKENS: u64 = 15_000;

/// The budget for a compaction's verbatim tail: **a quarter of the window,
/// clamped.**
///
/// `window` is in the same units as the item sizes it will be compared against —
/// for a provider session that is [`crate::compaction`]'s caller's business,
/// because the ledger over-counts what a provider is sent and the conversion is a
/// measurement this crate does not hold. Every caller passes the window it plans
/// against, which is what makes the number mean something.
pub fn tail_budget(window: u64) -> u64 {
    (window / 4).clamp(MIN_TAIL_TOKENS, MAX_TAIL_TOKENS)
}

/// Where a compaction's verbatim tail starts, and whether it starts at an
/// exchange boundary.
///
/// **The whole design of this side is that a boundary is preferred and a mid-turn
/// start is disclosed.** A tail that begins mid-turn is a conversation whose first
/// message answers a question that is no longer present, which is §3 of
/// `docs/compaction.md` — *something must stand where the evicted span was* —
/// applied *inside* the boundary rather than before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TailPlan {
    /// Nothing is carried verbatim: the local path, an empty history, or a single
    /// item larger than the whole budget.
    None,
    /// Whole exchanges, from `from` to the end of the history.
    Whole {
        from: usize,
        tokens: u64,
        exchanges: usize,
    },
    /// The tail begins **inside** the newest exchange, because that exchange alone
    /// is larger than the budget. `dropped` items of it were left out.
    Split {
        from: usize,
        tokens: u64,
        dropped: usize,
    },
}

impl TailPlan {
    /// Where the tail starts, or `None`. Both arms carry the same field so a
    /// caller does not have to match twice to slice the history.
    pub fn from(&self) -> Option<usize> {
        match *self {
            TailPlan::None => None,
            TailPlan::Whole { from, .. } | TailPlan::Split { from, .. } => Some(from),
        }
    }

    pub fn tokens(&self) -> u64 {
        match *self {
            TailPlan::None => 0,
            TailPlan::Whole { tokens, .. } | TailPlan::Split { tokens, .. } => tokens,
        }
    }

    /// What the fork must say about the tail, or `None` when there is nothing to
    /// disclose.
    pub fn split(&self) -> Option<TailSplit> {
        match *self {
            TailPlan::Split { dropped, .. } => Some(TailSplit { dropped }),
            _ => None,
        }
    }
}

/// **The tail did not start where the conversation did.**
///
/// One number, because there is only one thing to tell the reader: the verbatim
/// part begins in the middle of an exchange, so its first message answers
/// something that is no longer here. Carried on the fork rather than printed by
/// the code that chose the tail, because the model reading the new base is the one
/// who needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TailSplit {
    /// Items of the newest exchange that were left out of the tail.
    pub dropped: usize,
}

/// **The verbatim tail a compaction carries, and only for a model on the far side
/// of a bill.**
///
/// Ruled by the operator, 2026-09-23 (`head-parity-2026-09-21.md` R27), split by
/// where the model runs, and the reason is the reason rather than the policy: a
/// local model is bounded by the **KV cache in VRAM**, and a tail is resident
/// tokens competing with the very pressure the compaction was called to relieve. A
/// remote model is bounded by a **context limit and a bill**, where 15k of verbatim
/// recent turns is affordable and buys back exactly what a summary is worst at —
/// the literal text of the last few exchanges. Same mechanism, different budget,
/// so it is one requirement with a conditional and not two.
///
/// `remote` is `true` for a session whose turns go to a `MessagesBackend` and
/// `false` for one served by this box's own endpoint. It is a parameter rather
/// than a backend handle because that is the whole of the decision.
///
/// **Whole exchanges, or a disclosed split.** The newest exchange is tried first,
/// then the one before it, and so on: taking a whole exchange keeps the tail
/// readable on its own. Only when the newest exchange by itself is larger than the
/// whole budget — one big file read, exactly the case that fills a window — does
/// the tail start inside it, and then [`TailPlan::split`] says so.
///
/// `items` and `item_tokens` are parallel, as the ledger's rows are: one row per
/// item. A caller whose history is empty, or whose token counts are not known yet,
/// gets [`TailPlan::None`] rather than a guess.
pub fn plan_compaction_tail(
    items: &[TranscriptItem],
    item_tokens: &[u64],
    window: u64,
    remote: bool,
) -> TailPlan {
    if !remote {
        return TailPlan::None;
    }
    plan_tail(items, item_tokens, tail_budget(window))
}

/// The tail under an explicit budget, so the arithmetic is testable without a
/// window and a backend.
pub fn plan_tail(items: &[TranscriptItem], item_tokens: &[u64], budget: u64) -> TailPlan {
    let n = items.len().min(item_tokens.len());
    if n == 0 || budget == 0 {
        return TailPlan::None;
    }
    // Where an exchange can begin: the first item, and every user item after it.
    // The first item is a boundary by construction — there is nothing before it to
    // dangle off — which is also why a history that has no user item at all still
    // has something to plan against.
    let mut starts: Vec<usize> = vec![0];
    for (i, item) in items.iter().enumerate().take(n).skip(1) {
        if matches!(item, TranscriptItem::User { .. }) {
            starts.push(i);
        }
    }

    // The MOST RECENT whole exchanges that fit, newest first.
    let mut from = n;
    let mut acc = 0u64;
    let mut exchanges = 0usize;
    for &start in starts.iter().rev() {
        let size: u64 = item_tokens[start..from].iter().sum();
        if acc + size > budget {
            break;
        }
        acc += size;
        from = start;
        exchanges += 1;
    }
    if exchanges > 0 {
        return TailPlan::Whole {
            from,
            tokens: acc,
            exchanges,
        };
    }

    // Not even the newest exchange fits whole. Take its tail and say that is what
    // happened: carrying nothing here would drop the exchange the session is
    // actually in, which is the one thing a verbatim tail exists for.
    let last = *starts.last().expect("`starts` always holds 0");
    let mut from = n;
    let mut acc = 0u64;
    while from > last {
        let next = acc + item_tokens[from - 1];
        if next > budget {
            break;
        }
        acc = next;
        from -= 1;
    }
    if from == n {
        // One item larger than the whole budget: nothing can be carried without
        // cutting an item in half, which this engine never does.
        return TailPlan::None;
    }
    TailPlan::Split {
        from,
        tokens: acc,
        dropped: from - last,
    }
}

/// What must be disclosed about a tail the caller placed itself, rather than one
/// [`plan_tail`] chose.
///
/// The overrun fold splits halfway **by tokens**, not at an exchange boundary —
/// one tool result can outweigh fifty exchanges — so the tail it keeps can begin
/// mid-exchange with nobody having decided that. This is the check that says so,
/// and it is separate from `plan_tail` because the two paths reach their `from` by
/// different arguments: one by budget, the other by arithmetic on the window.
pub fn tail_split_of(items: &[TranscriptItem], from: usize) -> Option<TailSplit> {
    if from == 0 || from >= items.len() {
        return None;
    }
    if matches!(items[from], TranscriptItem::User { .. }) {
        return None;
    }
    let mut start = from;
    while start > 0 && !matches!(items[start], TranscriptItem::User { .. }) {
        start -= 1;
    }
    Some(TailSplit {
        dropped: from - start,
    })
}

/// **The sections the WIRE carries, in wire order — the template's leaves.**
///
/// Seven of the template's eight headings: `Work State` is a grouping heading with no
/// body of its own, so a wire entry for it would be a section that is always empty by
/// construction, and a head drawing it would print *nothing here* about a heading that
/// was never a question. Its three children are the sections.
///
/// **These names are the daemon's and a head must not hold a copy** — the reason
/// `head-run.tools` and `SettingRow.choices` exist. The daemon publishes this list on a
/// `SettingRow` under `compaction.sections`, so a renamed section reaches a head that
/// was never rebuilt.
pub const WIRE_SECTIONS: [&str; 7] = [
    "Objective",
    "Important Details",
    "Completed",
    "Active",
    "Blocked",
    "Next Move",
    "Relevant Files",
];

/// **Split a summary the model wrote into the sections the template asked for.**
///
/// A heading is a line that is `#`s, a space, and one of [`WIRE_SECTIONS`]'s names
/// (trimmed, case-insensitively). Its body is everything after it, up to the next
/// heading line at any level.
///
/// **Only headings the model actually wrote come back, and that is the point.** An
/// entry with an empty body means the model wrote the heading and nothing under it; a
/// name missing from the result means it wrote no such heading at all. `"there is
/// nothing here"` and `"nobody said"` are different facts, and it is exactly the pair
/// the template's *keep every section, even when empty* rule exists to keep apart — the
/// rule tells the model to write `(none)`, and the return shape records what it did
/// instead when it did not.
///
/// `Work State`'s own body is never returned: its content is its children, and a
/// grouping heading is not a section. Anything before the first recognised heading is
/// dropped, which is where a model that ignored the template entirely ends up — an
/// empty result is read by a caller as *the structure was not followed*, and the
/// caller has the raw text either way.
pub fn parse_summary_sections(summary: &str) -> Vec<(String, String)> {
    // Every heading line, at any level, so a body stops at the next one whatever
    // level it is: `## Work State` followed immediately by `### Completed` gives the
    // parent no body at all, which is what it has.
    let mut found: Vec<(usize, String)> = Vec::new();
    for (i, line) in summary.lines().enumerate() {
        let Some(name) = heading_name(line) else {
            continue;
        };
        found.push((i, name));
    }
    let lines: Vec<&str> = summary.lines().collect();
    let mut out: Vec<(String, String)> = Vec::new();
    for (n, (start, name)) in found.iter().enumerate() {
        let end = found.get(n + 1).map(|(i, _)| *i).unwrap_or(lines.len());
        let Some(want) = WIRE_SECTIONS.iter().find(|w| w.eq_ignore_ascii_case(name)) else {
            continue;
        };
        let body = lines[start + 1..end].join("\n");
        out.push(((*want).to_string(), body.trim().to_string()));
    }
    // **In the template's order, not the model's.** A summary that wrote its sections
    // back to front should still read the way the next reader expects them, and the
    // wire is where that is decided rather than left to each head.
    let mut ordered: Vec<(String, String)> = Vec::new();
    for want in WIRE_SECTIONS {
        if let Some((_, body)) = out.iter().find(|(n, _)| n == want) {
            ordered.push((want.to_string(), body.clone()));
        }
    }
    ordered
}

/// The section name a line is a heading for, or `None`.
fn heading_name(line: &str) -> Option<String> {
    let t = line.trim_start();
    let hashes = t.len() - t.trim_start_matches('#').len();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = t[hashes..].strip_prefix(' ')?;
    let rest = rest.trim().trim_end_matches('#').trim();
    let rest = rest.trim_matches('*').trim();
    if rest.is_empty() {
        return None;
    }
    // Any heading, known or not: an unknown one still ends the section before it.
    // A model that invents `## Also` must not have its body swallowed into
    // `Important Details`.
    Some(rest.to_string())
}

/// **Why a compaction's tail is what it is**, for a report's `tail.because`.
///
/// R27's ruled conditional has three ways to be empty and one way to be bounded, and
/// the fourth is here because the first three do not describe a tail that *was*
/// carried: `"nothing_fits"` over a tail of three turns would be false. The operator's
/// list named the three; the shape is ours to finish, and an axis bound is a different
/// fact from a failure to fit.
pub const TAIL_BECAUSE: [&str; 4] = ["budget", "local_model", "nothing_fits", "no_turns"];

/// The `because` for a plan.
pub fn tail_because(plan: &TailPlan, remote: bool, had_history: bool) -> &'static str {
    match plan {
        TailPlan::Whole { .. } | TailPlan::Split { .. } => "budget",
        TailPlan::None if !remote => "local_model",
        TailPlan::None if !had_history => "no_turns",
        TailPlan::None => "nothing_fits",
    }
}

pub fn plan_overrun(item_tokens: &[u64], prefix_tokens: u64, window: u64) -> OverrunPlan {
    let resident = prefix_tokens + item_tokens.iter().sum::<u64>();
    if resident + MIN_SUMMARY_ROOM <= window {
        return OverrunPlan::NotOverrun;
    }
    if prefix_tokens + WRITE_ROOM + CUT_MARGIN >= window {
        return OverrunPlan::Hopeless {
            prefix_tokens,
            window,
        };
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
mod the_tail_and_the_template {
    use super::*;
    use letibot_transcript::UserPart;

    fn user(text: &str) -> TranscriptItem {
        TranscriptItem::User {
            speaker: Default::default(),
            parts: vec![UserPart::Text { text: text.into() }],
        }
    }

    fn answered(text: &str) -> TranscriptItem {
        TranscriptItem::Assistant {
            text: text.into(),
            tool_calls: vec![],
            truncated: false,
        }
    }

    /// Three exchanges of two items each: `u a u a u a`.
    fn three_exchanges() -> Vec<TranscriptItem> {
        vec![
            user("one"),
            answered("one"),
            user("two"),
            answered("two"),
            user("three"),
            answered("three"),
        ]
    }

    /// **The ruled split, and it is the whole requirement.** A local model is
    /// bounded by VRAM and a tail competes with the pressure that called the
    /// compaction; a remote model is bounded by a bill, where the newest
    /// exchanges verbatim are affordable and are what a summary is worst at.
    #[test]
    fn only_a_remote_compaction_carries_a_tail() {
        let items = three_exchanges();
        let tokens = vec![10u64; 6];
        assert_eq!(
            plan_compaction_tail(&items, &tokens, 262_144, false),
            TailPlan::None,
            "a local compaction carries the template and nothing verbatim"
        );
        assert!(matches!(
            plan_compaction_tail(&items, &tokens, 262_144, true),
            TailPlan::Whole { .. }
        ));
    }

    /// The tail is whole exchanges when whole exchanges fit, newest first, and the
    /// count is reported so the fork can say how many were carried.
    #[test]
    fn a_remote_tail_is_whole_exchanges_starting_at_an_exchange() {
        let items = three_exchanges();
        let tokens = vec![10u64; 6];
        // 2000 is the floor of the clamp, so every one of the three fits.
        let TailPlan::Whole {
            from,
            tokens: carried,
            exchanges,
        } = plan_tail(&items, &tokens, 2_000)
        else {
            panic!("three small exchanges fit the whole budget")
        };
        assert_eq!(from, 0);
        assert_eq!(carried, 60);
        assert_eq!(exchanges, 3);

        // A budget of 45 takes the newest exchange and the one before it, and
        // stops: 60 would be all three.
        let TailPlan::Whole {
            from, exchanges, ..
        } = plan_tail(&items, &tokens, 45)
        else {
            panic!("two exchanges of 20 fit 45")
        };
        assert_eq!(from, 2, "exchange three and exchange two, not one");
        assert_eq!(exchanges, 2);
    }

    /// A tail that starts mid-exchange says so. This is the one big read that
    /// filled the window: the exchange the session is actually in is larger than
    /// the whole budget, and dropping it would drop the only part worth keeping.
    #[test]
    fn a_newest_exchange_too_big_to_keep_whole_starts_mid_exchange() {
        let items = three_exchanges();
        let tokens = vec![10, 10, 10, 10, 700, 500];
        let plan = plan_tail(&items, &tokens, 1_000);
        let TailPlan::Split {
            from,
            tokens: carried,
            dropped,
        } = plan
        else {
            panic!("the newest exchange is 1200 of a 1000 budget: only a part fits")
        };
        assert_eq!(from, 5, "the last item of the last exchange");
        assert_eq!(carried, 500);
        assert_eq!(dropped, 1, "one item of that exchange was left out");
        assert_eq!(plan.split(), Some(TailSplit { dropped: 1 }));
        assert_eq!(plan.from(), Some(5));
        assert_eq!(plan.tokens(), 500);
    }

    /// Nothing fits when a single item is larger than the whole budget: the engine
    /// never cuts an item in half, so there is no tail rather than a broken one.
    #[test]
    fn an_item_larger_than_the_budget_leaves_no_tail() {
        assert_eq!(
            plan_tail(&three_exchanges(), &[10, 10, 10, 10, 10, 9_000], 1_000),
            TailPlan::None
        );
    }

    /// A history with no exchange boundary at all still has the start of the
    /// history as a boundary, and a history with nothing in it has none.
    #[test]
    fn an_empty_history_and_one_without_a_user_item() {
        assert_eq!(plan_tail(&[], &[], 15_000), TailPlan::None);
        let no_boundary = vec![answered("a"), answered("b")];
        assert_eq!(
            plan_tail(&no_boundary, &[10, 10], 15_000),
            TailPlan::Whole {
                from: 0,
                tokens: 20,
                exchanges: 1
            },
            "item zero is a boundary: there is nothing before it to dangle off"
        );
    }

    /// The budget is opencode's clamp, in our units, and both ends bind.
    #[test]
    fn the_budget_is_a_quarter_of_the_window_clamped() {
        assert_eq!(
            tail_budget(262_144),
            MAX_TAIL_TOKENS,
            "65536 clamps to 15000"
        );
        assert_eq!(tail_budget(32_768), 8_192, "no clamp applies in the middle");
        assert_eq!(tail_budget(4_096), MIN_TAIL_TOKENS, "1024 floors at 2000");
    }

    /// **The template's sections are a contract with the next reader.** A section
    /// dropped from the instruction stops being a question asked, and nothing else
    /// in the system would notice — so the list is checked against the bytes.
    #[test]
    fn the_instruction_keeps_asking_every_section() {
        for section in SUMMARY_SECTIONS {
            assert!(
                SUMMARY_INSTRUCTION.contains(section),
                "the instruction no longer asks for `{section}`"
            );
        }
        // In the order the reader expects them, and the sub-sections after their
        // parent — a template that reordered itself would still contain every
        // string above.
        let mut at = 0usize;
        for section in SUMMARY_SECTIONS {
            let found = SUMMARY_INSTRUCTION[at..]
                .find(section)
                .unwrap_or_else(|| panic!("`{section}` is out of order"));
            at += found + section.len();
        }
        assert!(
            SUMMARY_INSTRUCTION.contains("keep every one of them even when it is empty"),
            "an empty section is the fact that nothing was said, so it is kept"
        );
        assert!(
            SUMMARY_INSTRUCTION.contains("anything you do not carry into it is lost"),
            "the loss is stated to the model rather than left to be assumed"
        );
        // **An image is the one fact the record cannot leave behind.** Everything else in a
        // conversation can be looked at again — a path re-read, a command re-run, an error
        // reproduced. A picture is bytes the summary replaces, so what the model SAW is the whole of
        // what survives, and an instruction that did not ask for it would lose it silently. See
        // `docs/compaction.md` §8, where the two open halves (the token accounting, and the fact
        // that a tail image stays for the life of the session) are written down.
        assert!(
            SUMMARY_INSTRUCTION.contains("If you read or were sent an IMAGE, say so"),
            "the record is not asked to carry what an image showed, and the image does not survive it"
        );
        assert!(
            SUMMARY_INSTRUCTION.contains("Do not call tools"),
            "the harness refuses a fork whose summary turn proposed a call; the \
             instruction is what makes that refusal rare rather than a trap"
        );
    }
    /// **The record, split into the template's sections, with the two absences kept
    /// apart.**
    ///
    /// Three facts are held apart by one call and each is one assertion: a heading the
    /// model wrote with words under it, a heading it wrote with NOTHING under it (an
    /// empty body — *there is nothing here*), and a heading it never wrote at all
    /// (absent from the list — *nobody said*). Collapsing any two of those is the
    /// defect the whole shape exists against, and it is the same distinction the
    /// template's *keep every section, even when empty* rule is written for.
    #[test]
    fn the_record_splits_into_the_sections_it_was_asked_for() {
        // Written with `Blocked` BEFORE `Active`, which is the other order, and with
        // `Blocked` empty — so the two things this test is about are both exercised.
        let summary = "## Objective\n\nFix the stale echoes.\n\n## Work State\n\n### \
                       Completed\n\n- the rule\n\n### Blocked\n\n### Active\n\n- \
                       measuring\n\n## Next Move\n\n1. push";
        let got = parse_summary_sections(summary);
        let names: Vec<&str> = got.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            vec!["Objective", "Completed", "Active", "Blocked", "Next Move"],
            "in the template's order — which is NOT the order the model wrote them in — \
             and only the headings it wrote at all"
        );
        let body = |n: &str| -> String {
            got.iter()
                .find(|(name, _)| name == n)
                .unwrap_or_else(|| panic!("no `{n}` section"))
                .1
                .clone()
        };
        assert_eq!(body("Objective"), "Fix the stale echoes.");
        assert_eq!(body("Completed"), "- the rule");
        // **Written and empty** — *nothing is blocked*.
        assert_eq!(body("Blocked"), "");
        // **Never written** — *nobody said*. Two facts, two shapes.
        assert!(!names.contains(&"Important Details") && !names.contains(&"Relevant Files"));
        // `Work State` is a grouping heading: its own body is never returned, because
        // a head drawing it would print *nothing here* about a heading that was never
        // a question.
        assert!(!names.contains(&"Work State"));
    }

    /// **A model that ignored the template gets an empty list, not a guess.** An
    /// unknown heading still ENDS the section before it — a model that invents
    /// `## Also` must not have its words swallowed into `Important Details` — and a
    /// record with no recognised heading at all yields nothing, which a caller reads
    /// as *the structure was not followed*. The raw text is the caller's either way,
    /// so nothing is lost by refusing to invent sections.
    #[test]
    fn headings_the_template_does_not_know_stay_out_and_still_end_a_section() {
        assert!(parse_summary_sections("just some prose\nand more").is_empty());
        let got = parse_summary_sections("## Objective\n\n- a\n\n## Also\n\n- b");
        assert_eq!(got, vec![("Objective".to_string(), "- a".to_string())]);
        // A heading at any level ends the body, including one deeper than its parent.
        let deep =
            parse_summary_sections("## Next Move\n\n1. ship\n\n#### Relevant Files\n\n- a.rs");
        assert_eq!(
            deep,
            vec![
                ("Next Move".to_string(), "1. ship".to_string()),
                ("Relevant Files".to_string(), "- a.rs".to_string()),
            ]
        );
    }

    /// The parse is **case-insensitive and trimmed**, because a model that writes
    /// `## objective` or `## **Objective**` has still answered the question the
    /// section asks; refusing it would turn a formatting habit into a lost fact.
    #[test]
    fn a_heading_is_recognised_whatever_case_or_emphasis_it_carries() {
        assert_eq!(
            parse_summary_sections("## objective\n\n- a"),
            vec![("Objective".to_string(), "- a".to_string())]
        );
        assert_eq!(
            parse_summary_sections("## **Relevant Files**\n\n- a.rs"),
            vec![("Relevant Files".to_string(), "- a.rs".to_string())]
        );
        // And the names that come out are the DAEMON's spelling, never the model's:
        // a head must not hold a second copy of a list.
        assert_eq!(
            wire_names(&parse_summary_sections("### active\n\n- x")),
            vec!["Active"]
        );
    }

    fn wire_names(s: &[(String, String)]) -> Vec<&str> {
        s.iter().map(|(n, _)| n.as_str()).collect()
    }

    /// **Why a tail is what it is**, the four answers, one per arm — so a report's
    /// `tail.because` cannot be a word the daemon never produces.
    #[test]
    fn every_tail_reason_is_reachable_and_named() {
        let whole = TailPlan::Whole {
            from: 0,
            tokens: 1,
            exchanges: 1,
        };
        assert_eq!(tail_because(&whole, true, true), "budget");
        assert_eq!(tail_because(&TailPlan::None, false, true), "local_model");
        assert_eq!(tail_because(&TailPlan::None, true, false), "no_turns");
        assert_eq!(tail_because(&TailPlan::None, true, true), "nothing_fits");
        for why in TAIL_BECAUSE {
            assert!(!why.is_empty());
        }
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
        let items: Vec<u64> = std::iter::repeat(1_000)
            .take(((resident - P) / 1_000) as usize)
            .collect();
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
        let items: Vec<u64> = std::iter::repeat(1_000)
            .take(((resident - P) / 1_000) as usize)
            .collect();
        assert!(matches!(
            plan_overrun(&items, P, W),
            OverrunPlan::Cut { .. }
        ));
    }

    /// The measured case: arrives at ~260k of a 262k window, so the in-place
    /// summary has 1754 tokens to write in and cannot.
    #[test]
    fn a_conversation_that_arrived_over_the_budget_is_cut() {
        let items: Vec<u64> = std::iter::repeat(1_000).take(255).collect();
        let OverrunPlan::Cut {
            cut,
            tail_from,
            old_tokens,
            tail_tokens,
        } = plan_overrun(&items, P, W)
        else {
            panic!("260k of a 262k window is an overrun");
        };

        // The old half fits, with room to write about it and the margin to spare.
        assert!(
            P + old_tokens + WRITE_ROOM + CUT_MARGIN <= W,
            "the old half must be summarisable: {old_tokens}"
        );
        // The tail is only the excess, not half the conversation.
        assert!(
            tail_tokens < old_tokens,
            "tail {tail_tokens} vs old {old_tokens}"
        );
        // And the tail is itself summarisable, comfortably.
        assert!(P + tail_tokens + WRITE_ROOM <= W);
        // The halves overlap rather than abut: the tail starts at or before the
        // cut, and here it starts well before it.
        assert!(
            tail_from <= cut,
            "tail_from {tail_from} must not be past cut {cut}"
        );
        assert_eq!(tail_from + (tail_tokens / 1_000) as usize, items.len());
        assert!(
            cut > tail_from,
            "there is a real overlap: {tail_from}..{cut}"
        );
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
            OverrunPlan::Hopeless {
                prefix_tokens: 260_000,
                window: W
            }
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
    answerer: &Answerer<'_>,
) -> Result<Harvest, TurnFailure> {
    let OverrunPlan::Cut { cut, tail_from, .. } = plan else {
        return Ok(Harvest::default());
    };

    let tail = summarise_one(
        engine,
        prefix,
        &format!("{scratch_id}-tail"),
        &items[*tail_from..],
        sink,
        answerer,
    )?;
    let old = summarise_one(
        engine,
        prefix,
        &format!("{scratch_id}-old"),
        &items[..*cut],
        sink,
        answerer,
    )?;

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
    answerer: &Answerer<'_>,
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
    scratch
        .append_items(engine, slice, &mut quiet)
        .map_err(TurnFailure::from)?;
    let instruction = TranscriptItem::System {
        text: SUMMARY_INSTRUCTION.to_string(),
        origin: SystemOrigin::Update,
    };
    scratch
        .append_items(engine, &[instruction], &mut quiet)
        .map_err(TurnFailure::from)?;

    let ok = engine.without_reasoning(|engine| -> Result<TurnOk, TurnFailure> {
        loop {
            match answerer.run(engine, &mut scratch, &mut quiet) {
                Ok(ok) => break Ok(ok),
                Err(TurnFailure::UnfinishedReasoning { .. }) => {
                    append_salvage_notice(
                        &mut scratch,
                        engine,
                        &mut quiet,
                        UNFINISHED_REASONING_NOTICE,
                    )?;
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
    answerer: &Answerer<'_>,
) -> Result<Harvest, TurnFailure> {
    summarise_one(engine, prefix, scratch_id, &items[..split], sink, answerer)
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
        assert!(
            P + first + WRITE_ROOM <= W,
            "the first half is summarisable"
        );
        assert!(
            P + second + WRITE_ROOM <= W,
            "the second half leaves room to work"
        );
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
