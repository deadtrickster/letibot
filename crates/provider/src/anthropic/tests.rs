//! The Messages client against the protocol's own documents: the request it builds from a
//! transcript, the stream it reads, and a stub server on localhost for the shapes a unit test
//! cannot see (the headers, the refusal statuses, the retry that a documented remedy earns).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use letibot_backend::{BackendError, Delta, Finish, MessagesBackend, StreamFlow, TurnRequest};
use letibot_transcript::{
    CallOrigin, ReasoningField, SystemOrigin, ToolCall, ToolOutcome, TranscriptItem, UserPart,
};
use serde_json::{Value, json};

use super::*;

// -- fixtures ----------------------------------------------------------------------------------

fn creds(url: Option<&str>) -> Credentials {
    Credentials {
        key: "sk-ant-test".into(),
        from: "test".into(),
        prices: Default::default(),
        url: url.map(str::to_string),
    }
}

fn provider() -> AnthropicProvider {
    provider_at("http://127.0.0.1:1/v1/messages")
}

fn provider_at(url: &str) -> AnthropicProvider {
    AnthropicProvider::new(
        &crate::presets::ANTHROPIC,
        Some("claude-opus-5-5"),
        creds(Some(url)),
    )
    // A box with no catalogue: a price or a modality read off the machine would make the test
    // an assertion about the machine.
    .with_catalogue(Arc::new(crate::catalogue::Catalogue::default()))
}

fn user(text: &str) -> TranscriptItem {
    TranscriptItem::User {
        speaker: Default::default(),
        parts: vec![UserPart::Text { text: text.into() }],
    }
}

fn thinking(text: &str, signature: &str) -> TranscriptItem {
    TranscriptItem::Reasoning {
        text: text.into(),
        field: ReasoningField::Signed {
            signature: signature.into(),
        },
        truncated: false,
    }
}

fn plain_reasoning(text: &str) -> TranscriptItem {
    TranscriptItem::Reasoning {
        text: text.into(),
        field: ReasoningField::ReasoningContent,
        truncated: false,
    }
}

fn assistant(text: &str) -> TranscriptItem {
    TranscriptItem::Assistant {
        text: text.into(),
        tool_calls: Vec::new(),
        truncated: false,
    }
}

fn calls(ids: &[&str]) -> TranscriptItem {
    TranscriptItem::Assistant {
        text: String::new(),
        tool_calls: ids
            .iter()
            .map(|id| ToolCall {
                id: (*id).into(),
                name: "read".into(),
                arguments: r#"{"path":"a.txt"}"#.into(),
            })
            .collect(),
        truncated: false,
    }
}

fn result(id: &str, outcome: ToolOutcome, payload: &str) -> TranscriptItem {
    TranscriptItem::ToolResult {
        call_id: id.into(),
        name: "read".into(),
        outcome,
        payload: payload.into(),
        edits: Vec::new(),
        origin: None,
        media: None,
    }
}

fn request<'a>(
    system: &'a str,
    items: &'a [TranscriptItem],
    tools: &'a [String],
) -> TurnRequest<'a> {
    TurnRequest {
        system,
        tools_json: tools,
        items,
        tail: &[],
        max_output_tokens: None,
        thinking: None,
    }
}

/// One request the stub was sent: its headers and its body, in the order it arrived.
type Seen = Vec<(Vec<(String, String)>, String)>;

/// The stub provider: one request per connection, and the answer the test wrote for that body.
struct Fake {
    url: String,
    seen: Arc<Mutex<Seen>>,
}

impl Fake {
    fn header(&self, n: usize, name: &str) -> Option<String> {
        let seen = self.seen.lock().unwrap();
        seen.get(n)?
            .0
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.clone())
    }

    fn body(&self, n: usize) -> Value {
        let seen = self.seen.lock().unwrap();
        serde_json::from_str(&seen[n].1).expect("the request is JSON")
    }

    fn count(&self) -> usize {
        self.seen.lock().unwrap().len()
    }
}

/// A stub that answers with `status` and `script`, plus the headers a real refusal carries.
fn fake_headers(
    status: u16,
    script: &'static str,
    headers: &'static [(&'static str, &'static str)],
) -> Fake {
    fake_answering(move |_| (status, script.to_string(), headers.to_vec()))
}

fn fake(status: u16, script: &'static str) -> Fake {
    fake_headers(status, script, &[])
}

