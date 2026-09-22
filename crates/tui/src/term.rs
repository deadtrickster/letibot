//! Raw mode, the alternate screen, the window size, and key decoding.
//!
//! Fifty lines of `termios` instead of a TUI framework, for the same reason
//! `letibot-turn` writes its own HTTP: what this needs is byte-level control of one
//! well-understood interface, and a framework brings an event loop that would then
//! be the second one in the process.
//!
//! # The restore is the whole risk
//!
//! A head that panics with the terminal in raw mode leaves the operator's shell
//! unusable. [`Terminal`] restores on `Drop`, and the panic hook installed by
//! [`Terminal::enter`] restores before the message is printed — otherwise the
//! backtrace prints as a staircase and the shell has no echo.
//!
//! # The modes this file turns on, and what each one buys
//!
//! | sequence | on enter | why |
//! |---|---|---|
//! | `?1049h` | alternate screen | the head owns the screen and gives it back |
//! | `?25l` / `?25h` | cursor | parked on the composer by [`Terminal::draw_with_cursor`] |
//! | `?2004h` | bracketed paste | a paste arrives as **one** [`Key::Paste`] |
//! | `\x1b[2 q` | steady block cursor | the composer's only affordance is the caret |
//! | `?2026h` / `?2026l` | synchronised output | see below |
//!
//! **DEC 2026 (synchronised output)** wraps every frame the head emits. A frame is
//! a sequence of absolute cursor moves and erases; without it a terminal is free
//! to present the screen halfway through one, which is a torn frame — and a torn
//! frame at 10 Hz is exactly what "flicker" describes. Terminals that do not know
//! the mode ignore the private sequence, so it costs eight bytes per frame that
//! writes anything and nothing at all on an idle one.
//!
//! # Counting what was written, because a capture cannot
//!
//! `tmux capture-pane` shows the *rendered* pane, so it can say what the screen
//! ended up looking like and never how much was written to get there — and
//! "how much of the glass did that redraw" is the question a lost mouse
//! selection asks, since a terminal's selection is over drawn cells. So the
//! write path counts itself: [`WriteStats`], reported on restore when
//! `LETIBOT_TUI_WRITE_STATS` names a file (or `-` for stderr).
//!
//! ```text
//! LETIBOT_TUI_WRITE_STATS=/tmp/before letibot-tui --replay session.jsonl
//! ```
//!
//! # Reading is not a single fixed-size read
//!
//! It was: one 64-byte buffer, decoded, and anything that did not fit was the
//! next read's problem — except a read that ends **mid-UTF-8** failed
//! `from_utf8` and was dropped on the floor, silently. Pasting a stack trace
//! into the composer lost bytes. [`Terminal::keys`] now loops while the buffer
//! keeps filling or a bracketed paste is still open, and carries an incomplete
//! tail — a partial UTF-8 sequence, a half-arrived escape, an unterminated paste
//! — into the next read instead of discarding it.

use std::io::{Read, Write};
use std::os::fd::AsRawFd;

use crate::app::Key;

pub struct Terminal {
    original: libc::termios,
    fd: i32,
    entered: bool,
    /// The frame currently on the glass. [`Terminal::draw`] writes the difference
    /// against it and nothing else; see the note on flicker.
    shown: std::cell::RefCell<Vec<String>>,
    /// Where the cursor was left, so an unchanged frame does not even move it.
    cursor: std::cell::Cell<Option<(usize, usize)>>,
    /// Frames drawn, and frames that needed no bytes at all. Instrumentation kept
    /// in the shipping type for the same reason `IncrementalMarkdown::bytes_lexed`
    /// is: "is it repainting when nothing changed" is unanswerable after the fact.
    frames: std::cell::Cell<u64>,
    silent: std::cell::Cell<u64>,
    /// The rest of the encoder: bytes, row rewrites, repeated payloads, screen
    /// erases. See [`WriteStats`].
    stats: std::cell::Cell<WriteStats>,
    /// The previous frame's payload, kept only while `stats_to` is set — a clone
    /// per frame is not something an uninstrumented head should pay for.
    prev_payload: std::cell::RefCell<String>,
    /// `LETIBOT_TUI_WRITE_STATS`: a path to write the line to on restore, or `-`
    /// for stderr. `None` switches the whole encoder off.
    stats_to: Option<String>,
    /// Bytes read but not yet decodable: a partial UTF-8 sequence, an escape
    /// that arrived in halves, or a bracketed paste whose terminator has not
    /// come. Carried to the next read rather than dropped.
    pending: std::cell::RefCell<Vec<u8>>,
    /// The terminal size the last frame was painted at, and whether the next
    /// frame must erase everything before it paints.
    last_size: std::cell::Cell<(usize, usize)>,
    full: std::cell::Cell<bool>,
}

/// How much is read at once. Large enough that a paste is one or two reads
/// rather than fifty, and it is a ceiling rather than a promise: the loop in
/// [`Terminal::keys`] keeps going while the buffer keeps filling.
const READ_CHUNK: usize = 8192;

/// A hard ceiling on the carry, so a terminal that opens a bracketed paste and
/// never closes it cannot grow this without bound. Past it the carry is decoded
/// as-is — visibly wrong beats invisibly unbounded.
const MAX_PENDING: usize = 4 * 1024 * 1024;

/// What one run wrote to the terminal.
///
/// `frames` and `silent` were already here and they answer *"is it drawing when
/// nothing changed"*. They cannot answer the question a lost selection asks,
/// which is **how much of the glass got rewritten** — a terminal's selection is
/// over drawn cells, so the quantity that destroys one is `rows`, not `frames`.
///
/// `rows` counts row rewrites by counting the `ESC[K` that begins each one.
/// [`paint_full`] is the only thing in this file that emits that sequence and it
/// emits exactly one per row it repaints, so the count is the fact rather than a
/// proxy for it.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct WriteStats {
    /// Calls to [`Terminal::draw_with_cursor`].
    pub frames: u64,
    /// Of those, the ones that wrote no bytes at all.
    pub silent: u64,
    /// Bytes written to stdout, the `?2026` wrappers included.
    pub bytes: u64,
    /// Row rewrites — the cells a selection would have been sitting on.
    pub rows: u64,
    /// Frames whose payload was byte-identical to the previous **written** one.
    /// The 10 Hz full-repaint regression, in the form it was found in.
    pub repeats: u64,
    /// Whole-screen erases (`ESC[2J`): a resize, or Ctrl-L.
    pub clears: u64,
}

