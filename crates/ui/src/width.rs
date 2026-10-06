//! Columns, clusters and wrapping — the layer everything else measures with.
//!
//! `letibot-tui`'s `render::visible_width` says of itself: *"Counts a char as one
//! column, which is wrong for CJK and emoji and is the accepted cost of not
//! vendoring a width table for a terminal head."* That cost is not actually
//! accepted anywhere a person can see it. It shows up as a status line that
//! wraps and scrolls the screen the moment a model quotes a Chinese identifier,
//! as a box drawing that is one column short per emoji, and as a truncation that
//! cuts a `é` in half and leaves a combining acute alone on the line.
//!
//! There are three separate mistakes in one function and they need separating:
//!
//! 1. **A char is not a column.** CJK, Hangul and most emoji occupy two.
//! 2. **A char is not a cluster.** `e` + U+0301 is one thing on screen and must
//!    not be split by a truncation; a ZWJ emoji sequence is five code points and
//!    one glyph.
//! 3. **A word is not the only break opportunity.** CJK prose contains no
//!    spaces, so a space-only wrapper returns one line of 400 columns; and a
//!    120-character URL overflows the same way in English.
//!
//! Each is fixed below and each has a test named after the symptom.
//!
//! # What this implements, and what it does not
//!
//! It implements: the East-Asian Wide and Fullwidth blocks that a code assistant
//! actually meets (CJK ideographs, kana, Hangul, fullwidth ASCII, CJK
//! punctuation), the common emoji planes, the combining-mark and
//! variation-selector ranges, ZWJ joining, and regional-indicator pairing for
//! flags.
//!
//! It does **not** implement UAX #11 or UAX #29. There is no Unicode database
//! here and no generated table. A character outside the listed ranges is one
//! column and its own cluster. The failure mode of a miss is a line that is a
//! column narrow, in a terminal, once — which is the trade this file is making
//! on purpose and states so that a future reader can price it rather than
//! rediscover it.
//!
//! # Escapes are not content
//!
//! Every function here walks *escape-aware*: a CSI sequence contributes zero
//! columns and is never split. [`wrap`] goes further and carries the active SGR
//! state onto each continuation line, closing it at each line end, so that every
//! line it returns is independently paintable. A full-screen painter that emits
//! `\x1b[K` per line — which is what `letibot-tui::term::draw` does — erases to
//! end of line *in the current attributes*, so a line that leaves bold open
//! paints the rest of the row bold on the next frame. That is one of the ways a
//! screen "flickers".

/// One display cell: the escape run that precedes it, the grapheme cluster
/// itself, and how many columns it occupies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell<'a> {
    /// Zero or more ANSI escape sequences immediately before this cluster. Zero
    /// columns, never split, always carried with the cluster they style.
    pub esc: &'a str,
    /// The cluster. Empty only for the trailing escape-only cell.
    pub text: &'a str,
    /// Columns occupied: 0, 1 or 2.
    pub cols: usize,
}

/// Split a string into escape-prefixed grapheme clusters.
///
/// The last cell may have an empty `text` when the string ends in escapes — a
/// trailing `\x1b[0m` is the normal case and dropping it would leave attributes
/// open.
///
/// **This is the collecting half of [`for_each_cell`]**, which is the walk itself. See that
/// function for why the walk is shared rather than written twice: the cluster rules below (ZWJ,
/// regional indicators, control characters that do NOT combine) are the subtlest code in this
/// crate, and a second copy of them for the sake of one caller's buffer would be a second set of
/// rules to keep in step.
pub fn cells(s: &str) -> Vec<Cell<'_>> {
    // A capacity guess rather than `Vec::new()`: the loop is per cluster and this is the
    // measurement layer under every frame the head draws, so the early doublings are worth
    // skipping. It is a guess, so a wrong one costs a little memory and nothing else.
    let mut out = Vec::with_capacity(s.len() / 2 + 1);
    for_each_cell(s, |c| out.push(c));
    out
}

