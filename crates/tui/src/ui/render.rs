//! The head's render config and the glue over `rano::markdown`: [`RenderConfig`], the reasoning
//! pane's [`Decor`], the picture placement and [`BlockCache`]. The cache exists so a frame costs
//! the tail and the window - the settled prefix is converted once per width, not once per frame.

use rano::style::{Palette, Role};

use rano::markdown::{Block, IncrementalMarkdown, MarkdownView, RenderOptions};
use rano::render::{Line, Span, Style};

/// Columns, wrapping and truncation come from `letibot-ui`: counting a `char` as one column lets
/// the terminal wrap a row the head did not count, which scrolls the frame it just painted, and
/// the SGR state has to survive a break because `paint` erases each row with `\x1b[K`.
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
    /// "this is the one Enter takes" rather than a category - a second hue would read
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
    /// The role of the block this render happens *inside*. `None` is the top level, where a span
    /// closes with a plain reset. `Some(..)` is the whole reason the field exists: a reset is not a
    /// restore, so a heading or a code span in a themed block would close to the terminal default.
    pub base: Option<Role>,
    /// **Hyperlinks (OSC 8) are on, and relative paths resolve against this** - the session's
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

/// **The images a reply's markdown names** - `![alt](target)` - as `(alt, target)`, in order,
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

/// The pictures a reply's markdown named that the head has uploaded: `(item, target)` ->
/// `(id, pixel width, pixel height)`. The upload pass reads the file; the renderer must not touch
/// the disk while it draws, so a reference the pass has not reached draws nothing yet.
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

/// The pictures an operator's row carries that the head has uploaded: `(item, part)` ->
/// `(id, pixel width, pixel height)`. Same bargain as [`REPLY_IMAGES`]: the renderer has `&` and
/// must not encode a megabyte of base64 to draw, so an unreached picture draws nothing yet.
type UserImages = std::collections::BTreeMap<(String, usize), (u32, Option<u32>, Option<u32>)>;
static USER_IMAGES: std::sync::Mutex<UserImages> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// The word a user row's image id is spelled with - `s-7#img1`. The `#` and the word keep a
/// picture's id from colliding with the row's own, and the index keeps two pictures in one prompt
/// apart.
pub(crate) const USER_PART: &str = "img";

pub fn remember_user_image(item_id: &str, part: usize, image: (u32, Option<u32>, Option<u32>)) {
    if let Ok(mut m) = USER_IMAGES.lock() {
        m.insert((item_id.to_string(), part), image);
    }
}

/// The picture an uploaded row's part was sent under, or `None` while the pass has not reached it.
pub fn user_image(item_id: &str, part: usize) -> Option<(u32, Option<u32>, Option<u32>)> {
    USER_IMAGES
        .lock()
        .ok()?
        .get(&(item_id.to_string(), part))
        .copied()
}

/// **A picture placed in cells**: its image id, the box it occupies, and the pixels it was cut
/// from, absent when the upload pass could not measure them.
pub(crate) type PlacedPicture = (u32, u32, u32, (Option<u32>, Option<u32>));

/// The cell box of a user row's picture at this width: the thumbnail, the popup, and the placement
/// the upload pass uses, so the rows drawn always match what the terminal holds. The pixels come
/// back with it because the popup's box is cut from them.
pub(crate) fn user_picture_box(item_id: &str, part: usize, width: usize) -> Option<PlacedPicture> {
    let (id, w, h) = user_image(item_id, part)?;
    let (cols, rows) =
        rano::term::graphics::image_cells(w, h, rano::term::graphics::image_box(width));
    Some((id, cols, rows, (w, h)))
}

