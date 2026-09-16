//! Diffs: a line diff, an intra-line word diff, and a unified rendering.
//!
//! # Why a head needs this at all
//!
//! Today a completed `edit` tool call renders as
//! `● edit(call_7) — ok · 214 B`. That is a true statement about a number of
//! bytes and it answers none of the questions a person has, which are: *which
//! file, and what changed in it*. An agent that edits code and shows the
//! operator a byte count has moved the review burden onto a second tool.
//!
//! # The algorithm, and the bound on it
//!
//! Myers' greedy O(ND) edit-script algorithm, with the two standard
//! preconditioners (strip the common prefix and suffix first) and one
//! non-standard guard: **`max_d`**. Myers is O(ND) where D is the size of the
//! edit script, so two unrelated 10,000-line files cost 10⁸ steps — and a head
//! is not allowed to stall for a frame, ever, because the same loop is servicing
//! the socket. Past `max_d` the diff gives up and reports the region as a whole
//! replacement, which is both honest and what a reader would conclude anyway.
//! [`Diff::degraded`] says when that happened, so it is never silent.
//!
//! # Word-level highlight, and where it stops
//!
//! Inside a hunk, a removed line adjacent to an added line is *usually* an edit
//! of that line, and showing which run of characters changed is the difference
//! between reading a diff and scanning it. The pairing rule here is positional —
//! the k-th removal in a run pairs with the k-th addition — which is what every
//! terminal diff does and is wrong when the run is a reordering. The cost of
//! being wrong is a highlight that emphasises too much; the guard is that
//! pairing is only attempted when the two lines are similar enough
//! ([`SIMILARITY_FLOOR`]), so two entirely different lines are shown plainly
//! rather than as a sea of emphasis.
//!
//! # Provenance
//!
//! **Adapted from grok-build** (xAI, Apache-2.0),
//! `crates/codegen/xai-grok-pager-diff/src/lib.rs` and
//! `crates/codegen/xai-grok-pager/src/scrollback/blocks/tool/edit.rs`. Taken:
//! the hunk model (`DiffLine{text, lo, ln, tag}` → [`Row`]), `MAX_CONTEXT = 3`,
//! overlapping-hunk stitching, and the decision to render **unified only**.
//!
//! Also taken, and it is the kind of edge case one only finds by shipping: an
//! empty-to-empty edit *with* context is a blank-line insertion and renders as
//! `"\n"`, but *without* context it is an empty file write and must produce **no
//! hunk lines at all** — `lib.rs:52` says it "must not produce a fabricated
//! one-line insertion". [`hunks`] inherits that by construction, since it emits
//! rows only for ops.
//!
//! Changed:
//!
//! - They use the `similar` crate; this is a hand-written Myers with a `max_d`
//!   cap. grok-build has no cap because their diff runs off the render thread
//!   and mid-file syntax highlighting is already backgrounded
//!   (`EditHighlightPhase::Pending{job_id}`); a letibot head is single-threaded
//!   over the same loop that services the socket, so an unbounded diff is a
//!   dropped frame, and [`Diff::degraded`] is the honest alternative.
//! - **Word-level intra-line highlight is added.** grok-build has none — a grep
//!   for `iter_inline_changes` across the whole pager finds nothing — so a
//!   renamed variable shows as a whole line removed and a whole line added.
//!   [`word_spans`] and [`SIMILARITY_FLOOR`] are letibot's.
//! - Their per-hunk-then-whole-file progressive highlight
//!   (`EditHighlightPhase`, capped at 2 MiB / 50k lines) is **not** ported: it
//!   needs a worker thread to be worth anything, and its benefit — correct
//!   scopes for a multi-line string that opens above the hunk — does not arise
//!   here because [`crate::highlight`] is line-oriented and carries its state
//!   explicitly. The right integration is to seed a [`crate::highlight::State`]
//!   from the lines above the hunk; see the design note.

use crate::style::{Palette, Role};
use crate::width::{self, RESET};

/// Byte spans within one line, in order and non-overlapping.
pub type Spans = Vec<(usize, usize)>;

/// Below this ratio of shared tokens, two paired lines are treated as unrelated
/// and no intra-line highlight is attempted. Chosen so that a renamed variable
/// highlights and a rewritten line does not.
pub const SIMILARITY_FLOOR: f32 = 0.35;

