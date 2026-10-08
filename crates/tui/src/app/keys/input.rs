//! **What the terminal said, read as this head's [`Key`].**
//!
//! The bytes are rano's to read: `rano::term` decodes them into an [`Event`] that says what
//! was *pressed* — `k` with Ctrl, Enter with Alt — and nothing about what it means. What it
//! means is this head's, and this is the one place that decides it. Until the terminal layer
//! moved into rano, letibot's own decoder produced [`Key`] directly (`0x0b` *was*
//! `KillToEnd`); this map reproduces exactly what that decoder produced, so every byte sequence
//! the operator's terminal sends still means the key it meant.
//!
//! # The readline keys
//!
//! Ctrl+A/E are Home/End, Ctrl+B/F are Left/Right, Ctrl+K/U kill to the end and the start,
//! Ctrl+W kills a word back, Ctrl+Y yanks, Ctrl+Z and Ctrl+_ undo, Alt+Z redoes. Alt+B/F and
//! Ctrl+Left/Right move by word, Alt+Backspace kills a word back, and Alt+Enter — or Shift+Enter
//! under the kitty keyboard — is a newline that does not submit. Any other Alt+key is swallowed
//! rather than typed, so Alt+j does not insert a `j`.
//!
//! # What maps to nothing, on purpose
//!
//! **Ctrl+`\` (`0x1c`)** is the pane's way out. A program inside a pane must never be given it,
//! or it could trap it, so it is found on the raw stream (`Terminal::raw_input`, read in the
//! head's loop) before anything is forwarded — and it is never a [`Key`]. rano decodes it as
//! Ctrl+`\`; this map is where that is dropped. Ctrl+`]`, Ctrl+`^` and Ctrl+Space likewise:
//! letibot's decoder had no arm for them, and a key that did nothing stays a key that does
//! nothing.
//!
//! Every mouse report but a plain wheel step and a plain left press is dropped: selecting text
//! stays the terminal's own Shift+drag, and a report the head does not act on must never
//! become typed punctuation.

use super::Key;
use rano::term::{Event, KeyCode, KeyEvent, Mods, MouseButton, MouseEvent, MouseKind};

/// The [`Key`] a terminal event is, or `None` for one this head does not act on.
pub fn key_of(e: Event) -> Option<Key> {
    match e {
        Event::Key(k) => key(k),
        Event::Paste(s) => Some(Key::Paste(s)),
        Event::Mouse(m) => mouse(m),
        Event::FocusGained => Some(Key::FocusIn),
        Event::FocusLost => Some(Key::FocusOut),
        Event::Background { light } => Some(Key::Background { light }),
    }
}

fn key(k: KeyEvent) -> Option<Key> {
    let m = k.mods;
    Some(match k.code {
        KeyCode::Char(c) if m.is_empty() => Key::Char(c),
        KeyCode::Char(c) if m == Mods::CTRL => return ctrl(c),
        // Alt arrives as an ESC before the key; only these three mean anything.
        KeyCode::Char('b') if m == Mods::ALT => Key::WordLeft,
        KeyCode::Char('f') if m == Mods::ALT => Key::WordRight,
        // In many terminals Ctrl+Shift+Z arrives byte-identical to Ctrl+Z, so redo needs a
        // second binding.
        KeyCode::Char('z') if m == Mods::ALT => Key::Redo,
        KeyCode::Char(_) => return None,
        // Alt+Enter, and the kitty keyboard's Shift+Enter: the soft newline. Enter carries a
        // modifier only for those two.
        KeyCode::Enter if m.is_empty() => Key::Enter,
        KeyCode::Enter => Key::SoftEnter,
        KeyCode::Tab if m.is_empty() => Key::Tab,
        KeyCode::Tab => return None,
        KeyCode::Backspace if m.is_empty() => Key::Backspace,
        KeyCode::Backspace if m == Mods::ALT => Key::KillWordBack,
        KeyCode::Backspace => return None,
        KeyCode::Esc => Key::Esc,
        // Ctrl+arrow (`CSI 1;5 C`) is the word motion every editor binds it to. Every other
        // modifier on a cursor key is the key itself.
        KeyCode::Left if m == Mods::CTRL => Key::WordLeft,
        KeyCode::Right if m == Mods::CTRL => Key::WordRight,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::Delete => Key::Delete,
        KeyCode::BackTab | KeyCode::Insert | KeyCode::F(_) => return None,
    })
}

