//! The plan-check board: the operator's half of the todo list, the sentence the idle clock counts,
//! and the nudge it delivers as a turn of its own.
use super::*;

impl Harness {
    /// The operator's half of the board: replaces their rows and leaves the model's alone, so
    /// anything that reads the board afterwards sees both.
    ///
    /// `moved` is a STATE and never a membership: `/todo postpone|resume <the row's words>` on a
    /// row the model wrote, which the operator can silence and can never add, drop or re-word. A
    /// name that resolves to nothing is the daemon's log, and the union published below corrects it.
    pub fn set_operator_todos(
        &mut self,
        items: Vec<letibot_tokencore::store::TodoItem>,
        moved: &[(String, letibot_tokencore::store::TodoStatus)],
    ) {
        self.todos.set_operator(items);
        // And the state moves that are not the head's to make: the board resolves the words against
        // the model's half and sets the status, which is the one thing the operator can change
        // about a row they did not write.
        //
        // A refusal is the daemon's log rather than the operator's silence: a model that rewrote its
        // plan in between makes a name resolve to nothing, and the union this write publishes is the
        // correction.
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
        self.flush_todos_or_say("the operator's row");
    }

    /// The children this daemon is running, as the plan-check reads them: the task journal says
    /// which are still running, the session registry how long, the child's own hub what it last
    /// showed. `crate::sessions::child_sessions` is the one builder, so the two readers of one plan
    /// cannot come to different answers about a child.
    ///
    /// Taken per ask rather than held: it is the one input that moves while nothing else does, and
    /// a cached snapshot would be a check that cannot see a child stop.
    pub(crate) fn child_sessions(&self) -> letibot_tools::builtins::todo::ChildSessions {
        crate::sessions::child_sessions(
            &self.session_registry,
            &self.tasks,
            letibot_sessionlog::registry::now_ms(),
        )
    }

    /// The jobs this session is running, as the plan-check reads them: a row that waits on one is
    /// not work the check may speak about ([`letibot_tokencore::store::TodoNeed::Job`]), and the
    /// moment it ends the row is.
    ///
    /// Asked per handle through [`Harness::job_fate`], the same lookup the `[todo] … is due` firing
    /// reads, so the two readers cannot disagree and a handle never heard of answers `Unknown`. A
    /// listing instead would be a second way to ask *is this running*, and the one that disagreed
    /// after a restart, where the live table is empty and only the record knows what became of it.
    ///
    /// Taken per ask, for `child_sessions`' reason: a job ends while nothing else moves.
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

    /// The `[todo check]` text, or nothing, and a row the operator has postponed is not part of it.
    ///
    /// The rows are read through `sessions::the_plan_as_checked`, the same reading the arming
    /// decision takes, so the clock and the sentence cannot disagree about what the plan is, even if
    /// the clock was armed before the row was postponed.
    ///
    /// Reading it does not remember the row it names: both delivery doors read this once to record
    /// the notice as said and then [`Harness::nag_turn`] reads it again, so a choice written here
    /// would count the first reading as asked. `nag_turn` commits it, where a check is delivered.
    pub fn nag_notice(&self) -> Option<String> {
        self.nag_notice_naming().map(|(text, _)| text)
    }

    /// What the idle clock counts a delivered check AS: the plan's sentence read the way the clock's
    /// own arming reads it, so *the same notice again* means the same plan and not the same wording.
    ///
    /// The wording moves on its own, and recording the wording as said would make every check after
    /// the second come due at the base window, which is the metronome the ladder exists to prevent.
    ///
    /// The queue's census is not in here: it is a door of its own, recorded against its own anchor,
    /// because its sentence names the age of the oldest row and so is new at every delivery.
    pub fn nag_said_as(&self) -> Option<String> {
        let board = self.todos.snapshot();
        let children = self.child_sessions();
        let jobs = self.running_jobs(&board);
        let askable = crate::sessions::the_plan_as_checked(&board, &children, &jobs);
        unfinished_plan(&askable, &board, &children, &jobs)
    }

