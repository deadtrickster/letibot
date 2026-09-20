//! `edit` — replace exact text in a file, and answer a miss with what is
//! actually there.
//!
//! The first tool in this harness that can change the operator's tree, so the
//! order of its checks is the design and is worth reading as one:
//!
//! ```text
//!   arguments (clause 2, in the runtime)
//!     → the GATE (clause 4, in the runtime) — refuses with no adjudicator
//!       → the file exists?        no  → the nearest listing, and `write` named
//!         → the backend writable? no  → which of the two gates stopped this
//!           → exactly one match?  no  → what IS there (§8.1 clause 1)
//!             → apply, atomically
//! ```
//!
//! Every `no` on that ladder produces **more** output than the `yes` does, and
//! every one of those extra bytes is something the model can act on without
//! another call. That is the module-level rule the read-only built-ins already
//! keep; it costs more here, because the alternative to a good miss report is a
//! model guessing at a file it is about to write to.
//!
//! # Two rungs that used to be on that ladder, and why they are not
//!
//! **read-before-write** — refuse an edit to a file this session had not read,
//! *"so that a write can never be aimed at a file the model is remembering
//! rather than reading"* — and **staleness**, refuse when the file's bytes had
//! changed since this session was shown them. Both went on 2026-09-17, for
//! reasons the operator put better than the code had:
//!
//! *"if before region matches what is in file it just looks dumb"*. It did: the
//! read rung ran BEFORE the match, and the match already refuses a miss and an
//! ambiguity — so every edit that lands has proved `old_string` occurs exactly
//! once in the file's current bytes, which is not something a model does from a
//! faulty memory. The rung was asking for evidence that had just been produced.
//!
//! *"if a file was never truly read what is the value"*. None: over the paste
//! ceiling the refusal showed an EXCERPT, recorded the file as seen, and let the
//! retry through — so on exactly the large files where being wrong about
//! surroundings is likeliest, it pasted a fragment and stood aside. It was
//! bookkeeping that cleared its own flag.
//!
//! The staleness rung was the better of the two — it stated a fact rather than
//! guessing — but the fact it stated was too coarse to act on: the digest is of
//! the WHOLE file, so a formatter touching line 900 refused an edit at line 12
//! whose target was verified current. Making it mean what it says needs the set
//! of lines the model was actually shown, across `read` ranges and grep hits,
//! which is a real piece of work and is on the board rather than half-done here.
//!
//! What is left is one check that is true by construction: the text being
//! replaced is in the file, exactly once, right now.

use serde_json::Value;

use crate::backend::BackendError;
use crate::edit::{Candidate, FileEdit, FileText, changed_span, display_lines, occurrences, probe};
use crate::runtime::{Invocation, InvokeCtx, Tool};
use crate::schema::{Access, ToolSchema};

use super::{near_names, nearest_listing, render_listing};

/// How many lines of the file are shown either side of a reported region.
const CONTEXT: usize = 3;

/// How many occurrences an ambiguity report lists before it says how many more.
///
/// grok-build's message on the same case is *"The string to replace was found
/// multiple times in the file. Use replace_all to replace all occurrences, or
/// include more context to only edit one occurrence."* and opencode's is *"Found
/// multiple matches for oldString. Provide more surrounding context to make the
/// match unique."* Neither says **where**, so the model's only move is to guess a
/// longer string. Clause 1 says the answer is the line numbers, so this is how
/// many of them fit before the report stops being readable.
const MAX_OCCURRENCES_SHOWN: usize = 10;

pub struct Edit;

