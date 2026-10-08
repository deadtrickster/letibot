//! **The head's render config, and markdown drawn through `rano::markdown`.**
//!
//! The block painter that used to live here — headings, paragraphs, fences and their
//! highlighter, lists, quotes, tables, the bounded body and the frozen-prefix cache — is
//! rano's now (`rano::markdown::{render, view}`), ported from this file with its reasoning.
//! What stays is the glue only the head has: [`RenderConfig`] (the width, the palette a
//! `color`/`light` pair implies, the [`Budget`], the register a block is drawn inside), the
//! reasoning pane's string [`Decor`], the picture placement, and [`BlockCache`] — rano's
//! [`MarkdownView`] with its rows turned into the head's row strings once, as they settle.
//!
//! §13.3's wire rule and its lexer rule buy nothing if the *renderer* rebuilds every line
//! from every block on every delta — that is pi's mistake moved one layer down. So the
//! frozen prefix renders **once per width** (rano's view keeps its lines; this cache keeps
//! their strings), and only the live tail is re-rendered and re-converted per frame.
//!
//! # Bounded body
//!
//! > There is a matching known-good shape from that same work: render the head of
//! > a long block as a title and the tail as a bounded body, with the buffer size
//! > configurable rather than hardcoded.
//!
//! [`Budget`] is that size, and it is a value on [`RenderConfig`], not a constant in the
//! middle of a function; it reaches rano as [`RenderOptions::max_block_lines`]. A long
//! reasoning block shows its first line as a title, a count of what was elided, and its
//! last N lines — which is what a reader of a streaming model actually wants, because the
//! interesting end is the end.

use letibot_ui::painter::Painter;
use rano::style::{Palette, Role};

use rano::markdown::{Block, IncrementalMarkdown, MarkdownView, RenderOptions};
use rano::render::Line;

/// Columns, wrapping and truncation come from `letibot-ui`.
///
/// These were three functions here, and all three counted a `char` as one column.
/// The comment on the old `visible_width` called that *"the accepted cost of not
/// vendoring a width table"*, and the cost is not accepted anywhere a person can
/// see it: a status line one column too long is wrapped by the terminal, which
/// puts a row on the screen the head did not count, which scrolls the frame it
/// just painted. [`rano::width::text`] measures grapheme clusters and knows the
/// wide ranges, never splits a cluster or an escape, breaks between wide clusters
/// so CJK wraps at all, and carries SGR state across a break — which matters here
/// because [`crate::term::paint`] emits `\x1b[K` per row and erase-to-end-of-line
/// uses the *current* attributes.
pub use rano::width::text::{truncate as trim_to, width as visible_width, wrap};

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
    /// **Hyperlinks (OSC 8) are on, and relative paths resolve against this** — the session's
    /// workspace. `None` is a terminal that does not speak them, and every pipe, replay and
    /// test: the output is then byte-for-byte what it always was.
    pub links: Option<String>,
    /// The terminal said its background is light (OSC 11): colour renders with
    /// [`Palette::Light`].
    pub light: bool,
    /// Inline images (the kitty graphics protocol's Unicode placeholders) are on.
    pub images: bool,
}

impl Default for RenderConfig {
    fn default() -> Self {
        RenderConfig {
            width: 100,
            color: true,
            budget: Budget::default(),
            base: None,
            links: None,
            light: false,
            images: false,
        }
    }
}

/// **The images a reply's markdown names** — `![alt](target)` — as `(alt, target)`, in order,
/// local targets only: a URL is a picture this head would have to fetch, and it does not reach
/// the network.
pub fn markdown_images(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("![") {
        rest = &rest[at + 2..];
        let Some(close) = rest.find("](") else { break };
        // The alt text is one line; a `![` whose `](` is paragraphs away is not an image.
        if rest[..close].contains('\n') {
            continue;
        }
        let alt = rest[..close].trim().to_string();
        rest = &rest[close + 2..];
        let Some(end) = rest.find(')') else { break };
        // `(path "title")`: the title is not part of the path.
        let target = rest[..end].split(" \"").next().unwrap_or("").trim();
        let target = target.trim_start_matches('<').trim_end_matches('>');
        if !target.is_empty() && !target.contains("://") && !target.contains('\n') {
            out.push((alt, target.to_string()));
        }
        rest = &rest[end + 1..];
    }
    out
}

/// **The pictures a reply's markdown named that the head has uploaded**: `(item, target)` →
/// `(id, pixel width, pixel height)`. Filled by the head's upload pass, which reads the file;
/// read by the renderer, which must not touch the disk while it draws a frame — so a reference
/// the pass has not reached yet simply draws nothing until it has.
type ReplyImages = std::collections::BTreeMap<(String, String), (u32, Option<u32>, Option<u32>)>;
static REPLY_IMAGES: std::sync::Mutex<ReplyImages> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

