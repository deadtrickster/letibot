//! **A background job's output, as a window**: the numbers a pane needs beside the bytes, the fate a
//! row's condition turns on, and what a job that is no longer in this daemon's table can still say.
//!
//! Every decision here is apart from the harness that feeds it, so the wording and the arithmetic are
//! asserted without a backend, a registry, a gate or a store.

use super::*;

/// A window onto a background job's output, with the numbers a pane needs for its own header and
/// paging. `job_output` returns prose for a head that prints what it is handed; this is the same read
/// with the offsets beside the text, so the pane need not parse a sentence it did not write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobWindow {
    /// The offset this window actually starts at - the request is clamped to what
    /// survives, not refused.
    pub from: u64,
    /// One past the window's last byte.
    pub to: u64,
    /// Everything the job has written, ever. The denominator.
    pub produced: u64,
    /// Bytes that fell off the front of the ring before this window: what a head
    /// must disclose so a window's start is not read as the job's start.
    pub dropped: u64,
    /// The job's own word - `exited 0`, `running`, `killed by …`.
    pub state: String,
    /// **Whether anything was ever executed for this job** - `true` only for the one
    /// state where the wrapper could not join the cgroup, so a head with an empty
    /// window can say *never ran* rather than *wrote nothing* (A.2, §11.6).
    pub never_ran: bool,
    /// The window, split into lines **here**, so two heads cannot disagree about
    /// where a line ends.
    pub lines: Vec<String>,
    /// Where to ask next when there is more that is still readable, and `None`
    /// when the end is here. Decided here rather than by the head computing `to`,
    /// because only the daemon knows how much of the ring survives.
    pub next: Option<u64>,
}

/// What the world says about one handle: three answers, because a reader acts differently on each.
/// `Unknown` MEETS the condition, since a job that is not running has ended and a handle this daemon
/// never heard of is what that looks like after a restart. It does not say what became of the job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobFate {
    /// Still running **here**. The condition is not met.
    Running,
    /// It ended, and this is its own word - `exited 0`, `killed by job_kill`, `signalled 15`.
    Ended { state: String },
    /// **Not a job this session knows**: it ended before this daemon came back, or the handle is
    /// wrong. The condition is met and the result is not here.
    Unknown,
}

/// One due row, as the sentence the model is handed: pure, so the wording is asserted without a
/// session. `handle` is passed rather than carried in the fate, because a fate is a fact about a job
/// and the handle is how a row names it.
pub(crate) fn fate_line(handle: &str, fate: &JobFate) -> String {
    match fate {
        // Never printed - a running job is not due - and answerable anyway, because a function
        // that panics on the arm nobody reaches is a function whose contract is a rumour.
        JobFate::Running => format!("`{handle}` is still running"),
        JobFate::Ended { state } => format!("`{handle}` {state}"),
        JobFate::Unknown => format!(
            "`{handle}`: unknown job (lost in a restart, or a wrong handle); its result is \
             unavailable."
        ),
    }
}

/// **The rows a condition just met, as the note the model is handed**: the head says what fired and
/// never how many, because a count is a second statement that can disagree with the lines under it.
pub(crate) fn due_notice(due: &[(String, String, JobFate)]) -> String {
    let lines: Vec<String> = due
        .iter()
        .map(|(text, handle, fate)| format!("  - {text} — {}", fate_line(handle, fate)))
        .collect();
    format!("[todo] Due (its condition is met):\n{}", lines.join("\n"))
}

/// The decision `Harness::job_fate` makes, as a function of what its two looks returned: pulled out so
/// the choice a conditioned row turns on is asserted without a backend, a registry, a gate and a store.
/// The ORDER is the whole of the honesty, since only the host can say a job runs NOW.
pub(crate) fn fate_of(
    live: Option<letibot_tools::exec::JobState>,
    record: Option<&JobRecord>,
) -> JobFate {
    if let Some(state) = live {
        return if state.is_running() {
            JobFate::Running
        } else {
            JobFate::Ended {
                state: state.word(),
            }
        };
    }
    match record {
        Some(row) => JobFate::Ended {
            state: row.state.clone(),
        },
        None => JobFate::Unknown,
    }
}