impl Tool for Edit {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "edit",
            // Clause 6, and one rule grok-build keeps that is worth copying: it
            // strips a sentence from the served description when the behaviour it
            // describes is switched off, *"so the model is never told a rule that
            // isn't running"*. opencode's `edit.txt` is the counter-example — it
            // promises a read-before-write error and two error strings, none of
            // which exist in its code. Every sentence here is a behaviour in this
            // file.
            "Replace exact text in a file. Give `path`, `old_string` (the text to \
             replace, copied exactly — whitespace, indentation and all) and \
             `new_string`. `old_string` must occur exactly once unless `replace_all` is true; if it \
             occurs more than once the reply lists every line it occurs on. If it is \
             not found, the reply says what is at the nearest matching place instead, \
             and whether the difference is whitespace, indentation or case.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "File to change, relative to the session root."},
                    "old_string": {"type": "string", "description": "The exact text to replace."},
                    "new_string": {"type": "string", "description": "What to put in its place. Empty deletes the text."},
                    "replace_all": {"type": "boolean", "description": "Replace every occurrence instead of requiring exactly one."},
                    "edits": {
                        "type": "array",
                        "description":
                            "Several changes to ONE file, applied in order and all-or-nothing: \
                             if any step does not resolve, nothing is written. Use this instead \
                             of one `edit` per change, and instead of a shell heredoc. Each item \
                             is either a replacement — `old_string` with `new_string`, and \
                             optional `replace_all` — or an insertion: `insert_before` or \
                             `insert_after` naming an anchor that occurs exactly once, with \
                             `new_string` as the text to put there. An insertion does not \
                             repeat the anchor.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "old_string": {"type": "string"},
                                "insert_before": {"type": "string"},
                                "insert_after": {"type": "string"},
                                "new_string": {"type": "string"},
                                "replace_all": {"type": "boolean"}
                            },
                            "required": ["new_string"]
                        }
                    }
                },
                "required": ["path"]
            }),
            // Clause 4: declared, once, in the schema. `Access::Write` is what
            // §11.3's policy keys on and it does not depend on the arguments — an
            // `edit` whose `new_string` happens to equal `old_string` is still a
            // write tool, because the class must be knowable before the arguments
            // are.
            Access::Write,
        )
    }

    fn invoke(&self, ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        let Some(path) = args.get("path").and_then(|v| v.as_str()) else {
            return Invocation::failed(
                "edit needs a path",
                "call `edit` again with `path`, `old_string` and `new_string`.",
            );
        };
        // **Several changes to one file, in one call.** The reason this exists is
        // measured rather than assumed: a model that wants four non-adjacent
        // changes to one file has, until now, had to make four calls — four round
        // trips and four gate decisions — or reach for a shell heredoc, which is
        // what both this session's model and the operator's own agent kept doing.
        // A tool that cannot say what somebody means is a tool they route around.
        if args.get("edits").is_some() {
            return batch(ctx, path, args);
        }
        let (Some(old), Some(new)) = (
            args.get("old_string").and_then(|v| v.as_str()),
            args.get("new_string").and_then(|v| v.as_str()),
        ) else {
            return missing_strings(ctx, path, args);
        };
        let replace_all = args
            .get("replace_all")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if old.is_empty() {
            // grok-build's default here is to overwrite the whole file, which is a
            // very large effect for a very small typo. opencode's rewrite refuses.
            // So does this, and it names the tool that does mean that.
            return Invocation::failed(
                "`old_string` is empty",
                "an empty `old_string` does not mean anything for `edit`: there is no \
                 text to replace. To replace a file's whole contents, or to create one, \
                 call `write` with `path` and `content`.",
            );
        }

        // The file, or the miss that answers itself.
        let bytes = match ctx.backend.read(path) {
            Ok(b) => b,
            Err(BackendError::NotFound(_)) => return missing_file(ctx, path),
            Err(BackendError::IsADirectory(_)) => {
                let entries = ctx.backend.list(path).unwrap_or_default();
                return Invocation::failed(
                    format!("`{path}` is a directory"),
                    render_listing(path, &entries, 60),
                )
                .with_note("its listing is below; call `edit` again on one of these files.");
            }
            Err(e @ BackendError::Outside(_)) => {
                return Invocation::failed(
                    e.to_string(),
                    "this session can only change files inside its own root. Give a path \
                     relative to it.",
                );
            }
            Err(e) => return Invocation::failed(e.to_string(), String::new()),
        };

        let file = FileText::of(&bytes);
        if file.lossy {
            // Writing back a lossily-decoded file replaces its undecodable bytes
            // with U+FFFD **permanently**, which is a change nobody asked for
            // hiding inside a change somebody did. grok-build does exactly this
            // and does not mention it.
            return Invocation::failed(
                format!("`{path}` is not valid UTF-8"),
                "editing it as text would replace its undecodable bytes with U+FFFD \
                 everywhere in the file, which is a change you did not ask for. Nothing \
                 was written.",
            );
        }
        let hits = occurrences(&file.lf, old);
        match (hits.len(), replace_all) {
            (0, _) => no_match(&file, path, old),
            (n, false) if n > 1 => ambiguous(&file, path, old, &hits),
            _ => {
                if old == new {
                    return Invocation::failed(
                        "`old_string` and `new_string` are identical",
                        format!(
                            "`{path}` already has exactly that text at line {}. Nothing was \
                             written.",
                            file.line_of(hits[0])
                        ),
                    );
                }
                apply(ctx, path, &file, old, new, &hits)
            }
        }
    }
}

/// One step of a batch: a replacement, or an insertion at an anchor.
enum Step {
    Replace {
        old: String,
        new: String,
        all: bool,
    },
    /// `before` is `true` for `insert_before`. The anchor is kept; only the new
    /// text is added, so nothing has to repeat it.
    Insert {
        anchor: String,
        new: String,
        before: bool,
    },
}

