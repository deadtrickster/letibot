//! The jobs pane: the background jobs this session started, and a job's output - drawn by
//! `rano::agent::jobs` from the daemon's job table this head keeps.

use crate::app::*;
use crate::ui::render::row_strings;
use rano::agent::jobs::{JobOutput, JobRow, JobsPane};

impl App {
    /// The job-output view: one job's retained window, with its own header because the daemon sent
    /// the offsets rather than a sentence. The tail shows by default and arrows walk back toward the
    /// beginning of the loaded window, so the footer must say how much further a reader can go.
    pub(crate) fn job_out_lines(&mut self, w: usize, room: usize) -> Vec<String> {
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
        // The whole command first, wrapped: the list row shortened it, and a reader deciding
        // whether to kill a job needs to see what it runs. At most half the room.
        let head = wrap_command(&v.command, w.max(8), (room / 2).max(1));
        let (lines, scroll) = view.lines(room.saturating_sub(head.len()));
        // The scroll clamped against what was drawn, stored back: the arrows move it, and
        // only the draw knows how much of the window fits.
        if v.error.is_none() {
            v.scroll = scroll;
        }
        let mut out = head;
        out.extend(row_strings(&lines, p));
        out
    }

    /// The jobs pane's or a job output's `x`: `/job kill ID` for the running job it is on.
    pub(crate) fn kill_selected_job(&mut self, s: Surface) -> Option<Action> {
        if self.session_id.is_empty() {
            return None;
        }
        let id = match s {
            Surface::JobOut => self.job_out.as_ref()?.job.clone(),
            _ => {
                let stops = self.job_stops();
                let JobStop::Job(i) =
                    *stops.get(self.jobs_sel.min(stops.len().saturating_sub(1)))?
                else {
                    return None;
                };
                let row = self.jobs.get(i)?;
                if !row.running {
                    self.say(&format!("`{}` is not running", row.id));
                    return None;
                }
                row.id.clone()
            }
        };
        self.say(&format!("killing job `{id}`"));
        Some(Action::Slash {
            line: format!("job kill {id}"),
        })
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

/// `$ command`, cut into lines of `w` characters, at most `max` of them: the last one ends in `…`
/// when the command is longer than that.
fn wrap_command(command: &str, w: usize, max: usize) -> Vec<String> {
    if command.is_empty() {
        return Vec::new();
    }
    let flat = format!("$ {}", command.replace('\n', " ⏎ "));
    let chars: Vec<char> = flat.chars().collect();
    let mut out: Vec<String> = chars.chunks(w).map(|c| c.iter().collect()).collect();
    if out.len() > max {
        out.truncate(max);
        if let Some(last) = out.last_mut() {
            last.pop();
            last.push('…');
        }
    }
    out
}

#[cfg(test)]
mod wrap_command_tests {
    use super::wrap_command;

    #[test]
    fn a_long_command_wraps_and_a_longer_one_is_cut_with_an_ellipsis() {
        assert_eq!(wrap_command("", 10, 3), Vec::<String>::new());
        assert_eq!(wrap_command("echo hi", 20, 3), vec!["$ echo hi"]);
        assert_eq!(wrap_command("abcdefghij", 6, 3), vec!["$ abcd", "efghij"]);
        let cut = wrap_command("abcdefghijklmnopqrstuvwxyz", 6, 2);
        assert_eq!(cut, vec!["$ abcd", "efghi…"]);
        assert_eq!(wrap_command("a\nb", 20, 3), vec!["$ a ⏎ b"]);
    }
}
