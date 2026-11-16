//! Events about the transcript: a row appended, and its content arriving.

use super::*;
use letibot_sessionlog::event::SessionEvent;
use letibot_sessionlog::view::{CallState, SnapshotItem};

impl App {
    /// The transcript: a row appended, and its content arriving. One family of
    /// [`App::event`]'s arms, moved here verbatim; `event` hands it only these variants.
    pub(crate) fn on_transcript_event(&mut self, e: SessionEvent, ts: u64) -> Disposition {
        match e {
            SessionEvent::TranscriptAppended {
                item_id,
                kind,
                ledger_head,
                item: body,
                body_bytes,
            } => {
                // The turn's ledger FIRST, whatever the window does with the row: `appended` is what
                // the live pane's hand-over counts, so a row let go below the window and fetched back
                // later must still be named here or its live card stands under the page that draws it.
                if let Some(t) = self.turn.as_mut() {
                    t.appended.push(item_id.clone());
                    t.turn_rows.push(item_id.clone());
                }
                // A row that arrives below the window is counted, not held: this head let the tail go (see
                // `App::drop_rows_below`), so a spliced row would take the ordinal of one still missing. The page the
                // reader comes down to fills that gap; until then the count is the truth.
                if self.items_below > 0 {
                    self.items_below += 1;
                    return Disposition::Rendered;
                }
                // A row announced without its body: what it is and how much is still coming, so the
                // row draws one line instead of none while that body takes its second journey. The
                // daemon is the one who decided the payload would not ride the announcement.
                if body.is_none() && kind == "tool_result" {
                    let (name, target) = self.pending_call_words();
                    self.pending_rows.insert(
                        item_id.clone(),
                        PendingRow {
                            bytes: body_bytes,
                            name,
                            target,
                        },
                    );
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
                // - the `!` candidates and the model's suggestions - is stale. The next
                // Tab for the same prefix is a fresh ask.
                self.the_rows_moved();
                // The body, when it rode the announcement: the row is whole now and waits for no
                // second event.
                if let Some((id, body)) = drawn {
                    self.record_item(&id, body);
                }
                // The row is on the screen now, body or not, so the call it answers is the
                // transcript's and the live card that drew it has stood down.
                self.settle_rows();
                // And the restore this head was waiting on has begun to arrive. The line that
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
            // What a reply cited, read by the daemon when it landed - put on the row, and the
            // row drawn again so its fences show the lines.
            SessionEvent::Citations { item_id, citations } => {
                if let Some(idx) = self.items.iter().position(|r| r.item_id == item_id) {
                    self.items[idx].citations = citations;
                    self.invalidate_history_from(idx);
                    // And the same range as a mark, so the pane's gutter and `alt-d` reach it.
                    self.marks_of_row(idx);
                }
                Disposition::Rendered
            }
            // The daemon has forked the transcript, so these rows are no longer this session's and
            // the ones that follow carry the same conversation under new ids - keeping both draws it
            // twice. `load` cannot take this: a fork publishes appends, so its place is taken below.
            SessionEvent::TranscriptForked {
                transcript_id,
                parent_id,
                context_after,
            } => {
                self.rows_replaced(&transcript_id, &parent_id);
                if let Some(after) = context_after {
                    self.fork_context(after);
                }
                Disposition::Rendered
            }
            _ => unreachable!("on_transcript_event was handed an event it does not handle"),
        }
    }
}

impl App {
    /// Bind a result row to the call it answers, by call id, and carry that call's facts onto the row:
    /// duration, edit pair, decision. Only for a row of this round's ledger, since an older row paged
    /// back in names a call id the round reuses.
    pub(crate) fn carry_call_facts(&mut self, item_id: &str, call_id: &str) {
        let Some(t) = self.turn.as_mut() else {
            return;
        };
        if !t.appended.iter().any(|a| a == item_id) {
            return;
        }
        let Some(c) = t.calls.iter().find(|c| c.call_id == call_id) else {
            return;
        };
        let ms = (c.started_ms > 0 && c.ended_ms > c.started_ms).then(|| c.ended_ms - c.started_ms);
        let edits = match &c.state {
            CallState::Finished { edits, .. } if !edits.is_empty() => Some(edits.clone()),
            _ => None,
        };
        let decision = c.decision.clone();
        if !t.bound_calls.iter().any(|b| b == call_id) {
            t.bound_calls.push(call_id.to_string());
        }
        if let Some(ms) = ms {
            self.call_ms.insert(item_id.to_string(), ms);
        }
        if let Some(e) = edits {
            self.call_edits.insert(item_id.to_string(), e);
        }
        if let Some(d) = decision {
            self.call_decisions.insert(item_id.to_string(), d);
        }
    }

    /// The facts a result row carries from the daemon: its duration and its decision. They fill what
    /// this head did not watch live and never replace it, so a restart or a resync keeps both.
    pub(crate) fn stored_call_fact(&mut self, item_id: &str, item: &TranscriptItem) {
        let TranscriptItem::ToolResult {
            ran_ms, decision, ..
        } = item
        else {
            return;
        };
        if let Some(ms) = ran_ms {
            self.call_ms.entry(item_id.to_string()).or_insert(*ms);
        }
        // A value the head cannot read is dropped: the card then shows no approval, as before the field.
        if let Some(d) = decision
            .as_ref()
            .and_then(|v| serde_json::from_value::<SettledDecision>(v.clone()).ok())
        {
            self.call_decisions.entry(item_id.to_string()).or_insert(d);
        }
    }

    /// [`App::stored_call_fact`] for every row of a page or a snapshot that carries its body.
    pub(crate) fn stored_call_facts(&mut self, rows: &[SnapshotItem]) {
        for r in rows {
            if let Some(item) = &r.item {
                self.stored_call_fact(&r.item_id, item);
            }
        }
    }

    /// What a bodyless result row says until its body names its call: the words of the first call of
    /// the round no row is bound to or waits on. A placeholder only; the facts wait for the call id.
    fn pending_call_words(&self) -> (String, String) {
        let Some(t) = self.turn.as_ref() else {
            return Default::default();
        };
        let waiting = t
            .appended
            .iter()
            .filter(|a| self.pending_rows.contains_key(a.as_str()))
            .count();
        t.calls
            .iter()
            .filter(|c| !t.bound_calls.contains(&c.call_id))
            .nth(waiting)
            .map(|c| (c.name.clone(), c.target.clone()))
            .unwrap_or_default()
    }
}
