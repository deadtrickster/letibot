//! The turn loop: submit, stream, dispatch a round's tool calls, append their
//! results and resubmit - with the retry ladder, outage and fallback handling
//! around it, and the restart restore that starts a turn nobody typed for.
use super::*;
use letibot_plugin::notice::Facts;

/// The one notice door, for every caller: the harness's sentence offered to the `notice_<kind>`
/// layer for the facts' kind, and `None` when a layer dropped a kind that may be dropped. Free
/// rather than a method because a `HubSteering` says its sentences where no `Harness` is behind it.
pub(crate) fn say(
    host: Option<&crate::plugins::PluginHost>,
    hub: &Arc<Hub>,
    facts: &Facts,
    text: &str,
) -> Option<String> {
    match host {
        // No layer in this session: the harness's own sentence, untouched.
        None => Some(text.to_string()),
        Some(host) => host.notice(hub, facts.kind(), text, facts.to_value()),
    }
}

/// [`say`] for a sentence a RUNNING turn reads at its next step boundary: queued only if a layer
/// left it standing. A `steer`, a `stall` and a `context_claim` may be dropped; nothing else may.
pub(crate) fn queue_mid_turn(
    host: Option<&crate::plugins::PluginHost>,
    hub: &Arc<Hub>,
    injected: &Mutex<VecDeque<String>>,
    facts: &Facts,
    text: &str,
) {
    if let Some(text) = say(host, hub, facts, text) {
        injected
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push_back(text);
    }
}

/// How a wait ended: the time passed, the session is going away, or the operator asked for a stop.
/// A retry ends at a stop; a store write is the caller's to keep or leave, and `busy_after` says which.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Waited {
    Out,
    Closed,
    Interrupted,
}

/// What a wait on a busy store leaves the write: take it again, leave it for the next store write
/// (a person stopped the turn, and the rows are in memory), or fail it - the session is going away.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum BusyWait {
    Again,
    Leave,
    Gone,
}

pub(crate) fn busy_after(w: Waited) -> BusyWait {
    match w {
        Waited::Out => BusyWait::Again,
        Waited::Interrupted => BusyWait::Leave,
        Waited::Closed => BusyWait::Gone,
    }
}

/// A `LogSink` that also hands back the item ids it saw, so a row's body can be paired with the
/// id the event named. A sink that filtered would make the head's view depend on which daemon
/// published it, so the one event it can hold is the round loop's to ask for.
pub(crate) struct CapturingSink {
    inner: LogSink,
    hub: Arc<Hub>,
    ids: Vec<String>,
    /// The last `turn_id` this sink saw start, taken off the event on its way past: a failure has
    /// to publish a terminal event for its turn, and `HarnessError` does not carry an id.
    turn_id: Option<String>,
    /// Whether a round's ending waits for [`Self::finish`] rather than going straight out. Only the
    /// round loop may hold one: for every other door the ending is the only terminal frame its turn
    /// has, and a held one a head never hears is a turn stuck on `Responding`.
    hold_end: bool,
    /// The round's ending, held: emitted before the round's calls have run and before a nudge is
    /// earned, so whether the turn ends with it is not known until the loop says.
    finish: Option<SessionEvent>,
    /// The flag the round's silence watch reads: a delta on this sink is the round speaking, and
    /// the watch's only other clock is the round's start.
    spoke: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

impl CapturingSink {
    /// Forwards everything, as `LogSink` does, and keeps the ids on the way past.
    pub(crate) fn new(hub: Arc<Hub>) -> Self {
        CapturingSink {
            inner: LogSink::new(hub.clone()),
            hub,
            ids: Vec::new(),
            turn_id: None,
            hold_end: false,
            finish: None,
            spoke: None,
        }
    }

    /// Arm the flag the silence watch reads for this round. Set by the round loop, since it is the
    /// one place that knows which round is running.
    pub(crate) fn note_speech_to(&mut self, flag: std::sync::Arc<std::sync::atomic::AtomicBool>) {
        self.spoke = Some(flag);
    }

    /// Holds the round's ending back for [`Self::finish`], which is the round loop's, since it is the
    /// only place that knows whether another round follows.
    pub(crate) fn holding_the_rounds_end(hub: Arc<Hub>) -> Self {
        let mut sink = CapturingSink::new(hub);
        sink.hold_end = true;
        sink
    }

    fn take_ids(&mut self) -> Vec<String> {
        std::mem::take(&mut self.ids)
    }

    /// Publish the round's ending, saying whether another round follows. A round that emitted no
    /// ending, or one already published, has nothing left to say.
    pub(crate) fn finish(&mut self, more: bool) {
        let Some(mut e) = self.finish.take() else {
            return;
        };
        if let SessionEvent::TurnFinished { ends_turn, .. } = &mut e {
            *ends_turn = !more;
        }
        self.hub.publish(e);
    }
}

impl EventSink for CapturingSink {
    fn emit(&mut self, event: TurnEvent) {
        match &event {
            TurnEvent::TranscriptAppended { item_id, .. } => self.ids.push(item_id.clone()),
            TurnEvent::TurnStarted { turn_id, .. } => self.turn_id = Some(turn_id.clone()),
            TurnEvent::Delta { .. } => {
                if let Some(flag) = &self.spoke {
                    flag.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            }
            _ => {}
        }
        if self.hold_end && matches!(&event, TurnEvent::TurnFinished { .. }) {
            self.finish = Some(letibot_sessionlog::from_turn_event(event));
            return;
        }
        self.inner.emit(event);
    }
}

/// Pair every id a sink captured with its row's body: the free half of `Harness::reconcile`, so the
/// summary-turn helper pairs rows by the same rule. A count mismatch is a bug in the pairing, and it
/// is announced rather than papered over.
pub(crate) fn reconcile_rows(
    hub: &Arc<Hub>,
    sink: &mut CapturingSink,
    items: &[TranscriptItem],
) -> Vec<String> {
    let ids = sink.take_ids();
    if ids.len() != items.len() {
        hub.warn(
            "record_item_pairing",
            format!(
                "{} TranscriptAppended events for {} items; the head will show \
                 empty rows. This is a daemon bug, not a transport one.",
                ids.len(),
                items.len()
            ),
        );
    }
    for (id, item) in ids.iter().zip(items) {
        hub.record_item(id, item.clone());
    }
    ids
}

// ---------------------------------------------------------------------------
// The authorisation trail
// ---------------------------------------------------------------------------

/// How one round ends, for a decision that is not the round's: the loop goes on, or the turn does.
enum RoundEnd {
    Go(Box<TurnOk>),
    Again,
    /// A cheap turn that ran out of room or reasoning with nothing kept ends here, unnudged.
    Silent(Box<letibot_turn::TurnMetrics>),
}

/// The output cap of a turn woken only by passing-by room talk: room for one short reply.
pub(crate) const CHEAP_WAKE_TOKENS: u32 = 512;

impl Harness {
    /// The loop: one user message in, one answer out, however many tool rounds that takes. This is
    /// the one door the operator's own words come through, and the only place that records them as
    /// the operator's: the speaker cannot be recovered from the transcript afterwards.
    pub fn submit(&mut self, text: &str) -> Result<Reply, HarnessError> {
        self.submit_spoken(Speaker::Operator, text)
    }

    /// The operator's words with the chips they put beside them. The files are read HERE, since the daemon
    /// writes the transcript and the row carries the `data:` URI rather than a path that may be gone. A file
    /// that could not be read is said in the row and warned; an empty list is `submit`, byte for byte.
    pub fn submit_attached(
        &mut self,
        text: &str,
        attachments: &[letibot_sessionlog::protocol::Attachment],
    ) -> Result<Reply, HarnessError> {
        self.submit_with_context(text, attachments, None)
    }

    /// The same, with what the operator pointed at in an editor: one more part after the chips, written
    /// by the DAEMON ([`crate::editor_context::part`]) so every head's prompt reads the same way.
    pub fn submit_with_context(
        &mut self,
        text: &str,
        attachments: &[letibot_sessionlog::protocol::Attachment],
        context: Option<&letibot_sessionlog::protocol::EditorContext>,
    ) -> Result<Reply, HarnessError> {
        let (mut extra, refused) = crate::attachments::parts(attachments);
        // Only what an editor captured is worth a block: a context with no refs, no open buffers and no
        // ask is a head sending a defaulted field, and a bare sentence about nothing would be noise.
        let workspace = self.cfg.workspace.to_string_lossy().into_owned();
        if let Some(part) = context.and_then(|c| crate::editor_context::part(c, &workspace)) {
            extra.push(part);
        }
        for said in &refused {
            self.warn("attachment_refused", said.clone());
        }
        if extra.is_empty() {
            return self.submit(text);
        }
        self.submit_spoken_parts(Speaker::Operator, text, extra)
    }

    /// One turn, with the speaker given. The speaker is not decoration: [`Speaker::Agent`] never
    /// authorises on its own, so a row saying the wrong speaker grants the wrong thing, which is why
    /// a parent's message has a door of its own rather than a flag on this one.
    pub(crate) fn submit_spoken(
        &mut self,
        speaker: Speaker,
        text: &str,
    ) -> Result<Reply, HarnessError> {
        self.submit_spoken_parts(speaker, text, Vec::new())
    }

    /// The same, with the parts an attachment became - see [`Harness::submit_attached`].
    fn submit_spoken_parts(
        &mut self,
        speaker: Speaker,
        text: &str,
        extra: Vec<UserPart>,
    ) -> Result<Reply, HarnessError> {
        self.trail.begin_turn();
        // the results held for this break ride in with the turn - as the harness's
        // own row, BEFORE the words that started the turn, so what the operator (or a parent)
        // said is still the last thing the model reads.
        let mut items = Vec::new();
        let riding = self.batch_riding_in();
        if let Some(now) = &riding {
            let batch = now.text();
            if !batch.is_empty() {
                self.trail.say(Speaker::Agent, &batch, Some(Instant::now()));
                items.push(TranscriptItem::User {
                    speaker: letibot_transcript::Speaker::Agent,
                    parts: vec![UserPart::Text { text: batch }],
                });
            }
        }
        self.trail.say(speaker, text, Some(Instant::now()));
        items.push(TranscriptItem::User {
            // The same speaker the trail records, one line up.
            speaker: match speaker {
                Speaker::Operator => letibot_transcript::Speaker::Operator,
                // A session talking to itself, a tool's row, a parent's message: none of
                // them is the operator's words, and none may be drawn as them.
                Speaker::Agent | Speaker::Tool => letibot_transcript::Speaker::Agent,
            },
            // The words first and whole, the chips after - a head retires its `queued`
            // echo by the row's leading lines, so nothing may be spliced in front of them.
            parts: {
                let mut parts = vec![UserPart::Text { text: text.into() }];
                parts.extend(extra);
                parts
            },
        });
        // The door is judged by whether its row went in, not by how the turn ended: the model reads
        // the batch the moment the rows are appended, and a round that fails afterwards said nothing
        // about whether it was read.
        let mut opened = false;
        let out = self.submit_items_then(items, |_| opened = true);
        self.the_batch_arrived(riding, opened);
        out
    }

    /// The shared tail has taken a turn on this harness, once per door and before the door's own act. It says
    /// only that there is a turn to end, not that one has started speaking, which is what the first words do.
    /// The tail's act rather than every caller's, since a turn taken and never ended is a state to avoid.
    pub(crate) fn turn_taken(&mut self) {
        self.turn_open = true;
    }

    /// A prompt has arrived, and this is where the whole turn's clock starts: once per prompt, never per round,
    /// with the engine's field carried on every `TurnStarted` so a head does not restart its clock. Idempotent
    /// within one prompt, and not called by the door: a door that says nothing must not publish a turn.
    pub fn begin_turn_clock(&mut self, began_ms: u64) {
        // A settlement taken for this turn is no longer owed - the words are in the rows this
        // turn opens with, whether or not the clock below starts. See
        // `JobWatchers::settlement_in_flight`.
        if let Some(w) = &self.job_watch {
            w.turn_began();
        }
        // Only a turn the shared tail is running can start a clock: a `run_rounds` reached outside
        // one has no `end_turn_clock` behind it, and a clock nobody clears reads `waiting on: turn`
        // for the life of the daemon. A door that has not spoken yet has not started a turn either.
        if !self.turn_open || self.prompt_in_flight.load(Ordering::SeqCst) {
            return;
        }
        self.turn_began_ms = Some(began_ms);
        self.engine.turn_began_ms = Some(began_ms);
        // And a prompt is in flight from here until the loop returns - see the field. The
        // engine's rounds come and go inside this, which is exactly why a reader that times the
        // round events reads a working session as idle at every boundary.
        self.prompt_in_flight.store(true, Ordering::SeqCst);
        // And the room is told the session is WORKING: the beat is the daemon's, not the model's, so
        // a model thinking about its work cannot lose the item to the lease. Idempotent, and a
        // session with no room pays one `Option` test.
        if let Some(room) = &self.room {
            room.turn_started();
        }
        self.seat.turn_began(began_ms);
        self.publish_activity();
    }

    /// The turn is over: stop publishing a start, so a later round that belongs to a NEW prompt
    /// cannot inherit this one's clock. And this is where a switch the operator made during the turn
    /// takes effect, because every way out of a turn comes through here.
    pub fn end_turn_clock(&mut self) {
        self.turn_began_ms = None;
        self.engine.turn_began_ms = None;
        // The turn the tail took is over - see the field. Cleared here rather than in `run_turn`
        // because this is the call every path out of the tail goes through, the empty one included.
        self.turn_open = false;
        // The prompt's loop has returned - a final answer, an interrupt, a failure, or a wall that
        // ended it. Whatever the session is waiting on from here is said by the rest of the facts
        // (`delivery`, a job, a monitor, a child, a card) and not by a turn.
        self.prompt_in_flight.store(false, Ordering::SeqCst);
        // And a turn that ends owes nothing - one that never opened the batch it took hands the
        // results back with `put_back`, and this keeps a take from outliving its turn.
        if let Some(w) = &self.job_watch {
            w.turn_began();
        }
        // After the flag, deliberately: the switch is applied through the same door a between-turns
        // switch takes, so a gate that saw the span still open would pend it again for ever. The
        // room's beats stop too, and the lease then counts from the last one.
        if let Some(room) = &self.room {
            room.turn_ended();
        }
        self.seat.turn_ended(letibot_sessionlog::event::now_ms());
        self.apply_pending_switch();
        self.publish_activity();
    }

    /// Something fired while nothing was running: the worker's wake, and since R7 the door a job's completion
    /// comes through. `Ok(None)` means nothing had settled after all, which is a real outcome: returning it
    /// rather than running an empty turn is what stops a spurious wake costing a generation.
    pub fn wake(&mut self) -> Result<Option<Reply>, HarnessError> {
        // The merge queue's reviews first, and they are never a turn of this session: a gatekeeper
        // started or a verdict written is the queue's business. A wake is one of three doors into it,
        // beside the round boundary and the daemon's own clock.
        let _ = self.serve_reviews();
        // A wake on a workspace that has gone: said on the hub once per harness, and to the model
        // only when this wake has something to say anyway, because a wake for this alone would be a
        // turn whose every tool call fails.
        let gone = !self.cfg.workspace.is_dir();
        if gone && !self.workspace_gone_said {
            self.workspace_gone_said = true;
            self.hub
                .warn(WORKSPACE_GONE, workspace_gone(&self.cfg.workspace));
        }
        // Two kinds of thing arrive between turns and share one turn: a monitor that fired and a job that
        // ended. Both must reach the model unprompted, and neither may jump a person waiting, since commands
        // drain before wakes. The queue's gate entries and the store's watch records are updated here too.
        self.sync_monitor_records();
        // Every sentence below goes through the `notice` hook: one kind per source, because a layer that patches
        // a firing's wording is not thereby the answerer for the queue's verdicts. Applied per source and not to
        // the joined text, so the facts a layer is handed are the ones its sentence was composed from.
        let queue = self.queue_notices();
        let mut notices = self.say_all(&Facts::MergeQueue { rows: queue.len() }, queue);
        // The room's lines are NOT said here: they are one queue with the results now, and the door
        // below says them in arrival order - see [`Deliveries::between_turns_unread`].
        // And the children a restart brought back, one line each, saying which came back and which could not.
        // Counted with the queue's notices, since both are facts the daemon found in the store rather than
        // settlements that arrived.
        let restart_taken = self.restart_notices.clone();
        let restarts = std::mem::take(&mut self.restart_notices);
        notices.extend(self.say_all(
            &Facts::Restart {
                lines: restarts.len(),
            },
            restarts,
        ));
        // And the entries the queue has handed to this session, which is the fallback: no reviewer could judge
        // them and the project merges such a branch through its own session. NOT a refused review, which goes to
        // the child that made the branch. Second, because an entry is in one of the two states.
        let handover = self.handover_notices();
        notices.extend(self.say_all(
            &Facts::MergeQueue {
                rows: handover.len(),
            },
            handover,
        ));
        // And a target emergency, or a row parked for a person - the one the
        // queue says on the hubs and here.
        let emergency = self.queue_emergency_notices();
        notices.extend(self.say_all(
            &Facts::MergeQueue {
                rows: emergency.len(),
            },
            emergency,
        ));
        let queue_n = notices.len();
        let cursor_before = self.monitor_cursor.load(Ordering::SeqCst);
        // A firing is a delivery and not one more sentence among the settlements: it gets a row
        // of its own, folded per monitor, so nothing has to remember whose words it is.
        let mut firing_row: Option<String> = None;
        // The facts the firing's sentence was composed from, for the row: the head draws the card
        // from them, so a `notice_` layer's rewording of the sentence leaves the card where it was.
        let mut firing_facts: Option<letibot_plugin::notice::Facts> = None;
        if let Some(monitors) = self.monitors.clone() {
            let since = self.monitor_cursor.load(Ordering::SeqCst);
            let settled = monitors.settled_count();
            if settled > since {
                let fired: Vec<_> = monitors.firings().into_iter().skip(since).collect();
                self.monitor_cursor.store(settled, Ordering::SeqCst);
                // The monitors pane's table moved with it: a fire, an expiry or a retire.
                self.publish_jobs();
                if !fired.is_empty() {
                    // Immediate by the rules, and recorded as such.
                    if let Some(d) = self.deliveries() {
                        d.log_firings(&fired);
                    }
                    if let Some(text) = monitor_notice(&fired) {
                        let facts = monitor_facts(&fired);
                        firing_row = self.say(&facts, &text);
                        firing_facts = Some(facts);
                    }
                }
            }
        }
        // The hop that did not exist: `JobSettled` reaches every head, and this is what reaches the model,
        // taking it here so it cannot arrive twice. It is THIS session's queue that is drained, so a subagent's
        // job is not announced to its parent as one the parent backgrounded. Between turns nothing is held.
        let monitor_n = usize::from(firing_row.is_some());
        let deliveries = self.deliveries();
        // A gatekeeper's settlement is the review's, not this session's news: the verdict went onto
        // the queue's row above, so telling the model here would be a turn about work already
        // recorded and already gone from the board.
        let gatekeepers = &self.gatekeepers;
        // ONE DOOR. The room's lines and the results the session is due come out of one take in the
        // order they arrived, so a room line is never said ahead of something that happened before it
        // and a trigger never arrives before the line it follows.
        let mut door = deliveries.as_ref().map(|d| {
            d.between_turns_unread(
                |c| gatekeepers.contains(&c.job),
                crate::delivery::Rule::TurnEnd,
            )
        });
        let mut room_seqs: Vec<u64> = Vec::new();
        let mut parts: Vec<Said> = Vec::new();
        let room_cheap;
        match &door {
            Some(now) => {
                room_seqs.extend(
                    now.told
                        .iter()
                        .filter_map(|h| h.room())
                        .flat_map(|r| r.seqs()),
                );
                room_cheap = now.cheap;
                parts.extend(now.parts.iter().cloned());
            }
            // Nothing to hold a result in: the room's lines are taken and said here, as they were
            // before the door existed, so a session with no watcher set still hears its room.
            None => {
                let took = crate::room::wake::take_wake(&self.cfg.session_id);
                room_cheap = took.cheap;
                room_seqs.extend(took.seqs.iter().copied());
                crate::room::wake::ack_through(&self.cfg.session_id, took.ack);
                parts.extend(took.lines.into_iter().map(|r| Said::Room {
                    seqs: r.seqs(),
                    text: r.line,
                }));
            }
        }
        // Said per source, through the same hook, so a layer may reword and may not delete: the
        // model is owed both facts.
        for part in &parts {
            let (facts, text) = match part {
                Said::Room { seqs, text } => (
                    Facts::Room {
                        items: seqs.len().max(1),
                    },
                    text.clone(),
                ),
                Said::Result { jobs, text, .. } => {
                    (Facts::Job { jobs: jobs.clone() }, text.clone())
                }
            };
            notices.extend(self.say_all(&facts, vec![text]));
        }
        // And the rows whose condition just became met: a firing is the same kind of fact as a job
        // ending, and the line carries both halves, because an intent with no fate is a sentence the
        // model cannot act on and a fate with no intent is a settlement it already has.
        let result_n = notices.len() - queue_n;
        let due = self.due_todo_rows();
        let fired: Vec<String> = due.iter().map(|(_, handle, _)| handle.clone()).collect();
        if !due.is_empty() {
            // the head said "{n} row you attached a condition to is due" for any n.
            let due_line = due_notice(&due);
            let rows = due.iter().map(|(text, _, _)| text.clone()).collect();
            notices.extend(self.say(&Facts::Todo { rows }, &due_line));
        }
        let causes = [
            queue_n,
            monitor_n,
            result_n,
            due.len(),
            door.as_ref().map_or(0, |now| {
                now.told.iter().filter(|h| h.room().is_some()).count()
            }),
        ];
        let anything = !notices.is_empty() || firing_row.is_some();
        // A wake with nothing to say runs no turn, so nothing is owed for what the take
        // removed - and this has to be cleared BEFORE the publish below, or the value a reader
        // holds says delivery about a turn that is not coming.
        if !anything && let Some(w) = &self.job_watch {
            w.turn_began();
        }
        // What this session is waiting on has moved: the monitor cursor and `between_turns_unread`
        // took things that publish no event of their own, and the move that matters most is the one
        // that empties the list.
        self.publish_activity();
        self.meter_wake(causes, anything);
        if !anything {
            return Ok(None);
        }
        if !self.cfg.workspace.is_dir() {
            let line = format!("[workspace] {}.", workspace_gone(&self.cfg.workspace));
            let facts = Facts::Workspace {
                path: self.cfg.workspace.display().to_string(),
            };
            if let Some(line) = self.say(&facts, &line) {
                notices.insert(0, line);
            }
        }
        // Cheap only when passing-by room talk is ALL this wake carries: anything else directs it.
        let cheap =
            room_cheap && !parts.is_empty() && notices.len() == parts.len() && firing_row.is_none();
        // Two rows sometimes, and one turn: the settlements in the session's own voice as it
        // has always said them, and a firing as a delivery with a kind of its own - never a `User`
        // row, whose `speaker` default is what would read as the operator's words.
        let mut items: Vec<TranscriptItem> = Vec::new();
        if !notices.is_empty() {
            let text = notices.join("\n\n");
            items.push(TranscriptItem::User {
                // The same speaker the trail records, one line up: this is the session talking
                // to itself, and a head must not draw it as the operator's words.
                speaker: letibot_transcript::Speaker::Agent,
                parts: vec![UserPart::Text { text }],
            });
        }
        if let Some(text) = firing_row {
            items.push(notice_row(
                letibot_transcript::NoticeKind::Monitor,
                text,
                firing_facts.as_ref(),
            ));
        }
        // The wall is checked BEFORE the send, as `nag_turn`'s pre-flight is. Not the load-bearing
        // fix: a refused wake is classified as the wall and recovered by the turn tail, so this only
        // spares a round trip the daemon's own ledger already knows will not fit.
        self.compact_if_at_the_wall();
        // The harness talking to itself, not the operator. A firing - or a job's end  -
        // must never be able to authorise the action it reports on.
        self.trail.begin_turn();
        for item in &items {
            self.trail.say(
                Speaker::Agent,
                &letibot_sessionlog::view::body_of(item),
                Some(Instant::now()),
            );
        }
        let mut opened = false;
        let out = self.cheaply(cheap, |h| h.submit_items_then(items, |_| opened = true));
        if opened && !room_seqs.is_empty() {
            // Silent: nothing said and nothing called. The tokens are the whole turn's.
            let (silent, tokens) = match &out {
                Ok(r) => (
                    r.text.trim().is_empty() && r.tool_calls == 0,
                    r.metrics
                        .iter()
                        .map(|m| m.prompt_tokens + m.predicted_tokens)
                        .sum(),
                ),
                Err(_) => (false, 0),
            };
            crate::room::link::wake_ended(&self.cfg.session_id, room_seqs, cheap, silent, tokens);
        }
        // Told only once the turn has opened: opened means the notice is in the transcript and in the store, so
        // the model reads it whatever its rounds come to. A turn that never opened put nothing in front of
        // anybody, so everything the door took - the results and the room's lines, unacked - goes back and the
        // next wake says it.
        if opened {
            if let Some(d) = &deliveries
                && let Some(now) = door.take()
            {
                d.read(&now);
            }
        } else {
            if let Some(d) = &deliveries
                && let Some(now) = door.take()
            {
                d.put_back(now);
            }
            let mut back = restart_taken;
            back.append(&mut self.restart_notices);
            self.restart_notices = back;
            self.monitor_cursor.store(cursor_before, Ordering::SeqCst);
        }
        out.map(Some)
            // ...and the condition is spent only once the model HAS been told. Consuming before the submit would
            // let a turn that failed to start silently eat the firing, and a firing nobody hears is the failure
            // this mechanism exists to rule out, where one reported twice is merely dull.
            .inspect(|_out| {
                if !fired.is_empty() {
                    self.todos.consume_conditions(&fired);
                    // Unwritten, the condition is still pending in the store and fires again after
                    // a restart - which is why this is said on the hub and not only on stderr.
                    self.flush_todos_or_say("a fired condition");
                }
            })
    }

