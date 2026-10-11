//! **The terminal pane's state**: the program it shows, what it said last, and the ask it puts
//! to the person. The pane's way out is [`WAY_OUT`].

use super::*;
use letibot_vt::MouseTracking;

/// **The way out of a pane: `Ctrl-\`.** See [`TermPane`] for why this byte and not `Esc`.
///
/// It is looked for on the raw byte stream, before anything is forwarded, so the program never
/// receives it — and it is looked for in **one** place, [`App::pane_keys`], called by
/// `Link::tick` when a pane is open at the tick's start or was opened by the keys in the same
/// read. A read the composer consumed — no pane, none opening, such as a paste of text with a
/// literal `0x1c` in it — has no way out in it: the byte there is content, and keeping it (as
/// the head once did, in a latch spent on the next pane that opened) counterfeited a leaving
/// nobody asked for. `0x1c` is `FS` in ASCII and `QUIT` only under `ISIG`, which raw mode
/// clears; it is one of the three bytes letibot's own decoder had no arm for, and
/// [`crate::app::key_of`] keeps it that way: rano reads it as Ctrl+`\`, and the map drops it.
///
/// `pub` because the interception is the driver's as much as the app's: the byte is a fact
/// about the stream, and `Link::tick` is where the stream is routed.
pub const WAY_OUT: u8 = 0x1c;

/// **How many rows one wheel notch moves**, in the scrollback or as cursor keys to a program on
/// the alternate screen. Three, which is what xterm, VTE and this head's own conversation scroll
/// by — a person's wrist already knows it.
pub(crate) const WHEEL_ROWS: usize = 3;

/// **One mouse report the operator's terminal sent**, as read off the raw stream.
///
/// `x` and `y` are the terminal's own, one-based, over the whole window — the pane's are worked
/// out by [`TermPane::route`], which is the one place that knows where the pane was drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Report {
    /// The button byte, SGR's numbering: `0`–`2` the buttons, `+32` motion, `+64` the wheel, and
    /// the modifier bits `4`/`8`/`16` as the terminal sent them.
    pub(crate) button: u16,
    pub(crate) x: usize,
    pub(crate) y: usize,
    /// SGR's `m`. The X10 form has no release flag of its own — it says *button 3* — and is read
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
    /// The start of one that the read ended inside — R74 item 30.
    Partial,
    /// Not a report.
    No,
}

