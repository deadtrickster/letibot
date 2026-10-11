//! `todo_write` — the session's plan, written by the model that runs it.
//!
//! The list is **session state**, not a file: [`Access::Session`] exists because
//! `docs/tool-survey.md` §1.4 found `todo_write` under-declared as `Read` in the
//! wild while it mutates session state, and this is the tool that declares it
//! right. It writes nothing the operator owns, so nobody adjudicates it — but it
//! is not read-class either, and the schema says so.
//!
//! The whole list every time, and **the whole list is an UPSERT: nothing a model
//! sends removes a row.** The operator's rule is *"regarding todo - only i should be
//! able to delete todo items. as a rule everything that ever created stays in
//! history"*, and *"so done items or canceled items should be kept."* So a row left
//! out of `todos` stays on the board, and the reply says so at the one call that
//! would have meant a removal. A row is retired by marking it `completed`, which is
//! a state and not a deletion.
//!
//! **A single row is addressed by quoting its own words**, in `update` (one of the
//! model's own rows) or `operator` (one of the operator's): a quote that resolves to
//! exactly one row moves that row's state and nothing else, and a quote that fits
//! none or two is refused with the candidates named. There is no id on the wire —
//! `TodoEntry` is `content`, `status`, `by` — so the words are the key, and the model
//! has them character for character from the reply.
//!
//! The harness persists the list and announces it, and [`TodoBoard`] is the seam
//! that makes that split possible without the tools crate knowing about stores or
//! logs: the tool mutates the board, the harness watches the version.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use letibot_tokencore::store::{
    TODO_TITLE_MAX, TodoBy, TodoCondition, TodoItem, TodoNeed, TodoStatus, todo_title,
};
use serde_json::{Value, json};

use crate::runtime::{Invocation, InvokeCtx, Tool};
use crate::schema::{Access, ToolSchema};

/// The session's todo list, shared between the tool that writes it and the
/// harness that persists and announces it.
pub struct TodoBoard {
    todos: Mutex<Vec<TodoItem>>,
    /// **The operator's rows, kept BESIDE the model's and never written by the `todo` tool.**
    ///
    /// The operator's ruling: *"the existing getter should return mine and yours, and the rest is
    /// also the same. the only difference is who created and that is it."* So this is one board with
    /// two halves and no second concept — `snapshot` is their union, which is what makes the pane,
    /// the NAG (`harness.rs`'s `nag_notice` → `unfinished_plan`) and the prompt the model reads all
    /// pick the operator's items up for free, with no code that knows they exist.
    ///
    /// **Separate, and not appended to `todos`, for one reason: every write the `todo` tool makes is
    /// scoped to the MODEL's own rows** — the whole list is what the model is saying about its own
    /// plan, and it neither reaches nor deletes anybody else's row — so an operator row left in that
    /// vector would be written, re-worded and `by`-stamped as the model's. Two halves, one getter.
    operator: Mutex<Vec<TodoItem>>,
    /// **A PARENT session's rows on THIS board — the third half, and the one that must never meet
    /// the model's own write.**
    ///
    /// The operator: *"yes - i want parent agents to be able to create todos for subagents.
    /// throught tree author - (Parent <session-id-of-parent>)"*. A child's board therefore has three
    /// authors, and the new one's write is scoped to the parent's own authorship
    /// ([`TodoBoard::upsert_parent`]), because *"send the whole list"* is a contract about **your
    /// own** rows — carried across sessions it would be a parent's three rows restating a child's
    /// plan and the operator's rows, which is precisely the collision the two-author rule on this
    /// board exists to prevent. Same reasoning as the operator's half, one author over: three halves,
    /// one getter.
    ///
    /// Single-author by construction — [`ChildTodos::upsert_child`] is the only writer, it stamps
    /// `by` itself from the calling session's id, and only a child's own parent reaches it — so the
    /// half holds rows by exactly one `Parent <id>` string.
    parent: Mutex<Vec<TodoItem>>,
    /// **The merge queue's rows on THIS board — the fourth half, and the only one that is a VIEW.**
    ///
    /// The operator's design: *"reviews are subagents managed by the main gatekeeper session which
    /// is hardcoded … it is a normal auto-driving session that treats review queue as todo list - so
    /// nags and all"*, and *"parents with children sending their homework to the teachers. School
    /// administration doesnt teach or checks homework. it just makes sure pencils are here and rooms
    /// are warm"*. So the daemon is the ADMINISTRATION — it fills the queue and keeps the worktrees
    /// — and the teacher is a session whose board is the queue. This half is that queue.
    ///
    /// **It is REPLACED, never accumulated, and that is the difference between it and the other
    /// three.** A row here is written from the store's own queue tables every pass and the whole
    /// half is set at once ([`TodoBoard::set_queue`]), so a row leaves the board when its entry
    /// leaves the queue — which is what *"an entry that reaches a verdict elsewhere takes its row
    /// off"* means, and something no UPSERT on the other three halves can express. It also means a
    /// write to this half is corrected by the next pass rather than becoming a second opinion.
    ///
    /// **AND THE STATE IS NOT HERE.** `MergeEntry::state` and `ReviewRecord::decision` in the store
    /// are the only place an entry's state lives; this half is what a SESSION can be nagged about.
    /// Anything that COUNTS the queue — a status line, the merge-queue pane, a head that is not this
    /// daemon — counts the store, and a reader that counted these rows would be counting a
    /// projection that is up to one pass old.
    queue: Mutex<Vec<TodoItem>>,
    version: AtomicU64,
}

impl TodoBoard {
    /// Start with a list — the store's, when this session was resumed, so the
    /// plan the model was working from is what it keeps working from.
    ///
    /// **AND THE STORE'S LIST IS THE UNION, so it is SPLIT BY AUTHOR here.** `flush_todos` persists
    /// `snapshot()`, which is all three halves in one list, and this used to put all of it into the
    /// model's half with the operator's half empty. A resumed session then had the operator's rows
    /// **inside the model's list** — so the model's own write would have taken them for its own, the
    /// nag would count them twice once the head re-pushed its own half on hello, and the pane would
    /// draw each of them twice. Sorting it once, here, is what makes *several halves, one getter* an
    /// invariant rather than a property of the callers: nothing that loads a list can get it wrong.
    /// A `Parent` row is the third case, and a resumed child is the one that has them: its board is
    /// restored from the store the same way, so what its parent told it survives the restart.
    pub fn new(initial: Vec<TodoItem>) -> Self {
        let mut mine = Vec::new();
        let mut told = Vec::new();
        let mut theirs = Vec::new();
        let mut queued = Vec::new();
        for t in initial {
            match t.by {
                TodoBy::Model => mine.push(t),
                TodoBy::Parent(_) => told.push(t),
                TodoBy::Operator => theirs.push(t),
                // **The queue's rows are restored like any other**, and that is safe precisely
                // because they are a view: whatever the store's todo table held survives a restart
                // until the daemon's next pass replaces the half from the queue's own tables.
                TodoBy::Queue => queued.push(t),
            }
        }
        TodoBoard {
            todos: Mutex::new(mine),
            parent: Mutex::new(told),
            operator: Mutex::new(theirs),
            queue: Mutex::new(queued),
            version: AtomicU64::new(0),
        }
    }

