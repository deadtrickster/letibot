//! Transcript items → `RenderSpan`s, following Qwen3.8's shipped jinja.
//!
//! # How this file is organised, and why it can be checked
//!
//! Every string this renderer emits is either **LITERAL** — bytes the template
//! wrote — or **DATA** — bytes that came out of the conversation. That distinction
//! is not commentary: it decides whether a control literal in the output may become
//! a `Control` span.
//!
//! * [`lit`] emits template bytes and **splits out control literals**, because the
//!   training runtime tokenizes them as the single vocab entry. The tools block
//!   contains a literal `<tool_call>` in its worked example, and that occurrence is
//!   one token in training.
//! * [`data`] emits conversation bytes and **never splits**. A user who types
//!   `<tool_call>` gets six ordinary tokens, and no amount of adversarial content
//!   can produce a turn boundary. This is the property `run_gate.py`'s provenance
//!   phase checks, and it is why the two helpers exist rather than one.
//!
//! # The one divergence from the shipped template, declared
//!
//! **Consecutive tool results are merged by the template into a single user turn;
//! we emit one user turn each.**
//!
//! ```text
//! template : <|im_start|>user\n<tool_response>\nA\n</tool_response>\n<tool_response>\nB\n</tool_response><|im_end|>\n
//! ours     : <|im_start|>user\n<tool_response>\nA\n</tool_response><|im_end|>\n<|im_start|>user\n<tool_response>\nB\n</tool_response><|im_end|>\n
//! ```
//!
//! The reason is structural and it is worth stating plainly, because it is the
//! sharpest place the assembled parts did not fit. `Session::append_items` renders
//! **one item at a time** against the history before it, and writes one ledger row
//! per item. To merge, the first result's `<|im_end|>\n` would have to be withheld
//! until something that is not a tool result arrives — and the thing that usually
//! arrives next is the *generation prompt*, which `PromptRenderer::generation_prompt`
//! produces with no access to the history. So merging would require either a
//! renderer that can retract bytes already in the ledger (it cannot: the ledger is
//! append-only, which is the whole design) or a fourth method on the trait.
//!
//! Given the choice, this renderer keeps `render_incremental ≡ render` (§18.1-I3b)
//! and the append-only ledger (I1, I2) and pays with template fidelity on a shape
//! the model still reads correctly — two user turns instead of one. The alternative
//! trades a provable invariant for a formatting nicety. The fidelity gate reports
//! this divergence as a failure rather than being taught to ignore it, which is the
//! right way round: it should keep showing up until the trait grows the hook.

use letibot_dialect::{RenderSpan, StablePrefix};
use letibot_transcript::{ToolCall, ToolOutcome, TranscriptItem, UserPart};
use serde_json::Value;

use crate::json::{parameter_value_text, qwen_tool_json};
use crate::tokens as tk;
use crate::{QWEN_TOKENS, ReasoningEffort};

/// The worked example and the reminder block the template prints under `<tools>`.
/// One string, copied byte for byte, because every byte is prefix.
const CALL_FORMAT: &str = "\n\nIf you choose to call a function ONLY reply in the following format with NO suffix:\n\n<tool_call>\n<function=example_function_name>\n<parameter=example_parameter_1>\nvalue_1\n</parameter>\n<parameter=example_parameter_2>\nThis is the value for the second parameter\nthat can span\nmultiple lines\n</parameter>\n</function>\n</tool_call>\n\n<IMPORTANT>\nReminder:\n- Function calls MUST follow the specified format: an inner <function=...></function> block must be nested within <tool_call></tool_call> XML tags\n- Required parameters MUST be specified\n- You may provide optional reasoning for your function call in natural language BEFORE the function call, but NOT after\n- If there is no function call available, answer the question like normal with your current knowledge and do not tell the user about function calls\n</IMPORTANT>";

const TOOLS_HEAD: &str = "# Tools\n\nYou have access to the following functions:\n\n<tools>";

