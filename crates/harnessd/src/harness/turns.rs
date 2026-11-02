//! **The turn loop**: submit, stream, dispatch a round's tool calls, append their
//! results and resubmit — with the retry ladder, outage and fallback handling
//! around it, and the restart restore that starts a turn nobody typed for.
use super::*;

/// A `LogSink` that also hands back the item ids it saw.
///
/// See point 1 in the module header. It forwards everything unchanged: a sink that
/// filtered would make the head's view depend on which daemon published it.
pub(crate) struct CapturingSink {
    inner: LogSink,
    ids: Vec<String>,
    /// The last `turn_id` this sink saw start.
    ///
    /// Read for §4.5: when a turn fails, the daemon has to publish a terminal event
    /// *for that turn*, and `HarnessError` does not carry an id — the failure is
    /// raised in several places, one of them before a turn id exists at all. Taken
    /// off the event on its way past, for the same reason the item ids are: the
    /// minting rule lives in the engine and a second copy of it goes stale.
    turn_id: Option<String>,
}

impl CapturingSink {
    pub(crate) fn new(hub: Arc<Hub>) -> Self {
        CapturingSink {
            inner: LogSink::new(hub),
            ids: Vec::new(),
            turn_id: None,
        }
    }

    fn take_ids(&mut self) -> Vec<String> {
        std::mem::take(&mut self.ids)
    }
}

impl EventSink for CapturingSink {
    fn emit(&mut self, event: TurnEvent) {
        match &event {
            TurnEvent::TranscriptAppended { item_id, .. } => self.ids.push(item_id.clone()),
            TurnEvent::TurnStarted { turn_id, .. } => self.turn_id = Some(turn_id.clone()),
            _ => {}
        }
        self.inner.emit(event);
    }
}

/// **Pair every id a sink captured with its row's body** — the free half of
/// `Harness::reconcile`, so the summary-turn helper (which cannot borrow `&self`,
/// see [`Harness::run_summary_turn`]) pairs rows by the same rule and cannot drift
/// from it.
///
/// The ids come off the events rather than being recomputed; see point 1 in the
/// module header. A count mismatch is a bug in the pairing, not something to paper
/// over, so it is announced as a warning on the same log the head reads.
pub(crate) fn reconcile_rows(
    hub: &Arc<Hub>,
    sink: &mut CapturingSink,
    items: &[TranscriptItem],
) -> Vec<String> {
    let ids = sink.take_ids();
    if ids.len() != items.len() {
        hub.publish(letibot_sessionlog::SessionEvent::Warning {
            code: "record_item_pairing".into(),
            detail: format!(
                "{} TranscriptAppended events for {} items; the head will show \
                 empty rows. This is a daemon bug, not a transport one.",
                ids.len(),
                items.len()
            ),

            compaction: None,
        });
    }
    for (id, item) in ids.iter().zip(items) {
        hub.record_item(id, item.clone());
    }
    ids
}

// ---------------------------------------------------------------------------
// The authorisation trail
// ---------------------------------------------------------------------------

impl Harness {
    /// **The loop.** One user message in, one answer out, however many tool rounds
    /// that takes.
    ///
    /// This is the one door the **operator's own words** come through, and it is the
    /// only place that records them as such: see [`TrailMirror`] for why the speaker
    /// cannot be recovered from the transcript afterwards.
    pub fn submit(&mut self, text: &str) -> Result<Reply, HarnessError> {
        self.submit_spoken(Speaker::Operator, text)
    }

    /// **The operator's words with the chips they put beside them** — `PROTOCOL_VERSION` 50.
    ///
    /// The files are read HERE, by the daemon, because the daemon is the half that writes the
    /// transcript: the row is the durable artifact, and what it carries is the `data:` URI and
    /// the inlined text rather than a path that may be gone by the time the next round renders
    /// the conversation (`letibot_transcript::media`'s own argument for putting `Media` on a
    /// row). See [`crate::attachments`] for what each kind becomes.
    ///
    /// A file that could not be read is said in the row and warned on the log — the words still
    /// send, because they are the operator's and the attachment is not — and an EMPTY list is
    /// [`Harness::submit`], byte for byte.
    pub fn submit_attached(
        &mut self,
        text: &str,
        attachments: &[letibot_sessionlog::protocol::Attachment],
    ) -> Result<Reply, HarnessError> {
        if attachments.is_empty() {
            return self.submit(text);
        }
        let (extra, refused) = crate::attachments::parts(attachments);
        for said in &refused {
            self.warn("attachment_refused", said.clone());
        }
        self.submit_spoken_parts(Speaker::Operator, text, extra)
    }

    /// **One turn, with the speaker given** — the operator's own words through
    /// [`Harness::submit`], a parent's correction through
    /// [`Harness::submit_a_parents_message`].
    ///
    /// The speaker is not decoration. [`AuthorisationTrail`] is where a turn's authority is
    /// decided and [`Speaker::Agent`] never authorises on its own, so a row that says the wrong
    /// speaker is a row that grants the wrong thing — which is why the parent's message has a
    /// door of its own rather than a flag on this one. The two speaker vocabularies are matched
    /// inline here for the same reason `TrailMirror::seed` matches them inline: the one place
    /// they meet is the one place a reader has to look for the mapping.
    pub(crate) fn submit_spoken(
        &mut self,
        speaker: Speaker,
        text: &str,
    ) -> Result<Reply, HarnessError> {
        self.submit_spoken_parts(speaker, text, Vec::new())
    }

    /// The same, with the parts an attachment became — see [`Harness::submit_attached`].
    fn submit_spoken_parts(
        &mut self,
        speaker: Speaker,
        text: &str,
        extra: Vec<UserPart>,
    ) -> Result<Reply, HarnessError> {
        self.trail.begin_turn();
        // **R79: the results held for this break ride in with the turn** — as the harness's
        // own row, BEFORE the words that started the turn, so what the operator (or a parent)
        // said is still the last thing the model reads.
        let mut items = Vec::new();
        if let Some(meanwhile) = self.deliveries().and_then(|d| d.at_turn_start()) {
            self.trail
                .say(Speaker::Agent, &meanwhile, Some(Instant::now()));
            items.push(TranscriptItem::User {
                speaker: letibot_transcript::Speaker::Agent,
                parts: vec![UserPart::Text { text: meanwhile }],
            });
        }
        self.trail.say(speaker, text, Some(Instant::now()));
        items.push(TranscriptItem::User {
            // The same speaker the trail records, one line up (R42).
            speaker: match speaker {
                Speaker::Operator => letibot_transcript::Speaker::Operator,
                // A session talking to itself, a tool's row, a parent's message: none of
                // them is the operator's words, and none may be drawn as them.
                Speaker::Agent | Speaker::Tool => letibot_transcript::Speaker::Agent,
            },
            // **The words first and whole, the chips after** — a head retires its `queued`
            // echo by the row's leading lines, so nothing may be spliced in front of them.
            parts: {
                let mut parts = vec![UserPart::Text { text: text.into() }];
                parts.extend(extra);
                parts
            },
        });
        self.submit_items(items)
    }

    /// **The shared tail has taken a turn on this harness** — `turn_tail::run_turn`, once per door,
    /// before the door's own act.
    ///
    /// It says only that there is a turn to end, not that one has started speaking: what starts the
    /// clock is the turn's first words ([`Harness::begin_turn_clock`]), and [`Harness::end_turn_clock`]
    /// clears this again. A turn taken and never ended would be exactly the state
    /// `Harness::system_update`'s own rounds would leave behind, which is why it is the tail's act
    /// rather than every caller's.
    pub(crate) fn turn_taken(&mut self) {
        self.turn_open = true;
    }

    /// **A prompt has arrived: this is where the whole turn's clock starts.**
    ///
    /// Called once per prompt, never per round — the engine's `turn_began_ms` is then carried on
    /// every `TurnStarted` that prompt produces, which is what stops a head restarting its clock
    /// at each round. The engine field is private to this module and this is the only writer, so
    /// the two cannot drift out of step.
    ///
    /// **Idempotent within one prompt**, and that is load-bearing now that the two callers are the
    /// places a turn actually speaks: [`Harness::submit_items_then`] (which a wall-continuation
    /// re-enters, one prompt later, with this still running) and [`Harness::run_rounds`] (which the
    /// operator's `!` line reaches with its rows already in). A second call leaves the start where
    /// the first put it, so the promise above holds without either caller having to know.
    ///
    /// **And it is NOT called by the door.** `turn_tail::run_turn` used to start it before the
    /// door's own act, which meant a door that found nothing to say — `Harness::wake` with no notice
    /// behind it, `Ok(None)` — published `waiting on: turn` for a turn that never ran; a `wait`
    /// blocked on that session armed on it and answered `0 — the turn ended` about a session that
    /// had run nothing at all. MEASURED 2026-10-26 as the bridge's end-to-end flake, one run in
    /// four: the empty wake was the daemon's own startup ring
    /// (`Sessions::put_a_restored_session_to_work`) draining on the worker's first pass, on the
    /// session that had never turned.
    pub fn begin_turn_clock(&mut self, began_ms: u64) {
        // **A settlement taken for this turn is no longer owed** — the words are in the rows this
        // turn opens with, whether or not the clock below starts. See
        // `JobWatchers::settlement_in_flight`.
        if let Some(w) = &self.job_watch {
            w.turn_began();
        }
        // **Only a turn the shared tail is running can start a clock** — see the field. A
        // `run_rounds` reached outside one (`Harness::system_update`'s, from a compaction or the
        // `m1` script) has no `end_turn_clock` behind it, and a clock nobody clears is a session
        // that reads `waiting on: turn` for the life of the daemon. And a door that has not spoken
        // yet has not started a turn either: the clock waits for the first words.
        if !self.turn_open || self.prompt_in_flight.load(Ordering::SeqCst) {
            return;
        }
        self.turn_began_ms = Some(began_ms);
        self.engine.turn_began_ms = Some(began_ms);
        // **And a prompt is in flight from here until the loop returns** — see the field. The
        // engine's rounds come and go inside this, which is exactly why a reader that times the
        // round events reads a working session as idle at every boundary.
        self.prompt_in_flight.store(true, Ordering::SeqCst);
        // **And the room is told the session is WORKING** — the beat, run by the daemon and not by
        // the model. A model that must remember to call `beat` is thinking about its work, and the
        // ninety-second lease would take its item while it works; the turn clock is the daemon's,
        // so this is where *working* starts and [`Harness::end_turn_clock`] is where it stops.
        // Idempotent, and a session with no room pays one `Option` test.
        if let Some(room) = &self.room {
            room.turn_started();
        }
        self.publish_activity();
    }

    /// The turn is over: stop publishing a start, so a later round that belongs to a NEW prompt
    /// cannot inherit this one's clock.
    ///
    /// **And this is where a switch the operator made during the turn takes effect** — see
    /// [`Harness::set_provider`], and [`Harness::apply_pending_switch`], which runs after the
    /// span is marked closed and before anything else reads what answers. Every way out of a
    /// turn comes through here (`turn_tail::run_turn`: a final answer, an interrupt, a failure,
    /// a wall, and the door that said nothing at all), which is exactly why the pending switch
    /// is applied HERE and not on a success path.
    pub fn end_turn_clock(&mut self) {
        self.turn_began_ms = None;
        self.engine.turn_began_ms = None;
        // The turn the tail took is over — see the field. Cleared here rather than in `run_turn`
        // because this is the call every path out of the tail goes through, the empty one included.
        self.turn_open = false;
        // The prompt's loop has returned — a final answer, an interrupt, a failure, or a wall that
        // ended it. Whatever the session is waiting on from here is said by the rest of the facts
        // (`delivery`, a job, a monitor, a child, a card) and not by a turn.
        self.prompt_in_flight.store(false, Ordering::SeqCst);
        // **And a turn that ends owes nothing** — one that never opened the batch it took hands the
        // results back with `put_back`, and this keeps a take from outliving its turn.
        if let Some(w) = &self.job_watch {
            w.turn_began();
        }
        // **After the flag, deliberately**: the switch is applied through the same door a
        // between-turns switch takes, and a gate that saw the span still open would pend it again
        // for ever.
        // **The turn is over, so the beats stop.** The lease now counts from the last one, which is
        // exactly the fact it measures: a session whose turn ended is alive and not working on the
        // item — the operator's *"so they don't occupy it for ever"*, without the model having to
        // do anything.
        if let Some(room) = &self.room {
            room.turn_ended();
        }
        self.apply_pending_switch();
        self.publish_activity();
    }

    /// **Something fired while nothing was running.** T24's wake, from the worker —
    /// and, since R7, **the same door a background job's completion comes through.**
    ///
    /// The daemon calls this when [`letibot_sessionlog::registry::Work::Woken`]
    /// names this session. Every monitor that settled since the cursor, and every
    /// background job the watcher has queued a completion for, becomes one user item
    /// and the loop runs — so a condition that happened between turns is acted on,
    /// rather than sitting in `job_list` until the model happens to ask.
    ///
    /// `Ok(None)` means nothing had settled after all: a wake that raced a mid-turn
    /// pickup by [`HubSteering`], or a job settlement this harness had already turned
    /// into a turn, or a wake with no completion behind it for any other reason. That
    /// is a real outcome and not a failure, and returning `None` rather than running
    /// an empty turn is what stops a spurious wake from costing a generation.
    ///
    /// It returns `Ok(None)` even when there are no monitors: a session that can
    /// background a job and watches no condition still has completions to deliver,
    /// and the early return that used to sit on `self.monitors` is what would have
    /// made R7 silently monitor-only.
    pub fn wake(&mut self) -> Result<Option<Reply>, HarnessError> {
        // **The merge queue's reviews first**, and they are never a turn of this session: a
        // gatekeeper started, or a verdict written, is the queue's business. See
        // [`Harness::serve_reviews`]. A wake is one of the three doors into it and not the only
        // one any more: a session inside a turn is served at its round boundary, and a session
        // this daemon holds is served by the daemon's own clock whatever its turn state.
        let _ = self.serve_reviews();
        // **R92.24: a wake on a workspace that has gone** — a worktree retired under a live
        // session. Said on the hub once per harness, and to the model when this wake has
        // something to say anyway (a wake for this alone would be a turn whose every tool call
        // fails).
        let gone = !self.cfg.workspace.is_dir();
        if gone && !self.workspace_gone_said {
            self.workspace_gone_said = true;
            self.hub.publish(SessionEvent::Warning {
                code: WORKSPACE_GONE.into(),
                detail: workspace_gone(&self.cfg.workspace),
                compaction: None,
            });
        }
        // **Two kinds of thing arrive between turns, and they share one turn.**
        //
        // A monitor that fired, and a background job that ended. Both are the machine's
        // reading of the world with nobody having asked, both must reach the model
        // unprompted, and neither may jump a person who is waiting: that ordering is
        // `Bell::next_any`'s — commands drain before wakes — and it holds for both
        // because both arrive as `Work::Woken`.
        // **And the queue's entries that wait for a merge gate**, said to this session's model —
        // the operator: *"let main project agent manage it"*.
        // **A wake is where a monitor's firing (or expiry) is seen**, so the store's record of
        // what is watching is brought up to date here (`crate::restart`).
        self.sync_monitor_records();
        // **Every sentence below goes through the `notice` hook** (`docs/plugins-plan.md` §4.3):
        // one kind per source, because a layer that patches the wording of a firing is not thereby
        // the answerer for the queue's verdicts. It is applied per source and not to the joined
        // text, so the facts a layer is handed are the ones its sentence was composed from.
        let queue = self.queue_notices();
        let mut notices = self.noticed_all(
            letibot_plugin::notice::MERGE_QUEUE,
            queue,
            serde_json::json!({}),
        );
        // **The room's bells** (the room's slice 2). A room event about this session became a
        // sentence on the connection's own thread — `crate::room::wake` — because the thread that
        // hears a bell is not the thread that runs a turn. This is where it reaches the model, as
        // one notice per event, through the same `notice` hook as every other sentence the harness
        // composes: a layer may reword a room notice and may not delete it (it is a fact the model
        // is owed — somebody else's claim on a resource it also wants).
        let room = crate::room::wake::take(&self.cfg.session_id);
        notices.extend(self.noticed_all(letibot_plugin::notice::ROOM, room, serde_json::json!({})));
        // **And the children a restart brought back** — the operator's *"we should bring
        // subagents back … and restart automatically"*: the model is told, in one line each,
        // which came back and which could not. Counted with the queue's notices: both are facts
        // the daemon found in the store rather than settlements that arrived.
        let restart_taken = self.restart_notices.clone();
        let restarts = std::mem::take(&mut self.restart_notices);
        notices.extend(self.noticed_all(
            letibot_plugin::notice::RESEAT,
            restarts,
            serde_json::json!({}),
        ));
        // **And the entries the queue has handed to this session**, which is the fallback: no
        // reviewer could judge them, and the project merges such a branch through its own main
        // session (`[queue] merge_via_session`), so the project that owns the branch is told — and
        // this is the telling. NOT a refused review: a refusal goes to the child that made the
        // branch through `mergequeue::review::deliver_refusal`, and one that cannot be delivered
        // is raised as `refusal_undelivered`, never handed over here. Second, and
        // after the gate notice, because an entry can be in only one of the two states: the first
        // is *the repository has no gate*, the second is *the queue has given the entry back to
        // you*, and the one that matters to the model reading them is the one that is true.
        let handover = self.handover_notices();
        notices.extend(self.noticed_all(
            letibot_plugin::notice::MERGE_QUEUE,
            handover,
            serde_json::json!({}),
        ));
        // **And a target emergency, or a row parked for a person** (R76 item 1) — the one the
        // queue used to say only on the hubs.
        let emergency = self.queue_emergency_notices();
        notices.extend(self.noticed_all(
            letibot_plugin::notice::MERGE_QUEUE,
            emergency,
            serde_json::json!({}),
        ));
        let queue_n = notices.len();
        let cursor_before = self.monitor_cursor.load(Ordering::SeqCst);
        if let Some(monitors) = self.monitors.clone() {
            let since = self.monitor_cursor.load(Ordering::SeqCst);
            let settled = monitors.settled_count();
            if settled > since {
                let fired: Vec<_> = monitors.firings().into_iter().skip(since).collect();
                self.monitor_cursor.store(settled, Ordering::SeqCst);
                // The monitors pane's table moved with it: a fire, an expiry or a retire.
                self.publish_jobs();
                if !fired.is_empty() {
                    // Immediate by R79's rules, and recorded as such.
                    if let Some(d) = self.deliveries() {
                        d.log_firings(&fired);
                    }
                    if let Some(text) = monitor_notice(&fired) {
                        let facts = monitor_facts(&fired);
                        notices.extend(self.noticed(letibot_plugin::notice::MONITOR, &text, facts));
                    }
                }
            }
        }
        // **The hop that did not exist (R7).** `JobSettled` reaches every head; this is
        // what reaches the model, and taking it here is what stops it arriving twice.
        //
        // **And it is THIS session's queue that is drained, which is the routing fix of
        // 2026-10-05.** The queue is per session: a job a subagent backgrounded is queued
        // on the subagent's set and drained by the subagent, so the notice says *you
        // backgrounded* to the one session that did. Sharing the tree's queue (R58) is
        // what put a subagent's `sleep 25` in the main session's conversation, where the
        // handle was not in `job_list` and `job_output` could not open it. The one thing
        // that still arrives here from below is a settlement whose own session stopped
        // before it could be drained ([`JobWatchers::stop`]) — and `completion_notices`
        // is given this session's id so that it is not read as this session's own.
        //
        // **R79: between turns nothing is held** — a wake runs no turn (`Ok(None)` below) only
        // when there is truly nothing to say. Holding is for a RUNNING turn; see `crate::delivery`
        // and the operator's ruling quoted on `Deliveries::between_turns`.
        let monitor_n = notices.len() - queue_n;
        let deliveries = self.deliveries();
        // **A gatekeeper's settlement is the review's, not this session's news.** The verdict
        // went onto the queue's row above; telling this session's model that "a subagent you
        // started has finished" would be a turn about work that is already recorded and already
        // gone from the board. It holds for the QUEUE's session too, and for the same reason — the
        // pass above ran first and the row is answered (see the field's own doc).
        let gatekeepers = &self.gatekeepers;
        // The sentences, and which kind gets which — [`completion_notices`], which is where that
        // decision lives so that it can be asserted without a live session.
        //
        // **And nothing stays held between turns** (the operator, 2026-10-11: *"jobs were ran for
        // a reason and held for a reason. once turn ends no reason to hold"*): a result that
        // arrived while the session was idle is said here as an ordinary completion, and the batch
        // held while the last turn ran is this wake's own content — the turn-end ring
        // (`JobWatchers::turn_ended`) is what brought the worker here for it.
        //
        // **Not marked read yet (R91.11).** `between_turns` marked them delivered — in the store,
        // for a subagent — and emptied the held set before `submit_item` had put a word in front
        // of the model; a turn that then failed to open lost them. They are marked once the turn
        // has opened, and held again if it does not.
        let between = deliveries.as_ref().map(|d| {
            d.between_turns_unread(
                |c| gatekeepers.contains(&c.job),
                crate::delivery::Rule::TurnEnd,
            )
        });
        let mut held_batch = None;
        let mut said: Vec<JobCompletion> = Vec::new();
        if let Some((b, told)) = between {
            let now = self.noticed_all(letibot_plugin::notice::JOB, b.now, serde_json::json!({}));
            notices.extend(now);
            held_batch = b.batch;
            said = told;
        }
        // **And the rows whose condition just became met.** The same door, because a condition
        // firing is the same kind of fact as a job ending: nothing asked, and the model has to be
        // told. What the line carries is BOTH halves — the row's own words and what the world says
        // about its handle — because either alone is unusable: an intent with no fate is a sentence
        // the model cannot act on, and a fate with no intent is the settlement notice it already
        // has.
        let result_n = notices.len() - queue_n - monitor_n;
        let due = self.due_todo_rows();
        let fired: Vec<String> = due.iter().map(|(_, handle, _)| handle.clone()).collect();
        if !due.is_empty() {
            let lines: Vec<String> = due
                .iter()
                .map(|(text, handle, fate)| format!("  - {text} — {}", fate_line(handle, fate)))
                .collect();
            // R93.C9: "{n} row you attached a condition to is due" was wrong for n > 1.
            let due_line = format!("[todo] Due (its condition is met):\n{}", lines.join("\n"));
            notices.extend(self.noticed(
                letibot_plugin::notice::TODO,
                &due_line,
                serde_json::json!({"rows": due.len()}),
            ));
        }
        let causes = [
            queue_n,
            monitor_n,
            result_n,
            due.len(),
            usize::from(held_batch.is_some()),
        ];
        // **The held batch last**, after everything said now.
        let held: Vec<String> = match held_batch.as_deref() {
            Some(text) => self
                .noticed(letibot_plugin::notice::JOB, text, serde_json::json!({}))
                .into_iter()
                .collect(),
            None => Vec::new(),
        };
        notices.extend(held);
        // **A wake with nothing to say runs no turn**, so nothing is owed for what the take
        // removed — and this has to be cleared BEFORE the publish below, or the value a reader
        // holds says delivery about a turn that is not coming.
        if notices.is_empty()
            && let Some(w) = &self.job_watch
        {
            w.turn_began();
        }
        // **What this session is waiting on has moved** (`PROTOCOL_VERSION` 52): the monitor cursor
        // above took the firings, and `between_turns_unread` took the queue and the held set — none
        // of which publishes an event of its own. Said before the early return below, because the
        // move that matters most is the one that empties the list: a wake with nothing left to say
        // is a session that has just gone quiet.
        self.publish_activity();
        self.meter_wake(causes, !notices.is_empty());
        if notices.is_empty() {
            return Ok(None);
        }
        if !self.cfg.workspace.is_dir() {
            let line = format!("[workspace] {}.", workspace_gone(&self.cfg.workspace));
            if let Some(line) = self.noticed(
                letibot_plugin::notice::WORKSPACE,
                &line,
                serde_json::json!({}),
            ) {
                notices.insert(0, line);
            }
        }
        let text = notices.join("\n\n");
        // **The wall is checked BEFORE the send — `nag_turn`'s pre-flight, the
        // same function.** MEASURED 2026-10-09: the FIRST of the thirty
        // consecutive provider refusals on `s-1789919514688401228` was
        // attributed to `monitor` — this door — so the insurance is not
        // theoretical. As with the nag, this is not the load-bearing fix: a
        // refused wake is classified as the wall and recovered by the turn tail
        // (`Sessions::wake` calls it), and this pre-flight is what spares the
        // refused round-trip when the daemon's own ledger already says the next
        // turn will not fit.
        self.compact_if_at_the_wall();
        // The harness talking to itself, not the operator. A firing — or a job's end —
        // must never be able to authorise the action it reports on.
        self.trail.begin_turn();
        self.trail.say(Speaker::Agent, &text, Some(Instant::now()));
        let mut opened = false;
        let out = self.submit_items_then(
            vec![TranscriptItem::User {
                // The same speaker the trail records, one line up: this is the session talking
                // to itself, and a head must not draw it as the operator's words (R42).
                speaker: letibot_transcript::Speaker::Agent,
                parts: vec![UserPart::Text { text }],
            }],
            |_| opened = true,
        );
        // **Told only once the turn has opened (R91.11).** Opened means the notice is in the
        // transcript and in the store, so the model reads it whatever its rounds come to. A turn
        // that never opened put nothing in front of anybody: the results go back to the held
        // set, the restart notices back to the harness, and the monitor cursor back to where it
        // was, so the next wake says all of it.
        if opened {
            if let Some(d) = &deliveries {
                d.read(&said);
            }
        } else {
            if let Some(d) = &deliveries {
                d.put_back(said);
            }
            let mut back = restart_taken;
            back.append(&mut self.restart_notices);
            self.restart_notices = back;
            self.monitor_cursor.store(cursor_before, Ordering::SeqCst);
        }
        out.map(Some)
            // ...and the condition is spent only once the model HAS been told. Consuming before the
            // submit would let a turn that failed to start silently eat the firing, and **a firing
            // nobody hears is the failure this whole mechanism exists to rule out**, where a firing
            // reported twice is merely dull. So the order here is the requirement and not tidiness.
            .inspect(|_out| {
                if !fired.is_empty() {
                    self.todos.consume_conditions(&fired);
                    // Unwritten, the condition is still pending in the store and fires again after
                    // a restart — which is why this is said on the hub and not only on stderr.
                    self.flush_todos_or_say("a fired condition");
                }
            })
    }

    /// **A subagent brought back after a daemon restart carries on** — the operator, 2026-10-12:
    /// *"we should bring subagents back - i cant afford to wait till they finish. we should record
    /// them (session persist anyway) and restart automatically"*.
    ///
    /// The session was reopened from the store under its own id, so the conversation is the
    /// child's own; what it lacks is the turn that was in flight. This is that turn, opened by
    /// `notice` (`crate::subagent_record::child_notice`) in the harness's own voice — the shape of
    /// [`Harness::continue_after_wall`]: `Speaker::Agent`, never an authorisation, never readable
    /// as the operator's words. A child of THIS child that was brought back too is said in the
    /// same turn ([`Harness::restore_after_restart`]), and so is anything held for it.
    pub fn continue_after_restart(&mut self, notice: &str) -> Result<Reply, HarnessError> {
        let mut parts = vec![notice.to_string()];
        parts.append(&mut self.restart_notices);
        if let Some(meanwhile) = self.deliveries().and_then(|d| d.at_turn_start()) {
            parts.push(meanwhile);
        }
        let text = parts.join("\n\n");
        self.trail.begin_turn();
        self.trail.say(Speaker::Agent, &text, Some(Instant::now()));
        self.submit_item(TranscriptItem::User {
            speaker: letibot_transcript::Speaker::Agent,
            parts: vec![UserPart::Text { text }],
        })
    }

