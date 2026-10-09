//! **The subagents', jobs' and queue panes' state**: their rows, which row is under a click,
//! folding the tree, and a job's output paged.

use super::*;
use letibot_sessionlog::StoredEnd;
use letibot_sessionlog::registry::short_id;

impl App {
    /// The same, for the subagent tree — `/subagents` and `ctrl-g`.
    ///
    /// **And the fold runs on the way in.** The pane used to be built only by the live
    /// `Subagent` events, and the comment here used to claim *"the tree is folded from
    /// durable `Subagent` events, which a snapshot carries"* — **which was not true of this
    /// daemon** (`SessionEvent::Subagent` was folded into nothing at all by the view: see
    /// the arm in `letibot_sessionlog::view`, which now folds it). So a head that attached
    /// after the spawns drew an empty pane and no count, and the only thing that could ever
    /// fill it was a later spawn — the operator: *"i just restarted the head and the
    /// subagents list is gone … when you started new subagents the subagents pane
    /// refreshed"*. Two durable halves now feed it: the snapshot's own children, and the
    /// daemon's session list — so [`App::fold_subagents`] reads it here, where the rows are
    /// about to be looked at.
    pub(crate) fn toggle_subagents(&mut self) {
        self.subagents_pane = !self.subagents_pane;
        self.pane_scroll = 0;
        self.fold_subagents();
        self.redraw = true;
    }

    /// **The jobs pane's own toggle — and the only spelling of it.** `ctrl-q`, `/jobs` and
    /// a click on the jobs count label all end here, so the three cannot drift the way a
    /// second copy of "open the pane" drifts: one of them asking for the table and the
    /// others forgetting is exactly the defect §6's rule ("both spellings end in the same
    /// function") exists to close.
    ///
    /// Opening asks the daemon for the table, the way the todos pane asks for its rows:
    /// the process table is the daemon's and a head that drew its own version drew a stale
    /// one. Later changes arrive as `JobSettled`.
    pub(crate) fn toggle_jobs(&mut self) -> Option<Action> {
        self.jobs_pane = !self.jobs_pane;
        self.pane_scroll = 0;
        self.redraw = true;
        self.jobs_pane.then_some(Action::ListJobs)
    }

    /// **The pane's rows, as ONE enumeration** — the arrows, Enter, `p`, the drawn `▸` and the
    /// scroll all read this and nothing else.
    ///
    /// # The two groups, and why `finished` is folded
    ///
    /// The operator, 2026-10-06: *"i went to subagents panel and dont see it here"* — a subagent
    /// just started, and the pane drew the finished ones and pushed the running one off the
    /// bottom, because a child this head WATCHED spawn is appended after the durable rows
    /// ([`App::fold_subagents`]) and so lands LAST. (It is not a delay: the daemon publishes the
    /// child within a fraction of a second of the spawn — `subagent … open after 0.3s — running`
    /// is its own progress line — so the row is there and simply below the fold.) And then:
    /// *"please group finished separately in the finished group which will be collapsed"*.
    ///
    /// So the children still going come first, then one `finished (N)` row, then — only when it
    /// is unfolded — the finished children themselves. The active half is never empty for a live
    /// spawn, which is the whole point: the row the operator opened the pane to see is at the top.
    pub(crate) fn subagent_stops(&self) -> Vec<SubStop> {
        self.subagents_view().stops()
    }

    /// **The pane row the stop at the cursor was DRAWN on**, read out of
    /// [`App::subagents_stop_rows`] — the record the pane wrote while drawing, and not arithmetic
    /// over the lists it drew from. The sibling of [`App::todos_row_of`], and the clamp is the
    /// same: the list can change under the cursor, and an arrow pressed against a shorter list
    /// must land on a row rather than on an index that no longer exists.
    pub(crate) fn subagents_row_of(&self) -> usize {
        let at = self
            .subagents_sel
            .min(self.subagents_stop_rows.len().saturating_sub(1));
        self.subagents_stop_rows.get(at).copied().unwrap_or(0)
    }