/// Which of a board's rows the world now meets: the filter and the pairing, apart from the harness. Four
/// skips, each a fact: no condition, a running job, a row the check may not speak about, and one already
/// done. `jobs` is handed in so this stays a function of its arguments.
pub(crate) fn due_rows(
    rows: &[TodoItem],
    children: &letibot_tools::builtins::todo::ChildSessions,
    jobs: &letibot_tools::builtins::todo::RunningJobs,
    fate: impl Fn(&str) -> JobFate,
) -> Vec<(String, String, JobFate)> {
    // A firing is a delivery, not a nag: the grace after an ending keeps the CHECK from asking about a
    // row the model has not digested the news for, and this IS that news, so it is read without the grace.
    let jobs = &jobs.clone().without_grace();
    let mut out = Vec::new();
    for row in rows {
        if !crate::sessions::the_check_may_ask_about(row, children, jobs) {
            continue;
        }
        let Some(letibot_tokencore::store::TodoCondition::Job { handle }) = &row.when else {
            continue;
        };
        let fate = fate(handle);
        if matches!(fate, JobFate::Running) {
            continue;
        }
        out.push((row.content.clone(), handle.clone(), fate));
    }
    out
}

/// What a recorded job's window says, apart from the harness that found the row: a free function, so the
/// words can be asserted without a store, a vocabulary or a session.
pub(crate) fn recorded_job_lines(row: &JobRecord) -> Vec<String> {
    let mut lines = vec![format!(
        "this job is not in this daemon's table: {} after {}, {} bytes out. Its bytes were \
         not kept — the ring died with the process that held them.",
        row.state,
        human_secs(row.elapsed_ms),
        row.produced,
    )];
    if let Some(path) = &row.redirect {
        lines.push(format!(
            "its output went to {path} — that is the file to read."
        ));
    }
    lines
}

impl Harness {
    /// The window for a handle this daemon never ran, or the refusal when the store has never heard of it
    /// either. It says *not in this table* rather than *it ran*, because the row carries a state word and
    /// this does not parse it. The bytes are gone; the row's `redirect` says where they went instead.
    fn recorded_job_window(&self, job: &str) -> Result<JobWindow, String> {
        let Some(row) = self.recorded_job(job) else {
            return Err(format!(
                "no job `{job}` here; `/job` with no argument lists them"
            ));
        };
        let lines = recorded_job_lines(&row);
        Ok(JobWindow {
            from: 0,
            to: 0,
            produced: row.produced,
            // Everything the job wrote is outside this window, which is what `dropped` says.
            dropped: row.produced,
            state: row.state.clone(),
            // Not known here: the row records a state WORD and this function does not parse it.
            never_ran: false,
            lines,
            next: None,
        })
    }

    /// **The stored row for one handle**, or `None` - the one read the output window and the
    /// condition evaluator share, so the two cannot look in different places.
    fn recorded_job(&self, handle: &str) -> Option<JobRecord> {
        let store = self.store.as_ref()?;
        store
            .jobs(&self.cfg.session_id)
            .ok()?
            .into_iter()
            .find(|r| r.handle == handle)
    }

    /// What the world says about one handle. The live table is asked FIRST, which is the whole of the
    /// honesty: only the process host can say a job runs now, and a stored `running` row is a fact about
    /// then. The record is the second look, because it is what survives a restart.
    pub fn job_fate(&self, handle: &str) -> JobFate {
        // **Two looks, and [`fate_of`] is the decision between them** - the live table first,
        // because only the process host can say a job runs *now*, and the record second, because
        // it is what survives a restart.
        let live = self
            .runtime
            .backend
            .processes()
            .and_then(|host| host.job(&letibot_tools::exec::JobId(handle.to_string())))
            .map(|view| view.state);
        fate_of(live, self.recorded_job(handle).as_ref())
    }

    /// The rows whose condition the world now meets: the row's text, the handle and the fate, so the
    /// caller can report and consume them in one breath. The fate is carried and not collapsed into a
    /// boolean, because a handle that ended here can be read and one never heard of cannot.
    pub fn due_todo_rows(&self) -> Vec<(String, String, JobFate)> {
        // **The rows and both live facts, taken together** - the same reading `nag_notice` takes, so
        // the firing and the nag cannot disagree about whether a row is one the check may speak
        // about: a row waiting on a young child or on a running job is not due by EITHER door.
        let rows = self.todos.snapshot();
        let children = self.child_sessions();
        let jobs = self.running_jobs(&rows);
        due_rows(&rows, &children, &jobs, |handle| self.job_fate(handle))
    }

