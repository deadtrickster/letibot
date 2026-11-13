//! The transcript as a Messages request: `system` at the top level, content blocks inside
//! `messages`, and the blocks whose bytes have to go back unchanged.
//!
//! Field by field, against <https://platform.claude.com/docs/en/api/messages>:
//!
//! ```text
//! system      top level, string or text blocks                     (not a message role)
//! messages    role: "user" | "assistant"; consecutive same-role turns combine
//! text        {type: "text", text}
//! image       {type: "image", source: {type: "base64", media_type, data}}
//! tool_use    {type: "tool_use", id, name, input}                   (in an assistant turn)
//! tool_result {type: "tool_result", tool_use_id, content, is_error} (in a user turn)
//! thinking    {type: "thinking", thinking, signature}               (assistant, unmodified)
//! redacted    {type: "redacted_thinking", data}                     (assistant, unmodified)
//! tools       [{name, description, input_schema}]                   (not `{type: function}`)
//! ```
//!
//! Four rules of the protocol shape this file, each cited where it bites:
//!
//! * A `tool_result` must answer a `tool_use` in the message directly above it, and the
//!   results must come FIRST in their user message
//!   (<https://platform.claude.com/docs/en/agents-and-tools/tool-use/handle-tool-calls>).
//! * A thinking block that is replayed must be the one the API signed, unmodified and in order
//!   (<https://platform.claude.com/docs/en/build-with-claude/thinking#preserving-thinking-blocks>);
//!   within a tool-use turn that is required, across turns it is only recommended, so a block
//!   whose signature this build does not hold is left out rather than sent unsigned - the API
//!   rejects an unsigned one, and a dropped block is legal where a modified one is not.
//! * `system` has no role of its own inside `messages`, so an instruction that arrives after the
//!   conversation (compaction writes one) is carried as a user turn - the same call `messages.rs`
//!   makes for a dialect that cannot take a mid-history system message.
//! * A result the OPERATOR ran has no `tool_use` above it, so it becomes the person's own turn
//!   rather than a `tool_result`: `messages.rs`'s `person_ran`, for its reason.

use letibot_transcript::{CallOrigin, ToolOutcome, TranscriptItem, UserPart};
use serde_json::{Value, json};

use crate::messages::{Eyes, Withheld, not_sent_said, picture_fate};

/// Which side of the conversation a message is on. The API has exactly two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    User,
    Assistant,
}

impl Role {
    fn name(self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Assistant => "assistant",
        }
    }
}

/// One message being assembled: its blocks, and how many of the leading ones are `tool_result`s,
/// which is the position the next result goes in - see [`push`].
struct Message {
    role: Role,
    blocks: Vec<Value>,
    results: usize,
}

impl Message {
    fn render(self) -> Value {
        json!({"role": self.role.name(), "content": self.blocks})
    }
}

/// A block that answers a call goes before every word of its message.
fn push(message: &mut Message, block: Value, is_result: bool) {
    if is_result {
        let at = message.results;
        message.blocks.insert(at, block);
        message.results += 1;
    } else {
        message.blocks.push(block);
    }
}

/// The message a block belongs to: the last one when it is the same side, which is how the API
/// reads consecutive same-role turns anyway ("Consecutive `user` or `assistant` turns in your
/// request will be combined into a single turn"), and one message fewer to cache.
fn open(out: &mut Vec<Message>, role: Role) -> &mut Message {
    if out.last().map(|m| m.role) != Some(role) {
        out.push(Message {
            role,
            blocks: Vec::new(),
            results: 0,
        });
    }
    out.last_mut().expect("just pushed")
}

/// What one turn of the walk is handed: the picture policy and whether signed thinking may be
/// replayed. `replay_thoughts` is false for the one retry the API itself asks for - see
/// `crate::anthropic`'s refusal handling.
#[derive(Clone, Copy)]
pub(super) struct Policy<'a> {
    pub eyes: Eyes<'a>,
    pub withheld: Withheld<'a>,
    pub replay_thoughts: bool,
}

