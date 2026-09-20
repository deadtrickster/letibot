//! `grep` — the acceptance case for clause 1, stated in the plan as:
//!
//! > a `grep` that finds nothing under a scoped path reports where the term *does*
//! > occur … a too-strict anchor is auto-relaxed to the bare identifier and the
//! > relaxation is reported.
//!
//! Both are implemented as a **ladder of named attempts**, and every rung that was
//! climbed is reported. The order matters: relax the pattern before widening the
//! scope, because a model that wrote `^\s*fn parse_args\(` usually had the right
//! file and the wrong syntax, and telling it about another directory first sends
//! it somewhere it did not need to go.
//!
//! What this deliberately does not do is rewrite the query and report the result
//! as though the original had matched. §9.4: *"the harness must not silently
//! 'improve' a tool's query … the rewrite must be visible in the tool result."*
//! Every relaxation here is in the notes, in the words the model reads.

use serde_json::Value;

use crate::backend::{DirEntry, default_skip, walk};
use crate::runtime::{Invocation, InvokeCtx, Tool};
use crate::schema::{Access, ToolSchema};

use super::pattern::{Pattern, PatternError, bare_identifier, glob_match};
use super::text_of;

pub struct Grep;

struct Hit {
    path: String,
    line_no: usize,
    line: String,
    /// `(line_no, text)` for the lines around the match, empty when `context` is
    /// 0. Captured during the scan because that is the only place the file's
    /// text is in hand; rendering re-reads nothing.
    region: Vec<(usize, String)>,
}

/// A ceiling on `context`, so one match cannot return a file. Twenty lines
/// either side is more than enough to see what a match sits in, which is the
/// question this answers.
const MAX_CONTEXT: usize = 20;

/// One rung of the ladder: a pattern, a scope, and what to call it if it works.
struct Attempt {
    pattern: Pattern,
    scope: String,
    /// `None` for the rung the model actually asked for.
    relaxation: Option<String>,
}