    /// **The operator's half, replaced wholesale.** A head sends its whole list on every change: it
    /// owns these rows, they are its own store's contents, and a delta protocol for a list of tens of
    /// items would be a second source of truth about them.
    ///
    /// **THE TITLE CAP IS NOT HERE, and that is the whole-list contract rather than an omission.**
    /// A replace carries every row the operator owns — including every row written before the cap
    /// existed — so a check here would refuse the operator's own `/todo done 2` because some old row
    /// is long, and a row older than the cap could never be closed. The rule binds the words a writer
    /// CREATES, and the operator's words are created in the HEAD
    /// (`tui/src/app/todos.rs::title_refusal`, asked by `/todo TEXT`, the card and the seed); the
    /// daemon's job on this door is to hold the half it was given. The model's half and a parent's
    /// are the other way round — `upsert_model`/`upsert_parent` ADD rows, so the check is there.
    ///
    /// Returns the new version, so the caller decides whether an announcement is owed exactly as it
    /// does for the model's half.
    pub fn set_operator(&self, todos: Vec<TodoItem>) -> u64 {
        *self.operator.lock().unwrap_or_else(|e| e.into_inner()) = todos;
        self.version.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// **Move the STATE of rows on this board — the model's own, the operator's, or both, in ONE
    /// all-or-nothing call.**
    ///
    /// `mine` addresses rows of the MODEL's half (the `update` field of `todo_write`), `theirs` the
    /// operator's (its `operator` field). The board is one list (*"the existing getter should
    /// return mine and yours"*), and this is the model's only way to dispose of a row it did not
    /// write: `TodoBy::Operator`'s doc claimed it could already act on the operator's half — *"the
    /// model can mark the operator's item done, and the nag in `harness.rs` picks it up like any
    /// other"* — and **it could not**, because `todo_write` replaced the model's half and
    /// `set_operator` is the HEAD's frame. A row the operator wrote could then be nagged about
    /// every idle turn and never disposed of — the wedge R48 describes, and the reason this exists.
    ///
    /// **The words are the key, and that is not a shortcut — there is no other key.** Measured:
    /// `TodoEntry` on the wire is `content`, `status`, `by` (protocol.rs:677), and the operator has
    /// ruled out a bump, so a row cannot be addressed by an id. What the model HAS is the exact
    /// string, because the reply and the nag hand it over character for character.
    ///
    /// **So a name that does not resolve exactly once is REFUSED, and the candidates are named.**
    /// This is the house rule for an ambiguous name — `ClientFrame`'s own precedent for an
    /// ambiguous option prefix is *"refused, with the candidates named"* — and it is what makes
    /// content-keying safe rather than sloppy: the guess this refuses to make is a row silently
    /// changing state under a model that quoted something else. [`resolve`] is the one spelling of
    /// it, shared by both halves.
    ///
    /// **ALL OR NOTHING, ACROSS BOTH HALVES.** Every quote in both lists resolves before any of
    /// them is applied, so a call that names one good row and one bad one moves neither — not the
    /// bad half's and not the good half's. A partial write would leave the model's plan updated and
    /// the operator's half not, which is the drift this whole board is arranged to prevent.
    ///
    /// **It sets STATE and never membership.** A model may mark the operator's row done — or put
    /// it back to pending — and may not delete it: the row is the operator's own words, and a model
    /// that misquotes must not be able to take them off the board. Membership is the head's, by
    /// `set_operator`, and the operator's own `/todo rm`.
    ///
    /// Returns how many rows on each half actually CHANGED — `(mine, theirs)` — and bumps the
    /// version only when that is not zero: a status set to what it already was is not an event, and
    /// announcing it would put a row on the wire and a write in the store for nothing.
    pub fn set_states_on_both_halves(
        &self,
        mine: &[(String, TodoStatus)],
        theirs: &[(String, TodoStatus)],
    ) -> Result<(usize, usize), String> {
        let mut model = self.todos.lock().unwrap_or_else(|e| e.into_inner());
        let mut operator = self.operator.lock().unwrap_or_else(|e| e.into_inner());
        // Resolve every quote on BOTH halves before either is touched — see ALL OR NOTHING.
        let my_plan = resolve_plan(&model, mine, "your own")?;
        let their_plan = resolve_plan(&operator, theirs, "the operator's")?;
        let mine_moved = apply_states(&mut model, my_plan);
        let theirs_moved = apply_states(&mut operator, their_plan);
        if mine_moved + theirs_moved > 0 {
            self.version.fetch_add(1, Ordering::SeqCst);
        }
        Ok((mine_moved, theirs_moved))
    }

    /// **The operator's half alone** — the shape a caller that has only their rows to move wants,
    /// and the one this board's own tests and the `operator` field had before `update` existed.
    pub fn set_operator_states(&self, updates: &[(String, TodoStatus)]) -> Result<usize, String> {
        self.set_states_on_both_halves(&[], updates)
            .map(|(_, theirs)| theirs)
    }

    /// **The OPERATOR's door for a row that is not theirs** — the whole of what
    /// `/todo postpone|resume <the row's words>` needs when the row is the MODEL's.
    ///
    /// **Why a second door exists.** [`TodoBoard::set_operator_states`] is the model's door and
    /// resolves inside the operator's half only, because a model may not add, drop or re-author the
    /// operator's rows. Nothing resolved the other way: a row the MODEL wrote that waits on a
    /// person's hand — *"operator: paste the token"* — could be neither silenced by the model
    /// (`todo_write` refuses `postponed`, deliberately: a model that could silence its own plan
    /// could abandon it) nor named by the operator's verb, which numbered their own rows. So it sat
    /// there being restated. MEASURED: fourteen turns on one true sentence.
    ///
    /// **The model's half only, and that is the boundary rather than an omission.** A parent's row
    /// is its author's to retire — [`TodoBoard::upsert_parent`] owns both its state and its edges,
    /// and would take a state written here straight back on the parent's next write — so the head
    /// refuses one by name instead of writing a silence that evaporates.
    ///
    /// **It sets STATE and never membership**, which is `set_operator_states`'s rule with the
    /// halves swapped: the operator can set a model's row aside and can never add, drop or re-word
    /// one. The words are the key and a name that does not resolve exactly once is refused with the
    /// candidates named, so nothing here can move a row nobody meant.
    ///
    /// Returns how many rows actually CHANGED, and bumps the version only when that is not zero —
    /// the same rule the other two writers keep, so a re-send of an unchanged board is not an event.
    pub fn set_states(&self, updates: &[(String, TodoStatus)]) -> Result<usize, String> {
        let mut half = self.todos.lock().unwrap_or_else(|e| e.into_inner());
        // Resolve every update BEFORE any is applied — see `set_operator_states` on ALL OR NOTHING:
        // a call that names one good row and one bad one moves neither.
        let mut plan: Vec<(usize, TodoStatus)> = Vec::new();
        for (asked, status) in updates {
            let want = asked.trim();
            let hits: Vec<usize> = half
                .iter()
                .enumerate()
                .filter(|(_, t)| t.content.trim() == want)
                .map(|(i, _)| i)
                .collect();
            match hits.as_slice() {
                [one] => plan.push((*one, *status)),
                [] => {
                    return Err(format!(
                        "no row of the model's says `{want}` — it may have rewritten its plan since \
                         you named it, and a name that matches nothing is not a row to move."
                    ));
                }
                _ => {
                    return Err(format!(
                        "`{want}` is TWO of the model's rows, so which one changes is not something \
                         this can know. Quote the one you mean."
                    ));
                }
            }
        }
        let mut changed = 0usize;
        for (i, status) in plan {
            if half[i].status != status {
                half[i].status = status;
                changed += 1;
            }
        }
        if changed > 0 {
            self.version.fetch_add(1, Ordering::SeqCst);
        }
        Ok(changed)
    }

    /// The operator's half alone, for a caller that needs to tell the two apart.
    pub fn operator_snapshot(&self) -> Vec<TodoItem> {
        self.operator
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// **The rows a parent has put on this board, as a sibling reader sees them** — the child
    /// cannot act on them (no field of `todo_write` aims at this half), but the tests and the
    /// daemon-side resolver both need to say what survived.
    pub fn parent_snapshot(&self) -> Vec<TodoItem> {
        self.parent
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// **The merge queue's rows on this board, as they stand** — the fourth half, read on its own.
    ///
    /// The half a caller reads when it wants *the queue* rather than *the board*: the daemon that
    /// filled it does not need this (it has the rows it wrote), and what does is a test or a reader
    /// asking whether a session is being nagged about the queue, and about what.
    pub fn queue_snapshot(&self) -> Vec<TodoItem> {
        self.queue.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// **The queue's half, SET rather than merged — the one writer of the fourth half, and the one
    /// door on this board that can take a row off.**
    ///
    /// The operator's ruling for the other three halves is *"only i should be able to delete todo
    /// items. as a rule everything that ever created stays in history"*, and this is not an
    /// exception to it: a queue row is not a record of work somebody decided to do, it is the queue
    /// SEEN from a session, and the record of it — the entry, its states, its verdict, its history —
    /// is the store's own tables. So the half is replaced wholesale from the store, and a row that
    /// leaves the queue leaves the board.
    ///
    /// **It is not the model's, the operator's or a parent's to write**, which is why this is a door
    /// of its own and not a fourth arm of `upsert_model`: a session that could add, drop or restate a
    /// queue row could close a review nobody made. `todo_write` has no field that reaches this half,
    /// `/todo rm N` numbers the operator's own rows, and a head never sends it — the only caller is
    /// the daemon that just read the queue.
    ///
    /// **And the title cap is not this half's business either**, for that same reason: these words
    /// are a VIEW of the store's own queue entries rather than anybody's title — a branch, a
    /// project, a state — and the daemon that writes them is not the writer the cap binds.
    ///
    /// Returns how many rows the half now holds, and bumps the version only when the rows actually
    /// CHANGED — the rule every other writer here keeps, so a queue that has not moved does not cost
    /// the store a write or the pane a publish on every pass.
    pub fn set_queue(&self, rows: Vec<TodoItem>) -> usize {
        let n = rows.len();
        let mut half = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        // **Same rows, same order, same states: not an event.** The daemon writes this half on every
        // pass, and a publish per pass would put a `TodosUpdated` on every head twice a minute for a
        // queue that is standing still. `TodoItem` is `PartialEq`, so this is the whole reading.
        if *half != rows {
            *half = rows;
            self.version.fetch_add(1, Ordering::SeqCst);
        }
        n
    }

    /// **A PARENT's write: an UPSERT scoped to the parent's own authorship** — the shape the model's
    /// own half takes too, and for the same reason.
    ///
    /// The whole of the safety here is WHAT IT DOES NOT TOUCH, so read this twice. A parent's list is
    /// a statement about the PARENT's own rows, and it does not reach across sessions: under the old
    /// wholesale-replace contract a parent sending its own three rows as the whole list would have
    /// deleted the child's plan and the operator's rows —
    /// one agent overwriting another agent's plan, the exact collision the author split on this
    /// board exists to prevent. So here the rows sent are the rows the parent is ADDING or
    /// state-updating AS THE PARENT, and nothing else moves:
    ///
    /// * a row whose trimmed text matches one of this author's existing rows **moves its state and
    ///   its edges** (the text is the name, and a re-worded row is a NEW row, the same rule
    ///   `set_operator_states` keeps for the operator's — a row the parent restates is restated,
    ///   edges included, because the parent's list is the whole of what it is saying about its own
    ///   rows);
    /// * a row that matches none is **appended** to this half, stamped `by` the author the CALLER
    ///   passed — never a `by` from the wire, which the tool refuses as an unknown field;
    /// * the child's rows, the operator's rows, and any other author's rows are **untouched**, and
    ///   there is **no delete**: a parent retires a row by marking it `completed`, the same way the
    ///   operator's rows are disposed of, and omission leaves a row exactly where it was.
    ///
    /// The author match is part of the scoping and not a nicety: this half is single-author by
    /// construction, and comparing anyway is what makes that a checked fact rather than an
    /// assumption a future writer could break.
    ///
    /// **A NEW row's title is capped here, and a row already on this half never is** — the check is
    /// [`check_titles`], the one spelling of the rule, and it runs BEFORE anything is pushed so a
    /// refusal costs the board nothing. It is here rather than in the tool because a `target` write
    /// reaches the CHILD's half through the daemon's resolver, and this is the only place that half
    /// is in hand.
    ///
    /// Returns how many rows were ADDED or CHANGED, and bumps the version only when that is not
    /// zero — the same rule `set_operator_states` keeps, so a re-send of an unchanged plan is not
    /// an event and does not cost the store a write or the pane a publish.
    ///
    /// `Err` is a refusal written to be shown to the model as it stands — the title cap, and only
    /// it: the resolver above this has already decided the target is the caller's own child.
    pub fn upsert_parent(
        &self,
        rows: &[(String, TodoStatus, Vec<TodoNeed>)],
        by: &TodoBy,
    ) -> Result<usize, String> {
        let mut half = self.parent.lock().unwrap_or_else(|e| e.into_inner());
        check_titles(&half, rows)?;
        let mut changed = 0usize;
        for (content, status, needs) in rows {
            let want = content.trim();
            if let Some(hit) = half
                .iter_mut()
                .find(|t| t.by == *by && t.content.trim() == want)
            {
                if hit.status != *status || &hit.needs != needs {
                    hit.status = *status;
                    hit.needs = needs.clone();
                    changed += 1;
                }
            } else {
                half.push(TodoItem {
                    content: content.to_string(),
                    status: *status,
                    by: by.clone(),
                    // A condition is the OPERATOR's own act — a job-conditioned row fires on a
                    // clock the parent does not own, and `[p]`/`postponed` is the operator's own
                    // state besides. A parent's row is plain work, asked for now.
                    when: None,
                    // **But its EDGES are the parent's to write**, and they are the one thing about
                    // a parent's row that is not plain work: a parent handing a child a plan hands
                    // it the ORDER too, and a child's check offering its own ready set is the whole
                    // reason the DAG exists on a child's board at all.
                    needs: needs.clone(),
                });
                changed += 1;
            }
        }
        if changed > 0 {
            self.version.fetch_add(1, Ordering::SeqCst);
        }
        Ok(changed)
    }

    /// **The model's own rows, as an UPSERT — and never a delete.**
    ///
    /// The operator's rule is *"only i should be able to delete todo items. as a rule everything
    /// that ever created stays in history"*, so there is no key on this board that removes a row:
    ///
    /// * a row whose trimmed text matches one of the model's own rows **moves that row's state and
    ///   its edges** — the text is the name, and a re-worded row is a NEW row, which is why the
    ///   reply prints the whole board rather than a diff;
    /// * a row that matches none is **appended** to this half;
    /// * a row of the model's that this call does not name is **left exactly where it was**, and the
    ///   tool says which ones, at the call that would have meant a removal under the old contract.
    ///
    /// **A name fits at most one row here by construction.** This is the only writer of the model's
    /// half and it never writes a second row under a name the half already holds, so the upsert needs
    /// no ambiguity refusal of [`resolve`]'s kind: there is nothing to choose between. (A store
    /// written by hand could hold two rows that trim alike, and the first is the one that moves.)
    ///
    /// **A STATUS THE MODEL CANNOT SPELL IS NOT THE MODEL'S TO CLEAR.** `Postponed` and `Cancelled`
    /// are the operator's own words: the model can neither set them nor write them back, and without
    /// the rule below the operator's silence would die on the model's very next write — the row comes
    /// back `pending` (the only sane word for work it is not doing) and the check starts asking again
    /// about a row the operator had settled. MEASURED as the shape of the defect this whole half
    /// exists for: a model's row that waits on a person's hand, silenced, then un-silenced by the
    /// plan's next revision.
    ///
    /// **A row the model ANSWERS is answered.** `Completed` is not a way of clearing the operator's
    /// word — it is the work being done — and there is nothing left to be silent about, so a row that
    /// comes back `completed` takes it.
    ///
    /// Returns how many rows were ADDED or CHANGED, and bumps the version only when that is not
    /// zero — [`TodoBoard::upsert_parent`]'s rule, so a re-send of an unchanged plan is not an event
    /// and does not cost the store a write or the pane a publish.
    pub fn upsert_model(&self, rows: &[(String, TodoStatus, Vec<TodoNeed>)]) -> usize {
        let mut half = self.todos.lock().unwrap_or_else(|e| e.into_inner());
        let mut changed = 0usize;
        for (content, status, needs) in rows {
            let want = content.trim();
            if let Some(hit) = half.iter_mut().find(|t| t.content.trim() == want) {
                // The operator's word survives unless the model ANSWERED the row.
                let kept = hit.status != *status
                    && !matches!(status, TodoStatus::Completed)
                    && matches!(hit.status, TodoStatus::Postponed | TodoStatus::Cancelled);
                let wanted = if kept { hit.status } else { *status };
                if hit.status != wanted || &hit.needs != needs {
                    hit.status = wanted;
                    hit.needs = needs.clone();
                    changed += 1;
                }
            } else {
                half.push(TodoItem {
                    content: content.clone(),
                    status: *status,
                    by: TodoBy::Model,
                    // **A condition is the OPERATOR's own act** and this tool has no `when` field:
                    // a job-conditioned row fires on a clock the model does not own. See
                    // `TodoItem::when`.
                    when: None,
                    // **But its EDGES are the model's to write**, and the whole list is the whole of
                    // what it is saying about them: a row restated is restated, edges included.
                    needs: needs.clone(),
                });
                changed += 1;
            }
        }
        if changed > 0 {
            self.version.fetch_add(1, Ordering::SeqCst);
        }
        changed
    }

    /// **The model's own half alone** — what a call STARTED with, which is what lets the tool name
    /// the rows that call left out (and so did not remove).
    pub fn model_snapshot(&self) -> Vec<TodoItem> {
        self.todos.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// **Clear the condition a row has been reported with** — the firing, as the board records it.
    ///
    /// The operator's own shape, and the reasoning is his: the row's condition is what makes it
    /// *due*, so a firing that left it in place would fire again on the next wake and again after
    /// every restart — and **a condition that never stops firing is indistinguishable from one that
    /// never fired.** Clearing it here means a daemon that comes back re-reads the row as ordinary
    /// open work: the store IS the memory, which is what makes this survive the restart the whole
    /// mechanism exists for.
    ///
    /// **Only the condition moves.** The text and the status are left exactly as they were, so what
    /// the reader sees still says what the row was waiting for, and the idle nag — which reads
    /// `unfinished_plan` off this board — picks it up like any other work the model owes somebody.
    ///
    /// The OPERATOR's half only, and not for symmetry: `todo_write` has no `when` field and writes
    /// `when: None` on every row it adds, so a condition on a model row is not something this build
    /// can write at all — and the caller here is the harness, whose rows these are.
    ///
    /// Returns how many rows changed, which is what the caller checks before announcing: `0` must
    /// not bump the version, or every wake would republish the board.
    pub fn consume_conditions(&self, handles: &[String]) -> u64 {
        let mut half = self.operator.lock().unwrap_or_else(|e| e.into_inner());
        let mut n = 0u64;
        for row in half.iter_mut() {
            let due = matches!(&row.when, Some(TodoCondition::Job { handle })
                if handles.iter().any(|h| h == handle));
            if due {
                row.when = None;
                n += 1;
            }
        }
        if n > 0 {
            self.version.fetch_add(1, Ordering::SeqCst);
        }
        n
    }

    /// The list as it stands.
    ///
    /// **THE UNION, and it is the whole of the feature.** Everything downstream reads this one
    /// getter — the pane's `Todos` reply, the model's own view of the plan, and the idle nag
    /// (`harness.rs`'s `nag_notice`, which asks `unfinished_plan` of exactly this) — so nothing
    /// else had to learn that the operator or a parent or the queue can write a row too.
    ///
    /// **The QUEUE's rows come first**, and that is a decision rather than an accident of the order
    /// the halves were added in: the nag asks about ONE row, drawn from the ready set in
    /// `open_priority`'s order (in progress first, then list order), and a session started to work
    /// the queue must not be able to bury a review nobody has judged under a plan of its own. Then
    /// the model's list, because it is the list the model has been working from; then what it was
    /// TOLD, parent before operator — the caller nearest first, the human last and on top — and each
    /// block is contiguous, the property `the_union_is_two_contiguous_blocks…` asserts for its two.
    pub fn snapshot(&self) -> Vec<TodoItem> {
        let mut out = self.queue.lock().unwrap_or_else(|e| e.into_inner()).clone();
        out.extend(
            self.todos
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .cloned(),
        );
        let told = self.parent.lock().unwrap_or_else(|e| e.into_inner());
        out.extend(told.iter().cloned());
        let operator = self.operator.lock().unwrap_or_else(|e| e.into_inner());
        out.extend(operator.iter().cloned());
        out
    }

    /// How many writes have landed. Version, not dirty-flag: a harness that
    /// persisted version 3 and then sees version 3 again does nothing, and two
    /// rapid writes that both need persisting are not lost the way a boolean
    /// cleared too early loses them.
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::SeqCst)
    }
}

/// **One quote, resolved against one half of the board — and the house rule for a name.**
///
/// `whose` is the phrase a refusal speaks with: `"your own"` or `"the operator's"`. Both readings
/// work in every sentence below, which is why there is one function rather than two.
///
/// Three outcomes and no fourth. Exactly one row is the answer. None is refused **with the rows that
/// half DOES hold**, so the model can quote one of them instead of guessing again. Two or more is
/// refused **with every candidate named** — by its place and its exact words — because which one
/// changes is not something this can know: the rows can differ only in whitespace (the match trims),
/// so the place is the only thing that tells them apart.
///
/// This is the same rule [`unmet_needs`] keeps for a `needs` name, and `set_states`' own doc says
/// why it is the rule: a guess here is a row silently changing state under a model that quoted
/// something else.
fn resolve(half: &[TodoItem], asked: &str, whose: &str) -> Result<usize, String> {
    let want = asked.trim();
    let hits: Vec<usize> = half
        .iter()
        .enumerate()
        .filter(|(_, t)| t.content.trim() == want)
        .map(|(i, _)| i)
        .collect();
    match hits.as_slice() {
        [one] => Ok(*one),
        [] if half.is_empty() => Err(format!(
            "no row of {whose} says `{want}` — there are none of them at all, so there is nothing to \
             move. `todos` adds rows; `update` and `operator` move the ones already on the board."
        )),
        [] => {
            let have: Vec<String> = half
                .iter()
                .enumerate()
                .map(|(i, t)| format!("{}. `{}`", i + 1, t.content.trim()))
                .collect();
            Err(format!(
                "no row of {whose} says `{want}`. {whose} rows are: {}",
                have.join(", ")
            ))
        }
        many => {
            let which: Vec<String> = many
                .iter()
                .map(|i| format!("row {} `{}`", i + 1, half[*i].content))
                .collect();
            Err(format!(
                "`{want}` is {} of {whose} rows — {} — so which one changes is not something this \
                 can know. Quote the one you mean exactly, or say in your reply which you left and \
                 why.",
                many.len(),
                which.join(", ")
            ))
        }
    }
}

/// Resolve a whole list of `{content, status}` quotes against one half, or refuse the call.
fn resolve_plan(
    half: &[TodoItem],
    updates: &[(String, TodoStatus)],
    whose: &str,
) -> Result<Vec<(usize, TodoStatus)>, String> {
    let mut plan = Vec::with_capacity(updates.len());
    for (asked, status) in updates {
        plan.push((resolve(half, asked, whose)?, *status));
    }
    Ok(plan)
}

/// Apply a resolved plan to one half. Returns how many rows actually CHANGED — a status set to what
/// it already said is not a move, which is what keeps a re-stated plan from being announced.
fn apply_states(half: &mut [TodoItem], plan: Vec<(usize, TodoStatus)>) -> usize {
    let mut changed = 0usize;
    for (at, status) in plan {
        if half[at].status != status {
            half[at].status = status;
            changed += 1;
        }
    }
    changed
}

/// **The open work as a PRIORITY QUEUE, in the order it should be served.**
///
/// The operator: *"priority queues - in progress items than not done, one-by-one."* Two bands:
///
///   1. **`in_progress`** — the model told the board it was doing this, so it is the thing it owes an
///      answer about first: finished, or admitted unfinished.
///   2. **`pending`** — work nobody has started.
///
/// **`Completed` is not in the queue at all**, which is what makes it a queue of work rather than a
/// copy of the list.
///
/// **Within a band the order is the list's own**, and the list is the model's rows before the
/// operator's (`TodoBoard::snapshot`). So a model that has started two things and been asked for one
/// more is pointed at its own in-progress work before the operator's, and at its own pending work
/// before theirs — the order it would choose itself, which is the point of not shuffling it.
/// **The idle check reads this order and then puts the operator's ready rows first** (R77,
/// [`the_row_to_name`]): which row is NAMED is the check's question, and this stays the plan's
/// order for every other reader.
///
/// A STABLE sort, deliberately: equal keys keep their list order, and that is what makes the
/// paragraph above true rather than aspirational.
///
/// **Why it is a function and not a `.find().or(first)` inside the message.** The message is one
/// caller and the pane is a second reader of the same list; a queue stated once can be tested as an
/// order — five items in, five items out in the serving order — where a find can only be tested by
/// reading one message and hoping.
///
/// **It is the ORDER, not the answer.** This says what the plan's open work is and how it is
/// ranked; WHICH of those rows can start is the graph's question, and `as_a_graph` is where it is
/// answered — the ready set is this order with the blocked rows taken out, so *in progress first,
/// then list order* survives the DAG unchanged.
pub fn open_priority(todos: &[TodoItem]) -> Vec<&TodoItem> {
    let mut open: Vec<&TodoItem> = todos
        .iter()
        // **A cancelled row is not work at all**, so it is not in the queue of work: the operator
        // struck it off, and the row survives only as the history that ruling is about. `Postponed`
        // is not excluded here and must not be — it is work still owed, and the narrowing that
        // keeps it out of the idle check is the daemon's own predicate (`the_check_may_ask_about`),
        // one level up, so that a postponed row still counts as part of the plan's size.
        .filter(|t| !matches!(t.status, TodoStatus::Completed | TodoStatus::Cancelled))
        .collect();
    open.sort_by_key(|t| match t.status {
        TodoStatus::InProgress => 0,
        _ => 1,
    });
    open
}

/// **How long a row that waits on a live child stays SILENT.**
///
/// The operator's directive, 2026-10-10, verbatim: *"add dependency to todos - with child ids so
/// you are not nagged till childs are running for the first 30 minutes of child life, later we will
/// think about better heruistics for you - the goal is that if your item depends on the child you
/// must be give a room to breath yet be steared to check the child is not stuck"*.
///
/// **The failure it was written against was MEASURED the same night**: a row was marked in progress
/// with a child working on it, and the idle check nagged it four times in twenty minutes. Every one
/// of those nags said the same thing, and none of them could tell *the child is working* from *the
/// child is stuck*.
///
/// **Thirty minutes is deliberately crude, and that is the instruction** — *"later we will think
/// about better heuristics"*. What a better one would read is named here rather than built: the
/// daemon holds how long the child has run and what its own session last did, so a rule that
/// watched a child's PROGRESS — rather than its age — is the next thing to think about. It is not
/// built because the operator said not to build it yet, and because a heuristic nobody has watched
/// fail on this box is a guess wearing a measurement's clothes.
pub const CHILD_ROOM_TO_BREATHE: Duration = Duration::from_secs(30 * 60);

/// **How long a child may show NOTHING before the notice stops calling it working.**
///
/// The other half of the same directive — *"be steared to check the child is not stuck"* — and it
/// is not a second talkability rule: [`CHILD_ROOM_TO_BREATHE`] is the only thing that decides
/// whether the check speaks, and this decides only which of the two sentences it says. The
/// operator's own session, 2026-10-10: three asks about one child produced the identical sentence,
/// *"still working"*, which says it has not died and nothing about whether it is stuck. So the
/// notice reports what the daemon can SEE ([`RunningChild`]) and this is the boundary between the
/// two forms — *it is working*, or *check whether it is stuck*.
///
/// **Two minutes, and the number is a reading of what lands on a child's own log**: a turn that is
/// generating publishes `TokensGenerated` and `Delta` continuously, a tool call that is still
/// producing publishes `ToolProgress`, and every round publishes a `TurnStarted`. What it does NOT
/// cover is said rather than hidden — a tool call that runs for ten minutes without writing
/// anything publishes nothing while it runs, so a child inside one reads as quiet. That is why the
/// evidence is printed beside the sentence instead of being folded into it: a reader can see
/// *"no turn is in flight, nothing for 12 minutes"* and judge it, which is exactly what the
/// liveness-only sentence denied them.
pub const CHILD_QUIET_ENOUGH_TO_LOOK: Duration = Duration::from_secs(2 * 60);

/// **A child this daemon is running, as the daemon can see it** — one entry of [`ChildSessions`].
///
/// **The AGE is what the room to breathe is measured against; the rest is what the check SAYS.**
/// That is the whole of why this is a struct and not a `Duration`: the directive has two halves —
/// *"you must be give a room to breath"* is `age`, and *"be steared to check the child is not
/// stuck"* is the rest — and a fact that carried only liveness could not serve the second. See
/// [`CHILD_QUIET_ENOUGH_TO_LOOK`] for what the numbers are read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunningChild {
    /// How long ago the daemon opened this child's session.
    pub age: Duration,
    /// **How long since anything last happened in it** — the timestamp of the last event on the
    /// child's OWN log, or `None` for a child that has published nothing at all yet.
    pub quiet_for: Option<Duration>,
    /// **Whether a turn is in flight in it at this instant.**
    ///
    /// **Filled from the daemon's own record of the turn, not from `SessionStatus::running`.**
    /// That flag is the ROUND's (`TurnStarted`/`TurnFinished` are published once per round, so it
    /// is false for the whole of a round's tool calls and for the boundary between two rounds),
    /// and a child executing a ten-minute build read as *no turn in flight* for ten minutes — which
    /// is the lie that made the check send its reader to `job_kill` about a child that was working.
    /// What fills this instead is `harnessd`'s `sessions::child_reading`: the journal's `running`
    /// state (or `TaskSlot::Running`), which is the daemon's record that this child's turn has
    /// started and has not settled, OR `Hub::turn_in_flight`, which sees the tool calls of a round
    /// whose generation has finished — the half the record cannot see for a turn that is not the
    /// child's first.
    ///
    /// Deliberately not called `stuck`: a turn in flight is true of a child that is generating,
    /// of one inside a tool call, and of one that has wedged mid-call, so on its own it decides
    /// nothing — it is the other half of the evidence, and `quiet_for` is what moves. What it does
    /// decide is the difference between a child that needs LOOKING at and a child that needs
    /// ASKING: see [`ChildDoing`].
    pub working: bool,
}

/// **What the daemon knows about the children a plan waits on** — the fact a BOARD cannot hold,
/// handed in by the caller.
///
/// [`unmet_needs`] is a pure function over the board, and a child's liveness and age are not on the
/// board: the daemon knows them — its task journal records the spawn and the finish, its session
/// registry holds when the session was opened and what its hub last published — and this crate
/// knows none of it. So the caller takes a SNAPSHOT and passes it.
///
/// **A value rather than a live query, and that is the choice that keeps this file testable.** Every
/// test below builds one, and the cases the operator asked for — a young child, an old one, one
/// that has finished — are three maps rather than three daemons.
///
/// **A child that is not in here is not running**, and that is an answer rather than a gap: it is
/// the reading [`TodoCondition::Job`] already gives a handle it does not know — *"a job that is not
/// running — including one this session has never heard of, which is what a handle looks like after
/// a restart — is a job that ended"* — and the alternative is an edge nothing can ever answer. A
/// child of this daemon is a thread this daemon holds, so a daemon that has restarted has no
/// children left to be running: the two facts agree rather than merely coexisting.
#[derive(Debug, Clone, Default)]
pub struct ChildSessions {
    live: BTreeMap<String, RunningChild>,
}

impl ChildSessions {
    /// Nothing running — a reading with no daemon behind it, and the honest one for every board
    /// whose rows wait on rows.
    pub fn none() -> Self {
        Self::default()
    }

    /// **A child that is running HERE, with what the daemon can see about it.** A builder, so a
    /// caller states its whole snapshot in one expression and a test states one case per line.
    pub fn running(mut self, id: impl Into<String>, child: RunningChild) -> Self {
        self.live.insert(id.into(), child);
        self
    }

    /// **How long this child has been running**, or `None` when it is not running here — the one
    /// read [`unmet_needs`] and the check's narrowing both take.
    pub fn child(&self, id: &str) -> Option<&RunningChild> {
        self.live.get(id)
    }
}

/// **What the daemon knows about the JOBS a plan waits on** — the second fact a BOARD cannot hold,
/// handed in beside [`ChildSessions`].
///
/// A row may wait on a background job ([`TodoNeed::Job`]), and whether that job is still running is
/// on no board: the process table is the daemon's, and the store holds only what outlived it. So the
/// caller takes a SNAPSHOT and passes it — a value rather than a live query, for [`ChildSessions`]'
/// own reason: the three cases that matter (a job that is running, one that has ended, and a handle
/// nobody has heard of) are three values rather than three daemons.
///
/// **A handle that is not in here is not running, and that is the ANSWER rather than a gap.** It is
/// the reading [`TodoCondition::Job`] already gives a handle it does not know — *"a job that is not
/// running — including one this session has never heard of, which is what a handle looks like after
/// a restart — is a job that ended"* — and an edge into a job that ended is therefore MET: the row
/// that waited on it can start. The alternative is an edge nothing can ever answer, which is the
/// failure [`TodoNeed::Unknown`] exists to refuse; and it is the same rule
/// [`ChildSessions`] keeps for a child that is not in its map.
///
/// **Liveness is the whole of it, which is where this differs from [`ChildSessions`].** A child
/// carries its age and what it last showed because the check has to STEER the model at a child that
/// has gone quiet; a job needs no steering — its settlement is already on its way to the model
/// (`jobwatch` submits it as a turn of its own), so all the check has to know is whether the row's
/// blocker is still there.
#[derive(Debug, Clone, Default)]
pub struct RunningJobs {
    live: BTreeSet<String>,
}

impl RunningJobs {
    /// Nothing running — a reading with no daemon behind it, and the honest one for every board
    /// whose rows wait on rows.
    pub fn none() -> Self {
        Self::default()
    }

    /// **A job that is running HERE**, by the handle `bash background: true` and `job_list` print.
    /// A builder, so a caller states its whole snapshot in one expression and a test states one
    /// case per line.
    pub fn running(mut self, handle: impl Into<String>) -> Self {
        self.live.insert(handle.into());
        self
    }

    /// **Whether this handle is running here** — the one read [`unmet_needs`] and the check's
    /// narrowing both take.
    pub fn is_running(&self, handle: &str) -> bool {
        self.live.contains(handle)
    }
}

/// **Whether a row waits on a child that is still inside its room to breathe.**
///
/// A row whose need is a live child younger than [`CHILD_ROOM_TO_BREATHE`] is one the check may not
/// SPEAK about at all — not asked about, and not listed as blocked either. That is the operator's
/// *"not nagged till childs are running for the first 30 minutes of child life"*, and it is why the
/// fact lands here as well as in [`unmet_needs`]: `harnessd`'s `the_check_may_ask_about` narrows
/// the plan before the message is rendered, so a row filtered here never reaches [`blocked_line`]
/// and the nag is SILENT rather than a blocked line about a child that is doing what it was told.
///
/// **Any need of that shape makes the row quiet**, whatever else it waits on: the child is one
/// reason the row cannot start, and the one reason that is out of the plan's hands.
///
/// A caller that does not narrow gets the other half of the same design: [`unmet_needs`] still says
/// the child is running, because it is, and the sentence for it says so.
pub fn waits_on_a_child_with_room_to_breathe(row: &TodoItem, children: &ChildSessions) -> bool {
    row.needs.iter().any(|need| match need {
        TodoNeed::Child { id } => children
            .child(id)
            .is_some_and(|child| child.age < CHILD_ROOM_TO_BREATHE),
        _ => false,
    })
}

/// **Whether a row waits on a job that is still running.**
///
/// The job half of [`waits_on_a_child_with_room_to_breathe`], and it is the operator's own ask of
/// 2026-10-11: *"todo can be declared to be dependent on a subsession and on a job ... and that
/// should drastically decrease nags count"*, measured the same night — each of the operator's long
/// jobs had a row nagging while it ran, because there was no way to say *this waits on that job*.
///
/// **A job is silent for as long as it RUNS, where a child is silent only while it is YOUNG**, and
/// the difference is not an oversight. A child is a session the plan cannot see into, so past its
/// room to breathe the check has to say *look at the child* — that is the steering
/// [`CHILD_ROOM_TO_BREATHE`] exists for. A job needs no steering and no age: the daemon watches
/// every background job and submits its settlement as a turn of its own, so a row blocked on a
/// running job is a row whose answer is already on its way, and the only honest thing the check can
/// do while it runs is say nothing. **And the silence ends by itself**: the settlement is a turn, a
/// turn's end re-arms the idle clock, and the row is then asked about — no deadline of its own is
/// needed, which is why there is no job-side twin of [`when_a_young_child_grows_up`].
///
/// **Any need of that shape makes the row quiet**, whatever else it waits on — [`waits_on_a_child_with_room_to_breathe`]'s
/// rule one kind over: the job is one reason the row cannot start, and one the plan cannot move.
///
/// A caller that does not narrow gets the other half of the same design: [`unmet_needs`] still says
/// the job is running, because it is, and the sentence for it says so.
pub fn waits_on_a_running_job(row: &TodoItem, jobs: &RunningJobs) -> bool {
    row.needs.iter().any(|need| match need {
        TodoNeed::Job { handle } => jobs.is_running(handle),
        _ => false,
    })
}

/// **When a plan that is quiet only because a child is young becomes speakable** — how much of the
/// SHORTEST room to breathe is left among the children its rows wait on, or `None` when no row is
/// in that state.
///
/// **This is the clock half of the silence, and without it the silence has no end.** A clock is
/// armed by a turn, a board write or a prompt — none of which a child's AGE is — so a row that went
/// quiet at five minutes would stay quiet until somebody happened to do something, and the steering
/// the operator asked for (*"be steared to check the child is not stuck"*) would arrive only by
/// accident. What this answers is the deadline the silence itself has: the moment the first of
/// those children leaves its room to breathe.
///
/// It is the same crude rule read from the other side — no new heuristic, and no new number:
/// [`CHILD_ROOM_TO_BREATHE`] is the whole of it.
pub fn when_a_young_child_grows_up(
    rows: &[TodoItem],
    children: &ChildSessions,
) -> Option<Duration> {
    rows.iter()
        .flat_map(|row| row.needs.iter())
        .filter_map(|need| match need {
            TodoNeed::Child { id } => children.child(id).and_then(|child| {
                CHILD_ROOM_TO_BREATHE
                    .checked_sub(child.age)
                    .filter(|left| !left.is_zero())
            }),
            _ => None,
        })
        .min()
}

/// **A gap of time, in words** — `8 seconds`, `45 minutes`, `2 hours`.
///
/// The reader of these sentences is judging whether a child has been quiet for too long, and a raw
/// `2700s` is a number they have to divide first. Rounded DOWN to the coarsest unit that is not
/// zero, because a rounded-up `1 minute` about a child that moved forty seconds ago would be the
/// same class of overstatement the sentence exists to avoid.
fn human_gap(gap: Duration) -> String {
    let secs = gap.as_secs();
    match secs {
        0 => "less than a second".to_string(),
        1 => "1 second".to_string(),
        2..=119 => format!("{secs} seconds"),
        120..=7199 => format!("{} minutes", secs / 60),
        _ => format!("{} hours", secs / 3600),
    }
}

/// **The plan a turn ended without finishing**, as a message for the model, or `None`
/// when there is nothing to say — which is the common case and is meant to be.
///
/// The operator, on what this is for: *"i guess the expectation from harness can be like
/// this - that if model stops the turn while there are todos pending it gets respective
/// notification."*
///
/// **Why this is not a convenience.** A plan is a thing a model writes and then may
/// quietly abandon: the turn ends, the list still says `in_progress`, and nothing in the
/// loop says a word about it. The plan's only enforcement was the model's own attention,
/// which is exactly the thing that fails on a long session — so *the model forgot what it
/// was doing* was a whole class of failure the loop had no mechanism against. This makes
/// the list a contract at the one moment the contract can be honoured: the turn boundary.
///
/// **The trigger is PENDING WORK**, and that is the whole of the condition. The obvious
/// way to get this wrong is a notification that fires on every turn end regardless of the
/// list — which teaches the model to clear its todos to make the message stop, worse than
/// no check at all. An empty list, or one where everything is `Completed`, is silent.
///
/// **And it is answerable**, which is the other half of the same point: the model is told
/// what to do about it. *Do them*, *mark them done*, and *drop what you no longer mean to
/// do* are the three honest answers, and without the third the check is a loop a model
/// escapes by lying about its own statuses — the failure it exists to prevent. A model
/// that is deliberately stopping is told to say so, which is a fourth answer the harness
/// can read: it is the turn's own reply, and the next turn is a new decision.
///
/// [`TodoBoard::snapshot`] is the state, taken at the turn boundary rather than watched,
/// so a list written and finished inside one turn never produces a message.
///
/// **The rows handed here are the ones the caller may speak about.** This renders the queue it is
/// given and does not decide what the idle check is allowed to say — and the difference is a real
/// state, because a POSTPONED row persists and is deliberately not being asked for. So the
/// narrowing happens before the message (`harnessd`'s `the_plan_as_checked`, which the arming
/// decision reads through as well) rather than inside this function, which would then be two
/// answers to one question about what a plan is.
///
/// **And `board` is the WHOLE board, which is the second half of the same fact**: the rows the
/// check may speak about have had their DONE rows taken out of them, and a done row is exactly what
/// an edge is satisfied by. Two arguments rather than one because the narrowing is the caller's and
/// the resolution is this function's — see [`unfinished_plan_for`].
///
/// **And `children` is the third fact, and it is the caller's too.** A row that waits on a CHILD
/// resolves against something no board holds — whether that session is still running, and what it
/// has shown lately — so the daemon hands its snapshot in; [`ChildSessions`] says why it is a value
/// and [`unmet_needs`] says what is read from it. `ChildSessions::none()` is the honest reading for
/// a caller with no daemon behind it, and the only one that leaves a board of row-edges unchanged.
///
/// **And `jobs` is the fourth, one layer down from `children`.** A row that waits on a background
/// JOB resolves against the process table, which no board holds either, so the daemon hands that
/// snapshot in too — [`RunningJobs`] says what absence means and [`unmet_needs`] says what is read
/// from it. The two are separate arguments rather than one *live* value because they are two facts
/// with two readers: a child's age is what the check STEERS by, a job's liveness is all it has to
/// know, and a caller that has one and not the other is an ordinary caller (a session with a process
/// host and no subagents, and the reverse).
///
/// **The order is the operator's own**: *"dependent on a subsession and on a job"* — the child
/// first, the job second, everywhere in this file.
pub fn unfinished_plan(
    rows: &[TodoItem],
    board: &[TodoItem],
    children: &ChildSessions,
    jobs: &RunningJobs,
) -> Option<String> {
    unfinished_plan_for(rows, board, None, children, jobs).map(|(text, _)| text)
}

/// **The check's sentence, and the row it named** — the row handed back so the caller can remember
/// the CHOICE, which is what makes the question sticky.
///
/// **The choice must not drift under the agent.** The row is picked once and named again until it
/// stops being askable (done, or postponed by the operator) — so a row the MODEL adds elsewhere on
/// the plan cannot change which row this check is about. **A row the OPERATOR adds is the one
/// exception, and deliberately** (R77): their open, ready rows are named before the sticky choice,
/// because they are the person saying what they want done — see [`the_row_to_name`] for the order
/// and why. What a row becoming `InProgress` changes
/// is the QUESTION rather than the choice: *you were asked about this and it has not been started*
/// becomes *it is marked in progress and still open*, which is the model's own claim about itself
/// and the thing holding it to one row is for. That is also why the answer is a PAIR: the text alone
/// cannot tell the next caller which row to stick to.
///
/// `chosen` is the content of the row the last check named, or `None` for a plan's first check.
///
/// **THE READY SET, and no longer the head of the list.** A row is READY when every row it waits
/// for is done ([`TodoItem::needs`]), so what this check asks about is work that can actually
/// start: an old blocked row no longer heads the line for ever, and a row nobody can start is not
/// asked for at all — it is SAID, with what it waits for. The order inside the ready set is
/// [`open_priority`]'s, so *in progress first, then list order* is unchanged.
///
/// **The set is OFFERED and the ask is still ONE row** — the operator's two rulings, which look
/// opposed and are not: *"i think the nagger should mention only one todo at a time, so a model
/// will not be defocused"* and *"and since it is a DAG - multiple items can be offered - subagents
/// case"*. The ready set is the pool the check draws from, and the `in_progress` mark is how the
/// model CHOOSES inside it (only the model may set that mark; `postponed` is deliberately the
/// operator's alone, so a model cannot silence the check). The ASK is the row this check is holding
/// the agent to: an operator's ready row first, then the sticky `chosen` while it is still ready,
/// and the head of the ready set otherwise ([`the_row_to_name`]). The set is recomputed as the graph moves — a row completing makes others ready — and
/// the CHOICE is what sticks.
///
/// **A plan with no edges anywhere is read EXACTLY as it was before any of this existed**, and that
/// is not a special case in the code: nothing can be blocked and the ready set cannot be empty, so
/// neither of the two additions below can fire. The blocked line and the nothing-can-start sentence
/// are the only places this message knows there is a graph — which is what makes the compatibility
/// claim a consequence of the design rather than a promise kept by hand. **The same holds for a
/// board whose edges are all `Row`s and whose `children` and `jobs` are empty**, which is every
/// board in a runtime with no daemon behind it.
pub fn unfinished_plan_for(
    rows: &[TodoItem],
    board: &[TodoItem],
    chosen: Option<&str>,
    children: &ChildSessions,
    jobs: &RunningJobs,
) -> Option<(String, String)> {
    let PlanAsGraph {
        open,
        ready,
        blocked,
    } = as_a_graph(rows, board, children, jobs);
    let Some(next) = the_row_to_name(&ready, chosen) else {
        // **THE DEGENERATE CASE, and it is the one that has to be SAID out loud.** An unfinished
        // plan whose ready set is EMPTY — every row left waits on another — is not a plan to work,
        // it is a plan to UNBLOCK: something has to finish, or a need that names nothing has to be
        // corrected, before any of this can start. Silence here would read as *nothing to do*,
        // which is the one reading that is wrong; and asking about the head of the list anyway is
        // the head-of-line blocking this replaced, wearing the new field's clothes.
        //
        // The name handed back is EMPTY, which is the one string no row can have (`todo_write`
        // refuses an entry whose text trims to nothing): nothing is being held to, so the next
        // check draws its choice from whatever the graph has made ready by then.
        return (!blocked.is_empty()).then(|| (nothing_can_start(&blocked), String::new()));
    };
    // The count of what is BEHIND this one, because the model is entitled to know the plan is bigger
    // than the row it is being asked about — and that is exactly the fact that must not become a
    // list. **It is the OPEN rows and not the ready ones**: a blocked row is still work nobody has
    // done, and the count has always been the plan's size rather than the queue's.
    let left = open - 1;
    let rest = match left {
        0 => String::new(),
        1 => " (1 more open)".to_string(),
        n => format!(" ({n} more open)"),
    };
    // **WHO ASKED, and it is not a courtesy — it decides WHICH FIELD disposes of the row.** A row the
    // model wrote is moved with `update`, its text quoted exactly; a row the OPERATOR wrote is moved
    // with `operator`, the same way; and a row a PARENT wrote is one NO field of this tool may
    // dispose of — the author retires it — which the child has to be told rather than left to guess.
    // The nag is the one place the model hears about a row before acting on it, so leaving the author
    // out is how a model comes to rewrite its own plan at the row somebody else is waiting on.
    let who = match &next.by {
        TodoBy::Operator => "the operator's",
        // The author string itself (`Parent <session id>`), verbatim: a child reading its own
        // board must be able to tell what it decided from what it was told, and by whom.
        TodoBy::Parent(author) => author.as_str(),
        // **The queue is named as the queue**, because that is what it is: not a session, not the
        // person, and not the model's own plan. The row's state is the store's and the verdict is
        // what takes it off — which is the advice below, one line down.
        TodoBy::Queue => "the merge queue's",
        TodoBy::Model => "yours",
    };
    // **The operator's row the model has not been told about yet** (R77) — the one case the head
    // line changes for. *Told* is read from `chosen`: a check that named this row last time has
    // told the model, and a row the model has itself moved to in progress is one it already knows.
    // So the FIRST check that names an operator's row after they add it (or re-word it, which is a
    // new row by its words) says who put it there, and the ones after it name it like any other.
    let operator_added = next.by == TodoBy::Operator
        && next.status == TodoStatus::Pending
        && chosen != Some(next.content.as_str());
    let state = match (next.status, chosen == Some(next.content.as_str())) {
        // **A row the model marked in progress is a claim it made**, and the check holds it to that
        // claim rather than asking the same question it asked before the mark existed.
        (TodoStatus::InProgress, true) => {
            format!(" — {who}, and it is marked in progress and still open")
        }
        (TodoStatus::InProgress, false) => format!(" — {who}, and you had it in progress"),
        // The row this check already named, still not started: the question is the same one, and it
        // says so — which is what makes the repeat legible as a repeat rather than a new demand.
        (_, true) => format!(
            " — {who}, and you were asked about this one already and it has not been started"
        ),
        (_, false) => format!(" — {who}"),
    };
    // **AND THE VERBS DIFFER BY AUTHOR, for the same reason.** The model may mark its OWN row done —
    // `update`, quoting it — and may not do that to the operator's: their row is their words, so the
    // model moves its STATE with `operator` and says in its reply why it is not doing the work.
    // Neither verb removes a row, and the message names the field because a model that has to guess a
    // mechanism guesses wrong.
    // A parent's row is the third case and the line is deliberately harder than the operator's: the
    // parent is a live session that will read this child's reply, so the child's way out is to DO
    // the work and SAY it did — no field of this tool aims at the parent's half, and a child that
    // could silently close a parent's row could silently close work it was told to do.
    let advice = match &next.by {
        TodoBy::Operator => {
            "the operator asked for this one, so do it — or mark it done with `todo_write`'s \
             `operator` field, quoting the text above exactly. You cannot remove their row: if you \
             think it should not be done, say why in your reply."
        }
        TodoBy::Parent(_) => {
            "your parent asked for this one, so do it and say in your reply when it is done — \
             they retire the row themselves when they read you. You cannot remove or restate \
             their row: it is theirs, and if you think it should not be done, say why in your \
             reply."
        }
        // **The queue's rows are the one kind whose disposal is not a tool call at all**, and the
        // advice says so rather than naming a verb that does not exist: the verdict is what takes a
        // row off, and the row is replaced from the store on the daemon's next pass. It states the
        // row and what moves it, and commands nothing — a model told to *do this one* would reach
        // for a verb that has no door.
        TodoBy::Queue => {
            "this row is the merge queue's, not your plan's: it is one entry of this daemon's \
             review queue, written onto your board from the store, and it comes off when that \
             entry's review is ANSWERED. No field of `todo_write` reaches it — `update`, `todos` \
             and `operator` do not move it — and neither does anything you say in your reply: the \
             verdict is what takes it off."
        }
        TodoBy::Model => {
            // **"or drop it" was the old contract's verb and it went with it.** A row left out of a
            // `todo_write` STAYS on the board now, so the model retires one by marking it done — and
            // dropping a line of work is something it SAYS in its reply rather than something a call
            // does. The phrase `mark it done with \`todo_write\`` is kept whole on purpose:
            // `blocks.rs`'s notice fold matches on it, so the nag still folds to its item.
            "do this one, or mark it done with `todo_write`'s `update` field, quoting the text \
             above exactly — a row left out of `todo_write` STAYS on the board, so a line of work \
             you are dropping is something you say in your reply, not something a call does. If you \
             are stopping here deliberately, say why in your reply."
        }
    };
    // **AND WHAT CANNOT START IS SAID, not merely skipped.** A blocked row is invisible to the ask
    // — that is the whole point — so the model would otherwise learn nothing about the rows it
    // cannot be asked for, and a plan whose edges have gone stale would look like a shorter plan.
    let waiting = if blocked.is_empty() {
        String::new()
    } else {
        format!("\n{}", blocked_said(&blocked))
    };
    // **The head line says the operator added it, and the rest of the notice keeps its shape** —
    // the bullet, the advice and the waiting line are the same lines in the same order, because
    // the head's notice fold (`blocks.rs`) reads a `[todo check] ` head and `  - ` bullets, and the
    // advice still says to quote *the text above*, which the bullet is.
    let head = if operator_added {
        format!(
            "[todo check] the operator added: \"{}\" — this turn is finished and it is not done{rest}",
            next.content.trim(),
        )
    } else {
        format!("[todo check] this turn is finished and one item is not done{rest}")
    };
    Some((
        format!(
            "{head}:\n  - {}{state}\n{advice}{waiting}",
            next.content.trim(),
        ),
        next.content.clone(),
    ))
}

/// **Which READY row the check names** — the operator's first, then the sticky choice, then the
/// head of the ready set (R77).
///
/// The operator: *"i added a todo to pg-noop session, but it was placed at the end, and didnt
/// arrive the next nag"*. Measured in that session: their row was row 21 of 22, and a check that
/// names the sticky `chosen` row or the head of the ready set never reached it — it was only ever
/// part of *"(N more open)"*. So the order is three steps, and the first is the fix:
///
///   1. **The operator's open, ready rows** (`TodoBy::Operator`), in [`open_priority`]'s order —
///      and among them the one already named, so a second operator row does not pull the question
///      off the first. Their row is the person saying what they want done; a plan the model wrote
///      for itself does not outrank it, and neither does the model's own sticky choice.
///   2. **The row already asked about**, while it is still ready — the stickiness
///      [`unfinished_plan_for`] describes, now one step down. A row that has left the ready set
///      (done, set aside by the operator, or BLOCKED) is no longer a choice;
///      `the_check_may_ask_about` decided the first two, so this agrees with the clock by
///      construction rather than by a second rule.
///   3. **The head of the ready set.**
///
/// **A BLOCKED operator row is not named**, because it is not in `ready`: it is said on the
/// waiting line with what it waits for, exactly as any blocked row is. **One function, and the
/// only one**: the daemon's sessions and a subagent's own thread both reach it through
/// `Harness::nag_notice` → [`unfinished_plan_for`], and the arming decision through
/// [`unfinished_plan`], so the clock, the text and the child's board cannot rank differently.
fn the_row_to_name<'a>(ready: &[&'a TodoItem], chosen: Option<&str>) -> Option<&'a TodoItem> {
    let theirs = |row: &&TodoItem| row.by == TodoBy::Operator;
    let named = |row: &&TodoItem| chosen == Some(row.content.as_str());
    let ready = || ready.iter().copied();
    ready()
        .find(|row| theirs(row) && named(row))
        .or_else(|| ready().find(theirs))
        .or_else(|| ready().find(named))
        .or_else(|| ready().next())
}

