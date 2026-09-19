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
                // Display-only; the prompt never sees it.
                edit: _,
            } => {
                let content = match outcome {
                    ToolOutcome::Ok => payload.clone(),
                    other => format!("[{name}: {}]\n{payload}", outcome_word(other)),
                };
                out.push(json!({"role": "tool", "tool_call_id": call_id, "content": content}));
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

#[cfg(test)]
mod pairing_tests {
    use super::*;
    use letibot_transcript::{ToolCall, TranscriptItem, UserPart};

    fn user(t: &str) -> TranscriptItem {
        TranscriptItem::User {
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
        let next = m.iter().position(|r| r["content"] == "never mind, carry on").unwrap();
        assert!(at < next, "the filler answers the call it belongs to");
    }

    /// A transcript can simply END on an unanswered call — an interrupted turn
    /// does exactly that — and the provider counts it the same way.
    #[test]
    fn a_transcript_that_ends_mid_call_is_still_valid() {
        let m = convert("sys", &[user("go"), calls(&["c1", "c2"])]);
        assert_paired(&m);
        assert_eq!(m.iter().filter(|r| r["role"] == "tool").count(), 2);
    }

    /// Some answered, some not: only the gaps are filled, and the real results
    /// keep their own content.
    #[test]
    fn only_the_missing_half_of_a_fan_out_is_filled() {
        let m = convert(
            "sys",
            &[user("go"), calls(&["c1", "c2", "c3"]), result("c2"), user("next")],
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
        let m = convert("sys", &[user("go"), result("ghost"), user("next")]);
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
        let m = convert("sys", &items);
        assert_paired(&m);
        assert_eq!(m.len(), 5, "system + four, nothing added: {m:?}");
        assert!(m[3]["content"].as_str().unwrap().contains("done"));
    }
}
