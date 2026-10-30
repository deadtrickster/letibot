//! **The turn in flight**: its calls, its rows, whether the model is generating or waiting on
//! a call, and a compaction's progress.

use super::*;
use crate::ui::render::BlockCache;
use letibot_sessionlog::activity::Waiting;
use letibot_sessionlog::view::{CallState, SettledDecision, TurnState};
use rano::markdown::IncrementalMarkdown;

/// line. See `crates/ui/DESIGN.md` §2.3.
#[derive(Debug, Clone)]
pub(crate) struct CallRow {
    pub(crate) call_id: String,
    pub(crate) name: String,
    /// The §4.1 display target: the path, pattern or command line the call is
    /// about. Empty when the event carried none — a log recorded before the field
    /// existed, or a call first seen as `ToolStarted` — and the card then renders
    /// the verb alone rather than a guess.
    pub(crate) target: String,
    pub(crate) state: CallState,
    /// `Envelope::ts` of the proposal or the start, and of the finish. Zero means
    /// this call came out of a snapshot, which has no timestamps — and a duration
    /// invented from a clock the events were not measured against is worse than
    /// no duration, so that case renders as `card::Phase::Replayed`.
    pub(crate) started_ms: u64,
    /// **The head's own clock when this call started, or 0 when the head had no
    /// clock then** (R13).
    ///
    /// `started_ms` above is the *log's* clock — the `ts` the daemon stamped — and
    /// for as long as a call runs, that number **stops**: no event means no new
    /// timestamp, so the elapsed read off it sat at `0ms` for the whole of a
    /// `cargo build` while the spinner two rows below it turned. The two clocks are
    /// the same clock on this machine, so which one is used only matters for a head
    /// that was never told the time — and the anchor is taken **at the moment the
    /// call starts** rather than at render time for exactly that reason: a `--replay`
    /// applies every frame before it ever sets a clock (`bin/letibot-tui.rs`: the
    /// envelopes are applied in a loop, and `app.clock` is called only in the
    /// interactive loop after them), so a replayed call has no anchor and renders
    /// from the log's own span, which is the only honest measurement there.
    pub(crate) started_at: u64,
    pub(crate) ended_ms: u64,
    /// The most recent `ToolProgress { note }`.
    pub(crate) note: Option<String>,
    /// The settled decision this call was gated by, when there was one. Rendered on
    /// the card in the dim register — the approval is a fact about the call, not a
    /// stray note — and expanded to the brief the oracle saw and its reply.
    pub(crate) decision: Option<SettledDecision>,
}

