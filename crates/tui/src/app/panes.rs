//! The subagents', jobs' and queue panes' state: their rows, which row is under a click,
//! folding the tree, and a job's output paged.

use super::*;
use letibot_sessionlog::StoredEnd;
use letibot_sessionlog::registry::short_id;

impl App {
    /// The pane's rows, as one enumeration: the arrows, Enter, `p`, the drawn mark and the scroll all read this and
    /// nothing else. The finished children are folded into one group, since eighteen of them pushed the running ones
    /// off the screen.
    pub(crate) fn subagent_stops(&self) -> Vec<SubStop> {
        self.subagents_view().stops()
    }

    /// The pane row the stop at the cursor was drawn on, read out of [`App::subagents_stop_rows`], which is the
    /// record the pane wrote while drawing and not arithmetic over the lists it drew from.
    pub(crate) fn subagents_row_of(&self) -> usize {
        let at = self
            .subagents_sel
            .min(self.subagents_stop_rows.len().saturating_sub(1));
        self.subagents_stop_rows.get(at).copied().unwrap_or(0)
    }

    /// The subagent rows: this session's children as the daemon's list has them. The pane and the composer's count
    /// are the same list, and a second source is how the two came to disagree.
    pub(crate) fn fold_subagents(&mut self) {
        let known: Vec<SubagentState> = std::mem::take(&mut self.subagents);
        let mut rows: Vec<SubagentState> = Vec::with_capacity(known.len().max(4));
        // The session whose children these are: the one this head is in, or the one whose pane it is looking at
        // while a switch is in flight. See [`App::subagent_parent`], which is the whole rule.
        let parent = self.subagent_parent().to_string();
        for b in &self.sessions {
            if b.parent_session_id.as_deref() != Some(parent.as_str()) {
                continue;
            }
            // The merge queue's reviewer, rebuilt from the list - which carries no role, so it is
            // known by the brief its title was cut from. See `on_child_event`.
            if b.title
                .starts_with(letibot_sessionlog::GATEKEEPER_TITLE_PREFIX)
            {
                continue;
            }
            match known.iter().find(|k| k.session_id == b.session_id) {
                // Watched: the event's own row, which knows more than the list does about
                // everything except whether a turn is generating in it right now.
                Some(k) => {
                    let mut k = k.clone();
                    // The list's measurement of NOW, kept beside the lifecycle word rather
                    // than written over it. See [`SubagentState::generating`].
                    k.generating = b.status.running;
                    k.state = if b.status.running {
                        // A positive measurement of life, and the one direction the list may move a row: a turn is
                        // generating in this child now, so it is working, and this overrides a `done` on the row.
                        "running".into()
                    } else if k.state == "running" && k.answer.is_some() {
                        // `false` may retire a row that has already completed: the finish this row is still running on
                        // happened before the list was cut, and the child holds the answer the daemon published.
                        String::new()
                    } else {
                        // And it may not touch any other row: `false` is *no turn is generating in this child this
                        // instant*, which is exactly a child parked on its own background job.
                        k.state
                    };
                    // The daemon's own stamp wins over the event's, when the list carries one:
                    // that is the session's creation time, and an event's `ts` is only when this
                    // head heard about the child.
                    if b.created_ms > 0 {
                        k.spawned_ms = b.created_ms;
                    }
                    // The list's name for a child the event did not name - a spawn this
                    // head heard from a daemon that published no slug, then listed by one that
                    // records it.
                    if k.slug.is_empty() {
                        k.slug = b.slug.clone();
                    }
                    rows.push(k);
                }
                None => rows.push(SubagentState {
                    session_id: b.session_id.clone(),
                    // Only what the daemon actually said: `running` is the list's own *a turn is generating now*, and
                    // past that the stored conversation's last row decides.
                    state: match (&b.stored_end, b.status.running, b.live) {
                        (_, true, _) => "running".into(),
                        (Some(StoredEnd::Answered { .. }), false, _) => "done".into(),
                        (Some(StoredEnd::MidTurn), false, false) => "stopped mid-turn".into(),
                        _ => String::new(),
                    },
                    generating: b.status.running,
                    prompt: String::new(),
                    // The child's name, or the id the daemon shows for one it has not named: a row whose words are
                    // all empty is a row the reader cannot tell from any other.
                    task: if b.title.is_empty() {
                        short_id(&b.session_id)
                    } else {
                        b.title.clone()
                    },
                    role: String::new(),
                    model: b.status.model.clone(),
                    answer: match &b.stored_end {
                        Some(StoredEnd::Answered { first_line }) if !b.status.running => {
                            Some(first_line.clone())
                        }
                        _ => None,
                    },
                    spawned_ms: b.created_ms,
                    slug: b.slug.clone(),
                }),
            }
        }
        for k in known {
            if !rows.iter().any(|r| r.session_id == k.session_id) {
                rows.push(k);
            }
        }
        // Newest first, which is the operator's ask: the rows above are built in the daemon's list order, which is
        // creation order, and the most recent agents have to be on top.
        rows.sort_by_key(|r| std::cmp::Reverse(r.spawned_ms));
        self.subagents = rows;
        // The child this head climbed up out of, by id, once the rebuild has happened: a stop index taken before it
        // would point at whatever the new list has there. The fold is the person's, so the walk does not open one.
        let up_from = self.up_from.clone();
        if let Some(from) = up_from
            && let Some(i) = self.subagents.iter().position(|r| r.session_id == from)
        {
            let stops = self.subagent_stops();
            self.subagents_sel = stops
                .iter()
                .position(|s| matches!(s, SubStop::Agent(j) if *j == i))
                // Folded away: the group row is the row that stands for it now.
                .or_else(|| stops.iter().position(|s| matches!(s, SubStop::Finished)))
                .unwrap_or(0);
            self.up_from = None;
        }
        self.subagents_sel = self
            .subagents_sel
            .min(self.subagent_stops().len().saturating_sub(1));
    }

