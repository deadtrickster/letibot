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
//! # One walk, two readers, and which half is whose
//!
//! What an SGR parameter list *sets* — a foreground slot, bold, dim, reverse, and an extended
//! colour consumed whole — is `letibot_vt::attr`'s, because **a screen is a second reader of the
//! same parameters**: `Screen::feed` reads a program's whole byte stream into the same pen. That
//! walk used to live here as `Wanted`/`apply`, and moving it down is what keeps `38;5;1` from
//! meaning two things in one process.
//!
//! What is left here is the half that is the *head's*: **which role a pen is drawn as**. That is a
//! table from a hue and a weight to a [`Role`], and a role is a meaning in this head's own
//! vocabulary — `Role::Failure` is `31` in this palette and nowhere else — so it stays where the
//! palette is. The tests below are the proof the move changed nothing: they are byte-exact, they
//! were not touched, and they include the operator's own `ls -la`.
//!
//! # The same table, asked of a grid
//!
//! [`lines`] is that second reader: a [`Screen`]'s cells turned into the rows a pane draws. It asks
//! the *same* [`role`], which is the whole of why the walk moved down — `ls`'s `01;34` on a payload
//! row and `mc`'s `1;34` on a screen come out as one [`Role::Subheading`] and there is one answer in
//! the process to what a pen is drawn as. [`pane_rows`] is the pane's own entry point, and the one
//! property its caller depends on is in its signature: **exactly `room` rows**.
//!
//! **Two things differ between the readers, and both are deliberate.** A program's *background* and
//! its *reverse* are painted by [`lines`] and dropped by [`painted`]: a payload row sits inside a
//! block this head chose and its colour is a meaning, while a screen **is** the program's own
//! drawing — `mc`'s panels, `less`'s status bar and `nano`'s status bar are backgrounds and
//! reverses, and a pane that dropped them would draw their text on the transcript's own background
//! and lose the selected row. Both are consumed through the same pen and both come out of
//! [`Palette`], so there is still exactly one walk and one table of sequences.
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
//! The rows are read off the **pen** rather than off the parameters: `letibot_vt::attr` has already
//! applied them, so what this table asks is *"what is the pen now"* and not *"what did the program
//! just write"*. A hue and a weight are the whole of it, which is why `Attr::hue` takes the
//! intensity off — a bright red and a red are the same role here, for the same reason a bold blue
//! and a blue are not.
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
//!   as the `1` that means bold. **The consumption is `letibot_vt::attr`'s**, which is the same
//!   rule the screen needs for the same reason.
//! - **A background** (`40`–`47`, `100`–`107`). Consumed by the walk — `letibot_vt::attr` carries
//!   the slot — and **dropped here, on the payload path only**. A payload row already sits on a
//!   block this head chose and a program's background beside it would fight it, and `ls`'s
//!   directory colours are foregrounds in any case. The screen is the reader that keeps it:
//!   `mc`'s panels and `nano`'s status bar are backgrounds, and [`lines`] paints them from the same
//!   pen. So the two readers of one parameter list disagree about exactly this, deliberately, and
//!   the disagreement is one line of code in [`lines`] rather than a second walk.
//! - **Reverse** (`7`). Carried by the pen — a *screen* needs it, and `mc`'s selected row is one —
//!   and **dropped here**, for the same reason: the row already sits in a block and in a palette
//!   slot the head chose, so a `\u{1b}[7m` in a payload paints nothing here, exactly as it did
//!   before the pen learned the attribute. [`lines`] draws it. `Role::UserBlock` is reverse as
//!   well, and that is a coincidence of the palette rather than a mapping: a program's reverse is
//!   not this head's raised user block, and `Palette::reverse` is the one spelling of it.
//! - **Underline, italic, blink, conceal, strike, a font.** No field on the pen and no role here,
//!   so a program that underlines a menu accelerator draws it plain on both readers.
//!
//! # Per line, and that is deliberate
//!
//! Colour does not cross a line here. Each line is painted and closed on its own, so a line
//! whose colour runs to its end cannot tint the next line's prefix — and a head draws a
//! payload line with a gutter (`  `) in front of it, which is exactly what a leaked colour
//! would ruin. A terminal would carry the state; a row list must not.

