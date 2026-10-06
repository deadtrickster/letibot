//! The grid, the cursor, the modes, and the sequences that act on them.
//!
//! # What this is
//!
//! A terminal's *display half*: bytes in, rows of cells out. It knows nothing about a pty, a
//! file descriptor, a thread or a clock, so a test is a `feed` and an assertion, and a head can
//! hold one without a terminal being anywhere near.
//!
//! # What it implements, and what it deliberately does not
//!
//! Implemented, because a full-screen program emits them:
//!
//! | | |
//! |---|---|
//! | cursor addressing | `CUP` `HVP` `CUU` `CUD` `CUF` `CUB` `CNL` `CPL` `CHA` `HPA` `VPA` |
//! | erase | `ED` `EL` `ECH`, and `ED 3` as `ED 2` (there is no scrollback in a pane) |
//! | line and character edits | `IL` `DL` `ICH` `DCH` |
//! | scrolling | `DECSTBM`, scrolling at the region's boundaries, `IND` `RI` `NEL` |
//! | modes | `?7` autowrap, `?6` origin, `?25` cursor visibility, `?47`/`?1047`/`?1049` the alternate screen |
//! | pen | `SGR` through [`crate::attr`], which is the head's own walk |
//! | screen control | `ESC 7`/`ESC 8` and `CSI s`/`CSI u`, `RIS`, `DECSTR` |
//!
//! **Not implemented, and each one is a thing the pane will therefore not show:**
//!
//! - **Mouse reporting** (`?1000` `?1002` `?1003` `?1006` `?1015`). The mode is consumed and
//!   dropped. A program that turns it on gets no events, because the pane has no input path of
//!   its own: keys and clicks are the head's, and the head must not hand a *program* the
//!   operator's mouse. What this costs: `mc`'s click-to-select, and a `less` scroll by wheel.
//! - **Bracketed paste** (`?2004`) and **synchronised output** (`?2026`): consumed and dropped.
//!   The first is an input mode; the second is a promise about when a frame is presented, and
//!   the pane's frames are the head's to time.
//! - **Hyperlinks** (`OSC 8`). An `OSC` is consumed whole by [`crate::parser`] and its content
//!   dropped, so a URL a program marks up is drawn as its text and is not clickable.
//! - **Truecolour and 256-colour fidelity.** The extended forms are consumed and paint nothing —
//!   see [`crate::attr`] — so a program that asks for a specific RGB gets the pen it already had.
//! - **Fonts, italic, underline, blink, strike.** A cell has four attributes and none of them is
//!   a font. A program that underlines a menu accelerator draws it plain.
//! - **A background.** Not carried; `mc`'s blue panels are the visible cost, and
//!   [`crate::attr`]'s header says so.
//! - **A reply to anything.** `CSI 6n` and `CSI c` are dropped, so a program that waits for the
//!   terminal to answer a cursor-position report waits. **This is the one gap that can look like
//!   a hang**, and the fix belongs to the pane rather than here: it is the one caller that has a
//!   write path, and it is the one that should answer.
//! - **A resize.** A `SIGWINCH` is the pane's; this crate has `Screen::new` and nothing that
//!   reflows a grid that already has content, because what a program is told about a resize and
//!   what it redraws are the pane's business rather than the model's.
//!
//! # The line feed is a line feed
//!
//! `LF` moves the cursor down and **does not** return the carriage; `NEL` (`ESC E`) does both,
//! and `CR` alone returns it. That is what ECMA-48 says and it is why a program's `\n` arrives
//! as `\r\n`: a pty in its default output mode translates it (`ONLCR`). A pane that opened a pty
//! with `ONLCR` cleared would show a staircase, and the fault would be the pty's rather than
//! this file's — which is worth knowing before reading the next bug report about indentation.

use crate::attr::{Attr, apply_sgr};
use crate::parser::{Csi, Event, Parser};
use crate::width::char_width;

/// One cell of the grid.
///
/// A cell is one *column*, not one character: a wide glyph is a lead cell and the tail beside it,
/// and the tail is never written on its own. [`Cell::is_wide_tail`] is how a reader tells the
/// halves apart, and [`Cell::is_wide_lead`] how it knows the next cell belongs to this one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cell {
    /// The character in this cell. A space for a blank cell, and for the tail of a wide glyph.
    pub ch: char,
    /// What the program asked the pen to be when it wrote this cell.
    pub attr: Attr,
    /// The trailing half of a wide glyph. Set by the screen and by nothing else.
    wide_tail: bool,
}

impl Cell {
    /// A blank cell in the terminal's own attributes.
    pub fn blank() -> Cell {
        Cell {
            ch: ' ',
            attr: Attr::default(),
            wide_tail: false,
        }
    }

    /// Whether this cell is the trailing half of a wide glyph.
    ///
    /// **The glyph is in the cell to the left**, and a reader that draws this cell's `ch` draws a
    /// space over half of it.
    pub fn is_wide_tail(&self) -> bool {
        self.wide_tail
    }

    /// Whether this cell holds the leading half of a wide glyph, so the cell beside it belongs to
    /// it and is [`Cell::is_wide_tail`].
    pub fn is_wide_lead(&self) -> bool {
        !self.wide_tail && char_width(self.ch) == 2
    }

    /// Whether nothing was ever written here: a space, in the default attributes.
    pub fn is_blank(&self) -> bool {
        !self.wide_tail && self.ch == ' ' && self.attr == Attr::default()
    }
}

impl Default for Cell {
    fn default() -> Cell {
        Cell::blank()
    }
}

/// A position in the grid, zero-based, row first.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
struct Pos {
    row: usize,
    col: usize,
}

/// One screen: the cells, and everything about them that the alternate screen keeps its own copy
/// of.
///
/// This is a struct rather than a pile of fields on [`Screen`] because `?1049h`/`?1049l` swap two
/// of them, and the whole of the "restored intact" promise is that the swap moves all of it.
#[derive(Clone, Debug)]
struct Face {
    cells: Vec<Cell>,
    cursor: Pos,
    pen: Attr,
    /// The deferred wrap: the cursor is sitting in the last column and the *next* character is
    /// what wraps. See [`Screen::print`].
    pending_wrap: bool,
    /// `DECAWM`: whether the cursor wraps at the right margin at all.
    wrap: bool,
    /// `DECOM`: whether `CUP` is relative to the scrolling region.
    origin: bool,
    /// The scrolling region, inclusive, as `DECSTBM` sets it.
    top: usize,
    bottom: usize,
    /// `DECTCEM`: whether the program wants a cursor on the glass.
    visible: bool,
}

impl Face {
    fn blank(rows: usize, cols: usize) -> Face {
        Face {
            cells: vec![Cell::blank(); rows * cols],
            cursor: Pos::default(),
            pen: Attr::default(),
            pending_wrap: false,
            wrap: true,
            origin: false,
            top: 0,
            bottom: rows - 1,
            visible: true,
        }
    }
}

