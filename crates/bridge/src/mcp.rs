//! **MCP over stdio**: JSON-RPC 2.0, one message per line, and nothing else on stdout.
//!
//! No MCP library: the protocol here is three methods and one notification, the framing is a
//! newline, and a library would add a dependency, an executor and a decoder for it.
//! Two rules are easy to get wrong and are tested: **a line that does not parse is an answer,
//! not the end of the server** (a client gets `-32700` and the next line is served as usual),
//! and **a notification gets no reply**. And **stdout is the wire**: every diagnostic goes to
//! stderr, because one stray `print!` lands in the middle of a JSON line.

use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, mpsc};
use std::time::Duration;

use serde_json::{Value, json};

use crate::tools::Bridge;

/// What a client that asks for the MCP revision gets. `2025-06-18` is the revision the
/// tool/`isError` shape here is written against; a client that speaks an older one still
/// reads it, because the fields it uses are the same three.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// Serve one client, reading requests from `input` and writing answers to `output`. Returns when
/// the input ends. Errors on the writer are returned rather than swallowed: a client that closed
/// stdout is gone, and reading its requests would be work nobody receives.
///
/// The output is shared with the room stream's writer thread, so a pushed notification and an
/// answer are never interleaved inside one line.
pub fn serve<R: BufRead, W: Write + Send>(
    mut input: R,
    output: W,
    bridge: &Bridge,
) -> std::io::Result<()> {
    let out = Mutex::new(output);
    let (push_tx, push_rx) = mpsc::channel::<Value>();
    let done = AtomicBool::new(false);
    let (done, out_ref) = (&done, &out);
    std::thread::scope(|scope| {
        let writer = scope.spawn(move || {
            // The stream's senders live as long as the bridge, so the channel never closes:
            // `done` is what ends this thread when the client's input does.
            while !done.load(Ordering::SeqCst) {
                let Ok(message) = push_rx.recv_timeout(Duration::from_millis(100)) else {
                    continue;
                };
                let mut w = out_ref.lock().unwrap_or_else(|e| e.into_inner());
                if write_message(&mut *w, &message).is_err() {
                    break;
                }
            }
        });
        let result = read_loop(&mut input, &out, bridge, &push_tx);
        done.store(true, Ordering::SeqCst);
        let _ = writer.join();
        result
    })
}

fn read_loop<R: BufRead, W: Write>(
    input: &mut R,
    out: &Mutex<W>,
    bridge: &Bridge,
    push: &mpsc::Sender<Value>,
) -> std::io::Result<()> {
    let send = |message: &Value| {
        let mut w = out.lock().unwrap_or_else(|e| e.into_inner());
        write_message(&mut *w, message)
    };
    let mut line = String::new();
    loop {
        line.clear();
        // Malformed UTF-8 is a fact about one line, not about the stream: `read_line`
        // reports it as an error and the byte has already been consumed, so the loop
        // answers with a parse error and reads on rather than ending the server.
        match input.read_line(&mut line) {
            Ok(0) => return Ok(()),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                send(&error(Value::Null, -32700, "parse error"))?;
                continue;
            }
            Err(e) => return Err(e),
        }
        if line.trim().is_empty() {
            continue;
        }
        let answer = match serde_json::from_str::<Value>(line.trim()) {
            Ok(request) => {
                // The client has applied `initialize`: from here a channel notification is
                // legal, so the room's stream may start.
                if request["method"] == "notifications/initialized" && bridge.stream_rooms() {
                    let tx = push.clone();
                    bridge.rooms().stream(std::sync::Arc::new(move |m| {
                        let _ = tx.send(crate::rooms::channel_notification(m));
                    }));
                }
                handle(bridge, &request)
            }
            Err(e) => Some(error(Value::Null, -32700, &format!("parse error: {e}"))),
        };
        if let Some(answer) = answer {
            send(&answer)?;
        }
    }
}

fn write_message<W: Write>(out: &mut W, message: &Value) -> std::io::Result<()> {
    serde_json::to_writer(&mut *out, message)?;
    out.write_all(b"\n")?;
    out.flush()
}

