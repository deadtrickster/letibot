//! The head side of the protocol: small on purpose, because a head's obligations are one
//! method each here and the frames carry the rest.

use std::io;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::mpsc::Sender;

use crate::protocol::{Ack, Caps, ClientFrame, PROTOCOL_VERSION, PeekShape, ServerFrame};
use crate::wire::{FrameReader, FrameWriter, WireError};

#[derive(Debug)]
pub enum ClientError {
    Io(io::Error),
    Wire(WireError),
    /// The daemon said goodbye before saying hello.
    Refused(String),
    /// The first frame was not `Hello`.
    Protocol(String),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::Io(e) => write!(f, "{e}"),
            ClientError::Wire(e) => write!(f, "{e}"),
            ClientError::Refused(r) => write!(f, "the daemon refused the attach: {r}"),
            ClientError::Protocol(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for ClientError {}

impl From<io::Error> for ClientError {
    fn from(e: io::Error) -> Self {
        ClientError::Io(e)
    }
}

impl From<WireError> for ClientError {
    fn from(e: WireError) -> Self {
        ClientError::Wire(e)
    }
}

/// A connected head.
pub struct HeadClient {
    writer: FrameWriter<UnixStream>,
    head_id: String,
    next_request: u64,
    /// The name this head attached under, kept so a frame that has to say WHO
    /// asked can take it from the attach rather than from a caller's guess.
    identity: String,
}

impl HeadClient {
    /// Connect and ATTACH. Returns the client, the `Hello`, and a reader that the
    /// caller drives on its own thread.
    pub fn attach(
        path: impl AsRef<Path>,
        session_id: &str,
        since_seq: u64,
        kind: &str,
        identity: &str,
        caps: Caps,
    ) -> Result<(HeadClient, ServerFrame, FrameReader<UnixStream>), ClientError> {
        let stream = UnixStream::connect(path.as_ref())?;
        let mut reader = FrameReader::new(stream.try_clone()?);
        let mut writer = FrameWriter::new(stream);
        writer.write(&ClientFrame::Attach {
            protocol_version: PROTOCOL_VERSION,
            session_id: session_id.to_string(),
            since_seq,
            kind: kind.to_string(),
            identity: identity.to_string(),
            caps,
        })?;
        let hello: ServerFrame = reader.read().map_err(|e| match e {
            // A first frame this build cannot read is a skew, and it is named as one: the
            // `Wire` error prints a decoder's complaint about a line with nothing in it about
            // two builds, and the pump says the same words, so both paths describe it alike.
            WireError::Malformed { line, detail } => {
                ClientError::Protocol(Unreadable { line, detail }.said())
            }
            other => ClientError::Wire(other),
        })?;
        match &hello {
            ServerFrame::Hello { head_id, .. } => {
                let head_id = head_id.clone();
                Ok((
                    HeadClient {
                        writer,
                        head_id,
                        next_request: 0,
                        identity: identity.to_string(),
                    },
                    hello,
                    reader,
                ))
            }
            ServerFrame::Bye { reason } => Err(ClientError::Refused(reason.clone())),
            other => Err(ClientError::Protocol(format!(
                "expected Hello, got {other:?}"
            ))),
        }
    }

    /// Connect and ATTACH without waiting for the `Hello`. `attach` blocks on an answer that
    /// carries the snapshot, and a caller that pumps its own socket must not block.
    pub fn start_attach(
        path: impl AsRef<Path>,
        session_id: &str,
        since_seq: u64,
        kind: &str,
        identity: &str,
        caps: Caps,
    ) -> Result<(HeadClient, FrameReader<UnixStream>), ClientError> {
        let stream = UnixStream::connect(path.as_ref())?;
        let reader = FrameReader::new(stream.try_clone()?);
        let mut writer = FrameWriter::new(stream);
        writer.write(&ClientFrame::Attach {
            protocol_version: PROTOCOL_VERSION,
            session_id: session_id.to_string(),
            since_seq,
            kind: kind.to_string(),
            identity: identity.to_string(),
            caps,
        })?;
        Ok((
            HeadClient {
                writer,
                // The daemon names the head in the `Hello`; until it arrives this
                // connection has no name, and `seated` is how it learns one.
                head_id: String::new(),
                next_request: 0,
                identity: identity.to_string(),
            },
            reader,
        ))
    }

    /// Take the head id from a `Hello` this client has read off its own reader: the pair to
    /// [`Self::start_attach`], and whichever loop reads that `Hello` must say the name back or
    /// every later `Ack` is unattributed.
    pub fn seated_by(&mut self, hello: &ServerFrame) {
        if let ServerFrame::Hello { head_id, .. } = hello {
            self.head_id = head_id.clone();
        }
    }

    /// This head's id - empty until a `Hello` has named it.
    pub fn head_id(&self) -> &str {
        &self.head_id
    }

    /// Send the read mark. Call this after the batch is on the screen.
    pub fn ack(&mut self, ack: Ack) -> Result<(), ClientError> {
        self.writer.write(&ClientFrame::Ack(ack))?;
        Ok(())
    }

    pub fn request_resync(&mut self) -> Result<(), ClientError> {
        self.writer.write(&ClientFrame::Resync)?;
        Ok(())
    }

    fn next_id(&mut self) -> String {
        self.next_request += 1;
        format!("{}-{}", self.head_id, self.next_request)
    }

    pub fn prompt(&mut self, expected_seq: u64, text: &str) -> Result<String, ClientError> {
        self.prompt_attached(expected_seq, text, Vec::new())
    }

    /// A prompt with files beside it: the attachments half of `ClientFrame::Prompt`. A second
    /// door rather than a changed signature, so no existing caller has to know.
    pub fn prompt_attached(
        &mut self,
        expected_seq: u64,
        text: &str,
        attachments: Vec<crate::protocol::Attachment>,
    ) -> Result<String, ClientError> {
        self.prompt_with_context(expected_seq, text, attachments, None)
    }

    /// A prompt with what the operator pointed at in an editor beside it: the third door, and the
    /// reason the other two are written in terms of it.
    pub fn prompt_with_context(
        &mut self,
        expected_seq: u64,
        text: &str,
        attachments: Vec<crate::protocol::Attachment>,
        context: Option<crate::protocol::EditorContext>,
    ) -> Result<String, ClientError> {
        let client_request_id = self.next_id();
        self.writer.write(&ClientFrame::Prompt {
            client_request_id: client_request_id.clone(),
            expected_seq,
            text: text.to_string(),
            attachments,
            context,
        })?;
        Ok(client_request_id)
    }

    /// Take back every prompt this head queued that the running turn has not consumed yet: the
    /// companion of the prompt door, and the head's own act.
    pub fn withdraw_prompts(&mut self, expected_seq: u64) -> Result<String, ClientError> {
        let client_request_id = self.next_id();
        self.writer.write(&ClientFrame::WithdrawPrompts {
            client_request_id: client_request_id.clone(),
            expected_seq,
        })?;
        Ok(client_request_id)
    }

    pub fn interrupt(&mut self, expected_seq: u64, reason: &str) -> Result<String, ClientError> {
        let client_request_id = self.next_id();
        self.writer.write(&ClientFrame::Interrupt {
            client_request_id: client_request_id.clone(),
            expected_seq,
            reason: reason.to_string(),
        })?;
        Ok(client_request_id)
    }

    /// The name this head attached under.
    pub fn identity(&self) -> &str {
        &self.identity
    }

    /// The socket this connection is on, for a caller that has to ask the kernel about the process
    /// at the other end.
    pub fn socket(&mut self) -> &UnixStream {
        self.writer.get_mut()
    }

    /// Ask the daemon to stop, not just this head. `who` rides along because the notice every
    /// other head gets has to say on whose word it stopped.
    pub fn stop(&mut self, expected_seq: u64, who: &str) -> Result<String, ClientError> {
        let client_request_id = self.next_id();
        self.writer.write(&ClientFrame::Stop {
            client_request_id: client_request_id.clone(),
            expected_seq,
            who: who.to_string(),
        })?;
        Ok(client_request_id)
    }

    /// Close this session here and let go of it: what a migration sends before it carries the rows
    /// to another store, and what a bridge's stop sends so a placement settles now. `reason` is the
    /// sentence the session's log carries, so the two callers cannot be read as each other.
    pub fn close_session(
        &mut self,
        expected_seq: u64,
        who: &str,
        reason: &str,
    ) -> Result<String, ClientError> {
        let client_request_id = self.next_id();
        self.writer.write(&ClientFrame::CloseSession {
            client_request_id: client_request_id.clone(),
            expected_seq,
            who: who.to_string(),
            reason: reason.to_string(),
        })?;
        Ok(client_request_id)
    }

    /// Ask the daemon to move the running command to the background (Ctrl+B).
    pub fn promote(&mut self, expected_seq: u64) -> Result<String, ClientError> {
        let client_request_id = self.next_id();
        self.writer.write(&ClientFrame::Promote {
            client_request_id: client_request_id.clone(),
            expected_seq,
        })?;
        Ok(client_request_id)
    }

    /// Ask for this session's todo list: the todos pane's bootstrap read.
    /// Answered off the queue, like a list.
    pub fn list_todos(&mut self) -> Result<String, ClientError> {
        let client_request_id = self.next_id();
        self.writer.write(&ClientFrame::ListTodos)?;
        Ok(client_request_id)
    }

    /// The operator's half of the todo board, replaced wholesale: a delta for a list of tens of
    /// items would be a second language to get wrong.
    pub fn set_operator_todos(
        &mut self,
        expected_seq: u64,
        items: Vec<crate::event::TodoEntry>,
        moved: Vec<crate::event::TodoState>,
    ) -> Result<String, ClientError> {
        let client_request_id = self.next_id();
        self.writer.write(&ClientFrame::SetOperatorTodos {
            client_request_id: client_request_id.clone(),
            expected_seq,
            items,
            moved,
        })?;
        Ok(client_request_id)
    }

    /// This session's background jobs, as the daemon's process table has them.
    /// The head renders what comes back; it does not decide what is in it.
    pub fn list_jobs(&mut self) -> Result<(), ClientError> {
        self.writer.write(&ClientFrame::ListJobs)?;
        Ok(())
    }

    /// Ask for the standing notes, one row each: the notes pane's bootstrap read. The corpus and
    /// the form each file takes are the harness's answer.
    pub fn list_notes(&mut self) -> Result<(), ClientError> {
        self.writer.write(&ClientFrame::ListNotes)?;
        Ok(())
    }

    /// Ask what this session can be handed: the `/skills` pane's bootstrap read.
    pub fn list_skills(&mut self) -> Result<(), ClientError> {
        self.writer.write(&ClientFrame::ListSkills)?;
        Ok(())
    }

    /// Hand this session one skill's body: `/skills NAME`, and the pane's Enter. The daemon writes
    /// the operator's message and runs the turn; `then` is the rest of the line.
    pub fn load_skill(
        &mut self,
        expected_seq: u64,
        name: &str,
        then: &str,
    ) -> Result<String, ClientError> {
        let client_request_id = self.next_id();
        self.writer.write(&ClientFrame::LoadSkill {
            client_request_id: client_request_id.clone(),
            expected_seq,
            name: name.to_string(),
            then: then.to_string(),
        })?;
        Ok(client_request_id)
    }

    /// Ask for the merge queue, whole: the pane's bootstrap read. The queue is the daemon's, so a
    /// head that drew its own version would draw a stale one; from then on a `QueueMoved` event
    /// says when to ask again.
    pub fn list_merge_queue(&mut self) -> Result<(), ClientError> {
        self.writer.write(&ClientFrame::ListMergeQueue)?;
        Ok(())
    }

    /// Ask the daemon's GitHub reviewer seat: `letibot attach github <scope>` sends
    /// `{"verb": "attach", "scope": …}` and the pane's read sends `{"verb": "list"}`. The answer
    /// arrives as `ServerFrame::Github` carrying the same id.
    pub fn github(&mut self, op: serde_json::Value) -> Result<String, ClientError> {
        let client_request_id = self.next_id();
        self.writer.write(&ClientFrame::Github {
            client_request_id: client_request_id.clone(),
            op,
        })?;
        Ok(client_request_id)
    }

    /// Send one chat request as the operator. The answer arrives as `ServerFrame::RoomChat`
    /// carrying the same id. A caller that must recognise its own answer passes the id it
    /// minted; `None` mints one here.
    pub fn room_chat(
        &mut self,
        id: Option<String>,
        op: serde_json::Value,
    ) -> Result<String, ClientError> {
        let client_request_id = id.unwrap_or_else(|| self.next_id());
        self.writer.write(&ClientFrame::RoomChat {
            client_request_id: client_request_id.clone(),
            op,
        })?;
        Ok(client_request_id)
    }

    /// Ask the daemon to run a plugin pane's look now, because a head just opened it: the answer
    /// arrives as a `PluginPane` event rather than a reply.
    pub fn pane_open(&mut self, pane: &str, w: u32, h: u32) -> Result<(), ClientError> {
        self.writer.write(&ClientFrame::PaneOpen {
            pane: pane.to_string(),
            w,
            h,
        })?;
        Ok(())
    }

    /// Tell the daemon a plugin pane is off the screen, so it stops refreshing it. The opposite of
    /// `pane_open`, and quiet: nothing is published in answer, because the head that closed the pane
    /// is the one that says so.
    pub fn pane_close(&mut self, pane: &str) -> Result<(), ClientError> {
        self.writer.write(&ClientFrame::PaneClose {
            pane: pane.to_string(),
        })?;
        Ok(())
    }

    /// One key of one stop back to the pane that drew it: the `event` word its action named, or
    /// `"look"`. Answered like an open, with the new view as an event on the log. `say` never
    /// travels here; see [`ClientFrame::PaneAction`].
    pub fn pane_action(&mut self, pane: &str, item: &str, action: &str) -> Result<(), ClientError> {
        self.writer.write(&ClientFrame::PaneAction {
            pane: pane.to_string(),
            item: item.to_string(),
            action: action.to_string(),
        })?;
        Ok(())
    }

    /// Ask the daemon to compact this session: one summary turn over the history
    /// as it stands, then the history is replaced by that summary through a
    /// transcript fork. Queued behind a running turn, like a prompt.
    pub fn compact(&mut self, expected_seq: u64) -> Result<String, ClientError> {
        let client_request_id = self.next_id();
        self.writer.write(&ClientFrame::CompactSession {
            client_request_id: client_request_id.clone(),
            expected_seq,
        })?;
        Ok(client_request_id)
    }

    /// Ask the daemon to rebuild this conversation's prompt from the tools it seats now, forking
    /// onto it; queued like a compaction, because it is one. `summarise` replaces the conversation
    /// with a summary, and `false` changes message zero and carries every item across.
    pub fn reseat(&mut self, expected_seq: u64, summarise: bool) -> Result<String, ClientError> {
        let client_request_id = self.next_id();
        self.writer.write(&ClientFrame::ReseatSession {
            client_request_id: client_request_id.clone(),
            expected_seq,
            summarise,
        })?;
        Ok(client_request_id)
    }

    /// Move this session's project to a named point: the daemon persists it in the mode store, so
    /// it applies without a restart.
    pub fn set_mode(
        &mut self,
        expected_seq: u64,
        name: &str,
        consented: bool,
    ) -> Result<String, ClientError> {
        let client_request_id = self.next_id();
        self.writer.write(&ClientFrame::Mode {
            client_request_id: client_request_id.clone(),
            expected_seq,
            name: name.to_string(),
            consented,
        })?;
        Ok(client_request_id)
    }

    /// Ask a PERSON to confirm the point that needs one. Not `set_mode` with `consented: true`:
    /// a head that is a program cannot consent on a person's behalf.
    pub fn mode_confirm(&mut self, expected_seq: u64, name: &str) -> Result<String, ClientError> {
        let client_request_id = self.next_id();
        self.writer.write(&ClientFrame::ModeConfirm {
            client_request_id: client_request_id.clone(),
            expected_seq,
            name: name.to_string(),
        })?;
        Ok(client_request_id)
    }

    /// A person's answer to somebody else's request for a confirmation. `req_id` is the one the
    /// daemon sent, so an answer cannot land on the wrong card.
    pub fn mode_consent(
        &mut self,
        expected_seq: u64,
        req_id: &str,
        yes: bool,
    ) -> Result<String, ClientError> {
        let client_request_id = self.next_id();
        self.writer.write(&ClientFrame::ModeConsent {
            client_request_id: client_request_id.clone(),
            expected_seq,
            req_id: req_id.to_string(),
            yes,
        })?;
        Ok(client_request_id)
    }

    /// A slash command for the daemon, as typed minus the `/`.
    pub fn slash(&mut self, expected_seq: u64, line: &str) -> Result<String, ClientError> {
        let client_request_id = self.next_id();
        self.writer.write(&ClientFrame::Slash {
            client_request_id: client_request_id.clone(),
            expected_seq,
            line: line.to_string(),
        })?;
        Ok(client_request_id)
    }

    /// An operator's mark: `polarity` is `good`, `bad` or `note`, `at_ms` the press time on this
    /// head's clock, `item_id` a row read back or `None` for now.
    pub fn feedback(
        &mut self,
        at_ms: u64,
        polarity: &str,
        note: &str,
        item_id: Option<&str>,
    ) -> Result<(), ClientError> {
        self.writer.write(&ClientFrame::Feedback {
            at_ms,
            polarity: polarity.to_string(),
            note: note.to_string(),
            item_id: item_id.map(str::to_string),
        })?;
        Ok(())
    }

    /// Ask for the bytes that justified one decision: a locator, not a payload. Answered with
    /// `ServerFrame::Diagnostic` on the same receive loop as everything else, because this returns
    /// nothing and the answer is a frame.
    pub fn fetch_diagnostic(
        &mut self,
        request_id: &str,
        kind: crate::protocol::DiagnosticKind,
    ) -> Result<(), ClientError> {
        self.writer.write(&ClientFrame::FetchDiagnostic {
            request_id: request_id.to_string(),
            kind,
        })?;
        Ok(())
    }

    /// Ask the daemon to admit an operator's own call: the first of the two frames. A head must
    /// not run the call before the admission, which is what puts it in the record.
    pub fn operator_call(
        &mut self,
        expected_seq: u64,
        call_id: &str,
        name: &str,
        arguments: &str,
        execute: bool,
    ) -> Result<String, ClientError> {
        let client_request_id = self.next_id();
        self.writer.write(&ClientFrame::OperatorCall {
            client_request_id: client_request_id.clone(),
            expected_seq,
            call_id: call_id.to_string(),
            name: name.to_string(),
            arguments: arguments.to_string(),
            execute,
        })?;
        Ok(client_request_id)
    }

    /// Hand back what the call produced: the second frame.
    pub fn operator_result(
        &mut self,
        call_id: &str,
        outcome: letibot_transcript::ToolOutcome,
        payload: &str,
    ) -> Result<(), ClientError> {
        self.writer.write(&ClientFrame::OperatorResult {
            call_id: call_id.to_string(),
            outcome,
            payload: payload.to_string(),
        })?;
        Ok(())
    }

    /// The operator's own shell line: a `!` command for the daemon to run. `line` is verbatim,
    /// `!` first, because the daemon parses it as the operator typed it.
    pub fn operator_shell(&mut self, expected_seq: u64, line: &str) -> Result<String, ClientError> {
        let client_request_id = self.next_id();
        self.writer.write(&ClientFrame::OperatorShell {
            client_request_id: client_request_id.clone(),
            expected_seq,
            line: line.to_string(),
        })?;
        Ok(client_request_id)
    }

    /// Open a pane and run a screen program in it: `!term <command>`. `cols` and `rows` are the
    /// conversation's own rectangle, because the pane is drawn there.
    pub fn term_open(&mut self, line: &str, cols: usize, rows: usize) -> Result<(), ClientError> {
        self.writer.write(&ClientFrame::TermOpen {
            line: line.to_string(),
            cols,
            rows,
        })?;
        Ok(())
    }

    /// The operator's keys, verbatim, to the pane's program: bytes and not a keycode, because
    /// the program is a terminal program.
    pub fn term_input(&mut self, pane: u32, bytes: &[u8]) -> Result<(), ClientError> {
        self.writer.write(&ClientFrame::TermInput {
            pane,
            bytes: bytes.to_vec(),
        })?;
        Ok(())
    }

    /// The pane's rectangle moved. The head's fact: the daemon has no screen, so this is the only
    /// way the program is told the size it is being drawn at.
    pub fn term_resize(&mut self, pane: u32, cols: usize, rows: usize) -> Result<(), ClientError> {
        self.writer
            .write(&ClientFrame::TermResize { pane, cols, rows })?;
        Ok(())
    }

    /// End the pane: the daemon kills the program and everything it started, and the ending comes
    /// back as a `TermClosed` event.
    pub fn term_close(&mut self, pane: u32) -> Result<(), ClientError> {
        self.writer.write(&ClientFrame::TermClose { pane })?;
        Ok(())
    }

    /// Ask what this session's pane is running; the answer is a `TermStatus` frame, read off the
    /// pump like every other answer.
    pub fn term_status(&mut self) -> Result<(), ClientError> {
        self.writer.write(&ClientFrame::TermStatus)?;
        Ok(())
    }

    /// Ask the model to propose `!` completions for a prefix: the smart half of completion, for
    /// what the head's own history cannot answer.
    pub fn suggest_shell(
        &mut self,
        expected_seq: u64,
        client_request_id: &str,
        prefix: &str,
    ) -> Result<(), ClientError> {
        self.writer.write(&ClientFrame::SuggestShell {
            client_request_id: client_request_id.to_string(),
            expected_seq,
            prefix: prefix.to_string(),
        })?;
        Ok(())
    }

    /// This head's rendered rows, answering a `ScreenRequested`.
    pub fn screen(
        &mut self,
        req_id: &str,
        cols: usize,
        rows_n: usize,
        rows: Vec<String>,
    ) -> Result<(), ClientError> {
        self.writer.write(&ClientFrame::Screen {
            req_id: req_id.to_string(),
            cols,
            rows_n,
            rows,
        })?;
        Ok(())
    }

    /// A head's answer to a `SecretRequested`: the password, or `None` to refuse.
    /// Not a command: no request id comes back, nothing is announced.
    pub fn secret(&mut self, req_id: &str, secret: Option<String>) -> Result<(), ClientError> {
        self.writer.write(&ClientFrame::Secret {
            req_id: req_id.to_string(),
            secret,
        })?;
        Ok(())
    }

    /// A head's answer to a `PromptRequested`: the line the person typed, for the stdin of the
    /// operator's own running command.
    pub fn prompt_answer(&mut self, req_id: &str, line: &str) -> Result<(), ClientError> {
        self.writer.write(&ClientFrame::PromptAnswer {
            req_id: req_id.to_string(),
            line: line.to_string(),
        })?;
        Ok(())
    }

    /// One line to this session's own running command, on demand: the manual way in. No request
    /// id and no job handle: the session has one command.
    pub fn send_line(&mut self, line: &str) -> Result<(), ClientError> {
        self.writer.write(&ClientFrame::SendLine {
            line: line.to_string(),
        })?;
        Ok(())
    }

    /// The `askpass` helper's one frame: `sudo` wants a password for `command`.
    /// The answer arrives on the reader as [`ServerFrame::Secret`].
    pub fn askpass(&mut self, prompt: &str, command: &str) -> Result<(), ClientError> {
        self.writer.write(&ClientFrame::Askpass {
            prompt: prompt.to_string(),
            command: command.to_string(),
        })?;
        Ok(())
    }

    /// Grant or deny an open permission, by option id.
    pub fn answer(&mut self, req_id: &str, option_id: &str) -> Result<String, ClientError> {
        self.answer_with(req_id, option_id, None, None)
    }

    /// The same, with the operator's own glob for an always allow. Separate rather than a fourth
    /// argument on the plain answer, so a caller that has no glob says so.
    pub fn answer_with(
        &mut self,
        req_id: &str,
        option_id: &str,
        pattern: Option<&str>,
        note: Option<&str>,
    ) -> Result<String, ClientError> {
        let client_request_id = self.next_id();
        self.writer.write(&ClientFrame::Answer {
            client_request_id: client_request_id.clone(),
            req_id: req_id.to_string(),
            option_id: option_id.to_string(),
            pattern: pattern.map(str::to_string),
            note: note.map(str::to_string),
        })?;
        Ok(client_request_id)
    }

    /// Answer an open question with what the person actually did: an option, an option and a
    /// note, a typed reply, or an abstention.
    pub fn answer_question(
        &mut self,
        req_id: &str,
        answer: crate::question::QuestionAnswer,
    ) -> Result<String, ClientError> {
        let client_request_id = self.next_id();
        self.writer.write(&ClientFrame::AnswerQuestion {
            client_request_id: client_request_id.clone(),
            req_id: req_id.to_string(),
            answer,
        })?;
        Ok(client_request_id)
    }

    /// Answer the question a todo row waits on: the row's words, the question being answered, and
    /// what the person did. See [`ClientFrame::AnswerTodo`].
    pub fn answer_todo(
        &mut self,
        content: &str,
        question: &str,
        answer: crate::question::QuestionAnswer,
    ) -> Result<String, ClientError> {
        let client_request_id = self.next_id();
        self.writer.write(&ClientFrame::AnswerTodo {
            client_request_id: client_request_id.clone(),
            content: content.to_string(),
            question: question.to_string(),
            answer,
        })?;
        Ok(client_request_id)
    }

    /// Ask what sessions this daemon holds. Answered with a `Sessions` frame on the same stream
    /// the events arrive on.
    pub fn list_sessions(&mut self) -> Result<(), ClientError> {
        self.writer.write(&ClientFrame::ListSessions)?;
        Ok(())
    }

    /// `workspace` is the tree the new session is about - the head's own working
    /// directory. Empty leaves it to the daemon, which is what a head that has no
    /// opinion sends.
    pub fn new_session(&mut self, title: &str, workspace: &str) -> Result<String, ClientError> {
        let client_request_id = self.next_id();
        self.writer.write(&ClientFrame::NewSession {
            client_request_id: client_request_id.clone(),
            title: title.to_string(),
            workspace: workspace.to_string(),
        })?;
        Ok(client_request_id)
    }

    /// Ask the daemon to bring a stored session in. Answered with `Sessions` carrying it as
    /// created, exactly as a new session is.
    pub fn resume_session(&mut self, session_id: &str) -> Result<String, ClientError> {
        let client_request_id = self.next_id();
        self.writer.write(&ClientFrame::ResumeSession {
            client_request_id: client_request_id.clone(),
            session_id: session_id.to_string(),
        })?;
        Ok(client_request_id)
    }

    /// Name a session, or clear its name with an empty title.
    pub fn rename_session(&mut self, session_id: &str, title: &str) -> Result<String, ClientError> {
        let client_request_id = self.next_id();
        self.writer.write(&ClientFrame::RenameSession {
            client_request_id: client_request_id.clone(),
            session_id: session_id.to_string(),
            title: title.to_string(),
        })?;
        Ok(client_request_id)
    }

    /// Put a session out of sight, or bring it back. The answer is a `Sessions` frame, the same one
    /// a rename gets, so a picker redraws from the daemon's own roster.
    pub fn archive_session(
        &mut self,
        session_id: &str,
        reason: &str,
    ) -> Result<String, ClientError> {
        let client_request_id = self.next_id();
        self.writer.write(&ClientFrame::ArchiveSession {
            client_request_id: client_request_id.clone(),
            session_id: session_id.to_string(),
            reason: reason.to_string(),
        })?;
        Ok(client_request_id)
    }

    /// [`Client::archive_session`]'s other direction.
    pub fn unarchive_session(&mut self, session_id: &str) -> Result<String, ClientError> {
        let client_request_id = self.next_id();
        self.writer.write(&ClientFrame::UnarchiveSession {
            client_request_id: client_request_id.clone(),
            session_id: session_id.to_string(),
        })?;
        Ok(client_request_id)
    }

    /// Move this connection to another session. The answer is a second `Hello`, applied exactly
    /// as the first was.
    pub fn switch(&mut self, session_id: &str, since_seq: u64) -> Result<(), ClientError> {
        self.writer.write(&ClientFrame::Switch {
            session_id: session_id.to_string(),
            since_seq,
        })?;
        Ok(())
    }

    /// Read another session's scrollback, or its rows, without leaving this one: answered with a
    /// `Peeked` frame and nothing else moves.
    pub fn peek(&mut self, session_id: &str, shape: PeekShape) -> Result<(), ClientError> {
        self.writer.write(&ClientFrame::Peek {
            session_id: session_id.to_string(),
            shape,
        })?;
        Ok(())
    }

    /// Ask for a page of whole rows by ordinal, without leaving this session: answered with a
    /// `RowsFetched` frame on the pump. The head holds a window, so this is how a reader scrolling
    /// above it reaches the conversation behind it.
    pub fn fetch_rows(
        &mut self,
        session_id: &str,
        from: usize,
        count: usize,
    ) -> Result<(), ClientError> {
        self.writer.write(&ClientFrame::FetchRows {
            session_id: session_id.to_string(),
            from,
            count,
        })?;
        Ok(())
    }

    /// Read a window of one row's body without leaving this session: answered with a `RowFetched`
    /// frame on the pump.
    pub fn fetch_row(
        &mut self,
        session_id: &str,
        row: usize,
        at: usize,
        len: usize,
    ) -> Result<(), ClientError> {
        self.writer.write(&ClientFrame::FetchRow {
            session_id: session_id.to_string(),
            row,
            at,
            len,
        })?;
        Ok(())
    }

    /// Ask for a window of one background job's output. Answered by an
    /// `SessionEvent::JobOutput` on the session log, like every other verb.
    pub fn read_job_output(&mut self, job: &str, offset: u64) -> Result<String, ClientError> {
        let client_request_id = self.next_id();
        self.writer.write(&ClientFrame::ReadJobOutput {
            client_request_id: client_request_id.clone(),
            job: job.to_string(),
            offset,
        })?;
        Ok(client_request_id)
    }

    /// Ask for the settings this session runs under; answered with
    /// `ServerFrame::Settings`.
    pub fn settings(&mut self) -> Result<(), ClientError> {
        self.writer.write(&ClientFrame::Settings)?;
        Ok(())
    }

    /// Take the head id from a `Hello` the caller pumped off the socket. Not folded into `switch`,
    /// because the caller is the one that read the frame.
    pub fn seated(&mut self, head_id: &str) {
        self.head_id = head_id.to_string();
    }

    pub fn detach(&mut self) -> Result<(), ClientError> {
        self.writer.write(&ClientFrame::Detach)?;
        Ok(())
    }

    /// Make the reader's `read` return at once: a reader blocked in `read` is woken by data, by
    /// EOF or by nothing at all, so a shutdown needs this.
    pub fn shut_down(&mut self) -> Result<(), ClientError> {
        self.writer.get_mut().shutdown(std::net::Shutdown::Both)?;
        Ok(())
    }
}

/// What the pump hands a head: a frame, or a line this build could not read. `ServerFrame` and
/// `SessionEvent` share one stream, so a reader has to say which it got.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)] // the frame is the protocol's whole vocabulary; boxing one variant taxes every receive
pub enum Inbound {
    Frame(ServerFrame),
    /// One line of the stream that did not parse, kept so the head can say what it
    /// was. The connection is still up.
    Unreadable(Unreadable),
}

impl Inbound {
    /// The frame, or a panic naming the line: for a caller that owns both ends of the socket, an
    /// unreadable line is a bug in the test.
    pub fn frame(self) -> ServerFrame {
        match self {
            Inbound::Frame(f) => f,
            Inbound::Unreadable(u) => panic!("unreadable frame: {u:?}"),
        }
    }
}

/// One line of the stream that did not parse, with the line: the line is the whole reason this
/// type exists, because a version skew is diagnosed from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unreadable {
    /// The line, as it came off the wire. Kept whole; truncated at the point of
    /// display, where the width is known.
    pub line: String,
    /// serde's own complaint - `unknown variant \`job_output\`, expected one of ...` -
    /// which is what names the skew when the tag is one this build has never heard
    /// of.
    pub detail: String,
}

impl Unreadable {
    /// The sentence the operator reads: what it was, why it is almost always a daemon newer than
    /// this head, and what to do about it.
    pub fn said(&self) -> String {
        // Truncated here rather than kept short: the line is the evidence and the
        // first 200 columns of it are what identifies the frame.
        let shown: String = self.line.chars().take(200).collect();
        let ellipsis = if self.line.chars().count() > 200 {
            "…"
        } else {
            ""
        };
        format!(
            "the daemon sent a frame this head cannot read ({detail}). This head speaks \
             protocol {PROTOCOL_VERSION}; a daemon built against a newer one will do \
             this on the first frame the two do not share, and it is almost always \
             that rather than a corrupt stream. The connection is still up. The line \
             was: {shown}{ellipsis}",
            detail = self.detail,
        )
    }
}
/// Drive a reader on this thread, pushing everything it reads into `tx`. Returns when the daemon
/// closes or the receiver is dropped.
pub fn pump(mut reader: FrameReader<UnixStream>, tx: Sender<Inbound>) {
    loop {
        match reader.read::<ServerFrame>() {
            Ok(f) => {
                let bye = matches!(f, ServerFrame::Bye { .. });
                if tx.send(Inbound::Frame(f)).is_err() || bye {
                    return;
                }
            }
            // One unreadable line is a fact about two builds, not about the stream: the next
            // line is very likely one this head reads, and a `Malformed` costs the reader
            // nothing because the line has already been consumed.
            Err(WireError::Malformed { line, detail }) => {
                if tx
                    .send(Inbound::Unreadable(Unreadable { line, detail }))
                    .is_err()
                {
                    return;
                }
            }
            // The stream is over. `Eof` is detach and says nothing; an io error is
            // the socket, not a frame, and there is nothing left to read either way.
            Err(WireError::Eof) | Err(WireError::Io(_)) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// One line this build cannot read does not end the connection, and it keeps the line: a
    /// daemon one version ahead must not look like a dead socket.
    #[test]
    fn an_unreadable_line_is_handed_over_and_the_stream_carries_on() {
        let (server, client) = UnixStream::pair().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let pumping = std::thread::spawn(move || pump(FrameReader::new(client), tx));
        {
            let mut w = server;
            // A frame from the future, then a frame this build knows, then close.
            w.write_all(b"{\"frame\":\"from_the_future\",\"payload\":1}\n")
                .unwrap();
            let mut live = FrameWriter::new(w);
            live.write(&ServerFrame::Bye {
                reason: "still here".into(),
            })
            .unwrap();
        }
        pumping.join().unwrap();

        let first = rx.recv().expect("the unreadable line is handed over");
        let Inbound::Unreadable(u) = first else {
            panic!("the first message must be the complaint: {first:?}");
        };
        assert!(u.line.contains("from_the_future"), "{u:?}");
        assert!(u.detail.contains("from_the_future"), "{u:?}");
        // The line was consumed and the pump read on: the frame after it arrived.
        let second = rx.recv().expect("the next line is read");
        assert!(
            matches!(
                second,
                Inbound::Frame(ServerFrame::Bye { ref reason }) if reason == "still here"
            ),
            "{second:?}"
        );
    }

    /// The sentence names the skew: what arrived, that it is almost always a newer daemon, and
    /// that the head is still attached - a head that exits and a head that shrugs are the two
    /// failures this middle path exists between.
    #[test]
    fn the_complaint_names_the_frame_the_skew_and_the_version() {
        let u = Unreadable {
            line: r#"{"frame":"peeked_v2","rows":[]}"#.into(),
            detail: "unknown variant `peeked_v2`, expected one of `hello`, `bye`".into(),
        };
        let said = u.said();
        assert!(said.contains("peeked_v2"), "{said}");
        assert!(said.contains("newer"), "{said}");
        assert!(
            said.contains(&PROTOCOL_VERSION.to_string()),
            "the version it speaks, so the two can be compared: {said}"
        );
        assert!(said.contains("still up"), "{said}");
        // A long line is truncated with the ellipsis, so the sentence cannot be a
        // megabyte of somebody else's JSON.
        let long = Unreadable {
            line: "x".repeat(5_000),
            detail: "d".into(),
        }
        .said();
        assert!(long.chars().count() < 500, "{}", long.len());
        assert!(long.ends_with('…'), "{long}");
    }
}
