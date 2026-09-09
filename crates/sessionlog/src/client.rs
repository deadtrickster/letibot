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

    pub fn answer(&mut self, req_id: &str, option_id: &str) -> Result<String, ClientError> {
        let client_request_id = self.next_id();
        self.writer.write(&ClientFrame::Answer {
            client_request_id: client_request_id.clone(),
            req_id: req_id.to_string(),
            option_id: option_id.to_string(),
        })?;
        Ok(client_request_id)
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
