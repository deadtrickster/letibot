//! A blocking HTTP/1.1 client, just large enough for `POST /completion`.
//!
//! # Why this is hand-written rather than `reqwest`
//!
//! The turn engine talks to exactly one endpoint, over loopback, with no TLS, no
//! redirects, no proxies, no compression and no auth. What it *does* need is
//! byte-level control of a `text/event-stream` response so that a partial chunk is
//! delivered to the accumulator the instant it lands — a long prefill must produce
//! `PromptProgress` events while it is happening, which is §8.5's server-warming
//! rule, and omp's cautionary tale is a harness that could not see one.
//!
//! Against that, an async HTTP stack would put a runtime under a crate that is
//! otherwise a pure state machine over token ids. The whole engine is testable
//! today with no executor. That is worth more than the ~120 lines below.
//!
//! What this deliberately does **not** implement, so nobody mistakes it for a
//! general client: TLS, redirects, `Content-Encoding`, connection reuse,
//! HTTP/2, and trailers. It speaks `Connection: close`, chunked or
//! content-length framing, and nothing else.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

#[derive(Debug)]
pub enum HttpError {
    Io(std::io::Error),
    /// The server answered, but not with 2xx. The body is kept: llama.cpp puts a
    /// JSON error object there and discarding it turns a precise complaint into
    /// "the request failed".
    Status {
        code: u16,
        body: String,
    },
    Malformed(String),
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HttpError::Io(e) => write!(f, "http io: {e}"),
            HttpError::Status { code, body } => write!(f, "http {code}: {body}"),
            HttpError::Malformed(m) => write!(f, "malformed http response: {m}"),
        }
    }
}

impl std::error::Error for HttpError {}

impl From<std::io::Error> for HttpError {
    fn from(e: std::io::Error) -> Self {
        HttpError::Io(e)
    }
}

/// Where the model server is, and how patient to be with it.
#[derive(Debug, Clone)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
    /// How long to wait for the *next* byte.
    ///
    /// Not a total-request timeout, and that distinction is §8.5's whole point: a
    /// cold model reload costs 29–31 s and a long prefill costs minutes, both of
    /// which are progress, not a hang. A read timeout resets on every progress
    /// chunk; a total timeout would kill a healthy turn.
    pub read_timeout: Duration,
    pub connect_timeout: Duration,
}

