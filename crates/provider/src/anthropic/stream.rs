//! The Messages SSE stream, as deltas and as a finished answer.
//!
//! Every event is `event: <name>` plus one `data: {…}` object whose own `type` repeats the name,
//! so the reader keys on the payload only and a name it does not know is ignored - the API says new
//! event types may appear (<https://platform.claude.com/docs/en/api/versioning>).
//!
//! The order, and what each event carries
//! (<https://platform.claude.com/docs/en/build-with-claude/streaming>):
//!
//! ```text
//! message_start        message.model, message.usage (input, cache read/write, output)
//! content_block_start  the block's kind; tool_use carries id and name, redacted_thinking its data
//! content_block_delta  text_delta.text | thinking_delta.thinking | signature_delta.signature
//!                      | input_json_delta.partial_json
//! content_block_stop
//! message_delta        delta.stop_reason (and the cumulative usage)
//! message_stop         the end; without it a closed socket is a fragment, not an answer
//! error                {type, message}: the refusal a 200 already promised would not come
//! ```
//!
//! Thinking is the one place a delta is not enough: the text arrives in pieces and the `signature`
//! arrives once at the end, and the pair is what a later request has to send back unmodified
//! (<https://platform.claude.com/docs/en/build-with-claude/thinking#preserving-thinking-blocks>).
//! So each `thinking` block is held whole until its `content_block_stop`, and comes out as one
//! [`Thought`] - the head still draws it a token at a time, because the deltas are also forwarded.

use letibot_backend::{Completion, Delta, Finish, Meter, Thought, TurnCost};
use letibot_transcript::ReasoningField;
use serde_json::Value;

use crate::catalogue::Catalogue;
use crate::keys::Credentials;
use crate::presets::Preset;

/// One tool call being streamed. `input_json_delta` fragments append to `arguments`, so the count
/// here is the one the tree's `ToolCall` carries: the model's own JSON text.
#[derive(Default)]
struct Call {
    id: String,
    name: String,
    arguments: String,
}

/// One content block, by its stream `index`. The index is the block's place in the final
/// `content` array and is not the call slot: a turn can hold thinking, text and calls in one.
enum Block {
    Text,
    Thinking {
        text: String,
        signature: String,
    },
    Tool(usize),
    /// A block this build does not model. Its deltas are dropped, and it does not fail the turn:
    /// the API adds block types, and one unrecognised block is not a broken answer.
    Other,
}

/// What the stream built up, and the last usage it was told.
#[derive(Default)]
pub(super) struct Accumulator {
    text: String,
    reasoning: String,
    /// Finished thinking blocks, in the order the model produced them.
    thoughts: Vec<Thought>,
    blocks: Vec<Block>,
    calls: Vec<Call>,
    finish: Option<Finish>,
    /// `message_stop` arrived. A socket that closes cleanly after half an answer looks exactly
    /// like a finished one, so its absence has to be a failure.
    stopped: bool,
    /// `input_tokens`, `cache_read_input_tokens`, `cache_creation_input_tokens` and
    /// `output_tokens`, merged across `message_start` and `message_delta`, whose counts are
    /// cumulative rather than per-event.
    usage: Option<Value>,
    /// The model the API says served this turn, which is not always the one asked for.
    model: Option<String>,
}

