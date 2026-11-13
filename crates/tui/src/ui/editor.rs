//! The editor pane, drawn - rano's own renderer into the conversation's rectangle - and the
//! map from the conversation's rows to the files they are about.

use crate::app::*;
use crate::ui::render::{row, visible_width};
use rano::render::{Line, Rect, Span};
use rano::style::Role;
use std::time::Instant;

/// A cell coordinate as rano counts them. A terminal wider or taller than `u16` does not
/// exist; the clamp is what keeps a nonsense size from wrapping round to a small one.
fn cells(n: usize) -> u16 {
    n.min(u16::MAX as usize) as u16
}

/// Where rano draws inside the pane's frame: under the top edge, over the bottom one, and
/// two columns in from each side - the diff popup's own indent, so the two read as one kind of
/// thing. The one rectangle both the drawing and the mouse routing use (`App::edit_area`).
pub(crate) fn editor_inner(x: usize, y: usize, w: usize, room: usize) -> Rect {
    Rect::new(
        cells(x + 2),
        cells(y + 1),
        // Two columns off EACH side, not the left alone: the frame's corners are its first and
        // last columns, so `w - 2` put rano's last column on the right edge - a long line ran to
        // the corner and `Ln/Col` sat on it.
        cells(w.saturating_sub(4)),
        cells(room.saturating_sub(2)),
    )
}

/// The pane's top edge: the file, and what a reader must know before typing into it -
/// unsaved edits, or that it cannot be written. The name is cut from the LEFT when the
/// edge is short, so the file's own name and the state after it survive, not the path head.
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

/// rano's live key hints as one line of words - `ctrl-o write out · ctrl-f where is · …`. The
/// head's hint row carries them, so the pane's frame draws a plain bottom edge: one row says what
/// the keys do, whichever of the two halves wrote it.
pub(crate) fn pane_keys(keys: &[(String, String)]) -> String {
    keys.iter()
        .map(|(k, l)| format!("{k} {}", l.trim_end_matches('…').to_lowercase()))
        .collect::<Vec<_>>()
        .join(" · ")
}

/// A drawn edge in the focus marker's register: the frame's own glyphs take the line's style, and
/// the legends inlaid in it keep theirs.
fn edge(mut l: Line, style: &rano::render::Style) -> Line {
    l.style = style.clone();
    l
}

impl App {
    /// The pane's rows for this frame, at column `x` and row `y` - where rano is told it is,
    /// so its mouse mapping and caret land in the terminal's own coordinates. A frame where
    /// nothing changed serves the last rows rather than drawing them twice.
    pub(crate) fn editor_rows(&mut self, x: usize, y: usize, w: usize, room: usize) -> Vec<String> {
        let palette = self.cfg.palette();
        // The focus marker's register for this frame: the pane's edges are `Strong` while it has
        // the keyboard and `Faint` while the composer does.
        let frame = rano::render::Style::of(self.editor_frame());
        let Some(name) = self.editor_name() else {
            return Vec::new();
        };
        let Some(p) = self.edit_pane.as_mut() else {
            return Vec::new();
        };
        let rect = editor_inner(x, y, w, room);
        // A new size is laid out before it is drawn: rano's scroll and its diff view's lines
        // are measured against the area, and `tick` is where it measures them.
        if p.set_area(rect.into()) {
            p.ed.tick(Instant::now());
        }
        if p.dirty || p.palette != Some(palette) || p.rows.len() != room {
            // The operator's rano theme, lent to this pane's draw and its emit and to nothing else:
            // the conversation beside it stays under the head's palette.
            let theme = p.theme.clone();
            let (caret, rows) = rano::theme::scoped(theme, || {
                if p.buf.area() != rect {
                    p.buf.resize(rect);
                }
                // rano says `(column, row)`; the head's caret is `(row, column)`.
                let caret = rano::ui::draw_in(&mut p.buf, rect, &p.ed)
                    .map(|(col, row)| (row as usize, col as usize));
                // The frame: the popup's two edges around rano's text, each row of the text
                // indented by the columns `editor_inner` left.
                let mut rows = Vec::with_capacity(room);
                rows.push(row(
                    &edge(
                        rano::agent::composer::box_edge(
                            w,
                            '╭',
                            '╮',
                            &editor_title(&name, &p.ed.frame_title(), w),
                            &Line::default(),
                        ),
                        &frame,
                    ),
                    palette,
                ));
                rows.extend(p.buf.emit(palette).into_iter().map(|r| format!("  {r}")));
                rows.truncate(room.saturating_sub(1));
                while rows.len() + 1 < room {
                    rows.push(String::new());
                }
                // A plain edge: rano's keys are on the head's hint row, so this row says nothing
                // rather than saying it twice.
                rows.push(row(
                    &edge(
                        rano::agent::composer::box_edge(
                            w,
                            '╰',
                            '╯',
                            &Line::default(),
                            &Line::default(),
                        ),
                        &frame,
                    ),
                    palette,
                ));
                (caret, rows)
            });
            p.caret = caret;
            p.rows = rows;
            p.dirty = false;
            p.palette = Some(palette);
        }
        let rows = p.rows.clone();
        self.forward_editor_graphics();
        rows
    }

    /// The places among lines `start..end` that belong to a finished edit or write row whose
    /// change this head holds, as `(row of the window, place)`. In line order (the backward
    /// fill prepends and shifts), so each line's row is a binary search away.
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
            } else if !self.items[sp.row].citations.is_empty() {
                // A cited line opens its file there - the header at the range's first line,
                // each numbered line at its own. Read off the drawn row, so the line clicked is
                // the line named.
                if let Some(f) = self.cited_line(sp.at, line) {
                    out.push((line - start, f));
                }
            }
        }
        out
    }

    /// The file and line a drawn line of a citation names, walking up from `line` to the
    /// header of the block it is in - `None` for a line outside any citation (the reply's prose).
    fn cited_line(&self, row_at: usize, line: usize) -> Option<FileRef> {
        let plain = |l: usize| -> String {
            crate::app::keyhints::cells(&self.hist_lines[l])
                .into_iter()
                .collect::<String>()
        };
        let here = plain(line);
        let body = here.trim_start();
        let mut at = None;
        if let Some(label) = body.strip_prefix("┌─ ") {
            let (path, first) = rano::markdown::citation(label)?;
            return Some(FileRef {
                path: path.to_string(),
                line: first as usize,
                change: None,
            });
        }
        if let Some(rest) = body.strip_prefix("│ ") {
            at = rest.split_whitespace().next()?.parse::<usize>().ok();
        }
        let n = at?;
        // The header above names the file; the walk stops at the row's top or another block's end.
        for up in (row_at..line).rev() {
            let l = plain(up);
            let b = l.trim_start();
            if b.starts_with("└─") {
                return None;
            }
            if let Some(label) = b.strip_prefix("┌─ ") {
                let (path, _) = rano::markdown::citation(label)?;
                return Some(FileRef {
                    path: path.to_string(),
                    line: n,
                    change: None,
                });
            }
        }
        None
    }
}