    /// **A settlement this daemon queued for a session that was not open is not lost.**
    ///
    /// # The hole this closes
    ///
    /// [`crate::jobwatch::JobWatchers`] is built **per open**, so a settlement that lands while
    /// its session is not open has nowhere to go: the queue it is pushed into belongs to a
    /// harness nobody will drain again, and the ring that would have started the turn names a
    /// session `Sessions::wake` answers `Gone` for (`sessions::wake_route`). Both die with the
    /// harness. What survives is the **row**, because it is written where the settlement is first
    /// known rather than where it is read: `subagent_run` by the child's own thread
    /// ([`crate::subagent_record::SubagentRecord::settled`]) and `job` by
    /// [`crate::jobwatch::JobRecorder::record`] — each with `delivered = 0`, which is that
    /// column's whole meaning (*the model has not been told*).
    ///
    /// This is the door that reads it, at the one moment it is true: the session has just been
    /// opened, so its watcher set is new and empty and nothing in this process is holding the
    /// settlement. Queued as an ordinary [`JobCompletion`], it is worded by the same
    /// `completion_notices` a live settlement is — *a subagent you started has finished: …* — so
    /// a settlement that outlived its session is not a second kind of notice. The open's own
    /// ring (`Sessions::put_a_restored_session_to_work`) is what makes the turn.
    ///
    /// # Only this daemon's own, and that is the other half rather than a narrowing
    ///
    /// `instance` is the daemon that last owned the row. A row a **gone** daemon wrote is
    /// [`Harness::restore_after_restart`]'s (`runs`/`settled` below, which filter on
    /// `instance != me`), and it is worded as a restart because that is what it was. A row
    /// **this** daemon wrote and never delivered is what the open of a session it had closed
    /// finds, and it is worded as a completion because nothing restarted. The two filters are
    /// disjoint, so no settlement is delivered twice.
    ///
    /// # What it cannot do, said rather than implied
    ///
    /// A job's output is gone by now — `job_output` reads the process host's own capture, and
    /// the process is long reaped — so the notice names the job and says how it ended, which is
    /// what `job_output`'s absence leaves true. A child's answer IS in the row (`outcome`), and
    /// the notice carries its first line exactly as a live settlement does.
    ///
    /// # What it returns
    ///
    /// How many settlements it queued. A queued settlement is a notice the next wake says, and a
    /// session whose ONLY news is one of them is a session that still owes work — which is
    /// [`Harness::restore_after_restart`]'s decision about the restart sentence's door, so the
    /// count has to leave this function rather than be recomputed there.
    fn deliver_settlements_left_undelivered(&self, store: &Store, me: &str, sid: &str) -> usize {
        let Some(watch) = self.job_watch.clone() else {
            return 0;
        };
        let mut queued = 0usize;
        // **The children first** — this row's own subject, and the one the operator went
        // looking for: a `task` child that answered into a session nobody was holding.
        for run in store.subagent_runs_of(sid).unwrap_or_default() {
            if run.instance != me
                || run.delivered
                || run.parent_id != sid
                || crate::subagent_record::is_helper(&run)
                || !matches!(run.state.as_str(), "done" | "failed")
            {
                continue;
            }
            watch.queue(JobCompletion {
                memory: None,
                kind: BackgroundKind::Subagent,
                // The session that started it, which is this one — the sentence is *a subagent
                // YOU started*, and it is true: the row's `parent_id` is this session.
                owner: sid.to_string(),
                job: run.child_id.clone(),
                command: String::new(),
                slug: Some(crate::subagent_record::recorded_slug(&run)),
                state: run.state.clone(),
                produced: 0,
                elapsed_ms: 0,
                detail: crate::jobwatch::first_line(run.outcome.as_deref().unwrap_or_default()),
                arrived: std::time::SystemTime::now(),
            });
            queued += 1;
        }
        // **And the jobs**, whose rows the recorder writes for the same reason and with the same
        // `delivered = 0`. A child's handle has a `running` job row too (the sink writes one for
        // every `Backgrounded`, before it knows which kind it is), so it is skipped here the way
        // `restore_after_restart` skips it.
        for job in store.undelivered_jobs(sid).unwrap_or_default() {
            if store.subagent_run(&job.handle).ok().flatten().is_some() {
                continue;
            }
            let Some(ctx) = store.job_context(sid, &job.handle).ok().flatten() else {
                // No context means no daemon is named, so there is nothing that says this row is
                // this daemon's — and a row nobody can attribute is `restore_after_restart`'s
                // (a relic) rather than a settlement this door dropped.
                continue;
            };
            if ctx.instance != me {
                continue;
            }
            watch.queue(JobCompletion {
                memory: None,
                kind: BackgroundKind::Job,
                owner: sid.to_string(),
                job: job.handle.clone(),
                command: job.command.clone(),
                slug: ctx.slug.clone(),
                state: job.state.clone(),
                produced: job.produced,
                elapsed_ms: job.elapsed_ms,
                detail: String::new(),
                arrived: std::time::SystemTime::now(),
            });
            queued += 1;
        }
        if queued > 0 {
            eprintln!(
                "  {sid} · {queued} settlement(s) this daemon queued while the session was not \
                 open are said at this open"
            );
        }
        queued
    }

    /// **What this session was running when its daemon stopped — restored, or reminded** — called
    /// once, when the session is opened (`Sessions::harness_as`, `Sessions::open_first_with_seat`,
    /// and a resumed child's own `run_to_completion`). See `crate::restart` for the whole of it.
    ///
    /// The operator, 2026-10-12: *"to make restarts as much painless as possible - by default we
    /// restore agents, monitors, and remind the model about jobs. Now, if some threshold passed -
    /// say more than 1 hours after the stop, we start to simply remind the model stating that time
    /// passed and this is what you wre running and how to restart that"* — and *"this is the case
    /// for when harnessd stopped of course"*: only records a daemon that is GONE wrote are read, so
    /// a session opened by the daemon that holds its records (an attach after a long idle, a
    /// `/resume`) does nothing here.
    ///
    /// Everything it decides is one notice: queued for this session's next turn
    /// ([`Harness::restart_notices`] — the daemon rings the session right after the open, so that
    /// turn is now), published on the hub as `restart_restored` or `restart_reminded` for the
    /// operator, and printed on the daemon's log. Every child resumed or re-delivered is watched by
    /// this session's own watcher set, so its completion arrives exactly as a fresh child's does.
    ///
    /// `None` when there was nothing to do — the ordinary open.
    pub fn restore_after_restart(&mut self) -> Option<String> {
        use letibot_tools::builtins::task::{RestartFate, RestoreMode};
        let path = self.cfg.store.clone()?;
        let store = Store::open(&path).ok()?;
        let me = self.session_registry.instance().to_string();
        let back_ms = self.session_registry.started_ms();
        // **When this session is being opened** — the clock the restore threshold is applied to
        // (R91.8): the operator's rule is *"more than 1 hours after the stop"*, and a session
        // opened hours after the daemon came back is exactly the case the threshold exists for.
        let opened_ms = letibot_sessionlog::event::now_ms();
        let sid = self.cfg.session_id.clone();
        // **Before anything about a restart, the settlements THIS daemon dropped** — see
        // [`Harness::deliver_settlements_left_undelivered`]. It lives here because this is the
        // door every open passes through (`Sessions::harness_as`, `Sessions::open_first_with_seat`,
        // a resumed child's `run_to_completion`) and it is the door whose own ring, a few lines
        // later in `Sessions`, is what turns a queued settlement into a turn. The count is kept
        // because a settlement nobody has been told about is one of the things that makes an
        // otherwise empty restart a TURN ([`crate::restart::Owed`]).
        let settlements = self.deliver_settlements_left_undelivered(&store, &me, &sid);

        // **What a gone daemon left**, and which daemon that was. Read before anything is
        // touched, so the down time is decided once for the whole session.
        let runs: Vec<_> = store
            .subagent_runs_of(&sid)
            .unwrap_or_default()
            .into_iter()
            .filter(|r| {
                r.instance != me
                    // A tool's helper is never restored or re-delivered (`decide`); a running one
                    // is only closed, which needs no notice.
                    && !crate::subagent_record::is_helper(r)
                    && (r.state == "running"
                        || (matches!(r.state.as_str(), "done" | "failed") && !r.delivered))
            })
            .collect();
        // Helpers caught mid-task by the stop are closed quietly by the runner's pass below even
        // when nothing else in this session needs a notice.
        let retire_helpers = store
            .subagent_runs_of(&sid)
            .unwrap_or_default()
            .iter()
            .any(|r| {
                r.instance != me && r.state == "running" && crate::subagent_record::is_helper(r)
            });
        let monitors: Vec<_> = store
            .monitors_of(&sid)
            .unwrap_or_default()
            .into_iter()
            .filter(|m| m.live && m.instance != me)
            .collect();
        let mut jobs = Vec::new();
        // **A job row with no context is a relic of a build before `job_context`**, not a job
        // that was running at THIS stop: it names no daemon, so it cannot say when anything
        // stopped, and reminding the model of it as running is false. pg-noop, 2026-10-15: four
        // such rows (j900, j610, j624, j2437, from builds long gone) were the session's only
        // records, `Stopped::latest` over no daemon at all said the stop was unknown, and a
        // daemon down a couple of seconds told the model *"when the previous one stopped is
        // unknown … NOTHING was restored automatically"* over four jobs nobody was running.
        // They are closed quietly below, the way a reminded job is, and said to nobody.
        let mut relics = Vec::new();
        for job in store.jobs(&sid).unwrap_or_default() {
            // **A job the old daemon's ORDERLY stop ended is a job that was running at the
            // stop**, and is reminded like one. `Registry::close` ends every run in flight
            // (`end_running`, *"killed by the daemon stopping"*) before the process exits, so
            // after a clean restart no job row reads `running` at all — and the reminder this
            // pass exists to give was never given: pg-noop's `slt-sweep` (j17239), 4,262 of
            // 4,771 files into its measurement, killed at 02:05:55 and mentioned to nobody.
            if job.state != "running" && job.state != crate::restart::STOPPED_WITH_THE_DAEMON {
                continue;
            }
            // A subagent's handle is backgrounded through the same sink and has a `job` row too;
            // it is the child record's business, not a job's.
            if store.subagent_run(&job.handle).ok().flatten().is_some() {
                continue;
            }
            let ctx = store.job_context(&sid, &job.handle).ok().flatten();
            if ctx.as_ref().is_some_and(|c| c.instance == me) {
                continue;
            }
            if ctx.is_none() && job.command.is_empty() {
                continue;
            }
            match ctx {
                Some(ctx) => jobs.push((job, ctx)),
                None => relics.push(job),
            }
        }
        // **Jobs that ENDED and were never delivered** (TODO.md R91.5): a result held under R79,
        // or queued, when the daemon stopped. Only a gone daemon's — one this daemon holds is
        // still in its own queue — and only with a context, which is what names the daemon.
        let mut settled = Vec::new();
        for job in store.undelivered_jobs(&sid).unwrap_or_default() {
            if store.subagent_run(&job.handle).ok().flatten().is_some() {
                continue;
            }
            let Some(ctx) = store.job_context(&sid, &job.handle).ok().flatten() else {
                continue;
            };
            if ctx.instance == me {
                continue;
            }
            settled.push((job, ctx));
        }
        for job in relics {
            let mut lost = job.clone();
            lost.state = "lost at restart".into();
            let _ = store.put_job(&sid, &lost);
            let _ = store.deliver_job(&sid, &job.handle);
        }
        // **A restart is SAID, whatever the store holds** — the operator, 2026-10-30, after one
        // restart had told the model something and the next had said nothing: *"mb always tell
        // agent about restart? with the current time lol"*. That is a clock and not a courtesy:
        // the model's sense of time went through a stop and a start, and only this daemon knows
        // when. A session that existed before this daemon started was held by a daemon that is
        // gone — which is the whole of what `came_back` asks, and it is asked of the session ROW
        // because a session with nothing running has no other record to ask.
        // **And a daemon actually stopped here.** A session older than this daemon is not by itself
        // a restart: a seeded session, a resumed one, one this daemon opened first — all predate
        // `back_ms` while nothing stopped. What makes it a restart is another daemon's `daemon_life`
        // row carrying a stop, which is a fact about the box rather than an inference from a clock.
        let a_daemon_stopped = store
            .lives()
            .map(|lives| {
                lives
                    .iter()
                    .any(|l| l.stopped_ms.is_some() && l.instance != me)
            })
            .unwrap_or(false);
        // **And only the sessions a person reads.** The review queue's own session is opened by
        // this daemon too, and it is nobody's conversation: the operator saw the restart twice —
        // once for his session and once for the gatekeeper's — and asked for one
        // (*"i dont need two"*). A subagent is nobody's either; it reads what its parent tells it.
        let a_persons_session = !letibot_sessionlog::is_gatekeeper_session(&sid)
            && !matches!(store.session_parent(&sid), Ok(Some(Some(p))) if !p.is_empty());
        // **The session's own row, read once**, because it answers two questions: whether this
        // session existed before this daemon started (so a daemon that is gone held it), and what
        // its last row was — which is the only evidence a session that was mid-turn when the
        // daemon stopped leaves behind ([`crate::restart::mid_turn`]).
        let row = store.session(&sid).ok().flatten();
        let came_back = a_daemon_stopped
            && a_persons_session
            && row
                .as_ref()
                .is_some_and(|s| (s.created_ms.max(0) as u64) < back_ms);
        // **Once per (session, restart)** — `queue_told` is the store's own "said once" table and
        // the key names the daemon, so a session this daemon opens twice says it once, and the
        // NEXT daemon says it again because it is a different restart. Spent on the first open
        // whatever that open says, so an empty reopen cannot say what the first one already did.
        // A store that cannot record it is treated as *not yet said*: a sentence said twice is
        // dull, and one never said is the defect this exists for.
        let already_said = came_back
            && !store
                .tell_once(&format!("restart:{sid}:{me}"), back_ms)
                .unwrap_or(true);
        if runs.is_empty() && monitors.is_empty() && jobs.is_empty() && settled.is_empty() {
            if retire_helpers {
                let _ = self.subagents.resume_recorded(RestoreMode::Remind);
            }
            if !came_back || already_said {
                return None;
            }
            // **What this session still owes decides the door its sentence takes** — the
            // operator, 2026-11-01: *"it is not ok that restart notice arrived with my continue"*.
            // Nothing owed keeps the row (a generation spent on a sentence is still a generation
            // spent); anything owed makes the sentence the content of the turn the work already
            // costs. See [`crate::restart::Owed`].
            let owed = self
                .owed_after_a_restart(row.as_ref().and_then(|s| s.last_item.as_ref()), settlements);
            return self.say_nothing_was_running(&store, back_ms, &owed);
        }

        // **How long the daemon was down**: from the latest stop among the daemons these records
        // came from, to this daemon's start — the fact the notice's first line states. **The
        // threshold is applied to a different interval** (R91.8): stop → this open
        // (`restores(opened_ms, …)`), because a session opened hours after the daemon came back is
        // the case the threshold is for.
        let mut instances: Vec<(String, u64)> = Vec::new();
        for (inst, at) in runs
            .iter()
            .map(|r| (r.instance.clone(), r.updated_ms))
            .chain(monitors.iter().map(|m| (m.instance.clone(), m.updated_ms)))
            .chain(jobs.iter().map(|(_, c)| (c.instance.clone(), c.started_ms)))
            .chain(
                settled
                    .iter()
                    .map(|(_, c)| (c.instance.clone(), c.started_ms)),
            )
        {
            match instances.iter_mut().find(|(i, _)| *i == inst) {
                Some((_, t)) => *t = (*t).max(at),
                None => instances.push((inst, at)),
            }
        }
        // **Every record here names a daemon** (a relic does not, and was closed above), so the
        // stop is UNKNOWN only when one of those daemons kept no life row — no clock, not "down
        // for 0s" — and an unknown down time restores nothing (`restart` module docs).
        let stop = crate::restart::Stopped::latest(
            instances
                .iter()
                .map(|(i, at)| crate::restart::stop_of(&store, i, *at))
                .chain(std::iter::once(latest_box_stop(&store))),
        );
        let restore = stop.restores(opened_ms, self.cfg.restore_within);
        // Whether anything WAS brought back — which decides the notice's first line and its code.
        let mut restored_any = false;
        let mode = if restore {
            RestoreMode::Resume
        } else {
            RestoreMode::Remind
        };

        let mut lines: Vec<String> = Vec::new();
        let mut said: Vec<String> = Vec::new();

        // **Subagents.**
        for c in self.subagents.resume_recorded(mode) {
            let name = format!("`{}` ({})", self.subagents.label_of(&c.handle), c.title);
            match &c.fate {
                RestartFate::Resumed => {
                    restored_any = true;
                    if let Some(w) = &self.job_watch {
                        w.watch(c.handle.clone());
                    }
                    lines.push(crate::subagent_record::parent_resumed_line(&name));
                    said.push(crate::restart::said::resumed(&name));
                }
                RestartFate::Settled => {
                    restored_any = true;
                    // The settlement itself is the notice the watcher queues; a second line about
                    // it would tell the model the same thing twice.
                    if let Some(w) = &self.job_watch {
                        w.watch(c.handle.clone());
                    }
                    said.push(crate::restart::said::delivering(&name));
                }
                RestartFate::NotResumed { why } => {
                    lines.push(crate::subagent_record::parent_not_resumed_line(&name, why));
                    said.push(crate::restart::said::not_resumed(&name, why));
                }
                RestartFate::Reminded { worktree, branch } => {
                    lines.push(crate::subagent_record::parent_reminded_line(
                        &name,
                        &c.handle,
                        worktree.as_deref(),
                        branch.as_deref(),
                    ));
                    said.push(crate::restart::said::subagent_reminded(&name));
                }
            }
        }

        // **Monitors.** One that had already run out its TTL before the stop is closed quietly:
        // it would have ended on its own, restart or none.
        let rt = self.runtime.clone();
        for m in monitors {
            let _ = store.end_monitor(&sid, &m.name);
            if m.declared_ms.saturating_add(m.ttl_ms) <= stop.no_earlier_than_ms() {
                continue;
            }
            if !restore {
                lines.push(crate::restart::monitor_reminded_line(&m));
                said.push(crate::restart::said::monitor_reminded(&m.name));
                continue;
            }
            // **A monitor a named scope owned is the model's to bring back**, however short the
            // stop: the scope was something it set up, the stopped daemon was the only one that
            // knew it, and whether it still matters is a judgement, not a replay.
            if let Some(scope) = crate::restart::named_owner(&m) {
                lines.push(crate::restart::monitor_scoped_line(&m, &scope));
                said.push(crate::restart::said::monitor_scoped(&m.name, &scope));
                continue;
            }
            // **The same call, made again.** It was gated when the model first made it; this
            // re-establishes that declaration rather than making a new one, so it goes through the
            // door with no gate (the operator's own runs use it for the same reason: nobody is
            // left to ask). The recording wrapper writes the new declaration under this daemon.
            let call = ToolCall {
                id: format!("restart-monitor-{}", m.name),
                name: "monitor".into(),
                arguments: m.args_json.clone(),
            };
            let r = rt.invoke_operator("restart", &call, &mut letibot_tools::NullToolSink);
            if r.outcome == letibot_transcript::ToolOutcome::Ok {
                restored_any = true;
                lines.push(crate::restart::monitor_rearmed_line(&m));
                said.push(crate::restart::said::rearmed(&m.name));
            } else {
                let why = r
                    .payload
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim_end_matches('.')
                    .to_string();
                lines.push(crate::restart::monitor_not_rearmed_line(&m, &why));
                said.push(crate::restart::said::not_rearmed(&m.name, &why));
            }
        }

        // **Jobs: reminded, never re-run.**
        let rows = self.todos.snapshot();
        for (job, ctx) in jobs {
            let slug = ctx.slug.clone();
            let waiting = crate::restart::rows_waiting_on(&rows, &job.handle, slug.as_deref());
            let ran = stop.at_ms().map(|at| at.saturating_sub(ctx.started_ms));
            lines.push(crate::restart::job_line(
                &job,
                slug.as_deref(),
                Some(ctx.cwd.as_str()),
                ran,
                &waiting,
            ));
            said.push(crate::restart::said::job(&job.handle, waiting.len()));
            // **Said once**: the row stops reading `running`, so the next open does not remind
            // again and the pane does not draw a job nobody runs.
            let mut lost = job.clone();
            lost.state = "lost at restart".into();
            let _ = store.put_job(&sid, &lost);
            // And said: a `lost at restart` row is an ending, and an undelivered ending is what
            // the next open would announce again.
            let _ = store.deliver_job(&sid, &job.handle);
        }

        // **Jobs that ended untold: their ending, said now** — whatever the down time, the way a
        // settled child's result is delivered past the threshold too: a result is not work to
        // redo, and nothing is re-run. Marked delivered once said, so it is said once.
        for (job, ctx) in settled {
            restored_any |= restore;
            lines.push(crate::restart::settled_job_line(&job, ctx.slug.as_deref()));
            said.push(crate::restart::said::job_settled(&job.handle));
            let _ = store.deliver_job(&sid, &job.handle);
        }

        let outcome = match (restore, restored_any) {
            (true, true) => crate::restart::Outcome::Restored,
            (true, false) => crate::restart::Outcome::NothingRestorable,
            (false, _) => crate::restart::Outcome::Reminded,
        };
        let text = crate::restart::notice(
            &crate::restart::header(&stop, back_ms, opened_ms, self.cfg.restore_within, outcome),
            &lines,
            outcome,
        );
        for line in &said {
            eprintln!("  {sid} · restart: {line}");
        }
        self.hub.publish(SessionEvent::Warning {
            code: outcome.code().into(),
            detail: crate::restart::warning_detail(&text, &said),
            compaction: None,
        });
        self.restart_notices.push(text.clone());
        Some(text)
    }

    /// **A restart with nothing to restore, said anyway** — the operator, 2026-10-30: *"mb always
    /// tell agent about restart? with the current time lol"*. The sentence is
    /// [`crate::restart::nothing_running`]'s; this is the reading behind it and the door it takes.
    ///
    /// # Which daemon stopped, from a session with no work in it
    ///
    /// The stop is dated from the daemons the session's OWN rows name, whatever state those rows
    /// are in: a monitor it retired, a child that finished, the last job it ran. A session that
    /// names nobody — created, never used, nothing run in it — has no record that could date a
    /// stop, and the sentence says `unknown` rather than borrowing the newest stop in the store,
    /// which belongs to whichever daemon on this box stopped last and is a claim about another
    /// folder's process.
    ///
    /// # The door, and which of the two it takes
    ///
    /// **A session that owes nothing** has nothing to continue, so the sentence is filed and the
    /// model reads it when it next reads ([`Harness::say_without_a_turn`]): no wake, no turn. A
    /// restore is the opposite — a resumed child is work to continue — and keeps the turn it has
    /// always had.
    ///
    /// **A session that owes something is the operator's ruling of 2026-11-01**: *"it is not ok
    /// that restart notice arrived with my continue … so i expected that after restart you will
    /// continue"*. The sentence is queued as a restart notice instead, which is the turn the
    /// daemon's own ring at open already costs for the work ([`crate::restart::Owed`], and
    /// [`Harness::owed_after_a_restart`] for what counts). The cost of the whole mechanism is one
    /// model call per restart per session that owed anything; a session that owes nothing keeps
    /// the row and spends none.
    fn say_nothing_was_running(
        &mut self,
        store: &Store,
        back_ms: u64,
        owed: &crate::restart::Owed,
    ) -> Option<String> {
        let sid = self.cfg.session_id.clone();
        let mut instances: Vec<(String, u64)> = Vec::new();
        let mut note =
            |instance: &str, at: u64| match instances.iter_mut().find(|(i, _)| i == instance) {
                Some((_, t)) => *t = (*t).max(at),
                None => instances.push((instance.to_string(), at)),
            };
        for m in store.monitors_of(&sid).unwrap_or_default() {
            note(&m.instance, m.updated_ms);
        }
        for r in store.subagent_runs_of(&sid).unwrap_or_default() {
            note(&r.instance, r.updated_ms);
        }
        // The newest job row is one query, and it is the row a session that ran nothing since
        // its last `bash` names its daemon by.
        if let Some(last) = store.jobs(&sid).unwrap_or_default().last()
            && let Some(ctx) = store.job_context(&sid, &last.handle).ok().flatten()
        {
            note(&ctx.instance, ctx.started_ms);
        }
        let stop = crate::restart::Stopped::latest(
            instances
                .iter()
                .map(|(i, at)| crate::restart::stop_of(store, i, *at))
                .chain(std::iter::once(latest_box_stop(store))),
        );
        // **The same sentence, and one clause when the session owes work** — a model woken to
        // *nothing was running* alone could not know why it is reading it now.
        let text = match owed.any() {
            true => crate::restart::nothing_running_but(&stop, back_ms, owed),
            false => crate::restart::header(
                &stop,
                back_ms,
                letibot_sessionlog::event::now_ms(),
                self.cfg.restore_within,
                crate::restart::Outcome::Nothing,
            ),
        };
        eprintln!("  {sid} · restart: nothing was running — {text}");
        // **One line, not two.** In the quiet case the sentence is written as a transcript row
        // ([`Harness::say_without_a_turn`]), which the person sees where the rest of the
        // conversation is and the model reads at its next turn. In the owed case it is queued as
        // a restart notice, which the wake spends a turn on and the person sees as that turn's
        // opening line. Neither publishes a hub warning: the row drew the restart twice for one
        // stop — the operator: *"i need only one restart notification"* — and the row is the copy
        // that both readers can use.
        if owed.any() {
            self.restart_notices.push(text.clone());
        } else if let Err(e) = self.say_without_a_turn(&text) {
            eprintln!("  {sid} · restart: the notice was not recorded ({e})");
        }
        Some(text)
    }

    /// **What this session still owes**, as [`crate::restart::Owed`] — the four facts that decide
    /// whether an otherwise empty restart is a turn.
    ///
    /// `last` is the session row's last transcript row (read once by
    /// [`Harness::restore_after_restart`]) and `settlements` is what
    /// [`Harness::deliver_settlements_left_undelivered`] queued at this open.
    ///
    /// The board is read twice on purpose: `due_rows` is the rows whose condition the world now
    /// meets ([`Harness::due_todo_rows`], the wake's own predicate, so the two cannot disagree)
    /// and `plan_rows` is every row still owed — the plan the model wrote, which is the one thing
    /// a session with no record of a turn leaves that says it has work left. A `Postponed` or
    /// `Cancelled` row is the operator's act and is not owed; a `Completed` one is answered.
    fn owed_after_a_restart(
        &self,
        last: Option<&TranscriptItem>,
        settlements: usize,
    ) -> crate::restart::Owed {
        let plan_rows = self
            .todos
            .snapshot()
            .iter()
            .filter(|r| matches!(r.status, TodoStatus::Pending | TodoStatus::InProgress))
            .count();
        crate::restart::Owed {
            mid_turn: crate::restart::mid_turn(last),
            settlements,
            due_rows: self.due_todo_rows().len(),
            plan_rows,
        }
    }

