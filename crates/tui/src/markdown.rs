//! Incremental markdown: §13.3's other half.
//!
//! > **Head:** incremental lexing with a frozen stable prefix. omp's mechanism is
//! > the one to copy: `stableBlockBoundary` finds the offset past the last token
//! > whose raw text ends in `\n\n`, subject to guards (the break must be strictly
//! > inside the text, the next char must start real block content, a preceding list
//! > must be provably closed since CommonMark can continue a list across a blank
//! > line); `#lexTokens` then lexes only the grown tail and concatenates.
//! >
//! > **What not to do:** pi's `updateContent` calls `contentContainer.clear()` and
//! > rebuilds every child from the whole accumulated `message.content` on **every**
//! > `message_update`, i.e. once per delta — O(n²) over a message.
//!
//! The licence for the concatenation, in omp's own words: *"block tokenization is
//! local across a `\n\n` boundary with balanced fences, so
//! `lex(prefix) ++ lex(tail) === lex(prefix+tail)`."* That equation is a property
//! test in this file, not a comment.
//!
//! # The measurement that makes this worth building
//!
//! [`IncrementalMarkdown::bytes_lexed`] is instrumentation, kept in the shipping
//! type rather than in a test harness, because "is the renderer quadratic again"
//! is a question that gets asked once a year and is unanswerable after the fact.
//! A full re-parse per delta lexes `O(n²)` bytes over a message; this lexes
//! `O(n · paragraph)`. The test asserts the ratio, not the absolute number.

/// One block. Line-oriented on purpose: a head renders lines, and a block model
/// finer than the thing being rendered is cost with no buyer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Heading {
        level: u8,
        text: String,
    },
    Paragraph {
        lines: Vec<String>,
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
        items: Vec<String>,
    },
    Quote {
        lines: Vec<String>,
    },
    Rule,
}

impl Block {
    /// A one-line name for the block, for §13.3's "render the head of a long block
    /// as a title".
    pub fn title(&self) -> String {
        match self {
            Block::Heading { text, .. } => text.clone(),
            Block::Paragraph { lines } => lines.first().cloned().unwrap_or_default(),
            Block::Code { lang, lines, .. } => {
                let lang = if lang.is_empty() { "code" } else { lang };
                format!("{lang} · {} lines", lines.len())
            }
            Block::List { items, .. } => items.first().cloned().unwrap_or_default(),
            Block::Quote { lines } => lines.first().cloned().unwrap_or_default(),
            Block::Rule => "───".into(),
        }
    }
}

/// How large the unfrozen tail may grow before guard 4 is relaxed.
///
/// Configurable rather than hardcoded, per §13.3's matching rule for the display
/// buffer. Four kilobytes is about a screen of prose: large enough that no
/// ordinary list reaches it, small enough that the quadratic term never gets going.
pub const DEFAULT_MAX_UNFROZEN: usize = 4 * 1024;

/// A markdown document that grows only at the end.
#[derive(Debug)]
pub struct IncrementalMarkdown {
    raw: String,
    /// Everything before this byte offset has been lexed and will never be lexed
    /// again.
    stable_len: usize,
    stable: Vec<Block>,
    tail: Vec<Block>,
    max_unfrozen: usize,
    bytes_lexed: u64,
    lex_calls: u64,
}

impl Default for IncrementalMarkdown {
    fn default() -> Self {
        IncrementalMarkdown {
            raw: String::new(),
            stable_len: 0,
            stable: Vec::new(),
            tail: Vec::new(),
            max_unfrozen: DEFAULT_MAX_UNFROZEN,
            bytes_lexed: 0,
            lex_calls: 0,
        }
    }
}

impl IncrementalMarkdown {
    pub fn new() -> Self {
        IncrementalMarkdown::default()
    }

