//! `write` — put a file's whole contents there, atomically.
//!
//! The narrower of the two write tools, and the more dangerous, because its miss
//! is not "I could not find the text" but "I replaced everything". So the two
//! rules it keeps are about the cases where that is not what was meant:
//!
//! - **creating** a file needs no prior read, because there is nothing to have
//!   read;
//! - **overwriting** one does, and it is the same read-before-write and staleness
//!   check `edit` keeps ([`crate::files`]). grok-build's `search_replace` will
//!   overwrite a whole file from an empty `old_string` by default; that is the
//!   effect this tool exists to make explicit rather than reachable by accident.
//!
//! What it does not do, and both prior harnesses do: run a formatter over the
//! result and report the formatted text as what was written. That turns one tool
//! call into two writes and makes the returned content something the model did
//! not author. If a formatter should run, it is a tool call.

use serde_json::Value;

use crate::backend::BackendError;
use crate::edit::{FileEdit, FileText, changed_span, display_lines};
use crate::runtime::{Invocation, InvokeCtx, Tool};
use crate::schema::{Access, ToolSchema};

use super::{near_names, nearest_listing, render_listing};

const CONTEXT: usize = 3;

pub struct Write;

impl Tool for Write {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "write",
            "Write a file's whole contents. Give `path` and `content`. Creating a file \
             that does not exist needs nothing else; missing parent directories are \
             created. Overwriting one that exists **lands** — an unread file is not \
             refused, because a shared path is not this session's to have seen. What is \
             refused is a file **changed since this session read it**: somebody else's \
             write would be discarded unseen, and their contents come back with the \
             refusal. Content identical to what is already there is reported and the \
             file is left alone. To change part of a file, call `edit` instead.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "File to write, relative to the session root."},
                    "content": {"type": "string", "description": "The file's entire new contents."}
                },
                "required": ["path", "content"]
            }),
            Access::Write,
        )
    }

    fn invoke(&self, ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        let Some(path) = args.get("path").and_then(|v| v.as_str()) else {
            return Invocation::failed(
                "write needs a path",
                "call `write` again with `path` and `content`.",
            );
        };
        let Some(content) = args.get("content").and_then(|v| v.as_str()) else {
            return Invocation::failed(
                "write needs `content`",
                format!(
                    "`write` replaces `{path}`'s whole contents with `content`, and an \
                     absent `content` cannot mean an empty file — that has to be said. \
                     Nothing was written. To change part of a file, call `edit` with \
                     `old_string` and `new_string`."
                ),
            );
        };

        // Existing or new decides which rules apply, so it is the first thing
        // established and it is established from the backend rather than assumed.
        let existing = match ctx.backend.read(path) {
            Ok(b) => Some(b),
            Err(BackendError::NotFound(_)) => None,
            Err(BackendError::IsADirectory(_)) => {
                let entries = ctx.backend.list(path).unwrap_or_default();
                return Invocation::failed(
                    format!("`{path}` is a directory"),
                    render_listing(path, &entries, 60),
                )
                .with_note("its listing is below; call `write` again with a file path.");
            }
            Err(e @ BackendError::Outside(_)) => {
                return Invocation::failed(
                    e.to_string(),
                    "this session can only write inside its own root. Give a path \
                     relative to it.",
                );
            }
            Err(e) => return Invocation::failed(e.to_string(), String::new()),
        };

        match existing {
            None => create(ctx, path, content),
            Some(bytes) => overwrite(ctx, path, content, &bytes),
        }
    }
}

fn create(ctx: &mut InvokeCtx<'_>, path: &str, content: &str) -> Invocation {
    // A new file with no parent is a typo often enough to be worth naming: the
    // listing says whether `src/parser/` was meant to exist.
    let parent_missing = path
        .rsplit_once('/')
        .is_some_and(|(dir, _)| !dir.is_empty() && ctx.backend.stat(dir).is_none());

    if let Err(e) = ctx.backend.write(path, content.as_bytes()) {
        return write_failed(ctx, path, e);
    }
    ctx.files.record(path, content.as_bytes(), true);

    let lines = if content.is_empty() {
        0
    } else {
        content.lines().count()
    };
    let mut inv = Invocation::ok(format!(
        "created `{path}`: {} bytes, {lines} line(s).\n",
        content.len()
    ));
    if parent_missing {
        let (dir, entries) = nearest_listing(ctx.backend, path);
        let near = near_names(path, &entries, 5);
        inv = inv.with_note(format!(
            "`{path}`'s parent directory did not exist and was created. If that was not \
             intended, `{dir}` holds: {}{}",
            entries
                .iter()
                .take(20)
                .map(|e| e.name.clone())
                .collect::<Vec<_>>()
                .join(", "),
            if near.is_empty() {
                String::new()
            } else {
                format!(" — closest names: {}", near.join(", "))
            }
        ));
    }
    inv.edit = Some(FileEdit {
        path: path.to_string(),
        before: String::new(),
        after: content.to_string(),
        created: true,
        before_digest: crate::spill::content_hash(b""),
        after_digest: crate::spill::content_hash(content.as_bytes()),
        replacements: 1,
        changed: changed_span("", content),
    });
    inv
}

