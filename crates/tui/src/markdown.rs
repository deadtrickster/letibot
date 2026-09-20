//! Incremental markdown: §13.3's other half, on tree-sitter.
//!
//! > **What not to do:** pi's `updateContent` calls `contentContainer.clear()` and
//! > rebuilds every child from the whole accumulated `message.content` on **every**
//! > `message_update`, i.e. once per delta — O(n²) over a message.
//!
//! The mechanism is a **window**: everything the model has written that has settled
//! into a complete block is frozen into `stable` and never shown to a parser again;
//! only the still-growing tail is parsed, per delta.
//!
//! # Why the window, and not tree-sitter's incremental re-parse
//!
//! The obvious design is to hand the whole growing document to one tree-sitter
//! parser and let it reuse the prefix. `rano::syntax::Stream` was built for exactly
//! that, and it was measured: **it does not work.** `ts_parser__can_reuse_first_leaf`
//! (`parser.c`) refuses to reuse a token when the current parse state admits external
//! tokens, and markdown's block grammar runs a 48-state external scanner in nearly
//! every block state — so a markdown push re-lexes essentially the whole document,
//! ~106 ns per document byte, about what a full parse costs. Measured: 829 µs at the
//! start of a 1,000-push stream, 9.5 ms at the end (11×), and 88.8 ms per push for the
//! two-pass pipeline. Linear per push means quadratic over the stream. The numbers and
//! the tree-sitter source behind them are in `rano/TODO.md` §9 and on `Stream`'s doc
//! comment.
//!
//! So the bound cannot come from the parser's reuse; it has to come from **not giving
//! the parser the settled text at all**. That is what this window is, and it is also
//! what the hand-written lexer this replaced did. `rano::syntax::Stream` is still the
//! engine underneath — what changed is that it is fed a bounded window instead of an
//! unbounded document.
//!
//! # What settles, and what that costs
//!
//! A block settles when a *later* block starts after it: the parser has then seen the
//! text that could have changed its mind, so the next delta cannot. That is
//! tree-sitter's own reuse criterion, read off the tree instead of guessed from the
//! text, and it is strictly better than the four text guards this file used to carry
//! (it knows whether it is inside a fence, a list or a quote, because it parsed them).
//!
//! The one shape that never settles is a single block that keeps growing: one long
//! paragraph, and — because CommonMark makes a blank line between items one *loose*
//! list — one long bulleted answer. `max_unfrozen` bounds it: past that, the window is
//! cut at the last [`stable_boundary`] inside it, which is where the old text guards
//! survive. The cost of being wrong there is bounded and visible: two tight lists
//! render where one loose list was, a blank line's difference in a terminal. The cost
//! of the alternative is O(n²) on a 50 KB answer.
//!
//! # Instrumentation
//!
//! [`IncrementalMarkdown::bytes_lexed`] is kept in the shipping type rather than in a
//! test harness, because "is the renderer quadratic again" is a question that gets
//! asked once a year and is unanswerable after the fact. It counts the bytes handed to
//! a parser — the deltas, plus every window re-parse. A full re-parse per delta lexes
//! `O(n²)` bytes over a message; this lexes `O(n · window)`.

use rano::syntax::{Lang, Node, Stream};

/// One block. Line-oriented on purpose: a head renders lines, and a block model
/// finer than the thing being rendered is cost with no buyer.
///
/// The text-bearing fields are [`Run`]s, not `String`s: the inline grammar parses
/// bold, italic, `code`, links and strikethrough, and a block model that threw that
/// away would leave the renderer scanning for `**` again. `Code` keeps raw lines —
/// it is highlighted by `letibot_ui::highlight::StreamingCode`, and an emphasis
/// marker inside a fence is source code, not emphasis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Heading {
        level: u8,
        runs: Vec<Run>,
    },
    Paragraph {
        lines: Vec<Vec<Run>>,
    },
    /// `closed` is false while the fence is still open — which is the normal state
    /// of the last block of a streaming message, and the reason a head must be able
    /// to render an unterminated code block without waiting.
    Code {
        lang: String,
        lines: Vec<String>,
        closed: bool,
    },
    List {
        ordered: bool,
        /// The number the **first** item was written with, so an ordered list
        /// renders the numbers the model wrote.
        ///
        /// A blank line between items ends a block — that is what a blank line
        /// does here and it is right for paragraphs — so a *loose* list, which is
        /// what a model writes whenever an item runs to more than a sentence,
        /// arrives as six one-item lists. The renderer numbered from the item's
        /// index within its block, so all six rendered `1.` and the prose above
        /// them said "the six points below". Found by reading a real answer on the
        /// screen; it is the head misquoting the model, which is the same class as
        /// a card naming the wrong file.
        ///
        /// Keeping the written number is the smaller fix and the more honest one:
        /// a list that says `4.` says `4.` because the model typed `4.`, and a
        /// model that numbers its own list wrongly is not something a head should
        /// quietly correct.
        start: usize,
        items: Vec<Vec<Run>>,
    },
    Quote {
        lines: Vec<Vec<Run>>,
    },
    /// A GFM pipe table. Held as cells, never as the lines it was written on:
    /// the whole point is that the renderer decides the columns for the width it
    /// has, and a table lexed as a paragraph is joined with spaces and wrapped as
    /// prose — which is what the operator saw (2026-09-17, "table rendering is
    /// broken"): three rows of a branch table became one grey block of pipes.
    Table {
        head: Vec<Vec<Run>>,
        /// One per column of `head`, from the delimiter row.
        align: Vec<Align>,
        /// Ragged by construction: a row with fewer cells than the header is a
        /// row with empty cells, and one with more keeps them. What the model
        /// wrote is what is shown.
        rows: Vec<Vec<Vec<Run>>>,
    },
    Rule,
}

/// A run of text with one inline style, the unit the inline grammar produces.
///
/// The markers themselves are gone: `**bold**` is one `Run` whose text is `bold`
/// and whose style is [`InlineStyle::Bold`]. That is the whole point of parsing it —
/// the renderer no longer has to scan for asterisks, and a `*` that is not emphasis
/// (a multiplication sign, a footnote marker) stays literal because the grammar
/// said so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub text: String,
    pub style: InlineStyle,
}

/// The inline styles the conversation renderer knows.
///
/// A link carries no URL. The TUI has no pointer, so a head that cannot follow a
/// link showing the destination is noise; the *text* is the content, and an autolink
/// (`<https://…>`) already reads as its own URL because that is what its text is. The
/// grammar has the destination and the projection can grow a field for it if a
/// follow-a-link feature ever arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlineStyle {
    Plain,
    Bold,
    Italic,
    /// `***both***` — nesting, not a third delimiter.
    BoldItalic,
    Code,
    Strikethrough,
}

impl InlineStyle {
    /// The style of text inside a container of this style — i.e. `self` is the
    /// enclosing style and `inner` is the one being entered.
    ///
    /// Bold inside italic is the same as italic inside bold; beyond that the
    /// container being entered decides, and there is no deeper level, because a
    /// terminal has no more attributes to spend and a font that stacks them is
    /// unreadable.
    fn nest(self, inner: InlineStyle) -> InlineStyle {
        match (self, inner) {
            (InlineStyle::Bold, InlineStyle::Italic)
            | (InlineStyle::Italic, InlineStyle::Bold) => InlineStyle::BoldItalic,
            // Already both: a third marker does not add a fourth attribute.
            (InlineStyle::BoldItalic, _) => InlineStyle::BoldItalic,
            _ => inner,
        }
    }
}

/// What the delimiter row's colons asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

impl Block {
    /// A one-line name for the block, for §13.3's "render the head of a long block
    /// as a title".
    pub fn title(&self) -> String {
        match self {
            Block::Heading { runs, .. } => runs_text(runs),
            Block::Paragraph { lines } => lines.first().map(|l| runs_text(l)).unwrap_or_default(),
            Block::Code { lang, lines, .. } => {
                let lang = if lang.is_empty() { "code" } else { lang };
                format!("{lang} · {} lines", lines.len())
            }
            Block::List { items, .. } => items.first().map(|i| runs_text(i)).unwrap_or_default(),
            Block::Quote { lines } => lines.first().map(|l| runs_text(l)).unwrap_or_default(),
            Block::Table { head, rows, .. } => {
                format!("table · {} × {}", rows.len(), head.len())
            }
            Block::Rule => "───".into(),
        }
    }
}

/// The plain text of a run list, markers already gone.
pub fn runs_text(runs: &[Run]) -> String {
    let mut out = String::new();
    for r in runs {
        out.push_str(&r.text);
    }
    out
}