#[derive(Debug, Default)]
pub(crate) struct TurnPane {
    pub(crate) turn_id: String,
    pub(crate) model: String,
    pub(crate) text: IncrementalMarkdown,
    /// **The thinking no row holds yet** — not the whole round's reasoning.
    ///
    /// A `Reasoning` row is appended with the round's thinking in it, so the same words exist
    /// twice for as long as this buffer keeps them: once in the transcript, where they belong, and
    /// once here. [`TurnPane::hand_over_reasoning`] spends what the rows hold off the front of this
    /// buffer at the row's body — the rule the reply one field up already had — and what is left is
    /// what the pane draws and what `live_work` counts.
    ///
    /// **It used to keep the whole round and be trimmed only where it was counted**, which is the
    /// defect the fold found: `[N thinking lines]` measured the unlanded part while the fold, opened
    /// with `ctrl-r`, drew all of it, so a landed thought was on the screen twice and the number
    /// beside it described only half of what was there.
    pub(crate) reasoning: IncrementalMarkdown,
    pub(crate) text_cache: BlockCache,
    pub(crate) reasoning_cache: BlockCache,
    pub(crate) calls: Vec<CallRow>,
    /// The raw `<function=…>` markup of this turn's tool calls, as it arrives on
    /// `DeltaTarget::ToolCall`.
    ///
    /// Kept, never shown by default. The default view shows a pending affordance
    /// while it is being written and the settled card afterwards; this is what the
    /// raw chord reveals, and it is the reason the chord can tell the truth
    /// instead of re-deriving markup the head never saw.
    pub(crate) raw_call: String,
    /// True between `<tool_call>` and the `ToolCallProposed` that settles it.
    ///
    /// A separate fact from `raw_call.is_empty()`: a turn that has written one call
    /// and is now writing prose has a non-empty `raw_call` and is not in a call.
    pub(crate) writing_call: bool,
    pub(crate) progress: Option<letibot_sessionlog::event::PromptProgress>,
    pub(crate) state: Option<TurnState>,
    /// Transcript rows appended while this turn ran.
    ///
    /// The pane is the *live* view of a turn. Once the turn has ended and those
    /// rows carry their content, the transcript is authoritative and the pane is a
    /// duplicate of it — so the pane stands down and only its summary line
    /// survives. Without this the answer is on the screen twice, once in the wrong
    /// order, which is what the first run of `--demo` showed.
    pub(crate) appended: Vec<String>,
    /// **Every row this TURN has produced, across all of its rounds** — and the difference from
    /// [`TurnPane::appended`] is the whole of a defect the operator caught twice.
    ///
    /// `appended` is per ROUND, because the daemon re-emits `TurnStarted` for every round of one
    /// prompt (`run_turn_steered` does `turn_seq += 1` inside the round loop) and each one builds a
    /// fresh `TurnPane`. That is right for what `appended` is used for — which rows the live pane is
    /// still drawing, and whether the turn has been superseded — and it was silently wrong for the
    /// one question asked with it: *does this run hold a row of the current turn.*
    ///
    /// Their screen, and their question:
    ///
    /// ```text
    ///   ▌ their message
    ///                    ← blank
    ///   [2 tool calls, 28 thinking lines]      ← the walk's marker, on the run of committed rows
    ///   [8 thinking lines]                     ← the pane's marker, for the same turn
    ///   ⠇ Responding · 1m42s
    /// ```
    ///
    /// *"ok, still not here - [2 tool calls] empty line [N thinking lines]. why not [2 tool calls, N
    /// thinking lines] on a single row?"* **Because round 3 forgot what rounds 1 and 2 did.** The run
    /// of committed rows stopped reading as this turn's, so `live_here` and `walk_carried_live` both
    /// answered *no*, the in-flight reasoning was not folded into the run, and the pane drew it
    /// beside the run instead — two markers, and two different thinking counts.
    ///
    /// One turn is one `began_ms` (stamped once per prompt and carried on every round's
    /// `TurnStarted`), so that is what this is carried on. **Cleared only when the turn is PROVEN
    /// to have changed** — a `began_ms` that differs from the one this pane is counting from. A
    /// boundary that carries no `began_ms` at all is *nobody measured this one* and not *a new
    /// prompt*: read as the latter it emptied this field, the run stopped reading as the turn's,
    /// and one run's work was drawn by two markers again (see the `TurnStarted` arm for the frame
    /// that made it reachable, and for the second `Responding` it manufactured).
    pub(crate) turn_rows: Vec<String>,
    /// How many of `calls` the transcript has already taken over.
    ///
    /// A round's tool-result rows are appended **in call order**, after every call
    /// in the round has been invoked (`harnessd::harness`, the `for call in &calls`
    /// loop, then one `append_items`). So the *n*th `tool_result` row of this turn
    /// is about `calls[n-1]`, exactly, with no id matching involved — which is the
    /// point, because the ids repeat.
    ///
    /// Everything below this index is on the screen already as a settled card with
    /// its payload under it, and drawing it a second time in the live pane is the
    /// wall the operator was looking at: eight `● Read …` rows above eight
    /// `▸ Read … · ok · N lines` rows, no added fact between them.
    ///
    /// **The takeover is the row's BODY landing, not its announcement.** The row is
    /// announced (`TranscriptAppended`) before its content arrives
    /// (`TranscriptContent`), and a row with no body renders zero rows — so a mark
    /// that advanced at the announcement handed the card to a row that could not
    /// draw yet, and for the frames between the two events the call was in *neither
    /// half*: the card erased, nothing in its place, the settled row a frame later.
    /// That gap is the operator's *"periodic flicker while diff card settles"*, and
    /// [`App::record_item`] is where the mark advances now — the same frame the row
    /// first draws, so the handover is one frame with the card on screen throughout.
    ///
    /// **And the mark is not the pane's alone.** Three readers ask which calls are still the live
    /// one's, and they have to agree: the live cards drawn below (`calls.get(*settled_calls..)`),
    /// the marker's NUMBER (`live_work`), and — since the stuck yellow — the marker's COLOUR, which
    /// asks which of the calls still executing have no result row yet ([`round_answered`]). A call
    /// the transcript has taken over is not executing, and a colour that kept asking the whole pane
    /// stayed yellow for the rest of the session on a call whose `ToolFinished` this head never
    /// received.
    pub(crate) settled_calls: usize,
    /// **How far down [`TurnPane::appended`] this pane's thinking has been handed over** — an
    /// index into that list, not a byte offset into [`TurnPane::reasoning`].
    ///
    /// The mark `settled_calls` is for calls, and it is here for the reason that one exists: work
    /// the transcript has taken over must not be drawn — or counted — a second time by the live
    /// pane. What it was before is worth keeping: `reasoned_upto`, a byte offset into the whole
    /// round's reasoning, honoured by the COUNT and by nothing else. The count was therefore
    /// trimmed and the text was not, and with the fold open (`ctrl-r`) one thinking line was drawn
    /// twice — the row where it belongs, then the same words again inside the pane, which is what
    /// the operator saw (*"yeah when reasoning is open wiered things happen, saw it too"*).
    ///
    /// **An index, and the buffer trimmed, because a frozen prefix cannot be sliced.** See
    /// [`TurnPane::hand_over_reasoning`] for the rule and for why the offset is not a thing that
    /// can be trusted: `reasoning` is lexed by `IncrementalMarkdown`, whose settled half is frozen,
    /// so *draw the slice after byte N* is a second document rather than a suffix of this one —
    /// and the count read one document while the fold drew another. Here there is one document:
    /// the pane's `reasoning` **is** the thinking no row holds, so the count and the fold cannot
    /// disagree about it, and the only state this cursor keeps is *which rows have been spent*.
    ///
    /// **A count that went DOWN is what a mark with no drawing behind it costs.** Impossible from
    /// the arithmetic — `reasoning_display_lines` is `ceil(width / cols)` summed, so adding text
    /// can only add lines. The operator saw it happen: *"lol, just saw how thinking lines count
    /// went from 22 to 15."* What the number was counting was the reasoning *plus* the rows that
    /// reasoning had already become; the round boundary rebuilds the pane — empty `reasoning`, mark
    /// at zero — and the inflation vanished with it. A count of work done fell, which is the same
    /// defect leticl measured on the calls side: *"2 (in yellow) tool calls dropping to 1 (in
    /// yellow) tool calls and then changing back to 2 (in white) tool calls."*
    pub(crate) reasoned_rows: usize,
    /// The `ts` of `TurnStarted`, and of the last event seen for this turn. The
    /// difference is how long the turn has been going, taken from the log's own
    /// clock rather than from a wall clock in the head — a head that reads a
    /// recorded session must show the same elapsed time as the one that watched it.
    pub(crate) started_ms: u64,
    pub(crate) last_ms: u64,
    /// The `ts` of the first and last `Delta { target: Reasoning }`.
    ///
    /// `card::reasoning` renders `Thought for 4.2s`, and this is where the 4.2
    /// comes from — no engine change needed, only a head that keeps the two
    /// timestamps it was already being handed. Zero means the reasoning arrived
    /// in a snapshot and has no honest duration.
    pub(crate) think_started_ms: u64,
    pub(crate) think_last_ms: u64,
}

