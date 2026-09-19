//! Blocks to terminal lines, with a bounded body and a cache over the frozen
//! prefix.
//!
//! §13.3's wire rule and its lexer rule buy nothing if the *renderer* rebuilds
//! every line from every block on every delta — that is pi's mistake moved one
//! layer down. So:
//!
//! - The frozen prefix of a document renders **once per width**. [`BlockCache`]
//!   keeps the lines and extends them as blocks freeze; a resize is the only thing
//!   that invalidates it.
//! - Only the live tail is re-rendered per frame, and the tail is bounded by the
//!   lexer's frozen frontier.
//!
//! # Bounded body
//!
//! > There is a matching known-good shape from that same work: render the head of
//! > a long block as a title and the tail as a bounded body, with the buffer size
//! > configurable rather than hardcoded.
//!
//! [`Budget`] is that size, and it is a value on [`RenderConfig`], not a constant
//! in the middle of a function. A long reasoning block shows its first line as a
//! title, a count of what was elided, and its last N lines — which is what a reader
//! of a streaming model actually wants, because the interesting end is the end.

use letibot_ui::highlight::StreamingCode;
use letibot_ui::style::{Painter, Palette, Role};

use crate::markdown::{Align, Block, IncrementalMarkdown, InlineStyle, Run};

/// Columns, wrapping and truncation come from `letibot-ui`.
///
/// These were three functions here, and all three counted a `char` as one column.
/// The comment on the old `visible_width` called that *"the accepted cost of not
/// vendoring a width table"*, and the cost is not accepted anywhere a person can
/// see it: a status line one column too long is wrapped by the terminal, which
/// puts a row on the screen the head did not count, which scrolls the frame it
/// just painted. [`letibot_ui::width`] measures grapheme clusters and knows the
/// wide ranges, never splits a cluster or an escape, breaks between wide clusters
/// so CJK wraps at all, and carries SGR state across a break — which matters here
/// because [`crate::term::paint`] emits `\x1b[K` per row and erase-to-end-of-line
/// uses the *current* attributes.
pub use letibot_ui::width::{truncate as trim_to, width as visible_width, wrap};

/// ANSI, kept as constants rather than a dependency.
pub mod sgr {
    pub const RESET: &str = "\x1b[0m";
    pub const BOLD: &str = "\x1b[1m";
    pub const DIM: &str = "\x1b[2m";
    pub const ITALIC: &str = "\x1b[3m";
    /// Bold and italic in one sequence. `1;3` rather than two escapes because a
    /// second `SGR` for the same attribute pair is two transitions where one will do,
    /// and a wrapped `***word***` pays that on every line.
    pub const BOLD_ITALIC: &str = "\x1b[1;3m";
    pub const CYAN: &str = "\x1b[36m";
    pub const GREEN: &str = "\x1b[32m";
    pub const YELLOW: &str = "\x1b[33m";
    pub const RED: &str = "\x1b[31m";
    pub const MAGENTA: &str = "\x1b[35m";
    pub const GREY: &str = "\x1b[90m";
    /// Inverse video. Used for the highlighted row of a choice, where the point is
    /// "this is the one Enter takes" rather than a category — a second hue would read
    /// as a second kind of thing.
    pub const REVERSE: &str = "\x1b[7m";
}

/// What this module draws its own frames with: a code fence, a horizontal rule,
/// an elision marker.
///
/// [`sgr::DIM`], not [`sgr::GREY`]. 90 is the theme's *bright black*, and
/// `letibot_ui::style` measured it landing within a hair of the background on
/// several light themes — which is why every role in that table is an attribute
/// or a named slot and none of them is 90. The attribute de-emphasises whatever
/// foreground the reader already chose, which is the thing a frame wants.
const FRAME: &str = sgr::DIM;

/// How many lines a single block may occupy before it is summarised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// Lines kept for an ordinary block. `usize::MAX` disables the bound.
    pub body_lines: usize,
    /// Lines kept for a reasoning block, which is usually the longest thing on the
    /// screen and the least interesting in the middle.
    pub reasoning_lines: usize,
}

