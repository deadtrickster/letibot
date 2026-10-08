//! **rano's lines as this head's row strings** — the one edge every `rano::agent` and
//! `rano::markdown` line crosses on its way to the frame.
//!
//! rano draws lines of role-tagged spans; this head still composes its frame out of ANSI
//! strings (`crate::ui::screen`, the row-diffing painter in `rano::term`). A row is written
//! one way: the line's own style (a tool row's dim register, a highlighted row's inverse, a
//! composer edge's faint) opened once, each styled span in its own look closed by a reset
//! *and the line's style again* — [`Line::to_ansi_inside`] — and a reset at the end. A line
//! with no style of its own is `to_ansi_inside` alone, each span opened and reset.
//!
//! A reset is not a restore. The string painters this replaced closed an inner span with a
//! plain reset inside a container, so the text after a code span in a quote was never faint,
//! a highlighted row was inverse only up to its first coloured mark, the turn's `Responding`
//! lost its colour after the spinner, and the separators of the composer's legend lost the
//! edge's faint. rano's lines say what was meant, and this writes what they say.

use rano::render::Line;
use rano::style::Palette;
use rano::width::text::RESET;

/// **One rano line as the head's row string.** See the module header.
pub fn row(l: &Line, p: Palette) -> String {
    let open = l.style.look(p).sgr();
    if open.is_empty() {
        return l.to_ansi_inside(p);
    }
    format!("{open}{}{RESET}", l.to_ansi_inside(p))
}

/// [`row`] for each of `lines`.
pub fn row_strings(lines: &[Line], p: Palette) -> Vec<String> {
    lines.iter().map(|l| row(l, p)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rano::render::{Span, Style};
    use rano::style::Role;

    /// A quote's spans each carry its faint, so the text after a code span is faint again.
    #[test]
    fn the_text_after_a_code_span_in_a_quote_is_still_faint() {
        let faint = Style::of(Role::Faint);
        let l = Line::new(vec![
            Span::styled("│ Note that ", faint.clone()),
            Span::styled("pred", faint.clone().role(Role::Code)),
            Span::styled(" is it", faint),
        ]);
        assert_eq!(
            row(&l, Palette::Colour),
            "\x1b[2m│ Note that \x1b[0m\x1b[2;36mpred\x1b[0m\x1b[2m is it\x1b[0m"
        );
        assert_eq!(row(&l, Palette::None), "│ Note that pred is it");
    }

    /// A highlighted row stays inverse past its coloured mark.
    #[test]
    fn a_highlight_holds_its_inverse_to_the_end_of_the_row() {
        let mut l = Line::new(vec![
            Span::raw("▸ "),
            Span::role("[~]", Role::Pending),
            Span::raw(" asked"),
        ]);
        l.style = Style::new().reverse();
        assert_eq!(
            row(&l, Palette::Colour),
            "\x1b[7m▸ \x1b[33m[~]\x1b[0m\x1b[7m asked\x1b[0m"
        );
    }

    /// A register line closes each coloured span back to its register — the composer's edge
    /// included, whose legend separators stay faint.
    #[test]
    fn a_register_restores_after_every_coloured_span() {
        let mut l = Line::new(vec![Span::raw("  out "), Span::role("red", Role::Failure)]);
        l.style = Style::of(Role::Faint);
        assert_eq!(
            row(&l, Palette::Colour),
            "\x1b[2m  out \x1b[31mred\x1b[0m\x1b[2m\x1b[0m"
        );
        assert_eq!(row(&l, Palette::None), "  out red");

        let mut l = Line::new(vec![
            Span::raw("╰── "),
            Span::role("⚠", Role::Attention),
            Span::raw(" · "),
            Span::role("holding", Role::Pending),
            Span::raw(" ─╯"),
        ]);
        l.style = Style::of(Role::Faint);
        assert_eq!(
            row(&l, Palette::Colour),
            "\x1b[2m╰── \x1b[1;33m⚠\x1b[0m\x1b[2m · \x1b[33mholding\x1b[0m\x1b[2m ─╯\x1b[0m"
        );
    }
}
