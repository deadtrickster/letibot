//! **Every key hint on the screen is a button** — a click on `esc closes`, `ctrl-g subagents`,
//! `↑↓`, `C-o write out` or `/help` is that key, typed.
//!
//! The operator, 2026-10-11: *"say i open diff popup by clicking - i cant close it by clicking -
//! make at least Esc keymap entry clickable as well as maybe others. generally - all keymap items
//! anywhere must be clickable"*.
//!
//! # How, in one place
//!
//! The hints are drawn by a dozen renderers — the hint bar, every pane's footer, the popup's and
//! the editor's edges, the cards — so they are not recorded by each of them. After a frame is
//! composed, its rows are read back as text ([`scan`]) and every `key label` segment of a hint
//! line becomes a [`KeyHint`]: where it is, and the bytes a terminal sends for that key. A click
//! on one queues those bytes ([`App::press_hint`]), and the head's next read takes them as if
//! they had been typed (`head::read_routed`) — so the key reaches whatever has the keyboard by
//! exactly the route a keypress takes, and nothing here has to know what any key does.
//!
//! **Not the conversation and not the composer.** Their text is the model's and the operator's,
//! and a `p reads` in somebody's prose is not a button: rows the conversation is drawn on, the
//! composer's boxed rows, and the editor's own text are never scanned.

use super::*;
use std::ops::ControlFlow;

/// **One key hint on the last frame**: its cells, and what pressing it types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KeyHint {
    pub(crate) y: usize,
    pub(crate) x: usize,
    pub(crate) w: usize,
    /// The bytes a terminal sends for the key.
    pub(crate) bytes: Vec<u8>,
    /// **A hint on the editor's frame** — rano's keys: the editor takes the keyboard first.
    pub(crate) editor: bool,
    /// **A `/command`**, typed only into an empty prompt: into a half-written one it would land
    /// in the middle of somebody's sentence.
    pub(crate) command: bool,
}

/// **One frame row, and the key hints read off it** — the cache [`App::scan_hints`] keeps so an
/// unchanged row is not read twice. `key` is whether the row was skipped and whether it is the
/// editor's frame, which can change while the text does not.
#[derive(Debug, Clone, Default)]
pub(crate) struct HintRow {
    row: String,
    key: (bool, bool),
    pub(crate) hints: Vec<KeyHint>,
}

