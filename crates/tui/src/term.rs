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
        // affordance is that caret and a one-pixel bar is not one.
        let _ = out.write_all(b"\x1b[?1049h\x1b[?25l\x1b[?2004h\x1b[2 q");
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
        self.frames.set(self.frames.get() + 1);
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
        let s = paint_full(
            &mut self.shown.borrow_mut(),
            lines,
            cursor,
            self.cursor.get(),
            self.full.replace(false),
        );
        if s.is_empty() {
            self.silent.set(self.silent.get() + 1);
            return;
        }
        self.cursor.set(cursor);
        let mut out = std::io::stdout();
        // DEC 2026. A frame is a run of absolute cursor moves and erases, and a
        // terminal is otherwise free to present the screen in the middle of one.
        // Two lines, and the single most effective anti-flicker measure there is;
        // a terminal that does not know the mode ignores it.
        let _ = out.write_all(b"\x1b[?2026h");
        let _ = out.write_all(s.as_bytes());
        let _ = out.write_all(b"\x1b[?2026l");
        let _ = out.flush();
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
}

impl Drop for Terminal {
    fn drop(&mut self) {
        if self.entered {
            restore(self.fd, &self.original);
        }
    }
}

/// The bytes that turn `shown` into `lines`, updating `shown` as it goes.
///
/// Free and pure-ish so the flicker property is testable without a pty: the whole
/// claim is "an unchanged frame produces an empty string", and a test that needs a
/// terminal to check that is a test nobody runs.
pub fn paint(
    shown: &mut Vec<String>,
    lines: &[String],
    cursor: Option<(usize, usize)>,
    prev_cursor: Option<(usize, usize)>,
) -> String {
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
    shown: &mut Vec<String>,
    lines: &[String],
    cursor: Option<(usize, usize)>,
    prev_cursor: Option<(usize, usize)>,
    full: bool,
) -> String {
    let mut s = String::new();
    if full {
        s.push_str("\x1b[2J");
        shown.clear();
    }
    // Rows the frame no longer has: erase them, rather than the whole screen.
    for i in lines.len()..shown.len() {
        s.push_str(&format!("\x1b[{};1H\x1b[0m\x1b[K", i + 1));
    }
    // Rows the frame gained are blank on the glass — either it was just erased,
    // or the loop above erased them when the frame last shrank past them.
    shown.resize(lines.len(), String::new());
    for (i, l) in lines.iter().enumerate() {
        if shown[i] == *l {
            continue;
        }
        s.push_str(&format!("\x1b[{};1H\x1b[0m\x1b[K", i + 1));
        s.push_str(l);
        shown[i] = l.clone();
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

fn restore(fd: i32, original: &libc::termios) {
    unsafe { libc::tcsetattr(fd, libc::TCSANOW, original) };
    let mut out = std::io::stdout();
    // Every mode `enter` turned on, off again, in the reverse order: end any open
    // synchronised update, bracketed paste off, the cursor shape back to whatever
    // the operator's terminal had, then show it and leave the alternate screen.
    let _ = out.write_all(b"\x1b[?2026l\x1b[?2004l\x1b[0 q\x1b[?25h\x1b[?1049l");
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
            0x0c => {
                out.push(Key::CtrlL);
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
        assert!(!paint(&mut shown, &frame, cur, None).is_empty(), "first draw");
        for _ in 0..100 {
            assert_eq!(paint(&mut shown, &frame, cur, cur), "");
        }
    }

    #[test]
    fn a_frame_that_changed_height_does_not_erase_the_screen() {
        // The composer grows a row every time a prompt wraps. Erasing the screen
        // for that is a flash per wrap.
        let a: Vec<String> = ["one", "two", "three"].iter().map(|s| s.to_string()).collect();
        let mut shown = Vec::new();
        paint_full(&mut shown, &a, None, None, true);
        let mut b = a.clone();
        b.push("four".into());
        let bytes = paint_full(&mut shown, &b, None, None, false);
        assert!(!bytes.contains("\x1b[2J"), "{bytes:?}");
        assert!(bytes.contains("four"), "{bytes:?}");
        assert!(!bytes.contains("one"), "unchanged rows stay put: {bytes:?}");
        // And shrinking erases exactly the row that went, not the screen.
        let bytes = paint_full(&mut shown, &a, None, None, false);
        assert!(!bytes.contains("\x1b[2J"), "{bytes:?}");
        assert!(bytes.contains("\x1b[4;1H"), "row four is erased: {bytes:?}");
        // …and the glass is still an honest model of itself.
        assert_eq!(paint_full(&mut shown, &a, None, None, false), "");
    }

    #[test]
    fn only_the_rows_that_changed_are_written() {
        let a: Vec<String> = ["one", "two", "three"].iter().map(|s| s.to_string()).collect();
        let mut b = a.clone();
        b[1] = "TWO".into();
        let mut shown = Vec::new();
        paint(&mut shown, &a, None, None);
        let bytes = paint(&mut shown, &b, None, None);
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
