//! Compaction is one more turn, not a special shape: a turn's prompt is the
//! ledger's whole token region, so appending the instruction as a
//! [`TranscriptItem`] and running a normal turn asks for the summary over a prefix
//! the server is already holding. [`CompactionOutcome`] carries the measured reuse
//! so a caller discloses it rather than assuming it.

//! The log - the ledger and its items - is the record of what was said, and it is
//! the truth. The KV cache is an optimisation a server process holds: per-slot,
//! evictable, gone on a restart. Designing the log around the cache makes a scratch
//! session look impossible; dismissing the cache because the log is fine is correct
//! and unusable.
//!
//! A cold prompt costs something on both sides - DeepSeek prices cached input at a
//! tenth of uncached, by prefix - and what differs is the currency:
//!
//! ```text
//! local llama.cpp   a miss costs TIME    200-375 tok/s -> ~10 minutes
//!                                    on a 240k prompt
//! DeepSeek          a miss costs MONEY   0.28 vs 0.028 per Mtok ->
//!                                    $0.067 against $0.0067
//! ```
//!
//! Ten minutes before a word is generated disqualifies a strategy; six cents does
//! not. A cloud provider takes MESSAGES rather than tokens
//! (`letibot_provider::messages::convert`), so none of the prefix machinery applies
//! to it at all.
//!
//! So the strategy branches on the BACKEND: a local server keeps the expensive half
//! a true prefix of what it already holds (`summarise_overrun`), a cloud provider
//! folds and continues with no overlap and no seam (`plan_fold` +
//! `summarise_first_half`). `plan_overrun` chooses, `Harness::compact_inner` carries
//! out the answer.
//!
//! Building the new base is deliberately not here: it reaches the store, the resume
//! chain and the session registry, and this module has none of them.

use letibot_dialect::StablePrefix;
use letibot_transcript::{SystemOrigin, TranscriptItem, UserPart};

use crate::engine::{Session, TurnEngine, TurnFailure, TurnOk};
use crate::events::{EventSink, NullSink, TurnEvent};
use crate::fidelity::{Facts, Fidelity};
use crate::length::EmptyReason;

/// The compaction turn's instruction, on both the local and the remote path, because it is the
/// record's own words and a provider must be asked the same thing a local model is.
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

/// The sections [`SUMMARY_INSTRUCTION`] promises, in order, as a list a test can
/// hold that text to.
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

/// What is appended when the summary turn stops inside its own reasoning and says nothing: the
/// record has to say why there is no summary rather than leave a gap.
pub const UNFINISHED_REASONING_NOTICE: &str = "\
Your reply ended inside reasoning with no output. Write the summary now.";

/// **How much of the room a record may claim**, in per-mille of `window - prefix - headroom`.
///
/// R22's measured failure: a 1,353,917-token conversation became a 2,250-token record, 600:1, on a
/// session whose window left ~1.34M to share. Nothing truncated it - the instruction named no
/// budget at all - so the record is sized by a stated fraction now rather than by the model's
/// guess. At 150 the record takes 15% of the room and the session keeps 85% of its window before
/// it compacts again.
pub const SUMMARY_FIDELITY_PER_MILLE: u64 = 150;

/// The least a budget may be, so a small window still buys a record that can enumerate something.
/// Never above what the summary turn can actually write - see [`summary_budget`].
pub const MIN_SUMMARY_TOKENS: u64 = 2_000;

/// What the ASK itself costs in the summary turn's own room: the instruction is appended after the
/// conversation and its tokens compete with the record's for the window. An allowance rather than a
/// measurement, because the ask is not tokenized until it is appended and 2k against an ask of
/// about 700 errs on the side of a record that fits.
pub const ASK_ROOM: u64 = 2_000;

/// Characters per token, for the one line that tells a model its room in characters: a model
/// cannot count tokens and can count lines.
pub const CHARS_PER_TOKEN: u64 = 4;

/// The room a summary gets, in LEDGER tokens, and which bound chose it. A struct rather than one
/// number because the sentence the operator reads has to be able to say WHY the record was as big
/// as it was, and "the ratio said so" and "the turn had no more room" are different answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SummaryBudget {
    /// `window - prefix - headroom`: what the conversation leaves to share between the record and
    /// the span the session continues on.
    pub room: u64,
    /// `room × SUMMARY_FIDELITY_PER_MILLE`: what the ratio alone would allow.
    pub share: u64,
    /// `window - resident`: what the summary turn can actually write, since its prompt is the
    /// whole conversation and the generation has to fit beside it.
    pub writable: u64,
    /// What the record may fill: the smaller of the two, floored at [`MIN_SUMMARY_TOKENS`].
    pub tokens: u64,
    /// Which bound chose `tokens`: `room` when the ratio bound it, `turn` when the turn's own free
    /// room did.
    pub by: &'static str,
}

impl SummaryBudget {
    /// The room in characters, for the instruction's own sentence.
    pub fn chars(&self) -> u64 {
        self.tokens * CHARS_PER_TOKEN
    }

    /// The room in lines of about 120 characters, which is the unit a model can act on.
    pub fn lines(&self) -> u64 {
        self.chars() / 120
    }

    /// How much of the budget a record of `chars` characters used, in per cent. Zero when there is
    /// no budget to speak of.
    pub fn used_per_cent(&self, chars: usize) -> u64 {
        let budget = self.chars();
        if budget == 0 {
            return 0;
        }
        ((chars as u64) * 100 / budget).min(100)
    }

    /// The clause a report carries: what the record was allowed, and what bound that.
    pub fn said(&self) -> String {
        match self.by {
            "turn" => format!(
                "the record was allowed {} of {} ledger tokens - every token the summary turn \
                 had left after its own ask",
                self.tokens, self.room
            ),
            _ => format!(
                "the record was allowed {} of {} ledger tokens, {} per mille of the room the \
                 conversation leaves",
                self.tokens, self.room, SUMMARY_FIDELITY_PER_MILLE
            ),
        }
    }
}