    /// Run `f` as a cheap turn when `cheap`: thinking off and [`CHEAP_WAKE_TOKENS`] for every
    /// round, restored however the turn ends.
    pub(crate) fn cheaply<T>(&mut self, cheap: bool, f: impl FnOnce(&mut Self) -> T) -> T {
        if !cheap {
            return f(self);
        }
        let was = (self.cheap_turn, self.engine.thinking);
        self.cheap_turn = true;
        self.engine.thinking = Some(false);
        let out = f(self);
        (self.cheap_turn, self.engine.thinking) = was;
        out
    }

    /// A subagent brought back after a daemon restart carries on: its session was reopened from the store
    /// under its own id, so the conversation is its own, and what it lacks is the turn that was in flight. This
    /// is that turn, opened by a notice in the harness's own voice, with its own children and held work in it.
    pub fn continue_after_restart(&mut self, notice: &str) -> Result<Reply, HarnessError> {
        let mut parts = vec![notice.to_string()];
        parts.append(&mut self.restart_notices);
        let riding = self.batch_riding_in();
        let text = match &riding {
            Some(now) => now.text_after(&parts.join("\n\n")),
            None => parts.join("\n\n"),
        };
        // A restart is a fact the model is owed, so the door may reword it and cannot drop it.
        let text = self.say_kept(&Facts::Restart { lines: parts.len() }, text);
        self.trail.begin_turn();
        self.trail.say(Speaker::Agent, &text, Some(Instant::now()));
        // Opened, not successful: the row is what the model reads, and the rounds after it are the
        // turn's own business.
        let mut opened = false;
        let out = self.submit_items_then(
            vec![TranscriptItem::User {
                speaker: letibot_transcript::Speaker::Agent,
                parts: vec![UserPart::Text { text }],
            }],
            |_| opened = true,
        );
        self.the_batch_arrived(riding, opened);
        out
    }

    /// A settlement queued but never announced survives as a `delivered = 0` row; `JobWatchers` is per open.
    /// Read at open and at fork (which replaces the addressed transcript). The queue guard dedups repeats.
    fn deliver_settlements_left_undelivered(&self, store: &Store, me: &str, sid: &str) -> usize {
        let Some(watch) = self.job_watch.clone() else {
            return 0;
        };
        let mut queued = 0usize;
        // The children first - this row's own subject, and the one the operator went
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
            // The child's latest word, and whether it is a later turn's: a later answer puts the row
            // back to undelivered, so the sentence that carries it here is the one `task_result` promises.
            let (answer, later) = crate::subagent_record::latest_answer(&run);
            if watch.queue_unless_owed(JobCompletion {
                memory: None,
                kind: BackgroundKind::Subagent,
                // The session that started it, which is this one - the sentence is *a subagent
                // YOU started*, and it is true: the row's `parent_id` is this session.
                owner: sid.to_string(),
                job: run.child_id.clone(),
                command: String::new(),
                slug: Some(crate::subagent_record::recorded_slug(&run)),
                state: run.state.clone(),
                produced: 0,
                elapsed_ms: 0,
                detail: crate::jobwatch::first_line(&answer),
                arrived: std::time::SystemTime::now(),
                later,
            }) {
                queued += 1;
            }
        }
        // And the jobs, whose rows the recorder writes for the same reason and with the same `delivered = 0`.
        // A child's handle has a `running` job row too, so it is skipped here the way `restore_after_restart` skips it.
        for job in store.undelivered_jobs(sid).unwrap_or_default() {
            if store.subagent_run(&job.handle).ok().flatten().is_some() {
                continue;
            }
            let Some(ctx) = store.job_context(sid, &job.handle).ok().flatten() else {
                // No context means no daemon is named, so there is nothing that says this row is
                // this daemon's - and a row nobody can attribute is `restore_after_restart`'s
                // (a relic) rather than a settlement this door dropped.
                continue;
            };
            if ctx.instance != me {
                continue;
            }
            if watch.queue_unless_owed(JobCompletion {
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
                later: false,
            }) {
                queued += 1;
            }
        }
        if queued > 0 {
            eprintln!(
                "  {sid} · {queued} settlement(s) nobody was told about are said in {}",
                self.transcript_id
            );
        }
        queued
    }

    /// Re-arms `delivered = 0` rows into the transcript a fork just created; they were addressed to the one it replaced.
    /// The set's queue dedups. Returns how many were queued.
    pub(crate) fn rearm_owed_settlements(&self) -> usize {
        let Some(store) = self.store.as_ref() else {
            return 0;
        };
        let me = self.session_registry.instance().to_string();
        let sid = self.cfg.session_id.clone();
        self.deliver_settlements_left_undelivered(store, &me, &sid)
    }

