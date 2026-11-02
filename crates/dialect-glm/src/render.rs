//! The second renderer: hand-written, kept as a differential against the template.
//!
//! The shipped template run through minijinja is the production renderer; this one exists
//! because two independent implementations that agree are stronger evidence than one that
//! passes its own tests, and the fidelity corpus is measured through it. It renders one
//! thing only: the training format as CPython Jinja2 produces it. It does not reproduce
//! llama.cpp's cross-iteration `{% set %}` leak, because the Jinja2 reading is what the
//! model was trained against.

use crate::ReasoningEffort;
use crate::json::arg_value_text;
use crate::tokens as tk;
use letibot_dialect::{ControlToken, RenderSpan, StablePrefix};
use letibot_transcript::{ToolOutcome, TranscriptItem, UserPart};
use serde_json::Value;

/// GLM's hand-written renderer: a pure function of `(effort, prefix, items)`, kept as a
/// differential against the template. Appending takes the history, which is what the
/// boundary state needs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GlmRenderer {
    effort: ReasoningEffort,
}

impl GlmRenderer {
    pub fn new() -> Self {
        GlmRenderer::default()
    }

    pub fn with_effort(mut self, effort: ReasoningEffort) -> Self {
        self.effort = effort;
        self
    }

    pub fn effort(&self) -> ReasoningEffort {
        self.effort
    }

    /// The whole prompt: stable prefix, then every item.
    pub fn render(&self, prefix: &StablePrefix, items: &[TranscriptItem]) -> Vec<RenderSpan> {
        let mut out = Vec::new();
        render_prefix(self.effort, prefix, &mut out);
        let mut st = State::default();
        render_items(items, &mut st, &mut out);
        out
    }

    /// The spans to append to a render of `history`, for `new_items`: must agree with
    /// [`GlmRenderer::render`], which is what the append-only design rests on. `history` is the
    /// whole transcript, not a count, because the boundary state is not derivable from one.
    pub fn render_incremental(
        &self,
        history: &[TranscriptItem],
        new_items: &[TranscriptItem],
    ) -> Vec<RenderSpan> {
        let mut st = State::default();
        if !history.is_empty() {
            // Replay the already-rendered items for their effect on the state only.
            let mut discard = Vec::new();
            render_items(history, &mut st, &mut discard);
        }
        let mut out = Vec::new();
        render_items(new_items, &mut st, &mut out);
        out
    }
}

/// Where inside an assistant turn the renderer currently is. `Open` means `<think>` was
/// emitted with nothing in it yet - the state a generation prompt leaves behind, which is
/// why append-only survives a turn that starts mid-`<think>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Think {
    #[default]
    None,
    Open,
    Closed,
}

#[derive(Debug, Clone, Default)]
struct State {
    assistant_open: bool,
    think: Think,
    prev_was_tool_result: bool,
}

/// Push a control token. A `clone` of a `Cow::Borrowed`, so it allocates nothing.
fn ctl(out: &mut Vec<RenderSpan>, token: &ControlToken) {
    out.push(RenderSpan::Control(token.clone()));
}

fn text(out: &mut Vec<RenderSpan>, s: impl Into<String>) {
    let s = s.into();
    if !s.is_empty() {
        out.push(RenderSpan::Text(s));
    }
}

/// The head of every GLM prompt: `[gMASK]<sop>`, the effort line, the tool block, then the
/// bootstrap system message. This is what the server's prefix cache is keyed on, and the
/// order is the template's: the effort line and tools come before the message loop.
fn render_prefix(effort: ReasoningEffort, prefix: &StablePrefix, out: &mut Vec<RenderSpan>) {
    ctl(out, &tk::GMASK);
    ctl(out, &tk::SOP);

    ctl(out, &tk::SYSTEM);
    text(out, format!("Reasoning Effort: {}", effort.rendered()));

    if !prefix.tools_json.is_empty() {
        ctl(out, &tk::SYSTEM);
        let mut head = String::from(
            "\n# Tools\n\nYou may call one or more functions to assist with the user query.\n\n\
             You are provided with function signatures within <tools></tools> XML tags:\n<tools>\n",
        );
        for tool in &prefix.tools_json {
            head.push_str(tool);
            head.push('\n');
        }
        head.push_str(
            "</tools>\n\nFor each function call, output the function name and arguments within \
             the following XML format:\n",
        );
        text(out, head);
        // The instruction line spells out GLM's own tool-call tokens, and they are
        // real single-token vocab entries even here, inside a system prompt. Emitting
        // them as text would tokenize the instruction differently from training.
        ctl(out, &tk::TOOL_CALL_OPEN);
        text(out, "{function-name}");
        ctl(out, &tk::ARG_KEY_OPEN);
        text(out, "{arg-key-1}");
        ctl(out, &tk::ARG_KEY_CLOSE);
        ctl(out, &tk::ARG_VALUE_OPEN);
        text(out, "{arg-value-1}");
        ctl(out, &tk::ARG_VALUE_CLOSE);
        ctl(out, &tk::ARG_KEY_OPEN);
        text(out, "{arg-key-2}");
        ctl(out, &tk::ARG_KEY_CLOSE);
        ctl(out, &tk::ARG_VALUE_OPEN);
        text(out, "{arg-value-2}");
        ctl(out, &tk::ARG_VALUE_CLOSE);
        text(out, "...");
        ctl(out, &tk::TOOL_CALL_CLOSE);
    }

    if !prefix.system.is_empty() {
        ctl(out, &tk::SYSTEM);
        text(out, prefix.system.clone());
    }
}

