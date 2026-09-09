//! A **test fixture**, not a dialect.
//!
//! # Read this before reusing anything in this file
//!
//! The box serves `qwen-3.8-flash-next` on 8080 and GLM is not running. We have a
//! verified GLM renderer and no Qwen renderer, and T1 settled that the real one
//! will be template-driven (the model's shipped jinja through minijinja) — building
//! a second hand-written renderer is explicitly *not* this strand's work.
//!
//! So this is the smallest thing that makes the engine's own pipeline exercisable
//! against a live model: enough ChatML to produce tokens Qwen recognises. It is
//! **not** claimed to be byte-exact with Qwen's shipped template, it is not in the
//! fidelity gate, and it must never be promoted out of `tests/`. It lives here
//! rather than in the library precisely so that promoting it takes a deliberate
//! move rather than an import.
//!
//! What it *is* good for: proving that render → tokenize → ledger → submit →
//! stream → parse → commit closes against a real server, with real ids, real cache
//! numbers and the real progress-frame behaviour. None of that depends on the
//! renderer being the canonical one.
//!
//! The control tokens are the model's own, read out of the GGUF with
//! `python3 tests/fidelity/extract_template.py --tokens <gguf>`:
//!
//! ```text
//! 248045 '<|im_start|>' CONTROL      248068 '<think>'  USER_DEFINED
//! 248046 '<|im_end|>'   CONTROL      248069 '</think>' USER_DEFINED
//! 248058 '<tool_call>'  USER_DEFINED 248066 '<tool_response>'  USER_DEFINED
//! ```

use std::borrow::Cow;

use letibot_dialect::{
    ControlRole, ControlToken, ControlTokens, DialectSpec, Guard, ParsedSpan, Parser, RenderSpan,
    StablePrefix, StopToken, SystemUpdateMode, TokenDecoder,
};
use letibot_transcript::{ToolOutcome, TranscriptItem, UserPart};
use letibot_turn::PromptRenderer;

const fn t(role: ControlRole, literal: &'static str) -> ControlToken {
    ControlToken::borrowed(role, literal)
}

pub const IM_START: ControlToken = t(ControlRole::Other, "<|im_start|>");
pub const IM_END: ControlToken = t(ControlRole::TurnEnd, "<|im_end|>");
pub const ENDOFTEXT: ControlToken = t(ControlRole::EndOfTurn, "<|endoftext|>");
pub const THINK_OPEN: ControlToken = t(ControlRole::ThinkOpen, "<think>");
pub const THINK_CLOSE: ControlToken = t(ControlRole::ThinkClose, "</think>");
pub const TOOL_CALL_OPEN: ControlToken = t(ControlRole::ToolCallOpen, "<tool_call>");
pub const TOOL_CALL_CLOSE: ControlToken = t(ControlRole::ToolCallClose, "</tool_call>");
pub const TOOL_RESPONSE_OPEN: ControlToken = t(ControlRole::ToolResultOpen, "<tool_response>");
pub const TOOL_RESPONSE_CLOSE: ControlToken = t(ControlRole::ToolResultClose, "</tool_response>");

const TOKENS: &[ControlToken] = &[
    IM_START,
    IM_END,
    ENDOFTEXT,
    THINK_OPEN,
    THINK_CLOSE,
    TOOL_CALL_OPEN,
    TOOL_CALL_CLOSE,
    TOOL_RESPONSE_OPEN,
    TOOL_RESPONSE_CLOSE,
];

