//! **Bytes from the terminal, read as keys** — the legacy encodings, the kitty keyboard,
//! focus reports and the strings a terminal sends back (OSC, APC). Split out of `term.rs`:
//! [`super::terminal`] reads the bytes and this decides what they mean.

use crate::app::Key;

/// Is a bracketed paste open at the end of `b`?
///
/// The signal that says "keep reading": a paste's terminator is the only precise
/// evidence that more bytes are on their way, and without it a 40 KB paste is
/// decided by a 100 ms timeout in the middle of somebody's stack trace.
pub(super) fn paste_open(b: &[u8]) -> bool {
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
}
