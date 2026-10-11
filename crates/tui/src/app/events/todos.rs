//! **Events about the plan**: the todo list changed, and a row of the review or merge queue moved.

use super::*;
use letibot_sessionlog::event::SessionEvent;

impl App {
    /// **The plan**: the todo list, and the queues' moves. One family of
    /// [`App::event`]'s arms, moved here verbatim; `event` hands it only these variants.
    pub(crate) fn on_todo_event(&mut self, e: SessionEvent) -> Disposition {
        match e {
            // The model revised its plan. The whole list, not a delta — keep the
            // latest and let the pane show it. Said only when the pane is open:
            // a line in the scrollback for every todo write would bury the work
            // the todos exist to organize, and the pane is where this state
            // lives.
            SessionEvent::TodosUpdated { todos } => {
                self.todos = todos;
                // **AND A WAITING SEED RUNS HERE** — the board has just been read whole, which is
                // the only moment this head may add rows to its own half without risking a wipe:
                // `SetOperatorTodos` REPLACES the operator half, so seeding against a stale list
                // would take rows off the board rather than add to it.
                if self.todo_seed_pending {
                    self.todo_seed_pending = false;
                    self.seed_todos();
                }
                self.redraw = true;
                if self.is_open(Surface::Todos) {
                    Disposition::Rendered
                } else {
                    Disposition::Filtered
                }
            }
            SessionEvent::QueueMoved { .. } => {
                // **A move is a reason to read again, not a row to fold** (R61): the snapshot is
                // the daemon's, and a head that patched its own copy from the events would draw a
                // stale queue after any event it missed. While the pane is open, one re-read is
                // queued (a burst of moves is one read).
                if self.is_open(Surface::Queue) && !self.queued.contains(&Action::ListMergeQueue) {
                    self.queued.push(Action::ListMergeQueue);
                    self.queues_asked_ms = self.now_ms;
                }
                self.redraw = true;
                Disposition::Filtered
            }
            _ => unreachable!("on_todo_event was handed an event it does not handle"),
        }
    }
}