/// Where a reply's picture goes: after the first line at or past `from` showing its reference -
/// `](target` first, then the bare target, then the alt text - because a reply names the path in a
/// sentence before the `![..]` line. `None` when none is on a line; the caller ends the block.
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
    /// The `letibot-ui` palette this config implies. One place, because `color: false` is not a
    /// monochrome theme but the `--replay`, pipe and CI case, and its output has to be
    /// byte-identical on every machine.
    pub fn palette(&self) -> Palette {
        if self.color && self.light {
            Palette::Light
        } else if self.color {
            Palette::Colour
        } else {
            Palette::None
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

/// The `rano::markdown` options a config implies, with `limit` rows per block. The register is
/// [`RenderConfig::base`]: rano lays it under every row as the row's own style, so a heading or a
/// code span inside the reasoning cannot close to the terminal default.
pub fn md_options(cfg: &RenderConfig, limit: usize) -> RenderOptions {
    RenderOptions {
        base: cfg.base,
        max_block_lines: limit,
    }
}

/// **rano's markdown lines as the head's rows**: each one ANSI under the config's palette  -
/// the plain text and nothing else, byte for byte, under [`Palette::None`], which is what
/// `--replay`, a pipe and CI compare.
pub fn rows(lines: &[Line], p: Palette) -> Vec<String> {
    lines.iter().map(|l| md_row(l, p)).collect()
}

/// One markdown row. A row in the reasoning register is opened and reset by the pane's own
/// decor (`Decor::open`), so its spans close back to it: `Painter::inside`'s shape. Any other
/// row is written as the head's string painter nested it (`crate::ui::rows`).
fn md_row(l: &Line, p: Palette) -> String {
    if l.style == rano::render::Style::default() {
        row(l, p)
    } else {
        l.to_ansi_inside(p)
    }
}

pub use crate::ui::rows::{row, row_strings};

/// A printed path as a span the terminal can open (OSC 8), through rano's `file_url`, which
/// decides what a path is and carries this machine's name so a link drawn over ssh opens there.
/// `root` is `RenderConfig::links`: `None` - a pipe, a replay, a test - draws the plain span.
pub fn path_span(root: Option<&str>, text: &str, style: Style) -> Span {
    match root.and_then(|r| rano::term::links::file_url(r, text)) {
        Some(url) => Span::styled(text, style.link(url)),
        None => Span::styled(text, style),
    }
}

/// Render one block to rows, unbounded. One-shot: see `rano::markdown::render_block`.
pub fn render_block(b: &Block, cfg: &RenderConfig) -> Vec<String> {
    rows(
        &rano::markdown::render_block(b, cfg.width, &md_options(cfg, usize::MAX)),
        cfg.palette(),
    )
}

/// Render a block, summarising it if it exceeds `limit` lines: title, elision count, tail.
/// Never a silent truncation - the count is the disclosure, the same rule the log applies
/// to `dropped`.
pub fn render_bounded(b: &Block, cfg: &RenderConfig, limit: usize) -> Vec<String> {
    rows(
        &rano::markdown::render_bounded(b, cfg.width, &md_options(cfg, limit)),
        cfg.palette(),
    )
}

/// A per-line decoration applied to everything a [`BlockCache`] produces, so the reasoning pane
/// carries its rail without copying its frozen prefix into every frame: it is applied once, when
/// the line enters the cache. A string, because the rail wraps a row rano already drew.
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
    /// Decorate one line. Public because a head drawing a single line beside a cached block - the
    /// live tail of a folded reasoning pane - has to decorate it the same way, and only one
    /// function guarantees that.
    pub fn apply(&self, l: &str) -> String {
        match (self.prefix.is_empty(), self.open.is_empty()) {
            (true, true) => l.to_string(),
            (false, true) => format!("{}{l}", self.prefix),
            (true, false) => format!("{}{l}{}", self.open, sgr::RESET),
            (false, false) => format!("{}{}{l}{}", self.prefix, self.open, sgr::RESET),
        }
    }
}

/// The bytes of a document's head the cache compares to tell *the same document, grown* from
/// *another document*. See [`BlockCache::split`].
const DRAWN_HEAD: usize = 64;

/// Rendered rows for a growing document, with the frozen prefix cached: rano's `MarkdownView`
/// keeps the settled blocks' lines, this keeps their strings, converted once as each settles, so
/// a frame costs the tail and the window rather than everything said so far.
#[derive(Debug, Default)]
pub struct BlockCache {
    view: MarkdownView,
    decor: Decor,
    /// What `stable_rows` were converted under: width, palette, register, bound. The view
    /// re-renders on the first, third and fourth itself; the palette is the head's alone  -
    /// rano's lines hold no colour - so a `color` flip must drop the strings here.
    key: Option<(usize, Palette, Option<Role>, usize, bool)>,
    /// The document these rows were drawn from, as far as a frame can afford to check: its head
    /// bytes and its length. A hash would be O(document) per frame, so a replacement that shares
    /// those bytes and is no shorter still reads as growth.
    drawn: Option<(String, usize)>,
    /// The view's settled lines, as rows, decorated.
    stable_rows: Vec<String>,
    /// The live tail's rows and the document they were drawn from. A frame with nothing new - the
    /// settled steady state - hands these back instead of rendering the tail again; the key and the
    /// decor clear them, so what they depend on cannot change under them.
    tail_rows: Vec<String>,
    tail_of: Option<(u64, usize)>,
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

    /// Set the decoration, throwing the converted prefix away if it changed: a prefix decorated
    /// under the old palette is stale in exactly the way one rendered at the old width is.
    pub fn set_decor(&mut self, decor: Decor) {
        if self.decor != decor {
            self.decor = decor;
            self.stable_rows.clear();
            self.tail_of = None;
            self.drawn = None;
        }
    }

    /// Total lines for the document: the cached prefix plus a freshly rendered tail. Convenience
    /// over [`BlockCache::split`] that copies the prefix, so it is for a one-shot render of a
    /// finished document, not the per-frame path where that copy is O(accumulated output).
    pub fn lines(
        &mut self,
        md: &IncrementalMarkdown,
        cfg: &RenderConfig,
        limit: usize,
    ) -> Vec<String> {
        let (stable, tail) = self.split(md, cfg, limit);
        let mut out = stable.to_vec();
        out.extend(tail.iter().cloned());
        while out.last().is_some_and(|l| l.is_empty()) {
            out.pop();
        }
        out
    }

    /// The frozen prefix by reference and the live tail, rendered afresh when the document has
    /// moved. This is the per-frame form: the prefix grows without bound, and borrowing it keeps a
    /// frame proportional to the tail and the window.
    pub fn split(
        &mut self,
        md: &IncrementalMarkdown,
        cfg: &RenderConfig,
        limit: usize,
    ) -> (&[String], &[String]) {
        let p = cfg.palette();
        // **Whether a link may be written** - a markdown link's label carries its URL (rano's
        // `Run::link`), and a terminal that does not speak OSC 8 is given the label alone.
        let links = cfg.links.is_some();
        let key = Some((cfg.width, p, cfg.base, limit, links));
        if self.key != key {
            self.key = key;
            self.stable_rows.clear();
            self.tail_of = None;
            self.drawn = None;
        }
        // A document that is not an extension of the one these rows were drawn from starts the
        // cache over: a re-seat, a fork, a row re-rendered from another item. Without it the frame
        // is the old document's settled rows followed by the new one's tail.
        let raw = md.raw();
        let head = raw.get(..raw.len().min(DRAWN_HEAD)).unwrap_or("");
        if !self
            .drawn
            .as_ref()
            .is_some_and(|(h, n)| h == head && *n <= raw.len())
        {
            // The view goes with the rows: it keeps the settled blocks' lines keyed on the width
            // and the bound alone, so clearing the strings here would still draw the other
            // document's lines. A fresh view is what the parse layer's own fix does.
            self.view = MarkdownView::new();
            self.stable_rows.clear();
            self.tail_rows.clear();
            self.tail_of = None;
        }
        self.drawn = Some((head.to_string(), raw.len()));
        let now = Some((md.lex_calls(), md.raw().len()));
        if self.tail_of == now {
            return (&self.stable_rows, &self.tail_rows);
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
            } else if !links && l.spans.iter().any(|s| s.style.link.is_some()) {
                let mut l = l.clone();
                for s in &mut l.spans {
                    s.style.link = None;
                }
                decor.apply(&md_row(&l, p))
            } else {
                decor.apply(&md_row(l, p))
            }
        };
        let have = self.stable_rows.len();
        self.stable_rows.extend(stable[have..].iter().map(row));
        self.tail_rows = tail.iter().map(row).collect();
        self.tail_of = now;
        (&self.stable_rows, &self.tail_rows)
    }

    /// Blocks handed to rano's renderer over this cache's life.
    pub fn blocks_rendered(&self) -> u64 {
        self.view.blocks_rendered()
    }

    /// Parses performed over this cache's life, summed over every live code block - the
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
    //! The glue's own tests. The painter's - fences, tabs, the bound, tables, inline styles  -
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

    /// A fence streamed in is highlighted once its language arrives: the first frame of a streamed
    /// fence usually holds only its backticks, so a highlighter built then and kept would leave the
    /// block plain while a one-shot render of the same text is coloured.
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
            streamed.contains(keyword),
            "the streamed fence is plain:\n{streamed}"
        );
    }

    /// A document that is not an extension of the one the cache drew is not drawn as the old one's
    /// rows: reusing the settled rows by count would put the first document's fences above the
    /// second's tail. Growth keeps its rows - the restart is for another document, not for growth.
    #[test]
    fn a_replaced_document_is_not_drawn_as_the_previous_ones_rows() {
        let cfg = RenderConfig {
            width: 60,
            color: true,
            ..RenderConfig::default()
        };
        let mut md = IncrementalMarkdown::new();
        let mut cache = BlockCache::new();
        for delta in [
            "```rust\nfn one() {}\n```\n\n",
            "```rust\nlet x = \"\u{e9}\u{e9}\";\n```\n\n",
            "```rust\nlet y = \"\u{e9}\";\n```\n\nDone.\n",
        ] {
            md.push(delta);
            assert!(!cache.lines(&md, &cfg, 40).is_empty());
        }
        // Another document entirely, handed to the cache that drew the one above.
        let mut other = IncrementalMarkdown::new();
        other.push(
            "# a different document entirely\n\nwith a fence:\n\n```rust\nlet z = 1;\n```\n\n",
        );
        let joined = cache.lines(&other, &cfg, 40).join("\n");
        assert!(
            joined.contains("a different document"),
            "the cache drew the previous document's rows:\n{joined}"
        );
        assert!(
            !joined.contains("fn one"),
            "a row of a document nobody wrote survived:\n{joined}"
        );
        // **And a document that only GREW keeps its rows** - the restart is for another
        // document, not for growth. One more paragraph, the same cache: the text above it is
        // still on the frame and the new paragraph is drawn under it.
        let before = cache.lines(&other, &cfg, 40);
        other.push("and one more paragraph\n");
        let after = cache.lines(&other, &cfg, 40);
        let grown_rows = after.join("\n");
        assert!(grown_rows.contains("one more paragraph"), "{grown_rows}");
        assert!(
            grown_rows.contains("a different document"),
            "the settled prefix was dropped:\n{grown_rows}"
        );
        assert!(
            after.len() > before.len(),
            "the new paragraph drew no rows: {after:?}"
        );
    }

    /// **A printed path becomes a span the terminal can open** (OSC 8) - and a span that is
    /// not a path is left exactly as it was.
    #[test]
    fn a_paths_span_carries_the_link_the_terminal_opens() {
        let sp = path_span(Some("/w/proj"), "crates/tui/src/ui/render.rs", Style::new());
        let url = sp.style.link.as_deref().expect("the path is not a link");
        assert!(
            url.starts_with("file://") && url.ends_with("/w/proj/crates/tui/src/ui/render.rs"),
            "{url}"
        );
        assert_eq!(sp.content, "crates/tui/src/ui/render.rs");
        // An absolute path resolves to itself.
        let abs = path_span(Some("/w"), "/tmp/x.md", Style::new());
        assert!(
            abs.style
                .link
                .as_deref()
                .is_some_and(|u| u.ends_with("/tmp/x.md")),
            "{abs:?}"
        );
        // A terminal that does not speak links, and a target that is not one path, are the
        // same span they were before - no sequence, no guess.
        for (root, text) in [
            (None, "a/b.rs"),
            (Some("/w"), "cargo test --workspace"),
            (Some("/w"), "\"quoted\""),
            (Some("/w"), "(call_0)"),
            (Some("/w"), ""),
        ] {
            let sp = path_span(root, text, Style::of(Role::Strong));
            assert!(sp.style.link.is_none(), "{text:?} was linked: {sp:?}");
            assert_eq!(sp.content, text);
            assert_eq!(sp.style, Style::of(Role::Strong));
        }
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
                link: None,
            },
            Run {
                text: "code".into(),
                style: InlineStyle::Code,
                link: None,
            },
            Run {
                text: " b".into(),
                style: InlineStyle::Plain,
                link: None,
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