impl Default for Budget {
    fn default() -> Self {
        Budget {
            body_lines: 40,
            reasoning_lines: 8,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderConfig {
    pub width: usize,
    pub color: bool,
    pub budget: Budget,
    /// The role of the block this render is happening *inside*, if any.
    ///
    /// `None` is the top level, where a span closes with a plain reset and the
    /// output is byte-for-byte what it always was. `Some(Role::Reasoning)` is the
    /// model's working-out, and it is the whole reason this field exists: a
    /// heading or an inline code span inside a themed block used to close to the
    /// *terminal default*, so the reasoning "tries to be grey, then goes green and
    /// becomes white for several rows and then grey again" — the operator's words,
    /// from looking at the screen.
    ///
    /// A reset is not a restore. Carried here rather than passed alongside because
    /// every span this module paints has to close the same way, and a parameter
    /// that is threaded by hand is a parameter one call site will be missing.
    pub base: Option<Role>,
}

impl Default for RenderConfig {
    fn default() -> Self {
        RenderConfig {
            width: 100,
            color: true,
            budget: Budget::default(),
            base: None,
        }
    }
}

impl RenderConfig {
    /// The `letibot-ui` palette this config implies.
    ///
    /// One place, because `color: false` is not a monochrome theme — it is the
    /// `--replay`, pipe-to-a-file and CI case, and it has to produce
    /// byte-identical output on every machine.
    pub fn palette(&self) -> Palette {
        if self.color {
            Palette::Colour
        } else {
            Palette::None
        }
    }

    /// The palette bound to [`RenderConfig::base`]. Everything painted by this
    /// module goes through it, which is what makes the restore structural rather
    /// than a fix applied to whichever call site somebody noticed.
    pub fn painter(&self) -> Painter {
        match self.base {
            Some(b) => Painter::inside(self.palette(), b),
            None => Painter::new(self.palette()),
        }
    }

    /// The same config, rendering inside a block styled as `base`.
    pub fn inside(&self, base: Role) -> RenderConfig {
        RenderConfig {
            base: Some(base),
            ..self.clone()
        }
    }

    fn c(&self, code: &str, s: &str) -> String {
        if self.color {
            format!("{code}{s}{}", self.painter().close())
        } else {
            s.to_string()
        }
    }
}

/// A code block's streaming highlighter, and how much of its source it has seen.
///
/// The pair is the whole of §2.5's "do not reintroduce a full re-parse per delta".
/// `pushed` is a byte offset into `lines.join("\n")`, which grows only at the end
/// while a fence is open, so the delta handed to the lexer each frame is the new
/// bytes and nothing else.
#[derive(Debug)]
struct CodePaint {
    sc: StreamingCode,
    pushed: usize,
}

impl CodePaint {
    fn new(lang: &str, painter: Painter) -> CodePaint {
        CodePaint {
            sc: StreamingCode::inside(lang, painter),
            pushed: 0,
        }
    }

    /// Hand over whatever is new. A complete line is highlighted exactly once,
    /// ever; only the incomplete tail is repainted, and that is bounded by the
    /// line, not by the block.
    fn feed(&mut self, lines: &[String], closed: bool) {
        let mut src = lines.join("\n");
        if closed {
            src.push('\n');
        }
        if src.len() > self.pushed {
            // `src` only ever grows at its end while the fence is open, so this
            // is always a character boundary.
            self.sc.push(&src[self.pushed..]);
            self.pushed = src.len();
        }
    }
}

/// Render one block to lines, unbounded.
///
/// One-shot: a fresh highlighter per call, which is right for a block that is
/// rendered once (a transcript row, a frozen prefix) and wrong for one that is
/// rendered every frame. [`BlockCache`] is the second case and keeps the
/// highlighter between frames; the output of the two paths is identical, because
/// it is the same lexer fed the same bytes in the same order.
pub fn render_block(b: &Block, cfg: &RenderConfig) -> Vec<String> {
    let mut paint = match b {
        Block::Code { lang, .. } => Some(CodePaint::new(lang, cfg.painter())),
        _ => None,
    };
    render_block_with(b, cfg, paint.as_mut())
}

fn render_block_with(b: &Block, cfg: &RenderConfig, code: Option<&mut CodePaint>) -> Vec<String> {
    let w = cfg.width.max(20);
    match b {
        // Coloured by level, with the hashes kept and de-emphasised.
        //
        // Both surveyed heads drop the hashes and colour the text; that is right
        // while there is colour and wrong without it, because the level is then
        // unrecoverable — and `color: false` here is `--replay`, a pipe and CI,
        // not a theme. So the hashes stay, faint, and carry the level for the
        // monochrome reader; the colour carries it for everyone else.
        Block::Heading { level, runs } => {
            let p = cfg.painter();
            let role = match level {
                1 => Role::Heading,
                2 => Role::Subheading,
                _ => Role::Strong,
            };
            let hashes = "#".repeat(*level as usize);
            vec![trim_to(
                &format!(
                    "{} {}",
                    p.paint(Role::Faint, &hashes),
                    p.paint(role, &paint_runs(runs, p))
                ),
                w,
            )]
        }
        Block::Paragraph { lines } => {
            let joined = joined_runs(lines);
            wrap(&paint_runs(&joined, cfg.painter()), w)
        }
        Block::Code {
            lang,
            lines,
            closed,
        } => {
            let mut owned;
            let paint = match code {
                Some(p) => p,
                None => {
                    owned = CodePaint::new(lang, cfg.painter());
                    &mut owned
                }
            };
            paint.feed(lines, *closed);
            let painted = paint.sc.lines();
            let mut out = Vec::with_capacity(painted.len() + 2);
            // The fence's own info string when the highlighter did not recognise
            // it: naming a language we are not colouring is honest, and inventing
            // one we are is not.
            let head = match (paint.sc.language(), lang.is_empty()) {
                (Some(name), _) => format!("┌─ {name}"),
                (None, false) => format!("┌─ {lang}"),
                (None, true) => "┌─ code".to_string(),
            };
            out.push(cfg.c(FRAME, &head));
            for l in painted {
                out.push(format!("{}{l}", cfg.c(FRAME, "│ ")));
            }
            out.push(cfg.c(
                FRAME,
                if *closed {
                    "└─"
                } else {
                    "└─ (still writing…)"
                },
            ));
            out
        }
        Block::List {
            ordered,
            start,
            items,
        } => {
            let p = cfg.painter();
            let mut out = Vec::new();
            for (i, it) in items.iter().enumerate() {
                // `·` rather than `•`, from grok-build: a bullet the same weight as
                // the prose competes with it down a long list, and what the marker
                // has to do is mark the indent, not be seen.
                // An ordered list's number is content — it is what the prose
                // refers back to — so it is not de-emphasised. A bullet is pure
                // structure and is.
                let (marker, marker_role) = if *ordered {
                    // `start + i`, not `i + 1`. A loose list — one whose items are
                    // separated by blank lines, which is what a model writes as
                    // soon as an item runs past a sentence — arrives as one block
                    // per item, and numbering from the index inside the block made
                    // every item of a six-point answer read `1.`.
                    (format!("{}. ", start + i), Role::Plain)
                } else {
                    ("· ".to_string(), Role::Faint)
                };
                // Columns, not bytes. `"· "` is two columns and three bytes, and
                // indenting a wrapped bullet by its byte length put every
                // continuation line a column too far right.
                let pad = visible_width(&marker);
                let body = wrap(&paint_runs(it, p), w.saturating_sub(pad));
                for (j, line) in body.into_iter().enumerate() {
                    if j == 0 {
                        out.push(format!("{}{line}", p.paint(marker_role, &marker)));
                    } else {
                        out.push(format!("{:width$}{line}", "", width = pad));
                    }
                }
            }
            out
        }
        Block::Quote { lines } => {
            let joined = joined_runs(lines);
            wrap(&paint_runs(&joined, cfg.painter()), w.saturating_sub(2))
                .into_iter()
                .map(|l| cfg.c(sgr::DIM, &format!("│ {l}")))
                .collect()
        }
        Block::Table { head, align, rows } => table_lines(head, align, rows, cfg, w),
        Block::Rule => vec![cfg.c(FRAME, &"─".repeat(w.min(60)))],
    }
}

/// A pipe table, at the width the terminal actually has.
///
/// The operator, 2026-09-17: *"table rendering is broken"*. It was not rendered
/// at all — a table lexed as a paragraph, joined with spaces and wrapped as
/// prose. What a table owes its reader is the column, so:
///
/// - **Columns are as wide as their content wants, until they do not fit.** Then
///   the wide ones give way first (water-filling): a table of three short columns
///   and one long one shrinks the long one and leaves the others alone, rather
///   than taking an equal slice off each and truncating the short ones to nothing.
/// - **A cell too narrow wraps, it does not get cut.** A row is as tall as its
///   tallest cell. Truncation would lose bytes the model wrote and a table is
///   most often where the numbers are.
/// - **No outer box.** The frame is one faint rule under the header and a faint
///   `│` between columns — the same weight as the quote rail and the code fence,
///   so a table sits in a turn rather than shouting from it.
fn table_lines(
    head: &[Vec<Run>],
    align: &[Align],
    rows: &[Vec<Vec<Run>>],
    cfg: &RenderConfig,
    w: usize,
) -> Vec<String> {
    let p = cfg.painter();
    // The column count is the header's; a row with more cells than the header has
    // is showing something the header does not name, so the table widens to it
    // rather than dropping it.
    let cols = head.len().max(rows.iter().map(Vec::len).max().unwrap_or(0)).max(1);
    fn cell<'a>(r: &'a [Vec<Run>], i: usize) -> &'a [Run] {
        r.get(i).map(Vec::as_slice).unwrap_or(&[])
    }

    // Painted once: the paint is what gets measured, wrapped and padded, so a
    // `**bold**` cell does not measure its escape bytes as columns.
    let head_p: Vec<String> = (0..cols)
        .map(|i| p.paint(Role::Strong, &paint_runs(cell(head, i), p)))
        .collect();
    let rows_p: Vec<Vec<String>> = rows
        .iter()
        .map(|r| (0..cols).map(|i| paint_runs(cell(r, i), p)).collect())
        .collect();

    let natural: Vec<usize> = (0..cols)
        .map(|i| {
            std::iter::once(visible_width(&head_p[i]))
                .chain(rows_p.iter().map(|r| visible_width(&r[i])))
                .max()
                .unwrap_or(0)
                .max(1)
        })
        .collect();

    // Three columns per gap: `" │ "`.
    let gaps = 3 * cols.saturating_sub(1);
    let available = w.saturating_sub(gaps).max(cols);
    let widths = fit_columns(&natural, available);

    let sep = p.paint(Role::Faint, " │ ");
    let mut out = Vec::new();
    let push_row = |cells: &[String], out: &mut Vec<String>| {
        // Wrap every cell to its column, then emit one screen line per wrapped
        // line, padding the cells that ran out.
        let wrapped: Vec<Vec<String>> = cells
            .iter()
            .zip(&widths)
            .map(|(c, wd)| {
                let v = wrap(c, *wd);
                if v.is_empty() { vec![String::new()] } else { v }
            })
            .collect();
        let height = wrapped.iter().map(Vec::len).max().unwrap_or(1);
        for line in 0..height {
            let mut row = String::new();
            for i in 0..cols {
                if i > 0 {
                    row.push_str(&sep);
                }
                let text = wrapped[i].get(line).cloned().unwrap_or_default();
                row.push_str(&pad(&text, widths[i], align.get(i).copied().unwrap_or(Align::Left)));
            }
            // The last column's padding is trailing whitespace on the screen and
            // in a copy-paste; the columns are already established by the ones
            // before it.
            out.push(row.trim_end().to_string());
        }
    };

    push_row(&head_p, &mut out);
    let rule: String = widths
        .iter()
        .map(|wd| "─".repeat(*wd))
        .collect::<Vec<_>>()
        .join("─┼─");
    out.push(p.paint(Role::Faint, &rule));
    for r in &rows_p {
        push_row(r, &mut out);
    }
    out
}

/// Give each column its natural width if they all fit; otherwise let the wide
/// ones give way first.
///
/// Water-filling: every column narrower than an equal share keeps what it wants,
/// and the slack they leave is shared again among the rest. An equal cut instead
/// would take the same columns off a two-character `n` column as off a sixty-
/// character `status` one, and the short columns are the ones that cannot spare it.
fn fit_columns(natural: &[usize], available: usize) -> Vec<usize> {
    let n = natural.len();
    if n == 0 {
        return Vec::new();
    }
    if natural.iter().sum::<usize>() <= available {
        return natural.to_vec();
    }
    let mut widths = vec![0usize; n];
    let mut settled = vec![false; n];
    loop {
        let taken: usize = widths.iter().zip(&settled).filter(|(_, s)| **s).map(|(w, _)| *w).sum();
        let free = settled.iter().filter(|s| !**s).count();
        if free == 0 {
            break;
        }
        let share = available.saturating_sub(taken) / free;
        let mut moved = false;
        for i in 0..n {
            if !settled[i] && natural[i] <= share {
                widths[i] = natural[i];
                settled[i] = true;
                moved = true;
            }
        }
        if !moved {
            // Everything left wants more than its share. A floor of four columns:
            // narrower than that and a wrapped word is one letter per line, which
            // is not a table any more.
            for i in 0..n {
                if !settled[i] {
                    widths[i] = share.max(4);
                }
            }
            break;
        }
    }
    widths
}

fn pad(s: &str, width: usize, align: Align) -> String {
    let have = visible_width(s);
    let slack = width.saturating_sub(have);
    match align {
        Align::Left => format!("{s}{:slack$}", ""),
        Align::Right => format!("{:slack$}{s}", ""),
        Align::Center => {
            let left = slack / 2;
            let right = slack - left;
            format!("{:left$}{s}{:right$}", "", "")
        }
    }
}

/// Render a block, summarising it if it exceeds `limit` lines.
///
/// The shape is title, elision count, tail. Never a silent truncation: the count
/// is the disclosure, the same rule the log applies to `dropped`.
pub fn render_bounded(b: &Block, cfg: &RenderConfig, limit: usize) -> Vec<String> {
    let mut paint = match b {
        Block::Code { lang, .. } => Some(CodePaint::new(lang, cfg.painter())),
        _ => None,
    };
    render_bounded_with(b, cfg, limit, paint.as_mut())
}

fn render_bounded_with(
    b: &Block,
    cfg: &RenderConfig,
    limit: usize,
    code: Option<&mut CodePaint>,
) -> Vec<String> {
    let full = render_block_with(b, cfg, code);
    if full.len() <= limit || limit < 3 {
        return full;
    }
    let keep = limit - 2;
    let elided = full.len() - keep;
    let mut out = Vec::with_capacity(limit);
    out.push(cfg.c(sgr::DIM, &format!("▸ {}", trim_to(&b.title(), cfg.width))));
    out.push(cfg.c(FRAME, &format!("  … {elided} lines elided …")));
    out.extend(full[full.len() - keep..].iter().cloned());
    out
}

/// A per-line decoration applied to everything a [`BlockCache`] produces.
///
/// It exists so the reasoning pane can carry `card::REASONING_RAIL_WIDTH`'s `┃`
/// rail **without** copying its frozen prefix into every frame. The rail has to
/// be on every line — that is the point of it, it is the signal that survives a
/// copy-paste when the colour does not — and the only place it can be applied
/// once per line rather than once per line per frame is at the moment the line
/// enters the cache.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Decor {
    /// Prepended to every line, already painted. Its *visible* width must be
    /// subtracted from the width the caller wraps at, or the block renders one
    /// row taller than the space reserved for it.
    pub prefix: String,
    /// Opened before the line body and reset after it.
    pub open: String,
}

