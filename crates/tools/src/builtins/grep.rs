//! `grep`: search a tree, and answer a miss with where the term does occur.
//! Both halves are a ladder of named attempts, every rung climbed is reported, and the pattern is
//! relaxed before the scope is widened. A relaxation is in the notes, never silent.

use letibot_transcript::text::clip_line_counted;
use serde_json::Value;

use crate::backend::{BackendError, DirEntry, Ignored, walk_ignoring};
use crate::runtime::{Invocation, InvokeCtx, Tool};
use crate::schema::{Access, ToolSchema};

use super::paging::{Reader, Stop};
use super::pattern::{Pattern, PatternError, bare_identifier, glob_match};
use super::text_of;

pub struct Grep;

struct Hit {
    path: String,
    line_no: usize,
    line: String,
    /// `(line_no, text)` for the lines around the match, empty when `context` is zero.
    /// Captured during the scan because that is the only place the file's text is in hand;
    /// rendering re-reads nothing.
    region: Vec<(usize, String)>,
    /// Where in `line` the match is, as byte offsets, and `None` for a reading that did not say.
    /// It is what centres the window a very long line is shown through, and what moves `edit`'s
    /// gate from the line's first 400 characters to the piece that was really displayed.
    span: Option<(usize, usize)>,
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
             paths and line numbers. Give `pattern`; optionally `context` to see the \
             surrounding lines (like `grep -C`), `path` to scope the search, `glob` to \
             restrict which files are read, and `case_insensitive`. **With `context` \
             this is one call where it used to be two** — a grep for the line numbers \
             and then a read around them — so reach for it whenever the question is \
             what a match sits in rather than where it is. \
             A search that matches nothing under `path` reports where the pattern does \
             match, and an over-anchored pattern is retried as its bare identifier — \
             both in the same call, so a miss does not need a second one. Files \
             git ignores (`.gitignore`, `.git/info/exclude`, your global excludes) are \
             skipped unless `path` names them.",
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
        // Find and see, in one call: without this, a model with a shell reaches for `grep -n`
        // piped into `sed -n 'A,Bp'`, because the shell does both in one round trip. At
        // `automode` a round trip is also a gate decision, so the second call is not only latency.
        let context = args
            .get("context")
            .and_then(|v| v.as_i64())
            .filter(|n| *n > 0)
            .map(|n| (n as usize).min(MAX_CONTEXT))
            .unwrap_or(0);

        // Before the miss below: a fenced path is denied, not absent, and the difference is the
        // whole answer - one says the boundary decided, the other says nothing is there.
        if let Some(why) = ctx.backend.file_refusal(&scope) {
            return Invocation::failed(why, String::new());
        }

        // A `path` naming a file would otherwise search nothing and report "matched nothing" about
        // a file that is there. The stat seeds the listing below, and `scope != "."` is tested
        // first to skip a round trip for the root.
        let at = if scope == "." {
            None
        } else {
            ctx.backend.stat(&scope)
        };
        if scope != "." && at.is_none() {
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

        // A pattern that does not compile is a reported error: treating an unsupported construct as
        // a literal is still a search for something other than what was asked. What comes back is
        // the engine's own message, naming the position in the pattern.
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
        // What a reading said about itself - which `rg` answered, where - said once and kept even
        // when the rung that ran it found nothing: it is a fact about this call, not about a hit.
        let mut notes: Vec<String> = Vec::new();
        let mut files_scanned = 0usize;
        let mut unopened: Option<String> = None;
        let mut ignored = Ignored::default();
        let mut parseable: Vec<&'static str> = Vec::new();
        // One listing per scope, not one per rung: the ladder has four rungs and three of them can
        // share a scope, and a walk is a round trip over every directory in it. A scope that named
        // a file starts from the stat above, which is the whole of its listing.
        let mut walked = match at {
            Some(entry) if !entry.is_dir => Listings {
                scope: Some(scope.clone()),
                entries: vec![entry],
                ignored: Ignored::default(),
            },
            _ => Listings::default(),
        };

        for attempt in &ladder {
            let scan = search(
                ctx,
                &mut walked,
                &attempt.pattern,
                &attempt.scope,
                file_glob,
                max,
                context,
            );
            files_scanned = files_scanned.max(scan.scanned);
            notes.extend(scan.notes.iter().cloned());
            if walked.ignored.files > ignored.files {
                ignored = walked.ignored.clone();
            }
            // A scan that stopped on its bound or on a dying channel ends the ladder: every rung
            // below would pay the same round trips to learn the same nothing.
            if let Some(stop) = &scan.stop {
                mark_shown(ctx, &scan.hits);
                let mut inv = stopped(&scan, stop, &attempt.scope, ctx.call_bound());
                for t in &tried {
                    inv = inv.with_note(t.clone());
                }
                return with_notes(inv, &notes);
            }
            if attempt.scope == scope {
                unopened = unopened_note(scan.skipped_large, scan.unreached);
            }
            // Only from the scope the model asked about: the widened rung reads the whole
            // session root, and letting its languages count would say "the files searched here
            // are rust" about a `path: docs` holding only markdown.
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

            mark_shown(ctx, &hits);
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
                // MATCHED POORLY: a relaxation rescued the call. For a structural question that
                // is not the fix, only the second-best lexical approximation of a question one
                // tool over answers exactly.
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
            return with_notes(inv, &notes);
        }

        // Zero files searched is not a fact about the tree: `glob: "*.rs"` matches the whole relative
        // path, so it can select none of `src/*.rs`. A count does not travel without its denominator,
        // and a denominator of zero is a failed scope rather than an answer about content.
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
            if let Some(n) = ignored.note() {
                why.push_str(&format!("\n{n}."));
            }
            return with_notes(
                Invocation::failed(
                    format!("0 files matched the scope, so `{source}` was never searched for"),
                    why,
                ),
                &notes,
            );
        }

        // Nothing, anywhere, under any relaxation. That is not a result and must not be dressed
        // as one, but the body still says what was searched and what to do next.
        // The count and the miss are the reason's; the body adds where and what else was tried.
        let mut body = format!(
            "searched under `{scope}`{}.\n",
            file_glob
                .map(|g| format!(" matching `{g}`"))
                .unwrap_or_default()
        );
        if let Some(n) = &unopened {
            body.push_str(&format!("{n}\n"));
        }
        if let Some(n) = ignored.note() {
            body.push_str(&format!("{n}.\n"));
        }
        body.push_str("relaxations tried, all of them empty:\n");
        for t in &tried {
            body.push_str(&format!("  - {t}\n"));
        }
        body.push_str(
            "\n`glob` searches file *names*, and `ask_code` answers questions the text does \
             not spell out.\n",
        );
        let suggestion = outline_suggestion(source, &scope, &parseable);
        let mut inv = Invocation::abstained(
            format!("`{source}` does not occur in the {files_scanned} file(s) searched"),
            body,
        );
        if let Some(n) = suggestion {
            inv = inv.with_note(n);
        }
        with_notes(inv, &notes)
    }
}

/// The notes a call's readings left, on the result the model reads.
fn with_notes(mut inv: Invocation, notes: &[String]) -> Invocation {
    for n in notes {
        inv = inv.with_note(n.clone());
    }
    inv
}

/// The definition keywords that make a pattern structural rather than lexical: words that name a
/// KIND OF DEFINITION, which is what `outline` indexes. `pub`, `return` and `let` are absent,
/// because a search for `pub` is a search for text.
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

/// Does this pattern ask a structural question in a lexical language? Deliberately narrow, a line
/// anchor plus a definition keyword: a suggestion that fires on every search mentioning `fn` is
/// noise, and noise in the notes is how the notes stop being read.
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

/// Name `outline` when it would actually have answered, and stay quiet otherwise: the pattern has to
/// be structural, and the files actually opened have to be in a language this build parses. It
/// suggests and does not run, because running a different tool is the model's decision.
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

