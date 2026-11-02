//! **The operator's own calls**: the door's admitted run, the `!` line's two halves,
//! and the rows and notes each of them leaves.
use super::*;

impl Harness {
    /// **Record an operator's own call as their act** — R24 part two, decisions 2 and 4.
    ///
    /// The row this writes is what makes the difference between three facts a corpus must
    /// not confuse: *the guard allowed this*, *a rule allowed this*, and **the person ran it
    /// themselves**. `by` is `human:<who>` — the vocabulary the corpus already carries 121
    /// `human:dead` and 37 `human:leticl` rows of — `asked` is `true` because a person
    /// answered it (they are the answer), and `consulted` is `false` because no model spoke.
    /// That last field is what stops `agreement` reading it as the guard's success: the gate
    /// only labels a row against the model's verdict when a model gave one.
    pub fn admit_operator_call(
        &mut self,
        call_id: &str,
        name: &str,
        arguments: &str,
        who: &str,
    ) -> Result<(), String> {
        let Some(sink) = &self.corpus else {
            // **Not a silent success.** A daemon with no store cannot keep the record, and
            // a head told "admitted" would run a call whose admission is nowhere — which is
            // the one thing this arm exists to prevent.
            return Err(
                "this daemon keeps no corpus, so an operator's own call cannot be recorded \
                 as theirs. Refusing rather than admitting it unrecorded."
                    .into(),
            );
        };
        let request_id = format!("op-{call_id}");
        let row = letibot_tools::CorpusRow {
            request_id: request_id.clone(),
            session_id: self.hub.session_id(),
            turn_id: String::new(),
            action: format!("`{who}` ran `{name}` themselves, from this console"),
            trail: letibot_tools::AuthorisationTrail {
                utterances: Vec::new(),
                provenance: letibot_tools::authorise::TrailProvenance::Scanned {
                    messages_scanned: 0,
                    operator_messages: 0,
                },
            },
            shown: None,
            reply: None,
            shape: None,
            shape_class: None,
            baseline: String::new(),
            tier: "may_approve",
            tool: name.to_string(),
            arguments: serde_json::from_str(arguments)
                .unwrap_or(serde_json::Value::String(arguments.to_string())),
            mode: "head-run".into(),
            options: Vec::new(),
            agent: "operator".into(),
            model_verdict: None,
            verdict: Some("selected".into()),
            verdict_by: Some(format!("human:{who}")),
            verdict_basis: Some(
                "the operator ran this from their own console; there was nobody left to ask".into(),
            ),
            p_allow: None,
            decision_ms: 0,
            brief_format: "head-run",
            consulted: false,
            oracle_reading: None,
            effect: "admit",
            asked: true,
            operator: None,
        };
        sink.decided(&row);
        self.hub
            .publish(letibot_sessionlog::SessionEvent::OperatorCallAllowed {
                call_id: call_id.to_string(),
                name: name.to_string(),
                who: who.to_string(),
                arguments: arguments.to_string(),
            });
        Ok(())
    }

    /// **Append what the operator's call produced**, with its `origin` set.
    ///
    /// Through `append_imported`'s writer and no other, so the row lands in the ledger, the
    /// store and every head the same way a turn's rows do — and carries
    /// `origin: Some(Operator { who })`, which is the whole of decision 1: a head draws it as
    /// the person's act rather than the model's, and no head has to infer it.
    pub fn finish_operator_call(
        &mut self,
        call_id: &str,
        name: &str,
        who: &str,
        outcome: letibot_transcript::ToolOutcome,
        payload: &str,
    ) -> Result<(), HarnessError> {
        let item = TranscriptItem::ToolResult {
            call_id: call_id.to_string(),
            name: name.to_string(),
            outcome,
            payload: payload.to_string(),
            edit: None,
            origin: Some(letibot_transcript::CallOrigin::Operator {
                who: who.to_string(),
            }),
            media: None,
        };
        self.append_imported(&[item])
    }