impl Decor {
    /// Decorate one line. Public because a head that draws a single line beside a
    /// cached block — the live tail of a folded reasoning pane — has to decorate
    /// it the same way, and the only way to guarantee that is for there to be one
    /// function.
    pub fn apply(&self, l: &str) -> String {
        match (self.prefix.is_empty(), self.open.is_empty()) {
            (true, true) => l.to_string(),
            (false, true) => format!("{}{l}", self.prefix),
            (true, false) => format!("{}{l}{}", self.open, sgr::RESET),
            (false, false) => format!("{}{}{l}{}", self.prefix, self.open, sgr::RESET),
        }
    }

    fn is_none(&self) -> bool {
        self.prefix.is_empty() && self.open.is_empty()
    }
}

/// Rendered lines for a growing document, with the frozen prefix cached.
#[derive(Debug, Default)]
pub struct BlockCache {
    width: usize,
    color: bool,
    /// The block style the cached prefix was painted inside. Part of the
    /// invalidation key for the same reason `color` is: a prefix painted at a
    /// different base closes its spans to a different sequence.
    base: Option<Role>,
    decor: Decor,
    /// One streaming highlighter per open code block, keyed by the block's
    /// **absolute** index in the document.
    ///
    /// Absolute rather than tail-relative because the tail is re-lexed from the
    /// frozen frontier on every push, so a tail block's position moves as blocks
    /// freeze while `stable_count() + tail_index` does not. An entry is dropped
    /// the moment its block freezes: the block is then in `stable_lines` and will
    /// never be rendered again.
    codes: std::collections::HashMap<usize, CodePaint>,
    /// The per-block line bound the prefix was rendered under. Part of the cache
    /// key: collapsing reasoning changes it, and a prefix rendered under the old
    /// bound is as stale as one rendered at the old width.
    limit: usize,
    /// Lines for the blocks that were frozen when they were rendered.
    stable_lines: Vec<String>,
    /// How many of the document's stable blocks are already in `stable_lines`.
    rendered_blocks: usize,
    /// Instrumentation: blocks handed to the renderer over this cache's life.
    blocks_rendered: u64,
}

