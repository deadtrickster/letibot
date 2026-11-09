//! `lsp`: run a language server and read the diagnostics for one file.
//! A minimal client: spawn the server, handshake, open the file, then take the first of a
//! `textDocument/diagnostic` answer or a `publishDiagnostics` push for it.
//! Best effort: a server that is not installed refuses by name rather than pretending.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::runtime::{Invocation, InvokeCtx, Tool};
use crate::schema::{Access, ToolSchema};

/// language -> argv of its server. A server that is not installed fails at the spawn.
#[derive(Debug, Clone)]
pub struct LspConfig {
    pub servers: BTreeMap<String, Vec<String>>,
    /// How long one call waits on the server in all, so a wedged server is a failure and not a
    /// turn that never ends.
    pub deadline: Duration,
}

impl Default for LspConfig {
    fn default() -> Self {
        let mut servers = BTreeMap::new();
        servers.insert("rust".into(), vec!["rust-analyzer".into()]);
        servers.insert("go".into(), vec!["gopls".into()]);
        servers.insert(
            "python".into(),
            vec!["pyright-langserver".into(), "--stdio".into()],
        );
        servers.insert(
            "typescript".into(),
            vec!["typescript-language-server".into(), "--stdio".into()],
        );
        LspConfig {
            servers,
            deadline: Duration::from_secs(60),
        }
    }
}

impl LspConfig {
    pub fn languages(&self) -> Vec<&str> {
        self.servers.keys().map(|s| s.as_str()).collect()
    }
}

/// A spawned server: its stdin, and its stdout parsed on a thread so every wait has a deadline.
struct Server {
    program: String,
    child: Child,
    stdin: ChildStdin,
    inbox: Receiver<Value>,
    deadline: Instant,
    budget: Duration,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

enum Stopped {
    Exited,
    TimedOut,
}

impl Server {
    fn next(&self) -> Result<Value, Stopped> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        self.inbox.recv_timeout(left).map_err(|e| match e {
            RecvTimeoutError::Timeout => Stopped::TimedOut,
            RecvTimeoutError::Disconnected => Stopped::Exited,
        })
    }

    fn send(&mut self, msg: &Value) -> Result<(), Stopped> {
        write_message(&mut self.stdin, msg).map_err(|_| Stopped::Exited)
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), Stopped> {
        self.send(&json!({"jsonrpc":"2.0","method":method,"params":params}))
    }

    fn ask(&mut self, id: u64, method: &str, params: Value) -> Result<(), Stopped> {
        self.send(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
    }

    /// A request from the server (progress tokens, capability registration) answered with
    /// null, since some servers hold their diagnostics until it is.
    fn answer_if_request(&mut self, msg: &Value) {
        if let (Some(id), Some(_)) = (msg.get("id"), msg.get("method")) {
            let _ = self.send(&json!({"jsonrpc":"2.0","id":id,"result":null}));
        }
    }

    fn why(&self, stopped: Stopped, what: &str) -> String {
        match stopped {
            Stopped::Exited => format!(
                "the language server `{}` exited before answering `{what}`",
                self.program
            ),
            Stopped::TimedOut => format!(
                "the language server `{}` did not answer `{what}` within {}s",
                self.program,
                self.budget.as_secs_f32()
            ),
        }
    }
}

fn write_message(w: &mut ChildStdin, msg: &Value) -> std::io::Result<()> {
    let body = serde_json::to_string(msg).expect("json");
    write!(w, "Content-Length: {}\r\n\r\n{}", body.len(), body)?;
    w.flush()
}

/// `None` only at end of stream. A body that is not JSON is `Null`, skipped by every reader.
fn read_message(r: &mut BufReader<ChildStdout>) -> Option<Value> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(rest) = line.strip_prefix("Content-Length:") {
            content_length = rest.trim().parse().ok();
        }
    }
    let Some(n) = content_length else {
        return Some(Value::Null);
    };
    let mut buf = vec![0u8; n];
    r.read_exact(&mut buf).ok()?;
    Some(serde_json::from_slice(&buf).unwrap_or(Value::Null))
}