/// A control letter. Control keys rather than plain letters for the head's own chords,
/// because every printable character has to remain typeable.
fn ctrl(c: char) -> Option<Key> {
    Some(match c {
        // Readline's motion and kill keys, which are muscle memory in every shell.
        'a' => Key::Home,
        'b' => Key::Left,
        'e' => Key::End,
        'f' => Key::Right,
        'k' => Key::KillToEnd,
        'u' => Key::KillToStart,
        'w' => Key::KillWordBack,
        'y' => Key::Yank,
        // Raw mode means Ctrl+Z is not a suspend here, and a composer with no undo is what
        // makes a kill frightening.
        'z' | '_' => Key::Undo,
        'c' => Key::CtrlC,
        'd' => Key::Eof,
        'g' => Key::CtrlG,
        'l' => Key::CtrlL,
        'n' => Key::CtrlN,
        'o' => Key::CtrlO,
        'p' => Key::CtrlP,
        'q' => Key::CtrlQ,
        'r' => Key::CtrlR,
        's' => Key::CtrlS,
        't' => Key::CtrlT,
        'v' => Key::CtrlV,
        'x' => Key::CtrlX,
        // `\` is the pane's way out (see the module header); ` `, `]` and `^` were never keys.
        _ => return None,
    })
}

fn mouse(m: MouseEvent) -> Option<Key> {
    // A modified report (Shift/Alt/Ctrl held) is not one the head acts on.
    if !m.mods.is_empty() {
        return None;
    }
    match m.kind {
        MouseKind::WheelUp => Some(Key::WheelUp),
        MouseKind::WheelDown => Some(Key::WheelDown),
        // A left-button press is a click: an open picker takes the row under the pointer as
        // its selection, and Enter still does the switching.
        MouseKind::Press(MouseButton::Left) => Some(Key::Click { x: m.x, y: m.y }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    //! letibot's decoder tests, over rano's decoder and this map: every byte sequence they
    //! pinned still yields the same [`Key`].
    use super::*;
    use rano::term::{decode_prefix, legacy_bytes};

    fn decode(b: &[u8]) -> Vec<Key> {
        rano::term::decode(b)
            .into_iter()
            .filter_map(key_of)
            .collect()
    }

    /// `(keys, bytes consumed)`, as letibot's `decode_prefix` returned them.
    fn decode_keys(b: &[u8], force: bool) -> (Vec<Key>, usize) {
        let (events, used) = decode_prefix(b, force);
        (events.into_iter().filter_map(key_of).collect(), used)
    }

    fn paste_open(b: &[u8]) -> bool {
        rano::term::decode::paste_open(b)
    }

    #[test]
    fn arrows_and_control_keys_decode() {
        assert_eq!(decode(b"\x1b[A"), vec![Key::Up]);
        assert_eq!(decode(b"\x1b[B"), vec![Key::Down]);
        assert_eq!(decode(b"\x03"), vec![Key::CtrlC]);
        assert_eq!(decode(b"\r"), vec![Key::Enter]);
        assert_eq!(decode(b"\x7f"), vec![Key::Backspace]);
        assert_eq!(decode(b"\x1b"), vec![Key::Esc]);
        assert_eq!(decode(b"\x18"), vec![Key::CtrlX]);
        // **R22's chord.** `0x0e` had no arm in letibot's first decoder, so it was eaten —
        // a key that did nothing rather than a key bound to nothing, which is why it was free.
        assert_eq!(decode(b"\x0e"), vec![Key::CtrlN]);
        // Every C0 byte, as letibot's decoder read it. `None` is a byte it had no arm for.
        for (byte, want) in [
            (0x00u8, None),
            (0x01, Some(Key::Home)),
            (0x02, Some(Key::Left)),
            (0x03, Some(Key::CtrlC)),
            (0x04, Some(Key::Eof)),
            (0x05, Some(Key::End)),
            (0x06, Some(Key::Right)),
            (0x07, Some(Key::CtrlG)),
            (0x08, Some(Key::Backspace)),
            (0x09, Some(Key::Tab)),
            (0x0a, Some(Key::Enter)),
            (0x0b, Some(Key::KillToEnd)),
            (0x0c, Some(Key::CtrlL)),
            (0x0d, Some(Key::Enter)),
            (0x0e, Some(Key::CtrlN)),
            (0x0f, Some(Key::CtrlO)),
            (0x10, Some(Key::CtrlP)),
            (0x11, Some(Key::CtrlQ)),
            (0x12, Some(Key::CtrlR)),
            (0x13, Some(Key::CtrlS)),
            (0x14, Some(Key::CtrlT)),
            (0x15, Some(Key::KillToStart)),
            (0x16, Some(Key::CtrlV)),
            (0x17, Some(Key::KillWordBack)),
            (0x18, Some(Key::CtrlX)),
            (0x19, Some(Key::Yank)),
            (0x1a, Some(Key::Undo)),
            (0x1c, None),
            (0x1d, None),
            (0x1e, None),
            (0x1f, Some(Key::Undo)),
            (0x7f, Some(Key::Backspace)),
        ] {
            assert_eq!(
                decode(&[byte]),
                want.into_iter().collect::<Vec<_>>(),
                "{byte:#04x}"
            );
        }
    }

    #[test]
    fn a_read_that_ends_mid_utf8_loses_nothing() {
        // A read that split a character used to fail the whole decode and drop the bytes,
        // silently. Pasting a stack trace with an arrow or a box-drawing character in it
        // lost bytes.
        let whole = "héllo → wörld ⣿".as_bytes();
        for cut in 1..whole.len() {
            let (a, b) = whole.split_at(cut);
            let (mut keys, used) = decode_keys(a, false);
            // Whatever was not decodable is carried, never dropped.
            let mut rest = a[used..].to_vec();
            rest.extend_from_slice(b);
            keys.extend(decode_keys(&rest, true).0);
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
        // A lone ESC is the one deliberate exception: it is genuinely ambiguous, and it is
        // resolved in favour of the key a person pressed on purpose, because Esc twice is the
        // composer's interrupt and an Esc that waits for a disambiguating read is an Esc that
        // arrives a frame late.
        assert_eq!(decode_keys(b"\x1b", false), (vec![Key::Esc], 1));
        assert_eq!(decode(b"\x1b\x1b"), vec![Key::Esc, Key::Esc]);

        let whole = b"\x1b[1;5C";
        for cut in 2..whole.len() {
            let (keys, used) = decode_keys(&whole[..cut], false);
            assert!(keys.is_empty(), "cut {cut}: {keys:?}");
            assert_eq!(used, 0, "the partial sequence must be carried, not eaten");
        }
        assert_eq!(decode(whole), vec![Key::WordRight]);
    }

    #[test]
    fn the_wheel_arrives_as_sgr_mouse_and_every_other_report_is_dropped() {
        assert_eq!(decode(b"\x1b[<64;10;5M"), vec![Key::WheelUp]);
        assert_eq!(decode(b"\x1b[<65;10;5M"), vec![Key::WheelDown]);
        // A left-button press is a click, on 0-based coordinates; drags, motion and releases
        // are decoded and dropped: a report the head does not act on must never become typed
        // punctuation.
        assert_eq!(decode(b"\x1b[<0;3;4M"), vec![Key::Click { x: 2, y: 3 }]);
        assert_eq!(decode(b"\x1b[<32;3;4M"), Vec::<Key>::new());
        assert_eq!(decode(b"\x1b[<0;3;4m"), Vec::<Key>::new());
        // Other buttons, the sideways wheel, and anything with a modifier held.
        for report in [
            &b"\x1b[<1;3;4M"[..],
            b"\x1b[<2;3;4M",
            b"\x1b[<66;3;4M",
            b"\x1b[<67;3;4M",
            b"\x1b[<4;3;4M",
            b"\x1b[<68;3;4M",
            b"\x1b[<80;3;4M",
            b"\x1b[<35;3;4M",
        ] {
            assert_eq!(decode(report), Vec::<Key>::new(), "{report:?}");
        }
        // A report cut mid-sequence is carried, not eaten.
        let whole = b"\x1b[<65;1;1M";
        for cut in 2..whole.len() {
            let (keys, used) = decode_keys(&whole[..cut], false);
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
        // …and while the terminator has not arrived, nothing is consumed: the read loop is
        // still waiting for the rest of the paste.
        let open = &b[..b.len() - 3];
        assert!(paste_open(open));
        assert_eq!(decode_keys(open, false), (Vec::new(), 0));
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
            // The rest of the spellings letibot's decoder had an arm for.
            (b"\x1b\n", Key::SoftEnter),
            (b"\x1bf", Key::WordRight),
            (b"\x1bz", Key::Redo),
            (b"\x1b\x7f", Key::KillWordBack),
            (b"\x1b[1;5C", Key::WordRight),
            (b"\x1bOA", Key::Up),
            (b"\x1bOB", Key::Down),
            (b"\x1bOD", Key::Left),
            (b"\x1bOH", Key::Home),
            (b"\x1bOF", Key::End),
            (b"\x1b[F", Key::End),
            (b"\x1b[1~", Key::Home),
            (b"\x1b[7~", Key::Home),
            (b"\x1b[4~", Key::End),
            (b"\x1b[8~", Key::End),
            // A modifier on a cursor key other than Ctrl-on-left/right is the key itself.
            (b"\x1b[1;2A", Key::Up),
            (b"\x1b[1;3B", Key::Down),
            (b"\x1b[1;2C", Key::Right),
            (b"\x1b[1;3D", Key::Left),
            (b"\x1b[3;5~", Key::Delete),
            (b"\x1b[1;5H", Key::Home),
        ] {
            assert_eq!(decode(bytes), vec![want.clone()], "{bytes:?}");
        }
        // And the spellings it decoded to nothing — whole, so nothing is typed after them.
        for bytes in [
            &b"\x1bj"[..],
            b"\x1bB",
            b"\x1b\t",
            b"\x1b\x03",
            b"\x1b ",
            "\x1bé".as_bytes(),
            b"\x1b[Z",
            b"\x1b[2~",
            b"\x1b[15~",
            b"\x1bOP",
            b"\x1b[1;5P",
            b"\x1b[9~",
            b"\x1b[X",
        ] {
            assert_eq!(decode(bytes), Vec::<Key>::new(), "{bytes:?}");
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
    /// It can never arrive at [`App::key`] as a `Key` — which is why the interception lives on
    /// the raw stream (`Terminal::raw_input`) and not on the key path, and why the way out
    /// cannot be something a program could also be given.
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
        // Under the kitty keyboard too.
        assert!(decode(b"\x1b[92;5u").is_empty());
        // It cannot be part of a character (it is below 0x20, so no UTF-8 sequence contains
        // it) and it cannot be the final byte of a CSI sequence (those are 0x40-0x7e), so a
        // raw scan for it is exact and not a guess about where a sequence ends.
        assert!(0x1c < 0x20);
        assert!(!(0x40..=0x7e).contains(&0x1c));
    }

    /// **The kitty keyboard means the keys this head already binds**, plus the two it exists
    /// for. Every binding is reached through the legacy spelling, so Ctrl-C is still `CtrlC`
    /// and Alt+b is still a word left; Shift+Enter is the soft newline Alt+Enter was the only
    /// way to type; Esc is Esc.
    #[test]
    fn kitty_keys_decode_to_the_keys_this_head_already_binds() {
        let one = |b: &[u8]| {
            let (k, used) = decode_keys(b, false);
            assert_eq!(used, b.len(), "{b:?} not consumed whole");
            k
        };
        assert_eq!(one(b"\x1b[13;2u"), vec![Key::SoftEnter]);
        assert_eq!(one(b"\x1b[13;4u"), vec![Key::SoftEnter]);
        assert_eq!(one(b"\x1b[13;3u"), vec![Key::SoftEnter]);
        assert_eq!(one(b"\x1b[13;5u"), vec![Key::Enter]);
        assert_eq!(one(b"\x1b[27u"), vec![Key::Esc]);
        assert_eq!(one(b"\x1b[99;5u"), vec![Key::CtrlC]);
        assert_eq!(one(b"\x1b[118;5u"), vec![Key::CtrlV]);
        assert_eq!(one(b"\x1b[98;3u"), vec![Key::WordLeft]);
        assert_eq!(one(b"\x1b[127;3u"), vec![Key::KillWordBack]);
        assert_eq!(one(b"\x1b[122;6u"), vec![Key::Undo]);
        assert_eq!(one(b"\x1b[9;2u"), vec![Key::Tab]);
        // A plain key the protocol chose to report anyway.
        assert_eq!(one(b"\x1b[97u"), vec![Key::Char('a')]);
        // The keypad's private-use codes have no legacy spelling, and are not typed.
        assert_eq!(one(b"\x1b[57399u"), Vec::<Key>::new());
        // An arrow is still the legacy CSI under flag 1.
        assert_eq!(one(b"\x1b[A"), vec![Key::Up]);
    }

    #[test]
    fn focus_reports_and_the_background_reply_are_keys_not_text() {
        let (k, _) = decode_keys(b"\x1b[I\x1b[O", false);
        assert_eq!(k, vec![Key::FocusIn, Key::FocusOut]);
        // Ghostty answers OSC 11 with four hex digits a channel, terminated by ST or BEL.
        let (k, used) = decode_keys(b"\x1b]11;rgb:ffff/ffff/ffff\x1b\\x", false);
        assert_eq!(k, vec![Key::Background { light: true }, Key::Char('x')]);
        assert_eq!(used, 26, "the reply and the key after it, all consumed");
        let (k, _) = decode_keys(b"\x1b]11;rgb:1e1e/1e1e/2e2e\x07", false);
        assert_eq!(k, vec![Key::Background { light: false }]);
        // Half a reply is held, not typed.
        let (k, used) = decode_keys(b"\x1b]11;rgb:ff", false);
        assert!(k.is_empty() && used == 0, "{k:?} {used}");
        // A kitty graphics reply (APC) is swallowed whole.
        let (k, _) = decode_keys(b"\x1b_Gi=1;OK\x1b\\", false);
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
