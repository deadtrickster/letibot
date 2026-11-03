//! `letibot-render` as a FUNCTION, so more than one binary can be it.
//!
//! The fidelity gate execs it by PATH and knows nothing about subcommands, so this has to
//! be callable from both the binary and the multicall dispatcher. The gate must not know
//! what a `TranscriptItem` is, so expanding a fixture, mapping items onto OpenAI messages
//! and deciding whether a generation prompt belongs all happen here, once:
//! ```text
//! letibot-render --dialect glm-5.3-flash --profile faithful FIXTURE.json [FIXTURE.json …]
//! ```

use crate::{GlmRenderer, ReasoningEffort, ends_mid_turn, generation_prompt, glm_tool_json};
use letibot_dialect::{RenderSpan, StablePrefix, spans_to_string};
use letibot_transcript::{ToolCall, TranscriptItem, UserPart};
use serde::Deserialize;
use serde_json::{Map, Value, json};

#[derive(Debug, Deserialize)]
struct Fixture {
    name: String,
    #[serde(default)]
    why: String,
    #[serde(default = "default_dialect")]
    dialect: String,
    #[serde(default)]
    reasoning_effort: Option<String>,
    #[serde(default)]
    prefix: FixturePrefix,
    #[serde(default)]
    items: Vec<TranscriptItem>,
    /// Places this fixture is *known* to differ from `/apply-template`, each with an
    /// id the runner two-sided-checks: a declared divergence that stops reproducing
    /// fails the gate just as loudly as an undeclared one.
    #[serde(default)]
    divergences: Vec<Divergence>,
    /// Places byte equality is not available, as opposed to not holding - the oracle emits
    /// something unreproducible (a per-process nonce) and both sides have to be folded onto a
    /// canonical form first. Two-sided checked: an unnecessary rewrite is a place a diff hides.
    #[serde(default)]
    normalise: Vec<Divergence>,
}

#[derive(Debug, Default, Deserialize)]
struct FixturePrefix {
    #[serde(default)]
    system: String,
    /// OpenAI-shaped tool schemas. Converted to `StablePrefix.tools_json` by
    /// `glm_tool_json`, which is also what the harness must use to fill that field.
    #[serde(default)]
    tools: Vec<Value>,
}

#[derive(Debug, Deserialize)]
struct Divergence {
    id: String,
    #[serde(default)]
    why: String,
}

fn default_dialect() -> String {
    "glm-5.3-flash".to_string()
}

