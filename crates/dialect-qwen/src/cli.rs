//! `letibot-render-qwen` as a function, so more than one binary can be it.
//!
//! The fidelity gate execs this by PATH, so the multicall must be reachable from the name, and the
//! code that does the work has to be callable from both the binary this crate builds and the
//! dispatcher. One implementation, not two: a renderer that drifted by a byte would re-prefill every
//! conversation.
//!
//! Being a function forces two mechanical changes: the arguments arrive as a parameter, and
//! `std::process::exit(n)` becomes `return n` so the caller decides whether the process ends.
//!
//! The contract is `dialect-glm`'s: stdout is a JSON array of cases, one per (fixture, prefix length,
//! generation prompt on or off), each carrying the render, its span list, and the
//! `apply_chat_template` request that must produce the same string.
//!
//! ```text
//! letibot-render-qwen FIXTURE.json [FIXTURE.json …]   > cases.json
//! ```
//!
//! It is a second binary rather than a `--dialect` arm because the fidelity driver hardcodes GLM's
//! binary: an arm there would mean a sibling dialect depending on a sibling dialect, so a build of
//! GLM pulls in a model it has nothing to do with.
//!
//! It is not the gate. The gate is GLM's and is untouched; this is the same measurement on a corpus
//! this crate owns, reported as its own number.

use crate::{QwenRenderer, ReasoningEffort, ends_mid_turn, generation_prompt, qwen_tool_json};
use letibot_dialect::{RenderSpan, StablePrefix, spans_to_string};
use letibot_transcript::{ToolCall, TranscriptItem, UserPart};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug, Deserialize)]
struct Fixture {
    name: String,
    #[serde(default)]
    why: String,
    #[serde(default)]
    reasoning_effort: Option<String>,
    #[serde(default)]
    prefix: FixturePrefix,
    #[serde(default)]
    items: Vec<TranscriptItem>,
    /// Places this renderer is known to differ from the shipped template. A declared divergence
    /// that stopped reproducing is reported just as loudly as an undeclared one.
    #[serde(default)]
    divergences: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
struct FixturePrefix {
    #[serde(default)]
    system: String,
    #[serde(default)]
    tools: Vec<Value>,
}

/// Run the renderer over `args`, returning the process exit code.
///
/// `args` is what followed the program name.
pub fn run(args: &[String]) -> i32 {
    let paths: Vec<String> = args.to_vec();
    if paths.is_empty() {
        eprintln!("letibot-render-qwen FIXTURE.json [FIXTURE.json …]");
        return 2;
    }
    let mut cases = Vec::new();
    for path in &paths {
        let src =
            std::fs::read_to_string(path).unwrap_or_else(|e| panic!("reading fixture {path}: {e}"));
        let fx: Fixture =
            serde_json::from_str(&src).unwrap_or_else(|e| panic!("parsing fixture {path}: {e}"));
        cases.extend(expand(&fx));
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&Value::Array(cases)).expect("serialising cases")
    );
    0
}

/// One fixture becomes every prefix of itself, with and without a generation prompt: a renderer
/// right about a whole conversation and wrong about its third prefix is one whose cache never hits.
fn expand(fx: &Fixture) -> Vec<Value> {
    let effort = fx
        .reasoning_effort
        .as_deref()
        .and_then(ReasoningEffort::parse)
        .unwrap_or_default();
    let prefix = StablePrefix {
        system: fx.prefix.system.clone(),
        tools_json: fx.prefix.tools.iter().map(qwen_tool_json).collect(),
    };
    let renderer = QwenRenderer::new().with_effort(effort);

    let mut out = Vec::new();
    for k in 0..=fx.items.len() {
        let items = &fx.items[..k];
        let spans = renderer.render(&prefix, items);
        // A prefix that ends inside an assistant turn has no oracle: the template closes every
        // message it renders, so it emits a turn end we must not emit yet. Reported rather than
        // dropped, so a case count cannot quietly shrink.
        let mid_turn = ends_mid_turn(items);
        let reason = if items.is_empty() && fx.prefix.system.is_empty() {
            "empty-messages"
        } else if mid_turn {
            "mid-turn"
        } else {
            ""
        };
        out.push(case(fx, k, false, &spans, items, &prefix, effort, reason));
        if reason.is_empty() {
            let mut with_gen = spans.clone();
            with_gen.extend(generation_prompt());
            out.push(case(fx, k, true, &with_gen, items, &prefix, effort, ""));
        }
    }
    out
}