    /// **Tell this session's model something without starting a turn** — the door a restart with
    /// nothing owed takes (`crate::restart`), and the two states it has to tell apart.
    ///
    /// * **A turn is running** → the step boundary of THAT turn (`self.injected`, the queue the
    ///   harness's own nudges ride): the model reads the sentence in the turn already open, and
    ///   nothing is preempted.
    /// * **Idle** → a transcript row, which the model reads at its next turn — whenever that is.
    ///   **No wake**: a wake is a model call, and a call spent to say *"the daemon came back at
    ///   21:28"* to a session that owes nothing is a call spent on a sentence. What the line is
    ///   for is closing a gap in the model's sense of time, and that gap is closed whenever it
    ///   next reads.
    ///
    /// **And that is the cost argument's whole reach.** It decides the door for a session with
    /// nothing outstanding; a session that owes work takes the turn it already costs
    /// ([`Harness::say_nothing_was_running`], [`crate::restart::Owed`]), because there the person
    /// is waiting for the session to continue and a row is read when they type — the operator,
    /// 2026-11-01: *"it is not ok that restart notice arrived with my continue"*.
    ///
    /// A row rather than an injection in the idle case, deliberately: an injection is read at a
    /// step boundary and there is no step boundary to wait for, while a row is in the transcript
    /// whether or not a turn ever runs — the same reason [`Harness::note_to_model`] queues rather
    /// than speaks.
    pub(crate) fn say_without_a_turn(&mut self, text: &str) -> Result<(), HarnessError> {
        let Some(text) = self.noticed(letibot_plugin::notice::RESEAT, text, serde_json::json!({}))
        else {
            return Ok(());
        };
        if self.prompt_in_flight.load(Ordering::SeqCst) {
            self.injected
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push_back(text);
            return Ok(());
        }
        self.record_notice(&text)
    }

    /// **A turn is running, as far as this test is concerned** — the one state
    /// [`Harness::say_without_a_turn`] branches on that a test cannot reach by running a turn: a
    /// turn holds the harness for its whole life, so nothing else can call in while one is in
    /// flight. See `tests::restart_notice`.
    #[cfg(test)]
    pub(crate) fn hold_a_turn_open_for_test(&self, open: bool) {
        self.prompt_in_flight.store(open, Ordering::SeqCst);
    }

    /// **The turn a fallback earned, opened by the notice that says so** — the operator's ruling
    /// of 2026-10-12 (*"when it finally applied the turn didnt continue"*), built as an act.
    ///
    /// `turn_tail::run_turn` is the only caller, and it makes this a whole turn of its own through
    /// the same sequence every door gets: the refusal ended the last one, and this one starts with
    /// a user item in the harness's own voice (`Speaker::Agent`, as a wake's and a plan-check's
    /// notices are) — which is also what makes the work legal for the model that took over, since
    /// a model's first request has to follow a user message.
    ///
    /// **The held batch rides in**, by R79's rule: this is a turn the model takes anyway, so the
    /// results it is owed at a turn start go with it rather than waiting for a prompt.
    pub fn continue_after_fallback(&mut self, notice: String) -> Result<Reply, HarnessError> {
        let mut text = notice;
        if let Some(meanwhile) = self.deliveries().and_then(|d| d.at_turn_start()) {
            text.push_str("\n\n");
            text.push_str(&meanwhile);
        }
        // The wall is checked BEFORE the send, like every other door: a session that inherited a
        // refusal is not more likely to fit, and a first round refused for LENGTH would be read as
        // another provider failure.
        self.compact_if_at_the_wall();
        self.trail.begin_turn();
        self.trail.say(Speaker::Agent, &text, Some(Instant::now()));
        self.submit_item(TranscriptItem::User {
            speaker: letibot_transcript::Speaker::Agent,
            parts: vec![UserPart::Text { text }],
        })
    }

    /// **The continuation turn a fallback owed, taken — at most once, and never over a stop.**
    ///
    /// `None` is the ordinary case. Two ways to get it back empty, and both are the point:
    ///
    /// * the turn that just ended was itself a fallback continuation — see
    ///   [`crate::turn_tail::run_turn`], which never takes one twice, so a fallback chain cannot
    ///   ping-pong between two names and pay a model change per turn for ever;
    /// * **the operator asked for a stop the failed turn did not take.** An interrupt that
    ///   arrived after that turn's last round sits in the queue with nothing to act on it, and a
    ///   fresh turn started on top of it would either be aborted by it or, worse, ignore their
    ///   stop. Their word ends the work here, and the session stays on the fallback model.
    ///
    /// **It always clears the field**, taken or not: a notice left armed would open a turn nobody
    /// asked for at the end of some later turn.
    pub(crate) fn take_fallback_continuation(&mut self) -> Option<String> {
        let notice = self.fallback_continuation.take()?;
        if self.hub.has_queued_interrupt() {
            return None;
        }
        Some(notice)
    }

    /// **The turn a fallback earned is running** — the one-shot, seen from inside it: read by
    /// [`Harness::fall_back_from`], which moves the session to the next name in the list but does
    /// not arm a second continuation.
    pub(crate) fn in_fallback_continuation(&self) -> bool {
        self.continuation_of_fallback
    }

    /// Marked by `turn_tail::run_turn` around the continuation turn and cleared after it, so the
    /// flag can never outlive the turn it describes.
    pub(crate) fn set_fallback_continuation(&mut self, on: bool) {
        self.continuation_of_fallback = on;
    }

    /// A prompt through the seam every session has, which is the one a child was missing.
    ///
    /// The wall is detected in the harness, between one round's tool results and the next round's
    /// send, so a child stops at the wall exactly as a root does. What REACTS to it, compacting and
    /// continuing on the summary, lived only in `Sessions`, and a child never gets there: its runner
    /// drives `submit` on the thread its parent spawned it on, and it is adopted into the registry
    /// and not into `Sessions::open`. So the child could not reach the protection the notice
    /// promised.
    ///
    /// This adds exactly the tail the daemon gives its own sessions: the wall is checked before the
    /// send, a turn that comes back at the wall is compacted and continued under the one bound every
    /// door runs, and every announcement is the same line a daemon session gets.
    ///
    /// There is one tail and not two: this is [`crate::turn_tail::run_turn`] with no owner around
    /// it, and the daemon's sessions run the same function with `Sessions` as the owner.
    pub fn submit_as_a_normal_session(&mut self, prompt: &str) -> Result<Reply, HarnessError> {
        self.submit_spoken_as_a_normal_session(Speaker::Operator, prompt)
    }

    /// **A parent's message to a child BETWEEN turns — the turn it makes, spoken as the
    /// parent's.**
    ///
    /// The operator's ruling, verbatim: *"fix task_message - it should enqueue"*. Until this
    /// existed, a message could only enter a turn that was already running (`HubSteering`'s
    /// `Message` arm), and the parent's tool refused to send one to a child that was not —
    /// because *"nothing drains a child's queue between turns"*, so a queued message would
    /// have been accepted and heard by nobody. A child between turns is now reached: the
    /// message is queued, the child's own serving thread is woken (`Hub::wake_its_own_reader`),
    /// and this is the turn that reads it.
    ///
    /// **Why this is not [`Harness::submit`]**: the speaker. The words are recorded as the
    /// sender's — [`message_from_a_session`], naming which session — so they cannot authorise the
    /// act they race, cannot coalesce with the operator's own text, and cannot be taken back.
    /// That is the same rule the mid-turn door follows, and the same text: a session cannot tell
    /// which state it was in when the other end spoke.
    pub fn submit_a_parents_message(
        &mut self,
        from: &str,
        text: &str,
    ) -> Result<Reply, HarnessError> {
        let said = message_from_a_session(from, text, self.cfg.parent_session_id.as_deref());
        self.submit_spoken_as_a_normal_session(Speaker::Agent, &said)
    }

    /// **The turn and its tail, with the speaker given** — the body both doors above share, and
    /// since R68.4 nothing but the shared tail itself ([`crate::turn_tail::run_turn`]) with no
    /// owner around it ([`crate::turn_tail::Bare`]): the pre-wall check, the failure, the
    /// compaction after the turn (the refused door after a wall) and the continuation past it.
    /// This used to be a second copy of that sequence, and the copy was a step behind the
    /// daemon's: no failure published, a different `auto_compact_skipped` sentence.
    fn submit_spoken_as_a_normal_session(
        &mut self,
        speaker: Speaker,
        text: &str,
    ) -> Result<Reply, HarnessError> {
        let door = match speaker {
            Speaker::Operator => crate::turn_tail::Door::Prompt,
            _ => crate::turn_tail::Door::Message,
        };
        crate::turn_tail::run_turn(self, &mut crate::turn_tail::Bare, door, |h| {
            h.submit_spoken(speaker, text).map(Some)
        })
        .expect("a spoken turn always runs")
    }

    /// **A parent's or peer's message as a turn, bare** — the words
    /// [`Harness::submit_a_parents_message`] speaks, without the tail around them, for a caller
    /// that runs the tail itself ([`crate::turn_tail::run_turn`]).
    pub fn parents_message_turn(&mut self, from: &str, text: &str) -> Result<Reply, HarnessError> {
        let said = message_from_a_session(from, text, self.cfg.parent_session_id.as_deref());
        self.submit_spoken(Speaker::Agent, &said)
    }

    /// §5.3: change the system prompt mid-session, in the form the dialect can
    /// render.
    ///
    /// **Appended, never rewritten.** Rewriting message 0 cost a full cold
    /// re-prefill of a 179k conversation when measured; that is what this method
    /// exists not to do.
    pub fn system_update(&mut self, text: &str) -> Result<Reply, HarnessError> {
        self.system_updates += 1;
        // A turn, so distances stay right — but **not** an utterance. A system
        // update is the operator changing the instructions, not the operator
        // authorising an action, and letting it into the trail would make
        // "be helpful with files" readable as consent to touch one.
        self.trail.begin_turn();
        let item = self
            .cfg
            .dialect
            .wiring(self.cfg.effort.as_deref())
            .system_update(self.system_updates, text);
        self.submit_item(item)
    }

