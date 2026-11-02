//! Transcript items to `RenderSpan`s, following Qwen3.8's shipped jinja.
//!
//! Every string this renderer emits is either literal, bytes the template wrote, or data, bytes that
//! came out of the conversation. The distinction decides whether a control literal in the output may
//! become a `Control` span.
//!
//! [`lit`] emits template bytes and splits out control literals, because the training runtime
//! tokenizes them as the single vocab entry. [`data`] emits conversation bytes and never splits: a
//! user who types `<tool_call>` gets six ordinary tokens, and no adversarial content can produce a
//! turn boundary. That is why the two helpers exist rather than one.
//!
//! The one divergence from the shipped template is declared rather than hidden: it merges
//! consecutive tool results into a single user turn and we emit one user turn each.
//!
//! ```text
//! template : <|im_start|>user\n<tool_response>\nA\n</tool_response>\n<tool_response>\nB\n</tool_response><|im_end|>\n
//! ours     : <|im_start|>user\n<tool_response>\nA\n</tool_response><|im_end|>\n<|im_start|>user\n<tool_response>\nB\n</tool_response><|im_end|>\n
//! ```
//!
//! The reason is structural. A session renders one item at a time against the history before it and
//! writes one ledger row per item, so merging would need the first result's closing token withheld
//! until something that is not a tool result arrives, and what usually arrives next is the generation
//! prompt, produced with no access to the history. Either the renderer retracts bytes already in the
//! ledger, which an append-only ledger cannot, or the trait grows a method.
//!
//! So this keeps incremental rendering equal to a full render and pays with template fidelity on a
//! shape the model still reads correctly. The fidelity gate reports the divergence as a failure
//! rather than being taught to ignore it, which is the right way round: it should keep showing up
//! until the trait grows the hook.

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
                && best.is_none_or(|(b_at, b_lit)| {
                    at < b_at || (at == b_at && lit.len() > b_lit.len())
                })
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

/// Conversation bytes. Never split; see the module header.
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

/// Qwen3.8's renderer. Stateless and cheap, and its only configuration is [`ReasoningEffort`], which
/// is part of the stable prefix rather than of a request.
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

    /// The spans to append for `new_items`, given `history` already rendered: the history is replayed
    /// into a throwaway buffer to recover the boundary state, because an offset cannot say whether an
    /// assistant turn is open.
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

    /// The system turn: reasoning instruction, tools, then the operator's text. The tool block comes
    /// before the system text, which is the opposite of every other template here and invisible until
    /// a prompt stops caching.
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
                // A tool schema is JSON the harness produced, but still not template text: a
                // description containing `</tool_call>` must not become a boundary.
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

/// Open an assistant message and emit its possibly empty thinking block: the template emits this
/// unconditionally, so a turn with no reasoning still carries its boundary tokens.
fn open_assistant(st: &mut State, out: &mut Vec<RenderSpan>, reasoning: &str) {
    debug_assert!(
        !st.assistant_open,
        "the caller decides whether to close first"
    );
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

            // Unrenderable past position 0 on this template, which raises. The daemon is expected to
            // use `system_update_item`; rendering it as a user turn means an older transcript still
            // renders, visibly marked.
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

            TranscriptItem::User { parts, .. } => {
                close_assistant(st, out);
                ctl(out, &tk::IM_START);
                lit(out, "user\n");
                render_user_parts(parts, out);
                ctl(out, &tk::IM_END);
                lit(out, "\n");
            }

            // A message carries one `reasoning_content`, so a second reasoning item opens a second
            // message. An assistant item does not: it is the visible half of the message the reasoning
            // opened, and closing here would split one turn into two.
            TranscriptItem::Reasoning {
                text, truncated, ..
            } => {
                close_assistant(st, out);
                // An abandoned draft is not replayed: the store keeps it, but the prompt gets a
                // sentence instead, because a model that reads its own killed loop starts it again.
                open_assistant(
                    st,
                    out,
                    if *truncated {
                        ABANDONED_REASONING
                    } else {
                        text
                    },
                );
            }

            TranscriptItem::Assistant {
                text, tool_calls, ..
            } => {
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
                outcome,
                payload,
                media,
                ..
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
                // And the picture, where the reading happened: the payload says what was read and
                // the image is the thing itself, in the order `read` produces them.
                if media.is_some() {
                    ctl(out, &tk::VISION_START);
                    ctl(out, &tk::IMAGE_PAD);
                    ctl(out, &tk::VISION_END);
                }
                lit(out, "\n");
                ctl(out, &tk::TOOL_RESPONSE_CLOSE);
                ctl(out, &tk::IM_END);
                lit(out, "\n");
            }
        }
    }
}