    /// **Run an operator's admitted call, in this session, and put it in the transcript**
    /// — R31's `execute`.
    ///
    /// # Why the daemon runs it rather than the head
    ///
    /// Not convenience: **the payload has to be the one this program would have produced.**
    /// The corpus row for an operator's call says `human:<who>` ran `web_fetch`; a head that
    /// ran its own HTTP client would make that row false, and the model's next prompt would
    /// hold text `web_fetch` never returned. Here the tool is the seated one, so the byte
    /// caps, the spill policy, the network rules and the scratch directory are the same ones
    /// a model's call gets.
    ///
    /// **The gate is not consulted and must not be.** The operator's call has already been
    /// admitted against the door's allowlist and written as `human:<who>` — the row's own
    /// basis says *"there was nobody left to ask"*. Re-gating here would either double-record
    /// the decision or, at `/mode automode`, let a model refuse the person's own act.
    /// [`ToolRuntime::invoke_operator`] is that distinction made explicit rather than a flag
    /// at the call site.
    ///
    /// # The size, said before the row lands
    ///
    /// R31's fourth consequence: *"a 40k-token page is 40k of context the operator chose to
    /// buy — show the size before it lands, because the alternative is discovering it at the
    /// next compaction."* So the measurement is published as a note **between** the run and
    /// the append, which is the only window in which it is true: after the run the size is
    /// known, and before the append nothing has been added to what the model reads.
    pub fn run_operator_call(
        &mut self,
        call_id: &str,
        name: &str,
        arguments: &str,
        who: &str,
    ) -> Result<(), String> {
        let call = ToolCall {
            id: call_id.to_string(),
            name: name.to_string(),
            arguments: arguments.to_string(),
        };
        // The runtime's own events go to a null sink: the transcript row is the durable
        // record and every head draws *it*, so a second copy as an event would be §13.2b's
        // "one fact, two places". What the tool emits here is a `ToolStarted`/`ToolProgress`
        // pair for a call no head proposed.
        //
        // **Null, and WATCHED** — see [`Self::unprompted_sink`]. A call from this door can
        // return `Backgrounded` (`task` above all, and any tool the door's list grows to hold),
        // and an unwatched sink leaves that handle in nobody's table: the row promises a
        // completion and no watcher exists to queue one.
        let mut quiet = self.unprompted_sink();
        // `turn_id` is empty on purpose: this call belongs to no turn, and a head that saw a
        // turn id here would draw the row inside a turn that did not propose it.
        let result = self.runtime.invoke_operator("", &call, &mut quiet);
        // **What the model will read**, which is the number the operator is buying: the
        // payload is *already* what the spill policy left, so this is the post-cap figure and
        // not the tool's raw output. The full size, when it differs, is on the `SpillRef` the
        // runtime recorded — reported rather than recomputed, because a second implementation
        // of the cap is a second answer.
        let payload = result.render();
        let read = payload.len();
        let spilled = match &result.spill {
            Some(s) => format!(
                "; {} byte(s) were produced and the rest went to the spill store \
                 (`read_spill hash={}`)",
                s.full_bytes, s.hash
            ),
            None => String::new(),
        };
        // **Said before it is appended.** Not a refusal and not a gate — the call has already
        // been admitted — but the disclosure R31 asks for: the price is on the screen while
        // the operator can still do something about it, rather than at the next compaction.
        self.import_note(
            "operator_call_ran",
            format!(
                "`{who}` ran `{name}` from their own console: {read} byte(s) of context \
                 (about {} tokens) reach the model from its next turn{spilled}. No reply is \
                 generated — this is context, not a request.",
                read / 4,
            ),
        );
        // The row, with the outcome the tool gave and the size the note above promised.
        let outcome = result.outcome.clone();
        self.finish_operator_call(call_id, name, who, outcome, &payload)
            .map_err(|e| e.to_string())
    }