fn spawn(language: &str, root: &str, config: &LspConfig) -> Result<Server, String> {
    let argv = config
        .servers
        .get(language)
        .ok_or_else(|| format!("no LSP server configured for `{language}`"))?;
    let Some((program, args)) = argv.split_first() else {
        return Err(format!(
            "the LSP server for `{language}` has an empty command"
        ));
    };
    let mut child = Command::new(program)
        .args(args)
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("could not start `{program}` (is it installed?): {e}"))?;
    let stdin = child.stdin.take().ok_or("no stdin")?;
    let mut stdout = BufReader::new(child.stdout.take().ok_or("no stdout")?);
    let (tx, inbox) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        while let Some(msg) = read_message(&mut stdout) {
            if tx.send(msg).is_err() {
                break;
            }
        }
    });
    Ok(Server {
        program: program.clone(),
        child,
        stdin,
        inbox,
        deadline: Instant::now() + config.deadline,
        budget: config.deadline,
    })
}

/// The diagnostics a `publishDiagnostics` push carries, when it is for `uri`.
fn pushed_for<'a>(msg: &'a Value, uri: &str) -> Option<&'a Vec<Value>> {
    if msg.get("method").and_then(|m| m.as_str()) != Some("textDocument/publishDiagnostics") {
        return None;
    }
    let params = msg.get("params")?;
    if params.get("uri").and_then(|u| u.as_str()) != Some(uri) {
        return None;
    }
    params.get("diagnostics").and_then(|d| d.as_array())
}

fn render(path: &str, item: &Value) -> String {
    let sev = match item.get("severity").and_then(|v| v.as_i64()) {
        Some(1) => "error",
        Some(2) => "warning",
        Some(3) => "info",
        Some(4) => "hint",
        _ => "?",
    };
    let line = item
        .pointer("/range/start/line")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let message = item.get("message").and_then(|v| v.as_str()).unwrap_or("?");
    format!("{path}:{}: {sev}: {message}", line + 1)
}

/// The diagnostics for one file, from a live server.
pub fn diagnostics(
    path: &str,
    language: &str,
    root: &str,
    content: &str,
    config: &LspConfig,
) -> Result<Vec<String>, String> {
    let mut s = spawn(language, root, config)?;

    let root_uri = format!("file://{}", root.trim_end_matches('/'));
    let initialize = json!({"processId": null, "rootUri": root_uri, "capabilities": {}});
    s.ask(1, "initialize", initialize)
        .map_err(|e| s.why(e, "initialize"))?;
    loop {
        let msg = s.next().map_err(|e| s.why(e, "initialize"))?;
        if msg.get("id").and_then(|v| v.as_u64()) == Some(1) && msg.get("method").is_none() {
            break;
        }
        s.answer_if_request(&msg);
    }

    let absolute = std::path::Path::new(root).join(path);
    let uri = format!("file://{}", absolute.display());
    let open = json!({"textDocument": {"uri": uri, "languageId": language, "version": 1, "text": content}});
    let items = s
        .notify("initialized", json!({}))
        .and_then(|_| s.notify("textDocument/didOpen", open))
        .and_then(|_| {
            s.ask(
                2,
                "textDocument/diagnostic",
                json!({"textDocument": {"uri": uri}}),
            )
        })
        .map_err(|e| s.why(e, "textDocument/didOpen"))
        .and_then(|_| wait_for_diagnostics(&mut s, &uri))?;

    let _ = s.ask(3, "shutdown", Value::Null);
    let _ = s.notify("exit", Value::Null);
    Ok(items.iter().map(|item| render(path, item)).collect())
}

