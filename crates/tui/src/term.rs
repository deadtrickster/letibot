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
    features: crate::features::Features,
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
        let features = crate::features::Features::detect();
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
    pub fn features(&self) -> crate::features::Features {
        self.features
    }

    /// **The tab's progress bar**, written only when it changed and only where it is spoken.
    /// A terminal that does not know OSC 9;4 may read OSC 9 as a NOTIFICATION (iTerm2 does), so
    /// this writes nothing at all unless [`crate::features::Features::progress`] is on.
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
            features: crate::features::Features::default(),
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

fn restore(fd: i32, original: &libc::termios, features: crate::features::Features) {
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
            // **Hold the view** (R56). Ctrl+P is the print byte and nothing in a raw
            // terminal listens for it, and *pause* is what the key is for: while it is
            // held the head writes nothing, so a mouse selection survives a streaming
            // turn. The todos pane gave this byte up when it moved to `ctrl-t` — see the
            // `0x16` arm below for the whole rework.
            0x10 => {
                out.push(Key::CtrlP);
                i += 1;
            }
            // **The payload window**: open or close the newest long tool result (it was
            // `ctrl-t` until R56 moved it here). `0x16` had no arm before, so it reached
            // the `_ => i += 1` fallthrough and was eaten silently — free in the strongest
            // sense. It carries no tty meaning a raw terminal is waiting for (`VEOL`/`VLNEXT`
            // are the literal-next byte `0x16` only under `IEXTEN`, which `cfmakeraw`
            // clears), and `v` for *view* is the mnemonic the window was missing.
            0x16 => {
                out.push(Key::CtrlV);
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
            // a key that does nothing rather than a key bound to nothing. `0x1c`-`0x1e` are
            // the only bytes left in this table with no arm, and none of the three has a
            // mnemonic worth having (`0x16`, the fourth, became `ctrl-v` in R56).
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
        // **A string the terminal sent back**: OSC (`ESC ]`) for the background colour asked
        // at `enter`, APC (`ESC _`) for a kitty graphics reply. Consumed whole up to its
        // terminator — BEL or ST — and never typed: a reply is not a keystroke.
        b']' | b'_' => match string_end(&b[2..]) {
            Some((body, used)) => Step::Emit(
                if b[1] == b']' {
                    background_reply(&b[2..2 + body])
                } else {
                    None
                },
                2 + used,
            ),
            None if force => Step::Emit(Some(Key::Esc), 1),
            None => Step::Incomplete,
        },
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
        // **The kitty keyboard** (`CSI code ; mods u`), put back into the legacy bytes it
        // stands for and decoded as those — so every binding this file already has means the
        // same key under the protocol, and the only new facts are the two the protocol exists
        // for: Shift+Enter is a key, and Esc is never half of something else.
        b'u' => kitty_key(params),
        // Focus reports (`?1004`): the window gained or lost focus.
        b'I' if params.is_empty() => Some(Key::FocusIn),
        b'O' if params.is_empty() => Some(Key::FocusOut),
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

/// `CSI code ; mods u` → the key it means. See the arm in [`csi`].
fn kitty_key(params: &[u8]) -> Option<Key> {
    let (code, mods) = kitty_fields(params)?;
    let shift = mods & 1 != 0;
    if code == 13 && shift {
        return Some(Key::SoftEnter);
    }
    if code == 27 && mods == 0 {
        return Some(Key::Esc);
    }
    let legacy = kitty_legacy(code, mods)?;
    let (keys, _) = decode_prefix(&legacy, true);
    keys.into_iter().next()
}

/// The code point and the modifier bits (shift 1, alt 2, ctrl 4) of a `CSI … u`. The wire
/// carries `1 + bits`, and a missing modifier field is none.
fn kitty_fields(params: &[u8]) -> Option<(u32, u8)> {
    let text = std::str::from_utf8(params).ok()?;
    let mut fields = text.split(';');
    // `code:shifted:base` — only the first is this key.
    let code: u32 = fields.next()?.split(':').next()?.parse().ok()?;
    let mods: u8 = match fields.next() {
        Some(m) => m.split(':').next()?.parse::<u8>().ok()?.saturating_sub(1),
        None => 0,
    };
    Some((code, mods))
}

/// What a terminal without the protocol sends for this key, or `None` for a key it has no
/// spelling for (the keypad's private-use codes).
fn kitty_legacy(code: u32, mods: u8) -> Option<Vec<u8>> {
    let (alt, ctrl) = (mods & 2 != 0, mods & 4 != 0);
    let mut out = Vec::new();
    if alt {
        out.push(0x1b);
    }
    match code {
        13 => out.push(b'\r'),
        9 => out.push(b'\t'),
        27 => out.push(0x1b),
        127 => out.push(0x7f),
        8 => out.push(0x08),
        c => {
            let ch = char::from_u32(c)?;
            if ctrl && ch.is_ascii() && (ch == ' ' || ('@'..='~').contains(&ch)) {
                out.push((ch.to_ascii_uppercase() as u8) & 0x1f);
            } else if (0xe000..=0xf8ff).contains(&c) {
                return None;
            } else {
                let mut buf = [0u8; 4];
                out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
            }
        }
    }
    Some(out)
}

/// **The raw stream with every `CSI … u` rewritten to its legacy bytes**, for a pane whose
/// program never asked for the protocol. Everything else passes through untouched.
pub fn legacy_bytes(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i..].starts_with(b"\x1b[") {
            let mut j = i + 2;
            while j < raw.len() && (0x30..=0x3f).contains(&raw[j]) {
                j += 1;
            }
            if j < raw.len() && raw[j] == b'u' {
                let params = &raw[i + 2..j];
                let legacy = kitty_fields(params).and_then(|(code, mods)| {
                    if code == 13 && mods & 1 != 0 {
                        Some(vec![b'\r'])
                    } else {
                        kitty_legacy(code, mods)
                    }
                });
                if let Some(bytes) = legacy {
                    out.extend_from_slice(&bytes);
                }
                i = j + 1;
                continue;
            }
        }
        out.push(raw[i]);
        i += 1;
    }
    out
}

