//! A stdio MCP server for tests, the `letibot-mcp-fake` bin and the `fake` feature. `--die-at-start` exits before answering, `--once FILE` only when
//! FILE exists, `--endless` writes 64 MiB with no newline and then waits. Tools: `echo`, `big`, `fail` (`isError`), `crash` (exits mid-call), `pwd`, and
//! `grow`, which adds `extra` and sends `notifications/tools/list_changed`, `env` (one variable's
//! value, empty when unset), and `spawn`, which starts a `sleep` grandchild and says its pid.

use std::io::{BufRead, Write};

use serde_json::{Value, json};

/// Serve on stdin and stdout until EOF. `args` are the flags above.
pub fn serve(args: &[String]) {
    if args.iter().any(|a| a == "--die-at-start") {
        eprintln!("fake: refusing to start");
        std::process::exit(3);
    }
    if args.iter().any(|a| a == "--endless") {
        let mut out = std::io::stdout().lock();
        let chunk = vec![b'x'; 1 << 20];
        for _ in 0..64 {
            if out.write_all(&chunk).is_err() {
                std::process::exit(0);
            }
        }
        let _ = out.flush();
        std::thread::sleep(std::time::Duration::from_secs(600));
        std::process::exit(0);
    }
    if let Some(i) = args.iter().position(|a| a == "--once") {
        let marker = &args[i + 1];
        if std::path::Path::new(marker).exists() {
            eprintln!("fake: started once already");
            std::process::exit(4);
        }
        std::fs::write(marker, b"").unwrap();
    }
    let mut grown = false;
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
            "tools/list" => {
                let mut tools = json!([
                {"name": "echo", "description": "Echo the text back.",
                 "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}}},
                {"name": "big", "description": "Return many bytes of text.",
                 "inputSchema": {"type": "object", "properties": {"bytes": {"type": "integer"}}}},
                {"name": "fail", "description": "Report an error.",
                 "inputSchema": {"type": "object"}},
                {"name": "crash", "description": "Exit while answering.",
                 "inputSchema": {"type": "object"}},
                {"name": "grow", "description": "Offer one more tool.",
                 "inputSchema": {"type": "object"}},
                {"name": "pwd", "description": "Say the working directory.",
                 "inputSchema": {"type": "object"}},
                {"name": "env", "description": "Say one environment variable.",
                 "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}}}},
                {"name": "spawn", "description": "Start a grandchild and say its pid.",
                 "inputSchema": {"type": "object"}},
                {"name": "pid", "description": "Wait `ms`, then say this server's pid.",
                 "inputSchema": {"type": "object", "properties": {"ms": {"type": "integer"}}}},
                ]);
                if grown {
                    tools.as_array_mut().unwrap().push(json!({"name": "extra",
                        "description": "Offered after grow.", "inputSchema": {"type": "object"}}));
                }
                json!({"tools": tools})
            }
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
                    Some("grow") => {
                        grown = true;
                        text("grown")
                    }
                    Some("extra") => text("extra ran"),
                    Some("pwd") => text(&std::env::current_dir().unwrap().display().to_string()),
                    Some("env") => {
                        let name = args.get("name").and_then(Value::as_str).unwrap_or("");
                        text(&std::env::var(name).unwrap_or_default())
                    }
                    Some("spawn") => {
                        // Left running on purpose: the test checks the group kill reaps it.
                        #[allow(clippy::zombie_processes)]
                        let child = std::process::Command::new("sleep")
                            .arg("600")
                            .stdin(std::process::Stdio::null())
                            .stdout(std::process::Stdio::null())
                            .stderr(std::process::Stdio::null())
                            .spawn()
                            .unwrap();
                        text(&child.id().to_string())
                    }
                    Some("pid") => {
                        let ms = args.get("ms").and_then(Value::as_u64).unwrap_or(0);
                        std::thread::sleep(std::time::Duration::from_millis(ms));
                        text(&std::process::id().to_string())
                    }
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
        // After the answer, so the client reads it between calls.
        if msg.pointer("/params/name").and_then(Value::as_str) == Some("grow") {
            let _ = writeln!(
                out,
                "{}",
                json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"})
            );
        }
        let _ = out.flush();
    }
}

fn text(s: &str) -> Value {
    json!({"content": [{"type": "text", "text": s}]})
}
