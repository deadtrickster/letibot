//! `read`: the text of a file, and three misses that answer themselves.
//! A guessed line range that is not there hands back the file anyway, with a line saying what the
//! real extent is: "offset out of range" costs a call to learn a number the tool already had.

use letibot_transcript::text::chars_head;
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
            "Return a file's text, with line numbers — or an image (PNG, JPEG, \
             GIF, WebP) or a PDF document itself, attached. Give `path`; optionally \
             `offset` (the first line, 1-based) and `limit` (how many lines), or \
             `ranges` for SEVERAL windows of one file in a single call. Without a \
             limit, at most 200 lines and a few tens of KiB are returned per call, and \
             the result says which offset continues the file; very long lines are \
             clipped. A path that does not exist comes back with the nearest \
             directory's listing rather than an error, and a directory comes back as \
             its listing, so a wrong guess does not need a second call. Prefer this to \
             `sed -n 'A,Bp'`: the shell cites no line numbers, never says where the \
             file continues, and a wrong range prints nothing — which reads like a \
             right one.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "File to read, relative to the session root."},
                    "offset": {"type": "integer", "description": "First line to return, 1-based."},
                    "limit": {"type": "integer", "description": "How many lines to return."},
                    "column_offset": {"type": "integer", "description": "Zero-based character offset into line `offset`. With it (or `columns`) the call reads that one line, from this character on, so a line too long to show whole is read in pieces and each piece can be edited."},
                    "columns": {"type": "integer", "description": "How many characters of line `offset` to return from `column_offset`; at most 32768, the default."},
                    "pages": {"type": "string", "description": "For a PDF: the pages whose TEXT to return instead of the document, as `3` or `1-5` (1-based). Use it when the file is too big to send whole and only some of it matters."},
                    "ranges": {
                        "type": "array",
                        "description": "Several windows of THIS file in one call, instead of one `read` per window. Each is `offset` (1-based) and optional `limit`. Use it when you already know two or three places to look — it is one call and one gate decision instead of several. Overrides `offset`/`limit`.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "offset": {"type": "integer"},
                                "limit": {"type": "integer"}
                            }
                        }
                    }
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

        // The whole file is read before a window of it is shown, so a model shard or a
        // database would be read whole first. Sized before it is opened; the ceiling is far
        // above any source file.
        if let Some(e) = ctx.backend.stat(path)
            && !e.is_dir
            && e.bytes > super::grep::FILE_CEILING * 16
        {
            return Invocation::failed(
                format!(
                    "{path} is {} MiB, which is not a file `read` shows",
                    e.bytes / (1024 * 1024)
                ),
                "a file this size is data, not text. `bash` with `head -c`, `xxd` or \
                 the tool that made it is the way to look inside.",
            );
        }

        let mut bytes = match ctx.backend.read(path) {
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
            // The sentence already names the path, the reason and the key that would change
            // it, and it is the one a command's own absence note gives.
            Err(e @ BackendError::Fenced(_)) => {
                return Invocation::failed(e.to_string(), String::new());
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

        // A picture is read as a picture, not as lossy text. It has to be here, above the
        // decode: below, an image comes back as a wall of U+FFFD, and a model reading a
        // screenshot as noise concludes it has no vision.
        if let Some(m) = crate::media::Media::of(path, &bytes) {
            // The far end is the authority on what it can read, and this is the sentence it
            // gets. `summary` is the same string a head draws, so the row and the model are
            // told the same thing.
            return Invocation {
                outcome: letibot_transcript::ToolOutcome::Ok,
                payload: format!(
                    "{path} — {}\nThe image is attached to this result. Look at it and answer \
                     from what is in it; if the attachment did not arrive, say so rather than \
                     guessing at the contents.",
                    m.summary()
                ),
                notes: Vec::new(),
                edits: Vec::new(),
                needs_in_view: Vec::new(),
                media: Some(m),
            };
        }

        // A document is read as a document, for the same reason a picture is: below, a PDF is a
        // wall of `%PDF-1.4` and compressed streams, and a model reading that concludes the file
        // is broken. Over the cap the SAME bytes are extracted to text here, never stored again.
        let mut doc_note: Option<String> = None;
        if let Some(m) = crate::media::Media::document(path, &bytes) {
            if (m.bytes as u64) <= ctx.limits.max_document_bytes {
                return Invocation {
                    outcome: letibot_transcript::ToolOutcome::Ok,
                    payload: format!(
                        "{path} — {}\nThe document is attached to this result. Read it and answer \
                         from what is in it; if the attachment did not arrive, say so rather \
                         than guessing at the contents.",
                        m.summary()
                    ),
                    notes: Vec::new(),
                    edits: Vec::new(),
                    needs_in_view: Vec::new(),
                    media: Some(m),
                };
            }
            let cap = ctx.limits.max_document_bytes / (1024 * 1024);
            match letibot_transcript::pdf::markdown(&bytes, path) {
                Some(text) => {
                    doc_note = Some(format!(
                        "{path} is {} KiB, over the {cap} MiB a document may be sent whole, so \
                         its text is shown here instead",
                        bytes.len() / 1024
                    ));
                    bytes = text.into_bytes();
                }
                // No extractor on this box: say why, in the one sentence the model would
                // otherwise have to guess, rather than showing the PDF's bytes as noise.
                None => {
                    return Invocation {
                        outcome: letibot_transcript::ToolOutcome::Ok,
                        payload: format!(
                            "{path} — {} — is over the {cap} MiB a document may be sent whole, \
                             and this box has no `pdftotext` to extract it with, so its text \
                             cannot be read here. Say that the document was not read.",
                            m.summary()
                        ),
                        notes: Vec::new(),
                        edits: Vec::new(),
                        needs_in_view: Vec::new(),
                        media: None,
                    };
                }
            }
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

        let column_offset = args.get("column_offset").and_then(|v| v.as_i64());
        let columns = args.get("columns").and_then(|v| v.as_i64());
        if column_offset.is_some() || columns.is_some() {
            if args.get("ranges").is_some() {
                return Invocation::failed(
                    "`column_offset` and `columns` read one line, and `ranges` reads several",
                    "call `read` with `offset` and `column_offset` (and optionally `columns`), \
                     without `ranges`.",
                );
            }
            let col = column_offset.unwrap_or(0).max(0) as usize;
            let want = columns
                .filter(|n| *n > 0)
                .map_or(usize::MAX, |n| n as usize);
            return column_window(ctx, path, &bytes, &lines, want_offset, col, want, lossy);
        }

        let mut notes = Vec::new();
        if let Some(said) = doc_note {
            notes.push(said);
        }
        if lossy {
            notes.push(format!(
                "{path} is not valid UTF-8; undecodable bytes are shown as U+FFFD"
            ));
        }

        // The guessed offset is past the end: hand back the file from the top anyway, and say
        // what the real extent is.
        let offset = if want_offset > total && total > 0 {
            notes.push(format!(
                "offset {want_offset} is past the end of {path}, which has {total} line(s); \
                 the file is shown from line 1 instead"
            ));
            1
        } else {
            want_offset
        };

        // How far this call would reach if bytes were free. An explicit `limit` is the model's
        // own ask and is taken as given; the absence of one is where the default cap applies,
        // and a full thousand-line file in one call is the shape that eats a session's context.
        let want_end = match limit {
            Some(l) => (offset - 1 + l).min(total),
            None => (offset - 1 + ctx.limits.max_read_lines).min(total),
        };

        // Beyond the line count, a line longer than the clip is shown clipped and counted, and the
        // byte budget stops the call early. `ranges` batches lookups into one round trip. A window
        // bounded by its own `limit` clips at the byte budget, so a long line can be read whole.
        let windows: Vec<(usize, usize, bool)> = match args.get("ranges").and_then(|v| v.as_array())
        {
            Some(rs) if !rs.is_empty() => rs
                .iter()
                .map(|r| {
                    let o = r
                        .get("offset")
                        .and_then(|v| v.as_i64())
                        .filter(|n| *n > 0)
                        .map(|n| n as usize)
                        .unwrap_or(1)
                        .min(total.max(1));
                    match r.get("limit").and_then(|v| v.as_i64()).filter(|n| *n > 0) {
                        Some(l) => (o, (o - 1 + l as usize).min(total), true),
                        None => (o, (o - 1 + ctx.limits.max_read_lines).min(total), false),
                    }
                })
                .collect(),
            _ => vec![(offset, want_end, limit.is_some())],
        };

        let mut body = String::new();
        let mut truncated = 0usize;
        let mut byte_capped = false;
        let mut end = offset - 1;
        let mut first_window = true;
        // The lines this call displayed in full, for the shown-lines ledger. A
        // line clipped at the character cap is not added: the model saw a prefix
        // of it, and an edit aimed from that prefix is aimed at a memory.
        let mut shown: Vec<usize> = Vec::new();
        // Lines shown as a prefix: the line and the prefix's length in bytes and characters.
        let mut prefixes: Vec<(usize, usize, usize)> = Vec::new();
        let mut named_clip: Option<(usize, usize)> = None;
        for &(w_offset, w_end, named) in &windows {
            let line_cap = if named {
                ctx.limits.max_read_bytes
            } else {
                ctx.limits.max_read_line_chars
            };
            // A gap between windows is marked, so two regions are never read as
            // one continuous stretch of the file.
            if !first_window && !byte_capped {
                body.push_str("   …\n");
            }
            first_window = false;
            for (i, line) in lines[(w_offset - 1).min(total)..w_end].iter().enumerate() {
                let (shown_line, clipped) = clip(line, line_cap);
                let chunk = format!("{:>6}| {shown_line}\n", w_offset + i);
                if !body.is_empty() && body.len() + chunk.len() > ctx.limits.max_read_bytes {
                    byte_capped = true;
                    break;
                }
                if clipped {
                    truncated += 1;
                    let head = chars_head(line, line_cap).len();
                    prefixes.push((w_offset + i, head, line_cap));
                    if named {
                        named_clip.get_or_insert((w_offset + i, line_cap));
                    }
                } else {
                    shown.push(w_offset + i);
                }
                body.push_str(&chunk);
                end = w_offset + i;
            }
            if byte_capped {
                break;
            }
        }

        // The shown-lines ledger, recorded at the moment of the display: a read that showed every
        // line marks the whole file, a capped one marks only the lines the model saw, and the
        // whole-file digest is recorded either way because the bytes were read.
        let whole_file =
            end == total && !byte_capped && truncated == 0 && (total == 0 || end >= offset);
        if whole_file {
            ctx.files.mark_whole(path, &bytes);
        } else {
            ctx.files.mark_read(path, &bytes, &shown);
            for &(n, head, _) in &prefixes {
                ctx.files.mark_part(path, &bytes, n, 0, head);
            }
        }

        if let Some((n, at)) = named_clip {
            notes.push(format!(
                "line {n} is longer than {at} characters and is clipped there; call read \
                 with offset={n} and column_offset={at} for the rest"
            ));
        } else if truncated > 0 {
            notes.push(format!(
                "{truncated} line(s) were longer than read shows and are clipped; read one \
                 with offset=N and limit=1 to see it whole (up to {} characters), and past \
                 that with column_offset, or grep the file for the text you need",
                ctx.limits.max_read_bytes
            ));
        }
        if byte_capped {
            // The byte cap is the reader's number, not the model's: the offset is the whole of
            // what it needs to carry on, and a size here invites sizing work by the window.
            notes.push(format!(
                "stopped before line {}; call read again with offset={} to continue",
                end + 1,
                end + 1
            ));
        } else if end < total && limit.is_none() {
            // Only when the cap bound the output: a caller that passed `limit` chose this window
            // and knows where it ends, while the 200-line cap's boundary is OURS and silence would
            // read as "that was the whole file".
            if limit.is_some() {
                notes.push(format!(
                    "showing lines {offset}–{end} of {total}; call read again with \
                     offset={} for the rest",
                    end + 1
                ));
            } else {
                // The same sentence as the limit branch above, and no restating of the cap:
                // that is in this tool's schema, which sits in the stable prefix of every turn,
                // and the numbers in this line already imply it.
                notes.push(format!(
                    "showing lines {offset}–{end} of {total}; call read again with \
                     offset={} for the rest",
                    end + 1
                ));
            }
        }
        if total == 0 {
            notes.push(format!("{path} is empty ({} bytes)", bytes.len()));
        }

        let mut inv = Invocation::ok(body);
        inv.notes = notes;
        inv
    }
}

/// One line from character `col`, `want` characters of it at most and never more than the byte
/// budget's worth. The piece shown is marked in the ledger, so an edit inside it lands.
#[allow(clippy::too_many_arguments)] // one call site; a struct would only rename the arguments
fn column_window(
    ctx: &mut InvokeCtx<'_>,
    path: &str,
    bytes: &[u8],
    lines: &[&str],
    n: usize,
    col: usize,
    want: usize,
    lossy: bool,
) -> Invocation {
    let Some(line) = lines.get(n - 1) else {
        return Invocation::failed(
            format!("{path} has no line {n}; it has {} line(s)", lines.len()),
            "call `read` with an `offset` inside the file.",
        );
    };
    let len = line.chars().count();
    if col > len || (col == len && len > 0) {
        return Invocation::failed(
            format!("column_offset {col} is past the end of line {n}, which has {len} characters"),
            "call `read` with a smaller `column_offset`, or without one to see the line from \
             its start.",
        );
    }
    let take = want.min(ctx.limits.max_read_bytes).min(len - col);
    let byte_at = |c: usize| line.char_indices().nth(c).map_or(line.len(), |(b, _)| b);
    let (from, to) = (byte_at(col), byte_at(col + take));
    let end = col + take;
    let mut notes = Vec::new();
    if lossy {
        notes.push(format!(
            "{path} is not valid UTF-8; undecodable bytes are shown as U+FFFD"
        ));
    }
    if col > 0 || end < len {
        notes.push(if end < len {
            format!(
                "line {n}, characters {col}-{end} of {len}; call read with offset={n} and \
                 column_offset={end} for the rest"
            )
        } else {
            format!("line {n}, characters {col}-{end} of {len}, to its end")
        });
    }
    if from == 0 && to == line.len() {
        ctx.files.mark_read(path, bytes, &[n]);
    } else {
        ctx.files.mark_part(path, bytes, n, from, to);
    }
    let mut inv = Invocation::ok(format!("{n:>6}| {}\n", &line[from..to]));
    inv.notes = notes;
    inv
}

/// The first `max` characters of a line, whether anything was cut, and the cap named: a reader
/// of the payload is the one who has to go and read the whole line.
fn clip(line: &str, max: usize) -> (String, bool) {
    if line.chars().count() <= max {
        return (line.to_string(), false);
    }
    (
        format!(
            "{} … [line clipped at {max} characters]",
            chars_head(line, max)
        ),
        true,
    )
}

/// A path that does not exist: the nearest listing, plus the near-miss names.
fn missing(ctx: &mut InvokeCtx<'_>, path: &str) -> Invocation {
    let (dir, entries) = nearest_listing(ctx.backend, path);
    let near = near_names(path, &entries, 5);
    let mut body = render_listing(&dir, &entries, 40);
    if !near.is_empty() {
        body.push_str(&format!("\nclosest names here: {}\n", near.join(", ")));
    }
    // Failed, not Abstained: the tool was asked for a specific file and there is no
    // file. The body is what makes the miss answer itself, not the class.
    // The miss is the outcome's and the listing names its own directory; the note is the step.
    Invocation::failed(format!("no file at `{path}`"), body)
        .with_note("Call read again with one of these paths — do not guess a second one.")
}

/// A directory: hand back its listing rather than an error, which is the same move as a miss.
fn directory(ctx: &mut InvokeCtx<'_>, path: &str) -> Invocation {
    let entries = ctx.backend.list(path).unwrap_or_default();
    let body = render_listing(path, &entries, 200);
    Invocation::ok(body).with_note(format!(
        "`{path}` is a directory, so its listing is below rather than its text. Call \
         read again with one of these files."
    ))
}

/// A `pages` argument: one page (`3`) or a range (`1-5`), one-based, as a reader counts them.
/// `None` for anything else, which is a refusal rather than a guess at what was meant.
fn parse_pages(spec: &str) -> Option<(u32, u32)> {
    let spec = spec.trim();
    let (from, to) = match spec.split_once('-') {
        Some((a, b)) => (a.trim().parse().ok()?, b.trim().parse().ok()?),
        None => {
            let n: u32 = spec.parse().ok()?;
            (n, n)
        }
    };
    (from >= 1 && to >= from).then_some((from, to))
}

#[cfg(test)]
mod tests {

    /// Does `read` hand the model the file's bytes, spaces and all?
    /// Asked because a model that keeps failing on missing spaces is either shown a lossy file or
    /// losing them on the way in, and this is the cheapest place to find out.
    #[test]
    fn a_read_with_a_limit_is_never_told_about_the_cap_it_did_not_hit() {
        let mut h = crate::testing::harness();
        let body: String = (1..=300).map(|i| format!("line {i}\n")).collect();
        h.write_file("big.txt", &body);

        let asked = h.call("read", r#"{"path":"big.txt","limit":50}"#);
        let notes = asked.notes.join(" ");
        eprintln!("limit=50 note: {notes:?}");
        assert_eq!(
            asked.payload.lines().count(),
            50,
            "it returned {}",
            asked.payload.lines().count()
        );
        // It asked for fifty and got fifty: the window is its own, so there is
        // nothing to report. No note at all.
        assert!(
            notes.is_empty(),
            "a caller that chose its own window needs no note: {notes:?}"
        );

        // And with no limit, where the cap DID bind, it still says where to continue,
        // without restating the number, which is in the schema.
        let unasked = h.call("read", r#"{"path":"big.txt"}"#);
        let n2 = unasked.notes.join(" ");
        eprintln!("no-limit note: {n2}");
        assert!(!n2.contains("at most"), "{n2}");
        assert!(n2.contains("offset=201"), "{n2}");
    }

    #[test]
    fn the_description_says_an_image_comes_back_as_an_image() {
        // Advertised, or it is never asked for: a tool that attaches an image for any image
        // path but describes itself as "return the text of a file" gives a model with a
        // screenshot no reason to try it.
        let d = crate::runtime::Tool::schema(&super::Read).description;
        for needle in ["image", "PNG", "JPEG", "attached"] {
            assert!(d.contains(needle), "{needle:?} missing from: {d}");
        }
        assert_eq!(crate::schema::lint_description(&d), vec![]);
    }

    /// A picture comes back as a picture: the same `read`, the same verb, the same permission,
    /// and only what comes back differs - a media channel carrying the bytes, and a payload that
    /// says what was read instead of a wall of U+FFFD.
    #[test]
    fn an_image_read_returns_the_bytes_and_a_sentence_rather_than_lossy_text() {
        let mut h = crate::testing::harness();
        // A real PNG from the shared fixture: `Media::of` refuses a file that is a header and
        // no picture, and a provider then refuses that too.
        let png = letibot_transcript::testing::png(640, 480);
        std::fs::write(h.root().join("shot.png"), &png).expect("fixture write");

        let got = h.call("read", r#"{"path":"shot.png"}"#);
        let media = got
            .media
            .as_ref()
            .expect("the image did not reach the result at all");
        assert_eq!(media.mime, "image/png");
        assert_eq!((media.width, media.height), (Some(640), Some(480)));
        assert!(
            media.data_ref.starts_with("data:image/png;base64,"),
            "the model server takes a data: URI and refuses a bare path: {}",
            &media.data_ref[..40.min(media.data_ref.len())]
        );
        // The payload is the sentence, not the bytes: a payload that inlined the base64 would
        // put a megabyte through every renderer and every scrollback.
        assert!(got.payload.contains("shot.png"), "{}", got.payload);
        assert!(got.payload.contains("640×480"), "{}", got.payload);
        assert!(
            !got.payload.contains("U+FFFD"),
            "the image was decoded as text: {}",
            got.payload
        );
        assert!(
            got.payload.len() < 400,
            "the payload is carrying the bytes: {} chars",
            got.payload.len()
        );
        // And the sentence the model needs so it can say the picture did not arrive.
        assert!(
            got.payload.contains("attached"),
            "the payload does not tell the model the image is attached: {}",
            got.payload
        );
    }

    /// A text file is untouched by any of that: the control for the test above, and the same
    /// call with only the image differing.
    #[test]
    fn a_text_file_still_comes_back_as_its_own_lines_with_no_media() {
        let mut h = crate::testing::harness();
        h.write_file("notes.txt", "first line\nsecond line\n");
        let got = h.call("read", r#"{"path":"notes.txt"}"#);
        assert!(got.media.is_none(), "a text file claimed a media channel");
        assert!(got.payload.contains("first line"), "{}", got.payload);
        // A binary file that is not an image keeps lossy text with a note. Refusing it
        // wholesale is a separate ruling; this records which rule is in force, so the
        // difference is a decision rather than a drift.
        let mut junk = vec![0u8, 159, 146, 150];
        junk.extend_from_slice(&[0xff; 64]);
        std::fs::write(h.root().join("blob.bin"), &junk).expect("fixture write");
        let bin = h.call("read", r#"{"path":"blob.bin"}"#);
        assert!(bin.media.is_none(), "a non-image claimed a media channel");
    }

    /// A PDF comes back as a DOCUMENT: the same media channel a picture rides, with the file's own
    /// type, the name a door's request needs, and a sentence instead of a wall of PDF bytes. The
    /// file is named `.txt`, because the bytes are the claim and the name is not.
    #[test]
    fn a_pdf_read_returns_a_document_and_a_sentence_rather_than_its_own_bytes() {
        let mut h = crate::testing::harness();
        let pdf = letibot_transcript::testing::pdf("the answer is 42");
        std::fs::write(h.root().join("memo.txt"), &pdf).expect("fixture write");

        let got = h.call("read", r#"{"path":"memo.txt"}"#);
        let media = got
            .media
            .as_ref()
            .expect("the document did not reach the result at all");
        assert_eq!(media.mime, "application/pdf");
        assert!(media.is_document());
        assert!(
            media.data_ref.starts_with("data:application/pdf;base64,"),
            "{}",
            &media.data_ref[..40.min(media.data_ref.len())]
        );
        assert!(got.payload.contains("memo.txt"), "{}", got.payload);
        assert!(
            got.payload.contains("document application/pdf"),
            "{}",
            got.payload
        );
        assert!(got.payload.contains("attached"), "{}", got.payload);
        assert!(
            !got.payload.contains("%PDF-"),
            "the payload is carrying the document's bytes: {} chars",
            got.payload.len()
        );
    }

    /// Over the cap the same bytes are read as text instead - never stored twice, never refused
    /// silently: the note says the document was too big to send whole.
    #[test]
    fn a_document_over_the_cap_is_read_as_text_and_says_why() {
        let mut h = crate::testing::harness();
        let pdf = letibot_transcript::testing::pdf("a word in a small document");
        std::fs::write(h.root().join("big.pdf"), &pdf).expect("fixture write");
        // One byte under the fixture's size: the cap binds on the file, not on the page count.
        h.rt.limits.max_document_bytes = pdf.len() as u64 - 1;

        let got = h.call("read", r#"{"path":"big.pdf"}"#);
        assert!(got.media.is_none(), "an over-cap document was sent anyway");
        let notes = got.notes.join(" ");
        assert!(notes.contains("over the"), "{notes}");
        // With `pdftotext` on the box the words are here, and without it the sentence says so.
        // Both are the same ruling: the model is told, and nothing pretends to be the document.
        assert!(
            got.payload.contains("a word in a small document")
                || got.payload.contains("no `pdftotext`"),
            "neither the text nor the reason: {}",
            got.payload
        );
    }

    /// A PDF is read as a PDF: the description has to say so, or a model with a document in front
    /// of it never asks the tool that can hand it over.
    #[test]
    fn the_description_says_a_pdf_comes_back_as_a_document() {
        let d = crate::runtime::Tool::schema(&super::Read).description;
        for needle in ["PDF", "document", "attached"] {
            assert!(d.contains(needle), "{needle:?} missing from: {d}");
        }
        assert_eq!(crate::schema::lint_description(&d), vec![]);
    }

    #[test]
    fn read_returns_every_space_the_file_has() {
        let mut h = crate::testing::harness();
        let body = "fn a() {\n    if !text.is_empty() && x != y {\n        v.filter(|l| !l.trim().is_empty())\n    }\n}\n";
        h.write_file("s.rs", body);
        let r = h.call("read", r#"{"path":"s.rs"}"#);
        for needle in [
            "if !text.is_empty() && x != y {",
            "v.filter(|l| !l.trim().is_empty())",
        ] {
            assert!(
                r.payload.contains(needle),
                "read did not return `{needle}` intact — the space loss is in the TOOL:\n{}",
                r.payload
            );
        }
    }
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
        // The guess is refused and the content is handed back, so the model corrects itself
        // inside the same call.
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
        // The miss once: the outcome says it, the note says only what to do.
        let card = r.render();
        assert_eq!(card.matches("src/libb.rs").count(), 1, "{card}");
    }

    #[test]
    fn a_directory_returns_its_listing_rather_than_an_error() {
        let mut h = harness();
        let r = h.call("read", r#"{"path":"src"}"#);
        assert!(r.is_grounded());
        assert!(r.payload.contains("lib.rs"), "{}", r.payload);
    }

    #[test]
    fn a_file_past_the_line_cap_is_capped_and_the_note_names_the_offset() {
        // A thousand-line file read whole is twenty thousand tokens of one call. The cap is the
        // default, and the note is the continuation.
        let mut h = harness();
        let body: String = (1..=300)
            .map(|i| format!("line {i}\n"))
            .collect::<Vec<_>>()
            .join("");
        h.write_file("big.txt", &body);
        let r = h.call("read", r#"{"path":"big.txt"}"#);
        assert!(r.is_grounded());
        assert_eq!(r.payload.lines().count(), 200, "{}", r.payload);
        let notes = r.notes.join(" ");
        // The note says where to continue and how much there is, and does not restate the cap:
        // that is in this tool's schema, in the stable prefix of every turn, and repeating it per
        // call spends context on something already said.
        assert!(notes.contains("of 300"), "{notes}");
        assert!(notes.contains("offset=201"), "{notes}");
        assert!(
            !notes.contains("at most"),
            "the cap belongs in the schema, not on every result: {notes}"
        );
    }

    #[test]
    fn an_explicit_limit_is_taken_as_given() {
        // The cap is what happens when the call does not say. A call that names
        // a limit has made its own decision; the byte cap below is what still
        // bounds it.
        let mut h = harness();
        let body: String = (1..=300)
            .map(|i| format!("line {i}\n"))
            .collect::<Vec<_>>()
            .join("");
        h.write_file("big.txt", &body);
        let r = h.call("read", r#"{"path":"big.txt","limit":250}"#);
        assert_eq!(r.payload.lines().count(), 250, "{}", r.payload);
        assert!(
            !r.notes.join(" ").contains("at most 200"),
            "{}",
            r.notes.join(" ")
        );
    }

    #[test]
    fn a_minified_file_has_its_line_clipped_and_is_told_to_grep() {
        // Minified javascript is one line of megabytes, and the line cap cannot see it. The
        // clip keeps the head of the line and the note says what does work, because paging by
        // line on this file is a loop: offset 2 is past the end.
        let mut h = harness();
        let line = "x".repeat(10_000);
        h.write_file("min.js", &line);
        let r = h.call("read", r#"{"path":"min.js"}"#);
        assert!(r.is_grounded());
        assert!(
            r.payload.contains("[line clipped at 2000 characters]"),
            "{}",
            r.payload
        );
        let notes = r.notes.join(" ");
        assert!(notes.contains("clipped"), "{notes}");
        assert!(notes.contains("grep"), "{notes}");
    }

    #[test]
    fn a_long_line_read_with_a_limit_is_shown_whole_and_can_be_edited() {
        let mut h = crate::testing::writable_harness();
        let row = format!("| {} |", "cell ".repeat(600));
        h.write_file("t.md", &format!("# t\n{row}\nend\n"));

        let r = h.call("read", r#"{"path":"t.md"}"#);
        assert!(r.payload.contains("[line clipped at 2000 characters]"));
        let r = h.call(
            "edit",
            r#"{"path":"t.md","old_string":"cell  |","new_string":"last |"}"#,
        );
        assert!(
            !r.is_grounded(),
            "an edit of a clipped line: {}",
            r.render()
        );

        let r = h.call("read", r#"{"path":"t.md","offset":2,"limit":1}"#);
        assert!(r.payload.contains(&row), "{}", r.payload);
        assert!(!r.payload.contains("clipped"), "{}", r.payload);
        let r = h.call(
            "edit",
            r#"{"path":"t.md","old_string":"cell  |","new_string":"last |"}"#,
        );
        assert!(r.is_grounded(), "{}", r.render());
        assert!(h.read_file("t.md").contains("cell last |"));
    }

    /// A line over the byte budget is read in pieces with `column_offset`, each piece is marked
    /// shown, and an edit inside a shown piece lands while one past it is refused by name.
    #[test]
    fn a_line_over_the_budget_is_read_in_pieces_and_edited_inside_one() {
        let mut h = crate::testing::writable_harness();
        let row = format!("{}MIDDLE{}TAIL", "a".repeat(40_000), "b".repeat(30_000));
        h.write_file("big.md", &format!("# t\n{row}\nend\n"));

        let r = h.call("read", r#"{"path":"big.md","offset":2,"limit":1}"#);
        let notes = r.notes.join(" ");
        assert!(notes.contains("column_offset=32768"), "{notes}");

        let r = h.call(
            "edit",
            r#"{"path":"big.md","old_string":"aMIDDLEb","new_string":"aCENTREb"}"#,
        );
        assert!(!r.is_grounded(), "{}", r.render());
        assert!(r.render().contains("column_offset=39999"), "{}", r.render());

        let r = h.call(
            "read",
            r#"{"path":"big.md","offset":2,"column_offset":32768}"#,
        );
        assert!(r.is_grounded(), "{}", r.render());
        assert!(r.payload.starts_with("     2| a"), "{}", &r.payload[..40]);
        assert!(r.payload.contains("MIDDLE"));
        let notes = r.notes.join(" ");
        assert!(
            notes.contains("characters 32768-65536 of 70010")
                && notes.contains("column_offset=65536"),
            "{notes}"
        );
        // The tail is past the piece shown.
        let r = h.call(
            "edit",
            r#"{"path":"big.md","old_string":"bTAIL","new_string":"bEND"}"#,
        );
        assert!(!r.is_grounded(), "{}", r.render());
        let r = h.call(
            "read",
            r#"{"path":"big.md","offset":2,"column_offset":69990,"columns":100}"#,
        );
        assert!(r.payload.contains("bbbTAIL"), "{}", r.payload);
        assert!(r.notes.join(" ").contains("to its end"), "{:?}", r.notes);
        let r = h.call(
            "edit",
            r#"{"path":"big.md","old_string":"bTAIL","new_string":"bEND"}"#,
        );
        assert!(r.is_grounded(), "{}", r.render());
        let r = h.call(
            "edit",
            r#"{"path":"big.md","old_string":"aMIDDLEb","new_string":"aCENTREb"}"#,
        );
        assert!(r.is_grounded(), "{}", r.render());
        let after = h.read_file("big.md");
        assert!(after.contains("aCENTREb") && after.contains("bEND"));
    }

    /// The clipped prefix of an unbounded read was displayed, so an edit inside it lands.
    #[test]
    fn an_edit_inside_the_shown_prefix_of_a_clipped_line_lands() {
        let mut h = crate::testing::writable_harness();
        let row = format!("START{}", "x".repeat(5_000));
        h.write_file("m.js", &format!("{row}\n"));
        let r = h.call("read", r#"{"path":"m.js"}"#);
        assert!(r.payload.contains("[line clipped at 2000 characters]"));
        let r = h.call(
            "edit",
            r#"{"path":"m.js","old_string":"STARTxx","new_string":"BEGINxx"}"#,
        );
        assert!(r.is_grounded(), "{}", r.render());
        assert!(h.read_file("m.js").starts_with("BEGINxx"));
    }

    #[test]
    fn a_column_read_past_the_line_or_with_ranges_is_refused_by_name() {
        let mut h = crate::testing::harness();
        h.write_file("s.txt", "short\n");
        let r = h.call("read", r#"{"path":"s.txt","offset":1,"column_offset":9}"#);
        assert!(!r.is_grounded());
        assert!(r.render().contains("has 5 characters"), "{}", r.render());
        let r = h.call(
            "read",
            r#"{"path":"s.txt","column_offset":1,"ranges":[{"offset":1}]}"#,
        );
        assert!(!r.is_grounded());
        let r = h.call(
            "read",
            r#"{"path":"s.txt","offset":1,"column_offset":2,"columns":2}"#,
        );
        assert!(r.payload.contains("     1| or"), "{}", r.payload);
    }

    #[test]
    fn the_byte_cap_stops_a_wide_file_before_its_lines_run_out() {
        // Two hundred short lines fit; two hundred long ones do not. The byte
        // budget is the guarantee, and it names the line to continue from.
        let mut h = harness();
        let body: String = (1..=1000)
            .map(|i| format!("{:0>198}\n", i))
            .collect::<Vec<_>>()
            .join("");
        h.write_file("wide.txt", &body);
        let r = h.call("read", r#"{"path":"wide.txt"}"#);
        let notes = r.notes.join(" ");
        // The byte cap is the reader's own number and stays out of the note: the offset is what
        // carries the model on.
        assert!(notes.contains("stopped before line"), "{notes}");
        assert!(
            !notes.contains("KiB"),
            "no window size in the note: {notes}"
        );
        assert!(
            r.payload.len() < 40 * 1024,
            "the call returned {} bytes",
            r.payload.len()
        );
        let shown = r.payload.lines().count();
        assert!(
            shown < 400 && shown > 100,
            "the byte cap should stop well inside the line cap, got {shown}"
        );
    }

    /// A file past `FILE_CEILING * 16` is refused by its size, before it is opened, and the
    /// refusal names the size and points at `bash`.
    #[test]
    fn a_file_over_the_ceiling_is_refused_by_size_before_it_is_opened() {
        let mut h = harness();
        let big = std::fs::File::create(h.root().join("shard.bin")).expect("sparse file");
        big.set_len(crate::builtins::grep::FILE_CEILING * 16 + 1)
            .expect("set_len");
        drop(big);

        let r = h.call("read", r#"{"path":"shard.bin"}"#);
        assert!(!r.is_grounded(), "{}", r.render());
        match &r.outcome {
            letibot_transcript::ToolOutcome::Failed { reason } => {
                assert!(reason.contains("256 MiB"), "{reason}");
            }
            other => panic!("a file over the ceiling must be refused: {other:?}"),
        }
        assert!(r.payload.contains("head -c"), "{}", r.payload);
    }
}

#[cfg(test)]
mod ranges_tests {
    use crate::testing::writable_harness;

    /// Several windows, one call: three lookups would be three round trips, and at `automode` a
    /// round trip is also a gate decision.
    #[test]
    fn ranges_returns_every_window_in_one_call_with_a_gap_marked() {
        let mut h = writable_harness();
        let r = h
            .call(
                "read",
                r#"{"path":"src/lib.rs","ranges":[{"offset":1,"limit":2},{"offset":5,"limit":2}]}"#,
            )
            .render();
        // Both windows are there, numbered as they are in the file.
        assert!(r.contains("     1|"), "{r}");
        assert!(r.contains("     5|"), "{r}");
        // And the gap between them is marked, so two regions are never read as
        // one continuous stretch.
        assert!(
            r.contains('…'),
            "the gap between windows is not marked:\n{r}"
        );
    }

    /// One range behaves exactly as `offset`/`limit` did: the batch is an addition, not a
    /// change to what a plain read means.
    #[test]
    fn a_single_range_reads_like_offset_and_limit() {
        let mut h = writable_harness();
        let a = h
            .call("read", r#"{"path":"src/lib.rs","offset":2,"limit":3}"#)
            .render();
        let b = h
            .call(
                "read",
                r#"{"path":"src/lib.rs","ranges":[{"offset":2,"limit":3}]}"#,
            )
            .render();
        let body = |s: &str| -> Vec<String> {
            s.lines()
                .filter(|l| l.contains('|'))
                .map(str::to_string)
                .collect()
        };
        assert_eq!(body(&a), body(&b), "a:\n{a}\nb:\n{b}");
    }
}
