//! Transcript items → `RenderSpan`s, following Qwen3.8's shipped jinja.
//!
//! Every string this renderer emits is either LITERAL (template bytes, which [`lit`] splits
//! out control literals from, because the training runtime tokenized each as one vocab entry)
//! or DATA (conversation bytes, which [`data`] never splits, so a user who types `<tool_call>`
//! gets ordinary tokens and cannot produce a turn boundary).
//!
//! A run of consecutive tool results is ONE user turn, as the template writes it:
//! ```text
//! <|im_start|>user\n<tool_response>\nA\n</tool_response>\n<tool_response>\nB\n</tool_response><|im_end|>\n
//! ```
//! The `<|im_end|>` belongs to whichever result has no result after it, so a caller that rendered
//! one item at a time would write a whole turn per result. [`QwenRenderer::render_incremental_by_item`]
//! renders the batch and says where each item ends, which is what lets `Session::append_items` keep
//! one ledger row per item under a prompt that carries one turn per run.

use letibot_dialect::{RenderSpan, RenderState, StablePrefix};
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

/// Conversation bytes. Never split - see the module header.
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

/// Qwen3.8's renderer: stateless and cheap. The only configuration is [`ReasoningEffort`],
/// which is part of the stable prefix rather than of a request.
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
        render_items(items, &mut st, &mut out, None);
        out
    }

    /// The spans to append for `new_items`, given `history` already rendered. Replays `history`
    /// into a throwaway buffer to recover the boundary state, because a `usize` offset cannot carry
    /// "is an assistant turn open".
    pub fn render_incremental(
        &self,
        history: &[TranscriptItem],
        new_items: &[TranscriptItem],
    ) -> Vec<RenderSpan> {
        self.render_incremental_by_item(history, new_items).0
    }

    /// [`Self::render_incremental`], and the span each item ends at.
    /// A run of tool results is one user turn and only the last owns `<|im_end|>`, which a single
    /// item cannot know. One row per item is the ledger's rule; one turn per run is the prompt's.
    pub fn render_incremental_by_item(
        &self,
        history: &[TranscriptItem],
        new_items: &[TranscriptItem],
    ) -> (Vec<RenderSpan>, Vec<usize>) {
        self.render_incremental_from(history, &mut RenderState::default(), new_items)
    }

    /// The same, seeded from the boundary state a previous call left in `state` and leaving the
    /// state after `new_items` there. A caller that appends in batches carries one token and
    /// never replays; `history` is only read when the token is absent or another renderer's.
    pub fn render_incremental_from(
        &self,
        history: &[TranscriptItem],
        state: &mut RenderState,
        new_items: &[TranscriptItem],
    ) -> (Vec<RenderSpan>, Vec<usize>) {
        let mut st = match state.as_state::<State>() {
            Some(st) => *st,
            None => {
                let mut st = State::default();
                let mut discard = Vec::new();
                render_items(history, &mut st, &mut discard, None);
                st
            }
        };
        let mut out = Vec::new();
        let mut ends = Vec::with_capacity(new_items.len());
        render_items(new_items, &mut st, &mut out, Some(&mut ends));
        *state = RenderState::holding(st);
        (out, ends)
    }

    /// The system turn: reasoning instruction, tools, then the operator's text. The tool block comes
    /// before the system text, which is the opposite of every other template here - the kind of thing
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
                // A tool schema is normalised, harness-produced JSON, but it is still not template text: a
                // description containing `</tool_call>` must not become a boundary, so it goes through `data`.
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
    /// A user message opened to hold a run of tool results whose `<|im_end|>` is not yet written.
    /// The closing token belongs to the result with none after it, which is why
    /// [`QwenRenderer::render_incremental_by_item`] exists.
    tool_turn_open: bool,
}

fn close_assistant(st: &mut State, out: &mut Vec<RenderSpan>) {
    if st.assistant_open {
        ctl(out, &tk::IM_END);
        lit(out, "\n");
        *st = State::default();
    }
}

/// End a run of tool results. Called before any item that is not a result and at the end of a
/// batch, which is the whole of "is another result coming".
fn close_tool_turn(st: &mut State, out: &mut Vec<RenderSpan>) {
    if st.tool_turn_open {
        ctl(out, &tk::IM_END);
        lit(out, "\n");
        st.tool_turn_open = false;
    }
}

