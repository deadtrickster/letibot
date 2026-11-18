//! Reading a config file back to a session, which is exactly the file minus its credentials.
//! A masked directory is what stops a model editing its own perimeter; this is the door that
//! shows it the file without handing it a key.

use crate::{Format, lookup};

/// What `redact` answered: the bytes to show, and what was done to them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redacted {
    pub text: String,
    /// How many values were replaced.
    pub count: usize,
    /// One sentence saying what this file holds, for a reader deciding whether to trust it.
    pub note: String,
}

/// `text` with every credential value replaced.
///
/// A TOML file is walked by key, so an array of tokens or an inline table of headers is caught;
/// a file whose keys are not values at all (`permission.json`, `modes.tsv`) is returned as it is,
/// with a note saying that is what happened and why it is safe.
pub fn redact(text: &str, format: Format) -> Result<Redacted, String> {
    match format {
        Format::Toml => redact_toml(text),
        Format::Json => Ok(Redacted {
            text: text.to_string(),
            count: 0,
            note: "shown as written: this file's keys name tools, rules and paths, and the \
                   reference lists no credential among them"
                .into(),
        }),
        Format::Tsv => Ok(Redacted {
            text: text.to_string(),
            count: 0,
            note: "shown as written: this file has no values to be a credential".into(),
        }),
    }
}

const MARK: &str = "\"<redacted>\"";

fn redact_toml(text: &str) -> Result<Redacted, String> {
    // A file that does not parse cannot be redacted by key, and a line pass over it would be a
    // guess about which half of `key = "…" # …` is the value. So it is refused, with the reader's
    // own words so the operator can fix it.
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| format!("{e}"))?;
    let mut count = 0;
    walk(doc.as_table_mut(), false, &mut count);
    Ok(Redacted {
        text: doc.to_string(),
        count,
        note: if count == 0 {
            "no credential value in this file".into()
        } else {
            format!(
                "{count} credential value{} replaced with {MARK}",
                if count == 1 { "" } else { "s" }
            )
        },
    })
}

/// `all` while the walk is inside a table whose values are credentials whatever they are called:
/// an MCP server's `headers`, a boundary's or a perimeter's `env`, where the name IS the point.
fn walk(table: &mut toml_edit::Table, all: bool, count: &mut usize) {
    for (key, item) in table.iter_mut() {
        match item {
            toml_edit::Item::Value(v) => {
                redact_value(v, all || lookup::is_credential(key.get()), count)
            }
            toml_edit::Item::Table(t) => walk(t, all || named_by_credentials(key.get()), count),
            toml_edit::Item::ArrayOfTables(a) => {
                let all = all || named_by_credentials(key.get());
                for t in a.iter_mut() {
                    walk(t, all, count);
                }
            }
            toml_edit::Item::None => {}
        }
    }
}

/// A table whose every value is a credential, whatever the key is called.
fn named_by_credentials(key: &str) -> bool {
    matches!(key, "headers" | "env")
}

fn redact_value(v: &mut toml_edit::Value, all: bool, count: &mut usize) {
    if all {
        let old = v.decor().clone();
        let mut new: toml_edit::Value = MARK.parse().expect("a quoted string is a TOML value");
        *new.decor_mut() = old;
        *v = new;
        *count += 1;
        return;
    }
    if let toml_edit::Value::InlineTable(t) = v {
        for (k, val) in t.iter_mut() {
            let all = named_by_credentials(k.get()) || lookup::is_credential(k.get());
            redact_value(val, all, count);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A provider key and a bearer token are replaced; the comment beside them is not.
    #[test]
    fn the_values_go_and_the_comments_stay() {
        let text = "\
# mine
[deepseek]
key = \"sk-super-secret\"   # from the console

[mcp.search]
url = \"http://127.0.0.1:8931/mcp\"
headers = { Authorization = \"Bearer abc\" }
";
        let r = redact(text, Format::Toml).unwrap();
        assert!(!r.text.contains("sk-super-secret"), "{}", r.text);
        assert!(!r.text.contains("Bearer abc"), "{}", r.text);
        assert!(r.text.contains("# mine"), "{}", r.text);
        assert!(r.text.contains("# from the console"), "{}", r.text);
        assert_eq!(r.count, 2, "{}", r.text);
        assert!(r.note.contains("2 credential values"), "{}", r.note);
    }

    /// A perimeter's env is credentials under names of its own, and a token list is a list.
    #[test]
    fn an_env_table_and_a_list_are_redacted() {
        let text = "\
[perimeter.top-level.env]
GH_TOKEN = { command = \"gh auth token\" }

[perimeter.top-level]
secrets = [\"github\"]
";
        let r = redact(text, Format::Toml).unwrap();
        assert!(!r.text.contains("gh auth token"), "{}", r.text);
        // A secret's NAME is not a secret: which one the seat carries is what a reader asks.
        assert!(r.text.contains("[\"github\"]"), "{}", r.text);
    }

    /// A file that does not parse is refused, because a line pass would guess which half of a
    /// line is the value.
    #[test]
    fn a_broken_file_is_refused_rather_than_guessed_at() {
        let e = redact("key = \"sk-abc\"\noops\n", Format::Toml).unwrap_err();
        assert!(!e.is_empty());
    }
}
