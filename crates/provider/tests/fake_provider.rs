//! The client against a fake provider: the request it sends, the stream it
//! reads, a refusal, and an abort mid-stream.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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

/// A fake that decides per REQUEST: the body it was sent is the argument and the answer is what
/// the real provider would say to it. The learn-on-refusal test needs this shape, because the
/// whole question is whether the second request differs from the first.
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
    // A box with no catalogue. `OpenAiProvider::new` prices a model by the catalogue it finds
    // on the machine, so a fake whose turn is asserted to be UNPRICED only says that on a box
    // without one.
    OpenAiProvider::new(
        &letibot_provider::presets::DEEPSEEK,
        Some("deepseek-chat"),
        creds,
    )
    .with_catalogue(Arc::new(letibot_provider::catalogue::Catalogue::default()))
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

/// The tail reaches the wire as the last message, and an absent tail adds nothing: the same
/// claim `messages`' own unit test makes, against a real socket and a real request body rather
/// than against `convert`'s return value.
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
        BackendError::Refused { status, body, .. } => {
            assert_eq!(status, 401);
            // No `code` in this body, so nothing is appended. `type` is not a code: it says
            // what KIND of refusal this is, and the retry policy asks whether the account can
            // pay, which no `type` answers.
            assert_eq!(body, "Authentication Fails");
        }
        other => panic!("{other:?}"),
    }
}

/// The provider's own code survives the projection to its message: taking only the message threw
/// away the one machine-readable word in a refusal, and a 429 naming an unpaid account became prose.
#[test]
fn a_refusal_keeps_the_code_that_says_what_it_is() {
    let f = fake(
        "{\"error\":{\"code\":\"insufficient_quota\",\"message\":\"You exceeded your \
         current quota, please check your plan and billing details\"}}",
        429,
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
        BackendError::Refused { status, body, .. } => {
            assert_eq!(status, 429);
            assert!(
                body.contains("insufficient_quota"),
                "the code the classifier reads was dropped: {body}"
            );
            assert!(
                body.starts_with("You exceeded your current quota"),
                "and the message a person reads comes first: {body}"
            );
        }
        other => panic!("{other:?}"),
    }
}

/// A real picture, as the reference a row carries. A fixture that is a signature and nothing else
/// never reaches the provider at all, because `messages::picture_fate` refuses to send a
/// reference that is not a complete image.
fn picture_ref(w: u32, h: u32) -> String {
    letibot_transcript::testing::data_ref("image/png", &letibot_transcript::testing::png(w, h))
}

fn picture_bytes(w: u32, h: u32) -> usize {
    letibot_transcript::testing::png(w, h).len()
}

/// One session's marks, in force for the length of the call - what the daemon does around every
/// request. Two of these in one test are two sessions, and neither sees the other's refusals.
fn a_session() -> (
    Arc<Mutex<letibot_provider::marks::Marks>>,
    letibot_provider::marks::Entered,
) {
    let marks = Arc::new(Mutex::new(letibot_provider::marks::Marks::new()));
    let held = letibot_provider::marks::enter(marks.clone());
    (marks, held)
}

