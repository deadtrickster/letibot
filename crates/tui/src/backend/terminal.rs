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

use super::decode::{decode_prefix, legacy_bytes, paste_open};
use crate::app::Key;

pub struct Terminal {
    original: libc::termios,
    fd: i32,
    entered: bool,
    /// The window title last written (see [`Terminal::set_title`]), so a frame that does
    /// not change it writes nothing.
    title: std::cell::RefCell<String>,
    /// The frame currently on the glass. [`Terminal::draw`] writes the difference
    /// against it and nothing else; see the note on flicker.
    shown: std::cell::RefCell<Vec<String>>,
    /// **The glass state being built, kept between frames so its rows keep their buffers.**
    ///
    /// `paint_full` used to start from `shown.to_vec()` and hand the copy back, so a
    /// `Vec<String>` of one `String` per screen row was allocated and freed on EVERY frame — tens
    /// of times a second, on a screen that usually has not changed at all. Two buffers and a swap
    /// make the steady state allocation-free: this one is filled (each row reusing its own
    /// buffer) and then exchanged with `shown`.
    ///
    /// **It has to be a second buffer rather than in-place editing of `shown`, and that is the
    /// one thing here that is not an optimisation.** A write can fail partway, and the rule this
    /// type keeps is that `shown` records what is *actually on the glass*; editing it while
    /// encoding would leave a memory of a frame that may never have been written. See
    /// `draw_with_cursor`'s doc and the `clear()` on its error path.
    next: std::cell::RefCell<Vec<String>>,
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
    /// **The bytes the last [`Terminal::keys`] call consumed, verbatim.**
    ///
    /// See [`Terminal::raw_keys`] for who reads this and why a decoded `Key` cannot be
    /// re-encoded into it.
    last_raw: std::cell::RefCell<Vec<u8>>,
    /// The terminal size the last frame was painted at, and whether the next
    /// frame must erase everything before it paints.
    last_size: std::cell::Cell<(usize, usize)>,
    full: std::cell::Cell<bool>,
    /// **What this terminal speaks beyond cells** — see [`crate::features`]. Decided once at
    /// [`Terminal::enter`], and every mode it turns on there is turned off by [`restore`].
    features: crate::backend::features::Features,
    /// The progress state last written (OSC 9;4), so a tick that does not change it writes
    /// nothing.
    progress: std::cell::Cell<Progress>,
}

/// **The tab's progress bar** (OSC 9;4), as the head means it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Progress {
    /// Nothing to show: the bar is removed.
    #[default]
    Idle,
    /// The model is working — generating, or waiting on a call it made. No percentage exists,
    /// so the bar is the terminal's indeterminate one.
    Busy,
    /// Something is waiting on the PERSON: a permission, a key, a password. Drawn in the
    /// terminal's paused (warning) colour, so a tab that wants you looks different from a tab
    /// that is working.
    Waiting,
    /// The last turn ended in an error.
    Failed,
}