    /// [`Harness::nag_notice`] and the row it named — `None` for the second half when the notice is
    /// only the stale-prompt sentence, which names no row.
    fn nag_notice_naming(&self) -> Option<(String, Option<String>)> {
        // The board, the narrowed rows the check may speak about (the ONE predicate), the children
        // and the jobs. All four are taken here rather than remembered, because a need is met by a
        // DONE row that the narrowing takes out, and a child or job can go quiet between checks.
        let board = self.todos.snapshot();
        let children = self.child_sessions();
        let jobs = self.running_jobs(&board);
        let askable = crate::sessions::the_plan_as_checked(&board, &children, &jobs);
        // The plan's text as a value rather than an early return, because this notice has a second
        // thing it may have to say. `nag_should_arm` compares the TEXT, so a notice that gains a
        // sentence is a new thing to say at the base window and the ladder needs no change.
        let plan = unfinished_plan_for(
            &askable,
            &board,
            self.nag_choice.as_deref(),
            &children,
            &jobs,
        );
        let (plan, named) = match plan {
            Some((text, named)) => (Some(text), Some(named)),
            None => (None, None),
        };
        // And the prompt this session is speaking, when it is not this daemon's. The sentence is
        // composed once at open and left in the registry for the ATTACH path, and the model cannot
        // see it either, so a session can spend an evening speaking a prompt its daemon would not
        // compose. Nothing clears it but a re-seat, which is exactly when it stops being true.
        //
        // Told once and as information: the person has the choice on their screen as a card, so the
        // model is told the fact and that the decision is not its to press.
        let stale = self
            .stale_untold()
            .map(|said| stale_notice_for_the_model(&said));
        let text = match (plan, stale) {
            (None, None) => None,
            (Some(plan), None) => Some(plan),
            (None, Some(stale)) => Some(stale),
            (Some(plan), Some(stale)) => Some(format!("{plan}\n\n{stale}")),
        };
        text.map(|text| (text, named))
    }

    /// The stale-prompt sentence, when the model has not been told it yet: `None` when the prompt
    /// is current or the model already heard this very sentence.
    fn stale_untold(&self) -> Option<String> {
        self.session_registry
            .stale_prefix(&self.cfg.session_id)
            .filter(|said| self.stale_told.as_deref() != Some(said.as_str()))
    }

    /// The plan's nudge as a turn of its own, when nothing else is happening.
    ///
    /// This delivers the check without a turn boundary. Inside the loop it was one nudge per user
    /// turn, so a session that went idle with an unfinished plan was nudged once and never again,
    /// while one in constant conversation was nudged after every exchange.
    ///
    /// The text goes in as `Speaker::Agent`, exactly as [`Harness::wake`] and
    /// [`Harness::continue_after_wall`] do: the harness talking to itself, never readable as the
    /// operator's words and never an authorisation. `Ok(None)` is nothing to say, which is a real
    /// outcome and not a failure: no turn is spent on a nudge with no finding behind it.
    pub fn nag_turn(&mut self) -> Result<Option<Reply>, HarnessError> {
        let Some((mut text, named)) = self.nag_notice_naming() else {
            return Ok(None);
        };
        self.meter_nag(named.as_deref());
        // A plan-check is a turn the model takes anyway, so it carries the held batch, which is
        // what reaches a session that has a plan and nobody typing.
        if let Some(meanwhile) = self.deliveries().and_then(|d| d.at_turn_start()) {
            text.push_str("\n\n");
            text.push_str(&meanwhile);
        }
        // The choice is remembered HERE, where the check is delivered: see `nag_notice` for why
        // reading the notice must not.
        if let Some(named) = named {
            self.nag_choice = Some(named);
        }
        // And the stale prompt counts as told, once, so the next check does not repeat it.
        if let Some(said) = self.stale_untold() {
            self.stale_told = Some(said);
        }
        // The wall is checked BEFORE the send, as `submit_as_a_normal_session`'s pre-flight does.
        // Insurance rather than the load-bearing fix, because a refused nag is still classified as
        // the wall and recovered by the door that answers a refusal: this only spares a round trip
        // the daemon's own numbers can predict, since a nag is the cheapest turn and the most
        // pointless one to spend on a prompt already known not to fit.
        self.compact_if_at_the_wall();
        self.trail.begin_turn();
        self.trail.say(Speaker::Agent, &text, Some(Instant::now()));
        self.submit_item(TranscriptItem::User {
            speaker: letibot_transcript::Speaker::Agent,
            parts: vec![UserPart::Text { text }],
        })
        .map(Some)
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

    /// Pause this session's plan-check: the config, the row and the hub together, for
    /// [`Self::stand_down_auto_compact`]'s reason, that a reader who finds one without the others
    /// has been lied to. The `nags_paused` row is the loud half; the settings row is what a head
    /// that attaches later reads.
    ///
    /// A row the store refused is SAID, in the hub row and in the `Err` the verb's reply carries,
    /// and the pause still holds in memory: it is real until this daemon stops.
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
        self.hub.publish(SessionEvent::Warning {
            code: "nags_paused".into(),
            detail,
            compaction: None,
        });
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
        self.hub.publish(SessionEvent::Warning {
            code: "nags_resumed".into(),
            detail,
            compaction: None,
        });
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

    /// A resume comes back with the operator's pause, from the session's own row: called at open,
    /// for every session that opens, children included.
    ///
    /// The row is the answer even when it says nothing: a child's config is a clone of its parent's,
    /// and a parent's pause must not ride into it (see [`crate::nag_pause`]), so a session with no
    /// row, or no store, opens running. A pause whose end passed while nobody held the session is
    /// ended here and said, rather than restored as a pause nothing would ever lift.
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
