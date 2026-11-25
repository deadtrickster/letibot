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

pub(crate) fn open(
    spec: &ServerSpec,
    spawn: &dyn letibot_tools::exec::Spawn,
) -> Result<Box<dyn Wire>, String> {
    match &spec.transport {
        Transport::Stdio {
            command,
            args,
            env,
            cwd,
        } => Ok(Box::new(StdioWire::spawn(
            &letibot_tools::exec::Want {
                command,
                args,
                env,
                cwd: cwd.as_deref(),
            },
            spawn,
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

/// The next SSE event's `data`, its `data:` lines joined with a newline. A line and a whole event
/// are each capped at [`MAX_LINE_BYTES`]: a reader that buffers whatever arrives lets one server
/// OOM the daemon and every session in it. `Ok(None)` is the stream ending.
fn next_event(r: &mut impl BufRead, data: &mut String) -> Result<Option<String>, WireError> {
    loop {
        let Some(line) = bounded_line(r).map_err(WireError::Other)? else {
            // A stream may end on its last event without the blank line.
            if data.is_empty() {
                return Ok(None);
            }
            return Ok(Some(std::mem::take(data)));
        };
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
        } else if line.is_empty() && !data.is_empty() {
            return Ok(Some(std::mem::take(data)));
        }
    }
}

/// The answer to a request a server sent us: `ping` is served, and every other method is refused
/// by code, because a server left waiting on a client that cannot serve it hangs its own call.
fn answer_to(req: &Value) -> Value {
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    if req.get("method").and_then(Value::as_str) == Some("ping") {
        json!({"jsonrpc": "2.0", "id": id, "result": {}})
    } else {
        json!({"jsonrpc": "2.0", "id": id,
               "error": {"code": -32601, "message": "this client offers no such method"}})
    }
}

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
        want: &letibot_tools::exec::Want<'_>,
        spawn: &dyn letibot_tools::exec::Spawn,
    ) -> Result<StdioWire, String> {
        let launch = spawn.launch(want)?;
        let Some(program) = launch.argv.first() else {
            return Err(format!(
                "`{}` was launched with no program to run",
                want.command
            ));
        };
        let mut cmd = Command::new(program);
        // Its own group, so the drop's `killpg` reaches what an `npx` wrapper forked - and, under
        // a boundary, the helper's whole tree.
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
        if let Some(dir) = &launch.cwd {
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
        // A pair a boundary sealed rides the descriptor its argv names, so no argv carries a value
        // `ps` would read. The pipe is dropped only after the child has it.
        #[cfg(target_os = "linux")]
        let sealed = letibot_tools::exec::confine::hand_secrets(&mut cmd, &launch.secrets)
            .map_err(|e| format!("`{}` could not be handed its secrets: {e}", want.command))?;
        // Without one there is no way to hand a sealed value over, and starting the server with the
        // value missing is the silent failure this refuses.
        #[cfg(not(target_os = "linux"))]
        if !launch.secrets.is_empty() {
            return Err(format!(
                "`{}` runs under a boundary that seals {} environment value(s), and this platform \
                 has no descriptor to hand one over",
                want.command,
                launch.secrets.len()
            ));
        }
        let mut child = cmd
            .args(&launch.argv[1..])
            .envs(launch.env)
            .stdin(Piped::piped())
            .stdout(Piped::piped())
            .stderr(Piped::piped())
            .spawn()
            .map_err(|e| format!("`{}` did not start: {e}", want.command))?;
        #[cfg(target_os = "linux")]
        drop(sealed);
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
        let reply = answer_to(req);
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

/// How long the GET stream's socket may stay silent before its reader gives up: it lives as long as
/// the session, so a request's deadline would end it after the first quiet minute.
const STREAM_LIFETIME: Duration = Duration::from_secs(3600);

struct HttpWire {
    url: String,
    agent: ureq::Agent,
    /// The GET stream's own agent. Its socket is held for the session's life, so it must not be a
    /// pooled one a POST is waiting to borrow.
    stream_agent: ureq::Agent,
    headers: Vec<(String, String)>,
    session: Option<String>,
    notices: Vec<String>,
    /// The stream is legal only after `initialize`.
    ready: bool,
    /// The methods the GET stream's reader has seen. Dropping this ends the reader.
    stream: Option<Receiver<String>>,
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
        Ok(HttpWire {
            url: url.to_string(),
            agent: wire_agent(timeout),
            stream_agent: wire_agent(STREAM_LIFETIME),
            headers: headers
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            session: None,
            notices: Vec::new(),
            ready: false,
            stream: None,
        })
    }

    /// Open the GET stream, once per wire: a server's own notices and requests arrive on it. It is
    /// opened on its own thread, because a server that accepts the socket and says nothing must not
    /// hold up a call - and it is not reopened, so a server that refuses it (405) is asked once.
    fn open_stream(&mut self) {
        if !self.ready || self.stream.is_some() {
            return;
        }
        let (tx, rx) = channel();
        let (agent, url, headers, session) = (
            self.stream_agent.clone(),
            self.url.clone(),
            self.headers.clone(),
            self.session.clone(),
        );
        std::thread::spawn(move || stream_reader(agent, url, headers, session, tx));
        self.stream = Some(rx);
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
        // Nothing a server says is addressed to it before `initialize`, so the GET stream waits.
        if msg.get("method").and_then(Value::as_str) == Some("initialize") {
            self.ready = true;
        }
        Ok(res)
    }
}

/// The agent both halves of the wire use. `http_status_as_error(false)` so a 4xx is read and named.
fn wire_agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(timeout))
        .build()
        .into()
}

/// The GET stream's own thread: a notice reaches `take_notices`, and a server's request is answered
/// with a POST from here, because the stream carries no channel back and nobody else is reading it.
/// A server that answers it 405, or not at all, offers no stream - which the spec allows.
fn stream_reader(
    agent: ureq::Agent,
    url: String,
    headers: Vec<(String, String)>,
    session: Option<String>,
    notices: std::sync::mpsc::Sender<String>,
) {
    let mut req = agent
        .get(&url)
        .header("Accept", "text/event-stream")
        .header("MCP-Protocol-Version", crate::PROTOCOL_VERSION);
    if let Some(s) = &session {
        req = req.header("Mcp-Session-Id", s);
    }
    for (k, v) in &headers {
        req = req.header(k, v);
    }
    let Ok(res) = req.call() else {
        return;
    };
    let sse = res
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|c| c.contains("text/event-stream"));
    if !(200..300).contains(&res.status().as_u16()) || !sse {
        return;
    }
    let mut body = res.into_body();
    let mut stream = BufReader::new(body.as_reader());
    let mut data = String::new();
    loop {
        let Ok(Some(event)) = next_event(&mut stream, &mut data) else {
            // The server ended the stream, or sent past the caps: either way there is nothing left
            // to read, and a stream is optional, so the calls go on without it.
            return;
        };
        let Ok(v) = serde_json::from_str::<Value>(&event) else {
            continue;
        };
        // One event may be a batch, which is the one shape a server may use for either.
        let messages = match v {
            Value::Array(items) => items,
            other => vec![other],
        };
        for m in &messages {
            let Some(method) = m.get("method").and_then(Value::as_str) else {
                continue;
            };
            if m.get("id").is_some() {
                post_reply(&agent, &url, &headers, session.as_deref(), &answer_to(m));
            } else if notices.send(method.to_string()).is_err() {
                return;
            }
        }
    }
}

/// A server's request is answered by a POST: the stream has no channel for it, and the server's own
/// call waits until the answer arrives.
fn post_reply(
    agent: &ureq::Agent,
    url: &str,
    headers: &[(String, String)],
    session: Option<&str>,
    reply: &Value,
) {
    let mut req = agent
        .post(url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .header("MCP-Protocol-Version", crate::PROTOCOL_VERSION);
    if let Some(s) = session {
        req = req.header("Mcp-Session-Id", s);
    }
    for (k, v) in headers {
        req = req.header(k, v);
    }
    if let Ok(res) = req.send(reply.to_string()) {
        let _ = res
            .into_body()
            .with_config()
            .limit(MAX_LINE_BYTES as u64)
            .read_to_string();
    }
}

impl Wire for HttpWire {
    fn request(&mut self, id: u64, msg: &Value, timeout: Duration) -> Result<Value, WireError> {
        self.open_stream();
        let res = self.post(msg).map_err(|e| match e {
            WireError::Timeout(_) => WireError::Timeout(timeout),
            e => e,
        })?;
        let sse = res
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|c| c.contains("text/event-stream"));
        // A finite stream is drained after its answer, so the connection goes back to the pool.
        let finite = res.headers().contains_key("content-length");
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
        while let Some(event) = next_event(&mut stream, &mut data)? {
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
                if finite {
                    let _ = std::io::copy(
                        &mut (&mut stream).take(MAX_LINE_BYTES as u64),
                        &mut std::io::sink(),
                    );
                }
                return Ok(v);
            }
        }
        Err(WireError::Other(format!(
            "the stream ended without an answer to {id}"
        )))
    }

    fn notify(&mut self, msg: &Value) -> Result<(), WireError> {
        self.open_stream();
        let res = self.post(msg)?;
        let _ = res.into_body().read_to_string();
        Ok(())
    }

    fn take_notices(&mut self) -> Vec<String> {
        self.open_stream();
        if let Some(rx) = &self.stream {
            while let Ok(m) = rx.try_recv() {
                self.notices.push(m);
            }
        }
        std::mem::take(&mut self.notices)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn next(r: &mut impl BufRead) -> Result<Option<String>, WireError> {
        let mut data = String::new();
        next_event(r, &mut data)
    }

    #[test]
    fn an_event_joins_its_data_lines_and_the_last_one_needs_no_blank_line() {
        let mut r = BufReader::new("data: a\ndata: b\n\ndata: c\n".as_bytes());
        assert_eq!(next(&mut r).unwrap().as_deref(), Some("a\nb"));
        // A stream may end on its last event without the blank line.
        assert_eq!(next(&mut r).unwrap().as_deref(), Some("c"));
        assert_eq!(next(&mut r).unwrap(), None);
    }

    /// I65's two caps, on the one reader both streams use: a line nobody ended, and an event built
    /// out of short lines. Either would let one server grow the daemon past its memory.
    #[test]
    fn a_line_and_a_whole_event_are_capped() {
        let big = format!("data: x{}", "y".repeat(MAX_LINE_BYTES + 1));
        let err = next(&mut BufReader::new(big.as_bytes())).unwrap_err();
        assert!(format!("{err}").contains("no newline"), "{err}");

        let line = format!("data: {}\n", "z".repeat(1 << 20));
        let many = line.repeat(9);
        let err = next(&mut BufReader::new(many.as_bytes())).unwrap_err();
        assert!(format!("{err}").contains("more than"), "{err}");
    }

    #[test]
    fn a_ping_is_answered_and_every_other_method_is_refused_by_code() {
        let ping = answer_to(&json!({"jsonrpc": "2.0", "id": 7, "method": "ping"}));
        assert_eq!(ping, json!({"jsonrpc": "2.0", "id": 7, "result": {}}));
        let other = answer_to(&json!({"jsonrpc": "2.0", "id": "srv-2", "method": "x/y"}));
        assert_eq!(other["error"]["code"], json!(-32601));
        assert_eq!(other["id"], json!("srv-2"));
    }
}
