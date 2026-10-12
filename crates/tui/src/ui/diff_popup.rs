//! **The diff popup, drawn**: an edge naming the file and the change, the whole file as a diff,
//! and an edge with the popup's own keys. See `app::diff_popup`.

use crate::app::diff_popup::PopupBody;
use crate::app::*;
use crate::ui::render::path_span;
use crate::ui::rows::row;
use rano::render::{Line, Span, Style};
use rano::style::Role;

impl App {
    /// The popup in the conversation's rectangle, `room` rows of `w` columns exactly.
    pub(crate) fn diff_popup_rows(&mut self, w: usize, room: usize) -> Vec<String> {
        let palette = self.cfg.palette();
        let Some(what) = self.diff_popup.as_ref().map(|p| p.body.clone()) else {
            return Vec::new();
        };
        let split = self.diff_split;
        // The body is two columns in from each side, so the rows read as inside the box. No
        // floor wider than the box (R90.31): `max(20)` overflowed every row below ~24 columns.
        let inner = w.saturating_sub(4).max(1);
        // **With a decision open the card has ↑↓ and Home/End** (`key_diff_popup` yields them),
        // so the edge offers only the keys that still scroll the popup (R90.28).
        let deciding = !self.open.is_empty();
        let stale = self
            .diff_popup
            .as_ref()
            .and_then(|p| p.laid.as_ref())
            .is_none_or(|l| l.width != inner || l.split != split);
        if stale {
            let laid = match &what {
                PopupBody::Change(file) => self.diff_popup_layout(file, inner),
                PopupBody::Notice { text, jobs, answer } => {
                    self.notice_popup_layout(text, jobs, answer.as_deref(), inner)
                }
            };
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
        let (title, keys) = match &what {
            PopupBody::Change(file) => (
                Line::new(vec![
                    path_span(
                        self.cfg.links.as_deref(),
                        &file.path,
                        Style::of(Role::Strong),
                    ),
                    Span::raw(format!(" · the change at line {}", file.line)),
                ]),
                if deciding {
                    "esc closes · pgup/pgdn · ctrl-] edits it"
                } else {
                    "esc closes · ↑↓ pgup/pgdn home/end · ctrl-] edits it"
                },
            ),
            // **The notice names its jobs on the edge**, the way the change names its file.
            // **A card names what it is about** — a red target's the target and the tip, in the
            // alarm's register; a review's the branch and the verdict, in the verdict's
            // (`line_cards`).
            PopupBody::Notice { jobs, text, .. } if jobs.is_empty() => {
                let (title, role) = crate::ui::transcript::line_cards::popup_title(text)
                    .unwrap_or_else(|| ("notice".to_string(), Role::Failure));
                (
                    Line::new(vec![Span::styled(title, Style::of(role))]),
                    if deciding {
                        "esc closes · pgup/pgdn"
                    } else {
                        "esc closes · ↑↓ pgup/pgdn home/end"
                    },
                )
            }
            PopupBody::Notice { jobs, .. } => (
                Line::new(vec![
                    Span::styled(
                        jobs.iter()
                            .map(|(_, j)| j.id.as_str())
                            .collect::<Vec<_>>()
                            .join(" · "),
                        Style::of(Role::Strong),
                    ),
                    Span::raw(if jobs.len() == 1 {
                        " · a job ended"
                    } else {
                        " · jobs ended"
                    }),
                ]),
                if deciding {
                    "esc closes · pgup/pgdn"
                } else {
                    "esc closes · ↑↓ pgup/pgdn home/end"
                },
            ),
        };
        let keys = Line::new(vec![Span::raw(keys)]);
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