/// **Why a row cannot start** — one need the board does not meet, and the reason it does not.
///
/// **Everything that is not a DONE row is unmet**, and these are the five ways that happens. The
/// rule is `TodoCondition`'s, learned there first: *a condition nobody can evaluate must never read
/// as met.* An edge that names nothing is not satisfied by the naming, an edge that names two rows
/// is not satisfied by the coincidence, and an edge of a kind this build cannot evaluate is not
/// satisfied by the reader's ignorance — a row silently starting on the strength of a name that
/// meant something else is exactly the failure the graph exists to make impossible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NeedFate {
    /// The row it names is on the board and is not `Completed`.
    NotDone,
    /// **The row it names is one the OPERATOR struck off.** Not `NotDone`: *"still open"* about a
    /// row the operator has decided against sends the reader to finish work nobody owes, which is
    /// the nag this check exists not to produce. The edge can never be met, so the honest thing to
    /// say is that the plan has to be re-worded — the model owns the need, and `todo_write` is how
    /// it drops one.
    StruckOff,
    /// The name matches no row on the board — a re-worded row, a deleted one, or a name that was
    /// never right. The common case, and the one the message has to make repairable.
    NoSuchRow,
    /// The name matches more than one row, so which one is meant is not knowable — the house rule
    /// [`TodoBoard::set_operator_states`] already keeps for an ambiguous name.
    Ambiguous,
    /// A [`TodoNeed`] kind this build cannot evaluate — see that enum's `Unknown` variant.
    UnknownKind,
    /// **A child this daemon is running** — the row cannot start until the child finishes, and the
    /// sentence says so *with what the daemon can see about the child*, because a notice that can
    /// report only liveness cannot steer anybody: see [`RunningChild`] and [`child_said`].
    ///
    /// **It carries the child's facts and not a verdict**, and the verdict is [`child_said`]'s —
    /// which is also why a YOUNG child can carry this fate and never be spoken about: the check's
    /// silence is `waits_on_a_child_with_room_to_breathe`'s, taken by the caller before this
    /// function is handed the rows, so what is here is the fact and the policy is one step out.
    ChildRunning(RunningChild),
    /// **A background job this session is running** — the row cannot start until it ends, ANY
    /// ending, and while it runs the idle check stays SILENT about the row
    /// ([`waits_on_a_running_job`]): what is here is the fate for a caller that did not narrow.
    ///
    /// **It carries no payload, and that difference from [`NeedFate::ChildRunning`] is the design
    /// rather than an economy.** A child's fate carries the child's facts because the check has to
    /// STEER — *look at the child, it has gone quiet* — and a job needs no steering at all: the
    /// daemon already watches it and delivers its settlement as a turn of its own. All a reader of
    /// this line has to know is that the blocker is still there and is not the plan's to move.
    JobRunning,
}

