//! The Responses client, against bodies and recorded streams. The wire itself is
//! `crates/provider/tests/responses_stream.rs`, which runs a stub server.

use super::*;
use letibot_transcript::{CallOrigin, ReasoningField, Speaker, ToolOutcome};

fn creds(url: Option<&str>) -> Credentials {
    Credentials {
        key: "sk-test".into(),
        from: "test".into(),
        prices: Default::default(),
        url: url.map(str::to_string),
    }
}

/// A provider over a box with NO catalogue: a test's answer is then about this code and
/// not about which files the machine running it happens to have.
fn provider(model: Option<&str>) -> ResponsesProvider {
    ResponsesProvider::new(&crate::presets::OPENAI, model, creds(None))
        .with_catalogue(std::sync::Arc::new(crate::catalogue::Catalogue::default()))
}

fn user(text: &str) -> TranscriptItem {
    TranscriptItem::User {
        speaker: Speaker::Operator,
        parts: vec![UserPart::Text { text: text.into() }],
    }
}

fn reasoning(text: &str) -> TranscriptItem {
    TranscriptItem::Reasoning {
        text: text.into(),
        field: ReasoningField::ReasoningContent,
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
                name: "bash".into(),
                arguments: "{}".into(),
            })
            .collect(),
        truncated: false,
    }
}

fn result(id: &str) -> TranscriptItem {
    TranscriptItem::ToolResult {
        call_id: id.into(),
        name: "bash".into(),
        outcome: ToolOutcome::Ok,
        payload: "done".into(),
        edits: Vec::new(),
        origin: None,
        media: None,
    }
}

fn ran_by_the_operator(id: &str, payload: &str) -> TranscriptItem {
    TranscriptItem::ToolResult {
        call_id: id.into(),
        name: "bash".into(),
        outcome: ToolOutcome::Ok,
        payload: payload.into(),
        edits: Vec::new(),
        origin: Some(CallOrigin::Operator { who: "dead".into() }),
        media: None,
    }
}

fn issued(summary: &str, id: &str) -> Issued {
    Issued {
        summary: summary.into(),
        item_id: id.into(),
        encrypted: format!("cipher-{id}"),
    }
}

/// A catalogue from its JSON, through the same reader a file goes through: a fixture of what
/// a model's own row says, so a test about pictures is not a test about this box's files.
fn catalogue_saying(text: &str) -> crate::catalogue::Catalogue {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "responses-catalogue-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::write(&path, text).expect("the fixture is written");
    let cat = crate::catalogue::Catalogue::read(&path).expect("the fixture parses");
    let _ = std::fs::remove_file(&path);
    cat
}

/// The whole request shape, on one transcript: what the `messages` API would have called
/// roles and content, this protocol calls items and parts, and the record stays with us.
#[test]
fn the_request_is_input_items_and_the_record_is_ours() {
    let items = vec![user("read a.txt"), calls(&["call_1"]), result("call_1")];
    let tools = vec![
        r#"{"type":"function","function":{"name":"read","description":"a file","parameters":{"type":"object"}}}"#
            .to_string(),
    ];
    let p = provider(Some("gpt-6-astra"));
    let body = p
        .body(&TurnRequest {
            system: "be terse",
            tools_json: &tools,
            items: &items,
            tail: &[],
            max_output_tokens: Some(256),
            thinking: None,
        })
        .expect("the body builds");

    assert_eq!(body["model"], "gpt-6-astra");
    assert_eq!(body["stream"], true);
    // `store` defaults to TRUE on the API, which is thirty days of the operator's
    // conversation in somebody else's retention: this build says no on every request.
    assert_eq!(body["store"], false);
    assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
    assert_eq!(body["instructions"], "be terse");
    // A chat-completions field is not a field here at all.
    assert!(body.get("messages").is_none(), "{body:#?}");
    assert_eq!(body["max_output_tokens"], 256);

    let input = body["input"].as_array().expect("an input list");
    assert_eq!(input.len(), 3, "{input:#?}");
    assert_eq!(input[0]["role"], "user");
    // A message of words is a STRING: an array where a string was re-prefills the cache.
    assert_eq!(input[0]["content"], "read a.txt");
    // A call and its result are items of their own, paired by `call_id` rather than by
    // position, which is the shape this protocol has and chat completions does not.
    assert_eq!(input[1]["type"], "function_call");
    assert_eq!(input[1]["call_id"], "call_1");
    assert_eq!(input[1]["name"], "bash");
    assert_eq!(input[2]["type"], "function_call_output");
    assert_eq!(input[2]["call_id"], "call_1");
    assert_eq!(input[2]["output"], "done");
    // Tools are flat here, and `strict` is required: the harness's schemas do not set
    // `additionalProperties: false`, so `true` would be refused for the wrong reason.
    assert_eq!(body["tools"][0]["type"], "function");
    assert_eq!(body["tools"][0]["name"], "read");
    assert_eq!(body["tools"][0]["strict"], false);
    assert!(body["tools"][0].get("function").is_none(), "{body:#?}");
}

