//! The rooms sidebar: the Rooms and Sessions list kept beside the transcript. Off by default
//! (`[ui] rooms_sidebar`), toggled with alt-l or `/rooms sidebar on|off`, entered with alt-j and
//! left with Esc. Below `MAIN_MIN` columns for the transcript it collapses to one strip row that
//! says so.

use std::ops::ControlFlow;

use crate::app::rooms::{Chan, ListRow};
use crate::app::*;

/// The columns the transcript keeps before the sidebar gives way.
pub(crate) const MAIN_MIN: usize = 72;
/// The narrowest sidebar worth drawing.
pub(crate) const SIDE_MIN: usize = 18;
/// Columns one resize key moves the border.
const RESIZE_STEP: usize = 4;

#[derive(Debug, Default)]
pub(crate) struct Sidebar {
    pub on: bool,
    pub focus: bool,
    /// Set by the frame: the sidebar is drawn beside this frame's `/rooms` view.
    pub drawn: bool,
    /// The width the operator chose (drag or keys), kept in `head.toml`.
    pub width: Option<usize>,
    /// The configured starting width, `[ui] rooms_sidebar_width`.
    pub default_width: Option<usize>,
    /// The border is held: motion resizes instead of selecting.
    pub dragging: bool,
    /// The terminal width of the last frame and the sidebar's width in it.
    pub term_w: usize,
    pub last_w: usize,
    /// The rows of the last frame, by screen row.
    pub rows: Vec<(usize, ListRow)>,
}

/// How the sidebar sits in a frame of `term_w` columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fit {
    Off,
    /// Drawn, this many columns wide.
    Wide(usize),
    /// Switched on and no room for it.
    Collapsed,
}

impl App {
    pub(crate) fn sidebar_fit(&self, term_w: usize) -> Fit {
        if !self.sidebar.on {
            return Fit::Off;
        }
        if term_w < MAIN_MIN + 1 + SIDE_MIN {
            return Fit::Collapsed;
        }
        // A third of the terminal unless chosen or configured, never so wide that the transcript
        // drops under its minimum.
        let want = self
            .sidebar
            .width
            .or(self.sidebar.default_width)
            .unwrap_or(term_w / 3);
        Fit::Wide(want.clamp(SIDE_MIN, term_w - MAIN_MIN - 1))
    }

    /// The frame: the editor's column on the left when it shares the terminal, then the
    /// conversation's, then the sidebar beside that. The editor takes its columns first, because
    /// it is the one of the three that cannot collapse - a pane too narrow is zoomed instead
    /// ([`EditorFit`]).
    pub(crate) fn compose_framed(&mut self, term_w: usize, h: usize) -> Vec<String> {
        self.editor_term_w = term_w;
        let edw = self.editor_fit(term_w).columns();
        self.editor_col = edw;
        if edw == 0 {
            return self.compose_side(term_w, h);
        }
        let off = edw + 1;
        let mut out = self.compose_side(term_w - off, h);
        let ed = self.editor_rows(0, 0, edw, h);
        // rano's rectangle in the terminal's own cells: the frame just composed wrote the
        // MAIN column's into `edit_area`, and the pane is not there.
        self.edit_area = crate::ui::editor::editor_inner(0, 0, edw, h).into();
        for (i, row) in out.iter_mut().enumerate() {
            let mut line = ed.get(i).cloned().unwrap_or_default();
            let pad = edw.saturating_sub(crate::ui::render::visible_width(&line));
            line.push_str(&" ".repeat(pad));
            line.push('│');
            line.push_str(row);
            *row = line;
        }
        // Every x the main column recorded was measured from its own left edge, and the termi-
        // nal's counts from the frame's: the hits and the caret move by the editor's share.
        for hit in &mut self.hits {
            hit.rect.x += off;
        }
        // The caret last, and rano's read again rather than as the main column left it: the
        // pane's rows were drawn above, and a caret taken from before them would be a key behind.
        if self.editor_focused() {
            self.cursor = self.edit_pane.as_ref().and_then(|p| p.caret);
        } else if let Some(c) = self.cursor.as_mut() {
            c.1 += off;
        }
        // The border is the resize handle, pushed last so it is over the column's own target - a
        // click on it starts a drag rather than taking the keyboard. The column is the pane's:
        // anywhere on it that rano did not claim puts the keyboard back in the editor.
        self.hits.push(Hit {
            rect: Rect {
                x: 0,
                y: 0,
                w: off,
                h: out.len(),
            },
            target: Target::EditorColumn,
            stops: Vec::new(),
        });
        self.hits.push(Hit {
            rect: Rect {
                x: edw,
                y: 0,
                w: 1,
                h: out.len(),
            },
            target: Target::EditorBorder,
            stops: Vec::new(),
        });
        out
    }

