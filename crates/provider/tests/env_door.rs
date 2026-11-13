//! `ANTHROPIC_BASE_URL` defines a door of its own, Claude Code style, against a Messages stub on
//! loopback. One test, because the variables are process-global.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use letibot_backend::{MessagesBackend, StreamFlow, TurnRequest};
use letibot_provider::AnthropicProvider;
use letibot_provider::presets::{ANTHROPIC, Origin};
use letibot_transcript::{TranscriptItem, UserPart};

/// A Messages server answering every request with one word; records each request's head.
fn stub() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/proxy/", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorded = seen.clone();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut conn) = conn else { continue };
            let mut reader = BufReader::new(conn.try_clone().unwrap());
            let (mut head, mut length) = (String::new(), 0usize);
            loop {
                let mut h = String::new();
                if reader.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
                    break;
                }
                if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = v.trim().parse().unwrap_or(0);
                }
                head.push_str(&h);
            }
            let mut body = vec![0u8; length];
            let _ = reader.read_exact(&mut body);
            recorded.lock().unwrap().push(head);
            let ev = |p: serde_json::Value| {
                format!("event: {}\ndata: {p}\n\n", p["type"].as_str().unwrap())
            };
            let sse = [
                ev(
                    serde_json::json!({"type": "message_start", "message": {"id": "m",
                    "type": "message", "role": "assistant", "model": "proxy-model",
                    "usage": {"input_tokens": 3, "output_tokens": 1}}}),
                ),
                ev(
                    serde_json::json!({"type": "content_block_start", "index": 0,
                    "content_block": {"type": "text", "text": ""}}),
                ),
                ev(
                    serde_json::json!({"type": "content_block_delta", "index": 0,
                    "delta": {"type": "text_delta", "text": "pong"}}),
                ),
                ev(serde_json::json!({"type": "content_block_stop", "index": 0})),
                ev(serde_json::json!({"type": "message_delta",
                    "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 2}})),
                ev(serde_json::json!({"type": "message_stop"})),
            ]
            .concat();
            let _ = write!(
                conn,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{sse}",
                sse.len()
            );
        }
    });
    (base, seen)
}

#[test]
fn anthropic_base_url_defines_env_anthropic_and_without_it_the_builtin_is_unchanged() {
    let (base, seen) = stub();
    let dir = std::env::temp_dir().join(format!("letibot-env-door-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // An empty file: the operator's providers.toml is never read here.
    let file = dir.join("providers.toml");
    std::fs::write(&file, "").unwrap();
    // SAFETY: this binary has one test, and nothing else reads the environment while it runs.
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", &base);
        std::env::set_var("ANTHROPIC_API_KEY", "sk-env-door");
        std::env::set_var("ANTHROPIC_MODEL", "proxy-model");
        std::env::remove_var("ANTHROPIC_AUTH_TOKEN");
    }
    let door = letibot_provider::registry::lookup("env-anthropic", Some(&file)).unwrap();
    assert_eq!(door.origin, Origin::Env("ANTHROPIC_BASE_URL"));
    assert_eq!(door.url, format!("{}v1/messages", base));
    let creds = letibot_provider::keys::resolve(door, None, Some(&file)).unwrap();
    assert_eq!(creds.from, "$ANTHROPIC_API_KEY");
    let p = AnthropicProvider::new(door, None, creds);
    let items = [TranscriptItem::User {
        speaker: Default::default(),
        parts: vec![UserPart::Text {
            text: "ping".into(),
        }],
    }];
    let req = TurnRequest {
        system: "",
        tools_json: &[],
        items: &items,
        tail: &[],
        max_output_tokens: None,
        thinking: None,
    };
    let done = p
        .complete(&req, &mut |_| StreamFlow::Continue)
        .expect("the stub answers");
    assert_eq!(done.text, "pong");
    let head = seen.lock().unwrap().last().cloned().unwrap();
    assert!(head.starts_with("POST /proxy/v1/messages "), "{head}");
    assert!(
        head.contains("sk-env-door"),
        "the key from $ANTHROPIC_API_KEY"
    );

    // No base URL: no env door, and the built-in reads the same variable for its own key.
    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }
    assert!(letibot_provider::registry::lookup("env-anthropic", Some(&file)).is_err());
    let builtin = letibot_provider::registry::lookup("anthropic", Some(&file)).unwrap();
    assert_eq!(builtin, &ANTHROPIC);
    let c = letibot_provider::keys::resolve(builtin, None, Some(&file)).unwrap();
    assert_eq!(c.from, "$ANTHROPIC_API_KEY");
    let _ = std::fs::remove_dir_all(&dir);
}
