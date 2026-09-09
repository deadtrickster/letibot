//! `read` — the text of a file, and three misses that answer themselves.
//!
//! The oracle case this is modelled on is the offset one, and it is worth naming
//! because it is the closest analogue of the measured example: the model guessed a
//! line range, the range is not there, and the tool **hands back the file anyway**
//! with a line saying the guess was wrong and what the real extent is. A bare
//! "offset out of range" costs a whole extra call to learn a number the tool
//! already had.

use serde_json::Value;

use crate::backend::BackendError;
use crate::runtime::{Invocation, InvokeCtx, Tool};
use crate::schema::{Access, ToolSchema};

use super::{near_names, nearest_listing, render_listing, text_of};

pub struct Read;

impl Tool for Read {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "read",
            "Return the text of a file, with line numbers. Give `path`; optionally \
             `offset` (the first line, 1-based) and `limit` (how many lines). A path \
             that does not exist comes back with the nearest directory's listing \
             rather than an error, and a directory comes back as its listing, so a \
             wrong guess does not need a second call.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "File to read, relative to the session root."},
                    "offset": {"type": "integer", "description": "First line to return, 1-based."},
                    "limit": {"type": "integer", "description": "How many lines to return."}
                },
                "required": ["path"]
            }),
            Access::Read,
        )
    }

    fn invoke(&self, ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        let Some(path) = args.get("path").and_then(|v| v.as_str()) else {
            return Invocation::failed(
                "read needs a path",
                "call `read` again with `path` set to a file, relative to the session root.",
            );
        };

        let bytes = match ctx.backend.read(path) {
            Ok(b) => b,
            Err(BackendError::IsADirectory(_)) => return directory(ctx, path),
            Err(BackendError::NotFound(_)) => return missing(ctx, path),
            Err(e @ BackendError::Outside(_)) => {
                return Invocation::failed(
                    e.to_string(),
                    "this session can only read inside its own root. Give a path relative \
                     to it.",
                );
            }
            Err(e) => {
                // A path that does not exist reaches here on some backends; treat
                // any read failure on a non-existent path as the miss it is.
                if ctx.backend.stat(path).is_none() {
                    return missing(ctx, path);
                }
                return Invocation::failed(e.to_string(), String::new());
            }
        };

        if bytes.len() > ctx.limits.max_file_bytes {
            ctx.progress(format!("{path}: {} bytes", bytes.len()));
        }

        let (text, lossy) = text_of(&bytes);
        let lines: Vec<&str> = text.lines().collect();
        let total = lines.len();

        let want_offset = args
            .get("offset")
            .and_then(|v| v.as_i64())
            .unwrap_or(1)
            .max(1) as usize;
        let limit = args
            .get("limit")
            .and_then(|v| v.as_i64())
            .filter(|n| *n > 0)
            .map(|n| n as usize);

        let mut notes = Vec::new();
        if lossy {
            notes.push(format!(
                "{path} is not valid UTF-8; undecodable bytes are shown as U+FFFD"
            ));
        }

        // The clause 1 case: the guessed offset is past the end. Hand back the file
        // from the top **anyway**, and say what the real extent is.
        let offset = if want_offset > total && total > 0 {
            notes.push(format!(
                "offset {want_offset} is past the end of {path}, which has {total} line(s); \
                 the file is shown from line 1 instead"
            ));
            1
        } else {
            want_offset
        };

        let end = match limit {
            Some(l) => (offset - 1 + l).min(total),
            None => total,
        };
        let shown = &lines[(offset - 1).min(total)..end];

        let mut body = String::new();
        for (i, line) in shown.iter().enumerate() {
            body.push_str(&format!("{:>6}| {line}\n", offset + i));
        }
        if end < total {
            notes.push(format!(
                "showing lines {offset}–{end} of {total}; call read again with offset={} \
                 for the rest",
                end + 1
            ));
        }
        if total == 0 {
            notes.push(format!("{path} is empty ({} bytes)", bytes.len()));
        }

        let mut inv = Invocation::ok(body);
        inv.notes = notes;
        inv
    }
}

/// A path that does not exist: the nearest listing, plus the near-miss names.
fn missing(ctx: &mut InvokeCtx<'_>, path: &str) -> Invocation {
    let (dir, entries) = nearest_listing(ctx.backend, path);
    let near = near_names(path, &entries, 5);
    let mut body = render_listing(&dir, &entries, 40);
    if !near.is_empty() {
        body.push_str(&format!("\nclosest names here: {}\n", near.join(", ")));
    }
    // Failed, not Abstained: the tool was asked for a specific file and there is
    // no file. What makes it clause 1 is the body, not the class.
    Invocation::failed(format!("no file at `{path}`"), body).with_note(format!(
        "`{path}` does not exist; the listing of `{dir}` is below. Call read again \
         with one of these paths — do not guess a second one."
    ))
}

/// A directory: hand back the listing rather than an error. This is the "and
/// handed back the whole page anyway" move, one tool over.
fn directory(ctx: &mut InvokeCtx<'_>, path: &str) -> Invocation {
    let entries = ctx.backend.list(path).unwrap_or_default();
    let body = render_listing(path, &entries, 200);
    Invocation::ok(body).with_note(format!(
        "`{path}` is a directory, so its listing is below rather than its text. Call \
         read again with one of these files."
    ))
}

#[cfg(test)]
mod tests {
    use crate::testing::harness;

    #[test]
    fn a_hit_is_numbered_from_the_offset() {
        let mut h = harness();
        let r = h.call("read", r#"{"path":"src/lib.rs","offset":2,"limit":1}"#);
        assert!(r.payload.contains("     2| "), "{}", r.payload);
        assert!(r.payload.lines().count() == 1);
    }

    #[test]
    fn an_offset_past_the_end_returns_the_file_anyway_and_says_so() {
        // The measured shape: the guess is refused *and* the content is handed
        // back, so the model corrects itself inside the same call.
        let mut h = harness();
        let r = h.call("read", r#"{"path":"src/lib.rs","offset":9000}"#);
        assert!(r.is_grounded(), "the call must still succeed");
        assert!(r.payload.contains("     1| "), "{}", r.payload);
        let notes = r.notes.join(" ");
        assert!(notes.contains("past the end"), "{notes}");
        assert!(notes.contains("line(s)"), "{notes}");
    }

    #[test]
    fn a_missing_path_returns_the_surrounding_listing_and_the_near_miss() {
        let mut h = harness();
        let r = h.call("read", r#"{"path":"src/libb.rs"}"#);
        assert!(r.payload.contains("lib.rs"), "{}", r.payload);
        assert!(r.payload.contains("closest names"), "{}", r.payload);
    }

    #[test]
    fn a_directory_returns_its_listing_rather_than_an_error() {
        let mut h = harness();
        let r = h.call("read", r#"{"path":"src"}"#);
        assert!(r.is_grounded());
        assert!(r.payload.contains("lib.rs"), "{}", r.payload);
    }
}