/// A reasoning row goes back as the ITEM the account issued, or not at all. Its summary is
/// the only part the transcript kept, so it is what pairs the row with the ciphertext.
#[test]
fn a_reasoning_row_is_replayed_only_where_this_provider_issued_it() {
    let items = vec![
        user("what is the date?"),
        reasoning("I should call get_date."),
        calls(&["call_1"]),
        result("call_1"),
    ];
    let p = provider(Some("gpt-6-astra"));
    let req = |p: &ResponsesProvider| {
        p.body(&TurnRequest {
            system: "",
            tools_json: &[],
            items: &items,
            tail: &[],
            max_output_tokens: None,
            thinking: None,
        })
        .expect("a body")
    };

    // Nothing was issued on this instance, so there is no item to send: a `reasoning` item
    // with no ciphertext is an id the API cannot read back.
    let cold = req(&p);
    assert!(
        !cold["input"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["type"] == "reasoning"),
        "a thought this client did not issue cannot be replayed: {cold:#?}"
    );

    // The stream's own record, and now the item comes back - before the call it led to.
    p.keep(vec![issued("I should call get_date.", "rs_1")]);
    let warm = req(&p);
    let input = warm["input"].as_array().unwrap();
    let at = input
        .iter()
        .position(|i| i["type"] == "reasoning")
        .expect("the issued item is replayed");
    assert_eq!(
        input[at],
        json!({
            "type": "reasoning",
            "id": "rs_1",
            "encrypted_content": "cipher-rs_1",
            "summary": [{"type": "summary_text", "text": "I should call get_date."}],
        })
    );
    assert_eq!(
        input[at + 1]["type"],
        "function_call",
        "the item sits where it was produced: {warm:#?}"
    );
}

/// A round that issued two reasoning items wrote ONE row of both summaries, and the walk
/// re-joins them in that order rather than pairing half a thought.
#[test]
fn a_round_that_issued_two_items_pairs_them_back_in_order() {
    let ledger = vec![issued("first.", "rs_1"), issued(" second.", "rs_2")];
    let mut cursor = 0;
    let got = pairing("first. second.", &ledger, &mut cursor);
    assert_eq!(got.len(), 2);
    assert_eq!(got[0].item_id, "rs_1");
    assert_eq!(got[1].item_id, "rs_2");
    assert_eq!(cursor, 2);
}

/// The row a record answered can be gone - compaction evicted it, or the round it came from
/// was never committed - and one such record must not hide every thought after it.
#[test]
fn a_stale_record_does_not_hide_the_thoughts_after_it() {
    let ledger = vec![
        issued("a thought whose row was evicted", "rs_old"),
        issued("a thought that is still there", "rs_new"),
    ];
    let mut cursor = 0;
    let got = pairing("a thought that is still there", &ledger, &mut cursor);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].item_id, "rs_new");
    assert_eq!(cursor, 2);
    // And nothing pairs with words no record spelled.
    let mut cursor = 0;
    assert!(pairing("a thought nobody issued", &ledger, &mut cursor).is_empty());
    assert_eq!(cursor, 0, "a miss leaves the cursor where it was");
}