/// **Several edits to one file, applied in order, all or nothing.**
///
/// Each step is resolved against the text as the steps before it left it —
/// which is what makes a sequence expressible at all, and is why the failure
/// message names the step's index rather than a line number in the original.
///
/// Nothing is written until every step has resolved. A batch that fails leaves
/// the file exactly as it was, because the alternative — half a change, on disk,
/// reported as an error — is the state no caller can recover from.
fn batch(ctx: &mut InvokeCtx<'_>, path: &str, args: &Value) -> Invocation {
    let Some(list) = args.get("edits").and_then(|v| v.as_array()) else {
        return Invocation::failed(
            "`edits` is not an array",
            "`edits` is a list of steps; each is `old_string`+`new_string`, or \
             `insert_before`/`insert_after`+`new_string`.",
        );
    };
    if list.is_empty() {
        return Invocation::failed(
            "`edits` is empty",
            "give at least one step, or call `edit` with `old_string` and `new_string`.",
        );
    }
    let mut steps: Vec<Step> = Vec::with_capacity(list.len());
    for (i, e) in list.iter().enumerate() {
        let n = i + 1;
        let Some(new) = e.get("new_string").and_then(|v| v.as_str()) else {
            return Invocation::failed(
                format!("step {n} has no `new_string`"),
                "every step needs `new_string`, even an insertion. Nothing was written.",
            );
        };
        let before = e.get("insert_before").and_then(|v| v.as_str());
        let after = e.get("insert_after").and_then(|v| v.as_str());
        let old = e.get("old_string").and_then(|v| v.as_str());
        steps.push(match (old, before, after) {
            (Some(o), None, None) if !o.is_empty() => Step::Replace {
                old: o.to_string(),
                new: new.to_string(),
                all: e
                    .get("replace_all")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            },
            (None, Some(a), None) if !a.is_empty() => Step::Insert {
                anchor: a.to_string(),
                new: new.to_string(),
                before: true,
            },
            (None, None, Some(a)) if !a.is_empty() => Step::Insert {
                anchor: a.to_string(),
                new: new.to_string(),
                before: false,
            },
            _ => {
                return Invocation::failed(
                    format!("step {n} does not say what to change"),
                    "a step is EITHER `old_string` (non-empty) with `new_string`, OR one \
                     of `insert_before`/`insert_after` (non-empty) with `new_string`. \
                     Naming more than one of them, or none, is ambiguous and nothing \
                     was written.",
                );
            }
        });
    }

    let bytes = match ctx.backend.read(path) {
        Ok(b) => b,
        Err(BackendError::NotFound(_)) => return missing_file(ctx, path),
        Err(e) => return Invocation::failed(e.to_string(), String::new()),
    };
    let file = FileText::of(&bytes);
    if file.lossy {
        return Invocation::failed(
            format!("`{path}` is not valid UTF-8"),
            "editing it as text would replace its undecodable bytes with U+FFFD. \
             Nothing was written.",
        );
    }

    let mut text = file.lf.clone();
    let mut replacements = 0usize;
    let mut first_at: Option<usize> = None;
    for (i, step) in steps.iter().enumerate() {
        let n = i + 1;
        let (needle, all) = match step {
            Step::Replace { old, all, .. } => (old.as_str(), *all),
            Step::Insert { anchor, .. } => (anchor.as_str(), false),
        };
        let hits = occurrences(&text, needle);
        match hits.len() {
            0 => {
                return Invocation::failed(
                    format!("step {n} of {} found no match", steps.len()),
                    format!(
                        "nothing in `{path}` matches that text, as the {} step(s) before it \
                         left the file. Nothing was written.\n\nlooked for:\n{}",
                        i,
                        excerpt(needle)
                    ),
                );
            }
            k if k > 1 && !all => {
                return Invocation::failed(
                    format!("step {n} of {} matches {k} places", steps.len()),
                    format!(
                        "a step must name one place unless `replace_all` is set. Nothing \
                         was written.\n\nlooked for:\n{}",
                        excerpt(needle)
                    ),
                );
            }
            _ => {}
        }
        first_at.get_or_insert(hits[0]);
        let mut out = String::with_capacity(text.len() + 64);
        let mut from = 0usize;
        for &at in &hits {
            out.push_str(&text[from..at]);
            match step {
                Step::Replace { new, .. } => out.push_str(new),
                Step::Insert {
                    anchor,
                    new,
                    before,
                } => {
                    if *before {
                        out.push_str(new);
                        out.push_str(anchor);
                    } else {
                        out.push_str(anchor);
                        out.push_str(new);
                    }
                }
            }
            from = at + needle.len();
            replacements += 1;
        }
        out.push_str(&text[from..]);
        text = out;
    }

    if text == file.lf {
        return Invocation::failed(
            "the batch changed nothing",
            format!(
                "`{path}` is already exactly what those {} step(s) would make it. \
                     Nothing was written.",
                steps.len()
            ),
        );
    }
    write_out(
        ctx,
        path,
        &file,
        text,
        replacements,
        first_at.unwrap_or(0),
        None,
    )
}

/// A short, bounded quote of what a step looked for, for a failure message.
fn excerpt(needle: &str) -> String {
    let one: String = needle.lines().take(3).collect::<Vec<_>>().join("\n");
    if one.chars().count() > 240 {
        format!("{}…", one.chars().take(240).collect::<String>())
    } else {
        one
    }
}

/// The write, and everything that has to be true at the moment it happens.
fn apply(
    ctx: &mut InvokeCtx<'_>,
    path: &str,
    file: &FileText,
    old: &str,
    new: &str,
    hits: &[usize],
) -> Invocation {
    // Built whole, in memory, before anything is opened. A tool that wrote as it
    // matched would have a failure mode — a match that throws halfway — that no
    // atomic backend could rescue, because the damage would already be in the
    // bytes handed to it.
    let mut after = String::with_capacity(file.lf.len() + new.len());
    let mut from = 0usize;
    for &at in hits {
        after.push_str(&file.lf[from..at]);
        after.push_str(new);
        from = at + old.len();
    }
    after.push_str(&file.lf[from..]);

    let extra = if hits.len() > 1 {
        let rest: Vec<String> = hits[1..]
            .iter()
            .map(|h| file.line_of(*h).to_string())
            .collect();
        Some(format!(
            "\nthe other replacement(s) were at line(s) {} of the file as it was\n",
            rest.join(", ")
        ))
    } else {
        None
    };
    write_out(ctx, path, file, after, hits.len(), hits[0], extra)
}