/// Add a rung, or leave it off if its pattern will not compile. A rung is a relaxation, so it is
/// allowed to be unavailable, and a rung that silently searched for something else would be the
/// exact failure the compile error exists to prevent.
fn push_rung(ladder: &mut Vec<Attempt>, source: &str, ci: bool, scope: &str, relaxation: String) {
    if let Ok(pattern) = Pattern::compile(source, ci) {
        ladder.push(Attempt {
            pattern,
            scope: scope.to_string(),
            relaxation: Some(relaxation),
        });
    }
}

/// The engine's own words about the pattern, and what to do about it. `failed`, not `abstained`: an
/// abstention claims the content was looked at and did not contain the term, and this call opened no
/// files at all.
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

/// The largest file a search opens. A source file is kilobytes; past this it is a model shard,
/// a database, a tarball, and reading one whole spends minutes in the kernel. Skipped files are
/// counted and named in the result, so a search that missed something says so.
pub const FILE_CEILING: u64 = 16 * 1024 * 1024;

/// The most bytes one rung of a search reads in total. Past it the rung stops and says how many
/// files it never reached: a bounded answer that names its bound, not a daemon that stops
/// answering heads.
pub const BYTES_BUDGET: u64 = 512 * 1024 * 1024;

/// How many candidate files one [`crate::backend::ExecBackend::read_many`] asks for: `grep`'s page
/// and [`crate::builtins::outline`]'s, because the number belongs to the seam. A substrate whose
/// `read` is a round trip pays per file, so 64 paths a page turns hundreds into a handful.
pub const READ_PAGE_FILES: usize = 64;

/// The other bound on a page: 64 paths of 16 MiB each ([`FILE_CEILING`]) would be a gigabyte in
/// memory. A page stops growing at this many declared bytes but always takes its first file, so no
/// file is skipped for being the one that did not fit.
pub const READ_PAGE_BYTES: u64 = 16 * 1024 * 1024;

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
    /// The languages among the files opened that `outline` has a grammar for, collected here
    /// because it is the only place that knows which files were really read: a suggestion to use
    /// `outline` is only honest if it would have worked.
    languages: Vec<&'static str>,
    /// Candidates the walk offered, read or not: the second half of the denominator a stop needs.
    candidates: usize,
    /// Set when the scan stopped before the tree ran out. What it found is then part of an
    /// answer and never the whole of one.
    stop: Option<Stop>,
    /// What the reading itself said about how it ran - today, which `rg` answered and where. Empty
    /// for the in-process scan, which has no engine of its own to name.
    notes: Vec<String>,
}

/// The listings one call has walked, by scope. A rung that searches the same scope as the rung
/// above it reuses the walk: a walk is a round trip over every directory, and the ladder has four.
#[derive(Default)]
struct Listings {
    scope: Option<String>,
    entries: Vec<DirEntry>,
    ignored: Ignored,
}

impl Listings {
    /// The entries under `scope`, walked once. A `scope` naming a file is the file itself, and the
    /// stat that says so is asked only when the walk came back empty: over a channel every call is
    /// a round trip, and the common case is a directory.
    fn of(&mut self, ctx: &InvokeCtx<'_>, scope: &str) -> &[DirEntry] {
        if self.scope.as_deref() != Some(scope) {
            let (mut entries, _, ignored) = walk_ignoring(
                ctx.backend,
                scope,
                ctx.limits.max_walk_entries,
                &[scope],
                ctx.backend.home_path().as_deref(),
            );
            self.ignored = ignored;
            if entries.is_empty()
                && let Some(e) = ctx.backend.stat(scope)
                && !e.is_dir
            {
                entries = vec![e];
            }
            self.scope = Some(scope.to_string());
            self.entries = entries;
        }
        &self.entries
    }
}

fn search(
    ctx: &mut InvokeCtx<'_>,
    walked: &mut Listings,
    pattern: &Pattern,
    scope: &str,
    file_glob: Option<&str>,
    max: usize,
    context: usize,
) -> Scan {
    let entries = walked.of(ctx, scope);
    let files: Vec<&DirEntry> = entries
        .iter()
        .filter(|e| !e.is_dir)
        .filter(|e| file_glob.is_none_or(|g| glob_match(g, &e.path)))
        .collect();

    // Which candidates this rung could read at all: the size ceiling, then the byte budget, in
    // walk order. Both readings keep the same window, so the tallies a result reports about the
    // files it did not open do not depend on which one answered.
    let window = window(&files);
    let candidates = files.len();
    let mut scan = match pushed_down(ctx, pattern, scope, &files, &window, max, context) {
        Some(scan) => scan,
        None => read_and_match(ctx, pattern, &window, max, context),
    };
    scan.candidates = candidates;
    scan
}

/// The candidates a rung can read: everything at or under [`FILE_CEILING`], until
/// [`BYTES_BUDGET`] is spent. The tail the budget never reached is counted, not silently dropped.
struct Window<'a> {
    files: Vec<&'a DirEntry>,
    skipped_large: usize,
    unreached: usize,
}

fn window<'a>(files: &[&'a DirEntry]) -> Window<'a> {
    let mut kept: Vec<&'a DirEntry> = Vec::new();
    let mut skipped_large = 0usize;
    let mut unreached = 0usize;
    let mut read_bytes = 0u64;
    for (i, e) in files.iter().enumerate() {
        if e.bytes > FILE_CEILING {
            skipped_large += 1;
            continue;
        }
        if read_bytes >= BYTES_BUDGET {
            unreached = files.len() - i;
            break;
        }
        read_bytes += e.bytes;
        kept.push(e);
    }
    Window {
        files: kept,
        skipped_large,
        unreached,
    }
}

/// The search as `rg` runs it on the machine the backend reads, when that machine has one. `None`
/// is the in-process scan's turn: no `rg`, a backend that cannot run a command, a dialect that
/// refuses the pattern, or a run that failed. Nothing here is ever approximated - an answer this
/// cannot give exactly is an answer it does not give.
fn pushed_down(
    ctx: &mut InvokeCtx<'_>,
    pattern: &Pattern,
    scope: &str,
    files: &[&DirEntry],
    window: &Window<'_>,
    max: usize,
    context: usize,
) -> Option<Scan> {
    let tool = super::rg::arm(ctx.backend)?;
    if matches!(tool, super::rg::Tool::Absent) || window.files.is_empty() {
        return None;
    }
    let keep: std::collections::HashSet<&str> =
        window.files.iter().map(|e| e.path.as_str()).collect();
    // A directory the walk always skips is one rg need not open - unless a candidate sits under a
    // segment of that name, which is what naming the missing path in `path` does. The list is
    // `default_skip`'s; `.git` is rg's own skip as well, and is named here for the same rule.
    const SKIP: [&str; 6] = [
        ".git",
        "target",
        "node_modules",
        ".venv",
        "__pycache__",
        ".cache",
    ];
    let skip: Vec<&str> = SKIP
        .into_iter()
        .filter(|name| {
            !files
                .iter()
                .any(|e| e.path.split('/').any(|segment| segment == *name))
        })
        .collect();
    let question = super::rg::Question {
        scope,
        pattern: &pattern.source,
        case_insensitive: pattern.case_insensitive,
        context,
        max_matches: max,
        skip: &skip,
    };
    let found = super::rg::search(ctx.backend, &question, &keep, context).ok()?;
    // rg's per-file cap is the rung's own `max`, so a file that answered with exactly that many
    // may have had more. Its blocks arrive one file at a time, which is what makes the count
    // possible without asking anything else.
    let mut capped = false;
    let mut run = (String::new(), 0usize);
    let mut hits: Vec<Hit> = Vec::new();
    for f in found {
        if f.path != run.0 {
            run = (f.path.clone(), 0);
        }
        run.1 += 1;
        capped |= run.1 >= max.max(1);
        hits.push(Hit {
            path: f.path,
            line_no: f.line_no,
            line: f.line,
            region: f.region,
            span: f.span,
        });
    }
    let mut best = Best::new(max);
    best.offer(hits, capped);
    let (hits, truncated) = best.finish();
    let mut languages: Vec<&'static str> = Vec::new();
    for e in &window.files {
        if let Ok(l) = letibot_code::Language::of_path(&e.path)
            && !languages.contains(&l.name())
        {
            languages.push(l.name());
        }
    }
    let mut notes = Vec::new();
    if let Some(note) = super::rg::version_note(ctx.backend, &tool) {
        notes.push(note);
    }
    Some(Scan {
        hits,
        // Every file in the window was opened by rg. A read that fails is a thing this path
        // cannot see, and the page's own accounting is what the in-process scan would have gone
        // through to find out.
        scanned: window.files.len(),
        truncated,
        skipped_large: window.skipped_large,
        unreached: window.unreached,
        languages,
        candidates: files.len(),
        stop: None,
        notes,
    })
}

