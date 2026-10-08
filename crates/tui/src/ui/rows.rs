//! **rano's lines as this head's row strings** — the one edge every `rano::agent` and
//! `rano::markdown` line crosses on its way to the frame.
//!
//! rano draws lines of role-tagged spans; this head still composes its frame out of ANSI
//! strings (`crate::ui::screen`, the row-diffing painter in `rano::term`). The strings it
//! printed before the painters moved to rano were written by a family of string painters —
//! `Palette::painted`, `Painter::inside`, `colour(cfg, …)` — and each of them wrote a span
//! a particular way. A rano line carries enough of the structure to write the same bytes
//! again, and this module is where that is done, so that the move changed nothing a person
//! sees (and nothing the head's tests, which pin bytes, read).
//!
//! # The conventions, and where each came from
//!
//! - **A span on its own** is its look opened and a reset after it: `Palette::painted`.
//!   That is [`Line::to_ansi_inside`] for a line with no style.
//! - **A line with a register** (a tool row's dim payload) is the register opened once, its
//!   spans closing back to it with a reset *and the register again*, and a reset at the end:
//!   `dim(cfg, …)` around a `Painter::inside` line.
//! - **A container run** — consecutive spans that share a bottom role, at least one of them
//!   carrying more on top of it (a code span in a quote, a bold word in a heading, the
//!   spinner in the turn's pending row) — is the container opened once, the plain spans bare,
//!   each stacked span in its own look closed by a *plain* reset, and a reset at the end:
//!   `p.painted(container, &inner)` with the inner spans painted by `Palette::painted`.
//! - **A run of reversed spans** (a highlighted row) is one inverse with its spans inside it
//!   written the same way: `colour(cfg, REVERSE, row)`.
//!
//! The last two keep a defect, and keep it on purpose. The inner span's plain reset ends the
//! container too, so the text after a code span in a quote was never faint, the subagents
//! pane's highlighted row is inverse only up to its state mark, and the turn's `Responding`
//! is not yellow after its spinner — *a reset is not a restore*, the very defect
//! `Painter::inside` was written for, left standing where the string painters did not use
//! it. rano's lines say what was meant (the whole quote faint, the whole row inverse), and a
//! cell buffer draws that; switching this module to [`Line::to_ansi_inside`] is the one-line
//! fix for all of them at once, and it is a change to what the screen shows, which this move
//! is not.
//!
//! The tints of a diff (`Role::Added`, `Role::Removed`) are never a container here: letibot's
//! diff painted inside them with a restore, and rano's diff writes each span's look whole,
//! which is the same cells (the padding now tinted to the panel's edge, as letibot's sidediff
//! documented and did not do).

use rano::render::{Line, Span, Style};
use rano::style::{Attrs, Palette, Role};
use rano::width::text::RESET;

/// **One rano line as the head's row string.** See the module header for the conventions.
pub fn row(l: &Line, p: Palette) -> String {
    let line_look = l.style.look(p);
    if line_look.attrs.contains(Attrs::REVERSE) {
        // A highlighted row: the inverse is the line's, and it is written as a run over its
        // spans, so the spans are given the line's style first.
        let spans: Vec<Span> = l
            .spans
            .iter()
            .map(|s| Span::styled(s.content.clone(), l.style.patch(&s.style)))
            .collect();
        return spans_to_ansi(&spans, None, p);
    }
    let open = line_look.sgr();
    if !open.is_empty() {
        return format!("{open}{}{RESET}", l.to_ansi_inside(p));
    }
    spans_to_ansi(&l.spans, Some(l), p)
}

/// [`row`] for each of `lines`.
pub fn row_strings(lines: &[Line], p: Palette) -> Vec<String> {
    lines.iter().map(|l| row(l, p)).collect()
}

fn reversed(sp: &Span, p: Palette) -> bool {
    sp.style.look(p).attrs.contains(Attrs::REVERSE)
}

/// The role at the bottom of a span's stack, when it can be a container: not a diff's tint.
fn container_of(sp: &Span) -> Option<Role> {
    sp.style
        .roles()
        .next()
        .filter(|r| !matches!(r, Role::Added | Role::Removed))
}

/// The span's style with its container taken off the bottom, and the attributes the
/// container already gives taken off the top: what the span adds.
fn own(sp: &Span, c: Role, p: Palette) -> Style {
    let mut s = Style::new();
    for r in sp.style.roles().skip(1) {
        s = s.role(r);
    }
    s.attrs = sp.style.attrs.without(p.look(c).attrs);
    s.link = sp.style.link.clone();
    s.raw = sp.style.raw;
    s
}

