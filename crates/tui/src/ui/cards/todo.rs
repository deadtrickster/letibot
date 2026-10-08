//! **The todo card**: writing a todo, field by field.

use crate::app::*;
use crate::ui::render::row_strings;
use rano::agent::todos::{TodoCard, TodoField as Field};

impl App {
    /// The line the screen shows while `mode_confirm` is set. Spells out the three
    /// classes the point stops asking about, because "are you sure" is a question
    /// nobody can answer.
    /// **The new-todo card**: the three fields, which one is being typed, and the three keys.
    ///
    /// leticl's `todo-card-lines`, and the shape is deliberate — *"the card is the modal and the
    /// composer is the field"*, which is this head's one text widget, so all three fields are
    /// edited with every key the operator already has. **The third is not a text box with no
    /// label**: `when` takes a job handle, and the two sentences under the fields say what the row
    /// then waits on, because *a handle* is not something a reader can guess at.
    ///
    /// **The last line says whose the row will be**, because that is the whole difference the
    /// feature turns on and the place a reader will look for it: an item added here is the
    /// OPERATOR's, the model is shown it and reminded of it, and the model cannot remove it.
    pub(crate) fn todo_card_lines(&self, w: usize) -> Vec<String> {
        let Some(draft) = &self.todo_draft else {
            return Vec::new();
        };
        // The field with the keyboard shows what is being typed into the composer, not what
        // was last stored for it.
        let live = self.input();
        let card = TodoCard {
            title: draft.shown(&live, TodoField::Title),
            detail: draft.shown(&live, TodoField::Detail),
            when: draft.shown(&live, TodoField::When),
            focus: match draft.focus {
                TodoField::Title => Field::Title,
                TodoField::Detail => Field::Detail,
                TodoField::When => Field::When,
            },
        };
        row_strings(&card.lines(w), self.cfg.palette())
    }
}