/// How large the unsettled window may grow before it is cut at a text boundary.
///
/// Configurable rather than hardcoded, per §13.3's matching rule for the display
/// buffer. Four kilobytes is about a screen of prose: large enough that no ordinary
/// block reaches it, small enough that the quadratic term never gets going. It bounds
/// *parse cost* now, not only tail length — at markdown's ~106 ns per byte that is
/// under half a millisecond for the worst single block shape there is.
pub const DEFAULT_MAX_UNFROZEN: usize = 4 * 1024;

/// How many times one `push` may cut the window before giving up and rendering what
/// it has. Each round settles at least one byte off the front, so this is a guard
/// against a pathological input, not a limit a real message reaches.
const MAX_SETTLE_ROUNDS: usize = 8;

/// A markdown document that grows only at the end.
#[derive(Default)]
pub struct IncrementalMarkdown {
    raw: String,
    /// Byte offset into [`Self::raw`] where the unsettled window starts. Everything
    /// before it is in [`Self::stable`] and will never be parsed again.
    window_start: usize,
    stable: Vec<Block>,
    tail: Vec<Block>,
    max_unfrozen: usize,
    /// The block stream, holding `raw[window_start..]`. Dropped and re-seated
    /// whenever the window moves, which is what keeps the settled text out of the
    /// parser's input.
    block: Option<Stream>,
    /// The window offset `block` was seated at, and how many bytes of `raw` it has
    /// been shown. Both must match for an incremental `push` to be valid.
    block_start: usize,
    block_len: usize,
    bytes_lexed: u64,
    lex_calls: u64,
}

impl IncrementalMarkdown {
    pub fn new() -> Self {
        IncrementalMarkdown {
            max_unfrozen: DEFAULT_MAX_UNFROZEN,
            ..IncrementalMarkdown::default()
        }
    }

    pub fn with_max_unfrozen(max_unfrozen: usize) -> Self {
        IncrementalMarkdown {
            max_unfrozen,
            ..IncrementalMarkdown::new()
        }
    }

    /// Append an increment. **This is the only mutator**, which is what makes the
    /// "no event carries accumulated text" rule usable: there is no `set_content`
    /// to call with the whole message.
    pub fn push(&mut self, delta: &str) {
        if delta.is_empty() {
            return;
        }
        self.raw.push_str(delta);
        self.refresh();
    }

    /// Blocks in order: the settled prefix, then the live tail.
    pub fn blocks(&self) -> impl Iterator<Item = &Block> {
        self.stable.iter().chain(self.tail.iter())
    }

    /// How many blocks have settled. A renderer caches exactly this many.
    pub fn stable_count(&self) -> usize {
        self.stable.len()
    }

    pub fn stable(&self) -> &[Block] {
        &self.stable
    }

    pub fn tail(&self) -> &[Block] {
        &self.tail
    }

    pub fn raw(&self) -> &str {
        &self.raw
    }

    pub fn is_empty(&self) -> bool {
        self.raw.is_empty()
    }

    /// Bytes handed to a parser over this document's life: the deltas pushed into the
    /// window stream, plus every window re-parse the settling did.
    ///
    /// The number a regression test watches. It is *not* the bytes tree-sitter
    /// internally rescanned — it cannot report that, and rano says so on
    /// `Stream::parse_calls`. What it measures is the thing this file controls: how
    /// much text we show a parser. If that grows quadratically, so does the renderer.
    pub fn bytes_lexed(&self) -> u64 {
        self.bytes_lexed
    }

    pub fn lex_calls(&self) -> u64 {
        self.lex_calls
    }

    /// How long the unsettled window is, in bytes. The quantity §2's invariant is
    /// about, and the one a test can assert on without timing anything.
    pub fn window_len(&self) -> usize {
        self.raw.len() - self.window_start
    }

    /// Bring the streams up to date with `raw`, then settle what has completed.
    ///
    /// Two signals, and both are needed. The **tree** says where the block boundaries
    /// are; the **text guards** ([`cut_point`]) say which of them can be cut at. The
    /// tree alone is not enough, and the way it fails is worth recording: `1. a\n\n2`
    /// parses as a list and a paragraph, so a cut after the list looks settled — and
    /// then `. b` arrives and the whole thing is one loose list. Whether a blank line
    /// ends a list depends on text that has not been written yet, which no tree can
    /// answer. That is guard 4, and it was already here.
    ///
    /// Settling **re-parses the prefix on its own** rather than taking the leading
    /// blocks out of the window's tree. It is one extra parse of a window that is
    /// bounded by the cap, and it buys the property outright: the guard says
    /// `parse(win[..cut]) ++ parse(win[cut..]) == parse(win)`, so the prefix's blocks
    /// are the right blocks by construction, whatever the window's tree did with a
    /// block that straddles the cut.
    fn refresh(&mut self) {
        for _ in 0..MAX_SETTLE_ROUNDS {
            self.feed_window();
            let win = self.raw[self.window_start..].to_string();
            let Some(cut) = cut_point(&win, self.max_unfrozen) else {
                self.tail = self.project();
                return;
            };
            let prefix = parse_standalone(&win[..cut], &mut self.bytes_lexed, &mut self.lex_calls);
            self.stable.extend(prefix);
            self.window_start += cut;
            // Round again: the stream is now seated on a smaller window, and there is
            // usually nothing more to do because what is left is one trailing block.
        }
        // Unreachable for any real input: each round moves `window_start` forward.
        // Rendering the window as-is is the safe thing to do with a stream that
        // somehow kept producing work.
        self.tail = self.project();
    }

    /// Make the block stream hold `raw[window_start..]`, incrementally where the
    /// window has not moved.
    fn feed_window(&mut self) {
        let seated = self.block.is_some() && self.block_start == self.window_start;
        if seated && self.block_len <= self.raw.len() {
            if self.block_len == self.raw.len() {
                return;
            }
            let delta = self.raw[self.block_len..].to_string();
            self.bytes_lexed += delta.len() as u64;
            self.lex_calls += 1;
            self.block.as_mut().unwrap().push(&delta);
            self.block_len = self.raw.len();
            return;
        }
        // The window moved (or this is the first push): a fresh stream over it. The
        // settled text is not in this string, which is the whole design.
        let win = self.raw[self.window_start..].to_string();
        let mut stream = Stream::new(Lang::Markdown);
        stream.push(&win);
        self.bytes_lexed += win.len() as u64;
        self.lex_calls += 1;
        self.block = Some(stream);
        self.block_start = self.window_start;
        self.block_len = self.raw.len();
    }

    /// The window's blocks.
    fn project(&mut self) -> Vec<Block> {
        let Some(root) = self.block.as_ref().and_then(Stream::root) else {
            return Vec::new();
        };
        let src = self.block.as_ref().map(|s| s.src().to_string()).unwrap_or_default();
        let spans = inline_spans(&src, &root, &mut self.bytes_lexed, &mut self.lex_calls);
        blocks_of(&root, &src, &spans)
    }
}

/// Parse a standalone span of markdown into blocks, both passes.
///
/// Used for a window prefix cut at a text boundary, and by [`lex`]. A fresh pair of
/// streams: no state survives, which is right for text that is being settled once.
fn parse_standalone(src: &str, bytes_lexed: &mut u64, lex_calls: &mut u64) -> Vec<Block> {
    let mut block = Stream::new(Lang::Markdown);
    block.push(src);
    *bytes_lexed += src.len() as u64;
    *lex_calls += 1;
    let Some(root) = block.root() else {
        return Vec::new();
    };
    let spans = inline_spans(src, &root, bytes_lexed, lex_calls);
    blocks_of(&root, src, &spans)
}

/// Lex a self-contained span of markdown into blocks.
///
/// The one-shot form of [`IncrementalMarkdown`], for text rendered once — a settled
/// transcript row, a fixture in a test. Same engine, same model, so the two cannot
/// drift.
pub fn lex(s: &str) -> Vec<Block> {
    let mut bytes = 0;
    let mut calls = 0;
    parse_standalone(s, &mut bytes, &mut calls)
}

// ---------------------------------------------------------------------------
// The inline pass
// ---------------------------------------------------------------------------

/// A style over a byte range, before it is cut into lines.
///
/// `style: None` is a range that is deliberately **not text**: an emphasis marker, a
/// link destination. The walk covers every byte of the inline range, so a consumer
/// never has to guess whether a stretch it did not see a span for is text or a marker.
/// Getting this wrong is not subtle — the first version left the markers uncovered and
/// `runs_of` handed them back as literal `**`, which is exactly the bug this whole
/// workstream exists to remove.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Span {
    start: usize,
    end: usize,
    style: Option<InlineStyle>,
}

