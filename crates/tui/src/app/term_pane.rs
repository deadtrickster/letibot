//! **The terminal pane's state**: the program it shows, what it said last, and the ask it puts
//! to the person. The pane's way out is [`WAY_OUT`].

use super::*;
use letibot_vt::MouseTracking;

/// The way out of a pane: `Ctrl-\` (0x1c), intercepted on the raw stream before anything is
/// forwarded, so the program cannot trap it. One place looks for it, [`App::pane_keys`]; a read
/// the composer consumed has no way out in it, because there the byte is content.
pub const WAY_OUT: u8 = 0x1c;

/// **How many rows one wheel notch moves**, in the scrollback or as cursor keys to a program on
/// the alternate screen. Three, which is what xterm, VTE and this head's own conversation scroll
/// by - a person's wrist already knows it.
pub(crate) const WHEEL_ROWS: usize = 3;

/// One mouse report the terminal sent, as read off the raw stream. `x` and `y` are the
/// terminal's own, one-based, over the whole window; [`TermPane::route`] is the one place that
/// knows where the pane was drawn and turns them into the pane's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Report {
    /// The button byte, SGR's numbering: `0`–`2` the buttons, `+32` motion, `+64` the wheel, and
    /// the modifier bits `4`/`8`/`16` as the terminal sent them.
    pub(crate) button: u16,
    pub(crate) x: usize,
    pub(crate) y: usize,
    /// SGR's `m`. The X10 form has no release flag of its own - it says *button 3* - and is read
    /// into the same shape.
    pub(crate) release: bool,
}

impl Report {
    fn wheel(&self) -> Option<bool> {
        // 64 is up and 65 down; 66 and 67 are a horizontal wheel, which a pane does not scroll.
        (self.button & 64 != 0 && self.button & 2 == 0).then_some(self.button & 1 == 0)
    }

    fn motion(&self) -> bool {
        self.button & 32 != 0
    }
}

/// **What [`report_at`] found at one position of a read.**
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Scan {
    /// A whole report, ending (exclusive) here.
    Report(Report, usize),
    /// The start of one that the read ended inside - R74 item 30.
    Partial,
    /// Not a report.
    No,
}

/// **Is there a mouse report at `at`?** SGR (`ESC [ < b ; x ; y M|m`) and the legacy X10 form
/// (`ESC [ M` and three bytes, each offset by 32), which a terminal that ignored `?1006` sends.
pub(crate) fn report_at(raw: &[u8], at: usize) -> Scan {
    let rest = &raw[at..];
    // A prefix of a report's introducer that the read ended inside. `ESC` alone is not one: it
    // is the Esc key, which `vi` and `mc` read on purpose, and holding it would deliver a key
    // late. `ESC [` is held, and costs Alt-`[` the same wait.
    if rest == b"\x1b[" {
        return Scan::Partial;
    }
    if let Some(body) = rest.strip_prefix(b"\x1b[<") {
        let Some(len) = body
            .iter()
            .position(|b| !(b.is_ascii_digit() || *b == b';'))
        else {
            // Digits to the end of the read: a report the read cut in half - or, past any
            // plausible length, not a report at all.
            return if body.len() <= 16 {
                Scan::Partial
            } else {
                Scan::No
            };
        };
        if !matches!(body[len], b'M' | b'm') {
            return Scan::No;
        }
        let mut nums = body[..len]
            .split(|b| *b == b';')
            .map(|n| std::str::from_utf8(n).ok()?.parse::<usize>().ok());
        let (Some(Some(b)), Some(Some(x)), Some(Some(y)), None) =
            (nums.next(), nums.next(), nums.next(), nums.next())
        else {
            // Shaped like a report and not one: still the operator's mouse, never the
            // program's text. Swallowed whole, as the old strip did.
            return Scan::Report(
                Report {
                    button: u16::MAX,
                    x: 0,
                    y: 0,
                    release: true,
                },
                at + 3 + len + 1,
            );
        };
        return Scan::Report(
            Report {
                button: b.min(u16::MAX as usize) as u16,
                x,
                y,
                release: body[len] == b'm',
            },
            at + 3 + len + 1,
        );
    }
    if let Some(body) = rest.strip_prefix(b"\x1b[M") {
        if body.len() < 3 {
            return Scan::Partial;
        }
        let b = u16::from(body[0].saturating_sub(32));
        // X10 says *a button was released* as button 3, and says nothing about which.
        let release = b & 3 == 3 && b & 64 == 0 && b & 32 == 0;
        return Scan::Report(
            Report {
                button: b,
                x: usize::from(body[1].saturating_sub(32)),
                y: usize::from(body[2].saturating_sub(32)),
                release,
            },
            at + 6,
        );
    }
    Scan::No
}

