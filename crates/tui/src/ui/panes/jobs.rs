//! **The jobs pane**: the background jobs this session started, and a job's output.

use crate::app::*;
use crate::render::{bytes_human, sgr, trim_to};
use letibot_ui::text::{without_control, without_control_lines};

impl App {
    /// The job-output view: one job's retained window, with its own header because
    /// the daemon sent the **offsets** rather than a sentence. The tail shows by
    /// default; arrows walk back toward the beginning of the loaded window; the
    /// footer says how much further the reader can go.
    pub(crate) fn job_out_lines(&mut self, room: usize) -> Vec<String> {
        let Some(v) = self.job_out.as_mut() else {
            return Vec::new();
        };
        let mut out = vec![colour(
            &self.cfg,
            sgr::BOLD,
            &format!("job output — {}", v.job),
        )];
        // A refusal is not a window: the daemon could not answer, so the pane says
        // what it said rather than drawing an empty log the operator would read as
        // "the job wrote nothing".
        if let Some(err) = v.error.clone() {
            out.push(dim(&self.cfg, "    the daemon refused this read:"));
            out.push(String::new());
            out.extend(err.lines().map(without_control));
            out.push(String::new());
            out.push(dim(&self.cfg, "    Esc back to jobs"));
            out.truncate(room);
            return out;
        }
        // The state and the measurement on one line, because they are one fact:
        // what the job is, and what window of how much is on screen. A `dropped`
        // count is said here rather than in the footer — a window that begins
        // mid-log must not be read as the job's beginning.
        if v.loading && v.state.is_empty() {
            out.push(dim(&self.cfg, "    reading…"));
        } else {
            let mut meta = format!(
                "    {} — bytes {}..{} of {}",
                v.state, v.from, v.to, v.produced
            );
            if v.dropped > 0 {
                meta.push_str(&format!(
                    " ({} earlier byte{} gone off the front)",
                    v.dropped,
                    if v.dropped == 1 { "" } else { "s" }
                ));
            }
            out.push(dim(&self.cfg, &meta));
        }
        out.push(String::new());
        let footer = 1;
        let visible = room.saturating_sub(out.len() + footer).max(1);
        // A job that has written nothing is a different statement from a window of
        // nothing, and the state says which. `running` is the daemon's own word
        // (`JobState::word`), tested literally for the same reason the jobs pane
        // tests `exited 0` literally: the head renders the daemon's vocabulary and
        // keeps no second copy of the enum.
        if v.lines.is_empty() && !v.loading {
            // **§11.6 — an empty window is three cases, and the third was an
            // inversion of the operator's own rule.** A job whose scope could not be
            // joined *never ran*, so its window is empty because there is no process
            // behind it, and the card said
            //
            //     not run (could not join its scope)      ← the header
            //     it wrote nothing at all.                ← and it never started
            //
            // which is R17 read backwards: *a row with no output must not look like a
            // row whose output is empty*. The case is chosen by the **state** — the
            // daemon's `never_ran`, one fact the window's emptiness cannot carry — and
            // not by `lines.is_empty()` alone.
            //
            // `running` is still read off the state word, which is the daemon's own
            // spelling of the state it holds (`JobState::word`); that fact has no field
            // of its own on this frame.
            // **A redirected job is a fourth case, and the third was a lie about it.**
            // Its window is empty BY CONSTRUCTION (R41): the daemon gave the bytes to a file,
            // so `it wrote nothing at all` describes a job that wrote a build log. The
            // operator: *"entering a job never shows me its output - whether it went to file
            // or not"*. The name of the file is all this pane can honestly say, and it is said
            // where the reader is looking.
            let said = if v.never_ran {
                // **The one sentence of this fix that is not derived from the wire**, and
                // it is written in full so the two heads cannot hold two different
                // sentences about one state: §11.6's ruling is *A rules the words; both
                // heads render the same string*, and leticl renders this one verbatim.
                "    it never ran, so there is nothing it could have written.".to_string()
            } else if let Some(path) = v.redirect.as_deref() {
                let path = without_control_lines(path);
                if v.state == "running" {
                    format!("    it is running and writing to {path} — not to this window.")
                } else {
                    format!("    it wrote nothing HERE — its output went to {path}.")
                }
            } else if v.state == "running" {
                "    it is running and has written nothing yet.".to_string()
            } else {
                "    it wrote nothing at all.".to_string()
            };
            out.push(dim(&self.cfg, &said));
        }
        let max_scroll = v.lines.len().saturating_sub(visible);
        v.scroll = v.scroll.min(max_scroll);
        let end = v.lines.len() - v.scroll;
        let start = end.saturating_sub(visible);
        for l in &v.lines[start..end] {
            out.push(without_control(l));
        }
        while out.len() < room.saturating_sub(footer) {
            out.push(String::new());
        }
        let hint = match (v.next, v.back.is_empty()) {
            (Some(_), false) => "arrows scroll · → next page · ← back · Esc to jobs",
            (Some(_), true) => "arrows scroll · → next page · Esc to jobs",
            (None, false) => "arrows scroll · ← back · Esc to jobs",
            (None, true) => "arrows scroll · Esc to jobs",
        };
        out.push(dim(&self.cfg, &format!("    {hint}")));
        out.truncate(room);
        out
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
    pub(crate) fn jobs_line(&self) -> Option<String> {
        let running = self.jobs.iter().filter(|j| j.running).count();
        if running == 0 {
            return None;
        }
        let mut out = format!(
            "{running} job{} running",
            if running == 1 { "" } else { "s" }
        );
        let to_a_file = self
            .jobs
            .iter()
            .filter(|j| j.running && j.redirect.is_some())
            .count();
        if to_a_file > 0 {
            out.push_str(&format!(" · {to_a_file} to a file"));
        }
        Some(out)
    }

    /// **The daemon's job table, as rows** — running first, then one folded `finished (N)` row.
    ///
    /// The operator's own ask: *"jobs panel - same as subagents - show list of running, group
    /// finished"*. The rows come from [`App::job_stops`] — the same enumeration the arrows,
    /// Enter and the drawn `▸` read — and each stop's line is recorded in
    /// [`App::jobs_stop_rows`], which is what the arrows scroll to. Takes `&mut self` for that
    /// record alone, the sibling of [`App::subagents_lines`].
    pub(crate) fn jobs_lines(&mut self, w: usize) -> Vec<String> {
        let mut out = vec![colour(&self.cfg, sgr::BOLD, "background jobs")];
        out.push(String::new());
        if self.jobs.is_empty() {
            out.push(dim(
                &self.cfg,
                "    none. The model backgrounds a command with bash's `background: \
                 true`; ctrl-o moves the running one.",
            ));
        }
        let stops = self.job_stops();
        let cursor = self.jobs_sel.min(stops.len().saturating_sub(1));
        let mut stop_rows: Vec<usize> = Vec::with_capacity(stops.len());
        for (k, stop) in stops.iter().enumerate() {
            stop_rows.push(out.len());
            let picked = k == cursor;
            let i = match *stop {
                // **The `finished` fold, when the cursor is on it.** A group row and not a job:
                // there is nothing to read, and Enter folds or unfolds the settled ones.
                JobStop::Finished => {
                    let n = self.jobs.iter().filter(|j| !j.running).count();
                    let fold = if self.jobs_finished_open {
                        "[-]"
                    } else {
                        "[+]"
                    };
                    let left =
                        format!("{} {} finished ({n})", if picked { "▸" } else { " " }, fold);
                    let left = if picked {
                        colour(&self.cfg, sgr::REVERSE, &left)
                    } else {
                        left
                    };
                    out.push(left);
                    out.push(dim(
                        &self.cfg,
                        if self.jobs_finished_open {
                            "       the ones that have settled · enter folds them away"
                        } else {
                            "       enter shows the ones that have settled"
                        },
                    ));
                    continue;
                }
                JobStop::Job(i) => i,
            };
            let j = &self.jobs[i];
            // Every field here is the daemon's answer. The head decides colour
            // and layout and nothing else — no join against the current turn, no
            // "(command not in this head's window)", because the process table is
            // not a thing this head reconstructs any more.
            let (mark, state_colour) = if j.running {
                ("[~]", sgr::YELLOW)
            } else if j.state.starts_with("exited 0") {
                ("[x]", sgr::GREEN)
            } else {
                ("[!]", sgr::RED)
            };
            out.push(format!(
                "{} {} {} {}",
                if picked { "\u{25b8}" } else { " " },
                colour(&self.cfg, state_colour, mark),
                j.id,
                without_control_lines(&j.command)
            ));
            // **A job that never ran has no duration, and the row must not claim one**
            // (A.2, §11.6). It read
            //
            //     not run (could not join its scope) · 0 B out · ran 0.0s
            //
            // — the state word denying *ran* two fields before the row said it. The
            // byte count stays: it is a measurement that exists (nothing was produced),
            // and the word beside it is what says why.
            let tail = if j.running {
                format!("running · {} out so far", bytes_human(j.produced))
            } else if j.never_ran {
                format!(
                    "{} · {} out",
                    without_control_lines(&j.state),
                    bytes_human(j.produced),
                )
            } else {
                format!(
                    "{} · {} out · ran {}.{:01}s",
                    without_control_lines(&j.state),
                    bytes_human(j.produced),
                    j.elapsed_ms / 1000,
                    (j.elapsed_ms % 1000) / 100,
                )
            };
            out.push(dim(
                &self.cfg,
                &format!("         {} · {}", without_control_lines(&j.how), tail),
            ));
            // **And where the output actually went, when it did not come here** (R41), on a
            // line of its own so the file is readable rather than another clause on a row that
            // is already long. This is the operator's own question — *"im not sure it lets me
            // to see that in the jobs details, when i 'enter' a job"* — answered in the list
            // they are looking at, before they Enter and find an empty window.
            if let Some(path) = &j.redirect {
                out.push(dim(
                    &self.cfg,
                    &format!(
                        "         → {} (its output is there, not in the window)",
                        without_control_lines(path)
                    ),
                ));
            }
        }
        // **The rows the stops landed on, taken as they went out** — the record the arrows
        // scroll by. Assigned at the end because the loop above holds `&self.jobs`.
        self.jobs_stop_rows = stop_rows;
        out.push(String::new());
        out.push(dim(
            &self.cfg,
            "    a job still shows running until the daemon says it settled — between \
             turns, that saying is the daemon's alone.",
        ));
        out.into_iter().map(|l| trim_to(&l, w)).collect()
    }
}
