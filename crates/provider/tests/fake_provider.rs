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
    fake_answering(move |_| (status, script.to_string()))
}

/// **A fake that decides per REQUEST** — the body it was sent is the argument, and the
/// answer is what the real provider would say to that body. `fake` is this with one answer
/// for everything; the learn-on-refusal test needs the other shape, because the whole
/// question is whether the second request is different from the first.
fn fake_answering(answer: impl Fn(&str) -> (u16, String) + Send + 'static) -> Fake {
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
            let body = String::from_utf8_lossy(&body).into_owned();
            let (status, script) = answer(&body);
            seen2.lock().unwrap().push((auth, body));
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
        speaker: Default::default(),
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
        tail: &[],
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

/// **The tail reaches the wire as the last message, and an absent tail adds
/// nothing.** `TurnRequest::tail` is where the standing-notes offer rides
/// (`letibot_harnessd::notes_offer`); this is the same claim as `messages`' own
/// unit test, made against a real socket and a real request body rather than
/// against `convert`'s return value.
#[test]
fn a_tail_is_the_last_message_on_the_wire() {
    let f = fake(SCRIPT, 200);
    let p = provider(&f.url);
    let items = vec![TranscriptItem::User {
        speaker: Default::default(),
        parts: vec![UserPart::Text {
            text: "read a.txt".into(),
        }],
    }];
    let tail = vec![TranscriptItem::User {
        speaker: letibot_transcript::Speaker::Agent,
        parts: vec![UserPart::Text {
            text: "[standing-notes offer] `.letibot/notes/x.md` looks relevant".into(),
        }],
    }];
    let req = TurnRequest {
        system: "be terse",
        tools_json: &[],
        items: &items,
        tail: &tail,
        max_output_tokens: None,
    };
    p.complete(&req, &mut |_| StreamFlow::Continue).unwrap();
    let seen = f.seen.lock().unwrap();
    let v: serde_json::Value = serde_json::from_str(&seen[0].1).unwrap();
    let messages = v["messages"].as_array().expect("messages");
    let last = messages.last().expect("a last message");
    assert_eq!(last["role"], "user");
    assert!(
        last["content"]
            .as_str()
            .is_some_and(|c| c.contains("standing-notes offer")),
        "the tail is what the request ends on: {last}"
    );
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
        tail: &[],
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

/// **A real picture, as the reference a row carries.**
///
/// These bytes matter: `messages::picture_fate` refuses to send a reference that is not a
/// complete image, so a fixture that is a signature and nothing else never reaches the provider
/// at all — which is exactly what the incident's 33-byte PNG header was. See
/// `letibot_transcript::testing`.
fn picture_ref(w: u32, h: u32) -> String {
    letibot_transcript::testing::data_ref(
        "image/png",
        &letibot_transcript::testing::png(w, h),
    )
}

fn picture_bytes(w: u32, h: u32) -> usize {
    letibot_transcript::testing::png(w, h).len()
}

/// **A model that refuses the picture is LEARNED, and the same request goes again
/// without it** — the operator's rule: *"when a provider answers http 400 and the body
/// says the content type is invalid … mark this model text-only for the rest of the
/// session, say so once, and retry the same request once with placeholders."*
///
/// MEASURED 2026-10-25, `~/logs/harnessd-2000f08acfaa.log`: `glm-5.3` answered
/// `http 400: messages.content.type is invalid, allowed values: ['text']` to the round,
/// to the compaction that would have made room, and to every turn after — because the
/// pictures were `read` charts in the HISTORY. The request that failed is the request
/// this retries, so the compaction succeeds too; it goes through the same `complete`.
///
/// What this pins, in order: the FIRST request carried an `image_url` and the second did
/// not, the retry happened exactly once, the model is recorded as text-only for the rest
/// of the process, the operator was told exactly once — and a LATER request already goes
/// out in the shape that works, so the daemon never earns that 400 again.
#[test]
fn a_refusal_of_the_picture_is_learned_and_the_request_retried_with_placeholders() {
    // The model name is this test's own: the mark is process-wide, and a name another
    // test uses would make the order the two run in matter.
    const MODEL: &str = "glm-5.3-image-probe";
    const REFUSAL: &str =
        "{\"error\":{\"message\":\"messages.content.type is invalid, allowed values: ['text']\"}}";
    let f = fake_answering(|body| match body.contains("image_url") {
        true => (400, REFUSAL.to_string()),
        false => (200, SCRIPT.to_string()),
    });
    let notices: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = notices.clone();
    let creds = Credentials {
        key: "sk-test".into(),
        from: "test".into(),
        prices: Default::default(),
        url: Some(f.url.clone()),
    };
    let p = OpenAiProvider::new(&letibot_provider::presets::GLM, Some(MODEL), creds).with_notes(
        Arc::new(move |said: String| sink.lock().unwrap().push(said)),
    );
    let items = vec![TranscriptItem::User {
        speaker: Default::default(),
        parts: vec![
            UserPart::Text {
                text: "what does this chart say?".into(),
            },
            UserPart::Image {
                media_type: "image/png".into(),
                data_ref: picture_ref(2, 2),
            },
        ],
    }];
    let req = TurnRequest {
        system: "be terse",
        tools_json: &[],
        items: &items,
        tail: &[],
        max_output_tokens: None,
    };
    let done = p
        .complete(&req, &mut |_| StreamFlow::Continue)
        .expect("the retry answered");
    assert_eq!(done.text, "Hello", "the answer is the retried round's");

    {
        let seen = f.seen.lock().unwrap();
        assert_eq!(seen.len(), 2, "exactly one retry, not a ladder");
        assert!(
            seen[0].1.contains("image_url"),
            "the first request did not carry the picture: {}",
            seen[0].1
        );
        assert!(
            !seen[1].1.contains("image_url"),
            "the retry sent the picture again: {}",
            seen[1].1
        );
        assert!(
            seen[1].1.contains(&format!(
                "[image 1 · image/png · {} B — not sent: {MODEL} takes text only]",
                picture_bytes(2, 2)
            )),
            "the retry said nothing about the picture it withheld: {}",
            seen[1].1
        );
    }
    // **The mark, and the notice said once.** The mark is what makes every later request
    // skip the refusal.
    assert!(
        letibot_provider::messages::known_text_only("glm", MODEL),
        "the refusal was not learned"
    );
    {
        let said = notices.lock().unwrap();
        assert_eq!(said.len(), 1, "the operator was told {said:?}");
        assert!(said[0].contains("refused an image"), "{}", said[0]);
        assert!(said[0].contains("placeholders"), "{}", said[0]);
    }

    // And the next request needs no lesson: one connection, placeholders already.
    p.complete(&req, &mut |_| StreamFlow::Continue)
        .expect("the marked model is answered first time");
    let seen = f.seen.lock().unwrap();
    assert_eq!(seen.len(), 3, "the third request was refused and retried");
    assert!(!seen[2].1.contains("image_url"), "{}", seen[2].1);
}

/// **A provider that refuses THE BYTES is learned against the picture, not against the model.**
///
/// MEASURED 2026-10-30, the incident this exists for: a session died and no restart could revive
/// it, because a `read` of a 33-byte PNG header was stored as a picture and every turn sent it
/// again —
///
/// ```text
/// http 400: .messages[1861].image[0]: You have uploaded an unsupported image. Please make sure
/// your image is valid and has one of the following formats: webp, png, jpeg, and gif.
/// ```
///
/// That sentence is NOT `text_only_refusal`'s: it does not say *this model takes text*, it says
/// *these bytes are not an image*, and the two facts have different remedies. So the assertions
/// here are the ones that tell the two apart: the model is NOT marked text-only, the picture IS
/// marked, the retry is ONE, and the next turn sends no `image_url` without earning the 400 again.
#[test]
fn a_provider_that_refuses_the_bytes_is_learned_against_the_picture_and_the_request_retried() {
    const MODEL: &str = "glm-5.3-bytes-probe";
    const REFUSAL: &str = ".messages[1861].image[0]: You have uploaded an unsupported image. \
                           Please make sure your image is valid and has one of the following \
                           formats: webp, png, jpeg, and gif.";
    let f = fake_answering(|body| match body.contains("image_url") {
        true => (
            400,
            format!("{{\"error\":{{\"message\":{}}}}}", json_string(REFUSAL)),
        ),
        false => (200, SCRIPT.to_string()),
    });
    let notices: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = notices.clone();
    let creds = Credentials {
        key: "sk-test".into(),
        from: "test".into(),
        prices: Default::default(),
        url: Some(f.url.clone()),
    };
    let p = OpenAiProvider::new(&letibot_provider::presets::GLM, Some(MODEL), creds).with_notes(
        Arc::new(move |said: String| sink.lock().unwrap().push(said)),
    );
    // **These bytes are this test's own.** The mark is process-wide and keyed by the picture, so
    // a size another test uses would make the order the two run in matter.
    let reference = picture_ref(3, 3);
    let items = vec![TranscriptItem::User {
        speaker: Default::default(),
        parts: vec![
            UserPart::Text {
                text: "what does this chart say?".into(),
            },
            UserPart::Image {
                media_type: "image/png".into(),
                data_ref: reference.clone(),
            },
        ],
    }];
    let req = TurnRequest {
        system: "be terse",
        tools_json: &[],
        items: &items,
        tail: &[],
        max_output_tokens: None,
    };
    let done = p
        .complete(&req, &mut |_| StreamFlow::Continue)
        .expect("the retry answered, so the session is not wedged");
    assert_eq!(done.text, "Hello", "the answer is the retried round's");

    {
        let seen = f.seen.lock().unwrap();
        assert_eq!(seen.len(), 2, "exactly one retry, not a ladder");
        assert!(
            seen[0].1.contains("image_url"),
            "the first request did not carry the picture: {}",
            seen[0].1
        );
        assert!(
            !seen[1].1.contains("image_url"),
            "the retry sent the picture again: {}",
            seen[1].1
        );
        // The placeholder names the reason the PROVIDER gave, not a guess of ours.
        assert!(
            seen[1]
                .1
                .contains("the provider refused these bytes: .messages[1861].image[0]"),
            "the retry said nothing about the picture it withheld: {}",
            seen[1].1
        );
        assert!(
            seen[1].1.contains("unsupported image"),
            "the provider's own words are not in the placeholder: {}",
            seen[1].1
        );
    }
    // **The mark is against the picture** — and the model keeps its eyes, because the model was
    // never the problem.
    assert!(
        letibot_provider::messages::known_bad_picture(&reference).is_some(),
        "the refusal was not learned against the picture"
    );
    assert!(
        !letibot_provider::messages::known_text_only("glm", MODEL),
        "a refusal of these bytes marked the MODEL text-only, which would withhold every \
         picture it was ever sent"
    );
    {
        let said = notices.lock().unwrap();
        assert_eq!(said.len(), 1, "the operator was told {said:?}");
        assert!(said[0].contains("refused"), "{}", said[0]);
        assert!(said[0].contains("unsupported image"), "{}", said[0]);
    }

    // **And the next turn does not earn it again**: one connection, the picture already withheld.
    p.complete(&req, &mut |_| StreamFlow::Continue)
        .expect("the marked picture is withheld first time");
    let seen = f.seen.lock().unwrap();
    assert_eq!(seen.len(), 3, "the third request was refused and retried");
    assert!(!seen[2].1.contains("image_url"), "{}", seen[2].1);
    assert!(
        seen[2].1.contains("the provider refused these bytes"),
        "the third request forgot why it has no picture: {}",
        seen[2].1
    );
}

/// **A 400 that carries a picture but is not about it is NOT retried and marks nothing.**
///
/// The guard that keeps this arm from swallowing an unrelated refusal: a context-length 400 on a
/// request with a picture in it must surface as itself, or a session at its window would lose
/// every picture it holds to a fault that was never theirs.
#[test]
fn a_refusal_that_is_not_about_the_picture_is_answered_as_itself() {
    const MODEL: &str = "glm-5.3-context-probe";
    const REFUSAL: &str = "maximum context length is 1048576 tokens";
    let f = fake_answering(move |_| {
        (
            400,
            format!("{{\"error\":{{\"message\":{}}}}}", json_string(REFUSAL)),
        )
    });
    let creds = Credentials {
        key: "sk-test".into(),
        from: "test".into(),
        prices: Default::default(),
        url: Some(f.url.clone()),
    };
    let p = OpenAiProvider::new(&letibot_provider::presets::GLM, Some(MODEL), creds);
    let reference = picture_ref(4, 4);
    let items = vec![TranscriptItem::User {
        speaker: Default::default(),
        parts: vec![UserPart::Image {
            media_type: "image/png".into(),
            data_ref: reference.clone(),
        }],
    }];
    let req = TurnRequest {
        system: "",
        tools_json: &[],
        items: &items,
        tail: &[],
        max_output_tokens: None,
    };
    let err = p
        .complete(&req, &mut |_| StreamFlow::Continue)
        .expect_err("a context refusal is not a picture refusal");
    assert!(format!("{err}").contains(REFUSAL), "{err}");
    assert_eq!(f.seen.lock().unwrap().len(), 1, "it was retried anyway");
    assert!(
        letibot_provider::messages::known_bad_picture(&reference).is_none(),
        "an unrelated refusal was recorded against the picture"
    );
}

/// **A retry that is ALSO refused records nothing and surfaces the provider's own words.**
///
/// The reason the marks are committed only on success: a second refusal may be about something
/// else entirely, and a mark that outlived it would withhold the operator's pictures for ever for
/// a fault that was never theirs. The words of the second refusal are what the operator sees.
#[test]
fn a_retry_that_is_refused_too_records_nothing_and_says_what_the_provider_said() {
    const MODEL: &str = "glm-5.3-twice-probe";
    let f = fake_answering(|body| match body.contains("image_url") {
        true => (
            400,
            "{\"error\":{\"message\":\"You have uploaded an unsupported image\"}}".to_string(),
        ),
        false => (
            400,
            "{\"error\":{\"message\":\"insufficient balance\"}}".to_string(),
        ),
    });
    let notices: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = notices.clone();
    let creds = Credentials {
        key: "sk-test".into(),
        from: "test".into(),
        prices: Default::default(),
        url: Some(f.url.clone()),
    };
    let p = OpenAiProvider::new(&letibot_provider::presets::GLM, Some(MODEL), creds).with_notes(
        Arc::new(move |said: String| sink.lock().unwrap().push(said)),
    );
    let reference = picture_ref(5, 5);
    let items = vec![TranscriptItem::User {
        speaker: Default::default(),
        parts: vec![UserPart::Image {
            media_type: "image/png".into(),
            data_ref: reference.clone(),
        }],
    }];
    let req = TurnRequest {
        system: "",
        tools_json: &[],
        items: &items,
        tail: &[],
        max_output_tokens: None,
    };
    let err = p
        .complete(&req, &mut |_| StreamFlow::Continue)
        .expect_err("both attempts were refused");
    let said = format!("{err}");
    assert!(
        said.contains("insufficient balance"),
        "the second refusal's own words are not what surfaced: {said}"
    );
    assert_eq!(f.seen.lock().unwrap().len(), 2, "one retry, not a ladder");
    assert!(
        letibot_provider::messages::known_bad_picture(&reference).is_none(),
        "a failed retry recorded the picture as bad anyway"
    );
    let said = notices.lock().unwrap();
    assert_eq!(said.len(), 1, "the operator was told {said:?}");
    assert!(
        said[0].contains("Nothing has been recorded"),
        "the notice does not say the pictures are still tried: {}",
        said[0]
    );
}

/// A JSON string literal for a body this test builds — the fake's answer is a JSON document and a
/// refusal's words can hold anything.
fn json_string(s: &str) -> String {
    serde_json::Value::String(s.to_string()).to_string()
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
        tail: &[],
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