/// One `<tool_call>` block, in the XML-ish form this model was trained on. Not JSON, which is the
/// most consequential difference from Qwen3 and from the ChatML fixture.
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

/// A tool call's arguments as ordered key/value pairs, in the order the model wrote them, because
/// those bytes are replayed. Arguments that do not parse render as no parameters rather than a guess.
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
    // With one, the template emits the three vision tokens where the part sat, so the markers go at
    // the front of the trimmed run. The gate cannot check this against the server, so it stays minimal.
    for _ in &media {
        ctl(out, &tk::VISION_START);
        ctl(out, &tk::IMAGE_PAD);
        ctl(out, &tk::VISION_END);
    }
    data(out, &trimmed);
}

/// What the model is handed to start speaking: `<|im_start|>assistant\n<think>\n`. The trailing
/// newline is the template's and load-bearing, since the model was trained to begin its reasoning on
/// the next line.
pub fn generation_prompt_closing_reasoning() -> Vec<RenderSpan> {
    let mut v = generation_prompt();
    v.push(RenderSpan::Control(tk::THINK_CLOSE.clone()));
    v
}

/// What stands in for a reasoning block the operator stopped.
pub const ABANDONED_REASONING: &str = "[The operator stopped this reasoning before it finished. Its text is kept in the transcript but is not replayed: it was an abandoned draft, not a conclusion. Do not resume it.]";

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

