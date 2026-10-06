//! **Colour a foreign program asked for, drawn in this head's own roles** (§3.1, second half).
//!
//! # The requirement, in the operator's words
//!
//! *"i run `! ls -la` and the output is plain, while in a proper terminal directory names
//! are highlighted."* The first half of that is the run — a pipe is not a tty, so
//! `ls --color=auto` printed plain, and `letibot_tools::exec::pty` is what fixed it. This
//! is the second half, and without it the fix would be worse than the defect: `ls` would
//! write `ESC[01;34m` into a payload, and the head's sanitiser would **delete it** — the
//! colour would arrive and be thrown away, which is exactly the guarantee
//! `letibot_transcript::sanitize` was written to give and the wrong answer for a row a
//! person is reading.
//!
//! # What this does, and what it refuses to do
//!
//! It walks a line with that sanitiser's own parser ([`letibot_transcript::sanitize::pieces`])
//! and turns each SGR sequence into **the palette role that means the same thing**, painted
//! through a [`Painter`] so a span closes back into the block it sits in rather than to the
//! terminal's default. Nothing else about the line changes: a mode string, an OSC title, a
//! C1 control and a DEL are still removed, and a row that carried `ESC[?1002h` before still
//! cannot turn the operator's mouse wheel off.
//!
//! **No sequence is ever passed through.** The bytes on the frame come from [`Palette`] and
//! from nowhere else — a foreign program's exact escape never reaches the terminal, however
//! well-formed it is. That is the whole of the safety argument: what a command can say about
//! *colour* is now a closed set of roles this head chose, and what it can say about anything
//! else is nothing.
//!
//! **That is a statement about this function's output, and it is worth saying which caller's
//! row it lands on.** There is exactly one renderer of a tool payload in the head —
//! `letibot-tui`'s `item_lines`, in its `ToolResult` arm — and it draws the payload **twice**
//! through two different readers:
//!
//! - **The body**, folded and in the window `ctrl-v` opens, is the one that calls [`painted`].
//!   This is the path the operator's `! ls -la` takes: `ls`'s `\u{1b}[01;34m` arrives as
//!   [`Role::Subheading`]'s own `\u{1b}[1;34m`, which is the same bold blue and this head's
//!   sequence rather than the program's. A four-line payload is always this path, and a
//!   payload whose first line has no colour on it — `ls -la`'s `total 124` — shows no colour
//!   until the window is opened, which is a property of the fold and not of the paint.
//! - **The one-line header form**, for a payload of exactly one line, draws
//!   [`letibot_transcript::sanitize::without_control`]'s text instead: the escapes go whole
//!   and the colour goes with them, so a one-line `! ls` is drawn plain where a four-line
//!   `! ls -la` is drawn coloured. That is a **loss**, not a passthrough — nothing of the
//!   program's sequence reaches the frame either way — and it is the only place on the
//!   operator's own row where the two readers disagree.
//!
//! So *"no sequence is ever passed through"* holds on both paths, and it is **not** the same
//! claim as *"every payload is painted"*. The second is false, in the one place named above.
//!
//! # The mapping, and why it is a table rather than a translation
//!
//! There is no role per SGR code, and there should not be: a [`Role`] is a *meaning*
//! ("something failed", "this is syntax") and the sixteen colours are a *theme*. What the
//! table does is put a program's colour where this head already puts that colour when it has
//! something of its own to say:
//!
//! | the program says | the role | which is |
//! |---|---|---|
//! | `31` / `91` red | [`Role::Failure`] | `31` |
//! | `32` / `92` green | [`Role::Success`] | `32` |
//! | `33` / `93` yellow | [`Role::Pending`] | `33` |
//! | `1;33` bold yellow | [`Role::Attention`] | `1;33` |
//! | `34` / `94` blue | [`Role::FuncName`] | `34` |
//! | `1;34` bold blue | [`Role::Subheading`] | `1;34` |
//! | `35` / `95` magenta | [`Role::Keyword`] | `35` |
//! | `36` / `96` cyan | [`Role::Code`] | `36` |
//! | `1;36` bold cyan | [`Role::Heading`] | `1;36` |
//! | `1` bold alone | [`Role::Strong`] | `1` |
//! | `2` dim alone | [`Role::Faint`] | `2` |
//! | `0`, `39`, `22` | nothing — the run ends | — |
//!
//! **`ls`'s own defaults land on the second and sixth rows**, which is the case that was
//! reported: a directory is `01;34` and comes out bold blue, an executable `01;32` comes out
//! green, and a symlink `01;36` comes out bold cyan. `grep --color`'s match is `01;31` and
//! comes out red.
//!
//! Bold and colour **compose to the bold role of that hue where one exists** and the bold is
//! dropped where it does not, because the palette's two shades of one colour are an
//! *attribute* and not a second hue (see `style.rs`'s header). A bright colour (`9x`) takes
//! its normal-intensity role for the same reason: there is no role per slot, and inventing
//! one here would be this module choosing a colour rather than translating one.
//!
//! # What is dropped, and why that is the honest answer
//!
//! - **A 256-colour or truecolour value** (`38;5;167`, `48;2;r;g;b`). The cube's indices are
//!   absolute RGB: painting one would put a colour *beside* the reader's theme rather than
//!   within it, which is the mistake `style.rs` records having made once already. The whole
//!   extended parameter is consumed and nothing is painted, so `38;5;1` can never be misread
//!   as the `1` that means bold.
//! - **A background** (`40`–`47`, `100`–`107`). A row already sits on a block this head
//!   chose; a program's background would fight it, and `ls`'s directory colours are
//!   foregrounds in any case.
//! - **Anything else** — underline, blink, conceal, reverse. No role means those, and a
//!   sequence this module does not understand is consumed rather than forwarded.
//!
//! # Per line, and that is deliberate
//!
//! Colour does not cross a line here. Each line is painted and closed on its own, so a line
//! whose colour runs to its end cannot tint the next line's prefix — and a head draws a
//! payload line with a gutter (`  `) in front of it, which is exactly what a leaked colour
//! would ruin. A terminal would carry the state; a row list must not.

