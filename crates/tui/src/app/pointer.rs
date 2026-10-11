//! **The pointer: what it is over, and what it drags across.**
//!
//! The operator, 2026-10-11: *"simple mouse selection doesnt work in letibot - only with shift.
//! in rano it works"*, and *"i want clickable things to be somehow hinted - like say gray changed
//! to white, or the pointer changed"*.
//!
//! The head turns on mouse reporting so a click and the wheel reach it, and a terminal reporting
//! the mouse leaves selection to the program (Shift is its override). So the head selects, the
//! way rano does in its own pane:
//!
//! * **A press, a drag and a release** are a selection: the cells between the press and the
//!   pointer are drawn reversed while the drag goes on, and the release copies their text with
//!   OSC 52 — `/copy`'s door, and for its reason: it works over ssh, where a `pbcopy` would fill
//!   the far machine's clipboard. A press and a release with no motion
//!   between them are a click, delivered as the same [`Key::Click`] every click has always been.
//! * **Motion with no button held** is hover: whatever a click there would act on — a key hint,
//!   an edit card, a count label, a pane's or a card's row — is underlined, and the terminal is
//!   asked for the hand pointer (OSC 22) while it is.
//!
//! Both are drawn onto the finished frame ([`App::paint_pointer`]), after everything else, so no
//! renderer knows about either.

use super::*;

/// **What the pointer is over**, as cells of the last frame: a row, and the columns to mark —
/// `None` for the row's own text, first visible cell to last.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Hovered {
    pub(crate) y: usize,
    pub(crate) cols: Option<(usize, usize)>,
}

/// **The left button's run**, from the press to now.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Drag {
    pub(crate) from: Option<(u16, u16)>,
    pub(crate) to: Option<(u16, u16)>,
}

/// One piece of a frame row: an escape sequence, or one visible character and its width.
enum Piece<'a> {
    Escape(&'a str),
    Cell(char, usize),
}

fn pieces(row: &str) -> Vec<Piece<'_>> {
    let mut out = Vec::new();
    let mut i = 0;
    let b = row.as_bytes();
    while i < row.len() {
        if b[i] == 0x1b {
            let start = i;
            i += 1;
            match b.get(i) {
                Some(b'[') => {
                    i += 1;
                    while i < row.len() && !(0x40..=0x7e).contains(&b[i]) {
                        i += 1;
                    }
                    i = (i + 1).min(row.len());
                }
                Some(b']') => {
                    i += 1;
                    while i < row.len() {
                        if b[i] == 0x07 {
                            i += 1;
                            break;
                        }
                        if b[i] == 0x1b && b.get(i + 1) == Some(&b'\\') {
                            i += 2;
                            break;
                        }
                        i += 1;
                    }
                }
                _ => i = (i + 1).min(row.len()),
            }
            out.push(Piece::Escape(&row[start..i]));
            continue;
        }
        let ch = row[i..].chars().next().expect("in bounds");
        let w = crate::ui::render::visible_width(&ch.to_string());
        out.push(Piece::Cell(ch, w));
        i += ch.len_utf8();
    }
    out
}

/// **Cells `x0..x1` of a frame row, restyled** with `on` … `off` — escape-aware, and re-applied
/// after every style change inside the run, because a row resets its style between its spans and
/// a reset would otherwise end the mark halfway along.
pub(crate) fn restyle(row: &str, x0: usize, x1: usize, on: &str, off: &str) -> String {
    let mut out = String::with_capacity(row.len() + 16);
    let mut col = 0usize;
    let mut inside = false;
    for p in pieces(row) {
        match p {
            Piece::Escape(e) => {
                out.push_str(e);
                if inside && e.ends_with('m') && e.starts_with("\x1b[") {
                    out.push_str(on);
                }
            }
            Piece::Cell(c, w) => {
                if inside && col >= x1 {
                    out.push_str(off);
                    inside = false;
                }
                if !inside && col >= x0 && col < x1 {
                    out.push_str(on);
                    inside = true;
                }
                out.push(c);
                col += w;
            }
        }
    }
    if inside {
        out.push_str(off);
    }
    out
}

/// The first and one-past-last visible, non-blank cells of a row — `None` for a blank one.
fn text_span(cs: &[char]) -> Option<(usize, usize)> {
    let blank = |c: &char| matches!(*c, ' ' | '\0');
    let a = cs.iter().position(|c| !blank(c))?;
    let b = cs.iter().rposition(|c| !blank(c))? + 1;
    Some((a, b))
}

