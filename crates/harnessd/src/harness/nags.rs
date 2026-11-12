//! The plan-check board: the operator's half of the todo list, the sentence the idle clock counts,
//! and the nudge it delivers as a turn of its own.
use super::*;
use letibot_plugin::notice::Facts;
use std::time::Duration;

/// One plan-check as it stands: the sentence, the rows it names, and the interval a `nag_` layer
/// asked the clock for. The interval is unclamped here - the bounds are the clock's.
struct TheNag {
    text: String,
    names: Vec<String>,
    next: Option<Duration>,
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
                    if self
                        .monitors
                        .as_ref()
                        .is_some_and(|m| m.get(name).is_some())
                    {
                        jobs = jobs.watching(name.clone());
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

    /// The `[todo check]` text, or nothing, and a row the operator postponed is not part of it. The rows are read
    /// through `the_plan_as_checked`, the same reading the arming decision takes, so clock and sentence cannot disagree.
    /// Reading it does not remember what it named: `nag_turn` commits it, where a check is delivered.
    ///
    /// **Ask-free**, and that is what the daemon's due clock needs: it reads this to know whether a
    /// check is worth a turn, and the layer is asked once, inside the turn.
    pub fn nag_notice(&self) -> Option<String> {
        let plan = self.the_plans_nag_written().map(|(text, _)| text);
        self.with_the_stale(plan)
    }

    /// The plan's sentence with the stale-prompt one folded in, whichever of the two there is: the
    /// stale sentence is a fact of its own, so a plan a layer held back still reaches the model.
    fn with_the_stale(&self, plan: Option<String>) -> Option<String> {
        // The prompt this session speaks when it is not this daemon's: the sentence is composed once
        // at open for the attach path and the model cannot see it, so a session can run long on a
        // prompt its daemon would not compose. Told once, as information; the person has the choice
        // on screen.
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

    /// [`Harness::nag_notice`] and the row it named - `None` for the second half when the notice is
    /// only the stale-prompt sentence, which names no row. The plan's words are the `nag_` layer's
    /// to choose: it is asked here, and no layer means the harness's own sentence, which is what a
    /// session without layers has always read.
    fn nag_notice_naming(&self) -> Option<TheNag> {
        // The board, the narrowed rows the check may speak about (the ONE predicate), the children
        // and the jobs. All four are taken here rather than remembered, because a need is met by a
        // DONE row that the narrowing takes out, and a child or job can go quiet between checks.
        let board = self.todos.snapshot();
        let children = self.child_sessions();
        let jobs = self.running_jobs(&board);
        let askable = crate::sessions::the_plan_as_checked(&board, &children, &jobs);
        let written = unfinished_plan_for(
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

    /// The harness's own check, composed and remembered by nobody: the sentence and the row it
    /// named, asked of no layer. [`Self::nag_notice`] reads this, so the daemon's due clock never
    /// spends a layer's answer on a question it throws away.
    fn the_plans_nag_written(&self) -> Option<(String, Option<String>)> {
        let board = self.todos.snapshot();
        let children = self.child_sessions();
        let jobs = self.running_jobs(&board);
        let askable = crate::sessions::the_plan_as_checked(&board, &children, &jobs);
        let written = unfinished_plan_for(
            &askable,
            &board,
            self.nag_choice.as_deref(),
            &children,
            &jobs,
        )?;
        Some((written.0, Some(written.1)))
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
        named: &str,
    ) -> (Option<String>, Vec<String>, Option<Duration>) {
        let (native, native_names) = (
            Some(text.to_string()),
            if named.is_empty() {
                Vec::new()
            } else {
                vec![named.to_string()]
            },
        );
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
            |_| opened = true,
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
    /// session's fact and says it.
    pub fn nag_verb(&mut self, verb: crate::nag_pause::NagVerb) -> crate::slash::SlashReply {
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
                if let Err(why) = self.pause_nags(p) {
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
                    if let Err(why) = self.resume_nags(np::Resumed::ByVerb) {
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
    pub fn pause_nags(&mut self, p: letibot_sessionlog::NagPause) -> Result<(), String> {
        let now = letibot_sessionlog::registry::now_ms();
        self.cfg.nags_paused = Some(p);
        let stored = self.store_nag_pause(Some(p));
        let mut detail = format!(
            "nags PAUSED by the operator {}: this session's plan-check is not armed or sent while it holds",
            crate::nag_pause::until_words(p, now)
        );
        if let Err(why) = &stored {
            detail.push_str(&format!(". {why}"));
        }
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
    fn store_nag_pause(&self, p: Option<letibot_sessionlog::NagPause>) -> Result<(), String> {
        let Some(store) = &self.store else {
            return Ok(());
        };
        store
            .set_nags_paused_until(&self.cfg.session_id, p.map(|p| p.to_stored()))
            .map_err(|e| {
                format!(
                    "Writing it to the session row FAILED ({e}), so it holds only until this daemon stops"
                )
            })
    }

    /// A resume comes back with the operator's pause, from the session's own row, for every session that opens. The
    /// row is the answer even when it says nothing, since a child's config is a clone of its parent's and a parent's
    /// pause must not ride into it. A pause whose end passed while nobody held the session is ended here and said.
    pub fn restore_nag_pause(&mut self) {
        let stored = self
            .store
            .as_ref()
            .and_then(|s| s.session(&self.cfg.session_id).ok())
            .flatten()
            .and_then(|s| s.nags_paused_until)
            .map(letibot_sessionlog::NagPause::from_stored);
        self.cfg.nags_paused = stored;
        if let Some(p) = stored
            && p.over_at(letibot_sessionlog::registry::now_ms())
        {
            let _ = self.resume_nags(crate::nag_pause::Resumed::WhileClosed);
        }
    }
}
