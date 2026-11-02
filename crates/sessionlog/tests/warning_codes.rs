//! Every warning code named in this tree has a severity, and this test keeps the table from
//! rotting: it reads the tree, finds each code named at its emission site and fails until it has a
//! row. A code reaching the wire through a variable is invisible, so only one direction is asserted.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// The shape every code in this tree has: lowercase words joined by underscores, with a dot
/// allowed because `guard.empty` has one.
fn looks_like_a_code(s: &str) -> bool {
    !s.is_empty()
        && s.starts_with(|c: char| c.is_ascii_lowercase())
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '.')
}

/// The index just past the string literal beginning at `i`. Unterminated cannot happen in
/// source that compiles, and the end of the file is the safe answer if it ever does.
fn past_string(b: &[u8], i: usize) -> usize {
    let quote = b[i];
    let mut j = i + 1;
    while j < b.len() {
        if b[j] == b'\\' {
            j += 2;
            continue;
        }
        if b[j] == quote {
            return j + 1;
        }
        j += 1;
    }
    b.len()
}

/// Skip a `//` comment or a `/* ... */` block, returning where the scanner resumes, or `None` when
/// this is not a comment. Bytes only: a multi-byte character in a doc comment must not be sliced.
fn past_comment(b: &[u8], i: usize) -> Option<usize> {
    if b[i] != b'/' {
        return None;
    }
    match b.get(i + 1) {
        Some(b'/') => Some(
            b[i..]
                .iter()
                .position(|&c| c == b'\n')
                .map(|k| i + k)
                .unwrap_or(b.len()),
        ),
        Some(b'*') => {
            let mut j = i + 2;
            while j + 1 < b.len() {
                if b[j] == b'*' && b[j + 1] == b'/' {
                    return Some(j + 2);
                }
                j += 1;
            }
            Some(b.len())
        }
        _ => None,
    }
}

/// The value of every `code:` key inside one `{ ... }` literal whose opening brace is at `open`.
/// A `code:` expression ends at the first comma at the depth the key was found, so an `if`/`else`
/// is one expression and both its strings are codes, while a `detail:` after that comma is not.
fn codes_in_literal(src: &str, open: usize) -> Vec<String> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut i = open;
    let mut depth: i32 = 0;
    let mut in_code: Option<i32> = None;
    while i < b.len() {
        if let Some(next) = past_comment(b, i) {
            i = next;
            continue;
        }
        match b[i] {
            b'"' | b'\'' => {
                let end = past_string(b, i);
                if b[i] == b'"'
                    && in_code.is_some()
                    && let Ok(text) = std::str::from_utf8(&b[i + 1..end - 1])
                    && looks_like_a_code(text)
                {
                    out.push(text.to_string());
                }
                i = end;
                continue;
            }
            b'{' | b'(' | b'[' => depth += 1,
            b'}' | b')' | b']' => {
                depth -= 1;
                if depth <= 0 {
                    return out;
                }
            }
            b',' => {
                if in_code == Some(depth) {
                    in_code = None;
                }
            }
            b':' => {
                // The four bytes before the colon spell `code`, and the one before them is a
                // struct literal's separator - not `.code:` and not `::code:`.
                let field = i >= 5
                    && &b[i - 4..i] == b"code"
                    && matches!(b[i - 5], b' ' | b'\t' | b'\n' | b'{' | b',' | b'(');
                if field && in_code.is_none() {
                    in_code = Some(depth);
                }
            }
            _ => {}
        }
        i += 1;
    }
    out
}