/// The budget for one compaction of a session at `resident` ledger tokens.
///
/// `window`, `prefix_tokens`, `headroom` and `resident` are all in the same unit - the ledger's -
/// because that is the unit every caller measures in (`Config::planning_window`).
///
/// **The turn's own free room is a bound as well as the ratio, and it is the bound that usually
/// wins.** A compaction fires AT the wall, so `window - resident` is about one headroom, and a
/// record asked to fill 15% of a 1.34M room would be a generation of 200k tokens into 90k of free
/// context. Asking for that is asking for a cut record on every compaction; the honest budget is
/// the smaller of the two, and the report says which.
pub fn summary_budget(
    window: u64,
    prefix_tokens: u64,
    headroom: u64,
    resident: u64,
) -> SummaryBudget {
    let room = window
        .saturating_sub(prefix_tokens)
        .saturating_sub(headroom);
    let share =
        ((room as u128 * SUMMARY_FIDELITY_PER_MILLE as u128) / 1_000).min(u64::MAX as u128) as u64;
    let writable = window.saturating_sub(resident);
    let for_the_record = writable.saturating_sub(ASK_ROOM);
    let tokens = share
        .min(for_the_record)
        .max(MIN_SUMMARY_TOKENS.min(for_the_record));
    SummaryBudget {
        room,
        share,
        writable,
        tokens,
        by: if tokens == share { "room" } else { "turn" },
    }
}

/// **The words a verbatim tail will carry**, for the fidelity gate: what a reader of the new base
/// can still read. A tool call is included by its argument text, because that is where the path or
/// the command lives - a tail whose call was counted as a fact it does not carry would be re-asked
/// for something already in the base.
pub fn carried_text(items: &[TranscriptItem]) -> String {
    let mut out = String::new();
    for item in items {
        match item {
            TranscriptItem::User { parts, .. } => {
                for part in parts {
                    if let UserPart::Text { text } = part {
                        out.push_str(text);
                        out.push('\n');
                    }
                }
            }
            TranscriptItem::Assistant {
                text, tool_calls, ..
            } => {
                out.push_str(text);
                out.push('\n');
                for call in tool_calls {
                    out.push_str(&call.arguments);
                    out.push('\n');
                }
            }
            TranscriptItem::ToolResult { payload, .. } => {
                out.push_str(payload);
                out.push('\n');
            }
            TranscriptItem::Reasoning { text, .. } => {
                out.push_str(text);
                out.push('\n');
            }
            _ => {}
        }
    }
    out
}

/// **The instruction for a record that has a size**, which is the compaction that runs almost
/// always. [`SUMMARY_INSTRUCTION`] stays exactly as it was for the overrun halves, whose fold R22
/// deliberately left alone: there the failure is that the conversation does not fit in front of
/// the window, and the budget is `WRITE_ROOM` rather than a share of anything.
///
/// Two texts rather than one, because a budget belongs to the path that has one - and one test
/// holds the two together on the sections, which is the part a reader depends on
/// (`the_two_instructions_ask_for_the_same_sections`).
pub fn summary_instruction(budget: &SummaryBudget) -> String {
    format!(
        "\
The conversation above is what this record stands in for: it is what a reader will have \
instead of it, and anything you do not carry into it is lost.
\
You have room for about {tokens} tokens - roughly {chars} characters, or {lines} lines at 120 \
characters. USE IT. A record much shorter than that has dropped work that was in the \
conversation, and it will be sent back to be written again.
\
Write the record in exactly these sections, in this order, and keep every one of them even when \
it is empty:
\
## Objective
\
- one or two sentences: what the operator is trying to accomplish
\
## Important Details
\
- one line per thing that constrains the work: a preference, a decision and why it was taken, a \
command and what it did, a number still needed, an assumption
\
## Work State
\
### Completed
\
- one line per finished piece of work, verified fact or change made
\
### Active
\
- one line per thing in progress or being investigated
\
### Blocked
\
- one line per blocker, failing command or unknown
\
## Next Move
\
- one numbered line per next action, most immediate first
\
## Relevant Files
\
- one line per file the conversation created, changed, read for what it said, or was sent: its \
exact path, then why it matters
\
Rules, and the second one is what a record this size gets wrong:
\
- One line per thing. A file, a command, a decision, an open question: each gets its own line.
\
- Never write \"various\", \"and others\" or \"etc.\"; never fold a group of items into one \
entry; never drop an item for being small. A reader who has your record and nothing else cannot \
recover what you did not write.
\
- Exact strings for exact things: file paths, symbols, commands, error strings, URLs and \
identifiers, spelled as they appeared.
\
- Drop tool output bodies and reasoning. Carry what was concluded, what was run, what failed and \
what is left.
\
- If you read or were sent an IMAGE, say so and say what was in it: the picture does not survive \
this record.
\
- Do not call tools. Do not write about this record or about the conversation being shortened.
\
Answer with the record and nothing else.",
        tokens = budget.tokens,
        chars = budget.chars(),
        lines = budget.lines(),
    )
}

#[cfg(test)]
mod the_record_has_a_size {
    use super::*;

    /// R22's own session, in the units the decision was made in: a 1,440,006-token window, a
    /// 9,103-token prompt, a headroom of a sixteenth of it, and 1,352,917 tokens resident when the
    /// compaction fired. The record it produced was 2,250 tokens.
    fn the_measured_session() -> SummaryBudget {
        summary_budget(1_440_006, 9_103, 90_000, 1_352_917)
    }

    /// The size is a stated fraction of the room and not the model's guess: the same three numbers
    /// any reader can recompute from the report.
    #[test]
    fn the_budget_is_a_stated_fraction_of_the_room() {
        let b = the_measured_session();
        assert_eq!(b.room, 1_340_903, "window - prefix - headroom");
        assert_eq!(b.share, 1_340_903 * 150 / 1_000);
        assert_eq!(
            (b.share * 1_000 / b.room).abs_diff(SUMMARY_FIDELITY_PER_MILLE),
            1,
            "the ratio is the ratio, to the last per mille an integer division can keep: {}",
            b.share
        );
    }

    /// The measured record was a fraction of what it was allowed, and at the wall the bound is the
    /// turn's own free room rather than the ratio - a 200k generation does not fit in 87k.
    #[test]
    fn the_record_that_was_produced_is_a_fraction_of_what_it_was_allowed() {
        let b = the_measured_session();
        assert_eq!(b.writable, 1_440_006 - 1_352_917);
        assert_eq!(b.tokens, b.writable - ASK_ROOM, "less the ask's own room");
        assert_eq!(b.by, "turn");
        assert!(
            b.tokens > 2_250 * 20,
            "{} against the 2,250 it wrote",
            b.tokens
        );
    }

    /// And the point of the ratio: the new base is a small part of the window, so the session has
    /// most of its span before it has to compact again.
    #[test]
    fn the_session_keeps_most_of_its_window_for_working() {
        let b = the_measured_session();
        let base = 9_103 + b.tokens;
        assert!(
            base * 100 / 1_440_006 < 10,
            "a new base of {base} of 1,440,006"
        );
    }