/// A stub that decides per REQUEST: the body it was sent is the argument, so a test can ask
/// whether the second request differs from the first - which is the whole question a retry poses.
fn fake_answering(
    answer: impl Fn(&str) -> (u16, String, Vec<(&'static str, &'static str)>) + Send + 'static,
) -> Fake {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1/messages", listener.local_addr().unwrap());
    let seen: Arc<Mutex<Seen>> = Arc::new(Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut conn) = conn else { continue };
            let mut reader = BufReader::new(conn.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let mut len = 0usize;
            let mut headers: Vec<(String, String)> = Vec::new();
            loop {
                let mut h = String::new();
                if reader.read_line(&mut h).unwrap_or(0) == 0 {
                    break;
                }
                let h = h.trim_end();
                if h.is_empty() {
                    break;
                }
                if let Some((k, v)) = h.split_once(':') {
                    let k = k.trim().to_string();
                    let v = v.trim().to_string();
                    if k.eq_ignore_ascii_case("content-length") {
                        len = v.parse().unwrap_or(0);
                    }
                    headers.push((k, v));
                }
            }
            let mut body = vec![0u8; len];
            reader.read_exact(&mut body).unwrap();
            let body = String::from_utf8_lossy(&body).into_owned();
            let (status, script, extra) = answer(&body);
            seen2.lock().unwrap().push((headers, body));
            let mut head = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n",
                script.len()
            );
            for (k, v) in extra {
                head.push_str(&format!("{k}: {v}\r\n"));
            }
            head.push_str("\r\n");
            let _ = conn.write_all(head.as_bytes());
            let _ = conn.write_all(script.as_bytes());
            let _ = conn.flush();
        }
    });
    Fake { url, seen }
}

/// One SSE event as the wire carries it: a named event and its one JSON payload.
fn event(payload: Value) -> String {
    let name = payload["type"].as_str().unwrap_or("event").to_string();
    format!("event: {name}\ndata: {payload}\n\n")
}

/// A whole plain answer: start, one text block, the stop, the end.
fn text_answer(model: &str, text: &str) -> String {
    let mut out = String::new();
    out.push_str(&event(json!({
        "type": "message_start",
        "message": {"id": "msg_1", "type": "message", "role": "assistant", "model": model,
            "stop_reason": null, "stop_sequence": null,
            "usage": {"input_tokens": 11, "cache_read_input_tokens": 90,
                      "cache_creation_input_tokens": 0, "output_tokens": 1}},
    })));
    out.push_str(&event(json!({
        "type": "content_block_start", "index": 0,
        "content_block": {"type": "text", "text": ""},
    })));
    out.push_str(&event(json!({
        "type": "content_block_delta", "index": 0,
        "delta": {"type": "text_delta", "text": text},
    })));
    out.push_str(&event(json!({"type": "content_block_stop", "index": 0})));
    out.push_str(&event(json!({
        "type": "message_delta",
        "delta": {"stop_reason": "end_turn", "stop_sequence": null},
        "usage": {"output_tokens": 7},
    })));
    out.push_str(&event(json!({"type": "message_stop"})));
    out
}

fn run(
    p: &AnthropicProvider,
    req: &TurnRequest<'_>,
) -> (
    Vec<Delta>,
    Result<letibot_backend::Completion, BackendError>,
) {
    let mut deltas = Vec::new();
    let done = p.complete(req, &mut |d| {
        deltas.push(d.clone());
        StreamFlow::Continue
    });
    (deltas, done)
}

// -- the request the transcript becomes --------------------------------------------------------

/// The system prompt is a FIELD, not the first message: a `system` role is not one this API has.
/// The leading `System` item is that prompt, and its block is where the cache breakpoint goes,
/// because the prompt is the front of every request's prefix.
#[test]
fn the_system_prompt_is_a_field_and_its_block_carries_the_breakpoint() {
    let items = vec![
        TranscriptItem::System {
            text: "be terse".into(),
            origin: SystemOrigin::Bootstrap,
        },
        user("what is the date?"),
    ];
    let body = provider().body(&request("", &items, &[])).expect("a body");
    let system = body["system"].as_array().expect("an array of blocks");
    assert_eq!(system.len(), 1, "{system:?}");
    assert_eq!(system[0]["type"], "text");
    assert_eq!(system[0]["text"], "be terse");
    assert_eq!(
        system[0]["cache_control"]["type"], "ephemeral",
        "the stable prefix's end is where a breakpoint goes: {system:?}"
    );
    // And not a message: the API has no `system` role inside `messages`.
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[0]["content"][0]["text"], "what is the date?");
}

/// The second breakpoint is the end of the transcript: the tail is what this request adds, so the
/// prefix the next request shares stops at the last committed block and not one block further.
#[test]
fn the_transcript_ends_with_a_breakpoint_and_the_tail_is_after_it() {
    let committed = vec![user("first"), assistant("an answer")];
    let tail = vec![user("and now this")];
    let req = TurnRequest {
        system: "be terse",
        tools_json: &[],
        items: &committed,
        tail: &tail,
        max_output_tokens: None,
        thinking: None,
    };
    let body = provider().body(&req).expect("a body");
    let messages = body["messages"].as_array().unwrap();
    // The tail's words are in the same user message as nothing else, and carry no breakpoint.
    let last = messages.last().unwrap();
    let blocks = last["content"].as_array().unwrap();
    assert_eq!(blocks[0]["text"], "and now this");
    assert!(blocks[0].get("cache_control").is_none(), "{blocks:?}");
    // The assistant turn is the last thing the transcript committed, and it is the breakpoint.
    let answer = messages
        .iter()
        .find(|m| m["role"] == "assistant")
        .expect("the committed answer");
    assert_eq!(
        answer["content"][last_block(answer)]["cache_control"]["type"],
        "ephemeral"
    );
}