/// Every warning code in one file's source; the module doc names the two shapes.
fn codes_in(src: &str) -> Vec<String> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if let Some(next) = past_comment(b, i) {
            i = next;
            continue;
        }
        if b[i] == b'"' || b[i] == b'\'' {
            i = past_string(b, i);
            continue;
        }
        // An identifier boundary on the left: `Self::Warning` is a site and the tail of a longer
        // name is not. A dot is deliberately not excluded, because `self.import_note("...")` is one
        // of the two shapes this scan exists to find.
        let prev = if i == 0 { b' ' } else { b[i - 1] };
        if is_ident_byte(prev) {
            i += 1;
            continue;
        }
        let rest = &b[i..];
        for kw in [&b"Warning"[..], &b"Warned"[..], &b"import_note"[..]] {
            let Some(tail) = rest.strip_prefix(kw) else {
                continue;
            };
            if kw == b"import_note" {
                // `import_note("code", detail)` - the import's one writer, whose codes are
                // arguments and never an initialiser.
                let skip = |s: &[u8]| s.iter().position(|c| !matches!(c, b' ' | b'\t' | b'\n'));
                let Some(at) = skip(tail) else { continue };
                let Some(after) = tail[at..].strip_prefix(b"(") else {
                    continue;
                };
                let Some(at) = skip(after) else { continue };
                let Some(quoted) = after[at..].strip_prefix(b"\"") else {
                    continue;
                };
                let Some(end) = quoted.iter().position(|&c| c == b'"') else {
                    continue;
                };
                if let Ok(code) = std::str::from_utf8(&quoted[..end])
                    && looks_like_a_code(code)
                {
                    out.push(code.to_string());
                }
                break;
            }
            // `Warning { ... }` / `Warned { ... }`
            let skip = |s: &[u8]| s.iter().position(|c| !matches!(c, b' ' | b'\t' | b'\n'));
            let Some(at) = skip(tail) else { continue };
            if tail[at..].first() != Some(&b'{') {
                continue;
            }
            out.extend(codes_in_literal(src, i + kw.len() + at));
            break;
        }
        i += 1;
    }
    out
}

/// Every `.rs` file under `dir`, recursively.
fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|x| x == "rs") {
            out.push(path);
        }
    }
}

/// Every warning code named anywhere in this tree has a severity.
#[test]
fn every_warning_code_in_the_tree_is_classified() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let crates = root.join("crates");
    assert!(
        crates.is_dir(),
        "the workspace is not where this test thinks: {}",
        crates.display()
    );
    let mut files = Vec::new();
    sources(&crates, &mut files);
    assert!(files.len() > 100, "{} source files", files.len());

    // Code -> the files it was found in, for a failure that says where to look.
    let mut found: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for path in &files {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        for code in codes_in(&text) {
            let name = path
                .strip_prefix(&root)
                .unwrap_or(path)
                .display()
                .to_string();
            let where_ = found.entry(code).or_default();
            if !where_.contains(&name) {
                where_.push(name);
            }
        }
    }

    // The vacuity guards: a scan rots silently when a file moves, and a scan that found nothing
    // passes everything below.
    assert!(
        found.len() > 50,
        "the scan found {} codes, which is too few to be this tree — the scan is broken, \
         not the tree: {found:?}",
        found.len()
    );
    for must in [
        // A code the operator was met by.
        "compacted",
        // ...the other one, from a different crate and a different site.
        "daemon_stopping",
        // ...a `Warned { }` rather than a `Warning { }`, which is the head's own.
        "log_gap",
        // ...a `code:` whose value is an `if`/`else`.
        "slash_refused",
        // ...and the import's writer, which is the second shape.
        "import_failed",
    ] {
        assert!(
            found.contains_key(must),
            "the scan did not see `{must}`, so it is not reading the shape it must: \
             {:?}",
            found.keys().collect::<Vec<_>>()
        );
    }

    let unclassified: BTreeMap<&String, &Vec<String>> = found
        .iter()
        .filter(|(code, _)| {
            !letibot_sessionlog::warning::TABLE
                .iter()
                .any(|(known, _)| known == code)
        })
        .collect();
    assert!(
        unclassified.is_empty(),
        "these codes are named in this tree and have no severity in \
         `letibot_sessionlog::warning::TABLE` — R19's second fault is *routine painted \
         as failure*, and a code with no row is drawn as a failure until somebody \
         decides: {unclassified:#?}"
    );
}