    /// **The subagent rows: this session's children, as the DAEMON's list has them.**
    ///
    /// # Why this exists, and the two halves it joins
    ///
    /// The pane and the composer's count are the same list, and the list had exactly one
    /// source: the live `SessionEvent::Subagent` arm. That event carries **one child**, so
    /// it can only ever describe a spawn or a finish this head was attached for — a fresh
    /// head, a head that switched away and came back, and the parent of children spawned
    /// before it attached all drew an empty pane with no count, forever, because nothing
    /// replays a spawn. `App::load` used to clear the rows on every switch, on the belief
    /// that the live events were the only other source — and there was then nothing to put
    /// back, which is what made an empty pane (and an empty count) the permanent state of
    /// every head that attached late or switched back. Measured 2026-10-05: *"i just
    /// restarted the head and the subagents list is gone"*.
    ///
    /// **The rows are rebuilt from the snapshot's own children, then overlaid with the
    /// daemon's list** — and that order is the whole of the fix for a count that flapped.
    /// The snapshot half ([`letibot_sessionlog::view::Snapshot::subagents`], folded by the
    /// parent's view) is the events' own conclusion: it knows `opening`, `running`, `done`,
    /// `failed`, the role, the task and the answer. The list half knows one bit — whether a
    /// turn is generating in the child *at this instant* — and that bit is `false` for a
    /// child parked on its own background job or between two rounds. Rebuilding from the
    /// list alone therefore read that `false` as *finished*, and the composer's count
    /// dropped a live child on every switch back and picked it up again when some later
    /// list reply happened to catch the child generating: the operator's *"so the counter
    /// is gone"* … *"yep and now it is back. wtf"*, over a subagent that ran throughout.
    ///
    /// The durable half is on the wire already and needs no new frame: **`SessionBrief`
    /// carries `parent_session_id`** — the registry's own words for it are *"A head draws
    /// a subagent tree from this without reaching the store"* — and every `Hello` (which
    /// a `Switch` is answered with) and every `Sessions` frame carries the whole list. So
    /// a row is rebuilt from the same fact the picker's tree is drawn from, and the two
    /// cannot disagree about who is whose child.
    ///
    /// **What the list is still for, now that the snapshot carries the children.** Two
    /// things, and neither of them is redundant: the list's `running` is the only
    /// measurement of NOW on the wire, so it is what moves a row between *generating* and
    /// *between turns* without waiting for the child's next event; and a child of a daemon
    /// generation this view did not see — one only in the store, after a daemon was
    /// replaced — is on the list and nowhere else.
    ///
    /// # What a rebuilt row can and cannot say
    ///
    /// The list carries the child's **title** (the daemon's own one-line form of the
    /// subtask), its **model**, and whether a turn is **generating in it right now** —
    /// which is exactly what the count asks. It does not carry the state word, the role,
    /// or the answer, because those are what the event is for. So a rebuilt row says what
    /// the list says and **claims nothing about a state it was not told**: `state` stays
    /// empty, the pane draws `[?]` and `state unknown`, and the running count does not
    /// count it. A row the head *did* watch keeps every richer field, and a live event
    /// landing later fills the rebuilt row in place — same id, one row.
    ///
    /// # The one word the LIST is authoritative for, in both directions
    ///
    /// `state` is the one field both halves can speak to, and only through one word:
    /// the list's `running` is a measurement of *this instant* (a turn is generating in
    /// that session now), while a `running` a row holds is what an event said when it
    /// was published. So the word `running` comes from the list both ways round — **the
    /// list saying `true` makes the row `running` even if the row last said `done`** (a
    /// child asked for more work has started a second turn), and **the list saying
    /// `false` drops a `running` the row still claims** (a finish this head was not
    /// attached for leaves a count above the composer reading `1 subagent running` for a
    /// child that is not). Every other word is the event's and is kept as it stands:
    /// `done`, `failed` and `opening` are all *not generating*, which is a fact the list
    /// cannot tell apart from each other, and none of them is a claim about now.
    ///
    /// # Order, and one enumeration
    ///
    /// Children come in the daemon's order (the list is the enumeration the picker
    /// already numbers), and a child this head watched spawn whose brief the list does not
    /// carry yet — the list is a snapshot of its own moment, the event is not — is
    /// appended after them rather than dropped.
    pub(crate) fn fold_subagents(&mut self) {
        let known: Vec<SubagentState> = std::mem::take(&mut self.subagents);
        let mut rows: Vec<SubagentState> = Vec::with_capacity(known.len().max(4));
        for b in &self.sessions {
            if b.parent_session_id.as_deref() != Some(self.session_id.as_str()) {
                continue;
            }
            // The merge queue's reviewer, rebuilt from the list — which carries no role, so it is
            // known by the brief its title was cut from. See `on_child_event`.
            if b.title
                .starts_with(letibot_sessionlog::GATEKEEPER_TITLE_PREFIX)
            {
                continue;
            }
            match known.iter().find(|k| k.session_id == b.session_id) {
                // Watched: the event's own row, which knows more than the list does about
                // everything except whether a turn is generating in it right now.
                Some(k) => {
                    let mut k = k.clone();
                    // **The list's measurement of NOW**, kept beside the lifecycle word rather
                    // than written over it. See [`SubagentState::generating`].
                    k.generating = b.status.running;
                    k.state = if b.status.running {
                        // **A positive measurement of life, and the one direction the list may
                        // move a row**: a turn is generating in this child at this instant, so
                        // it is working. This overrides a `done` on purpose — a child asked for
                        // more work is generating whatever it last finished.
                        "running".into()
                    } else if k.state == "running" && k.answer.is_some() {
                        // **`false` may retire a row that has ALREADY completed.** The finish
                        // this row is still running on happened before the list was cut, and
                        // the child holds the answer the daemon published with its `done` — so
                        // the list's `false` is the later word about a child that has ended,
                        // and the pane may stop claiming it is running.
                        String::new()
                    } else {
                        // **And it may not touch any other row.** `false` here is *no turn is
                        // generating in this child this instant* — which is exactly what a
                        // child parked on its own background job, or sitting between two
                        // rounds, looks like — and it is NOT a completion. Reading it as one is
                        // the defect this change exists for: the count above the composer
                        // dropped live children (`4, 2, 3, 1`) while they were working, and the
                        // pane moved them into the `finished` group beside children that had
                        // actually ended.
                        k.state
                    };
                    // **The daemon's own stamp wins over the event's**, when the list carries one:
                    // that is the session's creation time, and an event's `ts` is only when this
                    // head heard about the child.
                    if b.created_ms > 0 {
                        k.spawned_ms = b.created_ms;
                    }
                    rows.push(k);
                }
                None => rows.push(SubagentState {
                    session_id: b.session_id.clone(),
                    // **Only what the daemon actually said.** `running` is the list's own
                    // "a turn is generating in this session at this instant"; past that, the
                    // stored conversation's last row (`stored_end`): an answer is a child
                    // that finished, and a turn cut off in a session the daemon no longer
                    // holds is one that stopped. Anything else — a live child between
                    // rounds, parked on its own job — is a state nobody has told this head,
                    // and an empty word draws as unknown rather than as `done`.
                    state: match (&b.stored_end, b.status.running, b.live) {
                        (_, true, _) => "running".into(),
                        (Some(StoredEnd::Answered { .. }), false, _) => "done".into(),
                        (Some(StoredEnd::MidTurn), false, false) => "stopped mid-turn".into(),
                        _ => String::new(),
                    },
                    generating: b.status.running,
                    prompt: String::new(),
                    // **The child's name, or the id the daemon shows for one it has not
                    // named** — the fallback the picker's own rows make, and the reason
                    // is the pane: a row whose words are all empty is a row the operator
                    // cannot tell from an empty pane, which is the report this change
                    // exists for.
                    task: if b.title.is_empty() {
                        short_id(&b.session_id)
                    } else {
                        b.title.clone()
                    },
                    role: String::new(),
                    model: b.status.model.clone(),
                    answer: match &b.stored_end {
                        Some(StoredEnd::Answered { first_line }) if !b.status.running => {
                            Some(first_line.clone())
                        }
                        _ => None,
                    },
                    spawned_ms: b.created_ms,
                }),
            }
        }
        for k in known {
            if !rows.iter().any(|r| r.session_id == k.session_id) {
                rows.push(k);
            }
        }
        // **NEWEST FIRST** — the operator's ask, 2026-10-05: *"fix agents pane - the ordering is
        // off - most recent agents must be on top"*.
        //
        // The rows above are built in the daemon's list order, which is creation order — oldest
        // first — with the children this head watched spawn appended after them, so without this
        // the pane drew a child that had just been started at the BOTTOM of its group: the worst
        // place for the one row the operator opened the pane to see.
        //
        // **`sort_by` and not `sort_unstable_by`**: the sort is STABLE, so rows nobody can date
        // (all the zeroes — a replay, a brief with no stamp) keep the order they arrived in rather
        // than being shuffled into an order that means nothing. A dated row still comes before an
        // undated one whatever the stability, because zero sorts last descending.
        rows.sort_by(|a, b| b.spawned_ms.cmp(&a.spawned_ms));
        self.subagents = rows;
        // **The child this head climbed up out of**, by id, once the rebuild has happened
        // — a stop index taken before it would point at whatever the new list has there.
        //
        // **A finished child is unfolded to land on.** The cursor is an index into the stops
        // ([`App::subagent_stops`]), and a finished child is not one of them while the group is
        // collapsed — so coming back up out of a child that has since ended opens the group it
        // went into, rather than dropping the cursor on the fold and hiding the row the operator
        // just left.
        let up_from = self.up_from.clone();
        if let Some(from) = up_from {
            if let Some(i) = self.subagents.iter().position(|r| r.session_id == from) {
                if self.subagents[i].is_finished() && !self.subagents_finished_open {
                    self.subagents_finished_open = true;
                }
                if let Some(k) = self
                    .subagent_stops()
                    .iter()
                    .position(|s| matches!(s, SubStop::Agent(j) if *j == i))
                {
                    self.subagents_sel = k;
                }
                self.up_from = None;
            }
        }
        self.subagents_sel = self
            .subagents_sel
            .min(self.subagent_stops().len().saturating_sub(1));
    }