    /// What this session was running when its daemon stopped, restored or reminded: called once, when the
    /// session is opened, over records a daemon that is GONE wrote. Everything it decides is one notice, and
    /// `None` when there was nothing to do, which is the ordinary open.
    pub fn restore_after_restart(&mut self) -> Option<String> {
        use letibot_tools::builtins::task::{RestartFate, RestoreMode};
        let Some(at) = self.cfg.store_at() else {
            return self.say_restore_unavailable("this session has no store".into());
        };
        // Prompts accepted but not started when the last daemon died: put back, and kept saved.
        let back = crate::restart::StoreJournal::attach(&self.hub, &at);
        if back > 0 {
            eprintln!(
                "  {} · restart: {back} queued prompt(s) put back",
                self.cfg.session_id
            );
        }
        let store = match at.open_existing() {
            Ok(s) => s,
            Err(e) => {
                return self.say_restore_unavailable(format!(
                    "its store {} could not be opened ({e})",
                    at.display()
                ));
            }
        };
        let me = self.session_registry.instance().to_string();
        let back_ms = self.session_registry.started_ms();
        // When this session is being opened - the clock the restore threshold is applied to
        //: the operator's rule is *"more than 1 hours after the stop"*, and a session
        // opened hours after the daemon came back is exactly the case the threshold exists for.
        let opened_ms = letibot_sessionlog::event::now_ms();
        let sid = self.cfg.session_id.clone();
        // Before anything about a restart, the settlements THIS daemon dropped: this is the door every open
        // passes through, and its own ring is what turns a queued settlement into a turn. The count is kept
        // because a settlement nobody was told about is one thing that makes an empty restart a TURN.
        let settlements = self.rearm_owed_settlements();
        // A daemon whose heartbeat is still fresh is not a gone one: the store is shared
        // by every daemon on the box, so its records are its own until its pid or its clock says not.
        let running = |instance: &str| crate::restart::is_running(&store, instance, opened_ms);

        // What a gone daemon left, and which daemon that was. Read before anything is
        // touched, so the down time is decided once for the whole session.
        let runs: Vec<_> = store
            .subagent_runs_of(&sid)
            .unwrap_or_default()
            .into_iter()
            .filter(|r| {
                r.instance != me
                    && !running(&r.instance)
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
                r.instance != me
                    && !running(&r.instance)
                    && r.state == "running"
                    && crate::subagent_record::is_helper(r)
            });
        let monitors: Vec<_> = store
            .monitors_of(&sid)
            .unwrap_or_default()
            .into_iter()
            .filter(|m| m.live && m.instance != me && !running(&m.instance))
            .collect();
        let mut jobs = Vec::new();
        // A job row with no context is a relic of a build before `job_context`, not a job running at this stop:
        // it names no daemon, so it cannot date anything, and reminding the model of it is false. They are closed
        // quietly below, the way a reminded job is, and said to nobody.
        let mut relics = Vec::new();
        for job in store.jobs(&sid).unwrap_or_default() {
            // A job the old daemon's ORDERLY stop ended was running at the stop, and is reminded like one:
            // `Registry::close` ends every run in flight before the process exits, so after a clean restart no row
            // reads `running` at all and the reminder this pass exists to give would never be given.
            if job.state != "running" && job.state != crate::restart::STOPPED_WITH_THE_DAEMON {
                continue;
            }
            // A subagent's handle is backgrounded through the same sink and has a `job` row too;
            // it is the child record's business, not a job's.
            if store.subagent_run(&job.handle).ok().flatten().is_some() {
                continue;
            }
            let ctx = store.job_context(&sid, &job.handle).ok().flatten();
            if ctx
                .as_ref()
                .is_some_and(|c| c.instance == me || running(&c.instance))
            {
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
        // Jobs that ENDED and were never delivered: a result held for the round boundary,
        // or queued, when the daemon stopped. Only a gone daemon's - one this daemon holds is
        // still in its own queue - and only with a context, which is what names the daemon.
        let mut settled = Vec::new();
        for job in store.undelivered_jobs(&sid).unwrap_or_default() {
            if store.subagent_run(&job.handle).ok().flatten().is_some() {
                continue;
            }
            let Some(ctx) = store.job_context(&sid, &job.handle).ok().flatten() else {
                continue;
            };
            if ctx.instance == me || running(&ctx.instance) {
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
        // A restart is SAID, whatever the store holds: the model's sense of time went through a stop and a
        // start, and only this daemon knows when. A session older than this daemon is not by itself a restart,
        // so what makes it one is another daemon's row carrying a stop, a fact about the box and not a clock.
        let a_daemon_stopped = store
            .lives()
            .map(|lives| {
                lives
                    .iter()
                    .any(|l| l.stopped_ms.is_some() && l.instance != me)
            })
            .unwrap_or(false);
        // And only the sessions a person reads: the review queue's own session is nobody's conversation, and the
        // operator saw the restart twice and asked for one. A subagent is nobody's either; it reads what its
        // parent tells it.
        let a_persons_session = !letibot_sessionlog::is_gatekeeper_session(&sid)
            && !matches!(store.session_parent(&sid), Ok(Some(Some(p))) if !p.is_empty());
        // The session's own row, read once, because it answers two questions: whether this session existed
        // before this daemon started, and what its last row was, which is the only evidence a session that was
        // mid-turn when the daemon stopped leaves behind.
        let row = store.session(&sid).ok().flatten();
        let came_back = a_daemon_stopped
            && a_persons_session
            && row
                .as_ref()
                .is_some_and(|s| (s.created_ms.max(0) as u64) < back_ms);
        // Once per (session, restart): `queue_told` keys on the daemon, so a session opened twice says it once
        // and the next daemon says it again, being a different restart. Spent on the first open whatever that
        // open says. A store that cannot record it is treated as not yet said.
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
            // What this session still owes decides the door its sentence takes: nothing owed keeps the row, since
            // a generation spent on a sentence is still spent; anything owed makes the sentence the content of the
            // turn the work already costs.
            let owed = self
                .owed_after_a_restart(row.as_ref().and_then(|s| s.last_item.as_ref()), settlements);
            return self.say_nothing_was_running(&store, back_ms, owed);
        }

        // How long the daemon was down: from the latest stop among the daemons these records came from, to this
        // daemon's start. The threshold is applied to a different interval, stop to this OPEN, because a session
        // opened hours after the daemon came back is the case it is for.
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
        // Every record here names a daemon (a relic does not, and was closed above), so the
        // stop is UNKNOWN only when one of those daemons kept no life row - no clock, not "down
        // for 0s" - and an unknown down time restores nothing (`restart` module docs).
        let stop = crate::restart::Stopped::latest(
            instances
                .iter()
                .map(|(i, at)| crate::restart::stop_of(&store, i, *at))
                .chain(std::iter::once(latest_box_stop(&store))),
        );
        let restore = stop.restores(opened_ms, self.cfg.restore_within);
        // Whether anything WAS brought back - which decides the notice's first line and its code.
        let mut restored_any = false;
        let mode = if restore {
            RestoreMode::Resume
        } else {
            RestoreMode::Remind
        };

        let mut lines: Vec<String> = Vec::new();
        let mut said: Vec<String> = Vec::new();

        // Subagents.
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

        // Monitors. One that had already run out its TTL before the stop is closed quietly:
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
            // A monitor a named scope owned is the model's to bring back, however short the
            // stop: the scope was something it set up, the stopped daemon was the only one that
            // knew it, and whether it still matters is a judgement, not a replay.
            if let Some(scope) = crate::restart::named_owner(&m) {
                lines.push(crate::restart::monitor_scoped_line(&m, &scope));
                said.push(crate::restart::said::monitor_scoped(&m.name, &scope));
                continue;
            }
            // The same call, made again: it was gated when the model first made it, and this re-establishes that
            // declaration rather than making a new one, so it goes through the door with no gate. The recording
            // wrapper writes the new declaration under this daemon.
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

        // Jobs: reminded, never re-run.
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
            // Said once: the row stops reading `running`, so the next open does not remind
            // again and the pane does not draw a job nobody runs.
            let mut lost = job.clone();
            lost.state = "lost at restart".into();
            let _ = store.put_job(&sid, &lost);
            // And said: a `lost at restart` row is an ending, and an undelivered ending is what
            // the next open would announce again.
            let _ = store.deliver_job(&sid, &job.handle);
        }

        // Jobs that ended untold: their ending, said now - whatever the down time, the way a
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
        self.hub
            .warn(outcome.code(), crate::restart::warning_detail(&text, &said));
        self.restart_notices.push(text.clone());
        Some(text)
    }

    /// One row saying restart restore cannot run, for a session that already has a history. A new
    /// session has nothing a stop could have cut short, so it says nothing.
    fn say_restore_unavailable(&mut self, why: String) -> Option<String> {
        if self.items().is_empty() {
            return None;
        }
        let text = format!(
            "[restart] Restart restore is off: {why}, so nothing that was running before a daemon \
             stop can be restored or reminded, whatever the {} restore window.",
            crate::restart::span(self.cfg.restore_within)
        );
        if let Err(e) = self.say_without_a_turn(&Facts::Restart { lines: 1 }, &text) {
            eprintln!(
                "  {} · restart: the notice was not recorded ({e})",
                self.cfg.session_id
            );
        }
        Some(text)
    }

    /// A restart with nothing to restore, said anyway. The stop is dated from the daemons the session's OWN
    /// rows name; a session that names nobody says `unknown` rather than borrowing the newest stop in the
    /// store, which is a claim about another folder's process. Owing nothing gets a row, owing work a turn.
    fn say_nothing_was_running(
        &mut self,
        store: &Store,
        back_ms: u64,
        mut owed: crate::restart::Owed,
    ) -> Option<String> {
        let sid = self.cfg.session_id.clone();
        let mut instances: Vec<(String, u64)> = Vec::new();
        let mut note =
            |instance: &str, at: u64| match instances.iter_mut().find(|(i, _)| i == instance) {
                Some((_, t)) => *t = (*t).max(at),
                None => instances.push((instance.to_string(), at)),
            };
        // A daemon that is still beating dates nothing: its rows are not a stop's, and
        // its heartbeat would otherwise be read as a daemon that came back a moment ago.
        let now = letibot_sessionlog::event::now_ms();
        let gone = |instance: &str| !crate::restart::is_running(store, instance, now);
        for m in store.monitors_of(&sid).unwrap_or_default() {
            if gone(&m.instance) {
                note(&m.instance, m.updated_ms);
            }
        }
        for r in store.subagent_runs_of(&sid).unwrap_or_default() {
            if gone(&r.instance) {
                note(&r.instance, r.updated_ms);
            }
        }
        // The newest job row is one query, and it is the row a session that ran nothing since
        // its last `bash` names its daemon by.
        if let Some(last) = store.jobs(&sid).unwrap_or_default().last()
            && let Some(ctx) = store.job_context(&sid, &last.handle).ok().flatten()
            && gone(&ctx.instance)
        {
            note(&ctx.instance, ctx.started_ms);
        }
        let stop = crate::restart::Stopped::latest(
            instances
                .iter()
                .map(|(i, at)| crate::restart::stop_of(store, i, *at))
                .chain(std::iter::once(latest_box_stop(store))),
        );
        // A session whose last words wait on a restart is woken, so a promise made to the operator
        // is kept. Being merely ACTIVE before the stop is not this: nothing is owed, so the sentence
        // stays the row below and no generation is spent on working out there was nothing to do.
        owed.promise = self.last_words_wait_on_a_restart();
        let owed = &owed;
        // The same sentence, and one clause when the session owes work - a model woken to
        // *nothing was running* alone could not know why it is reading it now. Composed in `restart`
        // because a lifecycle layer's `say` is part of it, and a layer may reword the restart without
        // being able to cancel it (X21).
        let host = self.plugin_host();
        let text = crate::restart::wake_text(
            host.as_deref(),
            &self.hub,
            &self.cfg.session_id,
            &stop,
            back_ms,
            owed,
            self.cfg.restore_within,
        );
        eprintln!("  {sid} · restart: nothing was running — {text}");
        // One line, not two: in the quiet case the sentence is a transcript row, which the person sees and the
        // model reads at its next turn, and in the owed case it is a restart notice the wake spends a turn on.
        // Neither publishes a hub warning, which drew the restart twice for one stop.
        if owed.any() {
            self.restart_notices.push(text.clone());
        } else if let Err(e) = self.say_without_a_turn(&Facts::Restart { lines: 1 }, &text) {
            eprintln!("  {sid} · restart: the notice was not recorded ({e})");
        }
        Some(text)
    }

    /// Whether the model's last message reads as waiting on a restart (`restart::RESTART_PROMISES`).
    fn last_words_wait_on_a_restart(&self) -> bool {
        let last = self.items().iter().rev().find_map(|i| match i {
            TranscriptItem::Assistant { text, .. } if !text.trim().is_empty() => Some(text),
            _ => None,
        });
        last.is_some_and(|t| {
            let tail: String = t
                .chars()
                .rev()
                .take(1500)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            crate::restart::promises_a_restart(&tail, self.cfg.tier().as_ref())
        })
    }

    /// What this session still owes, as `restart::Owed`: the facts that decide whether an otherwise empty
    /// restart is a turn, each with the row's own words where it is a row. The board is read twice on
    /// purpose, since `due` is the wake's own predicate and `plan` is every row still owed. A postponed
    /// or cancelled row is the operator's act.
    fn owed_after_a_restart(
        &self,
        last: Option<&TranscriptItem>,
        settlements: usize,
    ) -> crate::restart::Owed {
        use crate::restart::Rows;
        let board = self.todos.snapshot();
        crate::restart::Owed {
            mid_turn: crate::restart::mid_turn(last),
            settlements,
            due: Rows::of(
                self.due_todo_rows()
                    .iter()
                    .map(|(what, _, _)| what.as_str()),
            ),
            plan: Rows::of(
                board
                    .iter()
                    .filter(|r| matches!(r.status, TodoStatus::Pending | TodoStatus::InProgress))
                    .map(|r| r.content.as_str()),
            ),
            promise: false,
        }
    }

    /// Tell this session's model something without starting a turn, which is the door a restart with nothing
    /// owed takes. A turn running reads it at that turn's step boundary; an idle session gets a transcript row
    /// and NO wake, because a wake is a model call and the gap it closes is closed when the model next reads.
    pub(crate) fn say_without_a_turn(
        &mut self,
        facts: &Facts,
        text: &str,
    ) -> Result<(), HarnessError> {
        let Some(text) = self.say(facts, text) else {
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

    /// A turn is running, as far as this test is concerned: the one state `say_without_a_turn` branches on that
    /// a test cannot reach by running a turn, since a turn holds the harness for its whole life.
    #[cfg(test)]
    pub(crate) fn hold_a_turn_open_for_test(&self, open: bool) {
        self.prompt_in_flight.store(open, Ordering::SeqCst);
    }

    /// The turn a fallback earned, opened by the notice that says so: the refusal ended the last turn, and
    /// this one starts with a user item in the harness's voice, which is also what makes the work legal for the
    /// model that took over. The held batch rides in, since this is a turn the model takes anyway.
    pub fn continue_after_fallback(&mut self, notice: String) -> Result<Reply, HarnessError> {
        let mut text = notice;
        let riding = self.batch_riding_in();
        if let Some(now) = &riding {
            let batch = now.text();
            if !batch.is_empty() {
                text.push_str("\n\n");
                text.push_str(&batch);
            }
        }
        // The wall is checked BEFORE the send, like every other door: a session that inherited a
        // refusal is not more likely to fit, and a first round refused for LENGTH would be read as
        // another provider failure.
        self.compact_if_at_the_wall();
        self.trail.begin_turn();
        self.trail.say(Speaker::Agent, &text, Some(Instant::now()));
        // Opened, not successful: the row is what the model reads, and the rounds after it are the
        // turn's own business.
        let mut opened = false;
        let out = self.submit_items_then(
            vec![TranscriptItem::User {
                speaker: letibot_transcript::Speaker::Agent,
                parts: vec![UserPart::Text { text }],
            }],
            |_| opened = true,
        );
        self.the_batch_arrived(riding, opened);
        out
    }

    /// The continuation turn a fallback owed, taken at most once and never over a stop. Empty when the turn
    /// that ended was itself a continuation, so a chain cannot ping-pong, and when the operator asked for a stop
    /// the failed turn did not take. It always clears the field, or a notice left armed would open a turn later.
    pub(crate) fn take_fallback_continuation(&mut self) -> Option<String> {
        let notice = self.fallback_continuation.take()?;
        if self.hub.has_queued_interrupt() {
            return None;
        }
        Some(notice)
    }

    /// The turn a fallback earned is running - the one-shot, seen from inside it: read by
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

    /// A prompt through the seam every session has, which is the one a child was missing: the wall is detected
    /// in the harness, but what REACTS to it lived only in `Sessions`, which a child never reaches. One tail and
    /// not two: this is `run_turn` with no owner around it.
    pub fn submit_as_a_normal_session(&mut self, prompt: &str) -> Result<Reply, HarnessError> {
        self.submit_spoken_as_a_normal_session(Speaker::Operator, prompt)
    }

    /// A parent's message to a child BETWEEN turns: the turn it makes, spoken as the parent's. Until it arrived,
    /// a message could only enter a running turn, since nothing drained a child's queue between them. Not
    /// `submit`, because the speaker is the sender's, so the words can neither authorise nor be taken back.
    pub fn submit_a_parents_message(
        &mut self,
        from: &str,
        text: &str,
    ) -> Result<Reply, HarnessError> {
        let said = message_from_a_session(from, text, self.cfg.parent_session_id.as_deref());
        let facts = Facts::Parent { from: from.into() };
        let said = self.say_kept(&facts, said);
        self.submit_spoken_as_a_normal_session(Speaker::Agent, &said)
    }

    /// The turn and its tail, with the speaker given: the body both doors above share, and nothing
    /// but the shared tail with no owner around it, so there is no second copy to fall behind the daemon's.
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

    /// A parent's or peer's message as a turn, bare - the words
    /// [`Harness::submit_a_parents_message`] speaks, without the tail around them, for a caller
    /// that runs the tail itself ([`crate::turn_tail::run_turn`]).
    pub fn parents_message_turn(&mut self, from: &str, text: &str) -> Result<Reply, HarnessError> {
        let said = message_from_a_session(from, text, self.cfg.parent_session_id.as_deref());
        let facts = Facts::Parent { from: from.into() };
        let said = self.say_kept(&facts, said);
        self.submit_spoken(Speaker::Agent, &said)
    }

    /// Change the system prompt mid-session, in the form the dialect can render. Appended, never rewritten:
    /// rewriting message 0 cost a full cold re-prefill of a long conversation when measured, which is what this
    /// method exists not to do.
    pub fn system_update(&mut self, text: &str) -> Result<Reply, HarnessError> {
        self.system_updates += 1;
        // A turn, so distances stay right, but NOT an utterance: a system update is the operator changing the
        // instructions and not authorising an action, and letting it into the trail would make a general
        // instruction readable as consent to act on it.
        self.trail.begin_turn();
        let item = self
            .cfg
            .dialect
            .wiring(self.cfg.effort.as_deref())
            .system_update(self.system_updates, text);
        self.submit_item(item)
    }

    /// Compaction: one summary turn, then the fork, asked over the prefix the server already holds, which is
    /// the cache argument. A summary that proposes tool calls is refused, since a summary is a record and not an
    /// action. The provider answers it when there is one, so a metered summary is billed with its conversation.
    pub(crate) fn answerer<'p>(
        provider: &'p Option<Box<dyn letibot_backend::MessagesBackend>>,
        prefix: &'p StablePrefix,
    ) -> letibot_turn::compaction::Answerer<'p> {
        match provider {
            None => letibot_turn::compaction::Answerer::Local,
            Some(p) => letibot_turn::compaction::Answerer::Provider {
                backend: p.as_ref(),
                system: &prefix.system,
                // The prefix's own tools, so the summary goes under the SAME prefix as every other turn: not so
                // it can call one, since nothing executes what it proposes, but so a cached prefix still matches
                // and the largest call in the session is not a cold prefill.
                tools_json: &prefix.tools_json,
            },
        }
    }

    pub(crate) fn submit_item(&mut self, item: TranscriptItem) -> Result<Reply, HarnessError> {
        self.submit_items(vec![item])
    }

    /// [`Harness::submit_item`] for a turn that opens with more than one row - the held batch
    /// before the words that started the turn.
    fn submit_items(&mut self, items: Vec<TranscriptItem>) -> Result<Reply, HarnessError> {
        self.submit_items_then(items, |_| {})
    }

    /// `submit_items`, with `opened` called once the rows are in the transcript AND the store, the point past
    /// which the model reads them whatever the rounds come to. Not called when the turn never got there.
    pub(crate) fn submit_items_then(
        &mut self,
        items: Vec<TranscriptItem>,
        opened: impl FnOnce(&mut Self),
    ) -> Result<Reply, HarnessError> {
        // The prompt's clock starts before the rows go in: from the moment a turn has words it is in flight, and
        // the append below publishes, so a waiting head reads `turn` rather than a gap. Idempotent, since a
        // wall-continuation comes back with the clock already running.
        self.begin_turn_clock(letibot_sessionlog::event::now_ms());
        let mut sink = CapturingSink::new(self.hub.clone());
        self.session.append_items(&self.engine, &items, &mut sink)?;
        self.reconcile(&mut sink, &items);
        self.persist()?;
        opened(self);
        self.name_from_first_message();
        let out = self.run_rounds();
        self.note_turn_ended(&out);
        out
    }

    /// Marks a turn's rounds over on the watcher set's read record, so the plan check's grace can tell this turn's deliveries from older ones.
    /// Only a turn that finished on its own rings the wake of a held result: a stop asked for quiet, a failure has no waiter.
    pub(crate) fn note_turn_ended(&self, out: &Result<Reply, HarnessError>) {
        // Swapped rather than read: one turn, one answer, and a stop taken during the next turn is the next
        // turn's. The stop comes from the steering and not from `out` because a stopped turn is reported as a
        // turn failure, which would otherwise be indistinguishable from one that broke.
        let how = match (self.stopped.swap(false, Ordering::SeqCst), out.is_ok()) {
            (true, _) => crate::jobwatch::TurnEnd::Interrupted,
            (false, true) => crate::jobwatch::TurnEnd::Done,
            (false, false) => crate::jobwatch::TurnEnd::Failed,
        };
        if let Some(w) = &self.job_watch {
            w.turn_ended(how);
        }
    }

    /// Give an unnamed session a name, once, from the message that opened it: the defect in deriving a title is
    /// the CHANGING and not the deriving, so this can never fire again once a title is set. It is a default, not
    /// a decision, since `/rename` overwrites it.
    fn name_from_first_message(&mut self) {
        if !self.cfg.title.is_empty() {
            return;
        }
        // Read, not remembered: a head can name this session through the registry at any moment, so a
        // derivation trusting `cfg.title` would overwrite the name the operator had just typed with words from a
        // message they sent before it.
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
        let base = derive_title(text);
        if base.is_empty() {
            return;
        }
        let title = match &self.store {
            Some(store) => match store.name_default(&self.cfg.session_id, &base) {
                Ok(Some(t)) => t,
                // A name appeared between the read above and now; it stays.
                Ok(None) => return,
                Err(e) => {
                    // Not fatal and not silent: an unnamed session is usable, and a
                    // store that refused a title is a fact about the store.
                    self.hub.warn(
                        "title_not_stored",
                        format!("this session could not be named in the store: {e}"),
                    );
                    return;
                }
            },
            None => base,
        };
        self.cfg.title = title.clone();
        self.hub
            .publish(letibot_sessionlog::SessionEvent::SessionRenamed {
                title: title.clone(),
            });
        self.new_title = Some(title);
    }

    /// A title this harness has just derived, for the daemon to put in the registry: the harness holds no
    /// registry, so handing the title over is what lets a `Harness` be built in a test with nothing but a `Hub`.
    pub fn take_new_title(&mut self) -> Option<String> {
        self.new_title.take()
    }

    /// Rename this session: the store, the log and this harness's own config.
    pub fn rename(&mut self, title: &str) -> Result<(), HarnessError> {
        if let Some(store) = &self.store {
            store
                .rename_unique(&self.cfg.session_id, title)
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

    /// Name the worktree and branch this session's work must end up on - a `task_start`
    /// child's, set by its spawn. `None` clears it. See [`deliverable`].
    pub fn set_deliverable(&mut self, d: Option<deliverable::Deliverable>) {
        self.deliverable = d;
    }

    /// A turn's rounds, and what is left of its steering when they end: the rounds hand back whatever
    /// a failed or interrupted one had taken from the hub, and the next round's greedy poll takes it. When the
    /// turn is over an interrupt is spent, another session's message becomes a row, and everything else stays.
    pub(crate) fn run_rounds(&mut self) -> Result<Reply, HarnessError> {
        // The clock starts here for a door that appends no rows of its own: the operator's `!` line is already
        // on the log when it asks for its turn. A no-op when `submit_items_then` started it, which is every other
        // door; a door that finds nothing to say must not start it at all.
        self.begin_turn_clock(letibot_sessionlog::event::now_ms());
        let out = self.rounds();
        let (unread, left) = {
            let mut held = self.handed_back.lock().unwrap_or_else(|e| e.into_inner());
            held.retain(|m| !m.urgent);
            // A session's message comes out of the carried set, because it is about to be a row. Nothing else
            // re-offers it, so leaving it here is the one way a parent's words could be read twice.
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
                // Not fatal: the words are still on this session's log, and the turn's outcome is what the caller
                // waits for. Said, because a row that was not written is a row a later reader will not find, and
                // put back, so the next turn's greedy poll carries them as the injection they would have been.
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
            self.hub.warn(
                "steering_carried",
                format!(
                    "{left} message(s) that reached this turn while it ran were not read by \
                     the model before it ended; they go in at the start of the next turn."
                ),
            );
        }
        out
    }

    /// A message this turn never read, made a row anyway: the one path on which a parent's words could reach a
    /// child's queue and leave no record. A child may never run another turn, and the row is what makes the
    /// exchange visible, so the words are appended as the item the injection would have been.
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
        self.hub.warn(
            "message_carried",
            format!(
                "a message from another session reached this turn while it ran and the turn \
                 ended before the model read it; it is in the transcript now, so the next turn \
                 reads it there: {said}"
            ),
        );
        Ok(())
    }

    /// A turn needs somewhere to go, and a key to get there: the two refusals `rounds`
    /// makes before it drains anything the operator typed.
    fn the_turn_has_somewhere_to_go(&mut self) -> Result<(), HarnessError> {
        // The key first, before anything is drained or sent. A provider with no key
        // is asked for one here; a refusal returns before the turn takes the operator's
        // message, so nothing they typed is spent on a turn that could not run.
        if self.key_wanted.is_some() {
            self.obtain_key(None)?;
        }
        // And no model at all is said at the same point: a daemon with no GGUF and no provider has nothing a turn
        // can go to, since its server would be sent byte ids it cannot read. The head opens its chooser before
        // this; this is the floor under a head that did not, and it names the verb that ends it.
        if self.provider.is_none() && self.engine.vocab().is_bytes() {
            return Err(HarnessError::Setup(NO_MODEL_CHOSEN.to_string()));
        }
        Ok(())
    }

    /// The wall, checked between two rounds rather than only at a turn's boundary - and
    /// the whole turn stopped when it has been reached, which is the honest half a `Harness`
    /// can do on its own.
    fn stop_the_turn_at_the_wall(&mut self, round: usize) -> Result<(), HarnessError> {
        // The wall can arrive MID-TURN, and the turn boundary is too late: a tool result between rounds can put
        // the conversation past `n_ctx` and lose the next round whole. This stops the turn instead of compacting,
        // since building the new base reaches the store and the registry. Round 0 is checked against the window.
        let now = (!self.compacting).then(|| self.measure_resident());
        // Not on an estimate of a folded base, at round 0, on a metered session: that count is the fork's
        // re-based estimate, and the one round that would replace it with the provider's is this one. Stopping on
        // the estimate is a wall nothing could measure its way out of; if the provider counts it over, it says so.
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
                // In the units the operator's header shows, and the count the decision was made on. The ledger's
                // own figures are named once at the end, because three conversions in one sentence is not a sentence.
                let (w, h) = self.cfg.shown_wall();
                let r = now.map(|n| n.tokens).unwrap_or(resident);
                let unit = self.token_unit();
                let here = self
                    .provider
                    .as_ref()
                    .map(|p| format!("{}/{}", p.name(), p.model()))
                    .unwrap_or_default();
                let also = if let Some(model) = self.cfg.carried_note() {
                    // An estimate, named as one, with whose measurement it is: the ledger's bytes converted by the
                    // ratio another model measured, and not a number anything has confirmed for this one.
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
                    // The switch window, named rather than presented as calibrated: no round has run on this model
                    // yet, so these are the ledger's counts against a window measured on the model before it.
                    format!(
                        " (these are this box's own ledger counts: no round has run on \
                             {here} since it took over, so nothing has measured how its \
                             tokens relate to the ledger's. One round sets it)"
                    )
                } else {
                    String::new()
                };
                self.hub.warn(
                    "context_wall",
                    format!(
                        "stopping this turn after {round} round(s): {r} of {w} \
                             {unit} are resident and the next round needs {h} free{also}. \
                             Everything so far is committed, and the session compacts \
                             before the next turn — this is the wall, not a failure of \
                             the work."
                    ),
                );
                // And it names the session, for the reason the `compacting:` line does: one log file that every
                // daemon appends to, so without an id *which* session met the wall is unreadable. Fourteen bytes
                // make the pair a measurement rather than an adjacency.
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

    /// The rows a brief is checked against: this session's plan and its relatives' - a parent's
    /// and a child's board are the same tree's work, which is what "somebody is already doing
    /// this" spans. A storeless session still has its own board, so it is scored against that.
    fn open_rows(&self) -> Vec<crate::waits_offer::OpenRow> {
        let mut out: Vec<crate::waits_offer::OpenRow> = Vec::new();
        for item in self.todos.snapshot() {
            if crate::waits_offer::open(&item) {
                out.push(crate::waits_offer::OpenRow {
                    owner: "this session".into(),
                    content: item.content,
                });
            }
        }
        let Some(store) = &self.store else {
            return out;
        };
        let mut kin: Vec<(String, String)> = Vec::new();
        let kin_briefs = self
            .session_registry
            .kin(&self.cfg.session_id, self.cfg.parent_session_id.as_deref());
        for b in kin_briefs {
            let child = b.parent_session_id.as_deref() == Some(self.cfg.session_id.as_str());
            let parent = self.cfg.parent_session_id.as_deref() == Some(b.session_id.as_str());
            if child || parent {
                kin.push((
                    b.session_id.clone(),
                    letibot_tools::builtins::task::subagent_label(Some(&b.slug), &b.session_id),
                ));
            }
        }
        for (id, owner) in kin.into_iter().take(KIN_MAX) {
            for item in store.todos(&id).unwrap_or_default() {
                if crate::waits_offer::open(&item) {
                    out.push(crate::waits_offer::OpenRow {
                        owner: owner.clone(),
                        content: item.content,
                    });
                }
            }
        }
        out.truncate(ROWS_MAX);
        out
    }

    fn rounds(&mut self) -> Result<Reply, HarnessError> {
        self.the_turn_has_somewhere_to_go()?;
        // How many times this turn has asked again after a refused key. Bounded, so a
        // provider that refuses every key does not hold the worker in a loop of cards.
        let mut key_asks = 0usize;
        let mut metrics = Vec::new();
        let mut tool_calls = 0usize;
        let mut truncated = false;
        // The intent check nudges once per user turn, and the bound is the point: continuing means the answer
        // turn is not the end of the loop, and what it must not become is a loop where the harness argues with
        // the model. One is the error signal, two is the harness insisting, and an explanation is a real answer.
        let mut nudges_left = 1usize;
        // The `steer_` input's `after`, taken once per turn: the mark a compaction's fork leaves
        // describes the TURN that follows it, not one round of that turn (X14).
        let mut after_a_record: Option<letibot_plugin::steer::After> = None;
        // And a worktree child about to finish with its work outside its branch: the same budget of one, told
        // what is uncommitted once, and then its answer stands, with the parent's completion carrying the line if
        // the tree is still dirty.
        let mut end_of_work = deliverable::EndOfWork::new();
        // And a model that stops because its context is full when it is not: the same budget, one line telling it
        // the measured fill and then a warning to the operator rather than a second line. Gated by a measurement
        // and not by the prose heuristic.
        let mut context_check = crate::context_claim::Check::new().with_tier(self.cfg.tier());
        // The encoder for whether this turn is getting anywhere, per user turn because already-seen is a claim
        // about THIS turn: a file read earlier is a legitimate thing to read again. `max_tool_rounds` below is the
        // backstop it demotes.
        let mut progress = crate::progress::ProgressDetector::new(self.cfg.stall_rounds);
        // The turn's standing-notes offers: per ROUND for the round's own offer and per SESSION for the memory,
        // since a note never read won the same slot turn after turn. `recall` reads back what this session was
        // offered, from the log; the offer lives in the request assembly, because a row cannot be un-said.
        let mut offers = crate::notes_offer::Offers::new().with_tier(self.cfg.tier());
        offers.recall(&self.cfg.daemon_logs(), &self.cfg.session_id);
        // The turn's skill offers, by the same delivery and the same memory: a name in the tail, once
        // per session, settled against the round's own `skill` loads.
        let seat_tools = super::open::skill_seat(&self.cfg, &self.prefix.tools_json);
        let mut skill_offers = crate::skills_offer::Offers::new()
            .with_tier(self.cfg.tier())
            .with_seat(seat_tools.iter(), self.cfg.skills_never_offer().iter());
        skill_offers.recall(&self.cfg.daemon_logs(), &self.cfg.session_id);
        // And the tree's open rows, offered when the brief in hand is one of them: a row somebody
        // already holds is a wait, and spawning a child to redo it is what that line saves.
        let mut waits = crate::waits_offer::Offers::new().with_tier(self.cfg.tier());
        waits.recall(&self.cfg.daemon_logs(), &self.cfg.session_id);
        // And the work the repository has already done, by the same delivery: a commit subject or a
        // settled entry that says the brief is finished is a package nobody should write twice.
        let mut done = crate::done_offer::Offers::new().with_tier(self.cfg.tier());
        done.recall(&self.cfg.daemon_logs(), &self.cfg.session_id);
        // Read once per turn, not per round: the log is a fork, and a turn cannot change history.
        // Only read when the box has an embedder, so a box without one forks nothing.
        let done_records = match self.cfg.embedder.get().is_some() {
            true => crate::done_offer::records(&self.cfg.workspace),
            false => Vec::new(),
        };
        // And what this project already talked about, for the person who asks "remember we did
        // that?": the index of turns the prompt no longer carries, and the two tools that reach
        // them. Built once per turn, and not at all when the seat has not got `transcript` - a
        // hint naming a tool the session cannot call is a round spent on a refusal.
        let mut recall = crate::recall_offer::Offers::new()
            .with_tier(self.cfg.tier())
            .with_tools(seat_tools.iter());
        recall.recall(&self.cfg.daemon_logs(), &self.cfg.session_id);
        if recall.seated() && !self.compacting && !self.cheap_turn {
            let model = self
                .cfg
                .embedder
                .get()
                .map(|e| e.model())
                .unwrap_or_default();
            let built = crate::recall_index::build(
                self.store.as_ref(),
                &self.cfg.workspace,
                Some(&self.session.transcript_id),
                &model,
                self.cfg.tier().as_ref(),
            );
            recall.units(built.units, built.vectors);
        }
        // The bodies this session was STARTED with are not news to it. The ones its own rounds load
        // are read per round, below, because they are in the transcript only once that round ran.
        skill_offers.skip_names(self.cfg.skills.iter());

        let backstop = round_backstop(self.cfg.max_tool_rounds);
        for round in 0..backstop {
            self.stop_the_turn_at_the_wall(round)?;
            // This sink holds the round's ending, because the round's end and the turn's end are two
            // facts and this loop is the only place that decides the second (see `CapturingSink`).
            let mut sink = CapturingSink::holding_the_rounds_end(self.hub.clone());
            // This round's offer, a trailing item `session.items` never sees. Not on a compaction turn,
            // where it would settle against calls the summary never made, nor on a cheap turn.
            if !self.compacting && !self.cheap_turn {
                // The instruction files through the session's own backend, the project's notes from
                // the notes checkout.
                let notes =
                    crate::standing_notes::Reader::new(&*self.runtime.backend, &self.cfg.workspace)
                        .with_notes(self.cfg.project_notes());
                // One subject for all five offers, embedded at most once and only when words
                // leave the score undecided: the model's latest words, then the prompt and results.
                let shared = Some(
                    letibot_tools::similarity::bounded(&format!(
                        "{}\n{}",
                        crate::waits_offer::draft(&self.session.items),
                        crate::notes_offer::subject(&self.session.items)
                    ))
                    .to_string(),
                );
                offers.share_round(shared.clone());
                skill_offers.share_round(shared.clone());
                waits.share_round(shared.clone());
                done.share_round(shared.clone());
                recall.share_round(shared);
                offers.compose(
                    &notes,
                    &self.cfg.global_notes(),
                    &self.cfg.box_dirs,
                    &self.prefix.system,
                    &self.session.items,
                );
                // Score a skill against the prompt, plan and the model's latest text and reasoning, not just the note's subject.
                // Session contents are re-read each round so an already-loaded body is not offered again.
                skill_offers.skip_loaded(&self.session.items);
                skill_offers.compose(
                    &self.skills,
                    &crate::skills_offer::subject(&self.session.items, &self.todos.snapshot()),
                );
                // The box's embedder answers for every caller, so its first failure is said ONCE
                // here - and each caller has already fallen back to its own words by then.
                if let Some(said) = crate::embeddings::fault_note(self.cfg.embedder.get()) {
                    self.warn("embeddings_unavailable", said);
                }
                // The brief in hand is the person's prompt and the model's own latest words, which
                // is where a `task` call's prompt is written - the case this caller exists for.
                let brief = crate::waits_offer::draft(&self.session.items);
                let rows = self.open_rows();
                waits.compose(&rows, &brief);
                done.compose(&done_records, &brief);
                // The person's own words are the trigger and the subject is what they are doing:
                // a back-reference in a tool's output is nobody referring back.
                recall.compose(
                    &crate::notes_offer::subject(&self.session.items),
                    &crate::recall_offer::operator_text(&self.session.items),
                );
            }
            let mut tail = offers.tail();
            tail.extend(skill_offers.tail());
            tail.extend(waits.tail());
            tail.extend(done.tail());
            tail.extend(recall.tail());
            // Take the round again when the endpoint is what failed: an inner loop, so a retry does not spend a
            // `max_tool_rounds`, since it produced nothing. And `/models` is ANSWERED here but not APPLIED, because a
            // re-issued round carries the rest of the old turn, which a thinking-mode provider refuses.
            let (outcome, ladder) = self.take_the_round(&mut sink, &tail, &mut key_asks)?;
            // The outage this round was in, if any, is over, and it is said ONCE here where the count is final: a
            // round that ended on anything but an HTTP failure was answered, and a silent recovery is how somebody
            // spends an afternoon believing the endpoint is still down.
            self.settle_outage(!matches!(&outcome, Err(TurnFailure::Http(_))));
            // Before the match, deliberately: three of the four arms below leave this function, and the one that
            // matters most is the error that propagates. A failure whose turn id is only recorded on the success path
            // is a failure the daemon cannot name.
            self.last_turn_id = sink.turn_id.clone().unwrap_or_default();
            let ok: TurnOk = match self.judge_the_round(outcome, &ladder, round)? {
                RoundEnd::Go(ok) => *ok,
                RoundEnd::Again => continue,
                RoundEnd::Silent(m) => {
                    sink.finish(false);
                    metrics.push(*m);
                    self.publish_jobs();
                    return Ok(Reply {
                        text: String::new(),
                        metrics,
                        rounds: round + 1,
                        tool_calls,
                        truncated,
                    });
                }
            };

            // Pair against everything the ledger took, not just the model's rows: a steering message goes through
            // the same sink but is reported in `steering_applied`, so reconciling against `items` left an unmatched id
            // per message. Three groups, in transcript order: the tool's, the model's, then what arrived as it spoke.
            let mut appended = ok.steering_before.clone();
            appended.extend(ok.items.iter().cloned());
            appended.extend(ok.steering_applied.iter().cloned());
            self.reconcile(&mut sink, &appended);
            // The last round's prompt size is the session's context size, and the row is the only place it survives
            // a restart. Measured every round and for a local session too, and paired with the ledger the PROMPT had
            // rather than the one after the reply, or the ratio is of two moments and mostly reasoning.
            let rendered_at = {
                let rows = self.session.ledger.rows();
                let after = ok.items.len() + ok.steering_applied.len();
                match rows.len().checked_sub(after) {
                    Some(0) => u64::from(self.session.ledger.prefix_len()),
                    Some(n) => u64::from(rows[n - 1].end()),
                    None => self.session.ledger.len() as u64,
                }
            };
            // The pair rides the rows' own commit: a second fsync per round for one UPDATE was the larger half of the
            // round's write volume.
            let context_carried = self.persist_rows(
                &mut |_, _| {},
                Some((
                    ok.metrics.prompt_tokens,
                    ok.metrics.cached_tokens,
                    rendered_at,
                )),
            )?;
            // The request this round sent carried tools or it did not, and that is what decides whether its thought
            // was part of the prompt the provider counted.
            let echoed = self.echoes_reasoning();
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
                        echoed,
                    ),
                });
                self.cfg.ledger_scale = Some((rendered_at, ok.metrics.prompt_tokens));
                self.measure_resident();
                // The measurement the wall controller waits for: the first round on a folded base is where did the fold
                // make room is answered, by the provider's own count. Not the summary turn's rounds, which count the
                // history being folded.
                if !self.compacting {
                    self.observe_wall(ok.metrics.prompt_tokens);
                }
            }
            // Persisted beside the count, so a restart recovers the pair instead of pairing the count with whatever
            // ledger it has then. Not fatal: a measurement a busy store refused must not end the whole turn, and the
            // next round writes a fresh one.
            if !context_carried && let Err(e) = self.persist_context(&ok.metrics, rendered_at) {
                eprintln!("  store: {e}");
                self.hub.warn(
                    "context_not_stored",
                    format!(
                        "this round's context size was not written to the session row: {e}. \
                         The turn goes on; the next round's measurement replaces it."
                    ),
                );
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
            // The offer is settled here, where the round's calls are known, and logged whichever way it went. A round
            // that ends the turn with no calls settles with an empty list, which is an untaken offer recorded as one.
            let note_offer = offers
                .settle(&self.cfg.daemon_logs(), &self.cfg.session_id, &calls)
                .cloned();
            let skill_taken =
                skill_offers.settle(&self.cfg.daemon_logs(), &self.cfg.session_id, &calls);
            let mut staged = self.offer_rows(note_offer.as_ref(), &skill_taken);
            waits.settle(&self.cfg.daemon_logs(), &self.cfg.session_id, &calls);
            done.settle(&self.cfg.daemon_logs(), &self.cfg.session_id, &calls);
            // The recall hint's take-up: the round's own `transcript`/`digest` call, matched on the
            // term the hint named. Logged either way, because the ratio is what tunes the floor.
            recall.settle(&self.cfg.daemon_logs(), &self.cfg.session_id, &calls);
            // What the agent says it is doing, recorded before its calls are adjudicated: the sentence in front of its
            // tool calls is what the guard is missing when the operator's instruction names a goal rather than a file.
            // Never an authorisation and never citable.
            self.trail.claims(&text);
            self.meter_round(&ok.metrics, rendered_at);
            metrics.push(ok.metrics);

            // The context claim, checked against what the daemon measured: judged once per round whichever way it went,
            // so the log has every claim. And the reasoning in front of it, since a model that budgets by a percentage
            // says it there and nowhere the operator reads. A claim found only there is told, never forced.
            let (context_claim, claim_in_reasoning) =
                match self.judge_context_claim(&mut context_check, &text, false) {
                    Some((line, facts)) => (Some((line, facts)), false),
                    None => {
                        let thought = reasoning_text(&ok.items);
                        match self.judge_context_claim(&mut context_check, &thought, true) {
                            Some((line, facts)) => (Some((line, facts)), true),
                            None => (None, false),
                        }
                    }
                };

            // The round's ending, as the wire's `ends_turn`. A round that proposed calls has another
            // round owed to it; a round that proposed none is decided by the checks below, which are
            // this loop's own convergence and the only thing that knows whether the turn goes on.
            if calls.is_empty() {
                self.append_offer_rows(&mut staged)?;
                // Taken rather than read: `after` describes THIS turn, and a later turn begins on a
                // record only if something folded again between them (X14).
                let after = *after_a_record.get_or_insert_with(|| self.after_the_fold());
                // A model that stopped because the context is full when it is not: first, ahead of the intent check,
                // since the excuse is usually why a promise was not kept. It goes through `append_notice`, because the
                // turn is over and the loop continuing is what makes it a correction rather than a report.
                let mut notices: Vec<(Facts, String)> = Vec::new();
                let mut goes_on = false;
                if context_claim.is_some() {
                    goes_on = !claim_in_reasoning;
                }
                // The turn's end, as one decision (X14). The harness's own two detectors are
                // computed first and as they always were - their budgets are per turn, so a layer
                // that overrules them has not taken one from anybody - and a `steer_` layer's answer
                // replaces what they said. The claim's correction above and a queued operator
                // message below are not continue-or-stop decisions: a false sentence is corrected
                // and a person's message is answered whoever is on the stack. A cheap wake is not
                // offered to a layer at all: silence is the expected end of a turn woken by
                // passing-by room talk, and no layer may put that noise back.
                if !goes_on && !self.cheap_turn {
                    let here = letibot_plugin::steer::Here {
                        continues_left: nudges_left,
                        interrupted: ok.truncated,
                    };
                    // Not on a round cut short: an interrupt is the turn being ENDED, and a nudge
                    // that continued it would be the harness overruling the stop.
                    let mut native: Option<String> = None;
                    let mut intent = None;
                    if !ok.truncated {
                        // The turn boundary, and the one the error signal is about: a turn that ends
                        // with no tool call is exactly a model that says it will start something and
                        // forgets. The diff runs before the reply leaves, reading the assistant's text
                        // out of the rows just appended.
                        intent = (nudges_left > 0)
                            .then(|| self.close_the_turn(&ok.turn_id, &appended))
                            .flatten();
                        native = intent.clone();
                        if intent.is_some() {
                            // The one continuation a turn has, spent where it always was.
                            nudges_left -= 1;
                        }
                        // The branch is the deliverable, after the intent check, so a turn that
                        // promised work and did not start it hears about the promise first.
                        if native.is_none() {
                            native = end_of_work.at_turn_end(self.deliverable.as_ref());
                        }
                    }
                    let decided = self.steer_at_the_turn_end(
                        TurnEnd {
                            reply: &text,
                            claim: &context_claim,
                            native: native.clone(),
                            intent,
                            stall_rounds: progress.stalled_run(),
                            after,
                        },
                        &here,
                    );
                    // One `Stop` ends the turn the harness would have carried on; `None` is the
                    // layer saying nothing the host can use, and the harness's own answer stands.
                    let said = match decided {
                        Some(letibot_plugin::steer::Decision::Continue(sentence)) => {
                            // A continuation the harness would not have taken is the turn's one
                            // continuation, so it spends the budget; one that repeats the
                            // harness's own leaves the harness's own accounting alone.
                            if native.is_none() {
                                nudges_left = 0;
                            }
                            Some(sentence)
                        }
                        Some(letibot_plugin::steer::Decision::Stop) => None,
                        None => native,
                    };
                    if let Some(said) = said {
                        notices.push((Facts::Steer, said));
                        goes_on = true;
                    }
                }
                // A message queued while the head was running is a prompt, not a footnote: the engine appends what it
                // finds at the step boundary, and a turn that made no tool call must not return with the message
                // answered by nobody. The loop continues, and a round with nothing queued returns below.
                if !goes_on && !ok.steering_applied.is_empty() {
                    goes_on = true;
                }
                // A claim in the reasoning does not continue the turn, so its line is only worth saying when something
                // else does: appended to a turn that then ended it is the last item in the transcript, read at the
                // head of the next, unrelated turn.
                if goes_on && let Some((steer, facts)) = &context_claim {
                    notices.insert(0, (facts.clone(), steer.clone()));
                }
                // Said from the decision rather than from the calls: a round nudged into another one is not
                // the turn's end, and a head told it was drew `Responded` and then `Responding` again.
                sink.finish(goes_on);
                // Through `append_notice` rather than the steering queue: the turn is over, and a message queued for
                // the next step boundary of a turn that has ended would arrive after the reply the operator waits for.
                // The loop continuing is what makes it a correction.
                for (facts, steer) in &notices {
                    self.append_notice(facts, steer)?;
                }
                if goes_on {
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

            // A round that proposed calls is not the turn's end: the next one answers them.
            sink.finish(true);

            tool_calls += calls.len();
            let turn_id = ok.turn_id.clone();
            let (results, backgrounded) =
                self.run_the_rounds_calls(&calls, &turn_id, &mut progress);
            let round_verdict = progress.end_round();
            self.after_the_round(
                &results,
                &turn_id,
                &appended,
                context_claim,
                &mut nudges_left,
            )?;
            self.append_offer_rows(&mut staged)?;

            // A backgrounded job gives the floor back to the OPERATOR, not to the model: ending only the `bash` call
            // handed control straight back, and the model's next move was a bounded wait. So the turn ends here, and
            // their prompt is not consumed as steering, so the worker answers it as a turn of its own.
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

            // The stop, and it is two-stage on purpose: one round before the bound the model is told what the
            // detector sees, so a turn that was working can say so and carry on, and only the next stalled round
            // stops it. Cutting a working turn has cost two sessions; one extra round costs one round.
            if round_verdict == crate::progress::Round::Stalled {
                if progress.exhausted() {
                    return Err(HarnessError::NoProgress {
                        evidence: progress.evidence(round + 1, self.cfg.max_tool_rounds),
                    });
                }
                if let Some(steer) = progress.nudge() {
                    let facts = Facts::Stall { rounds: round + 1 };
                    self.say_mid_turn(&facts, &steer);
                }
            }
        }
        // Unreachable while unbounded - `usize::MAX` rounds is not a number this
        // process reaches - and honest if the bound is ever set back.
        Err(HarnessError::LoopBound { rounds: backstop })
    }

    /// What happens once the round's results are in: they are appended in one batch, the gate's
    /// admissions are said after them, the doors a running turn holds shut are answered at this
    /// boundary, and the two corrections ride the next step - the intent diff and a context claim.
    fn after_the_round(
        &mut self,
        results: &[TranscriptItem],
        turn_id: &str,
        appended: &[TranscriptItem],
        context_claim: Option<(String, Facts)>,
        nudges_left: &mut usize,
    ) -> Result<(), HarnessError> {
        let mut sink = CapturingSink::new(self.hub.clone());
        self.session
            .append_items(&self.engine, results, &mut sink)?;
        self.reconcile(&mut sink, results);
        // And the calls this round's gate admitted without asking anybody, said after the results and never
        // between them: a notice between a call and its result closes the pairing window.
        self.flush_gate_admissions()?;
        // A round can have started or ended a job; refill the mailbox now so a
        // pane opened mid-turn answers with this round's table, not last
        // round's. Cheap: the list is filtered before it is built.
        self.publish_jobs();
        // And a mode change the operator made while this turn was running: it sat in the queue until the turn
        // ended, so `/mode` moved nothing while the turn went on asking under the old point. Applied here, the
        // very next call is governed by it.
        self.apply_queued_mode();
        // And a tool the operator ran while this turn was running, which is what the door is FOR: the operator
        // drops the doc in and the next round of this same turn reads it, rather than waiting out a turn that can
        // run for minutes.
        self.apply_queued_head_run();
        // And the merge-queue reviews this session hosts, at the same boundary: they are work whose point is to
        // happen DURING a turn, and the door that would take them is the one this turn holds shut. One indexed
        // `SELECT` against a round that is a generation.
        let served = self.serve_reviews();
        if served > 0 {
            eprintln!(
                "  {} · reviews -> {served} gatekeeper(s)",
                self.hub.session_id()
            );
        }
        self.persist()?;
        // The calls of this round have run; if the model revised its plan,
        // the store and the heads hear about it now, at the round boundary  -
        // not when the turn ends, which is after the work the plan describes.
        self.flush_todos()?;
        // A turn that ran tools also gets the diff, since a completion marked done while nothing succeeded is
        // just as much the error signal. It goes through the steering queue, because the loop is about to
        // generate again.
        if *nudges_left > 0
            && let Some(steer) = self.close_the_turn(turn_id, appended)
        {
            *nudges_left -= 1;
            self.say_mid_turn(&Facts::Steer, &steer);
        }
        // And a context claim made by a round that went on to call tools: the model is
        // still working, so the correction rides the step boundary like the nudge above.
        if let Some((steer, facts)) = context_claim {
            self.say_mid_turn(&facts, &steer);
        }
        Ok(())
    }

    /// The round's calls, run in proposed order, each as its appended row.
    /// Order is the contract: GLM's template sorts results by call order, so any other order rewrites bytes the KV cache holds.
    /// Answers the rows and whether one gave the floor back to the operator.
    fn run_the_rounds_calls(
        &mut self,
        calls: &[ToolCall],
        turn_id: &str,
        progress: &mut crate::progress::ProgressDetector,
    ) -> (Vec<TranscriptItem>, bool) {
        let mut backgrounded = false;
        let mut results = Vec::with_capacity(calls.len());
        // The person's half of a question, held back until every result is in: a non-`tool` row closes
        // `pair_tool_calls`' answering window, so a `User` row inserted before the result makes the model read
        // *no result was recorded* beside the real one.
        let mut asked: Vec<TranscriptItem> = Vec::new();
        for call in calls {
            // Call order, and appended in call order. `self.tool_sink` is the log sink wrapped in the intent
            // encoder, so every result is measured on its way past. And the slot is emptied immediately before the
            // call runs, so nothing an earlier call left can be read as this one's answer.
            self.asked.lock().unwrap_or_else(|e| e.into_inner()).take();
            let started = Instant::now();
            let r = self.runtime.invoke(turn_id, call, &mut self.tool_sink);
            self.meter.tool(&self.cfg.session_id, &r, started);
            let mut item = ToolRuntime::transcript_item(&r);
            // A question the model asked is an exchange with a person, and the
            // person's half of it is theirs. The tool returns the answer; the
            // session writes it down, here, where every other result is appended.
            if call.name == ASK_USER_QUESTION {
                asked.extend(self.settle_asked(&mut item));
            }
            // A plugin layer sees the result before the model does (`docs/plugins-plan.md`),
            // and before the harness's own intercept below: the layer is about what the
            // CALL produced, and `pr_watch`'s sentence is the harness's own reading of it.
            if let TranscriptItem::ToolResult {
                outcome, payload, ..
            } = &mut item
            {
                // A successful result only. A call that failed did nothing to react to,
                // and its result is the failure's own words - which the model has to read
                // exactly, not through a layer's opinion of them.
                let how = match outcome {
                    letibot_transcript::ToolOutcome::Ok => Some(letibot_plugin::tool_result::OK),
                    letibot_transcript::ToolOutcome::Backgrounded { .. } => {
                        Some(letibot_plugin::tool_result::BACKGROUNDED)
                    }
                    _ => None,
                };
                if let Some(how) = how {
                    self.plugin_result(call, how, payload);
                }
            }
            // A pull request this call opened is watched from here on (`pr_watch`), and
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
        (results, backgrounded)
    }

    /// What a round's own failure means. Two arms carry a notice the model answers and the
    /// loop goes on; two end the turn - one of them as the wall the provider itself named, which is
    /// the only measurement of a session whose box set no window.
    fn judge_the_round(
        &mut self,
        outcome: Result<TurnOk, TurnFailure>,
        ladder: &letibot_http::retry::Ladder,
        round: usize,
    ) -> Result<RoundEnd, HarnessError> {
        let ok: TurnOk = match outcome {
            Ok(ok) => ok,
            // A cheap turn's silence is its expected end: a cap or a reasoning stop is not
            // argued with, and nothing was kept, so nothing is lost.
            Err(
                TurnFailure::EmptyLength { metrics, .. }
                | TurnFailure::UnfinishedReasoning { metrics, .. }
                | TurnFailure::BatchTruncated { metrics, .. },
            ) if self.cheap_turn => return Ok(RoundEnd::Silent(metrics)),
            // Nothing was committed, so there is no tool-call row to answer and the notice is a user item. Bounded
            // by the engine's own salvage budget: the round after this one either succeeds or comes back as
            // `SalvageExhausted`.
            Err(TurnFailure::BatchTruncated { notices, .. }) => {
                self.append_notice(&Facts::Salvage, &notices.join("\n"))?;
                return Ok(RoundEnd::Again);
            }
            // The operator's own sentence, because it is the only one with evidence: three machine notices
            // produced three more empty turns, and one imperative line produced the work. So: no question and no
            // condition, naming the behaviour rather than the mechanism.
            Err(TurnFailure::EmptyLength { .. }) => {
                self.append_notice(
                    &Facts::Nudge,
                    "Your reply was all reasoning and nothing was kept. Make the tool call \
                     or give the answer now.",
                )?;
                return Ok(RoundEnd::Again);
            }
            // The turn stopped inside its own reasoning and said nothing. GLM's end-of-turn token doubles as a
            // nameable string, so this is self-inflicted, and the answer is the same shape as the arms above:
            // tell the model and let the loop run, bounded by the same salvage budget.
            Err(TurnFailure::UnfinishedReasoning { .. }) => {
                // this arm is the turn ending INSIDE its reasoning, not running out.
                self.append_notice(
                    &Facts::Nudge,
                    "Your reply ended inside reasoning with no output. Make the tool call \
                     or give the answer now.",
                )?;
                return Ok(RoundEnd::Again);
            }
            Err(TurnFailure::Http(e)) => {
                // A context-length refusal IS the wall: the provider owns the window and has said the prompt does
                // not fit, and every recovery `Sessions` has is gated on `ContextWall`. The error carries this
                // box's own counts and the warning names the provider's two beside them, never a blend.
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
                    self.hub.warn(
                        "context_wall",
                        format!(
                            "stopping this turn after {round} round(s): the PROVIDER \
                             refused the prompt for length. This box counts {r} \
                             of {w} {unit} resident with {h} reserved for the next \
                             round.{theirs}{no_window} Everything already produced is \
                             committed, and the session compacts before the next turn \
                             — this is the wall, not a failure of the work.",
                        ),
                    );
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
                // The whole account of the failure, not just the last call of it: the ladder knows how many times
                // it tried and for how long, so the ending is its sentence rather than one call's error under
                // `FAILED` with no count. The typed cause rides along, since `switch_back_notice` asks which 4xx.
                return Err(HarnessError::Endpoint {
                    said: ladder.ending(&e),
                    cause: e,
                });
            }
            Err(e) => return Err(e.into()),
        };
        Ok(RoundEnd::Go(Box::new(ok)))
    }

    /// One round as the provider took it, plus the retry ladder. Endpoint failures retry here, not in `rounds`, since a retry produced nothing and must not spend a round.
    /// `key_asks` is the turn's refused-key budget, handed back.
    fn take_the_round(
        &mut self,
        sink: &mut CapturingSink,
        tail: &[TranscriptItem],
        key_asks: &mut usize,
    ) -> Result<(Result<TurnOk, TurnFailure>, letibot_http::retry::Ladder), HarnessError> {
        // A prompt that arrives while the session is parked WAITS, and nothing is sent: the
        // provider's own clock has not reached the reset, and one more call would only meet the
        // same refusal. The prompt is already a row, so the turn the reset opens answers it.
        if let Some(park) = self.limit_park.clone()
            && park.until_ms > self.limit_now_ms()
        {
            return Err(HarnessError::LimitParked {
                said: format!(
                    "this session is paused until {} on the {} limit, so nothing was sent: the \
                     prompt above waits for the reset, and the daemon answers it then with the \
                     turn the limit stopped.",
                    park.until_words(),
                    park.model
                ),
            });
        }
        let mut tried_fallbacks: Vec<String> = Vec::new();
        // One steering per round, outside the retry loop. A failed attempt
        // hands what it took back to this value, and the retried attempt's greedy poll
        // must find it there; one built per pass dropped it on the floor.
        let mut steering = self.steering();
        // The ladder this turn walks, and the only thing that decides whether a round is taken again. It is
        // also what the turn's ending is said from, since the count and the waiting are nobody else's to
        // report and a failure with no count is the report this was written for.
        let mut ladder = letibot_http::retry::Ladder::new(letibot_http::retry::Policy::new(
            self.cfg.http_retries,
        ));
        // How many of the LONG waits this round has taken - a rate limit that named no instant is
        // worth a few five-minute beats and not an afternoon. Bounded by `limit_retry_rounds`.
        let mut limit_retries = 0u32;
        let outcome = loop {
            // The operator's own line, answered before the attempt rather than after
            // it - the window they type in. What comes back says when the switch
            // takes; nothing about this attempt moves.
            self.apply_queued_model();
            // Start watching BEFORE the round, so the wait itself is covered.
            // The host is `Some` only on the metered route - a local prefill's
            // silence is progress, not a hang; see `SilenceWatch`.
            let cloud_host = silence_host(self.provider.as_deref());
            // The stall's own streak, computed before the round because the watchdog thread is the
            // one that knows whether a stall happened - and it cannot read this harness.
            let stalls = if self.trouble_kind == "stall" {
                self.trouble_count
            } else {
                0
            };
            let spoke = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            sink.note_speech_to(spoke.clone());
            let watch = SilenceWatch::start(
                self.hub.clone(),
                cloud_host,
                self.cfg
                    .deadlines_for(self.cfg.provider.as_ref())
                    .slow_notice,
                self.model_name(),
                stalls,
                spoke,
            );
            let attempted = match &self.provider {
                None => self.engine.run_turn_steered_with_tail(
                    &mut self.session,
                    sink,
                    &mut steering,
                    tail,
                ),
                // A cloud turn: the transcript as messages, the ledger as the
                // record. Same events, same verdicts, same TurnOk.
                Some(p) => self.engine.run_turn_messages_with_tail(
                    &mut self.session,
                    sink,
                    &mut steering,
                    p.as_ref(),
                    &self.prefix.system,
                    &self.prefix.tools_json,
                    tail,
                    self.cheap_turn.then_some(CHEAP_WAKE_TOKENS),
                ),
            };
            // The round has produced something or failed, either way the silence
            // is over and the watchdog has nothing left to say.
            if watch.finish() {
                self.trouble_kind = "stall".into();
                self.trouble_count = stalls + 1;
            }
            self.seat.called(
                letibot_sessionlog::event::now_ms(),
                match &attempted {
                    Err(TurnFailure::Http(e)) => Some(one_line(&e.to_string(), 160)),
                    _ => None,
                },
            );
            let Err(TurnFailure::Http(e)) = attempted else {
                // The new model answered, which is the whole of what a switch had
                // to prove - there is nothing to go back to any more. A round that came back
                // without a provider failure also ends any run of them.
                self.switch_back = None;
                self.trouble_kind.clear();
                self.trouble_count = 0;
                break attempted;
            };
            // A switch the operator made during this attempt is answered here and nothing else changes: it is
            // not this round's to apply, so the ladder goes on against the model the turn started on. Answered
            // before the key arm, since a 401 from the model they are leaving is no reason to ask for its key.
            self.apply_queued_model();
            // A refused key is asked for again, on the same card, and the round is taken again, since nothing
            // was recorded. Only a key this session can replace: one from the environment or `--api-key` wins
            // over the file, so a typed one would be saved and ignored.
            if let letibot_turn::HttpError::Status {
                code: code @ (401 | 403),
                body,
                ..
            } = &e
                && self.provider.is_some()
                && *key_asks < 2
                && let Some(pc) = self.cfg.provider.clone()
                && key_is_replaceable(&pc)
            {
                *key_asks += 1;
                let why = format!("{} refused the key ({code}): {body}", pc.name);
                self.provider = None;
                self.key_wanted = Some(pc);
                if let Err(e) = self.obtain_key(Some(why)) {
                    // The turn ends here, so the outage does too. Left in the field it
                    // would be continued by the NEXT turn's first failure, carrying this
                    // one's start time into a duration that never happened.
                    self.settle_outage(false);
                    return Err(e);
                }
                continue;
            }
            // This attempt failed, and the outage it belongs to remembers how many have: the record changes
            // here and is published nowhere, because the row is written once when the outage ends. After the
            // key arm, since a refused key is that card's news and not the endpoint's weather.
            self.outage_failed(&e);
            let Some(wait) = ladder.failed(&e) else {
                // The ladder is spent, and what comes next is the limit rule's to say: the operator's
                // ruling is that a provider which named its reset never costs a session its turn.
                let here = self.model_name();
                match self.after_the_ladder(&e, &mut tried_fallbacks) {
                    limit::AfterTheLadder::Parked { said } => {
                        // The provider's own trouble, as a type, with the reset on it: this is what
                        // tells a head and the bridge `limited until X`.
                        self.publish_trouble(&e, None);
                        // Nothing is retrying any more, so the outage row is over - and the park's
                        // own warning is the news that replaces it.
                        self.outage = None;
                        return Err(HarnessError::LimitParked { said });
                    }
                    limit::AfterTheLadder::Retry if limit_retries < self.cfg.limit_retry_rounds => {
                        limit_retries += 1;
                        self.publish_trouble(&e, None);
                        self.hub.warn(
                            "limit_retry",
                            format!(
                                "the provider is rate limiting this session and named no instant \
                                 to wait for, so the daemon waits {} and takes the round again \
                                 ({} of {}). Nothing was recorded by the round that failed.",
                                crate::restart::span(self.cfg.limit_retry_wait),
                                limit_retries,
                                self.cfg.limit_retry_rounds,
                            ),
                        );
                        match self.sleep_unless_closed(self.cfg.limit_retry_wait) {
                            Waited::Out => {
                                ladder = letibot_http::retry::Ladder::new(
                                    letibot_http::retry::Policy::new(self.cfg.http_retries),
                                );
                                continue;
                            }
                            Waited::Closed => break Err(TurnFailure::Http(e)),
                            Waited::Interrupted => {
                                let reason = self.hub.take_queued_interrupt().unwrap_or_default();
                                stop_children_first(Some(&self.subagents), &self.hub);
                                self.stopped.store(true, Ordering::SeqCst);
                                self.last_turn_id = sink.turn_id.clone().unwrap_or_default();
                                self.settle_outage(false);
                                return Err(HarnessError::Endpoint {
                                    said: format!(
                                        "stopped while waiting out a rate limit: {reason}. The \
                                         provider said {e} and named no instant, so the daemon was \
                                         waiting before taking the round again. Nothing was \
                                         recorded by the round that failed."
                                    ),
                                    cause: e,
                                });
                            }
                        }
                    }
                    other => {
                        // A move the limit rule earned, or today's path: a failure that is not a
                        // limit, or a limit with no rounds left, where a quota or a 5xx may still
                        // earn one. The sentence and the row are the same either way.
                        let next = match other {
                            limit::AfterTheLadder::Moved { next } => Some(next),
                            _ => self.fall_back_from(&e, &mut tried_fallbacks),
                        };
                        // The provider's own trouble, as a type, here and not per attempt: a ladder of five
                        // attempts on one 429 is one fact, and an automation wakes once for it.
                        self.publish_trouble(&e, next.clone());
                        if let Some(next) = next {
                            let was = here;
                            // Whether a turn was armed decides what may be promised: this turn is the continuation a
                            // fallback earns when one is already running, and then no second turn is opened, so the
                            // sentence must not say one is.
                            let opens_a_turn = self.fallback_continuation.is_some();
                            // The outage is this notice's to tell: one row per outage, and this is it, since the session
                            // is moving and the count belongs in that sentence. Taken, so `settle_outage` finds nothing.
                            let outage = self.outage.take();
                            // And the row the reader has been watching settles here, because this ending is
                            // not the ladder's: the session moves, so the row says where it went. Without it the
                            // row would still be saying `retrying` in a session that had stopped trying.
                            if let Some(o) = outage.as_ref()
                                && o.attempts >= 2
                            {
                                let line =
                                    format!("{}, and the session moves to {next}", o.gave_up());
                                self.publish_series(o, Some(line));
                            }
                            self.hub.warn(
                                "model_endpoint_retry",
                                fallback_notice(&was, &next, &e, opens_a_turn, outage.as_ref()),
                            );
                        }
                        break Err(TurnFailure::Http(e));
                    }
                }
            };
            // Nothing is published here: the wait is not news. The attempt that just failed moved the row
            // (`outage_failed`), and a row per attempt - three in three seconds for one endpoint - is the wall.
            match self.sleep_unless_closed(wait) {
                Waited::Out => {}
                Waited::Closed => break Err(TurnFailure::Http(e)),
                // The stop is the news, not the endpoint: the ladder stops where it is,
                // since taking the round again spends another wait on an answer the person
                // has already given up on.
                Waited::Interrupted => {
                    let reason = self.hub.take_queued_interrupt().unwrap_or_default();
                    // The whole stop, not only this wait's end: the steering door that takes an
                    // interrupt stops the subtree and tells the turn's ending, and a stop taken
                    // here is the same act arriving at a different door.
                    stop_children_first(Some(&self.subagents), &self.hub);
                    self.stopped.store(true, Ordering::SeqCst);
                    // This turn's own id, before the early return: the failure it becomes is
                    // published from this field, and a stale one is a failure no head can place.
                    self.last_turn_id = sink.turn_id.clone().unwrap_or_default();
                    self.settle_outage(false);
                    return Err(HarnessError::Endpoint {
                        said: stopped_in_the_wait(&reason, &ladder, &e),
                        cause: e,
                    });
                }
            }
        };
        Ok((outcome, ladder))
    }

    /// This session's steering source: the head's queue, the intent findings and a monitor that fires mid-turn.
    /// Rebuilt per round, but what it must SHARE is the trail, the injected queue and the monitor cursor, so a
    /// firing is not delivered twice. The boundary is also where a `/mode` and a door call are taken.
    fn apply_queued_head_run(&mut self) {
        while let Some(cmd) = self.hub.try_head_run_command() {
            // The operator's shell line is taken by the same picker and runs the same way, but it is not a door
            // call and has no admission to note, so the two kinds are separated before the bookkeeping starts.
            // Unless a run is already in flight, since one operator run per session is the rule: it parks instead.
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
                // The picker matched an `OperatorCall` or an `OperatorShell`; anything else is a bug in the picker,
                // and returning rather than panicking leaves the command queued for the worker, where it belongs.
                return;
            };
            let (call_id, name, arguments, who) = (
                call_id.clone(),
                name.clone(),
                arguments.clone(),
                who.clone(),
            );
            // Noted and taken around a synchronous run, as the worker's path does: the note is normally a no-op,
            // but keeping the pair means a head that detaches during the run still gets the sentence.
            self.hub
                .note_operator_call(&call_id, &cmd.head_id, &name, &who);
            let said = self.run_operator_call(&call_id, &name, &arguments, &who);
            self.hub.take_operator_call(&call_id);
            if let Err(e) = said {
                // Said, because there is nothing to return it to: this path has no caller to answer, so the failure
                // goes on the log or nowhere, and a call that ran with its row unpinned is the silence to avoid.
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
                self.hub.warn("mode_unknown", e);
                return;
            }
        };
        // The rows are written here too (`modes::record`, the worker's writes). This said the
        // worker's path would write them; it never saw this command, which this turn had already
        // taken off the queue.
        let consented = *consented;
        let moved = self.set_mode_consented(mode, consented);
        // And written down, as the worker's door writes it - applying the
        // point without writing would lose a `/mode` typed while the session worked at the
        // next restart.
        if let Err(e) = crate::modes::record(
            &self.mode_store,
            self.workspace(),
            &self.cfg.session_id,
            mode,
            consented,
            moved.is_ok(),
        ) {
            self.hub.warn(
                "mode_unpersisted",
                format!(
                    "`{}` applies to this session now, but could not be recorded, so a restart \
                     will not bring it back: {e}",
                    mode.name
                ),
            );
        }
        match moved {
            Ok(said) => {
                let applied = self.cfg.mode;
                self.hub
                    .warn("mode_set", format!("{said}. {}", applied.summary));
            }
            Err(why) => {
                self.hub.warn(
                    "mode_set_refused",
                    format!("this session stays at `{}`: {why}", self.cfg.mode.name),
                );
            }
        }
    }

    /// A `/models` the operator typed while this turn was running: taken and answered here, and RECORDED rather
    /// than applied, since the reply says what will happen and when. `mid_turn_switch_ok` refuses the two switches
    /// that re-seat the conversation, because a fork under a turn in flight has rows still to reconcile.
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
                // The picker only says yes to a model switch, so this is a bug in the predicate rather than a verb.
                // Answered rather than dropped: the command has been taken out of the queue, and a command that
                // vanishes without a word is the failure this tree keeps deleting.
                _ => crate::slash::SlashReply {
                    lines: vec![format!(
                        "/{line}: not a model switch this turn can apply, so nothing moved"
                    )],
                    ok: false,
                },
            };
            // The reply the worker would have published - the same two codes and
            // the same body, so "what did my switch do" does not depend on which thread
            // happened to apply it. `apply_queued_mode`'s rule, one verb along.
            self.hub.warn(
                if reply.ok { "slash" } else { "slash_refused" },
                format!("/{line}\n{}", reply.lines.join("\n")),
            );
        }
    }

