//! Token ids in, spans out.
//!
//! CONTRACT-GAP-3: the trait's `parse(&[u32]) -> Vec<ParsedSpan>` asks a crate with
//! no vocab to produce `Content(String)`. It cannot. The caller supplies a
//! [`TokenDecoder`] instead — the token core already owns that map, and handing it in
//! keeps the FFI out of this crate, which was the point of taking `u32` in the first
//! place.
//!
//! Boundaries are decided **by token id**, not by scanning text: a control token is
//! one id, so a decoded chunk is a boundary if and only if the whole chunk is that
//! literal. Text that merely spells `</think>` cannot be one, which is the same
//! guarantee `RenderSpan`'s Text/Control split gives on the way out.

use crate::GLM_TOKENS;
use letibot_dialect::{ControlToken, ParsedSpan};
use serde_json::{Map, Value};

/// id → text. Implemented by the token core; a trivial map is enough for tests.
pub trait TokenDecoder {
    fn decode(&self, id: u32) -> Option<&str>;
}

impl<F> TokenDecoder for F
where
    F: Fn(u32) -> Option<&'static str>,
{
    fn decode(&self, id: u32) -> Option<&str> {
        self(id)
    }
}

fn control_for(s: &str) -> Option<ControlToken> {
    GLM_TOKENS.iter().copied().find(|t| t.literal == s)
}

#[derive(Debug, PartialEq)]
enum Mode {
    Content,
    Reasoning,
    ToolName,
    ArgKey,
    ArgValue,
}

pub(crate) fn parse(decoder: &(dyn TokenDecoder + Send + Sync), tokens: &[u32]) -> Vec<ParsedSpan> {
    use crate::tokens as tk;

    let mut out: Vec<ParsedSpan> = Vec::new();
    let mut mode = Mode::Content;
    let mut buf = String::new();
    let mut call_name = String::new();
    let mut arg_key = String::new();
    let mut args: Map<String, Value> = Map::new();

    let flush_content = |buf: &mut String, out: &mut Vec<ParsedSpan>| {
        if !buf.is_empty() {
            out.push(ParsedSpan::Content(std::mem::take(buf)));
        }
    };

    for &id in tokens {
        let Some(txt) = decoder.decode(id) else {
            // An id the decoder does not know is not silently dropped: it would
            // become invisible content, which is the failure class this whole design
            // exists to abolish.
            buf.push_str(&format!("\u{fffd}<{id}>"));
            continue;
        };

        let ctl = control_for(txt);
        let Some(ctl) = ctl else {
            buf.push_str(txt);
            continue;
        };

        match ctl.literal {
            l if l == tk::THINK_OPEN.literal => {
                flush_content(&mut buf, &mut out);
                mode = Mode::Reasoning;
            }
            l if l == tk::THINK_CLOSE.literal => {
                // An empty think block is what an assistant turn with no reasoning
                // renders as, so it is not recoverable as a distinct item. Documented
                // as outside "the round-trippable parts".
                if !buf.is_empty() {
                    out.push(ParsedSpan::Reasoning(std::mem::take(&mut buf)));
                }
                buf.clear();
                mode = Mode::Content;
            }
            l if l == tk::TOOL_CALL_OPEN.literal => {
                flush_content(&mut buf, &mut out);
                call_name.clear();
                args = Map::new();
                mode = Mode::ToolName;
            }
            l if l == tk::TOOL_CALL_CLOSE.literal => {
                if mode == Mode::ToolName {
                    call_name = std::mem::take(&mut buf);
                }
                buf.clear();
                out.push(ParsedSpan::ToolCall {
                    // GLM's wire format carries no call id. It is assigned by the
                    // harness, so it is not round-trippable and must not be invented.
                    id: None,
                    name: std::mem::take(&mut call_name),
                    arguments: serde_json::to_string(&Value::Object(std::mem::take(&mut args)))
                        .expect("a JSON object always serialises"),
                });
                mode = Mode::Content;
            }
            l if l == tk::ARG_KEY_OPEN.literal => {
                if mode == Mode::ToolName {
                    call_name = std::mem::take(&mut buf);
                }
                buf.clear();
                mode = Mode::ArgKey;
            }
            l if l == tk::ARG_KEY_CLOSE.literal => {
                arg_key = std::mem::take(&mut buf);
                mode = Mode::Content;
            }
            l if l == tk::ARG_VALUE_OPEN.literal => {
                buf.clear();
                mode = Mode::ArgValue;
            }
            l if l == tk::ARG_VALUE_CLOSE.literal => {
                args.insert(
                    std::mem::take(&mut arg_key),
                    reconstruct_arg(std::mem::take(&mut buf)),
                );
                mode = Mode::Content;
            }
            _ => {
                flush_content(&mut buf, &mut out);
                out.push(ParsedSpan::Control(ctl.role));
                mode = Mode::Content;
            }
        }
    }
    flush_content(&mut buf, &mut out);
    out
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
#[derive(Debug, Clone, Default)]
pub struct TableDecoder {
    table: std::collections::HashMap<u32, String>,
}

impl TableDecoder {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with(mut self, id: u32, text: &str) -> Self {
        self.table.insert(id, text.to_string());
        self
    }
}

impl TokenDecoder for TableDecoder {
    fn decode(&self, id: u32) -> Option<&str> {
        self.table.get(&id).map(String::as_str)
    }
}
