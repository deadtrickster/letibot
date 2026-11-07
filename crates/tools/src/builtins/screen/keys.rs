//! Keys and mouse reports as the bytes a terminal sends. Text is literal and `<Name>` is a key:
//! `<Enter>`, `<Esc>`, `<Tab>`, `<BTab>`, `<Space>`, `<BSpace>`, `<Up>` `<Down>` `<Left>` `<Right>`,
//! `<Home>`, `<End>`, `<PgUp>`, `<PgDn>`, `<Ins>`, `<Del>`, `<F1>`..`<F12>`, with `C-`, `M-`, `S-`
//! prefixes (`<C-c>`, `<M-x>`, `<C-M-Left>`). `<<` is a literal `<`.

use letibot_vt::{MouseMode, MouseTracking};

/// What a key string means, or the sentence that says which token was not a key.
pub fn encode(keys: &str, app_cursor: bool) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut rest = keys;
    while let Some(c) = rest.chars().next() {
        if c == '<' {
            if let Some(r) = rest.strip_prefix("<<") {
                out.push(b'<');
                rest = r;
                continue;
            }
            let Some(end) = rest.find('>') else {
                return Err("an unclosed `<`: write `<<` for a literal one".into());
            };
            out.extend(named(&rest[1..end], app_cursor)?);
            rest = &rest[end + 1..];
        } else {
            // A newline in typed text is the Enter key.
            let mut buf = [0u8; 4];
            if c == '\n' {
                out.push(b'\r');
            } else {
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
            rest = &rest[c.len_utf8()..];
        }
    }
    Ok(out)
}

fn named(token: &str, app_cursor: bool) -> Result<Vec<u8>, String> {
    let (mut ctrl, mut alt, mut shift) = (false, false, false);
    let mut name = token;
    // A prefix is `C-`, `M-` or `S-`; a lone `-` after it is the key itself.
    while name.len() > 2 && name.as_bytes()[1] == b'-' {
        match name.as_bytes()[0] {
            b'C' | b'c' => ctrl = true,
            b'M' | b'm' | b'A' | b'a' => alt = true,
            b'S' | b's' => shift = true,
            _ => break,
        }
        name = &name[2..];
    }
    let m = 1 + shift as u8 + 2 * alt as u8 + 4 * ctrl as u8;
    let csi = |final_byte: char, app: bool| -> Vec<u8> {
        if m > 1 {
            format!("\x1b[1;{m}{final_byte}").into_bytes()
        } else if app {
            format!("\x1bO{final_byte}").into_bytes()
        } else {
            format!("\x1b[{final_byte}").into_bytes()
        }
    };
    let tilde = |n: u8| -> Vec<u8> {
        if m > 1 {
            format!("\x1b[{n};{m}~").into_bytes()
        } else {
            format!("\x1b[{n}~").into_bytes()
        }
    };
    let lower = name.to_ascii_lowercase();
    let bytes = match lower.as_str() {
        "up" => csi('A', app_cursor),
        "down" => csi('B', app_cursor),
        "right" => csi('C', app_cursor),
        "left" => csi('D', app_cursor),
        "home" => csi('H', app_cursor),
        "end" => csi('F', app_cursor),
        "pgup" | "pageup" => tilde(5),
        "pgdn" | "pagedown" => tilde(6),
        "ins" | "insert" => tilde(2),
        "del" | "delete" => tilde(3),
        "btab" => b"\x1b[Z".to_vec(),
        "f1" | "f2" | "f3" | "f4" => {
            let f = (b'P' + (lower.as_bytes()[1] - b'1')) as char;
            if m > 1 {
                format!("\x1b[1;{m}{f}").into_bytes()
            } else {
                format!("\x1bO{f}").into_bytes()
            }
        }
        "f5" => tilde(15),
        "f6" => tilde(17),
        "f7" => tilde(18),
        "f8" => tilde(19),
        "f9" => tilde(20),
        "f10" => tilde(21),
        "f11" => tilde(23),
        "f12" => tilde(24),
        _ => {
            let base: Vec<u8> = match lower.as_str() {
                "enter" | "return" | "cr" => vec![b'\r'],
                "esc" | "escape" => vec![0x1b],
                "tab" if shift => return Ok(b"\x1b[Z".to_vec()),
                "tab" => vec![b'\t'],
                "space" => vec![b' '],
                "bspace" | "backspace" | "bs" => vec![0x7f],
                _ => {
                    let mut it = name.chars();
                    match (it.next(), it.next()) {
                        (Some(c), None) => single(c, ctrl, shift),
                        _ => {
                            return Err(format!(
                                "`<{token}>` is not a key. Names: Enter Esc Tab BTab Space BSpace \
                                 Up Down Left Right Home End PgUp PgDn Ins Del F1-F12, with C- M- \
                                 S- prefixes or one character (`<C-c>`)"
                            ));
                        }
                    }
                }
            };
            if alt {
                let mut v = vec![0x1b];
                v.extend(base);
                return Ok(v);
            }
            return Ok(base);
        }
    };
    Ok(bytes)
}

