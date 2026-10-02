//! Transcript items → the API's messages.
//!
//! The rules, each one a documented provider behaviour rather than taste:
//!
//! - `System` → `{"role":"system"}`. Every system item, in order; a system
//!   update mid-session is a second system message, which every provider
//!   accepts.
//! - `User` → `{"role":"user","content":"…"}` with the text parts joined by a
//!   blank line. Images are not sent (none of the three is asked to see one
//!   here) and are named in the text so the model knows something was there.
//! - `Reasoning` → attached to the assistant message it belongs to, as
//!   `reasoning_content`, **when the request carries `tools`.** See below: this is
//!   a REQUEST-level rule, not a provider one, and getting it wrong wedged a head.
//! - `Assistant` → `{"role":"assistant","content":…,"tool_calls":[…]}`.
//!   `content` is `null` when there is no text and there are calls, which is
//!   the shape the APIs emit and the one they accept back.
//! - `ToolResult` → `{"role":"tool","tool_call_id":…,"content":…}`. The outcome
//!   is folded into the content the way the local dialects do it: an `ok`
//!   result is the payload; anything else is prefixed with what happened, so
//!   the model can tell a refusal from an answer.
//! - `SegmentMark` → dropped. It is the store's, not the model's.
//!
//! # `reasoning_content`, which this file has now been wrong about twice
//!
//! **First it said the opposite.** *"DeepSeek documents that `reasoning_content` from
//! earlier turns must not be sent back; GLM and Grok ignore it."* That was accurate
//! about `deepseek-reasoner` and wrong about the model actually in use, and the live
//! API said so — MEASURED in `~/logs/harnessd.log`, four times, on a head at 940k of
//! 999k tokens:
//!
//! ```text
//! compaction FAILED: http 400: The `reasoning_content` in the thinking mode must
//! be passed back to the API. (request_id: 5f6e40f5-…)
//! ```
//!
//! **Then it asserted a rule the API does not enforce.** The correction came from
//! DeepSeek's thinking-mode guide, and it was quoted here as the API's behaviour:
//!
//! > In subsequent requests, whether `reasoning_content` should be passed back …
//! > depends on whether the request carries the `tools` parameter: *carries `tools`*
//! > — passed back; *does not carry `tools`* — not needed, and ignored if passed.
//!
//! MEASURED against the live API 2026-10-02 (`deepseek-flash`). **All five answer
//! 200:**
//!
//! ```text
//! no tools, assistant without reasoning_content                 200
//! tools,    assistant without reasoning_content                 200
//! tools,    assistant with tool_calls, no reasoning_content     200
//! tools,    assistant with tool_calls, with reasoning_content   200
//! no tools, assistant with tool_calls, no reasoning_content     200
//! ```
//!
//! So `tools` neither requires the echo nor forbids it, and **omitting
//! `reasoning_content` does not produce a 400 in any of those shapes.** What follows
//! from that is the important part: [`convert`]'s `echo_reasoning` is **this
//! harness's own switch for how much to send, not a contract being obeyed.** It is
//! still worth having — it keeps half a million tokens of thinking off a request that
//! does not need them — but a choice is not evidence about the API, and it was
//! mistaken for evidence here for a day.
//!
//! # What is still unexplained, said rather than guessed at
//!
//! `The reasoning_content in the thinking mode must be passed back to the API` is a
//! refusal this harness really has received, and **nothing in this file explains its
//! trigger.** Both explanations it has offered are disproven: it is not "we dropped
//! them while carrying tools" — a compaction carrying **no** tools got exactly that
//! 400 on 2026-10-02 — and it is not the `tools` parameter at all, per the table
//! above.
//!
//! What is known is only the company this refusal has kept: it arrives on requests whose
//! conversation is large, alongside a size complaint. Whether the cause is a message
//! ordering, a field present-and-empty rather than absent, or something else is **not
//! known**, and it will not be inferred from documentation that has already misled this
//! file twice. The size, which was the last obvious answer, is measured away below.
//!
//! **And the size is refuted too, MEASURED 2026-10-02 against the live API.** That theory
//! was the last one standing and it was the most attractive, because it explained the
//! refusals without needing any structural anomaly: two clean requests, no tools, one
//! user message of prose, nothing else:
//!
//! ```text
//! 976,097 provider tokens   200   finish: length (16 of 16 output tokens, all reasoning)
//! 1,248,039 provider tokens 400   "maximum context length is 1048576 tokens. However, you
//!                                  requested 1248055 tokens (1248039 in the messages, 16 in
//!                                  the completion)"  — in 5.6 s
//! ```
//!
//! So a structurally clean request past the window gets the honest complaint, naming exact
//! counts, and gets it in seconds. Two further facts from the log, read off the OUTCOME
//! SIZES rather than from adjacency — a result of ~9-20k tokens is the two-half overrun
//! (everything summarised, nothing kept verbatim) and a result near half the session is
//! `letibot-turn`'s fold (summary plus the second half verbatim):
//!
//!   * the refusals at 938,613 / 937,942 / 940,211 ledger tokens were the two-half plan,
//!     whose requests are the conversation itself; the successful compactions at 940,451
//!     and 940,610 were the same plan. Same size, same plan, both outcomes.
//!   * 1,488,795 FAILED and 1,489,461 SUCCEEDED — the fold, at the same size, whose
//!     summary request is only the FIRST half. One 400 and one 200.
//!
//! Both refusing requests were comfortably inside the window. The trigger is still unknown;
//! what has changed is that three explanations have been measured away, and the next
//! occurrence has the shape log below to be read off instead of argued about.
//!
//! So a refused request logs its own SHAPE — roles in order, which fields each
//! message carries, and their lengths, never the content — from
//! `OpenAiProvider::complete`, at the moment of the refusal. The next occurrence is
//! read off the record instead of argued from a hypothesis.
//!
//! # The failure shape, which is why no test caught it
//!
//! **It is compaction that fails, and compaction is the remedy.** A head sits at
//! 94% of its context; the call that would free context is the one being rejected;
//! so the head cannot recover on its own, and the operator's only move is to stop
//! the daemon and reopen the session. Short turns keep working, so nothing looks
//! wrong until a conversation has run for hours — which is why this arrived as a
//! stuck head rather than as a failing test.
//!
//! And the reason it could not fail as a test: **nothing asserted the request
//! SHAPE.** The code could not tell "we dropped it and the model did not mind" from
//! "we dropped it and the next long session will 400", so there was no assertion to
//! go red. The tests added below are about the request, not about a symptom.