/// **The write, the record and the edit card** — the one place any of the three
/// happen, so a second caller cannot drift from the first.
///
/// `first_at` is a byte offset into the file AS IT WAS, used only to decide
/// which lines to show; `replacements` is how many places the caller changed.
fn write_out(
    ctx: &mut InvokeCtx<'_>,
    path: &str,
    file: &FileText,
    after: String,
    replacements: usize,
    first_at: usize,
    extra: Option<String>,
) -> Invocation {
    let out = file.restore(&after);
    if let Err(e) = ctx.backend.write(path, out.as_bytes()) {
        return write_failed(ctx, path, e);
    }

    // The model may now edit again without re-reading: it has been told exactly
    // what the file became. Recording the *new* content is what makes that true
    // rather than a courtesy.
    ctx.files.record(path, out.as_bytes(), true);

    let span = changed_span(&file.lf, &after);
    let lines: Vec<&str> = display_lines(&after);
    let shown_from = file.line_of(first_at).saturating_sub(CONTEXT).max(1);
    let shown_to = (span.last_after + CONTEXT).min(lines.len());

    let mut body = format!(
        "{path}: {replacements} replacement(s). {}\n\n",
        span.describe()
    );
    for i in shown_from..=shown_to {
        body.push_str(&format!("{i:>6}| {}\n", lines[i - 1]));
    }
    if let Some(x) = extra {
        body.push_str(&x);
    }

    let mut inv = Invocation::ok(body);
    if file.mixed {
        inv = inv.with_note(format!(
            "`{path}` has mixed line endings; the edited text was written with \\n and \
             the rest of the file was left as it was"
        ));
    }
    inv.edit = Some(FileEdit {
        path: path.to_string(),
        before: file.lf.clone(),
        after: after.clone(),
        created: false,
        before_digest: crate::spill::content_hash(file.lf.as_bytes()),
        after_digest: crate::spill::content_hash(after.as_bytes()),
        replacements,
        changed: span,
    });
    inv
}

/// Clause 1's hardest case: the text is not there, so say what is.
fn no_match(file: &FileText, path: &str, old: &str) -> Invocation {
    let candidates = probe(file, old);
    let mut body = String::new();

    if candidates.is_empty() {
        // Nothing even nearly matched. The next most useful fact is where the
        // first line of the search *does* occur, which is grok-build's
        // `build_nearest_match_hint` — its longest whitespace-delimited token
        // against every line of the file.
        let first = old.split('\n').next().unwrap_or(old);
        match anchor_lines(file, first) {
            hits if !hits.is_empty() => {
                body.push_str(&format!(
                    "nothing in `{path}` matches that text, even allowing for \
                     whitespace, indentation or case.\nthe most distinctive part of \
                     your first line does occur, here:\n"
                ));
                for (line, text) in hits.iter().take(MAX_OCCURRENCES_SHOWN) {
                    body.push_str(&format!("{line:>6}| {text}\n"));
                }
                if hits.len() > MAX_OCCURRENCES_SHOWN {
                    body.push_str(&format!(
                        "  … and {} more\n",
                        hits.len() - MAX_OCCURRENCES_SHOWN
                    ));
                }
            }
            _ => {
                body.push_str(&format!(
                    "nothing in `{path}` matches that text, and no part of your first \
                     line occurs anywhere in it either. `{path}` is {} line(s) long; its \
                     first lines are:\n",
                    display_lines(&file.lf).len()
                ));
                for (i, l) in file.lf.lines().take(20).enumerate() {
                    body.push_str(&format!("{:>6}| {l}\n", i + 1));
                }
            }
        }
        body.push_str(
            "\nnothing was written. Call `read` on this path and copy the text you \
             want to change out of what it returns.\n",
        );
        return Invocation::failed(format!("`old_string` is not in `{path}`"), body);
    }

    let relax = candidates[0].relax;
    body.push_str(&format!(
        "`old_string` is not in `{path}` byte for byte. {} place(s) match under a \
         relaxed comparison: {}. To fix it, {}.\n\n",
        candidates.len(),
        relax.headline(),
        relax.advice()
    ));
    for c in candidates.iter().take(MAX_OCCURRENCES_SHOWN) {
        body.push_str(&region(file, c));
        body.push('\n');
    }
    if candidates.len() > MAX_OCCURRENCES_SHOWN {
        body.push_str(&format!(
            "  … and {} more place(s) that match the same way\n",
            candidates.len() - MAX_OCCURRENCES_SHOWN
        ));
    }
    body.push_str(
        "\nnothing was written: a match that is only close is not the text you asked \
         for, and applying it would change bytes you did not name. Copy the text above \
         into `old_string` and call `edit` again.\n",
    );
    Invocation::failed(
        format!(
            "`old_string` is not in `{path}` exactly; at {} place(s), {}",
            candidates.len(),
            relax.differs()
        ),
        body,
    )
}

/// One reported region, with its line numbers and its context.
fn region(file: &FileText, c: &Candidate) -> String {
    let lines: Vec<&str> = display_lines(&file.lf);
    let from = c.first_line.saturating_sub(CONTEXT).max(1);
    let to = (c.last_line + CONTEXT).min(lines.len());
    let mut out = format!(
        "lines {}–{} of {}:\n",
        c.first_line,
        c.last_line,
        display_lines(&file.lf).len()
    );
    for i in from..=to {
        let mark = if i >= c.first_line && i <= c.last_line {
            ">"
        } else {
            " "
        };
        out.push_str(&format!("{mark}{i:>5}| {}\n", lines[i - 1]));
    }
    // The literal text, unnumbered and unmarked, because that is the thing to
    // copy. A model asked to reconstruct a string out of a numbered listing gets
    // the leading spaces wrong, which is how it arrived here.
    out.push_str("the exact text at that place, to copy into `old_string`:\n---8<---\n");
    out.push_str(c.text(file));
    out.push_str("\n--->8---\n");
    out
}

