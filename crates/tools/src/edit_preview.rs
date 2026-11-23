//! What an `edit` or `write` call would change, worked out before it runs, so the permission card
//! can show the diff it is asking about. Exact matching only, as the tool applies. A call that would
//! not resolve is not silenced: the card gets the tool's own sentence for why, over the text the
//! call asked to write, because a card that shows neither is a card the person cannot answer.

use serde_json::Value;

use crate::backend::{BackendError, ExecBackend};
use crate::edit::{FileEdit, FileText, ToolEditExcerpt, changed_span, occurrences};

/// Context and cap of the excerpt, the same the tool row's excerpt is taken with.
const CONTEXT: usize = 3;
const CAP: usize = 400;

/// The reader a preview takes, over a session's own backend: a file that is not there is `Ok(None)`,
/// and anything else is the reason it could not be read.
pub fn backend_reader(
    backend: &dyn ExecBackend,
) -> impl Fn(&str) -> Result<Option<Vec<u8>>, String> + '_ {
    move |path: &str| match backend.read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(BackendError::NotFound(_)) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

/// The excerpt a card draws from a change a tool has already worked out, bounded by the same context
/// and cap a tool row's excerpt is - so a tool that knows its own file (`notes`) does not restate
/// them.
pub fn excerpt_of(edit: &FileEdit) -> ToolEditExcerpt {
    edit.excerpt(CONTEXT, CAP)
}

/// The excerpt the call would produce, or `None` when it names no file, is not a file edit, or would
/// change nothing. `read` returns the file's bytes, `Ok(None)` when it does not exist.
///
/// An `edit` whose text does not resolve still gets an excerpt: its `note` is the sentence the tool
/// would answer with, and `before`/`after` are the call's proposal rather than a change to the file.
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
            match edited(&file.lf, args) {
                Ok(after) => (after != file.lf).then(|| file_edit(path, file.lf, after, false))?,
                // It would not apply, so there is no change to draw - and what the model asked to
                // write, over the tool's own reason, is the only thing that helps the reader.
                Err(why) => {
                    let mut ex =
                        file_edit(path, String::new(), proposal(args), false).excerpt(CONTEXT, CAP);
                    ex.note = why;
                    return Some(ex);
                }
            }
        }
        _ => return None,
    };
    Some(fe.excerpt(CONTEXT, CAP))
}

/// The text a call that would not apply asked to write, step by step: the only half of the change
/// that can be shown when the other half does not exist in the file as it is.
fn proposal(args: &Value) -> String {
    let steps: Vec<&Value> = match args.get("edits") {
        Some(list) => list
            .as_array()
            .map(|v| v.iter().collect())
            .unwrap_or_default(),
        None => vec![args],
    };
    steps
        .iter()
        .filter_map(|s| s.get("new_string").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
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

/// The text after the call's steps, applied in order as `edit` applies them. `Err` is the sentence
/// the tool itself would answer with when a step does not resolve - the card says the same words, so
/// the person reading it and the model retrying it are told one thing.
fn edited(text: &str, args: &Value) -> Result<String, String> {
    let listed = args.get("edits").and_then(Value::as_array);
    let steps: Vec<&Value> = match listed {
        Some(list) => list.iter().collect(),
        None => vec![args],
    };
    let path = args.get("path").and_then(Value::as_str).unwrap_or("");
    if steps.is_empty() {
        return Err("`edits` is empty".to_string());
    }
    // A batch names its step in the sentence and a lone call does not, exactly as the tool's own
    // two doors word it.
    let total = steps.len();
    let mut text = text.to_string();
    for (i, s) in steps.iter().enumerate() {
        let n = i + 1;
        let said = |what: &str| match total {
            1 => what.to_string(),
            _ => format!("step {n} of {total} {what}"),
        };
        let Some(new) = s.get("new_string").and_then(Value::as_str) else {
            return Err(said("has no `new_string`"));
        };
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
            _ => return Err(said("does not say what to change")),
        };
        let hits = occurrences(&text, needle);
        // The tool's own two sentences, word for word: a card whose reason reads differently from
        // the refusal that follows it is two accounts of one call.
        if hits.is_empty() {
            return Err(match total {
                1 => format!("`old_string` is not in `{path}`"),
                _ => said("found no match"),
            });
        }
        if hits.len() > 1 && !all {
            return Err(match total {
                1 => format!("`old_string` occurs {} times in `{path}`", hits.len()),
                _ => said(&format!("matches {} places", hits.len())),
            });
        }
        let mut out = String::with_capacity(text.len() + new.len());
        let mut from = 0;
        for hit in hits {
            // `get`, not an index: a byte offset that is not a char boundary ends the walk, and
            // this call then previews nothing rather than panicking over a file it cannot read.
            match text.get(from..hit) {
                Some(before) => out.push_str(before),
                None => {
                    return Err(format!(
                        "`{path}` is not the text this call was written against"
                    ));
                }
            }
            match insert {
                None => out.push_str(new),
                Some(true) => out.push_str(&format!("{new}{needle}")),
                Some(false) => out.push_str(&format!("{needle}{new}")),
            }
            from = hit + needle.len();
        }
        match text.get(from..) {
            Some(rest) => out.push_str(rest),
            None => {
                return Err(format!(
                    "`{path}` is not the text this call was written against"
                ));
            }
        }
        text = out;
    }
    Ok(text)
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
        // It would not apply, so there is no change to draw - and the card is still given the tool's
        // own sentence and the text the call asked to write, which is all there is to show.
        let ex = preview("edit", &twice, file("x x\n")).unwrap();
        assert_eq!(ex.note, "`old_string` occurs 2 times in `a`");
        assert!(ex.before.is_empty(), "nothing of the file is drawn: {ex:?}");
        assert_eq!(ex.after, "y");
    }

    /// The two sentences the `edit` tool answers with, so a card and the refusal that follows it
    /// cannot describe one call two ways. `no_match` and `ambiguous` are where the words come from.
    #[test]
    fn a_call_that_would_not_apply_carries_the_tools_own_words() {
        let missing = json!({"path": "a.rs", "old_string": "nowhere", "new_string": "x"});
        let ex = preview("edit", &missing, file("one\n")).unwrap();
        assert_eq!(ex.note, "`old_string` is not in `a.rs`");
        assert_eq!(
            ex.after, "x",
            "the prose it asked to write is what is drawn"
        );
        // A batch names its step, because a batch that failed at step 2 and one that failed at step
        // 1 are two different repairs.
        let batch = json!({"path": "a.rs", "edits": [
            {"old_string": "one", "new_string": "1"},
            {"old_string": "three", "new_string": "3"},
        ]});
        let ex = preview("edit", &batch, file("one\ntwo\n")).unwrap();
        assert_eq!(ex.note, "step 2 of 2 found no match");
        assert_eq!(
            ex.after, "1\n3",
            "every step's text, in the order it would be written"
        );
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