impl Progress {
    /// The OSC 9;4 state and value for this.
    fn osc(self) -> &'static [u8] {
        match self {
            Progress::Idle => b"\x1b]9;4;0\x07",
            Progress::Busy => b"\x1b]9;4;3\x07",
            Progress::Waiting => b"\x1b]9;4;4;100\x07",
            Progress::Failed => b"\x1b]9;4;2;100\x07",
        }
    }
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
        //
        // And the window title is SAVED (`CSI 22;0 t`, xterm's title stack), because the head
        // sets its own — the session's name, see `set_title` — and gives the terminal back
        // the title it had. A terminal without the stack ignores the sequence.
        let _ = out
            .write_all(b"\x1b[22;0t\x1b[?1049h\x1b[?25l\x1b[?2004h\x1b[?1002h\x1b[?1006h\x1b[2 q");
        // **The terminal's extras, each only where it is spoken** (see `crate::features`).
        //
        // `CSI > 1 u` pushes the kitty keyboard's "disambiguate" flag: Esc stops being the
        // first byte of every arrow key, and Shift+Enter becomes a key at all. `?1004h` asks for
        // focus reports, which is what lets a notification go only to a person who is not
        // looking. `OSC 11 ; ?` asks the background colour once; the answer arrives as input
        // and the decoder turns it into a key.
        let features = crate::backend::features::Features::detect();
        if features.keys {
            let _ = out.write_all(b"\x1b[>1u");
        }
        if features.notify {
            let _ = out.write_all(b"\x1b[?1004h");
        }
        if features.background {
            let _ = out.write_all(b"\x1b]11;?\x1b\\");
        }
        let _ = out.flush();

        // Restore before anything is printed, or the panic message is a staircase.
        let saved = original;
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore(fd, &saved, features);
            prev(info);
        }));

        Ok(Terminal {
            original,
            fd,
            entered: true,
            title: std::cell::RefCell::new(String::new()),
            shown: std::cell::RefCell::new(Vec::new()),
            next: std::cell::RefCell::new(Vec::new()),
            cursor: std::cell::Cell::new(None),
            frames: std::cell::Cell::new(0),
            silent: std::cell::Cell::new(0),
            stats: std::cell::Cell::new(WriteStats::default()),
            prev_payload: std::cell::RefCell::new(String::new()),
            stats_to: std::env::var("LETIBOT_TUI_WRITE_STATS")
                .ok()
                .filter(|s| !s.is_empty()),
            pending: std::cell::RefCell::new(Vec::new()),
            last_raw: std::cell::RefCell::new(Vec::new()),
            last_size: std::cell::Cell::new((0, 0)),
            full: std::cell::Cell::new(true),
            features,
            progress: std::cell::Cell::new(Progress::Idle),
        })
    }

    /// What this terminal was found to speak. See [`crate::features`].
    pub fn features(&self) -> crate::backend::features::Features {
        self.features
    }

    /// **The tab's progress bar**, written only when it changed and only where it is spoken.
    /// A terminal that does not know OSC 9;4 may read OSC 9 as a NOTIFICATION (iTerm2 does), so
    /// this writes nothing at all unless [`crate::backend::features::Features::progress`] is on.
    pub fn set_progress(&self, p: Progress) {
        if !self.features.progress || self.progress.get() == p {
            return;
        }
        let mut out = std::io::stdout();
        let _ = out.write_all(p.osc());
        let _ = out.flush();
        self.progress.set(p);
    }

    /// **A desktop notification** (OSC 9), where spoken. The text is stripped of control
    /// characters — it carries a session's words, and an escape in it would be one the
    /// terminal executes — and of `;`, which some terminals read as OSC 9's own separator.
    pub fn notify(&self, text: &str) {
        if !self.features.notify {
            return;
        }
        let clean: String = window_title_text(text).replace(';', ",");
        if clean.is_empty() {
            return;
        }
        let mut out = std::io::stdout();
        let _ = write!(out, "\x1b]9;{clean}\x07");
        let _ = out.flush();
    }

    /// **Put text on the system clipboard** (OSC 52), where spoken. Base64 of the bytes as
    /// they are: the clipboard is the operator's, and what they asked to copy is what lands.
    pub fn copy(&self, text: &str) -> bool {
        if !self.features.clipboard {
            return false;
        }
        let mut out = std::io::stdout();
        let _ = write!(out, "\x1b]52;c;{}\x07", base64(text.as_bytes()));
        let _ = out.flush();
        true
    }

    /// **Write bytes that are not part of the frame** — an inline image's upload — outside the
    /// diffing encoder, which only knows rows of text.
    pub fn write_raw(&self, bytes: &[u8]) {
        let mut out = std::io::stdout();
        let _ = out.write_all(bytes);
        let _ = out.flush();
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
        // **Cleared first, and that is not tidiness.** A stale carry would be re-forwarded to
        // the pane on the next read — the same keystroke twice — and this is the only place the
        // carry is allowed to be stale, so this is the only place it is cleared.
        self.last_raw.borrow_mut().clear();
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
        // **What the decoding consumed, before it is dropped.** See [`Terminal::raw_keys`].
        self.last_raw
            .borrow_mut()
            .extend_from_slice(&pending[..used]);
        pending.drain(..used);
        keys
    }

    /// **The bytes the last [`Terminal::keys`] call consumed, verbatim.**
    ///
    /// # Who reads this, and why `Key` cannot be re-encoded into it
    ///
    /// The pane. A program that owns the screen reads the bytes the operator's terminal
    /// actually sent, and this head's [`Key`] is a **lossy reading** of them: `ESC [ A` and
    /// `ESC O A` are both `Key::Up` and are *different byte strings* to a program that has
    /// asked for the application-cursor spelling, `Key::Paste` has had its bracketed-paste
    /// markers stripped, and a dozen `Key`s here are the composer's own vocabulary
    /// (`KillToEnd`, `Yank`, `Undo`, `WordLeft`) whose bytes a program would read as something
    /// else entirely. Re-encoding would be a keymap in front of a terminal, which is exactly
    /// what `letibot_tools::exec::term`'s own note says must not happen.
    ///
    /// # And it is why the way out is `ctrl-\`
    ///
    /// `0x1c` is one of the three bytes this file's decoder has **no arm** for — it reaches the
    /// `_ => i += 1` fallthrough and vanishes — so it can never arrive as a [`Key`] and can only
    /// be found here, on the raw stream, before anything is forwarded. See `App::pane_keys`.
    pub fn raw_keys(&self) -> Vec<u8> {
        // **Under the kitty keyboard the raw stream is in a spelling the pane's program never
        // asked for** — Ctrl-C arrives as `CSI 99;5u`, and the pane's own way out, `ctrl-\`,
        // as `CSI 92;5u`. So it is put back into the legacy bytes first: the program reads
        // what a terminal without the protocol would have sent it.
        if self.features.keys {
            legacy_bytes(&self.last_raw.borrow())
        } else {
            self.last_raw.borrow().clone()
        }
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
    /// **The window title** — the session's name, so a tab says which conversation it is
    /// rather than the name of the program (`leticode`), which every tab shares.
    ///
    /// Written as OSC 2 and only when it changed. Every control character is dropped
    /// first: the text is a session title, which anyone with the socket can set, and an
    /// escape inside it would be one the terminal executes (the tree's own hostile-title
    /// tests carry `ESC ] 0 ; pwned`). Capped, because a title bar has no use for a
    /// paragraph.
    pub fn set_title(&self, title: &str) {
        let clean: String = window_title_text(title);
        if *self.title.borrow() == clean {
            return;
        }
        let mut out = std::io::stdout();
        let _ = write!(out, "\x1b]2;{clean}\x07");
        let _ = out.flush();
        *self.title.borrow_mut() = clean;
    }

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
        // The two buffers are EXCHANGED, not copied: the scratch is filled from the frame and
        // then becomes the memory of the glass, so what was `shown` is free to be the scratch
        // next time and both keep the rows they have already allocated.
        let s = {
            let mut next = self.next.borrow_mut();
            paint_full(
                &self.shown.borrow(),
                lines,
                &mut next,
                cursor,
                self.cursor.get(),
                self.full.replace(false),
            )
        };
        if s.is_empty() {
            self.silent.set(self.silent.get() + 1);
            st.silent += 1;
            self.stats.set(st);
            // **Adopted, and nothing was written.** The frame needed no bytes, so the
            // memory it describes is already true — and taking it keeps `shown` the same
            // length as the frame, which is what the next frame diffs against.
            self.adopt_next();
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
        self.adopt_next();
        self.cursor.set(cursor);
        false
    }

    /// **The built state becomes the memory of the glass**, in one exchange.
    ///
    /// A swap rather than an assignment of a fresh `Vec`: the buffer `shown` gives up is the
    /// scratch the next frame rebuilds into, so its rows keep their allocations. This is what
    /// makes the steady state — a screen whose text has not changed — cost no allocation at all,
    /// where the old `shown.to_vec()` paid one `String` per row per frame for the privilege of
    /// comparing them and finding them equal.
    fn adopt_next(&self) {
        self.shown.swap(&self.next);
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
            restore(self.fd, &self.original, self.features);
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
    let mut next = Vec::new();
    let s = paint_full(
        shown,
        lines,
        &mut next,
        cursor,
        prev_cursor,
        shown.is_empty(),
    );
    (s, next)
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
    next: &mut Vec<String>,
    cursor: Option<(usize, usize)>,
    prev_cursor: Option<(usize, usize)>,
    full: bool,
) -> String {
    let mut s = String::new();
    if full {
        s.push_str("\x1b[2J");
    }
    // Rows the frame no longer has: erase them, rather than the whole screen.
    //
    // Over `shown` and not over `next`: what is on the glass is what must be erased, and `next`
    // is a scratch that happens to hold some other frame's rows.
    for i in lines.len()..shown.len() {
        s.push_str(&format!("\x1b[{};1H\x1b[0m\x1b[K", i + 1));
    }
    // **`next` becomes this frame, and a row it already holds costs nothing.**
    //
    // The loop this replaces started from a copy of `shown` and then assigned `l.clone()` over
    // every differing row, so each of the screen's rows was allocated afresh on every frame. Here
    // the buffer is reused: two frames of identical text leave every row untouched, and a row
    // whose text changed gets `clear()` + `push_str` — one reallocation at most, into the buffer
    // it already had.
    //
    // The comparison is against what the buffer holds rather than against `shown`, because that
    // is what decides whether a copy is needed at all; `shown` decides what goes on the wire.
    next.resize(lines.len(), String::new());
    for (i, l) in lines.iter().enumerate() {
        let on_the_glass = !full && shown.get(i) == Some(l);
        if !on_the_glass {
            s.push_str(&format!("\x1b[{};1H\x1b[0m\x1b[K", i + 1));
            s.push_str(l);
        }
        if next[i] != *l {
            next[i].clear();
            next[i].push_str(l);
        }
    }
    if s.is_empty() && cursor == prev_cursor {
        return String::new();
    }
    match cursor {
        Some((r, c)) => s.push_str(&format!("\x1b[{};{}H\x1b[?25h", r + 1, c + 1)),
        None => s.push_str("\x1b[?25l"),
    }
    s
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
            title: std::cell::RefCell::new(String::new()),
            shown: std::cell::RefCell::new(Vec::new()),
            next: std::cell::RefCell::new(Vec::new()),
            cursor: std::cell::Cell::new(None),
            frames: std::cell::Cell::new(0),
            silent: std::cell::Cell::new(0),
            stats: std::cell::Cell::new(WriteStats::default()),
            prev_payload: std::cell::RefCell::new(String::new()),
            stats_to: None,
            pending: std::cell::RefCell::new(Vec::new()),
            last_raw: std::cell::RefCell::new(Vec::new()),
            last_size: std::cell::Cell::new((80, 24)),
            full: std::cell::Cell::new(true),
            features: crate::backend::features::Features::default(),
            progress: std::cell::Cell::new(Progress::Idle),
        }
    }
}