/// Template bytes. Control literals inside are promoted to `Control` spans, because
/// that is how the training runtime tokenized them.
fn lit(out: &mut Vec<RenderSpan>, s: &str) {
    let mut rest = s;
    while !rest.is_empty() {
        // The earliest control literal, longest match first so `</think>` is not
        // shadowed by a shorter entry that happens to be a prefix.
        let mut best: Option<(usize, &'static str)> = None;
        for t in QWEN_TOKENS {
            let lit: &str = t.literal.as_ref();
            if let Some(at) = rest.find(lit)
                && best.is_none_or(|(b_at, b_lit)| at < b_at || (at == b_at && lit.len() > b_lit.len()))
            {
                // `literal` is a `Cow::Borrowed(&'static str)` for every entry in
                // `QWEN_TOKENS`; the table is a `const` slice.
                let s: &'static str = match &t.literal {
                    std::borrow::Cow::Borrowed(b) => b,
                    std::borrow::Cow::Owned(_) => unreachable!("QWEN_TOKENS is a const table"),
                };
                best = Some((at, s));
            }
        }
        match best {
            None => {
                out.push(RenderSpan::Text(rest.to_string()));
                return;
            }
            Some((at, literal)) => {
                if at > 0 {
                    out.push(RenderSpan::Text(rest[..at].to_string()));
                }
                let token = QWEN_TOKENS
                    .iter()
                    .find(|t| t.literal == literal)
                    .expect("the literal came from this table");
                out.push(RenderSpan::Control(token.clone()));
                rest = &rest[at + literal.len()..];
            }
        }
    }
}

/// Conversation bytes. Never split — see the module header.
fn data(out: &mut Vec<RenderSpan>, s: &str) {
    if !s.is_empty() {
        out.push(RenderSpan::Text(s.to_string()));
    }
}

fn ctl(out: &mut Vec<RenderSpan>, t: &letibot_dialect::ControlToken) {
    out.push(RenderSpan::Control(t.clone()));
}

/// Jinja's `|trim`: Python's `str.strip()`, which strips ASCII whitespace only.
fn trim(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_ascii_whitespace())
}

/// Qwen3.8's renderer.
///
/// Stateless and cheap. The only configuration is [`ReasoningEffort`], which is
/// part of the *stable prefix* rather than of a request — see the crate docs.
#[derive(Debug, Clone, Copy, Default)]
pub struct QwenRenderer {
    effort: ReasoningEffort,
}

impl QwenRenderer {
    pub fn new() -> Self {
        QwenRenderer::default()
    }

    pub fn with_effort(mut self, effort: ReasoningEffort) -> Self {
        self.effort = effort;
        self
    }

    pub fn effort(&self) -> ReasoningEffort {
        self.effort
    }

    /// The whole prompt, with no generation prompt.
    pub fn render(&self, prefix: &StablePrefix, items: &[TranscriptItem]) -> Vec<RenderSpan> {
        let mut out = Vec::new();
        self.render_prefix(prefix, &mut out);
        let mut st = State::default();
        render_items(items, &mut st, &mut out);
        out
    }

    /// The spans to append for `new_items`, given `history` already rendered.
    ///
    /// Replays `history` into a throwaway buffer to recover the boundary state, the
    /// same shape `GlmRenderer` uses. A `usize` offset cannot carry "is an assistant
    /// turn open"; that was CONTRACT-GAP-1.
    pub fn render_incremental(
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

    /// The system turn: reasoning instruction, tools, then the operator's text.
    ///
    /// Note the order — **the tool block comes before the system text**, which is
    /// the opposite of every other template in this repo, and is the kind of thing
    /// that is invisible until a prompt does not cache.
    fn render_prefix(&self, prefix: &StablePrefix, out: &mut Vec<RenderSpan>) {
        let instruction = self.effort.instruction();
        let system = trim(&prefix.system);
        let has_tools = !prefix.tools_json.is_empty();

        if has_tools {
            ctl(out, &tk::IM_START);
            lit(out, "system\n");
            if !instruction.is_empty() {
                lit(out, instruction);
                lit(out, "\n\n");
            }
            lit(out, TOOLS_HEAD);
            for tool in &prefix.tools_json {
                lit(out, "\n");
                // A tool schema is normalised, harness-produced JSON — but it is
                // still not template text, and a description containing
                // `</tool_call>` must not become a boundary. §5.2's rule that the
                // description is part of the prompt cuts both ways.
                data(out, tool);
            }
            lit(out, "\n</tools>");
            lit(out, CALL_FORMAT);
            if !system.is_empty() {
                lit(out, "\n\n");
                data(out, system);
            }
            ctl(out, &tk::IM_END);
            lit(out, "\n");
        } else if !system.is_empty() {
            ctl(out, &tk::IM_START);
            lit(out, "system\n");
            if !instruction.is_empty() {
                lit(out, instruction);
                lit(out, "\n\n");
            }
            data(out, system);
            ctl(out, &tk::IM_END);
            lit(out, "\n");
        } else if !instruction.is_empty() {
            ctl(out, &tk::IM_START);
            lit(out, "system\n");
            lit(out, instruction);
            ctl(out, &tk::IM_END);
            lit(out, "\n");
        }
    }
}

/// Where the renderer is inside an assistant turn.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct State {
    /// An assistant message has been opened and its `<|im_end|>` not yet written.
    assistant_open: bool,
    /// This assistant message has already emitted its `<think>…</think>` block, so
    /// a second `Reasoning` item must start a new message.
    thought: bool,
    /// The open assistant message has visible text, which decides whether the first
    /// tool call is preceded by `\n\n`.
    has_text: bool,
    /// A tool call has already been written in this message, so the next one is not
    /// `loop.first`.
    called: bool,
}