impl Tool for Grep {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "grep",
            "Search file contents for a pattern and return matching lines with their \
             paths and line numbers. Give `pattern`; optionally `path` to scope the \
             search, `glob` to restrict which files are read, and `case_insensitive`. \
             A search that matches nothing under `path` reports where the pattern does \
             match, and an over-anchored pattern is retried as its bare identifier — \
             both in the same call, so a miss does not need a second one.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "Regular expression, in Rust `regex` syntax: literals, ., *, +, ?, {n,m}, [...], (...), |, ^, $, \\b, \\w, \\d, \\s, \\p{...}, and inline flags like (?i). Matched against one LINE at a time. No backreferences and no lookaround, a question that needs those is structural, and `outline` answers it."},
                    "path": {"type": "string", "description": "Directory to search under. Defaults to the session root."},
                    "glob": {"type": "string", "description": "Only read files whose path matches this glob."},
                    "case_insensitive": {"type": "boolean"},
                    "max_matches": {"type": "integer"},
                    "context": {"type": "integer", "description": "Lines of surrounding file to show around each match, like `grep -C`. 0 (the default) lists matches only. With it set, this is ONE call instead of a grep followed by a read: use it whenever you want to see what a match sits in."}
                },
                "required": ["pattern"]
            }),
            Access::Read,
        )
    }

    fn invoke(&self, ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        let Some(source) = args.get("pattern").and_then(|v| v.as_str()) else {
            return Invocation::failed(
                "grep needs a pattern",
                "call `grep` again with `pattern` set to what you are looking for.",
            );
        };
        let scope = args
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or(".")
            .to_string();
        let file_glob = args.get("glob").and_then(|v| v.as_str());
        let ci = args
            .get("case_insensitive")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let max = args
            .get("max_matches")
            .and_then(|v| v.as_i64())
            .filter(|n| *n > 0)
            .map(|n| n as usize)
            .unwrap_or(ctx.limits.max_matches);
        // **Find and see, in one call.** Without this, `grep` names a line and a
        // second call has to go and read around it — which is why a model with a
        // shell reaches for `grep -n` piped into `sed -n 'A,Bp'` instead: not
        // because the tools cannot do it, but because the shell does it in one
        // round trip and this did it in two. At `automode` a round trip is also a
        // gate decision, so the second call is not only latency.
        let context = args
            .get("context")
            .and_then(|v| v.as_i64())
            .filter(|n| *n > 0)
            .map(|n| (n as usize).min(MAX_CONTEXT))
            .unwrap_or(0);

        // A `path` naming a FILE searched NOTHING and said so as if it were a fact
        // about the tree. `walk` starts by listing its root; `list` on a file errors,
        // the queue drains, and zero entries come back -- which the ladder then
        // reported as "matched nothing". Measured 2026-09-09 in a real session:
        // three rounds burned on `path: "src/main.rs"`, each answered with a
        // confident absence. Scoping to one file is an obvious thing to ask for, so
        // do it rather than diagnose it.
        // A scope that does not exist is its own miss, and `read`'s answer to it is
        // the right one here too.
        if ctx.backend.stat(&scope).is_none() && scope != "." {
            let (dir, entries) = super::nearest_listing(ctx.backend, &scope);
            return Invocation::failed(
                format!("no directory at `{scope}`"),
                super::render_listing(&dir, &entries, 40),
            )
            .with_note(format!(
                "`{scope}` does not exist, so nothing was searched. The listing of \
                 `{dir}` is below; search under one of these."
            ));
        }

        // A pattern that does not compile is a REPORTED ERROR. The engine that
        // used to sit here treated an unsupported construct as a literal string
        // and attached a note saying so -- which is still a search for something
        // other than what was asked, merely an annotated one, and the model has
        // no way to tell an annotated approximation from an answer. What comes
        // back now is `regex`'s own message, which names the position in the
        // pattern, plus the one sentence about what to do next.
        let asked = match Pattern::compile(source, ci) {
            Ok(p) => p,
            Err(e) => return compile_failed(&e),
        };

        let mut ladder: Vec<Attempt> = vec![Attempt {
            pattern: asked,
            scope: scope.clone(),
            relaxation: None,
        }];
        let bare = bare_identifier(source);
        if let Some(b) = &bare {
            push_rung(
                &mut ladder,
                b,
                ci,
                &scope,
                format!(
                    "the pattern `{source}` matched nothing, so it was relaxed to its bare \
                     identifier `{b}`"
                ),
            );
        }
        if !ci {
            let p = bare.clone().unwrap_or_else(|| source.to_string());
            let note = format!("`{p}` was retried without case sensitivity");
            push_rung(&mut ladder, &p, true, &scope, note);
        }
        if scope != "." {
            let p = bare.clone().unwrap_or_else(|| source.to_string());
            let note = format!(
                "nothing under `{scope}` matched, so `{p}` was searched across the \
                 whole session root"
            );
            push_rung(&mut ladder, &p, ci, ".", note);
        }

        let mut tried: Vec<String> = Vec::new();
        let mut files_scanned = 0usize;
        let mut unopened: Option<String> = None;
        let mut parseable: Vec<&'static str> = Vec::new();

        for attempt in &ladder {
            let scan = search(
                ctx,
                &attempt.pattern,
                &attempt.scope,
                file_glob,
                max,
                context,
            );
            files_scanned = files_scanned.max(scan.scanned);
            if attempt.scope == scope {
                unopened = unopened_note(scan.skipped_large, scan.unreached);
            }
            // Only from the scope the model ASKED about. The widened rung reads
            // the whole session root, and letting its languages count would
            // produce "the files searched here are rust" about a `path: docs`
            // holding nothing but markdown -- and then `outline` on that path
            // would refuse. A suggestion that costs a round to save a round is
            // worse than none.
            if attempt.scope == scope {
                for l in &scan.languages {
                    if !parseable.contains(l) {
                        parseable.push(l);
                    }
                }
            }
            let (hits, truncated) = (scan.hits, scan.truncated);
            if hits.is_empty() {
                if let Some(r) = &attempt.relaxation {
                    tried.push(r.clone());
                }
                continue;
            }

            let mut inv = Invocation::ok(render_hits(&hits, &attempt.scope));
            // The relaxation, and every rung below it, in the result the model
            // reads. A silent relaxation is a rewritten query.
            for t in &tried {
                inv = inv.with_note(t.clone());
            }
            if let Some(r) = &attempt.relaxation {
                inv = inv.with_note(format!(
                    "{r} — {} match(es). The original pattern still matches nothing.",
                    hits.len()
                ));
                if attempt.scope != scope {
                    inv = inv.with_note(where_it_occurs(&hits, &scope));
                }
                // MATCHED POORLY: the pattern the model wrote found nothing and a
                // relaxation rescued the call. If the pattern was structural, the
                // relaxation is not the fix -- it is the second-best lexical
                // approximation of a question that has an exact answer one tool
                // over.
                if let Some(n) = outline_suggestion(source, &scope, &parseable) {
                    inv = inv.with_note(n);
                }
            }
            if truncated {
                inv = inv.with_note(format!(
                    "stopped after {max} matches; narrow `path` or `glob`, or raise \
                     `max_matches`"
                ));
            }
            if let Some(n) = &unopened {
                inv = inv.with_note(n.clone());
            }
            return inv;
        }

        // ZERO FILES SEARCHED IS NOT A FACT ABOUT THE TREE. This is the defect that
        // sent a real session five rounds sideways: `glob: "*.rs"` matches against the
        // whole relative path, so it selected none of `src/*.rs`, and grep answered
        // "`#\[cfg\(test\)\]` does not occur in the searched tree" -- about a tree
        // holding nine of them. An empty corpus and an empty result are different
        // facts and they were reported identically.
        //
        // The general rule, and the one worth keeping: a count does not travel
        // without its denominator. A denominator of zero is a failed scope, never
        // an answer about content.
        if files_scanned == 0 {
            let mut why = String::new();
            if let Some(g) = file_glob {
                why.push_str(&format!(
                    "`glob` is matched against each file's PATH from the session \
                     root, not its name, so `{g}` selects nothing under a \
                     subdirectory. Try `**/{}` or drop `glob` and narrow with \
                     `path`.\n",
                    g.trim_start_matches("*/").trim_start_matches('*')
                ));
            }
            why.push_str(&format!(
                "nothing under `{scope}` was opened, so this call says NOTHING \
                 about whether `{source}` occurs. Fix the scope and ask again."
            ));
            return Invocation::failed(
                format!("0 files matched the scope, so `{source}` was never searched for"),
                why,
            );
        }

        // Nothing, anywhere, under any relaxation. That is not a result, and §8.2
        // says it must not be dressed as one — but the body still says what was
        // searched and what to do next.
        let mut body = format!(
            "searched {files_scanned} file(s) under `{scope}`{}.\n",
            file_glob
                .map(|g| format!(" matching `{g}`"))
                .unwrap_or_default()
        );
        if let Some(n) = &unopened {
            body.push_str(&format!("{n}\n"));
        }
        body.push_str("relaxations tried, all of them empty:\n");
        for t in &tried {
            body.push_str(&format!("  - {t}\n"));
        }
        body.push_str(
            "\nthe string is not in the searched tree. `glob` searches file *names*, \
             and `ask_code` answers questions the text does not spell out.\n",
        );
        let suggestion = outline_suggestion(source, &scope, &parseable);
        let mut inv = Invocation::abstained(
            format!("`{source}` does not occur in the {files_scanned} file(s) searched"),
            body,
        );
        if let Some(n) = suggestion {
            inv = inv.with_note(n);
        }
        inv
    }
}

