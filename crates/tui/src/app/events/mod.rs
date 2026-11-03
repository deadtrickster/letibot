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
        let d = self.apply_frame(frame);
        // R89: one event, one row — a warning and the session rows that say the same event.
        self.pair_one_event();
        // **The front card is settled after every frame** (R90.3, R90.4) — see
        // [`App::sync_front`].
        self.sync_front();
        d
    }

    fn apply_frame(&mut self, frame: ServerFrame) -> Disposition {
        match frame {
            frame @ ServerFrame::Hello { .. } => self.on_hello(frame),
            frame @ (ServerFrame::Sessions { .. }
            | ServerFrame::Jobs { .. }
            | ServerFrame::StandingNotes { .. }
            | ServerFrame::Skills { .. }
            | ServerFrame::Queues { .. }
            | ServerFrame::ShellSuggestions { .. }
            | ServerFrame::Todos { .. }
            | ServerFrame::Settings { .. }) => self.on_lists(frame),
            frame @ (ServerFrame::Diagnostic { .. }
            | ServerFrame::RowFetched { .. }
            | ServerFrame::RowsFetched { .. }
            | ServerFrame::Peeked { .. }
            | ServerFrame::Secret { .. }
            | ServerFrame::Resync { .. }
            | ServerFrame::Accepted { .. }
            | ServerFrame::Rejected { .. }
            | ServerFrame::Bye { .. }) => self.on_replies(frame),
            // **A confirmation of `allow-all` that another head asked for, and how it ended**
            // (`PROTOCOL_VERSION` 54, B8). Neither a list nor a reply to anything this head
            // sent: the card is the daemon's and the answer is the person's.
            frame @ (ServerFrame::ModeConfirm { .. } | ServerFrame::ModeSettled { .. }) => {
                self.on_mode_card(frame)
            }
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
        // **The held counts are not in a snapshot** (R91.12): the daemon says them again right
        // after one, so whatever this head last read is dropped rather than carried over.
        self.held = None;
        // **The activity IS in one** (`PROTOCOL_VERSION` 52), and it is read from the snapshot for
        // the reason the turn in flight is: a head that attached between two rounds has seen no
        // `ActivityChanged`, and the round-level turn would draw it as idle while a call runs.
        self.activity = s.activity.clone();
        // A snapshot renumbers nothing, so the ids a collapse was made on come back, and the pairings are
        // pruned below where the new rows are known. Clearing them here was the event that un-collapsed every
        // notice. The same session or a different one is read once at the top, since three things ask.
        let same_session = self.session_id == s.session_id;
        // **This head's own todo cards are put away before the open list is replaced** (R90.14)
        // — under the session they were asked in, which is still `self.session_id` here.
        self.stash_todo_cards();
        // Everything session-scoped goes, not just the transcript: a snapshot is a replacement and this is
        // also the switch path, and carrying the previous session's model name or a tool target keyed by a call
        // id that only existed over there shows the right conversation with the wrong facts on it.
        if !same_session {
            // The one switch goes with the session it was thrown in: it names a run by an item id, and an id is
            // per-transcript, so carried across a switch it names another conversation's row. See `app::runclick`.
            self.drop_lift();
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
            // The pane goes with the session it was opened in: the daemon's pane is not closed here, since a
            // `TermClose` now would arrive after the `Switch` on the new session's hub and kill the wrong thing.
            self.term = None;
            // The plugin panes go with the session too: a view is the daemon's answer about this
            // session's layers, so a carried one draws another session's dashboard in this one's
            // rectangle. The new session's own arrive as it opens panes.
            self.plugin_panes.clear();
            self.plugin_open = None;
            self.plugin_screen = Default::default();
            self.plugin_sel = 0;
            self.plugin_detail = None;
            // What this head believed about the pane is the session's, not this head's: a `Hello`
            // re-asks, and until the answer lands the fact is `Unasked`, which is what makes
            // `!term close` hold its line rather than guess.
            self.term_fact = PaneFact::Unasked;
            self.term_ask = None;
            self.close_pending = false;
            // The subagent tree is the parent's fact, replaced by this session's own children out of the
            // snapshot rather than cleared: the daemon's list says a turn is generating now, which is false for a
            // child parked on a job, so the count flapped over a child that ran throughout.
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
                    slug: v.slug.clone(),
                })
                .collect();
            self.subagents_sel = 0;
            // Jobs are the session's, the same way: a carried row is a question about a job that
            // was never in this session, since Enter on it asks this session for that id.
            self.jobs.clear();
            self.jobs_sel = 0;
            // And its monitors: a carried row would retire a watch in a session not on screen.
            self.monitors.clear();
            self.monitors_sel = 0;
            // The queue is the old session's: the hub drains it into that session's transcript, and an echo of
            // words from a conversation no longer on the screen is the same lie a carried model name is.
            self.pending_prompts.clear();
            self.unconfirmed.clear();
            // **And the viewport's place, which is a place in the rows that just went**
            // (R36). Carrying it across a switch would hold the reader on a row of another
            // conversation's transcript — the same lie a carried-over model name is.
            self.anchor = None;
            // **And the carry with it**, for the same reason and one more: a carry is a
            // place taken in THIS conversation's rows, and a place in another session's
            // transcript is not a place to look for here. See [`App::carry`].
            self.carry = None;
        }
        self.session_id = s.session_id;
        self.seq = s.seq;
        self.dropped = self.dropped.max(s.dropped);
        // The subagent rows are rebuilt from the snapshot's own children and overlaid with the daemon's list,
        // here because this is the late-join and the resync path at once; a seed anywhere else would be a second
        // way to fill the pane. `self.sessions` is already the fresh list.
        self.fold_subagents();
        // The hub's queue survives a resync of the same session, but anything the snapshot's
        // transcript already holds has landed, so its echo stands down here as `record_item`
        // would have stood it down had the row arrived live.
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
        // What the snapshot could not resolve stops claiming `queued`: an echo it does not carry is either a row
        // still coming or one a fork replaced. Marked here because a snapshot is the only route by which the
        // transcript is replaced.
        for q in &self.pending_prompts {
            if !self.unconfirmed.iter().any(|u| u == q) {
                self.unconfirmed.push(q.clone());
            }
        }
        // The snapshot's in-flight calls are not seeded into `call_targets`: they reach the screen as
        // `TurnPane::calls` with each call's own target, and an id-keyed table as well is how a live `call_0`
        // relabelled a settled one.
        if let Some(TurnState::Finished { usage, timings, .. }) = s.turn.as_ref().map(|t| &t.state)
        {
            // A turn that finished measured its own cache, so the percentage is
            // real even though the row's copy may not have been.
            self.usage = Some(*usage);
            self.usage_cache_measured = true;
            self.last_timings = Some(*timings);
        }
        // The reader's place, taken before the rows go: the ids are per transcript, so what survives is the
        // row's own words and the line, both taken while the rows are still here. See [`App::repair_anchor`].
        self.carry = same_session.then(|| Carry {
            words: self.held_row_words(),
            line: self.held_line(),
            // A snapshot carries its rows in the same breath as the replacement, so there is
            // nothing left to wait for — unlike a fork, whose rows follow the statement on the
            // wire. See [`Carry::arriving`].
            arriving: false,
        });
        self.items = s.items;
        // **Where this snapshot's first row sits in the session.** The daemon trims its own view, so a
        // head that attached to a long conversation holds a window whose first row is not row 0, and the
        // ordinal is how it asks for the rows above: `items_dropped` is that count and it is already on
        // the wire. A resync replaces the window, so what the head knew about rows below it goes too.
        self.items_base = s.items_dropped as usize;
        // A snapshot does not say which of its rows are above the base - a resume's republished
        // ancestors and the base's own rows arrive as one list - so the head takes them all as the
        // base's until a fork tells it otherwise.
        self.items_lead = 0;
        // **And a snapshot reaches the tail**, so nothing sits below this window: the count a
        // parked reader was holding names rows of the transcript this one replaced.
        self.items_below = 0;
        // A page asked for against the transcript this one replaced is a page nobody will place, and
        // the rows the head was told do not exist above may be another session's entirely.
        self.fetch_asked = None;
        self.fetch_below = None;
        self.no_more_above = false;
        // A collapse is about a row, so it goes only with the rows that went: the pairing is keyed by `item_id`
        // and a snapshot hands the same ids back. A retired one outlives even the session, since a hide the
        // reader asked for must not come back when they return.
        self.absorbed.retain(|id, key| {
            self.items.iter().any(|r| &r.item_id == id) || self.dismissed.contains(key)
        });
        // And whether this session's store row holds a conversation the head has not got: asked where the rows
        // are replaced, since a snapshot can be the empty one a registered but unopened session is handed. See
        // [`App::rows_the_store_holds`].
        self.restoring = self.rows_the_store_holds();
        // A snapshot replaces the rows, so everything derived from them — the `!`
        // candidates and the model's suggestions — is stale and goes with them.
        self.the_rows_moved();
        // The fill's bar goes with the stream that carried it: its only exit is a tick whose `done` reached
        // `total`, and a republish that overruns a head's queue demotes the head, so every later tick is skipped.
        // A fill still running is not lost, since its next tick re-arms the line and the bar ends by fact.
        self.filling = None;
        // **The two maps keyed by a row's id are pruned to the rows that are still here.**
        // One rule, two doors onto a replacement — this one and [`App::rows_replaced`].
        self.prune_to_the_rows();
        // The snapshot's turn carries its calls with their edit excerpts and the rows it appended in order,
        // and only the live path filled `call_edits`, so a restarted head drew every landed edit panel-less.
        // Seeded positionally: the Nth tool_result row is the Nth call.
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
        // The cards the person put away stay away across a snapshot: a model's question still open is refreshed
        // and kept off the screen, one the snapshot no longer holds was settled meanwhile and goes, and a todo
        // row's card is this head's own. A switch takes away nothing.
        let mut open = s.open_decisions;
        // R90.29: the snapshot is the truth about what is still open — a card whose answer was
        // in flight comes back if the daemon still holds its question, and goes if it does not.
        self.answering.clear();
        let sid = self.session_id.clone();
        self.away.retain(|a| {
            a.session_id != sid || a.is_todo() || open.iter().any(|d| d.req_id == a.decision.req_id)
        });
        for a in self.away.iter_mut().filter(|a| a.session_id == sid) {
            if let Some(d) = open.iter().find(|d| d.req_id == a.decision.req_id) {
                a.decision = d.clone();
            }
        }
        open.retain(|d| !self.away.iter().any(|a| a.decision.req_id == d.req_id));
        let was_asking = self.open.first().cloned();
        self.open = open;
        // **The question the operator was answering went while they typed it** - a deadline, or
        // another head answering it - and its draft would be sent as a prompt by the next Enter.
        // A SWITCH is not a question closing: the other session's list says nothing about this
        // one's cards, and its answer would be cleared with a sentence that is not true.
        self.drop_a_gone_questions_draft(was_asking, same_session);
        // A snapshot can replace the open set wholesale; the highlight is reset only if the
        // front card changed (`App::sync_front`), and kept in range here when it did not.
        if let Some(d) = self.open.first() {
            self.sel = self.sel.min(decision_rows(d).saturating_sub(1));
        }
        // A permission settles on the call it gated, so it rides the call's card rather than the
        // note list; a question has no call to ride and stays a note. The live arm makes the same
        // split, and the two have to agree.
        let (call_bound, notes_bound): (Vec<SettledDecision>, Vec<SettledDecision>) = s
            .settled_decisions
            .into_iter()
            .partition(|d| d.call_id.is_some());
        // A snapshot's notes are history and this head's own are not, so the two are sorted rather than
        // merged: notes this head filed keep their seam, and what the snapshot adds is [`Placed::Before`],
        // listed and counted and not drawn. A seam the new transcript lacks is not a seam.
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
            // Same rule as the live arm: `turn_failed` is the log's record of what the turn's own terminal state
            // already says on the screen, so filtering it here is what stops a snapshot putting it back.
            if w.code == "turn_failed" {
                continue;
            }
            // The boot's steps are state (`App::boot_warning`), here as on the live arm.
            if self.boot_warning(&w.code, &w.detail) {
                continue;
            }
            // The model is state too, and a snapshot replays the switch that named it: taken the
            // same way the live arm takes it, and never filed as a note.
            if w.code == letibot_sessionlog::protocol::MODEL_SET {
                self.model_set(&w.detail, self.model_from_live_at);
                continue;
            }
            let n = Note::Warned(w);
            if holds(&mine, &n) {
                continue;
            }
            before.push((Placed::Before, n));
        }
        for d in notes_bound {
            // The same split the live arm makes, through the same function: a question has no ladder, so its
            // settled row is its own note rather than rano's four permission words. See [`Note::settled`].
            let n = Note::settled(d);
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
        // A decision that gated a settled row rides that row rather than vanishing with the live card. The
        // ids are round-positional, so the match is best-effort: walking newest first, each decision goes to the
        // most recent result row carrying its id, and rows older than the window render without the approval.
        {
            let mut by_call: std::collections::HashMap<String, SettledDecision> =
                std::collections::HashMap::new();
            for d in &call_bound {
                if let Some(cid) = &d.call_id {
                    by_call.insert(cid.clone(), d.clone());
                }
            }
            for it in self.items.iter().rev() {
                if let Some(TranscriptItem::ToolResult { call_id, .. }) = &it.item
                    && let Some(d) = by_call.remove(call_id)
                {
                    self.call_decisions.insert(it.item_id.clone(), d);
                }
            }
        }
        self.heads = s.heads.len();
        let now = self.now_ms;
        self.turn = s.turn.map(|t| {
            self.model = t.model.clone();
            let mut pane = TurnPane {
                turn_id: t.turn_id,
                // A running call carries its start in the log's clock; nothing else here
                // has one, so the rest render without a duration.
                calls: t
                    .calls
                    .into_iter()
                    .map(|c| CallRow {
                        call_id: c.call_id,
                        name: c.name,
                        target: c.target,
                        state: c.state,
                        // The log's clock. 0 from an older daemon, so the call is measured
                        // from when this head first saw it: a lower bound, never an epoch.
                        started_ms: c.started_ts,
                        started_at: if c.started_ts == 0 { now } else { 0 },
                        ended_ms: 0,
                        note: None,
                        decision: None,
                    })
                    .collect(),
                progress: t.progress,
                state: Some(t.state),
                // Which rows this turn produced. Without it a head that joined late cannot tell the transcript
                // already holds the answer, and renders it twice.
                appended: t.appended,
                // `turn_rows` is deliberately not here, because it is not on the wire: a head attaching mid-turn
                // gets the current round's `appended` and no history of the earlier rounds, so it can draw the
                // duplicate this field prevents until the next `TurnStarted`. Closing that needs a daemon field.
                ..TurnPane::default()
            };
            // The snapshot carries the accumulated text **once**. Everything after
            // this is an increment. That is §13.3's wire half, arriving.
            pane.text.push(&t.text);
            pane.reasoning.push(&t.reasoning);
            // A head joining mid-call gets the markup too, so the raw chord shows the same thing. `writing_call`
            // stays false: a snapshot cannot say whether the block is still open.
            pane.raw_call = t.raw_calls;
            // A permission settles on the call it gated, so the snapshot attaches it to the call's card. A
            // decision whose call is not in this turn has nowhere to ride and is dropped rather than drawn twice.
            for d in call_bound {
                let cid = d.call_id.clone().unwrap_or_default();
                if let Some(c) = pane.calls.iter_mut().find(|c| c.call_id == cid) {
                    c.decision = Some(d);
                }
            }
            pane
        });
        // The prose a snapshot's rows already hold is not ours to draw again: the daemon never trims
        // `TurnView::text` when a row lands, and the head's handover fires on a body event it was not there for.
        // The working goes through the same door, so a re-attach cannot count a row that is above it.
        if let Some(t) = self.turn.as_mut() {
            let answered = round_answered(t, &self.items);
            while t
                .calls
                .get(t.settled_calls)
                .is_some_and(|c| answered.contains(&c.call_id.as_str()))
            {
                t.settled_calls += 1;
            }
            t.hand_over_prose(&self.items);
            t.hand_over_reasoning(&self.items);
        }
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
            e @ (SessionEvent::TodosUpdated { .. } | SessionEvent::QueueMoved { .. }) => {
                self.on_todo_event(e)
            }
            e @ (SessionEvent::Subagent { .. }
            | SessionEvent::JobSettled { .. }
            | SessionEvent::JobOutput { .. }) => self.on_child_event(e, ts),
            e @ (SessionEvent::Filling { .. }
            | SessionEvent::CompactionProgress { .. }
            | SessionEvent::HeldResults { .. }
            | SessionEvent::ActivityChanged { .. }
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
            | SessionEvent::TranscriptContent { .. }
            | SessionEvent::Citations { .. }
            | SessionEvent::TranscriptForked { .. }) => self.on_transcript_event(e, ts),
            // A plugin pane's view, held by name and drawn when the operator opens it: it carries no transcript
            // row and changes no conversation, so it goes straight to the pane's own handler.
            e @ (SessionEvent::PluginPane { .. } | SessionEvent::PluginPaneGone { .. }) => {
                self.on_plugin_event(e)
            }
            e @ SessionEvent::Warning { .. } => self.on_warning(e, ts),
            // The typed twin of a warning, drawn as one row for the same reason: a person reads a
            // sentence, and automation reads the type.
            e @ SessionEvent::ProviderTrouble { .. } => self.on_provider_trouble(e, ts),
        }
    }

    /// A page or a wheel over a tail-origin overlay: `sub_out` and `job_out` window their content from the
    /// end, so moving toward the beginning adds to their `scroll`, the opposite of `pane_scroll`. One function,
    /// because the second overlay was forgotten. Returns whether an overlay took the key.
    pub(crate) fn scroll_tail_overlay(&mut self, up: bool, by: usize) -> bool {
        // A closure over the value and not a binding to the struct: the two overlays are different types that
        // share a field name, so one `&mut` for both arms does not compile, which is the compiler saying there is
        // no shared type here, only a shared rule.
        let moved = |scroll: usize| {
            if up {
                scroll.saturating_add(by)
            } else {
                scroll.saturating_sub(by)
            }
        };
        // **The one on top, and only while it is drawn** (R69): an output under another pane,
        // or under rano, is not on the screen and does not move.
        match (self.focused_surface(), &mut self.sub_out, &mut self.job_out) {
            (Some(Surface::SubOut), Some(v), _) => v.scroll = moved(v.scroll),
            (Some(Surface::JobOut), _, Some(v)) => v.scroll = moved(v.scroll),
            _ => return false,
        }
        self.redraw = true;
        true
    }

    /// The conversation is about to be replaced: remember what is in the air, so the fork can resolve those
    /// echoes rather than orphan them. Cloned rather than flagged, since the list must survive the echoes being
    /// retired normally in between.
    pub(crate) fn mark_fork(&mut self) {
        self.fork_pending = self.pending_prompts.clone();
    }

    /// The two maps keyed by a row's id, pruned to the rows that are still here: a row that is gone or whose
    /// body landed has nothing for either promise to stand for. Pruned and not cleared, so a replacement
    /// carrying the same rows back drops no binding and forgets no announcement.
    pub(crate) fn prune_to_the_rows(&mut self) {
        // The probe's answers are pruned, not cleared, the same rule the two id-keyed maps keep: a resync that
        // carries the same rows back must not make this head ask the store again, and an id that left takes its
        // answer with it.
        let (held, absent, asked) = match self.bulk.take() {
            Some(b) => (b.held, b.absent, b.asked),
            None => Default::default(),
        };
        self.bulk = {
            let ids: std::collections::HashSet<String> = self
                .items
                .iter()
                .filter(|i| i.item.is_none())
                .map(|i| i.item_id.clone())
                .collect();
            (!ids.is_empty()).then(|| Bulk {
                announced: ids.len(),
                held: held.into_iter().filter(|i| ids.contains(i)).collect(),
                absent: absent.into_iter().filter(|i| ids.contains(i)).collect(),
                asked: asked.into_iter().filter(|i| ids.contains(i)).collect(),
                ids,
                at_ms: self.now_ms,
            })
        };
        let bodyless: std::collections::HashSet<&str> = self
            .items
            .iter()
            .filter(|it| it.item.is_none())
            .map(|it| it.item_id.as_str())
            .collect();
        self.bound_prompts
            .retain(|id, _| bodyless.contains(id.as_str()));
    }

    /// The rows this head holds that are numbered in a transcript the session has left: the head's half of
    /// `TranscriptForked`, whose carried rows are all published under new ids, so not one held id is in the
    /// carry. What it keeps above the new base is history; `items_base` is where the base starts.
    pub(crate) fn rows_replaced(&mut self, transcript_id: &str, parent_id: &str) {
        let gone = format!("{parent_id}.");
        let here = format!("{transcript_id}.");
        // The reader's place, taken before the rows go: the live door onto a replacement and the one `load`
        // cannot answer, since a fork's rows travel as appends. The row's own words and the line are taken here,
        // while the row is still there to be asked. See [`App::repair_anchor`].
        let arriving = !self
            .items
            .iter()
            .any(|r| r.item_id.starts_with(here.as_str()));
        self.carry = Some(Carry {
            words: self.held_row_words(),
            line: self.held_line(),
            arriving,
        });
        // **The rows before the first one that goes keep their places** - a resume republishes the tail
        // of the transcripts a compaction put behind the current one, and the reader draws them.
        let kept = self
            .items
            .iter()
            .position(|r| r.item_id.starts_with(gone.as_str()))
            .unwrap_or(self.items.len());
        self.items.retain(|r| !r.item_id.starts_with(gone.as_str()));
        // **But the ordinal space is the new base's.** It is numbered from zero, so the rows held
        // above it are rows no ordinal names: `items_lead` says where the base starts, and a page
        // above the window belongs there and not at the front.
        self.items_lead = self
            .items
            .iter()
            .position(|r| r.item_id.starts_with(here.as_str()))
            .unwrap_or(self.items.len());
        self.items_base = self
            .items
            .get(self.items_lead)
            .and_then(|r| letibot_sessionlog::view::ordinal_in(&r.item_id, &here))
            .unwrap_or(0);
        // A page in flight was asked for by an ordinal of the transcript that just went, and the rows
        // the head was told do not exist above may be another transcript's entirely.
        self.fetch_asked = None;
        self.fetch_below = None;
        self.no_more_above = false;
        // A fork's rows are not the ones this head counted below the window, and they arrive as
        // appends: the gap goes with the transcript it was a gap in.
        self.items_below = 0;
        // The one thing this head keeps by row INDEX goes with the rows: the seams its notes were filed at. A
        // seam into the rows that went joins the history, which is the rule `load` applies to a seam the new
        // transcript no longer has - and every row from `kept` on went, so every seam there goes with them.
        for (place, _) in &mut self.notes {
            if matches!(place, Placed::Seam(at) if *at >= kept) {
                *place = Placed::Before;
            }
        }
        // And the offset goes with the rows it was measured against: `scroll` is lines hidden below the
        // window, measured from the bottom of the transcript that just went. `load` has always zeroed it; the
        // live door did not, so a `/reseat` left the reader placed by a number that means nothing here.
        self.scroll = 0;
        // A row went, so everything derived from the rows — the `!` candidates and the model's
        // suggestions — is stale, by the same rule the live append follows.
        self.the_rows_moved();
        // The rendered history is of rows that are not here: dropped whole rather than from a row,
        // because the row indices themselves are about to mean something else.
        self.invalidate_history();
        // **And the two maps keyed by a row's id go with the rows that left.** The same rule
        // `load` applies to a replacement it can see the whole of.
        self.prune_to_the_rows();
    }

    /// The fork happened: retire the echoes that were waiting on a transcript that no longer exists. The
    /// prompt's words are in the ledger, as prose in the summary, so what is retired is the mark. Only the
    /// marked ones, since an echo queued after the fork began has its row still coming.
    pub(crate) fn resolve_fork(&mut self) {
        for text in std::mem::take(&mut self.fork_pending) {
            self.retire_pending(&text);
        }
        // A fork answers the question a snapshot could only raise: the marks were `unconfirmed` because the
        // head could not tell *still coming* from *replaced*, and a fork it asked for is the second. The same
        // intersection, so a mark whose echo survived a piece retirement stays until its own row lands.
        let queue: std::collections::HashSet<String> =
            self.pending_prompts.iter().cloned().collect();
        self.unconfirmed.retain(|u| queue.contains(u));
    }

    /// Stand down the echo of a queued prompt whose row has landed. The unit is a LINE and not a message,
    /// since the engine merges the person's consecutive prompts into one item joined by newlines while the
    /// head only sometimes joins them itself, and whole-string equality answers no to every such pair.
    pub(crate) fn retire_pending(&mut self, row: &str) {
        // An echo that is not in the queue is not in the unconfirmed set either: an intersection, not a
        // removal of the row's text. The row's text is the engine's join, so `retain(|u| u != row)` matches
        // nothing and a mark whose row landed inside a merged item would stay for ever.
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
        // The intersection, taken after the loop: building it before held the queue as it was when the row
        // arrived, so an echo that had just stood down was still in it, a mark outliving the thing it marked.
        let queue: std::collections::HashSet<String> =
            self.pending_prompts.iter().cloned().collect();
        self.unconfirmed.retain(|u| queue.contains(u));
    }

    /// Bind the oldest unbound echo to a row that has just been announced. A body-less `user` row is this
    /// head's own prompt and also a steering notice, a salvage notice or another head's prompt, so this is a
    /// guess: the row is drawn from the words the head holds, at the position the transcript gave it.
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

    /// The echo texts a body-less row on screen is already drawing, so the tail must not draw them again.
    /// Owned, since the caller holds it across the frame's disjoint borrow, and derived from `items` every
    /// frame because a snapshot replaces them wholesale. See [`App::bulk`].
    pub(crate) fn echoes_on_screen(&self) -> Vec<(String, Vec<String>)> {
        self.items
            .iter()
            .filter(|it| it.item.is_none())
            .filter_map(|it| self.bound_prompts.get(&it.item_id))
            .map(|text| (text.clone(), text.split('\n').map(str::to_string).collect()))
            .collect()
    }

    /// Move the running command to the background: `ctrl-o` and `/promote`. The guard is a command running
    /// and not the turn, since a terminal turn state can leave a call unsettled, which is how the head came to
    /// say *nothing is running to move* over a running card. So it asks the calls.
    pub(crate) fn promote(&mut self) -> Option<Action> {
        if self.running_call().is_some() {
            self.say("moving the running command to the background");
            return Some(Action::Promote);
        }
        // Two different silences, and one sentence for both sent the person looking for a command that had
        // not started. Busy, not generating: a turn waiting on a call is still working.
        if self.turn_busy() {
            self.say("the model is still working — there is no command running to move yet");
        } else {
            self.say("nothing is running to move to the background");
        }
        None
    }

    /// The command running right now, whatever the turn's own state says. Ctrl+O's precondition, and
    /// deliberately not [`App::turn_busy`]: this asks for a command being executed, which is a fact about one
    /// call.
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
        // A reasoning row takes over the reasoning it carries ([`TurnPane::hand_over_reasoning`]), called
        // once the body is in `items` for it to read. It used to advance a byte mark before the body was placed,
        // which is why the mark could only trim the count.
        let reasoning = matches!(item, TranscriptItem::Reasoning { .. });
        // Content ends the binding either way. Confirmed, the row renders from its real body and the echo
        // retires by text; contradicted, the row was never this head's prompt and the echo goes back to the
        // tail. Neither branch may retire on the announcement.
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
        // A user row with a body is the transcript taking a queued prompt over, so the text is the match, and
        // one row retires one entry. Every text part, like [`App::load`], since reading only the first let the
        // two paths retire different things from one row.
        if let TranscriptItem::User { parts, .. } = &item {
            for text in parts.iter().filter_map(|p| match p {
                UserPart::Text { text } => Some(text.as_str()),
                _ => None,
            }) {
                self.retire_pending(text);
            }
        }
        let Some(idx) = self.items.iter().position(|r| r.item_id == item_id) else {
            // **A head holding a window is sent bodies for rows it let go**, above and below it:
            // the page the reader asks for brings the row back whole, so this is the window doing
            // its job and not a body with nowhere to go.
            if self.items_base > 0 || self.items_below > 0 {
                return;
            }
            // A body with no row to land on is counted and never silent: the announcement was replaced by a
            // snapshot that no longer carries this id, so the words arrive with nowhere to go. Counted rather
            // than made to work, because there is nothing to recover.
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
        // A landed tool-result body is what hands a live card over. Not the announcement: a row with no body
        // renders zero rows, so a pane that stood down there left the call in neither half for the frames
        // between the two events, which is the flicker the operator saw.
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
        // A body landing is the transcript moving too: the row carried no tool calls a moment ago, so a `!`
        // candidate list built then is missing every command this row ran. See [`App::the_rows_moved`].
        self.the_rows_moved();
        // The row's rendered form changed, so the history from that row on is stale, and from the head of its
        // ROUND: this is the hottest invalidation, one per transcript row, and re-rendering above it is the
        // whole session for every row in it.
        let k = self.round_head(idx).min(self.run_head(idx));
        self.invalidate_history_from(k);
        // The pane's accumulated prose, handed over the same way its calls are: a turn's prose is committed a
        // round at a time, so once a round's assistant row has its body the sentence is on the screen twice.
        // Clearing rather than counting, since the row is the copy.
        if prose
            && let Some(t) = self.turn.as_mut()
            && (t.appended.iter().any(|a| a == item_id) || t.prose_taken_over(&self.items))
        {
            t.text = IncrementalMarkdown::new();
            t.text_cache = BlockCache::new();
        }
        // And the thinking, by the same rule and at the same moment: the row's body is in `items` by now, so
        // the walk that spends what the round's rows hold can read it. That is what stops a landed thought
        // being drawn twice with the fold open. See [`TurnPane::hand_over_reasoning`].
        if reasoning && let Some(t) = self.turn.as_mut() {
            t.hand_over_reasoning(&self.items);
        }
    }
}