    /// Two bounds, and both are real: the ratio on a session that compacts with room to spare, the
    /// turn's own free room on one that compacts at the wall.
    #[test]
    fn which_bound_chose_the_budget() {
        let early = summary_budget(1_000_000, 10_000, 62_500, 100_000);
        assert_eq!(early.room, 927_500);
        assert_eq!(early.tokens, 927_500 * 150 / 1_000);
        assert_eq!(early.by, "room");
        assert!(early.said().contains("150 per mille"), "{}", early.said());

        let wall = summary_budget(262_144, 6_880, 16_384, 250_000);
        assert_eq!(wall.tokens, 262_144 - 250_000 - ASK_ROOM);
        assert_eq!(wall.by, "turn");
        assert!(
            wall.said().contains("had left after its own ask"),
            "{}",
            wall.said()
        );
    }

    /// A small window still buys a record: the floor sits under the ratio, and never above what the
    /// turn has left once its own ask is paid for.
    #[test]
    fn the_floor_buys_a_record_on_a_small_window() {
        let b = summary_budget(12_000, 1_000, 2_048, 9_000);
        assert_eq!(b.share, 1_342, "under the floor");
        assert_eq!(b.tokens, 1_000, "what the window has left, less the ask");
        assert!(b.tokens <= b.writable, "{} of {}", b.tokens, b.writable);

        // And with room to spare, the floor is the least it ever gets.
        let roomy = summary_budget(12_000, 1_000, 2_048, 5_000);
        assert_eq!(roomy.tokens, MIN_SUMMARY_TOKENS);
    }

    /// Whatever the inputs, a budget never asks for a token the window has not got: a prompt larger
    /// than the window, a resident past it, and nothing at all are all arithmetic, not panics.
    #[test]
    fn a_budget_never_exceeds_what_the_turn_can_write() {
        for (window, prefix, headroom, resident) in [
            (262_144, 6_880, 16_384, 250_000),
            (8_192, 8_000, 2_048, 8_192),
            (0, 0, 0, 0),
            (1_000, 10_000, 500, 2_000),
            (1_440_006, 9_103, 90_000, 1_352_917),
        ] {
            let b = summary_budget(window, prefix, headroom, resident);
            assert!(
                b.tokens <= b.writable,
                "{window}/{prefix}/{headroom}/{resident}: {} > {}",
                b.tokens,
                b.writable
            );
            assert!(b.tokens <= window, "{window}: {} tokens", b.tokens);
            assert!(
                b.tokens <= b.writable.saturating_sub(ASK_ROOM),
                "{window}: a record that does not fit beside its own ask"
            );
        }
    }

    /// The instruction carries the number, and it does not carry the word that did the damage:
    /// *"write a **compact** factual record"* is what produced 2,250 tokens for 3,590 rows.
    #[test]
    fn the_instruction_states_the_budget_and_asks_for_every_line() {
        let b = the_measured_session();
        let text = summary_instruction(&b);
        assert!(
            text.contains(&b.tokens.to_string()),
            "the number the model is measured against: {}",
            b.tokens
        );
        assert!(
            text.contains(&b.lines().to_string()),
            "and the unit it can act on"
        );
        assert!(
            !text.to_lowercase().contains("compact"),
            "R22: the word was doing damage no number beside it undoes"
        );
        assert!(text.contains("One line per thing"));
        assert!(text.contains("Never write \"various\""));
        // The ask is a string literal with line continuations, which is exactly where a newline goes
        // missing: every heading starts a LINE, and no continuation backslash survives into it.
        assert!(
            text.contains("\n## Relevant Files\n"),
            "a heading has to start its own line: {text}"
        );
        assert!(!text.contains('\\'), "a continuation in the text: {text}");
    }

    /// A section is a question, and an absent section is a question nobody was asked
    /// (`docs/compaction.md` §6) - so the budgeted instruction asks every one of them.
    #[test]
    fn the_budgeted_instruction_keeps_every_section() {
        let text = summary_instruction(&the_measured_session());
        for section in SUMMARY_SECTIONS {
            assert!(text.contains(section), "the instruction lost {section}");
        }
    }

    /// Two texts - the overrun halves keep the one R22 left alone - and the sections are the part a
    /// reader depends on, so one test holds the pair together.
    #[test]
    fn the_two_instructions_ask_for_the_same_sections() {
        let budgeted = summary_instruction(&the_measured_session());
        for section in SUMMARY_SECTIONS {
            assert!(
                SUMMARY_INSTRUCTION.contains(section),
                "the overrun instruction lost {section}"
            );
            assert!(
                budgeted.contains(section),
                "the budgeted one lost {section}"
            );
        }
        for text in [SUMMARY_INSTRUCTION, budgeted.as_str()] {
            assert!(text.contains("Do not call tools"));
            assert!(text.contains("IMAGE"));
        }
    }

    /// How much of its room a record used, which is the other half of "sized to what it holds": a
    /// record at 11% of a 348k-character allowance is not the 600:1 record this entry is about.
    #[test]
    fn how_much_of_the_budget_a_record_used() {
        let b = the_measured_session();
        assert_eq!(b.chars(), b.tokens * 4);
        assert_eq!(b.used_per_cent(2_250 * 4), 2);
        assert_eq!(
            b.used_per_cent(b.chars() as usize * 2),
            100,
            "capped, never 200"
        );
    }
}

/// What is appended when the summary turn spends its whole output on reasoning and
/// nothing was kept: the answer this turn owes is the summary.
fn empty_length_notice(_reason: EmptyReason) -> String {
    "Your reply was all reasoning and nothing was kept. Write the summary now.".to_string()
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
    /// The model ran out of room before it finished the summary. Surfaced rather than refused: a
    /// session that cannot summarise must still be told why.
    pub truncated: bool,
    /// `cached_tokens` from the turn's metrics: how much of the prefix the server says it reused.
    /// Disclosed rather than acted on, because it is the server's claim.
    pub cached_tokens: u64,
    /// What the prefix invariant permitted the server to reuse: the previous
    /// turn's prompt plus its committed generation. The gap to `cached_tokens` is
    /// the server's, not ours - the prompts are proven identical over the span.
    pub reusable: u64,
    /// Tokens the summary turn generated — reasoning included. The summary's own
    /// size in the new base is at most the visible part of this.
    pub generated_tokens: u64,
    /// The room this record was given, when the compaction had a size at all: `None` on the
    /// overrun halves, which R22 left as they were.
    pub budget: Option<SummaryBudget>,
    /// What the record kept of what the conversation held, measured against the structure of the
    /// transcript. `None` when nothing was measured - the overrun halves, and any compaction that
    /// never got as far as a record.
    pub fidelity: Option<Fidelity>,
}