/// The definition keywords that make a pattern structural rather than lexical.
///
/// Not "words that appear in code" — words that name a KIND OF DEFINITION, which
/// is what `outline` indexes. `pub`, `return` and `let` are deliberately absent:
/// a search for `pub` is a search for text.
const DEFINITION_KEYWORDS: &[&str] = &[
    "fn",
    "func",
    "def",
    "struct",
    "enum",
    "impl",
    "trait",
    "mod",
    "class",
    "interface",
    "type",
    "macro_rules",
    "package",
];

/// Does this pattern ask a STRUCTURAL question in a lexical language?
///
/// The test is deliberately narrow — a LINE ANCHOR plus a definition keyword —
/// because a suggestion that fires on every search that mentions `fn` is noise,
/// and noise in the notes is how the notes stop being read. The anchor is what
/// makes it structural: `^fn` is not looking for the text `fn`, it is looking
/// for a definition and guessing at the column it starts in.
fn is_structural(source: &str) -> bool {
    if !source.contains('^') {
        return false;
    }
    let mut word = String::new();
    let mut hit = false;
    for c in source.chars().chain([' ']) {
        if c.is_alphanumeric() || c == '_' {
            word.push(c);
            continue;
        }
        if DEFINITION_KEYWORDS.contains(&word.as_str()) {
            hit = true;
        }
        word.clear();
    }
    hit
}