/// **Is there a mouse report at `at`?** SGR (`ESC [ < b ; x ; y M|m`) and the legacy X10 form
/// (`ESC [ M` and three bytes, each offset by 32), which a terminal that ignored `?1006` sends.
pub(crate) fn report_at(raw: &[u8], at: usize) -> Scan {
    let rest = &raw[at..];
    // A prefix of a report's introducer that the read ended inside. **`ESC` alone is not one**:
    // it is the Esc key, a key `vi` and `mc` read on purpose, and holding it for a read that may
    // not come for a minute would be a key that arrives late. `ESC [` is held, and costs Alt-`[`
    // the same wait — which is the rare one.
    if rest == b"\x1b[" {
        return Scan::Partial;
    }
    if let Some(body) = rest.strip_prefix(b"\x1b[<") {
        let Some(len) = body
            .iter()
            .position(|b| !(b.is_ascii_digit() || *b == b';'))
        else {
            // Digits to the end of the read: a report the read cut in half — or, past any
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
/// (one-based). `None` when the X10 form cannot carry it — a column or row past 223.
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

/// **The pane: a program that owns the screen, drawn in the conversation's rectangle.**
///
/// # What it is, and the two things it is not
///
/// It is [`letibot_vt::Screen`] — a rectangle of cells, a cursor, a pen and an alternate
/// buffer, driven by the bytes a pty's far end wrote — plus the three facts a head needs about
/// the program that is drawing in it. **It is not an emulator of this head's own**: there is one
/// in `letibot-vt`, it is a crate *below* this one, and this head's half of it is
/// `letibot_ui::ansi::pane_rows`. And it
/// is not the conversation: nothing here is a transcript row, and when the pane closes the
/// transcript is exactly what it was.
///
/// # The rectangle is the contract
///
/// [`TermPane::rows`] is `letibot_ui::ansi::pane_rows(&mut screen, cols, room, palette)` and it
/// returns **exactly `room` rows** — the same property `ansi.rs` keeps for the pane it was written for. That is the
/// whole of *"the composer, header and status keep their rows"*: the pane takes the
/// conversation's rectangle and gives it back, so nothing above it moves by a line when it
/// opens and nothing below it loses a row it had.
///
/// # The way out, and why it is `ctrl-\`
///
/// **`Ctrl-\` (0x1c), and it is intercepted on the raw byte stream before a single byte is
/// forwarded**, so the program never receives it and cannot trap it — which is the whole
/// requirement. The tree chose it long before this branch: letibot's own decoder listed the
/// bytes with no arm and says *"`0x1c`-`0x1e` are the only bytes left in this table with no
/// arm, and none of the three has a mnemonic worth having"*. It is not a tty control character
/// (`cfmakeraw` clears `IXON`/`IEXTEN`, and `0x1c` is `QUIT` only under `ISIG`, which raw mode
/// clears), it is not a chord this head binds, and it is not one a program expects to be
/// typed at it.
///
/// **Esc was the other candidate and it is wrong**: Esc is a key `vi`, `mc`, `nano` and every
/// `less` read on purpose — it is *cancel*, it is the first byte of every meta sequence, and a
/// pane that ate it would be a pane the program could not be driven from. `Ctrl-\` is the one
/// key whose whole meaning is *stop this*, and the operator asked for exactly that: *"one
/// unambiguous way out."*
///
/// # What `ctrl-\` does, and what it deliberately does not
///
/// **It detaches. It does not end anything.** The rectangle goes, the conversation comes back,
/// the composer has its rows and its keys — and **nothing is sent at all**: the program keeps
/// running on the daemon's pty, the daemon keeps the screen it has been keeping since protocol
/// 32, the slot stays occupied, and a later `!term` attaches back to the same run.
///
/// **That is a correction, and the operator's words are the reason:** *"but i dont want it to
/// exit"*. This key used to send [`Action::TermClose`], which ends the pane's cgroup — so
/// leaving `nano` killed it, and the attach work bought nothing for anything a person cares
/// about. **The default is the non-destructive act on purpose**: a person reaching for *get me
/// out of here* must not lose an hour's editing, and a program that must be ended can be asked
/// for by name.
///
/// **Ending is `!term close`**, typed at the composer (so it works whether the pane is on the
/// screen or not), and it is confirmed first — see [`App::term_ask`]. The two acts are as
/// different as they can be: one sends nothing and keeps everything, the other sends one frame
/// and kills a process tree, and neither is reachable by the other's key.
pub(crate) struct TermPane {
    /// The program's screen, fed the daemon's bytes and asked for rows.
    pub(crate) screen: letibot_vt::Screen,
    /// The line the operator submitted, verb included — kept for the one sentence this head
    /// says when the pane ends, so *what ended* is not a mystery.
    pub(crate) line: String,
    /// **The rectangle last sent to the daemon.** A resize is a frame, and a frame per tick
    /// would be a frame per keystroke — this is what makes `TermResize` fire on a change and
    /// not on a redraw.
    pub(crate) sent: (usize, usize),
    /// **The operator has left and an ending is on its way.** Set when the head sends
    /// [`Action::TermClose`] — the confirmed `!term close` — and it stops the keys: between the
    /// frame and the daemon's `TermEnded` there is a kill in flight, and a byte written into a
    /// pty whose program is being signalled is a byte nobody will read. The window is
    /// milliseconds, and this is what makes it closed rather than merely short.
    ///
    /// **Not the detach flag.** Leaving (`ctrl-\`) sends nothing and waits for nothing — see
    /// [`TermPane::detached`].
    pub(crate) closing: bool,
    /// **The operator left with `ctrl-\`, and the program is still running.**
    ///
    /// The rectangle is not drawn, the composer has its rows and its keys back — and the pane
    /// is **kept**: the bytes keep arriving into this screen, and the ending, whenever it comes,
    /// is filed as the row it would have been had the operator been looking. That is the whole
    /// of *detaching is not ending*, and it is why [`App::detach`] drops nothing.
    pub(crate) detached: bool,
    /// **How far into the scrollback the operator has wheeled**, in rows; `0` is the live screen.
    /// See [`TermPane::route`] for what moves it and what puts it back.
    pub(crate) back: usize,
    /// **Where the pane was last drawn**, in the terminal's cells (zero-based column and row of
    /// its top-left) — what turns the operator's click into the program's. Recorded by the draw,
    /// because the draw is the one layer that knows; see [`TermPane::route`].
    pub(crate) origin: (usize, usize),
    /// **The start of a mouse report that the read ended inside** (R74 item 30), kept for the
    /// next read rather than forwarded in halves. See [`report_at`].
    pub(crate) held: Vec<u8>,
    /// **The daemon's number for this pane** (protocol 44), `None` between the Enter that put
    /// the rectangle up and the `TermAttached` that names it. Every frame about a pane carries
    /// its number, and this head feeds only the one it holds.
    pub(crate) pane: Option<u32>,
    /// **Keys typed before the number arrived**, sent in order the moment it does — see
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

    /// **The pane's rows for this frame** — exactly `room` of them, which is the property the
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

    /// **The pane's share of one read: the bytes the program is sent**, with the operator's mouse
    /// taken out and — where the program asked for it — put back in the program's own terms.
    ///
    /// # The mouse, and whose it is (R69's open item, R74 item 22)
    ///
    /// rano turns on button and drag tracking in the SGR encoding for the head's own clicks and
    /// wheel, so every notch and drag the operator makes over the pane arrives here as
    /// `ESC [ < b ; x ; y M`. R67 item 6 found those reaching the program verbatim — a wheel over
    /// `!term bash` typed `ESC[<64;30;12M` at the prompt — and stripped them all, which was right
    /// for a program that had not asked and wrong for one that had: `mc`'s click-to-select and
    /// `less`'s wheel were the named costs. [`letibot_vt::Screen::mouse`] now records what the
    /// program asked for, so each report is decided here, one at a time:
    ///
    /// | the program asked for | a click or drag | a wheel notch |
    /// |---|---|---|
    /// | a mouse (`?1000`/`?1002`/`?1003`) | **sent**, at the pane's coordinates, in its encoding (`?1006` or X10), filtered to the level it asked for | **sent** |
    /// | nothing, on the alternate screen | dropped | **three cursor keys** — `less`, `man`, `vim` scroll by them, which is xterm's *alternate scroll* |
    /// | nothing, on the main screen | dropped | **the scrollback** (R60 item 1): [`WHEEL_ROWS`] back or forward, and nothing is sent |
    ///
    /// A report outside the pane's rectangle is dropped in every row of that table: the operator
    /// clicked the chrome, and the program has no column there to be told about.
    ///
    /// # What puts the scrollback back
    ///
    /// **Any byte that is the program's** — a key, a paste, a click it asked for. A person typing
    /// is talking to the live program, and a view left scrolled back while they type would be a
    /// screen that does not show what their keys did. Output does not: a program that keeps
    /// printing while the operator reads back does not yank the page from under them, and the
    /// rows it pushes past the top are counted into `back` so the page they are reading stays
    /// put.
    ///
    /// # A report the read cut in half (R74 item 30)
    ///
    /// The tty is read in coalesced chunks, and a 12-byte report can straddle two of them. It
    /// used to pass through in halves — the tail as text at the program's prompt. The start of a
    /// report at the end of a read is now held in [`TermPane::held`] and put in front of the next
    /// read, where it is either finished (and decided as above) or turns out not to have been a
    /// report and goes down as the bytes it was.
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

    /// One report, decided — see [`TermPane::route`]'s table. Returns the bytes the program is
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

    /// **The program's bytes, into the screen** — and the scrollback view held where the operator
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

    /// **The last rows the program left on the screen**, for the row its ending becomes.
    ///
    /// **The screen and not the byte stream**: what a program *printed* is what a person can
    /// read, and the bytes behind it are cursor addressing, `\r` and half a UTF-8 character —
    /// the raw stream is for [`letibot_vt::Screen`] and this is for a transcript row.
    ///
    /// **Blank rows are dropped and the tail is kept**, because the two shapes are different
    /// and the same rule answers both: a program that dies with a sentence about why put that
    /// sentence last (and its rectangle is otherwise empty), and a full-screen program leaves
    /// a grid of mostly blanks whose last rows are where its status line is. Capped at
    /// [`PANE_LAST_LINES`], which is what makes an unfolded note safe to draw.
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

/// **What this head believes about the session's pane** — the daemon's answer to
/// [`ClientFrame::TermStatus`](letibot_sessionlog::protocol::ClientFrame::TermStatus), as this
/// head holds it.
///
/// # Why it is a three-state and not an `Option`
///
/// `None` would mean two different things at once, and the difference decides whether a
/// `!term close` **asks or refuses**:
///
/// * **`Unasked`** — nobody has answered yet. The head has just attached, or switched, and the
///   question is in flight. A verb that read this as *no pane* would refuse to end a program
///   the operator can see, which is the one case the read exists for;
/// * **`None`** — the daemon said there is no live pane. That is a fact, and it is what makes
///   `!term close` a sentence rather than a card;
/// * **`Running(command)`** — the daemon said this is running in it, and it is what a
///   confirmation names.
///
/// **A fact about NOW, refreshed rather than accumulated.** It is set by the three frames that
/// can know ([`ServerFrame::TermStatus`], `TermAttached`, `TermEnded`), reset to `Unasked` on
/// every `Hello` — a head that has just been seated somewhere does not know what is in the
/// pane there — and never derived from anything durable, because there is nothing durable about
/// it: a detach is not an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PaneFact {
    /// Nobody has asked yet: the read is in flight, or this head has not attached.
    Unasked,
    /// The daemon said this session has no live pane.
    None,
    /// The daemon said these are running in it, by number — never empty (protocol 44: a
    /// session may hold several).
    Running(Vec<letibot_sessionlog::PaneBrief>),
}

impl PaneFact {
    /// **This fact with one pane added or renamed** — what a `TermAttached` tells every head.
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

    /// **This fact with one pane gone** — what a `TermEnded` tells every head. A list that
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

/// **The confirmation that ends a pane** — the daemon's own question about killing something,
/// and never the program's question about itself.
///
/// # Why it is not the prompt card
///
/// `SessionEvent::PromptRequested`'s card answers **the program**: it is a line written into a
/// pipe the daemon holds, drawn in the open, answered by typing and Enter. This card answers
/// **the daemon**: whether a process tree should die. The operator's rule is that the two must
/// not be confusable — *"a person must never be unsure which one they are looking at, and
/// neither may be answerable by the other's keystroke"* — so they are kept apart in every way a
/// person can see or type:
///
/// * **different words** — this one names the program and says what ending it does, and it
///   never quotes the program's own output (see [`App::term_ask_lines`]);
/// * **different key** — the yes here is `y`, and it is deliberately **not Enter**, because
///   Enter is the prompt card's own (an empty line is a real answer there) and the composer's.
///   A stray Enter cannot kill anything;
/// * **and the safe default** — *anything that is not a deliberate yes* cancels, Esc included.
///   A confirmation whose default is the destructive answer is not a confirmation.
///
/// **A prompt that arrives while this is up takes the screen back**, and the arm that does it
/// says why: a program that has just asked a question must not be killable by the answer to it.
///
/// **And it outranks a card that was already waiting**, which is this head's rule for two
/// questions at once rather than a choice made here: the newest question owns the screen and the
/// older one waits its turn (`App::decision_card`'s slot says the same thing about a decision
/// card under a picker). So a decision card that was up when the operator typed `!term close`
/// comes back when this is answered, cancelled or confirmed — and `esc` is the way out of the
/// stack, one question at a time, because *anything that is not a yes* cancels.
#[derive(Debug, Clone)]
pub(crate) struct TermAsk {
    /// **What is about to end, as a line a person reads** — `!term nano notes.txt`, the spelling
    /// the operator typed at the composer when there is one, or `!term <command>` rebuilt from
    /// the daemon's own word when the pane is another head's.
    pub(crate) line: String,
    /// **Which pane** — the daemon's number, which is what `TermClose` carries. The card names
    /// it beside the line, so a person ending one of several knows which one.
    pub(crate) pane: u32,
}