/// **The cluster walk, once.** Every cell of `s`, in order, handed to `f`.
///
/// The two things that need a cell list want different things from it — [`cells`] keeps every cell,
/// [`width`] sums one field of each — and **neither needs the list**. `width` used to `collect` a
/// `Vec<Cell>` and then sum its `cols`, which meant an allocation per measured line for a number
/// that a single pass produces without one: `visible_width` is called several times per row of
/// every frame the head draws (the trim, the wrap, the box edges, every card), so this is the
/// allocator underneath the whole measurement layer.
///
/// Shared rather than duplicated because the rules being walked are the subtle ones: a ZWJ pulls
/// in what follows, a regional-indicator pair is two columns however wide its halves claim to be, a
/// combining mark extends the cluster but **a control character does not**. A second implementation
/// written to avoid the closure would be a second place for those to be got wrong, and the crate's
/// own rule is that a module's behaviour is one implementation — see the header's note about
/// `%width-between` staying allocation-free for exactly this reason.
pub fn for_each_cell<'a>(s: &'a str, mut f: impl FnMut(Cell<'a>)) {
    let b = s.as_bytes();
    let mut i = 0usize;
    while i < b.len() {
        let esc_start = i;
        while i < b.len() && b[i] == 0x1b {
            i = skip_escape(b, i);
        }
        let esc = &s[esc_start..i];
        if i >= b.len() {
            if !esc.is_empty() {
                f(Cell {
                    esc,
                    text: "",
                    cols: 0,
                });
            }
            break;
        }
        let cluster_start = i;
        let mut cols = 0usize;
        let mut prev_ri = false;
        let mut first = true;
        while i < b.len() {
            let c = match s[i..].chars().next() {
                Some(c) => c,
                None => break,
            };
            if c == '\x1b' {
                break;
            }
            let w = char_width(c);
            if first {
                cols = w;
                first = false;
                prev_ri = is_regional_indicator(c);
                i += c.len_utf8();
                continue;
            }
            // Extend the cluster with anything that does not stand alone.
            if w == 0 {
                // …but a **control character is not a combining mark**. It
                // measures zero columns for the same reason a combining mark
                // does, and that is the whole of the resemblance: absorbing a
                // `\n` into the cluster before it hides the row break inside a
                // cell, and a break inside a cell is not a break at all. That is
                // exactly how a two-line composer wrapped to one row with a
                // literal newline in it.
                if is_control(c) {
                    break;
                }
                // A combining mark, a variation selector, or a ZWJ. A ZWJ also
                // pulls in whatever follows it, which is handled by the loop
                // simply continuing.
                i += c.len_utf8();
                continue;
            }
            if prev_ri && is_regional_indicator(c) {
                // A flag: exactly two regional indicators, and the pair is two
                // columns wide however wide each half claims to be.
                cols = 2;
                prev_ri = false;
                i += c.len_utf8();
                continue;
            }
            // A ZWJ immediately before this one joins it into the cluster.
            if s[cluster_start..i].ends_with('\u{200d}') {
                cols = cols.max(w);
                i += c.len_utf8();
                continue;
            }
            break;
        }
        f(Cell {
            esc,
            text: &s[cluster_start..i],
            cols,
        });
    }
}

/// Bytes past the escape sequence starting at `i`.
///
/// Handles CSI (`\x1b[…` terminated by `@`–`~`), OSC (`\x1b]…` terminated by BEL
/// or ST) and the two-byte forms. An unterminated sequence consumes the rest,
/// which is the right answer for a partially arrived frame: it is not content.
fn skip_escape(b: &[u8], i: usize) -> usize {
    let mut j = i + 1;
    if j >= b.len() {
        return j;
    }
    match b[j] {
        b'[' => {
            j += 1;
            while j < b.len() && !(0x40..=0x7e).contains(&b[j]) {
                j += 1;
            }
            (j + 1).min(b.len())
        }
        b']' => {
            j += 1;
            while j < b.len() {
                if b[j] == 0x07 {
                    return j + 1;
                }
                if b[j] == 0x1b && j + 1 < b.len() && b[j + 1] == b'\\' {
                    return j + 2;
                }
                j += 1;
            }
            j
        }
        _ => j + 1,
    }
}

/// Columns a string occupies on a terminal, escapes excluded.
///
/// **Allocation-free**, which is the whole reason it is a sum over a walk rather than over a
/// collected `Vec<Cell>` — see [`for_each_cell`]. This is called several times per row of every
/// frame the head draws, and on a settled screen most of those calls are on lines that have not
/// changed.
pub fn width(s: &str) -> usize {
    let mut cols = 0usize;
    for_each_cell(s, |c| cols += c.cols);
    cols
}

/// Columns one character claims. Zero, one or two.
pub fn char_width(c: char) -> usize {
    let u = c as u32;
    // C0/C1 and DEL. A head should never be measuring these; if one arrives it
    // is not a column.
    if u < 0x20 || (0x7f..0xa0).contains(&u) {
        return 0;
    }
    if is_zero_width(u) {
        return 0;
    }
    if is_wide(u) { 2 } else { 1 }
}

/// C0, C1 and DEL. Zero columns, and never part of the cluster beside them.
fn is_control(c: char) -> bool {
    let u = c as u32;
    u < 0x20 || (0x7f..0xa0).contains(&u)
}

