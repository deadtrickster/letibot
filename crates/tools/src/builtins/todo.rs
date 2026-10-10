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

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use letibot_tokencore::store::{TodoBy, TodoCondition, TodoItem, TodoNeed, TodoStatus};
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
        for t in initial {
            match t.by {
                TodoBy::Model => mine.push(t),
                TodoBy::Parent(_) => told.push(t),
                TodoBy::Operator => theirs.push(t),
            }
        }
        TodoBoard {
            todos: Mutex::new(mine),
            parent: Mutex::new(told),
            operator: Mutex::new(theirs),
            version: AtomicU64::new(0),
        }
    }

    /// **The operator's half, replaced wholesale.** A head sends its whole list on every change: it
    /// owns these rows, they are its own store's contents, and a delta protocol for a list of tens of
    /// items would be a second source of truth about them.
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
    pub fn set_states(
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
        self.set_states(&[], updates).map(|(_, theirs)| theirs)
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
    /// Returns how many rows were ADDED or CHANGED, and bumps the version only when that is not
    /// zero — the same rule `set_operator_states` keeps, so a re-send of an unchanged plan is not
    /// an event and does not cost the store a write or the pane a publish.
    pub fn upsert_parent(
        &self,
        rows: &[(String, TodoStatus, Vec<TodoNeed>)],
        by: &TodoBy,
    ) -> usize {
        let mut half = self.parent.lock().unwrap_or_else(|e| e.into_inner());
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
        changed
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
    /// Returns how many rows were ADDED or CHANGED, and bumps the version only when that is not
    /// zero — [`TodoBoard::upsert_parent`]'s rule, so a re-send of an unchanged plan is not an event
    /// and does not cost the store a write or the pane a publish.
    pub fn upsert_model(&self, rows: &[(String, TodoStatus, Vec<TodoNeed>)]) -> usize {
        let mut half = self.todos.lock().unwrap_or_else(|e| e.into_inner());
        let mut changed = 0usize;
        for (content, status, needs) in rows {
            let want = content.trim();
            if let Some(hit) = half.iter_mut().find(|t| t.content.trim() == want) {
                if hit.status != *status || &hit.needs != needs {
                    hit.status = *status;
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
    /// else had to learn that the operator or a parent can write a row too. The model's list comes
    /// first because it is the list the model has been working from; then what it was TOLD, parent
    /// before operator — the caller nearest first, the human last and on top — and each block is
    /// contiguous, the property `the_union_is_two_contiguous_blocks…` asserts for its two.
    pub fn snapshot(&self) -> Vec<TodoItem> {
        let mut out = self.todos.lock().unwrap_or_else(|e| e.into_inner()).clone();
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
        .filter(|t| t.status != TodoStatus::Completed)
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
    /// Deliberately not called `stuck`: a turn in flight is true of a child that is generating,
    /// of one inside a tool call, and of one that has wedged mid-call, so on its own it decides
    /// nothing — it is the other half of the evidence, and `quiet_for` is what moves.
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
pub fn unfinished_plan(
    rows: &[TodoItem],
    board: &[TodoItem],
    children: &ChildSessions,
) -> Option<String> {
    unfinished_plan_for(rows, board, None, children).map(|(text, _)| text)
}

/// **The check's sentence, and the row it named** — the row handed back so the caller can remember
/// the CHOICE, which is what makes the question sticky.
///
/// **The choice must not drift under the agent.** The row is picked once and named again until it
/// stops being askable (done, or postponed by the operator) — so a row the operator adds elsewhere
/// on the plan cannot change which row this check is about. What a row becoming `InProgress` changes
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
/// the agent to: the sticky `chosen` while it is still ready, and the head of the ready set
/// otherwise. The set is recomputed as the graph moves — a row completing makes others ready — and
/// the CHOICE is what sticks.
///
/// **A plan with no edges anywhere is read EXACTLY as it was before any of this existed**, and that
/// is not a special case in the code: nothing can be blocked and the ready set cannot be empty, so
/// neither of the two additions below can fire. The blocked line and the nothing-can-start sentence
/// are the only places this message knows there is a graph — which is what makes the compatibility
/// claim a consequence of the design rather than a promise kept by hand. **The same holds for a
/// board whose edges are all `Row`s and whose `children` is empty**, which is every board in a
/// runtime with no daemon behind it.
pub fn unfinished_plan_for(
    rows: &[TodoItem],
    board: &[TodoItem],
    chosen: Option<&str>,
    children: &ChildSessions,
) -> Option<(String, String)> {
    let PlanAsGraph {
        open,
        ready,
        blocked,
    } = as_a_graph(rows, board, children);
    // **AND THE ONE ALREADY ASKED ABOUT COMES FIRST**, while it is still ready. A row that has left
    // the ready set — done, set aside by the operator, or BLOCKED — is no longer a choice, and the
    // head of the ready set is; `the_check_may_ask_about` is what decided the first two, so this
    // agrees with the clock by construction rather than by a second rule.
    let next = chosen
        .and_then(|content| ready.iter().copied().find(|row| row.content == content))
        .or_else(|| ready.first().copied());
    let Some(next) = next else {
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
        TodoBy::Model => "yours",
    };
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
    Some((
        format!(
            "[todo check] this turn is finished and one item is not done{rest}:\n  - {}{state}\n{advice}{waiting}",
            next.content.trim(),
        ),
        next.content.clone(),
    ))
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
/// so the caller hands its snapshot in beside the board — see [`ChildSessions`].
fn as_a_graph<'a>(
    rows: &'a [TodoItem],
    board: &[TodoItem],
    children: &ChildSessions,
) -> PlanAsGraph<'a> {
    let queue = open_priority(rows);
    let mut ready = Vec::new();
    let mut blocked = Vec::new();
    for row in queue.iter().copied() {
        let unmet = unmet_needs(board, row, children);
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
fn unmet_needs(
    board: &[TodoItem],
    row: &TodoItem,
    children: &ChildSessions,
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
        })
        .collect();
    format!("`{}` waits on {}", row.content.trim(), waits.join(", "))
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
/// **The steering follows the evidence, and that is the second half of the same fix.** A child
/// whose own log moved inside [`CHILD_QUIET_ENOUGH_TO_LOOK`] is called WORKING, and the reader is
/// told it is alive rather than sent to look: the nag the operator reported four times in twenty
/// minutes was exactly a check asked for about a child that was fine. Anything else — including a
/// child with no evidence at all, which is what a caller that knows only an age hands in — is
/// *"check whether it is stuck"*, with both mechanisms named, because a model that has to guess a
/// mechanism guesses wrong.
///
/// **What this cannot tell apart is said rather than hidden**: a child inside a long tool call
/// that writes nothing publishes nothing while the call runs, so it reads as quiet. The evidence
/// beside the sentence is what a reader judges that by, and [`CHILD_QUIET_ENOUGH_TO_LOOK`] carries
/// the whole of the reasoning.
fn child_said(child: &RunningChild) -> String {
    let turn = if child.working {
        "a turn is in flight"
    } else {
        "no turn is in flight"
    };
    let moved = match child.quiet_for {
        Some(gap) => format!("its last event was {} ago", human_gap(gap)),
        None => "nothing has been published in it".to_string(),
    };
    if child.working
        && child
            .quiet_for
            .is_some_and(|gap| gap < CHILD_QUIET_ENOUGH_TO_LOOK)
    {
        format!("it is working ({turn}, {moved})")
    } else {
        format!(
            "{turn} and {moved} — check whether it is stuck: `task_result` with its id says where \
             it got to, and `job_kill` stops a child that is wedged"
        )
    }
}

/// **What the check says about the rows it cannot ask for**, in one line — so the model learns the
/// plan is not empty, it is STUCK, and on what.
///
/// **Two leads, because there are two kinds of stuck and they ask different things of the reader.**
/// A row waiting on another ROW waits on something the plan can move — finish it, or correct a need
/// that names nothing — and it gets *"cannot start yet"*, which is the sentence that has always
/// been here. A row waiting on a CHILD waits on something the plan cannot move at all, and the
/// operator's ruling is that it gets *check whether it is stuck* instead; one sentence for both
/// would make the second read as the first, which is exactly the nag the room to breathe exists to
/// avoid. A row with both kinds of need goes with the children, because the child is the half the
/// plan cannot answer.
fn blocked_said(blocked: &[(&TodoItem, Vec<(String, NeedFate)>)]) -> String {
    // A row goes with the CHILDREN when it waits on one, even if it also waits on a row: the
    // child is the half the plan cannot answer, and that is what the lead is for.
    let waiting_on_a_child = |unmet: &[(String, NeedFate)]| {
        unmet
            .iter()
            .any(|(_, fate)| matches!(fate, NeedFate::ChildRunning(_)))
    };
    let mut said: Vec<String> = Vec::new();
    for (on_a_child, lead) in [
        (false, "cannot start yet"),
        (true, "waiting on a child that is still running"),
    ] {
        let lines: Vec<String> = blocked
            .iter()
            .filter(|(_, unmet)| waiting_on_a_child(unmet) == on_a_child)
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

/// `1 row` / `2 rows` — the count both leads above open with.
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
/// so nothing the model writes moves it — what that row asks for is a look at the child. The extra
/// sentence is added only when such a row is here, which is the same rule the rest of this file
/// keeps about saying things a reader has no use for.
fn nothing_can_start(blocked: &[(&TodoItem, Vec<(String, NeedFate)>)]) -> String {
    let lines: Vec<String> = blocked
        .iter()
        .map(|(row, unmet)| format!("  - {}", blocked_line(row, unmet)))
        .collect();
    let a_child_is_waiting = blocked.iter().any(|(_, unmet)| {
        unmet
            .iter()
            .any(|(_, fate)| matches!(fate, NeedFate::ChildRunning(_)))
    });
    let out = if a_child_is_waiting {
        "unblock one of these rather than starting something new: finish a row they wait on, or \
         correct a need that names nothing on this board — and where what is waited on is a CHILD, \
         the look is at the child and not at the plan, because the plan cannot move it. A plan \
         where nothing can start is a plan to fix, not a plan to work."
    } else {
        "unblock one of these rather than starting something new: finish a row they wait on, or \
         correct a need that names nothing on this board. A plan where nothing can start is a plan \
         to fix, not a plan to work."
    };
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
             `update`, quoting its `content` exactly.\n\nA row the OPERATOR has POSTPONED comes \
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
              subagent's work is not work you can do yet.",
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
                                "content": {"type": "string"},
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
                                                    finish. Optional, and empty is the ordinary \
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
                    "a target must be a session THIS session spawned with `task`. Quote one of \
                     your children — the subagent pane (`ctrl-g`) and `task_result` list them — \
                     or leave `target` out to write your own board.",
                ),
            };
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
        let (moved_mine, moved_theirs) = match self.board.set_states(&updates, &ops) {
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
        let before = self.board.model_snapshot();
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
/// tagged form the store and the wire carry: `{"kind": "row", "content": "…"}` or
/// `{"kind": "child", "id": "s-…"}`.
///
/// **A kind this build cannot evaluate is REFUSED, by name.** The tag exists so a kind can be added
/// without a new field on every row — *"a kind can be added without a new field on every row and a
/// reader that does not know one can SAY SO rather than misread it"* — and the tool's half of that
/// promise is here: a model that writes a kind this build does not know is told so, rather than
/// having it quietly dropped, which would leave a row that looks like it waits for something and
/// waits for nothing. What the *store* does with a kind from a NEWER build is the other half of the
/// same rule, and it is [`TodoNeed`]'s own `Unknown` variant.
///
/// **A `child` need is a session id, and this tool does not check that it is live.** It cannot: the
/// board is this crate's and liveness is the daemon's ([`ChildSessions`]), so what a wrong id costs
/// is said where the answer is — the row reads as ready, and the model finds out by asking
/// `task_result`. Refusing one here would need a fact this layer does not hold, and a refusal
/// invented from a guess is worse than the sentence the daemon writes.
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
            Some(other) => Err(Invocation::failed(
                format!("{where_} has kind `{other}`, which this build cannot evaluate"),
                "the kinds of need are `row` — another row on this list, by its exact `content` — \
                 and `child` — a subagent you started with `task`, by the session id it gave you. \
                 Send a row's name as a plain string, or the tagged form {\"kind\": \"row\", \
                 \"content\": \"…\"} or {\"kind\": \"child\", \"id\": \"s-…\"}. A kind nobody can \
                 evaluate is refused rather than dropped: a need that cannot be answered must \
                 never read as met.",
            )),
            None => Err(Invocation::failed(
                format!("{where_} has no `kind`"),
                "a need is either a plain string (the other row's exact `content`) or \
                 {\"kind\": \"row\", \"content\": \"…\"} / {\"kind\": \"child\", \"id\": \"s-…\"}.",
            )),
        },
        _ => Err(Invocation::failed(
            format!("{where_} is neither a string nor an object"),
            "a need is either a plain string (the other row's exact `content`) or \
             {\"kind\": \"row\", \"content\": \"…\"} / {\"kind\": \"child\", \"id\": \"s-…\"}.",
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
            // **The operator's own mark.** See the legend below the list for what it means to the
            // reader that has to act on it.
            TodoStatus::Postponed => "[p]",
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
            "\n`[p]` is a row the OPERATOR has set aside — it is still on the board and still \
             theirs, and it is not work you are being asked for: do not propose it again. They \
             lift it themselves when they want it back.\n",
        );
    }
    out
}

#[cfg(test)]
mod tests {
    /// **REPLACING THE HALF WIPES STATE THE OTHER AUTHOR SET ON IT — the hazard the file-migration
    /// design has to answer before anything else.**
    ///
    /// The proposed shape is *"you literally migrate the whole todo, starting from the top level"*:
    /// `TODO.md` becomes the operator's half, re-read when a watcher notices the file change, and
    /// sent as `SetOperatorTodos`. That frame calls `set_operator`, which **replaces** the half.
    ///
    /// But the model's only way to dispose of the operator's rows is `set_operator_states`, which
    /// moves STATE and deliberately cannot change membership. So the two writes disagree about what
    /// the half IS: the model changes one row's status, and the next whole-file migration sends the
    /// FILE's statuses and takes that mark away.
    ///
    /// MEASURED here rather than argued. The order is the one a session would actually see:
    /// migrate, work, mark done, file touched, migrate again.
    #[test]
    fn a_whole_file_migration_would_wipe_the_state_the_model_set() {
        let b = TodoBoard::new(Vec::new());

        // 1. The file migrates whole — the operator's half, as `TODO.md` has it.
        let from_file = || {
            vec![
                TodoItem {
                    content: "T1 vendor the deps".into(),
                    status: TodoStatus::Pending,
                    by: TodoBy::Operator,
                    when: None,
                    needs: Vec::new(),
                },
                TodoItem {
                    content: "T2 wire the pane".into(),
                    status: TodoStatus::Pending,
                    by: TodoBy::Operator,
                    when: None,
                    needs: Vec::new(),
                },
            ]
        };
        b.set_operator(from_file());

        // 2. The model does T1 and marks it — the mechanism that already exists for exactly this.
        let changed = b
            .set_operator_states(&[("T1 vendor the deps".into(), TodoStatus::Completed)])
            .expect("the model marks the operator's row done");
        assert_eq!(changed, 1);
        assert_eq!(
            b.operator_snapshot()[0].status,
            TodoStatus::Completed,
            "the premise: the model's mark is on the board"
        );

        // 3. **The operator touches a line** — any line, even T2's, even a comment — and the watcher
        //    re-migrates the whole file, because a whole-file migration is what removes the
        //    line-level problem. The file still says `[ ]` for T1, because the model's work was
        //    never written back to it.
        b.set_operator(from_file());

        // **The assertion is that the wipe HAPPENED** — this test holds the hazard still while the
        // design answers it, so it fails the day `set_operator` learns to preserve state, and that
        // failure is the signal to rewrite it as the positive.
        assert_eq!(
            b.operator_snapshot()[0].status,
            TodoStatus::Pending,
            "the model's mark survived a whole-file migration, so the hazard this records is gone \
             — rewrite this test as the positive"
        );
    }

    /// And the tag question, measured: **a tag in the tail survives both paths to the model**, so
    /// `TodoEntry` needs nothing for it — provided the model is told the whole tree.
    ///
    /// `content` is printed VERBATIM by `render` (that is what makes the words quotable back, which
    /// `set_operator_states` depends on), and the nag's only edit is `trim`, which removes
    /// whitespace and not a trailing word. So a line's tail rides the wire as part of the row.
    #[test]
    fn a_tag_in_the_lines_tail_reaches_the_model_with_no_field_for_it() {
        let b = TodoBoard::new(vec![TodoItem {
            content: "T1 vendor the deps  #model".into(),
            status: TodoStatus::Pending,
            by: TodoBy::Operator,
            when: None,
            needs: Vec::new(),
        }]);
        let all = b.snapshot();
        // The renderer the model reads.
        let rendered = render(&all);
        assert!(
            rendered.contains("#model"),
            "the tag did not survive into what the model is told: {rendered}"
        );
        // And the nag, whose only edit is a trim.
        let nag = nag_for(&all).expect("one row is open");
        assert!(
            nag.contains("#model"),
            "the tag did not survive the nag: {nag}"
        );
    }

    /// **A POSTPONED row is still the model's to SEE, and the reply says what the mark means.**
    ///
    /// The operator's ask has two halves and this is the second one: the row *"persists"* — so it
    /// is in the list the model is handed, with its own words and its author, which is what keeps
    /// `todo_write`'s `operator` field able to aim at it — and it is *"without nag"*, which from the
    /// model's side means it can stop proposing the row **without being told twice**. That is what
    /// the legend below the list is for: `[p]` with no explanation is a mark the next turn reads as
    /// *mine to pick up*, and the model proposing it is the nagging arriving through its own good
    /// manners instead of through the clock.
    ///
    /// And the state is not one the model may set: `todo_write` still takes three words, so a model
    /// cannot silence the check that exists to stop it abandoning a plan. Asserted on the refusal
    /// itself, because that is where a fourth word would arrive.
    #[test]
    fn a_postponed_row_is_marked_in_what_the_model_is_shown() {
        let set_aside = TodoItem {
            content: "push once CI lands".into(),
            status: TodoStatus::Postponed,
            by: TodoBy::Operator,
            when: Some(TodoCondition::Job {
                handle: "j121".into(),
            }),
            needs: Vec::new(),
        };
        let all = vec![item("the model's own row", TodoStatus::Pending), set_aside];

        let shown = render(&all);
        assert!(
            shown.contains("[p] push once CI lands"),
            "the row keeps its place in the list and is marked: {shown}"
        );
        assert!(
            shown.contains("— the operator's"),
            "and says whose it is, so `operator` can still aim at it: {shown}"
        );
        assert!(
            shown.contains("do not propose it again"),
            "the mark's meaning has to be in the reply, not only in the mark: {shown}"
        );
        // The state is the OPERATOR's, so the word is not one this tool takes.
        let (mut rt, _board) = runtime();
        let mut sink = RecordingToolSink::new();
        let r = rt.invoke(
            "t1",
            &call(r#"{"todos": [{"content": "mine", "status": "postponed"}]}"#),
            &mut sink,
        );
        assert!(
            matches!(r.outcome, ToolOutcome::Failed { .. }),
            "a model must not be able to set a row aside: {:?}",
            r.outcome
        );
        let said = format!("{} {:?}", r.payload, r.outcome);
        assert!(
            said.contains("pending, in_progress, completed"),
            "and the refusal names the words it does take: {said}"
        );
    }

    use super::*;
    use crate::backend::HostBackend;
    use crate::backend::tempdir::TempDir;
    use crate::events::RecordingToolSink;
    use crate::runtime::{Registry, ToolRuntime};
    use letibot_transcript::{ToolCall, ToolOutcome};

    /// **THE OPERATOR'S ROWS ARE ON THE SAME BOARD, so the nag and the pane see them.** The
    /// operator's ruling: *"the existing getter should return mine and yours, and the rest is also
    /// the same. the only difference is who created and that is it."*
    ///
    /// Two claims, and the second is why the halves are separate: the union is what `snapshot`
    /// answers — which is what `unfinished_plan`, and therefore the idle nag, reads — and the `todo`
    /// tool's own write, scoped to the model's rows, does not take the operator's rows with it.
    #[test]
    fn the_board_returns_the_operators_rows_alongside_the_models() {
        let b = TodoBoard::new(vec![TodoItem {
            content: "the model's".into(),
            status: TodoStatus::Pending,
            by: TodoBy::Model,
            when: None,
            needs: Vec::new(),
        }]);
        assert_eq!(b.snapshot().len(), 1, "the model's list as given");

        b.set_operator(vec![TodoItem {
            content: "the operator's".into(),
            status: TodoStatus::Pending,
            by: TodoBy::Operator,
            when: None,
            needs: Vec::new(),
        }]);

        let all = b.snapshot();
        assert_eq!(all.len(), 2, "**the getter returns BOTH**: {all:?}");
        assert!(
            all.iter().any(|t| t.by == TodoBy::Operator),
            "and says who wrote each"
        );
        // **`unfinished_plan` — what the nag asks — sees the operator's row with no change at all**
        assert!(
            nag_for(&all).is_some(),
            "so the reminder can fire for work the OPERATOR queued"
        );

        // **AND THE MODEL'S OWN WRITE DOES NOT DELETE THEM.** `upsert_model` is the `todo` tool's
        // own write, and it is why the two halves are kept apart rather than concatenated.
        b.upsert_model(&[("the model's".into(), TodoStatus::Completed, Vec::new())]);
        let after = b.snapshot();
        assert_eq!(after.len(), 2, "the operator's row survived: {after:?}");
        assert_eq!(
            after[0].status,
            TodoStatus::Completed,
            "and the model's own row moved"
        );
        assert!(after.iter().any(|t| t.content == "the operator's"));
        // **A RE-WORDED ROW IS A NEW ROW** — the text is the name — so the old row stays as well:
        // there is no write on this board that removes one.
        b.upsert_model(&[(
            "the model's, revised".into(),
            TodoStatus::Pending,
            Vec::new(),
        )]);
        let after = b.snapshot();
        assert_eq!(after.len(), 3, "nothing was removed: {after:?}");
        assert!(after.iter().any(|t| t.content == "the model's"));
    }

    /// **The union is TWO BLOCKS, not an interleaving** — and with hierarchy coming, that is the
    /// fact the whole shape turns on.
    ///
    /// `snapshot` is a clone-then-`extend`: every model row precedes every operator row, always,
    /// because nothing merges the two vectors. So the halves cannot interleave by depth, which means
    /// **"my own write" stays a well-defined edit**: the model's write cannot move, delete or
    /// re-parent a row of the operator's, and the operator's half is a suffix of the union rather
    /// than a scatter through it.
    ///
    /// # And the hazard that comes with it, which is why this is asserted and not assumed
    ///
    /// Depth is POSITIONAL — that is what makes it markdown's model and rano's, and it is the right
    /// choice. But positional depth over a concatenation means a row's apparent parent is *the row
    /// before it in the union*, and at the seam that row belongs to somebody else. An operator row at
    /// depth 1 sitting after a model row at depth 0 renders as a child of it, and **the model's next
    /// write can change that parent without touching a single operator row**: a write that re-words
    /// or adds a row changes the last model row, and the operator's subtree is silently re-hung
    /// under whatever took its place. That is the same failure as a flattened subtree — data that reads as a statement
    /// nobody made — one level up, and it is silent.
    ///
    /// **The rule that removes it is one line and it is asserted here**: each half is its own tree,
    /// so the first row of each BLOCK is depth 0. The seam is a boundary, not a parent. When depth
    /// lands, a writer that indents the first row of a half must be refused where the board is built
    /// rather than coped with where it is drawn — see the operator's own instruction on the shape:
    /// *"the depth invariant enforced where the board is built, not where it is drawn. A renderer
    /// that copes with a gap hides a writer that made one."*
    ///
    /// Today there is no depth, so the assertion is the shape it will have to satisfy: the blocks are
    /// contiguous and their order is fixed.
    #[test]
    fn the_union_is_two_contiguous_blocks_so_a_half_is_a_suffix_not_a_scatter() {
        let b = TodoBoard::new(vec![
            TodoItem {
                content: "m1".into(),
                status: TodoStatus::Pending,
                by: TodoBy::Model,
                when: None,
                needs: Vec::new(),
            },
            TodoItem {
                content: "m2".into(),
                status: TodoStatus::Pending,
                by: TodoBy::Model,
                when: None,
                needs: Vec::new(),
            },
        ]);
        b.set_operator(vec![
            TodoItem {
                content: "o1".into(),
                status: TodoStatus::Pending,
                by: TodoBy::Operator,
                when: None,
                needs: Vec::new(),
            },
            TodoItem {
                content: "o2".into(),
                status: TodoStatus::Pending,
                by: TodoBy::Operator,
                when: None,
                needs: Vec::new(),
            },
        ]);

        let all = b.snapshot();
        let authors: Vec<TodoBy> = all.iter().map(|t| t.by.clone()).collect();
        assert_eq!(
            authors,
            vec![
                TodoBy::Model,
                TodoBy::Model,
                TodoBy::Operator,
                TodoBy::Operator
            ],
            "the union interleaved the halves, so the model's write is no longer a half-edit: {all:?}"
        );

        // **And the model's own write cannot reorder what it does not own.** It restates one of its
        // rows and adds another; the operator's block is still the tail, in the same order, with the
        // same contents — and the row it did not mention is still there, because a row left out is
        // not a row removed.
        b.upsert_model(&[
            ("m1".into(), TodoStatus::InProgress, Vec::new()),
            ("m3".into(), TodoStatus::Pending, Vec::new()),
        ]);
        let after = b.snapshot();
        assert_eq!(
            after.iter().map(|t| t.content.as_str()).collect::<Vec<_>>(),
            vec!["m1", "m2", "m3", "o1", "o2"],
            "the model's write moved the operator's rows: {after:?}"
        );
        assert_eq!(
            after.iter().filter(|t| t.by == TodoBy::Operator).count(),
            2,
            "and none of them went with it"
        );
    }

    /// The tool behind a real runtime, because the call goes through the gate on
    /// its way past — and `Access::Session` must pass it unattended, which this
    /// doubles as a check of.
    fn runtime() -> (ToolRuntime, Arc<TodoBoard>) {
        runtime_with(Vec::new())
    }

    /// The same runtime, on a board that already holds rows — the shape a RESUMED session gets from
    /// its store, and the only way two rows can share a name (this tool never writes a second row
    /// under a name the half already holds).
    fn runtime_with(initial: Vec<TodoItem>) -> (ToolRuntime, Arc<TodoBoard>) {
        let board = Arc::new(TodoBoard::new(initial));
        let mut reg = Registry::new();
        reg.register(Box::new(TodoWriteTool::new(board.clone())))
            .unwrap();
        let d = TempDir::new();
        let backend = HostBackend::new(d.path()).unwrap();
        // The temp dir outlives the backend only within one test; leak it there
        // rather than complicate every caller.
        std::mem::forget(d);
        (ToolRuntime::new(reg, Box::new(backend)), board)
    }

    fn call(args: &str) -> ToolCall {
        ToolCall {
            id: "c0".into(),
            name: "todo_write".into(),
            arguments: args.into(),
        }
    }

    /// **THE MODEL CAN DISPOSE OF THE OPERATOR'S ROW, which is what `TodoBy::Operator`'s doc
    /// claimed and the code did not do.** *"the model can mark the operator's item done, and the
    /// nag in `harness.rs` picks it up like any other"* — and it could not: `todo_write` was scoped
    /// to the model's half, `set_operator` is the head's frame, so a row the operator wrote was
    /// nagged about every idle turn and could never be answered. R48's wedge, from the other side.
    ///
    /// The words are the name — there is no id on the wire, and the operator ruled out a bump — so
    /// this also pins what happens when the name does not fit: **refused, with the candidates
    /// named**, and NOTHING written.
    /// **THE CALL THAT MADE A DUPLICATE ROW IN THE OPERATOR'S PANE, byte for byte from the
    /// transcript of a real session.**
    ///
    /// The model put `operator` one level too deep — inside a `todos` entry instead of beside it.
    /// Every field this function read was valid, so the call SUCCEEDED: the entry landed in the
    /// model's own half and the operator's row was never touched. The model then spent two more calls
    /// and a paragraph of its reply on why the row would not close — *"the mark didn't take, and I
    /// can't make it"* — while the operator watched a second row appear where they expected their own
    /// to change. **Ignoring a field is what made a wrong write silent**, which is the defect this
    /// refuses.
    #[test]
    fn an_operator_block_inside_a_todos_entry_is_refused_by_name() {
        let (mut rt, board) = runtime();
        board.set_operator(vec![TodoItem {
            content: "plain quoting".into(),
            status: TodoStatus::Pending,
            by: TodoBy::Operator,
            when: None,
            needs: Vec::new(),
        }]);
        let mut sink = RecordingToolSink::new();

        let r = rt.invoke(
            "t1",
            &call(
                r#"{"todos": [{"content": "plain quoting — marked complete; origin unrecoverable",
                                "operator": "mark that todo item as complete. no idea where it came from",
                                "status": "completed"}]}"#,
            ),
            &mut sink,
        );

        assert!(
            matches!(r.outcome, ToolOutcome::Failed { .. }),
            "**the call is refused, not half-obeyed**: {:?}",
            r.outcome
        );
        // **THE REFUSAL NAMES THE FIELD AND SAYS WHERE IT BELONGS** — a message that only said
        // "unknown field" would leave the model to guess, and it already guessed once.
        let said = format!("{} {:?}", r.payload, r.outcome);
        assert!(
            said.contains("operator"),
            "the refusal names the field it did not understand: {said}"
        );
        assert!(
            said.contains("TOP-LEVEL"),
            "**and says where it belongs**, because one level too deep is exactly the mistake: {said}"
        );
        // **AND NOTHING MOVED** — not their row, and not the model's list either, which is the
        // half-applied call the operator-rows-before-todos ordering exists to prevent.
        assert!(
            board
                .operator_snapshot()
                .iter()
                .all(|t| t.status == TodoStatus::Pending),
            "the operator's row is untouched: {:?}",
            board.operator_snapshot()
        );
        assert!(
            !board
                .snapshot()
                .iter()
                .any(|t| t.content.starts_with("plain quoting —")),
            "**and no duplicate row of the model's either** — which is what the operator saw: {:?}",
            board.snapshot()
        );
    }

    /// And the SAME argument, sent where it belongs, still works — the refusal is about the shape,
    /// not about the model touching the operator's rows at all.
    #[test]
    fn an_unknown_field_other_than_operator_is_refused_too() {
        let (mut rt, board) = runtime();
        board.set_operator(vec![TodoItem {
            content: "a row".into(),
            status: TodoStatus::Pending,
            by: TodoBy::Operator,
            when: None,
            needs: Vec::new(),
        }]);
        let mut sink = RecordingToolSink::new();
        let r = rt.invoke(
            "t1",
            &call(r#"{"todos": [{"content": "a step", "status": "pending", "depth": 2}]}"#),
            &mut sink,
        );
        assert!(
            matches!(r.outcome, ToolOutcome::Failed { .. }),
            "a field the schema does not name is refused rather than ignored: {:?}",
            r.outcome
        );
        assert!(
            format!("{} {:?}", r.payload, r.outcome).contains("depth"),
            "and it is named"
        );
        // `snapshot()` is the UNION of both halves, so the operator's own row is legitimately in
        // it; what must not be there is anything of the MODEL's.
        assert!(
            board.snapshot().iter().all(|t| t.by == TodoBy::Operator),
            "nothing of the model's was written: {:?}",
            board.snapshot()
        );
    }

    #[test]
    fn a_row_the_operator_wrote_is_moved_by_its_own_words() {
        let (mut rt, board) = runtime();
        board.set_operator(vec![
            TodoItem {
                content: "restart the daemon".into(),
                status: TodoStatus::Pending,
                by: TodoBy::Operator,
                when: None,
                needs: Vec::new(),
            },
            TodoItem {
                content: "push leticl".into(),
                status: TodoStatus::Pending,
                by: TodoBy::Operator,
                when: None,
                needs: Vec::new(),
            },
        ]);
        let mut sink = RecordingToolSink::new();

        // **the model names it by its exact words**, alongside its own list
        let r = rt.invoke(
            "t1",
            &call(
                r#"{"todos": [{"content": "my own step", "status": "in_progress"}],
                    "operator": [{"content": "restart the daemon", "status": "completed"}]}"#,
            ),
            &mut sink,
        );
        assert!(
            !matches!(r.outcome, ToolOutcome::Failed { .. }),
            "the move is not a failure: {:?}",
            r.outcome
        );
        let done = board.operator_snapshot();
        assert_eq!(
            done[0].status,
            TodoStatus::Completed,
            "**the operator's row moved**"
        );
        assert_eq!(
            done[1].status,
            TodoStatus::Pending,
            "and only the one that was named"
        );
        assert!(
            board.snapshot().iter().any(|t| t.content == "my own step"),
            "and the model's own list was written in the same call"
        );
        // and the model is told what happened to their row, and can see WHO wrote what
        let told = r.payload.clone();
        assert!(
            told.contains("— the operator's"),
            "the reply says whose each row is: {told}"
        );
        assert!(told.contains("— yours"), "{told}");

        // **A NAME THAT FITS NOTHING IS REFUSED AND WRITES NOTHING** — not their row, and not the
        // model's own list either, which is the half-applied call this ordering exists to prevent.
        let before = board.snapshot();
        let r = rt.invoke(
            "t2",
            &call(
                r#"{"todos": [{"content": "a plan that must not land", "status": "pending"}],
                    "operator": [{"content": "restart the daemon please", "status": "completed"}]}"#,
            ),
            &mut sink,
        );
        assert!(
            matches!(r.outcome, ToolOutcome::Failed { .. }),
            "a quote that fits nothing is not a write: {:?}",
            r.outcome
        );
        // BOTH halves, because both are shown to the model: `transcript_source.rs` renders a failed
        // result as `failed: {reason}` and then the payload under it.
        let told = format!("{:?}\n{}", r.outcome, r.payload);
        assert!(
            told.contains("restart the daemon") && told.contains("push leticl"),
            "**the refusal names the rows it DOES have**, so the model can quote one of them: {told}"
        );
        assert_eq!(
            board.snapshot(),
            before,
            "**AND NOTHING WAS WRITTEN** — the model's plan is untouched too"
        );

        // **TWO ROWS WITH THE SAME WORDS IS REFUSED RATHER THAN GUESSED.** The row that changes
        // state must not be the one the tool happened to find first.
        board.set_operator(vec![
            TodoItem {
                content: "same words".into(),
                status: TodoStatus::Pending,
                by: TodoBy::Operator,
                when: None,
                needs: Vec::new(),
            },
            TodoItem {
                content: "same words".into(),
                status: TodoStatus::Pending,
                by: TodoBy::Operator,
                when: None,
                needs: Vec::new(),
            },
        ]);
        let r = rt.invoke(
            "t3",
            &call(
                r#"{"todos": [], "operator": [{"content": "same words", "status": "completed"}]}"#,
            ),
            &mut sink,
        );
        assert!(
            matches!(r.outcome, ToolOutcome::Failed { .. }),
            "an ambiguous name is refused: {:?}",
            r.outcome
        );
        assert!(
            board
                .operator_snapshot()
                .iter()
                .all(|t| t.status == TodoStatus::Pending),
            "and neither of the two moved: {:?}",
            board.operator_snapshot()
        );
    }

    /// **The version is what the harness watches**, so it decides whether the wire and the store
    /// hear about a move at all: a real change announces, and a no-op does not — otherwise every
    /// call that re-states a status already set would publish the whole list again.
    #[test]
    fn moving_a_row_bumps_the_version_once_and_a_no_op_does_not() {
        let b = TodoBoard::new(vec![]);
        b.set_operator(vec![TodoItem {
            content: "the operator's".into(),
            status: TodoStatus::Pending,
            by: TodoBy::Operator,
            when: None,
            needs: Vec::new(),
        }]);
        let v = b.version();

        let moved = b
            .set_operator_states(&[("the operator's".into(), TodoStatus::Completed)])
            .expect("the words are there");
        assert_eq!(moved, 1, "one row changed");
        assert_eq!(
            b.version(),
            v + 1,
            "and that is ONE version, so one announcement"
        );

        let again = b
            .set_operator_states(&[("the operator's".into(), TodoStatus::Completed)])
            .expect("resolves");
        assert_eq!(again, 0, "nothing changed");
        assert_eq!(
            b.version(),
            v + 1,
            "so nothing is announced: {:?}",
            b.version()
        );
    }

    /// **AND THE NAG SAYS WHOSE ROW IT IS, because that is what chooses the mechanism.** A model
    /// told only *one item is not done* has to guess whether to rewrite its own list or quote the
    /// operator's row, and the guess it makes is the one it always makes — its own list.
    #[test]
    fn the_nag_says_whose_row_it_is_and_how_to_dispose_of_it() {
        let mine = nag_for(&[item("my step", TodoStatus::Pending)]).unwrap();
        assert!(mine.contains("— yours"), "{mine}");
        assert!(
            mine.contains("mark it done with `todo_write`'s `update` field"),
            "**the model's own row is retired by MARKING it**, and the field is named: {mine}"
        );
        assert!(
            !mine.contains("drop it"),
            "a row left out of `todo_write` STAYS on the board, so *drop it* is gone: {mine}"
        );

        let theirs = nag_for(&[TodoItem {
            content: "restart the daemon".into(),
            status: TodoStatus::Pending,
            by: TodoBy::Operator,
            when: None,
            needs: Vec::new(),
        }])
        .unwrap();
        assert!(theirs.contains("— the operator's"), "{theirs}");
        assert!(
            theirs.contains("`todo_write`'s `operator` field"),
            "**and names the mechanism**, because a model that has to guess one guesses wrong: {theirs}"
        );
        assert!(
            !theirs.contains("drop it"),
            "while *drop* is not offered for a row that is not the model's to remove: {theirs}"
        );
        assert!(theirs.contains("say why in your reply"), "{theirs}");
    }

    /// **THE FIRING IS RECORDED ON THE ROW, not in a daemon's memory.** The operator's own choice:
    /// the board is the record, so a fired row does not fire again, and a daemon that comes back
    /// re-reads it as having no condition rather than as one that is due.
    ///
    /// What moves is the condition and nothing else — which is what makes a fired row ordinary open
    /// work rather than a closed one, and what keeps the intent on the screen after the job it was
    /// waiting for is gone.
    #[test]
    fn a_fired_condition_is_cleared_from_the_row_and_nothing_else_moves() {
        let b = TodoBoard::new(vec![]);
        b.set_operator(vec![
            TodoItem {
                content: "push once CI lands".into(),
                status: TodoStatus::Pending,
                by: TodoBy::Operator,
                when: Some(TodoCondition::Job {
                    handle: "j121".into(),
                }),
                needs: Vec::new(),
            },
            TodoItem {
                content: "an ordinary row".into(),
                status: TodoStatus::Pending,
                by: TodoBy::Operator,
                when: None,
                needs: Vec::new(),
            },
        ]);
        let v = b.version();
        // **A handle nobody is waiting on consumes nothing, and must not announce anything.** A
        // version bump is a store write and a publish to every head, so a no-op that bumped would
        // republish the board on every wake.
        assert_eq!(b.consume_conditions(&["j999".to_string()]), 0);
        assert_eq!(b.version(), v, "nothing changed, so nothing is announced");
        // The one that was due.
        assert_eq!(b.consume_conditions(&["j121".to_string()]), 1);
        assert_eq!(b.version(), v + 1, "and that is ONE announcement");
        let rows = b.operator_snapshot();
        assert!(
            rows[0].when.is_none(),
            "the condition is consumed: {rows:?}"
        );
        assert_eq!(
            rows[0].content, "push once CI lands",
            "and the intent is left standing, which is what the reader acts on"
        );
        assert_eq!(
            rows[0].status,
            TodoStatus::Pending,
            "the status is not the firing's business — the row is open work now"
        );
        assert!(
            rows[1].when.is_none(),
            "a row that never had a condition is untouched"
        );
        assert_eq!(b.operator_snapshot().len(), 2, "and no row was dropped");
    }

    /// **A RESUMED BOARD COMES BACK SPLIT, and it is the store's own list that has to be.** The
    /// persisted list is the UNION, so a `new` that put all of it in the model's half gave a resumed
    /// session the operator's rows as its own: the model's write would have taken them for its own,
    /// and once the head re-pushed its own half on hello the union held each of them twice.
    #[test]
    fn the_stores_union_is_split_back_into_its_two_halves() {
        let stored = vec![
            TodoItem {
                content: "the model's, from the store".into(),
                status: TodoStatus::InProgress,
                by: TodoBy::Model,
                when: None,
                needs: Vec::new(),
            },
            TodoItem {
                content: "the operator's, from the store".into(),
                status: TodoStatus::Pending,
                by: TodoBy::Operator,
                when: None,
                needs: Vec::new(),
            },
        ];
        let b = TodoBoard::new(stored);
        assert_eq!(
            b.snapshot().len(),
            2,
            "the union is the same two rows: {:?}",
            b.snapshot()
        );
        let theirs = b.operator_snapshot();
        assert_eq!(
            theirs.len(),
            1,
            "**and the operator's half is the operator's row**"
        );
        assert_eq!(theirs[0].content, "the operator's, from the store");

        // the model's own write cannot take their row with it — the whole reason the halves are apart
        b.upsert_model(&[("a fresh plan".into(), TodoStatus::Pending, Vec::new())]);
        assert_eq!(
            b.snapshot().len(),
            3,
            "and it survives the model's next write: {:?}",
            b.snapshot()
        );
        assert!(
            b.snapshot()
                .iter()
                .any(|t| t.content == "the operator's, from the store"),
            "the operator's row is still on the board: {:?}",
            b.snapshot()
        );
        // and it came back with a status the MODEL can still move, which is the point of the split
        assert_eq!(
            b.set_operator_states(&[(
                "the operator's, from the store".into(),
                TodoStatus::Completed
            )])
            .expect("resolves"),
            1
        );
    }

    /// **THE WHOLE LIST IS AN UPSERT, and a row left out of it STAYS on the board.**
    ///
    /// The operator's rule, verbatim: *"regarding todo - only i should be able to delete todo items.
    /// as a rule everything that ever created stays in history"* and *"so done items or canceled
    /// items should be kept."* So the second write below — ONE entry, where the first wrote three —
    /// leaves all three on the board: the named row moves, the two it did not name are exactly where
    /// they were, and the reply SAYS so. That sentence is for the model whose frozen prefix still
    /// carries the old contract (*"omitting an entry removes it"*), and it is printed at the one call
    /// that would have meant a removal.
    #[test]
    fn the_whole_list_upserts_and_a_row_left_out_stays_on_the_board() {
        let (mut rt, board) = runtime();
        let mut sink = RecordingToolSink::new();
        let r = rt.invoke(
            "t1",
            &call(
                r#"{"todos": [
                    {"content": "read the harness", "status": "completed"},
                    {"content": "seat the tool", "status": "in_progress"},
                    {"content": "render the pane", "status": "pending"}
                ]}"#,
            ),
            &mut sink,
        );
        assert_eq!(r.outcome, ToolOutcome::Ok, "{}", r.payload);
        assert!(r.payload.contains("3)"), "{}", r.payload);
        assert!(r.payload.contains("[x] read the harness"), "{}", r.payload);
        assert!(r.payload.contains("[~] seat the tool"), "{}", r.payload);
        // The board holds what was written, in order.
        let snap = board.snapshot();
        assert_eq!(snap.len(), 3);
        assert_eq!(snap[0].status, TodoStatus::Completed);
        assert_eq!(snap[2].content, "render the pane");
        assert!(
            !r.payload.contains("left out of this call"),
            "the first write left nothing out: {}",
            r.payload
        );

        // **THREE ENTRIES IN, ONE ENTRY SENT, THREE STILL ON THE BOARD.** The named row moves; the
        // two it does not name are untouched.
        let r2 = rt.invoke(
            "t1",
            &call(r#"{"todos": [{"content": "render the pane", "status": "in_progress"}]}"#),
            &mut sink,
        );
        assert_eq!(r2.outcome, ToolOutcome::Ok, "{}", r2.payload);
        let after = board.snapshot();
        assert_eq!(
            after.len(),
            3,
            "a row left out of the list is NOT a deletion: {after:?}"
        );
        assert_eq!(after[0].content, "read the harness");
        assert_eq!(
            after[0].status,
            TodoStatus::Completed,
            "and its state is its own"
        );
        assert_eq!(after[1].content, "seat the tool");
        assert_eq!(after[1].status, TodoStatus::InProgress);
        assert_eq!(
            after[2].status,
            TodoStatus::InProgress,
            "and the named row moved"
        );
        assert!(
            r2.payload.contains("left out of this call")
                && r2.payload.contains("read the harness")
                && r2.payload.contains("seat the tool"),
            "**the reply names the rows it did NOT remove**: {}",
            r2.payload
        );
        // And the version moved twice, which is what the harness flush reads — once per call that
        // changed something.
        assert_eq!(board.version(), 2);
    }

    #[test]
    fn a_missing_list_a_bad_status_and_an_empty_entry_all_refuse_by_name() {
        let (mut rt, board) = runtime();
        let mut sink = RecordingToolSink::new();
        let r = rt.invoke("t1", &call("{}"), &mut sink);
        assert!(
            matches!(r.outcome, ToolOutcome::Failed { .. }),
            "{:?}",
            r.outcome
        );
        assert!(r.payload.contains("todos"), "{}", r.payload);

        let r = rt.invoke(
            "t1",
            &call(r#"{"todos": [{"content": "x", "status": "done"}]}"#),
            &mut sink,
        );
        assert!(
            r.payload.contains("pending, in_progress, completed"),
            "{}",
            r.payload
        );

        let r = rt.invoke(
            "t1",
            &call(r#"{"todos": [{"content": "  ", "status": "pending"}]}"#),
            &mut sink,
        );
        assert!(r.payload.contains("empty"), "{}", r.payload);
        // Nothing was written by any refusal.
        assert_eq!(board.version(), 0);
        assert!(board.snapshot().is_empty());
    }

    /// **A PLAN CAN WRITE ITS OWN EDGES, and the reply SHOWS them.** Both halves matter: the field
    /// is only a graph if the tool that writes the plan can express an edge, and an edge the model
    /// cannot SEE is an edge it cannot maintain — the next `todo_write` that re-words a row would
    /// break every edge into it with nothing said.
    #[test]
    fn a_plan_writes_its_edges_and_the_reply_shows_them() {
        let (mut rt, board) = runtime();
        let mut sink = RecordingToolSink::new();
        let r = rt.invoke(
            "t1",
            &call(
                r#"{"todos": [
                    {"content": "run the tests", "status": "pending"},
                    {"content": "deploy", "status": "pending", "needs": ["run the tests"]}
                ]}"#,
            ),
            &mut sink,
        );
        assert_eq!(r.outcome, ToolOutcome::Ok, "{}", r.payload);
        let snap = board.snapshot();
        assert_eq!(
            snap[1].needs,
            vec![TodoNeed::Row {
                content: "run the tests".into()
            }],
            "the edge is on the board"
        );
        assert!(
            r.payload.contains("(waiting on `run the tests`)"),
            "and the reply shows it, so the model can maintain it: {}",
            r.payload
        );
        // **And the check then asks about the row that can start** — the whole point of the edge.
        let msg = nag_for(&snap).expect("the plan is unfinished");
        assert!(msg.contains("  - run the tests"), "{msg}");
        assert!(
            msg.contains("`deploy` waits on `run the tests` (still open)"),
            "{msg}"
        );
    }

    /// **The tagged form is accepted as well as the plain name**, so the shape the store and the
    /// wire carry is one a model can write too — which is what keeps the tool's vocabulary and the
    /// store's from being two spellings a reader has to know.
    #[test]
    fn a_need_may_be_written_in_the_tagged_form() {
        let (mut rt, board) = runtime();
        let mut sink = RecordingToolSink::new();
        let r = rt.invoke(
            "t1",
            &call(
                r#"{"todos": [{"content": "run the tests", "status": "completed"},
                    {"content": "deploy", "status": "pending",
                     "needs": [{"kind": "row", "content": "run the tests"}]}]}"#,
            ),
            &mut sink,
        );
        assert_eq!(r.outcome, ToolOutcome::Ok, "{}", r.payload);
        assert_eq!(
            board.snapshot()[1].needs,
            vec![TodoNeed::Row {
                content: "run the tests".into()
            }]
        );
        // And it is satisfied, so the row is READY and nothing is blocked.
        let msg = nag_for(&board.snapshot()).expect("open work");
        assert!(msg.contains("  - deploy"), "{msg}");
        assert!(!msg.contains("cannot start yet"), "{msg}");
    }

    /// **A kind this build cannot evaluate is REFUSED by name** — never dropped. Dropping it would
    /// leave a row that looks like it waits for something and waits for nothing, which is the one
    /// reading a dependency must never have. (`session` here is a fiction, and deliberately one
    /// that is not a kind this build has: `child` IS one, and the whole point of the tag is that a
    /// model writing a kind nobody can answer is told so rather than silently dropped.)
    #[test]
    fn a_need_of_a_kind_this_build_does_not_know_is_refused_by_name() {
        let (mut rt, board) = runtime();
        let mut sink = RecordingToolSink::new();
        let r = rt.invoke(
            "t1",
            &call(
                r#"{"todos": [{"content": "deploy", "status": "pending",
                     "needs": [{"kind": "time", "at": "midnight"}]}]}"#,
            ),
            &mut sink,
        );
        assert!(
            matches!(r.outcome, ToolOutcome::Failed { .. }),
            "a kind nobody can evaluate is not a write: {:?}",
            r.outcome
        );
        let said = format!("{} {:?}", r.payload, r.outcome);
        assert!(
            said.contains("kind `time`") && said.contains("cannot evaluate"),
            "the refusal names the kind it does not know: {said}"
        );
        assert!(
            said.contains("the kinds of need are `row`") && said.contains("and `child`"),
            "and says what it does take: {said}"
        );
        assert!(
            board.snapshot().is_empty(),
            "and nothing was written: {:?}",
            board.snapshot()
        );
    }

    /// **A row can be told to WAIT ON A CHILD, and the reply shows it** — the write half of the
    /// dependency, and the half that makes the nag's silence reachable at all: a need nothing can
    /// write is a rule nothing can exercise.
    ///
    /// Both spellings the parser takes are asserted, because the model has to be able to say this
    /// in the tagged form the store and the wire carry — a plain string is a row's name and can
    /// never be a child's, so there is exactly one shape here and it has to be the tagged one.
    #[test]
    fn a_plan_can_wait_on_a_child_by_its_session_id() {
        let (mut rt, board) = runtime();
        let mut sink = RecordingToolSink::new();
        let r = rt.invoke(
            "t1",
            &call(
                r#"{"todos": [{"content": "collect the child's answer", "status": "pending",
                     "needs": [{"kind": "child", "id": "s-1-sub-2"}]}]}"#,
            ),
            &mut sink,
        );
        assert_eq!(r.outcome, ToolOutcome::Ok, "{}", r.payload);
        assert_eq!(
            board.snapshot()[0].needs,
            vec![TodoNeed::Child {
                id: "s-1-sub-2".into()
            }],
            "the edge is on the board"
        );
        assert!(
            r.payload.contains("(waiting on the child `s-1-sub-2`)"),
            "and the reply shows it, so the model can maintain it: {}",
            r.payload
        );
        // **And with no daemon behind this board the child is not running**, which is the honest
        // reading rather than a guess — `ChildSessions::none()` is what a caller with no children
        // hands in, and the row is then ordinary open work.
        assert!(
            nag_for(&board.snapshot()).is_some(),
            "a row whose child is not running is work, not a stuck plan"
        );

        // **An empty id is refused by name**, the same rule an empty row name keeps.
        let r = rt.invoke(
            "t2",
            &call(
                r#"{"todos": [{"content": "deploy", "status": "pending",
                     "needs": [{"kind": "child", "id": "  "}]}]}"#,
            ),
            &mut sink,
        );
        assert!(
            matches!(r.outcome, ToolOutcome::Failed { .. }),
            "{:?}",
            r.outcome
        );
        assert!(
            format!("{:?} {}", r.outcome, r.payload).contains("empty session id"),
            "the refusal says which kind of name was empty: {:?} {}",
            r.outcome,
            r.payload
        );
    }

    /// **An empty name and a `needs` that is not a list are refused by name too** — the same rule,
    /// one level down: an edge that names nothing is not an edge.
    #[test]
    fn an_empty_need_and_a_malformed_needs_are_refused_by_name() {
        let (mut rt, board) = runtime();
        let mut sink = RecordingToolSink::new();
        let r = rt.invoke(
            "t1",
            &call(r#"{"todos": [{"content": "deploy", "status": "pending", "needs": ["  "]}]}"#),
            &mut sink,
        );
        assert!(
            matches!(r.outcome, ToolOutcome::Failed { .. }),
            "{:?}",
            r.outcome
        );
        assert!(
            format!("{:?} {}", r.outcome, r.payload).contains("is empty"),
            "the empty name is named: {:?} {}",
            r.outcome,
            r.payload
        );

        let r = rt.invoke(
            "t2",
            &call(r#"{"todos": [{"content": "deploy", "status": "pending", "needs": "x"}]}"#),
            &mut sink,
        );
        assert!(
            matches!(r.outcome, ToolOutcome::Failed { .. }),
            "{:?}",
            r.outcome
        );
        assert!(
            format!("{:?} {}", r.outcome, r.payload).contains("not a list"),
            "a `needs` that is not a list is refused rather than ignored: {:?} {}",
            r.outcome,
            r.payload
        );
        assert!(
            board.snapshot().is_empty(),
            "nothing was written by either refusal"
        );
    }

    // -- the turn boundary ------------------------------------------------

    /// **The check's sentence for a plan that IS the whole board** — the shorthand every test below
    /// uses, and the honest one: these plans carry no POSTPONED row, so the caller's narrowing
    /// (`the_plan_as_checked`) would hand back exactly the same rows, and no row of theirs waits on
    /// a child, so `ChildSessions::none()` is the reading the daemon would hand in for them. A test
    /// that needs the two to differ says so by calling `unfinished_plan` with both — or by passing
    /// the children it means, as the child tests below do.
    fn nag_for(todos: &[TodoItem]) -> Option<String> {
        nag_with(todos, &ChildSessions::none())
    }

    /// [`nag_for`] with the daemon's own reading of the children a plan waits on.
    fn nag_with(todos: &[TodoItem], children: &ChildSessions) -> Option<String> {
        unfinished_plan(todos, todos, children)
    }

    /// **A child this daemon is running**, as a test states one: an age, and what it last showed.
    fn child(age_secs: u64, quiet_secs: Option<u64>, working: bool) -> RunningChild {
        RunningChild {
            age: Duration::from_secs(age_secs),
            quiet_for: quiet_secs.map(Duration::from_secs),
            working,
        }
    }

    /// **A row that waits on a CHILD** — the dispatch case, as these tests write one.
    fn waiting_on_a_child(content: &str, id: &str) -> TodoItem {
        TodoItem {
            needs: vec![TodoNeed::Child { id: id.to_string() }],
            ..item(content, TodoStatus::Pending)
        }
    }

    fn item(content: &str, status: TodoStatus) -> TodoItem {
        TodoItem {
            content: content.into(),
            status,
            by: TodoBy::Model,
            when: None,
            needs: Vec::new(),
        }
    }

    /// **The trigger is pending work and nothing else.** The obvious way to get this
    /// wrong is a message that fires whenever a turn ends, which teaches the model to
    /// clear its todos to make it stop — worse than no check at all. So the silence cases
    /// are asserted first and with the same weight as the firing one.
    #[test]
    fn a_plan_with_nothing_open_has_nothing_to_say() {
        assert!(nag_for(&[]).is_none(), "no plan at all");
        assert!(
            nag_for(&[
                item("one", TodoStatus::Completed),
                item("two", TodoStatus::Completed),
            ])
            .is_none(),
            "a finished plan is not a finding, it is the answer"
        );
    }

    /// **THE CHOICE IS STICKY, and this is the test for it.** The check names a row once; a row
    /// added elsewhere on the plan must not change which row the agent is being held to. Without
    /// the choice, `open_priority`'s head would move to the new row and the question would wander
    /// with it.
    #[test]
    fn the_check_keeps_asking_about_the_row_it_named() {
        let plan = vec![
            item("first", TodoStatus::Pending),
            item("second", TodoStatus::Pending),
        ];
        let (text, named) =
            unfinished_plan_for(&plan, &plan, None, &ChildSessions::none()).expect("open work");
        assert_eq!(named, "first", "the queue's head is named first");
        assert!(text.contains("first"), "{text}");

        // **A row arrives AHEAD of it**, which is the ordinary case: the operator's half of the
        // board is theirs to rewrite, and the model rewrites its own list at any time.
        let grown = vec![
            item("something else", TodoStatus::Pending),
            item("first", TodoStatus::Pending),
            item("second", TodoStatus::Pending),
        ];
        let (text, named) =
            unfinished_plan_for(&grown, &grown, Some(&named), &ChildSessions::none())
                .expect("open work");
        assert_eq!(
            named, "first",
            "the row already named is the row asked about"
        );
        assert!(
            text.contains("first") && !text.contains("something else"),
            "the question did not wander: {text}"
        );
        // And the count is still the plan's, so the model learns the plan is bigger than the row.
        assert!(text.contains("(2 more open)"), "{text}");
    }

    /// **The choice moves on when the named row leaves the queue** — done, or set aside by the
    /// operator — and then the head of the queue is the new choice. That is the difference between
    /// a sticky choice and a sticky QUESTION: the row is held only while it is still askable.
    #[test]
    fn the_choice_moves_on_when_the_named_row_leaves_the_queue() {
        let plan = vec![
            item("first", TodoStatus::Pending),
            item("second", TodoStatus::Pending),
        ];
        let (_, named) =
            unfinished_plan_for(&plan, &plan, None, &ChildSessions::none()).expect("open work");
        assert_eq!(named, "first");

        let first_done = vec![
            item("first", TodoStatus::Completed),
            item("second", TodoStatus::Pending),
        ];
        let (text, named) = unfinished_plan_for(
            &first_done,
            &first_done,
            Some("first"),
            &ChildSessions::none(),
        )
        .expect("open work");
        assert_eq!(named, "second", "the answered row is not a choice any more");
        assert!(text.contains("second"), "{text}");

        // The same for a row the operator set aside, seen from the one place the narrowing
        // happens: `the_plan_as_checked` is the CALLER's filter (a postponed row is not work this
        // check may speak about), so by the time this function is handed the plan the row the last
        // check named is simply not in it and the head of the queue is the new choice.
        let after_a_postponement = vec![item("second", TodoStatus::Pending)];
        let (_, named) = unfinished_plan_for(
            &after_a_postponement,
            &after_a_postponement,
            Some("first"),
            &ChildSessions::none(),
        )
        .expect("open work");
        assert_eq!(named, "second");

        // And with nothing askable left there is no sentence at all.
        assert!(
            unfinished_plan_for(
                &[item("first", TodoStatus::Completed)],
                &[item("first", TodoStatus::Completed)],
                Some("first"),
                &ChildSessions::none(),
            )
            .is_none(),
            "a finished plan is silence, whatever the last check named"
        );
    }

    /// **A row the model marked in progress is asked about in its own words** — the operator:
    /// *"deliberate in progress so nag can be specific … and then it keeps naggin while it is in
    /// progress."* The two sentences are different questions: one is *you were asked and have not
    /// started*, the other is *you claimed this and it is still open*.
    #[test]
    fn a_row_marked_in_progress_is_asked_about_as_its_own_claim() {
        let plan = vec![item("the migration", TodoStatus::InProgress)];
        let (fresh, _) =
            unfinished_plan_for(&plan, &plan, None, &ChildSessions::none()).expect("open work");
        assert!(
            fresh.contains("you had it in progress"),
            "a row already in progress at the first check: {fresh}"
        );

        let (sticky, _) =
            unfinished_plan_for(&plan, &plan, Some("the migration"), &ChildSessions::none())
                .expect("open work");
        assert!(
            sticky.contains("marked in progress and still open"),
            "the model's own claim, said back to it: {sticky}"
        );
        assert_ne!(fresh, sticky, "the question changed with the mark");

        // And a PENDING row that was already asked about says so, rather than repeating the first
        // check's sentence verbatim — which is what makes the repeat legible as a repeat.
        let pending = vec![item("the migration", TodoStatus::Pending)];
        let (first, _) = unfinished_plan_for(&pending, &pending, None, &ChildSessions::none())
            .expect("open work");
        let (repeat, _) = unfinished_plan_for(
            &pending,
            &pending,
            Some("the migration"),
            &ChildSessions::none(),
        )
        .expect("open work");
        assert!(
            repeat.contains("asked about this one already"),
            "the repeat says it is a repeat: {repeat}"
        );
        assert_ne!(first, repeat);
    }

    /// **The queue's ORDER, asserted as an order.** The operator: *"priority queues - in progress
    /// items than not done, one-by-one."*
    ///
    /// Five items in, five out, and the two things that matter are both visible here: `in_progress`
    /// before `pending` regardless of where each sits in the list, and list order kept WITHIN a band
    /// (a stable sort — otherwise the operator's pending row could leapfrog the model's).
    #[test]
    fn the_open_work_is_queued_in_progress_first_then_not_done() {
        let todos = vec![
            item("model pending A", TodoStatus::Pending),
            item("model done", TodoStatus::Completed),
            item("model in progress", TodoStatus::InProgress),
            item("operator pending", TodoStatus::Pending),
            item("operator in progress", TodoStatus::InProgress),
        ];
        let queued: Vec<&str> = open_priority(&todos)
            .iter()
            .map(|t| t.content.as_str())
            .collect();
        assert_eq!(
            queued,
            vec![
                "model in progress",
                "operator in progress",
                "model pending A",
                "operator pending",
            ],
            "in_progress band first, then pending, list order kept inside each"
        );
        assert!(
            !queued.contains(&"model done"),
            "**a completed item is not in the queue at all**: {queued:?}"
        );
        // and the head of that queue is what the message names
        let msg = nag_for(&todos).expect("four open items is a finding");
        assert!(msg.contains("model in progress"), "{msg}");
        assert!(msg.contains("(3 more open)"), "{msg}");
    }

    /// **Every serve takes the next one**, which is what *one-by-one* means: deal with the head and
    /// the queue's head moves on. This is the whole interaction the operator is describing, walked
    /// one step at a time rather than asserted as a formula.
    #[test]
    fn serving_the_queue_one_by_one_walks_the_work() {
        let mut todos = vec![
            item("first", TodoStatus::Pending),
            item("second", TodoStatus::Pending),
            item("third", TodoStatus::Pending),
        ];
        for expected in ["first", "second", "third"] {
            let msg = nag_for(&todos).expect("open work");
            assert!(
                msg.contains(expected),
                "the queue serves {expected} next: {msg}"
            );
            // the model deals with it — the only way the queue advances, and the reason a model that
            // ignores the check is not nagged about the same row for ever
            let at = todos.iter().position(|t| t.content == expected).unwrap();
            todos[at].status = TodoStatus::Completed;
        }
        assert!(
            nag_for(&todos).is_none(),
            "and when the work runs out, the check is silent"
        );
    }

    /// **It names ONE item, and it names the RIGHT one.** The operator: *"i think the nagger
    /// should mention only one todo at a time, so a model will not be defocused."*
    ///
    /// A list invites a model to touch all of it — read five open rows, do a little of each, end the
    /// next turn with five still open — which is the failure this check exists to prevent, arriving
    /// on the check's own message. And WHICH one is not the first by accident: an item the model
    /// marked `in_progress` is the one it told the board it was doing, so that is what gets named.
    #[test]
    fn an_open_plan_names_one_item_and_what_to_do_about_it() {
        let msg = nag_for(&[
            item("write it up", TodoStatus::Completed),
            item("wire the check", TodoStatus::InProgress),
            item("test it", TodoStatus::Pending),
        ])
        .expect("one in progress and one pending is work left open");
        // **the item it was WORKING ON**, not the first open row in the list
        assert!(
            msg.contains("wire the check"),
            "the in-progress item is the one named: {msg}"
        );
        assert!(
            msg.contains("yours, and you had it in progress"),
            "and the message says why this one, AND whose it is: {msg}"
        );
        // **and NOT the other open row** — that is the defocusing the operator asked me to stop
        assert!(
            !msg.contains("test it"),
            "the second open item is not named, because one is a directive and five is homework: {msg}"
        );
        assert!(
            !msg.contains("write it up"),
            "a completed item is not part of what is open: {msg}"
        );
        // the rest is a COUNT, which is the fact that must not become a list
        assert!(
            msg.contains("(1 more open)"),
            "the remainder is counted, not listed: {msg}"
        );
        // **all three honest answers are offered, and the third is the one that keeps this from
        // being a trap.** Without *say in your reply that you are dropping it* the model's only exit
        // is to lie about its own statuses, which is the failure this exists to stop — and now that
        // a row left out of `todo_write` STAYS on the board, saying so is the only way to drop one.
        assert!(msg.contains("do this one"), "{msg}");
        assert!(msg.contains("mark it done"), "{msg}");
        assert!(
            msg.contains("you are dropping is something you say in your reply"),
            "{msg}"
        );
        assert!(
            msg.contains("deliberately"),
            "and stopping on purpose is answerable: {msg}"
        );
        assert!(msg.starts_with("[todo check]"), "the house prefix: {msg}");
    }

    /// **With nothing started, it names the first open row** — the model has told the board nothing,
    /// so the list's own order is the only thing to go on, and singling one out is still the rule.
    #[test]
    fn with_nothing_in_progress_it_names_the_first_open_row() {
        let msg = nag_for(&[
            item("the first", TodoStatus::Pending),
            item("the second", TodoStatus::Pending),
            item("the third", TodoStatus::Pending),
        ])
        .expect("three open items is a finding");
        assert!(msg.contains("the first"), "{msg}");
        assert!(!msg.contains("the second"), "one at a time: {msg}");
        assert!(!msg.contains("the third"), "one at a time: {msg}");
        assert!(msg.contains("(2 more open)"), "{msg}");
        assert!(
            !msg.contains("in progress"),
            "and it does not claim to know why this one: {msg}"
        );
    }

    /// **A long plan is the same message as a short one.** Ten open items produce ONE named row and a
    /// count — the rule `intent`'s steering follows, and the reason `MAX_PLAN_LINES` is gone: there is
    /// no longer a length of list to cap.
    #[test]
    fn a_long_plan_is_one_item_and_a_count() {
        let todos: Vec<TodoItem> = (0..10)
            .map(|i| item(&format!("item {i}"), TodoStatus::Pending))
            .collect();
        let msg = nag_for(&todos).expect("ten open items is a finding");
        assert!(msg.contains("item 0"), "the first is named: {msg}");
        assert!(!msg.contains("item 1"), "and no other: {msg}");
        assert!(msg.contains("(9 more open)"), "the rest is counted: {msg}");
    }

    /// **A list written and finished inside the turn says nothing**, which is what taking a
    /// SNAPSHOT at the boundary buys: the check asks the state of the plan at the moment the
    /// turn stopped, not what the model did with it on the way.
    #[test]
    fn a_plan_written_and_completed_says_nothing() {
        let board = TodoBoard::new(vec![]);
        board.upsert_model(&[("do the thing".into(), TodoStatus::InProgress, Vec::new())]);
        board.upsert_model(&[("do the thing".into(), TodoStatus::Completed, Vec::new())]);
        assert!(
            nag_for(&board.snapshot()).is_none(),
            "the plan as it STANDS is what is asked about"
        );
    }

    /// **A row that waits on the rows it names** — the DAG's edge, as these tests write one.
    fn needs(content: &str, status: TodoStatus, on: &[&str]) -> TodoItem {
        TodoItem {
            needs: on
                .iter()
                .map(|c| TodoNeed::Row {
                    content: (*c).to_string(),
                })
                .collect(),
            ..item(content, status)
        }
    }

    /// **A BOARD WITH NO EDGES IS READ EXACTLY AS IT WAS BEFORE EDGES EXISTED** — the compatibility
    /// claim, as evidence rather than as a promise.
    ///
    /// Two halves, and the second is what makes the first more than a golden string. The TEXT is
    /// asserted byte for byte against what `1d24fab`'s implementation produced — every `state` arm
    /// and the count — so a change to any of them fails here. And the STRUCTURE is asserted too:
    /// with no `needs` anywhere, `as_a_graph` is the IDENTITY on `open_priority` — nothing can be
    /// blocked and the ready set is the whole queue — which is *why* the text cannot have moved,
    /// rather than a coincidence that it has not.
    #[test]
    fn a_board_with_no_dependencies_is_read_exactly_as_before() {
        let plan = vec![
            item("write it up", TodoStatus::Completed),
            item("wire the check", TodoStatus::InProgress),
            item("test it", TodoStatus::Pending),
        ];
        assert_eq!(
            nag_for(&plan).expect("open work"),
            "[todo check] this turn is finished and one item is not done (1 more open):\n  - wire \
             the check — yours, and you had it in progress\ndo this one, or mark it done with \
             `todo_write`'s `update` field, quoting the text above exactly — a row left out of \
             `todo_write` STAYS on the board, so a line of work you are dropping is something you \
             say in your reply, not something a call does. If you are stopping here deliberately, \
             say why in your reply.",
            "the sentence a dependency-free board gets is the one `1d24fab` produced, byte for byte"
        );

        // The other three arms, one at a time, because the arm is what the field could have moved.
        assert_eq!(
            nag_for(&[item("first", TodoStatus::Pending)]).expect("open work"),
            "[todo check] this turn is finished and one item is not done:\n  - first — yours\ndo \
             this one, or mark it done with `todo_write`'s `update` field, quoting the text above \
             exactly — a row left out of `todo_write` STAYS on the board, so a line of work you \
             are dropping is something you say in your reply, not something a call does. If you \
             are stopping here deliberately, say why in your reply."
        );
        assert_eq!(
            nag_for(&[
                item("first", TodoStatus::Pending),
                item("second", TodoStatus::Pending)
            ])
            .expect("open work")
            .lines()
            .next()
            .expect("a first line"),
            "[todo check] this turn is finished and one item is not done (1 more open):"
        );
        assert!(
            nag_for(&[
                item("first", TodoStatus::Pending),
                item("second", TodoStatus::Pending)
            ])
            .expect("open work")
            .contains("  - first — yours\n"),
            "the first open row is named, and the repeat arm says so"
        );
        assert_eq!(
            nag_for(&[item("first", TodoStatus::InProgress)])
                .expect("open work")
                .lines()
                .nth(1),
            Some("  - first — yours, and you had it in progress")
        );
        let sticky = unfinished_plan_for(
            &[item("first", TodoStatus::Pending)],
            &[item("first", TodoStatus::Pending)],
            Some("first"),
            &ChildSessions::none(),
        )
        .expect("open work");
        assert_eq!(
            sticky.0.lines().nth(1),
            Some(
                "  - first — yours, and you were asked about this one already and it has not \
                 been started"
            )
        );

        // **AND THE STRUCTURE**: the graph split of a board with no edges is the identity.
        let graph = as_a_graph(&plan, &plan, &ChildSessions::none());
        assert_eq!(
            graph.open, 2,
            "the plan's size, minus the row that is done — a completed row is not work this check \
             may speak about"
        );
        assert!(
            graph.blocked.is_empty(),
            "nothing can be blocked with no edges: {:?}",
            graph.blocked
        );
        assert_eq!(
            graph
                .ready
                .iter()
                .map(|t| t.content.as_str())
                .collect::<Vec<_>>(),
            open_priority(&plan)
                .iter()
                .map(|t| t.content.as_str())
                .collect::<Vec<_>>(),
            "the ready set IS the queue, in the queue's order"
        );
    }

    /// **An edge that is MET is inert.** The same board with a satisfied need is read exactly as
    /// the same board with no need at all — which is the general form of the compatibility claim,
    /// and the reason a plan that adopts the field gradually cannot change under itself.
    #[test]
    fn a_satisfied_need_is_inert() {
        let flat = vec![
            item("first", TodoStatus::Completed),
            item("second", TodoStatus::Pending),
        ];
        let edged = vec![
            item("first", TodoStatus::Completed),
            needs("second", TodoStatus::Pending, &["first"]),
        ];
        assert_eq!(nag_for(&flat), nag_for(&edged));
    }

    /// **A BLOCKED ROW IS NOT THE ASK, and the row behind it is** — the whole point of the DAG.
    ///
    /// The defect this replaces is head-of-line blocking: `open_priority(todos).first()` named the
    /// first open row whatever it was waiting for, so a row that could not start was asked about
    /// for ever and the row that could start was never reached.
    #[test]
    fn a_row_whose_need_is_open_is_not_asked_about() {
        let plan = vec![
            needs("deploy", TodoStatus::Pending, &["run the tests"]),
            item("run the tests", TodoStatus::Pending),
        ];
        let msg = nag_for(&plan).expect("one row can start");
        assert!(
            msg.contains("  - run the tests"),
            "the ready row is the one named: {msg}"
        );
        assert!(
            !msg.contains("  - deploy"),
            "and the blocked row is not named as the thing to do: {msg}"
        );
        // **And it is SAID rather than skipped**, because a blocked row is invisible to the ask and
        // a plan whose edges went stale would otherwise look like a shorter plan.
        assert!(
            msg.contains("1 row cannot start yet: `deploy` waits on `run the tests` (still open)."),
            "what cannot start is said, with what it waits for: {msg}"
        );
        assert!(
            msg.contains("(1 more open)"),
            "and a blocked row is still open work, so it is counted: {msg}"
        );
    }

    /// **THE DEGENERATE CASE, designed deliberately: an unfinished plan with an EMPTY ready set.**
    ///
    /// Everything left waits on something, so there is nothing to work on and something to UNBLOCK.
    /// Going quiet here would read as *nothing to do*, which is the one reading that is wrong; and
    /// asking about the head of the list anyway is the head-of-line blocking this replaced. A cycle
    /// lands in exactly this state, which is why it is the example.
    #[test]
    fn a_plan_where_nothing_can_start_says_so() {
        let plan = vec![
            needs("deploy", TodoStatus::Pending, &["run the tests"]),
            needs("run the tests", TodoStatus::Pending, &["deploy"]),
        ];
        let (msg, named) = unfinished_plan_for(&plan, &plan, None, &ChildSessions::none())
            .expect("a stuck plan is a finding");
        assert!(
            msg.starts_with("[todo check] this turn is finished and NOTHING can start"),
            "the state is said in the house prefix's own voice: {msg}"
        );
        assert!(
            msg.contains("  - `deploy` waits on `run the tests` (still open)\n")
                && msg.contains("  - `run the tests` waits on `deploy` (still open)\n"),
            "every row that is waiting, and on what: {msg}"
        );
        assert!(
            msg.contains("unblock one of these rather than starting something new"),
            "and what to do about it — unblock, rather than start: {msg}"
        );
        assert_eq!(
            named, "",
            "nothing is being held to, so the next check draws its choice afresh"
        );
    }

    /// **An unresolvable need is NEVER met** — the rule `TodoCondition` learned first, *a condition
    /// nobody can evaluate must never read as met*.
    ///
    /// A name that matches no row is the common case and the one the message has to make
    /// repairable: a row was re-worded, or deleted, or the name was never right.
    #[test]
    fn a_need_that_names_nothing_blocks_the_row_and_says_so() {
        let plan = vec![
            needs("deploy", TodoStatus::Pending, &["publish"]),
            item("run the tests", TodoStatus::Pending),
        ];
        let msg = nag_for(&plan).expect("one row can start");
        assert!(
            msg.contains("  - run the tests"),
            "the resolvable row is the one named: {msg}"
        );
        assert!(
            msg.contains("`deploy` waits on `publish` (no such row on this board)"),
            "and the unresolvable need is said as unresolvable, never quietly satisfied: {msg}"
        );

        // Alone, it is the degenerate case: the ONLY row cannot start.
        let alone = vec![needs("deploy", TodoStatus::Pending, &["publish"])];
        let (msg, named) = unfinished_plan_for(&alone, &alone, None, &ChildSessions::none())
            .expect("a stuck plan speaks");
        assert!(msg.contains("NOTHING can start"), "{msg}");
        assert!(
            msg.contains("`deploy` waits on `publish` (no such row on this board)"),
            "{msg}"
        );
        assert_eq!(named, "");
    }

    /// **A name that matches two rows is unresolvable** — the house rule for an ambiguous name,
    /// read one step earlier: the guess this refuses to make is a row silently starting on the
    /// strength of a name that meant something else.
    #[test]
    fn a_need_that_matches_two_rows_is_unresolvable() {
        let plan = vec![
            item("same words", TodoStatus::Pending),
            item("same words", TodoStatus::Pending),
            needs("deploy", TodoStatus::Pending, &["same words"]),
        ];
        let msg = nag_for(&plan).expect("two rows can start");
        assert!(
            msg.contains("`deploy` waits on `same words` (two rows on this board say that)"),
            "{msg}"
        );
    }

    /// **A need of a kind this build cannot evaluate blocks the row and SAYS SO** — the promise
    /// `TodoCondition` makes in its own words, kept where breaking it would be silent. This is the
    /// value a NEWER build's store leaves behind; see `TodoNeed::Unknown` for why it does not fail
    /// the whole plan instead.
    #[test]
    fn a_need_of_a_kind_this_build_cannot_evaluate_blocks_and_says_so() {
        let plan = vec![TodoItem {
            needs: vec![TodoNeed::Unknown],
            ..item("deploy", TodoStatus::Pending)
        }];
        let msg = nag_for(&plan).expect("a stuck plan speaks");
        assert!(msg.contains("NOTHING can start"), "{msg}");
        assert!(
            msg.contains("`deploy` waits on a dependency of a kind this build cannot evaluate"),
            "an edge nobody can answer must never read as met: {msg}"
        );
    }

    /// **A row that waits on a CHILD is silent while the child is YOUNG, and spoken about once it
    /// has been running long enough to be worth a look.**
    ///
    /// The operator's directive, and the failure it was written against: *"so you are not nagged
    /// till childs are running for the first 30 minutes of child life"*, because a row marked in
    /// progress with a child working on it was nagged four times in twenty minutes.
    ///
    /// The two halves are asserted on the SAME plan and the SAME child, forty minutes apart in age:
    /// the only fact that changes is the age, and what changes with it is whether the check may
    /// speak about the row at all. The narrowing is the caller's — `waits_on_a_child_with_room_to_
    /// breathe`, which is what `harnessd`'s `the_check_may_ask_about` reads — so this asserts the
    /// rule where it lives rather than through a daemon.
    #[test]
    fn a_child_that_is_still_young_keeps_the_row_silent_and_an_old_one_is_spoken_about() {
        let plan = vec![waiting_on_a_child(
            "collect the child's answer",
            "s-1-sub-2",
        )];

        // **Five minutes old: the row is untalkable.** The predicate the caller narrows with says
        // so — and the child is at its most alive, generating four seconds ago.
        let young = ChildSessions::none().running("s-1-sub-2", child(5 * 60, Some(4), true));
        assert!(
            waits_on_a_child_with_room_to_breathe(&plan[0], &young),
            "a row waiting on a five-minute-old child is one the check may not speak about"
        );

        // **Forty-five minutes old: it is talkable, and what it says is *check the child*.** The
        // child has gone quiet — nothing out of it for twelve minutes, no turn in flight — which is
        // the state the operator wants the model steered to.
        let old = ChildSessions::none().running("s-1-sub-2", child(45 * 60, Some(12 * 60), false));
        assert!(
            !waits_on_a_child_with_room_to_breathe(&plan[0], &old),
            "past the room to breathe, the check may speak"
        );
        let msg = nag_with(&plan, &old).expect("a stuck plan speaks");
        assert!(
            msg.contains("the child `s-1-sub-2`, running 45 minutes"),
            "the sentence names the child and its age: {msg}"
        );
        assert!(
            msg.contains("check whether it is stuck"),
            "and steers to the CHILD rather than to the plan: {msg}"
        );
        assert!(
            !msg.contains("cannot start yet"),
            "the one sentence it must not say is the one that tells the model nothing: {msg}"
        );
        assert!(
            msg.contains("no turn is in flight") && msg.contains("12 minutes"),
            "and it carries the evidence the verdict was made of: {msg}"
        );
        assert!(
            msg.contains("`task_result`") && msg.contains("`job_kill`"),
            "with the mechanisms named, because a model that guesses one guesses wrong: {msg}"
        );
    }

    /// **A WORKING child and a QUIET one are told apart by the sentence** — the second half of the
    /// directive, and the whole of the report that prompted it: *"I have just asked the registry
    /// three times about another row's child and got the identical sentence every time — 'still
    /// working' — which tells me it has not died and nothing at all about whether it is stuck."*
    ///
    /// Both children are the same age here — forty-five minutes — so the age decides nothing and
    /// the difference between the two notices is only what the child last showed. That is the whole
    /// point: a notice that can report liveness alone cannot steer anybody.
    #[test]
    fn a_child_that_is_moving_is_not_sent_to_be_checked_on() {
        let plan = vec![waiting_on_a_child(
            "collect the child's answer",
            "s-1-sub-2",
        )];
        let moving = ChildSessions::none().running("s-1-sub-2", child(45 * 60, Some(8), true));
        let msg = nag_with(&plan, &moving).expect("a stuck plan speaks");
        assert!(
            msg.contains("it is working (a turn is in flight, its last event was 8 seconds ago)"),
            "a child that moved eight seconds ago is called working: {msg}"
        );
        assert!(
            !msg.contains("check whether it is stuck"),
            "and the check is not asked for about it: {msg}"
        );

        // **The same age, nothing moving.** The two notices differ, and that difference is the
        // whole of what the operator asked for.
        let quiet =
            ChildSessions::none().running("s-1-sub-2", child(45 * 60, Some(12 * 60), false));
        let quiet_msg = nag_with(&plan, &quiet).expect("a stuck plan speaks");
        assert_ne!(msg, quiet_msg, "working and quiet must not read the same");
        assert!(
            quiet_msg.contains("check whether it is stuck"),
            "{quiet_msg}"
        );
    }

    /// **A child that has FINISHED resolves the need, and the row stops being about the child.**
    ///
    /// A finished child is simply not in the snapshot — the daemon's journal says `done` — so the
    /// need is met, the row is READY, and the check asks about it as ordinary work. This is the
    /// half that makes the need a dependency rather than a note about one, and it is the same
    /// reading `TodoCondition::Job` gives a handle it does not know.
    #[test]
    fn a_child_that_has_finished_resolves_the_need() {
        let plan = vec![waiting_on_a_child(
            "collect the child's answer",
            "s-1-sub-2",
        )];
        let finished = ChildSessions::none();
        assert!(
            !waits_on_a_child_with_room_to_breathe(&plan[0], &finished),
            "a child that is not running is not a reason to stay quiet"
        );
        let msg = nag_with(&plan, &finished).expect("the row is open work now");
        assert!(
            msg.contains("  - collect the child's answer — yours"),
            "the row itself is what the check asks about: {msg}"
        );
        assert!(
            !msg.contains("s-1-sub-2") && !msg.contains("check whether it is stuck"),
            "and it is not about the child any more — the row's own words say `child`, so what \
             this asserts is that the ID is gone and nothing asks to check on it: {msg}"
        );
    }

    /// **The silence a young child buys has an END, and the clock is told when it is.**
    ///
    /// A clock is armed by a turn, a board write or a prompt, and a child's AGE is none of those —
    /// so a row that went quiet at five minutes would stay quiet until somebody happened to do
    /// something, and the steering the operator asked for would arrive only by accident. This is the
    /// one number that closes that: the moment the FIRST of the children a plan waits on leaves its
    /// room to breathe.
    ///
    /// Three shapes, and the middle one is why it is a `min`: a plan waiting on two young children
    /// is due when the EARLIER one grows up, not when both have — the other row's silence is not
    /// this row's.
    #[test]
    fn the_silence_a_young_child_buys_ends_when_that_child_grows_up() {
        let two = vec![
            waiting_on_a_child("collect the first child's answer", "s-1-sub-2"),
            waiting_on_a_child("collect the second child's answer", "s-1-sub-3"),
        ];
        let children = ChildSessions::none()
            .running("s-1-sub-2", child(5 * 60, Some(4), true))
            .running("s-1-sub-3", child(20 * 60, Some(30), true));
        assert_eq!(
            when_a_young_child_grows_up(&two, &children),
            Some(Duration::from_secs(10 * 60)),
            "the EARLIER child's remaining breath, which is when the first row becomes talkable"
        );

        // **A child that is already past it is not a deadline** — that row is talkable now, and
        // `nag_should_arm` is what says so; this function answers only about the silence.
        let grown = ChildSessions::none().running("s-1-sub-2", child(45 * 60, Some(60), false));
        assert_eq!(when_a_young_child_grows_up(&two[..1], &grown), None);
        // **And a plan that waits on nothing has no such deadline**, whatever is running.
        assert_eq!(
            when_a_young_child_grows_up(&[item("open", TodoStatus::Pending)], &children),
            None
        );
    }

    /// **A daemon full of children changes nothing about a board that waits on rows** — the
    /// compatibility half, said the other way round. `ChildSessions` is read by a `Child` need and
    /// by nothing else, so a flat plan and a row-edged plan are both read exactly as they were
    /// before any of this existed, whether the caller knows about children or not.
    #[test]
    fn a_snapshot_of_children_changes_nothing_about_a_board_that_waits_on_rows() {
        let flat = vec![
            item("first", TodoStatus::Pending),
            item("second", TodoStatus::Pending),
        ];
        let edged = vec![
            item("run the tests", TodoStatus::Pending),
            needs("deploy", TodoStatus::Pending, &["run the tests"]),
        ];
        let children = ChildSessions::none().running("s-other", child(60 * 60, Some(1), true));
        assert_eq!(
            nag_for(&flat),
            nag_with(&flat, &children),
            "no needs at all"
        );
        assert_eq!(nag_for(&edged), nag_with(&edged, &children), "row edges");
    }

    /// **The graph MOVES**: finishing a row is what makes the rows that waited on it ready, so the
    /// check's question follows the work rather than the list's order.
    #[test]
    fn finishing_a_row_makes_what_waited_on_it_ready() {
        let mut plan = vec![
            item("first", TodoStatus::Pending),
            needs("second", TodoStatus::Pending, &["first"]),
        ];
        let (_, named) = unfinished_plan_for(&plan, &plan, None, &ChildSessions::none())
            .expect("first can start");
        assert_eq!(named, "first", "the only ready row is the one asked about");

        plan[0].status = TodoStatus::Completed;
        let (msg, named) = unfinished_plan_for(&plan, &plan, Some("first"), &ChildSessions::none())
            .expect("second can");
        assert_eq!(
            named, "second",
            "the answered row is gone and the graph moved on"
        );
        assert!(msg.contains("  - second"), "{msg}");
        assert!(
            !msg.contains("cannot start yet"),
            "nothing is blocked once the row it waited on is done: {msg}"
        );
    }

    /// **The sticky choice is held only while the row is READY** — the composition of the two
    /// halves. A row that has left the ready set — done, set aside, or BLOCKED — is no longer a
    /// choice, and the head of the ready set is.
    #[test]
    fn the_sticky_choice_moves_on_when_the_named_row_becomes_blocked() {
        let plan = vec![
            needs("deploy", TodoStatus::Pending, &["later"]),
            item("later", TodoStatus::Pending),
        ];
        let (msg, named) =
            unfinished_plan_for(&plan, &plan, Some("deploy"), &ChildSessions::none())
                .expect("one row can start");
        assert_eq!(
            named, "later",
            "the row the check was holding to is blocked, so it is not the choice any more"
        );
        assert!(msg.contains("  - later"), "{msg}");
        assert!(
            msg.contains("`deploy` waits on `later` (still open)"),
            "and the row it stopped holding is still SAID, as blocked: {msg}"
        );
    }

    /// **A CALL THAT NAMES NO ROW IS REFUSED, and an empty list is one of those.**
    ///
    /// `{"todos": []}` used to mean *clear the board*, and the operator has ruled that out: *"only i
    /// should be able to delete todo items."* With no delete on this board an empty list says
    /// nothing at all, so it is refused rather than read as one that did something — and the board
    /// keeps every row it had.
    #[test]
    fn an_empty_list_is_refused_and_removes_nothing() {
        let (mut rt, board) = runtime();
        let mut sink = RecordingToolSink::new();
        rt.invoke(
            "t1",
            &call(r#"{"todos": [{"content": "one", "status": "pending"}]}"#),
            &mut sink,
        );
        assert_eq!(board.snapshot().len(), 1);
        let v = board.version();
        let r = rt.invoke("t1", &call(r#"{"todos": []}"#), &mut sink);
        assert!(
            matches!(r.outcome, ToolOutcome::Failed { .. }),
            "an empty list is not a write: {:?}",
            r.outcome
        );
        let told = format!("{:?}\n{}", r.outcome, r.payload);
        assert!(
            told.contains("names no rows"),
            "the refusal says what is wrong with it: {told}"
        );
        assert_eq!(
            board.snapshot().len(),
            1,
            "**and the row is still there** — nothing this tool sends removes one"
        );
        assert_eq!(board.version(), v, "and nothing was announced");
    }

    // -- ONE ROW, BY ITS OWN WORDS -----------------------------------------

    /// **A QUOTE MARKS EXACTLY ONE ROW, and `update` does it without re-sending the list.**
    ///
    /// The operator's ask: an `update` field that takes the row's exact content plus the new status,
    /// because there was no way to say *this row, and only this one, is now done* without sending the
    /// whole list again.
    ///
    /// The assertion is a DIFF of the rendered board: one line moved, only the mark inside it, and
    /// the line count is the same. A status change that also moved a row, added one or dropped one
    /// would pass a count of `snapshot().len()` and fail here.
    #[test]
    fn a_quote_marks_exactly_one_row() {
        let (mut rt, board) = runtime();
        let mut sink = RecordingToolSink::new();
        rt.invoke(
            "t1",
            &call(
                r#"{"todos": [
                    {"content": "read the harness", "status": "pending"},
                    {"content": "seat the tool", "status": "pending"},
                    {"content": "render the pane", "status": "pending"}
                ]}"#,
            ),
            &mut sink,
        );
        let before = render(&board.snapshot());
        // **NO `todos` AT ALL.** One row, named by its own words — which is why `todos` is no longer
        // a required argument of this tool.
        let r = rt.invoke(
            "t1",
            &call(r#"{"update": [{"content": "seat the tool", "status": "completed"}]}"#),
            &mut sink,
        );
        assert_eq!(r.outcome, ToolOutcome::Ok, "{}", r.payload);
        let after = render(&board.snapshot());
        let moved: Vec<(&str, &str)> = before
            .lines()
            .zip(after.lines())
            .filter(|(b, a)| b != a)
            .collect();
        assert_eq!(
            moved.len(),
            1,
            "exactly one line moved:\n{before}\n---\n{after}"
        );
        assert!(moved[0].1.contains("[x] seat the tool"), "{:?}", moved[0]);
        assert_eq!(
            before.lines().count(),
            after.lines().count(),
            "and no line was added or removed:\n{after}"
        );
        // The status is the ONLY thing that moved, and it moved on the named row.
        let snap = board.snapshot();
        assert_eq!(snap[0].status, TodoStatus::Pending);
        assert_eq!(snap[1].status, TodoStatus::Completed);
        assert_eq!(snap[2].status, TodoStatus::Pending);
        assert!(
            r.payload
                .contains("of your own rows you named, 1 changed status"),
            "{}",
            r.payload
        );
        assert!(
            !r.payload.contains("left out of this call"),
            "no `todos` was sent, so nothing was left out of one: {}",
            r.payload
        );
        assert!(
            snap.iter().all(|t| t.needs.is_empty()),
            "and `update` wrote no edges: {snap:?}"
        );
    }

    /// **A QUOTE THAT FITS NO ROW IS REFUSED, and the refusal names the rows that half DOES hold.**
    ///
    /// The house rule for a name — `set_states`' own doc — and what makes content-keying safe: the
    /// model can quote one of the rows it was just shown instead of guessing again. **AND NOTHING IS
    /// WRITTEN** — not the row, and not the `todos` beside it.
    #[test]
    fn a_quote_that_fits_no_row_is_refused_by_name() {
        let (mut rt, board) = runtime();
        let mut sink = RecordingToolSink::new();
        rt.invoke(
            "t1",
            &call(r#"{"todos": [{"content": "seat the tool", "status": "pending"}]}"#),
            &mut sink,
        );
        let before = board.snapshot();
        let v = board.version();
        let r = rt.invoke(
            "t1",
            &call(
                r#"{"todos": [{"content": "a plan that must not land", "status": "pending"}],
                    "update": [{"content": "seat the tool please", "status": "completed"}]}"#,
            ),
            &mut sink,
        );
        assert!(
            matches!(r.outcome, ToolOutcome::Failed { .. }),
            "a quote that fits nothing is not a write: {:?}",
            r.outcome
        );
        let told = format!("{:?}\n{}", r.outcome, r.payload);
        assert!(
            told.contains("no row of your own says `seat the tool please`"),
            "the refusal names the fact: {told}"
        );
        assert!(
            told.contains("`seat the tool`"),
            "**and the row it does have**, so the model can quote that instead: {told}"
        );
        assert_eq!(
            board.snapshot(),
            before,
            "**AND NOTHING WAS WRITTEN** — not the row, and not the list beside it"
        );
        assert_eq!(board.version(), v, "and nothing was announced");
    }

    /// **A QUOTE THAT FITS TWO ROWS IS REFUSED, and BOTH are named.**
    ///
    /// Two rows answer to one quote when their text differs only in whitespace — the match trims —
    /// which `todo_write` cannot produce (it upserts by trimmed text) and a store written by hand
    /// can. So the board is built directly, through the same door a RESUMED session's store comes in
    /// by. The guess this refuses to make is a row silently changing state under a model that quoted
    /// something else.
    #[test]
    fn a_quote_that_fits_two_rows_is_refused_naming_both() {
        let (mut rt, board) = runtime_with(vec![
            TodoItem {
                content: "same words".into(),
                status: TodoStatus::Pending,
                by: TodoBy::Model,
                when: None,
                needs: Vec::new(),
            },
            TodoItem {
                content: "same words ".into(),
                status: TodoStatus::Pending,
                by: TodoBy::Model,
                when: None,
                needs: Vec::new(),
            },
        ]);
        let mut sink = RecordingToolSink::new();
        assert_eq!(board.snapshot().len(), 2, "both rows are on the board");
        let before = board.snapshot();
        let r = rt.invoke(
            "t1",
            &call(r#"{"update": [{"content": "same words", "status": "completed"}]}"#),
            &mut sink,
        );
        assert!(
            matches!(r.outcome, ToolOutcome::Failed { .. }),
            "an ambiguous name is refused: {:?}",
            r.outcome
        );
        let told = format!("{:?}\n{}", r.outcome, r.payload);
        assert!(told.contains("is 2 of your own rows"), "{told}");
        assert!(
            told.contains("row 1 `same words`") && told.contains("row 2 `same words `"),
            "**both candidates are named**, by their place and their exact words: {told}"
        );
        assert_eq!(board.snapshot(), before, "and neither of the two moved");
    }

    // -- a parent writing a child's board ---------------------------------

    /// The fake daemon half: one child id, one board, a refusal it hands back verbatim —
    /// everything the tool needs to be tested against, and nothing the real resolver does
    /// that the tool relies on (the parentage check and the author stamp are the daemon's,
    /// and are tested where the daemon is).
    struct FakeChildren {
        board: Arc<TodoBoard>,
        refused: Mutex<Vec<String>>,
    }

    impl super::ChildTodos for FakeChildren {
        fn upsert_child(
            &self,
            _target: &str,
            rows: &[(String, TodoStatus, Vec<TodoNeed>)],
        ) -> Result<Vec<TodoItem>, String> {
            // The real resolver stamps the author itself; the fake does the same, with an id
            // shaped like a real one so the rendered reply can be asserted on it.
            let by = TodoBy::parent_of("s-1789462738453908838");
            self.board.upsert_parent(rows, &by);
            Ok(self.board.snapshot())
        }
    }

    fn children_runtime(board: Arc<TodoBoard>) -> (ToolRuntime, Arc<TodoBoard>, Arc<FakeChildren>) {
        let own = Arc::new(TodoBoard::new(vec![]));
        let fake = Arc::new(FakeChildren {
            board,
            refused: Mutex::new(Vec::new()),
        });
        let mut reg = Registry::new();
        reg.register(Box::new(super::TodoWriteTool::with_children(
            own.clone(),
            fake.clone(),
        )))
        .unwrap();
        let d = TempDir::new();
        let backend = HostBackend::new(d.path()).unwrap();
        std::mem::forget(d);
        (ToolRuntime::new(reg, Box::new(backend)), own, fake)
    }

    /// **A parent's write UPSERTS and never replaces — the child's rows, the operator's rows and
    /// the version's economy all survive it.**
    ///
    /// This is the hazard the whole feature turns on, measured rather than argued: under the old
    /// whole-list-replace contract a parent that wrote a child's board would have deleted the
    /// child's plan with its own three rows. Every arm below is one thing that must not move.
    #[test]
    fn a_parents_write_upserts_its_own_rows_and_nobody_elses_move() {
        let b = TodoBoard::new(vec![
            item("the child's own step", TodoStatus::InProgress),
            TodoItem {
                content: "the operator's row".into(),
                status: TodoStatus::Pending,
                by: TodoBy::Operator,
                when: None,
                needs: Vec::new(),
            },
        ]);
        let author = TodoBy::parent_of("s-1789462738453908838");

        // Two rows arrive from the parent.
        let changed = b.upsert_parent(
            &[
                (
                    "land the parity row".into(),
                    TodoStatus::Pending,
                    Vec::new(),
                ),
                ("write the report".into(), TodoStatus::Pending, Vec::new()),
            ],
            &author,
        );
        assert_eq!(changed, 2, "both rows were added");
        assert_eq!(b.version(), 1);

        // **The child's rows and the operator's are untouched, and the union orders them:
        // what the model chose, then what it was told, then the operator's.**
        let all = b.snapshot();
        assert_eq!(
            all.iter().map(|t| t.content.as_str()).collect::<Vec<_>>(),
            vec![
                "the child's own step",
                "land the parity row",
                "write the report",
                "the operator's row",
            ],
            "the parent's rows went BETWEEN the model's and the operator's: {all:?}"
        );
        assert_eq!(b.parent_snapshot().len(), 2);
        assert!(
            b.parent_snapshot().iter().all(|t| t.by == author),
            "every row in the parent's half carries the author exactly: {:?}",
            b.parent_snapshot()
        );

        // **A re-send is a state update, not a duplicate** — and the SAME text under a different
        // author's name would be a different row, which is the scoping, but one parent is one
        // author, so the second send moves what the first wrote.
        let changed = b.upsert_parent(
            &[(
                "land the parity row".into(),
                TodoStatus::Completed,
                Vec::new(),
            )],
            &author,
        );
        assert_eq!(changed, 1, "the existing row moved, nothing was added");
        assert_eq!(b.parent_snapshot().len(), 2, "still two rows: no duplicate");
        assert_eq!(b.version(), 2);

        // **Omission is not deletion.** The parent sends one row and leaves the other out —
        // under the own-board contract that would delete it; here it must not.
        let changed = b.upsert_parent(
            &[(
                "write the report".into(),
                TodoStatus::InProgress,
                Vec::new(),
            )],
            &author,
        );
        assert_eq!(changed, 1);
        assert_eq!(
            b.parent_snapshot().len(),
            2,
            "omitting `land the parity row` did not remove it: {:?}",
            b.parent_snapshot()
        );

        // **An unchanged re-send is not an event** — the version rule `set_operator_states` keeps.
        let before = b.version();
        let changed = b.upsert_parent(
            &[(
                "write the report".into(),
                TodoStatus::InProgress,
                Vec::new(),
            )],
            &author,
        );
        assert_eq!(changed, 0, "nothing changed");
        assert_eq!(b.version(), before, "and nothing was announced");

        // **And the CHILD's own write cannot reach them either** — the other edge of the same rule.
        // The child re-plans its own half and the parent's rows stay; and because a row left out is
        // not a row removed, its own old row stays too — what it writes is what it ADDS or RESTATES.
        b.upsert_model(&[
            (
                "the child's own step".into(),
                TodoStatus::Completed,
                Vec::new(),
            ),
            (
                "the child's revised step".into(),
                TodoStatus::Pending,
                Vec::new(),
            ),
        ]);
        let all = b.snapshot();
        assert_eq!(
            all.iter().map(|t| t.content.as_str()).collect::<Vec<_>>(),
            vec![
                "the child's own step",
                "the child's revised step",
                "land the parity row",
                "write the report",
                "the operator's row",
            ],
            "the child's write moved somebody else's rows: {all:?}"
        );
    }

    /// **A parent's row is NAG-WORTHY BY CONSTRUCTION** — and that is the whole of “the child
    /// learns via usual todo nags”.
    ///
    /// The selection rules are read here, not assumed: `unfinished_plan` filters by STATUS
    /// (completed is out, postponed is out) and by nothing else — there is no `by` anywhere in
    /// `open_priority` — so a pending parent row is asked about exactly like the child's own.
    /// The child's own row is completed here so the parent's is the one named, which pins both
    /// the row and the author string the nag shows.
    #[test]
    fn a_parent_row_is_nag_worthy_by_construction() {
        let author = TodoBy::parent_of("s-1789462738453908838");
        let all = vec![
            item("the child's own step", TodoStatus::Completed),
            TodoItem {
                content: "land the parity row".into(),
                status: TodoStatus::Pending,
                by: author.clone(),
                when: None,
                needs: Vec::new(),
            },
        ];
        let nag = nag_for(&all).expect("a pending parent row is open work");
        assert!(
            nag.contains("land the parity row"),
            "the nag names the parent's row: {nag}"
        );
        assert!(
            nag.contains("Parent s-1789462738453908838"),
            "and its author — the operator's string, in full, so the child can tell what it was \
             told from what it decided: {nag}"
        );
        assert!(
            nag.contains("your parent asked for this one"),
            "and the advice is the parent's, not the operator's and not the model's own: {nag}"
        );
        assert!(
            nag.contains("they retire the row themselves"),
            "the child is told who disposes of the row: {nag}"
        );

        // And the queue serves the child's own open work first — the model's rows precede the
        // parent's in the union, and the stable sort keeps that order inside a band.
        let board = TodoBoard::new(vec![
            item("mine, pending", TodoStatus::Pending),
            TodoItem {
                content: "theirs, pending".into(),
                status: TodoStatus::Pending,
                by: author,
                when: None,
                needs: Vec::new(),
            },
        ]);
        let served: Vec<String> = open_priority(&board.snapshot())
            .into_iter()
            .map(|t| t.content.clone())
            .collect();
        assert_eq!(served, vec!["mine, pending", "theirs, pending"]);
    }

    /// **The reply names all three authors** — the child reading its own board (its own
    /// `todo_write` reply renders the union) has to be able to tell what it decided from what it
    /// was told, and by whom.
    #[test]
    fn the_reply_names_all_three_authors() {
        let shown = render(&[
            item("mine", TodoStatus::Pending),
            TodoItem {
                content: "told by the parent".into(),
                status: TodoStatus::Pending,
                by: TodoBy::parent_of("s-1789462738453908838"),
                when: None,
                needs: Vec::new(),
            },
            TodoItem {
                content: "the operator's".into(),
                status: TodoStatus::Pending,
                by: TodoBy::Operator,
                when: None,
                needs: Vec::new(),
            },
        ]);
        assert!(shown.contains("— yours"), "{shown}");
        assert!(
            shown.contains("— Parent s-1789462738453908838"),
            "the author string verbatim, not a display name: {shown}"
        );
        assert!(shown.contains("— the operator's"), "{shown}");
    }

    /// **A `target` write reaches the child and ONLY the child.** The tool's own board is the
    /// thing that must not move: the target path returns before any of the own-board code runs,
    /// and the reply is the CHILD's board — the parent's confirmation that its rows landed
    /// beside the child's own.
    #[test]
    fn a_target_write_reaches_the_child_and_not_this_board() {
        let child = Arc::new(TodoBoard::new(vec![item(
            "the child's step",
            TodoStatus::InProgress,
        )]));
        let (mut rt, own, _fake) = children_runtime(child);
        let mut sink = RecordingToolSink::new();
        let r = rt.invoke(
            "t1",
            &call(
                r#"{"target": "s-…908838", "todos": [{"content": "land the parity row", "status": "pending"}]}"#,
            ),
            &mut sink,
        );
        assert_eq!(r.outcome, ToolOutcome::Ok, "{} {:?}", r.payload, r.outcome);
        assert!(
            r.payload.contains("the child's step"),
            "the reply shows the CHILD's board: {}",
            r.payload
        );
        assert!(
            r.payload.contains("— Parent s-1789462738453908838"),
            "with the row's author as the child will see it: {}",
            r.payload
        );
        assert!(
            r.payload.contains("untouched"),
            "and the upsert rule said in words: {}",
            r.payload
        );
        // **And this session's own board did not move.**
        assert_eq!(own.version(), 0, "the parent's own board was not written");
        assert!(own.snapshot().is_empty(), "nor replaced, nor cleared");
    }

    /// **The tool's own refusals on the target path, each BY NAME** — the daemon-side refusal
    /// (not-a-child) is the resolver's and is tested with the daemon; these are the ones the
    /// tool owes itself.
    #[test]
    fn the_target_paths_own_refusals_are_by_name() {
        let child = Arc::new(TodoBoard::new(Vec::new()));
        let (mut rt, own, _fake) = children_runtime(child.clone());
        let mut sink = RecordingToolSink::new();

        // `target` and `operator` in one call: two boards' concerns, refused together.
        let r = rt.invoke(
            "t1",
            &call(
                r#"{"target": "s-…908838", "operator": [{"content": "x", "status": "completed"}], "todos": [{"content": "y", "status": "pending"}]}"#,
            ),
            &mut sink,
        );
        assert!(
            matches!(r.outcome, ToolOutcome::Failed { .. }),
            "{:?}",
            r.outcome
        );
        let said = format!("{} {:?}", r.payload, r.outcome);
        assert!(
            said.contains("do not mix"),
            "the refusal names both fields: {said}"
        );
        assert!(said.contains("s-…908838"), "and the child it named: {said}");
        assert!(own.snapshot().is_empty(), "and nothing moved here either");
        assert!(
            child.snapshot().is_empty(),
            "the child's board was not touched by the refusal"
        );

        // An empty `todos` with a `target` is not a clear — there is no delete to mean by it.
        let r = rt.invoke(
            "t1",
            &call(r#"{"target": "s-…908838", "todos": []}"#),
            &mut sink,
        );
        assert!(matches!(r.outcome, ToolOutcome::Failed { .. }));
        assert!(r.payload.contains("there is no delete"), "{}", r.payload);

        // A `by` smuggled into an entry: the author is the daemon's to stamp, never the call's.
        let r = rt.invoke(
            "t1",
            &call(
                r#"{"target": "s-…908838", "todos": [{"content": "x", "status": "pending", "by": "operator"}]}"#,
            ),
            &mut sink,
        );
        assert!(matches!(r.outcome, ToolOutcome::Failed { .. }));
        let said = format!("{} {:?}", r.payload, r.outcome);
        assert!(
            said.contains("`by` is not yours to send"),
            "the refusal names the forged field: {said}"
        );
        assert!(
            child.snapshot().is_empty(),
            "nothing reached the child's board"
        );
    }

    /// **A `target` with no resolver behind it is refused by name** — the standalone runtime's
    /// honest answer, rather than a write quietly aimed at this session's own board.
    #[test]
    fn a_target_with_no_children_behind_it_is_refused_by_name() {
        let (mut rt, own) = runtime();
        let mut sink = RecordingToolSink::new();
        let r = rt.invoke(
            "t1",
            &call(r#"{"target": "s-…908838", "todos": [{"content": "x", "status": "pending"}]}"#),
            &mut sink,
        );
        assert!(
            matches!(r.outcome, ToolOutcome::Failed { .. }),
            "{:?}",
            r.outcome
        );
        let said = format!("{} {:?}", r.payload, r.outcome);
        assert!(said.contains("no children's boards"), "{}", said);
        assert!(own.snapshot().is_empty(), "and this board stayed empty");
    }
}
