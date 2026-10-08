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
//! **That pairing is a consumer of an ordering guarantee, so the guarantee is
//! written down.** Within one changed region the algorithm lists every deletion
//! before every addition — [`diff_lines`] states it, [`pair_rows`] depends on it,
//! and `deletions_precede_additions_within_a_changed_region` pins it. It holds
//! today because of how Myers' walk is recovered, which is exactly the kind of
//! thing a rewrite takes away without failing anything.
//!
//! # One number column, not two
//!
//! The unified view's gutter carries the line's number **in its own file**: the old
//! file's on a deletion and on a context row, the new file's on an addition. Two
//! columns — old beside new — fill both on a context row and then leave one of them
//! blank on every changed line, because a `-` has no new number and a `+` has no old
//! one; half the gutter is empty exactly where the reader is looking. Collapsed, no
//! row is ever blank, which is the whole argument for collapsing it. Right-aligned,
//! and `numw` still spans both files' numbering because the one column carries
//! either. The split view ([`crate::sidediff`]) keeps two numbers, one per panel,
//! and that is a different thing: two panels over two files are two files' lines.
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
//!   needs a worker thread to be worth anything. What *is* here is the property
//!   that motif existed for — a multi-line string that opens above the hunk keeps
//!   its scope — because [`crate::sidediff`] highlights the **whole excerpt**
//!   rather than one hunk at a time, so a construct spanning lines sees its own
//!   context.

