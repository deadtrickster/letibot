//! The materialized view, and the snapshot cut from it.
//!
//! §13.2: *"`since_seq = 0` gets a snapshot — a materialized view of messages and
//! parts as of seq S, maintained by the daemon — followed by events from S+1. A
//! head that joins a 300-turn session never replays 300 turns of deltas."*
//!
//! Maintained incrementally, one `apply` per appended event, so `attach` is O(size
//! of the answer) and not O(length of the log). That matters for the same reason
//! §13.3 does: a cost that grows with the session is a cost that shows up only in
//! the session you cared about.
//!
//! # The view **is** the stored projection
//!
//! The scrub is not a filter bolted onto the exit. It is applied here, at
//! materialization, which is what makes it a projection:
//!
//! - `PromptProgress` never becomes history. It updates a `progress` field that is
//!   *cleared* when the turn ends. A late head attaching mid-prefill is told where
//!   the prefill is — current state, true right now — and a head attaching an hour
//!   later is told nothing, because there is nothing true to tell.
//! - `ToolProgress` does not touch the view at all. Partial tool output has no
//!   durable form; a running tool renders as *running*, with no body.
//! - `DecisionRequested` opens an entry; `DecisionAnswered` **removes** it and
//!   records the outcome. So the snapshot cannot contain a settled decision as an
//!   open prompt, because there is no code path that would put it there.
//!
//! That is the difference between "we remember to strip it on the way out" and
//! "there is nowhere for it to be".
//!
//! # `TranscriptAppended` carries no content, and the snapshot was not enough
//!
//! §4.5's event is `{item_id, kind, ledger_head}`. A head cannot reconstruct a
//! conversation from that, and the snapshot was what §13.2 promised it instead.
//!
//! That promise only covers a head that attaches *after* the row exists. A head
//! already attached is told a row landed and is never sent its body, so its
//! `item: None` is permanent — not a loading state, the final state. Measured on a
//! live session: the operator's own prompt rendered as
//! `[user … — content not loaded]` for the life of the head.
//!
//! So the body now travels on the log as
//! [`crate::event::SessionEvent::TranscriptContent`], and this view folds it in the
//! same place it folds everything else. [`SessionView::record_item`] survives as
//! the in-process form of that fold; [`crate::hub::Hub::record_item`] is the one a
//! daemon should call, because it publishes as well as folds. See the variant's
//! own note for why the body belongs on `TranscriptAppended` itself and what has to
//! change for it to get there.

use serde::{Deserialize, Serialize};

use letibot_transcript::{ToolOutcome, TranscriptItem};

use crate::event::{
    Decider, DecisionOption, DecisionOutcome, Envelope, FinishReason, OnTimeout, PromptProgress,
    SessionEvent, Timings, Usage,
};

/// One row of the conversation, as a head sees it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotItem {
    pub item_id: String,
    pub kind: String,
    pub ledger_head: String,
    /// `Envelope::ts` of the `TranscriptAppended` that announced this row: when it
    /// happened, on the log's own clock.
    ///
    /// A head renders it as the row's timestamp. **Zero means unknown** — a row
    /// replayed from a log recorded before this field existed — and a head shows
    /// nothing rather than 01:00:00, for the same reason a card from a snapshot
    /// shows no duration: a measurement that was never taken must not be rendered
    /// as one that was.
    #[serde(default)]
    pub ts: u64,
    /// The content, when the daemon supplied it. `None` is honest: it means the
    /// event was seen and the item was not reconciled, which a head should render
    /// as a placeholder rather than as an empty message.
    pub item: Option<TranscriptItem>,
}

/// A decision that is still owed an answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenDecision {
    pub req_id: String,
    pub kind: String,
    pub call_id: Option<String>,
    pub summary: String,
    /// The one thing being decided about — the command, the path, the URL — so a
    /// head can put it on its own line. See the event for why it is not left inside
    /// `summary`.
    #[serde(default)]
    pub target: String,
    /// Layer A's reading, for the dim line under the question.
    #[serde(default)]
    pub detail: String,
    /// The allow/deny ladder, for a permission. Empty for a question.
    pub options: Vec<DecisionOption>,
    /// The model's own plain-text choices, for a question (T25/D10). Empty for a
    /// permission. See [`crate::event::SessionEvent::DecisionRequested`] for why the
    /// two do not share a field.
    #[serde(default)]
    pub choices: Vec<String>,
    /// Why the model is stuck, for a question.
    #[serde(default)]
    pub because: String,
    /// The model's verdict on this permission, at `/mode supervised`. `None`
    /// everywhere else. Carried into the view so a head attaching mid-question
    /// renders the same thing a head that was there from the start does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advice: Option<crate::event::ModelAdvice>,
    pub deadline: Option<u64>,
    pub on_timeout: OnTimeout,
    /// When it was asked, so a head can show how long it has been waiting rather
    /// than presenting a ten-minute-old question as if it were new.
    pub asked_ts: u64,
}

