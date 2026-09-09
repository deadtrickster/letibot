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

use crate::markdown::{Block, IncrementalMarkdown};

/// ANSI, kept as constants rather than a dependency.
pub mod sgr {
    pub const RESET: &str = "\x1b[0m";
    pub const BOLD: &str = "\x1b[1m";
    pub const DIM: &str = "\x1b[2m";
    pub const ITALIC: &str = "\x1b[3m";
    pub const CYAN: &str = "\x1b[36m";
    pub const GREEN: &str = "\x1b[32m";
    pub const YELLOW: &str = "\x1b[33m";
    pub const RED: &str = "\x1b[31m";
    pub const MAGENTA: &str = "\x1b[35m";
    pub const GREY: &str = "\x1b[90m";
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
}

impl Default for RenderConfig {
    fn default() -> Self {
        RenderConfig {
            width: 100,
            color: true,
            budget: Budget::default(),
        }
    }
}

impl RenderConfig {
    fn c(&self, code: &str, s: &str) -> String {
        if self.color {
            format!("{code}{s}{}", sgr::RESET)
        } else {
            s.to_string()
        }
    }
}

/// Render one block to lines, unbounded.
pub fn render_block(b: &Block, cfg: &RenderConfig) -> Vec<String> {
    let w = cfg.width.max(20);
    match b {
        Block::Heading { level, text } => {
            let prefix = "#".repeat(*level as usize);
            vec![cfg.c(sgr::BOLD, &format!("{prefix} {}", inline(text, cfg.color)))]
        }
        Block::Paragraph { lines } => {
            let joined = lines.join(" ");
            wrap(&inline(&joined, cfg.color), w)
        }
        Block::Code {
            lang,
            lines,
            closed,
        } => {
            let mut out = Vec::with_capacity(lines.len() + 2);
            let head = if lang.is_empty() {
                "┌─ code".to_string()
            } else {
                format!("┌─ {lang}")
            };
            out.push(cfg.c(sgr::GREY, &head));
            for l in lines {
                out.push(cfg.c(sgr::GREEN, &format!("│ {l}")));
            }
            out.push(cfg.c(
                sgr::GREY,
                if *closed {
                    "└─"
                } else {
                    "└─ (still writing…)"
                },
            ));
            out
        }
        Block::List { ordered, items } => {
            let mut out = Vec::new();
            for (i, it) in items.iter().enumerate() {
                let marker = if *ordered {
                    format!("{}. ", i + 1)
                } else {
                    "• ".to_string()
                };
                // Columns, not bytes. `"• "` is two columns and four bytes, and
                // indenting a wrapped bullet by its byte length put every
                // continuation line two columns too far right.
                let pad = visible_width(&marker);
                let body = wrap(&inline(it, cfg.color), w.saturating_sub(pad));
                for (j, line) in body.into_iter().enumerate() {
                    if j == 0 {
                        out.push(format!("{marker}{line}"));
                    } else {
                        out.push(format!("{:width$}{line}", "", width = pad));
                    }
                }
            }
            out
        }
        Block::Quote { lines } => {
            let joined = lines.join(" ");
            wrap(&inline(&joined, cfg.color), w.saturating_sub(2))
                .into_iter()
                .map(|l| cfg.c(sgr::DIM, &format!("│ {l}")))
                .collect()
        }
        Block::Rule => vec![cfg.c(sgr::GREY, &"─".repeat(w.min(60)))],
    }
}

/// Render a block, summarising it if it exceeds `limit` lines.
///
/// The shape is title, elision count, tail. Never a silent truncation: the count
/// is the disclosure, the same rule the log applies to `dropped`.
pub fn render_bounded(b: &Block, cfg: &RenderConfig, limit: usize) -> Vec<String> {
    let full = render_block(b, cfg);
    if full.len() <= limit || limit < 3 {
        return full;
    }
    let keep = limit - 2;
    let elided = full.len() - keep;
    let mut out = Vec::with_capacity(limit);
    out.push(cfg.c(sgr::DIM, &format!("▸ {}", trim_to(&b.title(), cfg.width))));
    out.push(cfg.c(sgr::GREY, &format!("  … {elided} lines elided …")));
    out.extend(full[full.len() - keep..].iter().cloned());
    out
}

/// Rendered lines for a growing document, with the frozen prefix cached.
#[derive(Debug, Default)]
pub struct BlockCache {
    width: usize,
    color: bool,
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
        if self.width != cfg.width || self.color != cfg.color || self.limit != limit {
            // A resize is the only thing that invalidates the prefix — and a change
            // of budget, which is a resize of a different axis: the same block
            // renders to a different number of lines when the bound moves, so a
            // prefix rendered under the old one is stale in exactly the same way.
            self.width = cfg.width;
            self.color = cfg.color;
            self.limit = limit;
            self.stable_lines.clear();
            self.rendered_blocks = 0;
        }
        let stable = md.stable();
        for b in &stable[self.rendered_blocks..] {
            self.stable_lines.extend(render_bounded(b, cfg, limit));
            self.stable_lines.push(String::new());
            self.blocks_rendered += 1;
        }
        self.rendered_blocks = stable.len();

