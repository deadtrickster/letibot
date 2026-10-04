//! The two-panel file-edit diff: before on the left, after on the right.
//!
//! # Provenance
//!
//! The shape is **opencode**'s (MIT) diff viewer,
//! `packages/tui/src/feature-plugins/system/diff-viewer.tsx` in the checkout
//! this box carries (v1.18.29): a `split | unified` view, line
//! numbers both sides, the change carried by the sign column rather than by
//! repainting the line, and syntax colouring over the whole panel. What is
//! different, and why:
//!
//! - opencode distinguishes added/removed by **background tints** and lets the
//!   sign colour carry the rest. A terminal head that must survive
//!   [`Palette::None`] cannot spend the diff on backgrounds, so here the sign
//!   is the carrier — a glyph, not a colour — and the code keeps its syntax
//!   colours in both panels. The information survives a pipe to a file, which
//!   is the same bar [`crate::diff`]'s three-glyph bar already set. The
//!   operator's trial puts the tint **on top of** the glyph for colour
//!   terminals: the row is painted inside its own role, so an added line is
//!   green to its full width and a removed one red, while the glyph still
//!   carries the distinction alone when there is no palette at all.
//! - opencode highlights with its own tree-sitter integration; this crate
//!   embeds **rano**'s (`~/Projects/rano`, `syntax::Highlighter::classes`) and
//!   maps its capture names onto this crate's six syntax [`Role`]s. The
//!   capture → token decision is rano's; the capture → colour decision is
//!   ours, because a palette is a decision about a terminal.
//! - opencode picks split or unified from the pane's width (split only above a
//!   hundred columns). Here the choice is the **operator's** `/diff` toggle and
//!   nothing else — a narrow pane gets a narrow split rather than no diff,
//!   because an edit drawn cramped is an edit the operator can still read, and
//!   an edit not drawn is one they approved blind. The renderer degrades
//!   gracefully under [`MIN_BODY`]; it does not refuse.
//!
//! # Why the panels are re-diffed from excerpts
//!
//! The input is the bounded before/after a `ToolFinished` event carries (see
//! `letibot-tools`' `ToolEditExcerpt`): the changed region plus context, line
//! numbers preserved by `before_start` / `after_start`. The panels are aligned
//! by diffing those excerpts again — [`crate::diff`] owns the only edit script
//! in the process, and a second, worse one here is how two answers to one
//! question start disagreeing.
//!
//! # Everything here is a pure function of its inputs
//!
//! No clock, no filesystem, no terminal. The tree-sitter parse is deterministic
//! in the source text, so the output is too, and the tests need no fixture
//! beyond a string.

use crate::diff::{DiffConfig, Hunk, Row, diff_lines, hunks};
use crate::highlight::role_for_capture;
use crate::style::{Painter, Palette, Role};
use crate::width;

/// How the two panels are coloured, and where their numbers start.
pub struct SplitConfig<'a> {
    /// Width, palette, line numbers, context and the row cap come from the
    /// same config the unified renderer uses; `width` is the **full** row and
    /// the panels split it.
    pub cfg: &'a DiffConfig,
    /// 1-based line of the old file the left panel starts at, so the gutter
    /// numbers the file and not the excerpt.
    pub before_start: usize,
    /// 1-based line of the new file the right panel starts at.
    pub after_start: usize,
    /// The language both panels are coloured in, from the edited file's name.
    /// `None` — unknown extension, unknown grammar — renders plain, which is
    /// the same thing a `Palette::None` terminal reads anyway.
    pub lang: Option<rano::syntax::Lang>,
}

/// The separator between the panels, and the air either side of it.
const SEP: &str = " │ ";
const SEP_W: usize = 3;
/// A panel body narrower than this cannot show code and its gutter at the
/// same time; the renderer degrades rather than overprints, which is cheaper
/// than a caller that has to guess whether the panels will fit.
const MIN_BODY: usize = 8;

