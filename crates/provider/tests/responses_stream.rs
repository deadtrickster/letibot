//! The Responses client against a stub provider on localhost: the request it sends, the
//! stream it reads, and the three things a stream can end in. No network and no key: the
//! bytes below are handwritten from the API reference's own event examples.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use letibot_backend::{BackendError, Delta, Finish, MessagesBackend, StreamFlow, TurnRequest};
use letibot_provider::ResponsesProvider;
use letibot_provider::keys::Credentials;
use letibot_provider::presets::OPENAI;
use letibot_transcript::{ReasoningField, TranscriptItem, UserPart};

/// What one request reached the stub as: the request line, the key's header, and the body.
type Seen = Vec<(String, String, String)>;

struct Fake {
    url: String,
    seen: Arc<Mutex<Seen>>,
}

/// A stub that answers the nth request with the nth `(status, body)`, and the last one for
/// every request after that.
fn fake(answers: Vec<(u16, String)>) -> Fake {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1/responses", listener.local_addr().unwrap());
    let seen: Arc<Mutex<Seen>> = Default::default();
    let log = seen.clone();
    std::thread::spawn(move || {
        let mut nth = 0usize;
        for conn in listener.incoming() {
            let Ok(mut conn) = conn else { continue };
            let mut reader = BufReader::new(conn.try_clone().unwrap());
            let mut request_line = String::new();
            reader.read_line(&mut request_line).unwrap();
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
                // ureq sends header names in lower case; a real provider reads them
                // case-insensitively, and so does this.
                let lower = h.to_ascii_lowercase();
                if let Some(v) = lower.strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap();
                }
                if lower.starts_with("authorization:") {
                    auth = h[14..].trim().to_string();
                }
            }
            let mut body = vec![0u8; len];
            let _ = reader.read_exact(&mut body);
            let body = String::from_utf8_lossy(&body).into_owned();
            let (status, script) = answers
                .get(nth)
                .cloned()
                .or_else(|| answers.last().cloned())
                .expect("the stub was given an answer");
            nth += 1;
            log.lock()
                .unwrap()
                .push((request_line.trim_end().to_string(), auth, body));
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

fn provider(url: &str) -> ResponsesProvider {
    let creds = Credentials {
        key: "sk-test".into(),
        from: "test".into(),
        prices: Default::default(),
        url: Some(url.into()),
    };
    // A box with no catalogue: an answer about this code, not about the machine's files.
    ResponsesProvider::new(&OPENAI, Some("gpt-6-astra"), creds)
        .with_catalogue(Arc::new(letibot_provider::catalogue::Catalogue::default()))
}

/// The recorded stream of a round that thought, called a tool, and reported its usage. The
/// event shapes are the reference's: `response.output_item.done` carries the complete
/// `encrypted_content`, which is the one thing this client must not lose.
const ROUND: &str = "event: response.created\n\
data: {\"type\":\"response.created\",\"sequence_number\":0,\"response\":{\"id\":\"resp_1\",\"object\":\"response\",\"status\":\"in_progress\",\"model\":\"gpt-6-astra\",\"output\":[],\"usage\":null}}\n\n\
event: response.output_item.added\n\
data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"sequence_number\":1,\"item\":{\"id\":\"msg_1\",\"type\":\"message\",\"status\":\"in_progress\",\"role\":\"assistant\",\"content\":[]}}\n\n\
event: response.output_text.delta\n\
data: {\"type\":\"response.output_text.delta\",\"item_id\":\"msg_1\",\"output_index\":0,\"content_index\":0,\"sequence_number\":2,\"logprobs\":[],\"delta\":\"Let me \"}\n\n\
event: response.output_text.delta\n\
data: {\"type\":\"response.output_text.delta\",\"item_id\":\"msg_1\",\"output_index\":0,\"content_index\":0,\"sequence_number\":3,\"logprobs\":[],\"delta\":\"read it.\"}\n\n\
event: response.output_item.done\n\
data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"sequence_number\":4,\"item\":{\"id\":\"msg_1\",\"type\":\"message\",\"status\":\"completed\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"Let me read it.\",\"annotations\":[],\"logprobs\":[]}]}}\n\n\
event: response.output_item.added\n\
data: {\"type\":\"response.output_item.added\",\"output_index\":1,\"sequence_number\":5,\"item\":{\"id\":\"rs_1\",\"type\":\"reasoning\",\"summary\":[]}}\n\n\
event: response.reasoning_summary_text.delta\n\
data: {\"type\":\"response.reasoning_summary_text.delta\",\"item_id\":\"rs_1\",\"output_index\":1,\"summary_index\":0,\"sequence_number\":6,\"delta\":\"I should call read.\"}\n\n\
event: response.output_item.done\n\
data: {\"type\":\"response.output_item.done\",\"output_index\":1,\"sequence_number\":7,\"item\":{\"id\":\"rs_1\",\"type\":\"reasoning\",\"summary\":[{\"type\":\"summary_text\",\"text\":\"I should call read.\"}],\"encrypted_content\":\"gAAAAABm-cipher-1\"}}\n\n\
event: response.output_item.added\n\
data: {\"type\":\"response.output_item.added\",\"output_index\":2,\"sequence_number\":8,\"item\":{\"id\":\"fc_1\",\"type\":\"function_call\",\"status\":\"in_progress\",\"call_id\":\"call_9\",\"name\":\"read\",\"arguments\":\"\"}}\n\n\
event: response.function_call_arguments.delta\n\
data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc_1\",\"output_index\":2,\"sequence_number\":9,\"delta\":\"{\\\"path\\\":\"}\n\n\
event: response.function_call_arguments.delta\n\
data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc_1\",\"output_index\":2,\"sequence_number\":10,\"delta\":\"\\\"a.txt\\\"}\"}\n\n\
event: response.function_call_arguments.done\n\
data: {\"type\":\"response.function_call_arguments.done\",\"item_id\":\"fc_1\",\"output_index\":2,\"sequence_number\":11,\"arguments\":\"{\\\"path\\\":\\\"a.txt\\\"}\"}\n\n\
event: response.output_item.done\n\
data: {\"type\":\"response.output_item.done\",\"output_index\":2,\"sequence_number\":12,\"item\":{\"id\":\"fc_1\",\"type\":\"function_call\",\"status\":\"completed\",\"call_id\":\"call_9\",\"name\":\"read\",\"arguments\":\"{\\\"path\\\":\\\"a.txt\\\"}\"}}\n\n\
event: response.completed\n\
data: {\"type\":\"response.completed\",\"sequence_number\":13,\"response\":{\"id\":\"resp_1\",\"object\":\"response\",\"status\":\"completed\",\"model\":\"gpt-6-astra\",\"output\":[],\"usage\":{\"input_tokens\":50,\"input_tokens_details\":{\"cached_tokens\":20,\"cache_write_tokens\":0},\"output_tokens\":9,\"output_tokens_details\":{\"reasoning_tokens\":4},\"total_tokens\":59}}}\n\n";

/// A plain answer, for the round after the tool result.
const ANSWER: &str = "event: response.output_text.delta\n\
data: {\"type\":\"response.output_text.delta\",\"item_id\":\"msg_2\",\"output_index\":0,\"content_index\":0,\"sequence_number\":0,\"delta\":\"It says hi.\"}\n\n\
event: response.completed\n\
data: {\"type\":\"response.completed\",\"sequence_number\":1,\"response\":{\"status\":\"completed\",\"model\":\"gpt-6-astra\",\"output\":[],\"usage\":{\"input_tokens\":80,\"input_tokens_details\":{\"cached_tokens\":60},\"output_tokens\":3,\"output_tokens_details\":{\"reasoning_tokens\":0},\"total_tokens\":83}}}\n\n";

fn tools() -> Vec<String> {
    vec![
        r#"{"type":"function","function":{"name":"read","description":"a file","parameters":{"type":"object","properties":{"path":{"type":"string"}}}}}"#
            .to_string(),
    ]
}

/// One round end to end: the request goes to `/v1/responses` with the key, the stream's
/// deltas reach the sink, and the answer's text, thought, call and usage are all read back.
#[test]
fn a_turn_streams_text_a_summary_and_a_call_and_the_request_carries_the_key() {
    let f = fake(vec![(200, ROUND.to_string())]);
    let p = provider(&f.url);
    let items = vec![TranscriptItem::User {
        speaker: Default::default(),
        parts: vec![UserPart::Text {
            text: "read a.txt".into(),
        }],
    }];
    let tools = tools();
    let req = TurnRequest {
        system: "be terse",
        tools_json: &tools,
        items: &items,
        tail: &[],
        max_output_tokens: Some(256),
        thinking: Some(true),
    };
    let mut deltas = Vec::new();
    let done = p
        .complete(&req, &mut |d| {
            deltas.push(d.clone());
            StreamFlow::Continue
        })
        .expect("the round answers");

    assert_eq!(done.text, "Let me read it.");
    assert_eq!(done.reasoning, "I should call read.");
    assert_eq!(done.tool_calls.len(), 1);
    assert_eq!(done.tool_calls[0].id, "call_9", "the call's own id is kept");
    assert_eq!(done.tool_calls[0].name, "read");
    assert_eq!(done.tool_calls[0].arguments, "{\"path\":\"a.txt\"}");
    assert_eq!(done.finish, Finish::ToolCalls);
    assert_eq!(done.cost.prompt_tokens, 50);
    assert_eq!(done.cost.cached_tokens, 20);
    assert_eq!(done.cost.generated_tokens, 9);
    assert_eq!(done.cost.micros_usd, None, "unpriced is None, not zero");
    assert!(matches!(deltas[0], Delta::Text(_)));
    assert!(
        deltas
            .iter()
            .any(|d| matches!(d, Delta::Reasoning(t) if t == "I should call read.")),
        "the summary reaches the head as reasoning: {deltas:?}"
    );

    let seen = f.seen.lock().unwrap();
    let (line, auth, body) = &seen[0];
    assert_eq!(line, "POST /v1/responses HTTP/1.1");
    assert_eq!(auth, "Bearer sk-test");
    let v: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(v["model"], "gpt-6-astra");
    assert_eq!(v["stream"], true);
    assert_eq!(v["store"], false);
    assert_eq!(
        v["include"],
        serde_json::json!(["reasoning.encrypted_content"])
    );
    assert_eq!(v["instructions"], "be terse");
    assert_eq!(v["max_output_tokens"], 256);
    assert_eq!(v["reasoning"]["summary"], "auto");
    assert_eq!(v["input"][0]["role"], "user");
    assert_eq!(v["input"][0]["content"], "read a.txt");
    assert_eq!(v["tools"][0]["name"], "read");
}

/// The request both rounds send: the same system prompt and the same tools, which is the
/// point - only the transcript grew.
fn request<'a>(items: &'a [TranscriptItem], tools: &'a [String]) -> TurnRequest<'a> {
    TurnRequest {
        system: "be terse",
        tools_json: tools,
        items,
        tail: &[],
        max_output_tokens: None,
        thinking: Some(true),
    }
}

/// The whole reason this protocol is worth having, against a real socket: the round issues a
/// reasoning item, the engine records its summary as a row, and the NEXT request carries the
/// item back - ciphertext and all - before the call it explains.
#[test]
fn the_next_request_carries_the_reasoning_item_the_first_round_issued() {
    let f = fake(vec![(200, ROUND.to_string()), (200, ANSWER.to_string())]);
    let p = provider(&f.url);
    let tools = tools();
    let first = vec![TranscriptItem::User {
        speaker: Default::default(),
        parts: vec![UserPart::Text {
            text: "read a.txt".into(),
        }],
    }];
    let done = p
        .complete(&request(&first, &tools), &mut |_| StreamFlow::Continue)
        .expect("the first round answers");

    // What the engine writes: the thought as one row, then the answer with its call, then the
    // result. Nothing else of the reasoning item survives - which is why the client kept it.
    let items = vec![
        first[0].clone(),
        TranscriptItem::Reasoning {
            text: done.reasoning.clone(),
            field: ReasoningField::ReasoningContent,
            truncated: false,
        },
        TranscriptItem::Assistant {
            text: done.text.clone(),
            tool_calls: done.tool_calls.clone(),
            truncated: false,
        },
        TranscriptItem::ToolResult {
            call_id: done.tool_calls[0].id.clone(),
            name: "read".into(),
            outcome: letibot_transcript::ToolOutcome::Ok,
            payload: "hi there".into(),
            edits: Vec::new(),
            origin: None,
            media: None,
        },
    ];
    let second = p
        .complete(&request(&items, &tools), &mut |_| StreamFlow::Continue)
        .expect("the second round answers");
    assert_eq!(second.text, "It says hi.");

    let seen = f.seen.lock().unwrap();
    assert_eq!(seen.len(), 2, "two rounds, two requests");
    let v: serde_json::Value = serde_json::from_str(&seen[1].2).unwrap();
    let input = v["input"].as_array().expect("an input list");
    let thought = input
        .iter()
        .position(|i| i["type"] == "reasoning")
        .expect("the issued reasoning item is carried back");
    assert_eq!(thought, 1, "after the operator's turn: {input:#?}");
    assert_eq!(input[thought]["id"], "rs_1");
    assert_eq!(input[thought]["encrypted_content"], "gAAAAABm-cipher-1");
    assert_eq!(
        input[thought]["summary"][0]["text"], "I should call read.",
        "the summary is what the row and the item agree on"
    );
    // And the round's own words, its call and the result that answers it, in that order: the
    // thought is where it was produced, before the message it produced.
    assert_eq!(input[thought + 1]["role"], "assistant");
    assert_eq!(input[thought + 1]["content"][0]["type"], "output_text");
    assert_eq!(input[thought + 1]["content"][0]["text"], "Let me read it.");
    assert_eq!(input[thought + 2]["type"], "function_call");
    assert_eq!(input[thought + 2]["call_id"], "call_9");
    assert_eq!(input[thought + 3]["type"], "function_call_output");
    assert_eq!(input[thought + 3]["call_id"], "call_9");
    assert_eq!(input[thought + 3]["output"], "hi there");
}

/// A refusal is the status the provider sent and the words it sent, with its CODE kept: the
/// code is the one machine-readable word in it, and a quota is read off that word.
#[test]
fn a_refusal_carries_the_providers_own_message_and_the_status() {
    let f = fake(vec![(
        401,
        r#"{"error":{"message":"Incorrect API key provided","type":"invalid_request_error","param":null,"code":"invalid_api_key"}}"#
            .to_string(),
    )]);
    let p = provider(&f.url);
    let err = p
        .complete(
            &TurnRequest {
                system: "",
                tools_json: &[],
                items: &[],
                tail: &[],
                max_output_tokens: None,
                thinking: None,
            },
            &mut |_| StreamFlow::Continue,
        )
        .expect_err("a 401 is not an answer");
    match err {
        BackendError::Refused { status, body, .. } => {
            assert_eq!(status, 401);
            assert!(body.contains("Incorrect API key provided"), "{body}");
            assert!(body.contains("invalid_api_key"), "{body}");
        }
        other => panic!("{other:?}"),
    }
}

/// A rate limit keeps the provider's own `Retry-After`: it is the only form of *when may I
/// come back* the daemon's ladder may sleep on.
#[test]
fn a_rate_limit_keeps_its_retry_after() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1/responses", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut conn) = conn else { continue };
            let mut reader = BufReader::new(conn.try_clone().unwrap());
            let mut len = 0usize;
            loop {
                let mut h = String::new();
                if reader.read_line(&mut h).unwrap_or(0) == 0 || h.trim_end().is_empty() {
                    break;
                }
                if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap();
                }
            }
            let mut body = vec![0u8; len];
            let _ = reader.read_exact(&mut body);
            let script = r#"{"error":{"message":"Rate limit reached for gpt-6-astra","type":"rate_limit_error","code":"rate_limit_exceeded"}}"#;
            let _ = write!(
                conn,
                "HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\nRetry-After: 20\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{script}",
                script.len()
            );
            let _ = conn.flush();
        }
    });
    let p = provider(&url);
    let err = p
        .complete(
            &TurnRequest {
                system: "",
                tools_json: &[],
                items: &[],
                tail: &[],
                max_output_tokens: None,
                thinking: None,
            },
            &mut |_| StreamFlow::Continue,
        )
        .expect_err("a 429 is not an answer");
    match err {
        BackendError::Refused {
            status,
            retry_after,
            body,
        } => {
            assert_eq!(status, 429);
            assert_eq!(retry_after, Some(20), "the provider said when");
            // The retry policy classifies this as a rate limit because the status says so,
            // and as a quota because of this word - the two facts it needs are both here.
            assert!(body.contains("rate_limit_exceeded"), "{body}");
        }
        other => panic!("{other:?}"),
    }
}