/// Eight arguments, and a struct would be worse: every one is a fact this case must carry, and an
/// `Option` per field invites leaving one out of a corpus that exists to catch exactly that.
#[allow(clippy::too_many_arguments)]
fn case(
    fx: &Fixture,
    prefix_len: usize,
    add_generation_prompt: bool,
    spans: &[RenderSpan],
    items: &[TranscriptItem],
    prefix: &StablePrefix,
    effort: ReasoningEffort,
    no_oracle: &str,
) -> Value {
    let mut request = json!({
        "messages": to_openai_messages(prefix, items),
        "add_generation_prompt": add_generation_prompt,
    });
    if !fx.prefix.tools.is_empty() {
        request["tools"] = Value::Array(fx.prefix.tools.clone());
    }
    if let Some(w) = effort.wire() {
        request["reasoning_effort"] = Value::String(w.to_string());
    }
    json!({
        "fixture": fx.name,
        "why": fx.why,
        "dialect": "qwen3.8",
        "prefix_len": prefix_len,
        "add_generation_prompt": add_generation_prompt,
        "rendered": spans_to_string(spans),
        "spans": spans.iter().map(span_json).collect::<Vec<_>>(),
        "request": request,
        "divergences": fx.divergences,
        "no_oracle": no_oracle,
    })
}

fn span_json(s: &RenderSpan) -> Value {
    match s {
        RenderSpan::Text(t) => json!({"text": t}),
        RenderSpan::Control(c) => json!({"control": c.literal}),
    }
}

/// The same conversation in the shape `apply_chat_template` expects: deliberately dumb, since a
/// mapping bug should show up as a mismatch. `tool_calls[].function.arguments` must be an object
/// here, and `reasoning_content` is the field even though the thinking is replayed inline.
fn to_openai_messages(prefix: &StablePrefix, items: &[TranscriptItem]) -> Value {
    let mut msgs: Vec<Value> = Vec::new();
    if !prefix.system.is_empty() {
        msgs.push(json!({"role": "system", "content": prefix.system}));
    }

    let mut turn: Option<(Option<String>, String, Vec<ToolCall>)> = None;

    fn flush(turn: &mut Option<(Option<String>, String, Vec<ToolCall>)>, msgs: &mut Vec<Value>) {
        if let Some((reasoning, content, calls)) = turn.take() {
            let mut m = json!({"role": "assistant", "content": content});
            if let Some(r) = reasoning {
                m["reasoning_content"] = Value::String(r);
            }
            if !calls.is_empty() {
                m["tool_calls"] = Value::Array(
                    calls
                        .iter()
                        .map(|c| {
                            json!({
                                "id": c.id,
                                "type": "function",
                                "function": {
                                    "name": c.name,
                                    "arguments": serde_json::from_str::<Value>(&c.arguments)
                                        .unwrap_or_else(|_| json!({})),
                                },
                            })
                        })
                        .collect(),
                );
            }
            msgs.push(m);
        }
    }

    for item in items {
        match item {
            TranscriptItem::SegmentMark { .. } => {}
            TranscriptItem::System { text, .. } => {
                flush(&mut turn, &mut msgs);
                msgs.push(json!({"role": "system", "content": text}));
            }
            TranscriptItem::User { parts, .. } => {
                flush(&mut turn, &mut msgs);
                msgs.push(json!({"role": "user", "content": user_content(parts)}));
            }
            // Closes any open message, not only one that already has reasoning. The
            // renderer's rule is exactly this, and the two must agree or the gate
            // measures the mapping rather than the render.
            TranscriptItem::Reasoning { text, .. } => {
                flush(&mut turn, &mut msgs);
                turn = Some((Some(text.clone()), String::new(), Vec::new()));
            }
            TranscriptItem::Assistant {
                text, tool_calls, ..
            } => {
                let t = turn.get_or_insert((None, String::new(), Vec::new()));
                t.1.push_str(text);
                t.2.extend(tool_calls.iter().cloned());
            }
            TranscriptItem::ToolResult {
                call_id, payload, ..
            } => {
                flush(&mut turn, &mut msgs);
                msgs.push(json!({
                    "role": "tool",
                    "tool_call_id": call_id,
                    // The payload verbatim, which is what the renderer emits.
                    "content": payload,
                }));
            }
        }
    }
    flush(&mut turn, &mut msgs);
    Value::Array(msgs)
}

fn user_content(parts: &[UserPart]) -> Value {
    let has_media = parts.iter().any(|p| matches!(p, UserPart::Image { .. }));
    if !has_media {
        let mut s = String::new();
        for p in parts {
            let t = match p {
                UserPart::Text { text } => text.as_str(),
                UserPart::FileRef { path, .. } => path.as_str(),
                UserPart::Image { .. } => unreachable!(),
            };
            if !s.is_empty() {
                s.push('\n');
            }
            s.push_str(t);
        }
        return Value::String(s);
    }
    Value::Array(
        parts
            .iter()
            .map(|p| match p {
                UserPart::Text { text } => json!({"type": "text", "text": text}),
                UserPart::FileRef { path, .. } => json!({"type": "text", "text": path}),
                UserPart::Image { data_ref, .. } => {
                    json!({"type": "image_url", "image_url": {"url": data_ref}})
                }
            })
            .collect::<Vec<_>>(),
    )
}