/// **A row's cells, as text**: escape sequences (CSI, OSC) dropped, one entry per column — a wide
/// character takes two, the second a placeholder — so a column in the vector is a column on the
/// glass.
pub(crate) fn cells(row: &str) -> Vec<char> {
    let mut out = Vec::with_capacity(row.len());
    let mut it = row.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\x1b' {
            match it.next() {
                Some('[') => {
                    for d in it.by_ref() {
                        if ('@'..='~').contains(&d) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    while let Some(d) = it.next() {
                        if d == '\x07' {
                            break;
                        }
                        if d == '\x1b' && it.peek() == Some(&'\\') {
                            it.next();
                            break;
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        if c.is_control() {
            continue;
        }
        out.push(c);
        if crate::ui::render::visible_width(&c.to_string()) == 2 {
            out.push('\0');
        }
    }
    out
}

/// **The bytes one key token stands for**, or `None` for a word that is not a key. The token is
/// spelled the way this head's and rano's hints spell keys: `esc`, `enter`, `tab`, `ctrl-x` and
/// `alt-x`, rano's `C-x`, `M-x` and `<name>`, the arrows, the page keys, a `/command`, or one
/// letter (`p reads its output`).
fn key_bytes(token: &str) -> Option<(Vec<u8>, bool)> {
    let lower = token.to_ascii_lowercase();
    let seq = |s: &str| Some((s.as_bytes().to_vec(), false));
    let ctrl = |c: char| -> Option<(Vec<u8>, bool)> {
        let b = match c.to_ascii_lowercase() {
            c @ 'a'..='z' => (c as u8) & 0x1f,
            ']' => 0x1d,
            '^' => 0x1e,
            '/' | '_' => 0x1f,
            // `ctrl-\` is a `!term` pane's way out, and is never typed by a click.
            _ => return None,
        };
        Some((vec![b], false))
    };
    let one = |s: &str| {
        let mut cs = s.chars();
        match (cs.next(), cs.next()) {
            (Some(c), None) => Some(c),
            _ => None,
        }
    };
    match lower.as_str() {
        "esc" | "esc:" => return seq("\x1b"),
        "enter" | "enter:" | "⏎" => return seq("\r"),
        "tab" => return seq("\t"),
        "↑" | "<up>" | "up" => return seq("\x1b[A"),
        "↓" | "<down>" | "down" => return seq("\x1b[B"),
        "←" | "<left>" => return seq("\x1b[D"),
        "→" | "<right>" => return seq("\x1b[C"),
        "pgup" | "<pgup>" => return seq("\x1b[5~"),
        "pgdn" | "<pgdn>" => return seq("\x1b[6~"),
        "home" | "<home>" => return seq("\x1b[H"),
        "end" | "<end>" => return seq("\x1b[F"),
        "<del>" => return seq("\x1b[3~"),
        _ => {}
    }
    if let Some(n) = lower
        .strip_prefix("<f")
        .and_then(|r| r.strip_suffix('>'))
        .and_then(|d| d.parse::<u8>().ok())
    {
        let s = match n {
            1 => "\x1bOP",
            2 => "\x1bOQ",
            3 => "\x1bOR",
            4 => "\x1bOS",
            5 => "\x1b[15~",
            6 => "\x1b[17~",
            7 => "\x1b[18~",
            8 => "\x1b[19~",
            9 => "\x1b[20~",
            10 => "\x1b[21~",
            11 => "\x1b[23~",
            12 => "\x1b[24~",
            _ => return None,
        };
        return seq(s);
    }
    for p in ["ctrl-", "c-"] {
        if let Some(c) = lower.strip_prefix(p).and_then(one) {
            return ctrl(c);
        }
    }
    for p in ["alt-", "m-"] {
        if let Some(c) = token
            .get(p.len()..)
            .filter(|_| lower.starts_with(p))
            .and_then(one)
        {
            return Some((format!("\x1b{c}").into_bytes(), false));
        }
    }
    if let Some(cmd) = token.strip_prefix('/')
        && !cmd.is_empty()
        && cmd.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return Some((format!("/{cmd}\r").into_bytes(), true));
    }
    match one(token) {
        Some(c) if c.is_ascii_lowercase() || c == '?' => Some((vec![c as u8], false)),
        _ => None,
    }
}

/// **The key hints on one row** of text cells, at row `y`: every ` · `-separated segment that
/// starts with a key. A segment's first key takes the whole segment (`esc closes` is one button);
/// a segment that is only keys (`↑↓ PgUp PgDn Home End`, `pgup/pgdn`) gives each its own cells.
pub(crate) fn scan_row(y: usize, cs: &[char]) -> Vec<KeyHint> {
    let mut out = Vec::new();
    // Columns of the one-letter hints, kept only if the row proves to be a hint line.
    let mut letters: Vec<usize> = Vec::new();
    let edge = |c: char| {
        matches!(
            c,
            ' ' | '╭' | '╰' | '─' | '╮' | '╯' | '┤' | '├' | '▸' | '\0'
        )
    };
    // Segment bounds: split at " · ".
    let mut bounds = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i + 2 < cs.len() {
        if cs[i] == ' ' && cs[i + 1] == '·' && cs[i + 2] == ' ' {
            bounds.push((start, i));
            start = i + 3;
            i += 3;
        } else {
            i += 1;
        }
    }
    bounds.push((start, cs.len()));
    let single = bounds.len() == 1;
    for (a, b) in bounds {
        let mut a = a;
        while a < b && edge(cs[a]) {
            a += 1;
        }
        let mut b = b;
        while b > a && edge(cs[b - 1]) {
            b -= 1;
        }
        if a >= b {
            continue;
        }
        // The segment's words, with their columns.
        let mut words: Vec<(usize, String)> = Vec::new();
        let mut k = a;
        while k < b {
            while k < b && cs[k] == ' ' {
                k += 1;
            }
            let s = k;
            while k < b && cs[k] != ' ' {
                k += 1;
            }
            if k > s {
                words.push((s, cs[s..k].iter().filter(|c| **c != '\0').collect()));
            }
        }
        let Some((w0, first)) = words.first().cloned() else {
            continue;
        };
        // `↑↓` and `pgup/pgdn`: a run of keys packed into one word.
        let pieces = |w: &str, at: usize| -> Vec<(usize, usize, Vec<u8>, bool)> {
            if let Some((l, r)) = w.split_once('/')
                && !w.starts_with('/')
                && let (Some(lb), Some(rb)) = (key_bytes(l), key_bytes(r))
            {
                let lw = l.chars().count();
                return vec![
                    (at, lw, lb.0, lb.1),
                    (at + lw + 1, r.chars().count(), rb.0, rb.1),
                ];
            }
            if w.chars().count() > 1 && w.chars().all(|c| "↑↓←→".contains(c)) {
                return w
                    .chars()
                    .enumerate()
                    .filter_map(|(n, c)| key_bytes(&c.to_string()).map(|(b, s)| (at + n, 1, b, s)))
                    .collect();
            }
            key_bytes(w)
                .map(|(b, s)| vec![(at, w.chars().count(), b, s)])
                .unwrap_or_default()
        };
        let first_keys = pieces(&first, w0);
        if first_keys.is_empty() {
            continue;
        }
        // A one-letter key needs a label after it, and a line of its own segments: a lone
        // letter in a sentence is not a hint.
        let letter = first.chars().count() == 1 && first.chars().all(|c| c.is_ascii_lowercase());
        if letter && (words.len() < 2 || single) {
            continue;
        }
        if letter {
            letters.push(out.len());
        }
        let keys_only: Vec<Vec<(usize, usize, Vec<u8>, bool)>> =
            words.iter().map(|(at, w)| pieces(w, *at)).collect();
        if keys_only.iter().all(|p| !p.is_empty()) || first_keys.len() > 1 {
            for (at, w, bytes, command) in keys_only.into_iter().flatten() {
                out.push(KeyHint {
                    y,
                    x: at,
                    w,
                    bytes,
                    editor: false,
                    command,
                });
            }
        } else {
            let (_, _, bytes, command) = first_keys.into_iter().next().expect("checked non-empty");
            out.push(KeyHint {
                y,
                x: a,
                w: b - a,
                bytes,
                editor: false,
                command,
            });
        }
    }
    // **A one-letter key needs company**: `p reads its output` is a hint on a line that also has
    // `esc` or a `ctrl-` key, and the same shape in a sentence (`a sentence · with …`) is prose.
    let sure = out.iter().enumerate().any(|(n, _)| !letters.contains(&n));
    if !sure {
        return Vec::new();
    }
    out
}

impl App {
    /// **The key hints on a finished frame** — every row but the conversation's, the composer's
    /// and the editor's own text. Called by `screen` once the frame is whole.
    pub(crate) fn scan_hints(&mut self, frame: &[String]) {
        // **Only the rows that changed are read.** A frame equal to the last one costs this
        // function no allocation at all — the frame test holds the head to that — so each row's
        // hints are kept beside the row they were read from, and a row is read again only when
        // its text moved. Which rows are scanned at all can change without the text changing (a
        // pane opening over the conversation), so the skip rule is part of the cache's key.
        let convo: Vec<(usize, usize)> = self
            .hits
            .iter()
            .filter(|h| matches!(h.target, Target::Conversation | Target::Notice))
            .map(|h| (h.rect.y, h.rect.y + h.rect.h))
            .collect();
        let editor = self.editor_drawn().then_some(self.edit_area);
        self.hint_rows.truncate(frame.len());
        for (y, row) in frame.iter().enumerate() {
            let skipped = convo.iter().any(|(a, b)| y >= *a && y < *b)
                || editor.is_some_and(|a| {
                    let (top, h) = (usize::from(a.y), usize::from(a.h));
                    y >= top && y < top + h
                });
            let on_editor_frame = editor.is_some_and(|a| {
                let (top, h) = (usize::from(a.y), usize::from(a.h));
                y + 1 == top || y == top + h
            });
            let key = (skipped, on_editor_frame);
            if let Some(cached) = self.hint_rows.get(y)
                && cached.row == *row
                && cached.key == key
            {
                continue;
            }
            let hints = if skipped {
                Vec::new()
            } else {
                let cs = cells(row);
                if cs.iter().find(|c| **c != ' ').is_some_and(|c| *c == '│') {
                    Vec::new()
                } else {
                    let mut hs = scan_row(y, &cs);
                    for h in &mut hs {
                        h.editor = on_editor_frame;
                    }
                    hs
                }
            };
            let entry = HintRow {
                row: row.clone(),
                key,
                hints,
            };
            if y < self.hint_rows.len() {
                self.hint_rows[y] = entry;
            } else {
                self.hint_rows.push(entry);
            }
        }
    }

    /// The key hint under a cell of the last frame.
    pub(crate) fn key_hint_at(&self, x: u16, y: u16) -> Option<&KeyHint> {
        let (x, y) = (usize::from(x), usize::from(y));
        self.hint_rows
            .get(y)?
            .hints
            .iter()
            .find(|k| x >= k.x && x < k.x + k.w)
    }

    /// **A click on a key hint: the key, typed.** Queued for the head's next read, which takes it
    /// ahead of the terminal (`take_injected`). A hint on the editor's frame gives the editor the
    /// keyboard first — those are rano's keys — and a `/command` is typed only into an empty
    /// prompt.
    pub(crate) fn press_hint(&mut self, k: KeyHint) -> ControlFlow<Option<Action>> {
        if k.command && !self.editor.text().is_empty() {
            let cmd = String::from_utf8_lossy(&k.bytes).trim_end().to_string();
            self.say(&format!(
                "`{cmd}` is typed into an empty prompt — this one has text in it"
            ));
            return ControlFlow::Break(None);
        }
        if k.editor
            && let Some(p) = self.edit_pane.as_mut()
        {
            p.hidden = false;
            p.focused = true;
            p.dirty = true;
        }
        self.injected.extend(k.bytes);
        self.redraw = true;
        ControlFlow::Break(None)
    }

    /// **The keys a click typed**, for the head's read to take before the terminal's.
    pub fn take_injected(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.injected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hints(row: &str) -> Vec<(String, Vec<u8>)> {
        let cs = cells(row);
        scan_row(0, &cs)
            .into_iter()
            .map(|k| (cs[k.x..k.x + k.w].iter().collect(), k.bytes))
            .collect()
    }

    #[test]
    fn the_hint_lines_are_read_as_buttons() {
        assert_eq!(
            hints("  ╰─ esc closes · ↑↓ PgUp PgDn Home End · ctrl-] edits it ─╯"),
            vec![
                ("esc closes".into(), b"\x1b".to_vec()),
                ("↑".into(), b"\x1b[A".to_vec()),
                ("↓".into(), b"\x1b[B".to_vec()),
                ("PgUp".into(), b"\x1b[5~".to_vec()),
                ("PgDn".into(), b"\x1b[6~".to_vec()),
                ("Home".into(), b"\x1b[H".to_vec()),
                ("End".into(), b"\x1b[F".to_vec()),
                ("ctrl-] edits it".into(), vec![0x1d]),
            ]
        );
        assert_eq!(
            hints("╰─ C-g help · M-p back · <f8> open file · s split/unified ─╯"),
            vec![
                ("C-g help".into(), vec![0x07]),
                ("M-p back".into(), b"\x1bp".to_vec()),
                ("<f8> open file".into(), b"\x1b[19~".to_vec()),
                ("s split/unified".into(), b"s".to_vec()),
            ]
        );
        assert_eq!(
            hints("  ctrl-s sessions · /help"),
            vec![
                ("ctrl-s sessions".into(), vec![0x13]),
                ("/help".into(), b"/help\r".to_vec()),
            ]
        );
        // Through the escapes a frame row carries.
        assert_eq!(
            hints("\x1b[2m  esc\x1b[0m closes \x1b[38;5;8m·\x1b[0m p reads its output"),
            vec![
                ("esc closes".into(), b"\x1b".to_vec()),
                ("p reads its output".into(), b"p".to_vec()),
            ]
        );
    }

    #[test]
    fn prose_is_not_a_button() {
        assert!(hints("  the change at line 11 in notes.txt").is_empty());
        assert!(hints("  a sentence · with a separator").is_empty());
        // `ctrl-\` is a pane's way out and is never typed by a click.
        assert!(hints("  ctrl-\\ leaves the pane").is_empty());
        // A lone letter with nothing after it, or on a line with no other segment.
        assert!(hints("  p").is_empty());
        assert!(hints("  a fine day for it").is_empty());
    }
}
