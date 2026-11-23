//! What an `edit` or `write` call would change, worked out before it runs, so the permission card
//! can show the diff it is asking about. Exact matching only, as the tool applies: a call that
//! would not resolve gets no preview, and the tool's own refusal says why when it runs.

use serde_json::Value;

use crate::edit::{FileEdit, FileText, ToolEditExcerpt, changed_span, occurrences};

/// Context and cap of the excerpt, the same the tool row's excerpt is taken with.
const CONTEXT: usize = 3;
const CAP: usize = 400;

/// The excerpt the call would produce, or `None` when it would not resolve or is not a file edit.
/// `read` returns the file's bytes, `Ok(None)` when it does not exist.
pub fn preview(
    tool: &str,
    args: &Value,
    read: impl Fn(&str) -> Result<Option<Vec<u8>>, String>,
) -> Option<ToolEditExcerpt> {
    let path = args.get("path")?.as_str()?;
    let fe = match tool {
        "write" => {
            let content = args.get("content")?.as_str()?;
            write_edit(path, content, read(path).ok()?)?
        }
        "edit" => {
            let file = FileText::of(&read(path).ok()??);
            if file.lossy {
                return None;
            }
            let after = edited(&file.lf, args)?;
            (after != file.lf).then(|| file_edit(path, file.lf, after, false))?
        }
        _ => return None,
    };
    Some(fe.excerpt(CONTEXT, CAP))
}

fn write_edit(path: &str, content: &str, bytes: Option<Vec<u8>>) -> Option<FileEdit> {
    let after = content.replace("\r\n", "\n");
    match bytes {
        None => Some(file_edit(path, String::new(), after, true)),
        Some(bytes) => {
            let file = FileText::of(&bytes);
            (!file.lossy && file.lf != after).then(|| file_edit(path, file.lf, after, false))
        }
    }
}

fn file_edit(path: &str, before: String, after: String, created: bool) -> FileEdit {
    FileEdit {
        path: path.to_string(),
        changed: changed_span(&before, &after),
        before_digest: String::new(),
        after_digest: String::new(),
        replacements: 1,
        before,
        after,
        created,
    }
}

/// The text after the call's steps, applied in order as `edit` applies them; `None` when a step
/// matches nothing, or more than one place without `replace_all`.
fn edited(text: &str, args: &Value) -> Option<String> {
    let steps: Vec<&Value> = match args.get("edits") {
        Some(list) => list.as_array()?.iter().collect(),
        None => vec![args],
    };
    if steps.is_empty() {
        return None;
    }
    let mut text = text.to_string();
    for s in steps {
        let new = s.get("new_string")?.as_str()?;
        let all = s
            .get("replace_all")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let str_of = |k: &str| s.get(k).and_then(Value::as_str).filter(|v| !v.is_empty());
        // `Some(true)` inserts before the anchor, `Some(false)` after it, `None` replaces it.
        let (needle, insert) = match (
            str_of("old_string"),
            str_of("insert_before"),
            str_of("insert_after"),
        ) {
            (Some(o), None, None) => (o, None),
            (None, Some(a), None) => (a, Some(true)),
            (None, None, Some(a)) => (a, Some(false)),
            _ => return None,
        };
        let hits = occurrences(&text, needle);
        if hits.is_empty() || (hits.len() > 1 && !all) {
            return None;
        }
        let mut out = String::with_capacity(text.len() + new.len());
        let mut from = 0;
        for at in hits {
            // `get`, not an index: a byte offset that is not a char boundary answers `None` and
            // this call previews nothing rather than panicking over a file it cannot read.
            out.push_str(text.get(from..at)?);
            match insert {
                None => out.push_str(new),
                Some(true) => out.push_str(&format!("{new}{needle}")),
                Some(false) => out.push_str(&format!("{needle}{new}")),
            }
            from = at + needle.len();
        }
        out.push_str(text.get(from..)?);
        text = out;
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn file(body: &'static str) -> impl Fn(&str) -> Result<Option<Vec<u8>>, String> {
        move |_| Ok(Some(body.as_bytes().to_vec()))
    }

    #[test]
    fn an_edit_previews_the_lines_it_replaces() {
        let ex = preview(
            "edit",
            &json!({"path": "a.rs", "old_string": "two", "new_string": "TWO"}),
            file("one\ntwo\nthree\n"),
        )
        .unwrap();
        assert_eq!(ex.before, "one\ntwo\nthree");
        assert_eq!(ex.after, "one\nTWO\nthree");
        assert!(!ex.created);
    }

    #[test]
    fn a_batch_applies_in_order_and_an_ambiguous_step_previews_nothing() {
        let args = json!({"path": "a", "edits": [
            {"old_string": "a", "new_string": "b"},
            {"insert_after": "b", "new_string": "!"},
        ]});
        assert_eq!(preview("edit", &args, file("a\n")).unwrap().after, "b!");
        let twice = json!({"path": "a", "old_string": "x", "new_string": "y"});
        assert!(preview("edit", &twice, file("x x\n")).is_none());
    }

    #[test]
    fn a_write_of_a_new_file_is_created_and_an_unchanged_one_is_nothing() {
        let args = json!({"path": "n.txt", "content": "hi\nthere\n"});
        let ex = preview("write", &args, |_| Ok(None)).unwrap();
        assert!(ex.created && ex.before.is_empty());
        assert_eq!(ex.after, "hi\nthere");
        assert!(preview("write", &args, file("hi\nthere\n")).is_none());
        assert!(preview("bash", &args, file("")).is_none());
    }
}
