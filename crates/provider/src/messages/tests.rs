use super::*;
use letibot_transcript::{SystemOrigin, ToolCall, ToolOutcome};

/// [`super::convert`] for a model that SEES, which is what every test here means unless it
/// says otherwise: the subject is the SHAPE of a request, and a model's eyes are an input to
/// that shape. The tests about eyes call `super::convert` with their own [`Eyes`].
fn convert(system: &str, items: &[TranscriptItem], echo_reasoning: bool) -> Vec<Value> {
    super::convert(
        system,
        items,
        echo_reasoning,
        Eyes::Sees,
        Withheld::nothing(),
    )
}

/// A real picture, as the reference a row carries: a signature and nothing else is not an
/// image, and a request carrying one is a placeholder whatever the model's eyes are. Every test
/// here that expects an `image_url` needs a complete one.
fn picture_ref() -> String {
    letibot_transcript::testing::data_ref("image/png", &letibot_transcript::testing::png(2, 2))
}

/// The request's tail is the LAST message, and it is not in the transcript. It goes after
/// everything the session holds, so it re-prefills only the tail, and it is rendered from the
/// request's own slice, so a round that does not take it leaves nothing.
#[test]
fn the_tail_is_the_last_message_and_is_not_the_transcript() {
    let items = vec![TranscriptItem::User {
        speaker: letibot_transcript::Speaker::Operator,
        parts: vec![letibot_transcript::UserPart::Text {
            text: "what about the notes?".into(),
        }],
    }];
    let tail = vec![TranscriptItem::User {
        speaker: letibot_transcript::Speaker::Agent,
        parts: vec![letibot_transcript::UserPart::Text {
            text: "[standing-notes offer] `.letibot/notes/x.md` looks relevant".into(),
        }],
    }];
    let msgs = convert_with_tail(
        "be terse",
        &items,
        &tail,
        true,
        Eyes::Sees,
        Withheld::nothing(),
    );
    assert_eq!(msgs.len(), 3, "system, the prompt, the offer");
    let last = msgs.last().expect("messages");
    assert_eq!(last["role"], "user");
    assert!(
        last["content"]
            .as_str()
            .is_some_and(|c| c.contains("standing-notes offer")),
        "the tail is what the request ends on: {last}"
    );
    assert!(
        msgs[1]["content"]
            .as_str()
            .is_some_and(|c| c.contains("what about the notes?")),
        "and the transcript is carried unchanged ahead of it"
    );

    // And an empty tail adds nothing at all - which is what an offer nobody
    // composed, or an offer the round did not take, sends.
    assert_eq!(convert("be terse", &items, true).len(), 2);
}

/// A request must not end on a system message, which is the shape that refused every
/// compaction this session attempted. This pins the behaviour so the next reader need not
/// re-run it against a paid API.
#[test]
fn a_trailing_system_item_is_sent_as_a_user_message() {
    let items = vec![
        TranscriptItem::User {
            speaker: letibot_transcript::Speaker::Operator,
            parts: vec![letibot_transcript::UserPart::Text { text: "hi".into() }],
        },
        TranscriptItem::Assistant {
            text: "hello".into(),
            tool_calls: vec![],
            truncated: false,
        },
        TranscriptItem::System {
            text: "Write the record in exactly these sections.".into(),
            origin: SystemOrigin::Update,
        },
    ];
    let msgs = convert("you are helpful", &items, true);
    let last = msgs.last().expect("messages");
    assert_eq!(
        last["role"], "user",
        "the trailing instruction travels as a user turn: a trailing `system` with \
         `tools` present is the 400 (measured 2026-10-07)"
    );
    assert_eq!(
        last["content"], "Write the record in exactly these sections.",
        "the text is unchanged -- only the role is translated"
    );
}

/// A request whose only message is the system prompt keeps it: there is no
/// conversation for a trailing instruction to trail, and turning the prompt into a
/// user turn would be a different request rather than the same one reshaped.
#[test]
fn a_lone_system_prompt_is_not_translated() {
    let msgs = convert("you are helpful", &[], true);
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0]["role"], "system");
}

