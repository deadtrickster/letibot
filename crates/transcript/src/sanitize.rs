//! **Text a head did not author, made safe for a terminal** (§3.1).
//!
//! A head composes rows and writes them verbatim, so any control byte in content it did
//! not write is an instruction to the operator's terminal: `ESC ] 0 ; x BEL` renames the
//! window they are working in, `ESC [ 2 J` clears it, `ESC [ ? 1002 h` turns the mouse
//! wheel off for the rest of the session. The operator's own store measures the risk
//! rather than arguing it: 44 `tool_result` rows carry an escape and 20 carry a mode
//! string.
//!
//! # Why this is in `letibot-transcript`
//!
//! Because it is needed on **both sides of a boundary that cannot see across itself**:
//! `letibot-sessionlog` composes a call's display target from the model's own arguments
//! (`truncate_target`), and `letibot-ui` composes rows. `sessionlog` does not depend on
//! `letibot-ui` and must not start, and a second copy of a sequence parser is exactly the
//! kind of duplicate that drifts. This crate is the one both already see — the same
//! reason [`crate::ToolEditExcerpt`] lives here.
//!
//! # What this is FOR, and it is not "every string"
//!
//! **§3.1's subject is content the head did NOT write.** A row the head composed
//! itself — a note it painted with its own colour, a card body it built, a listing it
//! laid out — is not foreign, and running a sanitiser over it strips the head's own
//! escapes. That is not a smaller bug than the one this module exists for; it is the same
//! bug with the sign flipped, and it was measured on the operator's screen: a `/notes`
//! row read ` [31m… [0m` because a sanitiser had been pointed at the head's own painted
//! text.
//!
//! So call it **where foreign text enters** — a payload line, a model's prose, a tool's
//! reason, a daemon's title, a call's arguments — never on a string that may already
//! contain the head's own sequences. `Palette::paint` deliberately does not call it for
//! that reason.
//!
//! # Two functions, and the difference is not cosmetic
//!
//! `\n` **is a control character** — `char::is_control` is true of every C0 code
//! including it — so passing a document to [`without_control`] does not sanitise it, it
//! **collapses it onto one line**: every paragraph, every list item and every fenced
//! block gone. That mistake is invisible on a one-line fixture and destroys every long
//! message, so the two cases are two named functions rather than one and a `.map`.
//!
//! # And one walk under both of them
//!
//! [`pieces`] is that walk, and it exists because **removing every escape is not the only
//! honest thing to do with one**. `ls`, `grep` and `cargo` colour their output with SGR,
//! and the operator's own command is meant to look on their screen as it looks in their
//! console — so the walk reports an SGR sequence as the parameters it carries, and the two
//! readers take what they want: [`without_control`] the text, `letibot_ui::ansi` the text
//! *and* the colour, reduced to the palette's own roles. Everything that is not text and
//! not SGR is removed exactly as it always was, by this one parser rather than by a second
//! copy of the sequence rules kept in step by hand.

use std::borrow::Cow;
use std::iter::Peekable;
use std::str::Chars;

/// **One piece of a line, as a terminal would read it.**
///
/// The module above removes every control byte, and for a head that draws text that is
/// right. It is not right for the **operator's own command**, whose output is meant to
/// look on the screen as it looks in their console: `ls`, `grep` and `cargo` colour with
/// SGR, and dropping the sequence loses the one thing the program said. So the walk
/// reports SGR rather than swallowing it, and the caller that wants colour
/// (`letibot_ui::ansi`) interprets it into a palette role; the caller that wants text
/// ([`without_control`]) takes the `Text` pieces and nothing else.
///
/// **One parser, two readers.** The sequence-skipping rules are the subtle part of this
/// module — the C1 introducers, an `OSC` that ends at `BEL` or `ST`, a parameter byte that
/// is not a digit — and a second copy of them for the colour path is exactly the duplicate
/// that drifts. `without_control` is now this walk with the SGR pieces dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    /// Text a reader sees. A control byte that was **not** part of a sequence has
    /// already become a space here, by [`without_control`]'s own rule and for its reason:
    /// dropping it would silently reflow the line.
    Text(String),
    /// **An SGR sequence's parameters** — `ESC [ … m` — in order.
    ///
    /// A bare `ESC[m`, and an empty parameter inside a longer sequence (`ESC[;31m`), are
    /// the `0` that ECMA-48 says they are, so a reader that understands `0` needs no
    /// special case for them.
    Sgr(Vec<u16>),
}