        let mut tail = Vec::new();
        for b in md.tail() {
            tail.extend(render_bounded(b, cfg, limit));
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
}

/// Very small inline renderer: `code`, **bold**, *italic*.
///
/// Deliberately not a parser. A model's inline markup is shallow, and the failure
/// mode of getting it slightly wrong is a stray asterisk, not a wrong answer.
pub fn inline(s: &str, color: bool) -> String {
    if !color {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 16);
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'`'
            && let Some(end) = s[i + 1..].find('`')
        {
            out.push_str(sgr::CYAN);
            out.push_str(&s[i + 1..i + 1 + end]);
            out.push_str(sgr::RESET);
            i = i + 1 + end + 1;
            continue;
        }
        if b[i] == b'*'
            && i + 1 < b.len()
            && b[i + 1] == b'*'
            && let Some(end) = s[i + 2..].find("**")
        {
            out.push_str(sgr::BOLD);
            out.push_str(&s[i + 2..i + 2 + end]);
            out.push_str(sgr::RESET);
            i = i + 2 + end + 2;
            continue;
        }
        if b[i] == b'*'
            && let Some(end) = s[i + 1..].find('*')
        {
            out.push_str(sgr::ITALIC);
            out.push_str(&s[i + 1..i + 1 + end]);
            out.push_str(sgr::RESET);
            i = i + 1 + end + 1;
            continue;
        }
        let ch_len = utf8_len(b[i]);
        out.push_str(&s[i..(i + ch_len).min(s.len())]);
        i += ch_len;
    }
    out
}

fn utf8_len(b: u8) -> usize {
    match b {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

/// Word-wrap, counting *visible* columns so an SGR escape does not consume width.
///
/// A word wider than the whole line is **hard-broken** rather than emitted long.
/// Not a nicety: a model answering a question about this tree writes absolute
/// paths, and a 90-column path in an 80-column terminal used to be wrapped by the
/// terminal itself — which put a line on the screen the head had not counted, so
/// the bottom of the frame scrolled away under the status line and every
/// subsequent frame fought it. A head that decides the line count has to mean it.
pub fn wrap(s: &str, width: usize) -> Vec<String> {
    let width = width.max(8);
    let mut out = Vec::new();
    let mut line = String::new();
    let mut col = 0usize;
    for word in s.split(' ') {
        let mut word = word;
        let mut wlen = visible_width(word);
        // Break the oversized word across as many full lines as it needs, then let
        // the remainder flow normally.
        while wlen > width {
            if col > 0 {
                out.push(std::mem::take(&mut line));
                col = 0;
            }
            let (head, rest) = split_at_visible(word, width);
            out.push(head);
            word = rest;
            wlen = visible_width(word);
        }
        if col > 0 && col + 1 + wlen > width {
            out.push(std::mem::take(&mut line));
            col = 0;
        }
        if col > 0 {
            line.push(' ');
            col += 1;
        }
        line.push_str(word);
        col += wlen;
    }
    if !line.is_empty() || out.is_empty() {
        out.push(line);
    }
    out
}

/// Split at `width` visible columns, keeping escapes with the half they opened in.
fn split_at_visible(s: &str, width: usize) -> (String, &str) {
    let mut n = 0;
    let mut in_esc = false;
    for (i, c) in s.char_indices() {
        if in_esc {
            if c == 'm' {
                in_esc = false;
            }
            continue;
        }
        if c == '\x1b' {
            in_esc = true;
            continue;
        }
        if n == width {
            return (s[..i].to_string(), &s[i..]);
        }
        n += 1;
    }
    (s.to_string(), "")
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

/// A proportional bar `width` columns wide. Used for prefill, which is the one
/// thing in this harness that has a real denominator.
pub fn bar(done: u64, total: u64, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let filled = if total == 0 {
        0
    } else {
        ((done.min(total) as f64 / total as f64) * width as f64).round() as usize
    };
    let mut s = String::with_capacity(width * 3);
    for i in 0..width {
        s.push(if i < filled { '█' } else { '░' });
    }
    s
}

/// Columns a string occupies, ignoring SGR sequences. Counts a char as one column,
/// which is wrong for CJK and emoji and is the accepted cost of not vendoring a
/// width table for a terminal head.
pub fn visible_width(s: &str) -> usize {
    let mut n = 0;
    let mut in_esc = false;
    for c in s.chars() {
        if in_esc {
            if c == 'm' {
                in_esc = false;
            }
            continue;
        }
        if c == '\x1b' {
            in_esc = true;
            continue;
        }
        n += 1;
    }
    n
}

/// Cut to `width` visible columns, keeping any escape sequences intact.
pub fn trim_to(s: &str, width: usize) -> String {
    if visible_width(s) <= width {
        return s.to_string();
    }
    let mut out = String::new();
    let mut n = 0;
    let mut in_esc = false;
    for c in s.chars() {
        if in_esc {
            out.push(c);
            if c == 'm' {
                in_esc = false;
            }
            continue;
        }
        if c == '\x1b' {
            in_esc = true;
            out.push(c);
            continue;
        }
        if n + 1 > width.saturating_sub(1) {
            out.push('…');
            break;
        }
        out.push(c);
        n += 1;
    }
    out.push_str(sgr::RESET);
    out
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
        let coloured = inline("a `code` b", true);
        assert!(coloured.len() > 10);
        assert_eq!(visible_width(&coloured), "a code b".len());
        let lines = wrap(&coloured, 20);
        assert_eq!(lines.len(), 1, "escapes must not consume width: {lines:?}");
    }

    #[test]
    fn every_block_kind_renders() {
        for b in lex(MARKDOWN) {
            assert!(!render_block(&b, &cfg()).is_empty(), "{b:?}");
        }
    }
}
