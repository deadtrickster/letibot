//! A blocking HTTP/1.1 client, just large enough for `POST /completion` — and,
//! since the flowy connector, for a bearer-authenticated `GET`/`POST`/`DELETE`
//! against the node.
//!
//! # Why this is hand-written rather than `reqwest`
//!
//! The turn engine talks to exactly one endpoint, over loopback, with no TLS, no
//! redirects, no proxies and no compression. What it *does* need is
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
//! content-length framing, and nothing else. The flowy node is plain HTTP on the
//! LAN, so it needs nothing this list refuses.
//!
//! # One crate, two callers
//!
//! It is its own crate rather than a module of the turn engine because the flowy
//! connector needs the same client with two more things — an `Authorization`
//! header and a `DELETE` verb — and *a second hand-rolled HTTP client is a second
//! place for a framing bug to live*. [`send`] is the general entry; [`post_json`]
//! and [`get`] are the two spellings the engine already used.

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

    /// `HOST:PORT`, as a person types it. The inverse of [`Endpoint::authority`], so
    /// an address written to a file comes back as the same endpoint.
    ///
    /// `rsplit_once` because an IPv6 literal contains colons and only the last one
    /// separates the port.
    pub fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim();
        let (host, port) = s
            .rsplit_once(':')
            .ok_or_else(|| format!("`{s}` is not HOST:PORT"))?;
        if host.is_empty() {
            return Err(format!("`{s}` has no host"));
        }
        let port: u16 = port
            .parse()
            .map_err(|e| format!("`{s}`: port {port}: {e}"))?;
        Ok(Endpoint::new(host, port))
    }
}

/// One HTTP response body, delivered as a stream of decoded bytes.
pub struct Body {
    reader: BufReader<TcpStream>,
    framing: Framing,
    done: bool,
    /// **The chunk-size line, kept between chunks.** `read_line` appends to a `String`, so a fresh
    /// one per chunk was an allocation per chunk for a line of four hex digits. Cleared, not
    /// replaced: the capacity survives, and a chunked body is one chunk per TCP read.
    size_line: String,
}

enum Framing {
    Chunked,
    Length(usize),
    UntilClose,
}

impl Body {
    /// Read the next chunk of decoded body bytes **into `buf`**, or `false` at end of body.
    ///
    /// # Why the caller brings the buffer
    ///
    /// This returned a fresh `Vec<u8>` per chunk, and BOTH callers immediately copied it into a
    /// buffer of their own and dropped it — `read_to_string` extends into its output, and
    /// `for_each_frame` extends into its pending frame. So every chunk of every response was
    /// allocated, filled, copied and freed. The caller's buffer is the one that is actually
    /// wanted, and `resize` into it reuses its capacity when the next chunk is no larger.
    fn next_bytes(&mut self, buf: &mut Vec<u8>) -> Result<bool, HttpError> {
        buf.clear();
        if self.done {
            return Ok(false);
        }
        match self.framing {
            Framing::Chunked => {
                self.size_line.clear();
                if self.reader.read_line(&mut self.size_line)? == 0 {
                    self.done = true;
                    return Err(HttpError::Malformed(
                        "connection closed mid-chunk: the server went away before the \
                         terminating 0-length chunk, so the response is incomplete and \
                         must not be treated as a finished turn"
                            .into(),
                    ));
                }
                let size =
                    usize::from_str_radix(self.size_line.trim().split(';').next().unwrap_or(""), 16)
                        .map_err(|_| {
                            HttpError::Malformed(format!("chunk size {:?}", self.size_line))
                        })?;
                if size == 0 {
                    self.done = true;
                    return Ok(false);
                }
                // `resize` and not `vec![0u8; size]`: the buffer keeps its capacity across chunks,
                // so this is an allocation only when a chunk is larger than any before it.
                buf.resize(size, 0);
                self.reader.read_exact(buf)?;
                let mut crlf = [0u8; 2];
                self.reader.read_exact(&mut crlf)?;
                Ok(true)
            }
            Framing::Length(n) => {
                buf.resize(n, 0);
                self.reader.read_exact(buf)?;
                self.done = true;
                Ok(true)
            }
            Framing::UntilClose => {
                self.reader.read_to_end(buf)?;
                self.done = true;
                Ok(true)
            }
        }
    }