    /// The jobs pane's rows, as one enumeration: running first, then one folded `finished (N)` row. Same shape as the
    /// subagents pane, and for the same reason.
    pub(crate) fn job_stops(&self) -> Vec<JobStop> {
        self.jobs_view().stops()
    }

    /// The pane row the job stop at the cursor was DRAWN on, read out of
    /// [`App::jobs_stop_rows`] - the record the pane wrote while drawing, never arithmetic over
    /// the table it drew from. The sibling of [`App::subagents_row_of`].
    pub(crate) fn jobs_row_of(&self) -> usize {
        let at = self
            .jobs_sel
            .min(self.jobs_stop_rows.len().saturating_sub(1));
        self.jobs_stop_rows.get(at).copied().unwrap_or(0)
    }

    /// The pane row the entry at the cursor was drawn on, read out of [`App::queue_stop_rows`], which is the record
    /// the pane wrote while drawing and never arithmetic over the queue.
    pub(crate) fn queue_row_of(&self) -> usize {
        let at = self
            .queue_sel
            .min(self.queue_stop_rows.len().saturating_sub(1));
        self.queue_stop_rows.get(at).copied().unwrap_or(0)
    }

    /// The visible slice of a pane, and the two numbers the scroll keys need. Clamped here rather than at the
    /// keypress: the handler does not know how tall the terminal is or how many rows the pane has.
    pub(crate) fn pane_window(&mut self, rows: Vec<String>, room: usize) -> Vec<String> {
        self.pane_len = rows.len();
        self.pane_room = room;
        // The last screenful is the furthest anything scrolls: past that is
        // blank rows, which is not a place to be.
        let max = rows.len().saturating_sub(room);
        self.pane_scroll = self.pane_scroll.min(max);
        rows.into_iter().skip(self.pane_scroll).take(room).collect()
    }

    /// Keep the cursor on screen after an arrow moved it: `row` is the cursor's index among the pane's rows, and an
    /// arrow that walks it out of the window otherwise leaves a selection nobody can see.
    pub(crate) fn scroll_into_view(&mut self, row: usize) {
        if self.pane_room == 0 {
            return;
        }
        if row < self.pane_scroll {
            self.pane_scroll = row;
        } else if row >= self.pane_scroll + self.pane_room {
            self.pane_scroll = row + 1 - self.pane_room;
        }
    }

    /// The job-output view's paging: `forward` asks for the page after the one on screen, or re-reads the last page
    /// when the end is already here, since a running job appends and that is how you see what it wrote since.
    pub(crate) fn job_out_page(&mut self, forward: bool) -> Option<Action> {
        let v = self.job_out.as_mut()?;
        let offset = if forward {
            let at = v.from;
            if v.next.is_some() {
                v.back.push(at);
            }
            v.next.unwrap_or(at)
        } else {
            v.back.pop()?
        };
        v.loading = true;
        v.asked_ms = self.now_ms;
        Some(Action::ReadJobOutput {
            job: v.job.clone(),
            offset,
        })
    }