/// **Which cells of a row are the row's content** — what a selection can take, as
/// `(first, one past last)`, or `None` for a row that is all frame.
///
/// The operator, 2026-10-12: *"in say opencode i cant even select the bars, it is like selecting
/// text in the editor doesnt put line numbers"*. So the frame is not selectable at all, rather
/// than copied and stripped: a code block's `┌─ lang` and `└─` rows and a pane's or popup's
/// `╭─`/`╰─` edges are none of a selection's, a `│ ` rail (a code block, a quote, a box's side)
/// or the operator's `▌ ` bar is skipped, a box row's right side too, and the head's own gutter is
/// never content. What the selection highlights is exactly what it copies.
fn content_span(cs: &[char], gutter: usize) -> Option<(usize, usize)> {
    let blank = |c: char| c == ' ' || c == '\0';
    let Some(a) = cs.iter().position(|c| !blank(*c)) else {
        // A blank row inside a selection is a blank line of it.
        return Some((gutter.min(cs.len()), gutter.min(cs.len())));
    };
    let first = cs[a];
    if matches!(first, '┌' | '└' | '╭' | '╰') {
        return None;
    }
    let start = if matches!(first, '│' | '▌' | '┃') {
        let after = a + 1;
        if cs.get(after) == Some(&' ') {
            after + 1
        } else {
            after
        }
    } else {
        gutter.min(a)
    };
    let r = cs.iter().rposition(|c| !blank(*c)).unwrap_or(a);
    let end = if r > start && matches!(cs[r], '│' | '┃') {
        cs[..r]
            .iter()
            .rposition(|c| !blank(*c))
            .map_or(start, |e| (e + 1).max(start))
    } else {
        r + 1
    };
    Some((start, end.max(start)))
}

impl App {
    /// **What a click at `(x, y)` would act on**, as cells to mark — `None` where a click does
    /// nothing. The same answers the click itself uses: a key hint, then the last frame's hit map.
    fn clickable_at(&self, x: u16, y: u16) -> Option<Hovered> {
        if let Some(k) = self.key_hint_at(x, y) {
            return Some(Hovered {
                y: k.y,
                cols: Some((k.x, k.x + k.w)),
            });
        }
        let hit = self.hit_at(x, y)?;
        let row = usize::from(y);
        match hit.target {
            Target::CountLabel(_) | Target::QueueTab(_) => Some(Hovered {
                y: row,
                cols: Some((hit.rect.x, hit.rect.x + hit.rect.w)),
            }),
            // An edit or a write in the conversation opens its change; anything else in it is
            // text, and text is selected, not clicked.
            Target::Conversation => self
                .file_rows
                .iter()
                .any(|(r, _)| self.conversation_row(*r) == Some(row))
                .then_some(Hovered { y: row, cols: None }),
            _ => hit.stop_at(row).map(|_| Hovered { y: row, cols: None }),
        }
    }

    /// The screen row a conversation-window row is drawn on, through the conversation's hit.
    fn conversation_row(&self, r: usize) -> Option<usize> {
        let top = self
            .hits
            .iter()
            .find(|h| h.target == Target::Conversation)?
            .rect
            .y;
        Some(top + r)
    }

    /// **The pointer moved with no button held.** The mark follows on the next frame; no full
    /// repaint is asked for, because only the marked row's text changes and the frame's diff
    /// writes exactly that row.
    pub(crate) fn hover_at(&mut self, x: u16, y: u16) {
        let over = self.clickable_at(x, y);
        if over != self.hovered {
            if over.is_some() != self.hovered.is_some() {
                self.pointer_hand = Some(over.is_some());
            }
            self.hovered = over;
        }
    }

    /// **The left button went down**: the start of a click or of a selection, and the end of
    /// any selection already drawn.
    pub(crate) fn press_at(&mut self, x: u16, y: u16) {
        self.drag = Drag {
            from: Some((x, y)),
            to: None,
        };
        self.selection = None;
    }

    /// Motion with the left button held: the selection runs from the press to here.
    pub(crate) fn drag_to(&mut self, x: u16, y: u16) {
        if self.drag.from.is_some() {
            self.drag.to = Some((x, y));
            self.selection = self.drag.from.map(|f| (f, (x, y)));
            self.hovered = None;
        }
    }

    /// **The left button came up**: the end of a selection, which is copied — or, with no motion
    /// since the press, a click at the press.
    pub(crate) fn release(&mut self) -> Option<Key> {
        let drag = std::mem::take(&mut self.drag);
        match (drag.from, drag.to) {
            (Some(from), Some(to)) if from != to => {
                self.copy_selection();
                None
            }
            (Some((x, y)), _) => {
                self.selection = None;
                Some(Key::Click { x, y })
            }
            _ => None,
        }
    }