/// `--effort` is this protocol's `reasoning.effort`, and a cheap round asks for `none`
/// rather than for the model's default. A word the API does not document is dropped: an
/// unknown enum value is a 400 on every turn.
#[test]
fn effort_becomes_reasoning_effort_and_a_cheap_turn_asks_for_none() {
    let p = provider(Some("gpt-6-astra")).with_effort(Some("xhigh"));
    let req = |thinking| TurnRequest {
        system: "",
        tools_json: &[],
        items: &[],
        tail: &[],
        max_output_tokens: None,
        thinking,
    };
    let mut p = p;
    p.thinking = true;
    let usual = p.body(&req(None)).expect("a body");
    assert_eq!(usual["reasoning"]["effort"], "xhigh");
    // `summary: auto` is what puts the summary on the stream, and the summary is what the
    // transcript row that pairs the encrypted item back says.
    assert_eq!(usual["reasoning"]["summary"], "auto");
    let cheap = p.body(&req(Some(false))).expect("a body");
    assert_eq!(cheap["reasoning"], json!({"effort": "none"}));

    // Not thinking, nobody asked, no effort: no `reasoning` at all, which is the model's own
    // default. (`reasoning` on a model that does not reason is a 400 on every turn.)
    let plain = provider(Some("gpt-6-astra"))
        .body(&req(None))
        .expect("a body");
    assert!(plain.get("reasoning").is_none(), "{plain}");

    // A word the API does not document never reaches the wire.
    let odd = provider(Some("gpt-6-astra")).with_effort(Some("heroic"));
    let mut odd = odd;
    odd.thinking = true;
    let body = odd.body(&req(None)).expect("a body");
    assert!(body["reasoning"].get("effort").is_none(), "{body}");

    // An `--effort` this API understands IS a request to think: a session started with
    // `--effort high` and no `--thinking` would otherwise send no `reasoning` at all, and the
    // operator's own word would be the one thing the request dropped.
    let asked = provider(Some("gpt-6-astra")).with_effort(Some("high"));
    let body = asked.body(&req(None)).expect("a body");
    assert_eq!(body["reasoning"]["effort"], "high");
    // And an explicit `false` from the caller (a cheap round) still wins over it.
    let cheap = asked.body(&req(Some(false))).expect("a body");
    assert_eq!(cheap["reasoning"], json!({"effort": "none"}));
}

/// The operator's sampling, in this protocol's names: two of the chat-completions fields
/// are fields here, `max_tokens` is `max_output_tokens`, and the rest are not fields at
/// all - sending them is a 400 on a request nobody can see the reason for.
#[test]
fn sampling_is_translated_into_this_protocols_names() {
    let mut p = provider(Some("gpt-6-astra"));
    p.sampling = json!({
        "temperature": 0.2, "top_p": 0.9, "max_tokens": 8000,
        "seed": 7, "presence_penalty": 0.1,
    });
    let req = |max| TurnRequest {
        system: "",
        tools_json: &[],
        items: &[],
        tail: &[],
        max_output_tokens: max,
        thinking: None,
    };
    let body = p.body(&req(Some(256))).expect("a body");
    assert_eq!(body["temperature"], 0.2);
    assert_eq!(body["top_p"], 0.9);
    assert_eq!(body["max_output_tokens"], 256, "the cap wins over sampling");
    assert!(body.get("seed").is_none(), "{body}");
    assert!(body.get("presence_penalty").is_none(), "{body}");
    assert!(body.get("max_tokens").is_none(), "{body}");

    let loose = p.body(&req(None)).expect("a body");
    assert_eq!(loose["max_output_tokens"], 8000, "the operator's own cap");
}

