//! Clause 2: **malformed input is salvaged, not rejected**, and what was repaired
//! is said out loud.
//!
//! > oracle's shim took leaked `<function=…>` tool calls from 33% dropped to 0% by
//! > parsing them rather than asking for better formatting. Our targets are the
//! > same size class of model.
//!
//! Two halves, and the second one is the half people forget:
//!
//! 1. A best-effort parse. Envelope stripping, then a lenient JSON reader, then a
//!    shape and type normalisation against the tool's own schema.
//! 2. **An explicit note of what was repaired**, carried on the result and shown to
//!    the model. A silent repair teaches the model that its malformed spelling
//!    works, and the next call is malformed the same way — for the rest of the
//!    session, in permanent context.
//!
//! The one thing this module must never do is guess *semantics*. It will turn
//! `'path'` into `"path"` and `"40"` into `40`; it will not turn a missing `path`
//! into a plausible one. §9.4's rule — the harness must not silently improve a
//! tool's query — is the same rule one layer down.

use serde_json::{Map, Value};

use crate::schema::ToolSchema;

/// One thing that was wrong with the model's arguments and was fixed anyway.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repair {
    /// A stable code, so tests assert on the kind and not the wording.
    pub code: &'static str,
    /// What was done, in the words the model will read.
    pub detail: String,
}

impl Repair {
    fn new(code: &'static str, detail: impl Into<String>) -> Self {
        Repair {
            code,
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for Repair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}

/// Arguments that were understood, and the price of understanding them.
#[derive(Debug, Clone, PartialEq)]
pub struct Salvaged {
    pub value: Value,
    pub repairs: Vec<Repair>,
}

/// The arguments could not be made into an object at all.
///
/// Carries the corrective text rather than only the complaint, because clause 1
/// applies to argument parsing as much as to searching: a bare "invalid JSON"
/// leaves the model nothing to correct itself with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SalvageError {
    pub reason: String,
    pub guidance: String,
}

impl std::fmt::Display for SalvageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}\n{}", self.reason, self.guidance)
    }
}

/// Parse whatever the model emitted into this tool's arguments.
pub fn salvage(raw: &str, schema: &ToolSchema) -> Result<Salvaged, SalvageError> {
    let mut repairs = Vec::new();
    let body = strip_envelopes(raw, &mut repairs);

    let value = match serde_json::from_str::<Value>(&body) {
        Ok(v) => v,
        Err(_) => match lenient::parse(&body, &mut repairs) {
            // A top-level *bare word* is prose, not an argument. The lenient
            // reader will happily hand back `"please read the file for me"` as a
            // string, and dropping that into the tool's one required parameter
            // would be the harness inventing a path. §9.4's rule: salvage
            // spelling, never meaning.
            Some(Value::String(_)) if repairs.iter().any(|r| r.code == "json.unquoted_string") => {
                return Err(no_parse(raw, schema));
            }
            Some(v) => v,
            None => {
                return Err(no_parse(raw, schema));
            }
        },
    };

    let mut object = match into_object(value, schema, &mut repairs) {
        Some(o) => o,
        None => return Err(no_parse(raw, schema)),
    };

    unwrap_double_envelope(&mut object, schema, &mut repairs);
    rename_near_misses(&mut object, schema, &mut repairs);
    coerce_types(&mut object, schema, &mut repairs);

    Ok(Salvaged {
        value: Value::Object(object),
        repairs,
    })
}

fn no_parse(raw: &str, schema: &ToolSchema) -> SalvageError {
    let mut params = String::new();
    for (i, name) in schema.param_names().iter().enumerate() {
        if i > 0 {
            params.push_str(", ");
        }
        params.push_str(name);
        if let Some(t) = schema.param_type(name) {
            params.push_str(&format!(": {t}"));
        }
        if schema.required().contains(name) {
            params.push_str(" (required)");
        }
    }
    SalvageError {
        reason: format!(
            "the arguments to `{}` could not be read as an object, even leniently; \
             {} bytes were received",
            schema.name,
            raw.len()
        ),
        guidance: format!(
            "call `{}` again with a JSON object. Its parameters are: {params}",
            schema.name
        ),
    }
}

