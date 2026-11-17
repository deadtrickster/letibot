//! The plan-check board: the operator's half of the todo list, the sentence the idle clock counts,
//! and the nudge it delivers as a turn of its own.
use super::*;
use letibot_plugin::notice::Facts;
use letibot_tools::exec::procs;
use std::time::Duration;

/// One plan-check as it stands: the sentence, the rows it names, and the interval a `nag_` layer
/// asked the clock for. The interval is unclamped here - the bounds are the clock's.
struct TheNag {
    text: String,
    names: Vec<String>,
    next: Option<Duration>,
}

/// What the wait sweep freed: how many waited-on things, and whether a freed row can start now, so
/// the note rides a turn and the caller must ring for one.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FreedWaits {
    pub(crate) waits: usize,
    pub(crate) wake: bool,
}

impl Harness {
    /// The operator's half of the board: replaces their rows and leaves the model's alone, so anything reading the
    /// board afterwards sees both. `moved` is a STATE and never a membership, since a name the operator can silence is
    /// one they can never add, drop or re-word, and a name that resolves to nothing is the daemon's log.
    pub fn set_operator_todos(
        &mut self,
        items: Vec<letibot_tokencore::store::TodoItem>,
        moved: &[(String, letibot_tokencore::store::TodoStatus)],
    ) {
        // Whether this write ADDED a row - the operator's own act, which earns them a check sooner
        // than the idle window. Read before the board moves, and by the words, since a row is its
        // words on this board.
        let before: Vec<String> = self
            .todos
            .snapshot()
            .into_iter()
            .filter(|r| r.by == letibot_tokencore::store::TodoBy::Operator)
            .map(|r| r.content)
            .collect();
        self.todos.set_operator(items);
        // And the state moves that are not the head's to make: the board resolves the words against the model's half
        // and sets the status. A refusal is the daemon's log rather than the operator's silence, since a model that
        // rewrote its plan makes a name resolve to nothing and the union this write publishes is the correction.
        if !moved.is_empty()
            && let Err(why) = self.todos.set_states(moved)
        {
            eprintln!("  todos: a state move did not land: {why}");
            // The head that sent it is told too: the union this write publishes is the correction,
            // and this is the sentence that says why the row did not move.
            self.import_note(
                "todo_move_unresolved",
                format!(
                    "a `/todo` state move did not land: {why}. The board below is the plan as it \
                     stands now."
                ),
            );
        }
        // Persist it and tell every head: `flush_todos` is version-gated and runs at a turn
        // boundary, so on a session where no turn ever runs again the operator's rows would sit
        // unpublished. The row does not reach the model's tool reply, so this is not a turn.
        let added = self.todos.snapshot().iter().any(|r| {
            r.by == letibot_tokencore::store::TodoBy::Operator && !before.contains(&r.content)
        });
        self.flush_todos_or_say("the operator's row");
        if added {
            self.operator_added_at = Some(Instant::now());
        }
    }

    /// A driving head's rows, under its handle. Answers whether it added a row; the model is told
    /// what changed at its next turn, since a board moved by someone else is easy to miss.
    pub fn set_driver_todos(
        &mut self,
        from: &str,
        rows: &[(String, letibot_tokencore::store::TodoStatus)],
    ) -> Result<bool, String> {
        let by = letibot_tokencore::store::TodoBy::Peer(from.to_string());
        let before: Vec<TodoItem> = self
            .todos
            .parent_snapshot()
            .into_iter()
            .filter(|r| r.by == by)
            .collect();
        let (changed, reordered) = self.todos.upsert_driver(rows, &by)?;
        if changed == 0 && !reordered {
            return Ok(false);
        }
        let mut said = Vec::new();
        let mut added = false;
        for (content, status) in rows {
            use letibot_tokencore::store::TodoStatus as S;
            let word = match status {
                S::Pending => "pending",
                S::InProgress => "in progress",
                S::Completed => "completed",
                S::Postponed => "postponed",
                S::Cancelled => "cancelled",
            };
            match before.iter().find(|r| r.content.trim() == content.trim()) {
                None => {
                    added = true;
                    said.push(format!("added `{}` ({word})", content.trim()));
                }
                Some(r) if r.status != *status => {
                    said.push(format!("marked `{}` {word}", content.trim()))
                }
                Some(_) => {}
            }
        }
        if reordered {
            said.push("reordered its rows".into());
        }
        self.board_notices.push(format!(
            "{from}, the agent driving this session, changed your todo board: {}. Its rows are \
             work it asked for, not the operator's words; `todo_write` cannot change them.",
            said.join("; ")
        ));
        self.flush_todos_or_say("a driver's row");
        Ok(added)
    }