impl BlockCache {
    pub fn new() -> Self {
        BlockCache::default()
    }

    /// A cache whose every line carries `decor`. See [`Decor`].
    pub fn decorated(decor: Decor) -> Self {
        BlockCache {
            decor,
            ..BlockCache::default()
        }
    }

    /// Set the decoration, throwing the cache away if it changed.
    ///
    /// It changes when `color` does, and a prefix rendered under the old palette
    /// is stale in exactly the way a prefix rendered at the old width is.
    pub fn set_decor(&mut self, decor: Decor) {
        if self.decor != decor {
            self.decor = decor;
            self.stable_lines.clear();
            self.rendered_blocks = 0;
            self.codes.clear();
        }
    }

    /// Total lines for the document: the cached prefix plus a freshly rendered
    /// tail.
    ///
    /// Convenience over [`BlockCache::split`], and it **copies the prefix**. Fine
    /// for a one-shot render of a finished document; not for the per-frame path,
    /// where the copy is O(accumulated output) and §13.3 says a frame must not be.
    pub fn lines(
        &mut self,
        md: &IncrementalMarkdown,
        cfg: &RenderConfig,
        limit: usize,
    ) -> Vec<String> {
        let (stable, tail) = self.split(md, cfg, limit);
        let mut out = stable.to_vec();
        out.extend(tail);
        while out.last().is_some_and(|l| l.is_empty()) {
            out.pop();
        }
        out
    }