/// A tool schema the harness hands over in either shape becomes one flat tool; one with
/// neither `function` nor `name` is refused rather than sent as a string the API would 400.
#[test]
fn tools_are_flat_and_a_schema_with_no_name_is_refused() {
    let bare =
        vec![r#"{"name":"read","description":"a file","parameters":{"type":"object"}}"#.into()];
    let got = tools(&bare).expect("a bare schema is wrapped");
    assert_eq!(got[0]["name"], "read");
    assert_eq!(got[0]["description"], "a file");
    assert_eq!(got[0]["parameters"]["type"], "object");
    let why = tools(&[r#"{"description":"nobody"}"#.into()]).unwrap_err();
    assert!(why.contains("neither"), "{why}");
    assert!(
        tools(&["not json".into()])
            .unwrap_err()
            .contains("not JSON")
    );
}

/// A transcript is history: an output whose call nothing opened is dropped, and one the
/// OPERATOR ran becomes their own turn, because no `function_call` above it can answer it.
#[test]
fn an_orphan_output_is_dropped_and_the_operators_own_is_their_turn() {
    let items = vec![
        user("! ls -la"),
        ran_by_the_operator("bang-1", "total 8"),
        result("ghost"),
        user("next"),
    ];
    let p = provider(Some("gpt-6-astra"));
    let body = p
        .body(&TurnRequest {
            system: "",
            tools_json: &[],
            items: &items,
            tail: &[],
            max_output_tokens: None,
            thinking: None,
        })
        .expect("a body");
    let input = body["input"].as_array().unwrap();
    assert!(
        !input.iter().any(|i| i["call_id"] == "ghost"),
        "an output answering nothing is refused by the API: {input:#?}"
    );
    let carried = input
        .iter()
        .find(|i| i["content"] == "total 8")
        .expect("the operator's own output reached the model");
    assert_eq!(carried["role"], "user");
    assert!(
        !input.iter().any(|i| i["type"] == "function_call_output"),
        "there is no call above it for an output to answer: {input:#?}"
    );
}

/// A row recorded while a call was running - a room note, a steering message - must not
/// orphan the result that answers it: the note waits behind the results, which is what the
/// chat-completions path measured and the same shape here.
#[test]
fn a_note_between_a_call_and_its_result_waits_for_the_result() {
    let items = vec![
        user("go"),
        calls(&["c-1"]),
        TranscriptItem::Notice {
            kind: letibot_transcript::NoticeKind::Monitor,
            text: "room: a message for everyone".into(),
            facts: None,
        },
        result("c-1"),
    ];
    let p = provider(Some("gpt-6-astra"));
    let body = p
        .body(&TurnRequest {
            system: "",
            tools_json: &[],
            items: &items,
            tail: &[],
            max_output_tokens: None,
            thinking: None,
        })
        .expect("a body");
    let input = body["input"].as_array().unwrap();
    assert!(
        input
            .iter()
            .all(|i| i["type"] != "function_call_output" || i["output"] == "done"),
        "the real result was replaced by a filler: {input:#?}"
    );
    let answered = input
        .iter()
        .position(|i| i["type"] == "function_call_output")
        .expect("the call is answered");
    let note = input
        .iter()
        .position(|i| i["content"] == "room: a message for everyone")
        .expect("the note was not lost");
    assert!(
        note > answered,
        "the note waits behind the results: {input:#?}"
    );
}

/// A turn interrupted between a call and its result leaves exactly one shape behind, and
/// the gap is filled where it is rather than the request being sent to be refused.
#[test]
fn an_unanswered_call_is_filled_before_whatever_came_next() {
    let items = vec![user("go"), calls(&["c-1"]), user("never mind, carry on")];
    let p = provider(Some("gpt-6-astra"));
    let body = p
        .body(&TurnRequest {
            system: "",
            tools_json: &[],
            items: &items,
            tail: &[],
            max_output_tokens: None,
            thinking: None,
        })
        .expect("a body");
    let input = body["input"].as_array().unwrap();
    let filler = input
        .iter()
        .position(|i| i["type"] == "function_call_output")
        .expect("the gap was filled");
    let next = input
        .iter()
        .position(|i| i["content"] == "never mind, carry on")
        .expect("the operator's row is still there");
    assert!(filler < next, "the filler answers the call it belongs to");
    assert!(
        input[filler]["output"]
            .as_str()
            .unwrap()
            .contains("no result was recorded"),
        "it says what is true rather than faking a success: {input:#?}"
    );
}

/// A request may not END on an instruction: the compaction's own instruction is appended
/// after the conversation, and that is the shape that made a head fail every compaction for
/// hours with an error naming a field instead of the shape. Said as the operator's turn, it
/// is a message like any other.
#[test]
fn a_request_that_ends_on_an_instruction_ends_on_a_message() {
    let items = vec![
        user("go"),
        TranscriptItem::System {
            text: "summarise the above".into(),
            origin: letibot_transcript::SystemOrigin::Update,
        },
    ];
    let p = provider(Some("gpt-6-astra"));
    let body = p
        .body(&TurnRequest {
            system: "be terse",
            tools_json: &[],
            items: &items,
            tail: &[],
            max_output_tokens: None,
            thinking: None,
        })
        .expect("a body");
    let input = body["input"].as_array().unwrap();
    assert_eq!(input.len(), 2, "{input:#?}");
    assert_eq!(input[1]["role"], "user", "{input:#?}");
    assert_eq!(input[1]["content"], "summarise the above");
    // And the system PROMPT is still the top-level instruction, not a message.
    assert_eq!(body["instructions"], "be terse");
}

/// A transcript that leads with a System row keeps it as a message: the prompt must not move
/// between the two places, because the prefix cache is keyed on the bytes.
#[test]
fn a_transcript_that_leads_with_a_system_row_keeps_it_in_place() {
    let items = vec![
        TranscriptItem::System {
            text: "be terse".into(),
            origin: letibot_transcript::SystemOrigin::Bootstrap,
        },
        user("hi"),
    ];
    let p = provider(Some("gpt-6-astra"));
    let body = p
        .body(&TurnRequest {
            system: "be terse",
            tools_json: &[],
            items: &items,
            tail: &[],
            max_output_tokens: None,
            thinking: None,
        })
        .expect("a body");
    assert!(body.get("instructions").is_none(), "{body:#?}");
    assert_eq!(body["input"][0]["role"], "developer");
}

/// A picture is SENT to a model that takes pictures and NAMED to one the catalogue says is
/// text only, by the same rules and the same words the chat-completions client uses: it is
/// the model's fault, not the protocol's.
#[test]
fn a_picture_is_sent_to_a_model_that_sees_and_named_to_one_that_does_not() {
    const DATA: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFAAH/q842iQAAAABJRU5ErkJggg==";
    let items = vec![TranscriptItem::User {
        speaker: Speaker::Operator,
        parts: vec![
            UserPart::Text {
                text: "what is this".into(),
            },
            UserPart::Image {
                media_type: "image/png".into(),
                data_ref: DATA.into(),
            },
        ],
    }];
    let req = |p: &ResponsesProvider| {
        p.body(&TurnRequest {
            system: "",
            tools_json: &[],
            items: &items,
            tail: &[],
            max_output_tokens: None,
            thinking: None,
        })
        .expect("a body")
    };
    let seen = req(&provider(Some("gpt-6-astra")));
    assert_eq!(seen["input"][0]["content"][1]["type"], "input_image");
    assert_eq!(seen["input"][0]["content"][1]["image_url"], DATA);

    // A catalogue row that says `modalities.input: ["text"]` is a measurement of the model.
    let cat = catalogue_saying(
        r#"{"openai": {"models": {"gpt-6-astra": {
            "limit": {"context": 1000}, "modalities": {"input": ["text"], "output": ["text"]}}}}}"#,
    );
    let blind = provider(Some("gpt-6-astra")).with_catalogue(std::sync::Arc::new(cat));
    let named = req(&blind);
    // Nothing but words is left, so the message is a STRING again - an array where a string
    // was would re-prefill the cache for a picture that is not being sent anyway.
    let content = named["input"][0]["content"]
        .as_str()
        .expect("a message of words is a string");
    assert!(
        content.contains("not sent: gpt-6-astra takes text only"),
        "{named:#?}"
    );
    assert!(content.starts_with("what is this"), "{named:#?}");
}

