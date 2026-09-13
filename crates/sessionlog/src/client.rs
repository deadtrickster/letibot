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

    pub fn interrupt(&mut self, expected_seq: u64, reason: &str) -> Result<String, ClientError> {
        let client_request_id = self.next_id();
        self.writer.write(&ClientFrame::Interrupt {
            client_request_id: client_request_id.clone(),
            expected_seq,
            reason: reason.to_string(),
        })?;
        Ok(client_request_id)
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

    /// Grant or deny an open **permission**, by option id.
    pub fn answer(&mut self, req_id: &str, option_id: &str) -> Result<String, ClientError> {
        let client_request_id = self.next_id();
        self.writer.write(&ClientFrame::Answer {
            client_request_id: client_request_id.clone(),
            req_id: req_id.to_string(),
            option_id: option_id.to_string(),
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
    pub fn rename_session(
        &mut self,
        session_id: &str,
        title: &str,
    ) -> Result<String, ClientError> {
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