    /// The selection's two ends, in reading order: `(start, end)`, each `(x, y)`, end inclusive.
    fn selection_span(&self) -> Option<((usize, usize), (usize, usize))> {
        let (a, b) = self.selection?;
        let (a, b) = (
            (usize::from(a.0), usize::from(a.1)),
            (usize::from(b.0), usize::from(b.1)),
        );
        Some(if (a.1, a.0) <= (b.1, b.0) {
            (a, b)
        } else {
            (b, a)
        })
    }

    /// **The selected text, off the frame it was drawn on**: each row's selected cells, the
    /// trailing blanks dropped and the head's own gutter taken off the front — what was read, not
    /// the frame's padding.
    pub(crate) fn selected_text(&self) -> Option<String> {
        let ((x0, y0), (x1, y1)) = self.selection_span()?;
        let gutter = Self::gutter(self.term_cols);
        let mut lines = Vec::new();
        for y in y0..=y1 {
            let Some(row) = self.hint_rows.get(y) else {
                break;
            };
            let cs = keyhints::cells(row.text());
            // A frame row is none of the selection's: no line, not even an empty one.
            let Some((s, e)) = content_span(&cs, gutter) else {
                continue;
            };
            let from = if y == y0 { x0.max(s) } else { s };
            let to = if y == y1 { (x1 + 1).min(e) } else { e };
            let line: String = cs
                .get(from.min(cs.len())..to.max(from).min(cs.len()))
                .unwrap_or(&[])
                .iter()
                .filter(|c| **c != '\0')
                .collect();
            lines.push(line.trim_end().to_string());
        }
        let text = lines.join("\n");
        (!text.trim().is_empty()).then_some(text)
    }

    /// **Put the selection on the clipboard** — through `/copy`'s door (`clipboard_out`, which
    /// the driver writes as OSC 52), and said either way: a terminal not known to take OSC 52 is
    /// told so rather than left looking like a copy that worked.
    fn copy_selection(&mut self) {
        let Some(text) = self.selected_text() else {
            return;
        };
        if !self.features.clipboard {
            self.say(
                "selected — but this terminal is not known to take OSC 52, so nothing was \
                 copied (LETIBOT_TERM_FEATURES=clipboard turns it on; under tmux, \
                 `set -g set-clipboard on`)",
            );
            return;
        }
        let n = text.chars().count();
        self.clipboard_out = Some(text);
        self.say(&format!("copied {n} character(s)"));
    }

    /// **The pointer shape the terminal should show**, when it changed: the hand over something
    /// clickable, the default off it (OSC 22 — ignored by a terminal that does not speak it).
    pub fn take_pointer(&mut self) -> Option<bool> {
        self.pointer_hand.take()
    }

    /// **The hover mark and the selection, painted onto the finished frame** — the last thing a
    /// frame does, so no renderer has to know about the pointer.
    pub(crate) fn paint_pointer(&self, frame: &mut [String]) {
        if let Some(((x0, y0), (x1, y1))) = self.selection_span() {
            let gutter = Self::gutter(self.term_cols);
            for y in y0..=y1.min(frame.len().saturating_sub(1)) {
                // Only the row's content is marked — the frame is not selectable (see
                // `content_span`), so the bars a selection crosses stay as they are drawn.
                let Some((s, e)) = content_span(&keyhints::cells(&frame[y]), gutter) else {
                    continue;
                };
                let from = if y == y0 { x0.max(s) } else { s };
                let to = if y == y1 { (x1 + 1).min(e) } else { e };
                if from < to {
                    frame[y] = restyle(&frame[y], from, to, "\x1b[7m", "\x1b[27m");
                }
            }
            return;
        }
        if let Some(h) = self.hovered
            && let Some(row) = frame.get_mut(h.y)
        {
            let cols = h.cols.or_else(|| text_span(&keyhints::cells(row)));
            if let Some((a, b)) = cols {
                *row = restyle(row, a, b, "\x1b[4m", "\x1b[24m");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_restyle_marks_exactly_its_cells_through_the_rows_own_resets() {
        let row = "  \x1b[2mesc\x1b[0m closes · tab";
        let marked = restyle(row, 2, 12, "<", ">");
        assert_eq!(marked, "  \x1b[2m<esc\x1b[0m< closes> · tab");
        // Past the end of the row, the mark closes at the end.
        assert_eq!(restyle("abc", 1, 99, "<", ">"), "a<bc>");
        assert_eq!(restyle("abc", 5, 9, "<", ">"), "abc");
    }
}