    /// The conversation's frame with the rooms sidebar beside it, or a strip row that says it is
    /// collapsed.
    pub(crate) fn compose_side(&mut self, term_w: usize, h: usize) -> Vec<String> {
        let fit = self.sidebar_fit(term_w);
        self.sidebar.drawn = matches!(fit, Fit::Wide(_));
        self.sidebar.term_w = term_w;
        self.sidebar.last_w = match fit {
            Fit::Wide(w) => w,
            _ => 0,
        };
        match fit {
            Fit::Off => {
                self.sidebar.rows.clear();
                self.compose_screen(term_w, h)
            }
            Fit::Collapsed => {
                self.sidebar.rows.clear();
                self.sidebar.focus = false;
                let unread = self.rooms.unread_total();
                let strip = format!(
                    "Rooms sidebar collapsed: needs {} columns, this is {term_w}{}",
                    MAIN_MIN + 1 + SIDE_MIN,
                    if unread > 0 {
                        format!(" · {unread} unread")
                    } else {
                        String::new()
                    }
                );
                let mut out = vec![
                    crate::ui::render::row_strings(
                        &[rano::agent::pane::faint(crate::ui::render::trim_to(
                            &strip, term_w,
                        ))],
                        self.cfg.palette(),
                    )
                    .remove(0),
                ];
                out.extend(self.compose_screen(term_w, h.saturating_sub(1).max(1)));
                // Everything below the strip moved down one row.
                if let Some(c) = self.cursor.as_mut() {
                    c.0 += 1;
                }
                for hit in &mut self.hits {
                    hit.rect.y += 1;
                    for s in &mut hit.stops {
                        s.0 += 1;
                    }
                }
                out
            }
            Fit::Wide(sbw) => {
                let main_w = term_w - sbw - 1;
                let mut out = self.compose_screen(main_w, h);
                let side = self.sidebar_lines(sbw, out.len());
                for (i, row) in out.iter_mut().enumerate() {
                    let pad = main_w.saturating_sub(crate::ui::render::visible_width(row));
                    row.push_str(&" ".repeat(pad));
                    row.push('│');
                    if let Some(s) = side.get(i) {
                        row.push_str(s);
                    }
                }
                let x = main_w + 1;
                // The border column is the resize handle: a press on it starts a drag.
                self.hits.push(Hit {
                    rect: Rect {
                        x: main_w,
                        y: 0,
                        w: 1,
                        h: out.len(),
                    },
                    target: Target::SideBorder,
                    stops: Vec::new(),
                });
                for (k, (r, row)) in self.sidebar.rows.clone().into_iter().enumerate() {
                    // The checkbox is the last three cells of its line, a target of its own.
                    let (hx, hw) = match row {
                        ListRow::Mute(_) => (x + sbw - 3, 3),
                        _ => (x, sbw),
                    };
                    self.hits.push(Hit {
                        rect: Rect {
                            x: hx,
                            y: r,
                            w: hw,
                            h: 1,
                        },
                        target: Target::SideRow,
                        stops: vec![(r, k)],
                    });
                }
                out
            }
        }
    }

    /// Switch the sidebar on or off. Off takes the focus with it.
    pub(crate) fn sidebar_set(&mut self, on: bool) {
        self.sidebar.on = on;
        if !on {
            self.sidebar.focus = false;
        }
    }

    /// `/rooms sidebar on|off|toggle`.
    pub(crate) fn sidebar_command(&mut self, arg: &str) {
        match arg.trim() {
            "on" => self.sidebar_set(true),
            "off" => self.sidebar_set(false),
            "" | "toggle" => self.sidebar_set(!self.sidebar.on),
            other => {
                self.say(&format!("/rooms sidebar on|off - not `{other}`"));
                return;
            }
        }
        self.say(if self.sidebar.on {
            "rooms sidebar on - alt-j enters it, esc leaves it"
        } else {
            "rooms sidebar off"
        });
    }

    /// Set the width and keep it: clamped to the minimums, written to `head.toml`.
    pub(crate) fn sidebar_set_width(&mut self, want: usize) {
        let t = self.sidebar.term_w;
        if t < MAIN_MIN + 1 + SIDE_MIN {
            return;
        }
        self.sidebar.width = Some(want.clamp(SIDE_MIN, t - MAIN_MIN - 1));
    }

