//! **A program's byte stream, held as a screen** — the emulator half of the terminal pane.
//!
//! # The requirement, in the operator's words
//!
//! *"can we provide some sort of viewport? say if i run `mc` the conversation window replaced
//! by mc but prompt area stays"*. `mc`, `top`, `nano` and `vim` are refused by name on the `!`
//! line today, and `crates/tools/src/exec/terminal.rs` says why: the operator's run does get a
//! pty, but the pty is a **capture** — its output is folded into one transcript row and its
//! input is `/dev/null` — so a program that draws a screen draws cursor-addressing escapes
//! into that row and waits for a keystroke that cannot arrive. The refusal names the fix:
//! *"`!term`, a verb that runs such a program in a pty the daemon owns with the head as a
//! terminal emulator."* This module is the emulator half of that sentence and nothing else.
//!
//! # What this is, and the three things it is not
//!
//! It is a **VT screen**: a rectangle of cells, a cursor, a pen, a scroll region and an
//! alternate buffer, driven by a byte stream, with one way to ask what is on it ([`Screen::lines`]).
//!
//! It is not a pty (that is `letibot_tools::exec::pty`, and the pane's own module in the head
//! says which end of it feeds this), it is not a head (nothing here decides where the rows go,
//! or how many of them there are room for — [`Screen::pane_rows`] is told), and it is not a
//! program (it starts nothing, opens nothing and kills nothing).
//!
//! # Why it lives in `letibot-ui`
//!
//! Because that crate's own header already answers this question for exactly this kind of
//! thing: *"Measuring a string in terminal columns, colouring a code block, laying out a diff
//! and editing a line are library problems with no opinion about where the bytes came from…
//! this crate produces lines; a head decides where they go. Nothing here opens a file
//! descriptor, reads the environment, or knows what a session is."* A screen is the same kind
//! of object as [`crate::ansi`] — its nearest sibling, and the one it borrows its colour table
//! from — and it is the same kind as [`crate::width`], which it measures with.
//!
//! The alternative was `letibot-tui`, where the pane's state lives. That would put the
//! emulator behind one head's event loop, and the second head (`--replay`, a pager, leticl's
//! Lisp head, the flowy bridge) would fork it or do without — which is the argument this
//! crate's header makes for every other module in it.
//!
//! # What it holds
//!
//! | the program writes | what moves |
//! |---|---|
//! | `ESC[2J`, `ESC[J`, `ESC[1J` | erase in display, from the cursor or the whole screen |
//! | `ESC[K`, `ESC[1K`, `ESC[2K` | erase in line |
//! | `ESC[H`, `ESC[{r};{c}H`, `ESC[{n}A/B/C/D`, `ESC[{n}G`, `ESC[{n}d` | cursor addressing, clamped |
//! | `ESC[{t};{b}r` | the scroll region (DECSTBM), and it homes the cursor as xterm does |
//! | `ESC[{n}S` / `ESC[{n}T` | scroll up / down, inside the region |
//! | `ESC[{n}L` / `ESC[{n}M` / `ESC[{n}P` / `ESC[{n}X` | insert/delete line, delete/erase char |
//! | `ESC[{…}m` | SGR, **folded into the pen by [`crate::ansi::apply`]** — see below |
//! | `ESC[?1049h` / `ESC[?1049l` | the alternate screen, and the cursor with it |
//! | `ESC7` / `ESC8` / `ESC[s` / `ESC[u` | save and restore the cursor |
//! | `ESC]…BEL`, `ESC]…ESC\` | an OSC: consumed whole and **dropped** |
//! | `ESC(P`, `ESC=P`, `ESC>` | character set and keypad designation: consumed and dropped |
//! | `\r`, `\n`, `\t`, `\b`, `\x07` | the line discipline a pty hands over |
//!
//! **Everything not in that table is dropped**, and that is the same guarantee
//! [`crate::ansi`] makes for a line, made structurally instead: a cell holds a `char` and a
//! [`Role`], and a row is composed from cells. **No byte a program writes can reach the frame**
//! — not `ESC[?1002h`, which turns the operator's mouse reporting off, not `ESC[?2026h`, not an
//! OSC title, not a C1 control, not a DEL. The only sequences a pane's rows can carry are the
//! palette's own, chosen by this head.
//!
//! # The colour table is `ansi`'s, and there is exactly one
//!
//! `ESC[31m` becomes [`Role::Failure`] here because it becomes [`Role::Failure`] there: the pen
//! is [`crate::ansi::Wanted`], the fold is [`crate::ansi::apply`], and the answer is
//! `Wanted::role`. A screen has the same colour question a line has — *what did this program
//! mean by `1;33`* — and two tables would be two answers. `38;5;167` and `48;2;r;g;b` are
//! consumed whole and paint nothing, for `ansi`'s own reason: the cube's indices are absolute
//! RGB, and painting one puts a colour beside the reader's theme rather than within it.
//!
//! **A cell's role is fixed when the program writes it**, so a colour change is not a
//! retroactive act: `ESC[31mA ESC[0mB` is one red cell and one plain one, and a run of cells
//! that share a role is painted as one span when the row is built.
//!
//! # `\n` is `\r\n` here, and the pty is why
//!
//! A real VT has no opinion about the column: `\n` is LF, which indexes the cursor down one row
//! and leaves the column alone, and the reason a terminal's own output does not stair-step is
//! the line discipline's `ONLCR`, which rewrites every `\n` a program writes into `\r\n` on the
//! way out. [`letibot_tools::exec::pty`] *clears* `ONLCR` — deliberately, because a capture
//! wants one byte per newline — but **a screen wants it left on**, and the pane's pty will.
//!
//! So this screen is a **line-disciplined** screen: `\n` means CR+LF. That is exactly what the
//! pane's pty sends, and it is also the right answer for a stream that did not come through a
//! pty at all (a pipe, a recorded file, `top -b -n1`), where a bare `\n` at column 40 would
//! otherwise draw a staircase. A program that moves by `\r` alone — a progress bar — is
//! unaffected: `\r` is still CR.
//!
//! # The classic bug, and where it is pinned
//!
//! **A read ends wherever the kernel felt like ending it.** `ESC[2;5` and `H` can arrive in two
//! `read`s, and so can the two halves of a UTF-8 character, and a parser that treats each read
//! as a complete message draws the escape's own body as text on the screen. So [`Screen::feed`]
//! carries an unterminated tail into the next call rather than dropping it — the same
//! discipline [`crate::width`] and `letibot_tui::term` already keep for their own buffers — and
//! `a_sequence_split_across_two_reads_is_one_sequence` pins it. The tail is bounded
//! ([`MAX_SEQUENCE`]), so a program cannot make a head hold an unbounded string by writing
//! `ESC[` and never finishing it.
//!
//! # The row budget
//!
//! [`Screen::pane_rows`] is the one entry point a frame uses, and it is **the rectangle in and
//! the same rectangle out**: it resizes the screen to the columns and rows it is given, and
//! returns exactly that many rows. That is what makes the pane take the conversation's
//! rectangle and give it back — the composer, the status row and the header keep the rows they
//! had, and nothing above the pane moves by a line when the pane opens. It is the same
//! stability the composer's own suggestion row is required to keep (*"a completion list that is
//! not adjacent to the line being typed is the one row here that must not move"*), applied to a
//! rectangle instead of a row.
//!
//! A resize **keeps the top-left overlap and reflows nothing**. A real terminal reflows, and
//! this one does not: a reflow is a decision about the program's own line wrapping that belongs
//! with the `SIGWINCH` that would have to tell the program about it, and neither is built —
//! see the pane's TODOs.
//!
//! # Provenance
//!
//! Nothing here is taken from grok-build or opencode, the two projects this crate's header
//! credits, and no third emulator was read. The sequences above are implemented from their
//! names — CSI, ED, EL, CUP, DECSTBM, SGR, the alternate screen — and where a behaviour is
//! xterm's rather than the standard's (DECSTBM homes the cursor, `?1049` carries the cursor
//! with it, `?47`/`?1047` are treated as the same thing) the comment says so.