/// Where an OSC/APC string ends: its body's length and the bytes used including the
/// terminator (BEL, or ST = `ESC \`). `None` while it has not finished arriving.
fn string_end(b: &[u8]) -> Option<(usize, usize)> {
    for (i, &c) in b.iter().enumerate() {
        if c == 0x07 {
            return Some((i, i + 1));
        }
        if c == 0x1b && b.get(i + 1) == Some(&b'\\') {
            return Some((i, i + 2));
        }
    }
    None
}

/// `11;rgb:RRRR/GGGG/BBBB` → whether the background is light. Any other OSC is dropped.
fn background_reply(body: &[u8]) -> Option<Key> {
    let text = std::str::from_utf8(body).ok()?;
    let rgb = text.strip_prefix("11;")?.strip_prefix("rgb:")?;
    let mut chans = rgb.split('/').map(|h| {
        // 1–4 hex digits, scaled to 0..=1.
        let v = u32::from_str_radix(h, 16).ok()?;
        let max = (1u32 << (4 * h.len() as u32)) - 1;
        Some(v as f64 / max as f64)
    });
    let (r, g, b) = (chans.next()??, chans.next()??, chans.next()??);
    // Relative luminance, the sRGB weights; past half is a light background.
    let lum = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    Some(Key::Background { light: lum > 0.5 })
}