/// The index of a message's last block, for the assertions above.
fn last_block(message: &Value) -> usize {
    message["content"].as_array().map(Vec::len).unwrap_or(1) - 1
}

/// A request's own prompt and a leading item are the same fact carried twice; both go in the
/// field, and an empty one produces no field at all.
#[test]
fn an_empty_system_prompt_is_no_field_and_two_carriers_are_two_blocks() {
    let items = vec![
        TranscriptItem::System {
            text: "second".into(),
            origin: SystemOrigin::Bootstrap,
        },
        user("hi"),
    ];
    let body = provider()
        .body(&request("first", &items, &[]))
        .expect("a body");
    let system = body["system"].as_array().unwrap();
    assert_eq!(system.len(), 2, "{system:?}");
    assert_eq!(system[1]["text"], "second");
    // Only the last block is the breakpoint: one write, at the end of the shared prefix.
    assert!(system[0].get("cache_control").is_none(), "{system:?}");
    assert_eq!(system[1]["cache_control"]["type"], "ephemeral");

    let none = provider()
        .body(&request("", &[user("hi")], &[]))
        .expect("a body");
    assert!(none.get("system").is_none(), "{none}");
}

/// A picture goes as a base64 source, and a model that does not take pictures is TOLD what it is
/// missing rather than handed bytes it will refuse.
#[test]
fn an_image_is_a_base64_source_or_a_named_absence() {
    let png =
        letibot_transcript::testing::data_ref("image/png", &letibot_transcript::testing::png(2, 2));
    let items = vec![TranscriptItem::User {
        speaker: Default::default(),
        parts: vec![
            UserPart::Text {
                text: "look".into(),
            },
            UserPart::Image {
                media_type: "image/png".into(),
                data_ref: png.clone(),
            },
        ],
    }];
    let body = provider().body(&request("", &items, &[])).expect("a body");
    let blocks = body["messages"][0]["content"].as_array().unwrap();
    assert_eq!(blocks[0]["type"], "text");
    assert_eq!(blocks[1]["type"], "image", "{blocks:?}");
    assert_eq!(blocks[1]["source"]["type"], "base64");
    assert_eq!(blocks[1]["source"]["media_type"], "image/png");
    assert!(
        blocks[1]["source"]["data"]
            .as_str()
            .is_some_and(|d| !d.starts_with("data:")),
        "the payload goes without the reference's own prefix: {}",
        blocks[1]["source"]
    );

    // The catalogue says this model reads text only: the picture becomes a sentence naming why.
    // The snapshot's `zai-coding-plan` carries the plan's models, and `glm-5.3` there is text only.
    let blind =
        AnthropicProvider::new(&crate::presets::GLM_ANTHROPIC, Some("glm-5.3"), creds(None))
            .with_catalogue(Arc::new(crate::catalogue::Catalogue::snapshot()));
    let body = blind.body(&request("", &items, &[])).expect("a body");
    let blocks = body["messages"][0]["content"].as_array().unwrap();
    assert_eq!(blocks[1]["type"], "text", "{blocks:?}");
    assert!(
        blocks[1]["text"]
            .as_str()
            .is_some_and(|t| t.contains("not sent") && t.contains("takes text only")),
        "{blocks:?}"
    );
}

/// A `tool_result` answers the `tool_use` above it, and a call that failed says so - the API's
/// `is_error`, which is how a model tells a refusal from an answer.
#[test]
fn a_result_answers_its_call_and_a_failure_carries_is_error() {
    let items = vec![
        user("read it"),
        calls(&["toolu_1"]),
        result("toolu_1", ToolOutcome::Ok, "the page"),
        calls(&["toolu_2"]),
        result(
            "toolu_2",
            ToolOutcome::Failed {
                reason: "no".into(),
            },
            "no such file",
        ),
    ];
    let body = provider().body(&request("", &items, &[])).expect("a body");
    let messages = body["messages"].as_array().unwrap();
    // user, assistant(call), user(result), assistant(call), user(result)
    assert_eq!(messages.len(), 5, "{messages:?}");
    assert_eq!(messages[1]["content"][0]["type"], "tool_use");
    assert_eq!(messages[1]["content"][0]["name"], "read");
    assert_eq!(
        messages[1]["content"][0]["input"],
        json!({"path": "a.txt"}),
        "the raw argument text is sent as the object `input`"
    );
    let answered = &messages[2]["content"][0];
    assert_eq!(answered["type"], "tool_result");
    assert_eq!(answered["tool_use_id"], "toolu_1");
    assert_eq!(answered["content"][0]["text"], "the page");
    assert!(
        answered.get("is_error").is_none(),
        "an answer is not an error: {answered}"
    );
    assert_eq!(messages[4]["content"][0]["is_error"], true);
}