pub fn spec() -> DialectSpec {
    DialectSpec {
        name: Cow::Borrowed("qwen-chatml-test-fixture"),
        // Empty on purpose: this fixture does not run a template, and putting a
        // plausible-looking one here would invite somebody to trust it.
        template: Cow::Borrowed(""),
        template_sha: [0u8; 32],
        control_tokens: ControlTokens::borrowed(TOKENS),
        stop_tokens: vec![
            StopToken::borrowed(ControlRole::TurnEnd, "<|im_end|>"),
            StopToken::borrowed(ControlRole::EndOfTurn, "<|endoftext|>"),
        ],
        system_update_mode: SystemUpdateMode::InHistory,
        // Generous: these must not fire during a live test, and a guard that fires
        // on ordinary prose would make the test measure the guard.
        guards: vec![Guard::RepetitionRun { run: 128 }],
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Think {
    #[default]
    None,
    Open,
    Closed,
}

#[derive(Debug, Default, Clone)]
struct State {
    assistant_open: bool,
    think: Think,
}

pub struct ChatMlRenderer {
    spec: DialectSpec,
}

impl Default for ChatMlRenderer {
    fn default() -> Self {
        ChatMlRenderer { spec: spec() }
    }
}

impl ChatMlRenderer {
    // Used by `engine_decisions`, not by `live_qwen`; both include this module as
    // source, so one target always reports it unused.
    #[allow(dead_code)]
    /// A fixture with different declared guards, so §8.5's policy can be exercised
    /// without waiting for a model to actually collapse.
    pub fn with_guards(guards: Vec<Guard>) -> Self {
        ChatMlRenderer {
            spec: DialectSpec { guards, ..spec() },
        }
    }
}

fn ctl(out: &mut Vec<RenderSpan>, c: &ControlToken) {
    out.push(RenderSpan::Control(c.clone()));
}

fn text(out: &mut Vec<RenderSpan>, s: impl Into<String>) {
    let s = s.into();
    if !s.is_empty() {
        out.push(RenderSpan::Text(s));
    }
}

fn close_turn(st: &mut State, out: &mut Vec<RenderSpan>) {
    if st.assistant_open {
        ctl(out, &IM_END);
        text(out, "\n");
        st.assistant_open = false;
        st.think = Think::None;
    }
}

fn open_assistant(st: &mut State, out: &mut Vec<RenderSpan>) {
    if !st.assistant_open {
        ctl(out, &IM_START);
        text(out, "assistant\n");
        st.assistant_open = true;
        st.think = Think::None;
    }
}

fn render_items(items: &[TranscriptItem], st: &mut State, out: &mut Vec<RenderSpan>) {
    for item in items {
        match item {
            TranscriptItem::SegmentMark { .. } => {}
            TranscriptItem::System { text: s, .. } => {
                close_turn(st, out);
                ctl(out, &IM_START);
                text(out, format!("system\n{s}"));
                ctl(out, &IM_END);
                text(out, "\n");
            }
            TranscriptItem::User { parts } => {
                close_turn(st, out);
                ctl(out, &IM_START);
                text(out, "user\n");
                for p in parts {
                    match p {
                        UserPart::Text { text: s } => text(out, s.clone()),
                        UserPart::FileRef { path, .. } => text(out, format!("[file {path}]")),
                        UserPart::Image { media_type, .. } => {
                            text(out, format!("[image {media_type}]"))
                        }
                    }
                }
                ctl(out, &IM_END);
                text(out, "\n");
            }
            TranscriptItem::Reasoning { text: s, .. } => {
                open_assistant(st, out);
                if st.think == Think::None {
                    ctl(out, &THINK_OPEN);
                    st.think = Think::Open;
                }
                text(out, s.clone());
                ctl(out, &THINK_CLOSE);
                st.think = Think::Closed;
            }
            TranscriptItem::Assistant {
                text: s,
                tool_calls,
            } => {
                open_assistant(st, out);
                if st.think == Think::None {
                    ctl(out, &THINK_OPEN);
                    ctl(out, &THINK_CLOSE);
                    st.think = Think::Closed;
                }
                text(out, s.clone());
                for call in tool_calls {
                    ctl(out, &TOOL_CALL_OPEN);
                    text(
                        out,
                        format!(
                            "\n{{\"name\": \"{}\", \"arguments\": {}}}\n",
                            call.name, call.arguments
                        ),
                    );
                    ctl(out, &TOOL_CALL_CLOSE);
                }
            }
            TranscriptItem::ToolResult {
                outcome, payload, ..
            } => {
                close_turn(st, out);
                ctl(out, &IM_START);
                text(out, "user\n");
                ctl(out, &TOOL_RESPONSE_OPEN);
                text(out, "\n");
                text(out, envelope(outcome, payload));
                text(out, "\n");
                ctl(out, &TOOL_RESPONSE_CLOSE);
                ctl(out, &IM_END);
                text(out, "\n");
            }
        }
    }
}

/// §8.2: the outcome class is carried structurally, not merely worded. Same shape
/// as the GLM renderer's, because the rule is the harness's and not the model's.
fn envelope(outcome: &ToolOutcome, payload: &str) -> String {
    match outcome {
        ToolOutcome::Ok => payload.to_string(),
        ToolOutcome::Abstained { reason } => {
            format!("NO_RESULT\noutcome: abstained\nreason: {reason}")
        }
        ToolOutcome::Failed { reason } => format!("NO_RESULT\noutcome: failed\nreason: {reason}"),
        ToolOutcome::Denied { req_id } => format!("NO_RESULT\noutcome: denied\nrequest: {req_id}"),
        ToolOutcome::Timeout => "NO_RESULT\noutcome: timeout".into(),
        ToolOutcome::NotRun { why } => format!("NO_RESULT\noutcome: not_run\nreason: {why}"),
    }
}

impl PromptRenderer for ChatMlRenderer {
    fn spec(&self) -> &DialectSpec {
        &self.spec
    }

    fn render(&self, prefix: &StablePrefix, items: &[TranscriptItem]) -> Vec<RenderSpan> {
        let mut out = Vec::new();
        ctl(&mut out, &IM_START);
        let mut system = format!("system\n{}", prefix.system);
        if !prefix.tools_json.is_empty() {
            system.push_str("\n\n# Tools\n\n<tools>\n");
            for tool in &prefix.tools_json {
                system.push_str(tool);
                system.push('\n');
            }
            system.push_str("</tools>");
        }
        text(&mut out, system);
        ctl(&mut out, &IM_END);
        text(&mut out, "\n");

        let mut st = State::default();
        render_items(items, &mut st, &mut out);
        out
    }

    fn render_incremental(
        &self,
        history: &[TranscriptItem],
        new_items: &[TranscriptItem],
    ) -> Vec<RenderSpan> {
        let mut st = State::default();
        if !history.is_empty() {
            let mut discard = Vec::new();
            render_items(history, &mut st, &mut discard);
        }
        let mut out = Vec::new();
        render_items(new_items, &mut st, &mut out);
        out
    }

    fn generation_prompt(&self) -> Vec<RenderSpan> {
        vec![
            RenderSpan::Control(IM_START.clone()),
            RenderSpan::Text("assistant\n".into()),
            RenderSpan::Control(THINK_OPEN.clone()),
        ]
    }
}

/// The ChatML side of `Parser`. Boundaries by token id, never by scanning text.
#[derive(Debug, Default, Clone, Copy)]
pub struct ChatMlParser;

impl Parser for ChatMlParser {
    fn parse(&self, tokens: &[u32], decoder: &dyn TokenDecoder) -> Vec<ParsedSpan> {
        let mut out = Vec::new();
        let mut buf = String::new();
        let mut run: Vec<u32> = Vec::new();
        let mut in_reasoning = false;
        let mut in_call = false;

        for &id in tokens {
            let Some(role) = decoder.control_role(id) else {
                run.push(id);
                continue;
            };
            if !run.is_empty() {
                buf.push_str(&decoder.decode(&run));
                run.clear();
            }
            match role {
                ControlRole::ThinkOpen => {
                    flush(&mut buf, &mut out, in_reasoning);
                    in_reasoning = true;
                }
                ControlRole::ThinkClose => {
                    flush(&mut buf, &mut out, true);
                    in_reasoning = false;
                }
                ControlRole::ToolCallOpen => {
                    flush(&mut buf, &mut out, in_reasoning);
                    in_call = true;
                }
                ControlRole::ToolCallClose => {
                    let raw = std::mem::take(&mut buf);
                    out.push(tool_call_from(&raw));
                    in_call = false;
                }
                other => {
                    if !in_call {
                        flush(&mut buf, &mut out, in_reasoning);
                        out.push(ParsedSpan::Control(other));
                    }
                }
            }
        }
        if !run.is_empty() {
            buf.push_str(&decoder.decode(&run));
        }
        flush(&mut buf, &mut out, in_reasoning);
        out
    }
}

fn flush(buf: &mut String, out: &mut Vec<ParsedSpan>, reasoning: bool) {
    if buf.is_empty() {
        return;
    }
    let text = std::mem::take(buf);
    out.push(if reasoning {
        ParsedSpan::Reasoning(text)
    } else {
        ParsedSpan::Content(text)
    });
}

fn tool_call_from(raw: &str) -> ParsedSpan {
    let v: serde_json::Value = serde_json::from_str(raw.trim()).unwrap_or(serde_json::Value::Null);
    ParsedSpan::ToolCall {
        id: None,
        name: v
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .to_string(),
        arguments: v
            .get("arguments")
            .map(|a| a.to_string())
            // A tool call the model wrote that we cannot read is kept verbatim
            // rather than replaced with `{}`: §5.7 has to be able to tell a
            // truncated argument from an empty one.
            .unwrap_or_else(|| raw.trim().to_string()),
    }
}
