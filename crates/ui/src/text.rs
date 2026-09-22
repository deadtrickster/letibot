//! **Text this head did not author, made safe for a terminal** (§3.1).
//!
//! A head composes rows and writes them verbatim, so any control byte in content it
//! did not write is an instruction to the operator's terminal: `ESC ] 0 ; x BEL`
//! renames the window they are working in, `ESC [ 2 J` clears it, `ESC [ ? 1002 h`
//! turns the mouse wheel off for the rest of the session. The operator's own store
//! measures the risk rather than arguing it: 44 `tool_result` rows carry an escape and
//! 20 carry a mode string.
//!
//! # Why this is here and not in a head
//!
//! It was in `letibot-tui` and the *UI crate* — which draws every card — had no
//! sanitiser at all, so a tool-progress note reached a card's tail raw. One definition
//! in the crate both sides already depend on is what stops the next renderer from being
//! the leaky one; the falsification test that found that hole sweeps both crates.
//!
//! # Two functions, and the difference is not cosmetic
//!
//! `\n` **is a control character** — `char::is_control` is true of every C0 code
//! including it — so passing a document to [`without_control`] does not sanitise it, it
//! **collapses it onto one line**: every paragraph, every list item and every fenced
//! block gone. That mistake is invisible on a one-line fixture and destroys every long
//! message, so the two cases are two named functions rather than one and a `.map`.

/// One line: control bytes become spaces.
///
/// A space rather than a deletion, which is the trade the tool-payload path made first:
/// dropping a character silently reflows the line, and the wrapper about to measure it
/// counts columns.
pub fn without_control(line: &str) -> String {
    line.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// A document: the same, **line by line**, so its newlines survive.
///
/// See this module's header for what the single-line version does to a document. A
/// trailing newline is structure too, so the split-and-rejoin keeps it.
pub fn without_control_lines(s: &str) -> String {
    s.split('\n')
        .map(without_control)
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pair that is the whole point: the lines are kept, the controls are not.
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
            safe.contains("second  [31mred [0m and a tab here"),
            "controls become spaces: {safe:?}"
        );
        for c in safe.chars() {
            assert!(c == '\n' || !c.is_control(), "{c:?} survived: {safe:?}");
        }
        assert!(safe.ends_with('\n'), "a trailing newline is structure too");
    }

    /// **The one-line form still flattens**, and that is why the two are not merged:
    /// its callers split first (`payload.lines()`), so each call sees one line.
    #[test]
    fn the_line_form_flattens_and_that_is_its_contract() {
        assert_eq!(without_control("a\tb"), "a b");
        assert_eq!(without_control("a\nb"), "a b");
        // C1 and DEL are control characters too, which is what makes this sufficient
        // for the C1 forms a real payload carries (`ESC` is C0, `\u{9b}` is C1).
        for c in ['\u{1b}', '\u{7}', '\u{7f}', '\u{9b}', '\u{9c}', '\u{0}'] {
            assert!(c.is_control(), "{c:?} must be classified as a control character");
            assert_eq!(without_control(&c.to_string()), " ", "{c:?}");
        }
    }
}
