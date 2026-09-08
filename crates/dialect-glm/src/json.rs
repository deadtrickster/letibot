//! JSON text that is byte-identical to what the shipped template produces.
//!
//! GLM's jinja calls `{{ v | tojson(ensure_ascii=False) }}` in two places — the
//! tool schema block and a tool call's non-string argument values. Both land in the
//! prompt verbatim, so **the separators are part of the prompt bytes**, not a
//! formatting preference.
//!
//! Hugging Face's `tojson` filter (`transformers/utils/chat_template_utils.py`) is
//! `json.dumps(..., separators=(", ", ": "), ensure_ascii=False)`, and llama.cpp's
//! jinja matches it. `serde_json`'s compact writer uses `,`/`:` with no spaces, so a
//! naive `to_string` diverges on every tool schema. Hence this formatter.
//!
//! Verified against `POST /apply-template` on 2026-09-09: `{"a": 1, "b": "two"}`,
//! `[1, 2, "x"]`, and `"héllo →"` unescaped.

use serde_json::Value;
use serde_json::ser::Formatter;
use std::io;

/// `json.dumps(x, separators=(", ", ": "), ensure_ascii=False)`.
struct HfFormatter;

impl Formatter for HfFormatter {
    fn begin_array_value<W: ?Sized + io::Write>(&mut self, w: &mut W, first: bool) -> io::Result<()> {
        if first { Ok(()) } else { w.write_all(b", ") }
    }
    fn begin_object_key<W: ?Sized + io::Write>(&mut self, w: &mut W, first: bool) -> io::Result<()> {
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

/// One line of GLM's `<tools>` block, from an OpenAI-shaped tool schema.
///
/// The template unwraps `{"type":"function","function":{…}}`, drops `defer_loading`
/// and `strict`, and emits the remaining keys **in the order llama.cpp produced
/// them**, which is `common_chat_tools_to_json_oaicompat`'s fixed order: `name`,
/// `description`, `parameters` — with `description` present as `""` even when the
/// caller omitted it. That normalisation is not the template's; it is the server's,
/// and a renderer that skips it emits a shorter line than the model was trained on.
///
/// This lives here rather than in the dialect because `StablePrefix.tools_json`
/// holds finished text: whoever fills that field must produce these bytes, and
/// there should be exactly one implementation of "these bytes".
pub fn glm_tool_json(tool: &Value) -> String {
    let f = tool.get("function").unwrap_or(tool);
    let mut out = serde_json::Map::new();
    out.insert(
        "name".into(),
        f.get("name").cloned().unwrap_or_else(|| Value::String(String::new())),
    );
    out.insert(
        "description".into(),
        f.get("description").cloned().unwrap_or_else(|| Value::String(String::new())),
    );
    out.insert(
        "parameters".into(),
        f.get("parameters").cloned().unwrap_or_else(|| Value::Object(Default::default())),
    );
    hf_tojson(&Value::Object(out))
}

/// One `<arg_value>` body.
///
/// `{{ v | tojson(ensure_ascii=False) if v is not string else v }}` — a string
/// argument is emitted **raw**, everything else as JSON. So `{"path":"a.txt"}`
/// renders `a.txt`, not `"a.txt"`.
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
        assert_eq!(hf_tojson(&json!({"a": 1, "b": "two"})), r#"{"a": 1, "b": "two"}"#);
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