fn close_assistant(st: &mut State, out: &mut Vec<RenderSpan>) {
    if st.assistant_open {
        ctl(out, &tk::IM_END);
        lit(out, "\n");
        *st = State::default();
    }
}

/// Open an assistant message and emit its (possibly empty) thinking block.
///
/// `<think>\n{reasoning}\n</think>\n\n` — the template emits this **unconditionally**
/// for an assistant message, so an assistant turn with no reasoning of its own still
/// carries `<think>\n\n</think>\n\n`. §5.4's "an assistant turn with no visible text
/// must still emit its boundary tokens", one layer up.
fn open_assistant(st: &mut State, out: &mut Vec<RenderSpan>, reasoning: &str) {
    debug_assert!(!st.assistant_open, "the caller decides whether to close first");
    ctl(out, &tk::IM_START);
    lit(out, "assistant\n");
    ctl(out, &tk::THINK_OPEN);
    lit(out, "\n");
    data(out, trim(reasoning));
    lit(out, "\n");
    ctl(out, &tk::THINK_CLOSE);
    lit(out, "\n\n");
    st.assistant_open = true;
    st.thought = true;
}

fn render_items(items: &[TranscriptItem], st: &mut State, out: &mut Vec<RenderSpan>) {
    for item in items {
        match item {
            // Renders to nothing, by contract.
            TranscriptItem::SegmentMark { .. } => {}

            // Unrenderable on this template past position 0: it raises. The daemon
            // is expected to use `system_update_item` instead; rendering it as a
            // user turn here rather than panicking means a transcript loaded from an
            // older store still renders, visibly marked.
            TranscriptItem::System { text, .. } => {
                close_assistant(st, out);
                ctl(out, &tk::IM_START);
                lit(out, "user\n");
                lit(out, "<system-update>\n");
                data(out, trim(text));
                lit(out, "\n</system-update>");
                ctl(out, &tk::IM_END);
                lit(out, "\n");
            }

            TranscriptItem::User { parts } => {
                close_assistant(st, out);
                ctl(out, &tk::IM_START);
                lit(out, "user\n");
                render_user_parts(parts, out);
                ctl(out, &tk::IM_END);
                lit(out, "\n");
            }

            // A message carries exactly one `reasoning_content`, so a second
            // `Reasoning` item is a second message and closes the one before it.
            // An `Assistant` item is **not**: it is the visible half of the message
            // the reasoning already opened, and closing here would split one turn
            // into two — which the oracle caught on the first run of this corpus,
            // and which no unit test in this file would have.
            TranscriptItem::Reasoning { text, .. } => {
                close_assistant(st, out);
                open_assistant(st, out, text);
            }

            TranscriptItem::Assistant { text, tool_calls, .. } => {
                if !st.assistant_open {
                    open_assistant(st, out, "");
                }
                let body = trim(text);
                data(out, body);
                st.has_text = st.has_text || !body.is_empty();
                for call in tool_calls {
                    render_tool_call(call, st, out);
                }
                // An assistant item is the end of its message: the next Reasoning
                // opens a new one, and a User or ToolResult closes this one.
            }

            TranscriptItem::ToolResult {
                outcome, payload, ..
            } => {
                close_assistant(st, out);
                ctl(out, &tk::IM_START);
                lit(out, "user");
                lit(out, "\n");
                ctl(out, &tk::TOOL_RESPONSE_OPEN);
                lit(out, "\n");
                // `payload` verbatim: it is already `ToolResult::render`'s envelope
                // when the harness produced it. See `outcome_envelope`.
                let _ = outcome;
                data(out, trim(payload));
                lit(out, "\n");
                ctl(out, &tk::TOOL_RESPONSE_CLOSE);
                ctl(out, &tk::IM_END);
                lit(out, "\n");
            }
        }
    }
}