/// The generation prompt with the reasoning block opened and immediately closed, so the
/// model starts on assistant text instead of thinking - used for the summary turn, which
/// cannot spend its window reasoning. Not part of a render: it would break the append-only property.
pub fn generation_prompt_closing_reasoning() -> Vec<RenderSpan> {
    let mut v = generation_prompt();
    v.push(RenderSpan::Control(tk::THINK_CLOSE.clone()));
    v
}

/// What stands in for a reasoning block the operator stopped.
pub const ABANDONED_REASONING: &str = "[The operator stopped this reasoning before it finished. Its text is kept in the transcript but is not replayed: it was an abandoned draft, not a conclusion. Do not resume it.]";

/// `<|assistant|><think>`: what the model is handed to start speaking.
pub fn generation_prompt() -> Vec<RenderSpan> {
    vec![
        RenderSpan::Control(tk::ASSISTANT.clone()),
        RenderSpan::Control(tk::THINK_OPEN.clone()),
    ]
}

fn ensure_turn(st: &mut State, out: &mut Vec<RenderSpan>) {
    if !st.assistant_open {
        ctl(out, &tk::ASSISTANT);
        st.assistant_open = true;
        st.think = Think::None;
    }
}

fn ensure_think_open(st: &mut State, out: &mut Vec<RenderSpan>) {
    ensure_turn(st, out);
    if st.think == Think::None {
        ctl(out, &tk::THINK_OPEN);
        st.think = Think::Open;
    }
}

fn close_turn(st: &mut State) {
    st.assistant_open = false;
    st.think = Think::None;
}

fn render_items(items: &[TranscriptItem], st: &mut State, out: &mut Vec<RenderSpan>) {
    for item in items {
        render_item(item, st, out);
    }
}