/// Render the two-panel view of `old` → `new`.
///
/// One output row per terminal row: left gutter, sign and code, the
/// separator, right gutter, sign and code. A line that wraps keeps its
/// continuation under its own panel and blanks the other, so the eye reads
/// the pair and not the wrap.
pub fn render_split(old: &[&str], new: &[&str], sc: &SplitConfig) -> Vec<String> {
    let p = sc.cfg.palette;
    // **Tabs first, once, before anything colours or measures a line.**
    //
    // This used to happen inside `paint_classed`, per character, with the stop computed
    // from the character's index in the SOURCE — which is a different number from the
    // column it lands on the moment a line has more than one tab, or a double-width
    // character before it. `"\t\t"` came out seven columns wide instead of eight.
    //
    // Expanding HERE rather than in the painter is what makes the class grid line up:
    // `class_grid` colours the same strings this produces, so a grid indexed by expanded
    // character and a line of expanded characters are the same grid. Expanding later — in
    // the painter, after the classes were computed on the source — is the arrangement
    // that made the two disagree, and the reason this stop had to be duplicated.
    //
    // It is also what the unified diff already does, in the same words: *"Tabs must be
    // expanded before wrapping or the width is a lie."*
    let old_x: Vec<String> = old
        .iter()
        .map(|l| crate::diff::expand_tabs(l, crate::diff::TAB_STOP))
        .collect();
    let new_x: Vec<String> = new
        .iter()
        .map(|l| crate::diff::expand_tabs(l, crate::diff::TAB_STOP))
        .collect();
    let old: Vec<&str> = old_x.iter().map(String::as_str).collect();
    let new: Vec<&str> = new_x.iter().map(String::as_str).collect();
    let (old, new) = (&old[..], &new[..]);
    let d = diff_lines(old, new);
    let hs = hunks(&d, sc.cfg.context);
    let mut out = Vec::new();
    if hs.is_empty() {
        out.push(p.paint(Role::Faint, "no change"));
        return out;
    }
    if d.degraded {
        out.push(p.paint(
            Role::Attention,
            "! diff gave up on the minimal edit script; \
             the changed region is shown as a whole replacement",
        ));
    }

    let g = Geometry::of(sc, old, new);
    let old_classes = class_grid(old, sc.lang);
    let new_classes = class_grid(new, sc.lang);

    let mut budget = sc.cfg.max_rows;
    let mut dropped = 0usize;
    for h in hs.iter() {
        // **A HEADER BEFORE EVERY HUNK, INCLUDING THE ONLY ONE** — the same
        // correction `diff::render_from` records, and for the same reported
        // defect: the single-hunk edit is the common one, and `hi > 0 ||
        // hs.len() > 1` drew it with no `@@` at all.
        {
            if budget == 0 {
                dropped += 1;
            } else {
                budget -= 1;
                out.push(hunk_header(h, sc, p));
            }
        }
        for pair in pair_rows(&h.rows) {
            let lines = render_pair(&pair, old, new, &old_classes, &new_classes, sc, &g);
            if budget >= lines.len() {
                budget -= lines.len();
                out.extend(lines);
            } else {
                // The pair does not fit whole, and half a pair is worse than
                // none: an aligned row with one panel missing reads as a
                // deletion or an insertion that never happened.
                dropped += lines.len();
                budget = 0;
            }
        }
    }
    if dropped > 0 {
        out.push(p.paint(
            Role::Faint,
            &format!("… {dropped} more diff lines not shown"),
        ));
    }
    out
}

/// The row geometry, computed once per render.
struct Geometry {
    /// Visible width of the LEFT panel, separator included in nothing.
    ///
    /// **The two panels are not the same width, and that is the correction** (R53 §1.3). One
    /// `(width - SEP_W) / 2` was used for both, so an odd remainder was **thrown away**: at width
    /// 100 the row came to 99 columns, with two 48-column panels where there was room for a 49th
    /// on the right. The visible cost is that a 49-column line — the longest line a full-width
    /// half can hold — wrapped on this head and fitted on leticl, which gives the remainder to the
    /// right panel (`src/sidediff.lisp:291-292`, `:331`, `:339-341`).
    panel_w: usize,
    /// Visible width of the RIGHT panel: what the left one took, and the remainder after the
    /// separator. Equal on an even split; one column wider when the division is odd.
    panel_w_right: usize,
    /// Visible width of the code part of a cell.
    body_w: usize,
    /// Digits in the largest line number either panel can show.
    numw: usize,
}

impl Geometry {
    fn of(sc: &SplitConfig, old: &[&str], new: &[&str]) -> Geometry {
        // **The remainder goes to the RIGHT panel**, which is leticl's arithmetic and the whole of
        // this correction: `available = width - SEP_W`, `left = available / 2`, `right = available -
        // left`. The two therefore sum with the separator to exactly the width asked for — 48 + 3 +
        // 49 = 100 — where the old pair of 48s summed to 99.
        let available = sc.cfg.width.saturating_sub(SEP_W);
        let panel_w = available / 2;
        let panel_w_right = available - panel_w;
        let numw = if sc.cfg.line_numbers {
            (sc.before_start + old.len())
                .max(sc.after_start + new.len())
                .max(1)
                .to_string()
                .len()
        } else {
            0
        };
        // The gutter a cell carries before its code: number, space, sign,
        // space — or just sign and space when line numbers are off.
        let gutter_w = if sc.cfg.line_numbers { numw + 3 } else { 2 };
        let body_w = panel_w.saturating_sub(gutter_w).max(MIN_BODY);
        Geometry {
            panel_w,
            panel_w_right,
            body_w,
            numw,
        }
    }
}

fn hunk_header(h: &Hunk, sc: &SplitConfig, p: Palette) -> String {
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
    p.paint(
        Role::Faint,
        &format!(
            "@@ -{},{} +{},{} @@",
            sc.before_start + h.old_start,
            count_old,
            sc.after_start + h.new_start,
            count_new
        ),
    )
}

/// One aligned pair: what the left panel shows and what the right shows.
/// `None` is an empty panel — a pure insertion has no left, a pure deletion
/// no right, and a wrap continuation blanks whichever side ran out.
struct Pair {
    left: Option<Half>,
    right: Option<Half>,
}

struct Half {
    /// 0-based index into the excerpt's lines.
    line: usize,
    sign: char,
    role: Role,
}

/// Align a hunk's rows into side-by-side pairs.
///
/// A run of removals pairs index-wise with the run of additions beside it —
/// the edit script is not required to order one before the other, so whichever
/// run comes first is taken and the opposite run is taken after it — and a
/// context row pairs with itself. Leftovers pad with an empty panel, which is
/// what makes an insertion read as *appeared* rather than as *changed*.
fn pair_rows(rows: &[Row]) -> Vec<Pair> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < rows.len() {
        match rows[i] {
            Row::Context { a, b } => {
                out.push(Pair {
                    left: Some(Half {
                        line: a,
                        sign: ' ',
                        role: Role::Plain,
                    }),
                    right: Some(Half {
                        line: b,
                        sign: ' ',
                        role: Role::Plain,
                    }),
                });
                i += 1;
            }
            _ => {
                let mut removed = Vec::new();
                let mut added = Vec::new();
                while i < rows.len() {
                    match rows[i] {
                        Row::Removed { a } => removed.push(a),
                        Row::Added { b } => added.push(b),
                        Row::Context { .. } => break,
                    }
                    i += 1;
                }
                let n = removed.len().max(added.len());
                for k in 0..n {
                    out.push(Pair {
                        left: removed.get(k).map(|&a| Half {
                            line: a,
                            sign: '-',
                            role: Role::Removed,
                        }),
                        right: added.get(k).map(|&b| Half {
                            line: b,
                            sign: '+',
                            role: Role::Added,
                        }),
                    });
                }
            }
        }
    }
    out
}