/// Open an assistant message and emit its (possibly empty) thinking block. The template emits
/// `<think>\n{reasoning}\n</think>\n\n` unconditionally, so an assistant turn with no reasoning of
/// its own still carries `<think>\n\n</think>\n\n`.
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

/// `ends`, when given, gets the span index each item finished at - the seam
/// [`QwenRenderer::render_incremental_by_item`] needs to keep one ledger row per item while the
/// prompt carries one turn per run of results.
fn render_items(
    items: &[TranscriptItem],
    st: &mut State,
    out: &mut Vec<RenderSpan>,
    mut ends: Option<&mut Vec<usize>>,
) {
    for item in items {
        match item {
            // Renders to nothing, by contract.
            TranscriptItem::SegmentMark { .. } | TranscriptItem::Offer { .. } => {}

            // Unrenderable on this template past position 0: it raises. The daemon is expected to use
            // `system_update_item` instead; rendering it as a marked user turn rather than panicking means a
            // transcript loaded from an older store still renders.
            TranscriptItem::System { text, .. } => {
                close_assistant(st, out);
                close_tool_turn(st, out);
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
                close_tool_turn(st, out);
                ctl(out, &tk::IM_START);
                lit(out, "user\n");
                render_user_parts(parts, out);
                ctl(out, &tk::IM_END);
                lit(out, "\n");
            }

            // A delivery, rendered where this template puts the harness's own words: a user turn
            // with the text trimmed as a user part is - the same span a `User` row of one text
            // part rendered before this kind existed.
            TranscriptItem::Notice { text, .. } => {
                close_assistant(st, out);
                close_tool_turn(st, out);
                ctl(out, &tk::IM_START);
                lit(out, "user\n");
                data(out, trim(text));
                ctl(out, &tk::IM_END);
                lit(out, "\n");
            }

            // A message carries exactly one `reasoning_content`, so a second `Reasoning` item is a second
            // message and closes the one before it. An `Assistant` item is not: it is the visible half of the
            // message the reasoning already opened, and closing here would split one turn into two.
            TranscriptItem::Reasoning {
                text, truncated, ..
            } => {
                close_assistant(st, out);
                close_tool_turn(st, out);
                // An abandoned draft is not replayed: the store keeps it, and the prompt gets a sentence instead
                // of the thought. Reasoning is the one kind that can be elided this way - a killed draft is
                // referred to by nothing, unlike an assistant turn or a tool result.
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
                close_tool_turn(st, out);
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
                if st.tool_turn_open {
                    // Mid-run: the turn is already open and the template separates two results with
                    // one newline, which `</tool_response>` and the next `<tool_response>` share.
                    lit(out, "\n");
                } else {
                    ctl(out, &tk::IM_START);
                    lit(out, "user");
                    lit(out, "\n");
                    st.tool_turn_open = true;
                }
                ctl(out, &tk::TOOL_RESPONSE_OPEN);
                lit(out, "\n");
                // `payload` verbatim: it is already `ToolResult::render`'s envelope
                // when the harness produced it. See `outcome_envelope`.
                let _ = outcome;
                data(out, trim(payload));
                // The picture goes where the reading happened, after the payload that says what was read - the
                // same order the operator's own `read` output has. Without it the model is told an image is
                // attached and given none.
                if media.is_some() {
                    ctl(out, &tk::VISION_START);
                    ctl(out, &tk::IMAGE_PAD);
                    ctl(out, &tk::VISION_END);
                }
                lit(out, "\n");
                ctl(out, &tk::TOOL_RESPONSE_CLOSE);
                // The `<|im_end|>` is NOT written here: this result does not yet know whether
                // another follows it. `close_tool_turn` owns it.
            }
        }
        if let Some(ends) = ends.as_mut() {
            ends.push(out.len());
        }
    }
    // A batch is always followed by a generation prompt or by more items, never left hanging
    // inside a user turn - so whatever closes it belongs to the last item of the batch.
    close_tool_turn(st, out);
    if let Some(ends) = ends
        && let Some(last) = ends.last_mut()
    {
        *last = out.len();
    }
}