    /// The picker half of [`Harness::apply_queued_model`]: the next queued command
    /// this running turn can carry out, with the verb's own grammar answering.
    fn take_queued_model(&self) -> Option<letibot_sessionlog::hub::QueuedCommand> {
        let foreign = self.on_foreign_weights();
        self.hub.try_command_for_running_turn(|kind| match kind {
            letibot_sessionlog::hub::CommandKind::Slash { line } => {
                crate::slash::mid_turn_switch_ok(line, foreign)
            }
            _ => false,
        })
    }

    /// Records a failed attempt on the outage. The host named is the one contacted, not the local endpoint.
    /// From the second failure it is published as the series' one row (a head holding the row rewrites it); a failure the ladder never retried is not a series.
    fn outage_failed(&mut self, e: &letibot_turn::HttpError) {
        let host = retry_host(self.provider.as_deref(), &self.cfg.endpoint);
        let now = letibot_sessionlog::event::now_ms();
        let reason = letibot_http::retry::Policy::brief(e);
        // One line per failed attempt for the popup, with the clock when there is one: an attempt that
        // cannot say when it happened still says what happened.
        let stamp = |reason: &str| match outage_clock(now) {
            Some(at) => format!("{at} · {reason}"),
            None => reason.to_string(),
        };
        // A failure against a DIFFERENT host is a different outage: `/models` and a fallback both move this, and two
        // endpoints' attempts are not one counter. The record it displaces has a row on the screen, so that row is
        // settled here: left live it would say `retrying` for a host this session no longer fails against.
        let displaced = match &mut self.outage {
            Some(o) if o.host == host => {
                o.attempts += 1;
                o.last_ms = now;
                o.said = e.to_string();
                o.reason = reason.clone();
                o.log.push(stamp(&reason));
                None
            }
            slot => slot.replace(Outage {
                host: host.clone(),
                attempts: 1,
                began_ms: now,
                last_ms: now,
                said: e.to_string(),
                reason: reason.clone(),
                log: vec![stamp(&reason)],
            }),
        };
        if let Some(old) = displaced
            && old.attempts >= 2
        {
            let line = format!("{}, and the endpoint moved to {host}", old.gave_up());
            self.publish_series(&old, Some(line));
        }
        if let Some(o) = self.outage.as_ref()
            && o.attempts >= 2
        {
            self.publish_series(o, None);
        }
    }