use letibot_transcript::{ToolOutcome, TranscriptItem, UserPart};
use serde_json::{Value, json};

/// The `messages` array. `system` is prepended when non-empty and no `System`
/// item leads the transcript — the harness keeps the system prompt in item 0,
/// so normally it is already there and `system` is the same text.
///
/// `echo_reasoning` is **this harness's own choice about how much to send**, not a
/// provider rule being obeyed: the module header carries the five live-API
/// permutations that disprove the vendor's `tools` condition. The caller computes it
/// — `OpenAiProvider::body`, which knows whether it is about to send `tools` — so the
/// decision is made once, where the request is assembled, rather than guessed at here
/// from a provider name.
pub fn convert(system: &str, items: &[TranscriptItem], echo_reasoning: bool) -> Vec<Value> {
    let mut out = Vec::with_capacity(items.len() + 1);
    let leads_with_system = matches!(items.first(), Some(TranscriptItem::System { .. }));
    if !system.trim().is_empty() && !leads_with_system {
        out.push(json!({"role": "system", "content": system}));
    }
    // **The open assistant turn's reasoning, waiting for the assistant it belongs
    // to.** The transcript commits reasoning as it STREAMS and the assistant when it
    // completes, so a run of `Reasoning` items is followed by its `Assistant` —
    // MEASURED against the real transcript of the session that wedged: 9,272
    // reasoning rows, and the run-length pattern is `reasoning×7 assistant×1`. That
    // is the same shape `dialect-glm`'s renderer accumulates
    // (`turn: Option<(Option<String>, String, Vec<ToolCall>)>`), which is the tree's
    // existing precedent for this and is validated against the server by the
    // fidelity gate.
    //
    // It is held ACROSS intervening items rather than cleared by them: a steering
    // message can land between reasoning and its answer, and dropping the reasoning
    // at that point is exactly the 400 this exists to prevent. The API's requirement
    // is that it be present; folding it forward keeps it present.
    let mut pending_reasoning: Option<String> = None;
    for item in items {
        match item {
            TranscriptItem::System { text, .. } => {
                out.push(json!({"role": "system", "content": text}));
            }
            TranscriptItem::User { parts, .. } => {
                // **An image part is SENT, not described.**
                //
                // This arm used to render every image as *"[an image (image/png) was attached here
                // and is not sent to this provider]"* — a sentence that was true only because
                // nothing in the tree could produce an image part. The moment `read` could, the
                // sentence became the defect it described: a model told a picture exists and given
                // no picture is a model that answers as though the picture were uninteresting,
                // which is the failure R54 §7's third clause is about.
                //
                // The shape is opencode's and the OpenAI-compatible one: a content ARRAY of typed
                // parts, the image as `{"type":"image_url","image_url":{"url":"data:…"}}`. A
                // text-only message keeps the plain string, so nothing about an ordinary turn
                // changes — which matters beyond tidiness: these bytes are the prompt prefix, and
                // an array where a string was would re-prefill every cached turn.
                let mut content: Vec<Value> = Vec::new();
                for p in parts {
                    match p {
                        UserPart::Text { text } => {
                            content.push(json!({"type": "text", "text": text}))
                        }
                        UserPart::Image { data_ref, .. } => content
                            .push(json!({"type": "image_url", "image_url": {"url": data_ref}})),
                        UserPart::FileRef { path, .. } => {
                            content.push(json!({"type": "text", "text": path}))
                        }
                    }
                }
                let only_text = content
                    .iter()
                    .all(|p| p.get("type").and_then(|t| t.as_str()) == Some("text"));
                if only_text {
                    let text = content
                        .iter()
                        .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    out.push(json!({"role": "user", "content": text}));
                } else {
                    out.push(json!({"role": "user", "content": Value::Array(content)}));
                }
            }
            TranscriptItem::Reasoning { text, .. } => {
                if echo_reasoning && !text.is_empty() {
                    match &mut pending_reasoning {
                        // Joined, not replaced: a truncated reasoning row and its
                        // continuation are two items and one thought.
                        Some(prev) => {
                            prev.push_str("\n\n");
                            prev.push_str(text);
                        }
                        None => pending_reasoning = Some(text.clone()),
                    }
                }
            }
            TranscriptItem::SegmentMark { .. } => {}
            TranscriptItem::Assistant {
                text, tool_calls, ..
            } => {
                let mut m = json!({"role": "assistant"});
                // The reasoning this assistant produced, if the request must carry
                // it. Set BEFORE `content` so the field order reads the way the API
                // documents it, though order is not what it checks.
                if let Some(r) = pending_reasoning.take() {
                    m["reasoning_content"] = Value::String(r);
                }
                if text.is_empty() && !tool_calls.is_empty() {
                    m["content"] = Value::Null;
                } else {
                    m["content"] = json!(text);
                }
                if !tool_calls.is_empty() {
                    m["tool_calls"] = Value::Array(
                        tool_calls
                            .iter()
                            .map(|c| {
                                json!({
                                    "id": c.id,
                                    "type": "function",
                                    "function": {"name": c.name, "arguments": c.arguments}
                                })
                            })
                            .collect(),
                    );
                }
                out.push(m);
            }
            TranscriptItem::ToolResult {
                call_id,
                name,
                outcome,
                payload,
                // Display-only; the prompt never sees it.
                edit: _,
                media,
                ..
            } => {
                let content = match outcome {
                    ToolOutcome::Ok => payload.clone(),
                    other => format!("[{name}: {}]\n{payload}", outcome_word(other)),
                };
                out.push(json!({"role": "tool", "tool_call_id": call_id, "content": content}));
                // **And when the call read a picture, the picture follows as its own message.**
                //
                // This is opencode's shape for a provider that does not take media inside a tool
                // result — its `DEFAULT FALSE` — and the text is its own prompt
                // (`SYNTHETIC_ATTACHMENT_PROMPT`, *"Attached media from tool result:"*) because a
                // user message carrying a picture and no words is a message with nothing to
                // interpret.
                //
                // **Why the fallback and not the tool message here.** The local `llama-server` DOES
                // take media in a tool result — MEASURED, and the model named a green square
                // `Green` — so the dialect renderers use that directly. A `messages` provider is the
                // case opencode answers per provider, and the honest default is the one that works
                // everywhere: an ordinary user message is the one shape every vision API accepts.
                // When a provider is measured to take tool-result media, this is the arm to branch
                // on, and the branch is one `if`.
                if let Some(m) = media {
                    out.push(json!({
                        "role": "user",
                        "content": [
                            {"type": "text", "text": "Attached media from tool result:"},
                            {"type": "image_url", "image_url": {"url": m.data_ref}},
                        ]
                    }));
                }
            }
        }
    }
    pair_tool_calls(out)
}

