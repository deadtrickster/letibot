//! A door declared in providers.toml alone completes a tool-calling turn against a stub on
//! loopback: Kimi with its quirks as keys, and an Ollama endpoint with none.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use letibot_backend::{Finish, MessagesBackend, StreamFlow, TurnRequest};
use letibot_provider::openai::OpenAiProvider;
use letibot_provider::presets::Origin;
use letibot_transcript::{ToolCall, ToolOutcome, TranscriptItem, UserPart};
use serde_json::Value;

const CALL: &str = "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"look first\"}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"type\":\"function\",\
\"function\":{\"name\":\"read\",\"arguments\":\"{\\\"path\\\":\\\"a.txt\\\"}\"}}]}}]}\n\n\
data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n\
data: [DONE]\n\n";

const ANSWER: &str = "data: {\"choices\":[{\"delta\":{\"content\":\"it says hi\"}}]}\n\n\
data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
data: [DONE]\n\n";

/// A chat-completions server: a tool call to the first request, text to every later one.
/// Records each request's path and body.
/// Each request's path and body, in arrival order.
type Seen = Arc<Mutex<Vec<(String, Value)>>>;

fn stub(path: &str) -> (String, Seen) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}{path}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorded = seen.clone();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut conn) = conn else { continue };
            let mut reader = BufReader::new(conn.try_clone().unwrap());
            let mut first = String::new();
            reader.read_line(&mut first).unwrap();
            let mut length = 0usize;
            loop {
                let mut h = String::new();
                if reader.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
                    break;
                }
                if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = v.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; length];
            reader.read_exact(&mut body).unwrap();
            let n = {
                let mut seen = recorded.lock().unwrap();
                let target = first.split_whitespace().nth(1).unwrap_or("").to_string();
                seen.push((target, serde_json::from_slice(&body).unwrap()));
                seen.len()
            };
            let answer = if n == 1 { CALL } else { ANSWER };
            let _ = write!(
                conn,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                answer.len()
            );
        }
    });
    (url, seen)
}