/// **The line, split into the two things a terminal does with it**: text it shows and
/// SGR it obeys. Everything else — a mode string, an OSC title, a C1 control, a DEL — is
/// still removed, and this is the only walk in this module that does that.
///
/// A sequence this cannot read as SGR is **not** reported as one: `ESC[?25m` is a private
/// form and not a colour, `ESC[38;5;167m`'s parameters are reported but a reader that has
/// no role for the xterm cube drops it (see `letibot_ui::ansi`), and a parameter that is
/// not a decimal number makes the whole sequence a non-SGR that goes as it always did.
/// The rule is the same one this module has always had: **a well-formed sequence goes as
/// one thing**, and nothing of its body is ever left on the line as text.
pub fn pieces(line: &str) -> Vec<Piece> {
    let mut out: Vec<Piece> = Vec::new();
    let mut text = String::new();
    let mut it = line.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            // `ESC` introduces a sequence; the sequence goes with it.
            '\u{1b}' => {
                flush(&mut out, &mut text);
                match it.peek().copied() {
                    Some('[') => {
                        it.next();
                        if let Some(p) = csi(&mut it) {
                            out.push(Piece::Sgr(p));
                        }
                    }
                    Some(']') => {
                        it.next();
                        skip_osc(&mut it);
                    }
                    // Any other two-byte sequence (`ESC ( B`, `ESC =`), or an `ESC` that
                    // ends the line: one more character goes with it if there is one.
                    Some(_) => {
                        it.next();
                    }
                    None => {}
                }
            }
            // The C1 forms carry the same meaning with no `ESC` in front: `CSI` (U+009B)
            // and `OSC` (U+009D) introduce, `ST` (U+009C) is a bare terminator.
            '\u{9b}' => {
                flush(&mut out, &mut text);
                if let Some(p) = csi(&mut it) {
                    out.push(Piece::Sgr(p));
                }
            }
            '\u{9d}' => {
                flush(&mut out, &mut text);
                skip_osc(&mut it);
            }
            '\u{9c}' => flush(&mut out, &mut text),
            c if c.is_control() => text.push(' '),
            c => text.push(c),
        }
    }
    flush(&mut out, &mut text);
    out
}

/// Hand the text run built so far to `out`, if there is one.
fn flush(out: &mut Vec<Piece>, text: &mut String) {
    if !text.is_empty() {
        out.push(Piece::Text(std::mem::take(text)));
    }
}

/// **Is there anything in here for the sanitiser to do?**
///
/// The one predicate the whole module turns on, and it is exact rather than conservative: the
/// slow path below only ever *changes* a string when it meets a character that is a control
/// character — `ESC` and the C1 introducers are C0/C1 and so are `char::is_control`, and the
/// fallback arm replaces exactly the ones that are. So a string with no control character
/// anywhere comes out of [`without_control`] byte-for-byte identical, and asking this first is
/// free of any risk of skipping work that was needed.
///
/// **Whitespace is not a control character** and must not be treated as one: a tab *is* control
/// (and becomes a space), but `\n` and `\r` reaching [`without_control`] are the documented
/// document-destroying mistake that function's own doc warns about — the caller that means lines
/// is [`without_control_lines`], which splits first.
fn clean(s: &str) -> bool {
    !s.chars().any(char::is_control)
}

/// One line, with any escape sequence **wholly removed** and any other control byte
/// replaced by a space.
///
/// # Why the whole sequence and not just the `ESC`
///
/// The first version of this replaced every control character with a space, which for an
/// escape sequence's introducer is the worst of both: the `ESC` becomes a space, the
/// `[31m` stays, and the result is **five columns of visible garbage where the terminal
/// measured none** — which also corrupts the wrap, because the wrapper counts what is on
/// the line. `ESC[31m` is one instruction, so it goes as one thing. The same flaw was in
/// `sessionlog`'s `truncate_target`, which is why this function has one home.
///
/// # Why removal is the honest arithmetic
///
/// A well-formed sequence occupies **no columns**: a terminal acts on it and
/// `letibot_ui::width::width` skips it. Removing it therefore leaves the column count
/// exactly as it was, while replacing it with a space would add one. A **lone** control
/// byte that is not part of a sequence — a tab, a `\r`, a DEL — becomes a space instead,
/// because dropping it would silently reflow the line.
pub fn without_control(line: &str) -> String {
    if clean(line) {
        return line.to_string();
    }
    let mut out = String::with_capacity(line.len());
    for p in pieces(line) {
        if let Piece::Text(t) = p {
            out.push_str(&t);
        }
    }
    out
}

