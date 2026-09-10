//! Turning a generation into transcript items **and** the ledger rows beside them.
//!
//! # Why this is one operation and not two
//!
//! `Parser` answers "what did the model say"; the ledger asks "which tokens are
//! this item". Both answers have to come out of the same walk of the same ids, or
//! the rows and the items disagree about where a turn's reasoning ended — and then
//! §5.5's fork and §10's compaction address the wrong tokens.
//!
//! `ParsedSpan` carries no token offsets, so the parser alone cannot answer the
//! second question. Rather than reimplement the parser to get them, this module
//! **segments first** on the two boundaries every dialect spells as single vocab
//! entries (`ThinkOpen`, `ThinkClose`), then runs the real `Parser` over each
//! segment. Every generated token therefore lands in exactly one row, by
//! construction, and the parser stays the only thing that knows a dialect's
//! tool-call grammar.
//!
//! # The interleaving is the point
//!
//! One turn is `[reasoning, tool_call, reasoning, …, message]` and that order is
//! byte-stable across turns — which is what makes the server's prefix cache hit.
//! Segmentation preserves it: a second `<think>` after content opens a second
//! `Reasoning` item rather than overwriting the first.
//!
//! # Trailing stop tokens are stripped, and that is a decision with a price
//!
//! A dialect's stop token is a **boundary**, and boundaries belong to the renderer.
//! GLM's assistant turn is terminated by the next `<|user|>`; keeping the emitted
//! one would put a second `<|user|>` in the prompt when the next user item renders.
//! So the trailing stop is stripped and counted.
//!
//! The price is visible in the cache: llama.cpp's slot retains what it decoded,
//! including the stop, so the next prompt's common prefix with that slot is one
//! token shorter than the slot holds. That is why the prefix witness records
//! **committed** generated tokens rather than the server's `predicted` count — see
//! `crate::prefix`. §18.1 states the invariant with `predicted_tokens(N)`, which is
//! the wrong term for any harness that strips a boundary token.

use std::ops::Range;

use letibot_dialect::{ControlRole, ParsedSpan, Parser, TokenDecoder};
use letibot_tokencore::TokenId;
use letibot_transcript::{ReasoningField, ToolCall, TranscriptItem};

/// One produced item and the exact tokens it owns.
#[derive(Debug, Clone, PartialEq)]
pub struct ProducedItem {
    pub item: TranscriptItem,
    /// Indices into `lead ++ generated`, so the caller can commit them as one
    /// ledger row without re-deriving anything.
    pub range: Range<usize>,
}

/// Everything one generation turned into.
#[derive(Debug, Clone, PartialEq)]
pub struct Produced {
    pub items: Vec<ProducedItem>,
    /// `lead ++ generated`, with trailing stop tokens removed.
    pub tokens: Vec<TokenId>,
    /// The stop tokens that were removed, in order.
    pub stripped_stops: Vec<TokenId>,
    /// Concatenated visible (non-reasoning) text, for the length policy.
    pub visible_text: String,
    /// Concatenated reasoning text, for the length policy.
    pub reasoning_text: String,
}

impl Produced {
    pub fn tool_calls(&self) -> Vec<ToolCall> {
        self.items
            .iter()
            .filter_map(|p| match &p.item {
                TranscriptItem::Assistant { tool_calls, .. } => Some(tool_calls.clone()),
                _ => None,
            })
            .flatten()
            .collect()
    }

    pub fn mark_truncated(&mut self, truncated: bool) {
        for produced in &mut self.items {
            if let TranscriptItem::Assistant { truncated: cut, .. } = &mut produced.item {
                *cut = truncated;
            }
        }
    }
}