use crate::ansi::{self, Wanted};
use crate::style::{Painter, Palette, Role};
use crate::width;

/// The widest and tallest a screen may be made. The rectangle is the terminal's, so these
/// are a guard against arithmetic rather than a policy about size.
const MAX_COLS: usize = 1000;
const MAX_ROWS: usize = 500;

/// **How long an unfinished sequence may be before the parser gives up on it.** A program
/// that writes `ESC[` and then a megabyte of digits is not writing an escape; it is writing
/// a buffer this head would otherwise hold for ever.
const MAX_SEQUENCE: usize = 4096;

/// One cell: what the program put there, and what it meant by it.
///
/// `role` is [`crate::ansi`]'s answer for the pen at the moment of the write, so a cell
/// painted red is red whatever the program does to the pen afterwards. A **wide** character
/// occupies two cells and the second carries no column of its own — see [`Cell::cont`].
#[derive(Clone, PartialEq, Eq, Debug)]
struct Cell {
    text: String,
    role: Option<Role>,
    /// **The second half of a wide character.** It holds no text of its own and it emits no
    /// column, because the glyph beside it already claimed this one — which is the difference
    /// between *a blank cell* (a space, one column) and *half a glyph* (nothing, no column).
    cont: bool,
}

impl Cell {
    fn blank() -> Cell {
        Cell {
            text: String::new(),
            role: None,
            cont: false,
        }
    }
}

/// One buffer: the primary screen, or the alternate one. `Screen` owns one or two of these.
struct Grid {
    cols: usize,
    rows: usize,
    cells: Vec<Cell>,
    cur: (usize, usize),
    pen: Wanted,
    /// The scroll region, `(top, bottom)`, inclusive and 0-based. Full height until DECSTBM
    /// says otherwise.
    scroll: (usize, usize),
    /// **The wrap is pending, not performed.** A character written in the last column leaves
    /// the cursor there and sets this, so a row of exactly `cols` characters does not scroll
    /// the screen — which is what a real terminal does and what a program that fills a line
    /// relies on.
    wrap: bool,
    /// What the program asked for with `?25`. Recorded because it is a fact about the stream;
    /// **the pane does not draw a cursor from it** — the head has one caret and it is the
    /// composer's. See the pane's own module for that decision.
    cursor_visible: bool,
    /// DECSC / `ESC[s`.
    saved: Option<(usize, usize)>,
}

impl Grid {
    fn new(cols: usize, rows: usize) -> Grid {
        Grid {
            cols,
            rows,
            cells: vec![Cell::blank(); cols * rows],
            cur: (0, 0),
            pen: Wanted::default(),
            scroll: (0, rows - 1),
            wrap: false,
            cursor_visible: true,
            saved: None,
        }
    }

    fn at(&self, row: usize, col: usize) -> usize {
        row * self.cols + col
    }

    fn resize(&mut self, cols: usize, rows: usize) {
        let mut cells = vec![Cell::blank(); cols * rows];
        for r in 0..rows.min(self.rows) {
            for c in 0..cols.min(self.cols) {
                let v = self.cells[self.at(r, c)].clone();
                cells[r * cols + c] = v;
            }
            // **A wide character whose other half was cut off is not a character.** The
            // overlap keeps whole cells, and the second half of a two-column glyph is the
            // cell after the first: at the new right edge there is no cell after it, so the
            // lead is dropped rather than left claiming two columns in a one-column row.
            if cols < self.cols {
                let last = &cells[r * cols + cols - 1];
                if width::width(&last.text) == 2 {
                    cells[r * cols + cols - 1] = Cell::blank();
                }
            }
        }
        self.cells = cells;
        self.cols = cols;
        self.rows = rows;
        self.cur = (self.cur.0.min(rows - 1), self.cur.1.min(cols - 1));
        // **The region is reset rather than clamped.** A region is an assertion about a
        // screen of a particular height, and a resize is a different screen; clamping would
        // leave a program scrolling inside a region it never asked for at this size.
        self.scroll = (0, rows - 1);
        self.wrap = false;
    }

    fn blank_row(&mut self, row: usize) {
        for c in 0..self.cols {
            let i = self.at(row, c);
            self.cells[i] = Cell::blank();
        }
    }

    /// Blank the columns `from..=to` of one row. Clamped, so a caller may pass `cols`.
    fn blank_span(&mut self, row: usize, from: usize, to: usize) {
        for c in from..=to.min(self.cols - 1) {
            let i = self.at(row, c);
            self.cells[i] = Cell::blank();
        }
    }

    /// **Down one line, scrolling the region if the cursor is at its foot.** This is what
    /// `\n` and `ESC[D` do, and it is the only way a row ever leaves the top of the region.
    fn index(&mut self) {
        if self.cur.0 == self.scroll.1 {
            self.scroll_up(1);
        } else {
            self.cur.0 = (self.cur.0 + 1).min(self.rows - 1);
        }
    }

    fn reverse_index(&mut self) {
        if self.cur.0 == self.scroll.0 {
            self.scroll_down(1);
        } else {
            self.cur.0 = self.cur.0.saturating_sub(1);
        }
    }

    fn scroll_up(&mut self, n: usize) {
        let (top, bot) = self.scroll;
        let height = bot - top + 1;
        let n = n.min(height);
        if n == 0 {
            return;
        }
        let keep = height - n;
        for i in 0..keep {
            for c in 0..self.cols {
                let src = self.at(top + i + n, c);
                let v = self.cells[src].clone();
                let dst = self.at(top + i, c);
                self.cells[dst] = v;
            }
        }
        for i in keep..height {
            self.blank_row(top + i);
        }
    }

    fn scroll_down(&mut self, n: usize) {
        let (top, bot) = self.scroll;
        let height = bot - top + 1;
        let n = n.min(height);
        if n == 0 {
            return;
        }
        let keep = height - n;
        for i in (0..keep).rev() {
            for c in 0..self.cols {
                let src = self.at(top + i, c);
                let v = self.cells[src].clone();
                let dst = self.at(top + i + n, c);
                self.cells[dst] = v;
            }
        }
        for i in 0..n {
            self.blank_row(top + i);
        }
    }
}

/// **A VT screen.** Feed it bytes with [`Screen::feed`], ask it for rows with
/// [`Screen::lines`] or [`Screen::pane_rows`].
pub struct Screen {
    primary: Grid,
    /// `Some` while the alternate screen is up (`?1049h`). The program's bytes go here and
    /// the primary is untouched, which is what makes a full-screen program's exit restore
    /// the conversation of shell output that was on the screen before it started.
    alt: Option<Grid>,
    /// **The unfinished tail of the last read.** See the module header — this is the classic
    /// bug, and it is the reason `feed` is not a loop over complete sequences.
    pending: Vec<u8>,
}

impl Screen {
    /// A blank screen of this many columns and rows, clamped to the guards above.
    pub fn new(cols: usize, rows: usize) -> Screen {
        let (cols, rows) = (cols.clamp(1, MAX_COLS), rows.clamp(1, MAX_ROWS));
        Screen {
            primary: Grid::new(cols, rows),
            alt: None,
            pending: Vec::new(),
        }
    }

    pub fn cols(&self) -> usize {
        self.primary.cols
    }

    /// How many rows the screen has — which is how many [`Screen::lines`] returns.
    pub fn rows(&self) -> usize {
        self.primary.rows
    }

    /// The cursor, `(row, col)`, 0-based, on whichever buffer is up.
    pub fn cursor(&self) -> (usize, usize) {
        self.grid().cur
    }

    /// Whether the program asked for the cursor (`?25h`). Recorded, and drawn by nobody yet.
    pub fn cursor_visible(&self) -> bool {
        self.grid().cursor_visible
    }

    /// Whether the alternate screen is up. A program that took the screen is still holding it.
    pub fn on_alternate(&self) -> bool {
        self.alt.is_some()
    }