    /// The same read as [`Self::job_output`], as a window rather than a page of
    /// prose - see [`JobWindow`] for why both exist.
    pub fn job_output_window(
        &self,
        job: &str,
        offset: u64,
        limit: usize,
    ) -> Result<JobWindow, String> {
        let Some(host) = self.runtime.backend.processes() else {
            return Err("this session has no process host, so it has no jobs".into());
        };
        let jid = letibot_tools::exec::JobId(job.to_string());
        let Some(view) = host.job(&jid) else {
            // A job this daemon never ran, but whose ending it may have written down: the process table is
            // memory, so after a restart the host answers nothing about a job the store has a row for. The
            // host is still asked first, since only it can say a job runs now.
            return self.recorded_job_window(job);
        };
        let slice = host
            .output(&jid, offset, limit)
            .map_err(|e| e.to_string())?;
        // The decision is `job_window_for`'s, apart from the two reads, for the reason `fate_of` is apart
        // from `job_fate`: this is what a pane turns on, and reaching it through a harness needs a backend,
        // a registry, a gate and a store, so nothing held it to a real job.
        Ok(job_window_for(
            &view.command,
            &view.state,
            &slice,
            offset,
            limit,
        ))
    }
}

/// One live read of a job's output, as the event that answers it: the half of `job_output_window` that
/// needs only the process host, so it can run on a head's connection thread while the worker runs a turn.
/// `None` when the host does not hold the job, whose answer is the store's record.
pub(crate) fn live_job_output(
    host: &dyn letibot_tools::exec::ProcessHost,
    job: &str,
    offset: u64,
    limit: usize,
) -> Option<SessionEvent> {
    let jid = letibot_tools::exec::JobId(job.to_string());
    let view = host.job(&jid)?;
    Some(match host.output(&jid, offset, limit) {
        Ok(slice) => {
            job_window_for(&view.command, &view.state, &slice, offset, limit).into_event(job)
        }
        Err(e) => SessionEvent::Warning {
            code: "job_output_refused".into(),
            detail: e.to_string(),
            compaction: None,
        },
    })
}

impl JobWindow {
    /// The event a head's pane is filled from: the window with the job it belongs to.
    pub fn into_event(self, job: &str) -> SessionEvent {
        SessionEvent::JobOutput {
            job: job.to_string(),
            from: self.from,
            to: self.to,
            produced: self.produced,
            dropped: self.dropped,
            state: self.state,
            never_ran: self.never_ran,
            lines: self.lines,
            next: self.next,
        }
    }
}

/// The window one read of a job's output answers with: the decision half of
/// [`Harness::job_output_window`], which does the two reads and hands everything to this.
///
/// A job whose command redirects its output to a file answers with the FILE, and the daemon does the
/// reading, because a head has no business reading the box's filesystem. The redirect is enough on its
/// own: reading from the file only when the capture was completely empty took a command as either
/// redirecting or writing, and a compound command does both.
///
/// ```text
/// handle  state      produced  redirect
/// j87     exited 0   2426      /tmp/tpcc-trace2.log   ← the file was NOT read
/// j82     killed     13        /tmp/tpcc-trace.log    ← same shape
/// j70     killed      0        /tmp/tpcc-trace-server.log ← this one worked
/// ```
///
/// So when a command names a file, that file is the window whatever the capture holds, and the
/// capture's bytes are not drawn. They are not lost silently either: the read says how many are
/// outside the window and where the window is. Splicing the two coordinate spaces together, or
/// switching to the file only at settlement, would make `produced`/`from`/`dropped` two stories
/// about one source.
///
/// `offset == 0` is the TAIL, the last `limit` bytes, because that is what an output pane is for.
/// Any other offset is a forward window from there, so a reader can walk to the end of a long log.
pub(crate) fn job_window_for(
    command: &str,
    state: &letibot_tools::exec::JobState,
    slice: &letibot_tools::exec::OutputSlice,
    offset: u64,
    limit: usize,
) -> JobWindow {
    if let Some(path) = letibot_tools::builtins::output_redirect_path(command) {
        return match tail_file(&path, offset, limit) {
            Some(w) => {
                let mut lines = Vec::new();
                // The preamble, counted rather than hidden: the capture may hold bytes the redirect never
                // took, and this window is over the file. The disclosure belongs on the fresh read, since
                // repeating the count on every paged read would be noise.
                if offset == 0 && slice.produced > 0 {
                    lines.push(format!(
                        "{} bytes this job wrote outside the redirect are in its capture, \
                         not in this window — this window is `{path}`",
                        slice.produced
                    ));
                }
                lines.extend(progress_lines(&w.text));
                JobWindow {
                    from: w.from,
                    to: w.to,
                    produced: w.produced,
                    // Everything before the window was not shown: the pane says how much.
                    dropped: w.from,
                    state: state.word(),
                    never_ran: state.never_ran(),
                    lines,
                    next: (w.to < w.produced).then_some(w.to),
                }
            }
            // **The file is not there yet**, or it cannot be read. The sentence that names
            // it is still the honest answer, and it names the command that would read it.
            None => JobWindow {
                from: 0,
                to: 0,
                produced: 0,
                dropped: 0,
                state: state.word(),
                never_ran: state.never_ran(),
                lines: vec![format!(
                    "this job's output goes to `{path}` — the file is not there yet, or \
                     could not be read. `tail -n 50 {path}`"
                )],
                next: None,
            },
        };
    }
    JobWindow {
        from: slice.from,
        to: slice.to,
        produced: slice.produced,
        dropped: slice.dropped,
        state: state.word(),
        never_ran: state.never_ran(),
        lines: if slice.produced == 0 {
            // **Empty, and the redirected case cannot reach here** - a job whose command
            // redirects its output was answered from the FILE above.
            Vec::new()
        } else {
            progress_lines(&slice.text())
        },
        next: next_job_offset(slice),
    }
}