/// **What a record is asked for, when it has a size**: the budget it may fill, and the text of
/// whatever the compaction carries verbatim beside it - the tail's items are in the new base, so a
/// fact they hold is not a fact the record dropped, and the gate must not re-ask for it.
pub struct SummaryAsk<'a> {
    pub budget: SummaryBudget,
    /// The verbatim tail, as the words it will be carried as. Empty for a local model, which
    /// carries no tail at all.
    pub carried: &'a str,
}

/// Who answers a summary turn.
///
/// The engine holds one `endpoint` and one `model` and knows nothing about a
/// provider, which only arrives as an argument to
/// [`TurnEngine::run_turn_messages`] - so an unconditional `run_turn` sent a
/// metered session's summary, whole history in front of it, to the daemon's own
/// local model, which is why compaction could not rescue a session that had
/// overrun:
///
/// ```text
/// http 400: This model's maximum context length is 1048576 tokens.
///           However, you requested 1048607 tokens          <- deepseek, the turn
/// http 400: request (1504198 tokens) exceeds the available
///           context size (262144 tokens)                   <- qwen, the SUMMARY
/// ```
pub enum Answerer<'a> {
    /// The daemon's own endpoint, for a session that runs there anyway.
    Local,
    /// The session's provider, with the prefix's system prompt and its tools.
    ///
    /// The tools look like dead weight, since a summary cannot call one, but they
    /// are part of the prefix every other turn sends. A local server caches that
    /// prefix, and a summary that dropped them would be the one call that reads the
    /// whole history cold, on the largest conversation the session will ever have.
    ///
    /// Carrying them does not provoke a provider's demand for every previous turn's
    /// `reasoning_content`; a compaction without tools was refused for that just the
    /// same, and every combination answers 200:
    ///
    /// ```text
    /// no tools, assistant without reasoning_content                 200
    /// tools,    assistant without reasoning_content                 200
    /// tools,    assistant with tool_calls, no reasoning_content     200
    /// tools,    assistant with tool_calls, with reasoning_content   200
    /// no tools, assistant with tool_calls, no reasoning_content     200
    /// ```
    ///
    /// A compaction is the one call that must FIT, and sizing it for a span that
    /// cannot all be held is `plan_overrun`'s job, not this variant's.
    Provider {
        backend: &'a dyn letibot_backend::MessagesBackend,
        system: &'a str,
        tools_json: &'a [String],
    },
}

impl Answerer<'_> {
    fn run(
        &self,
        engine: &mut TurnEngine,
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
                // The same prefix every other turn sends: a different one turns the
                // summary into a cold prefill on the largest conversation in the
                // session.
                tools_json,
                None,
            ),
        }
    }
}

/// Run the compaction turn: append the instruction as a system update and take the turn that
/// answers it. One turn, not a special shape.
///
/// **A fold that fails leaves the transcript where it found it.** The instruction is a debt only a
/// summary pays, and nothing used to undo it: a session whose folds kept failing carried one more
/// copy of it per attempt, into the very record the fold was meant to produce.
///
/// `ask` is what makes the record SIZED (R22): the budget it may fill, and the tail measured with
/// it. `None` is the overrun path and the tests, which keep the instruction and the shape they had.
#[allow(clippy::too_many_arguments)] // the ask is the caller's fact and does not belong to the engine
pub fn run_compaction(
    engine: &mut TurnEngine,
    session: &mut Session,
    sink: &mut dyn EventSink,
    answerer: &Answerer<'_>,
    ladder: &mut letibot_http::retry::Ladder,
    ask: Option<&SummaryAsk<'_>>,
) -> Result<CompactionOutcome, TurnFailure> {
    // The facts are read BEFORE the instruction lands: the instruction is not part of what the
    // conversation held, and a record measured against its own ask would always name the ask.
    let facts = ask.map(|_| Facts::of(&session.items));
    let mark = session.items.len();
    let out = compact_turn(engine, session, sink, answerer, ladder, ask, facts);
    if out.is_err() {
        undo_failed_fold(session, mark);
    }
    out
}

/// Undo what a fold appended that did not become a record: the instruction, and the notices a
/// salvage wrote. The ledger is FORKED rather than truncated, because the region is append-only and
/// it must stay as long as the items: a longer ledger pairs later rows with the wrong item.
fn undo_failed_fold(session: &mut Session, mark: usize) {
    if session.items.len() <= mark {
        return;
    }
    match session.ledger.fork(&session.transcript_id, mark) {
        Ok(ledger) => {
            session.items.truncate(mark);
            session.ledger = ledger;
        }
        // The failed turn's rows stay, and it is said: worse than the record, and still better
        // than a transcript whose two halves have stopped being the same list.
        Err(e) => eprintln!(
            "letibot: a failed fold was not rolled back ({e}); its {} item(s) stay in the \
             transcript",
            session.items.len() - mark
        ),
    }
}

#[allow(clippy::too_many_arguments)] // the ask and its facts are the caller's, as above
fn compact_turn(
    engine: &mut TurnEngine,
    session: &mut Session,
    sink: &mut dyn EventSink,
    answerer: &Answerer<'_>,
    ladder: &mut letibot_http::retry::Ladder,
    ask: Option<&SummaryAsk<'_>>,
    facts: Option<Facts>,
) -> Result<CompactionOutcome, TurnFailure> {
    let instruction = TranscriptItem::System {
        text: match ask {
            Some(a) => summary_instruction(&a.budget),
            None => SUMMARY_INSTRUCTION.to_string(),
        },
        origin: SystemOrigin::Update,
    };
    session
        .append_items(engine, &[instruction], sink)
        .map_err(TurnFailure::from)?;

    // A user item, not a system update: this is the harness talking to the model
    // mid-compaction, and a system message at the end of a request is the shape a provider
    let mut reasked = false;
    let (ok, record, fidelity) = engine.without_reasoning(
        |engine| -> Result<(TurnOk, Harvest, Option<Fidelity>), TurnFailure> {
            loop {
                match answerer.run(engine, session, sink) {
                    Ok(ok) => {
                        let record = harvest(&ok.items);
                        let fidelity = match (ask, facts.as_ref()) {
                            (Some(a), Some(f)) => {
                                Some(f.measure(&format!("{}\n{}", record.summary, a.carried)))
                            }
                            _ => None,
                        };
                        // **Asked once.** A record that dropped most of what the conversation
                        // held is sent back with the gap named - and if the second answer is
                        // thin too, that is the answer: a loop here would spend the session's
                        // whole budget arguing with a model that cannot see through it. A
                        // record CUT by the window is not re-asked either: it stopped for a
                        // mechanical reason the second attempt would meet again.
                        if !record.truncated
                            && !reasked
                            && let Some(f) = &fidelity
                            && let Some(gap) = f.ask_again()
                        {
                            reasked = true;
                            append_salvage_notice(session, engine, sink, &gap)?;
                            continue;
                        }
                        break Ok((ok, record, fidelity));
                    }
                    Err(TurnFailure::UnfinishedReasoning { .. }) => {
                        append_salvage_notice(session, engine, sink, UNFINISHED_REASONING_NOTICE)?;
                    }
                    Err(TurnFailure::EmptyLength { reason, .. }) => {
                        let notice = empty_length_notice(reason);
                        append_salvage_notice(session, engine, sink, &notice)?;
                    }
                    // An endpoint that failed is the retry policy's, not the salvage's: safe here for the
                    // same reason the summary path is.
                    Err(TurnFailure::Http(e)) => match ladder.failed(&e) {
                        // The wait is the policy's own, bounded by its longest step; the
                        // alternative is a summary turn nobody tried twice.
                        Some(wait) => std::thread::sleep(wait),
                        None => return Err(TurnFailure::Http(e)),
                    },
                    // SalvageExhausted lands here, and so does everything that is not a
                    // say-nothing turn: a guard trip, a spent budget. Propagating is honest -
                    // a compaction that did not run must say so.
                    Err(e) => return Err(e),
                }
            }
        },
    )?;

    let Harvest {
        summary,
        tool_calls,
        truncated,
        ..
    } = record;
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
        budget: ask.map(|a| a.budget),
        fidelity,
    })
}