    /// **Resize, keeping the top-left overlap and reflowing nothing.** See the module header.
    pub fn resize(&mut self, cols: usize, rows: usize) {
        let (cols, rows) = (cols.clamp(1, MAX_COLS), rows.clamp(1, MAX_ROWS));
        if self.primary.cols == cols && self.primary.rows == rows {
            return;
        }
        self.primary.resize(cols, rows);
        if let Some(a) = self.alt.as_mut() {
            a.resize(cols, rows);
        }
    }

    /// **The rows, and the rectangle they came out of** — the frame's one entry point.
    ///
    /// Resizes to `(cols, room)` and returns **exactly `room` rows**, so a pane drawn with
    /// this takes the conversation's rectangle and gives it back: the composer and the status
    /// row keep the rows they had, and nothing above the pane moves when it opens.
    pub fn pane_rows(&mut self, cols: usize, room: usize, palette: Palette) -> Vec<String> {
        self.resize(cols, room);
        self.lines(palette)
    }

    /// The screen as rows, one per screen row, painted in the palette's roles.
    ///
    /// **A cell is a column, so an untouched cell is a space.** A row is the concatenation of
    /// its cells and nothing else: skipping the empty ones would slide everything left of the
    /// cursor into column zero, which is a screen that lines nothing up. The only cell that
    /// contributes no column is the second half of a wide character ([`Cell::cont`]).
    ///
    /// Trailing blanks are not written: the head erases each row's tail before it draws it
    /// (`letibot_tui::term::paint_full`), so a row of spaces would be bytes for nothing.
    pub fn lines(&self, palette: Palette) -> Vec<String> {
        let p = Painter::new(palette);
        let g = self.grid();
        let mut out = Vec::with_capacity(g.rows);
        for r in 0..g.rows {
            let mut row = String::new();
            let mut open: Option<Role> = None;
            for c in 0..g.cols {
                let cell = &g.cells[g.at(r, c)];
                if cell.cont {
                    continue;
                }
                if cell.role != open {
                    if open.is_some() {
                        row.push_str(&p.close());
                    }
                    if let Some(role) = cell.role {
                        row.push_str(p.open(role));
                    }
                    open = cell.role;
                }
                if cell.text.is_empty() {
                    row.push(' ');
                } else {
                    row.push_str(&cell.text);
                }
            }
            if open.is_some() {
                row.push_str(&p.close());
            }
            out.push(row.trim_end().to_string());
        }
        out
    }

    /// The whole screen as plain text, one line per row. For a test or a log; never a frame.
    pub fn text(&self) -> String {
        self.lines(Palette::None).join("\n")
    }

    /// **Feed a read.** The tail of an incomplete sequence is carried into the next call.
    pub fn feed(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        // Taken rather than borrowed, because `consume` needs `&mut self` while it reads the
        // buffer it is walking. The buffer is put back below, so the allocation is reused.
        let mut buf = std::mem::take(&mut self.pending);
        buf.extend_from_slice(bytes);
        let mut i = 0usize;
        while i < buf.len() {
            let n = self.consume(&buf[i..]);
            // Zero means *this is not a whole sequence yet*, and it is the only way out of
            // the loop that is not progress. Every other path consumes at least one byte.
            if n == 0 {
                break;
            }
            i += n;
        }
        if i < buf.len() {
            buf.drain(..i);
            self.pending = if buf.len() > MAX_SEQUENCE {
                Vec::new()
            } else {
                buf
            };
        }
    }

    fn grid(&self) -> &Grid {
        self.alt.as_ref().unwrap_or(&self.primary)
    }

    fn grid_mut(&mut self) -> &mut Grid {
        self.alt.as_mut().unwrap_or(&mut self.primary)
    }

    /// Bytes consumed from the front of `rest`; **0 means the sequence is not finished**.
    fn consume(&mut self, rest: &[u8]) -> usize {
        let b = rest[0];
        match b {
            0x1b => self.escape(rest),
            // The C1 introducer, which a program that writes 8-bit controls sends instead of
            // `ESC[`. A real terminal treats it as CSI; so does this one.
            0x9b => self.csi(&rest[1..]).map(|n| n + 1).unwrap_or(0),
            b'\r' => {
                let g = self.grid_mut();
                g.cur.1 = 0;
                g.wrap = false;
                1
            }
            // `\n`, and the two other bytes a terminal indexes on. **The column goes to zero
            // too** — see the module header: the pty's `ONLCR` is what makes this true of a
            // real stream, and treating a bare `\n` the same way is what keeps a pipe from
            // drawing a staircase.
            b'\n' | 0x0b | 0x0c => {
                let g = self.grid_mut();
                g.cur.1 = 0;
                g.wrap = false;
                g.index();
                1
            }
            b'\t' => {
                let g = self.grid_mut();
                let next = (g.cur.1 / 8 + 1) * 8;
                g.cur.1 = next.min(g.cols - 1);
                g.wrap = false;
                1
            }
            0x08 => {
                let g = self.grid_mut();
                g.cur.1 = g.cur.1.saturating_sub(1);
                g.wrap = false;
                1
            }
            // BEL, and every other C0 byte there is: consumed, dropped, and never a cell.
            0x00..=0x1f | 0x7f => 1,
            _ => match utf8_len(b) {
                // Not a lead byte at all, or a lead byte that cannot start a character: one
                // byte dropped, so a stream of nonsense cannot stall the screen.
                None => 1,
                Some(n) if rest.len() < n => 0,
                Some(n) => match std::str::from_utf8(&rest[..n]) {
                    Ok(s) => match s.chars().next() {
                        Some(c) => {
                            let u = c as u32;
                            match u {
                                // **A C1 control encoded as UTF-8 is still that control.**
                                // `U+009B` is CSI and a terminal in a UTF-8 locale reads it as
                                // one — xterm's own rule — so a program that writes the two-byte
                                // form gets the same screen as one that writes the single byte.
                                0x9b => match self.csi(&rest[n..]) {
                                    Some(k) => n + k,
                                    // Half a sequence: the whole character waits with it.
                                    None => 0,
                                },
                                // The rest of the C1 range, and DEL: a control byte this
                                // screen has no use for. Dropped, and never a cell.
                                0x80..=0x9f | 0x7f => n,
                                _ => {
                                    self.put(c);
                                    n
                                }
                            }
                        }
                        None => n,
                    },
                    Err(_) => 1,
                },
            },
        }
    }

    /// `ESC` and what follows it.
    fn escape(&mut self, rest: &[u8]) -> usize {
        if rest.len() < 2 {
            return 0;
        }
        match rest[1] {
            b'[' => self.csi(&rest[2..]).map(|n| n + 2).unwrap_or(0),
            // OSC. A title is a thing a program writes and a pane has no title bar for, so
            // the string is consumed whole — **including its terminator** — and dropped.
            b']' => self.string(&rest[2..]).map(|n| n + 2).unwrap_or(0),
            // DCS, SOS, PM and APC: the same treatment, for the same reason.
            b'P' | b'X' | b'^' | b'_' => self.string(&rest[2..]).map(|n| n + 2).unwrap_or(0),
            // A character set or keypad designation, which is a table this head does not
            // have: three bytes consumed, nothing drawn.
            b'(' | b')' | b'*' | b'+' | b'-' | b'.' | b'/' | b'#' => {
                if rest.len() < 3 {
                    0
                } else {
                    3
                }
            }
            b'7' => {
                let g = self.grid_mut();
                g.saved = Some(g.cur);
                2
            }
            b'8' => {
                let g = self.grid_mut();
                if let Some(c) = g.saved {
                    g.cur = c;
                }
                2
            }
            b'D' => {
                self.grid_mut().index();
                2
            }
            b'M' => {
                self.grid_mut().reverse_index();
                2
            }
            b'E' => {
                let g = self.grid_mut();
                g.cur.1 = 0;
                g.index();
                2
            }
            // RIS: a full reset, which is a fresh screen at this size.
            b'c' => {
                let (cols, rows) = (self.grid().cols, self.grid().rows);
                *self.grid_mut() = Grid::new(cols, rows);
                2
            }
            // Every other two-byte escape. `ESC=` and `ESC>` (keypad) are the common ones;
            // all of them are consumed and none of them is drawn.
            _ => 2,
        }
    }

