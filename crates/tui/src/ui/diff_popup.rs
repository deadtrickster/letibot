//! **The diff popup, drawn**: an edge naming the file and the change, the whole file as a diff,
//! and an edge with the popup's own keys. See `app::diff_popup`.

use crate::app::*;
use crate::ui::render::path_span;
use crate::ui::rows::row;
use rano::render::{Line, Span, Style};
use rano::style::Role;

impl App {
    /// The popup in the conversation's rectangle, `room` rows of `w` columns exactly.
    pub(crate) fn diff_popup_rows(&mut self, w: usize, room: usize) -> Vec<String> {
        let palette = self.cfg.palette();
        let Some(file) = self.diff_popup.as_ref().map(|p| p.file.clone()) else {
            return Vec::new();
        };
        let split = self.diff_split;
        // The body is two columns in from each side, so the rows read as inside the box.
        let inner = w.saturating_sub(4).max(20);
        let stale = self
            .diff_popup
            .as_ref()
            .and_then(|p| p.laid.as_ref())
            .is_none_or(|l| l.width != inner || l.split != split);
        if stale {
            let laid = self.diff_popup_layout(&file, inner);
            if let Some(p) = self.diff_popup.as_mut() {
                p.laid = Some(laid);
            }
        }
        let p = self.diff_popup.as_mut().expect("checked above");
        let laid = p.laid.as_ref().expect("laid out above");
        let note_rows = usize::from(laid.note.is_some());
        let body = room.saturating_sub(2 + note_rows);
        p.room = body;
        let max = laid.rows.len().saturating_sub(body);
        // **On the first change, with a little of what comes before it**: the reader opened the
        // popup to see the change, and two lines above it say where in the file they are.
        if !p.placed {
            p.scroll = laid.first.saturating_sub(2).min(max);
            p.placed = true;
        }
        p.scroll = p.scroll.min(max);
        // **The file's path, as a link the terminal opens** (OSC 8) where it speaks it: this
        // edge names the file the change is in, and a printed path that cannot be clicked is
        // the operator's own report. `Style::of(Role::Strong)` is the register the path was
        // drawn in, and the span is that same span when the terminal has no links.
        let title = Line::new(vec![
            path_span(
                self.cfg.links.as_deref(),
                &file.path,
                Style::of(Role::Strong),
            ),
            Span::raw(format!(" · the change at line {}", file.line)),
        ]);
        let keys = Line::new(vec![Span::raw(
            "esc closes · ↑↓ PgUp PgDn Home End · ctrl-] edits it",
        )]);
        let mut out = Vec::with_capacity(room);
        out.push(row(
            &rano::agent::composer::box_edge(w, '╭', '╮', &title, &Line::default()),
            palette,
        ));
        if let Some(note) = &laid.note {
            out.push(row(
                &rano::agent::text::one(format!("  {note}"), Role::Pending),
                palette,
            ));
        }
        for l in laid.rows.iter().skip(p.scroll).take(body) {
            let mut l = l.clone();
            l.spans.insert(0, Span::raw("  "));
            out.push(row(&l, palette));
        }
        while out.len() + 1 < room {
            out.push(String::new());
        }
        out.push(row(
            &rano::agent::composer::box_edge(w, '╰', '╯', &keys, &Line::default()),
            palette,
        ));
        out.truncate(room);
        out
    }
}
