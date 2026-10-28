//! **`/todo title:"…" body:"…"` — the operator's own way to add a row**, as the built-in `todo`
//! layer (`docs/plugins-plan.md` §4.7: a plugin answers a text hook; the host performs what it
//! asks).
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
//! **`pass` when the line is not an add.** `done 2`, `rm 3`, `postpone 1`, `resume 1` and
//! `when 1 j7` are the head's own verbs, and an empty title is not a row: `None` returns `-1`, the
//! layer passes and the head answers the line as it always did. (The head does not even ask about
//! its own verbs — but a layer is asked about everything else, and answering *nothing here is mine*
//! is how a line the guest cannot read reaches the head instead of vanishing.)
//!
//! **Body joins the title with an em dash** (`title — body`), because a session's todo row is one
//! string on the wire (`TodoEntry { content, status, by }`) and the pane draws it as one row. A
//! body that needs its own line is a field on the row and a schema bump, which is the operator's
//! call and not this layer's to invent.
//!
//! # The input: the line, and what the head already holds
//!
//! ```jsonc
//! // command_todo: in
//! {"line": "title:\"a\" body:\"b\"",
//!  "feeds": {
//!    "todos":      [{"content": "…", "status": "pending", "by": "operator"}],
//!    "session":    {"id": "s-1793…", "workspace": "/home/dead/Projects/x"},
//!    "repo_todos": [{"text": "T1 git init", "mark": "[x]", "indent": 4, "item": true}]}}
//! ```
//!
//! `line` is the whole rest of the line after `/todo`, verbatim and untrimmed by any rule of this
//! crate's — the field names in it are this layer's business and nobody else's, which is what lets
//! a project's own `todo` layer read `title:` differently without the head knowing.
//!
//! **`feeds` is what the head is already showing** — not a capability and not a query, so nothing
//! here is adjudicated and nothing is granted by reading it. Three feeds, and the shapes are
//! `docs/plugins-plan.md` §4.9's Feeds section:
//!
//! | feed | is |
//! |---|---|
//! | `todos` | the whole board, both halves, in board order: `content`, `status` (`pending`, `in_progress`, `completed`, `postponed`, `cancelled`), `by` (`model`, `operator`, `queue`, `Parent s-…`) |
//! | `session` | where the head is standing: `id` and `workspace` |
//! | `repo_todos` | the workspace's `TODO.md` as the todos pane parsed it — **absent, not empty, when the head has not read the file** |
//!
//! **This layer reads `line` and nothing else, on purpose.** A feed is handed so a layer CAN decide
//! rather than only parse — *is this already on the board*, *does this project use sections* — and
//! inventing a behaviour for the first layer to receive one would be a demo, not a policy. The day
//! a rule needs the board, the board is here and no format changes.
//!
//! # Why `actions = ["add_todo"]` and `capabilities = []` are the trust boundary
//!
//! They are the two halves of one sentence, and either one alone would be a hole:
//!
//! * **`capabilities = []` is what the layer cannot DO.** It has no `read`, no `exec:<program>`, no
//!   `http:<host>`, so its imports reach nothing: the words it answers with are all it can produce.
//!   A `todo` layer that wanted to append to `TODO.md` itself would have to declare `read` and
//!   `exec:` and be consented to for them — which is a different conversation from *let `/todo`
//!   accept a title and a body*.
//! * **`actions = ["add_todo"]` is what the HOST will do for it.** A layer can answer any JSON it
//!   likes; the head performs exactly one act name and ignores every other, so the manifest's list
//!   is the whole of what an answer can cause. That is also why the list is a manifest field rather
//!   than a constant in the head: it is what a reviewer reads before a session layer is consented
//!   to, and it is what the next verb — `move_todo`, `due_todo` — will have to be added to.
//!
//! Put together: **the guest may say anything and cause exactly one thing, and that one thing is a
//! row on the operator's own half of the board.** The write is the head's, through
//! `Action::SetOperatorTodos` — the same door `/todo buy milk` has used since the operator's rows
//! became theirs.

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
    Some(serde_json::json!({"act": {"name": ACT, "args": {"content": content}}}).to_string())
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

    /// **The feeds are the head's, and this layer reads none of them** — the input arrives whole
    /// and the answer is exactly what the line alone would produce.
    ///
    /// A layer that read a feed would be a layer whose answer changes when the BOARD changes, and
    /// that is a policy nobody has written yet: this test is the statement that the shape is handed
    /// over and nothing more. It is also the only place the three feed names are spelled against a
    /// real input, so a head that renamed one would be a diff here.
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
}
