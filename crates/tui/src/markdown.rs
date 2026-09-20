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

    /// Bring the parse up to date with `raw`, then settle what has completed.
    ///
    /// Two signals, and both are needed.
    ///
    /// The **text guards** ([`cut_point`]) say which `\n\n` boundaries are safe, because
    /// the tree alone is not enough: `1. a\n\n2` parses as a list and a paragraph and then
    /// becomes one loose list. And the **fences** say where a cut is not allowed at all,
    /// because the guards are not enough either: they count fences by lines that *start*
    /// with ```, and a fence may contain such a line. See [`fences_in`] — that is how the
    /// operator's code block came apart.
    ///
    /// Settling **re-parses the prefix on its own** rather than taking the leading blocks
    /// out of the window's parse. It is one extra parse of a window bounded by the cap, and
    /// it buys the property outright: the guard says
    /// `parse(win[..cut]) ++ parse(win[cut..]) == parse(win)`, so the prefix's blocks are
    /// the right blocks by construction.
    fn refresh(&mut self) {
        for _ in 0..MAX_SETTLE_ROUNDS {
            let win = self.raw[self.window_start..].to_string();
            let (blocks, fences) = self.project();
            let Some(cut) = cut_point(&win, self.max_unfrozen, &fences) else {
                self.tail = blocks;
                return;
            };
            let prefix = parse_standalone(&win[..cut], &mut self.bytes_lexed, &mut self.lex_calls);
            self.stable.extend(prefix);
            self.window_start += cut;
            // Round again: the window is now smaller, and there is usually nothing more to
            // do because what is left is one trailing block.
        }
        // Unreachable for any real input: each round moves `window_start` forward.
        // Rendering the window as-is is the safe thing to do with a parse that somehow
        // kept producing work.
        self.tail = self.project().0;
    }

    /// The window's blocks, and the byte ranges a cut may not land inside.
    fn project(&mut self) -> (Vec<Block>, Vec<(usize, usize)>) {
        let src = self.raw[self.window_start..].to_string();
        let fences = fences_in(&src);
        let (blocks, spans) = parse_blocks(&src, &fences, &mut self.bytes_lexed, &mut self.lex_calls);
        let _ = spans;
        (blocks, fence_spans(&src))
    }
}

/// The blocks of `src`, given the fences already found in it.
fn parse_blocks(
    src: &str,
    fences: &[Fence],
    bytes_lexed: &mut u64,
    lex_calls: &mut u64,
) -> (Vec<Block>, Vec<Span>) {
    let text = mask(src, fences);
    *bytes_lexed += text.len() as u64;
    *lex_calls += 1;
    let mut stream = Stream::new(Lang::Markdown);
    stream.push(&text);
    let Some(root) = stream.root() else {
        return (Vec::new(), Vec::new());
    };
    let spans = inline_spans(src, &root, bytes_lexed, lex_calls);
    (blocks_of(&root, src, &spans, fences), spans)
}

/// Parse a standalone span of markdown into blocks, both passes.
///
/// Used for a window prefix cut at a text boundary, and by [`lex`].
fn parse_standalone(src: &str, bytes_lexed: &mut u64, lex_calls: &mut u64) -> Vec<Block> {
    let fences = fences_in(src);
    parse_blocks(src, &fences, bytes_lexed, lex_calls).0
}

