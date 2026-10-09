//! **Events about the plan**: the todo list changed, and the merge queue gained or moved an entry.

use super::*;
use letibot_sessionlog::event::SessionEvent;

impl App {
    /// **The plan**: the todo list, and merge-queue entries added or moved. One family of
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
                if self.todos_pane {
                    Disposition::Rendered
                } else {
                    Disposition::Filtered
                }
            }
            SessionEvent::MergeEntryAdded { entry } => {
                // **Folded, never invented** — the jobs pane's rule: the daemon owns the queue,
                // so an entry this head has not been told about is not one to make up. An entry
                // already here is REPLACED, because the same id arriving twice is the same
                // entry (the enqueue is idempotent by id) and the newer word is the true one.
                match self.merge.iter_mut().find(|e| e.id == entry.id) {
                    Some(row) => *row = entry,
                    None => self.merge.push(entry),
                }
                self.redraw = true;
                // **Counted as filtered and not as control**, for the reason
                // `OperatorCallAllowed`'s arm records: this is a session event, read, and
                // deliberately not drawn as a row of the conversation — the pane is where it
                // goes. `Control`'s definition is *not an event*, and a reader would never find
                // the difference.
                Disposition::Filtered
            }
            SessionEvent::MergeEntryMoved {
                id,
                state,
                evidence,
            } => {
                // **The move, folded onto the row the queue already holds.** An id this head
                // does not have is a move for an entry whose `MergeEntryAdded` it missed —
                // possible, since the events and the snapshot are two arrivals — so the row is
                // NOT invented here: the next `ListMergeQueue` carries it. The state is applied
                // either way, because a row that is here must not go on claiming the state it
                // had.
                if let Some(row) = self.merge.iter_mut().find(|e| e.id == id) {
                    row.state = state;
                    row.evidence = evidence;
                }
                self.redraw = true;
                Disposition::Filtered
            }
            SessionEvent::MergeEntryRemoved { id, evidence } => {
                // **The row goes, and its verdict goes with it.** The store deletes the review
                // with the entry (`Store::remove_merge_entry`), so a head that kept it would
                // answer `review_of` for an entry that is not there — and the overlay's
                // *attach to the reviewer* line would outlive the verdict it points at.
                self.merge.retain(|e| e.id != id);
                self.merge_reviews.retain(|r| r.entry_id != id);
                // **And a cursor past the end is put back on a row.** The removal is the one
                // queue event that shortens the list, so the selection can be left pointing
                // past the last row — which the arrows and Enter would then read as *nothing*
                // while the pane still drew a cursor. `evidence` is deliberately not drawn:
                // there is no row to put it on, and the sentence the operator reads is the
                // verb's reply in the conversation. It is on the event because the session log
                // is the durable record of what the queue did.
                let _ = evidence;
                if self.queue_sel >= self.merge.len() {
                    self.queue_sel = self.merge.len().saturating_sub(1);
                }
                self.redraw = true;
                Disposition::Filtered
            }
            _ => unreachable!("on_todo_event was handed an event it does not handle"),
        }
    }
}