use crate::painter::Sgr;
use rano::style::{Palette, Role};
use rano::width::text::{self as width, RESET};

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
///
/// # A guarantee a consumer depends on: deletions precede additions
///
/// **Within one maximal run of non-`Equal` ops — one changed region — every
/// `Delete` comes before every `Insert`.** Given the old `a\nb\nc` and the new
/// `a\nx\nb\nc`, the script is `Equal(a) Delete(b) Insert(x) Equal(b) Equal(c)`
/// and never `… Insert(x) Delete(b) …`, even though both reconstruct the new file.
///
/// It falls out of Myers' greedy walk: between two snakes the path takes
/// `dx` horizontal and `dy` vertical steps, and the backtrack recovers them as the
/// removals then the additions of the region. It is therefore a property of the
/// **algorithm** rather than of anything this file asserts — which is exactly why
/// it is written down here and pinned by
/// `deletions_precede_additions_within_a_changed_region`. [`pair_rows`] silently
/// depends on it: it finds a run of removals followed by a run of additions and
/// pairs the k-th of each for the intra-line highlight. Reordering the two halves
/// of a region — or emitting an insert before its delete — would not fail to
/// compile, would not fail any existing test, and would silently take every
/// intra-line highlight away, because the scan would find no run of removals.
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
    while hi < old.len() - lo
        && hi < new.len() - lo
        && old[old.len() - 1 - hi] == new[new.len() - 1 - hi]
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
///
/// Rows are emitted in op order, so a hunk inherits [`diff_lines`]'s guarantee in
/// `Row` terms: within one changed region, every [`Row::Removed`] comes before
/// every [`Row::Added`]. [`pair_rows`] relies on that adjacency; see the note on
/// [`diff_lines`].
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
            old_start: if old_start == usize::MAX {
                0
            } else {
                old_start
            },
            new_start: if new_start == usize::MAX {
                0
            } else {
                new_start
            },
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
    /// Show the line's number in a gutter.
    ///
    /// **One column, not two.** The unified view shows the line's own number in its
    /// own file — the old file's on a deletion and on a context row, the new file's on
    /// an addition — so no row ever carries a blank number. Two columns (old beside
    /// new) leave half the gutter empty on every changed line, which is what the
    /// operator's R1 is against: the second column is why a unified diff reads as a
    /// table with a missing half rather than as a list with a margin.
    ///
    /// The split view in [`crate::sidediff`] keeps its own two numbers, one per panel,
    /// and that is a different thing: two panels over two files are two files' lines.
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
        out.push(cfg.palette.painted(Role::Faint, "no change"));
        return out;
    }
    if d.degraded {
        out.push(cfg.palette.painted(
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
    for h in hs.iter() {
        // **A HEADER BEFORE EVERY HUNK, INCLUDING THE ONLY ONE.** This used to read
        // `if hi > 0 || hs.len() > 1`, which is git's diff *between* hunks and not
        // git's diff: a real `@@ -a,b +c,d @@` precedes the FIRST hunk too, and the
        // condition meant the common small edit — one hunk — drew with no `@@` at
        // all. The operator, 2026-10-05: *"sometimes your Edited card doesnt have
        // file name and the `@@` tags."* The single-hunk case was the *sometimes*.
        // `hi` stays in scope for nothing here now; the enumerate is kept for the
        // count the seam below wants.
        {
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
            if rows_left == 0 {
                dropped += 1;
            } else {
                rows_left -= 1;
                out.push(cfg.palette.painted(
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
        out.push(cfg.palette.painted(
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
    // **One number column, not two.** The line's own number in its own file: the
    // old file's on a deletion and on a context row, the new file's on an
    // addition. Two columns would print a number beside a blank on every changed
    // line — `-` has no new number and `+` has no old one — and collapsing them
    // is the point: a row always says which line of some file it is.
    //
    // Right-aligned, and `numw` still spans **both** files' numbering, because
    // the one column carries either.
    let (sign, role, text, num) = match *r {
        Row::Context { a, .. } => (
            " ",
            Role::Plain,
            old.get(a).copied().unwrap_or(""),
            old_base + a + 1,
        ),
        Row::Removed { a } => (
            "-",
            Role::Removed,
            old.get(a).copied().unwrap_or(""),
            old_base + a + 1,
        ),
        Row::Added { b } => (
            "+",
            Role::Added,
            new.get(b).copied().unwrap_or(""),
            new_base + b + 1,
        ),
    };
    let gutter = if cfg.line_numbers {
        format!("{num:>numw$} ")
    } else {
        String::new()
    };
    let body_w = cfg.width.saturating_sub(width::width(&gutter) + 1).max(8);
    // Tabs must be expanded before wrapping or the width is a lie.
    let text = expand_tabs(text, TAB_STOP);
    let body = paint_with_emphasis(&text, role, emph, cfg.palette);
    let wrapped = width::wrap(&body, body_w);
    // The gutter takes the line's own foreground on a changed row — the same
    // rule the split renderer's number follows — and stays dim on a context
    // row.
    let gut = cfg.palette.painted(
        if role == Role::Plain {
            Role::Faint
        } else {
            role.foreground()
        },
        &gutter,
    );
    let mut out = Vec::with_capacity(wrapped.len());
    for (i, l) in wrapped.into_iter().enumerate() {
        if i == 0 {
            // The sign keeps the foreground the role always had; the line's
            // own paint is background-only, so the text keeps its original
            // foregrounds (the operator's ruling on the first cube tint).
            out.push(format!(
                "{gut}{}{l}",
                cfg.palette.painted(role.foreground(), sign)
            ));
        } else {
            // A wrapped continuation keeps the colour and loses the sign, so the
            // eye does not read it as a second changed line.
            out.push(format!(
                "{}{}{l}",
                cfg.palette
                    .painted(Role::Faint, &" ".repeat(width::width(&gutter))),
                " "
            ));
        }
    }
    out
}

/// Paint a line, emphasising the byte ranges in `emph`.
fn paint_with_emphasis(text: &str, role: Role, emph: Option<&Spans>, p: Palette) -> String {
    let base = p.sgr(role);
    let em = p.sgr(Role::Emphasis);
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
///
/// **This depends on [`diff_lines`]'s ordering guarantee** — one changed region is
/// a run of [`Row::Removed`] followed by a run of [`Row::Added`], never the
/// reverse and never interleaved. The scan below finds each run in turn and pairs
/// the k-th removal with the k-th addition; given the other order it would find no
/// run of removals at all and quietly return no emphasis anywhere, which is a
/// silent loss of the whole intra-line highlight rather than a visible failure.
/// `pair_rows_pairs_the_k_th_removal_with_the_k_th_addition` pins the pair, and
/// `deletions_precede_additions_within_a_changed_region` pins the order it needs.
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
            let ol = expand_tabs(ol, TAB_STOP);
            let nl = expand_tabs(nl, TAB_STOP);
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

/// **Where a tab lands, for every renderer in this workspace that shows code.**
///
/// One constant because three renderers draw the same fact — the unified diff, the
/// side-by-side diff and the markdown fence — and a reader who sees an indent four
/// deep in one and eight in another cannot tell which is lying. Eight is what a
/// terminal does with a raw tab, which is what the fence used to hand it; three of
/// these are ours, so the stop is a decision, and a decision gets one home.
///
/// Four rather than eight because that is the stop the diff has always used and the
/// one leticl's `classed-segments` agrees to; two heads must not disagree about how
/// deep an indent is.
pub const TAB_STOP: usize = 4;

/// Expand tabs to a tab stop. A diff that measures a tab as one column
/// mis-aligns every line that has one, which in Go and Makefiles is all of them.
///
/// **The stop is measured in EMITTED COLUMNS, not in the source index**, and the
/// difference is a run of tabs: `"\t\t"` is eight columns, not seven. A version that
/// computed each stop from the character's index would place the second tab at
/// `(1 / 4 + 1) * 4 = 4` and pad it by three, because the first tab had already
/// pushed the text four columns right while advancing the index by one. The same
/// arithmetic is wrong after any double-width character, for the same reason.
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
            col += rano::width::char_width(c);
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
    ///
    /// And the number is one column, so every row carries one: context and the
    /// deletion take the old file's, the addition the new file's.
    #[test]
    fn an_excerpt_is_numbered_from_where_it_starts_in_the_file() {
        let cfg = DiffConfig {
            width: 80,
            palette: Palette::None,
            context: 1,
            line_numbers: true,
            intra_line: false,
            max_rows: 60,
        };
        let old = ["a", "b", "c"];
        let new = ["a", "B", "c"];
        let rows = render_from(&old, &new, &cfg, 310, 310);
        // Pinned whole rather than probed: the gutter's shape *is* the requirement,
        // so `310  a` / `311 -b` / `311 +B` / `312  c` — three columns of number,
        // then the sign, then the code — is the assertion. The `@@` now precedes
        // the only hunk, as it does in a real diff (see `render_from`).
        assert_eq!(
            rows,
            vec![
                "@@ -310,3 +310,3 @@",
                "310  a",
                "311 -b",
                "311 +B",
                "312  c"
            ],
            "{:?}",
            rows.join("\n")
        );
        // `render` is the same thing from line 1, with the numbers one column wide.
        let from_one = render(&old, &new, &cfg);
        assert_eq!(
            from_one,
            vec!["@@ -1,3 +1,3 @@", "1  a", "2 -b", "2 +B", "3  c"],
            "{from_one:?}"
        );
    }

    /// **The single column never shows a blank.** Two columns printed an old number
    /// beside an empty cell on every deletion and the reverse on every addition —
    /// half the gutter empty on every changed line, which is what the requirement is
    /// against. Every row of a real diff must carry a number in the one column.
    #[test]
    fn every_row_of_a_diff_carries_a_number_in_the_one_column() {
        let cfg = DiffConfig {
            width: 80,
            palette: Palette::None,
            context: 2,
            line_numbers: true,
            intra_line: false,
            max_rows: 60,
        };
        let old = ["a", "b", "c", "d", "e", "f"];
        let new = ["a", "B", "c", "D", "E", "f"];
        let rows = render_from(&old, &new, &cfg, 1, 1);
        // Drop the hunk header only; every code row is checked.
        for r in rows.iter().filter(|r| !r.starts_with("@@")) {
            // `<numw> <sign><code>`: a digit, space, sign. There is no row whose
            // number column is empty, and none with a second column of padding.
            let sign = r.chars().nth(2);
            assert!(
                matches!(sign, Some(' ' | '-' | '+')),
                "row {r:?} has no number then sign in the one column"
            );
            assert!(
                r.chars().next().is_some_and(|c| c.is_ascii_digit()),
                "row {r:?} has a blank number column"
            );
        }
        // And every row is numbered by its own file — old on a context row and on a
        // deletion, new on an addition: 1 a, 2 -b, 2 +B, 3 c, 4 -d, 5 -e, 4 +D,
        // 5 +E, 6 f. The two deletions come before the two additions, which is the
        // invariant `deletions_precede_additions_within_a_changed_region` pins.
        assert_eq!(
            rows.iter()
                .filter(|r| !r.starts_with("@@"))
                .map(|r| r.as_str())
                .collect::<Vec<_>>(),
            vec![
                "1  a", "2 -b", "2 +B", "3  c", "4 -d", "5 -e", "4 +D", "5 +E", "6  f"
            ]
        );
    }

    /// **A wrapped continuation keeps the body's column.** `body_w` and the
    /// continuation's blank prefix are both derived from the gutter's width, so the
    /// gutter narrowing by `numw + 1` widens the body by the same amount and the
    /// continuation lines up under the first. Neither number is written down here:
    /// the assertion is that the two agree, at a width where wrapping happens.
    #[test]
    fn a_wrapped_continuation_starts_where_the_body_starts() {
        let long = "x".repeat(120);
        let other = "y".repeat(120);
        let old = [long.as_str(), "b"];
        let new = [other.as_str(), "b"];
        for width in [24usize, 40, 100] {
            let cfg = DiffConfig {
                width,
                palette: Palette::None,
                context: 0,
                line_numbers: true,
                intra_line: false,
                max_rows: 60,
            };
            let rows = render(&old, &new, &cfg);
            assert!(rows.len() > 2, "width {width} must wrap: {rows:?}");
            // Row 0 is the hunk header; row 1 is `<num> -<code>`; a continuation is
            // the same gutter's width of blanks, then a space where the sign was,
            // then the code — so the code stays in its column and only the sign's
            // does not. Both widths are derived from the gutter, so neither is
            // written down here: the assertion is that the two agree at a width
            // where wrapping happens.
            let body_col = rows[1].find('x').expect("code on the first diff row");
            assert_eq!(
                rows[2].find('x').expect("code on the continuation"),
                body_col,
                "width {width}: {rows:?}"
            );
            // And the sign's column is blank rather than a second number.
            assert!(
                !rows[2].chars().take(body_col).any(|c| c.is_ascii_digit()),
                "a continuation carries no number: {:?}",
                rows[2]
            );
        }
    }

    fn lines(s: &str) -> Vec<&str> {
        s.lines().collect()
    }

    /// One maximal run of non-`Equal` ops is a changed region, and in every one of
    /// them no `Insert` precedes a `Delete`.
    fn regions_list_deletions_first(ops: &[Op]) -> bool {
        ops.split(|o| matches!(o, Op::Equal { .. })).all(|run| {
            let mut added = false;
            for o in run {
                match o {
                    Op::Insert { .. } => added = true,
                    Op::Delete { .. } if added => return false,
                    _ => {}
                }
            }
            true
        })
    }

    /// The same rule in `Row` terms, which is the form a consumer sees: one run of
    /// `Context`-bounded removed rows then added rows.
    fn rows_list_removals_first(rows: &[Row]) -> bool {
        rows.split(|r| matches!(r, Row::Context { .. })).all(|run| {
            let mut added = false;
            for r in run {
                match r {
                    Row::Added { .. } => added = true,
                    Row::Removed { .. } if added => return false,
                    _ => {}
                }
            }
            true
        })
    }

    /// **Deletions precede additions within a changed region — an invariant, not a
    /// coincidence, and a consumer depends on it.**
    ///
    /// Rows follow Myers op order (`Op::Delete` then `Op::Insert` for a region), and
    /// [`pair_rows`] relies on the adjacency when it pairs a removed run with an
    /// added run for the intra-line emphasis. Nothing else in the crate states it:
    /// it is a property of how the walk is recovered, so a rewrite of `myers` or
    /// `backtrack` could take it away without failing to compile, failing any other
    /// test, or even changing the rendered diff — the only symptom would be that
    /// every intra-line highlight silently disappeared.
    ///
    /// Named cases first, then a pseudo-random corpus over a tiny alphabet, because
    /// the interesting regions are the ones with several deletions *and* several
    /// insertions, where the k-path can zig-zag — and that is exactly where a
    /// hand-written case stops looking.
    #[test]
    fn deletions_precede_additions_within_a_changed_region() {
        let named: [(&str, &str); 9] = [
            // A substitution: one each side.
            ("a\nb\nc", "a\nB\nc"),
            // Two deletions and two insertions in one region — four alignments have
            // the same D, and the walk has to pick one of them.
            ("a\nb\nc\nd\ne", "a\nB\nC\nD\nE"),
            ("a\nb\nc\nd\ne\nf", "a\nB\nc\nD\nE\nf"),
            // Uneven runs.
            ("a\nb\nc", "a\nX\nY\nZ\nc"),
            ("a\nb\nc\nd\ne", "a\nc"),
            // A region that starts the file and one that ends it.
            ("a\nb", "X\nY\na\nb"),
            ("a\nb\nc", "a\nb\nZ"),
            // Every line replaced, and a wholesale reversal.
            ("a\nb\nc", "x\ny\nz"),
            ("a\nb\nc", "c\nb\na"),
        ];
        for (o, n) in named {
            let (ol, nl) = (lines(o), lines(n));
            let d = diff_lines(&ol, &nl);
            assert!(
                regions_list_deletions_first(&d.ops),
                "{o:?} -> {n:?}: {:?}",
                d.ops
            );
            for h in hunks(&d, 3) {
                assert!(
                    rows_list_removals_first(&h.rows),
                    "{o:?} -> {n:?}: {:?}",
                    h.rows
                );
            }
        }

        // The same for the path that gives up: a degraded script is every deletion
        // then every insertion, which satisfies the rule by construction — asserted
        // rather than assumed, since it is the branch a reader would not check.
        let big: Vec<String> = (0..400).map(|i| format!("aaa {i}")).collect();
        let other: Vec<String> = (0..400).map(|i| format!("zzz {}", i * 3)).collect();
        let (bl, bnl): (Vec<&str>, Vec<&str>) = (
            big.iter().map(String::as_str).collect(),
            other.iter().map(String::as_str).collect(),
        );
        let d = diff_lines_with(&bl, &bnl, 16);
        assert!(
            d.degraded,
            "the cap must be reachable, or this case proves nothing"
        );
        assert!(regions_list_deletions_first(&d.ops), "the degraded path");

        // And the corpus.
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..4000 {
            let n = (next() % 14) as usize;
            let m = (next() % 14) as usize;
            let old: Vec<String> = (0..n).map(|_| format!("l{}", next() % 5)).collect();
            let new: Vec<String> = (0..m).map(|_| format!("l{}", next() % 5)).collect();
            let (ol, nl): (Vec<&str>, Vec<&str>) = (
                old.iter().map(String::as_str).collect(),
                new.iter().map(String::as_str).collect(),
            );
            let d = diff_lines(&ol, &nl);
            assert!(
                regions_list_deletions_first(&d.ops),
                "{old:?} -> {new:?}: {:?}",
                d.ops
            );
            for h in hunks(&d, 1) {
                assert!(
                    rows_list_removals_first(&h.rows),
                    "{old:?} -> {new:?}: {:?}",
                    h.rows
                );
            }
        }
    }

    /// **And the consumer is for real.** `pair_rows` pairs the k-th removal of a run
    /// with the k-th addition of the run that follows it, so reversing the two halves
    /// of a region takes the emphasis away entirely — a silent loss of the whole
    /// intra-line feature rather than a visible failure, which is why
    /// `deletions_precede_additions_within_a_changed_region` exists.
    #[test]
    fn pair_rows_pairs_the_k_th_removal_with_the_k_th_addition() {
        let old = ["    let total = a + b;", "    let count = n;"];
        let new = ["    let sum = a + b;", "    let tally = n;"];
        let rows = vec![
            Row::Removed { a: 0 },
            Row::Removed { a: 1 },
            Row::Added { b: 0 },
            Row::Added { b: 1 },
        ];
        let spans = pair_rows(&rows, &old, &new);
        let word = |text: &str, s: &Option<Spans>| {
            let s = s.as_ref().expect("emphasis");
            text[s[0].0..s[0].1].to_string()
        };
        assert_eq!(word(old[0], &spans[0]), "total");
        assert_eq!(word(new[0], &spans[2]), "sum");
        assert_eq!(word(old[1], &spans[1]), "count");
        assert_eq!(word(new[1], &spans[3]), "tally");

        // The same two rows in the other order pair with nothing at all, which is
        // the failure the invariant prevents.
        let flipped = vec![Row::Added { b: 0 }, Row::Removed { a: 0 }];
        let spans = pair_rows(&flipped, &[old[0]], &[new[0]]);
        assert!(
            spans.iter().all(Option::is_none),
            "an addition before its deletion must find no pair: {spans:?}"
        );
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
        // **THE RUN IS THE CASE THAT SEPARATES the two implementations**, and it is
        // the one no test covered until 2026-10-02 — which is why `sidediff` carried a
        // second, wrong copy of this arithmetic for as long as it did. A stop computed
        // from the source INDEX makes the second of two tabs three columns wide, since
        // the first tab moved the text four columns while advancing the index by one:
        // seven where a terminal shows eight.
        assert_eq!(expand_tabs("\t\treturn", 4), "        return");
        assert_eq!(width::width(&expand_tabs("\t\treturn", 4)), 14);
        // And the same arithmetic after a double-width character, which is the other
        // way index and column part company. Two CJK chars are FOUR columns, so the tab
        // lands exactly ON the stop and pads four more — five spaces before the `x`,
        // counting the literal one. (Written out because the first version of this line
        // guessed three: the arithmetic that pads by `stop - (col % stop)` is easy to
        // do in the head and get wrong, which is the whole reason it lives in one
        // function.) An index-based stop would have called the column 2 and padded by
        // two, putting the `x` three columns left of where a terminal puts it.
        assert_eq!(
            expand_tabs("\u{4e2d}\u{6587}\t x", 4),
            "\u{4e2d}\u{6587}     x"
        );
        // The stop is the workspace's one constant, not a literal at each call site.
        assert_eq!(TAB_STOP, 4, "both heads agree an indent is four deep");
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
            .map(|l| width::cells(l).iter().map(|c| c.text).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("let total = a + b;"), "{joined}");
        assert!(joined.contains("let sum = a + b;"), "{joined}");
    }
}