/// A title's text with every control character removed and its length capped — what
/// [`Terminal::set_title`] writes. Separate so it can be tested without a terminal.
pub fn window_title_text(title: &str) -> String {
    title
        .chars()
        .filter(|c| !c.is_control())
        .take(120)
        .collect::<String>()
        .trim()
        .to_string()
}

fn restore(fd: i32, original: &libc::termios, features: crate::backend::features::Features) {
    unsafe { libc::tcsetattr(fd, libc::TCSANOW, original) };
    let mut out = std::io::stdout();
    // The extras first, each only where `enter` turned it on: the kitty keyboard popped,
    // focus reports off, the progress bar removed.
    if features.keys {
        let _ = out.write_all(b"\x1b[<u");
    }
    if features.notify {
        let _ = out.write_all(b"\x1b[?1004l");
    }
    if features.progress {
        let _ = out.write_all(Progress::Idle.osc());
    }
    // Every mode `enter` turned on, off again, in the reverse order: end any open
    // synchronised update, mouse tracking off, bracketed paste off, the cursor
    // shape back to whatever the operator's terminal had, then show it and leave
    // the alternate screen.
    // Last, the window title `enter` saved (`CSI 23;0 t`): the shell's own, back.
    let _ = out.write_all(
        b"\x1b[?2026l\x1b[?1006l\x1b[?1002l\x1b[?2004l\x1b[0 q\x1b[?25h\x1b[?1049l\x1b[23;0t",
    );
    let _ = out.flush();
}