/// What a summary turn produced, read off its items: a free function because it is the part
/// worth testing without a model.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Harvest {
    pub summary: String,
    pub tool_calls: usize,
    pub truncated: bool,
    /// How many whole units the span lost before this record was asked for, and the bytes of body
    /// they cost: what the summariser was never shown, and so what the record cannot name.
    pub evicted: usize,
    pub gone: usize,
    /// The units themselves, in the order the eviction spent them. The caller writes their map into
    /// the new base and lets `recall` reach their bodies, so it needs these and not a count.
    pub units: Vec<crate::evict::Unit>,
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

/// Append one salvage notice as a user item: what this crate owes is the transcript row, and a
/// user row is the only one a later turn is sure to read.
fn append_salvage_notice(
    session: &mut Session,
    engine: &TurnEngine,
    sink: &mut dyn EventSink,
    text: &str,
) -> Result<(), TurnFailure> {
    let item = TranscriptItem::User {
        // `Agent`, not the operator: the trail that says so is harnessd's, but the ROW
        // is what a head draws, and a notice drawn as the operator's words is a lie.
        speaker: letibot_transcript::Speaker::Agent,
        parts: vec![UserPart::Text { text: text.into() }],
    };
    session
        .append_items(engine, &[item], sink)
        .map_err(TurnFailure::from)
}

/// How a conversation that arrived over the budget gets compacted. The ordinary path appends the
/// instruction and takes a summary turn; this is the one that has to cut first, because the
/// conversation does not fit the window it must be summarised in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverrunPlan {
    /// There is room to summarise in place; use the ordinary path.
    NotOverrun,
    /// Cut here: `items[..cut]` is summarised as the old half, `items[tail_from..]` as the tail,
    /// and the two are joined onto a new base.
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

/// The least room the in-place summary needs: the instruction plus the record. Not the trigger's
/// headroom, because the summary is a turn of its own.
pub const MIN_SUMMARY_ROOM: u64 = 8_192;

/// Room a scratchpad summary turn is given to write in, when planning the halves.
/// Generous on purpose: the failure avoided is a half that runs out, and erring wide
/// only costs a smaller tail.
pub const WRITE_ROOM: u64 = 16_384;

/// Slack on top, so a cut that only just works is not chosen. "with some margin".
const CUT_MARGIN: u64 = 8_192;

/// The least of a new base that may be a verbatim tail, and the most: both ends are real
/// configurations, and a tail outside them is a base that cannot be planned against.
pub const MIN_TAIL_TOKENS: u64 = 2_000;
pub const MAX_TAIL_TOKENS: u64 = 15_000;

/// The budget for a compaction's verbatim tail: a quarter of the window, clamped. `window` is in
/// the model's own tokens, so the budget is too.
pub fn tail_budget(window: u64) -> u64 {
    (window / 4).clamp(MIN_TAIL_TOKENS, MAX_TAIL_TOKENS)
}

/// Where a compaction's verbatim tail starts, and whether it starts at an exchange boundary: a
/// tail that opens mid-exchange is what makes a summary unreadable.
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
    /// The tail begins inside the newest exchange, because that exchange alone is
    /// larger than the budget. `dropped` items of it were left out.
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

/// The tail did not start where the conversation did. One number, because there is one thing to
/// tell a reader: how much of the front was left out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TailSplit {
    /// Items of the newest exchange that were left out of the tail.
    pub dropped: usize,
}

/// The verbatim tail a compaction carries, and only for a model on the far side of a bill: a
/// local model is bounded by its KV cache, and a tail there costs VRAM a turn needs.
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
    // Where an exchange can begin: the first item, and every user item after it. The
    // first is a boundary by construction, so a history with no user item at all still
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

/// What must be disclosed about a tail the caller placed itself, rather than one `plan_tail`
/// chose: a placed tail has no plan behind it, and the report must say so.
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

/// The sections the wire carries, in wire order: the template's leaves. Seven of the template's
/// eight are here, and the eighth is the record itself.
pub const WIRE_SECTIONS: [&str; 7] = [
    "Objective",
    "Important Details",
    "Completed",
    "Active",
    "Blocked",
    "Next Move",
    "Relevant Files",
];

/// Split a summary the model wrote into the sections the template asked for. A heading is a line
/// of the template's own text, so a model that renames one loses that section rather than the
/// summary.
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
    // In the template's order, not the model's: the wire is where that is decided
    // rather than left to each head.
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

/// Why a compaction's tail is what it is, for a report's `tail.because`: three ways to be empty
/// and one to be non-empty, and a reader acts differently on each.
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
    plan_cut(item_tokens, prefix_tokens, window)
}