/// A model that refuses the picture is LEARNED, and the same request goes again without it:
/// mark the model text-only for the session, say so once, retry once with placeholders. The
/// retry is the request that failed, so the compaction that would have made room succeeds too.
#[test]
fn a_refusal_of_the_picture_is_learned_and_the_request_retried_with_placeholders() {
    let (_marks, _held) = a_session();
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
    // The mark, and the notice said once. The mark is what makes every later request
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

/// A provider that refuses THE BYTES is learned against the picture, not against the model.
///
/// A session died and no restart could revive it: a `read` of a 33-byte PNG header was stored
/// as a picture and every turn sent it again. The provider said:
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
    let (_marks, _held) = a_session();
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
    // These bytes are this test's own, so the placeholder's own text can be asserted whole.
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
    // The mark is against the picture - and the model keeps its eyes, because the model was
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

    // And the next turn does not earn it again: one connection, the picture already withheld.
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

/// One session's refusal is not another session's lesson. The picture is in BOTH sessions'
/// histories, and a mark that crossed would re-render one conversation's prompt from the other's
/// 400 - which is what busted a session's cache, and what a restart then re-earned.
#[test]
fn a_refusal_learned_in_one_session_leaves_another_sessions_request_alone() {
    const MODEL: &str = "glm-5.3-two-sessions";
    const REFUSAL: &str = "{\"error\":{\"message\":\"You have uploaded an unsupported image\"}}";
    let f = fake_answering(|body| match body.contains("image_url") {
        true => (400, REFUSAL.to_string()),
        false => (200, SCRIPT.to_string()),
    });
    let creds = Credentials {
        key: "sk-test".into(),
        from: "test".into(),
        prices: Default::default(),
        url: Some(f.url.clone()),
    };
    let p = OpenAiProvider::new(&letibot_provider::presets::GLM, Some(MODEL), creds);
    let reference = picture_ref(6, 6);
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
    let a = Arc::new(Mutex::new(letibot_provider::marks::Marks::new()));
    let b = Arc::new(Mutex::new(letibot_provider::marks::Marks::new()));

    // Session A earns the refusal and learns it: its own next request withholds the picture.
    {
        let _held = letibot_provider::marks::enter(a.clone());
        p.complete(&req, &mut |_| StreamFlow::Continue)
            .expect("the retry answered");
        assert!(
            letibot_provider::messages::known_bad_picture(&reference).is_some(),
            "the refusal was not learned by the session that earned it"
        );
        let body = p.body(&req).unwrap().to_string();
        assert!(!body.contains("image_url"), "A still sends it: {body}");
    }

    // Session B has learned nothing, so its request is the one it would have sent by itself - the
    // same picture in the same history, and no trace of A's refusal in the prompt.
    {
        let _held = letibot_provider::marks::enter(b.clone());
        assert!(
            letibot_provider::messages::known_bad_picture(&reference).is_none(),
            "one session's refusal was read as another's"
        );
        let body = p.body(&req).unwrap().to_string();
        assert!(
            body.contains("image_url"),
            "B's picture was withheld: {body}"
        );
        assert!(
            !body.contains("not sent"),
            "B's prompt carries A's refusal: {body}"
        );
    }
}

/// A 400 that carries a picture but is not about it is NOT retried and marks nothing: the retry
/// exists for one refusal, and a mark made here would withhold a good picture for ever.
#[test]
fn a_refusal_that_is_not_about_the_picture_is_answered_as_itself() {
    let (_marks, _held) = a_session();
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

/// A retry that is ALSO refused records nothing and surfaces the provider's own words: the second
/// refusal may be about something else, and the next turn tries the pictures again.
#[test]
fn a_retry_that_is_refused_too_records_nothing_and_says_what_the_provider_said() {
    let (_marks, _held) = a_session();
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

/// A JSON string literal for a body this test builds - the fake's answer is a JSON document and a
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

/// A loopback server that answers every request with the raw bytes it was given, and another that
/// reads the request and then holds the connection without a byte: the two shapes a timeout and a
/// `Retry-After` need.
fn raw_server(answer: Option<String>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "http://{}/v1/chat/completions",
        listener.local_addr().unwrap()
    );
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut conn) = conn else { continue };
            let mut buf = [0u8; 8192];
            let _ = conn.read(&mut buf);
            match &answer {
                Some(bytes) => {
                    let _ = conn.write_all(bytes.as_bytes());
                    let _ = conn.flush();
                }
                // Held open and silent for longer than any deadline a test sets.
                None => std::thread::sleep(Duration::from_secs(60)),
            }
        }
    });
    url
}

fn bare_request(p: &OpenAiProvider) -> Result<(), BackendError> {
    let items: Vec<TranscriptItem> = Vec::new();
    let req = TurnRequest {
        system: "",
        tools_json: &[],
        items: &items,
        tail: &[],
        max_output_tokens: None,
    };
    p.complete(&req, &mut |_| StreamFlow::Continue).map(|_| ())
}

/// The same round, counting the deltas: a stream that fails half way still reaches the screen, and
/// a test about how a round ENDED has to say what arrived before it ended.
fn bare_with_deltas(p: &OpenAiProvider) -> (usize, Result<(), BackendError>) {
    let items: Vec<TranscriptItem> = Vec::new();
    let req = TurnRequest {
        system: "",
        tools_json: &[],
        items: &items,
        tail: &[],
        max_output_tokens: None,
    };
    let mut n = 0;
    let out = p
        .complete(&req, &mut |_| {
            n += 1;
            StreamFlow::Continue
        })
        .map(|_| ());
    (n, out)
}