fn is_regional_indicator(c: char) -> bool {
    (0x1f1e6..=0x1f1ff).contains(&(c as u32))
}

/// Combining marks, joiners, and selectors — the ranges a model's output
/// actually contains.
fn is_zero_width(u: u32) -> bool {
    matches!(u,
        0x0300..=0x036f      // combining diacritical marks
        | 0x0483..=0x0489    // Cyrillic combining
        | 0x0591..=0x05bd | 0x05bf | 0x05c1..=0x05c2 | 0x05c4..=0x05c5 | 0x05c7
        | 0x0610..=0x061a | 0x064b..=0x065f | 0x0670
        | 0x06d6..=0x06dc | 0x06df..=0x06e4 | 0x06e7..=0x06e8 | 0x06ea..=0x06ed
        | 0x0900..=0x0903 | 0x093a..=0x093c | 0x0941..=0x0948 | 0x094d
        | 0x0951..=0x0957 | 0x0962..=0x0963
        | 0x0e31 | 0x0e34..=0x0e3a | 0x0e47..=0x0e4e   // Thai
        | 0x1ab0..=0x1aff    // combining extended
        | 0x1dc0..=0x1dff    // combining supplement
        | 0x200b..=0x200f    // ZWSP, ZWNJ, ZWJ, LRM, RLM
        | 0x2028..=0x202e    // line/para separators, bidi overrides
        | 0x2060..=0x2064    // word joiner, invisible operators
        | 0x20d0..=0x20f0    // combining marks for symbols
        | 0xfe00..=0xfe0f    // variation selectors
        | 0xfe20..=0xfe2f    // combining half marks
        | 0xfeff             // BOM / ZWNBSP
        | 0xe0100..=0xe01ef  // variation selectors supplement
    )
}

/// East-Asian Wide and Fullwidth, plus the emoji planes that terminals render
/// double-width.
fn is_wide(u: u32) -> bool {
    matches!(u,
        0x1100..=0x115f      // Hangul Jamo initial
        | 0x231a..=0x231b | 0x23e9..=0x23ec | 0x23f0 | 0x23f3
        | 0x25fd..=0x25fe | 0x2614..=0x2615 | 0x2648..=0x2653
        | 0x267f | 0x2693 | 0x26a1 | 0x26aa..=0x26ab | 0x26bd..=0x26be
        | 0x26c4..=0x26c5 | 0x26ce | 0x26d4 | 0x26ea | 0x26f2..=0x26f3
        | 0x26f5 | 0x26fa | 0x26fd | 0x2705 | 0x270a..=0x270b | 0x2728
        | 0x274c | 0x274e | 0x2753..=0x2755 | 0x2757 | 0x2795..=0x2797
        | 0x27b0 | 0x27bf | 0x2b1b..=0x2b1c | 0x2b50 | 0x2b55
        | 0x2e80..=0x2e99 | 0x2e9b..=0x2ef3   // CJK radicals
        | 0x2f00..=0x2fd5    // Kangxi radicals
        | 0x2ff0..=0x2ffb    // ideographic description
        | 0x3000..=0x303e    // CJK symbols and punctuation
        | 0x3041..=0x3096 | 0x3099..=0x30ff   // kana
        | 0x3105..=0x312f | 0x3131..=0x318e | 0x3190..=0x31e3
        | 0x31f0..=0x321e | 0x3220..=0x3247 | 0x3250..=0x4dbf
        | 0x4e00..=0xa48c    // CJK unified ideographs, Yi
        | 0xa490..=0xa4c6
        | 0xa960..=0xa97c    // Hangul Jamo extended-A
        | 0xac00..=0xd7a3    // Hangul syllables
        | 0xf900..=0xfaff    // CJK compatibility ideographs
        | 0xfe10..=0xfe19 | 0xfe30..=0xfe52 | 0xfe54..=0xfe66 | 0xfe68..=0xfe6b
        | 0xff01..=0xff60    // fullwidth forms
        | 0xffe0..=0xffe6
        | 0x16fe0..=0x16fe4 | 0x17000..=0x18d08
        | 0x1b000..=0x1b2fb
        | 0x1f004 | 0x1f0cf | 0x1f18e | 0x1f191..=0x1f19a
        | 0x1f1e6..=0x1f1ff  // regional indicators, paired into flags above
        | 0x1f200..=0x1f320 | 0x1f32d..=0x1f335 | 0x1f337..=0x1f37c
        | 0x1f37e..=0x1f393 | 0x1f3a0..=0x1f3ca | 0x1f3cf..=0x1f3d3
        | 0x1f3e0..=0x1f3f0 | 0x1f3f4 | 0x1f3f8..=0x1f43e | 0x1f440
        | 0x1f442..=0x1f4fc | 0x1f4ff..=0x1f53d | 0x1f54b..=0x1f54e
        | 0x1f550..=0x1f567 | 0x1f57a | 0x1f595..=0x1f596 | 0x1f5a4
        | 0x1f5fb..=0x1f64f | 0x1f680..=0x1f6c5 | 0x1f6cc
        | 0x1f6d0..=0x1f6d2 | 0x1f6d5..=0x1f6d7 | 0x1f6eb..=0x1f6ec
        | 0x1f6f4..=0x1f6fc | 0x1f7e0..=0x1f7eb
        | 0x1f90c..=0x1f93a | 0x1f93c..=0x1f945 | 0x1f947..=0x1f9ff
        | 0x1fa70..=0x1faff
        | 0x20000..=0x3fffd  // CJK extension B and beyond
    )
}