/// **The rows the check may speak about, read as a graph** — split by whether they can start.
struct PlanAsGraph<'a> {
    /// How many rows the check may speak about at all: the plan's size, which is what the message
    /// counts. A blocked row is still open work, so it counts here and not in `ready`.
    open: usize,
    /// The READY rows, in [`open_priority`]'s order.
    ready: Vec<&'a TodoItem>,
    /// The rest, each with what it waits for that the board does not give it.
    blocked: Vec<(&'a TodoItem, Vec<(String, NeedFate)>)>,
}

/// **The plan as a graph**: which of the check's rows can start, and what the rest wait on.
///
/// `rows` are the rows the check may speak about — `harnessd`'s `the_plan_as_checked`, the ONE
/// narrowing — and `board` is the WHOLE board, because an edge is met by a row that is DONE and a
/// done row is exactly what that narrowing removes. Two arguments rather than one because the
/// narrowing is the caller's and the resolution is this function's: a function that narrowed would
/// be a second answer to *what is this plan*, which is the failure the one-filter rule exists to
/// prevent.
///
/// **`children` is the same split one fact over**: a child's liveness is not on the board either,
/// so the caller hands its snapshot in beside the board — see [`ChildSessions`] — and `jobs` is the
/// second such fact, for the reason [`RunningJobs`] gives.
fn as_a_graph<'a>(
    rows: &'a [TodoItem],
    board: &[TodoItem],
    children: &ChildSessions,
    jobs: &RunningJobs,
) -> PlanAsGraph<'a> {
    let queue = open_priority(rows);
    let mut ready = Vec::new();
    let mut blocked = Vec::new();
    for row in queue.iter().copied() {
        let unmet = unmet_needs(board, row, children, jobs);
        if unmet.is_empty() {
            ready.push(row);
        } else {
            blocked.push((row, unmet));
        }
    }
    PlanAsGraph {
        open: queue.len(),
        ready,
        blocked,
    }
}

