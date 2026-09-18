//! Turning a generation into transcript items **and** the ledger rows beside them.
//!
//! # Why this is one operation and not two
//!
//! `Parser` answers "what did the model say" and, since `ParsedSpan` carries token
//! offsets, "which tokens are this item" — both out of the same walk of the same
//! ids. This module groups those spans into transcript items and assigns each item
//! the exact token range it owns, so the rows and the items cannot disagree about
//! where a turn's reasoning ended — which is what §5.5's fork and §10's compaction
//! address.
//!
//! Two facts the parser cannot see are handled here, because they are the caller's
//! and not the parser's:
//!
//! * **The lead is never parsed.** The generation prompt is harness-written; parsing
//!   it turns a renderer's own bytes into model output. Its tokens still need a row,
//!   so the first item's range starts at zero and owns them.
//! * **Trailing stop tokens are stripped.** A stop is a boundary, boundaries belong
//!   to the renderer, and keeping the emitted one would put a second user-turn
//!   opener in the next prompt. The price is visible in the cache: the slot retains
//!   what it decoded, so the next prompt's common prefix with it is one token
//!   shorter — which is why the prefix witness records **committed** generated
//!   tokens rather than the server's `predicted` count. See `crate::prefix`.
//!
//! # The interleaving is the point
//!
//! One turn is `[reasoning, tool_call, reasoning, ..., message]` and that order is
//! byte-stable across turns — which is what makes the server's prefix cache hit.
//! Grouping by span kind preserves it: a second reasoning block after content
//! becomes a second `Reasoning` item rather than overwriting the first.
//!
//! An unterminated reasoning block — a turn cut mid-thought — needs no special
//! handling here any more: the parser ends in reasoning mode and reports the buffer
//! as a `Reasoning` span (Qwen's always did; GLM's was fixed when the offsets
//! landed, which is what deleted this module's old re-segmentation pass).

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
    /// Whether the generation ended with the reasoning block still open: the
    /// stream stopped before any `ThinkClose`. Computed over the same tokens the
    /// spans were parsed from, with the same `opens_in_reasoning` seed the parse
    /// used, by the same fold [`lead_opens_reasoning`] runs over the lead — one
    /// primitive (`decoder.control_role`), so the item view and the stream view
    /// cannot disagree about where the block ended. R7's turn-boundary check
    /// reads this: a turn that stops inside its own reasoning and says nothing
    /// else is a failure, not an empty success.
    pub ended_in_reasoning: bool,
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
            match &mut produced.item {
                TranscriptItem::Assistant { truncated: cut, .. } => *cut = truncated,
                // **A stopped turn stops the thought too**, and the thought is
                // what a renderer may decline to replay. Marked on the same pass
                // and from the same fact, so an interrupted turn cannot end up
                // with a cut answer beside a thought that claims to be whole.
                TranscriptItem::Reasoning { truncated: cut, .. } => *cut = truncated,
                _ => {}
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

    // **Only the generated tokens are parsed.** The lead is the generation
    // prompt: the harness wrote it, the model did not, and it is present here
    // solely so the first item's ledger row owns the turn-start tokens the
    // model was handed. Parsing it turns a renderer's own bytes into model
    // output — with the ChatML lead it makes the literal text "assistant" the
    // turn's visible content, which is enough to turn a §5.7 `ReasoningOnly`
    // failure into a `TruncatedText` success. Whether the lead leaves the
    // reasoning block open is the one fact the parser needs from outside, and
    // `lead_opens_reasoning` is its single source.
    let opens_in_reasoning = lead_opens_reasoning(lead, decoder);
    let spans = parser.parse(&tokens[lead.len()..], decoder, opens_in_reasoning);

    let mut items: Vec<ProducedItem> = Vec::new();
    let mut visible_text = String::new();
    let mut reasoning_text = String::new();
    // Absolute index into `tokens` up to which emitted items claim coverage. The
    // lead is never parsed, so the first item starts at 0; a gap between spans —
    // an empty think block the parser skips — is absorbed into the next item's
    // range rather than dropped, because a token with no row is a token the next
    // turn will re-send.
    let mut covered_to = 0usize;
    // The assistant item under construction, if the current run of spans is
    // content-shaped: (text, calls, first token).
    let mut assistant: Option<(String, Vec<ToolCall>, usize)> = None;

    for span in spans {
        match span {
            ParsedSpan::Reasoning { text, range } => {
                if let Some((atext, calls, start)) = assistant.take() {
                    items.push(ProducedItem {
                        item: TranscriptItem::Assistant {
                            text: atext,
                            tool_calls: calls,
                            truncated: false,
                        },
                        range: start..covered_to,
                    });
                }
                reasoning_text.push_str(&text);
                let end = lead.len() + range.end;
                items.push(ProducedItem {
                    item: TranscriptItem::Reasoning {
                        text,
                        field: reasoning_field,
                        // Stamped by `mark_truncated` once the engine knows how
                        // the turn ended; `produce` cannot see that from here.
                        truncated: false,
                    },
                    range: covered_to..end,
                });
                covered_to = end;
            }
            ParsedSpan::Content { text, range } => {
                let group =
                    assistant.get_or_insert_with(|| (String::new(), Vec::new(), covered_to));
                group.0.push_str(&text);
                visible_text.push_str(&text);
                covered_to = lead.len() + range.end;
            }
            ParsedSpan::ToolCall {
                id,
                name,
                arguments,
                range,
            } => {
                let group =
                    assistant.get_or_insert_with(|| (String::new(), Vec::new(), covered_to));
                // GLM's wire format carries no call id, so the harness assigns one.
                // Positional and stable within the turn.
                group.1.push(ToolCall {
                    id: id.unwrap_or_else(|| format!("call_{}", group.1.len())),
                    name,
                    arguments,
                });
                covered_to = lead.len() + range.end;
            }
            ParsedSpan::Control { range, .. } => {
                // A control token belongs to whatever item owns its neighbours: the
                // open assistant group if there is one, the next item otherwise.
                covered_to = lead.len() + range.end;
            }
        }
    }
    if let Some((text, calls, start)) = assistant.take() {
        // Nothing at all — no tokens, no text, no calls — does not make an item.
        if !text.is_empty() || !calls.is_empty() || start < covered_to {
            items.push(ProducedItem {
                item: TranscriptItem::Assistant {
                    text,
                    tool_calls: calls,
                    truncated: false,
                },
                range: start..covered_to,
            });
        }
    }

    // Coverage that stops short means the turn ended inside an empty think block.
    // Its tokens still need a row, and §5.4 says an assistant item with empty text
    // must exist and emit its boundary tokens anyway.
    if covered_to < tokens.len() {
        items.push(ProducedItem {
            item: TranscriptItem::Assistant {
                text: String::new(),
                tool_calls: Vec::new(),
                truncated: false,
            },
            range: covered_to..tokens.len(),
        });
    }

    // The same fold `lead_opens_reasoning` runs over the lead, run over the
    // generated body with the lead's answer as the seed. The trailing stop tokens
    // are already stripped; they are turn-end control, never think control, so
    // their removal cannot flip the answer.
    let ended_in_reasoning = tokens[lead.len()..]
        .iter()
        .fold(opens_in_reasoning, |open, id| match decoder.control_role(*id) {
            Some(ControlRole::ThinkOpen) => true,
            Some(ControlRole::ThinkClose) => false,
            _ => open,
        });

    Produced {
        items,
        tokens,
        stripped_stops,
        visible_text,
        reasoning_text,
        ended_in_reasoning,
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
                field: ReasoningField::ReasoningContent,
                truncated: false,
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
