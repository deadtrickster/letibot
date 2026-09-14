//! The client against a fake provider: the request it sends, the stream it
//! reads, a refusal, and an abort mid-stream.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use letibot_backend::{BackendError, Delta, Finish, MessagesBackend, StreamFlow, TurnRequest};
use letibot_provider::keys::Credentials;
use letibot_provider::openai::OpenAiProvider;
use letibot_transcript::{TranscriptItem, UserPart};

struct Fake {
    url: String,
    seen: Arc<Mutex<Vec<(String, String)>>>, // (auth header, body)
}

fn fake(script: &'static str, status: u16) -> Fake {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "http://{}/v1/chat/completions",
        listener.local_addr().unwrap()
    );
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut conn) = conn else { continue };
            let mut reader = BufReader::new(conn.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let (mut len, mut auth) = (0usize, String::new());
            loop {
                let mut h = String::new();
                if reader.read_line(&mut h).unwrap_or(0) == 0 {
                    break;
                }
                let h = h.trim_end();
                if h.is_empty() {
                    break;
                }
                // ureq sends header names in lower case; a real provider reads
                // them case-insensitively, and so does this.
                let lower = h.to_ascii_lowercase();
                if let Some(v) = lower.strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap();
                }
                if lower.starts_with("authorization:") {
                    auth = h[14..].trim().to_string();
                }
            }
            let mut body = vec![0u8; len];
            reader.read_exact(&mut body).unwrap();
            seen2
                .lock()
                .unwrap()
                .push((auth, String::from_utf8_lossy(&body).into_owned()));
            let _ = write!(
                conn,
                "HTTP/1.1 {status} X\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{script}",
                script.len()
            );
            let _ = conn.flush();
        }
    });
    Fake { url, seen }
}

fn provider(url: &str) -> OpenAiProvider {
    let creds = Credentials {
        key: "sk-test".into(),
        from: "test".into(),
        prices: Default::default(),
        url: Some(url.into()),
    };
    OpenAiProvider::new(
        &letibot_provider::presets::DEEPSEEK,
        Some("deepseek-chat"),
        creds,
    )
}

const SCRIPT: &str = "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"think\"}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_9\",\"function\":{\"name\":\"read\",\"arguments\":\"{\\\"path\\\":\"}}]}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"a.txt\\\"}\"}}]}}]}\n\n\
data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n\
data: {\"choices\":[],\"usage\":{\"prompt_tokens\":50,\"completion_tokens\":9,\"prompt_cache_hit_tokens\":20}}\n\n\
data: [DONE]\n\n";

#[test]
fn a_turn_streams_reasoning_text_and_a_tool_call_and_the_request_carries_the_key_and_tools() {
    let f = fake(SCRIPT, 200);
    let p = provider(&f.url);
    let items = vec![TranscriptItem::User {
        parts: vec![UserPart::Text {
            text: "read a.txt".into(),
        }],
    }];
    let tools = vec![
        r#"{"type":"function","function":{"name":"read","parameters":{"type":"object"}}}"#
            .to_string(),
    ];
    let req = TurnRequest {
        system: "be terse",
        tools_json: &tools,
        items: &items,
        max_output_tokens: Some(256),
    };
    let mut deltas = Vec::new();
    let done = p
        .complete(&req, &mut |d| {
            deltas.push(d.clone());
            StreamFlow::Continue
        })
        .unwrap();
    assert_eq!(done.text, "Hello");
    assert_eq!(done.reasoning, "think");
    assert_eq!(done.tool_calls[0].name, "read");
    assert_eq!(done.tool_calls[0].arguments, "{\"path\":\"a.txt\"}");
    assert_eq!(done.finish, Finish::ToolCalls);
    assert_eq!(done.cost.prompt_tokens, 50);
    assert_eq!(done.cost.cached_tokens, 20);
    assert_eq!(done.cost.generated_tokens, 9);
    assert_eq!(done.cost.micros_usd, None, "unpriced is None, not zero");
    assert!(matches!(deltas[0], Delta::Reasoning(_)));
    assert!(matches!(deltas[1], Delta::Text(_)));
    let seen = f.seen.lock().unwrap();
    let (auth, body) = &seen[0];
    assert_eq!(auth, "Bearer sk-test");
    let v: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(v["model"], "deepseek-chat");
    assert_eq!(v["stream"], true);
    assert_eq!(v["stream_options"]["include_usage"], true);
    assert_eq!(v["max_tokens"], 256);
    assert_eq!(v["messages"][0]["role"], "system");
    assert_eq!(v["messages"][1]["content"], "read a.txt");
    assert_eq!(v["tools"][0]["function"]["name"], "read");
}

#[test]
fn a_refusal_carries_the_providers_own_message() {
    let f = fake(
        "{\"error\":{\"message\":\"Authentication Fails\",\"type\":\"authentication_error\"}}",
        401,
    );
    let p = provider(&f.url);
    let items = vec![];
    let req = TurnRequest {
        system: "",
        tools_json: &[],
        items: &items,
        max_output_tokens: None,
    };
    let err = p.complete(&req, &mut |_| StreamFlow::Continue).unwrap_err();
    match err {
        BackendError::Refused { status, body } => {
            assert_eq!(status, 401);
            assert_eq!(body, "Authentication Fails");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn stopping_mid_stream_is_an_abort_not_an_answer() {
    let f = fake(SCRIPT, 200);
    let p = provider(&f.url);
    let items = vec![];
    let req = TurnRequest {
        system: "",
        tools_json: &[],
        items: &items,
        max_output_tokens: None,
    };
    let mut n = 0;
    let err = p
        .complete(&req, &mut |_| {
            n += 1;
            if n == 2 {
                StreamFlow::Stop
            } else {
                StreamFlow::Continue
            }
        })
        .unwrap_err();
    assert!(matches!(err, BackendError::Aborted));
}