    /// The frozen prefix **by reference**, and the live tail freshly rendered.
    ///
    /// This is the per-frame form. The prefix is the part that grows without bound
    /// over a long answer, and handing it back borrowed is what keeps the cost of
    /// drawing a frame proportional to the tail and the window rather than to
    /// everything said so far.
    pub fn split(
        &mut self,
        md: &IncrementalMarkdown,
        cfg: &RenderConfig,
        limit: usize,
    ) -> (&[String], Vec<String>) {
        if self.width != cfg.width
            || self.color != cfg.color
            || self.base != cfg.base
            || self.limit != limit
        {
            // A resize is the only thing that invalidates the prefix — and a change
            // of budget, which is a resize of a different axis: the same block
            // renders to a different number of lines when the bound moves, so a
            // prefix rendered under the old one is stale in exactly the same way.
            self.width = cfg.width;
            self.color = cfg.color;
            self.base = cfg.base;
            self.limit = limit;
            self.stable_lines.clear();
            self.rendered_blocks = 0;
            // The highlighters go too: they hold painted lines at the old
            // palette, and a `color` flip is exactly the case that must not
            // leave half a code block coloured.
            self.codes.clear();
        }
        let stable = md.stable();
        for (i, b) in stable.iter().enumerate().skip(self.rendered_blocks) {
            let mut paint = self.codes.remove(&i);
            let lines = render_bounded_with(b, cfg, limit, paint.as_mut());
            self.stable_lines
                .extend(lines.into_iter().map(|l| self.decor.apply(&l)));
            self.stable_lines.push(String::new());
            self.blocks_rendered += 1;
        }
        self.rendered_blocks = stable.len();

        let mut tail = Vec::new();
        for (j, b) in md.tail().iter().enumerate() {
            let abs = stable.len() + j;
            let mut paint = self.codes.remove(&abs);
            if paint.is_none()
                && let Block::Code { lang, .. } = b
            {
                paint = Some(CodePaint::new(lang, cfg.painter()));
            }
            let lines = render_bounded_with(b, cfg, limit, paint.as_mut());
            if let Some(p) = paint {
                self.codes.insert(abs, p);
            }
            if self.decor.is_none() {
                tail.extend(lines);
            } else {
                tail.extend(lines.iter().map(|l| self.decor.apply(l)));
            }
            tail.push(String::new());
            self.blocks_rendered += 1;
        }
        while tail.last().is_some_and(|l| l.is_empty()) {
            tail.pop();
        }
        (&self.stable_lines, tail)
    }

    pub fn blocks_rendered(&self) -> u64 {
        self.blocks_rendered
    }

    /// Bytes handed to the syntax lexer over this cache's life, summed over every
    /// live code block. The §13.3 instrument one layer down from
    /// `IncrementalMarkdown::bytes_lexed`: a full re-highlight per delta makes it
    /// quadratic, and nothing else about the screen would look different.
    pub fn bytes_highlighted(&self) -> u64 {
        self.codes.values().map(|c| c.sc.bytes_highlighted()).sum()
    }
}

/// Paint inline runs, one SGR transition per run.
///
/// There is nothing to scan for here any more. The markdown projection parsed the
/// inline grammar, so `**bold**` arrived as a [`Run`] whose text is `bold` and whose
/// style is [`InlineStyle::Bold`] — the asterisks are already gone, and a `*` that is
/// not emphasis stays literal because the grammar said so. What is left is the
/// mapping from style to escape, which belongs to the renderer because the palette
/// does.
///
/// A palette with no colour yields the plain text: `--replay`, a pipe and CI all read
/// that, and an escape nobody renders is noise in a log.
pub fn paint_runs(runs: &[Run], p: Painter) -> String {
    if !p.palette().is_colour() {
        return crate::markdown::runs_text(runs);
    }
    // Not `sgr::RESET`. Every run closes back to whatever block it is inside — see
    // `RenderConfig::base`.
    let close = p.close();
    let mut out = String::new();
    for r in runs {
        let code = match r.style {
            InlineStyle::Plain => "",
            InlineStyle::Bold => sgr::BOLD,
            InlineStyle::Italic => sgr::ITALIC,
            InlineStyle::BoldItalic => sgr::BOLD_ITALIC,
            InlineStyle::Code => sgr::CYAN,
            // No `9m`: an attribute half the terminals in use do not carry, and one
            // that a reader who has turned colour off would not see at all. Dim reads
            // as "this was struck" next to the same sentence undimmed, and it is the
            // same weight the frame uses.
            InlineStyle::Strikethrough => sgr::DIM,
        };
        out.push_str(code);
        out.push_str(&r.text);
        if !code.is_empty() {
            out.push_str(&close);
        }
    }
    out
}

/// One block's lines as one run list, joined with a plain space.
///
/// A paragraph's source lines are one paragraph; the renderer wraps the whole thing
/// to the display width, so the source's newlines are a wrap the model did not mean.
pub fn joined_runs(lines: &[Vec<Run>]) -> Vec<Run> {
    let mut out: Vec<Run> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            out.push(Run {
                text: " ".to_string(),
                style: InlineStyle::Plain,
            });
        }
        out.extend(line.iter().cloned());
    }
    out
}

/// A byte count a person can read. `8192` is a number to decode; `8.0 KB` is not.
pub fn bytes_human(n: u64) -> String {
    const K: u64 = 1024;
    match n {
        0..=1_023 => format!("{n} B"),
        _ if n < K * K => format!("{:.1} KB", n as f64 / K as f64),
        _ if n < K * K * K => format!("{:.1} MB", n as f64 / (K * K) as f64),
        _ => format!("{:.1} GB", n as f64 / (K * K * K) as f64),
    }
}