/// A terminal screen.
///
/// `Debug` and not `Clone`: a head that wants a copy of the glass is a head that wants a *snapshot*
/// of a moment, and what it should keep is the rows it drew rather than a second screen it could
/// feed behind the first one's back.
#[derive(Debug)]
pub struct Screen {
    rows: usize,
    cols: usize,
    face: Face,
    /// The main screen, parked while the alternate screen is up.
    parked: Option<Face>,
    alt: bool,
    /// `ESC 7`/`ESC 8` and `CSI s`/`CSI u`: the cursor and the pen, saved once.
    decsc: Option<(Pos, Attr)>,
    parser: Parser,
    /// A sequence asked for the parser's own state to be dropped (`RIS`, `DECSTR`), which cannot
    /// happen from inside the walk — see [`Screen::feed`].
    parser_reset: bool,
}

impl Screen {
    /// A blank screen of `rows` × `cols`.
    ///
    /// **A zero is clamped to one**, because the alternative is a panic inside a head at a
    /// degenerate window size — a terminal that has told us nothing useful, reported as a crash in
    /// the parser.
    pub fn new(rows: usize, cols: usize) -> Screen {
        let rows = rows.max(1);
        let cols = cols.max(1);
        Screen {
            rows,
            cols,
            face: Face::blank(rows, cols),
            parked: None,
            alt: false,
            decsc: None,
            parser: Parser::new(),
            parser_reset: false,
        }
    }

    /// Apply bytes. Whatever split they arrive in, the screen is the same.
    pub fn feed(&mut self, bytes: &[u8]) {
        // The parser is taken out for the duration so the closure can hold `&mut self`. It is a
        // plain value type — an array and a few counters — so this is a move and not a cost.
        let mut parser = std::mem::take(&mut self.parser);
        parser.advance(bytes, |ev| self.apply(ev));
        self.parser = parser;
        if std::mem::take(&mut self.parser_reset) {
            self.parser = Parser::new();
        }
    }