    /// **Run the operator's own shell line (`!`), and append BOTH halves as rows.**
    ///
    /// The `!` feature's daemon half. The frame's own docs (`ClientFrame::OperatorShell`)
    /// record why this is a frame of its own rather than the door; this is the run.
    ///
    /// # Why the daemon runs it, in one sentence the door already wrote
    ///
    /// [`Harness::run_operator_call`] runs the door's calls here "because the payload has to be
    /// the one this program would have produced" — and that is a fortiori true of a shell
    /// command, whose payload is bytes a program wrote. A head has no exec host, no confinement,
    /// no scratch directory and **no standing environment** — and the standing environment is
    /// what carries `LETIBOT_SOCKET`, `LETIBOT_SESSION` and `SUDO_ASKPASS`, without which `sudo`
    /// cannot ask anybody for a password. Running it anywhere but here is the feature not
    /// working.
    ///
    /// # The gate is not consulted, and neither is anything else that asks
    ///
    /// `ToolRuntime::invoke_operator` is the door's own ungated path — same schema lookup, same
    /// argument salvage, same byte caps, same spill policy, same `bash` execution path with its
    /// confinement and its `sudo` shim — and no `gate.admit`, no view-grant card. A session that
    /// does not seat `bash` (the default read-only seat) gets the runtime's own honest refusal
    /// as the result row, naming the tools it does have.
    ///
    /// # What lands, and in which order
    ///
    /// Two rows, one append: the operator's typed line as a `User` row with
    /// `speaker: Operator` — their words, verbatim, bang included, because `Speaker::Operator`
    /// means *the person at the keyboard* and that is exactly who typed it — then the result as a
    /// `ToolResult` named `bash` with `origin: CallOrigin::Operator`, which is the tree's existing
    /// vocabulary for *a person ran this call* and the row every head already draws with the
    /// tool-output treatment. The output is NOT speech: it is a program's bytes, reported, and
    /// giving it a `Speaker` would be pretending a shell spoke.
    ///
    /// # Why this synchronous composition still exists beside the split one
    ///
    /// [`Sessions::dispatch`] hands a between-turns `!` line to its own thread (see
    /// `crate::bangrun`) — a run that held the worker held every other session's work with it.
    /// Two paths still run the whole thing HERE, on the calling thread, and both are deliberate:
    /// the round boundary's pickup (`apply_queued_head_run`), whose whole point is that a turn
    /// already running reads the rows at its next round; and every caller that has no worker to
    /// hand back to — `letibot-m1`, and the tests that drive a harness directly. The pieces are
    /// shared with the split path ([`Self::prepare_operator_shell`],
    /// [`Self::settle_operator_shell`]), so there is one minting of the call id, one note, one
    /// row shape, whichever thread runs them.
    pub fn run_operator_shell(&mut self, line: &str, who: &str) -> Result<(), String> {
        let (call_id, call) = self.prepare_operator_shell(line)?;
        // The runtime's own events go to a null sink, exactly as the door's do: the
        // transcript row is the durable record and every head draws *it*. What the
        // tool emits here is a `ToolStarted`/`ToolProgress` pair for a call no head
        // proposed — and for a command the operator is watching the composer over.
        //
        // **And watched**, for the reason [`Self::unprompted_sink`] gives: a run moved to the
        // background mid-flight (Ctrl+O) settles as `Backgrounded`, and the row it leaves says
        // its completion will arrive on its own.
        let mut quiet = self.unprompted_sink();
        // `turn_id` empty on purpose, as the door's runs are: this call belongs to no
        // turn, and a head that saw a turn id would draw the row inside a turn that
        // did not propose it.
        let result = self.runtime.invoke_operator("", &call, &mut quiet);
        // The pieces, not the type: [`Self::settle_operator_shell`] takes what the split
        // path carries over the hub, and this path hands it the same shape.
        let spill = result
            .spill
            .as_ref()
            .map(|s| (s.full_bytes, s.hash.clone()));
        let outcome = result.outcome.clone();
        self.settle_operator_shell(line, who, &call_id, outcome, result.render(), spill)
    }