    /// The series' one row, published. `ended` is `None` while the ladder is still trying and the
    /// row's last value when it stops: the same event either way, so a head that holds the row rewrites
    /// it and one that attached mid-series is told where it stands.
    fn publish_series(&self, outage: &Outage, ended: Option<String>) {
        self.hub.publish(SessionEvent::ModelRetry {
            host: outage.host.clone(),
            attempts: outage.attempts,
            said: outage.retrying(),
            log: outage.log.clone(),
            ended,
        });
    }

    /// The outage is over: writes its one row, once (the record is taken). `answered` is any round ending on something other than an HTTP failure.
    /// The row's last value goes first, then the record; the warning below serves heads that attach later and `/notes`.
    fn settle_outage(&mut self, answered: bool) {
        let Some(outage) = self.outage.take() else {
            return;
        };
        let now = letibot_sessionlog::event::now_ms();
        // The endpoint came back, and that is said however few attempts it took - a silent
        // recovery is how somebody spends an afternoon believing it is still down.
        if answered {
            if outage.attempts >= 2 {
                self.publish_series(&outage, Some(outage.settled_well(now)));
            }
            self.hub
                .warn("model_endpoint_answered", outage.answered(now));
            return;
        }
        // A failure the ladder never retried (`http_retries = 0`) is not an outage: `turn_failed` is the whole news,
        // and no row was published, so nothing to settle.
        if outage.attempts < 2 {
            return;
        }
        self.publish_series(&outage, Some(outage.gave_up()));
        self.hub
            .warn("model_endpoint_retry", outage.did_not_answer());
    }

    /// The provider's trouble, as the type an automation branches on. Called where the daemon knows
    /// the round is over for good, not per attempt: a ladder of five calls against one 429 is one fact.
    /// The count is the run of this kind, so a caller can tell the first timeout from the fourth.
    fn publish_trouble(&mut self, e: &letibot_turn::HttpError, switched_to: Option<String>) {
        let Some(trouble) = letibot_http::retry::Policy::trouble(e) else {
            return;
        };
        let kind = trouble.name().to_string();
        self.trouble_count = if self.trouble_kind == kind {
            self.trouble_count + 1
        } else {
            1
        };
        self.trouble_kind = kind.clone();
        self.hub.publish(SessionEvent::ProviderTrouble {
            kind,
            // The model the round was on, named before any switch is applied: the fact is about the
            // provider that failed, and `switched_to` is the answer to where the session went instead.
            model: self.model_name(),
            detail: one_line(&e.to_string(), 240),
            reset_ms: letibot_http::retry::Policy::reset_ms(e, letibot_sessionlog::event::now_ms()),
            count: self.trouble_count,
            switched_to,
        });
    }

    /// Is the model itself the likely fault? A refusal, a spent account, a server that is down, or a run
    /// of timeouts long enough to stop reading as weather. The first two are the provider's own words; a
    /// timeout says nothing about the model on its own, which is why it has to repeat.
    fn worth_moving_for(&self, e: &letibot_turn::HttpError) -> bool {
        use letibot_http::retry::Trouble;
        match letibot_http::retry::Policy::trouble(e) {
            Some(Trouble::Quota | Trouble::RateLimit) => true,
            Some(Trouble::Timeout) => {
                let this_one = if self.trouble_kind == "timeout" {
                    self.trouble_count + 1
                } else {
                    1
                };
                this_one >= self.cfg.fallback_after_timeouts.max(1)
            }
            // A stall is a round that is slow, and a round that is slow may still answer; a 5xx is
            // `model_refused`'s and was the original rule.
            Some(Trouble::Stall) | None => model_refused(e),
        }
    }