/// A system message that is not last keeps its role: the translation is about the shape of
/// the request's END, so the head prompt and any mid-conversation update are left as they
/// were, which keeps this from being a rewrite of the transcript's meaning.
#[test]
fn an_interior_system_item_keeps_its_role() {
    let items = vec![
        TranscriptItem::System {
            text: "boot".into(),
            origin: SystemOrigin::Bootstrap,
        },
        TranscriptItem::System {
            text: "an update".into(),
            origin: SystemOrigin::Update,
        },
        TranscriptItem::User {
            speaker: letibot_transcript::Speaker::Operator,
            parts: vec![letibot_transcript::UserPart::Text { text: "go".into() }],
        },
    ];
    let msgs = convert("", &items, true);
    assert_eq!(msgs[0]["role"], "system", "the head stays a system message");
    assert_eq!(
        msgs[1]["role"], "system",
        "an interior update stays one too"
    );
    assert_eq!(
        msgs.last().unwrap()["role"],
        "user",
        "and the request already ended on a user turn"
    );
}

/// An offer row is for the operator's eyes: the request is the same with it anywhere in the
/// history, even between a call and its result.
#[test]
fn an_offer_row_is_not_in_the_request() {
    let offer = TranscriptItem::Offer {
        kind: letibot_transcript::OfferKind::Skill,
        title: "skill offered: x (0.40)".into(),
        body: "the whole skill".into(),
    };
    let call = TranscriptItem::Assistant {
        text: String::new(),
        tool_calls: vec![ToolCall {
            id: "c1".into(),
            name: "glob".into(),
            arguments: "{}".into(),
        }],
        truncated: false,
    };
    let result = TranscriptItem::ToolResult {
        call_id: "c1".into(),
        name: "glob".into(),
        outcome: ToolOutcome::Ok,
        payload: "a".into(),
        edits: Vec::new(),
        origin: None,
        media: None,
    };
    let user = TranscriptItem::User {
        speaker: Default::default(),
        parts: vec![UserPart::Text { text: "go".into() }],
    };
    let plain = vec![user.clone(), call.clone(), result.clone()];
    let with = vec![offer.clone(), user, call, offer.clone(), result, offer];
    assert_eq!(
        convert("s", &plain, true),
        convert("s", &with, true),
        "an offer row reached the request"
    );
}

#[test]
fn items_become_the_apis_messages_without_tools() {
    // Reasoning stays home only when the request carries no `tools`; with tools it must be
    // passed back. A test named for a rule that has changed is the same defect as a comment
    // that has, one layer in - and this is the test that passed while the head wedged.
    let items = vec![
        TranscriptItem::System {
            text: "be terse".into(),
            origin: SystemOrigin::Bootstrap,
        },
        TranscriptItem::User {
            speaker: Default::default(),
            parts: vec![UserPart::Text {
                text: "list the crates".into(),
            }],
        },
        TranscriptItem::Reasoning {
            text: "thinking…".into(),
            field: letibot_transcript::ReasoningField::ReasoningContent,
            truncated: false,
        },
        TranscriptItem::Assistant {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: "c1".into(),
                name: "glob".into(),
                arguments: "{\"pattern\":\"crates/*\"}".into(),
            }],
            truncated: false,
        },
        TranscriptItem::ToolResult {
            call_id: "c1".into(),
            name: "glob".into(),
            outcome: ToolOutcome::Failed {
                reason: "no such dir".into(),
            },
            payload: "nothing".into(),
            edits: Vec::new(),
            origin: None,
            media: None,
        },
    ];
    let m = convert("be terse", &items, false);
    assert_eq!(m.len(), 4, "{m:?}");
    assert_eq!(m[0]["role"], "system");
    assert_eq!(m[1]["content"], "list the crates");
    assert!(m[2]["content"].is_null());
    assert_eq!(m[2]["tool_calls"][0]["function"]["name"], "glob");
    assert_eq!(m[3]["role"], "tool");
    assert_eq!(m[3]["tool_call_id"], "c1");
    // The payload is sent as it was rendered: `ToolResult::render` already names the
    // outcome, and a second prefix said it twice (R93.F35).
    assert_eq!(m[3]["content"], "nothing");
    // A system prompt with no leading System item is prepended once.
    let m2 = convert("sys", &items[1..2], false);
    assert_eq!(m2[0]["role"], "system");
    assert_eq!(m2.len(), 2);
}