    /// `ESC[` and what follows it. `Some(n)` is the bytes consumed after the `[`.
    fn csi(&mut self, rest: &[u8]) -> Option<usize> {
        let mut i = 0usize;
        while i < rest.len() && (0x30..=0x3f).contains(&rest[i]) {
            i += 1;
        }
        let params_end = i;
        // Intermediates (0x20..=0x2f) are part of the sequence and none of them changes what
        // this screen does, so they are skipped rather than interpreted.
        while i < rest.len() && (0x20..=0x2f).contains(&rest[i]) {
            i += 1;
        }
        if i >= rest.len() {
            return None;
        }
        let final_byte = rest[i];
        if !(0x40..=0x7e).contains(&final_byte) {
            // Malformed — a parameter byte where a final byte should be. Consumed to here so
            // the parser cannot spin on it.
            return Some(i + 1);
        }
        self.csi_final(&rest[..params_end], final_byte);
        Some(i + 1)
    }

    fn csi_final(&mut self, raw: &[u8], f: u8) {
        // `?`, `>` and `=` introduce a private or secondary sequence: the same syntax, a
        // different namespace. `?` is the one that matters here (the DEC private modes).
        let (private, raw) = match raw.first() {
            Some(b'?') => (true, &raw[1..]),
            Some(b'>') | Some(b'=') | Some(b'<') => (false, &raw[1..]),
            _ => (false, raw),
        };
        let p = parse_params(raw);

        if f == b'm' {
            // **`ESC[m` is `ESC[0m`** — an empty parameter list is a reset, and `apply` has
            // no way to know that from an empty slice.
            let params = if p.is_empty() { vec![0] } else { p };
            ansi::apply(&params, &mut self.grid_mut().pen);
            return;
        }
        if private {
            match f {
                b'h' => {
                    for m in p {
                        self.private_mode(m, true);
                    }
                }
                b'l' => {
                    for m in p {
                        self.private_mode(m, false);
                    }
                }
                _ => {}
            }
            return;
        }
        // An absent or zero parameter means the default, which is 1 for every count and for
        // every address in this table.
        let arg = |i: usize, d: usize| -> usize {
            p.get(i)
                .copied()
                .filter(|v| *v != 0)
                .map(|v| v as usize)
                .unwrap_or(d)
        };
        match f {
            b'A' => self.move_by(0, -(arg(0, 1) as isize)),
            b'B' => self.move_by(0, arg(0, 1) as isize),
            b'C' => self.move_by(arg(0, 1) as isize, 0),
            b'D' => self.move_by(-(arg(0, 1) as isize), 0),
            b'E' => {
                self.move_by(0, arg(0, 1) as isize);
                self.column(0);
            }
            b'F' => {
                self.move_by(0, -(arg(0, 1) as isize));
                self.column(0);
            }
            b'G' => self.column(arg(0, 1) - 1),
            b'd' => self.row(arg(0, 1) - 1),
            b'H' | b'f' => {
                self.row(arg(0, 1) - 1);
                self.column(arg(1, 1) - 1);
            }
            b'J' => self.erase_display(arg(0, 0)),
            b'K' => self.erase_line(arg(0, 0)),
            b'L' => self.insert_lines(arg(0, 1)),
            b'M' => self.delete_lines(arg(0, 1)),
            b'P' => self.delete_chars(arg(0, 1)),
            b'X' => self.erase_chars(arg(0, 1)),
            b'S' => {
                let n = arg(0, 1);
                self.grid_mut().scroll_up(n);
            }
            b'T' => {
                let n = arg(0, 1);
                self.grid_mut().scroll_down(n);
            }
            // DECSTBM. **It homes the cursor**, as xterm's does and as a program that sets a
            // region and then addresses into it expects.
            b'r' => {
                let top = arg(0, 1) - 1;
                let bot = arg(1, self.grid().rows) - 1;
                let g = self.grid_mut();
                let (top, bot) = (top.min(g.rows - 1), bot.min(g.rows - 1));
                if top < bot {
                    g.scroll = (top, bot);
                    g.cur = (0, 0);
                    g.wrap = false;
                }
            }
            b's' => {
                let g = self.grid_mut();
                g.saved = Some(g.cur);
            }
            b'u' => {
                let g = self.grid_mut();
                if let Some(c) = g.saved {
                    g.cur = c;
                }
            }
            // DSR, and every other final byte this screen has no use for.
            _ => {}
        }
    }

    /// **The private modes, and the two that are honoured.**
    ///
    /// `?1049` is the alternate screen; `?47` and `?1047` are the older spellings of the same
    /// idea and are treated as the same thing (xterm's difference between them is whether the
    /// screen is cleared and whether the cursor travels, and this screen does both for all
    /// three). `?25` is the cursor's visibility, recorded and not drawn.
    ///
    /// **Everything else is ignored**, and that is the security half of this module rather
    /// than an omission: `?1002`/`?1006` are the operator's mouse reporting, `?2004` is
    /// bracketed paste, `?2026` is synchronised output. Those belong to the head that owns the
    /// terminal, and a program inside a pane does not get to change them — a rule
    /// [`crate::ansi`] already keeps for a line, kept here for a whole screen.
    fn private_mode(&mut self, mode: u16, on: bool) {
        match mode {
            1049 | 1047 | 47 => {
                if on {
                    self.alt_on();
                } else {
                    self.alt_off();
                }
            }
            25 => self.grid_mut().cursor_visible = on,
            _ => {}
        }
    }

    fn alt_on(&mut self) {
        if self.alt.is_some() {
            return;
        }
        let (cols, rows) = (self.primary.cols, self.primary.rows);
        // `?1049` carries the cursor: the program's own position is put back when it leaves.
        self.primary.saved = Some(self.primary.cur);
        self.alt = Some(Grid::new(cols, rows));
    }

    fn alt_off(&mut self) {
        if self.alt.take().is_none() {
            return;
        }
        // **The cursor `?1049h` saved comes back with the screen.** xterm's own rule, and
        // the one that puts the operator's shell prompt back where it was.
        if let Some(c) = self.primary.saved {
            self.primary.cur = c;
        }
    }

    fn row(&mut self, row: usize) {
        let g = self.grid_mut();
        g.cur.0 = row.min(g.rows - 1);
        g.wrap = false;
    }

    fn column(&mut self, col: usize) {
        let g = self.grid_mut();
        g.cur.1 = col.min(g.cols - 1);
        g.wrap = false;
    }

    fn move_by(&mut self, dx: isize, dy: isize) {
        let g = self.grid_mut();
        let r = (g.cur.0 as isize + dy).clamp(0, g.rows as isize - 1) as usize;
        let c = (g.cur.1 as isize + dx).clamp(0, g.cols as isize - 1) as usize;
        g.cur = (r, c);
        g.wrap = false;
    }

    fn erase_display(&mut self, mode: usize) {
        let g = self.grid_mut();
        let (r, c) = g.cur;
        let (cols, rows) = (g.cols, g.rows);
        match mode {
            0 => {
                g.blank_span(r, c, cols - 1);
                for rr in (r + 1)..rows {
                    g.blank_row(rr);
                }
            }
            1 => {
                for rr in 0..r {
                    g.blank_row(rr);
                }
                g.blank_span(r, 0, c);
            }
            // 2 is the whole screen. 3 asks for the scrollback, and a screen with no
            // scrollback does not have one: the same clearing is the honest answer rather
            // than a silently different one.
            _ => {
                for rr in 0..rows {
                    g.blank_row(rr);
                }
            }
        }
    }