impl WriteStats {
    /// One line, so a replay can be diffed against another replay.
    pub fn line(&self) -> String {
        let per = if self.frames > self.silent {
            self.bytes as f64 / (self.frames - self.silent) as f64
        } else {
            0.0
        };
        format!(
            "frames={} silent={} written={} bytes={} bytes_per_written={:.1} \
             rows={} repeats={} clears={}",
            self.frames,
            self.silent,
            self.frames - self.silent,
            self.bytes,
            per,
            self.rows,
            self.repeats,
            self.clears,
        )
    }
}

impl Terminal {
    /// Put the terminal in raw mode on the alternate screen.
    ///
    /// `Err` when stdin is not a tty, which is the `--replay` and CI case: the
    /// caller then renders once to stdout instead of failing.
    pub fn enter() -> std::io::Result<Terminal> {
        let fd = std::io::stdin().as_raw_fd();
        if unsafe { libc::isatty(fd) } != 1 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "stdin is not a terminal",
            ));
        }
        let mut original: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(fd, &mut original) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut raw = original;
        unsafe { libc::cfmakeraw(&mut raw) };
        // A 100 ms read timeout so the render loop can also service the network
        // without a second thread poking at stdin.
        raw.c_cc[libc::VMIN] = 0;
        raw.c_cc[libc::VTIME] = 1;
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut out = std::io::stdout();
        // Alternate screen; hide the cursor until a frame says where it goes;
        // bracketed paste, so a paste is one key and a pasted newline does not
        // submit the prompt; a steady block cursor, because the composer's whole
        // affordance is that caret and a one-pixel bar is not one; button-event
        // mouse tracking with SGR encoding, so the wheel scrolls the transcript.
        // The app acts on the wheel only — clicks and drags are decoded and
        // dropped, and selecting text stays the terminal's own Shift+drag.
        let _ = out.write_all(b"\x1b[?1049h\x1b[?25l\x1b[?2004h\x1b[?1002h\x1b[?1006h\x1b[2 q");
        let _ = out.flush();

        // Restore before anything is printed, or the panic message is a staircase.
        let saved = original;
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore(fd, &saved);
            prev(info);
        }));

        Ok(Terminal {
            original,
            fd,
            entered: true,
            shown: std::cell::RefCell::new(Vec::new()),
            cursor: std::cell::Cell::new(None),
            frames: std::cell::Cell::new(0),
            silent: std::cell::Cell::new(0),
            stats: std::cell::Cell::new(WriteStats::default()),
            prev_payload: std::cell::RefCell::new(String::new()),
            stats_to: std::env::var("LETIBOT_TUI_WRITE_STATS")
                .ok()
                .filter(|s| !s.is_empty()),
            pending: std::cell::RefCell::new(Vec::new()),
            last_size: std::cell::Cell::new((0, 0)),
            full: std::cell::Cell::new(true),
        })
    }

    /// Columns and rows, or a sane default when `TIOCGWINSZ` says nothing.
    pub fn size(&self) -> (usize, usize) {
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        if unsafe { libc::ioctl(self.fd, libc::TIOCGWINSZ, &mut ws) } == 0
            && ws.ws_col > 0
            && ws.ws_row > 0
        {
            (ws.ws_col as usize, ws.ws_row as usize)
        } else {
            (100, 30)
        }
    }

    /// Read whatever keys are available. Returns after at most ~100 ms of quiet.
    ///
    /// The loop continues while the last read **filled** the buffer — the only
    /// signal available under `VMIN=0 VTIME=1` that more is queued — or while a
    /// bracketed paste is open, which is the precise signal and the one that
    /// matters, since a paste is the case this exists for. Whatever is left
    /// undecodable is carried, not dropped.
    pub fn keys(&self) -> Vec<Key> {
        let mut buf = [0u8; READ_CHUNK];
        let mut pending = self.pending.borrow_mut();
        while let Ok(n) = std::io::stdin().read(&mut buf) {
            if n == 0 {
                break;
            }
            pending.extend_from_slice(&buf[..n]);
            let more = n == buf.len() || (paste_open(&pending) && pending.len() < MAX_PENDING);
            if !more {
                break;
            }
        }
        if pending.is_empty() {
            return Vec::new();
        }
        let force = pending.len() >= MAX_PENDING;
        let (keys, used) = decode_prefix(&pending, force);
        pending.drain(..used);
        keys
    }

    /// Paint the **difference** between this frame and the one on the glass.
    ///
    /// # Why this is not a full repaint
    ///
    /// It used to be, and that was the flicker. The loop wakes every 100 ms
    /// whether or not anything arrived, and the old `draw` unconditionally sent
    /// `ESC[H`, then `ESC[K` and the text for every row, then `ESC[J`. Measured over
    /// one 28-second session: 269 frames, **221 of them byte-identical to the frame
    /// before**. So the whole screen was erased and repainted ten times a second
    /// for a screen that was not changing — which is visible as flicker, throws away
    /// any selection the operator makes, and pins a core.
    ///
    /// Two properties, and the second matters more than the first:
    ///
    /// 1. Only rows whose text changed are written, each addressed absolutely, so
    ///    nothing is erased that is about to be rewritten identically.
    /// 2. **A frame equal to the last one writes zero bytes.** An idle head is
    ///    silent on its output, not merely cheap.
    ///
    /// A resize is a full repaint, once, because every row moved.
    pub fn draw(&self, lines: &[String]) {
        self.draw_with_cursor(lines, None)
    }

    /// As [`Terminal::draw`], with the terminal's own cursor parked at `(row, col)`
    /// — zero-based — and made visible there.
    ///
    /// A text field with no caret is the kind of thing that reads as "the program
    /// is not listening", and the cursor is free: the terminal already has one.
    pub fn draw_with_cursor(&self, lines: &[String], cursor: Option<(usize, usize)>) {
        // The frame counter moves for every *attempt*, which is what "frames drawn"
        // has always meant here: the caller asked for a frame and one was composed.
        // Whether its bytes reached the glass is the question `paint_to` answers, and
        // it is a different number.
        self.frames.set(self.frames.get() + 1);
        let mut out = std::io::stdout();
        let _ = self.paint_to(&mut out, lines, cursor);
    }

    /// **One pass: build the bytes, write them, and adopt the glass-state they leave —
    /// or adopt nothing at all.**
    ///
    /// # The bug this exists for: a frame that half-wrote and was believed
    ///
    /// The memory of the glass used to be updated *inside* `paint_full`, while the
    /// bytes were still being built, and the write afterwards was `let _ = …`, which
    /// throws the error away. So a write that died part-way — a pty that closed, a
    /// terminal that went away mid-frame, a short write — left the head **believing rows
    /// were on the glass that it had never written**. The next frame skipped them, on
    /// the legitimate rule that a row whose text has not changed need not be sent, and
    /// the hole was therefore permanent: nothing in this head could discover it except
    /// a full repaint, which is Ctrl-L or a **resize**.
    ///
    /// That last word is the operator's own report, and it is why this is worth a
    /// paragraph rather than a line: *"when i expand tools with Ct scroll stops working,
    /// even after collapsing back. i have to switch byobu windows back and forth"* —
    /// switching windows resizes, a resize forces `full`, and the frame repaired itself.
    /// The explanation that went in the log was escapes in a payload (which is real, and
    /// is fixed in `without_control`); **this is the other half, and it was the half that
    /// explained the repair.**
    ///
    /// # The rule, and why it is "forget" rather than "remember what landed"
    ///
    /// A `write_all` that fails may have written any prefix of its buffer, so the head
    /// cannot know which rows are up. It could parse its own output for cursor moves and
    /// count what fit — and a mistake there puts the hole back, silently, which is the
    /// whole class of defect being fixed. So a failed write means **the glass is
    /// unknown**: the memory is cleared and `full` is set, and the next frame repaints
    /// everything. One extra frame after a failure, and no way to be wrong.
    ///
    /// `writers` are split out for the same reason `paint_full` takes `&[String]`: this
    /// is the decision worth a test, and a test that needs a pty to reach it is a test
    /// nobody runs. Returns whether the glass was invalidated, which is what a test
    /// asserts and nothing else reads.
    fn paint_to(
        &self,
        out: &mut dyn std::io::Write,
        lines: &[String],
        cursor: Option<(usize, usize)>,
    ) -> bool {
        let mut st = self.stats.get();
        st.frames += 1;
        // A resize is the one thing that really does move every row: the terminal
        // reflowed the glass and this head's memory of it is now fiction. A frame
        // that merely changed *height* — which the composer does every time a
        // prompt wraps onto another row — is not that, and erasing the screen for
        // it is a flash on every wrap.
        let size = self.size();
        if size != self.last_size.get() {
            self.last_size.set(size);
            self.full.set(true);
        }
        let (s, next) = paint_full(
            &self.shown.borrow(),
            lines,
            cursor,
            self.cursor.get(),
            self.full.replace(false),
        );
        if s.is_empty() {
            self.silent.set(self.silent.get() + 1);
            st.silent += 1;
            self.stats.set(st);
            // **Adopted, and nothing was written.** The frame needed no bytes, so the
            // memory it describes is already true — and taking it keeps `shown` the same
            // length as the frame, which is what the next frame diffs against.
            *self.shown.borrow_mut() = next;
            return false;
        }
        // The encoder, before the bytes go out. `?2026h` and `?2026l` are eight
        // bytes each and they are bytes the terminal really is sent, so they are
        // counted rather than discounted as chrome.
        let wrote = out
            .write_all(b"\x1b[?2026h")
            .and_then(|()| out.write_all(s.as_bytes()))
            .and_then(|()| out.write_all(b"\x1b[?2026l"))
            .and_then(|()| out.flush());
        if wrote.is_err() {
            // **No record of a frame that may not be on the glass.** Cleared and marked
            // unknown, so the next frame is a `full` repaint: see this method's doc for
            // why "forget" beats "work out how much landed".
            self.shown.borrow_mut().clear();
            self.full.set(true);
            self.stats.set(st);
            return true;
        }
        // **The bytes are out, so now the memory is true.** This is the whole fix: the
        // adoption is one statement later than it was, and that one statement is the
        // difference between a diff against the glass and a diff against an intention.
        st.bytes += s.len() as u64 + 16;
        st.rows += s.matches("\x1b[K").count() as u64;
        st.clears += s.matches("\x1b[2J").count() as u64;
        if self.stats_to.is_some() {
            let mut prev = self.prev_payload.borrow_mut();
            if *prev == s {
                st.repeats += 1;
            }
            prev.clear();
            prev.push_str(&s);
        }
        self.stats.set(st);
        *self.shown.borrow_mut() = next;
        self.cursor.set(cursor);
        false
    }

    /// Forget what is on the glass, so the next draw repaints everything.
    ///
    /// Ctrl-L, and the only honest answer to "something else wrote to my terminal":
    /// the diff is against a memory of the screen, and anything that writes behind
    /// the head's back makes that memory wrong.
    pub fn invalidate(&self) {
        self.shown.borrow_mut().clear();
        self.full.set(true);
    }

    /// Frames drawn, and how many of those wrote nothing. The second number is the
    /// flicker regression, in a form that can be asserted on.
    pub fn frame_counts(&self) -> (u64, u64) {
        (self.frames.get(), self.silent.get())
    }

    /// Everything this run wrote. See [`WriteStats`].
    pub fn write_stats(&self) -> WriteStats {
        self.stats.get()
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        if self.entered {
            restore(self.fd, &self.original);
        }
        // After the restore, so the line lands on a terminal that is out of raw
        // mode and off the alternate screen — a report printed before it scrolls
        // away with the screen it was printed on.
        if let Some(to) = self.stats_to.clone() {
            let line = format!("letibot-tui write stats: {}\n", self.stats.get().line());
            if to == "-" || to == "1" {
                let _ = std::io::stderr().write_all(line.as_bytes());
            } else {
                use std::io::Write as _;
                if let Ok(mut f) = std::fs::File::create(&to) {
                    let _ = f.write_all(line.as_bytes());
                }
            }
        }
    }
}

