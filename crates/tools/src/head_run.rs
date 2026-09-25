//! **What a head needs to know to offer a tool to the operator without holding a schema.**
//!
//! R31 and R32, and the two are one publication. R31 asks for `/web_search blabla` to work
//! while the head knows nothing about `web_search`; R32 asks for Tab to complete a path for
//! `/read` while the head knows nothing about `read`. Both are answered by the same three
//! facts per tool, derived from **the tool's own declared parameters** — so a schema change
//! moves the head's behaviour with no head rebuilt:
//!
//! * **which field a bare line goes into**, from `parameters.required`;
//! * **what KIND that field is**, so a head can complete it;
//! * **which fields have defaults**, so a bare line sends the same object a model's call
//!   would have sent.
//!
//! # Why the kind is not read off the schema's `type`
//!
//! Every one of the three door tools declares its field as `{"type": "string"}` and nothing
//! more: `read`'s `path`, `web_fetch`'s `url`, `web_search`'s `query`. JSON Schema has a
//! `format` keyword for exactly this and none of them sets it. So `type` cannot tell a path
//! from a URL from free text, and the kind has to come from somewhere else.
//!
//! It comes from **the field's name**, by a rule stated once and tested against the three
//! real schemas: a field called `path`, `file`, `dir` or `directory` is a path; one called
//! `url`, `uri`, `link` or `host` is a URL; anything else is text. `format` wins when it is
//! declared, so a tool that says `"format": "uri"` is believed over its own field name.
//!
//! **A heuristic, in the daemon, and that is the point.** The alternative shapes are a
//! hand-written table in a head (drifts), a hand-written table here (a second copy of the
//! schemas, and untested against them), or nothing at all (the operator types
//! `crates/tui/src/app.rs` by hand and gets it wrong). One heuristic on the side that owns
//! the schemas, with a test that reads the real declarations, is the version that cannot
//! drift — and where it is wrong it is wrong in one place that both heads share, which is
//! what *"the walk must not differ between you"* requires.
//!
//! # Why a tool gets no bare form at all
//!
//! A tool whose parameters require **two or more** fields has no single thing a bare line
//! could mean, and one with **no required field** has nothing to put a line into. Both take
//! JSON, and [`BareForm::None`] carries the sentence a head says instead of guessing.

use serde_json::Value;

use crate::schema::ToolSchema;

/// The kind of value a bare line is, which is what a head completes on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A filesystem path: complete filenames, from the root or from the project.
    Path,
    /// An address: complete nothing, but say what is expected.
    Url,
    /// Words. Complete nothing.
    Text,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Path => "path",
            Kind::Url => "url",
            Kind::Text => "text",
        }
    }

    /// The kind of a field, from its declared `format` first and its name second.
    pub fn of(field: &str, schema: &Value) -> Kind {
        if let Some(f) = schema.get("format").and_then(Value::as_str) {
            match f {
                "uri" | "url" | "iri" | "uri-reference" => return Kind::Url,
                "path" | "file" | "filename" => return Kind::Path,
                _ => {}
            }
        }
        // The name, lowercased and stripped of punctuation: `file_path`, `URL`, `host`.
        let n: String = field
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        match n.as_str() {
            "path" | "file" | "filepath" | "filename" | "dir" | "directory" | "folder"
            | "source" | "target" => Kind::Path,
            "url" | "uri" | "link" | "href" | "host" | "hostname" | "endpoint" | "address"
            | "site" => Kind::Url,
            _ => Kind::Text,
        }
    }
}

/// **What a bare line means for one tool**, or why it means nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BareForm {
    /// A line goes into this field, of this kind, with these defaults alongside.
    One {
        field: String,
        kind: Kind,
        defaults: Vec<(String, String)>,
    },
    /// No bare form, and this is the sentence to say.
    None(String),
}