/// **What this row is waiting for that the board does not give it** — empty when the row is ready.
///
/// The name is matched against the row's `content` TRIMMED, which is the same key
/// [`TodoBoard::set_operator_states`] resolves the operator's rows by: the model quotes what the
/// nag handed it, and a pane that padded a row would otherwise break every edge into it.
///
/// **A `Child` need is answered by `children` and by nothing else.** The board cannot say whether a
/// session is running, so the caller's snapshot is the whole of the evidence — and a child that is
/// not in it is one that has finished, which is the answer the row waits for (see [`ChildSessions`]
/// for why absence is not a gap).
///
/// **A `Job` need is answered by `jobs`, and there ABSENCE IS THE ANSWER ITSELF.** The board cannot
/// say whether a process is running either, so a handle the snapshot does not hold is a job that
/// ENDED — which is the whole of what the row waits for — and the edge is therefore MET: nothing is
/// pushed and the row is READY. That is `TodoCondition::Job`'s ruling read as an edge instead of as
/// a clock (*"a job that is not running — including one this session has never heard of, which is
/// what a handle looks like after a restart — is a job that ended"*), and the two must not differ:
/// one handle, one answer, whether the row fires on the ending or waits for it.
fn unmet_needs(
    board: &[TodoItem],
    row: &TodoItem,
    children: &ChildSessions,
    jobs: &RunningJobs,
) -> Vec<(String, NeedFate)> {
    let mut out = Vec::new();
    for need in &row.needs {
        match need {
            TodoNeed::Row { content } => {
                let want = content.trim();
                let hits: Vec<&TodoItem> =
                    board.iter().filter(|t| t.content.trim() == want).collect();
                match hits.as_slice() {
                    // **The one answer that is MET.** A single row, and it is done.
                    [one] if one.status == TodoStatus::Completed => {}
                    // **A row the operator STRUCK OFF is not work that is coming.** It is not met —
                    // the work was never done — and it is not *still open* either, because that is
                    // an instruction to go and finish it and the operator has already said no. So
                    // it is its own fate, and its sentence says what to do about it: re-word the
                    // edge, which is a change the model can make with `todo_write`.
                    [one] if one.status == TodoStatus::Cancelled => {
                        out.push((content.clone(), NeedFate::StruckOff))
                    }
                    [_] => out.push((content.clone(), NeedFate::NotDone)),
                    [] => out.push((content.clone(), NeedFate::NoSuchRow)),
                    _ => out.push((content.clone(), NeedFate::Ambiguous)),
                }
            }
            // **A child the daemon is running is NOT met** — it has not finished, which is the
            // whole of what the row waits for — and what is pushed is the daemon's reading of it,
            // not a verdict. The id is the NAME the message prints, the same way a row's name is
            // its `content`: it is what `task_result` collects by, and the model has to be able to
            // quote it back.
            TodoNeed::Child { id } => {
                if let Some(child) = children.child(id) {
                    out.push((id.clone(), NeedFate::ChildRunning(*child)));
                }
            }
            // **A job the session is running is NOT met** — it has not ended, which is the whole of
            // what the row waits for — and the handle is the NAME the message prints, the same way
            // a child's id is: it is what `job_output` and `job_kill` are given, so the model has
            // to be able to quote it back. A handle that is not in the snapshot is a job that
            // ENDED, and nothing is pushed for it: see this function's own doc.
            TodoNeed::Job { handle } => {
                if jobs.is_running(handle) {
                    out.push((handle.clone(), NeedFate::JobRunning));
                }
            }
            TodoNeed::Unknown => out.push((String::new(), NeedFate::UnknownKind)),
        }
    }
    out
}

/// **One blocked row, as the message says it** — `` `deploy` waits on `run the tests` (still open) ``.
fn blocked_line(row: &TodoItem, unmet: &[(String, NeedFate)]) -> String {
    let waits: Vec<String> = unmet
        .iter()
        .map(|(name, fate)| match fate {
            NeedFate::NotDone => format!("`{name}` (still open)"),
            // **Said as the OPERATOR's act and not as a plan state**, because that is what the
            // reader has to act on: nobody is going to finish this row, and the fix is the model's
            // own — drop the need, or point it at work that can happen.
            NeedFate::StruckOff => format!(
                "`{name}` (struck off by the operator — this edge can never be met; re-word it with \
                 `todo_write`)"
            ),
            NeedFate::NoSuchRow => format!("`{name}` (no such row on this board)"),
            NeedFate::Ambiguous => format!("`{name}` (two rows on this board say that)"),
            NeedFate::UnknownKind => {
                "a dependency of a kind this build cannot evaluate".to_string()
            }
            // **A child is not named the way a row is**, and the difference is what the reader
            // has to do about it: a row's name is quoted back to this tool, a child's id is
            // handed to `task_result`, and the sentence therefore says which child and how long
            // it has been running before it says anything else.
            NeedFate::ChildRunning(child) => format!(
                "the child `{name}`, running {} — {}",
                human_gap(child.age),
                child_said(child)
            ),
            // **A job is named by its handle**, the one `job_list` and `job_output` are given, and
            // the sentence says the two things the reader needs and nothing else: the blocker is
            // still there, and it is not theirs to move — the settlement is already on its way.
            NeedFate::JobRunning => format!(
                "the job `{name}`, still running — its completion arrives as a turn of its own"
            ),
        })
        .collect();
    format!("`{}` waits on {}", row.content.trim(), waits.join(", "))
}

/// **What a live child is DOING, as the daemon can see it** — the three states the operator's
/// directive asks to be told apart, NAMED rather than left as evidence for a reader to judge.
///
/// The operator's table, 2026-10-10, and the reason the two booleans were not enough:
///
/// * **working** — a turn is in flight and the child's own log has moved inside
///   [`CHILD_QUIET_ENOUGH_TO_LOOK`]. Nothing to do but let it work.
/// * **between turns** — **no turn is in flight and the child is still here.** Its turn ended
///   (or none has started) and it is parked; the one thing that moves it is a word from its
///   parent, so the steering is `task_message` and NOT `job_kill`. This is the state the operator
///   measured and could not see: *"it was between turns … a turn has been started for it"* was
///   learned only by sending a message nothing said to send.
/// * **quiet** — a turn IS in flight and nothing has been published for longer than the window.
///   This is the only state the check may send a reader to *look* at, and the one `job_kill`
///   answers.
/// * **showing nothing yet** — no turn in flight and **nothing ever published in it**: the
///   daemon's honest answer for a child it has not heard from at all, which is not the same fact
///   as a child that has gone quiet. The tree's rule is to say that rather than fold it into one
///   of the two above.
///
/// **One judgement, so two doors cannot disagree about one child**: the idle plan-check's sentence
/// (`child_said`) and `task_result`'s (`builtins::task`) both read this, and the enum is the
/// vocabulary they share. Before it, the check said *"no turn is in flight"* about a child
/// mid-tool-call and sent its reader to `job_kill` about a child that only needed asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildDoing {
    /// A turn is in flight and the child's own log moved inside the window.
    Working,
    /// **No turn is in flight and the child is still here** — parked, and reachable by a message.
    BetweenTurns,
    /// A turn is in flight and nothing has been published for longer than the window.
    Quiet,
    /// Nothing has been published in it at all, so the daemon has no evidence either way.
    NoEvidence,
}

impl ChildDoing {
    /// **The state in the words both doors use** — one spelling, so a reader who has read the
    /// notice and then called `task_result` reads the same state and not two paraphrases of it.
    pub fn said(self) -> &'static str {
        match self {
            ChildDoing::Working => "working",
            ChildDoing::BetweenTurns => "between turns",
            ChildDoing::Quiet => "quiet",
            ChildDoing::NoEvidence => "showing nothing yet",
        }
    }
}

/// **Which of those states this reading is** — the whole judgement, in one place.
///
/// `working` is the daemon's own record that a turn is in flight in the child (see
/// [`RunningChild::working`]) and `quiet_for` is the age of the last event on the child's own log,
/// so the two axes are *is anything running* and *has anything moved* — the operator's table, and
/// nothing else. A reader that wants the evidence rather than the name takes [`child_evidence`].
pub fn child_doing(child: &RunningChild) -> ChildDoing {
    match (child.working, child.quiet_for) {
        // Moving: this is the state the check must NOT speak about.
        (true, Some(gap)) if gap < CHILD_QUIET_ENOUGH_TO_LOOK => ChildDoing::Working,
        // A turn is in flight and nothing has come out of it for longer than the window.
        (true, _) => ChildDoing::Quiet,
        // No turn in flight, and something HAS been published: the turn ended and the child is
        // parked. A child still opening reads the same way here, and the difference is the
        // journal's own state (`opening` versus `running`), which the caller that needs it holds —
        // what this can honestly say is that nothing is running in it.
        (false, Some(_)) => ChildDoing::BetweenTurns,
        (false, None) => ChildDoing::NoEvidence,
    }
}

/// **The evidence under the name** — what the daemon saw, in the one clause both doors print.
///
/// One function, because the notice's parenthesis and `task_result`'s sentence are the same
/// reading and a second rendering of it would be a second answer about one child.
pub fn child_evidence(child: &RunningChild) -> String {
    let turn = if child.working {
        "a turn is in flight"
    } else {
        "no turn is in flight"
    };
    let moved = match child.quiet_for {
        Some(gap) => format!("its last event was {} ago", human_gap(gap)),
        None => "nothing has been published in it".to_string(),
    };
    // The working arm reads as one parenthetical (`a turn is in flight, its last event …`), the
    // others as two clauses joined by `and`. Same facts, and the sentence keeps its shape.
    if child.working {
        format!("{turn}, {moved}")
    } else {
        format!("{turn} and {moved}")
    }
}