/// The `system` field: the request's own prompt first, then a leading `System` item's text - the
/// two carriers of a system prompt this tree has (`messages.rs` sends whichever, and an empty
/// request prompt with a leading item is the shape a bootstrap writes).
///
/// A cache breakpoint goes on the LAST block, because the system prompt is the front of the
/// prefix every request shares: <https://platform.claude.com/docs/en/build-with-claude/prompt-caching>
pub(super) fn system(system: &str, items: &[TranscriptItem], cache: bool) -> Option<Value> {
    let mut blocks: Vec<Value> = Vec::new();
    if !system.trim().is_empty() {
        blocks.push(json!({"type": "text", "text": system}));
    }
    if let Some(TranscriptItem::System { text, .. }) = items.first()
        && !text.trim().is_empty()
    {
        blocks.push(json!({"type": "text", "text": text}));
    }
    if blocks.is_empty() {
        return None;
    }
    if cache && let Some(last) = blocks.last_mut() {
        last["cache_control"] = json!({"type": "ephemeral"});
    }
    Some(Value::Array(blocks))
}

/// The `messages` array, paired and ready: every `tool_use` answered, every orphan result gone,
/// and no empty message.
pub(super) fn messages(
    items: &[TranscriptItem],
    tail: &[TranscriptItem],
    policy: Policy<'_>,
) -> Vec<Value> {
    let mut out: Vec<Message> = Vec::new();
    // Results whose author the operator is, by `call_id`: collected in the walk, and read by the
    // pairing pass, which is where a result with nothing above it is decided.
    let mut own_author: Vec<String> = Vec::new();
    // The leading `System` item is part of the top-level `system` field, so it is not also a turn.
    let skip_leading_system = matches!(items.first(), Some(TranscriptItem::System { .. }));
    // The tail is what this request adds, so the breakpoint goes at the end of the transcript - the
    // prefix every request of this conversation shares - and only once, at the boundary.
    let mut marked = false;
    for (n, item) in items.iter().chain(tail.iter()).enumerate() {
        if !marked && n == items.len() {
            marked = true;
            if let Some(last) = out.last_mut() {
                mark_cache(last);
            }
        }
        if n == 0 && skip_leading_system {
            continue;
        }
        match item {
            // The leading one became part of `system`; a later one is an instruction that arrived
            // after the conversation, and the protocol has no role for it here.
            TranscriptItem::System { text, .. } => {
                if text.trim().is_empty() {
                    continue;
                }
                push(
                    open(&mut out, Role::User),
                    json!({"type": "text", "text": text}),
                    false,
                );
            }
            TranscriptItem::Notice { text, .. } => {
                if text.trim().is_empty() {
                    continue;
                }
                push(
                    open(&mut out, Role::User),
                    json!({"type": "text", "text": text}),
                    false,
                );
            }
            TranscriptItem::User { parts, .. } => {
                let m = open(&mut out, Role::User);
                let mut images = 0usize;
                for part in parts {
                    match part {
                        UserPart::Text { text } if text.trim().is_empty() => {}
                        UserPart::Text { text } => {
                            push(m, json!({"type": "text", "text": text}), false);
                        }
                        UserPart::Image {
                            media_type,
                            data_ref,
                        } => {
                            images += 1;
                            match picture_fate(data_ref, policy.eyes, policy.withheld) {
                                None => push(m, image_block(media_type, data_ref), false),
                                Some(why) => push(
                                    m,
                                    json!({"type": "text", "text": not_sent_said(
                                        images,
                                        media_type,
                                        crate::messages::data_ref_bytes(data_ref),
                                        &why,
                                    )}),
                                    false,
                                ),
                            }
                        }
                        UserPart::FileRef { path, .. } => {
                            push(m, json!({"type": "text", "text": path}), false);
                        }
                    }
                }
            }
            TranscriptItem::Reasoning {
                text,
                field,
                truncated,
            } => {
                if !policy.replay_thoughts || *truncated {
                    continue;
                }
                let block = match field {
                    letibot_transcript::ReasoningField::Signed { signature } => {
                        json!({"type": "thinking", "thinking": text, "signature": signature})
                    }
                    letibot_transcript::ReasoningField::Redacted { data } => {
                        json!({"type": "redacted_thinking", "data": data})
                    }
                    // A thought from a provider that does not sign them: there is no block this
                    // API would accept, and its text as assistant words is a sentence the model
                    // never said.
                    _ => continue,
                };
                push(open(&mut out, Role::Assistant), block, false);
            }
            TranscriptItem::Assistant {
                text, tool_calls, ..
            } => {
                let m = open(&mut out, Role::Assistant);
                if !text.is_empty() {
                    push(m, json!({"type": "text", "text": text}), false);
                }
                for call in tool_calls {
                    push(
                        m,
                        json!({
                            "type": "tool_use",
                            "id": call.id,
                            "name": call.name,
                            // The API wants an object; the tree keeps the model's raw text
                            // because a dialect replays it verbatim.
                            "input": input_of(&call.arguments),
                        }),
                        false,
                    );
                }
            }
            TranscriptItem::ToolResult {
                call_id,
                outcome,
                payload,
                origin,
                media,
                ..
            } => {
                if matches!(origin, Some(CallOrigin::Operator { .. })) {
                    own_author.push(call_id.clone());
                }
                let failed = is_error(outcome);
                // An empty text block is refused by some servers of this API (Kimi's): a command
                // that printed nothing says so instead.
                let said = match payload.trim().is_empty() {
                    true => "(no output)",
                    false => payload.as_str(),
                };
                let mut content: Vec<Value> = vec![json!({"type": "text", "text": said})];
                if let Some(m) = media {
                    // A picture the call read rides the result that read it: the API takes an
                    // image block inside `tool_result.content`, and a model that does not get
                    // pictures is told what it is missing instead.
                    match picture_fate(&m.data_ref, policy.eyes, policy.withheld) {
                        None => content.push(image_block(&m.mime, &m.data_ref)),
                        Some(why) => content.push(json!({
                            "type": "text",
                            "text": format!(
                                "Attached media from tool result: {}",
                                not_sent_said(1, &m.mime, Some(m.bytes), &why)
                            ),
                        })),
                    }
                }
                let mut block = json!({
                    "type": "tool_result",
                    "tool_use_id": call_id,
                    "content": content,
                });
                if failed {
                    block["is_error"] = json!(true);
                }
                push(open(&mut out, Role::User), block, true);
            }
            // Display-only rows: every renderer skips them, so they are not prompt text.
            TranscriptItem::SegmentMark { .. } | TranscriptItem::Offer { .. } => {}
        }
    }
    pair(out, &own_author)
}