/// One `<tool_call>` block, in the XML-ish form this model was trained on.
///
/// Not JSON. That is the single most consequential difference from Qwen3 and from
/// the ChatML test fixture, and it is why this crate exists.
fn render_tool_call(call: &ToolCall, st: &mut State, out: &mut Vec<RenderSpan>) {
    if !st.called && st.has_text {
        lit(out, "\n\n");
    } else if st.called {
        lit(out, "\n");
    }
    st.called = true;

    ctl(out, &tk::TOOL_CALL_OPEN);
    lit(out, "\n<function=");
    data(out, &call.name);
    lit(out, ">\n");
    for (key, value) in arguments_of(&call.arguments) {
        lit(out, "<parameter=");
        data(out, &key);
        lit(out, ">\n");
        data(out, &parameter_value_text(&value));
        lit(out, "\n</parameter>\n");
    }
    lit(out, "</function>\n");
    ctl(out, &tk::TOOL_CALL_CLOSE);
}

/// A tool call's arguments as ordered key/value pairs.
///
/// Order is `preserve_order`'s, i.e. the order the model wrote them, because those
/// bytes are replayed into the next prompt. Arguments that do not parse as a JSON
/// object render as no parameters at all rather than as a guess — §5.7 has to be
/// able to tell a truncated argument from an empty one, and inventing structure
/// here would erase the difference.
fn arguments_of(arguments: &str) -> Vec<(String, Value)> {
    match serde_json::from_str::<Value>(arguments) {
        Ok(Value::Object(map)) => map.into_iter().collect(),
        _ => Vec::new(),
    }
}

/// User content parts, joined the way the server joins them before the template
/// runs: a newline between text parts, none around a media marker.
fn render_user_parts(parts: &[UserPart], out: &mut Vec<RenderSpan>) {
    // The template applies `|trim` to the whole joined content, so the join has to
    // happen before the trim rather than per part.
    let mut buf = String::new();
    let mut media: Vec<usize> = Vec::new();
    for part in parts {
        match part {
            UserPart::Text { text } => {
                if !buf.is_empty() {
                    buf.push('\n');
                }
                buf.push_str(text);
            }
            UserPart::FileRef { path, .. } => {
                if !buf.is_empty() {
                    buf.push('\n');
                }
                buf.push_str(path);
            }
            UserPart::Image { .. } => media.push(buf.len()),
        }
    }
    let trimmed = trim(&buf).to_string();

    // With no image this is the whole story and the common case.
    if media.is_empty() {
        data(out, &trimmed);
        return;
    }
    // With one, the template emits `<|vision_start|><|image_pad|><|vision_end|>`
    // where the part sat. Text on either side is still one trimmed run, so the
    // markers go at the front — which is where a harness that resolved a file into
    // an image puts it anyway. This is the shape the gate cannot check against the
    // server at all (§dialect-glm, point 3), so it is kept minimal on purpose.
    for _ in &media {
        ctl(out, &tk::VISION_START);
        ctl(out, &tk::IMAGE_PAD);
        ctl(out, &tk::VISION_END);
    }
    data(out, &trimmed);
}

/// What the model is handed to start speaking: `<|im_start|>assistant\n<think>\n`.
///
/// The trailing newline is the template's, and it is load-bearing: the model was
/// trained to begin its reasoning on the line after `<think>`. Dropping it is a
/// one-token divergence that costs a cold prefill on every turn.
pub fn generation_prompt() -> Vec<RenderSpan> {
    vec![
        RenderSpan::Control(tk::IM_START.clone()),
        RenderSpan::Text("assistant\n".into()),
        RenderSpan::Control(tk::THINK_OPEN.clone()),
        RenderSpan::Text("\n".into()),
    ]
}