/// A decision that has settled, kept so a head can render the outcome.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettledDecision {
    pub req_id: String,
    /// The tool call this decision was about, when it was a permission. `None` for
    /// a question, and for a log recorded before the field existed — in which case
    /// a head renders the outcome as a standalone note rather than on the call's
    /// card.
    #[serde(default)]
    pub call_id: Option<String>,
    pub summary: String,
    pub outcome: DecisionOutcome,
    pub by: Decider,
    /// **Why the DECIDER decided.** For an operator answer this is
    /// `dead chose \`allow_once\` at the head`; for a boundary refusal it is the
    /// rule and its evidence. It is not the oracle's reasoning, even when an
    /// oracle advised — see [`SettledDecision::advice`], which is.
    pub basis: String,
    /// **What the guard model said, when one was consulted.**
    ///
    /// Carried off the open decision at settle time, because the answer event
    /// has only the `req_id` and this is the one fact about a supervised
    /// decision that exists nowhere else afterwards: the corpus keeps it, but a
    /// head is not reading the corpus.
    ///
    /// It was dropped here until 2026-09-19, and the card then labelled `basis`
    /// as `oracle:` — so a permission the operator answered themselves rendered
    /// THEIR OWN words under the oracle's name. `None` is honest: no oracle was
    /// consulted, or the log was recorded before the field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advice: Option<crate::event::ModelAdvice>,
    pub late: bool,
}

/// A tool call the model proposed, and where it got to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallView {
    pub call_id: String,
    pub name: String,
    pub args_digest: String,
    /// The display target from `ToolCallProposed` (§4.1). Empty when the call was
    /// first seen as `ToolStarted`, or when it came from a log recorded before the
    /// field existed — in which case a head renders the verb alone rather than
    /// guessing.
    #[serde(default)]
    pub target: String,
    pub state: CallState,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CallState {
    Proposed,
    /// Running, with **no partial output**. See the module note.
    Running,
    Finished {
        outcome: ToolOutcome,
        payload_digest: String,
        /// What the model received. Under §8.3's spill policy this is smaller than
        /// `full_bytes`, and a head that shows only one of the two is showing the
        /// wrong one half the time.
        inline_bytes: u64,
        full_bytes: u64,
        /// The spill locator, when the output spilled: a head can offer the rest.
        spill: Option<String>,
        /// Both sides of the file a file-editing call changed, bounded to the
        /// region that differs — the raw material of the two-panel diff. A
        /// snapshot recorded before the field existed has none, and
        /// `#[serde(default)]` is what lets it still load.
        #[serde(default)]
        edit: Option<crate::event::ToolEdit>,
    },
}

/// How the turn in view ended, if it has.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TurnState {
    Running,
    Finished {
        finish_reason: FinishReason,
        usage: Usage,
        timings: Timings,
    },
    Interrupted {
        reason: String,
        partial_kept: bool,
    },
    /// The turn failed. A *terminal* state, which is the whole point of it
    /// existing: without one, `Running` was the last thing a head ever heard about
    /// a turn that died (`crates/ui/DESIGN.md` §4.5).
    Failed {
        error: String,
        partial_kept: bool,
    },
}

impl TurnState {
    /// Whether the turn is still going. One function, because "is it running" is
    /// asked in the head, in the registry's session list and in the snapshot, and
    /// a new terminal state that one of the three forgets is a spinner nobody can
    /// stop.
    pub fn is_running(&self) -> bool {
        matches!(self, TurnState::Running)
    }
}

/// The turn a late head is joining, with its text **accumulated once**.
///
/// This is the half of §13.3 that lives on the wire: the head does not replay
/// N deltas to learn the text, it is handed the text and then receives increments.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnView {
    pub turn_id: String,
    pub model: String,
    pub ledger_head: String,
    pub text: String,
    pub reasoning: String,
    /// The raw `<function=…>` markup of every tool call this turn has written so
    /// far, concatenated.
    ///
    /// Accumulated for the same reason `text` is: a head that joins mid-call has
    /// to be able to show the same thing as one that watched it. It is the
    /// *unparsed* form and no default view renders it — `calls` is the settled
    /// form — but the head keeps a chord that reveals it, and a snapshot that
    /// dropped it would make that chord lie on a reattach.
    #[serde(default)]
    pub raw_calls: String,
    pub calls: Vec<CallView>,
    /// The transcript rows this turn appended, in order.
    ///
    /// A head shows a running turn from `text`/`reasoning` and a finished one from
    /// the transcript, and it needs to know *which* rows are the finished form or
    /// it renders the answer twice — once live, once as history. That pairing is
    /// only visible to whoever folded both event streams, which is this view.
    #[serde(default)]
    pub appended: Vec<String>,
    /// Live prefill state. `None` once the turn has ended — a progress frame is
    /// true only while it is happening.
    pub progress: Option<PromptProgress>,
    /// The server's generation counter, as the last `TokensGenerated` reported it.
    ///
    /// Kept when the turn ends, unlike `progress`: a cumulative count stays true
    /// afterwards, and its final value is the fact `TurnFinished`'s
    /// `usage.predicted_tokens` states. `#[serde(default)]` for snapshots taken
    /// before the field existed.
    #[serde(default)]
    pub tokens: u64,
    pub state: TurnState,
}