/// Standard base64 — the tree's one spelling of it (`letibot_transcript::media`), for OSC 52.
pub fn base64(bytes: &[u8]) -> String {
    letibot_transcript::media::encode_base64(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn an_unchanged_frame_writes_nothing_at_all() {
        // The flicker, as an assertion. The loop wakes ten times a second whether
        // or not anything arrived; over one measured 28-second session, 221 of 269
        // frames were byte-identical to the one before and every one of them
        // erased and repainted the whole screen.
        let frame: Vec<String> = ["one", "two", "three"]
            .iter()
            .map(|s| s.to_string())
            .collect();
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
        let first;
        let next;
        {
            let mut scratch = Vec::new();
            first = paint_full(&shown, &a, &mut scratch, None, None, true);
            next = scratch;
        }
        assert_eq!(first.matches("\x1b[K").count(), 5, "every row, once");
        shown = next;

        let mut b = a.clone();
        b[1] = "TWO".into();
        b[3] = "FOUR".into();
        let mut scratch = Vec::new();
        let second = paint_full(&shown, &b, &mut scratch, None, None, false);
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
        let a: Vec<String> = ["one", "two", "three"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let mut shown = Vec::new();
        let mut next = Vec::new();
        paint_full(&shown, &a, &mut next, None, None, true);
        shown = next;
        let mut b = a.clone();
        b.push("four".into());
        let mut next = Vec::new();
        let bytes = paint_full(&shown, &b, &mut next, None, None, false);
        shown = next;
        assert!(!bytes.contains("\x1b[2J"), "{bytes:?}");
        assert!(bytes.contains("four"), "{bytes:?}");
        assert!(!bytes.contains("one"), "unchanged rows stay put: {bytes:?}");
        // And shrinking erases exactly the row that went, not the screen.
        let mut next = Vec::new();
        let bytes = paint_full(&shown, &a, &mut next, None, None, false);
        shown = next;
        assert!(!bytes.contains("\x1b[2J"), "{bytes:?}");
        assert!(bytes.contains("\x1b[4;1H"), "row four is erased: {bytes:?}");
        // …and the glass is still an honest model of itself.
        let mut scratch = Vec::new();
        assert_eq!(paint_full(&shown, &a, &mut scratch, None, None, false), "");
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
        let frame: Vec<String> = ["one", "two", "three"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        // The frame draws onto the glass, and the glass is remembered.
        let mut ok = Dies {
            left: usize::MAX,
            wrote: Vec::new(),
        };
        assert!(
            !t.paint_to(&mut ok, &frame, None),
            "a good write invalidates nothing"
        );
        assert_eq!(*t.shown.borrow(), frame, "the frame is on the glass");
        assert!(!ok.wrote.is_empty(), "the premise: bytes really went out");

        // **Now a frame whose write dies after one call.** `paint_to` writes the
        // synchronised-output opener first, so one call in means the glass got that and
        // nothing of the row itself.
        let mut dead = Dies {
            left: 1,
            wrote: Vec::new(),
        };
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
        let mut ok = Dies {
            left: usize::MAX,
            wrote: Vec::new(),
        };
        t.paint_to(&mut ok, &changed, None);
        let sent = String::from_utf8_lossy(&ok.wrote).to_string();
        assert!(
            sent.contains("\x1b[2J"),
            "the recovery must be a full repaint, not a diff against a guess: {sent:?}"
        );
        for row in ["one", "TWO", "three"] {
            assert!(
                sent.contains(row),
                "the repaint must carry every row: {sent:?}"
            );
        }

        // **And the counters do not claim bytes that were never written.** A frame that
        // failed is a frame; its bytes are not on the glass, so they are not counted as
        // having been sent.
        let t = Terminal::headless();
        let mut dead = Dies {
            left: 1,
            wrote: Vec::new(),
        };
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
        let a: Vec<String> = ["one", "two", "three"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let mut b = a.clone();
        b[1] = "TWO".into();
        let mut shown = Vec::new();
        let (_, next) = paint(&shown, &a, None, None);
        shown = next;
        let (bytes, _) = paint(&shown, &b, None, None);
        assert!(bytes.contains("TWO"));
        assert!(
            !bytes.contains("one") && !bytes.contains("three"),
            "{bytes:?}"
        );
        // Addressed absolutely: row 2, column 1.
        assert!(bytes.contains("\x1b[2;1H"), "{bytes:?}");
    }

    #[test]
    #[test]
    fn base64_is_the_standard_padded_alphabet() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64("ключ".as_bytes()), "0LrQu9GO0Yc=");
    }
}