impl TurnPane {
    /// **Does a row this round appended already hold the prose this pane is drawing?**
    ///
    /// The pane's prose and a stored `Assistant` row are two buffers holding one paragraph, and
    /// the head keeps them from both being drawn by *giving the pane's up* the moment the
    /// transcript has it ([`App::record_item`], at the row's body). That works because a body
    /// arrives as an event; a SNAPSHOT does not — it is a replacement, and it carries the turn's
    /// accumulated text (`Snapshot::turn`) beside rows that already hold their bodies. So a head
    /// that re-attaches, resyncs or switches back mid-turn is handed one paragraph twice, and
    /// nothing in the stream will ever take the pane's copy away: the body it waits for has
    /// already landed, and the next `TurnStarted` is a whole round away.
    ///
    /// The operator photographed exactly that, 2026-10-30: a reply drawn above their question and
    /// again below it, the second copy carrying the counts — which is how it is known that the
    /// second is the LIVE one: the pane draws its marker *after* the reply (`ui::transcript::
    /// live`), so the copy with `[1 tool call, 36 thinking lines]` attached is the pane's.
    pub(crate) fn prose_taken_over(
        &self,
        items: &[letibot_sessionlog::view::SnapshotItem],
    ) -> bool {
        self.a_row_holds(self.text.raw(), items, |it| match it {
            letibot_transcript::TranscriptItem::Assistant { text, .. } => Some(text.as_str()),
            _ => None,
        })
    }