/// A document: the same, **line by line**, so its newlines survive.
///
/// See [`without_control`] for what the single-line version does to a document. A
/// trailing newline is structure too, so the split-and-rejoin keeps it.
///
/// # Why this borrows when it can, and it usually can
///
/// **This is called once per model DELTA** — one per token — on the text and reasoning channels
/// (`tui/src/app.rs:4059`, `:4081`), and a token of ordinary prose has no control character in it.
/// Building a `String` for every one of those meant an allocation and a copy per token, on the hot
/// path, for a transformation that had nothing to do. `Cow::Borrowed` makes the common case a
/// comparison and no allocation at all, and the guarantee is unchanged: the returned text is
/// still safe to put on a terminal, because anything that needed changing still goes through
/// [`without_control`].
///
/// The `Owned` branch is the same split-and-rejoin as before. It is taken when ANY line has a
/// control character — a conservative test on the whole document, so a message with one escape
/// sequence in it pays for all its lines. That is the right trade for a hot path whose input is
/// almost always clean, and being wrong about which branch costs time rather than correctness.
pub fn without_control_lines(s: &str) -> Cow<'_, str> {
    if clean(s) {
        return Cow::Borrowed(s);
    }
    Cow::Owned(
        s.split('\n')
            .map(without_control)
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// The parameters of a `CSI` sequence, with the introducer already consumed: `0x20..=0x3f`
/// are parameters and intermediates, and one byte in `0x40..=0x7e` ends it.
///
/// Returns them **only when the sequence is an SGR** — final byte `m`, every parameter a
/// decimal number — and `None` otherwise, having consumed exactly the bytes a skip would
/// have. That `None` is not a failure: a mode string is not a colour and is removed.
fn csi(it: &mut Peekable<Chars<'_>>) -> Option<Vec<u16>> {
    let mut body = String::new();
    while it.peek().is_some_and(|c| ('\u{20}'..='\u{3f}').contains(c)) {
        body.push(it.next().expect("peeked"));
    }
    let final_byte = match it.peek() {
        Some(c) if ('\u{40}'..='\u{7e}').contains(c) => Some(it.next().expect("peeked")),
        _ => None,
    };
    if final_byte != Some('m') {
        return None;
    }
    let mut params = Vec::new();
    for part in body.split(';') {
        // An empty parameter is `0`: `ESC[m` is a reset and `ESC[;31m` sets red.
        params.push(if part.is_empty() {
            0
        } else {
            part.parse::<u16>().ok()?
        });
    }
    Some(params)
}

/// An `OSC` string, with the introducer already consumed: it ends at `BEL` or at `ST`,
/// and a sequence that never ends takes the rest of the line with it — which is what a
/// terminal would do with it too.
fn skip_osc(it: &mut Peekable<Chars<'_>>) {
    while let Some(c) = it.next() {
        if c == '\u{7}' {
            return;
        }
        if c == '\u{1b}' {
            if it.peek() == Some(&'\\') {
                it.next();
            }
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The defect that was measured on the operator's screen**: an escape's body must
    /// not survive as printable text. Every family is from the store's own vocabulary.
    #[test]
    fn an_escape_sequence_goes_whole_and_never_leaves_its_body_behind() {
        let hostile = "a\u{1b}[31mred\u{1b}[0m \u{1b}[8mdim \u{1b}[2J clear \
                       \u{1b}[?1002h mouse \u{1b}[?1006h \u{1b}[?1049h alt \u{1b}[?2004h \
                       paste \u{1b}[?2026h sync \u{1b}]0;title\u{7} end";
        let safe = without_control(hostile);
        // The text around the sequences is kept…
        for kept in [
            "a", "red", "dim", "clear", "mouse", "alt", "paste", "sync", "end",
        ] {
            assert!(safe.contains(kept), "{kept} was lost: {safe:?}");
        }
        // …and **not one byte of any sequence is**, which is the whole assertion. A
        // leftover `[31m` is the exact shape of the regression.
        for gone in [
            "\u{1b}", "[31m", "[0m", "[8m", "[2J", "[?1002h", "[?1006h", "[?1049h", "[?2004h",
            "[?2026h", "]0;", "title",
        ] {
            assert!(!safe.contains(gone), "{gone:?} survived: {safe:?}");
        }
        // Every C1 and DEL byte too, which is where a payload's second spelling lives.
        for gone in ["\u{9b}", "\u{9c}", "\u{7f}", "\u{7}"] {
            assert!(!safe.contains(gone), "{gone:?} survived: {safe:?}");
        }
    }

    /// **Removing a sequence must not change what a wrapper counts**, and that is the
    /// whole reason this is a removal rather than a space: a well-formed sequence measures
    /// **no columns**. A space where the terminal measured none is five columns of garbage
    /// on a `[31m`, which is exactly how the regression broke the wrapping.
    #[test]
    fn removing_a_sequence_leaves_the_bytes_and_the_text_behind_it() {
        let hostile = "a\u{1b}[31mred\u{1b}[0m \u{1b}[8mdim \u{1b}[2Jclear";
        let safe = without_control(hostile);
        // The text in order, with nothing of any sequence between it.
        assert_eq!(safe, "ared dim clear");
        // The bytes went too — so this is a removal, not a substitution.
        assert_eq!(
            safe.len(),
            hostile.len() - "\u{1b}[31m\u{1b}[0m\u{1b}[8m\u{1b}[2J".len()
        );
    }

    /// A control byte that is **not** part of a sequence still becomes a space, because
    /// dropping it would silently reflow the line.
    #[test]
    fn a_lone_control_byte_becomes_a_space_rather_than_nothing() {
        assert_eq!(without_control("a\tb"), "a b");
        assert_eq!(without_control("a\rb"), "a b");
        assert_eq!(without_control("a\u{7f}b"), "a b");
        // A **bare C1 CSI** is the same instruction without its `ESC`, so it takes its
        // sequence with it: `\u{9b}31m` is `ESC[31m`.
        assert_eq!(without_control("a\u{9b}31mb"), "ab");
    }

    /// **`pieces` reports a colour and still removes everything else** — the second reader
    /// this module gained, and the one the operator's `! ls` needs.
    ///
    /// The rule that has to survive is the old one, restated for a reader that keeps
    /// something: **a sequence goes as one thing**. An SGR comes back as its parameters and
    /// nothing of its body is left as text; a mode string, an OSC title, a private form
    /// (`ESC[?25m` is not a colour) and a C1 control are removed exactly as before; and a
    /// parameter that is not a decimal number makes the whole sequence a non-SGR rather than
    /// half of one.
    #[test]
    fn pieces_reports_a_colour_and_still_removes_everything_else() {
        let line = "a\u{1b}[01;34msrc\u{1b}[0m \u{1b}[2J\u{1b}[?1002h \u{1b}]0;t\u{7} \u{1b}[?25m \u{9b}31m\u{9c} \u{7f}b";
        let got = pieces(line);
        let sgr: Vec<&Vec<u16>> = got
            .iter()
            .filter_map(|p| match p {
                Piece::Sgr(v) => Some(v),
                Piece::Text(_) => None,
            })
            .collect();
        assert_eq!(
            sgr,
            vec![&vec![1, 34], &vec![0], &vec![31]],
            "the colours are reported in order, and only the colours: {got:?}"
        );
        // The text between them, with no byte of any removed sequence in it.
        let text: String = got
            .iter()
            .filter_map(|p| match p {
                Piece::Text(t) => Some(t.as_str()),
                Piece::Sgr(_) => None,
            })
            .collect();
        assert_eq!(
            text, "asrc      b",
            "`a`, `src`, six spaces, `b` — and nothing else"
        );
        for gone in [
            "[?25m", "[2J", "[?1002h", "]0;", "\u{1b}", "\u{9b}", "\u{9c}",
        ] {
            assert!(!text.contains(gone), "{gone:?} survived as text: {text:?}");
        }
        // And the old reader is this one with the colours dropped, which is what keeps the
        // two from drifting: the same line, through both.
        assert_eq!(without_control(line), text);
    }

    /// **The pair that is the whole point**: the document form keeps its lines, and the
    /// single-line form flattens — which is why the two are two functions.
    #[test]
    fn the_document_form_keeps_the_lines_and_loses_the_controls() {
        let doc = "first paragraph\n\nsecond \u{1b}[31mred\u{1b}[0m and a tab\there\nthird\n";
        let safe = without_control_lines(doc);
        assert_eq!(
            safe.matches('\n').count(),
            doc.matches('\n').count(),
            "the line structure is the document: {safe:?}"
        );
        assert_eq!(safe.lines().count(), 4, "{safe:?}");
        assert_eq!(safe.lines().next(), Some("first paragraph"));
        assert_eq!(safe.lines().last(), Some("third"));
        assert!(
            safe.contains("second red and a tab here"),
            "the sequences go and the text stays: {safe:?}"
        );
        for c in safe.chars() {
            assert!(c == '\n' || !c.is_control(), "{c:?} survived: {safe:?}");
        }
        assert!(safe.ends_with('\n'), "a trailing newline is structure too");
        assert_eq!(without_control("a\nb"), "a b", "the line form flattens");
    }
}