/// One pair into one or more terminal rows.
fn render_pair(
    pair: &Pair,
    old: &[&str],
    new: &[&str],
    old_classes: &[Vec<Role>],
    new_classes: &[Vec<Role>],
    sc: &SplitConfig,
    g: &Geometry,
) -> Vec<String> {
    // **BOTH LOOKUPS ARE GUARDED, and they were not.** The line list was bounds-checked and fell
    // back to `""`; the class grid beside it was a bare index — two lookups on one `h.line` in one
    // expression, with the guard on only the first. Whoever wrote the guard on `old` knew `h.line`
    // could exceed the excerpt; the grid did not get the same care.
    //
    // **The grid really can be shorter, and by more than a little.** `class_grid` returns
    // `vec![Vec::new(); lines.len()]` for an unknown language — the right length — but otherwise
    // returns what `rano`'s highlighter gives it, and that has **two early returns of
    // `Vec::new()`**: a parser that will not take the language, and a query that will not compile
    // against it. Zero rows, so the panic is at index `0` — the first line of the first hunk, not
    // some deep edge of a long file.
    //
    // **Latent rather than live, said honestly rather than at its most alarming.** Neither early
    // return fires on this box today: both heads render split diffs and neither has crashed. Both
    // conditions are build-level facts, cached per language, so they arrive as a grammar-version
    // regression — which is exactly what a `cargo update` of a tree-sitter grammar produces, on a
    // path that renders while the operator watches. leticl bounds-checks the same access and pads a
    // short grid, so it degrades to plain text where this panicked.
    let left = pair.left.as_ref().map(|h| {
        (
            h,
            old.get(h.line).copied().unwrap_or(""),
            old_classes.get(h.line).map(Vec::as_slice).unwrap_or(&[]),
            sc.before_start + h.line,
        )
    });
    let right = pair.right.as_ref().map(|h| {
        (
            h,
            new.get(h.line).copied().unwrap_or(""),
            new_classes.get(h.line).map(Vec::as_slice).unwrap_or(&[]),
            sc.after_start + h.line,
        )
    });
    let left_lines = side_lines(left, sc, g, g.panel_w);
    let right_lines = side_lines(right, sc, g, g.panel_w_right);

    let rows = left_lines.len().max(right_lines.len());
    let mut out = Vec::with_capacity(rows);
    for k in 0..rows {
        let mut s = String::with_capacity(sc.cfg.width + 32);
        // A side that has run out of lines is blank, not a repeat of its
        // last line: the pair is aligned rows, and a repeated row would
        // read as content that is there twice.
        let l = left_lines.get(k).map(String::as_str).unwrap_or("");
        let r = right_lines.get(k).map(String::as_str).unwrap_or("");
        // **Each half pads to its OWN width.** Using the left width for both is what lost the
        // remainder column; see [`Geometry::panel_w_right`].
        s.push_str(&pad_to(l, g.panel_w));
        s.push_str(&sc.cfg.palette.paint(Role::Faint, SEP));
        s.push_str(&pad_to(r, g.panel_w_right));
        out.push(s);
    }
    out
}

