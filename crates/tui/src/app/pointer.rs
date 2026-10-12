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
//!   an edit card, a count label, a pane's or a card's row — is drawn a tad brighter, and the
//!   terminal is asked for the hand pointer (OSC 22) while it is. Not underlined: the operator,
//!   2026-10-12, *"underline - dont like it"*, *"make text a tad brighter"*.
//!
//! Both are drawn onto the finished frame ([`App::paint_pointer`]), after everything else, so no
//! renderer knows about either.

use super::*;

/// **What the pointer is over**, as cells of the last frame: every row of the clickable block
/// (an edit card's rows, a pane entry's lines), each with the columns to mark — `None` for the
/// row's own text, first visible cell to last. The operator, 2026-10-12: *"can you light up the
/// whole clickable block?"*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Hovered {
    pub(crate) rows: Vec<(usize, Option<(usize, usize)>)>,
}

impl Hovered {
    fn one(y: usize, cols: Option<(usize, usize)>) -> Hovered {
        Hovered {
            rows: vec![(y, cols)],
        }
    }
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

/// **Cells `x0..x1` of a frame row, a tad brighter** — normal intensity and bold, so a faint
/// hint and an ordinary row both lift, re-applied after every style change inside the run. Where
/// the run ends, the row's own style is put back exactly — a reset and every style change the
/// row made before that point, replayed — so nothing after the mark is brightened by it.
pub(crate) fn brighten(row: &str, x0: usize, x1: usize) -> String {
    const ON: &str = "\x1b[22;1m";
    let mut out = String::with_capacity(row.len() + 24);
    let mut seen = String::new();
    let mut col = 0usize;
    let mut inside = false;
    let close = |out: &mut String, seen: &str| {
        out.push_str("\x1b[0m");
        out.push_str(seen);
    };
    for p in pieces(row) {
        match p {
            Piece::Escape(e) => {
                out.push_str(e);
                if e.starts_with("\x1b[") && e.ends_with('m') {
                    if e == "\x1b[0m" || e == "\x1b[m" {
                        seen.clear();
                    } else {
                        seen.push_str(e);
                    }
                    if inside {
                        out.push_str(ON);
                    }
                }
            }
            Piece::Cell(c, w) => {
                if inside && col >= x1 {
                    close(&mut out, &seen);
                    inside = false;
                }
                if !inside && col >= x0 && col < x1 {
                    out.push_str(ON);
                    inside = true;
                }
                out.push(c);
                col += w;
            }
        }
    }
    if inside {
        close(&mut out, &seen);
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
            return Some(Hovered::one(k.y, Some((k.x, k.x + k.w))));
        }
        let hit = self.hit_at(x, y)?;
        let row = usize::from(y);
        match hit.target {
            Target::CountLabel(_) | Target::QueueTab(_) => Some(Hovered::one(
                row,
                Some((hit.rect.x, hit.rect.x + hit.rect.w)),
            )),
            // An edit or a write in the conversation opens its change — every row of it, the
            // whole card; anything else in the conversation is text, selected and not clicked.
            // A run marker's clause lights alone: each opens its own kind.
            Target::Conversation if self.marker_at(x, y).is_some() => {
                let m = self.marker_at(x, y)?.clone();
                let (row, from, to) = self.marker_screen(&m)?;
                Some(Hovered::one(row, Some((from, to))))
            }
            Target::Conversation => {
                let top = hit.rect.y;
                let r = row.checked_sub(top)?;
                let place = &self.file_rows.iter().find(|(rr, _)| *rr == r)?.1;
                Some(Hovered {
                    rows: self
                        .file_rows
                        .iter()
                        .filter(|(_, f)| f == place)
                        .map(|(rr, _)| (top + rr, None))
                        .collect(),
                })
            }
            // A notice and a note card are one hit a row, and every row of one opens it: the
            // block is every row recorded for the same one.
            Target::Notice | Target::NoteCard => {
                let k = hit.stop_at(row)?;
                let of = if hit.target == Target::Notice {
                    &self.notice_rows
                } else {
                    &self.card_rows
                };
                Some(Hovered {
                    rows: of
                        .iter()
                        .filter(|(_, kk)| *kk == k)
                        .map(|(y, _)| (*y, None))
                        .collect(),
                })
            }
            _ => self.stop_block(hit, row).map(|(_, rows)| Hovered {
                rows: rows.into_iter().map(|r| (r, None)).collect(),
            }),
        }
    }

    /// Whether a row of the last frame has nothing on it.
    fn blank_row(&self, y: usize) -> bool {
        self.hint_rows.get(y).is_none_or(|r| {
            keyhints::cells(r.text())
                .iter()
                .all(|c| matches!(*c, ' ' | '\0'))
        })
    }

