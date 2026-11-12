//! The materialized view, and the snapshot cut from it. `since_seq = 0` gets a snapshot as of
//! seq S, then the events after it. The view holds what a head draws from, not what the log
//! already keeps.

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
    /// `Envelope::ts` of the `TranscriptAppended` that announced this row: when it happened, on
    /// the log's own clock. Zero means unknown - a measurement that was never taken, which a
    /// head must not render as 01:00:00.
    #[serde(default)]
    pub ts: u64,
    /// The content, when the daemon supplied it. `None` is honest: it means the
    /// event was seen and the item was not reconciled, which a head should render
    /// as a placeholder rather than as an empty message.
    pub item: Option<TranscriptItem>,
    /// The lines this reply cited, as the daemon read them when it landed
    /// ([`crate::SessionEvent::Citations`]) - empty for every row that cited nothing, and for a
    /// snapshot from a daemon built before citations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub citations: Vec<crate::event::Citation>,
}

/// A decision that is still owed an answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenDecision {
    pub req_id: String,
    pub kind: String,
    pub call_id: Option<String>,
    /// A subagent's call, when this card is not this session's own. Held on the view rather than
    /// re-derived, because the daemon is the one that knows which child made it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent: Option<crate::event::SubagentAsk>,
    /// The tool's declared access - `read | write | exec | network` - so a head can say WHY the
    /// card is asking when it is the declaration that asks. Empty from a daemon older than the
    /// field, and empty draws no clause.
    #[serde(default)]
    pub access: String,
    pub summary: String,
    /// The one thing being decided about - the command, the path, the URL - so a head can put it
    /// on its own line. See the event for why it is not left inside `summary`.
    #[serde(default)]
    pub target: String,
    /// The paths this action would write, verbatim from the event that carried them: held on the
    /// view rather than re-derived, so a card cannot name a different file than the gate did.
    pub write_targets: Vec<crate::event::WriteTarget>,
    /// Layer A's reading, for the dim line under the question.
    #[serde(default)]
    pub detail: String,
    /// The allow/deny ladder, for a permission. Empty for a question.
    pub options: Vec<DecisionOption>,
    /// Why a rung a person would look for is not on the ladder, in one line from the gate. Empty
    /// when nothing was left out.
    #[serde(default)]
    pub options_note: String,
    /// The model's own plain-text choices, for a question. Empty for a permission. See
    /// [`crate::event::SessionEvent::DecisionRequested`] for why the two do not share a field.
    #[serde(default)]
    pub choices: Vec<String>,
    /// Why the model is stuck, for a question.
    #[serde(default)]
    pub because: String,
    /// The model's verdict on this permission, when supervision is on
    /// (`--supervise` or `/supervise`). `None` everywhere else. Carried into the view so a head
    /// attaching mid-question renders the same thing a head that was there from the start does.
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
    /// Which vocabulary settled it: `permission`, `question`, or empty for a log recorded before
    /// the field existed.
    #[serde(default)]
    pub kind: String,
    /// The tool call this decision was about, when it was a permission. `None` for a question,
    /// and for a log recorded before the field existed - in which case a head renders the outcome
    /// as a standalone note rather than on the call's card.
    #[serde(default)]
    pub call_id: Option<String>,
    pub summary: String,
    pub outcome: DecisionOutcome,
    pub by: Decider,
    /// Why the DECIDER decided. For an operator answer this is `dead chose \`allow_once\` at the
    /// head`; for a boundary refusal it is the rule and its evidence. It is not the oracle's
    /// reasoning, even when an oracle advised - see [`SettledDecision::advice`], which is.
    pub basis: String,
    /// What the guard model said, when one was consulted: carried off the open decision at settle
    /// time, because the card must show it after the decision is no longer open.
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
    /// The display target from `ToolCallProposed`. Empty when the call was first seen as
    /// `ToolStarted`, or when it came from a log recorded before the field existed - in which
    /// case a head renders the verb alone rather than guessing.
    #[serde(default)]
    pub target: String,
    pub state: CallState,
    /// The log's `ts` of `ToolStarted`, so a head that attaches mid-call can say how long it
    /// has run. 0 while proposed, and in a view folded by a daemon older than the field.
    #[serde(default)]
    pub started_ts: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)] // Finished carries the outcome by value once per call; the others are unit
pub enum CallState {
    Proposed,
    /// Running, with no partial output. See the module note.
    Running,
    Finished {
        outcome: ToolOutcome,
        payload_digest: String,
        /// What the model received. Under the spill policy this is smaller than `full_bytes`, and
        /// a head that shows only one of the two is showing the wrong one half the time.
        inline_bytes: u64,
        full_bytes: u64,
        /// The spill locator, when the output spilled: a head can offer the rest.
        spill: Option<String>,
        /// Both sides of every file a file-editing call changed, bounded to the region that
        /// differs - the raw material of the two-panel diff. `#[serde(default)]` is what lets a
        /// snapshot recorded before the field still load, and `alias` what reads a snapshot that
        /// recorded the single excerpt as a one-item list.
        #[serde(
            default,
            alias = "edit",
            deserialize_with = "letibot_transcript::one_or_many_edits"
        )]
        edits: Vec<crate::event::ToolEdit>,
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
        /// Whether this round ended the prompt. Absent in a snapshot older than the field: an ending.
        #[serde(default = "crate::event::an_ending")]
        ends_turn: bool,
    },
    Interrupted {
        reason: String,
        partial_kept: bool,
    },
    /// The turn failed. A *terminal* state, which is the whole point of it existing: without
    /// one, `Running` was the last thing a head ever heard about a turn that died.
    Failed {
        error: String,
        partial_kept: bool,
    },
}

