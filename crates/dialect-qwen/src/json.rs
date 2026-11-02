//! JSON text that is byte-identical to what the shipped template produces.
//!
//! Deliberately not shared with `dialect-glm`'s `json.rs`: the two templates call `tojson` on
//! different things. GLM unwraps `{"type":"function","function":{…}}` and re-emits three fixed
//! keys, Qwen writes the tool object whole with its `type` wrapper, and a shared helper would need a
//! flag, which is where the next mistake goes.
//!
//! What is shared is the formatter: Hugging Face's `tojson` is
//! `json.dumps(..., separators=(", ", ": "), ensure_ascii=False)`, and `serde_json`'s compact
//! writer uses `,` and `:` with no spaces, so a naive `to_string` diverges on every tool schema.

use serde_json::Value;
use serde_json::ser::Formatter;
use std::io;

/// `json.dumps(x, separators=(", ", ": "), ensure_ascii=False)`.
struct HfFormatter;

impl Formatter for HfFormatter {
    fn begin_array_value<W: ?Sized + io::Write>(
        &mut self,
        w: &mut W,
        first: bool,
    ) -> io::Result<()> {
        if first { Ok(()) } else { w.write_all(b", ") }
    }
    fn begin_object_key<W: ?Sized + io::Write>(
        &mut self,
        w: &mut W,
        first: bool,
    ) -> io::Result<()> {
        if first { Ok(()) } else { w.write_all(b", ") }
    }
    fn begin_object_value<W: ?Sized + io::Write>(&mut self, w: &mut W) -> io::Result<()> {
        w.write_all(b": ")
    }
}

/// Serialize as the shipped template's `tojson` filter would.
pub fn hf_tojson(value: &Value) -> String {
    let mut buf = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, HfFormatter);
    serde::Serialize::serialize(value, &mut ser).expect("serializing a Value cannot fail");
    String::from_utf8(buf).expect("serde_json emits UTF-8")
}

/// One line of Qwen's `<tools>` block, from an OpenAI-shaped tool schema.
///
/// The object as given, keys in their given order, nothing unwrapped and nothing normalised. Key
/// order is preserved because it is prompt bytes: reordering `name` and `description` re-prefills
/// the whole conversation.
///
/// It lives here rather than in the daemon because `StablePrefix.tools_json` holds finished text,
/// and there should be one implementation of that text.
pub fn qwen_tool_json(tool: &Value) -> String {
    hf_tojson(tool)
}

/// One `<parameter=…>` body.
///
/// `{{ args_value | string if args_value is string else args_value | tojson }}` — a
/// string argument is emitted **raw**, everything else as JSON. So
/// `{"path": "a.txt"}` renders `a.txt`, not `"a.txt"`, and `{"limit": 40}` renders
/// `40`.
pub fn parameter_value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => hf_tojson(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn separators_match_the_training_runtime() {
        assert_eq!(
            hf_tojson(&json!({"a": 1, "b": "two"})),
            r#"{"a": 1, "b": "two"}"#
        );
        assert_eq!(hf_tojson(&json!([1, 2, "x"])), r#"[1, 2, "x"]"#);
        assert_eq!(hf_tojson(&json!({})), "{}");
    }

    #[test]
    fn non_ascii_is_not_escaped() {
        assert_eq!(hf_tojson(&json!("héllo →")), "\"héllo →\"");
    }

    #[test]
    fn the_tool_object_is_not_unwrapped() {
        // GLM's renderer unwraps this and Qwen's does not. The difference is the
        // reason there are two functions rather than one with a flag.
        let t = json!({"type": "function", "function": {"name": "f"}});
        assert_eq!(
            qwen_tool_json(&t),
            r#"{"type": "function", "function": {"name": "f"}}"#
        );
    }

    #[test]
    fn a_string_parameter_is_emitted_raw() {
        assert_eq!(parameter_value_text(&json!("a.txt")), "a.txt");
        assert_eq!(parameter_value_text(&json!(40)), "40");
        assert_eq!(parameter_value_text(&json!(["a", "b"])), r#"["a", "b"]"#);
        assert_eq!(parameter_value_text(&json!(null)), "null");
    }
}