/// A transcript written before ids were minted holds the positional `call_0` in every turn.
/// That history still replays: each turn's call and its result survive with the ids they were
/// stored under - the pairing window is the assistant message, not a session-wide id index, so a
/// repeated id must not orphan a result or rewrite a call.
#[test]
fn a_stored_transcript_with_positional_ids_still_replays() {
    let turn = |text: &str, out: &str| {
        vec![
            TranscriptItem::User {
                speaker: Default::default(),
                parts: vec![UserPart::Text { text: text.into() }],
            },
            TranscriptItem::Assistant {
                text: String::new(),
                tool_calls: vec![ToolCall {
                    id: "call_0".into(),
                    name: "glob".into(),
                    arguments: "{}".into(),
                }],
                truncated: false,
            },
            TranscriptItem::ToolResult {
                call_id: "call_0".into(),
                name: "glob".into(),
                outcome: ToolOutcome::Ok,
                payload: out.into(),
                edits: Vec::new(),
                origin: None,
                media: None,
            },
        ]
    };
    let mut items = turn("first", "one");
    items.extend(turn("second", "two"));
    let m = convert("be terse", &items, false);
    let calls: Vec<&Value> = m
        .iter()
        .filter_map(|r| r.get("tool_calls").and_then(|c| c.as_array()))
        .flatten()
        .collect();
    assert_eq!(calls.len(), 2, "both turns' calls: {m:?}");
    assert!(calls.iter().all(|c| c["id"] == "call_0"), "{calls:?}");
    let results: Vec<&Value> = m.iter().filter(|r| r["role"] == "tool").collect();
    assert_eq!(
        results.len(),
        2,
        "no result was dropped as an orphan: {m:?}"
    );
    assert!(results.iter().all(|r| r["tool_call_id"] == "call_0"));
    assert_eq!(results[0]["content"], "one");
    assert_eq!(results[1]["content"], "two");
}

/// A picture a call read comes out as a picture, on this backend: a `messages` provider does
/// not take media inside a tool message, so the tool result carries its text and the image
/// follows as an ordinary user message.
#[test]
fn a_tool_result_with_an_image_puts_the_image_on_the_wire() {
    let uri = picture_ref();
    // The row that proposed it, because `pair_tool_calls` drops an orphan tool result by
    // design - the fixture needs the shape the wire actually has.
    let proposed = TranscriptItem::Assistant {
        text: String::new(),
        tool_calls: vec![letibot_transcript::ToolCall {
            id: "c1".into(),
            name: "read".into(),
            arguments: "{}".into(),
        }],
        truncated: false,
    };
    let items = vec![
        proposed,
        TranscriptItem::ToolResult {
            call_id: "c1".into(),
            name: "read".into(),
            outcome: ToolOutcome::Ok,
            payload: "shot.png — image image/png 2×2 · 1 KiB".into(),
            edits: Vec::new(),
            origin: None,
            media: Some(letibot_transcript::media::Media {
                mime: "image/png".into(),
                bytes: 11,
                width: Some(2),
                height: Some(2),
                data_ref: uri.clone(),
                delivered: false,
            }),
        },
    ];
    let m = convert("be terse", &items, false);
    let tool = m
        .iter()
        .find(|x| x["role"] == "tool")
        .expect("the tool result");
    assert_eq!(tool["content"], "shot.png — image image/png 2×2 · 1 KiB");
    let att = m
        .iter()
        .find(|x| x["role"] == "user")
        .expect("the attachment did not reach the wire at all");
    assert_eq!(
        att["content"][0]["text"],
        "Attached media from tool result:"
    );
    assert_eq!(att["content"][1]["type"], "image_url");
    assert_eq!(att["content"][1]["image_url"]["url"], uri);
}