    /// A running job's open window follows what it writes, so a `docker pull` shows its bytes as they arrive:
    /// the pane is re-read while the job runs, not once at Enter.
    pub(crate) fn poll_job_out(&mut self) {
        if self.top_surface() != Some(Surface::JobOut) {
            return;
        }
        let now = self.now_ms;
        let Some(v) = self.job_out.as_mut() else {
            return;
        };
        if v.loading
            || v.polling
            || v.error.is_some()
            || v.state != "running"
            || v.next.is_some()
            || v.scroll != 0
            || now.saturating_sub(v.asked_ms) < JOB_OUT_FOLLOW_MS
        {
            return;
        }
        v.polling = true;
        v.asked_ms = now;
        self.queued.push(Action::ReadJobOutput {
            job: v.job.clone(),
            offset: v.from,
        });
    }
}

/// One tool call as the head watches it happen. It keeps the three things a person looking at a running call wants
/// and a tuple did not: how long it has been going, what it last said, and how much came out.
#[derive(Debug, Clone)]
pub(crate) struct SubagentState {
    pub(crate) session_id: String,
    /// `opening`, `running`, `done`, `failed`, or empty, which is a row rebuilt from the daemon's session list for a
    /// child this head never watched: that list says whether a turn is generating and nothing about how it settled.
    pub(crate) state: String,
    /// A turn is generating in this child at this instant: the daemon's session list's own word, and a measurement of
    /// now rather than of a life. It is the one fact that may move a row up the list.
    pub(crate) generating: bool,
    /// The legacy field and the pre-`task` fallback: the subtask's first line while opening, and the child's answer's
    /// first line once it has finished. A new row reads [`SubagentState::task`]; this is here for an older daemon.
    pub(crate) prompt: String,
    pub(crate) role: String,
    /// The subtask in full, from the event's `task`. Empty against a daemon that
    /// predates the field, and the pane then falls back to `prompt`.
    pub(crate) task: String,
    /// The model this child runs on, from the event's `model` - `local`, or
    /// `PROVIDER/MODEL`. Empty when the child inherited its parent's model, which is the
    /// default: the pane then draws no model clause rather than claiming one.
    pub(crate) model: String,
    /// The child's answer's first line, `Some` only once it has finished - the
    /// subtitle, kept apart from the row so a completion cannot be mistaken for the
    /// question.
    pub(crate) answer: Option<String>,
    /// When this child was spawned, for the pane's order, which is newest first. Two sources and the daemon's wins:
    /// the session's own `created_ms` over the event's stamp, since the list is the fresher of the two.
    pub(crate) spawned_ms: u64,
    /// The child's name - its slug (`fix-login`), from the event's `slug` or the daemon's
    /// list. Empty for a child nobody named (a daemon from before names), which is drawn by its
    /// short id exactly as before. See [`SubagentState::name`].
    pub(crate) slug: String,
}

impl SubagentState {
    /// How a person is shown this child: `fix-login · …14688401` - its name and the short id,
    /// the pane's spelling of the label the model reads (`fix-login · s-…`) - or the short id
    /// alone for a child nobody named.
    pub(crate) fn name(&self) -> String {
        child_name(&self.slug, &self.session_id)
    }

    /// Whether this child is done: the one thing the pane's two groups are made of, and the count above the composer
    /// too. Anything that is not `running` or `opening` is finished, a rebuilt row included.
    pub(crate) fn is_finished(&self) -> bool {
        !matches!(self.state.as_str(), "running" | "opening")
    }
}

/// A child as a person reads it: `slug · …tail`, or the short id alone when nobody named it.
/// One function, because the pane, its output view, the todos' `waits on …`, the attention line
/// and the header all name a child and must not spell it four ways.
pub(crate) fn child_name(slug: &str, session_id: &str) -> String {
    let short = letibot_sessionlog::registry::short_id(session_id);
    if slug.trim().is_empty() {
        short
    } else {
        format!(
            "{} · {short}",
            slug.split_whitespace().collect::<Vec<_>>().join(" ")
        )
    }
}

/// One row of the subagent pane: the ONE enumeration the arrows, Enter, `p`, the drawn mark and the scroll read. A
/// pane whose cursor comes from one list and whose rows come from another is the defect the todos pane records.
pub(crate) use rano::agent::subagents::SubStop;

/// One row of the jobs pane: the ONE enumeration the arrows, Enter, the drawn mark and the scroll read, exactly as
/// [`SubStop`] is for the subagents pane. See [`App::job_stops`].
pub(crate) use rano::agent::jobs::JobStop;