/// An error INSIDE the stream: the headers said 200 before the first event, so the event is
/// the only place the failure exists. A round that returned half an answer as a `Stop` with
/// no usage would record a truncated thought as if the model had finished it.
#[test]
fn an_error_event_fails_the_round_rather_than_ending_it() {
    let script = "data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"delta\":\"Hel\"}\n\n\
data: {\"type\":\"error\",\"code\":\"server_error\",\"param\":null,\"message\":\"The engines are overloaded\",\"sequence_number\":2}\n\n";
    let f = fake(vec![(200, script.to_string())]);
    let p = provider(&f.url);
    let err = p
        .complete(
            &TurnRequest {
                system: "",
                tools_json: &[],
                items: &[],
                tail: &[],
                max_output_tokens: None,
                thinking: None,
            },
            &mut |_| StreamFlow::Continue,
        )
        .expect_err("an in-band error is not an answer");
    match err {
        BackendError::Refused { status, body, .. } => {
            assert_eq!(status, 500, "a server_error is worth taking again");
            assert!(body.contains("The engines are overloaded"), "{body}");
        }
        other => panic!("{other:?}"),
    }
}

/// A stream that simply stops - no terminal event - is a fragment, not a short answer.
#[test]
fn a_stream_that_ends_without_a_terminal_event_is_a_fragment() {
    let script =
        "data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"delta\":\"Hel\"}\n\n";
    let f = fake(vec![(200, script.to_string())]);
    let p = provider(&f.url);
    let err = p
        .complete(
            &TurnRequest {
                system: "",
                tools_json: &[],
                items: &[],
                tail: &[],
                max_output_tokens: None,
                thinking: None,
            },
            &mut |_| StreamFlow::Continue,
        )
        .expect_err("half an answer is not an answer");
    match err {
        BackendError::Unreachable { message, .. } => {
            assert!(
                message.contains("without `response.completed`"),
                "{message}"
            );
        }
        other => panic!("{other:?}"),
    }
}

/// The caller closing the connection is `Aborted`, and nothing is recorded as an answer.
#[test]
fn a_sink_that_says_stop_aborts_the_round() {
    let f = fake(vec![(200, ROUND.to_string())]);
    let p = provider(&f.url);
    let err = p
        .complete(
            &TurnRequest {
                system: "",
                tools_json: &[],
                items: &[],
                tail: &[],
                max_output_tokens: None,
                thinking: None,
            },
            &mut |_| StreamFlow::Stop,
        )
        .expect_err("a stopped round records nothing");
    assert!(matches!(err, BackendError::Aborted), "{err:?}");
}