/// Run the renderer over `args`, returning the process exit code. `args` is what followed
/// the program name, so `letibot-render …` and `letibot render …` arrive here identically.
pub fn run(args: &[String]) -> i32 {
    let mut args = args.iter().cloned();
    let mut dialect_name = default_dialect();
    let mut profile = "faithful".to_string();
    let mut paths: Vec<String> = Vec::new();

    while let Some(a) = args.next() {
        match a.as_str() {
            "--dialect" => dialect_name = args.next().unwrap_or_default(),
            "--profile" => profile = args.next().unwrap_or_default(),
            "-h" | "--help" => {
                eprintln!(
                    "letibot-render --dialect <name> --profile faithful \
                     FIXTURE.json..."
                );
                return 0;
            }
            other => paths.push(other.to_string()),
        }
    }

    if dialect_name != "glm-5.3-flash" {
        eprintln!("unknown dialect {dialect_name:?}; this binary currently carries glm-5.3-flash");
        return 2;
    }
    if profile == "server-bug-compatible" {
        eprintln!(
            "the server-bug-compatible profile was removed (T2). It modelled minja's \
             cross-iteration set-inside-for leak; a template-driven renderer cannot produce \
             it without reintroducing the bug deliberately, and run_gate.py --interop still \
             reports how llama.cpp differs."
        );
        return 2;
    }
    if profile != "faithful" {
        eprintln!("unknown profile {profile:?}; the only profile is \"faithful\"");
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

/// One fixture becomes every prefix of itself, with and without a generation prompt: a
/// renderer that is right about a whole conversation and wrong about its third prefix is
/// one whose cache never hits.
fn expand(fx: &Fixture) -> Vec<Value> {
    let effort = match fx.reasoning_effort.as_deref() {
        Some("low") => ReasoningEffort::Low,
        Some("high") => ReasoningEffort::High,
        _ => ReasoningEffort::Max,
    };
    let tools_json: Vec<String> = fx.prefix.tools.iter().map(glm_tool_json).collect();
    let prefix = StablePrefix {
        system: fx.prefix.system.clone(),
        tools_json,
    };
    let renderer = GlmRenderer::new().with_effort(effort);
    let divergences: Vec<&str> = fx.divergences.iter().map(|d| d.id.as_str()).collect();
    let normalise: Vec<&str> = fx.normalise.iter().map(|d| d.id.as_str()).collect();
    let why: Map<String, Value> = fx
        .divergences
        .iter()
        .chain(fx.normalise.iter())
        .map(|d| (d.id.clone(), Value::String(d.why.clone())))
        .collect();

    let mut out = Vec::new();
    for k in 0..=fx.items.len() {
        let items = &fx.items[..k];
        let spans = renderer.render(&prefix, items);
        let mid_turn = ends_mid_turn(items);

        out.push(case(
            fx,
            k,
            false,
            &spans,
            items,
            &prefix,
            effort,
            &divergences,
            &normalise,
            &why,
        ));

        if !mid_turn {
            // The generation prompt is not a transcript item, so it is a separate
            // case rather than a suffix of the same one. Both are checked: the bytes
            // that start every turn are the ones a mistake is most expensive in.
            let mut with_gen = spans.clone();
            with_gen.extend(generation_prompt());
            out.push(case(
                fx,
                k,
                true,
                &with_gen,
                items,
                &prefix,
                effort,
                &divergences,
                &normalise,
                &why,
            ));
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn case(
    fx: &Fixture,
    prefix_len: usize,
    add_generation_prompt: bool,
    spans: &[RenderSpan],
    items: &[TranscriptItem],
    prefix: &StablePrefix,
    effort: ReasoningEffort,
    divergences: &[&str],
    normalise: &[&str],
    why: &Map<String, Value>,
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
        "dialect": fx.dialect,
        "prefix_len": prefix_len,
        "add_generation_prompt": add_generation_prompt,
        "rendered": spans_to_string(spans),
        "spans": spans.iter().map(span_json).collect::<Vec<_>>(),
        "request": request,
        "divergences": divergences,
        "normalise": normalise,
        "divergence_why": why,
    })
}

fn span_json(s: &RenderSpan) -> Value {
    match s {
        RenderSpan::Text(t) => json!({"text": t}),
        RenderSpan::Control(c) => json!({"control": c.literal}),
    }
}

/// The same conversation, in the shape `/apply-template` expects. Deliberately dumb - it
/// re-groups items and copies text, never renders - so a mapping bug shows up as a mismatch
/// rather than as mutual agreement on the wrong thing.
fn to_openai_messages(prefix: &StablePrefix, items: &[TranscriptItem]) -> Value {
    let mut msgs: Vec<Value> = Vec::new();
    if !prefix.system.is_empty() {
        msgs.push(json!({"role": "system", "content": prefix.system}));
    }

    // The open assistant message, if any: (reasoning, content, tool_calls).
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
                                "function": {"name": c.name, "arguments": c.arguments},
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
            // Where the harness's own words have always gone in this view: a user turn.
            TranscriptItem::Notice { text, .. } => {
                flush(&mut turn, &mut msgs);
                msgs.push(json!({"role": "user", "content": text}));
            }
            TranscriptItem::User { parts, .. } => {
                flush(&mut turn, &mut msgs);
                msgs.push(json!({"role": "user", "content": user_content(parts)}));
            }
            TranscriptItem::Reasoning { text, .. } => {
                // A second reasoning block opens the next assistant message, the same
                // rule the renderer applies.
                if turn.as_ref().is_some_and(|(r, _, _)| r.is_some()) {
                    flush(&mut turn, &mut msgs);
                }
                let t = turn.get_or_insert((None, String::new(), Vec::new()));
                t.0 = Some(text.clone());
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
                // The payload verbatim, which is what the renderer emits: under the harness these bytes
                // are already `ToolResult::render`'s envelope, and a fixture supplies them directly, so
                // both sides get the same string and the gate tests the render, not our wording.
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