fn render_item(item: &TranscriptItem, st: &mut State, out: &mut Vec<RenderSpan>) {
    match item {
        // Zero-width by contract. Not even a state transition: a segment boundary
        // between two tool results must not split the observation block.
        TranscriptItem::SegmentMark { .. } => {}

        TranscriptItem::System { text: t, .. } => {
            close_turn(st);
            st.prev_was_tool_result = false;
            ctl(out, &tk::SYSTEM);
            text(out, t.clone());
        }

        TranscriptItem::User { parts, .. } => {
            close_turn(st);
            st.prev_was_tool_result = false;
            ctl(out, &tk::USER);
            render_user_parts(parts, out);
        }

        TranscriptItem::Reasoning {
            text: t, truncated, ..
        } => {
            st.prev_was_tool_result = false;
            // A second reasoning block in the same turn is not a thing GLM's grammar
            // has. Treat it as the start of the next turn, which is what it is: the
            // model got a tool result, thought again, and spoke again.
            if st.think == Think::Closed {
                close_turn(st);
            }
            ensure_think_open(st, out);
            // An abandoned draft is not replayed: the store keeps the text, the prompt gets a sentence
            // saying it was stopped, which is more use to the next turn than a thought that was going
            // nowhere.
            text(
                out,
                if *truncated {
                    ABANDONED_REASONING.to_string()
                } else {
                    t.clone()
                },
            );
            ctl(out, &tk::THINK_CLOSE);
            st.think = Think::Closed;
        }

        TranscriptItem::Assistant {
            text: t,
            tool_calls,
            ..
        } => {
            st.prev_was_tool_result = false;
            if st.think != Think::Closed {
                ensure_think_open(st, out);
                ctl(out, &tk::THINK_CLOSE);
                st.think = Think::Closed;
            }
            // `{%- if content.strip() -%}{{ content.strip() }}{%- endif -%}`:
            // assistant content is stripped, user content is not.
            text(out, t.trim().to_string());
            for call in tool_calls {
                ctl(out, &tk::TOOL_CALL_OPEN);
                text(out, call.name.clone());
                for (key, value) in arguments_of(&call.arguments) {
                    ctl(out, &tk::ARG_KEY_OPEN);
                    text(out, key);
                    ctl(out, &tk::ARG_KEY_CLOSE);
                    ctl(out, &tk::ARG_VALUE_OPEN);
                    text(out, arg_value_text(&value));
                    ctl(out, &tk::ARG_VALUE_CLOSE);
                }
                ctl(out, &tk::TOOL_CALL_CLOSE);
            }
        }

        TranscriptItem::ToolResult {
            outcome,
            payload,
            media,
            ..
        } => {
            close_turn(st);
            if !st.prev_was_tool_result {
                ctl(out, &tk::OBSERVATION);
            }
            st.prev_was_tool_result = true;
            ctl(out, &tk::TOOL_RESPONSE_OPEN);
            // `payload` verbatim. It is already the envelope the tool runtime
            // rendered (`ToolRuntime::transcript_item`), and adding a second one
            // here wraps the model's result twice - see `outcome_envelope`.
            let _ = outcome;
            text(out, payload.clone());
            // The picture goes where the reading happened: a tool result that read an image carries it,
            // and without this the bytes reach the transcript and stop there. GLM's own three tokens, so
            // the marker is one span each rather than text.
            if media.is_some() {
                ctl(out, &tk::BEGIN_OF_IMAGE);
                ctl(out, &tk::IMAGE);
                ctl(out, &tk::END_OF_IMAGE);
            }
            ctl(out, &tk::TOOL_RESPONSE_CLOSE);
        }
    }
}

/// A tool call's arguments, in the order the model emitted them: key order is prompt bytes,
/// so the map is walked, not sorted. Anything that is not a JSON object renders no argument
/// pairs - inventing a key would put bytes in the prompt no model produced.
fn arguments_of(arguments: &str) -> Vec<(String, Value)> {
    match serde_json::from_str::<Value>(arguments) {
        Ok(Value::Object(map)) => map.into_iter().collect(),
        _ => Vec::new(),
    }
}

/// Superseded by `letibot_tools::ToolResult::render` and off the render path; kept because
/// the fixture corpora build a payload with it. Running both wraps one result in two
/// envelopes, which is why the renderer emits `payload` verbatim.
pub fn outcome_envelope(outcome: &ToolOutcome, payload: &str) -> String {
    let head = match outcome {
        ToolOutcome::Ok => return payload.to_string(),
        ToolOutcome::Abstained { reason } => {
            format!("NO_RESULT\noutcome: abstained\nreason: {reason}")
        }
        ToolOutcome::Failed { reason } => format!("NO_RESULT\noutcome: failed\nreason: {reason}"),
        ToolOutcome::Denied { req_id } => {
            format!("NO_RESULT\noutcome: denied\nrequest: {req_id}")
        }
        ToolOutcome::Timeout => "NO_RESULT\noutcome: timeout".to_string(),
        ToolOutcome::NotRun { why } => format!("NO_RESULT\noutcome: not_run\nreason: {why}"),
        // Not `NO_RESULT`: the other five non-`Ok` classes say there is nothing here to build on,
        // and this one says the opposite - the work is still happening and the handle reaches it.
        // Sharing the envelope would put "still running" in "abandoned"'s class.
        ToolOutcome::Backgrounded {
            handle,
            ran_for_ms,
            how,
            next,
        } => format!(
            "STILL_RUNNING\noutcome: backgrounded\njob: {handle}\nran in the \
             foreground for: {ran_for_ms} ms\nhow: {}\nnext: {next}",
            how.phrasing()
        ),
    };
    if payload.is_empty() {
        head
    } else {
        format!("{head}\n{payload}")
    }
}