/// More than one occurrence, which is the common real failure.
fn ambiguous(file: &FileText, path: &str, old: &str, hits: &[usize]) -> Invocation {
    let lines: Vec<&str> = display_lines(&file.lf);
    let mut body = format!(
        "`old_string` occurs {} times in `{path}`, so `edit` cannot tell which one you \
         mean. Nothing was written.\n\n",
        hits.len()
    );
    for &at in hits.iter().take(MAX_OCCURRENCES_SHOWN) {
        let line = file.line_of(at);
        let last = file.line_of(at + old.len().saturating_sub(1));
        body.push_str(&format!("at line {line}:\n"));
        let from = line.saturating_sub(2).max(1);
        let to = (last + 2).min(lines.len());
        for i in from..=to {
            let mark = if i >= line && i <= last { ">" } else { " " };
            body.push_str(&format!("{mark}{i:>5}| {}\n", lines[i - 1]));
        }
        body.push('\n');
    }
    if hits.len() > MAX_OCCURRENCES_SHOWN {
        body.push_str(&format!(
            "… and {} more\n\n",
            hits.len() - MAX_OCCURRENCES_SHOWN
        ));
    }
    body.push_str(
        "Either extend `old_string` with enough of the lines above it or below it to \
         pick out one of these — the surrounding lines shown above are the difference \
         between them — or call `edit` again with `replace_all` set to true to change \
         every one.\n",
    );
    Invocation::failed(
        format!("`old_string` occurs {} times in `{path}`", hits.len()),
        body,
    )
}

/// A path that does not exist. The same shape `read` uses, plus the one extra
/// fact this tool has: `write` is the thing that creates a file.
fn missing_file(ctx: &mut InvokeCtx<'_>, path: &str) -> Invocation {
    let (dir, entries) = nearest_listing(ctx.backend, path);
    let near = near_names(path, &entries, 5);
    let mut body = render_listing(&dir, &entries, 40);
    if !near.is_empty() {
        body.push_str(&format!("\nclosest names here: {}\n", near.join(", ")));
    }
    body.push_str(
        "\n`edit` changes a file that exists. To create one, call `write` with `path` \
         and `content`.\n",
    );
    Invocation::failed(format!("no file at `{path}`"), body).with_note(format!(
        "`{path}` does not exist; the listing of `{dir}` is below. Nothing was written."
    ))
}

/// A write the backend refused. Says **which** gate stopped it, because a
/// refusal that does not is the refusal that costs an hour.
fn write_failed(ctx: &mut InvokeCtx<'_>, path: &str, e: BackendError) -> Invocation {
    let extra = if !ctx.backend.is_writable() {
        format!(
            "\n\nThis session's execution backend is read-only ({}). That is a second, \
             independent gate below §11's adjudication: a write reaching the disk needs \
             both an adjudicator that admitted it and a backend that was opened to be \
             written to. The adjudicator admitted this call; the backend is what \
             refused. Nothing on disk changed.",
            ctx.backend.describe()
        )
    } else {
        "\n\nNothing on disk changed: the write is a temporary file renamed over the \
         target, so a failure leaves the original untouched."
            .to_string()
    };
    Invocation::failed(format!("could not write `{path}`: {e}"), extra)
}

/// `old_string` or `new_string` missing. Salvage cannot invent either, so this
/// says what was received and what the file is, which is the most useful pair.
fn missing_strings(ctx: &mut InvokeCtx<'_>, path: &str, args: &Value) -> Invocation {
    let have: Vec<&str> = args
        .as_object()
        .map(|o| o.keys().map(|k| k.as_str()).collect())
        .unwrap_or_default();
    let exists = ctx.backend.stat(path).is_some();
    Invocation::failed(
        "edit needs both `old_string` and `new_string`",
        format!(
            "the call carried {}. `edit` replaces `old_string` with `new_string` in \
             `path`; neither can be guessed, so nothing was written. `{path}` {}. To \
             replace the whole file, call `write` with `path` and `content`.",
            if have.is_empty() {
                "no arguments".to_string()
            } else {
                format!("only: {}", have.join(", "))
            },
            if exists { "exists" } else { "does not exist" }
        ),
    )
}