/// Remove trailing stop tokens. Returns `(body, stripped)`.
pub fn split_trailing_stops(ids: &[TokenId], stops: &[TokenId]) -> (Vec<TokenId>, Vec<TokenId>) {
    let mut end = ids.len();
    while end > 0 && stops.contains(&ids[end - 1]) {
        end -= 1;
    }
    (ids[..end].to_vec(), ids[end..].to_vec())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SegKind {
    Reasoning,
    Assistant,
}

/// Split `tokens` at reasoning boundaries.
fn segment(tokens: &[TokenId], decoder: &dyn TokenDecoder) -> Vec<(SegKind, Range<usize>)> {
    let mut out: Vec<(SegKind, Range<usize>)> = Vec::new();
    let mut start = 0usize;
    let mut past_first_reasoning = false;

    for (i, id) in tokens.iter().enumerate() {
        match decoder.control_role(*id) {
            Some(ControlRole::ThinkOpen) => {
                // Only a `<think>` that follows a completed reasoning block starts a
                // new item. The first one is preceded by the turn-start token the
                // generation prompt supplied, and those two belong together.
                if past_first_reasoning && start < i {
                    out.push((SegKind::Assistant, start..i));
                    start = i;
                }
            }
            Some(ControlRole::ThinkClose) => {
                out.push((SegKind::Reasoning, start..i + 1));
                start = i + 1;
                past_first_reasoning = true;
            }
            _ => {}
        }
    }
    if start < tokens.len() || out.is_empty() {
        out.push((SegKind::Assistant, start..tokens.len()));
    }
    out
}

/// Which channel the model is speaking on **before it has generated anything**.
///
/// A per-dialect fact, and the only honest source of it is the dialect's own
/// generation prompt — [`crate::PromptRenderer::generation_prompt`], tokenized and
/// read back through the same `control_role` lookup the parser uses. GLM's is
/// `<|assistant|><think>` and the ChatML fixture's is `<|im_start|>assistant\n<think>`,
/// so both hand the model a turn that is already inside a reasoning block. A dialect
/// whose generation prompt stops at `<|assistant|>` does not, and a hardcoded `true`
/// would label that model's *answer* as its reasoning — the same defect with the sign
/// flipped.
///
/// It is a fold rather than "does the last control token open a think block" because
/// a generation prompt is free to open and close one (`<think></think>`, which is how
/// a no-reasoning turn is spelled), and only the final state is the answer.
///
/// # Why the live stream and the committed transcript call this same function
///
/// [`produce`] needs it because the lead is not parsed: without it an aborted
/// deliberation reads as visible content, and §5.7's `ReasoningOnly` failure becomes
/// a `TruncatedText` success. The engine's stream loop needs it because a head is
/// told a channel per delta, and a head told the wrong one shows the reasoning as the
/// answer for the length of the turn. Those are the two views §13.2b requires to
/// agree, so they are seeded from one function over one input rather than from two
/// readings of the same intent.
pub fn lead_opens_reasoning(lead: &[TokenId], decoder: &dyn TokenDecoder) -> bool {
    lead.iter()
        .fold(false, |open, id| match decoder.control_role(*id) {
            Some(ControlRole::ThinkOpen) => true,
            Some(ControlRole::ThinkClose) => false,
            _ => open,
        })
}

/// Build items and their token ranges from one generation.
///
/// `lead` is the generation prompt that was submitted but not committed; it is
/// prepended so that the first item owns the turn-start tokens the model was
/// handed. `reasoning_field` is the dialect's replay field for `Reasoning` items.
///
/// Assistant items come out with `truncated: false`. Whether the turn was cut
/// short is decided from the finish reason and any interrupt, which `produce`
/// does not see; the engine stamps the value via [`Produced::mark_truncated`]
/// before committing.
pub fn produce(
    lead: &[TokenId],
    generated: &[TokenId],
    stops: &[TokenId],
    parser: &dyn Parser,
    decoder: &dyn TokenDecoder,
    reasoning_field: ReasoningField,
) -> Produced {
    let (body, stripped_stops) = split_trailing_stops(generated, stops);
    let mut tokens = Vec::with_capacity(lead.len() + body.len());
    tokens.extend_from_slice(lead);
    tokens.extend_from_slice(&body);

    let mut items: Vec<ProducedItem> = Vec::new();
    let mut visible_text = String::new();
    let mut reasoning_text = String::new();
    // Tokens from a segment that produced no item — an empty `<think></think>`, or
    // a bare turn-start — are carried forward onto the next item rather than
    // dropped. A token with no row is a token the next turn will re-send.
    let mut carried: Option<usize> = None;
    let lead_leaves_think_open = lead_opens_reasoning(lead, decoder);

    for (kind, range) in segment(&tokens, decoder) {
        let start = carried.take().unwrap_or(range.start);
        // **Only the generated tokens are parsed.** The lead is the generation
        // prompt: the harness wrote it, the model did not, and it is present here
        // solely so the first item's ledger row owns the turn-start tokens the
        // model was handed. Parsing it turns a renderer's own bytes into model
        // output — with GLM's `<|assistant|><think>` that is invisible because both
        // are control tokens, and with ChatML's `<|im_start|>assistant\n` it makes
        // the literal text "assistant" the turn's visible content, which is enough
        // to turn a §5.7 `ReasoningOnly` failure into a `TruncatedText` success.
        let parsed_from = range.start.max(lead.len());
        // An `Assistant` segment holding a `<think>` holds an **unterminated** one:
        // a closed block always ends its own segment. That matters because the
        // parser cannot tell — GLM's flushes whatever is in its buffer as
        // `Content` when the stream ends mid-thought, which would promote a cut-off
        // deliberation to visible content and hide §5.7's `ReasoningOnly` case
        // behind a `TruncatedText` verdict. So the split is made here, by token id,
        // before the parser sees it.
        let unterminated = if kind != SegKind::Assistant {
            None
        } else if lead_leaves_think_open && range.start < lead.len() {
            // The lead opened `<think>` and nothing closed it: every generated token
            // in this segment is reasoning.
            Some(parsed_from)
        } else {
            tokens[parsed_from..range.end]
                .iter()
                .position(|id| decoder.control_role(*id) == Some(ControlRole::ThinkOpen))
                .map(|k| parsed_from + k)
        };
        let parse_range = unterminated.map_or(parsed_from..range.end, |k| parsed_from..k);
        let spans = parser.parse(&tokens[parse_range], decoder);
        if let Some(k) = unterminated {
            // Everything after the unmatched `<think>` is reasoning, whatever the
            // parser would have called it.
            for span in parser.parse(&tokens[k..range.end], decoder) {
                if let ParsedSpan::Content(t) | ParsedSpan::Reasoning(t) = span {
                    reasoning_text.push_str(&t);
                }
            }
        }
        match kind {
            SegKind::Reasoning => {
                let text = spans
                    .iter()
                    .filter_map(|s| match s {
                        ParsedSpan::Reasoning(t) => Some(t.as_str()),
                        _ => None,
                    })
                    .collect::<String>();
                if text.is_empty() {
                    // An empty think block is not recoverable as a distinct item —
                    // it is how an assistant turn with no reasoning renders. Its
                    // tokens go to the next item.
                    carried = Some(start);
                    continue;
                }
                reasoning_text.push_str(&text);
                items.push(ProducedItem {
                    item: TranscriptItem::Reasoning {
                        text,
                        field: reasoning_field,
                    },
                    range: start..range.end,
                });
            }
            SegKind::Assistant => {
                let mut text = String::new();
                let mut calls = Vec::new();
                for span in &spans {
                    match span {
                        ParsedSpan::Content(t) => text.push_str(t),
                        // Already accounted for above; a closed reasoning block
                        // never lands in an Assistant segment.
                        ParsedSpan::Reasoning(t) => reasoning_text.push_str(t),
                        ParsedSpan::ToolCall {
                            id,
                            name,
                            arguments,
                        } => calls.push(ToolCall {
                            // GLM's wire format carries no call id, so the harness
                            // assigns one. Positional and stable within the turn.
                            id: id
                                .clone()
                                .unwrap_or_else(|| format!("call_{}", calls.len())),
                            name: name.clone(),
                            arguments: arguments.clone(),
                        }),
                        ParsedSpan::Control(_) => {}
                    }
                }
                if range.is_empty() && text.is_empty() && calls.is_empty() {
                    // Nothing at all: no tokens, no content. Do not manufacture an
                    // item, and there is nothing to carry.
                    continue;
                }
                visible_text.push_str(&text);
                items.push(ProducedItem {
                    item: TranscriptItem::Assistant {
                        text,
                        tool_calls: calls,
                        truncated: false,
                    },
                    range: start..range.end,
                });
            }
        }
    }

    // A trailing carry means the turn ended inside an empty think block. Its tokens
    // still need a row, and §5.4 says an assistant item with empty text must exist
    // and emit its boundary tokens anyway.
    if let Some(start) = carried
        && start < tokens.len()
    {
        items.push(ProducedItem {
            item: TranscriptItem::Assistant {
                text: String::new(),
                tool_calls: Vec::new(),
                truncated: false,
            },
            range: start..tokens.len(),
        });
    }

    Produced {
        items,
        tokens,
        stripped_stops,
        visible_text,
        reasoning_text,
    }
}

/// Every token has exactly one row, and the rows are contiguous from zero.
///
/// Not a debug assertion: a gap here is a token the next turn re-sends and the
/// cache diverges on, which is precisely the class this project exists to abolish.
pub fn rows_cover_every_token(p: &Produced) -> Result<(), String> {
    let mut next = 0usize;
    for item in &p.items {
        if item.range.start != next {
            return Err(format!(
                "row starts at {} but {next} was the next uncovered token",
                item.range.start
            ));
        }
        next = item.range.end;
    }
    if next != p.tokens.len() {
        return Err(format!(
            "{} token(s) covered but the turn holds {}",
            next,
            p.tokens.len()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use letibot_dialect_glm::{GlmParser, TableDecoder, tokens as tk};

    // A tiny synthetic vocabulary: control ids by role, ordinary ids as words.
    const ASSISTANT: TokenId = 100;
    const THINK_OPEN: TokenId = 101;
    const THINK_CLOSE: TokenId = 102;
    const CALL_OPEN: TokenId = 103;
    const CALL_CLOSE: TokenId = 104;
    const KEY_OPEN: TokenId = 105;
    const KEY_CLOSE: TokenId = 106;
    const VAL_OPEN: TokenId = 107;
    const VAL_CLOSE: TokenId = 108;
    const EOT: TokenId = 109;

    fn decoder() -> TableDecoder {
        TableDecoder::new()
            .with_control(ASSISTANT, &tk::ASSISTANT)
            .with_control(THINK_OPEN, &tk::THINK_OPEN)
            .with_control(THINK_CLOSE, &tk::THINK_CLOSE)
            .with_control(CALL_OPEN, &tk::TOOL_CALL_OPEN)
            .with_control(CALL_CLOSE, &tk::TOOL_CALL_CLOSE)
            .with_control(KEY_OPEN, &tk::ARG_KEY_OPEN)
            .with_control(KEY_CLOSE, &tk::ARG_KEY_CLOSE)
            .with_control(VAL_OPEN, &tk::ARG_VALUE_OPEN)
            .with_control(VAL_CLOSE, &tk::ARG_VALUE_CLOSE)
            .with_control(EOT, &tk::ENDOFTEXT)
            .with_text(1, "thinking")
            .with_text(2, "answer")
            .with_text(3, "read")
            .with_text(4, "path")
            .with_text(5, "/tmp/a")
            .with_text(6, "again")
            .with_text(7, "done")
    }

    fn produce_with(lead: &[TokenId], emitted: &[TokenId]) -> Produced {
        let d = decoder();
        produce(
            lead,
            emitted,
            &[EOT],
            &GlmParser::new(),
            &d,
            ReasoningField::ReasoningContent,
        )
    }

    #[test]
    fn a_plain_turn_makes_one_reasoning_and_one_assistant_item() {
        let p = produce_with(&[ASSISTANT, THINK_OPEN], &[1, THINK_CLOSE, 2, EOT]);
        assert_eq!(p.stripped_stops, vec![EOT]);
        assert_eq!(p.items.len(), 2);
        assert_eq!(
            p.items[0].item,
            TranscriptItem::Reasoning {
                text: "thinking".into(),
                field: ReasoningField::ReasoningContent
            }
        );
        // The reasoning row owns the turn-start tokens the generation prompt gave,
        // and the `</think>` that closed it.
        assert_eq!(p.items[0].range, 0..4);
        assert_eq!(
            p.items[1].item,
            TranscriptItem::Assistant {
                text: "answer".into(),
                tool_calls: vec![],
                truncated: false
            }
        );
        rows_cover_every_token(&p).unwrap();
    }

    /// The case a reasoning-as-a-field design loses. It is the reason
    /// `TranscriptItem::Reasoning` is a sibling, so it gets a test here too.
    #[test]
    fn reasoning_interleaved_with_tool_calls_keeps_its_order_and_its_rows() {
        let emitted = [
            1,
            THINK_CLOSE, // reasoning 1
            CALL_OPEN,
            3,
            KEY_OPEN,
            4,
            KEY_CLOSE,
            VAL_OPEN,
            5,
            VAL_CLOSE,
            CALL_CLOSE, // call
            THINK_OPEN,
            6,
            THINK_CLOSE, // reasoning 2
            7,
            EOT,
        ];
        let p = produce_with(&[ASSISTANT, THINK_OPEN], &emitted);
        let kinds: Vec<&str> = p
            .items
            .iter()
            .map(|i| match i.item {
                TranscriptItem::Reasoning { .. } => "reasoning",
                TranscriptItem::Assistant { .. } => "assistant",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, ["reasoning", "assistant", "reasoning", "assistant"]);

        let TranscriptItem::Assistant { tool_calls, .. } = &p.items[1].item else {
            panic!()
        };
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].name, "read");
        assert_eq!(tool_calls[0].arguments, r#"{"path":"/tmp/a"}"#);

        let TranscriptItem::Reasoning { text, .. } = &p.items[2].item else {
            panic!()
        };
        assert_eq!(text, "again");
        rows_cover_every_token(&p).unwrap();
    }

    #[test]
    fn an_empty_think_block_does_not_make_a_phantom_item_but_keeps_its_tokens() {
        let p = produce_with(&[ASSISTANT, THINK_OPEN], &[THINK_CLOSE, 2, EOT]);
        assert_eq!(p.items.len(), 1, "{:?}", p.items);
        assert_eq!(p.items[0].range, 0..4, "the empty block's tokens carried");
        rows_cover_every_token(&p).unwrap();
    }

    #[test]
    fn a_turn_that_was_cut_inside_reasoning_keeps_it_as_reasoning() {
        // No `</think>`: the turn was aborted mid-thought.
        let p = produce_with(&[ASSISTANT, THINK_OPEN], &[1, 6]);
        assert_eq!(p.reasoning_text, "thinkingagain");
        assert!(
            p.visible_text.is_empty(),
            "unterminated thinking must not be promoted to content"
        );
        rows_cover_every_token(&p).unwrap();
    }

    #[test]
    fn every_token_has_exactly_one_row_in_every_shape_above() {
        for emitted in [
            vec![1, THINK_CLOSE, 2, EOT],
            vec![THINK_CLOSE, EOT],
            vec![2],
            vec![EOT],
            vec![],
        ] {
            let p = produce_with(&[ASSISTANT, THINK_OPEN], &emitted);
            rows_cover_every_token(&p).unwrap_or_else(|e| panic!("{emitted:?}: {e}"));
        }
    }

    /// A regression this cost a real debugging pass to find.
    ///
    /// GLM's generation prompt is two control tokens, so a lead that leaked into
    /// the parse was invisible. ChatML's is `<|im_start|>` + the literal text
    /// `assistant\n` + `<think>`, and leaking it made "assistant" the turn's
    /// visible content — which turned a §5.7 `ReasoningOnly` **failure** into a
    /// `TruncatedText` **success**, i.e. reproduced the nine unnoticed rows through
    /// a different door.
    #[test]
    fn the_generation_prompts_own_text_is_never_parsed_as_model_output() {
        let d = decoder().with_text(20, "assistant\n");
        // lead = [turn-start, "assistant\n", <think>] — the harness wrote all three.
        let lead = [ASSISTANT, 20, THINK_OPEN];
        let p = produce(
            &lead,
            &[1, 6],
            &[EOT],
            &GlmParser::new(),
            &d,
            ReasoningField::ReasoningContent,
        );
        assert!(
            p.visible_text.is_empty(),
            "the lead's own text became model output: {:?}",
            p.visible_text
        );
        assert_eq!(p.reasoning_text, "thinkingagain");
        rows_cover_every_token(&p).unwrap();
    }

    /// The same lead, with the model completing its thought and answering.
    #[test]
    fn a_lead_with_text_still_yields_clean_items_when_the_turn_completes() {
        let d = decoder().with_text(20, "assistant\n");
        let p = produce(
            &[ASSISTANT, 20, THINK_OPEN],
            &[1, THINK_CLOSE, 2, EOT],
            &[EOT],
            &GlmParser::new(),
            &d,
            ReasoningField::ReasoningContent,
        );
        assert_eq!(p.reasoning_text, "thinking");
        assert_eq!(p.visible_text, "answer");
        assert_eq!(p.items.len(), 2);
        rows_cover_every_token(&p).unwrap();
    }

    #[test]
    fn trailing_stops_are_stripped_but_an_interior_one_is_not() {
        let (body, stripped) = split_trailing_stops(&[1, EOT, 2, EOT, EOT], &[EOT]);
        assert_eq!(body, vec![1, EOT, 2]);
        assert_eq!(stripped, vec![EOT, EOT]);
    }
}