pub fn remember_reply_image(item_id: &str, target: &str, image: (u32, Option<u32>, Option<u32>)) {
    if let Ok(mut m) = REPLY_IMAGES.lock() {
        m.insert((item_id.to_string(), target.to_string()), image);
    }
}

pub fn reply_image(item_id: &str, target: &str) -> Option<(u32, Option<u32>, Option<u32>)> {
    REPLY_IMAGES
        .lock()
        .ok()?
        .get(&(item_id.to_string(), target.to_string()))
        .copied()
}

/// **Where a reply's picture goes**: after the first rendered line, at or past `from`, that
/// shows its reference — the markdown itself when it is drawn literally (a code block), else
/// the bare target, else its alt text, which is what the markdown renderer leaves of an image.
/// In that order: the operator's reply named the path in a sentence before the `![…]` line, and
/// a search for the path alone hung the picture under the sentence. `None` when none of the
/// three is on any line, and the caller puts it at the end.
pub fn picture_anchor(lines: &[String], from: usize, alt: &str, target: &str) -> Option<usize> {
    let plain = |l: &String| {
        let mut t = String::with_capacity(l.len());
        rano::width::text::for_each_cell(l, |c| t.push_str(c.text));
        t
    };
    let reference = format!("]({target}");
    let on = |needle: &str| {
        lines
            .iter()
            .enumerate()
            .skip(from)
            .find(|(_, l)| plain(l).contains(needle))
    };
    on(&reference)
        .or_else(|| on(target))
        .or_else(|| {
            (!alt.is_empty())
                .then(|| {
                    lines
                        .iter()
                        .enumerate()
                        .skip(from)
                        .find(|(_, l)| plain(l).contains(alt))
                })
                .flatten()
        })
        .map(|(i, _)| {
            // **Out of the block it is in**: a reference drawn inside a code block's frame
            // (`│` rows, closed by `└`) puts the picture under the frame, not inside it.
            let mut at = i + 1;
            while at < lines.len() && plain(&lines[at]).trim_start().starts_with('│') {
                at += 1;
            }
            if at < lines.len() && plain(&lines[at]).trim_start().starts_with('└') {
                at += 1;
            }
            at
        })
}