/// **One report, in the encoding the program asked for**, at the pane's own coordinates
/// (one-based). `None` when the X10 form cannot carry it - a column or row past 223.
pub(crate) fn encode(r: &Report, sgr: bool, x: usize, y: usize) -> Option<Vec<u8>> {
    if sgr {
        let fin = if r.release { 'm' } else { 'M' };
        return Some(format!("\x1b[<{};{x};{y}{fin}", r.button).into_bytes());
    }
    if x > 223 || y > 223 {
        return None;
    }
    // X10 has no release flag: a release is button 3, the modifiers kept.
    let b = if r.release {
        (r.button & !3) | 3
    } else {
        r.button
    };
    Some(vec![
        0x1b,
        b'[',
        b'M',
        32 + (b as u8),
        32 + x as u8,
        32 + y as u8,
    ])
}

/// The pane: a `letibot_vt::Screen` the program's bytes drive, drawn in the conversation's
/// rectangle. [`TermPane::rows`] answers exactly `room` rows, which is what keeps the header,
/// composer and status in place; `ctrl-\` detaches and never ends anything.
pub(crate) struct TermPane {
    /// The program's screen, fed the daemon's bytes and asked for rows.
    pub(crate) screen: letibot_vt::Screen,
    /// The line the operator submitted, verb included - kept for the one sentence this head
    /// says when the pane ends, so *what ended* is not a mystery.
    pub(crate) line: String,
    /// **The rectangle last sent to the daemon.** A resize is a frame, and a frame per tick
    /// would be a frame per keystroke - this is what makes `TermResize` fire on a change and
    /// not on a redraw.
    pub(crate) sent: (usize, usize),
    /// The operator has left and an ending is on its way: between [`Action::TermClose`] and the
    /// daemon's `TermEnded` a kill is in flight, and a byte written into a pty being signalled is
    /// a byte nobody reads. Not the detach flag, which sends nothing.
    pub(crate) closing: bool,
    /// The operator left with `ctrl-\` and the program is still running. The pane is kept: its
    /// bytes keep arriving and an ending is filed as the row it would have been, which is why
    /// [`App::detach`] drops nothing.
    pub(crate) detached: bool,
    /// **How far into the scrollback the operator has wheeled**, in rows; `0` is the live screen.
    /// See [`TermPane::route`] for what moves it and what puts it back.
    pub(crate) back: usize,
    /// **Where the pane was last drawn**, in the terminal's cells (zero-based column and row of
    /// its top-left) - what turns the operator's click into the program's. Recorded by the draw,
    /// because the draw is the one layer that knows; see [`TermPane::route`].
    pub(crate) origin: (usize, usize),
    /// **The start of a mouse report that the read ended inside** (R74 item 30), kept for the
    /// next read rather than forwarded in halves. See [`report_at`].
    pub(crate) held: Vec<u8>,
    /// **The daemon's number for this pane** (protocol 44), `None` between the Enter that put
    /// the rectangle up and the `TermAttached` that names it. Every frame about a pane carries
    /// its number, and this head feeds only the one it holds.
    pub(crate) pane: Option<u32>,
    /// **Keys typed before the number arrived**, sent in order the moment it does - see
    /// [`App::pane_input`].
    pub(crate) waiting: Vec<u8>,
}

