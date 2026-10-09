//! **The daemon's events, applied**: a snapshot loaded, a frame applied, a transcript row
//! recorded, and the bookkeeping of forks and echoed prompts.

use super::*;
use crate::ui::render::BlockCache;
use crate::ui::round_answered;
use letibot_sessionlog::event::SessionEvent;
use letibot_sessionlog::protocol::ServerFrame;
use letibot_sessionlog::view::{
    CallState, OpenDecision, SettledDecision, Snapshot, TurnState, Warned,
};
use letibot_transcript::{TranscriptItem, UserPart};
use rano::markdown::IncrementalMarkdown;

mod asks;
mod children;
mod frames;
mod hello;
mod session;
mod term;
mod todos;
mod tools;
mod transcript;
mod turn;
mod warnings;

impl App {
    pub fn open_decisions(&self) -> &[OpenDecision] {
        &self.open
    }

    /// Apply one frame. Never sends anything; see the module note on acking.
    pub fn apply(&mut self, frame: ServerFrame) -> Disposition {
        match frame {
            frame @ ServerFrame::Hello { .. } => self.on_hello(frame),
            frame @ (ServerFrame::Sessions { .. }
            | ServerFrame::Jobs { .. }
            | ServerFrame::MergeQueue { .. }
            | ServerFrame::ShellSuggestions { .. }
            | ServerFrame::Todos { .. }
            | ServerFrame::Settings { .. }) => self.on_lists(frame),
            frame @ (ServerFrame::Diagnostic { .. }
            | ServerFrame::RowFetched { .. }
            | ServerFrame::Peeked { .. }
            | ServerFrame::Secret { .. }
            | ServerFrame::Resync { .. }
            | ServerFrame::Accepted { .. }
            | ServerFrame::Rejected { .. }
            | ServerFrame::Bye { .. }) => self.on_replies(frame),
            frame @ ServerFrame::Event { .. } => self.on_event_frame(frame),
            frame @ (ServerFrame::TermAttached { .. }
            | ServerFrame::TermStatus { .. }
            | ServerFrame::TermOutput { .. }
            | ServerFrame::TermEnded { .. }) => self.on_term_frame(frame),
        }
    }

