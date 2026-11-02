//! Text a head did not author, made safe for a terminal.
//!
//! Any control byte in content the head did not write is an instruction to the operator's terminal:
//! an OSC renames their window, `ESC [ 2 J` clears it, a mode string turns the mouse wheel off. It
//! lives here because both `letibot-sessionlog` and `letibot-ui` need it and neither can see the
//! other.
//!
//! Call it where foreign text enters -- a payload, a model's prose, a tool's reason -- never on a
//! string that may already carry the head's own escapes, or the head's own colour is stripped.
//!
//! [`without_control`] removes `\n` too, since it is a control character, and collapses a document
//! onto one line; [`without_control_lines`] is the one that means lines. [`pieces`] is the one walk
//! under both, reporting an SGR sequence rather than swallowing it.

use std::borrow::Cow;
use std::iter::Peekable;
use std::str::Chars;

/// One piece of a line, as a terminal would read it: the text it shows, or the SGR it obeys. The
/// walk reports SGR rather than leaving it as garbage, and one parser serves both readers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    /// Text a reader sees. A control byte that was **not** part of a sequence has
    /// already become a space here, by [`without_control`]'s own rule and for its reason:
    /// dropping it would silently reflow the line.
    Text(String),
    /// An SGR sequence's parameters, in order. A bare `ESC[m` and an empty parameter inside a
    /// longer sequence are the `0` ECMA-48 says they are.
    Sgr(Vec<u16>),
}

/// The line split into the two things a terminal does with it: text it shows and SGR it obeys.
/// Everything else is removed, and a sequence this cannot read as SGR is not reported as one.
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

/// Is there anything in here for the sanitiser to do? Exact rather than conservative: the slow path
/// only changes a string when it meets a control character, so a string with none comes out
/// identical and asking first risks nothing.
fn clean(s: &str) -> bool {
    !s.chars().any(char::is_control)
}

/// One line, with any escape sequence wholly removed and any other control byte replaced by a
/// space. Removal is the honest arithmetic: a well-formed sequence occupies no columns, so removing
/// it leaves the count where a terminal measured it, while a space would add one and corrupt the
/// wrap.
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

/// A document: the same, line by line, so its newlines survive. It borrows when it can, and it
/// usually can, because this runs once per model delta and prose has no control character in it.
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

/// The parameters of a `CSI` sequence, with the introducer consumed, and only when it is an SGR:
/// final byte `m`, every parameter a decimal number. `None` is not a failure -- a mode string is not
/// a colour and is removed.
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

    /// Removing a sequence must not change what a wrapper counts, which is why it is a removal and
    /// not a space: a well-formed sequence measures no columns.
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

    /// `pieces` reports a colour and still removes everything else. A sequence goes as one thing: an
    /// SGR comes back as its parameters, a mode string or an OSC is removed, and a parameter that is
    /// not a decimal number makes the whole sequence a non-SGR.
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
