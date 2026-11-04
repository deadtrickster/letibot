//! The turn in flight: its calls, its rows, whether the model is generating or waiting on
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
    /// The §4.1 display target: the path, pattern or command line the call is about. Empty when the
    /// event carried none (an older log, or a call first seen as `ToolStarted`), and the card then
    /// renders the verb alone rather than a guess.
    pub(crate) target: String,
    pub(crate) state: CallState,
    /// `Envelope::ts` of the proposal or the start, and of the finish. Zero means this call came out
    /// of a snapshot, which has no timestamps: a duration invented from a clock the events were not
    /// measured against is worse than none, so that case renders as `card::Phase::Replayed`.
    pub(crate) started_ms: u64,
    /// The head's own clock when this call started, or 0 when the head had no clock then: `started_ms`
    /// is the LOG's clock, which stops while a call runs (no event, no new timestamp), so the elapsed
    /// read off it sat at `0ms` through a `cargo build`. A `--replay` sets no clock, so 0 stays honest.
    pub(crate) started_at: u64,
    pub(crate) ended_ms: u64,
    /// The most recent `ToolProgress { note }`.
    pub(crate) note: Option<String>,
    /// The settled decision this call was gated by, when there was one. Rendered on
    /// the card in the dim register - the approval is a fact about the call, not a
    /// stray note - and expanded to the brief the oracle saw and its reply.
    pub(crate) decision: Option<SettledDecision>,
}

#[derive(Debug, Default)]
pub(crate) struct TurnPane {
    pub(crate) turn_id: String,
    pub(crate) text: IncrementalMarkdown,
    /// The thinking no row holds YET - not the whole round's reasoning. A `Reasoning` row is appended
    /// with the same words, and [`TurnPane::hand_over_reasoning`] spends what the rows hold off the
    /// front of this buffer at the row's body, so what is left is what the pane draws and counts.
    pub(crate) reasoning: IncrementalMarkdown,
    pub(crate) text_cache: BlockCache,
    pub(crate) reasoning_cache: BlockCache,
    pub(crate) calls: Vec<CallRow>,
    /// The raw `<function=...>` markup of this turn's tool calls, as it arrives on
    /// `DeltaTarget::ToolCall`. Kept and never shown by default: it is what the raw chord reveals, so
    /// the chord can tell the truth instead of re-deriving markup the head never saw.
    pub(crate) raw_call: String,
    /// True between `<tool_call>` and the `ToolCallProposed` that settles it. A separate fact from
    /// `raw_call.is_empty()`: a turn that wrote one call and is now writing prose has a non-empty
    /// `raw_call` and is not in a call.
    pub(crate) writing_call: bool,
    pub(crate) progress: Option<letibot_sessionlog::event::PromptProgress>,
    pub(crate) state: Option<TurnState>,
    /// Transcript rows appended while this turn ran - per ROUND, since each round builds a fresh pane.
    /// Once the turn has ended and those rows carry their content the transcript is authoritative, so
    /// the pane stands down and only its summary line survives; otherwise the answer is drawn twice.
    pub(crate) appended: Vec<String>,
    /// Every row this TURN has produced across all rounds. `appended` is per ROUND (each `TurnStarted` builds a fresh `TurnPane`), right for
    /// what the live pane still draws and whether the turn is superseded, but wrong for *does this run hold a row of the current turn*:
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
    /// Because round 3 forgot what rounds 1 and 2 did: the run of committed rows stopped reading as this turn's, so `live_here` and
    /// `walk_carried_live` answered *no* and the pane drew its reasoning beside the run - two markers, two thinking counts.
    /// One turn is one `began_ms`, carried on every round's `TurnStarted`. Cleared only when the turn is PROVEN to have changed: a
    /// boundary with no `began_ms` is *nobody measured this one*, not *a new prompt*.
    pub(crate) turn_rows: Vec<String>,
    /// How many of `calls` the transcript has taken over: rows are appended in call order after every call was invoked, so the nth row is about `calls[n-1]` with no id matching, the ids repeating every round.
    /// The mark advances at the row's BODY, not its announcement - a bodyless row draws nothing, so the call would be in neither half for the frames between - and the live cards, `live_work` and `round_answered` all read it.
    pub(crate) settled_calls: usize,
    /// How far down [`TurnPane::appended`] this pane's thinking has been handed over - an index, not a
    /// byte offset into [`TurnPane::reasoning`]: `IncrementalMarkdown` freezes its settled half, so a
    /// slice after byte N is a different document. One buffer, so the count and the fold cannot disagree.
    pub(crate) reasoned_rows: usize,
    /// The `ts` of `TurnStarted` and of the last event seen for this turn. The difference is the elapsed
    /// time, taken from the log's own clock: a head reading a recorded session must show the same
    /// elapsed time as the one that watched it.
    pub(crate) started_ms: u64,
    pub(crate) last_ms: u64,
    /// The `ts` of the first and last `Delta { target: Reasoning }`, which is where
    /// `card::reasoning`'s `Thought for 4.2s` comes from. Zero means the reasoning arrived in a
    /// snapshot and has no honest duration.
    pub(crate) think_started_ms: u64,
    pub(crate) think_last_ms: u64,
    /// The daemon's word on the last finish this pane saw: `Some(false)` is a ROUND that ended with the prompt still
    /// running, so the row stays up. `None` until one arrives: a snapshot carries a state and no finish, and reading
    /// that as *the turn continues* would leave a row nobody can stand down.
    pub(crate) ends_turn: Option<bool>,
}