/// Run the inline grammar over the parts of `root` that hold inline content and
/// return the styled spans, in document order.
///
/// This is markdown's two-pass structure (its README, "Standalone usage"): the block
/// grammar marks the ranges that are inline content, and a second parse with
/// `ts_parser_set_included_ranges` reads them. The ranges are the `inline` nodes and
/// the `pipe_table_cell` nodes — a table cell holds inline content too, which is easy
/// to miss and leaves every table cell plain.
fn inline_spans(src: &str, root: &Node, bytes_lexed: &mut u64, lex_calls: &mut u64) -> Vec<Span> {
    let ranges = inline_ranges(root);
    if ranges.is_empty() {
        return Vec::new();
    }
    let mut stream = Stream::new(Lang::MarkdownInline);
    stream.set_included_ranges(&ranges);
    stream.push(src);
    *bytes_lexed += src.len() as u64;
    *lex_calls += 1;
    let mut spans = Vec::new();
    if let Some(inline_root) = stream.root() {
        collect_spans(&inline_root, src, InlineStyle::Plain, &mut spans);
    }
    spans
}

/// The byte ranges that are inline content, with markdown's own block markers
/// removed: every `inline` and `pipe_table_cell` node in the block tree, in document
/// order, each split around the `block_continuation` children inside it.
///
/// That split is the whole reason this returns a *list* where one node would do. A
/// block quote's second line keeps its `> `, and the block grammar leaves it inside
/// the paragraph's inline node as a `block_continuation`. The inline grammar has never
/// heard of a block marker, so it parses that `>` as an anonymous token and it renders
/// as a literal `> three`. Cutting it out here — the block tree says exactly where it
/// is — is what keeps the second parse from seeing structure that is not its business.
///
/// `inline` nodes nest inside containers but never inside each other, so a plain walk
/// with no dedup is right.
fn inline_ranges(node: &Node) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    fn walk(n: &Node, out: &mut Vec<(usize, usize)>) {
        if n.kind == "inline" || n.kind == "pipe_table_cell" {
            push_around_continuations(n, out);
            return;
        }
        for c in &n.children {
            walk(c, out);
        }
    }
    walk(node, &mut out);
    out
}

/// A node's range, split around its `block_continuation` children.
fn push_around_continuations(n: &Node, out: &mut Vec<(usize, usize)>) {
    let mut cursor = n.start;
    for c in &n.children {
        if c.kind != "block_continuation" {
            continue;
        }
        if cursor < c.start {
            out.push((cursor, c.start));
        }
        cursor = cursor.max(c.end);
    }
    if cursor < n.end {
        out.push((cursor, n.end));
    }
}

/// Walk an inline tree, emitting a [`Span`] per styled piece of text.
///
/// The whole rule is **gaps are text**: a container's children are the things that
/// *are* something (`strong_emphasis`, `` `code_span` ``, a delimiter), and whatever
/// text lies between them belongs to the container's own style. That is not a
/// convenience — this grammar puts the content of `**bold**` in no node at all. Its
/// children are four `emphasis_delimiter`s and the word `bold` is the gap between the
/// second and the third. An implementation that recursed over children and expected
/// the text to be one of them produces a paragraph of literal asterisks, which is what
/// this one did first.
///
/// Markers are named nodes in this grammar, so dropping them by kind is what removes
/// the `**` without removing anything a reader wants. Link destinations and titles go
/// the same way: they are not text the reader sees.
fn collect_spans(node: &Node, src: &str, style: InlineStyle, out: &mut Vec<Span>) {
    match node.kind.as_str() {
        // Markers and link targets: covered, and not text.
        "emphasis_delimiter" | "code_span_delimiter" | "link_destination" | "link_title"
        | "link_label" => dropped(node, out),
        // A code span's content is raw: the inline grammar does not parse inside it,
        // so the whole interior is one span and the backticks are gone.
        "code_span" => {
            dropped(node, out);
            if let Some((a, b)) = code_span_inner(node) {
                out.push(Span {
                    start: a,
                    end: b,
                    style: Some(InlineStyle::Code),
                });
            }
        }
        // A hard break is the newline the model meant; the marker that made it hard —
        // two trailing spaces, or a backslash — goes, and the break stays.
        "hard_line_break" => {
            let end = node.end;
            if end > node.start && src.as_bytes().get(end - 1) == Some(&b'\n') {
                out.push(Span {
                    start: node.start,
                    end: end - 1,
                    style: None,
                });
                out.push(Span {
                    start: end - 1,
                    end,
                    style: Some(style),
                });
            } else {
                dropped(node, out);
            }
        }
        // An autolink shows its URL and not the angle brackets that made it a link:
        // the whole node is covered first, then the interior on top of it.
        "uri_autolink" | "email_autolink" => {
            dropped(node, out);
            if node.end > node.start + 1 && src.as_bytes().get(node.start) == Some(&b'<') {
                out.push(Span {
                    start: node.start + 1,
                    end: node.end - 1,
                    style: Some(style),
                });
            }
        }
        "emphasis" => gaps(node, src, style.nest(InlineStyle::Italic), out),
        "strong_emphasis" => gaps(node, src, style.nest(InlineStyle::Bold), out),
        "strikethrough" => gaps(node, src, InlineStyle::Strikethrough, out),
        // Only the label is shown; the destination and title were dropped above. The
        // label's own text is its gap, so it gets the same walk.
        "inline_link" | "collapsed_reference_link" | "full_reference_link" | "shortcut_link"
        | "image" => {
            for c in &node.children {
                if c.kind == "link_text" || c.kind == "image_description" {
                    gaps(c, src, style, out);
                } else {
                    dropped(c, out);
                }
            }
        }
        _ => gaps(node, src, style, out),
    }
}

/// Mark a node's whole range as not text.
fn dropped(node: &Node, out: &mut Vec<Span>) {
    if node.end > node.start {
        out.push(Span {
            start: node.start,
            end: node.end,
            style: None,
        });
    }
}

/// The text between `node`'s children, at `style`, with each child deciding for
/// itself. Covers every byte of the node.
fn gaps(node: &Node, src: &str, style: InlineStyle, out: &mut Vec<Span>) {
    let mut cursor = node.start;
    for c in &node.children {
        if cursor < c.start {
            out.push(Span {
                start: cursor,
                end: c.start,
                style: Some(style),
            });
        }
        collect_spans(c, src, style, out);
        cursor = cursor.max(c.end);
    }
    if cursor < node.end {
        out.push(Span {
            start: cursor,
            end: node.end,
            style: Some(style),
        });
    }
}

/// The interior of a `` `code` `` span: past the opening delimiter run and before the
/// closing one.
fn code_span_inner(node: &Node) -> Option<(usize, usize)> {
    let mut delims = node
        .children
        .iter()
        .filter(|c| c.kind == "code_span_delimiter");
    let first = delims.next()?;
    let last = delims.last().unwrap_or(first);
    Some((first.end, last.start.max(first.end)))
}

// ---------------------------------------------------------------------------
// The block pass
// ---------------------------------------------------------------------------

/// Map the window's tree children to blocks.
///
/// The block grammar wraps the document in containers — `document`, and a `section`
/// per heading — so the blocks are the leaves of that chain, not `root`'s direct
/// children. Walking through them is what makes the flat block list the renderer
/// wants, and it is why a heading's section does not become one giant block.
fn blocks_of(root: &Node, src: &str, spans: &[Span]) -> Vec<Block> {
    let mut blocks = Vec::new();
    collect_blocks(root, src, spans, &mut blocks);
    blocks
}

/// Containers the block grammar wraps blocks in. They carry no text of their own.
fn is_container(kind: &str) -> bool {
    matches!(kind, "document" | "section")
}

fn collect_blocks(node: &Node, src: &str, spans: &[Span], blocks: &mut Vec<Block>) {
    for c in &node.children {
        if !c.named {
            continue;
        }
        if is_container(&c.kind) {
            collect_blocks(c, src, spans, blocks);
            continue;
        }
        if let Some(block) = block_of(c, src, spans) {
            blocks.push(block);
        }
    }
}

fn block_of(node: &Node, src: &str, spans: &[Span]) -> Option<Block> {
    match node.kind.as_str() {
        "atx_heading" | "setext_heading" => {
            let level = heading_level(node);
            let runs = content_ranges(node)
                .first()
                .map(|&(a, b)| single_line(src, spans, a, b))
                .unwrap_or_default();
            Some(Block::Heading { level, runs })
        }
        "paragraph" => Some(Block::Paragraph {
            lines: lines_of_node(node, src, spans),
        }),
        "block_quote" => {
            let mut lines = Vec::new();
            subtree_lines(node, src, spans, &mut lines);
            if lines.is_empty() {
                lines.push(Vec::new());
            }
            Some(Block::Quote { lines })
        }
        "fenced_code_block" | "indented_code_block" => Some(code_block(node, src)),
        "list" => Some(list_block(node, src, spans)),
        "pipe_table" => Some(table_block(node, src, spans)),
        "thematic_break" => Some(Block::Rule),
        // Raw HTML and anything the grammar grew that this projection does not know:
        // shown as the text it is rather than dropped. A head that silently loses a
        // block is worse than one that shows it unstyled.
        _ => {
            let text = &src[node.start..node.end];
            if text.trim().is_empty() {
                return None;
            }
            Some(Block::Paragraph {
                lines: text.lines().map(|l| vec![run(l.to_string(), InlineStyle::Plain)]).collect(),
            })
        }
    }
}