/// One pending echo against one landing row: what of the echo is still owed. `None` when no whole line of
/// the row is a whole piece of the echo, `Some("")` when it is fully accounted for, `Some(rest)` when a run
/// of its lines landed. `claimed` and `cursor` are spent across the whole queue in order.
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
    // What is left is the words still owed and not the blank scaffolding: a blank piece is kept in the walk
    // (it can never be claimed) but is not content, so an entry whose remaining pieces are blank has been
    // accounted for and joining them would hand back a string that is truthy and empty.
    let rest = kept
        .iter()
        .filter(|p| !p.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    hit.then_some(rest)
}

/// The queue as the tail must draw it, the read-only twin of `strip_landed`. Comparing whole strings kept the
/// tail and a bound row apart, and an entry that GREW after it was bound defeats that, so `A` was drawn twice.
/// The unit is the piece, so the line a row is drawing is spent and the rest is drawn as one block.
pub(crate) fn unclaimed_prompts(
    pending: &[String],
    bound: &[(String, Vec<String>)],
) -> Vec<(usize, String, Option<String>)> {
    if pending.is_empty() {
        return Vec::new();
    }
    // One pass over the queue in the order the daemon will append the rows, the same rule `retire_pending`
    // keeps: an entry may not claim a line an earlier one already claimed. Flattened into one list with a flag
    // per bound drawing, so the walk can report which drawing spent a line.
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