/// The pull answer if the server has one, else its push. An empty push can be the server's
/// placeholder before analysis, so it ends the wait only once the pull has been refused.
fn wait_for_diagnostics(s: &mut Server, uri: &str) -> Result<Vec<Value>, String> {
    let mut pushed: Option<Vec<Value>> = None;
    let mut pull_refused = false;
    loop {
        let msg = match s.next() {
            Ok(msg) => msg,
            Err(e) => return pushed.ok_or_else(|| s.why(e, "textDocument/diagnostic")),
        };
        if let Some(items) = pushed_for(&msg, uri) {
            if !items.is_empty() || pull_refused {
                return Ok(items.clone());
            }
            pushed = Some(items.clone());
            continue;
        }
        if msg.get("id").and_then(|v| v.as_u64()) == Some(2) && msg.get("method").is_none() {
            if let Some(items) = msg.pointer("/result/items").and_then(|i| i.as_array()) {
                return Ok(items.clone());
            }
            if let Some(items) = pushed {
                return Ok(items);
            }
            pull_refused = true;
            continue;
        }
        s.answer_if_request(&msg);
    }
}

/// The `lsp` tool.
pub struct LspTool {
    config: std::sync::Arc<LspConfig>,
}

impl LspTool {
    pub fn new(config: std::sync::Arc<LspConfig>) -> Self {
        LspTool { config }
    }
}

