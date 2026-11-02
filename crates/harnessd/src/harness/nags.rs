//! **The plan-check board**: the operator's half of the todo list, the sentence the idle
//! clock counts, and the nudge it delivers as a turn of its own.
use super::*;

impl Harness {
    /// **The plan, as a nudge — or nothing when there is no plan to nudge about.**
    ///
    /// The finding half of [`Harness::close_the_turn`] on its own: `close_the_turn` joins
    /// this with the intent ledger's own finding, because a turn boundary is where both are
    /// checked and one nudge budget has to carry both. This is the same text with nothing
    /// joined to it, for the caller that has no turn boundary to hang it on.
    /// **The operator's half of the board** — see `Sessions::dispatch`'s `SetOperatorTodos`.
    ///
    /// The board is one list with two authors; this replaces the operator's half and leaves the
    /// model's alone. Anything that reads the board afterwards — the pane, the model's prompt, and
    /// `nag_notice` above — sees both, which is the whole of the design.
    ///
    /// **`moved` is the one thing that reaches the model's half**, and it is a STATE and never a
    /// membership: `/todo postpone|resume <the row's words>` on a row the model wrote, which the
    /// operator can silence and can never add, drop or re-word. It is resolved by
    /// `TodoBoard::set_states` — the words are the key — and a name that resolves to nothing (the
    /// model rewrote its plan between the head's read and this write) is the daemon's log and the
    /// union below is the correction.
    ///
    /// **The version bump IS the announcement.** `set_operator` raises the board's version, and
    /// `flush_todos` — which runs at every turn boundary — publishes a `TodosUpdated` and persists
    /// when the version has moved. So the operator's rows reach every head and the store by the same
    /// path the model's do, with no second mechanism.
    pub fn set_operator_todos(
        &mut self,
        items: Vec<letibot_tokencore::store::TodoItem>,
        moved: &[(String, letibot_tokencore::store::TodoStatus)],
    ) {
        self.todos.set_operator(items);
        // **And the state moves that are not the head's to make.** `/todo postpone|resume` names a
        // row the MODEL wrote by its words; the board resolves them against the model's half and
        // sets the status, which is the one thing the operator can change about a row they did not
        // write — never its membership, its words or its author.
        //
        // **A refusal is the daemon's log and not the operator's silence.** The head resolved these
        // names against the union it was looking at; a model that rewrote its plan in between makes
        // a name resolve to nothing, and there is no frame here to say so on — the union this write
        // publishes IS the correction, and the head draws it.
        if !moved.is_empty()
            && let Err(why) = self.todos.set_states(moved)
        {
            eprintln!("  todos: a state move did not land: {why}");
            // The head that sent it is told too (R67 item 15): the union this write publishes is
            // the correction, and this is the sentence that says why the row did not move.
            self.import_note(
                "todo_move_unresolved",
                format!(
                    "a `/todo` state move did not land: {why}. The board below is the plan as it \
                     stands now."
                ),
            );
        }
        // **AND PERSIST IT AND TELL EVERY HEAD, which is the half the version bump alone does not
        // do.** `flush_todos` is version-gated and runs at every TURN BOUNDARY — so on a session
        // where no turn ever runs again, the operator's rows would sit on the board unpublished and
        // unwritten, and `Sessions::dispatch` is the one place that can flush them at the moment they
        // arrive. MEASURED as an omission by reading the path rather than by watching it fail: the
        // bump makes `flush_todos` willing, and nothing made it happen.
        //
        // The row does not reach the model's TOOL REPLY — that is `todo`'s own — so nothing here is
        // a turn.
        self.flush_todos_or_say("the operator's row");
    }

    /// **The children this daemon is running, as the plan-check reads them.**
    ///
    /// The fact a board cannot hold, taken where it lives: the task journal says which children are
    /// still running and the session registry says how long each has been, and the child's own hub
    /// says what it last showed. `crate::sessions::child_sessions` is the one builder — the daemon's
    /// worker arm calls the same function for the clock it holds, so the two readers of one plan
    /// cannot come to different answers about a child.
    ///
    /// **Taken per ask rather than held**, because it is the one input to the check that moves
    /// while nothing else does: a board nobody touched reads differently ten minutes later, and a
    /// cached snapshot would be a check that cannot see a child stop.
    pub(crate) fn child_sessions(&self) -> letibot_tools::builtins::todo::ChildSessions {
        crate::sessions::child_sessions(
            &self.session_registry,
            &self.tasks,
            letibot_sessionlog::registry::now_ms(),
        )
    }