use crate::painter::Painter;
use letibot_vt::Screen;
use letibot_vt::attr::{Attr, Hue, apply_sgr};
use rano::style::{Palette, Role};

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
    // The pen the program has asked for so far on this line. `letibot_vt::attr` owns what a
    // parameter list *sets*; this function owns what a pen is *painted as*.
    let mut pen = Attr::default();
    let mut open = false;
    // The role the last painted run used, so a sequence that changes nothing paints nothing. A
    // reset clears it, and the walk says when one happened: a span a terminal would have ended
    // must not be remembered as open.
    let mut last: Option<Role> = None;
    for piece in letibot_transcript::sanitize::pieces(line) {
        let params = match piece {
            letibot_transcript::sanitize::Piece::Text(t) => {
                out.push_str(&t);
                continue;
            }
            letibot_transcript::sanitize::Piece::Sgr(params) => params,
        };
        if apply_sgr(&params, &mut pen) {
            last = None;
        }
        let role = role(pen);
        // **Only a change is painted.** `ls` writes `ESC[0m` before every name and a reset
        // after it, and a head that emitted a span per sequence would put four sequences on
        // a line that needs two — and would close a span that was never opened.
        match (open, role) {
            (true, None) => {
                out.push_str(&p.close());
                open = false;
            }
            (false, Some(r)) => {
                out.push_str(p.sgr(r));
                open = true;
            }
            (true, Some(r)) if Some(r) != last => {
                out.push_str(&p.close());
                out.push_str(p.sgr(r));
            }
            _ => {}
        }
        last = role;
    }
    // **A line that ends coloured is closed here.** A pty's state would run on into the
    // next row's gutter; a row list's must not.
    if open {
        out.push_str(&p.close());
    }
    out
}

/// **[`painted`], as a `rano::render::Line`** — the same walk and the same [`role`] table,
/// with each painted run a span carrying its role instead of the sequences that draw it.
///
/// For a widget that takes lines rather than strings (`rano::agent::tool_row`'s payload): the
/// row it goes into decides the register around it, and drawing the line under that register
/// with `Line::to_ansi_inside` writes the bytes [`painted`] writes under
/// [`Painter::inside`] — a run opened where the program's colour starts, closed back to the
/// block where it ends, a run with no text still opened and closed. That is why an empty run
/// is kept as a span here rather than dropped: the bytes the head's rows are compared by are
/// the bytes a terminal was always sent.
pub fn line(line: &str) -> rano::render::Line {
    use rano::render::{Line, Span};
    if !line.chars().any(char::is_control) {
        return Line::raw(line);
    }
    let mut spans: Vec<Span> = Vec::new();
    // The run being built: its role (`None` is the block's own style) and its text.
    let mut cur: (Option<Role>, String) = (None, String::new());
    let mut pen = Attr::default();
    let mut open = false;
    let mut last: Option<Role> = None;
    let flush = |cur: &mut (Option<Role>, String), spans: &mut Vec<Span>, force: bool| {
        let text = std::mem::take(&mut cur.1);
        match cur.0 {
            Some(r) => spans.push(Span::role(text, r)),
            None if !text.is_empty() || force => spans.push(Span::raw(text)),
            None => {}
        }
    };
    for piece in letibot_transcript::sanitize::pieces(line) {
        let params = match piece {
            letibot_transcript::sanitize::Piece::Text(t) => {
                cur.1.push_str(&t);
                continue;
            }
            letibot_transcript::sanitize::Piece::Sgr(params) => params,
        };
        if apply_sgr(&params, &mut pen) {
            last = None;
        }
        let role = role(pen);
        match (open, role) {
            (true, None) => {
                flush(&mut cur, &mut spans, false);
                cur.0 = None;
                open = false;
            }
            (false, Some(r)) => {
                flush(&mut cur, &mut spans, false);
                cur.0 = Some(r);
                open = true;
            }
            (true, Some(r)) if Some(r) != last => {
                flush(&mut cur, &mut spans, false);
                cur.0 = Some(r);
            }
            _ => {}
        }
        last = role;
    }
    flush(&mut cur, &mut spans, false);
    Line::new(spans)
}

