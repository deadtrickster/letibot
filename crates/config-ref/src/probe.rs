//! Drift checks: each parser's crate feeds its real parser the documents built here, so a key
//! the parser reads and the table does not list (or the reverse) fails a test.

use std::cell::Cell;
use std::collections::BTreeSet;

use crate::{File, Table};

/// The field names a `#[derive(Deserialize)]` struct asks for, read off the derive itself.
/// `None` for a type that is not a plain struct (flattened, untagged, a map).
pub fn serde_fields<'de, T: serde::Deserialize<'de>>() -> Option<&'static [&'static str]> {
    let seen = Cell::new(None);
    let _ = T::deserialize(Recorder(&seen));
    seen.get()
}

struct Recorder<'a>(&'a Cell<Option<&'static [&'static str]>>);

#[derive(Debug)]
struct Stop;

impl std::fmt::Display for Stop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("field probe")
    }
}

impl std::error::Error for Stop {}

impl serde::de::Error for Stop {
    fn custom<T: std::fmt::Display>(_: T) -> Self {
        Stop
    }
}

impl<'de> serde::Deserializer<'de> for Recorder<'_> {
    type Error = Stop;

    fn deserialize_any<V: serde::de::Visitor<'de>>(self, _: V) -> Result<V::Value, Stop> {
        Err(Stop)
    }

    fn deserialize_struct<V: serde::de::Visitor<'de>>(
        self,
        _: &'static str,
        fields: &'static [&'static str],
        _: V,
    ) -> Result<V::Value, Stop> {
        self.0.set(Some(fields));
        Err(Stop)
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string bytes byte_buf
        option unit unit_struct newtype_struct seq tuple tuple_struct map enum identifier
        ignored_any
    }
}

/// Every string literal in `src` shaped like a key: the candidates a hand-rolled walk could
/// match on. A parser's source is the superset; probing tells which ones it accepts.
pub fn literals(src: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let b = src.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'"' {
            i += 1;
            continue;
        }
        let start = i + 1;
        let mut j = start;
        while j < b.len() && b[j] != b'"' && b[j] != b'\n' {
            if b[j] == b'\\' {
                j += 1;
            }
            j += 1;
        }
        if j < b.len() && b[j] == b'"' {
            let s = &src[start..j];
            if is_keyish(s) {
                out.insert(s.to_string());
            }
            i = j + 1;
        } else {
            i = j;
        }
    }
    out
}

fn is_keyish(s: &str) -> bool {
    (2..=48).contains(&s.len())
        && s.starts_with(|c: char| c.is_ascii_lowercase() || c == '_' || c == '$')
        && s.chars().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-' | '.' | '$')
        })
}

/// Whether `key` is one the pattern names: `<x>` matches any run of characters.
pub fn matches(pattern: &str, key: &str) -> bool {
    fn go(p: &str, k: &str) -> bool {
        match p.find('<') {
            None => p == k,
            Some(open) => {
                let Some(close) = p[open..].find('>').map(|c| open + c) else {
                    return p == k;
                };
                let (lit, rest) = (&p[..open], &p[close + 1..]);
                let Some(tail) = k.strip_prefix(lit) else {
                    return false;
                };
                (0..=tail.len())
                    .filter(|n| tail.is_char_boundary(*n))
                    .any(|n| go(rest, &tail[n..]))
            }
        }
    }
    go(pattern, key)
}

/// `<x>` replaced by `probe`, the operator-chosen name every probe uses.
pub fn concrete(pattern: &str) -> String {
    let mut out = String::new();
    let mut rest = pattern;
    while let Some(open) = rest.find('<') {
        let Some(close) = rest[open..].find('>').map(|c| open + c) else {
            break;
        };
        out.push_str(&rest[..open]);
        out.push_str("probe");
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    out
}

fn toml_key(key: &str) -> String {
    let k = concrete(key);
    if k.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        k
    } else {
        format!("\"{k}\"")
    }
}

/// `[a.b]`, or `[[a.b]]` for a path ending in `[]` (an array of tables).
fn toml_header(path: &str) -> String {
    let (path, array) = match path.strip_suffix("[]") {
        Some(p) => (p, true),
        None => (path, false),
    };
    let parts: Vec<String> = path.split('.').map(toml_key).collect();
    if array {
        format!("[[{}]]", parts.join("."))
    } else {
        format!("[{}]", parts.join("."))
    }
}

/// A TOML document setting `key = value` in table `t`, plus the table's required lines.
pub fn toml_doc(t: &Table, key: &str, value: &str) -> String {
    let line = format!("{} = {value}\n", toml_key(key));
    if t.path.is_empty() {
        format!("{}{line}", t.needs)
    } else {
        format!("{}\n{}{line}", toml_header(t.path), t.needs)
    }
}

/// The values a reverse probe tries: a parser that dispatches on the name before the value
/// accepts a known key under at least one of them. No inline table: sub-tables are listed as
/// their own tables, and a reader that takes any table as `[x.<name>]` would accept them all.
pub const PROBE_VALUES: &[&str] = &["\"x\"", "1", "true", "[]", "1.5"];

/// Listed keys whose example `ok` refuses, as `table.key: why`.
pub fn refused_examples(
    f: &File,
    tables: &[&str],
    ok: impl Fn(&Table, &str) -> Result<(), String>,
) -> Vec<String> {
    let mut out = Vec::new();
    for t in f.tables.iter().filter(|t| tables.contains(&t.path)) {
        for k in t.keys {
            if let Err(why) = ok(t, &toml_doc(t, k.key, k.example)) {
                out.push(format!("[{}] {}: {why}", t.path, k.key));
            }
        }
    }
    out
}

/// Candidates the parser accepts in table `t` that the table does not list. `known(doc)` is
/// true unless the parser refused the probed key as unknown. A candidate naming one of `t`'s
/// listed sub-tables is not a key.
pub fn unlisted_accepted<'c>(
    f: &File,
    t: &Table,
    candidates: impl IntoIterator<Item = &'c String>,
    known: impl Fn(&str) -> bool,
) -> Vec<String> {
    let mut out = Vec::new();
    for c in candidates {
        if t.keys.iter().any(|k| matches(k.key, c)) {
            continue;
        }
        let sub = format!("{}.{c}", concrete(t.path));
        if f.tables.iter().filter(|s| s.path != t.path).any(|s| {
            let p = s.path.trim_end_matches("[]");
            matches(p, &sub) || concrete(p).starts_with(&format!("{sub}."))
        }) {
            continue;
        }
        if PROBE_VALUES.iter().any(|v| known(&toml_doc(t, c, v))) {
            out.push(format!("[{}] {c}", t.path));
        }
    }
    out
}

/// Whether a parser's error refuses a key by name rather than its value.
pub fn refuses_name(err: &str) -> bool {
    [
        "is not a key",
        "unknown field",
        "does not know",
        "has no key",
        "does not get to decide",
        "not a key this reads",
    ]
    .iter()
    .any(|p| err.contains(p))
}

/// The keys `t` lists, for an exact comparison with a parser's own list.
pub fn keys_of(t: &Table) -> BTreeSet<&'static str> {
    t.keys.iter().map(|k| k.key).collect()
}