/// One side of a pair: gutter, sign, code — painted, wrapped, one `String`
/// per terminal row. An absent half renders as one blank row so the opposite
/// side's wrap still has somewhere to go.
///
/// The whole cell is painted **inside the line's own role** (a
/// [`Painter::inside`] base): gutter, sign, code and the padding to the panel
/// edge all sit in it, so an added line is green to its full width and a
/// removed one red, not just where its text happens to reach. Two details
/// make the tint hold:
///
/// - a syntax span closes with a plain reset, which would end the background
///   at the first keyword — rebased, every span closes back to the line's
///   role and the tint runs on. A context row's base closes to a plain reset,
///   so the rebase is a no-op there and the bytes are what they always were;
/// - an SGR open never clears a background, so a cell that *ended* in its own
///   role would paint the separator and the panel after it green too. A
///   tinted cell therefore pads inside the role and then closes, handing the
///   separator a clean slate.
fn side_lines(
    half: Option<(&Half, &str, &[Role], usize)>,
    sc: &SplitConfig,
    g: &Geometry,
    // **This side's own width**, which is not always `g.panel_w`: the two panels differ by one
    // column when the division is odd. Padding the right cell to the LEFT panel's width left a
    // column of untinted space after its reset — the tint stopped one short of the panel and the
    // separator's clean slate started a character late. See [`Geometry::panel_w_right`].
    panel_w: usize,
) -> Vec<String> {
    let p = sc.cfg.palette;
    let Some((h, text, classes, num)) = half else {
        return vec![String::new()];
    };
    let tinted = h.role != Role::Plain && p.is_colour();
    let q = Painter::inside(p, h.role);
    let painted = paint_classed(text, classes, p);
    let painted = if tinted {
        q.rebase_resets(&painted)
    } else {
        painted
    };
    let mut wrapped = width::wrap(&painted, g.body_w);
    if wrapped.is_empty() {
        // An empty line is still a line: it takes a row, with its number.
        wrapped.push(String::new());
    }
    wrapped
        .into_iter()
        .enumerate()
        .map(|(k, body)| {
            let gutter = if k > 0 {
                // A continuation keeps its panel's colour and loses its
                // number, exactly as the unified renderer's continuation
                // loses its sign.
                q.paint(Role::Faint, &" ".repeat(g.numw + 1))
            } else if sc.cfg.line_numbers {
                // The number takes the line's own foreground on a changed
                // row — the operator, comparing notes: *"claude code does
                // change foregrounds but for line numbers and + - only"* —
                // which is what makes the gutter read as part of the change
                // rather than as furniture. A context row keeps the dim
                // number it always had.
                let fg = if tinted {
                    h.role.foreground()
                } else {
                    Role::Faint
                };
                q.paint(fg, &format!("{:>numw$} ", num, numw = g.numw))
            } else {
                String::new()
            };
            let sign = if k == 0 {
                // The sign keeps the foreground the role always had — green
                // and red — while the cell's base is background-only, so the
                // text keeps its own: the operator's *"keep original
                // foregrounds"*.
                q.paint(h.role.foreground(), &h.sign.to_string())
            } else {
                " ".to_string()
            };
            let mut cell = String::new();
            if tinted {
                // The role opens before the first visible column, or the
                // gutter's leading spaces would sit outside the tint: a dim
                // open sets an attribute, it does not set a background.
                cell.push_str(p.open(h.role));
            }
            cell.push_str(&format!("{gutter}{sign} {body}"));
            if tinted {
                let vis = width::width(&cell);
                if vis < panel_w {
                    cell.push_str(&" ".repeat(panel_w - vis));
                }
                cell.push_str(crate::width::RESET);
            }
            cell
        })
        .collect()
}

/// Pad a painted string to `w` visible columns.
fn pad_to(s: &str, w: usize) -> String {
    let vis = width::width(s);
    if vis >= w {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(w - vis))
    }
}

/// Paint a line by its syntax classes: runs of one role open and close
/// together.
///
/// **There is no tab arm here, deliberately.** The text reaching this function has
/// already been expanded by [`crate::diff::expand_tabs`] — see `render_split`, which
/// does it before the class grid is built so that the grid and the line are the same
/// grid. This function used to expand as it painted, computing each stop from the
/// source index; that is a second implementation of one fact, with its own bug, and
/// it is the shape this tree has already paid for twice.
fn paint_classed(text: &str, classes: &[Role], p: Palette) -> String {
    let mut out = String::with_capacity(text.len() + 16);
    let mut run = Role::Plain;
    let mut run_buf = String::new();
    for (i, ch) in text.chars().enumerate() {
        let role = classes.get(i).copied().unwrap_or(Role::Plain);
        if role != run {
            flush(&mut run, &mut run_buf, &mut out, p);
            run = role;
        }
        run_buf.push(ch);
    }
    flush(&mut run, &mut run_buf, &mut out, p);
    out
}

fn flush(run: &mut Role, run_buf: &mut String, out: &mut String, p: Palette) {
    if !run_buf.is_empty() {
        out.push_str(&p.paint(*run, run_buf));
        run_buf.clear();
    }
}

/// One class grid per excerpt, one role per character.
fn class_grid(lines: &[&str], lang: Option<rano::syntax::Lang>) -> Vec<Vec<Role>> {
    let Some(lang) = lang else {
        return vec![Vec::new(); lines.len()];
    };
    let src = lines.join("\n");
    let mut hl = rano::syntax::Highlighter::new();
    let grid = hl.classes(&src, lang);
    grid.into_iter()
        .map(|row| {
            row.into_iter()
                .map(|c| c.map(|n| role_for_capture(&n)).unwrap_or(Role::Plain))
                .collect()
        })
        .collect()
}

/// The language of an edited file, by the same extension table rano's editor
/// uses. Kept here so a head passes a path and nothing else.
pub fn lang_for(path: &str) -> Option<rano::syntax::Lang> {
    rano::syntax::detect(Some(std::path::Path::new(path)), None)
}

/// Everything a head calls with: the edited file's name and the bounded
/// before/after a `ToolFinished` event carried, in one call.
pub fn render_edit(
    path: &str,
    before: &str,
    after: &str,
    before_start: usize,
    after_start: usize,
    cfg: &DiffConfig,
) -> Vec<String> {
    let old: Vec<&str> = before.lines().collect();
    let new: Vec<&str> = after.lines().collect();
    let sc = SplitConfig {
        cfg,
        before_start,
        after_start,
        lang: lang_for(path),
    };
    render_split(&old, &new, &sc)
}

/// Which of the two shapes a file edit is drawn in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditView {
    /// opencode's two panels, before left and after right.
    Split,
    /// One panel, `-`/`+` signed, the file's own line numbers in the gutter —
    /// the shape claude code draws its edits in.
    Unified,
}

/// The view an edit gets, from what the operator asked for — and from nothing
/// else. Split when the toggle is on, unified when it is off, at any width:
/// the width used to gate this (`MIN_SPLIT_WIDTH = 100`) and the gate's two
/// answers were a cramped diff or no diff at all, and no diff at all is how an
/// operator ends up approving edits blind. The renderer degrades gracefully
/// under [`MIN_BODY`]; it does not refuse.
pub fn edit_view(split_wanted: bool) -> EditView {
    if split_wanted {
        EditView::Split
    } else {
        EditView::Unified
    }
}