    /// **Compaction.** One summary turn, then the fork.
    ///
    /// The turn asks for the summary over the prefix the server is already holding
    /// — that is the whole cache argument, and [`run_compaction`] carries it. If
    /// the model answers by proposing tool calls, the fork is refused: a summary is
    /// a record, not an action, and a summary turn that *worked* is a turn that did
    /// something with nobody watching. The instruction and the turn stay in the
    /// transcript either way — nothing is reduced until the fork lands, so a
    /// refusal here leaves the session exactly as it was, and the next attempt
    /// appends a fresh instruction over this one.
    /// **Who answers this session's summary turns.**
    ///
    /// The provider when there is one, so a compaction is rendered, counted and
    /// billed as part of the conversation it is summarising. Compaction used to
    /// call the engine's local path unconditionally, which sent a metered
    /// session's whole history to the daemon's own model — and a 1.5M-token
    /// history to a 262144-token server is a 400, which is why a conversation
    /// that had overrun could not compact its way out.
    ///
    /// Borrowed from three separate fields, so the caller can still take
    /// `&mut self.engine` and `&mut self.session` beside it.
    pub(crate) fn answerer<'p>(
        provider: &'p Option<Box<dyn letibot_backend::MessagesBackend>>,
        prefix: &'p StablePrefix,
    ) -> letibot_turn::compaction::Answerer<'p> {
        match provider {
            None => letibot_turn::compaction::Answerer::Local,
            Some(p) => letibot_turn::compaction::Answerer::Provider {
                backend: p.as_ref(),
                system: &prefix.system,
                // **The prefix's own tools, so the summary is sent under the SAME
                // prefix as every other turn in this conversation.** They are not here
                // so the summary can call one — nothing executes what it proposes.
                // They are here so a local server's cached prefix still matches and
                // the largest call in the session is not a cold prefill. See
                // `Answerer::Provider`'s docs.
                tools_json: &prefix.tools_json,
            },
        }
    }

    pub(crate) fn submit_item(&mut self, item: TranscriptItem) -> Result<Reply, HarnessError> {
        self.submit_items(vec![item])
    }

    /// [`Harness::submit_item`] for a turn that opens with more than one row — the held batch
    /// before the words that started the turn (R79).
    fn submit_items(&mut self, items: Vec<TranscriptItem>) -> Result<Reply, HarnessError> {
        self.submit_items_then(items, |_| {})
    }

    /// [`Harness::submit_items`], with `opened` called once the rows are in the transcript AND
    /// the store — the point past which the model reads them whatever the rounds come to — and
    /// not at all when the turn never got there (R91.11: `Harness::wake` marks results told
    /// there, not before).
    fn submit_items_then(
        &mut self,
        items: Vec<TranscriptItem>,
        opened: impl FnOnce(&mut Self),
    ) -> Result<Reply, HarnessError> {
        // **The prompt's clock starts before the rows go in** — from the moment a turn has words
        // to give the model it is in flight, and the append below publishes, so a head that was
        // waiting on this session reads `turn` and not a quiet moment between the queue and the
        // model. Idempotent: a wall-continuation comes back through here with the clock already
        // running. See [`Harness::begin_turn_clock`] for why the door does not start it.
        self.begin_turn_clock(letibot_sessionlog::event::now_ms());
        let mut sink = CapturingSink::new(self.hub.clone());
        self.session.append_items(&self.engine, &items, &mut sink)?;
        self.reconcile(&mut sink, &items);
        self.persist()?;
        opened(self);
        self.name_from_first_message();
        let out = self.run_rounds();
        self.note_turn_ended();
        out
    }

    /// **A turn's rounds are over** — counted on the watcher set's read record, which is how the
    /// plan check's grace after an ending tells *delivered in the turn that just ended* from
    /// *delivered a turn ago* (`JobWatchers::grace_left`). Called by both doors into
    /// `run_rounds`, whatever the rounds came to: a turn that failed still ended.
    pub(crate) fn note_turn_ended(&self) {
        if let Some(w) = &self.job_watch {
            w.turn_ended();
        }
    }

    /// Give an unnamed session a name, **once**, from the message that opened it.
    ///
    /// `Config::title`'s note said a title must not be derived from a prompt, and the
    /// reason it gave is the one this respects: *"a title guessed from content is a
    /// title that changes under you, and a session picker whose rows rename
    /// themselves is a picker you cannot learn."* The defect there is the **changing**,
    /// not the deriving. This fires on the first user item of a session that has no
    /// title, writes it to the store, and can never fire again — a second user message
    /// finds a title already set and leaves it alone. So a row's name is stable from
    /// the moment it first has one, which is the property that made the objection.
    ///
    /// It is a default, not a decision: `/rename` and `letibot --rename` overwrite it,
    /// and an operator who names a session up front never sees this run at all.
    fn name_from_first_message(&mut self) {
        if !self.cfg.title.is_empty() {
            return;
        }
        // Read, not remembered. A head can name this session through the registry's
        // own connection at any moment, and this harness's `cfg.title` would still be
        // empty — so a derivation that trusted its own copy would overwrite the name
        // the operator had just typed, with a few words from a message they sent
        // before they typed it.
        if let Some(store) = &self.store
            && let Ok(Some(t)) = store.title(&self.cfg.session_id)
        {
            self.cfg.title = t;
            return;
        }
        let first_user = self.session.items.iter().find_map(|i| match i {
            TranscriptItem::User { parts, .. } => parts.iter().find_map(|p| match p {
                UserPart::Text { text } => Some(text.as_str()),
                _ => None,
            }),
            _ => None,
        });
        let Some(text) = first_user else { return };
        let title = derive_title(text);
        if title.is_empty() {
            return;
        }
        if let Some(store) = &self.store
            && let Err(e) = store.set_title(&self.cfg.session_id, &title)
        {
            {
                // Not fatal and not silent: an unnamed session is usable, and a
                // store that refused a title is a fact about the store.
                self.hub.publish(letibot_sessionlog::SessionEvent::Warning {
                    code: "title_not_stored".into(),
                    detail: format!("this session could not be named in the store: {e}"),

                    compaction: None,
                });
                return;
            }
        }
        self.cfg.title = title.clone();
        self.hub
            .publish(letibot_sessionlog::SessionEvent::SessionRenamed {
                title: title.clone(),
            });
        self.new_title = Some(title);
    }

    /// A title this harness has just derived, for the daemon to put in the registry.
    ///
    /// The registry is what a head's picker is drawn from and the harness does not
    /// hold one — `Sessions` does. Handing it over rather than reaching for it keeps
    /// the harness free of the registry, which is what lets a `Harness` be built in a
    /// test with nothing but a `Hub`.
    pub fn take_new_title(&mut self) -> Option<String> {
        self.new_title.take()
    }

    /// Rename this session: the store, the log and this harness's own config.
    pub fn rename(&mut self, title: &str) -> Result<(), HarnessError> {
        if let Some(store) = &self.store {
            store
                .set_title(&self.cfg.session_id, title)
                .map_err(|e| HarnessError::Store(e.to_string()))?;
        }
        self.cfg.title = title.to_string();
        self.hub
            .publish(letibot_sessionlog::SessionEvent::SessionRenamed {
                title: title.to_string(),
            });
        self.new_title = Some(title.to_string());
        Ok(())
    }

    /// **Name the worktree and branch this session's work must end up on** — a `task_start`
    /// child's, set by its spawn. `None` clears it. See [`deliverable`].
    pub fn set_deliverable(&mut self, d: Option<deliverable::Deliverable>) {
        self.deliverable = d;
    }

    /// **A turn's rounds, and what is left of its steering when they end** (R91.1).
    ///
    /// The rounds hand back to [`Self::handed_back`] whatever a failed or interrupted round
    /// had taken out of the hub; while the turn runs, the next round's greedy poll takes it.
    /// When the turn is over:
    ///
    /// * **an interrupt is spent** — the turn it was pressed against has ended, and one
    ///   left behind would stop the NEXT turn at its first token;
    /// * **another session's message is WRITTEN INTO THE TRANSCRIPT** — see
    ///   [`Self::keep_the_words_it_never_read`], and `HubSteering`'s `Message` arm for why the
    ///   row and the injection are one item on every other path;
    /// * **everything else stays**, for the next turn's greedy poll: the operator's words, an
    ///   answer the board already took, results already marked delivered. A failed turn read
    ///   none of it, and the operator is told so.
    pub(crate) fn run_rounds(&mut self) -> Result<Reply, HarnessError> {
        // **The clock starts here for a door that appends no rows of its own** — the operator's
        // `!` line is already two rows on the log when `run_after_operator_shell` asks for its
        // turn, so this is the first point that turn exists. A no-op when `submit_items_then` has
        // already started it, which is every other door. See [`Harness::begin_turn_clock`] for
        // why a door that finds nothing to say must not have started it at all.
        self.begin_turn_clock(letibot_sessionlog::event::now_ms());
        let out = self.rounds();
        let (unread, left) = {
            let mut held = self.handed_back.lock().unwrap_or_else(|e| e.into_inner());
            held.retain(|m| !m.urgent);
            // **A session's message comes out of the carried set, because it is about to be a
            // row.** It is not re-offered by any source of its own, so leaving it here is the
            // one way a parent's words could be read twice (the next turn's greedy poll would
            // append the item a second time). See `SteeringMessage::from_session`.
            let mut unread: Vec<SteeringMessage> = Vec::new();
            held.retain(|m| match m.from_session.is_some() {
                true => {
                    unread.push(m.clone());
                    false
                }
                false => true,
            });
            (unread, held.len())
        };
        for m in &unread {
            if let Err(e) = self.keep_the_words_it_never_read(&m.text) {
                // Not fatal: the words are still on this session's log (the door recorded
                // them), and the turn's own outcome is what the caller is waiting for. Said,
                // because a row that was not written is a row a later reader will not find —
                // and **put back**, so the next turn's greedy poll carries them as the
                // injection they would have been rather than losing them to a store that was
                // busy at the wrong moment.
                self.handed_back
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push_back(m.clone());
                self.warn(
                    "message_not_stored",
                    format!(
                        "a message from another session was not written into this session's \
                         transcript: {e}. It is on this log, and it rides the next turn instead."
                    ),
                );
            }
        }
        if left > 0 {
            self.hub.publish(SessionEvent::Warning {
                code: "steering_carried".into(),
                detail: format!(
                    "{left} message(s) that reached this turn while it ran were not read by \
                     the model before it ended; they go in at the start of the next turn."
                ),
                compaction: None,
            });
        }
        out
    }

    /// **A message this turn never read, made a row anyway** — the one path on which a parent's
    /// words could reach a child's queue and leave no record of themselves.
    ///
    /// The ordinary path needs nothing: the engine appends the steering item at a step boundary
    /// and `run_rounds` persists it with the round's own rows (see `HubSteering`'s `Message`
    /// arm for the whole of that argument). This is the other path — a round that failed after
    /// the greedy poll had taken the message, which hands it back
    /// ([`letibot_turn::steering::SteeringSource::give_back`]) with no boundary ever arriving.
    /// The carried set survives a turn and would carry it into the next one, and that is enough
    /// for the operator's own prompt (a head is waiting on it, and the echo says `queued`). It
    /// is NOT enough for a session's message, for two reasons measured here:
    ///
    /// * **a child may never run another turn.** A parked child whose parent spoke while a
    ///   round was failing would hold the words in memory for ever — no row, no queue entry, and
    ///   nothing in the store for a later reader (the operator's own report: four
    ///   `task_message`s, *"of the child's 278 transcript rows, not one contains the parent's
    ///   words"*);
    /// * **the row is what makes the exchange visible at all.** A head attached to the child,
    ///   and the child's own conversation view, read the transcript and nothing else — so words
    ///   that exist only in a steering slot are words nobody can see.
    ///
    /// So the words are appended as the very item the injection would have been — same text,
    /// same `Speaker::Agent`, one `Text` part — and the next turn reads them as the history they
    /// now are. The trail is not told again: the door recorded the utterance when it took it
    /// (`HubSteering`'s `Message` arm), and a second `say` would be a second turn's worth of
    /// authority over one sentence.
    fn keep_the_words_it_never_read(&mut self, said: &str) -> Result<(), HarnessError> {
        let item = TranscriptItem::User {
            speaker: letibot_transcript::Speaker::Agent,
            parts: vec![UserPart::Text {
                text: said.to_string(),
            }],
        };
        let mut sink = CapturingSink::new(self.hub.clone());
        self.session
            .append_items(&self.engine, std::slice::from_ref(&item), &mut sink)?;
        self.reconcile(&mut sink, std::slice::from_ref(&item));
        self.persist()?;
        self.hub.publish(SessionEvent::Warning {
            code: "message_carried".into(),
            detail: format!(
                "a message from another session reached this turn while it ran and the turn \
                 ended before the model read it; it is in the transcript now, so the next turn \
                 reads it there: {said}"
            ),
            compaction: None,
        });
        Ok(())
    }

    /// **A turn needs somewhere to go, and a key to get there**: the two refusals `rounds`
    /// makes before it drains anything the operator typed.
    fn the_turn_has_somewhere_to_go(&mut self) -> Result<(), HarnessError> {
        // **The key first, before anything is drained or sent.** A provider with no key
        // is asked for one here; a refusal returns before the turn takes the operator's
        // message, so nothing they typed is spent on a turn that could not run.
        if self.key_wanted.is_some() {
            self.obtain_key(None)?;
        }
        // **And no model at all is said here too, at the same point and for the same reason.**
        // A daemon started with no GGUF and no provider (`Parts::load`) has nothing a turn can
        // go to: its own server would be sent byte ids it cannot read. The head opens its
        // model chooser before this is reached; this is the floor under a head that did not
        // (a one-shot, an older head), and it names the verb that ends it.
        if self.provider.is_none() && self.engine.vocab().is_bytes() {
            return Err(HarnessError::Setup(NO_MODEL_CHOSEN.to_string()));
        }
        Ok(())
    }

    /// **The wall, checked between two rounds rather than only at a turn's boundary** — and
    /// the whole turn stopped when it has been reached, which is the honest half a `Harness`
    /// can do on its own.
    fn stop_the_turn_at_the_wall(&mut self, round: usize) -> Result<(), HarnessError> {
        // The wall can arrive MID-TURN, and the turn boundary is too late: a tool result is
        // appended between rounds, so one big one can put the conversation past `n_ctx` while the
        // turn is still running, and the next round then sends a prompt the server refuses with the
        // whole turn lost and nothing recorded.
        //
        // This stops the turn instead of compacting in place, because building the new base reaches
        // the store, the resume chain and the registry, none of which a `Harness` owns. Stopping is
        // the honest half a `Harness` CAN do: the rounds so far are committed, `Sessions` compacts
        // at the boundary it already checks, and the next turn continues on the summary.
        //
        // `!self.compacting`: the compaction turn is exempt, because it is the one turn whose whole
        // purpose is to be over the wall, and stopping it there would make compaction impossible
        // exactly when it is needed.
        //
        // The count is measured before it is judged: the tool results of the round just ended are
        // in the ledger and not in the last round's count. And round 0 is checked against the window
        // itself, not the doors' reserve: the reserve is what let the prompt through in the first
        // place, and asking it again would stop every turn the doors had just allowed near the edge.
        let now = (!self.compacting).then(|| self.measure_resident());
        // **Not on an estimate of a folded base, at round 0, on a metered session**:
        // that count is the fork's re-based estimate, and the one round that would
        // replace it with the provider's is this one. Stopping it on the estimate
        // (pg-noop, 2026-10-11: ≈ 1,039,055, past the window; measured 507,355) is a
        // wall nothing could ever measure its way out of. If the provider counts it
        // over, it says so, and the refused door folds.
        let unmeasured_base = round == 0
            && self.provider.is_some()
            && self.cfg.wall.awaiting_on(&self.session.transcript_id);
        if !self.compacting
            && !unmeasured_base
            && let Some(window) = self.cfg.planning_window()
            && self.cfg.auto_compact
            && !self.cfg.wall.is_stuck()
        {
            let resident = self.session.ledger.len() as u64;
            let prefix = u64::from(self.session.ledger.prefix_len());
            if wall_stops_this_round(
                round,
                resident,
                prefix,
                self.cfg.headroom(),
                window,
                self.cfg.wall_basis(),
            ) {
                // In the units the operator's header shows, and the count the
                // decision was made on; see `Harness::measure_resident`. The
                // ledger's own figures are named once at the end rather than per
                // number, because three conversions in one sentence is not a
                // sentence.
                let (w, h) = self.cfg.shown_wall();
                let r = now.map(|n| n.tokens).unwrap_or(resident);
                let unit = self.token_unit();
                let here = self
                    .provider
                    .as_ref()
                    .map(|p| format!("{}/{}", p.name(), p.model()))
                    .unwrap_or_default();
                let also = if let Some(model) = self.cfg.carried_note() {
                    // **An estimate, named as one, with whose measurement it is.** This
                    // is the line the 2026-10-25 session needed and did not get: the
                    // count is the ledger's bytes converted by the ratio deepseek
                    // measured, not a number anything has confirmed for glm-coding.
                    format!(
                        " (estimated with {model}'s measured scale: no round has run on \
                             {here} yet, so this box's own ledger count is converted by the \
                             ratio {model} measured before the switch. One round here replaces \
                             it)"
                    )
                } else if self.cfg.tokens_are_converted() {
                    format!(
                        " (counted as the provider counts them; this box's own \
                             ledger says {resident} of {window}, and it over-counts \
                             because it holds the reasoning the provider is not sent)"
                    )
                } else if self.cfg.tokens_are_unscaled() {
                    // **The switch window, named rather than presented as calibrated.**
                    // No round has run on this model yet, so these are the ledger's own
                    // counts against a window measured on the model before it — and a
                    // reader who is not told cannot tell that from a measured sentence.
                    format!(
                        " (these are this box's own ledger counts: no round has run on \
                             {here} since it took over, so nothing has measured how its \
                             tokens relate to the ledger's. One round sets it)"
                    )
                } else {
                    String::new()
                };
                self.hub.publish(SessionEvent::Warning {
                    code: "context_wall".into(),
                    detail: format!(
                        "stopping this turn after {round} round(s): {r} of {w} \
                             {unit} are resident and the next round needs {h} free{also}. \
                             Everything so far is committed, and the session compacts \
                             before the next turn — this is the wall, not a failure of \
                             the work."
                    ),

                    compaction: None,
                });
                // **And it names the session, for the same reason the `compacting:` line
                // two steps below it does and only since 2026-10-03.** This is one log file
                // every daemon appends to, so a wall printed here and the compaction it
                // triggers (`sessions.rs`) land with other sessions' work between them —
                // and with no id either side, *which* session met the wall is unreadable,
                // which is exactly what left the operator's *"too much too early"*
                // unattributable. Measured 2026-10-03: a wall at 1.4M tokens followed by a
                // fold to 633k and a compaction to 8k, and no way to say whether those were
                // one session behaving differently or two behaving consistently. The id
                // costs fourteen bytes and makes the pair a measurement rather than an
                // adjacency — the same correction the `compacting:` line already had.
                let session_id = &self.cfg.session_id;
                eprintln!(
                    "  {session_id}: context wall: stopped after {round} round(s) at {r} of {w} \
                         {unit} ({resident} of {window} ledger tokens)"
                );
                return Err(HarnessError::ContextWall {
                    rounds: round,
                    resident: r,
                    window: w,
                    unit,
                    ledger: (resident, window),
                });
            }
        }
        Ok(())
    }

    fn rounds(&mut self) -> Result<Reply, HarnessError> {
        self.the_turn_has_somewhere_to_go()?;
        // How many times this turn has asked again after a refused key. Bounded, so a
        // provider that refuses every key does not hold the worker in a loop of cards.
        let mut key_asks = 0usize;
        let mut metrics = Vec::new();
        let mut tool_calls = 0usize;
        let mut truncated = false;
        // **The intent check nudges once per user turn, and the bound is the point.**
        //
        // T21.3's response is *"append 'you said you would X — do it or say why
        // not' and continue"*, and continuing means the answer turn is not the end
        // of the loop. What it must not become is a loop: a model that answers the
        // nudge with more prose and no tool call would produce a second finding, a
        // third, and burn `max_tool_rounds` on an argument. One is the error signal;
        // two is the harness insisting. If the model explains itself and stops, that
        // is a legitimate answer to the check — `docs/closed-loop.md`'s tolerance
        // band, set at its narrowest until somebody measures a better one.
        let mut nudges_left = 1usize;
        // **And a worktree child that is about to finish with its work outside its branch** —
        // `deliverable::EndOfWork`, the same budget of one: told what is uncommitted, once, and
        // then its answer stands (the parent's completion carries the line if it is still dirty).
        // The report: *"Seven times tonight a child has ended with its work outside its branch,
        // and each time the recovery was the same two commands."*
        let mut end_of_work = deliverable::EndOfWork::new();
        // **And a model that stops because "my context is full" when it is not** — R71,
        // `crate::context_claim`. The same budget as the nudge above, and for the same
        // reason: one line telling the model its measured fill, then — if it says it again
        // — one warning to the operator, never a second line to the model. Always on: it
        // is gated by a measurement, not by the prose heuristic `--intent-prose` guards.
        let mut context_check = crate::context_claim::Check::new();
        // **The encoder for "is this turn getting anywhere".**
        //
        // Per user turn, because "already seen" is a claim about *this* turn — a
        // file read in an earlier turn is a legitimate thing to read again now.
        // `max_tool_rounds` below is the backstop it demotes; see `crate::progress`
        // for why a round count was the wrong instrument and what replaced it.
        let mut progress = crate::progress::ProgressDetector::new(self.cfg.stall_rounds);
        // **The turn's standing-notes offers** — see `crate::notes_offer`, which is
        // where the whole argument lives. Per ROUND for the round's own offer, and
        // **per SESSION for the memory** — *"rare (once per note per turn at most)"*
        // was the wrong unit: a turn is minutes and a session is days, so a note
        // never read and never in the prompt won the same slot in turn after turn.
        // Measured 2026-10-10: 120 offers, 19 taken, one note offered 22 times, every
        // repeat a row in the operator's transcript. `recall` reads back what this
        // session has already been offered, so the offer is once per note per
        // SESSION, and does so from the log rather than from a set in memory —
        // see its own doc. The offer itself is per ROUND and lives in the request
        // assembly below, never in the transcript — *a row cannot be un-said; an
        // assembly can be recomposed*.
        let mut offers = crate::notes_offer::Offers::new();
        offers.recall(&self.cfg.daemon_logs(), &self.cfg.session_id);

        let backstop = round_backstop(self.cfg.max_tool_rounds);
        for round in 0..backstop {
            self.stop_the_turn_at_the_wall(round)?;
            let mut sink = CapturingSink::new(self.hub.clone());
            // **Compose this round's offer, before anything is sent.** It is scored
            // against the person's prompt and the turn's tool results — the design's
            // own subject — and handed to the engine as a trailing item that
            // `session.items` never sees. Nothing over the floor is said, and
            // nothing said is recorded: see `crate::notes_offer`.
            //
            // **Not on a compaction turn.** That turn is the harness asking for a
            // summary of what is already here; a note offered on top of it is a
            // fetch in the middle of a job whose whole instruction is to write, and
            // the offer would be settled against calls the summary turn never made.
            if !self.compacting {
                offers.compose(
                    &self.cfg.workspace,
                    &self.cfg.global_notes(),
                    &self.cfg.box_dirs,
                    &self.prefix.system,
                    &self.session.items,
                );
            }
            let tail = offers.tail();
            // **Take the round again when the endpoint is the thing that
            // failed.** An inner loop, so a retry does NOT spend one of
            // `max_tool_rounds`: it produced nothing and appended nothing, and
            // ending a turn early because a server was restarting would be the
            // budget measuring the wrong thing. See `letibot_http::retry`.
            //
            // # And the second question: is the operator's `/models` being heard?
            //
            // It is, and it is answered HERE — but it is not APPLIED here, and that
            // distinction is the whole of this loop's model rule.
            //
            // MEASURED 2026-10-12, the operator's GLM coding plan, quota exhausted:
            // `/models` sat in the hub's queue for a whole 63-second ladder and was
            // answered after the turn, by which time the turn was FAILED — *"while
            // those backoffs were cycling - the model change didnt take"*. So the turn
            // polls the queue itself and answers (`apply_queued_model`).
            //
            // MEASURED 2026-10-26, the other half: applying it here is what broke the
            // next session. The round was re-issued on deepseek carrying the rest of
            // GLM's turn — assistant tool-call rounds with no `reasoning_content` — and
            // deepseek's thinking mode refuses exactly that (`http 400: The
            // `reasoning_content` in the thinking mode must be passed back to the API`).
            // The operator's sentence for the rule: *"we can switch after turn ends"*.
            //
            // So: the switch is recorded and said (*"turns go to X when this turn
            // ends"*), every attempt of this turn is sent to the model the turn STARTED
            // on, and `end_turn_clock` applies it. `[fallback]` is the same rule for the
            // daemon's own switch — see `fall_back_from`.
            let mut tried_fallbacks: Vec<String> = Vec::new();
            // **One steering per round, outside the retry loop (R91.1).** A failed attempt
            // hands what it took back to this value, and the retried attempt's greedy poll
            // must find it there; one built per pass dropped it on the floor.
            let mut steering = self.steering();
            // **The ladder this turn walks, and the only thing that decides whether a round
            // is taken again** — see `letibot_http::retry`, where *what is retryable, and for
            // how long* is written down. It is also what the turn's ending is said from: the
            // count and the waiting are nobody else's to report, and a failure that reads as
            // `FAILED — http io: …` with no count is the report this was written for.
            let mut ladder = letibot_http::retry::Ladder::new(letibot_http::retry::Policy::new(
                self.cfg.http_retries,
            ));
            let outcome = loop {
                // **The operator's own line, answered before the attempt rather than after
                // it** — the window they type in. What comes back says when the switch
                // takes; nothing about this attempt moves.
                self.apply_queued_model();
                // **Start watching BEFORE the round, so the wait itself is covered.**
                // The host is `Some` only on the metered route — a local prefill's
                // silence is progress, not a hang; see `SilenceWatch`.
                let cloud_host = silence_host(self.provider.as_deref());
                let watch = SilenceWatch::start(
                    self.hub.clone(),
                    cloud_host,
                    std::time::Duration::from_secs(SILENT_ROUND_SECS),
                );
                let attempted = match &self.provider {
                    None => self.engine.run_turn_steered_with_tail(
                        &mut self.session,
                        &mut sink,
                        &mut steering,
                        &tail,
                    ),
                    // A cloud turn: the transcript as messages, the ledger as the
                    // record. Same events, same verdicts, same TurnOk.
                    Some(p) => self.engine.run_turn_messages_with_tail(
                        &mut self.session,
                        &mut sink,
                        &mut steering,
                        p.as_ref(),
                        &self.prefix.system,
                        &self.prefix.tools_json,
                        &tail,
                        None,
                    ),
                };
                // The round has produced something or failed, either way the silence
                // is over and the watchdog has nothing left to say.
                watch.finish();
                let Err(TurnFailure::Http(e)) = attempted else {
                    // **The new model answered**, which is the whole of what a switch had
                    // to prove — there is nothing to go back to any more.
                    self.switch_back = None;
                    break attempted;
                };
                // **A switch the operator made during this attempt is answered here, and
                // nothing else changes.** The wait below exists for an endpoint that is
                // coming back — a 5xx, a timeout — and a deliberate switch is not weather
                // either way: it is not this round's to apply, so the ladder goes on against
                // the model the turn started on and the switch takes when the turn ends.
                // Answered BEFORE the key arm deliberately: a 401 from the model they are
                // leaving is not a reason to ask them for a key to it.
                self.apply_queued_model();
                // **A refused key is asked for again**, on the same card, and the round is
                // taken again — nothing was recorded, so the retry sends the same bytes.
                // Only a key this session can replace: one from `$PROVIDER_API_KEY` or
                // `--api-key` wins over the file, so a typed one would be saved and ignored.
                if let letibot_turn::HttpError::Status {
                    code: code @ (401 | 403),
                    body,
                } = &e
                    && self.provider.is_some()
                    && key_asks < 2
                    && let Some(pc) = self.cfg.provider.clone()
                    && key_is_replaceable(&pc)
                {
                    key_asks += 1;
                    let why = format!("{} refused the key ({code}): {body}", pc.name);
                    self.provider = None;
                    self.key_wanted = Some(pc);
                    if let Err(e) = self.obtain_key(Some(why)) {
                        // **The turn ends here, so the outage does too.** Left in the field it
                        // would be continued by the NEXT turn's first failure, carrying this
                        // one's start time into a duration that never happened.
                        self.settle_outage(false);
                        return Err(e);
                    }
                    continue;
                }
                // **This attempt failed, and the outage it belongs to remembers how many
                // have** — the record is changed here and published nowhere: the row is
                // written once, when the outage ends. See [`Outage`]. After the key arm
                // deliberately: a key the provider refused is that card's news, not the
                // endpoint's weather.
                self.outage_failed(&e);
                let Some(wait) = ladder.failed(&e) else {
                    // **The ladder has nothing left to wait for, and the session may still
                    // have somewhere to go — but not this round.**
                    //
                    // The MODEL itself is what refused (a 429 or a 5xx, not a wire failure)
                    // and this box has a `[fallback] models` list, so `fall_back_from` names
                    // the first model that can answer and records it as the switch waiting
                    // for this turn to end. **The round is not re-issued there**, and that is
                    // the same rule as the operator's own `/models` above: a model's first
                    // request has to follow a user message, and the rest of this turn belongs
                    // to the model that refused it. So this turn ends here with the provider's
                    // own refusal, and the next one starts on the model the list names.
                    //
                    // A transport failure gets not even that: the model may be perfectly
                    // well and it is the route to it that is down, so moving the session
                    // would abandon the box the operator chose for a reason that is not
                    // about the model.
                    if let Some(next) = self.fall_back_from(&e, &mut tried_fallbacks) {
                        let was = self.provider_line();
                        // **Whether a turn was armed decides what may be promised.** This turn is
                        // the continuation a fallback earns whenever one is already running (the
                        // bound), and then no second turn is opened — so the sentence must not
                        // say one is.
                        let opens_a_turn = self.fallback_continuation.is_some();
                        // **The outage is this notice's to tell.** One row per outage, and
                        // this is the row: the session is moving, and the count belongs in
                        // the sentence that says so rather than in a second row beside it
                        // saying the same endpoint is out. Taken, so `settle_outage` below
                        // finds nothing left to write. See [`Outage`].
                        let outage = self.outage.take();
                        self.hub.publish(SessionEvent::Warning {
                            code: "model_endpoint_retry".into(),
                            detail: fallback_notice(&was, &next, &e, opens_a_turn, outage.as_ref()),

                            compaction: None,
                        });
                    }
                    break Err(TurnFailure::Http(e));
                };
                // **Nothing is published here, and that is the change.** This used to raise
                // `model_endpoint_retry` once per attempt — three rows in three seconds for
                // one endpoint, which is the operator's *"i dont need walls of this … a
                // coutner is what is need"*. The attempt is already in the outage's count
                // (above) and the row is written when the outage ends; see [`Outage`].
                if !self.sleep_unless_closed(wait) {
                    break Err(TurnFailure::Http(e));
                }
            };
            // **The outage this round was in, if any, is over — and it is said ONCE, here,
            // where the count is final.** A round that ended on anything but an HTTP
            // failure was answered: the endpoint is back, and that is the other half of the
            // news (a silent recovery is how somebody spends an afternoon believing it is
            // still down). See [`Outage`].
            self.settle_outage(!matches!(&outcome, Err(TurnFailure::Http(_))));
            // Before the match, deliberately: three of the four arms below leave
            // this function, and the one that matters most for §4.5 is the `Err(e)`
            // that propagates — a failure whose turn id is only recorded on the
            // success path is a failure the daemon cannot name.
            self.last_turn_id = sink.turn_id.clone().unwrap_or_default();
            let ok: TurnOk = match outcome {
                Ok(ok) => ok,
                // §5.7, and the notice goes where the model will read it.
                // Nothing was committed, so there is no tool-call row to answer
                // and the notice is a user item. Bounded by the engine's own
                // salvage budget: the round after this one either succeeds or
                // comes back as `SalvageExhausted`.
                Err(TurnFailure::BatchTruncated { notices, .. }) => {
                    self.append_notice(letibot_plugin::notice::SALVAGE, &notices.join("\n"))?;
                    continue;
                }
                // **The operator's own sentence, because it is the only one with evidence.**
                //
                // Measured in the transcript, 2026-09-18. The model overthought its way past the
                // output limit and said nothing. It was sent three machine notices in a row --
                // "continue or say why not", "Answer again, and put the answer before the
                // reasoning if you are close to the limit", then the first again -- and produced
                // three more empty turns. Then the operator typed:
                //
                //     you keep overthinking cut it short and do things
                //
                // and the very next reasoning block opened "The operator is frustrated. Let me
                // cut it short and just do the thing", followed by the work, in 34 tokens of
                // thinking instead of thousands.
                //
                // Why the old ones failed, specifically:
                //   * "continue or say why not" is an OPEN QUESTION. A model that just
                //     overthought is being invited to deliberate, and it accepts.
                //   * "if you are close to the limit" is a CONDITION it has to evaluate --
                //     more thinking -- and it is already true, so the hedge is pure cost.
                //   * Neither says the thing that worked: stop thinking, act.
                //
                // So: imperative, short, no question, no condition, and it names the behaviour
                // rather than the mechanism. A model does not need to be told about token
                // limits; it needs to be told what to do next.
                // R93.D1: it named the enum (`reasoning_only`), told models whose thinking
                // always comes first to "reason after", and accused on the first occurrence.
                Err(TurnFailure::EmptyLength { .. }) => {
                    self.append_notice(
                        letibot_plugin::notice::NUDGE,
                        "Your reply was all reasoning and nothing was kept. Make the tool call \
                         or give the answer now.",
                    )?;
                    continue;
                }
                // R7: the turn stopped inside its own reasoning block and said
                // nothing. GLM's end-of-turn token doubles as a nameable string,
                // so this is self-inflicted turn-ending, and the answer is the
                // same shape as the two arms above — tell the model, let the loop
                // run. Bounded by the engine's salvage budget exactly as they are:
                // once the cap is spent the engine returns `SalvageExhausted` and
                // this arm never sees another unfinished turn.
                Err(TurnFailure::UnfinishedReasoning { .. }) => {
                    // R93.D2: this arm is the turn ending INSIDE its reasoning, not running out.
                    self.append_notice(
                        letibot_plugin::notice::NUDGE,
                        "Your reply ended inside reasoning with no output. Make the tool call \
                         or give the answer now.",
                    )?;
                    continue;
                }
                Err(TurnFailure::Http(e)) => {
                    // **A context-length refusal IS the wall, and this is where it
                    // becomes one.** The provider owns the window and it has just
                    // said the prompt does not fit; every recovery `Sessions` has
                    // — the forced compaction and the continuation after it — is
                    // gated on `HarnessError::ContextWall`, so leaving the refusal
                    // a backend error leaves all of it unreachable. MEASURED
                    // 2026-10-09, session `s-1789919514688401228`: thirty
                    // consecutive 400s from `deepseek/deepseek-flash`
                    // (1,048,624 / 1,049,070 / 1,049,191 tokens against its
                    // 1,048,576), the first on a `monitor` wake and the last on a
                    // `todo check`, and not one compaction between them.
                    //
                    // The numbers stay honest about whose they are: the ERROR
                    // carries this box's own ledger and planning window (what it
                    // knows, in the units it decides in), and the WARNING names the
                    // provider's two alongside them — never a blend. A window this
                    // box never learned reads as zero in the error and is named as
                    // unconfigured in the warning, because inventing one here would
                    // point the operator at a wall nobody measured.
                    if let letibot_turn::HttpError::Status { body, .. } = &e
                        && let Some(refusal) = context_length_refusal(body)
                    {
                        let r = self.measure_resident().tokens;
                        let resident = self.session.ledger.len() as u64;
                        let window = self
                            .cfg
                            .planning_window()
                            .or(self.cfg.context_window)
                            .unwrap_or(0);
                        let (w, h) = self.cfg.shown_wall();
                        let unit = self.token_unit();
                        let theirs = match (refusal.requested, refusal.limit) {
                            (Some(req), Some(lim)) => format!(
                                " The provider's own count: {req} tokens against a {lim} \
                                 window."
                            ),
                            (Some(req), None) => {
                                format!(
                                    " The provider's own count: {req} tokens; it did \
                                     not name its window."
                                )
                            }
                            (None, Some(lim)) => format!(
                                " The provider named a {lim}-token window but not this \
                                 prompt's size."
                            ),
                            (None, None) => String::new(),
                        };
                        let no_window = if self.cfg.context_window.is_none() {
                            " This box has no --context-window set, so its own checks \
                                 could not have seen this coming; the provider's refusal \
                                 is the first measurement of this session's wall."
                        } else {
                            ""
                        };
                        self.hub.publish(SessionEvent::Warning {
                            code: "context_wall".into(),
                            detail: format!(
                                "stopping this turn after {round} round(s): the PROVIDER \
                                 refused the prompt for length. This box counts {r} \
                                 of {w} {unit} resident with {h} reserved for the next \
                                 round.{theirs}{no_window} Everything already produced is \
                                 committed, and the session compacts before the next turn \
                                 — this is the wall, not a failure of the work.",
                            ),
                            compaction: None,
                        });
                        eprintln!(
                            "  {}: context wall: the provider refused the prompt for \
                             length after {round} round(s) at {r} of {w} {unit} ({resident} \
                             of {window} ledger tokens)",
                            self.cfg.session_id
                        );
                        return Err(HarnessError::ContextWall {
                            rounds: round,
                            resident: r,
                            window: w,
                            unit,
                            ledger: (resident, window),
                        });
                    }
                    // **The whole account of the failure, not just the last call of it.**
                    //
                    // `http io: provider unreachable: io: failed to lookup address information:
                    // Temporary failure in name resolution` is the truth about ONE call, and the
                    // operator read it under `FAILED — ` with `(nothing was recorded)` after it and
                    // no way to tell whether the daemon had tried once or six times — nor, on the
                    // other class, whether a `401` had been asked six times for no reason. The
                    // ladder knows both, so the ending is its sentence: *tried 6 times over 31s,
                    // then gave up: name resolution failed*, or *http 401: … — the provider refused
                    // it, and a refusal is not retried (1 call, no waiting)*.
                    //
                    // The typed cause rides along ([`HarnessError::Endpoint`]) because
                    // `switch_back_notice` still asks it which 4xx refused.
                    return Err(HarnessError::Endpoint {
                        said: ladder.ending(&e),
                        cause: e,
                    });
                }
                Err(e) => return Err(e.into()),
            };

            // **Pair against everything the ledger took, not just the model's rows.**
            //
            // A steering message is appended inside the turn (`engine.rs`: "let
            // steering_items = pending.take_items(); session.append_items(...)"), through
            // the SAME sink, so it emits a `TranscriptAppended` like any other row — but
            // `TurnOk` reports it in `steering_applied` rather than in `items`.
            // Reconciling against `items` alone therefore left one unmatched id per
            // steering message, `record_item_pairing` fired, and the head drew the
            // steering rows empty.
            //
            // Harmless until T21.3 joined the intent check to the steering source, which
            // made an injection ordinary rather than rare. `reconcile` zips positionally,
            // so the order here is load-bearing — and there are now THREE groups, not
            // two, because the greedy poll appends whatever was waiting BEFORE the
            // generation as well. Transcript order is: what arrived while the last tool
            // ran, then the model's own rows, then what arrived while it was speaking.
            let mut appended = ok.steering_before.clone();
            appended.extend(ok.items.iter().cloned());
            appended.extend(ok.steering_applied.iter().cloned());
            self.reconcile(&mut sink, &appended);
            self.persist()?;
            // The last round's prompt size is the session's context size, and the
            // session row is the only place it survives a restart: a head that
            // attaches to a rebuilt view has no turn state to read it from.
            // Per round, the same way the heads' own `TurnFinished` updates theirs,
            // so the row and the screen agree at every point a round has landed.
            // **And what that prompt cost in each side's tokens.**
            //
            // The ledger and the provider count different things — reasoning is in
            // one and dropped from the other — so this is the only honest way to
            // convert between them, and it is a measurement rather than a
            // constant. See `Config::ledger_scale`.
            // **Any round, local included.** The guard here used to be
            // `self.provider.is_some()`, which meant a local round never refreshed the ratio:
            // after a provider → local switch the provider's scale stayed for the rest of the
            // session, and a session that had never been switched ran unscaled for ever. The
            // ledger and the server count the same prompt, so a local round measures a ratio
            // that is 1 by construction — and measuring it rather than assuming it is the
            // same rule the rest of this field follows.
            //
            // **And paired with the ledger the PROMPT had, not the one after the reply
            // (R72).** `prompt_tokens` counts what was sent; the round's own rows — the
            // model's reply and what was steered in while it spoke — were appended after
            // it, so the ledger read here is longer than the prompt by exactly those rows.
            // Pairing the two made a ratio of two moments, and on a reasoning-heavy reply
            // the difference is mostly reasoning. The prompt's ledger is the row boundary
            // before them: the rows are one-to-one with `appended`, and the last
            // `items + steering_applied` of them came after the prompt.
            let rendered_at = {
                let rows = self.session.ledger.rows();
                let after = ok.items.len() + ok.steering_applied.len();
                match rows.len().checked_sub(after) {
                    Some(0) => u64::from(self.session.ledger.prefix_len()),
                    Some(n) => u64::from(rows[n - 1].end()),
                    None => self.session.ledger.len() as u64,
                }
            };
            if ok.metrics.prompt_tokens > 0 {
                self.wall_anchor = Some(WallAnchor {
                    transcript: self.session.transcript_id.clone(),
                    ledger: rendered_at,
                    provider: ok.metrics.prompt_tokens,
                    measured: true,
                    scale: sent_scale(
                        &self.session.ledger,
                        &self.session.items,
                        rendered_at,
                        ok.metrics.prompt_tokens,
                        self.provider.is_some(),
                    ),
                });
                self.cfg.ledger_scale = Some((rendered_at, ok.metrics.prompt_tokens));
                self.measure_resident();
                // **The measurement the wall controller waits for** — the first round on a
                // folded base is where "did the fold make room" is answered, by the
                // provider's own count. Not the summary turn's rounds: those count the
                // history being folded, not the base.
                if !self.compacting {
                    self.observe_wall(ok.metrics.prompt_tokens);
                }
            }
            // Persisted with the ledger beside the count, so a restart recovers the
            // pair above instead of pairing the count with whatever ledger it has
            // then. See the gate in `Harness::open`.
            //
            // **Not fatal (R91.6).** The row is a measurement, and the round it measures is
            // already in the ledger and the store: a busy store that refused it used to end
            // the whole turn. Said, and the next round writes a fresh one.
            if let Err(e) = self.persist_context(&ok.metrics, rendered_at) {
                eprintln!("  store: {e}");
                self.hub.publish(SessionEvent::Warning {
                    code: "context_not_stored".into(),
                    detail: format!(
                        "this round's context size was not written to the session row: {e}. \
                         The turn goes on; the next round's measurement replaces it."
                    ),
                    compaction: None,
                });
            }
            truncated |= ok.truncated;
            // A steering message injected at the step boundary is already in the
            // transcript, and its own `TranscriptAppended` came through the same
            // sink, so `reconcile` picked it up above.
            let calls: Vec<ToolCall> = appended
                .iter()
                .filter_map(|i| match i {
                    TranscriptItem::Assistant { tool_calls, .. } => Some(tool_calls.clone()),
                    _ => None,
                })
                .flatten()
                .collect();
            let text = visible_text(&appended);
            // **The offer is settled here, where the round's calls are known**, and
            // logged whichever way it went — *"the signal is did a tool call in the
            // next round touch that path? Log offers and take-ups; tune the floor
            // from that ratio."* A round that ends the turn with no calls settles
            // with an empty list, which is an untaken offer and is recorded as one.
            offers.settle(&self.cfg.daemon_logs(), &self.cfg.session_id, &calls);
            // **What the agent says it is doing, recorded before its calls are
            // adjudicated.** A model's prose for a round is the sentence in front of
            // its tool calls — "the queued-prompt rendering lives in app.rs; I am
            // reading the composer block" — and that sentence is the one thing the
            // guard is missing when the operator's instruction names a goal rather
            // than a file. Never an authorisation and never citable; see
            // `ModelBrief::agent_claim`.
            self.trail.claims(&text);
            self.meter_round(&ok.metrics, rendered_at);
            metrics.push(ok.metrics);

            // **The context claim, checked against what the daemon measured** — R71. Judged
            // here, once per round and whichever way the round went, so the log has every
            // claim; acted on in the two branches below, where the intent check steers.
            // **And the reasoning in front of it**, when the reply holds no claim: a model that
            // budgets by "my context 49% → one call" says it there and nowhere the operator reads.
            // A claim found only there is told, never made to keep going — the reply may be whole.
            let (context_claim, claim_in_reasoning) =
                match self.judge_context_claim(&mut context_check, &text, false) {
                    Some(line) => (Some(line), false),
                    None => {
                        let thought = reasoning_text(&ok.items);
                        match self.judge_context_claim(&mut context_check, &thought, true) {
                            Some(line) => (Some(line), true),
                            None => (None, false),
                        }
                    }
                };

            if calls.is_empty() {
                // **A model that stopped "because the context is full" when it is not.**
                //
                // First, ahead of the intent check: the excuse is usually WHY a promise was
                // not kept, and the measured fill is the more useful of the two lines. It goes
                // through `append_notice` for the reason the nudge below does — the turn is
                // over, and a steering message would arrive after the reply — and the loop
                // continues, which is the half that makes it a correction rather than a report.
                if let Some(steer) = &context_claim {
                    self.append_notice(letibot_plugin::notice::NUDGE, steer)?;
                    if !claim_in_reasoning {
                        continue;
                    }
                }
                // **The turn boundary, and the one the error signal is about.**
                //
                // A turn that ends with no tool call is exactly T21.3's case —
                // *"when model says ill start that and by end of the turn forgets
                // and does not start anything"* — so the diff runs here, before the
                // reply leaves. `close_the_turn` reads the assistant's own text out
                // of `appended`; the tool calls are already in the ledger, put there
                // by the encoder as they ran.
                if nudges_left > 0
                    && let Some(steer) = self.close_the_turn(&ok.turn_id, &appended)
                {
                    nudges_left -= 1;
                    // Through `append_notice` rather than the steering queue: this
                    // turn is over, and a message queued for "the next step
                    // boundary" of a turn that has ended would arrive after the
                    // reply the operator is waiting on. The loop continues, which is
                    // the half of T21.3 that makes it a correction rather than a
                    // report.
                    self.append_notice(letibot_plugin::notice::NUDGE, &steer)?;
                    continue;
                }
                // **The branch is the deliverable.** After the intent check, so a turn that
                // promised work and did not start it hears about the promise first; the dirty
                // tree is still there to be told about on the round that answers it. Through
                // `append_notice` for the same reason as the nudge above: the turn is over, and
                // the loop continuing is what makes it a correction rather than a report.
                // **Not on a round that was cut short**: an interrupt (a `job_kill`, the
                // operator's stop) is the turn being ENDED, and a nudge that continued it would
                // be the harness overruling the stop. That child's line goes to its parent in
                // the completion instead (`AnswerOnce`).
                if !ok.truncated
                    && let Some(steer) = end_of_work.at_turn_end(self.deliverable.as_ref())
                {
                    self.append_notice(letibot_plugin::notice::NUDGE, &steer)?;
                    continue;
                }
                // **A message queued while the head was running is a prompt, not a
                // footnote.**
                //
                // The engine drains the steering queue at the step boundary — after
                // `TurnFinished` — and appends what it finds as user items, reporting
                // them in `steering_applied`. Until now that was where the story
                // ended: a turn that made no tool call returned here, the worker went
                // idle, and the queued message sat in the transcript answered by
                // nobody — "message was queued when you stopped and it didnt restart
                // you, while it went out o the queue". The loop continues instead:
                // the next round's prompt is the transcript, the queued items are its
                // last rows, and the model answers them. This covers every kind of
                // steering the boundary absorbs, not only the operator's words — a
                // monitor firing that lands at the boundary is `wake`'s business when
                // nothing is running, and this is the same obligation when something
                // just did. Bounded the way every round is: a round with nothing
                // queued and no tool call returns below, so one queued message costs
                // exactly one round.
                if !ok.steering_applied.is_empty() {
                    continue;
                }
                self.publish_jobs();
                return Ok(Reply {
                    text,
                    metrics,
                    rounds: round + 1,
                    tool_calls,
                    truncated,
                });
            }

            tool_calls += calls.len();
            let turn_id = ok.turn_id.clone();
            let mut backgrounded = false;
            let mut results = Vec::with_capacity(calls.len());
            // **The person's half of a question, held back until every result is in.**
            //
            // A question's answer is the operator's own row, and it is appended with this
            // round's results — but AFTER all of them, never between a call and its result.
            // `Harness::settle_asked` carries the measurement and the reason; the short of
            // it is that a non-`tool` row closes `pair_tool_calls`' answering window, so a
            // `User` row inserted before the result makes the model read *no result was
            // recorded* beside the real one.
            let mut asked: Vec<TranscriptItem> = Vec::new();
            for call in &calls {
                // Call order, and appended in call order. See point 3 above.
                // `self.tool_sink` is the log sink wrapped in the intent encoder, so
                // every result is measured on its way past — an `ok` is an effect
                // and anything else is an attempt, which is the distinction the
                // diff below is built on.
                // **And nothing an earlier call left can be read as this one's answer.**
                // The slot is emptied immediately before the call runs, so whatever is in
                // it afterwards belongs to THIS call — including nothing. Without this a
                // questioner that ran outside the round loop (the door's own execute path)
                // would leave an exchange for the next model question to claim.
                self.asked.lock().unwrap_or_else(|e| e.into_inner()).take();
                let started = Instant::now();
                let r = self.runtime.invoke(&turn_id, call, &mut self.tool_sink);
                self.meter.tool(&self.cfg.session_id, &r, started);
                let mut item = ToolRuntime::transcript_item(&r);
                // **A question the model asked is an exchange with a person, and the
                // person's half of it is theirs.** The tool returns the answer; the
                // session writes it down, here, where every other result is appended.
                if call.name == ASK_USER_QUESTION {
                    asked.extend(self.settle_asked(&mut item));
                }
                // **A plugin layer sees the result before the model does** (`docs/plugins-plan.md`
                // §4.2), and before the harness's own intercept below: the layer is about what the
                // CALL produced, and `pr_watch`'s sentence is the harness's own reading of it.
                if let TranscriptItem::ToolResult {
                    outcome, payload, ..
                } = &mut item
                {
                    // **A successful result only.** A call that failed did nothing to react to,
                    // and its result is the failure's own words — which the model has to read
                    // exactly, not through a layer's opinion of them.
                    let how = match outcome {
                        letibot_transcript::ToolOutcome::Ok => {
                            Some(letibot_plugin::tool_result::OK)
                        }
                        letibot_transcript::ToolOutcome::Backgrounded { .. } => {
                            Some(letibot_plugin::tool_result::BACKGROUNDED)
                        }
                        _ => None,
                    };
                    if let Some(how) = how {
                        self.plugin_result(call, how, payload);
                    }
                }
                // **A pull request this call opened is watched from here on** (`pr_watch`), and
                // the call's own result says so.
                if let TranscriptItem::ToolResult {
                    outcome: letibot_transcript::ToolOutcome::Ok,
                    payload,
                    ..
                } = &mut item
                    && let Some((url, n)) = crate::pr_watch::opened(call, payload)
                {
                    let said = self.watch_opened_pr(&url, n);
                    payload.push_str(&said);
                }
                // The progress encoder reads the *rendered* result, which is the
                // string the model will actually get to read. Digesting anything
                // else would measure novelty the model never saw.
                if let TranscriptItem::ToolResult {
                    outcome, payload, ..
                } = &item
                {
                    progress.observe(call, outcome, payload);
                    // Backgrounding is the operator reaching for the floor; see the
                    // yield at the end of this round.
                    if matches!(
                        outcome,
                        letibot_transcript::ToolOutcome::Backgrounded { .. }
                    ) {
                        backgrounded = true;
                    }
                }
                results.push(item);
            }
            results.extend(asked);
            let round_verdict = progress.end_round();
            let mut sink = CapturingSink::new(self.hub.clone());
            self.session
                .append_items(&self.engine, &results, &mut sink)?;
            self.reconcile(&mut sink, &results);
            // **And the calls this round's gate admitted without asking anybody** (D16). Said
            // here, after the results and never between them: a notice appended between a call
            // and its result closes the pairing window and the model reads *no result was
            // recorded* beside the real one. See [`Harness::flush_gate_admissions`].
            self.flush_gate_admissions()?;
            // A round can have started or ended a job; refill the mailbox now so a
            // pane opened mid-turn answers with this round's table, not last
            // round's. Cheap: the list is filtered before it is built.
            self.publish_jobs();
            // **And a mode change the operator made while this turn was running.**
            //
            // It sat in the command queue until the worker came back — which is
            // after the turn — so `/mode` moved nothing while the turn went on
            // asking under the old point. That is exactly when somebody reaches
            // for it. Applied here, at the round boundary, on the thread that owns
            // the harness: the gate reads its mode at decision time, so the very
            // next call is governed by it.
            self.apply_queued_mode();
            // **And a tool the operator ran while this turn was running** — R31's deposit,
            // which is what the door is FOR.
            //
            // The operator watches the model go down a wrong path and drops the doc in; the
            // row lands here, at the round boundary, and the next round of this same turn
            // reads it. The alternative was measured rather than feared: a door call sat in
            // the worker's queue until the turn ended, and a turn can run for minutes — see
            // `Hub::try_head_run_command`, which is where the mechanism is written down.
            self.apply_queued_head_run();
            // **And the merge-queue reviews this session hosts**, at the same boundary and for
            // the same reason as the two above: they are work whose whole point is to happen
            // *during* a turn, and the door that would otherwise take them (`Harness::wake`) is
            // the one this turn is holding shut. MEASURED, 2026-10-10: eighteen `Due` rows,
            // none of them ever asked, for half an hour — the ring was queued behind turns that
            // never looked. The cost here is one indexed `SELECT` over a table that is one row
            // per queue entry, against a round that is a generation.
            let served = self.serve_reviews();
            if served > 0 {
                eprintln!(
                    "  {} · reviews -> {served} gatekeeper(s)",
                    self.hub.session_id()
                );
            }
            self.persist()?;
            // The calls of this round have run; if the model revised its plan,
            // the store and the heads hear about it now, at the round boundary —
            // not when the turn ends, which is after the work the plan describes.
            self.flush_todos()?;
            // A turn that ran tools also gets the diff — a completion marked done
            // while nothing succeeded is `CompletedWithoutEffect` and is just as
            // much the error signal. This one goes through the steering queue,
            // because the loop is about to generate again and the step boundary is
            // where §5.8 injects.
            if nudges_left > 0
                && let Some(steer) = self.close_the_turn(&turn_id, &appended)
            {
                nudges_left -= 1;
                self.injected
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push_back(steer);
            }
            // And a context claim made by a round that went on to call tools: the model is
            // still working, so the correction rides the step boundary like the nudge above.
            if let Some(steer) = context_claim {
                self.injected
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push_back(steer);
            }

            // **A backgrounded job gives the floor back to the OPERATOR, not to
            // the model.**
            //
            // Ctrl-B exists because a tool is taking too long and there is
            // something to say. It used to end only the `bash` call: control went
            // straight back to the model, whose very next move was `job_wait` with
            // a three-minute bound — so the operator was deaf again immediately and
            // their line sat queued through all of it. *"i backgrounded a job but
            // my messsage still queued"*.
            //
            // So when a round backgrounded something and the operator is waiting,
            // the turn ends here. Their prompt is deliberately NOT consumed as
            // steering — `has_queued_prompt` only looks — so the worker picks it up
            // as a turn of its own and answers it first. That is what asking for
            // the floor means; the job is still running and still theirs to read.
            if backgrounded && self.hub.has_queued_prompt() {
                self.publish_jobs();
                return Ok(Reply {
                    text,
                    metrics,
                    rounds: round + 1,
                    tool_calls,
                    truncated,
                });
            }

            // **The stop, and it is two-stage on purpose.**
            //
            // `docs/closed-loop.md` §4: a closed loop corrects inside the tolerance
            // band and faults outside it. One round before the bound the model is
            // told what the detector sees — through the same step-boundary injection
            // T21.3 uses — so a turn that was in fact working can say so and carry
            // on. Only if the *next* round is also stalled does the turn stop.
            //
            // The asymmetry is deliberate. Cutting a working turn has now cost two
            // sessions; letting a stuck one run one extra round costs one round.
            if round_verdict == crate::progress::Round::Stalled {
                if progress.exhausted() {
                    return Err(HarnessError::NoProgress {
                        evidence: progress.evidence(round + 1, self.cfg.max_tool_rounds),
                    });
                }
                if let Some(steer) = progress.nudge() {
                    self.injected
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push_back(steer);
                }
            }
        }
        // Unreachable while unbounded — `usize::MAX` rounds is not a number this
        // process reaches — and honest if the bound is ever set back.
        Err(HarnessError::LoopBound { rounds: backstop })
    }

    /// This session's steering source: the head's queue, the intent findings, and
    /// a monitor that fires mid-turn.
    ///
    /// Rebuilt per round because it borrows nothing and holds only clones; what it
    /// must **share** is the trail, the injected queue and the monitor cursor, so a
    /// firing picked up in one round is not delivered again in the next.
    /// **A `/mode` issued while this turn is running**, taken at a round boundary.
    ///
    /// Reported exactly as the between-turns path reports it — same sentence,
    /// same codes — because "what did my mode change do" must not depend on
    /// whether a turn happened to be running when it was typed.
    /// **A door call the operator made while this turn is running**, run and appended here.
    ///
    /// Called at the round boundary beside [`Harness::apply_queued_mode`] and for the same
    /// reason: it is a command whose whole point is to act *during* a turn, so it is taken by
    /// the thread that owns this harness rather than waiting for the worker — which is
    /// inside the turn.
    ///
    /// # The admission is the same one, and it is not a second door
    ///
    /// The name was checked against [`letibot_sessionlog::HEAD_RUN_TOOLS`] on the connection's
    /// thread before this command was ever queued, and the corpus row is written by
    /// [`Harness::admit_operator_call`] — the same function the worker's path calls. So the
    /// sugar does not skip the door: **the same list, the same row, the same `human:<who>`**,
    /// and only the thread that runs the tool differs.
    ///
    /// # It loops, because the operator may drop two things in
    ///
    /// A round is long and the boundary is the only moment this can be done, so a queue that
    /// held two documents would otherwise deliver one per round.
    fn apply_queued_head_run(&mut self) {
        while let Some(cmd) = self.hub.try_head_run_command() {
            // **The operator's shell line is taken by the same picker and runs the
            // same way** — synchronously, here, at the round boundary — but it is
            // not a door call and has no admission to note, so the two kinds are
            // separated before the door's own bookkeeping starts rather than
            // threaded through it with an `if`.
            //
            // **Unless a run of its own kind is already in flight.** One operator run
            // per session is the rule (one input handle; see `crate::bangrun`), and a
            // line typed mid-turn while a between-turns run waits on its thread would
            // overlap it if this ran it here. It parks instead — worker-owned state,
            // drained by the settle arm when the in-flight run hands back — and the
            // turn goes on, which is what the operator watching both wanted: neither
            // their command nor the turn is lost, and neither runs on top of the
            // other. When NOTHING is in flight this keeps its old timing exactly.
            if let CommandKind::OperatorShell { line, who } = &cmd.kind {
                let (line, who) = (line.clone(), who.clone());
                if self.bang_in_flight() {
                    self.park_bang_line(line, who);
                    continue;
                }
                if let Err(e) = self.run_operator_shell(&line, &who) {
                    // Said, not swallowed: the run had no caller to answer, so the
                    // log is the only place the failure can land (the door's arm
                    // below does the same for its own runs).
                    self.import_note(
                        "operator_shell_failed",
                        format!(
                            "`{who}` ran `{line}` from their own console and it could not be \
                             recorded: {e}. The command may have run; what is missing is the \
                             row the next turn would have read."
                        ),
                    );
                }
                continue;
            }
            let CommandKind::OperatorCall {
                call_id,
                name,
                arguments,
                who,
                ..
            } = &cmd.kind
            else {
                // The picker matched an `OperatorCall` or an `OperatorShell`; anything
                // else here would be a bug in the picker, and returning rather than
                // panicking leaves the command queued for the worker, which is where it
                // belongs.
                return;
            };
            let (call_id, name, arguments, who) = (
                call_id.clone(),
                name.clone(),
                arguments.clone(),
                who.clone(),
            );
            // **Noted and taken around a synchronous run**, exactly as the worker's path does.
            // With the daemon running the tool there is no window in which the pending entry
            // is the only record of the call, so the note is normally a no-op — but keeping
            // the pair means a head that detaches during the run still gets the sentence, and
            // a future reader sees one pattern rather than two.
            self.hub
                .note_operator_call(&call_id, &cmd.head_id, &name, &who);
            let said = self.run_operator_call(&call_id, &name, &arguments, &who);
            self.hub.take_operator_call(&call_id);
            if let Err(e) = said {
                // **Said, because there is nothing to return it to.** The worker's path
                // reports a failure as `Outcome::Failed`, which the daemon prints; this one
                // has no caller to answer, so the failure goes on the log or it goes
                // nowhere — and a call that ran with its row unpinned is exactly the kind of
                // silence this tree exists against.
                self.import_note(
                    "transcript_store",
                    format!(
                        "`{who}` ran `{name}` from their own console and the transcript could not be written: {e}. The call ran; what is missing is the record of it, which is what the next turn would have read."
                    ),
                );
            }
        }
    }

    pub(crate) fn apply_queued_mode(&mut self) {
        let Some(cmd) = self.hub.try_mode_command() else {
            return;
        };
        let letibot_sessionlog::hub::CommandKind::Mode { name, consented } = &cmd.kind else {
            return;
        };
        let mode = match letibot_tools::mode::Mode::parse(name) {
            Ok(m) => m,
            Err(e) => {
                self.hub.publish(letibot_sessionlog::SessionEvent::Warning {
                    code: "mode_unknown".into(),
                    detail: e,

                    compaction: None,
                });
                return;
            }
        };
        // **The rows are written here too** (`modes::record`, the worker's writes). This said the
        // worker's path would write them; it never saw this command, which this turn had already
        // taken off the queue.
        let consented = *consented;
        let moved = self.set_mode_consented(mode, consented);
        // **And written down, as the worker's door writes it** — this door used to apply the
        // point and write nothing, so a `/mode` typed while the session worked was gone at the
        // next restart.
        if let Err(e) = crate::modes::record(
            &self.mode_store,
            self.workspace(),
            &self.cfg.session_id,
            mode,
            consented,
            moved.is_ok(),
        ) {
            self.hub.publish(letibot_sessionlog::SessionEvent::Warning {
                code: "mode_unpersisted".into(),
                detail: format!(
                    "`{}` applies to this session now, but could not be recorded, so a restart \
                     will not bring it back: {e}",
                    mode.name
                ),
                compaction: None,
            });
        }
        match moved {
            Ok(said) => {
                let applied = self.cfg.mode;
                self.hub.publish(letibot_sessionlog::SessionEvent::Warning {
                    code: "mode_set".into(),
                    detail: format!("{said}. {}", applied.summary),

                    compaction: None,
                });
            }
            Err(why) => {
                self.hub.publish(letibot_sessionlog::SessionEvent::Warning {
                    code: "mode_set_refused".into(),
                    detail: format!("this session stays at `{}`: {why}", self.cfg.mode.name),

                    compaction: None,
                });
            }
        }
    }

    /// **A `/models` the operator typed while this turn was running** — taken and answered here,
    /// on the thread that owns this harness, and **recorded rather than applied**.
    ///
    /// # Why the turn takes it at all
    ///
    /// `Hub::try_command_for_running_turn` carries the measurement (2026-10-12: the
    /// operator's `/models` sat in the queue for a whole 63-second ladder and was
    /// answered after the turn). `/mode` has had this door since R29 and `/models` did not, and
    /// the reply arriving while they are looking matters just as much now that the switch no
    /// longer takes effect mid-turn: the sentence they get back is *what will happen and when*,
    /// which is only useful inside the turn it is about.
    ///
    /// # Why it no longer takes effect here
    ///
    /// Because it must not, and the measurement is [`Harness::set_provider`]'s: a round re-issued
    /// on another model in the middle of a turn carries the rest of the old model's turn, and a
    /// thinking-mode provider refuses exactly that. So the switch goes through the ordinary door,
    /// which records it as pending, and [`Harness::end_turn_clock`] applies it.
    ///
    /// # What it will NOT take
    ///
    /// `slash::mid_turn_switch_ok` decides, and its two `no`s are the switches that
    /// re-seat the conversation — a fleet model's weights, and `local` when this
    /// session is on foreign weights. A re-seat forks the transcript and swaps the
    /// session under `self.session`, under a turn that is in flight with a sink open
    /// and rows still to reconcile. Those lines stay in the queue, in order, and the
    /// worker takes them when the turn ends, exactly as every `/models` did before
    /// this existed: the switch still lands, one turn later than it would have.
    fn apply_queued_model(&mut self) {
        while let Some(cmd) = self.take_queued_model() {
            let letibot_sessionlog::hub::CommandKind::Slash { line } = &cmd.kind else {
                continue;
            };
            let line = line.clone();
            let reply = match crate::slash::Slash::parse(&line) {
                crate::slash::Slash::ModelsSet {
                    provider,
                    model,
                    key,
                } => match crate::slash::models_choice(
                    &provider,
                    model.as_deref(),
                    key.as_deref(),
                    None,
                ) {
                    Ok((choice, lines)) => self.apply_model_choice(choice, lines),
                    Err(lines) => crate::slash::SlashReply { lines, ok: false },
                },
                // The picker only says yes to a model switch, so this is a bug in the
                // predicate rather than a verb. Answered rather than dropped: the
                // command has been taken out of the queue, and a command that vanishes
                // without a word is the failure this tree keeps deleting.
                _ => crate::slash::SlashReply {
                    lines: vec![format!(
                        "/{line}: not a model switch this turn can apply, so nothing moved"
                    )],
                    ok: false,
                },
            };
            // **The reply the worker would have published** — the same two codes and
            // the same body, so "what did my switch do" does not depend on which thread
            // happened to apply it. `apply_queued_mode`'s rule, one verb along.
            self.hub.publish(SessionEvent::Warning {
                code: if reply.ok {
                    "slash".into()
                } else {
                    "slash_refused".into()
                },
                detail: format!("/{line}\n{}", reply.lines.join("\n")),

                compaction: None,
            });
        }
    }

    /// The picker half of [`Harness::apply_queued_model`]: **the next queued command
    /// this running turn can carry out**, with the verb's own grammar answering.
    fn take_queued_model(&self) -> Option<letibot_sessionlog::hub::QueuedCommand> {
        let foreign = self.on_foreign_weights();
        self.hub.try_command_for_running_turn(|kind| match kind {
            letibot_sessionlog::hub::CommandKind::Slash { line } => {
                crate::slash::mid_turn_switch_ok(line, foreign)
            }
            _ => false,
        })
    }

    /// **This attempt failed, and the outage remembers it** — see [`Outage`] for why nothing is
    /// published here.
    ///
    /// **Which host, asked of the thing that was contacted.** This read
    /// `self.cfg.endpoint.authority()` — the LOCAL endpoint — on a path that also serves cloud
    /// turns, so a resolver failure against a provider was reported as a problem with
    /// `127.0.0.1:8080`. MEASURED 2026-10-01: the operator read *"the model server at
    /// 127.0.0.1:8080 did not answer: ... Temporary failure in name resolution"* and went to a
    /// `llama-server` that was answering `/health` with 200. Not knowing which host failed is
    /// what made the message expensive; `cfg.endpoint` is only right for the local route, so the
    /// cloud route is asked for its own.
    fn outage_failed(&mut self, e: &letibot_turn::HttpError) {
        let host = retry_host(self.provider.as_deref(), &self.cfg.endpoint);
        let now = letibot_sessionlog::event::now_ms();
        // **A failure against a DIFFERENT host is a different outage** — the operator's
        // `/models` and a `[fallback]` both move this, and two endpoints' attempts are not one
        // counter. The old record is dropped rather than told: it is over, and the row for it
        // would be about a host this session is no longer failing against.
        match &mut self.outage {
            Some(o) if o.host == host => {
                o.attempts += 1;
                o.last_ms = now;
                o.said = e.to_string();
            }
            slot => {
                *slot = Some(Outage {
                    host,
                    attempts: 1,
                    began_ms: now,
                    last_ms: now,
                    said: e.to_string(),
                });
            }
        }
    }

    /// **The outage is over, and this is the one row it earns** — see [`Outage`].
    ///
    /// `answered` is *the round ended on anything but an HTTP failure*, which includes a round
    /// that failed for some other reason: the endpoint spoke, so the outage is over either way.
    /// The record is TAKEN, so this is the only place that can write the row and it can write it
    /// once.
    fn settle_outage(&mut self, answered: bool) {
        let Some(outage) = self.outage.take() else {
            return;
        };
        // **The endpoint came back, and that is said however few attempts it took** — a silent
        // recovery is how somebody spends an afternoon believing it is still down.
        if answered {
            self.hub.publish(SessionEvent::Warning {
                code: "model_endpoint_answered".into(),
                detail: outage.answered(letibot_sessionlog::event::now_ms()),
                compaction: None,
            });
            return;
        }
        // **One failure the ladder never retried is not an outage**: with `http_retries = 0`
        // the turn's own `turn_failed` is the whole of the news, and a row saying `attempt 1`
        // beside it would be the same fact twice.
        if outage.attempts < 2 {
            return;
        }
        self.hub.publish(SessionEvent::Warning {
            code: "model_endpoint_retry".into(),
            detail: outage.did_not_answer(),
            compaction: None,
        });
    }

    /// **The model is out, and this box has somewhere else to go** — `[fallback]
    /// models` in `providers.toml`, tried in the operator's order, once each.
    ///
    /// # It records the move; it does not make it
    ///
    /// The operator's ruling stands — *"it is a standard thing to do - limits, 5xx, etc. we
    /// must be able to change models like for the main session"* — and what changed is WHEN. A
    /// 429 that says `Weekly/Monthly Limit Exhausted` is not weather and no wait lifts it, so the
    /// session moves at the end of the turn instead of inside it; the turn that met the refusal
    /// ends with it.
    ///
    /// **Why the round cannot go on elsewhere** (MEASURED 2026-10-26, [`Harness::set_provider`]
    /// has the whole of it): a re-issued round carries the rest of the old model's turn — its
    /// assistant tool-call rounds — and a thinking-mode provider refuses exactly that, by name.
    /// A model's first request must follow a user message, and the only user message available is
    /// the next turn's.
    ///
    /// # The two guards, and why each is here
    ///
    /// * **`model_refused`**: a 429 or a 5xx only. A connection that was refused, a
    ///   stream that did not parse, a 408 — those are the route rather than the model,
    ///   and moving the session would abandon the model the operator chose for a reason
    ///   that is not about it.
    /// * **`context_length_refusal`**: a provider refusing the prompt for LENGTH is
    ///   answering perfectly well. That is the wall, it is classified as one a few lines
    ///   below in `run_rounds`, and another model would refuse the same prompt.
    ///
    /// # What it will and will not switch to
    ///
    /// A **metered** name only, and this is narrower than `/models` on purpose. A fleet model or
    /// the daemon's own server can re-render the conversation into a fork, and that is not
    /// something a retry loop should do unbidden — not even at a turn boundary, where the fork
    /// would land between the rows the turn has written and the ones it has not. A name it will
    /// not take is SAID and skipped — a fallback nobody can reach, silently, is the defect the
    /// key's own reader is written against.
    ///
    /// Returns the model it recorded, or `None` when the list is empty, spent, or has
    /// nothing this turn can use.
    fn fall_back_from(
        &mut self,
        e: &letibot_turn::HttpError,
        tried: &mut Vec<String>,
    ) -> Option<String> {
        if !model_refused(e) || self.cfg.fallback.is_empty() {
            return None;
        }
        if let letibot_turn::HttpError::Status { body, .. } = e
            && context_length_refusal(body).is_some()
        {
            return None;
        }
        let here = self.model_name();
        for name in self.cfg.fallback.clone() {
            // **Once each, and never the model that just refused the round.** A name
            // already spent is skipped; a name that IS what is answering would be a
            // no-op switch and the same failure one attempt later.
            if tried.contains(&name) || name == here {
                continue;
            }
            tried.push(name.clone());
            let (provider, model) = crate::slash::split_model_name(&name);
            // **The resolver's own notes are dropped on the success arm**, deliberately: a
            // fallback is a switch nobody typed, so the sentence that has to be right is
            // `fallback_notice`'s, said by the caller with the provider's refusal in it.
            let (choice, _notes) =
                match crate::slash::models_choice(&provider, model.as_deref(), None, None) {
                    Ok(x) => x,
                    Err(lines) => {
                        self.fallback_skip(&name, &lines.join(" "));
                        continue;
                    }
                };
            let crate::slash::ModelChoice::Metered(pc) = choice else {
                self.fallback_skip(
                    &name,
                    "a metered model is the only thing an automatic switch may land on, and this \
                     one is the daemon's own server or a fleet model — either can re-render the \
                     conversation into a fork, which is not something the daemon's own switch \
                     should do unbidden",
                );
                continue;
            };
            // **The note the `models_choice` resolver came back with is not carried**: a
            // fallback is a switch nobody typed, so the sentence that matters is
            // `fallback_notice`'s, said by the caller with the provider's own refusal in it.
            let target = model_name_of(&Some(pc.clone()));
            self.pend_switch(Some(pc), target.clone());
            // **And the work goes on by itself** — the operator's ruling of 2026-10-12, and the
            // half a switch alone does not do: *"when it finally applied the turn didnt
            // continue"*. `turn_tail::run_turn` opens this turn when the one that met the refusal
            // has ended, with the notice as its user item.
            //
            // **Not when THIS turn is already such a continuation**: a fallback chain would then
            // ping-pong between two names, paying a model change per turn and never stopping.
            if !self.in_fallback_continuation() {
                self.fallback_continuation = Some(fallback_continuation_notice(&here, &target, e));
            }
            return Some(target);
        }
        None
    }

    /// **A fallback name that could not be taken, said.** The same code as the retry it
    /// is part of — the round is still being taken again, and what this adds is why it
    /// is not going *there*.
    fn fallback_skip(&self, name: &str, why: &str) {
        self.hub.publish(SessionEvent::Warning {
            code: "model_endpoint_retry".into(),
            detail: format!(
                "`[fallback]` names `{name}` and this session cannot take it: {why}. \
                 Trying the next name."
            ),

            compaction: None,
        });
    }

    pub(crate) fn steering(&self) -> HubSteering {
        HubSteering {
            hub: self.hub.clone(),
            trail: Some(self.trail.clone()),
            injected: self.injected.clone(),
            monitors: self.monitors.clone(),
            monitor_cursor: self.monitor_cursor.clone(),
            // The session's own set, read by R79's rules: what the work waits on at the
            // round boundary, the held batch only past the cap.
            deliveries: self.deliveries(),
            // **And the session's own children, so a stop takes them with it.** See the
            // interrupt arm of `try_next`.
            subagents: Some(self.subagents.clone()),
            held: self.handed_back.clone(),
            // **And the board, so the operator's answer lands mid-turn.** See the `AnswerTodo` arm.
            todos: Some(self.todos.clone()),
            // **And this session's own parent**, so a message's sentence says which way the edge
            // runs — the same field `submit_a_parents_message` reads, so the two doors word it
            // identically.
            parent: self.cfg.parent_session_id.clone(),
        }
    }

    /// **The intent diff at a turn boundary**, or `None` when there is nothing to
    /// say — which is the common case and is meant to be.
    ///
    /// Two callers into one of two functions, and the difference is a flag:
    ///
    /// * `intent::close_the_turn` reads the assistant's own prose for the
    ///   [`letibot_tools::builtins::intent::commitments`] heuristic **as well as**
    ///   the tool-declared half.
    /// * `IntentLedger::reconcile(turn_id, "")` is the tool-declared half alone,
    ///   which that method's own doc calls *"the deterministic one"*.
    ///
    /// **And it is TWO questions, not one** — both of them *did this turn stop with
    /// something it said it would do still open*. The ledger answers it of the intentions
    /// the turn declared; `todo::unfinished_plan` answers it of the model's OWN plan,
    /// which is the larger half because the plan is where a long session's work actually
    /// lives. Both findings are true and the model should hear both, so they are joined
    /// rather than one being chosen over the other; either alone is the message.
    ///
    /// The deterministic half is the default because of the rule this whole strand
    /// is under: **nothing widens by default.** The prose heuristic can only fire on
    /// a turn that ran nothing at all, which for a plain answer is the normal case,
    /// so switching it on for every existing session would put *"you said you would
    /// X"* into conversations that were working. It is the more useful half and it
    /// is the one with false positives, so it is `--intent-prose` and its cost is
    /// written where somebody deciding can read it.
    ///
    /// The deterministic half needs no flag because it cannot fire unless the model
    /// used `todo` or `goal`, and those are seated only by a role that names them.
    /// A default `letibot` session is behaviourally identical.
    fn close_the_turn(&self, turn_id: &str, items: &[TranscriptItem]) -> Option<String> {
        // **THE PLAN IS NOT CHECKED HERE ANY MORE, and that is the scheduling change.**
        //
        // The turn-boundary check is for a claim about the turn that just ended — T21.3's *"when
        // model says ill start that and by end of the turn forgets and does not start anything"* —
        // and the intent ledger is exactly that (`steer_for_turn`).
        //
        // The PLAN is not a claim about one turn; it is a standing fact about the session, and
        // checking it at every turn boundary gave the operator both failure modes at once: a
        // session in constant conversation was checked after every exchange (*"looks like our todo
        // nag is overkill"*), while a session that went quiet was checked once and then never
        // again. It is now delivered by the IDLE clock instead — `Sessions::rearm_todo_nag` and
        // `Harness::nag_turn`, on the operator's own instruction: *"maybe wait for a timeout
        // actually. so send it when model is idling"* and *"but certainly not after my message."*
        //
        // One check, one schedule. `None` here is what makes that true rather than a comment
        // saying so, and `close_the_turn_with` keeps the plan parameter so the join it performs is
        // still reachable from a test.
        self.close_the_turn_with(turn_id, items)
    }

    /// The intent finding for a turn that has just ended.
    ///
    /// A named function rather than the call inlined above, because it is the seam the tests
    /// reach — and it is now the WHOLE of the turn-boundary check. The plan's finding used to be
    /// joined in here (see `close_the_turn` for why it is not), so a reader looking for where the
    /// todo check happens will not find it at this layer at all: it is `Harness::nag_turn`, on the
    /// idle clock.
    fn close_the_turn_with(&self, turn_id: &str, items: &[TranscriptItem]) -> Option<String> {
        // R93.F30: one plan tool per seat. Where it is `todo_write`, the intent ledger's own
        // board is empty and the check reads the `todo_write` board instead.
        let seated = tool_names(&self.prefix.tools_json);
        let plan = (seated.contains("todo_write") && !seated.contains("todo"))
            .then(|| intent_tools::ledger::Board::of_plan(&self.todos.snapshot()));
        steer_for_turn_over(&self.intent, self.cfg.intent_prose, turn_id, items, plan)
    }

    /// **One round's context claim, judged, logged and — for the operator — warned
    /// about.** The line for the MODEL is returned, for the caller to deliver where the
    /// round's shape says it goes (a notice after a turn that ended, the step boundary
    /// for one that is still calling tools). See `crate::context_claim`.
    fn judge_context_claim(
        &mut self,
        check: &mut crate::context_claim::Check,
        text: &str,
        reasoning: bool,
    ) -> Option<String> {
        use crate::context_claim::{REPEATED, Verdict};
        if text.trim().is_empty() {
            return None;
        }
        let fill = self.context_fill();
        let mut finding = match reasoning {
            false => check.judge(text, fill)?,
            true => check.judge_reasoning(text, fill)?,
        };
        // R93.A4: with auto-compaction off, "the harness handles it" is false — nothing compacts
        // at the wall (`turn_tail` says so when the turn stops there) — so the model is told
        // nothing, and the operator is not told it was. The claim is still logged and measured.
        if !self.cfg.auto_compact {
            finding.verdict = Verdict::Quiet;
        }
        finding.log(&self.cfg.daemon_logs(), &self.cfg.session_id);
        crate::instruments::context_claim(&self.cfg.session_id, &finding);
        match finding.verdict {
            Verdict::Slap(line) => Some(line),
            Verdict::Repeated(detail) => {
                self.hub.publish(SessionEvent::Warning {
                    code: REPEATED.into(),
                    detail,
                    compaction: None,
                });
                None
            }
            Verdict::Quiet => None,
        }
    }

    /// **The session's fill, as the wall judges it** — the one place a context claim's
    /// measurement is read, so the day the wall's measurement changes (R72 rewrites
    /// `planning_window` and the ledger scale) this changes with it and nothing else
    /// does.
    ///
    /// The verdict is the wall's own: `Config::at_the_wall` over the ledger, the
    /// predicate `compact_at_the_wall` acts on — so the check never calls a claim false
    /// that the harness would have compacted for. The two numbers the model is told are
    /// in the units the head shows (`Config::shown_tokens`), because a figure the
    /// operator cannot find on their own screen is a second disagreement. `None` when the
    /// window is unknown: not knowing is not a measurement, and a claim cannot be
    /// checked against it.
    fn context_fill(&mut self) -> Option<crate::context_claim::Fill> {
        // **R72's count, the one the head and the wall show.** `measure_resident` refreshes
        // `cfg.ledger_scale` from the provider's own last count plus what was sent since, so
        // `planning_window`, `at_the_wall` and `shown_tokens` below all read that one number —
        // the slap quotes the fill the operator sees, not the ledger's reasoning-heavy length.
        self.measure_resident();
        self.cfg.planning_window()?;
        let resident = self.session.ledger.len() as u64;
        Some(crate::context_claim::Fill {
            resident: self.cfg.shown_tokens(resident),
            // **The window as it was GIVEN** (`shown_wall`, R72's rule for a line), not
            // `shown_tokens(planning_window())` — a round trip through the ratio, which is where
            // every row of R71's log got `999999` for a 1,000,000-token window (TODO R81).
            window: self.cfg.shown_wall().0,
            at_wall: self.cfg.at_the_wall(resident),
        })
    }

    /// **Tell this session's model something the HARNESS found, without running a turn.**
    ///
    /// The door [`crate::plugins`] needs and the reason it queues rather than publishes: a layer
    /// the shadow gate refused, or a pane that answered something the contract refuses, is a fact
    /// the MODEL is the one that can fix — and a fact only the harness can put in front of it,
    /// because a notice is a transcript row and rows are the harness's to write. The tick that
    /// discovers it runs on the worker's idle pass, where the harness is in hand; the gate itself
    /// runs off that pass, on a thread, which is why it hands the sentence over instead of
    /// speaking ([`crate::plugins::PluginHost::take_notices`]).
    ///
    /// It is `append_notice` and not a turn: a plugin that failed to load is not a reason to
    /// spend a generation, and the row is read by the model's next turn like any other harness
    /// note.
    pub(crate) fn note_to_model(&mut self, text: &str) -> Result<(), HarnessError> {
        // **Not through the `notice` hook, on purpose.** This is a plugin layer's own failure — a
        // gate refusal, a pane the contract refused — and a layer that traps inside the hook that
        // reports trapping would be a loop with a model at the end of it. §4.3's rule is that the
        // harness's notices may be reworded; a notice about a broken report is not one of them.
        self.record_notice(text)
    }

    /// **One notice through the plugin stack** — the sentence to record, or `None` when a layer
    /// dropped it (`docs/plugins-plan.md` §4.3).
    ///
    /// A session with no layers, or one whose layers answer other hooks, gets its sentence back
    /// unchanged and pays one walk over an empty slice: `PluginHost::notice` answers before it
    /// builds an input when nothing exports `notice_<kind>`, which is what makes *a session with
    /// no layers behaves as it did* a property of the code rather than a promise.
    fn noticed(&self, kind: &str, text: &str, facts: serde_json::Value) -> Option<String> {
        match self.plugin_host() {
            Some(host) => host.notice(&self.hub, kind, text, facts),
            None => Some(text.to_string()),
        }
    }

    /// [`Harness::noticed`] over the batch a wake composed from one source — the queue's sentences,
    /// the monitor firings, the jobs that settled.
    fn noticed_all(&self, kind: &str, lines: Vec<String>, facts: serde_json::Value) -> Vec<String> {
        if lines.is_empty() {
            return lines;
        }
        lines
            .into_iter()
            .filter_map(|line| self.noticed(kind, &line, facts.clone()))
            .collect()
    }

    /// **A notice this harness injects, said once and dropped if a layer dropped it.**
    ///
    /// The kinds are `letibot_plugin::notice`'s; the sentence is the site's own.
    fn append_notice(&mut self, kind: &str, text: &str) -> Result<(), HarnessError> {
        let Some(text) = self.noticed(kind, text, serde_json::json!({})) else {
            return Ok(());
        };
        self.record_notice(&text)
    }

    /// The row itself, whoever decided the sentence.
    fn record_notice(&mut self, text: &str) -> Result<(), HarnessError> {
        // The harness talking to itself. Recorded as such, so a §5.7 salvage notice
        // or an intent nudge can never be read back as the operator authorising the
        // action it is about — which is the whole reason [`Speaker`] has three
        // values rather than one.
        self.trail.say(Speaker::Agent, text, Some(Instant::now()));
        let item = TranscriptItem::User {
            speaker: letibot_transcript::Speaker::Agent,
            parts: vec![UserPart::Text { text: text.into() }],
        };
        let mut sink = CapturingSink::new(self.hub.clone());
        self.session
            .append_items(&self.engine, std::slice::from_ref(&item), &mut sink)?;
        self.reconcile(&mut sink, std::slice::from_ref(&item));
        self.persist()
    }

    /// **Say the calls the gate admitted without asking anybody** (D16), one transcript row
    /// each.
    ///
    /// Called at the round boundary — after a round's results are on the log and before the
    /// model generates again — because that is the one point in a turn where a row can be
    /// appended without landing between a tool call and its result. `pair_tool_calls` reads
    /// the first non-`tool` row as the end of an answering window, so a notice written
    /// between them would make the model read *no result was recorded* beside the real one.
    ///
    /// A session whose gate has no admission sink (no adjudicator, or a fixture) has an empty
    /// queue and pays one lock; a session at a point that asks nothing of this kind never
    /// queues anything at all.
    fn flush_gate_admissions(&mut self) -> Result<(), HarnessError> {
        let lines: Vec<String> = {
            let mut q = self
                .gate_admissions
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            std::mem::take(&mut *q)
        };
        for line in lines {
            self.append_notice("gate", &line)?;
        }
        Ok(())
    }

    /// Attach content to the rows the log already announced (T13.1).
    ///
    /// The ids come off the events rather than being recomputed; see point 1 in the
    /// module header. A count mismatch is a bug in the pairing, not something to
    /// paper over, so it is announced as a warning on the same log the head reads.
    pub(crate) fn reconcile(&self, sink: &mut CapturingSink, items: &[TranscriptItem]) {
        // **What this session now holds** (R66). Restated here because this is the one function
        // every append goes through — the six `append_items` call sites and the compaction
        // fork's swap — so the census's number is a walk of the transcript as it is, and not a
        // counter that can drift from it. See [`crate::footprint`].
        self.restate_footprint();
        // The trail's denominator, counted where every appended batch passes.
        // `messages_scanned` has to be what was **looked at**, and this is the one
        // place that knows how much there is.
        self.trail.note_items(items.len());
        let ids = reconcile_rows(&self.hub, sink, items);
        // **And what each reply cited**, read now — the moment it landed.
        for (id, item) in ids.iter().zip(items) {
            self.publish_citations(id, item);
        }
    }

    /// **The lines a reply cites, published after its row** ([`crate::citations`]). The stored
    /// snapshot when there is one — a reply restored or carried by a fork shows what it cited
    /// when it was written — and otherwise the file as it is now, read through this session's
    /// backend and stored. Silent for a row that cites nothing.
    pub(crate) fn publish_citations(&self, item_id: &str, item: &TranscriptItem) {
        let TranscriptItem::Assistant { text, .. } = item else {
            return;
        };
        let cited = crate::citations::cited_fences(text);
        if cited.is_empty() {
            return;
        }
        let hash = hex32(&Sha256::digest(text.as_bytes()).into());
        let session = &self.cfg.session_id;
        let stored = self
            .store
            .as_ref()
            .and_then(|s| s.citations(session, &hash).ok().flatten())
            .and_then(|j| serde_json::from_str::<Vec<letibot_sessionlog::Citation>>(&j).ok());
        let citations = match stored {
            Some(c) => c,
            None => {
                let backend = &self.runtime.backend;
                let read = |p: &str| backend.read(p).map_err(|e| e.to_string());
                let fresh: Vec<letibot_sessionlog::Citation> = cited
                    .iter()
                    .map(|c| crate::citations::snapshot(c, read))
                    .collect();
                if let Some(s) = self.store.as_ref()
                    && let Ok(json) = serde_json::to_string(&fresh)
                    && let Err(e) = s.put_citations(
                        session,
                        &hash,
                        &json,
                        letibot_sessionlog::registry::now_ms(),
                    )
                {
                    eprintln!("  {session} · a reply's citations were not stored: {e}");
                }
                fresh
            }
        };
        self.hub
            .publish(letibot_sessionlog::SessionEvent::Citations {
                item_id: item_id.to_string(),
                citations,
            });
    }

    /// **Run the summary turn and pair every row it appended** — the one door both
    /// callers go through, because the two of them forgetting is exactly the defect
    /// this closes.
    ///
    /// `run_compaction` appends the summary INSTRUCTION and the model's ANSWER to the
    /// session through the sink, and a sink announces each row it is handed.
    /// `reconcile` is the only thing that hands the hub the row's BODY. Two callers
    /// (`compact_inner`'s ordinary arm and `reseat`) ran `run_compaction` against this
    /// session's own sink and never reconciled it — the ids stayed in the sink and
    /// were dropped with it, so the two rows were announced and never filled.
    ///
    /// **The hole was in the DAEMON's view, so nothing on a head could repair it**:
    /// every later snapshot announced the same two rows with `item: None`, and
    /// `/resync` serves that same view — which is why the operator ran it three times
    /// and the `2 row(s) announced … and never filled` line did not move. MEASURED on
    /// their live session 2026-10-05: rows `#t45.510` (the instruction) and `#t45.511`
    /// (the summary), both stored with their bodies and both served body-less, until
    /// the daemon was restarted and the resume's republish filled them.
    ///
    /// The slice is taken from the session AFTER the turn, so the pairing is the
    /// session's own order — the same positional zip `reconcile` documents, and it
    /// covers the salvage notices a say-nothing summary turn appends as well as the
    /// instruction and the answer.
    ///
    /// An associated function rather than a method, and the reason is the borrow:
    /// `answerer` already holds `&self.provider` and `&self.prefix` for the call, so
    /// a `&self` receiver here would overlap it. `hub` is passed in for the same
    /// reason the row pairing is a free function.
    pub(crate) fn run_summary_turn(
        hub: &Arc<Hub>,
        engine: &mut TurnEngine,
        session: &mut Session,
        sink: &mut CapturingSink,
        answerer: &letibot_turn::compaction::Answerer<'_>,
        ladder: &mut letibot_http::retry::Ladder,
    ) -> Result<CompactionOutcome, HarnessError> {
        let mark = session.items.len();
        // **The summary turn is a provider call, so it walks the same ladder** — and its
        // failure is said the same way, with the count in it. A compaction that failed on a
        // blip used to end the session's only chance to fold, and the operator read
        // `compaction FAILED: http io: …` with no way to tell one attempt from six.
        let outcome =
            run_compaction(engine, session, sink, answerer, ladder).map_err(|e| match e {
                TurnFailure::Http(e) => HarnessError::Endpoint {
                    said: ladder.ending(&e),
                    cause: e,
                },
                other => HarnessError::Turn(other),
            })?;
        let added: Vec<TranscriptItem> = session.items[mark..].to_vec();
        reconcile_rows(hub, sink, &added);
        Ok(outcome)
    }

    /// Write every ledger row that has not reached the store yet.
    ///
    /// Persist and announce the todo list, if the model wrote one since last time.
    ///
    /// The tool mutates the board; this is the harness noticing. Called at every
    /// round boundary so the pane moves with the work rather than after it — a
    /// plan that reaches the head only when the turn ends is a plan the operator
    /// watches being executed blind. A storeless session announces but does not
    /// persist: the list is still real for the life of the session, and saying so
    /// once in the open notes would be the honest form; silently pretending it
    /// persisted would not be.
    /// **[`Harness::flush_todos`] for a caller with no error to return** — the three doors that
    /// used to `eprintln!` the failure and nothing else (R67 item 15): the operator's own rows,
    /// the review queue's board, and a fired condition being spent. Each is a plan the store does
    /// not hold, and the last is the worst of them: a condition that fired and was not written
    /// down fires AGAIN after a restart. So the failure reaches the session's hub as
    /// `todos_not_stored`, beside the daemon's log line.
    pub(crate) fn flush_todos_or_say(&mut self, what: &str) {
        if let Err(e) = self.flush_todos() {
            eprintln!("  todos: {what} could not be written down: {e}");
            self.import_note(
                "todos_not_stored",
                format!(
                    "the plan moved ({what}) and the store did not take it: {e}. The board you \
                     see is right for this daemon's life; after a restart it comes back as the \
                     store last held it."
                ),
            );
        }
    }

    pub(crate) fn flush_todos(&mut self) -> Result<(), HarnessError> {
        let v = self.todos.version();
        if v == self.todos_version {
            return Ok(());
        }
        let items = self.todos.snapshot();
        // **Patient (R91.6), and only then marked written (R91.7).** The version was recorded
        // BEFORE the write, so a write that failed left the next flush seeing "nothing moved"
        // and returning `Ok` without writing — and a restart then reverted the operator's
        // answers, `wait_on` edges and consumed conditions. Now a busy store is waited out as
        // the transcript's rows are, and the version moves only once the store has the board.
        if self.store.is_some() {
            let session = self.cfg.session_id.clone();
            self.store_patiently("todos", |store| store.put_todos(&session, &items))?;
        }
        self.todos_version = v;
        // **AND SAY IT LOUDLY WHEN A PERSON STARTS OR STOPS BEING WAITED ON** — *"the heads show
        // loud notification that im waited"*. Compared across flushes, so a re-publish of the same
        // board says nothing and each question is announced once when it is asked and once when it
        // is settled (answered, detached by the model, or its row closed). The settings row moves
        // with it, because that is what a head attaching later reads.
        let waiting = crate::operator_ask::waiting(&items);
        if waiting != self.asks_said {
            for (row, question) in waiting.iter().filter(|w| !self.asks_said.contains(w)) {
                self.hub.publish(SessionEvent::Warning {
                    code: "waiting_on_operator".into(),
                    detail: crate::operator_ask::asked_words(row, question),
                    compaction: None,
                });
            }
            for (row, question) in self.asks_said.iter().filter(|w| !waiting.contains(w)) {
                self.hub.publish(SessionEvent::Warning {
                    code: "operator_question_settled".into(),
                    // **And WHY it stopped** — an answer, a take-back and a row that closed are
                    // three different facts, and the person who was asked can only tell their own
                    // answer landed if the notice says so. See `operator_ask::settled_words`.
                    detail: crate::operator_ask::settled_words(&items, row, question),
                    compaction: None,
                });
            }
            self.asks_said = waiting;
            self.publish_settings();
        }
        self.hub.publish(SessionEvent::TodosUpdated {
            todos: items.into_iter().map(Self::todo_entry).collect(),
        });
        Ok(())
    }

    /// The store's todo shape, as the wire spells it. Two types because the two
    /// crates cannot share one — the protocol does not grow a storage dependency
    /// to save a `struct` — and one conversion because the fields are the same
    /// four words.
    pub(crate) fn todo_entry(t: TodoItem) -> TodoEntry {
        TodoEntry {
            content: t.content,
            // **The store's condition, as the WIRE spells it.** A `match` rather than a
            // pass-through because the two crates keep their own copy of the type on purpose,
            // so a variant has to be spelled on both sides — which is what makes an
            // unevaluable condition a compile error instead of a row that waits for ever.
            when: t.when.map(|c| match c {
                letibot_tokencore::store::TodoCondition::Job { handle } => {
                    letibot_sessionlog::event::TodoCondition::Job { handle }
                }
            }),
            // **And its EDGES, by the same rule and for a second reason.** The `match` is the
            // compile-time half (a need kind this head cannot decode would be a row nobody could
            // act on); the field itself is what makes the pane able to CARRY an edge rather than
            // only draw one — see `TodoEntry::needs`.
            needs: t
                .needs
                .into_iter()
                .map(|need| match need {
                    letibot_tokencore::store::TodoNeed::Row { content } => {
                        letibot_sessionlog::event::TodoNeed::Row { content }
                    }
                    // **The child's id travels verbatim**, the same rule `TodoBy::Parent`'s author
                    // string keeps: it is the handle `task_result` collects by, so a head that
                    // shortened it would hand the model a name nothing answers to.
                    letibot_tokencore::store::TodoNeed::Child { id } => {
                        letibot_sessionlog::event::TodoNeed::Child { id }
                    }
                    // **And the job handle travels verbatim too**, for the same reason one layer
                    // down: it is the name `job_output` and `job_kill` are given, and the pane has
                    // to be able to carry an edge into a job rather than only draw one.
                    letibot_tokencore::store::TodoNeed::Job { handle } => {
                        letibot_sessionlog::event::TodoNeed::Job { handle }
                    }
                    letibot_tokencore::store::TodoNeed::Monitor { name } => {
                        letibot_sessionlog::event::TodoNeed::Monitor { name }
                    }
                    // **The room's item number crosses verbatim**, the same rule the child's id and
                    // the job handle keep: it is what `room`'s `done`, `beat` and `abandon` take,
                    // and the head draws the edge so a person can see the row is quiet on purpose.
                    letibot_tokencore::store::TodoNeed::Room { item, working } => {
                        letibot_sessionlog::event::TodoNeed::Room { item, working }
                    }
                    // **An unknown kind stays unknown on the wire**, which is what lets a head
                    // that does not know one pass it back to a daemon that does rather than
                    // dropping the edge it cannot read.
                    // **The operator's question travels whole** — the options are what the head's
                    // card offers, and an answer is checked against them on the way back.
                    letibot_tokencore::store::TodoNeed::Operator { question, options } => {
                        letibot_sessionlog::event::TodoNeed::Operator { question, options }
                    }
                    letibot_tokencore::store::TodoNeed::Answered { question, reply } => {
                        letibot_sessionlog::event::TodoNeed::Answered { question, reply }
                    }
                    letibot_tokencore::store::TodoNeed::Unknown => {
                        letibot_sessionlog::event::TodoNeed::Unknown
                    }
                })
                .collect(),
            by: match t.by {
                letibot_tokencore::store::TodoBy::Model => letibot_sessionlog::event::TodoBy::Model,
                letibot_tokencore::store::TodoBy::Operator => {
                    letibot_sessionlog::event::TodoBy::Operator
                }
                // **The author string TRAVELS**: `Parent s-…` is the same string in the store and
                // on the wire, which is what lets a head or a `sqlite3` reader name the session
                // that wrote a child's row without a second lookup.
                letibot_tokencore::store::TodoBy::Parent(who) => {
                    letibot_sessionlog::event::TodoBy::Parent(who)
                }
                // **The queue's own word, and it is the same word on both sides** — the fourth
                // author is a name and not a string, so there is nothing to carry. A head that
                // predates it cannot decode it, which is what moved `PROTOCOL_VERSION` to 42.
                letibot_tokencore::store::TodoBy::Queue => letibot_sessionlog::event::TodoBy::Queue,
            },
            status: match t.status {
                letibot_tokencore::store::TodoStatus::Pending => WireTodoStatus::Pending,
                letibot_tokencore::store::TodoStatus::InProgress => WireTodoStatus::InProgress,
                letibot_tokencore::store::TodoStatus::Completed => WireTodoStatus::Completed,
                letibot_tokencore::store::TodoStatus::Postponed => WireTodoStatus::Postponed,
                letibot_tokencore::store::TodoStatus::Cancelled => WireTodoStatus::Cancelled,
            },
        }
    }

    /// The database enforces the append-only property itself — a trigger refuses a
    /// `seq` that is not the next one and a `tok_offset` that does not continue the
    /// previous row — so a divergence between the ledger and the store is a failed
    /// INSERT rather than a quiet inconsistency.
    /// Wait, unless this session is going away. `false` means it is: the hub
    /// closed under us and there is nobody left to answer.
    ///
    /// Sliced rather than one `sleep`, because the longest wait here is
    /// thirty-two seconds and a daemon asked to stop during one should not have
    /// to sit it out — `Ctrl+C Ctrl+C` closes the registry from the connection's
    /// thread, and this is the worker noticing.
    fn sleep_unless_closed(&self, total: std::time::Duration) -> bool {
        let slice = std::time::Duration::from_millis(250);
        let deadline = std::time::Instant::now() + total;
        while std::time::Instant::now() < deadline {
            if self.hub.is_closed() {
                return false;
            }
            std::thread::sleep(slice.min(deadline - std::time::Instant::now()));
        }
        !self.hub.is_closed()
    }

    pub(crate) fn persist(&mut self) -> Result<(), HarnessError> {
        self.persist_reporting(&mut |_, _| {})
    }

    /// **One store write, with the patience [`Self::persist_reporting`] has** (R91.6): the
    /// store's own minute of busy retries, and past that a wait — said on the session's log
    /// every two minutes — until the store frees, the session closes, or
    /// [`STORE_BUSY_PATIENCE`] runs out counted from the first busy failure. Anything that is
    /// not busy is returned at once. `f` must be safe to run again: a busy write committed
    /// nothing.
    pub(crate) fn store_patiently<T>(
        &self,
        what: &str,
        mut f: impl FnMut(&Store) -> letibot_tokencore::store::Result<T>,
    ) -> Result<T, HarnessError> {
        let Some(store) = &self.store else {
            return Err(HarnessError::Store(format!(
                "{what}: this session has no store"
            )));
        };
        let mut first_busy: Option<std::time::Instant> = None;
        let mut said_at: Option<std::time::Instant> = None;
        loop {
            match f(store) {
                Ok(t) => return Ok(t),
                Err(e) if e.is_busy() => {
                    let since = *first_busy.get_or_insert_with(std::time::Instant::now);
                    let waited = since.elapsed();
                    if waited >= STORE_BUSY_PATIENCE {
                        return Err(HarnessError::Store(format!(
                            "{what}: {e} — still busy after waiting {}s",
                            waited.as_secs()
                        )));
                    }
                    if said_at.is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(120)) {
                        said_at = Some(std::time::Instant::now());
                        self.hub.publish(SessionEvent::Warning {
                            code: "store_busy_wait".into(),
                            detail: format!(
                                "the session store is busy (`{e}`) — another daemon holds its \
                                 write lock. The {what} write is kept in memory and made when it \
                                 frees; this session is waiting, not failing ({}s so far).",
                                waited.as_secs()
                            ),
                            compaction: None,
                        });
                    }
                    if !self.sleep_unless_closed(std::time::Duration::from_secs(5)) {
                        return Err(HarnessError::Store(format!(
                            "{what}: {e} — the session closed while waiting for the store"
                        )));
                    }
                }
                Err(e) => return Err(HarnessError::Store(format!("{what}: {e}"))),
            }
        }
    }

    /// **The rows not yet written, written in chunks of [`PERSIST_CHUNK`], one transaction
    /// each** — and `progress(done, total)` told after every chunk, in rows of this call.
    ///
    /// It used to be one autocommit `INSERT` per row, which is free for the two or three rows
    /// a round appends and ruinous for a fork that carries the whole conversation: the rano
    /// session's `/reseat`, 2026-10-12, spent 6.5 minutes writing 971 rows that took two
    /// seconds to render. See [`letibot_tokencore::store::Store::append_items`] for where the
    /// time went. Chunked rather than one transaction for the lot so the write lock every
    /// daemon on the box shares is held for a bounded stretch at a time, and so a long carry
    /// can say how far it has got between chunks.
    ///
    /// `persisted` advances only after a chunk has committed, so a chunk that fails leaves
    /// the count at the last row that is really in the store, and the next call retries from
    /// there.
    ///
    /// **A busy store is waited out here, not failed on** (`STORE_BUSY_PATIENCE`): the store
    /// retries a busy chunk for a minute itself, and past that this loop resyncs `persisted`
    /// from the store, says `store_busy_wait` on the session's hub at most every two minutes,
    /// and tries again — giving up only when the session closes or the patience runs out.
    pub(crate) fn persist_reporting(
        &mut self,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Result<(), HarnessError> {
        let Some(store) = &self.store else {
            self.persisted = self.session.ledger.rows().len();
            return Ok(());
        };
        let rows = self.session.ledger.rows();
        let start = self.persisted;
        let total = rows.len().saturating_sub(start) as u64;
        let started = std::time::Instant::now();
        let mut said_at: Option<std::time::Instant> = None;
        while self.persisted < rows.len() {
            let from = self.persisted;
            let to = (from + PERSIST_CHUNK).min(rows.len());
            let mut chunk: Vec<(&TranscriptItem, &letibot_tokencore::LedgerRow, &[_])> =
                Vec::with_capacity(to - from);
            for (i, row) in rows.iter().enumerate().take(to).skip(from) {
                let item = self.session.items.get(i).ok_or_else(|| {
                    HarnessError::Store(format!(
                        "ledger row {i} ({}) has no item; the two are supposed to be \
                         appended together",
                        row.item_id
                    ))
                })?;
                let tokens = self
                    .session
                    .ledger
                    .item_tokens(i)
                    .ok_or_else(|| HarnessError::Store(format!("no tokens for row {i}")))?;
                chunk.push((item, row, tokens));
            }
            match store.append_items(&self.transcript_id, from as u32, chunk) {
                Ok(()) => {
                    self.persisted = to;
                    progress((to - start) as u64, total);
                }
                // **A busy store is waited for, never died of.** pg-noop's model, 2026-10-12
                // ~01:20: *"the child died on sqlite: database is locked in the harness's own
                // store … both cost an agent's worth of work."* The store has already retried
                // for a minute on top of its busy timeout; past that the turn WAITS, says so on
                // the session's own log, and only gives up when the session is going away or
                // the wait is past `STORE_BUSY_PATIENCE` — a store wedged for half an hour is a
                // fault to report, not a contention to sit out.
                Err(e) if e.is_busy() => {
                    // A chunk is one transaction and a busy one committed nothing, but the
                    // store is the authority: resume from what it holds, or the next attempt
                    // re-sends a row and the append-only trigger refuses it.
                    if let Ok(n) = store.item_count(&self.transcript_id) {
                        self.persisted = (n as usize).clamp(from, rows.len());
                    }
                    let at = self.persisted;
                    let waited = started.elapsed();
                    if waited >= STORE_BUSY_PATIENCE {
                        return Err(HarnessError::Store(format!(
                            "row {at}: {e} — still busy after waiting {}s; nothing from row \
                             {at} on is stored",
                            waited.as_secs()
                        )));
                    }
                    if said_at.is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(120)) {
                        said_at = Some(std::time::Instant::now());
                        self.hub.publish(SessionEvent::Warning {
                            code: "store_busy_wait".into(),
                            detail: format!(
                                "the session store is busy (`{e}`) — another daemon holds its \
                                 write lock. Rows from {at} on are kept in memory and written \
                                 when it frees; this session is waiting, not failing \
                                 ({}s so far).",
                                waited.as_secs()
                            ),
                            compaction: None,
                        });
                    }
                    if !self.sleep_unless_closed(std::time::Duration::from_secs(5)) {
                        return Err(HarnessError::Store(format!(
                            "row {at}: {e} — the session closed while waiting for the store"
                        )));
                    }
                }
                Err(e) => {
                    if let Ok(n) = store.item_count(&self.transcript_id) {
                        self.persisted = (n as usize).clamp(from, rows.len());
                    }
                    return Err(HarnessError::Store(format!(
                        "rows {from}..{to} ({}..): {e}",
                        rows[from].item_id
                    )));
                }
            }
        }
        self.seal_fork();
        Ok(())
    }

    /// The round's prompt size onto the session row. See the call in `run_rounds`
    /// for why this is the number a head shows as the session's context.
    fn persist_context(&self, metrics: &TurnMetrics, rendered_at: u64) -> Result<(), HarnessError> {
        let Some(store) = &self.store else {
            return Ok(());
        };
        store
            .set_context(
                &self.cfg.session_id,
                Some(metrics.prompt_tokens),
                Some(metrics.cached_tokens),
                // **The same ledger the in-memory anchor above just used**, so a
                // restart recovers what this daemon had rather than a second
                // opinion about it: the ledger length of the PROMPT the count
                // measured (R72), which the round's reply was appended after. A
                // restart adds those rows back as `recovered_scale` describes.
                Some(rendered_at),
            )
            .map_err(|e| HarnessError::Store(format!("context: {e}")))
    }
}

