//! **MCP over stdio**: JSON-RPC 2.0, one message per line, and nothing else on stdout.
//!
//! # Why there is no MCP library here
//!
//! The protocol this needs is three methods and one notification, and the message framing
//! is a newline. A library would add a dependency, an executor and a decoder between the
//! bridge and the seven hundred lines it already has; what it would save is this file.
//!
//! # The two rules that are easy to get wrong, and are tested
//!
//! * **A line that does not parse is an answer, not the end of the server.** A client that
//!   writes something odd to stdin gets `-32700` and the next line is served as usual:
//!   a bridge that exited there would look to Claude Code like a server that crashed, with
//!   the real cause invisible.
//! * **A notification gets no reply.** `notifications/initialized` has no `id`, and a
//!   JSON-RPC peer that answers it is a peer that will be answered out of order.
//!
//! **stdout is the wire.** Every diagnostic goes to stderr, because a single stray `print!`
//! lands in the middle of a JSON line and the client's decoder reports a protocol error
//! about the bridge's own logging.

use std::io::{BufRead, Write};

use serde_json::{Value, json};

use crate::tools::Bridge;

/// What a client that asks for the MCP revision gets. `2025-06-18` is the revision the
/// tool/`isError` shape here is written against; a client that speaks an older one still
/// reads it, because the fields it uses are the same three.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// Serve one client, reading requests from `input` and writing answers to `output`.
///
/// Returns when the input ends. Errors on the writer are returned rather than swallowed:
/// a client that closed stdout is gone, and continuing to read its requests would be work
/// nobody receives.
pub fn serve<R: BufRead, W: Write>(
    mut input: R,
    mut output: W,
    bridge: &Bridge,
) -> std::io::Result<()> {
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
                write_message(&mut output, &error(Value::Null, -32700, "parse error"))?;
                continue;
            }
            Err(e) => return Err(e),
        }
        if line.trim().is_empty() {
            continue;
        }
        let answer = match serde_json::from_str::<Value>(line.trim()) {
            Ok(request) => handle(bridge, &request),
            Err(e) => Some(error(Value::Null, -32700, &format!("parse error: {e}"))),
        };
        if let Some(answer) = answer {
            write_message(&mut output, &answer)?;
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
            "capabilities": { "tools": {} },
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
        "tools/call" => Some(call_tool(bridge, &params)),
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

/// `tools/call`: run the verb and wrap what it said.
///
/// Two kinds of failure and two different shapes, on purpose. **A caller's mistake** — an
/// unknown tool, a missing argument — is a JSON-RPC error, because the request was not a
/// well-formed call of anything. **A tool that ran and failed** is `isError: true` with the
/// reason as text, because the client asked a question this server understood and the answer
/// is no: an error object there would make a perfectly answerable call look like a broken
/// server.
fn call_tool(bridge: &Bridge, params: &Value) -> Value {
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return text("tools/call needs a `name`".into(), true);
    };
    if !crate::tools::names().contains(&name) {
        return text(
            format!(
                "unknown tool `{name}`; this build serves {}",
                crate::tools::names().join(", ")
            ),
            true,
        );
    }
    let args = params
        .get("arguments")
        .cloned()
        .filter(|v| v.is_object())
        .unwrap_or_else(|| json!({}));
    match crate::tools::call(bridge, name, &args) {
        Ok(value) => text(value.to_string(), false),
        Err(why) => text(why, true),
    }
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

    /// **Eight tools, each with a schema** — the design's table, and the shape MCP requires.
    #[test]
    fn tools_list_lists_the_eight_tools_with_schemas() {
        let answers = run(&[r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#]);
        let tools = answers[0]["result"]["tools"].as_array().expect("tools");
        assert_eq!(tools.len(), 8, "{:?}", answers[0]);
        for t in tools {
            assert!(t["name"].is_string(), "{t}");
            assert!(t["description"].is_string(), "{t}");
            assert_eq!(t["inputSchema"]["type"], "object", "{t}");
        }
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

    /// A `tools/call` naming a tool this build does not have is `isError` with the list, not
    /// a crashed request: the arguments were well-formed and the answer is that there is no
    /// such verb.
    #[test]
    fn an_unknown_tool_is_an_error_result_with_the_list() {
        let req = call_request(3, "leticode_nope", json!({}));
        let answers = run(&[&req]);
        let r = &answers[0];
        assert_eq!(r["result"]["isError"], true, "{r}");
        let text = r["result"]["content"][0]["text"].as_str().expect("text");
        assert!(text.contains("leticode_start"), "{text}");
        assert_eq!(r["result"]["content"][0]["type"], "text");
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