/// True when the transcript ends with an assistant turn still open, so no
/// generation prompt may be appended.
pub fn ends_mid_turn(items: &[TranscriptItem]) -> bool {
    let mut st = State::default();
    let mut discard = Vec::new();
    render_items(items, &mut st, &mut discard);
    st.assistant_open
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
        ToolOutcome::Denied { req_id } => format!("NO_RESULT\noutcome: denied\nrequest: {req_id}"),
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

/// The tool schemas of a registry as Qwen's `<tools>` lines.
pub fn tools_json(schemas: &[Value]) -> Vec<String> {
    schemas.iter().map(qwen_tool_json).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use letibot_dialect::spans_to_string;
    use letibot_transcript::ReasoningField;
    use serde_json::json;

    fn user(text: &str) -> TranscriptItem {
        TranscriptItem::User {
            parts: vec![UserPart::Text { text: text.into() }],
        }
    }

    fn prefix(system: &str, tools: &[Value]) -> StablePrefix {
        StablePrefix {
            system: system.into(),
            tools_json: tools_json(tools),
        }
    }

    #[test]
    fn the_system_turn_matches_the_template_with_no_tools() {
        let r = QwenRenderer::new();
        let got = spans_to_string(&r.render(&prefix("Be terse.", &[]), &[]));
        assert_eq!(
            got,
            format!(
                "<|im_start|>system\n{}\n\nBe terse.<|im_end|>\n",
                ReasoningEffort::Xhigh.instruction()
            )
        );
    }

    #[test]
    fn medium_effort_emits_no_instruction_and_no_blank_line() {
        let r = QwenRenderer::new().with_effort(ReasoningEffort::Medium);
        assert_eq!(
            spans_to_string(&r.render(&prefix("Be terse.", &[]), &[])),
            "<|im_start|>system\nBe terse.<|im_end|>\n"
        );
    }

    #[test]
    fn the_system_text_comes_after_the_tool_block() {
        // The thing that is invisible until a prompt does not cache.
        let tools = vec![json!({"type": "function", "function": {"name": "read"}})];
        let got = spans_to_string(&QwenRenderer::new().render(&prefix("Be terse.", &tools), &[]));
        let tools_at = got.find("<tools>").unwrap();
        let system_at = got.find("Be terse.").unwrap();
        assert!(tools_at < system_at, "{got}");
        assert!(got.contains("\n\nBe terse.<|im_end|>\n"));
    }

    #[test]
    fn a_tool_call_is_xml_not_json() {
        let items = vec![
            TranscriptItem::Reasoning {
                text: "Look at it.".into(),
                field: ReasoningField::Inline,
            },
            TranscriptItem::Assistant {
                text: String::new(),
                tool_calls: vec![ToolCall {
                    id: "call_0".into(),
                    name: "read".into(),
                    arguments: r#"{"path":"a.txt","limit":40}"#.into(),
                }],
                truncated: false,
            },
        ];
        let got = spans_to_string(&QwenRenderer::new().render(&prefix("s", &[]), &items));
        assert!(got.ends_with(
            "<|im_start|>assistant\n<think>\nLook at it.\n</think>\n\n\
             <tool_call>\n<function=read>\n\
             <parameter=path>\na.txt\n</parameter>\n\
             <parameter=limit>\n40\n</parameter>\n\
             </function>\n</tool_call>"
        ), "{got}");
        // And nothing that looks like Qwen3's JSON form.
        assert!(!got.contains(r#"{"name":"#));
    }

    #[test]
    fn text_before_a_call_gets_the_blank_line_the_template_gives_it() {
        let items = vec![TranscriptItem::Assistant {
            text: "Reading it now.".into(),
            tool_calls: vec![ToolCall {
                id: "call_0".into(),
                name: "read".into(),
                arguments: r#"{"path":"a"}"#.into(),
            }],
            truncated: false,
        }];
        let got = spans_to_string(&QwenRenderer::new().render(&prefix("s", &[]), &items));
        assert!(got.contains("Reading it now.\n\n<tool_call>"), "{got}");
    }

    #[test]
    fn an_assistant_turn_with_no_reasoning_still_emits_an_empty_think_block() {
        let items = vec![TranscriptItem::Assistant {
            text: "4.".into(),
            tool_calls: vec![],
            truncated: false,
        }];
        let got = spans_to_string(&QwenRenderer::new().render(&prefix("s", &[]), &items));
        assert!(got.ends_with("<|im_start|>assistant\n<think>\n\n</think>\n\n4."), "{got}");
    }

    #[test]
    fn render_incremental_equals_render_at_every_prefix() {
        // §18.1-I3b, over a conversation that exercises every item kind this
        // renderer has a branch for.
        let items = [
            user("Compare a and b."),
            TranscriptItem::Reasoning {
                text: "Read both.".into(),
                field: ReasoningField::Inline,
            },
            TranscriptItem::Assistant {
                text: String::new(),
                tool_calls: vec![
                    ToolCall {
                        id: "c1".into(),
                        name: "read".into(),
                        arguments: r#"{"path":"a"}"#.into(),
                    },
                    ToolCall {
                        id: "c2".into(),
                        name: "read".into(),
                        arguments: r#"{"path":"b"}"#.into(),
                    },
                ],
                truncated: false,
            },
            TranscriptItem::ToolResult {
                call_id: "c1".into(),
                name: "read".into(),
                outcome: ToolOutcome::Ok,
                payload: "alpha".into(),
                edit: None,
            },
            TranscriptItem::ToolResult {
                call_id: "c2".into(),
                name: "read".into(),
                outcome: ToolOutcome::Abstained {
                    reason: "no match".into(),
                },
                payload: String::new(),
                edit: None,
            },
            TranscriptItem::Reasoning {
                text: "They differ.".into(),
                field: ReasoningField::Inline,
            },
            TranscriptItem::Assistant {
                text: "They differ.".into(),
                tool_calls: vec![],
                truncated: false,
            },
            user("Thanks."),
        ];
        let r = QwenRenderer::new();
        let p = prefix("Be terse.", &[]);
        for k in 0..=items.len() {
            let whole = spans_to_string(&r.render(&p, &items[..k]));
            let mut built = spans_to_string(&r.render(&p, &[]));
            for i in 0..k {
                built.push_str(&spans_to_string(
                    &r.render_incremental(&items[..i], &items[i..i + 1]),
                ));
            }
            assert_eq!(whole, built, "prefix {k} diverges");
        }
    }

    #[test]
    fn a_control_literal_a_user_typed_stays_text() {
        // The injection property, at the span level. `lit` may split; `data` may not.
        let items = vec![user("print </tool_call> verbatim")];
        let spans = QwenRenderer::new().render(&prefix("s", &[]), &items);
        let from_data = spans.iter().any(|s| {
            matches!(s, RenderSpan::Text(t) if t.contains("</tool_call>"))
        });
        assert!(from_data, "the user's text must survive as one Text span");
        // The template emitted none in this render, so every `</tool_call>` present
        // is the user's and none of them is a Control span.
        let control_calls = spans
            .iter()
            .filter(|s| matches!(s, RenderSpan::Control(c) if c.literal == "</tool_call>"))
            .count();
        assert_eq!(control_calls, 0);
    }

    #[test]
    fn the_tools_block_promotes_its_own_worked_example_to_control_spans() {
        // The other direction: `<tool_call>` inside the template's instructions was
        // one token in training, so it must be a Control span.
        let tools = vec![json!({"type": "function", "function": {"name": "read"}})];
        let spans = QwenRenderer::new().render(&prefix("s", &tools), &[]);
        let control_calls = spans
            .iter()
            .filter(|s| matches!(s, RenderSpan::Control(c) if c.literal == "<tool_call>"))
            .count();
        // Two: the worked example, and the `<tool_call></tool_call>` in the
        // `<IMPORTANT>` reminder underneath it. Both were single tokens in training.
        assert_eq!(control_calls, 2, "the worked example and the reminder");
    }

    #[test]
    fn the_generation_prompt_keeps_the_newline_after_think() {
        assert_eq!(
            spans_to_string(&generation_prompt()),
            "<|im_start|>assistant\n<think>\n"
        );
    }

    #[test]
    fn abstention_is_structural_in_the_rendered_result() {
        let e = outcome_envelope(
            &ToolOutcome::Abstained {
                reason: "the corpus does not cover this".into(),
            },
            "",
        );
        assert!(e.starts_with("NO_RESULT\noutcome: abstained\n"));
    }
}