    /// The model is out and a `[fallback] models` entry exists, tried once each. Records the move without making it: a re-issued round carries the old turn's rest, which a thinking-mode provider refuses.
    /// Triggers: provider refusal, spent account, 5xx, or a long timeout run. Metered names only.
    pub(crate) fn fall_back_from(
        &mut self,
        e: &letibot_turn::HttpError,
        tried: &mut Vec<String>,
    ) -> Option<String> {
        if !self.worth_moving_for(e) || self.cfg.fallback.is_empty() {
            return None;
        }
        if let letibot_turn::HttpError::Status { body, .. } = e
            && context_length_refusal(body).is_some()
        {
            return None;
        }
        let here = self.model_name();
        for name in self.cfg.fallback.clone() {
            // Once each, and never the model that just refused the round. A name
            // already spent is skipped; a name that IS what is answering would be a
            // no-op switch and the same failure one attempt later. A name the provider has
            // already PUT A TIME ON is skipped too - a fallback that is limited as well is not a
            // fallback, and landing there would spend the turn to learn what this session knows.
            if tried.contains(&name) || name == here {
                continue;
            }
            if self.limited_until(&name).is_some() {
                self.fallback_skip(
                    &name,
                    "the provider has already put a time on this model's own limit, so a switch \
                     there would meet the same refusal",
                );
                continue;
            }
            tried.push(name.clone());
            let (provider, model) = crate::slash::split_model_name(&name);
            // The resolver's own notes are dropped on the success arm, deliberately: a
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
            // The note the `models_choice` resolver came back with is not carried: a
            // fallback is a switch nobody typed, so the sentence that matters is
            // `fallback_notice`'s, said by the caller with the provider's own refusal in it.
            let target = model_name_of(&Some(pc.clone()));
            self.pend_switch(Some(pc), target.clone());
            // And the work goes on by itself: `run_turn` opens this turn when the one that met the refusal has
            // ended, with the notice as its user item. Not when THIS turn is already a continuation, or a fallback
            // chain would ping-pong between two names for ever - which a limit chain would too, one turn later:
            // the next turn finds the model it moved to limited as well and moves on from there.
            if !self.in_fallback_continuation() {
                // The sentence goes through the one door HERE, where the two models it names are in
                // hand: `continue_after_fallback` says what this decided, and the turn opens with it.
                let said = fallback_continuation_notice(&here, &target, e);
                let facts = Facts::Fallback {
                    was: here.clone(),
                    now: target.clone(),
                };
                self.fallback_continuation = Some(self.say_kept(&facts, said));
            }
            return Some(target);
        }
        None
    }

    /// A fallback name that could not be taken, said. The same code as the retry it
    /// is part of - the round is still being taken again, and what this adds is why it
    /// is not going *there*.
    fn fallback_skip(&self, name: &str, why: &str) {
        self.hub.warn(
            "model_endpoint_retry",
            format!(
                "`[fallback]` names `{name}` and this session cannot take it: {why}. \
                 Trying the next name."
            ),
        );
    }

    pub(crate) fn steering(&self) -> HubSteering {
        HubSteering {
            hub: self.hub.clone(),
            trail: Some(self.trail.clone()),
            // The same stack the turn's own sentences are offered to, so a bell, a parent's message
            // or an answer that arrives MID-turn is offered to the layers that own its kind.
            host: self.plugin_host(),
            injected: self.injected.clone(),
            monitors: self.monitors.clone(),
            monitor_cursor: self.monitor_cursor.clone(),
            // The session's own set, read by the rules: what the work waits on at the
            // round boundary, the held batch only past the cap.
            deliveries: self.deliveries(),
            // And the session's own children, so a stop takes them with it. See the
            // interrupt arm of `try_next`.
            subagents: Some(self.subagents.clone()),
            held: self.handed_back.clone(),
            // And the same for a stop: the turn's ending reads it, and a `HubSteering` built per
            // round would report a round's stop as nothing at all.
            stopped: self.stopped.clone(),
            // And the board, so the operator's answer lands mid-turn. See the `AnswerTodo` arm.
            todos: Some(self.todos.clone()),
            // And this session's own parent, so a message's sentence says which way the edge
            // runs - the same field `submit_a_parents_message` reads, so the two doors word it
            // identically.
            parent: self.cfg.parent_session_id.clone(),
            at_boundary: false,
            cheap: self.cheap_turn,
        }
    }

    /// The `steer_` input's `after`, off the flag a compaction's fork leaves. Taken, so it describes
    /// one turn: the next turn begins on a record only if something folded again between them.
    pub(crate) fn after_the_fold(&mut self) -> letibot_plugin::steer::After {
        if std::mem::take(&mut self.folded) {
            letibot_plugin::steer::After::Compaction
        } else {
            letibot_plugin::steer::After::Turn
        }
    }

    /// The turn's end offered to a `steer_` layer (X14), with the harness's own detectors as the facts
    /// it reads to overrule them. `None` is *no layer decided* - no layer on the stack, a trap, or an
    /// answer the host refused - and the harness's own answer then stands.
    fn steer_at_the_turn_end(
        &self,
        at: TurnEnd<'_>,
        here: &letibot_plugin::steer::Here,
    ) -> Option<letibot_plugin::steer::Decision> {
        use letibot_plugin::steer;
        let host = self.plugin_host()?;
        // Asked before the facts are gathered: a session with no layer must not pay a `git status`
        // for a deliverable nothing reads.
        if !host.answers(steer::HOOK) {
            return None;
        }
        let input = steer::Input {
            v: letibot_plugin::V,
            reply_tail: reply_tail(at.reply),
            open_rows: self.plan_rows(),
            native: at.native,
            intent: at.intent,
            deliverable: self
                .deliverable
                .as_ref()
                .and_then(|d| d.uncommitted())
                .map(|uncommitted| steer::Deliverable { uncommitted }),
            claim: at.claim.as_ref().and_then(|(line, facts)| match facts {
                Facts::ContextClaim { resident, window } => Some(steer::Claim {
                    line: line.clone(),
                    resident: *resident,
                    window: *window,
                }),
                _ => None,
            }),
            stall_rounds: at.stall_rounds,
            after: at.after,
            budget: steer::Budget {
                continues_left: here.continues_left,
            },
        };
        host.steer(&self.hub, &input, here)
    }

    /// The intent diff at a turn boundary, or `None` when there is nothing to say, which is the common case. Two
    /// questions, both asking whether the turn stopped with something it said it would do still open: the ledger's
    /// declared intentions and the model's own plan. Both are true and are joined. The prose half is opt-in.
    fn close_the_turn(&self, turn_id: &str, items: &[TranscriptItem]) -> Option<String> {
        // The plan is not checked here any more: the turn boundary is for a claim about the turn that just ended,
        // while the plan is a standing fact about the session, and checking it here gave both failure modes at once.
        // It is delivered by the idle clock instead, and `None` here is what makes that true.
        self.close_the_turn_with(turn_id, items)
    }

    /// The intent finding for a turn that has just ended: a named function because it is the seam the tests reach,
    /// and now the whole of the turn-boundary check. The plan's finding is not joined here, so a reader looking for
    /// the todo check will not find it at this layer: it is `nag_turn`, on the idle clock.
    fn close_the_turn_with(&self, turn_id: &str, items: &[TranscriptItem]) -> Option<String> {
        // one plan tool per seat. Where it is `todo_write`, the intent ledger's own
        // board is empty and the check reads the `todo_write` board instead.
        let seated = tool_names(&self.prefix.tools_json);
        let plan = (seated.contains("todo_write") && !seated.contains("todo"))
            .then(|| intent_tools::ledger::Board::of_plan(&self.todos.snapshot()));
        steer_for_turn_over(&self.intent, self.cfg.intent_prose, turn_id, items, plan)
    }

    /// One round's context claim, judged, logged and warned about. The line for the MODEL is returned, for the
    /// caller to deliver where the round's shape says it goes: a notice after a turn that ended, the step boundary
    /// for one still calling tools.
    fn judge_context_claim(
        &mut self,
        check: &mut crate::context_claim::Check,
        text: &str,
        reasoning: bool,
    ) -> Option<(String, Facts)> {
        use crate::context_claim::{REPEATED, Verdict};
        if text.trim().is_empty() {
            return None;
        }
        let fill = self.context_fill();
        // The numbers the sentence is about, handed with it: the door's facts and the verdict's own
        // measurement are the same reading, taken once.
        let facts = Facts::ContextClaim {
            resident: fill.map(|f| f.resident).unwrap_or(0),
            window: fill.map(|f| f.window).unwrap_or(0),
        };
        let mut finding = match reasoning {
            false => check.judge(text, fill)?,
            true => check.judge_reasoning(text, fill)?,
        };
        // with auto-compaction off, "the harness handles it" is false - nothing compacts
        // at the wall (`turn_tail` says so when the turn stops there) - so the model is told
        // nothing, and the operator is not told it was. The claim is still logged and measured.
        if !self.cfg.auto_compact {
            finding.verdict = Verdict::Quiet;
        }
        finding.log(&self.cfg.daemon_logs(), &self.cfg.session_id);
        crate::instruments::context_claim(&self.cfg.session_id, &finding);
        match finding.verdict {
            Verdict::Slap(line) => Some((line, facts)),
            Verdict::Repeated(detail) => {
                self.hub.warn(REPEATED, detail);
                None
            }
            Verdict::Quiet => None,
        }
    }

    /// The session's fill, as the wall judges it: the one place a claim's measurement is read, so the day the wall's
    /// measurement changes this changes with it. The verdict is the wall's own predicate, and the numbers are in the
    /// units the head shows. `None` when the window is unknown, since not knowing is not a measurement.
    fn context_fill(&mut self) -> Option<crate::context_claim::Fill> {
        // The count, the one the head and the wall show: `measure_resident` refreshes the scale from the
        // provider's last count plus what was sent since, so every reader below takes that one number rather than the
        // ledger's reasoning-heavy length.
        self.measure_resident();
        self.cfg.planning_window()?;
        let resident = self.session.ledger.len() as u64;
        Some(crate::context_claim::Fill {
            resident: self.cfg.shown_tokens(resident),
            // The window as it was GIVEN (`shown_wall`, the rule for a line), not
            // `shown_tokens(planning_window())` - a round trip through the ratio, which is where
            // every row of the log got `999999` for a 1,000,000-token window.
            window: self.cfg.shown_wall().0,
            at_wall: self.cfg.at_the_wall(resident),
        })
    }

    /// The rows that show the operator what the model was offered this round, settled.
    fn offer_rows(
        &self,
        note: Option<&crate::notes_offer::Offer>,
        skills: &[crate::skills_offer::Offer],
    ) -> Vec<TranscriptItem> {
        use crate::offer_rows as rows;
        let mut out = Vec::new();
        if let Some(o) = note {
            let notes =
                crate::standing_notes::Reader::new(&*self.runtime.backend, &self.cfg.workspace)
                    .with_notes(self.cfg.project_notes());
            out.push(rows::note_row(o, notes.read(&o.path).as_deref()));
        }
        for o in skills {
            out.push(rows::skill_row(o, self.skills.load(&o.name)));
        }
        out
    }

    /// File staged offer rows at a round boundary, after the results: a row between a call and its
    /// result closes the pairing window. They render to nothing, so the prompt is unchanged.
    pub(crate) fn append_offer_rows(
        &mut self,
        staged: &mut Vec<TranscriptItem>,
    ) -> Result<(), HarnessError> {
        if staged.is_empty() {
            return Ok(());
        }
        let items = std::mem::take(staged);
        let mut sink = CapturingSink::new(self.hub.clone());
        self.session.append_items(&self.engine, &items, &mut sink)?;
        self.reconcile(&mut sink, &items);
        self.persist()
    }

    /// Tell this session's model something the HARNESS found, without running a turn: a plugin failure is a fact
    /// the model can fix and only the harness can put in front of it. It is `append_notice` and not a turn, because a
    /// layer that failed to load is no reason to spend a generation.
    pub(crate) fn note_to_model(&mut self, text: &str) -> Result<(), HarnessError> {
        // Not through the `notice` hook, on purpose: this is a plugin layer's own failure, and a layer that traps
        // inside the hook that reports trapping would be a loop with a model at the end of it.
        self.record_notice(text)
    }

    /// The one door a model-facing sentence goes through: the harness's words offered to the
    /// `notice_<kind>` layer for the facts' kind, and `None` when a layer dropped a kind that may be
    /// dropped. A session with no layer answers at once, so its sentence is byte-identical to the
    /// one it said before this door existed.
    pub(crate) fn say(&self, facts: &Facts, text: &str) -> Option<String> {
        say(self.plugin_host().as_deref(), &self.hub, facts, text)
    }

    /// [`Harness::say`] for a kind the model is owed: a must-see notice cannot be dropped, so this
    /// answers the sentence itself. A kind that is NOT must-see must go through `say`, or the
    /// `Option` it answers would be thrown away here.
    pub(crate) fn say_kept(&self, facts: &Facts, text: String) -> String {
        match self.say(facts, &text) {
            Some(said) => said,
            // Unreachable while the kind is must-see, and the harness's own words are the right
            // answer if that ever changes: a fact the model is owed is not lost to a bug.
            None => text,
        }
    }

    /// [`Harness::say`] over the batch a wake composed from one source - the queue's sentences, the
    /// monitor firings, the jobs that settled.
    fn say_all(&self, facts: &Facts, lines: Vec<String>) -> Vec<String> {
        if lines.is_empty() {
            return lines;
        }
        let host = self.plugin_host();
        lines
            .into_iter()
            .filter_map(|line| say(host.as_deref(), &self.hub, facts, &line))
            .collect()
    }

    /// [`Harness::say`] for a sentence a RUNNING turn reads at its next step boundary: queued only if
    /// a layer left it standing, which a `steer`, a `stall` and a `context_claim` may not.
    fn say_mid_turn(&self, facts: &Facts, text: &str) {
        queue_mid_turn(
            self.plugin_host().as_deref(),
            &self.hub,
            &self.injected,
            facts,
            text,
        );
    }

    /// A notice this harness injects, said once and dropped if a layer dropped it.
    ///
    /// The kinds are `letibot_plugin::notice`'s; the sentence is the site's own.
    fn append_notice(&mut self, facts: &Facts, text: &str) -> Result<(), HarnessError> {
        let Some(text) = self.say(facts, text) else {
            return Ok(());
        };
        self.record_notice(&text)
    }