    /// **The jobs pane's rows, as ONE enumeration** — running first, then one folded
    /// `finished (N)` row.
    ///
    /// The operator's own ask: *"jobs panel - same as subagents - show list of running, group
    /// finished"*. It is [`App::subagent_stops`]' shape because that pane already learned the two
    /// lessons this one needs: the rows the cursor walks and the rows the keys act on must be the
    /// same list, and a settled row the reader has stopped caring about must not push a running
    /// one off the bottom of the pane.
    pub(crate) fn job_stops(&self) -> Vec<JobStop> {
        self.jobs_view().stops()
    }

    /// **The pane row the job stop at the cursor was DRAWN on**, read out of
    /// [`App::jobs_stop_rows`] — the record the pane wrote while drawing, never arithmetic over
    /// the table it drew from. The sibling of [`App::subagents_row_of`].
    pub(crate) fn jobs_row_of(&self) -> usize {
        let at = self
            .jobs_sel
            .min(self.jobs_stop_rows.len().saturating_sub(1));
        self.jobs_stop_rows.get(at).copied().unwrap_or(0)
    }

    /// **The pane row the entry at the cursor was DRAWN on**, read out of
    /// [`App::queue_stop_rows`] — the record the pane wrote while drawing, never arithmetic over
    /// the queue. The sibling of [`App::jobs_row_of`], and the clamp is the same: the queue can
    /// move under the cursor (a `MergeEntryAdded` arriving), and an arrow pressed against a
    /// shorter queue must land on a row rather than on an index that no longer exists.
    pub(crate) fn queue_row_of(&self) -> usize {
        let at = self
            .queue_sel
            .min(self.queue_stop_rows.len().saturating_sub(1));
        self.queue_stop_rows.get(at).copied().unwrap_or(0)
    }