use crate::style::{Painter, Role};

/// One line of a foreign program's output, with its SGR drawn as this head's roles and
/// every other control byte dropped.
///
/// `p` is the painter for the block the line is going into — [`Painter::inside`] where the
/// row paints its payload in [`Role::Faint`], so a coloured run ends by restoring the dim
/// body rather than the terminal's default.
pub fn painted(p: Painter, line: &str) -> String {
    // The fast path, and the common one: a line with no control character in it has no
    // sequence to interpret, and this is called once per payload line of every tool row
    // on the screen.
    if !line.chars().any(char::is_control) {
        return line.to_string();
    }
    let mut out = String::with_capacity(line.len());
    let mut want = Wanted::default();
    let mut open = false;
    for piece in letibot_transcript::sanitize::pieces(line) {
        let params = match piece {
            letibot_transcript::sanitize::Piece::Text(t) => {
                out.push_str(&t);
                continue;
            }
            letibot_transcript::sanitize::Piece::Sgr(params) => params,
        };
        apply(&params, &mut want);
        let role = want.role();
        // **Only a change is painted.** `ls` writes `ESC[0m` before every name and a reset
        // after it, and a head that emitted a span per sequence would put four sequences on
        // a line that needs two — and would close a span that was never opened.
        match (open, role) {
            (true, None) => {
                out.push_str(&p.close());
                open = false;
            }
            (false, Some(r)) => {
                out.push_str(p.open(r));
                open = true;
            }
            (true, Some(r)) if Some(r) != want.last => {
                out.push_str(&p.close());
                out.push_str(p.open(r));
            }
            _ => {}
        }
        want.last = role;
    }
    // **A line that ends coloured is closed here.** A pty's state would run on into the
    // next row's gutter; a row list's must not.
    if open {
        out.push_str(&p.close());
    }
    out
}

/// What the SGR seen so far on this line asks for, reduced to the three things a role can
/// be built from.
///
/// **`pub` for one reason: it is the pen [`crate::vt::Screen`] holds, and a screen's
/// SGR state is this state.** A cell keeps the role the pen had when the program wrote
/// it, so a second table for the screen would be a second answer to *what does `1;33`
/// mean* — the exact drift [`crate::ansi`]'s module header exists to prevent. The
/// fields stay private: a caller folds sequences in with [`apply`] and reads the answer
/// out with [`Wanted::role`], which is the whole of the interface either caller needs.
#[derive(Default, Clone, Copy)]
pub struct Wanted {
    bold: bool,
    faint: bool,
    colour: Option<Colour>,
    /// The role the last painted run used, so a sequence that changes nothing paints
    /// nothing.
    ///
    /// A run's bookkeeping rather than a pen's, so [`crate::vt`] never reads it: a
    /// screen's runs are grouped at `Screen::lines` time, from the roles the cells hold.
    last: Option<Role>,
}

