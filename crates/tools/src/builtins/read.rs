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
             `offset` (the first line, 1-based) and `limit` (how many lines). Without \
             a limit, at most 200 lines and a few tens of KiB are returned per call, \
             and the result says which offset continues the file; very long lines \
             are clipped. A path that does not exist comes back with the nearest \
             directory's listing rather than an error, and a directory comes back \
             as its listing, so a wrong guess does not need a second call.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "File to read, relative to the session root."},
                    "offset": {"type": "integer", "description": "First line to return, 1-based."},
                    "limit": {"type": "integer", "description": "How many lines to return."},
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

        // The whole file is read before a window of it is shown, so a file that is
        // not text — a model shard, a database — would be read whole first. Sized
        // before it is opened; the ceiling is far above any source file.
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

        // How far this call would reach if bytes were free. An explicit `limit`
        // is the model's own ask and is taken as given; the absence of one is
        // where the default cap applies — a full thousand-line file in one call
        // is the shape that eats a session's context.
        let want_end = match limit {
            Some(l) => (offset - 1 + l).min(total),
            None => (offset - 1 + ctx.limits.max_read_lines).min(total),
        };

        // Rendered with two more bounds than the line count. A line longer than
        // the clip is shown clipped and counted — minified javascript is one
        // line — and the byte budget stops the call even under the line cap,
        // because two hundred lines of two hundred characters is already the
        // budget. At least one line always renders: a cap that returns nothing
        // answers no question.
        // **Several windows of one file, in one call.** `read` took one span, so
        // three lookups were three round trips — and at `automode` a round trip is
        // also a gate decision. That is the cost a model is avoiding when it
        // reaches for `grep -n` piped into `sed -n 'A,Bp'` instead: the shell does
        // the batch in one call. `ranges` is that batch, and it keeps what `sed`
        // throws away — line numbers, the file's real bounds, and a refusal that
        // says what is there instead of printing nothing.
        //
        // The budget is shared across the windows, not per window: the cap exists
        // to bound one REPLY, and a per-window cap would make three ranges cost
        // three times the ceiling.
        let windows: Vec<(usize, usize)> = match args.get("ranges").and_then(|v| v.as_array()) {
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
                    let e = match r.get("limit").and_then(|v| v.as_i64()).filter(|n| *n > 0) {
                        Some(l) => (o - 1 + l as usize).min(total),
                        None => (o - 1 + ctx.limits.max_read_lines).min(total),
                    };
                    (o, e)
                })
                .collect(),
            _ => vec![(offset, want_end)],
        };

        let mut body = String::new();
        let mut truncated = 0usize;
        let mut byte_capped = false;
        let mut end = offset - 1;
        let mut first_window = true;
        for (w_offset, w_end) in &windows {
            let (w_offset, w_end) = (*w_offset, *w_end);
            // A gap between windows is marked, so two regions are never read as
            // one continuous stretch of the file.
            if !first_window && !byte_capped {
                body.push_str("   …\n");
            }
            first_window = false;
            for (i, line) in lines[(w_offset - 1).min(total)..w_end].iter().enumerate() {
                let (shown_line, clipped) = clip(line, ctx.limits.max_read_line_chars);
                let chunk = format!("{:>6}| {shown_line}\n", w_offset + i);
                if !body.is_empty() && body.len() + chunk.len() > ctx.limits.max_read_bytes {
                    byte_capped = true;
                    break;
                }
                if clipped {
                    truncated += 1;
                }
                body.push_str(&chunk);
                end = w_offset + i;
            }
            if byte_capped {
                break;
            }
        }

        // Read-before-write's other half. Recorded here, at the moment the bytes
        // are read, rather than at the moment they are rendered: the digest is
        // about the file, and an `offset`/`limit` window changes what is shown and
        // not what is there. "Whole" means the model saw the whole of it — a
        // capped or clipped read has not, whatever the arguments said.
        // See `crate::files`.
        let whole_file =
            end == total && !byte_capped && truncated == 0 && (total == 0 || end >= offset);
        ctx.files.record(path, &bytes, whole_file);

        if truncated > 0 {
            notes.push(format!(
                "{truncated} line(s) were longer than the {} characters read shows and \
                 are clipped; a file whose lines do not fit does not page by line — use \
                 grep on it for the text you need",
                ctx.limits.max_read_line_chars
            ));
        }
        if byte_capped {
            notes.push(format!(
                "stopped before line {}: one read is capped at {} KiB of text; call read \
                 again with offset={} to continue",
                end + 1,
                ctx.limits.max_read_bytes / 1024,
                end + 1
            ));
        } else if end < total && limit.is_none() {
            // **Only when the cap bound the output.** A caller that passed
            // `limit` chose this window and knows where it ends; telling it "there
            // is more of the file" is news about something it decided. The note
            // exists for the other case — no limit, and the 200-line cap stopped
            // short of the end — where the boundary is OURS and silence would
            // read as "that was the whole file".
            //
            // The operator's example, 2026-09-15: `read 30 25 app.rs` returned 31
            // lines and was told "showing lines 25–54 of 7861; call read again
            // with offset=55 for the rest". It asked for thirty. It got thirty.
            if limit.is_some() {
                notes.push(format!(
                    "showing lines {offset}–{end} of {total}; call read again with \
                     offset={} for the rest",
                    end + 1
                ));
            } else {
                // The SAME sentence as the limit branch above. The cap is already
                // stated once in this tool's schema, which is in the stable prefix
                // of every turn; repeating "read returns at most 200 lines per
                // call" on each capped read spends context restating something the
                // model was told at the top, and the numbers in this line already
                // imply it. Context is the scarce resource for attention as well
                // as for memory — the operator's point, 2026-09-15.
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

/// The first `max` characters of a line, and whether anything was cut.
fn clip(line: &str, max: usize) -> (String, bool) {
    if line.chars().count() <= max {
        return (line.to_string(), false);
    }
    (
        format!(
            "{} … [line clipped at {max} characters]",
            line.chars().take(max).collect::<String>()
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

    /// **Does `read` hand the model the file's bytes, spaces and all?**
    ///
    /// Asked because a fleet seat reported that the same model edits files fine
    /// in opencode and keeps failing here on missing spaces — `if self.x!= y`
    /// where the file has `if self.x != y`. If the model is copying back what it
    /// was shown, the loss is on the way OUT, and this is the cheapest place to
    /// find out.
    /// What a model that asked for 50 lines actually gets told. The operator's
    /// question, 2026-09-15: is the 200-line cap still being recited at it?
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

        // And with no limit, where the cap DID bind, it still says where to
        // continue — without restating the number, which is in the schema.
        let unasked = h.call("read", r#"{"path":"big.txt"}"#);
        let n2 = unasked.notes.join(" ");
        eprintln!("no-limit note: {n2}");
        assert!(!n2.contains("at most"), "{n2}");
        assert!(n2.contains("offset=201"), "{n2}");
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

    #[test]
    fn a_file_past_the_line_cap_is_capped_and_the_note_names_the_offset() {
        // The shape the operator measured: a thousand-line file read whole is
        // twenty thousand tokens of one call. The cap is the default, and the
        // note is the continuation.
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
        // The note says WHERE to continue and how much there is. It does not
        // restate the cap: that is in this tool's schema, which is in the stable
        // prefix of every turn, and repeating it per call is context spent saying
        // something the model was already told.
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
        // Minified javascript is one line of megabytes; the line cap cannot see
        // it. The clip keeps the head of the line, and the note says what does
        // work, because paging by line on this file is a loop: offset 2 is past
        // the end, and the fallback would show the same clipped line again.
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
        assert!(notes.contains("capped at 32 KiB"), "{notes}");
        assert!(notes.contains("stopped before line"), "{notes}");
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

    /// A file past `FILE_CEILING * 16` is refused by its size, before it is opened.
    ///
    /// The defect this guards is the one the ceiling was added for: the whole file
    /// is read before a window of it is shown, so a model shard or a database would
    /// be read whole first. The file is created SPARSE — `set_len` gives it the size
    /// without the bytes — so the test costs a metadata call rather than 256 MiB of
    /// writes, and the refusal has to name the size and point at `bash`.
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

    /// **Several windows, one call.** Three lookups were three round trips, and
    /// at `automode` a round trip is also a gate decision — which is the cost a
    /// model avoids by batching into one `bash`.
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

    /// One range behaves exactly as `offset`/`limit` always did — the batch is
    /// an addition, not a change to what a plain read means.
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
