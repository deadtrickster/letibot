//! `outline` — the definitions in a file or a tree, from a parse rather than a
//! pattern.
//!
//! # The call this replaces
//!
//! Measured in a real session, 2026-09-09: five of twelve tool rounds went on
//! these, in order, each one a repair of the last.
//!
//! ```text
//! ^(pub )?(mod|fn|struct|enum|impl|const|static)\s
//! ^\s*(pub )?fn |^mod |^#\[cfg\(test\)\]|^impl
//! ```
//!
//! Every one is the question *what is the structure of this file* written in a
//! language that cannot ask it. And the miss is not repairable by relaxing,
//! which is what makes this a second tool rather than a better ladder rung:
//! **relaxing a structural query lexically can only ever miss.** Keep the `^`
//! and a method indented under an `impl` is invisible; drop it and `fn` matches
//! inside comments, strings, and the word "define". There is no third pattern.
//!
//! # What it does about a miss (clause 1)
//!
//! - A path that does not exist returns the nearest listing, the way `read` and
//!   `grep` do.
//! - A file whose language has no grammar **says which extension and lists what
//!   is supported.** It never returns an empty outline: an empty outline for a
//!   `.tf` file is the empty-corpus bug in structural clothing, and a count
//!   still does not travel without its denominator.
//! - A `kind` filter that selects nothing reports the kinds that ARE present.
//! - A directory holding no file this can parse is a failed SCOPE, not an empty
//!   answer about definitions.
//! - A file that did not parse cleanly is marked partial, because a short
//!   outline of a broken file and a short outline of a small file are different
//!   facts.

use letibot_code::{Language, Outline, Symbol};
use serde_json::Value;

use crate::backend::{DirEntry, default_skip, walk};
use crate::runtime::{Invocation, InvokeCtx, Tool};
use crate::schema::{Access, ToolSchema};

use super::text_of;

pub struct OutlineTool;

/// The default ceiling on rows. Clause 5's rule applies — the payload spills
/// rather than being cut — but a tree-wide outline of a large workspace is a
/// different call from the one the model meant to make, and saying so is more
/// use than returning it.
const DEFAULT_MAX_SYMBOLS: usize = 400;
/// A tree outline stops naming files long before it stops being useful.
const MAX_FILES: usize = 200;

impl Tool for OutlineTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "outline",
            "List what a file or directory DEFINES — functions, methods, structs, \
             enums, traits, impls, classes, constants, modules — with line numbers, \
             the containing symbol, and the declaration as written. This reads a \
             parse, not the text, so an indented method and a definition split \
             across lines are found and a match inside a comment or a string is \
             not. Give `path`; optionally `kind` to filter (comma-separated, e.g. \
             `fn,struct`) and `language` for a file with no extension. Ask this \
             instead of grepping for `^(pub )?(fn|struct|impl)`.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "File or directory to outline."},
                    "kind": {"type": "string", "description": "Comma-separated kinds to keep: fn, method, struct, enum, trait, impl, mod, class, interface, const, static, type, union, macro, var, prototype, key."},
                    "language": {"type": "string", "description": "Override the grammar chosen from the extension: rust, python, go, c, bash, json."},
                    "max_symbols": {"type": "integer"}
                },
                "required": ["path"]
            }),
            Access::Read,
        )
    }

    fn invoke(&self, ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        let Some(path) = args.get("path").and_then(|v| v.as_str()) else {
            return Invocation::failed(
                "outline needs a path",
                "call `outline` again with `path` set to the file or directory whose \
                 structure you want.",
            );
        };
        let filter = args
            .get("kind")
            .and_then(|v| v.as_str())
            .map(parse_kinds)
            .filter(|k| !k.is_empty());
        let forced = args
            .get("language")
            .and_then(|v| v.as_str())
            .and_then(Language::of_name);
        if let Some(bad) = args.get("language").and_then(|v| v.as_str())
            && forced.is_none()
        {
            return Invocation::failed(
                format!("no grammar called `{bad}`"),
                supported_list("`language` must be one of"),
            );
        }
        let max = args
            .get("max_symbols")
            .and_then(|v| v.as_i64())
            .filter(|n| *n > 0)
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_MAX_SYMBOLS);

        let Some(entry) = ctx.backend.stat(path) else {
            let (dir, entries) = super::nearest_listing(ctx.backend, path);
            return Invocation::failed(
                format!("nothing at `{path}`"),
                super::render_listing(&dir, &entries, 40),
            )
            .with_note(format!(
                "`{path}` does not exist, so nothing was parsed. The listing of `{dir}` \
                 is below; outline one of these."
            ));
        };

        if entry.is_dir {
            self.tree(ctx, path, forced, filter.as_deref(), max)
        } else {
            self.one_file(ctx, path, forced, filter.as_deref(), max)
        }
    }
}

