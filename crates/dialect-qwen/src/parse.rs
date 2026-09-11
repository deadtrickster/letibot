//! Token ids → `ParsedSpan`s. The half of a dialect that a template cannot give us.
//!
//! A template says how a turn is *written*. There is no inverse, so this is code,
//! and it stays code after the template-driven renderer lands.
//!
//! # Boundaries by id, never by scanning text
//!
//! Every structural boundary Qwen uses is a single vocab entry, so the walk is over
//! ids and asks the decoder for a role. Text that merely *spells* `</think>` is
//! several ids and can never be mistaken for the boundary — the same guarantee
//! `RenderSpan` gives on the way out, restated on the way in.
//!
//! What is **not** a single entry is the tool call's body: `<function=`,
//! `<parameter=` and their closers are ordinary text. So the walk hands the decoded
//! body of a `<tool_call>…</tool_call>` block to [`parse_call_body`], which is a
//! small string parser, and everything it cannot read is preserved verbatim rather
//! than replaced with `{}` — §5.7 has to be able to tell a truncated argument from
//! an empty one, and that distinction is the difference between re-prompting the
//! model and executing a call it never finished writing.

use letibot_dialect::{ControlRole, ParsedSpan, Parser, TokenDecoder};
use serde_json::{Map, Value};

use crate::json::hf_tojson;

/// Qwen3.8's parser.
#[derive(Debug, Default, Clone, Copy)]
pub struct QwenParser;

impl Parser for QwenParser {
    fn parse(
        &self,
        tokens: &[u32],
        decoder: &dyn TokenDecoder,
        reasoning_open: bool,
    ) -> Vec<ParsedSpan> {
        let mut out = Vec::new();
        let mut buf = String::new();
        let mut run: Vec<u32> = Vec::new();
        let mut in_reasoning = reasoning_open;
        let mut in_call = false;
        // Offsets into `tokens` for the span ranges: where the buffered run began,
        // where the open reasoning block began, where the open tool call began. With
        // `reasoning_open` seeded the block began before this slice, so its range
        // starts at 0 and the caller's lead tokens stay the caller's to commit.
        let mut run_start = 0usize;
        let mut block_start = 0usize;
        let mut call_start = 0usize;

        for (i, &id) in tokens.iter().enumerate() {
            let Some(role) = decoder.control_role(id) else {
                if run.is_empty() {
                    run_start = i;
                }
                run.push(id);
                continue;
            };
            // Decode the accumulated run as a run: detokenization is not per-token
            // concatenation on a BPE vocabulary.
            if !run.is_empty() {
                buf.push_str(&decoder.decode(&run));
                run.clear();
            }
            match role {
                ControlRole::ThinkOpen => {
                    flush(&mut buf, &mut out, in_reasoning, run_start, i);
                    in_reasoning = true;
                    block_start = i;
                }
                ControlRole::ThinkClose => {
                    flush(&mut buf, &mut out, true, block_start, i + 1);
                    in_reasoning = false;
                }
                ControlRole::ToolCallOpen => {
                    flush(&mut buf, &mut out, in_reasoning, run_start, i);
                    in_call = true;
                    call_start = i;
                }
                ControlRole::ToolCallClose => {
                    let raw = std::mem::take(&mut buf);
                    out.push(parse_call_body(&raw, call_start..i + 1));
                    in_call = false;
                }
                other => {
                    // A boundary inside an unterminated call is not a boundary: the
                    // model is still writing arguments. Everything else flushes.
                    if !in_call {
                        flush(&mut buf, &mut out, in_reasoning, run_start, i);
                        out.push(ParsedSpan::Control {
                            role: other,
                            range: i..i + 1,
                        });
                    }
                }
            }
        }
        let end = tokens.len();
        if !run.is_empty() {
            buf.push_str(&decoder.decode(&run));
        }
        if in_call {
            // `<tool_call>` opened and never closed — a `length` truncation, almost
            // always. Reported as a call with the body verbatim in `arguments`, so
            // `LengthPolicy` sees an argument that does not parse and fails the batch
            // instead of executing half a call.
            out.push(ParsedSpan::ToolCall {
                id: None,
                name: function_name(&buf).unwrap_or_default(),
                arguments: buf,
                range: call_start..end,
            });
        } else {
            flush(&mut buf, &mut out, in_reasoning, run_start, end);
        }
        out
    }
}

fn flush(
    buf: &mut String,
    out: &mut Vec<ParsedSpan>,
    reasoning: bool,
    start: usize,
    end: usize,
) {
    if buf.is_empty() {
        return;
    }
    let text = std::mem::take(buf);
    let range = start..end;
    out.push(if reasoning {
        ParsedSpan::Reasoning { text, range }
    } else {
        ParsedSpan::Content { text, range }
    });
}

