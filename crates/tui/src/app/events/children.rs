//! **Events about work this session started**: a subagent's state, a background job settling
//! or writing output.

use super::*;
use letibot_sessionlog::event::SessionEvent;

impl App {
    /// **Work this session started**: a subagent's state, a job settling or writing output. One family of
    /// [`App::event`]'s arms, moved here verbatim; `event` hands it only these variants.
    pub(crate) fn on_child_event(&mut self, e: SessionEvent, ts: u64) -> Disposition {
        match e {
            // A subagent spawn/finish. Fold into the tree, replacing the row with the
            // same session id, so `running` becomes `done` rather than a second line.
            SessionEvent::Subagent {
                subagent_id,
                state,
                prompt,
                role,
                task,
                model,
                answer,
            } => {
                // **The event's own word for the instant**: it publishes `running` when the
                // child's harness comes up and `opening` before that, so `running` is the one
                // state it names in which a turn is generating. See
                // [`SubagentState::generating`].
                let generating = state == "running";
                if let Some(row) = self
                    .subagents
                    .iter_mut()
                    .find(|s| s.session_id == subagent_id)
                {
                    row.state = state;
                    row.generating = generating;
                    row.prompt = prompt;
                    row.role = role;
                    row.task = task;
                    row.model = model;
                    row.answer = answer;
                } else {
                    self.subagents.push(SubagentState {
                        session_id: subagent_id,
                        state,
                        generating,
                        prompt,
                        role,
                        task,
                        model,
                        answer,
                        // When this head heard of it, which is the best a spawn event can say.
                        // `fold_subagents` replaces it with the daemon's own `created_ms` the
                        // moment a list carrying the child arrives.
                        spawned_ms: ts,
                    });
                }
                self.redraw = true;
                Disposition::Filtered
            }
            SessionEvent::JobSettled {
                job,
                state,
                produced,
                elapsed_ms,
            } => {
                // **Folded, never invented.** The daemon owns the table; a
                // settlement for a job this head has not been told about is not a
                // row to make up, it is a row that arrives with the next
                // `ListJobs`. Inventing one is how the pane used to show a job
                // with no command and no idea how it got there.
                if let Some(row) = self.jobs.iter_mut().find(|j| j.id == job) {
                    row.state = state;
                    row.running = false;
                    row.produced = produced;
                    row.elapsed_ms = elapsed_ms;
                    // `never_ran` is deliberately **not** taken from this event: the
                    // settlement carries no such fact, and it cannot be stale here — a
                    // job that never ran never started, so it was never listed as a
                    // running one, and the `never_ran` the row already holds came from
                    // the daemon's own listing (`JobEntry`). A job that ran is never
                    // settled as one that did not.
                }
                // **And the count drops by one**, so the row above the composer re-asks (R51 item
                // 5). The fold just above is the pane's copy: a job settled in a turn this head
                // never watched has no row to fold into, which is exactly the case a count taken
                // from the pane's rows would get wrong.
                self.queued.push(Action::ListJobs);
                self.redraw = true;
                Disposition::Filtered
            }
            // **A job's output, the answer to the jobs pane's Enter.** Not folded
            // into any view: the pane that asked draws the window, and only that
            // pane has anywhere to put it. It is ephemeral besides
            // (`scrub::is_interactive`), so no late head replays one.
            SessionEvent::JobOutput {
                job,
                from,
                to,
                produced,
                dropped,
                state,
                never_ran,
                lines,
                next,
            } => {
                // Taken only when a window is open for *this* job: a head here may
                // have closed the pane with Esc before the reply landed, and a
                // window for a job nobody is looking at is nothing to keep.
                if let Some(v) = self.job_out.as_mut()
                    && v.job == job
                {
                    v.state = state;
                    v.never_ran = never_ran;
                    v.from = from;
                    v.to = to;
                    v.produced = produced;
                    v.dropped = dropped;
                    v.lines = lines;
                    v.next = next;
                    v.loading = false;
                    v.error = None;
                    // A window lands at its **tail**: a fresh page, or a re-read of
                    // a running job, should show what it just wrote. `back` is left
                    // alone, so ← still walks the pages the reader came through.
                    v.scroll = 0;
                    self.redraw = true;
                    return Disposition::Rendered;
                }
                Disposition::Filtered
            }
            _ => unreachable!("on_child_event was handed an event it does not handle"),
        }
    }
}