    /// Replace all state from a snapshot. This is the late-join path and the
    /// resync path; they are the same path, which is why resync is not special.
    pub(crate) fn load(&mut self, s: Snapshot) {
        // **The same conversation, or a different one**, read once and at the top: the
        // session id is assigned a few lines down, and three things here ask the
        // question — what is carried over, and (R19) which of this head's own notes keep
        // the seam they were filed at.
        let same_session = self.session_id == s.session_id;
        // Everything session-scoped goes, not just the transcript. A snapshot is a
        // *replacement*, and this is also the switch path: carrying the previous
        // session's model name or a tool target keyed by a call id that only
        // existed over there is how a switched head shows the right conversation
        // with the wrong facts attached to it.
        if !same_session {
            self.call_targets.clear();
            self.call_ms.clear();
            self.call_edits.clear();
            self.usage = None;
            self.usage_cache_measured = true;
            self.last_timings = None;
            // The total belongs to the conversation, not to the head: switching
            // sessions must not carry one session's bill onto another's header.
            self.spent_micros = 0;
            self.spent_seen = false;
            self.model.clear();
            self.turn = None;
            self.heads = 0;
            // **The pane goes with the session it was opened in.**
            //
            // A screen belongs to the conversation it was drawn over: carried across a switch it
            // would be another session's program drawn in this one's rectangle, which is the
            // same lie a carried-over model name is. **The daemon's pane is not closed here**,
            // and it cannot be — a `TermClose` sent now would arrive *after* the `Switch`, on
            // the new session's hub, and kill the wrong thing. So the program is left running
            // for the session it belongs to, and it ends when the daemon stops or when a head in
            // that session leaves it. **And a head that switches back finds it again**: a bare
            // `!term` attaches to the pane this session has, and the daemon — which held the
            // screen all along — replays it. That is the half this rectangle's own TODO used to
            // say was missing (*"a head that switches back does not find its pane again, it
            // finds the transcript"*).
            self.term = None;
            // **And what this head believed about the pane is the session's, not this head's.**
            // A `Hello` re-asks (see that arm), and until the answer lands the fact is
            // `Unasked` — which is the state that makes `!term close` hold its line rather than
            // guess. Carrying the previous session's answer across a switch would be a
            // confirmation naming another session's program.
            self.term_fact = PaneFact::Unasked;
            self.term_ask = None;
            self.close_pending = false;
            // The subagent tree is the PARENT's fact. Carried across a switch it
            // put "1 subagent running" on the composer of the very subagent being
            // looked at (measured 2026-09-16), and Enter in the pane there would
            // have switched to itself.
            //
            // **So the rows are REPLACED, by this session's own children out of the
            // snapshot — not cleared, and not carried.** The snapshot's rows are the
            // parent's fact and nobody else's: the view they are cut from is per-session,
            // so a head that switches into `s-sub-1` is handed `s-sub-1`'s children and
            // not `s`'s, and the rule above is kept by construction rather than by a
            // clear. See `Snapshot::subagents` for the measurement that put them there.
            //
            // **This is the fix for a count that flapped.** The clear this replaces was
            // what turned a live child into a finished one on the way back: the rows were
            // gone, and the only thing left to rebuild them from was the daemon's session
            // list — whose `running` is *a turn is generating in that session at this
            // instant*, and `false` for a child parked on its own background job. So the
            // operator's `N subagents running` segment went away on a switch and came back
            // when some later list reply happened to catch the child generating, over a
            // subagent that ran throughout (*"so the counter is gone"* … *"yep and now it
            // is back. wtf"*). A switch sends `since_seq = 0`, so nothing is replayed and
            // the snapshot is the only route by which the head can be told what it watched
            // — see the `Subagent` arm in `letibot_sessionlog::view`.
            //
            // **What the clear was protecting is still protected.** A row can no longer
            // arrive from a session the head is not in: this is the snapshot's list, keyed
            // by the session the snapshot is of. And the fold below still refuses to emit a
            // row for a child whose brief belongs to somebody else.
            self.subagents = s
                .subagents
                .iter()
                .map(|v| SubagentState {
                    session_id: v.session_id.clone(),
                    state: v.state.clone(),
                    // **The event's own word for the instant**, read exactly as the live arm
                    // reads it: `running` is the one state it names in which a turn is
                    // generating. See [`SubagentState::generating`].
                    generating: v.state == "running",
                    prompt: v.prompt.clone(),
                    role: v.role.clone(),
                    task: v.task.clone(),
                    model: v.model.clone(),
                    answer: v.answer.clone(),
                    // The daemon's own stamp for when the event was published. The list's
                    // `created_ms` overrides it in the fold, the way it always did.
                    spawned_ms: v.ts,
                })
                .collect();
            self.subagents_sel = 0;
            // Jobs are the session's, the same way. The rows survived a switch
            // and kept drawing the old session's ids with the old session's byte
            // counts — and now that Enter on a row asks THIS session for that id,
            // a carried row is a question about a job that was never here.
            self.jobs.clear();
            self.jobs_sel = 0;
            // The queue is the old session's. Whatever was queued there stays
            // queued *there* — the hub drains it into that session's transcript —
            // but this head is no longer looking at that session, and an echo of
            // words belonging to a conversation that is no longer on the screen is
            // the same lie a carried-over model name is.
            self.pending_prompts.clear();
            self.unconfirmed.clear();
            // **And the viewport's place, which is a place in the rows that just went**
            // (R36). Carrying it across a switch would hold the reader on a row of another
            // conversation's transcript — the same lie a carried-over model name is.
            self.anchor = None;
        }
        self.session_id = s.session_id;
        self.seq = s.seq;
        self.dropped = self.dropped.max(s.dropped);
        // **And the subagent rows are rebuilt from the snapshot's own children, then
        // overlaid with the daemon's list.**
        //
        // Here, and not in the `Subagent` event arm, because this IS the late-join path
        // and the resync path at once (see the docstring above) — and a seed that ran
        // somewhere else would be a second way for the pane to be filled, which is how
        // the two come to disagree. Called for a same-session resync as well as a switch:
        // the fold is a rebuild from the current facts and is the same answer either way,
        // and a resync is exactly when a head's own list may be the stale one.
        //
        // The rows themselves were seeded by the `!same_session` block above, from
        // `Snapshot::subagents` — the parent's own view of its children, which is what a
        // switch can carry and a session list cannot. This call then folds the list over
        // them: the list's `running` is the one measurement of NOW either half has.
        //
        // `self.sessions` is already the fresh list by now — the `Hello` arm assigns it
        // before calling this — so the fold is reading the daemon's word and not the
        // previous session's.
        self.fold_subagents();
        // A resync of the *same* session keeps the queue — the hub's command
        // queue survives a resync, and a prompt queued behind a running turn is
        // still behind that turn — but anything the snapshot's transcript already
        // holds has landed, and its echo stands down the way `record_item` would
        // have stood it down had the row arrived live.
        for it in &s.items {
            if let Some(TranscriptItem::User { parts, .. }) = &it.item {
                for text in parts.iter().filter_map(|p| match p {
                    UserPart::Text { text } => Some(text.as_str()),
                    _ => None,
                }) {
                    self.retire_pending(text);
                }
            }
        }
        // **And what the snapshot could not resolve stops claiming `queued`** — R16's
        // third mark, and the claim the head can actually support.
        //
        // `pending_prompts` says *the daemon owes me a row for this*. A snapshot
        // **replaces** the transcript, so after one, an echo the snapshot does not carry
        // is either a row still coming or a row that a fork replaced — and from the head
        // those two are the same picture. Leaving it at `queued` asserts the first when
        // it might be the second; dropping it silently loses the operator's words. So it
        // is `unconfirmed`, and it retires normally when a row does land.
        //
        // **Where this is marked, and why here rather than at the fork.** A snapshot is
        // the only route by which the transcript is replaced — `reconnect`, `/resync`,
        // `Switch`, an import — so marking at the snapshot catches every one of them
        // instead of the two that happened to be thought of. `App::resolve_fork` handles
        // the fork the head *asked for*, where it knows the row will never arrive.
        //
        // An echo queued AFTER this point is untouched: it is added to
        // `pending_prompts` by a later `submit`, so it is not in the set being marked.
        for q in &self.pending_prompts {
            if !self.unconfirmed.iter().any(|u| u == q) {
                self.unconfirmed.push(q.clone());
            }
        }
        // The snapshot's in-flight calls are **not** seeded into `call_targets`.
        // They reach the screen as `TurnPane::calls`, which carries each call's own
        // target on the row that is about to draw it; putting them in an id-keyed
        // table as well is how a live `call_0` came to relabel a settled one.
        if let Some(TurnState::Finished { usage, timings, .. }) = s.turn.as_ref().map(|t| &t.state)
        {
            // A turn that finished measured its own cache, so the percentage is
            // real even though the row's copy may not have been.
            self.usage = Some(*usage);
            self.usage_cache_measured = true;
            self.last_timings = Some(*timings);
        }
        self.items = s.items;
        // A snapshot replaces the rows, so everything derived from them — the `!`
        // candidates and the model's suggestions — is stale and goes with them.
        self.the_rows_moved();
        // **And the fill's bar goes with the stream that carried it.**
        //
        // A `Filling` tick rides the event stream and its ONLY exit is a tick whose `done` has
        // reached `total` — so a stream that stops carrying ticks leaves the bar standing for
        // the rest of the session. That is not hypothetical: a republish of 2000 rows overruns
        // a head's 1024-event queue, the hub **demotes** the head rather than blocking
        // (`Inner::append_and_fan`), and from that moment every tick is skipped — the final one
        // included, because a demoted head is not written to at all. The head is handed a
        // snapshot instead, and this line is the head taking it: **the view it was watching is
        // gone, so the progress it was reporting belongs to a stream that no longer exists.**
        // Measured on the operator's own head, 2026-10-04: it stood at `897 of 2000 rows —
        // restoring the stored conversation` and did not move.
        //
        // **A fill that is genuinely still running is not lost by this.** Its next tick re-arms
        // the line, and ticks come one per 64 rows — so clearing here can cost one tick of a
        // bar that is still going, and buys the end of one that never will. The bar must end by
        // FACT rather than by a clock (that is why `republish_after` publishes its completion
        // unconditionally), and the fact here is that the head was just told its queue was
        // thrown away.
        self.filling = None;
        // **A snapshot records the bulk announcement; a live append never does.**
        //
        // The rows a snapshot carries without bodies are a *carry* — a fork, a reseat, a
        // resume, an import, or an attach to a daemon mid-carry. The rows a live
        // `TranscriptAppended` adds are the R2 window of an ordinary message, and putting
        // them here is exactly the defect this replaces: a trigger built on "some row
        // lacks a body" fires on every healthy turn.
        self.bulk = {
            let ids: std::collections::HashSet<String> = self
                .items
                .iter()
                .filter(|i| i.item.is_none())
                .map(|i| i.item_id.clone())
                .collect();
            (!ids.is_empty()).then(|| Bulk {
                ids,
                at_ms: self.now_ms,
            })
        };
        // **A snapshot replaced every row, so the bindings are pruned to what is
        // still there and still body-less.** Pruned rather than cleared: a resync
        // mid-prompt is exactly when the reply is racing the prompt, and dropping
        // the binding for one frame would put the echo back at the tail and take it
        // away again on the next `TranscriptContent`. An id that is gone, or whose
        // row now has its body, has nothing left for a binding to stand for — the
        // row renders from its content, and the echo at the tail is the echo's own
        // business again.
        {
            let bodyless: std::collections::HashSet<&str> = self
                .items
                .iter()
                .filter(|it| it.item.is_none())
                .map(|it| it.item_id.as_str())
                .collect();
            self.bound_prompts
                .retain(|id, _| bodyless.contains(id.as_str()));
        }
        // The snapshot's turn carries its calls **with their edit excerpts**, and
        // the rows it appended in order — the same two facts the live hand-off
        // used when it moved a card's excerpt into `call_edits` as the row landed.
        // Only the live path filled that map, so a restarted head drew every
        // landed edit panel-less even though the wire had just handed it the
        // excerpt (operator, 2026-09-17: past edits lose their diff panels on
        // restart). Seed it the same way the live arm does: positionally, the
        // Nth tool_result row is the Nth call. Rows from turns before this one
        // are not on the wire — the view keeps one turn's calls — and render as
        // they always did.
        if let Some(t) = &s.turn {
            let kinds: std::collections::HashMap<&str, &str> = self
                .items
                .iter()
                .map(|it| (it.item_id.as_str(), it.kind.as_str()))
                .collect();
            let mut call_idx = 0usize;
            for item_id in &t.appended {
                if kinds.get(item_id.as_str()).copied() == Some("tool_result") {
                    if let Some(CallState::Finished { edit: Some(e), .. }) =
                        t.calls.get(call_idx).map(|c| &c.state)
                    {
                        self.call_edits.insert(item_id.clone(), e.clone());
                    }
                    call_idx += 1;
                }
            }
        }
        self.invalidate_history();
        self.open = s.open_decisions;
        // A snapshot can replace the open set wholesale; keep the highlight in range.
        self.sel = 0;
        // A permission settles on the call it gated, so it rides the call's card
        // rather than the note list; a question — or a log recorded before the field
        // existed — has no call to ride and stays a note. The live arm makes the same
        // split, and the two have to agree.
        let (call_bound, notes_bound): (Vec<SettledDecision>, Vec<SettledDecision>) = s
            .settled_decisions
            .into_iter()
            .partition(|d| d.call_id.is_some());
        // **A snapshot's notes are HISTORY, and this head's own are not** (R19).
        //
        // Until this, everything the snapshot carried was planted at anchor 0 —
        // *everything in a snapshot is history and none of it is anchored* — which is
        // right about where it goes and wrong about what it is: a fresh head showed
        // nothing, so replaying hours of announcements as though they had just happened
        // put them above a conversation they did not precede. The operator restarted a
        // head and was met by twelve red lines: *"i dont want to see that on restart."*
        //
        // So the two kinds are sorted rather than merged. This head's own notes keep
        // their seam — it filed them while watching, at rows of this very conversation —
        // and what the snapshot adds is [`Placed::Before`]: listed by `/notes`, counted
        // by `/status`, and not drawn, because a head that has just attached has shown
        // nothing and the log is where these facts live.
        //
        // **A seam the new transcript no longer has is not a seam.** A compaction or a
        // reseat forks the conversation, so a note filed at row 200 of a 250-row
        // transcript is no longer between any two rows of this one; it joins the history
        // rather than being drawn at a place that has stopped existing.
        let mut mine: Vec<(Placed, Note)> = if same_session {
            std::mem::take(&mut self.notes)
                .into_iter()
                .map(|(place, note)| match place {
                    Placed::Seam(at) if at <= self.items.len() => (Placed::Seam(at), note),
                    _ => (Placed::Before, note),
                })
                .collect()
        } else {
            // A different conversation: these are that session's notes about rows this
            // head no longer holds, and the same rule that clears `call_targets` clears
            // them.
            Vec::new()
        };
        let mut before: Vec<(Placed, Note)> = Vec::new();
        for w in s.warnings {
            // Same rule as the live arm: `turn_failed` is the log's record of what
            // the turn's own terminal state already says on the screen. Filtering
            // it here as well is what stops a *snapshot* from putting it back —
            // which is exactly what happened the first time, and is the reason the
            // live path and the snapshot path have to agree about every filter.
            if w.code == "turn_failed" {
                continue;
            }
            let n = Note::Warned(w);
            if holds(&mine, &n) {
                continue;
            }
            before.push((Placed::Before, n));
        }
        for d in notes_bound {
            let n = Note::Decided(d);
            if holds(&mine, &n) {
                continue;
            }
            before.push((Placed::Before, n));
        }
        // Oldest first: the facts from before this window are older than anything this
        // head filed, and `/notes` numbers them in the order a reader reads.
        before.append(&mut mine);
        self.notes = before;
        self.note_upto = 0;
        // The settled rows this snapshot carries are history, and a decision that
        // gated one of them has to ride that row rather than vanish with the live
        // card. The snapshot's decisions are keyed by call id, which is
        // round-positional, so the match is best-effort: walking the rows newest
        // first, each decision goes to the most recent tool_result row that carries
        // its id, and a decision is spent on the first row it matches. Rows older
        // than the snapshot's decision window render without the approval — the same
        // `Replayed` rule the duration and the edit pair follow.
        {
            let mut by_call: std::collections::HashMap<String, SettledDecision> =
                std::collections::HashMap::new();
            for d in &call_bound {
                if let Some(cid) = &d.call_id {
                    by_call.insert(cid.clone(), d.clone());
                }
            }
            for it in self.items.iter().rev() {
                if let Some(TranscriptItem::ToolResult { call_id, .. }) = &it.item {
                    if let Some(d) = by_call.remove(call_id) {
                        self.call_decisions.insert(it.item_id.clone(), d);
                    }
                }
            }
        }
        self.heads = s.heads.len();
        self.turn = s.turn.map(|t| {
            self.model = t.model.clone();
            let mut pane = TurnPane {
                turn_id: t.turn_id,
                model: t.model,
                // No timestamps in a snapshot, so every one of these renders
                // without a duration rather than with a fabricated one.
                calls: t
                    .calls
                    .into_iter()
                    .map(|c| CallRow {
                        call_id: c.call_id,
                        name: c.name,
                        target: c.target,
                        state: c.state,
                        started_ms: 0,
                        // The same rule one field along: a snapshot carries no
                        // timestamps, so there is no anchor to measure against
                        // either.
                        started_at: 0,
                        ended_ms: 0,
                        note: None,
                        decision: None,
                    })
                    .collect(),
                progress: t.progress,
                state: Some(t.state),
                // Which rows this turn produced. Without it a head that joined late
                // cannot tell that the transcript already holds the answer, and
                // renders it twice — measured on a second head attached to a
                // finished turn, where the whole reply appeared above itself.
                appended: t.appended,
                // **`turn_rows` is deliberately NOT here, because it is not on the wire.** A head
                // that attaches mid-turn gets the current round's `appended` from the snapshot and
                // starts with no history of the turn's earlier rounds, so for the window between
                // attaching and the next `TurnStarted` it can draw the duplicate this field exists
                // to prevent. Said rather than hidden: closing it needs a daemon field, and the
                // window is one attach rather than every round.
                ..TurnPane::default()
            };
            // The snapshot carries the accumulated text **once**. Everything after
            // this is an increment. That is §13.3's wire half, arriving.
            pane.text.push(&t.text);
            pane.reasoning.push(&t.reasoning);
            // A head joining mid-call gets the markup too, so the raw chord shows
            // the same thing on a reattach as it does on the head that watched it.
            // `writing_call` stays false: a snapshot cannot say whether the block
            // is still open, and inventing a spinner that never stops is worse
            // than not showing one.
            pane.raw_call = t.raw_calls;
            // A permission settles on the call it gated, so the snapshot attaches
            // it to the call's card the way the live arm does. A decision whose call
            // is not in this turn — a log recorded before the field existed, or a
            // call the snapshot did not carry — has nowhere to ride and is dropped
            // here rather than rendered twice.
            for d in call_bound {
                let cid = d.call_id.clone().unwrap_or_default();
                if let Some(c) = pane.calls.iter_mut().find(|c| c.call_id == cid) {
                    c.decision = Some(d);
                }
            }
            pane
        });
        self.scroll = 0;
        self.redraw = true;
    }