/// Default cap on Myers' D. Roughly "a thousand changed lines", which is far
/// past the point where a human reads the diff rather than the file.
pub const DEFAULT_MAX_D: usize = 2000;

/// One element of an edit script.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// Line `a` of the old file equals line `b` of the new.
    Equal { a: usize, b: usize },
    /// Line `a` of the old file is gone.
    Delete { a: usize },
    /// Line `b` of the new file is new.
    Insert { b: usize },
}

/// An edit script plus whether it is exact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diff {
    pub ops: Vec<Op>,
    /// True when `max_d` was hit and part of the file is reported as a
    /// wholesale replacement rather than a minimal edit.
    pub degraded: bool,
}

/// Line-level diff of two sequences.
pub fn diff_lines(old: &[&str], new: &[&str]) -> Diff {
    diff_lines_with(old, new, DEFAULT_MAX_D)
}

pub fn diff_lines_with(old: &[&str], new: &[&str], max_d: usize) -> Diff {
    // Strip the common prefix and suffix. On a real edit this removes almost
    // everything, which is what makes Myers affordable on a large file.
    let mut lo = 0usize;
    while lo < old.len() && lo < new.len() && old[lo] == new[lo] {
        lo += 1;
    }
    let mut hi = 0usize;
    while hi < old.len() - lo && hi < new.len() - lo && old[old.len() - 1 - hi] == new[new.len() - 1 - hi]
    {
        hi += 1;
    }
    let a = &old[lo..old.len() - hi];
    let b = &new[lo..new.len() - hi];

    let mut ops: Vec<Op> = (0..lo).map(|i| Op::Equal { a: i, b: i }).collect();
    let (mid, degraded) = myers(a, b, max_d);
    for op in mid {
        ops.push(match op {
            Op::Equal { a: i, b: j } => Op::Equal {
                a: lo + i,
                b: lo + j,
            },
            Op::Delete { a: i } => Op::Delete { a: lo + i },
            Op::Insert { b: j } => Op::Insert { b: lo + j },
        });
    }
    for k in 0..hi {
        ops.push(Op::Equal {
            a: old.len() - hi + k,
            b: new.len() - hi + k,
        });
    }
    Diff { ops, degraded }
}

/// Myers' greedy algorithm with a D cap.
///
/// The trace is kept per D — `v_trace` — and walked backwards to recover the
/// script, which is the memory-hungry but simple form. Memory is O(D²) worst
/// case and D is capped, so the cap bounds both axes at once.
fn myers<T: PartialEq>(a: &[T], b: &[T], max_d: usize) -> (Vec<Op>, bool) {
    let n = a.len();
    let m = b.len();
    if n == 0 && m == 0 {
        return (Vec::new(), false);
    }
    if n == 0 {
        return ((0..m).map(|b| Op::Insert { b }).collect(), false);
    }
    if m == 0 {
        return ((0..n).map(|a| Op::Delete { a }).collect(), false);
    }
    let max = (n + m).min(max_d);
    let off = n + m;
    let mut v = vec![0isize; 2 * (n + m) + 1];
    let mut trace: Vec<Vec<isize>> = Vec::new();

    for d in 0..=max {
        trace.push(v.clone());
        let di = d as isize;
        let mut k = -di;
        while k <= di {
            let ki = (k + off as isize) as usize;
            let mut x = if k == -di || (k != di && v[ki - 1] < v[ki + 1]) {
                v[ki + 1]
            } else {
                v[ki - 1] + 1
            };
            let mut y = x - k;
            while (x as usize) < n && (y as usize) < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[ki] = x;
            if x as usize >= n && y as usize >= m {
                return (backtrack(&trace, n, m, off), false);
            }
            k += 2;
        }
    }
    // Gave up: report the whole middle as a replacement.
    let mut ops: Vec<Op> = (0..n).map(|a| Op::Delete { a }).collect();
    ops.extend((0..m).map(|b| Op::Insert { b }));
    (ops, true)
}