    /// alt-< and alt->: narrower and wider by a few columns.
    pub(crate) fn sidebar_resize_key(&mut self, wider: bool) {
        if !self.sidebar.on || self.sidebar.last_w == 0 {
            return;
        }
        let w = self.sidebar.last_w;
        self.sidebar_set_width(if wider {
            w + RESIZE_STEP
        } else {
            w.saturating_sub(RESIZE_STEP)
        });
        let _ = self.save_prefs(RetiredWrite::Union);
    }

    /// A press on column `x`: the border starts a resize. `true` when it was taken. The x is the
    /// terminal's, and the sidebar's own arithmetic is measured from the conversation's left edge.
    pub(crate) fn sidebar_press(&mut self, x: u16, _y: u16) -> bool {
        let x = usize::from(x).saturating_sub(self.editor_offset());
        if self.sidebar.drawn && x == self.sidebar.term_w.saturating_sub(self.sidebar.last_w + 1) {
            self.sidebar.dragging = true;
            true
        } else {
            false
        }
    }

    /// Motion while the border is held: the sidebar's left edge follows the pointer.
    pub(crate) fn sidebar_drag(&mut self, x: u16) -> bool {
        if !self.sidebar.dragging {
            return false;
        }
        let t = self.sidebar.term_w;
        let x = usize::from(x).saturating_sub(self.editor_offset());
        self.sidebar_set_width(t.saturating_sub(x + 1));
        true
    }

    /// The button came up on a resize: keep the width.
    pub(crate) fn sidebar_release(&mut self) -> bool {
        if !std::mem::take(&mut self.sidebar.dragging) {
            return false;
        }
        let _ = self.save_prefs(RetiredWrite::Union);
        true
    }

    /// alt-l: the sidebar on or off.
    pub(crate) fn sidebar_toggle_key(&mut self) {
        self.sidebar_command("toggle");
    }

    /// alt-j: the keyboard goes into the sidebar.
    pub(crate) fn sidebar_enter_key(&mut self) {
        if !self.sidebar.on {
            self.say("the rooms sidebar is off - alt-l or /rooms sidebar on");
        } else if self.sidebar.rows.is_empty() {
            self.say("the rooms sidebar is collapsed at this width");
        } else {
            self.sidebar.focus = true;
        }
    }

    /// The keys of a focused sidebar: the arrows walk the list, Enter acts on the row, `d` opens
    /// a session's DM, Esc leaves. Anything else is the composer's.
    pub(crate) fn key_sidebar(&mut self, k: &Key) -> ControlFlow<Option<Action>> {
        if !self.sidebar.focus {
            return ControlFlow::Continue(());
        }
        match k {
            Key::Esc => {
                self.sidebar.focus = false;
                // Leaving the list also leaves a room that is open: back to the transcript.
                self.close_surface(Surface::Rooms);
                ControlFlow::Break(None)
            }
            Key::Up | Key::Down => {
                self.rooms_step(matches!(k, Key::Up));
                ControlFlow::Break(None)
            }
            Key::Enter => match self.rooms.selected() {
                Some(c) => ControlFlow::Break(self.sidebar_act(c)),
                None => ControlFlow::Break(None),
            },
            Key::Char('d') if self.editor.text().is_empty() => {
                if let Some(Chan::Session(id)) = self.rooms.selected() {
                    self.sidebar_open_dm(id);
                }
                ControlFlow::Break(None)
            }
            _ => ControlFlow::Continue(()),
        }
    }

    /// Enter or a click on a row: a session is switched to, a room or a DM is entered in `/rooms`.
    pub(crate) fn sidebar_act(&mut self, c: Chan) -> Option<Action> {
        if let Chan::Session(id) = c {
            // The highlight is the list's one: it moves to the session even though the view leaves.
            self.rooms.queue_sel = None;
            self.rooms.sel = Some(Chan::Session(id.clone()));
            return self.rooms_switch_session(id);
        }
        self.sidebar_show(c)
    }

    fn sidebar_show(&mut self, c: Chan) -> Option<Action> {
        let open = if self.is_open(Surface::Rooms) {
            None
        } else {
            self.open_surface(Surface::Rooms)
        };
        self.rooms_go(c);
        open
    }

    fn sidebar_open_dm(&mut self, id: String) {
        let _ = self.sidebar_show(Chan::Dm(id.clone()));
        self.rooms.fresh.insert(id);
    }

    /// A click on row `k` of the sidebar.
    pub(crate) fn sidebar_click(&mut self, k: usize) -> Option<Action> {
        let (_, row) = self.sidebar.rows.get(k).cloned()?;
        self.list_row_act(row)
    }
}
