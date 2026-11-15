//! The two transports: a child's stdio, and streamable HTTP over the shared blocking client.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio as Piped};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::{ServerSpec, Transport};

#[derive(Debug)]
pub(crate) enum WireError {
    /// The server is gone for good: exited, or its pipe closed.
    Gone(String),
    Timeout(Duration),
    Other(String),
}

impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WireError::Gone(s) | WireError::Other(s) => f.write_str(s),
            WireError::Timeout(d) => write!(f, "no answer within {}s", d.as_secs()),
        }
    }
}

pub(crate) trait Wire: Send {
    /// Send one request and wait for the response with the same id.
    fn request(&mut self, id: u64, msg: &Value, timeout: Duration) -> Result<Value, WireError>;
    fn notify(&mut self, msg: &Value) -> Result<(), WireError>;
}

pub(crate) fn open(spec: &ServerSpec) -> Result<Box<dyn Wire>, String> {
    match &spec.transport {
        Transport::Stdio { command, args, env } => {
            Ok(Box::new(StdioWire::spawn(command, args, env)?))
        }
        Transport::Http { url, headers } => Ok(Box::new(HttpWire::new(url, headers)?)),
    }
}

/// Bytes of a server's stderr kept for the report when it dies.
const STDERR_TAIL: usize = 2048;

struct StdioWire {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    stderr: Arc<Mutex<Vec<u8>>>,
}

impl StdioWire {
    fn spawn(
        command: &str,
        args: &[String],
        env: &std::collections::BTreeMap<String, String>,
    ) -> Result<StdioWire, String> {
        let mut child = Command::new(command)
            .args(args)
            .envs(env)
            .stdin(Piped::piped())
            .stdout(Piped::piped())
            .stderr(Piped::piped())
            .spawn()
            .map_err(|e| format!("`{command}` did not start: {e}"))?;
        let stdin = child.stdin.take().ok_or("no stdin pipe")?;
        let stdout = child.stdout.take().ok_or("no stdout pipe")?;
        let mut err = child.stderr.take().ok_or("no stderr pipe")?;
        let (tx, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let tail = stderr.clone();
        // Drained so a chatty server cannot block on a full pipe; only the tail is kept.
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = err.read(&mut buf) {
                if n == 0 {
                    break;
                }
                let mut t = tail.lock().unwrap_or_else(|e| e.into_inner());
                t.extend_from_slice(&buf[..n]);
                let over = t.len().saturating_sub(STDERR_TAIL);
                t.drain(..over);
            }
        });
        Ok(StdioWire {
            child,
            stdin,
            lines,
            stderr,
        })
    }

    fn gone(&mut self) -> WireError {
        // The pipe closing races the exit; a short wait lets the status be named.
        let deadline = Instant::now() + Duration::from_millis(500);
        let status = loop {
            match self.child.try_wait() {
                Ok(Some(s)) => break Some(s),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                _ => break None,
            }
        };
        let tail = {
            let t = self.stderr.lock().unwrap_or_else(|e| e.into_inner());
            String::from_utf8_lossy(&t).trim().to_string()
        };
        let mut why = match status {
            Some(s) => format!("the server exited ({s})"),
            None => "the server closed its stdout".to_string(),
        };
        if !tail.is_empty() {
            why.push_str(&format!("; its stderr ends: {tail}"));
        }
        WireError::Gone(why)
    }

    fn write(&mut self, msg: &Value) -> Result<(), WireError> {
        let mut line = msg.to_string();
        line.push('\n');
        if self
            .stdin
            .write_all(line.as_bytes())
            .and_then(|_| self.stdin.flush())
            .is_err()
        {
            return Err(self.gone());
        }
        Ok(())
    }

    /// Answer a request the server sent us. `ping` is the only one this client serves.
    fn answer(&mut self, req: &Value) -> Result<(), WireError> {
        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let reply = if req.get("method").and_then(Value::as_str) == Some("ping") {
            json!({"jsonrpc": "2.0", "id": id, "result": {}})
        } else {
            json!({"jsonrpc": "2.0", "id": id,
                   "error": {"code": -32601, "message": "this client offers no such method"}})
        };
        self.write(&reply)
    }
}