/// User content parts, joined the way the server joins them before the template runs: a
/// newline between text parts, and no newline on either side of a media marker.
fn render_user_parts(parts: &[UserPart], out: &mut Vec<RenderSpan>) {
    let mut pending = String::new();
    let mut any_text_yet = false;
    let mut last_was_media = false;

    let flush = |pending: &mut String, out: &mut Vec<RenderSpan>| {
        if !pending.is_empty() {
            out.push(RenderSpan::Text(std::mem::take(pending)));
        }
    };

    for part in parts {
        match part {
            UserPart::Text { text: t } => {
                if !last_was_media && any_text_yet {
                    pending.push('\n');
                }
                pending.push_str(t);
                any_text_yet = any_text_yet || !t.is_empty();
                last_was_media = false;
            }
            // The harness is expected to have resolved a file reference into text or
            // an image before rendering. The path is a placeholder so that an
            // unresolved one is visible in the prompt rather than silently dropped.
            UserPart::FileRef { path, .. } => {
                if !last_was_media && any_text_yet {
                    pending.push('\n');
                }
                pending.push_str(path);
                any_text_yet = true;
                last_was_media = false;
            }
            UserPart::Image { .. } => {
                flush(&mut pending, out);
                ctl(out, &tk::BEGIN_OF_IMAGE);
                ctl(out, &tk::IMAGE);
                ctl(out, &tk::END_OF_IMAGE);
                last_was_media = true;
            }
        }
    }
    flush(&mut pending, out);
}

/// True when the transcript ends with an assistant turn already open - i.e. no
/// generation prompt should be appended, and the oracle must be asked with
/// `add_generation_prompt: false`.
pub fn ends_mid_turn(items: &[TranscriptItem]) -> bool {
    let mut st = State::default();
    let mut discard = Vec::new();
    render_items(items, &mut st, &mut discard);
    st.assistant_open
}

/// Something about this transcript that GLM's template cannot render faithfully.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Anomaly {
    /// Tool results appear in an order other than the order of the calls they answer. The
    /// shipped template sorts them by tool-call order; we render transcript order, because a
    /// sort would have to insert before bytes already in the KV cache. Append in call order.
    ToolResultsOutOfCallOrder { at: usize },
    /// Two tool calls in one turn share an id. The template's sort gives up on this
    /// too, so it is not a divergence - but it makes a result unattributable.
    DuplicateToolCallId { at: usize, id: String },
    /// `ToolCall.arguments` is not a JSON object, so it renders no argument pairs.
    ToolCallArgumentsNotAnObject { at: usize, name: String },
}

/// Report transcript shapes that cannot be rendered faithfully: declarative on purpose, so
/// this says what is wrong and the caller decides. A free function because every anomaly is
/// a property of GLM's template, so it applies to whoever renders it.
pub fn check_transcript(items: &[TranscriptItem]) -> Vec<Anomaly> {
    let mut out = Vec::new();
    let mut last_calls: Vec<String> = Vec::new();
    let mut block_ids: Vec<(usize, String)> = Vec::new();

    let finish_block =
        |block: &mut Vec<(usize, String)>, calls: &[String], out: &mut Vec<Anomaly>| {
            if block.is_empty() {
                return;
            }
            let positions: Vec<Option<usize>> = block
                .iter()
                .map(|(_, id)| calls.iter().position(|c| c == id))
                .collect();
            if positions.iter().all(Option::is_some) {
                let p: Vec<usize> = positions.into_iter().map(Option::unwrap).collect();
                if p.windows(2).any(|w| w[0] > w[1]) {
                    out.push(Anomaly::ToolResultsOutOfCallOrder { at: block[0].0 });
                }
            }
            block.clear();
        };

    for (i, item) in items.iter().enumerate() {
        match item {
            TranscriptItem::SegmentMark { .. } => {}
            TranscriptItem::ToolResult { call_id, .. } => block_ids.push((i, call_id.clone())),
            TranscriptItem::Assistant { tool_calls, .. } => {
                finish_block(&mut block_ids, &last_calls, &mut out);
                let mut seen: Vec<&str> = Vec::new();
                for call in tool_calls {
                    if seen.contains(&call.id.as_str()) {
                        out.push(Anomaly::DuplicateToolCallId {
                            at: i,
                            id: call.id.clone(),
                        });
                    }
                    seen.push(&call.id);
                    if !matches!(
                        serde_json::from_str::<Value>(&call.arguments),
                        Ok(Value::Object(_))
                    ) {
                        out.push(Anomaly::ToolCallArgumentsNotAnObject {
                            at: i,
                            name: call.name.clone(),
                        });
                    }
                }
                if !tool_calls.is_empty() {
                    last_calls = tool_calls.iter().map(|c| c.id.clone()).collect();
                }
            }
            _ => finish_block(&mut block_ids, &last_calls, &mut out),
        }
    }
    finish_block(&mut block_ids, &last_calls, &mut out);
    out
}