/// And a result with no image keeps its string content: the bytes are the prompt PREFIX, and a
/// plain tool result coming out as an array would re-prefill every cached turn.
#[test]
fn a_tool_result_without_an_image_sends_no_attachment_and_keeps_its_string() {
    // The row that proposed it, because `pair_tool_calls` drops an orphan tool result by
    // design - the fixture needs the shape the wire actually has.
    let proposed = TranscriptItem::Assistant {
        text: String::new(),
        tool_calls: vec![letibot_transcript::ToolCall {
            id: "c1".into(),
            name: "read".into(),
            arguments: "{}".into(),
        }],
        truncated: false,
    };
    let items = vec![
        proposed,
        TranscriptItem::ToolResult {
            call_id: "c1".into(),
            name: "bash".into(),
            outcome: ToolOutcome::Ok,
            payload: "ok".into(),
            edits: Vec::new(),
            origin: None,
            media: None,
        },
    ];
    let m = convert("be terse", &items, false);
    assert_eq!(m.iter().filter(|x| x["role"] == "user").count(), 0, "{m:?}");
    assert!(
        m.iter().find(|x| x["role"] == "tool").unwrap()["content"].is_string(),
        "a text result changed shape: {m:?}"
    );
}

/// A user row carrying an image sends it, rather than a sentence saying one was attached.
#[test]
fn a_user_row_carrying_an_image_sends_the_image_not_a_sentence_about_it() {
    let uri = picture_ref();
    let items = vec![TranscriptItem::User {
        speaker: letibot_transcript::Speaker::Operator,
        parts: vec![
            UserPart::Text {
                text: "what is this?".into(),
            },
            UserPart::Image {
                media_type: "image/png".into(),
                data_ref: uri.clone(),
            },
        ],
    }];
    let m = convert("be terse", &items, false);
    let user = m
        .iter()
        .find(|x| x["role"] == "user")
        .expect("the user message");
    assert_eq!(user["content"][0]["text"], "what is this?");
    assert_eq!(user["content"][1]["image_url"]["url"], uri);
    assert!(
        !m.iter()
            .any(|x| x.to_string().contains("not sent to this provider")),
        "the placeholder is still on the wire: {m:?}"
    );
}

/// A model the catalogue does not carry is SENT the picture, not a sentence about it: only a
/// catalogue saying `Some(false)`, or a provider that refused one, turns pictures off. A
/// harness that withholds them has turned every attached chart into prose about a chart.
#[test]
fn an_unknown_model_is_sent_the_picture() {
    let uri = picture_ref();
    let proposed = TranscriptItem::Assistant {
        text: String::new(),
        tool_calls: vec![letibot_transcript::ToolCall {
            id: "c1".into(),
            name: "read".into(),
            arguments: "{}".into(),
        }],
        truncated: false,
    };
    let items = vec![
        TranscriptItem::User {
            speaker: letibot_transcript::Speaker::Operator,
            parts: vec![
                UserPart::Text {
                    text: "what is this?".into(),
                },
                UserPart::Image {
                    media_type: "image/png".into(),
                    data_ref: uri.clone(),
                },
            ],
        },
        proposed,
        TranscriptItem::ToolResult {
            call_id: "c1".into(),
            name: "read".into(),
            outcome: ToolOutcome::Ok,
            payload: "shot.png — image image/png 2×2 · 1 KiB".into(),
            edits: Vec::new(),
            origin: None,
            media: Some(letibot_transcript::media::Media {
                mime: "image/png".into(),
                bytes: 11,
                width: Some(2),
                height: Some(2),
                data_ref: uri.clone(),
                delivered: false,
            }),
        },
    ];
    let m = super::convert(
        "be terse",
        &items,
        false,
        Eyes::Unknown("some-new-model"),
        Withheld::nothing(),
    );
    // Both carriers: the operator's own part, and the picture a call read.
    let urls = m
        .iter()
        .filter(|x| x.to_string().contains("image_url"))
        .count();
    assert_eq!(urls, 2, "one picture is missing from the wire: {m:?}");
    assert!(
        !m.iter().any(|x| x.to_string().contains("not sent:")),
        "a model nobody has ruled out was sent a placeholder: {m:?}"
    );
}