/// The six colours this module has a role for. The sixteen slots a theme defines are
/// reached through [`Role`], and this is the subset of them that means something.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Colour {
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
}

impl Wanted {
    /// The role this run is painted in, or `None` for the block's own style.
    pub fn role(self) -> Option<Role> {
        match (self.bold, self.colour) {
            (_, Some(Colour::Red)) => Some(Role::Failure),
            (_, Some(Colour::Green)) => Some(Role::Success),
            (true, Some(Colour::Yellow)) => Some(Role::Attention),
            (false, Some(Colour::Yellow)) => Some(Role::Pending),
            (true, Some(Colour::Blue)) => Some(Role::Subheading),
            (false, Some(Colour::Blue)) => Some(Role::FuncName),
            (_, Some(Colour::Magenta)) => Some(Role::Keyword),
            (true, Some(Colour::Cyan)) => Some(Role::Heading),
            (false, Some(Colour::Cyan)) => Some(Role::Code),
            (true, None) => Some(Role::Strong),
            // Faint only when it is all there is: no role is both dim and coloured, and
            // the colour is the part a program meant.
            (false, None) if self.faint => Some(Role::Faint),
            (false, None) => None,
        }
    }
}

/// Fold one SGR sequence's parameters into `want`.
///
/// A parameter this does not know is **ignored and the rest of the sequence still
/// applies**: `4;31` is an underline this head has no role for and a red it does, and
/// dropping the red over the underline would be the worse answer.
pub fn apply(params: &[u16], want: &mut Wanted) {
    let mut i = 0usize;
    while i < params.len() {
        let p = params[i];
        i += 1;
        match p {
            // Reset, and the two "back to normal" codes a program writes instead of it.
            0 => *want = Wanted::default(),
            22 => {
                want.bold = false;
                want.faint = false;
            }
            1 => want.bold = true,
            2 => want.faint = true,
            30..=37 | 90..=97 => {
                want.colour = match p % 10 {
                    1 => Some(Colour::Red),
                    2 => Some(Colour::Green),
                    3 => Some(Colour::Yellow),
                    4 => Some(Colour::Blue),
                    5 => Some(Colour::Magenta),
                    6 => Some(Colour::Cyan),
                    // Black and white are not roles. `30` is invisible on half the themes
                    // this tree is read on and `37`/`97` are the body text's own colour, so
                    // both mean "no colour of mine".
                    _ => None,
                };
            }
            // The default foreground, which is where a reset of the colour alone goes.
            39 => want.colour = None,
            // **An extended colour is consumed whole and paints nothing.** `38;5;n` and
            // `38;2;r;g;b` carry parameters that are *not* SGR codes, and reading them one
            // by one would take the `1` of `38;5;1` for a bold — which is a colour mistake
            // this module would be making rather than the program.
            38 | 48 => match params.get(i) {
                Some(5) => i += 2,
                Some(2) => i += 4,
                _ => i += 1,
            },
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::Palette;

    fn colour() -> Painter {
        Painter::new(Palette::Colour)
    }

    /// **The case that was reported, and the whole of the requirement**: what a colour-aware
    /// program writes comes out as the roles this head already draws those colours with.
    ///
    /// Every sequence here is one `ls`, `grep` or `cargo` actually writes, and the expected
    /// bytes are `Palette::open`'s own — so the assertion is not "there is an escape in the
    /// output" but "the escape is the one this head chose for that meaning".
    #[test]
    fn what_a_foreign_program_colours_becomes_the_palette_role_for_it() {
        // `ls`'s own default for a directory, which is the line the operator was looking at.
        assert_eq!(
            painted(colour(), "\u{1b}[01;34msrc\u{1b}[0m"),
            span(Role::Subheading, "src")
        );
        // Red, green, yellow, magenta and cyan at normal intensity.
        for (code, role) in [
            ("31", Role::Failure),
            ("32", Role::Success),
            ("33", Role::Pending),
            ("35", Role::Keyword),
            ("36", Role::Code),
        ] {
            assert_eq!(
                painted(colour(), &format!("\u{1b}[{code}mX\u{1b}[0m")),
                span(role, "X"),
                "`{code}` is not painted as {}",
                Palette::Colour.open(role)
            );
        }
        // The bright forms take the same role: there is no second shade in the palette, and
        // inventing one here would be this module choosing a colour rather than translating
        // one. A bare `m` is the `0` ECMA-48 says it is.
        assert_eq!(
            painted(colour(), "\u{1b}[91mX\u{1b}[m"),
            span(Role::Failure, "X")
        );
        // Bold alone, and bold composed with a hue that has a bold role of its own.
        assert_eq!(
            painted(colour(), "\u{1b}[1mX\u{1b}[0m"),
            span(Role::Strong, "X")
        );
        assert_eq!(
            painted(colour(), "\u{1b}[1;33mX\u{1b}[0m"),
            span(Role::Attention, "X")
        );
        // Dim alone.
        assert_eq!(
            painted(colour(), "\u{1b}[2mX\u{1b}[0m"),
            span(Role::Faint, "X")
        );
    }

    /// One run, painted: the role's own opening sequence, the text, and a reset — which is
    /// what `Painter::paint` produces at the top level, spelled out so the test asserts on
    /// bytes rather than on another call into the thing it is testing.
    fn span(role: Role, text: &str) -> String {
        format!(
            "{}{text}{}",
            Palette::Colour.open(role),
            crate::width::RESET
        )
    }

    /// **`grep --color`'s own sequence**, which is the other half of the operator's report:
    /// a match is `01;31` and comes out red, and the text around it is untouched.
    #[test]
    fn a_grep_match_keeps_its_red_and_the_text_around_it_its_own_colour() {
        assert_eq!(
            painted(colour(), "src/\u{1b}[01;31ma.rs\u{1b}[0m:12:fn a()"),
            format!("src/{}:12:fn a()", span(Role::Failure, "a.rs"))
        );
    }

    /// **The operator's own `ls -la`, as `ls` actually writes it** — not the fixture.
    ///
    /// The synthetic payload the first cut used was `"\u{1b}[01;34msrc\u{1b}[0m"`: a colour,
    /// the text, a close. `ls` writes **three** sequences on a directory row and the middle one
    /// is a reset *before* the colour — `\u{1b}[0m\u{1b}[01;34m.\u{1b}[0m` — because it restores
    /// the default between entries. That extra leading `0` is the whole reason the operator's
    /// paste reads `[0m[01;34m.`: the two bodies sit next to each other with no visible
    /// introducer between them, which is what makes a paste look like "the escapes are printed
    /// as text". The walk has to see it as *reset then open* and paint nothing for the first —
    /// which is what the assertion on the exact bytes below holds.
    #[test]
    fn the_operators_own_ls_line_is_painted_from_its_real_bytes() {
        // The directory row verbatim, `ls`'s own reset-before-colour included.
        assert_eq!(
            painted(colour(), "\u{1b}[0m\u{1b}[01;34m.\u{1b}[0m"),
            span(Role::Subheading, "."),
            "a reset before a colour is a no-op and the colour is still painted"
        );
        // The header line of the same capture carries no escape at all, and is handed back as
        // it stands — the fast path, which is what makes the per-line call affordable.
        let header = "total 124";
        assert_eq!(painted(colour(), header), header);
        // And the sequence bodies are nowhere in the painted line: no `[` a reader could see.
        let out = painted(colour(), "\u{1b}[0m\u{1b}[01;34m.\u{1b}[0m");
        let seen: String = letibot_transcript::sanitize::without_control(&out);
        assert!(
            !seen.contains('['),
            "a sequence body survived as text: {seen:?}"
        );
        assert_eq!(seen, ".");
    }

    /// **A colour that runs to the end of the line is closed at the end of the line.**
    ///
    /// A terminal carries SGR state across a newline and a head must not: the next row is
    /// drawn with a gutter this head wrote, and a leaked red would tint it. The assertion is
    /// on the trailing bytes, which is where a leak would be visible.
    #[test]
    fn a_run_that_reaches_the_end_of_the_line_is_closed_before_the_next_one() {
        let out = painted(colour(), "src/\u{1b}[31mno reset here");
        assert!(
            out.ends_with(crate::width::RESET),
            "the line must not leave a colour open: {out:?}"
        );
        assert_eq!(
            painted(colour(), "\u{1b}[31mX"),
            format!(
                "{}X{}",
                Palette::Colour.open(Role::Failure),
                crate::width::RESET
            )
        );
    }

    /// **A sequence this head has no role for paints nothing and is still consumed.**
    ///
    /// The extended-colour case is the one with a trap in it: `38;5;1` is the cube's index 1
    /// and its last parameter is the `1` that means bold. Reading the parameters one at a
    /// time would turn a red the palette cannot name into a bold this head would be
    /// inventing, so the whole extended form goes — and the text either side is kept.
    #[test]
    fn a_colour_outside_the_palette_is_dropped_rather_than_guessed_at() {
        for hostile in [
            "\u{1b}[38;5;167mX\u{1b}[0m",
            "\u{1b}[38;2;255;0;0mX\u{1b}[0m",
            "\u{1b}[48;5;52mX\u{1b}[0m",
            "\u{1b}[4mX\u{1b}[0m",
            "\u{1b}[7mX\u{1b}[0m",
        ] {
            assert_eq!(painted(colour(), hostile), "X", "{hostile:?}");
        }
    }

    /// **The existing guarantee, re-asserted where the colour now enters**: everything that
    /// is not SGR is still removed whole, and no byte of any sequence survives as text.
    ///
    /// **The assertion is not "no `ESC` on the line"** — the head's own escapes are there,
    /// by design, and the first version of this test failed on exactly that. What is
    /// asserted is the property that matters and that the whole of §3.1 is about: every
    /// escape in the output is **one this head chose**, and once those are removed nothing
    /// is left that a terminal would act on.
    #[test]
    fn a_control_byte_that_is_not_a_colour_is_still_dropped() {
        let hostile = "a\u{1b}[2J\u{1b}[?1002h\u{1b}[?1006h\u{1b}[?1049h\u{1b}[?2004h\
                       \u{1b}]0;pwned\u{7}\u{9b}31m\u{9c}\u{7f}b";
        let out = painted(colour(), hostile);
        // Not one byte of any hostile sequence, as text or as an escape.
        for gone in [
            "[2J", "[?1002h", "[?1006h", "[?1049h", "[?2004h", "]0;", "pwned", "\u{7}", "\u{9b}",
            "\u{9c}", "\u{7f}",
        ] {
            assert!(!out.contains(gone), "{gone:?} survived: {out:?}");
        }
        // Take this head's own vocabulary off the line, and there is no control byte left:
        // the `\u{9b}31m` in the middle was a colour, so `Failure`'s sequence is what it
        // became, and it is the only escape the output carries.
        let mut rest = out.clone();
        for own in [Palette::Colour.open(Role::Failure), crate::width::RESET] {
            rest = rest.replace(own, "");
        }
        assert!(
            !rest.chars().any(char::is_control),
            "a control byte reached the line that this head did not choose: {rest:?}"
        );
        assert_eq!(
            rest, "a b",
            "the text around the sequences is kept, and the lone DEL is a space"
        );
        assert_eq!(
            out.matches(Palette::Colour.open(Role::Failure)).count(),
            1,
            "the C1 form of a colour is a colour: {out:?}"
        );
    }

    /// **A no-colour head is byte-identical to the sanitiser it replaces.**
    ///
    /// `Palette::None` is the `--replay`, pipe-to-a-file and CI case, and it has to produce
    /// the same bytes on every machine: with no palette there is nothing to paint, and the
    /// line is exactly what `without_control` would have given.
    #[test]
    fn under_the_none_palette_the_line_is_what_the_sanitiser_alone_would_give() {
        let line = "src/\u{1b}[01;34mthe-dir\u{1b}[0m and \u{1b}[2J clear \u{1b}[?1002h";
        assert_eq!(
            painted(Painter::new(Palette::None), line),
            letibot_transcript::sanitize::without_control(line)
        );
    }

    /// **A line with nothing to interpret is handed back untouched**, which is what keeps
    /// this affordable on the per-payload-line path.
    #[test]
    fn an_ordinary_line_is_returned_as_it_stands() {
        let line = "  -rw-r--r-- 1 dead dead 4096 Oct  6 10:29 Cargo.toml";
        assert_eq!(painted(colour(), line), line);
    }

    /// **Inside a themed block, a coloured run closes back into the block.** The payload is
    /// drawn dim, so a run that closed to the terminal's default would make the rest of the
    /// row brighter than the line it is part of — the defect `Painter` exists for. The close
    /// is a reset **plus the block's own opening sequence**, which is why a `Painter` is
    /// what this module takes rather than a `Palette`.
    #[test]
    fn a_coloured_run_closes_back_into_the_block_it_is_drawn_in() {
        let p = Painter::inside(Palette::Colour, Role::Faint);
        let out = painted(p, "\u{1b}[31mred\u{1b}[0m tail");
        assert_eq!(
            out,
            format!("{}red{} tail", p.open(Role::Failure), p.close())
        );
        // And the block's own style is re-established, not the terminal's default.
        assert!(
            out.contains(&format!("{}{}", crate::width::RESET, p.open(Role::Faint))),
            "the run closed to the terminal rather than to the block: {out:?}"
        );
    }
}