impl TurnPane {
    /// Does a row this round appended already hold the prose this pane is drawing? The live door closes
    /// the pane's copy when a body lands, but a SNAPSHOT is a replacement: it carries the turn's text
    /// beside rows that already hold their bodies, so a re-attach or a resync is handed it twice.
    pub(crate) fn prose_taken_over(
        &self,
        items: &[letibot_sessionlog::view::SnapshotItem],
    ) -> bool {
        self.a_row_holds(self.text.raw(), items, |it| match it {
            letibot_transcript::TranscriptItem::Assistant { text, .. } => Some(text.as_str()),
            _ => None,
        })
    }

    /// Give the thinking up, up to what this round's rows hold - the handover for BOTH doors (a row's body landing, and `App::load`'s rows that arrived with bodies). A PREFIX walk, not equality: a round can produce several `Reasoning` rows while the pane holds one buffer.
    /// A mismatch stops the walk, so the failure is a duplicate and never a hole, and the comparison is exact bytes because only the pane's copy went through `without_control_lines`.
    /// Re-seated rather than sliced: `IncrementalMarkdown` freezes its settled half, so "draw after byte N" is a different document - and one buffer is why the count and the fold agree.
    pub(crate) fn hand_over_reasoning(
        &mut self,
        items: &[letibot_sessionlog::view::SnapshotItem],
    ) -> bool {
        let mut rest = self.reasoning.raw();
        let mut cursor = self.reasoned_rows;
        while let Some(id) = self.appended.get(cursor) {
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

    /// Is `words` a body this round's rows already carry? - the rule behind the PROSE's mark. Paired by
    /// TEXT and only within `appended`, so a row of an earlier round cannot own the words even when the
    /// model repeated a sentence; equality after trimming, since clearing on a prefix would hide text.
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

    /// Give the prose up if a row holds it - the snapshot's half of the handover. The cache goes with the
    /// text: `IncrementalMarkdown` is a frozen-prefix lexer, so the rendered blocks are the same fact in
    /// another form and cannot outlive it.
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
    /// What `written` counts - `tokens` or `chars`, the daemon's word, because the two
    /// transports do not report the same thing.
    pub(crate) unit: String,
}

/// A bulk announcement: the ids a snapshot carried with no body, and when it landed - the evidence of
/// a carry (a fork, a reseat, a resume, an import, an attach mid-carry), recorded at ingestion. A live
/// `TranscriptAppended` never makes one, so "some row lacks a body" is not the trigger.
#[derive(Debug, Clone)]
pub(crate) struct Bulk {
    /// The ids the snapshot announced with no body. A body landing removes its id; an
    /// empty set means the announcement is complete and the trigger clears itself.
    pub(crate) ids: std::collections::HashSet<String>,
    /// How many the snapshot announced - `ids` as it was when it landed, so the status row can
    /// say how many of them have arrived rather than only how many are still missing.
    pub(crate) announced: usize,
    /// When the snapshot landed, on this head's clock - [`BODY_PATIENCE`]'s origin.
    pub(crate) at_ms: u64,
    /// What the STORE said about each position, asked by session ordinal through `FetchRows` - the
    /// door that falls through to the store when the view cannot answer. A snapshot is cut from the daemon's
    /// in-memory view, so a resync re-cuts the same body-less row; only a re-seat rebuilds it.
    pub(crate) held: std::collections::HashSet<String>,
    /// The ids the store answered nothing for - a row nothing the daemon can reach holds.
    /// Not the same statement as `held`, and the two must not look alike.
    pub(crate) absent: std::collections::HashSet<String>,
    /// The ids this head has already asked about, so one snapshot is one question per id and
    /// a resync does not flood the daemon with the same probes.
    pub(crate) asked: std::collections::HashSet<String>,
}

pub(crate) fn open_call<'a>(calls: &'a mut [CallRow], call_id: &str) -> Option<&'a mut CallRow> {
    calls
        .iter_mut()
        .rev()
        .find(|c| c.call_id == call_id && !matches!(c.state, CallState::Finished { .. }))
}

impl App {
    /// Is the model GENERATING right now? The state name, and only that: `TurnFinished` fires per ROUND,
    /// so this goes false the instant a round's generation ends, which is when a tool call starts. Read
    /// [`App::turn_busy`] for working; [`App::turn_slow`] is the one caller that genuinely asks this.
    pub(crate) fn turn_generating(&self) -> bool {
        matches!(
            self.turn.as_ref().and_then(|t| t.state.as_ref()),
            Some(TurnState::Running)
        )
    }

    /// Is the model WORKING - generating, or waiting on a call it made?
    ///
    /// The question the call sites must ask instead of the state name, as this daemon-side view shows for a
    /// `sleep 60` in flight:
    ///
    /// ```text
    /// (:TURN-STATE "finished"  :CALLS (("call_…" "running")))
    /// ```
    ///
    /// The turn is plainly working while *generating* is false, so "is the model busy" must ask the CALLS:
    /// generating, OR any call of this turn has not finished. Not "a call exists" and not "a call is running":
    /// a finished call is history. `Waiting::Turn` is the prompt's span (`PROTOCOL_VERSION` 52), asked first;
    /// the round-level state below is the fallback for an older daemon. One definition, so the colour and the
    /// status line cannot disagree.
    pub(crate) fn turn_busy(&self) -> bool {
        // The daemon's answer first (`PROTOCOL_VERSION` 52) - see the paragraph above: `Turn`
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
        // And the same boundary from the frame that ended the round (`TurnFinished::ends_turn`):
        // a round whose calls have all landed is not the prompt's end, and the daemon said so. This
        // is what closes the window without the activity, which a head that attached late has none of.
        if t.ends_turn == Some(false) {
            return true;
        }
        t.calls
            .iter()
            .any(|c| !matches!(c.state, CallState::Finished { .. }))
    }
}
