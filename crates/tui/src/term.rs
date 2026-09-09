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
        // Alternate screen, hide the cursor.
        let _ = out.write_all(b"\x1b[?1049h\x1b[?25l");
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

    /// Read whatever keys are available. Returns after at most ~100 ms.
    pub fn keys(&self) -> Vec<Key> {
        let mut buf = [0u8; 64];
        let n = match std::io::stdin().read(&mut buf) {
            Ok(n) => n,
            Err(_) => return Vec::new(),
        };
        decode(&buf[..n])
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
        let s = paint(
            &mut self.shown.borrow_mut(),
            lines,
            cursor,
            self.cursor.get(),
        );
        if s.is_empty() {
            self.silent.set(self.silent.get() + 1);
            return;
        }
        self.cursor.set(cursor);
        let mut out = std::io::stdout();
        let _ = out.write_all(s.as_bytes());
        let _ = out.flush();
    }

    /// Forget what is on the glass, so the next draw repaints everything.
    ///
    /// Ctrl-L, and the only honest answer to "something else wrote to my terminal":
    /// the diff is against a memory of the screen, and anything that writes behind
    /// the head's back makes that memory wrong.
    pub fn invalidate(&self) {
        self.shown.borrow_mut().clear();
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
    let mut s = String::new();
    if shown.len() != lines.len() {
        // Every row moved. One erase, then the whole frame.
        s.push_str("\x1b[2J");
        shown.clear();
        shown.resize(lines.len(), String::new());
        for (i, l) in lines.iter().enumerate() {
            s.push_str(&format!("\x1b[{};1H\x1b[0m\x1b[K", i + 1));
            s.push_str(l);
            shown[i] = l.clone();
        }
    } else {
        for (i, l) in lines.iter().enumerate() {
            if shown[i] == *l {
                continue;
            }
            s.push_str(&format!("\x1b[{};1H\x1b[0m\x1b[K", i + 1));
            s.push_str(l);
            shown[i] = l.clone();
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

fn restore(fd: i32, original: &libc::termios) {
    unsafe { libc::tcsetattr(fd, libc::TCSANOW, original) };
    let mut out = std::io::stdout();
    // Leave the alternate screen, show the cursor.
    let _ = out.write_all(b"\x1b[?25h\x1b[?1049l");
    let _ = out.flush();
}

/// Decode a read buffer into keys.
///
/// Enough of the escape vocabulary for a head: arrows, page up/down, and the
/// distinction between ESC alone and ESC as a prefix.
pub fn decode(b: &[u8]) -> Vec<Key> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            0x03 => {
                out.push(Key::CtrlC);
                i += 1;
            }
            // The two toggles and a redraw. Control keys rather than plain letters
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
            0x1b => {
                if i + 2 < b.len() && b[i + 1] == b'[' {
                    match b[i + 2] {
                        b'A' => {
                            out.push(Key::Up);
                            i += 3;
                        }
                        b'B' => {
                            out.push(Key::Down);
                            i += 3;
                        }
                        b'5' => {
                            out.push(Key::PageUp);
                            i += if i + 3 < b.len() { 4 } else { 3 };
                        }
                        b'6' => {
                            out.push(Key::PageDown);
                            i += if i + 3 < b.len() { 4 } else { 3 };
                        }
                        _ => {
                            out.push(Key::Esc);
                            i += 3;
                        }
                    }
                } else {
                    out.push(Key::Esc);
                    i += 1;
                }
            }
            c if c >= 0x20 => {
                // Decode one UTF-8 char so a paste of non-ASCII does not become
                // replacement characters.
                let len = match c {
                    0x00..=0x7f => 1,
                    0xc0..=0xdf => 2,
                    0xe0..=0xef => 3,
                    _ => 4,
                };
                let end = (i + len).min(b.len());
                if let Ok(s) = std::str::from_utf8(&b[i..end]) {
                    for ch in s.chars() {
                        out.push(Key::Char(ch));
                    }
                }
                i = end;
            }
            _ => i += 1,
        }
    }
    out
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