/// The tail of a file, as the window a pane draws: the file half of a redirected job. `offset == 0` is
/// the tail, any other offset a forward window, and `None` when the file cannot be read so the caller
/// falls back to the sentence naming it. Bytes, lossily decoded, because a log is not text.
pub(crate) struct FileWindow {
    pub(crate) from: u64,
    pub(crate) to: u64,
    /// The file's whole length, so the pane's `of N` is the file and not the window.
    pub(crate) produced: u64,
    pub(crate) text: String,
}

pub(crate) fn tail_file(path: &str, offset: u64, limit: usize) -> Option<FileWindow> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let produced = f.metadata().ok()?.len();
    let limit = limit as u64;
    let from = if offset == 0 {
        produced.saturating_sub(limit)
    } else {
        offset.min(produced)
    };
    let end = if offset == 0 {
        produced
    } else {
        produced.min(from.saturating_add(limit))
    };
    f.seek(SeekFrom::Start(from)).ok()?;
    // **Read what is actually there**, not the length the metadata promised: a running job
    // appends while this runs, and a file that grew between the two must not fail the read.
    let mut buf = Vec::new();
    f.take(end.saturating_sub(from))
        .read_to_end(&mut buf)
        .ok()?;
    Some(FileWindow {
        from,
        to: from + buf.len() as u64,
        produced,
        text: String::from_utf8_lossy(&buf).into_owned(),
    })
}

/// The captured bytes, as lines a person can read, and a progress bar is one line: `\r` returns to
/// column 0 and what follows OVERWRITES it, so the line is each segment written over the last. The split
/// is the daemon's, so two heads cannot disagree about where a line ends.
pub(crate) fn progress_lines(text: &str) -> Vec<String> {
    text.split('\n')
        .map(|line| {
            if !line.contains('\r') {
                return line.trim_end_matches('\r').to_string();
            }
            // Written over the current row, segment by segment, from column 0.
            let mut row: Vec<char> = Vec::new();
            for (i, seg) in line.split('\r').enumerate() {
                let chars: Vec<char> = seg.chars().collect();
                if i == 0 {
                    row = chars;
                } else {
                    for (col, c) in chars.into_iter().enumerate() {
                        if col < row.len() {
                            row[col] = c;
                        } else {
                            row.push(c);
                        }
                    }
                }
            }
            row.into_iter().collect()
        })
        .collect()
}

/// Where a further read of a job's output would start, or `None` when the window has reached the end of
/// what the ring holds: `produced == dropped + retained`, so `to < produced` says there is more. A named
/// free function because it is the one piece of arithmetic in the window.
pub(crate) fn next_job_offset(slice: &letibot_tools::exec::OutputSlice) -> Option<u64> {
    (slice.to < slice.produced).then_some(slice.to)
}

/// What a job killed with its subagent's VM reads as, on its row.
pub(crate) const VM_LOST: &str = "lost with its VM";

/// **The background jobs a VM child left running, as its parent is told they died** (TODO.md
/// R92.1) - `None` when there were none. Pure, so the wording is asserted without a VM.
pub(crate) fn vm_jobs_lost_line(lost: &[letibot_tools::exec::JobView]) -> Option<String> {
    if lost.is_empty() {
        return None;
    }
    let jobs = lost
        .iter()
        .map(|j| {
            let cmd: String = j
                .command
                .lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(80)
                .collect();
            format!("`{}` (`{cmd}`)", j.label())
        })
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!(
        "[subagent jobs] {} background job(s) were still running in the subagent's VM when it \
         was closed after its turn, and died with it: {jobs}. Nothing reported their ending and \
         their output is gone — if the work matters, run it again yourself.",
        lost.len()
    ))
}