/// The bytes that turn `shown` into `lines`, **and the memory of the glass those bytes
/// would leave behind**.
///
/// Two values rather than one, and the second is the fix for a defect this shared with
/// the other head: the function used to update the memory *while building the string*,
/// so a write that died part-way left the head claiming rows were on the glass that it
/// had never written. Every later frame then skipped them — `shown[i] == *l` — and the
/// hole was permanent until a resize or Ctrl-L. See [`Terminal::paint_to`], which is
/// where the two are finally put together, and `a_paint_that_dies_part_way_leaves_no_
/// record` for the failure measured.
///
/// The new memory is built **from the old one plus the lines**, so a caller that has not
/// written anything yet holds the old memory and nothing else: there is no window in
/// which the head believes a byte it has not sent.
pub fn paint(
    shown: &[String],
    lines: &[String],
    cursor: Option<(usize, usize)>,
    prev_cursor: Option<(usize, usize)>,
) -> (String, Vec<String>) {
    paint_full(shown, lines, cursor, prev_cursor, shown.is_empty())
}

/// As [`paint`], with `full` forcing a whole-screen erase first.
///
/// # A frame that got taller is not a resize
///
/// This used to erase the screen whenever the row count changed, on the argument
/// that "every row moved". That was true while the chrome was a fixed two lines.
/// It is not true now: the composer grows a row every time a prompt wraps and the
/// in-flight line appears and disappears with the turn, so the row count changes
/// while you type — and erasing the screen for it is a flash per wrap, which is
/// the thing this function exists to prevent. Rows are addressed absolutely, so a
/// frame of a different height needs only the rows that differ, plus an erase of
/// the rows that no longer exist.
///
/// `full` is for the two cases where the glass really is unknown: a resize, and
/// Ctrl-L — *"the diff is against a memory of the screen, and anything that
/// writes behind the head's back makes that memory wrong"*.
pub fn paint_full(
    shown: &[String],
    lines: &[String],
    cursor: Option<(usize, usize)>,
    prev_cursor: Option<(usize, usize)>,
    full: bool,
) -> (String, Vec<String>) {
    let mut s = String::new();
    // **The glass-state this frame is building TOWARD**, kept apart from the one it is
    // diffed against. A copy rather than a second pass because the copy is one row per
    // screen — fifty-odd `String`s next to the frame the caller is about to write — and
    // the alternative is a diff list whose indices the caller has to re-apply, which is
    // one more thing to get wrong in the one place where being wrong is silent.
    let mut next: Vec<String> = shown.to_vec();
    if full {
        s.push_str("\x1b[2J");
        next.clear();
    }
    // Rows the frame no longer has: erase them, rather than the whole screen.
    for i in lines.len()..next.len() {
        s.push_str(&format!("\x1b[{};1H\x1b[0m\x1b[K", i + 1));
    }
    // Rows the frame gained are blank on the glass — either it was just erased,
    // or the loop above erased them when the frame last shrank past them.
    next.resize(lines.len(), String::new());
    for (i, l) in lines.iter().enumerate() {
        if next[i] == *l {
            continue;
        }
        s.push_str(&format!("\x1b[{};1H\x1b[0m\x1b[K", i + 1));
        s.push_str(l);
        next[i] = l.clone();
    }
    if s.is_empty() && cursor == prev_cursor {
        return (String::new(), next);
    }
    match cursor {
        Some((r, c)) => s.push_str(&format!("\x1b[{};{}H\x1b[?25h", r + 1, c + 1)),
        None => s.push_str("\x1b[?25l"),
    }
    (s, next)
}

