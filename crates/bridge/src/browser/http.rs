//! The little HTTP/1.1 the bridge needs: one request per connection, a bounded header and a
//! bounded body, JSON answers and one SSE stream. `Connection: close` on every answer.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;

/// A request head larger than this is refused: the extension sends a handful of short headers.
pub const MAX_HEAD: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    pub path: String,
    /// Lowercased names, values trimmed. A repeated header keeps its first value.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

/// Why a request was not read: the status to answer with and the sentence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub status: u16,
    pub why: String,
}

fn refuse(status: u16, why: impl Into<String>) -> Refusal {
    Refusal {
        status,
        why: why.into(),
    }
}

/// Read one request. The body is read only up to `max_body`; a larger `Content-Length` is a 413
/// that names both numbers, and nothing past the head is read for it.
pub fn read_request(stream: &mut TcpStream, max_body: usize) -> Result<Request, Refusal> {
    let mut reader = BufReader::new(stream);
    let mut head = Vec::new();
    loop {
        let mut line = Vec::new();
        let n = (&mut reader)
            .take(MAX_HEAD as u64 + 1)
            .read_until(b'\n', &mut line)
            .map_err(|e| refuse(400, format!("reading the request: {e}")))?;
        if n == 0 {
            return Err(refuse(400, "the connection closed inside the request head"));
        }
        head.extend_from_slice(&line);
        if head.len() > MAX_HEAD {
            return Err(refuse(431, format!("request head over {MAX_HEAD} bytes")));
        }
        if line == b"\r\n" || line == b"\n" {
            break;
        }
    }
    let text = String::from_utf8(head).map_err(|_| refuse(400, "request head is not UTF-8"))?;
    let mut lines = text.lines();
    let first = lines.next().unwrap_or_default();
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();
    if method.is_empty() || !path.starts_with('/') {
        return Err(refuse(400, "not an HTTP request line"));
    }
    let mut headers: Vec<(String, String)> = Vec::new();
    for line in lines {
        if let Some((n, v)) = line.split_once(':') {
            let n = n.trim().to_ascii_lowercase();
            if !headers.iter().any(|(have, _)| *have == n) {
                headers.push((n, v.trim().to_string()));
            }
        }
    }
    let mut req = Request {
        method,
        path,
        headers,
        body: Vec::new(),
    };
    let len = match req.header("content-length") {
        Some(v) => v
            .parse::<usize>()
            .map_err(|_| refuse(400, format!("Content-Length `{v}` is not a number")))?,
        None => 0,
    };
    if len > max_body {
        return Err(refuse(
            413,
            format!("the request body is {len} bytes and the bridge takes at most {max_body}"),
        ));
    }
    let mut body = vec![0u8; len];
    reader
        .read_exact(&mut body)
        .map_err(|e| refuse(400, format!("reading the body: {e}")))?;
    req.body = body;
    Ok(req)
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        413 => "Payload Too Large",
        431 => "Request Header Fields Too Large",
        _ => "Error",
    }
}

/// The CORS lines for an allowed origin, or nothing: a refused origin gets no allow header.
fn cors(origin: Option<&str>) -> String {
    match origin {
        Some(o) => format!("Access-Control-Allow-Origin: {o}\r\nVary: Origin\r\n"),
        None => String::new(),
    }
}

pub fn json(stream: &mut TcpStream, status: u16, origin: Option<&str>, body: &serde_json::Value) {
    let text = body.to_string();
    let _ = write!(
        stream,
        "HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{}Cache-Control: no-store\r\nConnection: close\r\n\r\n{text}",
        reason(status),
        text.len(),
        cors(origin),
    );
    let _ = stream.flush();
}

pub fn error(stream: &mut TcpStream, status: u16, origin: Option<&str>, why: &str) {
    json(stream, status, origin, &serde_json::json!({ "error": why }));
}

/// The preflight answer for the one origin allowed.
pub fn preflight(stream: &mut TcpStream, origin: &str) {
    let _ = write!(
        stream,
        "HTTP/1.1 204 No Content\r\n{}Access-Control-Allow-Methods: GET, POST\r\nAccess-Control-Allow-Headers: authorization, content-type\r\nAccess-Control-Max-Age: 600\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        cors(Some(origin)),
    );
    let _ = stream.flush();
}

/// An SSE stream's head. The events follow with [`event`] until the handler returns.
pub fn sse_head(stream: &mut TcpStream, origin: &str) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\n{}Connection: close\r\n\r\n",
        cors(Some(origin)),
    )?;
    stream.flush()
}

/// One event. `data` is one line of JSON, which `serde_json` never breaks.
pub fn event(stream: &mut TcpStream, name: &str, data: &serde_json::Value) -> std::io::Result<()> {
    write!(stream, "event: {name}\ndata: {data}\n\n")?;
    stream.flush()
}

/// A comment line, so a client that left is noticed while the session is quiet.
pub fn heartbeat(stream: &mut TcpStream) -> std::io::Result<()> {
    stream.write_all(b": keepalive\n\n")?;
    stream.flush()
}