/// A result the OPERATOR ran has no `tool_use` above it, so a `tool_result` would answer an id this
/// request never opened. It becomes the person's own turn, with the output still in the request.
#[test]
fn the_operators_own_result_is_the_persons_turn() {
    let ran = TranscriptItem::ToolResult {
        call_id: "bang-1".into(),
        name: "bash".into(),
        outcome: ToolOutcome::Ok,
        payload: "total 8".into(),
        edits: Vec::new(),
        origin: Some(CallOrigin::Operator { who: "dead".into() }),
        media: None,
    };
    let items = vec![user("! ls"), ran];
    let body = provider().body(&request("", &items, &[])).expect("a body");
    let messages = body["messages"].as_array().unwrap();
    let all = json!(messages);
    assert!(
        !all.to_string().contains("tool_result"),
        "nothing answers it, so it cannot be a result: {messages:?}"
    );
    assert!(
        messages[0]["content"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["text"] == "total 8"),
        "and its output still reaches the model: {messages:?}"
    );
}

/// A call that never came back is filled in the message DIRECTLY below it, which is the only place
/// the API reads a result; a note recorded while the call ran does not push the result down.
#[test]
fn an_unanswered_call_is_filled_and_a_note_never_precedes_the_result() {
    let items = vec![
        user("go"),
        calls(&["toolu_1"]),
        user("room: a message for everyone"),
        result("toolu_1", ToolOutcome::Ok, "done"),
    ];
    let body = provider().body(&request("", &items, &[])).expect("a body");
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(messages[2]["role"], "user");
    assert_eq!(
        messages[2]["content"][0]["type"], "tool_result",
        "a text block before the result is a 400: {messages:?}"
    );
    assert_eq!(messages[2]["content"][0]["tool_use_id"], "toolu_1");
    // The note follows the result, in the same message.
    assert!(
        messages[2]["content"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["text"] == "room: a message for everyone"),
        "{messages:?}"
    );

    // And a transcript that simply ends on a call gets the filler as a user message of its own.
    let lonely = vec![user("go"), calls(&["toolu_9"])];
    let body = provider().body(&request("", &lonely, &[])).expect("a body");
    let messages = body["messages"].as_array().unwrap();
    let last = messages.last().unwrap();
    assert_eq!(last["role"], "user");
    assert_eq!(last["content"][0]["tool_use_id"], "toolu_9");
    assert!(
        last["content"][0]["content"][0]["text"]
            .as_str()
            .is_some_and(|t| t.contains("no result was recorded")),
        "{last}"
    );
}

/// The signature is the whole reason a thinking block can be replayed: the block goes back exactly
/// as it came, in its place, and a thought from another provider has no block this API would read.
#[test]
fn a_signed_thought_goes_back_verbatim_and_an_unsigned_one_is_left_out() {
    let items = vec![
        user("go"),
        plain_reasoning("a GLM thought, signed by nobody"),
        thinking("I should read a.txt.", "EqQBCgIYAhIM1gbc"),
        calls(&["toolu_1"]),
        result("toolu_1", ToolOutcome::Ok, "the page"),
    ];
    let body = provider().body(&request("", &items, &[])).expect("a body");
    let messages = body["messages"].as_array().unwrap();
    let assistant = messages
        .iter()
        .find(|m| m["role"] == "assistant")
        .expect("the assistant turn");
    let blocks = assistant["content"].as_array().unwrap();
    assert_eq!(blocks[0]["type"], "thinking", "{blocks:?}");
    assert_eq!(blocks[0]["thinking"], "I should read a.txt.");
    assert_eq!(blocks[0]["signature"], "EqQBCgIYAhIM1gbc");
    assert_eq!(blocks[1]["type"], "tool_use");
    // The GLM thought is gone: it carries no signature, and an unsigned block is a 400.
    assert!(
        !json!(messages).to_string().contains("a GLM thought"),
        "{messages:?}"
    );
}

/// A redacted block has no text at all; it goes back as the encrypted bytes it arrived as.
#[test]
fn a_redacted_block_goes_back_as_its_bytes() {
    let items = vec![
        user("go"),
        TranscriptItem::Reasoning {
            text: String::new(),
            field: ReasoningField::Redacted {
                data: "EmwKAhIM1gbc".into(),
            },
            truncated: false,
        },
        assistant("done"),
    ];
    let body = provider().body(&request("", &items, &[])).expect("a body");
    let assistant = &body["messages"][1];
    assert_eq!(assistant["role"], "assistant");
    assert_eq!(assistant["content"][0]["type"], "redacted_thinking");
    assert_eq!(assistant["content"][0]["data"], "EmwKAhIM1gbc");
}