/// The stream: text deltas, a summary delta, a call's name from the item and its arguments
/// from the fragments, and the usage off the terminal event.
#[test]
fn the_stream_builds_text_a_summary_a_call_and_the_usage() {
    let mut acc = Accumulator::default();
    let events = [
        json!({"type": "response.created", "response": {"model": "gpt-6-astra", "usage": null}}),
        json!({"type": "response.output_item.added", "output_index": 0,
               "item": {"id": "msg_1", "type": "message", "status": "in_progress",
                        "role": "assistant", "content": []}}),
        json!({"type": "response.output_text.delta", "item_id": "msg_1",
               "output_index": 0, "content_index": 0, "delta": "Hel"}),
        json!({"type": "response.output_text.delta", "item_id": "msg_1",
               "output_index": 0, "content_index": 0, "delta": "lo"}),
        json!({"type": "response.reasoning_summary_text.delta", "item_id": "rs_1",
               "output_index": 1, "summary_index": 0, "delta": "I should read."}),
        json!({"type": "response.output_item.done", "output_index": 1,
               "item": {"id": "rs_1", "type": "reasoning", "encrypted_content": "cipher-rs_1",
                        "summary": [{"type": "summary_text", "text": "I should read."}]}}),
        json!({"type": "response.output_item.added", "output_index": 2,
               "item": {"id": "fc_1", "type": "function_call", "call_id": "call_1",
                        "name": "read", "arguments": ""}}),
        json!({"type": "response.function_call_arguments.delta", "item_id": "fc_1",
               "output_index": 2, "delta": "{\"path\":"}),
        json!({"type": "response.function_call_arguments.delta", "item_id": "fc_1",
               "output_index": 2, "delta": "\"a.txt\"}"}),
        json!({"type": "response.function_call_arguments.done", "item_id": "fc_1",
               "output_index": 2, "arguments": "{\"path\":\"a.txt\"}"}),
        json!({"type": "response.completed", "response": {
            "model": "gpt-6-astra", "status": "completed", "output": [],
            "usage": {"input_tokens": 100, "output_tokens": 7,
                      "input_tokens_details": {"cached_tokens": 60, "cache_write_tokens": 0},
                      "output_tokens_details": {"reasoning_tokens": 3},
                      "total_tokens": 107}}}),
    ];
    let mut deltas = 0;
    for e in &events {
        deltas += acc.absorb(e).len();
    }
    // Two text deltas, one summary, one name and two argument fragments.
    assert_eq!(deltas, 6, "the stream's own divisions are kept");
    assert!(acc.ended, "a completed response is the end of the answer");

    let mut creds = creds(None);
    creds.prices.insert(
        "gpt-6-astra".into(),
        crate::presets::Prices {
            input: 1.0,
            cached: 0.1,
            output: 2.0,
        },
    );
    let (done, kept) = acc.finish(
        "gpt-6-astra",
        &creds,
        12,
        &crate::presets::OPENAI,
        &crate::catalogue::Catalogue::default(),
    );
    assert_eq!(done.text, "Hello");
    assert_eq!(done.reasoning, "I should read.");
    assert_eq!(done.tool_calls.len(), 1);
    assert_eq!(
        done.tool_calls[0].id, "call_1",
        "the call's own id is echoed"
    );
    assert_eq!(done.tool_calls[0].name, "read");
    assert_eq!(done.tool_calls[0].arguments, "{\"path\":\"a.txt\"}");
    assert_eq!(done.finish, Finish::ToolCalls);
    assert_eq!(done.cost.prompt_tokens, 100);
    assert_eq!(done.cost.cached_tokens, 60);
    assert_eq!(done.cost.generated_tokens, 7);
    // 40*1 + 60*0.1 + 7*2 = 60 micro-USD.
    assert_eq!(done.cost.micros_usd, Some(60));
    // Reasoning tokens are inside the generated count and inside the price: the verbatim
    // usage object is where a reader finds them.
    assert!(
        done.raw_usage
            .as_deref()
            .unwrap()
            .contains("reasoning_tokens"),
        "{:?}",
        done.raw_usage
    );
    // And the round's thought is kept, worded so the row saying the same words finds it.
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].summary, "I should read.");
    assert_eq!(kept[0].encrypted, "cipher-rs_1");
}

