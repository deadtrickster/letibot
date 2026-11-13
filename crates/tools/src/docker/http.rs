//! Just enough HTTP/1.1 to speak the Docker Engine API over a unix socket: one request, one
//! response, no keep-alive, no TLS.
//!
//! The proxy is the only path between a seat and the engine, so it has to read what a client wrote
//! and write back what the engine said without either side noticing. Two pieces of HTTP matter and
//! neither is optional: a `Transfer-Encoding: chunked` body (compose and the build client send
//! one) is decoded and re-sent with a `Content-Length`, and a chunked answer is decoded the same
//! way. A client cannot tell the difference; a proxy that dropped the body could.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::time::Duration;

/// One request, as it came off the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    /// The request target: `/v1.44/containers/create?name=x`.
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn new(method: &str, target: &str) -> Request {
        Request {
            method: method.to_string(),
            target: target.to_string(),
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    pub fn get(target: &str) -> Request {
        Request::new("GET", target)
    }

    pub fn json(method: &str, target: &str, body: &serde_json::Value) -> Request {
        let mut req = Request::new(method, target);
        req.set_header("Content-Type", "application/json");
        req.body = serde_json::to_vec(body).unwrap_or_default();
        req
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn set_header(&mut self, name: &str, value: &str) {
        self.headers.retain(|(k, _)| !k.eq_ignore_ascii_case(name));
        self.headers.push((name.to_string(), value.to_string()));
    }

    /// The target up to the `?`.
    pub fn path(&self) -> &str {
        match self.target.find('?') {
            Some(i) => &self.target[..i],
            None => &self.target,
        }
    }

    /// The query, without its `?`.
    pub fn query(&self) -> &str {
        match self.target.find('?') {
            Some(i) => &self.target[i + 1..],
            None => "",
        }
    }

    /// A copy with a new target, keeping method, headers and body.
    pub fn with_target(&self, target: String) -> Request {
        Request {
            target,
            ..self.clone()
        }
    }

    pub fn json_body(&self) -> Option<serde_json::Value> {
        serde_json::from_slice(&self.body).ok()
    }

    pub fn set_body(&mut self, body: &serde_json::Value) {
        // A body the client spelled one way and we re-spell is still one JSON object: the engine
        // reads fields, not bytes, and the key order of a docker object is nobody's contract.
        self.body = serde_json::to_vec(body).unwrap_or_default();
        if !self.body.is_empty() {
            self.set_header("Content-Type", "application/json");
        }
    }

    /// One request off `r`, or `None` at a clean end of stream.
    pub fn read(r: &mut impl BufRead) -> std::io::Result<Option<Request>> {
        let mut line = String::new();
        if r.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            return Ok(None);
        }
        let mut parts = line.split_whitespace();
        let method = parts.next().unwrap_or_default().to_string();
        let target = parts.next().unwrap_or_default().to_string();
        if method.is_empty() || target.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("not an HTTP request line: {line:?}"),
            ));
        }
        let headers = read_headers(r)?;
        let body = read_body(r, &headers)?;
        Ok(Some(Request {
            method,
            target,
            headers,
            body,
        }))
    }

    pub fn write_to(&self, w: &mut impl Write) -> std::io::Result<()> {
        let mut head = format!("{} {} HTTP/1.1\r\n", self.method, self.target);
        let mut saw_length = false;
        for (k, v) in &self.headers {
            // Re-spelled by this writer, never passed through: a chunked body was decoded above.
            if k.eq_ignore_ascii_case("transfer-encoding")
                || k.eq_ignore_ascii_case("connection")
                || k.eq_ignore_ascii_case("content-length")
            {
                saw_length |= k.eq_ignore_ascii_case("content-length");
                continue;
            }
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        let _ = saw_length;
        if !self.body.is_empty() {
            head.push_str(&format!("Content-Length: {}\r\n", self.body.len()));
        }
        head.push_str("Connection: close\r\n\r\n");
        w.write_all(head.as_bytes())?;
        w.write_all(&self.body)?;
        w.flush()
    }
}

/// One response, as the engine wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn new(status: u16, body: Vec<u8>) -> Response {
        Response {
            status,
            headers: vec![("Content-Type".to_string(), "application/json".to_string())],
            body,
        }
    }

    pub fn ok(body: serde_json::Value) -> Response {
        Response::new(200, serde_json::to_vec(&body).unwrap_or_default())
    }

    /// The Engine API's own error shape: a client reads `message` and prints it.
    pub fn error(status: u16, message: &str) -> Response {
        Response::new(
            status,
            serde_json::to_vec(&serde_json::json!({ "message": message })).unwrap_or_default(),
        )
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn json_body(&self) -> Option<serde_json::Value> {
        serde_json::from_slice(&self.body).ok()
    }

    pub fn read(r: &mut impl BufRead) -> std::io::Result<Response> {
        let mut line = String::new();
        if r.read_line(&mut line)? == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "the engine closed without a status line",
            ));
        }
        let line = line.trim_end_matches(['\r', '\n']).to_string();
        let mut parts = line.split_whitespace();
        let _version = parts.next().unwrap_or_default();
        let status: u16 = parts.next().and_then(|s| s.parse().ok()).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("not an HTTP status line: {line:?}"),
            )
        })?;
        let headers = read_headers(r)?;
        let body = read_body(r, &headers)?;
        Ok(Response {
            status,
            headers,
            body,
        })
    }

    pub fn write_to(&self, w: &mut impl Write) -> std::io::Result<()> {
        let mut head = format!("HTTP/1.1 {} OK\r\n", self.status);
        for (k, v) in &self.headers {
            if k.eq_ignore_ascii_case("transfer-encoding")
                || k.eq_ignore_ascii_case("connection")
                || k.eq_ignore_ascii_case("content-length")
            {
                continue;
            }
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        head.push_str(&format!("Content-Length: {}\r\n", self.body.len()));
        head.push_str("Connection: close\r\n\r\n");
        w.write_all(head.as_bytes())?;
        w.write_all(&self.body)?;
        w.flush()
    }
}