fn backtrack(trace: &[Vec<isize>], n: usize, m: usize, off: usize) -> Vec<Op> {
    let mut ops = Vec::new();
    let mut x = n as isize;
    let mut y = m as isize;
    for d in (0..trace.len()).rev() {
        let v = &trace[d];
        let di = d as isize;
        let k = x - y;
        let ki = (k + off as isize) as usize;
        let prev_k = if k == -di || (k != di && v[ki - 1] < v[ki + 1]) {
            k + 1
        } else {
            k - 1
        };
        let prev_x = v[(prev_k + off as isize) as usize];
        let prev_y = prev_x - prev_k;
        while x > prev_x && y > prev_y {
            x -= 1;
            y -= 1;
            ops.push(Op::Equal {
                a: x as usize,
                b: y as usize,
            });
        }
        if d == 0 {
            break;
        }
        if x > prev_x {
            x -= 1;
            ops.push(Op::Delete { a: x as usize });
        } else {
            y -= 1;
            ops.push(Op::Insert { b: y as usize });
        }
    }
    ops.reverse();
    ops
}

/// A run of changes with `context` unchanged lines either side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub old_start: usize,
    pub new_start: usize,
    pub rows: Vec<Row>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    Context { a: usize, b: usize },
    Removed { a: usize },
    Added { b: usize },
}

/// Group an edit script into hunks. `context` is lines of unchanged code kept
/// either side; three is the `diff -u` convention and is a parameter here
/// because a terminal head with fifteen rows wants one.
pub fn hunks(d: &Diff, context: usize) -> Vec<Hunk> {
    let changed: Vec<usize> = d
        .ops
        .iter()
        .enumerate()
        .filter(|(_, o)| !matches!(o, Op::Equal { .. }))
        .map(|(i, _)| i)
        .collect();
    if changed.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<Hunk> = Vec::new();
    let mut i = 0usize;
    while i < changed.len() {
        let start = changed[i].saturating_sub(context);
        let mut j = i;
        // Extend while the next change is close enough that the context would
        // overlap; otherwise a two-line gap becomes two hunks and reads worse.
        while j + 1 < changed.len() && changed[j + 1] <= changed[j] + 2 * context + 1 {
            j += 1;
        }
        let end = (changed[j] + context + 1).min(d.ops.len());
        let mut rows = Vec::new();
        let mut old_start = usize::MAX;
        let mut new_start = usize::MAX;
        for op in &d.ops[start..end] {
            match *op {
                Op::Equal { a, b } => {
                    old_start = old_start.min(a);
                    new_start = new_start.min(b);
                    rows.push(Row::Context { a, b });
                }
                Op::Delete { a } => {
                    old_start = old_start.min(a);
                    rows.push(Row::Removed { a });
                }
                Op::Insert { b } => {
                    new_start = new_start.min(b);
                    rows.push(Row::Added { b });
                }
            }
        }
        out.push(Hunk {
            old_start: if old_start == usize::MAX { 0 } else { old_start },
            new_start: if new_start == usize::MAX { 0 } else { new_start },
            rows,
        });
        i = j + 1;
    }
    out
}

/// How a diff is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffConfig {
    pub width: usize,
    pub palette: Palette,
    /// Unchanged lines either side of a change.
    pub context: usize,
    /// Show old/new line numbers in a gutter.
    pub line_numbers: bool,
    /// Highlight the changed run inside a paired removed/added line.
    pub intra_line: bool,
    /// Stop after this many rows and say how many were dropped. A tool that
    /// rewrites a file produces a diff longer than the screen, and a head that
    /// prints all of it has just scrolled the conversation away.
    pub max_rows: usize,
}

impl Default for DiffConfig {
    fn default() -> Self {
        DiffConfig {
            width: 100,
            palette: Palette::Colour,
            context: 3,
            line_numbers: true,
            intra_line: true,
            max_rows: 60,
        }
    }
}

/// Render a unified diff.
///
/// Unified rather than side-by-side, and that is a decision about the medium: a
/// side-by-side diff needs 160 columns to show two 72-column files, and below
/// that it truncates code — which is the one thing a diff must not do, because
/// the truncated part is where the change is. Unified degrades to a narrow
/// terminal by wrapping, which loses alignment but no content.
pub fn render(old: &[&str], new: &[&str], cfg: &DiffConfig) -> Vec<String> {
    render_from(old, new, cfg, 1, 1)
}