/// The request's own cap is a LENGTH stop, not a failure, and any other reason keeps the
/// provider's word.
#[test]
fn an_incomplete_response_is_a_length_stop_and_says_why() {
    let mut acc = Accumulator::default();
    acc.absorb(&json!({"type": "response.output_text.delta", "delta": "half a th"}));
    acc.absorb(&json!({"type": "response.incomplete", "response": {
        "status": "incomplete", "incomplete_details": {"reason": "max_output_tokens"},
        "usage": {"input_tokens": 10, "output_tokens": 16,
                  "input_tokens_details": {"cached_tokens": 0},
                  "output_tokens_details": {"reasoning_tokens": 16}}}}));
    let (done, _) = acc.finish(
        "gpt-6-astra",
        &creds(None),
        1,
        &crate::presets::OPENAI,
        &crate::catalogue::Catalogue::default(),
    );
    assert_eq!(done.finish, Finish::Length);
    assert_eq!(done.text, "half a th", "what arrived is kept, not dropped");

    let mut other = Accumulator::default();
    other.absorb(&json!({"type": "response.incomplete", "response": {
        "status": "incomplete", "incomplete_details": {"reason": "content_filter"}}}));
    let (done, _) = other.finish(
        "gpt-6-astra",
        &creds(None),
        1,
        &crate::presets::OPENAI,
        &crate::catalogue::Catalogue::default(),
    );
    assert_eq!(done.finish, Finish::Other("content_filter".into()));
}