/// **What the check says about a child it is waiting on** — the evidence and the steering, in one
/// clause, because the operator's directive has both halves in one breath: *"you must be give a
/// room to breath yet be steared to check the child is not stuck"*.
///
/// **The evidence is what the daemon can SEE, and liveness is not enough of it.** *"Still
/// working"* is the sentence the operator's own session got three times about one child, and it
/// is the failure this clause exists to fix: it says the child has not died and nothing about
/// whether it is stuck. So both recorded facts are printed — how long since anything happened in
/// the child's own session, and whether a turn is in flight in it — and they are what moves when
/// a child works and stops when it stops.
///
/// **The steering follows the state, and there are three of them** ([`ChildDoing`]): a child whose
/// own log moved inside [`CHILD_QUIET_ENOUGH_TO_LOOK`] is called WORKING and the reader is told it
/// is alive rather than sent to look; a child with **no turn in flight** is BETWEEN TURNS, and the
/// move for that one is a word — `task_message` starts a turn for it, and `job_kill` would stop a
/// child that is doing nothing wrong; and only a child with a turn in flight and nothing published
/// for the window is *"check whether it is stuck"*, with both mechanisms named, because a model
/// that has to guess a mechanism guesses wrong.
///
/// **What this cannot tell apart is said rather than hidden**: a child inside a long tool call
/// that writes nothing publishes nothing while the call runs, so it reads as quiet. The evidence
/// beside the sentence is what a reader judges that by, and [`CHILD_QUIET_ENOUGH_TO_LOOK`] carries
/// the whole of the reasoning.
fn child_said(child: &RunningChild) -> String {
    match child_doing(child) {
        ChildDoing::Working => format!("it is working ({})", child_evidence(child)),
        // **The state that had no sentence.** A child whose turn has ended is not stuck and is not
        // working: it is parked, and the one door that reaches a parked child is the parent's
        // message. Sending its reader to `job_kill` was the defect — that stops a child that is
        // waiting to be asked, and the ask is one call.
        ChildDoing::BetweenTurns => format!(
            "{} — nothing is running in it, so it is between turns, and a word from you is what \
             starts one: `task_message` reaches a parked child, and `task_result` reads what it \
             answered last",
            child_evidence(child)
        ),
        ChildDoing::Quiet | ChildDoing::NoEvidence => format!(
            "{} — check whether it is stuck: `task_result` with its id says where it got to, and \
             `job_kill` stops a child that is wedged",
            child_evidence(child)
        ),
    }
}

/// **What the check says about the rows it cannot ask for**, in one line — so the model learns the
/// plan is not empty, it is STUCK, and on what.
///
/// **Three leads, because there are three kinds of stuck and they ask different things of the
/// reader.** A row waiting on another ROW waits on something the plan can move — finish it, or
/// correct a need that names nothing — and it gets *"cannot start yet"*, which is the sentence that
/// has always been here. A row waiting on a CHILD waits on something the plan cannot move at all,
/// and the operator's ruling is that it gets *check whether it is stuck* instead; one sentence for
/// both would make the second read as the first, which is exactly the nag the room to breathe exists
/// to avoid. A row waiting on a JOB waits on something nobody has to do anything about — the
/// settlement is already on its way — and it gets a lead of its own rather than the child's, because
/// *check whether it is stuck* about a job would send the reader to look at a process that is doing
/// exactly what it was started for. **A row with more than one kind of need goes with the furthest
/// one**, and the order of the three is the order of how little the plan can do about it: child,
/// then job, then row.
fn blocked_said(blocked: &[(&TodoItem, Vec<(String, NeedFate)>)]) -> String {
    let mut said: Vec<String> = Vec::new();
    for lead in [
        "cannot start yet",
        "waiting on a child that is still running",
        "waiting on a job that is still running",
    ] {
        let lines: Vec<String> = blocked
            .iter()
            .filter(|(_, unmet)| lead_of(unmet) == lead)
            .map(|(row, unmet)| blocked_line(row, unmet))
            .collect();
        if !lines.is_empty() {
            said.push(format!(
                "{} {lead}: {}.",
                rows_count(lines.len()),
                lines.join("; ")
            ));
        }
    }
    said.join(" ")
}

/// [`blocked_said`]'s classification, split out because a closure cannot be called by name in the
/// filter there — see that function's doc for what the order of the three means. A row goes with
/// the CHILD when it waits on one, then with the JOB, and only otherwise with the plain rows: what
/// the lead is for is the thing the plan is least able to answer.
fn lead_of(unmet: &[(String, NeedFate)]) -> &'static str {
    if unmet
        .iter()
        .any(|(_, fate)| matches!(fate, NeedFate::ChildRunning(_)))
    {
        "waiting on a child that is still running"
    } else if unmet
        .iter()
        .any(|(_, fate)| matches!(fate, NeedFate::JobRunning))
    {
        "waiting on a job that is still running"
    } else {
        "cannot start yet"
    }
}

/// `1 row` / `2 rows` — the count the leads above open with.
fn rows_count(n: usize) -> String {
    match n {
        1 => "1 row".to_string(),
        n => format!("{n} rows"),
    }
}

/// **The sentence for an unfinished plan with an EMPTY ready set** — every row left waits on
/// another, so there is nothing to work on and something to UNBLOCK.
///
/// This is the degenerate case the whole feature has to design deliberately. A plan that is stuck
/// is the state where the agent's next move is *finish a row something is waiting on*, or *correct
/// a need that names nothing* — not *start something*. Saying nothing would read as *nothing to
/// do*, which is the one reading that is wrong, and it is why this is a sentence rather than the
/// absence of one. A cycle lands here too, and it is said the same way: every row in it is waiting,
/// and the fix is the same one.
///
/// **And the way out is not one way out.** *"Unblock one of these"* is the whole of the advice when
/// the blockers are rows, and it is wrong when one of them is a CHILD: a child is outside the plan,
/// so nothing the model writes moves it — what that row asks for is a look at the child. A JOB is
/// the third case and it is not a way out at all: nothing has to be done about a job that is running,
/// because its completion arrives as a turn of its own — so what the reader is told is to leave it
/// alone, which is the sentence that keeps this from reading as a plan to repair. Each extra clause
/// is added only when such a row is here, which is the same rule the rest of this file keeps about
/// saying things a reader has no use for.
fn nothing_can_start(blocked: &[(&TodoItem, Vec<(String, NeedFate)>)]) -> String {
    let lines: Vec<String> = blocked
        .iter()
        .map(|(row, unmet)| format!("  - {}", blocked_line(row, unmet)))
        .collect();
    let any = |fate: fn(&NeedFate) -> bool| {
        blocked
            .iter()
            .any(|(_, unmet)| unmet.iter().any(|(_, f)| fate(f)))
    };
    let a_child_is_waiting = any(|f| matches!(f, NeedFate::ChildRunning(_)));
    let a_job_is_waiting = any(|f| matches!(f, NeedFate::JobRunning));
    let mut out = String::from(
        "unblock one of these rather than starting something new: finish a row they wait on, or \
         correct a need that names nothing on this board",
    );
    if a_child_is_waiting {
        out.push_str(
            " — and where what is waited on is a CHILD, the look is at the child and not at the \
             plan, because the plan cannot move it",
        );
    }
    if a_job_is_waiting {
        out.push_str(
            " — and where what is waited on is a JOB, there is nothing to unblock and nothing to \
             look at: its completion arrives as a turn of its own, and the row can start when it \
             does",
        );
    }
    out.push_str(". A plan where nothing can start is a plan to fix, not a plan to work.");
    format!(
        "[todo check] this turn is finished and NOTHING can start — every item left waits on \
         another:\n{}\n{out}",
        lines.join("\n")
    )
}

// `MAX_PLAN_LINES` is gone with the list it capped: the message names ONE item now, so there is no
// length of list to truncate. The operator's ruling — *"only one todo at a time, so a model will not
// be defocused"* — removes the thing that constant existed for.

/// **The daemon-side half of `todo_write`'s `target`: reaching a CHILD session's board.**
///
/// The tool owns THIS session's board; a child's board is another `Arc` held by the daemon, and
/// only the daemon can say which sessions are whose children — `parent_session_id` on the session
/// list — and which session is calling now. So the implementation is built PER SESSION with the
/// caller already bound, and decides EVERYTHING the safety of this feature turns on:
///
/// * whether the target names one of the CALLER's own children (full id or the short `…tail` form
///   the subagent pane shows), refused BY NAME otherwise — a session that could write a sibling's
///   or its parent's board is a session that can overwrite another agent's plan;
/// * the AUTHOR — `Parent <the caller's full session id>`, built daemon-side and never taken from
///   the call, so a model cannot claim an authorship it does not have — which is also why `caller`
///   is not a parameter: an author a tool could name is an author a tool could forge;
/// * the upsert itself ([`TodoBoard::upsert_parent`]) and the flush that makes it durable
///   (`put_todos` + `TodosUpdated` on the child's hub), so the rows are on the pane and in the
///   store the moment they are written, not at the child's next turn boundary.
///
/// Returns the child's whole board — the union — so the tool can render what the child now sees,
/// which is the parent's confirmation that the right rows survived beside the child's own.
pub trait ChildTodos: Send + Sync {
    /// Add or state-update ROWS on the named child's board, authored as the CALLER this
    /// implementation was built for. `Err` is the refusal, written to be shown to the model
    /// as it stands.
    fn upsert_child(
        &self,
        target: &str,
        rows: &[(String, TodoStatus, Vec<TodoNeed>)],
    ) -> Result<Vec<TodoItem>, String>;
}

/// The write tool. Holds the board; the harness holds the same `Arc`.
pub struct TodoWriteTool {
    board: Arc<TodoBoard>,
    /// The daemon's child-board resolver — `None` in a runtime with no daemon behind it, in which
    /// case a `target` write is refused by name rather than quietly aimed at this session's board.
    children: Option<Arc<dyn ChildTodos>>,
}

impl TodoWriteTool {
    pub fn new(board: Arc<TodoBoard>) -> Self {
        TodoWriteTool {
            board,
            children: None,
        }
    }

    /// Seat the tool with a way to reach CHILDREN's boards — the daemon's half of `target`. The
    /// board stays the session's own; nothing here can write it as anybody else.
    pub fn with_children(board: Arc<TodoBoard>, children: Arc<dyn ChildTodos>) -> Self {
        TodoWriteTool {
            board,
            children: Some(children),
        }
    }
}

