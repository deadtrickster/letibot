//! What this protocol does with a transcript, a stream and a refusal, from the docs it cites.

use super::*;
use letibot_transcript::{CallOrigin, ReasoningField, Speaker, SystemOrigin};

fn user(text: &str) -> TranscriptItem {
    TranscriptItem::User {
        speaker: Speaker::default(),
        parts: vec![UserPart::Text { text: text.into() }],
    }
}

fn call(id: &str, name: &str, arguments: &str) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: name.into(),
        arguments: arguments.into(),
    }
}

fn assistant(text: &str, calls: Vec<ToolCall>) -> TranscriptItem {
    TranscriptItem::Assistant {
        text: text.into(),
        tool_calls: calls,
        truncated: false,
    }
}

fn result(call_id: &str, name: &str, payload: &str) -> TranscriptItem {
    TranscriptItem::ToolResult {
        call_id: call_id.into(),
        name: name.into(),
        outcome: ToolOutcome::Ok,
        payload: payload.into(),
        edits: Vec::new(),
        origin: None,
        media: None,
    }
}

fn provider() -> GeminiProvider {
    let creds = Credentials {
        key: "k".into(),
        from: "test".into(),
        prices: Default::default(),
        url: None,
    };
    GeminiProvider::new(&crate::presets::GEMINI, Some("gemini-4-hydrogen"), creds)
        .with_catalogue(Arc::new(crate::catalogue::Catalogue::default()))
}

fn body(items: &[TranscriptItem], tools_json: &[String], system: &str) -> Value {
    let p = provider();
    let req = TurnRequest {
        system,
        tools_json,
        items,
        tail: &[],
        max_output_tokens: None,
        thinking: None,
    };
    p.body(&req).expect("a body")
}

/// A `data:` picture of one complete PNG, written out rather than fetched: the fate of an image
/// is asked before the bytes go, and a signature is all this needs.
const PNG: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