    /// The children this daemon is running, as the plan-check reads them: the journal says which are running, the
    /// registry how long, the child's hub what it last showed. Taken per ask rather than held, since it is the one input
    /// that moves while nothing else does, and a cached snapshot could not see a child stop.
    pub(crate) fn child_sessions(&self) -> letibot_tools::builtins::todo::ChildSessions {
        crate::sessions::child_sessions(
            &self.session_registry,
            &self.tasks,
            letibot_sessionlog::registry::now_ms(),
        )
    }

    /// Whether this session's monitor `name` is still watching: the one reading the nag's silence
    /// and the wait sweep share.
    fn monitor_watching(&self, name: &str) -> bool {
        self.monitors
            .as_ref()
            .is_some_and(|m| m.get(name).is_some())
    }

    /// The jobs this session is running, as the plan-check reads them: a row waiting on one is not work the check may
    /// speak about, and the moment it ends the row is. Asked per handle through `job_fate`, the same lookup the due
    /// firing reads, so the two cannot disagree and a handle never heard of answers `Unknown`. Taken per ask.
    pub(crate) fn running_jobs(
        &self,
        rows: &[TodoItem],
    ) -> letibot_tools::builtins::todo::RunningJobs {
        let mut jobs = letibot_tools::builtins::todo::RunningJobs::none();
        for row in rows {
            for need in &row.needs {
                // A monitor still watching is the job's sibling: the row waits on it, and the
                // check is quiet about it, until it ends.
                if let letibot_tokencore::store::TodoNeed::Monitor { name } = need {
                    if self.monitor_watching(name) {
                        jobs = jobs.watching(name.clone());
                    }
                    continue;
                }
                // A process named by pid is read off this host's process table, as the seat does.
                if let letibot_tokencore::store::TodoNeed::Pid { pid, start } = need {
                    if procs::alive_edge(*pid, *start) {
                        jobs = jobs.pid(*pid, *start);
                    }
                    continue;
                }
                let letibot_tokencore::store::TodoNeed::Job { handle } = need else {
                    continue;
                };
                if matches!(self.job_fate(handle), JobFate::Running) {
                    jobs = jobs.running(handle.clone());
                }
            }
        }
        // And the grace after an ending: a job or child a row waits on that has ended, whose
        // completion the model has not digested yet. Read off the watcher set's own record of what
        // it delivered when; a running job is skipped, because the grace is for after.
        if let Some(w) = &self.job_watch {
            jobs = graced_endings(
                jobs,
                rows,
                w,
                self.cfg.held_results_cap,
                std::time::SystemTime::now(),
            );
        }
        // And what this session COULD wait on, for the check's offer of `wait_on`: every running job
        // in its own process table and every child it spawned that is still going. The children are
        // this session's alone, because an offer to wait on another's child is one `wait_on` refuses.
        if let Some(host) = self.runtime.backend.processes() {
            for view in host.jobs() {
                if view.state.is_running() {
                    let about = match &view.slug {
                        Some(slug) => format!("{slug}: {}", view.command),
                        None => view.command.clone(),
                    };
                    jobs = jobs.about(view.id.0.clone(), about).running(view.id.0);
                }
            }
        }
        for entry in self.tasks.snapshot() {
            if entry.parent == self.cfg.session_id
                && matches!(entry.state.as_str(), "opening" | "running")
            {
                jobs = jobs
                    .about(entry.name.clone(), entry.prompt)
                    .subagent(entry.name);
            }
        }
        jobs
    }