/// An in-band failure has no status of its own - the headers said 200 before the first
/// event - so its CODE is what the retry policy is left to read.
#[test]
fn an_in_band_failure_carries_its_code_and_the_status_the_retry_policy_reads() {
    // A rate limit is *not now*, and the daemon's ladder waits for it rather than giving up.
    let mut acc = Accumulator::default();
    acc.absorb(&json!({"type": "error", "code": "rate_limit_exceeded",
                       "param": null, "message": "Rate limit reached"}));
    let failure = acc.failure.take().expect("a failure");
    match failure {
        BackendError::Refused { status, body, .. } => {
            assert_eq!(
                status, 429,
                "a rate limit is the status the ladder waits on"
            );
            assert!(body.contains("rate_limit_exceeded"), "{body}");
        }
        other => panic!("{other:?}"),
    }

    // A `response.failed` says it in its own vocabulary; an unreadable code is a 5xx that
    // is worth taking again, which is what `server_error` is.
    let mut acc = Accumulator::default();
    acc.absorb(
        &json!({"type": "response.failed", "response": {"status": "failed",
        "error": {"code": "server_error", "message": "The model failed"}}}),
    );
    let failure = acc.failure.take().expect("a failure");
    match failure {
        BackendError::Refused { status, body, .. } => {
            assert_eq!(status, 500);
            assert!(body.contains("The model failed"), "{body}");
        }
        other => panic!("{other:?}"),
    }

    // A quota is refused, not retried, and the code in the sentence is what says so:
    // `insufficient_quota` is in `letibot_http::retry`'s money list.
    let refusal = in_band_failure(
        &json!({"code": "insufficient_quota", "message": "You exceeded your quota"}),
        400,
        None,
    );
    match refusal {
        BackendError::Refused { body, .. } => {
            assert!(body.contains("insufficient_quota"), "{body}")
        }
        other => panic!("{other:?}"),
    }
}

/// A refusal on a model that takes pictures may be about ONE picture, and the client says
/// which of the two refusals it read: this protocol's validator names the part type.
#[test]
fn this_protocols_text_only_refusal_is_read_and_a_picture_refusal_is_not_it() {
    assert!(text_only_refusal(
        "Invalid value: 'input_image'. Supported values are: 'input_text'."
    ));
    // The chat-completions spelling still counts: the same server answers both doors.
    assert!(text_only_refusal(
        "Invalid 'input[0].content[1].type': 'image_url' is not supported"
    ));
    // Bytes that are not an image are the other fact, and must not withhold every picture
    // this model is ever sent.
    assert!(!text_only_refusal(
        "You have uploaded an unsupported image. Please make sure your image is valid"
    ));
    assert!(crate::messages::picture_refusal(
        "You have uploaded an unsupported image. Please make sure your image is valid"
    ));
}

/// The refused-request dump says what a diagnosis needs and leaks nothing: the distinctions
/// a theory turns on - absent against present, counts, the ids - and never the conversation.
#[test]
fn the_refused_request_shape_says_what_is_needed_and_leaks_nothing() {
    let body = json!({
        "model": "gpt-6-astra",
        "stream": true,
        "store": false,
        "tools": [{"type": "function", "name": "read", "strict": false}],
        "input": [
            {"role": "developer", "content": "SECRET-SYSTEM-TEXT"},
            {"role": "user", "content": [{"type": "input_text", "text": "SECRET-WORDS"},
                                          {"type": "input_image", "image_url": "SECRET-DATA"}]},
            {"type": "reasoning", "id": "rs_abcdefghijkl", "encrypted_content": "SECRET-CIPHER",
             "summary": [{"type": "summary_text", "text": "SECRET-THOUGHT"}]},
            {"type": "function_call", "call_id": "call_abcdefghijkl", "name": "read",
             "arguments": "{\"path\":\"SECRET-ARG\"}"},
            {"type": "function_call_output", "call_id": "call_abcdefghijkl", "output": "SECRET-OUT"},
        ],
    });
    let dump = shape(&body, 4242);
    assert!(dump.contains("4242 bytes"), "{dump}");
    assert!(dump.contains("tools=1"), "{dump}");
    assert!(dump.contains("store=false"), "{dump}");
    assert!(dump.contains("5 input item(s)"), "{dump}");
    assert!(dump.contains("[0] developer content=18ch"), "{dump}");
    assert!(dump.contains("[1] user content=array(2)"), "{dump}");
    assert!(
        dump.contains("reasoning content=absent output=absent id=rs_abcdefg call_id=absent encrypted_content=13ch"),
        "the ciphertext's LENGTH is the fact a diagnosis uses: {dump}"
    );
    assert!(dump.contains("call_id=call_abcde"), "{dump}");
    assert!(dump.contains("[4] function_call_output"), "{dump}");
    for secret in [
        "SECRET-SYSTEM-TEXT",
        "SECRET-WORDS",
        "SECRET-DATA",
        "SECRET-CIPHER",
        "SECRET-THOUGHT",
        "SECRET-ARG",
        "SECRET-OUT",
    ] {
        assert!(
            !dump.contains(secret),
            "the dump quoted content ({secret}): {dump}"
        );
    }
}

