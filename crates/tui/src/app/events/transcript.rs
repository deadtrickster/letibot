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
                item: body,
                ..
            } => {
                // **A row that arrives below the window is counted and not held.** This head let
                // the tail go (see `App::drop_rows_below`), so a row spliced onto the end here
                // would take the ordinal of one still missing: the page the reader comes down to
                // is what fills that gap, and until then the count is the truth.
                if self.items_below > 0 {
                    self.items_below += 1;
                    return Disposition::Rendered;
                }
                // A tool result's facts - duration, edit pair, decision - ride across at the
                // announcement, positionally (a round's rows are appended in call order and the ids
                // repeat). The CARD waits for its body in `record_item`, or the call flickers out.
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
                // A user row is drawn from the moment it is announced: the body follows on its own
                // channel and the reply streams meanwhile, so without this the prompt is invisible
                // while its answer is on screen. `App::bound_prompts` says why the guess is safe.
                if kind == "user" {
                    self.bind_echo(&item_id);
                }
                let drawn = body.map(|b| (item_id.clone(), *b));
                self.items.push(SnapshotItem {
                    citations: Vec::new(),
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
                // The body, when it rode the announcement: the row is whole now and waits for no
                // second event.
                if let Some((id, body)) = drawn {
                    self.record_item(&id, body);
                }
                // **And the restore this head was waiting on has begun to arrive.** The line that
                // said *N rows are in the store and not here yet* was true until this row; from
                // here the conversation draws itself, so it goes. See [`App::restoring`].
                self.restoring = self.rows_the_store_holds();
                Disposition::Rendered
            }
            // The body for a row already announced. Without this route a head attached when the row
            // landed rendered `[kind id - content not loaded]` for the rest of the session, the
            // operator's own prompt included.
            SessionEvent::TranscriptContent { item_id, item } => {
                self.record_item(&item_id, *item);
                Disposition::Rendered
            }
            // **What a reply cited, read by the daemon when it landed** — put on the row, and the
            // row drawn again so its fences show the lines.
            SessionEvent::Citations { item_id, citations } => {
                if let Some(idx) = self.items.iter().position(|r| r.item_id == item_id) {
                    self.items[idx].citations = citations;
                    self.invalidate_history_from(idx);
                }
                Disposition::Rendered
            }
            // The daemon has forked the transcript, so these rows are no longer this session's and
            // the ones that follow carry the same conversation under new ids - keeping both draws it
            // twice. `load` cannot take this: a fork publishes appends, so its place is taken below.
            SessionEvent::TranscriptForked {
                transcript_id,
                parent_id,
            } => {
                self.rows_replaced(&transcript_id, &parent_id);
                Disposition::Rendered
            }
            _ => unreachable!("on_transcript_event was handed an event it does not handle"),
        }
    }
}
