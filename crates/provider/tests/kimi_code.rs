//! Kimi's coding endpoint against a fake on localhost: the request it needs, and the two
//! refusals it is known for. No call leaves this box — the guest has no key and no network to
//! Moonshot, so every fact here is the client's own shape rather than the endpoint's answer.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use letibot_backend::{BackendError, Finish, MessagesBackend, StreamFlow, TurnRequest};
use letibot_provider::keys::Credentials;
use letibot_provider::openai::OpenAiProvider;
use letibot_provider::presets::KIMI_CODE;
use letibot_transcript::{ToolCall, TranscriptItem, UserPart};

/// One request as it arrived: the whole header set (names lowercased) and the body.
struct Request {
    headers: Vec<(String, String)>,
    body: String,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).expect("the body is JSON")
    }
}

struct Fake {
    url: String,
    seen: Arc<Mutex<Vec<Request>>>,
}

/// A server for one status and one body. `url` carries the coding plan's path, so the request
/// that arrives is the one the preset would post.
fn fake(status: u16, answer: &'static str) -> Fake {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "http://{}/coding/v1/chat/completions",
        listener.local_addr().unwrap()
    );
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorded = seen.clone();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut conn) = conn else { continue };
            let mut reader = BufReader::new(conn.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let (mut length, mut headers) = (0usize, Vec::new());
            loop {
                let mut h = String::new();
                if reader.read_line(&mut h).unwrap_or(0) == 0 {
                    break;
                }
                let h = h.trim_end();
                if h.is_empty() {
                    break;
                }
                let (name, value) = h.split_once(':').unwrap_or((h, ""));
                let (name, value) = (name.trim().to_ascii_lowercase(), value.trim().to_string());
                if name == "content-length" {
                    length = value.parse().unwrap_or(0);
                }
                headers.push((name, value));
            }
            let mut body = vec![0u8; length];
            reader.read_exact(&mut body).unwrap();
            recorded.lock().unwrap().push(Request {
                headers,
                body: String::from_utf8_lossy(&body).into_owned(),
            });
            let _ = write!(
                conn,
                "HTTP/1.1 {status} X\r\nContent-Type: text/event-stream\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                answer.len()
            );
            let _ = conn.flush();
        }
    });
    Fake { url, seen }
}

fn provider(url: &str) -> OpenAiProvider {
    let creds = Credentials {
        key: "sk-kimi-test".into(),
        from: "test".into(),
        prices: Default::default(),
        url: Some(url.into()),
    };
    OpenAiProvider::new(&KIMI_CODE, Some("k3"), creds)
}

const SCRIPT: &str = "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"let me look\"}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\"the file says hi\"}}]}\n\n\
data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
data: {\"choices\":[],\"usage\":{\"prompt_tokens\":40,\"completion_tokens\":7}}\n\n\
data: [DONE]\n\n";

/// A transcript whose assistant called a tool and recorded no thought: the shape Kimi's own
/// client answers with `reasoning_content: ""`, and the shape this harness produced before.
fn a_call_with_no_thought() -> Vec<TranscriptItem> {
    vec![
        TranscriptItem::User {
            speaker: Default::default(),
            parts: vec![UserPart::Text {
                text: "read a.txt".into(),
            }],
        },
        TranscriptItem::Assistant {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: "c1".into(),
                name: "read".into(),
                arguments: "{\"path\":\"a.txt\"}".into(),
            }],
            truncated: false,
        },
        TranscriptItem::ToolResult {
            call_id: "c1".into(),
            name: "read".into(),
            outcome: letibot_transcript::ToolOutcome::Ok,
            payload: "hi".into(),
            edits: Vec::new(),
            origin: None,
            media: None,
        },
    ]
}

const TOOLS: &[&str] = &[
    r#"{"type":"function","function":{"name":"read","description":"read a file","parameters":{"type":"object","properties":{"path":{"type":"string"}}}}}"#,
];