/// `src` with every fence blanked out, byte for byte.
///
/// `tree-sitter-md` gets the *extent* of a fence wrong when its closing delimiter is not
/// alone on its line (see [`fences_in`]), and the damage is not local: a block closed
/// early leaves the parser in the wrong state, so the blocks after it are wrong too and
/// there is no repairing them one at a time. The way out is to keep fences away from the
/// grammar altogether. Each fence's bytes become spaces, newlines kept, which leaves the
/// block structure *around* them exactly as it was — a fence was a separator, and blank
/// lines are the same separator — and costs nothing in offsets, because the length is
/// unchanged and every newline is where it was. So a node the grammar reports at byte `n`
/// corresponds to byte `n` of the real text.
///
/// Replacing bytes of a multi-byte character with spaces is safe: the result is ASCII
/// there, and no newline moved.
fn mask(src: &str, fences: &[Fence]) -> String {
    if fences.is_empty() {
        return src.to_string();
    }
    let mut bytes = src.as_bytes().to_vec();
    for f in fences {
        let end = f.end.min(bytes.len());
        for b in &mut bytes[f.open.min(end)..end] {
            if *b != b'\n' {
                *b = b' ';
            }
        }
    }
    String::from_utf8(bytes).unwrap_or_else(|_| src.to_string())
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
/// **One parse per range**, each over that range's text alone. That is not a
/// preference — it is what correctness costs. The obvious shape, one parse with
/// `set_included_ranges` over all the ranges, is *wrong*: tree-sitter concatenates
/// included ranges in the byte stream, so a delimiter in one range pairs with a
/// delimiter in another. Shipped that way (2026-09-20) and the operator's screen showed
/// the result — a paragraph's ``` opening a code span that closed 3,000 bytes later,
/// swallowing ten blocks of the message. The grammar's own reference implementation
/// (`tree-sitter-md`'s `MarkdownParser`) parses one inline node at a time for this
/// reason.
///
/// A range with nothing in it that can start a construct is skipped without parsing,
/// which is most paragraphs.
fn inline_spans(src: &str, root: &Node, bytes_lexed: &mut u64, lex_calls: &mut u64) -> Vec<Span> {
    let mut spans = Vec::new();
    for (a, b) in inline_ranges(root) {
        let text = &src[a..b];
        if !has_inline_syntax(text) {
            continue;
        }
        let mut stream = Stream::new(Lang::MarkdownInline);
        stream.push(text);
        *bytes_lexed += text.len() as u64;
        *lex_calls += 1;
        let Some(root) = stream.root() else {
            continue;
        };
        let mut local = Vec::new();
        collect_spans(&root, text, InlineStyle::Plain, &mut local);
        // Offsets are relative to the slice; the rest of this module works in offsets
        // into the whole document.
        for mut s in local {
            s.start += a;
            s.end += a;
            spans.push(s);
        }
    }
    spans
}

/// Whether `text` holds anything the inline grammar could read as a construct.
///
/// The converse is the useful direction: if it holds none of these, the grammar can only
/// find plain text, so the range can be left alone. Escapes (`\`) and autolinks (`<`) and
/// images (`!`) are in the set because they *are* constructs; `&` is not, because an
/// entity reference is shown as written either way and a parse would change nothing.
///
/// The one case this lets through is a hard line break — two trailing spaces — which a
/// parse would turn into a break and a skip leaves as trailing whitespace on a rendered
/// line. Invisible, and not worth a parse per paragraph.
fn has_inline_syntax(text: &str) -> bool {
    text.bytes()
        .any(|b| matches!(b, b'`' | b'*' | b'_' | b'~' | b'[' | b'<' | b'\\' | b'!'))
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

/// A fenced code block found in the text.
#[derive(Debug, Clone)]
struct Fence {
    /// Offset of the opening line's start — the whole line is the fence's, prefix and all.
    open: usize,
    /// The container syntax the opening line carries — `"> "` in a quote, an item's indent
    /// in a list, `""` at the top level. Every line of the block repeats it.
    prefix: String,
    /// Offset of the body: just after the opening line's newline.
    body: usize,
    /// Offset of the closing line's start, or the end of the text when there is none.
    close: usize,
    /// Offset just after the closing line.
    end: usize,
    /// The info string, trimmed.
    lang: String,
    /// Whether a closing fence was found.
    closed: bool,
}

/// Every fenced code block in `src`, in order.
///
/// **This is not read off the tree, and that is the point.** `tree-sitter-md` accepts a
/// run of backticks anywhere on a line as a closing fence: for `"abc ```"` the delimiter
/// node is `" ```"` at bytes 7..11, so the block closes there and the rest of the code is
/// parsed as prose. CommonMark requires the closing fence to be a line of its own, and
/// the grammar's README says it is built for syntax highlighting rather than correctness —
/// so the extent of a fence is computed here, from the text, where the rule is four lines
/// long.
///
/// Found on the operator's screen (2026-09-20, *"awful"*): a message that drew box art
/// containing ``` was cut in half at an art line ending in backticks, the remainder of
/// the code became a paragraph, and the message's own prose ended up inside an unclosed
/// code box.
fn fences_in(src: &str) -> Vec<Fence> {
    let mut out = Vec::new();
    let mut off = 0;
    while off < src.len() {
        let line_end = match src[off..].find('\n') {
            Some(i) => off + i,
            None => src.len(),
        };
        let (prefix, marker) = split_container(&src[off..line_end]);
        if let Some(&ch @ (b'`' | b'~')) = marker.as_bytes().first() {
            let n = marker.bytes().take_while(|b| *b == ch).count();
            if n >= 3 {
                let lang = marker[n..].trim().to_string();
                let body = (line_end + 1).min(src.len());
                let (close, end, closed) = match closing_fence(src, body, ch, n) {
                    Some((start, after)) => (start, after, true),
                    None => (src.len(), src.len(), false),
                };
                out.push(Fence {
                    open: off,
                    prefix: prefix.to_string(),
                    body,
                    close,
                    end,
                    lang,
                    closed,
                });
                off = end.max(line_end + 1);
                continue;
            }
        }
        off = line_end + 1;
    }
    out
}

/// Split a line into the container syntax it carries and what follows: `("> ", "```rust")`
/// for a fence inside a quote, `("  ", "```")` for an indented one, `("", "text")` for a
/// plain line.
///
/// The prefix is what a continuation line repeats, so it is what [`closing_fence`] has to
/// strip to recognise a closing delimiter, and what [`fence_lines`] has to strip to get at
/// the code.
///
/// Split a line into the container syntax it carries and what follows: `("> ", "```rust")`
/// for a fence inside a quote, `("  ", "```rust")` for one after a list marker, `("", "text")`
/// for a plain line.
///
/// The prefix is the **content column** — what every line of the block has in common — which
/// is why a list marker comes back as blanks of its own width rather than as itself. A quote
/// marker repeats on every line (`"> ```rust"`, `"> let a = 1;"`), and so does indentation,
/// but a list marker does not: `"- ```rust"` is continued by `"  let a = 1;"`. Substituting
/// blanks gives one prefix that strips both, and it is what makes a fence in a list item
/// detectable at all — a scan that only skipped whitespace and `>` missed every one of them,
/// which is how `- ```rust` came out as an item holding the literal markers.
fn split_container(line: &str) -> (String, &str) {
    let mut prefix = String::new();
    let mut cut = 0;
    loop {
        let rest = &line[cut..];
        let ws = rest.len() - rest.trim_start_matches([' ', '\t']).len();
        prefix.push_str(&rest[..ws]);
        cut += ws;
        if line[cut..].starts_with('>') {
            prefix.push('>');
            cut += 1;
            continue;
        }
        if let Some(len) = list_marker_len(&line[cut..]) {
            prefix.push_str(&" ".repeat(len));
            cut += len;
            continue;
        }
        break;
    }
    (prefix, &line[cut..])
}

/// The byte length of the list marker at the start of `s` — `"- "`, `"1. "` — or `None`.
///
/// The trailing space is part of it, because that is what separates the marker from the
/// item's content and so what decides the content column.
fn list_marker_len(s: &str) -> Option<usize> {
    for m in ["- ", "* ", "+ "] {
        if s.starts_with(m) {
            return Some(m.len());
        }
    }
    let digits = s.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digits > 0 && digits <= 9 {
        let rest = &s[digits..];
        if rest.starts_with(". ") || rest.starts_with(") ") {
            return Some(digits + 2);
        }
    }
    None
}

/// The closing fence of a block opened with `n` of `ch`: `(its line's start, offset just
/// after it)`.
///
/// Each candidate line has its own container syntax stripped before the test, which is what
/// recognises `"> ```"` in a quote and `"  ```"` in a list item. Testing the *opening* line's
/// prefix against the continuation instead would be wrong: a list marker does not repeat, so
/// `"- ```rust"` and its closing `"  ```"` have different prefixes and only the second is the
/// one a closing fence is written with.
fn closing_fence(src: &str, from: usize, ch: u8, n: usize) -> Option<(usize, usize)> {
    let mut off = from;
    while off < src.len() {
        let line_end = match src[off..].find('\n') {
            Some(i) => off + i,
            None => src.len(),
        };
        let (_, marker) = split_container(&src[off..line_end]);
        let t = marker.trim();
        if t.len() >= n && t.bytes().all(|b| b == ch) {
            return Some((off, (line_end + 1).min(src.len())));
        }
        off = line_end + 1;
    }
    None
}

/// A fence's code lines, with the opening line's own prefix stripped from each line.
///
/// The prefix is the *block's* — `"> "` for a quoted fence, an item's indent for one in a
/// list — not whatever each line happens to start with. They are the same in every
/// well-formed case, and a code line that legitimately begins with `"> "` inside a
/// top-level fence must keep it.
fn fence_lines(src: &str, f: &Fence) -> Vec<String> {
    let body = &src[f.body..f.close];
    let mut lines: Vec<String> = body
        .split('\n')
        .map(|l| {
            // The content column, or — for a line that does not carry it, which a list
            // item's continuation need not — its own leading whitespace.
            match l.strip_prefix(f.prefix.as_str()) {
                Some(rest) => rest.to_string(),
                None => l.trim_start().to_string(),
            }
        })
        .collect();
    // `"a\n"` splits to `["a", ""]`; the empty tail is the newline, not a line.
    if lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines
}

/// The byte ranges a cut may not land inside: one per fence, from its opening delimiter to
/// its closing one — or to the end of the window when it has none.
///
/// **Both ends are allowed and the middle is not.** A cut at `start` leaves the fence
/// entirely in the window, which is right; a cut at `end` leaves it entirely in the
/// prefix, which is also right, because a prefix that ends with a complete fence parses
/// as that fence. Anywhere strictly between them splits one code block in two, and the
/// half that keeps neither delimiter is not code at all — it parses as a paragraph. That
/// is not a spelling mistake a reader shrugs at; it is the model's code shown as prose.
///
/// An unclosed fence runs to the end of the input, so its range reaches the end of the
/// window: nothing at or after it may settle. That is what keeps a settle out of an open
/// code block, and it is why these come from [`fences_in`] rather than from the tree —
/// the guards and the block model have to agree about where the fences are, and only one
/// of them can be right.
fn fence_spans(src: &str) -> Vec<(usize, usize)> {
    fences_in(src).iter().map(|f| (f.open, f.end)).collect()
}

/// Is `at` inside a fence rather than at one of its ends?
fn inside_a_fence(fences: &[(usize, usize)], at: usize) -> bool {
    fences.iter().any(|&(s, e)| at > s && at < e)
}

/// Map the parsed tree's block children to blocks, with the fences in their place.
///
/// The block grammar wraps the document in containers — `document`, and a `section` per
/// heading — so the blocks are the leaves of that chain, not `root`'s direct children.
/// Walking through them is what makes the flat block list the renderer wants, and it is
/// why a heading's section does not become one giant block.
///
/// The fences are **not** in that tree at all: [`mask`] blanked them out before the parse,
/// because the grammar cannot be trusted with a closing delimiter that is not alone on its
/// line. So the two lists are merged by offset, which is what keeps a message's blocks in
/// the order the model wrote them.
fn blocks_of(root: &Node, src: &str, spans: &[Span], fences: &[Fence]) -> Vec<Block> {
    let mut tree: Vec<(usize, Block)> = Vec::new();
    collect_blocks(root, src, spans, fences, &mut tree);
    let mut out = Vec::new();
    let mut it = tree.into_iter().peekable();
    for f in fences {
        while it.peek().is_some_and(|(start, _)| *start < f.open) {
            out.push(it.next().unwrap().1);
        }
        out.push(Block::Code {
            lang: f.lang.clone(),
            lines: fence_lines(src, f),
            closed: f.closed,
        });
    }
    out.extend(it.map(|(_, b)| b));
    out
}

/// Containers the block grammar wraps blocks in. They carry no text of their own.
fn is_container(kind: &str) -> bool {
    matches!(kind, "document" | "section")
}

fn collect_blocks(
    node: &Node,
    src: &str,
    spans: &[Span],
    fences: &[Fence],
    blocks: &mut Vec<(usize, Block)>,
) {
    for c in &node.children {
        if !c.named {
            continue;
        }
        if is_container(&c.kind) {
            collect_blocks(c, src, spans, fences, blocks);
            continue;
        }
        // A block reported *inside* a fence is an artefact of the grammar having glued
        // itself back together around one; the fence is emitted from the scan.
        if fences.iter().any(|f| c.start >= f.open && c.start < f.end) {
            continue;
        }
        if let Some(block) = block_of(c, src, spans, fences) {
            blocks.push((c.start, block));
        }
    }
}


fn block_of(node: &Node, src: &str, spans: &[Span], fences: &[Fence]) -> Option<Block> {
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
            subtree_lines(node, src, spans, fences, &mut lines);
            if lines.is_empty() {
                lines.push(Vec::new());
            }
            Some(Block::Quote { lines })
        }
        // Only an indented block can reach here: a *fenced* one was masked away before the
        // parse, and `collect_blocks` emits it from the scan.
        "indented_code_block" => Some(code_block(node, src)),
        "list" => Some(list_block(node, src, spans, fences)),
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
fn subtree_lines(node: &Node, src: &str, spans: &[Span], fences: &[Fence], out: &mut Vec<Vec<Run>>) {
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
            // No arm for `fenced_code_block` or `code_fence_content`: after [`mask`] the
            // tree holds neither, because every fence's bytes are spaces by the time the
            // parser sees them (asserted by `masking_leaves_no_fence_in_the_tree`). A
            // fence that belongs to a container is emitted from the scan and the container
            // is dropped beside it, so a container that reaches here holds prose only.
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
            _ => subtree_lines(c, src, spans, fences, out),
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
fn item_runs(body: &[Node], src: &str, spans: &[Span], fences: &[Fence]) -> Vec<Run> {
    let mut lines = Vec::new();
    for n in body {
        subtree_lines_one(n, src, spans, fences, &mut lines);
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
fn subtree_lines_one(node: &Node, src: &str, spans: &[Span], fences: &[Fence], out: &mut Vec<Vec<Run>>) {
    let before = out.len();
    subtree_lines(node, src, spans, fences, out);
    // A node with no children of its own contributes its whole text — the case a
    // detached leaf (a `paragraph` stripped of its `inline`) falls into.
    if out.len() == before {
        let text = text_without_continuations(node, src);
        for line in text.trim_end_matches('\n').lines() {
            out.push(vec![run(line.to_string(), InlineStyle::Plain)]);
        }
    }
}

/// A code block from the tree, used when its start is not a fence [`fences_in`] found.
///
/// The text is authoritative for every fence that [`fences_in`] sees, so reaching here
/// means the node starts somewhere other than a fence's opening delimiter — an indented
/// code block, or a fence whose opening line the grammar placed differently. Reading it
/// off the tree is the fallback rather than the rule.
/// An indented code block: four spaces and no fence, so there are no markers to strip and
/// no language to name.
///
/// It is the only kind that reaches the tree at all. A *fenced* block is masked away before
/// the parse and emitted by `collect_blocks` from the scan, so this is not a fallback for
/// one — a fence never has to be read off a tree that gets its extent wrong.
fn code_block(node: &Node, src: &str) -> Block {
    let mut lines: Vec<String> = text_without_continuations(node, src)
        .trim_end_matches('\n')
        .lines()
        .map(str::to_string)
        .collect();
    // An indented block has no delimiters, and CommonMark runs it to the end of the input
    // like a fence does at EOF — so it is never "still being written".
    if lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    Block::Code {
        lang: String::new(),
        lines,
        closed: true,
    }
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

fn list_block(node: &Node, src: &str, spans: &[Span], fences: &[Fence]) -> Block {
    let mut items = Vec::new();
    let mut ordered = false;
    let mut start = 1;
    let mut first = true;
    collect_items(node, src, spans, fences, &mut items, &mut ordered, &mut start, &mut first);
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
    fences: &[Fence],
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
                let item = item_runs(&rest, src, spans, fences);
                if !item.is_empty() {
                    items.push(item);
                }
                for g in &c.children {
                    if g.kind == "list" {
                        collect_items(g, src, spans, fences, items, ordered, start, first);
                    }
                }
            }
            "list" => collect_items(c, src, spans, fences, items, ordered, start, first),
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

/// Where to settle an over-long window. `None` means nothing may settle yet.
///
/// [`stable_boundary_with`] first: a blank line the guards agree about, which is where two
/// parses of the halves agree with one parse of the whole. A candidate **inside a fence**
/// is refused whatever the guards say — see [`fence_spans`] for why, and for the bug that
/// made this necessary.
///
/// While the window is under the cap a refusal means nothing settles, which is the strict
/// and correct behaviour and is why a window can sit at a few kilobytes for a while.
///
/// Past the cap the window would otherwise grow without bound and take the per-push cost
/// with it, so the guards are relaxed: `stable_boundary_with` gets the cap as its
/// `relax_at`, and failing a blank line at all — one paragraph with no blank line anywhere
/// in it — the last line break, or failing that the last space. Never inside a fence: a
/// window that is one long code block has no legal cut in it at all, and it stays whole.
/// The cost is linear per push in the block, which is what it has to be, because you
/// cannot stream a block whose end you cannot see.
///
/// The relaxed cuts are *wrong* in the way §13.3's notes say a bounded wrongness is
/// acceptable: two halves render as two paragraphs where the model wrote one. The
/// alternative is quadratic on exactly the shapes that have no other boundary.
fn cut_point(win: &str, max: usize, fences: &[(usize, usize)]) -> Option<usize> {
    if let Some(b) = stable_boundary_with(win, max)
        && !inside_a_fence(fences, b)
    {
        return Some(b);
    }
    if win.len() < max {
        return None;
    }
    let head = &win[..max];
    for (i, _) in head.match_indices('\n').rev() {
        if !inside_a_fence(fences, i + 1) {
            return Some(i + 1);
        }
    }
    for (i, _) in head.match_indices(' ').rev() {
        if !inside_a_fence(fences, i + 1) {
            return Some(i + 1);
        }
    }
    None
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
                && let Some(was) = list_kind(prev.trim_start())
            {
                if !complete {
                    // The next line is still arriving; it cannot yet prove the
                    // list closed.
                    ok = false;
                } else if let Some(now) = list_kind(t)
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

/// The kind of list item this line opens: `Some(true)` for ordered, `Some(false)` for a
/// bullet, `None` if the line is not one. Used by guard 4 alone — the block pass reads
/// items off the tree.
///
/// **The kind, not the number.** Guard 4 asks "can this be another item of the list
/// above?", and CommonMark's answer is about the marker's *kind*: `1. a\n\n2. b` is one
/// loose list, and so is `- a\n\n- b`, while `- a\n\n1. b` is a bullet list and an
/// ordered one. Comparing the written numbers said `1.` and `2.` were different items
/// and cut between them, which made a settle land one block too early.
///
/// A marker still being typed counts as one: a bare `2.` is a list item whose content
/// has not arrived, and reading it as "not a list" is what let the cut happen between
/// the marker and its text.
fn list_kind(t: &str) -> Option<bool> {
    let t = t.trim_start();
    if t == "-" || t == "*" || t == "+" || t.starts_with("- ") || t.starts_with("* ")
        || t.starts_with("+ ")
    {
        return Some(false);
    }
    let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
    if !digits.is_empty() && digits.len() <= 9 {
        let rest = &t[digits.len()..];
        if rest.starts_with('.') || rest.starts_with(')') {
            return Some(true);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use letibot_sessionlog::testing::MARKDOWN;

    /// Feed a document in small pieces, the way deltas arrive.
    pub(super) fn stream(doc: &str, chunk: usize) -> IncrementalMarkdown {
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

    pub(super) fn blocks(md: &IncrementalMarkdown) -> Vec<Block> {
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

    /// A fenced block inside a block quote is a **code box of its own**.
    ///
    /// It used to be dropped outright — `Block::Quote` holds run lines and the quote had
    /// no inline content, so every byte of the model's code went on the floor as an empty
    /// quote. Then it was quote prose. Now the fence scan finds it (a quoted fence is
    /// written `"> ```rust"`, which a scan that only skipped whitespace would miss), the
    /// masked quote has nothing left in it and is dropped, and the code is a code box with
    /// the markers consumed and the `"> "` stripped from each line.
    #[test]
    fn a_fence_inside_a_quote_is_not_dropped() {
        let src = "> ```rust\n> let a = 1;\n> ```\n";
        keeps(src, &["let a = 1;"]);
        let b = lex(src);
        let Some(Block::Code { lang, lines, closed }) = b.first() else { panic!("{b:#?}") };
        assert_eq!(lang, "rust");
        assert_eq!(lines, &["let a = 1;"]);
        assert!(*closed);
        assert!(!lines.iter().any(|l| l.contains('>')), "{lines:?}");
        // No empty quote left behind.
        assert_eq!(b.len(), 1, "{b:#?}");
    }

    /// A fenced block inside a list item is a code box beside the item, not item text.
    ///
    /// The item keeps the prose that was written in it, and the code keeps its own lines,
    /// its language and its markers-consumed body — rather than one line of item text with
    /// ``` and "rust" in the middle of it.
    #[test]
    fn a_fence_inside_a_list_item_is_not_markers() {
        let src = "- item\n\n  ```rust\n  let a = 1;\n  ```\n";
        keeps(src, &["item", "let a = 1;"]);
        let b = lex(src);
        let Some(Block::List { items, .. }) = b.first() else { panic!("{b:#?}") };
        let item = runs_text(&items[0]);
        assert_eq!(item, "item", "the item is the item's own prose");
        let Some(Block::Code { lang, lines, .. }) = b.get(1) else { panic!("{b:#?}") };
        assert_eq!(lang, "rust");
        assert_eq!(lines, &["let a = 1;"], "the item's indent is not part of the code");
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

    /// A fence closes at the *next* fence, even when the author meant that one to be
    /// nested — so the text after it is not code, and a later fence is still open.
    ///
    /// This is CommonMark's rule (a fence of N backticks is closed by a line of M ≥ N
    /// backticks and nothing else), and it is why a message that quotes a fence inside a
    /// fence renders as one long open code block with `(still writing…)` at the bottom:
    /// the parser is right and the message was not able to say what it meant. Written up
    /// because it looked like a renderer bug on the operator's screen (2026-09-20) when
    /// it is a writer's mistake, and the fix is to open with four backticks.
    #[test]
    fn a_fence_cannot_quote_itself() {
        let b = lex("```\nalpha\n```\nbeta\n```\n");
        let kinds: Vec<&str> = b
            .iter()
            .map(|x| match x {
                Block::Code { .. } => "code",
                _ => "text",
            })
            .collect();
        assert_eq!(kinds, ["code", "text", "code"], "{b:#?}");
        // The first closed at the inner fence, so `beta` is prose…
        let Block::Code { lines, closed, .. } = &b[0] else { panic!("{b:#?}") };
        assert_eq!(lines, &["alpha"]);
        assert!(*closed);
        // …and the trailing fence is the one still open, which is what the footer says.
        let Some(Block::Code { closed, lines, .. }) = b.last() else { panic!("{b:#?}") };
        assert!(!*closed);
        assert!(lines.is_empty());
        // Four backticks is how to quote three: then the inner fence is content.
        let b = lex("````\nalpha\n```\n````\n");
        assert_eq!(b.len(), 1, "{b:#?}");
        let Some(Block::Code { lines, closed, .. }) = b.first() else { panic!("{b:#?}") };
        assert_eq!(lines, &["alpha", "```"]);
        assert!(closed);
    }

    /// A quote with prose and a fence keeps both, in order.
    #[test]
    fn a_quote_with_prose_and_a_fence_keeps_both() {
        let src = "> There is `redacted` here:\n>\n> ```rust\n> let a = 1;\n> ```\n";
        keeps(src, &["There is", "redacted", "let a = 1;"]);
        let b = lex(src);
        // The quote keeps its prose ...
        let Some(Block::Quote { lines }) = b.first() else { panic!("{b:#?}") };
        let texts: Vec<String> = lines.iter().map(|l| runs_text(l)).collect();
        assert_eq!(texts, &["There is redacted here:"], "{texts:?}");
        // ... and the code is the next block, as code.
        let Some(Block::Code { lang, lines, .. }) = b.get(1) else { panic!("{b:#?}") };
        assert_eq!(lang, "rust");
        assert_eq!(lines, &["let a = 1;"]);
        assert_eq!(b.len(), 2, "{b:#?}");
    }
}

#[cfg(test)]
mod streaming_matches_one_parse {
    use super::*;
    use crate::markdown::tests::{blocks, stream};

    /// Feed a document a byte at a time and assert the model equals a single parse.
    ///
    /// One byte is the cruellest chunk size and the only one worth a corpus this size: it
    /// puts every partial line, every half-written marker and every mid-token window
    /// under the guards. It is also cheap — the documents are a few hundred bytes — and it
    /// is what caught all four divergences below.
    fn agrees(doc: &str) {
        let whole = lex(doc);
        let strm = stream(doc, 1).blocks().cloned().collect::<Vec<_>>();
        assert_eq!(strm, whole, "{doc:?}");
    }

    /// **A fence whose content contains a fence.** From the operator's screen: the
    /// rendered message ended in an empty closed code box with the text after it spilled
    /// out as prose (2026-09-20, "code blocks still broken").
    ///
    /// The guards count fences by lines that start with ```, and the quoted line *is* such
    /// a line, so the count inverted from there on. The `\n\n` after the (really open)
    /// fence was taken for a boundary, the prefix settled, and a prefix that ends really
    /// does end at an end of input — so the open fence became a *closed* one, empty, and
    /// everything after the cut was parsed fresh as prose. `open_fence_start` is the fix:
    /// the tree says the fence has fewer than two delimiters, and nothing at or after it
    /// settles.
    #[test]
    fn a_settle_never_lands_inside_an_open_fence() {
        agrees("prose\n\n```\n``` inner\ncode\n```\n\ntail\n```\n\nreal tail\n");
        agrees("```\nalpha\n```\nbeta\n```\n\nPinned as a test.\n");
        agrees("para\n\n```\n```          →  code(alpha)\nalpha\n```\nbeta\n```\n\nPinned.\n");
        // And the shape the operator actually saw, spelled out: the code block is one
        // block, closed, and the text after the second fence belongs to the third.
        let doc = "```\nalpha\n```\nbeta\n```\n\nPinned as a test.\n";
        let b = lex(doc);
        assert_eq!(b.len(), 3, "{b:#?}");
        let Some(Block::Code { lines, closed, .. }) = b.get(2) else { panic!("{b:#?}") };
        assert!(!*closed, "the last fence is open");
        assert!(
            lines.iter().any(|l| l.contains("Pinned")),
            "the text after the open fence belongs to it: {lines:?}"
        );
    }

    /// **A loose ordered list.** `1. first\n\n2. second` is one list, not two.
    ///
    /// Guard 4 asks "can this be another item of the list above?" and compared the
    /// *written numbers*, so `1.` and `2.` read as different items of different lists and
    /// the cut went between them. CommonMark's answer is about the marker's *kind*.
    /// `MARKDOWN`'s list is tight (no blank lines), which is why the fixture never caught
    /// this.
    #[test]
    fn a_loose_ordered_list_is_one_list_however_many_times_it_wraps() {
        agrees("1. first\n\n2. second\n\n3. third\n\ntail\n");
        agrees("1. first\n\n2. second\n\ntail\n");
        // A bullet list and an ordered one are different lists and the cut is fine.
        agrees("- a\n\n1. b\n\ntail\n");
        // Six points, each a sentence, is the real shape this came from.
        let doc = "1. one\n\n2. two\n\n3. three\n\n4. four\n\n5. five\n\n6. six\n\nThat is all.\n";
        agrees(doc);
        let blocks = lex(doc);
        let Some(Block::List { items, start, .. }) = blocks.first() else { panic!() };
        assert_eq!(*start, 1);
        assert_eq!(items.len(), 6, "six points, one list");
    }

    /// A long code block stays whole, and the window stays the size of the block.
    ///
    /// There is no legal cut inside a fence (see [`fence_spans`]), so a window that is one
    /// long code block has none at all and settles nothing until its closing fence arrives
    /// — which is the price of showing the model's code **as code**. The alternative the
    /// first version of this took, cutting at a line break inside the fence, split one
    /// block in two and turned the half without delimiters into a paragraph: code on the
    /// screen as prose.
    ///
    /// So the window is bounded by the cap for prose and by the largest code block
    /// otherwise. That is the old lexer's behaviour too (its fence guard was never relaxed
    /// either) and it is the honest bound: you cannot stream a block whose end you cannot
    /// see. The per-push cost is the block's length times markdown's ~106 ns/byte, so a
    /// 10 KB block costs about a millisecond a push while it streams — under a frame, and
    /// only while that block is arriving.
    #[test]
    fn a_long_code_block_stays_whole_and_its_window_is_the_block() {
        let doc: String = format!(
            "```rust\n{}```\n",
            (0..800).map(|i| format!("fn f{i}() {{}}\n")).collect::<String>()
        );
        assert!(doc.len() > 2 * DEFAULT_MAX_UNFROZEN, "{}", doc.len());
        let mut md = IncrementalMarkdown::new();
        for c in doc.as_bytes().chunks(8) {
            md.push(std::str::from_utf8(c).unwrap());
        }
        // One code block, because the closing fence arrived and closed it.
        let all = blocks(&md);
        let code: Vec<&Block> = all.iter().filter(|b| matches!(b, Block::Code { .. })).collect();
        assert_eq!(code.len(), 1, "the block was split: {code:#?}");
        // Nothing else: the code did not become prose.
        assert_eq!(all.len(), 1, "{all:#?}");
        let Block::Code { lines, closed, .. } = code[0] else { panic!() };
        assert!(*closed);
        assert_eq!(lines.len(), 800, "a line was lost");
        assert_eq!(lines[0], "fn f0() {}");
        assert_eq!(lines[799], "fn f799() {}");
        // And it never settled, because a window that is one fence has no legal cut in
        // it: the block renders from the tail, at the cost of one parse of its length per
        // push while it streams. Correct is the point; cheap is not available here.
        assert_eq!(md.stable_count(), 0);
        assert_eq!(md.tail().len(), 1);
        // The contrast: a fence *followed by* a blank line does settle, because the cut
        // after it is legal — it is past the closing delimiter.
        let mut md = IncrementalMarkdown::new();
        let with_prose = format!(
            "```rust\n{}```\n\nprose after\n",
            (0..800).map(|i| format!("fn f{i}() {{}}\n")).collect::<String>()
        );
        for c in with_prose.as_bytes().chunks(64) {
            md.push(std::str::from_utf8(c).unwrap());
        }
        assert!(md.stable_count() >= 1, "the finished block settled");
        let all = blocks(&md);
        assert_eq!(all.len(), 2, "{all:#?}");
        assert!(matches!(all[0], Block::Code { closed: true, .. }), "{all:#?}");
    }

    /// The shapes the corpus above was built from, plus the fixture, at several chunk
    /// sizes so the assertion is about the guards and not about one stride.
    #[test]
    fn every_shape_agrees_at_every_chunk_size() {
        let docs = [
            "prose\n\n```\n``` inner\ncode\n```\n\ntail\n```\n\nreal tail\n",
            "```\nalpha\n```\nbeta\n```\n\nPinned as a test.\n",
            "1. first\n\n2. second\n\n3. third\n\ntail\n",
            "- a\n\n1. b\n\ntail\n",
            "> ```rust\n> let a = 1;\n> ```\n",
            "- item\n\n  ```rust\n  let a = 1;\n  ```\n",
            "> | a | b |\n> |---|---|\n> | 1 | 2 |\n",
            "## h\n\ntext **bold** `code`\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n---\n\ntail\n",
        ];
        for doc in docs {
            for chunk in [1, 2, 3, 7, 64, 4096] {
                let a = stream(doc, chunk).blocks().cloned().collect::<Vec<_>>();
                assert_eq!(a, lex(doc), "chunk {chunk} on {doc:?}");
            }
        }
    }
}

#[cfg(test)]
mod inline_ranges_are_not_one_document {
    use super::*;
    use crate::markdown::tests::stream;

    /// A real message, 3.4 KB with ten blocks, captured from the operator's screen when it
    /// rendered as eleven blocks of code.
    ///
    /// It is here as bytes rather than paraphrased because that is what it is: the shape
    /// that broke the inline pass, at the length where it broke. The signature is an odd
    /// ``` in the opening paragraph and another one ten blocks later — and a paragraph
    /// with no syntax characters at all is the other half of it, because the pass that
    /// skips those is the one that made the pairing rare enough to ship.
    const REAL: &str = include_str!("../tests/fixtures/streamed-message.md");

    /// **One parse over every inline range makes one document out of them.**
    ///
    /// `set_included_ranges` concatenates the ranges in the byte stream: the parser is
    /// handed the paragraphs as if they were adjacent, so a ``` in one pairs with a ``` in
    /// another. In this message that produced a code span from byte 180 to byte 3209 —
    /// three kilobytes, spanning ten blocks — and every block in between rendered as code
    /// on the operator's screen (2026-09-20, "first rust block is perfect, second
    /// absolutely not").
    ///
    /// The streamed path never showed it, because a settled prefix is parsed on its own;
    /// only the one-shot path (a transcript replay after a restart) did. So the assertion
    /// is on the specific damage, not only on agreement.
    #[test]
    fn a_delimiter_does_not_pair_with_one_in_another_paragraph() {
        let blocks = lex(REAL);
        // The first paragraph is prose with two code spans in it ...
        let Block::Paragraph { lines } = &blocks[0] else { panic!("{:#?}", blocks[0]) };
        assert!(
            runs_text(&lines[0])
                .starts_with("Here — each block below exercises one of the paths I just fixed"),
            "{:?}",
            runs_text(&lines[0])
        );
        // ... and its ``` is literal text, not a delimiter.
        let text = runs_text(&lines[0]);
        assert!(text.contains("``` markers"), "{text:?}");
        assert!(text.contains("still arriving."), "{text:?}");
        // The second block keeps the bold it was written with — under the bug the whole
        // paragraph was one Code run and the markers showed.
        let Block::Paragraph { lines } = &blocks[1] else { panic!("{:#?}", blocks[1]) };
        assert!(
            lines[0].iter().any(|r| r.style == InlineStyle::Bold),
            "{:?}",
            lines[0]
        );
        assert_eq!(
            runs_text(&lines[0]).chars().take(14).collect::<String>(),
            "1. Plain, with"
        );
        // The blocks as written: 10 numbered items, the fences and lists among them, and
        // the two closing paragraphs. Counted rather than shape-matched because a block
        // that goes *missing* is the other failure this projection has had.
        // Counted rather than shape-matched because a block that goes *missing* is the
        // other failure this projection has had. 24 is what this message is: 10 numbered
        // paragraphs, the fences among them (three of them inside a quote or an item, which
        // are code boxes of their own here), the lists, the table, and the two closing
        // paragraphs.
        assert_eq!(blocks.len(), 24, "{blocks:#?}");

        // And the property that catches the next one of these: the one-shot parse and the
        // byte-at-a-time stream are the same document and must agree.
        for chunk in [1, 7, 64, 4096] {
            let a = stream(REAL, chunk).blocks().cloned().collect::<Vec<_>>();
            assert_eq!(a, blocks, "chunk {chunk}");
        }
    }

    /// The same shape in eight lines, so the mechanism is visible without the fixture.
    #[test]
    fn the_smallest_case_of_the_same_bug() {
        let doc = "a ``` b\n\n**bold** c\n\n```rust\nlet a = 1;\n```\n\nd ``` e\n";
        let b = lex(doc);
        let Some(Block::Paragraph { lines }) = b.first() else { panic!("{b:#?}") };
        assert_eq!(runs_text(&lines[0]), "a ``` b", "the ``` is text, not a delimiter");
        assert!(
            lines[0].iter().all(|r| r.style == InlineStyle::Plain),
            "{:?}",
            lines[0]
        );
        let Some(Block::Paragraph { lines }) = b.get(1) else { panic!("{b:#?}") };
        assert_eq!(runs_text(&lines[0]), "bold c");
        assert!(lines[0].iter().any(|r| r.style == InlineStyle::Bold), "{:?}", lines[0]);
        let Some(Block::Paragraph { lines }) = b.last() else { panic!("{b:#?}") };
        assert_eq!(runs_text(&lines[0]), "d ``` e");
        // One-shot and streamed must be the same document.
        for chunk in [1, 3, 4096] {
            let a = stream(doc, chunk).blocks().cloned().collect::<Vec<_>>();
            assert_eq!(a, b, "chunk {chunk}");
        }
    }

    /// Two paragraphs that each hold a code span keep them separate.
    ///
    /// The other face of the same bug: with the ranges concatenated, `` `a` `` in the first
    /// paragraph and `` `b` `` in the second could be read as one span from one to the
    /// other, so the text between them — including the blank line — became code.
    #[test]
    fn code_spans_in_different_paragraphs_stay_in_their_own() {
        let doc = "`a` here\n\nplain prose\n\n`b` there\n";
        let b = lex(doc);
        assert_eq!(b.len(), 3, "{b:#?}");
        let spans = |i: usize| -> Vec<(InlineStyle, String)> {
            let Block::Paragraph { lines } = &b[i] else { panic!("{b:#?}") };
            lines[0].iter().map(|r| (r.style, r.text.clone())).collect()
        };
        assert_eq!(
            spans(0),
            [
                (InlineStyle::Code, "a".to_string()),
                (InlineStyle::Plain, " here".to_string())
            ]
        );
        assert!(spans(1).iter().all(|(s, _)| *s == InlineStyle::Plain));
        assert!(spans(2).iter().any(|(s, t)| *s == InlineStyle::Code && t == "b"));
        for chunk in [1, 5, 4096] {
            let a = stream(doc, chunk).blocks().cloned().collect::<Vec<_>>();
            assert_eq!(a, b, "chunk {chunk}");
        }
    }
}

#[cfg(test)]
mod a_fence_ends_only_at_a_line_of_its_own {
    use super::*;

    /// **The grammar's closing fence is not line-anchored.** `"abc ```"` closes a block
    /// opened with ``` — the delimiter node is `" ```"` at bytes 7..11 — so a fence whose
    /// content ends a line with backticks is cut in half there, and because the damage is
    /// the parser's *state*, everything after it is wrong too.
    ///
    /// On the operator's screen (2026-09-20, *"awful"*) a message that drew box art
    /// containing ``` lost two lines of the art to a paragraph, and the rest of the message
    /// — headings, lists, prose — rendered inside one unclosed code box.
    ///
    /// The rule the code now follows is `fences_in`'s, from the text: a closing fence is a
    /// line of its own, made only of the same run, at least as long as the opening one, and
    /// carrying the block's container prefix if it has one.
    #[test]
    fn a_line_that_merely_ends_in_backticks_is_content() {
        for (src, want) in [
            ("```\nabc ```\ndef\n```\n", vec!["abc ```", "def"]),
            ("```\nfoo │ ```\nbar\n```\n", vec!["foo │ ```", "bar"]),
            ("```\nbar                      ```\nend\n```\n", vec!["bar                      ```", "end"]),
        ] {
            let b = lex(src);
            assert_eq!(b.len(), 1, "{src:?} gave {b:#?}");
            let Some(Block::Code { lines, closed, .. }) = b.first() else { panic!("{b:#?}") };
            assert!(closed, "{src:?}");
            assert_eq!(lines, &want, "{src:?}");
        }
    }

    /// **After masking, the tree holds no fence at all.** That is why there is no
    /// `fenced_code_block` arm anywhere in the projection, and why a fence inside a quote
    /// or an item can only come from the scan.
    ///
    /// This test exists because its absence hid a real thing: the first version of the
    /// container handling had arms for `fenced_code_block` and `code_fence_content` in
    /// `subtree_lines`, and a mutation that deleted them changed no test result. They were
    /// unreachable, and unreachable code is where a wrong assumption sits unexamined.
    /// Measured (2026-09-20): masking leaves zero nodes whose kind mentions a fence or code,
    /// for every container shape in this file's corpus.
    #[test]
    fn masking_leaves_no_fence_in_the_tree() {
        for src in [
            "> ```rust\n> let a = 1;\n> ```\n",
            "- item\n\n  ```rust\n  let a = 1;\n  ```\n",
            "```rust\nfn main() {}\n```\n",
            "> prose\n>\n> ```rust\n> let a = 1;\n> ```\n",
            "prose\n\n```\nabc ```\ndef\n```\n",
            "````\n```rust\nlet a = 1;\n```\n````\n",
        ] {
            let masked = mask(src, &fences_in(src));
            let mut stream = Stream::new(Lang::Markdown);
            stream.push(&masked);
            let root = stream.root().expect("a parse");
            let mut kinds = Vec::new();
            fn walk(n: &Node, kinds: &mut Vec<String>) {
                if n.kind.contains("fence") || n.kind.contains("code") {
                    kinds.push(n.kind.clone());
                }
                for c in &n.children {
                    walk(c, kinds);
                }
            }
            walk(&root, &mut kinds);
            assert!(kinds.is_empty(), "{src:?} left {kinds:?} in the tree");
            // Indented code is the one exception and is not a fence: no backticks, so it is
            // never masked and the tree is the only place it can be read from.
            if src.contains("    ") && !src.contains("```") {
                assert!(masked.contains("    "));
            }
        }
        // An indented block *is* still in the tree, which is what makes `code_block` live.
        let mut stream = Stream::new(Lang::Markdown);
        stream.push("    let a = 1;\n");
        let root = stream.root().unwrap();
        let mut kinds = Vec::new();
        fn walk2(n: &Node, kinds: &mut Vec<String>) {
            kinds.push(n.kind.clone());
            for c in &n.children {
                walk2(c, kinds);
            }
        }
        walk2(&root, &mut kinds);
        assert!(kinds.iter().any(|k| k == "indented_code_block"), "{kinds:?}");
    }

    /// **A fence on the list marker's own line.** `- ```rust` was not found by the scan at
    /// all, because it looked only at whitespace and `>`, so the item rendered as one run of
    /// text holding the markers — `"```rust let a = 1;   ```"` — beside a spurious empty
    /// code box.
    ///
    /// The fix is that a list marker is part of the container prefix. It comes back as
    /// blanks rather than as itself, because a marker does not repeat on a continuation
    /// line: `"- ```rust"` is continued by `"  let a = 1;"`, so the only prefix both share
    /// is the content column.
    #[test]
    fn a_fence_on_a_list_markers_line_is_not_item_text() {
        for src in ["- ```rust\n  let a = 1;\n  ```\n", "1. ```rust\n   let a = 1;\n   ```\n"] {
            let b = lex(src);
            assert_eq!(b.len(), 1, "{src:?} gave {b:#?}");
            let Some(Block::Code { lang, lines, closed }) = b.first() else { panic!("{b:#?}") };
            assert_eq!(lang, "rust", "{src:?}");
            assert_eq!(lines, &["let a = 1;"], "{src:?}");
            assert!(*closed, "{src:?}");
        }
        // A marker the model wrote as `*` or `+` is the same shape.
        for marker in ["-", "*", "+"] {
            let src = format!("{marker} ```rust\n  let a = 1;\n  ```\n");
            let b = lex(&src);
            let Some(Block::Code { lang, .. }) = b.first() else { panic!("{src:?} gave {b:#?}") };
            assert_eq!(lang, "rust", "{src:?}");
        }
    }

    /// Two things the marker-stripping must **not** eat, because either would be worse than
    /// the bug it fixed.
    #[test]
    fn stripping_a_content_column_does_not_eat_content() {
        // Indentation inside a top-level fence is the code's, not the container's.
        let b = lex("```rust\n    indented();\n\n        deeper();\n```\n");
        let Some(Block::Code { lines, .. }) = b.first() else { panic!("{b:#?}") };
        assert_eq!(lines, &["    indented();", "", "        deeper();"]);

        // A list *inside* a fence is the code's too.
        let b = lex("- item\n\n  ```\n  - a\n  - b\n  ```\n");
        assert_eq!(b.len(), 2, "{b:#?}");
        let Some(Block::Code { lines, .. }) = b.get(1) else { panic!("{b:#?}") };
        assert_eq!(lines, &["- a", "- b"]);

        // And a quoted fence's code that begins with `> ` keeps it.
        let b = lex("> ```\n> > nested quote\n> ```\n");
        let Some(Block::Code { lines, .. }) = b.first() else { panic!("{b:#?}") };
        assert_eq!(lines, &["> nested quote"]);
    }

    /// A fence indented by four spaces is read as an indented code block by CommonMark, and
    /// as a fence here.
    ///
    /// A deliberate deviation, and it is recorded rather than fixed. What a model means by
    /// `    ```rust` in a chat message is a code block with that language; CommonMark's
    /// answer is a code block *containing the literal backticks*, which would show a reader
    /// the markers instead of the code. The hand-written lexer this replaced read it the
    /// same way, so nothing regressed — but a reader comparing against CommonMark deserves
    /// to find it written down.
    #[test]
    fn a_four_space_indented_fence_is_read_as_a_fence() {
        // CommonMark would make this literal text: ```rust / let a = 1; / ```
        let b = lex("    ```rust\n    let a = 1;\n    ```\n");
        let Some(Block::Code { lang, lines, closed }) = b.first() else { panic!("{b:#?}") };
        assert_eq!(lang, "rust", "read as a fence, not as an indented code block");
        assert_eq!(lines, &["let a = 1;"]);
        assert!(*closed);
        // And it is one block, not three lines of prose with the markers showing.
        assert_eq!(b.len(), 1, "{b:#?}");
    }

    /// The box art that broke it, line for line. Six lines in, six lines out.
    #[test]
    fn the_box_art_that_broke_it_keeps_all_six_lines() {
        let src = "```\n┌─ rust                              ┌─ code\n│ fn main() {                        │ \
                   ```rust\n│     let xs: Vec<u32> = (0..5)…     │ let a = 1;\n│     println!(\"{xs:?}\");            │ \
                   ```\n│ }                                  └─\n└─\n```\n";
        let b = lex(src);
        assert_eq!(b.len(), 1, "{b:#?}");
        let Some(Block::Code { lines, closed, .. }) = b.first() else { panic!("{b:#?}") };
        assert!(closed);
        assert_eq!(lines.len(), 6, "{lines:#?}");
        assert!(lines[0].contains("┌─ rust"), "{lines:?}");
        assert!(lines[3].contains("```"), "the art's own backticks survive: {lines:?}");
        assert_eq!(lines[5], "└─");
    }

    /// Four backticks quote three: the inner fence is **content**, markers and all.
    #[test]
    fn a_longer_fence_can_quote_a_shorter_one() {
        let b = lex("````\n```rust\nlet a = 1;\n```\n````\n");
        assert_eq!(b.len(), 1, "{b:#?}");
        let Some(Block::Code { lang, lines, closed }) = b.first() else { panic!("{b:#?}") };
        assert_eq!(lang, "", "the info string is the outer fence's, and it has none");
        assert_eq!(lines, &["```rust", "let a = 1;", "```"]);
        assert!(closed);
    }

    /// A fence inside a quote closes on a line with the quote's own prefix.
    #[test]
    fn a_quoted_fence_closes_on_a_prefixed_line() {
        let src = "> ```rust\n> let a = 1;\n> ```\n\nafter\n";
        let b = lex(src);
        let Some(Block::Code { lines, .. }) = b.first() else { panic!("{b:#?}") };
        assert_eq!(lines, &["let a = 1;"]);
        // The prose after it is prose, not part of the code.
        assert_eq!(b.len(), 2, "{b:#?}");
        assert!(matches!(b[1], Block::Paragraph { .. }), "{b:#?}");
    }

    /// The message the operator saw, as bytes: every block in it survives, and it parses
    /// the same one-shot and streamed.
    #[test]
    fn the_box_art_message_parses_whole() {
        const REAL: &str = include_str!("../tests/fixtures/box-art-message.md");
        let b = lex(REAL);
        // The art is one block with all six of its lines — found rather than indexed,
        // because the index is not what the test is about.
        let art = b
            .iter()
            .find_map(|x| match x {
                Block::Code { lines, .. } if lines.iter().any(|l| l.starts_with("┌─ rust")) => {
                    Some(lines)
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("the art is not a code block: {b:#?}"));
        assert_eq!(art.len(), 6, "{art:#?}");
        assert_eq!(art[5], "└─", "{art:#?}");
        // The four-backtick fence that quotes a fence is its own block, markers kept.
        let quoted = b
            .iter()
            .find_map(|x| match x {
                Block::Code { lines, .. } if lines.first().is_some_and(|l| l.starts_with("```rust")) => {
                    Some(lines)
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("the quoting fence is not a code block: {b:#?}"));
        assert_eq!(quoted.as_slice(), &["```rust", "let a = 1;", "```"]);
        // Nothing is unclosed: a fence left open at the end of this message would mean the
        // tail had been swallowed, which is exactly what was on the screen.
        for block in &b {
            if let Block::Code { closed, lines, .. } = block {
                assert!(*closed, "an open fence swallowed the message: {lines:?}");
            }
        }
        // And nothing the model wrote is missing.
        let all: String = b
            .iter()
            .map(|x| match x {
                Block::Code { lines, .. } => lines.join("\n"),
                other => other.title(),
            })
            .collect::<Vec<_>>()
            .join("\n");
        for word in [
            "the current build draws",
            "That second box is the message",
            "Four backticks is markdown for",
            "a nested code box",
            "Everything else from the earlier reports",
        ] {
            assert!(all.contains(word), "{word:?} is missing");
        }
        for chunk in [1, 7, 64, 4096] {
            let a = crate::markdown::tests::stream(REAL, chunk)
                .blocks()
                .cloned()
                .collect::<Vec<_>>();
            assert_eq!(a, b, "chunk {chunk}");
        }
    }
}