/// The in-process scan: the window's files, a page at a time, matched across the idle cores. The
/// page is one backend call where the substrate can do it; the matching is this process's work and
/// is the expensive half (`pattern`'s own notes measure it at 95% of a search).
fn read_and_match(
    ctx: &mut InvokeCtx<'_>,
    pattern: &Pattern,
    window: &Window<'_>,
    max: usize,
    context: usize,
) -> Scan {
    let files = &window.files;
    let mut best = Best::new(max);
    let mut scanned = 0usize;
    let mut languages: Vec<&'static str> = Vec::new();
    let mut reader = Reader::new();
    let mut i = 0usize;
    while i < files.len() {
        let mut page: Vec<usize> = Vec::new();
        let mut page_bytes = 0u64;
        while i < files.len() && page.len() < READ_PAGE_FILES {
            let e = files[i];
            if !page.is_empty() && page_bytes + e.bytes > READ_PAGE_BYTES {
                break;
            }
            if i > 0 && i.is_multiple_of(500) {
                // Progress is liveness: a walk over a large tree must produce it.
                ctx.progress(format!("scanned {i} of {} files", files.len()));
            }
            page_bytes += e.bytes;
            page.push(i);
            i += 1;
        }
        let paths: Vec<&str> = page.iter().map(|k| files[*k].path.as_str()).collect();
        // One call for the page where the backend can do it, and a BOUNDED retry when the page's
        // own call failed: a channel that answers nothing in ten seconds is not a slow channel.
        let got = reader.page(ctx, &paths);
        for (k, bytes) in page.iter().zip(&got) {
            if bytes.is_err() {
                continue;
            }
            // Opened, whether or not its bytes are text: a binary file is read and then left out
            // of the match, so the denominator counts it the same way rg's run does.
            scanned += 1;
            if let Ok(l) = letibot_code::Language::of_path(&files[*k].path)
                && !languages.contains(&l.name())
            {
                languages.push(l.name());
            }
        }
        let (page_hits, page_capped) = match_page(files, &page, &got, pattern, context, max);
        best.offer(page_hits, page_capped);
        // The cap ends the rung: what lies past it is an answer the model cannot see. Stopping at a
        // page's edge rather than inside one costs a page of reads, and keeps a search for a common
        // word as cheap as it was instead of reading a whole tree to cut it to the same size.
        if best.is_full() {
            break;
        }
        if reader.stop().is_some() {
            // What was never reached is the stop's own sentence: `unreached` means the byte
            // budget, and saying "stopped after reading 512 MiB" about a dying channel is a lie.
            break;
        }
    }
    let (hits, truncated) = best.finish();
    Scan {
        hits,
        scanned,
        truncated,
        skipped_large: window.skipped_large,
        unreached: window.unreached,
        languages,
        candidates: files.len(),
        stop: reader.stop().cloned(),
        notes: Vec::new(),
    }
}

/// How many threads a page is matched across. A cap, not a target: one `grep` should not take a
/// whole box, and the files in a page are 64 at most.
const MATCHERS: usize = 8;

/// One page's files, matched in parallel and put back in the page's own order, so the answer is
/// the same one whatever the cores did with it.
fn match_page(
    files: &[&DirEntry],
    page: &[usize],
    got: &[Result<Vec<u8>, BackendError>],
    pattern: &Pattern,
    context: usize,
    max: usize,
) -> (Vec<Hit>, bool) {
    let width = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .clamp(1, MATCHERS);
    let chunk = page.len().div_ceil(width).max(1);
    if width == 1 || page.len() <= 1 {
        return match_slice(files, page, got, pattern, context, max, 0..page.len());
    }
    let parts: Vec<(Vec<Hit>, bool)> = std::thread::scope(|s| {
        let mut handles = Vec::new();
        for k in 0..width {
            let (a, b) = (k * chunk, ((k + 1) * chunk).min(page.len()));
            if a >= b {
                break;
            }
            handles
                .push(s.spawn(move || match_slice(files, page, got, pattern, context, max, a..b)));
        }
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_default())
            .collect()
    });
    let mut capped = false;
    let mut hits = Vec::new();
    for (part, part_capped) in parts {
        capped |= part_capped;
        hits.extend(part);
    }
    (hits, capped)
}

/// `range` of a page, matched file by file. A file's cap is the rung's own `max`: a file that had
/// more matches than the whole answer could not contribute anything past them.
fn match_slice(
    files: &[&DirEntry],
    page: &[usize],
    got: &[Result<Vec<u8>, BackendError>],
    pattern: &Pattern,
    context: usize,
    max: usize,
    range: std::ops::Range<usize>,
) -> (Vec<Hit>, bool) {
    let mut hits = Vec::new();
    let mut capped = false;
    for k in range {
        let e = files[page[k]];
        let Ok(bytes) = &got[k] else {
            continue;
        };
        if bytes.contains(&0) {
            // Binary. Skipped rather than rendered as replacement characters.
            continue;
        }
        let (text, _) = text_of(bytes);
        // One scan of the whole buffer rather than one engine call per line: `line_hits`
        // confirms every candidate against the line-scoped program, so the semantics are the
        // same and only the cost differs.
        let path = e.path.clone();
        // Only when asked: collecting a file's lines is cheap but not free, and the common call
        // passes no context at all.
        let file_lines: Vec<&str> = if context > 0 {
            text.lines().collect()
        } else {
            Vec::new()
        };
        let truncated = pattern.line_hits(&text, max, |line_no, line| {
            let region = if context > 0 {
                let first = line_no.saturating_sub(context).max(1);
                let last = (line_no + context).min(file_lines.len());
                (first..=last)
                    .map(|n| (n, file_lines[n - 1].to_string()))
                    .collect()
            } else {
                Vec::new()
            };
            // Where in the line the match is, which is what centres the window a very long line is
            // shown through; rg says the same offsets for the same line.
            let span = pattern.find(line);
            hits.push(Hit {
                path: path.clone(),
                line_no,
                line: line.to_string(),
                region,
                span,
            });
        });
        capped |= truncated;
    }
    (hits, capped)
}

/// The answer's first `max` hits by `(path, line)`. Both readings answer in this order, so the
/// result does not depend on which files a substrate happened to read first, and a cap then cuts
/// the same hits wherever the search ran.
///
/// A CAPPED answer is the one place the two can disagree about WHICH hits they keep: the in-process
/// scan stops a page after it has enough (see `read_and_match`), so a tree of more than one page can
/// hand it a different `max` than a host's `rg` saw. The count, and the note about the cap, are the
/// same either way, and both are true of a tree the model asked to truncate.
struct Best {
    max: usize,
    hits: Vec<Hit>,
    seen: usize,
    capped: bool,
}

