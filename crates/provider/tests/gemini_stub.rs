//! The GenAI client against a stub on loopback: the request it sends, the stream it reads, the
//! tool-call round trip with its thought signature, an error, a picture refusal, a rate limit and
//! a safety block. No network: every byte here is written in this file.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use letibot_backend::{BackendError, Completion, Delta, MessagesBackend, StreamFlow, TurnRequest};
use letibot_provider::gemini::GeminiProvider;
use letibot_provider::keys::Credentials;
use letibot_transcript::{ToolOutcome, TranscriptItem, UserPart};
use serde_json::{Value, json};

/// One request the stub was handed, as the facts a test asserts on.
struct Seen {
    line: String,
    key: Option<String>,
    body: Value,
}

struct Stub {
    /// What a key file's `url` would point at: the base the endpoint is built from.
    base: String,
    seen: Arc<Mutex<Vec<Seen>>>,
}

/// A stub answering the nth request with the nth `(status, body)`; each answer is one SSE stream
/// as `data:` lines, already newline-terminated.
fn stub(answers: Vec<(u16, String)>) -> Stub {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/v1beta", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut conn) = conn else { continue };
            let mut reader = BufReader::new(conn.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let (mut len, mut key) = (0usize, None);
            loop {
                let mut h = String::new();
                if reader.read_line(&mut h).unwrap_or(0) == 0 {
                    break;
                }
                let h = h.trim_end();
                if h.is_empty() {
                    break;
                }
                // ureq sends header names in lower case, and so does this.
                let lower = h.to_ascii_lowercase();
                if let Some(v) = lower.strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap();
                }
                if lower.starts_with("x-goog-api-key:") {
                    key = Some(h["x-goog-api-key:".len()..].trim().to_string());
                }
            }
            let mut body = vec![0u8; len];
            if reader.read_exact(&mut body).is_err() {
                continue;
            }
            let body = String::from_utf8_lossy(&body).into_owned();
            let at = seen2.lock().unwrap().len();
            seen2.lock().unwrap().push(Seen {
                line: line.trim_end().to_string(),
                key,
                body: serde_json::from_str(&body)
                    .unwrap_or_else(|e| panic!("the request body is not JSON ({e}): {body}")),
            });
            let (status, said) = answers
                .get(at)
                .cloned()
                .unwrap_or_else(|| (500, format!("no answer scripted for request {at}")));
            let _ = write!(
                conn,
                "HTTP/1.1 {status} X\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{said}",
                said.len()
            );
            let _ = conn.flush();
        }
    });
    Stub { base, seen }
}

impl Stub {
    fn requests(&self) -> Vec<Seen> {
        let mut out = Vec::new();
        for r in self.seen.lock().unwrap().iter() {
            out.push(Seen {
                line: r.line.clone(),
                key: r.key.clone(),
                body: r.body.clone(),
            });
        }
        out
    }
}

fn provider(base: &str) -> GeminiProvider {
    let creds = Credentials {
        key: "stub-key".into(),
        from: "test".into(),
        prices: Default::default(),
        url: Some(base.into()),
    };
    // A box with no catalogue: prices and windows must come from the request, not the machine.
    GeminiProvider::new(
        &letibot_provider::presets::GEMINI,
        Some("gemini-4-hydrogen"),
        creds,
    )
    .with_catalogue(Arc::new(letibot_provider::catalogue::Catalogue::default()))
}

/// One SSE stream: each chunk is ONE `data:` line, because a chunk split over two lines is two
/// events and neither is JSON.
fn sse(chunks: &[Value]) -> String {
    chunks.iter().map(|c| format!("data: {c}\n\n")).collect()
}

fn user(text: &str) -> TranscriptItem {
    TranscriptItem::User {
        speaker: Default::default(),
        parts: vec![UserPart::Text { text: text.into() }],
    }
}

fn tools() -> Vec<String> {
    vec![
        r#"{"type":"function","function":{"name":"read","description":"Read a file.",
            "parameters":{"type":"object","properties":{"path":{"type":"string"}},
            "required":["path"]}}}"#
            .to_string(),
    ]
}