/// The refusal that means "not this kind of content", and the request it applies to: the
/// sentence is about a content type or names `image_url`, and the request carried a picture.
/// Every vendor says the same fact in its own words, so the table covers both directions.
#[test]
fn only_a_content_type_refusal_on_a_request_with_a_picture_is_this_refusal() {
    for said in [
        // Wordings seen from `glm-5.3` on the coding plan, then others.
        "messages.content.type is invalid, allowed values: ['text']",
        "Invalid 'messages[1].content[2].type': 'image_url' is not supported",
        "unknown variant `image_url`, expected `text`",
        "this model does not support image input; content must be text only",
        "content type must be text",
    ] {
        assert!(super::text_only_refusal(said), "not recognised: {said}");
    }
    for said in [
        "Authentication Fails",
        "maximum context length is 1048576 tokens",
        "invalid_request_error: unknown parameter 'temperature'",
        "insufficient balance",
    ] {
        assert!(
            !super::text_only_refusal(said),
            "wrongly recognised: {said}"
        );
    }
    // And the guard: a picture has to be IN the request for the retry to change
    // anything.
    let text_only = [TranscriptItem::User {
        speaker: letibot_transcript::Speaker::Operator,
        parts: vec![UserPart::Text { text: "hi".into() }],
    }];
    assert!(!super::carries_pictures(text_only.iter()));
    let picture = [TranscriptItem::User {
        speaker: letibot_transcript::Speaker::Operator,
        parts: vec![UserPart::Image {
            media_type: "image/png".into(),
            data_ref: picture_ref(),
        }],
    }];
    assert!(super::carries_pictures(picture.iter()));
}

/// A model that takes text only is sent the placeholder, and the message is a string again.
/// The images are in the HISTORY, so the request has to stop carrying them and the model has
/// to be told why. The determinism matters: the same part, the same sentence, every time.
#[test]
fn a_text_only_model_gets_a_placeholder_and_plain_string_content() {
    let uri = picture_ref();
    let items = vec![TranscriptItem::User {
        speaker: letibot_transcript::Speaker::Operator,
        parts: vec![
            UserPart::Text {
                text: "what is this?".into(),
            },
            UserPart::Image {
                media_type: "image/png".into(),
                data_ref: uri,
            },
        ],
    }];
    let m = super::convert(
        "be terse",
        &items,
        false,
        Eyes::TextOnly("glm-5.3"),
        Withheld::nothing(),
    );
    let user = m
        .iter()
        .find(|x| x["role"] == "user")
        .expect("the user message");
    // A plain string, not an array of one part. An array where a string was is a
    // re-prefill of every cached turn in the conversation.
    let content = user["content"]
        .as_str()
        .unwrap_or_else(|| panic!("an all-text message came out as {user}"));
    let size = letibot_transcript::testing::png(2, 2).len();
    assert_eq!(
        content,
        format!(
            "what is this?\n\n[image 1 · image/png · {size} B — not sent: glm-5.3 takes \
             text only]"
        ),
        "{content}"
    );
    assert!(
        !m.iter().any(|x| x.to_string().contains("image_url")),
        "an image_url reached a text-only model: {m:?}"
    );
    // And the same bytes twice: the prefix has to be stable per model.
    assert_eq!(
        m,
        super::convert(
            "be terse",
            &items,
            false,
            Eyes::TextOnly("glm-5.3"),
            Withheld::nothing(),
        )
    );
}