    /// **Mint the operator's own call: the `bang-<n>` id and the `bash` call it names.**
    ///
    /// The half of [`Self::run_operator_shell`] that needs the harness — the sequence counter
    /// (`bang_seq`) is this session's, so the id is minted HERE, on the worker, before the run
    /// is handed anywhere. What comes back is everything a thread that is not this one needs to
    /// run the command: a [`ToolCall`] whose payload is the line minus its bang, already
    /// validated by [`letibot_sessionlog::operator_shell_command`].
    pub fn prepare_operator_shell(&mut self, line: &str) -> Result<(String, ToolCall), String> {
        // **A pasted block is not a command, and the daemon checks this itself.** The composer
        // refuses it too, and that is not a second opinion: a `!` line reaches here from any
        // head that speaks the protocol, and the crate both halves share holds the rule. A
        // five-line notice used to become one `!` command whose newlines the shell split,
        // running `usage: sudo …`, `it.`, `so` and `/proc` as their own commands.
        if let Some(why) = letibot_sessionlog::operator_line_refusal(line) {
            return Err(why);
        }
        let command = letibot_sessionlog::operator_shell_command(line)
            .ok_or_else(|| format!("`{line}` is not a `!` command — nothing was run"))?
            .to_string();
        self.bang_seq += 1;
        let call_id = format!("bang-{}", self.bang_seq);
        let call = ToolCall {
            id: call_id.clone(),
            name: "bash".to_string(),
            arguments: serde_json::json!({ "command": command }).to_string(),
        };
        Ok((call_id, call))
    }

    /// **Append what the operator's run produced: the note and the two rows, one append.**
    ///
    /// The half of [`Self::run_operator_shell`] that needs the harness again after the run —
    /// which is why it runs on the worker even when the run did not: the transcript has one
    /// writer, and this is it. Everything here is byte-for-byte what the synchronous path
    /// always did, in the same order: the size note first (so the disclosure is on the log
    /// before the rows that carry the bytes), then the `User` row with the typed line verbatim,
    /// then the `bash` `ToolResult` with `origin: CallOrigin::Operator`.
    ///
    /// It takes the run's pieces rather than a [`letibot_tools::ToolResult`] because the
    /// split path's result crosses a thread as [`CommandKind::OperatorShellResult`] —
    /// already rendered and already carrying its spill numbers, so the worker never needs
    /// the type back. The synchronous path renders at its own call site; both arrive here
    /// identical.
    pub fn settle_operator_shell(
        &mut self,
        line: &str,
        who: &str,
        call_id: &str,
        outcome: letibot_transcript::ToolOutcome,
        payload: String,
        spill: Option<(usize, String)>,
    ) -> Result<(), String> {
        // The command as the note names it: the line minus its bang, by the same validator
        // the frame and [`Self::prepare_operator_shell`] used. The `None` arm is not a
        // silent default — it is the caller's own claim carried through: a line settled
        // without a bang was still run as written, and the note says so rather than
        // inventing a command nobody typed.
        let command = letibot_sessionlog::operator_shell_command(line)
            .map(str::to_string)
            .unwrap_or_else(|| line.to_string());
        // **What the model will read** — the same disclosure the door's note makes,
        // for the same reason: the price is on the screen while the operator can still
        // do something about it, rather than at the next compaction.
        let read = payload.len();
        let spilled = match &spill {
            Some((full_bytes, hash)) => format!(
                "; {full_bytes} byte(s) were produced and the rest went to the spill store \
                 (`read_spill hash={hash}`)"
            ),
            None => String::new(),
        };
        self.import_note(
            "operator_shell_ran",
            format!(
                "`{who}` ran `{command}` from their own console: {read} byte(s) of context \
                 (about {} tokens) reach the model in the turn this line starts{spilled}. \
                 The turn is the sending half — the operator asked for their command's \
                 result to be *sent to the model as a message*, so a reply is generated.",
                read / 4,
            ),
        );
        let rows = [
            TranscriptItem::User {
                speaker: letibot_transcript::Speaker::Operator,
                parts: vec![UserPart::Text {
                    text: line.to_string(),
                }],
            },
            TranscriptItem::ToolResult {
                call_id: call_id.to_string(),
                name: "bash".to_string(),
                outcome,
                payload,
                edit: None,
                origin: Some(letibot_transcript::CallOrigin::Operator {
                    who: who.to_string(),
                }),
                media: None,
            },
        ];
        self.append_imported(&rows).map_err(|e| e.to_string())
    }