/// **A terminal that writes where it is told, for the tests that are about the encoder
/// rather than about the pty.** `enter` needs a real terminal; nothing about the diff,
/// the cursor or the failure policy does.
#[cfg(test)]
impl Terminal {
    fn headless() -> Terminal {
        Terminal {
            original: unsafe { std::mem::zeroed() },
            fd: -1,
            entered: false,
            shown: std::cell::RefCell::new(Vec::new()),
            cursor: std::cell::Cell::new(None),
            frames: std::cell::Cell::new(0),
            silent: std::cell::Cell::new(0),
            stats: std::cell::Cell::new(WriteStats::default()),
            prev_payload: std::cell::RefCell::new(String::new()),
            stats_to: None,
            pending: std::cell::RefCell::new(Vec::new()),
            last_size: std::cell::Cell::new((80, 24)),
            full: std::cell::Cell::new(true),
        }
    }
}

fn restore(fd: i32, original: &libc::termios) {
    unsafe { libc::tcsetattr(fd, libc::TCSANOW, original) };
    let mut out = std::io::stdout();
    // Every mode `enter` turned on, off again, in the reverse order: end any open
    // synchronised update, mouse tracking off, bracketed paste off, the cursor
    // shape back to whatever the operator's terminal had, then show it and leave
    // the alternate screen.
    let _ = out.write_all(b"\x1b[?2026l\x1b[?1006l\x1b[?1002l\x1b[?2004l\x1b[0 q\x1b[?25h\x1b[?1049l");
    let _ = out.flush();
}

/// Is a bracketed paste open at the end of `b`?
///
/// The signal that says "keep reading": a paste's terminator is the only precise
/// evidence that more bytes are on their way, and without it a 40 KB paste is
/// decided by a 100 ms timeout in the middle of somebody's stack trace.
fn paste_open(b: &[u8]) -> bool {
    let start = rfind(b, PASTE_START);
    let end = rfind(b, PASTE_END);
    match (start, end) {
        (Some(s), Some(e)) => s > e,
        (Some(_), None) => true,
        _ => false,
    }
}

const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

fn rfind(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len())
        .rev()
        .find(|&i| &hay[i..i + needle.len()] == needle)
}

/// Decode a read buffer into keys.
///
/// Convenience over [`decode_prefix`] for a caller with a complete buffer — a
/// test, or a `--replay` script. Anything trailing and incomplete is decoded as
/// best it can be, which is what "this is all there is" means.
pub fn decode(b: &[u8]) -> Vec<Key> {
    decode_prefix(b, true).0
}

