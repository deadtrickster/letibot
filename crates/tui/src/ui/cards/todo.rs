//! The todo card: writing a todo, field by field.

use crate::app::*;
use crate::ui::render::row_strings;
use rano::agent::todos::{TodoCard, TodoField as Field};

impl App {
    /// The new-todo card: the three fields, which one is being typed, and the three keys.
    /// `when` takes a job handle, so the two sentences under the fields say what the row then
    /// waits on; the last line says whose the row will be, because the model cannot remove it.
    pub(crate) fn todo_card_lines(&self, w: usize) -> Vec<String> {
        let Some(draft) = &self.todo_draft else {
            return Vec::new();
        };
        // The field with the keyboard shows what is being typed into the composer, not what
        // was last stored for it.
        let live = self.input();
        let card = TodoCard {
            title: draft.shown(live, TodoField::Title),
            detail: draft.shown(live, TodoField::Detail),
            when: draft.shown(live, TodoField::When),
            focus: match draft.focus {
                TodoField::Title => Field::Title,
                TodoField::Detail => Field::Detail,
                TodoField::When => Field::When,
            },
        };
        row_strings(&card.lines(w), self.cfg.palette())
    }
}