/// One request in, at most one message out.
fn handle(bridge: &Bridge, request: &Value) -> Option<Value> {
    let Some(method) = request.get("method").and_then(Value::as_str) else {
        // Not a request at all: no method. An `id` makes it answerable, and a JSON-RPC
        // invalid-request is the answer; without one there is nobody to tell.
        return request
            .get("id")
            .map(|id| error(id.clone(), -32600, "invalid request: no `method`"));
    };
    let id = request.get("id").cloned();
    let params = request.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => Some(json!({
            "protocolVersion": PROTOCOL_VERSION,
            // `claude/channel` lets Claude Code start a turn on a room message; a client that
            // was not started with the channel flag ignores it and the tools still work.
            "capabilities": { "tools": {}, "experimental": { "claude/channel": {} } },
            "instructions": "Room messages for `claude-code` arrive as <channel> events: read \
                them, answer with `room_say` or `room_dm`, and `room_inbox` with ack true marks \
                what you handled.",
            "serverInfo": {
                "name": "letibot-mcp",
                "version": env!("CARGO_PKG_VERSION"),
            },
        })),
        // A notification, by contract: the client says it has applied `initialize`. There
        // is nothing to do and nothing to answer.
        "notifications/initialized" | "notifications/cancelled" => return None,
        "ping" => Some(json!({})),
        "tools/list" => Some(crate::tools::list()),
        "tools/call" => match call_tool(bridge, &params) {
            Ok(result) => Some(result),
            // A caller's mistake is the protocol's to report, and the id is the only way the
            // client learns which request it was.
            Err((code, message)) => return id.map(|id| error(id, code, &message)),
        },
        _ => {
            // -32601 is the JSON-RPC code for a method this peer does not serve, and it is
            // the one a client uses to decide the server predates a feature. A notification
            // naming an unknown method is silence, like every other notification.
            let id = id?;
            return Some(error(id, -32601, &format!("method not found: {method}")));
        }
    };
    let id = id?;
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result.unwrap_or(Value::Null) }))
}

/// `tools/call`: run the verb and wrap what it said. Two kinds of failure, on purpose: **a
/// caller's mistake** - an unknown tool, a missing name - is a JSON-RPC error (`-32602`, the code
/// MCP names for it), while **a tool that ran and failed** is `isError: true`, because an error
/// object would look like a broken server.
fn call_tool(bridge: &Bridge, params: &Value) -> Result<Value, (i64, String)> {
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return Err((-32602, "tools/call needs a `name`".into()));
    };
    if !crate::tools::names().contains(&name) {
        return Err((
            -32602,
            format!(
                "unknown tool `{name}`; this build serves {}",
                crate::tools::names().join(", ")
            ),
        ));
    }
    let args = params
        .get("arguments")
        .cloned()
        .filter(|v| v.is_object())
        .unwrap_or_else(|| json!({}));
    Ok(match crate::tools::call(bridge, name, &args) {
        Ok(value) => text(value.to_string(), false),
        Err(why) => text(why, true),
    })
}