    /// The visible grid, one slice per row.
    ///
    /// The count of rows is `rows().len()`; [`Screen::size`] is the shape, for a caller that wants
    /// both without walking.
    pub fn rows(&self) -> std::slice::Chunks<'_, Cell> {
        self.face.cells.chunks(self.cols)
    }

    /// The screen's shape, `(rows, cols)`.
    pub fn size(&self) -> (usize, usize) {
        (self.rows, self.cols)
    }

    /// One row as text, with the trailing blanks trimmed — the reader's convenience, and what the
    /// tests assert on.
    ///
    /// A wide glyph contributes its one character, and its tail cell contributes nothing: the
    /// string is what a person sees, and it is `cols` columns wide only when the row is full. A
    /// row the screen does not have is an empty string rather than a panic, the same posture as
    /// [`Screen::new`]'s clamp.
    pub fn line(&self, row: usize) -> String {
        if row >= self.rows {
            return String::new();
        }
        let cells = &self.face.cells[row * self.cols..(row + 1) * self.cols];
        let end = cells
            .iter()
            .rposition(|c| c.ch != ' ' || c.wide_tail)
            .map(|i| i + 1)
            .unwrap_or(0);
        cells[..end]
            .iter()
            .filter(|c| !c.wide_tail)
            .map(|c| c.ch)
            .collect()
    }

    /// The cursor, zero-based.
    pub fn cursor(&self) -> (usize, usize) {
        (self.face.cursor.row, self.face.cursor.col)
    }

    /// Whether the program wants a cursor on the glass (`?25h`/`?25l`).
    ///
    /// **This is what the program asked for and not what the head should draw.** A pane shows a
    /// cursor only when the pane has the focus, which is a fact this crate does not have.
    pub fn cursor_visible(&self) -> bool {
        self.face.visible
    }

    /// Whether the alternate screen is up (`?1049h`), so the main screen is parked behind it.
    pub fn alternate(&self) -> bool {
        self.alt
    }

    /// The pen as it stands, for a head that wants to draw its own cursor in the program's colours.
    pub fn pen(&self) -> Attr {
        self.face.pen
    }

    /// The scrolling region as `DECSTBM` set it, inclusive and zero-based.
    pub fn scroll_region(&self) -> (usize, usize) {
        (self.face.top, self.face.bottom)
    }

    // ---- the walk's own plumbing -------------------------------------------------------------

    fn apply(&mut self, ev: Event) {
        match ev {
            Event::Print(c) => self.print(c),
            Event::Control(b) => self.control(b),
            Event::Csi(c) => self.csi(c),
            Event::Esc(b) => self.esc(b),
        }
    }

    fn control(&mut self, b: u8) {
        match b {
            // BEL. A pane has no bell and a head decides whether to flash; nothing here.
            0x07 => {}
            0x08 => self.backspace(),
            0x09 => self.tab(),
            // LF, VT and FF are one thing to a screen.
            0x0a..=0x0c => self.line_feed(),
            0x0d => self.carriage_return(),
            // SO and SI shift the GL charset, which is not implemented: see `crate::parser`.
            0x0e | 0x0f => {}
            _ => {}
        }
    }

    fn esc(&mut self, b: u8) {
        match b {
            b'7' => self.decsc = Some((self.face.cursor, self.face.pen)),
            b'8' => self.restore_cursor(),
            b'D' => self.line_feed(),
            b'E' => {
                self.carriage_return();
                self.line_feed();
            }
            b'M' => self.reverse_index(),
            // RIS: everything, including the alternate screen, which is what a program that sends
            // it has given up on.
            b'c' => self.reset(),
            // HTS sets a tab stop. Tab stops here are every eight columns and are not settable.
            b'H' => {}
            // Keypad and single-shift modes: a head's business, not a screen's.
            b'=' | b'>' | b'N' | b'O' => {}
            b'\\' => {}
            _ => {}
        }
    }

    fn csi(&mut self, c: Csi) {
        let f = c.final_byte;
        if c.private == Some(b'?') {
            if f == b'h' || f == b'l' {
                self.dec_mode(&c, f == b'h');
            }
            return;
        }
        // `>`, `=` and `<` are xterm's own and are consumed and dropped rather than read as the
        // sequence they would be without the marker.
        if c.private.is_some() {
            return;
        }
        match f {
            b'm' if c.intermediate.is_none() => {
                // **`CSI m` with no parameters is `CSI 0 m`** — a reset — which is ECMA-48's rule
                // and the rule the tree's sanitiser already reads into the parameter list.
                let params = c.params();
                if params.is_empty() {
                    apply_sgr(&[0], &mut self.face.pen);
                } else {
                    apply_sgr(params, &mut self.face.pen);
                }
            }
            b'A' => self.move_up(c.n(0)),
            b'B' => self.move_down(c.n(0)),
            b'C' => self.move_forward(c.n(0)),
            b'D' => self.move_back(c.n(0)),
            // CNL and CPL move and return the carriage, which is what makes them worth having
            // beside CUU/CUD.
            b'E' => {
                self.move_down(c.n(0));
                self.carriage_return();
            }
            b'F' => {
                self.move_up(c.n(0));
                self.carriage_return();
            }
            b'G' | b'`' => self.set_cursor(self.face.cursor.row, c.n(0) - 1),
            b'd' => {
                let row = c.n(0) - 1 + if self.face.origin { self.face.top } else { 0 };
                self.set_cursor(row, self.face.cursor.col);
            }
            b'H' | b'f' => {
                let row = c.n(0) - 1 + if self.face.origin { self.face.top } else { 0 };
                self.set_cursor(row, c.n(1) - 1);
            }
            b'J' => self.erase_in_display(c.mode(0)),
            b'K' => self.erase_in_line(c.mode(0)),
            b'X' => self.erase_chars(c.n(0)),
            b'L' => self.insert_lines(c.n(0)),
            b'M' => self.delete_lines(c.n(0)),
            b'@' => self.insert_chars(c.n(0)),
            b'P' => self.delete_chars(c.n(0)),
            b'r' => self.set_scroll_region(&c),
            b's' => self.decsc = Some((self.face.cursor, self.face.pen)),
            b'u' => self.restore_cursor(),
            // DECSTR, which a program sends to put the terminal back to defaults without clearing
            // the screen: the pen, the margins, the modes — and not the cells.
            b'p' if c.intermediate == Some(b'!') => self.soft_reset(),
            _ => {}
        }
    }

    fn dec_mode(&mut self, c: &Csi, set: bool) {
        for p in c.params() {
            match p {
                // DECCKM: application cursor keys. A head's business.
                1 => {}
                // DECOM: `CUP` is relative to the scrolling region.
                6 => self.face.origin = set,
                // DECAWM.
                7 => self.face.wrap = set,
                // DECTCEM.
                25 => self.face.visible = set,
                // **The alternate screen, in the three spellings that occur.** 1049 is what
                // `xterm`'s terminfo sends and 47 what an older one sends; the difference between
                // them is what the alternate screen's *contents* are on the way out, and a pane
                // that owns its own buffer has nothing to preserve there. See `switch_screen`.
                47 | 1047 | 1049 => self.switch_screen(set),
                // Mouse reporting, bracketed paste, synchronised output, cursor blink: consumed
                // and dropped. The module header says what the pane therefore will not show.
                12 | 1000..=1006 | 1015 | 1016 | 2004 | 2026 => {}
                _ => {}
            }
        }
    }

    /// The alternate screen, and the whole of "the main screen restored intact on exit".
    fn switch_screen(&mut self, enter: bool) {
        if enter {
            // Already there: a second `?1049h` does not throw the parked screen away. A program
            // that sends one twice keeps its own screen, which is what it meant.
            if self.alt {
                return;
            }
            let visible = self.face.visible;
            let main = std::mem::replace(&mut self.face, Face::blank(self.rows, self.cols));
            // A mode is a mode: the switch does not change whether the cursor is shown, and the
            // alternate screen starts with the visibility the main screen had.
            self.face.visible = visible;
            self.parked = Some(main);
            self.alt = true;
        } else if self.alt {
            if let Some(main) = self.parked.take() {
                self.face = main;
            }
            self.alt = false;
        }
    }

    /// `RIS`: back to a blank screen in every respect, alternate screen included.
    fn reset(&mut self) {
        self.face = Face::blank(self.rows, self.cols);
        self.parked = None;
        self.alt = false;
        self.decsc = None;
        // The charset a program selected is part of the terminal's state, and the parser holds it.
        self.parser_reset = true;
    }

    /// `DECSTR`: the modes and the pen, but not the cells and not the cursor's position.
    fn soft_reset(&mut self) {
        self.face.pen = Attr::default();
        self.face.top = 0;
        self.face.bottom = self.rows - 1;
        self.face.origin = false;
        self.face.wrap = true;
        self.face.visible = true;
        self.face.pending_wrap = false;
        self.parser_reset = true;
    }

    fn restore_cursor(&mut self) {
        if let Some((pos, pen)) = self.decsc {
            self.face.cursor = pos;
            self.face.pen = pen;
            self.face.pending_wrap = false;
        }
    }

    // ---- the grid ----------------------------------------------------------------------------

    fn print(&mut self, ch: char) {
        let w = char_width(ch);
        if w == 0 {
            // A combining mark or a zero-width character. **It has no cell and this crate has no
            // cluster model to attach it to**, so it is dropped — the one place a program's byte
            // does not reach the grid. The cost is stated in `crate::width`: `e` + U+0301 draws as
            // `e`, and a ZWJ sequence draws as its parts.
            return;
        }
        if self.face.pending_wrap {
            // **The deferred wrap.** The cursor sat in the last column and the *next* character is
            // what wraps — so a program that fills the last column and then addresses the cursor
            // elsewhere gets no extra line, which is what makes a frame drawn edge to edge come
            // out as a frame.
            self.carriage_return();
            self.line_feed();
        }
        if w == 2 && self.face.cursor.col + 1 >= self.cols {
            // A two-column glyph with one column left. A terminal puts it on the next line rather
            // than drawing half of it.
            if self.face.wrap && self.cols >= 2 {
                self.carriage_return();
                self.line_feed();
            } else {
                // Autowrap off, or a screen one column wide: a blank, which is what is left of it.
                // Writing the lead without its tail would break the grid's own invariant.
                let (r, c) = (self.face.cursor.row, self.face.cursor.col);
                self.put_glyph(r, c, ' ', 1);
                return;
            }
        }
        let (r, c) = (self.face.cursor.row, self.face.cursor.col);
        self.put_glyph(r, c, ch, w);
        if c + w >= self.cols {
            if self.face.wrap {
                self.face.pending_wrap = true;
            }
        } else {
            self.face.cursor.col = c + w;
        }
    }

    /// Write a glyph at a cell, clearing both halves of anything it lands on.
    fn put_glyph(&mut self, row: usize, col: usize, ch: char, w: usize) {
        for c in col..(col + w).min(self.cols) {
            self.clear_glyph(row, c);
        }
        let base = row * self.cols;
        self.face.cells[base + col] = Cell {
            ch,
            attr: self.face.pen,
            wide_tail: false,
        };
        if w == 2 && col + 1 < self.cols {
            self.face.cells[base + col + 1] = Cell {
                ch: ' ',
                attr: self.face.pen,
                wide_tail: true,
            };
        }
    }

    /// Blank one cell — **and the other half of the wide glyph it belongs to, if it is one half**.
    ///
    /// Half a glyph is not a glyph: a terminal asked to erase one column of a two-column character
    /// either draws a broken box or drops the whole thing, and the grid's rule is that a wide
    /// character is a lead and its tail or it is nothing.
    fn clear_glyph(&mut self, row: usize, col: usize) {
        if col >= self.cols {
            return;
        }
        let base = row * self.cols;
        if self.face.cells[base + col].wide_tail && col > 0 {
            self.face.cells[base + col - 1] = Cell::blank();
        }
        if col + 1 < self.cols && self.face.cells[base + col + 1].wide_tail {
            self.face.cells[base + col + 1] = Cell::blank();
        }
        self.face.cells[base + col] = Cell::blank();
    }

    /// Blank an inclusive range of one row, both halves of any wide glyph it touches.
    fn blank_range(&mut self, row: usize, from: usize, to: usize) {
        if row >= self.rows {
            return;
        }
        let last = to.min(self.cols - 1);
        for c in from..=last {
            self.clear_glyph(row, c);
        }
    }

    /// Blank a whole row in the default attributes.
    fn blank_row(&mut self, row: usize) {
        let base = row * self.cols;
        for c in 0..self.cols {
            self.face.cells[base + c] = Cell::blank();
        }
    }

    /// **Restore the grid's invariant after a shift**: a wide glyph is a lead and its tail, or
    /// neither.
    ///
    /// `ICH` and `DCH` move cells along a row and can leave half a glyph at either edge, which is
    /// the one thing a reader cannot draw. The repair is a pass over the row rather than a special
    /// case in the shift, because the shift's boundary arithmetic is where a bug would live.
    fn repair(&mut self, row: usize) {
        let base = row * self.cols;
        for c in 0..self.cols {
            let cell = self.face.cells[base + c];
            if cell.wide_tail {
                let lead = c > 0 && self.face.cells[base + c - 1].is_wide_lead();
                if !lead {
                    self.face.cells[base + c] = Cell::blank();
                }
            } else if cell.is_wide_lead() {
                let tail = c + 1 < self.cols && self.face.cells[base + c + 1].wide_tail;
                if !tail {
                    self.face.cells[base + c] = Cell::blank();
                }
            }
        }
    }

    // ---- the cursor --------------------------------------------------------------------------

    fn carriage_return(&mut self) {
        self.face.cursor.col = 0;
        self.face.pending_wrap = false;
    }

    fn line_feed(&mut self) {
        self.face.pending_wrap = false;
        if self.face.cursor.row == self.face.bottom {
            self.scroll_up(1);
        } else if self.face.cursor.row + 1 < self.rows {
            self.face.cursor.row += 1;
        }
    }

    fn backspace(&mut self) {
        self.face.cursor.col = self.face.cursor.col.saturating_sub(1);
        self.face.pending_wrap = false;
    }

    fn tab(&mut self) {
        let next = (self.face.cursor.col / 8 + 1) * 8;
        self.face.cursor.col = next.min(self.cols - 1);
        self.face.pending_wrap = false;
    }

    fn reverse_index(&mut self) {
        self.face.pending_wrap = false;
        if self.face.cursor.row == self.face.top {
            self.scroll_down(1);
        } else if self.face.cursor.row > 0 {
            self.face.cursor.row -= 1;
        }
    }

    fn set_cursor(&mut self, row: usize, col: usize) {
        let (lo, hi) = if self.face.origin {
            (self.face.top, self.face.bottom)
        } else {
            (0, self.rows - 1)
        };
        self.face.cursor.row = row.clamp(lo, hi);
        self.face.cursor.col = col.min(self.cols - 1);
        self.face.pending_wrap = false;
    }

    /// `CUU`. The cursor stops at the top margin **when it is inside the region**, and can reach
    /// row 0 when it is above it — which is what a terminal does and what a program that parks the
    /// cursor outside its region relies on.
    fn move_up(&mut self, n: usize) {
        let limit = if self.face.cursor.row >= self.face.top {
            self.face.top
        } else {
            0
        };
        self.face.cursor.row = self.face.cursor.row.saturating_sub(n).max(limit);
        self.face.pending_wrap = false;
    }

    fn move_down(&mut self, n: usize) {
        let limit = if self.face.cursor.row <= self.face.bottom {
            self.face.bottom
        } else {
            self.rows - 1
        };
        self.face.cursor.row = (self.face.cursor.row + n).min(limit);
        self.face.pending_wrap = false;
    }

    fn move_forward(&mut self, n: usize) {
        self.face.cursor.col = (self.face.cursor.col + n).min(self.cols - 1);
        self.face.pending_wrap = false;
    }

    fn move_back(&mut self, n: usize) {
        self.face.cursor.col = self.face.cursor.col.saturating_sub(n);
        self.face.pending_wrap = false;
    }

    // ---- erasing, scrolling and the line edits ------------------------------------------------

    fn erase_in_display(&mut self, mode: u16) {
        let (r, c) = (self.face.cursor.row, self.face.cursor.col);
        match mode {
            0 => {
                self.blank_range(r, c, self.cols - 1);
                for row in (r + 1)..self.rows {
                    self.blank_row(row);
                }
            }
            1 => {
                for row in 0..r {
                    self.blank_row(row);
                }
                self.blank_range(r, 0, c);
            }
            // `ED 3` is the scrollback, which a pane does not have: it clears the screen, which is
            // the visible half of what was asked for.
            2 | 3 => {
                for row in 0..self.rows {
                    self.blank_row(row);
                }
            }
            _ => {}
        }
    }

    fn erase_in_line(&mut self, mode: u16) {
        let (r, c) = (self.face.cursor.row, self.face.cursor.col);
        match mode {
            0 => self.blank_range(r, c, self.cols - 1),
            1 => self.blank_range(r, 0, c),
            2 => self.blank_range(r, 0, self.cols - 1),
            _ => {}
        }
    }

    /// `ECH`: blank `n` cells from the cursor, and **do not move the cursor and do not shift**.
    fn erase_chars(&mut self, n: usize) {
        let (r, c) = (self.face.cursor.row, self.face.cursor.col);
        self.blank_range(r, c, c.saturating_add(n).saturating_sub(1));
    }

    fn scroll_up(&mut self, n: usize) {
        self.scroll_up_in(self.face.top, self.face.bottom, n);
    }

    fn scroll_down(&mut self, n: usize) {
        self.scroll_down_in(self.face.top, self.face.bottom, n);
    }

    fn scroll_up_in(&mut self, top: usize, bottom: usize, n: usize) {
        let n = n.min(bottom - top + 1);
        if n == 0 {
            return;
        }
        for row in top..=bottom {
            let src = row + n;
            if src > bottom {
                self.blank_row(row);
            } else {
                for c in 0..self.cols {
                    self.face.cells[row * self.cols + c] = self.face.cells[src * self.cols + c];
                }
            }
        }
    }

    fn scroll_down_in(&mut self, top: usize, bottom: usize, n: usize) {
        let n = n.min(bottom - top + 1);
        if n == 0 {
            return;
        }
        // In reverse, so a row is copied before it is overwritten.
        for row in (top..=bottom).rev() {
            let src = row.checked_sub(n).filter(|s| *s >= top);
            match src {
                None => self.blank_row(row),
                Some(s) => {
                    for c in 0..self.cols {
                        self.face.cells[row * self.cols + c] = self.face.cells[s * self.cols + c];
                    }
                }
            }
        }
    }

    /// `IL`: blank lines at the cursor, pushing what is below down — **within the region, and only
    /// when the cursor is inside it**, which is what ECMA-48 says and what stops a program's
    /// insert-line from touching a status line below its region.
    fn insert_lines(&mut self, n: usize) {
        let row = self.face.cursor.row;
        if row < self.face.top || row > self.face.bottom {
            return;
        }
        self.scroll_down_in(row, self.face.bottom, n);
    }

    fn delete_lines(&mut self, n: usize) {
        let row = self.face.cursor.row;
        if row < self.face.top || row > self.face.bottom {
            return;
        }
        self.scroll_up_in(row, self.face.bottom, n);
    }

    fn insert_chars(&mut self, n: usize) {
        let (row, col) = (self.face.cursor.row, self.face.cursor.col);
        let n = n.min(self.cols - col);
        let base = row * self.cols;
        for c in (col..self.cols).rev() {
            let src = c.checked_sub(n).filter(|s| *s >= col);
            self.face.cells[base + c] = match src {
                Some(s) => self.face.cells[base + s],
                None => Cell::blank(),
            };
        }
        self.repair(row);
    }

    fn delete_chars(&mut self, n: usize) {
        let (row, col) = (self.face.cursor.row, self.face.cursor.col);
        let n = n.min(self.cols - col);
        let base = row * self.cols;
        for c in col..self.cols {
            let src = c + n;
            self.face.cells[base + c] = if src < self.cols {
                self.face.cells[base + src]
            } else {
                Cell::blank()
            };
        }
        self.repair(row);
    }

    /// `DECSTBM`.
    fn set_scroll_region(&mut self, c: &Csi) {
        let top = c.n(0) - 1;
        let bottom = c
            .param(1)
            .map(|v| v as usize)
            .unwrap_or(self.rows)
            .saturating_sub(1)
            .min(self.rows - 1);
        // An inverted or empty region is ignored rather than clamped: a program that sent one has
        // made a mistake, and inventing a region for it would move the cursor somewhere it never
        // asked for.
        if top >= bottom {
            return;
        }
        self.face.top = top;
        self.face.bottom = bottom;
        // `DECSTBM` homes the cursor: the screen's origin, or the region's when origin mode is on.
        self.face.cursor = Pos {
            row: if self.face.origin { top } else { 0 },
            col: 0,
        };
        self.face.pending_wrap = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(rows: usize, cols: usize) -> Screen {
        Screen::new(rows, cols)
    }

    /// Every row as text, which is what most of these tests assert on.
    fn lines(s: &Screen) -> Vec<String> {
        (0..s.size().0).map(|r| s.line(r)).collect()
    }

    /// **The grid's own invariants, after whatever a test just did.**
    ///
    /// Two of them, and both are things a reader would draw wrongly rather than fail on: every row
    /// is exactly `cols` cells, and a wide glyph is a lead and its tail or neither — never half of
    /// one, which is a glyph a terminal draws as a broken box.
    fn assert_well_formed(s: &Screen) {
        let (rows, cols) = s.size();
        assert_eq!(s.rows().len(), rows);
        for (r, row) in s.rows().enumerate() {
            assert_eq!(row.len(), cols, "row {r} is not {cols} cells");
            for (c, cell) in row.iter().enumerate() {
                if cell.is_wide_tail() {
                    assert!(
                        c > 0 && row[c - 1].is_wide_lead(),
                        "half a glyph: a tail at {r},{c} with no lead"
                    );
                }
                if cell.is_wide_lead() {
                    assert!(
                        c + 1 < cols && row[c + 1].is_wide_tail(),
                        "half a glyph: a lead at {r},{c} with no tail"
                    );
                }
            }
        }
    }

    /// **A sequence split across two `feed` calls is one sequence.**
    ///
    /// The classic bug, and the reason the parser is a state machine: `ESC [ 1 2 ; 3 4 H` arriving
    /// in two pieces must address one cell, not print `[12;34` and then move the cursor somewhere
    /// else. The assertion is on the rows that come out, because that is what a person sees.
    #[test]
    fn a_sequence_split_across_two_feeds_lands_where_it_was_addressed() {
        let mut s = screen(4, 10);
        s.feed(b"\x1b[2;3HAB");
        s.feed(b"\x1b[4;1HCD");
        let want = lines(&s);
        assert_eq!(want[1], "  AB");
        assert_eq!(want[3], "CD");

        // The same bytes, split at every point, must produce the same screen — including inside
        // the address, inside a parameter list and inside an SGR.
        let whole = b"\x1b[2;3HAB\x1b[4;1H\x1b[1;33mCD\x1b[0m";
        for i in 0..whole.len() {
            let mut split = screen(4, 10);
            split.feed(&whole[..i]);
            split.feed(&whole[i..]);
            assert_eq!(lines(&split), want, "split at {i}");
            assert_well_formed(&split);
        }
        // And one byte at a time, which is the worst case a pty can produce.
        let mut drip = screen(4, 10);
        for b in whole {
            drip.feed(&[*b]);
        }
        assert_eq!(lines(&drip), want);
    }

    /// **A character split across two feeds is one character**, and a sequence split inside a long
    /// parameter list is still one sequence.
    ///
    /// The two together, because they are the same defect at two levels: bytes that mean nothing
    /// alone. The wide character is here as well, because its *cell* arithmetic is the part a split
    /// would corrupt.
    #[test]
    fn a_character_split_across_two_feeds_is_one_character() {
        let bytes = "\u{65e5}\u{672c}\u{8a9e}".as_bytes();
        let mut whole = screen(2, 8);
        whole.feed(bytes);
        assert_eq!(whole.line(0), "日本語");
        for i in 0..bytes.len() {
            let mut s = screen(2, 8);
            s.feed(&bytes[..i]);
            s.feed(&bytes[i..]);
            assert_eq!(s.line(0), "日本語", "split at {i}");
            assert_eq!(s.cursor(), (0, 6));
            assert_well_formed(&s);
        }
        // A long parameter list, split inside it: the parameters survive, the extended colour is
        // consumed whole, and nothing of it is printed. The list is deliberately long — a reset, a
        // run of parameters this pen has no field for, an extended colour, and a colour after it —
        // because a split inside it is where a per-chunk parse reads the tail as text.
        let sgr = b"\x1b[0;1;2;3;4;5;6;7;8;9;38;5;167;31mX";
        let mut want = screen(2, 8);
        want.feed(sgr);
        assert_eq!(want.line(0), "X");
        assert_eq!(want.pen().hue(), Some(crate::attr::Hue::Red));
        for i in 0..sgr.len() {
            let mut s = screen(2, 8);
            s.feed(&sgr[..i]);
            s.feed(&sgr[i..]);
            assert_eq!(s.line(0), "X", "split at {i}");
            assert_eq!(s.pen().hue(), Some(crate::attr::Hue::Red), "split at {i}");
        }
    }

    /// **The pane the operator asked for, drawn and then given back.**
    ///
    /// *"if i run `mc` the conversation window replaced by mc but prompt area stays"*: the
    /// transcript is the main screen, `mc` takes the alternate one, and on the way out the
    /// transcript's rows are **byte for byte** what they were — which is the whole reason the main
    /// screen is parked rather than thrown away.
    ///
    /// The frame is drawn the way `mc` draws one on a terminal without Unicode: `ESC ( 0` for the
    /// box, ASCII for the content, and a path bar addressed onto the last line.
    #[test]
    fn a_full_screen_program_replaces_the_transcript_and_gives_it_back_intact() {
        let mut s = screen(8, 24);
        s.feed(b"letibot 0.2.3\r\n");
        s.feed(b"user: list the files\r\n");
        s.feed(b"assistant: here they are\r\n");
        let transcript = lines(&s);
        assert_eq!(transcript[0], "letibot 0.2.3");
        assert_eq!(transcript[1], "user: list the files");
        assert_eq!(transcript[2], "assistant: here they are");
        let cursor_before = s.cursor();
        assert!(!s.alternate());

        // ---- mc takes the screen
        s.feed(b"\x1b[?1049h");
        assert!(s.alternate(), "the alternate screen is up");
        s.feed(b"\x1b[2J\x1b[1;1H\x1b[?25l");
        assert!(!s.cursor_visible(), "mc turns the cursor off in its panels");
        // The frame, in the DEC graphics charset: `l q k` are the corners and the line, `x` the
        // sides. This is what terminfo's `smacs` is for.
        let bar = "q".repeat(22);
        s.feed(format!("\x1b(0l{bar}k\x1b(B\r\n").as_bytes());
        s.feed(format!("\x1b(0x\x1b(B{:<22}\x1b(0x\x1b(B\r\n", " left panel").as_bytes());
        s.feed(format!("\x1b(0m{bar}j\x1b(B").as_bytes());
        // The path bar, addressed onto the last line — which is what "the address bar replacing
        // the last line" means: the alternate screen's last row is gone, and the main screen
        // behind it still has its own.
        s.feed(b"\x1b[8;1H/home/dead/Projects\x1b[K");
        let mc = lines(&s);
        let line = "─".repeat(22);
        assert_eq!(mc[0], format!("┌{line}┐"));
        assert_eq!(mc[1], format!("│{:<22}│", " left panel"));
        assert_eq!(mc[2], format!("└{line}┘"));
        assert_eq!(mc[7], "/home/dead/Projects");
        assert_eq!(mc[3], "", "the rest of the alternate screen is blank");
        assert_well_formed(&s);

        // ---- and mc gives it back
        s.feed(b"\x1b[?1049l");
        assert!(!s.alternate(), "the main screen is back");
        assert_eq!(
            lines(&s),
            transcript,
            "the transcript's rows must be exactly what they were"
        );
        assert_eq!(
            s.cursor(),
            cursor_before,
            "and the cursor with them: the program parked it and the pane must not leave it in mc's"
        );
        assert!(
            s.cursor_visible(),
            "and a program that hid the cursor must not hide it for ever"
        );
        assert_well_formed(&s);
    }

    /// **An unknown or malformed sequence is ignored.** It never panics, and it never appears as
    /// text — which is the property that matters, because a screen showing the bytes of its own
    /// escape sequences is the defect this whole crate exists to prevent.
    #[test]
    fn an_unknown_or_malformed_sequence_is_ignored() {
        let mut s = screen(3, 20);
        // A sequence from a terminal we are not, a mode we do not know, a device report we cannot
        // answer, a title, a paste mode, an unended string, a byte that cannot be in a sequence.
        let hostile = b"\x1b[>4;2m\x1b[?99999h\x1b[6n\x1b[c\x1b]0;pwned\x07\x1b[?2004h\
                        \x1bPq\x1b\\\x1b]8;;http://example.com\x07link\x1b]8;;\x07\
                        a\x1b[12\x1b[1;\x80b";
        s.feed(hostile);
        // The hyperlink's *text* survives and its URL does not, which is the shape of the whole
        // rule: a sequence is consumed, the characters a person reads are kept.
        assert_eq!(s.line(0), "linkab");
        s.feed(b"\x1b[2;1Hok");
        for bad in [
            "[>4;2", "?99999", "[6n", "[c", "]0;", "pwned", "2004", "]8;;", "http", "[12", "[1;",
        ] {
            let all = lines(&s).join("\n");
            assert!(
                !all.contains(bad),
                "{bad:?} appeared on the screen: {all:?}"
            );
        }
        assert_eq!(s.line(1), "ok", "the text around the sequences is kept");
        // A lone ESC at the end of the stream, and a designator with no byte after it, are both
        // held rather than printed — and a screen fed only those is blank.
        let mut s = screen(2, 10);
        s.feed(b"\x1b");
        s.feed(b"\x1b(");
        assert_eq!(lines(&s), vec!["".to_string(), String::new()]);
        assert_well_formed(&s);
    }

    /// **A wide character occupies two cells, and both halves are one glyph.**
    ///
    /// This is the test for the fifth thing the brief asked about: a CJK character is two columns,
    /// the cell beside it is its tail, and the grid's invariant holds — including when a wide glyph
    /// is written over, erased at one edge, or pushed off the end of a row by an insert.
    #[test]
    fn a_wide_character_occupies_two_cells() {
        let mut s = screen(3, 8);
        s.feed("日本語".as_bytes());
        assert_eq!(s.line(0), "日本語");
        assert_eq!(s.cursor(), (0, 6), "three glyphs, six columns");
        let row: Vec<Cell> = s.rows().next().unwrap().to_vec();
        assert_eq!(row[0].ch, '日');
        assert!(row[0].is_wide_lead());
        assert!(row[1].is_wide_tail());
        assert_eq!(row[1].ch, ' ', "a tail holds no glyph of its own");
        assert_eq!(row[2].ch, '本');
        assert_well_formed(&s);

        // A wide glyph with one column left goes to the next line rather than being cut in half.
        let mut s = screen(3, 5);
        s.feed("abcd日".as_bytes());
        assert_eq!(s.line(0), "abcd");
        assert_eq!(s.line(1), "日");
        assert_well_formed(&s);

        // **Writing over one half clears the other.** A lead without its tail is half a glyph, and
        // the whole point of the pair rule is that it cannot exist.
        let mut s = screen(2, 6);
        s.feed("日本".as_bytes());
        s.feed(b"\x1b[1;2HX");
        assert_eq!(s.line(0), " X本", "the overwritten glyph is gone whole");
        assert_well_formed(&s);

        // Erasing one column of a wide glyph erases the glyph: `EL` from the tail's column.
        let mut s = screen(2, 6);
        s.feed("日本".as_bytes());
        s.feed(b"\x1b[1;3H\x1b[0K");
        assert_eq!(s.line(0), "日");
        assert_well_formed(&s);

        // And a shift that cuts one in half repairs the row rather than leaving half a glyph: the
        // glyph that was cut loses the half it no longer has, and the row keeps its shape.
        let mut s = screen(2, 6);
        s.feed("日本".as_bytes());
        s.feed(b"\x1b[1;1H\x1b[1P");
        assert_eq!(
            s.line(0),
            " 本",
            "the half that was cut is dropped, not drawn"
        );
        assert_well_formed(&s);

        // A screen one column wide cannot show a two-column glyph, and shows a blank rather than
        // a cell that claims to be half of one.
        let mut s = screen(2, 1);
        s.feed("日".as_bytes());
        assert_eq!(s.line(0), "");
        assert_well_formed(&s);
    }

    /// **The deferred wrap.** The cursor sits in the last column after the character that filled
    /// it, and the *next* character is what wraps — so a program that writes the last column and
    /// then addresses the cursor elsewhere gets no extra line.
    #[test]
    fn the_wrap_at_the_right_margin_is_deferred() {
        let mut s = screen(4, 4);
        s.feed(b"abcd");
        assert_eq!(s.cursor(), (0, 3), "the cursor stays in the last column");
        assert_eq!(s.line(0), "abcd");
        assert_eq!(s.line(1), "", "and nothing has wrapped yet");
        s.feed(b"e");
        assert_eq!(s.line(0), "abcd");
        assert_eq!(s.line(1), "e");
        assert_eq!(s.cursor(), (1, 1));

        // The deferred wrap is dropped by a cursor movement, which is what makes a frame drawn to
        // the last column come out as a frame.
        let mut s = screen(4, 4);
        s.feed(b"abcd\x1b[2;1HX");
        assert_eq!(s.line(0), "abcd");
        assert_eq!(s.line(1), "X", "the wrap did not happen after the address");

        // Filling the last line wraps *and scrolls*, because the cursor is at the bottom margin.
        let mut s = screen(2, 4);
        s.feed(b"abcd\r\nefgh");
        assert_eq!(lines(&s), vec!["abcd", "efgh"]);
        s.feed(b"i");
        assert_eq!(lines(&s), vec!["efgh", "i"], "the screen scrolled by one");

        // With autowrap off, the last column is overwritten in place and the cursor never leaves
        // it — which is what a program that has turned autowrap off is asking for.
        let mut s = screen(2, 4);
        s.feed(b"\x1b[?7labcdef");
        assert_eq!(lines(&s), vec!["abcf", ""]);
        assert_eq!(s.cursor(), (0, 3));
        // Turning it back on does not move the cursor; it makes the *next* character after this
        // one wrap, which is the deferral again.
        s.feed(b"\x1b[?7hgh");
        assert_eq!(s.line(0), "abcg");
        assert_eq!(s.line(1), "h");
    }

    /// **`DECSTBM`, and scrolling at the region's boundaries.**
    ///
    /// The region is what makes a full-screen program's status line stay put: `mc` sets one, and a
    /// line feed at the bottom of it scrolls the region rather than the screen.
    #[test]
    fn a_scroll_region_scrolls_and_the_lines_outside_it_stay_put() {
        let mut s = screen(5, 6);
        for (n, row) in ["one", "two", "three", "four", "five"].iter().enumerate() {
            s.feed(format!("\x1b[{};1H{row}", n + 1).as_bytes());
        }
        s.feed(b"\x1b[2;4r");
        assert_eq!(
            s.scroll_region(),
            (1, 3),
            "DECSTBM is 1-based and inclusive"
        );
        assert_eq!(s.cursor(), (0, 0), "and it homes the cursor");
        // A line feed at the bottom of the region scrolls the region, not the screen: `one` above
        // it and `five` below it stay exactly where they were.
        s.feed(b"\x1b[4;1H\n\x1b[4;1HX");
        assert_eq!(
            lines(&s),
            vec!["one", "three", "four", "X", "five"],
            "rows 1-3 scrolled inside the region and the rows outside it did not move"
        );
        // Reverse index at the top of the region scrolls the other way.
        s.feed(b"\x1b[2;1H\x1bM");
        assert_eq!(lines(&s), vec!["one", "", "three", "four", "five"]);
        // A line feed *outside* the region just moves down, and stops at the last row.
        s.feed(b"\x1b[5;1H\n\n\n");
        assert_eq!(s.cursor(), (4, 0));
        assert_eq!(lines(&s)[4], "five");
        // `CSI r` with no parameters puts the region back to the whole screen.
        s.feed(b"\x1b[r");
        assert_eq!(s.scroll_region(), (0, 4));
        assert_well_formed(&s);
    }

    /// **Erase, insert and delete, with the cursor exactly where ECMA-48 says it stays.**
    #[test]
    fn erasing_and_the_line_edits_leave_the_cursor_alone() {
        let mut s = screen(4, 8);
        s.feed(b"aaaaaaaa\r\nbbbbbbbb\r\ncccccccc\r\ndddddddd");
        s.feed(b"\x1b[2;4H\x1b[K");
        assert_eq!(lines(&s), vec!["aaaaaaaa", "bbb", "cccccccc", "dddddddd"]);
        assert_eq!(s.cursor(), (1, 3), "EL does not move the cursor");
        s.feed(b"\x1b[1K");
        let row: Vec<Cell> = s.rows().nth(1).unwrap().to_vec();
        assert!(
            row[..4].iter().all(|c| c.ch == ' '),
            "EL 1 blanks the cursor's own column as well as the ones before it"
        );
        s.feed(b"\x1b[2;1H\x1b[1J");
        assert_eq!(lines(&s), vec!["", "", "cccccccc", "dddddddd"]);
        s.feed(b"\x1b[2J");
        assert_eq!(lines(&s), vec![""; 4]);
        assert_eq!(s.cursor(), (1, 0));

        // ECH blanks in place; ICH and DCH move the rest of the row.
        let mut s = screen(2, 8);
        s.feed(b"abcdefgh");
        s.feed(b"\x1b[1;3H\x1b[2X");
        assert_eq!(s.line(0), "ab  efgh");
        s.feed(b"\x1b[1;1H\x1b[2@");
        assert_eq!(
            s.line(0),
            "  ab  ef",
            "two columns inserted and two pushed off"
        );
        s.feed(b"\x1b[1;1H\x1b[3P");
        assert_eq!(s.line(0), "b  ef", "three deleted from the left");
        assert_eq!(s.cursor(), (0, 0), "none of them moved the cursor");

        // IL and DL, inside a region.
        let mut s = screen(4, 5);
        s.feed(b"one\r\ntwo\r\nthree\r\nfour");
        s.feed(b"\x1b[2;1H\x1b[1L");
        assert_eq!(lines(&s), vec!["one", "", "two", "three"]);
        s.feed(b"\x1b[2;1H\x1b[2M");
        assert_eq!(lines(&s), vec!["one", "three", "", ""]);
        assert_well_formed(&s);
    }

    /// **The pen is the program's, and a cell keeps the pen it was written with** — including
    /// across a scroll, which is what makes a frame that scrolls keep its colours.
    #[test]
    fn a_cell_keeps_the_attributes_it_was_written_with() {
        let mut s = screen(4, 8);
        s.feed(b"top");
        s.feed(b"\r\n\x1b[1;33mhello\x1b[0m");
        s.feed(b"\r\n\x1b[7mbar\x1b[0m");
        let row: Vec<Cell> = s.rows().next().unwrap().to_vec();
        assert!(
            !row[0].attr.bold,
            "the first row is the program's plain text"
        );
        let second: Vec<Cell> = s.rows().nth(1).unwrap().to_vec();
        assert!(second[0].attr.bold);
        assert_eq!(second[0].attr.hue(), Some(crate::attr::Hue::Yellow));
        assert!(
            second[7].attr.is_default(),
            "past the text the cells are blank"
        );
        let third: Vec<Cell> = s.rows().nth(2).unwrap().to_vec();
        assert!(third[0].attr.reverse);
        // A line feed at the bottom margin scrolls, and the pens travel with their cells.
        s.feed(b"\x1b[4;1H\n");
        assert_eq!(lines(&s), vec!["hello", "bar", "", ""]);
        assert!(s.rows().next().unwrap()[0].attr.bold);
        assert!(s.rows().nth(1).unwrap()[0].attr.reverse);
        assert_well_formed(&s);
    }

    /// **`RIS` and `DECSTR` are different things**, and the difference is the cells.
    #[test]
    fn a_full_reset_clears_the_screen_and_a_soft_reset_only_puts_the_modes_back() {
        let mut s = screen(3, 6);
        s.feed(b"\x1b[2;3r\x1b[?6h\x1b[?7l\x1b[?25l\x1b[31mhi\x1b[2;2H\x1b[!p");
        assert_eq!(s.scroll_region(), (0, 2), "DECSTR put the margins back");
        assert_eq!(s.line(0), "hi", "and left the cells alone");
        assert_eq!(
            s.cursor(),
            (2, 1),
            "and did not move the cursor: origin mode addressed row 2 of the region"
        );
        assert!(s.cursor_visible());
        assert_eq!(s.pen(), Attr::default(), "and reset the pen");
        // **And autowrap is on again**, which is the half of a soft reset that is invisible until a
        // program writes to the last column: `b` fills column 6 and `c` wraps to the next row,
        // where with autowrap still off it would have overwritten `b` in place.
        s.feed(b"\x1b[1;5Habc");
        assert_eq!(s.line(0), "hi  ab");
        assert_eq!(s.line(1), "c");

        // RIS is everything, including the alternate screen.
        let mut s = screen(3, 6);
        s.feed(b"\x1b[?1049h\x1b[31mhi\x1b[3;1H\x1b[?25l\x1bc");
        assert!(!s.alternate(), "RIS leaves the alternate screen");
        assert_eq!(lines(&s), vec![""; 3]);
        assert_eq!(s.cursor(), (0, 0));
        assert!(s.cursor_visible());
        assert_eq!(s.pen(), Attr::default());
        assert_well_formed(&s);
    }

    /// **`ESC 7`/`ESC 8` put the cursor and the pen back**, which is what a program uses to return
    /// to where it was drawing.
    #[test]
    fn a_saved_cursor_comes_back_with_its_pen() {
        let mut s = screen(3, 8);
        s.feed(b"\x1b[1;33m\x1b7\x1b[3;5HX\x1b8Y");
        assert_eq!(s.line(0), "Y");
        assert_eq!(s.cursor(), (0, 1));
        assert!(s.pen().bold, "the pen came back with the cursor");
        assert_eq!(s.pen().hue(), Some(crate::attr::Hue::Yellow));
        // `CSI s`/`CSI u` are the same thing under another name, and `ESC 8` with nothing saved is
        // a no-op rather than a jump to nowhere.
        let mut s = screen(2, 4);
        s.feed(b"\x1b[2;3H\x1b[s\x1b[1;1H\x1b[u");
        assert_eq!(s.cursor(), (1, 2));
        // `ESC s` is not a sequence — it is an `ESC` whose final byte nothing knows — and it must
        // not save anything, which is the rule for every sequence this crate does not implement.
        let mut s = screen(2, 4);
        s.feed(b"\x1b[2;3H\x1bs\x1b[1;1H\x1bu");
        assert_eq!(s.cursor(), (0, 0));
        let mut s = screen(2, 4);
        s.feed(b"\x1b[2;3H\x1b8");
        assert_eq!(
            s.cursor(),
            (1, 2),
            "nothing saved: the cursor stays where it is"
        );
    }

    /// **The cursor's visibility is the program's to set and the main screen's to keep**, and a
    /// program that hides it and dies must not leave the transcript's cursor hidden.
    #[test]
    fn cursor_visibility_survives_the_alternate_screen() {
        let mut s = screen(3, 8);
        assert!(s.cursor_visible(), "a new screen shows the cursor");
        s.feed(b"\x1b[?25l");
        assert!(!s.cursor_visible());
        s.feed(b"\x1b[?25h");
        assert!(s.cursor_visible());
        // `mc` hides the cursor in its panels and never turns it back on before leaving.
        s.feed(b"\x1b[?1049h\x1b[?25l\x1b[2J");
        assert!(!s.cursor_visible());
        s.feed(b"\x1b[?1049l");
        assert!(
            s.cursor_visible(),
            "the main screen's own visibility is what is restored"
        );
    }

    /// **Origin mode, which is the mode a program uses to keep its cursor inside its region.**
    #[test]
    fn origin_mode_addresses_the_cursor_inside_the_region() {
        let mut s = screen(6, 8);
        s.feed(b"\x1b[3;5r\x1b[?6h\x1b[1;1HX");
        assert_eq!(
            s.cursor(),
            (2, 1),
            "row 1 of the region is row 3 of the screen"
        );
        assert_eq!(s.line(2), "X");
        // An address past the bottom of the region is clamped to it, not to the screen.
        s.feed(b"\x1b[99;1HY");
        assert_eq!(s.cursor(), (4, 1));
        assert_eq!(s.line(4), "Y");
        // And with origin mode off, addressing is absolute again.
        s.feed(b"\x1b[?6l\x1b[1;1HZ");
        assert_eq!(s.cursor(), (0, 1));
        assert_eq!(s.line(0), "Z");
        assert_well_formed(&s);
    }

    /// **A screen's shape is its own, and a degenerate one does not panic.** A head whose window
    /// size came back as zero must get a screen, not a crash.
    #[test]
    fn a_degenerate_screen_is_clamped_rather_than_panicking() {
        let mut s = Screen::new(0, 0);
        assert_eq!(s.size(), (1, 1));
        s.feed(b"\x1b[9;9Habc\r\ndef\x1b[2J\x1b[3;4r\x1b[1L\x1b[1M\x1b[2X");
        assert_eq!(s.cursor(), (0, 0));
        assert_eq!(s.line(0), "");
        // And a row that does not exist is an empty string rather than a panic.
        assert_eq!(s.line(9), "");
        assert_well_formed(&s);
    }
}