impl OutlineTool {
    fn one_file(
        &self,
        ctx: &mut InvokeCtx<'_>,
        path: &str,
        forced: Option<Language>,
        filter: Option<&[String]>,
        max: usize,
    ) -> Invocation {
        let lang = match forced.map(Ok).unwrap_or_else(|| Language::of_path(path)) {
            Ok(l) => l,
            Err(e) => {
                // NOT an empty outline. An empty outline here would say "this
                // file defines nothing", about a file nothing in this process
                // can read the structure of.
                return Invocation::failed(
                    format!("{e}, so `{path}` was not parsed"),
                    format!(
                        "{}\n\nIf the file is one of these under a different name, pass \
                         `language` explicitly. Otherwise `grep` still searches its text \
                         — this call made NO claim about what it defines.",
                        supported_list("the grammars compiled into this build are")
                    ),
                );
            }
        };
        let Ok(bytes) = ctx.backend.read(path) else {
            return Invocation::failed(
                format!("could not read `{path}`"),
                "the path exists but the read failed; check it is a regular file.",
            );
        };
        let (text, _) = text_of(&bytes);
        let o = letibot_code::outline(&text, lang);

        let kept = apply_filter(&o.symbols, filter);
        if kept.is_empty() {
            return empty_outline(path, &o, filter);
        }
        let (rows, capped) = render_file(path, &o, &kept, max);
        let mut inv = Invocation::ok(rows);
        if capped {
            inv = inv.with_note(format!(
                "stopped after {max} definitions of {}; raise `max_symbols` or narrow \
                 with `kind`",
                kept.len()
            ));
        }
        if o.partial {
            inv = inv.with_note(partial_note(path, lang));
        }
        if let Some(f) = filter {
            let rest: Vec<&Symbol> = o
                .symbols
                .iter()
                .filter(|s| !f.iter().any(|k| k == s.kind))
                .collect();
            if !rest.is_empty() {
                inv = inv.with_note(format!(
                    "`kind` was {}, so this is not the whole file: it also defines {}",
                    f.join(", "),
                    kinds_present_ref(&rest)
                ));
            }
        }
        inv
    }