/// A head the daemon believes is attached.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeadPresence {
    pub head_id: String,
    pub kind: String,
    pub identity: String,
}

/// What a late head is handed. Everything true as of `seq`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub session_id: String,
    /// The snapshot is true as of this seq. The next event a head receives is
    /// `seq + 1`, with no gap — see [`crate::hub::Hub::attach`].
    pub seq: u64,
    /// Events that fell off the back of the scrollback and this head will never
    /// see. **Present and zero**, never omitted (§13.2b).
    pub dropped: u64,
    /// Transcript rows trimmed out of this snapshot. Present and zero, same rule.
    pub items_dropped: u64,
    pub items: Vec<SnapshotItem>,
    pub turn: Option<TurnView>,
    /// Only decisions genuinely still open. A settled one cannot appear here.
    pub open_decisions: Vec<OpenDecision>,
    /// Recent outcomes, so a head can render "denied, by policy" rather than
    /// silence where a prompt used to be.
    pub settled_decisions: Vec<SettledDecision>,
    pub warnings: Vec<Warned>,
    pub heads: Vec<HeadPresence>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Warned {
    pub code: String,
    pub detail: String,
    pub ts: u64,
}

/// How much of each unbounded thing the view keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewBounds {
    pub items: usize,
    /// How many bytes of row text the view will hold before it drops the oldest.
    ///
    /// **`items` alone bounds a count, and rows are not the same size.** The operator's
    /// sessions reach 160 MB across thousands of turns, and one `read` row can be 418 KB
    /// — so 2,000 rows of those is most of a gigabyte, cloned for every attaching head
    /// and sent over the socket. Measured 2026-09-20: eight rows over 64 KB in one store,
    /// the largest 418 KB.
    ///
    /// This is the first half of R19 in `letibot`'s TODO — the head renders only the tail
    /// of a conversation (R19.1), and a window needs something bounded to be a window
    /// over. A head is told what was dropped through `items_dropped` either way.
    pub item_bytes: usize,
    pub settled_decisions: usize,
    pub warnings: usize,
}

impl Default for ViewBounds {
    fn default() -> Self {
        ViewBounds {
            items: 2_000,
            // An order of magnitude above any session this box has today (the largest is
            // ~24 MB in the store, of which the snapshot carries a fraction), and far
            // below the 160 MB the operator reports elsewhere. Chosen so that a normal
            // session is untouched and a pathological one is bounded.
            item_bytes: 8 * 1024 * 1024,
            // A decision is a fact about the tool call it gated, and a head renders
            // it on that call's card — including a card that has settled into the
            // transcript, which is where most of the session's history lives. A
            // head that reattaches two compactions in must still find the decision
            // that gated a call three turns back, so the window has to span a long
            // session's worth of gates, not just the last handful. Each entry is a
            // few hundred bytes, so a thousand of them is a small fraction of the
            // snapshot the `items` bound already carries.
            settled_decisions: 1_024,
            warnings: 128,
        }
    }
}

/// The daemon's materialized view of one session.
#[derive(Debug)]
pub struct SessionView {
    session_id: String,
    bounds: ViewBounds,
    items: Vec<SnapshotItem>,
    items_dropped: u64,
    turn: Option<TurnView>,
    open: Vec<OpenDecision>,
    settled: Vec<SettledDecision>,
    warnings: Vec<Warned>,
    heads: Vec<HeadPresence>,
}

impl SessionView {
    pub fn new(session_id: impl Into<String>, bounds: ViewBounds) -> Self {
        SessionView {
            session_id: session_id.into(),
            bounds,
            items: Vec::new(),
            items_dropped: 0,
            turn: None,
            open: Vec::new(),
            settled: Vec::new(),
            warnings: Vec::new(),
            heads: Vec::new(),
        }
    }