/// How many times a round is re-attempted when the model endpoint fails.
///
/// **The default for [`Config::http_retries`], and only that** — the RULE is
/// [`letibot_http::retry::Policy`], which is where *what is retryable, and for how long* is
/// written down (three classes, a bounded ladder, and a sentence for each ending). This
/// constant is the number the daemon hands it.
///
/// Five, doubling from a second: 1, 2, 4, 8, 16 — six calls, 31 seconds of waiting. That is
/// the measured 29–31 s llama.cpp cold reload, which is the longest legitimate silence the
/// ladder exists for, and half the 63 s it used to be: the count no longer has to carry the
/// classification, because a refusal is now refused on the first call.
///
/// A caller that already KNOWS the endpoint is not there — a test pointed at a dead port —
/// sets it to 0 and is told so at once. That minute was real: seven tests in `compact.rs`
/// took 63.7 seconds of wall clock for 3.5 seconds of CPU, all of it one of them walking
/// this ladder.
pub const MAX_HTTP_RETRIES: u32 = letibot_http::retry::STANDARD_RETRIES;

/// **Did the MODEL refuse, rather than the wire failing?**
///
/// The discriminator `Harness::fall_back_from` turns on: a `429` or a `5xx` and nothing else.
/// A `408` is the one that looks like it and is not — the server waited too long for US, which
/// is the route rather than a statement about the model — and a transport failure says nothing
/// about it either. The whole of the argument is
/// [`letibot_http::retry::Policy::the_model_refused`], which is where it now lives so that a
/// reader changing what counts as weather sees the retry rule in the same file.
fn model_refused(e: &letibot_turn::HttpError) -> bool {
    letibot_http::retry::Policy::the_model_refused(e)
}

