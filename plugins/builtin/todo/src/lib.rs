//! **`/todo title:"…" body:"…"` — the operator's own way to add a row**, as the built-in `todo`
//! layer (`docs/plugins-plan.md`: a plugin answers a text hook; the host performs what it asks).
//!
//! The head owns the slash line, so the split is the one the plan draws:
//!
//! * **the line is the guest's**: `title:` and `body:` are parsed here, quoted or bare, in any
//!   order, and a line with neither is the whole thing as a title — the shape a person types;
//! * **the act is the host's**: this layer cannot write a todo (its `capabilities` are empty), so
//!   it answers `{"act": {"name": "add_todo", "args": {"content": …}}}` and `plugin.toml` declares
//!   `add_todo` in `actions`. The head understands exactly that one name, so the manifest and the
//!   host are two ends of the same sentence and a layer cannot ask for anything else.
//!
//! **`pass` when the line is not an add.** `done 2`, `rm 3`, `postpone 1` and `resume 1` are the
//! head's own verbs, and an empty title is not a row: `None` returns `-1`, the layer passes and the
//! head answers the line as it always did.
//!
//! **Body joins the title with an em dash** (`title — body`), because a session's todo row is one
//! string on the wire (`TodoEntry { content, status, by }`) and the pane draws it as one row. A
//! body that needs its own line is a field on the row and a schema bump, which is the operator's
//! call and not this layer's to invent.

/// The act name the host performs, and the one its `plugin.toml` declares.
const ACT: &str = "add_todo";

use letibot_guest_abi as abi;

#[unsafe(no_mangle)]
pub extern "C" fn command_todo(ptr: i32, len: i32) -> i32 {
    abi::hook(ptr, len, answer)
}

fn answer(bytes: &[u8]) -> Option<String> {
    let input: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    // The head hands the line it was given, after the verb: `/todo title:"a" body:"b"` arrives as
    // `a" body:"b` only if the head has already eaten `title:` — so the whole rest of the line is
    // what this reads, and the field names are its own business.
    let line = input.get("line")?.as_str()?;
    let content = parse(line)?;
    Some(
        serde_json::json!({"act": {"name": ACT, "args": {"content": content}}})
            .to_string(),
    )
}

/// **The row's words, out of the line** — `Some(content)` when the line adds a row, `None` when it
/// is one of the head's verbs or names nothing.
///
/// `title:"…"` and `body:"…"` are read in either order and either may be bare
/// (`title:werwer`), because a person types both; quotes may contain the other field's name and a
/// colon, which is why the scan is quote-aware rather than a split on whitespace. Text with no
/// field name at all is the title, which is what `/todo buy milk` has always meant.
pub fn parse(line: &str) -> Option<String> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let mut title: Option<String> = None;
    let mut body: Option<String> = None;
    let mut bare: Vec<&str> = Vec::new();
    let mut rest = line;
    while !rest.is_empty() {
        let (word, after) = next_token(rest);
        rest = after;
        let (name, value) = match word.split_once(':') {
            Some((name, value)) if is_field(name) => (name, value),
            _ => {
                bare.push(word);
                continue;
            }
        };
        let value = value.trim();
        let Some(value) = quoted_or_bare(value).map(str::to_string) else {
            continue;
        };
        match name {
            "title" => title = Some(value),
            "body" => body = Some(value),
            _ => unreachable!("is_field said so"),
        }
    }
    let title = match title {
        Some(t) if !t.trim().is_empty() => t.trim().to_string(),
        _ if !bare.is_empty() => bare.join(" "),
        _ => return None,
    };
    Some(match body.map(|b| b.trim().to_string()) {
        Some(b) if !b.is_empty() => format!("{title} — {b}"),
        _ => title,
    })
}

fn is_field(name: &str) -> bool {
    name == "title" || name == "body"
}

/// One whitespace-separated token, **quote-aware**: `title:"two words"` is one token, and the
/// quote is what makes the space inside it not a separator.
fn next_token(s: &str) -> (&str, &str) {
    let bytes = s.as_bytes();
    let mut at = 0usize;
    let mut quote: Option<u8> = None;
    while at < bytes.len() {
        let b = bytes[at];
        match quote {
            Some(q) if b == q => quote = None,
            Some(_) => {}
            None if b == b'"' || b == b'\'' => quote = Some(b),
            None if b.is_ascii_whitespace() => break,
            None => {}
        }
        at += 1;
    }
    (&s[..at], s[at..].trim_start())
}

/// `"quoted value"` or a bare `value`, without its quotes.
fn quoted_or_bare(v: &str) -> Option<&str> {
    let v = v.trim();
    if v.len() >= 2 {
        let first = v.as_bytes()[0];
        let last = v.as_bytes()[v.len() - 1];
        if (first == b'"' || first == b'\'') && first == last {
            return Some(&v[1..v.len() - 1]);
        }
    }
    if v.is_empty() { None } else { Some(v) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The line the operator types** — `title:"werwer" body:"blabla"` — and the shapes around
    /// it: the other order, a bare value, a title alone, and the head's own verbs left alone.
    #[test]
    fn a_title_and_a_body_become_one_row() {
        assert_eq!(
            parse(r#"title:"werwer" body:"blabla""#).as_deref(),
            Some("werwer — blabla")
        );
        assert_eq!(
            parse(r#"body:"blabla" title:"werwer""#).as_deref(),
            Some("werwer — blabla")
        );
        assert_eq!(
            parse(r#"title:"two words" body:"and three more""#).as_deref(),
            Some("two words — and three more")
        );
        assert_eq!(parse("title:werwer").as_deref(), Some("werwer"));
        assert_eq!(parse("buy milk").as_deref(), Some("buy milk"));
        assert_eq!(
            parse(r#"title:"a: b" body:"c, d""#).as_deref(),
            Some("a: b — c, d")
        );
    }

    /// **What is not an add** — an empty line, a title that is only quotes, and the verbs the head
    /// owns (`pass`, so the head answers them as it always did).
    #[test]
    fn a_line_that_is_not_an_add_passes() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("   "), None);
        assert_eq!(parse(r#"title:"   ""#), None);
        assert_eq!(parse("done 2"), Some("done 2".to_string()));
    }

    /// **The answer is the act the host performs**, named exactly as `plugin.toml` declares it.
    #[test]
    fn the_answer_names_the_act_and_its_words() {
        let out = answer(br#"{"line":"title:\"werwer\" body:\"blabla\""}"#).expect("an act");
        let v: serde_json::Value = serde_json::from_str(&out).expect("json");
        assert_eq!(v["act"]["name"], "add_todo");
        assert_eq!(v["act"]["args"]["content"], "werwer — blabla");
    }
}