/// The span carries more than its container.
fn stacked(sp: &Span, c: Role, p: Palette) -> bool {
    sp.style.roles().count() > 1 || !sp.style.attrs.without(p.look(c).attrs).is_empty()
}

/// One span written on its own: its look, the text, a reset.
fn one(out: &mut String, content: &str, style: Style, p: Palette) {
    out.push_str(&Line::new(vec![Span::styled(content, style)]).to_ansi_inside(p));
}

/// `whole` is the line `spans` are, when they are one: the fast path then writes it without
/// copying its spans, which is an allocation per span per row per frame otherwise.
fn spans_to_ansi(spans: &[Span], whole: Option<&Line>, p: Palette) -> String {
    let as_is = || match whole {
        Some(l) => l.to_ansi_inside(p),
        None => Line::new(spans.to_vec()).to_ansi_inside(p),
    };
    // No palette, no sequences: the text (and nothing else) is what a replay and CI compare.
    if p == Palette::None {
        return as_is();
    }
    // **The fast path, and the common one**: nothing reversed and nothing stacked, so every
    // span is written on its own and the line is `to_ansi_inside` exactly. This runs for
    // every row of every frame.
    let plain_shape = !spans
        .iter()
        .any(|s| reversed(s, p) || (container_of(s).is_some_and(|c| stacked(s, c, p))));
    if plain_shape {
        return as_is();
    }
    let mut out = String::new();
    let mut i = 0;
    while i < spans.len() {
        if reversed(&spans[i], p) {
            // A highlighted run: one inverse, the spans inside in their own look with a
            // plain reset, one reset at the end.
            out.push_str(&p.reverse_look().sgr());
            while i < spans.len() && reversed(&spans[i], p) {
                let sp = &spans[i];
                let mut look = sp.style.look(p);
                look.attrs = look.attrs.without(Attrs::REVERSE);
                let seq = look.sgr();
                let text = Line::raw(sp.content.clone()).plain();
                if seq.is_empty() {
                    out.push_str(&text);
                } else {
                    out.push_str(&seq);
                    out.push_str(&text);
                    out.push_str(RESET);
                }
                i += 1;
            }
            out.push_str(RESET);
            continue;
        }
        if let Some(c) = container_of(&spans[i]) {
            let mut j = i;
            while j < spans.len() && !reversed(&spans[j], p) && container_of(&spans[j]) == Some(c) {
                j += 1;
            }
            if j - i >= 2 && spans[i..j].iter().any(|s| stacked(s, c, p)) {
                out.push_str(&p.look(c).sgr());
                for sp in &spans[i..j] {
                    one(&mut out, &sp.content, own(sp, c, p), p);
                }
                out.push_str(RESET);
                i = j;
                continue;
            }
        }
        one(&mut out, &spans[i].content, spans[i].style.clone(), p);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// letibot's quote row: the rail and text in one faint run, the code span inside it
    /// closed with a plain reset — the bytes `cfg.c(DIM, "│ {l}")` around a painted run wrote.
    #[test]
    fn a_container_run_is_written_as_the_string_painter_nested_it() {
        let faint = Style::of(Role::Faint);
        let l = Line::new(vec![
            Span::styled("│ Note that ", faint.clone()),
            Span::styled("pred", faint.clone().role(Role::Code)),
            Span::styled(" is it", faint.clone()),
        ]);
        assert_eq!(
            row(&l, Palette::Colour),
            "\x1b[2m│ Note that \x1b[36mpred\x1b[0m is it\x1b[0m"
        );
        // Two faint runs side by side with nothing stacked are two painted spans.
        let l = Line::new(vec![
            Span::styled("▾", faint.clone()),
            Span::styled(" Ran ", faint),
        ]);
        assert_eq!(
            row(&l, Palette::Colour),
            "\x1b[2m▾\x1b[0m\x1b[2m Ran \x1b[0m"
        );
    }

    /// A highlighted row is one inverse; a register line closes back to its register.
    #[test]
    fn a_highlight_is_one_inverse_and_a_register_restores() {
        let mut l = Line::new(vec![
            Span::raw("▸ "),
            Span::role("[~]", Role::Pending),
            Span::raw(" asked"),
        ]);
        l.style = Style::new().reverse();
        assert_eq!(
            row(&l, Palette::Colour),
            "\x1b[7m▸ \x1b[33m[~]\x1b[0m asked\x1b[0m"
        );
        let mut l = Line::new(vec![Span::raw("  out "), Span::role("red", Role::Failure)]);
        l.style = Style::of(Role::Faint);
        assert_eq!(
            row(&l, Palette::Colour),
            "\x1b[2m  out \x1b[31mred\x1b[0m\x1b[2m\x1b[0m"
        );
        assert_eq!(row(&l, Palette::None), "  out red");
    }
}