    /// **Write down the question the model asked and the answer a person gave it.**
    ///
    /// Called from the round loop with the result of an [`ASK_USER_QUESTION`] call — *the*
    /// place a result is appended, and the reason the tool does not write these rows itself:
    /// the transcript has one writer, and it is the session. What the tool returns is the
    /// answer the model reads; what a PERSON said is written here, as their own row.
    ///
    /// # What lands, and in which order
    ///
    /// The question as the session's own row ([`question_row_text`] — never the operator's,
    /// because the trail may cite a `User` row of theirs and a question is not theirs), then
    /// the answer as a `User` row with `speaker: Operator` whose text is **exactly** what
    /// they said ([`operator_answer_text`]).
    ///
    /// # Why they land AFTER the call's own result
    ///
    /// The brief's shape — the person's row, then the result — is the `!` line's, and it is
    /// right there because nothing above the result proposed the call. Here something did:
    /// the model's `Assistant { tool_calls }` row is already on the log, and
    /// `letibot_provider::messages::pair_tool_calls` **closes the answering window on any
    /// non-`tool` row**. A `User` row between the proposal and its result therefore makes
    /// the model read *"no result was recorded for this call"* beside the real one, which is
    /// a lie about the exchange the rows exist to record. So the result goes first and the
    /// person's half follows it — the round loop appends both in one batch, all results
    /// before all of these.
    ///
    /// # What makes the result the person's act
    ///
    /// The row keeps its payload — the model reads the same bytes it always did — and takes
    /// `origin: Some(CallOrigin::Operator { who })`, which is what makes every head draw it
    /// as the person's act at every verbosity (`tui::app::visibility::operator_act`). The
    /// operator's report this answers is *"by the way i dont see my answer to the selector"*:
    /// the card is a live overlay and this row is what is left of it afterwards.
    ///
    /// # The trail
    ///
    /// Both rows are recorded where the composer's own speech is (`TrailMirror::say`), and
    /// the SPEAKER is the whole point: the answer goes in as `Speaker::Operator`, so a *yes*
    /// given through a question card can be cited as authorisation exactly as a *yes* typed
    /// into the composer can. Before this it could not — the answer was a tool payload and
    /// nothing else, and no `User` row of theirs existed to cite.
    pub(crate) fn settle_asked(&mut self, result: &mut TranscriptItem) -> Vec<TranscriptItem> {
        let Some(asked) = self.asked.lock().unwrap_or_else(|e| e.into_inner()).take() else {
            return Vec::new();
        };
        // **Only an answer the tool ACCEPTED.** `ask_user_question` reports an abstention as
        // `Abstained` — a real answer — and everything it refused (an empty answer, an option
        // index nobody offered, a note qualifying nothing) as `not_run`. Writing a person's
        // row for one of those would attribute to them a sentence the harness rejected.
        let TranscriptItem::ToolResult {
            outcome, origin, ..
        } = result
        else {
            return Vec::new();
        };
        if !matches!(
            outcome,
            letibot_transcript::ToolOutcome::Ok | letibot_transcript::ToolOutcome::Abstained { .. }
        ) {
            return Vec::new();
        }
        let Some(said) = operator_answer_text(&asked) else {
            return Vec::new();
        };
        *origin = Some(letibot_transcript::CallOrigin::Operator {
            who: asked.by.clone(),
        });
        let at = Some(Instant::now());
        self.trail
            .say(Speaker::Agent, &question_row_text(&asked), at);
        self.trail.say(Speaker::Operator, &said, at);
        asked_rows(&asked, &said).to_vec()
    }