/// Decode as much of `b` as is unambiguously complete, and say how many bytes
/// that was.
///
/// The returned count is the contract: everything past it is an **incomplete
/// tail** — a UTF-8 sequence cut in half, a CSI whose final byte has not
/// arrived, a bracketed paste still open — and the caller carries it into the
/// next read. The old decoder had no such notion, so a read that ended
/// mid-character failed `from_utf8` and the arm dropped the bytes without a
/// word. A person pasting an error message into an agent is not an edge case.
///
/// `force` decodes the tail anyway, for the last read of a stream and for the
/// ceiling at [`MAX_PENDING`].
pub fn decode_prefix(b: &[u8], force: bool) -> (Vec<Key>, usize) {
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        // Bracketed paste, first: everything inside it is content, including
        // bytes that would otherwise be keys. This is what stops a pasted
        // newline from submitting the prompt.
        if b[i..].starts_with(PASTE_START) {
            let body = i + PASTE_START.len();
            match find(&b[body..], PASTE_END) {
                Some(k) => {
                    out.push(Key::Paste(
                        String::from_utf8_lossy(&b[body..body + k]).into_owned(),
                    ));
                    i = body + k + PASTE_END.len();
                    continue;
                }
                None if force => {
                    out.push(Key::Paste(String::from_utf8_lossy(&b[body..]).into_owned()));
                    return (out, b.len());
                }
                None => return (out, i),
            }
        }
        let c = b[i];
        match c {
            0x1b => match escape(&b[i..], force) {
                Step::Emit(k, n) => {
                    if let Some(k) = k {
                        out.push(k);
                    }
                    i += n;
                }
                Step::Incomplete => return (out, i),
            },
            0x03 => {
                out.push(Key::CtrlC);
                i += 1;
            }
            0x04 => {
                out.push(Key::Eof);
                i += 1;
            }
            // Readline's motion keys, which are muscle memory in every shell.
            0x01 => {
                out.push(Key::Home);
                i += 1;
            }
            0x02 => {
                out.push(Key::Left);
                i += 1;
            }
            0x05 => {
                out.push(Key::End);
                i += 1;
            }
            0x06 => {
                out.push(Key::Right);
                i += 1;
            }
            0x0b => {
                out.push(Key::KillToEnd);
                i += 1;
            }
            0x15 => {
                out.push(Key::KillToStart);
                i += 1;
            }
            0x17 => {
                out.push(Key::KillWordBack);
                i += 1;
            }
            0x19 => {
                out.push(Key::Yank);
                i += 1;
            }
            // Ctrl+Z and Ctrl+_ both undo. Raw mode means Ctrl+Z is not a suspend
            // here, and a composer with no undo is what makes a kill frightening.
            0x1a | 0x1f => {
                out.push(Key::Undo);
                i += 1;
            }
            // The two folds and a redraw. Control keys rather than plain letters
            // because every printable character has to remain typeable — a head
            // whose `r` means "collapse" cannot be used to ask a question.
            0x12 => {
                out.push(Key::CtrlR);
                i += 1;
            }
            0x14 => {
                out.push(Key::CtrlT);
                i += 1;
            }
            // Ctrl+X: the raw form of a tool call. `x` for the XML-ish markup it
            // shows; the argument for this byte rather than a nicer one is in
            // `App::key`. Not a tty control character, and readline uses it only as
            // a prefix, so nothing downstream is waiting for a second byte.
            0x18 => {
                out.push(Key::CtrlX);
                i += 1;
            }
            0x0c => {
                out.push(Key::CtrlL);
                i += 1;
            }
            // The session list. Ctrl+S is normally XOFF and would freeze a
            // terminal; `cfmakeraw` clears `IXON`, so nothing here is listening for
            // it and the byte reaches this decoder.
            0x13 => {
                out.push(Key::CtrlS);
                i += 1;
            }
            // The todos pane. Ctrl+P is the print byte and nothing in a raw
            // terminal listens for it.
            0x10 => {
                out.push(Key::CtrlP);
                i += 1;
            }
            // The subagent tree. Ctrl+G is BEL; in raw mode nothing rings on it and
            // the byte reaches this decoder like any other.
            0x07 => {
                out.push(Key::CtrlG);
                i += 1;
            }
            // Background the running command. Ctrl+O — B is the readline left-arrow
            // and is muscle memory — so the chord is the next free control byte.
            0x0f => {
                out.push(Key::CtrlO);
                i += 1;
            }
            // The background-jobs pane. Ctrl+Q is XON, dead the same way Ctrl+S's
            // XOFF would be — and fixed the same way: `cfmakeraw` clears IXON, so
            // nothing is listening for flow control and the byte arrives like any
            // other. J would have been the mnemonic; it is line-feed.
            0x11 => {
                out.push(Key::CtrlQ);
                i += 1;
            }
            // **Retire every note. Ctrl+N, and `N` is the whole mnemonic** (R22).
            //
            // Free on this side and free in the strongest sense: this byte had NO arm
            // before, so it reached the `_ => i += 1` fallthrough and was eaten silently —
            // a key that does nothing rather than a key bound to nothing. `ctrl-v` (`0x16`)
            // and `0x1c`-`0x1e` are the only bytes left in this table with no arm, and none
            // of those four has a mnemonic worth having.
            //
            // **Not a tty control character**, so nothing upstream is listening for it: it
            // is not `IXON`/`IXOFF` (`ctrl-s`/`ctrl-q` are, and `cfmakeraw` clears them),
            // and it is not one of the six chords the hint bar already lists. leticl checked
            // the other half of this — the operator's own multiplexer, `tmux list-keys -T
            // root`, 75 bindings and not one bare `C-n` — because a chord can be eaten
            // before a head ever sees it.
            0x0e => {
                out.push(Key::CtrlN);
                i += 1;
            }
            // Tab: the composer's slash-command completion. A plain 0x09 used to
            // fall through the `c >= 0x20` arm and vanish — a byte the head eats
            // silently is a key nobody can learn.
            0x09 => {
                out.push(Key::Tab);
                i += 1;
            }
            b'\r' | b'\n' => {
                out.push(Key::Enter);
                i += 1;
            }
            0x7f | 0x08 => {
                out.push(Key::Backspace);
                i += 1;
            }
            c if c >= 0x20 => {
                let len = utf8_len(c);
                if i + len > b.len() {
                    // The read ended mid-character. This is the byte-losing bug:
                    // hold it, do not decode it.
                    return (out, if force { b.len() } else { i });
                }
                match std::str::from_utf8(&b[i..i + len]) {
                    Ok(t) => out.extend(t.chars().map(Key::Char)),
                    // Not UTF-8 at all. Skipping one byte resynchronises without
                    // stalling the stream on it forever.
                    Err(_) => {
                        i += 1;
                        continue;
                    }
                }
                i += len;
            }
            _ => i += 1,
        }
    }
    (out, b.len())
}

enum Step {
    /// A key — or nothing, for a sequence recognised and deliberately ignored —
    /// and how many bytes it took.
    Emit(Option<Key>, usize),
    /// The sequence has not finished arriving.
    Incomplete,
}