fn single(c: char, ctrl: bool, shift: bool) -> Vec<u8> {
    let c = if shift { c.to_ascii_uppercase() } else { c };
    if ctrl {
        let b = match c {
            'a'..='z' => Some(c as u8 - b'a' + 1),
            'A'..='Z' => Some(c as u8 - b'A' + 1),
            '@' | ' ' => Some(0),
            '[' => Some(27),
            '\\' => Some(28),
            ']' => Some(29),
            '^' => Some(30),
            '_' => Some(31),
            '?' => Some(127),
            _ => None,
        };
        if let Some(b) = b {
            return vec![b];
        }
    }
    c.to_string().into_bytes()
}

/// What the mouse did: a click, a move, or a wheel step.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pointer {
    Click(u8),
    Move,
    Wheel { up: bool },
}

/// The report for `pointer` at zero-based cell `(x, y)`, or the sentence that says why the
/// program would not take one: it has not asked for mouse reports.
pub fn mouse(mode: MouseMode, pointer: Pointer, x: usize, y: usize) -> Result<Vec<u8>, String> {
    if mode.tracking == MouseTracking::Off {
        return Err(
            "the program has not turned mouse reporting on, so a click would reach it as \
                    garbage. Use keys."
                .into(),
        );
    }
    if pointer == Pointer::Move && mode.tracking != MouseTracking::Motion {
        return Err("the program asked for clicks and drags, not bare motion".into());
    }
    let (col, row) = (x + 1, y + 1);
    let report = |code: u8, release: bool| -> Result<Vec<u8>, String> {
        if mode.sgr {
            let end = if release { 'm' } else { 'M' };
            Ok(format!("\x1b[<{code};{col};{row}{end}").into_bytes())
        } else if col > 223 || row > 223 {
            Err("this program uses the old mouse encoding, which cannot address past 223".into())
        } else {
            let code = if release { 3 } else { code };
            Ok(vec![
                0x1b,
                b'[',
                b'M',
                32 + code,
                32 + col as u8,
                32 + row as u8,
            ])
        }
    };
    match pointer {
        Pointer::Click(b) => {
            let mut v = report(b, false)?;
            v.extend(report(b, true)?);
            Ok(v)
        }
        Pointer::Move => report(32 + 3, false),
        Pointer::Wheel { up } => report(if up { 64 } else { 65 }, false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_literal_and_names_are_keys() {
        assert_eq!(encode("ls<Enter>", false).unwrap(), b"ls\r");
        assert_eq!(encode("a<<b", false).unwrap(), b"a<b");
        assert_eq!(encode("<C-c><Esc><M-x>", false).unwrap(), b"\x03\x1b\x1bx");
        assert_eq!(encode("<Up>", false).unwrap(), b"\x1b[A");
        assert_eq!(encode("<Up>", true).unwrap(), b"\x1bOA");
        assert_eq!(encode("<C-Left>", false).unwrap(), b"\x1b[1;5D");
        assert_eq!(encode("<F1><F5>", false).unwrap(), b"\x1bOP\x1b[15~");
        assert_eq!(encode("<S-Tab>", false).unwrap(), b"\x1b[Z");
        assert!(encode("<Bogus>", false).unwrap_err().contains("not a key"));
        assert!(encode("<oops", false).is_err());
    }

    #[test]
    fn mouse_follows_the_programs_encoding() {
        let sgr = MouseMode {
            tracking: MouseTracking::Press,
            sgr: true,
        };
        assert_eq!(
            mouse(sgr, Pointer::Click(0), 4, 2).unwrap(),
            b"\x1b[<0;5;3M\x1b[<0;5;3m"
        );
        assert_eq!(
            mouse(sgr, Pointer::Wheel { up: false }, 0, 0).unwrap(),
            b"\x1b[<65;1;1M"
        );
        let x10 = MouseMode {
            tracking: MouseTracking::Press,
            sgr: false,
        };
        assert_eq!(
            mouse(x10, Pointer::Click(2), 0, 0).unwrap(),
            [0x1b, b'[', b'M', 34, 33, 33, 0x1b, b'[', b'M', 35, 33, 33]
        );
        assert!(mouse(MouseMode::default(), Pointer::Click(0), 0, 0).is_err());
        assert!(mouse(sgr, Pointer::Move, 0, 0).is_err());
    }
}
