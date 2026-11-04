//! The gate's two sinks and the steering source: what a denial and an admission say on the session's log, and what a running turn is handed between deltas.
//! Hub-shaped, not the harness: built at open, handed to the gate and engine.

use super::*;

// ---------------------------------------------------------------------------
// Surfacing a denial
// ---------------------------------------------------------------------------

/// Every refusal, onto the session's own log, the moment it is decided: the operator otherwise learns
/// of a denial by noticing a task that stopped. It publishes rather than returning, because the gate
/// calls it from inside the tool path and the operator may be somewhere else entirely.
pub struct HubDenials {
    hub: Arc<Hub>,
}

impl HubDenials {
    pub fn new(hub: Arc<Hub>) -> Self {
        HubDenials { hub }
    }
}

/// Where a gate admission goes: the channel in the other direction from [`HubDenials`], and a seam
/// because an admission is not a denial and must not be drawn as one. It collects the sentence and
/// [`Harness::flush_gate_admissions`] says it at a round boundary.
pub struct HubAdmissions {
    lines: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl HubAdmissions {
    pub fn new(lines: std::sync::Arc<std::sync::Mutex<Vec<String>>>) -> Self {
        HubAdmissions { lines }
    }
}

impl letibot_tools::adjudicate::AdmissionSink for HubAdmissions {
    fn admitted(&self, notice: &letibot_tools::adjudicate::AdmissionNotice) {
        // **One line, and the command is the part a reader scans for.** The reason is in the
        // audit row (`basis`); what a reader of the transcript needs is *which command ran
        // without me* and *which rule said so*.
        self.lines
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(format!(
                "admitted without asking — `{}` {} · inside the measured boundary ({}, \
                 {}), the row and `/gate` carry the full reason",
                notice.tool,
                notice
                    .summary
                    .trim_start_matches(&format!("`{}` ", notice.tool)),
                notice.boundary,
                notice.mode
            ));
    }
}

impl DenialSink for HubDenials {
    fn denied(&self, notice: &DenialNotice) -> Reach {
        // Read before publishing: a head that is not attached when the refusal is raised has
        // not been told, whatever it later finds in the snapshot.
        let heads = self.hub.deciding_heads().len();
        // And an ask still open is one a head that attaches is handed, so the model may be told
        // an answer given then is read. Both read here, where the refusal is decided.
        let open = !self.hub.open_ask_ids().is_empty();
        let (repeat_count, breaker_open) = match notice.repeat {
            BreakerState::Closed => (1u32, false),
            BreakerState::Repeat { consecutive } => (consecutive.max(1) as u32, false),
            BreakerState::Open { consecutive } => (consecutive.max(1) as u32, true),
        };
        self.hub.publish(SessionEvent::DenialRaised {
            request_id: notice.request_id.clone(),
            turn_id: notice.turn_id.clone(),
            call_id: notice.call_id.clone(),
            tool: notice.tool.clone(),
            summary: notice.summary.clone(),
            baseline: notice.baseline.clone(),
            by: notice.by.clone(),
            basis: notice.basis.clone(),
            tier: notice.tier.to_string(),
            outcome: notice.outcome.to_string(),
            repeat_count,
            breaker_open,
            grant: notice.grant.clone(),
        });
        match (heads, open) {
            (0, false) => Reach::Nobody,
            (0, true) => Reach::OpenAsk,
            _ => Reach::OnScreen,
        }
    }
}

// ---------------------------------------------------------------------------
// Steering
// ---------------------------------------------------------------------------

/// Steering from the head socket, the intent check, a fired monitor and a settlement: the step
/// boundary is the only place any of them can be injected, so they share one source. Provenance is
/// recorded here, because after the engine appends them they are four identical `User` items.
pub struct HubSteering {
    pub(crate) hub: Arc<Hub>,
    /// Where the speaker of each injected message is recorded. `None` in a test
    /// that only wants the head's queue.
    pub(crate) trail: Option<Arc<TrailMirror>>,
    /// Text this harness generated for the next step boundary: the intent diff's
    /// findings, and a monitor that fired mid-turn.
    pub(crate) injected: Arc<Mutex<VecDeque<String>>>,
    /// This session's monitors, when it has an exec backend.
    pub(crate) monitors: Option<Arc<Monitors>>,
    /// How many monitors had settled when this source last looked. Shared with the
    /// harness so a firing is delivered exactly once, whether it is picked up
    /// mid-turn here or between turns by [`Harness::wake`].
    pub(crate) monitor_cursor: Arc<AtomicUsize>,
    /// **This session's results, delivered by R79's rules** - the immediates at every round
    /// boundary, the held batch only past the starvation cap. `None` in a test that has no
    /// daemon behind it.
    pub(crate) deliveries: Option<Deliveries>,
    /// **This session's children** - the tree's downward edge. An `Interrupt` that reaches a
    /// running turn IS this session being stopped, and a supervisor stops its children before
    /// it goes (see [`HarnessTaskRunner`]). `None` in a test with no session behind it.
    pub(crate) subagents: Option<Arc<dyn letibot_tools::builtins::task::TaskRunner>>,
    /// Messages handed back by a failed turn, drained before the hub's queue so the next poll finds
    /// them first. Shared with the harness on purpose: a `HubSteering` is built per round, and one
    /// that owned them lost them when the round let go.
    pub(crate) held: Arc<Mutex<VecDeque<SteeringMessage>>>,
    /// **An interrupt this turn took**, shared with the harness the way `held` is: what the turn's
    /// ending is told, so a stop does not ring a wake of its own a moment after the operator's.
    pub(crate) stopped: Arc<AtomicBool>,
    /// **This session's board**, for the one command a running turn applies to it: the
    /// operator's answer to a row's question (`CommandKind::AnswerTodo`), which reaches the model
    /// at the next step boundary rather than after the turn. `None` in a test with no board.
    pub(crate) todos: Option<Arc<TodoBoard>>,
    /// This session's own parent, if it has one: the one fact `message_from_a_session` needs to word
    /// a message correctly. A message says "your parent session" only when the sender IS this
    /// session's parent, and the between-turns door reads the same field off the config.
    pub(crate) parent: Option<String>,
    /// Whether the next poll is a step boundary, which is the only poll that may read the
    /// session's results: see [`SteeringSource::at_a_step_boundary`].
    pub(crate) at_boundary: bool,
}

impl HubSteering {
    pub fn new(hub: Arc<Hub>) -> Self {
        HubSteering {
            hub,
            trail: None,
            injected: Arc::new(Mutex::new(VecDeque::new())),
            monitors: None,
            monitor_cursor: Arc::new(AtomicUsize::new(0)),
            deliveries: None,
            subagents: None,
            held: Default::default(),
            stopped: Default::default(),
            todos: None,
            parent: None,
            at_boundary: false,
        }
    }
}

impl SteeringSource for HubSteering {
    fn try_next(&mut self) -> Option<SteeringMessage> {
        // A handed-back message comes out first, with the flags it had when it was taken: it was taken
        // before anything still in the hub's queue, so a later message cannot reorder an interrupt. It
        // is not recorded in the trail here, because `try_next` recorded it the first time it was taken.
        if let Some(m) = self
            .held
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front()
        {
            return Some(m);
        }
        // The head first: somebody is waiting on it. Steering-scoped, so a
        // compaction queued behind this turn stays queued for the worker - see
        // [`Hub::try_steering_command`].
        if let Some(cmd) = self.hub.try_steering_command() {
            return match cmd.kind {
                CommandKind::Prompt { text, attachments } => {
                    // The operator, typing into a running turn: the one utterance that can authorise the
                    // action it is racing. The chips are read HERE and not by the engine, because a turn's
                    // library has no business opening the operator's files.
                    if let Some(t) = &self.trail {
                        t.say(Speaker::Operator, &text, Some(Instant::now()));
                    }
                    let (extra, refused) = crate::attachments::parts(&attachments);
                    for said in &refused {
                        self.hub.publish(letibot_sessionlog::SessionEvent::Warning {
                            code: "attachment_refused".into(),
                            detail: said.clone(),
                            compaction: None,
                        });
                    }
                    Some(SteeringMessage::operator_with(text, extra))
                }
                CommandKind::Interrupt { reason } => {
                    // The children first, then this session: the one door an interrupt reaches a turn in
                    // flight through. Nothing here waits for the children, because a stop that blocked on
                    // its subtree would hold the turn it is stopping open.
                    stop_children_first(self.subagents.as_ref(), &self.hub);
                    // And the turn's ending is told: a session the operator stopped must not ring a wake
                    // for the results it held, which is a turn nobody asked for.
                    self.stopped.store(true, Ordering::SeqCst);
                    Some(SteeringMessage::urgent(reason))
                }
                CommandKind::Message { from, text } => {
                    // A parent's live correction is an agent's utterance, so it is recorded as
                    // `Speaker::Agent`: an operator's words can authorise and be taken back, a parent's
                    // cannot. It arrives at the next round boundary, as context rather than an interrupt.
                    let said = message_from_a_session(&from, &text, self.parent.as_deref());
                    if let Some(t) = &self.trail {
                        t.say(Speaker::Agent, &said, Some(Instant::now()));
                    }
                    Some(SteeringMessage::from_a_session(said, from))
                }
                // The operator answered a row's question while the turn runs: an operator's words are
                // immediate, so the answer is applied to the board HERE and the model reads it at this
                // step boundary, as a notice that stands alone and a take-back cannot drop.
                CommandKind::AnswerTodo {
                    content,
                    question,
                    answer,
                } => {
                    let settled = match &self.todos {
                        Some(board) => {
                            crate::operator_ask::settle(board, &content, &question, &answer)
                        }
                        None => Err("this session has no board".into()),
                    };
                    match settled {
                        Ok(line) => {
                            if let Some(t) = &self.trail {
                                t.say(Speaker::Operator, &line, Some(Instant::now()));
                            }
                            Some(SteeringMessage::normal(line))
                        }
                        Err(why) => {
                            self.hub.publish(SessionEvent::Warning {
                                code: "todo_answer_refused".into(),
                                detail: format!(
                                    "your answer on `{}` was not taken: {why}",
                                    content.trim()
                                ),
                                compaction: None,
                            });
                            self.try_next()
                        }
                    }
                }
                // Nothing else here: the filter above only hands over prompts, interrupts, a
                // parent's message and an answer to a row's question, and anything else stays
                // queued for the worker.
                _ => None,
            };
        }
        // Then anything this harness queued for the boundary.
        if let Some(text) = self
            .injected
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front()
        {
            if let Some(t) = &self.trail {
                t.say(Speaker::Agent, &text, Some(Instant::now()));
            }
            return Some(SteeringMessage::normal(text));
        }
        // Then the room, at the boundary a running turn actually reaches. A room event becomes a
        // sentence on the connection's thread and `wake` reads it between turns, which is the wrong door
        // for a long turn: the note arrives after the work it would have changed. One queue, so once.
        let room = crate::room::wake::take(&self.hub.session_id());
        if !room.is_empty() {
            let text = room.join("\n\n");
            if let Some(t) = &self.trail {
                t.say(Speaker::Agent, &text, Some(Instant::now()));
            }
            return Some(SteeringMessage::normal(text));
        }
        // Then a background job or subagent of THIS session's own that has ended, above the
        // monitors because that block returns early with no registry. **Only at a step boundary**:
        // saying it reads the whole board and takes the held set (R91.16).
        if let Some(d) = &self.deliveries
            && std::mem::take(&mut self.at_boundary)
            && let Some(text) = d.at_round_boundary()
        {
            if let Some(t) = &self.trail {
                t.say(Speaker::Agent, &text, Some(Instant::now()));
            }
            return Some(SteeringMessage::normal(text));
        }
        // Then a monitor that fired while this turn was running. Between turns the
        // same firing arrives through `Harness::wake`; the shared cursor is what
        // stops it arriving twice.
        let monitors = self.monitors.as_ref()?;
        let since = self.monitor_cursor.load(Ordering::SeqCst);
        let settled = monitors.settled_count();
        if settled <= since {
            return None;
        }
        let fired: Vec<_> = monitors.firings().into_iter().skip(since).collect();
        self.monitor_cursor.store(settled, Ordering::SeqCst);
        if fired.is_empty() {
            return None;
        }
        // A firing is immediate by R79's rules and recorded as one - every one, even those
        // the sentence leaves out (R93.C8).
        if let Some(d) = &self.deliveries {
            d.log_firings(&fired);
        }
        let text = monitor_notice(&fired)?;
        if let Some(t) = &self.trail {
            t.say(Speaker::Agent, &text, Some(Instant::now()));
        }
        // The typed door: a firing is a delivery, so the row it becomes says so.
        Some(SteeringMessage::notice(text))
    }

    /// The take-back the operator issued from a head: the hub drops the head's still-queued prompts and
    /// the engine's `Pending` drops the held text when this returns true. Both halves are one recall  -
    /// one prompt is in the hub's queue, another is already held here.
    fn try_withdraw(&mut self) -> bool {
        self.hub.try_withdraw_command()
    }

    /// Hand a message back to the source, for a turn that failed: the words go back rather than being
    /// appended, because appending on an error path advances the ledger and the prefix. No `t.say` here,
    /// because `try_next` already recorded it; the message keeps its flags, being the same value.
    fn give_back(&mut self, msgs: Vec<SteeringMessage>) {
        self.held
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend(msgs);
    }

    /// The engine's word that the next drain is a round's boundary: the one poll that reads the
    /// session's results.
    fn at_a_step_boundary(&mut self) {
        self.at_boundary = true;
    }
}
