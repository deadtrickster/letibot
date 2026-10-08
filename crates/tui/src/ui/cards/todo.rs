//! **The todo card**: writing a todo, field by field.

use crate::app::*;
use crate::ui::render::{sgr, trim_to};
use crate::ui::*;
use letibot_ui::style::Role;
use letibot_ui::text::without_control_lines;

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
        let p = self.cfg.palette();
        // The field under the cursor is drawn from the COMPOSER, the other two from the draft — so
        // the row being typed is never a keystroke behind. leticl's `%todo-draft-focus` for the same
        // reason.
        let live = self.input();
        let field = |key: &str, which: TodoField, empty: &str| {
            let head = dim(&self.cfg, &format!("  {key:<7} "));
            let value = draft.shown(&live, which);
            let body = if value.is_empty() {
                dim(&self.cfg, empty)
            } else if draft.focus == which {
                p.paint(Role::Strong, &without_control_lines(&value))
            } else {
                p.paint(Role::Faint, &without_control_lines(&value))
            };
            format!("{head}{body}")
        };
        let mut out = vec![colour(&self.cfg, sgr::BOLD, "adding a todo item")];
        out.push(String::new());
        out.push(field("title", TodoField::Title, "(empty)"));
        out.push(field("detail", TodoField::Detail, "(empty)"));
        // **The field nobody knows**, so it says what it wants rather than `(empty)`: a handle, and
        // the sentence under the fields says what a handle DOES.
        out.push(field("when", TodoField::When, "(waits on nothing)"));
        out.push(String::new());
        for (k, why) in [
            ("tab", "moves between the fields"),
            ("enter", "adds it to the session's plan, marked as yours"),
            ("esc", "cancels, and adds nothing"),
        ] {
            out.push(format!(
                "{}{}",
                dim(&self.cfg, &format!("  {k:<7}")),
                p.paint(Role::Plain, why)
            ));
        }
        out.push(String::new());
        out.push(dim(
            &self.cfg,
            "  `when` is a JOB handle: the row is filed now and comes up again when that job is \
             not running. A job this daemon has never heard of counts as ended, which is what a \
             restart looks like.",
        ));
        out.push(dim(
            &self.cfg,
            "  the model sees these and is reminded of them; it can mark one done, and cannot \
             remove yours",
        ));
        out.into_iter().map(|l| trim_to(&l, w)).collect()
    }
}
