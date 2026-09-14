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
//! - `Reasoning` → dropped. DeepSeek documents that `reasoning_content` from
//!   earlier turns must not be sent back; GLM and Grok ignore it. Each turn's
//!   reasoning is the model's own scratch, kept in our record only.
//! - `Assistant` → `{"role":"assistant","content":…,"tool_calls":[…]}`.
//!   `content` is `null` when there is no text and there are calls, which is
//!   the shape the APIs emit and the one they accept back.
//! - `ToolResult` → `{"role":"tool","tool_call_id":…,"content":…}`. The outcome
//!   is folded into the content the way the local dialects do it: an `ok`
//!   result is the payload; anything else is prefixed with what happened, so
//!   the model can tell a refusal from an answer.
//! - `SegmentMark` → dropped. It is the store's, not the model's.

use letibot_transcript::{ToolOutcome, TranscriptItem, UserPart};
use serde_json::{Value, json};

/// The `messages` array. `system` is prepended when non-empty and no `System`
/// item leads the transcript — the harness keeps the system prompt in item 0,
/// so normally it is already there and `system` is the same text.
pub fn convert(system: &str, items: &[TranscriptItem]) -> Vec<Value> {
    let mut out = Vec::with_capacity(items.len() + 1);
    let leads_with_system = matches!(items.first(), Some(TranscriptItem::System { .. }));
    if !system.trim().is_empty() && !leads_with_system {
        out.push(json!({"role": "system", "content": system}));
    }
    for item in items {
        match item {
            TranscriptItem::System { text, .. } => {
                out.push(json!({"role": "system", "content": text}));
            }
            TranscriptItem::User { parts } => {
                let text = parts
                    .iter()
                    .map(|p| match p {
                        UserPart::Text { text } => text.clone(),
                        UserPart::Image { media_type, .. } => {
                            format!("[an image ({media_type}) was attached here and is not sent to this provider]")
                        }
                        UserPart::FileRef { path, .. } => format!("[file: {path}]"),
                    })
                    .collect::<Vec<_>>()
                    .join("\n\n");
                out.push(json!({"role": "user", "content": text}));
            }
            TranscriptItem::Reasoning { .. } | TranscriptItem::SegmentMark { .. } => {}
            TranscriptItem::Assistant {
                text, tool_calls, ..
            } => {
                let mut m = json!({"role": "assistant"});
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
            } => {
                let content = match outcome {
                    ToolOutcome::Ok => payload.clone(),
                    other => format!("[{name}: {}]\n{payload}", outcome_word(other)),
                };
                out.push(json!({"role": "tool", "tool_call_id": call_id, "content": content}));
            }
        }
    }
    out
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
    fn items_become_the_apis_messages_and_reasoning_stays_home() {
        let items = vec![
            TranscriptItem::System {
                text: "be terse".into(),
                origin: SystemOrigin::Bootstrap,
            },
            TranscriptItem::User {
                parts: vec![UserPart::Text {
                    text: "list the crates".into(),
                }],
            },
            TranscriptItem::Reasoning {
                text: "thinking…".into(),
                field: ReasoningField::ReasoningContent,
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
            },
        ];
        let m = convert("be terse", &items);
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
        let m2 = convert("sys", &items[1..2]);
        assert_eq!(m2[0]["role"], "system");
        assert_eq!(m2.len(), 2);
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
