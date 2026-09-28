//! The second renderer: hand-written, demoted, kept as a differential.
//!
//! Every rule here was measured against `POST /apply-template` and is now gated
//! against CPython Jinja2 (`tests/fidelity/`). T1 replaced it as the *production*
//! renderer — the shipped template run through minijinja does the same job for every
//! model rather than for one — but two independent implementations that agree on 139
//! cases are stronger evidence than one implementation that passes its own tests, and
//! that is the job this file now has.
//!
//! It renders one thing only: the training format, as CPython Jinja2 produces it. The
//! `server-bug-compatible` profile that used to live here — reproducing minja's
//! cross-iteration `{% set %}` leak — is gone (T2). It cannot be produced by a
//! template-driven renderer without deliberately reintroducing someone else's bug, and
//! the thing it was evidence for is better served by running the real template through
//! a correct engine.

use crate::ReasoningEffort;
use crate::json::arg_value_text;
use crate::tokens as tk;
use letibot_dialect::{ControlToken, RenderSpan, StablePrefix};
use letibot_transcript::{ToolOutcome, TranscriptItem, UserPart};
use serde_json::Value;

/// GLM's hand-written renderer.
///
/// A pure function of `(effort, prefix, items)`, and now visibly so: the builder that
/// carried a conversation around so that `render_incremental`'s trait signature could
/// pretend to be stateless is gone with the trait. Appending takes the history as an
/// argument, which is what it always needed.
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

    /// The spans to **append** to a render of `history`, for `new_items`.
    ///
    /// Must agree with [`GlmRenderer::render`]: rendering `history + new` from scratch
    /// equals rendering `history` and appending this. The whole append-only design
    /// rests on it, and because it is a pure function it is property-testable
    /// exhaustively with no model present (`tests/invariants.rs`).
    ///
    /// `history` is the transcript already rendered, in full. It is not a `usize`,
    /// because GLM's boundary state — is an assistant turn open, was `<think>` already
    /// emitted, was the previous item a tool result — is not derivable from one. That
    /// was CONTRACT-GAP-1, and the fix is an argument rather than a builder that
    /// panicked when the caller forgot.
    ///
    /// The stable prefix is never re-emitted here: it is not an item, so
    /// `render(prefix, &[])` owns it and `render_incremental(&[], items)` starts at
    /// item 0.
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

/// Where inside an assistant turn the renderer currently is.
///
/// `Open` means `<think>` has been emitted with nothing in it yet — which is exactly
/// the state a generation prompt leaves behind, and why the append-only property
/// survives a turn that starts mid-`<think>`.
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

/// The head of every GLM prompt: `[gMASK]<sop>`, the reasoning-effort line, the tool
/// block, then the bootstrap system message.
///
/// This is the whole of what the server's prefix cache is keyed on, and the order is
/// not ours to choose: the template emits the effort line and the tool block before
/// it enters the message loop, so a bootstrap system message is *after* both.
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

/// `<|assistant|><think>` — what the model is handed to start speaking.
///
/// Deliberately **not** part of a render. A generation prompt is not a transcript
/// item, and folding it in would break `render_incremental ≡ render` for every
/// conversation whose next item is a user message: the appended bytes would have to be
/// un-appended first. The template-driven renderer says the same thing with
/// `add_generation_prompt`, which is where the contract now leaves it.
/// [`generation_prompt`] with the reasoning block opened and immediately closed.
///
/// The model is handed `<think></think>` rather than an open `<think>`, so it
/// starts on assistant text instead of reasoning. Used for the summary turn,
/// which has to fit in whatever a full context window has left -- 1754 tokens,
/// the day this was written -- and cannot spend it thinking.
///
/// `lead_opens_reasoning` reads the lead the engine is about to submit, so
/// nothing downstream has to be told which of the two it got.
pub fn generation_prompt_closing_reasoning() -> Vec<RenderSpan> {
    let mut v = generation_prompt();
    v.push(RenderSpan::Control(tk::THINK_CLOSE.clone()));
    v
}

