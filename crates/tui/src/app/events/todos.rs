//! Events about the plan: the todo list changed, and a row of the review or merge queue moved.

use super::*;
use letibot_sessionlog::event::SessionEvent;

impl App {
    /// The plan: the todo list, and the queues' moves. One family of [`App::event`]'s
    /// arms; `event` hands it only these variants.
    pub(crate) fn on_todo_event(&mut self, e: SessionEvent) -> Disposition {
        match e {
            // The model revised its plan. The whole list, not a delta: keep the latest
            // and let the pane show it. Drawn only when the pane is open, because a
            // scrollback line per write would bury the work the todos organize.
            SessionEvent::TodosUpdated { todos } => {
                self.todos = todos;
                // A published board is a board read: the hub this event arrives on is
                // this session's, so the mark names the session the head is in.
                self.asks_board = Some(self.session_id.clone());
                // The board is the truth once it arrives: a row answered from here is
                // either off it or, the answer refused, still asking and asked again.
                self.todo_answered.clear();
                self.prune_todo_asks();
                // The seed runs here because the board has just been read whole, the only
                // moment this head may add to its own half without risking a wipe:
                // `SetOperatorTodos` REPLACES the operator half, so a stale list loses rows.
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
                // A move is a reason to read again, not a row to fold: the snapshot is the
                // daemon's, and patching this copy from events goes stale after a missed one. One
                // re-read is queued, open pane or not, because the composer's edge draws it too.
                if !self.skew_locked() && !self.queued.contains(&Action::ListMergeQueue) {
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
