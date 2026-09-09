//! `glob` — file names, and clause 1's second acceptance case:
//!
//! > a `find`/`glob` that misses **returns the surrounding listing** and what
//! > would have matched.
//!
//! Both halves are here and they are different things. *What would have matched*
//! is the relaxation ladder — the same idea as `grep`'s, over path syntax instead
//! of pattern syntax. *The surrounding listing* is the directory the pattern's
//! literal prefix points at, printed even when every rung failed, because a model
//! that guessed `src/parser/**/*.rs` needs to see that `src` holds `parse/` and
//! not `parser/`, and no relaxation of the pattern will tell it that.

use serde_json::Value;

use crate::backend::{DirEntry, default_skip, walk};
use crate::runtime::{Invocation, InvokeCtx, Tool};
use crate::schema::{Access, ToolSchema};

use super::pattern::glob_match;
use super::{nearest_listing, render_listing};

pub struct Glob;

impl Tool for Glob {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "glob",
            "List files whose path matches a glob: `*` within one segment, `**` across \
             segments, `?`, `[a-z]`, `{a,b}`. Give `pattern`; optionally `path` to \
             scope the walk. A pattern that matches nothing comes back with the \
             listing around it and with what a relaxed pattern would have matched, so \
             a wrong guess is corrected in the same call.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "Glob to match against paths."},
                    "path": {"type": "string", "description": "Directory to walk. Defaults to the session root."},
                    "limit": {"type": "integer", "description": "Most paths to return."}
                },
                "required": ["pattern"]
            }),
            Access::Read,
        )
    }

    fn invoke(&self, ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        let Some(source) = args.get("pattern").and_then(|v| v.as_str()) else {
            return Invocation::failed(
                "glob needs a pattern",
                "call `glob` again with `pattern` set to the paths you are looking for, \
                 for example `src/**/*.rs`.",
            );
        };
        let scope = args
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or(".")
            .to_string();
        let limit = args
            .get("limit")
            .and_then(|v| v.as_i64())
            .filter(|n| *n > 0)
            .map(|n| n as usize)
            .unwrap_or(200);

        let (entries, truncated) = walk(
            ctx.backend,
            &scope,
            ctx.limits.max_walk_entries,
            &default_skip,
        );
        ctx.progress(format!("walked {} entries under {scope}", entries.len()));
        let files: Vec<&DirEntry> = entries.iter().filter(|e| !e.is_dir).collect();

        for (pattern, relaxation) in ladder(source) {
            let hits: Vec<&&DirEntry> = files
                .iter()
                .filter(|e| glob_match(&pattern, &e.path))
                .collect();
            if hits.is_empty() {
                continue;
            }
            let mut body = format!("{} path(s) match `{pattern}`:\n", hits.len());
            for e in hits.iter().take(limit) {
                body.push_str(&format!("{} ({})\n", e.path, super::human(e.bytes)));
            }
            if hits.len() > limit {
                body.push_str(&format!("… and {} more\n", hits.len() - limit));
            }
            let mut inv = Invocation::ok(body);
            if let Some(r) = relaxation {
                // The rewrite is visible, always. §9.4.
                inv = inv.with_note(format!(
                    "`{source}` matched nothing as written. {r}, and that is what the \
                     paths above matched."
                ));
                inv = inv.with_note(surroundings(ctx, source));
            }
            if truncated {
                inv = inv.with_note(format!(
                    "the walk stopped at {} entries; scope it with `path` to see the rest",
                    ctx.limits.max_walk_entries
                ));
            }
            return inv;
        }

        // Nothing matched under any relaxation. The listing still goes back — that
        // is the half of clause 1 a relaxation ladder cannot supply.
        let mut body = surroundings(ctx, source);
        body.push_str(&format!(
            "\n{} file(s) were walked under `{scope}`. Relaxations tried: {}.\n",
            files.len(),
            ladder(source)
                .iter()
                .skip(1)
                .map(|(p, _)| format!("`{p}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
        Invocation::abstained(
            format!("no path matches `{source}`, and no relaxation of it matches either"),
            body,
        )
    }
}

/// The rungs, in order of how much they give up.
fn ladder(source: &str) -> Vec<(String, Option<String>)> {
    let mut out = vec![(source.to_string(), None)];
    let last = source.rsplit('/').next().unwrap_or(source).to_string();

    if !source.starts_with("**/") {
        out.push((
            format!("**/{source}"),
            Some(format!("it was retried at any depth, as `**/{source}`")),
        ));
    }
    if last != source {
        out.push((
            format!("**/{last}"),
            Some(format!(
                "the directory part was dropped and only the file part `{last}` was \
                 matched, at any depth"
            )),
        ));
    }
    if let Some(dot) = last.rfind('.')
        && dot + 1 < last.len()
    {
        let ext = &last[dot + 1..];
        out.push((
            format!("**/*.{ext}"),
            Some(format!(
                "everything with the `.{ext}` extension was listed instead"
            )),
        ));
    }
    let stem: String = last
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
        .collect();
    if stem.len() >= 3 {
        out.push((
            format!("**/*{stem}*"),
            Some(format!(
                "`{stem}` was matched as a substring of the file name"
            )),
        ));
    }
    out
}

/// The listing around the pattern's literal prefix — the part before the first
/// wildcard, cut back to a directory.
fn surroundings(ctx: &mut InvokeCtx<'_>, pattern: &str) -> String {
    let literal: String = pattern
        .chars()
        .take_while(|c| !matches!(c, '*' | '?' | '[' | '{'))
        .collect();
    let dir = match literal.rfind('/') {
        Some(i) => literal[..i].to_string(),
        None => ".".to_string(),
    };
    // `nearest_listing` walks up to the deepest ancestor that exists, and its
    // first return value is the directory it actually found — which is the point:
    // the model guessed `src/parser/` and what it needs to see is what `src` holds.
    let (found, entries) = nearest_listing(ctx.backend, &format!("{dir}/x"));
    let mut out = render_listing(&found, &entries, 60);
    if found != dir && !dir.is_empty() && dir != "." {
        out = format!("`{dir}` does not exist.\n{out}");
    }
    out
}

#[cfg(test)]
mod tests {
    use crate::testing::harness;

    #[test]
    fn a_hit_lists_the_paths() {
        let mut h = harness();
        let r = h.call("glob", r#"{"pattern":"src/**/*.rs"}"#);
        assert!(r.is_grounded());
        assert!(r.payload.contains("src/lib.rs"), "{}", r.payload);
    }

    #[test]
    fn a_miss_returns_the_surrounding_listing_and_what_would_have_matched() {
        // The plan's second acceptance test for clause 1. `src/parser/` does not
        // exist; `src/util/` does.
        let mut h = harness();
        let r = h.call("glob", r#"{"pattern":"src/parser/**/*.rs"}"#);
        assert!(
            r.is_grounded(),
            "a relaxed match is a result: {:?}",
            r.outcome
        );
        let notes = r.notes.join(" ");
        assert!(notes.contains("matched nothing as written"), "{notes}");
        // The surrounding listing came back too, not only the relaxed matches.
        assert!(notes.contains("util"), "{notes}");
        assert!(r.payload.contains(".rs"), "{}", r.payload);
    }

    #[test]
    fn an_extension_nobody_has_abstains_with_the_listing_anyway() {
        let mut h = harness();
        let r = h.call("glob", r#"{"pattern":"**/*.qqq"}"#);
        assert!(!r.is_grounded());
        // Still useful: the listing is in the body.
        assert!(r.payload.contains("src"), "{}", r.payload);
        assert!(r.payload.contains("Relaxations tried"), "{}", r.payload);
    }
}