/// A duration a person can read.
pub fn dur_human(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms} ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format!("{}m{:02}s", ms / 60_000, (ms % 60_000) / 1000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::lex;
    use letibot_sessionlog::testing::MARKDOWN;

    fn cfg() -> RenderConfig {
        RenderConfig {
            width: 72,
            color: false,
            budget: Budget::default(),
            base: None,
        }
    }

    #[test]
    fn a_long_block_becomes_a_title_a_count_and_a_tail() {
        let block = Block::Code {
            lang: "rust".into(),
            lines: (0..100).map(|i| format!("line {i}")).collect(),
            closed: true,
        };
        let lines = render_bounded(&block, &cfg(), 10);
        assert_eq!(lines.len(), 10);
        assert!(lines[0].contains("rust · 100 lines"), "{:?}", lines[0]);
        assert!(lines[1].contains("lines elided"), "{:?}", lines[1]);
        // The tail is the end, because the end is the interesting part of a
        // streaming block.
        assert!(lines.last().unwrap().contains("└─"));
        assert!(lines[lines.len() - 2].contains("line 99"));
    }

    #[test]
    fn the_bound_is_configurable_not_hardcoded() {
        let block = Block::Code {
            lang: String::new(),
            lines: (0..100).map(|i| format!("line {i}")).collect(),
            closed: true,
        };
        assert_eq!(render_bounded(&block, &cfg(), 5).len(), 5);
        assert_eq!(render_bounded(&block, &cfg(), 30).len(), 30);
        assert_eq!(render_bounded(&block, &cfg(), usize::MAX).len(), 102);
    }

    #[test]
    fn the_frozen_prefix_is_rendered_once() {
        let mut md = IncrementalMarkdown::new();
        let mut cache = BlockCache::new();
        let cfg = cfg();
        let doc = MARKDOWN.repeat(4);
        let mut frames = 0;
        let mut buf = String::new();
        for ch in doc.chars() {
            buf.push(ch);
            if buf.chars().count() >= 8 {
                md.push(&buf);
                buf.clear();
                cache.lines(&md, &cfg, 40);
                frames += 1;
            }
        }
        let blocks = md.stable_count() + md.tail().len();
        assert!(
            cache.blocks_rendered() < (blocks + frames * 3) as u64,
            "rendered {} block-renders over {frames} frames for {blocks} blocks; \
             the stable prefix is being rebuilt",
            cache.blocks_rendered()
        );
    }

    #[test]
    fn a_resize_is_the_only_thing_that_invalidates_the_cache() {
        let mut md = IncrementalMarkdown::new();
        md.push(MARKDOWN);
        let mut cache = BlockCache::new();
        let mut cfg = cfg();
        let a = cache.lines(&md, &cfg, 40);
        let after_first = cache.blocks_rendered();
        let b = cache.lines(&md, &cfg, 40);
        assert_eq!(a, b);
        assert!(
            cache.blocks_rendered() - after_first <= md.tail().len() as u64,
            "an idle frame re-rendered the prefix"
        );
        cfg.width = 40;
        let c = cache.lines(&md, &cfg, 40);
        assert_ne!(a, c, "a resize must re-wrap");
    }

    #[test]
    fn wrapping_counts_visible_columns_not_escape_bytes() {
        let runs = vec![
            Run { text: "a ".into(), style: InlineStyle::Plain },
            Run { text: "code".into(), style: InlineStyle::Code },
            Run { text: " b".into(), style: InlineStyle::Plain },
        ];
        let coloured = paint_runs(&runs, Painter::new(Palette::Colour));
        assert!(coloured.len() > 10);
        assert_eq!(visible_width(&coloured), "a code b".len());
        let lines = wrap(&coloured, 20);
        assert_eq!(lines.len(), 1, "escapes must not consume width: {lines:?}");
    }

    /// A marker in the *hand-written* inline syntax is literal text now.
    ///
    /// The point of the tree-sitter projection is that `**bold**` is `Run { text:
    /// "bold", style: Bold }` and the asterisks are gone before the renderer sees
    /// them. A `**` that is not emphasis is therefore also literal, and this pins the
    /// two halves of that: the styles map to escapes, and nothing is scanned for.
    #[test]
    fn the_renderer_sees_styles_not_markers() {
        let runs = vec![
            Run { text: "plain ".into(), style: InlineStyle::Plain },
            Run { text: "bold".into(), style: InlineStyle::Bold },
            Run { text: " and ".into(), style: InlineStyle::Plain },
            Run { text: "code".into(), style: InlineStyle::Code },
        ];
        let painted = paint_runs(&runs, Painter::new(Palette::Colour));
        assert!(painted.contains(sgr::BOLD), "{painted:?}");
        assert!(painted.contains(sgr::CYAN), "{painted:?}");
        assert_eq!(visible_width(&painted), "plain bold and code".len());
        // A palette with no colour is the plain text and no escapes at all.
        let plain = paint_runs(&runs, Painter::new(Palette::None));
        assert_eq!(plain, "plain bold and code");
        assert!(!plain.contains('\x1b'), "{plain:?}");
    }

    #[test]
    fn a_streamed_code_block_is_highlighted_once_per_line_not_once_per_frame() {
        // §13.3 one layer down, and the reason `BlockCache` keeps a highlighter
        // rather than calling a one-shot painter per frame. A full repaint per
        // delta would make the bytes handed to the lexer quadratic in the block,
        // and nothing on the screen would look different — which is exactly the
        // kind of regression an instrument has to exist for.
        let src: String = (0..300)
            .map(|i| format!("    let x{i} = \"value {i}\"; // comment {i}\n"))
            .collect();
        let doc = format!("```rust\n{src}```\n");
        let mut md = IncrementalMarkdown::new();
        let mut cache = BlockCache::new();
        let cfg = RenderConfig {
            width: 100,
            color: true,
            budget: Budget::default(),
            base: None,
        };
        let mut buf = String::new();
        let mut frames = 0u64;
        for ch in doc.chars() {
            buf.push(ch);
            if buf.chars().count() >= 16 {
                md.push(&buf);
                buf.clear();
                cache.split(&md, &cfg, 40);
                frames += 1;
            }
        }
        md.push(&buf);
        cache.split(&md, &cfg, 40);

        // What a repaint-per-frame would cost: the whole block, every frame.
        let naive = src.len() as u64 * frames;
        let actual = cache.bytes_highlighted();
        assert!(
            actual < naive / 8,
            "highlighted {actual} bytes over {frames} frames for a {}-byte block; \
             a full repaint per delta would be {naive}",
            src.len()
        );
    }

    #[test]
    fn painting_a_code_block_never_changes_the_text_in_it() {
        // The invariant that makes a highlighter safe to put underneath a
        // wrapper: strip the escapes and the source comes back.
        let block = Block::Code {
            lang: "rust".into(),
            lines: vec![
                "fn main() {".into(),
                "    let s = \"a string\"; // and a comment".into(),
                "}".into(),
            ],
            closed: true,
        };
        let cfg = RenderConfig {
            width: 100,
            color: true,
            budget: Budget::default(),
            base: None,
        };
        let lines = render_block(&block, &cfg);
        let plain: Vec<String> = lines
            .iter()
            .map(|l| strip(l))
            .filter(|l| l.starts_with("│ "))
            .map(|l| l[4..].to_string())
            .collect();
        assert_eq!(
            plain,
            vec![
                "fn main() {",
                "    let s = \"a string\"; // and a comment",
                "}"
            ]
        );
        // …and it is actually painted. In the theme's own colours: the roles moved
        // off the 256-colour cube, so the evidence is a keyword wearing `35`, not
        // an absolute `38;5;140`.
        let kw = letibot_ui::style::Palette::Colour.open(Role::Keyword);
        assert!(lines.iter().any(|l| l.contains(kw)), "{lines:?}");
        assert!(
            !lines.iter().any(|l| l.contains("\x1b[38;5;")),
            "a cube colour survived: {lines:?}"
        );
    }

    fn strip(s: &str) -> String {
        let mut out = String::new();
        let mut esc = false;
        for c in s.chars() {
            if esc {
                if c.is_ascii_alphabetic() {
                    esc = false;
                }
                continue;
            }
            if c == '\x1b' {
                esc = true;
                continue;
            }
            out.push(c);
        }
        out
    }

    #[test]
    fn a_wide_character_measures_two_columns_and_wraps() {
        // The bug the whole width swap was for. The old measure counted a char as
        // one column, so a line of CJK was rendered at half its real width and the
        // terminal wrapped it — putting a row on the screen the head had not
        // counted.
        assert_eq!(visible_width("你好"), 4);
        assert_eq!(visible_width("héllo"), 5);
        for l in wrap("你好世界这是一个测试用的句子没有空格", 10) {
            assert!(visible_width(&l) <= 10, "{l:?}");
        }
        assert!(wrap("你好世界这是一个测试用的句子没有空格", 10).len() > 1);
        // …and a truncation never splits a cluster.
        assert_eq!(visible_width(&trim_to("你好世界", 5)), 5);
    }

    #[test]
    fn a_decorated_cache_puts_the_rail_on_every_line_including_the_frozen_ones() {
        let d = Decor {
            prefix: "┃ ".into(),
            open: String::new(),
        };
        let mut md = IncrementalMarkdown::new();
        md.push("one paragraph\n\nanother paragraph\n\nand a third that is still open");
        let mut cache = BlockCache::decorated(d);
        let cfg = RenderConfig {
            width: 40,
            color: false,
            budget: Budget::default(),
            base: None,
        };
        let lines = cache.lines(&md, &cfg, 40);
        assert!(md.stable_count() > 0, "some of it must be frozen");
        for l in lines.iter().filter(|l| !l.is_empty()) {
            assert!(l.starts_with("┃ "), "{l:?}");
        }
    }

    #[test]
    fn every_block_kind_renders() {
        for b in lex(MARKDOWN) {
            assert!(!render_block(&b, &cfg()).is_empty(), "{b:?}");
        }
    }
}