/// Lines containing the most distinctive token of `first`.
///
/// grok-build's `build_nearest_match_hint` takes *"the longest
/// whitespace-delimited token of `old_string`'s first line"* and reports the
/// first line containing it. Same idea, and every line rather than the first,
/// because "it occurs eleven times" is itself the answer sometimes.
///
/// With one refinement this model needs (R8): a missing space fuses two tokens
/// into one that occurs nowhere — `if!text.is_empty()` — and no other whole
/// token may remain to fall back on. So whole tokens are tried longest-first,
/// and only if none occurs anywhere is each split on non-alphanumeric
/// boundaries and the pieces tried the same way: `text` and `is_empty` land
/// on the line the model meant.
fn anchor_lines(file: &FileText, first: &str) -> Vec<(usize, String)> {
    let mut tokens: Vec<&str> = first.split_whitespace().filter(|t| t.len() >= 3).collect();
    tokens.sort_by_key(|t| std::cmp::Reverse(t.len()));
    tokens.dedup();

    for token in &tokens {
        let hits = lines_containing(file, token);
        if !hits.is_empty() {
            return hits;
        }
    }

    let mut pieces: Vec<&str> = tokens
        .iter()
        .flat_map(|t| t.split(|c: char| !c.is_alphanumeric()))
        .filter(|p| p.len() >= 4)
        .collect();
    pieces.sort_by_key(|p| std::cmp::Reverse(p.len()));
    pieces.dedup();

    for piece in &pieces {
        let hits = lines_containing(file, piece);
        if !hits.is_empty() {
            return hits;
        }
    }

    Vec::new()
}

fn lines_containing(file: &FileText, token: &str) -> Vec<(usize, String)> {
    file.lf
        .lines()
        .enumerate()
        .filter(|(_, l)| l.contains(token))
        .map(|(i, l)| (i + 1, l.chars().take(200).collect::<String>()))
        .collect()
}

#[cfg(test)]
mod tests {

    /// A refusal must not cost a context window. Measured 2026-09-15: a stale
    /// `edit` on a 345 KB file pasted it back in full — 123,648 tokens in one
    /// tool result, to say "the file moved, try again".
    ///
    /// Both halves matter. The refusal still has to be ACTIONABLE (the retry must
    /// work, so the current digest is recorded), and it has to be BOUNDED (an
    /// excerpt, not the file). A fix that only did the second would trade a huge
    /// refusal for an infinite loop of small ones.
    /// The fast path is untouched: an ordinary file still comes back whole, which
    /// is what makes the common retry one call instead of two.
    use crate::testing::{deny_all, writable_harness, writable_harness_with_gate};