fn heading_level(node: &Node) -> u8 {
    for c in &node.children {
        match c.kind.as_str() {
            "atx_h1_marker" | "setext_h1_underline" => return 1,
            "atx_h2_marker" | "setext_h2_underline" => return 2,
            "atx_h3_marker" => return 3,
            "atx_h4_marker" => return 4,
            "atx_h5_marker" => return 5,
            "atx_h6_marker" => return 6,
            _ => {}
        }
    }
    1
}

/// The ranges of the `inline` nodes under a block node, in document order.
///
/// A paragraph's text is its inline node's text — which is not the same as the
/// paragraph's own range, because the range of `### title` starts at the hashes and
/// the range of a list item starts at its marker.
fn content_ranges(node: &Node) -> Vec<(usize, usize)> {
    inline_ranges(node)
}

/// Every line of a block's inline content.
///
/// One inline node covers several source lines (a paragraph is one inline node), so
/// the text is cut at `\n` into the lines the renderer wraps. The ranges are walked
/// into one shared line accumulator rather than concatenated per range: a quote's
/// content arrives as two ranges with the `> ` cut out between them, and the first one
/// ends with the newline that starts the second's line. Appending `lines_of` results
/// would put an empty line there.
fn lines_of_node(node: &Node, src: &str, spans: &[Span]) -> Vec<Vec<Run>> {
    let mut out = Vec::new();
    lines_of_ranges(src, spans, &content_ranges(node), &mut out);
    if out.is_empty() {
        out.push(Vec::new());
    }
    out
}

/// A block's reader-facing lines, whatever kind of blocks it holds.
///
/// [`lines_of_node`] only sees inline content, which is right for a paragraph and
/// wrong for the containers the grammar lets hold *blocks*: a `block_quote` or a
/// `list_item` can contain a `fenced_code_block`, a `pipe_table`, a nested list. Asking
/// one of those for its inline content gets nothing, and the whole subtree — the code
/// the model wrote — is dropped. Found by rendering `> ```rust …``` `, which came out as
/// an empty quote.
///
/// So the flat model (`Block::Quote` holds run lines, `List::items` holds run lists —
/// the renderer has one indent level, see §2) is filled by walking the subtree for
/// everything a reader would see. The cost is that a quoted fence is quote prose rather
/// than a coloured code box: the model cannot say "this line is code" inside a quote,
/// and showing the ``` markers instead would be worse.
fn subtree_lines(node: &Node, src: &str, spans: &[Span], out: &mut Vec<Vec<Run>>) {
    for c in &node.children {
        match c.kind.as_str() {
            // Structure and link targets: not text.
            "block_quote_marker" | "block_continuation" | "list_marker_minus"
            | "list_marker_plus" | "list_marker_star" | "list_marker_dot"
            | "list_marker_parenthesis" | "task_list_marker_checked"
            | "task_list_marker_unchecked" | "fenced_code_block_delimiter" | "info_string"
            | "link_destination" | "link_title" | "link_label" => {}
            // Text, with inline styling.
            "inline" => lines_of_ranges(src, spans, &inline_ranges(c), out),
            // Raw code: its content, continuation markers cut, as plain lines.
            "code_fence_content" => {
                for line in text_without_continuations(c, src).lines() {
                    out.push(vec![run(line.to_string(), InlineStyle::Plain)]);
                }
            }
            // A nested table: one line per row, cells joined. The flat model has no
            // columns to give it here.
            "pipe_table" => {
                let rows = c
                    .children
                    .iter()
                    .filter(|g| g.kind == "pipe_table_header" || g.kind == "pipe_table_row");
                for row in rows {
                    let mut line: Vec<Run> = Vec::new();
                    for cell in row.children.iter().filter(|g| g.kind == "pipe_table_cell") {
                        if !line.is_empty() {
                            push_text(&mut line, " ", InlineStyle::Plain);
                        }
                        let (a, b) = trim_range(src, cell.start, cell.end);
                        push_text(&mut line, &src[a..b], InlineStyle::Plain);
                    }
                    if !line.is_empty() {
                        out.push(line);
                    }
                }
            }
            _ => subtree_lines(c, src, spans, out),
        }
    }
}

/// The lines of a run of ranges, cut at `\n`, appended to `out`.
fn lines_of_ranges(
    src: &str,
    spans: &[Span],
    ranges: &[(usize, usize)],
    out: &mut Vec<Vec<Run>>,
) {
    let mut cur: Vec<Run> = out.pop().unwrap_or_default();
    for &(a, b) in ranges {
        for r in runs_of(src, spans, a, b) {
            for (i, piece) in r.text.split('\n').enumerate() {
                if i > 0 {
                    out.push(std::mem::take(&mut cur));
                }
                if !piece.is_empty() {
                    cur.push(run(piece.to_string(), r.style));
                }
            }
        }
    }
    out.push(cur);
}

/// One list item's text from its body nodes, folded into the single run list the model
/// has room for.
///
/// `subtree_lines_one` per node rather than one call over the item: the item's children
/// have already been filtered, so a nested list is not in here, but the fallback that
/// catches a leaf without children has to apply to each node rather than to the item as
/// a whole.
fn item_runs(body: &[Node], src: &str, spans: &[Span]) -> Vec<Run> {
    let mut lines = Vec::new();
    for n in body {
        subtree_lines_one(n, src, spans, &mut lines);
    }
    let mut out: Vec<Run> = Vec::new();
    for (i, line) in lines.into_iter().enumerate() {
        if i > 0 {
            push_text(&mut out, " ", InlineStyle::Plain);
        }
        for r in line {
            push_text(&mut out, &r.text, r.style);
        }
    }
    out
}

/// [`subtree_lines`] for a single detached node: the node's own contribution, then
/// each of its children's.
fn subtree_lines_one(node: &Node, src: &str, spans: &[Span], out: &mut Vec<Vec<Run>>) {
    let before = out.len();
    subtree_lines(node, src, spans, out);
    // A node with no children of its own contributes its whole text — the case a
    // detached leaf (a `paragraph` stripped of its `inline`) falls into.
    if out.len() == before {
        let text = text_without_continuations(node, src);
        for line in text.trim_end_matches('\n').lines() {
            out.push(vec![run(line.to_string(), InlineStyle::Plain)]);
        }
    }
}

fn code_block(node: &Node, src: &str) -> Block {
    let mut lang = String::new();
    let mut fences = 0;
    for c in &node.children {
        match c.kind.as_str() {
            "info_string" => lang = src[c.start..c.end].trim().to_string(),
            "fenced_code_block_delimiter" => fences += 1,
            _ => {}
        }
    }
    // `tree-sitter-md` closes a fence at end of input, the way CommonMark does, so
    // the *missing* closing delimiter is not an error node — it is simply absent.
    // Counting delimiters is what tells the renderer to say "(still writing…)".
    let closed = node.kind == "indented_code_block" || fences >= 2;
    Block::Code {
        lang,
        lines: fence_content_lines(node, src),
        closed,
    }
}

/// The code lines of a fence, with the `block_continuation` markers cut out.
///
/// The content node carries them: a fence inside a block quote has content
/// `"let a = 1;\n> "` — the `> ` that opens the closing line is a child of the
/// *content*, not of the quote. Left in, the code block's last line would be a bare
/// `> `.
fn fence_content_lines(node: &Node, src: &str) -> Vec<String> {
    let Some(body) = node.children.iter().find(|c| c.kind == "code_fence_content") else {
        return Vec::new();
    };
    // The content's range stops before the closing fence, but its text ends with the
    // newline that opened that line.
    text_without_continuations(body, src)
        .trim_end_matches('\n')
        .lines()
        .map(str::to_string)
        .collect()
}

/// A node's text with its `block_continuation` children cut out.
fn text_without_continuations(node: &Node, src: &str) -> String {
    let mut out = String::new();
    let mut cursor = node.start;
    for c in &node.children {
        if c.kind != "block_continuation" {
            continue;
        }
        if cursor < c.start {
            out.push_str(&src[cursor..c.start]);
        }
        cursor = cursor.max(c.end);
    }
    if cursor < node.end {
        out.push_str(&src[cursor..node.end]);
    }
    out
}