/// `\n<function=NAME>\n<parameter=K>\nV\n</parameter>\n</function>\n` → a call.
///
/// Written as a scan rather than a regex for the same reason `letibot-tools` has no
/// `regex` dependency: the grammar is fixed, it is six literals long, and a partial
/// match has to be reportable rather than approximated.
pub fn parse_call_body(raw: &str, range: std::ops::Range<usize>) -> ParsedSpan {
    let Some(name) = function_name(raw) else {
        // No `<function=…>` at all. Keep the bytes; something upstream will have to
        // decide whether this was truncation or a format the model invented.
        return ParsedSpan::ToolCall {
            id: None,
            name: String::new(),
            arguments: raw.trim().to_string(),
            range,
        };
    };

    let mut args = Map::new();
    let mut rest = raw;
    let mut truncated = false;
    while let Some(at) = rest.find("<parameter=") {
        rest = &rest[at + "<parameter=".len()..];
        let Some(gt) = rest.find(">\n") else {
            truncated = true;
            break;
        };
        let key = rest[..gt].to_string();
        rest = &rest[gt + 2..];
        let Some(end) = rest.find("\n</parameter>") else {
            // The value was cut off mid-write. Everything left is the value so far,
            // and the call is marked unparseable by returning the raw body.
            truncated = true;
            break;
        };
        let value = &rest[..end];
        args.insert(key, value_of(value));
        rest = &rest[end + "\n</parameter>".len()..];
    }

    if truncated {
        return ParsedSpan::ToolCall {
            id: None,
            name,
            arguments: raw.trim().to_string(),
            range,
        };
    }
    ParsedSpan::ToolCall {
        id: None,
        name,
        arguments: hf_tojson(&Value::Object(args)),
        range,
    }
}

fn function_name(raw: &str) -> Option<String> {
    let at = raw.find("<function=")? + "<function=".len();
    let rest = &raw[at..];
    let gt = rest.find('>')?;
    Some(rest[..gt].to_string())
}

/// One `<parameter=…>` body, back into JSON.
///
/// The template emits a string argument **raw** and everything else through
/// `tojson`, so the two are not distinguishable by looking at the bytes alone: `40`
/// is either the integer or the string. The rule here is round-trip exactness — a
/// value is taken as structured JSON only if re-rendering it produces the identical
/// bytes — and everything else is a string.
///
/// That leaves a real ambiguity: a string argument whose content happens to be
/// valid JSON (`"40"`, `"true"`, `"[1, 2]"`) parses back as the structured value.
/// It is **not** papered over: `letibot-tools`' argument salvage coerces against the
/// declared parameter type, so the schema — which knows which it is — settles it one
/// layer up, and records the coercion as a `Repair` rather than doing it silently.
fn value_of(text: &str) -> Value {
    match serde_json::from_str::<Value>(text) {
        Ok(v) if !v.is_string() && hf_tojson(&v) == text => v,
        _ => Value::String(text.to_string()),
    }
}

/// A [`TokenDecoder`] over an explicit table, for tests and for the fidelity tools.
///
/// The real one is `letibot_tokencore::VocabDecoder`. This exists so the parser can
/// be property-tested with no vocabulary, no FFI and no GPU — which is the whole
/// reason `TokenDecoder` is in the contract.
pub struct TableDecoder {
    entries: Vec<(u32, String, Option<ControlRole>)>,
}

impl TableDecoder {
    pub fn new() -> Self {
        let mut entries = Vec::new();
        for (i, t) in crate::QWEN_TOKENS.iter().enumerate() {
            entries.push((i as u32, t.literal.to_string(), Some(t.role)));
        }
        TableDecoder { entries }
    }

    /// Register an ordinary (non-control) token.
    pub fn word(&mut self, text: &str) -> u32 {
        if let Some((id, _, _)) = self.entries.iter().find(|(_, t, r)| t == text && r.is_none()) {
            return *id;
        }
        let id = self.entries.len() as u32;
        self.entries.push((id, text.to_string(), None));
        id
    }

    pub fn control(&self, literal: &str) -> u32 {
        self.entries
            .iter()
            .find(|(_, t, r)| t == literal && r.is_some())
            .map(|(id, _, _)| *id)
            .unwrap_or_else(|| panic!("{literal} is not in the Qwen control table"))
    }

    /// Tokenize a string as one ordinary token per line-ish chunk, splitting on the
    /// control literals. Enough for a round-trip test and nothing more.
    pub fn encode(&mut self, s: &str) -> Vec<u32> {
        let mut out = Vec::new();
        let mut rest = s;
        'outer: while !rest.is_empty() {
            for t in crate::QWEN_TOKENS {
                let lit: &str = t.literal.as_ref();
                if let Some(at) = rest.find(lit) {
                    if at > 0 {
                        let head = rest[..at].to_string();
                        out.push(self.word(&head));
                    }
                    out.push(self.control(lit));
                    rest = &rest[at + lit.len()..];
                    continue 'outer;
                }
            }
            out.push(self.word(rest));
            break;
        }
        out
    }
}

impl Default for TableDecoder {
    fn default() -> Self {
        TableDecoder::new()
    }
}