    /// **The block of rows a pane's or a card's entry is drawn on, and its stop** — from the
    /// entry's first row (the stop the hit map records) to the next entry's, or to the first blank
    /// row or line of key hints, whichever is sooner, so a two-line entry (a subagent: its task and its facts) is one
    /// block and the pane's footer under the last entry is none of it. `None` for a row in no
    /// entry. What lights up under the pointer, and what a click there selects.
    pub(crate) fn stop_block(&self, hit: &Hit, y: usize) -> Option<(usize, Vec<usize>)> {
        let mut stops = hit.stops.clone();
        stops.sort();
        let i = stops.iter().rposition(|(r, _)| *r <= y)?;
        let (first, stop) = stops[i];
        let end = stops
            .get(i + 1)
            .map(|(r, _)| *r)
            .unwrap_or(hit.rect.y + hit.rect.h);
        let mut rows = vec![first];
        for r in first + 1..end {
            // A blank row ends the entry, and so does a line of key hints: a card's keys line
            // sits right under its last value, and it is the card's, not that value's.
            let hints = self.hint_rows.get(r).is_some_and(|h| !h.hints.is_empty());
            if self.blank_row(r) || hints {
                break;
            }
            rows.push(r);
        }
        rows.contains(&y).then_some((stop, rows))
    }

    /// **The pointer moved with no button held.** Only where it is is kept; what it is over is
    /// worked out at once (so the pointer's shape follows the motion) and again on every frame.
    pub(crate) fn hover_at(&mut self, x: u16, y: u16) {
        self.pointer_at = Some((x, y));
        self.relight();
    }

    /// **What the pointer lights, from the frame that is drawn now.**
    ///
    /// The light used to be kept as the rows the pointer was over when it MOVED, and every way
    /// the screen changes under a still pointer left it on whatever came next: a click that put
    /// the diff popup up lit the popup's text, a click that opened a run lit the reasoning it
    /// opened — the operator: *"i suggest you to refactor it properly so it isnot adhoc
    /// card-by-card click-by-click"*. So nothing is kept but the position, and the light is
    /// derived from this frame's click targets (`hits`, the key hints, the conversation's maps)
    /// each time it is drawn: what it lights is always what a click at the pointer would act on
    /// now. A drag or a selection lights nothing.
    pub(crate) fn relight(&mut self) {
        let over = match self.pointer_at {
            Some((x, y)) if self.drag.from.is_none() && self.selection.is_none() => {
                self.clickable_at(x, y)
            }
            _ => None,
        };
        if over.is_some() != self.hovered.is_some() {
            self.pointer_hand = Some(over.is_some());
        }
        self.hovered = over;
    }

    /// **The left button went down**: the start of a click or of a selection, and the end of
    /// any selection already drawn.
    pub(crate) fn press_at(&mut self, x: u16, y: u16) {
        self.drag = Drag {
            from: Some((x, y)),
            to: None,
        };
        self.drop_selection();
    }

    /// Motion with the left button held: the selection runs from the press to here.
    ///
    /// **And the view is held from its first motion** — ctrl-p's hold, by itself: the operator,
    /// 2026-10-12, *"can we auto trigger ctrl-p once selection starts?"*. A turn streaming in
    /// under a selection moves the text it was made over; held, the frame stays put while the
    /// events keep folding. Not when the operator already held it: that hold is theirs, and ends
    /// when they say so.
    pub(crate) fn drag_to(&mut self, x: u16, y: u16) {
        if self.drag.from.is_some() {
            if self.selection.is_none() && !self.hold {
                let _ = self.toggle_hold();
                self.selection_held = true;
            }
            self.drag.to = Some((x, y));
            self.selection = self.drag.from.map(|f| (f, (x, y)));
            self.hovered = None;
        }
    }

    /// **The selection goes, and the hold it took with it** — Esc, or a press elsewhere. A hold
    /// the operator took with ctrl-p is theirs and stays.
    pub(crate) fn drop_selection(&mut self) -> bool {
        let had = self.selection.take().is_some();
        if std::mem::take(&mut self.selection_held) && self.hold {
            let _ = self.toggle_hold();
        }
        had
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
        self.say(&format!(
            "copied {n} character(s){}",
            if self.selection_held {
                " — esc lets the view follow again"
            } else {
                ""
            }
        ));
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
        if let Some(h) = &self.hovered {
            for (y, cols) in &h.rows {
                let Some(row) = frame.get_mut(*y) else {
                    continue;
                };
                let cols = cols.or_else(|| text_span(&keyhints::cells(row)));
                if let Some((a, b)) = cols {
                    *row = brighten(row, a, b);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_brightened_run_puts_the_rows_own_style_back_where_it_ends() {
        // A faint hint: brightened through its own reset, and the separator after it faint again.
        let row = "\x1b[2mesc closes\x1b[0m\x1b[2m · tab\x1b[0m";
        let b = brighten(row, 0, 10);
        assert!(b.starts_with("\x1b[2m\x1b[22;1mesc closes"), "{b:?}");
        let sep = b.find(" · tab").expect("the separator");
        assert!(
            b[..sep].ends_with("\x1b[0m\x1b[2m"),
            "the separator after the mark is not drawn in the row's own style: {b:?}"
        );
        // A run that ends mid-span restores the span's style.
        let b = brighten("\x1b[38;5;8mabcdef", 1, 3);
        assert!(b.contains("bc\x1b[0m\x1b[38;5;8md"), "{b:?}");
    }

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