    pub(crate) fn event(&mut self, e: SessionEvent, ts: u64) -> Disposition {
        if let Some(t) = self.turn.as_mut() {
            t.last_ms = ts.max(t.last_ms);
        }
        match e {
            e @ (SessionEvent::SessionRenamed { .. }
            | SessionEvent::HeadAttached { .. }
            | SessionEvent::HeadDetached { .. }
            | SessionEvent::ScreenRequested { .. }
            | SessionEvent::CommandIssued { .. }) => self.on_session_event(e),
            e @ (SessionEvent::TodosUpdated { .. }
            | SessionEvent::MergeEntryAdded { .. }
            | SessionEvent::MergeEntryMoved { .. }) => self.on_todo_event(e),
            e @ (SessionEvent::Subagent { .. }
            | SessionEvent::JobSettled { .. }
            | SessionEvent::JobOutput { .. }) => self.on_child_event(e, ts),
            e @ (SessionEvent::Filling { .. }
            | SessionEvent::CompactionProgress { .. }
            | SessionEvent::TurnStarted { .. }
            | SessionEvent::PromptProgress { .. }
            | SessionEvent::TokensGenerated { .. }
            | SessionEvent::Delta { .. }
            | SessionEvent::TurnFinished { .. }
            | SessionEvent::TurnFailed { .. }
            | SessionEvent::TurnInterrupted { .. }) => self.on_turn_event(e, ts),
            e @ (SessionEvent::OperatorCallAllowed { .. }
            | SessionEvent::ToolCallProposed { .. }
            | SessionEvent::ToolStarted { .. }
            | SessionEvent::ToolProgress { .. }
            | SessionEvent::ToolFinished { .. }) => self.on_tool_event(e, ts),
            e @ (SessionEvent::DecisionRequested { .. }
            | SessionEvent::DecisionAnswered { .. }
            | SessionEvent::SecretRequested { .. }
            | SessionEvent::SecretSettled { .. }
            | SessionEvent::PromptRequested { .. }
            | SessionEvent::PromptSettled { .. }
            | SessionEvent::Explain { .. }
            | SessionEvent::DenialRaised { .. }) => self.on_ask_event(e, ts),
            e @ (SessionEvent::TranscriptAppended { .. }
            | SessionEvent::TranscriptContent { .. }) => self.on_transcript_event(e, ts),
            e @ SessionEvent::Warning { .. } => self.on_warning(e, ts),
        }
    }