    /// Take every job, child, pid and monitor wait off the open rows whose thing no longer runs -
    /// ended, killed, or gone after a restart - and leave the model one note saying how each
    /// ended. A delivered settlement freed its job rows already; this is the backstop for the rest.
    pub(crate) fn resolve_ended_waits(&mut self) -> FreedWaits {
        use letibot_tokencore::store::TodoNeed;
        use letibot_tools::builtins::todo::Boards as _;
        let board = self.todos.clone();
        let graced = |handle: &str| {
            self.job_watch.as_ref().is_some_and(|w| {
                w.grace_left(
                    handle,
                    self.cfg.held_results_cap,
                    std::time::SystemTime::now(),
                )
                .is_some()
            })
        };
        let children = self.tasks.snapshot();
        // The other boards this plan waits on, read at most once per name per sweep: a `todo` wait is
        // freed by that board's own row being finished, struck off or gone.
        let boards = crate::seat_boards::SeatBoards::new(
            self.cfg.store_at(),
            self.cfg.workspace.to_string_lossy(),
        );
        let mut read: std::collections::BTreeMap<
            String,
            Result<letibot_tools::builtins::todo::Board, String>,
        > = std::collections::BTreeMap::new();
        let resolved = board.resolve_waits(|need| match need {
            TodoNeed::Job { handle } => {
                let state = match self.job_fate(handle) {
                    JobFate::Running => return None,
                    _ if graced(handle) => return None,
                    // A row that still says `running` with nothing live behind it died with its daemon.
                    JobFate::Ended { state } if state != "running" => Some(state),
                    JobFate::Ended { .. } | JobFate::Unknown => None,
                };
                let at = state.as_ref().and_then(|_| {
                    let store = self.store.as_ref()?;
                    let ms = store.job_updated_ms(&self.cfg.session_id, handle).ok()??;
                    Some(super::open::seated_at(ms as i64))
                });
                Some(letibot_tools::builtins::todo::job_ended_words(
                    handle,
                    state.as_deref(),
                    at.as_deref(),
                ))
            }
            // Only the journal's own ending: a child it does not list may be placed elsewhere.
            TodoNeed::Child { id } => {
                let entry = children.iter().find(|e| &e.name == id)?;
                if !matches!(entry.state.as_str(), "done" | "failed") || graced(id) {
                    return None;
                }
                Some(format!("child {id} ended ({})", entry.state))
            }
            TodoNeed::Pid { pid, start } => (!procs::alive_edge(*pid, *start))
                .then(|| format!("pid {pid} is no longer running")),
            TodoNeed::Monitor { name } => (!self.monitor_watching(name)).then(|| {
                format!("monitor {name} is no longer watching (it fired, expired or was retired)")
            }),
            // Another board's row: read from the store, and freed by that board's answer. A board
            // that cannot be read at all is NO answer - an absence is not an ending, so the wait
            // stands until it can be.
            TodoNeed::Todo { seat, content } => {
                let board = read
                    .entry(seat.clone())
                    .or_insert_with(|| boards.board_of(seat));
                match board {
                    Ok(letibot_tools::builtins::todo::Board::Rows(rows)) => {
                        letibot_tools::builtins::todo::wait_ended(rows, seat, content)
                    }
                    Ok(letibot_tools::builtins::todo::Board::Unknown(_)) => {
                        Some(format!("{seat}'s board is gone"))
                    }
                    Err(_) => None,
                }
            }
            _ => None,
        });
        if resolved.is_empty() {
            return FreedWaits::default();
        }
        let note = letibot_tools::builtins::todo::resolved_waits_note(&resolved);
        let rows: Vec<String> = resolved.iter().flat_map(|r| r.rows.clone()).collect();
        // A freed row that can start now is work, and a note without a turn would read as ignored:
        // it rides the next turn instead, which the caller rings for.
        let board = self.todos.snapshot();
        let children = self.child_sessions();
        let jobs = self.running_jobs(&board);
        let wake = board.iter().any(|r| {
            rows.iter().any(|c| c == &r.content)
                && letibot_tools::builtins::todo::is_ready(&board, r, &children, &jobs)
        });
        if wake {
            self.board_notices.push(note);
        } else if let Err(e) = self.say_without_a_turn(&Facts::Todo { rows }, &note) {
            eprintln!(
                "  {} · todos: the resolved waits were not noted ({e})",
                self.cfg.session_id
            );
        }
        self.flush_todos_or_say("a resolved wait");
        FreedWaits {
            waits: resolved.len(),
            wake,
        }
    }