/// One streamed answer: a thought, then text, then the ending and the usage — the shape
/// `streamGenerateContent` sends, where every chunk is a whole `GenerateContentResponse`.
///
/// ```text
/// data: {"candidates":[{"content":{"parts":[{"text":"…","thought":true}]}}]}
/// data: {"candidates":[{"content":{"parts":[{"text":"…"}]},"finishReason":"STOP"}],
///        "usageMetadata":{…},"modelVersion":"gemini-4-hydrogen"}
/// ```
#[test]
fn a_turn_streams_thoughts_text_usage_and_the_ending() {
    let answer = sse(&[
        json!({"candidates":[{"content":{"parts":[{"text":"weighing it","thought":true}]}}]}),
        json!({"candidates":[{"content":{"parts":[{"text":"Hel"}]}}]}),
        json!({"candidates":[{"content":{"parts":[{"text":"lo"}]}}]}),
        json!({
            "candidates": [{"content": {"parts": [{"text": ""}]}, "finishReason": "STOP"}],
            "usageMetadata": {
                "promptTokenCount": 40, "cachedContentTokenCount": 10,
                "candidatesTokenCount": 2, "thoughtsTokenCount": 8, "totalTokenCount": 50,
            },
            "modelVersion": "gemini-4-hydrogen",
        }),
    ]);
    let s = stub(vec![(200, answer)]);
    let p = provider(&s.base);
    let items = vec![user("say hello")];
    let req = TurnRequest {
        system: "be terse",
        tools_json: &[],
        items: &items,
        tail: &[],
        max_output_tokens: Some(99),
        thinking: Some(true),
    };
    let mut deltas: Vec<Delta> = Vec::new();
    let done: Completion = p
        .complete(&req, &mut |d| {
            deltas.push(d.clone());
            StreamFlow::Continue
        })
        .expect("an answer");

    assert_eq!(done.text, "Hello");
    assert_eq!(done.reasoning, "weighing it");
    assert_eq!(done.finish, letibot_backend::Finish::Stop);
    // Ten candidates-tokens and eight thoughts: thoughts are billed as output.
    assert_eq!(done.cost.prompt_tokens, 40);
    assert_eq!(done.cost.cached_tokens, 10);
    assert_eq!(done.cost.generated_tokens, 10);
    assert!(
        deltas
            .iter()
            .any(|d| matches!(d, Delta::Reasoning(t) if t == "weighing it")),
        "a thought arrives as Reasoning, apart from the answer: {deltas:?}"
    );
    assert_eq!(
        deltas
            .iter()
            .filter(|d| matches!(d, Delta::Text(_)))
            .count(),
        2
    );

    // And the request: the key in the header the API documents, the model in the PATH, `alt=sse`.
    let seen = s.requests();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].key.as_deref(), Some("stub-key"));
    assert!(
        seen[0]
            .line
            .contains("/v1beta/models/gemini-4-hydrogen:streamGenerateContent?alt=sse"),
        "{}",
        seen[0].line
    );
    assert_eq!(
        seen[0].body["systemInstruction"]["parts"][0]["text"],
        "be terse"
    );
    assert_eq!(seen[0].body["contents"][0]["parts"][0]["text"], "say hello");
    // `thinking: Some(true)` asks for the thoughts; `max_output_tokens` is the ceiling.
    assert_eq!(
        seen[0].body["generationConfig"]["thinkingConfig"]["includeThoughts"],
        true
    );
    assert_eq!(
        seen[0].body["generationConfig"]["maxOutputTokens"], 99,
        "{}",
        seen[0].body
    );
}