impl TermPane {
    pub(crate) fn new(line: &str, cols: usize, rows: usize) -> TermPane {
        TermPane {
            screen: letibot_vt::Screen::new(rows, cols),
            line: line.to_string(),
            sent: (cols, rows),
            closing: false,
            detached: false,
            back: 0,
            origin: (0, 0),
            held: Vec::new(),
            pane: None,
            waiting: Vec::new(),
        }
    }

    /// **The pane's rows for this frame** - exactly `room` of them, which is the property the
    /// composer's own row budget depends on. See the type's note.
    pub(crate) fn rows(
        &mut self,
        cols: usize,
        room: usize,
        palette: rano::style::Palette,
    ) -> Vec<String> {
        // Clamped here and not only when wheeled: a resize, `ED 3` or a reset can take history
        // away under a view that was reading it.
        self.back = self.back.min(self.screen.history_len());
        letibot_ui::ansi::pane_rows_back(&mut self.screen, cols, room, palette, self.back)
    }

    /// The pane's share of one read: the bytes the program is sent, with the operator's mouse
    /// decided one report at a time - sent where the program asked for one, cursor keys on the
    /// alternate screen where it did not, and the scrollback on the main screen.
    pub(crate) fn route(&mut self, raw: &[u8]) -> Vec<u8> {
        let mut read = std::mem::take(&mut self.held);
        read.extend_from_slice(raw);
        let mut out = Vec::with_capacity(read.len());
        let mut keyed = false;
        let mut at = 0;
        while at < read.len() {
            match report_at(&read, at) {
                Scan::Report(r, end) => {
                    let sent = self.mouse(&r);
                    keyed |= !sent.is_empty() && self.screen.mouse().tracking != MouseTracking::Off;
                    out.extend_from_slice(&sent);
                    at = end;
                }
                Scan::Partial => {
                    self.held = read[at..].to_vec();
                    break;
                }
                Scan::No => {
                    out.push(read[at]);
                    keyed = true;
                    at += 1;
                }
            }
        }
        if keyed {
            self.back = 0;
        }
        out
    }

    /// One report, decided - see [`TermPane::route`]'s table. Returns the bytes the program is
    /// sent for it, which is none at all in most of the table.
    fn mouse(&mut self, r: &Report) -> Vec<u8> {
        if r.button == u16::MAX {
            return Vec::new();
        }
        let mode = self.screen.mouse();
        let (rows, cols) = self.screen.size();
        // The pane's own coordinates, one-based like the report's: the terminal's column minus
        // the columns left of the pane.
        let x =
            r.x.checked_sub(self.origin.0)
                .filter(|x| (1..=cols).contains(x));
        let y =
            r.y.checked_sub(self.origin.1)
                .filter(|y| (1..=rows).contains(y));
        let (Some(x), Some(y)) = (x, y) else {
            return Vec::new();
        };
        if mode.tracking != MouseTracking::Off {
            let wanted = match (r.wheel(), r.motion()) {
                (Some(_), _) => true,
                (None, false) => true,
                // Motion with a button held is a drag; with none (`b & 3 == 3`) it is a hover.
                (None, true) if r.button & 3 == 3 => mode.tracking == MouseTracking::Motion,
                (None, true) => {
                    matches!(mode.tracking, MouseTracking::Drag | MouseTracking::Motion)
                }
            };
            return if wanted {
                encode(r, mode.sgr, x, y).unwrap_or_default()
            } else {
                Vec::new()
            };
        }
        let Some(up) = r.wheel() else {
            return Vec::new();
        };
        if self.screen.alternate() {
            let key: &[u8] = match (up, self.screen.app_cursor_keys()) {
                (true, false) => b"\x1b[A",
                (false, false) => b"\x1b[B",
                (true, true) => b"\x1bOA",
                (false, true) => b"\x1bOB",
            };
            return key.repeat(WHEEL_ROWS);
        }
        self.back = if up {
            (self.back + WHEEL_ROWS).min(self.screen.history_len())
        } else {
            self.back.saturating_sub(WHEEL_ROWS)
        };
        Vec::new()
    }