/// Decode one escape sequence at the head of `b`, which starts with `0x1b`.
fn escape(b: &[u8], force: bool) -> Step {
    if b.len() == 1 {
        // A lone ESC is Esc. It is genuinely ambiguous — every arrow key starts
        // this way — and it is resolved in favour of the key a person pressed on
        // purpose, because Esc twice is how the composer interrupts a turn and a
        // held Esc is one that does not arrive.
        return Step::Emit(Some(Key::Esc), 1);
    }
    match b[1] {
        // Two of them. Esc twice is how the composer interrupts a turn, and at a
        // 100 ms read they usually arrive in the same buffer — consuming both as
        // one Alt+Esc would eat the interrupt.
        0x1b => Step::Emit(Some(Key::Esc), 1),
        b'[' => csi(b, force),
        // SS3: the application-cursor-mode arrows, which is what a terminal sends
        // after `smkx`.
        b'O' => {
            if b.len() < 3 {
                return if force {
                    Step::Emit(Some(Key::Esc), 1)
                } else {
                    Step::Incomplete
                };
            }
            let k = match b[2] {
                b'A' => Some(Key::Up),
                b'B' => Some(Key::Down),
                b'C' => Some(Key::Right),
                b'D' => Some(Key::Left),
                b'H' => Some(Key::Home),
                b'F' => Some(Key::End),
                _ => None,
            };
            Step::Emit(k, 3)
        }
        // Alt+Enter: a newline that does not submit. A terminal cannot report
        // Shift+Enter at all without the kitty protocol, so this is the one that
        // has to work.
        b'\r' | b'\n' => Step::Emit(Some(Key::SoftEnter), 2),
        b'b' => Step::Emit(Some(Key::WordLeft), 2),
        b'f' => Step::Emit(Some(Key::WordRight), 2),
        // grok-build's note, worth having: in many terminals Ctrl+Shift+Z arrives
        // byte-identical to Ctrl+Z, so redo needs a second binding.
        b'z' => Step::Emit(Some(Key::Redo), 2),
        0x7f => Step::Emit(Some(Key::KillWordBack), 2),
        // Any other Alt+key is swallowed rather than typed, so Alt+j does not
        // insert a `j`.
        _ => Step::Emit(None, 2),
    }
}

/// Decode one CSI sequence: `ESC [`, parameters, intermediates, a final byte.
fn csi(b: &[u8], force: bool) -> Step {
    let mut i = 2;
    while i < b.len() && (0x30..=0x3f).contains(&b[i]) {
        i += 1;
    }
    let params_end = i;
    while i < b.len() && (0x20..=0x2f).contains(&b[i]) {
        i += 1;
    }
    if i >= b.len() {
        // The final byte has not arrived. Holding is the whole point: decoding
        // `\x1b[` as an Esc and a `[` is how half an arrow key becomes typed
        // punctuation in the middle of a prompt.
        return if force {
            Step::Emit(Some(Key::Esc), 1)
        } else {
            Step::Incomplete
        };
    }
    let fin = b[i];
    let n = i + 1;
    let params = &b[2..params_end];
    // `1;5` — the modifier is the second parameter, and 5 is Ctrl. Ctrl+arrow is
    // the word motion every editor binds it to.
    let ctrl = params.split(|c| *c == b';').nth(1) == Some(&b"5"[..]);
    let k = match fin {
        b'A' => Some(Key::Up),
        b'B' => Some(Key::Down),
        b'C' => Some(if ctrl { Key::WordRight } else { Key::Right }),
        b'D' => Some(if ctrl { Key::WordLeft } else { Key::Left }),
        b'H' => Some(Key::Home),
        b'F' => Some(Key::End),
        b'~' => match params.split(|c| *c == b';').next().unwrap_or(b"") {
            b"1" | b"7" => Some(Key::Home),
            b"3" => Some(Key::Delete),
            b"4" | b"8" => Some(Key::End),
            b"5" => Some(Key::PageUp),
            b"6" => Some(Key::PageDown),
            _ => None,
        },
        b'M' | b'm' => {
            // SGR mouse (`?1006`): `ESC [ < b ; x ; y M` on press, `m` on release.
            // The wheel is button 64 (up) and 65 (down). A **left-button press**
            // (button 0) is a click, and an open picker takes it: the row under
            // the pointer becomes the selected row, and Enter still does the
            // switching — select and confirm stay two acts, because a gesture
            // that commits on press is how a misclick switches somebody's
            // conversation. Every other report — releases, drags, motion, other
            // buttons — is decoded and dropped, because a report the head does
            // not act on must never become typed punctuation, and selecting
            // text stays the terminal's own Shift+drag, which mouse tracking
            // does not take away. Coordinates are 1-based on the wire and
            // 0-based here.
            let mut fields = params
                .strip_prefix(b"<".as_slice())
                .unwrap_or(params)
                .split(|c| *c == b';');
            let btn = fields
                .next()
                .and_then(|f| std::str::from_utf8(f).ok())
                .and_then(|f| f.parse::<u8>().ok())
                .unwrap_or(0);
            let coord = |f: Option<&[u8]>| -> u16 {
                f.and_then(|f| std::str::from_utf8(f).ok())
                    .and_then(|f| f.parse::<u16>().ok())
                    .unwrap_or(1)
                    .saturating_sub(1)
            };
            let (x, y) = (coord(fields.next()), coord(fields.next()));
            match (fin, btn) {
                (b'M', 64) => Some(Key::WheelUp),
                (b'M', 65) => Some(Key::WheelDown),
                (b'M', 0) => Some(Key::Click { x, y }),
                _ => None,
            }
        }
        _ => None,
    };
    Step::Emit(k, n)
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|&i| &hay[i..i + needle.len()] == needle)
}