impl Wire for StdioWire {
    fn request(&mut self, id: u64, msg: &Value, timeout: Duration) -> Result<Value, WireError> {
        self.write(msg)?;
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(line) => {
                    // A line that is not JSON is the server's log on the wrong stream.
                    let Ok(v) = serde_json::from_str::<Value>(&line) else {
                        continue;
                    };
                    if v.get("method").is_some() {
                        if v.get("id").is_some() {
                            self.answer(&v)?;
                        }
                        continue;
                    }
                    if v.get("id").and_then(Value::as_u64) == Some(id) {
                        return Ok(v);
                    }
                }
                Err(RecvTimeoutError::Timeout) => return Err(WireError::Timeout(timeout)),
                Err(RecvTimeoutError::Disconnected) => return Err(self.gone()),
            }
        }
    }

    fn notify(&mut self, msg: &Value) -> Result<(), WireError> {
        self.write(msg)
    }
}

impl Drop for StdioWire {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct HttpWire {
    host: String,
    port: u16,
    path: String,
    headers: Vec<(String, String)>,
    session: Option<String>,
}

impl HttpWire {
    fn new(
        url: &str,
        headers: &std::collections::BTreeMap<String, String>,
    ) -> Result<Self, String> {
        if url.starts_with("https://") {
            return Err(format!(
                "`{url}`: https needs TLS and this build's HTTP client has none; use a stdio \
                 server or a local http:// proxy"
            ));
        }
        let rest = url
            .strip_prefix("http://")
            .ok_or_else(|| format!("`{url}` is not an http:// URL"))?;
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], rest[i..].to_string()),
            None => (rest, "/".to_string()),
        };
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) => (
                h.to_string(),
                p.parse().map_err(|e| format!("`{url}`: port `{p}`: {e}"))?,
            ),
            None => (authority.to_string(), 80),
        };
        if host.is_empty() {
            return Err(format!("`{url}` has no host"));
        }
        Ok(HttpWire {
            host,
            port,
            path,
            headers: headers
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            session: None,
        })
    }

    fn post(&mut self, msg: &Value, timeout: Duration) -> Result<letibot_http::Body, WireError> {
        let mut endpoint = letibot_http::Endpoint::new(self.host.clone(), self.port);
        endpoint.read_timeout = timeout;
        let body = msg.to_string();
        let mut owned: Vec<(String, String)> = vec![
            ("Content-Type".into(), "application/json".into()),
            (
                "Accept".into(),
                "application/json, text/event-stream".into(),
            ),
            (
                "MCP-Protocol-Version".into(),
                crate::PROTOCOL_VERSION.into(),
            ),
        ];
        if let Some(s) = &self.session {
            owned.push(("Mcp-Session-Id".into(), s.clone()));
        }
        owned.extend(self.headers.iter().cloned());
        let headers: Vec<(&str, &str)> = owned
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let req = letibot_http::Request {
            method: "POST",
            path: &self.path,
            headers: &headers,
            body: Some(&body),
        };
        let res = letibot_http::send(&endpoint, req).map_err(|e| match e {
            letibot_http::HttpError::Io(io) if io.kind() == std::io::ErrorKind::WouldBlock => {
                WireError::Timeout(timeout)
            }
            letibot_http::HttpError::Io(io) if io.kind() == std::io::ErrorKind::TimedOut => {
                WireError::Timeout(timeout)
            }
            other => WireError::Other(other.to_string()),
        })?;
        if let Some(s) = res.header("mcp-session-id") {
            self.session = Some(s.to_string());
        }
        Ok(res)
    }
}

impl Wire for HttpWire {
    fn request(&mut self, id: u64, msg: &Value, timeout: Duration) -> Result<Value, WireError> {
        let res = self.post(msg, timeout)?;
        let sse = res
            .header("content-type")
            .is_some_and(|c| c.contains("text/event-stream"));
        if !sse {
            let text = res
                .read_to_string()
                .map_err(|e| WireError::Other(e.to_string()))?;
            return serde_json::from_str(&text)
                .map_err(|e| WireError::Other(format!("not a JSON-RPC answer ({e}): {text}")));
        }
        let mut found = None;
        res.for_each_event(|data| {
            if let Ok(v) = serde_json::from_str::<Value>(data)
                && v.get("method").is_none()
                && v.get("id").and_then(Value::as_u64) == Some(id)
            {
                found = Some(v);
                return Ok(letibot_http::Flow::Stop);
            }
            Ok(letibot_http::Flow::Continue)
        })
        .map_err(|e| WireError::Other(e.to_string()))?;
        found.ok_or_else(|| WireError::Other(format!("the stream ended without an answer to {id}")))
    }

    fn notify(&mut self, msg: &Value) -> Result<(), WireError> {
        let res = self.post(msg, Duration::from_secs(10))?;
        let _ = res.read_to_string();
        Ok(())
    }
}