fn providers_toml(tag: &str, body: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("letibot-config-door-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    let f = d.join("providers.toml");
    std::fs::write(&f, body).unwrap();
    f
}

/// The `level` field has an untyped enum, which is the schema Kimi refuses.
const TOOLS: &[&str] = &[
    r#"{"type":"function","function":{"name":"read","description":"read a file","parameters":{"type":"object","properties":{"path":{"type":"string"},"level":{"enum":["a","b"]}}}}}"#,
];

/// One tool-calling turn: the model calls `read`, the result goes back, the model answers.
fn turn(p: &OpenAiProvider) {
    let tools: Vec<String> = TOOLS.iter().map(|t| t.to_string()).collect();
    let mut items = vec![TranscriptItem::User {
        speaker: Default::default(),
        parts: vec![UserPart::Text {
            text: "read a.txt".into(),
        }],
    }];
    let called = p
        .complete(&req(&tools, &items), &mut |_| StreamFlow::Continue)
        .expect("the stub answers the first round");
    assert_eq!(called.finish, Finish::ToolCalls);
    assert_eq!(
        called.tool_calls,
        vec![ToolCall {
            id: "c1".into(),
            name: "read".into(),
            arguments: "{\"path\":\"a.txt\"}".into(),
        }]
    );
    items.push(TranscriptItem::Reasoning {
        text: called.reasoning.clone(),
        field: letibot_transcript::ReasoningField::ReasoningContent,
        truncated: false,
    });
    items.push(TranscriptItem::Assistant {
        text: String::new(),
        tool_calls: called.tool_calls.clone(),
        truncated: false,
    });
    items.push(TranscriptItem::ToolResult {
        call_id: "c1".into(),
        name: "read".into(),
        outcome: ToolOutcome::Ok,
        payload: "hi".into(),
        edits: Vec::new(),
        origin: None,
        media: None,
    });
    let answered = p
        .complete(&req(&tools, &items), &mut |_| StreamFlow::Continue)
        .expect("the stub answers the second round");
    assert_eq!(answered.text, "it says hi");
    assert_eq!(answered.finish, Finish::Stop);
}

fn req<'a>(tools: &'a [String], items: &'a [TranscriptItem]) -> TurnRequest<'a> {
    TurnRequest {
        system: "you read files",
        tools_json: tools,
        items,
        tail: &[],
        max_output_tokens: Some(512),
        thinking: None,
        json: false,
    }
}

#[test]
fn a_kimi_door_from_config_alone_completes_a_tool_turn_with_its_quirks_on_the_wire() {
    let (url, seen) = stub("/coding/v1/chat/completions");
    let f = providers_toml(
        "kimi",
        &format!(
            "[provider.mykimi]\nprotocol = \"chat-completions\"\nurl = \"{url}\"\n\
             model = \"k3\"\nkey = \"sk-test\"\nthinking = \"thinking\"\n\
             effort_field = \"reasoning_effort\"\neffort_levels = [\"off\", \"low\", \"high\", \"max\"]\n\
             reasoning_echo = \"always\"\nreasoning_required = true\n\
             tool_schema = \"typed-enum\"\ncache_key = true\n\
             fixed = [\"temperature\"]\n"
        ),
    );
    let door = letibot_provider::registry::lookup("mykimi", Some(&f)).unwrap();
    assert_eq!(door.origin, Origin::Config);
    let creds = letibot_provider::keys::resolve(door, None, Some(&f)).unwrap();
    let p = OpenAiProvider::new(door, None, creds).with_effort(Some("medium"));
    assert!(
        p.effort_notice()
            .is_some_and(|n| n.contains("`medium` is sent as `high`")),
        "{:?}",
        p.effort_notice()
    );
    turn(&p);
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    for (path, body) in seen.iter() {
        assert_eq!(path, "/coding/v1/chat/completions");
        assert_eq!(body["model"], "k3");
        assert_eq!(body["thinking"], serde_json::json!({"type": "enabled"}));
        assert_eq!(body["reasoning_effort"], "high");
        assert!(body["prompt_cache_key"].is_string(), "{body}");
        let level = &body["tools"][0]["function"]["parameters"]["properties"]["level"];
        assert_eq!(level["type"], "string", "{body}");
    }
    let second = &seen[1].1["messages"];
    let call = second
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["tool_calls"].is_array())
        .expect("the assistant's call went back");
    assert_eq!(call["reasoning_content"], "look first");
}

#[test]
fn an_ollama_door_from_config_alone_completes_a_tool_turn_with_a_plain_body() {
    let (url, seen) = stub("/v1/chat/completions");
    let f = providers_toml(
        "ollama",
        &format!(
            "[provider.ollama]\nprotocol = \"chat-completions\"\nurl = \"{url}\"\n\
             model = \"qwen3-coder\"\nkey = \"ollama\"\nwindow = 32768\n\
             max_tokens_field = \"max_completion_tokens\"\n"
        ),
    );
    let door = letibot_provider::registry::lookup("ollama", Some(&f)).unwrap();
    let creds = letibot_provider::keys::resolve(door, None, Some(&f)).unwrap();
    let p = OpenAiProvider::new(door, None, creds).with_effort(Some("high"));
    assert!(
        p.effort_notice()
            .is_some_and(|n| n.contains("no effort switch")),
        "{:?}",
        p.effort_notice()
    );
    turn(&p);
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    for (path, body) in seen.iter() {
        assert_eq!(path, "/v1/chat/completions");
        assert_eq!(body["model"], "qwen3-coder");
        assert_eq!(body["max_completion_tokens"], 512);
        for absent in [
            "max_tokens",
            "thinking",
            "reasoning_effort",
            "prompt_cache_key",
            "response_format",
        ] {
            assert!(body.get(absent).is_none(), "{absent} in {body}");
        }
        let level = &body["tools"][0]["function"]["parameters"]["properties"]["level"];
        assert!(level.get("type").is_none(), "plain schemas: {body}");
    }
}