/// A cache breakpoint on the last block that can hold one. The API grows a conversation by caching
/// the prefix up to the breakpoint, and a thinking block cannot be cached directly - it is cached
/// only as part of a turn - so the walk goes back to the nearest block that can
/// (<https://platform.claude.com/docs/en/build-with-claude/prompt-caching>).
fn mark_cache(message: &mut Message) {
    for block in message.blocks.iter_mut().rev() {
        let kind = block["type"].as_str().unwrap_or_default();
        if matches!(kind, "text" | "image" | "tool_use" | "tool_result") {
            block["cache_control"] = json!({"type": "ephemeral"});
            return;
        }
    }
}

/// An `image` block from a `data:` reference. The reference is `data:<mime>;base64,<payload>`
/// and the API wants the three fields apart: <https://platform.claude.com/docs/en/api/messages>
fn image_block(mime: &str, data_ref: &str) -> Value {
    json!({
        "type": "image",
        "source": {
            "type": "base64",
            "media_type": mime,
            "data": letibot_transcript::media::wire_payload(data_ref),
        },
    })
}

/// The model's tool arguments as the object the API validates `input` against. The tree keeps the
/// raw text so a dialect can replay it byte for byte; this API takes JSON, so a call whose text is
/// not an object is sent with no arguments rather than dropped - its result still answers it, and
/// the payload the model reads is the result's.
fn input_of(arguments: &str) -> Value {
    match serde_json::from_str::<Value>(arguments) {
        Ok(v @ Value::Object(_)) => v,
        _ => json!({}),
    }
}

/// Did this call fail? A refusal, a timeout and a call that never ran are failures to the model in
/// the same way: it asked for something and did not get it. A backgrounded call and an abstention
/// are answers, and carry their own envelope in the payload.
fn is_error(outcome: &ToolOutcome) -> bool {
    matches!(
        outcome,
        ToolOutcome::Failed { .. }
            | ToolOutcome::Denied { .. }
            | ToolOutcome::Timeout
            | ToolOutcome::NotRun { .. }
    )
}