/// The round trip the docs describe: the model asks for a call and hands back a thought
/// signature on that part; the next request sends the call back WITH the signature "in the exact
/// part where it was received" and answers it with a `functionResponse` carrying the same `id`.
#[test]
fn a_tool_call_round_trip_carries_the_signature_and_the_id_back() {
    let first = sse(&[json!({
        "candidates": [{
            "content": {"parts": [{
                "functionCall": {"name": "read", "args": {"path": "a.txt"}, "id": "gfc-1"},
                "thoughtSignature": "sig-A",
            }]},
            "finishReason": "STOP",
        }],
        "usageMetadata": {"promptTokenCount": 20, "candidatesTokenCount": 4},
        "modelVersion": "gemini-4-hydrogen",
    })]);
    let second = sse(&[json!({
        "candidates": [{"content": {"parts": [{"text": "it says 42"}]}, "finishReason": "STOP"}],
        "usageMetadata": {"promptTokenCount": 30, "candidatesTokenCount": 4},
    })]);
    let s = stub(vec![(200, first), (200, second)]);
    let p = provider(&s.base);
    let items = vec![user("read a.txt")];
    let tools = tools();
    let req = TurnRequest {
        system: "",
        tools_json: &tools,
        items: &items,
        tail: &[],
        max_output_tokens: None,
        thinking: None,
    };
    let first_answer = p
        .complete(&req, &mut |_| StreamFlow::Continue)
        .expect("a call");
    assert_eq!(first_answer.tool_calls.len(), 1);
    let asked = first_answer.tool_calls[0].clone();
    assert_eq!(asked.id, "gfc-1", "a call Google named keeps that id");
    assert_eq!(asked.name, "read");
    assert_eq!(asked.arguments, r#"{"path":"a.txt"}"#);

    // The transcript now holds the call and its result, and the next round sends both.
    let held = vec![
        user("read a.txt"),
        TranscriptItem::Assistant {
            text: String::new(),
            tool_calls: vec![asked.clone()],
            truncated: false,
        },
        TranscriptItem::ToolResult {
            call_id: asked.id.clone(),
            name: asked.name.clone(),
            outcome: ToolOutcome::Ok,
            payload: "42".into(),
            edits: Vec::new(),
            origin: None,
            media: None,
        },
    ];
    let req = TurnRequest {
        system: "",
        tools_json: &tools,
        items: &held,
        tail: &[],
        max_output_tokens: None,
        thinking: None,
    };
    let done = p
        .complete(&req, &mut |_| StreamFlow::Continue)
        .expect("an answer");
    assert_eq!(done.text, "it says 42");

    let seen = s.requests();
    assert_eq!(seen.len(), 2);
    // The tool travels as a declaration, with its schema converted.
    let decl = &seen[0].body["tools"][0]["functionDeclarations"][0];
    assert_eq!(decl["name"], "read");
    assert_eq!(decl["parameters"]["properties"]["path"]["type"], "STRING");
    assert_eq!(decl["parameters"]["required"][0], "path");

    let contents = seen[1].body["contents"].as_array().expect("contents");
    let call_part = &contents[1]["parts"][0];
    assert_eq!(call_part["functionCall"]["name"], "read");
    assert_eq!(call_part["functionCall"]["id"], "gfc-1", "{}", seen[1].body);
    assert_eq!(
        call_part["thoughtSignature"], "sig-A",
        "Gemini 4 and 3 refuse a turn whose function call lost its signature: {}",
        seen[1].body
    );
    let answer_part = &contents[2]["parts"][0]["functionResponse"];
    assert_eq!(answer_part["name"], "read");
    assert_eq!(answer_part["id"], "gfc-1");
    assert_eq!(answer_part["response"]["result"], "42");
}

/// A 400 is the provider refusing, with its own words and its own status; the picture it names is
/// then WITHHELD and the request goes again — the second one carries the placeholder instead of
/// the bytes, because a picture the model will not take refuses every turn after it.
#[test]
fn a_refused_picture_is_named_on_the_retry_and_never_sent_again() {
    let refusal = json!({
        "error": {
            "code": 400,
            "message": "Unable to process input image. Please retry with a valid image.",
            "status": "INVALID_ARGUMENT",
        }
    })
    .to_string();
    let answer = "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"ok\"}]},\"finishReason\":\"STOP\"}]}\n\n";
    let s = stub(vec![(400, refusal), (200, answer.to_string())]);
    let p = provider(&s.base);
    let png = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
    let items = vec![TranscriptItem::User {
        speaker: Default::default(),
        parts: vec![
            UserPart::Text {
                text: "what is this".into(),
            },
            UserPart::Image {
                media_type: "image/png".into(),
                data_ref: png.into(),
            },
        ],
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
        .expect("the retry answered");
    assert_eq!(done.text, "ok");

    let seen = s.requests();
    assert_eq!(seen.len(), 2, "one refusal, one retry");
    assert!(
        seen[0].body["contents"][0]["parts"][1]["inlineData"].is_object(),
        "the first request sends the picture: {}",
        seen[0].body
    );
    let parts = seen[1].body["contents"][0]["parts"]
        .as_array()
        .expect("parts");
    assert!(
        parts.iter().all(|p| p.get("inlineData").is_none()),
        "the retry withholds it: {}",
        seen[1].body
    );
    let named = parts[1]["text"].as_str().unwrap_or_default();
    assert!(named.contains("not sent"), "{named}");
    assert!(
        named.contains("the provider refused these bytes"),
        "the placeholder carries the provider's own words: {named}"
    );
}

/// A bad key is a refusal with the provider's status and its own message, and nothing about it is
/// retried here: the ladder is the turn engine's, and it branches on the status.
#[test]
fn an_error_status_is_a_refusal_with_the_providers_words() {
    let refusal = json!({
        "error": {
            "code": 400,
            "message": "API key not valid. Please pass a valid API key.",
            "status": "INVALID_ARGUMENT",
        }
    })
    .to_string();
    let s = stub(vec![(400, refusal)]);
    let p = provider(&s.base);
    let items = vec![user("hi")];
    let req = TurnRequest {
        system: "",
        tools_json: &[],
        items: &items,
        tail: &[],
        max_output_tokens: None,
        thinking: None,
    };
    let err = p.complete(&req, &mut |_| StreamFlow::Continue).unwrap_err();
    match err {
        BackendError::Refused {
            status,
            body,
            retry_after,
        } => {
            assert_eq!(status, 400);
            assert!(body.contains("API key not valid."), "{body}");
            assert!(body.contains("INVALID_ARGUMENT"), "{body}");
            assert_eq!(retry_after, None);
        }
        other => panic!("a 400 is a refusal: {other:?}"),
    }
}

/// `429 RESOURCE_EXHAUSTED` is the rate limit, and its `RetryInfo.retryDelay` is the instant a
/// caller may sleep on when no header carried one. The status is what the turn engine's trouble
/// vocabulary reads, so a 429 that arrived as prose would never be waited out.
#[test]
fn a_rate_limit_keeps_the_status_the_word_and_the_instant_to_come_back() {
    let refusal = json!({
        "error": {
            "code": 429,
            "message": "You exceeded your current quota, please check your plan and billing details.",
            "status": "RESOURCE_EXHAUSTED",
            "details": [{
                "@type": "type.googleapis.com/google.rpc.RetryInfo",
                "retryDelay": "17s",
            }],
        }
    })
    .to_string();
    let s = stub(vec![(429, refusal)]);
    let p = provider(&s.base);
    let items = vec![user("hi")];
    let req = TurnRequest {
        system: "",
        tools_json: &[],
        items: &items,
        tail: &[],
        max_output_tokens: None,
        thinking: None,
    };
    let err = p.complete(&req, &mut |_| StreamFlow::Continue).unwrap_err();
    match err {
        BackendError::Refused {
            status,
            body,
            retry_after,
        } => {
            assert_eq!(status, 429, "the status is what `Trouble::RateLimit` reads");
            assert!(body.contains("RESOURCE_EXHAUSTED"), "{body}");
            assert_eq!(retry_after, Some(17));
        }
        other => panic!("{other:?}"),
    }
}

/// A candidate the filter stopped, with nothing in it, is the model refusing: 200 on the wire and
/// no answer. Recorded as an empty success it would be a turn that produced nothing with no
/// explanation on screen.
#[test]
fn a_safety_finish_with_no_content_is_the_providers_refusal() {
    let answer = sse(&[json!({
        "candidates": [{
            "finishReason": "SAFETY",
            "safetyRatings": [{
                "category": "HARM_CATEGORY_DANGEROUS_CONTENT",
                "probability": "HIGH",
                "blocked": true,
            }],
        }],
        "usageMetadata": {"promptTokenCount": 9},
    })]);
    let s = stub(vec![(200, answer)]);
    let p = provider(&s.base);
    let items = vec![user("hi")];
    let req = TurnRequest {
        system: "",
        tools_json: &[],
        items: &items,
        tail: &[],
        max_output_tokens: None,
        thinking: None,
    };
    let err = p.complete(&req, &mut |_| StreamFlow::Continue).unwrap_err();
    match err {
        BackendError::Refused { status, body, .. } => {
            assert_eq!(status, 200, "the wire answered; the model refused");
            assert!(body.contains("SAFETY"), "{body}");
            assert!(
                body.contains("HARM_CATEGORY_DANGEROUS_CONTENT=HIGH"),
                "{body}"
            );
        }
        other => panic!("{other:?}"),
    }
}

/// A socket that closes after half an answer looks exactly like a finished one, so the absence of
/// a `finishReason` is a failure rather than a short answer.
#[test]
fn a_stream_that_ends_without_a_finish_reason_is_a_fragment() {
    let answer =
        "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"half a thou\"}]}}]}\n\n";
    let s = stub(vec![(200, answer.to_string())]);
    let p = provider(&s.base);
    let items = vec![user("hi")];
    let req = TurnRequest {
        system: "",
        tools_json: &[],
        items: &items,
        tail: &[],
        max_output_tokens: None,
        thinking: None,
    };
    let err = p.complete(&req, &mut |_| StreamFlow::Continue).unwrap_err();
    match err {
        BackendError::Unreachable { message, .. } => {
            assert!(message.contains("without a finishReason"), "{message}");
            assert!(message.contains("fragment"), "{message}");
        }
        other => panic!("a closed socket is not an answer: {other:?}"),
    }
}

/// A schema Gemini cannot read is refused HERE, by name, rather than sent to be answered with
/// `Invalid JSON payload received. Unknown name …`.
#[test]
fn a_tool_schema_gemini_cannot_read_is_refused_before_the_post() {
    let s = stub(vec![]);
    let p = provider(&s.base);
    let items = vec![user("hi")];
    let bad = vec![
        r#"{"type":"function","function":{"name":"t","description":"d","parameters":
           {"type":"object","properties":{"x":{"$ref":"definitions/x"}}}}}"#
            .to_string(),
    ];
    let req = TurnRequest {
        system: "",
        tools_json: &bad,
        items: &items,
        tail: &[],
        max_output_tokens: None,
        thinking: None,
    };
    let err = p.complete(&req, &mut |_| StreamFlow::Continue).unwrap_err();
    match err {
        BackendError::Malformed(m) => {
            assert!(m.contains("t.parameters.properties.x"), "{m}");
            assert!(m.contains("$ref"), "{m}");
        }
        other => panic!("{other:?}"),
    }
    assert!(
        s.requests().is_empty(),
        "a request the API would reject is not sent"
    );
}

/// An in-band error arrives after a `200` header, so the response's status cannot carry it: the
/// chunk's own code is what the caller branches on.
#[test]
fn an_error_inside_the_stream_is_a_refusal_with_the_chunks_own_code() {
    let answer = sse(&[json!({
        "error": {"code": 503, "message": "The model is overloaded.", "status": "UNAVAILABLE"},
    })]);
    let s = stub(vec![(200, answer)]);
    let p = provider(&s.base);
    let items = vec![user("hi")];
    let req = TurnRequest {
        system: "",
        tools_json: &[],
        items: &items,
        tail: &[],
        max_output_tokens: None,
        thinking: None,
    };
    let err = p.complete(&req, &mut |_| StreamFlow::Continue).unwrap_err();
    match err {
        BackendError::Refused { status, body, .. } => {
            assert_eq!(status, 503);
            assert!(body.contains("overloaded"), "{body}");
        }
        other => panic!("{other:?}"),
    }
}

/// A caller that asks to stop gets `Aborted` and nothing recorded, and the stream is closed where
/// it stood rather than read to the end.
#[test]
fn a_stop_from_the_callback_aborts_the_turn() {
    let answer = concat!(
        "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"one\"}]}}]}\n\n",
        "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"two\"}]},\"finishReason\":\"STOP\"}]}\n\n",
    );
    let s = stub(vec![(200, answer.to_string())]);
    let p = provider(&s.base);
    let items = vec![user("hi")];
    let req = TurnRequest {
        system: "",
        tools_json: &[],
        items: &items,
        tail: &[],
        max_output_tokens: None,
        thinking: None,
    };
    let mut seen: Vec<String> = Vec::new();
    let err = p
        .complete(&req, &mut |d| {
            if let Delta::Text(t) = d {
                seen.push(t.clone());
            }
            StreamFlow::Stop
        })
        .unwrap_err();
    assert!(matches!(err, BackendError::Aborted), "{err:?}");
    assert_eq!(seen, ["one"], "the first delta closed the round");
}