impl Best {
    fn new(max: usize) -> Self {
        Best {
            max: max.max(1),
            hits: Vec::new(),
            seen: 0,
            capped: false,
        }
    }

    /// `capped` is the reading's own word for "it stopped counting hits in this file".
    fn offer(&mut self, hits: Vec<Hit>, capped: bool) {
        self.capped |= capped;
        for hit in hits {
            self.seen += 1;
            if self.hits.len() >= self.max && !before(&hit, &self.hits[self.max - 1]) {
                continue;
            }
            let at = self
                .hits
                .binary_search_by(|h| order(h, &hit))
                .unwrap_or_else(|e| e);
            self.hits.insert(at, hit);
            self.hits.truncate(self.max);
        }
    }

    fn finish(self) -> (Vec<Hit>, bool) {
        let truncated = self.capped || self.seen > self.max;
        (self.hits, truncated)
    }

    /// Has the answer reached its cap, so that a reading may stop looking?
    fn is_full(&self) -> bool {
        self.seen >= self.max
    }
}

fn order(a: &Hit, b: &Hit) -> std::cmp::Ordering {
    (a.path.as_str(), a.line_no).cmp(&(b.path.as_str(), b.line_no))
}

fn before(a: &Hit, b: &Hit) -> bool {
    order(a, b) == std::cmp::Ordering::Less
}

/// What a scan that stopped on its bound or a dying channel returns: every hit it did find, the
/// sentence that says what stopped it, and what to do instead. Never silence, and never a miss -
/// the files it never opened are where the answer might be.
fn stopped(scan: &Scan, stop: &Stop, scope: &str, bound: std::time::Duration) -> Invocation {
    let said = stop.sentence(scan.scanned, scan.candidates);
    let mut body = if scan.hits.is_empty() {
        format!(
            "no match(es) among the {} file(s) opened under `{scope}`.\n",
            scan.scanned
        )
    } else {
        render_hits(&scan.hits, scope)
    };
    body.push_str(&format!("\nTHE SEARCH DID NOT FINISH. {said}.\n"));
    body.push_str(
        "What is above is real; what is missing was never looked at, so a miss here says \
         nothing about the rest of the tree. Narrow `path` (one directory), add `glob`, or \
         search for a rarer term, and the smaller tree finishes.",
    );
    match stop {
        Stop::Time => Invocation::timed_out(body).with_note(format!(
            "this call was cancelled at its wall-clock bound ({bound:?}) rather than left to hold \
             the turn; ask again on a narrower scope."
        )),
        Stop::Channel(why) => {
            Invocation::failed(format!("the search could not read its tree: {why}"), body)
        }
    }
}

/// The files a rung did not open, as a note, or nothing when it opened them all.
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
            out.push_str(&format!(
                "{}:{}: {}\n",
                h.path,
                h.line_no,
                shown_line(&h.line, h.span, Some(h.line_no))
            ));
        }
        return out;
    }
    // With context, the answer is regions rather than lines: numbered, so a location can be cited and
    // an edit made from it without a second look. Overlapping regions are printed once, and `--`
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
            // through to find what was asked about - and the match line goes through the same
            // window as a hit without context, since it is the line the search is about.
            let mark = if *n == h.line_no { ':' } else { '-' };
            let shown = if *n == h.line_no {
                shown_line(text, h.span, Some(*n))
            } else {
                clip_line_counted(text, HIT_CAP)
            };
            out.push_str(&format!("{}:{n}{mark} {shown}\n", h.path));
            last = Some((&h.path, *n));
        }
    }
    out
}

/// Where the term does occur, by file, with counts: the answer a scoped miss needs.
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

/// A hit line is shown up to this many characters; the full line is one `read` away.
const HIT_CAP: usize = 400;

/// Where a long line is shown from: characters `a..b` of it, and how many it has. `None` when the
/// whole line fits.
fn window_of(line: &str, span: Option<(usize, usize)>) -> Option<(usize, usize, usize)> {
    let total = line.chars().count();
    if total <= HIT_CAP {
        return None;
    }
    // Centred on the match, half a window either side, so what was asked about is what is read and
    // not a prefix of the line that happens to come first. A match longer than the window is cut.
    let column = match span {
        Some((start, _)) => line
            .get(..crate::edit::floor_boundary(line, start))
            .map_or(0, |pre| pre.chars().count()),
        None => 0,
    };
    let a = column.saturating_sub(HIT_CAP / 2);
    let b = (a + HIT_CAP).min(total);
    Some((a, b, total))
}

/// Byte offset of character `at` of `line`.
fn byte_of(line: &str, at: usize) -> usize {
    line.char_indices().nth(at).map_or(line.len(), |(i, _)| i)
}

/// One hit line as the model reads it: whole when it fits, and otherwise the window around the
/// match, saying which characters of how many it is and how to read the rest. `line_no` names the
/// line for that `read`; `None` is a context line, which is shown from its start as it was before.
fn shown_line(line: &str, span: Option<(usize, usize)>, line_no: Option<usize>) -> String {
    let Some((a, b, total)) = window_of(line, span) else {
        return clip_line_counted(line, HIT_CAP);
    };
    let from = byte_of(line, a);
    let to = byte_of(line, b);
    let rest = match line_no {
        Some(n) => format!(
            " — characters {a}-{b} of {total}; `read` offset={n} column_offset={b} for the \
             rest, and an `edit` inside these characters is allowed"
        ),
        None => format!(" — characters {a}-{b} of {total}"),
    };
    let lead = if a > 0 { "…" } else { "" };
    let tail = if b < total { "…" } else { "" };
    format!("{lead}{}{tail}{rest}", &line[from..to])
}

/// Whether the output carried this line whole. A clipped line showed the model a prefix, so the
/// shown-lines ledger must not mark it.
fn shown_in_full(line: &str) -> bool {
    line.chars().count() <= HIT_CAP
}

/// Mark the hit and context lines this call rendered, for `edit`'s gate. Only the rung whose
/// hits were rendered is marked; rungs that found nothing showed the model nothing.
fn mark_shown(ctx: &mut InvokeCtx<'_>, hits: &[Hit]) {
    let mut by_path: std::collections::BTreeMap<&str, Vec<(usize, &str)>> =
        std::collections::BTreeMap::new();
    for h in hits {
        match window_of(&h.line, h.span) {
            // A long line was shown as one window of it: that window, and only it, may be edited
            // from this call.
            Some((a, b, _)) => ctx.files.mark_line_part(
                &h.path,
                h.line_no,
                &h.line,
                byte_of(&h.line, a),
                byte_of(&h.line, b),
            ),
            None => by_path
                .entry(h.path.as_str())
                .or_default()
                .push((h.line_no, h.line.as_str())),
        }
        for (n, text) in &h.region {
            if shown_in_full(text) {
                by_path
                    .entry(h.path.as_str())
                    .or_default()
                    .push((*n, text.as_str()));
            }
        }
    }
    for (path, lines) in by_path {
        ctx.files.mark_hits(path, &lines);
    }
}

#[cfg(test)]
mod tests {
    use crate::testing::harness;