    pub fn with_max_unfrozen(max_unfrozen: usize) -> Self {
        IncrementalMarkdown {
            max_unfrozen,
            ..IncrementalMarkdown::default()
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
        if let Some(b) = stable_boundary_with(&self.raw[self.stable_len..], self.max_unfrozen) {
            let abs = self.stable_len + b;
            let frozen = &self.raw[self.stable_len..abs];
            self.bytes_lexed += frozen.len() as u64;
            self.lex_calls += 1;
            self.stable.extend(lex(frozen));
            self.stable_len = abs;
        }
        let tail = &self.raw[self.stable_len..];
        self.bytes_lexed += tail.len() as u64;
        self.lex_calls += 1;
        self.tail = lex(tail);
    }

    /// Blocks in order: the frozen prefix, then the live tail.
    pub fn blocks(&self) -> impl Iterator<Item = &Block> {
        self.stable.iter().chain(self.tail.iter())
    }

    /// How many blocks are frozen. A renderer caches exactly this many.
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

    /// Total bytes handed to the lexer over this document's life. The number a
    /// regression test watches.
    pub fn bytes_lexed(&self) -> u64 {
        self.bytes_lexed
    }

    pub fn lex_calls(&self) -> u64 {
        self.lex_calls
    }
}

/// The offset past the last `\n\n` that is safe to freeze, if any.
///
/// The four guards, each of which is a case where `lex(a) ++ lex(b)` is *not*
/// `lex(a ++ b)`:
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
///    something that cannot be one of its items. Checking backwards instead ("was
///    the last block a list? then never freeze") is what a first attempt does, and
///    it costs a long bulleted answer its whole stable prefix — the exact case the
///    mechanism exists for.
///
/// Every guard is decided from the two lines either side of the candidate, which
/// is what keeps this O(tail) per call rather than O(document).
///
/// `relax_at` is the escape hatch, and it is not a hedge. Guard 4 is *unfalsifiable
/// while the list is still open*: a message that is one long loose list has no
/// provably-closed boundary until it ends, so a strict reader freezes nothing and
/// is quadratic again — on exactly the shape (a long bulleted answer) that made
/// §13.3 worth writing. Past `relax_at` unfrozen bytes the guard is dropped, and
/// the cost of being wrong is bounded and visible: two tight lists render where one
/// loose list was, a blank line's difference in a terminal. The cost of the
/// alternative is O(n²) on a 50 KB answer.
pub fn stable_boundary(s: &str) -> Option<usize> {
    stable_boundary_with(s, usize::MAX)
}

pub fn stable_boundary_with(s: &str, relax_at: usize) -> Option<usize> {
    // One forward pass over the tail, carrying the fence state, rather than
    // re-deciding "are the fences balanced here" per candidate. The per-candidate
    // form is O(tail²) per push and it is quietly fatal: it turns the fix for the
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

/// Lex a self-contained span of markdown into blocks.
///
/// Line-oriented and deliberately small. It is not a CommonMark implementation and
/// does not pretend to be: it covers what a model emits — headings, paragraphs,
/// fenced code, lists, quotes, rules — and the guards above are what keep the
/// difference between this and CommonMark from mattering at a boundary.
pub fn lex(s: &str) -> Vec<Block> {
    let mut out = Vec::new();
    let mut lines = s.lines().peekable();
    let mut para: Vec<String> = Vec::new();
    let mut list: Option<(bool, Vec<String>)> = None;
    let mut quote: Vec<String> = Vec::new();

    macro_rules! flush {
        () => {
            if !para.is_empty() {
                out.push(Block::Paragraph {
                    lines: std::mem::take(&mut para),
                });
            }
            if let Some((ordered, items)) = list.take() {
                out.push(Block::List { ordered, items });
            }
            if !quote.is_empty() {
                out.push(Block::Quote {
                    lines: std::mem::take(&mut quote),
                });
            }
        };
    }

    while let Some(line) = lines.next() {
        let t = line.trim_start();
        if t.is_empty() {
            flush!();
            continue;
        }
        if let Some(rest) = t.strip_prefix("```").or_else(|| t.strip_prefix("~~~")) {
            flush!();
            let lang = rest.trim().to_string();
            let mut body = Vec::new();
            let mut closed = false;
            for l in lines.by_ref() {
                let lt = l.trim_start();
                if lt.starts_with("```") || lt.starts_with("~~~") {
                    closed = true;
                    break;
                }
                body.push(l.to_string());
            }
            out.push(Block::Code {
                lang,
                lines: body,
                closed,
            });
            continue;
        }
        if t.starts_with('#') {
            let level = t.chars().take_while(|c| *c == '#').count();
            if level <= 6 && t.chars().nth(level) == Some(' ') {
                flush!();
                out.push(Block::Heading {
                    level: level as u8,
                    text: t[level + 1..].trim().to_string(),
                });
                continue;
            }
        }
        if is_rule(t) {
            flush!();
            out.push(Block::Rule);
            continue;
        }
        if let Some(rest) = t.strip_prefix("> ").or_else(|| t.strip_prefix(">")) {
            if !para.is_empty() || list.is_some() {
                flush!();
            }
            quote.push(rest.to_string());
            continue;
        }
        if let Some((ordered, item)) = list_item(t) {
            if !para.is_empty() || !quote.is_empty() {
                flush!();
            }
            match &mut list {
                Some((o, items)) if *o == ordered => items.push(item),
                _ => {
                    if let Some((o, items)) = list.take() {
                        out.push(Block::List { ordered: o, items });
                    }
                    list = Some((ordered, vec![item]));
                }
            }
            continue;
        }
        if let Some((_, items)) = list.as_mut() {
            // A lazy continuation of the last list item.
            if let Some(last) = items.last_mut() {
                last.push(' ');
                last.push_str(t);
            }
            continue;
        }
        if !quote.is_empty() {
            quote.push(t.to_string());
            continue;
        }
        para.push(line.to_string());
    }
    flush!();
    out
}

fn is_rule(t: &str) -> bool {
    let t = t.trim();
    (t.len() >= 3)
        && (t.chars().all(|c| c == '-')
            || t.chars().all(|c| c == '*')
            || t.chars().all(|c| c == '_'))
}

fn list_item(t: &str) -> Option<(bool, String)> {
    for m in ["- ", "* ", "+ "] {
        if let Some(rest) = t.strip_prefix(m) {
            return Some((false, rest.to_string()));
        }
    }
    let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
    if !digits.is_empty() && digits.len() <= 9 {
        let rest = &t[digits.len()..];
        if let Some(r) = rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") ")) {
            return Some((true, r.to_string()));
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

    #[test]
    fn lex_prefix_concat_lex_tail_equals_lex_of_the_whole() {
        // The licence for the whole mechanism, as an assertion.
        let doc = MARKDOWN;
        let whole = lex(doc);
        let mut any = false;
        let mut cut = 0;
        while let Some(b) = stable_boundary(&doc[cut..]) {
            let abs = cut + b;
            let mut joined = lex(&doc[..abs]);
            joined.extend(lex(&doc[abs..]));
            assert_eq!(joined, whole, "split at byte {abs} changed the lex");
            any = true;
            cut = abs;
        }
        assert!(any, "the fixture must contain at least one stable boundary");
    }

    #[test]
    fn streaming_gives_the_same_blocks_as_a_single_parse() {
        for chunk in [1, 3, 7, 64] {
            let md = stream(MARKDOWN, chunk);
            let streamed: Vec<Block> = md.blocks().cloned().collect();
            assert_eq!(streamed, lex(MARKDOWN), "chunk size {chunk}");
        }
    }

    #[test]
    fn the_renderer_is_not_quadratic() {
        // A full re-parse per delta lexes ~n²/2c bytes for n bytes in c-byte
        // chunks. This must be far below that. The assertion is the *ratio*,
        // because the absolute number depends on the fixture.
        let doc = MARKDOWN.repeat(8);
        let chunk = 4;
        let md = stream(&doc, chunk);
        let n = doc.len() as u64;
        let naive = n * n / (2 * chunk as u64);
        assert!(
            md.bytes_lexed() < naive / 10,
            "lexed {} bytes; a full re-parse per delta would lex about {naive}. \
             That ratio is the whole of §13.3.",
            md.bytes_lexed()
        );
        // And the frozen prefix is genuinely frozen: each byte of it was lexed once.
        assert!(md.stable_count() > 8);
    }

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
    fn an_open_loose_list_freezes_nothing_until_it_is_long_enough_to_matter() {
        // Strictly correct: a list that is still open has no provably-closed
        // boundary, so nothing freezes.
        let doc: String = (0..30).map(|i| format!("- item {i}\n\n")).collect();
        let mut md = IncrementalMarkdown::with_max_unfrozen(usize::MAX);
        for c in doc.as_bytes().chunks(4) {
            md.push(std::str::from_utf8(c).unwrap());
        }
        assert_eq!(md.stable_count(), 0);
    }

    #[test]
    fn a_message_that_is_one_long_list_is_linear_not_quadratic() {
        // The worst shape for guard 4, and the one the relaxation exists for.
        // Linearity is the claim, so the test doubles the input: quadratic work
        // would quadruple, linear work roughly doubles.
        fn lexed(items: usize) -> u64 {
            let doc: String = (0..items).map(|i| format!("- item {i}\n\n")).collect();
            let mut md = IncrementalMarkdown::new();
            for c in doc.as_bytes().chunks(4) {
                md.push(std::str::from_utf8(c).unwrap());
            }
            assert!(md.stable_count() > items / 2, "froze {}", md.stable_count());
            md.bytes_lexed()
        }
        let small = lexed(800);
        let large = lexed(1600);
        assert!(
            large < small * 3,
            "doubling the message multiplied the lexing by {:.1}; \
             quadratic is 4, linear is 2",
            large as f64 / small as f64
        );
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
    fn the_last_block_is_never_frozen_while_it_can_still_grow() {
        let mut md = IncrementalMarkdown::new();
        md.push("one\n\n");
        // The trailing boundary is at the very end, so nothing freezes: the next
        // delta may continue the paragraph after the blank line into a new block,
        // but it may equally be a lazy continuation.
        assert_eq!(md.stable_count(), 0);
        md.push("two");
        assert_eq!(md.stable_count(), 1);
        assert_eq!(md.tail().len(), 1);
    }

    #[test]
    fn an_unterminated_fence_still_renders() {
        let md = stream("```rust\nlet a = 1;\n", 3);
        let blocks: Vec<&Block> = md.blocks().collect();
        assert!(matches!(
            blocks.last(),
            Some(Block::Code { closed: false, .. })
        ));
    }
}