/// One `<tool_call>` block, in the XML-ish form this model was trained on. Not JSON: that is the
/// single most consequential difference from Qwen3 and from the ChatML fixture, and it is why this
/// crate exists.
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
/// those bytes are replayed into the next prompt. Arguments that do not parse as a JSON object
/// render as no parameters at all, so a truncated argument stays distinguishable from an empty one.
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
    // With media, the template emits `<|vision_start|><|image_pad|><|vision_end|>` where the part sat.
    // Text on either side is one trimmed run, so the markers go at the front, which is where a harness
    // that resolved a file into an image puts it anyway.
    for _ in &media {
        ctl(out, &tk::VISION_START);
        ctl(out, &tk::IMAGE_PAD);
        ctl(out, &tk::VISION_END);
    }
    data(out, &trimmed);
}

/// [`generation_prompt`] with the reasoning block opened and immediately closed, so the model
/// starts on assistant text instead of reasoning - used for the summary turn, which cannot spend a
/// nearly full window thinking. `lead_opens_reasoning` reads the lead, so nothing downstream asks.
pub fn generation_prompt_closing_reasoning() -> Vec<RenderSpan> {
    let mut v = generation_prompt();
    v.push(RenderSpan::Control(tk::THINK_CLOSE.clone()));
    v
}

/// What stands in for a reasoning block the operator stopped.
pub const ABANDONED_REASONING: &str = "[The operator stopped this reasoning before it finished. Its text is kept in the transcript but is not replayed: it was an abandoned draft, not a conclusion. Do not resume it.]";

/// `<|im_start|>assistant\n<think>\n`: what the model is handed to start speaking. The trailing
/// newline is the template's and is load-bearing - the model was trained to begin its reasoning on
/// the line after `<think>`, and dropping it costs a cold prefill on every turn.
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
    render_items(items, &mut st, &mut discard, None);
    st.assistant_open
}