    /// **A page or a wheel over a TAIL-ORIGIN overlay**, which both of them are.
    ///
    /// `sub_out` and `job_out` window their content from the end — a subagent's answer
    /// and a running job's newest bytes are what those panes are opened for — so their
    /// `scroll` counts rows hidden **below** the bottom and moving toward the beginning
    /// ADDS to it. That is the opposite of `pane_scroll`, which counts rows hidden above
    /// the top because a `help`/`todos`/`slash` pane is read from its head.
    ///
    /// One function, because the two must agree about the sign — and because the second
    /// one was **forgotten**: the page keys and the wheel reached this overlay's
    /// transcript instead of the overlay, which is the defect the subagent view had
    /// already been fixed for one arm above (`"a wheel in the subagent output view
    /// scrolled the conversation underneath it"`). One arm is a place to forget.
    ///
    /// Returns whether an overlay took the key, so the caller can fall through to the
    /// transcript when none did.
    pub(crate) fn scroll_tail_overlay(&mut self, up: bool, by: usize) -> bool {
        // **A closure over the value, not a binding to the struct.** The two overlays
        // are different types (`SubOut`, `JobOut`) that happen to share a field name, so
        // an `if let … else if let …` binding one `&mut` for both arms does not compile —
        // which is the compiler saying the obvious thing: there is no shared type here,
        // only a shared rule.
        let moved = |scroll: usize| {
            if up {
                scroll.saturating_add(by)
            } else {
                scroll.saturating_sub(by)
            }
        };
        if let Some(v) = self.sub_out.as_mut() {
            v.scroll = moved(v.scroll);
        } else if let Some(v) = self.job_out.as_mut() {
            v.scroll = moved(v.scroll);
        } else {
            return false;
        }
        self.redraw = true;
        true
    }