    /// **The entry drawn on SCREEN ROW `y`, or nothing** — the queue pane's `todo_stop_at_row`.
    ///
    /// Read from the rows the last draw recorded, so a click and the drawing cannot disagree
    /// about where a row is; guarded on the window, so a click into the blank space under a
    /// short queue moves nothing.
    pub(crate) fn queue_stop_at_row(&self, y: u16) -> Option<usize> {
        let y = usize::from(y).checked_sub(self.queue_pane_top)?;
        if y >= self.pane_room {
            return None;
        }
        let pane_row = y + self.pane_scroll;
        self.queue_stop_rows.iter().position(|r| *r == pane_row)
    }

    /// **The verdict on one entry, as the wire spells it**, or `None` when nobody has asked.
    ///
    /// `None` is *no review row at all* and `Some` with `decision: None` is *asked and not
    /// answered*: the pane draws them differently, because the first is a queue nobody has
    /// looked at and the second is a queue that is being looked at now.
    pub(crate) fn review_of(
        &self,
        entry_id: &str,
    ) -> Option<&letibot_sessionlog::event::MergeReview> {
        self.merge_reviews.iter().find(|r| r.entry_id == entry_id)
    }

    /// **The visible slice of a pane**, and the two numbers the scroll keys need.
    ///
    /// Clamped here rather than at the keypress: the key handler does not know
    /// how tall the terminal is or how many rows the pane has, and a scroll
    /// clamped against a stale height scrolls past the end and shows a blank
    /// screen the operator has to page back from.
    pub(crate) fn pane_window(&mut self, rows: Vec<String>, room: usize) -> Vec<String> {
        self.pane_len = rows.len();
        self.pane_room = room;
        // The last screenful is the furthest anything scrolls: past that is
        // blank rows, which is not a place to be.
        let max = rows.len().saturating_sub(room);
        self.pane_scroll = self.pane_scroll.min(max);
        rows.into_iter().skip(self.pane_scroll).take(room).collect()
    }