impl TokenDecoder for TableDecoder {
    fn decode(&self, tokens: &[u32]) -> String {
        tokens
            .iter()
            .map(|id| {
                self.entries
                    .iter()
                    .find(|(e, _, _)| e == id)
                    .map(|(_, t, _)| t.as_str())
                    .unwrap_or("\u{fffd}")
            })
            .collect()
    }

    fn control_role(&self, token: u32) -> Option<ControlRole> {
        self.entries
            .iter()
            .find(|(e, _, _)| *e == token)
            .and_then(|(_, _, r)| *r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::{QwenRenderer, generation_prompt};
    use letibot_dialect::{StablePrefix, spans_to_string};
    use letibot_transcript::{ReasoningField, ToolCall, TranscriptItem};

    fn roundtrip(text: &str) -> Vec<ParsedSpan> {
        let mut d = TableDecoder::new();
        let ids = d.encode(text);
        QwenParser.parse(&ids, &d, false)
    }

    #[test]
    fn a_generated_turn_parses_into_reasoning_content_and_a_call() {
        let spans = roundtrip(
            "Let me read it.\n</think>\n\nOn it.\n\n\
             <tool_call>\n<function=read>\n<parameter=path>\nsrc/main.rs\n</parameter>\n\
             </function>\n</tool_call>",
        );
        // The generation prompt already opened `<think>`, so a live stream starts
        // inside reasoning. Here the text before `</think>` is content because no
        // open token preceded it; the engine's `lead_opens_reasoning` handles that.
        let call = spans
            .iter()
            .find_map(|s| match s {
                ParsedSpan::ToolCall { name, arguments, .. } => Some((name, arguments)),
                _ => None,
            })
            .expect("a tool call");
        assert_eq!(call.0, "read");
        assert_eq!(call.1, r#"{"path": "src/main.rs"}"#);
    }

    #[test]
    fn parse_after_render_recovers_the_call_it_rendered() {
        // `parse ∘ render ≡ id` on the round-trippable part.
        let items = vec![
            TranscriptItem::Reasoning {
                text: "Read it.".into(),
                field: ReasoningField::Inline,
            },
            TranscriptItem::Assistant {
                text: "Here goes.".into(),
                tool_calls: vec![ToolCall {
                    id: "call_0".into(),
                    name: "grep".into(),
                    arguments: r#"{"pattern": "fn main", "glob": "*.rs"}"#.into(),
                }],
                truncated: false,
            },
        ];
        let r = QwenRenderer::new();
        let prefix = StablePrefix {
            system: "s".into(),
            tools_json: vec![],
        };
        let whole = spans_to_string(&r.render(&prefix, &items));
        // Only the assistant turn's own bytes, which is what a stream carries.
        let turn = whole.split("<|im_start|>assistant\n").nth(1).unwrap();
        let spans = roundtrip(turn);
        assert!(
            spans
                .iter()
                .any(|s| matches!(s, ParsedSpan::Reasoning { text, .. } if text == "\nRead it.\n")),
            "{spans:?}"
        );
        assert!(
            spans.iter().any(|s| matches!(s, ParsedSpan::ToolCall { name, arguments, .. }
                if name == "grep" && arguments == r#"{"pattern": "fn main", "glob": "*.rs"}"#)),
            "{spans:?}"
        );
    }

    #[test]
    fn a_truncated_call_keeps_its_bytes_and_does_not_become_an_empty_one() {
        // The §5.7 case. `{}` here would mean executing a call the model never
        // finished writing.
        let spans = roundtrip("<tool_call>\n<function=read>\n<parameter=path>\nsrc/ma");
        let ParsedSpan::ToolCall { name, arguments, .. } = &spans[spans.len() - 1] else {
            panic!("{spans:?}");
        };
        assert_eq!(name, "read");
        assert!(arguments.contains("src/ma"), "{arguments}");
        assert!(serde_json::from_str::<Value>(arguments).is_err(), "{arguments}");
    }

    #[test]
    fn a_numeric_looking_string_is_reported_as_the_number_and_the_schema_settles_it() {
        // Declared, not hidden: the template renders a string raw, so the bytes are
        // genuinely ambiguous. `letibot-tools` coerces against the declared type.
        assert_eq!(value_of("40"), Value::from(40));
        assert_eq!(value_of("a.txt"), Value::from("a.txt"));
        assert_eq!(value_of("[1, 2]"), serde_json::json!([1, 2]));
        // Not round-trip exact under `tojson`, so it stays a string.
        assert_eq!(value_of("[1,2]"), Value::from("[1,2]"));
    }

    #[test]
    fn the_generation_prompt_round_trips_as_control_tokens() {
        let mut d = TableDecoder::new();
        let ids = d.encode(&spans_to_string(&generation_prompt()));
        assert_eq!(d.control_role(ids[0]), Some(ControlRole::Other));
        assert!(ids.iter().any(|id| d.control_role(*id) == Some(ControlRole::ThinkOpen)));
    }
}