    /// **The conversation is about to be replaced** (R16): remember what is in the
    /// air, so the fork can resolve those echoes rather than orphan them.
    ///
    /// Clone rather than a flag, because the list has to survive the echoes being
    /// retired normally in between — a prompt whose row lands before the fork needs
    /// no help from this, and one still waiting does.
    pub(crate) fn mark_fork(&mut self) {
        self.fork_pending = self.pending_prompts.clone();
    }

    /// **The fork happened** (R16): retire the echoes that were waiting on a
    /// transcript that no longer exists.
    ///
    /// The echo's prompt is in the ledger — a fork summarises everything said
    /// before it, which is why the summary exists — so what is retired is the
    /// *mark*, not the words: the conversation above already holds them, as prose
    /// in the summary, and the row that would have carried them was replaced.
    ///
    /// **Only the marked ones.** An echo queued after the fork began belongs to the
    /// new transcript and its row is still coming; retiring it would take a sentence
    /// off the screen that has not landed, which is the defect `pending_prompts`
    /// exists for.
    pub(crate) fn resolve_fork(&mut self) {
        for text in std::mem::take(&mut self.fork_pending) {
            self.retire_pending(&text);
        }
        // **A fork answers the question a snapshot could only raise.** The marks here were
        // `unconfirmed` because the head could not tell *still coming* from *replaced*; a
        // fork the head itself asked for is the second, and the echo is gone with it. Same
        // intersection, so a mark whose echo survived a piece-of-the-row retirement (the
        // engine split the run across a notice) stays until its own row lands.
        let queue: std::collections::HashSet<String> =
            self.pending_prompts.iter().cloned().collect();
        self.unconfirmed.retain(|u| queue.contains(u));
    }

    /// Stand down the echo of a queued prompt whose row has landed.
    ///
    /// **The unit is a LINE, not a message, and that is the whole of the fix.** The
    /// old rule asked whether the landing row *was* the echo, or began with it, and
    /// both questions are about whole strings while the thing being compared is a
    /// **run of prompts**: the engine merges the operator's consecutive prompts into
    /// ONE user item joined by newlines (`SteeringMessage::to_item`), and a head only
    /// sometimes does the same joining itself ([`App::submit`]'s coalescing is
    /// conditional on a turn it thinks is running). So the shapes that meet are
    /// "six prompts in the queue, one six-line row" and "two prompts joined in the
    /// queue, one five-line row" — and against a whole-string rule every one of them
    /// compares NO, which leaves the echo on the screen for the rest of the session.
    ///
    /// Measured on this head, 2026-09-23: six `queued ·` echoes of the R27
    /// instruction's six paragraphs, every one of them answered, none retired —
    /// because the row that answered them was their **join** (2752 characters, six
    /// lines) while the queue held two hundred-to-five-hundred characters per entry.
    ///
    /// So: a row's lines are consumed, once each, by the pending pieces that equal
    /// them as **whole lines**, from the front of the queue backwards. A piece that
    /// consumes nothing keeps its place; an entry left with nothing retires. The
    /// word *whole* is the correctness: a prompt `second thing` is NOT retired by a
    /// row reading `first thing\nsecond thing-guess`, which is the failure that
    /// matters — a head that swallows a prompt the daemon has not answered has put a
    /// sentence the operator typed where nobody will ever see it.
    ///
    /// **What is deliberately not done: matching a substring, or matching pieces out
    /// of order.** A piece is claimed only by a whole line at or after the last line
    /// claimed, so a queue whose pieces appear reversed in a row keeps them (an echo
    /// left standing costs a stale line; an echo wrongly retired costs the
    /// sentence), and a piece that is a *fragment* of a line claims nothing at all.
    ///
    /// A prompt coincidentally EQUAL to one whole line of a longer prompt the
    /// operator typed separately would still retire. That residue is accepted and
    /// older than this fix — it is the same risk [`App::retire_pending`]'s old
    /// front-piece branch carried, and the alternative (never retiring a piece) is
    /// the defect measured above.
    ///
    /// **This is the only thing that retires an echo, and it takes content.** The
    /// announcement of a user row says nothing about whose words it carries — a
    /// harness notice and another head's prompt are the same item — so a head that
    /// retired on [`SessionEvent::TranscriptAppended`] would lose the echo of a
    /// prompt still sitting in the hub's queue, and the operator would watch their
    /// own sentence vanish. See [`App::bound_prompts`] for the half that *is* drawn
    /// from an announcement.
    pub(crate) fn retire_pending(&mut self, row: &str) {
        // **An echo that is not in the queue is not in the unconfirmed set either.**
        // An intersection, not a removal of the row's text.
        //
        // Measured on this surface, 2026-09-23, in leticl's words and true here for the
        // same structural reason: *"an unconfirmed echo's text is never a queued text"*,
        // so `unconfirmed.retain(|u| u != row)` matches nothing — the row's text is the
        // engine's JOIN, not the echo — and an echo marked unconfirmed by an earlier
        // snapshot **whose row later landed inside a merged item** would be retired from
        // the queue and stay in this set for ever. The set is not the echo's text; it is a
        // mark ON an echo, so the only honest update is to keep the marks whose echo is
        // still there.
        let lines: Vec<&str> = row.split('\n').collect();
        // **A line is spent once.** Two prompts that say the same thing stay queued
        // separately until each of their rows lands — the property the equality rule
        // gave, which a rule retiring every matching entry would lose.
        let mut claimed = vec![false; lines.len()];
        let mut cursor = 0usize;
        let mut i = 0usize;
        while i < self.pending_prompts.len() {
            match strip_landed(&self.pending_prompts[i], &lines, &mut claimed, &mut cursor) {
                // Nothing of this entry is in the row. It keeps its place.
                None => i += 1,
                // Every piece of it has landed. The echo stands down.
                Some(rest) if rest.is_empty() => {
                    self.pending_prompts.remove(i);
                }
                // Some of it has. The echo shrinks to what is still owed.
                Some(rest) => {
                    self.pending_prompts[i] = rest;
                    i += 1;
                }
            }
        }
        // **The intersection, taken AFTER the loop.** Building it before is the same
        // defect inverted, and it is how the first version of this fix leaked: the set
        // held the queue as it was when the row arrived, so an echo that had just stood
        // down was still in it — a mark outliving the thing it was a mark on, which is
        // precisely the leak the intersection exists to close. Found by the assertion
        // below this call, on the first run.
        let queue: std::collections::HashSet<String> =
            self.pending_prompts.iter().cloned().collect();
        self.unconfirmed.retain(|u| queue.contains(u));
    }