    /// Keep the cursor on screen after an arrow moved it.
    ///
    /// `row` is the cursor's index among the pane's rows. Called by the panes
    /// that have a cursor, after they move it: an arrow that walks the selection
    /// out of the window otherwise looks like a key that does nothing.
    pub(crate) fn scroll_into_view(&mut self, row: usize) {
        if self.pane_room == 0 {
            return;
        }
        if row < self.pane_scroll {
            self.pane_scroll = row;
        } else if row >= self.pane_scroll + self.pane_room {
            self.pane_scroll = row + 1 - self.pane_room;
        }
    }

    /// The job-output view's paging. `forward` asks for the page after the one on
    /// screen — or re-reads the last page when the end is already here, because a
    /// running job appends and that is how you see what it has written since.
    /// `!forward` walks back the way forward came, and does nothing at the front of
    /// the log, where there is no page before the first byte.
    ///
    /// The `back` stack lives on the head because the **page size is the daemon's**:
    /// the head remembers the offsets it was given rather than recomputing a window
    /// it does not size — the same reason `next` arrives on the event.
    pub(crate) fn job_out_page(&mut self, forward: bool) -> Option<Action> {
        let v = self.job_out.as_mut()?;
        let offset = if forward {
            let at = v.from;
            if v.next.is_some() {
                v.back.push(at);
            }
            v.next.unwrap_or(at)
        } else {
            v.back.pop()?
        };
        v.loading = true;
        Some(Action::ReadJobOutput {
            job: v.job.clone(),
            offset,
        })
    }
}

