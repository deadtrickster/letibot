//! The `/rooms` view's drawing.

use crate::app::*;
use rano::agent::pane::{self, PaneLines};

impl App {
    pub(crate) fn rooms_lines(&mut self, _w: usize, room: usize) -> Vec<String> {
        let mut out = PaneLines::new();
        out.push(pane::title("rooms"));
        self.pane_len = out.lines.len();
        self.pane_room = room;
        crate::ui::render::row_strings(&out.lines, self.cfg.palette())
    }
}