    fn erase_line(&mut self, mode: usize) {
        let g = self.grid_mut();
        let (r, c) = g.cur;
        let cols = g.cols;
        match mode {
            0 => g.blank_span(r, c, cols - 1),
            1 => g.blank_span(r, 0, c),
            _ => g.blank_span(r, 0, cols - 1),
        }
    }

    /// IL — open `n` blank rows at the cursor, inside the region, pushing the rest down.
    fn insert_lines(&mut self, n: usize) {
        let g = self.grid_mut();
        let r = g.cur.0;
        let (top, bot) = g.scroll;
        if r < top || r > bot {
            return;
        }
        let n = n.min(bot - r + 1);
        let keep = bot - r + 1 - n;
        for i in (0..keep).rev() {
            for c in 0..g.cols {
                let src = g.at(r + i, c);
                let v = g.cells[src].clone();
                let dst = g.at(r + i + n, c);
                g.cells[dst] = v;
            }
        }
        for i in 0..n {
            g.blank_row(r + i);
        }
    }

    /// DL — the mirror of IL.
    fn delete_lines(&mut self, n: usize) {
        let g = self.grid_mut();
        let r = g.cur.0;
        let (top, bot) = g.scroll;
        if r < top || r > bot {
            return;
        }
        let n = n.min(bot - r + 1);
        let keep = bot - r + 1 - n;
        for i in 0..keep {
            for c in 0..g.cols {
                let src = g.at(r + i + n, c);
                let v = g.cells[src].clone();
                let dst = g.at(r + i, c);
                g.cells[dst] = v;
            }
        }
        for i in keep..(bot - r + 1) {
            g.blank_row(r + i);
        }
    }

    /// DCH — close up `n` columns from the cursor, on this row alone.
    fn delete_chars(&mut self, n: usize) {
        let g = self.grid_mut();
        let (r, c) = g.cur;
        let cols = g.cols;
        let n = n.min(cols - c);
        for i in 0..(cols - c - n) {
            let src = g.at(r, c + i + n);
            let v = g.cells[src].clone();
            let dst = g.at(r, c + i);
            g.cells[dst] = v;
        }
        g.blank_span(r, cols - n, cols - 1);
    }

    /// ECH — blank `n` columns from the cursor. The row's length does not change.
    fn erase_chars(&mut self, n: usize) {
        let g = self.grid_mut();
        let (r, c) = g.cur;
        let cols = g.cols;
        let n = n.min(cols - c);
        g.blank_span(r, c, c + n - 1);
    }

    /// One printable character.
    fn put(&mut self, ch: char) {
        let w = width::char_width(ch);
        if w == 0 {
            // **A combining mark belongs to the cell before it.** Dropping it would lose the
            // character's identity, and giving it a cell of its own would shift the rest of
            // the row by one column — so it is appended to its base, which is what a
            // grapheme cluster is.
            let g = self.grid_mut();
            let (r, col) = g.cur;
            let back = if g.wrap { col } else { col.saturating_sub(1) };
            let i = g.at(r, back.min(g.cols - 1));
            g.cells[i].text.push(ch);
            return;
        }
        self.write(&ch.to_string(), w);
    }

    fn write(&mut self, text: &str, w: usize) {
        // **The wrap happens here, not at the last column.** A character that lands in the
        // last column stays there and sets the flag; the *next* one wraps. And a two-column
        // character never straddles the edge: it wraps first, which is what every terminal
        // does and what keeps a row's columns countable.
        {
            let g = self.grid_mut();
            if g.wrap || (w == 2 && g.cur.1 + 1 >= g.cols) {
                g.cur.1 = 0;
                g.wrap = false;
                g.index();
            }
        }
        let g = self.grid_mut();
        let role = g.pen.role();
        let (r, c) = g.cur;
        let i = g.at(r, c);
        g.cells[i] = Cell {
            text: text.to_string(),
            role,
            cont: false,
        };
        if w == 2 {
            let j = g.at(r, (c + 1).min(g.cols - 1));
            g.cells[j] = Cell {
                text: String::new(),
                role,
                cont: true,
            };
        }
        let next = c + w;
        if next >= g.cols {
            g.cur.1 = g.cols - 1;
            g.wrap = true;
        } else {
            g.cur.1 = next;
        }
    }

    /// A string sequence — OSC, DCS, SOS, PM, APC — up to BEL or ST. `None` is *not finished*.
    fn string(&self, rest: &[u8]) -> Option<usize> {
        let mut i = 0usize;
        while i < rest.len() {
            match rest[i] {
                0x07 => return Some(i + 1),
                0x1b => {
                    if i + 1 >= rest.len() {
                        return None;
                    }
                    if rest[i + 1] == b'\\' {
                        return Some(i + 2);
                    }
                    // An `ESC` that is not a terminator: a byte of the string's body.
                    i += 2;
                }
                _ => i += 1,
            }
        }
        None
    }
}

/// `ESC[` parameter bytes, split on `;` and `:`, with a non-digit anywhere meaning the whole
/// parameter is not a number.
///
/// **`:` is split as well as `;`** because that is the shape [`ansi::apply`] reads: xterm's
/// colon subparameters (`38:5:167`) and the semicolon spelling (`38;5;167`) are the same
/// colour, and a parser that kept the colons would hand the table one parameter it could not
/// recognise — and the `167` of a colour would be read as a code of its own.
fn parse_params(raw: &[u8]) -> Vec<u16> {
    if raw.is_empty() {
        return Vec::new();
    }
    raw.split(|b| *b == b';' || *b == b':')
        .map(|part| {
            let mut n: u32 = 0;
            for d in part {
                if !d.is_ascii_digit() {
                    return 0;
                }
                n = (n * 10 + u32::from(*d - b'0')).min(u32::from(u16::MAX));
            }
            n as u16
        })
        .collect()
}