    /// Consume the whole body as text.
    pub fn read_to_string(mut self) -> Result<String, HttpError> {
        let mut out = Vec::new();
        let mut chunk = Vec::new();
        while self.next_bytes(&mut chunk)? {
            out.extend_from_slice(&chunk);
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    /// Call `on_event` once per `data:` payload of a `text/event-stream`.
    ///
    /// Returning `Flow::Stop` closes the connection immediately. That is how an
    /// urgent steering message and a repetition guard abort a generation: llama.cpp
    /// notices the dropped socket and frees the slot, which is the only abort
    /// mechanism the fallback path has.
    pub fn for_each_event<F>(self, mut on_event: F) -> Result<(), HttpError>
    where
        F: FnMut(&str) -> Result<Flow, HttpError>,
    {
        self.for_each_frame(|frame| {
            let mut flow = Flow::Continue;
            for data in &frame.data {
                if on_event(data)? == Flow::Stop {
                    flow = Flow::Stop;
                    break;
                }
            }
            Ok(flow)
        })
    }

    /// Call `on_frame` once per SSE frame, with its `event:`, `id:` and every
    /// `data:` line. The flowy stream resumes from `id:` (`Last-Event-ID`), which
    /// [`Body::for_each_event`] drops, so the connector reads through this.
    pub fn for_each_frame<F>(mut self, mut on_frame: F) -> Result<(), HttpError>
    where
        F: FnMut(SseFrame) -> Result<Flow, HttpError>,
    {
        let mut pending = Vec::<u8>::new();
        let mut chunk = Vec::new();
        while self.next_bytes(&mut chunk)? {
            pending.extend_from_slice(&chunk);
            // SSE frames are terminated by a blank line. A chunk boundary is not a
            // frame boundary — assuming it is works right up until a long tool-call
            // argument spans two TCP segments.
            while let Some(pos) = find(&pending, b"\n\n") {
                // **Parsed from a slice of `pending`, not from a copy of it.** This was
                // `drain(..pos + 2).collect::<Vec<u8>>()` — a `Vec` allocated and freed per SSE
                // frame, and a frame is a token — purely so the bytes could be borrowed as a whole
                // string. Borrowing the slice directly is the same reading without the copy; the
                // drain still happens, just after the parse rather than as part of it.
                //
                // **What is deliberately NOT avoided: the `data:` line's `String` below.** A frame
                // outlives this borrow — `on_frame` is called after the drain and may keep it — so
                // the line has to be owned. Making `SseFrame` borrow would remove that allocation
                // too, at the cost of a lifetime parameter on a public type with two consumers, and
                // it would then need a `Vec<&str>` for the lines, which allocates as well. So the
                // trade is a wash and the API stays.
                let mut frame = SseFrame::default();
                let raw = String::from_utf8_lossy(&pending[..pos]);
                for line in raw.lines() {
                    if let Some(v) = line.strip_prefix("data:") {
                        frame.data.push(v.trim_start().to_string());
                    } else if let Some(v) = line.strip_prefix("event:") {
                        frame.event = Some(v.trim().to_string());
                    } else if let Some(v) = line.strip_prefix("id:") {
                        frame.id = Some(v.trim().to_string());
                    }
                    // A `:` comment line — the heartbeat — is a frame with nothing
                    // in it, and it is still handed over: a reader that wants to
                    // know the connection is alive reads the empty frame.
                }
                // The borrow of `pending` ends with `raw` and the frame's own strings, so the
                // drain is free to take it mutably — and it must come after the parse, which is
                // the whole point of borrowing rather than copying.
                drop(raw);
                pending.drain(..pos + 2);
                if on_frame(frame)? == Flow::Stop {
                    return Ok(());
                }
            }
        }
        Ok(())
    }
}

/// One `text/event-stream` frame. Empty `data` with no `event`/`id` is a
/// heartbeat (a `:` comment line).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SseFrame {
    pub event: Option<String>,
    pub id: Option<String>,
    pub data: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Stop,
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// One request, as the caller spells it. Headers are appended after `Host` and
/// `Connection: close`, so a caller can add `Authorization`, `Accept` or
/// `Last-Event-ID` without this crate knowing what they mean.
#[derive(Debug, Clone)]
pub struct Request<'a> {
    pub method: &'a str,
    pub path: &'a str,
    pub headers: &'a [(&'a str, &'a str)],
    /// Sent with `Content-Length` when present. `None` sends no body and no
    /// length — a `GET` or a `DELETE`.
    pub body: Option<&'a str>,
}

impl<'a> Request<'a> {
    pub fn get(path: &'a str) -> Self {
        Request {
            method: "GET",
            path,
            headers: &[],
            body: None,
        }
    }

    pub fn delete(path: &'a str) -> Self {
        Request {
            method: "DELETE",
            path,
            headers: &[],
            body: None,
        }
    }

    pub fn post_json(path: &'a str, body: &'a str) -> Self {
        Request {
            method: "POST",
            path,
            headers: &[("Content-Type", "application/json")],
            body: Some(body),
        }
    }

    pub fn with_headers(mut self, headers: &'a [(&'a str, &'a str)]) -> Self {
        self.headers = headers;
        self
    }
}

/// Send one request and hand back the undecoded response body. Non-2xx is
/// [`HttpError::Status`] with the body kept.
pub fn send(endpoint: &Endpoint, req: Request<'_>) -> Result<Body, HttpError> {
    let mut stream = connect(endpoint)?;
    let mut head = format!(
        "{} {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n",
        req.method,
        req.path,
        endpoint.authority()
    );
    // `Accept` is a default the caller may override by naming it.
    if !req
        .headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("accept"))
    {
        head.push_str("Accept: application/json, text/event-stream\r\n");
    }
    for (k, v) in req.headers {
        head.push_str(k);
        head.push_str(": ");
        head.push_str(v);
        head.push_str("\r\n");
    }
    if let Some(body) = req.body {
        head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    if let Some(body) = req.body {
        stream.write_all(body.as_bytes())?;
    }
    stream.flush()?;
    read_response(BufReader::new(stream))
}

/// `POST` a JSON body and hand back the undecoded response body.
pub fn post_json(endpoint: &Endpoint, path: &str, body: &str) -> Result<Body, HttpError> {
    send(
        endpoint,
        Request {
            method: "POST",
            path,
            headers: &[
                ("Content-Type", "application/json"),
                ("Accept", "text/event-stream"),
            ],
            body: Some(body),
        },
    )
}

/// A plain GET, for the endpoints that describe the server rather than drive it —
/// `/props` above all. Same connection discipline as [`post_json`], same response
/// reader, because a second hand-rolled HTTP client is a second place for a framing
/// bug to live.
pub fn get(endpoint: &Endpoint, path: &str) -> Result<Body, HttpError> {
    send(
        endpoint,
        Request::get(path).with_headers(&[("Accept", "application/json")]),
    )
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
        size_line: String::new(),
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