    #[test]
    fn an_edit_replaces_exactly_and_hands_the_head_both_sides() {
        let mut h = writable_harness();
        h.call("read", r#"{"path":"src/lib.rs"}"#);
        let r = h.call(
            "edit",
            r#"{"path":"src/lib.rs","old_string":"pub fn parse_args","new_string":"pub fn parse_argv"}"#,
        );
        assert!(r.is_grounded(), "{}", r.render());
        let e = r.edit.as_ref().expect("the head is handed both sides");
        assert!(e.before.contains("parse_args"));
        assert!(e.after.contains("parse_argv"));
        assert!(!e.after.contains("parse_args("));
        assert_eq!(e.replacements, 1);
        assert_eq!(h.read_file("src/lib.rs"), e.after);
    }

    #[test]
    fn an_edit_lands_on_its_target_whatever_this_session_has_read() {
        // Read-before-write and the staleness refusal are both gone. What is
        // left is the check that was always doing the work: `old_string` occurs
        // exactly once in the file's CURRENT bytes. See the module note for the
        // two findings that retired the other two.
        let mut h = writable_harness();
        let r = h.call(
            "edit",
            r#"{"path":"src/lib.rs","old_string":"pub fn parse_args","new_string":"pub fn parse_argv"}"#,
        );
        assert!(r.is_grounded(), "{}", r.render());
        assert!(
            h.read_file("src/lib.rs").contains("parse_argv"),
            "it landed"
        );

        // And a file somebody else rewrote under this session still edits, so
        // long as the target is there exactly once — the bytes it lands on are
        // the bytes that were matched.
        h.call("read", r#"{"path":"README.md"}"#);
        h.write_file("README.md", "somebody else was here\n");
        let r = h.call(
            "edit",
            r#"{"path":"README.md","old_string":"somebody","new_string":"nobody"}"#,
        );
        assert!(r.is_grounded(), "{}", r.render());
        assert_eq!(h.read_file("README.md"), "nobody else was here\n");
    }

    /// An unread file whose target is NOT unambiguous is refused by the match,
    /// which is the check that was always doing the work — and it says more than
    /// the read rule did: near misses, anchor lines, line numbers.
    #[test]
    fn an_unread_file_with_a_bad_target_is_refused_by_the_match() {
        let mut h = writable_harness();
        let r = h.call(
            "edit",
            r#"{"path":"src/lib.rs","old_string":"fn nothing_like_this_exists","new_string":"x"}"#,
        );
        assert!(!r.is_grounded());
        let out = r.render();
        assert!(out.contains("matches that text"), "{out}");
        assert!(
            h.read_file("src/lib.rs").contains("parse_args"),
            "untouched"
        );
    }

    #[test]
    fn two_occurrences_are_refused_with_every_line_number() {
        let mut h = writable_harness();
        h.write_file("dup.rs", "let x = 1;\nlet y = 2;\nlet x = 1;\n");
        h.call("read", r#"{"path":"dup.rs"}"#);
        let r = h.call(
            "edit",
            r#"{"path":"dup.rs","old_string":"let x = 1;","new_string":"let x = 3;"}"#,
        );
        let out = r.render();
        assert!(out.contains("occurs 2 times"), "{out}");
        assert!(
            out.contains("at line 1:") && out.contains("at line 3:"),
            "{out}"
        );
        assert!(out.contains("replace_all"), "{out}");
        assert_eq!(
            h.read_file("dup.rs"),
            "let x = 1;\nlet y = 2;\nlet x = 1;\n",
            "an ambiguous edit writes nothing"
        );
    }

    #[test]
    fn replace_all_changes_every_one_and_says_where() {
        let mut h = writable_harness();
        h.write_file("dup.rs", "let x = 1;\nlet y = 2;\nlet x = 1;\n");
        h.call("read", r#"{"path":"dup.rs"}"#);
        let r = h.call(
            "edit",
            r#"{"path":"dup.rs","old_string":"let x = 1;","new_string":"let x = 3;","replace_all":true}"#,
        );
        assert!(r.is_grounded(), "{}", r.render());
        assert_eq!(
            h.read_file("dup.rs"),
            "let x = 3;\nlet y = 2;\nlet x = 3;\n"
        );
        assert_eq!(r.edit.as_ref().unwrap().replacements, 2);
    }

    #[test]
    fn an_indentation_miss_reports_the_exact_text_and_writes_nothing() {
        // A tab against spaces, because that is the case exact matching genuinely
        // cannot see through. Four spaces against eight *does* match — the
        // eight-space line contains the four-space string — and that is not a
        // fuzzy match, it is a substring, which is what `edit` was asked for.
        let mut h = writable_harness();
        h.write_file("a.py", "def f():\n\treturn 1\n");
        h.call("read", r#"{"path":"a.py"}"#);
        let r = h.call(
            "edit",
            r#"{"path":"a.py","old_string":"    return 1","new_string":"    return 2"}"#,
        );
        let out = r.render();
        assert!(out.contains("indentation"), "{out}");
        assert!(out.contains("---8<---\n\treturn 1\n--->8---"), "{out}");
        assert_eq!(h.read_file("a.py"), "def f():\n\treturn 1\n");
    }

    #[test]
    fn an_exact_substring_of_a_line_is_a_match_and_not_a_relaxation() {
        // Stated as a test because it is the property that made the case above
        // need a tab: `edit` replaces a *string*, not a line, and a model that
        // sent a differently-indented line has still sent something that occurs.
        let mut h = writable_harness();
        h.write_file("a.py", "def f():\n        return 1\n");
        h.call("read", r#"{"path":"a.py"}"#);
        let r = h.call(
            "edit",
            r#"{"path":"a.py","old_string":"    return 1","new_string":"    return 2"}"#,
        );
        assert!(r.is_grounded(), "{}", r.render());
        assert_eq!(h.read_file("a.py"), "def f():\n        return 2\n");
    }

    #[test]
    fn a_miss_with_no_near_match_still_says_where_the_words_are() {
        let mut h = writable_harness();
        h.call("read", r#"{"path":"src/lib.rs"}"#);
        let r = h.call(
            "edit",
            r#"{"path":"src/lib.rs","old_string":"let ledger = TokenLedger::build();","new_string":"x"}"#,
        );
        let out = r.render();
        assert!(out.contains("TokenLedger"), "{out}");
        assert!(out.contains("nothing was written"), "{out}");
    }

    #[test]
    fn a_missing_space_merges_two_tokens_and_a_shorter_anchor_still_lands() {
        // R8, verbatim from the live session: the model sent `if!text.is_empty() {`
        // where the file holds `if !text.is_empty() {`. probe() cannot re-insert
        // the missing character and the merged token occurs nowhere, so the only
        // way back to the right line is a shorter anchor carved out of that token.
        let mut h = writable_harness();
        h.write_file(
            "src/gate.rs",
            "pub fn gate(text: &str) -> bool {\n    if !text.is_empty() {\n        return true;\n    }\n    false\n}\n",
        );
        h.call("read", r#"{"path":"src/gate.rs"}"#);
        let r = h.call(
            "edit",
            r#"{"path":"src/gate.rs","old_string":"    if!text.is_empty() {\n        return true;\n    }","new_string":"x"}"#,
        );
        let out = r.render();
        assert!(out.contains("does occur"), "{out}");
        assert!(out.contains("if !text.is_empty() {"), "{out}");
        assert!(out.contains("     2|"), "{out}");
        assert!(out.contains("nothing was written"), "{out}");
    }

    #[test]
    fn a_gated_edit_inside_the_workspace_never_says_host_other() {
        // R9's done-when, on the rendered payload rather than the class: a refusal
        // must not repeat a region the deciding classifier never used. Seen live as
        // "reading: ask — intents [write_file] over [host_other]" for a path the
        // same gate had just classed as inside the project.
        let mut h = writable_harness_with_gate(Some(deny_all()));
        h.call("read", r#"{"path":"src/lib.rs"}"#);
        let r = h.call(
            "edit",
            r#"{"path":"src/lib.rs","old_string":"pub fn parse_args","new_string":"pub fn parse_argv"}"#,
        );
        let out = r.render();
        assert!(!out.contains("host_other"), "{out}");
        assert!(out.contains("intents [write_file]"), "{out}");
        assert!(out.contains("Nothing was executed"), "{out}");
    }

    #[test]
    fn an_edit_to_a_missing_file_names_write_rather_than_creating_it() {
        let mut h = writable_harness();
        let r = h.call(
            "edit",
            r#"{"path":"src/libb.rs","old_string":"a","new_string":"b"}"#,
        );
        let out = r.render();
        assert!(out.contains("lib.rs"), "{out}");
        assert!(out.contains("call `write`"), "{out}");
    }

    #[test]
    fn with_no_adjudicator_nothing_is_written_and_nothing_claims_a_decision() {
        // The gate, not the tool. `writable_harness_with_gate(None)` is a session
        // whose backend *can* write and whose gate has nobody behind it.
        let mut h = writable_harness_with_gate(None);
        h.call("read", r#"{"path":"README.md"}"#);
        let r = h.call(
            "edit",
            r#"{"path":"README.md","old_string":"letibot","new_string":"x"}"#,
        );
        match &r.outcome {
            letibot_transcript::ToolOutcome::NotRun { why } => {
                assert!(why.contains("fails closed"), "{why}");
            }
            other => panic!("a write tool with no adjudicator must not run: {other:?}"),
        }
        assert!(h.read_file("README.md").contains("letibot"));
    }

    #[test]
    fn a_crlf_file_keeps_its_endings() {
        let mut h = writable_harness();
        h.write_file("w.txt", "one\r\ntwo\r\n");
        h.call("read", r#"{"path":"w.txt"}"#);
        let r = h.call(
            "edit",
            r#"{"path":"w.txt","old_string":"two","new_string":"three"}"#,
        );
        assert!(r.is_grounded(), "{}", r.render());
        assert_eq!(h.read_file("w.txt"), "one\r\nthree\r\n");
    }

    #[test]
    fn an_identical_replacement_is_refused_rather_than_touching_the_file() {
        let mut h = writable_harness();
        h.call("read", r#"{"path":"README.md"}"#);
        let r = h.call(
            "edit",
            r#"{"path":"README.md","old_string":"letibot","new_string":"letibot"}"#,
        );
        assert!(r.render().contains("identical"), "{}", r.render());
    }
}

#[cfg(test)]
mod batch_tests {
    use crate::testing::writable_harness;

    /// **Four changes to one file, in one call.** This is the capability whose
    /// absence sent models to shell heredocs: four non-adjacent edits were four
    /// round trips and four gate decisions, so `python3 - <<'PY'` was simply the
    /// cheaper way to say it. Measured across this session, in both the
    /// operator's agent and the one writing this.
    #[test]
    fn a_batch_applies_every_step_in_order_and_reports_one_edit() {
        let mut h = writable_harness();
        h.call("read", r#"{"path":"src/lib.rs"}"#);
        let before = h.read_file("src/lib.rs");
        assert!(before.contains("pub fn parse_args"));

        let r = h.call(
            "edit",
            r#"{"path":"src/lib.rs","edits":[
                 {"old_string":"pub fn parse_args","new_string":"pub fn parse_argv"},
                 {"insert_before":"pub fn parse_argv","new_string":"/// doc line\n"}
               ]}"#,
        );
        assert!(r.is_grounded(), "{}", r.render());
        let after = h.read_file("src/lib.rs");
        assert!(after.contains("pub fn parse_argv"), "{after}");
        assert!(
            after.contains("/// doc line\npub fn parse_argv"),
            "the insertion did not land before the anchor, and did not repeat it: {after}"
        );
        // One card for the whole batch, with both sides of the file.
        let e = r.edit.as_ref().expect("the head is handed both sides");
        assert_eq!(e.replacements, 2);
        assert!(e.before.contains("pub fn parse_args"));
        assert!(e.after.contains("/// doc line"));
    }

    /// **All or nothing.** A batch whose third step does not resolve must leave
    /// the file exactly as it was: half a change on disk, reported as an error,
    /// is the state no caller can recover from.
    #[test]
    fn a_batch_that_fails_partway_writes_nothing() {
        let mut h = writable_harness();
        h.call("read", r#"{"path":"src/lib.rs"}"#);
        let before = h.read_file("src/lib.rs");

        let r = h.call(
            "edit",
            r#"{"path":"src/lib.rs","edits":[
                 {"old_string":"pub fn parse_args","new_string":"pub fn parse_argv"},
                 {"old_string":"this text is nowhere in the file","new_string":"x"}
               ]}"#,
        );
        assert!(!r.is_grounded(), "a missing step must fail: {}", r.render());
        let msg = r.render();
        assert!(msg.contains("step 2"), "the failing step is named: {msg}");
        assert!(msg.contains("Nothing was written"), "{msg}");
        assert_eq!(
            h.read_file("src/lib.rs"),
            before,
            "the first step was written despite the batch failing"
        );
    }

    /// A step that names neither a replacement nor an insertion — or both — is
    /// ambiguous, and an ambiguous edit is refused rather than guessed at.
    #[test]
    fn a_step_must_say_exactly_what_it_changes() {
        let mut h = writable_harness();
        h.call("read", r#"{"path":"src/lib.rs"}"#);
        let before = h.read_file("src/lib.rs");

        for bad in [
            r#"{"path":"src/lib.rs","edits":[{"new_string":"x"}]}"#,
            r#"{"path":"src/lib.rs","edits":[{"old_string":"a","insert_after":"b","new_string":"x"}]}"#,
            r#"{"path":"src/lib.rs","edits":[]}"#,
        ] {
            let r = h.call("edit", bad);
            assert!(!r.is_grounded(), "{bad} was accepted: {}", r.render());
            assert_eq!(h.read_file("src/lib.rs"), before, "{bad} changed the file");
        }
    }
}