fn read_headers(r: &mut impl BufRead) -> std::io::Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    loop {
        let mut line = String::new();
        if r.read_line(&mut line)? == 0 {
            return Ok(out);
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            return Ok(out);
        }
        if let Some((k, v)) = line.split_once(':') {
            out.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

fn read_body(r: &mut impl BufRead, headers: &[(String, String)]) -> std::io::Result<Vec<u8>> {
    if header(headers, "transfer-encoding").is_some_and(|v| v.eq_ignore_ascii_case("chunked")) {
        let mut out = Vec::new();
        loop {
            let mut size = String::new();
            if r.read_line(&mut size)? == 0 {
                return Ok(out);
            }
            let size = size.trim();
            let n = usize::from_str_radix(size.split(';').next().unwrap_or(size), 16).map_err(
                |_| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("chunk size {size:?} is not hex"),
                    )
                },
            )?;
            if n == 0 {
                // The trailer, to the blank line.
                loop {
                    let mut t = String::new();
                    if r.read_line(&mut t)? == 0 || t.trim_end_matches(['\r', '\n']).is_empty() {
                        break;
                    }
                }
                return Ok(out);
            }
            let mut chunk = vec![0u8; n];
            r.read_exact(&mut chunk)?;
            out.extend_from_slice(&chunk);
            let mut crlf = String::new();
            r.read_line(&mut crlf)?;
        }
    }
    match header(headers, "content-length").and_then(|v| v.parse::<usize>().ok()) {
        Some(n) if n > 0 => {
            let mut body = vec![0u8; n];
            r.read_exact(&mut body)?;
            Ok(body)
        }
        _ => Ok(Vec::new()),
    }
}

/// One round trip to the engine on `socket`, bounded so a wedged engine cannot hold a reaper.
pub fn call(socket: &Path, req: &Request, bound: Duration) -> Result<Response, String> {
    let stream = std::os::unix::net::UnixStream::connect(socket)
        .map_err(|e| format!("{}: {e}", socket.display()))?;
    stream
        .set_read_timeout(Some(bound))
        .and_then(|_| stream.set_write_timeout(Some(bound)))
        .map_err(|e| format!("{}: {e}", socket.display()))?;
    let mut write = &stream;
    req.write_to(&mut write)
        .map_err(|e| format!("{}: writing the request: {e}", socket.display()))?;
    let mut reader = BufReader::new(&stream);
    Response::read(&mut reader)
        .map_err(|e| format!("{}: reading the answer: {e}", socket.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_with_a_length_round_trips() {
        let mut req = Request::get("/v1.44/info");
        req.set_header("Host", "docker");
        let mut buf = Vec::new();
        req.write_to(&mut buf).expect("writes");
        let back = Request::read(&mut &buf[..]).expect("reads").expect("one");
        assert_eq!(back.method, "GET");
        assert_eq!(back.path(), "/v1.44/info");
        assert_eq!(back.header("host"), Some("docker"));
    }

    #[test]
    fn a_chunked_body_is_decoded_and_re_spelled_with_a_length() {
        let raw = "POST /v1.44/build?t=x HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n\
                   5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n";
        let req = Request::read(&mut &raw.as_bytes()[..])
            .expect("reads")
            .expect("one");
        assert_eq!(req.body, b"hello world");
        let mut buf = Vec::new();
        req.write_to(&mut buf).expect("writes");
        let text = String::from_utf8_lossy(&buf);
        assert!(!text.to_ascii_lowercase().contains("chunked"), "{text}");
        assert!(text.contains("Content-Length: 11"), "{text}");
    }

    #[test]
    fn a_chunked_answer_is_decoded() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n\
                    7\r\n{\"a\":1}\r\n0\r\n\r\n";
        let res = Response::read(&mut &raw[..]).expect("reads");
        assert_eq!(res.status, 200);
        assert_eq!(res.body, b"{\"a\":1}");
    }

    #[test]
    fn the_query_survives_and_the_path_does_not_carry_it() {
        let mut req = Request::new("POST", "/v1.44/containers/create?name=web");
        req.set_header("X", "1");
        assert_eq!(req.path(), "/v1.44/containers/create");
        assert_eq!(req.query(), "name=web");
        let renamed = req.with_target(format!("{}?name=lb-1-web", req.path()));
        assert_eq!(renamed.query(), "name=lb-1-web");
        assert_eq!(renamed.header("x"), Some("1"));
    }
}
