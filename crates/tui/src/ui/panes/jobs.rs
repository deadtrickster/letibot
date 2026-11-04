//! The jobs pane: the background jobs this session started, and a job's output - drawn by
//! `rano::agent::jobs` from the daemon's job table this head keeps.

use crate::app::*;
use crate::ui::render::row_strings;
use rano::agent::jobs::{JobOutput, JobRow, JobsPane};

impl App {
    /// The job-output view: one job's retained window, with its own header because the daemon sent
    /// the offsets rather than a sentence. The tail shows by default and arrows walk back toward the
    /// beginning of the loaded window, so the footer must say how much further a reader can go.
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

    /// How many jobs this session has running, for the composer's top edge.
    ///
    /// `None` when the answer is zero, absent rather than `0 jobs`: the edge is spent on facts true only while they are.
    ///
    /// The count comes from the daemon's table, never from a fold of events: there is no `JobStarted` on the wire, and a head counting would
    /// be wrong for a job already running at attach. The head ASKS at the three moments something changes (`ToolFinished` with a
    /// `backgrounded` outcome, a `JobSettled`, the end of a turn).
    ///
    /// And the redirected half: a job whose output goes to a file has a window that stays EMPTY however long it runs
    /// (`JobEntry::redirect`), so *"3 jobs running"* and *"3 jobs running, one of which you cannot watch"* differ. Said as a count of the
    /// running ones, the set the reader is about to look at:
    ///
    /// ```text
    /// 3 jobs running · 1 to a file
    /// ```
    ///
    /// A settled job's redirect is not counted: the row is about what is running now.
    #[cfg(test)]
    pub(crate) fn jobs_line(&self) -> Option<String> {
        self.jobs_view().running_line()
    }

    /// The jobs pane's facts, in rano's words: the daemon's table as it stands, the fold
    /// of the finished ones, and the cursor.
    ///
    /// `JobEntry::slug` is the daemon's answer to *what did the agent call this job*, stated by the caller and now on the wire. This row
    /// cannot draw it yet: the layout is rano's (`rano::agent::jobs::JobsPane::content`), whose `JobRow` has no field for a name. No
    /// improvisation: a slug in `id` would stop being the handle `ReadJobOutput` takes, and one prefixed onto `command` is structure
    /// encoded as words in a string. The drawing half is one field in rano and one line here:
    ///
    /// ```text
    /// // rano, agent::jobs::JobRow
    /// pub slug: String,
    /// // and in `JobsPane::content`, the row becomes a name beside the id
    /// // ...and in this head's adapter, one line: `slug: j.slug.clone(),`
    /// ```
    ///
    /// Until then `/job` shows a name: `Harness::job_lines` draws it beside the id (`release-build · j65  running  4096 bytes  cargo build`).
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

    /// The daemon's job table as rows: running first, then one folded `finished (N)` row. The rows
    /// come from `job_stops` - the same enumeration the arrows, Enter and the drawn mark read - and
    /// each stop's line is recorded in `jobs_stop_rows`, which is what the arrows scroll to.
    pub(crate) fn jobs_lines(&mut self, w: usize) -> Vec<String> {
        let content = self.jobs_view().content(w);
        // The rows the stops landed on, taken as they went out - the record the arrows
        // scroll by and a click is tested against.
        self.jobs_stop_rows = content.stop_rows;
        row_strings(&content.lines, self.cfg.palette())
    }
}