/// Superseded by `letibot_tools::ToolResult::render` and off the render path; kept because the
/// fixture corpora build a payload with it. Running both wraps one result in two envelopes, which
/// is why the renderer emits `payload` verbatim.
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
        // Not `NO_RESULT`: the other five non-`Ok` classes say there is nothing here to build on, and this
        // one says the opposite - the work is still happening and the handle reaches it. Sharing the
        // envelope would put "still running" in "abandoned"'s class.
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

    /// Every image the transcript holds is placed exactly once. The server substitutes pictures at the
    /// markers in order, so a marker that does not line up puts a picture where it does not belong -
    /// worse than sending none. `media_spans` and `media_in_order` are the two walks the engine compares.
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
        // A tool result that read a picture - the `read` path.
        let read_one = TranscriptItem::ToolResult {
            call_id: "c1".into(),
            name: "read".into(),
            outcome: letibot_transcript::ToolOutcome::Ok,
            payload: "shot.png — image image/png 2×2 · 1 KiB".into(),
            edits: Vec::new(),
            origin: None,
            media: Some(png.clone()),
        };
        // And a result that read a file, which is every other call.
        let read_text = TranscriptItem::ToolResult {
            call_id: "c2".into(),
            name: "read".into(),
            outcome: letibot_transcript::ToolOutcome::Ok,
            payload: "fn main() {}".into(),
            edits: Vec::new(),
            origin: None,
            media: None,
        };
        // A user row carrying two images - the head-attached path, and the case a single-image
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

    /// The string form collapses the three vision tokens into one marker: the server wants its marker
    /// once per image and emits the model's three tokens itself. Pinned because three markers would ask
    /// for three pictures and be given the one it has.
    #[test]
    fn the_string_form_places_one_marker_per_image_not_three() {
        use letibot_dialect::{media_spans, spans_to_string_with_media};
        let items = vec![TranscriptItem::ToolResult {
            call_id: "c1".into(),
            name: "read".into(),
            outcome: letibot_transcript::ToolOutcome::Ok,
            payload: "shot.png".into(),
            edits: Vec::new(),
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

    /// A `Reasoning` item and the `Assistant` item that follows it are ONE message: the template
    /// writes one `<|im_start|>assistant`, one block of thinking, then the visible text. -
    /// the `/apply-template` oracle caught a second message being opened here and no unit test did.
    #[test]
    fn a_reasoning_and_the_assistant_after_it_are_one_message() {
        let items = vec![
            user("what is 2+2?"),
            TranscriptItem::Reasoning {
                text: "Trivial.".into(),
                field: ReasoningField::Inline,
                truncated: false,
            },
            TranscriptItem::Assistant {
                text: "4.".into(),
                tool_calls: vec![],
                truncated: false,
            },
        ];
        let got = spans_to_string(&QwenRenderer::new().render(&prefix("s", &[]), &items));
        assert_eq!(
            got.matches("<|im_start|>assistant").count(),
            1,
            "the assistant half of the message must not open a second one: {got}"
        );
        assert_eq!(got.matches("<think>").count(), 1, "{got}");
        assert!(
            got.ends_with("<|im_start|>assistant\n<think>\nTrivial.\n</think>\n\n4."),
            "{got}"
        );
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
    fn the_batch_render_equals_the_whole_render_at_every_prefix() {
        // Compared against the batch render, not item by item: the last tool result of a run owns
        // the turn's closing token, so item-by-item replay is not the same prompt.
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
                edits: Vec::new(),
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
                edits: Vec::new(),
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
            built.push_str(&spans_to_string(&r.render_incremental(&[], &items[..k])));
            assert_eq!(whole, built, "prefix {k} diverges");
            // And the seams cut that same render back into one slice per item, in order. An empty
            // batch has no seams and no spans.
            let (spans, ends) = r.render_incremental_by_item(&[], &items[..k]);
            assert_eq!(spans, r.render_incremental(&[], &items[..k]));
            assert_eq!(ends.len(), k);
            assert_eq!(ends.last().copied().unwrap_or(0), spans.len());
        }
    }

    /// The token a caller carries, with the history given to the first call only: three appends
    /// must build what three replays build. The splits land inside a run of tool results, whose
    /// `tool_turn_open` decides where the run's `<|im_end|>` goes.
    #[test]
    fn a_carried_token_builds_what_the_replays_build() {
        let items = [
            user("Compare a and b."),
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
                edits: Vec::new(),
                origin: None,
                media: None,
            },
            TranscriptItem::ToolResult {
                call_id: "c2".into(),
                name: "read".into(),
                outcome: ToolOutcome::Ok,
                payload: "beta".into(),
                edits: Vec::new(),
                origin: None,
                media: None,
            },
            user("Thanks."),
        ];
        let r = QwenRenderer::new();
        for k in 0..=items.len() {
            for j in k..=items.len() {
                let mut state = RenderState::default();
                let mut carried = r.render_incremental_from(&[], &mut state, &items[..k]).0;
                carried.extend(r.render_incremental_from(&[], &mut state, &items[k..j]).0);
                carried.extend(r.render_incremental_from(&[], &mut state, &items[j..]).0);

                let mut replayed = r.render_incremental(&[], &items[..k]);
                replayed.extend(r.render_incremental(&items[..k], &items[k..j]));
                replayed.extend(r.render_incremental(&items[..j], &items[j..]));
                assert_eq!(
                    spans_to_string(&carried),
                    spans_to_string(&replayed),
                    "carried token diverged at k={k} j={j}"
                );
            }
        }
        // A split at a run boundary is the ordinary case, and there the appends must also be the
        // whole render: a run split ACROSS appends is two turns, which is the template's rule and
        // not something a token could fix - `append_items` is handed a whole run.
        let p = prefix("Be terse.", &[]);
        let mut state = RenderState::default();
        let mut appends = r.render(&p, &[]);
        appends.extend(r.render_incremental_from(&[], &mut state, &items[..2]).0);
        appends.extend(r.render_incremental_from(&[], &mut state, &items[2..4]).0);
        appends.extend(r.render_incremental_from(&[], &mut state, &items[4..]).0);
        assert_eq!(
            spans_to_string(&appends),
            spans_to_string(&r.render(&p, &items)),
            "the appends are not the whole render"
        );
    }

    /// A token from another renderer falls back to the history rather than to a fresh state: the
    /// second call would otherwise open a second user turn for a run that is still going.
    #[test]
    fn another_renderers_token_falls_back_to_the_history() {
        let r = QwenRenderer::new();
        let history = [
            user("Compare a and b."),
            TranscriptItem::Assistant {
                text: "Reading both.".into(),
                tool_calls: vec![ToolCall {
                    id: "c1".into(),
                    name: "read".into(),
                    arguments: r#"{"path":"a"}"#.into(),
                }],
                truncated: false,
            },
        ];
        let batch = [TranscriptItem::ToolResult {
            call_id: "c1".into(),
            name: "read".into(),
            outcome: ToolOutcome::Ok,
            payload: "alpha".into(),
            edits: Vec::new(),
            origin: None,
            media: None,
        }];
        let mut foreign = RenderState::holding(7u8);
        let (spans, ends) = r.render_incremental_from(&history, &mut foreign, &batch);
        assert_eq!(ends.len(), 1);
        assert_eq!(
            spans_to_string(&spans),
            spans_to_string(&r.render_incremental(&history, &batch)),
            "a foreign token must read as the history, not as a fresh state"
        );
    }

    /// The template's own bytes for a run of two results, copied from its `messages` loop:
    /// one `<|im_start|>user`, each result in its own `<tool_response>`, one `<|im_end|>`.
    #[test]
    fn consecutive_tool_results_are_one_user_turn() {
        let calls = TranscriptItem::Assistant {
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
        };
        let results = [
            TranscriptItem::ToolResult {
                call_id: "c1".into(),
                name: "read".into(),
                outcome: ToolOutcome::Ok,
                payload: "A".into(),
                edits: Vec::new(),
                origin: None,
                media: None,
            },
            TranscriptItem::ToolResult {
                call_id: "c2".into(),
                name: "read".into(),
                outcome: ToolOutcome::Ok,
                payload: "B".into(),
                edits: Vec::new(),
                origin: None,
                media: None,
            },
        ];
        let history = [user("Compare a and b."), calls];
        let r = QwenRenderer::new();

        let got = spans_to_string(&r.render_incremental(&history, &results));
        assert_eq!(
            got,
            "<|im_end|>\n<|im_start|>user\n<tool_response>\nA\n</tool_response>\n<tool_response>\nB\n</tool_response><|im_end|>\n",
            "the assistant's turn closes, then ONE user turn with one im_end - the template's shape"
        );

        // And the whole prompt says the same thing, so the ledger and a re-render agree.
        let p = prefix("Be terse.", &[]);
        let all = [
            history[0].clone(),
            history[1].clone(),
            results[0].clone(),
            results[1].clone(),
        ];
        assert_eq!(
            spans_to_string(&r.render(&p, &all)),
            format!("{}{got}", spans_to_string(&r.render(&p, &history)))
        );

        // The seam is between the two results and the closing token belongs to the LAST one:
        // two rows, one turn.
        let (spans, ends) = r.render_incremental_by_item(&history, &results);
        assert_eq!(ends.len(), 2);
        assert_eq!(ends[1], spans.len());
        assert!(ends[0] < ends[1]);
        let first = spans_to_string(&spans[..ends[0]]);
        let second = spans_to_string(&spans[ends[0]..]);
        // The first row ends on `</tool_response>`: no turn close of its own, because it does not
        // know yet that a result follows. The second one carries the close.
        assert!(first.ends_with("</tool_response>"), "{first}");
        assert!(second.ends_with("</tool_response><|im_end|>\n"), "{second}");
        // And exactly one row opens the user turn - the one that starts the run.
        assert_eq!(first.matches("<|im_start|>user").count(), 1, "{first}");
        assert_eq!(second.matches("<|im_start|>user").count(), 0, "{second}");
        assert_eq!(format!("{first}{second}"), got);
    }

    /// The trailing `<|im_end|>` is written by whoever ends the run, so a batch that ends on a
    /// tool result still closes its turn - otherwise the generation prompt lands inside it.
    #[test]
    fn a_batch_that_ends_on_a_tool_result_closes_its_turn() {
        let r = QwenRenderer::new();
        let one = [TranscriptItem::ToolResult {
            call_id: "c1".into(),
            name: "read".into(),
            outcome: ToolOutcome::Ok,
            payload: "A".into(),
            edits: Vec::new(),
            origin: None,
            media: None,
        }];
        let got = spans_to_string(&r.render_incremental(&[], &one));
        assert!(got.ends_with("</tool_response><|im_end|>\n"), "{got}");

        // Items after the run open their own turn rather than landing inside it.
        let after = spans_to_string(&r.render_incremental(&one, &[user("next")]));
        assert!(after.starts_with("<|im_start|>user\nnext"), "{after}");
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

    /// The lead a summary turn is handed has its reasoning block closed. The ordinary lead ends on
    /// `<think>`, which is right for work; a summary turn runs when the window is nearly full, and those
    /// are the tokens it does not have.
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

        // The ordinary lead opens the block and leaves it open - the model reasons
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
    //! A model counted parentheses by hand, produced 25464 tokens of `+ 0 + 0 + 0` before the operator
    //! stopped it, and every later turn replayed that block - so a truncated reasoning item renders as
    //! a sentence instead. A killed draft is referred to by nothing.

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