    /// Fold one appended event into the view.
    ///
    /// The exhaustive match is the scrub's other half: a new event variant cannot
    /// be added without deciding, here, what it means for a head that was not
    /// watching.
    pub fn apply(&mut self, env: &Envelope) {
        match &env.event {
            SessionEvent::TurnStarted {
                turn_id,
                model,
                ledger_head,
            } => {
                self.turn = Some(TurnView {
                    turn_id: turn_id.clone(),
                    model: model.clone(),
                    ledger_head: ledger_head.clone(),
                    text: String::new(),
                    reasoning: String::new(),
                    raw_calls: String::new(),
                    calls: Vec::new(),
                    appended: Vec::new(),
                    progress: None,
                    tokens: 0,
                    state: TurnState::Running,
                });
            }
            SessionEvent::PromptProgress { turn_id, progress } => {
                // State, not history: overwritten, and cleared when the turn ends.
                if let Some(t) = self.turn.as_mut()
                    && &t.turn_id == turn_id
                {
                    t.progress = Some(*progress);
                }
            }
            SessionEvent::TokensGenerated { turn_id, tokens } => {
                // State, not history: the latest count wins. The counter is
                // monotonic in the stream, and `max` keeps a reordered or
                // duplicated frame from moving it backwards.
                if let Some(t) = self.turn.as_mut()
                    && &t.turn_id == turn_id
                {
                    t.tokens = (*tokens).max(t.tokens);
                }
            }
            SessionEvent::Delta {
                turn_id,
                target,
                text,
            } => {
                if let Some(t) = self.turn.as_mut()
                    && &t.turn_id == turn_id
                {
                    match target {
                        crate::event::DeltaTarget::Text => t.text.push_str(text),
                        crate::event::DeltaTarget::Reasoning => t.reasoning.push_str(text),
                        crate::event::DeltaTarget::ToolCall => t.raw_calls.push_str(text),
                    }
                }
            }
            SessionEvent::ToolCallProposed {
                turn_id,
                call_id,
                name,
                args_digest,
                target,
            } => {
                if let Some(t) = self.turn.as_mut()
                    && &t.turn_id == turn_id
                {
                    t.calls.push(CallView {
                        call_id: call_id.clone(),
                        name: name.clone(),
                        args_digest: args_digest.clone(),
                        target: target.clone(),
                        state: CallState::Proposed,
                    });
                }
            }
            SessionEvent::DecisionRequested {
                req_id,
                kind,
                call_id,
                summary,
                target,
                detail,
                options,
                choices,
                because,
                advice,
                deadline,
                on_timeout,
            } => {
                self.open.retain(|d| &d.req_id != req_id);
                self.open.push(OpenDecision {
                    req_id: req_id.clone(),
                    kind: kind.clone(),
                    call_id: call_id.clone(),
                    summary: summary.clone(),
                    target: target.clone(),
                    detail: detail.clone(),
                    options: options.clone(),
                    choices: choices.clone(),
                    because: because.clone(),
                    advice: advice.clone(),
                    deadline: *deadline,
                    on_timeout: *on_timeout,
                    asked_ts: env.ts,
                });
            }
            SessionEvent::DecisionAnswered {
                req_id,
                outcome,
                by,
                basis,
                late,
            } => {
                // The removal is the scrub. There is no path that leaves a settled
                // decision in `open`, so no snapshot can carry one. Three things are
                // read off the open decision **before** it is removed, because the
                // answer event carries only the `req_id`: the summary, the call to
                // put the outcome on, and the ORACLE'S ADVICE — which the answer
                // event does not carry and which nothing downstream can recover.
                let (summary, call_id, advice) = self
                    .open
                    .iter()
                    .find(|d| &d.req_id == req_id)
                    .map(|d| (d.summary.clone(), d.call_id.clone(), d.advice.clone()))
                    .unwrap_or_default();
                self.open.retain(|d| &d.req_id != req_id);
                self.settled.push(SettledDecision {
                    req_id: req_id.clone(),
                    call_id,
                    summary,
                    outcome: outcome.clone(),
                    by: by.clone(),
                    basis: basis.clone(),
                    advice,
                    late: *late,
                });
                let over = self
                    .settled
                    .len()
                    .saturating_sub(self.bounds.settled_decisions);
                self.settled.drain(..over);
            }
            SessionEvent::ToolStarted { call_id, name, .. } => {
                if let Some(c) = self.call_mut(call_id) {
                    c.state = CallState::Running;
                } else if let Some(t) = self.turn.as_mut() {
                    t.calls.push(CallView {
                        call_id: call_id.clone(),
                        name: name.clone(),
                        args_digest: String::new(),
                        // No proposal was seen, so there is no target and none is
                        // invented: the verb alone is what is true here.
                        target: String::new(),
                        state: CallState::Running,
                    });
                }
            }
            // Deliberately nothing. Partial tool output has no durable form; see
            // the module note. This arm exists so that "we forgot" and "we decided"
            // do not look the same.
            SessionEvent::ToolProgress { .. } => {}
            SessionEvent::ToolFinished {
                call_id,
                outcome,
                payload_digest,
                inline_bytes,
                full_bytes,
                spill,
                edit,
                ..
            } => {
                if let Some(c) = self.call_mut(call_id) {
                    c.state = CallState::Finished {
                        outcome: outcome.clone(),
                        payload_digest: payload_digest.clone(),
                        inline_bytes: *inline_bytes,
                        full_bytes: *full_bytes,
                        spill: spill.clone(),
                        edit: edit.clone(),
                    };
                }
            }
            SessionEvent::TurnFinished {
                turn_id,
                finish_reason,
                usage,
                timings,
            } => {
                if let Some(t) = self.turn.as_mut()
                    && &t.turn_id == turn_id
                {
                    t.progress = None;
                    t.state = TurnState::Finished {
                        finish_reason: finish_reason.clone(),
                        usage: *usage,
                        timings: *timings,
                    };
                }
            }
            SessionEvent::TurnInterrupted {
                turn_id,
                reason,
                partial_kept,
            } => {
                if let Some(t) = self.turn.as_mut()
                    && &t.turn_id == turn_id
                {
                    t.progress = None;
                    t.state = TurnState::Interrupted {
                        reason: reason.clone(),
                        partial_kept: *partial_kept,
                    };
                }
            }
            // §4.5. Not matched on `turn_id`: a turn can fail *before* it published
            // a `TurnStarted` (a render or a tokenize error), and in that case the
            // pane a head is looking at is the one that has to stop spinning. The
            // id is still carried, because a head that has moved on must be able to
            // tell that this failure is not about the turn it is watching.
            SessionEvent::TurnFailed {
                turn_id,
                error,
                partial_kept,
            } => {
                if let Some(t) = self.turn.as_mut()
                    && (t.turn_id == *turn_id || turn_id.is_empty())
                {
                    t.progress = None;
                    t.state = TurnState::Failed {
                        error: error.clone(),
                        partial_kept: *partial_kept,
                    };
                }
            }
            SessionEvent::TranscriptAppended {
                item_id,
                kind,
                ledger_head,
            } => {
                self.items.push(SnapshotItem {
                    item_id: item_id.clone(),
                    kind: kind.clone(),
                    ledger_head: ledger_head.clone(),
                    ts: env.ts,
                    item: None,
                });
                self.trim();
                // Which rows this turn produced. A head cannot work this out from
                // the snapshot — the rows and the turn are separate lists — and it
                // is the fact that decides whether the live pane is still the only
                // copy of the answer or has been superseded by the transcript.
                if let Some(t) = self.turn.as_mut() {
                    t.appended.push(item_id.clone());
                }
            }
            // The body for a row the log already announced. Idempotent, and a no-op
            // for an id the view has trimmed or never saw — the same contract
            // `record_item` has, because this is that call, arriving over the wire.
            SessionEvent::TranscriptContent { item_id, item } => {
                if let Some(row) = self.items.iter_mut().find(|r| &r.item_id == item_id) {
                    row.item = Some((**item).clone());
                }
            }
            SessionEvent::HeadAttached {
                head_id,
                kind,
                identity,
            } => {
                self.heads.retain(|h| &h.head_id != head_id);
                self.heads.push(HeadPresence {
                    head_id: head_id.clone(),
                    kind: kind.clone(),
                    identity: identity.clone(),
                });
            }
            SessionEvent::HeadDetached { head_id, .. } => {
                self.heads.retain(|h| &h.head_id != head_id);
            }
            SessionEvent::Warning { code, detail } => {
                self.warnings.push(Warned {
                    code: code.clone(),
                    detail: detail.clone(),
                    ts: env.ts,
                });
                let over = self.warnings.len().saturating_sub(self.bounds.warnings);
                self.warnings.drain(..over);
            }
            // A password request is live for two minutes and answered on the
            // connection that asked; a head attaching later has nothing to do
            // with it, and a settled one is history.
            SessionEvent::SecretRequested { .. }
            | SessionEvent::SecretSettled { .. }
            | SessionEvent::ScreenRequested { .. } => {}
            // §6's plan is a full document. It belongs in the log, where a head can
            // ask for it by seq; carrying every one of them in every snapshot would
            // make the snapshot grow with the session.
            SessionEvent::Explain { .. } => {}
            // Who did what is history, and a head that was there saw it. It is not
            // state a late head needs restated: the *effect* is in the transcript
            // rows and the turn, which the snapshot already carries.
            SessionEvent::CommandIssued { .. } => {}
            // The name is carried by `SessionBrief`, which travels in `Hello` and
            // `Sessions` — so a head that attaches *after* a rename is told the
            // current name by the attach itself and needs nothing restated here.
            // Folding it into the snapshot as well would put the same fact in two
            // places with two update paths, and the picker and the header would then
            // be able to disagree about what the session is called.
            SessionEvent::SessionRenamed { .. } => {}
            // The todo list is not turn state and decides nothing: it reaches the
            // head as the event itself, and the pane keeps the latest one it saw.
            // Nothing for the view to fold.
            SessionEvent::TodosUpdated { .. } => {}
            // **Not folded into the snapshot, and the reason is bounded rather than
            // absent.** A denial is durable on the log ([`crate::scrub`] keeps it),
            // so a head that attaches later replays it in the place it happened,
            // which is where §4b wants it — beside the tool call it refused, not in
            // a summary pane. What that costs is the retention window: a denial
            // older than [`LogBounds`] falls out of the log and this snapshot never
            // held it. That is a real gap and it is written here rather than
            // covered by a second copy in a second update path, which is how the
            // picker and the header learned to disagree about a session's name.
            SessionEvent::DenialRaised { .. } => {}
            // A subagent's state is carried by the durable event itself; a late head
            // rebuilds the tree from the replayed events and the session list, so
            // there is nothing to fold into the turn view here.
            SessionEvent::Subagent { .. } => {}
            // Same shape: a job's start is the `bash` call's own `Backgrounded`
            // finish, which the call view already folds, and its end is carried by
            // the durable `JobSettled` event. The jobs pane folds both itself.
            SessionEvent::JobSettled { .. } => {}
            // Nothing to fold: a job-output window is drawn by the pane that asked
            // for it, and the view carries no window state. It is ephemeral besides
            // (`scrub::is_interactive`), so a late head never replays one.
            SessionEvent::JobOutput { .. } => {}
        }
    }