    /// **Give the thinking up, up to what this round's rows hold** — the handover for BOTH doors.
    ///
    /// The live door is [`App::record_item`] (a `Reasoning` row's body landing); the snapshot door
    /// is `App::load` (rows that arrived already holding their bodies, with no event behind them).
    /// Both ask the same question, so both call this.
    ///
    /// **A PREFIX, not an equality**, and that is where this parts company with
    /// [`TurnPane::prose_taken_over`]: a round can produce more than one `Reasoning` row (the model
    /// thought, spoke, thought again — `turn::items`' `only_the_last_thought_can_be_the_open_one`
    /// pins the three-item shape), while the pane accumulates a round's reasoning into one buffer.
    /// So the rows hold a prefix of it, in order, and the walk spends them oldest first against the
    /// front of the buffer, stopping at the first one that is not a prefix of what is left. A row
    /// that is missing, bodyless, out of order or simply not this round's therefore leaves the rest
    /// of the text exactly where it was.
    ///
    /// **The failure is a duplicate, never a hole.** A mismatch stops the walk rather than clearing
    /// anything, because the two mistakes are not equal: text drawn twice is what the operator
    /// reports, and text drawn nowhere is a thought they watched arrive that has gone. That is also
    /// why the comparison is exact bytes — the pane's copy has been through `without_control_lines`
    /// and a row's has not, and a sanitised delta is a mismatch that leaves the words on the screen
    /// rather than swallowing them.
    ///
    /// **Re-seated rather than sliced, and this is what the frozen prefix forced.**
    /// `IncrementalMarkdown` settles blocks and never re-lexes them, so its `raw()` is a document
    /// with a frozen half and there is no `raw_from(n)` to hand the renderer: a slice of it would
    /// be a *different* document (a fence opened above the cut, a list numbered from the middle)
    /// and would need its own lexer and its own cache anyway. Re-seating is the same trade the
    /// reply makes one field up — [`App::record_item`] clears `text` rather than slicing it — and
    /// it costs one lex of what is left, at a row's landing: at most once per reasoning row, and
    /// normally zero bytes, because a round's thinking lands whole.
    ///
    /// **And the count needs no second mark.** `live_work` measures `reasoning.raw()` and
    /// `ui::transcript::live` draws it; with the landed part gone from the buffer, one string is
    /// the whole answer to both — which is the count and the fold agreeing by construction rather
    /// than by two pieces of arithmetic that happen to match.
    ///
    /// Returns whether anything was handed over, so a caller can say so rather than guess.
    pub(crate) fn hand_over_reasoning(
        &mut self,
        items: &[letibot_sessionlog::view::SnapshotItem],
    ) -> bool {
        let mut rest = self.reasoning.raw();
        let mut cursor = self.reasoned_rows;
        loop {
            let Some(id) = self.appended.get(cursor) else {
                break;
            };
            let Some(row) = items.iter().find(|r| &r.item_id == id) else {
                // The row is not in the window at all (a snapshot replaced it). It says nothing
                // about the text either way, so it is stepped over rather than stopping the walk.
                cursor += 1;
                continue;
            };
            let Some(item) = row.item.as_ref() else {
                // Announced and its body is not here yet. Bodies land in the order the rows were
                // announced, so the walk waits here rather than spending a later row's words.
                break;
            };
            match item {
                letibot_transcript::TranscriptItem::Reasoning { text, .. } => {
                    match rest.strip_prefix(text.as_str()) {
                        Some(tail) => {
                            rest = tail;
                            cursor += 1;
                        }
                        None => break,
                    }
                }
                // A row of any other kind is not thinking: stepped over.
                _ => cursor += 1,
            }
        }
        let landed = self.reasoning.raw().len() - rest.len();
        if landed == 0 {
            return false;
        }
        let rest = rest.to_string();
        self.reasoning = rano::markdown::IncrementalMarkdown::new();
        self.reasoning.push(&rest);
        // The cache goes with the document for the same reason the blocks do: it holds ROWS of
        // the document it was given, and this is not that document any more.
        self.reasoning_cache = BlockCache::new();
        self.reasoned_rows = cursor;
        true
    }

