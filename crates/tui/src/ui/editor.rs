//! **The editor pane, drawn** — rano's own renderer into the conversation's rectangle — and the
//! map from the conversation's rows to the files they are about.

use crate::app::*;
use crate::ui::render::{row, visible_width};
use rano::render::{Line, Rect, Span};
use rano::style::Role;
use std::path::Path;
use std::time::Instant;

/// A cell coordinate as rano counts them. A terminal wider or taller than `u16` does not
/// exist; the clamp is what keeps a nonsense size from wrapping round to a small one.
fn cells(n: usize) -> u16 {
    n.min(u16::MAX as usize) as u16
}

/// **Where rano draws inside the pane's frame**: under the top edge, over the bottom one, and
/// two columns in — the diff popup's own indent, so the two read as one kind of thing. The one
/// rectangle both the drawing and the mouse routing use (`App::edit_area`).
pub(crate) fn editor_inner(x: usize, y: usize, w: usize, room: usize) -> Rect {
    Rect::new(
        cells(x + 2),
        cells(y + 1),
        cells(w.saturating_sub(2)),
        cells(room.saturating_sub(2)),
    )
}

/// **The pane's top edge**: the file, and what a reader must know before typing into it —
/// unsaved edits, or that it cannot be written. A buffer with no file says so.
///
/// The name is cut from the LEFT when the edge is short — `…/dir/notes.txt` — so the file's own
/// name and the state after it are what survive, never the head of a long path.
fn editor_title(name: &str, t: &rano::editor::FrameTitle, w: usize) -> Line {
    let room = w.saturating_sub(24).max(12);
    let shown = if visible_width(name) > room {
        let tail: String = name
            .chars()
            .rev()
            .take(room.saturating_sub(1))
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!("…{tail}")
    } else {
        name.to_string()
    };
    let mut spans = if name.is_empty() {
        vec![Span::role("no file", Role::Faint)]
    } else {
        vec![Span::role(shown, Role::Strong)]
    };
    if t.read_only {
        spans.push(Span::raw(" · view only"));
    } else if t.modified {
        spans.push(Span::role(" · modified", Role::Pending));
    }
    Line::new(spans)
}

/// **The pane's bottom edge: rano's keys, in the head's words** — `C-o write out · C-f where
/// is · …`, faint like every other key line here, as many as the edge holds. What rano's
/// function bar would have said, in the order it would have said it.
fn editor_keys(keys: &[(String, String)]) -> Line {
    let words: Vec<String> = keys
        .iter()
        .map(|(k, l)| format!("{k} {}", l.trim_end_matches('…').to_lowercase()))
        .collect();
    Line::new(vec![Span::role(words.join(" · "), Role::Faint)])
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
        // The name the edge shows: relative to the session's workspace, as every other path this
        // head draws, keeping rano's `[i/n] ` in front of it with several buffers open.
        let name = match self.edit_pane.as_ref() {
            Some(p) => {
                let full = p.ed.frame_title().name;
                let (count, path) = match full.split_once("] ") {
                    Some((n, rest)) if full.starts_with('[') => {
                        (format!("{n}] "), rest.to_string())
                    }
                    _ => (String::new(), full),
                };
                if path.is_empty() {
                    String::new()
                } else {
                    format!("{count}{}", self.workspace_relative(Path::new(&path)))
                }
            }
            None => return Vec::new(),
        };
        let Some(p) = self.edit_pane.as_mut() else {
            return Vec::new();
        };
        let rect = editor_inner(x, y, w, room);
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
            // **The frame**: the popup's two edges around rano's text, each row of the text
            // indented by the columns `editor_inner` left.
            let mut rows = Vec::with_capacity(room);
            rows.push(row(
                &rano::agent::composer::box_edge(
                    w,
                    '╭',
                    '╮',
                    &editor_title(&name, &p.ed.frame_title(), w),
                    &Line::default(),
                ),
                palette,
            ));
            rows.extend(p.buf.emit(palette).into_iter().map(|r| format!("  {r}")));
            rows.truncate(room.saturating_sub(1));
            while rows.len() + 1 < room {
                rows.push(String::new());
            }
            rows.push(row(
                &rano::agent::composer::box_edge(
                    w,
                    '╰',
                    '╯',
                    &editor_keys(&p.ed.key_hints()),
                    &Line::default(),
                ),
                palette,
            ));
            p.rows = rows;
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