/// `plan_overrun`'s cut without its size test: the sliced fold, asked for by name. `plan_overrun`
/// decides whether to fold at all, and this is the fold.
pub fn plan_cut(item_tokens: &[u64], prefix_tokens: u64, window: u64) -> OverrunPlan {
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

    // The tail reaches back PAST the cut, deliberately (see `OverrunPlan::Cut`), but by
    // a third of the old half at most: reaching back as far as the budget allowed made
    // both halves the same conversation summarised twice, at twice the cost.
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
    // Only reachable through `plan_cut` (a history that fits whole, so `cut` took all of
    // it): the second half must read something.
    if tail_from == item_tokens.len() && cut == item_tokens.len() && cut > 0 {
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

    /// A local model is bounded by VRAM and a tail competes with the pressure that
    /// called the compaction; a remote model is bounded by a bill, where the newest
    /// exchanges verbatim are what a summary is worst at.
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

    /// The template's sections are a contract with the next reader: a section dropped
    /// from the instruction stops being a question asked, so the list is checked
    /// against the bytes.
    #[test]
    fn the_instruction_keeps_asking_every_section() {
        for section in SUMMARY_SECTIONS {
            assert!(
                SUMMARY_INSTRUCTION.contains(section),
                "the instruction no longer asks for `{section}`"
            );
        }
        // In the order the reader expects them, and the sub-sections after their
        // parent - a template that reordered itself would still contain every string
        // above.
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
        // An image is the one fact the record cannot leave behind: everything else can be
        // looked at again, but a picture is bytes the summary replaces, so what the model
        // saw is all that survives.
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
    /// The record split into the template's sections, with the two absences kept apart: a heading
    /// with words, a heading the model omitted, and a heading the template never asked for.
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

    /// A model that ignored the template gets an empty list, not a guess: an unknown
    /// heading still ends the section before it, so a model that invents `## Also` must
    /// not have its words swallowed into `Important Details`.
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

    /// The parse is case-insensitive and trimmed, because a model that writes
    /// `## objective` has still answered the question the section asks; refusing it
    /// would turn a formatting habit into a lost fact.
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

    /// Why a tail is what it is, one answer per arm, so a report's `tail.because`
    /// cannot be a word the daemon never produces.
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
        let items: Vec<u64> = std::iter::repeat_n(1_000, 100).collect();
        assert_eq!(plan_overrun(&items, P, W), OverrunPlan::NotOverrun);
    }

    /// Past the trigger line with room to spare is the ordinary case: the trigger fires
    /// at `resident + headroom >= window`, so every compaction arrives past that line
    /// and the wall lands wherever the last round stopped.
    #[test]
    fn arriving_past_the_trigger_with_room_for_a_summary_is_not_an_overrun() {
        let resident = 247_301u64;
        let items: Vec<u64> =
            std::iter::repeat_n(1_000, ((resident - P) / 1_000) as usize).collect();
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
        let items: Vec<u64> =
            std::iter::repeat_n(1_000, ((resident - P) / 1_000) as usize).collect();
        assert!(matches!(
            plan_overrun(&items, P, W),
            OverrunPlan::Cut { .. }
        ));
    }

    /// The measured case: arrives at ~260k of a 262k window, so the in-place
    /// summary has 1754 tokens to write in and cannot.
    #[test]
    fn a_conversation_that_arrived_over_the_budget_is_cut() {
        let items: Vec<u64> = std::iter::repeat_n(1_000, 255).collect();
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
        let items: Vec<u64> = std::iter::repeat_n(1_000, 255).collect();
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

/// Summarise an over-budget conversation into a scratchpad, in two halves: `plan_overrun` decides
/// when, and this is the two summary turns that make the new base. `rows` is the plan the session
/// is working from, which is what the eviction may not touch.
pub fn summarise_overrun(
    engine: &mut TurnEngine,
    prefix: &StablePrefix,
    scratch_id: &str,
    items: &[TranscriptItem],
    plan: &OverrunPlan,
    rows: &[String],
    store: &dyn crate::evict::BodyStore,
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
        rows,
        1,
        2,
        store,
        sink,
        answerer,
    )?;
    let old = summarise_one(
        engine,
        prefix,
        &format!("{scratch_id}-old"),
        &items[..*cut],
        rows,
        2,
        2,
        store,
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

    let mut units = old.units;
    units.extend(tail.units);
    Ok(Harvest {
        summary,
        tool_calls: old.tool_calls + tail.tool_calls,
        truncated: old.truncated || tail.truncated,
        evicted: units.len(),
        gone: units.iter().map(|u| u.gone).sum(),
        units,
    })
}

/// Carries a scratch summary turn's long wait out, labelled as the compaction's own: the slice is
/// history to a head, and the wait must not look like the session's turn.
struct CompactionSink<'a> {
    inner: &'a mut dyn EventSink,
    half: u64,
    halves: u64,
    prompt_tokens: u64,
    /// Prefill, from a transport that reports it.
    processed: u64,
    /// Characters received, for the transport that reports no token count.
    chars: u64,
    /// The server's own count, when the transport gives one.
    tokens: Option<u64>,
    /// Deltas seen, so the publish can be strided. See `emit`.
    deltas: u64,
}

impl CompactionSink<'_> {
    /// What to report as `written`, and in which unit. A token count wins where there is one: it
    /// is the same unit the window is in.
    fn written(&self) -> (u64, &'static str) {
        match self.tokens {
            Some(t) => (t, "tokens"),
            None => (self.chars, "chars"),
        }
    }

    fn publish(&mut self) {
        let (written, unit) = self.written();
        let p = TurnEvent::CompactionProgress {
            half: self.half,
            halves: self.halves,
            prompt_tokens: self.prompt_tokens,
            processed: self.processed,
            written,
            unit,
        };
        self.inner.emit(p);
    }
}

impl EventSink for CompactionSink<'_> {
    fn emit(&mut self, event: TurnEvent) {
        match event {
            TurnEvent::PromptProgress { progress, .. } => {
                self.processed = progress.processed;
                self.publish();
            }
            TurnEvent::TokensGenerated { tokens, .. } => {
                self.tokens = Some(tokens);
                self.publish();
            }
            TurnEvent::Delta { text, .. } => {
                self.chars += text.chars().count() as u64;
                self.deltas += 1;
                // Strided, because a tick per chunk is a flood and the flood is what breaks the
                // indicator: a messages transport has no token count, so characters are the unit.
                if self.deltas == 1 || self.deltas.is_multiple_of(32) {
                    self.publish();
                }
            }
            // Everything else: the scratch rows, the tool calls, the turn's own
            // boundaries. Dropped, which is the whole reason this sink exists.
            _ => {}
        }
    }
}

