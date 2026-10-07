//! **Events about the turn**: it started, streamed, generated tokens, compacted, finished, failed
//! or was interrupted.

use super::*;
use letibot_sessionlog::event::{DeltaTarget, SessionEvent};
use letibot_sessionlog::view::TurnState;
use letibot_ui::text::without_control_lines;

impl App {
    /// **The turn**: started, streaming, tokens, compaction, finished, failed, interrupted. One family of
    /// [`App::event`]'s arms, moved here verbatim; `event` hands it only these variants.
    pub(crate) fn on_turn_event(&mut self, e: SessionEvent, ts: u64) -> Disposition {
        match e {
            // **A fill the daemon NAMED (R6).** `what` is the operation in the daemon's
            // own words and `done`/`total` are its own count in the `unit` it named — so
            // the head draws the fact instead of inferring a cause from a symptom.
            // Counting the rows still lacking a body would draw *"N rows announced and
            // never filled in"* three seconds into a healthy generation (see
            // `BODY_PATIENCE`), and — worse — would name the wrong operation: an ordinary
            // reply is not a carry.
            //
            // Ephemeral (`scrub::is_interactive`), so a late head never replays a tick.
            // On the last one the line goes: the daemon's durable finish note is what
            // says the operation ended, and a bar left at `total of total` would sit on
            // the screen for ever.
            SessionEvent::Filling {
                what,
                unit,
                done,
                total,
            } => {
                self.filling = (done < total).then_some((what, unit, done, total));
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
                // **Stored in its own field, which is the whole point of the event.** This
                // must not touch `self.turn.progress`: that slot is the SESSION's turn, and
                // a scratch summary's numbers landing there is exactly the mislabel the
                // old suppression traded away the progress line to avoid. Nothing else
                // writes `self.compacting`, so the two cannot be confused.
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
                // **The WHOLE turn's start, when the emitter knows it.** This event fires once
                // per ROUND, so `started_ms: ts` below restarted the header's clock at every
                // round — `2.1s` a minute into a turn. `began_ms` is the prompt's own stamp and
                // is used when present; `ts` stays as the fallback for a turn nobody measured
                // (a snapshot, or a test that emits the event by hand).
                began_ms,
            } => {
                // Asked *before* the pane is replaced. The rows that stop being
                // drawn live are the previous turn's, and once its pane is gone
                // there is nothing left to ask which they were.
                let stale_from = self.turn_first_row();
                let previous = self.turn.as_ref();
                // **A boundary the head has ALREADY HAD, taken again.** The frame is an event like
                // any other and may be delivered twice — and rebuilding the pane here is not
                // idempotent: the round's own stream (its text, its reasoning, its proposed calls)
                // is state the event does not describe and cannot restore, so a second copy of one
                // boundary would throw the round's work away and re-open a run that is already
                // open.
                //
                // `turn_id` is the key that can say so, and it is a sound one: `run_turn_steered`
                // mints it as `{transcript_id}#{turn_seq}` with `turn_seq += 1` per round, and a
                // restored session starts that counter at the number of restored items precisely
                // so that *"within one transcript the ids never repeat"* (`crates/turn/src/
                // resume.rs`, "`turn_seq` is a watermark, not a turn count"). So an equal
                // non-empty `turn_id` is one round, and this event has already been folded in.
                if !turn_id.is_empty() && previous.is_some_and(|t| t.turn_id == turn_id) {
                    return Disposition::Filtered;
                }
                self.model = model.clone();
                // Stamped, so the header can prefer this over a settings row it read
                // earlier at attach. See `model_from_turn_at`.
                self.model_from_turn_at = self.seq;
                // **Which boundary is this?** — and only one of the answers is *a new run*.
                //
                // [`TurnPane::turn_rows`] hangs the run on `began_ms`, because it is stamped once
                // per prompt and carried by every round of it: the one thing on the wire that
                // distinguishes *the next round of this prompt* from *a new prompt*. What the head
                // then does with it is the defect this closes, and the mistake is asking the wrong
                // question — *does this number match* rather than *may I start a new run here*.
                //
                // A boundary the head cannot MATCH is not a boundary it can tell APART. `began_ms:
                // None` means *nobody measured this one*, and two very different events carry it: a
                // turn that began before this head attached, and **a turn the daemon ran without
                // stamping its clock at all.** The second is real and reachable —
                // `harnessd::sessions::run_prompt` calls `begin_turn_clock` and
                // `Sessions::wake` does not, so the turn a job's settlement or a monitor's firing
                // opens arrives with no start in it (`harnessd/src/sessions.rs`: `run_prompt` vs
                // `wake`).
                //
                // Read as a new run, such a boundary emptied `turn_rows` and moved `started_ms` to
                // the event's own `ts`: the run the work is still in stopped reading as this turn's,
                // so `live_here` and `walk_carried_live` both answered no, the pane drew its OWN
                // marker beside the walk's, and the `Responding` clock restarted. The operator's
                // report, in their words: *"it looks like turn end or some other border is
                // misinterpreted and Responding timer resets and I get new line with `[N thinking
                // lines]` which then gets merged to the previous `[N tools, M thinking]`"* — and the
                // merge is the next row landing, which puts a row of the turn back into
                // `turn_rows`.
                //
                // **So an unmeasured boundary over a turn that is still RUNNING is the same run.**
                // The head is drawing that turn right now; nothing the reader can see has separated
                // the work before the event from the work after it; and the row filter, not this
                // event, is what actually ends a run on the screen ([`row_drawn`]) — a new prompt
                // is preceded by the operator's own message, which is a drawn row and ends the run
                // by itself. A head that says *new run* here is inventing a border out of an event
                // that carries no evidence for one, which is exactly the second `Responding` the
                // operator watched appear.
                let same_turn = match began_ms {
                    Some(b) => previous.is_some_and(|t| t.started_ms == b),
                    None => previous.is_some_and(|t| matches!(t.state, Some(TurnState::Running))),
                };
                // **The clock is never restarted by an event that carries no clock.** Same rule, on
                // the one field the operator watches move: a boundary with no `began_ms` leaves the
                // base where it was, so the row keeps counting from the prompt that is actually
                // running rather than from the frame that arrived. `ts` stays the base for a turn
                // nobody measured at all — the first `TurnStarted` of a head that joined late.
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
                    model,
                    state: Some(TurnState::Running),
                    started_ms: started,
                    last_ms: started,
                    turn_rows,
                    ..TurnPane::default()
                });
                // A new turn takes the pane away from the previous one, so the
                // previous one's rows now own everything they proposed. Same
                // reason as the terminal states above: the history is cached —
                // from the first row that pane owned, which is the only part of
                // it that can render differently now.
                if let Some(k) = stale_from {
                    self.invalidate_history_from(k);
                }
                Disposition::Rendered
            }
            SessionEvent::PromptProgress { progress, .. } => {
                if let Some(t) = self.turn.as_mut() {
                    t.progress = Some(progress);
                }
                // Shown on the status line, not in the transcript — which is what a
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
                // **The counter it used to move is no longer drawn** — see `turn_status`: the
                // operator does not want the number to read. The event still counts as
                // `Rendered`, because it is a frame in which the turn is plainly alive: the
                // spinner beside the clock moves on it, and that is the row's whole business
                // now. It also counts as liveness for the stuck line, the same way a delta does.
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
                        // **§3.1: model prose is content this head did not author**, and
                        // this is where it enters — every streamed chunk that becomes the
                        // answer goes through here and ends up as a row `paint_full`
                        // writes verbatim. A control byte the model emits (or copies out
                        // of a file it read) is an instruction to the operator's
                        // terminal: `ESC ] 0 ; … BEL` sets the window title, `ESC [ 2 J`
                        // clears the screen.
                        //
                        // Sanitised here rather than at the renderer because
                        // `IncrementalMarkdown` is a frozen-prefix lexer (§13.3): a
                        // control byte that reaches it is frozen into the stable half
                        // and cannot be removed later without re-lexing. The trade is
                        // `without_control`'s own — one space per control byte, so the
                        // character count `arrived_chars` keeps is unchanged.
                        let text = without_control_lines(&text);
                        t.text.push(&text);
                        Disposition::Rendered
                    }
                    DeltaTarget::Reasoning => {
                        // **Accumulated at every rung, drawn only where the rung shows it.**
                        //
                        // The guard used to be on the accumulation, which is the same mistake
                        // R37's marker made one layer up: `conversation` *hides* the working and
                        // does not *discard* it — the rung is a view — so the text has to be
                        // here to be counted, to be drawn when the reader opens the run, and to
                        // be there the moment they change rung. Found by the marker's own test:
                        // at `conversation` nothing accumulated, so the streamed thinking the
                        // operator was watching could not be counted.
                        if t.think_started_ms == 0 {
                            t.think_started_ms = ts;
                        }
                        t.think_last_ms = ts;
                        // The same rule as the answer above, for the same reason: reasoning is
                        // the model's own text and it reaches the glass through the markdown
                        // renderer. **And it counts**, for the reason the field gives: on a
                        // `messages` backend this is the channel that streams from the first
                        // second, and it was the one the row's number could not see.
                        let text = without_control_lines(&text);
                        t.reasoning.push(&text);
                        // **The `thinking` switch, not a rung comparison** — it is the same
                        // question (`>= Normal` meant *the reasoning rows are drawn*, and at the
                        // bottom rung they are not) asked of the set, so a set that hides the
                        // reasoning cannot count it as rendered.
                        if self.visibility.shows(Show::Thinking) {
                            Disposition::Rendered
                        } else {
                            Disposition::Filtered
                        }
                    }
                    // Never `t.text`. This is the markup, and the whole point of
                    // the channel is that the default view does not show it — see
                    // `letibot_sessionlog::event::DeltaTarget`. It *is* reachable
                    // through `ctrl-x`, so it is sanitised where it is drawn
                    // (`raw_call_lines`) rather than here.
                    DeltaTarget::ToolCall => {
                        // **The other channel that arrives and that the row could not see**, and
                        // the one an agentic round is *made of*: a call's arguments come as deltas,
                        // and on the backend that produced the measurement above the row sat at a
                        // frozen number while thousands of tokens of exactly this went by.
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
                ..
            } => {
                // Kept on the head, not only on the pane: the session header says
                // how much context this conversation is carrying, and that question
                // is asked between turns, when the pane may have been superseded by
                // the transcript. The timings are kept with it — the header now
                // carries the turn's rate and duration too, which is why the footer
                // no longer does. The turn measured its own cache, so the
                // percentage is real.
                self.usage = Some(usage);
                // Summed as the turns land. A turn with no cost — the local
                // server, or a metered model nothing prices — adds nothing and
                // does not light the meter: free and unpriced are both "no
                // number", and `$0.0000` on every local header would be noise.
                if let Some(c) = usage.cost_micros_usd {
                    self.spent_micros += c;
                    self.spent_seen = true;
                }
                self.usage_cache_measured = true;
                self.last_timings = Some(timings);
                // **And the count is re-read at the turn's end** (R51 item 5's third moment). A job
                // that was already running when this head attached is announced by no event at all:
                // the `ToolFinished` that started it happened before the attach, and a `JobSettled`
                // may be an hour away. The turn boundary is the moment the head knows it has been
                // through a round without hearing about it, so it is where the count gets its chance
                // to be right.
                self.queued.push(Action::ListJobs);
                if let Some(t) = self.turn.as_mut() {
                    t.progress = None;
                    t.state = Some(TurnState::Finished {
                        finish_reason,
                        usage,
                        timings,
                    });
                }
                // A terminal state can hand a call back to the transcript.
                // While the pane is drawing a turn, that turn's assistant rows do
                // not draw their own unsettled calls; once it stands down they
                // must, or a call the turn was interrupted in the middle of leaves
                // the screen with nothing said about it. The rendered history is
                // cached, so it has to be told — from the first row this pane
                // owns, which is the only part of it that can render differently
                // now.
                self.invalidate_turn_rows();
                Disposition::Rendered
            }
            // §4.5's terminal event, which did not exist. The head used to be told
            // a `Warning` and nothing else, so `TurnState` stayed `Running` and the
            // spinner span at a dead turn until somebody closed the window; the
            // "nothing received for 17.0s" line below is the *disclosure* that
            // covered for it, and it stays as the backstop for a genuine stall.
            SessionEvent::TurnFailed {
                turn_id,
                error,
                partial_kept,
            } => {
                if let Some(t) = self.turn.as_mut()
                    && (t.turn_id == turn_id || turn_id.is_empty())
                {
                    t.progress = None;
                    t.state = Some(TurnState::Failed {
                        error,
                        partial_kept,
                    });
                }
                // A terminal state can hand a call back to the transcript.
                // While the pane is drawing a turn, that turn's assistant rows do
                // not draw their own unsettled calls; once it stands down they
                // must, or a call the turn was interrupted in the middle of leaves
                // the screen with nothing said about it. The rendered history is
                // cached, so it has to be told — from the first row this pane
                // owns, which is the only part of it that can render differently
                // now.
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
                    t.state = Some(TurnState::Interrupted {
                        reason,
                        partial_kept,
                    });
                }
                // A terminal state can hand a call back to the transcript.
                // While the pane is drawing a turn, that turn's assistant rows do
                // not draw their own unsettled calls; once it stands down they
                // must, or a call the turn was interrupted in the middle of leaves
                // the screen with nothing said about it. The rendered history is
                // cached, so it has to be told — from the first row this pane
                // owns, which is the only part of it that can render differently
                // now.
                self.invalidate_turn_rows();
                Disposition::Rendered
            }
            _ => unreachable!("on_turn_event was handed an event it does not handle"),
        }
    }
}