    /// **Bind the oldest unbound echo to a row that has just been announced.**
    ///
    /// Called for a body-less `user` row, which is the shape of this head's own
    /// prompt *and* of a steering notice, a §5.7 salvage notice and another head's
    /// prompt. The announcement cannot tell them apart, so this is a guess: what it
    /// buys is that the row is drawn from the words the head already holds, at the
    /// position the transcript gave it — above the reply it caused — instead of
    /// being invisible until its body catches up while the reply streams above it.
    ///
    /// Oldest first, and never an echo already bound: several prompts in the air at
    /// once is the normal case behind a running turn, and their rows are announced
    /// in the order they were sent. The echo **stays in `pending_prompts`** — this
    /// binds a drawing, it does not retire anything (see [`App::retire_pending`]).
    ///
    /// The order is: a `BTreeMap`-free [`HashMap`] lookup, one scan of the pending
    /// list, and one clone of the text being bound. `pending_prompts` is one to a few
    /// entries — behind a running turn the engine merges consecutive operator
    /// messages, so it is ordinarily *one* — and this runs once per announced row,
    /// so it is nothing next to the render it is feeding.
    pub(crate) fn bind_echo(&mut self, item_id: &str) {
        if self.pending_prompts.is_empty() {
            return;
        }
        let taken: std::collections::HashSet<&str> =
            self.bound_prompts.values().map(String::as_str).collect();
        let Some(text) = self
            .pending_prompts
            .iter()
            .find(|p| !taken.contains(p.as_str()))
            .cloned()
        else {
            return;
        };
        self.bound_prompts.insert(item_id.to_string(), text);
    }

    /// The echo texts a **body-less row on screen is already drawing**, so the tail
    /// must not draw them a second time.
    ///
    /// Owned rather than borrowed, because the caller holds it across the frame's
    /// disjoint borrow of `self`. It is one entry per prompt in the air — ordinarily
    /// one — and it is built once per frame.
    ///
    /// Derived from `items` on every frame rather than counted, for the reason
    /// [`App::bulk`] gives: `items` is replaced wholesale by a snapshot, so anything
    /// remembered about the rows it replaced describes rows that no longer exist. A
    /// binding whose row has been trimmed out of the view, or replaced by a snapshot,
    /// draws nothing — and this then draws the echo at the tail again, which is the
    /// honest answer: the words are still this head's to show.
    ///
    /// **As LINES, not as whole texts** (R51 item 15). The tail does not ask *is this
    /// entry already on screen* — that question is answered NO for any entry that grew
    /// after it was bound, and the whole entry is then drawn twice. What it needs is the
    /// pieces being drawn, so it can take exactly those out. See [`unclaimed_prompts`],
    /// which is the walk that spends them.
    pub(crate) fn echoes_on_screen(&self) -> Vec<(String, Vec<String>)> {
        self.items
            .iter()
            .filter(|it| it.item.is_none())
            .filter_map(|it| self.bound_prompts.get(&it.item_id))
            .map(|text| (text.clone(), text.split('\n').map(str::to_string).collect()))
            .collect()
    }

    /// **Move the running command to the background** — `ctrl-o` and `/promote`.
    ///
    /// The fact to guard is a command running, and the check used to ask whether the
    /// TURN was running instead. They come apart: a terminal turn state can leave a call
    /// unsettled — the comment on the `TurnFinished` arm says so in as many words, and
    /// the engine emits `TurnFinished` on the interrupt paths while a tool is still
    /// executing. The operator, looking at a `◐ Running "cargo test …"` card while the
    /// head said otherwise: *"nothing is running to move to the background"* /
    /// *"how come"*.
    ///
    /// So it asks the calls. The daemon honours a promote inside `bash`'s own wait loop,
    /// which exists only while a command is executing, so a running call is not a proxy
    /// for the thing being promoted — it IS it.
    pub(crate) fn promote(&mut self) -> Option<Action> {
        if self.running_call().is_some() {
            self.say("moving the running command to the background");
            return Some(Action::Promote);
        }
        // Two different silences, and a head that said the same thing for both sent the
        // operator looking for a command that had not been started yet. **Busy, not generating**: a
        // turn waiting on a call is still working, and *the model is still working* is the true
        // sentence for it.
        if self.turn_busy() {
            self.say("the model is still working — there is no command running to move yet");
        } else {
            self.say("nothing is running to move to the background");
        }
        None
    }

    /// **The command running right now**, whatever the turn's own state says.
    ///
    /// Ctrl+O's precondition, and deliberately not [`App::turn_busy`] either: this asks for a
    /// command the daemon is *executing*, which is a fact about ONE call and not about the turn.
    pub(crate) fn running_call(&self) -> Option<&CallRow> {
        self.turn
            .as_ref()?
            .calls
            .iter()
            .find(|c| matches!(c.state, CallState::Running))
    }

