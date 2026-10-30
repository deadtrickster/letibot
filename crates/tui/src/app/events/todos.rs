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
                // **A published board is a board read** — [`App::asks_board`]: the hub this event
                // arrives on is this session's, so the mark names the session the head is in.
                self.asks_board = Some(self.session_id.clone());
                // The board is the truth once it arrives: a row answered from here is either
                // off it or — the answer refused — still asking, and asked again.
                self.todo_answered.clear();
                self.prune_todo_asks();
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
                // stale queue after any event it missed. One re-read is queued (a burst of
                // moves is one read) **whether or not the pane is open** (R92 item 5): the
                // composer's edge draws this session's repository's standing from the same
                // snapshot, and a head that re-read only with the pane open kept `‼ 1 red
                // target(s)` on its edge long after `main` was green.
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