impl TurnState {
    /// Whether the turn is still going. One function, because "is it running" is asked in the
    /// head, in the registry's session list and in the snapshot, and a new terminal state that
    /// one of the three forgets is a spinner nobody can stop.
    pub fn is_running(&self) -> bool {
        matches!(self, TurnState::Running)
    }
}

/// The turn a late head is joining, with its text accumulated once: the head does not replay N
/// deltas to arrive at the same paragraph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnView {
    pub turn_id: String,
    pub model: String,
    pub ledger_head: String,
    pub text: String,
    pub reasoning: String,
    /// The raw `<function=` markup of every tool call this turn has written so far, concatenated:
    /// what a head renders before the call's card replaces it.
    #[serde(default)]
    pub raw_calls: String,
    pub calls: Vec<CallView>,
    /// The transcript rows this turn appended, in order: a head shows a running turn from the
    /// pane and a finished one from these rows.
    #[serde(default)]
    pub appended: Vec<String>,
    /// Live prefill state. `None` once the turn has ended - a progress frame is true only while
    /// it is happening.
    pub progress: Option<PromptProgress>,
    /// The server's generation counter, as the last `TokensGenerated` reported it: kept when the
    /// turn ends so a late head knows the counter is not stuck.
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
    /// The snapshot is true as of this seq. The next event a head receives is `seq + 1`, with no
    /// gap - see [`crate::hub::Hub::attach`].
    pub seq: u64,
    /// Events that fell off the back of the scrollback and this head will never see. Present and
    /// zero, never omitted.
    pub dropped: u64,
    /// Transcript rows trimmed out of this snapshot. Present and zero, same rule.
    pub items_dropped: u64,
    /// How many of `items` are above the base and numbered by nothing. A resume republishes the
    /// ancestors of a compacted transcript in the same list; ordinal 0 is the first row past this
    /// count. Defaulted, so an older head decodes the frame.
    #[serde(default)]
    pub items_lead: usize,
    pub items: Vec<SnapshotItem>,
    pub turn: Option<TurnView>,
    /// Only decisions genuinely still open. A settled one cannot appear here.
    pub open_decisions: Vec<OpenDecision>,
    /// Recent outcomes, so a head can render "denied, by policy" rather than silence where a
    /// prompt was.
    pub settled_decisions: Vec<SettledDecision>,
    pub warnings: Vec<Warned>,
    pub heads: Vec<HeadPresence>,
    /// The session's children, as its own `Subagent` events reported them: the parent's fact, held
    /// by the parent's view.
    #[serde(default)]
    pub subagents: Vec<SubagentView>,
    /// What this session is waiting on, as the daemon last published it: the value a head that
    /// attaches late needs and cannot derive.
    #[serde(default)]
    pub activity: crate::activity::Activity,
}

/// One child of this session, as the `Subagent` event last reported it. Every field is the
/// event's own, so a head draws what the daemon decided rather than a guess.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubagentView {
    /// The child's own session id - the key, and the same one the list's brief carries.
    pub session_id: String,
    pub state: String,
    pub prompt: String,
    pub role: String,
    #[serde(default)]
    pub task: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub answer: Option<String>,
    /// The child's name, from the event's `slug` - empty from a daemon that predates it.
    #[serde(default)]
    pub slug: String,
    /// `Envelope::ts` of the event that last said this.
    pub ts: u64,
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
    /// How many bytes of row text the view will hold before it drops the oldest: `items` alone
    /// bounds a count, and one enormous row would still be unbounded.
    pub item_bytes: usize,
    pub settled_decisions: usize,
    pub warnings: usize,
}

impl Default for ViewBounds {
    fn default() -> Self {
        ViewBounds {
            items: 2_000,
            // An order of magnitude above any session this box has today and far below what a
            // pathological one reaches, so a normal session is untouched and a bad one is
            // bounded.
            item_bytes: 8 * 1024 * 1024,
            // A decision is a fact about the tool call it gated, and a head renders it on that call's
            // card, including a card the head never saw while the call was running.
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
    /// The transcript the session is speaking: the one every ordinal names. Empty until a fork says
    /// which it is, when the rows a resume republished above the base cannot be told apart.
    transcript: String,
    turn: Option<TurnView>,
    open: Vec<OpenDecision>,
    settled: Vec<SettledDecision>,
    warnings: Vec<Warned>,
    heads: Vec<HeadPresence>,
    /// This session's children, as the `Subagent` events reported them - see the
    /// `SessionEvent::Subagent` arm for why a snapshot carries them.
    subagents: Vec<SubagentView>,
    /// See [`Snapshot::activity`]. Folded rather than asked for: the daemon publishes the value when
    /// it changes, so a view holding the last one has the current one at every seq.
    activity: crate::activity::Activity,
}

/// How much one session's view holds, by part - for the daemon's memory census. Counts, not
/// bytes: which part grows is the question a census answers, and a count is exact where a byte
/// estimate of a nested struct would be a guess.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ViewCensus {
    pub items: usize,
    pub open: usize,
    pub settled: usize,
    pub warnings: usize,
    pub subagents: usize,
}

impl SessionView {
    /// See [`ViewCensus`].
    pub fn census(&self) -> ViewCensus {
        ViewCensus {
            items: self.items.len(),
            open: self.open.len(),
            settled: self.settled.len(),
            warnings: self.warnings.len(),
            subagents: self.subagents.len(),
        }
    }