fn list_block(node: &Node, src: &str, spans: &[Span]) -> Block {
    let mut items = Vec::new();
    let mut ordered = false;
    let mut start = 1;
    let mut first = true;
    collect_items(node, src, spans, &mut items, &mut ordered, &mut start, &mut first);
    Block::List {
        ordered,
        start,
        items,
    }
}

/// Flatten a list's items, nested lists included.
///
/// Nesting is flattened rather than modelled because the renderer has one indent
/// level, and because that is what the lexer this replaced did: a sub-bullet showed
/// up as an item of the list above it. Modelling the tree would be better and is not
/// what a conversation needs — a nested list inside a chat answer is a definition
/// list, and reading it flat is fine.
fn collect_items(
    node: &Node,
    src: &str,
    spans: &[Span],
    items: &mut Vec<Vec<Run>>,
    ordered: &mut bool,
    start: &mut usize,
    first: &mut bool,
) {
    for c in &node.children {
        match c.kind.as_str() {
            "list_item" => {
                let marker = c.children.iter().find(|g| g.kind.starts_with("list_marker"));
                // Everything in the item except a nested list, which follows as its own
                // items. `subtree_runs` rather than a byte range: an item can hold a
                // fenced block, and a byte sweep of the range turned the fence markers
                // into item text.
                let rest: Vec<Node> = c
                    .children
                    .iter()
                    .filter(|g| {
                        g.kind != "list"
                            && g.kind != "block_continuation"
                            && !g.kind.starts_with("list_marker")
                            && g.kind != "task_list_marker_checked"
                            && g.kind != "task_list_marker_unchecked"
                    })
                    .cloned()
                    .collect();
                if *first {
                    *first = false;
                    if let Some(m) = marker {
                        let is_ordered = m.kind == "list_marker_dot"
                            || m.kind == "list_marker_parenthesis";
                        *ordered = is_ordered;
                        if is_ordered {
                            // The marker node's text includes the delimiter and the
                            // space that follows it — `"7. "` — so the trailing
                            // punctuation comes off before the digits are read.
                            *start = src[m.start..m.end]
                                .trim()
                                .trim_end_matches(['.', ')'])
                                .parse()
                                .unwrap_or(1);
                        }
                    }
                }
                let item = item_runs(&rest, src, spans);
                if !item.is_empty() {
                    items.push(item);
                }
                for g in &c.children {
                    if g.kind == "list" {
                        collect_items(g, src, spans, items, ordered, start, first);
                    }
                }
            }
            "list" => collect_items(c, src, spans, items, ordered, start, first),
            _ => {}
        }
    }
}

fn table_block(node: &Node, src: &str, spans: &[Span]) -> Block {
    let mut head = Vec::new();
    let mut align = Vec::new();
    let mut rows = Vec::new();
    for c in &node.children {
        match c.kind.as_str() {
            "pipe_table_header" => head = cells_of(c, src, spans),
            "pipe_table_delimiter_row" => align = align_of(c, src),
            "pipe_table_row" => rows.push(cells_of(c, src, spans)),
            _ => {}
        }
    }
    Block::Table { head, align, rows }
}

fn cells_of(row: &Node, src: &str, spans: &[Span]) -> Vec<Vec<Run>> {
    row.children
        .iter()
        .filter(|c| c.kind == "pipe_table_cell")
        .map(|c| {
            let (a, b) = trim_range(src, c.start, c.end);
            single_line(src, spans, a, b)
        })
        .collect()
}

fn align_of(row: &Node, src: &str) -> Vec<Align> {
    row.children
        .iter()
        .filter(|c| c.kind == "pipe_table_delimiter_cell")
        .map(|c| {
            let t = src[c.start..c.end].trim().trim_matches('|').trim();
            match (t.starts_with(':'), t.ends_with(':')) {
                (true, true) => Align::Center,
                (false, true) => Align::Right,
                _ => Align::Left,
            }
        })
        .collect()
}

/// Tighten a range past whitespace and the pipes a cell is written between, so a
/// cell does not carry its own `|` into the rendered table.
fn trim_range(src: &str, a: usize, b: usize) -> (usize, usize) {
    let text = &src[a..b];
    let start = a + (text.len() - text.trim_start_matches([' ', '|']).len());
    let end = b - (text.len() - text.trim_end_matches([' ', '|']).len());
    (start, end.max(start))
}

// ---------------------------------------------------------------------------
// Spans to runs
// ---------------------------------------------------------------------------

/// The styled runs of `src[a..b]`, with markers already gone.
///
/// A span with no style contributes no text — that is a marker or a link destination.
/// A stretch no span covers is text, which should not happen (the inline walk covers
/// every byte) and is shown rather than dropped if it does.
fn runs_of(src: &str, spans: &[Span], a: usize, b: usize) -> Vec<Run> {
    let mut out: Vec<Run> = Vec::new();
    if a >= b {
        return out;
    }
    let mut pos = a;
    for s in spans.iter().filter(|s| s.end > a && s.start < b) {
        let (s0, s1) = (s.start.max(a), s.end.min(b));
        if s0 > pos {
            push_text(&mut out, &src[pos..s0], InlineStyle::Plain);
        }
        if s1 > s0 && let Some(style) = s.style {
            push_text(&mut out, &src[s0..s1], style);
        }
        pos = pos.max(s1);
    }
    if b > pos {
        push_text(&mut out, &src[pos..b], InlineStyle::Plain);
    }
    out
}

/// Add a run, undoing any `\`-escape in it, and coalescing it into the previous run
/// when the style is the same.
///
/// The coalescing is not cosmetic tidiness: a `\|` is its own node, so without it
/// `a\|b` arrives as three runs where the model wrote one word, and every consumer
/// that compares a cell or a title against a string has to join them first.
///
/// Not inside a code span: a backslash in `` `a\|b` `` is a backslash, because
/// CommonMark does not read escapes in code. Everywhere else `\|` is a pipe, and the
/// grammar already decided which backslashes are escapes (`backslash_escape` nodes) —
/// this only has to undo the one it recognised, which is why a `\d` in a regex keeps
/// its backslash.
fn push_text(out: &mut Vec<Run>, text: &str, style: InlineStyle) {
    if text.is_empty() {
        return;
    }
    let text = if style != InlineStyle::Code && text.contains('\\') {
        unescape(text)
    } else {
        text.to_string()
    };
    if let Some(last) = out.last_mut()
        && last.style == style
    {
        last.text.push_str(&text);
        return;
    }
    out.push(run(text, style));
}

/// Drop the backslash from `\`-punctuation, the way CommonMark reads one.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\'
            && let Some(next) = chars.clone().next()
            && next.is_ascii_punctuation()
        {
            out.push(chars.next().unwrap());
            continue;
        }
        out.push(c);
    }
    out
}

/// One item or cell: its text with the newlines folded to spaces.
///
/// The block model has one run list per item, and an item that wrapped in the source
/// wrapped because the model was writing prose, not because it meant a line break.
fn single_line(src: &str, spans: &[Span], a: usize, b: usize) -> Vec<Run> {
    let mut runs = runs_of(src, spans, a, b);
    for r in &mut runs {
        if r.text.contains('\n') {
            r.text = r.text.split_whitespace().collect::<Vec<_>>().join(" ");
        }
    }
    runs.retain(|r| !r.text.is_empty());
    runs
}

fn run(text: String, style: InlineStyle) -> Run {
    Run { text, style }
}

/// Where to settle an over-long window.
///
/// [`stable_boundary_with`] first: a blank line the guards agree about, which is where
/// two parses of the halves agree with one parse of the whole. While the window is
/// under the cap that is the *only* answer — no safe boundary means nothing settles,
/// which is the strict and correct behaviour, and is why a window can sit at a few
/// kilobytes for a while.
///
/// Past the cap the window would otherwise grow without bound and take the per-push
/// cost with it, so the guards are relaxed: `stable_boundary_with` is handed the cap as
/// its `relax_at`, and when there is no blank line at all — one paragraph with no blank
/// line anywhere in it — the last line break, or failing that the last space.
///
/// The fallbacks are *wrong* in the way §13.3's notes say a bounded wrongness is
/// acceptable: two halves render as two paragraphs where the model wrote one. The
/// alternative is quadratic on exactly the shape that has no other boundary, which is
/// worse than a paragraph break whose cause a reader cannot see.
fn cut_point(win: &str, max: usize) -> Option<usize> {
    if let Some(b) = stable_boundary_with(win, max) {
        return Some(b);
    }
    if win.len() < max {
        return None;
    }
    let head = &win[..max];
    if let Some(i) = head.rfind('\n') {
        return Some(i + 1);
    }
    head.rfind(' ').map(|i| i + 1)
}