/// Standard base64 — the tree's one spelling of it (`letibot_transcript::media`), for OSC 52.
pub fn base64(bytes: &[u8]) -> String {
    letibot_transcript::media::encode_base64(bytes)
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

    /// **`ctrl-\` decodes to nothing at all, and that is what makes it the pane's way out.**
    ///
    /// The pane's exit must be a key **the program never receives**, or a program can trap it.
    /// This byte is one of the three this decoder has no arm for, so it can never arrive at
    /// [`App::key`] as a `Key` — which is why the interception lives on the raw stream
    /// ([`Terminal::raw_keys`]) and not on the key path, and why the way out cannot be
    /// something a program could also be given.
    ///
    /// **The letters around it are two keys**, which is the other half: the byte is *eaten*,
    /// not turned into a `Key::Char` or an `Esc`, so a head that forwarded keys would forward
    /// `ab` and `cd` with nothing between them and no way out at all.
    #[test]
    fn the_way_out_byte_decodes_to_nothing_and_cannot_hide_in_anything() {
        assert!(decode(&[0x1c]).is_empty(), "ctrl-\\ is not a `Key`");
        assert_eq!(
            decode(b"ab\x1ccd"),
            vec![
                Key::Char('a'),
                Key::Char('b'),
                Key::Char('c'),
                Key::Char('d')
            ],
            "the byte is eaten, and the letters around it are two keys"
        );
        // It cannot be part of a character (it is below 0x20, so no UTF-8 sequence contains
        // it) and it cannot be the final byte of a CSI sequence (those are 0x40-0x7e), so a
        // raw scan for it is exact and not a guess about where a sequence ends.
        assert!(0x1c < 0x20);
        assert!(!(0x40..=0x7e).contains(&0x1c));
    }

    /// **The kitty keyboard means the keys this decoder already knew**, plus the two it exists
    /// for. Every binding is reached through the legacy spelling, so Ctrl-C is still `CtrlC`
    /// and Alt+b is still a word left; Shift+Enter is the soft newline Alt+Enter was the only
    /// way to type; Esc is Esc.
    #[test]
    fn kitty_keys_decode_to_the_keys_this_head_already_binds() {
        let one = |b: &[u8]| {
            let (k, used) = decode_prefix(b, false);
            assert_eq!(used, b.len(), "{b:?} not consumed whole");
            k
        };
        assert_eq!(one(b"\x1b[13;2u"), vec![Key::SoftEnter]);
        assert_eq!(one(b"\x1b[27u"), vec![Key::Esc]);
        assert_eq!(one(b"\x1b[99;5u"), vec![Key::CtrlC]);
        assert_eq!(one(b"\x1b[118;5u"), vec![Key::CtrlV]);
        assert_eq!(one(b"\x1b[98;3u"), vec![Key::WordLeft]);
        // A plain key the protocol chose to report anyway.
        assert_eq!(one(b"\x1b[97u"), vec![Key::Char('a')]);
        // The keypad's private-use codes have no legacy spelling, and are not typed.
        assert_eq!(one(b"\x1b[57399u"), Vec::<Key>::new());
        // An arrow is still the legacy CSI under flag 1.
        assert_eq!(one(b"\x1b[A"), vec![Key::Up]);
    }

    #[test]
    fn focus_reports_and_the_background_reply_are_keys_not_text() {
        let (k, _) = decode_prefix(b"\x1b[I\x1b[O", false);
        assert_eq!(k, vec![Key::FocusIn, Key::FocusOut]);
        // Ghostty answers OSC 11 with four hex digits a channel, terminated by ST or BEL.
        let (k, used) = decode_prefix(b"\x1b]11;rgb:ffff/ffff/ffff\x1b\\x", false);
        assert_eq!(k, vec![Key::Background { light: true }, Key::Char('x')]);
        assert_eq!(used, 26, "the reply and the key after it, all consumed");
        let (k, _) = decode_prefix(b"\x1b]11;rgb:1e1e/1e1e/2e2e\x07", false);
        assert_eq!(k, vec![Key::Background { light: false }]);
        // Half a reply is held, not typed.
        let (k, used) = decode_prefix(b"\x1b]11;rgb:ff", false);
        assert!(k.is_empty() && used == 0, "{k:?} {used}");
        // A kitty graphics reply (APC) is swallowed whole.
        let (k, _) = decode_prefix(b"\x1b_Gi=1;OK\x1b\\", false);
        assert!(k.is_empty(), "{k:?}");
    }

    /// **The pane's program reads legacy bytes**, whatever spelling the operator's terminal
    /// used — and the way out, `ctrl-\`, is still the byte the pane scans for.
    #[test]
    fn the_pane_gets_legacy_bytes_under_the_kitty_keyboard() {
        assert_eq!(legacy_bytes(b"ls\x1b[99;5u"), b"ls\x03".to_vec());
        assert_eq!(legacy_bytes(b"\x1b[92;5u"), vec![0x1c]);
        assert_eq!(legacy_bytes(b"\x1b[27u"), vec![0x1b]);
        assert_eq!(legacy_bytes(b"\x1b[13;2u"), b"\r".to_vec());
        // Everything that is not a `CSI … u` passes untouched.
        assert_eq!(
            legacy_bytes(b"\x1b[A\x1b[1;5C"),
            b"\x1b[A\x1b[1;5C".to_vec()
        );
    }

    #[test]
    fn base64_is_the_standard_padded_alphabet() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64("ключ".as_bytes()), "0LrQu9GO0Yc=");
    }
}
