//! **Events about tool calls**: proposed, started, progressing, finished — and an operator's
//! call the gate allowed.

use super::*;
use letibot_sessionlog::event::SessionEvent;
use letibot_sessionlog::view::CallState;

impl App {
    /// **Tool calls**: proposed, started, progressing, finished, and an operator call the gate allowed. One family of
    /// [`App::event`]'s arms, moved here verbatim; `event` hands it only these variants.
    pub(crate) fn on_tool_event(&mut self, e: SessionEvent, ts: u64) -> Disposition {
        match e {
            // A background job settled — the daemon publishes this between turns,
            // which is exactly when nothing else could say it. Fold into the row
            // the backgrounded finish pushed; a settlement whose start is beyond
            // this head's window still gets its row, with the command unknown,
            // because a job that ran is a fact even when its beginning scrolled
            // off.
            // **The daemon has admitted a call the operator is running themselves** (R24 part
            // two, decision 4). Nothing is filed as a note: the admission is the daemon's
            // record, and the call's visible half is the result row, which arrives with its
            // `origin` set. A head that narrated the admission would be reading a keystroke the
            // operator just made back to them.
            //
            // **Counted, not ignored.** A head that runs operator calls wants to know one was
            // admitted; a head that does not — this one, until the chord lands — must not
            // swallow it silently, or *"the event never came"* and *"this head drops it"* look
            // the same, and the second is the one a reader would never find.
            //
            // **`Filtered` and NOT `Control`, and the difference is the whole of R53 §1.2.** The
            // paragraph above asks for "counted, not ignored" and the code answered `Control`,
            // whose own definition is *"Not an event: a Hello, a Resync, a command reply"* — so the
            // arm contradicted the variant's documentation and its own comment at the same time.
            // `Filtered` is the counted one and says so. MEASURED consequence before the fix: the
            // `filtered` figure on `Ack` (and on `/status`) differed by one per occurrence between
            // the two heads, and letibot's stated protection against *"the event never came"*
            // reading exactly like *"this head drops it"* was not in force on the one event whose
            // comment argues for it. **No test caught it because nothing asserts the count for
            // this event, and the comment reads as the specification** — a reader checking the
            // file found an argument for the correct behaviour sitting on top of the incorrect
            // one, which is the same failure mode as a docstring describing colours over a
            // function that returns a plain string.
            SessionEvent::OperatorCallAllowed { .. } => Disposition::Filtered,
            SessionEvent::ToolCallProposed {
                call_id,
                name,
                target,
                ..
            } => {
                // Not kept beyond the turn, and not put in `call_targets`. It used
                // to be, on the argument that a settled `Assistant { tool_calls }`
                // row needs the word and the proposal is where the head saw it —
                // but the row carries the arguments the word is derived from, and
                // the id it would be filed under is reused by the next round. The
                // proposal's target lives on the `CallRow` below, which is the row
                // that draws it, and dies with the turn that made it.
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
                            // **And the head's anchor moves with it** (R13): the two
                            // clocks are the same clock, so a row anchored at the
                            // *proposal* would count the decision wait as running
                            // time — the exact sentence the line above exists to
                            // avoid, arrived at from the other side.
                            c.started_at = self.now_ms;
                            // **And the note from before it started is over.**
                            //
                            // `ToolProgress` notes are facts about right now, and
                            // the ones a call collects while it is `Proposed` are
                            // about the DECISION — "asking the guard". Only
                            // `ToolFinished` cleared the field, so that sentence
                            // rode the card through the whole run.
                            //
                            // Measured 2026-09-20 in the rano session: `cargo test
                            // stream_tests` ran for its full 300 s deadline with
                            // "asking the guard" underneath it the entire time. The
                            // operator read the screen exactly as it was written and
                            // reported the session hung requesting the oracle — the
                            // guard had answered in milliseconds and the diagnosis
                            // cost an hour. A stale note is worse than no note: it
                            // is a measurement of a moment that has passed, with
                            // nothing on it to say so.
                            //
                            // Cleared rather than replaced: the tool is running and
                            // the phase already says so. A note appears again when
                            // the tool sends one of its own.
                            c.note = None;
                        }
                        None => t.calls.push(CallRow {
                            call_id,
                            name,
                            // `ToolStarted` carries no target and none is invented.
                            target: String::new(),
                            state: CallState::Running,
                            started_ms: ts,
                            // **The clock starts with the same event the log's
                            // does** (R13): a call that waited on a decision did
                            // not spend that time running, and the head's clock is
                            // read here rather than at render so that a replay —
                            // which has no clock yet — falls back to the log's.
                            started_at: self.now_ms,
                            ended_ms: 0,
                            note: None,
                            decision: None,
                        }),
                    }
                }
                Disposition::Rendered
            }
            // Still not accumulated — partial tool output has no durable form, and
            // this event is interactive-only and scrubbed for a late head. But the
            // *latest* note is a fact about right now and a running card has a
            // place for it, which a one-line renderer did not: the old comment
            // ("nowhere to put it, by design") was true of the renderer, not of
            // the event.
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
                // A backgrounded call leaves a job behind, and the row the jobs
                // pane shows starts here: the handle is the outcome's own field,
                // not a parse of the result text. The command joins at draw time,
                // through the call's §4.1 target.
                // A backgrounded finish used to push a row here, built from the
                // call this head happened to be holding. The daemon publishes its
                // table every round now, so the row arrives with the next
                // `ListJobs` — with a command that is right whichever turn it is
                // read in.
                // **A backgrounded call is the job STARTING, and the only event that says so**
                // (R51 item 5). There is no `JobStarted` on the wire: `JobSettled` is the whole
                // job vocabulary, so the count cannot be folded out of events — it has to be
                // ASKED for. The proof is the outcome rather than the tool's name: only a call
                // that was actually backgrounded carries a handle.
                //
                // Without this the row a reader sees is a **souvenir**: a count drawn from a list
                // only `/jobs` ever fetched sits at zero for the life of the job.
                //
                // Asked BEFORE the outcome is moved into the call's own state, because this is the
                // last read of it in this arm.
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