#[cfg(test)]
mod tables {
    //! **The operator's own table, on the operator's own terminal.** 2026-09-17:
    //! *"table rendering is broken"* — a GFM table had no block of its own, so it
    //! lexed as a paragraph, joined with spaces and wrapped as prose.
    use super::*;
    use crate::markdown::lex;

    /// The table from the session that reported this, verbatim.
    const BOARD: &str = "| branch | commits | status |\n\
        |---|---|---|\n\
        | `autocompact` | `1129111`, `aeee854`, `2c0c6c4` | done, tested, unmerged |\n\
        | `webfetch` | `f363cb0` | done, tested (18 + tools 509 + harnessd offline), unmerged |\n\
        | `main` | moved to `7056c64` (your intent + plan commits) | — |\n";

    fn cfg(width: usize) -> RenderConfig {
        RenderConfig { width, color: false, ..RenderConfig::default() }
    }

    fn render(src: &str, width: usize) -> Vec<String> {
        lex(src).iter().flat_map(|b| render_block(b, &cfg(width))).collect()
    }

    /// A cell's plain text. The model holds runs; a test asserting on a table's
    /// contents means the text in it.
    fn cells(v: &[Vec<Run>]) -> Vec<String> {
        v.iter().map(|c| crate::markdown::runs_text(c)).collect()
    }

    #[test]
    fn a_table_is_a_table_and_not_a_paragraph_of_pipes() {
        let blocks = lex(BOARD);
        assert_eq!(blocks.len(), 1, "{blocks:#?}");
        let Block::Table { head, align, rows } = &blocks[0] else {
            panic!("not a table: {blocks:#?}");
        };
        assert_eq!(cells(head), ["branch", "commits", "status"]);
        assert_eq!(align, &[Align::Left, Align::Left, Align::Left]);
        assert_eq!(rows.len(), 3);
        assert_eq!(
            cells(&rows[2]),
            ["main", "moved to 7056c64 (your intent + plan commits)", "—"]
        );
    }

    #[test]
    fn the_columns_line_up_and_nothing_is_lost() {
        let out = render(BOARD, 120);
        let screen = out.join("\n");
        // Every cell's text survives.
        for want in ["branch", "autocompact", "1129111", "webfetch", "f363cb0", "harnessd offline", "7056c64"] {
            assert!(screen.contains(want), "{want} missing:\n{screen}");
        }
        // The separator column sits at the same place on the header and on the
        // first body row — which is the whole claim a table makes.
        let bar = |l: &str| l.char_indices().filter(|(_, c)| *c == '│').map(|(i, _)| i).collect::<Vec<_>>();
        assert!(!bar(&out[0]).is_empty(), "no column separators:\n{screen}");
        assert_eq!(bar(&out[0]), bar(&out[2]), "header and first row disagree:\n{screen}");
        // Nothing runs past the terminal.
        for l in &out {
            assert!(visible_width(l) <= 120, "{} columns: {l:?}", visible_width(l));
        }
    }

    #[test]
    fn a_narrow_terminal_wraps_the_wide_column_and_keeps_the_short_ones() {
        let out = render(BOARD, 60);
        let screen = out.join("\n");
        for l in &out {
            assert!(visible_width(l) <= 60, "{} columns: {l:?}", visible_width(l));
        }
        // The long status text is wrapped, not cut: every word still there.
        assert!(screen.contains("harnessd"), "{screen}");
        assert!(screen.contains("unmerged"), "{screen}");
        // And the narrow `branch` column was not taken down with it.
        assert!(screen.contains("autocompact"), "{screen}");
    }

