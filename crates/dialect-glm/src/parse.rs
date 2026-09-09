//! Token ids in, spans out.
//!
//! # Why the decoder is an argument
//!
//! `parse(&[u32]) -> Vec<ParsedSpan>` asked a crate with no vocab to produce
//! `Content(String)`. It could not, and the workaround was a builder that stashed a
//! decoder and a panic for callers who forgot. Under T2 the decoder is in the
//! contract ([`TokenDecoder`]) and is passed in, so the type says what parsing needs
//! and there is no way to call it wrong.
//!
//! # Boundaries are decided by token id, never by scanning text
//!
//! A control token is **one** vocab entry, so the decoder answers `control_role` for
//! exactly the ids that are boundaries. Text that merely spells `</think>` is several
//! ids and cannot be one — the same guarantee `RenderSpan`'s Text/Control split gives
//! on the way out, restated on the way back in.
//!
//! Runs of ordinary tokens are decoded **as runs**, not one id at a time: on a BPE
//! vocabulary, detokenizing a sequence and concatenating per-token pieces are
//! different operations, and only the first is right.
//!
//! # What roles buy here
//!
//! This parser used to compare decoded control text against `GLM_TOKENS` literals,
//! because eight of GLM's tokens had no `ControlRole` to match on. They have one now,
//! so the match below is on roles: it is a `match` the compiler checks rather than a
//! chain of string comparisons, and a second dialect that spells `<arg_key>`
//! differently would need no new code path.

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
    fn parse(&self, tokens: &[u32], decoder: &dyn TokenDecoder) -> Vec<ParsedSpan> {
        let mut out: Vec<ParsedSpan> = Vec::new();
        let mut mode = Mode::Content;
        let mut buf = String::new();
        let mut run: Vec<u32> = Vec::new();
        let mut call_name = String::new();
        let mut arg_key = String::new();
        let mut args: Map<String, Value> = Map::new();

        let flush_content = |buf: &mut String, out: &mut Vec<ParsedSpan>| {
            if !buf.is_empty() {
                out.push(ParsedSpan::Content(std::mem::take(buf)));
            }
        };

        for &id in tokens {
            let Some(role) = decoder.control_role(id) else {
                run.push(id);
                continue;
            };
            if !run.is_empty() {
                buf.push_str(&decoder.decode(&run));
                run.clear();
            }

            match role {
                ControlRole::ThinkOpen => {
                    flush_content(&mut buf, &mut out);
                    mode = Mode::Reasoning;
                }
                ControlRole::ThinkClose => {
                    // An empty think block is what an assistant turn with no reasoning
                    // renders as, so it is not recoverable as a distinct item.
                    // Documented as outside "the round-trippable parts".
                    if !buf.is_empty() {
                        out.push(ParsedSpan::Reasoning(std::mem::take(&mut buf)));
                    }
                    buf.clear();
                    mode = Mode::Content;
                }
                ControlRole::ToolCallOpen => {
                    flush_content(&mut buf, &mut out);
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
                    flush_content(&mut buf, &mut out);
                    out.push(ParsedSpan::Control(other));
                    mode = Mode::Content;
                }
            }
        }
        if !run.is_empty() {
            buf.push_str(&decoder.decode(&run));
        }
        flush_content(&mut buf, &mut out);
        out
    }
}

/// Recover an argument value from its `<arg_value>` body.
///
/// The render side emits strings raw and everything else as JSON, so the inverse is:
/// if the text parses as a non-string JSON value, it was one; otherwise it was a
/// string. That is lossy in exactly one corner — a **string** argument whose text is
/// itself valid JSON, e.g. `"3"` or `"[1]"`, comes back as a number or an array. The
/// corner is pinned by a test rather than papered over; the shipped format simply
/// does not carry the type.
fn reconstruct_arg(text: String) -> Value {
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::String(_)) | Err(_) => Value::String(text),
        Ok(v) => v,
    }
}

/// A decoder over a fixed table, for tests and for anyone who has literal→id already.
///
/// The real one is `letibot_tokencore::VocabDecoder`, which has a vocabulary. This one
/// exists so the round-trip property can be checked with no GGUF, no FFI and no GPU —
/// which is the whole reason `RenderSpan` is text plus control tokens rather than ids.
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
