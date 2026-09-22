//! `lsp` — run a language server and read diagnostics.
//!
//! A minimal LSP client: spawn the server for a language, do the handshake, open a
//! file, pull `textDocument/diagnostic`, and return the diagnostics. Best-effort —
//! a server that is not installed refuses by name rather than pretending.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

use crate::runtime::{Invocation, InvokeCtx, Tool};
use crate::schema::{Access, ToolSchema};

/// language -> argv of its server. Best-effort; `which` decides whether it runs.
#[derive(Debug, Clone)]
pub struct LspConfig {
    pub servers: BTreeMap<String, Vec<String>>,
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
        LspConfig { servers }
    }
}

impl LspConfig {
    pub fn languages(&self) -> Vec<&str> {
        self.servers.keys().map(|s| s.as_str()).collect()
    }
}

/// A spawned server and its stdio ends.
struct Server {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

fn write_message(w: &mut ChildStdin, msg: &Value) -> std::io::Result<()> {
    let body = serde_json::to_string(msg).expect("json");
    write!(w, "Content-Length: {}\r\n\r\n{}", body.len(), body)?;
    w.flush()
}

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
    let n = content_length?;
    let mut buf = vec![0u8; n];
    r.read_exact(&mut buf).ok()?;
    serde_json::from_slice(&buf).ok()
}

fn request(
    stdin: &mut ChildStdin,
    stdout: &mut BufReader<ChildStdout>,
    id: u64,
    method: &str,
    params: Value,
) -> Option<Value> {
    write_message(
        stdin,
        &json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}),
    )
    .ok()?;
    loop {
        let msg = read_message(stdout)?;
        if msg.get("id").and_then(|v| v.as_u64()) == Some(id) {
            return Some(msg);
        }
    }
}

fn notify(stdin: &mut ChildStdin, method: &str, params: Value) -> std::io::Result<()> {
    write_message(
        stdin,
        &json!({"jsonrpc":"2.0","method":method,"params":params}),
    )
}

fn spawn(language: &str, root: &str, config: &LspConfig) -> Result<Server, String> {
    let argv = config
        .servers
        .get(language)
        .ok_or_else(|| format!("no LSP server configured for `{language}`"))?;
    let program = &argv[0];
    let mut child = Command::new(program)
        .args(&argv[1..])
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("could not start `{program}` (is it installed?): {e}"))?;
    let stdin = child.stdin.take().ok_or("no stdin")?;
    let stdout = BufReader::new(child.stdout.take().ok_or("no stdout")?);
    Ok(Server {
        child,
        stdin,
        stdout,
    })
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
    request(
        &mut s.stdin,
        &mut s.stdout,
        1,
        "initialize",
        json!({
            "processId": null,
            "rootUri": root_uri,
            "capabilities": {},
        }),
    )
    .ok_or("initialize got no response")?;
    notify(&mut s.stdin, "initialized", json!({})).map_err(|e| e.to_string())?;

    let uri = format!("file://{path}");
    notify(
        &mut s.stdin,
        "textDocument/didOpen",
        json!({"textDocument": {"uri": uri, "languageId": language, "version": 1, "text": content}}),
    )
    .map_err(|e| e.to_string())?;

    let resp = request(
        &mut s.stdin,
        &mut s.stdout,
        2,
        "textDocument/diagnostic",
        json!({"textDocument": {"uri": uri}}),
    )
    .ok_or("textDocument/diagnostic got no response")?;

    let _ = notify(&mut s.stdin, "shutdown", json!({}));
    let _ = notify(&mut s.stdin, "exit", json!({}));
    let _ = s.child.kill();

    let items = resp
        .get("result")
        .and_then(|r| r.get("items"))
        .and_then(|i| i.as_array())
        .ok_or("diagnostic response had no items")?;

    let mut out = Vec::new();
    for item in items {
        let severity = item.get("severity").and_then(|v| v.as_i64()).unwrap_or(0);
        let sev = match severity {
            1 => "error",
            2 => "warning",
            3 => "info",
            4 => "hint",
            _ => "?",
        };
        let range = item
            .get("range")
            .and_then(|r| r.get("start"))
            .and_then(|s| s.get("line"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let message = item.get("message").and_then(|v| v.as_str()).unwrap_or("?");
        out.push(format!("{}:{} {}: {message}", range + 1, sev, ""));
    }
    Ok(out)
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
        // **The workspace, not the backend's root** (R18): a language server rooted at
        // `/` is a server that indexes the whole host instead of the project.
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
}