    /// **The jobs this session is running, as the plan-check reads them.**
    ///
    /// The other fact a board cannot hold, and the one the check has to know to stay SILENT about a
    /// row that waits on a job ([`letibot_tokencore::store::TodoNeed::Job`]): while the job runs the
    /// row is not work the check may speak about, and the moment it ends the row is.
    ///
    /// **Asked PER HANDLE the plan names, through [`Harness::job_fate`]** — the same lookup the
    /// `[todo] … is due` firing reads through, so the two readers of one handle cannot come to
    /// different answers about it, and a handle this session has never heard of answers `Unknown`,
    /// which is a job that ENDED. A snapshot built from a listing instead would be a second way to
    /// ask *is this running* — and the second way would be the one that disagreed after a restart,
    /// where the live table is empty and only the record knows what became of the job.
    ///
    /// **Taken per ask rather than held**, for `child_sessions`' reason: a job ends while nothing
    /// else moves, and a cached snapshot would be a check that cannot see it stop.
    pub(crate) fn running_jobs(
        &self,
        rows: &[TodoItem],
    ) -> letibot_tools::builtins::todo::RunningJobs {
        let mut jobs = letibot_tools::builtins::todo::RunningJobs::none();
        for row in rows {
            for need in &row.needs {
                // **A monitor still watching** is the job's sibling: the row waits on it, and the
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
        // **And the grace after an ending** — a job or child a row waits on that has ended, whose
        // completion the model has not digested yet: still queued or held, or delivered during the
        // turn whose end is being checked. Read off the watcher set's own record of what it
        // delivered when (`JobWatchers::grace_left`), with R79's starvation cap as the backstop.
        // A running job is skipped: it is `running` above, and the grace is for after.
        if let Some(w) = &self.job_watch {
            jobs = graced_endings(
                jobs,
                rows,
                w,
                self.cfg.held_results_cap,
                std::time::SystemTime::now(),
            );
        }
        // **And what this session COULD wait on, for the check's offer of `wait_on`** — every job
        // its own process table says is running, and every child it spawned that the journal says
        // is still going. Adding a running job that no row names changes no row's reading (a row's
        // edge asks about its own handle), and the live table is the first look `job_fate` takes,
        // so the two cannot disagree about a job that runs. The children are this session's alone —
        // `child_sessions` is the daemon's whole journal — because an offer to wait on somebody
        // else's child is an offer `wait_on` would refuse.
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

    /// **The `[todo check]` text, or nothing** — and a row the operator has postponed is not part
    /// of it.
    ///
    /// The rows are read through `sessions::the_plan_as_checked`, which is the same reading the
    /// arming decision takes: a plan whose only unfinished row has been set aside is a plan with
    /// nothing to check, and it must not produce this sentence **even if the clock was armed
    /// before the row was postponed** — which is the one way a postponed row could still be
    /// nagged about, and the reason this is filtered here rather than left to the arming alone.
    /// One predicate, so the clock and the sentence cannot disagree about what the plan is.
    ///
    /// **And the row this names is remembered, so the question does not wander.** The check picks a
    /// row once and names it again until it stops being askable — a row the MODEL adds elsewhere on
    /// the plan cannot change which row it is being held to, and the operator's ready rows come
    /// before it (R77, `unfinished_plan_for`) — and a row that becomes `InProgress` changes the
    /// QUESTION rather than the choice. The clock keeps the TEXT, which is what the ladder counts
    /// repeats of; the row itself is this harness's to remember, because the text cannot say which
    /// row it named.
    ///
    /// **Reading it does not remember it** (R77). Both delivery doors read this once to record the
    /// notice as said and then run [`Harness::nag_turn`], which reads it again — and when this
    /// wrote `nag_choice` the second reading already counted the first as *asked*, so the model's
    /// FIRST check of any row said *"you were asked about this one already"*, and the first check
    /// after an operator's add could never say they added it. The choice is committed by
    /// `nag_turn`, the one place a check is actually delivered.
    pub fn nag_notice(&self) -> Option<String> {
        self.nag_notice_naming().map(|(text, _)| text)
    }

    /// **What the idle clock counts a delivered check AS** — the plan's sentence read the way the
    /// clock's own arming reads it (`sessions::nag_should_arm`: the narrowed rows, no sticky
    /// choice, no stale-prompt sentence), so *the same notice again* means the same plan and not
    /// the same wording (R77).
    ///
    /// The wording moves on its own: the first check of a row names it plainly (or says the
    /// operator added it) and the next says *"you were asked about this one already"*. Recording
    /// the WORDING as said made the second delivery a different notice from the arming's reading
    /// of an unchanged plan, so every check after it came due at the base window — the metronome
    /// the ladder exists to prevent. The delivery doors record this instead of the text they send.
    ///
    /// **The queue's census is NOT in here** — it is a door of its own (`Door::Census`,
    /// `Sessions::deliver_due_censuses`), recorded against its own anchor because its sentence names
    /// the age of the oldest row and so is a different notice at every delivery. What this returns
    /// is the plan's sentence, which is what the ladder counts repeats of; `None` is the right
    /// answer for a project with nothing on its board and eight rows stuck in its queue.
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
        // **The board and the rows it may speak about, both** — the second is the narrowing
        // (`the_plan_as_checked`, the ONE predicate) and the first is what a row's edges resolve
        // against, since a need is met by a row that is DONE and a done row is exactly what the
        // narrowing takes out. `unfinished_plan_for` says why they are two arguments.
        //
        // **And the children, which are the third thing a row can wait on** and the one fact none
        // of the above holds: `child_sessions` is the daemon's snapshot of what is running, taken
        // here so a child that has gone quiet since the last check is seen to have.
        //
        // **And the jobs, which are the fourth** — the same argument one layer down, and the one the
        // operator's own long jobs made: a row that waits on a job that is running is not work this
        // check may speak about, so the snapshot is taken here too rather than remembered.
        let board = self.todos.snapshot();
        let children = self.child_sessions();
        let jobs = self.running_jobs(&board);
        let askable = crate::sessions::the_plan_as_checked(&board, &children, &jobs);
        // **The plan's text, as a value rather than an early return**, because this notice now
        // has a second thing it may have to say and a completed plan is no reason to say
        // neither. `nag_should_arm` compares the TEXT, so a notice that gains a sentence is a
        // new thing to say at the base window and an unchanged one is a repeat at the cap —
        // the ladder needs no change at all for this.
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
        // **And the prompt this session is speaking, when it is not this daemon's.**
        //
        // The sentence is composed once, at open, by the fingerprint comparison and left in the
        // registry for the ATTACH path — because a warning published before a head arrives is
        // filed `Placed::Before` and never drawn. The head is not the only reader it needs: the
        // model cannot see it either, so a session can spend an evening speaking a prompt its
        // daemon would not compose, with the model unable to name the problem. This is the
        // model's copy, and it ends with what to DO rather than with the diagnosis — the same
        // shape `[todo check]` uses, for the same reason: the rule travels with the notice
        // instead of living somewhere the model has to remember to look.
        //
        // Nothing clears this but a re-seat, and that is deliberate: `set_stale_prefix(…, None)`
        // is written by the fork onto a new prompt, so the reminder stops being true exactly
        // when the person acts — the loop closes without anybody having to remember to close it.
        //
        // **Told ONCE, and as information** (see `stale_told`): the person has the choice on their
        // screen as a card that stays until the prompt is current, so the model is told the fact
        // and that the decision is not its to press — and is not told it again.
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

    /// **The stale-prompt sentence, when the model has not been told it yet** — `None` when the
    /// prompt is current or the model already heard this very sentence (see `stale_told`).
    fn stale_untold(&self) -> Option<String> {
        self.session_registry
            .stale_prefix(&self.cfg.session_id)
            .filter(|said| self.stale_told.as_deref() != Some(said.as_str()))
    }

    /// **The plan's nudge as a turn of its own, when nothing else is happening.**
    ///
    /// The scheduling question the operator raised: *"we have to think about scheduling.
    /// maybe wait for a timeout actually. so send it when model is idling"* — and, on the
    /// failure the old arrangement had, *"but certainly not after my message."*
    ///
    /// What this gives the caller is a way to deliver the check **without a turn boundary**.
    /// The old path could only run it inside the loop, at the end of a turn (`nudges_left`,
    /// one per user turn), so a session that went idle with an unfinished plan was nudged
    /// once and then never again — while a session in constant conversation was nudged after
    /// every exchange, which is the overkill the operator reported. Neither is a schedule;
    /// both are consequences of there being nowhere else to put the check.
    ///
    /// The text goes in as `Speaker::Agent`, exactly as [`Harness::wake`] and
    /// [`Harness::continue_after_wall`] do: the harness talking to itself, never readable as
    /// the operator's words and never an authorisation.
    ///
    /// `Ok(None)` when there is nothing to say — an empty or finished plan — which is a real
    /// outcome and not a failure: a caller that asked has its answer, and no turn is spent
    /// on a nudge with no finding behind it.
    pub fn nag_turn(&mut self) -> Result<Option<Reply>, HarnessError> {
        let Some((mut text, named)) = self.nag_notice_naming() else {
            return Ok(None);
        };
        self.meter_nag(named.as_deref());
        // **R79: a plan-check is a turn the model takes anyway, so it carries the held batch** —
        // which is what reaches a session that has a plan and nobody typing.
        if let Some(meanwhile) = self.deliveries().and_then(|d| d.at_turn_start()) {
            text.push_str("\n\n");
            text.push_str(&meanwhile);
        }
        // **The choice is remembered HERE, where the check is delivered** — see `nag_notice` for
        // why reading the notice must not.
        if let Some(named) = named {
            self.nag_choice = Some(named);
        }
        // **And the stale prompt counts as told** — once, so the next check does not repeat it.
        if let Some(said) = self.stale_untold() {
            self.stale_told = Some(said);
        }
        // **The wall is checked BEFORE the send — `submit_as_a_normal_session`'s
        // pre-flight, from the same function.** Insurance, not the load-bearing
        // fix: a refused nag is still classified as the wall (see the `Http` arm
        // in `run_rounds`) and recovered by the door that answers a refusal, so
        // this exists to spare the round-trip the daemon's own numbers can
        // predict — a nag is the cheapest turn the session takes, and the most
        // pointless one to spend on a prompt already known not to fit. MEASURED
        // 2026-10-09: the LAST of the thirty refusals on `s-1789919514688401228`
        // was attributed to `todo check` — this door.
        self.compact_if_at_the_wall();
        self.trail.begin_turn();
        self.trail.say(Speaker::Agent, &text, Some(Instant::now()));
        self.submit_item(TranscriptItem::User {
            speaker: letibot_transcript::Speaker::Agent,
            parts: vec![UserPart::Text { text }],
        })
        .map(Some)
    }

    /// **A board row's status by its words**, as a word — for a nag's follow-up row (TODO R81):
    /// the turn a check opened touched the row it named when this answers differently after it.
    /// `None` is a row no longer on the board, which is a touch too.
    pub(crate) fn row_status(&self, content: &str) -> Option<String> {
        self.todos
            .snapshot()
            .into_iter()
            .find(|t| t.content == content)
            .map(|t| format!("{:?}", t.status).to_lowercase())
    }

    /// Every board row as `(words, status)` — the nag follow-up's read (TODO R81).
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

    /// **The operator's pause of this session's plan-check**, or `None` while it runs. See
    /// [`crate::nag_pause`].
    pub fn nag_pause(&self) -> Option<letibot_sessionlog::NagPause> {
        self.cfg.nags_paused
    }

    /// **The pause as the clock arms for it** — the wall-clock end turned into an `Instant` from
    /// now, read fresh at every arming so the clock never holds a pause this session no longer
    /// has. See [`crate::sessions::NagHold`].
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

    /// **`/nag`, on this session** — the report, the pause or the resume, as the verb's reply.
    /// The clock is its owner's to re-arm afterwards (`Sessions`' arm, `serve_child`'s): this
    /// half moves the session's fact and says it.
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

    /// **Pause this session's plan-check** — the config, the row and the hub, together, for
    /// [`Self::stand_down_auto_compact`]'s reason: a reader who finds one without the others has
    /// been lied to. The `nags_paused` row is the loud half; the settings row is what a head that
    /// attaches later reads.
    ///
    /// A row the store refused is SAID — in the hub row and in the `Err` the verb's reply carries
    /// — and the pause still holds in memory: it is real until this daemon stops, and the operator
    /// is told it will not outlast it.
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

    /// **End the pause** — by the verb, at its own deadline, or noticed at open after it ran out
    /// while the session was closed. The `nags_resumed` row says which.
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

    /// **A resume comes back with the operator's pause**, from the session's own row — called
    /// at open, for every session that opens, children included.
    ///
    /// **The row is the answer even when it says nothing**: a child's config is a clone of its
    /// parent's, and a parent's pause must not ride into it (see [`crate::nag_pause`]), so a
    /// session with no row, or no store, opens running. A pause whose end passed while nobody
    /// held the session is ended here and said, rather than restored as a pause nothing would
    /// ever lift.
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