    /// The `[todo check]` text, or nothing; a postponed row is not part of it. Read through
    /// `the_plan_as_checked`, as the arming decision is, so clock and sentence agree. Asks no layer,
    /// so the due clock can call it; `nag_turn` commits what it named.
    pub fn nag_notice(&self) -> Option<String> {
        let plan = self.the_plans_nag_written().map(|(text, _)| text);
        self.with_the_stale(plan)
    }

    /// The plan's sentence with the stale-prompt one folded in, whichever of the two there is: the
    /// stale sentence is a fact of its own, so a plan a layer held back still reaches the model.
    fn with_the_stale(&self, plan: Option<String>) -> Option<String> {
        // The prompt this session speaks when it is not this daemon's: composed once at open for the
        // attach path and unseen by the model, so a session can run long on a stale one. Told once, as
        // information; the person decides.
        let stale = self.stale_untold();
        match (plan, stale) {
            (None, None) => None,
            (Some(plan), None) => Some(plan),
            (None, Some(stale)) => Some(stale),
            (Some(plan), Some(stale)) => Some(format!("{plan}\n\n{stale}")),
        }
    }

    /// The plan-check's clock as the daemon holding it last read it, written before the turn a check
    /// runs in, so a `nag_` layer is handed the numbers the clock itself decides on.
    pub fn nag_readings(&mut self, clock: letibot_plugin::nag::Clock) {
        self.nag_clock = clock;
    }

    /// The interval a `nag_` layer asked the clock for - unclamped, because the bounds are the
    /// clock's and `Sessions` is where it lives.
    pub fn nag_next(&self) -> Option<Duration> {
        self.nag_next
    }

    /// What the idle clock counts a delivered check AS: the plan's sentence read the way the clock's arming reads it,
    /// so the same notice again means the same plan and not the same wording. Recording the wording would make every
    /// check after the second come due at the base window, the metronome the ladder exists to prevent.
    pub fn nag_said_as(&self) -> Option<String> {
        let board = self.todos.snapshot();
        let children = self.child_sessions();
        let jobs = self.running_jobs(&board);
        let askable = crate::sessions::the_plan_as_checked(&board, &children, &jobs);
        unfinished_plan(&askable, &board, &children, &jobs)
    }

    /// [`Harness::nag_notice`] and the row it named, `None` for the row when the notice is only the
    /// stale-prompt sentence. The `nag_` layer chooses the plan's words; with no layer it is the
    /// harness's own sentence.
    fn nag_notice_naming(&self) -> Option<TheNag> {
        // The board, the narrowed rows the check may speak about (the ONE predicate), the children
        // and the jobs. All four are taken here rather than remembered, because a need is met by a
        // DONE row that the narrowing takes out, and a child or job can go quiet between checks.
        let board = self.todos.snapshot();
        let children = self.child_sessions();
        let jobs = self.running_jobs(&board);
        let askable = crate::sessions::the_plan_as_checked(&board, &children, &jobs);
        let written = unfinished_plan_naming(
            &askable,
            &board,
            self.nag_choice.as_deref(),
            &children,
            &jobs,
        );
        // The plan's text as a value rather than an early return, because this notice has a second
        // thing it may have to say. `nag_should_arm` compares the TEXT, so a notice that gains a
        // sentence is a new thing to say at the base window and the ladder needs no change.
        let (plan, names, next) = match written {
            Some((text, named)) => {
                self.the_layers_check(&board, &askable, &children, &jobs, &text, &named)
            }
            None => (None, Vec::new(), None),
        };
        let text = self.with_the_stale(plan)?;
        Some(TheNag { text, names, next })
    }

    /// The harness's own check, composed and remembered by nobody: the sentence and the rows it
    /// named, asked of no layer. [`Self::nag_notice`] reads this, so the daemon's due clock never
    /// spends a layer's answer on a question it throws away.
    fn the_plans_nag_written(&self) -> Option<(String, Vec<String>)> {
        let board = self.todos.snapshot();
        let children = self.child_sessions();
        let jobs = self.running_jobs(&board);
        let askable = crate::sessions::the_plan_as_checked(&board, &children, &jobs);
        unfinished_plan_naming(
            &askable,
            &board,
            self.nag_choice.as_deref(),
            &children,
            &jobs,
        )
    }

