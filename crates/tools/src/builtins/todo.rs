//! `todo_write` — the session's plan, written by the model that runs it.
//!
//! The list is **session state**, not a file: [`Access::Session`] exists because
//! `docs/tool-survey.md` §1.4 found `todo_write` under-declared as `Read` in the
//! wild while it mutates session state, and this is the tool that declares it
//! right. It writes nothing the operator owns, so nobody adjudicates it — but it
//! is not read-class either, and the schema says so.
//!
//! The whole list every time. A delta — add one, complete one — would let a model
//! that misremembered the current list drift it silently; a full replace either
//! matches what the head saw or shows the whole difference. The harness persists
//! the list and announces it, and [`TodoBoard`] is the seam that makes that
//! split possible without the tools crate knowing about stores or logs: the tool
//! mutates the board, the harness watches the version.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use letibot_tokencore::store::{TodoBy, TodoCondition, TodoItem, TodoStatus};
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
    /// **Separate, and not appended to `todos`, for one reason: the `todo` tool REPLACES the model's
    /// list wholesale** — *"the whole list is replaced on every write, because a delta the model got
    /// wrong is a delta nobody can audit"* — so an operator row left in that vector would be deleted
    /// by the model's next `todo` call. Two halves, one getter.
    operator: Mutex<Vec<TodoItem>>,
    /// **A PARENT session's rows on THIS board — the third half, and the one that must never meet
    /// the wholesale replace.**
    ///
    /// The operator: *"yes - i want parent agents to be able to create todos for subagents.
    /// throught tree author - (Parent <session-id-of-parent>)"*. A child's board therefore has three
    /// authors, and the new one's write is the OPPOSITE of the model's own: the parent's `todos` are
    /// an UPSERT scoped to the parent's authorship ([`TodoBoard::upsert_parent`]), because
    /// *"send the whole list; omitting an entry removes it"* is a contract about **your own** board —
    /// carried across sessions it would be a parent's three rows wiping a child's plan and the
    /// operator's rows, which is precisely the collision the two-author rule on this board exists to
    /// prevent. Same reasoning as the operator's half, one author over: three halves, one getter.
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
    /// **inside the model's list** — so `todo_write`'s wholesale replace would delete them, the nag
    /// would count them twice once the head re-pushed its own half on hello, and the pane would draw
    /// each of them twice. Sorting it once, here, is what makes *several halves, one getter* an
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

    /// **Move the STATE of the operator's rows — the model's only way to dispose of one.**
    ///
    /// The board is one list (*"the existing getter should return mine and yours"*), and
    /// `TodoBy::Operator`'s doc claimed the model could already act on the operator's half: *"the
    /// model can mark the operator's item done, and the nag in `harness.rs` picks it up like any
    /// other."* **It could not.** `todo_write` replaces the model's half and `set_operator` is the
    /// HEAD's frame, so a row the operator wrote could be nagged about every idle turn and never
    /// disposed of — the wedge R48 describes, and the reason this exists.
    ///
    /// **The words are the key, and that is not a shortcut — there is no other key.** Measured:
    /// `TodoEntry` on the wire is `content`, `status`, `by` (protocol.rs:677), and the operator has
    /// ruled out a bump, so a row cannot be addressed by an id. What the model HAS is the exact
    /// string, because the nag hands it over character for character.
    ///
    /// **So a name that does not resolve exactly once is REFUSED, and the candidates are named.**
    /// This is the house rule for an ambiguous name — `ClientFrame`'s own precedent for an
    /// ambiguous option prefix is *"refused, with the candidates named"* — and it is what makes
    /// content-keying safe rather than sloppy: the guess this refuses to make is a row silently
    /// changing state under a model that quoted something else.
    ///
    /// **ALL OR NOTHING.** Every update resolves before any is applied, so a call that names one
    /// good row and one bad one moves neither, and the caller may apply its own list only after
    /// this has succeeded. A partial write would leave the model's plan updated and the operator's
    /// half not, which is the drift this whole board is arranged to prevent.
    ///
    /// **It sets STATE and never membership.** A model may mark the operator's row done — or put
    /// it back to pending — and may not delete it: the row is the operator's own words, and a model
    /// that misquotes must not be able to take them off the board. Membership is the head's, by
    /// `set_operator`, and the operator's own delete key.
    ///
    /// Returns how many rows actually CHANGED, and bumps the version only when that is not zero: a
    /// status set to what it already was is not an event, and announcing it would put a row on the
    /// wire and a write in the store for nothing.
    pub fn set_operator_states(&self, updates: &[(String, TodoStatus)]) -> Result<usize, String> {
        let mut half = self.operator.lock().unwrap_or_else(|e| e.into_inner());
        // resolve every update BEFORE any is applied — see ALL OR NOTHING
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
                    let have: Vec<&str> = half.iter().map(|t| t.content.trim()).collect();
                    return Err(format!(
                        "no row of the operator's says `{want}`{}. The operator's own rows are:                          {}",
                        if have.is_empty() {
                            ", and there are none"
                        } else {
                            ""
                        },
                        if have.is_empty() {
                            "(none)".to_string()
                        } else {
                            have.iter()
                                .map(|c| format!("`{c}`"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        }
                    ));
                }
                _ => {
                    return Err(format!(
                        "`{want}` is TWO of the operator's rows, so which one changes is not                          something this can know. Quote the one you mean, or say in your reply                          which you left and why."
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

    /// **A PARENT's write: an UPSERT scoped to the parent's own authorship — never the wholesale
    /// replace the model's half takes.**
    ///
    /// The whole of the safety here is WHAT IT DOES NOT TOUCH, so read this twice. The model's
    /// `todo_write` contract is *"send the WHOLE list every time — there is no delta; omitting an
    /// entry removes it"*, and that contract **does not carry across sessions**: a parent sending
    /// its own three rows as the whole list would delete the child's plan and the operator's rows —
    /// one agent overwriting another agent's plan, the exact collision the author split on this
    /// board exists to prevent. So here the rows sent are the rows the parent is ADDING or
    /// state-updating AS THE PARENT, and nothing else moves:
    ///
    /// * a row whose trimmed text matches one of this author's existing rows **moves its state**
    ///   (and nothing else — the text is the name, and a re-worded row is a NEW row, the same rule
    ///   `set_operator_states` keeps for the operator's);
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
    pub fn upsert_parent(&self, rows: &[(String, TodoStatus)], by: &TodoBy) -> usize {
        let mut half = self.parent.lock().unwrap_or_else(|e| e.into_inner());
        let mut changed = 0usize;
        for (content, status) in rows {
            let want = content.trim();
            if let Some(hit) = half
                .iter_mut()
                .find(|t| t.by == *by && t.content.trim() == want)
            {
                if hit.status != *status {
                    hit.status = *status;
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
                });
                changed += 1;
            }
        }
        if changed > 0 {
            self.version.fetch_add(1, Ordering::SeqCst);
        }
        changed
    }

    /// Replace the list. Returns the new version, which is what the harness
    /// compares against to decide whether a store write and an announcement are
    /// owed.
    pub fn replace(&self, todos: Vec<TodoItem>) -> u64 {
        *self.todos.lock().unwrap_or_else(|e| e.into_inner()) = todos;
        self.version.fetch_add(1, Ordering::SeqCst) + 1
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
    /// The OPERATOR's half only, and not for symmetry: a model row is replaced wholesale by its
    /// next `todo_write`, so a condition written there would already be gone, and the caller here is
    /// the harness, whose rows these are.
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
pub fn unfinished_plan(todos: &[TodoItem]) -> Option<String> {
    // **THE QUEUE, served one at a time** — see `open_priority` for the order and why it is one.
    let open = open_priority(todos);
    let next = open.first()?;
    let left = open.len() - 1;
    // The count of what is BEHIND this one, because the model is entitled to know the plan is bigger
    // than the row it is being asked about — and that is exactly the fact that must not become a list.
    let rest = match left {
        0 => String::new(),
        1 => " (1 more open)".to_string(),
        n => format!(" ({n} more open)"),
    };
    // **WHO ASKED, and it is not a courtesy — it decides WHICH FIELD disposes of the row.** A row the
    // model wrote is rewritten with `todos`; a row the OPERATOR wrote is moved with `operator`, its
    // text quoted exactly; and a row a PARENT wrote is one NO field of this tool may dispose of —
    // the author retires it — which the child has to be told rather than left to guess. The nag is
    // the one place the model hears about a row before acting on it, so leaving the author out is
    // how a model comes to rewrite its own plan at the row somebody else is waiting on.
    let who = match &next.by {
        TodoBy::Operator => "the operator's",
        // The author string itself (`Parent <session id>`), verbatim: a child reading its own
        // board must be able to tell what it decided from what it was told, and by whom.
        TodoBy::Parent(author) => author.as_str(),
        TodoBy::Model => "yours",
    };
    let state = match next.status {
        TodoStatus::InProgress => format!(" — {who}, and you had it in progress"),
        _ => format!(" — {who}"),
    };
    // **AND THE VERBS DIFFER BY AUTHOR, for the same reason.** The model may drop its OWN row — that
    // is what `drop` is for — and may not drop the operator's: their row is their words, so the model
    // moves its STATE and says in its reply why it is not doing the work. `todo_write`'s `operator`
    // field is named in the message because a model that has to guess a mechanism guesses wrong.
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
            "do this one, or mark it done, or drop it — a plan left open is a plan nobody is \
             following. If you are stopping here deliberately, say why in your reply."
        }
    };
    Some(format!(
        "[todo check] this turn is finished and one item is not done{rest}:\n  - {}{state}\n{advice}",
        next.content.trim(),
    ))
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
        rows: &[(String, TodoStatus)],
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
            "Replace the session's todo list with exactly this list. Use it to plan \
             multi-step work and to keep the operator's pane current: one entry per \
             step, the step being worked on marked in_progress, finished steps \
             marked completed. Send the WHOLE list every time — there is no delta; \
             omitting an entry removes it.\n\nA row the OPERATOR has POSTPONED comes \
             back marked `[p]`: it stays on the board, it is still theirs, and it is \
             not work you are being asked for — do not propose it again. `postponed` \
             is not a status you may send; setting one aside and lifting it again are \
             the operator's own acts.\n\n`operator` is for rows the OPERATOR \
             wrote — the ones the reply marks `— the operator's` — and changes their \
             STATE only: quote `content` EXACTLY as the reply shows it. A quote that \
             does not match exactly one of their rows is refused and nothing is \
             written, because a guessed row is a row changing state under you. You \
             cannot delete their row; if you think it should not be done, say so in \
             your reply.\n\n`target` names a session YOU spawned with `task` and \
             writes YOUR rows onto THAT session's board instead of your own. There the \
             whole-list contract is OFF: your `todos` are the rows you are adding or \
             updating as the parent — a row whose text matches one of yours there \
             moves its state, a new text is added — and the child's rows, the \
             operator's rows and every other author's rows are UNTOUCHED. Omitting \
             one of your rows leaves it on the child's board; there is no delete, so \
             retire a row by marking it completed. The child is told through the usual \
             todo nags, and rows you write there show as `Parent <your session id>`. \
             `target` and `operator` do not mix: name one child, your own rows only.",
            json!({
                "type": "object",
                "properties": {
                    "operator": {
                        "type": "array",
                        "description": "Rows the OPERATOR wrote, whose STATE you are \
                                        changing. Quote `content` exactly as the reply \
                                        shows it. Sets state; never deletes.",
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
                        "description": "The complete list, in the order to do them. With \
                                        `target`: the rows you are adding to or \
                                        state-updating on that child's board — not a \
                                        replace; the child's own rows and everyone \
                                        else's are untouched.",
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
                "required": ["todos"]
            }),
            Access::Session,
        )
    }

    fn invoke(&self, _ctx: &mut InvokeCtx<'_>, args: &Value) -> Invocation {
        let Some(list) = args.get("todos").and_then(|v| v.as_array()) else {
            return Invocation::failed(
                "todo_write needs `todos`",
                "call it again with `todos` set to the complete list — every entry's \
                 `content` and `status`. Omitting the field writes nothing; it does \
                 not clear the list.",
            );
        };
        let rows = match parse_rows(list) {
            Ok(rows) => rows,
            Err(inv) => return inv,
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
            if let Some(op) = args.get("operator") {
                return refused(
                    format!(
                        "`target` and `operator` do not mix: this call named a child (`{target}`) \
                         and an `operator` block ({op})"
                    ),
                    "`operator` moves the rows the OPERATOR wrote on YOUR board; `target` writes \
                     your rows onto a CHILD's board. One call, one board — send them separately.",
                );
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
        let items: Vec<TodoItem> = rows
            .into_iter()
            .map(|(content, status)| TodoItem {
                content,
                status,
                // the MODEL's list, by definition: this arm of the function is the `todo` tool
                by: letibot_tokencore::store::TodoBy::Model,
                // **The model's own half is replaced wholesale on every call**, so a condition
                // written here would die on the model's next `todo_write` — see
                // `TodoItem::when`. A conditioned row belongs to the OPERATOR's half, which this
                // tool does not own and cannot overwrite.
                when: None,
            })
            .collect();
        // **THE OPERATOR'S ROWS ARE MOVED BEFORE THE MODEL'S LIST IS REPLACED**, so a quote that
        // does not resolve costs nothing at all: `set_operator_states` resolves every name before it
        // applies any, and a refusal here leaves the model's own half exactly as it was. The other
        // order would write the plan and then refuse, which is a half-applied call.
        let mut moved = 0usize;
        if let Some(rows) = args.get("operator") {
            let Some(rows) = rows.as_array() else {
                return Invocation::failed(
                    "`operator` needs to be a list of `{content, status}`",
                    "send `operator` as a list, or leave it out when you are not touching the \
                     operator's rows.",
                );
            };
            let mut updates: Vec<(String, TodoStatus)> = Vec::new();
            for (i, r) in rows.iter().enumerate() {
                let Some(content) = r.get("content").and_then(|v| v.as_str()) else {
                    return Invocation::failed(
                        format!("operator entry {} has no `content`", i + 1),
                        "every `operator` entry needs `content` — the row's own words, quoted \
                         exactly as the list shows them.",
                    );
                };
                let status = match r.get("status").and_then(|v| v.as_str()) {
                    Some("pending") => TodoStatus::Pending,
                    Some("in_progress") => TodoStatus::InProgress,
                    Some("completed") => TodoStatus::Completed,
                    other => {
                        return Invocation::failed(
                            format!(
                                "operator entry {} has status `{}`",
                                i + 1,
                                other.unwrap_or("(none)")
                            ),
                            "`status` is one of: pending, in_progress, completed.",
                        );
                    }
                };
                updates.push((content.to_string(), status));
            }
            match self.board.set_operator_states(&updates) {
                Ok(n) => moved = n,
                Err(why) => {
                    return Invocation::failed(
                        format!("the operator's rows were not moved: {why}"),
                        "**nothing was written** — not their rows and not yours. Quote the row's \
                         `content` exactly as the list below shows it, or leave `operator` out.",
                    );
                }
            }
        }
        self.board.replace(items);
        // The list back to the model, as it now stands — so the next call is
        // written against what the pane shows, not against what the model
        // believes it wrote.
        let mut out = render(&self.board.snapshot());
        if let Some(rows) = args.get("operator").and_then(|v| v.as_array()) {
            if !rows.is_empty() {
                out.push_str(&format!(
                    "\nof the operator's rows you named, {} changed status{}.\n",
                    moved,
                    if moved == rows.len() {
                        ""
                    } else {
                        " (the rest already said that)"
                    }
                ));
            }
        }
        Invocation::ok(out)
    }
}

/// **Parse and validate a `todos` list — the shared gate both boards' writes pass.**
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
fn parse_rows(list: &[Value]) -> Result<Vec<(String, TodoStatus)>, Invocation> {
    let mut rows = Vec::with_capacity(list.len());
    for (i, t) in list.iter().enumerate() {
        if let Some(obj) = t.as_object() {
            for key in obj.keys() {
                if !matches!(key.as_str(), "content" | "status") {
                    return Err(Invocation::failed(
                        format!("entry {} has an unknown field `{key}`", i + 1),
                        if key == "operator" {
                            "**`operator` is a TOP-LEVEL argument, not a field of a `todos` \
                             entry**: send it BESIDE `todos`, as {\"todos\": […], \"operator\": \
                             [{\"content\": \"<the row's own words, quoted exactly>\", \
                             \"status\": \"completed\"}]}. Left inside an entry it is ignored, \
                             and being ignored is what makes it look like the row changed when \
                             it did not."
                        } else if key == "by" {
                            "**`by` is not yours to send**: the author of a row is decided by the \
                             daemon from the session making the call — yours on your own board, \
                             `Parent <your session id>` on a child's. Send only `content` and \
                             `status`."
                        } else {
                            "a `todos` entry has exactly `content` and `status` — the whole list \
                             is replaced on every call, so an unknown field is refused rather \
                             than ignored."
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
        rows.push((content.to_string(), status));
    }
    Ok(rows)
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
        // operator's` is one this call moves with `operator`, one marked `— yours` is one it
        // replaces with `todos`, and one marked with a `Parent` author is one NO field of this
        // tool disposes of — the author retires it. Content is printed VERBATIM (never trimmed,
        // never elided), because that string is the name the model has to quote back.
        let author = match &t.by {
            TodoBy::Operator => "the operator's".to_string(),
            // **The operator's own string, verbatim and in full** — `Parent s-…` — because a
            // child reading its own board has to be able to tell what it decided from what it
            // was told, and by whom.
            TodoBy::Parent(who) => who.clone(),
            TodoBy::Model => "yours".to_string(),
        };
        out.push_str(&format!(
            "  {}. {} {}  — {}\n",
            i + 1,
            mark,
            t.content,
            author
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
                },
                TodoItem {
                    content: "T2 wire the pane".into(),
                    status: TodoStatus::Pending,
                    by: TodoBy::Operator,
                    when: None,
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
        }]);
        let all = b.snapshot();
        // The renderer the model reads.
        let rendered = render(&all);
        assert!(
            rendered.contains("#model"),
            "the tag did not survive into what the model is told: {rendered}"
        );
        // And the nag, whose only edit is a trim.
        let nag = unfinished_plan(&all).expect("one row is open");
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
    /// tool's wholesale REPLACE of the model's list does not take the operator's rows with it.
    #[test]
    fn the_board_returns_the_operators_rows_alongside_the_models() {
        let b = TodoBoard::new(vec![TodoItem {
            content: "the model's".into(),
            status: TodoStatus::Pending,
            by: TodoBy::Model,
            when: None,
        }]);
        assert_eq!(b.snapshot().len(), 1, "the model's list as given");

        b.set_operator(vec![TodoItem {
            content: "the operator's".into(),
            status: TodoStatus::Pending,
            by: TodoBy::Operator,
            when: None,
        }]);

        let all = b.snapshot();
        assert_eq!(all.len(), 2, "**the getter returns BOTH**: {all:?}");
        assert!(
            all.iter().any(|t| t.by == TodoBy::Operator),
            "and says who wrote each"
        );
        // **`unfinished_plan` — what the nag asks — sees the operator's row with no change at all**
        assert!(
            unfinished_plan(&all).is_some(),
            "so the reminder can fire for work the OPERATOR queued"
        );

        // **AND THE MODEL'S WHOLESALE REPLACE DOES NOT DELETE THEM.** `replace` is the `todo` tool's
        // own write, and it is why the two halves are kept apart rather than concatenated.
        b.replace(vec![TodoItem {
            content: "the model's, revised".into(),
            status: TodoStatus::Pending,
            by: TodoBy::Model,
            when: None,
        }]);
        let after = b.snapshot();
        assert_eq!(after.len(), 2, "the operator's row survived: {after:?}");
        assert!(after.iter().any(|t| t.content == "the operator's"));
    }

    /// **The union is TWO BLOCKS, not an interleaving** — and with hierarchy coming, that is the
    /// fact the whole shape turns on.
    ///
    /// `snapshot` is a clone-then-`extend`: every model row precedes every operator row, always,
    /// because nothing merges the two vectors. So the halves cannot interleave by depth, which means
    /// **"replace my half" stays a well-defined edit**: the model's wholesale write cannot move,
    /// delete or re-parent a row of the operator's, and the operator's half is a suffix of the
    /// union rather than a scatter through it.
    ///
    /// # And the hazard that comes with it, which is why this is asserted and not assumed
    ///
    /// Depth is POSITIONAL — that is what makes it markdown's model and rano's, and it is the right
    /// choice. But positional depth over a concatenation means a row's apparent parent is *the row
    /// before it in the union*, and at the seam that row belongs to somebody else. An operator row at
    /// depth 1 sitting after a model row at depth 0 renders as a child of it, and **the model's next
    /// write can change that parent without touching a single operator row**: a wholesale replace
    /// changes the last model row and the operator's subtree is silently re-hung under whatever took
    /// its place. That is the same failure as a flattened subtree — data that reads as a statement
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
            },
            TodoItem {
                content: "m2".into(),
                status: TodoStatus::Pending,
                by: TodoBy::Model,
                when: None,
            },
        ]);
        b.set_operator(vec![
            TodoItem {
                content: "o1".into(),
                status: TodoStatus::Pending,
                by: TodoBy::Operator,
                when: None,
            },
            TodoItem {
                content: "o2".into(),
                status: TodoStatus::Pending,
                by: TodoBy::Operator,
                when: None,
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
            "the union interleaved the halves, so `replace` is no longer a half-edit: {all:?}"
        );

        // **And a wholesale replace cannot reorder what it does not own.** The model rewrites its
        // own rows — fewer of them here — and the operator's block is still the tail, in the same
        // order, with the same contents.
        b.replace(vec![TodoItem {
            content: "m1 rewritten".into(),
            status: TodoStatus::Pending,
            by: TodoBy::Model,
            when: None,
        }]);
        let after = b.snapshot();
        assert_eq!(
            after.iter().map(|t| t.content.as_str()).collect::<Vec<_>>(),
            vec!["m1 rewritten", "o1", "o2"],
            "the model's replace moved the operator's rows: {after:?}"
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
        let board = Arc::new(TodoBoard::new(vec![]));
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
    /// nag in `harness.rs` picks it up like any other"* — and it could not: `todo_write` replaces
    /// the model's half, `set_operator` is the head's frame, so a row the operator wrote was nagged
    /// about every idle turn and could never be answered. R48's wedge, from the other side.
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
            },
            TodoItem {
                content: "push leticl".into(),
                status: TodoStatus::Pending,
                by: TodoBy::Operator,
                when: None,
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
            },
            TodoItem {
                content: "same words".into(),
                status: TodoStatus::Pending,
                by: TodoBy::Operator,
                when: None,
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
        let mine = unfinished_plan(&[item("my step", TodoStatus::Pending)]).unwrap();
        assert!(mine.contains("— yours"), "{mine}");
        assert!(
            mine.contains("or drop it"),
            "the model may drop its OWN row: {mine}"
        );

        let theirs = unfinished_plan(&[TodoItem {
            content: "restart the daemon".into(),
            status: TodoStatus::Pending,
            by: TodoBy::Operator,
            when: None,
        }])
        .unwrap();
        assert!(theirs.contains("— the operator's"), "{theirs}");
        assert!(
            theirs.contains("`todo_write`'s `operator` field"),
            "**and names the mechanism**, because a model that has to guess one guesses wrong: {theirs}"
        );
        assert!(
            !theirs.contains("or drop it"),
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
            },
            TodoItem {
                content: "an ordinary row".into(),
                status: TodoStatus::Pending,
                by: TodoBy::Operator,
                when: None,
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
    /// session the operator's rows as its own: `todo_write` would delete them, and once the head
    /// re-pushed its own half on hello the union held each of them twice.
    #[test]
    fn the_stores_union_is_split_back_into_its_two_halves() {
        let stored = vec![
            TodoItem {
                content: "the model's, from the store".into(),
                status: TodoStatus::InProgress,
                by: TodoBy::Model,
                when: None,
            },
            TodoItem {
                content: "the operator's, from the store".into(),
                status: TodoStatus::Pending,
                by: TodoBy::Operator,
                when: None,
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

        // the model's replace cannot take their row with it — the whole reason the halves are apart
        b.replace(vec![TodoItem {
            content: "a fresh plan".into(),
            status: TodoStatus::Pending,
            by: TodoBy::Model,
            when: None,
        }]);
        assert_eq!(
            b.snapshot().len(),
            2,
            "and it survives the model's next write: {:?}",
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

    #[test]
    fn the_whole_list_replaces_and_the_reply_is_the_list_as_it_stands() {
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

        // The second write is the list, not a patch on it: three entries in, one
        // entry out means one entry on the board.
        let r2 = rt.invoke(
            "t1",
            &call(r#"{"todos": [{"content": "render the pane", "status": "in_progress"}]}"#),
            &mut sink,
        );
        assert_eq!(r2.outcome, ToolOutcome::Ok);
        assert_eq!(board.snapshot().len(), 1);
        // And the version moved twice, which is what the harness flush reads.
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

    // -- the turn boundary ------------------------------------------------

    fn item(content: &str, status: TodoStatus) -> TodoItem {
        TodoItem {
            content: content.into(),
            status,
            by: TodoBy::Model,
            when: None,
        }
    }

    /// **The trigger is pending work and nothing else.** The obvious way to get this
    /// wrong is a message that fires whenever a turn ends, which teaches the model to
    /// clear its todos to make it stop — worse than no check at all. So the silence cases
    /// are asserted first and with the same weight as the firing one.
    #[test]
    fn a_plan_with_nothing_open_has_nothing_to_say() {
        assert!(unfinished_plan(&[]).is_none(), "no plan at all");
        assert!(
            unfinished_plan(&[
                item("one", TodoStatus::Completed),
                item("two", TodoStatus::Completed),
            ])
            .is_none(),
            "a finished plan is not a finding, it is the answer"
        );
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
        let msg = unfinished_plan(&todos).expect("four open items is a finding");
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
            let msg = unfinished_plan(&todos).expect("open work");
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
            unfinished_plan(&todos).is_none(),
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
        let msg = unfinished_plan(&[
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
        // being a trap.** Without *drop what you no longer mean to do* the model's only exit is to
        // lie about its own statuses, which is the failure this exists to stop.
        assert!(msg.contains("do this one"), "{msg}");
        assert!(msg.contains("mark it done"), "{msg}");
        assert!(msg.contains("drop it"), "{msg}");
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
        let msg = unfinished_plan(&[
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
        let msg = unfinished_plan(&todos).expect("ten open items is a finding");
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
        board.replace(vec![item("do the thing", TodoStatus::InProgress)]);
        board.replace(vec![item("do the thing", TodoStatus::Completed)]);
        assert!(
            unfinished_plan(&board.snapshot()).is_none(),
            "the plan as it STANDS is what is asked about"
        );
    }

    #[test]
    fn an_empty_list_is_a_real_write_and_clears_the_board() {
        let (mut rt, board) = runtime();
        let mut sink = RecordingToolSink::new();
        rt.invoke(
            "t1",
            &call(r#"{"todos": [{"content": "one", "status": "pending"}]}"#),
            &mut sink,
        );
        assert_eq!(board.snapshot().len(), 1);
        let r = rt.invoke("t1", &call(r#"{"todos": []}"#), &mut sink);
        assert_eq!(r.outcome, ToolOutcome::Ok);
        assert!(r.payload.contains("empty"), "{}", r.payload);
        assert!(board.snapshot().is_empty());
        assert_eq!(board.version(), 2);
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
            rows: &[(String, TodoStatus)],
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
    /// This is the hazard the whole feature turns on, measured rather than argued: `todo_write`'s
    /// own-board contract is *"send the WHOLE list; omitting an entry removes it"*, and a parent
    /// that wrote a child's board under that contract would delete the child's plan with its own
    /// three rows. Every arm below is one thing that must not move.
    #[test]
    fn a_parents_write_upserts_its_own_rows_and_nobody_elses_move() {
        let b = TodoBoard::new(vec![
            item("the child's own step", TodoStatus::InProgress),
            TodoItem {
                content: "the operator's row".into(),
                status: TodoStatus::Pending,
                by: TodoBy::Operator,
                when: None,
            },
        ]);
        let author = TodoBy::parent_of("s-1789462738453908838");

        // Two rows arrive from the parent.
        let changed = b.upsert_parent(
            &[
                ("land the parity row".into(), TodoStatus::Pending),
                ("write the report".into(), TodoStatus::Pending),
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
            &[("land the parity row".into(), TodoStatus::Completed)],
            &author,
        );
        assert_eq!(changed, 1, "the existing row moved, nothing was added");
        assert_eq!(b.parent_snapshot().len(), 2, "still two rows: no duplicate");
        assert_eq!(b.version(), 2);

        // **Omission is not deletion.** The parent sends one row and leaves the other out —
        // under the own-board contract that would delete it; here it must not.
        let changed = b.upsert_parent(
            &[("write the report".into(), TodoStatus::InProgress)],
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
            &[("write the report".into(), TodoStatus::InProgress)],
            &author,
        );
        assert_eq!(changed, 0, "nothing changed");
        assert_eq!(b.version(), before, "and nothing was announced");

        // **And the CHILD's own wholesale replace cannot reach them either** — the other edge of
        // the same rule. The child re-plans its own half and the parent's rows stay.
        b.replace(vec![item("the child's revised step", TodoStatus::Pending)]);
        let all = b.snapshot();
        assert_eq!(
            all.iter().map(|t| t.content.as_str()).collect::<Vec<_>>(),
            vec![
                "the child's revised step",
                "land the parity row",
                "write the report",
                "the operator's row",
            ],
            "the child's replace moved somebody else's rows: {all:?}"
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
            },
        ];
        let nag = unfinished_plan(&all).expect("a pending parent row is open work");
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
            },
            TodoItem {
                content: "the operator's".into(),
                status: TodoStatus::Pending,
                by: TodoBy::Operator,
                when: None,
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