    /// The row itself, whoever decided the sentence.
    fn record_notice(&mut self, text: &str) -> Result<(), HarnessError> {
        // The harness talking to itself, recorded as such, so a salvage notice or an intent nudge can never be read
        // back as the operator authorising the action it is about: the whole reason `Speaker` has three values.
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

    /// Say the calls the gate admitted without asking anybody, one row each, at the round boundary: the one point
    /// in a turn where a row can be appended without landing between a tool call and its result. A session with no
    /// admission sink pays one lock.
    fn flush_gate_admissions(&mut self) -> Result<(), HarnessError> {
        let lines: Vec<String> = {
            let mut q = self
                .gate_admissions
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            std::mem::take(&mut *q)
        };
        for line in lines {
            self.append_notice(&Facts::Gate, &line)?;
        }
        Ok(())
    }

    /// Attach content to the rows the log already announced: the ids come off the events rather than being
    /// recomputed, and a count mismatch is a bug in the pairing, so it is announced rather than papered over.
    pub(crate) fn reconcile(&self, sink: &mut CapturingSink, items: &[TranscriptItem]) {
        // What this session now holds, restated here because this is the one function every append goes through,
        // so the census's number is a walk of the transcript as it is and not a counter that can drift from it.
        self.restate_footprint();
        // The trail's denominator, counted where every appended batch passes.
        // `messages_scanned` has to be what was looked at, and this is the one
        // place that knows how much there is.
        self.trail.note_items(items.len());
        let ids = reconcile_rows(&self.hub, sink, items);
        // And what each reply cited, read now - the moment it landed.
        for (id, item) in ids.iter().zip(items) {
            self.publish_citations(id, item);
        }
    }

    /// The lines a reply cites, published after its row: the stored snapshot when there is one, since a reply
    /// restored or carried by a fork shows what it cited when it was written, and otherwise the file as it is now.
    /// Silent for a row that cites nothing.
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

    /// Run the summary turn and pair every row it appended: one door, because the two callers forgetting is the
    /// defect this closes. `run_compaction` appends through the sink and `reconcile` hands the hub the row's body, so
    /// a caller that skipped it announced rows and never filled them. The slice is taken AFTER the turn.
    pub(crate) fn run_summary_turn(
        hub: &Arc<Hub>,
        engine: &mut TurnEngine,
        session: &mut Session,
        sink: &mut CapturingSink,
        answerer: &letibot_turn::compaction::Answerer<'_>,
        ladder: &mut letibot_http::retry::Ladder,
        ask: Option<&letibot_turn::SummaryAsk<'_>>,
    ) -> Result<CompactionOutcome, HarnessError> {
        let mark = session.items.len();
        // The summary turn is a provider call, so it walks the same ladder and its failure is said with the count
        // in it: a compaction that failed on a blip must not end the session's only chance to fold, with no way to
        // tell one attempt from six.
        let outcome =
            run_compaction(engine, session, sink, answerer, ladder, ask).map_err(|e| match e {
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

    /// Write every ledger row that has not reached the store yet, and persist and announce the todo list if the
    /// model wrote one. Called at every round boundary, so the pane moves with the work. The `_or_say` half is for
    /// the doors that must say the failure, not only `eprintln!` it.
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
        // Patient, and only then marked written: the version was recorded BEFORE the write, so a failed write left
        // the next flush seeing nothing moved and returning without writing, and a restart then reverted the
        // operator's answers and consumed conditions. A busy store is waited out, and the version moves last.
        if self.store.is_some() {
            let session = self.cfg.session_id.clone();
            self.store_patiently("todos", |store| store.put_todos(&session, &items))?;
        }
        self.todos_version = v;
        // And say it loudly when a person starts or stops being waited on: compared across flushes, so each
        // question is announced once when asked and once when settled. The settings row moves with it, because that
        // is what a head attaching later reads.
        let waiting = crate::operator_ask::waiting(&items);
        if waiting != self.asks_said {
            for (row, question) in waiting.iter().filter(|w| !self.asks_said.contains(w)) {
                self.hub.warn(
                    "waiting_on_operator",
                    crate::operator_ask::asked_words(row, question),
                );
            }
            for (row, question) in self.asks_said.iter().filter(|w| !waiting.contains(w)) {
                self.hub.warn(
                    "operator_question_settled",
                    // And WHY it stopped - an answer, a take-back and a row that closed are
                    // three different facts, and the person who was asked can only tell their own
                    // answer landed if the notice says so. See `operator_ask::settled_words`.
                    crate::operator_ask::settled_words(&items, row, question),
                );
            }
            self.asks_said = waiting;
            self.publish_settings();
        }
        let todos: Vec<TodoEntry> = items.into_iter().map(Self::todo_entry).collect();
        self.seat.todos(&todos);
        if let Some(room) = self.room_link() {
            room.todos_changed(&todos);
        }
        self.hub.publish(SessionEvent::TodosUpdated { todos });
        Ok(())
    }

    /// The store's todo shape, as the wire spells it: two types because the two crates cannot share one, and one
    /// conversion because the fields are the same four words.
    pub(crate) fn todo_entry(t: TodoItem) -> TodoEntry {
        TodoEntry {
            content: t.content,
            // The store's condition, as the WIRE spells it: a `match` rather than a pass-through, because the two
            // crates keep their own copy on purpose, so a variant is spelled on both sides and an unevaluable
            // condition is a compile error rather than a row that waits for ever.
            when: t.when.map(|c| match c {
                letibot_tokencore::store::TodoCondition::Job { handle } => {
                    letibot_sessionlog::event::TodoCondition::Job { handle }
                }
            }),
            // And its EDGES, by the same rule and for a second reason: the `match` is the compile-time half, and the
            // field is what lets the pane CARRY an edge rather than only draw one.
            needs: t
                .needs
                .into_iter()
                .map(|need| match need {
                    letibot_tokencore::store::TodoNeed::Row { content } => {
                        letibot_sessionlog::event::TodoNeed::Row { content }
                    }
                    // The child's id travels verbatim, the same rule `TodoBy::Parent`'s author
                    // string keeps: it is the handle `task_result` collects by, so a head that
                    // shortened it would hand the model a name nothing answers to.
                    letibot_tokencore::store::TodoNeed::Child { id } => {
                        letibot_sessionlog::event::TodoNeed::Child { id }
                    }
                    // And the job handle travels verbatim too, for the same reason one layer
                    // down: it is the name `job_output` and `job_kill` are given, and the pane has
                    // to be able to carry an edge into a job rather than only draw one.
                    letibot_tokencore::store::TodoNeed::Job { handle } => {
                        letibot_sessionlog::event::TodoNeed::Job { handle }
                    }
                    // A pid crosses with its start time, the pair that makes it exact: a head that
                    // dropped the second half would draw another process after a reuse.
                    letibot_tokencore::store::TodoNeed::Pid { pid, start } => {
                        letibot_sessionlog::event::TodoNeed::Pid { pid, start }
                    }
                    letibot_tokencore::store::TodoNeed::Monitor { name } => {
                        letibot_sessionlog::event::TodoNeed::Monitor { name }
                    }
                    // The room's item number crosses verbatim, the same rule the child's id and
                    // the job handle keep: it is what `room`'s `done`, `beat` and `abandon` take,
                    // and the head draws the edge so a person can see the row is quiet on purpose.
                    letibot_tokencore::store::TodoNeed::Room { item, working } => {
                        letibot_sessionlog::event::TodoNeed::Room { item, working }
                    }
                    // An unknown kind stays unknown on the wire, so a head that does not know one passes it back to a
                    // daemon that does rather than dropping the edge. The operator's question travels whole, since its
                    // options are what the card offers and an answer is checked against them.
                    letibot_tokencore::store::TodoNeed::Operator { question, options } => {
                        letibot_sessionlog::event::TodoNeed::Operator { question, options }
                    }
                    letibot_tokencore::store::TodoNeed::Answered {
                        question,
                        reply,
                        ask,
                    } => letibot_sessionlog::event::TodoNeed::Answered {
                        question,
                        reply,
                        ask,
                    },
                    // The ask's id, target and room travel verbatim: the id is what the room's
                    // settle is matched on, and the words are what the row draws while it waits.
                    letibot_tokencore::store::TodoNeed::Ask {
                        ask,
                        of,
                        room,
                        question,
                    } => letibot_sessionlog::event::TodoNeed::Ask {
                        ask,
                        of,
                        room,
                        question,
                    },
                    letibot_tokencore::store::TodoNeed::Owed { ask } => {
                        letibot_sessionlog::event::TodoNeed::Owed { ask }
                    }
                    // Something outside the machine, in the row's own words: the head draws it as
                    // `waits on <what>` and holds no table behind it.
                    letibot_tokencore::store::TodoNeed::External { what } => {
                        letibot_sessionlog::event::TodoNeed::External { what }
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
                // The author string TRAVELS: `Parent s-…` is the same string in the store and
                // on the wire, which is what lets a head or a `sqlite3` reader name the session
                // that wrote a child's row without a second lookup.
                letibot_tokencore::store::TodoBy::Parent(who) => {
                    letibot_sessionlog::event::TodoBy::Parent(who)
                }
                // The queue's own word, and it is the same word on both sides - the fourth
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

    /// The database enforces append-only, so ledger/store divergence is a failed INSERT.
    /// Waits unless the session is going away or a stop was asked: `Waited` names which, since a stop must not abandon a store write but must end a retry. Sliced so a stopping daemon is not held.
    fn sleep_unless_closed(&self, total: std::time::Duration) -> Waited {
        let slice = std::time::Duration::from_millis(250);
        let deadline = std::time::Instant::now() + total;
        loop {
            if self.hub.is_closed() {
                return Waited::Closed;
            }
            // A person pressing stop ends the wait: the retry ladder's is the provider's own
            // `Retry-After` as much as its rungs, and watching only the daemon's own closure left
            // them waiting out a minute they had already given up on.
            if self.hub.has_queued_interrupt() {
                return Waited::Interrupted;
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                return Waited::Out;
            }
            std::thread::sleep(slice.min(deadline - now));
        }
    }

    pub(crate) fn persist(&mut self) -> Result<(), HarnessError> {
        self.persist_reporting(&mut |_, _| {})
    }

    /// A wait on the store, said on this session and on every other one this daemon holds: the
    /// worker runs one turn at a time, so the others' commands queue behind this write and their
    /// heads would show `queued` with no reason at all.
    fn say_the_store_is_busy(&self, detail: String) {
        self.hub.warn("store_busy_wait", detail.clone());
        for id in self.session_registry.held_ids() {
            if id == self.cfg.session_id {
                continue;
            }
            let Some(hub) = self.session_registry.get(&id) else {
                continue;
            };
            hub.warn(
                "store_busy_queue",
                format!(
                    "this daemon serves one turn at a time, and `{}` is waiting on the session \
                     store — your next command waits behind that write. {detail}",
                    self.cfg.session_id
                ),
            );
        }
    }

    /// One store write, with the patience `persist_reporting` has: the store's own minute of busy retries, and past
    /// that a wait stated every two minutes until the store frees, the session closes, or `STORE_BUSY_PATIENCE` runs
    /// out. Anything that is not busy is returned at once. `f` must be safe to run again, since a busy write wrote nothing.
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
                        self.say_the_store_is_busy(format!(
                            "the session store is busy (`{e}`) — another daemon holds its write \
                             lock. The {what} write is kept in memory and made when it frees; this \
                             session is waiting, not failing ({}s so far).",
                            waited.as_secs()
                        ));
                    }
                    match busy_after(self.sleep_unless_closed(std::time::Duration::from_secs(5))) {
                        BusyWait::Again => {}
                        // A person's stop is not waited out: this write cannot be made
                        // later by anybody else, so it fails - saying the stop, not the closure.
                        BusyWait::Leave => {
                            return Err(HarnessError::Store(format!(
                                "{what}: {e} — this session was stopped while it waited for the store"
                            )));
                        }
                        BusyWait::Gone => {
                            return Err(HarnessError::Store(format!(
                                "{what}: {e} — the session closed while waiting for the store"
                            )));
                        }
                    }
                }
                Err(e) => return Err(HarnessError::Store(format!("{what}: {e}"))),
            }
        }
    }

    /// The rows not yet written, in chunks of `PERSIST_CHUNK`, one transaction each, with `progress` told after
    /// every chunk. `persisted` advances only after a chunk commits, so a failure retries from the last real row, and
    /// a busy store is waited out here rather than failed on.
    pub(crate) fn persist_reporting(
        &mut self,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Result<(), HarnessError> {
        self.persist_rows(progress, None).map(|_| ())
    }

    /// [`Harness::persist_reporting`], with the round's context pair written in the last chunk's commit. `true` when
    /// that commit carried it; `false` when there was no row or no store to carry it, and the caller writes it alone.
    fn persist_rows(
        &mut self,
        progress: &mut dyn FnMut(u64, u64),
        context: Option<(u64, u64, u64)>,
    ) -> Result<bool, HarnessError> {
        let Some(store) = &self.store else {
            self.persisted = self.session.ledger.rows().len();
            return Ok(false);
        };
        let rows = self.session.ledger.rows();
        let start = self.persisted;
        let mut carried = false;
        let total = rows.len().saturating_sub(start) as u64;
        // The patience is measured from the FIRST busy chunk, not from the call: a long transcript
        // that wrote for twenty minutes before the store wedged has not spent its budget.
        let mut first_busy: Option<std::time::Instant> = None;
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
            let with = context.filter(|_| to == rows.len());
            let sid = self.cfg.session_id.as_str();
            match store.append_items_with_context(
                &self.transcript_id,
                from as u32,
                chunk,
                with.map(|(t, c, l)| (sid, Some(t), Some(c), Some(l))),
            ) {
                Ok(()) => {
                    carried |= with.is_some();
                    self.persisted = to;
                    first_busy = None;
                    progress((to - start) as u64, total);
                }
                // A busy store is waited for, never died of: the store has already retried for a minute, and past that
                // the turn WAITS, says so on the session's own log, and gives up only when the session is going away or
                // the wait passes `STORE_BUSY_PATIENCE`, since a store wedged half an hour is a fault to report.
                Err(e) if e.is_busy() => {
                    // A chunk is one transaction and a busy one committed nothing, but the
                    // store is the authority: resume from what it holds, or the next attempt
                    // re-sends a row and the append-only trigger refuses it.
                    if let Ok(n) = store.item_count(&self.transcript_id) {
                        self.persisted = (n as usize).clamp(from, rows.len());
                    }
                    let at = self.persisted;
                    let waited = first_busy
                        .get_or_insert_with(std::time::Instant::now)
                        .elapsed();
                    if waited >= STORE_BUSY_PATIENCE {
                        return Err(HarnessError::Store(format!(
                            "row {at}: {e} — still busy after waiting {}s; nothing from row \
                             {at} on is stored",
                            waited.as_secs()
                        )));
                    }
                    if said_at.is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(120)) {
                        said_at = Some(std::time::Instant::now());
                        self.say_the_store_is_busy(format!(
                            "the session store is busy (`{e}`) — another daemon holds its write \
                             lock. Rows from {at} on are kept in memory and written when it frees; \
                             this session is waiting, not failing ({}s so far).",
                            waited.as_secs()
                        ));
                    }
                    match busy_after(self.sleep_unless_closed(std::time::Duration::from_secs(5))) {
                        BusyWait::Again => {}
                        // A person's stop does not wait the store out: the rows are in
                        // the ledger, `persisted` is where to resume, and the next store write takes
                        // them - so the turn is not held here for another half hour.
                        BusyWait::Leave => return Ok(carried),
                        BusyWait::Gone => {
                            return Err(HarnessError::Store(format!(
                                "row {at}: {e} — the session closed while waiting for the store"
                            )));
                        }
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
        Ok(carried)
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
                // The same ledger the in-memory anchor just used, so a restart recovers what this daemon had rather
                // than a second opinion: the ledger length of the PROMPT the count measured, which the reply came after.
                Some(rendered_at),
            )
            .map_err(|e| HarnessError::Store(format!("context: {e}")))
    }
}

/// How many times a round is re-attempted when the model endpoint fails: the default for `Config::http_retries`
/// and only that, since the RULE is `letibot_http::retry::Policy`. Five, doubling from a second, which is the
/// measured cold reload the ladder exists for. A caller that knows the port is dead sets it to 0.
pub const MAX_HTTP_RETRIES: u32 = letibot_http::retry::STANDARD_RETRIES;

/// Did the MODEL refuse, rather than the wire failing? A 429 or a 5xx and nothing else: a 408 is the server
/// waiting too long for US, which is the route, and a transport failure says nothing about the model. The argument
/// lives in `letibot_http::retry::Policy::the_model_refused`, beside the retry rule.
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
/// So the count lives in ONE row whose value moves: the record is opened by the first failed attempt,
/// changed by every later one, and published as [`SessionEvent::ModelRetry`] - the row's whole value,
/// so a head rewrites the row rather than adding one and settles it once when the series ends.
///
/// A failure the ladder never retried is not an outage and writes nothing: with no retries the
/// turn's own `turn_failed` is the whole of the news. Told once structurally, because
/// [`Harness::settle_outage`] TAKES the record, so no path can tell one outage twice however many
/// ways it ends. A second outage is a second record, since two outages are not one counter.
pub(crate) struct Outage {
    /// The host that did not answer, as [`retry_host`] spelled it. A failure against a DIFFERENT
    /// host is a different outage - the operator's `/models` and a `[fallback]` both move this,
    /// and two endpoints' attempts are not one count.
    host: String,
    /// How many attempts of this outage have failed.
    attempts: u32,
    /// When the first one did, Unix ms - the outage's start, for the duration.
    began_ms: u64,
    /// When the last one did, Unix ms - the `last at 10:31:25`.
    last_ms: u64,
    /// The provider's own sentence for the LAST attempt: the text a bug report wants.
    said: String,
    /// The last failure in the four words the row counts attempts in - `429 rate limited` - because
    /// one row that moves has space for a phrase per attempt and not for a sentence.
    reason: String,
    /// Every failed attempt as `<clock> · <reason>`, oldest first: the popup's list, so the reader
    /// who wants the shape of the outage has it on one click and not in six rows.
    log: Vec<String>,
}

impl Outage {
    /// The row while the ladder is still trying: the count, the last reason, and how long. Terse
    /// on purpose, since a frame at eighty columns keeps about thirty of these characters; the span
    /// is as of the attempt that closed the count, since only a publish can move a row.
    fn retrying(&self) -> String {
        format!(
            "retrying: {n} {word}, {reason}, {span}",
            n = self.attempts,
            word = attempts_word(self.attempts),
            reason = self.reason,
            span = outage_span(self.last_ms.saturating_sub(self.began_ms)),
        )
    }

    /// The row's last value when the endpoint came back: the count a reader was watching, and
    /// how long it was out. The count is the FAILED attempts, the same number the live line was
    /// showing, because a count that jumps by one as the row settles reads as a different series.
    fn settled_well(&self, now_ms: u64) -> String {
        format!(
            "answered after {n} {word}, {span}",
            n = self.attempts,
            word = attempts_word(self.attempts),
            span = outage_span(now_ms.saturating_sub(self.began_ms)),
        )
    }

    /// The row's last value when the ladder gave up: the count and the failure it stopped on,
    /// which is the one thing a person acts on. The provider's whole sentence, and when each
    /// attempt failed, are in the durable warning this rides beside.
    fn gave_up(&self) -> String {
        format!(
            "gave up after {n} {word}: {reason}",
            n = self.attempts,
            word = attempts_word(self.attempts),
            reason = self.reason,
        )
    }

    /// The row for an outage that ended without an answer: the count, when the last attempt was, and the provider's
    /// own words, so the head's one-line card keeps the count and the time before the cut and the sentence behind
    /// the click.
    fn did_not_answer(&self) -> String {
        format!(
            "the model endpoint at {host} did not answer (attempt {n}{last}): {said}",
            host = self.host,
            n = self.attempts,
            last = self.last_at(),
            said = self.said,
        )
    }

    /// The row for an outage that ended because the endpoint came back - the count, how long
    /// it was out, and the last thing it said before that (which is what a click opens and what
    /// gets pasted into a bug report).
    fn answered(&self, now_ms: u64) -> String {
        format!(
            "the model endpoint at {host} answered again after {n} {word} / {span}. \
             The last thing it said{last}, was: {said}",
            host = self.host,
            n = self.attempts,
            word = attempts_word(self.attempts),
            span = outage_span(now_ms.saturating_sub(self.began_ms)),
            last = self.last_at().replace(", last at", ", at"),
            said = self.said,
        )
    }

    /// `, last at 10:31:25`, or nothing when the clock cannot be read - an empty parenthesis is
    /// worse than the count alone.
    fn last_at(&self) -> String {
        match outage_clock(self.last_ms) {
            Some(at) => format!(", last at {at}"),
            None => String::new(),
        }
    }
}

/// A wall clock, `10:31:25`, for the sentence that says when the last attempt failed. `None` when the clock cannot
/// be read, and the caller then says the count without the time. The `localtime_r` form, since a harness is opened
/// from more than one thread.
fn outage_clock(ms: u64) -> Option<String> {
    let secs = (ms / 1000) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `localtime_r` writes into `tm` and reads `secs`; both are owned here.
    let ok = unsafe { !libc::localtime_r(&secs, &mut tm).is_null() };
    ok.then(|| format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec))
}

/// How long an outage lasted, in the words a sentence reads with: `14s`, `2m 14s`, `1h 02m`.
fn outage_span(ms: u64) -> String {
    let secs = ms / 1000;
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m {:02}s", secs / 60, secs % 60),
        _ => format!("{}h {:02}m", secs / 3600, (secs % 3600) / 60),
    }
}

/// `attempt` or `attempts`, in one place: the live line, the two endings and the durable
/// warning all count the same attempts, and a sentence that says `after 1 attempts` is what a
/// second copy of this rule looks like.
fn attempts_word(n: u32) -> &'static str {
    if n == 1 { "attempt" } else { "attempts" }
}

/// The retry notice for a session that moves because the model is OUT: what refused, what it said, where the
/// session goes and WHEN, and how to come back. The WHEN is the part that matters, since the switch takes at the
/// end of this turn and the notice must not promise the round continues. The WORK continues, which is the daemon's.
fn fallback_notice(
    was: &str,
    now: &str,
    e: &letibot_turn::HttpError,
    opens_a_turn: bool,
    outage: Option<&Outage>,
) -> String {
    // One continuation per fallback, so this branch is real: a chain whose second name is also
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
    // The count, when there is one to give - this notice IS the outage's row when a
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

/// What the harness's own detectors saw at a turn's end, gathered for the `steer_` layer that may
/// overrule them (X14). Borrowed rather than owned: every field is something the loop already holds.
struct TurnEnd<'a> {
    reply: &'a str,
    claim: &'a Option<(String, Facts)>,
    /// The sentence the harness itself would append, or `None` when it decided to end the turn.
    native: Option<String>,
    /// The intent detector's finding on its own, which the native sentence may or may not be.
    intent: Option<String>,
    stall_rounds: usize,
    after: letibot_plugin::steer::After,
}

/// The reply's tail, clipped: enough for a layer to read the promises in it, and not so much that a
/// long answer becomes the layer's whole context.
fn reply_tail(text: &str) -> String {
    const TAIL_CHARS: usize = 1_500;
    let chars: Vec<char> = text.chars().collect();
    chars[chars.len().saturating_sub(TAIL_CHARS)..]
        .iter()
        .collect()
}

/// The ending for a turn somebody stopped while the retry ladder was waiting: the stop is the
/// news, and the count says how far the ladder had got before they pressed it.
fn stopped_in_the_wait(
    reason: &str,
    ladder: &letibot_http::retry::Ladder,
    e: &letibot_turn::HttpError,
) -> String {
    format!(
        "stopped while the retry was waiting: {reason}. The ladder had called the model {} \
         time(s) over {}, and the last of them said {e}. Nothing was recorded by the turn that \
         failed, so your next message goes to the same model.",
        ladder.attempts(),
        crate::restart::span(ladder.waited())
    )
}

/// The notice that OPENS the turn a fallback earned: the model's own copy, as a user item in the harness's voice.
/// The register is the harness's, so a reader can tell it from the operator's words, and it carries what the model
/// cannot recover from the transcript: what refused, where it is now, and that nothing was lost.
fn fallback_continuation_notice(was: &str, now: &str, e: &letibot_turn::HttpError) -> String {
    format!(
        "[fallback] {was} refused ({}); continuing this task on {now}. The turn before this one \
         ended on that refusal and nothing it did was lost — its tool results are above. Pick the \
         work up where it stopped and carry on; do not repeat what is already done.",
        refusal_word(e)
    )
}

/// The refusal in the few words a notice can afford: `http 429`, or the kind of failure for one that is not a
/// status at all. Deliberately NOT the provider's body, which goes to the operator where it is worth the tokens.
fn refusal_word(e: &letibot_turn::HttpError) -> String {
    match e {
        letibot_turn::HttpError::Status { code, .. } => format!("http {code}"),
        letibot_turn::HttpError::Io(_) => "the connection failed".to_string(),
        letibot_turn::HttpError::Malformed(_) => "the answer did not parse".to_string(),
    }
}

/// The host a retry warning names, for the route this turn is on. Split out so the choice is testable, because the
/// wrong answer here is expensive and invisible to a passing build: the loop named `cfg.endpoint`, the LOCAL one, on
/// a path that also serves cloud turns, and sent the operator to debug the component that was working.
fn retry_host(provider: Option<&dyn letibot_backend::MessagesBackend>, local: &Endpoint) -> String {
    match provider {
        None => local.authority(),
        Some(p) => p.authority(),
    }
}

/// What a context-length refusal told us, in the provider's own numbers: the prompt size it refused and the window
/// it refused against, either of which a body may carry alone. They are the PROVIDER's counts, not this box's
/// ledger, and a caller comparing them with its own must say which is which.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ContextLengthRefusal {
    requested: Option<u64>,
    limit: Option<u64>,
}

/// The HTTP status a failure names, when it is a provider refusing what was SENT: 4xx is that class, while 429 is
/// the ladder's, 408 is the route, and 5xx or transport is a fact about the endpoint. Two shapes, because a failure
/// reaches a caller either as it left the engine or as the ladder's account of it.
pub(crate) fn provider_refusal(e: &HarnessError) -> Option<u16> {
    e.http_cause()
        .and_then(letibot_http::retry::Policy::refusal_of_what_was_sent)
}

/// Is this HTTP failure the context wall, spoken by the thing that owns it?
///
/// A provider refuses an over-long prompt with a STATUS, not with a wall event  -
/// `HttpError::Status` renders as `http {code}: {body}` and lands as a backend error,
/// so every recovery gated on `HarnessError::ContextWall` (the turn tail's
/// `out.is_ok() || wall`) reads false and no door reacts.
/// Thirty consecutive
/// 400s, the first attributed to `monitor`, the last to `todo check`, none recovered.
///
/// The two measured shapes, verbatim, because a matcher is only as narrow as its
/// fixtures:
///
/// * llama.cpp, JSON with a type field - the structured signal, preferred:
///
///   ```text
///   {"error":{"code":400,"message":"request (1504198 tokens) exceeds the available context size (262144 tokens), try increasing it","type":"exceed_context_size_error","n_prompt_tokens":1504198,"n_ctx":262144}}
///   ```
///
/// * an OpenAI-shaped provider, plain text naming a context length  -
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
/// is an overfit"*). Anything else - a 400 about invalid tokens, a 500 about a slot  -
/// is a backend error and stays one: if this cannot parse, it does not guess.
fn context_length_refusal(body: &str) -> Option<ContextLengthRefusal> {
    // The structured half: a JSON error object with a field that NAMES the context as the complaint. Both fields are
    // read independently, since an OpenAI-shaped body carries a `type` and a `code` together, so `type` cannot be the
    // only gate.
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
            // JSON, but about something else - `invalid_request_error` on bad
            // tokens, a quota, a model name. A backend error, not the wall.
            _ => None,
        };
    }
    // The text half: not JSON at all (the measured deepseek body), so the sentence
    // is the only carrier. Both anchors must be present - either alone is a phrase a
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

/// The digits immediately after `anchor`, skipping spaces - `is 1048576 tokens`,
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

/// Whether a silence notice applies at all, and about which host. `None` for the local route, and that is policy:
/// a local round is silent for 29-31 s reloading a cold GGUF and for minutes on a long prefill, both of which this
/// tree calls progress. Split from `SilenceWatch` so the decision is testable without a thread.
fn silence_host(provider: Option<&dyn letibot_backend::MessagesBackend>) -> Option<String> {
    provider.map(|p| p.authority())
}

/// The notice for a cloud round that has produced nothing, when one is warranted: pure, with no clock and no
/// thread, so the decision is testable on its own. Nothing here can see the status, so it claims nothing about the
/// request: it says the round has produced nothing and names the host it waits on.
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

/// Say something WHILE a cloud round is silent: a thread, because the provider blocks the round inside `complete`,
/// so nothing on this thread could notice. It REPORTS and does not abort, since a first-byte deadline would fail a
/// round that was going to succeed. Cloud only, since a local round is legitimately silent.
struct SilenceWatch {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
    /// Whether the round stalled past the threshold, which the watchdog is the only one to know. Read
    /// by `finish`, so the harness can carry the run of stalls rather than guess at it.
    stalled: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl SilenceWatch {
    /// Start watching, or return an inert watcher when there is nothing to say.
    /// `host` is `Some` only for a metered route. `model` and `streak` go into the typed event.
    /// `spoke` is the round's own word that it has started answering, set by the sink it streams through.
    fn start(
        hub: std::sync::Arc<letibot_sessionlog::hub::Hub>,
        host: Option<String>,
        threshold: std::time::Duration,
        model: String,
        streak: u32,
        spoke: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        use std::sync::atomic::Ordering;
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stalled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let Some(host) = host else {
            // Inert: the flag is already set, so `finish` is a no-op and no thread
            // exists to join.
            stop.store(true, Ordering::SeqCst);
            return SilenceWatch {
                stop,
                handle: None,
                stalled,
            };
        };
        let flag = stop.clone();
        let was = stalled.clone();
        let handle = std::thread::spawn(move || {
            // A short slice rather than one sleep of `threshold`, so the round can end
            // promptly without this thread outliving it - the flag is checked between
            // slices, and the thread exits within one slice of the round finishing.
            let slice = std::time::Duration::from_millis(100);
            let started = std::time::Instant::now();
            loop {
                if flag.load(Ordering::SeqCst) {
                    return;
                }
                // A round that has spoken is not silent, whatever its clock says. Measured from the
                // start alone, every streamed round past the threshold named a stall while text
                // was arriving; after the first delta the read timeout is the liveness instrument.
                if spoke.load(Ordering::SeqCst) {
                    return;
                }
                let silent = started.elapsed();
                if let Some(detail) = silence_notice(&host, silent, threshold) {
                    was.store(true, Ordering::SeqCst);
                    hub.warn("model_slow_first_byte", detail.clone());
                    // The same fact as a type, which is what an automation can branch on: the prose
                    // above is for the person watching the turn.
                    hub.publish(letibot_sessionlog::SessionEvent::ProviderTrouble {
                        kind: letibot_http::retry::Trouble::Stall.name().into(),
                        model,
                        detail,
                        reset_ms: None,
                        count: streak + 1,
                        switched_to: None,
                    });
                    // Once. A round that is slow for a minute should not fill the
                    // session with the same sentence; the first one carried the news.
                    return;
                }
                std::thread::sleep(slice.min(threshold.saturating_sub(silent)));
            }
        });
        SilenceWatch {
            stop,
            handle: Some(handle),
            stalled,
        }
    }

    /// Stop watching and wait for the thread to notice, answering whether the round stalled. Cheap: it
    /// is asleep on a 100 ms slice, so this costs at most that - and nothing at all for the local route,
    /// where no thread was started.
    fn finish(self) -> bool {
        use std::sync::atomic::Ordering;
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle {
            let _ = h.join();
        }
        self.stalled.load(Ordering::SeqCst)
    }
}

/// One line, at most `n` characters, with the newlines folded - a log row inside a sentence.
pub(crate) fn one_line(s: &str, n: usize) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= n {
        flat
    } else {
        let cut: String = flat.chars().take(n).collect();
        format!("{cut}…")
    }
}

/// Calls an assistant row asked for that no row ever answered, `(call_id, name)`, in order: the sweep's judgement,
/// apart from the plumbing so it is tested alone. It reads ONE transcript, since a row naming a call is the request
/// and a `ToolResult` with that id is the answer. Linear on purpose, since this runs on the worker.
pub fn abandoned_calls(items: &[TranscriptItem]) -> Vec<(String, String)> {
    SweptCalls::default().sweep(items)
}

/// The sweep's memory of the transcript it has already read. A turn appends a round to a
/// transcript that only grows, and the sweep runs after every turn: reading it whole each time is
/// one pass per turn over the whole conversation.
#[derive(Default)]
pub(crate) struct SweptCalls {
    /// How many of the transcript's rows have been read - the cursor the next sweep starts at.
    upto: usize,
    /// Calls asked and not yet answered, in the order they were asked.
    asked: Vec<(String, String)>,
    /// The ids an answer settled since the last sweep, dropped from `asked` by it.
    answered: std::collections::HashSet<String>,
}

impl SweptCalls {
    /// The unanswered calls, having read only the rows no earlier sweep has.
    pub(crate) fn sweep(&mut self, items: &[TranscriptItem]) -> Vec<(String, String)> {
        // A compaction replaces the transcript with a summary and a tail, so a cursor into the old
        // one would skip rows that exist and keep calls whose rows are gone. Start over instead.
        if self.upto > items.len() {
            *self = SweptCalls::default();
        }
        for item in &items[self.upto..] {
            probe_row();
            match item {
                TranscriptItem::Assistant { tool_calls, .. } => {
                    for c in tool_calls {
                        if !self.asked.iter().any(|(id, _)| id == &c.id) {
                            self.asked.push((c.id.clone(), c.name.clone()));
                        }
                    }
                }
                TranscriptItem::ToolResult { call_id, .. } => {
                    self.answered.insert(call_id.clone());
                }
                _ => {}
            }
        }
        self.upto = items.len();
        // A call asked twice and answered once is answered: the set difference, not a count - and
        // an answer that arrives after its call settles it whichever sweep reads which row.
        let answered = std::mem::take(&mut self.answered);
        self.asked.retain(|(id, _)| {
            probe_answer();
            !answered.contains(id)
        });
        self.asked.clone()
    }
}

// Answer lookups this thread's sweeps made, readable by tests.
// The sweep runs after every turn: one probe per asked call, one row read per row appended since the last sweep.
#[cfg(test)]
thread_local! {
    static ANSWER_PROBES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static SWEPT_ROWS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// One answer lookup - counted under test, and nothing at all in a shipped build.
#[inline(always)]
fn probe_answer() {
    #[cfg(test)]
    ANSWER_PROBES.with(|n| n.set(n.get() + 1));
}

/// One transcript row read - counted under test, nothing in a shipped build.
#[inline(always)]
fn probe_row() {
    #[cfg(test)]
    SWEPT_ROWS.with(|n| n.set(n.get() + 1));
}

#[cfg(test)]
mod endpoint_retry {
    //! When the model server is not answering, take the round again. The retry rule's own tests are the policy's,
    //! beside it in `crates/http/src/retry.rs`, since that is one subject with one home. What is left in this module
    //! is what the DAEMON does with an answer: which host a warning names, what a fallback says, when silence matters.
    use super::{
        CapturingSink, ContextLengthRefusal, Outage, SilenceWatch, context_length_refusal,
        fallback_continuation_notice, fallback_notice, model_refused, outage_span, retry_host,
        silence_host, silence_notice,
    };
    use letibot_turn::{Endpoint, HttpError};

    fn letibot_leticode_config_for_test(
        file: &std::path::Path,
    ) -> crate::leticode_config::LeticodeConfig {
        crate::leticode_config::LeticodeConfig::load(file).expect("test config parses")
    }

    fn status(code: u16) -> HttpError {
        HttpError::Status {
            code,
            body: String::new(),
            retry_after: None,
        }
    }

    /// The exception is the whole point: a credential the server rejected is rejected identically every time, so a
    /// minute of backoff buries the cause under six notices. The sweep's judgement, for a call whose executor and
    /// process both vanished: it sat `running` for eight minutes while `esc esc` did nothing.
    mod abandoned_call_sweep {
        use crate::harness::abandoned_calls;
        use crate::harness::turns::{ANSWER_PROBES, SWEPT_ROWS};
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
                edits: Vec::new(),
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

        /// And the case it must NOT touch. An answered call is the ordinary one, and a sweep
        /// that flagged it would rewrite every finished round in the transcript.
        #[test]
        fn an_answered_call_is_not() {
            let items = vec![assistant("c1", "grep"), result_for("c1")];
            assert!(
                abandoned_calls(&items).is_empty(),
                "the result answers the call"
            );
        }

        /// BY ID, not by position and not by name. Two calls to the same tool are two calls, and
        /// answering the first must not discharge the second - the mistake a name-keyed check makes
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

        /// The sweep's cost, pinned as a count - the reason a quadratic pass cannot come back
        /// unnoticed: it is one probe per asked call however many results the transcript holds.
        #[test]
        fn the_sweep_probes_the_answers_once_per_asked_call() {
            const CALLS: usize = 4_000;
            let items: Vec<TranscriptItem> = (0..CALLS)
                .flat_map(|i| {
                    [
                        assistant(&format!("c{i}"), "grep"),
                        result_for(&format!("c{i}")),
                    ]
                })
                .collect();
            ANSWER_PROBES.with(|n| n.set(0));
            assert!(abandoned_calls(&items).is_empty());
            let probes = ANSWER_PROBES.with(|n| n.get());
            assert_eq!(
                probes, CALLS,
                "{probes} probes for {CALLS} asked calls over {CALLS} answers: the answers are a \
                 set read once per call, not a list walked once per call"
            );
        }

        /// A transcript with no tool calls at all - the common case - costs nothing and says nothing.
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

        /// The sweep reads only what no earlier sweep has. A turn appends a round to a
        /// transcript that only grows, and the sweep runs after every turn: reading it whole each
        /// time is one pass over the whole conversation per turn, on the worker.
        #[test]
        fn a_second_sweep_reads_the_rows_since_the_first() {
            let mut swept = crate::harness::turns::SweptCalls::default();
            let mut items: Vec<TranscriptItem> = (0..2_000)
                .flat_map(|i| {
                    [
                        assistant(&format!("c{i}"), "grep"),
                        result_for(&format!("c{i}")),
                    ]
                })
                .collect();
            SWEPT_ROWS.with(|n| n.set(0));
            assert!(swept.sweep(&items).is_empty());
            assert_eq!(
                SWEPT_ROWS.with(|n| n.get()),
                items.len(),
                "the first sweep has nothing to remember and reads the transcript"
            );

            items.push(assistant("late", "grep"));
            SWEPT_ROWS.with(|n| n.set(0));
            assert_eq!(
                swept.sweep(&items),
                vec![("late".to_string(), "grep".to_string())],
                "the new call is the answer"
            );
            let read = SWEPT_ROWS.with(|n| n.get());
            assert_eq!(
                read, 1,
                "{read} rows read for one new row: the sweep remembers where it stopped"
            );
        }

        /// A transcript that SHRANK is a different conversation - a compaction forks onto a
        /// summary and a tail - so the cursor starts over, and a call whose row is gone is not
        /// reported as abandoned.
        #[test]
        fn a_transcript_that_shrank_is_read_again_from_the_top() {
            let mut swept = crate::harness::turns::SweptCalls::default();
            let full = vec![assistant("c-old", "grep"), result_for("c-old")];
            assert!(swept.sweep(&full).is_empty());
            assert!(swept.sweep(&full).is_empty(), "and it stays answered");

            // The fork: the summary row, and the one call this turn asked. `c-unanswered` was in
            // the folded half and its row is not here any more.
            let mut before = full.clone();
            before.push(assistant("c-unanswered", "bash"));
            assert_eq!(
                swept.sweep(&before),
                vec![("c-unanswered".to_string(), "bash".to_string())]
            );
            let compacted = vec![
                TranscriptItem::User {
                    parts: vec![UserPart::Text {
                        text: "summary".into(),
                    }],
                    speaker: Default::default(),
                },
                assistant("c-new", "grep"),
            ];
            assert_eq!(
                swept.sweep(&compacted),
                vec![("c-new".to_string(), "grep".to_string())],
                "the call whose row the fork dropped is not the sweep's business"
            );
        }
    }

    /// The model is out is a narrower question than is this worth waiting for, and the difference decides whether a
    /// session MOVES: too wide an answer moves a conversation over a failure that had nothing to do with the model,
    /// and too narrow leaves a quota exhaustion ending a turn.
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

    /// The fallback sentence, in the words the operator needed: what refused, where the session goes, when, and how
    /// to come back, and never promising the round continues. Two verdicts, because the usual case opens the
    /// continuation turn and a chain whose second name is also out opens nothing.
    #[test]
    fn a_fallback_names_the_way_back_and_says_when_the_switch_takes() {
        let out = || HttpError::Status {
            code: 429,
            body: "Weekly/Monthly Limit Exhausted".into(),
            retry_after: None,
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

        // And the case where no turn is opened says so: the session still moves, and the
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

        // And the count, when the ladder has one to give - this notice IS the outage's row
        // when a fallback fires, so the number the retry row would have carried is here.
        let outage = Outage {
            host: "open.bigmodel.cn".into(),
            attempts: 3,
            began_ms: 0,
            last_ms: 0,
            said: "http 429: Weekly/Monthly Limit Exhausted".into(),
            reason: "429 rate limited".into(),
            log: Vec::new(),
        };
        let counted = fallback_notice("glm/glm-4.6", "grok/grok-4", &out(), true, Some(&outage));
        assert!(
            counted.contains("nothing left to wait for after 3 attempts"),
            "the ladder's own count belongs in the sentence that says it is over: {counted}"
        );
    }

    /// The one row an outage is worth, in both of its endings: the count, when the last attempt was, and the
    /// provider's own words. The clock is the machine's, so the tests assert the SHAPE the card cuts at rather than a
    /// literal time.
    #[test]
    fn an_outage_is_one_row_that_carries_its_count() {
        let outage = Outage {
            host: "api.deepseek.com".into(),
            attempts: 3,
            began_ms: 0,
            last_ms: 0,
            said: "http 502: bad gateway".into(),
            reason: "502 server error".into(),
            log: Vec::new(),
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
        // One attempt is a word, not a count of one - and a short outage reads in seconds.
        let once = Outage {
            host: "local".into(),
            attempts: 1,
            began_ms: 0,
            last_ms: 0,
            said: "http io: connection refused".into(),
            reason: "the connection was refused".into(),
            log: Vec::new(),
        };
        let said = once.answered(2_400);
        assert!(said.contains("after 1 attempt / 2s"), "{said}");
    }

    /// The one row a retried call draws, in every value: the count and reason move on the same row, which ends as `gave up after 3 attempts` or names the failed attempts before an answer.
    #[test]
    fn a_series_of_retries_is_one_row_that_moves_and_then_settles() {
        let outage = Outage {
            host: "api.deepseek.com".into(),
            attempts: 3,
            began_ms: 1_000,
            last_ms: 42_000,
            said: "http 502: bad gateway".into(),
            reason: "502 server error".into(),
            log: vec![
                "10:31:22 · 502 server error".into(),
                "10:31:41 · timeout".into(),
                "10:32:03 · 502 server error".into(),
            ],
        };
        assert_eq!(
            outage.retrying(),
            "retrying: 3 attempts, 502 server error, 41s",
            "the row while the ladder runs: the count, the last reason and how long"
        );
        assert_eq!(
            outage.gave_up(),
            "gave up after 3 attempts: 502 server error",
            "and the row's last value when it stops trying"
        );
        assert_eq!(
            outage.settled_well(52_000),
            "answered after 3 attempts, 51s",
            "a recovery names the attempts that failed before it, so the count the reader was \
             watching is the count that settles"
        );
        // One attempt is a word, not a count of one.
        let once = Outage {
            attempts: 1,
            ..outage
        };
        assert_eq!(
            once.retrying(),
            "retrying: 1 attempt, 502 server error, 41s"
        );
        assert_eq!(once.gave_up(), "gave up after 1 attempt: 502 server error");
    }

    /// The duration in the words a sentence reads with - `14s`, `2m 14s`, `1h 02m`.
    #[test]
    fn an_outage_span_is_read_as_a_person_reads_it() {
        assert_eq!(outage_span(0), "0s");
        assert_eq!(outage_span(14_000), "14s");
        assert_eq!(outage_span(134_000), "2m 14s");
        assert_eq!(outage_span(3_720_000), "1h 02m");
    }

    /// The notice that OPENS the turn a fallback earned, in the harness's own register  -
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
                retry_after: None,
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

    /// The notice fires only past the threshold, and says what it knows: the number in the sentence is the measured
    /// silence and not the threshold, since the operator needs to know how long they waited.
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

    /// A round that has produced nothing publishes exactly one notice: a degraded provider took 12.44 s to its
    /// first byte and a non-streaming request returned nothing in 25, and neither could be told from a provider that
    /// was gone. Once, not once per slice, so a slow minute does not fill the session.
    #[test]
    fn a_silent_cloud_round_publishes_one_notice() {
        use letibot_sessionlog::SessionEvent;
        let hub = letibot_sessionlog::hub::Hub::new("silence-cloud");
        let watch = SilenceWatch::start(
            hub.clone(),
            Some("api.deepseek.com".into()),
            std::time::Duration::from_millis(50),
            "deepseek/deepseek-flash".into(),
            0,
            // Never set: this round says nothing at all.
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );
        // Long enough for several 100 ms slices - so a version that published per
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

    /// The silence threshold is the session's provider's: a `[provider.glm]` table moves it for GLM and
    /// leaves every other provider on the built-in 15 s, and a `/models` switch changes which one is read.
    #[test]
    fn the_silence_notice_uses_the_providers_configured_threshold() {
        use letibot_sessionlog::SessionEvent;
        let dir = std::env::temp_dir().join(format!("silence-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("leticode.toml");
        std::fs::write(
            &file,
            "[provider.glm]\nslow_notice_seconds = 1\n[provider.deepseek]\nslow_notice_seconds = 90\n",
        )
        .unwrap();
        let mut cfg = crate::config::Config::for_this_box(&dir);
        cfg.leticode = letibot_leticode_config_for_test(&file);
        let pc = |name: &str| crate::config::ProviderConfig {
            name: name.into(),
            model: None,
            api_key: None,
            thinking: false,
        };
        assert_eq!(cfg.deadlines_for(None).slow_notice.as_secs(), 15);
        assert_eq!(
            cfg.deadlines_for(Some(&pc("deepseek")))
                .slow_notice
                .as_secs(),
            90
        );
        let threshold = cfg.deadlines_for(Some(&pc("glm"))).slow_notice;
        assert_eq!(threshold.as_secs(), 1);
        let hub = letibot_sessionlog::hub::Hub::new("silence-cfg");
        let watch = SilenceWatch::start(
            hub.clone(),
            Some("api.z.ai".into()),
            threshold,
            "glm".into(),
            0,
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );
        std::thread::sleep(std::time::Duration::from_millis(1400));
        let said = hub
            .retained()
            .iter()
            .filter(|e| {
                matches!(&e.event, SessionEvent::Warning { code, .. } if code == "model_slow_first_byte")
            })
            .count();
        watch.finish();
        assert_eq!(said, 1, "the configured 1 s threshold fired");
    }

    /// `provider_timeout`, when a caller sets it, is the answer budget over the file's, and the first
    /// byte never outlasts the answer.
    #[test]
    fn the_provider_gets_both_deadlines_from_the_config() {
        let dir = std::env::temp_dir().join(format!("deadline-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("leticode.toml");
        std::fs::write(
            &file,
            "[provider.glm]\nfirst_byte_seconds = 120\nanswer_seconds = 900\n",
        )
        .unwrap();
        let mut cfg = crate::config::Config::for_this_box(&dir);
        cfg.leticode = letibot_leticode_config_for_test(&file);
        let glm = crate::config::ProviderConfig {
            name: "glm".into(),
            model: None,
            api_key: Some("k".into()),
            thinking: false,
        };
        let d = cfg.deadlines_for(Some(&glm));
        assert_eq!((d.first_byte.as_secs(), d.answer.as_secs()), (120, 900));
        // build_provider hands both to the provider, neither resetting the other.
        let hub = letibot_sessionlog::hub::Hub::new("deadline-cfg");
        let built = crate::harness::open::build_provider(
            &glm,
            &serde_json::json!({}),
            crate::harness::open::provider_notes(&hub),
            d,
        );
        assert!(built.is_ok(), "{:?}", built.err());
        cfg.provider_timeout = Some(std::time::Duration::from_secs(30));
        let d = cfg.deadlines_for(Some(&glm));
        assert_eq!((d.first_byte.as_secs(), d.answer.as_secs()), (30, 30));
    }

    /// A round that is STREAMING is not silent. The watch's clock was the round's start and it never
    /// saw a delta, so every cloud round over the threshold published `model_slow_first_byte` and a
    /// `stall` trouble while text was arriving, which on a thinking-mode provider is most rounds.
    #[test]
    fn a_delta_on_the_sink_ends_the_silence_watch() {
        use letibot_sessionlog::SessionEvent;
        use letibot_turn::{DeltaTarget, EventSink, TurnEvent};
        let hub = letibot_sessionlog::hub::Hub::new("silence-stream");
        let spoke = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        // The real path: the flag is the one the sink the engine streams through sets.
        let mut sink = CapturingSink::new(hub.clone());
        sink.note_speech_to(spoke.clone());
        let watch = SilenceWatch::start(
            hub.clone(),
            Some("api.deepseek.com".into()),
            std::time::Duration::from_millis(50),
            "deepseek/deepseek-flash".into(),
            0,
            spoke,
        );
        sink.emit(TurnEvent::Delta {
            turn_id: "t#1".into(),
            target: DeltaTarget::Text,
            text: "Hel".into(),
        });
        std::thread::sleep(std::time::Duration::from_millis(400));
        let said = hub
            .retained()
            .iter()
            .filter(|e| {
                matches!(&e.event,
                    SessionEvent::Warning { code, .. } if code == "model_slow_first_byte")
            })
            .count();
        assert!(
            !watch.finish(),
            "a round that spoke is not a stalled round, and the streak must not grow"
        );
        assert_eq!(said, 0, "a streaming round was named silent");
    }

    /// And a local round is never nagged about silence: a cold GGUF reload is 29-31 s and a long prefill is minutes,
    /// both progress. Two halves, since they are two mistakes: `silence_host` decides whether the route qualifies,
    /// and `SilenceWatch` decides what to do once it does.
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
            "deepseek/deepseek-flash".into(),
            0,
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
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

    /// A retry notice must name the host actually contacted: the loop named the LOCAL endpoint on a path that also
    /// serves cloud turns, so the operator debugged a server answering 200 while the failure was a resolver lookup
    /// against a provider host the message never named. The override comes from the backend, so a proxy is named.
    #[test]
    fn the_two_measured_refusal_bodies_are_recognised_with_their_numbers() {
        // llama.cpp, JSON, the numbers as fields - the structured half.
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
        // An OpenAI-shaped provider, plain text, the numbers in the sentence  -
        // deepseek's own words, request id and all.
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
        // And the same refusal as JSON - OpenAI's own envelope, `code` instead
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
        // A 400 about the TOKENS, not the length - measured in the same log.
        assert!(
            context_length_refusal(
                "{\"error\":{\"code\":400,\"message\":\"Prompt contains invalid tokens\",\
                 \"type\":\"invalid_request_error\"}}"
            )
            .is_none(),
            "an invalid_request_error about tokens is a backend error"
        );
        // llama.cpp's JSON with a DIFFERENT type - the structured field is the
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
        // And the old local failure - `500 Context size has been exceeded`,
        // plain text with neither anchor - stays what it was: unparseable,
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
        // A cloud route names ITS host - not the local one, which was the bug.
        let cloud = Cloud("api.deepseek.com");
        assert_eq!(retry_host(Some(&cloud), &local), "api.deepseek.com");
        assert_ne!(retry_host(Some(&cloud), &local), "127.0.0.1:8080");
        // And it follows the backend's own URL, so a moved endpoint is named as itself.
        let proxy = Cloud("10.0.0.7:8443");
        assert_eq!(retry_host(Some(&proxy), &local), "10.0.0.7:8443");
    }
}
