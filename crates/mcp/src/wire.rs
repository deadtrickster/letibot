//! The two transports: a child's stdio, and streamable HTTP over ureq with rustls.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio as Piped};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel, sync_channel};
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
    /// The methods of the notifications seen since the last call, pending ones drained first.
    fn take_notices(&mut self) -> Vec<String>;
    /// The child's pid, for a stdio server.
    fn pid(&self) -> Option<u32> {
        None
    }
}

/// The one notification acted on: the server's tool list changed, so it is listed again.
pub(crate) const TOOLS_CHANGED: &str = "notifications/tools/list_changed";

pub(crate) fn open(spec: &ServerSpec) -> Result<Box<dyn Wire>, String> {
    match &spec.transport {
        Transport::Stdio {
            command,
            args,
            env,
            cwd,
        } => Ok(Box::new(StdioWire::spawn(
            command,
            args,
            env,
            cwd.as_deref(),
        )?)),
        Transport::Http { url, headers } => {
            Ok(Box::new(HttpWire::new(url, headers, spec.timeout)?))
        }
    }
}

/// The daemon's variables a stdio server is handed: where it runs, never a credential.
const KEPT_ENV: &[&str] = &[
    "PATH", "HOME", "USER", "LOGNAME", "SHELL", "TERM", "LANG", "LC_ALL", "LC_CTYPE", "TMPDIR",
];

/// The longest line or SSE event a server may send. Past it the server is dropped: a reader
/// that buffers whatever arrives lets one server OOM the daemon and every session in it.
pub(crate) const MAX_LINE_BYTES: usize = 8 << 20;

/// Lines read ahead of the requests waiting for them. Bounded, so a flood blocks the server on
/// its pipe instead of growing the daemon.
const READ_AHEAD: usize = 16;

/// One line of at most [`MAX_LINE_BYTES`], its newline stripped. `Ok(None)` is EOF.
fn bounded_line(r: &mut impl BufRead) -> Result<Option<String>, String> {
    let mut buf = Vec::new();
    let n = r
        .take(MAX_LINE_BYTES as u64 + 1)
        .read_until(b'\n', &mut buf)
        .map_err(|e| e.to_string())?;
    if n == 0 {
        return Ok(None);
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
    } else if buf.len() > MAX_LINE_BYTES {
        return Err(format!(
            "the server sent more than {MAX_LINE_BYTES} bytes with no newline"
        ));
    }
    Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
}

/// Bytes of a server's stderr kept for the report when it dies.
const STDERR_TAIL: usize = 2048;

struct StdioWire {
    child: Child,
    stdin: ChildStdin,
    /// A line, or why the reader stopped before EOF.
    lines: Receiver<Result<String, String>>,
    stderr: Arc<Mutex<Vec<u8>>>,
    /// Closed when the stderr drain reaches EOF, so a crash report waits for the last bytes.
    stderr_done: Receiver<()>,
    notices: Vec<String>,
}