/// The reset sequence, spelled once.
pub const RESET: &str = "\x1b[0m";

/// Active SGR state, so a wrapped line can reopen what the previous one closed.
///
/// Not a full SGR model: it keeps the sequences seen since the last reset, in
/// order, and replays them. Replaying `\x1b[1m\x1b[36m` reproduces bold cyan
/// exactly; replaying a sequence that *cancels* an attribute (`\x1b[22m`) also
/// works, because it is replayed in the same order. The one thing it does not do
/// is collapse redundancy, which costs bytes and nothing else.
#[derive(Debug, Default, Clone)]
struct Sgr {
    open: String,
}

impl Sgr {
    fn feed(&mut self, esc: &str) {
        let b = esc.as_bytes();
        let mut i = 0;
        while i < b.len() {
            let end = skip_escape(b, i);
            let seq = &esc[i..end];
            // Only SGR (`…m`) affects rendition; a cursor move in the middle of
            // a wrapped paragraph is not ours to replay.
            if seq.ends_with('m') && seq.starts_with("\x1b[") {
                let params = &seq[2..seq.len() - 1];
                if params.is_empty() || params == "0" {
                    self.open.clear();
                } else {
                    self.open.push_str(seq);
                }
            }
            i = end;
        }
    }

    fn is_open(&self) -> bool {
        !self.open.is_empty()
    }
}

/// Truncate to `cols` columns, never splitting a cluster or an escape.
///
/// When something is dropped the last column becomes `…`, so the elision is
/// visible rather than silent. Any open attribute is closed at the end.
pub fn truncate(s: &str, cols: usize) -> String {
    if width(s) <= cols {
        return s.to_string();
    }
    if cols == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut sgr = Sgr::default();
    let mut used = 0usize;
    for c in cells(s) {
        sgr.feed(c.esc);
        out.push_str(c.esc);
        if used + c.cols > cols.saturating_sub(1) {
            break;
        }
        out.push_str(c.text);
        used += c.cols;
    }
    out.push('…');
    if sgr.is_open() {
        out.push_str(RESET);
    }
    out
}

/// **Shorten `s` to `cols` columns by eating its LEFT, keeping the end** — the pair to
/// [`truncate`], and the one to reach for on a path.
///
/// A path is recognised by where it ends: `…/src/protocol.rs` names the file, while
/// `crates/sessionlog/src/protoco…` names only the tree it is in — and on a screen where every
/// row is a file in the same tree, the second leaves every row looking alike. This is
/// `letibot-tui`'s `shorten_subject`/`ellipsise_left` pair lifted where a card can reach it:
/// the live tool card needs it for the same reason the transcript row did, and a second copy
/// in that crate would be the drift this tree keeps finding.
///
/// **The cut lands at a separator when one is available**, so the result reads as a path
/// rather than as a word with a piece missing: `…/tui/src/app.rs`, not `…ui/src/app.rs`.
pub fn ellipsise_left(s: &str, cols: usize) -> String {
    if width(s) <= cols {
        return s.to_string();
    }
    if cols < 2 {
        return String::new();
    }
    let keep = cols - 1;
    let cs = cells(s);
    // Walk the cells from the END, keeping whole characters until the budget is spent.
    let mut used = 0usize;
    let mut start = cs.len();
    for (i, c) in cs.iter().enumerate().rev() {
        if used + c.cols > keep {
            start = i + 1;
            break;
        }
        used += c.cols;
        start = i;
    }
    // **Then forward to the next separator, if the cut landed inside a name AND one is close.**
    // The reader gets a whole component back rather than most of one — but the nudge is BOUNDED,
    // and that bound is a measurement: unbounded, it walked to the next `/` however far away it
    // was, so a command with a long argument between separators lost everything up to it. Measured
    // on a 227-column card whose target was `cd /opt/secure_auth && gcc … -Wl,-rpath,/opt/…`: the
    // whole command was cut to its last fifty columns, and both a 227-wide and an 80-wide card
    // rendered the identical row. Six columns is enough to finish a short component and too few to
    // eat an argument.
    const NUDGE: usize = 6;
    if start < cs.len() && !cs[start].text.starts_with('/') {
        match cs[start..].iter().position(|c| c.text.starts_with('/')) {
            Some(next) if next <= NUDGE => start += next,
            _ => {}
        }
    }
    let mut out = String::from("…");
    for c in &cs[start..] {
        out.push_str(c.esc);
        out.push_str(c.text);
    }
    out
}

