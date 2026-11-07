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

#[derive(Debug, Default)]
pub(crate) struct Sidebar {
    pub on: bool,
    pub focus: bool,
    /// Set by the frame: the sidebar is drawn beside this frame's `/rooms` view.
    pub drawn: bool,
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
        let want = self
            .rooms_left_width(term_w * 2 / 3)
            .clamp(SIDE_MIN, (term_w / 3).max(SIDE_MIN));
        if term_w >= MAIN_MIN + 1 + want {
            Fit::Wide(want)
        } else {
            Fit::Collapsed
        }
    }

    /// The frame with the sidebar beside it, or a strip row that says it is collapsed.
    pub(crate) fn compose_framed(&mut self, term_w: usize, h: usize) -> Vec<String> {
        let fit = self.sidebar_fit(term_w);
        self.sidebar.drawn = matches!(fit, Fit::Wide(_));
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
