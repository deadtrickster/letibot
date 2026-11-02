//! JSON text that is byte-identical to what the shipped template produces.
//!
//! GLM's jinja calls `tojson` on the tool schema block and on a tool call's non-string argument
//! values, and both land in the prompt verbatim, so the separators are part of the prompt bytes
//! rather than a formatting preference.
//!
//! Hugging Face's `tojson` is `json.dumps(..., separators=(", ", ": "), ensure_ascii=False)` and
//! llama.cpp's jinja matches it. `serde_json`'s compact writer uses `,` and `:` with no spaces, so a
//! naive `to_string` diverges on every tool schema. Hence this formatter.

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

/// One line of GLM's `<tools>` block: the template unwraps the function and emits `name`,
/// `description`, `parameters` in that fixed order, with `description` present as `""` when the
/// caller omitted it. That normalisation is the server's, and skipping it emits a shorter line than
/// the model was trained on.
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

/// One `<arg_value>` body: a string argument is emitted raw and everything else as JSON, so
/// `{"path":"a.txt"}` renders `a.txt`.
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
        // Captured from POST /apply-template, 2026-09-09.
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