/// Pad to exactly `cols` columns with spaces, truncating if too long.
pub fn fit(s: &str, cols: usize) -> String {
    let w = width(s);
    if w > cols {
        return truncate(s, cols);
    }
    let mut out = s.to_string();
    out.push_str(&" ".repeat(cols - w));
    out
}

/// Wrap to `cols` columns.
///
/// Three break rules, in priority order:
///
/// 1. At a space, the ordinary case.
/// 2. Between two wide clusters, which is what makes CJK wrap at all. A
///    space-only wrapper returns one 400-column line for a paragraph of Chinese,
///    and that line then scrolls the whole screen sideways.
/// 3. Anywhere, when a single unbreakable run is longer than the width — a URL,
///    a base64 blob, a 200-character type signature. Overflowing instead is not
///    a gentler failure: it is the same corruption, deferred to the painter.
///
/// Every returned line is independently paintable: escapes are carried, and an
/// attribute left open at a break is closed and reopened.
pub fn wrap(s: &str, cols: usize) -> Vec<String> {
    let cs = cells(s);
    let rows = break_cells(&cs, cols.max(4));
    let mut sgr = Sgr::default();
    let mut out = Vec::with_capacity(rows.len());
    for (a, b) in rows {
        let mut line = String::new();
        if sgr.is_open() {
            line.push_str(&sgr.open);
        }
        for c in &cs[a..b] {
            sgr.feed(c.esc);
            line.push_str(c.esc);
            line.push_str(c.text);
        }
        // Trailing space at a break is invisible, and leaving it in means a
        // painter that erases to end of line paints the row's background one
        // column further than the text goes. A trailing newline is worse: the
        // terminal acts on it.
        while line.ends_with(' ') || line.ends_with('\n') || line.ends_with('\r') {
            line.pop();
        }
        if sgr.is_open() {
            line.push_str(RESET);
        }
        out.push(line);
    }
    out
}

/// The single breakpoint finder. [`wrap`] and [`wrap_ranges`] both go through
/// it, so the two cannot drift apart — which they otherwise would, and
/// grok-build says so about their own pair (`wrapping.rs:84`: the search-highlight
/// wrapper "reproduces the same breakpoints … explicitly documented as needing
/// to stay in sync with the visual path"). Two functions kept in sync by a
/// comment is a bug with a schedule.
///
/// Returns half-open **cell index** ranges that tile the input. A trailing space
/// belongs to the row it ended, so a row's slice may be one column over `cols`
/// *in trailing whitespace only* — the usual line-breaking contract.
fn break_cells(cs: &[Cell], cols: usize) -> Vec<(usize, usize)> {
    let mut rows: Vec<(usize, usize)> = Vec::new();
    let mut row_start = 0usize;
    let mut used = 0usize;
    let mut word_start: Option<usize> = None;
    let mut word_cols = 0usize;

    for (i, c) in cs.iter().enumerate() {
        if c.text.is_empty() {
            continue; // escape-only cell: it styles, it does not occupy
        }
        // A newline is a **hard break**, and it has to be one here rather than in
        // a caller. `char_width` measures a C0 byte as zero columns, so without
        // this a two-line composer wrapped to a single row with a literal `\n`
        // inside it — which the terminal then obeys, putting a row on the screen
        // the head did not count and scrolling the frame it had just painted.
        // The break belongs to the row it *ends*, so the ranges still tile the
        // input; [`wrap`] strips it, as does anything else rendering a row.
        if c.text == "\n" {
            if let Some(ws) = word_start.take() {
                if used > 0 && used + word_cols > cols {
                    rows.push((row_start, ws));
                    row_start = ws;
                }
                word_cols = 0;
            }
            rows.push((row_start, i + 1));
            row_start = i + 1;
            used = 0;
            continue;
        }
        let space = c.text == " " || c.text == "\t";
        let wide = c.cols == 2;

        if space || wide {
            if let Some(ws) = word_start.take() {
                if used > 0 && used + word_cols > cols {
                    rows.push((row_start, ws));
                    row_start = ws;
                    used = 0;
                }
                used += word_cols;
                word_cols = 0;
            }
            if space {
                used += c.cols;
                if used > cols {
                    rows.push((row_start, i + 1));
                    row_start = i + 1;
                    used = 0;
                }
            } else {
                if used > 0 && used + c.cols > cols {
                    rows.push((row_start, i));
                    row_start = i;
                    used = 0;
                }
                used += c.cols;
            }
            continue;
        }

        // An ordinary cluster joins the pending word.
        if word_start.is_none() {
            word_start = Some(i);
            word_cols = 0;
        }
        if word_cols + c.cols > cols {
            // The word alone is longer than a whole row. Place what there is,
            // then hard-break here — overflowing instead is the same corruption
            // deferred to the painter.
            let ws = word_start.unwrap();
            if used > 0 && used + word_cols > cols {
                rows.push((row_start, ws));
                row_start = ws;
            }
            rows.push((row_start, i));
            row_start = i;
            used = 0;
            word_start = Some(i);
            word_cols = 0;
        }
        word_cols += c.cols;
    }
    if let Some(ws) = word_start
        && used > 0
        && used + word_cols > cols
    {
        rows.push((row_start, ws));
        row_start = ws;
    }
    rows.push((row_start, cs.len()));
    rows
}