    /// **The runtime as a shareable handle**, for the thread that runs the operator's
    /// command. One exec host per session is a fact — `job_list`, a promotion and the
    /// run ender at shutdown all name jobs in this one — so the run goes through THIS
    /// runtime rather than a second one, and the clone is the whole of the handover.
    ///
    /// **The sink is the caller's, and it decides whether a `Backgrounded` outcome is
    /// watched.** A runtime does not know what a watcher is, so a handle used with a bare
    /// `NullToolSink` mints a job or subagent handle in nobody's table: the settlement is
    /// queued for no session and no bell rings. Whoever invokes through this must pass
    /// [`JobWatchSink::new`] over [`Harness::job_watchers`] — which is what
    /// [`Harness::unprompted_sink`] is for the two doors that live here, and what
    /// `crate::bangrun` carries across its thread.
    pub fn runtime_handle(&self) -> std::sync::Arc<ToolRuntime> {
        self.runtime.clone()
    }

    /// **Attach the worker's serialization state** — the same `Arc` the dispatch arm
    /// holds, so both places that could start an operator run read one fact. Called by
    /// `Sessions` at open, once per harness; a second call would replace the state and
    /// orphan any run in flight, which is why it is a setter rather than an argument:
    /// the harness can be built before the map that owns the state exists.
    pub fn set_bang_state(&mut self, state: std::sync::Arc<crate::bangrun::State>) {
        self.bang = Some(state);
    }

    /// Whether an operator run for this session is in flight on its own thread. The
    /// mid-turn pickup's question; `false` when no state was attached, which keeps the
    /// bare-harness callers on the synchronous path they always had.
    pub(crate) fn bang_in_flight(&self) -> bool {
        self.bang.as_ref().is_some_and(|s| s.in_flight())
    }

    /// Park a `!` line typed mid-turn while a run is in flight: it starts when that
    /// run settles, on the worker, between turns. Only called when [`Self::bang_in_flight`]
    /// answered true, so the state is there to park into.
    pub(crate) fn park_bang_line(&self, line: String, who: String) {
        if let Some(s) = &self.bang {
            s.queue(line, who);
        }
    }

    /// **The turn the operator's own `!` line starts** — the sending half.
    ///
    /// [`Harness::run_operator_shell`] appends the two rows; this is what makes them a
    /// message rather than silent context. The operator asked for both halves in their own
    /// words — the result *"printed (with meaningfull truncution and scrolled collapse,
    /// like we do for tool output now) and sent to model as a message"* — and corrected
    /// the design when the rows landed without the turn: *"my commands should start a turn
    /// and should be printed to me"*.
    ///
    /// **Nothing is appended here, on purpose.** The line and its output are already rows
    /// and already on the log, so a framing item would be a third row saying what the two
    /// above it say. This is `submit_item` minus its append: open a turn, run the rounds,
    /// and the model reads the operator's line and the command's output as the head of
    /// that turn.
    ///
    /// **Called only between turns.** Mid-turn the line is taken at the round boundary
    /// (`apply_queued_head_run`) and the turn already running reads the rows at its next
    /// round; starting one there would be a second turn about one command.
    pub fn run_after_operator_shell(&mut self) -> Result<Reply, HarnessError> {
        self.trail.begin_turn();
        let out = self.run_rounds();
        self.note_turn_ended();
        out
    }

    /// Append imported rows through the one writer, exactly as a turn's rows go in.
    pub(crate) fn append_imported(&mut self, items: &[TranscriptItem]) -> Result<(), HarnessError> {
        let mut sink = CapturingSink::new(self.hub.clone());
        self.session.append_items(&self.engine, items, &mut sink)?;
        self.reconcile(&mut sink, items);
        Ok(())
    }

    /// A warning on this session's log — the one place an import reports itself.
    pub(crate) fn import_note(&self, code: &str, detail: String) {
        self.hub.publish(letibot_sessionlog::SessionEvent::Warning {
            code: code.to_string(),
            detail,

            compaction: None,
        });
    }
}
