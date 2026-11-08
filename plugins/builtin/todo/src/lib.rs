//! `/todo title:"…" body:"…"` - the operator's own way to add a row, as the built-in `todo` layer.
//!
//! The line is the guest's: `title:` and `body:` are parsed here, quoted or bare, in any order, and
//! a line with neither is the whole thing as a title. The act is the host's: this layer cannot
//! write a todo (its `capabilities` are empty), so it answers one declared act name and `plugin.toml`
//! is what a reviewer reads. A line that is not an add (`done 2`, `rm 3`) passes, so the head answers
//! it as it always did.
//! ```jsonc
//! // command_todo: in
//! {"line": "title:\"a\" body:\"b\"",
//!  "feeds": {
//!    "todos":      [{"content": "…", "status": "pending", "by": "operator"}],
//!    "session":    {"id": "s-1793…", "workspace": "/home/dead/Projects/x"},
//!    "repo_todos": [{"text": "T1 git init", "mark": "[x]", "indent": 4, "item": true}]}}
//! ```
//!
//! `nag_todo` is the other export: the plan-check's decision (`docs/plugin-extension-points-wide.md`
//! X13). This layer says exactly what the harness's own nag says - one row, the harness's words -
//! so the bottom of the stack is today's daemon, and an operator's layer above it is where R127's
//! every-free-head answer is written. It names the input's row back, which is the whole of what an
//! identity layer may do: the clock and the eligibility filter are not its to move.

/// The act name the host performs, and the one its `plugin.toml` declares.
const ACT: &str = "add_todo";

use letibot_guest_abi as abi;

#[unsafe(no_mangle)]
pub extern "C" fn command_todo(ptr: i32, len: i32) -> i32 {
    abi::hook(ptr, len, answer)
}

#[unsafe(no_mangle)]
pub extern "C" fn nag_todo(ptr: i32, len: i32) -> i32 {
    abi::hook(ptr, len, the_nag)
}

/// The plan-check answered as the harness asked it: the row it named and the sentence it wrote.
/// `None` - unchanged - when the input is not a nag, which is the fail-down rule.
fn the_nag(bytes: &[u8]) -> Option<String> {
    let input: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let text = input.get("text")?.as_str()?;
    let named = input.get("name").and_then(|n| n.as_str());
    Some(match named {
        Some(row) => serde_json::json!({"name": [row], "text": text}).to_string(),
        None => serde_json::json!({"text": text}).to_string(),
    })
}

fn answer(bytes: &[u8]) -> Option<String> {
    let input: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    // The head hands the line it was given, after the verb: `/todo title:"a" body:"b"` arrives as
    // `a" body:"b` only if the head has already eaten `title:` — so the whole rest of the line is
    // what this reads, and the field names are its own business.
    let line = input.get("line")?.as_str()?;
    let content = parse(line)?;
    Some(serde_json::json!({"act": {"name": ACT, "args": {"content": content}}}).to_string())
}

/// The row's words, out of the line: `Some(content)` when the line adds a row, `None` when it is
/// one of the head's verbs. The scan is quote-aware rather than a split on whitespace because a
/// quoted title may hold a colon and the other field's name; text with no field name is the title.
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

    /// The feeds are the head's and this layer reads none of them: the answer is exactly what the
    /// line alone would produce. This is also the only place the three feed names are spelled
    /// against a real input, so a head that renamed one would be a diff here.
    #[test]
    fn the_feeds_arrive_and_change_nothing() {
        let input = serde_json::json!({
            "line": "title:\"a\" body:\"b\"",
            "feeds": {
                "todos": [{"content": "already on the board", "status": "pending", "by": "operator"}],
                "session": {"id": "s-1", "workspace": "/home/dead/Projects/x"},
                "repo_todos": [{"text": "T1 git init", "mark": "[x]", "indent": 4, "item": true}],
            },
        });
        let out = answer(input.to_string().as_bytes()).expect("an act");
        let v: serde_json::Value = serde_json::from_str(&out).expect("json");
        assert_eq!(v["act"]["args"]["content"], "a — b");
    }

    /// **`nag_todo` says the harness's own sentence about the harness's own row.** The layer is
    /// the identity of today's daemon, so the answer is the input's text and name, and nothing
    /// else - a row it was not handed is a row it cannot name.
    #[test]
    fn the_nag_answers_the_sentence_it_was_handed() {
        let input = serde_json::json!({
            "v": 1,
            "board": [{"id": "r3", "content": "r3", "status": "pending", "by": "operator"}],
            "eligible": ["r3"],
            "clock": {"repeats": 0, "idle_ms": 0, "floor_ms": 60000, "cap_ms": 1800000},
            "now_ms": 60000,
            "text": "[todo check] Not done:\n  - r3 — the operator's\n",
            "name": "r3",
        });
        let out = the_nag(input.to_string().as_bytes()).expect("an answer");
        let v: serde_json::Value = serde_json::from_str(&out).expect("json");
        assert_eq!(v["text"], input["text"]);
        assert_eq!(v["name"], serde_json::json!(["r3"]));
        // The degenerate case - a plan whose ready set is empty names no row - answers the
        // sentence and no name, rather than an empty one.
        let mut blocked = input.clone();
        blocked["name"] = serde_json::Value::Null;
        let out = the_nag(blocked.to_string().as_bytes()).expect("an answer");
        let v: serde_json::Value = serde_json::from_str(&out).expect("json");
        assert!(v.get("name").is_none(), "{v}");
        // And a document that is not a nag passes, so the layer below speaks.
        assert_eq!(the_nag(br#"{"line":"x"}"#), None);
    }
}
