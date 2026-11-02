//! The operator's own calls: the door's admitted run, the `!` line's two halves, and the rows and
//! notes each of them leaves.
use super::*;

impl Harness {
    /// Record an operator's own call as their act: `by` is `human:<who>`, `asked` is true because a
    /// person answered it, and `consulted` is false because no model spoke. That last field is what
    /// stops `agreement` reading the row as the guard's success.
    pub fn admit_operator_call(
        &mut self,
        call_id: &str,
        name: &str,
        arguments: &str,
        who: &str,
    ) -> Result<(), String> {
        let Some(sink) = &self.corpus else {
            // Not a silent success: a daemon that cannot keep the record must not let a head run
            // a call whose admission is nowhere.
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

    /// Append what the operator's call produced, with its `origin` set. Through `append_imported` and no other writer,
    /// so the row lands in the ledger, the store and every head as a turn's rows do, carrying `origin: Some(Operator)`:
    /// no head has to infer that the person, and not the model, made this call.
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

    /// Run an operator's admitted call, in this session, and put it in the transcript: the daemon runs it because the
    /// payload must be the one this program would have produced, and the gate is not consulted because the call was
    /// already admitted against the door's allowlist. The size is noted between the run and the append.
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
        // A null sink, and watched: the transcript row is the durable record and every head draws it, so a second
        // copy as an event would be one fact in two places. Watched because a call from this door can return
        // `Backgrounded`, and an unwatched sink would leave that handle in nobody's table.
        let mut quiet = self.unprompted_sink();
        // `turn_id` is empty on purpose: this call belongs to no turn, and a head that saw a
        // turn id here would draw the row inside a turn that did not propose it.
        let result = self.runtime.invoke_operator("", &call, &mut quiet);
        // What the model will read, which is what the operator is buying: the payload is already
        // what the spill policy left, and the full size is reported off its `SpillRef` rather than
        // recomputed, because a second implementation of the cap is a second answer.
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
        // Said before it is appended: not a refusal and not a gate, but the price on the screen
        // while the operator can still act on it, rather than at the next compaction.
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

    /// Run the operator's own shell line (`!`), and append both halves as rows: the daemon runs it because a head has
    /// no exec host, no confinement and no standing environment, and that environment carries the socket `sudo` needs.
    /// Nothing gates it, and both rows go in one append, the result as a `ToolResult` that gets no `Speaker`.
    pub fn run_operator_shell(&mut self, line: &str, who: &str) -> Result<(), String> {
        let (call_id, call) = self.prepare_operator_shell(line)?;
        // A null sink, as the door's runs have, and watched for the reason
        // [`Self::unprompted_sink`] gives: a run moved to the background mid-flight settles as
        // `Backgrounded`, and the row it leaves says its completion will arrive on its own.
        let mut quiet = self.unprompted_sink();
        // `turn_id` empty on purpose, as the door's runs are: this call belongs to no
        // turn, and a head that saw a turn id would draw the row inside a turn that
        // did not propose it.
        let result = self.runtime.invoke_operator("", &call, &mut quiet);
        // The pieces, not the type: [`Self::settle_operator_shell`] takes what the split path
        // carries over the hub, and this path hands it the same shape.
        let spill = result
            .spill
            .as_ref()
            .map(|s| (s.full_bytes, s.hash.clone()));
        let outcome = result.outcome.clone();
        self.settle_operator_shell(line, who, &call_id, outcome, result.render(), spill)
    }

    /// Mint the operator's own call: the `bang-<n>` id and the `bash` call it names. The counter is this session's, so
    /// the id is minted here before the run is handed anywhere: what another thread needs is a `ToolCall` whose payload
    /// is the line minus its bang.
    pub fn prepare_operator_shell(&mut self, line: &str) -> Result<(String, ToolCall), String> {
        // A pasted block is not a command, and the daemon checks this itself: a `!` line reaches
        // here from any head that speaks the protocol, and a row of five lines whose newlines the
        // shell split would run each fragment as its own command.
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

    /// Append what the operator's run produced: the note and the two rows, one append. The half that needs the harness
    /// after the run, which is why it runs on the worker even when the run did not, since the transcript has one writer.
    pub fn settle_operator_shell(
        &mut self,
        line: &str,
        who: &str,
        call_id: &str,
        outcome: letibot_transcript::ToolOutcome,
        payload: String,
        spill: Option<(usize, String)>,
    ) -> Result<(), String> {
        // The command as the note names it, by the same validator the frame used. The `None` arm
        // is not a silent default: a line settled without a bang was still run as written, and the
        // note says so rather than inventing a command nobody typed.
        let command = letibot_sessionlog::operator_shell_command(line)
            .map(str::to_string)
            .unwrap_or_else(|| line.to_string());
        // What the model will read: the door's note makes the same disclosure for the same reason,
        // that the price is on the screen while the operator can still act on it.
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

    /// Write down the question the model asked and the answer a person gave: the transcript has one writer and it is
    /// the session. The result goes first and the person's half follows, since a `User` row in between closes the
    /// answering window. The answer's speaker is the operator's, so a yes through a card can be cited as a typed one.
    pub(crate) fn settle_asked(&mut self, result: &mut TranscriptItem) -> Vec<TranscriptItem> {
        let Some(asked) = self.asked.lock().unwrap_or_else(|e| e.into_inner()).take() else {
            return Vec::new();
        };
        // Only an answer the tool ACCEPTED: a refusal is `not_run`, and writing a person's row for
        // one would attribute to them a sentence the harness rejected.
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

    /// The runtime as a shareable handle, for the thread that runs the operator's command: one exec host per session
    /// is a fact, so the run goes through this runtime. The sink is the caller's and decides whether a `Backgrounded`
    /// outcome is watched, which is what makes the settlement reach a session and a bell ring.
    pub fn runtime_handle(&self) -> std::sync::Arc<ToolRuntime> {
        self.runtime.clone()
    }

    /// Attach the worker's serialization state: the same `Arc` the dispatch arm holds, so both
    /// places that could start an operator run read one fact. A setter rather than an argument
    /// because the harness can be built before the map that owns the state exists.
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

    /// The turn the operator's own `!` line starts: the sending half, so the two rows are a message and not silent
    /// context. Nothing is appended here, since the line and its output are already rows. Called only between turns,
    /// because mid-turn the line is taken at the round boundary and a second turn would be one command twice.
    pub fn run_after_operator_shell(&mut self) -> Result<Reply, HarnessError> {
        self.trail.begin_turn();
        let out = self.run_rounds();
        self.note_turn_ended(&out);
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