/// [`render`] for an **excerpt**: `old_start` / `new_start` are the 1-based
/// lines of the whole files the two slices begin at, so the gutter and the hunk
/// headers number the file and not the excerpt. A `ToolFinished` event carries
/// exactly this — the changed region plus context, with both starts — and a
/// diff of it numbered from 1 would tell the reader line 4 changed when it was
/// line 312.
pub fn render_from(
    old: &[&str],
    new: &[&str],
    cfg: &DiffConfig,
    old_start: usize,
    new_start: usize,
) -> Vec<String> {
    let old_base = old_start.saturating_sub(1);
    let new_base = new_start.saturating_sub(1);
    let d = diff_lines(old, new);
    let hs = hunks(&d, cfg.context);
    let mut out = Vec::new();
    if hs.is_empty() {
        out.push(cfg.palette.paint(Role::Faint, "no change"));
        return out;
    }
    if d.degraded {
        out.push(cfg.palette.paint(
            Role::Attention,
            "! diff gave up on the minimal edit script; \
             the changed region is shown as a whole replacement",
        ));
    }
    let numw = if cfg.line_numbers {
        let m = (old_base + old.len()).max(new_base + new.len()).max(1);
        m.to_string().len()
    } else {
        0
    };
    let mut rows_left = cfg.max_rows;
    let mut dropped = 0usize;
    for (hi, h) in hs.iter().enumerate() {
        if hi > 0 || hs.len() > 1 {
            let count_old = h
                .rows
                .iter()
                .filter(|r| matches!(r, Row::Context { .. } | Row::Removed { .. }))
                .count();
            let count_new = h
                .rows
                .iter()
                .filter(|r| matches!(r, Row::Context { .. } | Row::Added { .. }))
                .count();
            out.push(cfg.palette.paint(
                Role::Faint,
                &format!(
                    "@@ -{},{} +{},{} @@",
                    old_base + h.old_start + 1,
                    count_old,
                    new_base + h.new_start + 1,
                    count_new
                ),
            ));
        }
        let paired = if cfg.intra_line {
            pair_rows(&h.rows, old, new)
        } else {
            Vec::new()
        };
        for (ri, r) in h.rows.iter().enumerate() {
            if rows_left == 0 {
                dropped += 1;
                continue;
            }
            rows_left -= 1;
            let emph: Option<&Spans> = paired.get(ri).and_then(|p| p.as_ref());
            out.extend(row_lines(r, old, new, cfg, numw, emph, old_base, new_base));
        }
    }
    if dropped > 0 {
        out.push(cfg.palette.paint(
            Role::Faint,
            &format!("… {dropped} more diff lines not shown"),
        ));
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn row_lines(
    r: &Row,
    old: &[&str],
    new: &[&str],
    cfg: &DiffConfig,
    numw: usize,
    emph: Option<&Spans>,
    old_base: usize,
    new_base: usize,
) -> Vec<String> {
    let (sign, role, text, num) = match *r {
        Row::Context { a, b } => (
            " ",
            Role::Plain,
            old.get(a).copied().unwrap_or(""),
            (Some(old_base + a + 1), Some(new_base + b + 1)),
        ),
        Row::Removed { a } => (
            "-",
            Role::Removed,
            old.get(a).copied().unwrap_or(""),
            (Some(old_base + a + 1), None),
        ),
        Row::Added { b } => (
            "+",
            Role::Added,
            new.get(b).copied().unwrap_or(""),
            (None, Some(new_base + b + 1)),
        ),
    };
    let gutter = if cfg.line_numbers {
        let (a, b) = num;
        format!(
            "{:>numw$} {:>numw$} ",
            a.map(|n| n.to_string()).unwrap_or_default(),
            b.map(|n| n.to_string()).unwrap_or_default(),
        )
    } else {
        String::new()
    };
    let body_w = cfg.width.saturating_sub(width::width(&gutter) + 1).max(8);
    // Tabs must be expanded before wrapping or the width is a lie.
    let text = expand_tabs(text, 4);
    let body = paint_with_emphasis(&text, role, emph, cfg.palette);
    let wrapped = width::wrap(&body, body_w);
    // The gutter takes the line's own foreground on a changed row — the same
    // rule the split renderer's number follows — and stays dim on a context
    // row.
    let gut = cfg.palette.paint(
        if role == Role::Plain { Role::Faint } else { role.foreground() },
        &gutter,
    );
    let mut out = Vec::with_capacity(wrapped.len());
    for (i, l) in wrapped.into_iter().enumerate() {
        if i == 0 {
            // The sign keeps the foreground the role always had; the line's
            // own paint is background-only, so the text keeps its original
            // foregrounds (the operator's ruling on the first cube tint).
            out.push(format!("{gut}{}{l}", cfg.palette.paint(role.foreground(), sign)));
        } else {
            // A wrapped continuation keeps the colour and loses the sign, so the
            // eye does not read it as a second changed line.
            out.push(format!(
                "{}{}{l}",
                cfg.palette.paint(Role::Faint, &" ".repeat(width::width(&gutter))),
                " "
            ));
        }
    }
    out
}

/// Paint a line, emphasising the byte ranges in `emph`.
fn paint_with_emphasis(
    text: &str,
    role: Role,
    emph: Option<&Spans>,
    p: Palette,
) -> String {
    let base = p.open(role);
    let em = p.open(Role::Emphasis);
    let Some(spans) = emph else {
        return if base.is_empty() {
            text.to_string()
        } else {
            format!("{base}{text}{RESET}")
        };
    };
    if base.is_empty() && em.is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len() + 32);
    let mut at = 0usize;
    for &(s, e) in spans {
        let (s, e) = (s.min(text.len()), e.min(text.len()));
        if s < at {
            continue;
        }
        out.push_str(base);
        out.push_str(&text[at..s]);
        out.push_str(RESET);
        out.push_str(base);
        out.push_str(em);
        out.push_str(&text[s..e]);
        out.push_str(RESET);
        at = e;
    }
    out.push_str(base);
    out.push_str(&text[at..]);
    out.push_str(RESET);
    out
}

/// For each row, the byte spans that changed relative to its pair, or `None`.
fn pair_rows(rows: &[Row], old: &[&str], new: &[&str]) -> Vec<Option<Spans>> {
    let mut out: Vec<Option<Spans>> = vec![None; rows.len()];
    let mut i = 0usize;
    while i < rows.len() {
        // Find a maximal run of removals followed by a maximal run of additions.
        let rem_start = i;
        while i < rows.len() && matches!(rows[i], Row::Removed { .. }) {
            i += 1;
        }
        let rem_end = i;
        let add_start = i;
        while i < rows.len() && matches!(rows[i], Row::Added { .. }) {
            i += 1;
        }
        let add_end = i;
        if rem_end == rem_start || add_end == add_start {
            if i == rem_start {
                i += 1;
            }
            continue;
        }
        let n = (rem_end - rem_start).min(add_end - add_start);
        for k in 0..n {
            let Row::Removed { a } = rows[rem_start + k] else {
                continue;
            };
            let Row::Added { b } = rows[add_start + k] else {
                continue;
            };
            let (Some(ol), Some(nl)) = (old.get(a), new.get(b)) else {
                continue;
            };
            let ol = expand_tabs(ol, 4);
            let nl = expand_tabs(nl, 4);
            if let Some((os, ns)) = word_spans(&ol, &nl) {
                out[rem_start + k] = Some(os);
                out[add_start + k] = Some(ns);
            }
        }
    }
    out
}

/// Byte spans that differ between two similar lines, or `None` when they are not
/// similar enough for the highlight to mean anything.
fn word_spans(a: &str, b: &str) -> Option<(Spans, Spans)> {
    let at = tokens(a);
    let bt = tokens(b);
    if at.is_empty() || bt.is_empty() {
        return None;
    }
    let av: Vec<&str> = at.iter().map(|t| &a[t.0..t.1]).collect();
    let bv: Vec<&str> = bt.iter().map(|t| &b[t.0..t.1]).collect();
    let (ops, degraded) = myers(&av, &bv, 512);
    if degraded {
        return None;
    }
    let equal = ops.iter().filter(|o| matches!(o, Op::Equal { .. })).count();
    let ratio = 2.0 * equal as f32 / (av.len() + bv.len()) as f32;
    if ratio < SIMILARITY_FLOOR {
        return None;
    }
    let mut asp = Vec::new();
    let mut bsp = Vec::new();
    for op in &ops {
        match *op {
            Op::Delete { a: i } => merge(&mut asp, at[i]),
            Op::Insert { b: j } => merge(&mut bsp, bt[j]),
            Op::Equal { .. } => {}
        }
    }
    if asp.is_empty() && bsp.is_empty() {
        return None;
    }
    Some((asp, bsp))
}

fn merge(v: &mut Spans, s: (usize, usize)) {
    match v.last_mut() {
        Some(last) if last.1 == s.0 => last.1 = s.1,
        _ => v.push(s),
    }
}

/// Split into runs of word characters and runs of everything else, as byte
/// spans. Whitespace is its own token so that an indentation change highlights.
fn tokens(s: &str) -> Spans {
    let mut out = Vec::new();
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        let word = c.is_alphanumeric() || c == '_';
        let mut end = i + c.len_utf8();
        while let Some(&(j, c2)) = it.peek() {
            let w2 = c2.is_alphanumeric() || c2 == '_';
            if w2 != word {
                break;
            }
            end = j + c2.len_utf8();
            it.next();
        }
        out.push((i, end));
    }
    out
}