/// Name `outline` when it would actually have answered, and stay quiet otherwise.
///
/// Two conditions, and the second is the one that keeps it honest: the pattern
/// has to be structural, AND the files that were actually opened have to be in a
/// language this build can parse. Suggesting `outline` for a tree of `.tf` files
/// would be handing the model a tool that will refuse it — a second wasted round
/// to save a first one.
///
/// It suggests and does not run. §9.4: the harness must not silently improve a
/// tool's query, and running a different tool is a larger rewrite than relaxing
/// a pattern, not a smaller one. The model decides.
fn outline_suggestion(source: &str, scope: &str, parseable: &[&'static str]) -> Option<String> {
    if !is_structural(source) || parseable.is_empty() {
        return None;
    }
    Some(format!(
        "`{source}` is a STRUCTURAL question asked lexically, and relaxing it cannot \
         fix that: keep the `^` and a definition indented under an `impl` or a `class` \
         is invisible, drop it and the keyword matches inside comments and strings. \
         `outline` with `path: \"{scope}\"` answers it from a parse — definitions with \
         line numbers, kinds and their containing symbol. The files searched here are \
         {} and it can parse those.",
        parseable.join(", ")
    ))
}

/// Add a rung, or leave it off if its pattern will not compile.
///
/// A rung is a RELAXATION, so it is allowed to be unavailable: the bare
/// identifier and the widened scope are both derived from a pattern that already
/// compiled, so this is close to unreachable — but a rung that silently searched
/// for something else would be the exact failure the compile error exists to
/// prevent, one level down.
fn push_rung(ladder: &mut Vec<Attempt>, source: &str, ci: bool, scope: &str, relaxation: String) {
    if let Ok(pattern) = Pattern::compile(source, ci) {
        ladder.push(Attempt {
            pattern,
            scope: scope.to_string(),
            relaxation: Some(relaxation),
        });
    }
}

/// The engine's own words about the pattern, and what to do about it.
///
/// `failed`, not `abstained`: an abstention is a claim that the content was
/// looked at and did not contain the term, and this call opened no files at all.
/// Same rule as the zero-files case below.
fn compile_failed(e: &PatternError) -> Invocation {
    Invocation::failed(
        format!("`{}` did not compile, so nothing was searched", e.source),
        format!("{}\n\n{}", e.message, e.remedy()),
    )
    .with_note(
        "the pattern was NOT approximated and NOT searched for literally: this call \
         says nothing about whether it occurs.",
    )
}

/// The largest file a search opens. A source file is kilobytes; a file past this
/// is a model shard, a database, a tarball — and one leticode session, searching
/// from `/home` for a file it had mis-addressed, read a 27 GB shard whole into
/// memory and spent twelve minutes in the kernel. Skipped files are counted and
/// named in the result, so a search that missed something says so.
pub const FILE_CEILING: u64 = 16 * 1024 * 1024;

/// The most bytes one rung of a search reads in total. Past it the rung stops and
/// says how many files it never reached — a bounded answer that names its bound,
/// not a daemon that stops answering heads.
pub const BYTES_BUDGET: u64 = 512 * 1024 * 1024;

/// What one rung of the ladder found, and over what.
struct Scan {
    hits: Vec<Hit>,
    /// Files actually OPENED. The denominator; zero is a failed scope.
    scanned: usize,
    truncated: bool,
    /// Files over [`FILE_CEILING`], never opened.
    skipped_large: usize,
    /// Files after [`BYTES_BUDGET`] ran out, never opened.
    unreached: usize,
    /// The languages among the files opened that `outline` has a grammar for.
    /// Collected here because it is the only place that knows which files were
    /// really read, and a suggestion to use `outline` is only honest if it would
    /// have worked.
    languages: Vec<&'static str>,
}

fn search(
    ctx: &mut InvokeCtx<'_>,
    pattern: &Pattern,
    scope: &str,
    file_glob: Option<&str>,
    max: usize,
    context: usize,
) -> Scan {
    // `scope` may name a single file; `walk` would list it, fail, and return
    // nothing. See the `single_file` note above.
    let one: Vec<DirEntry>;
    let entries: &[DirEntry] = match ctx.backend.stat(scope) {
        Some(e) if !e.is_dir => {
            one = vec![e];
            &one
        }
        _ => {
            let (walked, _) = walk(
                ctx.backend,
                scope,
                ctx.limits.max_walk_entries,
                &default_skip,
            );
            one = walked;
            &one
        }
    };
    let files: Vec<&DirEntry> = entries
        .iter()
        .filter(|e| !e.is_dir)
        .filter(|e| file_glob.is_none_or(|g| glob_match(g, &e.path)))
        .collect();

    let mut hits = Vec::new();
    let mut scanned = 0usize;
    let mut skipped_large = 0usize;
    let mut unreached = 0usize;
    let mut read_bytes = 0u64;
    let mut languages: Vec<&'static str> = Vec::new();
    for (i, e) in files.iter().enumerate() {
        if i > 0 && i % 500 == 0 {
            // §8.5: progress is liveness. A walk over a large tree must produce it.
            ctx.progress(format!("scanned {i} of {} files", files.len()));
        }
        if e.bytes > FILE_CEILING {
            skipped_large += 1;
            continue;
        }
        if read_bytes >= BYTES_BUDGET {
            unreached = files.len() - i;
            break;
        }
        read_bytes += e.bytes;
        let Ok(bytes) = ctx.backend.read(&e.path) else {
            continue;
        };
        if bytes.contains(&0) {
            // Binary. Skipped rather than rendered as replacement characters.
            continue;
        }
        scanned += 1;
        if let Ok(l) = letibot_code::Language::of_path(&e.path)
            && !languages.contains(&l.name())
        {
            languages.push(l.name());
        }
        let (text, _) = text_of(&bytes);
        // One scan of the whole buffer rather than one engine call per line. The
        // semantics are identical -- `line_hits` confirms every candidate against
        // the line-scoped program -- and the measurement is the whole reason the
        // matcher was replaced.
        let remaining = max - hits.len();
        let path = e.path.clone();
        // Only when asked: collecting a file's lines is cheap but not free, and
        // the common call passes no context at all.
        let file_lines: Vec<&str> = if context > 0 {
            text.lines().collect()
        } else {
            Vec::new()
        };
        let truncated = pattern.line_hits(&text, remaining, |line_no, line| {
            let region = if context > 0 {
                let first = line_no.saturating_sub(context).max(1);
                let last = (line_no + context).min(file_lines.len());
                (first..=last)
                    .map(|n| (n, file_lines[n - 1].to_string()))
                    .collect()
            } else {
                Vec::new()
            };
            hits.push(Hit {
                path: path.clone(),
                line_no,
                line: line.to_string(),
                region,
            });
        });
        if truncated {
            return Scan {
                hits,
                scanned,
                truncated: true,
                skipped_large,
                unreached,
                languages,
            };
        }
    }
    Scan {
        hits,
        scanned,
        truncated: false,
        skipped_large,
        unreached,
        languages,
    }
}

/// The files a rung did not open, as a note — or nothing, when it opened them all.
fn unopened_note(scan_skipped: usize, scan_unreached: usize) -> Option<String> {
    let mut parts = Vec::new();
    if scan_skipped > 0 {
        parts.push(format!(
            "{scan_skipped} file(s) over {} MiB were not opened",
            FILE_CEILING / (1024 * 1024)
        ));
    }
    if scan_unreached > 0 {
        parts.push(format!(
            "{scan_unreached} file(s) were never reached: the search stopped after \
             reading {} MiB",
            BYTES_BUDGET / (1024 * 1024)
        ));
    }
    if parts.is_empty() {
        None
    } else {
        Some(format!(
            "{}. This says nothing about those files; narrow `path` or `glob` to \
             reach them.",
            parts.join("; ")
        ))
    }
}

fn render_hits(hits: &[Hit], scope: &str) -> String {
    let mut out = format!("{} match(es) under `{scope}`:\n", hits.len());
    if hits.iter().all(|h| h.region.is_empty()) {
        for h in hits {
            out.push_str(&format!("{}:{}: {}\n", h.path, h.line_no, clip(&h.line)));
        }
        return out;
    }
    // **With context, the answer is regions rather than lines.** Numbered, so a
    // location can be cited and an edit can be made from it without a second
    // look — which is the whole reason `sed -n 'A,Bp'` was being reached for.
    //
    // Overlapping regions are printed once. Two matches three lines apart with
    // `context: 5` are one block, not two blocks quoting each other, and `--`
    // marks a real gap so nobody reads two blocks as contiguous.
    let mut last: Option<(&str, usize)> = None;
    for h in hits {
        for (n, text) in &h.region {
            match last {
                Some((p, prev)) if p == h.path && *n <= prev => continue,
                Some((p, prev)) if p == h.path && *n > prev + 1 => out.push_str("--\n"),
                Some((p, _)) if p != h.path => out.push_str("--\n"),
                _ => {}
            }
            // The match itself is marked, so a region does not have to be counted
            // through to find what was asked about.
            let mark = if *n == h.line_no { ':' } else { '-' };
            out.push_str(&format!("{}:{n}{mark} {}\n", h.path, clip(text)));
            last = Some((&h.path, *n));
        }
    }
    out
}

/// Clause 1's first acceptance case in one sentence: **where the term does occur**,
/// by file, with counts.
fn where_it_occurs(hits: &[Hit], asked_scope: &str) -> String {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for h in hits {
        match counts.iter_mut().find(|(p, _)| *p == h.path) {
            Some((_, n)) => *n += 1,
            None => counts.push((h.path.clone(), 1)),
        }
    }
    counts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let listed: Vec<String> = counts
        .iter()
        .take(10)
        .map(|(p, n)| format!("{p} ({n})"))
        .collect();
    format!(
        "nothing matches under `{asked_scope}`; it occurs in {}",
        listed.join(", ")
    )
}

/// A single matched line is shown up to a bound, with the omission counted.
///
/// Not a violation of clause 5: the payload as a whole is spilled, never
/// truncated, and the full line is one `read` away at the path and line number
/// printed beside it. What this avoids is one minified file turning a hit list
/// into a megabyte.
fn clip(line: &str) -> String {
    const CAP: usize = 400;
    if line.chars().count() <= CAP {
        return line.trim_end().to_string();
    }
    let head: String = line.chars().take(CAP).collect();
    format!(
        "{head}… (+{} more characters on this line)",
        line.chars().count() - CAP
    )
}

#[cfg(test)]
mod tests {
    use crate::testing::harness;

    #[test]
    fn a_plain_hit_is_path_line_text() {
        let mut h = harness();
        let r = h.call("grep", r#"{"pattern":"TokenLedger"}"#);
        assert!(r.is_grounded());
        assert!(r.payload.contains("src/lib.rs:"), "{}", r.payload);
    }

    #[test]
    fn nothing_under_the_scoped_path_reports_where_it_does_occur() {
        // The plan's first acceptance test for clause 1, verbatim.
        let mut h = harness();
        let r = h.call("grep", r#"{"pattern":"cache_prompt","path":"src"}"#);
        assert!(r.is_grounded(), "a redirect is a result: {:?}", r.outcome);
        let notes = r.notes.join(" ");
        assert!(notes.contains("docs/notes.md"), "{notes}");
        assert!(r.payload.contains("docs/notes.md:"), "{}", r.payload);
    }

    #[test]
    fn a_too_strict_anchor_is_relaxed_to_the_bare_identifier_and_says_so() {
        // The plan's third acceptance test. The source has `pub fn parse_args()`,
        // so `^\s*fn parse_args\(` is exactly the guess that misses.
        let mut h = harness();
        let r = h.call("grep", r#"{"pattern":"^\\s*fn parse_args\\("}"#);
        assert!(r.is_grounded(), "{:?}", r.outcome);
        let notes = r.notes.join(" ");
        assert!(notes.contains("bare identifier"), "{notes}");
        assert!(notes.contains("parse_args"), "{notes}");
        assert!(r.payload.contains("src/lib.rs:"), "{}", r.payload);
    }

    /// The rano session, 2026-09-09. `grep` was asked for `#\[cfg\(test\)\]`
    /// with `glob: "*.rs"` under `src` and answered *"does not occur in the
    /// searched tree"* — about a tree holding nine of them. `glob` is matched
    /// against the whole relative path, so it selected zero files, and an empty
    /// CORPUS was reported as an empty RESULT.
    ///
    /// The assertion is not about wording. It is that a call which opened no
    /// files must not come back as an abstention, because an abstention is a
    /// claim about content and this call examined none.
    #[test]
    fn zero_files_searched_is_never_a_claim_about_the_tree() {
        let mut h = harness();
        let r = h.call("grep", r#"{"pattern":"fn ","path":"src","glob":"*.rs"}"#);
        let rendered = r.render();
        assert_ne!(
            crate::result::Envelope::classify(&rendered),
            Some("NO_RESULT"),
            "0 files opened must not abstain -- abstention claims the content was \
             looked at:\n{rendered}"
        );
        assert!(
            rendered.contains("never searched") || rendered.contains("0 files"),
            "{rendered}"
        );
        assert!(
            rendered.contains("**/"),
            "must say how to fix the glob:\n{rendered}"
        );
    }

    /// Same session: `path: "src/main.rs"` searched nothing three times, because
    /// `walk` lists its root and listing a file fails, so the queue drained empty.
    /// Scoping a search to one file is an ordinary request.
    #[test]
    fn a_path_naming_one_file_searches_that_file() {
        let mut h = harness();
        let r = h.call("grep", r#"{"pattern":"TokenLedger","path":"src/lib.rs"}"#);
        assert!(
            r.is_grounded(),
            "a file path must be searched:\n{}",
            r.render()
        );
        assert!(r.payload.contains("src/lib.rs:"), "{}", r.payload);
        // The assertion that actually distinguishes the fix. Without it the scope
        // yielded nothing and the LADDER rescued the call by re-searching the whole
        // session root -- same hits, same payload, and a passing test that proves
        // nothing. What the fix changes is that the file was searched DIRECTLY.
        let notes = r.notes.join(" ");
        assert!(
            !notes.contains("whole session root"),
            "the file scope must be searched directly, not rescued by the fallback \
             rung:\n{notes}"
        );
    }

    /// The survey found ~20-space runs inside two guidance strings -- a `\`
    /// continuation lost when the code was pasted, so the literal spaces shipped
    /// to the model. Source-level scanning for this is unreliable: a correct
    /// continuation looks the same to a naive scan, and it cost three wrong
    /// attempts. So assert the FACT -- what is actually rendered -- and scope it
    /// to the defect's shape, a run of spaces BETWEEN WORDS. The regex engine's
    /// error diagnostic aligns a caret with leading spaces and must keep them;
    /// a first cut of this test forbade that too.
    #[test]
    fn no_guidance_string_ships_a_run_of_spaces() {
        let mut h = harness();
        let calls = [
            r#"{"pattern":"fn ","path":"src","glob":"*.rs"}"#,
            r#"{"pattern":"quokka_sentinel"}"#,
            r#"{"pattern":"x","path":"srcc"}"#,
            r#"{"pattern":"TokenLedger"}"#,
            r#"{"pattern":"^(pub )?(fn|struct)","path":"src"}"#,
        ];
        for c in calls {
            let rendered = h.call("grep", c).render();
            let bad: Vec<&str> = rendered
                .lines()
                .filter(|l| {
                    let b = l.as_bytes();
                    (0..b.len()).any(|i| {
                        b[i] == b' '
                            && i > 0
                            && b[i - 1].is_ascii_alphanumeric()
                            && b[i..].iter().take_while(|c| **c == b' ').count() >= 4
                            && b[i..]
                                .iter()
                                .find(|c| **c != b' ')
                                .is_some_and(|c| c.is_ascii_alphanumeric())
                    })
                })
                .collect();
            assert!(
                bad.is_empty(),
                "a lost `\\` continuation reaches the model for {c}:\n{}",
                bad.join("\n")
            );
        }
    }

    #[test]
    fn a_term_that_is_nowhere_abstains_and_says_what_it_searched() {
        let mut h = harness();
        let r = h.call("grep", r#"{"pattern":"quokka_sentinel"}"#);
        assert!(!r.is_grounded());
        let rendered = r.render();
        assert_eq!(
            crate::result::Envelope::classify(&rendered),
            Some("NO_RESULT"),
            "{rendered}"
        );
        assert!(rendered.contains("searched"), "{rendered}");
    }

    /// Phase 1's clause 3. The engine that used to sit here would have searched
    /// for the LITERAL text `(?<=fn )\w+` — found nothing, said so, and attached
    /// a note about an unsupported construct. A model reading that has been told
    /// two true things and one false one: that the pattern does not occur.
    #[test]
    fn a_pattern_that_does_not_compile_is_reported_and_nothing_is_searched() {
        let mut h = harness();
        let r = h.call("grep", r#"{"pattern":"(?<=fn )\\w+"}"#);
        assert!(!r.is_grounded(), "{:?}", r.outcome);
        let seen = r.render();
        // The engine's own words, with the position.
        assert!(
            seen.contains("look-around") || seen.contains("lookaround"),
            "{seen}"
        );
        // And the fact that separates this from an abstention: no file was opened,
        // so the call makes NO claim about whether the pattern occurs.
        assert!(
            seen.contains("says nothing about whether it occurs"),
            "{seen}"
        );
        assert_ne!(
            crate::result::Envelope::classify(&seen),
            Some("NO_RESULT"),
            "a pattern that never compiled cannot abstain about content:\n{seen}"
        );
    }

    /// The other half of the same clause: the syntax the old subset refused now
    /// works, and is not reported as an approximation.
    #[test]
    fn counted_repetition_is_matched_rather_than_apologised_for() {
        let mut h = harness();
        let r = h.call("grep", r#"{"pattern":"a{1,3}rgs"}"#);
        assert!(r.is_grounded(), "{}", r.render());
        assert!(r.payload.contains("src/lib.rs:"), "{}", r.payload);
        let notes = r.notes.join(" ");
        assert!(
            !notes.contains("literally"),
            "no approximation note: {notes}"
        );
    }

    /// A pattern big enough to be a denial of service is refused, in the same
    /// shape as a syntax error, rather than compiled while a turn waits.
    #[test]
    fn a_pattern_over_the_size_limit_is_refused_with_a_remedy() {
        let mut h = harness();
        let r = h.call("grep", r#"{"pattern":"((((a{100}){100}){100}){100})"}"#);
        assert!(!r.is_grounded());
        let seen = r.render();
        assert!(seen.contains("KiB"), "{seen}");
    }

    /// Phase 2's escalation. The rano session wrote
    /// `^(pub )?(mod|fn|struct|enum|impl|const|static)\s` and got a relaxation,
    /// which is the best `grep` can do and is still the wrong answer: relaxing a
    /// structural query lexically can only miss. The RESULT must name the tool
    /// that answers it.
    #[test]
    fn a_structural_pattern_that_matches_poorly_names_outline() {
        let mut h = harness();
        // `src/lib.rs` has `pub fn parse_args`, so the anchored form misses and
        // the ladder rescues the call with the bare identifier.
        let r = h.call("grep", r#"{"pattern":"^\\s*fn parse_args\\("}"#);
        assert!(r.is_grounded(), "{}", r.render());
        let notes = r.notes.join(" ");
        assert!(notes.contains("outline"), "{notes}");
        assert!(notes.contains("STRUCTURAL"), "{notes}");
        // Suggested, not run: the payload is still grep's hits.
        assert!(r.payload.contains("src/lib.rs:"), "{}", r.payload);
    }

    #[test]
    fn a_structural_pattern_that_matches_nothing_at_all_names_outline() {
        let mut h = harness();
        let r = h.call("grep", r#"{"pattern":"^impl QuokkaSentinel"}"#);
        assert!(!r.is_grounded());
        let notes = r.notes.join(" ");
        assert!(notes.contains("outline"), "{notes}");
    }

    /// Honesty is the whole value of the suggestion. A lexical search gets no
    /// suggestion however badly it misses, and neither does a structural one
    /// over files nothing here can parse — pointing a model at a tool that will
    /// refuse it spends a round to save a round.
    #[test]
    fn the_suggestion_stays_quiet_when_it_would_not_have_helped() {
        let mut h = harness();

        // Lexical: no anchor, no definition keyword, so no suggestion.
        let r = h.call("grep", r#"{"pattern":"quokka_sentinel"}"#);
        assert!(!r.notes.join(" ").contains("outline"), "{:?}", r.notes);

        // Structural, but every file in scope is markdown and there is no
        // grammar for it.
        let r = h.call("grep", r#"{"pattern":"^impl Widget","path":"docs"}"#);
        assert!(
            !r.notes.join(" ").contains("outline"),
            "docs/ is markdown: {:?}",
            r.notes
        );
    }

    #[test]
    fn a_scope_that_does_not_exist_returns_the_listing() {
        let mut h = harness();
        let r = h.call("grep", r#"{"pattern":"x","path":"srcc"}"#);
        assert!(r.payload.contains("src"), "{}", r.payload);
    }

    /// A file past [`FILE_CEILING`] is never opened, and the result says so.
    ///
    /// The defect this guards is the one the ceiling was added for: a search that
    /// silently skipped a file would report an absence it did not establish. The
    /// skipped file is created SPARSE — `set_len` gives it the size without the
    /// bytes — so the test costs a metadata call rather than 16 MiB of writes, and
    /// its name is a candidate for the same search so that only the ceiling can
    /// explain its absence from the hits.
    #[test]
    fn a_file_over_the_ceiling_is_skipped_and_the_result_says_so() {
        let mut h = harness();
        h.write_file("src/small.rs", "pub fn quokka_sentinel() {}\n");
        let big =
            std::fs::File::create(h.root().join("src/big_candidate.rs")).expect("sparse file");
        big.set_len(super::FILE_CEILING + 1).expect("set_len");
        drop(big);

        let r = h.call("grep", r#"{"pattern":"quokka_sentinel","path":"src"}"#);
        assert!(r.is_grounded(), "{}", r.render());
        assert!(r.payload.contains("src/small.rs:"), "{}", r.payload);
        assert!(
            !r.payload.contains("big_candidate.rs"),
            "a file over the ceiling must not be read:\n{}",
            r.payload
        );
        let notes = r.notes.join(" ");
        assert!(notes.contains("were not opened"), "{notes}");
        assert!(
            notes.contains("big_candidate.rs") || notes.contains("1 file(s)"),
            "{notes}"
        );
    }
}

#[cfg(test)]
mod context_tests {
    use crate::testing::writable_harness;

    /// **Find and see, in one call.** Without `context`, `grep` names a line and
    /// a second call has to read around it — which is the `grep -n` into `sed -n
    /// 'A,Bp'` loop, done with tools. The operator: *"so it does grep, gets line
    /// numbers for matches and does sed for surroundings? but we have grep tool
    /// and read tool"* — both exist, and the shell won on round trips.
    #[test]
    fn context_returns_the_region_around_a_match_with_line_numbers() {
        let mut h = writable_harness();
        let plain = h.call("grep", r#"{"pattern":"parse_args","path":"src/lib.rs"}"#);
        let plain = plain.render();
        assert!(plain.contains("parse_args"), "{plain}");

        let with = h.call(
            "grep",
            r#"{"pattern":"parse_args","path":"src/lib.rs","context":2}"#,
        );
        let with = with.render();
        // The match is still there, and now so is what surrounds it — numbered,
        // so the next call can be an `edit` rather than another look.
        assert!(with.contains("parse_args"), "{with}");
        assert!(
            with.lines().count() > plain.lines().count(),
            "context returned no more than a bare match:\n{with}"
        );
        // Every body line carries its own number, which is exactly what `sed`
        // does not give.
        let numbered = with.lines().filter(|l| l.contains("src/lib.rs:")).count();
        assert!(
            numbered >= 2,
            "the region is not numbered per line:\n{with}"
        );
    }

    /// The ceiling is real: one match cannot return a file.
    #[test]
    fn context_is_capped() {
        let mut h = writable_harness();
        let r = h
            .call(
                "grep",
                r#"{"pattern":"parse_args","path":"src/lib.rs","context":100000}"#,
            )
            .render();
        assert!(r.contains("parse_args"), "{r}");
        assert!(r.lines().count() < 200, "a match returned the world:\n{r}");
    }
}
