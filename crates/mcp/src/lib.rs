//! An MCP client for `letibot-tools`' catalogue seam: stdio and plain-HTTP JSON-RPC, blocking,
//! one lock per server. Servers start in parallel at [`Catalog::start`]; one that fails is kept
//! as a [`Failure`] and the rest serve anyway.

mod wire;

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

use letibot_tools::builtins::external::mcp::{
    McpCallResult, McpCatalog, McpError, McpServer, McpToolSpec,
};
use serde_json::{Value, json};

use wire::{Wire, WireError};

/// The protocol revision this client speaks first; a server answering an older one is accepted.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// A call's default deadline, dsh's 60 s.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// How one server is reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transport {
    /// A child process speaking newline-delimited JSON-RPC on stdin and stdout.
    Stdio {
        command: String,
        args: Vec<String>,
        env: BTreeMap<String, String>,
    },
    /// Streamable HTTP over `http://`. No TLS: the shared client has none.
    Http {
        url: String,
        headers: BTreeMap<String, String>,
    },
}

/// One `[mcp.NAME]` table, resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerSpec {
    pub name: String,
    pub transport: Transport,
    /// Per request: start, listing and each call.
    pub timeout: Duration,
}

/// A server that did not start, and why, in words for the operator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub server: String,
    pub why: String,
}

/// One live connection.
struct Client {
    wire: Box<dyn Wire>,
    next_id: u64,
    timeout: Duration,
    /// Set once the server is gone, so later calls say so without writing to a dead pipe.
    dead: Option<String>,
}

impl Client {
    fn connect(spec: &ServerSpec) -> Result<(Client, Vec<McpToolSpec>), String> {
        let wire = wire::open(spec)?;
        let mut client = Client {
            wire,
            next_id: 1,
            timeout: spec.timeout,
            dead: None,
        };
        let init = client
            .request(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {"name": "letibot", "version": env!("CARGO_PKG_VERSION")},
                }),
            )
            .map_err(|e| format!("initialize: {}", err_text(&e)))?;
        if init.get("protocolVersion").is_none() {
            return Err(format!(
                "initialize: the answer names no protocolVersion: {init}"
            ));
        }
        client
            .wire
            .notify(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .map_err(|e| format!("notifications/initialized: {e}"))?;
        let tools = client
            .list_tools()
            .map_err(|e| format!("tools/list: {}", err_text(&e)))?;
        Ok((client, tools))
    }

    fn list_tools(&mut self) -> Result<Vec<McpToolSpec>, McpError> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        // A bound on pages, so a server that always returns a cursor cannot hold the open.
        for _ in 0..64 {
            let params = match &cursor {
                Some(c) => json!({"cursor": c}),
                None => json!({}),
            };
            let page = self.request("tools/list", params)?;
            for t in page
                .get("tools")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let Some(name) = t.get("name").and_then(Value::as_str) else {
                    continue;
                };
                tools.push(McpToolSpec {
                    name: name.to_string(),
                    description: t
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    parameters: t
                        .get("inputSchema")
                        .cloned()
                        .unwrap_or_else(|| json!({"type": "object"})),
                });
            }
            cursor = page
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(str::to_string);
            if cursor.is_none() {
                break;
            }
        }
        Ok(tools)
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, McpError> {
        if let Some(why) = &self.dead {
            return Err(McpError::Transport(why.clone()));
        }
        let id = self.next_id;
        self.next_id += 1;
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let answer = match self.wire.request(id, &msg, self.timeout) {
            Ok(a) => a,
            Err(WireError::Gone(why)) => {
                self.dead = Some(why.clone());
                return Err(McpError::Transport(why));
            }
            Err(e) => return Err(McpError::Transport(e.to_string())),
        };
        if let Some(err) = answer.get("error") {
            let code = err.get("code").and_then(Value::as_i64).unwrap_or(0);
            let message = err.get("message").and_then(Value::as_str).unwrap_or("");
            return Err(McpError::Protocol(format!("{code}: {message}")));
        }
        Ok(answer.get("result").cloned().unwrap_or(Value::Null))
    }

    fn call(&mut self, tool: &str, args: &Value) -> Result<McpCallResult, McpError> {
        let result = self.request("tools/call", json!({"name": tool, "arguments": args}))?;
        Ok(McpCallResult {
            text: render_content(&result),
            is_error: result
                .get("isError")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }
}

/// A `tools/call` result as text. Non-text content is named, not inlined: a base64 image is
/// bytes the model cannot read and the cap would only spill.
fn render_content(result: &Value) -> String {
    let mut parts = Vec::new();
    for item in result
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let kind = item.get("type").and_then(Value::as_str).unwrap_or("");
        let part = match kind {
            "text" => item
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            "resource" => match item.pointer("/resource/text").and_then(Value::as_str) {
                Some(t) => t.to_string(),
                None => format!(
                    "[resource {}]",
                    item.pointer("/resource/uri")
                        .and_then(Value::as_str)
                        .unwrap_or("?")
                ),
            },
            "resource_link" => format!(
                "[resource_link {}]",
                item.get("uri").and_then(Value::as_str).unwrap_or("?")
            ),
            other => format!(
                "[{other} content, {}, {} chars not shown]",
                item.get("mimeType").and_then(Value::as_str).unwrap_or("?"),
                item.get("data").and_then(Value::as_str).map_or(0, str::len)
            ),
        };
        parts.push(part);
    }
    if parts.is_empty()
        && let Some(s) = result.get("structuredContent")
    {
        return s.to_string();
    }
    parts.join("\n")
}