/// How many bytes the UTF-8 character starting with `b` occupies, or `None` if `b` cannot
/// start one.
fn utf8_len(b: u8) -> Option<usize> {
    match b.leading_ones() {
        0 => Some(1),
        2 => Some(2),
        3 => Some(3),
        4 => Some(4),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The screen the tests read: 20 columns, 5 rows, colour on — small enough that a row
    /// number in an assertion is a thing a reader can count.
    fn screen() -> Screen {
        Screen::new(20, 5)
    }

    /// **Every role the palette can open**, for the one test that has to strip this head's own
    /// vocabulary off a row to prove nothing else is on it. A role missing from this list
    /// would let that test pass on a row carrying it, so it is the palette's own enum and
    /// rustc refuses a new variant here until it is named.
    const ROLES: [Role; 20] = [
        Role::Plain,
        Role::Faint,
        Role::Strong,
        Role::Heading,
        Role::Subheading,
        Role::UserAccent,
        Role::UserBlock,
        Role::Success,
        Role::Pending,
        Role::Failure,
        Role::Attention,
        Role::Reasoning,
        Role::Code,
        Role::Added,
        Role::Removed,
        Role::Emphasis,
        Role::Keyword,
        Role::StringLit,
        Role::NumberLit,
        Role::Comment,
    ];

    /// The screen's rows, plain. Every assertion below is about text; the two colour tests
    /// ask for the painted form instead.
    fn rows(s: &Screen) -> Vec<String> {
        s.lines(Palette::None)
    }

    fn row(s: &Screen, n: usize) -> String {
        rows(s).get(n).cloned().unwrap_or_default()
    }

    /// **`ESC[2J` clears the screen and leaves the cursor where it was.**
    #[test]
    fn erase_in_display_clears_the_screen() {
        let mut s = screen();
        s.feed(b"one\ntwo\nthree");
        assert_eq!(row(&s, 0), "one");
        assert_eq!(row(&s, 1), "two");
        s.feed(b"\x1b[2J");
        assert_eq!(rows(&s), vec![""; 5], "the screen is cleared");
        assert_eq!(s.cursor(), (2, 5), "2J does not move the cursor");

        // The partial forms, which is the half a program actually uses: a line editor
        // clears from the cursor down, and a status bar clears one line.
        s.feed(b"\x1b[Habc\x1b[2;1Hdef\x1b[1;2H\x1b[J");
        assert_eq!(
            row(&s, 0),
            "a",
            "0J erases from the cursor to the end of the screen"
        );
        assert_eq!(rows(&s)[1..], vec![""; 4]);

        s.feed(b"\x1b[2J\x1b[1;1Habc\x1b[3;1Hdef\x1b[2;1H\x1b[1J");
        assert_eq!(rows(&s)[..2], vec![""; 2], "1J erases to the cursor");
        assert_eq!(row(&s, 2), "def", "and not the row below it");
    }

    /// **Cursor addressing, in the operator's own example**: `ESC[H` then `ESC[2;5H`, and the
    /// character written after each lands where the sequence said.
    #[test]
    fn cursor_addressing_puts_the_next_character_where_it_says() {
        let mut s = screen();
        s.feed(b"\x1b[H");
        assert_eq!(s.cursor(), (0, 0), "H is home");
        s.feed(b"X");
        assert_eq!(row(&s, 0), "X");
        s.feed(b"\x1b[2;5H");
        assert_eq!(s.cursor(), (1, 4), "CUP is 1-based, and the cursor is not");
        s.feed(b"Y");
        assert_eq!(row(&s, 1), "    Y");

        // The relative moves, and the two column-only addresses.
        s.feed(b"\x1b[3;10H\x1b[1A\x1b[3C");
        assert_eq!(s.cursor(), (1, 12));
        s.feed(b"\x1b[1G");
        assert_eq!(s.cursor().1, 0, "CHA is a column and nothing else");
        s.feed(b"\x1b[4d");
        assert_eq!(s.cursor(), (3, 0), "VPA is a row and nothing else");

        // **Clamped, never off the screen.** A program that addresses row 999 of a five-row
        // pane writes on the last row; it does not panic and it does not wrap.
        s.feed(b"\x1b[999;999HZ");
        assert_eq!(s.cursor().0, 4);
        assert_eq!(row(&s, 4).trim(), "Z");
        s.feed(b"\x1b[1;1H\x1b[500A\x1b[500D");
        assert_eq!(s.cursor(), (0, 0));
    }

    /// **The alternate screen is a screen of its own, and leaving it gives the old one back.**
    ///
    /// This is the whole of what `mc` needs to not destroy the screen it was started from,
    /// and the assertion that matters is the last one: the primary is *unharmed*, not merely
    /// redrawn.
    #[test]
    fn the_alternate_screen_hides_the_primary_and_gives_it_back() {
        let mut s = screen();
        s.feed(b"\x1b[3;7Hshell");
        assert_eq!(row(&s, 2), "      shell");
        assert!(!s.on_alternate());

        s.feed(b"\x1b[?1049h");
        assert!(s.on_alternate(), "1049h takes the screen");
        assert_eq!(
            rows(&s),
            vec![""; 5],
            "and the alternate screen starts blank"
        );

        s.feed(b"\x1b[2;2Hmc");
        assert_eq!(row(&s, 1), " mc");

        s.feed(b"\x1b[?1049l");
        assert!(!s.on_alternate());
        assert_eq!(
            row(&s, 2),
            "      shell",
            "the primary is back, byte for byte"
        );
        assert_eq!(s.cursor(), (2, 11), "and so is the cursor 1049 saved");

        // The older spellings, and a `l` with nothing to leave.
        s.feed(b"\x1b[?47h\x1b[?47l\x1b[?1049l");
        assert!(!s.on_alternate(), "an unbalanced 1049l is not a panic");
        assert_eq!(row(&s, 2), "      shell");
    }

    /// **A program's colour is drawn in this head's roles, and nothing else is drawn at all.**
    ///
    /// The mapping itself is [`crate::ansi`]'s and is tested there; what this pins is that the
    /// screen *uses* it — a second table here is the drift both modules exist to prevent — and
    /// that a sequence with no role produces no sequence on the row.
    #[test]
    fn sgr_is_mapped_onto_the_palettes_roles_and_the_rest_is_dropped() {
        let mut s = screen();
        s.feed(b"\x1b[31mred\x1b[0m plain");
        let painted = s.lines(Palette::Colour);
        assert_eq!(
            painted[0],
            format!(
                "{}red{} plain",
                Palette::Colour.open(Role::Failure),
                Painter::new(Palette::Colour).close()
            ),
            "31 is Role::Failure, and the reset closes the span"
        );
        // **And under a colourless palette the same stream is plain text.** The replay and
        // CI path must not depend on the screen's own idea of a colour.
        assert_eq!(rows(&s), vec!["red plain", "", "", "", ""]);

        // A role per cell, not per row: the pen at the moment of the write is what a cell
        // keeps, so a colour change does not reach backwards.
        s.feed(b"\x1b[2J\x1b[1;1H\x1b[1;33m!\x1b[0mok");
        let painted = s.lines(Palette::Colour);
        assert!(painted[0].starts_with(Palette::Colour.open(Role::Attention)));

        // **The sequences this head has no role for, and the one that used to be misread.**
        // `38;5;1` is a 256-colour red whose *last* parameter is a `1`: read one by one it
        // would be a bold, which is this module's mistake rather than the program's.
        let mut s = screen();
        s.feed(b"\x1b[4;38;5;1mX\x1b[48;2;10;20;30mY\x1b[2mZ");
        let painted = s.lines(Palette::Colour);
        assert!(!painted[0].starts_with('\x1b'), "{:?}", painted[0]);
        assert_eq!(
            painted[0],
            format!(
                "XY{}Z{}",
                Palette::Colour.open(Role::Faint),
                Painter::new(Palette::Colour).close()
            ),
            "the underline and both extended colours paint nothing, and the 2 after them is a dim"
        );
    }

    /// **The classic bug, pinned.** A read ends wherever the kernel ended it, and an escape
    /// split across two of them must not draw its own body as text.
    ///
    /// Three splits, because they are three different parsers: a CSI, an SGR's parameter list,
    /// and a UTF-8 character. Each is fed one byte at a time as well as in halves, since the
    /// one-byte case is what a slow pty actually produces.
    #[test]
    fn a_sequence_split_across_two_reads_is_one_sequence() {
        let mut s = screen();
        s.feed(b"\x1b[2;5");
        assert_eq!(rows(&s), vec![""; 5], "half a sequence is not text");
        s.feed(b"H");
        s.feed(b"X");
        assert_eq!(row(&s, 1), "    X");

        let mut s = screen();
        s.feed(b"\x1b[31");
        s.feed(b"mred");
        assert_eq!(
            s.lines(Palette::Colour)[0],
            format!(
                "{}red{}",
                Palette::Colour.open(Role::Failure),
                Painter::new(Palette::Colour).close()
            )
        );

        // A split inside a multi-byte character, and a split inside a wide one.
        let mut s = screen();
        for b in "héllo 世界".as_bytes() {
            s.feed(std::slice::from_ref(b));
        }
        assert_eq!(row(&s, 0), "héllo 世界");

        // **And the tail is carried across many reads, not just two.** A pty that hands over
        // four bytes at a time is ordinary, and the state has to survive each of them.
        let mut s = screen();
        for b in b"\x1b[?1049h\x1b[10;3Hwelcome".chunks(4) {
            s.feed(b);
        }
        assert!(s.on_alternate());
        assert_eq!(
            row(&s, 4),
            "  welcome",
            "row 10 of a five-row screen is row 5"
        );

        // The negative, without which the test above would pass on a screen that swallowed
        // everything: a *complete* sequence in one read still works, and so does a lone ESC.
        let mut s = screen();
        s.feed(b"\x1b[2J\x1b[1;1Hdone");
        assert_eq!(row(&s, 0), "done");
        s.feed(b"\x1b");
        assert_eq!(
            row(&s, 0),
            "done",
            "a trailing ESC draws nothing and is held"
        );
    }

    /// **The scroll region is a window on the screen, and the rows outside it do not move.**
    ///
    /// This is what `top` and `mc` are made of: a fixed header and footer with a list that
    /// scrolls between them, and the defect a naive screen has is scrolling the whole screen
    /// and taking the header with it.
    #[test]
    fn the_scroll_region_scrolls_its_own_rows_and_no_others() {
        let mut s = screen();
        s.feed(b"\x1b[1;1Hheader\x1b[5;1Hfooter\x1b[2;4r");
        assert_eq!(
            s.cursor(),
            (0, 0),
            "DECSTBM homes the cursor, as xterm's does"
        );
        s.feed(b"\x1b[2;1Hone\x1b[3;1Htwo\x1b[4;1Hthree");
        assert_eq!(row(&s, 1), "one");
        assert_eq!(row(&s, 3), "three");
        // One more line at the foot of the region pushes `one` out of the region's top.
        s.feed(b"\x1b[4;1H\nfour");
        assert_eq!(row(&s, 1), "two", "the region scrolled");
        assert_eq!(row(&s, 3), "four");
        assert_eq!(
            row(&s, 0),
            "header",
            "the row above the region is untouched"
        );
        assert_eq!(row(&s, 4), "footer", "and so is the row below it");

        // The region's own scroll commands, and its reverse.
        s.feed(b"\x1b[2;4r\x1b[1S");
        assert_eq!(row(&s, 1), "three");
        s.feed(b"\x1b[1T");
        assert_eq!(
            row(&s, 1),
            "",
            "a reverse scroll opens a blank row at the top"
        );
        assert_eq!(row(&s, 2), "three");
        assert_eq!(row(&s, 0), "header");
        assert_eq!(row(&s, 4), "footer");

        // A region that is not a region is refused rather than half-set: one row, or
        // inverted, would be a screen that scrolls into its own header.
        s.feed(b"\x1b[3;3r");
        assert_eq!(row(&s, 0), "header");
        s.feed(b"\x1b[4;2r\x1b[5;1H\x1b[1S");
        assert_eq!(row(&s, 0), "header", "an inverted region is not a region");
    }

    /// **§3.1, on a whole screen: no byte a program writes reaches the frame as a byte.**
    ///
    /// The hostile vocabulary is the store's own (`HOSTILE`, in the §3.1 suite), and the
    /// assertion is the same one the head's own test makes for a payload row: **the pane's
    /// rows carry no ESC, no C1 control and no DEL** — and the four modes that would change the
    /// operator's terminal are not among them, because they never became cells.
    #[test]
    fn no_control_byte_from_the_program_reaches_a_row() {
        // 40 columns so that the text below does not wrap: the assertions are about bytes,
        // and a wrap would be a second variable.
        let hostile = " A\u{1b}[31mred\u{1b}[0m \u{1b}[8m(hidden) \u{1b}[2J \u{1b}[?1002h \
                       \u{1b}[?1006h \u{1b}[?2004h \u{1b}[?2026h \u{1b}]0;pwned\u{7} \
                       \u{9b}31m \u{9c} \u{7f} end";
        let mut s = Screen::new(40, 5);
        s.feed(hostile.as_bytes());
        assert!(!s.on_alternate(), "no mode in that string takes a screen");
        for (n, r) in s.lines(Palette::Colour).iter().enumerate() {
            assert!(
                !r.chars()
                    .any(|c| ('\u{80}'..='\u{9f}').contains(&c) || c == '\u{7f}'),
                "a C1/DEL byte reached the pane at row {n}: {r:?}"
            );
            // The only ESC sequences a pane's row may carry are this head's own: strip the
            // palette's vocabulary off and **nothing a terminal would act on is left**.
            let mut bare = r.clone();
            for role in ROLES {
                bare = bare.replace(Palette::Colour.open(role), "");
            }
            bare = bare.replace(&Painter::new(Palette::Colour).close(), "");
            assert!(
                !bare.contains('\u{1b}'),
                "an escape this head did not choose reached the pane at row {n}: {r:?}"
            );
        }
        // And what is left is the text, in the order it was written: the control bytes are
        // dropped, not turned into characters. **The prefix is gone because `2J` in the middle
        // of that string is an erase-all and this screen obeys it** — which is the other half
        // of the same guarantee, said rather than accidentally asserted.
        let top = row(&s, 0);
        assert_eq!(top.trim(), "end", "{top:?}");
        assert!(!top.contains("pwned"), "the OSC title is gone: {top:?}");
        assert!(!top.contains("1002"), "and so is the mode: {top:?}");
        // **`\u{9b}` was a real CSI and not a character**: the two-byte UTF-8 spelling of
        // `U+009B` sets the pen red exactly as the raw `0x9b` byte does, and either way it is
        // not a cell. This is the assertion that would fail if a C1 control were painted.
        assert!(
            s.lines(Palette::Colour)[0].contains(Palette::Colour.open(Role::Failure)),
            "the C1 CSI did not reach the pen"
        );

        // **The same stream with `?1049h` in it takes the alternate screen and leaves the
        // primary whole** — a program that owns a screen is a fact about this pane, not a
        // byte that reaches the operator's terminal.
        let mut s = screen();
        s.feed(b"\x1b[1;1Hshell\x1b[?1049h");
        assert!(s.on_alternate());
        assert_eq!(rows(&s), vec![""; 5], "the alternate screen is blank");
        s.feed(b"\x1b[?1049l");
        assert_eq!(row(&s, 0), "shell");
    }

    /// **The row budget.** The pane takes the conversation's rectangle and gives back exactly
    /// that many rows, whatever the program wrote — which is what keeps the composer and the
    /// status row where they were.
    #[test]
    fn a_pane_is_exactly_the_rows_it_was_given() {
        let mut s = screen();
        s.feed(b"hello world, this is a line of text longer than some of these panes\nsecond");
        for (cols, room) in [(20usize, 5usize), (40, 3), (10, 30), (1, 1), (7, 2)] {
            let got = s.pane_rows(cols, room, Palette::None);
            assert_eq!(got.len(), room, "asked for {room} rows at {cols} columns");
            assert_eq!(s.cols(), cols);
            assert_eq!(s.rows(), room);
            // Every row is inside the rectangle. The frame trims as a backstop, but a screen
            // that overflows is a screen whose columns are wrong.
            for r in &got {
                assert!(
                    width::width(r) <= cols,
                    "a row wider than the pane: {r:?} at {cols} columns"
                );
            }
        }
        // A shrink keeps the top-left overlap, and a grow keeps it too — nothing reflows.
        let mut s = Screen::new(20, 5);
        s.feed(b"\x1b[1;1Hab\x1b[2;1Hcd");
        assert_eq!(s.pane_rows(20, 2, Palette::None), vec!["ab", "cd"]);
        assert_eq!(
            s.pane_rows(20, 4, Palette::None),
            vec!["ab", "cd", "", ""],
            "the rows that were there are still there"
        );
        assert_eq!(s.pane_rows(20, 1, Palette::None), vec!["ab"]);
    }

    /// **`\n` returns to column zero**, because the pane's pty leaves `ONLCR` on and a pipe
    /// has no line discipline at all. The control for it: `\r\n` is the same screen.
    #[test]
    fn a_newline_is_a_carriage_return_and_a_line_feed() {
        let mut a = screen();
        a.feed(b"one\ntwo");
        let mut b = screen();
        b.feed(b"one\r\ntwo");
        assert_eq!(rows(&a), rows(&b), "`\\n` and `\\r\\n` are the same screen");
        assert_eq!(row(&a, 0), "one");
        assert_eq!(row(&a, 1), "two");

        // And `\r` alone is still a carriage return — the progress bar a program redraws in
        // place, which a screen that folded `\r` into `\n` would turn into a wall.
        let mut s = screen();
        s.feed(b"10%\r50%\r100%");
        assert_eq!(row(&s, 0), "100%", "the bar overwrote itself");
        assert_eq!(rows(&s)[1..], vec![""; 4]);
    }

    /// **A line's worth of text does not scroll the screen, and the next character wraps.**
    ///
    /// The wrap-pending rule, which is the difference between a screen a program can fill and
    /// one that scrolls a row early.
    #[test]
    fn a_full_line_wraps_on_the_next_character_and_not_on_the_last_one() {
        let mut s = Screen::new(5, 3);
        s.feed(b"abcde");
        assert_eq!(row(&s, 0), "abcde");
        assert_eq!(s.cursor(), (0, 4), "the cursor stays in the last column");
        assert_eq!(row(&s, 1), "", "and nothing has wrapped yet");
        s.feed(b"f");
        assert_eq!(row(&s, 1), "f");
        assert_eq!(s.cursor(), (1, 1));

        // A two-column character never straddles the edge: it wraps first, leaving the last
        // column of the row above blank rather than half a glyph.
        let mut s = Screen::new(3, 3);
        s.feed("ab世".as_bytes());
        assert_eq!(
            row(&s, 0),
            "ab",
            "the wide character did not fit, so it wrapped"
        );
        assert_eq!(row(&s, 1), "世");
        assert_eq!(s.cursor(), (1, 2));

        // And the screen scrolls at the foot, taking the top row with it.
        let mut s = Screen::new(4, 2);
        s.feed(b"1\n2\n3");
        assert_eq!(rows(&s), vec!["2", "3"]);
    }

    /// **A combining mark joins the cell before it instead of taking one of its own.** A row
    /// that shifted by one column per accent would be a screen where nothing lines up.
    #[test]
    fn a_combining_mark_joins_its_base_and_does_not_take_a_column() {
        let mut s = screen();
        s.feed("e\u{301}x".as_bytes());
        assert_eq!(row(&s, 0), "e\u{301}x");
        assert_eq!(s.cursor(), (0, 2), "two columns for three bytes");
        assert_eq!(width::width(&row(&s, 0)), 2);
    }

    /// **The other erases, and the line edits**, because a full-screen program uses all of
    /// them and a half-implemented screen is a screen with holes in it.
    #[test]
    fn erase_in_line_and_the_line_edits_do_what_a_program_expects() {
        let mut s = screen();
        s.feed(b"\x1b[1;1Habcdef\x1b[1;3H\x1b[K");
        assert_eq!(row(&s, 0), "ab", "0K erases from the cursor to the end");
        s.feed(b"\x1b[1;1Habcdef\x1b[1;4H\x1b[1K");
        assert_eq!(
            row(&s, 0),
            "    ef",
            "1K erases up to and including the cursor"
        );
        s.feed(b"\x1b[1;1Habcdef\x1b[1;4H\x1b[2K");
        assert_eq!(row(&s, 0), "", "2K erases the whole line");
        s.feed(b"\x1b[1;1Habcdef\x1b[1;3H\x1b[2P");
        assert_eq!(row(&s, 0), "abef", "DCH closes the gap up");
        s.feed(b"\x1b[1;1Habcdef\x1b[1;3H\x1b[2X");
        assert_eq!(
            row(&s, 0),
            "ab  ef",
            "ECH blanks in place, it does not close up"
        );
        s.feed(b"\x1b[2J\x1b[1;1Ha\x1b[2;1Hb\x1b[3;1Hc\x1b[2;1H\x1b[L");
        assert_eq!(rows(&s)[..4], vec!["a", "", "b", "c"], "IL opens a row");
        s.feed(b"\x1b[2;1H\x1b[M");
        assert_eq!(
            rows(&s)[..4],
            vec!["a", "b", "c", ""],
            "DL closes it up again"
        );
    }

    /// **A fresh screen is blank and the guards hold**: a zero-sized rectangle is one cell,
    /// not a panic, and an absurd one is clamped rather than allocated.
    #[test]
    fn a_screen_is_at_least_one_cell_and_at_most_the_guard() {
        let s = Screen::new(0, 0);
        assert_eq!((s.cols(), s.rows()), (1, 1));
        assert_eq!(rows(&s), vec![""]);
        let mut s = Screen::new(0, 0);
        s.feed(b"x");
        assert_eq!(row(&s, 0), "x");
        assert_eq!(s.pane_rows(0, 0, Palette::None).len(), 1);
        let s = Screen::new(usize::MAX, usize::MAX);
        assert_eq!((s.cols(), s.rows()), (MAX_COLS, MAX_ROWS));
    }

    /// **An unfinished sequence is held, and a program cannot make the head hold an unbounded
    /// one.** The tail is what makes the split test above work; the cap is what stops a
    /// program from turning that into a leak.
    #[test]
    fn an_unterminated_sequence_is_carried_and_then_abandoned() {
        let mut s = screen();
        s.feed(b"\x1b[");
        assert_eq!(rows(&s), vec![""; 5]);
        // A megabyte of parameters and no final byte: the parser drops it and the next text
        // is text again, rather than the screen holding the bytes for ever.
        let flood = vec![b'1'; MAX_SEQUENCE * 2];
        s.feed(&flood);
        s.feed(b"\x1b[1;1Hafter");
        assert_eq!(row(&s, 0), "after");
        assert!(s.pending.len() <= MAX_SEQUENCE);

        // An OSC that never ends is the same story.
        let mut s = screen();
        s.feed(b"\x1b]0;");
        s.feed(&vec![b't'; MAX_SEQUENCE * 2]);
        s.feed(b"ok\x1b[1;1Htext");
        assert_eq!(row(&s, 0), "text", "the OSC was abandoned, not drawn");
    }

    /// **A cursor a program asked for is recorded and drawn by nobody.** The head has one
    /// caret and it belongs to the composer; a pane that moved it would take the operator's
    /// typing surface away from them.
    #[test]
    fn the_cursors_visibility_is_recorded_and_not_drawn() {
        let mut s = screen();
        assert!(s.cursor_visible(), "a fresh screen shows its cursor");
        s.feed(b"\x1b[?25l");
        assert!(!s.cursor_visible());
        s.feed(b"\x1b[?25h");
        assert!(s.cursor_visible());
        // And a screen the program took still reports where its cursor is, for a head that
        // one day wants to put the terminal's caret there.
        s.feed(b"\x1b[3;4H");
        assert_eq!(s.cursor(), (2, 3));
    }

    /// **`ESC7`/`ESC8` and `ESC[s`/`ESC[u` are the same act**, and a restore with nothing
    /// saved leaves the cursor alone rather than sending it to the corner.
    #[test]
    fn the_cursor_can_be_saved_and_restored_both_ways() {
        let mut s = screen();
        s.feed(b"\x1b[4;9H\x1b7\x1b[1;1Hx\x1b8y");
        assert_eq!(row(&s, 3), "        y");
        s.feed(b"\x1b[2;3H\x1b[s\x1b[5;1Hz\x1b[uq");
        assert_eq!(row(&s, 1), "  q");
        let mut fresh = screen();
        fresh.feed(b"\x1b[3;3H\x1b8w");
        assert_eq!(
            row(&fresh, 2),
            "  w",
            "a restore with nothing saved is a no-op"
        );
    }

    /// **`parse_params` reads the two spellings of one colour the same way**, and a parameter
    /// that is not a number is not a number.
    #[test]
    fn the_parameter_parser_reads_both_spellings() {
        assert_eq!(parse_params(b""), Vec::<u16>::new());
        assert_eq!(parse_params(b"2;5"), vec![2, 5]);
        assert_eq!(parse_params(b"38:5:167"), vec![38, 5, 167]);
        assert_eq!(parse_params(b";;"), vec![0, 0, 0]);
        assert_eq!(parse_params(b"12x"), vec![0]);
        assert_eq!(parse_params(b"99999999999999"), vec![u16::MAX]);
        assert_eq!(parse_params(b"0"), vec![0]);
    }
}