fn overwrite(ctx: &mut InvokeCtx<'_>, path: &str, content: &str, bytes: &[u8]) -> Invocation {
    let file = FileText::of(bytes);
    if file.lossy {
        return Invocation::failed(
            format!("`{path}` is not valid UTF-8"),
            "overwriting it would be a write over content this session cannot show you, \
             so the read-before-write check cannot be honest about it. Nothing was \
             written.",
        );
    }

    // **The read-before-overwrite refusal is gone (R14), and the arm below is not it.**
    //
    // It refused to overwrite a file this session had not read, on the premise that
    // *"a write lands on content the model is remembering rather than reading, and
    // everything it does not remember is silently deleted"*. The premise does not hold
    // for a path this session never saw: there is nothing to silently delete that
    // anybody believed was there. Measured — the refusal fired on
    // `/tmp/letibot-scratch-NNNN/msg-r10.txt`, **a file another session had created
    // seconds earlier in a shared scratch directory**, which is exactly the shape the
    // fleet is acquiring more of. "Has this session read it" is the wrong question
    // there, and the operator ruled the guard abandoned.
    //
    // **What stays is a different question**, and it was ruled separately rather than
    // deleted in the same diff: *did the file move between my read and my write?* That
    // one is not about what the model remembers — it is a fact about the file, and a
    // shared directory makes it **more** relevant rather than less, because a second
    // writer is the thing that changes a file under a read. A write over somebody
    // else's change would discard it with neither of them seeing it.
    let seen = ctx.files.seen(path);
    let now = crate::spill::content_hash(bytes);
    let refusal = match &seen {
        Some(s) if s.digest != now => Some((
            format!("`{path}` changed since this session read it"),
            format!(
                "it was {} bytes when it was read and is {} bytes now, so somebody else \
                 has written to it, and overwriting it would discard their change \
                 without either of you seeing it. The current contents are below and are \
                 now recorded as read.",
                s.bytes,
                bytes.len()
            ),
        )),
        _ => None,
    };

    if let Some((reason, why)) = refusal {
        ctx.files.record(path, bytes, true);
        let mut body = format!("{why}\n\n");
        for (i, l) in file.lf.lines().enumerate() {
            body.push_str(&format!("{:>6}| {l}\n", i + 1));
        }
        return Invocation::failed(reason, body).with_note("nothing was written.");
    }

    if file.lf == content.replace("\r\n", "\n") {
        // Not an error and not a write. Touching the file would move its mtime and
        // wake every watcher on the box for a change that is not one.
        return Invocation::ok(format!(
            "`{path}` already has exactly that content; the file was not touched.\n"
        ))
        .with_note("no write was performed, so nothing that watches this file was woken.");
    }

    let out = file.restore(content);
    if let Err(e) = ctx.backend.write(path, out.as_bytes()) {
        return write_failed(ctx, path, e);
    }
    ctx.files.record(path, out.as_bytes(), true);

    let after = out.replace("\r\n", "\n");
    let span = changed_span(&file.lf, &after);
    let lines: Vec<&str> = display_lines(&after);
    let from = span.first.saturating_sub(CONTEXT).max(1);
    let to = (span.last_after + CONTEXT).min(lines.len());
    let mut body = format!("`{path}` rewritten. {}\n\n", span.describe());
    for i in from..=to {
        body.push_str(&format!("{i:>6}| {}\n", lines[i - 1]));
    }

    let mut inv = Invocation::ok(body);
    if file.mixed {
        inv = inv.with_note(format!(
            "`{path}` had mixed line endings; the content was written exactly as given"
        ));
    }
    inv.edit = Some(FileEdit {
        path: path.to_string(),
        before: file.lf.clone(),
        after: after.clone(),
        created: false,
        before_digest: crate::spill::content_hash(file.lf.as_bytes()),
        after_digest: crate::spill::content_hash(after.as_bytes()),
        replacements: 1,
        changed: span,
    });
    inv
}

