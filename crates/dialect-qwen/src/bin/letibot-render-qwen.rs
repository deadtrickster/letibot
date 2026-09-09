//! `letibot-render-qwen` — the seam between this renderer and the Python oracle.
//!
//! ```text
//! letibot-render-qwen FIXTURE.json [FIXTURE.json …]   > cases.json
//! ```
//!
//! Same contract as `letibot-render` in `dialect-glm`: stdout is a JSON array of
//! **cases**, one per (fixture, prefix length, generation prompt on/off), each
//! carrying the render, its span list, and the `apply_chat_template` request that
//! must produce the same string. `crates/dialect-qwen/fidelity.py` feeds those to
//! `tests/fidelity/oracle_hf.py` and compares.
//!
//! # Why a second binary rather than a `--dialect` arm on the first
//!
//! `tests/fidelity/run_gate.py` hardcodes `cargo run -p letibot-dialect-glm --bin
//! letibot-render`, and that file may not be modified. Adding a Qwen arm to that
//! binary would therefore mean `dialect-glm` depending on `dialect-qwen` — a
//! sibling dialect importing a sibling dialect, so that a build of GLM pulls in a
//! model it has nothing to do with. A second binary and a thirty-line driver keep
//! the crates independent, and `run_gate.py --dialect qwen3.8 --fixtures …` becomes
//! available for free the day that hardcoding is lifted.
//!
//! # What this is not
//!
//! It is not the gate. The gate is 139/139 on GLM and is untouched. This is the
//! same measurement, run on a corpus this crate owns, and it is reported as its own
//! number.

use letibot_dialect::{RenderSpan, StablePrefix, spans_to_string};
use letibot_dialect_qwen::{
    QwenRenderer, ReasoningEffort, ends_mid_turn, generation_prompt, qwen_tool_json,
};
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
    /// Places this renderer is **known** to differ from the shipped template. The
    /// driver reports a declared divergence that stopped reproducing just as loudly
    /// as an undeclared one: a divergence that quietly went away is a divergence
    /// somebody may have "fixed" by changing the wrong thing.
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

fn main() {
    let paths: Vec<String> = std::env::args().skip(1).collect();
    if paths.is_empty() {
        eprintln!("letibot-render-qwen FIXTURE.json [FIXTURE.json …]");
        std::process::exit(2);
    }
    let mut cases = Vec::new();
    for path in &paths {
        let src = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("reading fixture {path}: {e}"));
        let fx: Fixture = serde_json::from_str(&src)
            .unwrap_or_else(|e| panic!("parsing fixture {path}: {e}"));
        cases.extend(expand(&fx));
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&Value::Array(cases)).expect("serialising cases")
    );
}

/// One fixture becomes every prefix of itself, with and without a generation prompt.
///
/// The most valuable thing the corpus does: a renderer that is right about a whole
/// conversation and wrong about its third prefix is a renderer whose cache never
/// hits, and every prefix here is one the harness will actually submit.
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
        // A prefix that ends inside an assistant turn has **no oracle**, and this is
        // not a Qwen quirk being excused — it is a limit of the comparison. The
        // oracle is handed a message list, and a message list cannot express "this
        // assistant message is not finished": `apply_chat_template` closes every
        // message it renders, so it emits an `<|im_end|>` we must not emit yet. GLM
        // never hit this because its template has no explicit turn-end token, so the
        // open and closed renders are the same bytes there.
        //
        // Reported rather than dropped: the driver prints how many cases had no
        // oracle, so "87 of 87" cannot quietly become "87 of 40".
        //
        // The other no-oracle case is an **empty message list**, which this
        // template refuses outright (`raise_exception('No messages provided.')`).
        // It is reachable: `TurnEngine::open` renders `(prefix, [])` to seed the
        // ledger, and with an empty system prompt that is zero messages. What we
        // emit there — the bare reasoning-effort system turn — is the same bytes the
        // template emits for the same prefix once any message exists, which the
        // `no-system[1]` case does check.
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

/// Eight arguments, and a struct would be worse: every one is a fact about this
/// case that the JSON must carry, and bundling them would mean a builder with an
/// `Option` per field — an invitation to leave one out of a corpus that exists to
/// catch exactly that kind of omission.
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

/// The same conversation in the shape `apply_chat_template` expects.
///
/// Deliberately dumb: it re-groups items into messages and copies text. It never
/// renders. A mapping bug therefore shows up as a mismatch — the oracle rendering a
/// conversation we did not render — rather than as mutual agreement on the wrong
/// thing.
///
/// Two Qwen-specific facts live here and nowhere else:
///
/// * **`tool_calls[].function.arguments` must be an object.** The template raises on
///   a JSON *string*, in as many words. `TranscriptItem` keeps arguments as raw text
///   because those bytes are what gets replayed, so this is where they are parsed.
/// * **`reasoning_content` is the field**, even though this model replays its
///   thinking inline. The template reads `message.reasoning_content` and emits it
///   between `<think>` and `</think>` itself.
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
            TranscriptItem::User { parts } => {
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
            TranscriptItem::Assistant { text, tool_calls } => {
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
