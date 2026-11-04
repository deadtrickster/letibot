//! Newline-delimited JSON over any `Read`/`Write`, so a Unix socket, TCP stream and WebSocket
//! channel speak the same frames. NDJSON because operators read session logs and `nc -U` plus
//! `jq` beats a decoder. Binary belongs on the control channel.

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
    /// The peer closed. Not an error in itself: for a head, this is detach.
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

/// Frame writer. Flushes every frame: a head that renders in real time cannot wait for a buffer to
/// fill, and a client left waiting on an unflushed frame is the same defect as a server that warms
/// up in silence.
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

    /// Many frames, one flush: the frames are serialised into the buffer and sent together (a write
    /// per 64 KiB at most), so a batch already queued for a head costs one syscall, not one per frame.
    pub fn write_all<T: Serialize>(
        &mut self,
        frames: impl IntoIterator<Item = T>,
    ) -> Result<(), WireError> {
        const SEND_AT: usize = 64 * 1024;
        self.buf.clear();
        for frame in frames {
            serde_json::to_writer(&mut self.buf, &frame).map_err(|e| WireError::Malformed {
                line: String::new(),
                detail: e.to_string(),
            })?;
            self.buf.push(b'\n');
            if self.buf.len() >= SEND_AT {
                self.inner.write_all(&self.buf)?;
                self.buf.clear();
            }
        }
        if !self.buf.is_empty() {
            self.inner.write_all(&self.buf)?;
        }
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

    /// A writer that counts the calls it takes, which is the syscalls a socket would take.
    struct Counting {
        writes: usize,
        bytes: Vec<u8>,
    }
    impl Write for Counting {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.writes += 1;
            self.bytes.extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_batch_of_frames_is_one_write_and_reads_back_frame_by_frame() {
        let mut sink = Counting {
            writes: 0,
            bytes: Vec::new(),
        };
        {
            let mut w = FrameWriter::new(&mut sink);
            let frames: Vec<ClientFrame> = (0..50)
                .map(|seq| {
                    ClientFrame::Ack(Ack {
                        seq,
                        rendered: 0,
                        filtered: 0,
                    })
                })
                .collect();
            w.write_all(&frames).unwrap();
        }
        assert_eq!(sink.writes, 1, "fifty queued frames take one write");
        let mut r = FrameReader::new(&sink.bytes[..]);
        for want in 0..50 {
            match r.read::<ClientFrame>().unwrap() {
                ClientFrame::Ack(a) => assert_eq!(a.seq, want),
                other => panic!("{other:?}"),
            }
        }
        assert!(matches!(r.read::<ClientFrame>(), Err(WireError::Eof)));
    }

    #[test]
    fn a_bad_frame_names_itself() {
        let mut r = FrameReader::new(&b"{\"frame\":\"nope\"}\n"[..]);
        let e = r.read::<ClientFrame>().unwrap_err();
        assert!(format!("{e}").contains("nope"), "{e}");
    }
}
