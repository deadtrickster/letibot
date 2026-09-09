//! Newline-delimited JSON over any `Read`/`Write`.
//!
//! §13.4 requires the remote head to speak *"the same frames as the socket"*. The
//! way to make that true rather than aspirational is for the framing to know
//! nothing about the transport: a Unix socket, a TCP stream and a WebSocket text
//! channel are all a `Read` and a `Write` here.
//!
//! NDJSON rather than a length-prefixed binary encoding, for one reason that is not
//! taste: a session log is the thing an operator reads when something went wrong,
//! and `nc -U` plus `jq` beats a decoder you have to build first. The control
//! channel (§3.3, W15) is where the binary encoding belongs, because that one is on
//! the hot path and this one is not — a head's traffic is bounded by what a human
//! can read.

use std::io::{BufRead, BufReader, Read, Write};

use serde::Serialize;
use serde::de::DeserializeOwned;

#[derive(Debug)]
pub enum WireError {
    Io(std::io::Error),
    /// The peer sent something this version cannot parse. The offending line is
    /// kept: a decoder that reports "bad frame" without the frame turns a precise
    /// complaint into a shrug.
    Malformed {
        line: String,
        detail: String,
    },
    /// The peer closed. Not an error in itself — for a head, this is detach.
    Eof,
}

impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WireError::Io(e) => write!(f, "wire io: {e}"),
            WireError::Malformed { line, detail } => {
                let shown: String = line.chars().take(200).collect();
                write!(f, "malformed frame ({detail}): {shown}")
            }
            WireError::Eof => write!(f, "peer closed"),
        }
    }
}

impl std::error::Error for WireError {}

impl From<std::io::Error> for WireError {
    fn from(e: std::io::Error) -> Self {
        WireError::Io(e)
    }
}

/// Frame reader.
pub struct FrameReader<R: Read> {
    inner: BufReader<R>,
    line: String,
}

impl<R: Read> FrameReader<R> {
    pub fn new(r: R) -> Self {
        FrameReader {
            inner: BufReader::new(r),
            line: String::new(),
        }
    }

    pub fn read<T: DeserializeOwned>(&mut self) -> Result<T, WireError> {
        loop {
            self.line.clear();
            let n = self.inner.read_line(&mut self.line)?;
            if n == 0 {
                return Err(WireError::Eof);
            }
            let trimmed = self.line.trim();
            if trimmed.is_empty() {
                continue;
            }
            return serde_json::from_str(trimmed).map_err(|e| WireError::Malformed {
                line: trimmed.to_string(),
                detail: e.to_string(),
            });
        }
    }
}

/// Frame writer. Flushes every frame: a head that renders in real time cannot wait
/// for a buffer to fill, and this is exactly the mistake §8.5's server-warming rule
/// is written against on the other side of the connection.
pub struct FrameWriter<W: Write> {
    inner: W,
    buf: Vec<u8>,
}

impl<W: Write> FrameWriter<W> {
    pub fn new(w: W) -> Self {
        FrameWriter {
            inner: w,
            buf: Vec::with_capacity(4096),
        }
    }

    pub fn write<T: Serialize>(&mut self, frame: &T) -> Result<(), WireError> {
        self.buf.clear();
        serde_json::to_writer(&mut self.buf, frame).map_err(|e| WireError::Malformed {
            line: String::new(),
            detail: e.to_string(),
        })?;
        self.buf.push(b'\n');
        self.inner.write_all(&self.buf)?;
        self.inner.flush()?;
        Ok(())
    }

    pub fn get_mut(&mut self) -> &mut W {
        &mut self.inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Ack, ClientFrame};

    #[test]
    fn frames_round_trip_through_a_pipe() {
        let mut buf = Vec::new();
        {
            let mut w = FrameWriter::new(&mut buf);
            w.write(&ClientFrame::Ack(Ack {
                seq: 3,
                rendered: 1,
                filtered: 2,
            }))
            .unwrap();
            w.write(&ClientFrame::Detach).unwrap();
        }
        let mut r = FrameReader::new(&buf[..]);
        assert!(matches!(
            r.read::<ClientFrame>().unwrap(),
            ClientFrame::Ack(Ack { seq: 3, .. })
        ));
        assert!(matches!(
            r.read::<ClientFrame>().unwrap(),
            ClientFrame::Detach
        ));
        assert!(matches!(r.read::<ClientFrame>(), Err(WireError::Eof)));
    }

    #[test]
    fn a_bad_frame_names_itself() {
        let mut r = FrameReader::new(&b"{\"frame\":\"nope\"}\n"[..]);
        let e = r.read::<ClientFrame>().unwrap_err();
        assert!(format!("{e}").contains("nope"), "{e}");
    }
}