impl RenderConfig {
    /// The `letibot-ui` palette this config implies.
    ///
    /// One place, because `color: false` is not a monochrome theme — it is the
    /// `--replay`, pipe-to-a-file and CI case, and it has to produce
    /// byte-identical output on every machine.
    pub fn palette(&self) -> Palette {
        if self.color && self.light {
            Palette::Light
        } else if self.color {
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
}

/// The `rano::markdown` options a config implies, with `limit` rows per block.
///
/// The register is [`RenderConfig::base`]: rano lays it under every row as the row's own
/// style, so a heading or a code span inside the reasoning cannot close to the terminal
/// default — the defect `RenderConfig::base` was introduced for, fixed structurally.
pub fn md_options(cfg: &RenderConfig, limit: usize) -> RenderOptions {
    RenderOptions {
        base: cfg.base,
        max_block_lines: limit,
    }
}

/// **rano's lines as the head's rows**: each one ANSI under the config's palette.
///
/// `rano::render::Line::to_ansi` opens each span's look and closes it with a reset — the
/// shape this head's string painter wrote, and the one a row pasted into another string
/// needs, since it cannot leave an attribute open in the next. Under [`Palette::None`] it
/// is the plain text and nothing else, byte for byte, which is what `--replay`, a pipe and
/// CI compare.
pub fn rows(lines: &[Line], p: Palette) -> Vec<String> {
    lines.iter().map(|l| l.to_ansi_inside(p)).collect()
}

/// **One rano line as the head's row string** — the edge every `rano::agent` widget's output
/// crosses on its way to the frame.
///
/// A line with no style of its own is [`Line::to_ansi_inside`], which is `to_ansi`: every
/// styled span opened and reset. A line that carries a register — a tool row's dim payload,
/// the reasoning — is that register opened once at the start, its spans closing back to it,
/// and a reset at the end: the shape this head's `dim(cfg, …)` around a `Painter::inside`
/// line always wrote. Under [`Palette::None`] it is the text and nothing else.
///
/// **A run of reversed spans is one reverse** — the highlighted row of a picker or a pane,
/// which this head always drew as `colour(REVERSE, row)`: the inverse opened once, the
/// spans inside it written in their own look with a plain reset, and one reset at the end
/// of the run. rano patches the highlight into each span, which is the same cells; this is
/// the bytes the head's frames (and its tests) have always carried.
///
/// That includes what the convention does after a coloured span inside the run: its plain
/// reset ends the inverse too, so the subagents pane's highlighted row is inverse up to its
/// state mark and not after it. That is letibot's row as it was, kept here on purpose and
/// named as the defect it is: fixing it is a change to what the row shows, and this move
/// changes nothing a person sees.
pub fn row(l: &Line, p: Palette) -> String {
    let open = l.style.look(p).sgr();
    if !open.is_empty() {
        return format!("{open}{}{}", l.to_ansi_inside(p), sgr::RESET);
    }
    let reversed =
        |sp: &rano::render::Span| sp.style.look(p).attrs.contains(rano::style::Attrs::REVERSE);
    if !l.spans.iter().any(reversed) {
        return l.to_ansi_inside(p);
    }
    let mut out = String::new();
    let mut i = 0;
    while i < l.spans.len() {
        if !reversed(&l.spans[i]) {
            out.push_str(&Line::new(vec![l.spans[i].clone()]).to_ansi_inside(p));
            i += 1;
            continue;
        }
        out.push_str(&p.reverse_look().sgr());
        while i < l.spans.len() && reversed(&l.spans[i]) {
            let sp = &l.spans[i];
            let mut look = sp.style.look(p);
            look.attrs = look.attrs.without(rano::style::Attrs::REVERSE);
            let seq = look.sgr();
            let text = Line::new(vec![rano::render::Span::raw(sp.content.clone())]).plain();
            if seq.is_empty() {
                out.push_str(&text);
            } else {
                out.push_str(&seq);
                out.push_str(&text);
                out.push_str(sgr::RESET);
            }
            i += 1;
        }
        out.push_str(sgr::RESET);
    }
    out
}

/// [`row`] for each of `lines`.
pub fn row_strings(lines: &[Line], p: Palette) -> Vec<String> {
    lines.iter().map(|l| row(l, p)).collect()
}

/// Render one block to rows, unbounded. One-shot: see `rano::markdown::render_block`.
pub fn render_block(b: &Block, cfg: &RenderConfig) -> Vec<String> {
    rows(
        &rano::markdown::render_block(b, cfg.width, &md_options(cfg, usize::MAX)),
        cfg.palette(),
    )
}

/// Render a block, summarising it if it exceeds `limit` lines: title, elision count, tail.
/// Never a silent truncation — the count is the disclosure, the same rule the log applies
/// to `dropped`.
pub fn render_bounded(b: &Block, cfg: &RenderConfig, limit: usize) -> Vec<String> {
    rows(
        &rano::markdown::render_bounded(b, cfg.width, &md_options(cfg, limit)),
        cfg.palette(),
    )
}

/// A per-line decoration applied to everything a [`BlockCache`] produces.
///
/// It exists so the reasoning pane can carry `card::REASONING_RAIL_WIDTH`'s `┃`
/// rail **without** copying its frozen prefix into every frame. The rail has to
/// be on every line — that is the point of it, it is the signal that survives a
/// copy-paste when the colour does not — and the only place it can be applied
/// once per line rather than once per line per frame is at the moment the line
/// enters the cache.
///
/// A string, not `rano::markdown::Decor`'s spans: the rail and the step it is set in are
/// this head's frame, painted by the head, and they wrap a row rano has already drawn.
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
}

/// Rendered rows for a growing document, with the frozen prefix cached.
///
/// rano's [`MarkdownView`] keeps the settled blocks' *lines* and re-renders only the tail;
/// this keeps the settled lines' *strings* beside it, converted once as each one settles,
/// so the per-frame path hands the prefix back borrowed and converts only the tail. A
/// frame's cost is then the tail and the window, not everything said so far (§13.3).
#[derive(Debug, Default)]
pub struct BlockCache {
    view: MarkdownView,
    decor: Decor,
    /// What `stable_rows` were converted under: width, palette, register, bound. The view
    /// re-renders on the first, third and fourth itself; the palette is the head's alone —
    /// rano's lines hold no colour — so a `color` flip must drop the strings here.
    key: Option<(usize, Palette, Option<Role>, usize)>,
    /// The view's settled lines, as rows, decorated.
    stable_rows: Vec<String>,
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