/// The two roles, one content per item, the system instruction in its own field, and the key
/// facts of each part: `contents` of `user`/`model` with `text`/`inlineData`/`functionCall`/
/// `functionResponse` is the whole of the shape
/// (<https://ai.google.dev/api/generate-content/stream-generate-content#request-body>).
#[test]
fn a_transcript_becomes_contents_with_two_roles_and_a_system_instruction() {
    let items = vec![
        user("read the chart"),
        TranscriptItem::User {
            speaker: Speaker::default(),
            parts: vec![UserPart::Image {
                media_type: "image/png".into(),
                data_ref: PNG.into(),
            }],
        },
        assistant("", vec![call("call_1", "read", r#"{"path":"a.png"}"#)]),
        result("call_1", "read", "[image 1]"),
    ];
    let b = body(&items, &[], "be terse");

    assert_eq!(
        b["systemInstruction"]["parts"][0]["text"], "be terse",
        "the system prompt is one field, not a role: {b}"
    );
    let contents = b["contents"].as_array().expect("contents");
    let roles: Vec<&str> = contents
        .iter()
        .map(|c| c["role"].as_str().unwrap_or("?"))
        .collect();
    assert_eq!(roles, ["user", "user", "model", "user"], "{b}");

    assert_eq!(contents[0]["parts"][0]["text"], "read the chart");
    // A picture is inline bytes with the `data:` prefix stripped, which is the form the API
    // documents for `inlineData.data`.
    let blob = &contents[1]["parts"][0]["inlineData"];
    assert_eq!(blob["mimeType"], "image/png", "{b}");
    assert_eq!(
        blob["data"].as_str().map(|d| d.starts_with("data:")),
        Some(false),
        "the prefix would be decoded as image bytes: {b}"
    );
    // The call's arguments travel as an OBJECT, not as the string the transcript carries.
    assert_eq!(
        contents[2]["parts"][0]["functionCall"]["args"]["path"], "a.png",
        "{b}"
    );
    // The result answers the call by name, and folds its outcome under `result`.
    assert_eq!(
        contents[3]["parts"][0]["functionResponse"]["name"], "read",
        "{b}"
    );
    assert_eq!(
        contents[3]["parts"][0]["functionResponse"]["response"]["result"], "[image 1]",
        "{b}"
    );
}

/// A transcript can end on unanswered calls — an interrupted turn does exactly that — and an
/// orphan result answers nothing. The first is FILLED (a sentence saying no result was recorded
/// is better information than the provider's 400), the second dropped, unless the OPERATOR ran
/// it, in which case it is the person's own turn.
#[test]
fn an_unanswered_call_is_filled_and_an_orphan_result_becomes_the_operators_turn() {
    let mut orphan = result("call_gone", "bash", "the operator's own line");
    if let TranscriptItem::ToolResult { origin, .. } = &mut orphan {
        *origin = Some(CallOrigin::Operator {
            who: "human:dead".into(),
        });
    }
    let items = vec![
        user("go"),
        assistant(
            "",
            vec![call("call_1", "read", "{}"), call("call_2", "grep", "{}")],
        ),
        result("call_1", "read", "ok"),
        orphan,
        result("call_nobody", "read", "dropped"),
    ];
    let b = body(&items, &[], "");
    let contents = b["contents"].as_array().expect("contents");

    // user, model, the answer to call_1, the filler for call_2, then the person's own turn.
    assert_eq!(contents.len(), 5, "{b}");
    assert_eq!(
        contents[3]["parts"][0]["functionResponse"]["name"], "grep",
        "the second call owed an answer and got one that says so: {b}"
    );
    assert_eq!(
        contents[3]["parts"][0]["functionResponse"]["response"]["result"], NO_RESULT,
        "{b}"
    );
    let person = contents[4]["parts"][0]["text"].as_str().unwrap_or_default();
    assert!(
        person.contains("operator's own line"),
        "an operator-run result is the person's turn: {b}"
    );
    assert!(
        !b.to_string().contains("dropped"),
        "a result answering no call is not sent at all: {b}"
    );
}

/// A row recorded while calls were still running — a room note, a monitor firing — must not land
/// between a call and its result. It waits, and the results go first.
#[test]
fn a_row_between_a_call_and_its_result_waits_for_the_result() {
    let items = vec![
        user("go"),
        assistant("", vec![call("call_1", "read", "{}")]),
        TranscriptItem::Notice {
            kind: letibot_transcript::NoticeKind::Monitor,
            text: "the monitor fired".into(),
            facts: None,
        },
        result("call_1", "read", "ok"),
    ];
    let b = body(&items, &[], "");
    let contents = b["contents"].as_array().expect("contents");
    let roles: Vec<&str> = contents
        .iter()
        .map(|c| c["role"].as_str().unwrap_or("?"))
        .collect();
    assert_eq!(roles, ["user", "model", "user", "user"], "{b}");
    assert!(
        contents[2]["parts"][0]["functionResponse"].is_object(),
        "the result answers the call before the note is said: {b}"
    );
    assert_eq!(contents[3]["parts"][0]["text"], "the monitor fired");
}

/// `generateContent` has no thought blocks, so a reasoning row is not replayed; what carries the
/// reasoning forward is the SIGNATURE on the part, and it must go back "in the exact part where
/// it was received" (<https://ai.google.dev/gemini-api/docs/thought-signatures?generate-content=true>).
#[test]
fn a_thought_signature_rides_the_function_call_it_came_on() {
    let p = provider();
    {
        let mut sigs = p.signatures.lock().unwrap();
        sigs.keep("call_1", Some("google_id_1"), Some("sig-A"));
        sigs.keep("call_2", None, None);
        sigs.remember_answer("the answer", "sig-B");
    }
    let items = vec![
        user("go"),
        TranscriptItem::Reasoning {
            text: "a thought the API serves as a summary".into(),
            field: ReasoningField::ReasoningContent,
            truncated: false,
        },
        assistant(
            "the answer",
            vec![call("call_1", "read", "{}"), call("call_2", "grep", "{}")],
        ),
        result("call_1", "read", "ok"),
        result("call_2", "grep", "ok"),
    ];
    let req = TurnRequest {
        system: "",
        tools_json: &[],
        items: &items,
        tail: &[],
        max_output_tokens: None,
        thinking: None,
    };
    let b = p.body(&req).expect("a body");
    let contents = b["contents"].as_array().expect("contents");
    let parts = contents[1]["parts"].as_array().expect("model parts");
    assert_eq!(parts.len(), 3, "text, then two calls: {b}");
    assert!(
        !b.to_string().contains("a thought the API serves"),
        "a reasoning row is not replayed: {b}"
    );
    assert_eq!(
        parts[0]["thoughtSignature"], "sig-B",
        "the answer's own signature goes back on its last part: {b}"
    );
    assert_eq!(parts[1]["thoughtSignature"], "sig-A", "{b}");
    assert_eq!(
        parts[1]["functionCall"]["id"], "google_id_1",
        "only an id Google minted goes back in `id`: {b}"
    );
    assert!(
        parts[2]["functionCall"].get("id").is_none(),
        "a minted id is not one the model knows: {b}"
    );
    // And the response carries the id, which is what matches it to the call.
    assert_eq!(
        contents[2]["parts"][0]["functionResponse"]["id"], "google_id_1",
        "{b}"
    );
}

/// A tool's `parameters` is a JSON Schema; Gemini's `Schema` is a SUBSET of OpenAPI 3.0. A union
/// of types has no spelling there and becomes `anyOf`; the int64 bounds are quoted strings; the
/// annotation keys are dropped; and the constructs with no equivalent are refused by name rather
/// than silently widening a tool's contract.
#[test]
fn the_schema_converter_converts_drops_and_refuses() {
    // The two shapes this tree's own tools write.
    let union = json!({
        "description": "one or many",
        "type": ["boolean", "integer"],
    });
    let converted = schema(&union, "t").expect("a union is anyOf");
    assert_eq!(
        converted["anyOf"],
        json!([{"type": "BOOLEAN"}, {"type": "INTEGER"}]),
        "{converted}"
    );
    assert_eq!(converted["description"], "one or many");

    let map = json!({
        "type": "object",
        "properties": {
            "roles": {
                "type": "object",
                "additionalProperties": {"type": "string"},
            }
        },
        "required": ["roles"],
    });
    let converted = schema(&map, "t").expect("a free-form object is still an object");
    assert_eq!(converted["type"], "OBJECT");
    assert!(
        converted["properties"]["roles"]
            .get("additionalProperties")
            .is_none(),
        "Gemini's Schema has no additionalProperties: {converted}"
    );

    let bounded = json!({"type": "array", "items": {"type": "string"}, "minItems": 2});
    let converted = schema(&bounded, "t").expect("a bounded array");
    assert_eq!(
        converted["minItems"], "2",
        "the reference types it as a string: {converted}"
    );

    // Refused, each in its own words: a silent translation of one of these changes which values
    // a tool accepts.
    for (key, value) in [
        ("$ref", json!("#/definitions/x")),
        ("oneOf", json!([{"type": "string"}])),
        ("allOf", json!([{"type": "string"}])),
        ("const", json!("only")),
        ("uniqueItems", json!(true)),
    ] {
        let mut bad = json!({"type": "string"});
        bad[key] = value;
        let err = schema(&bad, "t.parameters").unwrap_err();
        assert!(
            err.contains(key) && err.contains("t.parameters"),
            "a refusal names the parameter and the keyword: {err}"
        );
    }
    // And the two that are about the value rather than the keyword.
    let err = schema(&json!({"type": "timestamp"}), "t").unwrap_err();
    assert!(
        err.contains("not one of Gemini's types") && err.contains("OBJECT"),
        "a refusal lists what the vocabulary IS: {err}"
    );
    let err = schema(&json!({"type": "string", "enum": [1, 2]}), "t").unwrap_err();
    assert!(err.contains("list of strings"), "{err}");
    let err = schema(&json!({"description": "nothing to send"}), "t").unwrap_err();
    assert!(err.contains("no `type`"), "{err}");
}

/// `description` is required on a `FunctionDeclaration`, and a declaration without it fails the
/// whole request — a hundred good tools refused with one bare one.
#[test]
fn every_declaration_carries_a_name_and_a_description() {
    let tools_json = vec![
        r#"{"type":"function","function":{"name":"read","parameters":{"type":"object"}}}"#
            .to_string(),
        r#"{"name":"bare","description":"  ","parameters":null}"#.to_string(),
    ];
    let decls = tools(&tools_json).expect("two declarations");
    assert_eq!(decls.len(), 2);
    assert_eq!(decls[0]["name"], "read");
    assert_eq!(
        decls[0]["description"], "read",
        "a missing description falls back to the name rather than failing the request"
    );
    assert_eq!(decls[1]["description"], "bare");

    let err = tools(&[r#"{"neither":true}"#.to_string()]).unwrap_err();
    assert!(err.contains("neither `function` nor `name`"), "{err}");
}

/// `--effort` is a LEVEL on this protocol (`thinkingLevel`), and the absence of one sends nothing
/// at all: `thinkingConfig` on a model that does not think is an error, not a no-op.
#[test]
fn effort_becomes_a_thinking_level_and_absence_sends_nothing() {
    let items = [user("go")];
    let with = |effort: Option<&str>, thinking: bool| {
        let mut p = provider();
        p.effort = effort.map(str::to_string);
        p.thinking = thinking;
        let req = TurnRequest {
            system: "",
            tools_json: &[],
            items: &items,
            tail: &[],
            max_output_tokens: None,
            thinking: None,
        };
        p.body(&req).expect("a body")
    };
    let b = with(None, false);
    assert!(
        b.get("generationConfig").is_none(),
        "nothing asked for, nothing sent: {b}"
    );
    let b = with(Some("xhigh"), false);
    assert_eq!(
        b["generationConfig"]["thinkingConfig"]["thinkingLevel"],
        "XHIGH"
    );
    assert!(
        b["generationConfig"]["thinkingConfig"]
            .get("includeThoughts")
            .is_none(),
        "a level is not a request to be shown the thoughts: {b}"
    );
    let b = with(Some("low"), true);
    assert_eq!(
        b["generationConfig"]["thinkingConfig"]["thinkingLevel"],
        "LOW"
    );
    assert_eq!(
        b["generationConfig"]["thinkingConfig"]["includeThoughts"],
        true
    );
    // The launcher's word for the top rung, and a word nobody meant is not guessed at.
    assert_eq!(thinking_level("max"), Some("XHIGH"));
    assert_eq!(thinking_level("medium"), Some("MEDIUM"));
    assert_eq!(thinking_level("ludicrous"), None);
}

/// `thoughtsTokenCount` is billed as output and `candidatesTokenCount` excludes it, so a thinking
/// turn costed on the candidates alone reports nearly nothing; `cachedContentTokenCount` is the
/// cache read.
#[test]
fn usage_counts_thoughts_as_output_and_the_cache_as_cached() {
    let mut a = Accumulator::default();
    a.absorb(&json!({
        "candidates": [{"content": {"parts": [{"text": "hi"}]}, "finishReason": "STOP"}],
        "usageMetadata": {
            "promptTokenCount": 100,
            "cachedContentTokenCount": 60,
            "candidatesTokenCount": 10,
            "thoughtsTokenCount": 5,
            "totalTokenCount": 115,
        },
        "modelVersion": "gemini-4-hydrogen",
    }));
    let mut creds = Credentials {
        key: "k".into(),
        from: "test".into(),
        prices: Default::default(),
        url: None,
    };
    creds.prices.insert(
        "gemini-4-hydrogen".into(),
        crate::presets::Prices {
            input: 1.0,
            cached: 0.1,
            output: 2.0,
        },
    );
    let done = a
        .finish(
            200,
            "gemini-4-hydrogen",
            &creds,
            5,
            &crate::presets::GEMINI,
            &crate::catalogue::Catalogue::default(),
        )
        .expect("an answer");
    assert_eq!(done.text, "hi");
    assert_eq!(done.cost.prompt_tokens, 100);
    assert_eq!(done.cost.cached_tokens, 60);
    assert_eq!(
        done.cost.generated_tokens, 15,
        "ten candidates and five thoughts"
    );
    // 40*1 + 60*0.1 + 15*2 = 76 micro-USD.
    assert_eq!(done.cost.micros_usd, Some(76));
    assert_eq!(done.finish, Finish::Stop);
    assert!(done.raw_usage.as_deref().is_some_and(|u| u.contains("115")));
}

/// A candidate the model's filter stopped is an answer that says NO, and an empty one recorded as
/// a success is a turn the operator watches produce nothing with no explanation. The response's
/// own status travels with it, so the retry policy does not take the round again.
#[test]
fn a_safety_block_is_a_refusal_rather_than_an_empty_success() {
    let mut a = Accumulator::default();
    a.absorb(&json!({
        "candidates": [{
            "finishReason": "SAFETY",
            "safetyRatings": [
                {"category": "HARM_CATEGORY_HATE_SPEECH", "probability": "HIGH", "blocked": true},
                {"category": "HARM_CATEGORY_HARASSMENT", "probability": "NEGLIGIBLE"},
            ],
        }],
        "usageMetadata": {"promptTokenCount": 12},
    }));
    let creds = Credentials {
        key: "k".into(),
        from: "test".into(),
        prices: Default::default(),
        url: None,
    };
    let err = a
        .finish(
            200,
            "m",
            &creds,
            1,
            &crate::presets::GEMINI,
            &crate::catalogue::Catalogue::default(),
        )
        .unwrap_err();
    match err {
        BackendError::Refused { status, body, .. } => {
            assert_eq!(status, 200, "the wire answered; the model refused");
            assert!(body.contains("SAFETY"), "{body}");
            assert!(
                body.contains("HARM_CATEGORY_HATE_SPEECH=HIGH"),
                "the category that fired, and not the one that did not: {body}"
            );
            assert!(!body.contains("HARASSMENT"), "{body}");
        }
        other => panic!("a block must be a refusal: {other:?}"),
    }

    // A prompt refused outright: no candidates at all, and `promptFeedback.blockReason`.
    let mut a = Accumulator::default();
    a.absorb(&json!({"promptFeedback": {"blockReason": "PROHIBITED_CONTENT"}}));
    let err = a
        .finish(
            200,
            "m",
            &creds,
            1,
            &crate::presets::GEMINI,
            &crate::catalogue::Catalogue::default(),
        )
        .unwrap_err();
    assert!(
        matches!(err, BackendError::Refused { ref body, .. } if body.contains("PROHIBITED_CONTENT")),
        "{err:?}"
    );
}

/// The endings, in the API's own words: `STOP`, `MAX_TOKENS`, and everything else kept verbatim
/// so the operator reads which one it was.
#[test]
fn finish_reasons_map_to_the_outcomes_the_tree_models() {
    assert_eq!(finish_of("STOP"), Finish::Stop);
    assert_eq!(finish_of("MAX_TOKENS"), Finish::Length);
    assert_eq!(
        finish_of("MALFORMED_FUNCTION_CALL"),
        Finish::Other("MALFORMED_FUNCTION_CALL".into())
    );
    assert_eq!(finish_of("RECITATION"), Finish::Other("RECITATION".into()));
    // A refusal is a refusal, and an ending is not.
    assert!(blocked_by(&json!({}), "STOP").is_none());
    assert!(blocked_by(&json!({}), "MAX_TOKENS").is_none());
    for word in ["SAFETY", "RECITATION", "MALFORMED_FUNCTION_CALL"] {
        let said = blocked_by(&json!({}), word).expect("a refusal");
        assert!(said.contains(word), "{said}");
    }
}

/// A refusal keeps the one machine-readable word and the provider's own instant: `retryDelay` is
/// what a caller may sleep on when no `Retry-After` header carried one
/// (<https://ai.google.dev/gemini-api/docs/rate-limits>).
#[test]
fn a_refusal_keeps_its_status_word_and_its_retry_delay() {
    let body = json!({
        "error": {
            "code": 429,
            "message": "You exceeded your current quota.",
            "status": "RESOURCE_EXHAUSTED",
            "details": [{
                "@type": "type.googleapis.com/google.rpc.RetryInfo",
                "retryDelay": "17s",
            }],
        }
    })
    .to_string();
    let (said, retry) = refusal_said(None, &body);
    assert!(said.contains("You exceeded your current quota."), "{said}");
    assert!(
        said.contains("RESOURCE_EXHAUSTED"),
        "the word a caller matches on: {said}"
    );
    assert_eq!(retry, Some(17));
    // A header outranks the body, and the body is a fallback rather than the rule.
    assert_eq!(refusal_said(Some(3), &body).1, Some(3));
    // And a chunk carrying an error is a refusal with the chunk's own code.
    let refusal = in_band_refusal(
        &json!({"error": {"code": 503, "message": "overloaded", "status": "UNAVAILABLE"}}),
        200,
        None,
    )
    .expect("a refusal");
    match refusal {
        BackendError::Refused { status, body, .. } => {
            assert_eq!(status, 503);
            assert!(body.contains("UNAVAILABLE"), "{body}");
        }
        other => panic!("{other:?}"),
    }
}

/// The model is in the PATH and the stream is asked for by name, which is this protocol's whole
/// difference from the other doors; a key file's URL overrides either half.
#[test]
fn the_endpoint_puts_the_model_in_the_path_and_asks_for_sse() {
    let p = provider();
    assert_eq!(
        p.endpoint(true),
        "https://generativelanguage.googleapis.com/v1beta/models/gemini-4-hydrogen:\
         streamGenerateContent?alt=sse"
    );
    assert_eq!(p.authority(), "generativelanguage.googleapis.com");

    // A proxy base gets the same path appended.
    let creds = Credentials {
        key: "k".into(),
        from: "test".into(),
        prices: Default::default(),
        url: Some("http://127.0.0.1:8080/v1beta".into()),
    };
    let p = GeminiProvider::new(&crate::presets::GEMINI, Some("m"), creds);
    assert_eq!(
        p.endpoint(true),
        "http://127.0.0.1:8080/v1beta/models/m:streamGenerateContent?alt=sse"
    );
    assert_eq!(p.authority(), "127.0.0.1:8080");

    // And a whole endpoint is used as it stands.
    let creds = Credentials {
        key: "k".into(),
        from: "test".into(),
        prices: Default::default(),
        url: Some("http://127.0.0.1:9/v1/models/x:streamGenerateContent".into()),
    };
    let p = GeminiProvider::new(&crate::presets::GEMINI, Some("m"), creds);
    assert_eq!(
        p.endpoint(true),
        "http://127.0.0.1:9/v1/models/x:streamGenerateContent?alt=sse"
    );
}

/// The refused-request dump says what a diagnosis needs and leaks nothing: the roles, the part
/// KINDS, the character counts, and never the conversation.
#[test]
fn the_refused_request_shape_says_the_structure_and_leaks_no_text() {
    let body = json!({
        "systemInstruction": {"parts": [{"text": "SECRET-SYSTEM"}]},
        "contents": [
            {"role": "user", "parts": [{"text": "SECRET-OPERATOR-WORDS"}]},
            {"role": "model", "parts": [
                {"text": "SECRET-ANSWER"},
                {"functionCall": {"name": "read", "args": {}}, "thoughtSignature": "SECRET-SIG"},
            ]},
        ],
        "tools": [{"functionDeclarations": [{"name": "read"}, {"name": "grep"}]}],
    });
    let dump = shape(&body, 4242);
    assert!(dump.contains("4242 bytes"), "{dump}");
    assert!(dump.contains("tools=2"), "{dump}");
    assert!(dump.contains("2 content(s)"), "{dump}");
    assert!(dump.contains("[0] user"), "{dump}");
    assert!(dump.contains("[1] model"), "{dump}");
    assert!(dump.contains("text=21ch"), "{dump}");
    assert!(dump.contains("text=13ch"), "{dump}");
    assert!(dump.contains("functionCall+sig"), "{dump}");
    for secret in [
        "SECRET-SYSTEM",
        "SECRET-OPERATOR-WORDS",
        "SECRET-ANSWER",
        "SECRET-SIG",
    ] {
        assert!(!dump.contains(secret), "{dump}");
    }
}

/// The first System item is the instruction and not a turn; one that arrives later is the
/// harness's words said as a user turn, because this protocol has no system role.
#[test]
fn a_leading_system_item_joins_the_instruction_and_a_later_one_is_a_user_turn() {
    let items = vec![
        TranscriptItem::System {
            text: "the bootstrap prompt".into(),
            origin: SystemOrigin::Bootstrap,
        },
        user("go"),
        TranscriptItem::System {
            text: "an update".into(),
            origin: SystemOrigin::Update,
        },
    ];
    let b = body(&items, &[], "the caller's system");
    assert_eq!(
        b["systemInstruction"]["parts"][0]["text"], "the caller's system\n\nthe bootstrap prompt",
        "{b}"
    );
    let contents = b["contents"].as_array().expect("contents");
    assert_eq!(contents.len(), 2, "the bootstrap item is not a turn: {b}");
    assert_eq!(contents[1]["parts"][0]["text"], "an update");
}

/// A picture that cannot be sent is NAMED, with the reason, rather than sent to be refused: one
/// bad picture refuses every turn after it, and the transcript is append-only.
#[test]
fn a_picture_that_is_not_inline_bytes_is_named_instead_of_sent() {
    let items = vec![TranscriptItem::User {
        speaker: Speaker::default(),
        parts: vec![UserPart::Image {
            media_type: "image/png".into(),
            data_ref: "https://example.test/a.png".into(),
        }],
    }];
    let b = body(&items, &[], "");
    let said = b["contents"][0]["parts"][0]["text"]
        .as_str()
        .unwrap_or_default();
    assert!(
        said.starts_with("[image 1 · image/png · size unknown — not sent:"),
        "{said}"
    );
    assert!(said.contains("only inline image bytes are sent"), "{said}");
}
