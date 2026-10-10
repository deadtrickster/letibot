//! The head side of the protocol.
//!
//! Small on purpose. A head's obligations are exactly three, and each is one method
//! here so that a second head (W8's remote, W12's flowy connector, the ACP adapter)
//! cannot quietly skip one:
//!
//! 1. ATTACH with a `since_seq`, and accept a `Resync` as a normal answer.
//! 2. **Ack after rendering**, with `seq` from the batch and both counts.
//! 3. Send `expected_seq` and a `client_request_id` on every mutating command.

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
            // **A first frame this build cannot read is a skew, and it is named as
            // one.** The alternative was `ClientError::Wire`, which prints
            // `malformed frame (unknown variant \`peeked_v2\`): {"frame":…}` — a
            // decoder's complaint about a line, with nothing in it about two builds.
            // Same words the pump's path says, so a head that fails at the handshake
            // and one that survives a skew later describe the same thing the same way.
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

    /// Connect and ATTACH **without waiting for the `Hello`**.
    ///
    /// [`Self::attach`] blocks on the answer, and that answer **carries the whole
    /// snapshot** — so on a session of thousands of rows a head spends a real wait
    /// with nothing on the screen and nothing to say, which is what the walking cat
    /// exists for. This is the same handshake with the blocking part left to the
    /// caller: it connects, sends `Attach`, and hands back the reader so the caller
    /// can drive it (a thread, or its own loop).
    ///
    /// The `Hello` arrives on the returned reader like any other frame, so a caller
    /// that wants the blocking form back writes `reader.read()` — which is all
    /// [`Self::attach`] does after this.
    pub fn start_attach(
        path: impl AsRef<Path>,
        session_id: &str,
        since_seq: u64,
        kind: &str,
        identity: &str,
        caps: Caps,
    ) -> Result<(HeadClient, FrameReader<UnixStream>), ClientError> {
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

    /// Take the head id from a `Hello` this client has read off its own reader.
    ///
    /// The pair to [`Self::start_attach`]: that sends the `Attach` and returns, and
    /// whichever loop reads the `Hello` has to say the name back or every later
    /// `Ack` is unattributed.
    pub fn seated_by(&mut self, hello: &ServerFrame) {
        if let ServerFrame::Hello { head_id, .. } = hello {
            self.head_id = head_id.clone();
        }
    }

    /// Consume a `Hello` from a reader this client started, blocking for it — the
    /// second half of [`Self::attach`], for a caller that has already read frames
    /// off the reader and wants to stop doing so by hand.
    pub fn head_id(&self) -> &str {
        &self.head_id
    }

    /// Send the read mark. **Call this after the batch is on the screen.**
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
        let client_request_id = self.next_id();
        self.writer.write(&ClientFrame::Prompt {
            client_request_id: client_request_id.clone(),
            expected_seq,
            text: text.to_string(),
        })?;
        Ok(client_request_id)
    }

    /// Take back every prompt this head queued that the running turn has not
    /// consumed yet. The companion of [`Self::prompt`] for the recall-to-edit
    /// flow: the head pulls the queued line into its composer and sends this,
    /// so the edited resend replaces the original instead of stacking onto it.
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

    /// **The socket this connection is on**, for a caller that has to ask the kernel a
    /// question about the process at the other end of it.
    ///
    /// Added for R30: a head that has asked the daemon to stop reports *which* process it
    /// was talking to, and `SO_PEERCRED` answers that for this very socket — where a pid
    /// looked up in a file could belong to a daemon that has already gone. The libc call
    /// lives in the caller (`letibot-tui`'s `peer_pid`) because this crate does not depend
    /// on `libc` and, as `server.rs` says in its own comment, should not start to.
    pub fn socket(&mut self) -> &UnixStream {
        self.writer.get_mut()
    }

    /// **Ask the daemon to stop**, not just this head.
    ///
    /// `who` is the identity this head attached under; it rides along because
    /// the notice every other head gets names the asker, and a daemon that
    /// said only "stopping" would leave a shared session guessing.
    pub fn stop(&mut self, expected_seq: u64, who: &str) -> Result<String, ClientError> {
        let client_request_id = self.next_id();
        self.writer.write(&ClientFrame::Stop {
            client_request_id: client_request_id.clone(),
            expected_seq,
            who: who.to_string(),
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

    /// Ask for this session's todo list — the todos pane's bootstrap read.
    /// Answered off the queue, like a list.
    pub fn list_todos(&mut self) -> Result<String, ClientError> {
        let client_request_id = self.next_id();
        self.writer.write(&ClientFrame::ListTodos)?;
        Ok(client_request_id)
    }

    /// **The operator's half of the todo board, replaced wholesale.**
    ///
    /// A head owns these rows — they are its own list — so it sends the WHOLE thing on every
    /// change rather than a delta: a delta protocol for a list of tens of items would be a second
    /// source of truth about them, which is the mistake this tree keeps deleting.
    ///
    /// **And the head is the only thing that can send it.** The daemon stores the rows, serves
    /// them to every head and hands them to the model as part of one union — but it cannot invent
    /// one, because the words are the operator's. Every row here carries
    /// [`letibot_sessionlog::event::TodoBy::Operator`], which is what makes the daemon file it in
    /// the operator's half instead of the model's.
    ///
    /// **`moved` is the other half of one act**: the state the operator asked for on rows that are
    /// NOT theirs — a row the model wrote, set aside with `/todo postpone <its words>` — named by
    /// content because the board has no other key. It rides this frame rather than a second one
    /// because both are *the board as the operator wants it*, and two frames for one act is how a
    /// head's copy and the store come to disagree.
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

    /// **Ask for the standing notes, one row each.** The pane's bootstrap read, exactly as
    /// [`Client::list_jobs`] is the jobs pane's: the corpus and the form each file has are the
    /// daemon's, decided with the session's own token counter, and a head that drew its own
    /// version would draw a second opinion about what the model was given.
    ///
    /// Asked on every pane-open rather than held: the notes are the operator's own files and
    /// can be written between two opens, and this is a read a pane can afford.
    pub fn list_notes(&mut self) -> Result<(), ClientError> {
        self.writer.write(&ClientFrame::ListNotes)?;
        Ok(())
    }

    /// **Ask for the merge queue, whole.** The pane's bootstrap read, exactly as
    /// [`Client::list_jobs`] is the jobs pane's: the queue is the daemon's and a head that drew
    /// its own version would draw a stale one. From then on the `MergeEntryAdded` and
    /// `MergeEntryMoved` events carry every change.
    pub fn list_merge_queue(&mut self) -> Result<(), ClientError> {
        self.writer.write(&ClientFrame::ListMergeQueue)?;
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

    /// Ask the daemon to rebuild this conversation's prompt from the tools it seats
    /// now, forking onto it. Queued like a compaction, because it is one.
    /// `summarise` replaces the conversation with a summary. The default —
    /// `false` — changes message zero and carries every item across.
    pub fn reseat(&mut self, expected_seq: u64, summarise: bool) -> Result<String, ClientError> {
        let client_request_id = self.next_id();
        self.writer.write(&ClientFrame::ReseatSession {
            client_request_id: client_request_id.clone(),
            expected_seq,
            summarise,
        })?;
        Ok(client_request_id)
    }

    /// Move this session's project to a named point. The daemon persists it in the
    /// mode store, so it applies without a daemon restart (D13).
    /// `consented` says the operator answered the unconfined-`allow-all`
    /// confirmation with yes. It is meaningless at every other point and is read
    /// only where `Mode::ALLOW_ALL_HERE` is selected.
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

    /// **Ask for the bytes that justified one decision** — R11's locator, leticl's ask.
    ///
    /// A locator and not a payload: the head names one decision and one half of its exchange
    /// and the daemon answers with those bytes or with *not recorded*. Answered with
    /// `ServerFrame::Diagnostic`, which a head reads from the same receive loop it reads
    /// everything else on — this returns nothing, the answer is a frame.
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

    /// **Ask the daemon to admit an operator's own call** — R24 part two, decision 4, the
    /// first of the two frames.
    ///
    /// The daemon answers [`crate::ServerFrame::Rejected`] when the name is not one of
    /// [`crate::HEAD_RUN_TOOLS`], and otherwise queues the admission and publishes
    /// [`crate::SessionEvent::OperatorCallAllowed`] once it is recorded. **A head must not run
    /// the call until that event arrives**: an `Accepted` reply means *queued*, and the whole
    /// point of the pair is that the admission is on the record before anything happens.
    ///
    /// `execute` says *who runs it* (R31). `true` is the daemon, which is what a head with no
    /// HTTP client and no tool runtime needs; `false` is the head, which is what a head that
    /// has one does. The admission is identical either way. See
    /// [`ClientFrame::OperatorCall::execute`] for why both exist and why the daemon's is the
    /// one that keeps the payload the same program's.
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

    /// **Hand back what the call produced** — the second frame.
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

    /// **The operator's own shell line** — a `!` command, for the daemon to run.
    ///
    /// `line` is the submitted line verbatim, `!` first; the daemon strips the bang and runs the
    /// rest through the session's `bash` execution path, appends the operator's line and the
    /// result as transcript rows, and never consults the gate. See
    /// [`ClientFrame::OperatorShell`] for why this is a frame of its own rather than the door.
    ///
    /// Answered `Accepted` (queued — mid-turn it runs at the next round boundary) or `Rejected`
    /// when the line is not a `!` line at all.
    pub fn operator_shell(&mut self, expected_seq: u64, line: &str) -> Result<String, ClientError> {
        let client_request_id = self.next_id();
        self.writer.write(&ClientFrame::OperatorShell {
            client_request_id: client_request_id.clone(),
            expected_seq,
            line: line.to_string(),
        })?;
        Ok(client_request_id)
    }

    /// **Open a pane and run a screen program in it** — `!term <command>`.
    ///
    /// `line` is the submitted line verbatim, verb included; the daemon strips `!term` and
    /// hands the rest to `/bin/sh -c` on a pty the daemon owns. `cols` and `rows` are **the
    /// conversation's rectangle** — the head is the half that knows it — and they reach the
    /// pty as its `winsize` before the program's first byte.
    ///
    /// **Nothing is answered on this socket but the pane itself.** There is no
    /// `Accepted`/`Rejected` for this frame: the daemon's first word is
    /// [`crate::protocol::ServerFrame::TermOutput`] carrying what the program drew, and a
    /// pane that never started is [`crate::protocol::ServerFrame::TermEnded`] with the
    /// sentence saying why.
    pub fn term_open(&mut self, line: &str, cols: usize, rows: usize) -> Result<(), ClientError> {
        self.writer.write(&ClientFrame::TermOpen {
            line: line.to_string(),
            cols,
            rows,
        })?;
        Ok(())
    }

    /// **The operator's keys, verbatim**, to the pane's program.
    ///
    /// Bytes and not a keycode — see [`ClientFrame::TermInput`] for why the pane's keyboard
    /// cannot be the head's own decoder. Nothing is returned: a keystroke has no answer, and
    /// the pane's next `TermOutput` is the program's reply to whatever it did with it.
    pub fn term_input(&mut self, bytes: &[u8]) -> Result<(), ClientError> {
        self.writer.write(&ClientFrame::TermInput {
            bytes: bytes.to_vec(),
        })?;
        Ok(())
    }

    /// **The pane's rectangle moved.** The head's fact: the daemon has no screen, so this is
    /// the only way the program is told the size it is being drawn at.
    pub fn term_resize(&mut self, cols: usize, rows: usize) -> Result<(), ClientError> {
        self.writer.write(&ClientFrame::TermResize { cols, rows })?;
        Ok(())
    }

    /// **End the pane.** The operator's deliberate act, and the frame the daemon ends the
    /// pane's scope on: it kills the program and everything it started, and the ending comes
    /// back as [`crate::protocol::ServerFrame::TermEnded`]. Quiet when there is no pane.
    ///
    /// **Not the way out.** `ctrl-\` detaches and sends nothing at all — see
    /// [`ClientFrame::TermClose`] — so this is only ever reached by the head's `!term close`,
    /// and only after its own confirmation card has been answered with a yes. A caller that
    /// sent this on a keystroke would be the defect the split exists to remove.
    pub fn term_close(&mut self) -> Result<(), ClientError> {
        self.writer.write(&ClientFrame::TermClose)?;
        Ok(())
    }

    /// **Ask what this session's pane is running.** The answer is
    /// [`crate::protocol::ServerFrame::TermStatus`], and it is not returned here because the
    /// frame is read on the head's reader like every other: a head that held a reply table for
    /// this would be a head with a second state machine for one fact.
    ///
    /// The read is what lets a head that is **not drawing** the pane say that something is
    /// running in it — and it is a read rather than a notification because a detach is not an
    /// event: see [`ClientFrame::TermStatus`] for the whole argument.
    pub fn term_status(&mut self) -> Result<(), ClientError> {
        self.writer.write(&ClientFrame::TermStatus)?;
        Ok(())
    }

    /// **Ask the model to propose `!` completions for a prefix** — the smart half of the
    /// `!` completion. The history is this head's own and is the first answer; this is
    /// asked for only when the history has no match for the prefix.
    ///
    /// `prefix` is the composer's line as typed, `!` first. The daemon builds the prompt
    /// from the session's own rows and asks the LOCAL model; the answer comes back as a
    /// [`crate::protocol::ServerFrame::ShellSuggestions`] on this head's reader, carrying
    /// the prefix back so the head can key its cache by it. **Nothing here submits**: the
    /// lines are candidates for the composer, and Enter is still the operator's.
    ///
    /// **`client_request_id` is the caller's, and this is the one client method where
    /// that is true.** Every other frame here mints its own id and returns it, because
    /// the only thing that needs to recognise the answer is the writer. A suggestion is
    /// not answered on the caller's behalf: the head has to match the answer to the
    /// (prefix, transcript position) it asked about, and it can only do that if the id it
    /// filed the ask under is the id that travels. Minting a second one here would leave
    /// the head holding a key the daemon never echoes — the answer would arrive, be
    /// looked up, miss, and be dropped, and the completion would hang on *asking the
    /// model* for ever. The head's ids are `{head_id}-s{n}`, which cannot collide with
    /// [`Self::next_id`]'s `{head_id}-{n}`.
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
    /// Not a command — no request id comes back, nothing is announced.
    pub fn secret(&mut self, req_id: &str, secret: Option<String>) -> Result<(), ClientError> {
        self.writer.write(&ClientFrame::Secret {
            req_id: req_id.to_string(),
            secret,
        })?;
        Ok(())
    }

    /// **A head's answer to a `PromptRequested`**: the line the person typed, for the
    /// stdin of the operator's own running command.
    ///
    /// Not a command, for [`ClientFrame::PromptAnswer`]'s own reason and the sharpest
    /// version of it: the session worker is **blocked inside the very command that is
    /// asking**, so a line queued behind that turn would be drained by the thread waiting
    /// for it. An empty `line` is a bare Enter and is a real answer.
    ///
    /// **Not a secret and not a path to one.** A password goes on
    /// [`HeadClient::secret`], to the `askpass` connection that asked for it.
    pub fn prompt_answer(&mut self, req_id: &str, line: &str) -> Result<(), ClientError> {
        self.writer.write(&ClientFrame::PromptAnswer {
            req_id: req_id.to_string(),
            line: line.to_string(),
        })?;
        Ok(())
    }

    /// **One line to this session's own running command, on demand** — the manual way in.
    ///
    /// No request id and no job id: it addresses *whatever operator command this session is
    /// running right now*, which the daemon knows and the head does not. The verb is
    /// [`letibot_sessionlog::send_line`], and the line arrives here with the verb stripped.
    ///
    /// This is the floor under the prompt card. The card is raised when the daemon can see
    /// that the run is **blocked reading the terminal it holds** — and nothing has been typed
    /// at it for a beat — and that reading has
    /// misses it names — a program blocked on another fd, one that asks and keeps drawing, a
    /// `/proc` a confined session's daemon may not read. None of those stops a person from
    /// answering, and this is how they do it.
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

    /// Grant or deny an open **permission**, by option id.
    pub fn answer(&mut self, req_id: &str, option_id: &str) -> Result<String, ClientError> {
        self.answer_with(req_id, option_id, None, None)
    }

    /// The same, with the operator's own glob for an *always allow*.
    ///
    /// Separate rather than a fourth argument on `answer`, because every caller that
    /// is not offering a pattern should keep saying so by not passing one — a
    /// `None` threaded through a dozen call sites is a `Some` waiting to be typed by
    /// mistake.
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

    /// Answer an open **question** with what the person actually did: chose an
    /// option, chose one and qualified it, or typed a reply (`PROTOCOL_VERSION` 5).
    ///
    /// There is deliberately no `defer` here. A head that wants to come back to a
    /// question simply does not call this, and the question stays open — a deferral
    /// that travelled as an answer is how a turn continues on an assumption nobody
    /// made.
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

    /// Ask what sessions this daemon holds. Answered with `ServerFrame::Sessions`
    /// on the same stream the events arrive on, so the caller reads it out of its
    /// own pump rather than blocking here — a head that stopped to wait for a list
    /// would stop rendering the turn it is watching.
    pub fn list_sessions(&mut self) -> Result<(), ClientError> {
        self.writer.write(&ClientFrame::ListSessions)?;
        Ok(())
    }

    /// `workspace` is the tree the new session is about — the head's own working
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

    /// Ask the daemon to bring a stored session in. Answered with `Sessions`
    /// carrying it as `created`, exactly as `new_session` is — a head then switches
    /// to it by the same code path, which is what keeps "make one" and "get the old
    /// one back" from being two half-tested flows.
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

    /// Move this connection to another session.
    ///
    /// The answer is a second `Hello`, which the caller applies exactly the way it
    /// applied the first — the head's late-join path *is* its switch path, which is
    /// the reason the daemon answers with a `Hello` rather than a frame of its own.
    ///
    /// **`head_id` changes.** The head is a different head in the new session, and a
    /// client that kept the old one would ack into a session it had left.
    pub fn switch(&mut self, session_id: &str, since_seq: u64) -> Result<(), ClientError> {
        self.writer.write(&ClientFrame::Switch {
            session_id: session_id.to_string(),
            since_seq,
        })?;
        Ok(())
    }

    /// Read another session's scrollback — or, with [`PeekShape::Rows`], its **rows** —
    /// without leaving this one. Answered with a `Peeked` frame on the pump, like every
    /// other ask — a head that stopped to wait for it would stop rendering the turn it is
    /// watching.
    ///
    /// **The shape is asked for and never assumed.** `Events` is what this has always
    /// sent, and what a head that draws the ring itself keeps sending; `Rows` is for a pane
    /// that wants to draw a session the way it draws any other, which is why it exists at
    /// all — see [`PeekShape`].
    pub fn peek(&mut self, session_id: &str, shape: PeekShape) -> Result<(), ClientError> {
        self.writer.write(&ClientFrame::Peek {
            session_id: session_id.to_string(),
            shape,
        })?;
        Ok(())
    }

    /// Read a **window** of one row's body without leaving this session. Answered with a
    /// `RowFetched` frame on the pump.
    ///
    /// `row` is the row's **position in the session** — `0` is its first row ever — which is
    /// the only name for a row a head can always construct: it knows the rows it holds and
    /// `items_dropped` says how many came before them. `at` is a byte offset into the body and
    /// `len` how much to ask for; the daemon caps `len` and clamps `at` to a character
    /// boundary, so the answer is authoritative about where the window starts. Nothing is
    /// cached: the same window asked twice is read twice, and a row the daemon has since
    /// trimmed answers `body: None`.
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

    /// Take the head id from a `Hello` the caller pumped off the socket.
    ///
    /// Not folded into `switch`: the `Hello` arrives on the reader thread, and a
    /// client that read it here would race its own pump for the same bytes.
    pub fn seated(&mut self, head_id: &str) {
        self.head_id = head_id.to_string();
    }

    pub fn detach(&mut self) -> Result<(), ClientError> {
        self.writer.write(&ClientFrame::Detach)?;
        Ok(())
    }

    /// **Make the reader's `read` return at once.**
    ///
    /// A reader blocked in `read` on a socket is woken by data, by EOF, or by nothing at
    /// all — and a daemon that is merely idle sends none of those. So a caller that has
    /// to *wait* for its reader thread has to end the read itself, or a `join` on a live
    /// connection is a hang rather than a wait. Measured: `Link::close` did exactly that,
    /// and the end-to-end reconnect test deadlocked on it.
    ///
    /// `Shutdown::Both` rather than `Read`: this leaves the socket unusable on purpose —
    /// the caller is done with it — and shutting both directions is what makes the
    /// reader's clone report EOF.
    pub fn shut_down(&mut self) -> Result<(), ClientError> {
        self.writer.get_mut().shutdown(std::net::Shutdown::Both)?;
        Ok(())
    }
}

/// What the pump hands a head: a frame, or a line this build could not read.
///
/// # Why an unreadable line is not the end of a connection
///
/// `ServerFrame` and `SessionEvent` are **internally tagged** — `{"frame": "event",
/// …}`, `{"event": "delta", …}` — so a tag this build does not know fails the whole
/// line. That is exactly what a daemon one version ahead looks like from here: the
/// frames the two share parse, the first one they do not does not, and the fact that
/// was learned is about two builds and not about the stream.
///
/// The pump used to discard the error and return, which killed the channel: the head
/// drew one more frame and exited, and `the daemon closed the connection` is the
/// closest it ever came to saying why. The daemon's own read loop had the same shape
/// (`Err(e) => break Err(e)`, so a frame it could not read hung up in silence) and
/// was fixed on 2026-09-20 by sending a `Bye` that names both protocol versions. This
/// is the head's half of that, and it is a *different* answer on purpose: the daemon
/// can only lose the connection, while a head reading a stream it mostly understands
/// should keep the stream and say what it could not read.
#[derive(Debug)]
pub enum Inbound {
    Frame(ServerFrame),
    /// One line of the stream that did not parse, kept so the head can say what it
    /// was. The connection is still up.
    Unreadable(Unreadable),
}

impl Inbound {
    /// The frame, or a panic naming the line.
    ///
    /// For a caller that owns both ends of the socket — the tests — where an
    /// unreadable line is a bug in the test rather than a skew to report.
    pub fn frame(self) -> ServerFrame {
        match self {
            Inbound::Frame(f) => f,
            Inbound::Unreadable(u) => panic!("unreadable frame: {u:?}"),
        }
    }
}

/// One line of the stream that did not parse, **with the line**.
///
/// The line is the whole reason this type exists. A decoder that reports "bad
/// frame" without the frame turns a precise complaint into a shrug, and the first
/// question anybody asks about a skew is *which frame*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unreadable {
    /// The line, as it came off the wire. Kept whole; truncated at the point of
    /// display, where the width is known.
    pub line: String,
    /// serde's own complaint — `unknown variant \`job_output\`, expected one of …` —
    /// which is what names the skew when the tag is one this build has never heard
    /// of.
    pub detail: String,
}

impl Unreadable {
    /// **The sentence the operator reads.** What it was, why it is almost always a
    /// daemon newer than this head, and that the head is still here.
    ///
    /// One function so a head cannot say this three ways — a head that exits, a head
    /// that shrugs and a head that counts have to agree about what happened, and the
    /// only way to guarantee that is one string.
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
/// Drive a reader on this thread, pushing everything it reads into `tx`.
///
/// Returns when the daemon closes or the channel's receiver is gone. Detach is
/// **not** an error: a head that closed its own connection is the normal exit.
///
/// **A line this build cannot read is handed over, not swallowed, and is not the end
/// of the connection.** See [`Inbound`]: the old `Err(_) => return` here killed the
/// channel, so a head met a daemon it could not parse by drawing one more frame and
/// exiting — `the daemon closed the connection` being the closest it ever came to
/// saying why.
pub fn pump(mut reader: FrameReader<UnixStream>, tx: Sender<Inbound>) {
    loop {
        match reader.read::<ServerFrame>() {
            Ok(f) => {
                let bye = matches!(f, ServerFrame::Bye { .. });
                if tx.send(Inbound::Frame(f)).is_err() || bye {
                    return;
                }
            }
            // **One unreadable line is a fact about two builds, not about the
            // stream.** The next line is very likely one this head reads perfectly,
            // so the connection stays up and the complaint travels to the head,
            // which counts it. A `Malformed` costs the reader nothing: the line has
            // already been consumed, and the frame after it is the next one read.
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

    /// **One line this build cannot read does not end the connection, and it keeps
    /// the line.** A daemon one version ahead sends a tag this build has never heard
    /// of; the frames the two share are fine, and the pump used to throw the error
    /// away and return — which killed the channel and left the head exiting with
    /// *"the daemon closed the connection"* about a daemon that was still talking.
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

    /// **The sentence names the skew.** What the operator reads has to say what
    /// arrived, that it is almost always a newer daemon, and that the head is still
    /// attached — a head that exits and a head that shrugs are the two failures this
    /// middle path exists between.
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