/// One outage of one model endpoint, and the single row it is worth.
///
/// ```text
/// session · model_endpoint_retry · the model endpoint at api.deepseek.com did not answer…   10:31:22
/// session · model_endpoint_retry · the model endpoint at api.deepseek.com did not answer…   10:31:23
/// session · model_endpoint_retry · the model endpoint at api.deepseek.com did not answer…   10:31:25
/// ```
///
/// A row per attempt is a wall of the same sentence with a different number in it, and a counter
/// means the row's text has to be current. A note on the screen cannot be revised: the head files a
/// warning under `(code, ts, detail)`, so a second publish carrying a bigger count is a SECOND row.
///
/// So the count is written where it is already final. The record is opened by the first failed
/// attempt and changed by every later one and published never, which is the whole of the fix, and it
/// is told once when the round ends. An endpoint that answered says `answered again after N
/// attempts`, because a silent recovery is how somebody spends an afternoon believing it is still
/// down; a ladder that gave up says the count, when, and the provider's own words; and a `[fallback]`
/// that moved the session is the row, because two rows about one outage is the wall again.
///
/// A failure the ladder never retried is not an outage and writes nothing: with no retries the
/// turn's own `turn_failed` is the whole of the news. Told once structurally, because
/// [`Harness::settle_outage`] TAKES the record, so no path can tell one outage twice however many
/// ways it ends. A second outage is a second record, since two outages are not one counter.
pub(crate) struct Outage {
    /// The host that did not answer, as [`retry_host`] spelled it. A failure against a DIFFERENT
    /// host is a different outage — the operator's `/models` and a `[fallback]` both move this,
    /// and two endpoints' attempts are not one count.
    host: String,
    /// How many attempts of this outage have failed.
    attempts: u32,
    /// When the first one did, Unix ms — the outage's start, for the duration.
    began_ms: u64,
    /// When the last one did, Unix ms — the `last at 10:31:25`.
    last_ms: u64,
    /// The provider's own sentence for the LAST attempt: the text a bug report wants.
    said: String,
}