/// Every `tool_use` gets its `tool_result` in the message directly after it, or the API refuses the
/// whole conversation ("tool_use ids were found without tool_result blocks immediately after").
/// A local dialect renders a call that never came back as text and carries on, so a transcript
/// that is fine on qwen is one this API will not read. The gap is filled rather than the request
/// being sent to be refused: the filler says no result was recorded, which is better information
/// than a fabricated success.
///
/// A result whose `tool_use` is gone - a call a compaction dropped - is removed for the mirror
/// reason, unless the OPERATOR ran it, and those become the person's own turn: nothing proposed
/// them, so a `tool_result` would answer an id that is not there.
fn pair(out: Vec<Message>, own_author: &[String]) -> Vec<Value> {
    let mut kept: Vec<Message> = Vec::with_capacity(out.len());
    for mut message in out {
        if message.role == Role::User {
            // The calls this message is the answer to: the assistant message directly above it.
            let owed = owed_by(kept.last());
            // 1. A result that answers nothing the message above opens goes. The operator's own
            // keep their words, as a line of theirs.
            let mut index = 0usize;
            while index < message.blocks.len() {
                if message.blocks[index]["type"] != "tool_result" {
                    index += 1;
                    continue;
                }
                let id = message.blocks[index]["tool_use_id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                if owed.contains(&id) {
                    index += 1;
                    continue;
                }
                let block = message.blocks.remove(index);
                message.results = message.results.saturating_sub(1);
                if own_author.contains(&id) {
                    let said = block["content"]
                        .as_array()
                        .and_then(|a| a.first())
                        .and_then(|t| t["text"].as_str())
                        .unwrap_or_default();
                    if !said.is_empty() {
                        message.blocks.push(json!({"type": "text", "text": said}));
                    }
                }
            }
            // 2. A call still owed is answered here, in the order it was made, among the results
            // at the front of the message - the only place the API reads them.
            for (at, id) in owed.iter().enumerate() {
                let answered = message
                    .blocks
                    .iter()
                    .any(|b| b["tool_use_id"].as_str() == Some(id.as_str()));
                if !answered {
                    message.blocks.insert(at, unanswered(id));
                    message.results += 1;
                }
            }
        }
        if message.blocks.is_empty() {
            continue;
        }
        kept.push(message);
    }
    // A transcript can simply end on an unanswered call - an interrupted turn does exactly that.
    let owed = owed_by(kept.last());
    if !owed.is_empty() {
        kept.push(Message {
            role: Role::User,
            blocks: owed.iter().map(|id| unanswered(id)).collect(),
            results: owed.len(),
        });
    }
    kept.into_iter().map(Message::render).collect()
}

/// The call ids an assistant message opens, in order - the ids the next user message must answer.
fn owed_by(message: Option<&Message>) -> Vec<String> {
    let Some(m) = message.filter(|m| m.role == Role::Assistant) else {
        return Vec::new();
    };
    m.blocks
        .iter()
        .filter(|b| b["type"] == "tool_use")
        .filter_map(|b| b["id"].as_str().map(str::to_string))
        .collect()
}

fn unanswered(id: &str) -> Value {
    json!({
        "type": "tool_result",
        "tool_use_id": id,
        "content": [{"type": "text", "text":
            "[no result was recorded for this call: the turn ended before it finished, or it was \
             never run]"}],
        "is_error": true,
    })
}

/// The harness hands over its dialect's rendering of a tool schema, either wrapped as
/// `{type: function, function: {...}}` or bare `{name, description, parameters}`. This API wants
/// the three fields at the top level and the schema under `input_schema`
/// (<https://platform.claude.com/docs/en/api/messages>), and anything else is refused here rather
/// than sent to be 400'd.
pub(super) fn tools(tools_json: &[String]) -> Result<Vec<Value>, String> {
    tools_json
        .iter()
        .map(|t| {
            let v: Value =
                serde_json::from_str(t).map_err(|e| format!("a tool schema is not JSON: {e}"))?;
            let inner = v.get("function").unwrap_or(&v);
            let Some(name) = inner.get("name").and_then(Value::as_str) else {
                return Err(format!(
                    "a tool schema has neither `function.name` nor `name`: {}",
                    t.chars().take(80).collect::<String>()
                ));
            };
            let mut tool = json!({
                "name": name,
                "input_schema": inner.get("parameters").cloned().unwrap_or_else(
                    || json!({"type": "object", "properties": {}})),
            });
            if let Some(d) = inner.get("description").and_then(Value::as_str) {
                tool["description"] = json!(d);
            }
            Ok(tool)
        })
        .collect()
}