/// Peel the wrappers a model puts around its arguments when the harness's own tool
/// syntax leaked into ordinary text.
fn strip_envelopes(raw: &str, repairs: &mut Vec<Repair>) -> String {
    let mut s = raw.trim().to_string();

    // `<function=name>{…}</function>` — the exact shape oracle measured.
    if let Some(rest) = s.strip_prefix("<function=")
        && let Some(gt) = rest.find('>')
    {
        let name = &rest[..gt];
        let mut inner = rest[gt + 1..].to_string();
        if let Some(end) = inner.find("</function>") {
            inner.truncate(end);
        }
        repairs.push(Repair::new(
            "envelope.function",
            format!("the arguments arrived wrapped in `<function={name}>…</function>`"),
        ));
        s = inner.trim().to_string();
    }

    // `<tool_call>…</tool_call>` and `<arguments>…</arguments>`.
    for (open, close) in [
        ("<tool_call>", "</tool_call>"),
        ("<arguments>", "</arguments>"),
        ("<parameters>", "</parameters>"),
    ] {
        if let Some(rest) = s.strip_prefix(open) {
            let inner = rest.split(close).next().unwrap_or(rest);
            repairs.push(Repair::new(
                "envelope.tag",
                format!("the arguments arrived wrapped in `{open}…{close}`"),
            ));
            s = inner.trim().to_string();
        }
    }

    // A markdown fence. Models fence JSON because JSON is usually for humans.
    if s.starts_with("```") {
        let after = s.trim_start_matches('`');
        let after = match after.find('\n') {
            // The first line is the language tag, if there is one.
            Some(nl) if !after[..nl].contains('{') => &after[nl + 1..],
            _ => after,
        };
        let inner = after.split("```").next().unwrap_or(after);
        repairs.push(Repair::new(
            "envelope.fence",
            "the arguments arrived inside a markdown code fence",
        ));
        s = inner.trim().to_string();
    }

    // Prose either side of a balanced object. Taken last so it also catches the
    // remains of a half-stripped envelope.
    if !s.starts_with('{')
        && let Some((start, end)) = balanced_object(&s)
    {
        repairs.push(Repair::new(
            "envelope.prose",
            "the arguments were embedded in surrounding text",
        ));
        s = s[start..end].to_string();
    }

    s
}

/// The first balanced `{…}` span, ignoring braces inside strings.
fn balanced_object(s: &str) -> Option<(usize, usize)> {
    let b = s.as_bytes();
    let start = s.find('{')?;
    let mut depth = 0usize;
    let mut in_str = false;
    let mut escaped = false;
    for (i, &c) in b.iter().enumerate().skip(start) {
        if in_str {
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                in_str = false;
            }
            continue;
        }
        match c {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((start, i + 1));
                }
            }
            _ => {}
        }
    }
    None
}