impl Endpoint {
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Endpoint {
            host: host.into(),
            port,
            // Comfortably above the measured 29–31 s cold reload, which is the
            // longest gap between bytes this endpoint can legitimately produce.
            read_timeout: Duration::from_secs(180),
            connect_timeout: Duration::from_secs(10),
        }
    }

    pub fn authority(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

/// One HTTP response body, delivered as a stream of decoded bytes.
pub struct Body {
    reader: BufReader<TcpStream>,
    framing: Framing,
    done: bool,
}

enum Framing {
    Chunked,
    Length(usize),
    UntilClose,
}

impl Body {
    /// Read the next chunk of decoded body bytes, or `None` at end of body.
    fn next_bytes(&mut self) -> Result<Option<Vec<u8>>, HttpError> {
        if self.done {
            return Ok(None);
        }
        match self.framing {
            Framing::Chunked => {
                let mut size_line = String::new();
                if self.reader.read_line(&mut size_line)? == 0 {
                    self.done = true;
                    return Err(HttpError::Malformed(
                        "connection closed mid-chunk: the server went away before the \
                         terminating 0-length chunk, so the response is incomplete and \
                         must not be treated as a finished turn"
                            .into(),
                    ));
                }
                let size =
                    usize::from_str_radix(size_line.trim().split(';').next().unwrap_or(""), 16)
                        .map_err(|_| HttpError::Malformed(format!("chunk size {size_line:?}")))?;
                if size == 0 {
                    self.done = true;
                    return Ok(None);
                }
                let mut buf = vec![0u8; size];
                self.reader.read_exact(&mut buf)?;
                let mut crlf = [0u8; 2];
                self.reader.read_exact(&mut crlf)?;
                Ok(Some(buf))
            }
            Framing::Length(n) => {
                let mut buf = vec![0u8; n];
                self.reader.read_exact(&mut buf)?;
                self.done = true;
                Ok(Some(buf))
            }
            Framing::UntilClose => {
                let mut buf = Vec::new();
                self.reader.read_to_end(&mut buf)?;
                self.done = true;
                Ok(Some(buf))
            }
        }
    }

    /// Consume the whole body as text.
    pub fn read_to_string(mut self) -> Result<String, HttpError> {
        let mut out = Vec::new();
        while let Some(b) = self.next_bytes()? {
            out.extend_from_slice(&b);
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    /// Call `on_event` once per `data:` payload of a `text/event-stream`.
    ///
    /// Returning `Flow::Stop` closes the connection immediately. That is how an
    /// urgent steering message and a repetition guard abort a generation: llama.cpp
    /// notices the dropped socket and frees the slot, which is the only abort
    /// mechanism the fallback path has.
    pub fn for_each_event<F>(mut self, mut on_event: F) -> Result<(), HttpError>
    where
        F: FnMut(&str) -> Result<Flow, HttpError>,
    {
        let mut pending = Vec::<u8>::new();
        while let Some(bytes) = self.next_bytes()? {
            pending.extend_from_slice(&bytes);
            // SSE frames are terminated by a blank line. A chunk boundary is not a
            // frame boundary — assuming it is works right up until a long tool-call
            // argument spans two TCP segments.
            while let Some(pos) = find(&pending, b"\n\n") {
                let frame = pending.drain(..pos + 2).collect::<Vec<u8>>();
                let frame = String::from_utf8_lossy(&frame);
                for line in frame.lines() {
                    let Some(data) = line.strip_prefix("data:") else {
                        continue;
                    };
                    if on_event(data.trim_start())? == Flow::Stop {
                        return Ok(());
                    }
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Stop,
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// `POST` a JSON body and hand back the undecoded response body.
pub fn post_json(endpoint: &Endpoint, path: &str, body: &str) -> Result<Body, HttpError> {
    let mut stream = connect(endpoint)?;
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\n\
         Accept: text/event-stream\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
        endpoint.authority(),
        body.len()
    );
    stream.write_all(request.as_bytes())?;
    stream.write_all(body.as_bytes())?;
    stream.flush()?;
    read_response(BufReader::new(stream))
}

/// A plain GET, for the endpoints that describe the server rather than drive it —
/// `/props` above all. Same connection discipline as [`post_json`], same response
/// reader, because a second hand-rolled HTTP client is a second place for a framing
/// bug to live.
pub fn get(endpoint: &Endpoint, path: &str) -> Result<Body, HttpError> {
    let mut stream = connect(endpoint)?;
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {}\r\nAccept: application/json\r\n\
         Connection: close\r\n\r\n",
        endpoint.authority()
    );
    stream.write_all(request.as_bytes())?;
    stream.flush()?;
    read_response(BufReader::new(stream))
}

fn connect(endpoint: &Endpoint) -> Result<TcpStream, HttpError> {
    let addr = endpoint
        .authority()
        .parse()
        .map(|a| TcpStream::connect_timeout(&a, endpoint.connect_timeout))
        .unwrap_or_else(|_| TcpStream::connect(endpoint.authority()))?;
    addr.set_nodelay(true)?;
    addr.set_read_timeout(Some(endpoint.read_timeout))?;
    Ok(addr)
}

fn read_response(mut reader: BufReader<TcpStream>) -> Result<Body, HttpError> {
    let mut status_line = String::new();
    if reader.read_line(&mut status_line)? == 0 {
        return Err(HttpError::Malformed("empty response".into()));
    }
    let code: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| HttpError::Malformed(format!("status line {status_line:?}")))?;

    let mut chunked = false;
    let mut length: Option<usize> = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Err(HttpError::Malformed("headers truncated".into()));
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let (name, value) = (name.trim().to_ascii_lowercase(), value.trim());
        if name == "transfer-encoding" && value.eq_ignore_ascii_case("chunked") {
            chunked = true;
        } else if name == "content-length" {
            length = value.parse().ok();
        }
    }

    let framing = if chunked {
        Framing::Chunked
    } else if let Some(n) = length {
        Framing::Length(n)
    } else {
        Framing::UntilClose
    };
    let body = Body {
        reader,
        framing,
        done: false,
    };

    if !(200..300).contains(&code) {
        return Err(HttpError::Status {
            code,
            body: body.read_to_string()?,
        });
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_locates_a_frame_boundary_that_straddles_nothing() {
        assert_eq!(find(b"abc\n\ndef", b"\n\n"), Some(3));
        assert_eq!(find(b"abc\ndef", b"\n\n"), None);
    }
}