// Hand-written because the parse state is a tree-sitter parser and tree, which have
// no `Debug` worth reading and would print a page of it per assertion failure.
impl std::fmt::Debug for IncrementalMarkdown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IncrementalMarkdown")
            .field("raw_len", &self.raw.len())
            .field("window_start", &self.window_start)
            .field("window_len", &self.window_len())
            .field("stable", &self.stable.len())
            .field("tail", &self.tail.len())
            .finish()
    }
}

// ---------------------------------------------------------------------------
// The text boundary, kept for the one shape the tree cannot settle
// ---------------------------------------------------------------------------

/// The offset past the last `\n\n` that is safe to freeze, if any.
///
/// This was the whole mechanism; it is now the escape hatch, reached only when the
/// window has grown past `max_unfrozen` with nothing settled — one long paragraph, or
/// one long *loose* list, which CommonMark keeps open across blank lines and which is
/// therefore a single top-level block.
///
/// The four guards, each of which is a case where cutting here changes the parse:
///
/// 1. **Strictly inside.** A boundary at the end freezes text that is still
///    growing; the next delta would then start a new block that should have joined
///    the last one.
/// 2. **Balanced fences.** A blank line inside a fenced code block is content, not
///    a boundary.
/// 3. **The next character starts real block content.** Leading whitespace after a
///    blank line is an indented continuation — of a list item, or an indented code
///    block — and joins backwards.
/// 4. **A preceding list must be *provably closed*.** CommonMark continues a list
///    across a blank line, making the whole thing one *loose* list; freezing in the
///    middle would render two tight ones. "Provably closed" is the operative
///    phrase and it looks **forward**: a list ends at a blank line followed by
///    something that cannot be one of its items.
///
/// Every guard is decided from the two lines either side of the candidate, which
/// is what keeps this O(window) per call rather than O(document).
///
/// `relax_at` is the escape hatch, and it is not a hedge. Guard 4 is *unfalsifiable
/// while the list is still open*, so a strict reader freezes nothing and is quadratic
/// again — on exactly the shape (a long bulleted answer) that made §13.3 worth
/// writing. Past `relax_at` unfrozen bytes the guard is dropped, and the cost of being
/// wrong is bounded and visible: two tight lists render where one loose list was, a
/// blank line's difference in a terminal.
pub fn stable_boundary(s: &str) -> Option<usize> {
    stable_boundary_with(s, usize::MAX)
}

pub fn stable_boundary_with(s: &str, relax_at: usize) -> Option<usize> {
    // One forward pass over the text, carrying the fence state, rather than
    // re-deciding "are the fences balanced here" per candidate. The per-candidate
    // form is O(n²) per push and it is quietly fatal: it turns the fix for the
    // quadratic renderer into a quadratic boundary finder.
    let mut best: Option<usize> = None;
    let mut fence_open = false;
    let mut pending_blank = false;
    let mut last_nonblank: Option<&str> = None;
    let mut off = 0usize;
    let relaxed = s.len() >= relax_at;

    while off < s.len() {
        let nl = s[off..].find('\n');
        let (line_end, next) = match nl {
            Some(i) => (off + i, off + i + 1),
            None => (s.len(), s.len()),
        };
        let complete = nl.is_some();
        let line = &s[off..line_end];
        let t = line.trim_start();

        if t.is_empty() {
            // Guard 2: a blank line inside a fence is content, not a boundary.
            if !fence_open {
                pending_blank = true;
            }
            off = next;
            continue;
        }

        if pending_blank && !fence_open && off > 0 {
            // Guard 1 is `off > 0` plus the fact that we are standing on real
            // content, so the boundary is strictly inside the text.
            // Guard 3: an indented line after a blank joins backwards.
            let indented = line.starts_with(' ') || line.starts_with('\t');
            let mut ok = !indented;
            // Guard 4.
            if ok
                && !relaxed
                && let Some(prev) = last_nonblank
                && let Some((was, _)) = list_item(prev.trim_start())
            {
                if !complete {
                    // The next line is still arriving; it cannot yet prove the
                    // list closed.
                    ok = false;
                } else if let Some((now, _)) = list_item(t)
                    && was == now
                {
                    ok = false;
                }
            }
            if ok {
                best = Some(off);
            }
        }

        pending_blank = false;
        if t.starts_with("```") || t.starts_with("~~~") {
            fence_open = !fence_open;
        }
        last_nonblank = Some(line);
        off = next;
    }
    best
}