fn utf8_len(b: u8) -> usize {
    match b {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrows_and_control_keys_decode() {
        assert_eq!(decode(b"\x1b[A"), vec![Key::Up]);
        assert_eq!(decode(b"\x1b[B"), vec![Key::Down]);
        assert_eq!(decode(b"\x03"), vec![Key::CtrlC]);
        assert_eq!(decode(b"\r"), vec![Key::Enter]);
        assert_eq!(decode(b"\x7f"), vec![Key::Backspace]);
        assert_eq!(decode(b"\x1b"), vec![Key::Esc]);
        assert_eq!(decode(b"\x18"), vec![Key::CtrlX]);
        // **R22's chord, and it is a NEW arm rather than an old one.** `0x0e` had no arm in
        // this decoder before `ctrl-n`, so it fell to the `_ => i += 1` fallthrough and was
        // eaten — a key that did nothing rather than a key bound to nothing. That is also
        // why it was free: nothing in this head, and nothing downstream of it, was waiting
        // for the byte. Asserted here rather than only through `App::key`, because what this
        // guards is a byte that vanishes before any handler can see it.
        assert_eq!(decode(b"\x0e"), vec![Key::CtrlN]);
    }

    #[test]
    fn an_unchanged_frame_writes_nothing_at_all() {
        // The flicker, as an assertion. The loop wakes ten times a second whether
        // or not anything arrived; over one measured 28-second session, 221 of 269
        // frames were byte-identical to the one before and every one of them
        // erased and repainted the whole screen.
        let frame: Vec<String> = ["one", "two", "three"].iter().map(|s| s.to_string()).collect();
        let mut shown = Vec::new();
        let cur = Some((2, 4));
        let (first, next) = paint(&shown, &frame, cur, None);
        assert!(!first.is_empty(), "first draw");
        shown = next;
        for _ in 0..100 {
            let (bytes, next) = paint(&shown, &frame, cur, cur);
            assert_eq!(bytes, "");
            // **And the memory is still adopted**, because a frame that needs no
            // bytes is one whose memory is already true. This is the half that makes
            // the loop above meaningful: a `paint` that returned the same memory every
            // time would answer `""` by never having learned anything.
            shown = next;
        }
        assert_eq!(shown, frame);
    }

    /// The encoder's one arithmetic claim, checked against the thing it counts.
    ///
    /// [`WriteStats::rows`] counts row rewrites by counting `ESC[K`, on the
    /// grounds that [`paint_full`] emits exactly one per row it repaints and
    /// nothing else in this file emits it at all. That is a fact about this file
    /// and it can rot, so it is asserted rather than asserted-in-a-comment: three
    /// rows changed of five is three, not five and not one.
    #[test]
    fn the_write_counter_counts_rows_and_not_frames() {
        let a: Vec<String> = ["one", "two", "three", "four", "five"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let mut shown = Vec::new();
        let (first, next) = paint_full(&shown, &a, None, None, true);
        assert_eq!(first.matches("\x1b[K").count(), 5, "every row, once");
        shown = next;

        let mut b = a.clone();
        b[1] = "TWO".into();
        b[3] = "FOUR".into();
        let (second, _) = paint_full(&shown, &b, None, None, false);
        assert_eq!(
            second.matches("\x1b[K").count(),
            2,
            "only the rows that changed: {second:?}"
        );
        assert_eq!(second.matches("\x1b[2J").count(), 0, "and no erase");
    }

    #[test]
    fn a_frame_that_changed_height_does_not_erase_the_screen() {
        // The composer grows a row every time a prompt wraps. Erasing the screen
        // for that is a flash per wrap.
        let a: Vec<String> = ["one", "two", "three"].iter().map(|s| s.to_string()).collect();
        let mut shown = Vec::new();
        let (_, next) = paint_full(&shown, &a, None, None, true);
        shown = next;
        let mut b = a.clone();
        b.push("four".into());
        let (bytes, next) = paint_full(&shown, &b, None, None, false);
        shown = next;
        assert!(!bytes.contains("\x1b[2J"), "{bytes:?}");
        assert!(bytes.contains("four"), "{bytes:?}");
        assert!(!bytes.contains("one"), "unchanged rows stay put: {bytes:?}");
        // And shrinking erases exactly the row that went, not the screen.
        let (bytes, next) = paint_full(&shown, &a, None, None, false);
        shown = next;
        assert!(!bytes.contains("\x1b[2J"), "{bytes:?}");
        assert!(bytes.contains("\x1b[4;1H"), "row four is erased: {bytes:?}");
        // …and the glass is still an honest model of itself.
        assert_eq!(paint_full(&shown, &a, None, None, false).0, "");
    }

    /// **A paint that dies part way writes no record of the frame it did not finish.**
    ///
    /// This is the other head's finding, and it was live here in the same shape: the
    /// memory of the glass was updated *while the bytes were being built*, and the write
    /// afterwards threw its error away with `let _ =`. A frame that died part-way
    /// therefore left this head believing rows were on the glass that it had never
    /// written, and every later frame skipped them — `shown[i] == *l` — so the hole was
    /// permanent until a `full` repaint, which is Ctrl-L or **a resize**.
    ///
    /// That last word is the operator's own symptom, which is why this is worth the test
    /// rather than the argument: *"when i expand tools with Ct scroll stops working, even
    /// after collapsing back. i have to switch byobu windows back and forth"*. Switching
    /// windows resizes, a resize forces `full`, and the frame repaired itself — so the
    /// unexplained half of that report was this, and the byobu switch was the cure.
    ///
    /// A writer that fails on its **second** call is the interesting one: the first write
    /// of the frame really does reach the glass, so "commit nothing" and "commit
    /// everything" are both wrong and the assertion has to be about what the head knows
    /// rather than about what it sent.
    #[test]
    fn a_paint_that_dies_part_way_leaves_no_record() {
        /// Writes `ok` times, then fails for ever.
        struct Dies {
            left: usize,
            wrote: Vec<u8>,
        }
        impl std::io::Write for Dies {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                if self.left == 0 {
                    return Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe));
                }
                self.left -= 1;
                self.wrote.extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let t = Terminal::headless();
        let frame: Vec<String> = ["one", "two", "three"].iter().map(|s| s.to_string()).collect();

        // The frame draws onto the glass, and the glass is remembered.
        let mut ok = Dies { left: usize::MAX, wrote: Vec::new() };
        assert!(!t.paint_to(&mut ok, &frame, None), "a good write invalidates nothing");
        assert_eq!(*t.shown.borrow(), frame, "the frame is on the glass");
        assert!(!ok.wrote.is_empty(), "the premise: bytes really went out");

        // **Now a frame whose write dies after one call.** `paint_to` writes the
        // synchronised-output opener first, so one call in means the glass got that and
        // nothing of the row itself.
        let mut dead = Dies { left: 1, wrote: Vec::new() };
        let mut changed = frame.clone();
        changed[1] = "TWO".into();
        assert!(
            t.paint_to(&mut dead, &changed, None),
            "a failed write must report that the glass is unknown"
        );
        // **Nothing is claimed.** The memory is empty — not the old frame and not the
        // new one — because the head cannot know which rows landed.
        assert!(
            t.shown.borrow().is_empty(),
            "a frame that died part-way was recorded: {:?}",
            t.shown.borrow()
        );
        // And the next frame is a **full** repaint, so the hole cannot outlive the
        // failure. This is the half the old code could not do: it had left a memory of
        // rows it never wrote, and `full` was the only way back out.
        let mut ok = Dies { left: usize::MAX, wrote: Vec::new() };
        t.paint_to(&mut ok, &changed, None);
        let sent = String::from_utf8_lossy(&ok.wrote).to_string();
        assert!(
            sent.contains("\x1b[2J"),
            "the recovery must be a full repaint, not a diff against a guess: {sent:?}"
        );
        for row in ["one", "TWO", "three"] {
            assert!(sent.contains(row), "the repaint must carry every row: {sent:?}");
        }

        // **And the counters do not claim bytes that were never written.** A frame that
        // failed is a frame; its bytes are not on the glass, so they are not counted as
        // having been sent.
        let t = Terminal::headless();
        let mut dead = Dies { left: 1, wrote: Vec::new() };
        t.paint_to(&mut dead, &frame, None);
        assert_eq!(
            t.write_stats().bytes,
            0,
            "a failed frame counted its bytes as written"
        );
        assert_eq!(t.write_stats().frames, 1, "but the frame was attempted");
    }

    #[test]
    fn only_the_rows_that_changed_are_written() {
        let a: Vec<String> = ["one", "two", "three"].iter().map(|s| s.to_string()).collect();
        let mut b = a.clone();
        b[1] = "TWO".into();
        let mut shown = Vec::new();
        let (_, next) = paint(&shown, &a, None, None);
        shown = next;
        let (bytes, _) = paint(&shown, &b, None, None);
        assert!(bytes.contains("TWO"));
        assert!(!bytes.contains("one") && !bytes.contains("three"), "{bytes:?}");
        // Addressed absolutely: row 2, column 1.
        assert!(bytes.contains("\x1b[2;1H"), "{bytes:?}");
    }

    #[test]
    fn a_read_that_ends_mid_utf8_loses_nothing() {
        // The bug, as an assertion. `term::keys` used to read 64 bytes and hand
        // them to `from_utf8`; a read that split a character failed the whole
        // decode and the arm returned nothing, silently. Pasting a stack trace
        // with an arrow or a box-drawing character in it lost bytes.
        let whole = "héllo → wörld ⣿".as_bytes();
        for cut in 1..whole.len() {
            let (a, b) = whole.split_at(cut);
            let (mut keys, used) = decode_prefix(a, false);
            // Whatever was not decodable is carried, never dropped.
            let mut rest = a[used..].to_vec();
            rest.extend_from_slice(b);
            keys.extend(decode_prefix(&rest, true).0);
            let text: String = keys
                .iter()
                .map(|k| match k {
                    Key::Char(c) => *c,
                    other => panic!("{other:?}"),
                })
                .collect();
            assert_eq!(text, "héllo → wörld ⣿", "split at {cut}");
        }
    }

    #[test]
    fn an_escape_sequence_split_across_two_reads_is_one_key_not_typed_punctuation() {
        // A lone ESC is the one deliberate exception: it is genuinely ambiguous,
        // and it is resolved in favour of the key a person pressed on purpose,
        // because Esc twice is the composer's interrupt and an Esc that waits for
        // a disambiguating read is an Esc that arrives a frame late.
        assert_eq!(decode_prefix(b"\x1b", false), (vec![Key::Esc], 1));
        assert_eq!(decode(b"\x1b\x1b"), vec![Key::Esc, Key::Esc]);

        let whole = b"\x1b[1;5C";
        for cut in 2..whole.len() {
            let (keys, used) = decode_prefix(&whole[..cut], false);
            assert!(keys.is_empty(), "cut {cut}: {keys:?}");
            assert_eq!(used, 0, "the partial sequence must be carried, not eaten");
        }
        assert_eq!(decode(whole), vec![Key::WordRight]);
    }

    #[test]
    fn the_wheel_arrives_as_sgr_mouse_and_every_other_report_is_dropped() {
        assert_eq!(decode(b"\x1b[<64;10;5M"), vec![Key::WheelUp]);
        assert_eq!(decode(b"\x1b[<65;10;5M"), vec![Key::WheelDown]);
        // A left-button press is a click, on 0-based coordinates; drags,
        // motion and releases are decoded and dropped: a report the head does
        // not act on must never become typed punctuation.
        assert_eq!(decode(b"\x1b[<0;3;4M"), vec![Key::Click { x: 2, y: 3 }]);
        assert_eq!(decode(b"\x1b[<32;3;4M"), Vec::<Key>::new());
        assert_eq!(decode(b"\x1b[<0;3;4m"), Vec::<Key>::new());
        // A report cut mid-sequence is carried, not eaten.
        let whole = b"\x1b[<65;1;1M";
        for cut in 2..whole.len() {
            let (keys, used) = decode_prefix(&whole[..cut], false);
            assert!(keys.is_empty(), "cut {cut}: {keys:?}");
            assert_eq!(used, 0, "the partial report must be carried, not eaten");
        }
        assert_eq!(decode(whole), vec![Key::WheelDown]);
    }

    #[test]
    fn a_bracketed_paste_is_one_key_and_its_newlines_do_not_submit() {
        let mut b = b"\x1b[200~".to_vec();
        b.extend_from_slice(b"line one\nline two\nline three");
        b.extend_from_slice(b"\x1b[201~");
        assert_eq!(
            decode(&b),
            vec![Key::Paste("line one\nline two\nline three".into())]
        );
        // …and while the terminator has not arrived, nothing is consumed: the
        // read loop is still waiting for the rest of the paste.
        let open = &b[..b.len() - 3];
        assert!(paste_open(open));
        assert_eq!(decode_prefix(open, false), (Vec::new(), 0));
    }

    #[test]
    fn the_keys_a_composer_needs_all_decode() {
        for (bytes, want) in [
            (&b"\x1b[C"[..], Key::Right),
            (b"\x1b[D", Key::Left),
            (b"\x1bOC", Key::Right),
            (b"\x1b[H", Key::Home),
            (b"\x1b[3~", Key::Delete),
            (b"\x1b[5~", Key::PageUp),
            (b"\x1b[6~", Key::PageDown),
            (b"\x1b[1;5D", Key::WordLeft),
            (b"\x1b\r", Key::SoftEnter),
            (b"\x1bb", Key::WordLeft),
            (b"\x01", Key::Home),
            (b"\x09", Key::Tab),
            (b"\x05", Key::End),
            (b"\x0b", Key::KillToEnd),
            (b"\x15", Key::KillToStart),
            (b"\x17", Key::KillWordBack),
            (b"\x19", Key::Yank),
            (b"\x1a", Key::Undo),
            (b"\x04", Key::Eof),
        ] {
            assert_eq!(decode(bytes), vec![want.clone()], "{bytes:?}");
        }
    }

    #[test]
    fn a_multibyte_paste_survives() {
        assert_eq!(
            decode("héllo".as_bytes()),
            vec![
                Key::Char('h'),
                Key::Char('é'),
                Key::Char('l'),
                Key::Char('l'),
                Key::Char('o')
            ]
        );
    }
}
