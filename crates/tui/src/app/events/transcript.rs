//! **Events about the transcript**: a row appended, and its content arriving.

use super::*;
use letibot_sessionlog::event::SessionEvent;
use letibot_sessionlog::view::{CallState, SnapshotItem};

impl App {
    /// **The transcript**: a row appended, and its content arriving. One family of
    /// [`App::event`]'s arms, moved here verbatim; `event` hands it only these variants.
    pub(crate) fn on_transcript_event(&mut self, e: SessionEvent, ts: u64) -> Disposition {
        match e {
            SessionEvent::TranscriptAppended {
                item_id,
                kind,
                ledger_head,
            } => {
                // A tool-result row is the answer to one live card, and its facts —
                // the duration, the edit pair, the decision — are carried across
                // HERE, at the announcement, because this is the one moment the
                // positional frontier is known AND the row exists to key them on.
                // Positional, not by id: the engine invokes a round's calls in
                // order and appends their rows in the same order, and the ids
                // repeat every round so there is nothing to match on.
                //
                // **The CARD itself is not handed over here.** The row cannot draw
                // until its body lands (`record_item`), and a row with no body
                // renders zero rows — so a pane that dropped the card at the
                // announcement left the call in *neither half* for the frames
                // between the two events: the card's rows erased, the window
                // re-derived around the hole, the settled row drawn a frame later.
                // The operator, in a loop of noop `edit` calls: *"periodic flicker
                // while diff card settles - even green caret appears briefly inside
                // the diff card"* — the erase sweep is what walked the terminal's
                // own caret through the card's rows. The handover is
                // [`App::record_item`]'s now, on the body: one frame, the card on
                // screen throughout.
                let mut carried: Option<u64> = None;
                let mut carried_edit: Option<letibot_sessionlog::event::ToolEdit> = None;
                let mut carried_decision: Option<letibot_sessionlog::view::SettledDecision> = None;
                if let Some(t) = self.turn.as_mut() {
                    t.appended.push(item_id.clone());
                    t.turn_rows.push(item_id.clone());
                    if kind == "tool_result" {
                        let c = t.calls.get(t.settled_calls);
                        carried = c
                            .filter(|c| c.started_ms > 0 && c.ended_ms > c.started_ms)
                            .map(|c| c.ended_ms - c.started_ms);
                        // The pair rides across with the duration: same card, same
                        // moment, same positional match.
                        carried_edit = c.and_then(|c| match &c.state {
                            CallState::Finished { edit: Some(e), .. } => Some(e.clone()),
                            _ => None,
                        });
                        // The approval rides across too: the decision is a fact
                        // about this call, and the row that outlives the card is
                        // where it has to keep being shown.
                        carried_decision = c.and_then(|c| c.decision.clone());
                    }
                }
                if let Some(ms) = carried {
                    self.call_ms.insert(item_id.clone(), ms);
                }
                if let Some(e) = carried_edit {
                    self.call_edits.insert(item_id.clone(), e);
                }
                if let Some(d) = carried_decision {
                    self.call_decisions.insert(item_id.clone(), d);
                }
                // **A user row is drawn from the moment it is announced.** The body
                // follows on its own channel and, behind a running turn, the reply
                // streams in the meantime — so without this the prompt is invisible
                // while the answer to it is already on the screen, and the echo
                // underneath goes on saying `queued` about words that have landed.
                // See `App::bound_prompts` for why this is a guess and what keeps it
                // honest.
                if kind == "user" {
                    self.bind_echo(&item_id);
                }
                self.items.push(SnapshotItem {
                    item_id,
                    kind,
                    ledger_head,
                    // When it happened, from the log's own clock. A head reading a
                    // recorded session must show the same times as the one that
                    // watched it, so this is never `now`.
                    ts,
                    item: None,
                });
                // A row landed, so the transcript moved and everything derived from it
                // — the `!` candidates and the model's suggestions — is stale. The next
                // Tab for the same prefix is a fresh ask.
                self.the_rows_moved();
                Disposition::Rendered
            }
            // The body for a row already announced. Before this existed, a head
            // that was attached when the row landed had no route to the content at
            // all and rendered `[kind id — content not loaded]` for the rest of the
            // session — including for the operator's own prompt.
            SessionEvent::TranscriptContent { item_id, item } => {
                self.record_item(&item_id, *item);
                Disposition::Rendered
            }
            // **The daemon has forked the transcript, so the rows this head holds are not
            // this session's rows any more.**
            //
            // The rows that follow are the carried conversation under the new transcript's
            // ids, so a head that keeps what it has folds two conversations into one list —
            // MEASURED: 40 rows plus a 60-row carry left 100, and the reader was left looking
            // at the whole of it twice. The clear is the whole of this head's half; the rows
            // arrive a moment later on the same log.
            //
            // **This is the third door onto "the rows were replaced", and the other two are
            // `load`.** A `resync`, a `Hello` and now a fork all replace the transcript whole,
            // and `load` is what the first two go through because both of those carry a
            // snapshot. A fork cannot: the rows are published as appends, and a snapshot taken
            // at the fork would be the old transcript's rows — which is why the daemon states
            // the replacement and the rows follow it.
            SessionEvent::TranscriptForked { parent_id, .. } => {
                self.rows_replaced(&parent_id);
                Disposition::Rendered
            }
            _ => unreachable!("on_transcript_event was handed an event it does not handle"),
        }
    }
}