/// **Every `tool_calls` gets its `tool` messages, or the provider refuses the
/// whole conversation.**
///
/// The OpenAI message shape requires that an assistant message carrying
/// `tool_calls` is followed by one `tool` message per `tool_call_id`. A local
/// dialect does not: it renders a call that never came back as text, and the
/// conversation carries on. So a transcript that is perfectly fine on qwen —
/// a turn interrupted between the call and its result, a call that was never
/// run — is one a provider will not accept at all.
///
/// Measured the first time a long local conversation was pointed at deepseek:
///
/// ```text
/// provider refused (400): An assistant message with 'tool_calls' must be
/// followed by tool messages responding to each 'tool_call_id'.
/// ```
///
/// and every later turn in that session would have failed the same way, because
/// the offending row is history and history does not change.
///
/// So the gap is filled rather than the request being sent to be refused. The
/// synthesised message says what is true — no result was recorded — which is
/// better information than the provider's refusal and better than a fabricated
/// success. A `tool` message whose id answers nothing is dropped for the same
/// reason in reverse: the providers reject those too.
fn pair_tool_calls(rows: Vec<Value>) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::with_capacity(rows.len());
    // Ids opened by the assistant message being answered right now, in order, so
    // the fillers go in the order the calls were made.
    let mut owed: Vec<String> = Vec::new();

    for row in rows {
        let role = row.get("role").and_then(|r| r.as_str()).unwrap_or("");
        if role == "tool" {
            let id = row
                .get("tool_call_id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            // An orphan: no assistant message above it opened this id. Dropped,
            // because a provider refuses it and nothing downstream reads it.
            if let Some(at) = owed.iter().position(|o| *o == id) {
                owed.remove(at);
                out.push(row);
            }
            continue;
        }
        // Any other role closes the answering window, so whatever is still owed
        // is owed forever and is filled here.
        for id in std::mem::take(&mut owed) {
            out.push(unanswered(&id));
        }
        if role == "assistant" {
            owed = row
                .get("tool_calls")
                .and_then(|v| v.as_array())
                .map(|cs| {
                    cs.iter()
                        .filter_map(|c| c.get("id").and_then(|i| i.as_str()))
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
        }
        out.push(row);
    }
    // The transcript can simply end on an unanswered call — an interrupted turn
    // does exactly that — and the provider counts it the same way.
    for id in owed {
        out.push(unanswered(&id));
    }
    out
}

fn unanswered(id: &str) -> Value {
    json!({
        "role": "tool",
        "tool_call_id": id,
        "content": "[no result was recorded for this call: the turn ended before it \
                    finished, or it was never run]",
    })
}

fn outcome_word(o: &ToolOutcome) -> String {
    match o {
        ToolOutcome::Ok => "ok".into(),
        ToolOutcome::Abstained { reason } => format!("abstained — {reason}"),
        ToolOutcome::Failed { reason } => format!("failed — {reason}"),
        ToolOutcome::Denied { req_id } => format!("denied (request {req_id})"),
        ToolOutcome::Timeout => "timed out".into(),
        ToolOutcome::NotRun { why } => format!("not run — {why}"),
        ToolOutcome::Backgrounded { .. } => "backgrounded".into(),
    }
}

/// Tool schemas as the API wants them: `{"type":"function","function":{…}}`.
/// The harness hands over its dialect's rendering, which is either that shape
/// already (Qwen) or the bare `{name, description, parameters}` (GLM strips the
/// wrapper for its template); the bare one is wrapped. Anything else is refused
/// rather than sent as a string the provider would 400 on.
pub fn tools(tools_json: &[String]) -> Result<Vec<Value>, String> {
    tools_json
        .iter()
        .map(|t| {
            let v: Value =
                serde_json::from_str(t).map_err(|e| format!("a tool schema is not JSON: {e}"))?;
            if v.get("function").is_some() {
                return Ok(v);
            }
            if v.get("name").is_some() {
                return Ok(json!({"type": "function", "function": v}));
            }
            Err(format!(
                "a tool schema has neither `function` nor `name`: {}",
                &t[..t.len().min(80)]
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use letibot_transcript::{ReasoningField, SystemOrigin, ToolCall};

    #[test]
    fn items_become_the_apis_messages_without_tools() {
        // **Renamed from `..._and_reasoning_stays_home`, which was the old rule.**
        // Reasoning stays home only when the request carries no `tools`; with tools
        // it must be passed back. A test named for a rule that has changed is the
        // same defect as a comment that has, one layer in — and this one is the
        // test that PASSED while the head wedged.
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
                edit: None,
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
        assert!(
            m[3]["content"]
                .as_str()
                .unwrap()
                .starts_with("[glob: failed — no such dir]")
        );
        // A system prompt with no leading System item is prepended once.
        let m2 = convert("sys", &items[1..2], false);
        assert_eq!(m2[0]["role"], "system");
        assert_eq!(m2.len(), 2);
    }

    /// **A picture a call read comes out as a picture** — the far end of R54, on this backend.
    ///
    /// The local `llama-server` takes media inside a *tool* message (measured: it named a green
    /// square `Green`), and the dialect renderers use that directly. A `messages` provider is the
    /// case opencode answers per provider with `DEFAULT FALSE`, so this arm takes the fallback every
    /// vision API accepts: the tool result carries its text, and the image follows as an ordinary
    /// user message whose text is opencode's own prompt.
    #[test]
    fn a_tool_result_with_an_image_puts_the_image_on_the_wire() {
        let uri = "data:image/png;base64,iVBORw0KGgo=";
        // **The row that proposed it**, because `pair_tool_calls` drops an orphan tool result by
        // design — the fixture needs the shape the wire actually has.
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
                edit: None,
                origin: None,
                media: Some(letibot_transcript::media::Media {
                    mime: "image/png".into(),
                    bytes: 11,
                    width: Some(2),
                    height: Some(2),
                    data_ref: uri.into(),
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

    /// **And a result with no image is unchanged, string content and all.**
    ///
    /// The bytes here are the prompt PREFIX: a plain tool result that started coming out as a
    /// content ARRAY would re-prefill every cached turn in the conversation.
    #[test]
    fn a_tool_result_without_an_image_sends_no_attachment_and_keeps_its_string() {
        // **The row that proposed it**, because `pair_tool_calls` drops an orphan tool result by
        // design — the fixture needs the shape the wire actually has.
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
                edit: None,
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

    /// **A user row carrying an image sends it** — the arm that used to render
    /// *"not sent to this provider"*.
    #[test]
    fn a_user_row_carrying_an_image_sends_the_image_not_a_sentence_about_it() {
        let uri = "data:image/png;base64,iVBORw0KGgo=";
        let items = vec![TranscriptItem::User {
            speaker: letibot_transcript::Speaker::Operator,
            parts: vec![
                UserPart::Text {
                    text: "what is this?".into(),
                },
                UserPart::Image {
                    media_type: "image/png".into(),
                    data_ref: uri.into(),
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

    #[test]
    fn tool_schemas_pass_through_and_a_bare_string_is_refused() {
        let ok =
            tools(&[r#"{"type":"function","function":{"name":"read","parameters":{}}}"#.into()])
                .unwrap();
        assert_eq!(ok[0]["function"]["name"], "read");
        let bare = tools(&[r#"{"name":"read","description":"","parameters":{}}"#.into()]).unwrap();
        assert_eq!(bare[0]["function"]["name"], "read");
        assert_eq!(bare[0]["type"], "function");
        assert!(tools(&["\"read\"".into()]).unwrap_err().contains("neither"));
    }
}

#[cfg(test)]
mod pairing_tests {
    use super::*;
    use letibot_transcript::{ToolCall, TranscriptItem, UserPart};

    fn user(t: &str) -> TranscriptItem {
        TranscriptItem::User {
            speaker: Default::default(),
            parts: vec![UserPart::Text { text: t.into() }],
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
            edit: None,
            origin: None,
            media: None,
        }
    }

    /// Every id an assistant message opens is answered by the time the next
    /// non-tool message starts. This is the provider's own rule, asserted over
    /// whatever `messages` produced.
    fn assert_paired(rows: &[Value]) {
        let mut owed: Vec<String> = Vec::new();
        for r in rows {
            let role = r["role"].as_str().unwrap_or("");
            if role == "tool" {
                let id = r["tool_call_id"].as_str().unwrap_or("").to_string();
                let at = owed
                    .iter()
                    .position(|o| *o == id)
                    .unwrap_or_else(|| panic!("a tool message answers nothing: {id}"));
                owed.remove(at);
                continue;
            }
            assert!(owed.is_empty(), "unanswered tool_call_id(s): {owed:?}");
            if let Some(cs) = r.get("tool_calls").and_then(|v| v.as_array()) {
                owed = cs
                    .iter()
                    .map(|c| c["id"].as_str().unwrap_or("").to_string())
                    .collect();
            }
        }
        assert!(owed.is_empty(), "unanswered at the end: {owed:?}");
    }

    /// **The 400 this exists for.** A turn interrupted between the call and its
    /// result leaves an assistant row with `tool_calls` and no `tool` message
    /// after it. A local dialect renders that as text and carries on; a provider
    /// refuses the whole conversation:
    ///
    /// > An assistant message with 'tool_calls' must be followed by tool messages
    /// > responding to each 'tool_call_id'.
    ///
    /// And the row is history, so every later turn in that session fails the same
    /// way — which is what made it worth filling rather than reporting.
    #[test]
    fn an_interrupted_call_is_answered_rather_than_left_dangling() {
        let m = convert(
            "sys",
            &[user("go"), calls(&["c1"]), user("never mind, carry on")],
            false,
        );
        assert_paired(&m);
        let filler = m
            .iter()
            .find(|r| r["role"] == "tool")
            .expect("the gap was filled");
        assert_eq!(filler["tool_call_id"], "c1");
        assert!(
            filler["content"]
                .as_str()
                .unwrap()
                .contains("no result was recorded"),
            "it says what is true rather than faking a success: {filler}"
        );
        // And it goes BEFORE the user turn that followed, not at the end.
        let at = m.iter().position(|r| r["role"] == "tool").unwrap();
        let next = m
            .iter()
            .position(|r| r["content"] == "never mind, carry on")
            .unwrap();
        assert!(at < next, "the filler answers the call it belongs to");
    }

    /// A transcript can simply END on an unanswered call — an interrupted turn
    /// does exactly that — and the provider counts it the same way.
    #[test]
    fn a_transcript_that_ends_mid_call_is_still_valid() {
        let m = convert("sys", &[user("go"), calls(&["c1", "c2"])], false);
        assert_paired(&m);
        assert_eq!(m.iter().filter(|r| r["role"] == "tool").count(), 2);
    }

    /// Some answered, some not: only the gaps are filled, and the real results
    /// keep their own content.
    #[test]
    fn only_the_missing_half_of_a_fan_out_is_filled() {
        let m = convert(
            "sys",
            &[
                user("go"),
                calls(&["c1", "c2", "c3"]),
                result("c2"),
                user("next"),
            ],
            false,
        );
        assert_paired(&m);
        let tools: Vec<&Value> = m.iter().filter(|r| r["role"] == "tool").collect();
        assert_eq!(tools.len(), 3);
        let real = tools.iter().find(|r| r["tool_call_id"] == "c2").unwrap();
        assert!(real["content"].as_str().unwrap().contains("done"));
    }

    /// The mirror image: a `tool` message answering nothing. Providers reject
    /// those too, and nothing downstream reads it.
    #[test]
    fn an_orphan_result_is_dropped_rather_than_sent() {
        let m = convert("sys", &[user("go"), result("ghost"), user("next")], false);
        assert_paired(&m);
        assert!(
            !m.iter().any(|r| r["role"] == "tool"),
            "the orphan went: {m:?}"
        );
    }

    /// The ordinary conversation is untouched — this must cost a well-formed
    /// transcript nothing.
    #[test]
    fn a_well_formed_conversation_is_unchanged() {
        let items = [user("go"), calls(&["c1"]), result("c1"), user("thanks")];
        let m = convert("sys", &items, false);
        assert_paired(&m);
        assert_eq!(m.len(), 5, "system + four, nothing added: {m:?}");
        assert!(m[3]["content"].as_str().unwrap().contains("done"));
    }

    // --- `reasoning_content`, the contract this file used to get backwards -------

    /// **A request carrying `tools` must pass every previous turn's reasoning back.**
    ///
    /// This is the assertion whose absence let a head wedge at 940k of 999k tokens:
    /// nothing checked the request SHAPE against the documented rule, so the code
    /// could not tell "we dropped it and the model did not mind" from "we dropped it
    /// and the next long session will 400". It fails on the old behaviour, which
    /// dropped the item unconditionally.
    #[test]
    fn a_request_with_tools_carries_each_turns_reasoning_on_its_assistant() {
        let items = vec![
            user("what is the date?"),
            TranscriptItem::Reasoning {
                text: "I should call get_date first.".into(),
                field: letibot_transcript::ReasoningField::ReasoningContent,
                truncated: false,
            },
            calls(&["c1"]),
            result("c1"),
            TranscriptItem::Reasoning {
                text: "Now I can answer.".into(),
                field: letibot_transcript::ReasoningField::ReasoningContent,
                truncated: false,
            },
            TranscriptItem::Assistant {
                text: "It is the 19th.".into(),
                tool_calls: vec![],
                truncated: false,
            },
        ];
        let m = convert("sys", &items, true);
        let assistants: Vec<&Value> = m.iter().filter(|r| r["role"] == "assistant").collect();
        assert_eq!(assistants.len(), 2, "two assistant turns: {m:#?}");
        for a in &assistants {
            assert!(
                a["reasoning_content"].is_string(),
                "every assistant whose turn had reasoning must carry it when the \
                 request has tools, or DeepSeek answers 400: {a:#?}"
            );
        }
        assert_eq!(
            assistants[0]["reasoning_content"], "I should call get_date first.",
            "the reasoning attaches to the assistant it belongs to"
        );
        assert_eq!(assistants[1]["reasoning_content"], "Now I can answer.");
    }

    /// **And a request WITHOUT tools does not carry it** — the other half of the
    /// documented rule, and the reason this is a parameter rather than a constant:
    /// DeepSeek ignores the field without `tools`, and adding it anyway would be a
    /// change to a prefix that buys nothing.
    #[test]
    fn a_request_without_tools_does_not_carry_it() {
        let items = vec![
            TranscriptItem::Reasoning {
                text: "thinking".into(),
                field: letibot_transcript::ReasoningField::ReasoningContent,
                truncated: false,
            },
            TranscriptItem::Assistant {
                text: "answer".into(),
                tool_calls: vec![],
                truncated: false,
            },
        ];
        let m = convert("sys", &items, false);
        assert!(
            m.iter().all(|r| r.get("reasoning_content").is_none()),
            "without tools the field is not sent: {m:#?}"
        );
    }

    /// A split reasoning row is one thought in two items, so it travels joined — and
    /// **nothing is dropped**, which is the whole requirement.
    #[test]
    fn a_truncated_reasoning_row_and_its_continuation_travel_together() {
        let items = vec![
            TranscriptItem::Reasoning {
                text: "first half".into(),
                field: letibot_transcript::ReasoningField::ReasoningContent,
                truncated: true,
            },
            TranscriptItem::Reasoning {
                text: "second half".into(),
                field: letibot_transcript::ReasoningField::ReasoningContent,
                truncated: false,
            },
            TranscriptItem::Assistant {
                text: "answer".into(),
                tool_calls: vec![],
                truncated: false,
            },
        ];
        let m = convert("sys", &items, true);
        let a = m.iter().find(|r| r["role"] == "assistant").unwrap();
        let r = a["reasoning_content"].as_str().unwrap();
        assert!(
            r.contains("first half") && r.contains("second half"),
            "{r:?}"
        );
    }

    /// An assistant with no reasoning before it gets no field — the field means
    /// "this turn reasoned", not "the request has tools".
    #[test]
    fn an_assistant_that_did_not_reason_carries_no_field() {
        let items = vec![user("hi"), calls(&["c1"])];
        let m = convert("sys", &items, true);
        let a = m.iter().find(|r| r["role"] == "assistant").unwrap();
        assert!(a.get("reasoning_content").is_none(), "{a:#?}");
    }
}