    /// **Is `words` a body this round's rows already carry?** — the rule behind the PROSE's mark.
    ///
    /// **The pairing is by TEXT and only within `appended`**, because that is the whole of what a
    /// snapshot proves. `appended` is this round's rows — the round whose deltas this pane is
    /// holding — so a row from an earlier round cannot be mistaken for the one that owns the
    /// words, even when the model repeated a sentence verbatim. And equality after trimming, not
    /// a prefix: clearing on a prefix would hide words no row holds, which is worse than the
    /// duplicate this exists to remove. A row still waiting for its body matches nothing, so the
    /// R2 window (a row announced, its words not yet here) is untouched — the live door closes
    /// that one when the body lands.
    ///
    /// **This is the PROSE's rule, and the thinking's is a prefix walk** —
    /// [`TurnPane::hand_over_reasoning`]. The difference is what one row can hold: a round commits
    /// one assistant paragraph and as many `Reasoning` blocks as the model opened, so the reply is
    /// either wholly in a row or not at all, while the thinking is spent a block at a time.
    fn a_row_holds(
        &self,
        words: &str,
        items: &[letibot_sessionlog::view::SnapshotItem],
        body: impl Fn(&letibot_transcript::TranscriptItem) -> Option<&str>,
    ) -> bool {
        let words = words.trim();
        if words.is_empty() {
            return false;
        }
        self.appended.iter().any(|id| {
            items.iter().any(|r| {
                &r.item_id == id
                    && r.item
                        .as_ref()
                        .and_then(&body)
                        .is_some_and(|t| t.trim() == words)
            })
        })
    }

    /// **Give the prose up if a row holds it** — the snapshot's half of the handover.
    ///
    /// Returns whether it did, so a caller can say so rather than guess. The cache goes with the
    /// text for [`App::record_item`]'s reason: `IncrementalMarkdown` is a frozen-prefix lexer, so
    /// the rendered blocks are the same fact in another form and cannot outlive it.
    pub(crate) fn hand_over_prose(
        &mut self,
        items: &[letibot_sessionlog::view::SnapshotItem],
    ) -> bool {
        if !self.prose_taken_over(items) {
            return false;
        }
        self.text = rano::markdown::IncrementalMarkdown::new();
        self.text_cache = BlockCache::new();
        true
    }
}

/// One compaction half, as the daemon reports it.
#[derive(Debug, Clone)]
pub(crate) struct CompactionLine {
    pub(crate) half: u64,
    pub(crate) halves: u64,
    pub(crate) prompt_tokens: u64,
    pub(crate) processed: u64,
    pub(crate) written: u64,
    /// What `written` counts — `tokens` or `chars`, the daemon's word, because the two
    /// transports do not report the same thing.
    pub(crate) unit: String,
}

