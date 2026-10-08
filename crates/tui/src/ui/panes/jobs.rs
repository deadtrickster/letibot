//! **The jobs pane**: the background jobs this session started, and a job's output — drawn
//! by `rano::agent::jobs` from the daemon's job table this head keeps.

use crate::app::*;
use crate::ui::render::row_strings;
use rano::agent::jobs::{JobOutput, JobRow, JobsPane};

impl App {
    /// The job-output view: one job's retained window, with its own header because
    /// the daemon sent the **offsets** rather than a sentence. The tail shows by
    /// default; arrows walk back toward the beginning of the loaded window; the
    /// footer says how much further the reader can go.
    pub(crate) fn job_out_lines(&mut self, room: usize) -> Vec<String> {
        let p = self.cfg.palette();
        let Some(v) = self.job_out.as_mut() else {
            return Vec::new();
        };
        let view = JobOutput {
            job: v.job.clone(),
            error: v.error.clone(),
            loading: v.loading,
            state: v.state.clone(),
            never_ran: v.never_ran,
            redirect: v.redirect.clone(),
            from: v.from,
            to: v.to,
            produced: v.produced,
            dropped: v.dropped,
            lines: v.lines.clone(),
            scroll: v.scroll,
            has_next: v.next.is_some(),
            has_back: !v.back.is_empty(),
        };
        let (lines, scroll) = view.lines(room);
        // The scroll clamped against what was drawn, stored back: the arrows move it, and
        // only the draw knows how much of the window fits.
        if v.error.is_none() {
            v.scroll = scroll;
        }
        row_strings(&lines, p)
    }

    /// **How many jobs this session has running, for the composer's top edge** — R51 item 5.
    ///
    /// `None` when the answer is zero, and absent rather than `0 jobs`: a count that is always
    /// there is furniture, and the edge it sits on is spent on facts that are true only while they
    /// are.
    ///
    /// **The count comes from the daemon's table, never from a fold of the events.** There is no
    /// `JobStarted` on the wire, so a head counting what it has seen start and settle would be
    /// guessing at the one number the daemon already knows — and would get it wrong for a job that
    /// was already running when the head attached. What the head owes is to ASK, at the three
    /// moments it can know something changed (`ToolFinished` with a `backgrounded` outcome, a
    /// `JobSettled`, and the end of a turn); see those arms.
    ///
    /// **And the redirected half, which is this head's own addition to the requirement.** A job
    /// whose output goes to a file has a window that will be EMPTY however long it runs (R41,
    /// `JobEntry::redirect`), so *"3 jobs running"* and *"3 jobs running, one of which you cannot
    /// watch"* are different answers to the question this row exists to answer. It is said as a
    /// count of the running ones, because that is the set the reader is about to go and look at:
    ///
    /// ```text
    /// 3 jobs running · 1 to a file
    /// ```
    ///
    /// A settled job's redirect is not counted: the row is about what is running now, and a
    /// finished job's output is readable wherever it went.
    #[cfg(test)]
    pub(crate) fn jobs_line(&self) -> Option<String> {
        self.jobs_view().running_line()
    }

    /// **The jobs pane's facts, in rano's words**: the daemon's table as it stands, the fold
    /// of the finished ones, and the cursor.
    pub(crate) fn jobs_view(&self) -> JobsPane {
        JobsPane {
            jobs: self
                .jobs
                .iter()
                .map(|j| JobRow {
                    id: j.id.clone(),
                    command: j.command.clone(),
                    how: j.how.clone(),
                    state: j.state.clone(),
                    running: j.running,
                    never_ran: j.never_ran,
                    redirect: j.redirect.clone(),
                    produced: j.produced,
                    elapsed_ms: j.elapsed_ms,
                })
                .collect(),
            finished_open: self.jobs_finished_open,
            selected: self.jobs_sel,
        }
    }

    /// **The daemon's job table, as rows** — running first, then one folded `finished (N)` row.
    ///
    /// The operator's own ask: *"jobs panel - same as subagents - show list of running, group
    /// finished"*. The rows come from [`App::job_stops`] — the same enumeration the arrows,
    /// Enter and the drawn `▸` read — and each stop's line is recorded in
    /// [`App::jobs_stop_rows`], which is what the arrows scroll to. Takes `&mut self` for that
    /// record alone, the sibling of [`App::subagents_lines`].
    pub(crate) fn jobs_lines(&mut self, w: usize) -> Vec<String> {
        let content = self.jobs_view().content(w);
        // **The rows the stops landed on, taken as they went out** — the record the arrows
        // scroll by and a click is tested against.
        self.jobs_stop_rows = content.stop_rows;
        row_strings(&content.lines, self.cfg.palette())
    }
}