    /// The plan as a `nag_` layer decides it: the sentence, the rows it names, and the interval it
    /// asked for. No layer, a layer that traps, and an answer the host refuses all answer the
    /// harness's own sentence - the check is never lost to a layer.
    fn the_layers_check(
        &self,
        board: &[TodoItem],
        askable: &[TodoItem],
        children: &letibot_tools::builtins::todo::ChildSessions,
        jobs: &letibot_tools::builtins::todo::RunningJobs,
        text: &str,
        named: &[String],
    ) -> (Option<String>, Vec<String>, Option<Duration>) {
        let (native, native_names) = (Some(text.to_string()), named.to_vec());
        // The layer's input names one row, the first: the sticky choice it may keep or move.
        let named = named.first().map(String::as_str).unwrap_or_default();
        let Some(host) = self.plugin_host() else {
            return (native, native_names, None);
        };
        let clock = letibot_plugin::nag::Clock {
            operator_added_ms: self
                .operator_added_at
                .map(|at| at.elapsed().as_millis().min(u128::from(u64::MAX)) as u64),
            ..self.nag_clock
        };
        let input = letibot_tools::builtins::todo::the_nag_input(
            board,
            askable,
            text,
            (!named.is_empty()).then_some(named),
            clock,
            children,
            jobs,
            letibot_sessionlog::registry::now_ms(),
        );
        let Some(decision) = host.nag(&self.hub, &input) else {
            return (native, native_names, None);
        };
        (
            decision.says.then_some(decision.text),
            decision.name,
            decision.next_ms.map(Duration::from_millis),
        )
    }

    /// The stale-prompt sentence, when the model has not been told it yet: `None` when the prompt
    /// is current or the model already heard this very sentence. The model's own copy,
    /// which names no session and no clock.
    fn stale_untold(&self) -> Option<String> {
        self.session_registry
            .stale_for_model(&self.cfg.session_id)
            .filter(|said| self.stale_told.as_deref() != Some(said.as_str()))
    }

    /// The plan's nudge as a turn of its own, when nothing else is happening, so a session that went idle with an
    /// unfinished plan is nudged again. The text goes in as `Speaker::Agent`, exactly as `wake` does: the harness talking
    /// to itself, never readable as the operator's. `Ok(None)` is nothing to say, which is a real outcome.
    pub fn nag_turn(&mut self) -> Result<Option<Reply>, HarnessError> {
        let Some(TheNag { text, names, next }) = self.nag_notice_naming() else {
            return Ok(None);
        };
        // The interval the layer asked for, for the clock's owner to clamp and arm - recorded even
        // when the notice below is dropped, since the layer asked for it either way.
        self.nag_next = next;
        self.meter_nag(names.first().map(String::as_str));
        // The plan-check goes through the one notice door like every other model-facing sentence:
        // a layer may reword it, and may drop it, since it is the harness's own reminder and not a
        // fact somebody is owed. Dropped, no turn is opened.
        let facts = Facts::Nag {
            rows: names.clone(),
        };
        let Some(mut text) = self.say(&facts, &text) else {
            return Ok(None);
        };
        // A plan-check is a turn the model takes anyway, so it carries the held batch, which is
        // what reaches a session that has a plan and nobody typing.
        let riding = self.batch_riding_in();
        if let Some(now) = &riding {
            let batch = now.text();
            if !batch.is_empty() {
                text.push_str("\n\n");
                text.push_str(&batch);
            }
        }
        // The choice is remembered HERE, where the check is delivered: see `nag_notice` for why
        // reading the notice must not. And the check the operator's own row earned has now been
        // said, so their add no longer earns them an earlier one.
        if let Some(named) = names.first() {
            self.nag_choice = Some(named.clone());
        }
        self.operator_added_at = None;
        // And the stale prompt counts as told, once, so the next check does not repeat it.
        if let Some(said) = self.stale_untold() {
            self.stale_told = Some(said);
        }
        // The wall is checked BEFORE the send, as `submit_as_a_normal_session`'s pre-flight does. Insurance rather
        // than the load-bearing fix, since a refused nag is classified as the wall and recovered by the door that
        // answers a refusal: this only spares a round trip the daemon's own numbers can predict.
        self.compact_if_at_the_wall();
        self.trail.begin_turn();
        self.trail.say(Speaker::Agent, &text, Some(Instant::now()));
        let mut opened = false;
        let out = self.submit_items_then(
            vec![TranscriptItem::User {
                speaker: letibot_transcript::Speaker::Agent,
                parts: vec![UserPart::Text { text }],
            }],
            |h| {
                opened = true;
                h.the_batch_stored(riding.as_ref());
            },
        );
        self.the_batch_arrived(riding, opened);
        out.map(Some)
    }