    /// Attach content to a transcript row, from whatever route the daemon offers.
    pub fn record_item(&mut self, item_id: &str, item: TranscriptItem) {
        let prose = matches!(item, TranscriptItem::Assistant { .. });
        // **A reasoning row takes over the reasoning it carries** — the mark advances to the end
        // of what has arrived, so the live count is only ever the part no row holds. See
        // [`TurnPane::reasoned_upto`]: without this the count includes landed reasoning twice, and
        // the round boundary then makes it fall.
        if matches!(item, TranscriptItem::Reasoning { .. })
            && let Some(t) = self.turn.as_mut()
        {
            t.reasoned_upto = t.reasoning.raw().len();
        }
        // **Content ends the binding, either way.** Confirmed: the row renders from
        // its real body and the echo retires by text below. Contradicted: the row was
        // never this head's prompt — a steering notice, a §5.7 salvage notice,
        // another head's prompt — and the echo is still in `pending_prompts`, so it
        // goes back to the tail where it belongs. Either way the guess has served its
        // purpose, and neither branch may retire on the announcement instead — see
        // `App::retire_pending`.
        self.bound_prompts.remove(item_id);
        // **A body landing takes its id off the bulk announcement**, so the count follows
        // the evidence and not a clock — and an empty set means the carry is complete and
        // the trigger clears itself.
        if let Some(b) = self.bulk.as_mut() {
            b.ids.remove(item_id);
            if b.ids.is_empty() {
                self.bulk = None;
            }
        }
        // A user row with body is the transcript taking a queued prompt over. The
        // steering path appends the operator's words verbatim
        // (`SteeringMessage::to_item`: "a plain `User` item with exactly its own
        // text"), so the text is the match — and one row retires one entry, so two
        // prompts that say the same thing stay queued separately until each of
        // their rows lands.
        // **Every text part, like [`App::load`].** This read only the FIRST part, and
        // the snapshot path read every one — so the two paths could retire different
        // things from the same row, which is the drift leticl measured on its own head
        // (*"the live arm read only the FIRST text part where the snapshot path reads
        // every part"*). One call site each; the common case is one part holding the
        // engine's join, and a two-part item is two things said.
        if let TranscriptItem::User { parts, .. } = &item {
            for text in parts.iter().filter_map(|p| match p {
                UserPart::Text { text } => Some(text.as_str()),
                _ => None,
            }) {
                self.retire_pending(text);
            }
        }
        let Some(idx) = self.items.iter().position(|r| r.item_id == item_id) else {
            // **A body with no row to land on — counted, never silent** (R17).
            //
            // This used to be a bare `return`, and it is the third way a row the
            // ledger has can be missing from the screen: the announcement was
            // replaced by a snapshot that no longer carries this id, so the words
            // arrive with nowhere to go and are thrown away. Nothing said so, and
            // nothing could — the row is not in `items`, so there is not even a
            // placeholder to notice.
            //
            // It is counted rather than made to work because there is nothing to
            // recover: an out-of-order body for a row nobody has is exactly the
            // case a snapshot exists to resolve. What can be wrong here is the
            // *frequency*, and a number is how that becomes visible.
            self.orphan_bodies += 1;
            self.note(Note::Warned(Warned {
                code: "orphan_body".into(),
                detail: format!(
                    "a row's content arrived for `{item_id}`, which this head is not \
                     holding — a snapshot replaced the rows and this one was not in it, \
                     so its words have nowhere to land and are recorded only here. \
                     `/status` counts how often this has happened; a body that arrives \
                     for a row that is gone is not a rendering choice."
                ),
                ts: 0,
            }));
            self.redraw = true;
            return;
        };
        self.items[idx].item = Some(item);
        // **A landed tool-result BODY is what hands a live card over to the transcript.**
        //
        // Not the announcement: a row with no body renders zero rows, so a pane that stood
        // down at the announcement left the call in *neither half* for the frames between
        // `TranscriptAppended` and `TranscriptContent` — the card erased, the settled row not
        // drawable yet, the whole window re-derived around the hole. That one-frame gap is the
        // operator's *"periodic flicker while diff card settles"*: on a terminal that does not
        // composite a paint (no mode 2026), the erase sweep is also what showed the hardware
        // caret inside the card's rows. Doing it here puts the pane's stand-down and the row's
        // first drawing in ONE frame, the card on screen throughout.
        //
        // The boundary is [`round_answered`]'s — the calls this round's landed bodies have
        // answered, keyed on the row's own `call_id` — because that is also the marker's
        // (`live_work`: *"it advances when the row's BODY lands"*), and the pane's cards, the
        // marker's number and the marker's colour are three readers of one frontier that have
        // to agree. The frontier still advances one call at a time in invocation order, so a
        // body that lands out of order (or a row for a call this pane does not hold, R31's
        // operator deposit) claims nothing it should not: it lands in `answered` and the loop
        // stops at the first frontier call no landed row answers.
        if matches!(
            self.items[idx].item,
            Some(TranscriptItem::ToolResult { .. })
        ) && self
            .turn
            .as_ref()
            .is_some_and(|t| t.appended.iter().any(|a| a == item_id))
        {
            let answered = self
                .turn
                .as_ref()
                .map(|t| round_answered(t, &self.items))
                .unwrap_or_default();
            let t = self.turn.as_mut().expect("checked above");
            while t
                .calls
                .get(t.settled_calls)
                .is_some_and(|c| answered.contains(&c.call_id.as_str()))
            {
                t.settled_calls += 1;
            }
        }
        // **A body landing is the transcript moving too.** The row was announced with no
        // content, so it carried no tool calls a moment ago: a `!` candidate list built
        // then is missing every command this row ran, and a model asked then was asked
        // about a row that had not arrived. See [`App::the_rows_moved`].
        self.the_rows_moved();
        // The row's rendered form changed, so the history cache from that row
        // on is stale. From that row on, and not from row zero: this is the
        // hottest of the invalidations — one per transcript row, so one per
        // row per session — and re-rendering the rows above a row whose body
        // just arrived is the whole session, again, for every row in it. From
        // the head of its ROUND, because a round is what renders as a unit; see
        // `round_head`.
        let k = self.round_head(idx);
        self.invalidate_history_from(k);
        // The pane's accumulated prose, handed over the same way its calls are.
        //
        // `TurnPane::text` is every `Delta { target: Text }` of the whole turn, and
        // a turn's prose is committed to the transcript one ROUND at a time. So
        // once a round's assistant row has its body, the sentence the model wrote
        // before its first tool call is on the screen twice — in history where it
        // belongs and again in the pane below the cards. Measured at 60x34 on the
        // operator's session: "I'll take a look at what's in the tree first."
        // appearing above the round's cards and again under them.
        //
        // Clearing rather than counting bytes, because that is what "the
        // transcript has taken this over" means, and because `IncrementalMarkdown`
        // is a frozen-prefix lexer — slicing it would mean re-lexing what it has
        // already frozen, which is the §13.3 rule this head is built around.
        //
        // Safe against a race only because the engine appends a round's assistant
        // row before generating the next round (`harnessd::harness`), so no delta
        // of round N+1 can arrive before round N's row.
        if prose
            && let Some(t) = self.turn.as_mut()
            && t.appended.iter().any(|a| a == item_id)
        {
            t.text = IncrementalMarkdown::new();
            t.text_cache = BlockCache::new();
        }
    }
}