/// The whole request a coding-plan turn is: our own name in `User-Agent`, the plan's key, the
/// thinking switch, a cache key, and the empty thought on the tool call. And it streams.
#[test]
fn a_request_names_itself_carries_the_thinking_switch_and_an_empty_thought() {
    let f = fake(200, SCRIPT);
    let p = provider(&f.url);
    let items = a_call_with_no_thought();
    let tools: Vec<String> = TOOLS.iter().map(|t| t.to_string()).collect();
    let req = TurnRequest {
        system: "be terse",
        tools_json: &tools,
        items: &items,
        tail: &[],
        max_output_tokens: Some(512),
        thinking: Some(true),
    };
    let done = p
        .complete(&req, &mut |_| StreamFlow::Continue)
        .expect("the answer streams");
    assert_eq!(done.text, "the file says hi");
    assert_eq!(done.reasoning, "let me look");
    assert_eq!(done.finish, Finish::Stop);

    let seen = f.seen.lock().unwrap();
    let sent = &seen[0];
    assert_eq!(sent.header("authorization"), Some("Bearer sk-kimi-test"));
    assert_eq!(sent.header("content-type"), Some("application/json"));
    assert_eq!(sent.header("accept"), Some("text/event-stream"));
    // Our own name, and no agent's: Moonshot calls a rewritten User-Agent a violation.
    let ua = sent
        .header("user-agent")
        .expect("the request says who it is");
    assert!(ua.starts_with("letibot/"), "{ua}");
    for impostor in ["kimi", "claude", "codex", "opencode"] {
        assert!(!ua.to_ascii_lowercase().contains(impostor), "{ua}");
    }

    let body = sent.json();
    assert_eq!(body["model"], "k3");
    assert_eq!(body["thinking"], serde_json::json!({"type": "enabled"}));
    assert_eq!(body["stream_options"]["include_usage"], true);
    assert_eq!(body["max_tokens"], 512);
    let key = body["prompt_cache_key"]
        .as_str()
        .expect("Kimi's implicit cache is keyed by this");
    assert!(key.starts_with("letibot-"), "{key}");
    assert_eq!(body["tools"][0]["function"]["name"], "read");
    let call = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["tool_calls"].is_array())
        .expect("the tool call is on the wire");
    assert_eq!(
        call["reasoning_content"], "",
        "the server refuses a tool-call message with no reasoning_content at all: {call}"
    );
}

/// A 429 that names the plan's spent quota is a refusal, not a wait: one request, the endpoint's
/// own code kept beside its words, and no second call from this client. `crates/http`'s policy
/// reads that same code as a quota rather than a rate limit.
#[test]
fn a_quota_429_is_not_retried_and_keeps_kimis_own_code() {
    let f = fake(
        429,
        "{\"error\":{\"code\":\"exceeded_current_quota_error\",\"message\":\"You have reached \
         your usage limit.\"}}",
    );
    let p = provider(&f.url);
    let items = vec![TranscriptItem::User {
        speaker: Default::default(),
        parts: vec![UserPart::Text { text: "hi".into() }],
    }];
    let req = TurnRequest {
        system: "",
        tools_json: &[],
        items: &items,
        tail: &[],
        max_output_tokens: None,
        thinking: None,
    };
    let why = p.complete(&req, &mut |_| StreamFlow::Continue).unwrap_err();
    match &why {
        BackendError::Refused { status, body, .. } => {
            assert_eq!(*status, 429);
            assert!(
                body.contains("exceeded_current_quota_error"),
                "the one machine-readable word in the refusal was dropped: {body}"
            );
            assert!(body.contains("usage limit"), "{body}");
        }
        other => panic!("a quota is a refusal, not {other}"),
    }
    assert_eq!(
        f.seen.lock().unwrap().len(),
        1,
        "the client called again after a refusal no wait lifts"
    );
}

/// A 403 is the plan's gate or the endpoint's client gate, and the second one is this build's to
/// answer: the refusal says so, and it names the User-Agent the request actually carried.
#[test]
fn a_403_for_an_unrecognised_client_says_so_plainly() {
    let f = fake(
        403,
        "{\"error\":{\"code\":\"access_terminated_error\",\"message\":\"access terminated\"}}",
    );
    let p = provider(&f.url);
    let items = vec![TranscriptItem::User {
        speaker: Default::default(),
        parts: vec![UserPart::Text { text: "hi".into() }],
    }];
    let req = TurnRequest {
        system: "",
        tools_json: &[],
        items: &items,
        tail: &[],
        max_output_tokens: None,
        thinking: None,
    };
    let why = p.complete(&req, &mut |_| StreamFlow::Continue).unwrap_err();
    let said = why.to_string();
    assert!(said.contains("403"), "{said}");
    assert!(
        said.contains("User-Agent") && said.to_ascii_lowercase().contains("client gate"),
        "a refused client must be told it is the client that was refused: {said}"
    );
    assert!(said.contains("letibot/"), "the answer names no UA: {said}");
    // And the UA it names is the one the server saw, not one written into the sentence.
    let seen = f.seen.lock().unwrap();
    let ua = seen[0].header("user-agent").expect("a UA");
    assert!(said.contains(ua), "{said} does not name {ua}");
}