    /// A gitignored tree is not searched from the root, the miss counts what it skipped, and
    /// naming the path searches it.
    #[test]
    fn gitignored_files_are_skipped_counted_and_reachable_by_path() {
        let mut h = harness();
        h.write_file(".gitignore", "gen/\n*.log\n");
        h.write_file("gen/out.rs", "quokka_generated\n");
        h.write_file("gen/more.rs", "quokka_generated\n");
        h.write_file("run.log", "quokka_generated\n");
        h.write_file(".hidden/note.txt", "hidden_sentinel\n");

        let r = h.call("grep", r#"{"pattern":"quokka_generated"}"#);
        assert!(!r.is_grounded(), "{}", r.payload);
        assert!(!r.payload.contains("gen/out.rs:"), "{}", r.payload);
        assert!(
            r.payload
                .contains("3 file(s) ignored by git's ignore rules were not searched"),
            "{}",
            r.payload
        );

        let r = h.call("grep", r#"{"pattern":"quokka_generated","path":"gen"}"#);
        assert!(r.is_grounded(), "{}", r.payload);
        assert!(r.payload.contains("gen/out.rs:"), "{}", r.payload);
        let r = h.call("grep", r#"{"pattern":"quokka_generated","path":"run.log"}"#);
        assert!(r.payload.contains("run.log:"), "{}", r.payload);

        // Hidden is not ignored.
        let r = h.call("grep", r#"{"pattern":"hidden_sentinel"}"#);
        assert!(r.payload.contains(".hidden/note.txt:"), "{}", r.payload);
    }

    #[test]
    fn a_plain_hit_is_path_line_text() {
        let mut h = harness();
        let r = h.call("grep", r#"{"pattern":"TokenLedger"}"#);
        assert!(r.is_grounded());
        assert!(r.payload.contains("src/lib.rs:"), "{}", r.payload);
    }

    #[test]
    fn nothing_under_the_scoped_path_reports_where_it_does_occur() {
        // A redirect is a result: nothing matched under the asked scope, and the term is
        // elsewhere in the tree.
        let mut h = harness();
        let r = h.call("grep", r#"{"pattern":"cache_prompt","path":"src"}"#);
        assert!(r.is_grounded(), "a redirect is a result: {:?}", r.outcome);
        let notes = r.notes.join(" ");
        assert!(notes.contains("docs/notes.md"), "{notes}");
        assert!(r.payload.contains("docs/notes.md:"), "{}", r.payload);
    }

    #[test]
    fn a_too_strict_anchor_is_relaxed_to_the_bare_identifier_and_says_so() {
        // The source has `pub fn parse_args()`, so `^\s*fn parse_args\(` is exactly the guess
        // that misses.
        let mut h = harness();
        let r = h.call("grep", r#"{"pattern":"^\\s*fn parse_args\\("}"#);
        assert!(r.is_grounded(), "{:?}", r.outcome);
        let notes = r.notes.join(" ");
        assert!(notes.contains("bare identifier"), "{notes}");
        assert!(notes.contains("parse_args"), "{notes}");
        assert!(r.payload.contains("src/lib.rs:"), "{}", r.payload);
    }

    /// `#\[cfg\(test\)\]` with `glob: "*.rs"` under `src` must not answer "does not occur in the
    /// searched tree" about a tree holding nine. A call that opened no files must not abstain,
    /// because an abstention is a claim about content.
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
        // The assertion that actually distinguishes the fix: without it the scope yielded nothing
        // and the LADDER rescued the call by re-searching the whole session root, same hits and
        // same payload, so a passing test proves nothing. The fix changes a DIRECT search.
        let notes = r.notes.join(" ");
        assert!(
            !notes.contains("whole session root"),
            "the file scope must be searched directly, not rescued by the fallback \
             rung:\n{notes}"
        );
    }

    /// Assert the FACT (what is rendered) and scope it to the defect's shape, a run of spaces BETWEEN
    /// WORDS: source-level scanning is unreliable, since a correct continuation looks the same to a
    /// naive scan. The regex engine's diagnostic aligns a caret with leading spaces and keeps them.
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
        // The file count and the miss once each: the reason says them, the body does not.
        assert_eq!(rendered.matches("file(s)").count(), 1, "{rendered}");
        assert!(!rendered.contains("not in the searched tree"), "{rendered}");
    }

    /// A pattern that does not compile is reported and nothing is searched. Treating it as a
    /// literal would tell the model two true things and one false one: that `(?<=fn )\w+` does
    /// not occur.
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

    /// The other half: the syntax the old subset refused now works, and is not reported as an
    /// approximation.
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

    /// A pattern big enough to be a denial of service is refused in the same shape as a syntax
    /// error, rather than compiled while a turn waits.
    #[test]
    fn a_pattern_over_the_size_limit_is_refused_with_a_remedy() {
        let mut h = harness();
        let r = h.call("grep", r#"{"pattern":"((((a{100}){100}){100}){100})"}"#);
        assert!(!r.is_grounded());
        let seen = r.render();
        assert!(seen.contains("KiB"), "{seen}");
    }

    /// A structural pattern that matches poorly must name the tool that answers it structurally:
    /// relaxing a structural query lexically can only miss.
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

    /// Honesty is the whole value of the suggestion: a lexical search gets none however badly it
    /// misses, and neither does a structural one over files nothing here can parse.
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

    /// A file past [`FILE_CEILING`] is never opened, and the result says so: a search that
    /// silently skipped a file would report an absence it did not establish. The file is created
    /// SPARSE, so the test costs a metadata call, and its name is a candidate for the search.
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

    /// Find and see, in one call. Without `context`, `grep` names a line and a second call has to
    /// read around it, which is the `grep -n` into `sed -n 'A,Bp'` loop done with two tools; the
    /// shell won on round trips.
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
        // The match is still there, and now so is what surrounds it, numbered, so the next call
        // can be an `edit` rather than another look.
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

#[cfg(test)]
mod paging_tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use letibot_transcript::ToolCall;

    use crate::backend::{
        BackendError, Command, DirEntry, ExecBackend, HostBackend, Output, tempdir::TempDir,
    };
    use crate::builtins::retrieval::Unavailable;
    use crate::events::RecordingToolSink;
    use crate::runtime::ToolRuntime;

    /// A repository-sized candidate list, because the defect is about the count of round trips
    /// and a four-file fixture cannot show it.
    const FILES: usize = 200;

    /// The file that matches, so the ladder stops at the rung the model asked for and
    /// the call count measures the search rather than three relaxations of it.
    const HIT: usize = 7;

    fn tree(dir: &std::path::Path) {
        for i in 0..FILES {
            let body = if i == HIT {
                format!("first line\nneedle_here on line two of {i}\nthird line\n")
            } else {
                format!("filler {i}\nnothing to see here\n")
            };
            std::fs::write(dir.join(format!("f{i:03}.txt")), body).expect("fixture file");
        }
    }

    #[derive(Default)]
    struct Counts {
        /// How many times the backend was asked for ONE file.
        reads: AtomicUsize,
        /// How many times it was asked for a PAGE.
        batches: AtomicUsize,
    }

    /// A backend over a real tree that counts how it was asked, and can be told to behave like a
    /// substrate whose `read` is a round trip (`batched`), to lose one file inside a page
    /// (`vanish`), or to fail the batch call itself (`fail`).
    struct Counting {
        inner: HostBackend,
        batched: bool,
        vanish: Option<String>,
        fail: bool,
        counts: Arc<Counts>,
    }

    impl ExecBackend for Counting {
        fn run(&self, cmd: &Command) -> Result<Output, BackendError> {
            self.inner.run(cmd)
        }
        fn read(&self, path: &str) -> Result<Vec<u8>, BackendError> {
            self.counts.reads.fetch_add(1, Ordering::SeqCst);
            self.inner.read(path)
        }
        fn write(&self, path: &str, bytes: &[u8]) -> Result<(), BackendError> {
            self.inner.write(path, bytes)
        }
        fn list(&self, path: &str) -> Result<Vec<DirEntry>, BackendError> {
            self.inner.list(path)
        }
        fn stat(&self, path: &str) -> Option<DirEntry> {
            self.inner.stat(path)
        }
        fn describe(&self) -> String {
            self.inner.describe()
        }
        fn processes_arc(&self) -> Option<Arc<dyn crate::exec::ProcessHost>> {
            None
        }
        fn read_many(
            &self,
            paths: &[&str],
        ) -> Result<Vec<Result<Vec<u8>, BackendError>>, BackendError> {
            if !self.batched {
                // The trait's default, spelled out: one `read` per path.
                return Ok(paths.iter().map(|p| self.read(p)).collect());
            }
            self.counts.batches.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                return Err(BackendError::Io("the VM went away mid-page".into()));
            }
            Ok(paths
                .iter()
                .map(|p| match &self.vanish {
                    // A candidate the walk listed and the read no longer finds.
                    Some(v) if v == p => Err(BackendError::NotFound((*p).to_string())),
                    _ => self.inner.read(p),
                })
                .collect())
        }
    }

    /// One `grep` call over the tree, and the counts it cost.
    fn grep_over(
        batched: bool,
        vanish: Option<usize>,
        fail: bool,
        args: &str,
    ) -> (String, usize, usize) {
        let dir = TempDir::new();
        tree(dir.path());
        let counts = Arc::new(Counts::default());
        let backend = Counting {
            inner: HostBackend::new(dir.path()).expect("fixture root"),
            batched,
            vanish: vanish.map(|i| format!("f{i:03}.txt")),
            fail,
            counts: Arc::clone(&counts),
        };
        let registry = crate::read_only_tools(Arc::new(Unavailable)).expect("built-ins register");
        let rt = ToolRuntime::new(registry, Box::new(backend));
        let mut sink = RecordingToolSink::new();
        let call = ToolCall {
            id: "call_1".into(),
            name: "grep".into(),
            arguments: args.to_string(),
        };
        let rendered = rt.invoke("turn_1", &call, &mut sink).render();
        (
            rendered,
            counts.reads.load(Ordering::SeqCst),
            counts.batches.load(Ordering::SeqCst),
        )
    }

    const ASK: &str = r#"{"pattern":"needle_here"}"#;

    /// A repository-sized candidate list costs one round trip per FILE without the batched read and
    /// one per PAGE with it. Both numbers are asserted, because either alone proves nothing: 0 batch
    /// calls is a backend that ignored the seam, one read per file a paging loop.
    #[test]
    fn a_page_of_files_is_one_call_and_a_file_is_one_call() {
        let (one_at_a_time, reads, batches) = grep_over(false, None, false, ASK);
        assert_eq!(
            (reads, batches),
            (FILES, 0),
            "one `read` per candidate file, and no page asked for"
        );
        assert!(
            one_at_a_time.contains("needle_here"),
            "the search found nothing to compare:\n{one_at_a_time}"
        );

        let (paged, reads, batches) = grep_over(true, None, false, ASK);
        let pages = FILES.div_ceil(super::READ_PAGE_FILES);
        assert_eq!(
            (reads, batches),
            (0, pages),
            "one call per page of {} files ({FILES} files, {pages} pages) and none per file",
            super::READ_PAGE_FILES
        );
        assert_eq!(
            paged, one_at_a_time,
            "the paged search and the one-at-a-time search must render the same result"
        );
    }

    /// A page's batch call failing loses no match: the page is retried one file at a time, and
    /// the result is the one the per-file path produced.
    #[test]
    fn a_failed_page_falls_back_to_one_file_at_a_time() {
        let (reference, _, _) = grep_over(false, None, false, ASK);
        let (fell_back, reads, batches) = grep_over(true, None, true, ASK);
        assert_eq!(fell_back, reference, "the fallback lost or changed a match");
        assert_eq!(batches, FILES.div_ceil(super::READ_PAGE_FILES));
        assert_eq!(
            reads, FILES,
            "every file was re-read one at a time after the page failed"
        );
    }

    /// How the files were read cannot be told from the answer: the whole rendered result is the same
    /// either way for every shape the tool has. Every shape goes through the same paging loop, so a
    /// boundary that changed the ordering, the caps or the truncation would show up here.
    #[test]
    fn the_answer_does_not_depend_on_how_the_files_were_read() {
        let shapes = [
            ASK,
            r#"{"pattern":"needle_here","context":2}"#,
            r#"{"pattern":"line","glob":"f00*.txt"}"#,
            r#"{"pattern":"filler","max_matches":3}"#,
            r#"{"pattern":"quokka_sentinel"}"#,
        ];
        for args in shapes {
            let (one_at_a_time, _, _) = grep_over(false, None, false, args);
            let (paged, _, _) = grep_over(true, None, false, args);
            assert_eq!(
                paged, one_at_a_time,
                "the paged search answered differently for {args}"
            );
        }
    }

    /// A file that is gone is skipped, exactly as it was: the rest of the page's matches still
    /// come back, and nothing says the search failed.
    #[test]
    fn a_file_that_vanished_inside_a_page_is_skipped_and_not_the_page() {
        let (reference, _, _) = grep_over(false, None, false, ASK);
        // The matching file is the one that vanished: the answer is the honest miss,
        // not an error, and not a hit from a page that gave up.
        let (gone_hit, _, _) = grep_over(true, Some(HIT), false, ASK);
        assert!(
            !gone_hit.contains("needle_here on line two"),
            "a file that could not be read was searched anyway:\n{gone_hit}"
        );
        // And the miss is honest about its denominator: 199 files were opened, not 200, so the
        // file the page could not read is not claimed as searched.
        assert!(
            gone_hit.contains("199 file(s)"),
            "an unreadable candidate was counted as searched:\n{gone_hit}"
        );
        // And a file that vanishes AROUND the hit does not cost the hit.
        let (other_gone, _, _) = grep_over(true, Some(0), false, ASK);
        assert_eq!(
            other_gone, reference,
            "an unreadable neighbour changed the answer"
        );
    }
}

#[cfg(test)]
mod pushdown_tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use letibot_transcript::ToolCall;

    use crate::backend::{
        BackendError, Command, DirEntry, ExecBackend, HostBackend, Output, tempdir::TempDir,
    };
    use crate::builtins::retrieval::Unavailable;
    use crate::events::RecordingToolSink;
    use crate::result::ToolResult;
    use crate::runtime::ToolRuntime;

    use super::FILE_CEILING;

    /// Whether this box can run `rg`. The pushdown is about what a host that HAS it answers, so a
    /// box that has never had one skips loudly instead of failing.
    fn rg_here() -> bool {
        std::process::Command::new("rg")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
    }

    macro_rules! need_rg {
        () => {
            if !rg_here() {
                eprintln!(
                    "SKIPPED: this box has no `rg`, so the pushdown half never ran; the in-process \
                     half is covered by the tests that do not need it"
                );
                return;
            }
        };
    }

    /// What a backend's `run` does.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Loud {
        /// The real host command.
        Real,
        /// A host whose PATH has no `rg`: the probe answers 127.
        NoRg,
        /// A backend that cannot run a command at all, which is what THIS box's own seat backend
        /// is: no cgroup, so no process to own a command with.
        Refuse,
    }

    impl Loud {
        fn label(self) -> &'static str {
            match self {
                Loud::Real => "real",
                Loud::NoRg => "no-rg",
                Loud::Refuse => "refuse",
            }
        }
    }

    /// A backend over a real tree that CAN run a command: a remote host or a VM, which is where the
    /// pages this tool used to pay for were round trips. `describe` carries a serial so two of them
    /// never share a probe: the probe cache is keyed by the backend's identity.
    struct Run {
        inner: HostBackend,
        mode: Loud,
        home: String,
        serial: usize,
        runs: Arc<AtomicUsize>,
    }

    static SERIAL: AtomicUsize = AtomicUsize::new(0);

    impl ExecBackend for Run {
        fn run(&self, cmd: &Command) -> Result<Output, BackendError> {
            self.runs.fetch_add(1, Ordering::SeqCst);
            match self.mode {
                Loud::Refuse => Err(BackendError::Unsupported(
                    "this session's backend cannot start processes",
                )),
                Loud::NoRg => Ok(Output {
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                    exit: 127,
                }),
                Loud::Real => {
                    let root = self.inner.root_path().unwrap_or_else(|| ".".into());
                    let out = std::process::Command::new(&cmd.argv[0])
                        .args(&cmd.argv[1..])
                        .current_dir(root)
                        .envs(cmd.env.iter().cloned())
                        .output()
                        .map_err(|e| BackendError::Io(e.to_string()))?;
                    Ok(Output {
                        stdout: out.stdout,
                        stderr: out.stderr,
                        exit: out.status.code().unwrap_or(-1),
                    })
                }
            }
        }
        fn read(&self, path: &str) -> Result<Vec<u8>, BackendError> {
            self.inner.read(path)
        }
        fn write(&self, path: &str, bytes: &[u8]) -> Result<(), BackendError> {
            self.inner.write(path, bytes)
        }
        fn list(&self, path: &str) -> Result<Vec<DirEntry>, BackendError> {
            self.inner.list(path)
        }
        fn stat(&self, path: &str) -> Option<DirEntry> {
            self.inner.stat(path)
        }
        fn describe(&self) -> String {
            format!("run/{}#{}", self.mode.label(), self.serial)
        }
        fn processes_arc(&self) -> Option<Arc<dyn crate::exec::ProcessHost>> {
            None
        }
        fn home_path(&self) -> Option<String> {
            Some(self.home.clone())
        }
        fn workspace_path(&self) -> Option<String> {
            self.inner.workspace_path()
        }
        fn root_path(&self) -> Option<String> {
            self.inner.root_path()
        }
    }

    /// A fixture tree holding every shape the two readings have to agree about: a gitignored tree,
    /// the repository's own exclude file, the operator's global excludes file, a hidden file, a
    /// binary file, one over the ceiling, context lines, CRLF, Cyrillic/CJK/emoji, and a line far
    /// longer than a hit shows.
    struct Tree(TempDir);

    impl Tree {
        fn new() -> Tree {
            let dir = TempDir::new();
            let root = dir.path();
            let put = |p: &str, body: &[u8]| {
                let f = root.join(p);
                std::fs::create_dir_all(f.parent().expect("a parent")).expect("fixture dir");
                std::fs::write(f, body).expect("fixture file");
            };
            put(".gitignore", b"gen/\n*.log\n");
            put(".git/info/exclude", b"*.skipme\n");
            // The session's home is part of the tree, so the global excludes file is a path the
            // backend can read: `<home>/.config/git/ignore`.
            put("home/.config/git/ignore", b"*.gen\n");
            put("gen/out.rs", b"quokka_gen\n");
            put("run.log", b"quokka_log\n");
            put("dropped.skipme", b"quokka_skipme\n");
            put("dropped.gen", b"quokka_global\n");
            put(".hidden/note.txt", b"quokka_hidden\n");
            put("blob.bin", b"quokka_bin\0\x01\x02");
            let big = std::fs::File::create(root.join("big.rs")).expect("sparse file");
            big.set_len(FILE_CEILING + 1).expect("set_len");
            put("crlf.txt", "one\r\nquokka crlf\r\nthree\r\n".as_bytes());
            put(
                "uni.txt",
                "привет quokka\r\n世界 quokka\r\n🦀 quokka\r\n".as_bytes(),
            );
            put("ctx.txt", b"a\nb\nquokka line4\nc\nd\n");
            let long = format!(
                "head {} quokka_sentinel {}",
                "x".repeat(1200),
                "y".repeat(1200)
            );
            put("long.txt", format!("{long}\ntail\n").as_bytes());
            put("src/lib.rs", b"pub fn parse_args() {}\n// quokka\n");
            Tree(dir)
        }

        fn runtime(&self, mode: Loud) -> (ToolRuntime, Arc<AtomicUsize>) {
            let runs = Arc::new(AtomicUsize::new(0));
            let backend = Run {
                inner: HostBackend::writable(self.0.path()).expect("fixture root"),
                mode,
                home: "home".to_string(),
                serial: SERIAL.fetch_add(1, Ordering::SeqCst),
                runs: Arc::clone(&runs),
            };
            let registry =
                crate::read_only_tools(Arc::new(Unavailable)).expect("built-ins register");
            (ToolRuntime::new(registry, Box::new(backend)), runs)
        }

        fn ask(&self, mode: Loud, args: &str) -> ToolResult {
            let (rt, _) = self.runtime(mode);
            call(rt, args)
        }

        fn both(&self, args: &str) -> (ToolResult, ToolResult) {
            (self.ask(Loud::NoRg, args), self.ask(Loud::Real, args))
        }
    }

    fn call(rt: ToolRuntime, args: &str) -> ToolResult {
        let mut sink = RecordingToolSink::new();
        let call = ToolCall {
            id: "call_1".into(),
            name: "grep".into(),
            arguments: args.to_string(),
        };
        rt.invoke("turn_1", &call, &mut sink)
    }

    /// The one note that names the engine: it is the evidence the pushdown ran, so it is compared
    /// apart from the notes both readings must share.
    fn engine_notes(r: &ToolResult) -> Vec<String> {
        r.notes
            .iter()
            .filter(|n| n.contains("on that host"))
            .cloned()
            .collect()
    }

    fn other_notes(r: &ToolResult) -> Vec<String> {
        r.notes
            .iter()
            .filter(|n| !n.contains("on that host"))
            .cloned()
            .collect()
    }

    /// The same call, answered by `rg` on the host and by this process, renders the same hits and
    /// says the same things about them. A substrate may change what a search costs; it may not
    /// change what it says.
    #[test]
    fn a_pushed_down_search_answers_exactly_as_the_in_process_one() {
        need_rg!();
        let t = Tree::new();
        for args in [
            r#"{"pattern":"quokka"}"#,
            r#"{"pattern":"quokka","context":1}"#,
            r#"{"pattern":"quokka","glob":"src/**"}"#,
            r#"{"pattern":"QUOKKA","case_insensitive":true}"#,
            r#"{"pattern":"quokka crlf$"}"#,
            r#"{"pattern":"привет|世界|🦀"}"#,
            r#"{"pattern":"quokka","max_matches":3}"#,
            r#"{"pattern":"quokka_sentinel"}"#,
            r#"{"pattern":"quokka_gen","path":"gen"}"#,
            r#"{"pattern":"quokka_skipme","path":"dropped.skipme"}"#,
            r#"{"pattern":"quokka_global","path":"dropped.gen"}"#,
            r#"{"pattern":"quokka_nothing_at_all"}"#,
        ] {
            let (fallback, pushed) = t.both(args);
            assert!(
                engine_notes(&fallback).is_empty(),
                "the reference ran an engine for {args}"
            );
            assert_eq!(
                engine_notes(&pushed).len(),
                1,
                "the pushdown did not run for {args}:\n{}",
                pushed.payload
            );
            assert_eq!(
                pushed.payload, fallback.payload,
                "the two readings answered differently for {args}"
            );
            assert_eq!(
                other_notes(&pushed),
                other_notes(&fallback),
                "the two readings said different things about {args}"
            );
        }
    }

    /// Asked once per backend, not once per rung: the probe is the round trip the pushdown exists
    /// to save. The version is said in the notes the first time a result carries it, and not again.
    #[test]
    fn the_probe_is_asked_once_and_the_version_is_said_once() {
        need_rg!();
        let t = Tree::new();
        let (rt, runs) = t.runtime(Loud::Real);
        let mut sink = RecordingToolSink::new();
        let ask = |rt: &ToolRuntime, sink: &mut RecordingToolSink| {
            let call = ToolCall {
                id: "call_1".into(),
                name: "grep".into(),
                arguments: r#"{"pattern":"quokka_hidden"}"#.into(),
            };
            rt.invoke("turn_1", &call, sink)
        };
        let first = ask(&rt, &mut sink);
        let after_first = runs.load(Ordering::SeqCst);
        let second = ask(&rt, &mut sink);
        assert_eq!(
            engine_notes(&first).len(),
            1,
            "the first result did not name the engine:\n{}",
            first.payload
        );
        assert!(
            engine_notes(&second).is_empty(),
            "the engine was named twice:\n{}",
            second.payload
        );
        assert_eq!(
            runs.load(Ordering::SeqCst) - after_first,
            1,
            "the second call probed again, so the answer was not cached"
        );
    }

    /// A host without `rg`, and a backend with no way to run anything, both answer with this
    /// process's own scan - and the answer is the same one, with no engine named.
    #[test]
    fn a_host_without_rg_answers_from_this_process() {
        let t = Tree::new();
        for args in [
            r#"{"pattern":"quokka"}"#,
            r#"{"pattern":"quokka","context":1}"#,
        ] {
            let absent = t.ask(Loud::NoRg, args);
            let unstartable = t.ask(Loud::Refuse, args);
            let real = t.ask(Loud::Real, args);
            assert!(engine_notes(&absent).is_empty(), "{args}");
            assert!(engine_notes(&unstartable).is_empty(), "{args}");
            assert_eq!(absent.payload, unstartable.payload, "{args}");
            assert_eq!(
                absent.payload, real.payload,
                "the pushed-down answer differs from the one this process gives for {args}"
            );
        }
    }

    /// rg's dialect has no newline in a pattern. The tree's engine compiles one, so that rung is
    /// answered here rather than by a guess at what rg meant.
    #[test]
    fn a_pattern_rg_cannot_express_is_refused_rather_than_guessed() {
        need_rg!();
        use std::collections::HashSet;

        let t = Tree::new();
        let backend = Run {
            inner: HostBackend::writable(t.0.path()).expect("fixture root"),
            mode: Loud::Real,
            home: "home".to_string(),
            serial: SERIAL.fetch_add(1, Ordering::SeqCst),
            runs: Arc::new(AtomicUsize::new(0)),
        };
        let keep: HashSet<&str> = ["src/lib.rs"].into_iter().collect();
        let question = super::super::rg::Question {
            scope: ".",
            pattern: "quokka\\n",
            case_insensitive: false,
            context: 0,
            max_matches: 5,
            skip: &[],
        };
        match super::super::rg::search(&backend, &question, &keep, 0) {
            Err(super::super::rg::Refusal::Said(why)) => {
                assert!(why.contains("rg exited 2"), "{why}");
            }
            Err(other) => panic!("rg did not refuse a pattern with a newline: {other:?}"),
            Ok(found) => panic!(
                "rg answered {} hit(s) for a pattern it cannot match",
                found.len()
            ),
        }
    }
}

#[cfg(test)]
mod long_line_tests {
    use crate::testing::writable_harness;

    /// A hit on a line of 2 500 characters used to be cut at 400 characters with nothing marked as
    /// shown, so the model could read a location and could not change it. It now shows a window
    /// around the match, says which characters of how many it is, names the `read` that continues
    /// it, and allows an edit inside what it showed.
    #[test]
    fn a_hit_on_a_very_long_line_shows_its_window_and_that_window_can_be_edited() {
        let mut h = writable_harness();
        let line = format!(
            "{}quokka_sentinel{}{TAIL}",
            "x".repeat(1200),
            "y".repeat(1200)
        );
        h.write_file("long.txt", &format!("{line}\nsecond\n"));

        let seen = h.call("grep", r#"{"pattern":"quokka_sentinel"}"#).payload;
        assert!(seen.contains("quokka_sentinel"), "{seen}");
        let total = line.chars().count();
        assert!(
            seen.contains(&format!(" of {total};")),
            "the window does not say which characters of how many it is:\n{seen}"
        );
        assert!(
            seen.contains("`read` offset=1 column_offset="),
            "nothing says how to read the rest:\n{seen}"
        );
        let hits: Vec<&str> = seen
            .lines()
            .filter(|l| l.starts_with("long.txt:"))
            .collect();
        assert_eq!(hits.len(), 1, "{seen}");
        assert!(
            hits[0].chars().count() < 700,
            "the window is not bounded:\n{}",
            hits[0]
        );
        // Centred on the match: the window starts with a cut marker and the line's own opening is
        // not in it, so what was asked about is what the model reads.
        assert!(
            hits[0].starts_with("long.txt:1: …"),
            "the window does not say it is a window:\n{}",
            hits[0]
        );
        assert!(
            !hits[0].contains("zzz_tail_marker"),
            "the window runs past the match to the end of the line:\n{}",
            hits[0]
        );

        // The piece that was displayed may be edited; the rest of the line was never shown.
        let ok = h.call(
            "edit",
            r#"{"path":"long.txt","old_string":"quokka_sentinel","new_string":"quokka_changed"}"#,
        );
        assert!(
            ok.is_grounded(),
            "an edit inside the shown window was refused:\n{}",
            ok.render()
        );
    }

    /// The tail of that line, far outside the window: a name unique to the part nobody was shown.
    const TAIL: &str = " zzz_tail_marker";

    /// The other half of the same rule: an edit outside the window is refused, because those
    /// characters were never shown.
    #[test]
    fn an_edit_outside_the_shown_window_is_still_refused() {
        let mut h = writable_harness();
        let line = format!(
            "{}quokka_sentinel{}{TAIL}",
            "x".repeat(1200),
            "y".repeat(1200)
        );
        h.write_file("long.txt", &format!("{line}\n"));
        let _ = h.call("grep", r#"{"pattern":"quokka_sentinel"}"#);

        let refused = h.call(
            "edit",
            r#"{"path":"long.txt","old_string":"zzz_tail_marker","new_string":"zzz"}"#,
        );
        assert!(
            !refused.is_grounded(),
            "an edit of the part that was never shown was allowed:\n{}",
            refused.render()
        );
        assert!(
            refused.render().contains("never shown") || refused.render().contains("not been shown"),
            "the refusal does not say the part was never shown:\n{}",
            refused.render()
        );
    }
}

#[cfg(test)]
mod timing {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use letibot_transcript::ToolCall;

    use crate::backend::HostBackend;
    use crate::builtins::retrieval::Unavailable;
    use crate::events::RecordingToolSink;
    use crate::runtime::ToolRuntime;

    /// One `grep` over THIS repository, timed: what a change to a search's cost is reported with.
    /// `--ignored`, because a wall clock is not an assertion, and the fastest of three, because a
    /// box under load says more about the box than about the tool.
    ///
    /// ```text
    /// cargo test -p letibot-tools --lib a_grep_over_this_tree -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore]
    fn a_grep_over_this_tree() {
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
        for args in [
            // A handful of hits: the whole tree is read either way, so this is the matching cost.
            r#"{"pattern":"walk_ignoring"}"#,
            r#"{"pattern":"walk_ignoring","context":2}"#,
            // Past the cap: an answer that truncates, which a reading may stop early on.
            r#"{"pattern":"the","max_matches":200}"#,
        ] {
            let mut best = Duration::MAX;
            let mut said = String::new();
            for _ in 0..3 {
                let backend = HostBackend::new(root).expect("this repository");
                let registry =
                    crate::read_only_tools(Arc::new(Unavailable)).expect("built-ins register");
                let rt = ToolRuntime::new(registry, Box::new(backend));
                let mut sink = RecordingToolSink::new();
                let call = ToolCall {
                    id: "c".into(),
                    name: "grep".into(),
                    arguments: args.to_string(),
                };
                let t = Instant::now();
                let r = rt.invoke("turn_1", &call, &mut sink);
                best = best.min(t.elapsed());
                said = format!("{} line(s) of answer", r.payload.lines().count());
            }
            println!(
                "TIMING {args}: {:.0} ms ({said})",
                best.as_secs_f64() * 1000.0
            );
        }
    }
}