impl StdioWire {
    fn spawn(
        command: &str,
        args: &[String],
        env: &std::collections::BTreeMap<String, String>,
        cwd: Option<&std::path::Path>,
    ) -> Result<StdioWire, String> {
        let mut cmd = Command::new(command);
        // Its own group, so the drop's `killpg` reaches what an `npx` wrapper forked.
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }
        // Cleared, then stated: the daemon's env carries provider keys. The kept names say where
        // a program runs, not who it acts for, and the spec's own env is what the operator gave.
        cmd.env_clear();
        for name in KEPT_ENV {
            if let Some(v) = std::env::var_os(name) {
                cmd.env(name, v);
            }
        }
        let mut child = cmd
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
        let (tx, lines) = sync_channel(READ_AHEAD);
        std::thread::spawn(move || {
            let mut stdout = BufReader::new(stdout);
            loop {
                match bounded_line(&mut stdout) {
                    Ok(Some(line)) => {
                        if tx.send(Ok(line)).is_err() {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(why) => {
                        let _ = tx.send(Err(why));
                        break;
                    }
                }
            }
        });
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let tail = stderr.clone();
        let (done, stderr_done) = channel::<()>();
        // Drained so a chatty server cannot block on a full pipe; only the tail is kept.
        std::thread::spawn(move || {
            let _done = done;
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
            stderr_done,
            notices: Vec::new(),
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
        // A grandchild holding the pipe open would keep it from EOF, hence the bound.
        let _ = self.stderr_done.recv_timeout(Duration::from_millis(500));
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

    /// A message that is not the answer waited for: a request is answered, a notice kept.
    fn aside(&mut self, v: &Value) -> Result<(), WireError> {
        if v.get("id").is_some() {
            return self.answer(v);
        }
        if let Some(m) = v.get("method").and_then(Value::as_str) {
            self.notices.push(m.to_string());
        }
        Ok(())
    }
}

impl Wire for StdioWire {
    fn request(&mut self, id: u64, msg: &Value, timeout: Duration) -> Result<Value, WireError> {
        self.write(msg)?;
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(Err(why)) => return Err(WireError::Gone(why)),
                Ok(Ok(line)) => {
                    // A line that is not JSON is the server's log on the wrong stream.
                    let Ok(v) = serde_json::from_str::<Value>(&line) else {
                        continue;
                    };
                    if v.get("method").is_some() {
                        self.aside(&v)?;
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

    fn take_notices(&mut self) -> Vec<String> {
        while let Ok(Ok(line)) = self.lines.try_recv() {
            if let Ok(v) = serde_json::from_str::<Value>(&line)
                && v.get("method").is_some()
            {
                // A dead pipe is found by the next request, which names the exit.
                let _ = self.aside(&v);
            }
        }
        std::mem::take(&mut self.notices)
    }

    fn pid(&self) -> Option<u32> {
        Some(self.child.id())
    }
}

impl Drop for StdioWire {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Ok(pgid) = libc::pid_t::try_from(self.child.id()) {
            // SAFETY: the group was made at spawn and is led by our unreaped child, so its id
            // cannot have been reused.
            unsafe { libc::killpg(pgid, libc::SIGKILL) };
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct HttpWire {
    url: String,
    agent: ureq::Agent,
    headers: Vec<(String, String)>,
    session: Option<String>,
    notices: Vec<String>,
}

impl HttpWire {
    fn new(
        url: &str,
        headers: &std::collections::BTreeMap<String, String>,
        timeout: Duration,
    ) -> Result<Self, String> {
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err(format!("`{url}` is not an http:// or https:// URL"));
        }
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(timeout))
            .build()
            .into();
        Ok(HttpWire {
            url: url.to_string(),
            agent,
            headers: headers
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            session: None,
            notices: Vec::new(),
        })
    }

    fn post(&mut self, msg: &Value) -> Result<ureq::http::Response<ureq::Body>, WireError> {
        let mut req = self
            .agent
            .post(&self.url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", crate::PROTOCOL_VERSION);
        if let Some(s) = &self.session {
            req = req.header("Mcp-Session-Id", s);
        }
        for (k, v) in &self.headers {
            req = req.header(k, v);
        }
        let res = req.send(msg.to_string()).map_err(|e| match e {
            ureq::Error::Timeout(_) => WireError::Timeout(Duration::ZERO),
            other => WireError::Other(other.to_string()),
        })?;
        let status = res.status().as_u16();
        if status == 404 && self.session.is_some() {
            // The spec's signal that the session is gone: only a new `initialize` serves.
            return Err(WireError::Gone(format!(
                "the server ended session {} (404)",
                self.session.as_deref().unwrap_or("")
            )));
        }
        if !(200..300).contains(&status) {
            let text = res.into_body().read_to_string().unwrap_or_default();
            let hint = if status == 401 {
                "; OAuth is not supported, set an `Authorization` header in `headers`"
            } else {
                ""
            };
            return Err(WireError::Other(format!(
                "HTTP {status}{hint}: {}",
                text.trim()
            )));
        }
        if let Some(s) = res
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            self.session = Some(s.to_string());
        }
        Ok(res)
    }
}

impl Wire for HttpWire {
    fn request(&mut self, id: u64, msg: &Value, timeout: Duration) -> Result<Value, WireError> {
        let res = self.post(msg).map_err(|e| match e {
            WireError::Timeout(_) => WireError::Timeout(timeout),
            e => e,
        })?;
        let sse = res
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|c| c.contains("text/event-stream"));
        let mut body = res.into_body();
        if !sse {
            let text = body
                .with_config()
                .limit(MAX_LINE_BYTES as u64)
                .read_to_string()
                .map_err(|e| WireError::Other(e.to_string()))?;
            return serde_json::from_str(&text)
                .map_err(|e| WireError::Other(format!("not a JSON-RPC answer ({e}): {text}")));
        }
        let mut data = String::new();
        let mut stream = BufReader::new(body.as_reader());
        while let Some(line) = bounded_line(&mut stream).map_err(WireError::Other)? {
            if let Some(d) = line.strip_prefix("data:") {
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(d.strip_prefix(' ').unwrap_or(d));
                if data.len() > MAX_LINE_BYTES {
                    return Err(WireError::Other(format!(
                        "an event of more than {MAX_LINE_BYTES} bytes"
                    )));
                }
                continue;
            }
            if !line.is_empty() || data.is_empty() {
                continue;
            }
            let event = std::mem::take(&mut data);
            let Ok(v) = serde_json::from_str::<Value>(&event) else {
                continue;
            };
            if let Some(m) = v.get("method").and_then(Value::as_str) {
                // A server request on this stream cannot be answered on it; only notices count.
                if v.get("id").is_none() {
                    self.notices.push(m.to_string());
                }
                continue;
            }
            if v.get("id").and_then(Value::as_u64) == Some(id) {
                return Ok(v);
            }
        }
        // A stream may end on its last event without the blank line.
        if let Ok(v) = serde_json::from_str::<Value>(&data)
            && v.get("id").and_then(Value::as_u64) == Some(id)
        {
            return Ok(v);
        }
        Err(WireError::Other(format!(
            "the stream ended without an answer to {id}"
        )))
    }

    fn notify(&mut self, msg: &Value) -> Result<(), WireError> {
        let res = self.post(msg)?;
        let _ = res.into_body().read_to_string();
        Ok(())
    }

    fn take_notices(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notices)
    }
}