/// Superseded by `letibot_tools::ToolResult::render` and no longer on the render path: the fixture
/// corpora still build a payload with it, and its wording is the reference for the envelope.
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
        // Not `NO_RESULT`: the other non-`Ok` classes say there is nothing to build on, and this one
        // says the work is still happening and the handle reaches it.
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
            speaker: Default::default(),
            parts: vec![UserPart::Text { text: text.into() }],
        }
    }

    fn prefix(system: &str, tools: &[Value]) -> StablePrefix {
        StablePrefix {
            system: system.into(),
            tools_json: tools_json(tools),
        }
    }

    /// Every image the transcript holds is placed exactly once: the markers a renderer places and
    /// the payloads the request carries must line up, since the server substitutes at the markers
    /// in order.
    #[test]
    fn every_image_the_transcript_holds_is_placed_exactly_once() {
        use letibot_dialect::media_spans;
        use letibot_transcript::media::{Media, media_in_order};

        let png = Media {
            mime: "image/png".into(),
            bytes: 11,
            width: Some(2),
            height: Some(2),
            data_ref: "data:image/png;base64,AAAA".into(),
            delivered: false,
        };
        // A tool result that read a picture — the `read` path.
        let read_one = TranscriptItem::ToolResult {
            call_id: "c1".into(),
            name: "read".into(),
            outcome: letibot_transcript::ToolOutcome::Ok,
            payload: "shot.png — image image/png 2×2 · 1 KiB".into(),
            edit: None,
            origin: None,
            media: Some(png.clone()),
        };
        // And a result that read a file, which is every other call.
        let read_text = TranscriptItem::ToolResult {
            call_id: "c2".into(),
            name: "read".into(),
            outcome: letibot_transcript::ToolOutcome::Ok,
            payload: "fn main() {}".into(),
            edit: None,
            origin: None,
            media: None,
        };
        // **A user row carrying two images** — the head-attached path, and the case a single-image
        // fixture cannot catch: the second marker is the one a `once` bug would drop.
        let attached = TranscriptItem::User {
            speaker: Default::default(),
            parts: vec![
                UserPart::Text {
                    text: "what are these?".into(),
                },
                UserPart::Image {
                    media_type: "image/png".into(),
                    data_ref: "data:image/png;base64,BBBB".into(),
                },
                UserPart::Image {
                    media_type: "image/png".into(),
                    data_ref: "data:image/png;base64,CCCC".into(),
                },
            ],
        };
        for items in [
            vec![read_one.clone(), read_text.clone()],
            vec![attached.clone()],
            vec![attached.clone(), read_one.clone()],
            vec![read_text.clone()],
        ] {
            let spans = QwenRenderer::new().render(&prefix("s", &[]), &items);
            let carried = media_in_order(&items);
            assert_eq!(
                media_spans(&spans),
                carried.len(),
                "the renderer placed a different number of images than the transcript holds: \
                 {items:?}"
            );
        }
    }

    /// And the string form collapses the three tokens into one marker, since the server wants its
    /// marker once per image. Pinned because the failure is silent: three markers would ask for three
    /// pictures and be given the one it has.
    #[test]
    fn the_string_form_places_one_marker_per_image_not_three() {
        use letibot_dialect::{media_spans, spans_to_string_with_media};
        let items = vec![TranscriptItem::ToolResult {
            call_id: "c1".into(),
            name: "read".into(),
            outcome: letibot_transcript::ToolOutcome::Ok,
            payload: "shot.png".into(),
            edit: None,
            origin: None,
            media: Some(letibot_transcript::media::Media {
                mime: "image/png".into(),
                bytes: 11,
                width: Some(2),
                height: Some(2),
                data_ref: "data:image/png;base64,AAAA".into(),
                delivered: false,
            }),
        }];
        let spans = QwenRenderer::new().render(&prefix("s", &[]), &items);
        assert_eq!(media_spans(&spans), 1, "one image");
        let text = spans_to_string_with_media(&spans, "<__media__>");
        assert_eq!(
            text.matches("<__media__>").count(),
            1,
            "one marker per image: {text}"
        );
        // The model's own vision tokens are gone from the string form, because the server writes
        // them itself once it has the picture.
        assert!(!text.contains("<|vision_start|>"), "{text}");
        assert!(!text.contains("<|image_pad|>"), "{text}");
        // And the picture sits where the reading happened: after the payload that describes it.
        let payload = text.find("shot.png").expect("the payload is there");
        let marker = text.find("<__media__>").expect("the marker is there");
        assert!(
            payload < marker,
            "the marker is before its own payload: {text}"
        );
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
                truncated: false,
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
        assert!(
            got.ends_with(
                "<|im_start|>assistant\n<think>\nLook at it.\n</think>\n\n\
             <tool_call>\n<function=read>\n\
             <parameter=path>\na.txt\n</parameter>\n\
             <parameter=limit>\n40\n</parameter>\n\
             </function>\n</tool_call>"
            ),
            "{got}"
        );
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
        assert!(
            got.ends_with("<|im_start|>assistant\n<think>\n\n</think>\n\n4."),
            "{got}"
        );
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
                truncated: false,
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
                origin: None,
                media: None,
            },
            TranscriptItem::ToolResult {
                call_id: "c2".into(),
                name: "read".into(),
                outcome: ToolOutcome::Abstained {
                    reason: "no match".into(),
                },
                payload: String::new(),
                edit: None,
                origin: None,
                media: None,
            },
            TranscriptItem::Reasoning {
                text: "They differ.".into(),
                field: ReasoningField::Inline,
                truncated: false,
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
        let from_data = spans
            .iter()
            .any(|s| matches!(s, RenderSpan::Text(t) if t.contains("</tool_call>")));
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

#[cfg(test)]
mod summary_turn_does_not_think {
    use super::*;

    /// The lead a summary turn is handed has its reasoning block closed: the ordinary lead ends on
    /// `<think>`, and a summary runs only when the window is nearly full and has no room to think.
    #[test]
    fn the_ordinary_lead_opens_reasoning_and_the_summary_lead_closes_it() {
        let open = generation_prompt();
        let closed = generation_prompt_closing_reasoning();

        // The summary lead is the ordinary one plus a close, so nothing about how
        // the turn begins changes except that the block is already finished.
        assert_eq!(closed.len(), open.len() + 1);
        assert_eq!(&closed[..open.len()], &open[..]);

        let roles = |v: &Vec<RenderSpan>| -> Vec<letibot_dialect::ControlRole> {
            v.iter()
                .filter_map(|s| match s {
                    RenderSpan::Control(c) => Some(c.role),
                    _ => None,
                })
                .collect()
        };
        use letibot_dialect::ControlRole::{ThinkClose, ThinkOpen};

        // The ordinary lead opens the block and leaves it open — the model reasons
        // from its first token.
        let o = roles(&open);
        assert!(o.contains(&ThinkOpen), "{o:?}");
        assert!(
            !o.contains(&ThinkClose),
            "the ordinary lead must leave it open: {o:?}"
        );

        // The summary lead opens and closes it, in that order, so the model's first
        // token is assistant text.
        let c = roles(&closed);
        let i = c.iter().position(|r| *r == ThinkOpen).expect("opens");
        let j = c.iter().position(|r| *r == ThinkClose).expect("and closes");
        assert!(i < j, "the close has to come after the open: {c:?}");
    }
}

#[cfg(test)]
mod an_abandoned_draft_is_not_replayed {
    //! The measured case: a model counting parentheses by hand produced 25464 tokens of `+ 0 + 0`
    //! before the operator stopped it, and replayed on every later turn it made the model start again.
    //! Reasoning is the one kind that can be elided this way: a killed draft is referred to by nothing.

    use super::*;
    use letibot_transcript::ReasoningField;

    fn rendered(truncated: bool) -> String {
        // The shape of the real thing, at a size a test can hold.
        let loop_text = " + 0".repeat(4_000);
        let items = vec![TranscriptItem::Reasoning {
            text: format!("Let me count them by hand.{loop_text}"),
            field: ReasoningField::Inline,
            truncated,
        }];
        QwenRenderer::new()
            .render_incremental(&[], &items)
            .iter()
            .map(|s| match s {
                RenderSpan::Text(t) => t.clone(),
                RenderSpan::Control(c) => c.literal.to_string(),
            })
            .collect()
    }

    #[test]
    fn a_finished_thought_is_replayed_whole() {
        let out = rendered(false);
        assert!(
            out.contains("+ 0 + 0"),
            "an ordinary reasoning block is history"
        );
        assert!(
            out.len() > 10_000,
            "and is carried at its own size: {}",
            out.len()
        );
    }

    #[test]
    fn a_stopped_thought_is_replaced_by_a_sentence() {
        let out = rendered(true);
        assert!(
            !out.contains("+ 0 + 0"),
            "the abandoned loop must not reach the prompt again"
        );
        assert!(
            out.contains("Do not resume it"),
            "and the model is told why: {out}"
        );
        assert!(
            out.len() < 500,
            "a sentence, not a thought: {} chars",
            out.len()
        );
    }

    /// The saving, stated as the ratio it actually is.
    #[test]
    fn the_prompt_stops_carrying_the_draft() {
        let (whole, stopped) = (rendered(false).len(), rendered(true).len());
        assert!(
            whole / stopped > 30,
            "elision has to be worth doing: {whole} -> {stopped}"
        );
    }
}