/// Wrap text, returning **byte ranges** rather than strings.
///
/// The editor needs this: to move the cursor up a row it has to know which
/// bytes are on which row, and a `Vec<String>` has thrown that away. Same
/// breakpoints as [`wrap`], because both call [`break_cells`].
///
/// The ranges tile the input: `ranges[0].start == 0`, each range's `end` is the
/// next range's `start`, and the last `end == s.len()`. A trailing space at a
/// break belongs to the row it ended, so `width(&s[r])` may exceed `cols` by
/// that whitespace; `width(s[r].trim_end())` does not.
pub fn wrap_ranges(s: &str, cols: usize) -> Vec<std::ops::Range<usize>> {
    // The empty string needs no special case: `cells` gives nothing,
    // `break_cells` still emits one row, and the offset table's sentinel makes
    // it `0..0`.
    let cs = cells(s);
    // Byte offset of each cell's start, plus the end sentinel.
    let mut at = Vec::with_capacity(cs.len() + 1);
    let mut off = 0usize;
    for c in &cs {
        at.push(off);
        off += c.esc.len() + c.text.len();
    }
    at.push(s.len());
    break_cells(&cs, cols.max(4))
        .into_iter()
        .map(|(a, b)| at[a]..at[b])
        .collect()
}

/// The row a byte offset lands on, and the display column within it.
///
/// The conversion an editor needs and the one that is easy to get subtly wrong:
/// **display columns, not characters and not bytes**. opencode's
/// `packages/tui/src/prompt/display.ts` is the same conversion done right in
/// TypeScript (`Intl.Segmenter` for clusters, `Bun.stringWidth` for columns),
/// and it is the one piece of genuinely portable rendering logic in that
/// codebase — everything else is inside the native `@opentui/core`.
pub fn locate(s: &str, byte: usize, cols: usize) -> (usize, usize) {
    let rows = wrap_ranges(s, cols);
    let byte = byte.min(s.len());
    let row = rows.iter().rposition(|r| r.start <= byte).unwrap_or(0);
    (row, width(&s[rows[row].start..byte]))
}