/// The provider's own answer to *when may I come back* reaches the caller: a 429 carrying
/// `Retry-After` is a rate limit an automation can sleep out, and one without it is a wait nobody
/// can size.
#[test]
fn a_refusal_carries_the_retry_after_it_was_sent() {
    let body = "{\"error\":{\"message\":\"rate limited\"}}";
    let url = raw_server(Some(format!(
        "HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\nRetry-After: 90\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )));
    let p = provider(&url);
    match bare_request(&p) {
        Err(BackendError::Refused {
            status,
            retry_after,
            ..
        }) => {
            assert_eq!(status, 429);
            assert_eq!(
                retry_after,
                Some(90),
                "the header is the only form to act on"
            );
        }
        other => panic!("expected the provider's own refusal, got {other:?}"),
    }
}

/// A provider that accepts the request and never answers ends at the deadline it was given, not
/// at the client's default of ten minutes: a caller that must react to a model API timeout cannot
/// wait longer than its own patience.
#[test]
fn a_provider_that_never_answers_times_out_at_the_deadline_it_was_given() {
    let url = raw_server(None);
    let p = provider(&url).with_response_timeout(Duration::from_millis(300));
    let started = Instant::now();
    let e = bare_request(&p).expect_err("a silent provider answers nothing");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the round outlived the deadline it was given: {:?}",
        started.elapsed()
    );
    let said = e.to_string().to_lowercase();
    assert!(
        said.contains("timed out") || said.contains("timeout"),
        "and the failure says it was a timeout, because that is what an automation branches on: {e}"
    );
    assert!(matches!(e, BackendError::Unreachable { .. }), "{e:?}");
    assert!(
        e.timed_out(),
        "the deadline is the kind, not a reading of the words: {e}"
    );
}

/// The first byte has a deadline of its own and does not wait on the answer's budget: a provider
/// that says nothing at all fails on the first-byte clock, while an answer still streaming keeps
/// the long one.
#[test]
fn a_provider_that_says_nothing_fails_at_the_first_byte_deadline() {
    let url = raw_server(None);
    let p = provider(&url).with_first_byte(Duration::from_millis(300));
    let started = Instant::now();
    let e = bare_request(&p).expect_err("a silent provider answers nothing");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the round waited out the answer budget instead of the first byte: {:?}",
        started.elapsed()
    );
    assert!(e.timed_out(), "and the deadline is its kind: {e}");
}

/// Setting one deadline leaves the other as it was, in either order.
#[test]
fn the_two_deadlines_are_set_independently_in_either_order() {
    let (f, a) = (Duration::from_secs(120), Duration::from_secs(900));
    let one = provider("http://x")
        .with_first_byte(f)
        .with_response_timeout(a);
    let two = provider("http://x")
        .with_response_timeout(a)
        .with_first_byte(f);
    assert_eq!(one.deadlines(), (f, a));
    assert_eq!(two.deadlines(), (f, a));
}

/// Half a stream is not an answer: a socket that closes cleanly is indistinguishable from a finished
/// one, so `Stop` with zero usage bills the fragment as nothing and draws it as a completed thought.
#[test]
fn a_stream_that_closes_without_an_ending_is_a_fragment_and_not_an_answer() {
    const HALF: &str = "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n\
                        data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\n";
    let f = fake(HALF, 200);
    let p = provider(&f.url);
    let (deltas, out) = bare_with_deltas(&p);
    let err = out.expect_err("a stream with no ending is not a finished answer");
    assert!(matches!(err, BackendError::Unreachable { .. }), "{err:?}");
    let said = format!("{err}");
    assert!(
        said.contains("[DONE]") && said.contains("finish_reason"),
        "the failure must name what was missing, or a reader cannot tell this from a reset: {said}"
    );
    assert_eq!(
        deltas, 2,
        "the fragment did reach the screen before it ended"
    );
}

/// The control: a `finish_reason` with no `[DONE]` after it IS an ending, because some servers
/// close on the reason and a rule that demanded both would fail a whole answer.
#[test]
fn a_finish_reason_is_an_ending_even_with_no_done_after_it() {
    const REASON_ONLY: &str = "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\n\
                             data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n";
    let f = fake(REASON_ONLY, 200);
    let p = provider(&f.url);
    let (deltas, out) = bare_with_deltas(&p);
    out.expect("a finish_reason is the provider saying the stream is over");
    assert_eq!(deltas, 1);
}

/// An `{"error":…}` chunk arrives after the 200, so it is the only place a mid-stream refusal can
/// be said: dropped as a chunk with no choices, the round ends looking like a truncated answer.
#[test]
fn an_error_chunk_inside_the_stream_is_the_providers_refusal() {
    const ERRORED: &str = "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n\
                          data: {\"error\":{\"code\":\"insufficient_quota\",\"message\":\"You \
                          exceeded your current quota\"}}\n\n";
    let f = fake(ERRORED, 200);
    let p = provider(&f.url);
    let err = bare_request(&p).expect_err("the provider refused inside the stream");
    match err {
        BackendError::Refused { body, .. } => {
            assert!(
                body.contains("exceeded your current quota"),
                "the provider's own words were dropped: {body}"
            );
            assert!(
                body.contains("insufficient_quota"),
                "the code the retry policy reads was dropped: {body}"
            );
        }
        other => panic!("expected the provider's own refusal, got {other:?}"),
    }
}