    fn call_mut(&mut self, call_id: &str) -> Option<&mut CallView> {
        self.turn
            .as_mut()?
            .calls
            .iter_mut()
            .find(|c| c.call_id == call_id)
    }

    /// **One row's body, whole, by the row's position in the session.**
    ///
    /// `row` is the session ordinal — `0` is the session's first row ever — not an index into
    /// [`Self::items`]. That is the number a head can express: it knows the rows it holds and
    /// `items_dropped` says how many came before them, so "the row above my oldest" is
    /// `items_dropped - 1`.
    ///
    /// `None` when the ordinal is outside what this view holds: trimmed by [`ViewBounds`], or
    /// past the end of the session. That distinction is the caller's to make — `FetchRow`
    /// answers `body: None` for it, because an empty body and a body nobody has must not look
    /// alike.
    ///
    /// The whole thing is returned rather than a window because the *windowing* belongs to
    /// whoever answers the frame — that is where the cap and the character-boundary arithmetic
    /// live, and a second implementation here would be a second answer to the same question.
    pub fn row_body_at(&self, row: usize) -> Option<String> {
        let idx = row.checked_sub(self.items_dropped as usize)?;
        let item = self.items.get(idx)?.item.as_ref()?;
        Some(match item {
            TranscriptItem::ToolResult { payload, .. } => payload.clone(),
            // The other variants' "body" is their text. A row a head can page is a tool
            // result in practice, but the accessor is not the place to decide that: a
            // head asking for a long answer's bytes gets them rather than a silence it
            // would have to interpret.
            TranscriptItem::Assistant { text, .. }
            | TranscriptItem::Reasoning { text, .. }
            | TranscriptItem::System { text, .. } => text.clone(),
            TranscriptItem::User { parts } => parts
                .iter()
                .filter_map(|p| match p {
                    letibot_transcript::UserPart::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
            // A segment mark renders to nothing, so it has no body to page.
            TranscriptItem::SegmentMark { .. } => String::new(),
        })
    }

    /// Attach content to an already-appended transcript row.
    ///
    /// See the module note: §4.5's `TranscriptAppended` has no content field, so
    /// this is the seam by which the daemon supplies it. Idempotent, and a no-op
    /// for an id the view has trimmed or never saw.
    pub fn record_item(&mut self, item_id: &str, item: TranscriptItem) {
        if let Some(row) = self.items.iter_mut().find(|r| r.item_id == item_id) {
            row.item = Some(item);
            // **The body can blow the byte bound on its own.** Rows are announced before
            // they are filled, so a row that arrives empty and lands 418 KB later is over
            // the bound the moment it lands — and trimming only when a *new* row is
            // announced would leave the snapshot oversized for as long as no new row came.
            self.trim();
        }
    }

    /// Drop the oldest rows until the view is inside both bounds, counting what went.
    ///
    /// A method rather than inline at the one place a row is pushed, because there are
    /// two places it can go over: a row announced (a count) and a body landing (bytes).
    ///
    /// **The newest row is kept whatever size it is.** A view with nothing in it cannot
    /// be scrolled or read, the row just added is the one the reader is watching, and a
    /// bound that emptied the transcript to satisfy itself would be the bound breaking
    /// the thing it exists to protect.
    fn trim(&mut self) {
        let over = self.items.len().saturating_sub(self.bounds.items);
        if over > 0 {
            self.items.drain(..over);
            self.items_dropped += over as u64;
        }
        let bytes_of = |r: &SnapshotItem| r.item.as_ref().map(|i| i.bytes()).unwrap_or(0);
        let mut total: usize = self.items.iter().map(bytes_of).sum();
        let mut drop = 0usize;
        while total > self.bounds.item_bytes && self.items.len() - drop > 1 {
            total -= bytes_of(&self.items[drop]);
            drop += 1;
        }
        if drop > 0 {
            self.items.drain(..drop);
            self.items_dropped += drop as u64;
        }
    }

    /// Cut a snapshot true as of `seq`.
    pub fn snapshot(&self, seq: u64, dropped: u64) -> Snapshot {
        Snapshot {
            session_id: self.session_id.clone(),
            seq,
            dropped,
            items_dropped: self.items_dropped,
            items: self.items.clone(),
            turn: self.turn.clone(),
            open_decisions: self.open.clone(),
            settled_decisions: self.settled.clone(),
            warnings: self.warnings.clone(),
            heads: self.heads.clone(),
        }
    }

    /// The turn in view, if there is one. For a session list, which wants to know
    /// whether this session is busy without cutting a whole snapshot to find out.
    pub fn turn(&self) -> Option<&TurnView> {
        self.turn.as_ref()
    }

    /// Transcript rows the view is holding. Bounded by [`ViewBounds::items`], so
    /// this is not the length of the conversation — `items_dropped` is the other
    /// half and is on the snapshot.
    pub fn item_count(&self) -> usize {
        self.items.len()
    }

    pub fn open_decisions(&self) -> &[OpenDecision] {
        &self.open
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::{LogBounds, SessionLog};
    use crate::testing::*;

    fn fold(log: &SessionLog) -> SessionView {
        let mut v = SessionView::new("s", ViewBounds::default());
        for e in log.retained() {
            v.apply(e);
        }
        v
    }

    /// **The byte bound, because a count bound is not a size bound.**
    ///
    /// Rows are not the same size, so 2,000 of them can be a few megabytes or most of a
    /// gigabyte — and the snapshot is cloned and sent for every attaching head.
    #[test]
    fn a_view_drops_the_oldest_rows_to_stay_inside_its_byte_bound() {
        let bounds = ViewBounds {
            items: 1_000,
            item_bytes: 4_000,
            ..ViewBounds::default()
        };
        let mut v = SessionView::new("s", bounds);
        // Twenty 1 KB rows: over the byte bound, well under the count bound.
        for i in 0..20 {
            let id = format!("i{i}");
            // Announce, then fill — the real order, and it is what `trim` has to cope
            // with: the count is checked at the announcement and the bytes at the fill.
            v.items.push(SnapshotItem {
                item_id: id.clone(),
                kind: "assistant".into(),
                ledger_head: String::new(),
                ts: 0,
                item: None,
            });
            v.record_item(
                &id,
                TranscriptItem::Assistant {
                    text: "x".repeat(1_000),
                    tool_calls: Vec::new(),
                    truncated: false,
                },
            );
        }
        let bytes: usize = v.items.iter().map(|r| r.item.as_ref().map(|i| i.bytes()).unwrap_or(0)).sum();
        assert!(
            bytes <= 4_000,
            "the view held {bytes} bytes against a 4,000 bound"
        );
        assert!(
            v.items.len() < 20,
            "nothing was dropped, so the bound did nothing"
        );
        // And the drop is counted, which is what a head discloses.
        assert!(v.items_dropped > 0, "the dropped rows were not counted");
    }

    /// A row bigger than the whole bound is **kept**, because a view with nothing in it
    /// cannot be read or scrolled — and a bound that emptied the transcript would break
    /// the thing it exists to protect.
    #[test]
    fn the_newest_row_is_kept_however_big_it_is() {
        let bounds = ViewBounds {
            items: 1_000,
            item_bytes: 100,
            ..ViewBounds::default()
        };
        let mut v = SessionView::new("s", bounds);
        v.items.push(SnapshotItem {
            item_id: "big".into(),
            kind: "tool_result".into(),
            ledger_head: String::new(),
            ts: 0,
            item: None,
        });
        v.record_item(
            "big",
            TranscriptItem::ToolResult {
                call_id: "c".into(),
                name: "read".into(),
                outcome: ToolOutcome::Ok,
                payload: "y".repeat(10_000),
                edit: None,
            },
        );
        assert_eq!(v.items.len(), 1, "the only row was dropped to satisfy the bound");
        assert!(v.items[0].item.is_some(), "and its body was kept");
    }

    /// **A body landing can blow the bound on its own.** A row is announced before it is
    /// filled, so a row that arrives empty and lands hundreds of kilobytes later is over
    /// the bound the moment it lands — and trimming only when a new row is *announced*
    /// would leave the snapshot oversized until the next one arrived.
    #[test]
    fn a_body_landing_trims_without_waiting_for_another_row() {
        let bounds = ViewBounds {
            items: 1_000,
            item_bytes: 2_000,
            ..ViewBounds::default()
        };
        let mut v = SessionView::new("s", bounds);
        for i in 0..3 {
            v.items.push(SnapshotItem {
                item_id: format!("i{i}"),
                kind: "assistant".into(),
                ledger_head: String::new(),
                ts: 0,
                item: None,
            });
        }
        // The bodies land, and the last one is large.
        for (i, n) in [(0, 500), (1, 500), (2, 5_000)] {
            v.record_item(
                &format!("i{i}"),
                TranscriptItem::Assistant {
                    text: "z".repeat(n),
                    tool_calls: Vec::new(),
                    truncated: false,
                },
            );
        }
        let bytes: usize = v.items.iter().map(|r| r.item.as_ref().map(|i| i.bytes()).unwrap_or(0)).sum();
        assert!(
            bytes <= 2_000 + 5_000,
            "a landing body did not trim: {bytes} bytes held"
        );
        assert!(v.items_dropped > 0, "the drop was not counted");
    }

    #[test]
    fn a_late_head_gets_the_text_once_not_the_deltas() {
        let mut log = SessionLog::new("s", LogBounds::default());
        log.append(turn_started("t1"));
        for w in ["Hel", "lo ", "wor", "ld"] {
            log.append(delta("t1", w));
        }
        let snap = fold(&log).snapshot(log.head_seq(), log.dropped());
        assert_eq!(snap.turn.unwrap().text, "Hello world");
    }

    #[test]
    fn progress_is_state_and_is_cleared_when_the_turn_ends() {
        let mut log = SessionLog::new("s", LogBounds::default());
        log.append(turn_started("t1"));
        log.append(progress("t1"));
        assert!(fold(&log).snapshot(1, 0).turn.unwrap().progress.is_some());
        log.append(turn_finished("t1"));
        assert!(
            fold(&log).snapshot(3, 0).turn.unwrap().progress.is_none(),
            "a finished turn has no prefill in flight, so there is nothing true to show"
        );
    }

    #[test]
    fn the_generation_counter_is_state_and_the_snapshot_carries_it() {
        // A head that joins mid-turn has missed every frame so far; the snapshot
        // is where the counter it missed comes from.
        let mut log = SessionLog::new("s", LogBounds::default());
        log.append(turn_started("t1"));
        log.append(tokens_generated("t1", 10));
        log.append(tokens_generated("t1", 25));
        assert_eq!(
            fold(&log).snapshot(2, 0).turn.unwrap().tokens,
            25,
            "the latest count wins"
        );
        // A count for another turn does not move it.
        log.append(tokens_generated("t2", 3));
        assert_eq!(fold(&log).snapshot(3, 0).turn.unwrap().tokens, 25);
        // Unlike `progress`, the count stays true when the turn ends: its final
        // value is the fact `TurnFinished`'s usage states.
        log.append(turn_finished("t1"));
        assert_eq!(fold(&log).snapshot(4, 0).turn.unwrap().tokens, 25);
    }

    #[test]
    fn a_running_tool_renders_as_running_with_no_partial_output() {
        let mut log = SessionLog::new("s", LogBounds::default());
        log.append(turn_started("t1"));
        log.append(proposed("t1", "c1", "read"));
        log.append(SessionEvent::ToolStarted {
            turn_id: "t1".into(),
            call_id: "c1".into(),
            name: "read".into(),
            access: "read".into(),
        });
        log.append(tool_progress("c1", "…400 lines so far…"));
        let snap = fold(&log).snapshot(4, 0);
        let call = &snap.turn.unwrap().calls[0];
        assert_eq!(call.state, CallState::Running);
        // There is nowhere in `CallState::Running` to put the partial output. That
        // is the design: it cannot leak because it has no home.
        let json = serde_json::to_string(&snap.open_decisions).unwrap();
        assert!(!json.contains("400 lines"));
    }

    #[test]
    fn a_settled_decision_is_not_in_open_decisions_by_construction() {
        let mut log = SessionLog::new("s", LogBounds::default());
        log.append(requested("r1", "rm -rf"));
        assert_eq!(fold(&log).open_decisions().len(), 1);
        log.append(answered("r1", "deny"));
        let v = fold(&log);
        assert!(v.open_decisions().is_empty());
        let snap = v.snapshot(2, 0);
        assert_eq!(snap.settled_decisions.len(), 1);
        assert_eq!(snap.settled_decisions[0].summary, "rm -rf");
    }

    #[test]
    fn dropped_is_present_and_zero_in_the_wire_form() {
        let v = SessionView::new("s", ViewBounds::default());
        let json = serde_json::to_string(&v.snapshot(0, 0)).unwrap();
        assert!(json.contains(r#""dropped":0"#), "{json}");
        assert!(json.contains(r#""items_dropped":0"#), "{json}");
    }

    #[test]
    fn transcript_content_arrives_out_of_band_and_absence_is_honest() {
        let mut log = SessionLog::new("s", LogBounds::default());
        log.append(SessionEvent::TranscriptAppended {
            item_id: "s.0".into(),
            kind: "user".into(),
            ledger_head: "ab".into(),
        });
        let mut v = fold(&log);
        assert_eq!(v.snapshot(1, 0).items[0].item, None);
        v.record_item(
            "s.0",
            TranscriptItem::User {
                parts: vec![letibot_transcript::UserPart::Text { text: "hi".into() }],
            },
        );
        assert!(v.snapshot(1, 0).items[0].item.is_some());
    }

    #[test]
    fn a_body_that_arrives_as_an_event_fills_the_row_it_names() {
        let mut log = SessionLog::new("s", LogBounds::default());
        log.append(appended("s.0", "user"));
        log.append(content("s.0", "hi"));
        let v = fold(&log);
        assert_eq!(
            v.snapshot(2, 0).items[0].item,
            Some(TranscriptItem::User {
                parts: vec![letibot_transcript::UserPart::Text { text: "hi".into() }],
            })
        );
    }

    #[test]
    fn the_turn_records_which_rows_it_appended() {
        // Without this a head cannot tell "the answer, live" from "the answer, as
        // history" and renders both.
        let mut log = SessionLog::new("s", LogBounds::default());
        log.append(turn_started("t1"));
        log.append(appended("t1.0", "reasoning"));
        log.append(appended("t1.1", "assistant"));
        log.append(turn_finished("t1"));
        let turn = fold(&log).snapshot(4, 0).turn.unwrap();
        assert_eq!(turn.appended, vec!["t1.0".to_string(), "t1.1".to_string()]);
    }
}