fn err_text(e: &McpError) -> String {
    match e {
        McpError::Transport(s) | McpError::Protocol(s) => s.clone(),
        McpError::NotAttached(_) => "not attached".into(),
    }
}

struct Entry {
    name: String,
    tools: Vec<McpToolSpec>,
    client: Mutex<Client>,
}

/// Every configured server, connected or failed.
pub struct Catalog {
    entries: Vec<Entry>,
    failures: Vec<Failure>,
}

impl Catalog {
    /// Start every server at once and wait for each up to its own timeout.
    pub fn start(specs: &[ServerSpec]) -> Catalog {
        let outcomes: Vec<_> = std::thread::scope(|s| {
            let handles: Vec<_> = specs
                .iter()
                .map(|spec| s.spawn(move || (spec.name.clone(), Client::connect(spec))))
                .collect();
            handles
                .into_iter()
                .zip(specs)
                .map(|(h, spec)| {
                    h.join().unwrap_or_else(|_| {
                        (spec.name.clone(), Err("the start thread panicked".into()))
                    })
                })
                .collect()
        });
        let mut entries = Vec::new();
        let mut failures = Vec::new();
        for (name, outcome) in outcomes {
            match outcome {
                Ok((client, tools)) => entries.push(Entry {
                    name,
                    tools,
                    client: Mutex::new(client),
                }),
                Err(why) => failures.push(Failure { server: name, why }),
            }
        }
        Catalog { entries, failures }
    }

    /// Add servers refused before any start, such as a table naming no transport.
    pub fn with_failures(mut self, mut failures: Vec<Failure>) -> Catalog {
        failures.append(&mut self.failures);
        self.failures = failures;
        self
    }

    /// The servers that did not start, in config order.
    pub fn failures(&self) -> &[Failure] {
        &self.failures
    }
}

impl McpCatalog for Catalog {
    fn servers(&self) -> Vec<McpServer> {
        self.entries
            .iter()
            .map(|e| McpServer {
                name: e.name.clone(),
                tools: e.tools.clone(),
            })
            .collect()
    }

    fn call(&self, server: &str, tool: &str, args: &Value) -> Result<McpCallResult, McpError> {
        let Some(entry) = self.entries.iter().find(|e| e.name == server) else {
            return Err(McpError::Transport(format!(
                "no MCP server `{server}` is connected"
            )));
        };
        let mut client = entry.client.lock().unwrap_or_else(|e| e.into_inner());
        client.call(tool, args)
    }

    fn describe(&self) -> String {
        let up: Vec<String> = self
            .entries
            .iter()
            .map(|e| format!("{} ({} tools)", e.name, e.tools.len()))
            .collect();
        let mut s = if up.is_empty() {
            "none connected".to_string()
        } else {
            format!("MCP servers {}", up.join(", "))
        };
        if !self.failures.is_empty() {
            let down: Vec<&str> = self.failures.iter().map(|f| f.server.as_str()).collect();
            s.push_str(&format!("; failed to start: {}", down.join(", ")));
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_renders_text_and_names_the_rest() {
        let r = json!({"content": [
            {"type": "text", "text": "a"},
            {"type": "image", "mimeType": "image/png", "data": "AAAA"},
            {"type": "resource", "resource": {"uri": "file:///x", "text": "b"}},
        ]});
        assert_eq!(
            render_content(&r),
            "a\n[image content, image/png, 4 chars not shown]\nb"
        );
    }

    #[test]
    fn structured_content_stands_in_for_no_content() {
        let r = json!({"content": [], "structuredContent": {"n": 1}});
        assert_eq!(render_content(&r), r#"{"n":1}"#);
    }
}