    fn tree(
        &self,
        ctx: &mut InvokeCtx<'_>,
        root: &str,
        forced: Option<Language>,
        filter: Option<&[String]>,
        max: usize,
    ) -> Invocation {
        let (entries, _) = walk(
            ctx.backend,
            root,
            ctx.limits.max_walk_entries,
            &default_skip,
        );
        let files: Vec<&DirEntry> = entries.iter().filter(|e| !e.is_dir).collect();
        let mut skipped: Vec<String> = Vec::new();
        let mut parsed = 0usize;
        let mut total = 0usize;
        let mut partial: Vec<String> = Vec::new();
        let mut body = String::new();
        let mut shown = 0usize;
        let mut capped = false;
        let mut all_kinds: Vec<&'static str> = Vec::new();

        for (i, e) in files.iter().enumerate() {
            if i > 0 && i % 200 == 0 {
                ctx.progress(format!("parsed {parsed} of {} files", files.len()));
            }
            let lang = match forced.map(Ok).unwrap_or_else(|| Language::of_path(&e.path)) {
                Ok(l) => l,
                Err(u) => {
                    skipped.push(u.extension.unwrap_or_else(|| "(none)".into()));
                    continue;
                }
            };
            let Ok(bytes) = ctx.backend.read(&e.path) else {
                continue;
            };
            if bytes.contains(&0) {
                continue;
            }
            let (text, _) = text_of(&bytes);
            let o = letibot_code::outline(&text, lang);
            parsed += 1;
            if o.partial {
                partial.push(e.path.clone());
            }
            for s in &o.symbols {
                if !all_kinds.contains(&s.kind) {
                    all_kinds.push(s.kind);
                }
            }
            let kept = apply_filter(&o.symbols, filter);
            total += kept.len();
            if kept.is_empty() || capped {
                continue;
            }
            if shown >= MAX_FILES {
                capped = true;
                continue;
            }
            shown += 1;
            let room = max.saturating_sub(count_rows(&body));
            if room == 0 {
                capped = true;
                continue;
            }
            let (rows, cut) = render_file(&e.path, &o, &kept, room);
            body.push_str(&rows);
            body.push('\n');
            capped |= cut;
        }

        // A denominator of zero is a failed scope, never an answer about content.
        // Same rule as `grep`'s "0 files matched the scope", and the same reason.
        if parsed == 0 {
            let mut why = format!(
                "`{root}` holds {} file(s) and none of them has a grammar in this build.\n",
                files.len()
            );
            if !skipped.is_empty() {
                why.push_str(&format!("what is there: {}\n", tally(&skipped)));
            }
            why.push_str(&supported_list("what `outline` can parse"));
            why.push_str(
                "\n\nSo this call says NOTHING about what is defined under that path. \
                 `grep` still searches the text.",
            );
            return Invocation::failed(
                format!("no file under `{root}` could be parsed, so nothing was outlined"),
                why,
            );
        }
        if total == 0 {
            let mut inv = Invocation::abstained(
                format!(
                    "no definitions{} in the {parsed} file(s) parsed under `{root}`",
                    filter
                        .map(|f| format!(" of kind {}", f.join(", ")))
                        .unwrap_or_default()
                ),
                match filter {
                    Some(f) => format!(
                        "`kind` was {}. The kinds actually present under `{root}` are: {}.",
                        f.join(", "),
                        if all_kinds.is_empty() {
                            "none".to_string()
                        } else {
                            all_kinds.join(", ")
                        }
                    ),
                    None => format!(
                        "{parsed} file(s) parsed and none of them defines anything this \
                         build recognises. That is unusual; if the files are a language \
                         listed below under a different extension, pass `language`.\n{}",
                        supported_list("supported")
                    ),
                },
            );
            if !skipped.is_empty() {
                inv = inv.with_note(format!(
                    "{} file(s) had no grammar and were not parsed: {}",
                    skipped.len(),
                    tally(&skipped)
                ));
            }
            return inv;
        }

        let head = format!("{total} definition(s) in {parsed} parsed file(s) under `{root}`\n\n");
        let mut inv = Invocation::ok(format!("{head}{body}"));
        if capped {
            inv = inv.with_note(format!(
                "stopped after {max} definitions (and {MAX_FILES} files) of {total}; \
                 narrow `path`, or filter with `kind`, or raise `max_symbols`"
            ));
        }
        if !skipped.is_empty() {
            // The denominator, always. "18 definitions" over a tree where 300
            // files were never opened is a different claim from the same number
            // over a tree where 3 were.
            inv = inv.with_note(format!(
                "{} file(s) under `{root}` have no grammar in this build and were NOT \
                 parsed: {}. This outline says nothing about them.",
                skipped.len(),
                tally(&skipped)
            ));
        }
        if !partial.is_empty() {
            inv = inv.with_note(format!(
                "{} file(s) did not parse cleanly, so their outlines are partial: {}",
                partial.len(),
                partial
                    .iter()
                    .take(5)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        inv
    }
}

fn parse_kinds(raw: &str) -> Vec<String> {
    raw.split([',', ' ', '|'])
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        // The names a model is likely to write, mapped onto the ones the parser
        // emits. A filter that silently matched nothing because `function` is
        // spelled `fn` here would be a miss with no way to see it.
        .map(|s| match s.as_str() {
            "function" | "func" | "def" | "fun" => "fn".into(),
            "structs" | "record" => "struct".into(),
            "constant" => "const".into(),
            "module" => "mod".into(),
            "methods" => "method".into(),
            "class" | "classes" => "class".into(),
            _ => s,
        })
        .collect()
}

fn apply_filter<'a>(symbols: &'a [Symbol], filter: Option<&[String]>) -> Vec<&'a Symbol> {
    symbols
        .iter()
        .filter(|s| filter.is_none_or(|f| f.iter().any(|k| k == s.kind)))
        .collect()
}

fn kinds_present(symbols: &[Symbol]) -> String {
    kinds_present_ref(&symbols.iter().collect::<Vec<_>>())
}

fn kinds_present_ref(symbols: &[&Symbol]) -> String {
    let mut seen: Vec<&str> = Vec::new();
    for s in symbols {
        if !seen.contains(&s.kind) {
            seen.push(s.kind);
        }
    }
    if seen.is_empty() {
        "nothing".to_string()
    } else {
        seen.join(", ")
    }
}

fn count_rows(body: &str) -> usize {
    body.lines().filter(|l| l.starts_with(' ')).count()
}

/// One file's block: a header with the denominators, then a row per definition.
fn render_file(path: &str, o: &Outline, kept: &[&Symbol], max: usize) -> (String, bool) {
    let name_w = kept
        .iter()
        .map(|s| s.qualified().chars().count())
        .max()
        .unwrap_or(8)
        .clamp(8, 44);
    let mut out = format!(
        "{path} — {}, {} lines, {} definition(s){}\n",
        o.language,
        o.lines,
        kept.len(),
        if o.partial { ", PARTIAL PARSE" } else { "" }
    );
    let mut capped = false;
    for s in kept.iter().take(max) {
        let q = s.qualified();
        // The signature is the payload: `pub fn insert(&mut self, c: char) -> bool`
        // is enough to call it without a second `read`. The qualified name sits
        // beside it rather than being spliced into it, because splicing a
        // container into somebody else's syntax is a guess.
        out.push_str(&format!(
            "  {:>5}  {:<9} {:<name_w$}  {}\n",
            s.line, s.kind, q, s.signature
        ));
    }
    if kept.len() > max {
        capped = true;
    }
    (out, capped)
}

fn partial_note(path: &str, lang: Language) -> String {
    format!(
        "`{path}` did not parse cleanly as {lang}, so this outline is PARTIAL — what is \
         listed is real, what is missing may not be. The usual causes are a syntax \
         error, a `.h` that is actually C++, and heavy macro use."
    )
}

/// A file that parsed and defines nothing, or defines nothing of the asked kind.
fn empty_outline(path: &str, o: &Outline, filter: Option<&[String]>) -> Invocation {
    match filter {
        Some(f) => Invocation::abstained(
            format!("`{path}` defines nothing of kind {}", f.join(", ")),
            format!(
                "{path} parsed as {} ({} lines) and defines: {}.\nAsk again with one of \
                 those in `kind`, or drop `kind` for all of them.",
                o.language,
                o.lines,
                kinds_present(&o.symbols)
            ),
        ),
        None => Invocation::abstained(
            format!("`{path}` defines nothing this build recognises"),
            format!(
                "{path} parsed as {} and is {} lines long{}. A file that is all `use` \
                 lines, all data, or all top-level statements has no definitions to \
                 list — `read` it, or `grep` its text.",
                o.language,
                o.lines,
                if o.partial {
                    ", and it did NOT parse cleanly, so the emptiness may be the parse \
                     rather than the file"
                } else {
                    ""
                }
            ),
        ),
    }
}

fn supported_list(lead: &str) -> String {
    let langs: Vec<String> = letibot_code::supported()
        .iter()
        .map(|(name, exts)| {
            format!(
                "{name} ({})",
                exts.iter()
                    .map(|e| format!(".{e}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        })
        .collect();
    format!("{lead}: {}", langs.join(", "))
}

fn tally(items: &[String]) -> String {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for it in items {
        match counts.iter_mut().find(|(k, _)| k == it) {
            Some((_, n)) => *n += 1,
            None => counts.push((it.clone(), 1)),
        }
    }
    counts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    counts
        .iter()
        .take(8)
        .map(|(k, n)| format!(".{k} ({n})"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use crate::testing::harness;

    #[test]
    fn a_file_comes_back_as_definitions_with_lines_and_containers() {
        let mut h = harness();
        h.write_file(
            "src/editor.rs",
            "pub struct Editor { x: u8 }\n\
             impl Editor {\n\
             \x20   pub fn insert(&mut self, c: char) -> bool { true }\n\
             }\n",
        );
        let r = h.call("outline", r#"{"path":"src/editor.rs"}"#);
        assert!(r.is_grounded(), "{}", r.render());
        let seen = r.render();
        // The line number, the kind, the container, and the signature: the four
        // things the rano session asked five greps for.
        assert!(seen.contains("Editor::insert"), "{seen}");
        assert!(
            seen.contains("pub fn insert(&mut self, c: char) -> bool"),
            "{seen}"
        );
        assert!(seen.contains("     3  fn "), "a line number: {seen}");
        assert!(seen.contains("rust"), "{seen}");
    }

    /// The empty-corpus bug in structural clothing. A `.tf` file has no grammar
    /// here, and "no definitions" would be a claim about Terraform.
    #[test]
    fn an_unknown_language_says_so_and_lists_what_is_supported() {
        let mut h = harness();
        h.write_file("infra/main.tf", "resource \"aws_s3_bucket\" \"b\" {}\n");
        let r = h.call("outline", r#"{"path":"infra/main.tf"}"#);
        assert!(!r.is_grounded(), "{}", r.render());
        let seen = r.render();
        assert!(seen.contains(".tf"), "name the extension: {seen}");
        for lang in ["rust", "python", "go", "bash", "json"] {
            assert!(
                seen.contains(lang),
                "list what IS supported ({lang}): {seen}"
            );
        }
        assert!(seen.contains("NO claim"), "{seen}");
    }

    #[test]
    fn a_directory_with_no_parseable_file_is_a_failed_scope_not_an_empty_answer() {
        let mut h = harness();
        let r = h.call("outline", r#"{"path":"docs"}"#);
        assert!(!r.is_grounded(), "{}", r.render());
        let seen = r.render();
        assert_ne!(
            crate::result::Envelope::classify(&seen),
            Some("NO_RESULT"),
            "0 files parsed must not abstain: an abstention claims the content was \
             looked at\n{seen}"
        );
        assert!(seen.contains("says NOTHING"), "{seen}");
        assert!(seen.contains(".md"), "name what was there: {seen}");
    }

    #[test]
    fn a_directory_outlines_every_file_it_can_and_counts_the_ones_it_cannot() {
        let mut h = harness();
        let r = h.call("outline", r#"{"path":"."}"#);
        assert!(r.is_grounded(), "{}", r.render());
        let seen = r.render();
        assert!(seen.contains("src/lib.rs"), "{seen}");
        assert!(seen.contains("parse_args"), "{seen}");
        // The denominator travels with the count.
        let notes = r.notes.join(" ");
        assert!(notes.contains("no grammar"), "{notes}");
        assert!(notes.contains(".md"), "{notes}");
    }

    #[test]
    fn a_kind_filter_that_selects_nothing_reports_the_kinds_that_are_there() {
        let mut h = harness();
        let r = h.call("outline", r#"{"path":"src/lib.rs","kind":"trait"}"#);
        assert!(!r.is_grounded(), "{}", r.render());
        let seen = r.render();
        assert!(seen.contains("fn"), "say what IS defined: {seen}");
    }

    #[test]
    fn the_words_a_model_writes_for_a_kind_are_accepted() {
        let mut h = harness();
        let r = h.call("outline", r#"{"path":"src/lib.rs","kind":"function"}"#);
        assert!(
            r.is_grounded(),
            "`function` must reach `fn`:\n{}",
            r.render()
        );
        assert!(r.payload.contains("parse_args"), "{}", r.payload);
    }

    #[test]
    fn a_path_that_does_not_exist_returns_the_listing_around_it() {
        let mut h = harness();
        let r = h.call("outline", r#"{"path":"src/parser/mod.rs"}"#);
        assert!(!r.is_grounded());
        assert!(r.payload.contains("util"), "{}", r.payload);
    }

    #[test]
    fn a_file_that_does_not_parse_cleanly_is_marked_partial() {
        let mut h = harness();
        h.write_file("src/broken.rs", "fn good() {}\nfn bad( {\nstruct After;\n");
        let r = h.call("outline", r#"{"path":"src/broken.rs"}"#);
        let seen = r.render();
        assert!(
            seen.contains("PARTIAL") || seen.contains("partial"),
            "{seen}"
        );
        assert!(seen.contains("good"), "what parsed is still real: {seen}");
    }
}