/// The byte offset at display column `col` of row `row`.
///
/// Clamps into the row rather than failing: a cursor moved down onto a shorter
/// row lands at its end, which is what every editor does and what the sticky
/// preferred column depends on.
pub fn offset_at(s: &str, row: usize, col: usize, cols: usize) -> usize {
    let rows = wrap_ranges(s, cols);
    let r = match rows.get(row) {
        Some(r) => r.clone(),
        None => return s.len(),
    };
    let mut used = 0usize;
    let mut at = r.start;
    for c in cells(&s[r.clone()]) {
        if used >= col {
            return at;
        }
        at += c.esc.len() + c.text.len();
        used += c.cols;
    }
    // Do not park the cursor past a trailing break space, or past the newline
    // that ended the row — the end of row *n* and the start of row *n+1* are two
    // different places and only one of them is where the person can see the
    // caret.
    let mut end = r.end;
    while end > r.start && matches!(s.as_bytes()[end - 1], b' ' | b'\n' | b'\r') {
        end -= 1;
    }
    end.max(r.start)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_newline_is_a_row_break_and_never_reaches_the_terminal() {
        // `char_width` measures a C0 byte as zero columns, so a newline used to
        // wrap to nothing: a two-line composer was one row with a `\n` in it, and
        // the terminal obeyed the `\n`.
        let s = "first line\nsecond line";
        assert_eq!(wrap(s, 40), vec!["first line", "second line"]);
        assert_eq!(wrap_ranges(s, 40), vec![0..11, 11..22]);
        // The break belongs to the row it ended, so the ranges still tile.
        assert_eq!(locate(s, 11, 40), (1, 0));
        assert_eq!(locate(s, 10, 40), (0, 10));
        // A trailing newline is a real empty row: it is where the caret is.
        assert_eq!(wrap("a\n", 40), vec!["a", ""]);
        assert_eq!(locate("a\n", 2, 40), (1, 0));
        // And the caret cannot be parked on the newline itself.
        assert_eq!(offset_at(s, 0, 99, 40), 10);
        for l in wrap("a very long first line that wraps\nand a second", 12) {
            assert!(!l.contains('\n'), "{l:?}");
            assert!(width(&l) <= 12, "{l:?}");
        }
    }

    #[test]
    fn wrap_ranges_tile_the_input_and_agree_with_wrap() {
        let inputs = [
            "the quick brown fox jumps over the lazy dog",
            "你好世界，这是一个测试。abc def",
            "short",
            "",
            &"x".repeat(120),
            "a bb ccc dddd eeeee ffffff ggggggg",
            "two\nlines",
            "trailing\n",
            "\n\n",
        ];
        for s in inputs {
            for w in [5usize, 8, 13, 40] {
                let r = wrap_ranges(s, w);
                assert_eq!(r[0].start, 0, "{s:?} @{w}");
                assert_eq!(r.last().unwrap().end, s.len(), "{s:?} @{w}");
                for pair in r.windows(2) {
                    assert!(pair[0].end <= pair[1].start, "{s:?} @{w}: {r:?}");
                }
                for range in &r {
                    let slice = &s[range.clone()];
                    assert!(
                        width(slice.trim_end()) <= w,
                        "{s:?} @{w}: {range:?} = {slice:?}"
                    );
                }
                // Same breakpoints, by construction; asserted anyway because
                // "by construction" is a claim about code that changes.
                let strings = wrap(s, w);
                assert_eq!(r.len(), strings.len(), "{s:?} @{w}: {r:?}");
                for (range, line) in r.iter().zip(&strings) {
                    assert_eq!(s[range.clone()].trim_end(), line, "{s:?} @{w}");
                }
            }
        }
    }

    #[test]
    fn a_cjk_char_is_two_columns() {
        assert_eq!(width("你好"), 4);
        assert_eq!(width("abc"), 3);
        // Fullwidth ASCII, which a model emits when quoting Japanese source.
        assert_eq!(width("ＡＢ"), 4);
    }

    #[test]
    fn a_combining_mark_is_not_a_column() {
        // "e" + U+0301 renders as one é.
        assert_eq!(width("e\u{301}"), 1);
        assert_eq!(cells("e\u{301}").len(), 1);
    }

    #[test]
    fn a_zwj_emoji_sequence_is_one_cluster() {
        // Family: man + ZWJ + woman + ZWJ + girl.
        let s = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}";
        let cs = cells(s);
        assert_eq!(cs.len(), 1, "{cs:?}");
        assert_eq!(cs[0].cols, 2);
    }

    #[test]
    fn a_flag_is_one_cluster_of_two_columns() {
        let s = "\u{1f1ef}\u{1f1f5}"; // JP
        let cs = cells(s);
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].cols, 2);
    }

    #[test]
    fn escapes_are_zero_columns_and_are_never_split() {
        let s = "\x1b[1mbold\x1b[0m";
        assert_eq!(width(s), 4);
        let cs = cells(s);
        assert_eq!(cs[0].esc, "\x1b[1m");
        assert_eq!(cs.last().unwrap().esc, "\x1b[0m");
        assert_eq!(cs.last().unwrap().text, "");
    }

    #[test]
    fn truncation_does_not_cut_a_cluster_in_half() {
        let s = "a\u{301}b\u{301}c\u{301}";
        let t = truncate(s, 2);
        // Two columns: one cluster plus the ellipsis.
        assert_eq!(width(&t), 2);
        assert!(t.starts_with("a\u{301}"), "{t:?}");
    }

    #[test]
    fn truncation_closes_an_open_attribute() {
        let t = truncate("\x1b[1maaaaaaaa", 4);
        assert!(t.ends_with(RESET), "{t:?}");
        assert_eq!(width(&t), 4);
    }

    #[test]
    fn cjk_prose_wraps_even_though_it_has_no_spaces() {
        // The symptom this exists for: a space-only wrapper returns one line.
        let doc = "你好".repeat(20); // 80 columns
        let lines = wrap(&doc, 20);
        assert!(lines.len() >= 4, "{} lines", lines.len());
        for l in &lines {
            assert!(width(l) <= 20, "{:?} is {} columns", l, width(l));
        }
    }

    #[test]
    fn an_unbreakable_run_is_hard_broken_not_overflowed() {
        let url = "https://example.com/".to_string() + &"a".repeat(200);
        let lines = wrap(&url, 40);
        assert!(lines.len() >= 5);
        for l in &lines {
            assert!(width(l) <= 40, "{} columns: {l:?}", width(l));
        }
        let rejoined: String = lines.concat();
        assert_eq!(rejoined, url, "hard breaking must not lose characters");
    }

    #[test]
    fn every_wrapped_line_is_independently_paintable() {
        // An attribute open at a break is closed and reopened, because a painter
        // that erases to end of line does so in the current attributes.
        let s = format!("\x1b[1m{}\x1b[0m", "word ".repeat(20));
        let lines = wrap(&s, 20);
        assert!(lines.len() > 1);
        for l in &lines[..lines.len() - 1] {
            assert!(l.ends_with(RESET), "line left bold open: {l:?}");
            assert!(l.starts_with("\x1b[1m"), "line did not reopen bold: {l:?}");
        }
    }

    #[test]
    fn wrapping_never_exceeds_the_width_for_any_input() {
        let inputs = [
            "the quick brown fox jumps over the lazy dog",
            "你好世界，这是一个测试。",
            "mixed 混合 text with émojis 🎉 and \x1b[31mcolour\x1b[0m",
            "a",
            "",
            &"x".repeat(300),
        ];
        for s in inputs {
            for w in [4usize, 7, 20, 41, 80] {
                for l in wrap(s, w) {
                    assert!(width(&l) <= w, "{s:?} at {w}: {l:?} is {}", width(&l));
                }
            }
        }
    }

    #[test]
    fn fit_pads_to_exactly_the_width() {
        assert_eq!(width(&fit("ab", 5)), 5);
        assert_eq!(width(&fit("你好世界", 5)), 5);
        assert_eq!(width(&fit("", 3)), 3);
    }

    /// **The two width tables in this process agree, and where they do not it is written down.**
    ///
    /// A cell grid has to measure a character in columns for itself: `letibot_vt` is below this
    /// crate and cannot call [`char_width`], and the ranges below are the ones this tree is willing
    /// to carry, so they are the ones it copies. **A copy is a thing that drifts**, so the
    /// agreement is asserted here rather than claimed in a comment — and the one deliberate
    /// divergence is asserted too, because a difference nobody wrote down is a bug somebody will
    /// "fix" in the wrong place.
    ///
    /// The divergence is the emoji planes: a grid has one cell per code point and this module has
    /// one per *grapheme cluster*, so a single emoji is two columns to the head and one to the
    /// screen, and a ZWJ sequence is one glyph here and its parts there. `letibot_vt::width`'s
    /// header is where the cost is stated.
    #[test]
    fn the_cell_grid_measures_the_way_this_module_does_except_where_it_says_otherwise() {
        // Everything a full-screen program's box, a path and a CJK filename are made of.
        let agree = [
            'a', 'Z', '~', ' ', '0', '-', '_', '\u{e9}', '\u{301}', '\u{200b}',
            '\u{fe0f}', // combining and zero-width
            '日', '本', '語', 'あ', 'ア', '한', 'Ａ', '。', '「',
            '　', // CJK, kana, Hangul, fullwidth
            '─', '│', '┌', '┐', '└', '┘', '├',
            '┼', // box drawing: one column, and it must stay one
            '\u{fffd}', '\u{a0}',
        ];
        for c in agree {
            assert_eq!(
                letibot_vt::width::char_width(c),
                char_width(c),
                "the two tables disagree about {c:?} ({:#x})",
                c as u32
            );
        }
        // And the divergence, named: two columns to the head, one to the grid.
        for c in ['✅', '🦀', '👍'] {
            assert_eq!(char_width(c), 2, "the head draws {c:?} double-width");
            assert_eq!(
                letibot_vt::width::char_width(c),
                1,
                "the grid gives {c:?} one cell, which is the documented cost"
            );
        }
    }
}