impl Outage {
    /// **The row for an outage that ended without an answer** — the count, when the last attempt
    /// was, and the provider's own words.
    ///
    /// `(attempt 3, last at 10:31:25): http 502: …` is one clause and one colon, so the head's
    /// one-line card keeps the count and the time in front of the cut and puts the provider's
    /// sentence behind the click — which is the shape the operator asked for.
    fn did_not_answer(&self) -> String {
        format!(
            "the model endpoint at {host} did not answer (attempt {n}{last}): {said}",
            host = self.host,
            n = self.attempts,
            last = self.last_at(),
            said = self.said,
        )
    }

    /// **The row for an outage that ended because the endpoint came back** — the count, how long
    /// it was out, and the last thing it said before that (which is what a click opens and what
    /// gets pasted into a bug report).
    fn answered(&self, now_ms: u64) -> String {
        format!(
            "the model endpoint at {host} answered again after {n} {word} / {span}. \
             The last thing it said{last}, was: {said}",
            host = self.host,
            n = self.attempts,
            word = if self.attempts == 1 {
                "attempt"
            } else {
                "attempts"
            },
            span = outage_span(now_ms.saturating_sub(self.began_ms)),
            last = self.last_at().replace(", last at", ", at"),
            said = self.said,
        )
    }

    /// `, last at 10:31:25`, or nothing when the clock cannot be read — an empty parenthesis is
    /// worse than the count alone.
    fn last_at(&self) -> String {
        match outage_clock(self.last_ms) {
            Some(at) => format!(", last at {at}"),
            None => String::new(),
        }
    }
}

/// **A wall clock, `10:31:25`** — for the sentence that says when the last attempt failed.
///
/// `None` when the clock cannot be read, and the caller then says the count without the time.
/// The same `localtime_r` shape as `seated_at`, for its reason: a harness is opened from more
/// than one thread, and the `_r` form hands back nothing shared.
fn outage_clock(ms: u64) -> Option<String> {
    let secs = (ms / 1000) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `localtime_r` writes into `tm` and reads `secs`; both are owned here.
    let ok = unsafe { !libc::localtime_r(&secs, &mut tm).is_null() };
    ok.then(|| format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec))
}

/// **How long an outage lasted**, in the words a sentence reads with: `14s`, `2m 14s`, `1h 02m`.
fn outage_span(ms: u64) -> String {
    let secs = ms / 1000;
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m {:02}s", secs / 60, secs % 60),
        _ => format!("{}h {:02}m", secs / 3600, (secs % 3600) / 60),
    }
}

/// **The retry notice for a session that moves because the model is OUT.**
///
/// Names what refused, what it said, where the session goes and WHEN, and how to come back —
/// five facts, because this is a model change the operator did not type and every one of them is
/// a question they will have. The WHEN is the one that changed on 2026-10-26: the switch takes at
/// the end of this turn, never inside it (see [`Harness::set_provider`]), so the notice must not
/// promise the round continues — it does not. **The WORK continues**, which is a different promise
/// and the daemon's own to keep: [`Harness::continue_after_fallback`] opens the next turn on the
/// model named here.
fn fallback_notice(
    was: &str,
    now: &str,
    e: &letibot_turn::HttpError,
    opens_a_turn: bool,
    outage: Option<&Outage>,
) -> String {
    // **One continuation per fallback, so this branch is real**: a chain whose second name is also
    // out moves the session again and opens nothing, and a sentence promising a turn nobody will
    // run is the kind of lie this tree keeps deleting.
    let on = if opens_a_turn {
        format!(
            "the daemon opens the next turn itself, on {now}, and the work goes on without you."
        )
    } else {
        format!(
            "the daemon opens no second turn of its own — this one was already the continuation a \
             fallback earns — so the next thing you send is what goes to {now}."
        )
    };
    // **The count, when there is one to give** — this notice IS the outage's row when a
    // fallback fires (`Harness::settle_outage` finds the record taken), so the number the
    // retry row would have carried belongs in the sentence that says the ladder is over.
    let after = match outage {
        Some(o) if o.attempts > 1 => format!(" after {} attempts{}", o.attempts, o.last_at()),
        _ => String::new(),
    };
    format!(
        "the model at {was} is out — {e} — and the retry ladder has nothing left to wait \
         for{after}, so this turn ends here and the session moves to {now}, the first name in \
         `[fallback]` that answers. The switch takes when the turn ends and not inside it, \
         because a new model's first request has to follow a user message: {on} Nothing was \
         recorded by the turn that failed. `/models` moves the session back, and the list is in \
         `[fallback] models` in providers.toml."
    )
}

/// **The notice that OPENS the turn a fallback earned** — the model's own copy, as a user item in
/// the harness's voice (`Speaker::Agent`), which is the same shape a plan-check's and a wake's
/// notice take.
///
/// The register is the harness's: `[fallback]` first, like `[todo]`, `[workspace]` and
/// `[reseat]`, so a reader can tell a notice the daemon wrote from the operator's words. What it
/// carries is the three facts the model cannot recover from the transcript alone: **what
/// refused** (this is not the model's own failure), **where it is now** (a different model is
/// answering), and **that nothing was lost** — the failed round committed nothing, so the tool
/// results above it are the work's real state and re-doing them is waste the operator pays for.
fn fallback_continuation_notice(was: &str, now: &str, e: &letibot_turn::HttpError) -> String {
    format!(
        "[fallback] {was} refused ({}); continuing this task on {now}. The turn before this one \
         ended on that refusal and nothing it did was lost — its tool results are above. Pick the \
         work up where it stopped and carry on; do not repeat what is already done.",
        refusal_word(e)
    )
}

/// **The refusal in the few words a notice can afford** — `http 429`, or the kind of failure for
/// one that is not a status at all.
///
/// Deliberately NOT the provider's body: the body goes to the operator in [`fallback_notice`],
/// where a person reading a quota message is worth the tokens. This is the model's copy, and what
/// it needs to know is *the model moved*, not what the old one said.
fn refusal_word(e: &letibot_turn::HttpError) -> String {
    match e {
        letibot_turn::HttpError::Status { code, .. } => format!("http {code}"),
        letibot_turn::HttpError::Io(_) => "the connection failed".to_string(),
        letibot_turn::HttpError::Malformed(_) => "the answer did not parse".to_string(),
    }
}

/// **The host a retry warning names, for the route this turn is on.**
///
/// Split out of the retry loop so the choice is testable on its own, because the wrong
/// answer here is expensive in a way a passing build cannot show: the loop used to name
/// `cfg.endpoint` — the LOCAL endpoint — on a path that also serves cloud turns, so the
/// operator read
///
///   the model server at 127.0.0.1:8080 did not answer: ... Temporary failure in name
///   resolution
///
/// on 2026-10-01 while `llama-server` was answering `/health` with 200 and
/// `letibot --status` reported it `ok`. The failure was a resolver lookup for a provider
/// host; the message sent them to debug the one component that was working.
///
/// So: the local route names the local endpoint, and a cloud route is asked for its own
/// host — the URL it actually posts to, which the operator's key file may have moved.
fn retry_host(provider: Option<&dyn letibot_backend::MessagesBackend>, local: &Endpoint) -> String {
    match provider {
        None => local.authority(),
        Some(p) => p.authority(),
    }
}

/// **What a context-length refusal told us**, in the provider's own numbers.
///
/// `requested` and `limit` are the two numbers the body names — the prompt size it
/// refused and the window it refused against — either of which a body may carry only
/// one of. They are the PROVIDER's counts, not this box's ledger, and a caller that
/// wants to compare them with its own must say which is which rather than silently
/// pairing them (the same rule `Config::ledger_scale` exists for).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ContextLengthRefusal {
    requested: Option<u64>,
    limit: Option<u64>,
}

/// **The HTTP status a failure names, when it is a provider refusing what was SENT.**
///
/// 4xx is that class: the request reached a model that read it and said no. 429 is not
/// (the ladder's — it waits, and may move the session), 408 is not (the server waited too
/// long for US, which is the route), and neither is anything 5xx or transport, which is a
/// fact about the endpoint rather than about what was sent. Read by
/// [`Harness::switch_back_notice`].
///
/// **Two shapes**, because a failure now reaches a caller either as it left the engine
/// ([`TurnFailure::Http`]) or as the ladder's account of it ([`HarnessError::Endpoint`]), and
/// a question that answered only the first would silently stop advising a switch back on
/// exactly the failures an operator most wants advice on — a `400` from the model they just
/// moved to.
pub(crate) fn provider_refusal(e: &HarnessError) -> Option<u16> {
    e.http_cause()
        .and_then(letibot_http::retry::Policy::refusal_of_what_was_sent)
}

/// **Is this HTTP failure the context wall, spoken by the thing that owns it?**
///
/// A provider refuses an over-long prompt with a STATUS, not with a wall event —
/// `HttpError::Status` renders as `http {code}: {body}` and lands as a backend error,
/// so every recovery gated on `HarnessError::ContextWall` (the turn tail's
/// `out.is_ok() || wall`) reads false and no door reacts. MEASURED 2026-10-09,
/// session `s-1789919514688401228` on `deepseek/deepseek-flash`: thirty consecutive
/// 400s, the first attributed to `monitor`, the last to `todo check`, none recovered.
///
/// The two measured shapes, verbatim, because a matcher is only as narrow as its
/// fixtures:
///
/// * **llama.cpp**, JSON with a type field — the structured signal, preferred:
///
///   ```text
///   {"error":{"code":400,"message":"request (1504198 tokens) exceeds the available context size (262144 tokens), try increasing it","type":"exceed_context_size_error","n_prompt_tokens":1504198,"n_ctx":262144}}
///   ```
///
/// * **an OpenAI-shaped provider**, plain text naming a context length —
///   `error.code` is absent and the numbers live in the sentence:
///
///   ```text
///   This model's maximum context length is 1048576 tokens. However, you requested 1463497 tokens (1463497 in the messages, 0 in the completion). Please reduce the length of the messages or completion. (request_id: 8a448ce7-fabf-4ffa-b350-16e6ddeed6ac)
///   ```
///
/// The rule this implements, and the reason it is ONE function rather than a `contains`
/// at the call site: parse the body as JSON first and read FIELDS
/// (`error.type = "exceed_context_size_error"`, with `n_prompt_tokens`/`n_ctx`; or
/// `error.code = "context_length_exceeded"`, with the numbers parsed out of
/// `error.message`); only a body that is not JSON at all falls back to the text shape,
/// matched by its two anchors (`maximum context length is` and `However, you requested`)
/// and NOT by a bare substring the operator has ruled against (*"prompt-text matching
/// is an overfit"*). Anything else — a 400 about invalid tokens, a 500 about a slot —
/// is a backend error and stays one: if this cannot parse, it does not guess.
fn context_length_refusal(body: &str) -> Option<ContextLengthRefusal> {
    // The structured half: a JSON error object with a field that NAMES the
    // context as the complaint. Both fields are read independently — an
    // OpenAI-shaped body carries `type: "invalid_request_error"` AND
    // `code: "context_length_exceeded"` TOGETHER, so `type` cannot be the
    // only gate or the specific field never gets its say.
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
        let err = v.get("error")?;
        let ty = err.get("type").and_then(|t| t.as_str());
        let code = err.get("code").and_then(|c| c.as_str());
        return match () {
            // llama.cpp: the type field IS the refusal; the numbers are fields.
            _ if ty == Some("exceed_context_size_error") => Some(ContextLengthRefusal {
                requested: err.get("n_prompt_tokens").and_then(|n| n.as_u64()),
                limit: err.get("n_ctx").and_then(|n| n.as_u64()),
            }),
            // OpenAI-shaped: the code field is the refusal; the numbers are in
            // the message, spelled the same way as the plain-text body below.
            _ if code == Some("context_length_exceeded") => {
                let msg = err.get("message").and_then(|m| m.as_str()).unwrap_or("");
                Some(openai_context_numbers(msg)?)
            }
            // JSON, but about something else — `invalid_request_error` on bad
            // tokens, a quota, a model name. A backend error, not the wall.
            _ => None,
        };
    }
    // The text half: not JSON at all (the measured deepseek body), so the sentence
    // is the only carrier. Both anchors must be present — either alone is a phrase a
    // body about something else could contain.
    openai_context_numbers(body)
}

/// The two numbers out of an OpenAI-shaped refusal sentence, or `None` when the
/// anchors are absent. Kept private to [`context_length_refusal`] so the text shape
/// has one spelling and one place to be narrow.
fn openai_context_numbers(body: &str) -> Option<ContextLengthRefusal> {
    let limit = number_after(body, "maximum context length is")?;
    let requested = number_after(body, "you requested")?;
    Some(ContextLengthRefusal {
        requested: Some(requested),
        limit: Some(limit),
    })
}