/// An image a tool result read is a plain-string message when the model cannot see it.
#[test]
fn a_tool_results_image_becomes_a_placeholder_message_for_a_text_only_model() {
    let proposed = TranscriptItem::Assistant {
        text: String::new(),
        tool_calls: vec![letibot_transcript::ToolCall {
            id: "c1".into(),
            name: "read".into(),
            arguments: "{}".into(),
        }],
        truncated: false,
    };
    let items = vec![
        proposed,
        TranscriptItem::ToolResult {
            call_id: "c1".into(),
            name: "read".into(),
            outcome: ToolOutcome::Ok,
            payload: "shot.png — image image/png 2×2 · 1 KiB".into(),
            edits: Vec::new(),
            origin: None,
            media: Some(letibot_transcript::media::Media {
                mime: "image/png".into(),
                bytes: 131_072,
                width: Some(2),
                height: Some(2),
                data_ref: picture_ref(),
                delivered: false,
            }),
        },
    ];
    let m = super::convert(
        "be terse",
        &items,
        false,
        Eyes::TextOnly("glm-5.3"),
        Withheld::nothing(),
    );
    let att = m
        .iter()
        .find(|x| x["role"] == "user")
        .expect("the attachment notice");
    assert_eq!(
        att["content"],
        "Attached media from tool result: [image 1 · image/png · 128 KiB — not sent: \
         glm-5.3 takes text only]",
        "{att}"
    );
    assert!(
        !m.iter().any(|x| x.to_string().contains("image_url")),
        "an image_url reached a text-only model: {m:?}"
    );
}

/// A picture in the STORE that is not a picture is named, not sent, on the user-row carrier.
/// The row is append-only, so the renderer is the only place the mistake can be undone and a
/// fixed binary can reopen the session a broken one wedged.
#[test]
fn a_user_rows_incomplete_picture_is_named_and_not_sent() {
    let bad = letibot_transcript::testing::data_ref(
        "image/png",
        &letibot_transcript::testing::png_header_only(640, 480),
    );
    let items = vec![TranscriptItem::User {
        speaker: letibot_transcript::Speaker::Operator,
        parts: vec![
            UserPart::Text {
                text: "what is this?".into(),
            },
            UserPart::Image {
                media_type: "image/png".into(),
                data_ref: bad,
            },
        ],
    }];
    let m = convert("be terse", &items, false);
    assert!(
        !m.iter().any(|x| x.to_string().contains("image_url")),
        "an incomplete picture reached the wire: {m:?}"
    );
    assert!(
        m.iter().any(|x| x.to_string().contains(&format!(
            "[image 1 · image/png · 33 B — not sent: {}]",
            super::NOT_A_PICTURE
        ))),
        "the model was not told why it has no picture: {m:?}"
    );
}

/// And the same for a tool result's media.
#[test]
fn a_tool_results_incomplete_picture_is_named_and_not_sent() {
    let proposed = TranscriptItem::Assistant {
        text: String::new(),
        tool_calls: vec![letibot_transcript::ToolCall {
            id: "c1".into(),
            name: "read".into(),
            arguments: "{}".into(),
        }],
        truncated: false,
    };
    let items = vec![
        proposed,
        TranscriptItem::ToolResult {
            call_id: "c1".into(),
            name: "read".into(),
            outcome: ToolOutcome::Ok,
            payload: "shot.png — image image/png 640×480 · 0 KiB".into(),
            edits: Vec::new(),
            origin: None,
            media: Some(letibot_transcript::media::Media {
                mime: "image/png".into(),
                bytes: 33,
                width: Some(640),
                height: Some(480),
                data_ref: letibot_transcript::testing::data_ref(
                    "image/png",
                    &letibot_transcript::testing::png_header_only(640, 480),
                ),
                delivered: false,
            }),
        },
    ];
    let m = convert("be terse", &items, false);
    assert!(
        !m.iter().any(|x| x.to_string().contains("image_url")),
        "an incomplete picture reached the wire: {m:?}"
    );
    assert!(
        m.iter().any(|x| x.to_string().contains(&format!(
            "Attached media from tool result: [image 1 · image/png · 33 B — not sent: {}]",
            super::NOT_A_PICTURE
        ))),
        "the model was not told why it has no picture: {m:?}"
    );
}

