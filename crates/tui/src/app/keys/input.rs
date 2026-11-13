//! What the terminal said, read as this head's [`Key`]: rano decodes the bytes into an [`Event`]
//! that says what was pressed, and this is the one place that decides what it means. Anything the
//! head does not act on answers `None`, so a report can never become typed punctuation.

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
        // ⌘ (super) is a chord, not a letter, and rano keeps the bit. ⌘C is deliberately `None`:
        // the TERMINAL copies the selection and this head's drag release copies it too
        // (`App::copy_selection`), so acting here would be a second copy of one fact.
        KeyCode::Char('c') if m == Mods::SUPER => return None,
        // ⌘V: the clipboard's picture, and nothing else - `ctrl-v` is the reader's window, and a chord
        // that attached a screenshot on its way to opening a row was two acts on one press.
        KeyCode::Char('v') if m == Mods::SUPER => Key::PasteImage,
        // ⌘Z undoes and ⌘⇧Z redoes, the chords everybody's hands already know. The shifted
        // spelling comes either as `z` with shift or as `Z` with it, depending on the terminal.
        KeyCode::Char('z') if m == Mods::SUPER => Key::Undo,
        KeyCode::Char('z') | KeyCode::Char('Z') if m == Mods::SUPER | Mods::SHIFT => Key::Redo,
        // Alt arrives as an ESC before the key; only these four mean anything. alt-b is a MARK on
        // an empty composer and word-left otherwise - settled in `App::key`, which can see the
        // composer (`app/feedback.rs`); alt-g is the other mark.
        KeyCode::Char('b') if m == Mods::ALT => Key::AltB,
        KeyCode::Char('g') if m == Mods::ALT => Key::AltG,
        // alt-a: answer the question a todo row is waiting on you for (`app/todos.rs`).
        KeyCode::Char('a') if m == Mods::ALT => Key::AltA,
        // alt-r / alt-p: reseat now, and show the reseat card (`app/reseat.rs`).
        KeyCode::Char('r') if m == Mods::ALT => Key::AltR,
        KeyCode::Char('p') if m == Mods::ALT => Key::AltP,
        // alt-n: open the newest notice line whole - the key for what a click on one does.
        KeyCode::Char('n') if m == Mods::ALT => Key::AltN,
        // alt-h: fold the questions waiting on you away, and back (`app/asks.rs`).
        KeyCode::Char('h') if m == Mods::ALT => Key::AltH,
        // alt-l / alt-j: the rooms sidebar on or off, and into it (`app/sidebar.rs`).
        KeyCode::Char('l') if m == Mods::ALT => Key::AltL,
        KeyCode::Char('j') if m == Mods::ALT => Key::AltJ,
        // alt-i / alt-m: the editor's two - the keyboard between the pane and the composer, and
        // the pane zoomed over the conversation or beside it (`app/editor.rs`).
        KeyCode::Char('i') if m == Mods::ALT => Key::AltI,
        KeyCode::Char('m') if m == Mods::ALT => Key::AltM,
        KeyCode::Char('<') if m == Mods::ALT => Key::AltNarrow,
        KeyCode::Char('>') if m == Mods::ALT => Key::AltWiden,
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
        // Ctrl+arrow (`CSI 1;5 C`) is the word motion every editor binds it to, and alt-↑ takes a
        // queued prompt back. Every other modifier on a cursor key is the key itself.
        KeyCode::Left if m == Mods::CTRL => Key::WordLeft,
        KeyCode::Right if m == Mods::CTRL => Key::WordRight,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Up if m == Mods::ALT => Key::AltUp,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::Delete => Key::Delete,
        // F1 is the one function key either program uses: `help`, in the shared key table. A
        // modifier on it is not that key, exactly as every other F-key is nothing here.
        KeyCode::F(1) if m.is_empty() => Key::Help,
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
        // Not `End` outright: on an empty prompt it is the editor. See `Key::CtrlE`.
        'e' => Key::CtrlE,
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
        // The editor pane's crossing: see `app/editor.rs`.
        ']' => Key::CtrlBracket,
        // The read-only override against an older daemon: see `Key::CtrlCaret`.
        '^' => Key::CtrlCaret,
        // `\` is the pane's way out (see the module header); ` ` was never a key.
        _ => return None,
    })
}