/// An MCP tool result: one text block holding the JSON (or the sentence), and whether it is
/// a failure.
fn text(body: String, is_error: bool) -> Value {
    json!({
        "content": [{ "type": "text", "text": body }],
        "isError": is_error,
    })
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Feed a whole session in and hand back the lines it answered.
    fn run(lines: &[&str]) -> Vec<Value> {
        let input = lines.join("\n") + "\n";
        let mut out: Vec<u8> = Vec::new();
        serve(Cursor::new(input.into_bytes()), &mut out, &Bridge::new()).expect("serve");
        String::from_utf8(out)
            .expect("utf-8")
            .lines()
            .map(|l| serde_json::from_str(l).expect("every answer is one JSON line"))
            .collect()
    }

    /// **`initialize` answers the revision, the capability and the name** — the three
    /// fields a client reads before it sends anything else.
    #[test]
    fn initialize_answers_the_revision_and_names_the_server() {
        let answers = run(&[
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        ]);
        assert_eq!(
            answers.len(),
            1,
            "a notification gets no answer: {answers:?}"
        );
        let r = &answers[0];
        assert_eq!(r["id"], 1);
        assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
        assert!(r["result"]["capabilities"]["tools"].is_object(), "{r}");
        assert_eq!(r["result"]["serverInfo"]["name"], "letibot-mcp");
    }

    /// **Twenty tools, each with a schema** — the design's table, and the shape MCP requires.
    /// Ten from B1–B6, B8's `leticode_transcript` and `leticode_mode`, `leticode_model`,
    /// `leticode_collect` for a session stopped before its branch came home, and the six room tools.
    #[test]
    fn tools_list_lists_the_twenty_tools_with_schemas() {
        let answers = run(&[r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#]);
        let tools = answers[0]["result"]["tools"].as_array().expect("tools");
        assert_eq!(tools.len(), 20, "{:?}", answers[0]);
        for t in tools {
            assert!(t["name"].is_string(), "{t}");
            assert!(t["description"].is_string(), "{t}");
            assert_eq!(t["inputSchema"]["type"], "object", "{t}");
        }
        // The one B2 added is published with its cursor's own schema, not a bare object.
        let events = tools
            .iter()
            .find(|t| t["name"] == "leticode_events")
            .expect("leticode_events is in tools/list");
        assert_eq!(
            events["inputSchema"]["properties"]["since"]["type"],
            "integer"
        );
        assert_eq!(&events["inputSchema"]["required"], &json!(["session"]));
        // B6's attach publishes the default that makes it a watcher, so a client that omits
        // `watch_only` has been told what it gets.
        let attach = tools
            .iter()
            .find(|t| t["name"] == "leticode_attach")
            .expect("leticode_attach is in tools/list");
        assert_eq!(
            &attach["inputSchema"]["required"],
            &json!(["worktree", "session"])
        );
        assert_eq!(
            attach["inputSchema"]["properties"]["watch_only"]["type"],
            "boolean"
        );
        // B8's transcript publishes its cursor and its cap, so a client can see that `since` is
        // the number the last answer handed back rather than a seq or a row id.
        let transcript = tools
            .iter()
            .find(|t| t["name"] == "leticode_transcript")
            .expect("leticode_transcript is in tools/list");
        assert_eq!(
            transcript["inputSchema"]["properties"]["since"]["type"],
            "integer"
        );
        assert_eq!(&transcript["inputSchema"]["required"], &json!(["session"]));
        // And `leticode_mode` takes a point and nothing else: there is no argument through which
        // a caller could consent on somebody's behalf.
        let mode = tools
            .iter()
            .find(|t| t["name"] == "leticode_mode")
            .expect("leticode_mode is in tools/list");
        assert_eq!(
            &mode["inputSchema"]["required"],
            &json!(["session", "mode"])
        );
        // A collect is asked for by the names a placement was made from, so the two that cannot be
        // inferred - the session and the host - are required and the folder is not.
        let collect = tools
            .iter()
            .find(|t| t["name"] == "leticode_collect")
            .expect("leticode_collect is in tools/list");
        assert_eq!(
            &collect["inputSchema"]["required"],
            &json!(["session", "host"])
        );
        assert_eq!(
            collect["inputSchema"]["properties"]["vm"]["type"],
            "boolean"
        );
    }

    /// A method this server does not serve is `-32601`, carrying the client's id so the
    /// caller knows which request failed.
    #[test]
    fn an_unknown_method_is_method_not_found() {
        let answers = run(&[r#"{"jsonrpc":"2.0","id":9,"method":"resources/list"}"#]);
        assert_eq!(answers[0]["error"]["code"], -32601, "{:?}", answers[0]);
        assert_eq!(answers[0]["id"], 9);
        assert!(
            answers[0]["error"]["message"]
                .as_str()
                .unwrap()
                .contains("resources/list"),
            "{:?}",
            answers[0]
        );
    }

    /// **A line that does not parse is a parse error and the server keeps going** — the
    /// next request on the next line is answered normally. This is the difference between a
    /// client that sees one bad message and a client whose server died.
    #[test]
    fn a_malformed_line_is_a_parse_error_and_the_server_keeps_going() {
        let answers = run(&[
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
            "{ this is not json",
            r#"{"jsonrpc":"2.0","id":2,"method":"initialize","params":{}}"#,
        ]);
        assert_eq!(answers.len(), 3, "{answers:?}");
        assert!(answers[0]["result"]["tools"].is_array());
        assert_eq!(answers[1]["error"]["code"], -32700, "{:?}", answers[1]);
        assert_eq!(answers[1]["id"], Value::Null);
        assert_eq!(answers[2]["id"], 2, "the server answered the next line");
    }

    /// One `tools/call` request line, built rather than spelled: the shape a client sends
    /// is `{"jsonrpc","id","method","params":{"name","arguments"}}` and a test that
    /// hand-wrote it across two lines would be sending two requests.
    fn call_request(id: u64, name: &str, args: Value) -> String {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": name, "arguments": args },
        })
        .to_string()
    }

    /// **A `tools/call` naming a tool this build does not have is `-32602`**, with the list in the
    /// message: the arguments were well-formed and the verb is not there, which is the caller's
    /// mistake and not a tool that ran and failed. A client given `isError: true` reads *the tool
    /// failed* and retries, which is the one direction that spends a round.
    #[test]
    fn an_unknown_tool_is_an_invalid_params_error_with_the_list() {
        let req = call_request(3, "leticode_nope", json!({}));
        let answers = run(&[&req]);
        let r = &answers[0];
        assert_eq!(r["error"]["code"], -32602, "{r}");
        assert_eq!(r["id"], 3, "the id says which request failed");
        assert!(r["result"].is_null(), "and there is no tool result: {r}");
        let message = r["error"]["message"].as_str().expect("message");
        assert!(message.contains("leticode_nope"), "{message}");
        assert!(message.contains("leticode_start"), "{message}");
    }

    /// The other caller's mistake the module names: a `tools/call` with no `name` at all.
    #[test]
    fn a_tools_call_with_no_name_is_an_invalid_params_error() {
        let req = json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": { "arguments": {} },
        })
        .to_string();
        let answers = run(&[&req]);
        assert_eq!(answers[0]["error"]["code"], -32602, "{:?}", answers[0]);
        assert!(
            answers[0]["error"]["message"]
                .as_str()
                .unwrap()
                .contains("name"),
            "{:?}",
            answers[0]
        );
    }

    /// A tool that runs and refuses (here: a session this bridge does not hold) answers
    /// with `isError` and a sentence, and the JSON is a string as MCP requires.
    #[test]
    fn a_tool_that_refuses_answers_iserror_with_a_sentence() {
        let req = call_request(4, "leticode_status", json!({ "session": "s-nope" }));
        let answers = run(&[&req]);
        let r = &answers[0];
        assert_eq!(r["result"]["isError"], true, "{r}");
        let text = r["result"]["content"][0]["text"].as_str().expect("text");
        assert!(text.contains("s-nope"), "{text}");
    }

    /// `leticode_list` on a bridge holding nothing is `isError: false` with an empty list —
    /// a listing is a question, and "none" is an answer.
    #[test]
    fn a_listing_of_nothing_is_not_an_error() {
        let req = call_request(5, "leticode_list", json!({}));
        let answers = run(&[&req]);
        let r = &answers[0];
        assert_eq!(r["result"]["isError"], false, "{r}");
        let body: Value =
            serde_json::from_str(r["result"]["content"][0]["text"].as_str().expect("text"))
                .expect("the content text is JSON");
        assert_eq!(body["sessions"].as_array().map(|v| v.len()), Some(0));
    }

    /// A blank line is silence, not a request: the client may write one and no answer is
    /// owed.
    #[test]
    fn a_blank_line_is_not_a_request() {
        let answers = run(&["", "   ", r#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#]);
        assert_eq!(answers.len(), 1, "{answers:?}");
        assert_eq!(answers[0]["id"], 7);
    }
}
