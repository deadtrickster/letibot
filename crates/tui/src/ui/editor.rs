//! **The editor pane, drawn** — rano's own renderer into the conversation's rectangle — and the
//! map from the conversation's rows to the files they are about.

use crate::app::*;
use rano::render::Rect;
use std::time::Instant;

/// A cell coordinate as rano counts them. A terminal wider or taller than `u16` does not
/// exist; the clamp is what keeps a nonsense size from wrapping round to a small one.
fn cells(n: usize) -> u16 {
    n.min(u16::MAX as usize) as u16
}

impl App {
    /// **The pane's rows for this frame**: `room` rows of `w` columns, drawn at column `x` and
    /// row `y` of the terminal — which is where rano is told it is, so its mouse mapping and its
    /// caret are in the terminal's own coordinates.
    ///
    /// rano draws into a cell buffer the size of the rectangle (`rano::ui::draw_in`), and the
    /// buffer is emitted as this head's row strings under its palette (`Buffer::emit`), so the
    /// pane's colours follow `color` and the light/dark choice like every other row. A frame in
    /// which nothing changed — no key, no tick that said so, no resize — serves the last rows
    /// again rather than drawing them twice.
    pub(crate) fn editor_rows(&mut self, x: usize, y: usize, w: usize, room: usize) -> Vec<String> {
        let palette = self.cfg.palette();
        let Some(p) = self.edit_pane.as_mut() else {
            return Vec::new();
        };
        let rect = Rect::new(cells(x), cells(y), cells(w), cells(room));
        // **A new size is laid out before it is drawn**: rano's scroll and its diff view's lines
        // are measured against the area, and `tick` is where it measures them.
        if p.set_area(rect.into()) {
            p.ed.tick(Instant::now());
        }
        if p.dirty || p.palette != Some(palette) || p.rows.len() != room {
            if p.buf.area() != rect {
                p.buf.resize(rect);
            }
            // rano says `(column, row)`; the head's caret is `(row, column)`.
            p.caret = rano::ui::draw_in(&mut p.buf, rect, &p.ed)
                .map(|(col, row)| (row as usize, col as usize));
            p.rows = p.buf.emit(palette);
            p.dirty = false;
            p.palette = Some(palette);
        }
        p.rows.clone()
    }

    /// **The places among lines `start..end` of the conversation**, as `(row of the window,
    /// place)`: each line of the history that belongs to a finished edit or write row whose
    /// change this head holds. The spans are in line order (the walk appends, the backward fill
    /// prepends and shifts), so each line's row is a binary search away.
    pub(crate) fn file_rows_in(&self, start: usize, end: usize) -> Vec<(usize, FileRef)> {
        let end = end.min(self.hist_lines.len());
        let mut out = Vec::new();
        let mut last: Option<(usize, Option<FileRef>)> = None;
        for line in start..end {
            let i = self.spans.partition_point(|sp| sp.at <= line);
            let Some(sp) = i.checked_sub(1).map(|i| self.spans[i]) else {
                continue;
            };
            if line >= sp.at + sp.lines {
                continue;
            }
            // One lookup per row rather than per line: a diff is many lines of one row.
            let place = match &last {
                Some((row, place)) if *row == sp.row => place.clone(),
                _ => {
                    let place = self.change_of_row(sp.row);
                    last = Some((sp.row, place.clone()));
                    place
                }
            };
            if let Some(f) = place {
                out.push((line - start, f));
            }
        }
        out
    }
}