impl Accumulator {
    /// One event's payload → the deltas it carries, in order. An `error` event is not a delta and
    /// is answered by [`crate::anthropic::in_band_refusal`] before this is called.
    pub(super) fn absorb(&mut self, event: &Value) -> Vec<Delta> {
        let mut out = Vec::new();
        match event.get("type").and_then(Value::as_str) {
            Some("message_start") => {
                let message = event.get("message").unwrap_or(&Value::Null);
                if let Some(m) = message.get("model").and_then(Value::as_str)
                    && !m.is_empty()
                {
                    self.model = Some(m.to_string());
                }
                if let Some(u) = message.get("usage").filter(|u| !u.is_null()) {
                    self.merge_usage(u);
                }
            }
            Some("content_block_start") => {
                let index = event.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                let block = event.get("content_block").unwrap_or(&Value::Null);
                let kind = match block.get("type").and_then(Value::as_str) {
                    Some("text") => Block::Text,
                    Some("thinking") => Block::Thinking {
                        text: block
                            .get("thinking")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        signature: block
                            .get("signature")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    },
                    // The whole block arrives at once, encrypted: nothing streams and nothing is
                    // shown, and it exists only to go back to the API unchanged.
                    Some("redacted_thinking") => {
                        let data = block
                            .get("data")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        self.thoughts.push(Thought {
                            text: String::new(),
                            field: ReasoningField::Redacted { data },
                        });
                        Block::Other
                    }
                    Some("tool_use") => {
                        let slot = self.calls.len();
                        self.calls.push(Call {
                            id: block
                                .get("id")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            name: block
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            arguments: String::new(),
                        });
                        out.push(Delta::ToolCall {
                            index: slot,
                            id: Some(self.calls[slot].id.clone()),
                            name: Some(self.calls[slot].name.clone()),
                            arguments: String::new(),
                        });
                        Block::Tool(slot)
                    }
                    _ => Block::Other,
                };
                while self.blocks.len() <= index {
                    self.blocks.push(Block::Other);
                }
                self.blocks[index] = kind;
            }
            Some("content_block_delta") => {
                let index = event.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                let delta = event.get("delta").unwrap_or(&Value::Null);
                match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        if let Some(t) = delta.get("text").and_then(Value::as_str)
                            && !t.is_empty()
                        {
                            self.text.push_str(t);
                            out.push(Delta::Text(t.to_string()));
                        }
                    }
                    Some("thinking_delta") => {
                        if let Some(t) = delta.get("thinking").and_then(Value::as_str)
                            && !t.is_empty()
                        {
                            self.reasoning.push_str(t);
                            if let Some(Block::Thinking { text, .. }) = self.blocks.get_mut(index) {
                                text.push_str(t);
                            }
                            out.push(Delta::Reasoning(t.to_string()));
                        }
                    }
                    Some("signature_delta") => {
                        if let Some(s) = delta.get("signature").and_then(Value::as_str)
                            && let Some(Block::Thinking { signature, .. }) =
                                self.blocks.get_mut(index)
                        {
                            signature.push_str(s);
                        }
                    }
                    Some("input_json_delta") => {
                        if let Some(part) = delta.get("partial_json").and_then(Value::as_str)
                            && !part.is_empty()
                            && let Some(slot) = self.slot_at(index)
                        {
                            self.calls[slot].arguments.push_str(part);
                            out.push(Delta::ToolCall {
                                index: slot,
                                id: None,
                                name: None,
                                arguments: part.to_string(),
                            });
                        }
                    }
                    // `citations_delta` and anything newer: not this tree's business, and a
                    // citation is not a token of the answer.
                    _ => {}
                }
            }
            Some("content_block_stop") => {
                let index = event.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                if let Some(Block::Thinking { text, signature }) = self.blocks.get_mut(index) {
                    // A thought with no signature cannot be replayed, and this is the only place
                    // the pair exists: keep it as the plain reasoning it is otherwise.
                    if !signature.is_empty() {
                        self.thoughts.push(Thought {
                            text: std::mem::take(text),
                            field: ReasoningField::Signed {
                                signature: std::mem::take(signature),
                            },
                        });
                    } else {
                        text.clear();
                    }
                }
            }
            Some("message_delta") => {
                if let Some(fr) = event
                    .get("delta")
                    .and_then(|d| d.get("stop_reason"))
                    .and_then(Value::as_str)
                {
                    self.finish = Some(finish_of(fr));
                }
                if let Some(u) = event.get("usage").filter(|u| !u.is_null()) {
                    self.merge_usage(u);
                }
            }
            Some("message_stop") => self.stopped = true,
            // `ping`, and whatever the API adds next: nothing to carry.
            _ => {}
        }
        out
    }

    /// The call slot a block index holds, when it holds one.
    fn slot_at(&self, index: usize) -> Option<usize> {
        match self.blocks.get(index) {
            Some(Block::Tool(slot)) => Some(*slot),
            _ => None,
        }
    }

    /// Usage arrives twice and both counts are cumulative: `message_start` carries the input side,
    /// `message_delta` the output. A key that is absent in the later event keeps the earlier value.
    fn merge_usage(&mut self, usage: &Value) {
        let mut merged = self
            .usage
            .take()
            .unwrap_or_else(|| Value::Object(Default::default()));
        if let (Some(to), Some(from)) = (merged.as_object_mut(), usage.as_object()) {
            for (k, v) in from {
                if !v.is_null() {
                    to.insert(k.clone(), v.clone());
                }
            }
        }
        self.usage = Some(merged);
    }

    /// Did the provider say the answer was over?
    pub(super) fn ended(&self) -> bool {
        self.stopped
    }

    /// What the deltas built, priced by the model the API says served the turn.
    pub(super) fn finish(
        self,
        model: &str,
        creds: &Credentials,
        wall_ms: u64,
        preset: &'static Preset,
        catalogue: &Catalogue,
    ) -> Completion {
        let u = self.usage.as_ref();
        let get = |k: &str| {
            u.and_then(|u| u.get(k))
                .and_then(Value::as_u64)
                .unwrap_or(0)
        };
        // The API's own arithmetic: "Total input tokens in a request is the summation of
        // `input_tokens`, `cache_creation_input_tokens`, and `cache_read_input_tokens`"
        // (<https://platform.claude.com/docs/en/api/messages>). A cache WRITE is billed at the
        // input rate here, because `Prices` has one cached rate and that is the read one.
        let fresh = get("input_tokens");
        let cache_read = get("cache_read_input_tokens");
        let cache_write = get("cache_creation_input_tokens");
        let prompt_tokens = fresh + cache_read + cache_write;
        let generated_tokens = get("output_tokens");
        let billed = self.model.as_deref().unwrap_or(model);
        let micros_usd = creds
            .prices
            .get(billed)
            .copied()
            .or_else(|| {
                catalogue
                    .model(preset.catalogue_id, billed)
                    .and_then(|m| m.prices)
            })
            .map(|p| p.micros(prompt_tokens, cache_read, generated_tokens));
        let tool_calls = self
            .calls
            .into_iter()
            .filter(|c| !c.name.is_empty())
            .map(|c| letibot_transcript::ToolCall {
                id: if c.id.is_empty() {
                    letibot_transcript::mint_call_id()
                } else {
                    c.id
                },
                name: c.name,
                arguments: c.arguments,
            })
            .collect();
        Completion {
            text: self.text,
            reasoning: self.reasoning,
            tool_calls,
            finish: self.finish.unwrap_or(Finish::Stop),
            cost: TurnCost {
                meter: Meter::Metered,
                prompt_tokens,
                cached_tokens: cache_read,
                generated_tokens,
                wall_ms,
                micros_usd,
            },
            raw_usage: u.map(|u| u.to_string()),
            thoughts: self.thoughts,
        }
    }
}

/// The API's `stop_reason` in this tree's terms
/// (<https://platform.claude.com/docs/en/api/messages>): `end_turn` and `stop_sequence` are a
/// finished answer, `max_tokens` is one cut short, `tool_use` is calls, and the rest are kept
/// verbatim so a head can say what happened rather than guess.
pub(super) fn finish_of(stop_reason: &str) -> Finish {
    match stop_reason {
        "end_turn" | "stop_sequence" => Finish::Stop,
        "max_tokens" | "model_context_window_exceeded" => Finish::Length,
        "tool_use" => Finish::ToolCalls,
        other => Finish::Other(other.to_string()),
    }
}