/// The digits immediately after `anchor`, skipping spaces — `is 1048576 tokens`,
/// `requested 1463497 tokens`. `None` when the anchor is absent or names no number,
/// because a number this box did not read is a number it must not invent.
fn number_after(body: &str, anchor: &str) -> Option<u64> {
    let at = body.find(anchor)? + anchor.len();
    let digits: String = body[at..]
        .chars()
        .skip_while(|c| c.is_whitespace())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

/// **Whether a silence notice applies at all, and about which host.**
///
/// `None` for the local route — and that is a policy, not an accident: a local round is
/// silent for 29-31 s reloading a cold GGUF and for minutes on a long prefill, both of
/// which this tree calls *progress*. Split from `SilenceWatch` so the decision can be
/// tested without starting a thread, and from `retry_host` because this one has to
/// answer `None` where that one always names something.
fn silence_host(provider: Option<&dyn letibot_backend::MessagesBackend>) -> Option<String> {
    provider.map(|p| p.authority())
}

/// **How long a CLOUD round may produce nothing before the silence is worth
/// naming.**
///
/// Fifteen seconds, and the number is a judgement rather than a measurement. What IS
/// measured (2026-10-02) is the shape it distinguishes: a degraded DeepSeek answered a
/// streaming request in **12.44 s** to first byte while rejecting a bogus key in
/// 0.33 s and a non-streaming request returned nothing at all in 25 s. So a metered
/// round can sit silent for tens of seconds and then succeed — and until this existed,
/// the operator learned that by asking somebody to run `curl -w`.
///
/// Chosen above the 12.44 s that was measured as *working*, so the notice means "this
/// is slower than a healthy provider", not "this failed".
const SILENT_ROUND_SECS: u64 = 15;

/// **The notice for a cloud round that has produced nothing, when one is
/// warranted.**
///
/// Pure — no clock, no thread — so the decision can be tested on its own. The
/// watchdog below is its only caller.
///
/// The sentence is careful about what is actually known. Nothing here can see the HTTP
/// status, so it does not claim the request was accepted: it says the round has
/// produced nothing, which is the fact, and names the host it is waiting on.
fn silence_notice(
    host: &str,
    silent: std::time::Duration,
    threshold: std::time::Duration,
) -> Option<String> {
    if silent < threshold {
        return None;
    }
    Some(format!(
        "no answer from {host} yet: this round has produced nothing in {}s. It is not a \
         failure — a provider that is slow to start answering looks exactly like this, and \
         so does one that has gone away. The retry ladder is what reports the second, so \
         nothing is being restarted and the request is still open.",
        silent.as_secs(),
    ))
}

/// **Say something WHILE a cloud round is silent**, which is the one thing the
/// operator could not interpret.
///
/// # Why a thread and not a timeout
///
/// The provider blocks its round inside `complete`, so nothing on this thread can
/// notice a silence: the round gets control back only when a delta arrives or the
/// request fails, and both of those are the END of the silence. A watchdog is the only
/// way to speak during the wait.
///
/// # Why it REPORTS rather than aborting
///
/// A first-byte *deadline* that aborted would break the case that was measured to be
/// working: 12.44 s to first byte is a degraded provider that DOES answer, and a
/// deadline short enough to catch it would fail a round that was going to succeed and
/// then retry into the same slow backend. What the operator lacked was not a kill
/// switch but a sentence; the retry ladder already handles a provider that is
/// genuinely gone, and `letibot_http::retry::Policy` is where that policy lives. So this fires
/// once, the round continues, and nothing is cancelled.
///
/// # Scope, and why it is the cloud route only
///
/// A LOCAL round is legitimately silent for 29-31 s reloading a cold GGUF and for
/// minutes on a long prefill — this tree calls both of those *progress*, and the retry
/// constants are written around them. A notice there would fire on healthy work and
/// teach the operator to ignore the code, which is worse than not having it. `None` for
/// the local route is therefore not an omission.
struct SilenceWatch {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl SilenceWatch {
    /// Start watching, or return an inert one when there is nothing worth saying.
    ///
    /// `host` is `Some` only for a metered route; see the type's own docs.
    fn start(
        hub: std::sync::Arc<letibot_sessionlog::hub::Hub>,
        host: Option<String>,
        threshold: std::time::Duration,
    ) -> Self {
        use std::sync::atomic::Ordering;
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let Some(host) = host else {
            // Inert: the flag is already set, so `finish` is a no-op and no thread
            // exists to join.
            stop.store(true, Ordering::SeqCst);
            return SilenceWatch { stop, handle: None };
        };
        let flag = stop.clone();
        let handle = std::thread::spawn(move || {
            // A short slice rather than one sleep of `threshold`, so the round can end
            // promptly without this thread outliving it — the flag is checked between
            // slices, and the thread exits within one slice of the round finishing.
            let slice = std::time::Duration::from_millis(100);
            let started = std::time::Instant::now();
            loop {
                if flag.load(Ordering::SeqCst) {
                    return;
                }
                let silent = started.elapsed();
                if let Some(detail) = silence_notice(&host, silent, threshold) {
                    hub.publish(letibot_sessionlog::SessionEvent::Warning {
                        code: "model_slow_first_byte".into(),
                        detail,
                        compaction: None,
                    });
                    // **Once.** A round that is slow for a minute should not fill the
                    // session with the same sentence; the first one carried the news.
                    return;
                }
                std::thread::sleep(slice.min(threshold.saturating_sub(silent)));
            }
        });
        SilenceWatch {
            stop,
            handle: Some(handle),
        }
    }

    /// Stop watching and wait for the thread to notice. Cheap: it is asleep on a 100 ms
    /// slice, so this costs at most that — and nothing at all for the local route, where
    /// no thread was started.
    fn finish(self) {
        use std::sync::atomic::Ordering;
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle {
            let _ = h.join();
        }
    }
}

#[cfg(test)]
mod endpoint_retry {
    //! **When the model server is not answering, take the round again.** The
    //! operator, 2026-09-17: *"implement exponential backoff and auto turn
    //! restart for when model http endpoint doesnt answer or answers with error
    //! codes except unauthenticated"*.
    // **The retry rule's own tests are the policy's**, beside it in
    // `crates/http/src/retry.rs`: what is retryable, the ladder's numbers, the billing
    // refusal and the ending sentence are one subject with one home, and a second copy here
    // is a second thing to keep true. What is left in this module is what the DAEMON does
    // with an answer — which host a warning names, what a fallback says, when a silence is
    // worth a row.
    use super::{
        ContextLengthRefusal, Outage, SilenceWatch, context_length_refusal,
        fallback_continuation_notice, fallback_notice, model_refused, outage_span, retry_host,
        silence_host, silence_notice,
    };
    use letibot_turn::{Endpoint, HttpError};

    fn status(code: u16) -> HttpError {
        HttpError::Status {
            code,
            body: String::new(),
        }
    }

    /// The exception is the whole point: a credential the server rejected is
    /// rejected identically every time, so a minute of backoff buys nothing and
    /// buries the real cause under six notices.
    /// **THE SWEEP'S JUDGEMENT, and the defect it exists for, in the operator's words: *"look at the
    /// window number 11 - stuck at the tool and i cant interrupt it"*.**
    ///
    /// MEASURED on that daemon: a `grep` was dispatched, its executor thread and its process both
    /// vanished, and the call sat `running` in the head for eight minutes. `esc esc` was dead
    /// because the interrupt stops GENERATION and generation had already ended — there was no token
    /// to stop and no boundary to reach. Nothing in the daemon looked, because settlement had three
    /// callers and every one of them was the thread that was gone.
    mod abandoned_call_sweep {
        use crate::harness::abandoned_calls;
        use letibot_transcript::{ToolCall, ToolOutcome, TranscriptItem, UserPart};

        fn assistant(call_id: &str, name: &str) -> TranscriptItem {
            TranscriptItem::Assistant {
                text: String::new(),
                tool_calls: vec![ToolCall {
                    id: call_id.into(),
                    name: name.into(),
                    arguments: "{}".into(),
                }],
                truncated: false,
            }
        }

        fn result_for(call_id: &str) -> TranscriptItem {
            TranscriptItem::ToolResult {
                call_id: call_id.into(),
                name: "grep".into(),
                outcome: ToolOutcome::Ok,
                payload: "ok".into(),
                edit: None,
                origin: None,
                media: None,
            }
        }

        /// The case that cost the operator eight minutes: asked, never answered.
        #[test]
        fn a_call_with_no_result_is_abandoned() {
            let items = vec![assistant("c1", "grep")];
            assert_eq!(
                abandoned_calls(&items),
                vec![("c1".to_string(), "grep".to_string())],
                "a call nothing answered is the whole of what this looks for"
            );
        }

        /// **And the case it must NOT touch.** An answered call is the ordinary one, and a sweep
        /// that flagged it would rewrite every finished round in the transcript.
        #[test]
        fn an_answered_call_is_not() {
            let items = vec![assistant("c1", "grep"), result_for("c1")];
            assert!(
                abandoned_calls(&items).is_empty(),
                "the result answers the call"
            );
        }

        /// **BY ID, not by position and not by name.** Two calls to the same tool are two calls, and
        /// answering the first must not discharge the second — the mistake a name-keyed check makes
        /// silently, because both rows read `grep`.
        #[test]
        fn answering_one_call_does_not_discharge_another_of_the_same_tool() {
            let items = vec![
                assistant("c1", "grep"),
                result_for("c1"),
                assistant("c2", "grep"),
            ];
            assert_eq!(
                abandoned_calls(&items),
                vec![("c2".to_string(), "grep".to_string())],
                "the second grep is still unanswered"
            );
        }

        /// The set difference and not a count: a call asked twice and answered once IS answered.
        #[test]
        fn a_call_asked_twice_and_answered_once_is_answered() {
            let items = vec![
                assistant("c1", "grep"),
                assistant("c1", "grep"),
                result_for("c1"),
            ];
            assert!(abandoned_calls(&items).is_empty());
        }

        /// A transcript with no tool calls at all — the common case — costs nothing and says nothing.
        #[test]
        fn a_conversation_with_no_calls_is_abandoned_by_nothing() {
            let items = vec![
                TranscriptItem::User {
                    parts: vec![UserPart::Text { text: "hi".into() }],
                    speaker: Default::default(),
                },
                TranscriptItem::Assistant {
                    text: "hello".into(),
                    tool_calls: Vec::new(),
                    truncated: false,
                },
            ];
            assert!(abandoned_calls(&items).is_empty());
        }
    }

    /// **"The model is out" is a narrower question than "is this worth waiting for",
    /// and the difference is what decides whether a session MOVES.**
    ///
    /// `fall_back_from` reads this, so an answer that is too wide moves a conversation to
    /// another model over a failure that had nothing to do with the model — a route that is
    /// down, a stream that did not parse, a server that waited too long for us. And one that
    /// is too narrow leaves the operator's quota exhaustion ending a turn, which is the
    /// report this whole change is.
    #[test]
    fn a_model_is_out_only_when_the_model_says_so() {
        // Out: the two the operator's plan produces, and any 5xx.
        assert!(model_refused(&status(429)), "a quota, a rate limit, a plan");
        assert!(model_refused(&status(500)));
        assert!(model_refused(&status(502)));
        assert!(
            model_refused(&status(503)),
            "a server reloading is still out"
        );
        // Not out: the wire, which the ladder waits on and this must not act on.
        assert!(!model_refused(&HttpError::Io(std::io::Error::other(
            "refused"
        ))));
        assert!(!model_refused(&HttpError::Malformed("truncated".into())));
        // And the two that look like timing and are not about the model: 408 is the
        // server waiting too long for US, and a 400 is the request.
        assert!(!model_refused(&status(408)));
        assert!(!model_refused(&status(400)));
        assert!(
            letibot_http::retry::Policy::standard()
                .after(&status(408), 0)
                .is_some(),
            "408 stays on the waiting side, which is exactly why it is not here"
        );
    }

    /// **The fallback sentence, in the words the operator needed to read.**
    ///
    /// It must name what refused, where the session goes, WHEN it goes there, and how to come
    /// back — and it must not promise the round continues, which is what it said until
    /// 2026-10-26: a model's first request has to follow a user message, so the round that met
    /// the refusal ends with it (see `Harness::set_provider`).
    ///
    /// **Two verdicts, because there are two cases**: the usual one opens the continuation turn,
    /// and a chain whose second name is also out opens nothing — the sentence must not promise a
    /// turn the tail will not run.
    #[test]
    fn a_fallback_names_the_way_back_and_says_when_the_switch_takes() {
        let out = || HttpError::Status {
            code: 429,
            body: "Weekly/Monthly Limit Exhausted".into(),
        };
        let moved = fallback_notice(
            "glm-coding/glm-5.3 (metered)",
            "deepseek/deepseek-flash",
            &out(),
            true,
            None,
        );
        assert!(moved.contains("deepseek/deepseek-flash"), "{moved}");
        assert!(
            moved.contains("Weekly/Monthly Limit Exhausted"),
            "what the provider actually said: {moved}"
        );
        assert!(
            moved.contains("[fallback]"),
            "where the choice came from: {moved}"
        );
        assert!(
            moved.contains("when the turn ends"),
            "and when it takes, which is the whole of the change: {moved}"
        );
        assert!(
            moved.contains("the daemon opens the next turn itself"),
            "and that the work goes on without the operator — the 2026-10-12 ruling: {moved}"
        );
        assert!(
            !moved.contains("the round is being taken again"),
            "a round is not re-issued on the new model any more: {moved}"
        );
        assert!(
            moved.contains("/models"),
            "and how a person undoes it: {moved}"
        );

        // **And the case where no turn is opened says so**: the session still moves, and the
        // sentence names what actually happens next at the operator's hand.
        let again = fallback_notice("glm/glm-4.6", "grok/grok-4", &out(), false, None);
        assert!(
            !again.contains("the daemon opens the next turn itself"),
            "a turn nobody will run was promised: {again}"
        );
        assert!(
            again.contains("the daemon opens no second turn of its own")
                && again.contains("the next thing you send is what goes to grok/grok-4"),
            "and the operator is told what does happen: {again}"
        );

        // **And the count, when the ladder has one to give** — this notice IS the outage's row
        // when a fallback fires, so the number the retry row would have carried is here.
        let outage = Outage {
            host: "open.bigmodel.cn".into(),
            attempts: 3,
            began_ms: 0,
            last_ms: 0,
            said: "http 429: Weekly/Monthly Limit Exhausted".into(),
        };
        let counted = fallback_notice("glm/glm-4.6", "grok/grok-4", &out(), true, Some(&outage));
        assert!(
            counted.contains("nothing left to wait for after 3 attempts"),
            "the ladder's own count belongs in the sentence that says it is over: {counted}"
        );
    }

    /// **The one row an outage is worth, in both of its endings** — the count, when the last
    /// attempt was, and the provider's own words, which is what the click opens.
    ///
    /// The clock is the machine's, so the assertions are about the SHAPE the head's one-line card
    /// cuts at (`(attempt 3, last at ` and then the colon) rather than about a literal time —
    /// `crates/harnessd/tests/model_outage.rs` is where the whole sentence is read off a real
    /// daemon, three real attempts and a real head.
    #[test]
    fn an_outage_is_one_row_that_carries_its_count() {
        let outage = Outage {
            host: "api.deepseek.com".into(),
            attempts: 3,
            began_ms: 0,
            last_ms: 0,
            said: "http 502: bad gateway".into(),
        };
        let failed = outage.did_not_answer();
        assert!(
            failed.starts_with("the model endpoint at api.deepseek.com did not answer ")
                && failed.contains("(attempt 3, last at "),
            "the count and when the last attempt was, in the clause a head cuts at: {failed}"
        );
        assert!(
            failed.ends_with("): http 502: bad gateway"),
            "and the provider's own sentence behind it, whole: {failed}"
        );
        let back = outage.answered(134_000);
        assert!(
            back.starts_with("the model endpoint at api.deepseek.com answered again after 3 ")
                && back.contains("attempts / 2m 14s"),
            "a recovery names how many attempts and how long it was out: {back}"
        );
        assert!(
            back.contains("The last thing it said, at ") && back.ends_with("http 502: bad gateway"),
            "and keeps the last thing it said, for the click: {back}"
        );
        // **One attempt is a word, not a count of one** — and a short outage reads in seconds.
        let once = Outage {
            host: "local".into(),
            attempts: 1,
            began_ms: 0,
            last_ms: 0,
            said: "http io: connection refused".into(),
        };
        let said = once.answered(2_400);
        assert!(said.contains("after 1 attempt / 2s"), "{said}");
    }

    /// The duration in the words a sentence reads with — `14s`, `2m 14s`, `1h 02m`.
    #[test]
    fn an_outage_span_is_read_as_a_person_reads_it() {
        assert_eq!(outage_span(0), "0s");
        assert_eq!(outage_span(14_000), "14s");
        assert_eq!(outage_span(134_000), "2m 14s");
        assert_eq!(outage_span(3_720_000), "1h 02m");
    }

    /// **The notice that OPENS the turn a fallback earned**, in the harness's own register —
    /// `[fallback]`, like `[todo]`, `[workspace]` and `[reseat]`, because a head draws the row and
    /// a reader has to be able to tell the daemon's words from the operator's.
    #[test]
    fn the_continuation_notice_names_the_refusal_the_new_model_and_that_nothing_was_lost() {
        let notice = fallback_continuation_notice(
            "glm-coding/glm-5.3",
            "glm/glm-4.6",
            &HttpError::Status {
                code: 429,
                body: "Weekly/Monthly Limit Exhausted. Your limit will reset at 2026-10-12 \
                       15:01:48."
                    .into(),
            },
        );
        assert!(
            notice.starts_with("[fallback] "),
            "the register is the harness's, and it is spelled the way every other notice is: \
             {notice}"
        );
        assert!(
            notice.contains("glm-coding/glm-5.3 refused (http 429)"),
            "what refused, by name and status: {notice}"
        );
        assert!(
            notice.contains("continuing this task on glm/glm-4.6"),
            "where the work goes on: {notice}"
        );
        assert!(
            notice.contains("nothing it did was lost"),
            "and that the failed round's tool results are still the work's state: {notice}"
        );
        assert!(
            !notice.contains("reset at"),
            "the provider's body is the OPERATOR's copy, not the model's — it is in \
             `fallback_notice`: {notice}"
        );
        assert!(
            notice.contains("do not repeat what is already done"),
            "and what to do with it: {notice}"
        );
    }

    /// **The notice fires only past the threshold, and says what it knows.**
    ///
    /// The number in the sentence is the measured silence, not the threshold — the
    /// operator needs to know how long they waited, not what the policy was.
    #[test]
    fn the_silence_notice_needs_the_threshold_passed() {
        let t = std::time::Duration::from_secs(15);
        assert!(
            silence_notice("api.deepseek.com", std::time::Duration::from_secs(14), t).is_none()
        );
        let n = silence_notice("api.deepseek.com", std::time::Duration::from_secs(22), t)
            .expect("past the threshold there is something to say");
        assert!(
            n.contains("api.deepseek.com"),
            "the host is the whole point: {n}"
        );
        assert!(n.contains("22s"), "the measured silence belongs in it: {n}");
        // And it must not claim to know what it cannot: nothing here sees the HTTP
        // status, so `accepted` would be an invention.
        assert!(!n.contains("accepted"), "{n}");
    }

    /// **A round that has produced nothing publishes exactly one notice.**
    ///
    /// MEASURED as the shape this exists for: a degraded DeepSeek took 12.44 s to its
    /// first byte and a NON-streaming request returned nothing in 25 s — and until this,
    /// the operator could not tell either from a provider that was gone without asking
    /// for `curl -w`.
    ///
    /// Once, not once per slice: a round that is slow for a minute must not fill the
    /// session with the same sentence.
    #[test]
    fn a_silent_cloud_round_publishes_one_notice() {
        use letibot_sessionlog::SessionEvent;
        let hub = letibot_sessionlog::hub::Hub::new("silence-cloud");
        let watch = SilenceWatch::start(
            hub.clone(),
            Some("api.deepseek.com".into()),
            std::time::Duration::from_millis(50),
        );
        // Long enough for several 100 ms slices — so a version that published per
        // slice would be caught here rather than in production.
        std::thread::sleep(std::time::Duration::from_millis(400));
        let said: Vec<String> = hub
            .retained()
            .iter()
            .filter_map(|e| match &e.event {
                SessionEvent::Warning { code, detail, .. } if code == "model_slow_first_byte" => {
                    Some(detail.clone())
                }
                _ => None,
            })
            .collect();
        watch.finish();
        assert_eq!(said.len(), 1, "one notice, not one per slice: {said:?}");
        assert!(said[0].contains("api.deepseek.com"), "{}", said[0]);
    }

    /// **And a local round is never nagged about silence.**
    ///
    /// A cold GGUF reload is 29-31 s and a long prefill is minutes; both are progress,
    /// and the retry constants are written around them. A notice there would fire on
    /// healthy work and teach the operator to ignore the code.
    ///
    /// Two halves, because they are two different mistakes: `silence_host` decides
    /// whether the route qualifies at all (tested here, no thread), and `SilenceWatch`
    /// decides what to do once it does (tested above and below).
    #[test]
    fn a_local_round_is_not_told_its_silence_is_suspicious() {
        use letibot_backend::{BackendCaps, BackendError, Completion, MessagesBackend, StreamFlow};
        use letibot_sessionlog::SessionEvent;

        struct Cloud(&'static str);
        impl MessagesBackend for Cloud {
            fn caps(&self) -> BackendCaps {
                BackendCaps::METERED_API
            }
            fn name(&self) -> &str {
                "deepseek"
            }
            fn model(&self) -> &str {
                "deepseek-flash"
            }
            fn authority(&self) -> String {
                self.0.into()
            }
            fn complete(
                &self,
                _req: &letibot_backend::TurnRequest<'_>,
                _on_delta: &mut dyn FnMut(&letibot_backend::Delta) -> StreamFlow,
            ) -> Result<Completion, BackendError> {
                unreachable!("this test is about the route decision, not the round")
            }
        }

        // The decision, first: the local route is not a silence to report.
        assert_eq!(silence_host(None), None, "a local prefill is progress");
        let cloud = Cloud("api.deepseek.com");
        assert_eq!(
            silence_host(Some(&cloud)).as_deref(),
            Some("api.deepseek.com")
        );
        // And it follows the backend's own URL, so a proxy is named as itself.
        assert_eq!(
            silence_host(Some(&Cloud("10.0.0.7:8443"))).as_deref(),
            Some("10.0.0.7:8443")
        );

        // And the mechanism: an inert watch says nothing however long it runs.
        let hub = letibot_sessionlog::hub::Hub::new("silence-local");
        let watch = SilenceWatch::start(
            hub.clone(),
            silence_host(None),
            std::time::Duration::from_millis(50),
        );
        std::thread::sleep(std::time::Duration::from_millis(300));
        let said = hub
            .retained()
            .iter()
            .filter(|e| {
                matches!(&e.event,
                    SessionEvent::Warning { code, .. } if code == "model_slow_first_byte")
            })
            .count();
        watch.finish();
        assert_eq!(said, 0, "a local prefill is not a fault");
    }

    /// **A retry notice must name the host that was actually contacted.**
    ///
    /// The loop named `cfg.endpoint` — the LOCAL endpoint — on a path that also serves
    /// cloud turns. MEASURED 2026-10-01: the operator read *"the model server at
    /// 127.0.0.1:8080 did not answer: ... Temporary failure in name resolution"* and went
    /// to a `llama-server` that was answering `/health` with 200, because the failure was
    /// a resolver lookup against a provider host the message never named. The retry
    /// policy was right and the diagnosis was wrong, which is why nothing caught it.
    ///
    /// The override half is the one worth pinning: the answer comes from the backend, so
    /// it follows whatever URL that backend was built with — a proxy or a rented host is
    /// named as itself, not as the preset it replaced.
    /// **The wall as the provider speaks it, recognised.** One test per measured
    /// body, verbatim, because the matcher is only as narrow as its fixtures —
    /// and one per way a body must NOT be read as the wall, because a matcher too
    /// wide sends a session through a compaction it did not need on top of a
    /// failure it did not deserve.
    #[test]
    fn the_two_measured_refusal_bodies_are_recognised_with_their_numbers() {
        // llama.cpp, JSON, the numbers as fields — the structured half.
        let llama = context_length_refusal(
            "{\"error\":{\"code\":400,\"message\":\"request (1504198 tokens) exceeds the available \
             context size (262144 tokens), try increasing it\",\"type\":\"exceed_context_size_error\",\
             \"n_prompt_tokens\":1504198,\"n_ctx\":262144}}",
        )
        .expect("the llama.cpp shape is the wall");
        assert_eq!(
            llama,
            ContextLengthRefusal {
                requested: Some(1_504_198),
                limit: Some(262_144),
            }
        );
        // An OpenAI-shaped provider, plain text, the numbers in the sentence —
        // deepseek's own words on 2026-10-09, request id and all.
        let openai = context_length_refusal(
            "This model's maximum context length is 1048576 tokens. However, you requested \
             1463497 tokens (1463497 in the messages, 0 in the completion). Please reduce the \
             length of the messages or completion. (request_id: \
             8a448ce7-fabf-4ffa-b350-16e6ddeed6ac)",
        )
        .expect("the OpenAI sentence shape is the wall");
        assert_eq!(
            openai,
            ContextLengthRefusal {
                requested: Some(1_463_497),
                limit: Some(1_048_576),
            }
        );
        // And the same refusal as JSON — OpenAI's own envelope, `code` instead
        // of `type`, the message carrying the numbers.
        let openai_json = context_length_refusal(
            "{\"error\":{\"message\":\"This model's maximum context length is 1048576 tokens. \
             However, you requested 1463497 tokens.\",\"type\":\"invalid_request_error\",\
             \"code\":\"context_length_exceeded\"}}",
        )
        .expect("the JSON spelling of the same refusal is the wall");
        assert_eq!(
            openai_json,
            ContextLengthRefusal {
                requested: Some(1_463_497),
                limit: Some(1_048_576),
            }
        );
    }

    #[test]
    fn a_body_that_is_not_a_context_refusal_is_not_the_wall() {
        // A 400 about the TOKENS, not the length — measured in the same log.
        assert!(
            context_length_refusal(
                "{\"error\":{\"code\":400,\"message\":\"Prompt contains invalid tokens\",\
                 \"type\":\"invalid_request_error\"}}"
            )
            .is_none(),
            "an invalid_request_error about tokens is a backend error"
        );
        // llama.cpp's JSON with a DIFFERENT type — the structured field is the
        // signal, not the JSON shape.
        assert!(
            context_length_refusal(
                "{\"error\":{\"code\":400,\"message\":\"no such model\",\"type\":\"model_error\"}}"
            )
            .is_none(),
            "JSON about something else stays a backend error"
        );
        // The OpenAI sentence with one anchor missing: either alone is a phrase
        // a body about something else could contain, so neither alone is the
        // wall.
        assert!(
            context_length_refusal("maximum context length is 4096 tokens, which you may raise")
                .is_none(),
            "a limit without a refusal of THIS prompt is documentation, not the wall"
        );
        assert!(
            context_length_refusal("you requested 1463497 tokens of quota, and the quota is spent")
                .is_none(),
            "a request size without the context anchor is a quota complaint"
        );
        // And the old local failure — `500 Context size has been exceeded`,
        // plain text with neither anchor — stays what it was: unparseable,
        // therefore not guessed at.
        assert!(
            context_length_refusal("Context size has been exceeded").is_none(),
            "a body this cannot parse is not guessed at"
        );
    }

    #[test]
    fn a_retry_names_the_host_that_was_contacted_and_not_the_local_one() {
        use letibot_backend::{BackendCaps, BackendError, Completion, MessagesBackend, StreamFlow};

        struct Cloud(&'static str);
        impl MessagesBackend for Cloud {
            fn caps(&self) -> BackendCaps {
                BackendCaps::METERED_API
            }
            fn name(&self) -> &str {
                "deepseek"
            }
            fn model(&self) -> &str {
                "deepseek-flash"
            }
            fn authority(&self) -> String {
                self.0.into()
            }
            fn complete(
                &self,
                _req: &letibot_backend::TurnRequest<'_>,
                _on_delta: &mut dyn FnMut(&letibot_backend::Delta) -> StreamFlow,
            ) -> Result<Completion, BackendError> {
                unreachable!("this test is about the notice, not the round")
            }
        }

        let local = Endpoint::new("127.0.0.1", 8080);
        // The local route keeps naming the local endpoint, which is what it is.
        assert_eq!(retry_host(None, &local), "127.0.0.1:8080");
        // A cloud route names ITS host — not the local one, which was the bug.
        let cloud = Cloud("api.deepseek.com");
        assert_eq!(retry_host(Some(&cloud), &local), "api.deepseek.com");
        assert_ne!(retry_host(Some(&cloud), &local), "127.0.0.1:8080");
        // And it follows the backend's own URL, so a moved endpoint is named as itself.
        let proxy = Cloud("10.0.0.7:8443");
        assert_eq!(retry_host(Some(&proxy), &local), "10.0.0.7:8443");
    }
}

/// One line, at most `n` characters, with the newlines folded — a log row inside a sentence.
pub(crate) fn one_line(s: &str, n: usize) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= n {
        flat
    } else {
        let cut: String = flat.chars().take(n).collect();
        format!("{cut}…")
    }
}

/// **Calls an assistant row asked for that no row ever answered** — `(call_id, name)`, in the order
/// they were asked.
///
/// The whole of the sweep's judgement, kept apart from the plumbing so it can be tested on its own.
/// It reads ONE transcript, which is why it is right here and not a per-call guess: an assistant row
/// that names a call is the request, a `ToolResult` row with that id is the answer, and a call
/// between the two is one nothing will ever answer.
///
/// **Linear, and that is a requirement rather than tidiness** (R67 item 14). The answered ids were a
/// `Vec` and each asked call was looked up with `iter().any`, which is O(calls × results) over the
/// whole in-memory transcript — and this runs for every held session two seconds after every
/// command or wake, ON THE WORKER. The tree the review measured had ~15k `bash` results, so one
/// sweep of one big session was on the order of 10⁸ string compares with every other session's
/// work queued behind it. A `HashSet` of borrowed ids makes it one pass and one lookup per call,
/// and nothing is cloned that is not returned.
pub fn abandoned_calls(items: &[TranscriptItem]) -> Vec<(String, String)> {
    let mut asked: Vec<(&str, &str)> = Vec::new();
    let mut answered: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for item in items {
        match item {
            TranscriptItem::Assistant { tool_calls, .. } => {
                for c in tool_calls {
                    asked.push((c.id.as_str(), c.name.as_str()));
                }
            }
            TranscriptItem::ToolResult { call_id, .. } => {
                answered.insert(call_id.as_str());
            }
            _ => {}
        }
    }
    // A call asked for TWICE and answered once is answered: the set difference, not a count.
    asked
        .into_iter()
        .filter(|(id, _)| !answered.contains(id))
        .map(|(id, name)| (id.to_string(), name.to_string()))
        .collect()
}
