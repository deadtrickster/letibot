//! Token ids in, spans out.
//! Boundaries are decided by token id, never by scanning text, so text that spells `</think>` is not a boundary.
//! Ordinary token runs are decoded as runs, since per-token concatenation differs from sequence detokenizing on BPE.

use letibot_dialect::{ControlRole, ControlToken, ParsedSpan, Parser, TokenDecoder};
use serde_json::{Map, Value};

/// GLM's parser.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GlmParser;

impl GlmParser {
    pub fn new() -> Self {
        GlmParser
    }
}

#[derive(Debug, PartialEq)]
enum Mode {
    Content,
    Reasoning,
    ToolName,
    ArgKey,
    ArgValue,
}

impl Parser for GlmParser {
    fn parse(
        &self,
        tokens: &[u32],
        decoder: &dyn TokenDecoder,
        reasoning_open: bool,
    ) -> Vec<ParsedSpan> {
        let mut out: Vec<ParsedSpan> = Vec::new();
        let mut mode = if reasoning_open {
            Mode::Reasoning
        } else {
            Mode::Content
        };
        let mut buf = String::new();
        let mut run: Vec<u32> = Vec::new();
        // Where the buffered run, the open reasoning block and the open tool call began, in
        // `tokens`. Every span's range is one of these, so a caller commits exactly the tokens a
        // span owns without re-walking the stream.
        let mut run_start = 0usize;
        let mut block_start = 0usize;
        let mut call_start = 0usize;
        let mut call_name = String::new();
        let mut arg_key = String::new();
        let mut args: Map<String, Value> = Map::new();

        let flush_content =
            |buf: &mut String, out: &mut Vec<ParsedSpan>, start: usize, end: usize| {
                if !buf.is_empty() {
                    out.push(ParsedSpan::Content {
                        text: std::mem::take(buf),
                        range: start..end,
                    });
                }
            };

        for (i, &id) in tokens.iter().enumerate() {
            let Some(role) = decoder.control_role(id) else {
                if run.is_empty() {
                    run_start = i;
                }
                run.push(id);
                continue;
            };
            if !run.is_empty() {
                buf.push_str(&decoder.decode(&run));
                run.clear();
            }

            match role {
                ControlRole::ThinkOpen => {
                    flush_content(&mut buf, &mut out, run_start, i);
                    mode = Mode::Reasoning;
                    block_start = i;
                }
                ControlRole::ThinkClose => {
                    // An empty think block is what an assistant turn with no reasoning
                    // renders as, so it is not recoverable as a distinct item.
                    // Documented as outside "the round-trippable parts".
                    if !buf.is_empty() {
                        out.push(ParsedSpan::Reasoning {
                            text: std::mem::take(&mut buf),
                            range: block_start..i + 1,
                        });
                    }
                    buf.clear();
                    mode = Mode::Content;
                }
                ControlRole::ToolCallOpen => {
                    flush_content(&mut buf, &mut out, run_start, i);
                    call_start = i;
                    call_name.clear();
                    args = Map::new();
                    mode = Mode::ToolName;
                }
                ControlRole::ToolCallClose => {
                    if mode == Mode::ToolName {
                        call_name = std::mem::take(&mut buf);
                    }
                    buf.clear();
                    out.push(ParsedSpan::ToolCall {
                        // GLM's wire format carries no call id. It is assigned by the
                        // harness, so it is not round-trippable and must not be
                        // invented.
                        id: None,
                        name: std::mem::take(&mut call_name),
                        arguments: serde_json::to_string(&Value::Object(std::mem::take(&mut args)))
                            .expect("a JSON object always serialises"),
                        range: call_start..i + 1,
                    });
                    mode = Mode::Content;
                }
                ControlRole::ArgKeyOpen => {
                    if mode == Mode::ToolName {
                        call_name = std::mem::take(&mut buf);
                    }
                    buf.clear();
                    mode = Mode::ArgKey;
                }
                ControlRole::ArgKeyClose => {
                    arg_key = std::mem::take(&mut buf);
                    mode = Mode::Content;
                }
                ControlRole::ArgValueOpen => {
                    buf.clear();
                    mode = Mode::ArgValue;
                }
                ControlRole::ArgValueClose => {
                    args.insert(
                        std::mem::take(&mut arg_key),
                        reconstruct_arg(std::mem::take(&mut buf)),
                    );
                    mode = Mode::Content;
                }
                other => {
                    flush_content(&mut buf, &mut out, run_start, i);
                    out.push(ParsedSpan::Control {
                        role: other,
                        range: i..i + 1,
                    });
                    mode = Mode::Content;
                }
            }
        }
        let end = tokens.len();
        if !run.is_empty() {
            buf.push_str(&decoder.decode(&run));
        }
        // The final flush honours the mode: a turn cut inside its reasoning would otherwise be
        // flushed as content, promoting the deliberation to visible output.
        if mode == Mode::Reasoning {
            if !buf.is_empty() {
                out.push(ParsedSpan::Reasoning {
                    text: std::mem::take(&mut buf),
                    range: block_start..end,
                });
            }
        } else {
            flush_content(&mut buf, &mut out, run_start, end);
        }
        out
    }
}

/// The render side emits strings raw and everything else as JSON, so the inverse tries JSON
/// first. Lossy in one corner - a string whose text is itself valid JSON comes back as a
/// number - which a test pins, because the shipped format does not carry the type.
fn reconstruct_arg(text: String) -> Value {
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::String(_)) | Err(_) => Value::String(text),
        Ok(v) => v,
    }
}

/// A decoder over a fixed table, so the round-trip property can be checked with no GGUF, no
/// FFI and no GPU. The real one is `letibot_tokencore::VocabDecoder`.
#[derive(Debug, Clone, Default)]
pub struct TableDecoder {
    text: std::collections::HashMap<u32, String>,
    roles: std::collections::HashMap<u32, ControlRole>,
}

impl TableDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// An ordinary token: text, no role, never a boundary.
    pub fn with_text(mut self, id: u32, text: &str) -> Self {
        self.text.insert(id, text.to_string());
        self
    }

    /// A control token: text *and* the role that makes it a boundary.
    pub fn with_control(mut self, id: u32, token: &ControlToken) -> Self {
        self.text.insert(id, token.literal.clone().into_owned());
        self.roles.insert(id, token.role);
        self
    }
}

impl TokenDecoder for TableDecoder {
    fn decode(&self, tokens: &[u32]) -> String {
        let mut s = String::new();
        for id in tokens {
            match self.text.get(id) {
                Some(t) => s.push_str(t),
                // An id the decoder does not know is not silently dropped: it would
                // become invisible content, which is the failure class this whole
                // design exists to abolish.
                None => s.push_str(&format!("\u{fffd}<{id}>")),
            }
        }
        s
    }

    fn control_role(&self, token: u32) -> Option<ControlRole> {
        self.roles.get(&token).copied()
    }
}