/// One list item: the number it was written with (`None` for a bullet) and its
/// text. Used by guard 4 alone — the block pass reads items off the tree.
fn list_item(t: &str) -> Option<(Option<usize>, String)> {
    for m in ["- ", "* ", "+ "] {
        if let Some(rest) = t.strip_prefix(m) {
            return Some((None, rest.to_string()));
        }
    }
    let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
    if !digits.is_empty() && digits.len() <= 9 {
        let rest = &t[digits.len()..];
        if let Some(r) = rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") ")) {
            // A parse that cannot fail: at most nine ascii digits.
            return Some((digits.parse::<usize>().ok(), r.to_string()));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use letibot_sessionlog::testing::MARKDOWN;

    /// Feed a document in small pieces, the way deltas arrive.
    fn stream(doc: &str, chunk: usize) -> IncrementalMarkdown {
        let mut md = IncrementalMarkdown::new();
        let mut buf = String::new();
        for c in doc.chars() {
            buf.push(c);
            if buf.chars().count() >= chunk {
                md.push(&buf);
                buf.clear();
            }
        }
        if !buf.is_empty() {
            md.push(&buf);
        }
        md
    }

    fn blocks(md: &IncrementalMarkdown) -> Vec<Block> {
        md.blocks().cloned().collect()
    }

    /// The licence for the whole mechanism, as an assertion.
    ///
    /// A delta at a time and all at once must agree, block for block and run for
    /// run. This is the test that makes the window safe: it is the *only* thing
    /// proving that a block the tree called complete reads the same when the text
    /// before it is gone. When the engine was a hand-written lexer this property was
    /// argued from the four text guards; now it is argued from the parse, and it is
    /// the same assertion either way.
    #[test]
    fn streaming_gives_the_same_blocks_as_a_single_parse() {
        for chunk in [1, 2, 3, 7, 64, 4096] {
            let md = stream(MARKDOWN, chunk);
            assert_eq!(blocks(&md), lex(MARKDOWN), "chunk size {chunk}");
        }
    }

    /// The same property over a document with the shapes that settle differently:
    /// a table, a nested list, a fence with a blank line in it, a heading, a quote.
    #[test]
    fn streaming_agrees_with_one_parse_on_every_block_shape() {
        let doc = "\
# Title

Some **bold** and *italic* and `code` and ~~struck~~ text.

- one
- two
  - nested

> quoted
> twice

| a | b |
|---|--:|
| 1 | 2 |

```rust
fn main() {}

fn other() {}
```

---

tail
";
        for chunk in [1, 3, 11, 512] {
            let md = stream(doc, chunk);
            assert_eq!(blocks(&md), lex(doc), "chunk size {chunk}");
        }
    }

    /// **The invariant §2 of the plan is about, and the one that must not flake.**
    ///
    /// The window is what replaced tree-sitter's incremental re-parse, because that
    /// re-parse re-lexes the whole document for markdown (11× per-push growth over
    /// 1,000 pushes — `rano/TODO.md` §9). So the thing to assert is not *time*, which
    /// is what made rano's measurement flaky, but the **size of what we hand a
    /// parser**: the window must stay bounded as the document grows without bound,
    /// and the per-push parse count must not grow with it.
    #[test]
    fn the_window_stays_bounded_as_the_document_grows() {
        // Long enough that an unbounded window is unmistakable: 400 paragraphs.
        let doc: String = (0..400)
            .map(|i| format!("Paragraph {i} with a little text in it.\n\n"))
            .collect();
        let mut md = IncrementalMarkdown::new();
        // Per push: an eighth of a paragraph, so the deltas are realistic in size.
        for c in doc.as_bytes().chunks(8) {
            md.push(std::str::from_utf8(c).unwrap());
        }
        assert!(md.stable_count() > 300, "only {} settled", md.stable_count());
        assert!(
            md.window_len() <= DEFAULT_MAX_UNFROZEN,
            "the window grew to {} bytes on a {} byte document",
            md.window_len(),
            doc.len()
        );
        // And the bytes shown to a parser are linear in the document, not quadratic.
        // A full re-parse per delta lexes ~n²/2c bytes for n bytes in c-byte chunks.
        let n = doc.len() as u64;
        let naive = n * n / (2 * 8);
        assert!(
            md.bytes_lexed() < naive / 10,
            "lexed {} bytes; a full re-parse per delta would lex about {naive}. \
             That ratio is the whole of §13.3.",
            md.bytes_lexed()
        );
    }

    /// A long *loose* list is the shape the tree alone cannot settle: CommonMark
    /// keeps it open across blank lines, so it is one top-level block from the first
    /// item to the last. The cap has to cut it, and the cost is the documented one.
    ///
    /// Counted in **items**, not blocks: a loose list is one block however many items
    /// it has, so the block count says nothing about how much of the message has
    /// settled.
    #[test]
    fn a_message_that_is_one_long_list_is_linear_not_quadratic() {
        fn items_settled(md: &IncrementalMarkdown) -> usize {
            md.stable()
                .iter()
                .map(|b| match b {
                    Block::List { items, .. } => items.len(),
                    _ => 1,
                })
                .sum()
        }
        fn lexed(items: usize) -> u64 {
            let doc: String = (0..items).map(|i| format!("- item {i}\n\n")).collect();
            let mut md = IncrementalMarkdown::new();
            for c in doc.as_bytes().chunks(4) {
                md.push(std::str::from_utf8(c).unwrap());
            }
            assert!(
                items_settled(&md) > items / 2,
                "settled {} of {items} items",
                items_settled(&md)
            );
            assert!(md.window_len() <= DEFAULT_MAX_UNFROZEN, "{}", md.window_len());
            md.bytes_lexed()
        }
        let small = lexed(800);
        let large = lexed(1600);
        assert!(
            large < small * 3,
            "doubling the message multiplied the parsing by {:.1}; \
             quadratic is 4, linear is 2",
            large as f64 / small as f64
        );
    }

    /// A long single paragraph: one block forever, so only the cap settles it.
    #[test]
    fn one_endless_paragraph_is_bounded_by_the_cap() {
        let mut md = IncrementalMarkdown::new();
        for i in 0..2000 {
            md.push(&format!("word{i} "));
        }
        assert!(
            md.window_len() <= DEFAULT_MAX_UNFROZEN,
            "one paragraph grew the window to {}",
            md.window_len()
        );
        assert!(md.stable_count() >= 3, "{}", md.stable_count());
    }

    #[test]
    fn the_last_block_is_never_settled_while_it_can_still_grow() {
        let mut md = IncrementalMarkdown::new();
        md.push("one\n\n");
        // One paragraph, and it is the last: nothing settles, because the next delta
        // may continue it.
        assert_eq!(md.stable_count(), 0);
        md.push("two\n\n");
        // Now the first paragraph has a block after it, so it is settled.
        assert_eq!(md.stable_count(), 1);
        assert_eq!(md.tail().len(), 1);
    }

    // ---- the block model, read off the tree ----------------------------------

    #[test]
    fn a_heading_does_not_keep_its_hashes() {
        let b = lex("## Why the cache missed\n");
        assert_eq!(
            b,
            vec![Block::Heading {
                level: 2,
                runs: vec![run("Why the cache missed".into(), InlineStyle::Plain)],
            }]
        );
        // Setext headings come from the same arm.
        assert!(matches!(
            lex("Title\n=====\n").first(),
            Some(Block::Heading { level: 1, .. })
        ));
    }

    #[test]
    fn inline_markers_become_runs_and_do_not_reach_the_model() {
        let b = lex("plain **bold** and *italic* and `code` and ~~struck~~ end\n");
        let Block::Paragraph { lines } = &b[0] else { panic!("{b:#?}") };
        let styles: Vec<InlineStyle> = lines[0].iter().map(|r| r.style).collect();
        assert!(styles.contains(&InlineStyle::Bold), "{:?}", lines[0]);
        assert!(styles.contains(&InlineStyle::Italic), "{:?}", lines[0]);
        assert!(styles.contains(&InlineStyle::Code), "{:?}", lines[0]);
        assert!(styles.contains(&InlineStyle::Strikethrough), "{:?}", lines[0]);
        // The markers are gone from the text, and so is nothing else.
        assert_eq!(
            runs_text(&lines[0]),
            "plain bold and italic and code and struck end"
        );
        for r in &lines[0] {
            assert!(!r.text.contains('*'), "{:?}", r.text);
            assert!(!r.text.contains('`'), "{:?}", r.text);
        }
    }

    #[test]
    fn nested_emphasis_is_one_style_not_two_markers() {
        let b = lex("***both*** and **bold *and italic***\n");
        let Block::Paragraph { lines } = &b[0] else { panic!("{b:#?}") };
        let both = lines[0]
            .iter()
            .find(|r| r.text == "both")
            .expect("the run for `both`");
        assert_eq!(both.style, InlineStyle::BoldItalic);
        assert_eq!(runs_text(&lines[0]), "both and bold and italic");
    }

    /// A `*` the grammar did not call emphasis stays a `*`.
    ///
    /// This is the whole difference from the hand-written scanner: `2 * 3` used to be
    /// italicised as ` 3` because two asterisks on a line look like a pair.
    #[test]
    fn a_star_that_is_not_emphasis_stays_literal() {
        let b = lex("the product 2 * 3 and 4 * 5 is 120\n");
        let Block::Paragraph { lines } = &b[0] else { panic!("{b:#?}") };
        assert_eq!(runs_text(&lines[0]), "the product 2 * 3 and 4 * 5 is 120");
        assert!(lines[0].iter().all(|r| r.style == InlineStyle::Plain), "{:?}", lines[0]);
    }

    #[test]
    fn a_link_shows_its_text_and_not_its_destination() {
        let b = lex("see [the plan](docs/plan.md) and <https://example.invalid>\n");
        let Block::Paragraph { lines } = &b[0] else { panic!("{b:#?}") };
        assert_eq!(
            runs_text(&lines[0]),
            "see the plan and https://example.invalid"
        );
    }

    #[test]
    fn a_table_is_cells_and_alignment_from_the_tree() {
        let b = lex("| n | name | size |\n|--:|:----:|:-----|\n| 1 | a\\|b | wide |\n");
        let Block::Table { head, align, rows } = &b[0] else { panic!("{b:#?}") };
        let text = |c: &Vec<Run>| runs_text(c);
        assert_eq!(head.iter().map(text).collect::<Vec<_>>(), ["n", "name", "size"]);
        assert_eq!(align, &[Align::Right, Align::Center, Align::Left]);
        assert_eq!(rows[0].iter().map(text).collect::<Vec<_>>(), ["1", "a|b", "wide"]);
    }

    #[test]
    fn an_unterminated_fence_still_renders() {
        let md = stream("```rust\nlet a = 1;\n", 3);
        let all = blocks(&md);
        let Some(Block::Code { lang, closed, lines }) = all.last() else {
            panic!("{all:#?}");
        };
        assert_eq!(lang, "rust");
        assert!(!closed, "the fence is still open");
        assert_eq!(lines, &["let a = 1;"]);
        // A closed one says so, and does not carry the closing fence as content.
        let b = lex("```rust\nlet a = 1;\n```\n");
        let Some(Block::Code { closed, lines, .. }) = b.first() else { panic!("{b:#?}") };
        assert!(closed);
        assert_eq!(lines, &["let a = 1;"]);
    }

    /// A loose list keeps the numbers the model wrote, and arrives as **one** list.
    ///
    /// This is where the tree is better than the lexer it replaced. A looselist — items
    /// separated by blank lines, which is what a model writes as soon as an item runs
    /// past a sentence — used to arrive as one block per item, because a blank line
    /// ended a block. The renderer numbered from the index inside the block, so all six
    /// points of a real answer rendered `1.` while the paragraph above them called them
    /// "the six points below". The grammar knows it is one list, so it is one block and
    /// `start + i` numbers it.
    #[test]
    fn a_loose_ordered_list_keeps_the_numbers_it_was_written_with() {
        let b = lex("1. first\n\n2. second\n\n3. third\n\ntail\n");
        let Some(Block::List { ordered, start, items }) = b.first() else {
            panic!("{b:#?}");
        };
        assert!(ordered);
        assert_eq!(*start, 1);
        assert_eq!(
            items.iter().map(|i| runs_text(i)).collect::<Vec<_>>(),
            ["first", "second", "third"]
        );
        // And the renderer's numbering — `start + i` — is the model's own.
        let numbered: Vec<usize> = (0..items.len()).map(|i| start + i).collect();
        assert_eq!(numbered, vec![1, 2, 3]);
        // A list the model started at seven stays at seven.
        let b = lex("7. seven\n8. eight\n\ntail\n");
        assert!(matches!(b.first(), Some(Block::List { start: 7, .. })), "{b:#?}");
        // A bullet list has no written number and starts at one.
        let b = lex("- a\n- b\n");
        assert!(matches!(
            b.first(),
            Some(Block::List { ordered: false, start: 1, .. })
        ));
    }

    #[test]
    fn a_list_item_does_not_keep_its_marker() {
        let b = lex("- one\n- two\n");
        let Some(Block::List { items, .. }) = b.first() else { panic!("{b:#?}") };
        assert_eq!(items.iter().map(|i| runs_text(i)).collect::<Vec<_>>(), ["one", "two"]);
    }

    #[test]
    fn a_nested_list_reads_flat() {
        let b = lex("- one\n  - sub\n- two\n");
        let Some(Block::List { items, .. }) = b.first() else { panic!("{b:#?}") };
        assert_eq!(
            items.iter().map(|i| runs_text(i)).collect::<Vec<_>>(),
            ["one", "sub", "two"]
        );
    }

    #[test]
    fn a_quote_does_not_keep_its_markers() {
        let b = lex("> one **two**\n> three\n");
        let Block::Quote { lines } = &b[0] else { panic!("{b:#?}") };
        assert_eq!(lines.len(), 2);
        assert_eq!(runs_text(&lines[0]), "one two");
        assert_eq!(runs_text(&lines[1]), "three");
        assert!(lines[0].iter().any(|r| r.style == InlineStyle::Bold));
    }

    // ---- the text boundary, which is now only the escape hatch ---------------

    #[test]
    fn a_blank_line_inside_a_fence_is_not_a_boundary() {
        let doc = "```rust\nlet a = 1;\n\nlet b = 2;\n```\n\nafter\n";
        let b = stable_boundary(doc).unwrap();
        assert!(
            doc[..b].contains("```rust") && doc[..b].contains("let b"),
            "the boundary landed inside the fence: {:?}",
            &doc[..b]
        );
    }

    #[test]
    fn a_list_that_may_still_continue_is_not_frozen() {
        // CommonMark makes `- a\n\n- b` ONE loose list, so the blank between them
        // is not a boundary. The blank after `- b`, followed by a paragraph, is:
        // that is where the list is provably closed.
        let doc = "- a\n\n- b\n\ntext more\n";
        let b = stable_boundary(doc).unwrap();
        assert_eq!(&doc[..b], "- a\n\n- b\n\n");
        assert!(stable_boundary("- a\n\n- b\n").is_none());
    }

    #[test]
    fn an_indented_continuation_is_not_a_boundary() {
        let doc = "- a\n\n  still a\n\nnew\n";
        if let Some(b) = stable_boundary(doc) {
            assert!(
                !doc[b..].starts_with("  "),
                "froze before an indented continuation"
            );
        }
    }

    #[test]
    fn the_prefix_cut_is_one_the_whole_document_agrees_with() {
        // The window's cap uses this; the property it must have is the same one the
        // streamed form has, and it is checked against the whole document rather
        // than against the text guards' own reasoning.
        let doc = MARKDOWN;
        let mut cut = 0;
        let mut any = false;
        while let Some(b) = stable_boundary(&doc[cut..]) {
            let abs = cut + b;
            let mut joined = lex(&doc[..abs]);
            joined.extend(lex(&doc[abs..]));
            assert_eq!(joined, lex(doc), "split at byte {abs} changed the parse");
            any = true;
            cut = abs;
        }
        assert!(any, "the fixture must contain at least one stable boundary");
    }

    #[test]
    fn cut_is_at_least_the_boundary() {
        // The prefix a cut produces must be non-empty. The old form spliced into an
        // existing parse; this one re-parses the prefix, so a zero-length cut would
        // mean a settled block that is not a block.
        let mut md = IncrementalMarkdown::new();
        md.push("a paragraph long enough to matter\n\nand a second one\n\n");
        assert!(md.stable_count() >= 1);
        let first = md.stable()[0].title();
        assert_eq!(first, "a paragraph long enough to matter");
        assert!(!md.stable()[0].title().is_empty());
    }
}