/// Derive the bare form of one schema.
///
/// `required` is read from the tool's own `parameters`, and **exactly one** required field
/// is the condition: two required fields means the operator would have to supply two things
/// and a bare line carries one, and none means there is nothing for the line to be.
pub fn bare_form(schema: &ToolSchema) -> BareForm {
    let params = &schema.parameters;
    let required: Vec<&str> = params
        .get("required")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    match required.as_slice() {
        [field] => {
            let declared = params.get("properties").and_then(|p| p.get(*field));
            let kind = Kind::of(field, declared.unwrap_or(&Value::Null));
            // **Every other property with a default travels with the line**, so the object
            // the daemon runs is the one a model's minimal call would have produced. Without
            // this the same tool answers two different questions depending on who asked:
            // `web_fetch {"url": …}` from a model reads markdown, and a bare line that
            // dropped `format` would read markdown too but a *different* tool's default
            // would not survive.
            // **Sorted**, because the wire type is a `BTreeMap` and a `Vec` that came back
            // in schema order would be a second order for one list.
            let mut defaults: Vec<(String, String)> = Vec::new();
            if let Some(props) = params.get("properties").and_then(Value::as_object) {
                for (name, spec) in props {
                    if name == *field {
                        continue;
                    }
                    if let Some(d) = spec.get("default") {
                        // A default that is not a string is rendered as its JSON, which is
                        // what a caller building an object wants; a string is unquoted.
                        defaults.push((
                            name.clone(),
                            match d {
                                Value::String(s) => s.clone(),
                                other => other.to_string(),
                            },
                        ));
                    }
                }
            }
            defaults.sort();
            BareForm::One {
                field: (*field).to_string(),
                kind,
                defaults,
            }
        }
        [] => BareForm::None(format!(
            "`{}` takes its arguments as JSON: it declares no required field, so a bare line \
             has nothing to put in one. `/run {} {{\"…\": …}}`",
            schema.name, schema.name
        )),
        many => BareForm::None(format!(
            "`{}` takes its arguments as JSON: it needs {} fields ({}), and a bare line carries \
             one. `/run {} {{\"…\": …}}`",
            schema.name,
            many.len(),
            many.join(", "),
            schema.name
        )),
    }
}

/// The bare forms of a list of names, for a daemon publishing its door.
///
/// `names` comes from the daemon's own allowlist and `schemas` from the registry it seats, so
/// a name in the door with no tool behind it is a **miss that is visible** rather than a
/// silent absence — it comes back with the sentence a head says instead of the bare form.
pub fn bare_forms<'a>(
    names: &[&str],
    schemas: impl IntoIterator<Item = &'a ToolSchema>,
) -> Vec<(String, BareForm)> {
    let schemas: Vec<&ToolSchema> = schemas.into_iter().collect();
    names
        .iter()
        .map(|name| {
            let found = schemas.iter().find(|s| s.name == *name);
            let form = match found {
                Some(s) => bare_form(s),
                None => BareForm::None(format!(
                    "`{name}` is in this daemon's door list and is not a tool this session \
                     seats. Nothing will run it; `/tools` shows what is seated."
                )),
            };
            ((*name).to_string(), form)
        })
        .collect()
}

#[cfg(test)]
mod the_derivation_is_read_off_the_real_schemas {
    use super::*;
    use crate::builtins;
    use crate::runtime::Registry;

    /// **The three door tools, from the registry this box actually seats.**
    ///
    /// Not hand-written schemas: the whole point of deriving the bare form is that a
    /// schema change moves a head's behaviour, so a test built on a fixture would prove
    /// nothing about the tools the operator will type.
    fn seat() -> Registry {
        use crate::builtins::external::web::{
            UnavailableFetcher, UnavailableSearch, WebFetch, WebSearch,
        };
        use std::sync::Arc;
        let mut reg = Registry::new();
        reg.register(Box::new(builtins::read::Read)).expect("read");
        reg.register(Box::new(WebSearch::new(Arc::new(UnavailableSearch))))
            .expect("web_search");
        reg.register(Box::new(WebFetch::new(Arc::new(UnavailableFetcher))))
            .expect("web_fetch");
        reg
    }

    /// **Each door tool has a bare form, and it is the field the operator would have
    /// typed by hand.** `web_search blabla` → `query`, `web_fetch ADDRESS` → `url`,
    /// `read crates/tui/src/app.rs` → `path`.
    #[test]
    fn every_door_tool_has_a_bare_field_and_it_is_the_obvious_one() {
        let reg = seat();
        let schemas = reg.schemas();
        let got = bare_forms(&["web_search", "web_fetch", "read"], schemas.iter());
        for (name, want) in [
            ("web_search", ("query", Kind::Text)),
            ("web_fetch", ("url", Kind::Url)),
            ("read", ("path", Kind::Path)),
        ] {
            let (_, form) = got
                .iter()
                .find(|(n, _)| n == name)
                .unwrap_or_else(|| panic!("`{name}` is in the door and not in the derivation"));
            match form {
                BareForm::One {
                    field,
                    kind,
                    defaults,
                } => {
                    assert_eq!(field, want.0, "`{name}`'s bare field");
                    assert_eq!(*kind, want.1, "`{name}`'s kind");
                    // **Nothing else required, so nothing to default** — asserted rather
                    // than assumed, because a default appearing here would silently change
                    // what a bare line sends.
                    assert!(
                        defaults.is_empty(),
                        "`{name}` gained defaults: {defaults:?}"
                    );
                }
                BareForm::None(why) => panic!("`{name}` has no bare form: {why}"),
            }
        }
    }

