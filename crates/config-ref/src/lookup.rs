//! Answering "what key is this?" from the reference: where a dotted path lives, what it does, and
//! whether its value is a credential. One copy of the rule, because the tool, the desk and the
//! redaction all ask it.

use crate::{File, Key, Table, files::ALL, probe};

/// One key the reference has, with the file and table it sits in.
#[derive(Debug, Clone, Copy)]
pub struct Hit {
    pub file: &'static File,
    pub table: &'static Table,
    pub key: &'static Key,
}

/// The descriptive half of an edit's path, when it names one: the table's path and the key.
/// `None` for a key the reference does not have, which a caller refuses by name.
pub fn describe(f: &File, path: &str) -> Option<(&'static Table, &'static Key)> {
    let parts: Vec<&str> = path.split('.').collect();
    for split in 0..=parts.len() {
        let table_path = parts[..split].join(".");
        let leaf = parts[split..].join(".");
        if leaf.is_empty() {
            continue;
        }
        for t in f.tables {
            let names_table = if t.path.is_empty() {
                split == 0
            } else {
                probe::matches(t.path.trim_end_matches("[]"), &table_path)
            };
            if !names_table {
                continue;
            }
            if let Some(k) = t.keys.iter().find(|k| probe::matches(k.key, &leaf)) {
                return Some((t, k));
            }
        }
    }
    None
}

/// Every file that has this key path, in the reference's own order.
///
/// A key the reference lists as `<tool>` or `<name>` accepts any name at all, so asking what
/// `answer_reads` does must not be answered with `permission.json`'s `<tool>`: a key the operator
/// names is found by [`search`], and a path landing on one has told this function nothing.
pub fn hits(path: &str) -> Vec<Hit> {
    let mut out = Vec::new();
    for f in ALL {
        if let Some((table, key)) = describe(f, path)
            && !key.key.starts_with('<')
        {
            out.push(Hit {
                file: f,
                table,
                key,
            });
        }
    }
    out
}

/// The key files whose tables match a word, so `explain {key: "answer_reads"}` finds
/// `shadow.answer_reads` from the leaf alone.
pub fn search(word: &str) -> Vec<Hit> {
    let mut out = Vec::new();
    let want = word.to_ascii_lowercase();
    for f in ALL {
        for t in f.tables {
            for key in t.keys {
                if key.key.eq_ignore_ascii_case(&want) || key.key.contains(&want) {
                    out.push(Hit {
                        file: f,
                        table: t,
                        key,
                    });
                }
            }
        }
    }
    out
}

/// Whether a value under this key name is a credential: an API key, a token, a password.
///
/// A key that NAMES one of those is not one - `key_env` is a variable's name and `token_path` a
/// path - which is why the match is the whole leaf or a `_`-suffixed one, never `contains`.
pub fn is_credential(leaf: &str) -> bool {
    const WORDS: &[&str] = &[
        "key",
        "apikey",
        "token",
        "secret",
        "password",
        "passwd",
        "authorization",
        "cookie",
        "bearer",
        "credential",
        "credentials",
    ];
    let leaf = leaf.to_ascii_lowercase();
    WORDS
        .iter()
        .any(|w| leaf == *w || leaf.ends_with(&format!("_{w}")) || leaf.ends_with(&format!("-{w}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The path the brief's own example uses resolves, and so does a leaf found by search.
    #[test]
    fn a_dotted_path_resolves_to_its_key() {
        let f = crate::find("providers.toml").unwrap();
        let (t, k) = describe(f, "shadow.answer_reads").unwrap();
        assert_eq!(t.path, "shadow");
        assert_eq!(k.key, "answer_reads");
        assert!(k.meaning.contains("permission job"), "{}", k.meaning);
        // A preset table is a name the operator chose, and a top-level key has no dot at all.
        assert!(describe(f, "deepseek.key").is_some());
        assert!(describe(crate::find("leticode.toml").unwrap(), "main_model").is_some());
        assert!(describe(f, "shadow.no_such_key").is_none());
        let hits = search("answer_reads");
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].file.name, "providers.toml");
    }

    /// The predicate separates a value from the NAME of one.
    #[test]
    fn a_credential_is_a_value_not_a_name() {
        for k in [
            "key",
            "api_key",
            "gh-token",
            "password",
            "Authorization",
            "secret",
        ] {
            assert!(is_credential(k), "{k}");
        }
        for k in [
            "key_env",
            "token_path",
            "secrets",
            "keys",
            "keywords",
            "monkey",
        ] {
            assert!(!is_credential(k), "{k}");
        }
    }
}
