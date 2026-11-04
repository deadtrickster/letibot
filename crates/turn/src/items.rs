//! Turning a generation into transcript items and the ledger rows beside them, from one walk
//! of the ids so rows and items agree where reasoning ended. The first item's range owns the
//! lead; trailing stop tokens are stripped because a boundary belongs to the renderer.

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
    /// Whether the generation ended with the reasoning block still open: the stream stopped
    /// before any `ThinkClose`. Computed over the same tokens and seed the parse used, so the
    /// item view and the stream view cannot disagree.
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

    /// Mark what this turn left unfinished: `truncated` says the TURN stopped early, which is
    /// right for an assistant item however it ended, while a thought is unfinished only when
    /// its block never closed - marking a finished one would elide it from every later prompt.
    pub fn mark_truncated(&mut self, truncated: bool) {
        let open_thought = self.ended_in_reasoning.then(|| {
            self.items
                .iter()
                .rposition(|p| matches!(p.item, TranscriptItem::Reasoning { .. }))
        });
        for (i, produced) in self.items.iter_mut().enumerate() {
            match &mut produced.item {
                TranscriptItem::Assistant { truncated: cut, .. } => *cut = truncated,
                TranscriptItem::Reasoning { truncated: cut, .. } => {
                    *cut = open_thought == Some(Some(i));
                }
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

/// Which channel the model is speaking on before it has generated anything: read off the
/// dialect's own generation prompt through the same `control_role` lookup the parser uses, and
/// folded, because a prompt may open and close a block. The stream and the transcript share it.
pub fn lead_opens_reasoning(lead: &[TokenId], decoder: &dyn TokenDecoder) -> bool {
    lead.iter()
        .fold(false, |open, id| match decoder.control_role(*id) {
            Some(ControlRole::ThinkOpen) => true,
            Some(ControlRole::ThinkClose) => false,
            _ => open,
        })
}

/// Build items and their token ranges from one generation. `lead` is prepended so the first
/// item owns the turn-start tokens the model was handed. Assistant items come out with
/// `truncated: false`; the engine stamps the real value before committing.
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

    // Only the generated tokens are parsed. The lead is the generation prompt, written by the
    // harness, present only so the first item's row owns the turn-start tokens - parsing it
    // would make the literal "assistant" the turn's visible content.
    let opens_in_reasoning = lead_opens_reasoning(lead, decoder);
    let spans = parser.parse(&tokens[lead.len()..], decoder, opens_in_reasoning);

    let mut items: Vec<ProducedItem> = Vec::new();
    let mut visible_text = String::new();
    let mut reasoning_text = String::new();
    // Absolute index into `tokens` up to which items claim coverage. The first item starts at
    // 0, and a gap between spans is absorbed into the next item's range rather than dropped,
    // because a token with no row is a token the next turn re-sends.
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
        // Nothing at all - no tokens, no text, no calls - does not make an item.
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

    // The same fold `lead_opens_reasoning` runs, over the generated body with the lead's answer
    // as the seed. The trailing stops are already stripped, and they are turn-end control, so
    // their removal cannot flip the answer.
    let ended_in_reasoning = tokens[lead.len()..]
        .iter()
        .fold(opens_in_reasoning, |open, id| {
            match decoder.control_role(*id) {
                Some(ControlRole::ThinkOpen) => true,
                Some(ControlRole::ThinkClose) => false,
                _ => open,
            }
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

/// Every token has exactly one row, and the rows are contiguous from zero. Not a debug
/// assertion: a gap here is a token the next turn re-sends and the cache diverges on.
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

    /// A regression this cost a real debugging pass to find: ChatML's lead carries the literal
    /// text `assistant\n`, so leaking it made "assistant" the visible content and turned a
    /// `ReasoningOnly` failure into a `TruncatedText` success.
    #[test]
    fn the_generation_prompts_own_text_is_never_parsed_as_model_output() {
        let d = decoder().with_text(20, "assistant\n");
        // lead = [turn-start, "assistant\n", <think>] - the harness wrote all three.
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

#[cfg(test)]
mod stopping_a_turn_stops_the_thought {
    //! The middle link of the abandoned-draft fix, tested where it lives: both real parsers
    //! emit a `Reasoning` span for a block cut before its `</think>`, and this asserts the
    //! stamp the engine applies from the interrupt reaches that span, not only the assistant.

    use super::*;
    use letibot_transcript::ReasoningField;

    fn produced_ending_in_reasoning(
        items: Vec<TranscriptItem>,
        ended_in_reasoning: bool,
    ) -> Produced {
        Produced {
            items: items
                .into_iter()
                .map(|item| ProducedItem { item, range: 0..1 })
                .collect(),
            tokens: vec![],
            stripped_stops: vec![],
            visible_text: String::new(),
            reasoning_text: String::new(),
            ended_in_reasoning,
        }
    }

    /// The case `ended_in_reasoning` exists to tell apart: a turn that thought, CLOSED the
    /// block, answered, and only then hit the limit. The turn is truncated; the thought is
    /// not, and eliding it would delete a finished thought from every later prompt.
    #[test]
    fn a_thought_that_finished_before_the_turn_was_cut_is_not_marked() {
        let mut p = produced_ending_in_reasoning(
            vec![
                TranscriptItem::Reasoning {
                    text: "a complete thought, closed properly".into(),
                    field: ReasoningField::Inline,
                    truncated: false,
                },
                TranscriptItem::Assistant {
                    text: "an answer that ran out of room".into(),
                    tool_calls: vec![],
                    truncated: false,
                },
            ],
            // The block was closed, so the stream did not end inside it.
            false,
        );
        p.mark_truncated(true);

        let TranscriptItem::Reasoning { truncated, .. } = &p.items[0].item else {
            unreachable!()
        };
        assert!(
            !truncated,
            "a closed thought is finished, whatever happened after it"
        );
        let TranscriptItem::Assistant { truncated, .. } = &p.items[1].item else {
            unreachable!()
        };
        assert!(truncated, "the ANSWER is what was cut");
    }

    /// Two blocks in one turn: the model thought, spoke, thought again, and was
    /// stopped inside the second. Only the second is open.
    #[test]
    fn only_the_last_thought_can_be_the_open_one() {
        let mut p = produced_ending_in_reasoning(
            vec![
                TranscriptItem::Reasoning {
                    text: "first, closed by the speaking that follows".into(),
                    field: ReasoningField::Inline,
                    truncated: false,
                },
                TranscriptItem::Assistant {
                    text: "let me check".into(),
                    tool_calls: vec![],
                    truncated: false,
                },
                TranscriptItem::Reasoning {
                    text: "second, stopped mid-flow + 0 + 0 + 0".into(),
                    field: ReasoningField::Inline,
                    truncated: false,
                },
            ],
            true,
        );
        p.mark_truncated(true);

        let marks: Vec<bool> = p
            .items
            .iter()
            .filter_map(|i| match &i.item {
                TranscriptItem::Reasoning { truncated, .. } => Some(*truncated),
                _ => None,
            })
            .collect();
        assert_eq!(marks, vec![false, true], "only the open one is abandoned");
    }

    #[test]
    fn an_interrupted_turn_marks_its_reasoning_and_not_only_its_answer() {
        let mut p = produced_ending_in_reasoning(
            vec![
                TranscriptItem::Reasoning {
                    text: "let me count them by hand + 0 + 0 + 0".into(),
                    field: ReasoningField::Inline,
                    truncated: false,
                },
                TranscriptItem::Assistant {
                    text: String::new(),
                    tool_calls: vec![],
                    truncated: false,
                },
            ],
            // Stopped mid-thought: the block never closed.
            true,
        );
        p.mark_truncated(true);

        for item in p.items.iter().map(|i| &i.item) {
            match item {
                TranscriptItem::Reasoning { truncated, .. } => assert!(
                    *truncated,
                    "a thought the operator stopped must be marked, or the renderer \
                     hands it back as though it were finished"
                ),
                TranscriptItem::Assistant { truncated, .. } => assert!(*truncated),
                other => panic!("{other:?}"),
            }
        }
    }

    /// And a turn that ended on its own marks neither.
    #[test]
    fn a_turn_that_finished_marks_nothing() {
        let mut p = produced_ending_in_reasoning(
            vec![TranscriptItem::Reasoning {
                text: "a complete thought".into(),
                field: ReasoningField::Inline,
                truncated: false,
            }],
            // A turn that finished did not finish inside its own reasoning.
            false,
        );
        p.mark_truncated(false);
        let TranscriptItem::Reasoning { truncated, .. } = &p.items[0].item else {
            unreachable!()
        };
        assert!(!truncated);
    }
}