/// Make an object out of whatever came back, where that is honest.
fn into_object(
    value: Value,
    schema: &ToolSchema,
    repairs: &mut Vec<Repair>,
) -> Option<Map<String, Value>> {
    match value {
        Value::Object(o) => Some(o),
        // A bare scalar, when the tool takes exactly one required parameter, is
        // unambiguous: there is only one place it can go. With two, it is a guess,
        // and a guess is what this module does not do.
        v @ (Value::String(_) | Value::Number(_) | Value::Bool(_)) => {
            let required = schema.required();
            if required.len() == 1 {
                let key = required[0].to_string();
                repairs.push(Repair::new(
                    "shape.bare_value",
                    format!(
                        "a bare value was given instead of an object; it was read as `{key}`, \
                         the tool's only required parameter"
                    ),
                ));
                let mut o = Map::new();
                o.insert(key, v);
                Some(o)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// `{"name": "read", "arguments": {…}}` — the model echoing the whole call into
/// the arguments slot. Common, and unambiguous when the inner object is there.
fn unwrap_double_envelope(
    o: &mut Map<String, Value>,
    schema: &ToolSchema,
    repairs: &mut Vec<Repair>,
) {
    for key in ["arguments", "parameters", "args", "input"] {
        let has_own_params = schema.param_names().iter().any(|p| o.contains_key(*p));
        if has_own_params || !o.contains_key(key) {
            continue;
        }
        let inner = o.get(key).cloned().unwrap_or(Value::Null);
        // The arguments as a *string* of JSON — the other half of the same habit.
        let inner = match inner {
            Value::String(s) => serde_json::from_str::<Value>(&s)
                .ok()
                .or_else(|| lenient::parse(&s, &mut Vec::new()))
                .unwrap_or(Value::Null),
            v => v,
        };
        if let Value::Object(m) = inner {
            repairs.push(Repair::new(
                "shape.double_envelope",
                format!("the arguments were nested under a `{key}` key and were unwrapped"),
            ));
            *o = m;
            return;
        }
    }
}

/// A key the schema does not have, spelled close enough to one it does.
///
/// Case and separators only, plus a short synonym table. Not fuzzy matching: a
/// tool called with `paht` is a tool called wrongly, and inventing the fix is how
/// a harness starts answering questions the model did not ask.
fn rename_near_misses(o: &mut Map<String, Value>, schema: &ToolSchema, repairs: &mut Vec<Repair>) {
    const SYNONYMS: &[(&str, &str)] = &[
        ("file", "path"),
        ("file_path", "path"),
        ("filename", "path"),
        ("filepath", "path"),
        ("dir", "path"),
        ("directory", "path"),
        ("q", "query"),
        ("question", "query"),
        ("search", "query"),
        ("regex", "pattern"),
        ("term", "pattern"),
        ("needle", "pattern"),
        ("glob", "pattern"),
        ("root", "path"),
        ("scope", "path"),
        ("max", "limit"),
        ("count", "limit"),
        ("n", "limit"),
        ("start", "offset"),
        ("skip", "offset"),
    ];

    let known: Vec<String> = schema.param_names().iter().map(|s| s.to_string()).collect();
    let unknown: Vec<String> = o.keys().filter(|k| !known.contains(k)).cloned().collect();

    for key in unknown {
        let norm = normalise(&key);
        let target = known
            .iter()
            .find(|k| normalise(k) == norm)
            .cloned()
            .or_else(|| {
                SYNONYMS
                    .iter()
                    .find(|(from, to)| normalise(from) == norm && known.iter().any(|k| k == to))
                    .map(|(_, to)| (*to).to_string())
            });
        let Some(target) = target else { continue };
        if o.contains_key(&target) {
            continue;
        }
        let v = o.remove(&key).unwrap_or(Value::Null);
        repairs.push(Repair::new(
            "key.renamed",
            format!("`{key}` is not a parameter of this tool; it was read as `{target}`"),
        ));
        o.insert(target, v);
    }
}

fn normalise(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// `"40"` where the schema says integer, `"true"` where it says boolean, a scalar
/// where it says array. All three are spellings, not meanings.
fn coerce_types(o: &mut Map<String, Value>, schema: &ToolSchema, repairs: &mut Vec<Repair>) {
    let names: Vec<String> = o.keys().cloned().collect();
    for name in names {
        let Some(want) = schema.param_type(&name) else {
            continue;
        };
        let have = o.get(&name).cloned().unwrap_or(Value::Null);
        let fixed = match (want, &have) {
            ("integer" | "number", Value::String(s)) => s
                .trim()
                .parse::<i64>()
                .ok()
                .map(|n| Value::Number(n.into()))
                .or_else(|| {
                    s.trim()
                        .parse::<f64>()
                        .ok()
                        .and_then(serde_json::Number::from_f64)
                        .map(Value::Number)
                }),
            ("boolean", Value::String(s)) => match s.trim().to_lowercase().as_str() {
                "true" | "yes" | "1" => Some(Value::Bool(true)),
                "false" | "no" | "0" => Some(Value::Bool(false)),
                _ => None,
            },
            ("string", Value::Number(n)) => Some(Value::String(n.to_string())),
            ("array", v) if !v.is_array() && !v.is_null() => Some(Value::Array(vec![v.clone()])),
            _ => None,
        };
        if let Some(fixed) = fixed {
            repairs.push(Repair::new(
                "type.coerced",
                format!(
                    "`{name}` was given as {} and read as {want}",
                    kind_of(&have)
                ),
            ));
            o.insert(name, fixed);
        }
    }
}

fn kind_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// A JSON reader that accepts what models actually emit.
///
/// Single quotes, unquoted keys, Python's `True`/`False`/`None`, `=` for `:`,
/// trailing commas, and a document that simply stops. Every deviation is recorded
/// as a repair; nothing is accepted quietly.
mod lenient {
    use super::Repair;
    use serde_json::{Map, Value};

    pub fn parse(s: &str, repairs: &mut Vec<Repair>) -> Option<Value> {
        let mut p = P {
            b: s.as_bytes(),
            i: 0,
            repairs,
            noted: Vec::new(),
        };
        p.ws();
        let v = p.value()?;
        Some(v)
    }

    struct P<'a> {
        b: &'a [u8],
        i: usize,
        repairs: &'a mut Vec<Repair>,
        noted: Vec<&'static str>,
    }

    impl P<'_> {
        /// One repair per *kind*, however many times it occurs: a model that
        /// quoted twelve keys with apostrophes made one mistake, not twelve.
        fn note(&mut self, code: &'static str, detail: &str) {
            if self.noted.contains(&code) {
                return;
            }
            self.noted.push(code);
            self.repairs.push(Repair {
                code,
                detail: detail.to_string(),
            });
        }

        fn ws(&mut self) {
            while self.i < self.b.len() && (self.b[self.i] as char).is_whitespace() {
                self.i += 1;
            }
        }

        fn peek(&self) -> Option<u8> {
            self.b.get(self.i).copied()
        }

        fn value(&mut self) -> Option<Value> {
            self.ws();
            match self.peek()? {
                b'{' => self.object(),
                b'[' => self.array(),
                b'"' | b'\'' | b'`' => self.string().map(Value::String),
                _ => self.bare(),
            }
        }

        fn object(&mut self) -> Option<Value> {
            self.i += 1; // '{'
            let mut m = Map::new();
            loop {
                self.ws();
                match self.peek() {
                    None => {
                        self.note(
                            "json.unclosed",
                            "the arguments object was not closed; it was closed at the end of the input",
                        );
                        return Some(Value::Object(m));
                    }
                    Some(b'}') => {
                        self.i += 1;
                        return Some(Value::Object(m));
                    }
                    Some(b',') => {
                        self.i += 1;
                        continue;
                    }
                    _ => {}
                }
                let key = match self.peek()? {
                    b'"' | b'\'' | b'`' => self.string()?,
                    _ => {
                        let k = self.ident()?;
                        self.note(
                            "json.unquoted_key",
                            "a key was not quoted; it was read as a string",
                        );
                        k
                    }
                };
                self.ws();
                match self.peek() {
                    Some(b':') => self.i += 1,
                    Some(b'=') => {
                        self.i += 1;
                        self.note(
                            "json.equals",
                            "`=` was used instead of `:` between a key and its value",
                        );
                    }
                    _ => {
                        // A key with no value at all. Recorded and skipped rather
                        // than guessed at.
                        self.note(
                            "json.missing_value",
                            "a key was given with no value and was dropped",
                        );
                        continue;
                    }
                }
                // A value that cannot be read ends the object rather than the
                // parse: the keys already understood are worth more than a clean
                // failure, which is the whole of clause 2.
                let Some(v) = self.value() else {
                    self.note(
                        "json.missing_value",
                        "a value could not be read and the object was closed there",
                    );
                    return Some(Value::Object(m));
                };
                m.insert(key, v);
            }
        }

        fn array(&mut self) -> Option<Value> {
            self.i += 1; // '['
            let mut a = Vec::new();
            loop {
                self.ws();
                match self.peek() {
                    None => {
                        self.note(
                            "json.unclosed",
                            "an array was not closed; it was closed at the end of the input",
                        );
                        return Some(Value::Array(a));
                    }
                    Some(b']') => {
                        self.i += 1;
                        return Some(Value::Array(a));
                    }
                    Some(b',') => {
                        self.i += 1;
                        continue;
                    }
                    _ => {}
                }
                a.push(self.value()?);
            }
        }

        fn string(&mut self) -> Option<String> {
            let quote = self.peek()?;
            if quote != b'"' {
                self.note(
                    "json.quotes",
                    "a string was written with the wrong quote character and was re-read",
                );
            }
            self.i += 1;
            let mut out = String::new();
            while self.i < self.b.len() {
                let c = self.b[self.i];
                if c == b'\\' {
                    self.i += 1;
                    let e = self.b.get(self.i).copied()?;
                    out.push(match e {
                        b'n' => '\n',
                        b't' => '\t',
                        b'r' => '\r',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'u' => {
                            let hex = self.b.get(self.i + 1..self.i + 5)?;
                            let code =
                                u32::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?;
                            self.i += 4;
                            char::from_u32(code).unwrap_or('\u{fffd}')
                        }
                        other => other as char,
                    });
                    self.i += 1;
                    continue;
                }
                if c == quote {
                    self.i += 1;
                    return Some(out);
                }
                // Multi-byte UTF-8 passes through whole.
                let len = utf8_len(c);
                let piece = std::str::from_utf8(self.b.get(self.i..self.i + len)?).ok()?;
                out.push_str(piece);
                self.i += len;
            }
            self.note(
                "json.unterminated_string",
                "a string was not closed; it was closed at the end of the input",
            );
            Some(out)
        }

        fn ident(&mut self) -> Option<String> {
            let start = self.i;
            while self.i < self.b.len()
                && (self.b[self.i].is_ascii_alphanumeric()
                    || self.b[self.i] == b'_'
                    || self.b[self.i] == b'-')
            {
                self.i += 1;
            }
            if self.i == start {
                // Not an identifier at all — skip one byte so the loop cannot spin.
                self.i += 1;
                return None;
            }
            Some(String::from_utf8_lossy(&self.b[start..self.i]).into_owned())
        }

        /// An unquoted value: a number, a keyword, or a bare word.
        fn bare(&mut self) -> Option<Value> {
            let start = self.i;
            while self.i < self.b.len() && !matches!(self.b[self.i], b',' | b'}' | b']' | b'\n') {
                self.i += 1;
            }
            let raw = std::str::from_utf8(&self.b[start..self.i]).ok()?.trim();
            if raw.is_empty() {
                return None;
            }
            Some(match raw {
                "true" | "True" => Value::Bool(true),
                "false" | "False" => Value::Bool(false),
                "null" | "None" | "nil" => Value::Null,
                _ => {
                    if let Ok(n) = raw.parse::<i64>() {
                        Value::Number(n.into())
                    } else if let Some(n) = raw
                        .parse::<f64>()
                        .ok()
                        .and_then(serde_json::Number::from_f64)
                    {
                        Value::Number(n)
                    } else {
                        self.note(
                            "json.unquoted_string",
                            "a value was not quoted; it was read as a string",
                        );
                        Value::String(raw.to_string())
                    }
                }
            })
        }
    }

    fn utf8_len(first: u8) -> usize {
        match first {
            0x00..=0x7f => 1,
            0xc0..=0xdf => 2,
            0xe0..=0xef => 3,
            _ => 4,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::Access;

    fn read_schema() -> ToolSchema {
        ToolSchema::new(
            "read",
            "Return the text of a file.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "offset": {"type": "integer"},
                    "limit": {"type": "integer"},
                },
                "required": ["path"],
            }),
            Access::Read,
        )
    }

    #[test]
    fn clean_json_is_not_repaired() {
        let s = salvage(r#"{"path":"src/lib.rs"}"#, &read_schema()).unwrap();
        assert_eq!(s.repairs, vec![]);
        assert_eq!(s.value["path"], "src/lib.rs");
    }

    #[test]
    fn the_leaked_function_envelope_is_parsed() {
        // The exact shape oracle measured at a 33% drop rate.
        let s = salvage(
            r#"<function=read>{"path": "src/lib.rs"}</function>"#,
            &read_schema(),
        )
        .unwrap();
        assert_eq!(s.value["path"], "src/lib.rs");
        assert!(s.repairs.iter().any(|r| r.code == "envelope.function"));
    }

    #[test]
    fn python_spelling_survives() {
        let s = salvage(
            "{'path': 'a.rs', 'limit': 40, 'raw': True,}",
            &read_schema(),
        )
        .unwrap();
        assert_eq!(s.value["path"], "a.rs");
        assert_eq!(s.value["limit"], 40);
        assert!(s.repairs.iter().any(|r| r.code == "json.quotes"));
    }

    #[test]
    fn an_unclosed_object_is_closed_and_said_so() {
        let s = salvage(r#"{"path": "a.rs", "limit": 12"#, &read_schema()).unwrap();
        assert_eq!(s.value["limit"], 12);
        assert!(s.repairs.iter().any(|r| r.code == "json.unclosed"));
    }

    #[test]
    fn a_double_envelope_is_unwrapped() {
        let s = salvage(
            r#"{"name": "read", "arguments": "{\"path\": \"a.rs\"}"}"#,
            &read_schema(),
        )
        .unwrap();
        assert_eq!(s.value["path"], "a.rs");
        assert!(s.repairs.iter().any(|r| r.code == "shape.double_envelope"));
    }

    #[test]
    fn a_near_miss_key_is_renamed_and_a_wrong_one_is_not() {
        let s = salvage(r#"{"file_path": "a.rs"}"#, &read_schema()).unwrap();
        assert_eq!(s.value["path"], "a.rs");
        assert!(s.repairs.iter().any(|r| r.code == "key.renamed"));

        // `paht` is a call made wrongly, and inventing the fix is exactly what
        // §9.4 forbids one layer up.
        let s = salvage(r#"{"paht": "a.rs"}"#, &read_schema()).unwrap();
        assert!(s.value.get("path").is_none());
    }

    #[test]
    fn types_are_coerced_against_the_schema() {
        let s = salvage(r#"{"path": "a.rs", "limit": "40"}"#, &read_schema()).unwrap();
        assert_eq!(s.value["limit"], 40);
        assert!(s.repairs.iter().any(|r| r.code == "type.coerced"));
    }

    #[test]
    fn a_bare_value_lands_in_the_only_required_parameter() {
        let s = salvage("\"src/lib.rs\"", &read_schema()).unwrap();
        assert_eq!(s.value["path"], "src/lib.rs");
        assert!(s.repairs.iter().any(|r| r.code == "shape.bare_value"));
    }

    #[test]
    fn prose_around_the_object_is_stripped() {
        let s = salvage(
            "Sure, here are the arguments: {\"path\": \"a.rs\"} — let me know.",
            &read_schema(),
        )
        .unwrap();
        assert_eq!(s.value["path"], "a.rs");
    }

    #[test]
    fn what_cannot_be_parsed_comes_back_with_the_parameter_list() {
        let e = salvage("please read the file for me", &read_schema()).unwrap_err();
        assert!(e.guidance.contains("path"), "{}", e.guidance);
        assert!(e.guidance.contains("required"), "{}", e.guidance);
    }
}