/// A retry withholds what it is TRYING without, and records nothing: a retry that fails proves
/// nothing about the pictures, so the caller commits the lesson only once it answers.
#[test]
fn a_retry_withholds_a_picture_it_is_only_trying_and_records_nothing() {
    let uri =
        letibot_transcript::testing::data_ref("image/png", &letibot_transcript::testing::png(6, 6));
    let items = vec![TranscriptItem::User {
        speaker: letibot_transcript::Speaker::Operator,
        parts: vec![UserPart::Image {
            media_type: "image/png".into(),
            data_ref: uri.clone(),
        }],
    }];
    let trying = vec![(
        super::picture_key(&uri),
        "these bytes are not an image".to_string(),
    )];
    let m = super::convert(
        "be terse",
        &items,
        false,
        Eyes::Sees,
        super::Withheld::trying(&trying),
    );
    assert!(
        !m.iter().any(|x| x.to_string().contains("image_url")),
        "the picture was sent again: {m:?}"
    );
    assert!(
        m.iter().any(|x| x.to_string().contains(
            "— not sent: the provider refused these bytes: these bytes are not an image]"
        )),
        "the placeholder does not name the reason: {m:?}"
    );
    assert!(
        super::known_bad_picture(&uri).is_none(),
        "a picture that was only TRIED is marked for the rest of the daemon"
    );
    // The same request without the trial sends it, which is what "nothing was recorded" means.
    assert!(
        convert("be terse", &items, false)
            .iter()
            .any(|x| x.to_string().contains("image_url")),
        "the picture stopped going out although nothing was recorded"
    );
}

/// The refusal that is about the BYTES, and the ones that are not. Answering it with a
/// text-only mark would withhold every picture the model was ever sent, for a fault one file
/// had.
#[test]
fn a_refusal_about_the_bytes_is_not_a_refusal_about_the_model() {
    const MEASURED: &str = ".messages[1861].image[0]: You have uploaded an unsupported image. \
                            Please make sure your image is valid and has one of the following \
                            formats: webp, png, jpeg, and gif.";
    for said in [
        MEASURED,
        "Invalid image: could not decode",
        "the picture is corrupt",
        "media format unsupported",
        "Unable to decode image data",
    ] {
        assert!(super::picture_refusal(said), "not recognised: {said}");
    }
    assert!(
        !super::text_only_refusal(MEASURED),
        "the bytes refusal is being read as a text-only refusal"
    );
    for said in [
        // A model that takes text only says a different thing, and `complete` asks that
        // predicate FIRST - so the overlap on the word `image_url` resolves the right way.
        "messages.content.type is invalid, allowed values: ['text']",
        "Authentication Fails",
        "maximum context length is 1048576 tokens",
        "An assistant message with 'tool_calls' must be followed by tool messages",
        "insufficient balance",
    ] {
        assert!(
            !super::picture_refusal(said),
            "wrongly recognised as a picture refusal: {said}"
        );
    }
}

#[test]
fn tool_schemas_pass_through_and_a_bare_string_is_refused() {
    let ok = tools(&[r#"{"type":"function","function":{"name":"read","parameters":{}}}"#.into()])
        .unwrap();
    assert_eq!(ok[0]["function"]["name"], "read");
    let bare = tools(&[r#"{"name":"read","description":"","parameters":{}}"#.into()]).unwrap();
    assert_eq!(bare[0]["function"]["name"], "read");
    assert_eq!(bare[0]["type"], "function");
    assert!(tools(&["\"read\"".into()]).unwrap_err().contains("neither"));
}

#[test]
fn a_long_multibyte_schema_is_refused_without_a_panic() {
    let wide = format!("\"{}\"", "\u{3042}".repeat(60));
    assert!(tools(&[wide]).unwrap_err().contains("neither"));
}

/// A command that printed nothing is sent as words, never as an empty string: Kimi's server
/// refuses a message whose text is empty, and the whole conversation with it.
#[test]
fn an_empty_tool_result_is_sent_as_no_output() {
    let items = vec![
        TranscriptItem::User {
            speaker: Default::default(),
            parts: vec![UserPart::Text {
                text: "run it".into(),
            }],
        },
        TranscriptItem::Assistant {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: "c1".into(),
                name: "bash".into(),
                arguments: "{}".into(),
            }],
            truncated: false,
        },
        TranscriptItem::ToolResult {
            call_id: "c1".into(),
            name: "bash".into(),
            outcome: ToolOutcome::Ok,
            payload: String::new(),
            edits: Vec::new(),
            origin: None,
            media: None,
        },
    ];
    let m = convert("", &items, false);
    let tool = m.iter().find(|x| x["role"] == "tool").expect("the result");
    assert_eq!(tool["content"], "(no output)");
}