/// **One pending echo against one landing row**: what of the echo is still owed.
///
/// `None` when the row says nothing about this echo — no whole line of it is a whole
/// piece of the echo. `Some("")` when the echo is fully accounted for. `Some(rest)`
/// when a run of its lines landed and the rest has not.
///
/// `claimed` and `cursor` are the row's lines, spent across the whole queue in one
/// call: each line of the row answers at most one piece, and only in the order the
/// queue holds them, which is the order the daemon appended them in. See
/// [`App::retire_pending`] for why the unit is a line and why the two guards —
/// whole-line equality and no going backwards — are what make it safe.
pub(crate) fn strip_landed(
    entry: &str,
    lines: &[&str],
    claimed: &mut [bool],
    cursor: &mut usize,
) -> Option<String> {
    let pieces: Vec<&str> = entry.split('\n').collect();
    let mut kept: Vec<&str> = Vec::with_capacity(pieces.len());
    let mut hit = false;
    for piece in &pieces {
        // **A blank line is not a claim.** It carries no words, so it can say
        // nothing about whether a prompt landed — and two prompts that differ only
        // in blank lines would otherwise retire each other.
        if piece.is_empty() {
            kept.push(piece);
            continue;
        }
        match (*cursor..lines.len()).find(|k| !claimed[*k] && lines[*k] == *piece) {
            Some(k) => {
                claimed[k] = true;
                *cursor = k + 1;
                hit = true;
            }
            None => kept.push(piece),
        }
    }
    // **What is left is the WORDS still owed, not the blank scaffolding around them.**
    // The blank pieces are kept in the walk above (a blank line matches nothing, so it can
    // never be *claimed* and must not be dropped mid-compare), but they are not content:
    // an entry whose only remaining pieces are blank has had every word of it accounted
    // for, and joining them back would hand the caller a string that is truthy and empty
    // — so the echo would stay on the screen for the rest of the session showing nothing.
    // Found by replaying the operator's own rows through this rule
    // (`docs/evidence/queued-echoes-2026-09-23.py`), where the entry's tail was a blank.
    let rest = kept
        .iter()
        .filter(|p| !p.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    hit.then_some(rest)
}

/// **The queue as the tail must DRAW it** — R51 item 15, and it is `strip_landed`'s
/// read-only twin.
///
/// # The defect
///
/// A body-less `user` row is drawn from the echo this head bound to it ([`App::bound_prompts`]),
/// and the tail draws the rest of the queue. The two were kept apart by comparing WHOLE
/// STRINGS — the tail skipped a pending entry whose text was exactly one a row was drawing —
/// and **an entry that GREW after it was bound defeats that**: bound as `"A"`, it becomes
/// `"A\nB\nC"` as the operator keeps typing (the coalescing item 14 requires), so the text no
/// longer matches and the tail draws **the whole entry again** — `A` on the screen twice, once
/// in the row's own place and once in the queue below it.
///
/// # The rule
///
/// **The unit of drawing is the entry; the unit of claiming is the piece.** Each whole line a
/// bound row is drawing is spent once against the queue (the walk is [`strip_landed`]'s, so a
/// claim here and a claim by a landing row cannot disagree about what a claim IS), and what is
/// left of an entry is joined back and returned as ONE block.
///
/// Returns, per entry the tail still owes something for: **the index into `pending`, the
/// remainder to draw, and the ORIGINAL text whose drawing claimed it** (`None` when nothing did).
///
/// # Why the third field, which is not about drawing at all
///
/// The `unconfirmed` mark is a sentence about the prompt this head sent — *the snapshot replaced
/// the transcript, so I can no longer tell `still coming` from `replaced`* — and it is looked up
/// **by text**. The row above looks it up under the text IT is drawing (the bound one), so a tail
/// that looked it up under the ENTRY's text would answer differently for the same prompt the moment
/// the entry grew: `unconfirmed` on one row and `queued` on the next, which is two statements about
/// one fact. **The claiming text is returned so the remainder can carry the mark its own row
/// carries** — the row in the transcript's own place is the senior drawing, and the tail's
/// remainder is its tail.
pub(crate) fn unclaimed_prompts(
    pending: &[String],
    bound: &[(String, Vec<String>)],
) -> Vec<(usize, String, Option<String>)> {
    if pending.is_empty() {
        return Vec::new();
    }
    // One pass over the queue, in the order the daemon will append the rows — the same rule
    // `retire_pending` keeps, and for the same reason: an entry may not claim a line that an
    // earlier entry already claimed.
    //
    // Flattened into one line list with a flag per bound drawing, so the walk below can report
    // which drawing spent a line as well as that it did.
    let mut lines: Vec<&str> = Vec::new();
    for (_, text_lines) in bound {
        for l in text_lines {
            lines.push(l.as_str());
        }
    }
    // `owner[k]` is which bound drawing contributed line `k`.
    let mut owner: Vec<usize> = Vec::with_capacity(lines.len());
    for (b, (_, text_lines)) in bound.iter().enumerate() {
        for _ in text_lines {
            owner.push(b);
        }
    }
    let mut claimed = vec![false; lines.len()];
    let mut cursor = 0usize;
    let mut out: Vec<(usize, String, Option<String>)> = Vec::new();
    for (i, q) in pending.iter().enumerate() {
        // Recorded before the walk, because `strip_landed` moves the cursor past what it spent and
        // the first line this entry claimed is what says which row is drawing it.
        let before = cursor;
        let rest = strip_landed(q, &lines, &mut claimed, &mut cursor);
        // **The drawing that claimed the FIRST line of this entry.** Cursor order is queue order,
        // so the earliest claim is the one the entry's head belongs to; a later one is drawing a
        // line further down the same prompt.
        let by = (before..cursor)
            .find(|k| claimed[*k])
            .map(|k| owner[k])
            .and_then(|b| bound.get(b))
            .map(|(text, _)| text.clone());
        match rest {
            // Nothing of this entry is on screen in the row that bound it. Draw it whole.
            None => out.push((i, q.clone(), None)),
            // Every piece of it is. Nothing left to draw.
            Some(rest) if rest.is_empty() => {}
            // Some of it is drawn above; the rest is what the tail owes.
            Some(rest) => out.push((i, rest, by)),
        }
    }
    out
}
