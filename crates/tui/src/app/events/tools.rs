//! Events about tool calls: proposed, started, progressing, finished - and an operator's call the
//! gate allowed.

use super::*;
use letibot_sessionlog::event::SessionEvent;
use letibot_sessionlog::view::CallState;

impl App {
    /// Tool calls: proposed, started, progressing, finished, and an operator call the gate allowed.
    /// `event` hands this function only these variants.
    pub(crate) fn on_tool_event(&mut self, e: SessionEvent, ts: u64) -> Disposition {
        match e {
            // The daemon admitted a call the operator is running themselves. Nothing is filed: the
            // admission is the daemon's record and the call's visible half is the result row, so a
            // note would read their own keystroke back. `Filtered`, not `Control`, so it is counted.
            SessionEvent::OperatorCallAllowed { .. } => Disposition::Filtered,
            SessionEvent::ToolCallProposed {
                call_id,
                name,
                target,
                ..
            } => {
                // Not kept beyond the turn, and not put in `call_targets`: the id it would be filed
                // under is reused by the next round, and the target lives on the `CallRow` that draws
                // it, which dies with the turn that made it.
                if let Some(t) = self.turn.as_mut() {
                    // The proposal is the settled form of whatever was being
                    // written, so the pending affordance stands down here.
                    t.writing_call = false;
                    t.calls.push(CallRow {
                        call_id,
                        name,
                        target,
                        state: CallState::Proposed,
                        started_ms: ts,
                        started_at: self.now_ms,
                        ended_ms: 0,
                        note: None,
                        decision: None,
                    });
                }
                Disposition::Rendered
            }
            SessionEvent::ToolStarted { call_id, name, .. } => {
                if let Some(t) = self.turn.as_mut() {
                    match open_call(&mut t.calls, &call_id) {
                        Some(c) => {
                            c.state = CallState::Running;
                            // The clock starts when the tool starts, not when the
                            // model asked for it: a call that waited on a decision
                            // did not spend that time running.
                            c.started_ms = ts;
                            // The head's anchor moves with it: the two clocks are the same clock, so
                            // a row anchored at the proposal would count the decision wait as running
                            // time - the sentence the line above exists to avoid.
                            c.started_at = self.now_ms;
                            // The note from before it started is over: a `ToolProgress` note is a fact
                            // about right now, so one collected while `Proposed` describes the decision
                            // and would read as the run's state for its whole length.
                            c.note = None;
                        }
                        None => t.calls.push(CallRow {
                            call_id,
                            name,
                            // `ToolStarted` carries no target and none is invented.
                            target: String::new(),
                            state: CallState::Running,
                            started_ms: ts,
                            // The clock starts with the same event the log's does, read here rather
                            // than at render so a replay - which has no clock yet - falls back to the
                            // log's, and a decision wait is not counted as running time.
                            started_at: self.now_ms,
                            ended_ms: 0,
                            note: None,
                            decision: None,
                        }),
                    }
                }
                Disposition::Rendered
            }
            // Partial tool output is not accumulated - it has no durable form, and the event is
            // scrubbed for a late head - but the LATEST note is a fact about right now and a running
            // card has a place for it.
            SessionEvent::ToolProgress { call_id, note, .. } => {
                match self
                    .turn
                    .as_mut()
                    .and_then(|t| open_call(&mut t.calls, &call_id))
                {
                    Some(c) => {
                        c.note = Some(note);
                        Disposition::Rendered
                    }
                    None => Disposition::Filtered,
                }
            }
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
                // A backgrounded call is the job STARTING and the only event that says so - there is
                // no `JobStarted` on the wire, so the count must be asked for, and the outcome is the
                // proof. Read here, before it moves into the call's state.
                if matches!(
                    outcome,
                    letibot_transcript::ToolOutcome::Backgrounded { .. }
                ) {
                    self.queued.push(Action::ListJobs);
                }
                if let Some(t) = self.turn.as_mut()
                    && let Some(c) = open_call(&mut t.calls, &call_id)
                {
                    c.ended_ms = ts;
                    c.note = None;
                    c.state = CallState::Finished {
                        outcome,
                        payload_digest,
                        inline_bytes,
                        full_bytes,
                        spill,
                        edit,
                    };
                }
                Disposition::Rendered
            }
            _ => unreachable!("on_tool_event was handed an event it does not handle"),
        }
    }
}
