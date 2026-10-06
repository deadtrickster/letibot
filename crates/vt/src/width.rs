//! How many cells a character takes. Zero, one or two.
//!
//! # Where these ranges come from
//!
//! `crates/ui/src/width.rs` is this tree's measurement layer, and its `is_wide`/`is_zero_width`
//! are the ranges the tree is willing to carry: the East-Asian Wide and Fullwidth blocks a code
//! assistant actually meets, the combining-mark and variation-selector ranges, and the emoji
//! planes. A cell grid needs the first two of those — a CJK filename in `mc` is two columns per
//! ideograph, and a combining acute must not claim a column of its own — and this module takes
//! **those ranges, unchanged**, rather than inventing a second table. `crates/ui`'s tests walk a
//! corpus and assert that the two functions agree, so a divergence is a failing test rather than
//! a discovery.
//!
//! # What is deliberately not here, and the column it costs
//!
//! **The emoji planes.** A single emoji code point is two columns wide in every terminal on this
//! box, so this is a real loss, and it is taken deliberately: an emoji as a *program draws one*
//! is not one code point. `👨‍👩‍👧` is three code points joined by ZWJ, a flag is two regional
//! indicators, and a skin tone is a second code point — `crates/ui/src/width.rs` handles all
//! three, because it walks **grapheme clusters**. A grid has no cluster model: it has cells, one
//! per code point, and giving each code point of a ZWJ sequence two cells would draw the family
//! as six columns of separate people, which is worse than drawing it as three narrow ones. The
//! honest version of this would be a cluster-aware grid, and that is a design this crate does not
//! have.
//!
//! **The cost, stated so the pane can be priced**: an emoji occupies one cell here and two in the
//! reader's terminal, so a row containing one is **one column narrow per emoji** and anything
//! drawn after it on that row shifts left by one. It is the same trade `crates/ui/src/width.rs`
//! makes for a character outside its ranges, and it is visible rather than silent.
//!
//! # This is not UAX #11
//!
//! There is no Unicode database here and no generated table. A character outside the listed
//! ranges is one column.

/// Columns one character claims in the grid: zero, one or two.
pub fn char_width(c: char) -> usize {
    let u = c as u32;
    // C0/C1 and DEL. A grid should never be measuring these — the parser executes or drops
    // them before a character ever reaches a cell — but if one arrives it is not a column.
    if u < 0x20 || (0x7f..0xa0).contains(&u) {
        return 0;
    }
    if is_zero_width(u) {
        return 0;
    }
    if is_wide(u) { 2 } else { 1 }
}

/// Combining marks, joiners, and selectors — the ranges a model's output actually contains.
///
/// The same list as `crates/ui/src/width.rs::is_zero_width`, which is where it was written and
/// where the reasoning for it lives.
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

/// East-Asian Wide and Fullwidth — the ranges a grid can honour, because each code point is
/// one glyph of two columns.
///
/// The same list as `crates/ui/src/width.rs::is_wide` **minus the emoji planes**, for the reason
/// the module header gives.
fn is_wide(u: u32) -> bool {
    matches!(u,
        0x1100..=0x115f      // Hangul Jamo initial
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
        | 0x20000..=0x3fffd  // CJK extension B and beyond
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The characters a full-screen program actually puts on a screen**, one per range family,
    /// with the column each claims.
    #[test]
    fn a_cell_is_measured_in_columns_and_not_in_characters() {
        for (c, cols) in [
            ('a', 1),
            ('~', 1),
            ('é', 1),        // precomposed: one column, one cell
            ('\u{0301}', 0), // the combining acute alone claims nothing
            ('日', 2),
            ('本', 2),
            ('語', 2),
            ('あ', 2), // hiragana
            ('한', 2), // Hangul syllable
            ('Ａ', 2), // fullwidth Latin
            ('。', 2), // ideographic full stop
            ('─', 1),  // box drawing: one column, and this is what mc's frames are
            ('│', 1),
            ('\u{200b}', 0), // zero-width space
            ('\u{fe0f}', 0), // variation selector
        ] {
            assert_eq!(char_width(c), cols, "{c:?} ({:#x})", c as u32);
        }
    }

    /// **An emoji is one cell here and two in the terminal, and that is the stated cost** —
    /// asserted rather than left as a claim in a comment, because it is the one place this
    /// module knowingly disagrees with the reader's terminal and with `crates/ui`.
    #[test]
    fn an_emoji_is_one_cell_and_that_is_the_documented_cost() {
        assert_eq!(char_width('✅'), 1);
        assert_eq!(char_width('🦀'), 1);
        // The reason, in one line: a ZWJ sequence is three code points and would be six
        // columns of separate people if each of them claimed two.
        let family: usize = "👨‍👩‍👧".chars().map(char_width).sum();
        assert_eq!(family, 3);
    }
}