    /// Set the decoration, throwing the converted prefix away if it changed.
    ///
    /// It changes when `color` does, and a prefix decorated under the old palette
    /// is stale in exactly the way a prefix rendered at the old width is.
    pub fn set_decor(&mut self, decor: Decor) {
        if self.decor != decor {
            self.decor = decor;
            self.stable_rows.clear();
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
        let p = cfg.palette();
        let key = Some((cfg.width, p, cfg.base, limit));
        if self.key != key {
            self.key = key;
            self.stable_rows.clear();
        }
        let (stable, tail) = self.view.split(md, cfg.width, &md_options(cfg, limit));
        // The view only ever appends to its prefix while the key holds; it starts again
        // when the key moves, and that is the case cleared above.
        if stable.len() < self.stable_rows.len() {
            self.stable_rows.clear();
        }
        let decor = &self.decor;
        let row = |l: &Line| {
            // The row between two blocks is the view's blank and carries no rail: the
            // rail marks text, and a gap with a rail in it is a row of nothing.
            if l.is_empty() {
                String::new()
            } else {
                decor.apply(&l.to_ansi_inside(p))
            }
        };
        let have = self.stable_rows.len();
        self.stable_rows.extend(stable[have..].iter().map(row));
        let tail = tail.iter().map(row).collect();
        (&self.stable_rows, tail)
    }

    /// Blocks handed to rano's renderer over this cache's life.
    pub fn blocks_rendered(&self) -> u64 {
        self.view.blocks_rendered()
    }

    /// Parses performed over this cache's life, summed over every live code block — the
    /// §13.3 instrument one layer down from `IncrementalMarkdown::bytes_lexed`. A frame
    /// that draws a settled fence must not add to this.
    pub fn parses(&self) -> u64 {
        self.view.parses()
    }
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
    //! The glue's own tests. The painter's — fences, tabs, the bound, tables, inline styles —
    //! moved to `rano::markdown` with the painter, under the same names.
    use super::*;
    use letibot_sessionlog::testing::MARKDOWN;
    use letibot_ui::painter::Sgr;
    use rano::markdown::{InlineStyle, Run, lex};

    fn cfg() -> RenderConfig {
        RenderConfig {
            width: 72,
            color: false,
            budget: Budget::default(),
            base: None,
            links: None,
            light: false,
            images: false,
        }
    }

    /// **A fence streamed in is highlighted once its language arrives.** The first frame of a
    /// streamed fence usually holds only its backticks; the cache built the highlighter then,
    /// for no language, and kept it — so the block stayed plain while a one-shot render of
    /// the same text was coloured. Found porting this cache to `rano::markdown`.
    #[test]
    fn a_fence_named_after_its_backticks_is_still_highlighted() {
        let cfg = RenderConfig {
            width: 60,
            color: true,
            ..RenderConfig::default()
        };
        let keyword = rano::style::Palette::Colour.sgr(Role::Keyword);
        let mut md = IncrementalMarkdown::new();
        let mut cache = BlockCache::new();
        let mut last = Vec::new();
        for delta in ["Here:\n\n```", "rust\nfn main() {}\n", "```\n\nDone.\n"] {
            md.push(delta);
            last = cache.lines(&md, &cfg, 40);
        }
        let streamed = last.join("\n");
        assert!(
            streamed.contains(&keyword),
            "the streamed fence is plain:\n{streamed}"
        );
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
            Run {
                text: "a ".into(),
                style: InlineStyle::Plain,
            },
            Run {
                text: "code".into(),
                style: InlineStyle::Code,
            },
            Run {
                text: " b".into(),
                style: InlineStyle::Plain,
            },
        ];
        let block = Block::Paragraph { lines: vec![runs] };
        let colour = RenderConfig {
            color: true,
            ..cfg()
        };
        let coloured = render_block(&block, &colour).concat();
        assert!(coloured.len() > 10);
        assert_eq!(visible_width(&coloured), "a code b".len());
        let lines = wrap(&coloured, 20);
        assert_eq!(lines.len(), 1, "escapes must not consume width: {lines:?}");
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
            links: None,
            light: false,
            images: false,
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
mod picture_tests {
    use super::*;

    #[test]
    fn a_picture_goes_after_the_line_that_names_it() {
        let lines: Vec<String> = ["Done.", "│ ![sun](/x/sun.png)", "more", "sun again"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(picture_anchor(&lines, 0, "sun", "/x/sun.png"), Some(2));
        // Rendered markdown keeps only the alt text.
        assert_eq!(picture_anchor(&lines, 2, "sun again", "/nope.png"), Some(4));
        assert_eq!(picture_anchor(&lines, 0, "", "/nope.png"), None);
        // The path named in a sentence first: the picture goes under the markdown, not there.
        let named_twice: Vec<String> = ["Wrote /x/sun.png for you.", "![sun](/x/sun.png)", "end"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            picture_anchor(&named_twice, 0, "sun", "/x/sun.png"),
            Some(2)
        );
        // Inside a frame: after the frame closes.
        let framed: Vec<String> = ["┌─", "│ ![a](/p.png)", "│ more code", "└─", "after"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(picture_anchor(&framed, 0, "a", "/p.png"), Some(4));
    }
}