/// **A bulk announcement: the ids a snapshot carried with no body, and when it landed.**
///
/// The evidence, not a symptom. A snapshot that arrives full of body-less rows is a
/// *carry* — a fork, a reseat, a resume, an import, or an attach to a daemon mid-carry —
/// and this is how the head knows that, because it recorded it at the moment of ingestion.
/// A live `TranscriptAppended` never creates one: it is the R2 window of an ordinary
/// message, which is why *"some row lacks a body"* was the wrong trigger.
#[derive(Debug, Clone)]
pub(crate) struct Bulk {
    /// The ids the snapshot announced with no body. A body landing removes its id; an
    /// empty set means the announcement is complete and the trigger clears itself.
    pub(crate) ids: std::collections::HashSet<String>,
    /// How many the snapshot announced — `ids` as it was when it landed, so the status row can
    /// say how many of them have arrived rather than only how many are still missing.
    pub(crate) announced: usize,
    /// When the snapshot landed, on this head's clock — [`BODY_PATIENCE`]'s origin.
    pub(crate) at_ms: u64,
}

pub(crate) fn open_call<'a>(calls: &'a mut [CallRow], call_id: &str) -> Option<&'a mut CallRow> {
    calls
        .iter_mut()
        .rev()
        .find(|c| c.call_id == call_id && !matches!(c.state, CallState::Finished { .. }))
}

impl App {
    /// **Is the model GENERATING right now?** The state name, and only that.
    ///
    /// This is the narrow question and it is almost never the one a caller means. Read
    /// [`App::turn_busy`] first: `TurnFinished` fires per ROUND, so this goes false the instant a
    /// round's generation ends — **which is precisely when a tool call starts.** A reader asking
    /// *is the model working* who reaches for this gets *no* for the whole of every command.
    ///
    /// It is kept because one caller genuinely asks the generating question: [`App::turn_slow`],
    /// which reads silence from a model that should be emitting. A call that runs for two
    /// minutes emits nothing and is not stuck, and gating that on `turn_busy` would make it
    /// cry wolf through every long command.
    pub(crate) fn turn_generating(&self) -> bool {
        matches!(
            self.turn.as_ref().and_then(|t| t.state.as_ref()),
            Some(TurnState::Running)
        )
    }

    /// **Is the model WORKING — generating, or waiting on a call it made?**
    ///
    /// This is R51's `turn-busy-p`, and it is the question four of this head's call sites were
    /// silently asking with the state name instead. Measured on 2026-09-25 with a `sleep 60`
    /// executing, the daemon's own view reads:
    ///
    /// ```text
    /// (:TURN-STATE "finished"  :CALLS (("call_…" "running")))
    /// ```
    ///
    /// — *generating* is false and the turn is plainly working, so **every question of the form
    /// "is the model busy" must ask the CALLS.** The fact is: generating, OR any call of this
    /// turn has not finished.
    ///
    /// **What it is not.** It is not "a call exists" and not "a call is running": a call that has
    /// finished is history, and the calls of earlier rounds are in `calls` until the pane stands
    /// down.
    ///
    /// **The window R51 §3 recorded is closed, by the daemon.** It used to say *not busy* for the
    /// few milliseconds between a last call finishing and the next round's `TurnStarted`, and that
    /// was a limitation of the round-level events rather than of this predicate: *the prompt is
    /// over* is a fact only the daemon has, and `PROTOCOL_VERSION` 52 publishes it —
    /// `Waiting::Turn` is the PROMPT's span, from the harness taking it to its loop returning. So
    /// the activity is asked first and the round-level state below is the fallback, for a daemon
    /// older than 52 (whose value is always quiet).
    ///
    /// **One definition, because this colour and this line have now been wrong in four
    /// directions** — see the call sites: the esc-esc gate dead exactly while a command ran, two
    /// `queued` rows for one message, the promote message saying the wrong one of two silences,
    /// and a status row that vanished during every call.
    pub(crate) fn turn_busy(&self) -> bool {
        // **The daemon's answer first** (`PROTOCOL_VERSION` 52) — see the paragraph above: `Turn`
        // is the prompt's span and `Calls` the round detail inside it, and together they hold this
        // row up across every round boundary of one prompt.
        if self
            .activity
            .waiting_on()
            .iter()
            .any(|w| matches!(w, Waiting::Turn | Waiting::Calls))
        {
            return true;
        }
        let Some(t) = self.turn.as_ref() else {
            return false;
        };
        if matches!(t.state, Some(TurnState::Running)) {
            return true;
        }
        t.calls
            .iter()
            .any(|c| !matches!(c.state, CallState::Finished { .. }))
    }
}
