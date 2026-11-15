//! A stdio MCP server for tests. `--die-at-start` exits before answering anything; otherwise it
//! serves `echo`, `big` (`bytes` of text), `fail` (`isError`) and `crash` (exits mid-call).

use std::io::{BufRead, Write};

use serde_json::{Value, json};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--die-at-start") {
        eprintln!("fake: refusing to start");
        std::process::exit(3);
    }
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    // A log line on stdout, which a client must skip.
    let _ = writeln!(out, "fake mcp server starting");
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(id) = msg.get("id").cloned() else {
            continue;
        };
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let result = match method {
            "initialize" => json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "fake", "version": "0"},
            }),
            "tools/list" => json!({"tools": [
                {"name": "echo", "description": "Echo the text back.",
                 "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}}},
                {"name": "big", "description": "Return many bytes of text.",
                 "inputSchema": {"type": "object", "properties": {"bytes": {"type": "integer"}}}},
                {"name": "fail", "description": "Report an error.",
                 "inputSchema": {"type": "object"}},
                {"name": "crash", "description": "Exit while answering.",
                 "inputSchema": {"type": "object"}},
            ]}),
            "tools/call" => {
                let args = msg
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or(json!({}));
                match msg.pointer("/params/name").and_then(Value::as_str) {
                    Some("echo") => text(args.get("text").and_then(Value::as_str).unwrap_or("")),
                    Some("big") => {
                        let n = args.get("bytes").and_then(Value::as_u64).unwrap_or(100_000);
                        let line =
                            "0123456789abcdefghijklmnopqrstuvwxyz0123456789abcdefghijklmnopqrs\n";
                        text(&line.repeat(n as usize / line.len() + 1)[..n as usize])
                    }
                    Some("fail") => json!({"content": [{"type": "text", "text": "it broke"}],
                                           "isError": true}),
                    Some("crash") => {
                        eprintln!("fake: crashing on purpose");
                        std::process::exit(7);
                    }
                    _ => {
                        let _ = writeln!(
                            out,
                            "{}",
                            json!({"jsonrpc": "2.0", "id": id,
                                   "error": {"code": -32602, "message": "unknown tool"}})
                        );
                        let _ = out.flush();
                        continue;
                    }
                }
            }
            _ => json!({}),
        };
        let _ = writeln!(
            out,
            "{}",
            json!({"jsonrpc": "2.0", "id": id, "result": result})
        );
        let _ = out.flush();
    }
}

fn text(s: &str) -> Value {
    json!({"content": [{"type": "text", "text": s}]})
}