/// The served model is what gets priced, not the name we asked for: a request for an alias
/// can be answered by a stream naming another, and nothing fails because it is supposed to
/// resolve.
#[test]
fn the_served_model_is_what_gets_priced() {
    let mut creds = creds(None);
    creds.prices.insert(
        "gpt-6-astra-old".into(),
        crate::presets::Prices {
            input: 10.0,
            cached: 1.0,
            output: 20.0,
        },
    );
    creds.prices.insert(
        "gpt-6-astra".into(),
        crate::presets::Prices {
            input: 1.0,
            cached: 0.1,
            output: 2.0,
        },
    );
    let mut acc = Accumulator::default();
    acc.absorb(&json!({"type": "response.output_text.delta", "delta": "hi"}));
    acc.absorb(&json!({"type": "response.completed", "response": {
        "model": "gpt-6-astra", "status": "completed", "output": [],
        "usage": {"input_tokens": 1000, "output_tokens": 100,
                  "input_tokens_details": {"cached_tokens": 600},
                  "output_tokens_details": {"reasoning_tokens": 0}}}}));
    let (done, _) = acc.finish(
        "gpt-6-astra-old",
        &creds,
        1,
        &crate::presets::OPENAI,
        &crate::catalogue::Catalogue::default(),
    );
    assert_eq!(
        done.cost.micros_usd,
        Some(660),
        "priced from the model that served the turn, not the one asked for"
    );
}

/// Unpriced is `None`, not zero: an account whose prices nobody wrote down is not free.
#[test]
fn an_unpriced_model_costs_nothing_rather_than_zero() {
    let mut acc = Accumulator::default();
    acc.absorb(&json!({"type": "response.completed", "response": {
        "model": "gpt-6-astra", "status": "completed", "output": []}}));
    let (done, _) = acc.finish(
        "gpt-6-astra",
        &creds(None),
        1,
        &crate::presets::OPENAI,
        &crate::catalogue::Catalogue::default(),
    );
    assert_eq!(done.cost.micros_usd, None);
}

/// The authority is the host the request actually goes to, including when the operator's key
/// file moved it.
#[test]
fn the_authority_is_the_host_the_request_goes_to() {
    let plain = ResponsesProvider::new(&crate::presets::OPENAI, Some("m"), creds(None));
    assert_eq!(plain.authority(), "api.openai.com");
    let moved = ResponsesProvider::new(
        &crate::presets::OPENAI,
        Some("m"),
        creds(Some("http://10.0.0.7:8443/v1/responses")),
    );
    assert_eq!(moved.authority(), "10.0.0.7:8443");
}

/// The preset says which protocol it speaks, and the key is read the way every other door's
/// is: the name in the file is the section, the environment variable is the one it declares.
#[test]
fn the_openai_door_is_the_responses_protocol_and_names_its_own_key() {
    use crate::presets::{ALL, Protocol};
    let preset = crate::Preset::parse("openai").expect("a door this build knows");
    assert_eq!(preset.protocol, Protocol::Responses);
    assert_eq!(preset.url, "https://api.openai.com/v1/responses");
    assert_eq!(preset.key_env, "OPENAI_API_KEY");
    assert!(preset.url.contains(&format!("/{}", "responses")));
    assert!(ALL.contains(&&crate::presets::OPENAI), "listed for /models");
    // No other door speaks it: the protocol is the preset's, not read off a path.
    for p in ALL.iter().filter(|p| p.name != "openai") {
        assert_ne!(p.protocol, Protocol::Responses, "{}", p.name);
        assert!(!p.url.contains("/responses"), "{}", p.name);
    }
}