    /// A board row's status by its words, as a word, for a nag's follow-up row: the turn a check
    /// opened touched the row it named when this answers differently after it. `None` is a row no
    /// longer on the board, which is a touch too.
    pub(crate) fn row_status(&self, content: &str) -> Option<String> {
        self.todos
            .snapshot()
            .into_iter()
            .find(|t| t.content == content)
            .map(|t| format!("{:?}", t.status).to_lowercase())
    }

    /// Every board row as `(words, status)`, for the nag follow-up's read.
    pub(crate) fn todo_snapshot_words(&self) -> Vec<(String, String)> {
        self.todos
            .snapshot()
            .into_iter()
            .map(|t| (t.content, format!("{:?}", t.status).to_lowercase()))
            .collect()
    }

    pub(crate) fn row_is_operators(&self, content: &str) -> bool {
        self.todos
            .snapshot()
            .iter()
            .any(|t| t.content == content && t.by == letibot_tokencore::store::TodoBy::Operator)
    }

    /// The operator's pause of this session's plan-check, or `None` while it runs. See
    /// [`crate::nag_pause`].
    pub fn nag_pause(&self) -> Option<letibot_sessionlog::NagPause> {
        self.cfg.nags_paused
    }

    /// The pause as the clock arms for it: the wall-clock end turned into an `Instant` from now,
    /// read fresh at every arming so the clock never holds a pause this session no longer has. See
    /// [`crate::sessions::NagHold`].
    pub fn nag_hold(&self) -> crate::sessions::NagHold {
        use crate::sessions::NagHold;
        use letibot_sessionlog::NagPause;
        // The provider's clock first: a parked session's plan-check and its census are both turns,
        // and a turn is what the model just refused. Nothing is armed until the park resumes it.
        if self.limit_park.is_some() {
            return NagHold::Parked;
        }
        match self.cfg.nags_paused {
            None => NagHold::Running,
            Some(NagPause::UntilResumed) => NagHold::Paused { until: None },
            Some(NagPause::Until { ms }) => {
                let left = ms.saturating_sub(letibot_sessionlog::registry::now_ms());
                NagHold::Paused {
                    until: Some(std::time::Instant::now() + std::time::Duration::from_millis(left)),
                }
            }
        }
    }

    /// `/nag`, on this session: the report, the pause or the resume, as the verb's reply. The clock
    /// is its owner's to re-arm afterwards (`Sessions`' arm, `serve_child`'s); this half moves the
    /// session's fact and says it. `by` is who sent the verb.
    pub fn nag_verb(
        &mut self,
        verb: crate::nag_pause::NagVerb,
        by: crate::nag_pause::NagBy,
    ) -> crate::slash::SlashReply {
        use crate::nag_pause::{self as np, NagVerb};
        let now = letibot_sessionlog::registry::now_ms();
        let lines = match verb {
            NagVerb::Report => vec![np::report(self.cfg.nags_paused, now)],
            NagVerb::Pause(d) => {
                let p = match d {
                    None => letibot_sessionlog::NagPause::UntilResumed,
                    Some(d) => letibot_sessionlog::NagPause::Until {
                        ms: now.saturating_add(d.as_millis() as u64),
                    },
                };
                let mut lines = vec![np::paused_reply(p, now)];
                if let Err(why) = self.pause_nags(p, by) {
                    lines.push(why);
                }
                lines
            }
            NagVerb::Resume => match self.cfg.nags_paused {
                None => vec!["nags are not paused in this session, so nothing changed".into()],
                Some(_) => {
                    let mut lines = vec![
                        "nags RESUMED for this session: the plan-check is armed again from now"
                            .into(),
                    ];
                    if let Err(why) = self.resume_nags(np::Resumed::ByVerb(by)) {
                        lines.push(why);
                    }
                    lines
                }
            },
        };
        crate::slash::SlashReply { lines, ok: true }
    }

