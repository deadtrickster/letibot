//! Events about the turn: it started, streamed, generated tokens, compacted, finished, failed or
//! was interrupted.

use super::*;
use letibot_sessionlog::event::{DeltaTarget, SessionEvent};
use letibot_sessionlog::view::TurnState;
use letibot_ui::text::without_control_lines;

impl App {
    /// The turn: started, streaming, tokens, compaction, finished, failed, interrupted. `event`
    /// hands this function only these variants.
    pub(crate) fn on_turn_event(&mut self, e: SessionEvent, ts: u64) -> Disposition {
        match e {
            // A fill the daemon NAMED: `what` is the operation in its own words and `done`/`total`
            // its own count, so the head draws the fact rather than inferring a cause - counting
            // rows still lacking a body would name the wrong operation. The line goes at the last.
            SessionEvent::Filling {
                what,
                unit,
                done,
                total,
            } => {
                // A new operation starts the clock; a tick of the same one does not. The fork names
                // its render and its write separately, and each is its own bar from zero.
                if done < total && self.filling.as_ref().is_none_or(|f| f.0 != what) {
                    self.filling_since = self.now_ms;
                }
                self.filling = (done < total).then_some((what, unit, done, total));
                self.redraw = true;
                Disposition::Rendered
            }
            // What the session is waiting on: the daemon's own answer to *will this act again by
            // itself*. The last value is the truth - it publishes only on change - and it is drawn as
            // one muted line on the status row, never a transcript row: a fact about now, not when.
            SessionEvent::ActivityChanged { activity } => {
                // A QUIET value is the daemon saying the prompt is over, and it outranks a round's
                // `ends_turn: false`. Only quiet: a card open mid-turn hides the turn from the list
                // rather than ending anything, and a session owed a turn is not ended either.
                if activity.is_quiet()
                    && let Some(t) = self.turn.as_mut()
                {
                    t.ends_turn = Some(true);
                }
                self.activity = activity;
                self.redraw = true;
                Disposition::Control
            }
            // The held counts, replaced whole: the daemon sends the set as it is now every time it
            // changes, so the last one is the truth and all zeros is nothing held.
            SessionEvent::HeldResults {
                jobs,
                subagents,
                failed,
            } => {
                self.held = (jobs + subagents > 0).then_some((jobs, subagents, failed));
                self.redraw = true;
                Disposition::Rendered
            }
            SessionEvent::CompactionProgress {
                half,
                halves,
                prompt_tokens,
                processed,
                written,
                unit,
            } => {
                // Stored in its own field, which is the point of the event: `self.turn.progress` is
                // the SESSION's turn, and a scratch summary's numbers landing there is the mislabel the
                // progress line was once traded away to avoid. Nothing else writes this one.
                if self.compacting.is_none() {
                    self.compacting_since = self.now_ms;
                }
                self.compacting = Some(CompactionLine {
                    half,
                    halves,
                    prompt_tokens,
                    processed,
                    written,
                    unit,
                });
                self.redraw = true;
                Disposition::Rendered
            }
            SessionEvent::TurnStarted {
                turn_id,
                model,
                ledger_head: _,
                // The WHOLE turn's start, when the emitter knows it: this event fires once per ROUND,
                // so a clock taken from `ts` restarted the header at every round. `ts` stays the
                // fallback for a turn nobody measured - a snapshot, a hand-emitted test event.
                began_ms,
            } => {
                // Asked *before* the pane is replaced. The rows that stop being
                // drawn live are the previous turn's, and once its pane is gone
                // there is nothing left to ask which they were.
                let stale_from = self.turn_first_row();
                let previous = self.turn.as_ref();
                // A boundary the head has ALREADY HAD: a frame may arrive twice, and rebuilding the
                // pane is not idempotent - the round's stream is state the event cannot restore, so a
                // second copy throws its work away. `turn_id` is minted per round: equal ids are one.
                if !turn_id.is_empty() && previous.is_some_and(|t| t.turn_id == turn_id) {
                    return Disposition::Filtered;
                }
                self.model = model.clone();
                // Stamped, so the header can prefer this over a settings row it read
                // earlier at attach. See `model_from_live_at`.
                self.model_from_live_at = self.seq;
                // Which boundary is this? `began_ms` is the only thing on the wire that tells the
                // next ROUND of a prompt from a new PROMPT, so an unmeasured one (a wake turn) over
                // a turn still RUNNING is the same run: the operator's own row ends a run, not this.
                let same_turn = match began_ms {
                    Some(b) => previous.is_some_and(|t| t.started_ms == b),
                    None => previous.is_some_and(|t| matches!(t.state, Some(TurnState::Running))),
                };
                // The clock is never restarted by an event that carries no clock: a boundary with no
                // `began_ms` leaves the base where it was, so the row counts from the prompt that is
                // running. `ts` is the base only for a turn nobody measured at all.
                let started = match (began_ms, same_turn) {
                    (Some(b), _) => b,
                    (None, true) => previous.map(|t| t.started_ms).unwrap_or(ts),
                    (None, false) => ts,
                };
                let turn_rows = if same_turn {
                    previous.map(|t| t.turn_rows.clone()).unwrap_or_default()
                } else {
                    Vec::new()
                };
                self.turn = Some(TurnPane {
                    turn_id,
                    state: Some(TurnState::Running),
                    started_ms: started,
                    last_ms: started,
                    turn_rows,
                    ..TurnPane::default()
                });
                // The pane's half goes on EVERY boundary: a lift on `LIVE_RUN` belongs to the round
                // that is over, and `same_turn` is a guess a state reset must not rest on. A panel on
                // a run of ROWS is the reader's, and only a new prompt - a new subject - closes it.
                if !same_turn || self.payload_sel.as_deref() == Some(runclick::LIVE_RUN) {
                    self.drop_lift();
                }
                // The pane leaving hands its calls back to the previous turn's rows, which now own
                // everything they proposed. The history is cached, so it is invalidated from the first
                // row that pane owned - the only part of it that can render differently now.
                if let Some(k) = stale_from {
                    self.invalidate_history_from(k);
                }
                Disposition::Rendered
            }
            SessionEvent::PromptProgress { progress, .. } => {
                if let Some(t) = self.turn.as_mut() {
                    t.progress = Some(progress);
                }
                // Shown on the status line, not in the transcript - which is what a
                // progress frame is for. It counts as rendered because it does
                // change the screen.
                Disposition::Rendered
            }
            SessionEvent::TokensGenerated { turn_id, tokens } => {
                let Some(t) = self.turn.as_mut() else {
                    return Disposition::Filtered;
                };
                if t.turn_id != turn_id {
                    return Disposition::Filtered;
                }
                // The counter this event moves is not drawn. It still counts as
                // `Rendered` - the spinner beside the clock moves on it, which is the row's whole
                // business now - and as liveness for the stuck line, the same way a delta does.
                let _ = tokens;
                Disposition::Rendered
            }
            SessionEvent::Delta {
                target,
                text,
                turn_id,
            } => {
                let Some(t) = self.turn.as_mut() else {
                    return Disposition::Filtered;
                };
                if t.turn_id != turn_id {
                    if std::env::var("LETIBOT_MARKER_DEBUG").is_ok() {
                        eprintln!(
                            "MARKER delta REFUSED: pane={:?} delta={:?} target={:?} bytes={}",
                            t.turn_id,
                            turn_id,
                            target,
                            text.len()
                        );
                    }
                    return Disposition::Filtered;
                }
                match target {
                    DeltaTarget::Text => {
                        // Model prose is content this head did not author: a control byte it emits
                        // or copies out of a file is an instruction to the operator's terminal. It is
                        // sanitised here because the frozen-prefix lexer cannot un-freeze a byte.
                        let text = without_control_lines(&text);
                        t.text.push(&text);
                        Disposition::Rendered
                    }
                    DeltaTarget::Reasoning => {
                        // Accumulated at every rung, drawn only where the rung shows it: `conversation`
                        // hides the working, it does not discard it, so the text has to be here to be
                        // counted, drawn when the reader opens the run, and there when they change rung.
                        if t.think_started_ms == 0 {
                            t.think_started_ms = ts;
                        }
                        t.think_last_ms = ts;
                        // The same rule as the answer above: reasoning is the model's own text and
                        // reaches the glass through the markdown renderer. And it counts - on a
                        // `messages` backend this is the channel that streams from the first second.
                        let text = without_control_lines(&text);
                        t.reasoning.push(&text);
                        // The `thinking` switch, not a rung comparison: a set that hides the reasoning
                        // rows cannot count them as rendered, which `>= Normal` got wrong at the bottom
                        // rung.
                        if self.visibility.shows(Show::Thinking) {
                            Disposition::Rendered
                        } else {
                            Disposition::Filtered
                        }
                    }
                    // Never `t.text`: this is the markup, and the channel exists because the default
                    // view does not show it. It is reachable through `ctrl-x`, so it is sanitised where
                    // it is drawn (`raw_call_lines`) rather than here.
                    DeltaTarget::ToolCall => {
                        // The other channel that arrives and that the row could not see - and the one
                        // an agentic round is made of: a call's arguments come as deltas, and the row
                        // sat at a frozen number while thousands of tokens of this went by.
                        t.raw_call.push_str(&text);
                        t.writing_call = true;
                        Disposition::Rendered
                    }
                }
            }
            SessionEvent::TurnFinished {
                finish_reason,
                usage,
                timings,
                ends_turn,
                prefix_check,
                dialect_template_sha,
                ..
            } => {
                // Kept on the head, not only on the pane: the header says how much context the
                // conversation carries, and that is asked between turns, when the pane may have been
                // superseded by the transcript. The turn measured its own cache, so the number is real.
                self.usage = Some(usage);
                // Summed as the turns land. A turn with no cost - a local server, or a metered model
                // nothing prices - adds nothing and does not light the meter: free and unpriced are
                // both "no number", and `$0.0000` on every local header would be noise.
                if let Some(c) = usage.cost_micros_usd {
                    self.spent_micros += c;
                    self.spent_seen = true;
                }
                self.usage_cache_measured = true;
                self.last_timings = Some(timings);
                // The ending's own record of what the engine measured: a head that cannot read the
                // check reads zero reuse as a prompt that diverged, and the sha names the dialect it
                // was measured under. Read by `/status`; never derived from the numbers here.
                self.prefix_check = prefix_check;
                self.dialect_template_sha = dialect_template_sha;
                // The count is re-read at the turn's end: a job already running when this head
                // attached is announced by no event at all - the `ToolFinished` that started it came
                // before the attach, and a `JobSettled` may be an hour away.
                self.queued.push(Action::ListJobs);
                if let Some(t) = self.turn.as_mut() {
                    t.progress = None;
                    // The round's own ending, and the daemon's word on whether it ended the PROMPT:
                    // the row reads it in `turn_busy`, so a round boundary does not stand the row down.
                    t.ends_turn = Some(ends_turn);
                    t.state = Some(TurnState::Finished {
                        finish_reason,
                        usage,
                        timings,
                    });
                }
                // What the live pane had lifted stays lifted on the rows this round left.
                self.carry_live_open();
                // A terminal state hands the turn's calls back to the transcript, which does not draw
                // them while the pane is up. The history is cached, so it is invalidated from the first
                // row the pane owned - the only part that can render differently now.
                self.invalidate_turn_rows();
                Disposition::Rendered
            }
            // The terminal event for a failed turn. Without it `TurnState` stayed `Running` and the
            // spinner span at a dead turn; the "nothing received for" line below stays as the
            // disclosure that covers a genuine stall.
            SessionEvent::TurnFailed {
                turn_id,
                error,
                partial_kept,
            } => {
                if let Some(t) = self.turn.as_mut()
                    && (t.turn_id == turn_id || turn_id.is_empty())
                {
                    t.progress = None;
                    t.ends_turn = Some(true);
                    t.state = Some(TurnState::Failed {
                        error,
                        partial_kept,
                    });
                }
                // A terminal state hands the turn's calls back to the transcript, which does not draw
                // them while the pane is up. The history is cached, so it is invalidated from the first
                // row the pane owned - the only part that can render differently now.
                self.carry_live_open();
                self.invalidate_turn_rows();
                Disposition::Rendered
            }
            SessionEvent::TurnInterrupted {
                reason,
                partial_kept,
                ..
            } => {
                if let Some(t) = self.turn.as_mut() {
                    t.progress = None;
                    t.ends_turn = Some(true);
                    t.state = Some(TurnState::Interrupted {
                        reason,
                        partial_kept,
                    });
                }
                // A terminal state hands the turn's calls back to the transcript, which does not draw
                // them while the pane is up. The history is cached, so it is invalidated from the first
                // row the pane owned - the only part that can render differently now.
                self.carry_live_open();
                self.invalidate_turn_rows();
                Disposition::Rendered
            }
            _ => unreachable!("on_turn_event was handed an event it does not handle"),
        }
    }
}