#[cfg(test)]
mod nesting {
    use super::*;

    /// Assert that nothing the model wrote is missing from the projection.
    ///
    /// The failure this guards is silent: a block the projection cannot place is simply
    /// not there, and an empty quote renders as an empty quote — plausible, and a lie
    /// about what the model said. So the assertion is on the *words*, not on the shape.
    fn keeps(src: &str, words: &[&str]) {
        let blocks = lex(src);
        let mut got = String::new();
        fn text(blocks: &[Block], out: &mut String) {
            for b in blocks {
                match b {
                    Block::Code { lines, .. } => {
                        for l in lines {
                            out.push_str(l);
                            out.push(' ');
                        }
                    }
                    _ => {
                        out.push_str(&b.title());
                        out.push(' ');
                    }
                }
                if let Block::List { items, .. } = b {
                    for i in items {
                        out.push_str(&runs_text(i));
                        out.push(' ');
                    }
                }
                if let Block::Quote { lines } = b {
                    for l in lines {
                        out.push_str(&runs_text(l));
                        out.push(' ');
                    }
                }
                if let Block::Paragraph { lines } = b {
                    for l in lines {
                        out.push_str(&runs_text(l));
                        out.push(' ');
                    }
                }
            }
        }
        text(&blocks, &mut got);
        for w in words {
            assert!(got.contains(w), "{w:?} was dropped: {got:?} from {blocks:#?}");
        }
    }

    /// A fenced block inside a block quote. It rendered as an *empty* quote.
    ///
    /// `Block::Quote` holds run lines and `content_ranges` looks for `inline` nodes —
    /// and a quote holding only a fence has none, so every byte of the model's code
    /// went on the floor. The projection now walks the subtree for whatever a reader
    /// would see rather than asking for inline content.
    #[test]
    fn a_fence_inside_a_quote_is_not_dropped() {
        keeps("> ```rust\n> let a = 1;\n> ```\n", &["let a = 1;"]);
        let b = lex("> ```rust\n> let a = 1;\n> ```\n");
        let Block::Quote { lines } = &b[0] else { panic!("{b:#?}") };
        assert_eq!(lines.len(), 1, "{lines:#?}");
        assert_eq!(runs_text(&lines[0]), "let a = 1;");
        // The continuation `> ` is a child of the *content* node, so it must not
        // reach the code's last line either.
        assert!(!runs_text(&lines[0]).contains('>'), "{lines:#?}");
    }

    /// A fenced block inside a list item. Its markers became item text.
    #[test]
    fn a_fence_inside_a_list_item_is_not_markers() {
        let src = "- item\n\n  ```rust\n  let a = 1;\n  ```\n";
        keeps(src, &["item", "let a = 1;"]);
        let b = lex(src);
        let Block::List { items, .. } = &b[0] else { panic!("{b:#?}") };
        let item = runs_text(&items[0]);
        assert!(!item.contains("```"), "the fence markers reached the item: {item:?}");
        assert!(!item.contains("rust"), "the info string reached the item: {item:?}");
        assert_eq!(item.split_whitespace().collect::<Vec<_>>().join(" "), "item let a = 1;");
    }

    /// A table inside a quote or an item: rows survive as lines, columns do not.
    #[test]
    fn a_table_inside_a_quote_keeps_its_cells() {
        let src = "> | a | b |\n> |---|---|\n> | 1 | 2 |\n";
        keeps(src, &["a b", "1 2"]);
        let b = lex(src);
        let Block::Quote { lines } = &b[0] else { panic!("{b:#?}") };
        let texts: Vec<String> = lines.iter().map(|l| runs_text(l)).collect();
        assert!(texts.iter().any(|t| t.contains("1 2")), "{texts:?}");
        assert!(!texts.iter().any(|t| t.contains('|')), "{texts:?}");
    }

    /// A non-markdown fence language is still a code block; the renderer falls back to
    /// plain for one `StreamingCode` does not know, which is not this module's business.
    #[test]
    fn a_fence_in_an_unknown_language_is_still_code() {
        let b = lex("```brainfuck\n+++\n```\n");
        let Some(Block::Code { lang, lines, closed }) = b.first() else { panic!("{b:#?}") };
        assert_eq!(lang, "brainfuck");
        assert_eq!(lines, &["+++"]);
        assert!(closed);
    }

    /// A quote with prose and a fence keeps both, in order.
    #[test]
    fn a_quote_with_prose_and_a_fence_keeps_both() {
        let src = "> There is `redacted` here:\n>\n> ```rust\n> let a = 1;\n> ```\n";
        keeps(src, &["There is", "redacted", "let a = 1;"]);
        let b = lex(src);
        let Block::Quote { lines } = &b[0] else { panic!("{b:#?}") };
        let texts: Vec<String> = lines.iter().map(|l| runs_text(l)).collect();
        assert_eq!(texts[0], "There is redacted here:");
        assert!(texts.iter().any(|t| t == "let a = 1;"), "{texts:?}");
        assert!(
            texts[0] != "let a = 1;" && texts.iter().filter(|t| *t == "let a = 1;").count() == 1,
            "{texts:?}"
        );
    }
}