#[derive(Debug, Clone)]
pub(crate) struct SubOut {
    pub(crate) session_id: String,
    /// One line per rendered row - and which renderer produced them is [`SubOut::degraded`]'s
    /// business: the session's own rows go through `item_lines`, the same one every row of the
    /// transcript uses, and the event-ring fallback goes through `subagent_out_lines`.
    pub(crate) lines: Vec<String>,
    /// The daemon answered with the event ring rather than the session's rows: `Peeked::snapshot` is `None` when the
    /// daemon predates the field or the peek did not ask for rows, so the fallback is necessary.
    pub(crate) degraded: bool,
    /// Lines hidden off the bottom. Zero is "following the tail"; the pane draw
    /// clamps it, because only the draw knows the visible height.
    pub(crate) scroll: usize,
    /// Where the whole view was spilled, when it was written.
    pub(crate) spill: Option<String>,
    /// Events that fell off the daemon's scrollback before this read, the same disclosure a `Hello` makes: a snapshot
    /// is bounded by the daemon's view, so a read that came up short says so rather than looking complete.
    pub(crate) dropped: u64,
}

/// How often an open window over a running job re-reads its tail. See [`App::poll_job_out`].
pub(crate) const JOB_OUT_FOLLOW_MS: u64 = 1_000;

/// The output view the jobs pane's Enter opens: one job's retained output, as the daemon measured it, with the bytes
/// and the offsets beside them, since the pane draws its own header and paging.
#[derive(Debug, Clone)]
pub(crate) struct JobOut {
    pub(crate) job: String,
    /// The command, whole: the list row shortens it to fit.
    pub(crate) command: String,
    /// The daemon's word for where the job is - `running`, `exited 0`, … Empty
    /// until the first answer arrives.
    pub(crate) state: String,
    /// Whether anything was ever executed for this job. The daemon's answer and not the head's inference: an empty
    /// window is the shape of *ran and wrote nothing* and of *never ran*.
    pub(crate) never_ran: bool,
    /// Where this job's output actually went, when it did not come here: the file the job's `redirect` named, taken
    /// from the row Enter was pressed on. A redirected job's window is empty by construction.
    pub(crate) redirect: Option<String>,
    /// The offsets of the window actually loaded: `from..to` of `produced`.
    pub(crate) from: u64,
    pub(crate) to: u64,
    pub(crate) produced: u64,
    /// Bytes that fell off the front of the ring before this window. Disclosed
    /// because a window that starts mid-log is otherwise read as the job's start.
    pub(crate) dropped: u64,
    /// The window, already split into lines by the daemon so two heads cannot
    /// disagree about where a line ends.
    pub(crate) lines: Vec<String>,
    /// Where the daemon says the next page starts, or `None` when the end is here.
    pub(crate) next: Option<u64>,
    /// Offsets already loaded, newest last: `←` walks back the way `→` came. A
    /// stack rather than `from - page` arithmetic, because the page size is the
    /// daemon's choice and recomputing it here would be a second copy of it.
    pub(crate) back: Vec<u64>,
    /// Lines hidden off the bottom of the loaded window. Zero follows the tail;
    /// the draw clamps it, because only the draw knows the visible height.
    pub(crate) scroll: usize,
    /// True from the request until its answer: the overlay says so rather than
    /// showing an empty window it cannot yet fill.
    pub(crate) loading: bool,
    /// A background re-read of a running job's tail is in flight, which is separate from `loading`: the window on
    /// screen is still true while it is out, and `loading` would blank the `nothing yet` line.
    pub(crate) polling: bool,
    /// When this head last asked for this window or was last answered (its clock, ms) - the
    /// follow's interval runs from it.
    pub(crate) asked_ms: u64,
    /// The daemon's refusal, when the read could not be answered - a job that fell
    /// out of the host's table between the listing and Enter. Shown in place of the
    /// window so the pane does not sit at `reading…` forever.
    pub(crate) error: Option<String>,
}

/// The whole view, spilled: the pane caps like a terminal and the file does not, one name per subagent so the path
/// is stable enough to open twice.
pub(crate) fn head_runtime_dir() -> std::path::PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(d) => std::path::PathBuf::from(d).join("letibot"),
        None => std::env::temp_dir().join(format!("letibot-{}", unsafe { libc::getuid() })),
    }
}

pub(crate) fn spill_sub_out(session_id: &str, lines: &[String]) -> Option<String> {
    spill_sub_out_under(&head_runtime_dir(), session_id, lines)
}

pub(crate) fn spill_sub_out_under(
    dir: &std::path::Path,
    session_id: &str,
    lines: &[String],
) -> Option<String> {
    if std::fs::create_dir_all(dir).is_err() {
        return None;
    }
    let path = dir.join(format!("subagent-{session_id}.log"));
    let mut body = String::new();
    for l in lines {
        body.push_str(l);
        body.push('\n');
    }
    std::fs::write(&path, body)
        .ok()
        .map(|_| path.display().to_string())
}