/// A tool schema arrives in this tree's dialect rendering and goes out as the API's three fields.
#[test]
fn tool_schemas_become_name_description_and_input_schema() {
    let tools = vec![
        r#"{"type":"function","function":{"name":"read","description":"read a file",
             "parameters":{"type":"object","properties":{"path":{"type":"string"}}}}}"#
            .to_string(),
        r#"{"name":"bash","parameters":{"type":"object"}}"#.to_string(),
    ];
    let body = provider()
        .body(&request("sys", &[user("go")], &tools))
        .expect("a body");
    let tools = body["tools"].as_array().unwrap();
    assert_eq!(tools[0]["name"], "read");
    assert_eq!(tools[0]["description"], "read a file");
    assert_eq!(
        tools[0]["input_schema"]["properties"]["path"]["type"],
        "string"
    );
    assert!(
        tools[0].get("type").is_none(),
        "not the OpenAI wrapper: {}",
        tools[0]
    );
    assert_eq!(tools[1]["name"], "bash");
    assert_eq!(tools[1]["input_schema"]["type"], "object");
}

/// `max_tokens` is required and is a ceiling the turn and the operator both narrow. Thinking is
/// asked for in the current mode, and a turn that asks not to think sends no field at all -
/// `disabled` is a 400 wherever thinking is always on.
#[test]
fn max_tokens_is_required_and_thinking_is_the_adaptive_mode() {
    let items = vec![user("hi")];
    let bare = provider().body(&request("", &items, &[])).expect("a body");
    assert_eq!(bare["max_tokens"], DEFAULT_MAX_TOKENS);
    assert!(
        bare.get("thinking").is_none(),
        "no thinking was asked for: {bare}"
    );

    let mut p = provider();
    p.thinking = true;
    p.sampling = json!({"max_tokens": 4000, "temperature": 0.2, "presence_penalty": 0.4});
    let thinking = p.body(&request("", &items, &[])).expect("a body");
    assert_eq!(thinking["max_tokens"], 4000);
    assert_eq!(thinking["thinking"]["type"], "adaptive");
    assert_eq!(
        thinking["temperature"], 0.2,
        "the operator's sampling passes"
    );
    assert!(
        thinking.get("presence_penalty").is_none(),
        "a field this API does not have would refuse the whole request: {thinking}"
    );

    let capped = p
        .body(&TurnRequest {
            max_output_tokens: Some(256),
            ..request("", &items, &[])
        })
        .expect("a body");
    assert_eq!(capped["max_tokens"], 256, "the turn's cap wins");

    let quiet = p
        .body(&TurnRequest {
            thinking: Some(false),
            ..request("", &items, &[])
        })
        .expect("a body");
    assert!(
        quiet.get("thinking").is_none(),
        "asking not to think asks for nothing: {quiet}"
    );
}

// -- the stream --------------------------------------------------------------------------------