/// [`render_edit`] in whichever view [`edit_view`] picks. There is always a
/// drawing: an edit the operator cannot see is an edit they did not approve.
pub fn render_edit_view(
    path: &str,
    before: &str,
    after: &str,
    before_start: usize,
    after_start: usize,
    cfg: &DiffConfig,
    view: EditView,
) -> Vec<String> {
    // **THE FILE'S OWN NAME, ON EVERY EDIT.** The operator, 2026-10-05: *"sometimes
    // your Edited card doesnt have file name and the `@@` tags."* Sometimes is the
    // word: the CARD's target line names the file when the call carried one, and
    // then a card that did not — a call first seen as `ToolStarted`, a log older
    // than `display_target` — drew a diff with no file name on it anywhere, and a
    // diff with no file name is a change to nothing in particular. Git's own shape
    // names the file above every diff however the caller found it; one faint line,
    // in the `@@` header's own register, is that rule at card scale. It sits in the
    // ONE seam both views pass through, so split and unified cannot disagree about
    // whether the name is there.
    let mut out = vec![cfg.palette.paint(Role::Faint, path)];
    match view {
        EditView::Split => out.extend(render_edit(
            path,
            before,
            after,
            before_start,
            after_start,
            cfg,
        )),
        EditView::Unified => {
            let old: Vec<&str> = before.lines().collect();
            let new: Vec<&str> = after.lines().collect();
            out.extend(crate::diff::render_from(
                &old,
                &new,
                cfg,
                before_start,
                after_start,
            ))
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sc(
        width: usize,
        palette: Palette,
        before_start: usize,
        after_start: usize,
    ) -> SplitConfig<'static> {
        // A leaked config is fine in a test: `DiffConfig` is `Copy` and the
        // leak keeps `SplitConfig<'a>` lifetimes out of every assertion.
        let cfg = Box::leak(Box::new(DiffConfig {
            width,
            palette,
            context: 1,
            line_numbers: true,
            intra_line: false,
            max_rows: 60,
        }));
        SplitConfig {
            cfg,
            before_start,
            after_start,
            lang: None,
        }
    }

    fn plain(rows: &[String]) -> Vec<String> {
        // Strip SGR so assertions read the text a `Palette::None` terminal
        // would show.
        rows.iter()
            .map(|r| r.replace('\x1b', "").replace("[0m", "").replace("[2m", ""))
            .collect()
    }

    /// **A class grid shorter than the excerpt renders plain instead of panicking** — R53 §1.3.
    ///
    /// The line list was bounds-checked and the grid beside it was a bare index, two lookups on
    /// one `h.line` in one expression with the guard on only the first. `rano` returns an EMPTY
    /// grid — zero rows, not fewer — when a parser will not take the language or a query will not
    /// compile against it, so the panic was at index `0`: the first line of the first hunk.
    ///
    /// Driven at `render_pair` rather than through a real language because that is where the
    /// length assumption lived, and because the condition it guards is a build-level fact about
    /// `rano` that a test cannot induce. The grid below is the exact shape the two early returns
    /// produce: the right length for a healthy file, zero for this one.
    #[test]
    fn a_grid_shorter_than_the_lines_renders_plain_rather_than_panicking() {
        let cfg = sc(100, Palette::None, 0, 0);
        let old = vec!["fn main() {}", "let x = 1;"];
        let new = vec!["fn main() {}", "let x = 2;"];
        let g = Geometry::of(&cfg, &old, &new);
        let pair = Pair {
            left: Some(Half {
                line: 1,
                sign: '-',
                role: Role::Failure,
            }),
            right: Some(Half {
                line: 1,
                sign: '+',
                role: Role::Success,
            }),
        };
        // **The empty grid `rano` returns**, for both sides: zero rows against two lines.
        let rows = render_pair(&pair, &old, &new, &[], &[], &cfg, &g);
        let text = plain(&rows).join("\n");
        assert!(
            text.contains("let x = 1;"),
            "the left text still draws: {text}"
        );
        assert!(
            text.contains("let x = 2;"),
            "the right text still draws: {text}"
        );
        // And a grid one row SHORT, which is the other shape of the same edge.
        let short = vec![Vec::new()];
        let rows = render_pair(&pair, &old, &new, &short, &short, &cfg, &g);
        let text = plain(&rows).join("\n");
        assert!(text.contains("let x = 1;"), "{text}");
        assert!(text.contains("let x = 2;"), "{text}");
    }

    #[test]
    fn a_change_reads_side_by_side_with_its_context() {
        let old = ["fn a() {", "    old();", "}"];
        let new = ["fn a() {", "    new();", "}"];
        let rows = render_split(&old, &new, &sc(120, Palette::None, 1, 1));
        let p = plain(&rows);
        // The hunk header precedes the only hunk, as it does in a real diff.
        assert_eq!(p.len(), 4, "{p:?}");
        assert!(p[0].starts_with("@@"), "the hunk is unheaded: {p:?}");
        assert!(p[1].contains("fn a() {") && p[1].contains("│"), "{p:?}");
        assert!(p[2].contains('-') && p[2].contains("old();"), "{p:?}");
        assert!(p[2].contains('+') && p[2].contains("new();"), "{p:?}");
        assert!(p[3].contains('}'), "{p:?}");
    }

    #[test]
    fn an_insertion_has_no_left_and_a_deletion_no_right() {
        let old = ["a", "b"];
        let new = ["a", "X", "b"];
        let rows = plain(&render_split(&old, &new, &sc(80, Palette::None, 1, 1)));
        let ins = rows
            .iter()
            .find(|r| r.contains('X'))
            .expect("the inserted line is shown");
        let (left, right) = ins.split_once('│').unwrap();
        assert!(right.contains('+'), "{ins:?}");
        assert!(
            left.trim().is_empty(),
            "an insertion's left panel is blank: {ins:?}"
        );

        let rows = plain(&render_split(&new, &old, &sc(80, Palette::None, 1, 1)));
        let del = rows
            .iter()
            .find(|r| r.contains('X'))
            .expect("the deleted line is shown");
        let (left, right) = del.split_once('│').unwrap();
        assert!(left.contains('-'), "{del:?}");
        assert!(
            right.trim().is_empty(),
            "a deletion's right panel is blank: {del:?}"
        );
    }

    #[test]
    fn a_created_file_is_all_right_panel() {
        let rows = plain(&render_split(
            &[],
            &["x", "y"],
            &sc(80, Palette::None, 1, 1),
        ));
        assert_eq!(rows.len(), 3, "header plus the two lines: {rows:?}");
        assert!(rows[0].starts_with("@@"), "{rows:?}");
        for r in rows.iter().skip(1) {
            let (left, right) = r.split_once('│').unwrap();
            assert!(left.trim().is_empty(), "{r:?}");
            assert!(right.contains('+'), "{r:?}");
        }
    }

    #[test]
    fn the_gutter_numbers_the_file_not_the_excerpt() {
        let old = ["keep"];
        let new = ["keep", "added"];
        let rows = plain(&render_split(&old, &new, &sc(80, Palette::None, 41, 41)));
        // The added line is file line 42, not excerpt line 2.
        assert!(
            rows.iter().any(|r| r.contains("42") && r.contains('+')),
            "{rows:?}"
        );
    }

    #[test]
    fn a_row_never_exceeds_the_full_width_and_wraps_instead() {
        let long = "x".repeat(200);
        let old = [long.as_str()];
        let new = [long.as_str(), "short"];
        for w in [60usize, 100, 160, 210] {
            let rows = render_split(&old, &new, &sc(w, Palette::None, 1, 1));
            assert!(!rows.is_empty());
            for r in &rows {
                assert!(width::width(r) <= w, "{w}: {} cols {r:?}", width::width(r));
            }
        }
    }

    /// The operator's trial: an added line is green to its **full width** and
    /// a removed one red — not just where its text reaches — and neither
    /// colour leaks past its own cell into the separator or the panel after
    /// it. The sign glyph still carries the distinction alone: under
    /// [`Palette::None`] not one escape byte is emitted.
    #[test]
    fn a_changed_row_is_tinted_to_its_full_width_and_hands_the_separator_a_clean_slate() {
        let old = ["fn a() {", "    old();", "}"];
        let new = ["fn a() {", "    new();", "}"];
        let rows = render_split(&old, &new, &sc(60, Palette::Colour, 1, 1));
        let joined = rows.join("\n");

        // The tint leads the cell — before the gutter, not after the text —
        // and the sign closes back **into** it, so a syntax span or the sign
        // itself cannot end the background mid-row.
        let added = rows
            .iter()
            .find(|r| {
                r.contains("+ new();")
                    || r.split_once(" │ ")
                        .map_or(false, |(_, r)| r.contains("new();"))
            })
            .expect("the added row is shown");
        let (_, right) = added.split_once(" │ ").expect("two panels");
        assert!(
            right.starts_with("\x1b[0m\x1b[48;5;22m"),
            "the added cell opens with the cube's dark green, background only, as soon as the separator closes: {added:?}"
        );
        assert!(
            right.contains("\x1b[32m+\x1b[0m\x1b[48;5;22m"),
            "the sign keeps its original green foreground and closes back into the tint: {added:?}"
        );
        assert!(
            right.contains("\x1b[32m+\x1b[0m\x1b[48;5;22m     new();"),
            "the text after the sign carries no foreground of its own — the original foregrounds: {added:?}"
        );
        assert!(
            right.contains("\x1b[32m2 \x1b[0m\x1b[48;5;22m"),
            "the line number takes the line's foreground on a changed row, as claude code's does: {added:?}"
        );
        assert!(
            joined.contains("\x1b[31m-\x1b[0m\x1b[48;5;52m"),
            "the removed row is red: {joined:?}"
        );
        // The cell ends with a reset — the padding inside the tint, then a
        // clean handoff — so the separator opens from a clean slate, not from
        // inside the tint.
        assert!(added.ends_with(crate::width::RESET), "{added:?}");
        assert!(
            added.contains("\x1b[0m\x1b[2m │ "),
            "the separator must open from a clean slate, not from inside the tint: {added:?}"
        );

        // And a context row is untouched: its base closes to a plain reset,
        // so its bytes are what they always were.
        let ctx = rows
            .iter()
            .find(|r| r.contains("fn a() {"))
            .expect("context row");
        assert!(
            !ctx.contains("\x1b[48;5;22m") && !ctx.contains("\x1b[48;5;52m"),
            "{ctx:?}"
        );

        // No palette, no bytes: the glyph alone still says which is which.
        for r in render_split(&old, &new, &sc(60, Palette::None, 1, 1)) {
            assert!(!r.contains('\x1b'), "{r:?}");
        }
    }

    #[test]
    fn a_wrapped_line_keeps_its_panel_and_blanks_the_other() {
        let long = "y".repeat(120);
        let old = ["a"];
        let new = [long.as_str()];
        let rows = plain(&render_split(&old, &new, &sc(60, Palette::None, 1, 1)));
        assert!(rows.len() >= 2, "{rows:?}");
        // Row 0 is the hunk header; the continuation row's right panel carries
        // code and its left is blank.
        let cont = &rows[2];
        let (left, right) = cont.split_once('│').unwrap();
        assert!(left.trim().is_empty(), "{cont:?}");
        assert!(right.contains('y'), "{cont:?}");
    }

    #[test]
    fn the_row_cap_stops_the_panels_and_says_what_it_dropped() {
        let old: Vec<String> = (0..20).map(|i| format!("old {i}")).collect();
        let new: Vec<String> = (0..20).map(|i| format!("new {i}")).collect();
        let old: Vec<&str> = old.iter().map(String::as_str).collect();
        let new: Vec<&str> = new.iter().map(String::as_str).collect();
        let cfg = Box::leak(Box::new(DiffConfig {
            width: 120,
            palette: Palette::None,
            context: 0,
            line_numbers: true,
            intra_line: false,
            max_rows: 6,
        }));
        let sc = SplitConfig {
            cfg,
            before_start: 1,
            after_start: 1,
            lang: None,
        };
        let rows = plain(&render_split(&old, &new, &sc));
        assert!(
            rows.iter().any(|r| r.contains("more diff lines not shown")),
            "{rows:?}"
        );
        assert!(rows.len() <= 8, "{rows:?}");
    }

    #[test]
    fn no_colour_means_no_escapes_and_the_signs_survive() {
        let old = ["a"];
        let new = ["b"];
        let rows = render_split(&old, &new, &sc(80, Palette::None, 1, 1));
        for r in &rows {
            assert!(!r.contains('\x1b'), "{r:?}");
        }
        assert!(
            rows.iter().any(|r| r.contains('-') && r.contains("a")),
            "{rows:?}"
        );
        assert!(
            rows.iter().any(|r| r.contains('+') && r.contains("b")),
            "{rows:?}"
        );
    }

    #[test]
    fn identical_sides_say_so() {
        let old = ["same"];
        let rows = render_split(&old, &old, &sc(80, Palette::None, 1, 1));
        assert_eq!(rows, vec!["no change"]);
    }

    #[test]
    fn rust_code_arrives_coloured_by_rano_and_unknown_extensions_do_not() {
        let old = ["fn a() {}"];
        let new = ["fn a() { let x = 1; }"];
        let cfg = Box::leak(Box::new(DiffConfig {
            width: 160,
            palette: Palette::Colour,
            context: 1,
            line_numbers: true,
            intra_line: false,
            max_rows: 60,
        }));
        let sc = SplitConfig {
            cfg,
            before_start: 1,
            after_start: 1,
            lang: lang_for("a.rs"),
        };
        assert_eq!(sc.lang, Some(rano::syntax::Lang::Rust));
        let rows = render_split(&old, &new, &sc);
        // `fn` is a keyword: rano names it, the table maps it to Role::Keyword,
        // and the palette paints magenta.
        assert!(
            rows.iter()
                .any(|r| r.contains("\x1b[35m") && r.contains("fn")),
            "{rows:?}"
        );

        let sc = SplitConfig {
            cfg,
            before_start: 1,
            after_start: 1,
            lang: lang_for("a.txt"),
        };
        assert_eq!(sc.lang, None);
        let rows = render_split(&old, &new, &sc);
        // The gutters are still faint, but nothing is syntax-coloured: no
        // keyword magenta anywhere.
        assert!(!rows.iter().any(|r| r.contains("\x1b[35m")), "{rows:?}");
    }

    /// **A language rano gains is letibot's without a second table.**
    ///
    /// `lang_for` delegates to rano's `detect` on purpose — the extension table
    /// lives where the grammars live, and a copy here would drift the way the
    /// head's own mode-name list drifted. Common Lisp is the first grammar
    /// added after this dependency existed, so this pins the delegation end to
    /// end: the extension is detected through the path dependency, and the
    /// grammar's captures reach the palette through the shared vocabulary
    /// (`defun` → keyword → magenta). When rano grows the next language, this
    /// is the test that says nothing here had to move.
    #[test]
    fn a_language_rano_gains_arrives_without_a_second_table() {
        assert_eq!(lang_for("patch.lisp"), Some(rano::syntax::Lang::CommonLisp));
        // The batch of 2026-09-17 took rano from seven grammars to twenty-seven.
        // A few of them, across the table's shapes — extension, multi-extension,
        // filename — assert the delegation stayed a delegation.
        assert_eq!(lang_for("app.ts"), Some(rano::syntax::Lang::TypeScript));
        assert_eq!(lang_for("README.md"), Some(rano::syntax::Lang::Markdown));
        assert_eq!(lang_for("style.css"), Some(rano::syntax::Lang::Css));
        assert_eq!(lang_for("init.lua"), Some(rano::syntax::Lang::Lua));
        assert_eq!(lang_for("Makefile"), Some(rano::syntax::Lang::Make));
        assert_eq!(lang_for("schema.sql"), Some(rano::syntax::Lang::Sql));
        assert_eq!(lang_for("deps.edn"), Some(rano::syntax::Lang::Clojure));
        let old = [";; greet"];
        let new = ["(defun greet (name) t)"];
        let cfg = Box::leak(Box::new(DiffConfig {
            width: 160,
            palette: Palette::Colour,
            context: 1,
            line_numbers: true,
            intra_line: false,
            max_rows: 60,
        }));
        let sc = SplitConfig {
            cfg,
            before_start: 1,
            after_start: 1,
            lang: lang_for("patch.lisp"),
        };
        let rows = render_split(&old, &new, &sc);
        assert!(
            rows.iter()
                .any(|r| r.contains("\x1b[35m") && r.contains("defun")),
            "{rows:?}"
        );
    }

    #[test]
    fn capture_names_fall_back_through_their_prefix() {
        assert_eq!(role_for_capture("comment"), Role::Comment);
        assert_eq!(role_for_capture("string"), Role::StringLit);
        assert_eq!(
            role_for_capture("type.builtin"),
            Role::TypeName,
            "dotted → prefix"
        );
        assert_eq!(role_for_capture("variable.builtin"), Role::Keyword);
        assert_eq!(role_for_capture("punctuation.bracket"), Role::Plain);
        assert_eq!(role_for_capture("variable"), Role::Plain);
    }

    /// **A multi-byte character shifts no class but its own.**
    ///
    /// rano's capture walk counted chars where tree-sitter answers bytes, so
    /// an em-dash on one line pushed every later line's classes two cells to
    /// the right and clamped its own line's captures short — the head drew
    /// `tail o[0mff`, a reset landing mid-word. Pinned from this side because
    /// the grid is what the palette paints: rano's own test pins the engine,
    /// this one pins the hand-off, and it fails the moment the path
    /// dependency regresses under us.
    #[test]
    fn an_em_dash_shifts_no_class_but_its_own() {
        let lines = ["let s = \"a—b\"; // dash", "let done = build(); // tail"];
        // Char column of `needle`'s first char — `find` answers bytes and the
        // grid is one cell per char.
        let col_of = |line: &str, needle: &str| {
            let b = line.find(needle).unwrap();
            line[..b].chars().count()
        };
        let grid = class_grid(&lines, Some(rano::syntax::Lang::Rust));

        // The dash's own line: the string capture keeps its closing quote.
        let l0 = lines[0];
        assert_eq!(grid[0][col_of(l0, "—")], Role::StringLit, "{:?}", grid[0]);
        assert_eq!(
            grid[0][col_of(l0, "b\"") + 1],
            Role::StringLit,
            "the closing quote is still inside the capture: {:?}",
            grid[0]
        );
        assert_eq!(grid[0][col_of(l0, "// dash")], Role::Comment);

        // The line after it: not shifted two cells to the right.
        let l1 = lines[1];
        assert_eq!(grid[1][col_of(l1, "let")], Role::Keyword, "{:?}", grid[1]);
        assert_eq!(grid[1][col_of(l1, "build")], Role::FuncName);
        assert_eq!(grid[1][col_of(l1, "// tail")], Role::Comment);
    }

    #[test]
    fn tabs_expand_to_the_same_stops_the_unified_renderer_uses() {
        let old = ["\tfn a() {}"];
        let new = ["\tfn b() {}"];
        let rows = plain(&render_split(&old, &new, &sc(120, Palette::None, 1, 1)));
        // A tab is four columns on both panels, so the code starts at the
        // same offset in each half and the pair aligns. (`SEP` is " │ ", so
        // the right half carries one leading space that is not the panel's.)
        // The finder names the code, not the sign: the hunk header carries a
        // `-` of its own.
        let changed = rows
            .iter()
            .find(|r| r.contains('-') && r.contains("fn a"))
            .unwrap();
        let (left, right) = changed.split_once('│').unwrap();
        assert_eq!(
            left.find("fn").unwrap(),
            right.trim_start().find("fn").unwrap(),
            "{changed:?}"
        );

        // **A RUN of tabs, which is the case this test did not have and the one that
        // separates the two implementations.** MEASURED 2026-10-02, comparing notes with
        // leticl: the expansion here computed each stop from the character's index in the
        // SOURCE, so the second of two tabs was padded by three where a terminal pads by
        // four — seven columns instead of eight. With ONE tab the two agree, so this test
        // was green throughout, which is why the defect was silent rather than obvious.
        //
        // Asserted against the unified renderer's own function rather than against a
        // literal: "the same stops" is this test's whole subject, and a literal here
        // would let the two drift again while both matched the test.
        let lined = ["\t\treturn x", "\t\treturn y"];
        let rows = plain(&render_split(
            &lined[..1],
            &lined[1..],
            &sc(120, Palette::None, 1, 1),
        ));
        // The whole row, not one half of it: both panels are on it and the pair that
        // changed is on the left. `want` keeps its leading spaces — they are the thing
        // under test.
        let want = crate::diff::expand_tabs("\t\treturn x", crate::diff::TAB_STOP);
        let code = rows
            .iter()
            .find(|r| r.contains("return x"))
            .expect("the deleted line is on screen");
        assert!(
            code.contains(&want),
            "two tabs must be eight columns, the same stop the unified renderer uses: \
             {code:?} does not contain {want:?}"
        );
        // **Anchored at the sign, because a bare `contains` cannot tell seven spaces
        // from eight here.** The gutter's own trailing space sits directly before the
        // indent, so `code.contains("        return x")` is satisfied by a SEVEN-column
        // indent with the gutter's space in front of it. Measured: that assertion passed
        // against the very arithmetic it is written to catch, which is worse than not
        // having it — a test that cannot fail on the bug it names reads as coverage.
        let body = code
            .split_once("- ")
            .expect("the row carries a deletion sign")
            .1;
        assert!(
            body.starts_with("        return x"),
            "eight columns of indent, not seven: {body:?}"
        );
    }
}