    /// **The kind is what drives R32's Tab, and it comes from the field's name** — the
    /// schemas declare `{"type": "string"}` and nothing more, which is why `type` could
    /// not answer this. A tool that declares `format` is believed over its own name.
    #[test]
    fn the_kind_comes_from_the_format_then_the_name() {
        use serde_json::json;
        // A declared format wins.
        assert_eq!(
            Kind::of("thing", &json!({"type": "string", "format": "uri"})),
            Kind::Url
        );
        assert_eq!(
            Kind::of("thing", &json!({"type": "string", "format": "path"})),
            Kind::Path
        );
        // Otherwise the name, whatever punctuation or case it carries.
        assert_eq!(Kind::of("path", &json!({"type": "string"})), Kind::Path);
        assert_eq!(Kind::of("file_path", &json!({})), Kind::Path);
        assert_eq!(Kind::of("URL", &json!({})), Kind::Url);
        assert_eq!(Kind::of("host", &json!({})), Kind::Url);
        // And words are words.
        assert_eq!(Kind::of("query", &json!({})), Kind::Text);
        assert_eq!(Kind::of("question", &json!({})), Kind::Text);
        // An unknown format does not make it text by accident — the name still decides.
        assert_eq!(
            Kind::of("path", &json!({"type": "string", "format": "date"})),
            Kind::Path
        );
    }

    /// **A tool that needs two fields gets no bare form, and says why in the words a head
    /// will use.** Guessing here would be the head inventing a shape the daemon never
    /// published — which is the failure R31 exists to stop, one layer along.
    #[test]
    fn two_required_fields_means_json_only_with_a_sentence() {
        let two = ToolSchema::new(
            "pair",
            "two things at once",
            serde_json::json!({
                "type": "object",
                "properties": {"a": {"type": "string"}, "b": {"type": "string"}},
                "required": ["a", "b"]
            }),
            crate::schema::Access::Read,
        );
        let BareForm::None(why) = bare_form(&two) else {
            panic!("two required fields cannot have one bare form");
        };
        assert!(why.contains("needs 2 fields"), "{why}");
        assert!(
            why.contains("/run pair"),
            "it names the form that works: {why}"
        );

        let none = ToolSchema::new(
            "nothing",
            "no arguments at all",
            serde_json::json!({"type": "object", "properties": {}}),
            crate::schema::Access::Read,
        );
        let BareForm::None(why) = bare_form(&none) else {
            panic!("a tool with no required field has nothing to put a line in");
        };
        assert!(why.contains("no required field"), "{why}");
    }

    /// **Defaults travel with the line**, so the object the door runs is the one a model's
    /// minimal call would have produced. Without this the same tool answers two different
    /// questions depending on who asked.
    #[test]
    fn a_default_on_another_field_travels_with_the_bare_line() {
        let s = ToolSchema::new(
            "fetchy",
            "fetch a thing",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "url": {"type": "string"},
                    "format": {"type": "string", "default": "markdown"},
                    "depth": {"type": "integer", "default": 2}
                },
                "required": ["url"]
            }),
            crate::schema::Access::Network,
        );
        let BareForm::One {
            field,
            kind,
            defaults,
        } = bare_form(&s)
        else {
            panic!("one required field is a bare form");
        };
        assert_eq!(field, "url");
        assert_eq!(kind, Kind::Url);
        assert_eq!(
            defaults,
            vec![
                ("depth".to_string(), "2".to_string()),
                ("format".to_string(), "markdown".to_string())
            ],
            "the default's JSON, and the string unquoted"
        );
    }

    /// **A name in the door with no tool behind it is a visible miss.** The allowlist and
    /// the registry are two lists, and a name in the first and not the second would
    /// otherwise be a door that silently does nothing.
    #[test]
    fn a_door_name_with_no_seated_tool_says_so() {
        let reg = seat();
        let schemas = reg.schemas();
        let got = bare_forms(&["web_search", "not_a_tool"], schemas.iter());
        let (_, form) = got.iter().find(|(n, _)| n == "not_a_tool").unwrap();
        let BareForm::None(why) = form else {
            panic!("a tool that is not seated cannot have a bare form");
        };
        assert!(why.contains("not a tool this session seats"), "{why}");
    }
}