fn mouse(m: MouseEvent) -> Option<Key> {
    // A modified report (Shift/Alt/Ctrl held) is not one the head acts on.
    if !m.mods.is_empty() {
        return None;
    }
    match m.kind {
        // The position is kept: the notch scrolls what is under the pointer.
        MouseKind::WheelUp => Some(Key::Wheel {
            up: true,
            x: m.x,
            y: m.y,
        }),
        MouseKind::WheelDown => Some(Key::Wheel {
            up: false,
            x: m.x,
            y: m.y,
        }),
        // A left press is a click: an open picker takes the row under the pointer as its selection,
        // and Enter still switches. A press is not yet a click - with no motion before the release
        // it is one, and with motion it is a selection (`app::pointer`).
        MouseKind::Press(MouseButton::Left) => Some(Key::Press { x: m.x, y: m.y }),
        MouseKind::Drag(MouseButton::Left) => Some(Key::Drag { x: m.x, y: m.y }),
        MouseKind::Release(_) => Some(Key::Release { x: m.x, y: m.y }),
        MouseKind::Move => Some(Key::Hover { x: m.x, y: m.y }),
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

    /// The numpad's Enter is Enter: under the kitty keyboard this head asks for, a terminal reports
    /// the keypad by its own codes (`KP_ENTER` is `CSI 57414 u`). Its digits are digits too, so a
    /// card's number can be typed on it.
    #[test]
    fn the_numpad_enter_and_digits_are_the_keys_on_them() {
        assert_eq!(decode(b"\x1b[57414u"), vec![Key::Enter]);
        assert_eq!(
            decode(b"\x1bOM"),
            vec![Key::Enter],
            "the application-keypad spelling"
        );
        assert_eq!(decode(b"\x1b[57400u"), vec![Key::Char('1')]);
        assert_eq!(decode(b"\x1b[57420u"), vec![Key::Down]);
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
        // `0x0e` had no arm in letibot's first decoder, so it was eaten -
        // a key that did nothing rather than a key bound to nothing, which is why it was free.
        assert_eq!(decode(b"\x0e"), vec![Key::CtrlN]);
        // Every C0 byte, as letibot's decoder read it. `None` is a byte it had no arm for.
        for (byte, want) in [
            (0x00u8, None),
            (0x01, Some(Key::Home)),
            (0x02, Some(Key::Left)),
            (0x03, Some(Key::CtrlC)),
            (0x04, Some(Key::Eof)),
            (0x05, Some(Key::CtrlE)),
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
            (0x1d, Some(Key::CtrlBracket)),
            (0x1e, Some(Key::CtrlCaret)),
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
        // A read that split a character must not fail the whole decode and drop the bytes,
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
        // A lone ESC is the one deliberate exception: it is genuinely ambiguous, and it resolves in
        // favour of the key a person pressed on purpose - Esc twice is the composer's interrupt, and
        // an Esc that waits for a disambiguating read arrives a frame late.
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
        // The wheel keeps where the pointer was, 0-based like a click.
        assert_eq!(
            decode(b"\x1b[<64;10;5M"),
            vec![Key::Wheel {
                up: true,
                x: 9,
                y: 4
            }]
        );
        assert_eq!(
            decode(b"\x1b[<65;10;5M"),
            vec![Key::Wheel {
                up: false,
                x: 9,
                y: 4
            }]
        );
        // The left button's press, drag and release, and motion with no button, on 0-based
        // coordinates: a click is a press and a release with nothing between (`app::pointer`).
        assert_eq!(decode(b"\x1b[<0;3;4M"), vec![Key::Press { x: 2, y: 3 }]);
        assert_eq!(decode(b"\x1b[<32;3;4M"), vec![Key::Drag { x: 2, y: 3 }]);
        assert_eq!(decode(b"\x1b[<0;3;4m"), vec![Key::Release { x: 2, y: 3 }]);
        assert_eq!(decode(b"\x1b[<35;3;4M"), vec![Key::Hover { x: 2, y: 3 }]);
        // Other buttons, the sideways wheel, and anything with a modifier held.
        for report in [
            &b"\x1b[<1;3;4M"[..],
            b"\x1b[<2;3;4M",
            b"\x1b[<66;3;4M",
            b"\x1b[<67;3;4M",
            b"\x1b[<4;3;4M",
            b"\x1b[<68;3;4M",
            b"\x1b[<80;3;4M",
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
        assert_eq!(
            decode(whole),
            vec![Key::Wheel {
                up: false,
                x: 0,
                y: 0
            }]
        );
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
            (b"\x1bb", Key::AltB),
            // The reseat card's two (`app/reseat.rs`): reseat now, show the card.
            (b"\x1br", Key::AltR),
            (b"\x1bp", Key::AltP),
            // The newest notice, opened whole.
            (b"\x1bn", Key::AltN),
            (b"\x1bh", Key::AltH),
            (b"\x1bl", Key::AltL),
            (b"\x1bj", Key::AltJ),
            (b"\x1bi", Key::AltI),
            (b"\x1bm", Key::AltM),
            (b"\x1b<", Key::AltNarrow),
            (b"\x1b>", Key::AltWiden),
            (b"\x01", Key::Home),
            (b"\x09", Key::Tab),
            // Its own key now: end-of-line in the composer, the editor on an empty prompt. The
            // composer still reads it as `End` (see `Key::CtrlE`).
            (b"\x05", Key::CtrlE),
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
            // F1, the one function key either program uses: the shared key table's `help`.
            (b"\x1bOP", Key::Help),
        ] {
            assert_eq!(decode(bytes), vec![want.clone()], "{bytes:?}");
        }
        // And the spellings it decoded to nothing - whole, so nothing is typed after them.
        for bytes in [
            &b"\x1bq"[..],
            b"\x1bB",
            b"\x1b\t",
            b"\x1b\x03",
            b"\x1b ",
            "\x1bé".as_bytes(),
            b"\x1b[Z",
            b"\x1b[2~",
            b"\x1b[15~",
            // The next function key up: only f1 is a key here.
            b"\x1bOQ",
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

    /// `ctrl-\` decodes to nothing at all, and that is what makes it the pane's way out: the exit
    /// must be a key the program never receives, so it is caught on the raw stream, not here. The
    /// byte is eaten, not turned into a `Key::Char` or an `Esc`, so `ab` and `cd` stay two keys.
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
        assert!(!(0x40..=0x7e).contains(&0x1c));
    }

    /// The kitty keyboard means the keys this head already binds: every binding is reached through
    /// the legacy spelling (Ctrl-C is still `CtrlC`, Alt+b still a word left), plus Shift+Enter as
    /// the soft newline Alt+Enter was the only way to type.
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
        assert_eq!(one(b"\x1b[98;3u"), vec![Key::AltB]);
        assert_eq!(one(b"\x1b[127;3u"), vec![Key::KillWordBack]);
        assert_eq!(one(b"\x1b[122;6u"), vec![Key::Undo]);
        assert_eq!(one(b"\x1b[9;2u"), vec![Key::Tab]);
        // A plain key the protocol chose to report anyway.
        assert_eq!(one(b"\x1b[97u"), vec![Key::Char('a')]);
        // The keypad's private-use codes are the keys printed on it (rano 0.8.3): see
        // `the_numpad_enter_and_digits_are_the_keys_on_them`.
        assert_eq!(one(b"\x1b[57399u"), vec![Key::Char('0')]);
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

    /// ⌘C is a chord, and it is neither a `c` nor an interrupt: the copy is the terminal's and the
    /// drag release's (`App::copy_selection`), so this head's job is silence. The sequences below
    /// must decode WITH their super, or the chord types a `c` and ⌘⇧C interrupts the turn.
    #[test]
    fn a_cmd_chord_arrives_as_a_chord_and_never_as_a_typed_letter() {
        // The decode itself, in rano's vocabulary: the bit survives.
        assert_eq!(
            rano::term::decode(b"\x1b[99;9u"),
            vec![Event::Key(KeyEvent::new(KeyCode::Char('c'), Mods::SUPER))],
            "⌘C carries its super"
        );
        // ⌘C types nothing and interrupts nothing: the terminal owns the copy.
        assert_eq!(decode(b"\x1b[99;9u"), Vec::<Key>::new());
        // ⌘⌃C keeps both bits - it is NOT the interrupt.
        assert_eq!(
            rano::term::decode(b"\x1b[99;13u"),
            vec![Event::Key(KeyEvent::new(
                KeyCode::Char('c'),
                Mods::SUPER | Mods::CTRL
            ))]
        );
        assert_eq!(decode(b"\x1b[99;13u"), Vec::<Key>::new());
        // ⌘V asks the clipboard for a picture, on the key of its own - not `ctrl-v`'s.
        assert_eq!(decode(b"\x1b[118;9u"), vec![Key::PasteImage]);
        // ⌘Z undoes, ⌘⇧Z redoes, in either spelling of the shifted letter.
        assert_eq!(decode(b"\x1b[122;9u"), vec![Key::Undo]);
        assert_eq!(decode(b"\x1b[122;10u"), vec![Key::Redo]);
        assert_eq!(decode(b"\x1b[90;10u"), vec![Key::Redo]);
        // And the arm that makes all of it safe, stated in the modifiers this map CAN name: a
        // character carrying a modifier with no meaning here is swallowed, never typed.
        assert_eq!(
            key_of(Event::Key(KeyEvent::new(
                KeyCode::Char('c'),
                Mods::SHIFT | Mods::ALT
            ))),
            None,
            "a modifier this head does not bind is not a reason to type the letter"
        );
        assert_eq!(
            key_of(Event::Key(KeyEvent::new(KeyCode::Char('q'), Mods::SUPER))),
            None,
            "a ⌘ chord with no meaning here is swallowed too — ⌘Q must not type a `q`"
        );
        // The plain letter is still the plain letter, and the ctrl spellings are unchanged.
        assert_eq!(decode(b"c"), vec![Key::Char('c')]);
        assert_eq!(decode(b"\x1b[99u"), vec![Key::Char('c')]);
        assert_eq!(decode(b"\x1b[99;5u"), vec![Key::CtrlC]);
        assert_eq!(decode(b"\x03"), vec![Key::CtrlC]);
        assert_eq!(decode(b"\x1a"), vec![Key::Undo]);
    }

    /// The pane's program reads legacy bytes, whatever spelling the operator's terminal
    /// used - and the way out, `ctrl-\`, is still the byte the pane scans for.
    #[test]
    fn the_pane_gets_legacy_bytes_under_the_kitty_keyboard() {
        assert_eq!(legacy_bytes(b"ls\x1b[99;5u"), b"ls\x03".to_vec());
        assert_eq!(legacy_bytes(b"\x1b[92;5u"), vec![0x1c]);
        assert_eq!(legacy_bytes(b"\x1b[27u"), vec![0x1b]);
        assert_eq!(legacy_bytes(b"\x1b[13;2u"), b"\r".to_vec());
        // cmd is the head's: a super chord reaches the pane as nothing, never as its plain
        // letter, while plain `c` and ctrl-c still arrive.
        assert_eq!(legacy_bytes(b"\x1b[99;9u"), Vec::<u8>::new());
        assert_eq!(legacy_bytes(b"\x1b[99;13u"), Vec::<u8>::new());
        assert_eq!(legacy_bytes(b"\x1b[99u"), b"c".to_vec());
        assert_eq!(legacy_bytes(b"c"), b"c".to_vec());
        assert_eq!(legacy_bytes(b"\x1b[99;5u"), vec![3]);
        // Everything that is not a `CSI … u` passes untouched.
        assert_eq!(
            legacy_bytes(b"\x1b[A\x1b[1;5C"),
            b"\x1b[A\x1b[1;5C".to_vec()
        );
    }
}