impl Tool for LspTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "lsp",
            "Read compiler diagnostics for a file by running its language server. Give \
             `path` and `language`. The server is started per call and stopped when the \
             diagnostics are read, so this is for a one-off check, not a persistent \
             session.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "The file to check, relative to the workspace."},
                    "language": {"type": "string", "description": "rust, go, python or typescript."}
                },
                "required": ["path", "language"]
            }),
            Access::Read,
        )
    }

    fn invoke(&self, ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        let Some(path) = args.get("path").and_then(|v| v.as_str()) else {
            return Invocation::failed(
                "lsp needs a path",
                "call `lsp` with `path` set to the file to check.",
            );
        };
        let Some(language) = args.get("language").and_then(|v| v.as_str()) else {
            return Invocation::failed(
                "lsp needs a language",
                "call `lsp` with `language` set to rust, go, python or typescript.",
            );
        };
        let content = match ctx.backend.read(path) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(e) => {
                return Invocation::failed(
                    format!("could not read {path}: {e}"),
                    "check the path is inside the workspace and try again.",
                );
            }
        };
        // The workspace, not the backend's root: a language server rooted at `/` would
        // index the whole host instead of the project.
        let root = ctx
            .backend
            .workspace_path()
            .or_else(|| ctx.backend.root_path())
            .unwrap_or_default();
        match diagnostics(path, language, &root, &content, &self.config) {
            Ok(items) if items.is_empty() => Invocation::ok("no diagnostics."),
            Ok(items) => Invocation::ok(format!(
                "{} diagnostic(s):\n{}",
                items.len(),
                items.join("\n")
            )),
            Err(e) => Invocation::failed(
                e,
                "the language server refused; check the language name and that its server is installed.",
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_names_its_languages() {
        let c = LspConfig::default();
        assert!(c.languages().contains(&"rust"));
        assert!(c.languages().contains(&"go"));
    }

    #[test]
    fn an_empty_server_command_is_refused_not_indexed() {
        let mut c = LspConfig::default();
        c.servers.insert("rust".into(), Vec::new());
        let err = spawn("rust", ".", &c).err().expect("refused");
        assert!(err.contains("empty command"), "{err}");
    }

    // A server that only pushes, the way most do: `textDocument/diagnostic` is answered with
    // method-not-found, and the diagnostic arrives as a `publishDiagnostics` notification.
    const FAKE_SERVER: &str = r#"#!/usr/bin/env python3
import json, sys, time
if len(sys.argv) < 2:
    sys.exit(0)
mode = sys.argv[1]
inp, out = sys.stdin.buffer, sys.stdout.buffer
def recv():
    n = None
    while True:
        line = inp.readline()
        if not line:
            sys.exit(0)
        line = line.strip()
        if not line:
            break
        if line.lower().startswith(b"content-length:"):
            n = int(line.split(b":")[1])
    return json.loads(inp.read(n))
def send(msg):
    body = json.dumps(msg).encode()
    out.write(b"Content-Length: %d\r\n\r\n" % len(body) + body)
    out.flush()
while True:
    msg = recv()
    method = msg.get("method")
    if method == "initialize":
        if mode == "exit":
            sys.exit(3)
        if mode == "silent":
            time.sleep(30)
        send({"jsonrpc": "2.0", "id": msg["id"], "result": {"capabilities": {}}})
    elif method == "textDocument/didOpen":
        send({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": {
            "uri": msg["params"]["textDocument"]["uri"],
            "diagnostics": [{"range": {"start": {"line": 2, "character": 4},
                                       "end": {"line": 2, "character": 9}},
                             "severity": 1, "message": "cannot find value `nope`"}]}})
    elif "id" in msg:
        send({"jsonrpc": "2.0", "id": msg["id"],
              "error": {"code": -32601, "message": "method not found"}})
"#;

    struct Fixture {
        rt: crate::runtime::ToolRuntime,
        sink: crate::events::RecordingToolSink,
        _dir: crate::backend::tempdir::TempDir,
    }

    fn fixture(mode: &str) -> Fixture {
        let dir = crate::backend::tempdir::TempDir::new();
        std::fs::write(dir.path().join("main.rs"), "fn main() {\n\n    nope;\n}\n").unwrap();
        let stub = dir.path().join("fake-ls");
        crate::testing::write_stub(&stub, FAKE_SERVER);
        let mut config = LspConfig {
            deadline: Duration::from_secs(2),
            ..LspConfig::default()
        };
        config.servers.insert(
            "rust".into(),
            vec![stub.to_string_lossy().into_owned(), mode.into()],
        );
        let mut registry = crate::runtime::Registry::new();
        registry
            .register(Box::new(LspTool::new(std::sync::Arc::new(config))))
            .expect("lsp registers");
        let backend = crate::backend::HostBackend::new(dir.path()).expect("root");
        let rt = crate::runtime::ToolRuntime::new(registry, Box::new(backend))
            .with_gate(crate::testing::allow_all());
        Fixture {
            rt,
            sink: crate::events::RecordingToolSink::new(),
            _dir: dir,
        }
    }

    fn call(f: &mut Fixture) -> crate::result::ToolResult {
        let call = letibot_transcript::ToolCall {
            id: "call_1".into(),
            name: "lsp".into(),
            arguments: r#"{"path":"main.rs","language":"rust"}"#.into(),
        };
        f.rt.invoke("turn_1", &call, &mut f.sink)
    }

    #[test]
    fn a_pushed_diagnostic_comes_back_with_its_file_line_and_message() {
        let mut f = fixture("push");
        let r = call(&mut f);
        assert!(
            matches!(r.outcome, letibot_transcript::ToolOutcome::Ok),
            "{:?}: {}",
            r.outcome,
            r.payload
        );
        assert!(r.payload.contains("1 diagnostic(s)"), "{}", r.payload);
        assert!(
            r.payload
                .contains("main.rs:3: error: cannot find value `nope`"),
            "{}",
            r.payload
        );
    }

    fn failure(r: &crate::result::ToolResult) -> String {
        match &r.outcome {
            letibot_transcript::ToolOutcome::Failed { reason } => reason.clone(),
            other => panic!("expected a failure, got {other:?}: {}", r.payload),
        }
    }

    #[test]
    fn a_server_that_exits_before_answering_is_reported_not_waited_on() {
        let mut f = fixture("exit");
        let started = std::time::Instant::now();
        let reason = failure(&call(&mut f));
        assert!(reason.contains("exited"), "{reason}");
        assert!(reason.contains("initialize"), "{reason}");
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn a_server_that_never_answers_is_stopped_at_the_deadline() {
        let mut f = fixture("silent");
        let started = std::time::Instant::now();
        let reason = failure(&call(&mut f));
        assert!(reason.contains("did not answer"), "{reason}");
        assert!(reason.contains("initialize"), "{reason}");
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }
}