/// One tool call as the head watches it happen.
///
/// A tuple until now, and the three things it did not keep are the three a person
/// looking at a running call wants: **how long it has been going**, **what it
/// last said**, and **how much came out**. All three were derivable —
/// `Envelope::ts` is on every event and `ToolProgress { note }` was being read and
/// dropped — and none of them had anywhere to go while a call rendered as one
/// A subagent this session spawned, as the latest `Subagent` event reported it.
///
/// **The event is durable, and a head that was attached when it happened can replay it —
/// but a SNAPSHOT does not carry it**, and a head that attached after the spawn has no
/// event to fold at all. So a row has two possible sources and they are joined in one
/// place: this shape as the live event reported it, and the same fields as far as the
/// daemon's own session list can supply them ([`App::fold_subagents`]), which is what a
/// late head and every switch rebuilds the tree from.
#[derive(Debug, Clone)]
pub(crate) struct SubagentState {
    pub(crate) session_id: String,
    /// `opening` | `running` | `done` | `failed`, **or empty**, which is a row rebuilt from the
    /// daemon's session list for a child this head never watched: that list says whether a turn
    /// is generating in the session and nothing about how a settled one ended, so an empty word
    /// draws as `[?] state unknown` rather than as a `done` nobody measured. See
    /// [`App::fold_subagents`].
    pub(crate) state: String,
    /// **A turn is generating in this child at this instant** — the daemon's session list's
    /// own word (`SessionStatus::running`: *"a turn is generating in this session at this
    /// instant"*), and a measurement of NOW rather than of a life.
    ///
    /// **Kept BESIDE [`SubagentState::state`] and not folded into it**, which is the whole of
    /// this field's reason. The two sources spell the same word — the event's `running` and the
    /// list's `running` — and they mean different things: the event publishes its `running`
    /// **once**, when the child's harness is open, and it is a lifecycle word (*this child is
    /// up*), while the list's `running` is *a turn is generating in it right now*.
    /// [`App::fold_subagents`] merged the two, so a child that had merely stopped between
    /// turns went back to looking un-started: it left the count above the composer and moved
    /// into the `finished` group beside children that had actually ended. The operator
    /// measured exactly that on 2026-10-06 — the count reading `4, 2, 3, 1` over children
    /// that were alive throughout, one of them parked on its own background job with two
    /// commits already behind it.
    pub(crate) generating: bool,
    /// **The legacy field, and the pre-`task` fallback**: the subtask's first line on the
    /// opening states, and the child's answer's first line once it has finished. A new
    /// row reads [`SubagentState::task`]; this is here so a daemon older than that field
    /// still draws what it always did.
    pub(crate) prompt: String,
    pub(crate) role: String,
    /// **The subtask in full, from the event's `task`.** Empty against a daemon that
    /// predates the field, and the pane then falls back to `prompt`.
    pub(crate) task: String,
    /// **The model this child runs on**, from the event's `model` — `local`, or
    /// `PROVIDER/MODEL`. Empty when the child inherited its parent's model, which is the
    /// default: the pane then draws no model clause rather than claiming one.
    pub(crate) model: String,
    /// **The child's answer's first line**, `Some` only once it has finished — the
    /// subtitle, kept apart from the row so a completion cannot be mistaken for the
    /// question.
    pub(crate) answer: Option<String>,
    /// **When this child was spawned, for the pane's order** — the *"most recent agents must be on
    /// top"* the operator asked for on 2026-10-05.
    ///
    /// Two sources, and the daemon's wins: `SessionBrief::created_ms`, which is the session's own
    /// creation time and therefore the spawn, and — for a row this head watched appear before the
    /// list carried it — the `Subagent` event's own `ts`, which is when this head *heard* about the
    /// child rather than when it was made. A later finish event does not overwrite either: recency
    /// in this pane is *when the agent started*, so a child that has run for an hour does not jump
    /// above one spawned a minute ago for having ended last.
    ///
    /// **`0` is *not known*** — a replay, or a brief from a daemon that did not stamp the row — and
    /// it sorts LAST, below every row somebody can date. That is the honest place for it: a row
    /// nobody can order belongs at the bottom, not at the top pretending to be new.
    pub(crate) spawned_ms: u64,
}

