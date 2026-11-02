//! JSON text that is byte-identical to what the shipped GLM template produces.
//!
//! GLM's jinja calls `{{ v | tojson(ensure_ascii=False) }}` for the tool schema block
//! and for a tool call's non-string argument values, so the separators are prompt
//! bytes rather than a formatting preference. Hugging Face's `tojson` is
//! `json.dumps(..., separators=(", ", ": "), ensure_ascii=False)` and llama.cpp's
//! jinja matches it, while `serde_json`'s compact writer uses `,`/`:` - hence this
//! formatter.

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

/// One line of GLM's `<tools>` block, from an OpenAI-shaped tool schema. It must carry the
/// server's own normalisation (unwrapped, `description` defaulted to `""`, `name`/`description`/
/// `parameters` order) or the line is shorter than the model was trained on.
pub fn glm_tool_json(tool: &Value) -> String {
    let f = tool.get("function").unwrap_or(tool);
    let mut out = serde_json::Map::new();
    out.insert(
        "name".into(),
        f.get("name")
            .cloned()
            .unwrap_or_else(|| Value::String(String::new())),
    );
    out.insert(
        "description".into(),
        f.get("description")
            .cloned()
            .unwrap_or_else(|| Value::String(String::new())),
    );
    out.insert(
        "parameters".into(),
        f.get("parameters")
            .cloned()
            .unwrap_or_else(|| Value::Object(Default::default())),
    );
    hf_tojson(&Value::Object(out))
}

/// One `<arg_value>` body: a string argument is emitted raw, everything else as JSON,
/// so `{"path":"a.txt"}` renders `a.txt`. The template asks for exactly that.
pub fn arg_value_text(v: &Value) -> String {
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
    fn separators_match_the_shipped_template() {
        // Captured from POST /apply-template.
        assert_eq!(
            hf_tojson(&json!({"a": 1, "b": "two"})),
            r#"{"a": 1, "b": "two"}"#
        );
        assert_eq!(hf_tojson(&json!([1, 2, "x"])), r#"[1, 2, "x"]"#);
        assert_eq!(hf_tojson(&json!({})), "{}");
        assert_eq!(hf_tojson(&json!([])), "[]");
    }

    #[test]
    fn non_ascii_is_not_escaped() {
        assert_eq!(hf_tojson(&json!("héllo →")), "\"héllo →\"");
    }

    #[test]
    fn a_missing_description_is_an_empty_string_not_an_absent_key() {
        // The server fills it in before the template ever sees the tool. Dropping it
        // would shorten the stable prefix and diverge on every tools fixture.
        let t = json!({"type":"function","function":{"name":"f","parameters":{"type":"object"}}});
        assert_eq!(
            glm_tool_json(&t),
            r#"{"name": "f", "description": "", "parameters": {"type": "object"}}"#
        );
    }

    #[test]
    fn string_arguments_are_emitted_raw() {
        assert_eq!(arg_value_text(&json!("a.txt")), "a.txt");
        assert_eq!(arg_value_text(&json!(3)), "3");
        assert_eq!(arg_value_text(&json!(true)), "true");
        assert_eq!(arg_value_text(&json!(null)), "null");
    }
}