/// The role a pen is drawn as, or `None` for the block's own style.
///
/// **This is the whole of the head's half of the mapping**, and it is a function of the *pen*
/// rather than of the parameters: by the time it is asked, `letibot_vt::attr::apply_sgr` has
/// already applied whatever the program wrote, and the question here is only what this head calls
/// the result. A hue the palette has no role for is no role, and the intensity is not a second
/// shade — `Attr::hue` takes it off, which is why a bright red and a red are one role and a bold
/// blue and a blue are two.
///
/// `Attr::reverse` is deliberately not read here. See the module header: a program's reverse is not
/// a role on the payload path, and it now *is* carried by the pen because a screen needs to draw
/// it.
fn role(a: Attr) -> Option<Role> {
    match (a.bold, a.hue()) {
        (_, Some(Hue::Red)) => Some(Role::Failure),
        (_, Some(Hue::Green)) => Some(Role::Success),
        (true, Some(Hue::Yellow)) => Some(Role::Attention),
        (false, Some(Hue::Yellow)) => Some(Role::Pending),
        (true, Some(Hue::Blue)) => Some(Role::Subheading),
        (false, Some(Hue::Blue)) => Some(Role::FuncName),
        (_, Some(Hue::Magenta)) => Some(Role::Keyword),
        (true, Some(Hue::Cyan)) => Some(Role::Heading),
        (false, Some(Hue::Cyan)) => Some(Role::Code),
        // Black and white are not roles. `30` is invisible on half the themes this tree is read
        // on and `37`/`97` are the body text's own colour, so both mean "no colour of mine".
        (_, Some(Hue::Black)) | (_, Some(Hue::White)) => None,
        (true, None) => Some(Role::Strong),
        // Faint only when it is all there is: no role is both dim and coloured, and the
        // colour is the part a program meant.
        (false, None) if a.dim => Some(Role::Faint),
        (false, None) => None,
    }
}

/// What a cell's pen is drawn as: the head's role for its foreground, plus the two things a program
/// asked for that **no role names** — reverse, and a background slot.
///
/// This is the key a frame groups its runs by, so two cells share a span exactly when a reader
/// cannot tell them apart. `Eq` because that is what the grouping is.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Look {
    role: Option<Role>,
    reverse: bool,
    bg: Option<u8>,
}

impl Look {
    fn of(a: Attr) -> Look {
        Look {
            role: role(a),
            reverse: a.reverse,
            bg: a.bg,
        }
    }

    /// Nothing this head paints: no role, no reverse, no background. The common case — a screen is
    /// mostly its own default pen — and it must cost no bytes.
    fn is_plain(self) -> bool {
        self.role.is_none() && !self.reverse && self.bg.is_none()
    }

    /// The sequences that open a run drawn this way: the attributes first, then the colours, which
    /// is the order a program writes them in and the order a terminal expects them.
    fn open(self, p: Painter) -> String {
        let mut s = String::new();
        if self.reverse {
            s.push_str(p.reverse());
        }
        if let Some(slot) = self.bg {
            s.push_str(p.background(slot));
        }
        if let Some(r) = self.role {
            s.push_str(p.sgr(r));
        }
        s
    }
}

/// **A screen's cells, as the rows a pane draws** — the second reader, and the head's half of it.
///
/// [`painted`] is one *line* of a foreign program's output; this is a whole screen of it. Both go
/// through [`role`], so a program's colour means one thing in this process. What this one adds is
/// the two things a screen has and a payload row does not: a cell's **background** and its
/// **reverse**, both of which [`painted`] deliberately drops — see the module header.
///
/// # A cell is a column, so an untouched cell is a space
///
/// A row is its cells, one column each, and nothing else: skipping the blank ones would slide
/// everything left of the cursor into column zero, which is a screen that lines nothing up. The one
/// cell that contributes no column is the trailing half of a wide glyph
/// ([`letibot_vt::Cell::is_wide_tail`]) — the glyph is in the cell to its left and writing its
/// space over would erase half of it.
///
/// # What ends a row, and why it is not `trim_end`
///
/// The row stops at its last cell that is not a blank in the default pen
/// ([`letibot_vt::Cell::is_blank`]) — so a row of nothing is the empty string and costs no bytes,
/// which is what a head that erases each row's tail before drawing it wants
/// (`letibot_tui::backend::terminal::paint_full`).
/// **A trailing blank that a program *painted* is not a blank and is kept**, which `trim_end`
/// cannot see: a run that ends in a background or a reverse is the edge of a panel, and trimming
/// it would leave `mc`'s blue rectangle short of its own border.
///
/// The retired `crates/ui/src/vt.rs` did this with `trim_end` and therefore kept the trailing
/// blanks of every row that ended coloured and dropped those of every row that did not. This is the
/// same convenience with the property stated instead of approximated.
pub fn lines(screen: &Screen, palette: Palette) -> Vec<String> {
    let p = Painter::new(palette);
    let mut out = Vec::with_capacity(screen.size().0);
    for cells in screen.rows() {
        let end = cells
            .iter()
            .rposition(|c| !c.is_blank())
            .map_or(0, |i| i + 1);
        let mut row = String::new();
        let mut open: Option<Look> = None;
        for cell in &cells[..end] {
            if cell.is_wide_tail() {
                continue;
            }
            let l = Look::of(cell.attr);
            // A plain run is *no* span rather than a span with nothing in it, so the bookkeeping
            // below never opens one it would have to close.
            let want = if l.is_plain() { None } else { Some(l) };
            // **Only a change is painted**, the same rule `painted` follows: a screen's runs are
            // grouped here, from what the cells hold, and a sequence per cell would be a sequence
            // per column.
            if want != open {
                if open.is_some() {
                    row.push_str(&p.close());
                }
                if let Some(l) = want {
                    row.push_str(&l.open(p));
                }
                open = want;
            }
            row.push(cell.ch);
        }
        if open.is_some() {
            row.push_str(&p.close());
        }
        out.push(row);
    }
    out
}