    #[test]
    fn alignment_and_escaped_pipes_are_honoured() {
        let src = "| n | name | size |\n|--:|:----:|:-----|\n| 1 | a\\|b | wide |\n";
        let blocks = lex(src);
        let Block::Table { align, rows, .. } = &blocks[0] else { panic!("{blocks:#?}") };
        assert_eq!(align, &[Align::Right, Align::Center, Align::Left]);
        assert_eq!(rows[0][1], vec![Run { text: "a|b".into(), style: InlineStyle::Plain }],
            "an escaped pipe is a pipe, not a cell break");
        let out = render(src, 40);
        // Right-aligned `n`: the digit sits at the column's right edge, under the
        // header's own right edge.
        let col = |l: &str| l.find('│').unwrap_or(0);
        assert_eq!(col(&out[0]), col(&out[2]), "{out:#?}");
    }

    #[test]
    fn a_paragraph_with_a_pipe_in_it_is_still_a_paragraph() {
        for src in [
            "run `a | b` to pipe it\n",
            "| this looks like a row |\nbut the next line is prose\n",
            "|---|---|\n",
        ] {
            let blocks = lex(src);
            assert!(
                !blocks.iter().any(|b| matches!(b, Block::Table { .. })),
                "{src:?} lexed as a table: {blocks:#?}"
            );
        }
    }

    /// A table arriving a few bytes at a time renders the same as one that
    /// arrived whole — the incremental lexer freezes only at blank lines, and a
    /// table has none inside it.
    #[test]
    fn a_streamed_table_is_the_same_table() {
        let mut md = IncrementalMarkdown::new();
        for chunk in BOARD.as_bytes().chunks(7) {
            md.push(std::str::from_utf8(chunk).unwrap());
        }
        let streamed: Vec<&Block> = md.blocks().collect();
        assert_eq!(streamed.len(), 1, "{streamed:#?}");
        assert!(matches!(streamed[0], Block::Table { .. }), "{streamed:#?}");
    }
}

#[cfg(test)]
mod inline_render {
    use super::*;
    use crate::markdown::lex;

    fn cfg(width: usize) -> RenderConfig {
        RenderConfig { width, color: true, ..RenderConfig::default() }
    }

    /// The whole point of the workstream, asserted on the rendered output: a model's
    /// `**bold**` is bold on the screen and the asterisks are not.
    #[test]
    fn markers_are_styles_on_the_screen_and_not_characters() {
        let blocks = lex("plain **bold** and `code` and ~~struck~~ end\n");
        let out = render_block(&blocks[0], &cfg(60)).join("\n");
        assert!(out.contains(sgr::BOLD), "{out:?}");
        assert!(out.contains(sgr::CYAN), "{out:?}");
        assert!(out.contains(sgr::DIM), "{out:?}");
        assert!(!out.contains('*'), "a marker reached the screen: {out:?}");
        assert!(!out.contains('`'), "a marker reached the screen: {out:?}");
        assert!(!out.contains('~'), "a marker reached the screen: {out:?}");
        let plain = strip(&out);
        assert_eq!(plain, "plain bold and code and struck end");
    }

    /// A `*` that is not emphasis is text, on the screen as in the model.
    #[test]
    fn a_literal_asterisk_survives_the_renderer() {
        let blocks = lex("2 * 3 = 6\n");
        let out = render_block(&blocks[0], &cfg(40)).join("\n");
        assert_eq!(strip(&out), "2 * 3 = 6");
        assert!(!out.contains(sgr::ITALIC), "{out:?}");
    }

    /// Everything a real answer contains, rendered without losing a word.
    ///
    /// The failure this guards is the interesting one: a projection that drops a
    /// block, or a run, produces a *plausible* screen. Only comparing against the
    /// source's words catches it.
    #[test]
    fn a_whole_answer_renders_every_word_it_was_written_with() {
        let src = "\
## Why the cache missed

The short answer is `reasoning_content`. Three things had to line up:

1. The dialect replays prior reasoning.
2. The ledger appends **ids**, never re-derived text.

```rust
let a = 1;
```

> Note that **committed** tokens are what matters.

| n | name |
|--:|:-----|
| 1 | a\\|b |

---

done
";
        let blocks = lex(src);
        let out: String = blocks
            .iter()
            .flat_map(|b| render_block(b, &cfg(72)))
            .collect::<Vec<_>>()
            .join("\n");
        // Against the *stripped* text: a fenced block is syntax-coloured, so the raw
        // output has escapes inside `let a = 1;` and comparing un-stripped would be
        // asserting on the highlighter's token split rather than on the word.
        let text = strip(&out);
        for word in [
            "Why the cache missed",
            "reasoning_content",
            "line up",
            "The dialect replays",
            "never re-derived text",
            "let a = 1;",
            "committed",
            "tokens are what matters",
            "a|b",
            "done",
        ] {
            assert!(text.contains(word), "{word:?} is missing from: {text}");
        }
        assert!(!text.contains("**"), "{text}");
        assert!(!text.contains("|--:"), "the delimiter row reached the screen: {text}");
        // The hashes *do* stay, and deliberately: `render_block`'s heading arm keeps
        // them faint so the level survives a monochrome palette. What must not reach
        // the screen is a marker *inside* prose.
        assert!(
            text.lines().next().is_some_and(|l| l.starts_with("## Why")),
            "the level marker is drawn: {text}"
        );
        // The markers that did not reach the screen are styles that did.
        assert!(out.contains(sgr::BOLD), "nothing came out bold: {out:?}");
        assert!(out.contains(sgr::CYAN), "nothing came out as code: {out:?}");
    }

    /// Drop the SGR sequences, so an assertion can be about the text.
    fn strip(s: &str) -> String {
        let mut out = String::new();
        let mut it = s.chars().peekable();
        while let Some(c) = it.next() {
            if c == '\x1b' {
                while let Some(&n) = it.peek() {
                    it.next();
                    if n == 'm' {
                        break;
                    }
                }
                continue;
            }
            out.push(c);
        }
        out
    }
}