    /// **The program's bytes, into the screen** - and the scrollback view held where the operator
    /// left it: rows pushed past the top while they read are counted into `back`, so the page
    /// stays put rather than sliding under them.
    pub(crate) fn feed(&mut self, bytes: &[u8]) {
        let before = self.screen.scrolled();
        self.screen.feed(bytes);
        if self.back > 0 {
            let grown = (self.screen.scrolled() - before) as usize;
            self.back = self
                .back
                .saturating_add(grown)
                .min(self.screen.history_len());
        }
    }

    /// The last rows the program left on the screen, for the row its ending becomes: what it
    /// printed, not the byte stream behind it. Blank rows are dropped and the tail is kept,
    /// capped at [`PANE_LAST_LINES`], which is what makes an unfolded note safe to draw.
    pub(crate) fn last_rows(&self) -> Vec<String> {
        let (rows, _) = self.screen.size();
        let mut said: Vec<String> = (0..rows)
            .map(|r| self.screen.line(r))
            .map(|l| l.trim_end().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        if said.len() > PANE_LAST_LINES {
            said.drain(..said.len() - PANE_LAST_LINES);
        }
        said
    }
}

/// What this head believes about the session's pane, as a three-state rather than an `Option`:
/// `Unasked` means the question is in flight and a verb must ask, `None` means the daemon said
/// there is no live pane. Reset to `Unasked` on every `Hello`; never derived from anything durable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PaneFact {
    /// Nobody has asked yet: the read is in flight, or this head has not attached.
    Unasked,
    /// The daemon said this session has no live pane.
    None,
    /// The daemon said these are running in it, by number - never empty (protocol 44: a
    /// session may hold several).
    Running(Vec<letibot_sessionlog::PaneBrief>),
}

impl PaneFact {
    /// **This fact with one pane added or renamed** - what a `TermAttached` tells every head.
    pub(crate) fn with(&self, pane: u32, command: &str) -> PaneFact {
        let mut list = match self {
            PaneFact::Running(list) => list.clone(),
            _ => Vec::new(),
        };
        list.retain(|b| b.pane != pane);
        list.push(letibot_sessionlog::PaneBrief {
            pane,
            command: command.to_string(),
        });
        list.sort_by_key(|b| b.pane);
        PaneFact::Running(list)
    }

    /// **This fact with one pane gone** - what a `TermEnded` tells every head. A list that
    /// empties is `None`, which is a fact; `Unasked` stays unasked.
    pub(crate) fn without(&self, pane: u32) -> PaneFact {
        match self {
            PaneFact::Running(list) => {
                let rest: Vec<_> = list.iter().filter(|b| b.pane != pane).cloned().collect();
                if rest.is_empty() {
                    PaneFact::None
                } else {
                    PaneFact::Running(rest)
                }
            }
            other => other.clone(),
        }
    }
}

/// The confirmation that ends a pane: it answers the daemon about killing a process tree, never
/// the program about itself. Its yes is `y` and deliberately not Enter, so a stray Enter kills
/// nothing, and anything that is not a deliberate yes cancels.
#[derive(Debug, Clone)]
pub(crate) struct TermAsk {
    /// **What is about to end, as a line a person reads** - `!term nano notes.txt`, the spelling
    /// the operator typed at the composer when there is one, or `!term <command>` rebuilt from
    /// the daemon's own word when the pane is another head's.
    pub(crate) line: String,
    /// **Which pane** - the daemon's number, which is what `TermClose` carries. The card names
    /// it beside the line, so a person ending one of several knows which one.
    pub(crate) pane: u32,
}