/// What stands in for a reasoning block the operator stopped.
pub const ABANDONED_REASONING: &str = "[The operator stopped this reasoning before it finished. Its text is kept in the transcript but is not replayed: it was an abandoned draft, not a conclusion. Do not resume it.]";

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
            // **An abandoned draft is not replayed** -- see `ABANDONED_REASONING`.
            // The store keeps the text; the prompt gets a sentence saying it was
            // stopped, which is more use to the next turn than a thought that was
            // going nowhere and was killed for it.
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
            // here wraps the model's result twice — see `outcome_envelope`.
            let _ = outcome;
            text(out, payload.clone());
            // **And the picture, where the reading happened** — the same arm the qwen renderer
            // gained, and for the same reason: a tool result that read an image carries it, and
            // without this the bytes reached the transcript and stopped there. GLM's own three
            // tokens, because this dialect spells them `<|begin_of_image|>`/`<|image|>`/
            // `<|end_of_image|>` and `ControlRole` is what the string form collapses.
            if media.is_some() {
                ctl(out, &tk::BEGIN_OF_IMAGE);
                ctl(out, &tk::IMAGE);
                ctl(out, &tk::END_OF_IMAGE);
            }
            ctl(out, &tk::TOOL_RESPONSE_CLOSE);
        }
    }
}

/// A tool call's arguments, in the order the model emitted them.
///
/// Key order is prompt bytes, so `serde_json` is built with `preserve_order` and the
/// map is walked, not sorted. Arguments that are not a JSON object render **no**
/// argument pairs: the shipped template iterates `_args.items()` and would fail on
/// anything else, and inventing a key here would put bytes in the prompt that no
/// model ever produced. `check_transcript` reports it instead; salvaging malformed
/// tool-call text is W6's job, at the point where it enters the transcript.
fn arguments_of(arguments: &str) -> Vec<(String, Value)> {
    match serde_json::from_str::<Value>(arguments) {
        Ok(Value::Object(map)) => map.into_iter().collect(),
        _ => Vec::new(),
    }
}

/// **Superseded by `letibot_tools::ToolResult::render`, and no longer on the render
/// path.** Kept because the fixture corpora build a payload with it and because the
/// wording is the reference for what §8.2 asks of an envelope.
///
/// The original note on this function said *"W9 owns the final wording"*. W9 landed,
/// and it owns more than the wording: `ToolResult::render` produces the whole
/// envelope — a call-id-derived mark so two results in one turn cannot be confused,
/// the `[repaired]` and `[note]` lines, the spill notice, and the sentence saying
/// nothing inside it may be cited as an answer — and `ToolRuntime::transcript_item`
/// puts that string in `payload` precisely because it is *"the byte sequence the
/// next prompt replays"*.
///
/// T17 found out what happens when both fire: the model is handed two envelopes
/// around one result, the outer one thinner than the inner. Neither crate is wrong
/// on its own; nobody had run them together. The renderer now emits `payload`
/// verbatim, which is what a renderer should do with bytes the harness has already
/// decided on.
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
        // **Not `NO_RESULT`.** The other five non-`Ok` classes all say the same
        // thing at bottom — there is nothing here to build on — and this one says
        // the opposite: the work is still happening and the handle reaches it. A
        // shared envelope would put "still running" in the same visual class as
        // "abandoned", which is exactly the reading `ToolOutcome::Timeout` already
        // means and this variant exists not to be.
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

/// User content parts, joined the way the server joins them before the template runs.
///
/// `concat_content_parts` (`common/chat.cpp`): a newline between text parts, and
/// **no** newline on either side of a media marker.
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

/// True when the transcript ends with an assistant turn already open — i.e. no
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
    /// Tool results appear in an order other than the order of the calls they
    /// answer. The shipped template **sorts** them by tool-call order; we render them
    /// in transcript order, because sorting would mean an appended result could have
    /// to be inserted before bytes already in the KV cache. Append them in call order
    /// and the two agree.
    ToolResultsOutOfCallOrder { at: usize },
    /// Two tool calls in one turn share an id. The template's sort gives up on this
    /// too, so it is not a divergence — but it makes a result unattributable.
    DuplicateToolCallId { at: usize, id: String },
    /// `ToolCall.arguments` is not a JSON object, so it renders no argument pairs.
    ToolCallArgumentsNotAnObject { at: usize, name: String },
}

/// Report transcript shapes that cannot be rendered faithfully.
///
/// Declarative on purpose, in the same spirit as `Guard`: this says what is wrong, the
/// caller decides. Empty means the render will match the shipped template.
///
/// A free function, not a renderer method: every anomaly here is a property of GLM's
/// *template* — its sort, its `.items()` call — so it applies to whoever renders it.
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