/// **The rows, and the rectangle they came out of** — the pane's one entry point.
///
/// Resizes the screen to `cols` × `room` and returns **exactly `room` rows**, so a pane drawn with
/// this takes the conversation's rectangle and gives it back: the composer, the status row and the
/// header keep the rows they had, and nothing above the pane moves when it opens.
///
/// `room` is clamped to at least one by [`Screen::resize`], which is the model's own posture — a
/// rectangle of no rows is a window that has told us nothing useful, and one row is the smaller
/// lie. `cols` is clamped the same way.
pub fn pane_rows(screen: &mut Screen, cols: usize, room: usize, palette: Palette) -> Vec<String> {
    screen.resize(room, cols);
    lines(screen, palette)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::painter::Sgr;
    use rano::style::Palette;

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
                Palette::Colour.sgr(role)
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
            Palette::Colour.sgr(role),
            rano::width::text::RESET
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
            out.ends_with(rano::width::text::RESET),
            "the line must not leave a colour open: {out:?}"
        );
        assert_eq!(
            painted(colour(), "\u{1b}[31mX"),
            format!(
                "{}X{}",
                Palette::Colour.sgr(Role::Failure),
                rano::width::text::RESET
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
        for own in [Palette::Colour.sgr(Role::Failure), rano::width::text::RESET] {
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
            out.matches(Palette::Colour.sgr(Role::Failure)).count(),
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
            format!("{}red{} tail", p.sgr(Role::Failure), p.close())
        );
        // And the block's own style is re-established, not the terminal's default.
        assert!(
            out.contains(&format!(
                "{}{}",
                rano::width::text::RESET,
                p.sgr(Role::Faint)
            )),
            "the run closed to the terminal rather than to the block: {out:?}"
        );
    }

    /// **A program's background survives the feed and is not dropped**, and the frame draws it.
    ///
    /// This is the gap the operator's own programs name: `mc`'s blue panels and `nano`'s status bar
    /// are backgrounds, and a pane that dropped the slot would draw their text on the transcript's
    /// own background — the panel gone and the words left behind. The assertion is on the *bytes of
    /// the row*, because a screen that kept the slot in its cells and painted nothing would pass
    /// every assertion the model can make about itself.
    #[test]
    fn a_background_a_program_painted_reaches_the_frame() {
        let mut screen = Screen::new(1, 6);
        // `mc`'s panel, filled to the screen's edge: a blue background, and its rightmost column a
        // *space* the program painted. `37` is white, which this head has no role for — a panel's
        // text keeps the reader's own foreground, which is what the table above says about white.
        screen.feed(b"\x1b[44;37mpanel \x1b[0m");
        assert_eq!(
            lines(&screen, Palette::Colour),
            vec!["\x1b[44mpanel \x1b[0m"],
            "the slot the program asked for, the panel's own trailing blank, and a close"
        );
        // Under `Palette::None` the panel is its text and nothing else — no sequence reaches a
        // replay or a CI log from here either. The trailing blank is the *screen's* and stays: the
        // row is the grid's, and only the painting is the palette's.
        assert_eq!(lines(&screen, Palette::None), vec!["panel "]);
    }

    /// **A 256-colour or truecolour background is consumed and paints nothing.**
    ///
    /// The cube's indices are absolute RGB and a slot is a theme position, so `48;5;n` and
    /// `48;2;r;g;b` set no background — and, the load-bearing half, their parameters are not read as
    /// codes: `48;5;1`'s `1` is the cube's index and also the code for bold.
    #[test]
    fn a_256_colour_background_is_consumed_and_paints_nothing() {
        let mut screen = Screen::new(1, 6);
        screen.feed(b"\x1b[48;5;1mabc\x1b[0m");
        assert_eq!(
            lines(&screen, Palette::Colour),
            vec!["abc"],
            "a colour the palette cannot name paints nothing, and is not guessed at"
        );
        // The truecolour form, whose `0` is not a reset and whose `2` is not a dim.
        let mut screen = Screen::new(1, 6);
        screen.feed(b"\x1b[48;2;255;0;0mabc\x1b[0m");
        assert_eq!(lines(&screen, Palette::Colour), vec!["abc"]);
        // And a slot the palette *can* name, in the same feed, still arrives.
        let mut screen = Screen::new(1, 6);
        screen.feed(b"\x1b[48;5;22m\x1b[44mabc\x1b[0m");
        assert_eq!(lines(&screen, Palette::Colour), vec!["\x1b[44mabc\x1b[0m"]);
    }

    /// **Reverse still works**, and it is why this screen was chosen over the one that was retired:
    /// `mc`'s selected row and `less`'s status bar are `SGR 7`, and a cell that held an
    /// `Option<Role>` could not carry it at all.
    #[test]
    fn reverse_reaches_the_frame_beside_a_background() {
        // A reverse row with no colour of its own, which is the whole of what `less`'s status bar
        // is: no role means reverse, and the run is drawn reverse all the same.
        let mut screen = Screen::new(1, 6);
        screen.feed(b"\x1b[7m sel \x1b[0m");
        assert_eq!(
            lines(&screen, Palette::Colour),
            vec!["\x1b[7m sel \x1b[0m"],
            "a program's reverse is drawn even where no role means it"
        );
        // And inside a panel, which is `mc`'s selected row: reverse *and* a background, and neither
        // takes the other.
        let mut screen = Screen::new(1, 6);
        screen.feed(b"\x1b[7;44m sel \x1b[0m");
        assert_eq!(
            lines(&screen, Palette::Colour),
            vec!["\x1b[7m\x1b[44m sel \x1b[0m"],
            "reverse is an attribute and a background is a slot: a selected row inside a panel"
        );
        // Under `Palette::None` there is no reverse to draw, and the text is what is left.
        let mut screen = Screen::new(1, 6);
        screen.feed(b"\x1b[7m sel \x1b[0m");
        assert_eq!(lines(&screen, Palette::None), vec![" sel "]);
    }

    /// **The payload path still drops a background and a reverse**, and this is the one place the
    /// two readers of one parameter list deliberately disagree.
    ///
    /// It has to be asserted rather than assumed: `painted` and [`lines`] read the *same* pen, the
    /// pen carries both, and a payload row sits inside a block this head chose — so a `\u{1b}[44m`
    /// there would put a foreign panel's colour inside a card. The screen is the reader that keeps
    /// them, because a screen **is** the program's own drawing.
    #[test]
    fn a_payload_row_still_drops_the_background_and_the_reverse_a_screen_keeps() {
        let line = "\u{1b}[44;7mX\u{1b}[0m";
        assert_eq!(painted(colour(), line), "X");
        assert_eq!(painted(Painter::new(Palette::None), line), "X");
        // And the screen, fed the same bytes, draws both — so the difference is the reader and not
        // the pen.
        let mut screen = Screen::new(1, 2);
        screen.feed(line.as_bytes());
        assert_eq!(
            lines(&screen, Palette::Colour),
            vec!["\x1b[7m\x1b[44mX\x1b[0m"]
        );
    }

    /// **The pane takes the conversation's rectangle and gives it back** — the one property the
    /// frame's layout depends on, and the reason `pane_rows` exists rather than a caller doing
    /// `resize` and `lines` itself.
    ///
    /// This is one of the two conveniences the retired `crates/ui/src/vt.rs` carried, and it is
    /// asserted here rather than there because this is where it lives now.
    #[test]
    fn the_pane_gets_exactly_the_room_it_was_given() {
        let mut screen = Screen::new(24, 80);
        screen.feed(b"first\r\nsecond");
        for room in [1, 2, 3, 10, 24, 25] {
            let rows = pane_rows(&mut screen, 80, room, Palette::Colour);
            assert_eq!(
                rows.len(),
                room,
                "a pane given {room} rows came back with {} — the composer would move",
                rows.len()
            );
            assert_eq!(
                screen.size(),
                (room, 80),
                "and the screen is the shape it was asked for, so the next frame is too"
            );
        }
        // A rectangle of no rows is clamped to one rather than panicking. That is the model's own
        // posture (`Screen::new`'s clamp), and it is asserted from here because the pane is what
        // would meet a degenerate window.
        assert_eq!(pane_rows(&mut screen, 80, 0, Palette::Colour).len(), 1);
        // And `lines` alone is one row per screen row, whatever the shape.
        assert_eq!(lines(&screen, Palette::Colour).len(), screen.size().0);
    }

    /// **A cell is painted as the role a payload line would get for the same colour.** One table,
    /// two readers: `mc`'s `1;34` and `ls`'s `01;34` come out as the same `Role::Subheading`, and
    /// under `Palette::None` a screen is text and nothing else, exactly as a payload line is.
    #[test]
    fn a_screen_cell_is_painted_as_the_same_role_a_payload_line_gets() {
        let mut screen = Screen::new(2, 6);
        screen.feed(b"\x1b[1;34msrc\x1b[0m ok");
        let rows = lines(&screen, Palette::Colour);
        assert_eq!(rows[0], format!("{} ok", span(Role::Subheading, "src")));
        assert_eq!(
            rows[1], "",
            "a row nothing was written on is empty, not six spaces"
        );
        assert_eq!(lines(&screen, Palette::None), vec!["src ok", ""]);
    }

    /// **A wide glyph is one glyph, and its tail is not a column.** The tail cell carries the same
    /// pen and is skipped, so a coloured CJK filename is one run of two columns rather than two runs
    /// of one — and the glyph is not overwritten by its own second half.
    #[test]
    fn a_wide_glyph_is_one_run_and_its_tail_contributes_no_column() {
        let mut screen = Screen::new(1, 4);
        screen.feed("\u{1b}[31m日\u{1b}[0m".as_bytes());
        assert_eq!(
            lines(&screen, Palette::Colour),
            vec![span(Role::Failure, "日")]
        );
        let cells = screen.rows().next().unwrap();
        assert!(
            cells[1].is_wide_tail(),
            "the tail is the cell beside the lead"
        );
    }

    /// **A trailing blank a program painted is not a blank.**
    ///
    /// The row ends at the last cell in the *default* pen, not at the last cell that is not a
    /// space: a run that ends in a colour, a background or a reverse is the edge of a panel, and a
    /// frame that trimmed it would cut `mc`'s rectangle short of its own border. The retired screen
    /// used `trim_end`, which cannot tell the two apart — it kept the trailing blanks of every row
    /// that ended coloured and dropped those of every row that did not.
    #[test]
    fn a_trailing_blank_the_program_painted_is_part_of_the_run() {
        let mut screen = Screen::new(1, 6);
        screen.feed(b"\x1b[31mab   \x1b[0m");
        assert_eq!(
            lines(&screen, Palette::Colour),
            vec![span(Role::Failure, "ab   ")],
            "the run's own trailing blanks are the run"
        );
    }

    /// (Moved here with `width.rs`'s deletion: the head measures with `rano::width` now, and
    /// this crate is still the one that reads both.)
    ///
    /// **The two width tables in this process agree, and where they do not it is written down.**
    ///
    /// A cell grid has to measure a character in columns for itself: `letibot_vt` is below this
    /// crate and cannot call `rano::width::char_width`, and the ranges below are the ones this tree is willing
    /// to carry, so they are the ones it copies. **A copy is a thing that drifts**, so the
    /// agreement is asserted here rather than claimed in a comment — and the one deliberate
    /// divergence is asserted too, because a difference nobody wrote down is a bug somebody will
    /// "fix" in the wrong place.
    ///
    /// The divergence is the emoji planes: a grid has one cell per code point and the head has
    /// one per *grapheme cluster*, so a single emoji is two columns to the head and one to the
    /// screen, and a ZWJ sequence is one glyph here and its parts there. `letibot_vt::width`'s
    /// header is where the cost is stated.
    #[test]
    fn the_cell_grid_measures_the_way_the_head_does_except_where_it_says_otherwise() {
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
                rano::width::char_width(c),
                "the two tables disagree about {c:?} ({:#x})",
                c as u32
            );
        }
        // And the divergence, named: two columns to the head, one to the grid.
        for c in ['✅', '🦀', '👍'] {
            assert_eq!(
                rano::width::char_width(c),
                2,
                "the head draws {c:?} double-width"
            );
            assert_eq!(
                letibot_vt::width::char_width(c),
                1,
                "the grid gives {c:?} one cell, which is the documented cost"
            );
        }
    }
}