    /// Pause this session's plan-check: the config, the row and the hub together, since a reader who finds one without
    /// the others has been lied to. The `nags_paused` row is the loud half and the settings row is what a head attaching
    /// later reads. A row the store refused is SAID, and the pause still holds in memory.
    pub fn pause_nags(
        &mut self,
        p: letibot_sessionlog::NagPause,
        by: crate::nag_pause::NagBy,
    ) -> Result<(), String> {
        // Read before `now`, so this pause is never older than the life it belongs to.
        crate::nag_pause::daemon_life_started_ms();
        let now = letibot_sessionlog::registry::now_ms();
        let who = crate::nag_pause::PausedBy {
            by,
            at_ms: Some(now),
        };
        let stored = self.store_nag_pause(Some((p, &who)));
        let mut detail = format!(
            "nags PAUSED by {} {}: this session's plan-check is not armed or sent while it holds",
            who.by.words(),
            crate::nag_pause::until_words(p, now)
        );
        if let Err(why) = &stored {
            detail.push_str(&format!(". {why}"));
        }
        self.cfg.nags_paused = Some(p);
        self.cfg.nags_paused_by = Some(who);
        self.hub.warn("nags_paused", detail);
        self.publish_settings();
        stored
    }

    /// End the pause: by the verb, at its own deadline, or noticed at open after it ran out while
    /// the session was closed. The `nags_resumed` row says which.
    pub fn resume_nags(&mut self, why: crate::nag_pause::Resumed) -> Result<(), String> {
        let Some(was) = self.cfg.nags_paused.take() else {
            return Ok(());
        };
        self.cfg.nags_paused_by = None;
        let stored = self.store_nag_pause(None);
        let mut detail =
            crate::nag_pause::resumed_words(why, was, letibot_sessionlog::registry::now_ms());
        if let Err(why) = &stored {
            detail.push_str(&format!(". {why}"));
        }
        self.hub.warn("nags_resumed", detail);
        self.publish_settings();
        stored
    }

    /// The row half of [`Self::pause_nags`] and [`Self::resume_nags`]. No store is no row and no
    /// error: a bare harness has nothing to outlast.
    fn store_nag_pause(
        &self,
        p: Option<(letibot_sessionlog::NagPause, &crate::nag_pause::PausedBy)>,
    ) -> Result<(), String> {
        let Some(store) = &self.store else {
            return Ok(());
        };
        let row = p.map(|(p, who)| letibot_tokencore::store::StoredNagPause {
            until: p.to_stored(),
            at_ms: who.at_ms.map(|ms| ms as i64),
            by: Some(who.by.to_stored()),
        });
        store
            .set_nag_pause(&self.cfg.session_id, row.as_ref())
            .map_err(|e| {
                format!(
                    "Writing it to the session row FAILED ({e}), so it holds only until this daemon stops"
                )
            })
    }

    /// The pause at open, from the session's own row. A pause made by this daemon is kept; one from
    /// an earlier daemon is dropped and said, since a pause never survives a restart. The row is read
    /// even when empty because a child's config clones its parent's, pause included.
    pub fn restore_nag_pause(&mut self) {
        use crate::nag_pause::{NagBy, PausedBy};
        self.cfg.nags_paused = None;
        self.cfg.nags_paused_by = None;
        let Some(row) = self
            .store
            .as_ref()
            .and_then(|s| s.nag_pause(&self.cfg.session_id).ok())
            .flatten()
        else {
            return;
        };
        let started = crate::nag_pause::daemon_life_started_ms();
        let now = letibot_sessionlog::registry::now_ms();
        let p = letibot_sessionlog::NagPause::from_stored(row.until);
        let who = PausedBy {
            by: row
                .by
                .as_deref()
                .map(NagBy::from_stored)
                .unwrap_or(NagBy::Unknown),
            at_ms: row.at_ms.and_then(|ms| u64::try_from(ms).ok()),
        };
        if who.at_ms.is_some_and(|at| at >= started) {
            self.cfg.nags_paused = Some(p);
            self.cfg.nags_paused_by = Some(who);
            if p.over_at(now) {
                let _ = self.resume_nags(crate::nag_pause::Resumed::WhileClosed);
            }
            return;
        }
        let mut detail = crate::nag_pause::restart_words(&who, now);
        if let Err(why) = self.store_nag_pause(None) {
            detail.push_str(&format!(". {why}"));
        }
        self.hub.warn("nags_resumed", detail);
    }
}