/// One summary turn over one slice, in a throwaway session. `half` and `halves` are what the
/// caller knows, and the progress line is built from them.
///
/// The slice is REDUCED first: the bodies whose units cannot be current, whose subagent answered,
/// or whose call is finished go as stubs before the record is asked about anything. What the
/// record is asked, then, is what cannot be evicted.
#[allow(clippy::too_many_arguments)] // the slice and its place among the halves are the caller's facts
fn summarise_one(
    engine: &mut TurnEngine,
    prefix: &StablePrefix,
    scratch_id: &str,
    slice: &[TranscriptItem],
    plan: &[String],
    half: u64,
    halves: u64,
    store: &dyn crate::evict::BodyStore,
    sink: &mut dyn EventSink,
    answerer: &Answerer<'_>,
) -> Result<Harvest, TurnFailure> {
    // The rows go to a `NullSink` and stay there; see `CompactionSink` for why that is
    // the right answer for the slice and the wrong one for the progress.
    let mut quiet = NullSink;
    let reduced = crate::evict::reduce(slice, plan, store);
    let mut scratch = engine.open(scratch_id, prefix).map_err(TurnFailure::from)?;
    scratch
        .append_items(engine, &reduced.items, &mut quiet)
        .map_err(TurnFailure::from)?;
    let instruction = TranscriptItem::System {
        text: SUMMARY_INSTRUCTION.to_string(),
        origin: SystemOrigin::Update,
    };
    scratch
        .append_items(engine, &[instruction], &mut quiet)
        .map_err(TurnFailure::from)?;

    // The half's own prompt, in LEDGER tokens: prefix, slice and instruction. It is the bar's
    // denominator, so a half with no room refuses before it starts.
    let prompt_tokens = scratch.ledger.len() as u64;

    // Progress, not warnings: each half used to open and close with a warning, and four rows
    // per compaction is a wall a person scrolls past.
    let ok = run_half(sink, half, halves, prompt_tokens, |progress| {
        engine.without_reasoning(|engine| -> Result<TurnOk, TurnFailure> {
            loop {
                match answerer.run(engine, &mut scratch, progress) {
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
        })
    })?;
    Ok(Harvest {
        evicted: reduced.units.len(),
        gone: reduced.gone(),
        units: reduced.units,
        ..harvest(&ok.items)
    })
}

/// One half's life on the progress line: published as it begins, carried while the body runs,
/// published again as it ends.
fn run_half<T>(
    sink: &mut dyn EventSink,
    half: u64,
    halves: u64,
    prompt_tokens: u64,
    body: impl FnOnce(&mut CompactionSink<'_>) -> Result<T, TurnFailure>,
) -> Result<T, TurnFailure> {
    let mut progress = CompactionSink {
        inner: sink,
        half,
        halves,
        prompt_tokens,
        processed: 0,
        chars: 0,
        tokens: None,
        deltas: 0,
    };
    progress.publish();
    let out = body(&mut progress)?;
    // The half's last word on the line, with its final count — the strided publish
    // may have skipped the last few deltas.
    progress.publish();
    Ok(out)
}

/// The room a fold's verbatim tail may spend: what `plan_fold` leaves it after the prefix and
/// the fold's own reserve for the record, halved by the caller where the tail is.
pub fn fold_tail_room(window: u64, prefix_tokens: u64, headroom: u64) -> u64 {
    window
        .saturating_sub(prefix_tokens)
        .saturating_sub(headroom.max(WRITE_ROOM + CUT_MARGIN))
}

/// Summarise the first half, then continue on `summary ++ second half`: the operator's design
/// for a conversation that must be folded without losing its tail.
pub fn plan_fold(
    item_tokens: &[u64],
    prefix_tokens: u64,
    window: u64,
    reserve: u64,
) -> Option<usize> {
    let total: u64 = item_tokens.iter().sum();
    // A caller with no reserve to name still has to be over the window by something
    // before a fold is worth a summary turn: `MIN_SUMMARY_ROOM` is that floor, so the
    // callers that predate `reserve` pass zero and keep their meaning.
    let reserve = reserve.max(MIN_SUMMARY_ROOM);
    if prefix_tokens + total + reserve <= window {
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

/// Carry out `plan_fold`: one summary turn over `items[..split]`, returned as the record that
/// stands in for those items. `row` is the plan the session is working from, which is what the
/// eviction may not touch.
pub fn summarise_first_half(
    engine: &mut TurnEngine,
    prefix: &StablePrefix,
    scratch_id: &str,
    items: &[TranscriptItem],
    split: usize,
    rows: &[String],
    store: &dyn crate::evict::BodyStore,
    sink: &mut dyn EventSink,
    answerer: &Answerer<'_>,
) -> Result<Harvest, TurnFailure> {
    summarise_one(
        engine,
        prefix,
        scratch_id,
        &items[..split],
        rows,
        1,
        1,
        store,
        sink,
        answerer,
    )
}

#[cfg(test)]
mod folding {
    use super::*;

    const W: u64 = 262_144;
    const P: u64 = 6_880;

    #[test]
    fn a_conversation_with_room_is_not_folded() {
        let items: Vec<u64> = std::iter::repeat_n(1_000, 100).collect();
        assert_eq!(plan_fold(&items, P, W, 0), None);
    }

    /// The band the door acts in, which is where a resume lands: 200k of a 262k window is not
    /// overrun, and a door that fired there would summarise a conversation with room to spare.
    #[test]
    fn a_reserve_makes_a_conversation_at_the_wall_fold() {
        let items: Vec<u64> = std::iter::repeat_n(1_000, 200).collect();
        assert_eq!(
            plan_fold(&items, P, W, 0),
            None,
            "it fits, and no reserve is named: no fold"
        );
        let split = plan_fold(&items, P, W, 62_500).expect("the door's own band folds");
        assert!((98..=102).contains(&split), "halfway, got {split}");
    }

    /// The split is halfway BY TOKENS, and both halves are workable.
    #[test]
    fn the_split_is_halfway_by_tokens() {
        let items: Vec<u64> = std::iter::repeat_n(1_000, 255).collect();
        let split = plan_fold(&items, P, W, 0).expect("255k of a 262k window folds");
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
        items.extend(std::iter::repeat_n(1_000, 55));
        // The first item alone is past half, so the split lands before it and the
        // first half would be empty -- there is no useful fold here.
        assert_eq!(plan_fold(&items, P, W, 0), None);
    }
}

/// The scratch turn's long wait, carried out labelled: this is a unit the operator watches, and
/// a wait with no label reads as a stuck daemon.
#[cfg(test)]
mod compaction_sink {
    use super::*;
    use crate::completion::PromptProgress;
    use crate::events::DeltaTarget;

    /// A sink that keeps what it was given, so a test can say what a head would see.
    #[derive(Default)]
    struct Recorder {
        seen: Vec<TurnEvent>,
    }

    impl EventSink for Recorder {
        fn emit(&mut self, event: TurnEvent) {
            self.seen.push(event);
        }
    }

    fn sink<'a>(inner: &'a mut Recorder, prompt_tokens: u64) -> CompactionSink<'a> {
        CompactionSink {
            inner,
            half: 2,
            halves: 2,
            prompt_tokens,
            processed: 0,
            chars: 0,
            tokens: None,
            deltas: 0,
        }
    }

    fn progress_of(event: &TurnEvent) -> (u64, u64, u64, u64, u64, &'static str) {
        match event {
            TurnEvent::CompactionProgress {
                half,
                halves,
                prompt_tokens,
                processed,
                written,
                unit,
            } => (*half, *halves, *prompt_tokens, *processed, *written, unit),
            other => panic!("expected CompactionProgress, got {other:?}"),
        }
    }

    /// Reading: a `PromptProgress` becomes a `CompactionProgress` carrying the half's
    /// own identity and prompt size, and the number is the server's `processed`.
    #[test]
    fn a_scratch_prompts_progress_is_relabelled_as_the_halfs_own() {
        let mut rec = Recorder::default();
        {
            let mut s = sink(&mut rec, 940_612);
            s.emit(TurnEvent::PromptProgress {
                turn_id: "scratch#1".into(),
                progress: PromptProgress {
                    total: 940_612,
                    cache: 0,
                    processed: 12_345,
                    time_ms: 900,
                },
            });
        }
        assert_eq!(rec.seen.len(), 1, "one event in, one event out");
        assert_eq!(
            progress_of(&rec.seen[0]),
            (2, 2, 940_612, 12_345, 0, "chars"),
            "the half, its prompt, and what has been read — in the compaction's own fields"
        );
        assert!(
            matches!(rec.seen[0], TurnEvent::CompactionProgress { .. }),
            "and it is the one variant a head has a field of its own for"
        );
    }

    /// Writing: the server's own count wins and says so, in `tokens`.
    #[test]
    fn the_servers_token_count_is_what_written_reports() {
        let mut rec = Recorder::default();
        {
            let mut s = sink(&mut rec, 500);
            s.emit(TurnEvent::TokensGenerated {
                turn_id: "scratch#1".into(),
                tokens: 1_200,
            });
        }
        assert_eq!(
            progress_of(&rec.seen[0]),
            (2, 2, 500, 0, 1_200, "tokens"),
            "the provider's own figure, in the unit it is in"
        );
    }

    /// A messages transport has no token count at all, so characters are what `written` reports,
    /// and the report says which unit it used.
    #[test]
    fn a_messages_stream_counts_characters_and_publishes_on_a_stride() {
        let mut rec = Recorder::default();
        {
            let mut s = sink(&mut rec, 940_612);
            for i in 0..96 {
                s.emit(TurnEvent::Delta {
                    turn_id: "scratch#1".into(),
                    target: DeltaTarget::Text,
                    text: "word ".into(),
                });
                assert_eq!(s.deltas, i + 1);
            }
        }
        // 1, 32, 64 and 96 — four publishes for ninety-six deltas, not ninety-six.
        assert_eq!(rec.seen.len(), 4, "strided, not a tick per chunk");
        assert_eq!(
            progress_of(&rec.seen[0]),
            (2, 2, 940_612, 0, 5, "chars"),
            "the first delta publishes at once, so the line appears when writing starts"
        );
        assert_eq!(
            progress_of(&rec.seen[3]),
            (2, 2, 940_612, 0, 480, "chars"),
            "96 deltas x 5 chars"
        );
    }

    /// A half is one updating line, never a row: the operator pasting what a compaction drew
    /// for one turn is what made this a single line.
    #[test]
    fn a_half_is_progress_from_its_first_event_to_its_last_and_never_a_warning() {
        let mut rec = Recorder::default();
        let got = super::run_half(&mut rec, 1, 2, 976_900, |s| {
            s.emit(TurnEvent::Delta {
                turn_id: "scratch#1".into(),
                target: DeltaTarget::Text,
                text: "## Goal".into(),
            });
            Ok(7)
        })
        .expect("the half runs");
        assert_eq!(got, 7);
        assert!(
            rec.seen
                .iter()
                .all(|e| matches!(e, TurnEvent::CompactionProgress { .. })),
            "nothing but progress reaches a head: {:?}",
            rec.seen
        );
        assert_eq!(
            progress_of(&rec.seen[0]),
            (1, 2, 976_900, 0, 0, "chars"),
            "the line names the half before the server has said anything"
        );
        assert_eq!(
            progress_of(rec.seen.last().unwrap()),
            (1, 2, 976_900, 0, 7, "chars"),
            "and its last word carries the final count"
        );
    }

    /// The scratch rows and the tool calls do not go out: announcing them puts a card on a
    /// head for work nobody asked for.
    #[test]
    fn the_scratch_conversations_own_rows_are_dropped() {
        let mut rec = Recorder::default();
        {
            let mut s = sink(&mut rec, 100);
            s.emit(TurnEvent::TurnStarted {
                turn_id: "scratch#1".into(),
                model: "deepseek-flash".into(),
                ledger_head: "deadbeef".into(),
                began_ms: None,
            });
            s.emit(TurnEvent::ToolCallProposed {
                turn_id: "scratch#1".into(),
                call_id: "call_1".into(),
                name: "read".into(),
                args_digest: "sha".into(),
                arguments: "{}".into(),
            });
            s.emit(TurnEvent::Warning {
                code: "row_coverage_gap",
                detail: "the scratch transcript is not the session's".into(),
            });
        }
        assert!(
            rec.seen.is_empty(),
            "a scratch turn's rows belong to no head: {:?}",
            rec.seen
        );
    }
}

#[cfg(test)]
mod fold_room {
    use super::*;

    /// `plan_fold` bounds the verbatim tail by `window - prefix - WRITE_ROOM - CUT_MARGIN` whatever
    /// the headroom, so a window whose headroom is smaller must not be offered the larger room.
    #[test]
    fn a_tail_is_never_offered_more_than_the_fold_planned_for() {
        let (window, prefix) = (200_000u64, 5_000u64);
        let headroom = window / 16;
        assert!(headroom < WRITE_ROOM + CUT_MARGIN);
        assert_eq!(
            fold_tail_room(window, prefix, headroom),
            window - prefix - WRITE_ROOM - CUT_MARGIN,
            "the record's reserve is the fold's own, not the door's headroom"
        );
    }

    #[test]
    fn a_larger_headroom_still_wins() {
        let (window, prefix) = (1_000_000u64, 5_000u64);
        let headroom = window / 16;
        assert!(headroom > WRITE_ROOM + CUT_MARGIN);
        assert_eq!(
            fold_tail_room(window, prefix, headroom),
            window - prefix - headroom
        );
    }
}