/// Text, a signed thought and a tool call, with usage from both events: the arithmetic the API
/// documents is `input + cache_read + cache_creation` for the prompt.
#[test]
fn the_events_build_text_a_signed_thought_a_call_and_the_usage() {
    let mut acc = stream::Accumulator::default();
    let events = [
        json!({"type": "message_start", "message": {"model": "claude-opus-5-5", "usage":
            {"input_tokens": 20, "cache_read_input_tokens": 100,
             "cache_creation_input_tokens": 5, "output_tokens": 1}}}),
        json!({"type": "content_block_start", "index": 0,
               "content_block": {"type": "thinking", "thinking": "", "signature": ""}}),
        json!({"type": "content_block_delta", "index": 0,
               "delta": {"type": "thinking_delta", "thinking": "the page"}}),
        json!({"type": "content_block_delta", "index": 0,
               "delta": {"type": "thinking_delta", "thinking": " first"}}),
        json!({"type": "content_block_delta", "index": 0,
               "delta": {"type": "signature_delta", "signature": "EqQBCgIY"}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "content_block_start", "index": 1,
               "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 1,
               "delta": {"type": "text_delta", "text": "Hel"}}),
        json!({"type": "content_block_delta", "index": 1,
               "delta": {"type": "text_delta", "text": "lo"}}),
        json!({"type": "content_block_stop", "index": 1}),
        json!({"type": "content_block_start", "index": 2,
               "content_block": {"type": "tool_use", "id": "toolu_1", "name": "read",
                                 "input": {}}}),
        json!({"type": "content_block_delta", "index": 2,
               "delta": {"type": "input_json_delta", "partial_json": "{\"path\": \"a.txt\"}"}}),
        json!({"type": "content_block_stop", "index": 2}),
        json!({"type": "message_delta",
               "delta": {"stop_reason": "tool_use", "stop_sequence": null},
               "usage": {"output_tokens": 9}}),
        json!({"type": "message_stop"}),
    ];
    let mut n = 0;
    for e in &events {
        n += acc.absorb(e).len();
    }
    assert_eq!(
        n, 6,
        "two thinking deltas, two text, and the call's own start and argument"
    );
    let mut creds = creds(None);
    creds.prices.insert(
        "claude-opus-5-5".into(),
        crate::presets::Prices {
            input: 4.0,
            cached: 0.4,
            output: 20.0,
        },
    );
    let done = acc.finish(
        "claude-opus-5-5",
        &creds,
        12,
        &crate::presets::ANTHROPIC,
        &crate::catalogue::Catalogue::default(),
    );
    assert_eq!(done.text, "Hello");
    assert_eq!(done.reasoning, "the page first");
    assert_eq!(done.thoughts.len(), 1);
    assert_eq!(done.thoughts[0].text, "the page first");
    assert_eq!(
        done.thoughts[0].field,
        ReasoningField::Signed {
            signature: "EqQBCgIY".into()
        }
    );
    assert_eq!(done.tool_calls.len(), 1);
    assert_eq!(done.tool_calls[0].id, "toolu_1");
    assert_eq!(done.tool_calls[0].arguments, r#"{"path": "a.txt"}"#);
    assert_eq!(done.finish, Finish::ToolCalls);
    assert_eq!(
        done.cost.prompt_tokens, 125,
        "20 fresh + 100 read + 5 written"
    );
    assert_eq!(done.cost.cached_tokens, 100);
    assert_eq!(done.cost.generated_tokens, 9);
    // (125-100)*4 + 100*0.4 + 9*20 = 100 + 40 + 180 = 320 micro-USD.
    assert_eq!(done.cost.micros_usd, Some(320));
    let usage = done.raw_usage.expect("the provider's own usage is kept");
    assert!(usage.contains("cache_creation_input_tokens"), "{usage}");
    assert!(usage.contains("cache_read_input_tokens"), "{usage}");
}

/// A redacted block arrives whole and has nothing to show: it is carried, and no delta of it
/// reaches a head.
#[test]
fn a_redacted_block_is_carried_and_nothing_of_it_is_shown() {
    let mut acc = stream::Accumulator::default();
    let deltas = acc.absorb(&json!({"type": "content_block_start", "index": 0,
        "content_block": {"type": "redacted_thinking", "data": "EmwKAhIM1gbc"}}));
    assert!(deltas.is_empty(), "{deltas:?}");
    acc.absorb(&json!({"type": "content_block_stop", "index": 0}));
    let done = acc.finish(
        "m",
        &creds(None),
        0,
        &crate::presets::ANTHROPIC,
        &crate::catalogue::Catalogue::default(),
    );
    assert_eq!(
        done.thoughts[0].field,
        ReasoningField::Redacted {
            data: "EmwKAhIM1gbc".into()
        }
    );
    assert_eq!(done.thoughts[0].text, "");
}

/// The refusal a 200 already promised would not come: its type is the status a caller classifies
/// on, and an overloaded provider is a 529 rather than a broken server.
#[test]
fn an_error_event_is_the_refusal_its_type_names() {
    let refusal = in_band_refusal(
        &json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}),
        Some(30),
    )
    .expect("an error event is a refusal");
    match refusal {
        BackendError::Refused {
            status,
            body,
            retry_after,
        } => {
            assert_eq!(status, 529);
            assert_eq!(body, "Overloaded (overloaded_error)");
            assert_eq!(retry_after, Some(30));
        }
        other => panic!("{other:?}"),
    }
    assert!(
        in_band_refusal(&json!({"type": "content_block_stop", "index": 0}), None).is_none(),
        "only an error event is a refusal"
    );
    // Every stop reason the API documents, mapped the way the tree's `Finish` reads.
    assert_eq!(stream::finish_of("end_turn"), Finish::Stop);
    assert_eq!(stream::finish_of("stop_sequence"), Finish::Stop);
    assert_eq!(stream::finish_of("max_tokens"), Finish::Length);
    assert_eq!(stream::finish_of("tool_use"), Finish::ToolCalls);
    assert_eq!(
        stream::finish_of("pause_turn"),
        Finish::Other("pause_turn".into())
    );
    assert_eq!(
        stream::finish_of("refusal"),
        Finish::Other("refusal".into())
    );
}

// -- the wire ----------------------------------------------------------------------------------

/// The headers are the protocol: the key in `x-api-key`, the version this client speaks, and a
/// stream asked for.
#[test]
fn the_request_carries_the_key_the_version_and_the_stream() {
    let f = fake(200, "");
    let p = provider_at(&f.url);
    let items = vec![user("hi")];
    let _ = run(&p, &request("be terse", &items, &[]));
    assert_eq!(f.count(), 1);
    assert_eq!(f.header(0, "x-api-key").as_deref(), Some("sk-ant-test"));
    assert_eq!(
        f.header(0, "anthropic-version").as_deref(),
        Some(API_VERSION)
    );
    assert_eq!(f.header(0, "accept").as_deref(), Some("text/event-stream"));
    assert_eq!(f.body(0)["stream"], true);
    assert_eq!(f.body(0)["model"], "claude-opus-5-5");
}