impl SubagentState {
    /// **Whether this child is done** — the one thing the pane's two groups are made of, and
    /// now the one thing the count above the composer is made of too.
    ///
    /// Anything that is not `running` or `opening` is finished, and that includes a row the
    /// daemon's list rebuilt with no state word at all: a child this head did not watch, whose
    /// brief said only *a turn is not generating here* — which a running child would have
    /// contradicted.
    ///
    /// **A life, and not a measurement of an instant.** The word here is the one the daemon
    /// published for the child — `opening` at the spawn, `running` when its harness came up,
    /// `done` or `failed` at the end — and the only thing that may end it is that end. A child
    /// with a tool call in flight, a child between two rounds, a child parked on its own
    /// background job: all three are `running` and all three are alive, which is the standard
    /// the operator set in their own words — *"claude code for example shows subagent as alive
    /// until it finished turn with reply. not 'pausing it' on tool calls"*.
    ///
    /// **A stale `answer` does not enter into it.** A row can hold the answer of a turn that
    /// has since ended while the list says a second turn is generating in it right now, and
    /// that child is alive — see [`SubagentState::generating`] and the merge in
    /// [`App::fold_subagents`], which is where the two facts are kept apart.
    pub(crate) fn is_finished(&self) -> bool {
        !matches!(self.state.as_str(), "running" | "opening")
    }
}

/// **One row of the subagent pane** — the ONE enumeration the arrows, Enter, `p`, the drawn
/// `▸` and the scroll all read. A pane whose cursor comes from one list and whose rows come
/// from another is the defect leticl's `todos-stops` docstring names; see [`App::subagent_stops`].
/// rano's, because the pane that draws the rows is rano's: `Agent(i)` is a child in
/// [`App::subagents`] by index, `Finished` the folded group row.
pub(crate) use rano::agent::subagents::SubStop;

/// **One row of the jobs pane** — the ONE enumeration the arrows, Enter, the drawn `▸` and the
/// scroll all read, exactly as [`SubStop`] is for the subagents pane. See [`App::job_stops`].
/// rano's, because the pane that draws the rows is rano's and the keys must walk the list it
/// drew: `Job(i)` is a job in [`App::jobs`] by index, `Finished` the folded group row.
pub(crate) use rano::agent::jobs::JobStop;

#[derive(Debug, Clone)]
pub(crate) struct SubOut {
    pub(crate) session_id: String,
    /// One line per rendered row — **and which renderer produced them is [`SubOut::degraded`]'s
    /// business**: the session's own rows go through `item_lines`, the same one every row of the
    /// transcript uses, and the event-ring fallback goes through `subagent_out_lines`.
    pub(crate) lines: Vec<String>,
    /// **The daemon answered with the event ring rather than the session's rows.**
    ///
    /// `Peeked::snapshot` is `None` when the daemon predates the field, or when the peek did not ask
    /// for rows — so the fallback is necessary, and the operator's rule is that a fallback has to be
    /// *visible*: a degraded render and a plain one must not look alike, or a reader cannot tell
    /// whether they are looking at a session or at a list of its events.
    pub(crate) degraded: bool,
    /// Lines hidden off the bottom. Zero is "following the tail"; the pane draw
    /// clamps it, because only the draw knows the visible height.
    pub(crate) scroll: usize,
    /// Where the whole view was spilled, when it was written.
    pub(crate) spill: Option<String>,
    /// Events that fell off the daemon's scrollback before this read — the same
    /// disclosure a `Hello` makes, because a peek is a replay.
    ///
    /// **Kept across the rewrite and it had to be**: a snapshot is bounded by the daemon's view
    /// bounds exactly as the ring is by its cap, so a trimmed read must still say it was trimmed.
    /// A missing answer rendering as an empty one is `card::Outcome::Abstained`'s rule, one pane
    /// along.
    pub(crate) dropped: u64,
}