fn write_failed(ctx: &mut InvokeCtx<'_>, path: &str, e: BackendError) -> Invocation {
    let extra = if !ctx.backend.is_writable() {
        format!(
            "\n\nThis session's execution backend is read-only ({}). That is a second, \
             independent gate below §11's adjudication: the adjudicator admitted this \
             call and the backend is what refused. Nothing on disk changed.",
            ctx.backend.describe()
        )
    } else {
        "\n\nNothing on disk changed: the write is a temporary file renamed over the \
         target, so a failure leaves the original untouched."
            .to_string()
    };
    Invocation::failed(format!("could not write `{path}`: {e}"), extra)
}

#[cfg(test)]
mod tests {
    use crate::testing::writable_harness;

    #[test]
    fn a_new_file_needs_no_prior_read_and_is_readable_like_its_neighbours() {
        let mut h = writable_harness();
        let r = h.call("write", r#"{"path":"new.txt","content":"hello\n"}"#);
        assert!(r.is_grounded(), "{}", r.render());
        assert_eq!(h.read_file("new.txt"), "hello\n");
        let e = r.edit.as_ref().unwrap();
        assert!(e.created && e.before.is_empty());
        // The temp file is created 0600 so nothing can read a half-written file;
        // what lands must not keep that, or every file the agent creates is
        // unreadable to everything but the agent.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(h.root().join("new.txt"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o644, "a created file must be readable");
        }
    }

    #[test]
    fn missing_parent_directories_are_created_and_the_fact_is_reported() {
        let mut h = writable_harness();
        let r = h.call(
            "write",
            r#"{"path":"src/parser/mod.rs","content":"// x\n"}"#,
        );
        assert!(r.is_grounded(), "{}", r.render());
        assert_eq!(h.read_file("src/parser/mod.rs"), "// x\n");
        assert!(
            r.render().contains("did not exist and was created"),
            "{}",
            r.render()
        );
    }

    /// **R14: an unread file is written, and the write is disclosed.**
    ///
    /// The refusal this replaces asked *has this session read it*, which is a question
    /// about the session and not about the file — and it is the wrong question for a
    /// shared surface. Measured: it fired on a file another session had created seconds
    /// earlier in a scratch directory this one had never opened.
    ///
    /// What the row and the card must still show is that content was replaced, and that
    /// is asserted with the diff: the guard goes, the disclosure stays.
    #[test]
    fn overwriting_a_file_this_session_has_not_read_lands_and_the_change_is_disclosed() {
        let mut h = writable_harness();
        let r = h.call("write", r#"{"path":"README.md","content":"mine now\n"}"#);
        assert!(
            r.is_grounded(),
            "an unread file must be writable: {}",
            r.render()
        );
        assert_eq!(h.read_file("README.md"), "mine now\n");
        // **The disclosure survives the guard.** The operator still sees what was
        // replaced — the diff pair, with the old contents on the before side — which is
        // the half of this that was never about refusing anything.
        let e = r.edit.as_ref().expect("the write carries its before/after pair");
        assert!(
            e.before.contains("letibot"),
            "the replaced content is the before side: {:?}",
            e.before
        );
        assert!(e.after.contains("mine now"), "{:?}", e.after);
        assert!(!e.created, "this was an overwrite, not a creation");
    }

    #[test]
    fn identical_content_does_not_touch_the_file() {
        let mut h = writable_harness();
        h.call("read", r#"{"path":"README.md"}"#);
        let before = h.mtime("README.md");
        let r = h.call(
            "write",
            r#"{"path":"README.md","content":"letibot\na harness\n"}"#,
        );
        assert!(r.is_grounded());
        assert!(r.render().contains("was not touched"), "{}", r.render());
        assert_eq!(h.mtime("README.md"), before);
        assert!(
            r.edit.is_none(),
            "nothing changed, so the head has no diff to draw"
        );
    }

    #[test]
    fn a_stale_overwrite_is_refused_with_the_other_writer_s_content() {
        let mut h = writable_harness();
        h.call("read", r#"{"path":"README.md"}"#);
        h.write_file("README.md", "theirs\n");
        let r = h.call("write", r#"{"path":"README.md","content":"mine\n"}"#);
        let out = r.render();
        assert!(out.contains("changed since"), "{out}");
        assert!(out.contains("theirs"), "{out}");
        assert_eq!(h.read_file("README.md"), "theirs\n");
    }

    #[test]
    fn a_write_with_no_content_argument_does_not_become_an_empty_file() {
        let mut h = writable_harness();
        h.call("read", r#"{"path":"README.md"}"#);
        let r = h.call("write", r#"{"path":"README.md"}"#);
        assert!(!r.is_grounded());
        assert_eq!(h.read_file("README.md"), "letibot\na harness\n");
    }
}