/// Expand tabs to a tab stop. A diff that measures a tab as one column
/// mis-aligns every line that has one, which in Go and Makefiles is all of them.
pub fn expand_tabs(s: &str, stop: usize) -> String {
    if !s.contains('\t') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 8);
    let mut col = 0usize;
    for c in s.chars() {
        if c == '\t' {
            let n = stop - (col % stop);
            out.push_str(&" ".repeat(n));
            col += n;
        } else {
            out.push(c);
            col += width::char_width(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An excerpt of lines 310..314 must be numbered 310..314, not 1..5: the
    /// pair a `ToolFinished` carries is a window, and a diff numbered from 1
    /// tells the reader line 4 changed when it was line 313.
    #[test]
    fn an_excerpt_is_numbered_from_where_it_starts_in_the_file() {
        let cfg = DiffConfig { width: 80, palette: Palette::None, context: 1, line_numbers: true, intra_line: false, max_rows: 60 };
        let old = ["a", "b", "c"];
        let new = ["a", "B", "c"];
        let rows = render_from(&old, &new, &cfg, 310, 310);
        let text = rows.join("\n");
        assert!(text.contains("311     -b") || text.contains("311      -b"), "{text}");
        assert!(text.contains("    311 +B"), "{text}");
        assert!(!text.contains(" 1 "), "numbered from 1: {text}");
        // `render` is the same thing from line 1.
        let from_one = render(&old, &new, &cfg).join("\n");
        assert!(from_one.contains("2   -b") || from_one.contains("2  -b"), "{from_one}");
    }


    fn lines(s: &str) -> Vec<&str> {
        s.lines().collect()
    }

    #[test]
    fn an_identical_file_has_no_hunks() {
        let a = lines("one\ntwo\nthree\n");
        let d = diff_lines(&a, &a);
        assert!(hunks(&d, 3).is_empty());
        assert!(d.ops.iter().all(|o| matches!(o, Op::Equal { .. })));
    }

    #[test]
    fn the_edit_script_reconstructs_the_new_file() {
        // The property that makes a diff trustworthy: apply it and you get `new`.
        let cases = [
            ("a\nb\nc\n", "a\nx\nc\n"),
            ("a\nb\nc\n", "a\nb\nc\nd\n"),
            ("", "a\nb\n"),
            ("a\nb\n", ""),
            ("a\nb\nc\nd\ne\n", "e\nd\nc\nb\na\n"),
            ("one\ntwo\n", "one\ntwo\n"),
        ];
        for (o, n) in cases {
            let (ol, nl) = (lines(o), lines(n));
            let d = diff_lines(&ol, &nl);
            let mut rebuilt: Vec<&str> = Vec::new();
            for op in &d.ops {
                match *op {
                    Op::Equal { b, .. } | Op::Insert { b } => rebuilt.push(nl[b]),
                    Op::Delete { .. } => {}
                }
            }
            assert_eq!(rebuilt, nl, "{o:?} -> {n:?}");
            // And deleting the insertions gives the old file back.
            let mut back: Vec<&str> = Vec::new();
            for op in &d.ops {
                match *op {
                    Op::Equal { a, .. } | Op::Delete { a } => back.push(ol[a]),
                    Op::Insert { .. } => {}
                }
            }
            assert_eq!(back, ol, "{o:?} -> {n:?}");
        }
    }

    #[test]
    fn a_one_line_change_in_a_large_file_is_cheap_and_local() {
        let old: Vec<String> = (0..5000).map(|i| format!("line {i}")).collect();
        let mut new = old.clone();
        new[2500] = "line 2500 CHANGED".into();
        let o: Vec<&str> = old.iter().map(|s| s.as_str()).collect();
        let n: Vec<&str> = new.iter().map(|s| s.as_str()).collect();
        let d = diff_lines(&o, &n);
        assert!(!d.degraded);
        let hs = hunks(&d, 3);
        assert_eq!(hs.len(), 1);
        assert_eq!(hs[0].rows.len(), 8, "3 context each side plus - and +");
    }

    #[test]
    fn two_unrelated_files_degrade_rather_than_stall() {
        let old: Vec<String> = (0..3000).map(|i| format!("aaa {i}")).collect();
        let new: Vec<String> = (0..3000).map(|i| format!("bbb {}", i * 7)).collect();
        let o: Vec<&str> = old.iter().map(|s| s.as_str()).collect();
        let n: Vec<&str> = new.iter().map(|s| s.as_str()).collect();
        let d = diff_lines_with(&o, &n, 64);
        assert!(d.degraded, "the cap must be reachable");
        // And it is still a valid script.
        assert_eq!(d.ops.len(), 6000);
        // The degradation is announced, not silent.
        let cfg = DiffConfig {
            palette: Palette::None,
            ..Default::default()
        };
        let out = render(&o, &n, &cfg);
        assert!(out[0].contains("gave up"), "{:?}", out[0]);
    }

    #[test]
    fn a_renamed_variable_highlights_only_the_name() {
        let o = ["    let total = a + b;"];
        let n = ["    let sum = a + b;"];
        let (os, ns) = word_spans(o[0], n[0]).expect("similar lines must pair");
        assert_eq!(&o[0][os[0].0..os[0].1], "total");
        assert_eq!(&n[0][ns[0].0..ns[0].1], "sum");
    }

    #[test]
    fn two_unrelated_lines_are_not_word_highlighted() {
        // Otherwise the whole line is emphasis, which is the same as none.
        assert!(word_spans("let total = a + b;", "impl Display for Widget {}").is_none());
    }

    #[test]
    fn tabs_are_expanded_before_the_width_is_measured() {
        assert_eq!(expand_tabs("\tif x {", 4), "    if x {");
        assert_eq!(expand_tabs("ab\tc", 4), "ab  c");
        assert_eq!(width::width(&expand_tabs("a\tb", 4)), 5);
    }

    #[test]
    fn rendering_respects_the_width_and_the_row_cap() {
        let old: Vec<String> = (0..200).map(|i| format!("old line number {i}")).collect();
        let new: Vec<String> = (0..200).map(|i| format!("new line number {i}")).collect();
        let o: Vec<&str> = old.iter().map(|s| s.as_str()).collect();
        let n: Vec<&str> = new.iter().map(|s| s.as_str()).collect();
        let cfg = DiffConfig {
            width: 40,
            max_rows: 20,
            ..Default::default()
        };
        let out = render(&o, &n, &cfg);
        for l in &out {
            assert!(width::width(l) <= 40, "{} cols: {l:?}", width::width(l));
        }
        assert!(
            out.iter().any(|l| l.contains("more diff lines not shown")),
            "the cap must disclose what it dropped"
        );
    }

    #[test]
    fn painting_a_diff_does_not_change_the_text() {
        let o = ["    let total = a + b;", "keep"];
        let n = ["    let sum = a + b;", "keep"];
        let cfg = DiffConfig {
            width: 200,
            palette: Palette::Colour,
            ..Default::default()
        };
        let out = render(&o, &n, &cfg);
        let joined: String = out
            .iter()
            .map(|l| {
                width::cells(l)
                    .iter()
                    .map(|c| c.text)
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("let total = a + b;"), "{joined}");
        assert!(joined.contains("let sum = a + b;"), "{joined}");
    }
}
