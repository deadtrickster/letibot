//! **Warnings**: a diagnostic the daemon raised, routed to the conversation, the notes or the
//! alarm.

use super::*;
use letibot_sessionlog::event::SessionEvent;
use letibot_sessionlog::view::Warned;
use letibot_ui::text::without_control;

impl App {
    /// **A warning the daemon raised.** One family of
    /// [`App::event`]'s arms, moved here verbatim; `event` hands it only these variants.
    pub(crate) fn on_warning(&mut self, e: SessionEvent, ts: u64) -> Disposition {
        match e {
            SessionEvent::Warning { code, detail, .. } => {
                // `turn_failed` is the log's grep-able record of the same fact
                // `TurnFailed` puts under the turn, and the daemon publishes both
                // on purpose — one is state, the other is history. On a *screen*
                // they are the same sentence twice, three lines apart, so this head
                // renders the terminal state and counts the warning as filtered.
                // Counted, not dropped: the status line's `filtered` is what makes
                // "I chose not to show this" different from "nothing happened".
                if code == "turn_failed" {
                    return Disposition::Filtered;
                }
                // **R16, the two halves of a fork.** `auto_compact` is published
                // *before* the fork and says the conversation is about to be
                // replaced; `compacted`/`reseated` are published after it and say it
                // has been. An echo in the air across those two lines was waiting for
                // a row the fork summarised away, so it is resolved here rather than
                // left saying `queued` for the rest of the session.
                if code == "auto_compact" {
                    self.mark_fork();
                }
                if code == "compacted" || code == "reseated" {
                    self.resolve_fork();
                    // **And the fold is over, so the line that walks stops walking.** The
                    // progress event is ephemeral by construction — it has no "done" — so
                    // what ends it is this: the durable warning that says the fork landed.
                    // `auto_compact_failed` clears it too, below: a fold that failed is not
                    // a fold that is still running, and a line that outlives its operation is
                    // the stale measurement this file's `ToolStarted` arm already refuses.
                    self.compacting = None;
                }
                if code == "auto_compact_failed" {
                    self.compacting = None;
                }
                // **The guard's finding becomes a state this head holds.** The
                // warning is the live half of the channel — this head was
                // attached to see the guard fire — and `auto_compact_off` is
                // what the resident line and `/status` read. The settings row is
                // the other half, for a head that attaches later; see the
                // `Settings` arm.
                if code == "auto_compact_no_progress" {
                    self.auto_compact_off = Some(format!("auto-compaction is off: {detail}"));
                }
                // A slash LISTING opens the pane; a slash sentence stays a note.
                // The daemon sends both under one code — `detail` is the command
                // it echoes back, then the reply — so the head splits them by the
                // only thing that distinguishes them, which is length.
                if code == "slash" || code == "slash_refused" {
                    let (echo, body) = detail.split_once('\n').unwrap_or((&detail, ""));
                    let lines: Vec<String> = body.lines().map(|l| without_control(l)).collect();
                    if lines.len() > 3 {
                        self.slash_out = Some((echo.to_string(), lines));
                        self.push_surface(Surface::Listing);
                        return Disposition::Rendered;
                    }
                }
                // **A refused read a completion notice asked for is the notice's answer** —
                // the head asked on its own, for a row's second line, and a refusal of THAT is
                // not something the operator did and has to be told about in the record. The
                // notice says it instead (its popup shows the daemon's words). Matched by the
                // handle the refusal names, or — for a refusal that names none (*this session
                // has no process host*) — every notice ask still pending, when no pane is
                // waiting on a read of its own.
                if code == "job_output_refused" {
                    use crate::ui::transcript::job_card::JobTail;
                    let pending: Vec<String> = self
                        .job_tails
                        .iter()
                        .filter(|(_, t)| matches!(t, JobTail::Asked))
                        .map(|(j, _)| j.clone())
                        .collect();
                    let pane_reading = self.job_out.as_ref().is_some_and(|v| v.loading);
                    let named: Vec<String> = pending
                        .iter()
                        .filter(|j| detail.contains(&format!("`{j}`")))
                        .cloned()
                        .collect();
                    let theirs = if !named.is_empty() {
                        named
                    } else if !pane_reading {
                        pending
                    } else {
                        Vec::new()
                    };
                    let pane_named = self
                        .job_out
                        .as_ref()
                        .is_some_and(|v| v.loading && theirs.contains(&v.job));
                    if !theirs.is_empty() && !pane_named {
                        for j in theirs {
                            self.job_tail_landed(&j, JobTail::Gone(detail.clone()));
                        }
                        return Disposition::Filtered;
                    }
                }
                // A refused job-output read is answered **in the pane that asked**,
                // which is still open — otherwise it would sit at `reading…` for
                // ever, waiting for a window that is not coming. The conversation
                // gets the note as well.
                if code == "job_output_refused"
                    && let Some(v) = self.job_out.as_mut()
                {
                    v.loading = false;
                    v.error = Some(detail.clone());
                    self.redraw = true;
                }
                // **A note about the weather goes on the edge, not in the record.**
                //
                // The operator, on `model_slow_first_byte`: *"it is important diagnostics -
                // we have a yellow triangle for that. both heads should not emit it inside
                // conversation."* So the diagnostic is kept and its PLACEMENT is moved: the
                // count moves a counter, the triangle comes up, and `/status` is where the
                // number lives. `warning::ALARM_ONLY` is the rule and the docstring there
                // says why a compaction stays a row and this does not.
                //
                // **Counted, and `Filtered` rather than dropped.** `Filtered` is what makes
                // "I chose not to show this" different from "nothing happened" — the same
                // distinction the `turn_failed` arm above is refused for. And if this head
                // has no register for a code the tree says is edge-bound, it says so rather
                // than swallowing it: a note that reaches neither the record nor a counter
                // is a note nobody has.
                if letibot_sessionlog::warning::to_the_alarm(&code) {
                    if !self.count_edge_note(&code) {
                        self.note(Note::Warned(Warned {
                            code: "alarm_only_unregistered".into(),
                            detail: format!(
                                "`{code}` is classified as edge-bound and this head has no \
                                 counter for it, so the diagnostic above is the only copy. \
                                 See `warning::ALARM_ONLY`."
                            ),
                            ts,
                        }));
                    }
                    self.redraw = true;
                    return Disposition::Filtered;
                }
                self.note(Note::Warned(Warned { code, detail, ts }));
                Disposition::Rendered
            }
            _ => unreachable!("on_warning was handed an event it does not handle"),
        }
    }
}
