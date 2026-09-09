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

    /// Paint a full screen. One write, so a frame never tears.
    pub fn draw(&self, lines: &[String]) {
        let mut s = String::with_capacity(lines.iter().map(|l| l.len() + 8).sum());
        s.push_str("\x1b[H");
        for (i, l) in lines.iter().enumerate() {
            s.push_str("\x1b[K");
            s.push_str(l);
            if i + 1 < lines.len() {
                s.push_str("\r\n");
            }
        }
        s.push_str("\x1b[J");
        let mut out = std::io::stdout();
        let _ = out.write_all(s.as_bytes());
        let _ = out.flush();
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        if self.entered {
            restore(self.fd, &self.original);
        }
    }
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
