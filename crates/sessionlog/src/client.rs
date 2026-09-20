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

use crate::protocol::{Ack, Caps, ClientFrame, PROTOCOL_VERSION, ServerFrame};
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
        let hello: ServerFrame = reader.read()?;
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

    /// This session's background jobs, as the daemon's process table has them.
    /// The head renders what comes back; it does not decide what is in it.
    pub fn list_jobs(&mut self) -> Result<(), ClientError> {
        self.writer.write(&ClientFrame::ListJobs)?;
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

    /// Read another session's scrollback without leaving this one. Answered with
    /// a `Peeked` frame on the pump, like every other ask — a head that stopped
    /// to wait for it would stop rendering the turn it is watching.
    pub fn peek(&mut self, session_id: &str) -> Result<(), ClientError> {
        self.writer.write(&ClientFrame::Peek {
            session_id: session_id.to_string(),
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
}

/// Drive a reader on this thread, pushing every frame into `tx`.
///
/// Returns when the daemon closes or the channel's receiver is gone. Detach is
/// **not** an error: a head that closed its own connection is the normal exit.
pub fn pump(mut reader: FrameReader<UnixStream>, tx: Sender<ServerFrame>) {
    loop {
        match reader.read::<ServerFrame>() {
            Ok(f) => {
                let bye = matches!(f, ServerFrame::Bye { .. });
                if tx.send(f).is_err() || bye {
                    return;
                }
            }
            Err(_) => return,
        }
    }
}