/// The output view the jobs pane's Enter opens: one job's retained output, as the
/// daemon measured it.
///
/// The bytes **and the offsets beside them**, because the pane draws its own header
/// and its own paging — see `SessionEvent::JobOutput` for why the read comes back as
/// an event with the numbers attached rather than as the `Warning` prose `/job`
/// replies with.
#[derive(Debug, Clone)]
pub(crate) struct JobOut {
    pub(crate) job: String,
    /// The daemon's word for where the job is — `running`, `exited 0`, … Empty
    /// until the first answer arrives.
    pub(crate) state: String,
    /// **Whether anything was ever executed for this job** (A.2, §11.6).
    ///
    /// The daemon's answer, not the head's inference: an empty window is the shape of
    /// *ran and wrote nothing* and of *never ran*, and until this field existed the head
    /// had one sentence for both — so a card whose header read `not run (could not join
    /// its scope)` went on to say `it wrote nothing at all` about a command that was
    /// never started.
    ///
    /// `false` until the answer arrives, which is the reading that renders what every
    /// daemon before this field produced.
    pub(crate) never_ran: bool,
    /// **Where this job's output actually went, when it did not come here** (R41) — the file the
    /// job's `redirect` named in the list, taken from the row Enter was pressed on.
    ///
    /// A redirected job's window is empty BY CONSTRUCTION: the daemon gave its bytes to the file,
    /// so an empty window here is the shape of *this pane cannot show it*, and `it wrote nothing at
    /// all` would be a lie about a job that wrote a build log. The operator: *"entering a job never
    /// shows me its output - whether it went to file or not"*.
    pub(crate) redirect: Option<String>,
    /// The offsets of the window actually loaded: `from..to` of `produced`.
    pub(crate) from: u64,
    pub(crate) to: u64,
    pub(crate) produced: u64,
    /// Bytes that fell off the front of the ring before this window. Disclosed
    /// because a window that starts mid-log is otherwise read as the job's start.
    pub(crate) dropped: u64,
    /// The window, already split into lines by the daemon so two heads cannot
    /// disagree about where a line ends.
    pub(crate) lines: Vec<String>,
    /// Where the daemon says the next page starts, or `None` when the end is here.
    pub(crate) next: Option<u64>,
    /// Offsets already loaded, newest last: `←` walks back the way `→` came. A
    /// stack rather than `from - page` arithmetic, because the page size is the
    /// daemon's choice and recomputing it here would be a second copy of it.
    pub(crate) back: Vec<u64>,
    /// Lines hidden off the bottom of the loaded window. Zero follows the tail;
    /// the draw clamps it, because only the draw knows the visible height.
    pub(crate) scroll: usize,
    /// True from the request until its answer: the overlay says so rather than
    /// showing an empty window it cannot yet fill.
    pub(crate) loading: bool,
    /// The daemon's refusal, when the read could not be answered — a job that fell
    /// out of the host's table between the listing and Enter. Shown in place of the
    /// window so the pane does not sit at `reading…` forever.
    pub(crate) error: Option<String>,
}

/// The whole view, spilled: the pane caps like a terminal, the file does not
/// cap. One name per subagent, overwritten on each read, so the path is stable
/// enough to open twice.
/// Where a head keeps files of its own: `$XDG_RUNTIME_DIR/letibot`, where the
/// socket already lives — per-user, mode 0700, tmpfs. Not `/tmp`: a subagent's
/// tool output is whatever the model read, and a world-readable file at a name
/// anyone can predict is both a disclosure and the classic symlink target. The
/// fallback is the shape the sudo shims use when there is no runtime dir.
pub(crate) fn head_runtime_dir() -> std::path::PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(d) => std::path::PathBuf::from(d).join("letibot"),
        None => std::env::temp_dir().join(format!("letibot-{}", unsafe { libc::getuid() })),
    }
}

pub(crate) fn spill_sub_out(session_id: &str, lines: &[String]) -> Option<String> {
    spill_sub_out_under(&head_runtime_dir(), session_id, lines)
}

pub(crate) fn spill_sub_out_under(
    dir: &std::path::Path,
    session_id: &str,
    lines: &[String],
) -> Option<String> {
    if std::fs::create_dir_all(dir).is_err() {
        return None;
    }
    let path = dir.join(format!("subagent-{session_id}.log"));
    let mut body = String::new();
    for l in lines {
        body.push_str(l);
        body.push('\n');
    }
    std::fs::write(&path, body)
        .ok()
        .map(|_| path.display().to_string())
}