impl Tool for TodoWriteTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "todo_write",
            "Write the session's todo list: one entry per step, the step being \
             worked on marked in_progress, finished steps marked completed. Send the \
             WHOLE list every time — and a row you leave out STAYS on the board: \
             nothing this tool sends removes a row, because only the operator \
             deletes, and only through their own `/todo rm`. Retire a row by marking \
             it `completed`. To mark ONE row without re-sending the list, name it in \
             `update`, quoting its `content` exactly.\n\n**A row is a TITLE, and a title is at most 135 \
             characters.** The todos pane draws a row at the width of the operator's terminal less the \
             gutter and the row's own mark, number and `— model`, which leaves 135 — so a longer title \
             is cut off at the pane's right edge and read by nobody, and a `todos` entry whose title is \
             over that is REFUSED by name, with nothing written. **The rest of a row goes after ` — ` \
             in the same `content`**: `title — the detail`. That part is the row's DETAIL, it is not \
             capped, and the pane draws it after the title — so put anything long there rather than in \
             the title: `Fix the flaky test — it fails when the store is busy; see the gate run`. A row \
             already on the board is never refused for its length: the cap binds the words a call \
             WRITES, so an old long row can still be restated, or moved by quoting it in `update`.\n\nA row the OPERATOR has POSTPONED comes \
             back marked `[p]`: it stays on the board, it is still theirs, and it is \
             not work you are being asked for — do not propose it again. `postponed` \
             is not a status you may send; setting one aside and lifting it again are \
             the operator's own acts.\n\n`operator` is for rows the OPERATOR \
             wrote — the ones the reply marks `— the operator's` — and changes their \
             STATE only: quote `content` EXACTLY as the reply shows it. A quote that \
             does not match exactly one of their rows is refused and nothing is \
             written, because a guessed row is a row changing state under you. You \
             cannot delete their row; if you think it should not be done, say so in \
             your reply.\n\n`update` moves ONE of YOUR OWN rows — the way to mark a \
             single row without re-sending the list. It takes `content` (the row's \
             own words, quoted exactly as the reply shows them) and `status`, and \
             sets the status and nothing else: it cannot add a row, cannot change \
             one's edges, and cannot remove one. A quote that matches no row is \
             refused with the rows you do have; one that matches more than one is \
             refused with the candidates named; and either refusal writes NOTHING — \
             not even the `todos` beside it.\n\n`target` names a session YOU spawned with `task` and \
             writes YOUR rows onto THAT session's board instead of your own. There the \
             whole-list contract is OFF: your `todos` are the rows you are adding or \
             updating as the parent — a row whose text matches one of yours there \
             moves its state, a new text is added — and the child's rows, the \
             operator's rows and every other author's rows are UNTOUCHED. Omitting \
             one of your rows leaves it on the child's board; there is no delete, so \
             retire a row by marking it completed. The child is told through the usual \
             todo nags, and rows you write there show as `Parent <your session id>`. \
             `target` and `operator` do not mix: name one child, your own rows only.\n\nA row \
              may WAIT ON other rows: `needs` names them by their exact `content`, and the row \
              cannot start until every row it names is `completed`. The plan is then a graph \
              rather than a queue — the check asks about what can actually start, and says what \
              cannot and what it is waiting for — so an order you mean should be written down \
              rather than implied by position. A name that matches no row, or two rows, blocks \
              the row that needs it and the check says so; a need that is satisfied is silent.\n\nA \
              row may also WAIT ON A CHILD you started with `task`: `needs` takes \
              {\"kind\": \"child\", \"id\": \"<the session id `task` handed back>\"}, and the row \
              cannot start until that child has finished. While the child is young the check is \
              SILENT about the row — you are given room to work on other things — and once it has \
              been running a while the check tells you where the child has got to and says to \
              look at it if it has gone quiet. Use it for a dispatch: a row that collects a \
              subagent's work is not work you can do yet.\n\nA row \
               may also WAIT ON A BACKGROUND JOB: `needs` takes \
               {\"kind\": \"job\", \"handle\": \"<the handle `bash` with `background: true`, or \
               `job_list`, printed>\"}, and the row cannot start until that job has ENDED, any \
               ending. While the job runs the check is SILENT about the row — its completion \
               arrives as a turn of its own, so there is nothing to watch and nothing to look at \
               — and the row is asked about once it is over. Use it for work that has to land \
               first: a row that pushes a branch waits on the job that tests it. This is an EDGE \
               and not a clock: a row that FIRES when a job ends is the operator's `when`, which \
               is not yours to write.",
            json!({
                "type": "object",
                "properties": {
                    "operator": {
                        "type": "array",
                        "description": "Rows the OPERATOR wrote, whose STATE you are \
                                        changing. Quote `content` exactly as the reply \
                                        shows it. Sets state and never membership: it \
                                        can neither add a row nor remove one.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "content": {"type": "string"},
                                "status": {
                                    "type": "string",
                                    "enum": ["pending", "in_progress", "completed"]
                                }
                            },
                            "required": ["content", "status"]
                        }
                    },
                    "update": {
                        "type": "array",
                        "description": "ONE OR MORE OF YOUR OWN ROWS, moved by quoting \
                                        each one's `content` exactly as the reply shows \
                                        it — the way to mark a single row without \
                                        re-sending `todos`. Sets `status` and nothing \
                                        else: it never adds a row, never changes one's \
                                        edges and never removes one. A quote that \
                                        matches no row of yours is refused with the \
                                        rows you do have, and one that matches two is \
                                        refused with both named; either refusal writes \
                                        nothing at all.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "content": {"type": "string"},
                                "status": {
                                    "type": "string",
                                    "enum": ["pending", "in_progress", "completed"]
                                }
                            },
                            "required": ["content", "status"]
                        }
                    },
                    "todos": {
                        "type": "array",
                        "description": "Your plan, in the order to do them. A row whose \
                                        trimmed text matches one you already wrote moves \
                                        that row's state (and its edges); a new text is \
                                        added; and a row you leave OUT stays on the board \
                                        — there is no delete. With `target`: the rows you \
                                        are adding to or state-updating on that child's \
                                        board — not a replace; the child's own rows and \
                                        everyone else's are untouched.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "content": {
                                    "type": "string",
                                    "description": "The step, as a TITLE of at most 135 \
                                                    characters. Anything longer goes after ` — ` \
                                                    in this same string — that part is the row's \
                                                    DETAIL, it is not capped, and the pane draws \
                                                    it after the title."
                                },
                                "status": {
                                    "type": "string",
                                    "enum": ["pending", "in_progress", "completed"]
                                },
                                "needs": {
                                    "type": "array",
                                    "items": {"type": "string"},
                                    "description": "What this row waits for. A plain string is \
                                                    the exact `content` of another row on this \
                                                    list that must be COMPLETED before this one \
                                                    can start. The tagged form \
                                                    {\"kind\": \"child\", \"id\": \"s-…\"} names a \
                                                    CHILD you started with `task` — the session id \
                                                    it handed back — and waits for that child to \
                                                    finish. {\"kind\": \"job\", \"handle\": \"j…\"} names a \
                                                     BACKGROUND JOB by the handle `job_list` \
                                                     prints, and waits for it to END, any ending — \
                                                     the check is silent about the row while the \
                                                     job runs. Optional, and empty is the ordinary \
                                                    row: a row that names nothing is ready as soon \
                                                    as it is open. A row name that matches no \
                                                    row, or two, blocks this row and the check says \
                                                    which one it could not place."
                                }
                            },
                            "required": ["content", "status"]
                        }
                    },
                    "target": {
                        "type": "string",
                        "description": "Write onto a CHILD session's board instead of \
                                        your own: the child's session id, full or the \
                                        short `…tail` form the subagent pane and \
                                        `task_result` show. Only a session YOU spawned \
                                        with `task` may be named; anything else — a \
                                        sibling, your own parent, yourself — is refused \
                                        by name. With `target`, `todos` is an upsert of \
                                        your own rows there (matched by exact text) \
                                        and NEVER a replace: the child's rows, the \
                                        operator's rows and other authors' rows are \
                                        untouched, and there is no delete."
                    }
                },
                "required": []
            }),
            Access::Session,
        )
    }

    fn invoke(&self, _ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        // **AN UNKNOWN TOP-LEVEL FIELD IS REFUSED BY NAME** — the same rule an entry's unknown
        // field keeps below, one level up. This call has three fields a model confuses (`todos`,
        // `update`, `operator`) and a fourth (`target`) that aims at another session's board, and a
        // field that is silently ignored is exactly how a wrong write becomes a quiet one.
        if let Some(obj) = args.as_object() {
            for key in obj.keys() {
                if !matches!(key.as_str(), "todos" | "update" | "operator" | "target") {
                    return Invocation::failed(
                        format!("`{key}` is not an argument of `todo_write`"),
                        "**nothing was written.** The arguments are `todos` (the whole list), \
                         `update` (one of YOUR rows, quoted), `operator` (one of the OPERATOR's \
                         rows, quoted) and `target` (a child's board).",
                    );
                }
            }
        }
        // **A MISSING `todos` IS NOT A MISSING ARGUMENT ANY MORE.** `update` and `operator` name a
        // row on their own, which is the whole point of `update`; what is refused is a call that
        // names no row by ANY of the three — see the block below the target path.
        let rows = match args.get("todos").and_then(|v| v.as_array()) {
            Some(list) => match parse_rows(list) {
                Ok(rows) => rows,
                Err(inv) => return inv,
            },
            None => Vec::new(),
        };
        // **THE TARGET PATH — a write onto a CHILD's board — runs before anything of this
        // session's own moves, and it never reaches `board` at all.**
        //
        // Everything below this block is the two-author contract on THIS board (wholesale
        // replace + the operator's state-only edits); none of it may leak across sessions, so
        // the block validates its own rows above (`parse_rows`, the same refusals), refuses the
        // `operator` field beside it (two boards' concerns in one call is a wrong write waiting
        // to happen), and hands the rows to the daemon-side resolver — which checks the target is
        // the CALLER's own child, stamps the author itself, upserts, and returns the child's
        // board for the reply. `Err` from it is already a refusal written for the model.
        if let Some(target) = args.get("target") {
            let refused = |what: String, why: &str| {
                Invocation::failed(
                    what,
                    "**nothing was written — not on any board.** ".to_string() + why,
                )
            };
            let Some(target) = target.as_str() else {
                return refused(
                    "`target` needs to be a session id".into(),
                    "send `target` as the child's session id — full, or the short `…tail` form the \
                     subagent pane shows — or leave it out to write your own board.",
                );
            };
            // **One call, one board.** `operator` and `update` both move a row on THIS session's
            // board and `target` writes onto a child's, so either of them beside `target` is a
            // wrong write waiting to happen.
            for field in ["operator", "update"] {
                if let Some(given) = args.get(field) {
                    return refused(
                        format!(
                            "`target` and `{field}` do not mix: this call named a child \
                             (`{target}`) and a `{field}` block ({given})"
                        ),
                        &format!(
                            "`{field}` moves a row on YOUR board; `target` writes your rows onto \
                             a CHILD's board. One call, one board — send them separately."
                        ),
                    );
                }
            }
            if rows.is_empty() {
                return refused(
                    "`todos` is empty and `target` names a child".into(),
                    "on a child's board your rows are ADDED or state-updated, never replaced, so \
                     an empty list has nothing to say — and there is no delete to mean by it. Name \
                     the rows you are adding, or mark one you already wrote there `completed`.",
                );
            }
            let Some(children) = &self.children else {
                return refused(
                    format!(
                        "`target` was given (`{target}`) but this session has no children's boards to address"
                    ),
                    "this runtime seats `todo_write` without a child-board resolver — a standalone \
                     or test runtime. Leave `target` out: the call writes your own board.",
                );
            };
            return match children.upsert_child(target, &rows) {
                // The resolver is built per session with the caller already bound, which is why
                // the author never comes from the wire — see `ChildTodos::upsert_child`'s doc.
                Ok(child_board) => Invocation::ok(format!(
                    "written onto {target}'s board as `Parent <your session id>` — your rows were \
                     added or state-updated as yours; the child's rows, the operator's rows and \
                     every other author's are untouched, and nothing was deleted.\n\n{}",
                    render(&child_board),
                )),
                Err(why) => Invocation::failed(
                    format!("`{target}` was not written: {why}"),
                    format!(
                        "a `target` must be a session THIS session spawned with `task`, and the rows \
                         you send are titles of at most {TODO_TITLE_MAX} characters (the part after \
                         ` — ` is the detail, which is not capped). Quote one of your children — the \
                         subagent pane (`ctrl-g`) and `task_result` list them — or leave `target` out \
                         to write your own board."
                    ),
                ),
            };
        }
        // **THE TITLE CAP, BEFORE ANYTHING MOVES.** The check is HERE and not in `upsert_model`
        // because this call is ALL OR NOTHING: `set_states_on_both_halves` below writes both halves,
        // and a list refused after it would leave the operator's rows moved and the plan not — the
        // drift `set_states_on_both_halves`' own doc is arranged against. The `target` path keeps
        // its copy inside `upsert_parent`, where the half that matters is the child's.
        //
        // `before` is read here, above the moves, and used twice: the check reads it, and the reply
        // names the rows this call left out against it. A status moved between those two readings
        // cannot change which rows were named — `omitted` compares contents.
        let before = self.board.model_snapshot();
        if let Err(why) = check_titles(&before, &rows) {
            return Invocation::failed(format!("nothing was written: {why}"), title_why());
        }
        let updates = match parse_moves(args.get("update"), "update") {
            Ok(v) => v,
            Err(inv) => return inv,
        };
        let ops = match parse_moves(args.get("operator"), "operator") {
            Ok(v) => v,
            Err(inv) => return inv,
        };
        // **A CALL THAT NAMES NO ROW BY ANY FIELD IS REFUSED.** `{"todos": []}` used to mean *clear
        // the board*; with no delete on this board it means nothing at all, and a call that says
        // nothing must not be read as one that did something. The three fields are the three ways to
        // name a row — `todos` is the whole list, `update` one of the model's own, `operator` one of
        // the operator's — and a call with none of them has nothing to say.
        if rows.is_empty() && updates.is_empty() && ops.is_empty() {
            return Invocation::failed(
                "this call names no rows",
                "**nothing was written**: there is no delete and no way to clear the board. \
                 `todos` is the whole list, `update` moves one of YOUR rows by quoting it, \
                 `operator` moves one of the operator's. Send one of the three — and to retire a \
                 row, mark it `completed` rather than leaving it out.",
            );
        }
        // **EVERY QUOTE RESOLVES BEFORE ANYTHING MOVES**, on both halves at once: a name that does
        // not resolve costs nothing at all — not the operator's rows, not the model's own, and not
        // the list beside them. `TodoBoard::set_states` is where that all-or-nothing lives; the
        // model's own list is an upsert and cannot refuse.
        let (moved_mine, moved_theirs) = match self.board.set_states_on_both_halves(&updates, &ops)
        {
            Ok(counts) => counts,
            Err(why) => {
                return Invocation::failed(
                    format!("nothing was moved: {why}"),
                    "**nothing was written** — not the operator's rows, not your own rows, and not \
                     your list. Quote a row's `content` exactly as the list below shows it, or \
                     leave the field out.",
                );
            }
        };
        // **WHAT THIS CALL STARTED WITH**, so the reply can say which rows it did NOT remove. The
        // model's half is an upsert now, and a model whose frozen prefix still carries the old
        // contract (*"omitting an entry removes it"*) has to be told — at the call that would have
        // meant a removal, and only when a row was actually left out — that the row is still there.
        // **ONLY WHEN `todos` WAS SENT.** A call that names one row with `update` leaves nothing
        // out — it never said what the list is — so this sentence belongs to the whole-list form
        // alone.
        let list_sent = args.get("todos").and_then(|v| v.as_array()).is_some();
        self.board.upsert_model(&rows);
        let omitted: Vec<String> = if list_sent {
            before
                .iter()
                .filter(|t| !rows.iter().any(|(c, _, _)| c.trim() == t.content.trim()))
                .map(|t| t.content.clone())
                .collect()
        } else {
            Vec::new()
        };
        // The list back to the model, as it now stands — so the next call is written against what
        // the pane shows, not against what the model believes it wrote.
        let mut out = render(&self.board.snapshot());
        if !updates.is_empty() {
            out.push_str(&format!(
                "\nof your own rows you named, {} changed status{}.\n",
                moved_mine,
                if moved_mine == updates.len() {
                    ""
                } else {
                    " (the rest already said that)"
                }
            ));
        }
        if !ops.is_empty() {
            out.push_str(&format!(
                "\nof the operator's rows you named, {} changed status{}.\n",
                moved_theirs,
                if moved_theirs == ops.len() {
                    ""
                } else {
                    " (the rest already said that)"
                }
            ));
        }
        if !omitted.is_empty() {
            out.push_str(&format!(
                "\n**{} of your rows were left out of this call and are still on the board**: {}. \
                 Nothing this tool sends removes a row — mark one `completed` when it is done, and \
                 say in your reply when you are dropping a line of work.\n",
                omitted.len(),
                omitted
                    .iter()
                    .map(|c| format!("`{c}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        Invocation::ok(out)
    }
}

/// **The title cap, in one place** — the check every write of a board row's WORDS passes.
///
/// The rule itself is [`letibot_tokencore::store::TODO_TITLE_MAX`]'s, and the reason it is 135 is
/// written there: the operator's terminal is 160 cells, the pane draws a row at 156, and the row's
/// own furniture — the mark, its number, `— model` — takes 21, so a longer title is cut at the
/// pane's right edge and read by nobody.
///
/// **It bounds what a call WRITES, and never what a row IS.** A row whose trimmed words are
/// already on this half is being RESTATED — the whole-list contract's ordinary case — so its words
/// are the board's and not this call's, and the cap has nothing to say about it. That is what keeps
/// a row written before this cap usable: an old long row can still be restated, and can still be
/// disposed of by quoting it in `update`/`operator`, because a quote is not a new title either. A
/// cap that refused those would make every row older than it immortal — the operator could not
/// close one and the model could not restate its own plan — which is a worse failure than the cut
/// this exists to prevent.
///
/// **All-or-nothing**: the whole list is checked before any of it is written, so a call carrying
/// one over-long row costs the board nothing at all — the rule `set_states_on_both_halves` keeps
/// for its quotes, one step earlier.
fn check_titles(
    existing: &[TodoItem],
    rows: &[(String, TodoStatus, Vec<TodoNeed>)],
) -> Result<(), String> {
    for (content, _, _) in rows {
        if existing.iter().any(|t| t.content.trim() == content.trim()) {
            continue;
        }
        let title = todo_title(content);
        let n = title.chars().count();
        if n > TODO_TITLE_MAX {
            return Err(format!(
                "`{title}` is {n} characters, and a title is at most {TODO_TITLE_MAX}"
            ));
        }
    }
    Ok(())
}

/// **What a title over the cap is told**, beside the refusal itself: the cap, where the number
/// comes from, and where the rest of the row goes.
///
/// One spelling, because both writers that can write a NEW row reach it — `todo_write`'s own
/// `todos` list, and a parent's `target` write — and a second sentence is how one of them ends up
/// telling a model something different about the same rule.
fn title_why() -> String {
    format!(
        "**nothing was written.** A row's TITLE — the words before ` — `, or the whole row when it \
         has none — is at most {TODO_TITLE_MAX} characters: the todos pane draws a row at the \
         operator's 160-cell terminal less the gutter and the row's own mark, number and `— model`, \
         and a longer title is cut off at the pane's right edge. **The rest of a row is not lost: \
         put it after ` — ` in the same `content`** — `Fix the flaky test — it fails when the store \
         is busy` — and that part is the row's DETAIL: it is not capped, and the pane draws it after \
         the title. A row already on the board is never refused for its length — the cap binds the \
         words a call WRITES — so a long row written before it can still be restated, or moved by \
         quoting it in `update`."
    )
}

/// **Parse and validate a `todos` list — the shared gate both boards' writes pass.**
///
/// Each row comes back as `(content, status, needs)`: the row's own words, its state, and the edges
/// it declares. The edges ride through to whichever board the call aims at — this session's, or a
/// child's by `target` — because a plan's ORDER is part of the plan.
///
/// The refusals are the tool's own and both paths keep them: an unknown field inside an entry is
/// refused BY NAME, an empty entry is refused, a status outside the three words is refused. The
/// unknown-field check is the one that cost a real turn — MEASURED, verbatim, from the transcript
/// of a real session:
///
///   {"todos": [{"content": "plain quoting — marked complete on the operator's
///               instruction; origin unrecoverable",
///               "operator": "mark that todo item as complete. no idea where it came from",
///               "status": "completed"}]}
///
/// `operator` is a **TOP-LEVEL** argument of this tool and the model put it one level too deep.
/// Every field this function read was valid, so the call SUCCEEDED: the entry went into the
/// model's own half and **the operator's row was never touched**. The model then spent two more
/// calls and a paragraph of its reply working out why the row would not close — *"the mark
/// didn't take, and I can't make it"* — while the operator watched a duplicate appear in their
/// pane. **That is the same defect this crate refuses everywhere else**: a tool that ignores a
/// field rather than saying it does not know it turns a wrong write into a silent one. With
/// `target` the same check refuses a smuggled `by` — the author is the daemon's to stamp, never
/// the call's to claim.
fn parse_rows(list: &[Value]) -> Result<Vec<(String, TodoStatus, Vec<TodoNeed>)>, Invocation> {
    let mut rows = Vec::with_capacity(list.len());
    for (i, t) in list.iter().enumerate() {
        if let Some(obj) = t.as_object() {
            for key in obj.keys() {
                if !matches!(key.as_str(), "content" | "status" | "needs") {
                    return Err(Invocation::failed(
                        format!("entry {} has an unknown field `{key}`", i + 1),
                        if key == "operator" || key == "update" {
                            "**`operator` and `update` are TOP-LEVEL arguments, not fields of a \
                             `todos` entry**: send one BESIDE `todos`, as {\"todos\": […], \
                             \"update\": [{\"content\": \"<one of YOUR rows, quoted exactly>\", \
                             \"status\": \"completed\"}]}. Left inside an entry it is ignored, \
                             and being ignored is what makes it look like the row changed when \
                             it did not."
                        } else if key == "by" {
                            "**`by` is not yours to send**: the author of a row is decided by the \
                             daemon from the session making the call — yours on your own board, \
                             `Parent <your session id>` on a child's. Send only `content` and \
                             `status`."
                        } else {
                            "a `todos` entry has `content`, `status` and an optional `needs` — an \
                             unknown field is refused rather than ignored."
                        },
                    ));
                }
            }
        }
        let Some(content) = t.get("content").and_then(|v| v.as_str()) else {
            return Err(Invocation::failed(
                format!("entry {} has no content", i + 1),
                "every entry needs `content` (what the step is) and `status`.",
            ));
        };
        if content.trim().is_empty() {
            return Err(Invocation::failed(
                format!("entry {} is empty", i + 1),
                "an empty entry says nothing; drop it or write the step.",
            ));
        }
        let needs = match t.get("needs") {
            None => Vec::new(),
            Some(v) => {
                let Some(list) = v.as_array() else {
                    return Err(Invocation::failed(
                        format!("entry {} has a `needs` that is not a list", i + 1),
                        "`needs` is a list of the exact `content` of other rows on this list — the \
                         rows this one waits for. Leave it out for a row that waits for nothing.",
                    ));
                };
                let mut edges = Vec::with_capacity(list.len());
                for (j, need) in list.iter().enumerate() {
                    edges.push(parse_need(need, i, j)?);
                }
                edges
            }
        };
        let status = match t.get("status").and_then(|v| v.as_str()) {
            Some("pending") => TodoStatus::Pending,
            Some("in_progress") => TodoStatus::InProgress,
            Some("completed") => TodoStatus::Completed,
            Some(other) => {
                return Err(Invocation::failed(
                    format!("entry {} has status `{other}`", i + 1),
                    "`status` is one of: pending, in_progress, completed.",
                ));
            }
            None => {
                return Err(Invocation::failed(
                    format!("entry {} has no status", i + 1),
                    "every entry needs `content` and `status`.",
                ));
            }
        };
        rows.push((content.to_string(), status, needs));
    }
    Ok(rows)
}

/// **Parse a `{content, status}` list — `update` and `operator`, one function.**
///
/// Both fields name rows by quoting them and set their state; they differ only in WHICH half of the
/// board they aim at, which is the board's business and not this parse's. The refusals name the field
/// the model actually sent, because a message that says `operator` to a model that wrote `update` is
/// the same class of lie as a key that does nothing.
///
/// **Nothing here can add or remove a row**: an entry has `content` and `status` and nothing else,
/// which is the whole of what this field means, and an unknown key is refused by name rather than
/// ignored — the same rule `parse_rows` keeps one level down.
fn parse_moves(v: Option<&Value>, field: &str) -> Result<Vec<(String, TodoStatus)>, Invocation> {
    let Some(v) = v else {
        return Ok(Vec::new());
    };
    let Some(rows) = v.as_array() else {
        return Err(Invocation::failed(
            format!("`{field}` needs to be a list of `{{content, status}}`"),
            format!(
                "send `{field}` as a list of the rows you are moving, each quoting `content` \
                 exactly as the reply shows it, or leave it out."
            ),
        ));
    };
    let mut out = Vec::with_capacity(rows.len());
    for (i, r) in rows.iter().enumerate() {
        if let Some(obj) = r.as_object() {
            for key in obj.keys() {
                if !matches!(key.as_str(), "content" | "status") {
                    return Err(Invocation::failed(
                        format!("{field} entry {} has an unknown field `{key}`", i + 1),
                        format!(
                            "a `{field}` entry has `content` and `status` — nothing else. The \
                             words are the name and the status is the only thing that moves: a row \
                             is never added or removed by this field."
                        ),
                    ));
                }
            }
        }
        let Some(content) = r.get("content").and_then(|v| v.as_str()) else {
            return Err(Invocation::failed(
                format!("{field} entry {} has no `content`", i + 1),
                "every entry needs `content` — the row's own words, quoted exactly as the list \
                 shows them.",
            ));
        };
        let status = match r.get("status").and_then(|v| v.as_str()) {
            Some("pending") => TodoStatus::Pending,
            Some("in_progress") => TodoStatus::InProgress,
            Some("completed") => TodoStatus::Completed,
            other => {
                return Err(Invocation::failed(
                    format!(
                        "{field} entry {} has status `{}`",
                        i + 1,
                        other.unwrap_or("(none)")
                    ),
                    "`status` is one of: pending, in_progress, completed.",
                ));
            }
        };
        out.push((content.to_string(), status));
    }
    Ok(out)
}

/// **One `needs` entry, as the tool spells it** — a plain string (the other row's own words), or the
/// tagged form the store and the wire carry: `{"kind": "row", "content": "…"}`,
/// `{"kind": "child", "id": "s-…"}` or `{"kind": "job", "handle": "j…"}`.
///
/// **A kind this build cannot evaluate is REFUSED, by name.** The tag exists so a kind can be added
/// without a new field on every row — *"a kind can be added without a new field on every row and a
/// reader that does not know one can SAY SO rather than misread it"* — and the tool's half of that
/// promise is here: a model that writes a kind this build does not know is told so, rather than
/// having it quietly dropped, which would leave a row that looks like it waits for something and
/// waits for nothing. What the *store* does with a kind from a NEWER build is the other half of the
/// same rule, and it is [`TodoNeed`]'s own `Unknown` variant.
///
/// **A `child` need is a session id and a `job` need is a job handle, and this tool checks NEITHER
/// for liveness.** It cannot: the board is this crate's and liveness is the daemon's
/// ([`ChildSessions`], [`RunningJobs`]), so what a wrong name costs is said where the answer is —
/// the row reads as ready, and the model finds out by asking `task_result` or `job_output`. Refusing
/// one here would need a fact this layer does not hold, and a refusal invented from a guess is worse
/// than the sentence the daemon writes. What IS refused here is a name that is EMPTY, because an
/// empty name waits for nothing while saying it waits — a thing this layer can see for itself.
fn parse_need(v: &Value, entry: usize, at: usize) -> Result<TodoNeed, Invocation> {
    let where_ = format!("entry {}'s need {}", entry + 1, at + 1);
    let named = |content: &str| -> Result<TodoNeed, Invocation> {
        if content.trim().is_empty() {
            return Err(Invocation::failed(
                format!("{where_} is empty"),
                "a need names another row by its exact `content`; an empty name waits for nothing \
                 while saying it waits.",
            ));
        }
        Ok(TodoNeed::Row {
            content: content.to_string(),
        })
    };
    let child = |id: &str| -> Result<TodoNeed, Invocation> {
        if id.trim().is_empty() {
            return Err(Invocation::failed(
                format!("{where_} names an empty session id"),
                "a `child` need names a subagent by the session id `task` handed back; an empty id \
                 waits for nothing while saying it waits.",
            ));
        }
        Ok(TodoNeed::Child { id: id.to_string() })
    };
    let job = |handle: &str| -> Result<TodoNeed, Invocation> {
        if handle.trim().is_empty() {
            return Err(Invocation::failed(
                format!("{where_} names an empty job handle"),
                "a `job` need names a background job by the handle `bash background: true` and \
                 `task` hand back, and the one `job_list` prints; an empty handle waits for nothing \
                 while saying it waits.",
            ));
        }
        Ok(TodoNeed::Job {
            handle: handle.to_string(),
        })
    };
    match v {
        Value::String(content) => named(content),
        Value::Object(o) => match o.get("kind").and_then(|k| k.as_str()) {
            Some("row") => match o.get("content").and_then(|c| c.as_str()) {
                Some(content) => named(content),
                None => Err(Invocation::failed(
                    format!("{where_} has kind `row` and no `content`"),
                    "a `row` need names the other row with `content` — the exact text this list \
                     shows for it.",
                )),
            },
            Some("child") => match o.get("id").and_then(|c| c.as_str()) {
                Some(id) => child(id),
                None => Err(Invocation::failed(
                    format!("{where_} has kind `child` and no `id`"),
                    "a `child` need names a subagent you started with `task`, by the session id \
                     that call handed back — the full id, as its reply printed it.",
                )),
            },
            Some("job") => match o.get("handle").and_then(|h| h.as_str()) {
                Some(handle) => job(handle),
                None => Err(Invocation::failed(
                    format!("{where_} has kind `job` and no `handle`"),
                    "a `job` need names a background job by its `handle` — the one `bash` with \
                     `background: true` and `job_list` print.",
                )),
            },
            Some(other) => Err(Invocation::failed(
                format!("{where_} has kind `{other}`, which this build cannot evaluate"),
                "the kinds of need are `row` — another row on this list, by its exact `content` — \
                 `child` — a subagent you started with `task`, by the session id it gave you — and \
                 `job` — a background job, by the handle `job_list` prints. Send a row's name as a \
                 plain string, or the tagged form {\"kind\": \"row\", \"content\": \"…\"}, \
                 {\"kind\": \"child\", \"id\": \"s-…\"} or {\"kind\": \"job\", \"handle\": \"j…\"}. \
                 A kind nobody can evaluate is refused rather than dropped: a need that cannot be \
                 answered must never read as met.",
            )),
            None => Err(Invocation::failed(
                format!("{where_} has no `kind`"),
                "a need is either a plain string (the other row's exact `content`) or \
                 {\"kind\": \"row\", \"content\": \"…\"} / {\"kind\": \"child\", \"id\": \"s-…\"} / \
                 {\"kind\": \"job\", \"handle\": \"j…\"}.",
            )),
        },
        _ => Err(Invocation::failed(
            format!("{where_} is neither a string nor an object"),
            "a need is either a plain string (the other row's exact `content`) or \
             {\"kind\": \"row\", \"content\": \"…\"} / {\"kind\": \"child\", \"id\": \"s-…\"} / \
             {\"kind\": \"job\", \"handle\": \"j…\"}.",
        )),
    }
}

/// The list, the way the model wrote it.
fn render(todos: &[TodoItem]) -> String {
    if todos.is_empty() {
        return "the todo list is now empty".into();
    }
    let mut out = format!("the todo list is now ({}):\n", todos.len());
    for (i, t) in todos.iter().enumerate() {
        let mark = match t.status {
            TodoStatus::Pending => "[ ]",
            TodoStatus::InProgress => "[~]",
            TodoStatus::Completed => "[x]",
            // **The operator's own marks.** See the legend below the list for what each means to the
            // reader that has to act on it.
            TodoStatus::Postponed => "[p]",
            TodoStatus::Cancelled => "[c]",
        };
        // **AND WHO WROTE IT, or `operator` is a field the model cannot aim.** The `by` field is
        // the whole of the difference between the halves, the pane has drawn it on every row since
        // R44, and this is the same fact for the reader that has to ACT on it: a row marked `— the
        // operator's` is one this call moves with `operator`, one marked `— yours` is one it moves
        // with `update` or restates with `todos`, and one marked with a `Parent` author is one NO
        // field of this tool disposes of — the author retires it. Content is printed VERBATIM (never
        // trimmed, never elided), because that string is the name the model has to quote back.
        let author = match &t.by {
            TodoBy::Operator => "the operator's".to_string(),
            // **The operator's own string, verbatim and in full** — `Parent s-…` — because a
            // child reading its own board has to be able to tell what it decided from what it
            // was told, and by whom.
            TodoBy::Parent(who) => who.clone(),
            // **The queue's own word.** A row of the merge queue's is one no field of this tool
            // disposes of: the verdict takes it off, and the daemon replaces the half from the store.
            TodoBy::Queue => "the merge queue's".to_string(),
            TodoBy::Model => "yours".to_string(),
        };
        out.push_str(&format!(
            "  {}. {} {}  — {}{}\n",
            i + 1,
            mark,
            t.content,
            author,
            // **AND WHAT IT WAITS FOR, because an edge the model cannot SEE is an edge it cannot
            // maintain.** The list is the whole of what the model knows about the plan at the start
            // of a turn, so a row that waits on another has to say so here — otherwise the first
            // `todo_write` that rewrites a row's text silently breaks every edge into it, and the
            // model has no way to know that is what happened. `the_check` says it too, but only
            // about the rows it cannot ask for.
            if t.needs.is_empty() {
                String::new()
            } else {
                let named: Vec<String> = t
                    .needs
                    .iter()
                    .map(|need| match need {
                        TodoNeed::Row { content } => format!("`{}`", content.trim()),
                        // **The child, by its id**, because that is what the model has to hand to
                        // `task_result` — the same rule the nag's own line keeps.
                        TodoNeed::Child { id } => format!("the child `{id}`"),
                        // **The job, by its handle**, because that is what the model has to hand
                        // `job_output` or `job_kill` — the same rule the child's id keeps, and the
                        // same rule the nag's own line keeps.
                        TodoNeed::Job { handle } => format!("the job `{handle}`"),
                        TodoNeed::Unknown => "a kind this build cannot evaluate".to_string(),
                    })
                    .collect();
                format!("  (waiting on {})", named.join(", "))
            }
        ));
    }
    // **AND WHAT `[p]` MEANS, because the model has to stop proposing the row without being told
    // twice.** The operator set the row aside: it is still on the board, it is still theirs, and it
    // is deliberately not being asked for. A mark with no legend would be a model reading `[p]` as
    // *mine to pick up* and proposing it on the next turn — which is the nagging the state exists
    // to stop, arriving through the model's own good manners instead of through the clock.
    //
    // Said only when there IS such a row: a list that has never used the state does not need a
    // paragraph about it, and the reply is read by a model that pays for every word.
    if todos.iter().any(|t| t.status == TodoStatus::Postponed) {
        out.push_str(
            "\n`[p]` is a row the OPERATOR has set aside — it is still on the board, it is not work \
             you are being asked for, and your own `todo_write` does not lift it: do not propose it \
             again. They lift it themselves when they want it back.\n",
        );
    }
    // **AND WHAT `[c]` MEANS, for the same reason one word over.** A cancelled row is one the
    // operator struck off: it is still in the history because *"everything that ever created stays
    // in history"*, and it is not work at all. A model that read the mark as *still mine to pick
    // up* would propose a row the operator has already decided against — which is the nag this
    // whole board exists to stop.
    if todos.iter().any(|t| t.status == TodoStatus::Cancelled) {
        out.push_str(
            "\n`[c]` is a row the OPERATOR has STRUCK OFF — it stays in the history and it is not \
             work you are owed: do not propose it, do not restore it, and do not put it back on the \
             list under other words.\n",
        );
    }
    out
}

#[cfg(test)]
mod tests;