/// A tool round trip over the wire: the call arrives, and the SECOND request is the one that
/// proves the mapping - it must carry the assistant's thinking block with its signature, the
/// `tool_use` it proposed, and the `tool_result` that answers it.
#[test]
fn a_tool_round_trip_carries_the_signed_thought_and_the_result() {
    let mut first = String::new();
    first.push_str(&event(
        json!({"type": "message_start", "message": {"model": "claude-opus-5-5",
        "usage": {"input_tokens": 30, "output_tokens": 1}}}),
    ));
    first.push_str(&event(json!({"type": "content_block_start", "index": 0,
        "content_block": {"type": "thinking", "thinking": "", "signature": ""}})));
    first.push_str(&event(json!({"type": "content_block_delta", "index": 0,
        "delta": {"type": "thinking_delta", "thinking": "read a.txt first"}})));
    first.push_str(&event(json!({"type": "content_block_delta", "index": 0,
        "delta": {"type": "signature_delta", "signature": "SIG-1"}})));
    first.push_str(&event(json!({"type": "content_block_stop", "index": 0})));
    first.push_str(&event(json!({"type": "content_block_start", "index": 1,
        "content_block": {"type": "tool_use", "id": "toolu_01", "name": "read", "input": {}}})));
    first.push_str(&event(json!({"type": "content_block_delta", "index": 1,
        "delta": {"type": "input_json_delta", "partial_json": "{\"path\":\"a.txt\"}"}})));
    first.push_str(&event(json!({"type": "content_block_stop", "index": 1})));
    first.push_str(&event(json!({"type": "message_delta",
        "delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 12}})));
    first.push_str(&event(json!({"type": "message_stop"})));

    let first: &'static str = Box::leak(first.into_boxed_str());
    let second: &'static str =
        Box::leak(text_answer("claude-opus-5-5", "the page").into_boxed_str());
    let answers = Arc::new(Mutex::new(0usize));
    let f = fake_answering(move |_| {
        let mut n = answers.lock().unwrap();
        let script = if *n == 0 { first } else { second };
        *n += 1;
        (200, script.to_string(), Vec::new())
    });
    let p = provider_at(&f.url);

    // Turn one: the model proposes a call.
    let asked = vec![user("read a.txt")];
    let (_, done) = run(&p, &request("be terse", &asked, &[]));
    let done = done.expect("a tool call answers the first request");
    assert_eq!(done.finish, Finish::ToolCalls);
    assert_eq!(done.tool_calls[0].id, "toolu_01");
    assert_eq!(
        done.thoughts[0].field,
        ReasoningField::Signed {
            signature: "SIG-1".into()
        },
        "the signature came back with the block"
    );

    // Turn two: the transcript now holds what the model said and what the call answered.
    let carried = vec![
        user("read a.txt"),
        thinking("read a.txt first", "SIG-1"),
        TranscriptItem::Assistant {
            text: String::new(),
            tool_calls: done.tool_calls.clone(),
            truncated: false,
        },
        result("toolu_01", ToolOutcome::Ok, "the page"),
    ];
    let (_, done) = run(&p, &request("be terse", &carried, &[]));
    assert_eq!(done.expect("an answer").text, "the page");
    let body = f.body(1);
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 3, "user, assistant, user: {messages:?}");
    let assistant = &messages[1]["content"];
    assert_eq!(assistant[0]["type"], "thinking");
    assert_eq!(
        assistant[0]["signature"], "SIG-1",
        "a signed block goes back unmodified, or the API refuses the turn"
    );
    assert_eq!(assistant[1]["type"], "tool_use");
    assert_eq!(messages[2]["content"][0]["type"], "tool_result");
    assert_eq!(messages[2]["content"][0]["tool_use_id"], "toolu_01");
    assert_eq!(messages[2]["content"][0]["content"][0]["text"], "the page");
}

/// A refusal keeps the provider's own type, its `retry-after`, and the `request-id` an operator
/// takes to support.
#[test]
fn a_refusal_keeps_the_type_the_retry_after_and_the_request_id() {
    let f = fake_headers(
        429,
        r#"{"type":"error","error":{"type":"rate_limit_error","message":"slow down"},"request_id":"req_011C"}"#,
        &[("retry-after", "30"), ("request-id", "req_011C")],
    );
    let p = provider_at(&f.url);
    let items = vec![user("hi")];
    let (_, done) = run(&p, &request("", &items, &[]));
    match done.expect_err("a 429 is a refusal") {
        BackendError::Refused {
            status,
            body,
            retry_after,
        } => {
            assert_eq!(status, 429);
            assert!(body.contains("slow down"), "{body}");
            assert!(body.contains("rate_limit_error"), "{body}");
            assert!(body.contains("req_011C"), "{body}");
            assert_eq!(retry_after, Some(30));
        }
        other => panic!("{other:?}"),
    }
}

/// A 200 whose stream carries an error is a refusal, not a half answer.
#[test]
fn a_mid_stream_error_is_a_refusal() {
    let script = format!(
        "{}{}",
        event(
            json!({"type": "message_start", "message": {"model": "claude-opus-5-5",
            "usage": {"input_tokens": 3, "output_tokens": 0}}})
        ),
        event(
            json!({"type": "error", "error": {"type": "overloaded_error",
            "message": "Overloaded"}})
        )
    );
    let script: &'static str = Box::leak(script.into_boxed_str());
    let f = fake(200, script);
    let p = provider_at(&f.url);
    let items = vec![user("hi")];
    let (_, done) = run(&p, &request("", &items, &[]));
    match done.expect_err("an error event is a refusal") {
        BackendError::Refused { status, body, .. } => {
            assert_eq!(status, 529, "{body}");
        }
        other => panic!("{other:?}"),
    }
}

/// A socket that closes before `message_stop` is a fragment, not an answer.
#[test]
fn a_stream_that_stops_without_message_stop_is_a_fragment() {
    let script = format!(
        "{}{}",
        event(
            json!({"type": "message_start", "message": {"model": "claude-opus-5-5",
            "usage": {"input_tokens": 3, "output_tokens": 0}}})
        ),
        event(json!({"type": "content_block_delta", "index": 0,
            "delta": {"type": "text_delta", "text": "half a thought"}}))
    );
    let script: &'static str = Box::leak(script.into_boxed_str());
    let f = fake(200, script);
    let p = provider_at(&f.url);
    let items = vec![user("hi")];
    let (_, done) = run(&p, &request("", &items, &[]));
    match done.expect_err("a fragment is not an answer") {
        BackendError::Unreachable { message, .. } => {
            assert!(message.contains("message_stop"), "{message}");
        }
        other => panic!("{other:?}"),
    }
}

/// The API's own remedy for a thinking block it will not accept is to strip it, and this client
/// says so once and takes the round again without the blocks - the alternative is a conversation
/// that fails at every turn because its history was once edited.
#[test]
fn a_refused_thinking_block_is_dropped_once_and_the_request_goes_again() {
    let answer = text_answer("claude-opus-5-5", "recovered");
    let answer: &'static str = Box::leak(answer.into_boxed_str());
    const REFUSAL: &str = r#"{"type":"error","error":{"type":"invalid_request_error","message":"messages.1.content.0: `thinking` or `redacted_thinking` blocks in the latest assistant message cannot be modified."}}"#;
    let f = fake_answering(move |body| match body.contains("\"thinking\"") {
        true => (400, REFUSAL.to_string(), Vec::new()),
        false => (200, answer.to_string(), Vec::new()),
    });
    let notices: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = notices.clone();
    let p = provider_at(&f.url).with_notes(Arc::new(move |said| {
        sink.lock().unwrap().push(said);
    }));
    let carried = vec![
        user("go"),
        thinking("a thought", "SIG-1"),
        assistant("an answer"),
        user("next"),
    ];
    let (_, done) = run(&p, &request("", &carried, &[]));
    assert_eq!(done.expect("the retry answers").text, "recovered");
    assert_eq!(f.count(), 2, "one retry, no ladder");
    assert!(
        !f.body(1).to_string().contains("\"thinking\""),
        "the retry carries no thinking blocks: {}",
        f.body(1)
    );
    let said = notices.lock().unwrap();
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].contains("thinking"), "{said:?}");
}

/// A model with only the old manual mode refuses `adaptive` by name, and the API documents the
/// mode to use instead - so the request goes once more with it, and the budget is legal.
#[test]
fn a_model_without_adaptive_thinking_is_asked_again_in_the_manual_mode() {
    let answer = text_answer("claude-sonnet-4-6", "thought about it");
    let answer: &'static str = Box::leak(answer.into_boxed_str());
    const REFUSAL: &str = r#"{"type":"error","error":{"type":"invalid_request_error","message":"adaptive thinking is not supported on this model"}}"#;
    let f = fake_answering(move |body| match body.contains("\"adaptive\"") {
        true => (400, REFUSAL.to_string(), Vec::new()),
        false => (200, answer.to_string(), Vec::new()),
    });
    let mut p = provider_at(&f.url);
    p.thinking = true;
    let items = vec![user("think")];
    let (_, done) = run(&p, &request("", &items, &[]));
    assert_eq!(
        done.expect("the manual mode answers").text,
        "thought about it"
    );
    assert_eq!(f.count(), 2);
    let body = f.body(1);
    assert_eq!(body["thinking"]["type"], "enabled", "{body}");
    let budget = body["thinking"]["budget_tokens"].as_u64().unwrap();
    let max = body["max_tokens"].as_u64().unwrap();
    assert!(
        (1024..max).contains(&budget),
        "budget {budget} of max {max}"
    );
}