    // (the free function `body_of` at the foot of this file is the definition both
    // readers use - see its own doc)
    pub fn new(session_id: impl Into<String>, bounds: ViewBounds) -> Self {
        SessionView {
            session_id: session_id.into(),
            bounds,
            items: Vec::new(),
            items_dropped: 0,
            transcript: String::new(),
            turn: None,
            open: Vec::new(),
            settled: Vec::new(),
            warnings: Vec::new(),
            heads: Vec::new(),
            subagents: Vec::new(),
            activity: crate::activity::Activity::Quiet,
        }
    }

    /// Fold one appended event into the view. The exhaustive match is the scrub's other half: a
    /// new event variant must decide here what a snapshot carries.
    pub fn apply(&mut self, env: &Envelope) {
        match &env.event {
            SessionEvent::TurnStarted {
                turn_id,
                model,
                ledger_head,
                // the view does not keep it: a head's clock is the head's, and this is the
                // emitter's stamp for the wire
                began_ms: _,
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
                // State, not history: the latest count wins. The counter is monotonic in the
                // stream, and `max` keeps a reordered or duplicated frame from moving it
                // backwards.
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
                        started_ts: 0,
                    });
                }
            }
            SessionEvent::DecisionRequested {
                req_id,
                write_targets,
                kind,
                call_id,
                access,
                summary,
                target,
                detail,
                options,
                options_note,
                choices,
                because,
                advice,
                deadline,
                on_timeout,
                subagent,
                ..
            } => {
                self.open.retain(|d| &d.req_id != req_id);
                self.open.push(OpenDecision {
                    write_targets: write_targets.clone(),
                    req_id: req_id.clone(),
                    kind: kind.clone(),
                    call_id: call_id.clone(),
                    subagent: subagent.clone(),
                    access: access.clone(),
                    summary: summary.clone(),
                    target: target.clone(),
                    detail: detail.clone(),
                    options: options.clone(),
                    options_note: options_note.clone(),
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
                // The removal is the scrub: no path leaves a settled decision open, so no snapshot can
                // carry one.
                let (summary, call_id, kind, advice) = self
                    .open
                    .iter()
                    .find(|d| &d.req_id == req_id)
                    .map(|d| {
                        (
                            d.summary.clone(),
                            d.call_id.clone(),
                            d.kind.clone(),
                            d.advice.clone(),
                        )
                    })
                    .unwrap_or_default();
                self.open.retain(|d| &d.req_id != req_id);
                self.settled.push(SettledDecision {
                    req_id: req_id.clone(),
                    kind,
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
                    c.started_ts = env.ts;
                } else if let Some(t) = self.turn.as_mut() {
                    t.calls.push(CallView {
                        call_id: call_id.clone(),
                        name: name.clone(),
                        args_digest: String::new(),
                        // No proposal was seen, so there is no target and none is
                        // invented: the verb alone is what is true here.
                        target: String::new(),
                        state: CallState::Running,
                        started_ts: env.ts,
                    });
                }
            }
            // Deliberately nothing. Partial tool output has no durable form; see the module
            // note. This arm exists so that "we forgot" and "we decided" do not look the same.
            SessionEvent::ToolProgress { .. } => {}
            SessionEvent::ToolFinished {
                call_id,
                outcome,
                payload_digest,
                inline_bytes,
                full_bytes,
                spill,
                edits,
                ..
            } => {
                if let Some(c) = self.call_mut(call_id) {
                    c.state = CallState::Finished {
                        outcome: outcome.clone(),
                        payload_digest: payload_digest.clone(),
                        inline_bytes: *inline_bytes,
                        full_bytes: *full_bytes,
                        spill: spill.clone(),
                        edits: edits.clone(),
                    };
                }
            }
            SessionEvent::TurnFinished {
                turn_id,
                finish_reason,
                usage,
                timings,
                ends_turn,
                // Also not read here: the check and the dialect are a turn's record, and this
                // view's finished state is what a head draws about the turn that just stopped.
                prefix_check: _,
                dialect_template_sha: _,
            } => {
                if let Some(t) = self.turn.as_mut()
                    && &t.turn_id == turn_id
                {
                    t.progress = None;
                    t.state = TurnState::Finished {
                        finish_reason: finish_reason.clone(),
                        usage: *usage,
                        timings: *timings,
                        ends_turn: *ends_turn,
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
            // Not matched on `turn_id` alone: a turn can fail before it published a `TurnStarted` (a
            // render or a tokenize can refuse first).
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
                item,
                ..
            } => {
                self.items.push(SnapshotItem {
                    citations: Vec::new(),
                    item_id: item_id.clone(),
                    kind: kind.clone(),
                    ledger_head: ledger_head.clone(),
                    ts: env.ts,
                    // A row that rode its own announcement is whole the moment it is published,
                    // which is what makes a log of these events readable as a conversation.
                    item: item.as_deref().cloned(),
                });
                self.trim();
                // Which rows this turn produced. A head cannot work this out from the snapshot -
                // the rows and the turn are separate lists - and it is the fact that decides
                // whether the live pane is still the only copy of the answer.
                if let Some(t) = self.turn.as_mut() {
                    t.appended.push(item_id.clone());
                }
            }
            // The body for a row the log already announced. Idempotent, and a no-op for an id the
            // view has trimmed or never saw - the same contract `record_item` has, because this is
            // that call, arriving over the wire.
            SessionEvent::TranscriptContent { item_id, item } => {
                if let Some(row) = self.items.iter_mut().find(|r| &r.item_id == item_id) {
                    row.item = Some((**item).clone());
                }
            }
            // What the row's reply cited, beside its content - the same contract.
            SessionEvent::Citations {
                item_id,
                citations,
            } => {
                if let Some(row) = self.items.iter_mut().find(|r| &r.item_id == item_id) {
                    row.citations = citations.clone();
                }
            }
            // A fork numbers the conversation from zero. Rows of the replaced transcript, and rows a
            // resume republished above the base, have no ordinal, so the view says which transcript it
            // speaks and `items_dropped` is where the base's own window starts.
            SessionEvent::TranscriptForked {
                transcript_id,
                parent_id,
            } => {
                let gone = format!("{parent_id}.");
                self.items.retain(|r| !r.item_id.starts_with(gone.as_str()));
                self.transcript = transcript_id.clone();
                let here = format!("{transcript_id}.");
                self.items_dropped = self
                    .items
                    .iter()
                    .find(|r| r.item_id.starts_with(here.as_str()))
                    .and_then(|r| ordinal_in(&r.item_id, &here))
                    .unwrap_or(0) as u64;
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
            SessionEvent::Warning { code, detail, .. } => {
                self.warnings.push(Warned {
                    code: code.clone(),
                    detail: detail.clone(),
                    ts: env.ts,
                });
                let over = self.warnings.len().saturating_sub(self.bounds.warnings);
                self.warnings.drain(..over);
            }
            // A password request is live for two minutes and answered on the connection that
            // asked; a head attaching later has nothing to do with it, and a settled one is
            // history.
            SessionEvent::SecretRequested { .. }
            | SessionEvent::SecretSettled { .. }
            // And the same for the operator's own run's card: it is live only while the run is
            // blocked, and the head that attached later has not seen the output the question is
            // about. The settlement is not carried either - the run's own rows are.
            | SessionEvent::PromptRequested { .. }
            | SessionEvent::PromptSettled { .. }
            // The typed twin of a warning a person reads, and a snapshot that kept both would
            // draw one failure twice: the turn's own end is the durable copy.
            | SessionEvent::ProviderTrouble { .. }
            | SessionEvent::ScreenRequested { .. } => {}
            // The plan is a full document: it belongs in the log, where a head can ask for it by
            // seq, and carrying every one of them in every snapshot would make the snapshot grow
            // with the session.
            SessionEvent::Explain { .. } => {}
            // Who did what is history, and a head that was there saw it. The *effect* is in the
            // transcript rows and the turn, which the snapshot already carries.
            SessionEvent::CommandIssued { .. } => {}
            // The name travels in `SessionBrief`, in `Hello` and `Sessions`, so a head that attaches
            // after a rename is told by its own snapshot.
            SessionEvent::SessionRenamed { .. } => {}
            // The todo list is not turn state and decides nothing: it reaches the head as the
            // event itself, and the pane keeps the latest one it saw.
            SessionEvent::TodosUpdated { .. } => {}
            // Not folded into the snapshot, and the reason is bounded rather than absent: a denial is
            // durable on the log, so a late head reads it from the replay.
            SessionEvent::DenialRaised { .. } => {}
            // A subagent spawn or finish, FOLDED: rebuilding the tree from replayed events alone is
            // false for a session whose children predate the log.
            SessionEvent::Subagent {
                subagent_id,
                state,
                prompt,
                role,
                task,
                model,
                answer,
                slug,
            } => {
                let row = SubagentView {
                    session_id: subagent_id.clone(),
                    state: state.clone(),
                    prompt: prompt.clone(),
                    role: role.clone(),
                    task: task.clone(),
                    model: model.clone(),
                    answer: answer.clone(),
                    slug: slug.clone(),
                    // When this happened, on the log's own clock - the head has its own `now_ms`
                    // and this is not it, the same rule `SnapshotItem::ts` keeps.
                    ts: env.ts,
                };
                match self
                    .subagents
                    .iter_mut()
                    .find(|s| s.session_id == *subagent_id)
                {
                    Some(known) => *known = row,
                    None => self.subagents.push(row),
                }
            }
            // Same shape: a job's start is the `bash` call's own `Backgrounded` finish, which the
            // call view already folds, and its end is carried by the durable `JobSettled` event.
            // The jobs pane folds both itself.
            SessionEvent::JobSettled { .. } => {}
            // Not carried, and the reason is a real gap: a head that attaches after the admission
            // does not learn of it from the snapshot.
            SessionEvent::OperatorCallAllowed { .. } => {}
            // Nothing to fold: a job-output window is drawn by the pane that asked for it, and
            // the view carries no window state. Ephemeral besides, so a late head never replays
            // one.
            SessionEvent::JobOutput { .. } => {}
            // Nothing to fold: a fill's progress is drawn by the head from the event itself and
            // the view carries no counter. Ephemeral besides, so a late head never replays a tick.
            SessionEvent::Filling { .. } => {}
            // Live-only, as `Filling` is: a snapshot is what is TRUE at a seq, and a fold's
            // progress is a line that walks. What a late head needs from a compaction it missed is
            // the fork and the `compacted` warning, which the view already keeps.
            SessionEvent::CompactionProgress { .. } => {}
            // Live-only, for `CompactionProgress`'s reason: a count stored here would reach a late
            // head as a fact about now. A head that attaches mid-turn draws nothing until the held
            // set next changes.
            SessionEvent::HeldResults { .. } => {}
            // The one live value this view does keep, and the exception is the point: what a session
            // is waiting on is a fact only the daemon computes.
            SessionEvent::ActivityChanged { activity } => self.activity = activity.clone(),
            // Not folded: a retry series is a row a head draws from the event itself, and the
            // snapshot is what is true at a seq - a stored count would reach a late head as a
            // count it has already passed.
            SessionEvent::ModelRetry { .. } => {}
            // Nothing to fold, and the snapshot must not carry one: a pane's view is what its last
            // look answered, and a stored one would be a stale drawing.
            SessionEvent::PluginPane { .. } => {}
            // Nothing to fold: which panes a session has is the head's copy of the events, and
            // this view is per-session state the daemon owns (an attach re-asks).
            SessionEvent::PluginPaneGone { .. } => {}
            // Not carried, and the reason is the snapshot's: the queues are daemon-level and the
            // view is per-session. A head that wants them asks with `ListMergeQueue` and gets the
            // whole snapshot as of now; a move only tells it to ask again.
            SessionEvent::QueueMoved { .. } => {}
        }
    }

    fn call_mut(&mut self, call_id: &str) -> Option<&mut CallView> {
        self.turn
            .as_mut()?
            .calls
            .iter_mut()
            .find(|c| c.call_id == call_id)
    }

    /// How many of the rows this view holds are above the base. A resume republishes the tail of
    /// the transcripts a compaction put behind the current one, and those rows are not ordinals of
    /// it: ordinal 0 is the base's first row, which is the first one past this count.
    fn lead(&self) -> usize {
        if self.transcript.is_empty() {
            return 0;
        }
        let here = format!("{}.", self.transcript);
        self.items
            .iter()
            .take_while(|r| !r.item_id.starts_with(here.as_str()))
            .count()
    }

    /// One row's body, whole, by the row's position in the session. `row` is the session ordinal,
    /// where 0 is the first row of the conversation.
    pub fn row_body_at(&self, row: usize) -> Option<String> {
        let idx = self.slot(row)?;
        let item = self.items.get(idx)?.item.as_ref()?;
        Some(body_of(item))
    }

    /// One whole row by the same ordinal: a head scrolling above its window needs the block and
    /// its kind, not only the text, or it could draw the words as the wrong thing.
    pub fn row_at(&self, row: usize) -> Option<SnapshotItem> {
        self.items.get(self.slot(row)?).cloned()
    }

    /// Position in `items` of a session ordinal. The ordinal comes off the wire, so the sum is checked.
    fn slot(&self, row: usize) -> Option<usize> {
        row.checked_sub(self.items_dropped as usize)?
            .checked_add(self.lead())
    }

    /// Whether this view already holds a row's body, by the row's id: an announcement that carried
    /// it filled the row, so the body has no second journey to make. See `Hub::record_item`.
    pub fn row_has_body(&self, item_id: &str) -> bool {
        self.items
            .iter()
            .find(|r| r.item_id == item_id)
            .is_some_and(|r| r.item.is_some())
    }

    /// Attach content to an already-appended transcript row: a row too large to ride its own
    /// announcement becomes readable here.
    pub fn record_item(&mut self, item_id: &str, item: TranscriptItem) {
        if let Some(row) = self.items.iter_mut().find(|r| r.item_id == item_id) {
            row.item = Some(item);
            // The body can blow the byte bound on its own: rows are announced before they are filled,
            // so a row that arrives whole is a row the bound has not seen.
            self.trim();
        }
    }

    /// Drop the oldest rows until the view is inside both bounds, counting what went: a method
    /// rather than inline, so the two bounds cannot be applied apart.
    fn trim(&mut self) {
        let over = self.items.len().saturating_sub(self.bounds.items);
        if over > 0 {
            self.drop_front(over);
        }
        let bytes_of = |r: &SnapshotItem| r.item.as_ref().map(|i| i.bytes()).unwrap_or(0);
        let mut total: usize = self.items.iter().map(bytes_of).sum();
        let mut drop = 0usize;
        while total > self.bounds.item_bytes && self.items.len() - drop > 1 {
            total -= bytes_of(&self.items[drop]);
            drop += 1;
        }
        if drop > 0 {
            self.drop_front(drop);
        }
    }

    /// Drop `n` rows off the front, counting the ones the base lost. A row above the base is
    /// history the conversation no longer numbers, so dropping it does not move `items_dropped`.
    fn drop_front(&mut self, n: usize) {
        let lead = self.lead();
        self.items.drain(..n);
        self.items_dropped += n.saturating_sub(lead) as u64;
    }

    /// Cut a snapshot true as of `seq`.
    pub fn snapshot(&self, seq: u64, dropped: u64) -> Snapshot {
        Snapshot {
            session_id: self.session_id.clone(),
            seq,
            dropped,
            items_dropped: self.items_dropped,
            // Read off the same `lead` the ordinals are: a head that guessed this was five rows
            // out on every page it asked for, and nothing announced the drift.
            items_lead: self.lead(),
            items: self.items.clone(),
            turn: self.turn.clone(),
            open_decisions: self.open.clone(),
            settled_decisions: self.settled.clone(),
            warnings: self.warnings.clone(),
            heads: self.heads.clone(),
            subagents: self.subagents.clone(),
            activity: self.activity.clone(),
        }
    }

    /// The turn in view, if there is one. For a session list, which wants to know whether this
    /// session is busy without cutting a whole snapshot to find out.
    pub fn turn(&self) -> Option<&TurnView> {
        self.turn.as_ref()
    }

    /// Transcript rows the view is holding. Bounded by [`ViewBounds::items`], so this is not the
    /// length of the conversation - `items_dropped` is the other half and is on the snapshot.
    pub fn item_count(&self) -> usize {
        self.items.len()
    }

    pub fn open_decisions(&self) -> &[OpenDecision] {
        &self.open
    }
}

/// The ordinal an item id names: a row of transcript `prefix` is `<prefix><n>`, `n` being its
/// position in that transcript - the store's `seq`, and what a page above a window is asked for by.
pub fn ordinal_in(item_id: &str, prefix: &str) -> Option<usize> {
    item_id.strip_prefix(prefix)?.parse().ok()
}

/// A row's body, as text: the one definition, for every reader. Two callers and they must agree,
/// because a head that fetched a row and one that replayed it draw the same card.
pub fn body_of(item: &TranscriptItem) -> String {
    match item {
        TranscriptItem::ToolResult { payload, .. } => payload.clone(),
        // The other variants' "body" is their text. A row a head can page is a tool result in
        // practice, but the accessor is not the place to decide that: a head asking for a long
        // answer's bytes gets them rather than a silence it would have to interpret.
        TranscriptItem::Assistant { text, .. }
        | TranscriptItem::Reasoning { text, .. }
        | TranscriptItem::System { text, .. }
        | TranscriptItem::Notice { text, .. } => text.clone(),
        TranscriptItem::User { parts, .. } => parts
            .iter()
            .filter_map(|p| match p {
                letibot_transcript::UserPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        TranscriptItem::Offer { body, .. } => body.clone(),
        // A segment mark renders to nothing, so it has no body to page.
        TranscriptItem::SegmentMark { .. } => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::{LogBounds, SessionLog};
    use crate::testing::*;

    /// The one definition of a row's `body`, per variant. Two tiers answer `FetchRow`: the
    /// bounded view, and the store behind it.
    #[test]
    fn a_rows_body_is_its_own_kind_of_text() {
        use letibot_transcript::{ReasoningField, ToolOutcome, UserPart};
        // A tool result's body is its payload - the case a head pages, since that is where a
        // 418 KB log lives.
        assert_eq!(
            body_of(&TranscriptItem::ToolResult {
                call_id: "c".into(),
                name: "bash".into(),
                outcome: ToolOutcome::Ok,
                payload: "the payload".into(),
                edits: Vec::new(),
                origin: None,
                media: None,
            }),
            "the payload"
        );
        // A prose row's body is its text, for all three kinds that have one.
        for item in [
            TranscriptItem::Assistant {
                text: "an answer".into(),
                tool_calls: vec![],
                truncated: false,
            },
            TranscriptItem::Reasoning {
                text: "thinking".into(),
                field: ReasoningField::Inline,
                truncated: false,
            },
            TranscriptItem::System {
                text: "bootstrap".into(),
                origin: letibot_transcript::SystemOrigin::Bootstrap,
            },
        ] {
            let want = match &item {
                TranscriptItem::Assistant { text, .. }
                | TranscriptItem::Reasoning { text, .. }
                | TranscriptItem::System { text, .. } => text.clone(),
                _ => unreachable!(),
            };
            assert_eq!(body_of(&item), want);
        }
        // A user row's body is its text parts joined - and the non-text parts contribute nothing,
        // because an image or a file ref *names* a thing rather than being one. Asserted with two
        // text parts, which is the shape this can construct without inventing other variants.
        assert_eq!(
            body_of(&TranscriptItem::User {
                speaker: Default::default(),
                parts: vec![
                    UserPart::Text {
                        text: "first".into(),
                    },
                    UserPart::Text {
                        text: "second".into(),
                    },
                ],
            }),
            "first\nsecond"
        );
        // A segment mark renders to nothing, so it has no body to page - that is the
        // `SegmentMark => String::new()` arm, and it is a *different* answer from "there is no
        // such row", which is the wire's `body: Some("")` against `body: None`.
    }

    fn fold(log: &SessionLog) -> SessionView {
        let mut v = SessionView::new("s", ViewBounds::default());
        for e in log.retained() {
            v.apply(e);
        }
        v
    }

    /// The byte bound, because a count bound is not a size bound: rows are not the same size, so
    /// 2,000 of them can be a few megabytes or most of a gigabyte, and the snapshot is cloned and
    /// sent for every attaching head.
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
            // Announce, then fill - the real order, and it is what `trim` has to cope with: the
            // count is checked at the announcement and the bytes at the fill.
            v.items.push(SnapshotItem {
                citations: Vec::new(),
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
        let bytes: usize = v
            .items
            .iter()
            .map(|r| r.item.as_ref().map(|i| i.bytes()).unwrap_or(0))
            .sum();
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

    /// A row bigger than the whole bound is kept, because a view with nothing in it cannot be read
    /// or scrolled - and a bound that emptied the transcript would break the thing it exists to
    /// protect.
    #[test]
    fn the_newest_row_is_kept_however_big_it_is() {
        let bounds = ViewBounds {
            items: 1_000,
            item_bytes: 100,
            ..ViewBounds::default()
        };
        let mut v = SessionView::new("s", bounds);
        v.items.push(SnapshotItem {
            citations: Vec::new(),
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
                edits: Vec::new(),
                origin: None,
                media: None,
            },
        );
        assert_eq!(
            v.items.len(),
            1,
            "the only row was dropped to satisfy the bound"
        );
        assert!(v.items[0].item.is_some(), "and its body was kept");
    }

    /// A body landing can blow the bound on its own: a row is announced before it is filled, so it
    /// arrives after the count has been checked.
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
                citations: Vec::new(),
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
        let bytes: usize = v
            .items
            .iter()
            .map(|r| r.item.as_ref().map(|i| i.bytes()).unwrap_or(0))
            .sum();
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

    /// A switch back is handed the children the parent's own view folded: neither half of the old
    /// reading holds, because the tree may have changed while the head was away.
    #[test]
    fn a_snapshot_carries_the_sessions_children_as_their_events_reported_them() {
        let mut log = SessionLog::new("s", LogBounds::default());
        log.append(turn_started("t1"));
        log.append(SessionEvent::Subagent {
            subagent_id: "s-sub-1".into(),
            state: "running".into(),
            prompt: "find the bug in the reader".into(),
            role: "coder".into(),
            task: "find the bug in the reader".into(),
            model: "local".into(),
            answer: None,
            slug: String::new(),
        });
        let snap = fold(&log).snapshot(2, 0);
        assert_eq!(snap.subagents.len(), 1, "the child is in the snapshot");
        let row = &snap.subagents[0];
        assert_eq!(row.session_id, "s-sub-1");
        assert_eq!(row.state, "running");
        assert_eq!(row.role, "coder");
        assert_eq!(row.task, "find the bug in the reader");
        assert_eq!(row.model, "local");
        assert_eq!(row.answer, None);

        // A finish REPLACES the row rather than adding a second one - the same rule the head's own
        // fold keeps, so a child that has ended is one row and not two.
        log.append(SessionEvent::Subagent {
            subagent_id: "s-sub-1".into(),
            state: "done".into(),
            prompt: "find the bug in the reader".into(),
            role: "coder".into(),
            task: "find the bug in the reader".into(),
            model: "local".into(),
            answer: Some("the reader drops the last byte".into()),
            slug: String::new(),
        });
        let snap = fold(&log).snapshot(3, 0);
        assert_eq!(snap.subagents.len(), 1, "one child, one row");
        assert_eq!(snap.subagents[0].state, "done");
        assert_eq!(
            snap.subagents[0].answer.as_deref(),
            Some("the reader drops the last byte")
        );

        // And a second child is a second row, so the field is a list and not a slot.
        log.append(SessionEvent::Subagent {
            subagent_id: "s-sub-2".into(),
            state: "opening".into(),
            prompt: "check the docs".into(),
            role: "coder".into(),
            task: "check the docs".into(),
            model: String::new(),
            answer: None,
            slug: String::new(),
        });
        let snap = fold(&log).snapshot(4, 0);
        let ids: Vec<&str> = snap
            .subagents
            .iter()
            .map(|s| s.session_id.as_str())
            .collect();
        assert_eq!(ids, vec!["s-sub-1", "s-sub-2"]);
    }

    /// An older daemon's snapshot still parses: the field is `#[serde(default)]`, so a head built
    /// from this tree reads a daemon that never sent it.
    #[test]
    fn a_snapshot_without_the_children_field_still_parses() {
        let snap = fold(&{
            let mut log = SessionLog::new("s", LogBounds::default());
            log.append(turn_started("t1"));
            log
        })
        .snapshot(1, 0);
        let mut json = serde_json::to_value(&snap).expect("a snapshot serialises");
        json.as_object_mut()
            .expect("a snapshot is an object")
            .remove("subagents");
        let back: Snapshot = serde_json::from_value(json).expect("an absent field is the default");
        assert!(
            back.subagents.is_empty(),
            "an absent list and an empty one are one statement"
        );
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
        // There is nowhere in `CallState::Running` to put the partial output. That is the design:
        // it cannot leak because it has no home.
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

    /// A fork replaces this view's rows, so a LATE head is not handed both conversations: this view
    /// is the daemon's, and it drops what the fork replaced.
    #[test]
    fn a_fork_replaces_the_rows_this_view_holds() {
        let mut log = SessionLog::new("s", LogBounds::default());
        for i in 0..40u64 {
            log.append(SessionEvent::TranscriptAppended {
                item_id: format!("s.{i}"),
                kind: "assistant".into(),
                ledger_head: "ab".into(),
                item: None,
                body_bytes: 0,
            });
        }
        assert_eq!(
            fold(&log).snapshot(40, 0).items.len(),
            40,
            "the fixture's premise: a 40-row conversation"
        );

        log.append(forked("s#t1", "s"));
        for i in 0..60u64 {
            log.append(SessionEvent::TranscriptAppended {
                item_id: format!("s#t1.{i}"),
                kind: "assistant".into(),
                ledger_head: "cd".into(),
                item: None,
                body_bytes: 0,
            });
        }

        let snap = fold(&log).snapshot(101, 0);
        // 60, and not 100. A fork replaces the conversation rather than adding to it.
        assert_eq!(
            snap.items.len(),
            60,
            "the fork's rows were appended to the ones they replace"
        );
        assert!(
            snap.items.iter().all(|r| r.item_id.starts_with("s#t1.")),
            "a row of the transcript that went is still in the snapshot: {:?}",
            snap.items
                .iter()
                .map(|r| &r.item_id)
                .take(3)
                .collect::<Vec<_>>()
        );
        // And the rows are REPLACED, not missing. `items_dropped` says a head is short of a
        // window and can ask for what it lost; these rows are gone from the session, and a head
        // told to count them would ask for rows that exist nowhere.
        assert_eq!(
            snap.items_dropped, 0,
            "a replacement is not a window: nothing here fell out of one"
        );
    }

    /// After a fork the view's ordinals are the new base's. The view kept rows of the earlier
    /// transcripts with the old `items_dropped`, so a page asked for from the head's window was
    /// answered from the transcript the fork replaced.
    #[test]
    fn a_fork_numbers_the_window_from_the_new_base() {
        let mut log = SessionLog::new("s", LogBounds::default());
        for i in 0..500u64 {
            log.append(SessionEvent::TranscriptAppended {
                item_id: format!("s.{i}"),
                kind: "assistant".into(),
                ledger_head: "ab".into(),
                item: None,
                body_bytes: 0,
            });
        }
        let bounds = ViewBounds {
            items: 40,
            ..ViewBounds::default()
        };
        let mut v = SessionView::new("s", bounds);
        for e in log.retained() {
            v.apply(e);
        }
        assert_eq!(v.items_dropped, 460, "the fixture did not trim its window");

        // A compaction: the new base, carried row by row under the new transcript's ids.
        log.append(forked("s#t1", "s"));
        for i in 0..100u64 {
            log.append(SessionEvent::TranscriptAppended {
                item_id: format!("s#t1.{i}"),
                kind: "assistant".into(),
                ledger_head: "cd".into(),
                item: None,
                body_bytes: 0,
            });
        }
        for e in log.retained() {
            v.apply(e);
        }
        assert_eq!(v.items.len(), 40, "the fixture's window is not 40 rows");
        assert_eq!(
            v.items_dropped, 60,
            "ordinal 0 does not name the first row of the new base: items_dropped is {}",
            v.items_dropped
        );
        assert_eq!(
            v.row_at(60).map(|r| r.item_id),
            Some("s#t1.60".to_string()),
            "the page above the window is a row of the transcript the fork replaced"
        );
    }

    /// A fork drops the rows of the transcript it replaced and no others. A resume republishes
    /// the tail of the transcripts a compaction put behind the current one, and the head draws those
    /// rows - they are not ordinals of the new base, and the base starts past them.
    #[test]
    fn a_fork_leaves_the_rows_of_the_transcripts_it_did_not_replace() {
        let mut log = SessionLog::new("s", LogBounds::default());
        // The chain a resume leaves behind, oldest transcript first.
        for i in 0..5u64 {
            log.append(SessionEvent::TranscriptAppended {
                item_id: format!("s#t1.{i}"),
                kind: "assistant".into(),
                ledger_head: "ab".into(),
                item: None,
                body_bytes: 0,
            });
        }
        for i in 0..40u64 {
            log.append(SessionEvent::TranscriptAppended {
                item_id: format!("s#t2.{i}"),
                kind: "assistant".into(),
                ledger_head: "cd".into(),
                item: None,
                body_bytes: 0,
            });
        }
        // The fork replaces the transcript the session is speaking, and not the one behind it.
        log.append(forked("s#t3", "s#t2"));
        for i in 0..20u64 {
            log.append(SessionEvent::TranscriptAppended {
                item_id: format!("s#t3.{i}"),
                kind: "assistant".into(),
                ledger_head: "ef".into(),
                item: None,
                body_bytes: 0,
            });
        }

        let snap = fold(&log).snapshot(100, 0);
        let ids: Vec<&str> = snap.items.iter().map(|r| r.item_id.as_str()).collect();
        assert_eq!(ids.len(), 25, "5 ancestors plus 20 carried: {ids:?}");
        assert!(
            !ids.iter().any(|id| id.starts_with("s#t2.")),
            "a row of the transcript the fork replaced is still held: {ids:?}"
        );
        assert_eq!(
            ids.iter().filter(|id| id.starts_with("s#t1.")).count(),
            5,
            "a fork took the rows of a transcript it did not replace: {ids:?}"
        );
        // And the ordinals are the new base's. They start past the rows above it, so ordinal 0
        // is the fork's first row and not an ancestor's.
        let v = fold(&log);
        assert_eq!(
            v.row_at(0).map(|r| r.item_id),
            Some("s#t3.0".to_string()),
            "ordinal 0 names a row above the base"
        );
        assert!(
            v.row_at(usize::MAX).is_none(),
            "an ordinal past the end is absent"
        );
        assert!(v.row_body_at(usize::MAX).is_none());
        assert_eq!(v.snapshot(100, 0).items_dropped, 0);
        // And the snapshot says how many of its rows sit above that base, because a head cannot
        // work it out: the daemon's own trim adds `n - lead`, and a head that guessed zero drifted
        // five rows on every page it asked for.
        assert_eq!(
            v.snapshot(100, 0).items_lead,
            5,
            "the five ancestors are numbered by nothing, and the snapshot does not say so"
        );
    }

    #[test]
    fn transcript_content_arrives_out_of_band_and_absence_is_honest() {
        let mut log = SessionLog::new("s", LogBounds::default());
        log.append(SessionEvent::TranscriptAppended {
            item_id: "s.0".into(),
            kind: "user".into(),
            ledger_head: "ab".into(),
            item: None,
            body_bytes: 0,
        });
        let mut v = fold(&log);
        assert_eq!(v.snapshot(1, 0).items[0].item, None);
        v.record_item(
            "s.0",
            TranscriptItem::User {
                speaker: Default::default(),
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
                speaker: Default::default(),
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
